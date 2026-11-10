// ============================================================================
// stockout_model.rs —— 出库单数据层：建单 / 改单 / 软删 / 取号 / 三本账负向 / 作废回滚
//
// 依据：`backend/migrations/business/V0203__erp_stock_out.up.sql`（本卡新建的两张表）
// 与 ARCH-3 审计 §5 DC-13 / §6 R-P0-1。
//
// # 三本账（与收货入库完全镜像，方向相反）
//
// ```text
// 成本账  biz_inventory            qty −/amount −（avg_cost 按剩余金额重算）
// 库位账  biz_inventory_location   qty −（只记数量）
// 流水    biz_inventory_txn        txn_type='OUT'、qty 恒正、amount ≥ 0
// ```
//
// 三条不变式（`scripts/verify/erp_invariants.py` 的 B1 / B4 / C1）必须同时成立：
//
// ```text
// B1  Σ biz_inventory_location.qty（按物料×仓）= Σ biz_inventory.qty
// B4  Σ 流水（IN 加、OUT 减）                  = Σ biz_inventory.qty
// C1  biz_inventory.qty 非负（allow_negative = 0 的仓库）
// ```
//
// 所以**三本账必须在同一个事务里一起减**：只减成本账不减库位账 → B1 立刻红；
// 只减账不写流水 → B4 立刻红。这与 P6「收货同时写两本账」是同一条铁律的反向版。
//
// # 金额口径（硬红线）
//
// 出库金额 = **账上真实差额**（`before.amount − after.amount`），不是 `qty × avg_cost` 重算：
// 多桶 / 4 位舍入下两者不等，用重算值去贷 1405 会让 B2（库存金额 = 存货科目余额）报 0.00x。
// 差额同时回写到 `biz_stock_out_item.amount`，作废时按它原路退回（不查当前均价 ——
// 期间又进过货，均价已变，用现价退会污染成本，与 `stockin_cancel` 同口径）。
//
// # 负库存
//
// `biz_warehouse.allow_negative = 0`（默认）时，任一明细的出库数量超过该
// (物料, 仓库, 库位桶) 的可用量 → `DbError::InvalidValue`，handler 翻成
// **409 + 点名物料编码 / 名称与可用量**。允许负库存的仓库放行，金额只减到 0 为止
// （不能把 amount 减成负数：`C1` 之外 `ck_biz_inv_txn_amt` 也禁负），差额按实际减掉的部分结转。
//
// # 锁顺序（防死锁，PRD §3.2.6）
//
// 明细一律按 `material_id, location_id NULLS FIRST, line_no` 升序处理，
// 每条依次锁「成本账行 → 库位账行」；作废走**同一个顺序**，
// 所以两张单并发（过账 vs 作废）不会交叉等待。
//
// # 数据权限
//
// ```text
// SCOPE-EXEMPT: 出库单是 ERP 单据表（V0203 建表，无 dept_id / 归属人列），
// 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:stock-out:* 权限码控制。
// 列表分页走 scope::paginate_unscoped（闸①口径）。
// SCOPE-EXEMPT: biz_inventory / biz_inventory_location / biz_inventory_txn 是仓储台账，无归属列。
// SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
// ```
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::normalize_page;
use crate::business::erp::voucher;
use crate::business::shared::scope;
use crate::business::shared::state_machine;
use crate::utils;

use super::inventory_writer::{self, TxnRecord};
use super::stockin_base::InventorySnapshot;
use super::stockout_base::*;

// ==================== 只读 ====================

/// 按主键取单头（未删除）。
pub async fn find(id: Uuid) -> DbResult<Option<StockOutRow>> {
    let sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(StockOutRow::from_row).transpose()
}

/// 在事务/连接上按主键取单头并**加行锁**（过账 / 作废先锁头，重复提交在这里串行化）。
pub async fn lock(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<StockOutRow>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {HEADER_COLUMNS} FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(StockOutRow::from_row).transpose()
}

/// 取整单明细（**按锁顺序排序**：`material_id, location_id NULLS FIRST, line_no`）。
///
/// 过账与作废都必须用这个顺序拿锁，否则两张单并发会交叉等待（`40P01`）。
/// 返回值顺序就是落库行号顺序，调用方不要再自己 `sort`（会破坏锁序）。
pub async fn items_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Vec<StockOutItemRow>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_TABLE} WHERE stock_out_id = ?::uuid \
                 ORDER BY material_id, location_id NULLS FIRST, line_no"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.iter().map(StockOutItemRow::from_row).collect()
}

/// 分页（关键词 / 状态 / 类型 / 仓库 / 日期区间）。
pub async fn page(q: &StockOutFilter) -> DbResult<(Vec<StockOutRow>, i64)> {
    let mut sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (out_no LIKE ? OR reason LIKE ? OR remark LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(out_type) = q.out_type.as_deref() {
        sql.push_str(" AND out_type = ?");
        params.push(Value::from(out_type));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?::uuid");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(date_from) = q.date_from {
        sql.push_str(" AND out_date >= ?");
        params.push(Value::from(date_from));
    }
    if let Some(date_to) = q.date_to {
        sql.push_str(" AND out_date <= ?");
        params.push(Value::from(date_to));
    }
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result = scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size).await?;
    let rows = result
        .list
        .iter()
        .map(StockOutRow::from_row)
        .collect::<DbResult<Vec<StockOutRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// DTO 里的 id 是 `String`：非法 UUID 一律报错（不静默跳过条件）。
pub(super) fn parse_id(value: &str) -> DbResult<Uuid> {
    Uuid::parse_str(value.trim()).map_err(|err| DbError::InvalidValue(format!("非法 UUID `{value}`：{err}")))
}

/// 物料展示信息（错误文案 / 详情用：编码 + 名称 + 基础单位）。
#[derive(Debug, Clone)]
pub struct MaterialInfo {
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 基础单位。
    pub unit_id: Uuid,
    /// 是否强制一物一码（本版本出库**不支持**一物一码物料，见 handler）。
    pub serial_required: bool,
}

impl MaterialInfo {
    /// 错误文案里的物料标识：`M-001 螺丝`。
    pub fn label(&self) -> String {
        format!("{} {}", self.material_code, self.material_name)
    }
}

/// 单查物料（不存在 / 已删 → `None`）。
pub async fn material_info(ex: &impl SqlExecutor, material_id: Uuid) -> DbResult<Option<MaterialInfo>> {
    let row = ex
        .query_first_sql(
            "SELECT material_code, material_name, unit_id, serial_required FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(MaterialInfo {
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            unit_id: row.get_typed("unit_id")?,
            serial_required: row.get_typed("serial_required")?,
        })),
    }
}

/// 仓库的负库存策略（`allow_negative`）。
#[derive(Debug, Clone)]
pub struct WarehousePolicy {
    /// 仓库编码。
    pub warehouse_code: String,
    /// 仓库名称。
    pub warehouse_name: String,
    /// 是否允许负库存。
    pub allow_negative: bool,
}

/// 单查仓库的策略（不存在 / 已删 → `None`）。
pub async fn warehouse_policy(ex: &impl SqlExecutor, warehouse_id: Uuid) -> DbResult<Option<WarehousePolicy>> {
    let row = ex
        .query_first_sql(
            "SELECT warehouse_code, warehouse_name, allow_negative FROM biz_warehouse \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&warehouse_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => {
            let flag: i16 = row.get_typed("allow_negative")?;
            Ok(Some(WarehousePolicy {
                warehouse_code: row.get_typed("warehouse_code")?,
                warehouse_name: row.get_typed("warehouse_name")?,
                allow_negative: flag == 1,
            }))
        }
    }
}

/// 成本对象的**只读引用**（校验 / 展示用：对象号 + 状态）。
///
/// 为什么不复用 `cost_object_model::CostObjectRow`：那张行结构要 join 产品 / 仓库 /
/// 库位 / 客户 / 车间五张表（详情页用），而出库单只需要「对象号 + 状态」两列 ——
/// 建单校验走这条窄查询，列表填充走 [`cost_object_labels`]（整页一次）。
#[derive(Debug, Clone)]
pub struct CostObjectRef {
    /// 对象号（`CB-yyyyMMdd-NNNNNN`，用户认的就是它 —— `biz_cost_object` 没有独立的「名称」列）。
    pub object_no: String,
    /// 状态（`OPEN` / `CLOSED` / `CANCELLED`，与 `ck_biz_cost_object_status` 同值域）。
    pub status: String,
}

/// 单查成本对象（不存在 / 已删 → `None`）。
pub async fn cost_object_ref(ex: &impl SqlExecutor, cost_object_id: Uuid) -> DbResult<Option<CostObjectRef>> {
    let row = ex
        .query_first_sql(
            "SELECT object_no, status FROM biz_cost_object \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(CostObjectRef {
            object_no: row.get_typed("object_no")?,
            status: row.get_typed("status")?,
        })),
    }
}

/// 批量取「成本对象 id → 对象号」（列表 / 详情填展示名用，**一次查询**防 N+1）。
///
/// 约定 13：主数据关联类字段返回实时名 —— 对象号是 `biz_cost_object` 的业务键，
/// 改号（本版本只读，但二开可能放开）后列表要立刻跟着变。
pub async fn cost_object_labels(
    ex: &impl SqlExecutor,
    ids: &[Uuid],
) -> DbResult<std::collections::HashMap<Uuid, String>> {
    let mut unique: Vec<Uuid> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    // 与 `user_name::names_of` 同一个写法：`= ANY(?::text::uuid[])`，整页**一条 SQL**。
    let joined = unique.iter().map(Uuid::to_string).collect::<Vec<_>>().join(",");
    let rows = ex
        .query_sql(
            "SELECT id, object_no FROM biz_cost_object \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        map.insert(id, row.get_typed("object_no")?);
    }
    Ok(map)
}

/// 成本对象下拉行（B1-12 出库单表单的选择器；**只有 `OPEN`**）。
#[derive(Debug, Clone)]
pub struct CostObjectOptionRow {
    /// 主键。
    pub id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 产品（物料）。
    pub product_id: Uuid,
    /// 产品编码（实时 join）。
    pub product_code: Option<String>,
    /// 产品名称（实时 join）。
    pub product_name: Option<String>,
    /// 计划数量。
    pub plan_qty: Decimal,
    /// 开工日期。
    pub start_date: Option<NaiveDate>,
}

impl CostObjectOptionRow {
    /// 行 → 出参。
    pub fn to_res(&self) -> super::stockout_dto::CostObjectOptionRes {
        super::stockout_dto::CostObjectOptionRes {
            id: self.id.to_string(),
            object_no: self.object_no.clone(),
            product_id: self.product_id.to_string(),
            product_code: self.product_code.clone(),
            product_name: self.product_name.clone(),
            plan_qty: self.plan_qty.round_dp(4).to_string(),
            start_date: self.start_date.map(|date| date.to_string()),
        }
    }
}

/// 「可挂对象的成本对象」下拉（只 `OPEN`；`keyword` 命中对象号 / 产品编码 / 产品名称）。
///
/// # 为什么这条查询在出库模块，而不在 `cost_object_model`
///
/// 这张下拉服务的是**出库单表单**，权限码走 `business:erp:stock-out:list`。
/// V0378 的授权矩阵把 `cost-object:list` 只授给财务角色（仓管没有），
/// 若下拉去查成本对象页那个接口，仓管打开出库单会吃 403 —— 于是这里给一条
/// 「按出库单权限就能读的最小投影」：只 `OPEN`、只 4 列、上限
/// [`super::stockout_dto::COST_OBJECT_OPTION_LIMIT`] 条。
///
/// 已关批 / 已作废的对象**不出现在下拉里**：挂上去会被 `ensure_cost_object_usable`
/// 409 拦掉（`66057`），列出来只是让用户白点一次。
pub async fn open_cost_objects(ex: &impl SqlExecutor, keyword: Option<&str>) -> DbResult<Vec<CostObjectOptionRow>> {
    let mut sql = String::from(
        "SELECT o.id, o.object_no, o.product_id, m.material_code AS product_code, \
                m.material_name AS product_name, o.plan_qty, o.start_date \
           FROM biz_cost_object o \
           LEFT JOIN biz_material m ON m.id = o.product_id AND m.deleted_at IS NULL \
          WHERE o.deleted_at IS NULL AND o.status = 'OPEN'",
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = like_pattern(keyword) {
        sql.push_str(
            " AND (o.object_no LIKE ? OR COALESCE(m.material_code, '') LIKE ? \
               OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..3 {
            params.push(Value::from(pattern.clone()));
        }
    }
    // 新开的批排前面（同一天开工按对象号兜底，保证顺序稳定）
    sql.push_str(" ORDER BY o.start_date DESC NULLS LAST, o.object_no LIMIT ?");
    params.push(Value::I64(super::stockout_dto::COST_OBJECT_OPTION_LIMIT));
    let rows = ex.query_sql(sql, params).await?;
    rows.iter()
        .map(|row| {
            Ok(CostObjectOptionRow {
                id: row.get_typed("id")?,
                object_no: row.get_typed("object_no")?,
                product_id: row.get_typed("product_id")?,
                product_code: row.get_typed("product_code")?,
                product_name: row.get_typed("product_name")?,
                plan_qty: row.get_typed("plan_qty")?,
                start_date: row.get_typed("start_date")?,
            })
        })
        .collect()
}

/// 关键字 → LIKE 模式（与 `lot_model::like_pattern` 同口径，`%` / `_` 先转义）。
fn like_pattern(keyword: Option<&str>) -> Option<String> {
    keyword
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))
}

// ==================== 一键领料预填（B4-3） ====================

/// 预填的**物料范围**一行：该成本对象历史领用过的物料 + 最近一次的领用数量。
#[derive(Debug, Clone)]
pub struct IssuePrefillMaterialRow {
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码（实时 join）。
    pub material_code: Option<String>,
    /// 物料名称（实时 join）。
    pub material_name: Option<String>,
    /// 最近一次的领用数量。
    pub qty: Decimal,
    /// 最近一次的出库日期。
    pub last_out_date: Option<NaiveDate>,
}

impl IssuePrefillMaterialRow {
    /// 行 → 出参。
    pub fn to_res(&self) -> super::stockout_dto::IssuePrefillMaterialRes {
        super::stockout_dto::IssuePrefillMaterialRes {
            material_id: self.material_id.to_string(),
            material_code: self.material_code.clone(),
            material_name: self.material_name.clone(),
            qty: self.qty.round_dp(4).to_string(),
            last_out_date: self.last_out_date.map(|date| date.to_string()),
        }
    }
}

/// 「一键领料」的物料范围：**该成本对象历史上领用过的物料**（每个物料取最近一次）。
///
/// # 为什么是「历史」而不是 BOM
///
/// 设计 §8.3 R-12 明确一期**不做 BOM 定额控制**，没有 BOM 就没有「这批该领什么」的
/// 客观来源。凭空生成物料行等于替用户编数据（R-1 那类「算出来但不准」的成因），
/// 所以只把事实上领过的料摆出来当默认值，数量也照抄上一次 —— 用户改数比他核对
/// 一串凭空出现的物料便宜。
///
/// # 口径
///
/// * 只认 `out_type='ISSUE'`（其他类型归集不到料，§4.3）；
/// * `CANCELLED` 的作废单不参与（那笔料没进车间）；
/// * 软删的物料**剔除**（`JOIN biz_material … AND deleted_at IS NULL`）：物料被删后
///   出库单会 400，预填一个删掉的物料等于挖坑；
/// * 按 `so.out_date DESC, so.created_at DESC, i.line_no DESC` 取「最近一次」，
///   同一天多单用创建时间兜底，保证结果稳定（同一份数据两次调用必须一样）。
pub async fn issue_prefill_materials(
    ex: &impl SqlExecutor,
    cost_object_id: Uuid,
) -> DbResult<Vec<IssuePrefillMaterialRow>> {
    let rows = ex
        .query_sql(
            "SELECT DISTINCT ON (i.material_id) i.material_id, m.material_code, m.material_name, \
                    i.qty, so.out_date \
               FROM biz_stock_out_item i \
               JOIN biz_stock_out so ON so.id = i.stock_out_id \
               JOIN biz_material m ON m.id = i.material_id AND m.deleted_at IS NULL \
              WHERE so.cost_object_id = ?::uuid \
                AND so.deleted_at IS NULL \
                AND so.out_type = 'ISSUE' \
                AND so.status <> 'CANCELLED' \
              ORDER BY i.material_id, so.out_date DESC, so.created_at DESC, i.line_no DESC \
              LIMIT ?"
                .to_string(),
            vec![
                Value::from(&cost_object_id),
                Value::I64(super::stockout_dto::ISSUE_PREFILL_MATERIAL_LIMIT),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(IssuePrefillMaterialRow {
                material_id: row.get_typed("material_id")?,
                material_code: row.get_typed("material_code")?,
                material_name: row.get_typed("material_name")?,
                qty: row.get_typed("qty")?,
                last_out_date: row.get_typed("out_date")?,
            })
        })
        .collect()
}

/// 预填的建议出库仓库：该成本对象**上一次领料的仓库**（含 `id` + 名称）。
#[derive(Debug, Clone)]
pub struct IssuePrefillWarehouseRow {
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 仓库名称（实时 join，主数据删了就是 `None`）。
    pub warehouse_name: Option<String>,
}

/// 取该对象上一次领料的仓库（没有历史领料 → `None`，前端让用户自己选）。
///
/// 为什么是「上次的领料仓库」而不是成本对象的目标仓（`target_warehouse_id`）：
/// 目标仓是**完工入库**的去处（成品仓），领料是从**原料仓**出 —— 拿成品的仓去预填
/// 领料，用户第一次保存就会被「库存不足」拦下（还可能误开一张跨仓脏单）。
pub async fn last_issue_warehouse(
    ex: &impl SqlExecutor,
    cost_object_id: Uuid,
) -> DbResult<Option<IssuePrefillWarehouseRow>> {
    let row = ex
        .query_first_sql(
            "SELECT so.warehouse_id, w.warehouse_name \
               FROM biz_stock_out so \
               LEFT JOIN biz_warehouse w ON w.id = so.warehouse_id AND w.deleted_at IS NULL \
              WHERE so.cost_object_id = ?::uuid \
                AND so.deleted_at IS NULL \
                AND so.out_type = 'ISSUE' \
                AND so.status <> 'CANCELLED' \
              ORDER BY so.out_date DESC, so.created_at DESC \
              LIMIT 1"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(IssuePrefillWarehouseRow {
            warehouse_id: row.get_typed("warehouse_id")?,
            warehouse_name: row.get_typed("warehouse_name")?,
        })),
    }
}

#[cfg(test)]
mod option_tests {
    use super::like_pattern;

    /// 空白关键字 = 不筛（否则 `%  %` 会查出 0 行，用户以为没数据）。
    #[test]
    fn blank_keyword_yields_no_filter() {
        assert_eq!(like_pattern(None), None);
        assert_eq!(like_pattern(Some("")), None);
        assert_eq!(like_pattern(Some("   ")), None);
    }

    /// 通配符先转义再包 `%`：用户输入 `50%` 不能变成「任意」。
    #[test]
    fn wildcard_characters_are_escaped() {
        assert_eq!(like_pattern(Some(" CB-2026 ")).as_deref(), Some("%CB-2026%"));
        assert_eq!(like_pattern(Some("A_B")).as_deref(), Some("%A\\_B%"));
        assert_eq!(like_pattern(Some("50%")).as_deref(), Some("%50\\%%"));
    }
}

// ==================== 写入：建单 / 改单 / 软删 ====================

/// 取一个出库单号（`OUT-yyyyMMdd-000001`，按天 CST 滚动）。
///
/// 走 `voucher::next_doc_no`：`biz_seq` 行缺失时它幂等补行，时钟回拨也不会撞号
/// （与采购 / 收货 / 付款同一个取号实现，不另写一套）。
pub async fn next_out_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &STOCK_OUT_SEQ, now).await
}

/// 插明细（建单 / 改单共用），行号从 1 连续。
async fn insert_items(ex: &impl SqlExecutor, stock_out_id: Uuid, lines: &[NewStockOutLine]) -> DbResult<()> {
    for (index, line) in lines.iter().enumerate() {
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (id, stock_out_id, line_no, material_id, unit_id, qty, unit_cost, amount, \
                     location_id, lot_no, remark) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, 0, 0, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,''))"
            ),
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&stock_out_id),
                Value::from(i32::try_from(index + 1).unwrap_or(i32::MAX)),
                Value::from(&line.material_id),
                Value::from(&line.unit_id),
                Value::Text(line.qty.round_dp(4).to_string()),
                Value::Text(line.location_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.lot_no.clone().unwrap_or_default()),
                Value::Text(line.remark.clone().unwrap_or_default()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
pub async fn create(
    ex: &impl SqlExecutor,
    new: &NewStockOut<'_>,
    lines: &[NewStockOutLine],
    totals: &Totals,
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let out_no = next_out_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.id.map(|v| v.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, out_no, out_type, warehouse_id, out_date, status, total_qty, total_amount, \
                 reason, remark, source_type, source_id, cost_object_id, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, ?::uuid, ?, '{STATUS_DRAFT}', \
                 NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,''), NULLIF(?::text,''), \
                 NULLIF(?::text,''), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(out_no.clone()),
            Value::from(new.out_type),
            Value::from(&new.warehouse_id),
            Value::from(new.out_date),
            Value::Text(totals.qty.round_dp(4).to_string()),
            Value::Text(new.reason.unwrap_or_default().to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            // 来源（V0285）：手工建单两个都空 → NULL，不参与 uk_biz_so_source。
            // ⚠ 可空 uuid 一律 `NULLIF(?::text,'')::uuid`：驱动不接受空串直绑 uuid。
            Value::Text(new.source_type.unwrap_or_default().to_string()),
            Value::Text(new.source_id.map(|value| value.to_string()).unwrap_or_default()),
            // 成本对象（V0375）：不挂 → NULL（存量口径，过账借方仍 6401）
            Value::Text(new.cost_object_id.map(|value| value.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    insert_items(ex, id, lines).await?;
    // 建单留痕（`from_status = NULL → DRAFT`），与建单**同一事务**：
    // 失败即整单回滚 —— 宁可建不出单，也不要「有单没轨迹」（P10 铁律 ③）。
    state_machine::log_created(
        ex,
        state_machine::ENTITY_STOCK_OUT,
        id,
        &out_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(id)
}

/// 改单（**仅 DRAFT 由调用方保证**）：单头整单覆盖 + 明细「删旧插新」。返回 `affected`。
pub async fn update(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewStockOut<'_>,
    lines: &[NewStockOutLine],
    totals: &Totals,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {TABLE} \
                    SET out_type = ?, warehouse_id = ?::uuid, out_date = ?, \
                        total_qty = NULLIF(?::text,'')::numeric, total_amount = 0, \
                        reason = NULLIF(?::text,''), remark = NULLIF(?::text,''), \
                        cost_object_id = NULLIF(?::text,'')::uuid, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
            ),
            vec![
                Value::from(new.out_type),
                Value::from(&new.warehouse_id),
                Value::from(new.out_date),
                Value::Text(totals.qty.round_dp(4).to_string()),
                Value::Text(new.reason.unwrap_or_default().to_string()),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                // 三态：`None` = 清空成本对象（改单是**整单覆盖**，与 reason/remark 同口径）
                Value::Text(new.cost_object_id.map(|value| value.to_string()).unwrap_or_default()),
                Value::from(now),
                operator_text,
                Value::from(&id),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    // 明细没有 deleted_at，也不该留历史行：整单重建（行号从 1 连续）
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE stock_out_id = ?::uuid"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_items(ex, id, lines).await?;
    Ok(affected)
}

/// 软删（**仅 DRAFT 由调用方保证**），返回 `affected`。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>, now: DateTime<Utc>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET deleted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态 CAS（`WHERE id = ? AND status = from`），返回 `affected`。
///
/// * `→ POSTED`：写 `posted_at / posted_by`（库存变动由 handler 在同一事务里做）；
/// * `→ CANCELLED`：写 `cancelled_at/by/reason`（已过账的回滚同样在 handler 里）；
/// * `→ PENDING_POST` / `→ DRAFT`：只改状态（提交待过账 / 撤回）。
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    let reason_text = reason.unwrap_or_default().to_string();
    let (sql, params) = match to {
        STATUS_POSTED => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_POSTED}', posted_at = ?, \
                        posted_by = NULLIF(?::text,'')::uuid, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_CANCELLED => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_CANCELLED}', cancelled_at = ?, \
                        cancelled_by = NULLIF(?::text,'')::uuid, cancel_reason = ?::text, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::Text(reason_text),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_PENDING_POST => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_PENDING_POST}', updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_DRAFT => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_DRAFT}', updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        other => {
            return Err(DbError::InvalidValue(format!(
                "状态流转的目标 `{other}` 不在 4 个可写目标里（DRAFT 只由建单产生）"
            )));
        }
    };
    ex.exec_sql(sql, params).await
}

/// 回写凭证引用（弱引用，无外键；作废后指向红字凭证）。
pub async fn link_voucher(ex: &impl SqlExecutor, id: Uuid, voucher_id: Uuid, now: DateTime<Utc>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET voucher_id = ?::uuid, updated_at = ? \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![Value::from(&voucher_id), Value::from(now), Value::from(&id)],
    )
    .await
}

/// 回写单头金额合计（过账时按账上真实差额写入）。
pub async fn set_total_amount(ex: &impl SqlExecutor, id: Uuid, amount: Decimal, now: DateTime<Utc>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET total_amount = NULLIF(?::text,'')::numeric, updated_at = ? \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(amount.round_dp(4).to_string()),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

/// 回写明细的过账成本快照（`unit_cost` / `amount`），作废按它原路退回。
pub async fn set_item_cost(
    ex: &impl SqlExecutor,
    item_id: Uuid,
    unit_cost: Decimal,
    amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {ITEM_TABLE} \
                SET unit_cost = NULLIF(?::text,'')::numeric, \
                    amount = NULLIF(?::text,'')::numeric, updated_at = ? \
              WHERE id = ?::uuid"
        ),
        vec![
            Value::Text(unit_cost.to_string()),
            Value::Text(amount.round_dp(4).to_string()),
            Value::from(now),
            Value::from(&item_id),
        ],
    )
    .await
}

// ==================== 三本账：负向变动（过账） ====================

/// 一个库位桶的账面快照（成本账 `biz_inventory` 一行）。
#[derive(Debug, Clone, Copy)]
pub struct BucketSnapshot {
    /// 成本账行 id（`UPDATE ... WHERE id = ?` 用）。
    pub inventory_id: Uuid,
    /// 账面数量。
    pub qty: Decimal,
    /// 账面金额。
    pub amount: Decimal,
    /// 移动加权单价。
    pub avg_cost: Decimal,
}

/// 锁读 `(物料, 仓库, 库位桶)` 的成本账行（`FOR UPDATE`）。
///
/// `location_id` 为 `None` 时命中「未分配库位汇总桶」（`uk_biz_inv_mat_wh_loc` 用
/// 全零 uuid 折叠 NULL，这里用同一表达式，否则走不上索引也锁不到同一行）。
pub async fn lock_bucket(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Option<BucketSnapshot>> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列。
    let row = ex
        .query_first_sql(
            "SELECT id, qty, amount, avg_cost FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                  = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
              FOR UPDATE"
                .to_string(),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(BucketSnapshot {
            inventory_id: row.get_typed("id")?,
            qty: row.get_typed("qty")?,
            amount: row.get_typed("amount")?,
            avg_cost: row.get_typed("avg_cost")?,
        })),
    }
}

/// 成本账一次扣减的结果。
#[derive(Debug, Clone, Copy)]
pub struct AppliedDeduct {
    /// 扣减后账面数量。
    pub after_qty: Decimal,
    /// 扣减后账面金额。
    pub after_amount: Decimal,
    /// 扣减后移动加权单价（`qty <= 0` 时为 0）。
    pub after_cost: Decimal,
    /// **账上真实扣减金额** = `before.amount − after.amount`（凭证与明细都用它）。
    pub amount: Decimal,
}

/// 成本账扣减（**金额是账上差额，不是 `qty × avg` 重算**）。
///
/// # 两条分支
///
/// * 整桶取完（`qty_out >= 账面数量`）：`amount` 归零 —— 避免留下「0 件还有钱」的幽灵金额；
/// * 部分取：`amount −= ROUND(qty_out × avg_cost, 4)`，并用 `GREATEST(…, 0)` 兜住
///   脏账（`amount < qty × avg`）—— 金额绝不减成负数（`ck_biz_inv_txn_amt` 与 B2 都不允许）。
///
/// `avg_cost` 按**剩余金额 / 剩余数量**重算：扣减只从「这一桶的金额池」里拿走对应金额，
/// 拿走之后均价在数学上就是 `(amount − out) / (qty − out)`；原样不动会在
/// 分数位（`ROUND` 到 4 位）上让 `amount ≠ qty × avg_cost`，触发巡检 C4。
pub async fn deduct_cost(
    ex: &impl SqlExecutor,
    bucket: &BucketSnapshot,
    qty_out: Decimal,
    now: DateTime<Utc>,
) -> DbResult<AppliedDeduct> {
    // P2-5A：扣减 SQL（整桶取完金额归零 + GREATEST 夹 0 + 均价重算）收口到
    // [`inventory_writer::deduct_cost`]；「账上真实扣减金额」仍按差额算。
    let after = inventory_writer::deduct_cost(ex, bucket.inventory_id, qty_out, now).await?;
    let amount = (bucket.amount - after.amount).round_dp(4);
    Ok(AppliedDeduct {
        after_qty: after.qty,
        after_amount: after.amount,
        after_cost: after.avg_cost,
        amount,
    })
}

/// **FIFO 选单个库位**（过账时「明细没指定库位」的兜底路径）：从该物料该仓的库位账里
/// 挑「最早建账」且**单个库位够量**的那一行，返回 `(库位账行 id, 该行数量)`。
///
/// # 谁还在用它（P1-FIX-2 之后）
///
/// 正常路径上**用不到了**：建单 / 改单已经把 FIFO 结果预填进明细（跨库位够量时自动拆行，
/// 见 [`allocate_fifo`] / [`fifo_plan`]）。只有「没走建单预填的老单据」「明细被人手工清空库位」
/// 这类情况会落到这里；都不够量时返回 `None`，由调用方给出带缺口与库位分布的 409
/// （见 [`FifoPlan::availability_text`]）。
///
/// 口径（卡面 §二「出库时从哪个库位扣：优先 FIFO，或指定」取**FIFO + 指定**并存）：
///
/// - **FIFO 的排序键**是 `biz_inventory_location.created_at`（= 该库位**第一次**收到
///   这个物料的时间），并列时按行 id。这是当前库位账能做出来的最近似口径 ——
///   逐笔入库时间没有落在库位账上（`biz_inventory_txn` 没有 location 列），
///   真正的批次库龄 FIFO 要等批次卡，这里不假装它是。
/// - `allow_negative = true` 的仓库放宽为「最早建账的那一行」（负库存是它的既定策略）。
pub async fn fifo_location_row(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
    allow_negative: bool,
) -> DbResult<Option<(Option<Uuid>, Decimal)>> {
    let sql = if allow_negative {
        "SELECT id, qty, location_id FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
          ORDER BY created_at ASC, id ASC LIMIT 1 FOR UPDATE"
    } else {
        "SELECT id, qty, location_id FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
            AND qty >= NULLIF(?::text,'')::numeric \
          ORDER BY created_at ASC, id ASC LIMIT 1 FOR UPDATE"
    };
    let mut params = vec![Value::from(&material_id), Value::from(&warehouse_id)];
    if !allow_negative {
        params.push(Value::Text(qty.round_dp(4).to_string()));
    }
    let row = ex.query_first_sql(sql.to_string(), params).await?;
    match row {
        None => Ok(None),
        Some(row) => {
            // ★ 「未分配」桶在成本账里是 `biz_inventory.location_id IS NULL`，**不是**它的行 id：
            //   账行 id 只在「具体库位」口径下与成本账一一对应。命中 NULL 桶时返回值必须是
            //   `None`，否则 lock_bucket 会去 `location_id = <行 id>` 找一行不存在的成本账
            //   （DC-13 的负库存/全链用例当场 409）。
            let ledger_dict: Option<Uuid> = row.get_typed("location_id")?;
            let key = match ledger_dict {
                None => None,
                Some(_) => Some(row.get_typed::<Uuid>("id")?),
            };
            Ok(Some((key, row.get_typed("qty")?)))
        }
    }
}

/// FIFO 选库位的一个桶（该物料在该仓的一个库位账行）。
///
/// `location_row_id = None` = 「未分配」库位桶（`biz_inventory_location.location_id IS NULL`）。
#[derive(Debug, Clone)]
pub struct FifoBucket {
    /// 库位账行 id（`None` = 未分配库位桶）。
    pub location_row_id: Option<Uuid>,
    /// 库位编码（未分配桶显示「未分配」）。
    pub location_code: String,
    /// 可用数量（库位账 `qty`，负数桶不参与拣货）。
    pub available: Decimal,
}

/// FIFO 拣货的一行：从某个桶里取走多少。
#[derive(Debug, Clone, PartialEq)]
pub struct FifoPick {
    /// 库位账行 id（`None` = 未分配库位桶）。
    pub location_row_id: Option<Uuid>,
    /// 库位编码（未分配桶为「未分配」）。
    pub location_code: String,
    /// 本次从该桶取走的数量（> 0）。
    pub take: Decimal,
}

/// 一条出库明细的 FIFO 分配结论。
#[derive(Debug, Clone, Default)]
pub struct FifoPlan {
    /// 该物料该仓的全部可用桶（**FIFO 序**：库位账 `created_at` 升序，并列按行 id）。
    pub buckets: Vec<FifoBucket>,
    /// 逐桶拣货计划（`shortfall > 0` 时只是「够得着的部分」，调用方不要直接落库）。
    pub picks: Vec<FifoPick>,
    /// 各库位可用合计（Σ 库位账 `qty`）。
    pub available: Decimal,
    /// 缺口 = 需求 − 可用（`≤ 0` 表示够量）。
    pub shortfall: Decimal,
}

impl FifoPlan {
    /// 是否够量（够量才允许把 `picks` 落成多行明细）。
    pub fn is_complete(&self) -> bool {
        self.shortfall <= Decimal::ZERO
    }

    /// 各库位可用量的可读摘要（文案用）：`A-01-03: 30、未分配: 20`。
    pub fn availability_text(&self) -> String {
        describe_buckets(&self.buckets)
    }
}

/// 桶列表 → 可读摘要：`A-01-03: 30、未分配: 20`（空列表 → 「无库存」）。
///
/// 只保留一处拼接口径：过账 409 与详情 `stock_warnings` 说的是同一句话，不许两边各写各的。
pub fn describe_buckets(buckets: &[FifoBucket]) -> String {
    if buckets.is_empty() {
        return "无库存".to_string();
    }
    buckets
        .iter()
        .map(|bucket| format!("{}: {}", bucket.location_code, bucket.available.round_dp(4)))
        .collect::<Vec<_>>()
        .join("、")
}

/// **FIFO 贪心分配**（纯函数，不碰库）：按桶的 FIFO 顺序依次取，取满即停。
///
/// 返回 `(逐桶拣货, 缺口)`：缺口 `> 0` 表示所有桶加一起也不够。
///
/// 这是 P1-FIX-2 的核心口径 —— 一条明细跨多个库位时**由系统自动拆成多行**，
/// 不再让用户手工拆行：每行仍然只对应一个库位账行（一行明细 ↔ 一笔流水 ↔ 一个成本账桶），
/// 下游（过账 / 作废原位退回 / 流水行号）都不需要知道「这一行是从多行拆出来的」。
pub fn allocate_fifo(buckets: &[FifoBucket], qty: Decimal) -> (Vec<FifoPick>, Decimal) {
    let mut remaining = qty.round_dp(4);
    let mut picks = Vec::new();
    for bucket in buckets {
        if remaining <= Decimal::ZERO {
            break;
        }
        let take = bucket.available.round_dp(4).min(remaining);
        if take <= Decimal::ZERO {
            continue;
        }
        picks.push(FifoPick {
            location_row_id: bucket.location_row_id,
            location_code: bucket.location_code.clone(),
            take,
        });
        remaining -= take;
    }
    let shortfall = if remaining > Decimal::ZERO {
        remaining.round_dp(4)
    } else {
        Decimal::ZERO
    };
    (picks, shortfall)
}

/// 该物料该仓**可参与拣货的库位桶**（`qty > 0`），按 FIFO 序返回。
///
/// FIFO 的排序键是 `biz_inventory_location.created_at`（= 该库位**第一次**收到这个物料的时间），
/// 并列时按行 id。这是当前库位账能做出来的最近似口径 —— 逐笔入库时间没有落在库位账上
/// （`biz_inventory_txn` 没有 location 列），真正的批次库龄 FIFO 要等批次卡，这里不假装它是。
pub async fn fifo_buckets(ex: &impl SqlExecutor, material_id: Uuid, warehouse_id: Uuid) -> DbResult<Vec<FifoBucket>> {
    let rows = ex
        .query_sql(
            "SELECT il.id, il.qty, il.location_id, \
                    COALESCE(b.location_code, '未分配') AS location_code \
               FROM biz_inventory_location il \
               LEFT JOIN biz_location b ON b.id = il.location_id \
              WHERE il.material_id = ?::uuid AND il.warehouse_id = ?::uuid AND il.qty > 0 \
              ORDER BY il.created_at ASC, il.id ASC"
                .to_string(),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?;
    let mut buckets = Vec::with_capacity(rows.len());
    for row in rows {
        // 「未分配」桶在成本账里是 `biz_inventory.location_id IS NULL`，**不是**它的行 id
        // （与 [`fifo_location_row`] 同口径）：命中它时明细的 `location_id` 必须留空，
        // 否则过账会去 `biz_inventory` 找一行不存在的成本账。
        let ledger_dict: Option<Uuid> = row.get_typed("location_id")?;
        let location_code: String = row.get_typed("location_code")?;
        buckets.push(FifoBucket {
            location_row_id: ledger_dict.map(|_| row.get_typed::<Uuid>("id")).transpose()?,
            location_code,
            available: row.get_typed("qty")?,
        });
    }
    Ok(buckets)
}

/// **FIFO 分配计划**：给一条明细（物料 + 仓库 + 数量）算出逐桶拣货与缺口。
pub async fn fifo_plan(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    qty: Decimal,
) -> DbResult<FifoPlan> {
    let buckets = fifo_buckets(ex, material_id, warehouse_id).await?;
    let available = buckets
        .iter()
        .fold(Decimal::ZERO, |acc, bucket| acc + bucket.available)
        .round_dp(4);
    let (picks, shortfall) = allocate_fifo(&buckets, qty);
    Ok(FifoPlan {
        buckets,
        picks,
        available,
        shortfall,
    })
}

/// 库位账行 id → 库位编码（用于「该库位不够」的 409 文案）。
///
/// `None`（未分配桶）或行已不存在 → 「未分配」/「已删除的库位账行」。
pub async fn location_label(ex: &impl SqlExecutor, location_row_id: Option<Uuid>) -> DbResult<String> {
    let Some(row_id) = location_row_id else {
        return Ok("未分配".to_string());
    };
    let row = ex
        .query_first_sql(
            "SELECT COALESCE(b.location_code, '未分配') AS location_code \
               FROM biz_inventory_location il \
               LEFT JOIN biz_location b ON b.id = il.location_id \
              WHERE il.id = ?::uuid"
                .to_string(),
            vec![Value::from(&row_id)],
        )
        .await?;
    Ok(match row {
        Some(row) => row.get_typed("location_code")?,
        None => "已删除的库位账行".to_string(),
    })
}

/// 过账时把 FIFO 选中的库位账行回写到明细（作废 / 红冲按它原路退回）。
///
/// `None` = 命中的是「未分配」桶（明细的 `location_id` 留空，与成本账的 NULL 桶同口径）。
pub async fn set_item_location(
    ex: &impl SqlExecutor,
    item_id: Uuid,
    location_row_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    ex.exec_sql(
        "UPDATE biz_stock_out_item SET location_id = NULLIF(?::text,'')::uuid, updated_at = ? \
          WHERE id = ?::uuid"
            .to_string(),
        vec![
            Value::Text(location_row_id.map(|id| id.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::from(&item_id),
        ],
    )
    .await?;
    Ok(())
}

/// 锁读库位账行（`biz_inventory_location`），返回 `(行 id, 数量)`。
///
/// `location_id = None` → 未分配库位桶（`location_id IS NULL` 的那一行，
/// `uk_biz_inv_loc … NULLS NOT DISTINCT` 保证最多一行）。
pub async fn lock_location_row(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Option<(Uuid, Decimal)>> {
    // P2-5A：锁读 SQL 收口到 [`inventory_writer::lock_location`]。
    inventory_writer::lock_location(ex, material_id, warehouse_id, location_id).await
}

/// 库位账扣减（只记数量，不记成本）。返回扣减后的数量。
pub async fn deduct_location_qty(
    ex: &impl SqlExecutor,
    location_row_id: Uuid,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<Decimal> {
    // P2-5A：扣减 SQL 收口到 [`inventory_writer::deduct_location_qty`]。
    inventory_writer::deduct_location_qty(ex, location_row_id, qty, now).await
}

/// [`write_txn`] 的入参（`ex` 之外的 14 项）。
///
/// 一条出库流水（`OUT`）需要的全部事实：哪条明细 / 哪个仓 / 指向哪张单 / 业务类型 /
/// 扣减前后的库存三值 / 计价 / 操作人 / 时间。
#[derive(Debug, Clone, Copy)]
pub struct StockOutTxnInput<'a> {
    /// 出库明细行（提供 `material_id` / `line_no` / `qty`）。
    pub item: &'a StockOutItemRow,
    /// 仓库主键。
    pub warehouse_id: Uuid,
    /// 单据主键。
    pub doc_id: Uuid,
    /// 单据号（流水上留可读号）。
    pub doc_no: &'a str,
    /// 业务类型（`SALE_OUT` / `OTHER_OUT` / …，落 `biz_type`，作废闸按它认流水）。
    pub biz_type: &'a str,
    /// 扣减前的库存数量。
    pub before_qty: Decimal,
    /// 扣减前的库存金额。
    pub before_amount: Decimal,
    /// 扣减前的移动加权均价。
    pub before_cost: Decimal,
    /// 扣减后的库存数量。
    pub after_qty: Decimal,
    /// 扣减后的库存金额。
    pub after_amount: Decimal,
    /// 扣减后的移动加权均价。
    pub after_cost: Decimal,
    /// 本次出库的计价单价（落流水 `unit_cost`）。
    pub unit_cost: Decimal,
    /// 本次出库的成本金额（落流水 `amount`）。
    pub amount: Decimal,
    /// 操作人（落 `create_by`；`None` = 系统 / 测试）。
    pub operator: Option<Uuid>,
    /// 业务时间（落 `txn_date` / `created_at`）。
    pub now: DateTime<Utc>,
}

/// 写一条出库流水（`txn_type='OUT'`，数量与金额恒正，方向看 `txn_type`）。
pub async fn write_txn(ex: &impl SqlExecutor, input: StockOutTxnInput<'_>) -> DbResult<()> {
    let StockOutTxnInput {
        item,
        warehouse_id,
        doc_id,
        doc_no,
        biz_type,
        before_qty,
        before_amount,
        before_cost,
        after_qty,
        after_amount,
        after_cost,
        unit_cost,
        amount,
        operator,
        now,
    } = input;
    // P2-5A：流水 SQL 收口到 [`inventory_writer::insert_txn`]。
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id: item.material_id,
            warehouse_id,
            txn_type: TXN_TYPE_OUT,
            biz_type,
            doc_type: DOC_TYPE_STOCK_OUT,
            doc_id,
            doc_no,
            line_no: item.line_no,
            qty: item.qty.round_dp(4),
            unit_cost,
            amount: amount.round_dp(4),
            before: InventorySnapshot {
                qty: before_qty,
                amount: before_amount,
                avg_cost: before_cost,
            },
            after: InventorySnapshot {
                qty: after_qty,
                amount: after_amount,
                avg_cost: after_cost,
            },
            operator,
            period_code: None,
            remark: None,
            now,
        },
    )
    .await
    .map(|_| ())
}

// ==================== 三本账：正向回滚（作废） ====================

/// 成本账回加（作废出库单：把当时扣掉的 `qty` 与 `amount` 原路加回）。
///
/// 金额取**作废时的明细 `amount` 快照**（过账时写入的账上真实差额），
/// 不用当前 `avg_cost × qty` 重算：期间又进过货，均价已变，用现价加回会污染成本
/// （与 `stockin_cancel::reverse_cost` 同一条硬口径）。
pub async fn restore_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<()> {
    // P2-5A：回加 SQL（按桶键加 + 缺行建档）收口到 [`inventory_writer::restore_cost`]。
    inventory_writer::restore_cost(ex, material_id, warehouse_id, location_id, qty, amount, now).await
}

/// 库位账回加（作废出库单）。
///
/// 行存在 → `qty += 本次数量`；行不存在 → 建档（`location_id` 用明细上那个库位账行的
/// 库位字典 id —— 这里为了不引入第二个 id 语义，**按原行 id 找回**；找不到就只补数量桶）。
pub async fn restore_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<()> {
    // P2-5A：回加 SQL（按行 id / 未分配桶两条分支 + 缺行建档）收口到
    // [`inventory_writer::restore_location_qty`]。
    inventory_writer::restore_location_qty(ex, material_id, warehouse_id, location_id, qty, now).await
}

/// [`write_cancel_txn`] 的入参（`ex` 之外的 12 项）。
///
/// 冲销流水不需要 `biz_type` / 计价（单价与金额取明细上的过账快照）：
/// 方向、业务类型、成本都已被「作废」这一件事定死，调用方只负责给对 before/after。
#[derive(Debug, Clone, Copy)]
pub struct StockOutCancelTxnInput<'a> {
    /// 出库明细行（提供 `material_id` / `line_no` / `qty` / 过账成本快照）。
    pub item: &'a StockOutItemRow,
    /// 仓库主键。
    pub warehouse_id: Uuid,
    /// 单据主键。
    pub doc_id: Uuid,
    /// 单据号（冲销流水摘要里带它）。
    pub doc_no: &'a str,
    /// 回加前的库存数量（作废前的账面）。
    pub before_qty: Decimal,
    /// 回加前的库存金额（作废前的账面）。
    pub before_amount: Decimal,
    /// 回加前的移动加权均价（作废前的账面）。
    pub before_cost: Decimal,
    /// 回加后的库存数量（回读的真实值）。
    pub after_qty: Decimal,
    /// 回加后的库存金额（回读的真实值）。
    pub after_amount: Decimal,
    /// 回加后的移动加权均价（回读的真实值）。
    pub after_cost: Decimal,
    /// 操作人（落 `create_by`；`None` = 系统 / 测试）。
    pub operator: Option<Uuid>,
    /// 业务时间（落 `txn_date` / `created_at`）。
    pub now: DateTime<Utc>,
}

/// 写作废冲销流水（`txn_type='IN'` + `biz_type='STOCK_OUT_CANCEL'`）。
///
/// 必须有这条：作废把数量加回账上，B4（流水累计 = 当前库存）靠它才平。
/// 方向用 `IN`（`ck_biz_inv_txn_type` 只认 IN/OUT，且 `qty > 0` 禁负数）。
pub async fn write_cancel_txn(ex: &impl SqlExecutor, input: StockOutCancelTxnInput<'_>) -> DbResult<()> {
    let StockOutCancelTxnInput {
        item,
        warehouse_id,
        doc_id,
        doc_no,
        before_qty,
        before_amount,
        before_cost,
        after_qty,
        after_amount,
        after_cost,
        operator,
        now,
    } = input;
    // P2-5A：流水 SQL 收口到 [`inventory_writer::insert_txn`]。
    let remark = format!("出库单作废冲销：{doc_no}");
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id: item.material_id,
            warehouse_id,
            txn_type: TXN_TYPE_IN,
            biz_type: BIZ_TYPE_STOCK_OUT_CANCEL,
            doc_type: DOC_TYPE_STOCK_OUT,
            doc_id,
            doc_no,
            line_no: item.line_no,
            qty: item.qty.round_dp(4),
            unit_cost: item.unit_cost,
            amount: item.amount.round_dp(4),
            before: InventorySnapshot {
                qty: before_qty,
                amount: before_amount,
                avg_cost: before_cost,
            },
            after: InventorySnapshot {
                qty: after_qty,
                amount: after_amount,
                avg_cost: after_cost,
            },
            operator,
            period_code: None,
            remark: Some(remark.as_str()),
            now,
        },
    )
    .await
    .map(|_| ())
}

/// 该单的过账流水是否齐（作废前的数据完整性闸）。
///
/// 返回每条明细「过账流水合计数量」与明细数量是否一致的报告；不一致 / 缺流水
/// 一律让调用方报 409 —— **绝不按现价估算**（那是污染成本的捷径）。
pub async fn txn_report(ex: &impl SqlExecutor, doc_id: Uuid) -> DbResult<Vec<(i32, Decimal, Decimal, i64)>> {
    // SCOPE-EXEMPT: 库存流水是仓储台账，无归属列。
    // biz_type 列表直接取自 `stockout_base::OUT_TXN_BIZ_TYPES`（含销售出库的 SALE_OUT）：
    // 漏一个值 = 作废前完整性闸看不见那条流水（FIX-7），所以不在这里再抄一遍清单。
    let biz_types = super::stockout_base::OUT_TXN_BIZ_TYPES;
    let marks = vec!["?"; biz_types.len()].join(", ");
    let mut params = vec![
        Value::from(DOC_TYPE_STOCK_OUT),
        Value::from(&doc_id),
        Value::from(TXN_TYPE_OUT),
    ];
    params.extend(biz_types.iter().map(|value| Value::from(*value)));
    let rows = ex
        .query_sql(
            format!(
                "SELECT line_no, sum(qty) AS txn_qty, sum(amount) AS txn_amount, count(*) AS cnt \
                   FROM biz_inventory_txn \
                  WHERE doc_type = ? AND doc_id = ?::uuid AND txn_type = ? \
                    AND biz_type IN ({marks}) \
                  GROUP BY line_no ORDER BY line_no"
            ),
            params,
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        out.push((
            row.get_typed("line_no")?,
            row.get_typed("txn_qty")?,
            row.get_typed("txn_amount")?,
            row.get_typed("cnt")?,
        ));
    }
    Ok(out)
}

/// 库位账行归属校验（明细上的 `location_id` 必须属于本次的物料 / 仓库）。
pub async fn location_ref(ex: &impl SqlExecutor, location_row_id: Uuid) -> DbResult<Option<(Uuid, Uuid)>> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列。
    let row = ex
        .query_first_sql(
            "SELECT material_id, warehouse_id FROM biz_inventory_location WHERE id = ?::uuid".to_string(),
            vec![Value::from(&location_row_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("material_id")?, row.get_typed("warehouse_id")?))),
    }
}

/// 单据出库日期对应会计期间是否已结账（过账闸，错在入口而不是在凭证层）。
pub async fn period_closed(ex: &impl SqlExecutor, date: NaiveDate) -> DbResult<Option<(String, String)>> {
    let period = voucher::period_of(ex, date).await?;
    let status = voucher::period_status(ex, &period).await?;
    Ok(status.map(|status| (period, status)))
}

#[cfg(test)]
#[path = "stockout_tests.rs"]
mod tests;

/// P1-FIX-2：出库 FIFO 自动分配（建单预填 / 跨库位自动拆行 / 库存不足文案）。只在 `cargo test` 时编译。
#[cfg(test)]
#[path = "stockout_fifo_tests.rs"]
mod fifo_tests;

/// B1-10：领料单挂成本对象（对象 OPEN 校验 + 借方科目复算 + 过账 4001.01）。只在 `cargo test` 时编译。
#[cfg(test)]
#[path = "stockout_cost_object_tests.rs"]
mod cost_object_tests;

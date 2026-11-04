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

use super::stockout_base::*;

// ==================== 只读 ====================

/// 按主键取单头（未删除）。
pub async fn find(id: Uuid) -> DbResult<Option<StockOutRow>> {
    let sql =
        format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE id = ?::uuid AND deleted_at IS NULL");
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
    let result =
        scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size)
            .await?;
    let rows = result
        .list
        .iter()
        .map(StockOutRow::from_row)
        .collect::<DbResult<Vec<StockOutRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// DTO 里的 id 是 `String`：非法 UUID 一律报错（不静默跳过条件）。
pub(super) fn parse_id(value: &str) -> DbResult<Uuid> {
    Uuid::parse_str(value.trim())
        .map_err(|err| DbError::InvalidValue(format!("非法 UUID `{value}`：{err}")))
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
pub async fn material_info(
    ex: &impl SqlExecutor,
    material_id: Uuid,
) -> DbResult<Option<MaterialInfo>> {
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
pub async fn warehouse_policy(
    ex: &impl SqlExecutor,
    warehouse_id: Uuid,
) -> DbResult<Option<WarehousePolicy>> {
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

// ==================== 写入：建单 / 改单 / 软删 ====================

/// 取一个出库单号（`OUT-yyyyMMdd-000001`，按天 CST 滚动）。
///
/// 走 `voucher::next_doc_no`：`biz_seq` 行缺失时它幂等补行，时钟回拨也不会撞号
/// （与采购 / 收货 / 付款同一个取号实现，不另写一套）。
pub async fn next_out_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &STOCK_OUT_SEQ, now).await
}

/// 插明细（建单 / 改单共用），行号从 1 连续。
async fn insert_items(
    ex: &impl SqlExecutor,
    stock_out_id: Uuid,
    lines: &[NewStockOutLine],
) -> DbResult<()> {
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
                 reason, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, ?::uuid, ?, '{STATUS_DRAFT}', \
                 NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,''), NULLIF(?::text,''), \
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
                        reason = NULLIF(?::text,''), remark = NULLIF(?::text,''), updated_at = ?, \
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
pub async fn soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
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
pub async fn link_voucher(
    ex: &impl SqlExecutor,
    id: Uuid,
    voucher_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<u64> {
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
pub async fn set_total_amount(
    ex: &impl SqlExecutor,
    id: Uuid,
    amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<u64> {
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
    let qty_text = qty_out.round_dp(4).to_string();
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列。
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory \
                SET qty = qty - NULLIF(?::text,'')::numeric, \
                    amount = CASE WHEN NULLIF(?::text,'')::numeric >= qty THEN 0 \
                                  ELSE GREATEST(amount - ROUND(NULLIF(?::text,'')::numeric * avg_cost, 4), 0) END, \
                    avg_cost = CASE WHEN qty - NULLIF(?::text,'')::numeric > 0 \
                                    THEN ROUND( \
                                        CASE WHEN NULLIF(?::text,'')::numeric >= qty THEN 0 \
                                             ELSE GREATEST(amount - ROUND(NULLIF(?::text,'')::numeric * avg_cost, 4), 0) END \
                                        / (qty - NULLIF(?::text,'')::numeric), 6) \
                                    ELSE 0 END, \
                    last_txn_at = ?, updated_at = ? \
              WHERE id = ?::uuid \
             RETURNING qty, amount, avg_cost"
                .to_string(),
            vec![
                Value::Text(qty_text.clone()), // $1 qty 减法
                Value::Text(qty_text.clone()), // $2 整桶取完判断
                Value::Text(qty_text.clone()), // $3 金额减数
                Value::Text(qty_text.clone()), // $4 avg 的 CASE 判断
                Value::Text(qty_text.clone()), // $5 整桶取完判断（avg 分支）
                Value::Text(qty_text.clone()), // $6 金额减数（avg 分支）
                Value::Text(qty_text),         // $7 avg 分母
                Value::from(now),              // $8 last_txn_at
                Value::from(now),              // $9 updated_at
                Value::from(&bucket.inventory_id), // $10
            ],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue(
                "出库扣减成本账未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into(),
            )
        })?;
    let after_amount: Decimal = row.get_typed("amount")?;
    let amount = (bucket.amount - after_amount).round_dp(4);
    Ok(AppliedDeduct {
        after_qty: row.get_typed("qty")?,
        after_amount,
        after_cost: row.get_typed("avg_cost")?,
        amount,
    })
}

/// **FIFO 选库位**（P2-4 出库策略）：明细没指定库位时，从该物料该仓的库位账里
/// 挑「最早建账」且**单个库位够量**的那一行，返回 `(库位账行 id, 该行数量)`。
///
/// 口径（卡面 §二「出库时从哪个库位扣：优先 FIFO，或指定」取**FIFO + 指定**并存）：
///
/// - **FIFO 的排序键**是 `biz_inventory_location.created_at`（= 该库位**第一次**收到
///   这个物料的时间），并列时按行 id。这是当前库位账能做出来的最近似口径 ——
///   逐笔入库时间没有落在库位账上（`biz_inventory_txn` 没有 location 列），
///   真正的批次库龄 FIFO 要等批次卡，这里不假装它是。
/// - **只挑够量的库位**：一次出库只扣一个库位桶（明细只有一个 `location_id` 字段，
///   拆桶会让「一行明细 ↔ 一笔流水」的对应关系断掉）。都不够量 → 返回 `None`，
///   由调用方给出「分散在 N 个库位、请拆行或指定库位」的可读 409（见
///   [`location_availability`]）。
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

/// 该物料该仓**各库位的可用量**（`(库位编码或「未分配」, 数量)`，按数量降序）。
///
/// 只用于 FIFO 失败时的 409 文案：「分散在 A-01-03: 30 / A-01-04: 20，单个库位都不够 60」。
pub async fn location_availability(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Vec<(String, Decimal)>> {
    let rows = ex
        .query_sql(
            "SELECT COALESCE(b.location_code, '未分配') AS label, il.qty \
               FROM biz_inventory_location il \
               LEFT JOIN biz_location b ON b.id = il.location_id \
              WHERE il.material_id = ?::uuid AND il.warehouse_id = ?::uuid \
              ORDER BY il.qty DESC, il.created_at ASC"
                .to_string(),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in rows {
        list.push((row.get_typed("label")?, row.get_typed("qty")?));
    }
    Ok(list)
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
    let sql = match location_id {
        Some(_) => {
            "SELECT id, qty FROM biz_inventory_location WHERE id = ?::uuid FOR UPDATE".to_string()
        }
        None => "SELECT id, qty FROM biz_inventory_location \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
                  FOR UPDATE"
            .to_string(),
    };
    let params = match location_id {
        Some(row_id) => vec![Value::from(&row_id)],
        None => vec![Value::from(&material_id), Value::from(&warehouse_id)],
    };
    let row = ex.query_first_sql(sql, params).await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("id")?, row.get_typed("qty")?))),
    }
}

/// 库位账扣减（只记数量，不记成本）。返回扣减后的数量。
pub async fn deduct_location_qty(
    ex: &impl SqlExecutor,
    location_row_id: Uuid,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<Decimal> {
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory_location \
                SET qty = qty - NULLIF(?::text,'')::numeric, last_txn_at = ?, updated_at = ? \
              WHERE id = ?::uuid RETURNING qty"
                .to_string(),
            vec![
                Value::Text(qty.round_dp(4).to_string()),
                Value::from(now),
                Value::from(now),
                Value::from(&location_row_id),
            ],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue(
                "出库扣减库位账未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into(),
            )
        })?;
    row.get_typed("qty")
}

/// 写一条出库流水（`txn_type='OUT'`，数量与金额恒正，方向看 `txn_type`）。
#[allow(clippy::too_many_arguments)]
pub async fn write_txn(
    ex: &impl SqlExecutor,
    item: &StockOutItemRow,
    warehouse_id: Uuid,
    doc_id: Uuid,
    doc_no: &str,
    biz_type: &str,
    before_qty: Decimal,
    before_amount: Decimal,
    before_cost: Decimal,
    after_qty: Decimal,
    after_amount: Decimal,
    after_cost: Decimal,
    unit_cost: Decimal,
    amount: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let period_code = super::serial_model::period_code_of(now);
    // SCOPE-EXEMPT: 库存流水是仓储台账，无归属列。
    ex.exec_sql(
        "INSERT INTO biz_inventory_txn \
             (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, \
              qty, unit_cost, amount, before_qty, before_amount, before_cost, \
              after_qty, after_amount, after_cost, period_code, operator_id, created_at) \
         VALUES (?, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,'')::uuid, ?)"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&item.material_id),
            Value::from(&warehouse_id),
            Value::from(TXN_TYPE_OUT),
            Value::from(biz_type),
            Value::from(DOC_TYPE_STOCK_OUT),
            Value::from(&doc_id),
            Value::from(doc_no),
            Value::from(item.line_no),
            Value::Text(item.qty.round_dp(4).to_string()),
            Value::Text(unit_cost.to_string()),
            Value::Text(amount.round_dp(4).to_string()),
            Value::Text(before_qty.to_string()),
            Value::Text(before_amount.to_string()),
            Value::Text(before_cost.to_string()),
            Value::Text(after_qty.to_string()),
            Value::Text(after_amount.to_string()),
            Value::Text(after_cost.to_string()),
            Value::from(period_code.as_str()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
        ],
    )
    .await?;
    Ok(())
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
    let qty_text = qty.round_dp(4).to_string();
    let amount_text = amount.round_dp(4).to_string();
    let location_text = Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default());
    // 行已存在 → 加数量 + 加金额 + 重算均价；行不存在（历史缺口）→ 建档
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory \
                SET qty = qty + NULLIF(?::text,'')::numeric, \
                    amount = amount + NULLIF(?::text,'')::numeric, \
                    avg_cost = CASE WHEN qty + NULLIF(?::text,'')::numeric > 0 \
                                    THEN ROUND((amount + NULLIF(?::text,'')::numeric) \
                                               / (qty + NULLIF(?::text,'')::numeric), 6) \
                                    ELSE 0 END, \
                    last_txn_at = ?, updated_at = ? \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                  = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
             RETURNING id"
                .to_string(),
            vec![
                Value::Text(qty_text.clone()),    // $1 qty 加法
                Value::Text(amount_text.clone()), // $2 金额加法
                Value::Text(qty_text.clone()),    // $3 avg 判断
                Value::Text(amount_text.clone()), // $4 avg 分子
                Value::Text(qty_text.clone()),    // $5 avg 分母
                Value::from(now),
                Value::from(now),
                Value::from(&material_id),
                Value::from(&warehouse_id),
                location_text,
            ],
        )
        .await?;
    if row.is_some() {
        return Ok(());
    }
    // 建档（成本账行被硬删过的历史缺口）：数量与金额直接用本次退回值
    ex.exec_sql(
        "INSERT INTO biz_inventory \
             (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, \
              last_txn_at, created_at, updated_at) \
         VALUES (?, ?::uuid, ?::uuid, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,'')::numeric, 0, \
                 CASE WHEN NULLIF(?::text,'')::numeric > 0 \
                      THEN ROUND(NULLIF(?::text,'')::numeric / NULLIF(?::text,'')::numeric, 6) \
                      ELSE 0 END, \
                 NULLIF(?::text,'')::numeric, ?, ?, ?)"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::Text(qty_text.clone()),
            Value::Text(amount_text.clone()),
            Value::Text(amount_text),
            Value::Text(qty_text),
            Value::from(now),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await?;
    Ok(())
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
    let qty_text = qty.round_dp(4).to_string();
    let updated = match location_id {
        Some(row_id) => {
            ex.query_first_sql(
                "UPDATE biz_inventory_location SET qty = qty + NULLIF(?::text,'')::numeric, \
                        last_txn_at = ?, updated_at = ? WHERE id = ?::uuid RETURNING id"
                    .to_string(),
                vec![
                    Value::Text(qty_text.clone()),
                    Value::from(now),
                    Value::from(now),
                    Value::from(&row_id),
                ],
            )
            .await?
        }
        None => {
            ex.query_first_sql(
                "UPDATE biz_inventory_location SET qty = qty + NULLIF(?::text,'')::numeric, \
                        last_txn_at = ?, updated_at = ? \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
                 RETURNING id"
                    .to_string(),
                vec![
                    Value::Text(qty_text.clone()),
                    Value::from(now),
                    Value::from(now),
                    Value::from(&material_id),
                    Value::from(&warehouse_id),
                ],
            )
            .await?
        }
    };
    if updated.is_some() {
        return Ok(());
    }
    // 行不存在：往未分配库位桶建档（`ON CONFLICT` 目标与 uk_biz_inv_loc 一致，NULL 也命中）
    ex.exec_sql(
        "INSERT INTO biz_inventory_location \
             (id, material_id, warehouse_id, location_id, qty, locked_qty, last_txn_at, created_at, updated_at) \
         VALUES (?, ?::uuid, ?::uuid, NULL, NULLIF(?::text,'')::numeric, 0, ?, ?, ?) \
         ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET \
             qty = biz_inventory_location.qty + EXCLUDED.qty, \
             last_txn_at = EXCLUDED.last_txn_at, updated_at = EXCLUDED.updated_at"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(qty_text),
            Value::from(now),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await?;
    Ok(())
}

/// 写作废冲销流水（`txn_type='IN'` + `biz_type='STOCK_OUT_CANCEL'`）。
///
/// 必须有这条：作废把数量加回账上，B4（流水累计 = 当前库存）靠它才平。
/// 方向用 `IN`（`ck_biz_inv_txn_type` 只认 IN/OUT，且 `qty > 0` 禁负数）。
#[allow(clippy::too_many_arguments)]
pub async fn write_cancel_txn(
    ex: &impl SqlExecutor,
    item: &StockOutItemRow,
    warehouse_id: Uuid,
    doc_id: Uuid,
    doc_no: &str,
    before_qty: Decimal,
    before_amount: Decimal,
    before_cost: Decimal,
    after_qty: Decimal,
    after_amount: Decimal,
    after_cost: Decimal,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let period_code = super::serial_model::period_code_of(now);
    // SCOPE-EXEMPT: 库存流水是仓储台账，无归属列。
    ex.exec_sql(
        "INSERT INTO biz_inventory_txn \
             (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, \
              qty, unit_cost, amount, before_qty, before_amount, before_cost, \
              after_qty, after_amount, after_cost, period_code, operator_id, created_at, remark) \
         VALUES (?, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,'')::uuid, ?, ?)"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&item.material_id),
            Value::from(&warehouse_id),
            Value::from(TXN_TYPE_IN),
            Value::from(BIZ_TYPE_STOCK_OUT_CANCEL),
            Value::from(DOC_TYPE_STOCK_OUT),
            Value::from(&doc_id),
            Value::from(doc_no),
            Value::from(item.line_no),
            Value::Text(item.qty.round_dp(4).to_string()),
            Value::Text(item.unit_cost.to_string()),
            Value::Text(item.amount.round_dp(4).to_string()),
            Value::Text(before_qty.to_string()),
            Value::Text(before_amount.to_string()),
            Value::Text(before_cost.to_string()),
            Value::Text(after_qty.to_string()),
            Value::Text(after_amount.to_string()),
            Value::Text(after_cost.to_string()),
            Value::from(period_code.as_str()),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(now),
            Value::Text(format!("出库单作废冲销：{doc_no}")),
        ],
    )
    .await?;
    Ok(())
}

/// 该单的过账流水是否齐（作废前的数据完整性闸）。
///
/// 返回每条明细「过账流水合计数量」与明细数量是否一致的报告；不一致 / 缺流水
/// 一律让调用方报 409 —— **绝不按现价估算**（那是污染成本的捷径）。
pub async fn txn_report(
    ex: &impl SqlExecutor,
    doc_id: Uuid,
) -> DbResult<Vec<(i32, Decimal, Decimal, i64)>> {
    // SCOPE-EXEMPT: 库存流水是仓储台账，无归属列。
    let rows = ex
        .query_sql(
            "SELECT line_no, sum(qty) AS txn_qty, sum(amount) AS txn_amount, count(*) AS cnt \
               FROM biz_inventory_txn \
              WHERE doc_type = ? AND doc_id = ?::uuid AND txn_type = ? \
                AND biz_type IN (?, ?, ?, ?) \
              GROUP BY line_no ORDER BY line_no"
                .to_string(),
            vec![
                Value::from(DOC_TYPE_STOCK_OUT),
                Value::from(&doc_id),
                Value::from(TXN_TYPE_OUT),
                Value::from(BIZ_TYPE_ISSUE),
                Value::from(BIZ_TYPE_SCRAP),
                Value::from(BIZ_TYPE_LOSS),
                Value::from(BIZ_TYPE_OTHER),
            ],
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
pub async fn location_ref(
    ex: &impl SqlExecutor,
    location_row_id: Uuid,
) -> DbResult<Option<(Uuid, Uuid)>> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列。
    let row = ex
        .query_first_sql(
            "SELECT material_id, warehouse_id FROM biz_inventory_location WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&location_row_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get_typed("material_id")?,
            row.get_typed("warehouse_id")?,
        ))),
    }
}

/// 单据出库日期对应会计期间是否已结账（过账闸，错在入口而不是在凭证层）。
pub async fn period_closed(
    ex: &impl SqlExecutor,
    date: NaiveDate,
) -> DbResult<Option<(String, String)>> {
    let period = voucher::period_of(ex, date).await?;
    let status = voucher::period_status(ex, &period).await?;
    Ok(status.map(|status| (period, status)))
}

#[cfg(test)]
#[path = "stockout_tests.rs"]
mod tests;

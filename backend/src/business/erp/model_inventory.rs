// ============================================================================
// model_inventory.rs —— 库存台账只读：两级库存账、库存余额、库存流水
//
// 由 `model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::{InventoryFilter, InventoryLocationFilter, InventoryTxnFilter};
use crate::business::shared::scope;

use super::model_base::*;

/// `biz_inventory` 列清单。
///
/// **成本账**：`(material_id, warehouse_id, location_id)` 唯一（`uk_biz_inv_mat_wh_loc`，V0139 起为三列 + NULL 折叠），放数量合计与成本；
/// 库位维度在 [`InventoryLocationRow`]（两级账，见 [`inventory_balance`]）。
pub const INVENTORY_COLUMNS: &str = "id, material_id, warehouse_id, qty, locked_qty, avg_cost, \
     amount, last_txn_at, created_at, updated_at";

/// 即时库存行（物料 × 仓库）。
#[derive(Debug, Clone)]
pub struct InventoryRow {
    /// 主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 账面数量（`NUMERIC(18,4)`）。
    pub qty: Decimal,
    /// 锁定量（`>= 0`）。
    pub locked_qty: Decimal,
    /// 移动加权平均成本（`NUMERIC(18,6)`）。
    pub avg_cost: Decimal,
    /// 账面金额（`NUMERIC(18,4)`）。
    pub amount: Decimal,
    /// 最近一次流水时间。
    pub last_txn_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl InventoryRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            qty: row.get_typed("qty")?,
            locked_qty: row.get_typed("locked_qty")?,
            avg_cost: row.get_typed("avg_cost")?,
            amount: row.get_typed("amount")?,
            last_txn_at: row.get_typed("last_txn_at")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_inventory` 的分页锚点。
pub const INVENTORY_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_inventory");

/// 库存列表排序：物料 → 仓库（与唯一索引 `uk_biz_inv_mat_wh_loc` 前两列同序）。
pub const INVENTORY_ORDER: &str = "ORDER BY material_id ASC, warehouse_id ASC";

/// 按「物料 × 仓库」取即时库存（两级账的成本侧）。
pub async fn inventory_find(
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Option<InventoryRow>> {
    let sql = format!(
        "SELECT {INVENTORY_COLUMNS} FROM biz_inventory WHERE material_id = ? AND warehouse_id = ?"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![Value::from(material_id), Value::from(warehouse_id)],
        )
        .await?;
    rows.first().map(InventoryRow::from_row).transpose()
}

/// 即时库存分页（按物料 / 仓库）。
pub async fn inventory_page(q: &InventoryFilter) -> DbResult<(Vec<InventoryRow>, i64)> {
    let mut sql = format!("SELECT {INVENTORY_COLUMNS} FROM biz_inventory WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if q.only_nonzero {
        sql.push_str(" AND qty <> 0");
    }
    page_of(
        INVENTORY_TARGET,
        sql,
        params,
        INVENTORY_ORDER,
        q.page,
        q.page_size,
        InventoryRow::from_row,
    )
    .await
}

// ==================== ⑫ biz_inventory_location 库位库存（数量账） ====================

/// `biz_inventory_location` 列清单。
///
/// **数量账**：`(material_id, warehouse_id, location_id)` 唯一
/// （`uk_biz_inv_loc … NULLS NOT DISTINCT`——`location_id` 为 NULL 时只允许一行）。
pub const INVENTORY_LOCATION_COLUMNS: &str = "id, material_id, warehouse_id, location_id, qty, \
     locked_qty, last_txn_at, created_at, updated_at";

/// 库位库存行。
#[derive(Debug, Clone)]
pub struct InventoryLocationRow {
    /// 主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 库位（`None` = 该仓库未分配到具体库位的数量）。
    pub location_id: Option<Uuid>,
    /// 库位数量（`NUMERIC(18,4)`）。
    pub qty: Decimal,
    /// 库位锁定量（`>= 0`）。
    pub locked_qty: Decimal,
    /// 最近一次流水时间。
    pub last_txn_at: Option<DateTime<Utc>>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl InventoryLocationRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            location_id: row.get_typed("location_id")?,
            qty: row.get_typed("qty")?,
            locked_qty: row.get_typed("locked_qty")?,
            last_txn_at: row.get_typed("last_txn_at")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_inventory_location` 的分页锚点。
pub const INVENTORY_LOCATION_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::unscoped("biz_inventory_location");

/// 库位库存排序：物料 → 仓库 → 库位。
pub const INVENTORY_LOCATION_ORDER: &str =
    "ORDER BY material_id ASC, warehouse_id ASC, location_id ASC";

/// 取某物料在某仓库下的**全部库位库存**（数量账的明细来源）。
pub async fn inventory_locations(
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Vec<InventoryLocationRow>> {
    let sql = format!(
        "SELECT {INVENTORY_LOCATION_COLUMNS} FROM biz_inventory_location \
         WHERE material_id = ? AND warehouse_id = ? ORDER BY location_id ASC"
    );
    let rows = db()
        .find_with_params(
            &sql,
            vec![Value::from(material_id), Value::from(warehouse_id)],
        )
        .await?;
    rows.iter().map(InventoryLocationRow::from_row).collect()
}

/// 库位库存分页（按物料 / 仓库 / 库位）。
pub async fn inventory_location_page(
    q: &InventoryLocationFilter,
) -> DbResult<(Vec<InventoryLocationRow>, i64)> {
    let mut sql =
        format!("SELECT {INVENTORY_LOCATION_COLUMNS} FROM biz_inventory_location WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(location_id) = q.location_id.as_deref() {
        sql.push_str(" AND location_id = ?");
        params.push(Value::from(parse_id(location_id)?));
    }
    page_of(
        INVENTORY_LOCATION_TARGET,
        sql,
        params,
        INVENTORY_LOCATION_ORDER,
        q.page,
        q.page_size,
        InventoryLocationRow::from_row,
    )
    .await
}

/// 库存**两级账**快照：成本账（[`InventoryRow`]）+ 数量账（各库位 [`InventoryLocationRow`]）。
///
/// # 为什么把两张表一起返回
///
/// W0 设计 §2.0 的硬结论：库位维度不塞进 `biz_inventory`（否则「物料 × 仓库 × 库位」
/// 会把成本行打散，移动加权平均失去意义）。代价是**同一次查询必须同时看两张表**，
/// 否则「仓库有 5 个、库位加起来 3 个」这种账实不符看不出来。这个结构把这层不变量
/// 变成可断言的方法（[`Self::location_qty_sum`] / [`Self::is_consistent`]）。
#[derive(Debug, Clone)]
pub struct InventoryBalance {
    /// 成本账（数量合计 + 均价 + 金额）。
    pub inventory: InventoryRow,
    /// 数量账（按库位展开，可能为空——历史数据未分库位）。
    pub locations: Vec<InventoryLocationRow>,
}

impl InventoryBalance {
    /// 各库位数量之和。
    pub fn location_qty_sum(&self) -> Decimal {
        self.locations
            .iter()
            .fold(Decimal::ZERO, |acc, location| acc + location.qty)
    }

    /// 两级账是否一致：`Σ 库位数量 == 成本账数量`。
    ///
    /// 没有库位行时按「不一致」处理（空集合求和是 0，账面却非 0 就是账实不符）；
    /// 账面数量本身为 0 且无库位行时视为一致（新物料空仓）。
    pub fn is_consistent(&self) -> bool {
        if self.locations.is_empty() {
            return self.inventory.qty.is_zero();
        }
        self.location_qty_sum() == self.inventory.qty
    }
}

/// 取「物料 × 仓库」的两级账快照。
///
/// # 返回
///
/// - 成本账不存在 → `Ok(None)`（该物料在这个仓库没有任何库存记录）；
/// - 存在 → 成本账 + 库位明细（库位明细可能为空，用 [`InventoryBalance::is_consistent`] 判定）。
pub async fn inventory_balance(
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<Option<InventoryBalance>> {
    let Some(inventory) = inventory_find(material_id, warehouse_id).await? else {
        return Ok(None);
    };
    let locations = inventory_locations(material_id, warehouse_id).await?;
    Ok(Some(InventoryBalance {
        inventory,
        locations,
    }))
}

// ==================== ⑬ biz_inventory_txn 库存流水 ====================

/// `biz_inventory_txn` 列清单。
pub const INVENTORY_TXN_COLUMNS: &str = "id, material_id, warehouse_id, txn_type, biz_type, \
     doc_type, doc_id, doc_no, line_no, qty, unit_cost, amount, before_qty, before_amount, \
     before_cost, after_qty, after_amount, after_cost, period_code, operator_id, remark, \
     created_at";

/// 库存流水行（**只增不改**：没有 `updated_at`，也没有编辑接口）。
#[derive(Debug, Clone)]
pub struct InventoryTxnRow {
    /// 主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 出入方向：`IN` / `OUT`。
    pub txn_type: String,
    /// 业务类型（如 `PURCHASE` / `SALE` / `ADJUST`）。
    pub biz_type: String,
    /// 单据类型（如 `PURCHASE_ORDER`）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: Uuid,
    /// 单据号（冗余，便于按号检索）。
    pub doc_no: String,
    /// 单据行号。
    pub line_no: i32,
    /// 本次数量（`> 0`，方向看 `txn_type`）。
    pub qty: Decimal,
    /// 本次单位成本（`NUMERIC(18,6)`）。
    pub unit_cost: Decimal,
    /// 本次金额（`>= 0`）。
    pub amount: Decimal,
    /// 变动前数量。
    pub before_qty: Decimal,
    /// 变动前金额。
    pub before_amount: Decimal,
    /// 变动前均价。
    pub before_cost: Decimal,
    /// 变动后数量。
    pub after_qty: Decimal,
    /// 变动后金额。
    pub after_amount: Decimal,
    /// 变动后均价。
    pub after_cost: Decimal,
    /// 会计期间编码。
    pub period_code: String,
    /// 操作人。
    pub operator_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 记账时间。
    pub created_at: DateTime<Utc>,
}

impl InventoryTxnRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            txn_type: row.get_typed("txn_type")?,
            biz_type: row.get_typed("biz_type")?,
            doc_type: row.get_typed("doc_type")?,
            doc_id: row.get_typed("doc_id")?,
            doc_no: row.get_typed("doc_no")?,
            line_no: row.get_typed("line_no")?,
            qty: row.get_typed("qty")?,
            unit_cost: row.get_typed("unit_cost")?,
            amount: row.get_typed("amount")?,
            before_qty: row.get_typed("before_qty")?,
            before_amount: row.get_typed("before_amount")?,
            before_cost: row.get_typed("before_cost")?,
            after_qty: row.get_typed("after_qty")?,
            after_amount: row.get_typed("after_amount")?,
            after_cost: row.get_typed("after_cost")?,
            period_code: row.get_typed("period_code")?,
            operator_id: row.get_typed("operator_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// `biz_inventory_txn` 的分页锚点。
pub const INVENTORY_TXN_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::unscoped("biz_inventory_txn");

/// 流水排序：时间倒序 → 主键倒序（与 `idx_biz_inv_txn_time` 同向，稳定分页）。
pub const INVENTORY_TXN_ORDER: &str = "ORDER BY created_at DESC, id DESC";

/// 库存流水分页（物料 / 仓库 / 单据号 / 单据类型 / 出入方向 / 期间 / 记账号区间）。
pub async fn inventory_txn_page(q: &InventoryTxnFilter) -> DbResult<(Vec<InventoryTxnRow>, i64)> {
    let mut sql = format!("SELECT {INVENTORY_TXN_COLUMNS} FROM biz_inventory_txn WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(doc_no) = q.doc_no.as_deref() {
        sql.push_str(" AND doc_no = ?");
        params.push(Value::from(doc_no));
    }
    if let Some(doc_type) = q.doc_type.as_deref() {
        sql.push_str(" AND doc_type = ?");
        params.push(Value::from(doc_type));
    }
    if let Some(txn_type) = q.txn_type.as_deref() {
        sql.push_str(" AND txn_type = ?");
        params.push(Value::from(txn_type));
    }
    if let Some(period_code) = q.period_code.as_deref() {
        sql.push_str(" AND period_code = ?");
        params.push(Value::from(period_code));
    }
    if let Some(date_from) = q.date_from {
        sql.push_str(" AND created_at >= ?");
        params.push(Value::from(day_start(date_from)));
    }
    if let Some(date_to) = q.date_to {
        // 右开区间：`< 次日零点`，避免 `<= 当日 00:00` 漏掉当天的流水
        sql.push_str(" AND created_at < ?");
        params.push(Value::from(day_start(
            date_to.succ_opt().unwrap_or(date_to),
        )));
    }
    page_of(
        INVENTORY_TXN_TARGET,
        sql,
        params,
        INVENTORY_TXN_ORDER,
        q.page,
        q.page_size,
        InventoryTxnRow::from_row,
    )
    .await
}

/// `NaiveDate` → 当日零点（UTC）。`created_at` 是 `timestamptz`，按 UTC 日界过滤。
pub(super) fn day_start(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0)
        .map(|naive| naive.and_utc())
        .unwrap_or_else(|| DateTime::<Utc>::UNIX_EPOCH)
}

// ==================== 单元测试（真库，`ERP_DB_URL`） ====================
//
// 需要真库：本卡要证的「字段映射 / 仓库子树 / 两账一致 / Decimal 精度」都只有真 PG 能证
// （`NUMERIC(18,4)` 的标度、`text_pattern_ops` 的算子、`NULLS NOT DISTINCT` 的唯一索引）。
//
// 用法（**必须用一次性库，不要指到生产**）：
//
// ```bash
// su - postgres -c 'createdb -O cloudpivot cloudpivot_w1b'
// # 用该库起一次实例把迁移跑完（V0001..V0137），或直接用已迁移好的克隆库
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_w1b'
// cargo test --lib business::erp
// ```
//
// 缺 `ERP_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。
//
// # 夹具
//
// 固定 UUID（`b1b0…` 段）+ `W1B-` 前缀，全部 `INSERT … ON CONFLICT DO NOTHING`，
// 重复跑不积累垃圾，也不依赖清理（一次性库的语义）。

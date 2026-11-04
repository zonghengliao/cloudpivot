// ============================================================================
// model_order.rs —— 采购订单只读：订单头与明细的行结构 + 分页/按号查
//
// 由 `model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::{PurchaseOrderFilter, PurchaseOrderItemFilter};
use crate::business::shared::scope;
use crate::utils;

use super::model_base::*;

/// `biz_purchase_order` 列清单。
///
/// `currency_code` 是 `CHAR(3)`：显式 `::text`，避免定长补空格
/// （`bpchar` 的末尾空格会带进 `String`，比较与展示都会出错）。
pub const PURCHASE_ORDER_COLUMNS: &str = "id, order_no, supplier_id, order_date, expect_date, \
     warehouse_id, status, currency_code::text AS currency_code, total_qty, total_amount, \
     total_tax, total_with_tax, credit_days, remark, price_mode, submitted_at, submitted_by, \
     approved_at, approved_by, closed_at, closed_by, approval_instance_id, created_at, updated_at, \
     create_by, update_by, deleted_at, \
     purchase_type, contract_id, contract_no_snapshot, supplier_name_snapshot, \
     over_credit_limit, credit_exposure, credit_limit_snapshot";

/// 采购订单行。
#[derive(Debug, Clone)]
pub struct PurchaseOrderRow {
    /// 主键。
    pub id: Uuid,
    /// 订单号（未删除内唯一，由 `biz_seq` 的 `PO` 序列生成）。
    pub order_no: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 期望到货日。
    pub expect_date: Option<NaiveDate>,
    /// 收货仓库。
    pub warehouse_id: Uuid,
    /// 状态：`DRAFT` / `SUBMITTED` / `APPROVED` / `PARTIAL_RECEIVED` / `RECEIVED` / `CLOSED` / `CANCELLED`。
    pub status: String,
    /// 币种（`CHAR(3)`，已 `::text`）。
    pub currency_code: String,
    /// 合计数量（`NUMERIC(18,4)`）。
    pub total_qty: Decimal,
    /// 合计金额（不含税，`NUMERIC(18,4)`）。
    pub total_amount: Decimal,
    /// 合计税额（`NUMERIC(18,4)`）。
    pub total_tax: Decimal,
    /// 合计含税金额（DB 约束 `= total_amount + total_tax`）。
    pub total_with_tax: Decimal,
    /// 账期天数。
    pub credit_days: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 金额口径（`EXCL` 不含税录入 / `INCL` 含税录入，V0145 加的列）。
    pub price_mode: String,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 提交人。
    pub submitted_by: Option<Uuid>,
    /// 审批时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 审批人。
    pub approved_by: Option<Uuid>,
    /// 关闭时间。
    pub closed_at: Option<DateTime<Utc>>,
    /// 关闭人。
    pub closed_by: Option<Uuid>,
    /// 提交审批的实例 id（P-FLOW-3A；`None` = 未走审批 / 已驳回打回）。
    ///
    /// 与 `status` 的关系由代码维护（没有库侧 CHECK，因为 `APPROVED` 之后还要留着做审计）：
    /// `SUBMITTED`（审批中）必有值；驳回回 `DRAFT` 时清空；通过后保留（详情显示「审批通过 FLW-xxx」）。
    pub approval_instance_id: Option<Uuid>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 未删除）。
    pub deleted_at: Option<DateTime<Utc>>,
    /// 采购类型（`CONTRACT` 合同采购 / `SPOT` 零星采购，V0198；存量全是 `SPOT`）。
    pub purchase_type: String,
    /// 合同（弱引用 `biz_contract.id`；`SPOT` 必为 `None`）。
    pub contract_id: Option<Uuid>,
    /// 合同号快照（建单时刻；列表/打印只读这一列，不 JOIN 合同表）。
    pub contract_no_snapshot: Option<String>,
    /// 供应商名称快照（业务发生那一刻；**快照不是冗余**，合同换乙方不回写）。
    pub supplier_name_snapshot: Option<String>,
    /// 提交时点是否超供应商信用额度（V0215 / DC-12；`false` **不等于**「查过没问题」——
    /// 要看 [`Self::credit_exposure`] 是否为 `None`，见 `purchase_credit::assess_summary`）。
    pub over_credit_limit: bool,
    /// 提交时点「该供应商累计未结采购额（含本单）」快照（`None` = 从未提交过 / 未评估）。
    pub credit_exposure: Option<Decimal>,
    /// 提交时点供应商额度快照（`0` = 不限）。
    pub credit_limit_snapshot: Option<Decimal>,
}

impl PurchaseOrderRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            order_no: row.get_typed("order_no")?,
            supplier_id: row.get_typed("supplier_id")?,
            order_date: row.get_typed("order_date")?,
            expect_date: row.get_typed("expect_date")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            status: row.get_typed("status")?,
            currency_code: row.get_typed("currency_code")?,
            total_qty: row.get_typed("total_qty")?,
            total_amount: row.get_typed("total_amount")?,
            total_tax: row.get_typed("total_tax")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            credit_days: row.get_typed("credit_days")?,
            remark: row.get_typed("remark")?,
            price_mode: row.get_typed("price_mode")?,
            submitted_at: row.get_typed("submitted_at")?,
            submitted_by: row.get_typed("submitted_by")?,
            approved_at: row.get_typed("approved_at")?,
            approved_by: row.get_typed("approved_by")?,
            closed_at: row.get_typed("closed_at")?,
            closed_by: row.get_typed("closed_by")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
            purchase_type: row.get_typed("purchase_type")?,
            contract_id: row.get_typed("contract_id")?,
            contract_no_snapshot: row.get_typed("contract_no_snapshot")?,
            supplier_name_snapshot: row.get_typed("supplier_name_snapshot")?,
            over_credit_limit: row.get_typed("over_credit_limit")?,
            credit_exposure: row.get_typed("credit_exposure")?,
            credit_limit_snapshot: row.get_typed("credit_limit_snapshot")?,
        })
    }
}

/// `biz_purchase_order` 的分页锚点。
pub const PURCHASE_ORDER_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::unscoped("biz_purchase_order");

/// 采购订单列表排序：下单日期降序 → 单号降序（走 `idx_biz_po_date`）。
pub const PURCHASE_ORDER_ORDER: &str = "ORDER BY order_date DESC, order_no DESC";

/// 按主键取采购订单。
pub async fn purchase_order_find_by_id(id: Uuid) -> DbResult<Option<PurchaseOrderRow>> {
    find_by_id(
        "biz_purchase_order",
        PURCHASE_ORDER_COLUMNS,
        id,
        true,
        PurchaseOrderRow::from_row,
    )
    .await
}

/// 按单号取采购订单（幂等校验 / 按单号跳转）。
pub async fn purchase_order_find_by_no(order_no: &str) -> DbResult<Option<PurchaseOrderRow>> {
    let sql = format!(
        "SELECT {PURCHASE_ORDER_COLUMNS} FROM biz_purchase_order \
         WHERE order_no = ? AND deleted_at IS NULL"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(order_no)])
        .await?;
    rows.first().map(PurchaseOrderRow::from_row).transpose()
}

/// 采购订单分页（关键词 / 状态 / 供应商 / 仓库 / 下单日期区间）。
pub async fn purchase_order_page(
    q: &PurchaseOrderFilter,
) -> DbResult<(Vec<PurchaseOrderRow>, i64)> {
    let mut sql =
        format!("SELECT {PURCHASE_ORDER_COLUMNS} FROM biz_purchase_order WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (order_no LIKE ? OR remark LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(purchase_type) = q.purchase_type.as_deref() {
        sql.push_str(" AND purchase_type = ?");
        params.push(Value::from(purchase_type));
    }
    if let Some(contract_id) = q.contract_id.as_deref() {
        // 走部分索引 `idx_biz_po_contract(contract_id, order_date DESC)`（V0198）
        sql.push_str(" AND contract_id = ?");
        params.push(Value::from(parse_id(contract_id)?));
    }
    if let Some(supplier_id) = q.supplier_id.as_deref() {
        sql.push_str(" AND supplier_id = ?");
        params.push(Value::from(parse_id(supplier_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(date_from) = q.date_from {
        sql.push_str(" AND order_date >= ?");
        params.push(Value::from(date_from));
    }
    if let Some(date_to) = q.date_to {
        sql.push_str(" AND order_date <= ?");
        params.push(Value::from(date_to));
    }
    page_of(
        PURCHASE_ORDER_TARGET,
        sql,
        params,
        PURCHASE_ORDER_ORDER,
        q.page,
        q.page_size,
        PurchaseOrderRow::from_row,
    )
    .await
}

// ==================== ⑩ biz_purchase_order_item 采购明细 ====================

/// `biz_purchase_order_item` 列清单（**不含派生列** `received_qty`）。
///
/// P2-3 起「已收数量」不再落库：迁移 V0217 删掉了那一列，读的时候由
/// [`purchase_order_item_columns`] 追加一段现算表达式（唯一真相 = 已过账收货单）。
/// 列名统一带 `i.` 前缀：调用方一律 `FROM biz_purchase_order_item i`，
/// 派生表达式里写死了 `i.order_id` / `i.id`（见 `POSTED_RECEIVED_SQL`）。
pub const PURCHASE_ORDER_ITEM_COLUMNS: &str = "i.id, i.order_id, i.line_no, i.material_id, i.unit_id, \
     i.qty, i.price, i.price_incl, i.tax_rate, i.amount, i.tax_amount, i.with_tax_amount, \
     i.remark, i.created_at, i.updated_at";

/// 采购明细列清单 + **派生**「已收数量」（`received_qty`）。
///
/// `received_qty` 的口径只有一份：`purchase_model::POSTED_RECEIVED_SQL`
/// （只算 `status='POSTED'` 且未软删的收货单、按采购明细行匹配）。
/// 行结构 [`PurchaseOrderItemRow::received_qty`] 是它的投影，前端字段名不变。
pub fn purchase_order_item_columns() -> String {
    format!(
        "{PURCHASE_ORDER_ITEM_COLUMNS}, ({}::numeric(18,4)) AS received_qty",
        super::purchase_model::POSTED_RECEIVED_SQL
    )
}

/// 采购明细行。
///
/// 明细**没有 `deleted_at`**：唯一外键 `order_id → biz_purchase_order(id) ON DELETE CASCADE`，
/// 主单软删时明细留在库里（随主单一起不可见），主单物理删除时级联删。
#[derive(Debug, Clone)]
pub struct PurchaseOrderItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属订单。
    pub order_id: Uuid,
    /// 行号（订单内唯一）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 计量单位。
    pub unit_id: Uuid,
    /// 数量（`NUMERIC(18,4)`，`> 0`）。
    pub qty: Decimal,
    /// 不含税单价（`NUMERIC(18,6)`，权威值）。
    pub price: Decimal,
    /// 含税单价（`NUMERIC(18,6)`，V0145 加的输入快照：口径 B 时用户键入的就是它）。
    pub price_incl: Decimal,
    /// 税率（`NUMERIC(7,4)`，百分数 0–100）。
    pub tax_rate: Decimal,
    /// 不含税金额（`NUMERIC(18,4)`）。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 含税金额（DB 约束 `= amount + tax_amount`）。
    pub with_tax_amount: Decimal,
    /// 已收货数量（`0 <= received_qty <= qty`）。
    ///
    /// **派生值**（P2-3）：读时由 `POSTED_RECEIVED_SQL` 现算，库里没有这一列。
    /// 口径 = 只算 `status='POSTED'` 且未软删的收货单明细，按采购明细行匹配。
    pub received_qty: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl PurchaseOrderItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            order_id: row.get_typed("order_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            unit_id: row.get_typed("unit_id")?,
            qty: row.get_typed("qty")?,
            price: row.get_typed("price")?,
            price_incl: row.get_typed("price_incl")?,
            tax_rate: row.get_typed("tax_rate")?,
            amount: row.get_typed("amount")?,
            tax_amount: row.get_typed("tax_amount")?,
            with_tax_amount: row.get_typed("with_tax_amount")?,
            received_qty: row.get_typed("received_qty")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 采购明细的**列表排序**：行号升序。
pub const PURCHASE_ORDER_ITEM_ORDER: &str = "ORDER BY i.line_no ASC";

/// 按订单取全部明细（行号升序，走 `idx_biz_po_item_order`）。
///
/// 明细是「一个主单几十行」的小集合，不做分页（与前端一次拉整单的口径一致）。
/// `received_qty` 是派生列（`POSTED_RECEIVED_SQL`）：一次查询拿整单，不做 N+1。
pub async fn purchase_order_items(order_id: Uuid) -> DbResult<Vec<PurchaseOrderItemRow>> {
    let sql = format!(
        "SELECT {} FROM biz_purchase_order_item i \
         WHERE i.order_id = ? ORDER BY i.line_no ASC",
        purchase_order_item_columns()
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(order_id)])
        .await?;
    rows.iter().map(PurchaseOrderItemRow::from_row).collect()
}

/// 按主键取采购明细。
///
/// 不走通用 `find_by_id`：派生列要求表别名 `i`（见 [`PURCHASE_ORDER_ITEM_COLUMNS`]），
/// 而通用模板只拼 `FROM {table}`。明细没有 `deleted_at`（随主单级联），语义不变。
pub async fn purchase_order_item_find_by_id(id: Uuid) -> DbResult<Option<PurchaseOrderItemRow>> {
    let sql = format!(
        "SELECT {} FROM biz_purchase_order_item i WHERE i.id = ?::uuid",
        purchase_order_item_columns()
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(PurchaseOrderItemRow::from_row).transpose()
}

/// 采购明细分页（按订单 / 按物料），供「物料采购记录」类页面使用。
pub async fn purchase_order_item_page(
    q: &PurchaseOrderItemFilter,
) -> DbResult<(Vec<PurchaseOrderItemRow>, i64)> {
    let mut sql = format!(
        "SELECT {} FROM biz_purchase_order_item i WHERE 1 = 1",
        purchase_order_item_columns()
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(order_id) = q.order_id.as_deref() {
        sql.push_str(" AND i.order_id = ?");
        params.push(Value::from(parse_id(order_id)?));
    }
    if let Some(material_id) = q.material_id.as_deref() {
        sql.push_str(" AND i.material_id = ?");
        params.push(Value::from(parse_id(material_id)?));
    }
    page_of(
        scope::ScopeTarget::unscoped("biz_purchase_order_item"),
        sql,
        params,
        PURCHASE_ORDER_ITEM_ORDER,
        q.page,
        q.page_size,
        PurchaseOrderItemRow::from_row,
    )
    .await
}

// ==================== ⑪ biz_inventory 即时库存（成本账） ====================

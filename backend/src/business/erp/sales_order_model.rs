//! 销售订单数据层（销售链 · SO-05）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0264__sales_order.up.sql`
//! （`biz_sales_order` / `biz_sales_order_item`）。**取号 / 单号解析 / 状态域 /
//! 金额守卫在 [`crate::business::erp::sales_model`]**（SO-04 已交付并真库验证过），
//! 本文件只做「行结构 + 读写 + 状态迁移落库」，**不另造一份取号**。
//!
//! # 与 `sales_model` 的分工
//!
//! | 文件 | 负责 |
//! |------|------|
//! | `sales_model.rs`（SO-04） | 表名常量、7 态状态域、`SO-yyyyMMdd-6位` 取号、单号解析、金额恒等 / 数量守卫 |
//! | 本文件（SO-05） | 行结构、建单 / 改单 / 分页 / 详情、状态迁移落库、信用额度占用口径 |
//!
//! # 金额与数量口径（与库里的 CHECK 逐条对应）
//!
//! | 列 | 口径 | 库层兜底 |
//! |----|------|----------|
//! | `amount` | `round(price × qty, 4)`（逐行先舍入再汇总，**禁止先加总再舍入**） | `ck_sales_order_item_amount`（非负） |
//! | `tax_amount` | `round(amount × tax_rate / 100, 4)` | — |
//! | `total_amount` / `total_tax` | 各明细同名列之和 | `ck_sales_order_amount_nonneg` |
//! | `total_with_tax` | `total_amount + total_tax`（**精确恒等**） | `ck_sales_order_amount` |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_order / biz_sales_order_item 是 ERP 单据表（V0264 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由
//! business:erp:sale-order:* 权限码控制。
//! 分页走 model_base::page_of → scope::paginate_unscoped（闸①口径），本文件不含手写 COUNT/LIMIT。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::{find_by_id, page_of};
use crate::business::erp::sales_model as base;
use crate::business::shared::{scope, state_machine};

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）直接复用底座。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};
/// 状态域与标签（唯一真相在 `sales_model`，这里只做转发，避免两处漂移）。
pub use crate::business::erp::sales_model::{
    STATUS_APPROVED, STATUS_CANCELLED, STATUS_CLOSED, STATUS_DRAFT, STATUS_OUTBOUND,
    STATUS_PARTIAL_OUT, STATUS_SUBMITTED, STATUSES, is_valid_status, status_label,
};

// ==================== 常量 ====================

/// 表名（转发 `sales_model`，全仓只有一处字面量）。
pub const ORDER_TABLE: &str = base::ORDER_TABLE;
/// 明细表名。
pub const ITEM_TABLE: &str = base::ITEM_TABLE;

/// 单头列清单（与 V0264 逐列对应）。
pub const ORDER_COLUMNS: &str = "id, so_no, customer_id, order_date, delivery_date, status, \
     total_amount, total_tax, total_with_tax, remark, created_at, updated_at, \
     create_by, update_by, deleted_at";

/// 明细列清单（与 V0264 逐列对应）。
pub const ITEM_COLUMNS: &str = "id, so_id, line_no, material_id, qty, price, amount, \
     tax_rate, tax_amount, delivered_qty";

/// 合法边表：**唯一真相**在 `state_machine::SALES_ORDER_EDGES`（本文件只引用，不复制）。
///
/// 10 条边（PRD §4.13.3）：
/// `DRAFT→SUBMITTED→APPROVED→{PARTIAL_OUT→OUTBOUND, OUTBOUND, CLOSED}`、
/// `{DRAFT, SUBMITTED, APPROVED}→CANCELLED`。
pub const LEGAL_EDGES: [(&str, &str); 10] = state_machine::SALES_ORDER_EDGES;

/// 状态机判定：只有 [`LEGAL_EDGES`] 里的边放行（其余一律 409，见 handler）。
pub fn can_transition(from: &str, to: &str) -> bool {
    LEGAL_EDGES.iter().any(|(f, t)| *f == from && *t == to)
}

/// 从 `from` 出发的合法目标态（非法迁移的 409 文案里要列出来，让用户知道能点哪几个）。
pub fn allowed_targets(from: &str) -> Vec<&'static str> {
    LEGAL_EDGES
        .iter()
        .filter(|(f, _)| *f == from)
        .map(|(_, t)| *t)
        .collect()
}

/// 「在手订单」状态域：占用客户信用额度的三个状态。
///
/// 口径：订单**提交之后**就占用额度（客户可能已经据此排产 / 备货），
/// 出库完成（`OUTBOUND`）转成应收账款、`CLOSED` / `CANCELLED` 不再占用，
/// `DRAFT` 是草稿不占用。与 PRD §4.13 「信用额度 = 未出库订单价税合计」一致。
pub const CREDIT_OCCUPYING_STATUSES: [&str; 3] =
    [STATUS_SUBMITTED, STATUS_APPROVED, STATUS_PARTIAL_OUT];

/// 分页锚点（ERP 单据表，不参与数据范围）。
pub const ORDER_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(ORDER_TABLE);

/// 列表排序：下单日期倒序、同一天按单号倒序（最新单在前）。
pub const ORDER_ORDER: &str = "ORDER BY order_date DESC, so_no DESC";

/// 明细排序：行号升序。
pub const ITEM_ORDER: &str = "ORDER BY line_no ASC";

// ==================== 行结构 ====================

/// `biz_sales_order` 一行。
#[derive(Debug, Clone)]
pub struct SalesOrderRow {
    /// 主键。
    pub id: Uuid,
    /// 单号（`SO-yyyyMMdd-000001`，存活行唯一）。
    pub so_no: String,
    /// 客户（弱引用 `biz_customer.id`，无外键）。
    pub customer_id: Uuid,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 要求交期。
    pub delivery_date: Option<NaiveDate>,
    /// 状态（7 值之一）。
    pub status: String,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计（= 前两项之和）。
    pub total_with_tax: Decimal,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 存活）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl SalesOrderRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            so_no: row.get_typed("so_no")?,
            customer_id: row.get_typed("customer_id")?,
            order_date: row.get_typed("order_date")?,
            delivery_date: row.get_typed("delivery_date")?,
            status: row.get_typed("status")?,
            total_amount: row.get_typed("total_amount")?,
            total_tax: row.get_typed("total_tax")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_sales_order_item` 一行。
#[derive(Debug, Clone)]
pub struct SalesOrderItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属单头。
    pub so_id: Uuid,
    /// 行号（1 起，单内唯一）。
    pub line_no: i32,
    /// 物料（弱引用 `biz_material.id`）。
    pub material_id: Uuid,
    /// 订单数量（> 0）。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 行金额（不含税）。
    pub amount: Decimal,
    /// 税率（百分数 0–100）。
    pub tax_rate: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
    /// 已发货数量（出库过账回写，`0..=qty`）。
    pub delivered_qty: Decimal,
}

impl SalesOrderItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            so_id: row.get_typed("so_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            qty: row.get_typed("qty")?,
            price: row.get_typed("price")?,
            amount: row.get_typed("amount")?,
            tax_rate: row.get_typed("tax_rate")?,
            tax_amount: row.get_typed("tax_amount")?,
            delivered_qty: row.get_typed("delivered_qty")?,
        })
    }

    /// 未发货数量（`qty - delivered_qty`，恒 ≥ 0，DDL 的 CHECK 保证）。
    pub fn open_qty(&self) -> Decimal {
        self.qty - self.delivered_qty
    }
}

/// 一行待落库的明细（金额已由 [`line_amounts`] 算好）。
#[derive(Debug, Clone)]
pub struct NewItem {
    /// 物料。
    pub material_id: Uuid,
    /// 数量（> 0）。
    pub qty: Decimal,
    /// 不含税单价（≥ 0）。
    pub price: Decimal,
    /// 税率百分数（0–100）。
    pub tax_rate: Decimal,
    /// 行不含税金额（= `round(price × qty, 4)`）。
    pub amount: Decimal,
    /// 行税额（= `round(amount × tax_rate / 100, 4)`）。
    pub tax_amount: Decimal,
}

/// 单头待落库字段（新建 / 编辑共用；`status` / 金额由 model 维护，调用方不许传）。
#[derive(Debug, Clone)]
pub struct NewSalesOrder<'a> {
    /// 客户（弱引用；有效性由 handler 在事务外校验）。
    pub customer_id: Uuid,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 要求交期。
    pub delivery_date: Option<NaiveDate>,
    /// 备注。
    pub remark: Option<&'a str>,
}

/// 单据汇总（与 `total_amount` / `total_tax` / `total_with_tax` 三列一一对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// 不含税合计。
    pub amount: Decimal,
    /// 税额合计。
    pub tax: Decimal,
    /// 价税合计（= 前两项之和，精确恒等）。
    pub with_tax: Decimal,
}

impl Totals {
    /// 由明细行汇总：逐行先按 4 位定标再相加（禁止先加总再舍入）。
    pub fn of(items: &[NewItem]) -> Self {
        let amount = items
            .iter()
            .fold(Decimal::ZERO, |acc, item| acc + item.amount.round_dp(4))
            .round_dp(4);
        let tax = items
            .iter()
            .fold(Decimal::ZERO, |acc, item| acc + item.tax_amount.round_dp(4))
            .round_dp(4);
        Self {
            amount,
            tax,
            with_tax: amount + tax,
        }
    }
}

/// 行金额拆分：`(amount, tax_amount)`，各 4 位定标。
///
/// `amount = round(price × qty, 4)`、`tax_amount = round(amount × tax_rate / 100, 4)`；
/// 含税金额不再单独存（库里只有 `amount` / `tax_amount`，含税 = 两者之和）。
///
/// # 舍入方式必须是「四舍五入」而不是 `round_dp` 的默认值
///
/// `rust_decimal` 的 `round_dp` 默认是 **银行家舍入（MidpointNearestEven）**：
/// `6.88545 → 6.8854`（末位取偶）。财务口径是**四舍五入**（MidpointAwayFromZero），
/// 两者在「恰好 5」时差 1 个最小单位 —— 单测实测过（`line_amounts_round_per_line`：
/// 3 × 17.655 × 13% 用默认策略得 6.8854，四舍五入得 6.8855）。
/// 所以这里显式传策略，别用默认值。
pub fn line_amounts(qty: Decimal, price: Decimal, tax_rate: Decimal) -> (Decimal, Decimal) {
    let amount = (price * qty).round_dp_with_strategy(4, RoundingStrategy::MidpointAwayFromZero);
    let tax = (amount * tax_rate / Decimal::from(100))
        .round_dp_with_strategy(4, RoundingStrategy::MidpointAwayFromZero);
    (amount, tax)
}

// ==================== 查询 ====================

/// 按主键取销售订单（软删行不返回）。
pub async fn find_order(id: Uuid) -> DbResult<Option<SalesOrderRow>> {
    find_by_id(
        ORDER_TABLE,
        ORDER_COLUMNS,
        id,
        true,
        SalesOrderRow::from_row,
    )
    .await
}

/// 取一张单的明细（行号升序）。
pub async fn items_of(so_id: Uuid) -> DbResult<Vec<SalesOrderItemRow>> {
    let sql = format!("SELECT {ITEM_COLUMNS} FROM {ITEM_TABLE} WHERE so_id = ? {ITEM_ORDER}");
    let rows = db()
        .find_with_params(&sql, vec![Value::from(so_id)])
        .await?;
    rows.iter().map(SalesOrderItemRow::from_row).collect()
}

/// 分页：`so_no`（模糊）/ `status`（精确）/ `customer_id`（精确）三个可选条件。
pub async fn order_page(
    so_no: Option<&str>,
    status: Option<&str>,
    customer_id: Option<Uuid>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<SalesOrderRow>, i64)> {
    let mut sql = format!("SELECT {ORDER_COLUMNS} FROM {ORDER_TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = crate::utils::like_pattern(so_no) {
        sql.push_str(" AND so_no LIKE ?");
        params.push(Value::from(pattern));
    }
    if let Some(status) = status.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(customer_id) = customer_id {
        sql.push_str(" AND customer_id = ?");
        params.push(Value::from(customer_id));
    }
    page_of(
        ORDER_TARGET,
        sql,
        params,
        ORDER_ORDER,
        page,
        page_size,
        SalesOrderRow::from_row,
    )
    .await
}

/// 批量取「客户 id → （编码, 名称）」，分页列表一次往返补齐（避免 N+1）。
///
/// 软删客户不出现在结果里（调用方按缺失处理 = 两列留 `None`），
/// 这与「客户软删后历史单据仍可读」的口径一致：单号 / 金额照常显示，
/// 只是不再显示已删客户的名字。
pub async fn customer_labels(ids: &[Uuid]) -> DbResult<Vec<(Uuid, String, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // 去重：一页里多张单常常是同一个客户
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let placeholders = vec!["?"; unique.len()].join(", ");
    let sql = format!(
        "SELECT id, code, name FROM biz_customer \
          WHERE id IN ({placeholders}) AND deleted_at IS NULL"
    );
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let code: String = row.get_typed("code")?;
        let name: String = row.get_typed("name")?;
        out.push((id, code, name));
    }
    Ok(out)
}

/// 客户在手订单占用的信用额度（价税合计之和，只算 [`CREDIT_OCCUPYING_STATUSES`]）。
///
/// `exclude` = 编辑 / 提交时把「本单自己」排除掉（否则一张已提交的单会把自己算两遍）。
pub async fn customer_open_credit(
    ex: &impl SqlExecutor,
    customer_id: Uuid,
    exclude: Option<Uuid>,
) -> DbResult<Decimal> {
    let mut sql = format!(
        "SELECT COALESCE(SUM(total_with_tax), 0)::text AS used FROM {ORDER_TABLE} \
          WHERE customer_id = ? AND deleted_at IS NULL AND status IN (?, ?, ?)"
    );
    let mut params = vec![
        Value::from(customer_id),
        Value::from(STATUS_SUBMITTED),
        Value::from(STATUS_APPROVED),
        Value::from(STATUS_PARTIAL_OUT),
    ];
    if let Some(id) = exclude {
        sql.push_str(" AND id <> ?");
        params.push(Value::from(&id));
    }
    let rows = ex.query_sql(sql, params).await?;
    let text: Option<String> = rows.first().and_then(|row| row.get_typed("used").ok());
    Ok(text
        .as_deref()
        .and_then(|value| value.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO))
}

// ==================== 写入 ====================

/// 插明细（调用方保证 `so_id` 存在、`line_no` 从 1 起且互不重复）。
async fn insert_items(ex: &impl SqlExecutor, so_id: Uuid, items: &[NewItem]) -> DbResult<()> {
    for (index, item) in items.iter().enumerate() {
        // `?::text::numeric`：驱动不接受把 `Value::Text` 直接绑到 numeric 列（SO-04 实测坑）
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount) \
                 VALUES (?, ?, ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                    ?::text::numeric, ?::text::numeric)"
            ),
            vec![
                Value::from(&so_id),
                Value::from(i32::try_from(index + 1).unwrap_or(i32::MAX)),
                Value::from(&item.material_id),
                Value::Text(item.qty.to_string()),
                Value::Text(item.price.to_string()),
                Value::Text(item.amount.to_string()),
                Value::Text(item.tax_rate.to_string()),
                Value::Text(item.tax_amount.to_string()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
///
/// 返回 `(新单 id, 单号)`：单号要回给前端（列表里立刻能搜到）。
/// 取号走 [`base::next_so_no`]（同一事务内，与单头一起提交或一起回滚 —— 取号推了
/// `biz_seq.current_no` 却没落单会永久跳号，见 sales_model 的注释）。
pub async fn create_order(
    ex: &impl SqlExecutor,
    new: &NewSalesOrder<'_>,
    items: &[NewItem],
    totals: &Totals,
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    let so_no = base::next_so_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(
        operator
            .id
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    ex.exec_sql(
        format!(
            "INSERT INTO {ORDER_TABLE} \
                (id, so_no, customer_id, order_date, delivery_date, status, \
                 total_amount, total_tax, total_with_tax, remark, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, NULLIF(?::text,'')::date, NULLIF(?::text,'')::date, ?, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, NULLIF(?::text,''), \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(so_no.clone()),
            Value::from(&new.customer_id),
            Value::Text(new.order_date.to_string()),
            Value::Text(
                new.delivery_date
                    .map(|date| date.to_string())
                    .unwrap_or_default(),
            ),
            Value::Text(STATUS_DRAFT.to_string()),
            Value::Text(totals.amount.to_string()),
            Value::Text(totals.tax.to_string()),
            Value::Text(totals.with_tax.to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    insert_items(ex, id, items).await?;
    // 建单留痕（`from_status = NULL → DRAFT`），与建单同一事务
    state_machine::log_created(
        ex,
        state_machine::ENTITY_SALES_ORDER,
        id,
        &so_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok((id, so_no))
}

/// 改单（**调用方保证只有 DRAFT 能改**）：单头覆盖 + 明细「删旧插新」。
///
/// 返回 `affected`（0 = 读状态与写状态之间被别人推走，调用方按 409 处理）。
pub async fn update_order(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewSalesOrder<'_>,
    items: &[NewItem],
    totals: &Totals,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {ORDER_TABLE} \
                    SET customer_id = ?, order_date = NULLIF(?::text,'')::date, \
                        delivery_date = NULLIF(?::text,'')::date, \
                        total_amount = ?::text::numeric, total_tax = ?::text::numeric, \
                        total_with_tax = ?::text::numeric, remark = NULLIF(?::text,''), \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ? AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(&new.customer_id),
                Value::Text(new.order_date.to_string()),
                Value::Text(
                    new.delivery_date
                        .map(|date| date.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(totals.amount.to_string()),
                Value::Text(totals.tax.to_string()),
                Value::Text(totals.with_tax.to_string()),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(STATUS_DRAFT.to_string()),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE so_id = ?"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_items(ex, id, items).await?;
    Ok(affected)
}

/// 状态迁移落库（CAS：`WHERE status = from`，返回 0 = 并发被别人推走）。
///
/// **调用方必须先过 `state_machine::transition`**（边表校验 + reason 校验 + 写
/// `biz_status_log`），本函数只改单据头的 `status` 列。
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "UPDATE {ORDER_TABLE} \
                SET status = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ? AND status = ? AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(to.to_string()),
            Value::from(now),
            operator_text,
            Value::from(&id),
            Value::Text(from.to_string()),
        ],
    )
    .await
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 定点字面量（`17.65` 这类在 f64 下不精确，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn item(qty: &str, price: &str, rate: &str) -> NewItem {
        let qty = dec(qty);
        let price = dec(price);
        let tax_rate = dec(rate);
        let (amount, tax_amount) = line_amounts(qty, price, tax_rate);
        NewItem {
            material_id: Uuid::now_v7(),
            qty,
            price,
            tax_rate,
            amount,
            tax_amount,
        }
    }

    /// 边表与 `sales_model::STATUSES` 同域：所有边的端点都在 7 个状态里。
    #[test]
    fn legal_edges_use_declared_status_domain() {
        for (from, to) in LEGAL_EDGES {
            assert!(base::is_valid_status(from), "边起点 `{from}` 不在状态域内");
            assert!(base::is_valid_status(to), "边终点 `{to}` 不在状态域内");
        }
        // 边表与 state_machine 的登记逐字一致（两处不许漂移）
        assert_eq!(LEGAL_EDGES, state_machine::SALES_ORDER_EDGES);
        // 状态机侧也认这个实体（否则 transition() 直接 500「未知实体」）
        assert_eq!(
            state_machine::Entity::parse(state_machine::ENTITY_SALES_ORDER),
            Some(state_machine::Entity::SaleOrder),
            "SALE_ORDER 必须登记在 state_machine::Entity 里"
        );
    }

    /// 主链正向：DRAFT → SUBMITTED → APPROVED → PARTIAL_OUT → OUTBOUND → … 逐条放行。
    #[test]
    fn main_chain_edges_allowed() {
        let chain = [
            (STATUS_DRAFT, STATUS_SUBMITTED),
            (STATUS_SUBMITTED, STATUS_APPROVED),
            (STATUS_APPROVED, STATUS_PARTIAL_OUT),
            (STATUS_PARTIAL_OUT, STATUS_OUTBOUND),
            (STATUS_APPROVED, STATUS_OUTBOUND),
            (STATUS_APPROVED, STATUS_CLOSED),
            (STATUS_PARTIAL_OUT, STATUS_CLOSED),
            (STATUS_DRAFT, STATUS_CANCELLED),
            (STATUS_SUBMITTED, STATUS_CANCELLED),
            (STATUS_APPROVED, STATUS_CANCELLED),
        ];
        for (from, to) in chain {
            assert!(can_transition(from, to), "`{from} → {to}` 应在合法边表里");
        }
        assert_eq!(LEGAL_EDGES.len(), chain.len(), "边表条数与主链清单不符");
    }

    /// 非法迁移全被拒（**卡面验收点**）：跳步、回退、终态出边、自环、越界值。
    #[test]
    fn illegal_transitions_rejected() {
        let illegal = [
            // 跳步（未提交就审批 / 未审批就出库 / 未出库就关闭）
            (STATUS_DRAFT, STATUS_APPROVED),
            (STATUS_DRAFT, STATUS_PARTIAL_OUT),
            (STATUS_SUBMITTED, STATUS_OUTBOUND),
            (STATUS_DRAFT, STATUS_CLOSED),
            (STATUS_SUBMITTED, STATUS_CLOSED),
            // 回退（本状态机没有任何回退边）
            (STATUS_SUBMITTED, STATUS_DRAFT),
            (STATUS_APPROVED, STATUS_SUBMITTED),
            (STATUS_OUTBOUND, STATUS_PARTIAL_OUT),
            // 终态出边（OUTBOUND / CLOSED / CANCELLED 全是终态）
            (STATUS_OUTBOUND, STATUS_CLOSED),
            (STATUS_OUTBOUND, STATUS_CANCELLED),
            (STATUS_CLOSED, STATUS_CANCELLED),
            (STATUS_CANCELLED, STATUS_DRAFT),
            (STATUS_CANCELLED, STATUS_SUBMITTED),
            // 已出库 / 已关闭不能作废（要退走退货单）
            (STATUS_PARTIAL_OUT, STATUS_CANCELLED),
            // 自环（原地流转不是状态变更）
            (STATUS_DRAFT, STATUS_DRAFT),
            (STATUS_APPROVED, STATUS_APPROVED),
            // 域外值（技术架构旧稿的 SHIPPED 系、小写、空串）
            (STATUS_APPROVED, "SHIPPED"),
            (STATUS_APPROVED, "PARTIAL_SHIPPED"),
            ("DRAFT", "submitted"),
            ("", STATUS_DRAFT),
        ];
        for (from, to) in illegal {
            assert!(
                !can_transition(from, to),
                "`{from} → {to}` 非法迁移必须被拒，但边表放行了"
            );
        }
        // 非法迁移时给出的「可选去向」必须是合法边，且不含被拒的目标
        let targets = allowed_targets(STATUS_DRAFT);
        assert_eq!(targets, vec![STATUS_SUBMITTED, STATUS_CANCELLED]);
        assert!(!targets.contains(&STATUS_APPROVED));
        assert!(allowed_targets(STATUS_CLOSED).is_empty(), "终态不应有出边");
        assert!(allowed_targets(STATUS_OUTBOUND).is_empty());
        assert!(allowed_targets(STATUS_CANCELLED).is_empty());
    }

    /// 行金额：`amount = round(price×qty,4)`、`tax = round(amount×rate/100,4)`，逐行先舍入。
    #[test]
    fn line_amounts_round_per_line() {
        let (amount, tax) = line_amounts(dec("3"), dec("17.655"), dec("13"));
        // 17.655 × 3 = 52.965 → 4 位后 52.9650；税 52.965 × 13% = 6.88545 → 6.8855（四舍五入）
        assert_eq!(amount, dec("52.9650"));
        assert_eq!(tax, dec("6.8855"));
        // 0 税率
        let (amount, tax) = line_amounts(dec("2"), dec("100"), Decimal::ZERO);
        assert_eq!((amount, tax), (dec("200.0000"), Decimal::ZERO));
        // 4 位定点：0.3333 × 3 = 0.9999（不是 1）
        let (amount, _) = line_amounts(dec("3"), dec("0.3333"), Decimal::ZERO);
        assert_eq!(amount, dec("0.9999"));
    }

    /// 汇总：三项恒等（`with_tax = amount + tax`），且与逐行手算一致。
    #[test]
    fn totals_are_consistent_and_exact() {
        let items = vec![
            item("3", "17.655", "13"),
            item("2", "100", "0"),
            item("1", "0.3333", "6"),
        ];
        let totals = Totals::of(&items);
        assert_eq!(
            totals.with_tax,
            totals.amount + totals.tax,
            "价税合计必须精确恒等（ck_sales_order_amount 是精确 CHECK）"
        );
        // 逐行手算：52.9650 + 6.8855；200 + 0；0.3333 + 0.0200（0.3333×6% = 0.019998 → 0.0200）
        assert_eq!(totals.amount, dec("253.2983"));
        assert_eq!(totals.tax, dec("6.9055"));
        assert_eq!(totals.with_tax, dec("260.2038"));
        // model 层守卫也认这三个值（与 handler 提交前的最后一道同口径）
        assert!(base::totals_consistent(
            totals.amount,
            totals.tax,
            totals.with_tax
        ));
        assert!(base::amounts_non_negative(
            totals.amount,
            totals.tax,
            totals.with_tax
        ));
        // 空明细：三项全 0（但 handler 会拒「0 行明细」提交，这里是纯算术口径）
        let empty = Totals::of(&[]);
        assert_eq!(
            empty,
            Totals {
                amount: Decimal::ZERO,
                tax: Decimal::ZERO,
                with_tax: Decimal::ZERO
            }
        );
    }

    /// 未发货数量：`qty - delivered_qty`，边界 `0` / `qty` 都对。
    #[test]
    fn open_qty_boundaries() {
        let mut row = SalesOrderItemRow {
            id: Uuid::now_v7(),
            so_id: Uuid::now_v7(),
            line_no: 1,
            material_id: Uuid::now_v7(),
            qty: dec("10"),
            price: dec("1"),
            amount: dec("10"),
            tax_rate: Decimal::ZERO,
            tax_amount: Decimal::ZERO,
            delivered_qty: Decimal::ZERO,
        };
        assert_eq!(row.open_qty(), dec("10"));
        row.delivered_qty = dec("4");
        assert_eq!(row.open_qty(), dec("6"));
        row.delivered_qty = dec("10");
        assert_eq!(row.open_qty(), Decimal::ZERO, "全部发完时未发量为 0");
    }

    /// 信用额度占用口径：只有 SUBMITTED / APPROVED / PARTIAL_OUT 三个状态算占用。
    #[test]
    fn credit_occupying_statuses_are_declared() {
        assert_eq!(
            CREDIT_OCCUPYING_STATUSES,
            [STATUS_SUBMITTED, STATUS_APPROVED, STATUS_PARTIAL_OUT]
        );
        for status in CREDIT_OCCUPYING_STATUSES {
            assert!(base::is_valid_status(status));
        }
        for not_occupying in [
            STATUS_DRAFT,
            STATUS_OUTBOUND,
            STATUS_CLOSED,
            STATUS_CANCELLED,
        ] {
            assert!(
                !CREDIT_OCCUPYING_STATUSES.contains(&not_occupying),
                "`{not_occupying}` 不应占用信用额度"
            );
        }
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关） ----------

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `SO5T-` 前缀夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 夹具客户编码 —— 清场按它定位（单号是取号器生成的，不能按前缀删单据）。
    const CUSTOMER_CODE: &str = "SO5T-CUST";

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场：状态日志 → 单头（明细 CASCADE）→ 夹具客户。
    ///
    /// ⚠ 单号是**取号器生成的**（`SO-yyyyMMdd-NNNNNN`），不是 `SO5T-` 前缀 ——
    /// 所以单头只能按**夹具客户 id** 定位，按前缀删永远删不到（第一版实测栽在这）。
    async fn cleanup() {
        let statements: [(&str, Vec<Value>); 3] = [
            (
                "DELETE FROM biz_status_log \
                  WHERE entity_type = 'SALE_ORDER' \
                    AND entity_id IN ( \
                          SELECT o.id FROM biz_sales_order o \
                            JOIN biz_customer c ON c.id = o.customer_id \
                           WHERE c.code = ?)",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_sales_order \
                  WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = ?)",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_customer WHERE code = ?",
                vec![Value::from(CUSTOMER_CODE)],
            ),
        ];
        for (sql, params) in statements {
            if let Err(err) = db().execute_with_params(sql, params).await {
                panic!("清理夹具失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 夹具客户（ACTIVE、额度 10000）。存在则复用（`ON CONFLICT DO NOTHING` 后再查）。
    async fn fixture_customer() -> Uuid {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, 'SO-05 夹具客户', 10000, 'ACTIVE') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("插夹具客户应成功");
        let id: Uuid = db()
            .scalar_with_params(
                "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
                vec![Value::from(CUSTOMER_CODE)],
            )
            .await
            .expect("查夹具客户应成功");
        id
    }

    fn new_order(customer_id: Uuid, remark: &str) -> NewSalesOrder<'_> {
        NewSalesOrder {
            customer_id,
            order_date: Utc::now().date_naive(),
            delivery_date: None,
            remark: Some(remark),
        }
    }

    /// 建单：取号 → 单头 DRAFT → 明细行号从 1 起 → 金额三项恒等 → 建单日志一行。
    #[tokio::test]
    async fn create_order_persists_header_items_and_log() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let items = vec![item("3", "17.655", "13"), item("2", "100", "0")];
        let totals = Totals::of(&items);
        let tx = db().begin().await.expect("开事务");
        let (id, so_no) = create_order(
            &tx,
            &new_order(customer, "SO-05 建单"),
            &items,
            &totals,
            state_machine::Operator::of(None),
            Utc::now(),
        )
        .await
        .expect("建单应成功");
        tx.commit().await.expect("提交事务");

        assert!(
            so_no.starts_with("SO-"),
            "单号前缀必须是 SO-（实际 {so_no}）"
        );
        assert!(
            base::is_valid_so_no(&so_no),
            "取出的单号必须能拆解：{so_no}"
        );
        let row = find_order(id).await.expect("查单头").expect("单头必须存在");
        assert_eq!(row.status, STATUS_DRAFT);
        assert_eq!(row.so_no, so_no);
        assert_eq!(row.total_amount, totals.amount);
        assert_eq!(row.total_tax, totals.tax);
        assert_eq!(
            row.total_with_tax,
            row.total_amount + row.total_tax,
            "价税合计必须恒等"
        );
        assert_eq!(row.remark.as_deref(), Some("SO-05 建单"));

        let lines = items_of(id).await.expect("查明细");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].line_no, 1, "行号必须从 1 起");
        assert_eq!(lines[1].line_no, 2);
        assert_eq!(
            lines[0].delivered_qty,
            Decimal::ZERO,
            "delivered_qty 默认 0"
        );
        assert_eq!(lines[0].qty, dec("3"));

        // 建单日志：from NULL → DRAFT
        let logs: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_status_log \
                  WHERE entity_type = 'SALE_ORDER' AND entity_id = ? AND from_status IS NULL \
                    AND to_status = 'DRAFT'",
                vec![Value::from(&id)],
            )
            .await
            .expect("数状态日志");
        assert_eq!(logs, 1, "建单必须留一条 from_status=NULL 的状态日志");
        cleanup().await;
    }

    /// 改单：只改草稿；明细整单替换（旧行不残留）；状态被别人推走后 CAS 返回 0。
    #[tokio::test]
    async fn update_order_replaces_items_and_cas_on_draft() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let first = vec![item("1", "10", "13")];
        let totals = Totals::of(&first);
        let tx = db().begin().await.expect("开事务");
        let (id, _) = create_order(
            &tx,
            &new_order(customer, "改单前"),
            &first,
            &totals,
            state_machine::Operator::of(None),
            Utc::now(),
        )
        .await
        .expect("建单应成功");
        tx.commit().await.expect("提交事务");

        let second = vec![item("2", "20", "13"), item("5", "1", "0")];
        let totals2 = Totals::of(&second);
        let affected = update_order(
            db(),
            id,
            &new_order(customer, "改单后"),
            &second,
            &totals2,
            None,
            Utc::now(),
        )
        .await
        .expect("改单应成功");
        assert_eq!(affected, 1);
        let row = find_order(id).await.expect("查单头").expect("单头必须存在");
        assert_eq!(row.remark.as_deref(), Some("改单后"));
        assert_eq!(row.total_with_tax, totals2.with_tax);
        let lines = items_of(id).await.expect("查明细");
        assert_eq!(lines.len(), 2, "明细必须整单替换（旧行不残留）");
        assert_eq!(lines[0].qty, dec("2"));

        // 把状态推到 SUBMITTED，再改单 → CAS 0 行（handler 会翻 409）
        let tx = db().begin().await.expect("开事务");
        apply_status(&tx, id, STATUS_DRAFT, STATUS_SUBMITTED, None, Utc::now())
            .await
            .expect("推状态应成功");
        tx.commit().await.expect("提交事务");
        let affected = update_order(
            db(),
            id,
            &new_order(customer, "非草稿改单"),
            &second,
            &totals2,
            None,
            Utc::now(),
        )
        .await
        .expect("改单 SQL 应执行成功（只是 0 行）");
        assert_eq!(affected, 0, "非 DRAFT 的单据不许改（CAS 必须挡）");
        cleanup().await;
    }

    /// 分页：单号模糊 + 状态精确 + 客户精确；软删行不出现。
    #[tokio::test]
    async fn page_filters_so_no_status_and_customer() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let items = vec![item("1", "10", "0")];
        let totals = Totals::of(&items);
        let mut numbers = Vec::new();
        for remark in ["甲", "乙"] {
            let tx = db().begin().await.expect("开事务");
            let (_, so_no) = create_order(
                &tx,
                &new_order(customer, remark),
                &items,
                &totals,
                state_machine::Operator::of(None),
                Utc::now(),
            )
            .await
            .expect("建单应成功");
            tx.commit().await.expect("提交事务");
            numbers.push(so_no);
        }

        // ① 单号精确筛（单号是取号器生成的 SO- 前缀，不是夹具前缀）
        let by_no = order_page(Some(&numbers[0]), None, None, 1, 50)
            .await
            .expect("按单号筛应成功");
        assert_eq!(by_no.1, 1, "按单号应恰好命中 1 张");
        assert_eq!(by_no.0[0].so_no, numbers[0]);

        // ② 客户精确筛
        let by_customer = order_page(None, None, Some(customer), 1, 50)
            .await
            .expect("按客户筛应成功");
        assert_eq!(by_customer.1, 2, "夹具客户下应有 2 张单");

        // ②b 批量客户标签（分页列表一次往返补齐编码 / 名称）
        let labels = customer_labels(&[customer, customer])
            .await
            .expect("取客户标签");
        assert_eq!(labels.len(), 1, "重复客户 id 应去重");
        assert_eq!(labels[0].0, customer);
        assert_eq!(labels[0].1, CUSTOMER_CODE);
        assert!(!labels[0].2.is_empty(), "名称不能为空");
        assert!(
            customer_labels(&[]).await.expect("空入参").is_empty(),
            "空入参不发 SQL"
        );

        // ③ 状态精确筛：把第二张推成 SUBMITTED，DRAFT 只应剩 1 张
        let second_id = by_customer
            .0
            .iter()
            .find(|row| row.so_no == numbers[1])
            .expect("第二张单必须在分页里")
            .id;
        let tx = db().begin().await.expect("开事务");
        apply_status(
            &tx,
            second_id,
            STATUS_DRAFT,
            STATUS_SUBMITTED,
            None,
            Utc::now(),
        )
        .await
        .expect("推状态应成功");
        tx.commit().await.expect("提交事务");
        let draft_only = order_page(None, Some(STATUS_DRAFT), Some(customer), 1, 50)
            .await
            .expect("按状态筛应成功");
        assert_eq!(draft_only.1, 1, "DRAFT 只剩 1 张");
        assert_eq!(draft_only.0[0].so_no, numbers[0]);
        let submitted = order_page(None, Some(STATUS_SUBMITTED), Some(customer), 1, 50)
            .await
            .expect("按状态筛应成功");
        assert_eq!(submitted.1, 1);
        assert_eq!(submitted.0[0].so_no, numbers[1]);

        // ④ 软删行不出现在分页里
        db().execute_with_params(
            "UPDATE biz_sales_order SET deleted_at = now() WHERE id = ?",
            vec![Value::from(second_id)],
        )
        .await
        .expect("软删应成功");
        let after = order_page(None, None, Some(customer), 1, 50)
            .await
            .expect("软删后分页应成功");
        assert_eq!(after.1, 1, "软删行不得出现在分页里");
        cleanup().await;
    }

    /// 信用额度占用：只算 SUBMITTED / APPROVED / PARTIAL_OUT，且 `exclude` 能排除本单。
    #[tokio::test]
    async fn customer_open_credit_counts_only_in_flight_orders() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let items = vec![item("1", "100", "13")];
        let totals = Totals::of(&items); // 100 + 13 = 113
        let tx = db().begin().await.expect("开事务");
        let (id, _) = create_order(
            &tx,
            &new_order(customer, "额度"),
            &items,
            &totals,
            state_machine::Operator::of(None),
            Utc::now(),
        )
        .await
        .expect("建单应成功");
        tx.commit().await.expect("提交事务");

        let draft_used = customer_open_credit(db(), customer, None)
            .await
            .expect("算占用");
        assert_eq!(draft_used, Decimal::ZERO, "DRAFT 不占额度");

        let tx = db().begin().await.expect("开事务");
        apply_status(&tx, id, STATUS_DRAFT, STATUS_SUBMITTED, None, Utc::now())
            .await
            .expect("推状态");
        tx.commit().await.expect("提交");
        let used = customer_open_credit(db(), customer, None)
            .await
            .expect("算占用");
        assert_eq!(used, dec("113"), "SUBMITTED 占用 113");
        let excluded = customer_open_credit(db(), customer, Some(id))
            .await
            .expect("算排除本单后的占用");
        assert_eq!(excluded, Decimal::ZERO, "exclude 应把本单排除");

        let tx = db().begin().await.expect("开事务");
        apply_status(
            &tx,
            id,
            STATUS_SUBMITTED,
            STATUS_CANCELLED,
            None,
            Utc::now(),
        )
        .await
        .expect("推状态");
        tx.commit().await.expect("提交");
        let after_cancel = customer_open_credit(db(), customer, None)
            .await
            .expect("算占用");
        assert_eq!(after_cancel, Decimal::ZERO, "CANCELLED 释放额度");
        cleanup().await;
    }

    /// 状态迁移落库：CAS 成功 1 行、旧状态过期 0 行（并发保护）。
    #[tokio::test]
    async fn apply_status_is_compare_and_swap() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let items = vec![item("1", "10", "0")];
        let totals = Totals::of(&items);
        let tx = db().begin().await.expect("开事务");
        let (id, _) = create_order(
            &tx,
            &new_order(customer, "CAS"),
            &items,
            &totals,
            state_machine::Operator::of(None),
            Utc::now(),
        )
        .await
        .expect("建单");
        tx.commit().await.expect("提交");

        let affected = apply_status(db(), id, STATUS_DRAFT, STATUS_SUBMITTED, None, Utc::now())
            .await
            .expect("推状态");
        assert_eq!(affected, 1);
        let again = apply_status(db(), id, STATUS_DRAFT, STATUS_SUBMITTED, None, Utc::now())
            .await
            .expect("重复推状态 SQL 应成功（0 行）");
        assert_eq!(again, 0, "并发 / 重复流转必须 CAS 成 0 行");
        let row = find_order(id).await.expect("查单").expect("必须存在");
        assert_eq!(row.status, STATUS_SUBMITTED);
        cleanup().await;
    }
}

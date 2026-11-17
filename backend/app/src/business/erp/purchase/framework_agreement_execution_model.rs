//! 框架协议执行量统计的数据访问层（M4-09）。
//!
//! **只读**：本文件一句写 SQL 都没有 —— 已用量的真相是单据链（协议物料行 → 采购单明细），
//! 由 V0452 的两个视图现算，没有任何回写列需要维护（回写列只作为快照并排带出对账）。
//!
//! | 读入口 | 走哪 | 用途 |
//! |:--|:--|:--|
//! | [`page_execution`] | `v_framework_agreement_execution` | 跨协议总览（分页 + 筛选） |
//! | [`head_of`] | `v_framework_agreement_execution` | 单协议单头（详情页的汇总） |
//! | [`items_of_agreement`] | `v_framework_agreement_item_execution` | 单协议逐物料行执行量 |
//! | [`sources_of_agreement`] | 采购明细 → 采购单 | 下钻：这些量被哪张单的哪一行用掉 |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 与框架协议表同口径（V0450 头注释 8）—— 协议是公司级采购文件，
//!        可见性由 business:erp:framework-agreement:execution 权限码控制；
//!        总览走 scope::paginate_unscoped（闸①口径），明细按协议主键取。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot_kernel::business::shared::scope;
use cloudpivot_kernel::utils;

use crate::business::erp::purchase::framework_agreement_execution_dto::{ExecutionFilter, HEAD_VIEW, ITEM_VIEW};

/// 总览视图的分页目标（不登记进 `SCOPED_TABLES`：SCOPE-EXEMPT，见模块头）。
pub const HEAD_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(HEAD_VIEW);

/// 单头（总览的一行 / 详情页的汇总）。
#[derive(Debug, Clone)]
pub struct HeadRow {
    /// 协议 id。
    pub agreement_id: Uuid,
    /// 协议号。
    pub agreement_no: String,
    /// 协议名称。
    pub agreement_name: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 供应商名快照。
    pub supplier_name: Option<String>,
    /// 供应商现名。
    pub supplier_display_name: String,
    /// 经办部门。
    pub dept_id: Option<Uuid>,
    /// 经办部门名。
    pub dept_name: Option<String>,
    /// 经办人。
    pub owner_id: Option<Uuid>,
    /// 经办人姓名快照。
    pub owner_name: Option<String>,
    /// 有效期起。
    pub start_date: NaiveDate,
    /// 有效期止。
    pub end_date: NaiveDate,
    /// 签订日期。
    pub sign_date: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 定价方式。
    pub price_mode: String,
    /// 协议价是否含税。
    pub price_includes_tax: bool,
    /// 币种。
    pub price_currency: String,
    /// 容差。
    pub price_tolerance: Decimal,
    /// 单头数量上限（None = 不限）。
    pub total_max_qty: Option<Decimal>,
    /// 单头金额上限（None = 不限）。
    pub total_max_amount: Option<Decimal>,
    /// 有效数量封顶（None = 不限）。
    pub cap_qty: Option<Decimal>,
    /// 有效金额封顶（None = 不限）。
    pub cap_amount: Option<Decimal>,
    /// 明细上限合计（仅明细全有上限时有值）。
    pub item_max_qty_sum: Option<Decimal>,
    /// 物料行数。
    pub item_count: i64,
    /// 不限量的行数。
    pub unlimited_items: i64,
    /// 超上限的行数。
    pub over_items: i64,
    /// 已用满的行数。
    pub done_items: i64,
    /// 部分执行的行数。
    pub partial_items: i64,
    /// 未开工的行数。
    pub not_started_items: i64,
    /// 已用量合计。
    pub executed_qty: Decimal,
    /// 已用金额（不含税）。
    pub executed_amount: Decimal,
    /// 已用金额（含税）。
    pub executed_amount_with_tax: Decimal,
    /// 已用金额（协议价口径）。
    pub executed_amount_capped: Decimal,
    /// 回写快照：已用量合计。
    pub booked_total_qty: Decimal,
    /// 回写快照：已用金额合计。
    pub booked_total_amount: Decimal,
    /// 剩余数量（None = 不限；可为负）。
    pub remaining_qty: Option<Decimal>,
    /// 剩余金额（None = 不限；可为负）。
    pub remaining_amount: Option<Decimal>,
    /// 数量执行率。
    pub exec_qty_rate: Option<Decimal>,
    /// 金额执行率。
    pub exec_amount_rate: Option<Decimal>,
    /// 单头状态。
    pub exec_state: String,
    /// 计入额度的采购单单数。
    pub order_doc_count: i64,
    /// 已释放的采购单单数。
    pub released_doc_count: i64,
    /// 最近下单日。
    pub last_order_on: Option<NaiveDate>,
    /// 距截止日天数（已过期为负）。
    pub days_left: i32,
    /// 今天是否生效。
    pub is_effective: bool,
    /// 有效期状态。
    pub validity_state: String,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 物料行执行量。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 协议行 id。
    pub agreement_item_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 单位。
    pub unit_id: Uuid,
    /// 单位名。
    pub unit_name: String,
    /// 协议单价。
    pub agreed_price: Decimal,
    /// 税率（0–1）。
    pub tax_rate: Decimal,
    /// 数量上限（None = 不限）。
    pub max_qty: Option<Decimal>,
    /// 已用量（单据现算）。
    pub executed_qty: Decimal,
    /// 回写快照。
    pub booked_qty: Decimal,
    /// 已用金额（不含税）。
    pub executed_amount: Decimal,
    /// 已用金额（含税）。
    pub executed_amount_with_tax: Decimal,
    /// 已用金额（协议价口径）。
    pub executed_amount_capped: Decimal,
    /// 行金额上限（None = 不限）。
    pub cap_amount: Option<Decimal>,
    /// 剩余数量（None = 不限）。
    pub remaining_qty: Option<Decimal>,
    /// 剩余金额（None = 不限）。
    pub remaining_amount: Option<Decimal>,
    /// 数量执行率。
    pub exec_qty_rate: Option<Decimal>,
    /// 金额执行率。
    pub exec_amount_rate: Option<Decimal>,
    /// 行状态。
    pub exec_state: String,
    /// 计入的采购单单数。
    pub order_doc_count: i64,
    /// 计入的采购明细行数。
    pub order_line_count: i64,
    /// 最近下单日。
    pub last_order_on: Option<NaiveDate>,
}

/// 下钻来源行（一条 = 采购单的一行明细）。
#[derive(Debug, Clone)]
pub struct SourceRow {
    /// 协议行 id。
    pub agreement_item_id: Uuid,
    /// 采购单。
    pub order_id: Uuid,
    /// 采购单号。
    pub order_no: String,
    /// 采购单状态。
    pub order_status: String,
    /// 下单日。
    pub order_date: NaiveDate,
    /// 采购单软删时间。
    pub order_deleted_at: Option<DateTime<Utc>>,
    /// 采购明细行号。
    pub line_no: i32,
    /// 数量。
    pub qty: Decimal,
    /// 不含税金额。
    pub amount: Decimal,
    /// 含税金额。
    pub with_tax_amount: Decimal,
    /// 下单价（不含税）。
    pub price: Decimal,
}

impl SourceRow {
    /// 来源判定：`(是否计入已用, 原因)` —— 纯函数在 dto 里，这里只喂参数。
    pub fn verdict(&self) -> (bool, &'static str) {
        crate::business::erp::purchase::framework_agreement_execution_dto::source_verdict(
            &self.order_status,
            self.order_deleted_at.is_some(),
        )
    }
}

// ==================== SQL 片段 ====================

/// 单头视图的列清单（顺序与 [`read_head`] 一一对应；改一处必须两处一起改）。
const HEAD_COLUMNS: &str = "agreement_id, agreement_no, agreement_name, supplier_id, supplier_name, \
                            supplier_display_name, dept_id, dept_name, owner_id, owner_name, \
                            start_date, end_date, sign_date, status, price_mode, price_includes_tax, \
                            price_currency, price_tolerance, total_max_qty, total_max_amount, \
                            cap_qty, cap_amount, item_max_qty_sum, item_count, unlimited_items, \
                            over_items, done_items, partial_items, not_started_items, \
                            executed_qty, executed_amount, executed_amount_with_tax, executed_amount_capped, \
                            booked_total_qty, booked_total_amount, remaining_qty, remaining_amount, \
                            exec_qty_rate, exec_amount_rate, exec_state, order_doc_count, released_doc_count, \
                            last_order_on, days_left, is_effective, validity_state, created_at";

/// 物料行视图的列清单（同上）。
const ITEM_COLUMNS: &str = "agreement_item_id, line_no, material_id, material_code, material_name, \
                            unit_id, unit_name, agreed_price, tax_rate, max_qty, executed_qty, booked_qty, \
                            executed_amount, executed_amount_with_tax, executed_amount_capped, cap_amount, \
                            remaining_qty, remaining_amount, exec_qty_rate, exec_amount_rate, exec_state, \
                            order_doc_count, order_line_count, last_order_on";

// ==================== 读：总览分页 ====================

/// 跨协议执行量总览（分页 + 筛选）。
///
/// 不吃执行器：`scope::paginate_unscoped` 走全局 `db()`（与采购计划执行率分页同款）。
pub async fn page_execution(filter: &ExecutionFilter) -> DbResult<(Vec<HeadRow>, u64)> {
    let mut sql = format!("SELECT {HEAD_COLUMNS} FROM {HEAD_VIEW} e WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND e.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(state) = filter.validity_state.as_deref() {
        sql.push_str(" AND e.validity_state = ?");
        params.push(Value::Text(state.to_string()));
    }
    if let Some(supplier_id) = filter.supplier_id {
        sql.push_str(" AND e.supplier_id = ?::uuid");
        params.push(Value::from(&supplier_id));
    }
    if filter.over_only {
        sql.push_str(" AND e.over_items > 0");
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        let like = Value::Text(pattern);
        sql.push_str(
            " AND (e.agreement_no LIKE ? OR e.agreement_name LIKE ? \
                OR COALESCE(e.supplier_display_name, '') LIKE ? \
                OR COALESCE(e.owner_name, '') LIKE ?)",
        );
        for _ in 0..4 {
            params.push(like.clone());
        }
    }
    let page = scope::paginate_unscoped(
        HEAD_TARGET,
        &sql,
        params,
        // 生效中的排前面；同一档里快到期的排前面（控盘先看「要过期 + 快用完」）
        Some("ORDER BY e.is_effective DESC, e.end_date ASC, e.agreement_no DESC"),
        filter.page,
        filter.page_size,
    )
    .await?;
    let rows = page.list.iter().map(read_head).collect::<DbResult<Vec<_>>>()?;
    Ok((rows, page.total))
}

// ==================== 读：单协议 ====================

/// 单协议的执行量单头（协议不存在 / 已软删 → `None`，调用方按 404 处理）。
pub async fn head_of(ex: &impl SqlExecutor, agreement_id: Uuid) -> DbResult<Option<HeadRow>> {
    // SCOPE-EXEMPT: 按主键取 1 行聚合视图（见模块头）。
    let rows = ex
        .query_sql(
            format!("SELECT {HEAD_COLUMNS} FROM {HEAD_VIEW} WHERE agreement_id = ?::uuid"),
            vec![Value::from(&agreement_id)],
        )
        .await?;
    rows.first().map(read_head).transpose()
}

/// 单协议的逐物料行执行量（按行号升序）。
pub async fn items_of_agreement(ex: &impl SqlExecutor, agreement_id: Uuid) -> DbResult<Vec<ItemRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_VIEW} \
                  WHERE agreement_id = ?::uuid ORDER BY line_no"
            ),
            vec![Value::from(&agreement_id)],
        )
        .await?;
    rows.iter().map(read_item).collect()
}

/// 单协议的下钻来源（采购明细 → 采购单，按行号 / 下单日 / 单号排序）。
///
/// 采购单**不过滤** `deleted_at`：已软删的采购单也要出现在下钻里（`counted = false` +
/// 原因「采购单已删除」）—— 直接从列表里消失的话，用户会以为「这批量凭空少了」。
pub async fn sources_of_agreement(ex: &impl SqlExecutor, agreement_id: Uuid) -> DbResult<Vec<SourceRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头）。
    let rows = ex
        .query_sql(
            "SELECT i.framework_agreement_item_id AS agreement_item_id, \
                    o.id AS order_id, o.order_no, o.status AS order_status, o.order_date, \
                    o.deleted_at AS order_deleted_at, i.line_no, i.qty, i.amount, \
                    i.with_tax_amount, i.price \
               FROM biz_purchase_order_item i \
               JOIN biz_purchase_order o ON o.id = i.order_id \
               JOIN biz_framework_agreement_item it ON it.id = i.framework_agreement_item_id \
              WHERE it.agreement_id = ?::uuid \
              ORDER BY it.line_no, o.order_date, o.order_no, i.line_no"
                .to_string(),
            vec![Value::from(&agreement_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(SourceRow {
                agreement_item_id: row.get_typed("agreement_item_id")?,
                order_id: row.get_typed("order_id")?,
                order_no: row.get_typed("order_no")?,
                order_status: row.get_typed("order_status")?,
                order_date: row.get_typed("order_date")?,
                order_deleted_at: row.get_typed("order_deleted_at")?,
                line_no: row.get_typed("line_no")?,
                qty: row.get_typed("qty")?,
                amount: row.get_typed("amount")?,
                with_tax_amount: row.get_typed("with_tax_amount")?,
                price: row.get_typed("price")?,
            })
        })
        .collect()
}

// ==================== 行 → 结构体 ====================

fn read_head(row: &Row) -> DbResult<HeadRow> {
    Ok(HeadRow {
        agreement_id: row.get_typed("agreement_id")?,
        agreement_no: row.get_typed("agreement_no")?,
        agreement_name: row.get_typed("agreement_name")?,
        supplier_id: row.get_typed("supplier_id")?,
        supplier_name: row.get_typed("supplier_name")?,
        supplier_display_name: row.get_typed("supplier_display_name")?,
        dept_id: row.get_typed("dept_id")?,
        dept_name: row.get_typed("dept_name")?,
        owner_id: row.get_typed("owner_id")?,
        owner_name: row.get_typed("owner_name")?,
        start_date: row.get_typed("start_date")?,
        end_date: row.get_typed("end_date")?,
        sign_date: row.get_typed("sign_date")?,
        status: row.get_typed("status")?,
        price_mode: row.get_typed("price_mode")?,
        price_includes_tax: row.get_typed("price_includes_tax")?,
        price_currency: row.get_typed("price_currency")?,
        price_tolerance: row.get_typed("price_tolerance")?,
        total_max_qty: row.get_typed("total_max_qty")?,
        total_max_amount: row.get_typed("total_max_amount")?,
        cap_qty: row.get_typed("cap_qty")?,
        cap_amount: row.get_typed("cap_amount")?,
        item_max_qty_sum: row.get_typed("item_max_qty_sum")?,
        item_count: row.get_typed("item_count")?,
        unlimited_items: row.get_typed("unlimited_items")?,
        over_items: row.get_typed("over_items")?,
        done_items: row.get_typed("done_items")?,
        partial_items: row.get_typed("partial_items")?,
        not_started_items: row.get_typed("not_started_items")?,
        executed_qty: row.get_typed("executed_qty")?,
        executed_amount: row.get_typed("executed_amount")?,
        executed_amount_with_tax: row.get_typed("executed_amount_with_tax")?,
        executed_amount_capped: row.get_typed("executed_amount_capped")?,
        booked_total_qty: row.get_typed("booked_total_qty")?,
        booked_total_amount: row.get_typed("booked_total_amount")?,
        remaining_qty: row.get_typed("remaining_qty")?,
        remaining_amount: row.get_typed("remaining_amount")?,
        exec_qty_rate: row.get_typed("exec_qty_rate")?,
        exec_amount_rate: row.get_typed("exec_amount_rate")?,
        exec_state: row.get_typed("exec_state")?,
        order_doc_count: row.get_typed("order_doc_count")?,
        released_doc_count: row.get_typed("released_doc_count")?,
        last_order_on: row.get_typed("last_order_on")?,
        days_left: row.get_typed("days_left")?,
        is_effective: row.get_typed("is_effective")?,
        validity_state: row.get_typed("validity_state")?,
        created_at: row.get_typed("created_at")?,
    })
}

fn read_item(row: &Row) -> DbResult<ItemRow> {
    Ok(ItemRow {
        agreement_item_id: row.get_typed("agreement_item_id")?,
        line_no: row.get_typed("line_no")?,
        material_id: row.get_typed("material_id")?,
        material_code: row.get_typed("material_code")?,
        material_name: row.get_typed("material_name")?,
        unit_id: row.get_typed("unit_id")?,
        unit_name: row.get_typed("unit_name")?,
        agreed_price: row.get_typed("agreed_price")?,
        tax_rate: row.get_typed("tax_rate")?,
        max_qty: row.get_typed("max_qty")?,
        executed_qty: row.get_typed("executed_qty")?,
        booked_qty: row.get_typed("booked_qty")?,
        executed_amount: row.get_typed("executed_amount")?,
        executed_amount_with_tax: row.get_typed("executed_amount_with_tax")?,
        executed_amount_capped: row.get_typed("executed_amount_capped")?,
        cap_amount: row.get_typed("cap_amount")?,
        remaining_qty: row.get_typed("remaining_qty")?,
        remaining_amount: row.get_typed("remaining_amount")?,
        exec_qty_rate: row.get_typed("exec_qty_rate")?,
        exec_amount_rate: row.get_typed("exec_amount_rate")?,
        exec_state: row.get_typed("exec_state")?,
        order_doc_count: row.get_typed("order_doc_count")?,
        order_line_count: row.get_typed("order_line_count")?,
        last_order_on: row.get_typed("last_order_on")?,
    })
}

// ==================== 单测（纯逻辑：列清单与结构体字段名对不上时先在这里炸） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 列清单必须一个不少地包含 `read_head` 读的每一列（改名时这里先红，而不是运行期 500）。
    #[test]
    fn head_columns_cover_reader() {
        for column in [
            "agreement_id",
            "agreement_no",
            "agreement_name",
            "supplier_id",
            "supplier_name",
            "supplier_display_name",
            "dept_id",
            "dept_name",
            "owner_id",
            "owner_name",
            "start_date",
            "end_date",
            "sign_date",
            "status",
            "price_mode",
            "price_includes_tax",
            "price_currency",
            "price_tolerance",
            "total_max_qty",
            "total_max_amount",
            "cap_qty",
            "cap_amount",
            "item_max_qty_sum",
            "item_count",
            "unlimited_items",
            "over_items",
            "done_items",
            "partial_items",
            "not_started_items",
            "executed_qty",
            "executed_amount",
            "executed_amount_with_tax",
            "executed_amount_capped",
            "booked_total_qty",
            "booked_total_amount",
            "remaining_qty",
            "remaining_amount",
            "exec_qty_rate",
            "exec_amount_rate",
            "exec_state",
            "order_doc_count",
            "released_doc_count",
            "last_order_on",
            "days_left",
            "is_effective",
            "validity_state",
            "created_at",
        ] {
            assert!(HEAD_COLUMNS.contains(column), "HEAD_COLUMNS 缺 {column}");
        }
    }

    /// 物料行列清单同上。
    #[test]
    fn item_columns_cover_reader() {
        for column in [
            "agreement_item_id",
            "line_no",
            "material_id",
            "material_code",
            "material_name",
            "unit_id",
            "unit_name",
            "agreed_price",
            "tax_rate",
            "max_qty",
            "executed_qty",
            "booked_qty",
            "executed_amount",
            "executed_amount_with_tax",
            "executed_amount_capped",
            "cap_amount",
            "remaining_qty",
            "remaining_amount",
            "exec_qty_rate",
            "exec_amount_rate",
            "exec_state",
            "order_doc_count",
            "order_line_count",
            "last_order_on",
        ] {
            assert!(ITEM_COLUMNS.contains(column), "ITEM_COLUMNS 缺 {column}");
        }
    }

    /// 总览分页目标是 SCOPE-EXEMPT（写错成 scoped = 每页都会被数据权限再裁一刀）。
    #[test]
    fn head_target_is_unscoped() {
        assert!(
            !HEAD_TARGET.is_scoped(),
            "框架协议执行量总览是 SCOPE-EXEMPT（V0450 头注释 8）"
        );
        assert_eq!(HEAD_TARGET.table, HEAD_VIEW);
    }
}

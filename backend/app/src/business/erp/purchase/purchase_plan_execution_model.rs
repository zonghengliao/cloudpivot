//! 采购计划执行率的数据访问层（M4-04）。
//!
//! **只读**：本文件一句写 SQL 都没有 —— 执行率的真相是单据链（计划 → 申请 → 采购单），
//! 由 V0449 的两个视图现算，没有任何回写列需要维护。
//!
//! | 读入口 | 走哪 | 用途 |
//! |:--|:--|:--|
//! | [`page_execution`] | `v_purchase_plan_execution` | 跨计划总览（分页 + 筛选） |
//! | [`head_of`] | `v_purchase_plan_execution` | 单计划单头（详情页的汇总） |
//! | [`items_of_plan`] | `v_purchase_plan_item_execution` | 单计划逐行执行率 |
//! | [`sources_of_plan`] | 申请明细 → 申请单 → 采购单 | 下钻：这一行的量被哪些单消耗 |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 与采购计划单同口径（V0446 头注释 8）—— 计划是公司级滚动文件，
//!        可见性由 business:erp:purchase-plan:execution 权限码控制；
//!        总览走 scope::paginate_unscoped（闸①口径），明细按计划主键取。
//! ```

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot_kernel::business::shared::scope;
use cloudpivot_kernel::utils;

use crate::business::erp::purchase::purchase_plan_execution_dto::{
    ExecutionFilter, HEAD_VIEW, ITEM_VIEW, source_verdict,
};

/// 总览视图的分页目标（不登记进 `SCOPED_TABLES`：SCOPE-EXEMPT，见模块头）。
pub const HEAD_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(HEAD_VIEW);

/// 单头（总览的一行 / 详情页的汇总）。
#[derive(Debug, Clone)]
pub struct HeadRow {
    /// 计划 id。
    pub plan_id: Uuid,
    /// 计划单号。
    pub plan_no: String,
    /// 计划名称。
    pub plan_name: String,
    /// 类型。
    pub plan_type: String,
    /// 年度。
    pub plan_year: i32,
    /// 月份。
    pub plan_month: Option<i32>,
    /// 编制部门。
    pub dept_id: Option<Uuid>,
    /// 编制部门名。
    pub dept_name: Option<String>,
    /// 编制人姓名。
    pub planner_name: Option<String>,
    /// 状态。
    pub status: String,
    /// 计划金额合计。
    pub total_planned_amount: Decimal,
    /// 明细行数。
    pub line_count: i64,
    /// 计划量合计。
    pub planned_qty: Decimal,
    /// 已申请量（回写列快照）。
    pub booked_qty: Decimal,
    /// 已申请量（单据现算）。
    pub req_qty: Decimal,
    /// 已下单量。
    pub ordered_qty: Decimal,
    /// 剩余可下单（可为负）。
    pub remaining_qty: Decimal,
    /// 整单执行率。
    pub exec_rate: Option<Decimal>,
    /// 整单已申请率。
    pub req_rate: Option<Decimal>,
    /// 未开工行数。
    pub not_started_lines: i64,
    /// 部分下单行数。
    pub partial_lines: i64,
    /// 已下满行数。
    pub done_lines: i64,
    /// 超计划行数。
    pub over_lines: i64,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 明细行（一格 = 物料 × 仓库）。
#[derive(Debug, Clone)]
pub struct ItemRow {
    /// 计划明细 id。
    pub plan_item_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 仓库名。
    pub warehouse_name: String,
    /// 单位。
    pub unit_id: Uuid,
    /// 单位名。
    pub unit_name: String,
    /// 计划量。
    pub planned_qty: Decimal,
    /// 计划单价。
    pub planned_price: Decimal,
    /// 计划金额。
    pub planned_amount: Decimal,
    /// 已申请量（回写列快照）。
    pub booked_qty: Decimal,
    /// 已申请量（单据现算）。
    pub req_qty: Decimal,
    /// 已下单量。
    pub ordered_qty: Decimal,
    /// 剩余（可为负）。
    pub remaining_qty: Decimal,
    /// 执行率。
    pub exec_rate: Option<Decimal>,
    /// 已申请率。
    pub req_rate: Option<Decimal>,
    /// 行状态（视图的 CASE）。
    pub exec_state: String,
    /// 申请明细行数。
    pub req_count: i64,
    /// 涉及申请单张数。
    pub req_doc_count: i64,
    /// 涉及采购单张数。
    pub po_doc_count: i64,
}

/// 来源单据行（申请明细 → 申请单 → 采购单）。
#[derive(Debug, Clone)]
pub struct SourceRow {
    /// 计划明细 id（分组用）。
    pub plan_item_id: Uuid,
    /// 申请明细 id。
    pub req_item_id: Uuid,
    /// 申请单 id。
    pub req_id: Uuid,
    /// 申请单号。
    pub req_no: String,
    /// 申请单状态。
    pub req_status: String,
    /// 这一行拨给该申请单的数量。
    pub qty: Decimal,
    /// 采购单 id（未转单为 None）。
    pub po_id: Option<Uuid>,
    /// 采购单号。
    pub po_no: Option<String>,
    /// 采购单状态。
    pub po_status: Option<String>,
    /// 采购单软删时间（有值 = 已删，不计入已下单）。
    pub po_deleted_at: Option<DateTime<Utc>>,
}

impl SourceRow {
    /// 来源判定：`(是否计入已下单, 原因)` —— 纯函数在 dto 里，这里只喂参数。
    pub fn verdict(&self) -> (bool, &'static str) {
        source_verdict(self.po_id, self.po_status.as_deref(), self.po_deleted_at.is_some())
    }
}

// ==================== SQL 片段 ====================

/// 单头视图的列清单（顺序与 [`read_head`] 一一对应；改一处必须两处一起改）。
const HEAD_COLUMNS: &str = "plan_id, plan_no, plan_name, plan_type, plan_year, plan_month, \
                            dept_id, dept_name, planner_name, status, total_planned_amount, \
                            line_count, planned_qty, booked_qty, req_qty, ordered_qty, \
                            remaining_qty, exec_rate, req_rate, \
                            not_started_lines, partial_lines, done_lines, over_lines, created_at";

/// 明细视图的列清单（同上）。
const ITEM_COLUMNS: &str = "plan_item_id, line_no, material_id, material_code, material_name, \
                            warehouse_id, warehouse_name, unit_id, unit_name, \
                            planned_qty, planned_price, planned_amount, booked_qty, \
                            req_qty, ordered_qty, remaining_qty, exec_rate, req_rate, exec_state, \
                            req_count, req_doc_count, po_doc_count";

// ==================== 读：总览分页 ====================

/// 跨计划执行率总览（分页 + 筛选）。
///
/// 不吃执行器：`scope::paginate_unscoped` 走全局 `db()`（与采购计划分页同款）。
pub async fn page_execution(filter: &ExecutionFilter) -> DbResult<(Vec<HeadRow>, u64)> {
    let mut sql = format!("SELECT {HEAD_COLUMNS} FROM {HEAD_VIEW} e WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND e.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(plan_type) = filter.plan_type.as_deref() {
        sql.push_str(" AND e.plan_type = ?");
        params.push(Value::Text(plan_type.to_string()));
    }
    if let Some(year) = filter.plan_year {
        sql.push_str(" AND e.plan_year = ?");
        params.push(Value::I32(year));
    }
    if let Some(month) = filter.plan_month {
        sql.push_str(" AND e.plan_month = ?");
        params.push(Value::I32(month));
    }
    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND e.dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(min_rate) = filter.min_rate {
        // 视图里的 exec_rate 是 numeric；没明细的计划是 NULL —— `>= ` 对 NULL 为 NULL ⇒ 自然被筛掉，
        // 这正是「min_rate 只看有执行率的」语义（空计划不该出现在「已下满」里）。
        sql.push_str(" AND e.exec_rate >= ?::text::numeric");
        params.push(Value::Text(min_rate.to_string()));
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        let like = Value::Text(pattern);
        sql.push_str(
            " AND (e.plan_no LIKE ? OR e.plan_name LIKE ? OR COALESCE(e.dept_name, '') LIKE ? \
                OR COALESCE(e.planner_name, '') LIKE ?)",
        );
        for _ in 0..4 {
            params.push(like.clone());
        }
    }
    let page = scope::paginate_unscoped(
        HEAD_TARGET,
        &sql,
        params,
        Some("ORDER BY e.plan_year DESC, e.plan_month DESC NULLS LAST, e.plan_no DESC"),
        filter.page,
        filter.page_size,
    )
    .await?;
    let rows = page.list.iter().map(read_head).collect::<DbResult<Vec<_>>>()?;
    Ok((rows, page.total))
}

// ==================== 读：单计划 ====================

/// 单计划的执行率单头（计划不存在 / 已软删 → `None`，调用方按 404 处理）。
pub async fn head_of(ex: &impl SqlExecutor, plan_id: Uuid) -> DbResult<Option<HeadRow>> {
    // SCOPE-EXEMPT: 按主键取 1 行聚合视图（见模块头）。
    let rows = ex
        .query_sql(
            format!("SELECT {HEAD_COLUMNS} FROM {HEAD_VIEW} WHERE plan_id = ?::uuid"),
            vec![Value::from(&plan_id)],
        )
        .await?;
    rows.first().map(read_head).transpose()
}

/// 单计划的逐行执行率（按行号升序）。
pub async fn items_of_plan(ex: &impl SqlExecutor, plan_id: Uuid) -> DbResult<Vec<ItemRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_VIEW} \
                  WHERE plan_id = ?::uuid ORDER BY line_no"
            ),
            vec![Value::from(&plan_id)],
        )
        .await?;
    rows.iter().map(read_item).collect()
}

/// 单计划的下钻来源（申请明细 → 申请单 → 采购单，按计划行号 + 申请单号排序）。
///
/// 采购单**不过滤** `deleted_at`：已软删的采购单也要出现在下钻里（`counted = false` +
/// 原因「采购单已删除」）—— 直接从视图里消失的话，用户会以为「这批量凭空少了」。
pub async fn sources_of_plan(ex: &impl SqlExecutor, plan_id: Uuid) -> DbResult<Vec<SourceRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头）。
    let rows = ex
        .query_sql(
            "SELECT ri.plan_item_id, ri.id AS req_item_id, rq.id AS req_id, rq.req_no, \
                    rq.status AS req_status, ri.qty, \
                    po.id AS po_id, po.order_no AS po_no, po.status AS po_status, po.deleted_at AS po_deleted_at \
               FROM biz_purchase_requisition_item ri \
               JOIN biz_purchase_requisition rq \
                 ON rq.id = ri.requisition_id AND rq.deleted_at IS NULL \
               JOIN biz_purchase_plan_item it \
                 ON it.id = ri.plan_item_id AND it.plan_id = ?::uuid \
               LEFT JOIN biz_purchase_order po ON po.id = rq.po_id \
              ORDER BY it.line_no, rq.req_no, ri.line_no"
                .to_string(),
            vec![Value::from(&plan_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(SourceRow {
                plan_item_id: row.get_typed("plan_item_id")?,
                req_item_id: row.get_typed("req_item_id")?,
                req_id: row.get_typed("req_id")?,
                req_no: row.get_typed("req_no")?,
                req_status: row.get_typed("req_status")?,
                qty: row.get_typed("qty")?,
                po_id: row.get_typed("po_id")?,
                po_no: row.get_typed("po_no")?,
                po_status: row.get_typed("po_status")?,
                po_deleted_at: row.get_typed("po_deleted_at")?,
            })
        })
        .collect()
}

// ==================== 行 → 结构体 ====================

fn read_head(row: &Row) -> DbResult<HeadRow> {
    Ok(HeadRow {
        plan_id: row.get_typed("plan_id")?,
        plan_no: row.get_typed("plan_no")?,
        plan_name: row.get_typed("plan_name")?,
        plan_type: row.get_typed("plan_type")?,
        plan_year: row.get_typed("plan_year")?,
        plan_month: row.get_typed("plan_month")?,
        dept_id: row.get_typed("dept_id")?,
        dept_name: row.get_typed("dept_name")?,
        planner_name: row.get_typed("planner_name")?,
        status: row.get_typed("status")?,
        total_planned_amount: row.get_typed("total_planned_amount")?,
        line_count: row.get_typed("line_count")?,
        planned_qty: row.get_typed("planned_qty")?,
        booked_qty: row.get_typed("booked_qty")?,
        req_qty: row.get_typed("req_qty")?,
        ordered_qty: row.get_typed("ordered_qty")?,
        remaining_qty: row.get_typed("remaining_qty")?,
        exec_rate: row.get_typed("exec_rate")?,
        req_rate: row.get_typed("req_rate")?,
        not_started_lines: row.get_typed("not_started_lines")?,
        partial_lines: row.get_typed("partial_lines")?,
        done_lines: row.get_typed("done_lines")?,
        over_lines: row.get_typed("over_lines")?,
        created_at: row.get_typed("created_at")?,
    })
}

fn read_item(row: &Row) -> DbResult<ItemRow> {
    Ok(ItemRow {
        plan_item_id: row.get_typed("plan_item_id")?,
        line_no: row.get_typed("line_no")?,
        material_id: row.get_typed("material_id")?,
        material_code: row.get_typed("material_code")?,
        material_name: row.get_typed("material_name")?,
        warehouse_id: row.get_typed("warehouse_id")?,
        warehouse_name: row.get_typed("warehouse_name")?,
        unit_id: row.get_typed("unit_id")?,
        unit_name: row.get_typed("unit_name")?,
        planned_qty: row.get_typed("planned_qty")?,
        planned_price: row.get_typed("planned_price")?,
        planned_amount: row.get_typed("planned_amount")?,
        booked_qty: row.get_typed("booked_qty")?,
        req_qty: row.get_typed("req_qty")?,
        ordered_qty: row.get_typed("ordered_qty")?,
        remaining_qty: row.get_typed("remaining_qty")?,
        exec_rate: row.get_typed("exec_rate")?,
        req_rate: row.get_typed("req_rate")?,
        exec_state: row.get_typed("exec_state")?,
        req_count: row.get_typed("req_count")?,
        req_doc_count: row.get_typed("req_doc_count")?,
        po_doc_count: row.get_typed("po_doc_count")?,
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
            "plan_id",
            "plan_no",
            "plan_name",
            "plan_type",
            "plan_year",
            "plan_month",
            "dept_id",
            "dept_name",
            "planner_name",
            "status",
            "total_planned_amount",
            "line_count",
            "planned_qty",
            "booked_qty",
            "req_qty",
            "ordered_qty",
            "remaining_qty",
            "exec_rate",
            "req_rate",
            "not_started_lines",
            "partial_lines",
            "done_lines",
            "over_lines",
            "created_at",
        ] {
            assert!(HEAD_COLUMNS.contains(column), "HEAD_COLUMNS 缺 {column}");
        }
    }

    /// 明细列清单同上。
    #[test]
    fn item_columns_cover_reader() {
        for column in [
            "plan_item_id",
            "line_no",
            "material_id",
            "material_code",
            "material_name",
            "warehouse_id",
            "warehouse_name",
            "unit_id",
            "unit_name",
            "planned_qty",
            "planned_price",
            "planned_amount",
            "booked_qty",
            "req_qty",
            "ordered_qty",
            "remaining_qty",
            "exec_rate",
            "req_rate",
            "exec_state",
            "req_count",
            "req_doc_count",
            "po_doc_count",
        ] {
            assert!(ITEM_COLUMNS.contains(column), "ITEM_COLUMNS 缺 {column}");
        }
    }

    /// 视图名与 V0449 逐字一致（视图改名 = 迁移与代码同时改，这里钉住）。
    #[test]
    fn view_names_match_migration() {
        assert_eq!(HEAD_VIEW, "v_purchase_plan_execution");
        assert_eq!(ITEM_VIEW, "v_purchase_plan_item_execution");
        // paginate 目标就是总览视图（写错成明细视图 = 分页数会按明细行数算）
        assert!(!HEAD_TARGET.is_scoped(), "执行率总览是 SCOPE-EXEMPT（V0446 头注释 8）");
        assert_eq!(HEAD_TARGET.table, HEAD_VIEW);
    }
}

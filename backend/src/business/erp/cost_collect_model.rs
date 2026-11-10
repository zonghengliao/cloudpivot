//! 成本归集明细（`biz_cost_collect`）的数据层（ARCH-9 §3.1 ③ · B2-6）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0371__cost_collect_four_tables.up.sql`
//! （与设计 §3.1 ③ 逐字一致）。本文件只做**读写原语**与口径 SQL，不做编排
//! （编排在 [`crate::business::erp::cost_calc_material`]，纯逻辑在
//! [`crate::business::erp::cost_collect_dto`]）。
//!
//! # 为什么扫描口径写在 SQL 里、校验写在 dto 里
//!
//! 「只扫已过账 + 挂了成本对象的领料」是**取数条件**，必须由数据库一次过滤
//! （否则要把全表领料行搬进内存）；而「金额取账上真实扣减 / 幂等键 / 增量计划」
//! 是**判据**，放纯函数才能单测。两边各守一段，互不重复。
//!
//! # 关于 `dept_id` / `dept_name`
//!
//! 料行的车间取成本对象上的 `workshop_dept_id`（`biz_cost_object`），车间名优先用
//! `sys_dept` 的**实时名**（AGENTS 约定 13：主数据关联要显示「现在是谁」），
//! join 落空时回落到成本对象上的车间名快照（部门已删的兜底）。
//! 池行（费）的 `dept_id` 另有来源，与本文件无关。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_collect_dto::{self as dto, CollectKey, ExistingCollectRow, MaterialLine};
use crate::business::erp::cost_labor_collect_dto::{self as labor_dto, LaborLine};

/// 归集表名。
pub const COLLECT_TABLE: &str = "biz_cost_collect";

/// 本模块要读回的列（与 V0371 逐列对应；写语句另有列清单）。
pub const COLLECT_COLUMNS: &str = "id, cost_object_id, period_code, element, source_type, source_id, \
     source_no, line_no, occur_date, material_id, qty, unit_price, amount, dept_id, dept_name";

/// 候补领料行的取数 SQL（设计 §4.3 料行口径，逐条落在 WHERE 里）。
///
/// | 条件 | 为什么 |
/// |---|---|
/// | `h.out_type = 'ISSUE'` | 只有领用进料成本；报废/盘亏恒进 1901（§4.3 + B1-10 的借方分流） |
/// | `h.status = 'POSTED'` | 未过账的出库没有账上真实金额（草稿 `amount` 恒 0），归集它等于归集 0 |
/// | `h.cost_object_id IS NOT NULL` | 不挂对象 = 车间公用消耗，不进料成本（§4.3 / `ck_biz_cost_collect_obj`） |
/// | `h.deleted_at IS NULL` | 软删的旧单不参与 |
/// | `h.out_date BETWEEN ? AND ?` | 期间归属按出库日期（§4.3 口径说明 ②） |
pub const MATERIAL_SOURCE_SQL: &str = "\
    SELECT i.id              AS item_id, \
           h.id              AS stock_out_id, \
           h.out_no          AS out_no, \
           h.out_date        AS out_date, \
           h.cost_object_id  AS cost_object_id, \
           i.material_id     AS material_id, \
           i.qty             AS qty, \
           i.unit_cost       AS unit_price, \
           i.amount          AS amount, \
           o.workshop_dept_id AS dept_id, \
           COALESCE(d.dept_name, o.workshop_dept_name) AS dept_name \
      FROM biz_stock_out_item i \
      JOIN biz_stock_out h ON h.id = i.stock_out_id \
      LEFT JOIN biz_cost_object o ON o.id = h.cost_object_id \
      LEFT JOIN sys_dept d ON d.id = o.workshop_dept_id \
     WHERE h.deleted_at IS NULL \
       AND h.status = 'POSTED' \
       AND h.out_type = 'ISSUE' \
       AND h.cost_object_id IS NOT NULL \
       AND h.out_date >= ?::text::date \
       AND h.out_date <= ?::text::date \
     ORDER BY h.out_date ASC, h.out_no ASC, i.line_no ASC, i.id ASC";

/// 写一条料归集行的 SQL（幂等：撞 `uk_biz_cost_collect_src` 就跳过）。
///
/// `ON CONFLICT` 的目标列与部分索引 `uk_biz_cost_collect_src` 的谓词必须逐字一致
/// （PG 靠「列 + 谓词」做索引推断，少一半就报 `no unique or exclusion constraint matching`）。
pub const INSERT_MATERIAL_SQL: &str = "\
    INSERT INTO biz_cost_collect \
        (id, cost_object_id, period_code, element, source_type, source_id, source_no, line_no, \
         occur_date, material_id, qty, unit_price, amount, dept_id, dept_name, \
         created_at, updated_at, create_by, update_by) \
    VALUES (?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, ?::text::date, \
            ?::uuid, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
            NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
            now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
    ON CONFLICT (source_type, source_id, line_no) \
        WHERE deleted_at IS NULL AND source_id IS NOT NULL \
    DO NOTHING";

// ==================== 读 ====================

impl MaterialLine {
    /// 由 [`MATERIAL_SOURCE_SQL`] 的行构造。
    fn from_source_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            item_id: row.get_typed("item_id")?,
            stock_out_id: row.get_typed("stock_out_id")?,
            out_no: row.get_typed("out_no")?,
            out_date: row.get_typed("out_date")?,
            cost_object_id: row.get_typed("cost_object_id")?,
            material_id: row.get_typed("material_id")?,
            qty: row.get_typed("qty")?,
            unit_price: row.get_typed("unit_price")?,
            amount: row.get_typed("amount")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
        })
    }
}

/// 扫描期间内**全部**候补领料行（口径见 [`MATERIAL_SOURCE_SQL`]）。
///
/// 只取数不做幂等判断：已归集与否由 [`collect_rows_of_period`] 与
/// [`dto::plan`] 决定（这样「为什么这行没写」在纯函数里可单测）。
pub async fn material_source_lines(
    ex: &impl SqlExecutor,
    start: NaiveDate,
    end: NaiveDate,
) -> DbResult<Vec<MaterialLine>> {
    let rows = ex
        .query_sql(
            MATERIAL_SOURCE_SQL.to_string(),
            vec![Value::Text(start.to_string()), Value::Text(end.to_string())],
        )
        .await?;
    rows.iter().map(MaterialLine::from_source_row).collect()
}

/// 当期已归集的**料行**（软删的不算；`source_id` 为空的行不可能来自领料，一并排除）。
pub async fn collect_rows_of_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<ExistingCollectRow>> {
    let sql = format!(
        "SELECT id, source_type, source_id, line_no, source_no, amount, cost_object_id \
           FROM {COLLECT_TABLE} \
          WHERE period_code = ? AND element = ? AND source_type = ? \
            AND deleted_at IS NULL AND source_id IS NOT NULL \
          ORDER BY source_no ASC NULLS LAST, id ASC"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(dto::ELEMENT_MATERIAL.to_string()),
                Value::Text(dto::SOURCE_STOCK_OUT.to_string()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ExistingCollectRow {
                id: row.get_typed("id")?,
                key: CollectKey {
                    source_type: row.get_typed("source_type")?,
                    source_id: row.get_typed("source_id")?,
                    line_no: row.get_typed("line_no")?,
                },
                source_no: row.get_typed("source_no")?,
                amount: row.get_typed("amount")?,
                cost_object_id: row.get_typed("cost_object_id")?,
            })
        })
        .collect()
}

/// 写后回查：当期料归集的行数与金额合计（`C-COST-5` 用的就是这个数）。
pub async fn collected_total(ex: &impl SqlExecutor, period_code: &str) -> DbResult<(i64, Decimal)> {
    let sql = format!(
        "SELECT COUNT(*) AS rows, COALESCE(SUM(amount), 0) AS amount \
           FROM {COLLECT_TABLE} \
          WHERE period_code = ? AND element = ? AND source_type = ? AND deleted_at IS NULL"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(dto::ELEMENT_MATERIAL.to_string()),
                Value::Text(dto::SOURCE_STOCK_OUT.to_string()),
            ],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok((row.get_typed("rows")?, row.get_typed("amount")?)),
        None => Ok((0, Decimal::ZERO)),
    }
}

// ==================== 写 ====================

/// 写一条料归集行（幂等：已存在则 0 行）。
pub async fn insert_material_line(
    ex: &impl SqlExecutor,
    period_code: &str,
    line: &MaterialLine,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        INSERT_MATERIAL_SQL.to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&line.cost_object_id),
            Value::Text(period_code.to_string()),
            Value::Text(dto::ELEMENT_MATERIAL.to_string()),
            Value::Text(dto::SOURCE_STOCK_OUT.to_string()),
            Value::from(&line.item_id),
            Value::Text(line.out_no.clone()),
            Value::I32(dto::MATERIAL_LINE_NO),
            Value::Text(line.out_date.to_string()),
            Value::from(&line.material_id),
            Value::Text(line.qty.to_string()),
            Value::Text(line.unit_price.to_string()),
            Value::Text(line.amount.to_string()),
            Value::Text(dto::opt_uuid_text(line.dept_id)),
            Value::Text(line.dept_name.clone().unwrap_or_default()),
            Value::Text(dto::opt_uuid_text(operator)),
            Value::Text(dto::opt_uuid_text(operator)),
        ],
    )
    .await
}

/// 批量写（**调用方负责事务**；返回受影响行数之和 = 真实新增行数）。
pub async fn insert_material_lines(
    ex: &impl SqlExecutor,
    period_code: &str,
    lines: &[MaterialLine],
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let mut inserted = 0u64;
    for line in lines {
        inserted += insert_material_line(ex, period_code, line, operator).await?;
    }
    Ok(inserted)
}

/// 软删一条归集行（保留留痕；`uk_biz_cost_collect_src` 是部分索引，删后键即释放）。
pub async fn soft_delete_row(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    let sql = format!(
        "UPDATE {COLLECT_TABLE} SET deleted_at = now(), updated_at = now(), \
                update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
    );
    ex.exec_sql(sql, vec![Value::Text(dto::opt_uuid_text(operator)), Value::from(&id)])
        .await
}

// ==================== 工归集（calc 第 2 段 · B2-7） ====================
//
// 与料行同一张表、同一套幂等键，只换 `element` / `source_type` / 取数来源。
// 各段各守自己的 (element, source_type) 组合，互不踩（料段只看 MATERIAL+STOCK_OUT，
// 工段只看 LABOR+LABOR_TICKET）。

/// 候补工时行的取数 SQL（设计 §4.3 工行口径，逐条落在 WHERE 里）。
///
/// | 条件 | 为什么 |
/// |---|---|
/// | `l.status = 'CONFIRMED'` | 验收项：结算只取已确认的工时（DRAFT 还能改，归集它等于归集一个会变的数） |
/// | `l.period_code = ?` | 期间归属按台账上选的期间（§4.3 口径说明 ②：跨月补录按录入时选的期间） |
/// | `l.deleted_at IS NULL` | 软删的旧行不参与 |
///
/// 金额列直接取 `l.amount`（`ck_biz_cost_labor_amount` 已钉死 `= round(hours × rate, 4)`），
/// **不在 SQL 里再乘一遍**：多一个乘点就多一个舍入点，且会让「台账金额」与「归集金额」有分毫差的可能。
pub const LABOR_SOURCE_SQL: &str = "\
    SELECT l.id             AS labor_id, \
           l.work_date      AS work_date, \
           l.cost_object_id AS cost_object_id, \
           o.object_no      AS cost_object_no, \
           l.employee_name  AS employee_name, \
           l.dept_id        AS dept_id, \
           COALESCE(d.dept_name, l.dept_name) AS dept_name, \
           l.hours          AS hours, \
           l.machine_hours  AS machine_hours, \
           l.rate           AS rate, \
           l.amount         AS amount, \
           l.source_type    AS source_type \
      FROM biz_cost_labor l \
      LEFT JOIN biz_cost_object o ON o.id = l.cost_object_id \
      LEFT JOIN sys_dept d ON d.id = l.dept_id \
     WHERE l.deleted_at IS NULL \
       AND l.status = 'CONFIRMED' \
       AND l.period_code = ? \
     ORDER BY l.work_date ASC, l.id ASC";

/// 写一条工归集行的 SQL（幂等：撞 `uk_biz_cost_collect_src` 就跳过）。
///
/// 与料行的区别（都受库层 CHECK 约束）：`material_id` / `qty` / `unit_price` 必须为 NULL
/// （`ck_biz_cost_collect_mat`：工行不许挂物料），`source_no` 为 NULL（工时台账没有单号列，
/// 设计 §3.1 ④），人话上下文写 `remark`。
pub const INSERT_LABOR_SQL: &str = "\
    INSERT INTO biz_cost_collect \
        (id, cost_object_id, period_code, element, source_type, source_id, source_no, line_no, \
         occur_date, material_id, qty, unit_price, amount, dept_id, dept_name, remark, \
         created_at, updated_at, create_by, update_by) \
    VALUES (?::uuid, ?::uuid, ?, ?, ?, ?::uuid, NULL, ?, ?::text::date, \
            NULL, NULL, NULL, ?::text::numeric, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), \
            NULLIF(?::text,''), \
            now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
    ON CONFLICT (source_type, source_id, line_no) \
        WHERE deleted_at IS NULL AND source_id IS NOT NULL \
    DO NOTHING";

impl LaborLine {
    /// 由 [`LABOR_SOURCE_SQL`] 的行构造。
    fn from_labor_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            labor_id: row.get_typed("labor_id")?,
            work_date: row.get_typed("work_date")?,
            cost_object_id: row.get_typed("cost_object_id")?,
            cost_object_no: row.get_typed("cost_object_no")?,
            employee_name: row.get_typed("employee_name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            hours: row.get_typed("hours")?,
            machine_hours: row.get_typed("machine_hours")?,
            rate: row.get_typed("rate")?,
            amount: row.get_typed("amount")?,
            source_type: row.get_typed("source_type")?,
        })
    }
}

/// 扫描期间内**全部**候补工时行（口径见 [`LABOR_SOURCE_SQL`]）。
pub async fn labor_source_lines(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<LaborLine>> {
    let rows = ex
        .query_sql(LABOR_SOURCE_SQL.to_string(), vec![Value::Text(period_code.to_string())])
        .await?;
    rows.iter().map(LaborLine::from_labor_row).collect()
}

/// 当期已归集的**工行**（软删的不算；`source_id` 为空的行不可能来自工时，一并排除）。
pub async fn labor_collect_rows_of_period(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> DbResult<Vec<ExistingCollectRow>> {
    let sql = format!(
        "SELECT id, source_type, source_id, line_no, source_no, amount, cost_object_id \
           FROM {COLLECT_TABLE} \
          WHERE period_code = ? AND element = ? AND source_type = ? \
            AND deleted_at IS NULL AND source_id IS NOT NULL \
          ORDER BY source_id ASC, id ASC"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(labor_dto::ELEMENT_LABOR.to_string()),
                Value::Text(labor_dto::SOURCE_LABOR_TICKET.to_string()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ExistingCollectRow {
                id: row.get_typed("id")?,
                key: CollectKey {
                    source_type: row.get_typed("source_type")?,
                    source_id: row.get_typed("source_id")?,
                    line_no: row.get_typed("line_no")?,
                },
                source_no: row.get_typed("source_no")?,
                amount: row.get_typed("amount")?,
                cost_object_id: row.get_typed("cost_object_id")?,
            })
        })
        .collect()
}

/// 写一条工归集行（幂等：已存在则 0 行）。
pub async fn insert_labor_line(
    ex: &impl SqlExecutor,
    period_code: &str,
    line: &LaborLine,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        INSERT_LABOR_SQL.to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&line.cost_object_id),
            Value::Text(period_code.to_string()),
            Value::Text(labor_dto::ELEMENT_LABOR.to_string()),
            Value::Text(labor_dto::SOURCE_LABOR_TICKET.to_string()),
            Value::from(&line.labor_id),
            Value::I32(labor_dto::LABOR_LINE_NO),
            Value::Text(line.work_date.to_string()),
            Value::Text(line.amount.to_string()),
            Value::Text(dto::opt_uuid_text(Some(line.dept_id))),
            Value::Text(line.dept_name.clone().unwrap_or_default()),
            Value::Text(line.remark()),
            Value::Text(dto::opt_uuid_text(operator)),
            Value::Text(dto::opt_uuid_text(operator)),
        ],
    )
    .await
}

/// 批量写（**调用方负责事务**；返回受影响行数之和 = 真实新增行数）。
pub async fn insert_labor_lines(
    ex: &impl SqlExecutor,
    period_code: &str,
    lines: &[LaborLine],
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let mut inserted = 0u64;
    for line in lines {
        inserted += insert_labor_line(ex, period_code, line, operator).await?;
    }
    Ok(inserted)
}

/// 写后回查：当期工归集的行数与金额合计（「人工进了多少成本」就取这个数）。
pub async fn collected_labor_total(ex: &impl SqlExecutor, period_code: &str) -> DbResult<(i64, Decimal)> {
    let sql = format!(
        "SELECT COUNT(*) AS rows, COALESCE(SUM(amount), 0) AS amount \
           FROM {COLLECT_TABLE} \
          WHERE period_code = ? AND element = ? AND source_type = ? AND deleted_at IS NULL"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(labor_dto::ELEMENT_LABOR.to_string()),
                Value::Text(labor_dto::SOURCE_LABOR_TICKET.to_string()),
            ],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok((row.get_typed("rows")?, row.get_typed("amount")?)),
        None => Ok((0, Decimal::ZERO)),
    }
}

// ==================== 单测（不需要数据库：只钉 SQL 形状与列清单） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 扫描 SQL 的三个硬条件一个都不能掉（掉了就会归集不该归集的钱）。
    #[test]
    fn source_sql_keeps_the_three_hard_filters() {
        assert!(MATERIAL_SOURCE_SQL.contains("h.status = 'POSTED'"));
        assert!(MATERIAL_SOURCE_SQL.contains("h.out_type = 'ISSUE'"));
        assert!(MATERIAL_SOURCE_SQL.contains("h.cost_object_id IS NOT NULL"));
        assert!(MATERIAL_SOURCE_SQL.contains("h.deleted_at IS NULL"));
    }

    /// 金额取明细行上的真实扣减值，不按「数量 × 单价」重算。
    #[test]
    fn source_sql_takes_the_posted_amount_not_a_recomputation() {
        assert!(MATERIAL_SOURCE_SQL.contains("i.amount"));
        assert!(MATERIAL_SOURCE_SQL.contains("AS amount"));
        assert!(!MATERIAL_SOURCE_SQL.to_uppercase().contains("QTY *"));
        assert!(MATERIAL_SOURCE_SQL.contains("i.unit_cost"));
    }

    /// 幂等插入的 `ON CONFLICT` 目标必须与部分索引谓词逐字一致。
    #[test]
    fn insert_sql_matches_the_partial_unique_index() {
        assert!(INSERT_MATERIAL_SQL.contains("ON CONFLICT (source_type, source_id, line_no)"));
        assert!(INSERT_MATERIAL_SQL.contains("WHERE deleted_at IS NULL AND source_id IS NOT NULL"));
        assert!(INSERT_MATERIAL_SQL.contains("DO NOTHING"));
    }

    /// 写入的要素 / 来源类型由代码定（不接受外部传值），避免「同一列两种语义」。
    #[test]
    fn insert_binds_element_and_source_from_constants() {
        assert_eq!(dto::ELEMENT_MATERIAL, "MATERIAL");
        assert_eq!(dto::SOURCE_STOCK_OUT, "STOCK_OUT");
        // SQL 里这两个值是占位符（?），由 insert_material_line 绑常量
        assert!(INSERT_MATERIAL_SQL.contains("element, source_type"));
    }

    /// 读列清单必须覆盖 `ExistingCollectRow` 的字段（漏一列就会在真库用例里炸）。
    #[test]
    fn existing_row_columns_are_all_selected() {
        assert!(COLLECT_COLUMNS.contains("source_type"));
        assert!(COLLECT_COLUMNS.contains("source_id"));
        assert!(COLLECT_COLUMNS.contains("line_no"));
        assert!(COLLECT_COLUMNS.contains("amount"));
    }

    // ---------- 工归集（B2-7） ----------

    /// 验收项「只取 CONFIRMED 状态」：取数 SQL 里必须有状态闸，且按期间过滤。
    #[test]
    fn labor_source_sql_only_takes_confirmed_rows_of_the_period() {
        assert!(LABOR_SOURCE_SQL.contains("l.status = 'CONFIRMED'"));
        assert!(LABOR_SOURCE_SQL.contains("l.period_code = ?"));
        assert!(LABOR_SOURCE_SQL.contains("l.deleted_at IS NULL"));
        assert!(!LABOR_SOURCE_SQL.contains("l.status <> 'CONFIRMED'"));
    }

    /// 金额取台账上的 `amount`（`round(hours × rate, 4)` 已由库层 CHECK 钉死），不重算。
    #[test]
    fn labor_source_sql_takes_the_ledger_amount_not_a_recomputation() {
        assert!(LABOR_SOURCE_SQL.contains("l.amount"));
        assert!(LABOR_SOURCE_SQL.contains("AS amount"));
        assert!(!LABOR_SOURCE_SQL.to_uppercase().contains("HOURS *"));
        assert!(!LABOR_SOURCE_SQL.to_uppercase().contains("ROUND("));
    }

    /// 工行的库层约束：`material_id` / `qty` / `unit_price` 必须写 NULL（`ck_biz_cost_collect_mat`）。
    #[test]
    fn labor_insert_sql_leaves_material_columns_null() {
        assert!(INSERT_LABOR_SQL.contains("NULL, NULL, NULL"));
        assert!(INSERT_LABOR_SQL.contains("NULL, ?, ?::text::date"));
        assert!(!INSERT_LABOR_SQL.contains("material_id)"));
    }

    /// 幂等插入的 `ON CONFLICT` 目标必须与部分索引谓词逐字一致（与料行同一索引）。
    #[test]
    fn labor_insert_sql_matches_the_partial_unique_index() {
        assert!(INSERT_LABOR_SQL.contains("ON CONFLICT (source_type, source_id, line_no)"));
        assert!(INSERT_LABOR_SQL.contains("WHERE deleted_at IS NULL AND source_id IS NOT NULL"));
        assert!(INSERT_LABOR_SQL.contains("DO NOTHING"));
    }

    /// 两段的要素 / 来源组合不同（各扫各的，互不踩）。
    #[test]
    fn material_and_labor_segments_use_disjoint_elements() {
        assert_eq!(labor_dto::ELEMENT_LABOR, "LABOR");
        assert_eq!(labor_dto::SOURCE_LABOR_TICKET, "LABOR_TICKET");
        assert_ne!(labor_dto::ELEMENT_LABOR, dto::ELEMENT_MATERIAL);
        assert_ne!(labor_dto::SOURCE_LABOR_TICKET, dto::SOURCE_STOCK_OUT);
        // 设计 §4.3 幂等键第三段：工 0、料 1
        assert_ne!(labor_dto::LABOR_LINE_NO, dto::MATERIAL_LINE_NO);
    }
}

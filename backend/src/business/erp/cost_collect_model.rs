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
}

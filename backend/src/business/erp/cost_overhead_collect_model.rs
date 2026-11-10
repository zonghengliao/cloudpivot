//! 费归集（`biz_cost_collect` 的 OVERHEAD 段）数据层（ARCH-9 §4.3 费三路 · B2-8）。
//!
//! 与料 / 工段**同一张表、同一套幂等键**，只换 `element` / `source_type` / 取数来源。
//! 本文件只做**读写原语**与口径 SQL，不做编排（编排在
//! [`crate::business::erp::cost_calc_overhead`]，纯逻辑在
//! [`crate::business::erp::cost_overhead_collect_dto`]）。
//!
//! # 三路来源（设计 §4.3 表）
//!
//! | 路 | 表 | 只扫什么 | 科目从哪来 |
//! |---|---|---|---|
//! | 折旧 | `biz_depreciation` | `status='POSTED'` + 当期 + `expense_account_code LIKE '4101.%'` | 行上的 `expense_account_code` |
//! | 报销 | `biz_expense_item` ⋈ `biz_expense_report` | 主单 `status IN ('APPROVED','PAID')` + 当期 + 未软删 + `account_code LIKE '4101.%'` | 明细上的 `account_code`（入账时快照） |
//! | 手工 | `biz_cost_overhead` | 当期 + 未软删 | 行上的 `debit_account_code` |
//!
//! # 为什么金额都取来源行的账上值
//!
//! - 折旧 `biz_depreciation.amount`：计提时写入，且已由 `depreciation_linkage` 生成凭证；
//! - 报销 `biz_expense_item.amount`：**不含税**，与 `expense_bridge` 生成凭证时借记费用科目的
//!   金额同源（税金单独走「应交税费」行）；
//! - 手工 `biz_cost_overhead.amount`：财务录入值。
//!
//! 段里重算（按数量 / 按价税拆分）必然与 4101 的账面发生额分毫，违反 §4.3 口径说明 ①。
//!
//! # 池汇总为什么还要 join 回来源表
//!
//! `biz_cost_collect`（V0371 落地的 DDL）**没有 `account_code` 列**，而设计 §4.4.3 ① 的池是
//! 「(车间, 科目) → 金额」。所以池汇总必须按 `source_type` 反查来源行的科目 ——
//! 与 B2-5 试算的 `cost_alloc_rule_model::pool_rows` 口径一致（那边也是这么反查的）。
//! 这里多带出车间名与笔数，供 calc 编排卡直接展示 / 对账。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_collect_dto::{self as collect, CollectKey};
use crate::business::erp::cost_overhead_collect_dto::{
    self as dto, ELEMENT_OVERHEAD, OVERHEAD_LINE_NO, PoolAmount, SOURCE_DEPRECIATION, SOURCE_EXPENSE,
    SOURCE_OVERHEAD_TICKET,
};

/// 归集表名（与料 / 工段同一张表）。
pub const COLLECT_TABLE: &str = crate::business::erp::cost_collect_model::COLLECT_TABLE;

// ==================== 取数 SQL（三路） ====================

/// 折旧行取数 SQL（设计 §4.3 费·折旧）。
///
/// | 条件 | 为什么 |
/// |---|---|
/// | `dp.status = 'POSTED'` | 只有已过账的计提进成本；`REVERSED`（已红冲）不算 |
/// | `dp.period_code = ?` | 期间归属按单据期间（§4.3 口径说明 ②） |
/// | `dp.expense_account_code LIKE '4101.%'` | 费用科目配成 5602.* 的资产「不进成本」（§4.5.1，零代码可切） |
///
/// `occur_date` 取批次计提日（`biz_depreciation_batch.run_at`）：折旧明细表没有日期列，
/// 计提日是该行唯一真实的「发生时间」；批次缺失（理论上不会）时回落期间首日。
pub const DEPRECIATION_SOURCE_SQL: &str = "\
    SELECT dp.id                      AS source_id, \
           dp.asset_no                AS source_no, \
           dp.asset_no                AS asset_no, \
           dp.asset_name              AS asset_name, \
           COALESCE(b.run_at::date, (dp.period_code || '-01')::date) AS occur_date, \
           dp.dept_id                 AS dept_id, \
           COALESCE(d.dept_name, dp.dept_name) AS dept_name, \
           dp.expense_account_code    AS account_code, \
           dp.amount                  AS amount \
      FROM biz_depreciation dp \
      LEFT JOIN biz_depreciation_batch b ON b.id = dp.batch_id \
      LEFT JOIN sys_dept d ON d.id = dp.dept_id \
     WHERE dp.status = 'POSTED' \
       AND dp.period_code = ? \
       AND dp.expense_account_code LIKE '4101.%' \
     ORDER BY dp.asset_no ASC, dp.id ASC";

/// 报销明细取数 SQL（设计 §4.3 费·报销）。
///
/// | 条件 | 为什么 |
/// |---|---|
/// | `r.status IN ('APPROVED','PAID')` | 设计口径：只有审批通过（已过账生成凭证）的单据才进成本 |
/// | `r.period_code = ?` | 期间归属按报销单期间（审批通过时写入） |
/// | `r.deleted_at IS NULL` | 软删的旧单不参与 |
/// | `ei.account_code LIKE '4101.%'` | 费用类型没绑 4101.* 的明细「不进成本」（§4.5.1） |
///
/// 车间取**报销单**的 `dept_id`（与 `expense_bridge` 生成凭证时借记部门口径一致），
/// 不是报销人当前所在部门 —— 部门快照，事后调岗不会把历史费用搬到新车间。
pub const EXPENSE_SOURCE_SQL: &str = "\
    SELECT ei.id                     AS source_id, \
           r.report_no               AS source_no, \
           COALESCE(ei.summary, '')  AS summary, \
           ei.expense_date           AS occur_date, \
           r.dept_id                 AS dept_id, \
           COALESCE(d.dept_name, r.dept_name) AS dept_name, \
           ei.account_code           AS account_code, \
           ei.amount                 AS amount \
      FROM biz_expense_item ei \
      JOIN biz_expense_report r ON r.id = ei.report_id \
      LEFT JOIN sys_dept d ON d.id = r.dept_id \
     WHERE r.deleted_at IS NULL \
       AND r.status IN ('APPROVED','PAID') \
       AND r.period_code = ? \
       AND ei.account_code LIKE '4101.%' \
     ORDER BY ei.expense_date ASC, r.report_no ASC, ei.line_no ASC, ei.id ASC";

/// 手工台账取数 SQL（设计 §4.3 费·手工）。
///
/// | 条件 | 为什么 |
/// |---|---|
/// | `t.period_code = ?` | 台账按期间录入 |
/// | `t.deleted_at IS NULL` | B2-4 的删除是软删（留痕可追溯） |
/// | `t.debit_account_code LIKE '4101.%'` | 库层 `ck_biz_cost_oh_debit` 已钉死，这里显式写出便于自检 |
///
/// `occur_date` 取录入日（`created_at::date`）：台账表没有业务日期列，
/// 归集期间由 `period_code` 决定，`occur_date` 只作展示。
pub const OVERHEAD_SOURCE_SQL: &str = "\
    SELECT t.id                        AS source_id, \
           t.cost_object_id            AS cost_object_id, \
           t.period_code               AS period_code, \
           t.created_at::date          AS occur_date, \
           t.dept_id                   AS dept_id, \
           COALESCE(d.dept_name, t.dept_name) AS dept_name, \
           t.debit_account_code        AS account_code, \
           t.expense_item              AS expense_item, \
           t.amount                    AS amount \
      FROM biz_cost_overhead t \
      LEFT JOIN sys_dept d ON d.id = t.dept_id \
     WHERE t.deleted_at IS NULL \
       AND t.period_code = ? \
       AND t.debit_account_code LIKE '4101.%' \
     ORDER BY t.dept_id ASC, t.created_at ASC, t.id ASC";

/// 与 V0371 的 `ck_biz_cost_collect_src` 值域一致（本段只写这三种）。
pub const SEGMENT_SOURCES_SQL_LIST: &str = "'OVERHEAD_TICKET','DEPRECIATION','EXPENSE'";

/// 写一条费归集行的 SQL（幂等：撞 `uk_biz_cost_collect_src` 就跳过）。
///
/// 与料 / 工行的区别（都受库层 CHECK 约束）：
/// - `material_id` / `qty` / `unit_price` 必须 NULL（`ck_biz_cost_collect_mat`）；
/// - `cost_object_id` **可为空**（池行）—— 只有费三路允许（`ck_biz_cost_collect_obj`）；
/// - `source_no`：报销取单号、折旧取资产编号、手工台账没有单号列 ⇒ NULL。
pub const INSERT_OVERHEAD_SQL: &str = "\
    INSERT INTO biz_cost_collect \
        (id, cost_object_id, period_code, element, source_type, source_id, source_no, line_no, \
         occur_date, material_id, qty, unit_price, amount, dept_id, dept_name, remark, \
         created_at, updated_at, create_by, update_by) \
    VALUES (?::uuid, NULLIF(?::text,'')::uuid, ?, ?, ?, ?::uuid, NULLIF(?::text,''), ?, ?::text::date, \
            NULL, NULL, NULL, ?::text::numeric, ?::uuid, NULLIF(?::text,''), NULLIF(?::text,''), \
            now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
    ON CONFLICT (source_type, source_id, line_no) \
        WHERE deleted_at IS NULL AND source_id IS NOT NULL \
    DO NOTHING";

/// 池汇总 SQL（设计 §4.4.3 ①：按 (车间, 科目) 汇总当期车间费用池）。
///
/// 只汇总**不挂成本对象**的费行（池行），科目按 `source_type` 反查来源表；
/// `ALLOC`（分摊结果）不进池 —— 它是池的**去向**不是来源，进来会自我循环。
const POOL_SQL: &str = "\
    SELECT p.dept_id, COALESCE(d.dept_name, p.dept_name) AS dept_name, p.account_code, \
           SUM(p.amount) AS amount, COUNT(*) AS rows \
      FROM ( \
          SELECT c.dept_id, oh.dept_name AS dept_name, oh.debit_account_code AS account_code, c.amount \
            FROM biz_cost_collect c \
            JOIN biz_cost_overhead oh ON oh.id = c.source_id \
           WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
             AND c.source_type = 'OVERHEAD_TICKET' AND c.deleted_at IS NULL \
          UNION ALL \
          SELECT c.dept_id, dp.dept_name, dp.expense_account_code, c.amount \
            FROM biz_cost_collect c \
            JOIN biz_depreciation dp ON dp.id = c.source_id \
           WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
             AND c.source_type = 'DEPRECIATION' AND c.deleted_at IS NULL \
          UNION ALL \
          SELECT c.dept_id, r.dept_name, ei.account_code, c.amount \
            FROM biz_cost_collect c \
            JOIN biz_expense_item ei ON ei.id = c.source_id \
            JOIN biz_expense_report r ON r.id = ei.report_id \
           WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
             AND c.source_type = 'EXPENSE' AND c.deleted_at IS NULL \
      ) p \
      LEFT JOIN sys_dept d ON d.id = p.dept_id \
     WHERE p.dept_id IS NOT NULL \
     GROUP BY p.dept_id, COALESCE(d.dept_name, p.dept_name), p.account_code \
     ORDER BY p.dept_id, p.account_code";

// ==================== 读 ====================

/// 扫描当期折旧行。
pub async fn depreciation_source_lines(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<dto::OverheadLine>> {
    let rows = ex
        .query_sql(
            DEPRECIATION_SOURCE_SQL.to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            let asset_no: String = row.get_typed("asset_no")?;
            let asset_name: String = row.get_typed("asset_name")?;
            Ok(dto::OverheadLine {
                source_type: SOURCE_DEPRECIATION.to_string(),
                source_id: row.get_typed("source_id")?,
                source_no: Some(row.get_typed("source_no")?),
                subject: format!("折旧计提 {period_code} {asset_no} {asset_name}"),
                occur_date: row.get_typed::<NaiveDate>("occur_date")?,
                cost_object_id: None,
                dept_id: row.get_typed("dept_id")?,
                dept_name: row.get_typed("dept_name")?,
                account_code: row.get_typed("account_code")?,
                amount: row.get_typed("amount")?,
            })
        })
        .collect()
}

/// 扫描当期报销行（已过账 + 科目 4101.*）。
pub async fn expense_source_lines(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<dto::OverheadLine>> {
    let rows = ex
        .query_sql(
            EXPENSE_SOURCE_SQL.to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            let report_no: String = row.get_typed("source_no")?;
            let summary: String = row.get_typed("summary")?;
            let summary = if summary.trim().is_empty() {
                "（无摘要）".to_string()
            } else {
                summary
            };
            Ok(dto::OverheadLine {
                source_type: SOURCE_EXPENSE.to_string(),
                source_id: row.get_typed("source_id")?,
                source_no: Some(report_no.clone()),
                subject: format!("报销单 {report_no} {summary}"),
                occur_date: row.get_typed::<NaiveDate>("occur_date")?,
                cost_object_id: None,
                dept_id: row.get_typed("dept_id")?,
                dept_name: row.get_typed("dept_name")?,
                account_code: row.get_typed("account_code")?,
                amount: row.get_typed("amount")?,
            })
        })
        .collect()
}

/// 扫描当期手工台账行（挂了对象的直接归集、没挂的进池）。
pub async fn overhead_source_lines(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<dto::OverheadLine>> {
    let rows = ex
        .query_sql(
            OVERHEAD_SOURCE_SQL.to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            let expense_item: String = row.get_typed("expense_item")?;
            Ok(dto::OverheadLine {
                source_type: SOURCE_OVERHEAD_TICKET.to_string(),
                source_id: row.get_typed("source_id")?,
                // 手工台账没有单号列（设计 §3.1 ⑤），不编造单号
                source_no: None,
                subject: format!("手工台账 {period_code} {expense_item}"),
                occur_date: row.get_typed::<NaiveDate>("occur_date")?,
                cost_object_id: row.get_typed("cost_object_id")?,
                dept_id: row.get_typed("dept_id")?,
                dept_name: row.get_typed("dept_name")?,
                account_code: row.get_typed("account_code")?,
                amount: row.get_typed("amount")?,
            })
        })
        .collect()
}

/// 三路候选行一次扫全（顺序：折旧 → 报销 → 手工，各自内部已定序）。
pub async fn all_source_lines(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<dto::OverheadLine>> {
    let mut lines = depreciation_source_lines(ex, period_code).await?;
    lines.extend(expense_source_lines(ex, period_code).await?);
    lines.extend(overhead_source_lines(ex, period_code).await?);
    Ok(lines)
}

/// 当期已归集的**费行**（三路来源，软删的不算）。
///
/// **只取本段三种 `source_type`**：`ALLOC` 行同样是 `element='OVERHEAD'`，
/// 若一并取回，本段的「过期判定」会把编排卡第 ⑥ 步刚写的分摊行全删掉。
pub async fn overhead_collect_rows_of_period(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> DbResult<Vec<dto::ExistingOverheadRow>> {
    let sql = format!(
        "SELECT id, source_type, source_id, line_no, source_no, amount, cost_object_id \
           FROM {COLLECT_TABLE} \
          WHERE period_code = ? AND element = ? \
            AND source_type IN ({SEGMENT_SOURCES_SQL_LIST}) \
            AND deleted_at IS NULL AND source_id IS NOT NULL \
          ORDER BY source_type ASC, source_id ASC, id ASC"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(ELEMENT_OVERHEAD.to_string()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(dto::ExistingOverheadRow {
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

/// 写后回查：当期费归集的行数与金额合计（三路来源）。
pub async fn collected_overhead_total(ex: &impl SqlExecutor, period_code: &str) -> DbResult<(i64, Decimal)> {
    let sql = format!(
        "SELECT COUNT(*) AS rows, COALESCE(SUM(amount), 0) AS amount \
           FROM {COLLECT_TABLE} \
          WHERE period_code = ? AND element = ? \
            AND source_type IN ({SEGMENT_SOURCES_SQL_LIST}) AND deleted_at IS NULL"
    );
    let rows = ex
        .query_sql(
            sql,
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(ELEMENT_OVERHEAD.to_string()),
            ],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok((row.get_typed("rows")?, row.get_typed("amount")?)),
        None => Ok((0, Decimal::ZERO)),
    }
}

/// 池汇总：当期车间费用池按 (车间, 科目) 汇总（验收项「成池」）。
///
/// 与 B2-5 试算用的 `cost_alloc_rule_model::pool_rows` 口径逐条相同（同一段 `UNION ALL`），
/// 区别是这里走**调用方的事务**（`ex`）并多带车间名与笔数，供归集段出参与对账。
pub async fn pool_of_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<PoolAmount>> {
    let params = vec![
        Value::Text(period_code.to_string()),
        Value::Text(period_code.to_string()),
        Value::Text(period_code.to_string()),
    ];
    let rows = ex.query_sql(POOL_SQL.to_string(), params).await?;
    rows.iter()
        .map(|row| {
            Ok(PoolAmount {
                dept_id: row.get_typed("dept_id")?,
                dept_name: row.get_typed("dept_name")?,
                account_code: row.get_typed("account_code")?,
                amount: row.get_typed("amount")?,
                rows: row.get_typed("rows")?,
            })
        })
        .collect()
}

// ==================== 写 ====================

/// 写一条费归集行（幂等：已存在则 0 行）。
pub async fn insert_overhead_line(
    ex: &impl SqlExecutor,
    period_code: &str,
    line: &dto::OverheadLine,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        INSERT_OVERHEAD_SQL.to_string(),
        vec![
            Value::from(&Uuid::now_v7()),
            Value::Text(collect::opt_uuid_text(line.cost_object_id)),
            Value::Text(period_code.to_string()),
            Value::Text(ELEMENT_OVERHEAD.to_string()),
            Value::Text(line.source_type.clone()),
            Value::from(&line.source_id),
            Value::Text(line.source_no.clone().unwrap_or_default()),
            Value::I32(OVERHEAD_LINE_NO),
            Value::Text(line.occur_date.to_string()),
            Value::Text(line.amount.to_string()),
            // uuid 参数必须绑原生类型：`$n::uuid` 会让驱动把参数类型推成 uuid，
            // 绑 Text 会报 `error serializing parameter`（见 `shared::test_helpers` 同款注释）
            Value::from(&line.dept_id),
            Value::Text(line.dept_name.clone().unwrap_or_default()),
            Value::Text(line.remark()),
            Value::Text(collect::opt_uuid_text(operator)),
            Value::Text(collect::opt_uuid_text(operator)),
        ],
    )
    .await
}

/// 批量写（**调用方负责事务**；返回受影响行数之和 = 真实新增行数）。
pub async fn insert_overhead_lines(
    ex: &impl SqlExecutor,
    period_code: &str,
    lines: &[dto::OverheadLine],
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let mut inserted = 0u64;
    for line in lines {
        inserted += insert_overhead_line(ex, period_code, line, operator).await?;
    }
    Ok(inserted)
}

// ==================== 单测（不需要数据库：只钉 SQL 形状与列口径） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 折旧：只取 POSTED + 当期 + 4101.*（三条都是验收项/设计口径）。
    #[test]
    fn depreciation_sql_keeps_the_three_hard_filters() {
        assert!(DEPRECIATION_SOURCE_SQL.contains("dp.status = 'POSTED'"));
        assert!(DEPRECIATION_SOURCE_SQL.contains("dp.period_code = ?"));
        assert!(DEPRECIATION_SOURCE_SQL.contains("dp.expense_account_code LIKE '4101.%'"));
        // 已红冲的同样 status='REVERSED'，不能被扫进来
        assert!(!DEPRECIATION_SOURCE_SQL.contains("'REVERSED'"));
    }

    /// 折旧金额取 `dp.amount`（计提时写入的账上值），不重算。
    #[test]
    fn depreciation_sql_takes_the_posted_amount() {
        assert!(DEPRECIATION_SOURCE_SQL.contains("dp.amount"));
        assert!(DEPRECIATION_SOURCE_SQL.contains("AS amount"));
        assert!(!DEPRECIATION_SOURCE_SQL.to_uppercase().contains("ROUND("));
    }

    /// 报销：主单状态 + 当期 + 未软删 + 4101.*（验收项「已过账的费用报销」）。
    #[test]
    fn expense_sql_keeps_the_hard_filters() {
        assert!(EXPENSE_SOURCE_SQL.contains("r.status IN ('APPROVED','PAID')"));
        assert!(EXPENSE_SOURCE_SQL.contains("r.period_code = ?"));
        assert!(EXPENSE_SOURCE_SQL.contains("r.deleted_at IS NULL"));
        assert!(EXPENSE_SOURCE_SQL.contains("ei.account_code LIKE '4101.%'"));
        // 不含税金额（凭证按 amount 借记费用科目，税金单独一行）
        assert!(EXPENSE_SOURCE_SQL.contains("ei.amount"));
        assert!(!EXPENSE_SOURCE_SQL.contains("with_tax_amount"));
        assert!(!EXPENSE_SOURCE_SQL.contains("tax_amount"));
    }

    /// 报销的车间取**单据**部门（凭证口径），不 join 报销人当前部门。
    #[test]
    fn expense_sql_takes_the_report_dept_not_the_employee_dept() {
        assert!(EXPENSE_SOURCE_SQL.contains("r.dept_id"));
        assert!(EXPENSE_SOURCE_SQL.contains("COALESCE(d.dept_name, r.dept_name)"));
        assert!(!EXPENSE_SOURCE_SQL.contains("sys_user"));
    }

    /// 手工台账：当期 + 未软删 + 4101.*；`cost_object_id` 可为空（池行）。
    #[test]
    fn overhead_ticket_sql_keeps_the_hard_filters() {
        assert!(OVERHEAD_SOURCE_SQL.contains("t.period_code = ?"));
        assert!(OVERHEAD_SOURCE_SQL.contains("t.deleted_at IS NULL"));
        assert!(OVERHEAD_SOURCE_SQL.contains("t.debit_account_code LIKE '4101.%'"));
        assert!(OVERHEAD_SOURCE_SQL.contains("t.cost_object_id"));
    }

    /// 三路都不许在 SQL 里筛成本对象为空（那会把手工台账的直接行丢掉）。
    #[test]
    fn no_source_sql_filters_out_direct_rows() {
        for sql in [DEPRECIATION_SOURCE_SQL, EXPENSE_SOURCE_SQL, OVERHEAD_SOURCE_SQL] {
            assert!(!sql.contains("cost_object_id IS NULL"), "取数层不许丢直接行");
        }
    }

    /// 幂等插入的 `ON CONFLICT` 目标必须与部分索引谓词逐字一致。
    #[test]
    fn insert_sql_matches_the_partial_unique_index() {
        assert!(INSERT_OVERHEAD_SQL.contains("ON CONFLICT (source_type, source_id, line_no)"));
        assert!(INSERT_OVERHEAD_SQL.contains("WHERE deleted_at IS NULL AND source_id IS NOT NULL"));
        assert!(INSERT_OVERHEAD_SQL.contains("DO NOTHING"));
        // 池行要能写空对象（NULLIF 而不是硬绑）
        assert!(INSERT_OVERHEAD_SQL.contains("NULLIF(?::text,'')::uuid"));
        // 料列必须 NULL（ck_biz_cost_collect_mat）
        assert!(INSERT_OVERHEAD_SQL.contains("NULL, NULL, NULL"));
    }

    /// 已归集行只取本段三种来源：`ALLOC` 行绝不能被本段「过期判定」误删。
    #[test]
    fn existing_rows_sql_excludes_alloc_rows() {
        assert!(!SEGMENT_SOURCES_SQL_LIST.contains("ALLOC"));
        assert!(SEGMENT_SOURCES_SQL_LIST.contains("DEPRECIATION"));
        assert!(SEGMENT_SOURCES_SQL_LIST.contains("EXPENSE"));
        assert!(SEGMENT_SOURCES_SQL_LIST.contains("OVERHEAD_TICKET"));
        assert_eq!(dto::OVERHEAD_SOURCES.len(), 3);
    }

    /// 池汇总：三路反查科目 + 只算池行 + 按 (车间, 科目) 分组 + 排除 ALLOC。
    #[test]
    fn pool_sql_derives_account_from_every_pool_source() {
        assert!(POOL_SQL.contains("oh.debit_account_code"));
        assert!(POOL_SQL.contains("dp.expense_account_code"));
        assert!(POOL_SQL.contains("ei.account_code"));
        assert!(POOL_SQL.contains("c.cost_object_id IS NULL"));
        assert!(POOL_SQL.contains("GROUP BY p.dept_id"));
        assert!(POOL_SQL.contains("p.account_code"));
        // 车间名一次 join 拿全（不做 N+1）
        assert!(POOL_SQL.contains("LEFT JOIN sys_dept d ON d.id = p.dept_id"));
        assert!(!POOL_SQL.contains("'ALLOC'"));
        // 池里不许出现工 / 料段
        assert!(!POOL_SQL.contains("'LABOR'"));
        assert!(!POOL_SQL.contains("'MATERIAL'"));
    }

    /// 科目前缀常量与 DDL 的 `ck_biz_cost_oh_debit` 一致。
    #[test]
    fn account_prefix_matches_the_ddl() {
        assert_eq!(dto::ACCOUNT_PREFIX, "4101.");
        assert!(OVERHEAD_SOURCE_SQL.contains("LIKE '4101.%'"));
    }
}

//! calc 第 4 段的对账取数（ARCH-9 §4.11 C-COST-1 · B2-9）。
//!
//! 两张口径表：**池侧**（`biz_cost_collect` 池行，科目从三张来源单据反查）与
//! **账面侧**（`biz_voucher_entry` 的 `4101.*` 借方，带凭证号）。
//!
//! # 池侧为什么要自己拼这一段 `UNION ALL`
//!
//! `biz_cost_collect` 表（V0371）**没有 `account_code` 列** —— 科目只存在于三张来源单据里
//! （`biz_cost_overhead.debit_account_code` / `biz_depreciation.expense_account_code` /
//! `biz_expense_item.account_code`），所以池侧必须按 `source_type` 反查。
//! [`crate::business::erp::cost_overhead_collect_model`] 的 `POOL_SQL` 做的是同一件事，但它按
//! (车间, 科目) 汇总后**丢掉了 `source_type`**，而对账要按来源判断「此刻该不该有凭证」
//! （见 [`crate::business::erp::cost_reconcile_dto::Stage`]），因此这里多带一列来源类型。
//! 两份 SQL 的口径漂移由真库用例 `pool_by_source_matches_the_pool_of_the_collect_segment`
//! 兜住（它把本函数的 (车间,科目) 汇总与 `pool_of_period` 逐组对比）。
//!
//! # 账面侧为什么"只算借方 + 排除已红冲 / 红字凭证"
//!
//! - C-COST-1 的判据是**借方发生额**（不是净额）：结算阶段分摊凭证会把 4101 贷掉，
//!   用净额会让「结算后 4101 归零」与池永远对不上；
//! - 红字凭证的 4101 行落在**贷方**，天然不进借方合计；被红冲的原凭证（`reversed_by` 非空）
//!   必须整体排除，否则「折旧红冲后归集行已软删、凭证借方还在」会变成永久假差额。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::erp::cost_reconcile_dto::{LedgerSideRow, PoolSideRow};

/// 池侧：当期池行按 (车间, 科目, 来源类型) 汇总（三个 `?` 都是期间，按 `UNION ALL` 分支顺序绑定）。
pub const POOL_BY_SOURCE_SQL: &str = "\
    SELECT p.dept_id, COALESCE(d.dept_name, p.dept_name) AS dept_name, p.account_code, p.source_type, \
           SUM(p.amount) AS amount, COUNT(*) AS rows \
      FROM ( \
          SELECT c.dept_id, oh.dept_name AS dept_name, oh.debit_account_code AS account_code, \
                 'OVERHEAD_TICKET' AS source_type, c.amount \
            FROM biz_cost_collect c \
            JOIN biz_cost_overhead oh ON oh.id = c.source_id \
           WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
             AND c.source_type = 'OVERHEAD_TICKET' AND c.deleted_at IS NULL \
          UNION ALL \
          SELECT c.dept_id, dp.dept_name, dp.expense_account_code, 'DEPRECIATION', c.amount \
            FROM biz_cost_collect c \
            JOIN biz_depreciation dp ON dp.id = c.source_id \
           WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
             AND c.source_type = 'DEPRECIATION' AND c.deleted_at IS NULL \
          UNION ALL \
          SELECT c.dept_id, r.dept_name, ei.account_code, 'EXPENSE', c.amount \
            FROM biz_cost_collect c \
            JOIN biz_expense_item ei ON ei.id = c.source_id \
            JOIN biz_expense_report r ON r.id = ei.report_id \
           WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
             AND c.source_type = 'EXPENSE' AND c.deleted_at IS NULL \
      ) p \
      LEFT JOIN sys_dept d ON d.id = p.dept_id \
     WHERE p.dept_id IS NOT NULL \
     GROUP BY p.dept_id, COALESCE(d.dept_name, p.dept_name), p.account_code, p.source_type \
     ORDER BY p.dept_id, p.account_code, p.source_type";

/// 账面侧：当期 `4101.*` 借方发生额（逐条分录，带凭证号与部门辅助核算）。
pub const LEDGER_DEBIT_SQL: &str = "\
    SELECT e.aux_dept_id AS dept_id, d.dept_name AS dept_name, e.account_code AS account_code, \
           v.voucher_no AS voucher_no, e.debit_amount AS debit_amount \
      FROM biz_voucher_entry e \
      JOIN biz_voucher v ON v.id = e.voucher_id \
      LEFT JOIN sys_dept d ON d.id = e.aux_dept_id \
     WHERE v.period = ? AND v.status = 'POSTED' AND v.deleted_at IS NULL \
       AND v.is_red = 0 AND v.reversed_by IS NULL \
       AND e.account_code LIKE '4101.%' AND e.debit_amount > 0 \
     ORDER BY e.account_code ASC, v.voucher_no ASC, e.line_no ASC";

/// 读池侧（走调用方事务）。
pub async fn pool_by_source(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<PoolSideRow>> {
    let params = vec![
        Value::Text(period_code.to_string()),
        Value::Text(period_code.to_string()),
        Value::Text(period_code.to_string()),
    ];
    let rows = ex.query_sql(POOL_BY_SOURCE_SQL.to_string(), params).await?;
    rows.iter()
        .map(|row| {
            Ok(PoolSideRow {
                dept_id: row.get_typed("dept_id")?,
                dept_name: row.get_typed("dept_name")?,
                account_code: row.get_typed("account_code")?,
                source_type: row.get_typed("source_type")?,
                amount: row.get_typed::<Decimal>("amount")?,
                rows: row.get_typed("rows")?,
            })
        })
        .collect()
}

/// 读账面侧（走调用方事务）。
pub async fn ledger_debits(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<LedgerSideRow>> {
    let rows = ex
        .query_sql(LEDGER_DEBIT_SQL.to_string(), vec![Value::Text(period_code.to_string())])
        .await?;
    rows.iter()
        .map(|row| {
            Ok(LedgerSideRow {
                dept_id: row.get_typed("dept_id")?,
                dept_name: row.get_typed("dept_name")?,
                account_code: row.get_typed("account_code")?,
                voucher_no: row.get_typed("voucher_no")?,
                debit_amount: row.get_typed::<Decimal>("debit_amount")?,
            })
        })
        .collect()
}

// ==================== 单测（不需要数据库：只钉 SQL 形状与列口径） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 池侧只认池行（`cost_object_id IS NULL`）+ 三种费来源 + 未软删。
    #[test]
    fn pool_sql_keeps_the_pool_filters() {
        assert_eq!(POOL_BY_SOURCE_SQL.matches("c.cost_object_id IS NULL").count(), 3);
        assert_eq!(POOL_BY_SOURCE_SQL.matches("c.deleted_at IS NULL").count(), 3);
        assert_eq!(POOL_BY_SOURCE_SQL.matches("c.element = 'OVERHEAD'").count(), 3);
        for source in ["'DEPRECIATION'", "'EXPENSE'", "'OVERHEAD_TICKET'"] {
            assert!(
                POOL_BY_SOURCE_SQL.contains(source),
                "池侧必须覆盖 {source}：{POOL_BY_SOURCE_SQL}"
            );
        }
        // ALLOC（分摊结果）是池的去向，不是来源 —— 进来会自我循环
        assert!(!POOL_BY_SOURCE_SQL.contains("'ALLOC'"));
        // 料 / 工不是制造费用池
        assert!(!POOL_BY_SOURCE_SQL.contains("'MATERIAL'"));
        assert!(!POOL_BY_SOURCE_SQL.contains("'LABOR_TICKET'"));
    }

    /// 池侧科目必须从**三张来源单据**反查（collect 表没有 account_code 列）。
    #[test]
    fn pool_sql_reverse_looks_the_account_from_each_source_table() {
        assert!(POOL_BY_SOURCE_SQL.contains("oh.debit_account_code"));
        assert!(POOL_BY_SOURCE_SQL.contains("dp.expense_account_code"));
        assert!(POOL_BY_SOURCE_SQL.contains("ei.account_code"));
        // 分组键带来源类型（阶段判定要用）
        assert!(POOL_BY_SOURCE_SQL.contains("p.source_type"));
        assert!(POOL_BY_SOURCE_SQL.contains("GROUP BY p.dept_id"));
    }

    /// 账面侧四条硬过滤：只在账内（POSTED / 未删）、只算借方、排除红字与被红冲、只认 4101.*。
    #[test]
    fn ledger_sql_keeps_the_four_hard_filters() {
        assert!(LEDGER_DEBIT_SQL.contains("v.period = ?"));
        assert!(LEDGER_DEBIT_SQL.contains("v.status = 'POSTED'"));
        assert!(LEDGER_DEBIT_SQL.contains("v.deleted_at IS NULL"));
        assert!(LEDGER_DEBIT_SQL.contains("v.is_red = 0"));
        assert!(LEDGER_DEBIT_SQL.contains("v.reversed_by IS NULL"));
        assert!(LEDGER_DEBIT_SQL.contains("e.account_code LIKE '4101.%'"));
        assert!(LEDGER_DEBIT_SQL.contains("e.debit_amount > 0"));
        // 净额口径（debit - credit）不适用于借方发生额对账
        assert!(!LEDGER_DEBIT_SQL.contains("credit_amount"));
    }

    /// 账面侧必须带得出凭证号与部门（差额要能点到凭证、点到车间）。
    #[test]
    fn ledger_sql_carries_voucher_no_and_department() {
        assert!(LEDGER_DEBIT_SQL.contains("v.voucher_no AS voucher_no"));
        assert!(LEDGER_DEBIT_SQL.contains("e.aux_dept_id AS dept_id"));
        // 车间名实时取（不做逐行查询，无 N+1）
        assert!(LEDGER_DEBIT_SQL.contains("LEFT JOIN sys_dept d ON d.id = e.aux_dept_id"));
    }

    /// 段函数与 SQL 的参数个数是契约：池侧三个 `?`（每个 UNION 分支一个期间）。
    #[test]
    fn pool_params_match_the_placeholders() {
        assert_eq!(POOL_BY_SOURCE_SQL.matches('?').count(), 3);
        assert_eq!(LEDGER_DEBIT_SQL.matches('?').count(), 1);
    }
}

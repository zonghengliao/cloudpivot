//! 「未归集清单」的取数层（`biz_stock_out` / `biz_cost_labor` / `biz_cost_collect` /
//! `biz_cost_alloc_result` —— ARCH-9 §3.1 · B2-12）。
//!
//! **本卡不新增迁移**：四块缺口全部读已有表（V0203 出库单、V0371 归集 / 工时、V0372
//! 分摊结果 / 规则），`git status backend/migrations` 必须 0 行。
//!
//! # 只读 + 单个连接
//!
//! 四块取数彼此独立、**互不依赖**（不像 calc 六段有先后），所以这里不开事务：
//! 每条查询各自一致，合起来是「同一秒钟的四个快照」。开事务反而会为一个查询接口
//! 长期占住一个连接（结算高峰期被清单页拖住 = 事故）。
//!
//! # 口径逐条（与 §4.3 归集口径**同源**，改一处必须改两处）
//!
//! | 块 | 口径 |
//! |---|---|
//! | 未挂对象的领料 | `status='POSTED'` + `out_type='ISSUE'` + `cost_object_id IS NULL` + `out_date` 落在期间（与 `cost_collect_model::MATERIAL_SOURCE_SQL` 恰好互补：那边 `IS NOT NULL`） |
//! | 缺工时对象 | 本期要点计算单的对象集合（复用 `cost_period_item_model::objects_of_period`）里，本期**没有** `CONFIRMED` 工时行的那些 |
//! | 缺费率工时 | 复用 `cost_collect_model::labor_source_lines` + `LaborLine::uncollected_reason`（B2-7 的口径，不另写一份 SQL） |
//! | 未摊完的池 | 池（三类费用来源，`cost_object_id IS NULL`）− `biz_cost_alloc_result` 当期已摊，余额 > 0 |

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 出库单表（深链 / 口径注释引用它）。
pub const STOCK_OUT_TABLE: &str = "biz_stock_out";
/// 工时台账表。
pub const LABOR_TABLE: &str = "biz_cost_labor";
/// 分摊结果表。
pub const ALLOC_RESULT_TABLE: &str = "biz_cost_alloc_result";
/// 分摊规则表。
pub const ALLOC_RULE_TABLE: &str = "biz_cost_alloc_rule";

// ==================== ① 未挂成本对象的领料 ====================

/// 一张未挂成本对象的已过账领料单。
#[derive(Debug, Clone, PartialEq)]
pub struct StockOutGapRow {
    /// 出库单主键（跳转来源单据）。
    pub id: Uuid,
    /// 出库单号。
    pub out_no: String,
    /// 出库日期。
    pub out_date: NaiveDate,
    /// 出库类型。
    pub out_type: String,
    /// 出库仓库名。
    pub warehouse_name: Option<String>,
    /// 明细行数。
    pub item_count: i64,
    /// 数量合计。
    pub qty_total: Decimal,
    /// 金额合计（过账时账上真实成本）。
    pub amount_total: Decimal,
}

/// 未挂对象领料单的口径（`LIMIT` 由调用方拼，见 [`stock_out_gap_rows`]）。
///
/// 只多一个 `h.cost_object_id IS NULL`（料归集 SQL 是 `IS NOT NULL`）：两张单子的差别
/// 只有这一处，所以「这张单为什么没进成本」的答案在库里是**可判定**的，不需要猜。
const STOCK_OUT_GAP_WHERE: &str = "\
      FROM biz_stock_out h \
      LEFT JOIN biz_warehouse w ON w.id = h.warehouse_id \
     WHERE h.deleted_at IS NULL \
       AND h.status = 'POSTED' \
       AND h.out_type = 'ISSUE' \
       AND h.cost_object_id IS NULL \
       AND h.out_date >= ?::text::date \
       AND h.out_date <= ?::text::date";

/// ① 汇总：`(张数, 金额合计)` —— 用 `count(*)` 单独算，截断行数不影响条数。
pub async fn stock_out_gap_total(ex: &impl SqlExecutor, start: NaiveDate, end: NaiveDate) -> DbResult<(i64, Decimal)> {
    let rows = ex
        .query_sql(
            format!("SELECT count(*) AS heads, COALESCE(sum(h.total_amount), 0) AS amount {STOCK_OUT_GAP_WHERE}"),
            vec![Value::Text(start.to_string()), Value::Text(end.to_string())],
        )
        .await?;
    match rows.first() {
        None => Ok((0, Decimal::ZERO)),
        Some(row) => Ok((row.get_typed("heads")?, row.get_typed("amount")?)),
    }
}

/// ① 明细行（按出库日期、单号排；`limit` 由调用方给 [`crate::business::erp::cost_uncollected_dto::ROW_LIMIT`]）。
pub async fn stock_out_gap_rows(
    ex: &impl SqlExecutor,
    start: NaiveDate,
    end: NaiveDate,
    limit: usize,
) -> DbResult<Vec<StockOutGapRow>> {
    let sql = format!(
        "SELECT h.id AS id, h.out_no AS out_no, h.out_date AS out_date, h.out_type AS out_type, \
                w.warehouse_name AS warehouse_name, h.total_qty AS qty_total, \
                h.total_amount AS amount_total, \
                (SELECT count(*) FROM biz_stock_out_item i WHERE i.stock_out_id = h.id) AS item_count \
         {STOCK_OUT_GAP_WHERE} \
         ORDER BY h.out_date ASC, h.out_no ASC \
         LIMIT {}",
        i64::try_from(limit).unwrap_or(200)
    );
    let rows = ex
        .query_sql(sql, vec![Value::Text(start.to_string()), Value::Text(end.to_string())])
        .await?;
    rows.iter()
        .map(|row| {
            Ok(StockOutGapRow {
                id: row.get_typed("id")?,
                out_no: row.get_typed("out_no")?,
                out_date: row.get_typed("out_date")?,
                out_type: row.get_typed("out_type")?,
                warehouse_name: row.get_typed("warehouse_name")?,
                item_count: row.get_typed("item_count")?,
                qty_total: row.get_typed("qty_total")?,
                amount_total: row.get_typed("amount_total")?,
            })
        })
        .collect()
}

// ==================== ② 缺工时对象 ====================

/// 一个对象在本期的工时行数（`CONFIRMED` / `DRAFT` 分开）。
#[derive(Debug, Clone, PartialEq)]
pub struct LaborCountRow {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 已确认行数（归集只认这些）。
    pub confirmed_rows: i64,
    /// 草稿行数（已录未确认 ⇒ 文案不同）。
    pub draft_rows: i64,
}

/// ② 当期工时行数（按对象、按状态）。没有工时行的对象**不在**结果里（`None` 即 0 行）。
pub async fn labor_counts_of_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<LaborCountRow>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT l.cost_object_id AS cost_object_id, \
                        count(*) FILTER (WHERE l.status = 'CONFIRMED') AS confirmed_rows, \
                        count(*) FILTER (WHERE l.status = 'DRAFT') AS draft_rows \
                   FROM {LABOR_TABLE} l \
                  WHERE l.deleted_at IS NULL AND l.period_code = ? \
                  GROUP BY l.cost_object_id"
            ),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(LaborCountRow {
                cost_object_id: row.get_typed("cost_object_id")?,
                confirmed_rows: row.get_typed("confirmed_rows")?,
                draft_rows: row.get_typed("draft_rows")?,
            })
        })
        .collect()
}

// ==================== ④ 未摊完的池 ====================

/// 一个 (车间, 科目) 的池 + 已摊 + 余额。
#[derive(Debug, Clone, PartialEq)]
pub struct PoolBucketRow {
    /// 车间。
    pub dept_id: Uuid,
    /// 车间名（`sys_dept` 实时名优先，退化为快照）。
    pub dept_name: String,
    /// 费用科目。
    pub account_code: String,
    /// 科目名（账套里没有该科目时为 `None`）。
    pub account_name: Option<String>,
    /// 池总额。
    pub pool_amount: Decimal,
    /// 已分摊合计（`biz_cost_alloc_result`，本期）。
    pub allocated_amount: Decimal,
    /// 未摊余额 = 池 − 已摊。
    pub remaining_amount: Decimal,
    /// 池里的来源行数。
    pub source_rows: i64,
}

/// 池汇总 SQL（与 `cost_overhead_collect_model::POOL_SQL` 的口径逐条一致，
/// 差异只有两处：多了已摊合计、并按余额 > 0 过滤）。
///
/// **为什么另写一份而不复用**：`POOL_SQL` 是 B2-8 的私有常量（归集段用），
/// 本卡要的是「池 − 已摊」的差；把它改成带 `allocated` 的版本会改变归集段的出参语义，
/// 那是跨卡改口径。两段 SQL 的条件写在同一条注释里，改一处必须同步另一处。
const POOL_BUCKETS_SQL: &str = "\
    WITH pool AS ( \
        SELECT c.dept_id AS dept_id, oh.dept_name AS dept_name, \
               oh.debit_account_code AS account_code, c.amount AS amount \
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
    ), bucket AS ( \
        SELECT pool.dept_id AS dept_id, max(pool.dept_name) AS dept_name, \
               pool.account_code AS account_code, sum(pool.amount) AS pool_amount, \
               count(*) AS source_rows \
          FROM pool \
         WHERE pool.dept_id IS NOT NULL \
         GROUP BY pool.dept_id, pool.account_code \
    ), alloc AS ( \
        SELECT a.dept_id AS dept_id, a.account_code AS account_code, \
               sum(a.alloc_amount) AS allocated_amount \
          FROM biz_cost_alloc_result a \
         WHERE a.period_code = ? \
         GROUP BY a.dept_id, a.account_code \
    ) \
    SELECT b.dept_id AS dept_id, COALESCE(d.dept_name, b.dept_name) AS dept_name, \
           b.account_code AS account_code, ac.account_name AS account_name, \
           b.pool_amount AS pool_amount, COALESCE(al.allocated_amount, 0) AS allocated_amount, \
           b.pool_amount - COALESCE(al.allocated_amount, 0) AS remaining_amount, \
           b.source_rows AS source_rows \
      FROM bucket b \
      LEFT JOIN alloc al ON al.dept_id = b.dept_id AND al.account_code = b.account_code \
      LEFT JOIN sys_dept d ON d.id = b.dept_id \
      LEFT JOIN biz_account ac ON ac.account_code = b.account_code \
     WHERE b.pool_amount - COALESCE(al.allocated_amount, 0) > 0 \
     ORDER BY b.dept_id, b.account_code";

/// ④ 当期池汇总（只回余额 > 0 的桶）。
pub async fn pool_buckets(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<PoolBucketRow>> {
    let rows = ex
        .query_sql(
            POOL_BUCKETS_SQL.to_string(),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(period_code.to_string()),
                Value::Text(period_code.to_string()),
                Value::Text(period_code.to_string()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(PoolBucketRow {
                dept_id: row.get_typed("dept_id")?,
                dept_name: row.get_typed("dept_name")?,
                account_code: row.get_typed("account_code")?,
                account_name: row.get_typed("account_name")?,
                pool_amount: row.get_typed("pool_amount")?,
                allocated_amount: row.get_typed("allocated_amount")?,
                remaining_amount: row.get_typed("remaining_amount")?,
                source_rows: row.get_typed("source_rows")?,
            })
        })
        .collect()
}

/// 期间内有生效分摊规则的 (车间, 科目) 集合。
///
/// 判据与分配段一致：`status = 1` 且生效区间**与期间有交集**
/// （`effective_from <= 期间末 AND (effective_to IS NULL OR effective_to >= 期间首)`）。
/// 规则表很小（每车间每科目一两行），一次全company 取回，再在内存里查。
pub async fn rules_covering(ex: &impl SqlExecutor, start: NaiveDate, end: NaiveDate) -> DbResult<Vec<(Uuid, String)>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT r.source_dept_id AS dept_id, r.account_code AS account_code \
                   FROM {ALLOC_RULE_TABLE} r \
                  WHERE r.deleted_at IS NULL AND r.status = 1 \
                    AND r.effective_from <= ?::text::date \
                    AND (r.effective_to IS NULL OR r.effective_to >= ?::text::date)"
            ),
            vec![Value::Text(end.to_string()), Value::Text(start.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_typed("dept_id")?, row.get_typed("account_code")?)))
        .collect()
}

/// 期间内**有活动**的在产对象所属车间（`OPEN` + 本期有归集行或有工时行）。
///
/// 只判车间级：设计 §6.2.2 的「池无对象」文案是「本期没有在产成本对象」（车间口径），
/// 逐对象判基数还得跑一遍分摊基数计算 —— 那是 calc 第 5 段的事，清单不该重算。
pub async fn active_object_depts(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT DISTINCT o.workshop_dept_id AS dept_id \
               FROM biz_cost_object o \
              WHERE o.deleted_at IS NULL AND o.status = 'OPEN' \
                AND (EXISTS (SELECT 1 FROM biz_cost_collect cc \
                              WHERE cc.cost_object_id = o.id AND cc.period_code = ? \
                                AND cc.deleted_at IS NULL) \
                  OR EXISTS (SELECT 1 FROM biz_cost_labor lb \
                              WHERE lb.cost_object_id = o.id AND lb.period_code = ? \
                                AND lb.deleted_at IS NULL))"
                .to_string(),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(period_code.to_string()),
            ],
        )
        .await?;
    rows.iter().map(|row| row.get_typed("dept_id")).collect()
}

// ==================== 批次 ====================

/// 成本批次摘要（`/cost-period/{id}/uncollected` 的入参解析结果）。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchBriefRow {
    /// 批次主键。
    pub id: Uuid,
    /// 期间。
    pub period_code: String,
    /// 批次号。
    pub batch_no: String,
    /// 状态。
    pub status: String,
}

/// 按批次 id 取（软删的不算；`REVERSED` / `CANCELLED` 也照回 —— 红冲批次仍然能看
/// 当期缺口，只是 `settled` 标记不同）。
pub async fn batch_of_id(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<BatchBriefRow>> {
    let rows = ex
        .query_sql(
            "SELECT p.id AS id, p.period_code AS period_code, p.batch_no AS batch_no, p.status AS status \
               FROM biz_cost_period p \
              WHERE p.id = ?::uuid AND p.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(BatchBriefRow {
            id: row.get_typed("id")?,
            period_code: row.get_typed("period_code")?,
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
        })),
    }
}

// ==================== 单测（SQL 口径自检，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_out_scope_is_the_mirror_of_material_collection() {
        // 与 cost_collect_model::MATERIAL_SOURCE_SQL 恰好互补：那边 IS NOT NULL，这边 IS NULL
        assert!(STOCK_OUT_GAP_WHERE.contains("h.cost_object_id IS NULL"));
        assert!(STOCK_OUT_GAP_WHERE.contains("h.status = 'POSTED'"));
        assert!(STOCK_OUT_GAP_WHERE.contains("h.out_type = 'ISSUE'"));
        assert!(STOCK_OUT_GAP_WHERE.contains("h.deleted_at IS NULL"));
        // 期间按出库日期（§4.3 口径说明 ②），不是创建时间
        assert!(STOCK_OUT_GAP_WHERE.contains("h.out_date >= ?::text::date"));
    }

    #[test]
    fn pool_sql_mirrors_collection_pool_and_subtracts_alloc() {
        // 三类来源缺一不可（费用池的「成池」口径在 B2-8，这里只读）
        assert!(POOL_BUCKETS_SQL.contains("oh.debit_account_code"));
        assert!(POOL_BUCKETS_SQL.contains("dp.expense_account_code"));
        assert!(POOL_BUCKETS_SQL.contains("ei.account_code"));
        assert!(POOL_BUCKETS_SQL.contains("c.cost_object_id IS NULL"));
        // 分摊结果只用来减，不能进池（否则自我循环）
        assert!(POOL_BUCKETS_SQL.contains("sum(a.alloc_amount)"));
        // 只回余额 > 0 的桶
        assert!(POOL_BUCKETS_SQL.contains("> 0"));
    }

    #[test]
    fn pool_sql_placeholder_count_matches_params() {
        // 占位符与参数必须一对一：少一个就是运行期 Parameters(n, n-1) 报错
        let placeholders = POOL_BUCKETS_SQL.matches('?').count();
        assert_eq!(placeholders, 4, "三个来源期间 + 一个分摊结果期间");
    }

    #[test]
    fn labor_count_sql_splits_confirmed_and_draft() {
        let sql = format!(
            "SELECT l.cost_object_id AS cost_object_id, \
                    count(*) FILTER (WHERE l.status = 'CONFIRMED') AS confirmed_rows, \
                    count(*) FILTER (WHERE l.status = 'DRAFT') AS draft_rows \
               FROM {LABOR_TABLE} l WHERE l.deleted_at IS NULL AND l.period_code = ?"
        );
        assert!(sql.contains("'CONFIRMED'"));
        assert!(sql.contains("'DRAFT'"));
        assert_eq!(LABOR_TABLE, "biz_cost_labor");
    }

    #[test]
    fn rule_overlap_window_covers_effective_range() {
        // 区间交集判据：effective_from <= 期末 AND (effective_to IS NULL OR effective_to >= 期初)
        let sql = "r.effective_from <= ?::text::date AND (r.effective_to IS NULL OR r.effective_to >= ?::text::date)";
        assert!(sql.contains("effective_from <="));
        assert!(sql.contains("effective_to IS NULL"));
    }
}

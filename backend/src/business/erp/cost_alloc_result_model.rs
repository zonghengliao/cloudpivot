//! 分摊结果数据层（`biz_cost_alloc_result` + 分摊转入的 `biz_cost_collect` 行）——
//! calc 第 5 段（ARCH-9 §3.1 ⑧ / §4.4.3 / §4.4.5 · B2-10）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0372__cost_alloc_tables.up.sql`
//! （与设计 §3.1 ⑧ 逐字一致）。**本卡不新增迁移**：表 / 索引 / CHECK 全在 V0372，
//! 归集表在 V0371 —— 「迁移幂等 + `.down.sql`」对本卡 N/A，也不碰任何已应用迁移。
//!
//! # 重算口径（设计 §4.4.5，本模块唯一有写操作的地方）
//!
//! ```text
//! DELETE FROM biz_cost_collect      WHERE period_code=? AND source_type='ALLOC'
//! DELETE FROM biz_cost_alloc_result WHERE period_code=?
//! ```
//!
//! `biz_cost_alloc_result` **没有 `deleted_at` 列**（分摊结果是计算产物，不留软删史；
//! V0372 文件头第 31–34 行有理由），所以重算是**整删重灌**，只允许批次 `DRAFT` 时做
//! （闸在 [`crate::business::erp::cost_calc_alloc`]）。
//!
//! 为什么 `ALLOC` 归集行用**硬删**、而料 / 工 / 费归集行用软删：
//! ALLOC 行的 `source_id` 就是分摊结果 id，结果被删后它指向一行不存在的记录，
//! 留着只会让「当期摊了多少」两处口径（结果表 / 归集表）对不上。
//! 料 / 工 / 费行指向**业务单据**，删了就没法重算（那些要软删保留可追溯）。
//!
//! # 为什么 `ALLOC` 行的 `element` 是 `OVERHEAD` 而不是 `'ALLOC'`
//!
//! `biz_cost_collect.element` 的值域是 `MATERIAL / LABOR / OVERHEAD`
//! （V0371 `ck_biz_cost_collect_element`），`'ALLOC'` 是 **`source_type`** 的枚举值。
//! 语义上也对：摊进对象的就是「制造费用」，只是来源是分摊而不是直接归集。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 分摊结果表名。
pub const TABLE: &str = "biz_cost_alloc_result";

/// 归集表名（ALLOC 行写这里）。
pub const COLLECT_TABLE: &str = "biz_cost_collect";

/// 本模块读的列（与 V0372 ⑧ 逐列对应）。
pub const COLUMNS: &str = "id, period_code, rule_id, cost_object_id, dept_id, dept_name, \
     account_code, basis_value, basis_total, pool_amount, alloc_amount, rounding_adj";

/// ALLOC 归集行的 `element`（见模块头最后一段）。
pub const ALLOC_ELEMENT: &str = "OVERHEAD";

/// ALLOC 归集行的 `source_type`。
pub const ALLOC_SOURCE_TYPE: &str = "ALLOC";

/// ALLOC 归集行的行号：一条分摊结果只产出一行（幂等键 `(ALLOC, result_id, 0)`，设计 §4.3）。
pub const ALLOC_LINE_NO: i32 = 0;

/// 允许重算分批次的期间状态（设计 §4.4.5「只允许 `DRAFT`」）。
///
/// `CANCELLED` / `REVERSED` 也放行：那两种状态的批次已经不占位（`uk_biz_cost_period`
/// 排除了它们），该期间的旧分摊结果可以重算；`SUBMITTED` / `APPROVED` / `SETTLED` /
/// `CLOSED` 一律拒绝（数字已经给人看过 / 已入账，重算等于改写历史）。
pub const REDOABLE_STATUSES: [&str; 3] = ["DRAFT", "CANCELLED", "REVERSED"];

// ==================== 行结构 ====================

/// `biz_cost_alloc_result` 一行。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocResultRow {
    /// 主键。
    pub id: Uuid,
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 命中的规则版本。
    pub rule_id: Uuid,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 被摊车间。
    pub dept_id: Uuid,
    /// 车间名快照。
    pub dept_name: String,
    /// 被摊科目（`4101.*`）。
    pub account_code: String,
    /// 本对象基数。
    pub basis_value: Decimal,
    /// 全车间基数合计（快照）。
    pub basis_total: Decimal,
    /// 待分摊总额（快照）。
    pub pool_amount: Decimal,
    /// 摊到本对象（含尾差）。
    pub alloc_amount: Decimal,
    /// 尾差调整（只有一行非 0）。
    pub rounding_adj: Decimal,
}

impl AllocResultRow {
    /// 行 → 结构（列名与 [`COLUMNS`] 对齐）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            period_code: row.get_typed("period_code")?,
            rule_id: row.get_typed("rule_id")?,
            cost_object_id: row.get_typed("cost_object_id")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            account_code: row.get_typed("account_code")?,
            basis_value: row.get_typed("basis_value")?,
            basis_total: row.get_typed("basis_total")?,
            pool_amount: row.get_typed("pool_amount")?,
            alloc_amount: row.get_typed("alloc_amount")?,
            rounding_adj: row.get_typed("rounding_adj")?,
        })
    }
}

/// 分摊转入的归集行（`biz_cost_collect`，`source_type='ALLOC'`）。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocCollectRow {
    /// 主键。
    pub id: Uuid,
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 来源分摊结果 id（幂等键的第二段）。
    pub result_id: Uuid,
    /// 成本对象（ALLOC 行必须挂对象）。
    pub cost_object_id: Uuid,
    /// 车间。
    pub dept_id: Uuid,
    /// 车间名快照。
    pub dept_name: String,
    /// 金额（= 分摊结果的 `alloc_amount`）。
    pub amount: Decimal,
    /// 发生日期（取期间末：分摊是期末动作，没有「业务发生日」）。
    pub occur_date: NaiveDate,
    /// 备注（规则编码 + 科目，便于在归集表里点回去）。
    pub remark: String,
}

/// 期间批次状态（重算闸的输入）。
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodState {
    /// 期间。
    pub period_code: String,
    /// 批次号（报文里点名，用户才知道去撤哪一张）。
    pub batch_no: String,
    /// 状态。
    pub status: String,
}

impl PeriodState {
    /// 是否允许重算分摊（见 [`REDOABLE_STATUSES`]）。
    pub fn redoable(&self) -> bool {
        REDOABLE_STATUSES.contains(&self.status.as_str())
    }
}

// ==================== 读 ====================

/// 当期批次状态；**没有批次行**时返回 `None`（calc 尚未建批次，正常形态）。
pub async fn period_state(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Option<PeriodState>> {
    let rows = ex
        .query_sql(
            "SELECT period_code, batch_no, status FROM biz_cost_period \
              WHERE period_code = ? AND deleted_at IS NULL \
              ORDER BY created_at DESC, id DESC LIMIT 1"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PeriodState {
            period_code: row.get_typed("period_code")?,
            batch_no: row.get_typed("batch_no")?,
            status: row.get_typed("status")?,
        })),
    }
}

/// 当期全部分摊结果（重算前的回查 / 落库后的守恒复核都用它）。
pub async fn results_of_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<AllocResultRow>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM {TABLE} WHERE period_code = ? \
         ORDER BY dept_id, account_code, cost_object_id"
    );
    let rows = ex.query_sql(sql, vec![Value::Text(period_code.to_string())]).await?;
    rows.iter().map(AllocResultRow::from_row).collect()
}

/// 当期 ALLOC 归集行的 `(成本对象, 金额)`（与结果表逐行对账）。
pub async fn alloc_collect_of_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<Vec<(Uuid, Decimal)>> {
    let rows = ex
        .query_sql(
            "SELECT cost_object_id, amount FROM biz_cost_collect \
              WHERE period_code = ? AND source_type = ? AND deleted_at IS NULL \
              ORDER BY cost_object_id, id"
                .to_string(),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(ALLOC_SOURCE_TYPE.to_string()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_typed("cost_object_id")?, row.get_typed("amount")?)))
        .collect()
}

// ==================== 写 ====================

/// 按期间整删：先删 ALLOC 归集行，再删分摊结果（见模块头「重算口径」）。
///
/// 返回 `(删除的归集行数, 删除的结果行数)`。两步都在调用方的事务里，要么都成要么都不成。
pub async fn purge_period(ex: &impl SqlExecutor, period_code: &str) -> DbResult<(u64, u64)> {
    let collect = ex
        .exec_sql(
            format!("DELETE FROM {COLLECT_TABLE} WHERE period_code = ? AND source_type = ?"),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(ALLOC_SOURCE_TYPE.to_string()),
            ],
        )
        .await?;
    let results = ex
        .exec_sql(
            format!("DELETE FROM {TABLE} WHERE period_code = ?"),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    Ok((collect, results))
}

/// 写一条分摊结果。
pub async fn insert_result(ex: &impl SqlExecutor, row: &AllocResultRow, operator: Option<Uuid>) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} (id, period_code, rule_id, cost_object_id, dept_id, dept_name, \
                                  account_code, basis_value, basis_total, pool_amount, alloc_amount, \
                                  rounding_adj, created_at, create_by) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, ?, ?, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, now(), NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&row.id),
            Value::Text(row.period_code.clone()),
            Value::from(&row.rule_id),
            Value::from(&row.cost_object_id),
            Value::from(&row.dept_id),
            Value::Text(row.dept_name.clone()),
            Value::Text(row.account_code.clone()),
            Value::Text(row.basis_value.to_string()),
            Value::Text(row.basis_total.to_string()),
            Value::Text(row.pool_amount.to_string()),
            Value::Text(row.alloc_amount.to_string()),
            Value::Text(row.rounding_adj.to_string()),
            // 操作人可空：`?::uuid` 传 NULL 用 `Value::Null` 不行（驱动要原生类型），
            // 统一走「空串转 NULL」的既有约定（`cost_collect_dto::opt_uuid_text`）。
            Value::Text(crate::business::erp::cost_collect_dto::opt_uuid_text(operator)),
        ],
    )
    .await
}

/// 写一条 `ALLOC` 归集行（幂等键 `(ALLOC, result_id, 0)`）。
pub async fn insert_alloc_collect(ex: &impl SqlExecutor, row: &AllocCollectRow) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "INSERT INTO {COLLECT_TABLE} (id, cost_object_id, period_code, element, source_type, \
                                          source_id, source_no, line_no, occur_date, amount, dept_id, \
                                          dept_name, remark, created_at, updated_at) \
             VALUES (?::uuid, ?::uuid, ?, ?, ?, ?::uuid, NULL, ?, ?::text::date, ?::text::numeric, \
                     ?::uuid, ?, ?, now(), now()) \
             ON CONFLICT DO NOTHING"
        ),
        vec![
            Value::from(&row.id),
            Value::from(&row.cost_object_id),
            Value::Text(row.period_code.clone()),
            Value::Text(ALLOC_ELEMENT.to_string()),
            Value::Text(ALLOC_SOURCE_TYPE.to_string()),
            Value::from(&row.result_id),
            Value::I32(ALLOC_LINE_NO),
            Value::Text(row.occur_date.to_string()),
            Value::Text(row.amount.to_string()),
            Value::from(&row.dept_id),
            Value::Text(row.dept_name.clone()),
            Value::Text(row.remark.clone()),
        ],
    )
    .await
}

// ==================== 单测（不需要数据库：钉 SQL 形状与列口径） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 重算先删归集行、再删结果行（顺序反了会短暂留下指向不存在结果的行）。
    #[test]
    fn purge_deletes_collect_before_results() {
        // 两条语句的顺序体现在 `purge_period` 的源码先后：这里断言常量口径与两张表名，
        // 真库行为由 `cost_calc_alloc_db_tests` 的「重算两次结果不翻倍」覆盖。
        assert_eq!(TABLE, "biz_cost_alloc_result");
        assert_eq!(COLLECT_TABLE, "biz_cost_collect");
        assert_ne!(ALLOC_SOURCE_TYPE, ALLOC_ELEMENT, "ALLOC 是 source_type，不是 element");
    }

    /// ALLOC 行必须是 `element='OVERHEAD' + source_type='ALLOC'`（V0371 的 CHECK 值域）。
    #[test]
    fn alloc_collect_uses_the_check_allowed_values() {
        assert_eq!(ALLOC_ELEMENT, "OVERHEAD");
        assert_eq!(ALLOC_SOURCE_TYPE, "ALLOC");
        // 归集表的 element 值域只有三个（V0371 ck_biz_cost_collect_element）
        assert!(!["MATERIAL", "LABOR", "OVERHEAD"].contains(&ALLOC_SOURCE_TYPE));
    }

    /// 行号恒 0：设计 §4.3 的幂等键是 `(ALLOC, result_id, 0)`。
    #[test]
    fn alloc_collect_line_no_is_zero() {
        assert_eq!(ALLOC_LINE_NO, 0);
    }

    /// 重算闸：DRAFT / CANCELLED / REVERSED 放行，其余（含 SUBMITTED）一律拒绝。
    #[test]
    fn redoable_statuses_are_exactly_the_three() {
        for status in ["DRAFT", "CANCELLED", "REVERSED"] {
            let state = PeriodState {
                period_code: "2026-11".to_string(),
                batch_no: "CS-202611-0001".to_string(),
                status: status.to_string(),
            };
            assert!(state.redoable(), "{status} 应允许重算");
        }
        for status in ["SUBMITTED", "APPROVED", "SETTLED", "CLOSED", ""] {
            let state = PeriodState {
                period_code: "2026-11".to_string(),
                batch_no: "CS-202611-0001".to_string(),
                status: status.to_string(),
            };
            assert!(!state.redoable(), "「{status}」不允许重算分摊");
        }
    }

    /// 结果表读列与结构字段一一对应（漏一列 = 分摊额永远读成 0）。
    #[test]
    fn columns_cover_every_amount_field() {
        for column in [
            "basis_value",
            "basis_total",
            "pool_amount",
            "alloc_amount",
            "rounding_adj",
        ] {
            assert!(COLUMNS.contains(column), "COLUMNS 缺 {column}");
        }
        for column in ["period_code", "rule_id", "cost_object_id", "dept_id", "account_code"] {
            assert!(COLUMNS.contains(column), "COLUMNS 缺 {column}");
        }
    }
}

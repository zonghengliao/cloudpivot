//! 成本计算单导出 · 分摊取数（成本核算 · ARCH-9 §5.2 #32 · COST-B4-1）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0372__cost_alloc_tables.up.sql`
//! （⑧ `biz_cost_alloc_result`）。**本卡不新增迁移**：表、CHECK、唯一键全在 V0372，
//! 迁移幂等 / `.down.sql` 对本卡 N/A。
//!
//! # 为什么导出的「分摊」列要去 `biz_cost_alloc_result` 取，而不是加在 `biz_cost_period_item` 上
//!
//! `biz_cost_period_item`（V0373 ⑩）**没有分摊列**，而且不该有：摊进对象的分摊结果会以
//! `source_type='ALLOC'` 的行落进 `biz_cost_collect.element='OVERHEAD'`，再经
//! calc 第 3 段汇总进 `oh_cur`（`cost_alloc_result_model` 模块头有口径说明）。
//! 也就是说 **`oh_cur` 已经含分摊**，再存一列「分摊」就是同一笔钱两处真相。
//!
//! 所以导出时的「其中：分摊转入」是**只读聚合**：按 `(period_code, cost_object_id)`
//! 把 `biz_cost_alloc_result.alloc_amount` 求和，只用来解释「制造费用里有多少是摊来的」，
//! 不参与任何计算，也不写回。
//!
//! # 与页面的关系
//!
//! 页面（COST-B3-11 步③）只展示 `biz_cost_period_item`；导出件多一列分摊来源解释，
//! 主表的料 / 工 / 费 / 完工 / 在制**逐格等于页面的同一行**（同一份数据源：
//! `cost_period_handler::detail_of`），合计行等于页面表尾的 `items_summary`。
//!
//! # 没有任何软删过滤
//!
//! `biz_cost_alloc_result` 没有 `deleted_at` / `status` 列（V0372 文件头第 31–34 行：
//! 分摊结果是计算产物，重算=按期间整删重灌）。所以这里的 SQL **不能**写
//! `WHERE deleted_at IS NULL` —— 那一列根本不存在，写了就是运行期 500。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 分摊明细全序：对象号 → 科目 → id（没有全序，同一份数据两次导出顺序可能不同）。
pub const ALLOC_ORDER: &str = "ORDER BY c.object_no ASC NULLS LAST, i.account_code ASC, i.id ASC";

/// 一行分摊结果（导出「分摊明细」sheet 的一行）。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocSheetRow {
    /// 成本对象（与计算单行 `cost_object_id` 同值，用来做「其中：分摊转入」的聚合键）。
    pub cost_object_id: Uuid,
    /// 对象号（主数据关联 ⇒ join 实时取名，AGENTS 约定 13）。
    pub object_no: Option<String>,
    /// 被摊车间（结果表里的名字**快照**：车间改名后仍能说明当时摊的是哪个车间）。
    pub dept_name: String,
    /// 命中规则编码（规则被停用 / 软删时可能为空，故 `Option`）。
    pub rule_code: Option<String>,
    /// 命中规则名。
    pub rule_name: Option<String>,
    /// 被摊制造费用科目（`4101.*`）。
    pub account_code: String,
    /// 本对象基数。
    pub basis_value: Decimal,
    /// 全车间基数合计（计算当时快照）。
    pub basis_total: Decimal,
    /// 待分摊总额（计算当时快照）。
    pub pool_amount: Decimal,
    /// 摊到本对象的金额（含尾差）。
    pub alloc_amount: Decimal,
    /// 尾差调整（最大余数法：同一条规则下只有一行非 0）。
    pub rounding_adj: Decimal,
}

impl AllocSheetRow {
    /// 行 → 结构（列名与本模块 SQL 的 SELECT 段逐字对应）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            cost_object_id: row.get_typed("cost_object_id")?,
            object_no: row.get_typed("object_no")?,
            dept_name: row.get_typed("dept_name")?,
            rule_code: row.get_typed("rule_code")?,
            rule_name: row.get_typed("rule_name")?,
            account_code: row.get_typed("account_code")?,
            basis_value: row.get_typed("basis_value")?,
            basis_total: row.get_typed("basis_total")?,
            pool_amount: row.get_typed("pool_amount")?,
            alloc_amount: row.get_typed("alloc_amount")?,
            rounding_adj: row.get_typed("rounding_adj")?,
        })
    }
}

/// 「分摊明细」的取数 SQL（独立成函数：单测直接检查它**没有**按不存在的列过滤）。
pub fn alloc_sql() -> String {
    format!(
        "SELECT i.cost_object_id, c.object_no, i.dept_name, r.rule_code, r.rule_name, \
                i.account_code, i.basis_value, i.basis_total, i.pool_amount, i.alloc_amount, \
                i.rounding_adj \
           FROM biz_cost_alloc_result i \
           LEFT JOIN biz_cost_alloc_rule r ON r.id = i.rule_id \
           LEFT JOIN biz_cost_object c ON c.id = i.cost_object_id \
          WHERE i.period_code = ? {ALLOC_ORDER}"
    )
}

/// 取该期间的全部分摊结果（导出「分摊明细」用）。
///
/// 量级：一行 = 一条规则 × 一个对象 × 一个科目。1000 个对象 × 3 个科目 = 3000 行，
/// 单条 SQL 一次拿全（不做逐对象 N+1）；只有导出时才走这条路，列表 / 详情不受影响。
pub async fn alloc_sheet_rows(period_code: &str) -> DbResult<Vec<AllocSheetRow>> {
    let rows = db()
        .find_with_params(&alloc_sql(), vec![Value::Text(period_code.to_string())])
        .await?;
    rows.iter().map(AllocSheetRow::from_row).collect()
}

// ==================== 单测 ====================
//
// 纯映射（行 → 结构）与排序口径在 `cost_period_sheet` 的 builder 单测里一起覆盖；
// 真库用例见 `docs/evidence/2026-11-11-cost-b4-1-cost-sheet-export/`（1000 行实测）。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_order_is_total() {
        // 全序三段：对象号 → 科目 → id。少一段就可能出现「两次导出顺序不同」
        assert!(ALLOC_ORDER.contains("c.object_no"));
        assert!(ALLOC_ORDER.contains("i.account_code"));
        assert!(ALLOC_ORDER.contains("i.id"));
    }

    #[test]
    fn alloc_sql_does_not_filter_missing_soft_delete_column() {
        // 回归：`biz_cost_alloc_result` 没有 deleted_at / status 列，
        // 任何一处照抄别处 SQL 加上 `deleted_at IS NULL` 都会在运行期 500。
        let sql = alloc_sql();
        assert!(!sql.contains("deleted_at"), "分摊结果表没有 deleted_at 列：{sql}");
        assert!(!sql.contains("status ="), "分摊结果表没有 status 列：{sql}");
        assert!(sql.contains("i.period_code = ?"), "必须按期间过滤");
    }
}

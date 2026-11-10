//! calc 第 5 段「分摊计算」的**纯逻辑**（ARCH-9 §4.4.3 ①②③④ · B2-10）。
//!
//! 本文件不碰库：行结构、守恒断言、未覆盖池的诊断、落库后回查的比对，全是
//! 「输入 → 输出」的纯函数。取数 SQL 见
//! [`cost_alloc_rule_model`](crate::business::erp::cost_alloc_rule_model) /
//! [`cost_overhead_collect_model`](crate::business::erp::cost_overhead_collect_model)，
//! 写库见 [`cost_alloc_result_model`](crate::business::erp::cost_alloc_result_model)，
//! 段编排见 [`cost_calc_alloc`](crate::business::erp::cost_calc_alloc)。
//!
//! # 三条硬断言（本卡验收项）
//!
//! | 断言 | 在哪 | 为什么是硬错误 |
//! |---|---|---|
//! | `Σ alloc_amount == pool_amount`（**逐规则**） | [`ensure_conservation`] | 差一分就说明 4101 永远清不掉（C-COST-1 天天红） |
//! | `rounding_adj ≠ 0` 的行**最多一行** | [`ensure_conservation`] | 两行有尾差 = 尾差被拆开摊，等于各行业务口径不一致 |
//! | 落库后回查 `Σ alloc == 池`（逐 (车间, 科目)） | [`verify_readback`] | 算法对但写库丢行/截断标度，只有回查能发现 |
//!
//! 三条都是 500（`INTERNAL_ERROR`），因为它们的成因只能是**代码/驱动/库**出错，
//! 不是用户填错数据 —— 让人去「修单据」只会白跑一趟。

use std::collections::{HashMap, HashSet};

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_alloc_result_model::AllocResultRow;
use crate::business::erp::cost_alloc_rule_dto::{self as rule_dto, AllocOutcome};
use crate::business::erp::cost_alloc_rule_model::CostAllocRuleRow;
use crate::business::erp::cost_overhead_collect_dto::PoolAmount;
use crate::shared::error_code;

/// 报文里最多列几个「没规则覆盖的池」（超出只计数）。
pub const UNCOVERED_LIMIT: usize = 10;

// ==================== 产出结构 ====================

/// 一个成本对象的一行分摊结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocLineView {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号（回显）。
    pub cost_object_no: String,
    /// 对象状态（`OPEN` / `CLOSED`）。
    pub cost_object_status: String,
    /// 本对象基数。
    pub basis_value: Decimal,
    /// 摊到本对象（含尾差）。
    pub alloc_amount: Decimal,
    /// 尾差调整（只有一行非 0）。
    pub rounding_adj: Decimal,
}

/// 一条规则（= 一个 (车间, 科目) 族当期选中的版本）的分摊结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RuleOutcome {
    /// 规则版本 id。
    pub rule_id: Uuid,
    /// 规则编码。
    pub rule_code: String,
    /// 规则名。
    pub rule_name: String,
    /// 被摊车间。
    pub dept_id: Uuid,
    /// 车间名（实时名优先，落空回落快照）。
    pub dept_name: String,
    /// 被摊科目。
    pub account_code: String,
    /// 分配标准。
    pub basis: String,
    /// 待分摊池（快照）。
    pub pool_amount: Decimal,
    /// 基数合计（快照）。
    pub basis_total: Decimal,
    /// 分摊合计（== `pool_amount`，断言）。
    pub alloc_total: Decimal,
    /// 尾差合计（== `pool_amount − Σ trunc(raw)`）。
    pub rounding_total: Decimal,
    /// 逐对象结果。
    pub lines: Vec<AllocLineView>,
}

impl RuleOutcome {
    /// 报文里的一行摘要（日志 / 汇报用）。
    pub fn summary_line(&self) -> String {
        format!(
            "{} {}/{} 池 {} → 摊 {}（{} 个对象，尾差 {}）",
            self.rule_code,
            self.dept_name,
            self.account_code,
            self.pool_amount,
            self.alloc_total,
            self.lines.len(),
            self.rounding_total
        )
    }
}

/// 池为 0（或当期没有该科目池行）被跳过的规则：**明确行为，不是静默跳过**。
#[derive(Debug, Clone, PartialEq)]
pub struct ZeroPoolRule {
    /// 规则版本 id。
    pub rule_id: Uuid,
    /// 规则编码。
    pub rule_code: String,
    /// 车间名。
    pub dept_name: String,
    /// 科目。
    pub account_code: String,
    /// 人话原因。
    pub reason: String,
}

/// 有池但当期没有任何生效规则覆盖（钱摊不出去）—— 诊断项，不静默吞掉。
#[derive(Debug, Clone, PartialEq)]
pub struct UncoveredPool {
    /// 车间。
    pub dept_id: Uuid,
    /// 车间名。
    pub dept_name: Option<String>,
    /// 科目。
    pub account_code: String,
    /// 池金额。
    pub amount: Decimal,
    /// 人话原因 + 怎么修。
    pub reason: String,
}

/// 第 5 段的整体产出。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocRunOutcome {
    /// 期间。
    pub period_code: String,
    /// 逐规则结果（按 priority → rule_code 顺序）。
    pub rules: Vec<RuleOutcome>,
    /// 池为 0 被跳过的规则。
    pub zero_pool: Vec<ZeroPoolRule>,
    /// 有池但没有规则覆盖。
    pub uncovered: Vec<UncoveredPool>,
    /// 本次分摊的池合计（只算真正摊出去的组）。
    pub total_pool: Decimal,
    /// 本次分摊额合计（== `total_pool`）。
    pub total_alloc: Decimal,
    /// 落库的分摊结果行数。
    pub result_rows: i64,
    /// 落库的 `ALLOC` 归集行数。
    pub collect_rows: i64,
    /// 重算前清掉的旧结果行数（第一次跑为 0）。
    pub purged_rows: i64,
}

impl AllocRunOutcome {
    /// 一句话摘要。
    pub fn summary_line(&self) -> String {
        format!(
            "{} 分摊完成：{} 条规则 / {} 行结果 / 池 {} = 摊 {}（跳过 {} 条无池规则，{} 个池无规则覆盖）",
            self.period_code,
            self.rules.len(),
            self.result_rows,
            self.total_pool,
            self.total_alloc,
            self.zero_pool.len(),
            self.uncovered.len()
        )
    }
}

// ==================== 纯函数 ====================

/// 一条规则的分摊结果 → 段产出的一行（`dto::allocate` 的 `AllocOutcome` 已是最终值）。
pub fn rule_outcome(rule: &CostAllocRuleRow, outcome: &AllocOutcome) -> RuleOutcome {
    let lines = outcome
        .lines
        .iter()
        .map(|line| AllocLineView {
            cost_object_id: line.cost_object_id,
            cost_object_no: line.cost_object_no.clone(),
            cost_object_status: line.cost_object_status.clone(),
            basis_value: line.basis_value,
            alloc_amount: line.alloc_amount,
            rounding_adj: line.rounding_adj,
        })
        .collect::<Vec<_>>();
    let alloc_total = lines
        .iter()
        .map(|line| line.alloc_amount)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    let rounding_total = lines
        .iter()
        .map(|line| line.rounding_adj)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    RuleOutcome {
        rule_id: rule.id,
        rule_code: rule.rule_code.clone(),
        rule_name: rule.rule_name.clone(),
        dept_id: rule.source_dept_id,
        dept_name: rule.display_dept_name(),
        account_code: rule.account_code.clone(),
        basis: rule.basis.clone(),
        pool_amount: outcome.pool_amount,
        basis_total: outcome.basis_total,
        alloc_total,
        rounding_total,
        lines,
    }
}

/// **守恒断言**（§4.4.3 ④）：逐规则 `Σ alloc = pool`，且尾差最多落在一行。
///
/// 失败是 `INTERNAL_ERROR`（见模块头）：`dto::allocate` 已经保证过一遍，这里再验
/// **转换之后**的值（`rule_outcome` 的累加），因为「算法对、累加错」同样会让账永远不平。
pub fn ensure_conservation(group: &RuleOutcome) -> std::result::Result<(), ApiError> {
    let sum = group
        .lines
        .iter()
        .map(|line| line.alloc_amount)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    if sum != group.pool_amount || group.alloc_total != group.pool_amount {
        return Err(conservation_broken(
            &group.rule_code,
            format!(
                "各对象分摊额之和 {} ≠ 待分摊池 {}（{} + 尾差 {}）；池里的钱没摊完，4101 会永远挂着余额",
                sum, group.pool_amount, group.alloc_total, group.rounding_total
            ),
        ));
    }
    let negative = group.lines.iter().find(|line| line.alloc_amount < Decimal::ZERO);
    if let Some(line) = negative {
        return Err(conservation_broken(
            &group.rule_code,
            format!("对象「{}」的分摊额为负（{}）", line.cost_object_no, line.alloc_amount),
        ));
    }
    let adjusted = group
        .lines
        .iter()
        .filter(|line| line.rounding_adj != Decimal::ZERO)
        .count();
    if adjusted > 1 {
        return Err(conservation_broken(
            &group.rule_code,
            format!("尾差落在 {adjusted} 行上；最大余数法只允许一行收尾差（§4.4.3 ③）"),
        ));
    }
    let base = group
        .lines
        .iter()
        .map(|line| line.alloc_amount - line.rounding_adj)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    if (group.pool_amount - base) != group.rounding_total {
        return Err(conservation_broken(
            &group.rule_code,
            format!(
                "尾差合计 {} ≠ 池 − 截断合计 {}（截断合计 {}）",
                group.rounding_total,
                group.pool_amount - base,
                base
            ),
        ));
    }
    Ok(())
}

/// 有池但没规则覆盖的组（`covered` = 当期选中规则覆盖的 (车间, 科目)）。
///
/// 这些钱**摊不出去**：4101 会在结算时留下余额。它不是「静默跳过」—— 进
/// [`AllocRunOutcome::uncovered`] 由编排方/未归集清单（B2-12）展示。
pub fn uncovered_pools(pools: &[PoolAmount], covered: &HashSet<(Uuid, String)>) -> Vec<UncoveredPool> {
    pools
        .iter()
        .filter(|row| !covered.contains(&(row.dept_id, row.account_code.clone())))
        .map(|row| UncoveredPool {
            dept_id: row.dept_id,
            dept_name: row.dept_name.clone(),
            account_code: row.account_code.clone(),
            amount: row.amount,
            reason: format!(
                "该车间 + 科目当期有 {} 待分摊，但没有生效的分摊规则覆盖它；\
                 新增一条 effective_from ≤ 期间末日的规则后重算（否则 4101 清不掉）",
                row.amount
            ),
        })
        .collect()
}

/// 落库后回查：`(车间, 科目) → Σ alloc` 必须与本次要摊的池**逐组相等**。
///
/// `expected` 是本次真正摊出去的组（(车间, 科目, 池金额)）；`results` 是从
/// `biz_cost_alloc_result` 读回来的当期全部行。多出来的组也算错（说明旧结果没清干净）。
pub fn verify_readback(
    period_code: &str,
    expected: &[(Uuid, String, Decimal)],
    results: &[AllocResultRow],
) -> std::result::Result<(), ApiError> {
    let mut actual: HashMap<(Uuid, String), Decimal> = HashMap::new();
    for row in results {
        *actual
            .entry((row.dept_id, row.account_code.clone()))
            .or_insert(Decimal::ZERO) += row.alloc_amount;
    }
    for (dept_id, account_code, pool) in expected {
        let got = actual
            .get(&(*dept_id, account_code.clone()))
            .copied()
            .unwrap_or(Decimal::ZERO);
        if got != *pool {
            return Err(readback_broken(period_code, account_code, *pool, got));
        }
    }
    let expected_keys: HashSet<(Uuid, String)> = expected
        .iter()
        .map(|(dept_id, account_code, _)| (*dept_id, account_code.clone()))
        .collect();
    if let Some((dept_id, account_code)) = actual.keys().find(|key| !expected_keys.contains(*key)) {
        return Err(readback_broken(
            period_code,
            account_code,
            Decimal::ZERO,
            actual[&(*dept_id, account_code.clone())],
        ));
    }
    Ok(())
}

// ==================== 错误构造 ====================

/// 409 + `66066`：该期间成本批次已提交/审批/结算，禁止重算分摊。
pub fn period_locked(period_code: &str, batch_no: &str, status: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_ALLOC_PERIOD_LOCKED,
        format!(
            "期间 {period_code} 的成本批次 {batch_no} 当前状态 {status}，不允许重算分摊。\n\
             分摊结果属于「已给人看过 / 已入账」的数字，重算会改写历史成本。\n\
             修法：提交中的批次先「撤回」、已结算的批次先「红冲」，退回 DRAFT 后再重算；\
             已结算期间的更正请走红冲 + 下期重算，不要直接改数"
        ),
    )
}

/// 500：分摊守恒断言被打破（算法/累加/写库出了问题，不是用户数据问题）。
pub fn conservation_broken(rule_code: &str, detail: String) -> ApiError {
    tracing::error!(rule_code, detail = %detail, "分摊守恒断言失败");
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("分摊守恒断言失败（规则 {rule_code}）：{detail}"),
    )
}

/// 500：落库后回查与本次要摊的池对不上（写库丢行 / 标度被截断 / 旧结果没清干净）。
pub fn readback_broken(period_code: &str, account_code: &str, pool: Decimal, got: Decimal) -> ApiError {
    tracing::error!(
        period_code = period_code,
        account_code = account_code,
        pool = %pool,
        got = %got,
        "分摊落库后回查不一致"
    );
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!(
            "期间 {period_code} 科目 {account_code} 的分摊结果落库后回查不一致：\
             待分摊 {pool}，库里合计 {got}"
        ),
    )
}

/// 500：结果行数与 `ALLOC` 归集行数不等（一行结果必须一行归集，设计 §4.3 幂等键）。
pub fn readback_rows_broken(period_code: &str, results: i64, collects: i64) -> ApiError {
    tracing::error!(period_code, results, collects, "分摊结果行与 ALLOC 归集行数不等");
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!(
            "期间 {period_code} 的分摊结果有 {results} 行，但只写进 {collects} 行 ALLOC 归集；\
             计算单按归集行汇总会少算成本"
        ),
    )
}

/// 池为 0 的跳过原因（统一文案，便于前端和日志对齐）。
pub fn zero_pool_reason(period_code: &str) -> String {
    format!(
        "期间 {period_code} 该车间 + 科目没有待分摊金额（池 = 0），本轮不产生分摊结果；\
         有金额时重算即可"
    )
}

/// 4 位小数字符串（出参固定标度，避免 `2400.0000` 打成 `2400` 让前端字面比较抖）。
pub fn money4(value: Decimal) -> String {
    rule_dto::money4(value)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    fn line(no: &str, basis: &str, alloc: &str, adjust: &str) -> AllocLineView {
        AllocLineView {
            cost_object_id: Uuid::from_u128(no.len() as u128 + basis.len() as u128),
            cost_object_no: no.to_string(),
            cost_object_status: "OPEN".to_string(),
            basis_value: dec(basis),
            alloc_amount: dec(alloc),
            rounding_adj: dec(adjust),
        }
    }

    fn group(pool: &str, lines: Vec<AllocLineView>) -> RuleOutcome {
        let alloc_total = lines
            .iter()
            .map(|line| line.alloc_amount)
            .fold(Decimal::ZERO, |acc, value| acc + value);
        let rounding_total = lines
            .iter()
            .map(|line| line.rounding_adj)
            .fold(Decimal::ZERO, |acc, value| acc + value);
        RuleOutcome {
            rule_id: Uuid::from_u128(1),
            rule_code: "WL-001".to_string(),
            rule_name: "一车间水电按工时分摊".to_string(),
            dept_id: Uuid::from_u128(2),
            dept_name: "一车间".to_string(),
            account_code: "4101.02".to_string(),
            basis: "LABOR_HOURS".to_string(),
            pool_amount: dec(pool),
            basis_total: Decimal::ZERO,
            alloc_total,
            rounding_total,
            lines,
        }
    }

    // ---------- 守恒断言 ----------

    /// 设计算例 B1：池 4000，基数 60/20/20（合计 100）→ 2400/800/800，整除无尾差。
    #[test]
    fn exact_division_is_conserved() {
        let outcome = group(
            "4000",
            vec![
                line("MO-1", "60", "2400.0000", "0"),
                line("MO-2", "20", "800.0000", "0"),
                line("MO-3", "20", "800.0000", "0"),
            ],
        );
        assert!(ensure_conservation(&outcome).is_ok());
        assert_eq!(outcome.alloc_total, dec("4000"));
        assert_eq!(outcome.rounding_total, Decimal::ZERO);
    }

    /// 设计算例 B2：池 1000、基数 1:1:1 → 333.3334 / 333.3333 / 333.3333，尾差只落一行。
    #[test]
    fn remainder_lands_on_exactly_one_row() {
        let outcome = group(
            "1000",
            vec![
                line("MO-1", "1", "333.3334", "0.0001"),
                line("MO-2", "1", "333.3333", "0"),
                line("MO-3", "1", "333.3333", "0"),
            ],
        );
        assert!(ensure_conservation(&outcome).is_ok());
        assert_eq!(outcome.alloc_total, dec("1000"));
        assert_eq!(outcome.rounding_total, dec("0.0001"));
    }

    /// 少摊一分：断言必须拦（这是「4101 永远清不掉」的根因）。
    #[test]
    fn missing_cent_is_rejected() {
        let outcome = group(
            "1000",
            vec![
                line("MO-1", "1", "333.3300", "0"),
                line("MO-2", "1", "333.3300", "0"),
                line("MO-3", "1", "333.3300", "0"),
            ],
        );
        let error = ensure_conservation(&outcome).expect_err("少摊必须在段内被拦");
        assert_eq!(error.status, 500);
        assert!(error.msg.contains("≠ 待分摊池"), "{}", error.msg);
    }

    /// 尾差落在两行：断言必须拦（说明不是最大余数法）。
    #[test]
    fn split_remainder_is_rejected() {
        let outcome = group(
            "1000",
            vec![
                line("MO-1", "1", "333.3334", "0.0001"),
                line("MO-2", "1", "333.3334", "0.0001"),
                line("MO-3", "1", "333.3332", "-0.0002"),
            ],
        );
        // 合计仍是 1000，但尾差落在 3 行（含负数）
        let error = ensure_conservation(&outcome).expect_err("尾差散落必须被拦");
        assert!(error.msg.contains("尾差落在 3 行"), "{}", error.msg);
    }

    /// 负分摊：拦（负数分摊 = 把成本摊成收入）。
    #[test]
    fn negative_alloc_is_rejected() {
        let outcome = group(
            "0.0001",
            vec![line("MO-1", "1", "-1.0000", "0"), line("MO-2", "1", "1.0001", "0")],
        );
        let error = ensure_conservation(&outcome).expect_err("负分摊必须被拦");
        assert!(error.msg.contains("分摊额为负"), "{}", error.msg);
    }

    /// 尾差合计与「池 − 截断合计」不符：拦（说明 rounding_adj 被谁改过）。
    #[test]
    fn rounding_total_must_match_shortfall() {
        let mut outcome = group(
            "1000",
            vec![
                line("MO-1", "1", "333.3334", "0.0001"),
                line("MO-2", "1", "333.3333", "0"),
                line("MO-3", "1", "333.3333", "0"),
            ],
        );
        outcome.rounding_total = dec("0.0002");
        let error = ensure_conservation(&outcome).expect_err("尾差合计对不上必须被拦");
        assert!(error.msg.contains("尾差合计"), "{}", error.msg);
    }

    // ---------- 未覆盖池 ----------

    #[test]
    fn uncovered_pool_is_reported_not_swallowed() {
        let dept = Uuid::from_u128(9);
        let pools = vec![
            PoolAmount {
                dept_id: dept,
                dept_name: Some("一车间".to_string()),
                account_code: "4101.02".to_string(),
                amount: dec("1000"),
                rows: 2,
            },
            PoolAmount {
                dept_id: dept,
                dept_name: Some("一车间".to_string()),
                account_code: "4101.03".to_string(),
                amount: dec("500"),
                rows: 1,
            },
        ];
        let mut covered = HashSet::new();
        covered.insert((dept, "4101.02".to_string()));
        let uncovered = uncovered_pools(&pools, &covered);
        assert_eq!(uncovered.len(), 1);
        assert_eq!(uncovered[0].account_code, "4101.03");
        assert_eq!(uncovered[0].amount, dec("500"));
        assert!(uncovered[0].reason.contains("没有生效的分摊规则覆盖"));
    }

    #[test]
    fn covered_pool_produces_no_diagnostic() {
        let dept = Uuid::from_u128(9);
        let pools = vec![PoolAmount {
            dept_id: dept,
            dept_name: None,
            account_code: "4101.02".to_string(),
            amount: dec("1000"),
            rows: 1,
        }];
        let mut covered = HashSet::new();
        covered.insert((dept, "4101.02".to_string()));
        assert!(uncovered_pools(&pools, &covered).is_empty());
    }

    // ---------- 回查 ----------

    fn result_row(dept: Uuid, account: &str, alloc: &str) -> AllocResultRow {
        AllocResultRow {
            id: Uuid::from_u128(alloc.len() as u128),
            period_code: "2026-11".to_string(),
            rule_id: Uuid::from_u128(1),
            cost_object_id: Uuid::from_u128(2),
            dept_id: dept,
            dept_name: "一车间".to_string(),
            account_code: account.to_string(),
            basis_value: dec("1"),
            basis_total: dec("3"),
            pool_amount: dec(alloc),
            alloc_amount: dec(alloc),
            rounding_adj: Decimal::ZERO,
        }
    }

    #[test]
    fn readback_matches_when_sum_equals_pool() {
        let dept = Uuid::from_u128(9);
        let results = vec![result_row(dept, "4101.02", "600"), result_row(dept, "4101.02", "400")];
        let expected = vec![(dept, "4101.02".to_string(), dec("1000"))];
        assert!(verify_readback("2026-11", &expected, &results).is_ok());
    }

    #[test]
    fn readback_catches_short_write() {
        let dept = Uuid::from_u128(9);
        let results = vec![result_row(dept, "4101.02", "600")];
        let expected = vec![(dept, "4101.02".to_string(), dec("1000"))];
        let error = verify_readback("2026-11", &expected, &results).expect_err("少写一行必须被回查拦下");
        assert!(error.msg.contains("回查不一致"), "{}", error.msg);
    }

    #[test]
    fn readback_catches_stale_rows() {
        let dept = Uuid::from_u128(9);
        let results = vec![
            result_row(dept, "4101.02", "1000"),
            result_row(dept, "4101.09", "0.0001"),
        ];
        let expected = vec![(dept, "4101.02".to_string(), dec("1000"))];
        let error = verify_readback("2026-11", &expected, &results).expect_err("旧结果没清干净必须被拦");
        assert!(error.msg.contains("4101.09"), "{}", error.msg);
    }

    // ---------- 名字 / 报文 ----------

    #[test]
    fn locked_period_message_points_to_the_fix() {
        let error = period_locked("2026-11", "CS-202611-0001", "SUBMITTED");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_ALLOC_PERIOD_LOCKED);
        assert!(error.msg.contains("CS-202611-0001"), "{}", error.msg);
        assert!(error.msg.contains("撤回"), "{}", error.msg);
        assert!(error.msg.contains("红冲"), "{}", error.msg);
    }

    #[test]
    fn zero_pool_reason_names_the_period() {
        assert!(zero_pool_reason("2026-11").contains("2026-11"));
    }

    #[test]
    fn money4_keeps_trailing_zeros() {
        assert_eq!(money4(dec("2400")), "2400.0000");
        assert_eq!(money4(dec("0.0001")), "0.0001");
    }
}

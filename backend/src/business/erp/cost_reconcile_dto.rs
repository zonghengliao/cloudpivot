//! calc 第 4 段「费用池与 `4101.*` 对账」的**纯逻辑**（ARCH-9 §4.11 C-COST-1 · B2-9）。
//!
//! ```text
//! 池侧（biz_cost_collect 池行，科目从来源单据反查）  ┐
//!                                                   ├→ 按 (车间, 科目) 分组 → 逐组求差 → 不平就 409 列凭证号
//! 账面侧（biz_voucher_entry 4101.* 借方，带凭证号）  ┘
//! ```
//!
//! 本文件不碰库：分组 / 求差 / 报文全是「输入 → 输出」的纯函数（真库用例在
//! `cost_calc_reconcile_db_tests.rs`）。取数 SQL 见
//! [`crate::business::erp::cost_reconcile_model`]，段编排见
//! [`crate::business::erp::cost_calc_reconcile`]。
//!
//! # 为什么按 (车间, 科目) 分组而不是只比总额
//!
//! 总额相等是**假绿**最常见的来源：A 车间多摊 100、B 车间少摊 100，总额分毫不差。
//! C-COST-1 的判据写的就是「按 (车间, 科目) 分组」——逐组平才算平（[`reconcile`] 的
//! `mismatched_groups` 按组计数，不看总额）。
//!
//! # 两个阶段（本卡的**关键口径**，不是设计走样）
//!
//! | 来源 | 4101 借方什么时候入账 | `Stage::Calc` 是否要求账面背书 |
//! |---|---|---|
//! | 折旧 `DEPRECIATION` | 计提过账时（`biz_depreciation.status='POSTED'` 同刻） | **是** |
//! | 报销 `EXPENSE` | 审批通过时（`expense_bridge::execute_approval`） | **是** |
//! | 手工台账 `OVERHEAD_TICKET` | **结算时**才生成「借 4101 / 贷 对方科目」（设计 §4.8 ④） | **否**（算「待过账」） |
//!
//! 所以 calc 时刻三路池里必然有一段**账上还没有**的手工台账金额。把它当差错会让每一张
//! 有手工台账的期间都红；把它当「不存在」又会让 C-COST-1 在结算时失去意义。处理办法是把
//! 它**显式命名**为 `pending_amount`（待结算过账），只在 `Stage::Settle`（B3-6 事务内自检，
//! 那时第四张凭证已过账）才要求它也有账面背书。见 [`Stage::is_vouchered`]。

use std::collections::BTreeMap;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_overhead_collect_dto::{SOURCE_DEPRECIATION, SOURCE_EXPENSE};
use crate::shared::error_code;

/// 报文里最多列几个差额分组（超出只计数）：400 行报文没人看，也截断日志。
pub const DIFF_DETAIL_LIMIT: usize = 10;

// ==================== 阶段 ====================

/// 对账阶段：决定「哪些池行的 4101 借方**此刻**应当已经入账」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// calc（本卡）：折旧 / 报销在来源过账时就已生成凭证；手工台账的凭证在**结算**时才生成。
    Calc,
    /// settle（B3-6）：四张凭证已在同一事务内过账，三路池行都必须有账面背书。
    Settle,
}

impl Stage {
    /// 报文里的口径名（用户看得懂的两个词）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Calc => "计算",
            Stage::Settle => "结算",
        }
    }

    /// 该来源的池金额在本阶段是否应当已经躺在 `4101.*` 借方。
    ///
    /// `Calc` 只认折旧与报销（凭证随来源单据过账生成）；`Settle` 认全部来源
    /// （手工台账的第四张凭证已过账）。未知来源在 `Settle` 下一律要求背书 —— 宁可报错，
    /// 也不能让一个没人认识的来源类型悄悄绕过对账。
    pub fn is_vouchered(&self, source_type: &str) -> bool {
        match self {
            Stage::Settle => true,
            Stage::Calc => matches!(source_type, SOURCE_DEPRECIATION | SOURCE_EXPENSE),
        }
    }
}

// ==================== 两侧输入行 ====================

/// 池侧一行：**已按 (车间, 科目, 来源类型) 汇总**的池金额。
///
/// 带 `source_type` 是因为 [`Stage`] 要按来源判断「此刻该不该有凭证」；池本身的
/// 公开口径（供分摊用）仍是 `cost_overhead_collect_model::pool_of_period` 的 (车间, 科目)。
#[derive(Debug, Clone)]
pub struct PoolSideRow {
    /// 车间（池行必有车间：费归集段已把「没车间」判为账面非法）。
    pub dept_id: Uuid,
    /// 车间名（`sys_dept` 实时名优先，落空回落单据快照）。
    pub dept_name: Option<String>,
    /// 制造费用科目（`4101.*`，从来源单据反查）。
    pub account_code: String,
    /// 来源类型（`DEPRECIATION` / `EXPENSE` / `OVERHEAD_TICKET`）。
    pub source_type: String,
    /// 汇总金额。
    pub amount: Decimal,
    /// 汇入该行的池行笔数。
    pub rows: i64,
}

/// 账面侧一行：`4101.*` 的**一条借方分录**（不汇总 —— 要留凭证号）。
#[derive(Debug, Clone)]
pub struct LedgerSideRow {
    /// 部门辅助核算（凭证分录 `aux_dept_id`）。为空 = 这笔 4101 没挂车间。
    pub dept_id: Option<Uuid>,
    /// 部门名（实时名，落空为空）。
    pub dept_name: Option<String>,
    /// 科目编码（`4101.*` 末级）。
    pub account_code: String,
    /// 凭证号（`biz_voucher.voucher_no`）—— 差额要能点到凭证。
    pub voucher_no: String,
    /// 借方金额（> 0）。
    pub debit_amount: Decimal,
}

// ==================== 分组与结论 ====================

/// 一组 (车间, 科目) 的对账结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    /// 平：本阶段应当入账的池金额 = 账面借方发生额。
    Balanced,
    /// 只有「待结算过账」的手工台账（`Calc` 阶段的正常形态）。
    Pending,
    /// 账面有、池里没有 ⇒ 有人绕过成本模块记了 4101。
    LedgerOnly,
    /// 池里有、账面没有 ⇒ 费用已在成本模块归集，但 4101 凭证没生成。
    PoolOnly,
    /// 两边都有但金额不等。
    Mismatch,
}

impl GroupKind {
    /// 报文里的短标签。
    pub fn as_str(&self) -> &'static str {
        match self {
            GroupKind::Balanced => "平",
            GroupKind::Pending => "待结算过账",
            GroupKind::LedgerOnly => "账面多（旁路记账）",
            GroupKind::PoolOnly => "账面少（凭证缺失）",
            GroupKind::Mismatch => "金额不符",
        }
    }
}

/// 一组 (车间, 科目) 的对账明细（报文与接口都用它）。
#[derive(Debug, Clone)]
pub struct ReconcileGroup {
    /// 车间；`None` = 该组只出现在账面侧且凭证**没挂部门**。
    pub dept_id: Option<Uuid>,
    /// 车间名。
    pub dept_name: Option<String>,
    /// 科目编码。
    pub account_code: String,
    /// 池金额（含待结算过账的手工台账）。
    pub pool_amount: Decimal,
    /// 其中本阶段**应当已入账**的部分。
    pub expected_amount: Decimal,
    /// 其中「待结算过账」的部分（`Calc` 阶段 = 手工台账池行）。
    pub pending_amount: Decimal,
    /// 账面 `4101.*` 借方发生额。
    pub ledger_amount: Decimal,
    /// 差额 = `expected_amount - ledger_amount`（正数 = 账上少了）。
    pub diff: Decimal,
    /// 该组账面侧涉及的凭证号（去重、按凭证号升序）。
    pub voucher_nos: Vec<String>,
    /// 该组的池行笔数。
    pub pool_rows: i64,
    /// 结论。
    pub kind: GroupKind,
}

impl ReconcileGroup {
    /// 该组是否对平（判据只有差额，`kind` 只是给人看的解释）。
    pub fn is_balanced(&self) -> bool {
        self.diff.is_zero()
    }

    /// 「机加工车间 / 4101.01」这种一眼能定位的标签。
    pub fn label(&self) -> String {
        let dept = match (&self.dept_name, self.dept_id) {
            (Some(name), _) if !name.trim().is_empty() => name.trim().to_string(),
            (_, Some(id)) => id.to_string(),
            _ => "（无车间）".to_string(),
        };
        format!("{dept} / {}", self.account_code)
    }
}

/// 对账结论（段函数的返回值）。
#[derive(Debug, Clone)]
pub struct ReconcileOutcome {
    /// 期间。
    pub period_code: String,
    /// 阶段。
    pub stage: Stage,
    /// 全部 (车间, 科目) 组（按车间名 + 科目升序）。
    pub groups: Vec<ReconcileGroup>,
    /// 池金额合计（含待结算过账）。
    pub pool_amount: Decimal,
    /// 本阶段应已入账的池金额合计。
    pub expected_amount: Decimal,
    /// 待结算过账金额合计。
    pub pending_amount: Decimal,
    /// 账面 `4101.*` 借方发生额合计。
    pub ledger_amount: Decimal,
    /// 差额合计 = `expected_amount - ledger_amount`。
    pub diff_amount: Decimal,
    /// 不平的组数（**判据**：逐组比，不看总额）。
    pub mismatched_groups: usize,
}

impl ReconcileOutcome {
    /// 是否对平（逐组全平才算平）。
    pub fn balanced(&self) -> bool {
        self.mismatched_groups == 0
    }

    /// 不平的组（报文用）。
    pub fn differing(&self) -> impl Iterator<Item = &ReconcileGroup> {
        self.groups.iter().filter(|group| !group.is_balanced())
    }

    /// 全部差额凭证号（去重保序）。
    pub fn difference_vouchers(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for group in self.differing() {
            for no in &group.voucher_nos {
                if !out.iter().any(|existing| existing == no) {
                    out.push(no.clone());
                }
            }
        }
        out
    }

    /// 日志 / 汇报用的一行摘要。
    pub fn summary_line(&self) -> String {
        format!(
            "期间 {} {}口径：池 {}（其中待结算过账 {}），应已入账 {}，账面 {}，差额 {}，不平 {} 组",
            self.period_code,
            self.stage.as_str(),
            money(self.pool_amount),
            money(self.pending_amount),
            money(self.expected_amount),
            money(self.ledger_amount),
            money(self.diff_amount),
            self.mismatched_groups
        )
    }
}

// ==================== 纯函数：对账 ====================

/// 累加器（分组用的中间结构）。
#[derive(Default)]
struct Acc {
    dept_name: Option<String>,
    pool_amount: Decimal,
    expected_amount: Decimal,
    pending_amount: Decimal,
    pool_rows: i64,
    ledger_amount: Decimal,
    voucher_nos: Vec<String>,
}

/// 池侧 + 账面侧 → 逐组对账结论（**纯函数**，无库无时间）。
///
/// 分组键是 `(车间, 科目)`：池侧车间必有值，账面侧可能为空（凭证没挂部门），
/// 空车间的账面分录单独成组 —— 那正是「绕过成本模块记账」最典型的形态。
pub fn reconcile(period_code: &str, stage: Stage, pool: &[PoolSideRow], ledger: &[LedgerSideRow]) -> ReconcileOutcome {
    let mut map: BTreeMap<(Option<Uuid>, String), Acc> = BTreeMap::new();

    for row in pool {
        let acc = map.entry((Some(row.dept_id), row.account_code.clone())).or_default();
        acc.dept_name = acc.dept_name.clone().or_else(|| row.dept_name.clone());
        acc.pool_amount += row.amount;
        acc.pool_rows += row.rows;
        if stage.is_vouchered(&row.source_type) {
            acc.expected_amount += row.amount;
        } else {
            acc.pending_amount += row.amount;
        }
    }

    for row in ledger {
        let acc = map.entry((row.dept_id, row.account_code.clone())).or_default();
        // 账面侧有车间名时优先保留（池侧没有该组，名字只能从这里来）
        if acc.dept_name.is_none() {
            acc.dept_name = row.dept_name.clone();
        }
        acc.ledger_amount += row.debit_amount;
        if !acc.voucher_nos.iter().any(|no| no == &row.voucher_no) {
            acc.voucher_nos.push(row.voucher_no.clone());
        }
    }

    let mut groups: Vec<ReconcileGroup> = map
        .into_iter()
        .map(|((dept_id, account_code), acc)| {
            let diff = acc.expected_amount - acc.ledger_amount;
            let kind = classify(&acc, diff);
            let mut voucher_nos = acc.voucher_nos;
            voucher_nos.sort();
            ReconcileGroup {
                dept_id,
                dept_name: acc.dept_name,
                account_code,
                pool_amount: acc.pool_amount,
                expected_amount: acc.expected_amount,
                pending_amount: acc.pending_amount,
                ledger_amount: acc.ledger_amount,
                diff,
                voucher_nos,
                pool_rows: acc.pool_rows,
                kind,
            }
        })
        .collect();

    // 车间名 → 科目 → 车间 id（名字缺失时用 id 兜底排序，保证输出稳定可断言）
    groups.sort_by(|left, right| {
        let key = |group: &ReconcileGroup| {
            (
                group.dept_name.clone().unwrap_or_default(),
                group.account_code.clone(),
                group.dept_id.map(|id| id.to_string()).unwrap_or_default(),
            )
        };
        key(left).cmp(&key(right))
    });

    let pool_amount = groups.iter().map(|group| group.pool_amount).sum();
    let expected_amount = groups.iter().map(|group| group.expected_amount).sum();
    let pending_amount = groups.iter().map(|group| group.pending_amount).sum();
    let ledger_amount = groups.iter().map(|group| group.ledger_amount).sum();
    let diff_amount = groups.iter().map(|group| group.diff).sum();
    let mismatched_groups = groups.iter().filter(|group| !group.is_balanced()).count();

    ReconcileOutcome {
        period_code: period_code.to_string(),
        stage,
        groups,
        pool_amount,
        expected_amount,
        pending_amount,
        ledger_amount,
        diff_amount,
        mismatched_groups,
    }
}

/// 单组定性（**只看差额**判平，其余是给人看的解释）。
fn classify(acc: &Acc, diff: Decimal) -> GroupKind {
    if diff.is_zero() {
        return if acc.pending_amount.is_zero() {
            GroupKind::Balanced
        } else {
            GroupKind::Pending
        };
    }
    match (acc.expected_amount.is_zero(), acc.ledger_amount.is_zero()) {
        // 账面有、应入账为 0 ⇒ 旁路记账
        (true, false) => GroupKind::LedgerOnly,
        // 应入账有、账面为 0 ⇒ 凭证缺失
        (false, true) => GroupKind::PoolOnly,
        // 两边都有但不等；或两边都 0 却出现非零差额（浮点/脏数据兜底，按金额不符报）
        _ => GroupKind::Mismatch,
    }
}

// ==================== 错误构造 ====================

/// 409 + `66065`：费用池与 `4101.*` 借方发生额对不平（C-COST-1）。
///
/// 报文必须回答三个问题：**差多少**（总额 + 逐组）、**差在哪几张凭证**（凭证号）、
/// **怎么修**（改走费用台账 / 报销，或把该笔改记别的科目）。只说「对不平」等于把
/// 排查成本丢给财务。
pub fn mismatch(outcome: &ReconcileOutcome) -> ApiError {
    let mut lines = Vec::new();
    let mut shown = 0usize;
    for group in outcome.differing() {
        if shown >= DIFF_DETAIL_LIMIT {
            break;
        }
        shown += 1;
        let vouchers = if group.voucher_nos.is_empty() {
            "账面没有对应凭证".to_string()
        } else {
            format!("凭证：{}", group.voucher_nos.join("、"))
        };
        lines.push(format!(
            "· {}：池 {}（应已入账 {}，待结算过账 {}），账面 {}，差额 {}｜{}｜{}",
            group.label(),
            money(group.pool_amount),
            money(group.expected_amount),
            money(group.pending_amount),
            money(group.ledger_amount),
            money(group.diff),
            vouchers,
            group.kind.as_str()
        ));
    }
    let rest = outcome.mismatched_groups.saturating_sub(shown);
    if rest > 0 {
        lines.push(format!("· 另有 {rest} 组不平（未逐条列出）"));
    }

    ApiError::new(
        409,
        error_code::ERP_COST_POOL_LEDGER_MISMATCH,
        format!(
            "费用池与总账 4101.* 借方发生额对不平（期间 {}，{}口径）：池 {}，应已入账 {}，账面 {}，\
             差额 {}（= 应已入账 − 账面，负数表示账面多），共 {} 组不平。\n{}\n\
             账面多出来的 4101.* 分录是绕过成本模块直接记的制造费用，成本核算看不见它们，\
             结算后 4101 清不掉。请把这类费用改走「费用台账（无单据）」或「费用报销」后重算本期成本；\
             若该笔本来就不是制造费用，请改记对应费用科目。\
             账面少的那部分说明费用已在成本模块归集、4101 凭证却没生成，请检查来源单据是否已过账。",
            outcome.period_code,
            outcome.stage.as_str(),
            money(outcome.pool_amount),
            money(outcome.expected_amount),
            money(outcome.ledger_amount),
            money(outcome.diff_amount),
            outcome.mismatched_groups,
            lines.join("\n")
        ),
    )
}

/// 平 ⇒ `Ok(())`；不平 ⇒ 409 + `66065`（一次性把定性与报文都给出）。
pub fn ensure_balanced(outcome: &ReconcileOutcome) -> std::result::Result<(), ApiError> {
    if outcome.balanced() {
        Ok(())
    } else {
        Err(mismatch(outcome))
    }
}

/// 金额展示：去掉无意义的尾零（`1000.0000` → `1000`，`0.0001` 保持）。
fn money(value: Decimal) -> String {
    let normalized = value.normalize();
    if normalized.is_zero() {
        "0".to_string()
    } else {
        normalized.to_string()
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost_overhead_collect_dto::{ACCOUNT_PREFIX, SOURCE_OVERHEAD_TICKET};
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    fn pool(dept: Uuid, account: &str, source: &str, amount: &str) -> PoolSideRow {
        PoolSideRow {
            dept_id: dept,
            dept_name: Some("机加工车间".to_string()),
            account_code: account.to_string(),
            source_type: source.to_string(),
            amount: dec(amount),
            rows: 1,
        }
    }

    fn ledger(dept: Option<Uuid>, account: &str, voucher_no: &str, amount: &str) -> LedgerSideRow {
        LedgerSideRow {
            dept_id: dept,
            dept_name: dept.map(|_| "机加工车间".to_string()),
            account_code: account.to_string(),
            voucher_no: voucher_no.to_string(),
            debit_amount: dec(amount),
        }
    }

    /// 空期间（没有池也没有账面）：平、0 组、不报错。
    #[test]
    fn empty_period_is_balanced() {
        let outcome = reconcile("2026-11", Stage::Calc, &[], &[]);
        assert!(outcome.balanced());
        assert_eq!(outcome.groups.len(), 0);
        assert_eq!(outcome.diff_amount, Decimal::ZERO);
        assert!(outcome.difference_vouchers().is_empty());
    }

    /// 池与账面逐组相等 ⇒ 平；手工台账在 Calc 阶段算「待结算过账」而不是差额。
    #[test]
    fn pool_matching_ledger_is_balanced_and_manual_ticket_is_pending() {
        let dept = Uuid::now_v7();
        let pool = vec![
            pool(dept, "4101.01", SOURCE_DEPRECIATION, "1000"),
            pool(dept, "4101.02", SOURCE_EXPENSE, "300"),
            pool(dept, "4101.03", SOURCE_OVERHEAD_TICKET, "150"),
        ];
        let ledger = vec![
            ledger(Some(dept), "4101.01", "记-2026-11-0001", "1000"),
            ledger(Some(dept), "4101.02", "记-2026-11-0002", "300"),
        ];
        let outcome = reconcile("2026-11", Stage::Calc, &pool, &ledger);
        assert!(outcome.balanced(), "{}", outcome.summary_line());
        assert_eq!(outcome.pool_amount, dec("1450"));
        assert_eq!(outcome.expected_amount, dec("1300"));
        assert_eq!(outcome.pending_amount, dec("150"));
        assert_eq!(outcome.ledger_amount, dec("1300"));
        assert_eq!(outcome.diff_amount, Decimal::ZERO);
        let manual = outcome
            .groups
            .iter()
            .find(|group| group.account_code == "4101.03")
            .expect("手工台账那一组要在");
        assert_eq!(manual.kind, GroupKind::Pending);
        assert_eq!(manual.pending_amount, dec("150"));
    }

    /// 同一批数据换到结算阶段：手工台账的第四张凭证还没过账 ⇒ 不平（PoolOnly）。
    #[test]
    fn manual_ticket_becomes_a_blocker_at_settle_stage() {
        let dept = Uuid::now_v7();
        let pool = vec![pool(dept, "4101.03", SOURCE_OVERHEAD_TICKET, "150")];
        assert!(reconcile("2026-11", Stage::Calc, &pool, &[]).balanced());
        let settle = reconcile("2026-11", Stage::Settle, &pool, &[]);
        assert!(!settle.balanced(), "结算阶段手工台账必须有账面背书");
        assert_eq!(settle.diff_amount, dec("150"));
        assert_eq!(settle.groups[0].kind, GroupKind::PoolOnly);
        assert_eq!(settle.groups[0].pending_amount, Decimal::ZERO);
    }

    /// 账面多出来的分录（旁路记账）⇒ LedgerOnly，报文列出凭证号。
    #[test]
    fn ledger_only_group_is_flagged_with_voucher_numbers() {
        let dept = Uuid::now_v7();
        let ledger = vec![ledger(Some(dept), "4101.04", "记-2026-11-0007", "200")];
        let outcome = reconcile("2026-11", Stage::Calc, &[], &ledger);
        assert!(!outcome.balanced());
        assert_eq!(outcome.groups[0].kind, GroupKind::LedgerOnly);
        assert_eq!(outcome.diff_amount, dec("-200"));
        assert_eq!(outcome.difference_vouchers(), vec!["记-2026-11-0007".to_string()]);
        let message = mismatch(&outcome).msg;
        assert!(message.contains("记-2026-11-0007"), "报文必须点名凭证：{message}");
        assert!(message.contains("4101.04"), "{message}");
        assert!(message.contains("改走"), "报文必须给修法：{message}");
    }

    /// 两边都有但金额不等 ⇒ Mismatch，差额 = 应已入账 - 账面。
    #[test]
    fn unequal_amounts_are_a_mismatch_not_a_match() {
        let dept = Uuid::now_v7();
        let pool = vec![pool(dept, "4101.01", SOURCE_DEPRECIATION, "1000")];
        let ledger = vec![ledger(Some(dept), "4101.01", "记-2026-11-0003", "800")];
        let outcome = reconcile("2026-11", Stage::Calc, &pool, &ledger);
        assert!(!outcome.balanced());
        assert_eq!(outcome.groups.len(), 1);
        assert_eq!(outcome.groups[0].kind, GroupKind::Mismatch);
        assert_eq!(outcome.groups[0].diff, dec("200"));
        assert_eq!(outcome.diff_amount, dec("200"));
    }

    /// ★ 总额相抵不能算平：A 车间多 100、B 车间少 100，总额 0 但两组都不平。
    #[test]
    fn offsetting_groups_do_not_cancel_out() {
        let dept_a = Uuid::now_v7();
        let dept_b = Uuid::now_v7();
        let pool = vec![pool(dept_b, "4101.01", SOURCE_DEPRECIATION, "100")];
        let ledger = vec![ledger(Some(dept_a), "4101.01", "记-2026-11-0011", "100")];
        let mut outcome = reconcile("2026-11", Stage::Calc, &pool, &ledger);
        // 车间名相同（夹具写死），按 id 排序后再断言
        outcome
            .groups
            .sort_by_key(|group| group.dept_id.map(|id| id.to_string()));
        assert_eq!(outcome.diff_amount, Decimal::ZERO, "总额恰好相抵");
        assert_eq!(outcome.mismatched_groups, 2, "逐组比 → 两组都不平");
        assert!(!outcome.balanced(), "总额 0 也必须报不平");
        let message = mismatch(&outcome).msg;
        assert!(message.contains("2 组不平"), "{message}");
    }

    /// 凭证没挂部门（`aux_dept_id` 为空）⇒ 单独成组，标签写「（无车间）」。
    #[test]
    fn ledger_line_without_dept_forms_its_own_group() {
        let ledger = vec![ledger(None, "4101.01", "记-2026-11-0099", "500")];
        let outcome = reconcile("2026-11", Stage::Calc, &[], &ledger);
        assert!(!outcome.balanced());
        assert_eq!(outcome.groups[0].dept_id, None);
        assert_eq!(outcome.groups[0].label(), "（无车间） / 4101.01");
    }

    /// 同科目不同车间不合并（分组键含车间）。
    #[test]
    fn same_account_in_two_workshops_is_not_merged() {
        let dept_a = Uuid::now_v7();
        let dept_b = Uuid::now_v7();
        let pool = vec![
            pool(dept_a, "4101.01", SOURCE_DEPRECIATION, "100"),
            pool(dept_b, "4101.01", SOURCE_DEPRECIATION, "200"),
        ];
        let ledger = vec![
            ledger(Some(dept_a), "4101.01", "记-2026-11-0001", "100"),
            ledger(Some(dept_b), "4101.01", "记-2026-11-0002", "200"),
        ];
        let outcome = reconcile("2026-11", Stage::Calc, &pool, &ledger);
        assert_eq!(outcome.groups.len(), 2);
        assert!(outcome.balanced(), "{}", outcome.summary_line());
    }

    /// 同一组里多张凭证的号去重后按升序给出。
    #[test]
    fn voucher_numbers_are_deduped_and_sorted() {
        let dept = Uuid::now_v7();
        let ledger = vec![
            ledger(Some(dept), "4101.01", "记-2026-11-0009", "10"),
            ledger(Some(dept), "4101.01", "记-2026-11-0002", "10"),
            ledger(Some(dept), "4101.01", "记-2026-11-0009", "10"),
        ];
        let outcome = reconcile("2026-11", Stage::Calc, &[], &ledger);
        assert_eq!(
            outcome.groups[0].voucher_nos,
            vec!["记-2026-11-0002".to_string(), "记-2026-11-0009".to_string()]
        );
        assert_eq!(outcome.groups[0].ledger_amount, dec("30"));
    }

    /// 报文里的组数上限：超过 [`DIFF_DETAIL_LIMIT`] 只计数。
    #[test]
    fn message_truncates_after_the_detail_limit() {
        let dept = Uuid::now_v7();
        let ledger: Vec<LedgerSideRow> = (0..DIFF_DETAIL_LIMIT + 3)
            .map(|index| ledger(Some(dept), &format!("4101.{:02}", index + 1), "记-1", "10"))
            .collect();
        let outcome = reconcile("2026-11", Stage::Calc, &[], &ledger);
        assert_eq!(outcome.mismatched_groups, DIFF_DETAIL_LIMIT + 3);
        let message = mismatch(&outcome).msg;
        assert!(message.contains("另有 3 组不平"), "{message}");
    }

    /// 阶段口径：Calc 只认折旧 / 报销；Settle 认全部来源。
    #[test]
    fn stage_decides_which_sources_need_voucher_backing() {
        assert!(Stage::Calc.is_vouchered(SOURCE_DEPRECIATION));
        assert!(Stage::Calc.is_vouchered(SOURCE_EXPENSE));
        assert!(!Stage::Calc.is_vouchered(SOURCE_OVERHEAD_TICKET));
        assert!(Stage::Settle.is_vouchered(SOURCE_OVERHEAD_TICKET));
        // 没人认识的来源在结算阶段也必须背书（不允许悄悄绕过）
        assert!(Stage::Settle.is_vouchered("SOMETHING_NEW"));
        assert_eq!(Stage::Calc.as_str(), "计算");
        assert_eq!(Stage::Settle.as_str(), "结算");
    }

    /// 错误码与 HTTP 状态（前端按码给引导，不能随手换）。
    #[test]
    fn mismatch_uses_the_dedicated_error_code() {
        let ledger = vec![ledger(None, "4101.01", "记-1", "1")];
        let outcome = reconcile("2026-11", Stage::Calc, &[], &ledger);
        let error = mismatch(&outcome);
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_POOL_LEDGER_MISMATCH);
        assert_eq!(error_code::ERP_COST_POOL_LEDGER_MISMATCH, 66065);
        assert!(error.msg.contains("2026-11"));
    }

    /// 金额展示去尾零，但 4 位小数的小额不能被吃掉。
    #[test]
    fn money_formatting_keeps_meaningful_decimals() {
        assert_eq!(money(dec("1000.0000")), "1000");
        assert_eq!(money(dec("0.0001")), "0.0001");
        assert_eq!(money(Decimal::ZERO), "0");
        assert_eq!(money(dec("-200.5000")), "-200.5");
    }

    /// 科目前缀常量与费归集段共用（对账口径与池口径必须同一条）。
    #[test]
    fn account_prefix_is_shared_with_the_collect_segment() {
        assert_eq!(ACCOUNT_PREFIX, "4101.");
    }
}

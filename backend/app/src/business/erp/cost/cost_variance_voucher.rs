//! 差异凭证（成本核算 · M5-16）：把 M5-15 的**差异分摊结果**变成一张可落库的凭证。
//!
//! ```text
//! 输入：PeriodVarianceAllocOutcome（六类差异池 × 成本对象的分摊结果，M5-15）
//! 输出：VarianceVoucherDraft（分录草案，纯数据）
//!       └─ post_period_voucher（唯一落库口：voucher::post_voucher）
//!
//!   分录形态（借贷两腿，按「不利 / 有利」翻转）：
//!     不利差异（alloc_amount > 0）  借 5401 主营业务成本     贷 1404 材料成本差异
//!     有利差异（alloc_amount < 0）  借 1404 材料成本差异     贷 5401 主营业务成本
//!     · 差异腿逐成本对象成行（明细可追到批次），损益腿按方向汇总两行
//!     · 金额一律取 `VarianceAllocLine::alloc_amount`（含尾差），**不用 share_rate 算钱**
//! ```
//!
//! # 为什么是「1404 ↔ 5401」这一对（这是本卡唯一有争议的设计点，写清楚）
//!
//! ARCH-9 §4.9.3 的 D-15 原本是「差异不入账」，但本卡（M5-16）就是要出差异凭证，
//! 所以差异必须落到某个科目上。约束有三条，缺一不可：
//!
//! 1. **不碰 `4001.01/.02/.03`**：巡检 C-COST-3 的判据是「4001 三末级净余额 = Σ期末在制 + 在途」，
//!    差异挂上去这条每天都会红；
//! 2. **不碰 `4101.*`**：C-COST-1 / C-COST-4 要求制造费用池结算后清零，差异挂上去就清不了；
//! 3. **不碰 `1405`**：C-COST-2 要求完工结转凭证借方 1405 = 入库金额，差异混进去就不同源了。
//!
//! 剩下的候选只有「差异类科目」与「损益类科目」。设计 §4.9.3 给未来 ADR 留的接口
//! 就是 `1404 材料成本差异`（V0183:76 已存在，资产负债表存货项已含它），所以差异腿取 1404，
//! 对方腿取 `5401 主营业务成本`（差异是「已销产品成本口径的偏差」，结转进 COGS 是标准做法）。
//!
//! **已知妥协（不是 bug，别去「修」）**：
//! * 本系统存货按**实际成本**计价，1404 在采购端没有借方，所以本凭证会让 1404 出现
//!   与差异等额的**贷方余额** —— 它不是「少了资产」，而是「本期差异单列」的表达：
//!   报表层把 1404 与 1405 合并看即为实际成本。将来真按计划成本法入账时，
//!   采购端补 1404 借方后这个余额自然归零（ADR 落地时只需改科目政策，见下）。
//! * 人工 / 制造费用差异也挂 1404（科目名是「材料成本差异」）：全仓**只有这一个**差异类科目，
//!   为了一个科目名去加新科目会牵动科目表与资产负债表映射，属另一张卡的活儿。
//!   因此科目映射抽成 [`VarianceAccountPolicy`]（按要素三个字段 + 对方科目），
//!   ADR 落地时只改 [`DEFAULT_VARIANCE_ACCOUNTS`] 一张表，算数与分录结构一行不动
//!   （单测 `six_types_map_into_the_default_policy` 把当前口径钉死，改口径会立刻红）。
//!
//! # 金额与方向的铁律
//!
//! | 口径 | 取值 | 为什么 |
//! |---|---|---|
//! | 分录金额 | `VarianceAllocLine::alloc_amount`（含 `rounding_adj` 尾差） | 凭证与分摊结果必须同源；用 `share_rate × 池` 反算会少/多 0.0001（M5-15 单测已证） |
//! | 方向 | 该行金额的**符号**（正=不利、负=有利），0 行不生成分录 | 库层 `ck_biz_cost_var_sign` 与 §4.9.4 同号；0 元分录是噪声，`post_voucher` 也不接受 0 元行 |
//! | 尾差 | 分摊阶段已收口（M5-15 最大余数法单行），本文件**不再二次取整** | 二次取整 = 两套尾差规则，凭证与报表就会差一厘 |
//! | 守恒 | `Σ分录金额(带符号) = Σ池待分摊额`，`Σ借 = Σ贷` | 两条都是硬断言：破一条即 500（代码问题，不是数据问题） |
//!
//! # 幂等 / 事务边界
//!
//! * **一个会计期间一张**：`source_type='COST_VARIANCE'` + `source_id=会计期间 id`。
//!   已有未红冲的蓝字 ⇒ **409 + `66112`**（要重出请先红冲）。
//! * `post_period_voucher` **不 commit**，事务边界归调用方（与 `cost_settle_voucher` 同款契约）。
//! * 没有 HTTP 入口：设计 §5.2 的接口清单里没有「发差异凭证」这一步，它是期末结账 / 成本结算
//!   事务的内部步骤；单独暴露会让期间停在「差异算了但没入账」的半算状态（AGENTS 约定 11）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_standard_compare_dto::{
    DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE, ELEMENT_LABOR, ELEMENT_MATERIAL, ELEMENT_OVERHEAD,
};
use crate::business::erp::cost::cost_variance_alloc_dto::{
    PeriodVarianceAllocOutcome, VARIANCE_TYPES_ALL, VarianceAllocLine, VarianceAllocOutcome, canonical_element_of,
    variance_type_label,
};
use crate::business::erp::cost::cost_variance_dto::db_direction_of;
use crate::business::erp::voucher::{self, AccountRef, EntryLine, PostVoucher, STATUS_POSTED};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 金额标度（`biz_voucher_entry.debit_amount / credit_amount` 均为 `NUMERIC(18,4)`）。
pub const MONEY_SCALE: u32 = 4;

/// 自动凭证（落库即记账）。
pub const VOUCHER_TYPE_AUTO: &str = "AUTO";

/// 凭证来源类型（`biz_voucher.source_type`）。
pub const SOURCE_TYPE_COST_VARIANCE: &str = "COST_VARIANCE";

/// 差异科目：`1404 材料成本差异`（设计 §4.9.3 给未来入账留的唯一差异类科目）。
pub const ACCOUNT_VARIANCE: &str = "1404";

/// 对方科目：`5401 主营业务成本`（差异结转进销售成本）。
pub const ACCOUNT_COST_OF_SALES: &str = "5401";

/// 凭证摘要前缀。
pub const SUMMARY_VARIANCE: &str = "成本差异结转";

/// 一条分录行的摘要前缀（明细行）。
pub const SUMMARY_LINE: &str = "成本差异";

/// 差异科目政策（按要素给科目；对方科目一个）。
///
/// 抽成结构体的目的：ADR 给人工 / 制造费用差异单独开科目时，**只换这张表**，
/// [`build_draft`] / [`build_entries`] 的逻辑与单测结构不用动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VarianceAccountPolicy {
    /// 材料类差异（`MAT_PRICE` / `MAT_QTY`）的差异科目。
    pub material_variance: &'static str,
    /// 人工类差异（`LABOR_RATE` / `LABOR_EFF`）的差异科目。
    pub labor_variance: &'static str,
    /// 制造费用类差异（`OH_SPEND` / `OH_VOLUME`）的差异科目。
    pub overhead_variance: &'static str,
    /// 对方科目（不利差异借记 / 有利差异贷记）。
    pub counterpart: &'static str,
}

/// 默认科目政策（口径见模块头「为什么是 1404 ↔ 5401」）。
pub const DEFAULT_VARIANCE_ACCOUNTS: VarianceAccountPolicy = VarianceAccountPolicy {
    material_variance: ACCOUNT_VARIANCE,
    labor_variance: ACCOUNT_VARIANCE,
    overhead_variance: ACCOUNT_VARIANCE,
    counterpart: ACCOUNT_COST_OF_SALES,
};

// ==================== 结构 ====================

/// 一条差异凭证明细（对应「一个成本对象 × 一个差异池」）。
///
/// 金额恒为正数，方向由 [`Self::direction`] 表达 —— 这与 `biz_voucher_entry`
/// 「借贷各一列、行内不出现负数」的口径一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarianceVoucherLine {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号（摘要 / 报文用）。
    pub cost_object_no: String,
    /// 差异类型（`biz_cost_variance.variance_type` 六值之一）。
    pub variance_type: String,
    /// 成本要素（`MATERIAL` / `LABOR` / `OVERHEAD`）。
    pub element: String,
    /// 物料（材料类差异才有；人工 / 费用类为 `None`）。
    pub material_id: Option<Uuid>,
    /// 分录金额（**正数**，4 位）= `|alloc_amount|`。
    pub amount: Decimal,
    /// `UNFAVORABLE`（不利，超支）/ `FAVORABLE`（有利，节约）。
    pub direction: &'static str,
    /// 差异科目（不利时在贷方、有利时在借方）。
    pub variance_account: &'static str,
    /// 对方科目（不利时在借方、有利时在贷方）。
    pub counterpart_account: &'static str,
}

impl VarianceVoucherLine {
    /// 带符号金额（不利为正、有利为负）—— 汇总与守恒断言用它。
    pub fn signed_amount(&self) -> Decimal {
        if self.direction == DIRECTION_FAVORABLE {
            -self.amount
        } else {
            self.amount
        }
    }

    /// 差异腿在这条明细上的借方金额（有利才借）。
    pub fn variance_debit(&self) -> Decimal {
        if self.direction == DIRECTION_FAVORABLE {
            self.amount
        } else {
            Decimal::ZERO
        }
    }

    /// 差异腿在这条明细上的贷方金额（不利才贷）。
    pub fn variance_credit(&self) -> Decimal {
        if self.direction == DIRECTION_FAVORABLE {
            Decimal::ZERO
        } else {
            self.amount
        }
    }

    /// 摘要（人看的那一句）。
    pub fn summary(&self, period_code: &str) -> String {
        format!(
            "{SUMMARY_LINE} · {} · 成本对象 {} · 期间 {period_code}",
            variance_type_label(&self.variance_type),
            self.cost_object_no
        )
    }
}

/// 一期差异凭证的**分录草案**（纯数据，未落库）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarianceVoucherDraft {
    /// 会计期间码 `yyyy-MM`。
    pub period_code: String,
    /// 明细行（按对象号 + 差异类型稳定排序）。
    pub lines: Vec<VarianceVoucherLine>,
    /// 参与过账的差异池个数（含「全 0 行」的池 —— 它们不产生明细）。
    pub pools: usize,
    /// 因金额为 0 被跳过的行数（`participates=false` 的零基数行 + 池为 0 的行）。
    pub skipped_zero: usize,
    /// 不利差异合计（正数）。
    pub unfavorable_total: Decimal,
    /// 有利差异合计（**负数**，与 `signed_amount` 同号）。
    pub favorable_total: Decimal,
    /// 净额 = 不利 + 有利 = Σ 带符号金额。
    pub net_amount: Decimal,
    /// 分录借方合计（含损益腿）。
    pub debit_total: Decimal,
    /// 分录贷方合计（含损益腿）。
    pub credit_total: Decimal,
}

impl VarianceVoucherDraft {
    /// 空草案（没有一条非零差异）。
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// 明细行数。
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// 借贷是否平衡（**必须为真**，否则不许落库）。
    pub fn balanced(&self) -> bool {
        self.debit_total == self.credit_total
    }

    /// 净额是否闭合：`不利 + 有利 = 净额`（三位一体，尾差不会漂）。
    pub fn closes_exactly(&self) -> bool {
        (self.unfavorable_total + self.favorable_total - self.net_amount).is_zero()
    }
}

/// 凭证上下文（期间 + 日期 + 操作人）。
#[derive(Debug, Clone)]
pub struct VarianceVoucherContext<'a> {
    /// 会计期间码 `yyyy-MM`（凭证 `source_no` / `source_doc_no`，也是幂等键的一半）。
    pub period_code: &'a str,
    /// 凭证日期（必须落在 `period_code` 内）。
    pub voucher_date: NaiveDate,
    /// 操作人（`posted_by` / 留痕）。
    pub operator: Option<Uuid>,
    /// 业务时间。
    pub now: DateTime<Utc>,
}

/// 落库结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedVarianceVoucher {
    /// `biz_voucher.id`。
    pub voucher_id: Uuid,
    /// 分录行数。
    pub entry_count: usize,
    /// 借方合计（= 贷方合计）。
    pub total: Decimal,
}

// ==================== 纯逻辑：草案 ====================

/// 4 位金额（`NUMERIC(18,4)`）。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp(MONEY_SCALE)
}

/// 差异类型在默认政策下用哪个差异科目（未知类型 → 400，不静默兜底）。
pub fn variance_account_of(
    policy: &VarianceAccountPolicy,
    variance_type: &str,
) -> std::result::Result<&'static str, ApiError> {
    match canonical_element_of(variance_type) {
        Some(ELEMENT_MATERIAL) => Ok(policy.material_variance),
        Some(ELEMENT_LABOR) => Ok(policy.labor_variance),
        Some(ELEMENT_OVERHEAD) => Ok(policy.overhead_variance),
        _ => Err(invalid(format!(
            "差异类型 {variance_type} 不认识：只能是 {} 之一（库层 ck_biz_cost_var_type 同口径）；\
             科目映射不猜",
            VARIANCE_TYPES_ALL.join(" / ")
        ))),
    }
}

/// **一期差异凭证**：期间级分摊结果 → 分录草案（默认科目政策）。
pub fn build_draft(outcome: &PeriodVarianceAllocOutcome) -> std::result::Result<VarianceVoucherDraft, ApiError> {
    build_draft_with(&DEFAULT_VARIANCE_ACCOUNTS, outcome)
}

/// 同上，科目政策可替换（ADR 落地 / 单测用）。
pub fn build_draft_with(
    policy: &VarianceAccountPolicy,
    outcome: &PeriodVarianceAllocOutcome,
) -> std::result::Result<VarianceVoucherDraft, ApiError> {
    ensure_period_closing(outcome)?;

    let mut draft = VarianceVoucherDraft {
        period_code: outcome.period_code.clone(),
        lines: Vec::new(),
        pools: outcome.outcomes.len(),
        skipped_zero: 0,
        unfavorable_total: Decimal::ZERO,
        favorable_total: Decimal::ZERO,
        net_amount: Decimal::ZERO,
        debit_total: Decimal::ZERO,
        credit_total: Decimal::ZERO,
    };
    let mut allocated = Decimal::ZERO;
    for pool in &outcome.outcomes {
        push_pool(policy, &mut draft, pool)?;
        allocated += pool.allocated_total();
    }
    finish_draft(&mut draft, allocated)?;
    Ok(draft)
}

/// **单池**草案（池级调用；期间级请用 [`build_draft`]）。
pub fn build_pool_draft(
    policy: &VarianceAccountPolicy,
    pool: &VarianceAllocOutcome,
) -> std::result::Result<VarianceVoucherDraft, ApiError> {
    let mut draft = VarianceVoucherDraft {
        period_code: pool.period_code.clone(),
        lines: Vec::new(),
        pools: 1,
        skipped_zero: 0,
        unfavorable_total: Decimal::ZERO,
        favorable_total: Decimal::ZERO,
        net_amount: Decimal::ZERO,
        debit_total: Decimal::ZERO,
        credit_total: Decimal::ZERO,
    };
    push_pool(policy, &mut draft, pool)?;
    let allocated = pool.allocated_total();
    finish_draft(&mut draft, allocated)?;
    Ok(draft)
}

/// 一个池的明细进草案（零金额行跳过，不生成 0 元分录）。
fn push_pool(
    policy: &VarianceAccountPolicy,
    draft: &mut VarianceVoucherDraft,
    pool: &VarianceAllocOutcome,
) -> std::result::Result<(), ApiError> {
    if !pool.closes_exactly() {
        return Err(not_closing(format!(
            "期间 {} 的{}（物料 {}）分摊结果不闭合：已归属 {} + 已摊 {} ≠ 差异合计 {}（残差 {}）。\
             请先重跑差异分摊，再用结果出凭证",
            pool.period_code,
            variance_type_label(&pool.variance_type),
            pool.material_code.as_deref().unwrap_or("—"),
            pool.attributed_amount,
            pool.allocated_total(),
            pool.total_amount,
            pool.residual()
        )));
    }
    let variance_account = variance_account_of(policy, &pool.variance_type)?;
    for line in &pool.lines {
        if let Some(detail) = line_of(policy, pool, line, variance_account)? {
            draft.lines.push(detail);
        } else {
            draft.skipped_zero += 1;
        }
    }
    Ok(())
}

/// 一条分摊结果 → 一条凭证明细（金额 0 ⇒ `None`）。
fn line_of(
    policy: &VarianceAccountPolicy,
    pool: &VarianceAllocOutcome,
    line: &VarianceAllocLine,
    variance_account: &'static str,
) -> std::result::Result<Option<VarianceVoucherLine>, ApiError> {
    let amount = money(line.alloc_amount);
    if amount.is_zero() {
        // 零基数行 / 池为 0：留在分摊结果里（「这批本月没做」要查得到），但不进凭证
        return Ok(None);
    }
    let direction = db_direction_of(amount);
    if direction != DIRECTION_FAVORABLE && direction != DIRECTION_UNFAVORABLE {
        return Err(not_closing(format!(
            "差异金额 {} 的方向字面值 {direction} 不在 FAVORABLE / UNFAVORABLE 里（库层 ck_biz_cost_var_dir 同口径）",
            line.alloc_amount
        )));
    }
    Ok(Some(VarianceVoucherLine {
        cost_object_id: line.cost_object_id,
        cost_object_no: line.cost_object_no.clone(),
        variance_type: pool.variance_type.clone(),
        element: pool.element.clone(),
        // 材料类差异带物料辅助核算；人工 / 费用类必须不带（池的 material_id 为空）
        material_id: pool.material_id,
        amount: amount.abs(),
        direction,
        variance_account,
        counterpart_account: policy.counterpart,
    }))
}

/// 收口：排序 + 三组合计 + 借贷平衡。
fn finish_draft(draft: &mut VarianceVoucherDraft, allocated: Decimal) -> std::result::Result<(), ApiError> {
    // 稳定排序：对象号 → 差异类型 → 物料（同一坐标下不会因为池的入参顺序不同而漂）
    draft.lines.sort_by(|left, right| {
        left.cost_object_no
            .cmp(&right.cost_object_no)
            .then_with(|| left.variance_type.cmp(&right.variance_type))
            .then_with(|| left.amount.cmp(&right.amount))
    });

    let mut unfavorable = Decimal::ZERO;
    let mut favorable = Decimal::ZERO;
    let mut variance_debit = Decimal::ZERO;
    let mut variance_credit = Decimal::ZERO;
    for line in &draft.lines {
        let signed = line.signed_amount();
        if signed < Decimal::ZERO {
            favorable += signed;
        } else {
            unfavorable += signed;
        }
        variance_debit += line.variance_debit();
        variance_credit += line.variance_credit();
    }
    draft.unfavorable_total = money(unfavorable);
    draft.favorable_total = money(favorable);
    draft.net_amount = money(unfavorable + favorable);

    // 守恒 ①：Σ明细带符号金额 = Σ池待分摊额（尾差已经收在行内，必须精确相等）
    if draft.net_amount != money(allocated) {
        return Err(not_closing(format!(
            "期间 {} 的分录金额合计 {} ≠ 分摊结果待分摊合计 {}：出凭证前请先重跑分摊（不要手工拼金额）",
            draft.period_code, draft.net_amount, allocated
        )));
    }

    // 分录：差异腿（逐行）+ 损益腿（按方向汇总两行）→ 借贷必然相等
    draft.debit_total = money(variance_debit + draft.unfavorable_total);
    draft.credit_total = money(variance_credit + -draft.favorable_total);
    if draft.debit_total != draft.credit_total {
        return Err(not_closing(format!(
            "期间 {} 的差异凭证借贷不平：Σ借 {} ≠ Σ贷 {}（代码问题，请报工单）",
            draft.period_code, draft.debit_total, draft.credit_total
        )));
    }
    if !draft.closes_exactly() {
        return Err(not_closing(format!(
            "期间 {} 的差异凭证三合计不自洽：不利 {} + 有利 {} ≠ 净额 {}（代码问题，请报工单）",
            draft.period_code, draft.unfavorable_total, draft.favorable_total, draft.net_amount
        )));
    }
    Ok(())
}

// ==================== 纯逻辑：分录 ====================

/// 草案 → 分录（`AccountRef` 由调用方解析好传进来，纯函数好单测）。
///
/// 结构：差异腿逐行（借 / 贷由方向决定，带物料辅助核算），损益腿按方向汇总两行。
pub fn build_entries(
    draft: &VarianceVoucherDraft,
    accounts: &VarianceAccounts,
) -> std::result::Result<Vec<EntryLine>, ApiError> {
    if draft.is_empty() {
        return Err(not_closing(format!(
            "期间 {} 没有非零差异行，不构成一张凭证（0 元凭证应在生成前就被跳过）",
            draft.period_code
        )));
    }
    let mut lines: Vec<EntryLine> = Vec::with_capacity(draft.lines.len() + 2);
    for detail in &draft.lines {
        let account = accounts.variance_of(detail.variance_account)?;
        lines.push(EntryLine {
            line_no: 0, // 统一在末尾编号
            summary: detail.summary(&draft.period_code),
            account,
            debit: detail.variance_debit(),
            credit: detail.variance_credit(),
            aux_supplier_id: None,
            aux_material_id: detail.material_id,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    let counterpart = &accounts.counterpart;
    if !draft.unfavorable_total.is_zero() {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!(
                "{SUMMARY_VARIANCE} · 不利差异转出主营业务成本 · 期间 {}",
                draft.period_code
            ),
            account: counterpart.clone(),
            debit: draft.unfavorable_total,
            credit: Decimal::ZERO,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    if !draft.favorable_total.is_zero() {
        lines.push(EntryLine {
            line_no: 0,
            summary: format!(
                "{SUMMARY_VARIANCE} · 有利差异冲减主营业务成本 · 期间 {}",
                draft.period_code
            ),
            account: counterpart.clone(),
            debit: Decimal::ZERO,
            credit: -draft.favorable_total,
            aux_supplier_id: None,
            aux_material_id: None,
            aux_warehouse_id: None,
            aux_dept_id: None,
        });
    }
    for (index, line) in lines.iter_mut().enumerate() {
        line.line_no = i32::try_from(index + 1).unwrap_or(1);
    }
    let debit: Decimal = lines.iter().map(|line| line.debit).sum();
    let credit: Decimal = lines.iter().map(|line| line.credit).sum();
    if debit != credit {
        return Err(not_closing(format!(
            "期间 {} 的分录借贷不平：Σ借 {debit} ≠ Σ贷 {credit}（代码问题，请报工单）",
            draft.period_code
        )));
    }
    if lines.len() < 2 {
        return Err(not_closing(format!(
            "期间 {} 只生成了 {} 条分录，不构成一张凭证",
            draft.period_code,
            lines.len()
        )));
    }
    Ok(lines)
}

/// 解析好的科目集合（差异科目按编码去重 + 对方科目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarianceAccounts {
    /// 差异科目（可能多个：ADR 拆细后材料 / 人工 / 费用各一个）。
    pub variance: Vec<AccountRef>,
    /// 对方科目。
    pub counterpart: AccountRef,
}

impl VarianceAccounts {
    /// 按编码取差异科目（政策表里引用了没有解析的科目 ⇒ 400，不静默换科目）。
    pub fn variance_of(&self, code: &str) -> std::result::Result<AccountRef, ApiError> {
        self.variance
            .iter()
            .find(|account| account.code == code)
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    400,
                    error_code::PARAM_INVALID,
                    format!(
                        "会计科目 {code} 未解析到（差异凭证的科目政策引用了不存在的科目）；\
                         请先在「会计科目」里补齐该末级科目，或调整差异科目政策"
                    ),
                )
            })
    }
}

/// 草案里用到的差异科目编码（去重、升序）。
pub fn variance_codes(draft: &VarianceVoucherDraft) -> Vec<String> {
    let mut codes: Vec<String> = draft
        .lines
        .iter()
        .map(|line| line.variance_account.to_string())
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

// ==================== 落库 ====================

/// 会计期间 id（幂等键的 `source_id`）。
async fn accounting_period_id_of(ex: &impl SqlExecutor, period_code: &str) -> std::result::Result<Uuid, ApiError> {
    let row = ex
        .query_first_sql(
            "SELECT id FROM biz_accounting_period WHERE period_code = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .map_err(|error| db_error(error, "差异凭证：查会计期间"))?
        .ok_or_else(|| {
            ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "会计期间 {period_code} 不存在：差异凭证要落在已建立的会计期间上。\
                     请先在「会计期间」里建立该期间"
                ),
            )
        })?;
    Ok(row.get_typed("id")?)
}

/// 该期间已发出的（未红冲的蓝字）差异凭证 —— 红冲 / 查重用。
pub async fn voucher_ids_of(ex: &impl SqlExecutor, accounting_period_id: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_voucher \
              WHERE source_type = ? AND source_id = ?::uuid \
                AND deleted_at IS NULL AND is_red = 0 AND reversed_by IS NULL \
              ORDER BY voucher_no ASC, id ASC"
                .to_string(),
            vec![
                Value::Text(SOURCE_TYPE_COST_VARIANCE.to_string()),
                Value::from(&accounting_period_id),
            ],
        )
        .await?;
    rows.iter().map(|row| row.get_typed("id")).collect()
}

/// 幂等闸：该会计期间是否已发过差异凭证。
async fn ensure_not_posted(
    ex: &impl SqlExecutor,
    accounting_period_id: Uuid,
    period_code: &str,
) -> std::result::Result<(), ApiError> {
    let existing: i64 = ex
        .query_first_sql(
            "SELECT count(*)::bigint AS n FROM biz_voucher \
              WHERE source_type = ? AND source_id = ?::uuid \
                AND deleted_at IS NULL AND is_red = 0 AND reversed_by IS NULL"
                .to_string(),
            vec![
                Value::Text(SOURCE_TYPE_COST_VARIANCE.to_string()),
                Value::from(&accounting_period_id),
            ],
        )
        .await
        .map_err(|error| db_error(error, "差异凭证：查该期间是否已发凭证"))?
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()?
        .unwrap_or(0);
    if existing > 0 {
        return Err(ApiError::new(
            409,
            error_code::ERP_COST_VARIANCE_VOUCHER_DUPLICATE,
            format!(
                "会计期间 {period_code} 已经生成过差异凭证（{existing} 张蓝字未红冲），不会重复记账；\
                 要按新的差异结果重出，请先红冲该期间的差异凭证"
            ),
        ));
    }
    Ok(())
}

/// 解析科目（差异科目去重 + 对方科目）。
pub async fn resolve_accounts(
    ex: &impl SqlExecutor,
    draft: &VarianceVoucherDraft,
) -> std::result::Result<VarianceAccounts, ApiError> {
    let mut variance = Vec::new();
    for code in variance_codes(draft) {
        let account = voucher::account_by_code(ex, &code)
            .await
            .map_err(|error| db_error(error, "差异凭证：取差异科目"))?;
        variance.push(account_of(account, &code)?);
    }
    let counterpart_code = draft
        .lines
        .first()
        .map(|line| line.counterpart_account)
        .unwrap_or(ACCOUNT_COST_OF_SALES);
    let counterpart = voucher::account_by_code(ex, counterpart_code)
        .await
        .map_err(|error| db_error(error, "差异凭证：取对方科目"))?;
    Ok(VarianceAccounts {
        variance,
        counterpart: account_of(counterpart, counterpart_code)?,
    })
}

/// **落一张差异凭证**（唯一收口：`voucher::post_voucher`，不 commit）。
///
/// 返回值 `None` = 草案为空（期间没有非零差异），**不发空凭证**。
///
/// 契约（与 `cost_settle_voucher::post_all` 同款）：
/// * 调用方负责事务打开 / 提交 / 回滚；
/// * 期间已结账、科目非末级等护栏都在 `post_voucher` 里；
/// * 失败一律不落半张凭证（`post_voucher` 内部同一事务）。
pub async fn post_period_voucher(
    ex: &impl SqlExecutor,
    ctx: &VarianceVoucherContext<'_>,
    draft: &VarianceVoucherDraft,
) -> std::result::Result<Option<PostedVarianceVoucher>, ApiError> {
    ensure_context_matches(draft, ctx)?;
    if draft.is_empty() {
        return Ok(None);
    }
    if !draft.balanced() || !draft.closes_exactly() {
        return Err(not_closing(format!(
            "期间 {} 的差异凭证草案自身不平（Σ借 {} / Σ贷 {}），拒绝落库",
            draft.period_code, draft.debit_total, draft.credit_total
        )));
    }

    let accounting_period_id = accounting_period_id_of(ex, ctx.period_code).await?;
    ensure_not_posted(ex, accounting_period_id, ctx.period_code).await?;
    let accounts = resolve_accounts(ex, draft).await?;
    let lines = build_entries(draft, &accounts)?;
    let entry_count = lines.len();
    let summary = format!(
        "{SUMMARY_VARIANCE} · 期间 {} · {} 条明细 · 净额 {}",
        draft.period_code,
        draft.line_count(),
        draft.net_amount
    );
    let remark = format!(
        "成本差异入账（M5-16）：按差异分摊结果逐成本对象结转，不利差异借 5401 / 贷 {ACCOUNT_VARIANCE}，\
         有利差异反向；已分摊合计 {}，其中不利 {} / 有利 {}。\
         本凭证不动存货计价（4001/4101/1405 零影响），差异单列在差异科目上。",
        draft.net_amount, draft.unfavorable_total, draft.favorable_total
    );
    let voucher_id = voucher::post_voucher(
        ex,
        &PostVoucher {
            voucher_type: VOUCHER_TYPE_AUTO,
            source_type: SOURCE_TYPE_COST_VARIANCE,
            source_id: Some(accounting_period_id),
            source_no: Some(ctx.period_code),
            source_doc_no: Some(ctx.period_code),
            voucher_date: ctx.voucher_date,
            summary,
            lines,
            remark: Some(&remark),
            red_of: None,
            status: STATUS_POSTED,
        },
        ctx.operator,
        ctx.now,
    )
    .await
    .map_err(|error| db_error(error, "差异凭证：落库"))?;
    Ok(Some(PostedVarianceVoucher {
        voucher_id,
        entry_count,
        total: draft.debit_total,
    }))
}

// ==================== 错误构造 ====================

/// 差异凭证口径问题（用户可修：重跑分摊 / 补科目）—— 400。
fn invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

/// 结果不闭合 / 借贷不平 / 三合计不自洽 —— 500（代码或上游算法问题，不是用户数据问题）。
fn not_closing(message: String) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, message)
}

/// 分期结果闭合校验（期间级双恒等式）。
fn ensure_period_closing(outcome: &PeriodVarianceAllocOutcome) -> std::result::Result<(), ApiError> {
    if outcome.closes_exactly() {
        return Ok(());
    }
    Err(not_closing(format!(
        "期间 {} 的差异分摊结果不闭合：Σ已归属 {} + Σ已摊 {} ≠ Σ差异合计 {}（残差 {}）。\
         凭证会记错钱，请先重跑差异分摊",
        outcome.period_code,
        outcome.total_attributed(),
        outcome.total_allocated(),
        outcome.total_amount(),
        outcome.residual()
    )))
}

fn account_of(account: Option<AccountRef>, code: &str) -> std::result::Result<AccountRef, ApiError> {
    account.ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "会计科目 {code} 不存在或已停用，无法生成差异凭证\
                 （差异科目 1404 材料成本差异 / 对方科目 5401 主营业务成本应随 V0183 已有；\
                 被停用时请先在「会计科目」里启用）"
            ),
        )
    })
}

fn db_error(error: DbError, context: &str) -> ApiError {
    utils::db_error(error, context)
}

/// 草案与上下文一致性（期间码 + 凭证日期都必须在同一月）—— 落库前零 IO 的护栏。
fn ensure_context_matches(
    draft: &VarianceVoucherDraft,
    ctx: &VarianceVoucherContext<'_>,
) -> std::result::Result<(), ApiError> {
    if draft.period_code != ctx.period_code {
        return Err(invalid(format!(
            "差异凭证草案属于期间 {}，上下文是 {}：草案与凭证期间必须一致（否则凭证会落到隔壁月份）",
            draft.period_code, ctx.period_code
        )));
    }
    ensure_voucher_date_in_period(ctx)
}

/// 凭证日期必须落在差异期间内（`post_voucher` 按日期反查会计期间）。
fn ensure_voucher_date_in_period(ctx: &VarianceVoucherContext<'_>) -> std::result::Result<(), ApiError> {
    let month = ctx.voucher_date.format("%Y-%m").to_string();
    if month == ctx.period_code {
        return Ok(());
    }
    Err(invalid(format!(
        "差异凭证日期 {}（{month}）与差异期间 {} 不在同一个月：凭证会落到隔壁会计期间，\
         请把凭证日期改到期间内",
        ctx.voucher_date, ctx.period_code
    )))
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_qty_variance_dto::VARIANCE_LABOR_EFF;
    use crate::business::erp::cost::cost_variance_alloc_dto::VARIANCE_OH_SPEND;
    use crate::business::erp::cost::cost_variance_dto::VARIANCE_MAT_PRICE;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    fn account(code: &str) -> AccountRef {
        AccountRef {
            id: Uuid::now_v7(),
            code: code.to_string(),
            name: format!("科目 {code}"),
        }
    }

    fn accounts() -> VarianceAccounts {
        VarianceAccounts {
            variance: vec![account(ACCOUNT_VARIANCE)],
            counterpart: account(ACCOUNT_COST_OF_SALES),
        }
    }

    /// 一行分摊结果。
    fn alloc_line(object_no: &str, amount: &str) -> VarianceAllocLine {
        VarianceAllocLine {
            cost_object_id: Uuid::now_v7(),
            cost_object_no: object_no.to_string(),
            cost_object_status: "OPEN".to_string(),
            basis_value: dec("100"),
            share_rate: Some(dec("0.5")),
            alloc_amount: dec(amount),
            rounding_adj: Decimal::ZERO,
            participates: !dec(amount).is_zero(),
        }
    }

    /// 一个池（合计 = 已归属 + Σ行）。
    fn pool(
        period: &str,
        variance_type: &str,
        element: &str,
        material: Option<&str>,
        attributed: &str,
        lines: Vec<VarianceAllocLine>,
    ) -> VarianceAllocOutcome {
        let allocated: Decimal = lines.iter().map(|line| line.alloc_amount).sum();
        let attributed = dec(attributed);
        let material_id = material.map(|_| Uuid::now_v7());
        VarianceAllocOutcome {
            period_code: period.to_string(),
            variance_type: variance_type.to_string(),
            element: element.to_string(),
            material_id,
            material_code: material.map(|code| code.to_string()),
            rule_id: None,
            rule_code: "DEFAULT".to_string(),
            rule_name: "默认口径".to_string(),
            basis: "ACTUAL_QTY".to_string(),
            basis_source: "DEFAULT",
            total_amount: attributed + allocated,
            attributed_amount: attributed,
            allocatable_amount: allocated,
            basis_total: dec("200"),
            lines,
            rounding_total: Decimal::ZERO,
        }
    }

    fn period_of(outcomes: Vec<VarianceAllocOutcome>, period: &str) -> PeriodVarianceAllocOutcome {
        PeriodVarianceAllocOutcome {
            period_code: period.to_string(),
            outcomes,
        }
    }

    /// 六类差异在默认政策下全部落 1404（ADR 拆科目时这条会红，改政策而不是改断言）。
    #[test]
    fn six_types_map_into_the_default_policy() {
        for variance_type in VARIANCE_TYPES_ALL {
            let code = variance_account_of(&DEFAULT_VARIANCE_ACCOUNTS, variance_type).expect("六类差异都应有科目");
            assert_eq!(code, ACCOUNT_VARIANCE, "{variance_type} 的差异科目");
        }
        assert_eq!(DEFAULT_VARIANCE_ACCOUNTS.counterpart, ACCOUNT_COST_OF_SALES);
        assert_eq!(ACCOUNT_VARIANCE, "1404");
        assert_eq!(ACCOUNT_COST_OF_SALES, "5401");
        assert_eq!(SOURCE_TYPE_COST_VARIANCE, "COST_VARIANCE");
        assert_eq!(MONEY_SCALE, 4);
    }

    /// 政策可替换：人工差异换成别的科目后，只有人工那几行跟着换。
    #[test]
    fn policy_override_moves_only_the_touched_element() {
        let policy = VarianceAccountPolicy {
            labor_variance: "1406",
            ..DEFAULT_VARIANCE_ACCOUNTS
        };
        assert_eq!(
            variance_account_of(&policy, VARIANCE_LABOR_EFF).expect("人工差异"),
            "1406"
        );
        assert_eq!(
            variance_account_of(&policy, VARIANCE_MAT_PRICE).expect("材料价差"),
            ACCOUNT_VARIANCE
        );
        assert_eq!(
            variance_account_of(&policy, VARIANCE_OH_SPEND).expect("费用差异"),
            ACCOUNT_VARIANCE
        );
    }

    /// 未知差异类型 ⇒ 400（不静默兜底到某个「看起来通用」的科目）。
    #[test]
    fn unknown_variance_type_is_rejected() {
        let error = variance_account_of(&DEFAULT_VARIANCE_ACCOUNTS, "MAT_FREIGHT").expect_err("应拒绝");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(format!("{error:?}").contains("MAT_FREIGHT"), "报文要点名类型");
    }

    /// 不利差异：金额取 alloc_amount、方向 UNFAVORABLE、差异科目在贷方。
    #[test]
    fn unfavorable_line_credits_the_variance_account() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "0",
                vec![alloc_line("CB-1", "600.0000"), alloc_line("CB-2", "400.0000")],
            )],
            "2026-11",
        ))
        .expect("草案");
        assert_eq!(draft.line_count(), 2);
        assert_eq!(draft.unfavorable_total, dec("1000"));
        assert_eq!(draft.favorable_total, Decimal::ZERO);
        assert_eq!(draft.net_amount, dec("1000"));
        for line in &draft.lines {
            assert_eq!(line.direction, DIRECTION_UNFAVORABLE);
            assert_eq!(line.variance_debit(), Decimal::ZERO);
            assert_eq!(line.variance_credit(), line.amount);
            assert_eq!(line.variance_account, ACCOUNT_VARIANCE);
            assert_eq!(line.counterpart_account, ACCOUNT_COST_OF_SALES);
            assert!(line.material_id.is_some(), "材料类差异要带物料辅助核算");
        }
        assert_eq!(draft.debit_total, dec("1000"));
        assert_eq!(draft.credit_total, dec("1000"));
        assert!(draft.balanced());
    }

    /// 有利差异：符号翻转（差异科目在借方），金额取绝对值。
    #[test]
    fn favorable_line_debits_the_variance_account() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_LABOR_EFF,
                ELEMENT_LABOR,
                None,
                "0",
                vec![alloc_line("CB-1", "-180.0000")],
            )],
            "2026-11",
        ))
        .expect("草案");
        let line = &draft.lines[0];
        assert_eq!(line.direction, DIRECTION_FAVORABLE);
        assert_eq!(line.amount, dec("180"));
        assert_eq!(line.variance_debit(), dec("180"));
        assert_eq!(line.variance_credit(), Decimal::ZERO);
        assert!(line.material_id.is_none(), "人工差异不带物料");
        assert_eq!(draft.favorable_total, dec("-180"));
        assert_eq!(draft.net_amount, dec("-180"));
        assert_eq!(draft.debit_total, dec("180"));
        assert_eq!(draft.credit_total, dec("180"));
    }

    /// 不利 + 有利同期间：两腿各自汇总，借贷都是 `Σ不利 + Σ有利`。
    #[test]
    fn mixed_period_balances_both_legs() {
        let draft = build_draft(&period_of(
            vec![
                pool(
                    "2026-11",
                    VARIANCE_MAT_PRICE,
                    ELEMENT_MATERIAL,
                    Some("MAT-A"),
                    "0",
                    vec![alloc_line("CB-1", "600.0000"), alloc_line("CB-2", "600.0000")],
                ),
                pool(
                    "2026-11",
                    VARIANCE_LABOR_EFF,
                    ELEMENT_LABOR,
                    None,
                    "0",
                    vec![alloc_line("CB-1", "-300.0000"), alloc_line("CB-2", "0")],
                ),
            ],
            "2026-11",
        ))
        .expect("草案");
        assert_eq!(draft.pools, 2);
        assert_eq!(draft.line_count(), 3, "0 元行被跳过");
        assert_eq!(draft.skipped_zero, 1);
        assert_eq!(draft.unfavorable_total, dec("1200"));
        assert_eq!(draft.favorable_total, dec("-300"));
        assert_eq!(draft.net_amount, dec("900"));
        assert_eq!(draft.debit_total, dec("1500"));
        assert_eq!(draft.credit_total, dec("1500"));
        assert!(draft.closes_exactly());

        let entries = build_entries(&draft, &accounts()).expect("分录");
        // 3 条差异腿 + 2 条损益腿（不利一行、有利一行）
        assert_eq!(entries.len(), 5);
        let debit: Decimal = entries.iter().map(|line| line.debit).sum();
        let credit: Decimal = entries.iter().map(|line| line.credit).sum();
        assert_eq!(debit, credit);
        assert_eq!(debit, dec("1500"));
        // 相对方科目的两行汇总在损益腿上
        let counterpart_lines: Vec<&EntryLine> = entries
            .iter()
            .filter(|line| line.account.code == ACCOUNT_COST_OF_SALES)
            .collect();
        assert_eq!(counterpart_lines.len(), 2);
        assert_eq!(counterpart_lines.iter().map(|l| l.debit).sum::<Decimal>(), dec("1200"));
        assert_eq!(counterpart_lines.iter().map(|l| l.credit).sum::<Decimal>(), dec("300"));
    }

    /// 尾差随行进凭证：`alloc_amount` 已含 `rounding_adj`，凭证不再取整。
    #[test]
    fn rounding_adjustment_rides_on_the_line() {
        let mut first = alloc_line("CB-1", "333.3334");
        first.rounding_adj = dec("0.0001");
        let second = alloc_line("CB-2", "333.3333");
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "0",
                vec![first, second],
            )],
            "2026-11",
        ))
        .expect("草案");
        assert_eq!(draft.net_amount, dec("666.6667"));
        let entries = build_entries(&draft, &accounts()).expect("分录");
        let total: Decimal = entries.iter().map(|line| line.debit).sum();
        assert_eq!(total, dec("666.6667"), "尾差 0.0001 必须在凭证里");
    }

    /// 零基数行（`participates=false`）留在分摊结果里但不进凭证。
    #[test]
    fn zero_basis_lines_are_skipped() {
        let mut idle = alloc_line("CB-9", "0");
        idle.participates = false;
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "0",
                vec![alloc_line("CB-1", "100"), idle],
            )],
            "2026-11",
        ))
        .expect("草案");
        assert_eq!(draft.line_count(), 1);
        assert_eq!(draft.skipped_zero, 1);
    }

    /// 全 0 池（差异池为 0）：草案为空 —— 不发空凭证。
    #[test]
    fn zero_pool_yields_empty_draft() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_OH_SPEND,
                ELEMENT_OVERHEAD,
                None,
                "0",
                vec![alloc_line("CB-1", "0"), alloc_line("CB-2", "0")],
            )],
            "2026-11",
        ))
        .expect("草案");
        assert!(draft.is_empty());
        assert_eq!(draft.pools, 1);
        assert_eq!(draft.net_amount, Decimal::ZERO);
        let error = build_entries(&draft, &accounts()).expect_err("空草案不出分录");
        assert_eq!(error.code, error_code::INTERNAL_ERROR);
    }

    /// 没有池（期间没有差异）：空草案。
    #[test]
    fn empty_period_yields_empty_draft() {
        let draft = build_draft(&period_of(Vec::new(), "2026-11")).expect("草案");
        assert!(draft.is_empty());
        assert_eq!(draft.pools, 0);
        assert!(draft.balanced());
    }

    /// 已归属差异（`attributed_amount`）不进凭证 —— 那部分差异行自己挂着对象，由落表方出。
    #[test]
    fn attributed_amount_is_not_double_posted() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "500",
                vec![alloc_line("CB-1", "300")],
            )],
            "2026-11",
        ))
        .expect("草案");
        assert_eq!(draft.net_amount, dec("300"), "只发已分摊部分");
        assert_eq!(draft.debit_total, dec("300"));
    }

    /// 分摊结果不闭合（残差 ≠ 0）⇒ 500，绝不出凭证。
    #[test]
    fn not_closing_pool_is_rejected() {
        let mut bad = pool(
            "2026-11",
            VARIANCE_MAT_PRICE,
            ELEMENT_MATERIAL,
            Some("MAT-A"),
            "0",
            vec![alloc_line("CB-1", "100")],
        );
        // 手工把合计改成 112（已归属 11 + 已摊 100 = 111）⇒ 残差 1
        bad.attributed_amount = dec("11");
        bad.total_amount = dec("112");
        let error = build_draft(&period_of(vec![bad], "2026-11")).expect_err("应拒绝");
        assert_eq!(error.status, 500);
        assert_eq!(error.code, error_code::INTERNAL_ERROR);
    }

    /// 期间级不闭合（池各自平但期间合计不守恒）⇒ 500。
    #[test]
    fn period_residual_is_rejected() {
        let mut outcome = period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "0",
                vec![alloc_line("CB-1", "100")],
            )],
            "2026-11",
        );
        // 篡改期间级合计：池自身仍闭合，但期间级 Σ合计 ≠ 已归属 + 已摊
        outcome.outcomes[0].total_amount = dec("101");
        let error = build_draft(&outcome).expect_err("应拒绝");
        assert_eq!(error.status, 500);
    }

    /// 排序稳定：同一组池换个入参顺序，明细顺序一样。
    #[test]
    fn order_is_stable_across_pool_order() {
        let mat = pool(
            "2026-11",
            VARIANCE_MAT_PRICE,
            ELEMENT_MATERIAL,
            Some("MAT-A"),
            "0",
            vec![alloc_line("CB-2", "10"), alloc_line("CB-1", "20")],
        );
        let labor = pool(
            "2026-11",
            VARIANCE_LABOR_EFF,
            ELEMENT_LABOR,
            None,
            "0",
            vec![alloc_line("CB-1", "-5")],
        );
        let first = build_draft(&period_of(vec![mat.clone(), labor.clone()], "2026-11")).expect("草案");
        let second = build_draft(&period_of(vec![labor, mat], "2026-11")).expect("草案");
        let keys = |draft: &VarianceVoucherDraft| {
            draft
                .lines
                .iter()
                .map(|line| (line.cost_object_no.clone(), line.variance_type.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(&first), keys(&second));
        assert_eq!(
            keys(&first),
            vec![
                ("CB-1".to_string(), VARIANCE_LABOR_EFF.to_string()),
                ("CB-1".to_string(), VARIANCE_MAT_PRICE.to_string()),
                ("CB-2".to_string(), VARIANCE_MAT_PRICE.to_string()),
            ]
        );
    }

    /// 摘要里人看得见「哪一批、哪个差异、哪个期间」。
    #[test]
    fn summary_names_object_type_and_period() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "0",
                vec![alloc_line("CB-1", "100")],
            )],
            "2026-11",
        ))
        .expect("草案");
        let text = draft.lines[0].summary("2026-11");
        assert!(text.contains("CB-1"));
        assert!(text.contains("材料价差"));
        assert!(text.contains("2026-11"));
    }

    /// 分录行号从 1 起连续、借贷只在一边。
    #[test]
    fn entry_line_numbers_are_sequential() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "0",
                vec![alloc_line("CB-1", "100"), alloc_line("CB-2", "-40")],
            )],
            "2026-11",
        ))
        .expect("草案");
        let entries = build_entries(&draft, &accounts()).expect("分录");
        let numbers: Vec<i32> = entries.iter().map(|line| line.line_no).collect();
        assert_eq!(numbers, (1..=entries.len() as i32).collect::<Vec<_>>());
        for line in &entries {
            assert!(line.debit.is_zero() || line.credit.is_zero(), "一行只能在一侧有金额");
        }
    }

    /// 科目政策引用了没解析的科目 ⇒ 400（不静默换科目）。
    #[test]
    fn unresolved_variance_account_is_rejected() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_MAT_PRICE,
                ELEMENT_MATERIAL,
                Some("MAT-A"),
                "0",
                vec![alloc_line("CB-1", "100")],
            )],
            "2026-11",
        ))
        .expect("草案");
        let accounts = VarianceAccounts {
            variance: vec![account("1406")],
            counterpart: account(ACCOUNT_COST_OF_SALES),
        };
        let error = build_entries(&draft, &accounts).expect_err("应拒绝");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
    }

    /// 差异科目编码去重升序（政策拆细后可能有多个）。
    #[test]
    fn variance_codes_are_deduped_and_sorted() {
        let draft = build_draft(&period_of(
            vec![
                pool(
                    "2026-11",
                    VARIANCE_MAT_PRICE,
                    ELEMENT_MATERIAL,
                    Some("MAT-A"),
                    "0",
                    vec![alloc_line("CB-1", "100")],
                ),
                pool(
                    "2026-11",
                    VARIANCE_LABOR_EFF,
                    ELEMENT_LABOR,
                    None,
                    "0",
                    vec![alloc_line("CB-1", "-50")],
                ),
            ],
            "2026-11",
        ))
        .expect("草案");
        assert_eq!(variance_codes(&draft), vec![ACCOUNT_VARIANCE.to_string()]);
    }

    /// 草案期间与上下文不一致 ⇒ 400（不许把凭证落到隔壁月份）。
    #[test]
    fn period_mismatch_is_rejected_before_any_db_work() {
        let draft = build_draft(&period_of(Vec::new(), "2026-11")).expect("草案");
        let ctx = VarianceVoucherContext {
            period_code: "2026-12",
            voucher_date: NaiveDate::from_ymd_opt(2026, 12, 31).expect("日期"),
            operator: None,
            now: Utc::now(),
        };
        let error = ensure_context_matches(&draft, &ctx).expect_err("应拒绝");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        let same_period = VarianceVoucherContext {
            period_code: "2026-11",
            voucher_date: NaiveDate::from_ymd_opt(2026, 11, 30).expect("日期"),
            ..ctx
        };
        assert!(ensure_context_matches(&draft, &same_period).is_ok());
    }

    /// 凭证日期与期间错月 ⇒ 400（不碰库）。
    #[test]
    fn voucher_date_outside_period_is_rejected() {
        let ctx = VarianceVoucherContext {
            period_code: "2026-11",
            voucher_date: NaiveDate::from_ymd_opt(2026, 12, 1).expect("日期"),
            operator: None,
            now: Utc::now(),
        };
        let error = ensure_voucher_date_in_period(&ctx).expect_err("应拒绝");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        let ok = VarianceVoucherContext {
            voucher_date: NaiveDate::from_ymd_opt(2026, 11, 30).expect("日期"),
            ..ctx
        };
        assert!(ensure_voucher_date_in_period(&ok).is_ok());
    }

    /// 大金额不溢出（`NUMERIC(18,4)` 上限量级），仍是 4 位。
    #[test]
    fn large_amounts_stay_at_four_decimals() {
        let draft = build_draft(&period_of(
            vec![pool(
                "2026-11",
                VARIANCE_OH_SPEND,
                ELEMENT_OVERHEAD,
                None,
                "0",
                vec![alloc_line("CB-1", "123456789012.3456")],
            )],
            "2026-11",
        ))
        .expect("草案");
        assert_eq!(draft.net_amount, dec("123456789012.3456"));
        assert_eq!(draft.net_amount.scale(), 4);
        assert!(draft.balanced());
    }
}

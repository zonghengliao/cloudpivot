//! 固定资产的**纯计算**口径（ARCH-6 §5.3 / §5.4 / §5.6.4）：不碰库、不碰 HTTP。
//!
//! 为什么要单独一个文件：折旧算法是这张卡唯一「算错了不会报错、只会悄悄错账」的地方
//! （净值永远偏一点、尾差永远差 1 分）。纯函数才能用单测把 §5.4.3 的三个算例
//! 一分不差地钉住；handler 只负责取数与落库。
//!
//! # 三条铁律（ARCH-6 §5.4.1，来自中国会计实务）
//!
//! | # | 规则 | 落地点 |
//! |---|---|---|
//! | ① | 当月增加的资产当月不提，从下月起提 | [`start_depreciation_date`] = 购入日的**次月 1 日** |
//! | ② | 当月减少的资产当月照提，从下月起停 | 筛选 SQL 用 `disposal_date >= 期初`（见 `depreciation_model`） |
//! | ③ | 提足即停 + 最后一个月补齐尾差 | [`month_amount`] |
//!
//! # 金额一律 `Decimal`（定点四位）
//!
//! 用 `f64` 会在 60 个月的累加上分叉出「差 0.0001」的账，而 `NUMERIC(18,4)` 的
//! 唯一比较口径是「四位小数完全相等」—— 库里的 CHECK（`ck_biz_fa_net` /
//! `ck_biz_dep_accum`）会当场拒写。

use nik::prelude::chrono::{Datelike, NaiveDate};
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};

use crate::business::erp::voucher::AccountRef;
use crate::business::erp::voucher_multi::ExplicitLine;

/// 金额四位小数（与 `NUMERIC(18,4)` 同口径）。
pub const MONEY_SCALE: u32 = 4;

/// ROUND_HALF_UP 到四位。
///
/// **不能**用 `Decimal::round_dp`：它默认是「银行家舍入」（.00005 → .0000），
/// 而会计口径是四舍五入（.00005 → .0001）。两者在 5 结尾的金额上会差 1 分，
/// 单测的算例（1,583.3333）正好踩不到，但 1/8 之类的分母会踩到。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(MONEY_SCALE, RoundingStrategy::MidpointAwayFromZero)
}

/// 预计净残值 = 原值 × 残值率（四位）。
pub fn residual_value(original: Decimal, salvage_rate: Decimal) -> Decimal {
    money(original * salvage_rate)
}

/// 可折旧总额 = 原值 − 残值。
pub fn depreciable_base(original: Decimal, residual: Decimal) -> Decimal {
    original - residual
}

/// 月折旧额 = round(可折旧总额 / 使用月数, 4)（ROUND_HALF_UP）。
///
/// `useful_life_months` 由 DDL 保证 1..=600，这里再判一次是为了「除零不出 panic」。
pub fn monthly_depreciation(base: Decimal, useful_life_months: i32) -> Decimal {
    if useful_life_months <= 0 {
        return Decimal::ZERO;
    }
    money(base / Decimal::from(useful_life_months))
}

/// 开始计提日 = 购入日的**次月 1 日**（规则 ①：当月增加当月不提）。
pub fn start_depreciation_date(purchase_date: NaiveDate) -> NaiveDate {
    let (year, month) = if purchase_date.month() == 12 {
        (purchase_date.year() + 1, 1)
    } else {
        (purchase_date.year(), purchase_date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(purchase_date)
}

/// `yyyy-MM`（会计期间编码，与 `biz_accounting_period.period_code` 同宽 7）。
pub fn period_of(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

/// 期间的首日 / 末日。非法编码返回 `None`（调用方 400）。
pub fn period_range(period_code: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (year, month) = split_period(period_code)?;
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let next = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
    let end = next.pred_opt()?;
    Some((start, end))
}

/// 上一个期间（`2026-01` → `2025-12`）。
pub fn prev_period(period_code: &str) -> Option<String> {
    let (year, month) = split_period(period_code)?;
    Some(if month == 1 {
        format!("{:04}-12", year - 1)
    } else {
        format!("{:04}-{:02}", year, month - 1)
    })
}

/// 期间编码是否合法（`yyyy-MM`，月份 01–12）。
pub fn is_valid_period(period_code: &str) -> bool {
    split_period(period_code).is_some()
}

fn split_period(period_code: &str) -> Option<(i32, u32)> {
    let text = period_code.trim();
    if text.len() != 7 || text.as_bytes()[4] != b'-' {
        return None;
    }
    let year: i32 = text[..4].parse().ok()?;
    let month: u32 = text[5..].parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

/// 折旧计算所需的一张卡的折旧参数（**快照**：分类改了不影响已建卡的资产）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepreciationCard {
    /// 原值。
    pub original_value: Decimal,
    /// 预计净残值（建卡时算死）。
    pub residual_value: Decimal,
    /// 可折旧总额 = 原值 − 残值。
    pub depreciable_base: Decimal,
    /// 月折旧额（建卡时算好；尾差在最后一个月调）。
    pub monthly_depreciation: Decimal,
    /// 使用月数。
    pub useful_life_months: i32,
    /// 已提累计折旧。
    pub accumulated_depreciation: Decimal,
    /// 已提月数。
    pub months_done: i32,
}

/// 本期应提额。`None` = 本期不提取（已提足 / 已到期末 / 金额为 0）。
///
/// ```text
/// n = months_done + 1
/// n < useful_life_months   → monthly
/// n = useful_life_months   → base − monthly × (useful_life_months − 1)   ← 尾差补齐
/// 已提足（accumulated ≥ base）→ None（提足即停）
/// ```
///
/// 尾差补齐**不是**「每月都取差额」，而是**只在最后一期**取差额：
/// 前面 59 个月必须逐月一致（财务核对时按月对得上），最后一期吸收累计误差。
pub fn month_amount(card: &DepreciationCard) -> Option<Decimal> {
    let remaining = card.depreciable_base - card.accumulated_depreciation;
    if remaining <= Decimal::ZERO {
        return None;
    }
    let next_month = card.months_done + 1;
    let amount = if next_month >= card.useful_life_months {
        // 最后一期（含「已超期」的脏数据）：把剩下的全提完，保证累计恰好等于可折旧总额。
        remaining
    } else {
        card.monthly_depreciation.min(remaining)
    };
    let amount = money(amount);
    if amount <= Decimal::ZERO { None } else { Some(amount) }
}

/// 本期是否是「提足的最后一期」（尾差补齐期，落 `biz_depreciation.is_last_month`）。
pub fn is_last_month(card: &DepreciationCard) -> bool {
    card.months_done + 1 >= card.useful_life_months
}

// ==================== 规则②的落点：处置闸（FIX-3 方案 ①，口径定稿） ====================

/// 「当月折旧未过账」的两种原因（handler 拼报文用；`label` 给日志）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepGateMiss {
    /// 处置当月**没有**该资产的折旧明细行（批次还没跑 / 跑了但不含它）。
    NoDetail,
    /// 处置当月的明细**被红冲**（批次红冲后没有重新计提：那一个月的折旧等于没提）。
    Reversed,
}

impl DepGateMiss {
    /// 人读原因（日志 / 报文）。
    pub const fn label(self) -> &'static str {
        match self {
            DepGateMiss::NoDetail => "处置当月没有该资产的折旧明细（批次未计提，或批次已过账但不含本资产）",
            DepGateMiss::Reversed => "处置当月的折旧明细已被红冲（批次红冲后没有重新计提）",
        }
    }
}

/// 「不用查当月折旧」的两种情形（放行，但要能说清为什么放行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepGateSkip {
    /// 当月购入当月处置：起提日 = 购入次月 1 日，本月**本来就不提**（铁律①）。
    BoughtThisMonth,
    /// 已提足（累计 ≥ 可折旧总额）：提足即停，扫描 SQL 也不会再命中它。
    FullyDepreciated,
}

impl DepGateSkip {
    /// 人读原因（日志用）。
    pub const fn label(self) -> &'static str {
        match self {
            DepGateSkip::BoughtThisMonth => "当月购入当月处置：本月本来不提折旧",
            DepGateSkip::FullyDepreciated => "已提足折旧：本月本来不再计提",
        }
    }
}

/// 处置前置闸（「处置当月的折旧已计提并过账」）的判定结果（纯值，不碰库 / 不碰 HTTP）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisposalDepGate {
    /// 放行：当月本来不提折旧，无需明细（当月购入当月处置 / 已提足）。
    NotRequired(DepGateSkip),
    /// 放行：处置当月有 `POSTED` 的折旧明细。
    Posted,
    /// **拦截**：应提而未提 → handler 回 409 `ERP_ASSET_DEPRECIATION_PENDING`。
    Missing(DepGateMiss),
}

impl DisposalDepGate {
    /// 是否要拦下这次处置。
    pub const fn is_blocked(self) -> bool {
        matches!(self, DisposalDepGate::Missing(_))
    }
}

/// 处置闸：给定卡片与「处置当月那台资产的折旧明细状态」，判这次处置能不能提交。
///
/// # 口径定稿（FIX-3，写死在这里；要改口径先改这张表，别只改代码）
///
/// 铁律②「当月减少当月照提」在代码里有**两个**落点，缺一个就会静默漏提一个月：
///
/// | # | 落点 | 位置 |
/// |---|---|---|
/// | 1 | 扫描条件 `disposal_date IS NULL OR disposal_date >= 期初` | [`crate::business::erp::asset_model::scan_assets_for_period`] |
/// | 2 | **处置前置闸**「处置当月的折旧已计提并过账」 | 本函数 + `asset_handler::asset_dispose` |
///
/// ## 为什么选方案 ①（处置前置闸），不选方案 ②（扫描纳入本月处置 + 处置凭证延后）
///
/// FIX-3 之前的真实缺口：`scan_assets_for_period` 的状态条件只有
/// `status IN ('IN_USE','IDLE')`，卡片一旦置 `DISPOSED` 就被**整类排除** ⇒
/// 「6 月处置的资产，6 月的折旧批次里没有它」；而处置凭证已按处置时点的累计折旧落账
/// ⇒ 卡片缓存 / 折旧明细 / 凭证三处永久差一个月（补提也补不回来：批次一期一条，
/// 处置凭证不会跟着改）。
///
/// * **方案 ①（本实现）**：把动作顺序钉死成「**先计提当月 → 再提交处置**」。
///   处置时点的累计折旧因此已含当月，处置凭证按它落账天然正确；扫描 SQL 一行不改，
///   批次状态机 / 凭证时序零改动 —— 最小改动 + 不动账务口径。
/// * **方案 ②（未采纳）**：扫描要纳入 `DISPOSED` 且 `disposal_date >= 期初` 的卡片，
///   并让处置凭证延后到期末折旧后再出。它把「处置凭证的时点」绑到「批次什么时候跑」上：
///   反结账 / 红冲 / 重提都会让凭证日期与审批通过日脱钩，等于重定义会计口径（需财务确认）。
///
/// 闸门放在**提交处置**（`asset_handler::asset_dispose`）这一步：fail fast、在审批之前，
/// 不放审批落定后的执行步 —— 那时审批已通过，半路 409 只会把处置单卡在中间态。
///
/// # 三种放行 / 两种拦截
///
/// | 情形 | 判定 |
/// |---|---|
/// | 当月购入当月处置（起提日 = 处置月的**次月** 1 日 ⇒ 处置月 < 起提月） | 放行 [`DisposalDepGate::NotRequired`] |
/// | 已提足（`accumulated >= depreciable_base`，提足即停） | 放行 [`DisposalDepGate::NotRequired`] |
/// | 处置当月有 `POSTED` 明细 | 放行 [`DisposalDepGate::Posted`] |
/// | 处置当月没有明细 | 拦截 [`DepGateMiss::NoDetail`] |
/// | 处置当月的明细是 `REVERSED` | 拦截 [`DepGateMiss::Reversed`] |
pub fn disposal_dep_gate(
    // 处置**当月**的会计期间编码（`yyyy-MM`，由 `period_of(disposal_date)` 得到）
    disposal_period: &str,
    // 卡片起提日（购入次月 1 日，见 [`start_depreciation_date`]）
    start_depreciation_date: NaiveDate,
    // 卡片可折旧总额
    depreciable_base: Decimal,
    // 卡片累计折旧（处置时点的缓存值）
    accumulated_depreciation: Decimal,
    // `biz_depreciation.status`：`None` = 这一期没有该资产的明细行
    detail_status: Option<&str>,
) -> DisposalDepGate {
    // ① 当月购入当月处置：`period_code` 是零填充的 `yyyy-MM`，字典序 = 时间序
    if disposal_period < period_of(start_depreciation_date).as_str() {
        return DisposalDepGate::NotRequired(DepGateSkip::BoughtThisMonth);
    }
    // ② 已提足：提足即停，本月没有明细是**正常**的（扫描 SQL 也不会命中）
    if accumulated_depreciation >= depreciable_base {
        return DisposalDepGate::NotRequired(DepGateSkip::FullyDepreciated);
    }
    match detail_status {
        Some("POSTED") => DisposalDepGate::Posted,
        Some("REVERSED") => DisposalDepGate::Missing(DepGateMiss::Reversed),
        // `None` = 没有明细；`biz_depreciation.status` 的取值域只有 POSTED / REVERSED
        // （V0230 `ck_biz_dep_status`），其余值按「未过账」处理（宁可拦，不放行算错的账）
        _ => DisposalDepGate::Missing(DepGateMiss::NoDetail),
    }
}

// ==================== 处置分录（ARCH-6 §5.6.4） ====================

/// 处置方式（与 `ck_biz_fa_disposal` 逐字一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisposalKind {
    /// 出售（有售价 + 销项税）。
    Sale,
    /// 报废（无收入）。
    Scrap,
    /// 盘亏（先挂待处理，批准后转损失）。
    Loss,
}

impl DisposalKind {
    /// 落库值。
    pub const fn as_str(self) -> &'static str {
        match self {
            DisposalKind::Sale => "SALE",
            DisposalKind::Scrap => "SCRAP",
            DisposalKind::Loss => "LOSS",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            DisposalKind::Sale => "出售",
            DisposalKind::Scrap => "报废",
            DisposalKind::Loss => "盘亏",
        }
    }

    /// 字符串 → 处置方式（严格全等；`None` = 非法取值，调用方 400）。
    pub fn parse(raw: &str) -> Option<DisposalKind> {
        match raw.trim() {
            "SALE" => Some(DisposalKind::Sale),
            "SCRAP" => Some(DisposalKind::Scrap),
            "LOSS" => Some(DisposalKind::Loss),
            _ => None,
        }
    }

    /// 本处置方式要生成几张凭证（ARCH-6 §5.6.3）。
    pub const fn voucher_count(self) -> usize {
        match self {
            DisposalKind::Sale => 3,
            DisposalKind::Scrap | DisposalKind::Loss => 2,
        }
    }
}

/// 处置要用到的科目（全部按编码从 `biz_account` 取，**本卡不代建科目**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposalAccounts {
    /// 1601 固定资产。
    pub asset: AccountRef,
    /// 1602 累计折旧。
    pub accum: AccountRef,
    /// 1606 固定资产清理。
    pub clearing: AccountRef,
    /// 1002 银行存款（出售收款）。
    pub cash: AccountRef,
    /// 2221.02 应交税费-销项税（出售）。
    pub output_tax: AccountRef,
    /// 5711 营业外支出（处置净损失）。
    pub loss: AccountRef,
    /// 5301 营业外收入（处置净收益）。
    pub income: AccountRef,
    /// 1901 待处理财产损溢（盘亏先挂这里）。
    pub pending: AccountRef,
}

/// 处置的金额口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisposalAmounts {
    /// 原值。
    pub original: Decimal,
    /// 累计折旧（处置时点）。
    pub accumulated: Decimal,
    /// 净值 = 原值 − 累计折旧（**不清零**，它是计算处置损益的基数）。
    pub net_value: Decimal,
    /// 出售价（不含税；`SALE` 必填，其余必须为 0）。
    pub sale_price: Decimal,
    /// 销项税（`SALE` 可填；其余必须为 0）。
    pub output_tax: Decimal,
}

impl DisposalAmounts {
    /// 出售实收 = 售价 + 销项税。
    pub fn cash_in(&self) -> Decimal {
        money(self.sale_price + self.output_tax)
    }

    /// 处置损益 = 售价 − 净值（正数 = 收益，负数 = 损失）。
    pub fn gain(&self) -> Decimal {
        money(self.sale_price - self.net_value)
    }
}

/// 一张待生成的处置凭证（摘要后缀 + 分录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposalVoucher {
    /// 摘要后缀（`处置` / `收款` / `结转清理`）。
    pub label: &'static str,
    /// 分录（借贷方向已定，金额为正）。
    pub lines: Vec<ExplicitLine>,
}

/// 按 ARCH-6 §5.6.4 生成处置凭证的分录（**纯函数**，单测钉借贷方向）。
///
/// ```text
/// ① 出售（原值 100000 / 累计 40000 / 净值 60000 / 售价 50000 / 销项税 6500）
///    凭证 1 处置：借 1602 40000 + 借 1606 60000 / 贷 1601 100000
///    凭证 2 收款：借 1002 56500 / 贷 1606 50000 + 贷 2221.02 6500
///    凭证 3 结转：借 5711 10000 / 贷 1606 10000        （净损失 60000−50000）
///    ⇒ 1606 = 60000(借) − 50000(贷) − 10000(贷) = 0   （巡检 K7）
/// ② 报废：凭证 1 同上；凭证 2 结转 借 5711 60000 / 贷 1606 60000
/// ③ 盘亏：凭证 1 借 1602 40000 + 借 1901 60000 / 贷 1601 100000
///          凭证 2 借 5711 60000 / 贷 1901 60000      （1901 归零）
/// ```
pub fn disposal_vouchers(
    kind: DisposalKind,
    accounts: &DisposalAccounts,
    amounts: &DisposalAmounts,
) -> Vec<DisposalVoucher> {
    // 凭证 1：把资产从账上清掉（三种处置共用形状，只有中间科目不同）
    let mut clear_lines = Vec::with_capacity(3);
    if amounts.accumulated > Decimal::ZERO {
        clear_lines.push(ExplicitLine::debit(accounts.accum.clone(), amounts.accumulated));
    }
    let mut vouchers = Vec::with_capacity(kind.voucher_count());
    match kind {
        DisposalKind::Sale | DisposalKind::Scrap => {
            clear_lines.push(ExplicitLine::debit(accounts.clearing.clone(), amounts.net_value));
            clear_lines.push(ExplicitLine::credit(accounts.asset.clone(), amounts.original));
        }
        DisposalKind::Loss => {
            clear_lines.push(ExplicitLine::debit(accounts.pending.clone(), amounts.net_value));
            clear_lines.push(ExplicitLine::credit(accounts.asset.clone(), amounts.original));
        }
    }
    vouchers.push(DisposalVoucher {
        label: "处置",
        lines: clear_lines,
    });

    match kind {
        DisposalKind::Sale => {
            // 凭证 2：收款与销项税
            let mut cash_lines = vec![ExplicitLine::debit(accounts.cash.clone(), amounts.cash_in())];
            if amounts.sale_price > Decimal::ZERO {
                cash_lines.push(ExplicitLine::credit(accounts.clearing.clone(), amounts.sale_price));
            }
            if amounts.output_tax > Decimal::ZERO {
                cash_lines.push(ExplicitLine::credit(accounts.output_tax.clone(), amounts.output_tax));
            }
            vouchers.push(DisposalVoucher {
                label: "收款",
                lines: cash_lines,
            });
            // 凭证 3：结转清理净额（1606 必须归零）
            let gain = amounts.gain();
            let mut settle = Vec::with_capacity(2);
            if gain < Decimal::ZERO {
                settle.push(ExplicitLine::debit(accounts.loss.clone(), -gain));
                settle.push(ExplicitLine::credit(accounts.clearing.clone(), -gain));
            } else if gain > Decimal::ZERO {
                settle.push(ExplicitLine::debit(accounts.clearing.clone(), gain));
                settle.push(ExplicitLine::credit(accounts.income.clone(), gain));
            }
            // 收益为 0 时不生成第三张（借贷合计为 0 不构成凭证）——
            // 此时 1606 已经在凭证 1 与凭证 2 里刚好对冲为 0。
            if !settle.is_empty() {
                vouchers.push(DisposalVoucher {
                    label: "结转清理",
                    lines: settle,
                });
            }
        }
        DisposalKind::Scrap => {
            // 报废无收入：直接把清理净额转营业外支出
            let mut settle = Vec::with_capacity(2);
            if amounts.net_value > Decimal::ZERO {
                settle.push(ExplicitLine::debit(accounts.loss.clone(), amounts.net_value));
                settle.push(ExplicitLine::credit(accounts.clearing.clone(), amounts.net_value));
                vouchers.push(DisposalVoucher {
                    label: "结转清理",
                    lines: settle,
                });
            }
        }
        DisposalKind::Loss => {
            // 盘亏：批准后从待处理转损失（1901 必须归零）
            let mut settle = Vec::with_capacity(2);
            if amounts.net_value > Decimal::ZERO {
                settle.push(ExplicitLine::debit(accounts.loss.clone(), amounts.net_value));
                settle.push(ExplicitLine::credit(accounts.pending.clone(), amounts.net_value));
                vouchers.push(DisposalVoucher {
                    label: "结转损失",
                    lines: settle,
                });
            }
        }
    }
    vouchers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 金额字面量（`dec!(1583.3333)`）—— 本仓没有引 `rust_decimal_macros`，
    /// 用 `stringify!` + `FromStr` 自己造一个：定点四位，不做 f64 中转。
    macro_rules! dec {
        ($v:literal) => {{
            use std::str::FromStr as _;
            Decimal::from_str(stringify!($v)).expect("金额字面量非法")
        }};
    }

    fn card(
        original: Decimal,
        residual: Decimal,
        months: i32,
        accumulated: Decimal,
        months_done: i32,
    ) -> DepreciationCard {
        let base = depreciable_base(original, residual);
        DepreciationCard {
            original_value: original,
            residual_value: residual,
            depreciable_base: base,
            monthly_depreciation: monthly_depreciation(base, months),
            useful_life_months: months,
            accumulated_depreciation: accumulated,
            months_done,
        }
    }

    /// ① ARCH-6 §5.4.3 算例：100000 / 5% / 60 月 → 月折旧 1583.3333，尾差在第 60 月补齐。
    #[test]
    fn straight_line_matches_design_example() {
        let residual = residual_value(dec!(100000), dec!(0.05));
        assert_eq!(residual, dec!(5000.0000), "残值 = 原值 × 残值率");
        let base = depreciable_base(dec!(100000), residual);
        assert_eq!(base, dec!(95000.0000));
        assert_eq!(
            monthly_depreciation(base, 60),
            dec!(1583.3333),
            "95000 / 60 = 1583.3333（ROUND_HALF_UP 四位）"
        );

        // 第 59 月：累计 1583.3333 × 59 = 93416.6647，本月仍是固定额
        let at_59 = card(dec!(100000), dec!(5000), 60, dec!(93416.6647), 58);
        assert_eq!(month_amount(&at_59), Some(dec!(1583.3333)));
        assert!(!is_last_month(&at_59));

        // 第 60 月：尾差补齐 1583.3353
        let at_60 = card(dec!(100000), dec!(5000), 60, dec!(95000.0000) - dec!(1583.3353), 59);
        assert_eq!(month_amount(&at_60), Some(dec!(1583.3353)));
        assert!(is_last_month(&at_60));

        // 一分不差：93,416.6647 + 1,583.3353 = 95,000.0000
        assert_eq!(
            dec!(93416.6647) + dec!(1583.3353),
            dec!(95000.0000),
            "60 个月合计必须等于可折旧总额"
        );
    }

    /// ② 整除无尾差：90000 / 60 = 1500 整，最后一个月**不加**尾差。
    #[test]
    fn exact_division_has_no_tail() {
        let base = dec!(90000);
        assert_eq!(monthly_depreciation(base, 60), dec!(1500));
        let last = card(dec!(95000), dec!(5000), 60, dec!(88500), 59);
        assert_eq!(month_amount(&last), Some(dec!(1500.0000)));
    }

    /// ③ 中途处置：第 30 月仍计提，第 31 月起不再提（30 × 1583.3333 = 47499.9990）。
    ///
    /// 设计 §5.4.3 把这个数写成「累计 = 47,500.0000」——那是**四舍五入到元的表述**；
    /// 定点四位下的真值是 47,499.9990（59 个月后由尾差补齐到 95,000）。单测按真值钉，
    /// 免得后来有人拿「和设计文档差 1 分」当成 bug 改坏尾差规则。
    #[test]
    fn disposed_in_month_30_still_depreciates() {
        let mut c = card(dec!(100000), dec!(5000), 60, dec!(45916.6657), 29);
        let amount = month_amount(&c).expect("第 30 月仍应计提（当月减少当月照提）");
        assert_eq!(amount, dec!(1583.3333));
        c.accumulated_depreciation += amount;
        c.months_done += 1;
        assert_eq!(c.accumulated_depreciation, dec!(47499.9990));
        assert_eq!(
            c.original_value - c.accumulated_depreciation,
            dec!(52500.0010),
            "第 30 月末净值（≈ 设计写的 52,500）"
        );
        // 提足即停
        let full = card(dec!(100000), dec!(5000), 60, dec!(95000), 60);
        assert_eq!(month_amount(&full), None, "提足即停");
    }

    /// ④ 提足即停：累计 = 可折旧总额 → 不再计提（净值 = 残值）。
    #[test]
    fn stops_when_fully_depreciated() {
        let c = card(dec!(100000), dec!(5000), 60, dec!(95000), 60);
        assert_eq!(month_amount(&c), None);
        assert_eq!(c.original_value - c.accumulated_depreciation, dec!(5000));
    }

    /// ⑤ 起提日 = 购入次月 1 日（跨年也要对）。
    #[test]
    fn start_date_is_next_month_first_day() {
        assert_eq!(
            start_depreciation_date(NaiveDate::from_ymd_opt(2026, 11, 20).unwrap()),
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()
        );
        assert_eq!(
            start_depreciation_date(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
            "12 月跨年"
        );
    }

    /// ⑥ 期间工具：首末日 / 上期 / 合法性。
    #[test]
    fn period_helpers() {
        let (start, end) = period_range("2026-02").expect("合法期间");
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        assert_eq!(end, NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
        let (_, leap_end) = period_range("2028-02").expect("合法期间");
        assert_eq!(leap_end, NaiveDate::from_ymd_opt(2028, 2, 29).unwrap(), "闰年");
        assert_eq!(prev_period("2026-01").as_deref(), Some("2025-12"));
        assert_eq!(prev_period("2026-11").as_deref(), Some("2026-10"));
        assert!(!is_valid_period("2026-13"));
        assert!(!is_valid_period("2026-1"));
        assert!(period_range("2026-13").is_none());
        assert_eq!(period_of(NaiveDate::from_ymd_opt(2026, 3, 9).unwrap()), "2026-03");
    }

    fn accounts() -> DisposalAccounts {
        let acc = |code: &str| AccountRef {
            id: nik::uuid::Uuid::now_v7(),
            code: code.to_string(),
            name: code.to_string(),
        };
        DisposalAccounts {
            asset: acc("1601"),
            accum: acc("1602"),
            clearing: acc("1606"),
            cash: acc("1002"),
            output_tax: acc("2221.02"),
            loss: acc("5711"),
            income: acc("5301"),
            pending: acc("1901"),
        }
    }

    fn amounts() -> DisposalAmounts {
        DisposalAmounts {
            original: dec!(100000),
            accumulated: dec!(40000),
            net_value: dec!(60000),
            sale_price: dec!(50000),
            output_tax: dec!(6500),
        }
    }

    fn sums(voucher: &DisposalVoucher) -> (Decimal, Decimal) {
        let debit: Decimal = voucher.lines.iter().map(|line| line.debit).sum();
        let credit: Decimal = voucher.lines.iter().map(|line| line.credit).sum();
        (debit, credit)
    }

    /// ⑦ 出售：三张凭证，借贷各自平衡，1606 归零（巡检 K7 的口径）。
    #[test]
    fn sale_vouchers_balance_and_clear_1606() {
        let accounts = accounts();
        let amounts = amounts();
        let vouchers = disposal_vouchers(DisposalKind::Sale, &accounts, &amounts);
        assert_eq!(vouchers.len(), 3, "出售 = 处置 + 收款 + 结转清理");

        for voucher in &vouchers {
            let (debit, credit) = sums(voucher);
            assert_eq!(debit, credit, "凭证「{}」借贷不平衡", voucher.label);
        }

        // 凭证 1：借 1602 40000 / 借 1606 60000 / 贷 1601 100000
        assert_eq!(vouchers[0].lines.len(), 3);
        assert_eq!(vouchers[0].lines[0].account.code, "1602");
        assert_eq!(vouchers[0].lines[0].debit, dec!(40000));
        assert_eq!(vouchers[0].lines[1].account.code, "1606");
        assert_eq!(vouchers[0].lines[1].debit, dec!(60000));
        assert_eq!(vouchers[0].lines[2].account.code, "1601");
        assert_eq!(vouchers[0].lines[2].credit, dec!(100000));

        // 凭证 2：借 1002 56500 / 贷 1606 50000 / 贷 2221.02 6500
        assert_eq!(vouchers[1].lines[0].account.code, "1002");
        assert_eq!(vouchers[1].lines[0].debit, dec!(56500));
        assert_eq!(vouchers[1].lines[1].credit, dec!(50000));
        assert_eq!(vouchers[1].lines[2].account.code, "2221.02");
        assert_eq!(vouchers[1].lines[2].credit, dec!(6500));

        // 凭证 3：借 5711 10000 / 贷 1606 10000
        assert_eq!(vouchers[2].lines[0].account.code, "5711");
        assert_eq!(vouchers[2].lines[0].debit, dec!(10000));
        assert_eq!(vouchers[2].lines[1].credit, dec!(10000));

        // 1606 净额 = 借 60000 − 贷 50000 − 贷 10000 = 0
        let clearing: Decimal = vouchers
            .iter()
            .flat_map(|voucher| voucher.lines.iter())
            .filter(|line| line.account.code == "1606")
            .map(|line| line.debit - line.credit)
            .sum();
        assert_eq!(clearing, Decimal::ZERO, "处置完成后 1606 必须归零（K7）");
    }

    /// ⑧ 报废：两张凭证，清理净额全额转营业外支出。
    #[test]
    fn scrap_vouchers_move_net_value_to_loss() {
        let accounts = accounts();
        let mut amounts = amounts();
        amounts.sale_price = Decimal::ZERO;
        amounts.output_tax = Decimal::ZERO;
        let vouchers = disposal_vouchers(DisposalKind::Scrap, &accounts, &amounts);
        assert_eq!(vouchers.len(), 2, "报废 = 处置 + 结转损失");
        for voucher in &vouchers {
            let (debit, credit) = sums(voucher);
            assert_eq!(debit, credit);
        }
        assert_eq!(vouchers[1].lines[0].account.code, "5711");
        assert_eq!(vouchers[1].lines[0].debit, dec!(60000));
        assert_eq!(vouchers[1].lines[1].account.code, "1606");
        assert_eq!(vouchers[1].lines[1].credit, dec!(60000));
        let clearing: Decimal = vouchers
            .iter()
            .flat_map(|voucher| voucher.lines.iter())
            .filter(|line| line.account.code == "1606")
            .map(|line| line.debit - line.credit)
            .sum();
        assert_eq!(clearing, Decimal::ZERO);
    }

    /// ⑨ 盘亏：先挂 1901，批准后转 5711，1901 归零（与报废是两条路径）。
    #[test]
    fn loss_vouchers_go_through_pending_account() {
        let accounts = accounts();
        let mut amounts = amounts();
        amounts.sale_price = Decimal::ZERO;
        amounts.output_tax = Decimal::ZERO;
        let vouchers = disposal_vouchers(DisposalKind::Loss, &accounts, &amounts);
        assert_eq!(vouchers.len(), 2);
        assert_eq!(vouchers[0].lines[1].account.code, "1901");
        assert_eq!(vouchers[0].lines[1].debit, dec!(60000));
        assert_eq!(vouchers[1].lines[0].account.code, "5711");
        assert_eq!(vouchers[1].lines[1].account.code, "1901");
        let pending: Decimal = vouchers
            .iter()
            .flat_map(|voucher| voucher.lines.iter())
            .filter(|line| line.account.code == "1901")
            .map(|line| line.debit - line.credit)
            .sum();
        assert_eq!(pending, Decimal::ZERO, "盘亏结转后 1901 必须归零");
    }

    /// ⑩ 出售价高于净值 → 收益进 5301（不是负数的 5711）。
    #[test]
    fn sale_gain_goes_to_other_income() {
        let accounts = accounts();
        let mut amounts = amounts();
        amounts.sale_price = dec!(70000);
        amounts.output_tax = dec!(9100);
        let vouchers = disposal_vouchers(DisposalKind::Sale, &accounts, &amounts);
        assert_eq!(vouchers.len(), 3);
        assert_eq!(vouchers[2].lines[0].account.code, "1606");
        assert_eq!(vouchers[2].lines[0].debit, dec!(10000));
        assert_eq!(vouchers[2].lines[1].account.code, "5301");
        assert_eq!(vouchers[2].lines[1].credit, dec!(10000));
    }

    /// ⑪ 处置方式取值域只有三个（与 `ck_biz_fa_disposal` 一致，不许自己加）。
    #[test]
    fn disposal_kind_domain_is_three_values() {
        assert_eq!(DisposalKind::parse("SALE"), Some(DisposalKind::Sale));
        assert_eq!(DisposalKind::parse("SCRAP"), Some(DisposalKind::Scrap));
        assert_eq!(DisposalKind::parse("LOSS"), Some(DisposalKind::Loss));
        assert_eq!(DisposalKind::parse("DONATE"), None, "捐赠不在本期取值域");
        assert_eq!(DisposalKind::parse("sale"), None, "严格全等，不做归一");
    }

    // ==================== 处置闸（FIX-3 方案 ①） ====================

    /// 一台 120000 / 残值 6000 / 60 月资产的起提日（购入 2025-12-15 → 起提 2026-01-01）。
    fn start_of(purchase: &str) -> NaiveDate {
        start_depreciation_date(NaiveDate::parse_from_str(purchase, "%Y-%m-%d").expect("日期"))
    }

    /// 闸门：**处置当月没有明细** ⇒ 拦（这就是「6 月处置、6 月批次里没有它」那条漏提的入口）。
    #[test]
    fn gate_blocks_when_disposal_month_has_no_detail() {
        let gate = disposal_dep_gate(
            "2026-06",
            start_of("2025-12-15"),
            dec!(114000),
            dec!(9500), // 已提 5 个月
            None,
        );
        assert_eq!(gate, DisposalDepGate::Missing(DepGateMiss::NoDetail));
        assert!(gate.is_blocked(), "没有当月折旧明细时必须拦下处置");
    }

    /// 闸门：**已计提并过账** ⇒ 放行（正常路径：先计提当月，再提交处置）。
    #[test]
    fn gate_passes_when_disposal_month_detail_posted() {
        let gate = disposal_dep_gate(
            "2026-06",
            start_of("2025-12-15"),
            dec!(114000),
            dec!(9500),
            Some("POSTED"),
        );
        assert_eq!(gate, DisposalDepGate::Posted);
        assert!(!gate.is_blocked());
    }

    /// 闸门：**明细被红冲** ⇒ 拦（那一个月的折旧等于没提，不能按「有行」放行）。
    #[test]
    fn gate_blocks_when_detail_reversed() {
        let gate = disposal_dep_gate(
            "2026-06",
            start_of("2025-12-15"),
            dec!(114000),
            dec!(9500),
            Some("REVERSED"),
        );
        assert_eq!(gate, DisposalDepGate::Missing(DepGateMiss::Reversed));
        assert!(gate.is_blocked());
        assert!(
            DepGateMiss::Reversed.label().contains("红冲"),
            "报文要说清是「红冲过」，不能与「没计提」混成一句话"
        );
    }

    /// 闸门：**当月购入当月处置** ⇒ 放行（铁律①：本月本来不提，没有明细才是对的）。
    ///
    /// 这是卡面点名的边界：购入 2026-06-10 → 起提 2026-07-01；6 月处置时
    /// `biz_depreciation` 里本来就不会有 2026-06 这一行，不能因此拦下。
    #[test]
    fn gate_skips_asset_bought_and_disposed_in_same_month() {
        let gate = disposal_dep_gate("2026-06", start_of("2026-06-10"), dec!(114000), dec!(0), None);
        assert_eq!(gate, DisposalDepGate::NotRequired(DepGateSkip::BoughtThisMonth));
        assert!(!gate.is_blocked());

        // 跨年边界：2025-12 购入 → 起提 2026-01；2025-12 处置仍是「当月购入当月处置」
        let gate = disposal_dep_gate("2025-12", start_of("2025-12-31"), dec!(114000), dec!(0), None);
        assert_eq!(
            gate,
            DisposalDepGate::NotRequired(DepGateSkip::BoughtThisMonth),
            "跨年时 12 月的起提日是次年 1 月 1 日，期间编码字典序比较仍然成立"
        );

        // 次月处置（2026-01）就不再有豁免：起提月 = 处置月 ⇒ 必须已计提
        let gate = disposal_dep_gate("2026-01", start_of("2025-12-31"), dec!(114000), dec!(0), None);
        assert_eq!(gate, DisposalDepGate::Missing(DepGateMiss::NoDetail));
    }

    /// 闸门：**已提足** ⇒ 放行（提足即停，扫描 SQL 也不再命中，没有明细是正常的）。
    #[test]
    fn gate_skips_fully_depreciated_asset() {
        let gate = disposal_dep_gate(
            "2026-06",
            start_of("2020-01-10"),
            dec!(114000),
            dec!(114000), // 累计 == 可折旧总额
            None,
        );
        assert_eq!(gate, DisposalDepGate::NotRequired(DepGateSkip::FullyDepreciated));
        assert!(!gate.is_blocked());

        // 高一分也算提足（脏数据 / 尾差）；低一分则必须已计提，不许蒙混
        let over = disposal_dep_gate("2026-06", start_of("2020-01-10"), dec!(114000), dec!(114000.0001), None);
        assert!(!over.is_blocked(), "累计超过可折旧总额同样是提足");
        let under = disposal_dep_gate("2026-06", start_of("2020-01-10"), dec!(114000), dec!(113999.9999), None);
        assert!(under.is_blocked(), "差一分也还没提足，当月折旧必须先计提");
    }
}

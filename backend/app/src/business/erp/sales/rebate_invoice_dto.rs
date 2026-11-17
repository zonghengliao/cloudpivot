//! 销售返利 · 红字发票（M3-17）接口契约与纯计算。
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | GET  | `/business/erp/rebate-settlement/{id}/invoice-preview` | `business:erp:rebate-settlement:invoice` |
//! | POST | `/business/erp/rebate-settlement/{id}/invoice`         | `business:erp:rebate-settlement:invoice` |
//!
//! # 本卡做什么（一句话）
//!
//! 一张 **DRAFT** 的返利结算单 → 一张 **销项红字发票（`OUT_RED`）** + 红字凭证，
//! 结算单置 `SETTLED`。卡面「生成应付或红字发票 **二选一**」，本卡交付**红字发票**。
//!
//! # 为什么是红字发票（不是应付单）
//!
//! 返利在会计上是**销售折让**：冲减收入与销项税，同时形成欠客户的负债。
//!
//! * 本仓 `biz_invoice`（V0298）早已把红字票做成一等公民（`OUT_RED` +
//!   `ck_biz_invoice_sign`「amount < 0 且 tax ≤ 0」），退货过账
//!   （`sales_return_posting::create_red_invoice`）已有「程序造 OUT_RED + 收入冲减凭证」
//!   的成熟路径 —— 本卡按同一条路走，不另立一套。
//! * 本仓**没有**「对客户的应付」表：`biz_payable`（V0148）的列是 `supplier_id /
//!   due_date / credit_days`，语义是「应付账款-供应商」。把客户填进 `supplier_id`
//!   等于拿采购台账记客户返利（账龄 / 对账 / 付款审批全认错对象）；为返利新造一张付款台账
//!   则是另一张卡的范围。
//! * 欠客户的钱并不因此失踪：红字凭证贷 `2241.05 其他应付款-销售返利`（V0445 种的末级科目），
//!   财务在总账里能直接看到「还欠客户多少返利」。
//!
//!   > 这个码原来是 `2241.02`（V0440 种的），但 `2241.02` 早被 V0311 的「其他应付款-佣金」
//!   > 占着（V0440 的 `WHERE NOT EXISTS` 种子静默跳过），返利负债实际记到了佣金科目上。
//!   > `V0445__sales_rebate_payable_account` 另种 `2241.05` 收口，见 handler 模块头。
//!
//! # 金额拆分口径（税率是**百分数**，FIX-6 / V0327）
//!
//! 结算单的 `rebate_amount` 是**含税**金额（它由「本位币含税净销售额 × 比率」算出，
//! 与基数同口径），所以红字发票按含税价**反算**：
//!
//! ```text
//! 价税合计 total_with_tax = −rebate_amount                            （负）
//! 不含税   amount         = −round(rebate_amount / (1 + rate/100), 2) （负）
//! 税额     tax_amount     = total_with_tax − amount                   （≤ 0）
//! ```
//!
//! 反算而不是「按不含税开票再单加税」：返利金额是谈出来的**实际到手**数，
//! 开票时把它当含税额拆出不含税与税额，客户拿到的价税合计正好等于谈定的返利。
//!
//! `net.abs() + tax.abs() == total.abs()` 恒等（分位相等）：拆错一分钱
//! 会撞库层 `ck_biz_invoice_amount`（total = amount + tax），但在 handler 里先算清
//! 能给出「哪个数算歪了」的报文，而不是 PG 的 23514。
//!
//! # 凭证（同一事务）
//!
//! ```text
//! 借 5001    主营业务收入       |amount|         冲减收入
//! 借 2221.02 应交税费-销项税     |tax|            冲减销项税（tax = 0 时不出这一行）
//! 贷 2241.05 其他应付款-销售返利 |total_with_tax| 欠客户的返利负债
//! ```
//!
//! 贷方**不是** `1122 应收账款`：返利通常以现款或抵扣下期货款结清，不冲某张具体应收。
//!
//! # 状态机（结算单不新增状态）
//!
//! ```text
//! DRAFT ──开红字发票──▶ SETTLED     （本卡唯一的写路径；SETTLED 不可重算、不可作废）
//! DRAFT ──作废────────▶ CANCELLED  （M3-16）
//! ```
//!
//! 票被作废（`biz_invoice.status = 'VOID'`）**不会**把结算单改回 `DRAFT`：
//! 那属于红冲 / 调账业务，靠「红冲票 + 重新结算」走，不是悄悄回退一张已开过票的单据。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::RoundingStrategy;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::sales::rebate_settle_dto::{STATUS_CANCELLED, STATUS_DRAFT, STATUS_SETTLED, status_label};
use cloudpivot_kernel::shared::error_code;

// ==================== 域值（与 V0440 / biz_invoice 逐字一致，单测钉住） ====================

/// 红字发票票种（`biz_invoice.invoice_type`，V0295/V0298 的 `ck_biz_invoice_type` 只认 OUT / OUT_RED）。
pub const INVOICE_TYPE_OUT_RED: &str = "OUT_RED";
/// 发票来源类型（V0440 把 `SALES_REBATE` 加进 `ck_biz_invoice_source`）。
pub const INVOICE_SOURCE_REBATE: &str = "SALES_REBATE";
/// 红字凭证的来源类型：凭证是「这张发票的记账依据」，与退货红冲同口径（`INVOICE`）。
pub const VOUCHER_SOURCE_INVOICE: &str = "INVOICE";
/// 发票号备注里用的业务名。
pub const INVOICE_TYPE_LABEL: &str = "销项红字票";

/// 默认税率（**百分数**：13 = 13%；与 `biz_invoice.tax_rate` 的库层默认值 13.0000 同值）。
pub const DEFAULT_TAX_RATE_PERCENT: i32 = 13;
/// 税率上界（百分数；`ck_biz_invoice_tax_rate` 是 0~100）。
pub const TAX_RATE_MAX_PERCENT: i32 = 100;

/// 备注上限（与 M3-16 的 `normalize_remark` 同口径，前端一个输入框）。
pub const MAX_REMARK_LEN: usize = 200;

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `409 + 66093`：结算单当前状态不允许开票（已开过 / 已作废）。
fn state_blocked(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_REBATE_INVOICE_STATE, message.into())
}

/// `409 + 66094`：应返金额为 0（没有可开票的金额）。
fn zero_amount(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::ERP_REBATE_INVOICE_AMOUNT, message.into())
}

// ==================== 税率 ====================

/// 默认税率（百分数 `Decimal`）。
pub fn default_tax_rate() -> Decimal {
    Decimal::from(DEFAULT_TAX_RATE_PERCENT)
}

/// 校验税率：**百分数** 0~100，最多 4 位小数（`NUMERIC(7,4)`）。
///
/// 收到 `0.13` 这种小数标度不静默 ×100 —— 与 `invoice::tax_rate` 的过渡期规则不同，
/// 本卡是**新接口**，没有历史包袱：0.13% 与 13% 差 100 倍，猜错就是错账，宁可 400。
pub fn validate_tax_rate(raw: Option<Decimal>) -> std::result::Result<Decimal, ApiError> {
    let Some(rate) = raw else {
        return Ok(default_tax_rate());
    };
    if rate < Decimal::ZERO || rate > Decimal::from(TAX_RATE_MAX_PERCENT) {
        return Err(bad_request(format!(
            "tax_rate 是百分数（13 = 13%），取值 0~{TAX_RATE_MAX_PERCENT}（收到 {rate}）"
        )));
    }
    if rate.scale() > 4 {
        return Err(bad_request(format!(
            "tax_rate 最多 4 位小数（库列是 NUMERIC(7,4)，收到 {rate}）"
        )));
    }
    Ok(rate)
}

// ==================== 票面拆分（纯函数：单测的主力） ====================

/// 红字发票的票面三列（**全为负数**，与 `ck_biz_invoice_sign` 同号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvoiceSplit {
    /// 不含税金额（负）。
    pub amount: Decimal,
    /// 税额（≤ 0）。
    pub tax_amount: Decimal,
    /// 价税合计（负）。
    pub total_with_tax: Decimal,
}

impl InvoiceSplit {
    /// 分录金额（绝对值）：借方收入 / 借方销项税 / 贷方其他应付款。
    pub fn debit_revenue(&self) -> Decimal {
        self.amount.abs()
    }

    /// 借方销项税（绝对值；0 表示不出这一行）。
    pub fn debit_tax(&self) -> Decimal {
        self.tax_amount.abs()
    }

    /// 贷方其他应付款-销售返利（绝对值）。
    pub fn credit_payable(&self) -> Decimal {
        self.total_with_tax.abs()
    }

    /// 分录是否借贷平衡（借方 = 收入 + 税；贷方 = 价税合计）。
    ///
    /// 两个等号都要成立：`net + tax == total`（拆分自洽）且 `|net| + |tax| == |total|`
    /// （符号同向，没有一行被算成反向）。
    pub fn balanced(&self) -> bool {
        self.total_with_tax == self.amount + self.tax_amount
            && self.credit_payable() == self.debit_revenue() + self.debit_tax()
    }
}

/// 把**含税**应返金额拆成红字发票三列（分位：2 位小数）。
///
/// * `rebate_amount` 必须 **> 0**（0 或负数没有红字票可言：`ck_biz_invoice_sign`
///   要求 `amount < 0` 严格小于，0 元发票也无业务含义）；
/// * `tax_rate` 是百分数（13 = 13%），已在 [`validate_tax_rate`] 校验过；
/// * 拆分口径：`net = round(total / (1 + rate/100), 2)`，`tax = total − net`。
///   税额由**减法**得出而不是各自四舍五入 —— 两个数各自 round 会在分位上差一分，
///   撞 `ck_biz_invoice_amount`（total = amount + tax）。
pub fn split_invoice(rebate_amount: Decimal, tax_rate: Decimal) -> std::result::Result<InvoiceSplit, ApiError> {
    let rebate = rebate_amount.round_dp(2);
    if rebate <= Decimal::ZERO {
        return Err(zero_amount(format!(
            "应返金额 {rebate_amount} 不大于 0：没有可开红字发票的金额（0 元票无业务含义，\
             负返利是红冲 / 调账业务）"
        )));
    }
    if rebate > Decimal::from(10_000_000_000_000_i64) {
        return Err(bad_request(format!(
            "应返金额 {rebate} 超出 NUMERIC(18,4) 能表达的范围（整数上限 14 位）"
        )));
    }
    let rate = validate_tax_rate(Some(tax_rate))?;
    // net = round(rebate / (1 + rate/100), 2)，四舍五入（背离零）后再取负
    let divisor = Decimal::ONE + rate / Decimal::from(100);
    let net = (rebate / divisor).round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    let amount = -net;
    let total_with_tax = -rebate;
    let tax_amount = total_with_tax - amount;
    let split = InvoiceSplit {
        amount,
        tax_amount,
        total_with_tax,
    };
    // 三条库层约束的**前置**自检：算歪了在这里就报 400，而不是让 PG 抛 23514（只有约束名）
    if amount >= Decimal::ZERO {
        return Err(bad_request(format!(
            "红字票不含税金额必须 < 0（ARCH-7 §15.2.3）：应返 {rebate} × 税率 {rate}% 拆出 {amount}"
        )));
    }
    if tax_amount > Decimal::ZERO {
        return Err(bad_request(format!(
            "红字票税额必须 ≤ 0：应返 {rebate} 拆出不含税 {amount} / 税额 {tax_amount}"
        )));
    }
    if !split.balanced() {
        return Err(bad_request(format!(
            "红字票三列不自洽：不含税 {amount} + 税额 {tax_amount} ≠ 价税合计 {total_with_tax}"
        )));
    }
    Ok(split)
}

// ==================== 入参 ====================

/// 开红字发票的入参（三个都可省：省了就是「今天 / 13% / 无备注」）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RebateInvoiceReq {
    /// 开票日期（省 = 服务端当天，CST 口径的日期由调用方给；不早于账套开账日期由期间闸管）。
    pub invoice_date: Option<NaiveDate>,
    /// 税率（百分数；省 = 13）。
    pub tax_rate: Option<Decimal>,
    /// 备注（省 = 自动生成「返利结算 RB-… 红冲」）。
    pub remark: Option<String>,
}

/// 备注归一化：空白 → `None`；超长 → 400。
pub fn normalize_remark(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > MAX_REMARK_LEN {
        return Err(bad_request(format!(
            "remark 最长 {MAX_REMARK_LEN} 字（当前 {} 字）",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

// ==================== 出参 ====================

/// 一张已开出的红字发票（详情 / 预览的 `existing_invoice` 共用）。
#[derive(Debug, Clone, Serialize)]
pub struct RebateInvoiceRes {
    /// 结算单 id。
    pub settlement_id: String,
    /// 结算单号。
    pub settlement_no: String,
    /// 结算单状态（`SETTLED`）。
    pub settlement_status: String,
    /// 结算单状态中文。
    pub settlement_status_label: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码。
    pub customer_code: Option<String>,
    /// 客户名称（join 实时名：返利票要显示「现在叫什么」）。
    pub customer_name: Option<String>,
    /// 结算粒度。
    pub settle_period: String,
    /// 期间码。
    pub period_code: String,
    /// 应返金额（含税，正数；结算单原值）。
    pub rebate_amount: Decimal,
    /// 红字发票 id。
    pub invoice_id: String,
    /// 红字发票号（`IV-yyyyMMdd-000001`）。
    pub invoice_no: String,
    /// 票种（`OUT_RED`）。
    pub invoice_type: String,
    /// 票种中文。
    pub invoice_type_label: String,
    /// 开票日期。
    pub invoice_date: NaiveDate,
    /// 不含税金额（负）。
    pub invoice_amount: Decimal,
    /// 税额（≤ 0）。
    pub invoice_tax: Decimal,
    /// 价税合计（负 = 冲减的含税金额）。
    pub invoice_total: Decimal,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 币种（本位币）。
    pub currency_code: String,
    /// 发票状态（`ISSUED` / 后续可能被作废成 `VOID`）。
    pub invoice_status: String,
    /// 红字凭证 id。
    pub voucher_id: Option<String>,
    /// 红字凭证号。
    pub voucher_no: Option<String>,
    /// 转结时间。
    pub settled_at: Option<DateTime<Utc>>,
}

/// 凭证分录预览（借 / 贷各一行；不落库，给财务「先看一眼再开票」）。
#[derive(Debug, Clone, Serialize)]
pub struct VoucherLineRes {
    /// 借方或贷方（`DEBIT` / `CREDIT`）。
    pub side: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 金额（正数，绝对值）。
    pub amount: Decimal,
    /// 摘要。
    pub summary: String,
}

/// 试算 / 预览结果（**不落库**）。
#[derive(Debug, Clone, Serialize)]
pub struct RebateInvoicePreviewRes {
    /// 结算单 id。
    pub settlement_id: String,
    /// 结算单号。
    pub settlement_no: String,
    /// 结算单当前状态。
    pub settlement_status: String,
    /// 结算单状态中文。
    pub settlement_status_label: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 结算粒度。
    pub settle_period: String,
    /// 期间码。
    pub period_code: String,
    /// 应返金额（含税）。
    pub rebate_amount: Decimal,
    /// 将采用的税率（百分数）。
    pub tax_rate: Decimal,
    /// 票种（恒 `OUT_RED`）。
    pub invoice_type: String,
    /// 票种中文。
    pub invoice_type_label: String,
    /// 将采用的开票日期。
    pub invoice_date: NaiveDate,
    /// 币种（本位币）。
    pub currency_code: String,
    /// 不含税金额（负）。
    pub invoice_amount: Decimal,
    /// 税额（≤ 0）。
    pub invoice_tax: Decimal,
    /// 价税合计（负）。
    pub invoice_total: Decimal,
    /// 能否开票（`false` 时看 `blockers`）。
    pub can_generate: bool,
    /// 拦下来的原因（人话；每条都是一个可修的动作）。
    pub blockers: Vec<String>,
    /// 提醒（不拦，例如税率与上期不一致）。
    pub warnings: Vec<String>,
    /// 凭证分录预览。
    pub voucher_lines: Vec<VoucherLineRes>,
    /// 已开出的发票（`SETTLED` 时回显；`DRAFT` / `CANCELLED` 为 `None`）。
    pub existing_invoice: Option<RebateInvoiceRes>,
}

// ==================== 状态闸（纯函数） ====================

/// 结算单是否允许开红字发票：只有 `DRAFT` 能转。
///
/// `SETTLED` → 409 + 66093（已经开过，重复开票 = 重复付钱 / 重复冲收入）；
/// `CANCELLED` → 409 + 66093（作废单不产生返利）。
pub fn ensure_invoiceable(status: &str) -> std::result::Result<(), ApiError> {
    match status {
        STATUS_DRAFT => Ok(()),
        STATUS_SETTLED => Err(state_blocked(
            "该结算单已开过红字发票（SETTLED）：不能重复开票 —— 重复开票会重复冲减收入与销项税。\
             要调整请按红冲 / 调账业务处理（先红冲原票，再重新结算）",
        )),
        STATUS_CANCELLED => Err(state_blocked(
            "该结算单已作废（CANCELLED）：作废单不产生返利，不能开红字发票。\
             确实要给这个期间返利，请重新计算一张结算单",
        )),
        other => Err(state_blocked(format!(
            "结算单状态「{other}」不允许开红字发票（只认 {}）",
            STATUS_DRAFT
        ))),
    }
}

/// 结算单状态的展示标签（与 M3-16 同一个函数，不另立一套）。
pub fn settlement_status_label(status: &str) -> &'static str {
    status_label(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    /// 域值与迁移 / biz_invoice 的约束逐字一致（改一个字母就是开不出票）。
    #[test]
    fn domain_values_match_schema() {
        assert_eq!(INVOICE_TYPE_OUT_RED, "OUT_RED");
        assert_eq!(INVOICE_SOURCE_REBATE, "SALES_REBATE");
        assert_eq!(VOUCHER_SOURCE_INVOICE, "INVOICE");
        assert_eq!(INVOICE_TYPE_LABEL, "销项红字票");
        assert_eq!(DEFAULT_TAX_RATE_PERCENT, 13);
        assert_eq!(TAX_RATE_MAX_PERCENT, 100);
        println!("[M3-17] 域值 OUT_RED / SALES_REBATE / INVOICE / 13% ✓");
    }

    /// 含税 1000 按 13% 拆：884.96 + 115.04 = 1000，三列全负。
    #[test]
    fn split_1000_at_13_percent() {
        let split = split_invoice(dec("1000"), dec("13")).expect("正常拆分");
        assert_eq!(split.total_with_tax, dec("-1000"));
        assert_eq!(split.amount, dec("-884.96"));
        assert_eq!(split.tax_amount, dec("-115.04"));
        assert!(split.amount < Decimal::ZERO && split.tax_amount <= Decimal::ZERO);
        assert!(split.balanced(), "借贷必须平衡");
        assert_eq!(split.debit_revenue(), dec("884.96"));
        assert_eq!(split.debit_tax(), dec("115.04"));
        assert_eq!(split.credit_payable(), dec("1000"));
        println!("[M3-17] 1000 @13% → -884.96 / -115.04 / -1000 ✓");
    }

    /// 分位边界：0.01 元的应返也能开出票（不含税 -0.01、税额 0），不会撞「amount 必须 < 0」。
    #[test]
    fn split_tiny_amount_keeps_sign() {
        let split = split_invoice(dec("0.01"), dec("13")).expect("一分钱也能开票");
        assert_eq!(split.total_with_tax, dec("-0.01"));
        assert_eq!(split.amount, dec("-0.01"));
        assert_eq!(split.tax_amount, Decimal::ZERO);
        assert!(split.balanced());
    }

    /// 0 税率：全额进收入、税额 0（不出销项税那一行）。
    #[test]
    fn split_zero_tax_rate() {
        let split = split_invoice(dec("500"), Decimal::ZERO).expect("0% 合法");
        assert_eq!(split.amount, dec("-500"));
        assert_eq!(split.tax_amount, Decimal::ZERO);
        assert!(split.balanced());
    }

    /// 表驱动：一堆金额 × 税率都必须 self-consistent（分位相等、符号同向、借方=贷方）。
    #[test]
    fn split_is_always_balanced() {
        for amount in [
            "0.01",
            "0.02",
            "1",
            "9.99",
            "13",
            "99.99",
            "333.33",
            "1000",
            "12345.67",
            "999999.99",
        ] {
            for rate in ["0", "1", "3", "6", "9", "13", "16", "17", "100"] {
                let split = split_invoice(dec(amount), dec(rate))
                    .unwrap_or_else(|error| panic!("{amount} @ {rate}% 应当可拆：{error:?}"));
                assert!(split.balanced(), "{amount} @ {rate}% 不自洽：{split:?}");
                assert!(split.amount < Decimal::ZERO, "{amount} @ {rate}% 不含税必须为负");
                assert!(split.tax_amount <= Decimal::ZERO, "{amount} @ {rate}% 税额必须 ≤ 0");
                assert_eq!(
                    split.total_with_tax.round_dp(2),
                    dec(&format!("-{amount}")).round_dp(2),
                    "{amount} @ {rate}% 价税合计必须等于 -应返"
                );
                // 分位：三列都不得出现第三位小数
                for value in [split.amount, split.tax_amount, split.total_with_tax] {
                    assert!(value.round_dp(2) == value, "{value} 不是分位值");
                }
            }
        }
        println!("[M3-17] 10 金额 × 9 税率 = 90 组分位自洽 ✓");
    }

    /// 应返 0 / 负数 → 409 + 66094（不静默开 0 元票）。
    #[test]
    fn zero_or_negative_rebate_is_66094() {
        for value in ["0", "0.00", "-1", "-1000.55"] {
            let error = split_invoice(dec(value), dec("13")).expect_err("0 / 负数必须拒");
            assert_eq!(error.status, 409);
            assert_eq!(error.code, error_code::ERP_REBATE_INVOICE_AMOUNT);
        }
    }

    /// 税率越界 → 400 + 10001（不猜标度：0.13 就是 0.13%）。
    #[test]
    fn tax_rate_bounds() {
        let error = split_invoice(dec("100"), dec("100.01")).expect_err(">100% 必须拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        let error = split_invoice(dec("100"), dec("-1")).expect_err("负税率必须拒");
        assert_eq!(error.status, 400);
        assert_eq!(validate_tax_rate(None).unwrap(), dec("13"), "省税率 = 13%");
    }

    /// 状态闸：只有 DRAFT 能开票；SETTLED / CANCELLED 都报 409 + 66093。
    #[test]
    fn only_draft_is_invoiceable() {
        ensure_invoiceable(STATUS_DRAFT).expect("DRAFT 可开票");
        for status in [STATUS_SETTLED, STATUS_CANCELLED, "FOO"] {
            let error = ensure_invoiceable(status).expect_err("非 DRAFT 必须拒");
            assert_eq!(error.status, 409, "{status} 应当是 409");
            assert_eq!(error.code, error_code::ERP_REBATE_INVOICE_STATE);
        }
        assert_eq!(settlement_status_label(STATUS_SETTLED), "已开红字发票");
    }

    /// 备注：空白归一成 None、超长 400、正常保留。
    #[test]
    fn remark_normalization() {
        assert_eq!(normalize_remark(None).unwrap(), None);
        assert_eq!(normalize_remark(Some("   ")).unwrap(), None);
        assert_eq!(
            normalize_remark(Some(" 红冲 11 月返利 ")).unwrap(),
            Some("红冲 11 月返利".to_string())
        );
        let long = "字".repeat(MAX_REMARK_LEN + 1);
        assert_eq!(normalize_remark(Some(&long)).expect_err("超长必须拒").status, 400);
    }
}

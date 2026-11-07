//! 税率口径（FIX-6）：全系统统一 **百分数**（`13` = 13%），并在过渡期兼容小数入参。
//!
//! # 为什么需要这个模块
//!
//! 本仓历史上存在**两套税率标度**：
//!
//! | 落库位置 | 历史标度 | 13% 存成 |
//! |---|---|---|
//! | `biz_sales_order_item.tax_rate` / `biz_purchase_order_item` / `biz_stock_in_item` | 百分数 0–100 | `13.0000` |
//! | `biz_invoice.tax_rate` / `biz_material.tax_rate` | 小数 0–1 | `0.1300` |
//!
//! 同一张单上「订单行税率 13」与「发票税率 0.13」并存 ⇒ 任何一处按错标度相乘就是 **100 倍税差**。
//! FIX-6 起一律百分数（V0327 把存量的 0.13 迁成 13），本模块是**唯一**的解析/校验/换算入口：
//!
//! ```text
//! 税额 = round(不含税金额 × 税率 / 100, 4)
//! 不含税 = round(价税合计 / (1 + 税率/100), 4)；税额 = 价税合计 − 不含税
//! ```
//!
//! # 过渡期兼容（`tax_rate_mode`）
//!
//! 老前端 / 老脚本仍在传小数（`0.13`）。三种解析模式（[`TaxRateMode`]）：
//!
//! | 模式 | 入参 `13` | 入参 `0.13` | 入参 `1` | 入参 `0` |
//! |---|---|---|---|---|
//! | `PERCENT`（新前端显式声明） | 13% | **400**（越界） | 1% | 0%（免税） |
//! | `DECIMAL`（老前端显式声明） | **400**（越界） | 13% | 100% | 0%（免税） |
//! | 缺省/`LEGACY`（都不声明） | 13% | 13% + WARN | **100%** + WARN | 0%（免税） |
//!
//! `LEGACY` 的歧义只有 `(0, 1]` 这一段：`1` 到底是 1% 还是 100%，**系统无从判断**，
//! 所以按卡面口径一律 ×100（并打 WARN 点名字段）。要表达「1%」请显式传
//! `tax_rate_mode = "PERCENT"`（新前端都这么传）。
//!
//! 落库时把**实际采用的模式**写进 `tax_rate_mode` 列（V0327）：审计能看出某张单是
//! 「老前端按小数传进来的」还是「按百分数传的」；过渡期结束后删列、删 `LEGACY` 分支即可。
//!
//! # 边界：`0` 与 `1` 必须能区分
//!
//! * `0` = 免税（任何模式都是 0，永远不 ×100）；
//! * `1` = 百分数模式下是 1%，小数模式下是 100% —— 靠 [`TaxRateMode`] 区分，不靠数值猜。

use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;

use crate::shared::error_code;

/// 百分数标度的上限（`tax_rate <= 100`）。
pub const PERCENT_MAX: Decimal = Decimal::ONE_HUNDRED;

/// 落库模式：按百分数提交（新前端）。
pub const MODE_PERCENT: &str = "PERCENT";
/// 落库模式：按小数提交（老前端，过渡期）。
pub const MODE_DECIMAL: &str = "DECIMAL";

/// 税率标度的小数位（与 `NUMERIC(7,4)` 同精度）。
pub const SCALE: u32 = 4;

/// 税率入参的解析模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaxRateMode {
    /// 百分数：`13` = 13%，`1` = 1%，范围 `[0, 100]`。
    Percent,
    /// 小数：`0.13` = 13%，`1` = 100%，范围 `[0, 1]`。
    Decimal,
    /// 未声明：`(0, 1]` 当小数 ×100 并打 WARN，其余按百分数。
    Legacy,
}

impl TaxRateMode {
    /// 解析请求里的 `tax_rate_mode`（空 / 缺省 → [`TaxRateMode::Legacy`]）。
    ///
    /// 大小写不敏感；只认 `PERCENT` / `DECIMAL`，其它值 400（别静默当 LEGACY：
    /// 前端写错一个字母就要被 ×100，宁可让它在建单时就报错）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
            return Ok(TaxRateMode::Legacy);
        };
        match text.to_ascii_uppercase().as_str() {
            MODE_PERCENT => Ok(TaxRateMode::Percent),
            MODE_DECIMAL => Ok(TaxRateMode::Decimal),
            other => Err(bad_request(format!(
                "tax_rate_mode 只认 {MODE_PERCENT}（13 = 13%）或 {MODE_DECIMAL}（0.13 = 13%），\
                 收到「{other}」；不传则按过渡期规则自动识别"
            ))),
        }
    }

    /// 落库值（`Legacy` 不落库，由 [`normalize_with_mode`] 给出实际采用的模式）。
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            TaxRateMode::Percent => Some(MODE_PERCENT),
            TaxRateMode::Decimal => Some(MODE_DECIMAL),
            TaxRateMode::Legacy => None,
        }
    }
}

/// 归一结果：百分数税率 + **实际采用的**标度模式（供落库 / 回显）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalized {
    /// 百分数税率（4 位定标，`13` = 13%）。
    pub percent: Decimal,
    /// 实际采用的模式：永远是 [`MODE_PERCENT`] 或 [`MODE_DECIMAL`]。
    pub mode: &'static str,
}

impl Normalized {
    /// 落库值（`tax_rate_mode` 列）。
    pub fn mode_text(self) -> &'static str {
        self.mode
    }
}

/// 把入参税率归一到**百分数**，并返回实际采用的模式。
///
/// `field` 只用于错误 / 日志文案（例：`"开票.tax_rate"`、`"第 2 行 tax_rate"`）。
pub fn normalize_with_mode(
    value: Decimal,
    mode: TaxRateMode,
    field: &str,
) -> std::result::Result<Normalized, ApiError> {
    let value = value.round_dp(SCALE);
    match mode {
        TaxRateMode::Percent => {
            require_percent_range(value, field)?;
            Ok(Normalized {
                percent: value,
                mode: MODE_PERCENT,
            })
        }
        TaxRateMode::Decimal => {
            require_decimal_range(value, field)?;
            Ok(Normalized {
                percent: scale_up(value).round_dp(SCALE),
                mode: MODE_DECIMAL,
            })
        }
        TaxRateMode::Legacy => {
            // 0（免税）**永远**不放大：`0 × 100 = 0`，放大也没意义，但语义上要写清楚
            if value.is_zero() {
                return Ok(Normalized {
                    percent: Decimal::ZERO,
                    mode: MODE_PERCENT,
                });
            }
            if value < Decimal::ZERO {
                return Err(bad_request(format!(
                    "{field} 不能为负（收到 {value}）：税率是 0–100 的百分数"
                )));
            }
            if value <= Decimal::ONE {
                let percent = scale_up(value).round_dp(SCALE);
                tracing::warn!(
                    field = %field,
                    raw = %value,
                    percent = %percent,
                    "税率按小数（0–1）传入，已自动 ×100 转成百分数：\
                     请前端改传百分数（13 = 13%），或显式传 tax_rate_mode"
                );
                return Ok(Normalized {
                    percent,
                    mode: MODE_DECIMAL,
                });
            }
            if value > PERCENT_MAX {
                return Err(bad_request(format!(
                    "{field} 超出 0–100（收到 {value}）：税率是百分数，13 表示 13%"
                )));
            }
            Ok(Normalized {
                percent: value,
                mode: MODE_PERCENT,
            })
        }
    }
}

/// [`normalize_with_mode`] 的简化版：只要百分数，不要模式。
pub fn normalize(
    value: Decimal,
    mode: TaxRateMode,
    field: &str,
) -> std::result::Result<Decimal, ApiError> {
    Ok(normalize_with_mode(value, mode, field)?.percent)
}

/// 百分数 → 小数（`13` → `0.13`），给仍按小数运算的调用方（如报销）用。
pub fn to_decimal(percent: Decimal) -> Decimal {
    (percent / Decimal::ONE_HUNDRED).round_dp(SCALE + 2)
}

/// 由不含税金额与百分数税率算税额（4 位定标，四舍五入）。
pub fn tax_of(amount: Decimal, percent: Decimal) -> Decimal {
    (amount * percent / Decimal::ONE_HUNDRED)
        .round_dp_with_strategy(SCALE, RoundingStrategy::MidpointAwayFromZero)
}

/// 价税合计 = 不含税 + 税额。
pub fn total_of(amount: Decimal, tax_amount: Decimal) -> Decimal {
    (amount + tax_amount).round_dp(SCALE)
}

/// 由价税合计与百分数税率倒推 `(不含税, 税额)`。
///
/// 先定不含税（`total / (1 + rate/100)`）、再让税额 = 合计 − 不含税 ⇒ 两者相加恒等于合计。
/// 反过来先定税额会有 1 分尾差（撞 `ck_biz_invoice_amount`）。
pub fn split(total_with_tax: Decimal, percent: Decimal) -> (Decimal, Decimal) {
    let divisor = Decimal::ONE + percent / Decimal::ONE_HUNDRED;
    let amount = (total_with_tax / divisor).round_dp(SCALE);
    let tax_amount = (total_with_tax - amount).round_dp(SCALE);
    (amount, tax_amount)
}

/// 由 `(不含税, 税额)` 反算百分数税率（红字票 / 退货红冲用），夹在 `[0, 100]`。
pub fn percent_of(total_amount: Decimal, total_tax: Decimal) -> Decimal {
    if total_amount <= Decimal::ZERO || total_tax <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let percent = (total_tax / total_amount * Decimal::ONE_HUNDRED).round_dp(SCALE);
    if percent < Decimal::ZERO {
        Decimal::ZERO
    } else if percent > PERCENT_MAX {
        PERCENT_MAX
    } else {
        percent
    }
}

/// 是否是「过渡期歧义值」（`0 < rate <= 1`：可能是 1% 也可能是 100%）。
pub fn is_ambiguous(value: Decimal) -> bool {
    value > Decimal::ZERO && value <= Decimal::ONE
}

fn scale_up(value: Decimal) -> Decimal {
    value * Decimal::ONE_HUNDRED
}

fn require_percent_range(value: Decimal, field: &str) -> std::result::Result<(), ApiError> {
    if value < Decimal::ZERO || value > PERCENT_MAX {
        return Err(bad_request(format!(
            "{field} 必须在 0–100 之间（百分数，收到 {value}）：13 表示 13%，0 表示免税"
        )));
    }
    Ok(())
}

fn require_decimal_range(value: Decimal, field: &str) -> std::result::Result<(), ApiError> {
    if value < Decimal::ZERO || value > Decimal::ONE {
        return Err(bad_request(format!(
            "{field} 标度为小数（tax_rate_mode = {MODE_DECIMAL}）时必须在 0–1 之间，\
             收到 {value}：0.13 表示 13%；要传百分数请用 13"
        )));
    }
    Ok(())
}

fn bad_request(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(value: &str) -> Decimal {
        value.parse().expect("测试常量必须是合法十进制")
    }

    /// ① 百分数模式：13 就是 13%，范围 0–100，越界 400。
    #[test]
    fn percent_mode_accepts_0_to_100() {
        assert_eq!(
            normalize(dec("13"), TaxRateMode::Percent, "t").unwrap(),
            dec("13")
        );
        assert_eq!(
            normalize(dec("6"), TaxRateMode::Percent, "t").unwrap(),
            dec("6")
        );
        assert!(normalize(dec("100"), TaxRateMode::Percent, "t").is_ok());
        assert!(normalize(dec("0"), TaxRateMode::Percent, "t").is_ok());
        assert!(normalize(dec("100.0001"), TaxRateMode::Percent, "t").is_err());
        assert!(normalize(dec("-1"), TaxRateMode::Percent, "t").is_err());
        // 百分数模式下 0.13 是「0.13%」，不是 13% —— 0.13 合法但语义不同
        assert_eq!(
            normalize(dec("0.13"), TaxRateMode::Percent, "t").unwrap(),
            dec("0.13")
        );
    }

    /// ② 小数模式：0.13 → 13%，1 → 100%，超过 1 直接拒（不猜）。
    #[test]
    fn decimal_mode_scales_up_and_rejects_percent_input() {
        assert_eq!(
            normalize(dec("0.13"), TaxRateMode::Decimal, "t").unwrap(),
            dec("13")
        );
        assert_eq!(
            normalize(dec("1"), TaxRateMode::Decimal, "t").unwrap(),
            dec("100")
        );
        assert!(normalize(dec("13"), TaxRateMode::Decimal, "t").is_err());
        assert!(normalize(dec("-0.1"), TaxRateMode::Decimal, "t").is_err());
    }

    /// ③ 过渡期（未声明模式）：`(0,1]` ×100，`>1` 原样，0 恒为 0。
    #[test]
    fn legacy_mode_heuristic() {
        let scaled = normalize_with_mode(dec("0.13"), TaxRateMode::Legacy, "t").unwrap();
        assert_eq!(scaled.percent, dec("13"), "0.13 → 13%");
        assert_eq!(scaled.mode, MODE_DECIMAL, "按小数处理要落 DECIMAL 审计位");

        let plain = normalize_with_mode(dec("13"), TaxRateMode::Legacy, "t").unwrap();
        assert_eq!(plain.percent, dec("13"));
        assert_eq!(plain.mode, MODE_PERCENT, "13 已是百分数，落 PERCENT");

        // 边界：1 在过渡期被当成 100%（歧义值一律 ×100，卡面口径）
        assert_eq!(
            normalize(dec("1"), TaxRateMode::Legacy, "t").unwrap(),
            dec("100")
        );
        // 0 = 免税，永远不放大
        let zero = normalize_with_mode(dec("0"), TaxRateMode::Legacy, "t").unwrap();
        assert_eq!(zero.percent, Decimal::ZERO);
        assert_eq!(zero.mode, MODE_PERCENT);
        // 越界
        assert!(normalize(dec("100.5"), TaxRateMode::Legacy, "t").is_err());
        assert!(normalize(dec("-0.01"), TaxRateMode::Legacy, "t").is_err());
    }

    /// ④ 归一在「结果是百分数」时幂等（重放不会 ×100 两次）。
    ///
    /// ⚠ 例外必须说清楚：`0.005 → 0.5` 仍在「歧义带 `(0,1]`」里，**再次归一会被 ×100 成 50**。
    /// 所以过渡期兼容的幂等**不靠**这个函数，而靠迁移的 `tax_rate_mode IS NULL` 闸
    /// （V0327 的两次 up 实测：0.005 只放大一次；见 `idem_backfill_fix6.sh`）。
    #[test]
    fn normalize_is_idempotent_for_percent_output() {
        for raw in ["0.13", "0.06", "1"] {
            let once = normalize(dec(raw), TaxRateMode::Legacy, "t").unwrap();
            let twice = normalize(once, TaxRateMode::Legacy, "t").unwrap();
            assert_eq!(once, twice, "{raw} 归一后再归一必须不变（结果已 > 1）");
        }
        // 反例（写进测试是为了让下一个人别误以为「函数自带幂等」）
        let once = normalize(dec("0.005"), TaxRateMode::Legacy, "t").unwrap();
        assert_eq!(once, dec("0.5"), "0.005 → 0.5（0.5%），仍落在歧义带");
        assert_eq!(
            normalize(once, TaxRateMode::Legacy, "t").unwrap(),
            dec("50"),
            "同值再走一次 LEGACY 会被再 ×100 —— 所以落库幂等只能靠 mode 标记，不靠启发式"
        );
    }

    /// ⑤ 税额 / 拆分：百分数口径，且「合计 = 不含税 + 税额」恒成立。
    #[test]
    fn tax_and_split_use_percent() {
        // 2000 × 13% = 260
        assert_eq!(tax_of(dec("2000"), dec("13")), dec("260"));
        assert_eq!(tax_of(dec("2000"), dec("0")), Decimal::ZERO);
        // 2260 倒推：2000 / 260
        let (amount, tax) = split(dec("2260"), dec("13"));
        assert_eq!(amount, dec("2000"));
        assert_eq!(tax, dec("260"));
        assert_eq!(total_of(amount, tax), dec("2260"));
        // 1% 与 100% 都不至于算成 100 倍
        assert_eq!(tax_of(dec("100"), dec("1")), dec("1"));
        assert_eq!(tax_of(dec("100"), dec("100")), dec("100"));
    }

    /// ⑥ 反算税率（红字票）：0.13 的旧口径必须给出 13，而不是 0.13。
    #[test]
    fn percent_of_returns_percent() {
        assert_eq!(percent_of(dec("2000"), dec("260")), dec("13"));
        assert_eq!(percent_of(dec("0"), dec("260")), Decimal::ZERO);
        assert_eq!(percent_of(dec("100"), dec("0")), Decimal::ZERO);
        // 反算值超界要夹住，别让 CHECK 约束炸
        assert_eq!(percent_of(dec("100"), dec("260")), PERCENT_MAX);
    }

    /// ⑦ 模式解析：大小写不敏感；空 = 过渡期；其它值 400（不静默降级）。
    #[test]
    fn mode_parse() {
        assert_eq!(TaxRateMode::parse(None).unwrap(), TaxRateMode::Legacy);
        assert_eq!(TaxRateMode::parse(Some("  ")).unwrap(), TaxRateMode::Legacy);
        assert_eq!(
            TaxRateMode::parse(Some("percent")).unwrap(),
            TaxRateMode::Percent
        );
        assert_eq!(
            TaxRateMode::parse(Some("DECIMAL")).unwrap(),
            TaxRateMode::Decimal
        );
        assert!(TaxRateMode::parse(Some("PCT")).is_err());
        assert_eq!(TaxRateMode::Percent.as_str(), Some(MODE_PERCENT));
        assert_eq!(TaxRateMode::Legacy.as_str(), None);
    }

    /// ⑧ 小数模式换算给报销用：13% → 0.13。
    #[test]
    fn to_decimal_round_trip() {
        assert_eq!(to_decimal(dec("13")), dec("0.13"));
        assert_eq!(to_decimal(dec("0")), Decimal::ZERO);
        assert!(is_ambiguous(dec("1")));
        assert!(!is_ambiguous(dec("0")));
        assert!(!is_ambiguous(dec("13")));
    }
}

//! 销售返利政策 · 接口契约（M3-15）。
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | GET | `/business/erp/rebate-policy/page` | `business:erp:rebate-policy:list` |
//! | GET | `/business/erp/rebate-policy/{id}` | `business:erp:rebate-policy:list` |
//! | POST | `/business/erp/rebate-policy` | `business:erp:rebate-policy:create` |
//! | PUT | `/business/erp/rebate-policy/{id}` | `business:erp:rebate-policy:update` |
//! | DELETE | `/business/erp/rebate-policy/{id}` | `business:erp:rebate-policy:delete` |
//!
//! # 三种规则类型（本文件是**唯一**的校验实现；迁移 V0438 的注释是同一份口径的文字版）
//!
//! ```text
//! rule_type   rate_unit              规则行数          规则行的 tier_*
//! ---------   --------------------   ---------------   -------------------------------
//! BY_QTY      PER_UNIT（元/件）      恰好 1            tier_from = 0 且 tier_to IS NULL
//! BY_AMOUNT   PERCENT（销售额 %）    恰好 1            tier_from = 0 且 tier_to IS NULL
//! TIERED      PER_UNIT 或 PERCENT    2 ~ 20 档         从 0 起、首尾相接、末档 tier_to IS NULL
//! ```
//!
//! 三种类型都只钉**数据形状**；「怎么乘、落哪档」在 [`pick_tier`]（M3-16 的唯一落档实现，
//! 本卡先给纯函数与单测，不连库、不算金额）。
//!
//! # 为什么 `rate_unit` 是政策级而不是规则级
//!
//! 放到规则级意味着同一张政策里可以出现「第 1 档元/件、第 2 档百分比」——那不是灵活配置，
//! 是两份量纲不同的数相加（算出来的返利没有任何意义）。放政策级一行，库层
//! `ck_biz_srbp_unit_pair` 就能在**建表期**挡住「按量却按百分比」这种矛盾。
//!
//! # 为什么「阶梯依据」不单列一列
//!
//! 它由 `rate_unit` 决定：`PER_UNIT` ⇒ 档位按**销量**分、`PERCENT` ⇒ 档位按**销售额**分。
//! 多一列就多一个能自相矛盾的字段。
//!
//! # 金额 / 比率一律 JSON 字符串
//!
//! `NUMERIC` 走 `rust_decimal::Decimal`，serde 默认序列化成字符串（`"0.500000"`），
//! 前端只展示不自算（与 `sales_target_dto` / 客户信用面板同口径）。
//!
//! # 入参 `rules` 的顺序不重要
//!
//! 服务端按 `tier_from` 升序重排后重新编号（`seq` = 1..N）；调用方给的顺序、`seq` 一律忽略。
//! 理由：档位顺序是**业务事实**（从 0 往上），不是排版偏好；让前端决定顺序，就会出现
//! 「界面看着对、库里档位倒过来」的脏数据，而 M3-16 按 `tier_from` 落档时结果全错。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use cloudpivot_kernel::shared::error_code;

// ==================== 域值（与 V0438 的 CHECK 逐字一致，单测钉住） ====================

/// 按量返利：返利 = 期间销量 × `rate`（元/件）。
pub const RULE_BY_QTY: &str = "BY_QTY";
/// 按额返利：返利 = 期间销售额 × `rate`%（`rate` 是百分数）。
pub const RULE_BY_AMOUNT: &str = "BY_AMOUNT";
/// 阶梯返利：`rate_unit` 决定按销量还是按销售额落档，落档后按该档 `rate` 算。
pub const RULE_TIERED: &str = "TIERED";

/// 全部规则类型（顺序即前端下拉顺序）。
pub const RULE_TYPES: [&str; 3] = [RULE_BY_QTY, RULE_BY_AMOUNT, RULE_TIERED];

/// 计价单位：元/件（与数量相乘）。
pub const UNIT_PER_UNIT: &str = "PER_UNIT";
/// 计价单位：销售额百分比（`rate = 10` 表示 10%）。
pub const UNIT_PERCENT: &str = "PERCENT";

/// 全部计价单位。
pub const RATE_UNITS: [&str; 2] = [UNIT_PER_UNIT, UNIT_PERCENT];

/// 结算粒度：月度。
pub const SETTLE_MONTH: &str = "MONTH";
/// 结算粒度：季度。
pub const SETTLE_QUARTER: &str = "QUARTER";

/// 全部结算粒度。
pub const SETTLE_PERIODS: [&str; 2] = [SETTLE_MONTH, SETTLE_QUARTER];

/// 政策启用（参与 M3-16 结算）。
pub const STATUS_ENABLED: i16 = 1;
/// 政策停用（保留政策与历史，结算跳过）。
pub const STATUS_DISABLED: i16 = 0;

/// 阶梯档数上限（超过就是录错了：真实返利政策的档位极少超过 10 档）。
pub const MAX_TIERS: usize = 20;
/// 阶梯返利的最少档数（只有一档不叫阶梯，请改用按量/按额 —— 否则「阶梯」页面上只有一个区间）。
pub const MIN_TIER_TIERS: usize = 2;

/// `NUMERIC(18,4)` 的整数部分上限（10^14）。
const QTY_LIMIT: i128 = 100_000_000_000_000;
/// `NUMERIC(18,6)` 的整数部分上限（10^12）。
const RATE_LIMIT: i128 = 1_000_000_000_000;
/// 百分比的上限（100%）。
const PERCENT_MAX: i128 = 100;

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 规则不合法（400 + 66089）：比 `PARAM_INVALID` 更细，前端要把这条翻成「第几档哪一项不对」。
fn rule_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REBATE_RULE_INVALID, message.into())
}

// ==================== 纯校验：三种规则类型 ====================

/// 一档规则（已校验、已编号，可直接落库）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleWrite {
    /// 档号（1 起；由 [`checked_rules`] 按 `tier_from` 升序重新编号）。
    pub seq: i16,
    /// 本档下限（含）。
    pub tier_from: Decimal,
    /// 本档上限（不含）；`None` = 无上限（只有末档）。
    pub tier_to: Option<Decimal>,
    /// 比率（`PER_UNIT` 元/件 / `PERCENT` 百分数）。
    pub rate: Decimal,
    /// 本档备注。
    pub remark: Option<String>,
}

/// 入参的一档（DTO 形态：`tier_to` 可空、不带宽高精度约束）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInput {
    /// 本档下限（含）。
    pub tier_from: Decimal,
    /// 本档上限（不含）；`None` = 无上限。
    pub tier_to: Option<Decimal>,
    /// 比率。
    pub rate: Decimal,
    /// 本档备注。
    pub remark: Option<String>,
}

/// 规则类型对应的**默认**计价单位（`TIERED` 没有默认值，必须显式给）。
pub fn default_unit_of(rule_type: &str) -> Option<&'static str> {
    match rule_type {
        RULE_BY_QTY => Some(UNIT_PER_UNIT),
        RULE_BY_AMOUNT => Some(UNIT_PERCENT),
        _ => None,
    }
}

/// 规则类型的中文名（错误文案 / 日志用）。
pub fn rule_type_label(rule_type: &str) -> &str {
    match rule_type {
        RULE_BY_QTY => "按量",
        RULE_BY_AMOUNT => "按额",
        RULE_TIERED => "阶梯",
        other => other,
    }
}

/// 计价单位的中文名。
pub fn unit_label(unit: &str) -> &str {
    match unit {
        UNIT_PER_UNIT => "元/件",
        UNIT_PERCENT => "销售额百分比",
        other => other,
    }
}

/// 结算粒度的中文名。
pub fn settle_label(period: &str) -> &str {
    match period {
        SETTLE_MONTH => "月度",
        SETTLE_QUARTER => "季度",
        other => other,
    }
}

/// 归一化规则类型（`trim` + 大写）；未知值原样返回，由 [`checked_unit`] / [`checked_rules`] 拒。
pub fn normalize_rule_type(raw: &str) -> String {
    raw.trim().to_ascii_uppercase()
}

/// 归一化结算粒度（`trim` + 大写）。
pub fn normalize_settle_period(raw: &str) -> String {
    raw.trim().to_ascii_uppercase()
}

/// 校验并归一化计价单位。
///
/// # 规则
///
/// | 入参 | 结果 |
/// |------|------|
/// | `BY_QTY` + 省略 | `PER_UNIT`（默认） |
/// | `BY_AMOUNT` + 省略 | `PERCENT`（默认） |
/// | `TIERED` + 省略 | **400**：阶梯必须说清「按量分档」还是「按额分档」 |
/// | 显式给了但与类型矛盾（如 `BY_QTY` + `PERCENT`） | **400**，点名两个值 |
///
/// # 异常情况
///
/// 返回 [`ApiError`]（400 + 66089）。未知 `rate_unit` 也在这里拒（不静默回落默认值 ——
/// 回落会让「按额政策按了元/件」这种错配悄悄生效，算出来的返利小两个数量级）。
pub fn checked_unit(rule_type: &str, raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let explicit = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_ascii_uppercase);

    if let Some(unit) = explicit.as_deref()
        && !RATE_UNITS.contains(&unit)
    {
        return Err(rule_invalid(format!(
            "rate_unit 只能是 {} 之一（收到 `{unit}`）",
            RATE_UNITS.join(" / ")
        )));
    }

    match (rule_type, explicit) {
        (RULE_BY_QTY, None) => Ok(UNIT_PER_UNIT.to_string()),
        (RULE_BY_QTY, Some(unit)) => {
            if unit == UNIT_PER_UNIT {
                Ok(unit)
            } else {
                Err(rule_invalid(format!(
                    "按量返利（BY_QTY）的计价单位只能是元/件（PER_UNIT），不能是 {unit}：\
                     按量政策按百分比算出来的数不是钱"
                )))
            }
        }
        (RULE_BY_AMOUNT, None) => Ok(UNIT_PERCENT.to_string()),
        (RULE_BY_AMOUNT, Some(unit)) => {
            if unit == UNIT_PERCENT {
                Ok(unit)
            } else {
                Err(rule_invalid(format!(
                    "按额返利（BY_AMOUNT）的计价单位只能是销售额百分比（PERCENT），不能是 {unit}：\
                     按额政策的本金就是销售额，再乘单价没有意义"
                )))
            }
        }
        (RULE_TIERED, None) => Err(rule_invalid(
            "阶梯返利（TIERED）必须显式给 rate_unit：PER_UNIT = 按销量分档（元/件），\
             PERCENT = 按销售额分档（百分比）",
        )),
        (RULE_TIERED, Some(unit)) => Ok(unit),
        (other, _) => Err(rule_invalid(format!(
            "未知 rule_type `{other}`（只能是 {}）",
            RULE_TYPES.join(" / ")
        ))),
    }
}

/// 校验 `rule_type` 是否在域内（`trim` + 大写后再判）。
pub fn checked_rule_type(raw: &str) -> std::result::Result<String, ApiError> {
    let value = normalize_rule_type(raw);
    if RULE_TYPES.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(rule_invalid(format!(
            "未知 rule_type `{raw}`（只能是 {}）",
            RULE_TYPES.join(" / ")
        )))
    }
}

/// 校验结算粒度。
pub fn checked_settle_period(raw: &str) -> std::result::Result<String, ApiError> {
    let value = normalize_settle_period(raw);
    if SETTLE_PERIODS.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(bad_request(format!(
            "settle_period 只能是 {} 之一（收到 `{raw}`）",
            SETTLE_PERIODS.join(" / ")
        )))
    }
}

/// 校验状态值（1 启用 / 0 停用）。
pub fn checked_status(raw: i16) -> std::result::Result<i16, ApiError> {
    if raw == STATUS_ENABLED || raw == STATUS_DISABLED {
        Ok(raw)
    } else {
        Err(bad_request(format!("status 只能是 1（启用）或 0（停用），收到 {raw}")))
    }
}

/// 数值校验：非负、小数位不超、装得进 `NUMERIC(18,4)`。
fn check_tier_value(value: Decimal, scale_label: &str, field: &str) -> std::result::Result<(), ApiError> {
    if value.is_sign_negative() {
        return Err(rule_invalid(format!("{field} 不能为负（收到 {value}）")));
    }
    if value.scale() > 4 {
        return Err(rule_invalid(format!(
            "{field} 最多 4 位小数（收到 {value}）：档位按 NUMERIC(18,4) 存，多的位会被库静默四舍五入"
        )));
    }
    if value.trunc().mantissa().abs() >= QTY_LIMIT {
        return Err(rule_invalid(format!(
            "{field} 超出 NUMERIC(18,4) 的整数上限（{value}）：{scale_label} 的数值明显录错了"
        )));
    }
    Ok(())
}

/// 比率校验：> 0、小数位 ≤ 6、百分比 ≤ 100、装得进 `NUMERIC(18,6)`。
fn check_rate(value: Decimal, unit: &str, field: &str) -> std::result::Result<(), ApiError> {
    if value <= Decimal::ZERO {
        return Err(rule_invalid(format!(
            "{field} 必须大于 0（收到 {value}）：0 元档没有意义，会让人以为「这一档不返」"
        )));
    }
    if value.scale() > 6 {
        return Err(rule_invalid(format!(
            "{field} 最多 6 位小数（收到 {value}）：比率按 NUMERIC(18,6) 存"
        )));
    }
    if value.trunc().mantissa().abs() >= RATE_LIMIT {
        return Err(rule_invalid(format!(
            "{field} 超出 NUMERIC(18,6) 的整数上限（{value}）"
        )));
    }
    if unit == UNIT_PERCENT && value.trunc().mantissa().abs() > PERCENT_MAX {
        return Err(rule_invalid(format!(
            "{field} 是百分比，不能超过 100（收到 {value}）：返利比销售额还高只会是录错"
        )));
    }
    Ok(())
}

/// 校验并归一化**规则行**（三种类型的形状全部在这里收口）。
///
/// # 参数
///
/// - `rule_type`：已归一化的规则类型（[`checked_rule_type`] 的返回）。
/// - `rate_unit`：已归一化的计价单位（[`checked_unit`] 的返回）。
/// - `rules`：入参档位（顺序无关，服务端按 `tier_from` 升序重排）。
///
/// # 返回值
///
/// 已编号（`seq = 1..N`）的规则行，可直接落库。
///
/// # 规则
///
/// ```text
/// 通用：至少 1 档、最多 20 档；tier_from >= 0；tier_to > tier_from；rate > 0；
///       PERCENT 的 rate <= 100
/// BY_QTY / BY_AMOUNT：恰好 1 档，且 tier_from = 0、tier_to 为空（平铺）
/// TIERED：至少 2 档；首档 tier_from = 0；逐档 tier_to == 下一档 tier_from；
///         末档 tier_to 为空；档位下限不得重复
/// ```
///
/// # 异常情况
///
/// 任一条不符 → 400 + 66089，文案点名「第几档、哪个字段、错在哪」。
pub fn checked_rules(
    rule_type: &str,
    rate_unit: &str,
    rules: &[RuleInput],
) -> std::result::Result<Vec<RuleWrite>, ApiError> {
    if rules.is_empty() {
        return Err(rule_invalid("至少要有 1 档规则：没有比率的政策算不出返利"));
    }
    if rules.len() > MAX_TIERS {
        return Err(rule_invalid(format!(
            "档位最多 {MAX_TIERS} 档（收到 {} 档）",
            rules.len()
        )));
    }

    // 逐档基础校验（先做，错误文案能直接点名第几档）
    for (index, rule) in rules.iter().enumerate() {
        let label = format!("第 {} 档", index + 1);
        check_tier_value(rule.tier_from, "档位", &format!("{label}的 tier_from（下限）"))?;
        if let Some(to) = rule.tier_to {
            check_tier_value(to, "档位", &format!("{label}的 tier_to（上限）"))?;
            if to <= rule.tier_from {
                return Err(rule_invalid(format!(
                    "{label}的区间不成立：tier_to（{to}）必须大于 tier_from（{}）—— 上下限相等即空区间",
                    rule.tier_from
                )));
            }
        }
        check_rate(rule.rate, rate_unit, &format!("{label}的 rate（比率）"))?;
    }

    let flat = matches!(rule_type, RULE_BY_QTY | RULE_BY_AMOUNT);
    if flat {
        if rules.len() != 1 {
            return Err(rule_invalid(format!(
                "{}返利只能有 1 档规则（收到 {} 档）：要按区间分档请改用阶梯（TIERED）",
                rule_type_label(rule_type),
                rules.len()
            )));
        }
        let rule = &rules[0];
        if !rule.tier_from.is_zero() || rule.tier_to.is_some() {
            return Err(rule_invalid(format!(
                "{}返利是平铺费率：唯一一档必须 tier_from = 0 且 tier_to 留空（收到 {} ~ {}）——\
                 要设起征区间请改用阶梯（TIERED）",
                rule_type_label(rule_type),
                rule.tier_from,
                rule.tier_to
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "∞".to_string())
            )));
        }
        return Ok(vec![RuleWrite {
            seq: 1,
            tier_from: rule.tier_from,
            tier_to: None,
            rate: rule.rate,
            remark: rule.remark.clone(),
        }]);
    }

    // TIERED
    if rules.len() < MIN_TIER_TIERS {
        return Err(rule_invalid(format!(
            "阶梯返利至少要 {MIN_TIER_TIERS} 档（收到 {} 档）：只有一档请改用按量（BY_QTY）\
             或按额（BY_AMOUNT），否则它就是一张伪装成阶梯的平铺政策",
            rules.len()
        )));
    }
    let mut sorted: Vec<&RuleInput> = rules.iter().collect();
    sorted.sort_by(|left, right| left.tier_from.cmp(&right.tier_from));
    for pair in sorted.windows(2) {
        if pair[0].tier_from == pair[1].tier_from {
            return Err(rule_invalid(format!(
                "阶梯档位下限重复（{} 出现两次）：同一档位区间只能有一行",
                pair[0].tier_from
            )));
        }
    }
    if !sorted[0].tier_from.is_zero() {
        return Err(rule_invalid(format!(
            "阶梯的第一档必须从 0 起（收到 {}）：否则 0 ~ {} 这段销量/销售额没有政策可算，\
             结算时会被静默算成 0 返利",
            sorted[0].tier_from, sorted[0].tier_from
        )));
    }
    for index in 0..sorted.len() - 1 {
        let current = sorted[index];
        let next = sorted[index + 1];
        match current.tier_to {
            None => {
                return Err(rule_invalid(format!(
                    "第 {} 档（{} 起）没写上限，但它不是最后一档：中间档必须写 tier_to，\
                     否则下一个档位的区间和它重叠",
                    index + 1,
                    current.tier_from
                )));
            }
            Some(to) => {
                if to != next.tier_from {
                    return Err(rule_invalid(format!(
                        "第 {} 档的上限（{to}）与第 {} 档的下限（{}）不相接：\
                         阶梯必须首尾相接（{} ~ {to} → {to} ~ …），否则中间那一段没有费率",
                        index + 1,
                        index + 2,
                        next.tier_from,
                        current.tier_from
                    )));
                }
            }
        }
    }
    if sorted[sorted.len() - 1].tier_to.is_some() {
        return Err(rule_invalid(
            "最后一档必须留空上限（tier_to 为空 = 无上限）：定了上限的话，超过上限的销量/销售额\
             会落在所有档位之外，返利算成 0 而没有任何提示",
        ));
    }

    Ok(sorted
        .iter()
        .enumerate()
        .map(|(index, rule)| RuleWrite {
            seq: i16::try_from(index + 1).unwrap_or(i16::MAX),
            tier_from: rule.tier_from,
            tier_to: rule.tier_to,
            rate: rule.rate,
            remark: rule.remark.clone(),
        })
        .collect())
}

/// **落档**：给定销量 / 销售额，返回命中的那一档（M3-16 的唯一落档实现）。
///
/// # 口径
///
/// 区间左闭右开 `[tier_from, tier_to)`；末档 `tier_to` 为空即无上限。**逐档命中**，
/// 不做累进、不回溯（落哪档就按哪档 `rate` 乘 —— 见 V0438 建表注释的同一句话）。
///
/// # 参数
///
/// - `rules`：已按 `seq` 升序的规则行（[`checked_rules`] 的输出顺序）。
/// - `value`：销量（`PER_UNIT`）或销售额（`PERCENT`）。
///
/// # 返回值
///
/// 命中的档；`None` = 没有一档命中（只可能发生在数据被绕过接口写歪时，例如首档不从 0 起）。
///
/// # 异常情况
///
/// 纯函数，不返回错误；负数入参同样按区间比较（不会 panic）。
pub fn pick_tier(rules: &[RuleWrite], value: Decimal) -> Option<&RuleWrite> {
    rules
        .iter()
        .find(|rule| value >= rule.tier_from && rule.tier_to.is_none_or(|to| value < to))
}

// ==================== 出参 ====================

/// 一档规则（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct RebateRuleRes {
    /// 规则行主键。
    pub id: String,
    /// 档号（1 起）。
    pub seq: i16,
    /// 本档下限（含）。
    pub tier_from: Decimal,
    /// 本档上限（不含）；`null` = 无上限。
    pub tier_to: Option<Decimal>,
    /// 比率。
    pub rate: Decimal,
    /// 本档备注。
    pub remark: Option<String>,
}

/// 返利政策（出参：详情与列表同形）。
#[derive(Debug, Clone, Serialize)]
pub struct RebatePolicyRes {
    /// 政策主键。
    pub id: String,
    /// 政策编号（`BR-yyyyMMdd-000001`）。
    pub policy_no: String,
    /// 政策名称。
    pub policy_name: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（`LEFT JOIN biz_customer`；客户已软删时为 `null`）。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 规则类型（`BY_QTY` / `BY_AMOUNT` / `TIERED`）。
    pub rule_type: String,
    /// 计价单位（`PER_UNIT` / `PERCENT`）。
    pub rate_unit: String,
    /// 结算粒度（`MONTH` / `QUARTER`）。
    pub settle_period: String,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`null` = 长期有效。
    pub effective_to: Option<NaiveDate>,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 规则行（按 `seq` 升序；列表也带上，前端不必逐条再请求）。
    pub rules: Vec<RebateRuleRes>,
}

// ==================== 入参 ====================

/// 一档规则（入参）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RebateRuleReq {
    /// 本档下限（含；按量/按额必须 0）。
    #[serde(default)]
    pub tier_from: Option<Decimal>,
    /// 本档上限（不含）；`null`/省略 = 无上限。
    #[serde(default)]
    pub tier_to: Option<Decimal>,
    /// 比率（元/件 或 百分数）。
    #[serde(default)]
    pub rate: Option<Decimal>,
    /// 本档备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl RebateRuleReq {
    /// 转成纯校验用的入参（缺 `rate` → 400 点名第几档）。
    pub fn to_input(&self, index: usize) -> std::result::Result<RuleInput, ApiError> {
        let rate = self.rate.ok_or_else(|| {
            rule_invalid(format!(
                "第 {} 档缺少 rate（比率）：按量填每件返多少元，按额/百分比阶梯填百分比",
                index + 1
            ))
        })?;
        Ok(RuleInput {
            tier_from: self.tier_from.unwrap_or(Decimal::ZERO),
            tier_to: self.tier_to,
            rate,
            remark: self
                .remark
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
        })
    }
}

/// 新建 / 编辑返利政策（**整体覆盖**：政策头 + 规则整单替换）。
///
/// 不做「部分更新」：政策是**条款**，只改一半（比如改了 `rule_type` 却留着旧档位）会
/// 让库里的档位与类型互相解释不通。前端编辑弹窗本来就持有全量字段，整单提交即可。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RebatePolicyWriteReq {
    /// 政策名称（必填，≤ 200 字）。
    #[serde(default)]
    pub policy_name: String,
    /// 客户 id（UUID；必须存在、未软删且 `ACTIVE`）。
    #[serde(default)]
    pub customer_id: String,
    /// 规则类型（`BY_QTY` / `BY_AMOUNT` / `TIERED`）。
    #[serde(default)]
    pub rule_type: String,
    /// 计价单位（`PER_UNIT` / `PERCENT`；按量/按额可省略，阶梯必填）。
    #[serde(default)]
    pub rate_unit: Option<String>,
    /// 结算粒度（`MONTH` / `QUARTER`）。
    #[serde(default)]
    pub settle_period: String,
    /// 生效日（`yyyy-MM-dd`）。
    #[serde(default)]
    pub effective_from: String,
    /// 失效日（`yyyy-MM-dd`；省略/空 = 长期有效）。
    #[serde(default)]
    pub effective_to: Option<String>,
    /// 状态（1 启用 / 0 停用；省略 = 新建 1 / 编辑保留原值）。
    #[serde(default)]
    pub status: Option<i16>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
    /// 规则档位（顺序无关）。
    #[serde(default)]
    pub rules: Vec<RebateRuleReq>,
}

/// 政策列表筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RebatePolicyFilter {
    /// 关键字：政策编号 / 政策名称 / 客户编码 / 客户名称 模糊。
    #[serde(default)]
    pub keyword: Option<String>,
    /// 客户精确。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 规则类型精确。
    #[serde(default)]
    pub rule_type: Option<String>,
    /// 结算粒度精确。
    #[serde(default)]
    pub settle_period: Option<String>,
    /// 状态精确（1 / 0）。
    #[serde(default)]
    pub status: Option<i16>,
    /// 「在指定日期生效」筛选（`yyyy-MM-dd`；省略 = 不筛）。
    #[serde(default)]
    pub active_on: Option<String>,
    /// 页码（1 起，缺省 1）。
    #[serde(default)]
    pub page: u64,
    /// 每页条数（缺省 20，上限 200）。
    #[serde(default)]
    pub page_size: u64,
}

/// 政策名称的长度上限（与 `VARCHAR(200)` 一致）。
pub const MAX_NAME_LEN: usize = 200;

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 测试用定点数（本仓不引 `rust_decimal_macros`，与 asset / purchase 用例同口径）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试字面量必须是合法小数")
    }

    fn rule(from: Decimal, to: Option<Decimal>, rate: Decimal) -> RuleInput {
        RuleInput {
            tier_from: from,
            tier_to: to,
            rate,
            remark: None,
        }
    }

    /// 域值与 V0438 的 CHECK 逐字一致（改一个字母就是「授不上权 / 插不进库」）。
    #[test]
    fn domain_values_match_migration() {
        assert_eq!(RULE_TYPES, ["BY_QTY", "BY_AMOUNT", "TIERED"]);
        assert_eq!(RATE_UNITS, ["PER_UNIT", "PERCENT"]);
        assert_eq!(SETTLE_PERIODS, ["MONTH", "QUARTER"]);
        assert_eq!((STATUS_ENABLED, STATUS_DISABLED), (1, 0));
        assert_eq!((MAX_TIERS, MIN_TIER_TIERS), (20, 2));
        println!("[M3-15] 域值：3 类型 / 2 单位 / 2 粒度 / 状态 1,0 / 档数 2~20 ✓");
    }

    /// 按量：默认元/件；显式给元/件也行；给百分比 400。
    #[test]
    fn by_qty_unit_is_pinned() {
        assert_eq!(checked_unit(RULE_BY_QTY, None).expect("默认元/件"), UNIT_PER_UNIT);
        assert_eq!(
            checked_unit(RULE_BY_QTY, Some(" per_unit ")).expect("显式元/件"),
            UNIT_PER_UNIT
        );
        let error = checked_unit(RULE_BY_QTY, Some("PERCENT")).expect_err("按量不能按百分比");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);
        assert!(error.msg.contains("PER_UNIT"), "{}", error.msg);
        println!("[M3-15] 按量 → 元/件（默认/显式），给百分比 400 + 66089 ✓");
    }

    /// 按额：默认百分比；给元/件 400；未知值 400（不静默回落默认）。
    #[test]
    fn by_amount_unit_is_pinned() {
        assert_eq!(checked_unit(RULE_BY_AMOUNT, None).expect("默认百分比"), UNIT_PERCENT);
        assert!(checked_unit(RULE_BY_AMOUNT, Some("PER_UNIT")).is_err());
        let error = checked_unit(RULE_BY_AMOUNT, Some("PERCENTAGE")).expect_err("未知单位必须拒");
        assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);
        assert!(error.msg.contains("PERCENTAGE"), "文案要点名收到的值：{}", error.msg);
        println!("[M3-15] 按额 → 百分比（默认/显式），元/件与未知值 400 ✓");
    }

    /// 阶梯：必须显式给单位；两种都行；未知类型 400。
    #[test]
    fn tiered_requires_explicit_unit() {
        let error = checked_unit(RULE_TIERED, None).expect_err("阶梯必须显式给单位");
        assert!(error.msg.contains("PER_UNIT"), "{}", error.msg);
        assert_eq!(
            checked_unit(RULE_TIERED, Some("PERCENT")).expect("按额分档"),
            UNIT_PERCENT
        );
        assert_eq!(
            checked_unit(RULE_TIERED, Some("PER_UNIT")).expect("按量分档"),
            UNIT_PER_UNIT
        );
        assert!(checked_unit("BY_VOLUME", None).is_err(), "未知类型必须拒");
        println!("[M3-15] 阶梯单位必填（两者皆可）、未知类型 400 ✓");
    }

    /// 按量 / 按额：恰好一档、平铺（tier_from = 0、tier_to 空）。
    #[test]
    fn flat_rules_are_single_and_unbounded() {
        let ok = checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(dec("0"), None, dec("0.5"))]).expect("合法按量");
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].seq, 1);
        assert_eq!(ok[0].tier_to, None);

        // 多档 → 400
        let error = checked_rules(
            RULE_BY_QTY,
            UNIT_PER_UNIT,
            &[
                rule(dec("0"), Some(dec("100")), dec("0.5")),
                rule(dec("100"), None, dec("0.8")),
            ],
        )
        .expect_err("按量不能多档");
        assert!(error.msg.contains("阶梯"), "{}", error.msg);

        // 有起征点 → 400
        let error = checked_rules(RULE_BY_AMOUNT, UNIT_PERCENT, &[rule(dec("1000"), None, dec("2"))])
            .expect_err("按额不能设区间");
        assert!(error.msg.contains("TIERED"), "{}", error.msg);
        println!("[M3-15] 按量/按额 = 单档平铺（多档/设区间 → 400 并指路 TIERED）✓");
    }

    /// 百分比比率 > 100 → 400；元/件则允许 > 100（大额返利是合法的）。
    #[test]
    fn percent_rate_is_capped_at_100() {
        let error = checked_rules(RULE_BY_AMOUNT, UNIT_PERCENT, &[rule(dec("0"), None, dec("101"))])
            .expect_err("百分比 > 100 必须拒");
        assert!(error.msg.contains("100"), "{}", error.msg);
        assert!(
            checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(dec("0"), None, dec("200"))]).is_ok(),
            "元/件 200 元是合法政策"
        );
        println!("[M3-15] 百分比 ≤ 100，元/件不设上限 ✓");
    }

    /// 阶梯：乱序入参也会被重排编号；首档不从 0 起 / 不续接 / 末档带上限 / 重复下限 全部 400。
    #[test]
    fn tiered_rules_are_sorted_and_contiguous() {
        let sorted = checked_rules(
            RULE_TIERED,
            UNIT_PER_UNIT,
            &[
                rule(dec("500"), None, dec("2")),
                rule(dec("0"), Some(dec("100")), dec("0.5")),
                rule(dec("100"), Some(dec("500")), dec("1")),
            ],
        )
        .expect("合法三档（乱序入参）");
        assert_eq!(sorted.iter().map(|rule| rule.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(sorted[0].tier_from, dec("0"));
        assert_eq!(sorted[2].tier_to, None);

        let error = checked_rules(
            RULE_TIERED,
            UNIT_PER_UNIT,
            &[
                rule(dec("10"), Some(dec("100")), dec("1")),
                rule(dec("100"), None, dec("2")),
            ],
        )
        .expect_err("首档不从 0 起");
        assert!(error.msg.contains("从 0 起"), "{}", error.msg);

        let error = checked_rules(
            RULE_TIERED,
            UNIT_PER_UNIT,
            &[
                rule(dec("0"), Some(dec("100")), dec("1")),
                rule(dec("200"), None, dec("2")),
            ],
        )
        .expect_err("中间断档");
        assert!(error.msg.contains("相接"), "{}", error.msg);

        let error = checked_rules(
            RULE_TIERED,
            UNIT_PER_UNIT,
            &[
                rule(dec("0"), Some(dec("100")), dec("1")),
                rule(dec("100"), Some(dec("200")), dec("2")),
            ],
        )
        .expect_err("末档带上限");
        assert!(error.msg.contains("最后一档"), "{}", error.msg);

        let error = checked_rules(
            RULE_TIERED,
            UNIT_PER_UNIT,
            &[
                rule(dec("0"), Some(dec("100")), dec("1")),
                rule(dec("0"), None, dec("2")),
            ],
        )
        .expect_err("下限重复");
        assert!(error.msg.contains("重复"), "{}", error.msg);

        let error =
            checked_rules(RULE_TIERED, UNIT_PER_UNIT, &[rule(dec("0"), None, dec("1"))]).expect_err("阶梯只有一档");
        assert!(error.msg.contains("至少要 2 档"), "{}", error.msg);
        println!("[M3-15] 阶梯：乱序重排 / 首档 0 / 首尾相接 / 末档开口 / 下限唯一 / 至少 2 档 ✓");
    }

    /// 通用边界：0 元档、上下限相等、负数、小数位超限、档数超上限。
    #[test]
    fn numeric_boundaries_are_rejected() {
        assert!(
            checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(dec("0"), None, dec("0"))]).is_err(),
            "0 元档"
        );
        let error = checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(dec("-1"), None, dec("1"))]).expect_err("负档位");
        assert!(error.msg.contains("不能为负"), "{}", error.msg);
        assert!(
            checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(dec("0.00001"), None, dec("1"))]).is_err(),
            "档位 5 位小数"
        );
        assert!(
            checked_rules(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(dec("0"), None, dec("0.0000001"))]).is_err(),
            "比率 7 位小数"
        );

        let many: Vec<RuleInput> = (0..=MAX_TIERS)
            .map(|index| rule(Decimal::from(index), None, dec("1")))
            .collect();
        let error = checked_rules(RULE_TIERED, UNIT_PER_UNIT, &many).expect_err("超上限");
        assert!(error.msg.contains("最多 20 档"), "{}", error.msg);
        println!("[M3-15] 0 元档 / 负数 / 小数位 / 档数上限 全部 400 ✓");
    }

    /// 落档：左闭右开、末档无上限（M3-16 的唯一实现）。
    #[test]
    fn pick_tier_is_half_open() {
        let rules = checked_rules(
            RULE_TIERED,
            UNIT_PER_UNIT,
            &[
                rule(dec("0"), Some(dec("100")), dec("0.5")),
                rule(dec("100"), Some(dec("500")), dec("1")),
                rule(dec("500"), None, dec("2")),
            ],
        )
        .expect("合法三档");

        assert_eq!(pick_tier(&rules, dec("0")).expect("0 落第一档").seq, 1);
        assert_eq!(pick_tier(&rules, dec("99.9999")).expect("99.9999 落第一档").seq, 1);
        // 99.9999 是小数；100 正好是边界 → 落第二档（左闭右开，不会两档都算）
        assert_eq!(pick_tier(&rules, dec("100")).expect("100 落第二档").seq, 2);
        assert_eq!(pick_tier(&rules, dec("499.9999")).expect("499.9999 落第二档").seq, 2);
        assert_eq!(pick_tier(&rules, dec("500")).expect("500 落第三档").seq, 3);
        assert_eq!(pick_tier(&rules, dec("99999999")).expect("超大值落末档").seq, 3);
        println!("[M3-15] 落档：[0,100) / [100,500) / [500,∞)，边界值 100/500 归下一档 ✓");
    }

    /// 状态 / 粒度 / 类型的归一化与拒绝。
    #[test]
    fn enum_normalization_and_rejection() {
        assert_eq!(checked_status(1).expect("1"), STATUS_ENABLED);
        assert_eq!(checked_status(0).expect("0"), STATUS_DISABLED);
        assert!(checked_status(2).is_err(), "状态 2 必须拒");
        assert_eq!(checked_settle_period(" quarter ").expect("季度"), SETTLE_QUARTER);
        assert!(checked_settle_period("YEAR").is_err(), "年度粒度不在域内");
        assert_eq!(checked_rule_type(" by_qty ").expect("按量"), RULE_BY_QTY);
        assert!(checked_rule_type("TIER").is_err(), "TIER ≠ TIERED（不猜）");
        println!("[M3-15] 归一化 trim+大写；域外值一律 400（不猜、不回落）✓");
    }

    /// 缺 `rate` 的档位 → 400 并点名第几档。
    #[test]
    fn missing_rate_names_the_tier() {
        let error = RebateRuleReq {
            tier_from: Some(dec("0")),
            tier_to: None,
            rate: None,
            remark: None,
        }
        .to_input(2)
        .expect_err("缺 rate 必须拒");
        assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);
        assert!(error.msg.contains("第 3 档"), "{}", error.msg);
        println!("[M3-15] 缺 rate → 400 + 66089 且点名「第 3 档」✓");
    }
}

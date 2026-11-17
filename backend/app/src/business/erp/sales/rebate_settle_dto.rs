//! 销售返利结算 · 接口契约与纯计算（M3-16）。
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | POST | `/business/erp/rebate-settlement/preview` | `business:erp:rebate-settlement:calc` |
//! | POST | `/business/erp/rebate-settlement` | `business:erp:rebate-settlement:calc` |
//! | POST | `/business/erp/rebate-settlement/run` | `business:erp:rebate-settlement:calc` |
//! | POST | `/business/erp/rebate-settlement/{id}/recalc` | `business:erp:rebate-settlement:calc` |
//! | GET | `/business/erp/rebate-settlement/page` | `business:erp:rebate-settlement:list` |
//! | GET | `/business/erp/rebate-settlement/{id}` | `business:erp:rebate-settlement:list` |
//! | DELETE | `/business/erp/rebate-settlement/{id}` | `business:erp:rebate-settlement:delete` |
//!
//! # 本卡算什么（一句话）
//!
//! 「某客户某个结算期间（月 / 季）的**净销售**，按该客户该粒度**覆盖整期**的返利政策落档，
//! 乘出应返金额」。落档只有一份实现：[`rebate_dto::pick_tier`]（M3-15 交付，本卡直接调，
//! 不重写第二份）；三种规则类型的**形状**校验也只有一份：[`rebate_dto::checked_rules`]。
//!
//! # 基数口径（与销售目标 / 收款口径同一条链，别另立一套）
//!
//! ```text
//! 销售额 = Σ 订单本位币含税金额  −  Σ 退货含税金额（折本位币）
//!          （base_total_with_tax，为 0 时回落 total_with_tax）
//! 销量   = Σ 订单明细 qty      −  Σ 退货明细 qty
//!
//! 订单白名单 = APPROVED / PARTIAL_OUT / OUTBOUND / CLOSED
//!              （DRAFT / SUBMITTED 还没算销售，CANCELLED 不算）
//! 退货白名单 = POSTED（没冲减账的退货先减返利 = 提前认可一笔还没发生的减少）
//! 期间归属   = 订单按 order_date、退货按 return_date，一律左闭右开 [from, to)
//! ```
//!
//! # 为什么「净基数」可以为负，而「应返金额」恒 ≥ 0
//!
//! 退货多于销售是真实业务（上个月的货这个月全退）。净基数为负时：
//! 落档基数按 **0** 算（不落负档、不产生负返利），`base_amount` / `base_qty` 仍存**原值**
//! —— 报表要能看见「这个月没戏」，而应付侧只需要知道「这期一分不返」。
//! 把返利算成负数等于凭空生成一笔红字应付，那是 M3-17 红冲/调账的事，不是结算的事。
//!
//! # 为什么要「政策覆盖整期」
//!
//! 政策 11-15 才生效、期间是 11 月全月时：前 14 天按什么返？套新政策会把整月都算进去
//! （超额返利），按 0 算又少给一半。两种都是**有争议的数**。所以本卡的选择是**不算**，
//! 报 `66091` 并把缺口日期写进报文，让人去补政策 —— 结算金额不猜。
//!
//! # 金额精度
//!
//! 应返金额保留 **2 位**（分），与 `sales_target_dto::money2` / 对账单 `money()` 同口径；
//! 落库列仍是 `NUMERIC(18,4)`（仓内金额列统一精度），多出的两位恒为 0。
//!
//! # 快照
//!
//! 结算单落库时把政策条款整体快照下来（[`PolicySnapshot`]：政策头 + **全部档位**），
//! 因为政策在结算之后会被改名、改档位、停用、软删 —— 只存 `policy_id` 就再也回溯不出
//! 「当时按什么算」。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::sales::rebate_dto::{
    self, RULE_BY_AMOUNT, RULE_BY_QTY, RULE_TIERED, RuleWrite, UNIT_PER_UNIT, UNIT_PERCENT, pick_tier,
};
use crate::business::erp::sales::sales_target_dto::Period;
use cloudpivot_kernel::shared::error_code;

// ==================== 域值（与 V0439 的 CHECK 逐字一致，单测钉住） ====================

/// 已计算（可重算 / 可作废）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 已转应付或红字发票（**M3-17 置位**；本卡只读不写）。
pub const STATUS_SETTLED: &str = "SETTLED";
/// 已作废（行保留供审计；同期间可重开一张）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 全部结算状态（顺序即前端标签顺序）。
pub const STATUSES: [&str; 3] = [STATUS_DRAFT, STATUS_SETTLED, STATUS_CANCELLED];

/// 计入销售基数的订单状态（与 `sales_target_dto::ORDER_STATUSES` **同一个常量**，不另抄一份）。
pub use crate::business::erp::sales::sales_target_dto::ORDER_STATUSES;

/// 计入退货基数的退货状态：只有**已过账**的退货才真的冲减了账。
pub const RETURN_STATUS_POSTED: &str = "POSTED";

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 政策档位与结算基数对不上（400 + 66089）：只可能发生在政策被绕过接口改歪时。
fn rule_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REBATE_RULE_INVALID, message.into())
}

// ==================== 期间 ====================

/// 一个结算期间：粒度 + 期间码 + 左闭右开区间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodArgs {
    /// 结算粒度（`MONTH` / `QUARTER`，已归一化大写）。
    pub settle_period: String,
    /// 期间码（`yyyy-MM` / `yyyy-QN`，季度 `Q` 已归一化大写）。
    pub period_code: String,
    /// 期间首日（含）。
    pub from: NaiveDate,
    /// 期间末日 +1（不含）。
    pub to: NaiveDate,
}

impl PeriodArgs {
    /// 解析「粒度 + 期间码」，并校验**两者形状互配**。
    ///
    /// 期间码的日期换算复用 `sales_target_dto::Period`（目标/预测已经用了它，
    /// 再来一套就是把「季度怎么算」写第二遍）；本函数只补两件事：
    /// ① 粒度归一化（`month` → `MONTH`）；② **月度不能配 `yyyy-QN`、季度不能配 `yyyy-MM`**
    /// （库层 `ck_biz_srs_period_shape` 是同一句话，两处都挡）。
    pub fn resolve(settle_period: &str, period_code: &str) -> std::result::Result<Self, ApiError> {
        let settle_period = rebate_dto::checked_settle_period(settle_period)?;
        let period = Period::from_code(period_code)?;
        let is_quarter_code = period.code.as_bytes().get(5) == Some(&b'Q');
        match settle_period.as_str() {
            rebate_dto::SETTLE_MONTH if is_quarter_code => {
                return Err(bad_request(format!(
                    "settle_period=MONTH（月度）要配 yyyy-MM 期间码，不能配季度期间码「{}」",
                    period.code
                )));
            }
            rebate_dto::SETTLE_QUARTER if !is_quarter_code => {
                return Err(bad_request(format!(
                    "settle_period=QUARTER（季度）要配 yyyy-QN 期间码，不能配月度期间码「{}」",
                    period.code
                )));
            }
            _ => {}
        }
        Ok(Self {
            settle_period,
            period_code: period.code,
            from: period.from,
            to: period.to,
        })
    }

    /// 期间末日（含）= `to - 1` 天（政策覆盖判定用：政策的 `effective_to` 是**含**的）。
    pub fn last_day(&self) -> NaiveDate {
        self.to.pred_opt().unwrap_or(self.to)
    }
}

// ==================== 基数 ====================

/// 期间的销售/退货基数（原值，**净额在方法里现算**，不存两处）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaseNumbers {
    /// 计入的已确认销售订单张数。
    pub order_count: i64,
    /// 订单本位币含税金额合计。
    pub order_amount: Decimal,
    /// 订单明细数量合计。
    pub order_qty: Decimal,
    /// 计入的已过账退货张数。
    pub return_count: i64,
    /// 退货含税金额合计（折本位币）。
    pub return_amount: Decimal,
    /// 退货明细数量合计。
    pub return_qty: Decimal,
}

impl BaseNumbers {
    /// 净销售额 = 订单 − 退货（可能为负）。
    pub fn net_amount(&self) -> Decimal {
        self.order_amount - self.return_amount
    }

    /// 净销量 = 订单 − 退货（可能为负）。
    pub fn net_qty(&self) -> Decimal {
        self.order_qty - self.return_qty
    }
}

// ==================== 纯计算 ====================

/// 一次结算的计算结果（不落库也能完整解释「为什么是这个数」）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalcOutcome {
    /// 用于落档的基数（净额，**负数按 0 计**）。
    pub pick_value: Decimal,
    /// 命中的档（`None` 只可能发生在政策被绕过接口改歪时，[`calc_rebate`] 会先报错）。
    pub tier: RuleWrite,
    /// 应返金额（2 位小数，恒 ≥ 0）。
    pub rebate_amount: Decimal,
    /// 给人看的提示（无订单 / 负基数 …），不阻断结算。
    pub warnings: Vec<String>,
}

/// 按政策口径算应返金额（**纯函数**：不连库、不看状态，只认规则与基数）。
///
/// # 口径
///
/// ```text
/// pick_value = PER_UNIT ? 净销量 : 净销售额        （负数按 0）
/// 命中档     = pick_tier(rules, pick_value)        （左闭右开、末档无上限、不累进）
/// 应返金额   = PER_UNIT ? pick_value × rate        （元/件 × 件）
///            :            pick_value × rate / 100  （百分数）
/// ```
///
/// # 异常情况
///
/// - 规则类型 / 计价单位不配对（`BY_QTY` 配 `PERCENT` 之类）→ 400 + 66089；
/// - 没有一档命中的基数（首档不从 0 起 —— 只可能是绕过接口写歪）→ 400 + 66089。
pub fn calc_rebate(
    rule_type: &str,
    rate_unit: &str,
    rules: &[RuleWrite],
    base: &BaseNumbers,
) -> std::result::Result<CalcOutcome, ApiError> {
    match (rule_type, rate_unit) {
        (RULE_BY_QTY, UNIT_PER_UNIT) | (RULE_BY_AMOUNT, UNIT_PERCENT) | (RULE_TIERED, _) => {}
        _ => {
            return Err(rule_invalid(format!(
                "政策的规则类型与计价单位不配对（rule_type={rule_type} / rate_unit={rate_unit}）：\
                 按量只能是元/件、按额只能是百分比"
            )));
        }
    }
    if rules.is_empty() {
        return Err(rule_invalid("政策没有任何档位规则，无法落档"));
    }

    let raw_value = if rate_unit == UNIT_PER_UNIT {
        base.net_qty()
    } else {
        base.net_amount()
    };
    let pick_value = if raw_value.is_sign_negative() {
        Decimal::ZERO
    } else {
        raw_value
    };

    let mut warnings = Vec::new();
    if base.order_count == 0 && base.return_count == 0 {
        warnings.push(format!(
            "期间内没有已确认销售订单、也没有已过账退货（{} 起算），应返 0",
            ORDER_STATUSES.join(" / ")
        ));
    }
    if raw_value.is_sign_negative() {
        warnings.push(format!(
            "净{}为负（退货多于销售：{}），落档基数按 0 计，应返 0；\
             负数差额请走红冲/调账，不在这里算负返利",
            if rate_unit == UNIT_PER_UNIT {
                "销量"
            } else {
                "销售额"
            },
            money2(raw_value)
        ));
    }

    let tier = pick_tier(rules, pick_value).ok_or_else(|| {
        rule_invalid(format!(
            "政策的档位没有覆盖结算基数 {pick_value}（首档下限应为 0、末档应为开口）——\
             政策档位可能被绕过接口改过，请先在返利政策页修好档位"
        ))
    })?;

    let rebate = if rate_unit == UNIT_PER_UNIT {
        pick_value * tier.rate
    } else {
        pick_value * tier.rate / Decimal::from(100)
    };

    Ok(CalcOutcome {
        pick_value,
        tier: tier.clone(),
        rebate_amount: money2(rebate),
        warnings,
    })
}

/// 金额保留 2 位（分）。
pub fn money2(value: Decimal) -> Decimal {
    value.round_dp(2)
}

// ==================== 域值标签 ====================

/// 状态中文（`SETTLED` 是「已开红字发票」：M3-17 定的终态口径 —— 卡面「生成应付或红字发票
/// 二选一」选了红字发票那一支，`SETTLED` 的含义就是「票已开、收入与销项税已冲」）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "已计算",
        STATUS_SETTLED => "已开红字发票",
        STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

// ==================== 政策快照 ====================

/// 政策条款快照（落 `policy_snapshot` JSONB；**全部档位**都在里面）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicySnapshot {
    /// 政策编号。
    pub policy_no: String,
    /// 政策名称。
    pub policy_name: String,
    /// 规则类型。
    pub rule_type: String,
    /// 计价单位。
    pub rate_unit: String,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）；`null` = 长期有效。
    pub effective_to: Option<NaiveDate>,
    /// 全部档位（按 `seq` 升序）。
    pub tiers: Vec<PolicyTierSnapshot>,
}

/// 政策档位快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyTierSnapshot {
    /// 档号（1 起）。
    pub seq: i16,
    /// 本档下限（含）。
    pub tier_from: Decimal,
    /// 本档上限（不含）；`null` = 无上限。
    pub tier_to: Option<Decimal>,
    /// 比率（元/件 或 百分数）。
    pub rate: Decimal,
    /// 本档备注。
    pub remark: Option<String>,
}

impl From<&RuleWrite> for PolicyTierSnapshot {
    fn from(rule: &RuleWrite) -> Self {
        Self {
            seq: rule.seq,
            tier_from: rule.tier_from,
            tier_to: rule.tier_to,
            rate: rule.rate,
            remark: rule.remark.clone(),
        }
    }
}

// ==================== 出参 ====================

/// 基数出参（列表/详情/试算同形）。
#[derive(Debug, Clone, Serialize)]
pub struct RebateBaseRes {
    /// 计入的已确认销售订单张数。
    pub order_count: i64,
    /// 订单本位币含税金额合计。
    pub order_amount: Decimal,
    /// 订单明细数量合计。
    pub order_qty: Decimal,
    /// 计入的已过账退货张数。
    pub return_count: i64,
    /// 退货含税金额合计（折本位币）。
    pub return_amount: Decimal,
    /// 退货明细数量合计。
    pub return_qty: Decimal,
    /// 净销售额。
    pub net_amount: Decimal,
    /// 净销量。
    pub net_qty: Decimal,
}

impl From<&BaseNumbers> for RebateBaseRes {
    fn from(base: &BaseNumbers) -> Self {
        Self {
            order_count: base.order_count,
            order_amount: base.order_amount,
            order_qty: base.order_qty,
            return_count: base.return_count,
            return_amount: base.return_amount,
            return_qty: base.return_qty,
            net_amount: money2(base.net_amount()),
            net_qty: base.net_qty(),
        }
    }
}

/// 结算单（详情与列表同形）。
#[derive(Debug, Clone, Serialize)]
pub struct RebateSettleRes {
    /// 主键。
    pub id: String,
    /// 结算单号（`RB-yyyyMMdd-000001`）。
    pub settlement_no: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（客户已软删时为 `null`）。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 结算粒度。
    pub settle_period: String,
    /// 粒度中文（月度 / 季度）。
    pub settle_period_label: String,
    /// 期间码。
    pub period_code: String,
    /// 期间首日（含）。
    pub period_from: NaiveDate,
    /// 期间末日 +1（不含）。
    pub period_to: NaiveDate,
    /// 期间末日的「含」展示值（= period_to − 1）。
    pub period_last_day: NaiveDate,
    /// 政策 id。
    pub policy_id: String,
    /// 政策编号（快照）。
    pub policy_no: String,
    /// 政策名称（快照）。
    pub policy_name: String,
    /// 规则类型（快照）。
    pub rule_type: String,
    /// 规则类型中文。
    pub rule_type_label: String,
    /// 计价单位（快照）。
    pub rate_unit: String,
    /// 计价单位中文。
    pub rate_unit_label: String,
    /// 政策生效日（快照）。
    pub policy_effective_from: NaiveDate,
    /// 政策失效日（快照）。
    pub policy_effective_to: Option<NaiveDate>,
    /// 政策全部档位（快照）。
    pub policy_tiers: Vec<PolicyTierSnapshot>,
    /// 基数。
    pub base: RebateBaseRes,
    /// 命中档号（未命中为 `null`）。
    pub tier_seq: Option<i16>,
    /// 命中档下限。
    pub tier_from: Decimal,
    /// 命中档上限。
    pub tier_to: Option<Decimal>,
    /// 命中档比率。
    pub rate: Decimal,
    /// 应返金额。
    pub rebate_amount: Decimal,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 红字发票 id（M3-17；未开票为 `null`）。
    pub invoice_id: Option<String>,
    /// 红字发票号（M3-17；已开票后列表直接显示，免二次请求）。
    pub invoice_no: Option<String>,
    /// 转结时间（M3-17；置 `SETTLED` 的时刻）。
    pub settled_at: Option<DateTime<Utc>>,
}

/// 结算单摘要（试算里回显「这期已经算过」用）。
#[derive(Debug, Clone, Serialize)]
pub struct RebateSettleBriefRes {
    /// 主键。
    pub id: String,
    /// 结算单号。
    pub settlement_no: String,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 应返金额。
    pub rebate_amount: Decimal,
}

/// 试算出参（**不落库**，用于「算一下看看」）。
#[derive(Debug, Clone, Serialize)]
pub struct RebatePreviewRes {
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 结算粒度。
    pub settle_period: String,
    /// 粒度中文。
    pub settle_period_label: String,
    /// 期间码。
    pub period_code: String,
    /// 期间首日（含）。
    pub period_from: NaiveDate,
    /// 期间末日 +1（不含）。
    pub period_to: NaiveDate,
    /// 期间末日的「含」展示值。
    pub period_last_day: NaiveDate,
    /// 适用政策（`null` = 该期间无覆盖整期的启用政策，此时 `rebate_amount = 0`）。
    pub policy: Option<RebatePreviewPolicyRes>,
    /// 基数。
    pub base: RebateBaseRes,
    /// 命中档号。
    pub tier_seq: Option<i16>,
    /// 命中档下限。
    pub tier_from: Decimal,
    /// 命中档上限。
    pub tier_to: Option<Decimal>,
    /// 命中档比率。
    pub rate: Decimal,
    /// 应返金额。
    pub rebate_amount: Decimal,
    /// 提示（无订单 / 负基数 / 无政策 …）。
    pub warnings: Vec<String>,
    /// 该客户该期间已有的结算单（未作废的；`null` = 还没算过）。
    pub existing: Option<RebateSettleBriefRes>,
}

/// 试算里的政策片段。
#[derive(Debug, Clone, Serialize)]
pub struct RebatePreviewPolicyRes {
    /// 政策 id。
    pub policy_id: String,
    /// 政策编号。
    pub policy_no: String,
    /// 政策名称。
    pub policy_name: String,
    /// 规则类型。
    pub rule_type: String,
    /// 计价单位。
    pub rate_unit: String,
    /// 生效日（含）。
    pub effective_from: NaiveDate,
    /// 失效日（含）。
    pub effective_to: Option<NaiveDate>,
    /// 全部档位。
    pub tiers: Vec<PolicyTierSnapshot>,
}

/// 跑批里单个客户的结果。
#[derive(Debug, Clone, Serialize)]
pub struct RebateRunItemRes {
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 结果（`CREATED` 新建 / `EXISTS` 已有 / `FAILED` 失败）。
    pub outcome: String,
    /// 结算单 id（`CREATED` / `EXISTS` 时有）。
    pub settlement_id: Option<String>,
    /// 结算单号（`CREATED` / `EXISTS` 时有）。
    pub settlement_no: Option<String>,
    /// 客户结算单状态（`CREATED` / `EXISTS` 时有）。
    pub status: Option<String>,
    /// 应返金额（`CREATED` / `EXISTS` 时有）。
    pub rebate_amount: Option<Decimal>,
    /// 失败原因（`FAILED` 时有）。
    pub message: Option<String>,
}

/// 跑批出参。
#[derive(Debug, Clone, Serialize)]
pub struct RebateRunRes {
    /// 结算粒度。
    pub settle_period: String,
    /// 粒度中文。
    pub settle_period_label: String,
    /// 期间码。
    pub period_code: String,
    /// 期间首日（含）。
    pub period_from: NaiveDate,
    /// 期间末日 +1（不含）。
    pub period_to: NaiveDate,
    /// 命中政策数（= 参与跑批的客户数）。
    pub total: usize,
    /// 新建张数。
    pub created: usize,
    /// 已有跳过张数。
    pub skipped: usize,
    /// 失败张数。
    pub failed: usize,
    /// 逐客户结果。
    pub items: Vec<RebateRunItemRes>,
}

// ==================== 入参 ====================

/// 计算 / 试算入参（试算忽略 `remark`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RebateSettleReq {
    /// 客户 id（UUID）。
    #[serde(default)]
    pub customer_id: String,
    /// 结算粒度（`MONTH` / `QUARTER`）。
    #[serde(default)]
    pub settle_period: String,
    /// 期间码（`yyyy-MM` / `yyyy-QN`）。
    #[serde(default)]
    pub period_code: String,
    /// 备注（重算原因等）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 整期跑批入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RebateRunReq {
    /// 结算粒度（`MONTH` / `QUARTER`）。
    #[serde(default)]
    pub settle_period: String,
    /// 期间码（`yyyy-MM` / `yyyy-QN`）。
    #[serde(default)]
    pub period_code: String,
    /// 只跑这些客户（空 = 该期间有政策的全部客户）。
    #[serde(default)]
    pub customer_ids: Vec<String>,
    /// 备注（写进新建的结算单）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 结算单列表筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RebateSettleFilter {
    /// 关键字：结算单号 / 客户编码 / 客户名称 / 政策编号 模糊。
    #[serde(default)]
    pub keyword: Option<String>,
    /// 客户精确。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 结算粒度精确。
    #[serde(default)]
    pub settle_period: Option<String>,
    /// 期间码精确。
    #[serde(default)]
    pub period_code: Option<String>,
    /// 状态精确（`DRAFT` / `SETTLED` / `CANCELLED`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 政策精确。
    #[serde(default)]
    pub policy_id: Option<String>,
    /// 页码（1 起）。
    #[serde(default)]
    pub page: u64,
    /// 每页条数（缺省 20，上限 200）。
    #[serde(default)]
    pub page_size: u64,
}

/// 备注长度上限（`remark` 是 `TEXT`，这里只是防呆）。
pub const MAX_REMARK_LEN: usize = 500;

/// 备注归一化：去首尾空白、空串转 `None`、超长 400。
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

// ==================== 单测（纯逻辑，不碰库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试字面量必须是合法小数")
    }

    fn rule(seq: i16, from: &str, to: Option<&str>, rate: &str) -> RuleWrite {
        RuleWrite {
            seq,
            tier_from: dec(from),
            tier_to: to.map(dec),
            rate: dec(rate),
            remark: None,
        }
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    /// 域值与 V0439 的 CHECK 逐字一致。
    #[test]
    fn statuses_match_migration() {
        assert_eq!(STATUSES, ["DRAFT", "SETTLED", "CANCELLED"]);
        assert_eq!(RETURN_STATUS_POSTED, "POSTED");
        // 订单白名单复用销售目标那份，不另抄
        assert_eq!(ORDER_STATUSES, ["APPROVED", "PARTIAL_OUT", "OUTBOUND", "CLOSED"]);
        assert_eq!(status_label("DRAFT"), "已计算");
        // M3-17 定稿：SETTLED 的终态语义 = 已开红字发票（卡面「生成应付或红字发票」二选一取了红字票）
        assert_eq!(status_label("SETTLED"), "已开红字发票");
        assert_eq!(status_label("CANCELLED"), "已作废");
        println!("[M3-16] 状态 / 退货状态 / 订单白名单与常量对齐 ✓");
    }

    /// 期间解析：月度只能配 yyyy-MM、季度只能配 yyyy-QN；边界是左闭右开。
    #[test]
    fn period_resolve_shape_and_bounds() {
        let month = PeriodArgs::resolve("month", "2026-11").expect("月度");
        assert_eq!(month.settle_period, "MONTH");
        assert_eq!(month.period_code, "2026-11");
        assert_eq!(month.from, day("2026-11-01"));
        assert_eq!(month.to, day("2026-12-01"), "左闭右开：末日 +1");
        assert_eq!(month.last_day(), day("2026-11-30"));

        let quarter = PeriodArgs::resolve("QUARTER", "2026-q4").expect("季度（小写 q 归一）");
        assert_eq!(quarter.period_code, "2026-Q4");
        assert_eq!(quarter.from, day("2026-10-01"));
        assert_eq!(quarter.to, day("2027-01-01"), "跨年季度");
        assert_eq!(quarter.last_day(), day("2026-12-31"));

        // 形状不互配必须被拒（库层 ck_biz_srs_period_shape 同一句话）
        let cross = PeriodArgs::resolve("MONTH", "2026-Q4").expect_err("月度配季度码");
        assert_eq!(cross.status, 400);
        assert!(cross.msg.contains("yyyy-MM"), "{}", cross.msg);
        let cross = PeriodArgs::resolve("QUARTER", "2026-11").expect_err("季度配月度码");
        assert!(cross.msg.contains("yyyy-QN"), "{}", cross.msg);
        for bad in ["2026-13", "2026-Q5", "2026/11", "202611"] {
            assert!(
                PeriodArgs::resolve("MONTH", bad).is_err() || PeriodArgs::resolve("QUARTER", bad).is_err(),
                "非法期间码必须被拒：{bad}"
            );
        }
        println!("[M3-16] 期间：月度/季度形状互钉、区间左闭右开、跨年季度正确 ✓");
    }

    /// 按量：净销量 × 元/件。
    #[test]
    fn by_qty_multiplies_net_qty() {
        let base = BaseNumbers {
            order_count: 2,
            order_amount: dec("1000"),
            order_qty: dec("120"),
            return_count: 1,
            return_amount: dec("100"),
            return_qty: dec("20"),
        };
        let outcome = calc_rebate(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(1, "0", None, "0.5")], &base).expect("按量能算");
        assert_eq!(outcome.pick_value, dec("100"), "落档基数是净销量");
        assert_eq!(outcome.rebate_amount, dec("50.00"), "100 件 × 0.5 元/件");
        assert!(outcome.warnings.is_empty());
        println!("[M3-16] 按量：净销量 100 × 0.5 = 50 ✓");
    }

    /// 按额：净销售额 × 百分比。
    #[test]
    fn by_amount_uses_percent() {
        let base = BaseNumbers {
            order_count: 1,
            order_amount: dec("1130"),
            order_qty: dec("10"),
            return_count: 0,
            return_amount: Decimal::ZERO,
            return_qty: Decimal::ZERO,
        };
        let outcome = calc_rebate(RULE_BY_AMOUNT, UNIT_PERCENT, &[rule(1, "0", None, "2.5")], &base).expect("按额能算");
        assert_eq!(outcome.pick_value, dec("1130"), "落档基数是净销售额（含税）");
        assert_eq!(outcome.rebate_amount, dec("28.25"), "1130 × 2.5% = 28.25");
        println!("[M3-16] 按额：1130 × 2.5% = 28.25 ✓");
    }

    /// 阶梯按量：落档到命中那一档、**按该档比率全额乘**（不累进）。
    #[test]
    fn tiered_per_unit_picks_one_tier() {
        let rules = [rule(1, "0", Some("100"), "1"), rule(2, "100", None, "2")];
        let base = |qty: &str| BaseNumbers {
            order_count: 1,
            order_amount: dec("9999"),
            order_qty: dec(qty),
            ..Default::default()
        };
        // 左闭右开：99 落第 1 档、100 落第 2 档
        let at_99 = calc_rebate(RULE_TIERED, UNIT_PER_UNIT, &rules, &base("99")).expect("99");
        assert_eq!(at_99.tier.seq, 1);
        assert_eq!(at_99.rebate_amount, dec("99.00"));
        let at_100 = calc_rebate(RULE_TIERED, UNIT_PER_UNIT, &rules, &base("100")).expect("100");
        assert_eq!(at_100.tier.seq, 2, "100 是第 2 档的下限（左闭）");
        assert_eq!(at_100.rebate_amount, dec("200.00"), "150→200 都不是累进：150×2=300");
        let at_150 = calc_rebate(RULE_TIERED, UNIT_PER_UNIT, &rules, &base("150")).expect("150");
        assert_eq!(
            at_150.rebate_amount,
            dec("300.00"),
            "不累进：150 × 2 = 300（不是 100×1+50×2）"
        );
        println!("[M3-16] 阶梯按量：左闭右开落档、按该档比率全额乘（不累进）✓");
    }

    /// 阶梯按额：`PERCENT` 时档位按销售额分。
    #[test]
    fn tiered_percent_picks_by_amount() {
        let rules = [rule(1, "0", Some("10000"), "1"), rule(2, "10000", None, "3")];
        let base = BaseNumbers {
            order_count: 1,
            order_amount: dec("12000"),
            order_qty: dec("1"),
            ..Default::default()
        };
        let outcome = calc_rebate(RULE_TIERED, UNIT_PERCENT, &rules, &base).expect("阶梯按额");
        assert_eq!(outcome.pick_value, dec("12000"));
        assert_eq!(outcome.tier.seq, 2);
        assert_eq!(outcome.rebate_amount, dec("360.00"), "12000 × 3% = 360");
        println!("[M3-16] 阶梯按额：按销售额落档，12000 × 3% = 360 ✓");
    }

    /// 负基数：落档按 0、应返 0、给提示（不生成负返利）。
    #[test]
    fn negative_base_clamps_to_zero_and_warns() {
        let base = BaseNumbers {
            order_count: 1,
            order_amount: dec("100"),
            order_qty: dec("10"),
            return_count: 1,
            return_amount: dec("300"),
            return_qty: dec("30"),
        };
        let outcome =
            calc_rebate(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(1, "0", None, "0.5")], &base).expect("负基数也要能算");
        assert_eq!(outcome.pick_value, Decimal::ZERO, "落档基数按 0");
        assert_eq!(outcome.rebate_amount, Decimal::ZERO, "应返 0（不生成负返利）");
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("退货多于销售"), "{:?}", outcome.warnings);
        println!("[M3-16] 负基数：落档按 0、应返 0、报文提示走红冲 ✓");
    }

    /// 空期间（无单无退）：应返 0 并提示（不是错误）。
    #[test]
    fn empty_period_is_zero_with_warning() {
        let outcome = calc_rebate(
            RULE_BY_AMOUNT,
            UNIT_PERCENT,
            &[rule(1, "0", None, "5")],
            &BaseNumbers::default(),
        )
        .expect("空期间也能算");
        assert_eq!(outcome.rebate_amount, Decimal::ZERO);
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].contains("没有已确认销售订单"));
        println!("[M3-16] 空期间：应返 0 + 提示（不报错）✓");
    }

    /// 单位与类型不配对 / 档位不覆盖基数 → 400 + 66089（不静默算错）。
    #[test]
    fn mismatched_unit_and_uncovered_base_are_rejected() {
        let base = BaseNumbers {
            order_count: 1,
            order_amount: dec("100"),
            order_qty: dec("100"),
            ..Default::default()
        };
        let error =
            calc_rebate(RULE_BY_QTY, UNIT_PERCENT, &[rule(1, "0", None, "1")], &base).expect_err("按量配百分比");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);

        let error = calc_rebate(RULE_BY_QTY, UNIT_PER_UNIT, &[], &base).expect_err("没有档位");
        assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);

        // 档位区间不覆盖基数（绕过接口写歪）→ 落不到档，报错而不是静默返回 0
        let error =
            calc_rebate(RULE_BY_QTY, UNIT_PER_UNIT, &[rule(1, "0", Some("50"), "1")], &base).expect_err("档位不覆盖");
        assert_eq!(error.code, error_code::ERP_REBATE_RULE_INVALID);
        assert!(error.msg.contains("没有覆盖"), "{}", error.msg);
        println!("[M3-16] 不配对/无档位/档位不覆盖 三种脏政策 → 400 + 66089 ✓");
    }

    /// 应返金额保留 2 位（分），不出现「0.30000000000000000001」这类尾巴。
    #[test]
    fn rebate_amount_keeps_two_decimals() {
        let base = BaseNumbers {
            order_count: 1,
            order_amount: dec("3"),
            order_qty: dec("3"),
            ..Default::default()
        };
        let outcome = calc_rebate(RULE_BY_AMOUNT, UNIT_PERCENT, &[rule(1, "0", None, "10")], &base).expect("算");
        assert_eq!(outcome.rebate_amount, dec("0.30"));
        assert_eq!(outcome.rebate_amount.scale(), 2);
        println!("[M3-16] 应返金额 2 位小数（3 × 10% = 0.30）✓");
    }

    /// 备注归一化：空白转 None、超长 400。
    #[test]
    fn remark_is_trimmed_and_bounded() {
        assert_eq!(normalize_remark(Some("   ")).expect("空白"), None);
        assert_eq!(
            normalize_remark(Some("  退货多于销售，重算  ")).expect("正常"),
            Some("退货多于销售，重算".to_string())
        );
        let long = "重".repeat(MAX_REMARK_LEN + 1);
        assert_eq!(normalize_remark(Some(&long)).expect_err("超长").status, 400);
        println!("[M3-16] 备注：去空白 / 空串转 None / 超 {MAX_REMARK_LEN} 字 400 ✓");
    }

    /// 快照 JSON 能序列化再读回（落库列是 JSONB，形状错了 M3-17 就取不到档位）。
    #[test]
    fn policy_snapshot_roundtrip() {
        let snapshot = PolicySnapshot {
            policy_no: "BR-20260101-000001".to_string(),
            policy_name: "2026 阶梯返利".to_string(),
            rule_type: RULE_TIERED.to_string(),
            rate_unit: UNIT_PER_UNIT.to_string(),
            effective_from: day("2026-01-01"),
            effective_to: None,
            tiers: vec![
                PolicyTierSnapshot::from(&rule(1, "0", Some("100"), "1")),
                PolicyTierSnapshot::from(&rule(2, "100", None, "2")),
            ],
        };
        let text = serde_json::to_string(&snapshot).expect("序列化");
        let back: PolicySnapshot = serde_json::from_str(&text).expect("反序列化");
        assert_eq!(back, snapshot);
        assert!(text.contains("\"tier_to\":null"), "末档必须显式 null：{text}");
        println!("[M3-16] 政策快照 JSONB 序列化/反序列化互逆（末档 tier_to=null）✓");
    }
}

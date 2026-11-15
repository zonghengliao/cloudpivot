//! 薪资明细**手工覆盖**的入参 / 回执契约 + 汇总重算纯函数（设计 §5.2 #41 · HR-37）。
//!
//! # 这条接口存在的理由（设计 §4.8.4 末段）
//!
//! 公式永远算不准第一个月：期中入职按整月发、客户口头答应的补发、社保基数手工调 ——
//! 没有人工出口，客户第一个月就卡死。所以允许改**任意**
//! `EARNING` / `PRE_DEDUCT` / `WITHHOLD` / `EMPLOYER` 项的金额。
//!
//! # 改一项之后必须做的两件事（本卡验收项）
//!
//! 1. **重算汇总**：明细行 11 个金额列 = 本行明细项按类型/桶重新聚合（[`summarize`]），
//!    再把单头 7 个金额列 = `SUM(明细)` 重新聚合（SQL 在
//!    [`crate::business::hr::salary_sheet_model::recalc_sheet_totals`]）。
//!    只改 `biz_salary_detail_item.amount` 而不改这两层，就是「工资表数字和明细对不上」——
//!    设计 §4.9 的巡检 A6/A7 会当场把它抓成 BLOCKER。
//! 2. **重写恒等式**：`应发 = 收入 − 应发前扣减`、`代扣 = 社保 + 公积金 + 个税 + 其他`、
//!    `实发 = 应发 − 代扣` 三条既写在库层 `CHECK`（`ck_biz_sal_det_*`）也由本文件保证。
//!    **覆盖后算不出合法恒等式就不许落库**：负数工资在这里就拦下（报文说清是谁、哪个字段、
//!    差多少），而不是等库层 `23514` 变成 500。
//!
//! # 不改的东西（写清楚，别指望它会跟着变）
//!
//! * **其它项不重算**：覆盖是「人工出口」，不是「重新试算」。改了 `BASE` 之后
//!   `GROSS`/`NET` 的**公式值**不会回写 —— 它们本来就不落 `detail_item`
//!   （`SUMMARY` 不落，见 HR-34 §4.4.3 末尾注），明细行/单头的应发与实发是**聚合**出来的，
//!   所以永远与明细项一致。要按新基数重算别的公式项，走「重新试算」（HR-34）。
//! * `expr_snapshot`（公式原文快照）**不动**：它是「这一项当初是按什么公式算的」的证据，
//!   覆盖只改金额与 `calc_note`（写「人工覆盖 原值→新值」）。
//!
//! # 为什么这些是纯函数
//!
//! 「重算汇总」这件事有两个消费者：HTTP 接口（本卡）与巡检/对账（HR-45）。
//! 写成 `fn(&[ItemAmount]) -> DetailAmounts` 就能被单测逐桶钉住（`WITHHOLD` 的四个桶按
//! **编码**分、`EMPLOYER` 按**编码再科目**分），不必连库。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::hr::salary_compute::{
    EMPLOYER_FUND_CODE, EMPLOYER_SOCIAL_CODE, EmployerBucket, WithholdBucket, employer_bucket, round_amount,
    withhold_bucket,
};
use crate::business::hr::salary_item_dto::{
    self as item_dto, ITEM_TYPE_EARNING, ITEM_TYPE_EMPLOYER, ITEM_TYPE_PRE_DEDUCT, ITEM_TYPE_SUMMARY,
    ITEM_TYPE_WITHHOLD, ITEM_TYPES,
};
use crate::business::hr::salary_sheet_dto::{SalaryDetailRes, SalarySheetHeaderRes, status_label};
use crate::shared::error_code;

// ==================== 常量 ====================

/// 状态日志的动作码（`biz_status_log.action`，V0365 加的列）。
///
/// 与设计 §4.8.4 的行文逐字一致；`from_status = to_status = 当前状态`（覆盖不改状态，
/// 走 `state_machine::log_event` 而不是 `transition`）。
pub const ACTION_AMOUNT_OVERRIDE: &str = "AMOUNT_OVERRIDE";

/// 覆盖原因的最少字数（与设计 §4.2.1「原因 ≥ 5 字进 `biz_status_log.reason`」同一把尺子）。
pub const MIN_REASON_CHARS: usize = 5;

/// 覆盖原因的最大字数（`biz_status_log.reason` 是 `text`，但一句人话不该超 500）。
pub const MAX_REASON_CHARS: usize = 500;

/// 一次请求最多改几项：一屏工资条一列也就十几个输入框，64 足够；超了多半是脚本在乱调。
pub const MAX_OVERRIDE_ITEMS: usize = 64;

/// 允许覆盖的项类型（设计 §4.8.4 逐字：`EARNING/PRE_DEDUCT/WITHHOLD/EMPLOYER`）。
pub const OVERRIDABLE_TYPES: [&str; 4] = [
    ITEM_TYPE_EARNING,
    ITEM_TYPE_PRE_DEDUCT,
    ITEM_TYPE_WITHHOLD,
    ITEM_TYPE_EMPLOYER,
];

// ==================== 入参 ====================

/// 一条要覆盖的薪资项。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryOverrideItemReq {
    /// 薪资项编码（必须在本行明细项里已存在）。
    pub item_code: Option<String>,
    /// 新金额（`None` ≠ 0：没传就是没传）。
    pub amount: Option<Decimal>,
}

/// 明细覆盖入参（设计 §5.2 #41：改项金额 + `reason`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SalaryOverrideReq {
    /// 覆盖原因（**必填**，≥ [`MIN_REASON_CHARS`] 字，进 `biz_status_log.reason`）。
    pub reason: Option<String>,
    /// 明细行备注（可选；写了才覆盖 `biz_salary_detail.remark`，见设计 §4.8.4「说明写 remark」）。
    pub remark: Option<String>,
    /// 要改的项（至少一条）。
    pub items: Option<Vec<SalaryOverrideItemReq>>,
}

/// 校验通过的一条覆盖。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideAmount {
    /// 薪资项编码（已 trim）。
    pub item_code: String,
    /// 新金额。
    pub amount: Decimal,
}

/// 校验通过的覆盖请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SalaryOverrideInput {
    /// 原因（已 trim）。
    pub reason: String,
    /// 明细行备注（已 trim，`None` = 不动原备注）。
    pub remark: Option<String>,
    /// 要改的项（按入参顺序，编码不重复）。
    pub items: Vec<OverrideAmount>,
}

// ==================== 错误原语 ====================

/// `400 + 10001`：入参形状错。
fn param_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `409 + 68008`：覆盖后与业务规则冲突（负数工资 / 单位项归不了桶）。
///
/// 复用 HR-16 种下的 `HR_SALARY_RULE_BLOCKED`：**本卡不新增错误码**
/// （加码要动注册表与 `docs/`，那是收口卡的事）。
pub fn rule_blocked(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::HR_SALARY_RULE_BLOCKED, message.into())
}

/// `403 + 30001`：越权（不在当前用户的数据范围内）。
///
/// 报文点名单号 + 员工 + 「不在你的数据范围内」，并给可行路径（找有权限的人改）。
pub fn forbidden(sheet_no: &str, employee: &str) -> ApiError {
    ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "无权修改薪资单 {sheet_no} 中 {employee} 的明细：该员工不在你的数据范围内\
             （部门 / 本人范围限制）。如需调整，请让有对应部门权限的 HR 操作"
        ),
    )
}

// ==================== 入参校验 ====================

/// 入参 → 校验通过的覆盖请求。
///
/// # 校验顺序（先形状后业务，一次说清一件事）
///
/// 1. `reason` 必填、trim 后 ≥ [`MIN_REASON_CHARS`] 字、≤ [`MAX_REASON_CHARS`] 字；
/// 2. `remark` ≤ 500 字（库列 `varchar(500)`）；
/// 3. `items` 非空、≤ [`MAX_OVERRIDE_ITEMS`] 条；
/// 4. 每条：`item_code` 非空、`amount` 必填；
/// 5. 同一次请求里 `item_code` 不许重复（重复会让「最后一条生效」变成暗规则）。
///
/// # 异常情况
///
/// 任一条不满足 → `400 + 10001`，报文点名 `items[2].amount` 这种可定位的坐标。
pub fn parse_override_req(req: &SalaryOverrideReq) -> std::result::Result<SalaryOverrideInput, ApiError> {
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            param_invalid(format!(
                "reason 必填（人工覆盖要留原因，≥{MIN_REASON_CHARS} 字，进状态日志）：\
                 例「客户口头确认 11 月绩效补 300」"
            ))
        })?;
    let reason_chars = reason.chars().count();
    if reason_chars < MIN_REASON_CHARS {
        return Err(param_invalid(format!(
            "reason 至少 {MIN_REASON_CHARS} 个字（收到 {reason_chars} 个：「{reason}」）—— \
             状态日志要能回答「为什么改这笔钱」"
        )));
    }
    if reason_chars > MAX_REASON_CHARS {
        return Err(param_invalid(format!(
            "reason 最多 {MAX_REASON_CHARS} 个字（收到 {reason_chars} 个）"
        )));
    }

    let remark = match req.remark.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => None,
        Some(text) => {
            let len = text.chars().count();
            if len > crate::business::hr::salary_sheet_dto::MAX_REMARK_LEN {
                return Err(param_invalid(format!(
                    "remark 最多 {} 个字符（收到 {len} 个）",
                    crate::business::hr::salary_sheet_dto::MAX_REMARK_LEN
                )));
            }
            Some(text.to_string())
        }
    };

    let raw_items = req
        .items
        .as_deref()
        .filter(|list| !list.is_empty())
        .ok_or_else(|| param_invalid("items 必填且至少一条（要改哪一项、改成多少）"))?;
    if raw_items.len() > MAX_OVERRIDE_ITEMS {
        return Err(param_invalid(format!(
            "items 一次最多 {MAX_OVERRIDE_ITEMS} 条（收到 {} 条）",
            raw_items.len()
        )));
    }

    let mut items: Vec<OverrideAmount> = Vec::with_capacity(raw_items.len());
    for (index, entry) in raw_items.iter().enumerate() {
        let item_code = entry
            .item_code
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| param_invalid(format!("items[{index}].item_code 必填（薪资项编码）")))?;
        let amount = entry
            .amount
            .ok_or_else(|| param_invalid(format!("items[{index}].amount 必填（允许 0 与负数）")))?;
        if items.iter().any(|item| item.item_code == item_code) {
            return Err(param_invalid(format!(
                "items[{index}].item_code = {item_code} 重复：同一个薪资项一次只能改成一个值"
            )));
        }
        items.push(OverrideAmount {
            item_code: item_code.to_string(),
            amount: round_amount(amount),
        });
    }

    Ok(SalaryOverrideInput {
        reason: reason.to_string(),
        remark,
        items,
    })
}

// ==================== 覆盖（纯函数） ====================

/// 一行明细项的覆盖快照（金额可变，其余是识别项用的信息）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemAmount {
    /// 薪资项编码。
    pub item_code: String,
    /// 项名称（快照，给报文与回执）。
    pub item_name: String,
    /// 项类型（`EARNING` / `PRE_DEDUCT` / `WITHHOLD` / `EMPLOYER`）。
    pub item_type: String,
    /// 金额（覆盖前 / 覆盖后）。
    pub amount: Decimal,
    /// 项的**贷方科目**（`EMPLOYER` 项归桶的第二判据，见
    /// [`crate::business::hr::salary_compute::employer_bucket`]）。
    pub credit_account_code: Option<String>,
}

/// 一项被改动的记录（回执 + 状态日志文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmountChange {
    /// 薪资项编码。
    pub item_code: String,
    /// 项名称。
    pub item_name: String,
    /// 项类型。
    pub item_type: String,
    /// 覆盖前金额。
    pub old_amount: Decimal,
    /// 覆盖后金额。
    pub new_amount: Decimal,
}

impl AmountChange {
    /// 差额（新 − 旧）。
    pub fn delta(&self) -> Decimal {
        round_amount(self.new_amount - self.old_amount)
    }
}

/// 把覆盖金额应用到明细项快照上（**纯函数**，不碰库）。
///
/// # 参数
///
/// - `current`：本行明细项（覆盖前）；
/// - `input`：校验通过的覆盖请求；
/// - `who`：员工显示名（`张三（EMP-0002）`，只用于报文）。
///
/// # 返回值
///
/// `(覆盖后的明细项, 真正发生变化的项)`。**金额未变的项不进 `changes`**（重试同一个请求
/// 不该在状态日志里多出第三条记录）。
///
/// # 异常情况
///
/// - `400`：本行没有这个薪资项（点名编码 + 列出可覆盖的编码）；
/// - `400`：项类型是 `SUMMARY`（`GROSS`/`NET` 是派生汇总，改它没有意义）；
/// - `400`：项类型不在 [`OVERRIDABLE_TYPES`] 里。
pub fn override_amounts(
    current: &[ItemAmount],
    input: &SalaryOverrideInput,
    who: &str,
) -> std::result::Result<(Vec<ItemAmount>, Vec<AmountChange>), ApiError> {
    let mut next = current.to_vec();
    let mut changes: Vec<AmountChange> = Vec::new();

    for wanted in &input.items {
        let Some(slot) = next.iter_mut().find(|item| item.item_code == wanted.item_code) else {
            return Err(param_invalid(format!(
                "{who} 的明细里没有薪资项 {}（可覆盖：{}）",
                wanted.item_code,
                available_codes(current)
            )));
        };
        if slot.item_type == ITEM_TYPE_SUMMARY {
            return Err(param_invalid(format!(
                "{who} 的 {} 是汇总项（{}），不能直接覆盖：它是按类型聚合出来的应发/实发，\
                 请改构成项（可覆盖：{}）",
                slot.item_code,
                slot.item_name,
                available_codes(current)
            )));
        }
        if !OVERRIDABLE_TYPES.contains(&slot.item_type.as_str()) {
            return Err(param_invalid(format!(
                "{who} 的 {} 类型是 {}，不在可覆盖范围（{}）",
                slot.item_code,
                slot.item_type,
                OVERRIDABLE_TYPES.join("/")
            )));
        }
        if slot.amount != wanted.amount {
            changes.push(AmountChange {
                item_code: slot.item_code.clone(),
                item_name: slot.item_name.clone(),
                item_type: slot.item_type.clone(),
                old_amount: slot.amount,
                new_amount: wanted.amount,
            });
            slot.amount = wanted.amount;
        }
    }

    Ok((next, changes))
}

/// 可覆盖项清单（报文里给人看：`BASE/POST/PERF/…`）。
fn available_codes(items: &[ItemAmount]) -> String {
    let codes: Vec<&str> = items
        .iter()
        .filter(|item| OVERRIDABLE_TYPES.contains(&item.item_type.as_str()))
        .map(|item| item.item_code.as_str())
        .collect();
    if codes.is_empty() {
        "无（本行没有可覆盖项）".to_string()
    } else {
        codes.join("/")
    }
}

// ==================== 汇总重算（纯函数） ====================

/// 明细行的 11 个金额列（= `biz_salary_detail` 里那一组 `NUMERIC(18,4)`）。
///
/// 与库层六条 `CHECK` 一一对应（`gross = earning − pre_deduct`、
/// `withhold = social + fund + iit + other`、`net = gross − withhold`、全非负）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DetailAmounts {
    /// Σ `EARNING`。
    pub earning_amount: Decimal,
    /// Σ `PRE_DEDUCT`。
    pub pre_deduct_amount: Decimal,
    /// 应发 = 收入 − 应发前扣减。
    pub gross_amount: Decimal,
    /// 社保（个人）。
    pub social_personal: Decimal,
    /// 公积金（个人）。
    pub fund_personal: Decimal,
    /// 个税。
    pub iit_amount: Decimal,
    /// 其他代扣。
    pub other_withhold: Decimal,
    /// 代扣合计。
    pub withhold_amount: Decimal,
    /// 实发 = 应发 − 代扣。
    pub net_amount: Decimal,
    /// 单位社保。
    pub si_company: Decimal,
    /// 单位公积金。
    pub hf_company: Decimal,
}

impl DetailAmounts {
    /// 三条恒等式的自检缺口（空 = 全部成立）。
    ///
    /// 覆盖后一定要成立 —— 这是「重写恒等式」的机器判据，接口在落库前用它兜底一道，
    /// 单测用它钉住纯函数。
    pub fn identity_gaps(&self) -> Vec<(&'static str, Decimal, Decimal)> {
        let mut gaps = Vec::new();
        let gross = round_amount(self.earning_amount - self.pre_deduct_amount);
        if gross != self.gross_amount {
            gaps.push(("应发 = 收入 − 应发前扣减", gross, self.gross_amount));
        }
        let withhold = round_amount(self.social_personal + self.fund_personal + self.iit_amount + self.other_withhold);
        if withhold != self.withhold_amount {
            gaps.push(("代扣 = 社保 + 公积金 + 个税 + 其他", withhold, self.withhold_amount));
        }
        let net = round_amount(self.gross_amount - self.withhold_amount);
        if net != self.net_amount {
            gaps.push(("实发 = 应发 − 代扣", net, self.net_amount));
        }
        gaps
    }

    /// 落库前的人话闸（对齐 `ck_biz_sal_det_nonneg` 与设计 §4.8.5「应发不能为负」）。
    ///
    /// 返回第一条不满足的字段（`(字段中文名, 值)`），全部合法时 `None`。
    pub fn first_negative(&self) -> Option<(&'static str, Decimal)> {
        let columns: [(&'static str, Decimal); 11] = [
            ("收入合计", self.earning_amount),
            ("应发前扣减合计", self.pre_deduct_amount),
            ("应发", self.gross_amount),
            ("社保（个人）", self.social_personal),
            ("公积金（个人）", self.fund_personal),
            ("个税", self.iit_amount),
            ("其他代扣", self.other_withhold),
            ("代扣合计", self.withhold_amount),
            ("实发", self.net_amount),
            ("单位社保", self.si_company),
            ("单位公积金", self.hf_company),
        ];
        columns.into_iter().find(|(_, value)| *value < Decimal::ZERO)
    }
}

/// 明细项 → 明细行 11 个金额列（**重算汇总**的核心）。
///
/// 与 HR-34 引擎的聚合口径**逐字对齐**（`salary_compute::compute` 的 ④ 段）：
///
/// | 项类型 | 归到哪 |
/// |---|---|
/// | `EARNING` | `earning_amount` |
/// | `PRE_DEDUCT` | `pre_deduct_amount` |
/// | `WITHHOLD` | 按编码分四桶（`SI_PERSONAL` / `HF_PERSONAL` / `IIT` / 其余 → 其他代扣） |
/// | `EMPLOYER` | 按编码再按贷方科目分两桶（`2211.02` → 单位社保、`2211.03` → 单位公积金） |
/// | `SUMMARY` | **不参与**（汇总项不落 `detail_item`；真出现就说明有人手工塞了行） |
///
/// # 异常情况
///
/// - `409 + 68008`：`WITHHOLD` / `EMPLOYER` 项为负（引擎同款口径：代扣与单位承担不能是负数，
///   负工资/冲回请改 `EARNING`，见设计 §4.8.5）；
/// - `409 + 68008`：`EMPLOYER` 项为**非零**但既不是种子编码、科目也不是 `2211.02/03`
///   （归不了桶 = 单位成本会凭空少一块，不许猜列）。
pub fn summarize(items: &[ItemAmount], who: &str) -> std::result::Result<DetailAmounts, ApiError> {
    let mut amounts = DetailAmounts::default();
    for item in items {
        let value = round_amount(item.amount);
        match item.item_type.as_str() {
            ITEM_TYPE_EARNING => amounts.earning_amount += value,
            ITEM_TYPE_PRE_DEDUCT => amounts.pre_deduct_amount += value,
            ITEM_TYPE_WITHHOLD => {
                if value < Decimal::ZERO {
                    return Err(rule_blocked(format!(
                        "{who} 的代扣项 {}（{}）覆盖后是 {value}：代扣与单位承担不允许负数，\
                         负数请改收入项（补发/冲回，见设计 §4.8.5）",
                        item.item_code, item.item_name
                    )));
                }
                match withhold_bucket(&item.item_code) {
                    WithholdBucket::Social => amounts.social_personal += value,
                    WithholdBucket::Fund => amounts.fund_personal += value,
                    WithholdBucket::Iit => amounts.iit_amount += value,
                    WithholdBucket::Other => amounts.other_withhold += value,
                }
            }
            ITEM_TYPE_EMPLOYER => {
                if value < Decimal::ZERO {
                    return Err(rule_blocked(format!(
                        "{who} 的单位承担项 {}（{}）覆盖后是 {value}：单位承担不允许负数",
                        item.item_code, item.item_name
                    )));
                }
                match employer_bucket(&item.item_code, item.credit_account_code.as_deref()) {
                    EmployerBucket::Social => amounts.si_company += value,
                    EmployerBucket::Fund => amounts.hf_company += value,
                    EmployerBucket::Unclassified => {
                        if value != Decimal::ZERO {
                            return Err(rule_blocked(format!(
                                "{who} 的单位承担项 {}（{}）覆盖后是 {value}，但它的编码不是 \
                                 {EMPLOYER_SOCIAL_CODE}/{EMPLOYER_FUND_CODE}、贷方科目也不是 \
                                 2211.02/2211.03 ⇒ 归不进「单位社保 / 单位公积金」两列。\
                                 请先在薪资项里补科目再覆盖",
                                item.item_code, item.item_name
                            )));
                        }
                    }
                }
            }
            ITEM_TYPE_SUMMARY => {}
            other => {
                return Err(rule_blocked(format!(
                    "{who} 的明细项 {} 类型 {other} 不在聚合口径里（应为 EARNING/PRE_DEDUCT/\
                     WITHHOLD/EMPLOYER）—— 这行是人工塞进来的，先查清来源",
                    item.item_code
                )));
            }
        }
    }

    amounts.earning_amount = round_amount(amounts.earning_amount);
    amounts.pre_deduct_amount = round_amount(amounts.pre_deduct_amount);
    amounts.gross_amount = round_amount(amounts.earning_amount - amounts.pre_deduct_amount);
    amounts.social_personal = round_amount(amounts.social_personal);
    amounts.fund_personal = round_amount(amounts.fund_personal);
    amounts.iit_amount = round_amount(amounts.iit_amount);
    amounts.other_withhold = round_amount(amounts.other_withhold);
    amounts.withhold_amount =
        round_amount(amounts.social_personal + amounts.fund_personal + amounts.iit_amount + amounts.other_withhold);
    amounts.net_amount = round_amount(amounts.gross_amount - amounts.withhold_amount);
    amounts.si_company = round_amount(amounts.si_company);
    amounts.hf_company = round_amount(amounts.hf_company);

    // 恒等式是先算再抄的，必然成立；这里留一道自检 —— 将来有人改聚合顺序也能当场炸。
    let gaps = amounts.identity_gaps();
    if !gaps.is_empty() {
        let (what, expected, actual) = gaps[0];
        return Err(rule_blocked(format!(
            "{who} 覆盖后恒等式不成立（{what}：应 {expected}，实 {actual}）—— 这是程序错误，请报开发"
        )));
    }

    if let Some((column, value)) = amounts.first_negative() {
        return Err(rule_blocked(format!(
            "{who} 覆盖后「{column}」= {value}（负数）：薪资明细的 11 个金额列全部要求 ≥ 0\
             （库层 ck_biz_sal_det_nonneg 同款）。请调整构成项，不要把某人算成负工资"
        )));
    }

    Ok(amounts)
}

// ==================== 状态日志文案 ====================

/// 写进 `biz_status_log.reason` 的完整文案。
///
/// 形态：`人工覆盖明细：张三（EMP-0002）PERF 1200.00→1500.00（+300.00）、…。原因：<用户填的>`
///
/// 为什么把改动逐项写进 reason：状态日志是这个动作**唯一**的审计面（明细项表本身只留
/// 最终金额 + `calc_note`）。半年后对账要回答「这笔为什么多了 300」，只能靠这一行。
pub fn log_reason(employee: &str, changes: &[AmountChange], reason: &str) -> String {
    let detail = changes
        .iter()
        .map(|change| {
            format!(
                "{} {}→{}（{}）",
                change.item_code,
                display_amount(change.old_amount),
                display_amount(change.new_amount),
                signed(change.delta())
            )
        })
        .collect::<Vec<_>>()
        .join("、");
    format!("人工覆盖明细：{employee} {detail}。原因：{reason}")
}

/// 覆盖项写回 `biz_salary_detail_item.calc_note` 的说明（≤500，超出截断）。
pub fn override_note(change: &AmountChange, reason: &str) -> String {
    let text = format!(
        "人工覆盖 {}→{}（{}）：{reason}",
        display_amount(change.old_amount),
        display_amount(change.new_amount),
        signed(change.delta())
    );
    truncate(&text, 500)
}

/// 人读金额：恒定两位小数。
///
/// 库里是 `NUMERIC(18,4)`（`Decimal` 读回来带 4 位），接口入参可能是 `"1500"`（0 位）。
/// 直接把 `Decimal` 塞进文案会写出「1200.0000→1500」这种半截话 —— 说明与状态日志都是**给人
/// 看的**，统一两位；`Decimal` 的机器字段（回执里的 `amount` / `delta`）保持原值不动。
pub fn display_amount(value: Decimal) -> String {
    format!("{:.2}", value)
}

/// 带符号的人读金额（`+300.00` / `-50.00` / `0.00`）。
pub fn signed(value: Decimal) -> String {
    if value > Decimal::ZERO {
        format!("+{}", display_amount(value))
    } else {
        display_amount(value)
    }
}

/// 按字符数截断（不切坏 UTF-8）。
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ==================== 回执 ====================

/// 一项覆盖的回执。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryAmountChangeRes {
    /// 薪资项编码。
    pub item_code: String,
    /// 项名称。
    pub item_name: String,
    /// 项类型码值。
    pub item_type: String,
    /// 项类型中文名。
    pub item_type_label: String,
    /// 覆盖前金额。
    pub old_amount: Decimal,
    /// 覆盖后金额。
    pub new_amount: Decimal,
    /// 差额（新 − 旧）。
    pub delta: Decimal,
}

impl SalaryAmountChangeRes {
    /// 由纯函数结果构造。
    pub fn of(change: &AmountChange) -> Self {
        Self {
            item_code: change.item_code.clone(),
            item_name: change.item_name.clone(),
            item_type: change.item_type.clone(),
            item_type_label: item_dto::label_of(&ITEM_TYPES, &change.item_type),
            old_amount: change.old_amount,
            new_amount: change.new_amount,
            delta: change.delta(),
        }
    }
}

/// 明细覆盖回执（设计 §5.2 #41）。
///
/// 为什么要回整行明细 + 整张单头：前端改一个输入框之后要立刻把「这行的应发/实发、
/// 这张单的代扣合计/实发合计」重画 —— 只回一个 `ok` 就得再发两三个请求。
#[derive(Debug, Clone, Serialize)]
pub struct SalaryOverrideRes {
    /// 是否真的改了东西（`false` = 传的金额与库里一致，幂等重试）。
    pub changed: bool,
    /// 单头 id。
    pub sheet_id: String,
    /// 单号。
    pub sheet_no: String,
    /// 单头状态（覆盖**不改**状态）。
    pub sheet_status: String,
    /// 单头状态中文名。
    pub sheet_status_label: String,
    /// 明细 id。
    pub detail_id: String,
    /// 工号（快照）。
    pub employee_no: String,
    /// 姓名（快照）。
    pub employee_name: String,
    /// 逐项改动（金额未变的项不出现）。
    pub changes: Vec<SalaryAmountChangeRes>,
    /// 覆盖后重新聚合的明细行（11 个金额列 + 明细项）。
    pub detail: SalaryDetailRes,
    /// 重算后的单头（7 个金额列 = `SUM(明细)`）。
    pub sheet: SalarySheetHeaderRes,
    /// 状态日志主键（`changed = false` 时为 `None`）。
    pub log_id: Option<String>,
    /// 本次覆盖原因（回显，前端可据此提示「已记录」）。
    pub reason: String,
}

impl SalaryOverrideRes {
    /// 状态中文名（与列表/详情同一套词表）。
    pub fn status_label_of(status: &str) -> String {
        status_label(status)
    }
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::hr::salary_compute::{WITHHOLD_FUND_CODE, WITHHOLD_IIT_CODE, WITHHOLD_SOCIAL_CODE};

    fn money(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    fn req(reason: &str, items: &[(&str, &str)]) -> SalaryOverrideReq {
        SalaryOverrideReq {
            reason: Some(reason.to_string()),
            remark: None,
            items: Some(
                items
                    .iter()
                    .map(|(code, amount)| SalaryOverrideItemReq {
                        item_code: Some((*code).to_string()),
                        amount: Some(money(amount)),
                    })
                    .collect(),
            ),
        }
    }

    fn item(code: &str, item_type: &str, amount: &str) -> ItemAmount {
        ItemAmount {
            item_code: code.to_string(),
            item_name: format!("{code} 名"),
            item_type: item_type.to_string(),
            amount: money(amount),
            credit_account_code: None,
        }
    }

    // ---- 入参校验 ----

    /// 原因必填且 ≥5 字：这是写进状态日志的唯一解释。
    #[test]
    fn reason_is_required_and_long_enough() {
        let mut empty = req("   ", &[("PERF", "100")]);
        let error = parse_override_req(&empty).expect_err("空原因必须拒");
        assert_eq!((error.status, error.code), (400, error_code::PARAM_INVALID));
        assert!(error.msg.contains("reason 必填"), "报文：{}", error.msg);

        empty.reason = Some("补钱".to_string());
        let error = parse_override_req(&empty).expect_err("4 个字以内的原因必须拒");
        assert!(error.msg.contains("至少 5 个字"), "报文：{}", error.msg);

        // 5 个汉字 = 合法（按字符数不是字节数）
        empty.reason = Some("客户确认补".to_string());
        assert!(parse_override_req(&empty).is_ok());
    }

    /// `amount` 没传 ≠ 0：没传是漏参数，传 0 是「这项清零」。
    #[test]
    fn missing_amount_is_not_zero() {
        let request = SalaryOverrideReq {
            reason: Some("客户确认调整".to_string()),
            remark: None,
            items: Some(vec![SalaryOverrideItemReq {
                item_code: Some("PERF".to_string()),
                amount: None,
            }]),
        };
        let error = parse_override_req(&request).expect_err("缺 amount 必须拒");
        assert!(error.msg.contains("items[0].amount 必填"), "报文：{}", error.msg);
    }

    /// 同一请求里重复编码直接拒（否则「最后一条生效」成为暗规则）。
    #[test]
    fn duplicate_item_code_is_rejected() {
        let request = req("客户确认调整", &[("PERF", "100"), ("PERF", "200")]);
        let error = parse_override_req(&request).expect_err("重复编码必须拒");
        assert!(
            error.msg.contains("items[1].item_code = PERF 重复"),
            "报文：{}",
            error.msg
        );
    }

    /// 空 items 拒；条数超限拒。
    #[test]
    fn items_must_be_present_and_bounded() {
        let empty = SalaryOverrideReq {
            reason: Some("客户确认调整".to_string()),
            remark: None,
            items: None,
        };
        assert!(
            parse_override_req(&empty)
                .expect_err("空 items 必须拒")
                .msg
                .contains("items 必填")
        );

        let too_many: Vec<SalaryOverrideItemReq> = (0..=MAX_OVERRIDE_ITEMS)
            .map(|index| SalaryOverrideItemReq {
                item_code: Some(format!("X{index}")),
                amount: Some(Decimal::ONE),
            })
            .collect();
        let request = SalaryOverrideReq {
            reason: Some("客户确认调整".to_string()),
            remark: None,
            items: Some(too_many),
        };
        let error = parse_override_req(&request).expect_err("超限必须拒");
        assert!(error.msg.contains("一次最多"), "报文：{}", error.msg);
    }

    /// 金额按 4 位小数收敛（库列 `NUMERIC(18,4)`；不收敛会让回执与库里显示两个数）。
    #[test]
    fn amount_is_rounded_to_four_decimals() {
        let request = req("客户确认调整", &[("PERF", "1234.56789")]);
        let input = parse_override_req(&request).expect("合法");
        assert_eq!(input.items[0].amount, money("1234.5679"));
    }

    // ---- 覆盖应用 ----

    /// 只认本行已有的项；不存在的编码点名 + 列出可覆盖项。
    #[test]
    fn unknown_item_code_names_the_available_codes() {
        let items = vec![
            item("BASE", ITEM_TYPE_EARNING, "8000"),
            item("PERF", ITEM_TYPE_EARNING, "0"),
        ];
        let input = parse_override_req(&req("客户确认调整", &[("BONUS", "100")])).expect("合法");
        let error = override_amounts(&items, &input, "张三（EMP-0002）").expect_err("不存在必须拒");
        assert!(error.msg.contains("没有薪资项 BONUS"), "报文：{}", error.msg);
        assert!(error.msg.contains("BASE/PERF"), "报文要点名可覆盖项：{}", error.msg);
    }

    /// `SUMMARY`（GROSS/NET）不能直接覆盖 —— 它是聚合出来的，改了也没有落点。
    #[test]
    fn summary_items_are_not_overridable() {
        let items = vec![item("GROSS", ITEM_TYPE_SUMMARY, "9000")];
        let input = parse_override_req(&req("客户确认调整", &[("GROSS", "9500")])).expect("合法");
        let error = override_amounts(&items, &input, "张三").expect_err("SUMMARY 必须拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("汇总项"), "报文：{}", error.msg);
    }

    /// 金额没变 → 不进 `changes`（幂等重试不写日志）。
    #[test]
    fn unchanged_amount_produces_no_change_record() {
        let items = vec![item("PERF", ITEM_TYPE_EARNING, "1200")];
        let input = parse_override_req(&req("客户确认调整", &[("PERF", "1200")])).expect("合法");
        let (next, changes) = override_amounts(&items, &input, "张三").expect("合法");
        assert!(changes.is_empty(), "金额没变不该产生改动记录");
        assert_eq!(next, items);
    }

    /// 改了才记录，且差额带符号（文案统一两位小数，人读）。
    #[test]
    fn changed_amount_is_recorded_with_delta() {
        let items = vec![item("PERF", ITEM_TYPE_EARNING, "1200")];
        let input = parse_override_req(&req("客户确认补 300", &[("PERF", "1500")])).expect("合法");
        let (next, changes) = override_amounts(&items, &input, "张三").expect("合法");
        assert_eq!(next[0].amount, money("1500"));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].delta(), money("300"));
        assert_eq!(signed(changes[0].delta()), "+300.00");
        // 机器字段保持原值（4 位精度），人读文案才收敛两位
        assert_eq!(display_amount(money("1200.0000")), "1200.00");
    }

    // ---- 汇总重算 ----

    /// 四个代扣桶按**编码**分，合计 = 四桶之和（A7 口径）。
    #[test]
    fn withhold_buckets_follow_item_code() {
        let items = vec![
            item("BASE", ITEM_TYPE_EARNING, "8000"),
            item(WITHHOLD_SOCIAL_CODE, ITEM_TYPE_WITHHOLD, "840"),
            item(WITHHOLD_FUND_CODE, ITEM_TYPE_WITHHOLD, "960"),
            item(WITHHOLD_IIT_CODE, ITEM_TYPE_WITHHOLD, "50"),
            item("OTHER_WITHHOLD", ITEM_TYPE_WITHHOLD, "10"),
        ];
        let amounts = summarize(&items, "张三").expect("合法");
        assert_eq!(amounts.social_personal, money("840"));
        assert_eq!(amounts.fund_personal, money("960"));
        assert_eq!(amounts.iit_amount, money("50"));
        assert_eq!(amounts.other_withhold, money("10"));
        assert_eq!(amounts.withhold_amount, money("1860"));
        assert_eq!(amounts.gross_amount, money("8000"));
        assert_eq!(amounts.net_amount, money("6140"));
        assert!(amounts.identity_gaps().is_empty());
    }

    /// 单位承担按编码 / 科目归两桶；`EMPLOYER` 只进 `si_company`/`hf_company`，不进应发。
    #[test]
    fn employer_buckets_split_by_code_and_account() {
        let items = vec![
            item("BASE", ITEM_TYPE_EARNING, "8000"),
            item(EMPLOYER_SOCIAL_CODE, ITEM_TYPE_EMPLOYER, "1680"),
            item(EMPLOYER_FUND_CODE, ITEM_TYPE_EMPLOYER, "960"),
            ItemAmount {
                // 客户自建项：靠贷方科目归位（2211.02）
                item_code: "SI_COMPANY_2".to_string(),
                item_name: "单位社保二".to_string(),
                item_type: ITEM_TYPE_EMPLOYER.to_string(),
                amount: money("20"),
                credit_account_code: Some("2211.02".to_string()),
            },
        ];
        let amounts = summarize(&items, "张三").expect("合法");
        assert_eq!(amounts.si_company, money("1700"));
        assert_eq!(amounts.hf_company, money("960"));
        assert_eq!(amounts.gross_amount, money("8000"), "单位承担不进应发");
    }

    /// 归不了桶的单位项（非种子编码且无科目）→ 409 点名，不猜列。
    #[test]
    fn unclassified_employer_item_is_blocked() {
        let items = vec![item("WEIRD_FEE", ITEM_TYPE_EMPLOYER, "100")];
        let error = summarize(&items, "张三").expect_err("必须拒");
        assert_eq!((error.status, error.code), (409, error_code::HR_SALARY_RULE_BLOCKED));
        assert!(error.msg.contains("2211.02/2211.03"), "报文：{}", error.msg);
    }

    /// 代扣项为负 → 409（与引擎同款：负工资靠 EARNING，不靠负数代扣）。
    #[test]
    fn negative_withhold_is_blocked() {
        let items = vec![item(WITHHOLD_IIT_CODE, ITEM_TYPE_WITHHOLD, "-10")];
        let error = summarize(&items, "张三").expect_err("必须拒");
        assert!(error.msg.contains("代扣与单位承担不允许负数"), "报文：{}", error.msg);
    }

    /// 覆盖后应发为负 → 409，报文点名「应发」与人。
    ///
    /// 这正是设计 §4.8.5 的边界：负数只能让某个 `EARNING` 变小，不能让应发变负。
    #[test]
    fn negative_gross_is_blocked_with_a_human_message() {
        let items = vec![
            item("BASE", ITEM_TYPE_EARNING, "1000"),
            item("ATT_DEDUCT", ITEM_TYPE_PRE_DEDUCT, "500"),
        ];
        let input = parse_override_req(&req("客户确认扣款", &[("ATT_DEDUCT", "1500")])).expect("合法");
        let (next, changes) = override_amounts(&items, &input, "张三（EMP-0002）").expect("合法");
        assert_eq!(changes.len(), 1);
        let error = summarize(&next, "张三（EMP-0002）").expect_err("负应发必须拒");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("张三（EMP-0002）"), "报文点名员工：{}", error.msg);
        assert!(error.msg.contains("应发"), "报文点名字段：{}", error.msg);
        assert!(error.msg.contains("-500"), "报文给值：{}", error.msg);
    }

    /// 负数 `EARNING`（补发/冲回）在应发仍 ≥ 0 时放行。
    #[test]
    fn negative_earning_item_is_allowed_when_gross_stays_positive() {
        let items = vec![
            item("BASE", ITEM_TYPE_EARNING, "8000"),
            item("PERF", ITEM_TYPE_EARNING, "1200"),
        ];
        let input = parse_override_req(&req("上月多发冲回", &[("PERF", "-200")])).expect("合法");
        let (next, _) = override_amounts(&items, &input, "张三").expect("合法");
        let amounts = summarize(&next, "张三").expect("应发仍为正，必须放行");
        assert_eq!(amounts.earning_amount, money("7800"));
        assert_eq!(amounts.gross_amount, money("7800"));
    }

    /// 状态日志文案含工号、逐项改动、原因（半年后对账只靠这一行）。
    #[test]
    fn log_reason_carries_the_audit_trail() {
        let change = AmountChange {
            item_code: "PERF".to_string(),
            item_name: "绩效".to_string(),
            item_type: ITEM_TYPE_EARNING.to_string(),
            old_amount: money("1200"),
            new_amount: money("1500"),
        };
        let text = log_reason(
            "张三（EMP-0002）",
            std::slice::from_ref(&change),
            "客户口头确认绩效补 300",
        );
        assert!(text.contains("张三（EMP-0002）"), "{text}");
        assert!(text.contains("PERF 1200.00→1500.00（+300.00）"), "{text}");
        assert!(text.ends_with("原因：客户口头确认绩效补 300"), "{text}");
        assert!(override_note(&change, "客户口头确认绩效补 300").contains("人工覆盖 1200.00→1500.00"));
    }

    /// `calc_note` 超长按字符截断（640 个汉字不许切坏 UTF-8）。
    #[test]
    fn override_note_is_truncated_by_chars() {
        let change = AmountChange {
            item_code: "PERF".to_string(),
            item_name: "绩效".to_string(),
            item_type: ITEM_TYPE_EARNING.to_string(),
            old_amount: money("1"),
            new_amount: money("2"),
        };
        let note = override_note(&change, &"客".repeat(600));
        assert_eq!(note.chars().count(), 500);
        assert!(note.ends_with('…'), "{note}");
    }

    /// 403 报文点名单号 + 员工 + 数据范围（越权要能自查）。
    #[test]
    fn forbidden_names_sheet_and_employee() {
        let error = forbidden("SAL-202701-000001", "李四（EMP-0003）");
        assert_eq!((error.status, error.code), (403, error_code::PERMISSION_DENIED));
        assert!(error.msg.contains("SAL-202701-000001"), "报文：{}", error.msg);
        assert!(error.msg.contains("李四（EMP-0003）"), "报文：{}", error.msg);
        assert!(error.msg.contains("数据范围"), "报文：{}", error.msg);
    }
}

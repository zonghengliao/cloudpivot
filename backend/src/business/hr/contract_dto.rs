//! 劳动合同的入参契约与字段校验（`biz_employee_contract` · HR-15）。
//!
//! 本文件只放**纯函数**：把 JSON 载荷变成 [`ContractWrite`]（模型的写载荷），
//! 并给出能照着改的报错。库与事务在 [`crate::business::hr::contract_handler`]，
//! SQL 在 [`crate::business::hr::contract_model`]。
//!
//! # 出参为什么不在本文件
//!
//! 出参契约由 HR-11 定稿（[`crate::business::hr::employee_dto::EmployeeContractRes`]，
//! 含 `contract_type_label` / `status_label` / `renewed_from_no` / `expire_in_days`）。
//! 本卡**不重新定义**一份平行的合同出参 —— 两份形状迟早漂移，
//! 详情页（吃 `contracts[]`）与合同回执（吃本卡接口）就会对不上。
//!
//! # 校验口径（比 DDL 更严的地方都写了理由）
//!
//! | 规则 | 来源 |
//! |---|---|
//! | 合同编号必填、≤32 字 | `contract_no VARCHAR(32)` + 列注释「不取号，必须与纸质一致」 |
//! | `contract_type` 五选一 | `ck_biz_emp_contract_type` |
//! | `sign_date ≤ start_date` | `ck_biz_emp_contract_sign` |
//! | `end_date ≥ start_date` | `ck_biz_emp_contract_range` |
//! | `0 ≤ probation_months ≤ 6` | `ck_biz_emp_contract_prob`（劳动合同法第 19 条） |
//! | `UNFIXED_TERM ⇔ end_date IS NULL` | `ck_biz_emp_contract_unfixed` 只约束了一半；**反向也拦**：`FIXED_TERM` 没有终止日就永远算不出到期提醒（`idx_biz_emp_contract_due` 按 `end_date` 扫），这种行在台账里是**不会响的哑弹** |
//! | 解除必须给解除日 + ≥5 字原因 | `ck_biz_emp_contract_term`（库）+ 设计 §4.2.1「原因能追溯到为什么」 |
//!
//! 新建只允许 `ACTIVE` / `EXPIRED` 两种初始状态：`RENEWED` 必须由续签产生（要 `renewed_from_id`），
//! `TERMINATED` 必须带解除日（那是「解除」动作，走 `PUT` + 状态机）。补录历史到期合同用 `EXPIRED`。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::hr::contract_model::{ContractRow, ContractWrite};
use crate::business::hr::employee_dto::{self, EmployeeContractRes};
use crate::shared::error_code;

/// `contract_no VARCHAR(32)`。
pub const MAX_CONTRACT_NO: usize = 32;
/// `terminate_reason VARCHAR(255)`。
pub const MAX_TERMINATE_REASON: usize = 255;
/// `remark TEXT`（库里无上限；接口层仍封顶，防止把备注当附件传）。
pub const MAX_REMARK: usize = 1000;
/// 试用期上限（月，`ck_biz_emp_contract_prob`）。
pub const PROBATION_MAX: i16 = 6;
/// 解除原因的最小字数（与员工状态变更的 `MIN_REASON_CHARS` 同口径）。
pub const MIN_TERMINATE_REASON_CHARS: usize = 5;

/// 合同类型域（`ck_biz_emp_contract_type`，与 `employee_dto::CONTRACT_TYPE_LABELS` 逐值对应）。
pub const CONTRACT_TYPES: [&str; 5] = ["FIXED_TERM", "UNFIXED_TERM", "INTERN", "LABOR_SERVICE", "DISPATCH"];

/// 新建时允许的初始状态。
///
/// - `ACTIVE`：新签 / 续签的新合同；
/// - `EXPIRED`：补录历史合同（纸质已到期，录入时就是到期态）。
///
/// `RENEWED` 只能由续签接口产生（要 `renewed_from_id` 指回原合同），
/// `TERMINATED` 只能由解除动作产生（要解除日 + 原因）—— 两者都不许「直接建出来」，
/// 否则续签链会出现没有来源的 `RENEWED` 行、或没有解除日的 `TERMINATED` 行（库会拒）。
pub const CREATE_STATUSES: [&str; 2] = ["ACTIVE", "EXPIRED"];

/// `PUT` 允许的目标状态（状态机边：`ACTIVE → EXPIRED`、`ACTIVE → TERMINATED`）。
pub const UPDATE_STATUSES: [&str; 2] = ["EXPIRED", "TERMINATED"];

/// 可续签的当前状态（`ACTIVE` 提前续、`EXPIRED` 到期补签）。
///
/// `RENEWED` 已经续过一次（再续是从新合同那条链续）—— 在原行上再续会让
/// `renewed_from_id` 出现两条指向同一份合同的链，续签历史直接分叉。
/// `TERMINATED` 是解除终态，续签合同不能盖在解除过的合同上。
pub const RENEWABLE_STATUSES: [&str; 2] = ["ACTIVE", "EXPIRED"];

// ==================== 错误原语 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 入参结构 ====================

/// 新建合同（`POST /business/hr/employee/{id}/contract`）。
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ContractCreateReq {
    /// 合同编号（必填，≤32，公司内唯一，与纸质一致）。
    #[serde(default)]
    pub contract_no: Option<String>,
    /// 合同类型码值（默认 `FIXED_TERM`，与 DDL 的列默认值一致）。
    #[serde(default)]
    pub contract_type: Option<String>,
    /// 签订日（必填，`yyyy-MM-dd`）。
    #[serde(default)]
    pub sign_date: Option<String>,
    /// 起始日（必填，`yyyy-MM-dd`）。
    #[serde(default)]
    pub start_date: Option<String>,
    /// 终止日（无固定期限留空）。
    #[serde(default)]
    pub end_date: Option<String>,
    /// 试用期（月，默认 0）。
    #[serde(default)]
    pub probation_months: Option<i16>,
    /// 初始状态（默认 `ACTIVE`；只允许 `ACTIVE` / `EXPIRED`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 修改合同（`PUT /business/hr/contract/{contract_id}`）。
///
/// 语义与员工修改一致：字段**不传 = 不改**；可空文本传空串 = 清空。
/// `to_status` 传值时才动状态（`EXPIRED` 手工标到期 / `TERMINATED` 解除），
/// 状态变更走 `shared::state_machine`（写 `biz_status_log` + CAS 回写）。
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ContractUpdateReq {
    /// 合同编号。
    #[serde(default)]
    pub contract_no: Option<String>,
    /// 合同类型。
    #[serde(default)]
    pub contract_type: Option<String>,
    /// 签订日。
    #[serde(default)]
    pub sign_date: Option<String>,
    /// 起始日。
    #[serde(default)]
    pub start_date: Option<String>,
    /// 终止日（传空串 = 清空；仅无固定期限允许）。
    #[serde(default)]
    pub end_date: Option<String>,
    /// 试用期（月）。
    #[serde(default)]
    pub probation_months: Option<i16>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 目标状态（`EXPIRED` / `TERMINATED`；不传 = 状态不变）。
    #[serde(default)]
    pub to_status: Option<String>,
    /// 解除日（`to_status = TERMINATED` 时必填）。
    #[serde(default)]
    pub terminated_date: Option<String>,
    /// 解除原因（`to_status = TERMINATED` 时必填，≥5 字）。
    #[serde(default)]
    pub terminate_reason: Option<String>,
}

/// 续签（`POST /business/hr/contract/{contract_id}/renew`）。
///
/// 载荷是**新合同**的字段（原合同由路径给出）；`renewed_from_id` 由服务端写，
/// 客户端传了也忽略 —— 续签链不能由前端指定。
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ContractRenewReq {
    /// 新合同编号（必填，必须与原合同不同）。
    #[serde(default)]
    pub contract_no: Option<String>,
    /// 新合同类型（默认与原合同相同）。
    #[serde(default)]
    pub contract_type: Option<String>,
    /// 新合同签订日（默认今天）。
    #[serde(default)]
    pub sign_date: Option<String>,
    /// 新合同起始日（必填；不能早于原合同起始日）。
    #[serde(default)]
    pub start_date: Option<String>,
    /// 新合同终止日（无固定期限留空）。
    #[serde(default)]
    pub end_date: Option<String>,
    /// 新合同试用期（月，默认 0 —— 续签不再约定试用期）。
    #[serde(default)]
    pub probation_months: Option<i16>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

// ==================== 出参（只加「续签回执」这一种，单份合同用 HR-11 的契约） ====================

/// 续签回执：新合同 + 原合同（已封闭）+ 完整续签链。
///
/// 为什么把 `chain` 一起返回：验收要求「续签链能追溯到原合同」，而前端时间线要
/// **一次请求**就能画出「原合同 → 第一次续签 → 本次续签」。让前端再发 N 次请求
/// 逐级回溯既慢又可能拿到半条链（中途有人又续了一次）。链由一条递归 CTE 查出
/// （[`crate::business::hr::contract_model::chain_of`]），顺序是原合同在前。
#[derive(Debug, Clone, Serialize)]
pub struct ContractRenewRes {
    /// 新签的合同（`renewed_from_id` 指回原合同）。
    pub contract: EmployeeContractRes,
    /// 原合同（本次续签后状态已翻成 `RENEWED`）。
    pub previous: EmployeeContractRes,
    /// 完整续签链（原合同 → … → 新合同），`chain.last()` 恒等于 `contract`。
    pub chain: Vec<EmployeeContractRes>,
}

// ==================== 字段原语（纯函数） ====================

fn required_text(raw: Option<&str>, field: &str, max: usize) -> std::result::Result<String, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    check_len(text, field, max)?;
    Ok(text.to_string())
}

fn optional_text(raw: Option<&str>, field: &str, max: usize) -> std::result::Result<Option<String>, ApiError> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Ok(None);
    }
    check_len(text, field, max)?;
    Ok(Some(text.to_string()))
}

fn check_len(text: &str, field: &str, max: usize) -> std::result::Result<(), ApiError> {
    if max != usize::MAX && text.chars().count() > max {
        return Err(bad_request(format!(
            "{field} 超长：最多 {max} 个字符（当前 {} 个）",
            text.chars().count()
        )));
    }
    Ok(())
}

fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 日期格式应为 yyyy-MM-dd（收到 `{text}`）")))
}

fn parse_optional_date(raw: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match raw.map(str::trim).unwrap_or_default() {
        "" => Ok(None),
        text => Ok(Some(parse_date(text, field)?)),
    }
}

fn require_one_of(raw: &str, allowed: &[&str], field: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if allowed.contains(&text) {
        return Ok(text.to_string());
    }
    Err(bad_request(format!(
        "{field} 只能是 {} 之一（收到 `{text}`）",
        allowed.join(" / ")
    )))
}

/// 可选文本合并：`None` = 不改，`Some("")` = 清空，其它 = 覆盖（限长）。
fn merge_text(
    patch: Option<&str>,
    current: &Option<String>,
    field: &str,
    max: usize,
) -> std::result::Result<Option<String>, ApiError> {
    match patch {
        None => Ok(current.clone()),
        Some(raw) => optional_text(Some(raw), field, max),
    }
}

/// 可选日期合并：`None` = 不改，`Some("")` = 清空，其它 = 覆盖。
fn merge_date(
    patch: Option<&str>,
    current: Option<NaiveDate>,
    field: &str,
) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match patch {
        None => Ok(current),
        Some(raw) => parse_optional_date(Some(raw), field),
    }
}

fn merge_required_date(
    patch: Option<&str>,
    current: NaiveDate,
    field: &str,
) -> std::result::Result<NaiveDate, ApiError> {
    match patch {
        None => Ok(current),
        Some(raw) => parse_date(raw, field),
    }
}

// ==================== 校验（新建 / 修改 / 续签） ====================

/// 校验一份写载荷（新建、修改合并后、续签都过这一道）。
///
/// 顺序很讲究：先查「必填 / 取值域」，再查「跨字段关系」，最后查「类型与终止日的配套」——
/// 报错永远指向**第一个**该改的字段，而不是让用户来回试。
pub fn check_draft(draft: &ContractWrite) -> std::result::Result<(), ApiError> {
    if draft.contract_no.trim().is_empty() {
        return Err(bad_request("contract_no 不能为空：合同编号必须与纸质合同一致"));
    }
    check_len(draft.contract_no.trim(), "contract_no", MAX_CONTRACT_NO)?;
    require_one_of(&draft.contract_type, &CONTRACT_TYPES, "contract_type")?;
    if !(0..=PROBATION_MAX).contains(&draft.probation_months) {
        return Err(bad_request(format!(
            "probation_months 只能是 0~{PROBATION_MAX} 个月（劳动合同法第 19 条上限，收到 {}）",
            draft.probation_months
        )));
    }
    if let Some(end) = draft.end_date
        && end < draft.start_date
    {
        return Err(bad_request(format!(
            "end_date（{end}）不能早于 start_date（{}）",
            draft.start_date
        )));
    }
    if draft.sign_date > draft.start_date {
        return Err(bad_request(format!(
            "sign_date（{}）不能晚于 start_date（{}）：先签后生效，倒签请改签订日",
            draft.sign_date, draft.start_date
        )));
    }
    // `ck_biz_emp_contract_unfixed` 只约束「无固定期限 ⇒ 无终止日」；
    // 反向（有期限 ⇒ 必须有终止日）在接口层补上，理由见模块文档。
    match (draft.contract_type.as_str(), draft.end_date) {
        ("UNFIXED_TERM", Some(end)) => {
            return Err(bad_request(format!(
                "无固定期限合同不能有终止日期（收到 end_date = {end}）：请清空终止日，\
                 或把 contract_type 改成 FIXED_TERM"
            )));
        }
        ("UNFIXED_TERM", None) => {}
        (_, None) => {
            return Err(bad_request(
                "非无固定期限合同必须填终止日期：没有 end_date 就算不出到期提醒\
                 （到期前 30 天提醒靠它），台账里会出现「永不到期」的假合同",
            ));
        }
        (_, Some(_)) => {}
    }
    if let Some(remark) = draft.remark.as_deref() {
        check_len(remark, "remark", MAX_REMARK)?;
    }
    Ok(())
}

/// 新建：解析 + 校验，返回（写载荷, 初始状态）。
pub fn parse_create(req: &ContractCreateReq) -> std::result::Result<(ContractWrite, String), ApiError> {
    let contract_no = required_text(req.contract_no.as_deref(), "contract_no", MAX_CONTRACT_NO)?;
    let contract_type = match req.contract_type.as_deref().map(str::trim) {
        None | Some("") => "FIXED_TERM".to_string(),
        Some(raw) => require_one_of(raw, &CONTRACT_TYPES, "contract_type")?,
    };
    let sign_date = parse_date(req.sign_date.as_deref().unwrap_or_default(), "sign_date")?;
    let start_date = parse_date(req.start_date.as_deref().unwrap_or_default(), "start_date")?;
    let end_date = parse_optional_date(req.end_date.as_deref(), "end_date")?;
    let probation_months = req.probation_months.unwrap_or(0);
    let status = match req.status.as_deref().map(str::trim) {
        None | Some("") => "ACTIVE".to_string(),
        Some(raw) => require_one_of(raw, &CREATE_STATUSES, "status")?,
    };
    let draft = ContractWrite {
        contract_no,
        contract_type,
        sign_date,
        start_date,
        end_date,
        probation_months,
        remark: optional_text(req.remark.as_deref(), "remark", MAX_REMARK)?,
    };
    check_draft(&draft)?;
    Ok((draft, status))
}

/// 修改：把补丁合并到旧行上（`None` = 不改），再整体校验。
pub fn parse_update(req: &ContractUpdateReq, old: &ContractRow) -> std::result::Result<ContractWrite, ApiError> {
    let draft = ContractWrite {
        contract_no: match req.contract_no.as_deref() {
            None => old.contract_no.clone(),
            Some(raw) => required_text(Some(raw), "contract_no", MAX_CONTRACT_NO)?,
        },
        contract_type: match req.contract_type.as_deref() {
            None => old.contract_type.clone(),
            Some(raw) => require_one_of(raw, &CONTRACT_TYPES, "contract_type")?,
        },
        sign_date: merge_required_date(req.sign_date.as_deref(), old.sign_date, "sign_date")?,
        start_date: merge_required_date(req.start_date.as_deref(), old.start_date, "start_date")?,
        end_date: merge_date(req.end_date.as_deref(), old.end_date, "end_date")?,
        probation_months: req.probation_months.unwrap_or(old.probation_months),
        remark: merge_text(req.remark.as_deref(), &old.remark, "remark", MAX_REMARK)?,
    };
    check_draft(&draft)?;
    Ok(draft)
}

/// 修改请求是否真的改了什么（没有改动就不该写库 / 不该写日志）。
///
/// 返回 `true` = 至少一个字段有变化（含状态动作）。全空的 `PUT` 报 409，
/// 而不是「200 但什么都没改」—— 后者会让前端以为保存成功、用户继续等生效。
pub fn update_changes_anything(req: &ContractUpdateReq, old: &ContractRow, draft: &ContractWrite) -> bool {
    req.to_status
        .as_deref()
        .map(str::trim)
        .is_some_and(|text| !text.is_empty())
        || draft.contract_no != old.contract_no
        || draft.contract_type != old.contract_type
        || draft.sign_date != old.sign_date
        || draft.start_date != old.start_date
        || draft.end_date != old.end_date
        || draft.probation_months != old.probation_months
        || draft.remark.as_deref().map(str::trim) != old.remark.as_deref().map(str::trim)
}

/// 解析状态动作（`PUT` 的 `to_status`）。
///
/// 返回 `None` = 不动状态；`Some(("TERMINATED", 解除日, 原因))` 等。
/// 解除必须给日期 + ≥[`MIN_TERMINATE_REASON_CHARS`] 字原因（`ck_biz_emp_contract_term` +
/// 设计 §4.2.1「原因要能追溯到为什么」）；「标到期」不接受解除三列（自然到期不是解除）。
#[derive(Debug)]
pub struct StatusAction {
    /// 目标状态（`EXPIRED` / `TERMINATED`）。
    pub to_status: String,
    /// 解除日（仅 `TERMINATED`）。
    pub terminated_date: Option<NaiveDate>,
    /// 解除原因（仅 `TERMINATED`）。
    pub terminate_reason: Option<String>,
}

/// 解析并校验状态动作；`to_status` 为空 → `Ok(None)`。
pub fn parse_status_action(
    req: &ContractUpdateReq,
    old: &ContractRow,
) -> std::result::Result<Option<StatusAction>, ApiError> {
    let Some(raw) = req.to_status.as_deref().map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let to_status = require_one_of(raw, &UPDATE_STATUSES, "to_status")?;
    if to_status == "TERMINATED" {
        let date = parse_optional_date(req.terminated_date.as_deref(), "terminated_date")?
            .ok_or_else(|| bad_request("解除劳动合同必须给出 terminated_date（解除生效日）"))?;
        if date < old.start_date {
            return Err(bad_request(format!(
                "解除日（{date}）不能早于合同起始日（{}）",
                old.start_date
            )));
        }
        let reason = req
            .terminate_reason
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                bad_request(format!(
                    "解除劳动合同必须写清原因（≥{MIN_TERMINATE_REASON_CHARS} 字）：\
                     原因会进状态日志，审计要能回答「为什么解除」"
                ))
            })?;
        if reason.chars().count() < MIN_TERMINATE_REASON_CHARS {
            return Err(bad_request(format!(
                "解除原因至少 {MIN_TERMINATE_REASON_CHARS} 个字（当前 {} 个）：\
                 审计要能回答「为什么解除」",
                reason.chars().count()
            )));
        }
        check_len(reason, "terminate_reason", MAX_TERMINATE_REASON)?;
        return Ok(Some(StatusAction {
            to_status,
            terminated_date: Some(date),
            terminate_reason: Some(reason.to_string()),
        }));
    }
    if req
        .terminated_date
        .as_deref()
        .map(str::trim)
        .is_some_and(|text| !text.is_empty())
        || req
            .terminate_reason
            .as_deref()
            .map(str::trim)
            .is_some_and(|text| !text.is_empty())
    {
        return Err(bad_request(
            "只有「解除」才填 terminated_date / terminate_reason：自然到期是事件，不是解除\
             （两者混用会让「合同解除率」统计失真）",
        ));
    }
    Ok(Some(StatusAction {
        to_status,
        terminated_date: None,
        terminate_reason: None,
    }))
}

/// 续签：解析新合同字段 + 校验「不能早于原合同起始日 / 编号不能相同」。
///
/// `today` 由调用方传入（业务日按 `+08:00`，服务端算；单测传固定日期避免假红）。
/// 续签的 `protation_months` 默认 0：同一单位与同一员工只约定一次试用期（劳动合同法第 19 条）。
pub fn parse_renew(
    req: &ContractRenewReq,
    old: &ContractRow,
    today: NaiveDate,
) -> std::result::Result<ContractWrite, ApiError> {
    let contract_no = required_text(req.contract_no.as_deref(), "contract_no", MAX_CONTRACT_NO)?;
    if contract_no.eq_ignore_ascii_case(old.contract_no.trim()) {
        return Err(bad_request(format!(
            "续签必须给**新**合同编号：`{contract_no}` 已被原合同使用（合同编号唯一）"
        )));
    }
    let contract_type = match req.contract_type.as_deref().map(str::trim) {
        None | Some("") => old.contract_type.clone(),
        Some(raw) => require_one_of(raw, &CONTRACT_TYPES, "contract_type")?,
    };
    let sign_date = match req.sign_date.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => today,
        Some(raw) => parse_date(raw, "sign_date")?,
    };
    let start_date = parse_date(req.start_date.as_deref().unwrap_or_default(), "start_date")?;
    if start_date < old.start_date {
        return Err(bad_request(format!(
            "续签合同的起始日（{start_date}）不能早于原合同起始日（{}）：\
             续签是往后接链，不是把历史重写一遍",
            old.start_date
        )));
    }
    let draft = ContractWrite {
        contract_no,
        contract_type,
        sign_date,
        start_date,
        end_date: parse_optional_date(req.end_date.as_deref(), "end_date")?,
        probation_months: req.probation_months.unwrap_or(0),
        remark: optional_text(req.remark.as_deref(), "remark", MAX_REMARK)?,
    };
    check_draft(&draft)?;
    Ok(draft)
}

/// 当前状态不可续签时的人话（`None` = 可以续签）。
pub fn renew_blocked_reason(status: &str) -> Option<String> {
    if RENEWABLE_STATUSES.contains(&status) {
        return None;
    }
    let label = employee_dto::contract_status_label(status);
    let hint = match status {
        "RENEWED" => "这份合同已经续签过：请对它续出来的新合同再续（续签链只能往前长）",
        "TERMINATED" => "这份合同已解除：解除是终态，不能再续签（要重新用工请新建合同）",
        _ => "只有「生效中 / 已到期」的合同可以续签",
    };
    Some(format!("合同状态是「{label}」，不能续签：{hint}"))
}

/// 当前状态不可修改时的人话（`None` = 可以修改）。
pub fn update_blocked_reason(row: &ContractRow) -> Option<String> {
    if !row.is_locked() {
        return None;
    }
    let label = employee_dto::contract_status_label(&row.status);
    let hint = if row.status == "RENEWED" {
        "已续签的合同是续签链上的历史节点，要改请改它续出来的新合同"
    } else {
        "已解除的合同是审计凭据，要重新用工请新建合同"
    };
    Some(format!("合同状态是「{label}」，不能修改：{hint}"))
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::uuid::Uuid;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    fn old_row() -> ContractRow {
        ContractRow {
            id: Uuid::nil(),
            employee_id: Uuid::from_u128(9),
            contract_no: "HT-2023-001".to_string(),
            contract_type: "FIXED_TERM".to_string(),
            sign_date: date("2023-01-01"),
            start_date: date("2023-01-05"),
            end_date: Some(date("2026-01-04")),
            probation_months: 3,
            status: "ACTIVE".to_string(),
            terminated_date: None,
            terminate_reason: None,
            renewed_from_id: None,
            renewed_from_no: None,
            remark: Some("原合同".to_string()),
            created_at: nik::prelude::chrono::Utc::now(),
            updated_at: nik::prelude::chrono::Utc::now(),
        }
    }

    fn create_req() -> ContractCreateReq {
        ContractCreateReq {
            contract_no: Some(" HT-2026-007 ".to_string()),
            contract_type: None,
            sign_date: Some("2026-01-01".to_string()),
            start_date: Some("2026-01-05".to_string()),
            end_date: Some("2029-01-04".to_string()),
            probation_months: Some(3),
            status: None,
            remark: Some("  备注  ".to_string()),
        }
    }

    /// 正常新建：编号 trim、类型默认 FIXED_TERM、状态默认 ACTIVE、备注 trim。
    #[test]
    fn create_fills_defaults_and_trims() {
        let (draft, status) = parse_create(&create_req()).expect("合法");
        assert_eq!(draft.contract_no, "HT-2026-007");
        assert_eq!(draft.contract_type, "FIXED_TERM");
        assert_eq!(draft.remark.as_deref(), Some("备注"));
        assert_eq!(status, "ACTIVE");
        assert_eq!(draft.start_date, date("2026-01-05"));
        assert_eq!(draft.end_date, Some(date("2029-01-04")));
    }

    /// 新建报错逐条指明该改哪个字段。
    #[test]
    fn create_rejects_each_bad_field_with_a_named_message() {
        let cases: [(&str, ContractCreateReq, &str); 6] = [
            (
                "缺编号",
                ContractCreateReq {
                    contract_no: Some("   ".to_string()),
                    sign_date: Some("2026-01-01".to_string()),
                    start_date: Some("2026-01-05".to_string()),
                    end_date: Some("2029-01-04".to_string()),
                    ..ContractCreateReq::default()
                },
                "contract_no",
            ),
            (
                "缺签订日",
                ContractCreateReq {
                    contract_no: Some("HT-1".to_string()),
                    start_date: Some("2026-01-05".to_string()),
                    end_date: Some("2029-01-04".to_string()),
                    ..ContractCreateReq::default()
                },
                "sign_date",
            ),
            (
                "签订日晚于起始日",
                ContractCreateReq {
                    sign_date: Some("2026-02-01".to_string()),
                    ..create_req()
                },
                "不能晚于",
            ),
            (
                "终止日早于起始日",
                ContractCreateReq {
                    end_date: Some("2025-01-04".to_string()),
                    ..create_req()
                },
                "不能早于",
            ),
            (
                "试用期 7 个月",
                ContractCreateReq {
                    probation_months: Some(7),
                    ..create_req()
                },
                "0~6",
            ),
            (
                "类型越界",
                ContractCreateReq {
                    contract_type: Some("FOREVER".to_string()),
                    ..create_req()
                },
                "contract_type",
            ),
        ];
        for (name, req, needle) in cases {
            let err = parse_create(&req).expect_err(name);
            assert_eq!(err.status, 400, "{name} 应是 400");
            assert!(
                err.msg.contains(needle),
                "{name} 的报文应含「{needle}」，实际：{}",
                err.msg
            );
        }
    }

    /// 无固定期限：终止日必须为空；有期限：终止日必须给。
    #[test]
    fn unfixed_term_and_end_date_are_mutually_exclusive() {
        let unfixed_with_end = ContractCreateReq {
            contract_type: Some("UNFIXED_TERM".to_string()),
            ..create_req()
        };
        let err = parse_create(&unfixed_with_end).expect_err("无固定期限不能有终止日");
        assert!(err.msg.contains("无固定期限"));

        let unfixed = ContractCreateReq {
            contract_type: Some("UNFIXED_TERM".to_string()),
            end_date: None,
            ..create_req()
        };
        let (draft, _) = parse_create(&unfixed).expect("合法");
        assert_eq!(draft.end_date, None);

        let fixed_without_end = ContractCreateReq {
            end_date: None,
            ..create_req()
        };
        let err = parse_create(&fixed_without_end).expect_err("固定期限必须有终止日");
        assert!(err.msg.contains("必须填终止日期"));
    }

    /// 新建只许 ACTIVE / EXPIRED 两种初始状态。
    #[test]
    fn create_rejects_terminal_statuses() {
        for status in ["RENEWED", "TERMINATED"] {
            let req = ContractCreateReq {
                status: Some(status.to_string()),
                ..create_req()
            };
            let err = parse_create(&req).expect_err("终态不能直接建");
            assert_eq!(err.status, 400);
            assert!(err.msg.contains("status"));
        }
        let expired = ContractCreateReq {
            status: Some("EXPIRED".to_string()),
            ..create_req()
        };
        assert_eq!(parse_create(&expired).expect("补录历史").1, "EXPIRED");
    }

    /// 修改：不传字段 = 不改；空串 = 清空备注；空串终止日 = 清空（仅无固定期限）。
    #[test]
    fn update_merges_patch_over_old_row() {
        let old = old_row();
        let keep = ContractUpdateReq::default();
        let draft = parse_update(&keep, &old).expect("不改也应合法");
        assert_eq!(
            draft,
            ContractWrite {
                contract_no: old.contract_no.clone(),
                contract_type: old.contract_type.clone(),
                sign_date: old.sign_date,
                start_date: old.start_date,
                end_date: old.end_date,
                probation_months: old.probation_months,
                remark: old.remark.clone(),
            }
        );
        assert!(!update_changes_anything(&keep, &old, &draft), "全空 = 没有改动");

        let clear_remark = ContractUpdateReq {
            remark: Some("".to_string()),
            ..ContractUpdateReq::default()
        };
        let draft = parse_update(&clear_remark, &old).expect("清空备注");
        assert_eq!(draft.remark, None);
        assert!(update_changes_anything(&clear_remark, &old, &draft));

        let to_unfixed = ContractUpdateReq {
            contract_type: Some("UNFIXED_TERM".to_string()),
            end_date: Some("".to_string()),
            ..ContractUpdateReq::default()
        };
        let draft = parse_update(&to_unfixed, &old).expect("改无固定期限并清空终止日");
        assert_eq!(draft.end_date, None);
    }

    /// 修改把固定期限的终止日清空 → 400（不然台账里出现算不出到期的假合同）。
    #[test]
    fn update_cannot_blank_end_date_of_fixed_term() {
        let err = parse_update(
            &ContractUpdateReq {
                end_date: Some("".to_string()),
                ..ContractUpdateReq::default()
            },
            &old_row(),
        )
        .expect_err("固定期限不能没有终止日");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("必须填终止日期"));
    }

    /// 解除：必须给解除日 + ≥5 字原因；标到期不接受解除三列。
    #[test]
    fn status_action_validates_termination_fields() {
        let old = old_row();
        let no_date = ContractUpdateReq {
            to_status: Some("TERMINATED".to_string()),
            terminate_reason: Some("个人原因主动离职".to_string()),
            ..ContractUpdateReq::default()
        };
        assert!(parse_status_action(&no_date, &old).is_err(), "缺解除日");

        let short = ContractUpdateReq {
            to_status: Some("TERMINATED".to_string()),
            terminated_date: Some("2025-06-30".to_string()),
            terminate_reason: Some("走了".to_string()),
            ..ContractUpdateReq::default()
        };
        let err = parse_status_action(&short, &old).expect_err("原因太短");
        assert!(err.msg.contains("至少"));

        let before_start = ContractUpdateReq {
            to_status: Some("TERMINATED".to_string()),
            terminated_date: Some("2022-12-31".to_string()),
            terminate_reason: Some("录入时间错了".to_string()),
            ..ContractUpdateReq::default()
        };
        assert!(parse_status_action(&before_start, &old).is_err(), "解除日早于起始日");

        let ok = ContractUpdateReq {
            to_status: Some("TERMINATED".to_string()),
            terminated_date: Some("2025-06-30".to_string()),
            terminate_reason: Some("个人原因主动离职".to_string()),
            ..ContractUpdateReq::default()
        };
        let action = parse_status_action(&ok, &old).expect("合法").expect("有动作");
        assert_eq!(action.to_status, "TERMINATED");
        assert_eq!(action.terminated_date, Some(date("2025-06-30")));

        let expire_with_reason = ContractUpdateReq {
            to_status: Some("EXPIRED".to_string()),
            terminated_date: Some("2026-01-04".to_string()),
            terminate_reason: Some("到期了".to_string()),
            ..ContractUpdateReq::default()
        };
        let err = parse_status_action(&expire_with_reason, &old).expect_err("标到期不该带解除列");
        assert!(err.msg.contains("只有「解除」"));

        let expire = ContractUpdateReq {
            to_status: Some("EXPIRED".to_string()),
            ..ContractUpdateReq::default()
        };
        let action = parse_status_action(&expire, &old).expect("合法").expect("有动作");
        assert_eq!(action.to_status, "EXPIRED");
        assert!(action.terminated_date.is_none());

        let no_action = parse_status_action(&ContractUpdateReq::default(), &old).expect("无动作");
        assert!(no_action.is_none());
    }

    /// 续签：编号必须换、起始日不能早于原合同、试用期默认 0、签订日默认今天。
    #[test]
    fn renew_validates_chain_rules() {
        let old = old_row();
        let today = date("2026-01-20");
        let ok = ContractRenewReq {
            contract_no: Some("HT-2026-001".to_string()),
            start_date: Some("2026-02-01".to_string()),
            end_date: Some("2029-01-31".to_string()),
            ..ContractRenewReq::default()
        };
        let draft = parse_renew(&ok, &old, today).expect("合法续签");
        assert_eq!(draft.contract_type, "FIXED_TERM", "类型沿用原合同");
        assert_eq!(draft.sign_date, today, "签订日默认业务日");
        assert_eq!(draft.probation_months, 0, "续签不再约定试用期");

        let same_no = ContractRenewReq {
            contract_no: Some("ht-2023-001".to_string()),
            start_date: Some("2026-01-05".to_string()),
            end_date: Some("2029-01-04".to_string()),
            ..ContractRenewReq::default()
        };
        let err = parse_renew(&same_no, &old, today).expect_err("编号必须换");
        assert!(
            err.msg.contains("新") && err.msg.contains("合同编号"),
            "报文要点名「换一个新编号」：{}",
            err.msg
        );

        let backdated = ContractRenewReq {
            contract_no: Some("HT-2026-002".to_string()),
            start_date: Some("2022-01-01".to_string()),
            end_date: Some("2025-01-01".to_string()),
            ..ContractRenewReq::default()
        };
        let err = parse_renew(&backdated, &old, today).expect_err("不能倒着接链");
        assert!(err.msg.contains("不能早于原合同起始日"));

        let unfixed_no_end = ContractRenewReq {
            contract_no: Some("HT-2026-003".to_string()),
            start_date: Some("2026-02-01".to_string()),
            ..ContractRenewReq::default()
        };
        let err = parse_renew(&unfixed_no_end, &old, today).expect_err("固定期限必须有终止日");
        assert!(err.msg.contains("必须填终止日期"));
    }

    /// 只有 ACTIVE / EXPIRED 可续签，TERMINATED / RENEWED 各自给下一步。
    #[test]
    fn renew_and_update_blocked_reasons_are_actionable() {
        for status in RENEWABLE_STATUSES {
            assert!(renew_blocked_reason(status).is_none(), "{status} 可以续签");
        }
        let renewed = renew_blocked_reason("RENEWED").expect("已续签不能续");
        assert!(renewed.contains("续签链"));
        let terminated = renew_blocked_reason("TERMINATED").expect("已解除不能续");
        assert!(terminated.contains("终态"));

        let mut row = old_row();
        assert!(update_blocked_reason(&row).is_none(), "ACTIVE 可以改");
        row.status = "EXPIRED".to_string();
        assert!(update_blocked_reason(&row).is_none(), "EXPIRED 允许补正");
        row.status = "RENEWED".to_string();
        assert!(update_blocked_reason(&row).expect("已续签锁").contains("历史节点"));
        row.status = "TERMINATED".to_string();
        assert!(update_blocked_reason(&row).expect("已解除锁").contains("审计凭据"));
    }

    /// 超长字段：合同编号 33 字 / 解除原因 256 字都要被拦在接口层（别让库来报错）。
    #[test]
    fn length_limits_are_enforced_before_the_database() {
        let long_no = ContractCreateReq {
            contract_no: Some("H".repeat(MAX_CONTRACT_NO + 1)),
            ..create_req()
        };
        let err = parse_create(&long_no).expect_err("编号超长");
        assert!(err.msg.contains("最多 32"));

        let err = parse_status_action(
            &ContractUpdateReq {
                to_status: Some("TERMINATED".to_string()),
                terminated_date: Some("2025-06-30".to_string()),
                terminate_reason: Some("因".repeat(MAX_TERMINATE_REASON + 1)),
                ..ContractUpdateReq::default()
            },
            &old_row(),
        )
        .expect_err("原因超长");
        assert!(err.msg.contains("最多 255"));
    }
}

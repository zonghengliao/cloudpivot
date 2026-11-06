//! 佣金规则 · 接口契约（销售链 · SO-36b / ARCH-7 §15.4.2 ②）。
//!
//! 四个接口（前缀 `/api` 由 `[router].prefix` 注入，宏内不写）：
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | GET | `/business/erp/commission-rule/page` | `business:erp:commission-rule:list` |
//! | POST | `/business/erp/commission-rule` | `business:erp:commission-rule:create` |
//! | PUT | `/business/erp/commission-rule/{id}` | `business:erp:commission-rule:update` |
//! | DELETE | `/business/erp/commission-rule/{id}` | `business:erp:commission-rule:delete` |
//!
//! # 出参字段名（前端 SO-37 逐字对齐，改名前先通知）
//!
//! `id / rule_name / scope_type / scope_id / scope_name / calc_type / rate / min_amount /
//! max_amount / effective_from / effective_to / status / remark / created_at / updated_at`
//!
//! * 金额与比率是 `NUMERIC` → JSON **字符串**（`"20.0000"`），前端只展示不自算；
//! * `scope_name` 是派生列（`SALESPERSON` ⇒ `sys_user` 姓名、`MATERIAL_CATEGORY` ⇒
//!   `biz_material.material_name`、`ALL` ⇒ `null`），前端「范围」列直接用它；
//! * `effective_from` / `effective_to` 是 `YYYY-MM-DD`（`effective_to = null` = 长期）。
//!
//! # 入参兼容（前端 PUT 发全量，本接口仍按「部分更新」实现）
//!
//! `PUT` 只改**传了的**字段（与 `customer_handler::UpdateCustomerReq` 同范式）：
//! 前端发全量时行为与「整体覆盖」完全一致，但「只发 `{"status":"INACTIVE"}`」也能
//! 停用一条规则 —— 停用是高频动作，不该逼前端拼回整行。
//! 三态字段（`scope_id` / `max_amount` / `effective_to`）用 [`double_option`] 区分
//! **字段缺失（不改）** 与 **显式 `null`（清空）**。
//!
//! # 库层不变量（DTO 先拒，DDL 兜底）
//!
//! | 不变量 | 库层约束 | 入口校验 |
//! |--------|----------|----------|
//! | `ALL ⇒ scope_id IS NULL`；`SALESPERSON`/`MATERIAL_CATEGORY` ⇒ 非空 | `ck_biz_cmrule_scope_id` | [`normalize_scope`] |
//! | `PERCENT ⇒ 0 ≤ rate ≤ 100`；`FIXED ⇒ rate ≥ 0` | `ck_biz_cmrule_rate` | [`check_rate`] |
//! | `min_amount ≥ 0`、`max_amount ≥ min_amount` | `ck_biz_cmrule_amount` | [`check_amounts`] |
//! | `effective_to ≥ effective_from` | `ck_biz_cmrule_effective` | [`check_effective`] |
//! | `status ∈ {ACTIVE, INACTIVE}` | `ck_biz_cmrule_status` | [`normalize_status`] |
//!
//! 除库层约束外，入口还挡住「列宽装不下」的输入（`rate` 是 `NUMERIC(8,4)`、
//! 金额是 `NUMERIC(18,4)`）：否则 5 位小数的比率会在落库时被静默四舍五入，
//! 计佣结果与用户填的值不一致，属于最难查的一类账。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 域值（与 V0309 的 CHECK 逐字一致，单测钉住） ====================

/// 适用范围：按销售员（`scope_id` = `sys_user.id`）。
pub const SCOPE_SALESPERSON: &str = "SALESPERSON";
/// 适用范围：按物料类别。
///
/// 基础版**没有** `biz_material_category` 表（见 `commission_handler` 头注释），
/// 本期 `scope_id` 存 `biz_material.id`；接分类表后由数据迁移换 id，匹配逻辑不变。
pub const SCOPE_MATERIAL: &str = "MATERIAL_CATEGORY";
/// 适用范围：全局（`scope_id` 必须为空）。
pub const SCOPE_ALL: &str = "ALL";
/// 范围类型白名单（`ck_biz_cmrule_scope`）。
pub const SCOPE_TYPES: [&str; 3] = [SCOPE_SALESPERSON, SCOPE_MATERIAL, SCOPE_ALL];

/// 计算方式：按基数百分比。
pub const CALC_PERCENT: &str = "PERCENT";
/// 计算方式：固定金额（与基数无关）。
pub const CALC_FIXED: &str = "FIXED";
/// 计算方式白名单（`ck_biz_cmrule_calc`）。
pub const CALC_TYPES: [&str; 2] = [CALC_PERCENT, CALC_FIXED];

/// 启用（参与计提匹配）。
pub const RULE_ACTIVE: &str = "ACTIVE";
/// 停用（不再参与匹配，历史明细不受影响）。
pub const RULE_INACTIVE: &str = "INACTIVE";
/// 规则状态白名单（`ck_biz_cmrule_status`）。
pub const RULE_STATUSES: [&str; 2] = [RULE_ACTIVE, RULE_INACTIVE];

/// 规则名上限（`rule_name VARCHAR(100)`，按**字符**数算，中文占 1）。
pub const MAX_RULE_NAME_LEN: usize = 100;
/// 备注上限（`remark TEXT`，仍设个上限防「贴一整段日志进来」）。
pub const MAX_REMARK_LEN: usize = 500;
/// 比率列精度（`rate NUMERIC(8,4)`）。
pub const RATE_PRECISION: (u32, u32) = (8, 4);
/// 金额列精度（`min_amount` / `max_amount` 均为 `NUMERIC(18,4)`）。
pub const AMOUNT_PRECISION: (u32, u32) = (18, 4);

/// 规则状态中文名（错误文案用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        RULE_ACTIVE => "启用",
        RULE_INACTIVE => "停用",
        _ => "未知",
    }
}

/// 范围类型中文名（错误文案 / 前端兜底展示用）。
pub fn scope_label(scope_type: &str) -> &'static str {
    match scope_type {
        SCOPE_SALESPERSON => "销售员",
        SCOPE_MATERIAL => "物料",
        SCOPE_ALL => "全局",
        _ => "未知",
    }
}

// ==================== 校验底座 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 解析 uuid 参数（空串 / 非法值都是 400，文案带字段名）。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim())
        .map_err(|_| bad_request(format!("{field} 不是合法的 UUID（收到「{raw}」）")))
}

/// 解析 `YYYY-MM-DD` 日期（前端日期控件就是这个形状）。
///
/// **形状先卡死再解析**：`chrono` 的 `%d` 接受 `7` 这种一位数写法（`2026-11-7` 也能过），
/// 一旦放进来，库里的 `effective_from` 就会和前端回显的格式不一致；接口契约只认 10 位定长。
pub fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    let text = raw.trim();
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit);
    if !shaped {
        return Err(bad_request(format!(
            "{field} 必须是 YYYY-MM-DD（收到「{raw}」）"
        )));
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field} 不是合法的日期（收到「{raw}」）")))
}

/// 可选文本清洗：trim 后空串 → `None`。
pub fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 三态字段反序列化：**字段缺失** ⇒ `None`（不改）；**显式 `null`** ⇒ `Some(None)`（清空）；
/// 有值 ⇒ `Some(Some(v))`。
///
/// serde 对裸 `Option<Option<T>>` 会把「缺失」与「null」都收敛成 `None`，
/// 于是「把封顶金额清空」这件事在部分更新里表达不出来 —— 这就是这个 helper 的全部意义。
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// 数值是否装得进 `NUMERIC(p, s)`（小数位不超 + 整数位不溢出）。
///
/// 超了就该 400：库里会静默四舍五入（`1.23456 → 1.2346`），用户看到的规则
/// 与实际计佣用的规则就不是同一条了。
fn fits_numeric(value: Decimal, precision: u32, scale: u32) -> bool {
    if value.scale() > scale {
        return false;
    }
    let integer_digits = precision.saturating_sub(scale);
    let limit = Decimal::from(10_i64.pow(integer_digits));
    value.abs() < limit
}

// ==================== 逐项校验（纯函数） ====================

/// 归一化 `scope_type` / `scope_id` 这一对。
///
/// 域值大小写不敏感（前端偶尔发小写），落库恒为大写；`ALL` 必须不带 `scope_id`，
/// 其余两种必须带（与 `ck_biz_cmrule_scope_id` 同一口径，只是提前成人话）。
pub fn normalize_scope(
    scope_type: &str,
    scope_id: Option<&str>,
) -> std::result::Result<(String, Option<Uuid>), ApiError> {
    let scope = scope_type.trim().to_ascii_uppercase();
    if !SCOPE_TYPES.contains(&scope.as_str()) {
        return Err(bad_request(format!(
            "scope_type 只认 {SCOPE_TYPES:?}（收到「{scope_type}」）"
        )));
    }
    let raw = scope_id.map(str::trim).filter(|text| !text.is_empty());
    match scope.as_str() {
        SCOPE_ALL => match raw {
            Some(value) => Err(bad_request(format!(
                "scope_type=ALL 是全局规则，不能指定 scope_id（收到「{value}」）"
            ))),
            None => Ok((scope, None)),
        },
        _ => match raw {
            Some(value) => Ok((scope, Some(parse_uuid(value, "scope_id")?))),
            None => Err(bad_request(format!(
                "scope_type={scope} 必须指定 scope_id（{SCOPE_SALESPERSON} ⇒ 销售员 id；\
                 {SCOPE_MATERIAL} ⇒ 物料 id）"
            ))),
        },
    }
}

/// 归一化 `calc_type`（域值大小写不敏感）。
pub fn normalize_calc(calc_type: &str) -> std::result::Result<String, ApiError> {
    let calc = calc_type.trim().to_ascii_uppercase();
    if !CALC_TYPES.contains(&calc.as_str()) {
        return Err(bad_request(format!(
            "calc_type 只认 {CALC_TYPES:?}（收到「{calc_type}」）"
        )));
    }
    Ok(calc)
}

/// 校验比率：`PERCENT` 0–100；`FIXED` ≥ 0；两者都得装进 `NUMERIC(8,4)`。
pub fn check_rate(calc_type: &str, rate: Decimal) -> std::result::Result<(), ApiError> {
    if rate < Decimal::ZERO {
        return Err(bad_request(format!("rate 不能为负（收到 {rate}）")));
    }
    if calc_type == CALC_PERCENT && rate > Decimal::ONE_HUNDRED {
        return Err(bad_request(format!(
            "calc_type=PERCENT 时 rate 是百分数，必须在 0–100 之间（收到 {rate}）"
        )));
    }
    if !fits_numeric(rate, RATE_PRECISION.0, RATE_PRECISION.1) {
        return Err(bad_request(format!(
            "rate 最多 4 位小数且不超过 9999.9999（收到 {rate}）"
        )));
    }
    Ok(())
}

/// 校验金额：起算额 ≥ 0、封顶 ≥ 起算额、都装得进 `NUMERIC(18,4)`。
pub fn check_amounts(
    min_amount: Decimal,
    max_amount: Option<Decimal>,
) -> std::result::Result<(), ApiError> {
    if min_amount < Decimal::ZERO {
        return Err(bad_request(format!(
            "min_amount 不能为负（收到 {min_amount}）"
        )));
    }
    if !fits_numeric(min_amount, AMOUNT_PRECISION.0, AMOUNT_PRECISION.1) {
        return Err(bad_request(format!(
            "min_amount 超出 NUMERIC(18,4)（收到 {min_amount}）"
        )));
    }
    if let Some(max) = max_amount {
        if max < Decimal::ZERO {
            return Err(bad_request(format!("max_amount 不能为负（收到 {max}）")));
        }
        if max < min_amount {
            return Err(bad_request(format!(
                "max_amount（{max}）不能小于 min_amount（{min_amount}）"
            )));
        }
        if !fits_numeric(max, AMOUNT_PRECISION.0, AMOUNT_PRECISION.1) {
            return Err(bad_request(format!(
                "max_amount 超出 NUMERIC(18,4)（收到 {max}）"
            )));
        }
    }
    Ok(())
}

/// 校验生效区间：`effective_to ≥ effective_from`（两端口都是**含**的，与 V0309 注释一致）。
pub fn check_effective(
    from: NaiveDate,
    to: Option<NaiveDate>,
) -> std::result::Result<(), ApiError> {
    if let Some(end) = to
        && end < from
    {
        return Err(bad_request(format!(
            "effective_to（{end}）不能早于 effective_from（{from}）"
        )));
    }
    Ok(())
}

/// 归一化规则状态（缺省 = `fallback`；域值大小写不敏感）。
pub fn normalize_status(
    raw: Option<&str>,
    fallback: &str,
) -> std::result::Result<String, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(fallback.to_string()),
        Some(value) => {
            let status = value.to_ascii_uppercase();
            if RULE_STATUSES.contains(&status.as_str()) {
                Ok(status)
            } else {
                Err(bad_request(format!(
                    "status 只认 {RULE_STATUSES:?}（收到「{value}」）"
                )))
            }
        }
    }
}

// ==================== 归一化后的规则（落库前的唯一真值） ====================

/// 校验并归一化后的规则字段；`insert` / `update` 只吃这个结构，
/// 保证「前端传什么」与「库里存什么」之间只有一层确定的映射。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSpec {
    /// 规则名（已 trim）。
    pub rule_name: String,
    /// 范围类型（大写域值）。
    pub scope_type: String,
    /// 范围对象（`ALL` 时为空）。
    pub scope_id: Option<Uuid>,
    /// 计算方式（大写域值）。
    pub calc_type: String,
    /// 比率（`PERCENT` 百分数 / `FIXED` 金额）。
    pub rate: Decimal,
    /// 起算金额。
    pub min_amount: Decimal,
    /// 单笔佣金封顶（空 = 不封顶）。
    pub max_amount: Option<Decimal>,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含；空 = 长期）。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 备注（空 = 无）。
    pub remark: Option<String>,
}

impl RuleSpec {
    /// 全量校验（建 / 改都过这一关；改之后也要过，因为部分更新的合并结果才是最终值）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        let name = self.rule_name.trim();
        if name.is_empty() {
            return Err(bad_request("rule_name 必填"));
        }
        if name.chars().count() > MAX_RULE_NAME_LEN {
            return Err(bad_request(format!(
                "rule_name 最多 {MAX_RULE_NAME_LEN} 个字符（收到 {} 个）",
                name.chars().count()
            )));
        }
        if let Some(remark) = &self.remark
            && remark.chars().count() > MAX_REMARK_LEN
        {
            return Err(bad_request(format!(
                "remark 最多 {MAX_REMARK_LEN} 个字符（收到 {} 个）",
                remark.chars().count()
            )));
        }
        // 域值一致性（库层 ck_biz_cmrule_scope 的入口版）
        if !SCOPE_TYPES.contains(&self.scope_type.as_str()) {
            return Err(bad_request(format!(
                "scope_type 只认 {SCOPE_TYPES:?}（收到「{}」）",
                self.scope_type
            )));
        }
        let scope_ok = match self.scope_type.as_str() {
            SCOPE_ALL => self.scope_id.is_none(),
            _ => self.scope_id.is_some(),
        };
        if !scope_ok {
            return Err(bad_request(format!(
                "scope_type={} 与 scope_id 不一致：{} ⇒ scope_id 必须{}",
                self.scope_type,
                scope_label(&self.scope_type),
                if self.scope_type == SCOPE_ALL {
                    "为空"
                } else {
                    "有值"
                }
            )));
        }
        if !CALC_TYPES.contains(&self.calc_type.as_str()) {
            return Err(bad_request(format!(
                "calc_type 只认 {CALC_TYPES:?}（收到「{}」）",
                self.calc_type
            )));
        }
        if !RULE_STATUSES.contains(&self.status.as_str()) {
            return Err(bad_request(format!(
                "status 只认 {RULE_STATUSES:?}（收到「{}」）",
                self.status
            )));
        }
        check_rate(&self.calc_type, self.rate)?;
        check_amounts(self.min_amount, self.max_amount)?;
        check_effective(self.effective_from, self.effective_to)?;
        Ok(())
    }
}

// ==================== 建 / 改入参 ====================

/// 新建规则入参（`rate` 可为空以给出人话 400，而不是 serde 的 422）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleCreateReq {
    /// 规则名（必填）。
    pub rule_name: String,
    /// 范围类型 `SALESPERSON` / `MATERIAL_CATEGORY` / `ALL`。
    pub scope_type: String,
    /// 范围对象 id（`ALL` 时留空）。
    #[serde(default)]
    pub scope_id: Option<String>,
    /// 计算方式 `PERCENT` / `FIXED`。
    pub calc_type: String,
    /// 比率（PERCENT 百分数 / FIXED 金额）；必填。
    #[serde(default)]
    pub rate: Option<Decimal>,
    /// 起算金额（缺省 0）。
    #[serde(default)]
    pub min_amount: Option<Decimal>,
    /// 单笔封顶（缺省 = 不封顶）。
    #[serde(default)]
    pub max_amount: Option<Decimal>,
    /// 生效起（缺省 = 今天）。
    #[serde(default)]
    pub effective_from: Option<String>,
    /// 生效止（缺省 = 长期）。
    #[serde(default)]
    pub effective_to: Option<String>,
    /// 状态（缺省 `ACTIVE`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 编辑规则入参（**部分更新**：未传字段保持原值）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleUpdateReq {
    /// 规则名。
    #[serde(default)]
    pub rule_name: Option<String>,
    /// 范围类型。
    #[serde(default)]
    pub scope_type: Option<String>,
    /// 范围对象：缺失 = 不改；`null` = 清空（仅 `ALL` 成立）。
    #[serde(default, deserialize_with = "double_option")]
    pub scope_id: Option<Option<String>>,
    /// 计算方式。
    #[serde(default)]
    pub calc_type: Option<String>,
    /// 比率。
    #[serde(default)]
    pub rate: Option<Decimal>,
    /// 起算金额。
    #[serde(default)]
    pub min_amount: Option<Decimal>,
    /// 单笔封顶：缺失 = 不改；`null` = 清空（不封顶）。
    #[serde(default, deserialize_with = "double_option")]
    pub max_amount: Option<Option<Decimal>>,
    /// 生效起。
    #[serde(default)]
    pub effective_from: Option<String>,
    /// 生效止：缺失 = 不改；`null` = 清空（长期有效）。
    #[serde(default, deserialize_with = "double_option")]
    pub effective_to: Option<Option<String>>,
    /// 状态（停用就发 `INACTIVE`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 新建入参 → 归一化规则（不含库查：范围对象是否存在由 handler 查）。
pub fn spec_from_create(
    req: &RuleCreateReq,
    today: NaiveDate,
) -> std::result::Result<RuleSpec, ApiError> {
    let rate = req
        .rate
        .ok_or_else(|| bad_request("rate 必填（PERCENT 填百分数 0–100；FIXED 填金额）"))?;
    let (scope_type, scope_id) = normalize_scope(&req.scope_type, req.scope_id.as_deref())?;
    let calc_type = normalize_calc(&req.calc_type)?;
    let effective_from = match req.effective_from.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => parse_date(raw, "effective_from")?,
        _ => today,
    };
    let effective_to = match req.effective_to.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(parse_date(raw, "effective_to")?),
        _ => None,
    };
    let spec = RuleSpec {
        rule_name: req.rule_name.trim().to_string(),
        scope_type,
        scope_id,
        calc_type,
        rate,
        min_amount: req.min_amount.unwrap_or(Decimal::ZERO),
        max_amount: req.max_amount,
        effective_from,
        effective_to,
        status: normalize_status(req.status.as_deref(), RULE_ACTIVE)?,
        remark: clean_optional(req.remark.clone()),
    };
    spec.validate()?;
    Ok(spec)
}

/// 编辑入参 + 当前值 → 归一化规则（部分更新合并，合并结果再整体校验）。
pub fn spec_from_update(
    current: &RuleSpec,
    req: &RuleUpdateReq,
) -> std::result::Result<RuleSpec, ApiError> {
    let scope_type_raw = req
        .scope_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_string())
        .unwrap_or_else(|| current.scope_type.clone());
    // `scope_id` 三态：缺失 → 沿用；显式 null → 清空；有值 → 覆盖。
    // 注意「换了 scope_type 却没给 scope_id」是最容易出脏数据的一种：带 null 会被
    // normalize_scope 拒（非 ALL 必须给 id），带旧 id 会被 handler 的存在性校验拒。
    let scope_id_raw = match &req.scope_id {
        None => current.scope_id.map(|id| id.to_string()),
        Some(None) => None,
        Some(Some(value)) => Some(value.clone()),
    };
    let (scope_type, scope_id) = normalize_scope(&scope_type_raw, scope_id_raw.as_deref())?;
    let calc_type = match req
        .calc_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(value) => normalize_calc(value)?,
        None => current.calc_type.clone(),
    };
    let effective_from = match req
        .effective_from
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(raw) => parse_date(raw, "effective_from")?,
        None => current.effective_from,
    };
    let effective_to = match &req.effective_to {
        None => current.effective_to,
        Some(None) => None,
        Some(Some(raw)) => {
            let raw = raw.trim();
            if raw.is_empty() {
                None
            } else {
                Some(parse_date(raw, "effective_to")?)
            }
        }
    };
    let max_amount = match &req.max_amount {
        None => current.max_amount,
        Some(None) => None,
        Some(Some(value)) => Some(*value),
    };
    let remark = match &req.remark {
        None => current.remark.clone(),
        Some(raw) => clean_optional(Some(raw.clone())),
    };
    let spec = RuleSpec {
        rule_name: req
            .rule_name
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| text.to_string())
            .unwrap_or_else(|| current.rule_name.clone()),
        scope_type,
        scope_id,
        calc_type,
        rate: req.rate.unwrap_or(current.rate),
        min_amount: req.min_amount.unwrap_or(current.min_amount),
        max_amount,
        effective_from,
        effective_to,
        status: normalize_status(req.status.as_deref(), &current.status)?,
        remark,
    };
    spec.validate()?;
    Ok(spec)
}

// ==================== 出参 ====================

/// 规则出参（列表行 / 新建 / 编辑回单同构）。
#[derive(Debug, Clone, Serialize)]
pub struct RuleRes {
    /// 主键。
    pub id: String,
    /// 规则名。
    pub rule_name: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围对象 id（`ALL` 为 `null`）。
    pub scope_id: Option<String>,
    /// 范围对象显示名（销售员姓名 / 物料名称；`ALL` 为 `null`）。
    pub scope_name: Option<String>,
    /// 计算方式。
    pub calc_type: String,
    /// 比率（PERCENT 百分数 / FIXED 金额），4 位小数字符串。
    pub rate: Decimal,
    /// 起算金额。
    pub min_amount: Decimal,
    /// 单笔封顶（`null` = 不封顶）。
    pub max_amount: Option<Decimal>,
    /// 生效起（含）。
    pub effective_from: NaiveDate,
    /// 生效止（含；`null` = 长期）。
    pub effective_to: Option<NaiveDate>,
    /// 状态 `ACTIVE` / `INACTIVE`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

// ==================== 分页 ====================

/// 分页查询入参（全部可空）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RulePageQuery {
    /// 页码（默认 1）。
    pub page: Option<u64>,
    /// 每页条数（默认 20，上限 200）。
    pub page_size: Option<u64>,
    /// 范围类型精确过滤。
    pub scope_type: Option<String>,
    /// 计算方式精确过滤。
    pub calc_type: Option<String>,
    /// 状态精确过滤。
    pub status: Option<String>,
    /// 范围对象 id 精确过滤。
    pub scope_id: Option<String>,
    /// 关键字：规则名 / 范围对象名 / 备注 模糊。
    pub keyword: Option<String>,
}

/// 数据层筛选条件（空白串 = 没传）。
#[derive(Debug, Clone, Default)]
pub struct RuleFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 范围类型。
    pub scope_type: Option<String>,
    /// 计算方式。
    pub calc_type: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 范围对象 id。
    pub scope_id: Option<Uuid>,
    /// 关键字。
    pub keyword: Option<String>,
}

impl RulePageQuery {
    /// 入参校验 + 归一化（域值乱码一律 400：静默忽略会把「停用规则」筛成「全部规则」）。
    pub fn filter(&self) -> std::result::Result<RuleFilter, ApiError> {
        let non_blank = |raw: &Option<String>| {
            raw.as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| text.to_string())
        };
        let scope_type = match non_blank(&self.scope_type) {
            None => None,
            Some(value) => {
                let upper = value.to_ascii_uppercase();
                if !SCOPE_TYPES.contains(&upper.as_str()) {
                    return Err(bad_request(format!(
                        "scope_type 只认 {SCOPE_TYPES:?}（收到「{value}」）"
                    )));
                }
                Some(upper)
            }
        };
        let calc_type = match non_blank(&self.calc_type) {
            None => None,
            Some(value) => Some(normalize_calc(&value)?),
        };
        let status = match non_blank(&self.status) {
            None => None,
            Some(value) => {
                let upper = value.to_ascii_uppercase();
                if !RULE_STATUSES.contains(&upper.as_str()) {
                    return Err(bad_request(format!(
                        "status 只认 {RULE_STATUSES:?}（收到「{value}」）"
                    )));
                }
                Some(upper)
            }
        };
        let scope_id = match non_blank(&self.scope_id) {
            None => None,
            Some(value) => Some(parse_uuid(&value, "scope_id")?),
        };
        Ok(RuleFilter {
            page: self.page.unwrap_or(0),
            page_size: self.page_size.unwrap_or(0),
            scope_type,
            calc_type,
            status,
            scope_id,
            keyword: non_blank(&self.keyword),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dt(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期")
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("小数")
    }

    fn base_spec() -> RuleSpec {
        RuleSpec {
            rule_name: "华东销售 8%".to_string(),
            scope_type: SCOPE_ALL.to_string(),
            scope_id: None,
            calc_type: CALC_PERCENT.to_string(),
            rate: dec("8"),
            min_amount: Decimal::ZERO,
            max_amount: None,
            effective_from: dt("2026-01-01"),
            effective_to: None,
            status: RULE_ACTIVE.to_string(),
            remark: None,
        }
    }

    // ---------- 域值与 V0309 的 CHECK 对齐 ----------

    #[test]
    fn domain_values_match_v0309_check_constraints() {
        assert_eq!(SCOPE_TYPES, ["SALESPERSON", "MATERIAL_CATEGORY", "ALL"]);
        assert_eq!(CALC_TYPES, ["PERCENT", "FIXED"]);
        assert_eq!(RULE_STATUSES, ["ACTIVE", "INACTIVE"]);
        assert_eq!(MAX_RULE_NAME_LEN, 100, "rule_name VARCHAR(100)");
        assert_eq!(RATE_PRECISION, (8, 4), "rate NUMERIC(8,4)");
        assert_eq!(AMOUNT_PRECISION, (18, 4), "金额列 NUMERIC(18,4)");
        // 范围中文名（前端兜底展示用）
        assert_eq!(scope_label(SCOPE_SALESPERSON), "销售员");
        assert_eq!(scope_label(SCOPE_MATERIAL), "物料");
        assert_eq!(scope_label(SCOPE_ALL), "全局");
        assert_eq!(status_label(RULE_INACTIVE), "停用");
    }

    // ---------- scope ----------

    #[test]
    fn scope_all_forbids_scope_id() {
        let (scope, id) = normalize_scope(" all ", None).expect("全局规则");
        assert_eq!(scope, SCOPE_ALL);
        assert_eq!(id, None);
        // 空白串视同没传
        assert_eq!(normalize_scope("ALL", Some("   ")).unwrap().1, None);
        let err = normalize_scope("ALL", Some("e3600000-0000-4000-8000-000000000011"))
            .expect_err("ALL 不允许 scope_id");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
    }

    #[test]
    fn scoped_types_require_existing_uuid() {
        let id = "e3600000-0000-4000-8000-000000000011";
        let (scope, parsed) = normalize_scope("salesperson", Some(id)).expect("销售员规则");
        assert_eq!(scope, SCOPE_SALESPERSON, "域值归一化成大写");
        assert_eq!(parsed.unwrap().to_string(), id);
        let (scope, parsed) = normalize_scope("material_category", Some(id)).unwrap();
        assert_eq!(scope, SCOPE_MATERIAL);
        assert!(parsed.is_some());
        // 缺 scope_id
        for scoped in [SCOPE_SALESPERSON, SCOPE_MATERIAL] {
            let err = normalize_scope(scoped, None).expect_err("必须给 scope_id");
            assert_eq!(err.status, 400, "{scoped}");
            let err = normalize_scope(scoped, Some("  ")).expect_err("空白同样算没给");
            assert_eq!(err.status, 400, "{scoped}");
        }
        // 乱码 uuid / 未知范围
        assert_eq!(
            normalize_scope(SCOPE_SALESPERSON, Some("nope"))
                .unwrap_err()
                .status,
            400
        );
        assert_eq!(
            normalize_scope("DEPARTMENT", Some(id)).unwrap_err().status,
            400
        );
    }

    // ---------- 比率 ----------

    #[test]
    fn percent_rate_is_capped_at_one_hundred() {
        check_rate(CALC_PERCENT, Decimal::ZERO).expect("0% 合法（虽然无效，但库层允许）");
        check_rate(CALC_PERCENT, Decimal::ONE_HUNDRED).expect("100% 是上限含");
        let err = check_rate(CALC_PERCENT, dec("100.0001")).expect_err("超过 100%");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("0–100"), "{}", err.msg);
    }

    #[test]
    fn fixed_amount_only_needs_non_negative() {
        check_rate(CALC_FIXED, dec("100")).expect("固定额 100 元合法");
        check_rate(CALC_FIXED, dec("9999.9999")).expect("列宽上限内");
        assert_eq!(check_rate(CALC_FIXED, dec("-1")).unwrap_err().status, 400);
        // NUMERIC(8,4) 装不下：10000 元整就溢出了
        assert_eq!(
            check_rate(CALC_FIXED, dec("10000")).unwrap_err().status,
            400
        );
        // 5 位小数会被库静默四舍五入 → 入口拒
        assert_eq!(
            check_rate(CALC_FIXED, dec("1.23456")).unwrap_err().status,
            400
        );
    }

    // ---------- 金额 ----------

    #[test]
    fn amounts_respect_min_and_max_relationship() {
        check_amounts(Decimal::ZERO, None).expect("不设起算额与封顶");
        check_amounts(dec("100"), Some(dec("100"))).expect("封顶等于起算额合法");
        check_amounts(dec("100"), Some(dec("300"))).expect("封顶大于起算额");
        let err = check_amounts(dec("100"), Some(dec("99.9999"))).expect_err("封顶小于起算额");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("min_amount"), "{}", err.msg);
        assert_eq!(check_amounts(dec("-0.0001"), None).unwrap_err().status, 400);
        assert_eq!(
            check_amounts(dec("0"), Some(dec("-1"))).unwrap_err().status,
            400
        );
        // 小数位与列宽
        assert_eq!(check_amounts(dec("1.00001"), None).unwrap_err().status, 400);
    }

    #[test]
    fn effective_range_cannot_go_backwards() {
        check_effective(dt("2026-01-01"), None).expect("长期有效");
        check_effective(dt("2026-01-01"), Some(dt("2026-01-01"))).expect("同一天合法（含端点）");
        let err = check_effective(dt("2026-02-01"), Some(dt("2026-01-31"))).expect_err("倒挂");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("effective_to"), "{}", err.msg);
    }

    #[test]
    fn date_parsing_is_strict() {
        assert_eq!(
            parse_date(" 2026-11-07 ", "effective_from").unwrap(),
            dt("2026-11-07")
        );
        for bad in ["2026/11/07", "2026-11-7", "20261107", ""] {
            assert_eq!(
                parse_date(bad, "effective_from").unwrap_err().status,
                400,
                "{bad}"
            );
        }
    }

    #[test]
    fn status_domain_is_normalized() {
        assert_eq!(normalize_status(None, RULE_ACTIVE).unwrap(), RULE_ACTIVE);
        assert_eq!(
            normalize_status(Some("  "), RULE_ACTIVE).unwrap(),
            RULE_ACTIVE
        );
        assert_eq!(
            normalize_status(Some("inactive"), RULE_ACTIVE).unwrap(),
            RULE_INACTIVE
        );
        let err = normalize_status(Some("DELETED"), RULE_ACTIVE).expect_err("域外值");
        assert_eq!(err.status, 400);
    }

    // ---------- spec ----------

    #[test]
    fn spec_validation_rejects_cross_field_inconsistency() {
        base_spec().validate().expect("基准规格合法");

        let mut bad = base_spec();
        bad.scope_type = SCOPE_SALESPERSON.to_string();
        bad.scope_id = None;
        let err = bad.validate().expect_err("SALESPERSON 缺 scope_id");
        assert!(err.msg.contains("scope_id"), "{}", err.msg);

        let mut bad = base_spec();
        bad.scope_id = Some(Uuid::now_v7());
        assert!(bad.validate().is_err(), "ALL 不能带 scope_id");

        let mut bad = base_spec();
        bad.rule_name = "   ".to_string();
        assert!(bad.validate().unwrap_err().msg.contains("rule_name"));

        let mut bad = base_spec();
        bad.rule_name = "规".repeat(MAX_RULE_NAME_LEN + 1);
        assert!(bad.validate().unwrap_err().msg.contains("100"));

        let mut bad = base_spec();
        bad.remark = Some("备".repeat(MAX_REMARK_LEN + 1));
        assert!(bad.validate().unwrap_err().msg.contains("remark"));

        let mut bad = base_spec();
        bad.calc_type = "MIXED".to_string();
        assert!(bad.validate().is_err());

        let mut bad = base_spec();
        bad.status = "DELETED".to_string();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn create_spec_fills_defaults() {
        let req = RuleCreateReq {
            rule_name: "  外贸部 3% ".to_string(),
            scope_type: "all".to_string(),
            scope_id: None,
            calc_type: "percent".to_string(),
            rate: Some(dec("3")),
            min_amount: None,
            max_amount: None,
            effective_from: None,
            effective_to: None,
            status: None,
            remark: Some("   ".to_string()),
        };
        let today = dt("2026-11-07");
        let spec = spec_from_create(&req, today).expect("合法入参");
        assert_eq!(spec.rule_name, "外贸部 3%", "规则名 trim");
        assert_eq!(spec.scope_type, SCOPE_ALL);
        assert_eq!(
            spec.effective_from, today,
            "没给生效日 = 今天（DDL DEFAULT CURRENT_DATE）"
        );
        assert_eq!(spec.status, RULE_ACTIVE);
        assert_eq!(spec.min_amount, Decimal::ZERO);
        assert_eq!(spec.max_amount, None);
        assert_eq!(spec.remark, None, "空白备注 → NULL");
    }

    #[test]
    fn create_requires_rate() {
        let req = RuleCreateReq {
            rule_name: "没填比率".to_string(),
            scope_type: SCOPE_ALL.to_string(),
            calc_type: CALC_PERCENT.to_string(),
            ..Default::default()
        };
        let err = spec_from_create(&req, dt("2026-11-07")).expect_err("rate 必填");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("rate"), "{}", err.msg);
    }

    #[test]
    fn update_merges_partial_and_can_clear_optionals() {
        let current = RuleSpec {
            scope_type: SCOPE_MATERIAL.to_string(),
            scope_id: Some(Uuid::now_v7()),
            max_amount: Some(dec("60")),
            effective_to: Some(dt("2026-12-31")),
            remark: Some("旧备注".to_string()),
            ..base_spec()
        };
        // 只发 status：其余全部沿用（前端「停用」按钮就是这一发）
        let stop = RuleUpdateReq {
            status: Some("INACTIVE".to_string()),
            ..Default::default()
        };
        let spec = spec_from_update(&current, &stop).expect("停用");
        assert_eq!(spec.status, RULE_INACTIVE);
        assert_eq!(spec.rate, current.rate);
        assert_eq!(spec.max_amount, Some(dec("60")));
        assert_eq!(spec.effective_to, Some(dt("2026-12-31")));
        assert_eq!(spec.scope_id, current.scope_id);

        // 显式 null 清空封顶与生效止
        let clear = RuleUpdateReq {
            max_amount: Some(None),
            effective_to: Some(None),
            remark: Some(String::new()),
            ..Default::default()
        };
        let spec = spec_from_update(&current, &clear).expect("清空可选值");
        assert_eq!(spec.max_amount, None);
        assert_eq!(spec.effective_to, None);
        assert_eq!(spec.remark, None);

        // 显式 null 清 scope_id 但范围是物料 → 合并结果不合法，必须 400
        let bad = RuleUpdateReq {
            scope_id: Some(None),
            ..Default::default()
        };
        assert_eq!(spec_from_update(&current, &bad).unwrap_err().status, 400);
    }

    #[test]
    fn double_option_distinguishes_absent_from_null() {
        // 缺失 → None（不改）
        let absent: RuleUpdateReq = serde_json::from_str(r#"{"rate":"5"}"#).expect("解析");
        assert!(absent.max_amount.is_none());
        assert!(absent.effective_to.is_none());
        assert!(absent.scope_id.is_none());
        // 显式 null → Some(None)（清空）
        let nulled: RuleUpdateReq =
            serde_json::from_str(r#"{"max_amount":null,"effective_to":null,"scope_id":null}"#)
                .expect("解析");
        assert_eq!(nulled.max_amount, Some(None));
        assert_eq!(nulled.effective_to, Some(None));
        assert_eq!(nulled.scope_id, Some(None));
        // 有值 → Some(Some(..))
        let valued: RuleUpdateReq = serde_json::from_str(
            r#"{"max_amount":"60.0000","effective_to":"2026-12-31","scope_id":"e3600000-0000-4000-8000-000000000021"}"#,
        )
        .expect("解析");
        assert_eq!(valued.max_amount, Some(Some(dec("60"))));
        assert_eq!(valued.effective_to, Some(Some("2026-12-31".to_string())));
        assert!(valued.scope_id.is_some());
    }

    // ---------- 分页入参 ----------

    #[test]
    fn page_filter_normalizes_and_rejects_garbage() {
        let query = RulePageQuery {
            page: Some(2),
            page_size: Some(50),
            scope_type: Some(" salesperson ".to_string()),
            calc_type: Some("percent".to_string()),
            status: Some("inactive".to_string()),
            scope_id: Some("e3600000-0000-4000-8000-000000000011".to_string()),
            keyword: Some("  甲  ".to_string()),
        };
        let filter = query.filter().expect("合法筛选");
        assert_eq!(filter.page, 2);
        assert_eq!(filter.page_size, 50);
        assert_eq!(filter.scope_type.as_deref(), Some(SCOPE_SALESPERSON));
        assert_eq!(filter.calc_type.as_deref(), Some(CALC_PERCENT));
        assert_eq!(filter.status.as_deref(), Some(RULE_INACTIVE));
        assert!(filter.scope_id.is_some());
        assert_eq!(filter.keyword.as_deref(), Some("甲"));

        let bad = RulePageQuery {
            status: Some("STOPPED".to_string()),
            ..Default::default()
        };
        assert_eq!(bad.filter().unwrap_err().status, 400);
        let bad = RulePageQuery {
            scope_type: Some("DEPARTMENT".to_string()),
            ..Default::default()
        };
        assert_eq!(bad.filter().unwrap_err().status, 400);
        let bad = RulePageQuery {
            scope_id: Some("not-a-uuid".to_string()),
            ..Default::default()
        };
        assert_eq!(bad.filter().unwrap_err().status, 400);
        // 空白串一律当「没传」
        let blank = RulePageQuery {
            scope_type: Some("  ".to_string()),
            status: Some(String::new()),
            keyword: Some("  ".to_string()),
            ..Default::default()
        };
        let filter = blank.filter().expect("空白不算筛选");
        assert!(filter.scope_type.is_none() && filter.status.is_none() && filter.keyword.is_none());
    }
}

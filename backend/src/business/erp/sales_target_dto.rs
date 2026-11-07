//! 销售目标 / 预测 · 接口契约（SO-44 / ARCH-7 §15.8）。
//!
//! | 方法 | 路径 | 权限码 |
//! |------|------|--------|
//! | GET | `/business/erp/sales-target/page` | `business:erp:sales-target:list` |
//! | GET | `/business/erp/sales-target/achievement` | `business:erp:sales-target:list` |
//! | POST | `/business/erp/sales-target` | `business:erp:sales-target:create` |
//! | PUT | `/business/erp/sales-target/{id}` | `business:erp:sales-target:update` |
//! | DELETE | `/business/erp/sales-target/{id}` | `business:erp:sales-target:delete` |
//! | GET | `/business/erp/sales-forecast/page` | `business:erp:sales-forecast:list` |
//! | POST | `/business/erp/sales-forecast` | `business:erp:sales-forecast:create` |
//! | PUT | `/business/erp/sales-forecast/{id}` | `business:erp:sales-forecast:update` |
//! | DELETE | `/business/erp/sales-forecast/{id}` | `business:erp:sales-forecast:delete` |
//!
//! # 期间码口径（一张目标回答「哪段时间」）
//!
//! `period_code` 只认两种形状（与 `ck_biz_st_target_period` 逐字一致）：
//!
//! ```text
//! yyyy-MM   月度：2026-11      ⇒ [2026-11-01, 2026-12-01)
//! yyyy-QN   季度：2026-Q4      ⇒ [2026-10-01, 2027-01-01)   ← Q 大小写不限，落库转大写
//! ```
//!
//! 区间一律**左闭右开**（`>= from AND < to`）：用 `<= 期末` 会把期末当天 0 点之后的单
//! 漏掉/多算，月份天数还得按大小月分支。`to` 由 [`Period::from_code`] 算出来，
//! 取数 SQL 里只出现 `>= / <`，不再出现「当月最后一天」这种易错写法。
//!
//! # 达成率口径
//!
//! ```text
//! achievement_rate = actual / target × 100   （2 位小数，四舍五入；target = 0 ⇒ 0）
//! gap              = target − actual         （正 = 缺口，负 = 超额）
//! ```
//!
//! `target = 0` 时达成率恒 0（不是 100、不是无穷）：目标填 0 的业务含义是「没定目标」，
//! 报「已达成 100%」会让看板撒谎。这条在 [`achievement_rate`] 里用单测钉住。
//!
//! # 出参字段（前端 SO-44 页逐字对齐，改名前先通知）
//!
//! 金额是 `NUMERIC` ⇒ JSON **字符串**（`"120000.00"`）；达成率也是字符串（`"85.50"`），
//! 前端只展示不自算。achievement 每行同时给**两个口径**：
//! `actual_amount` / `achievement_rate` / `gap` 是按请求参数 `basis` 选中的那一份，
//! `by_order` / `by_receipt` 是两个口径都在 —— 前端切口径不必重新请求。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::shared::error_code;

// ==================== 域值（与 V0318 的 CHECK 逐字一致，单测钉住） ====================

/// 按销售员（`scope_id` = `sys_user.id`）。
pub const SCOPE_SALESPERSON: &str = "SALESPERSON";
/// 按部门（`scope_id` = `sys_dept.id`）。
pub const SCOPE_DEPT: &str = "DEPT";
/// 按物料（`scope_id` = `biz_material.id`；基础版**没有**分类表，与佣金规则同口径）。
pub const SCOPE_MATERIAL: &str = "MATERIAL_CATEGORY";
/// 全局（`scope_id` 必须为 NULL）。
pub const SCOPE_ALL: &str = "ALL";

/// 全部范围类型（顺序即前端下拉顺序）。
pub const SCOPE_TYPES: [&str; 4] = [SCOPE_SALESPERSON, SCOPE_DEPT, SCOPE_MATERIAL, SCOPE_ALL];

/// 目标启用（进看板）。
pub const TARGET_ACTIVE: &str = "ACTIVE";
/// 目标停用（不进看板，历史照旧可查）。
pub const TARGET_INACTIVE: &str = "INACTIVE";

/// 全部目标状态。
pub const TARGET_STATUSES: [&str; 2] = [TARGET_ACTIVE, TARGET_INACTIVE];

/// 信心度：高。
pub const CONFIDENCE_HIGH: &str = "HIGH";
/// 信心度：中。
pub const CONFIDENCE_MEDIUM: &str = "MEDIUM";
/// 信心度：低。
pub const CONFIDENCE_LOW: &str = "LOW";

/// 全部信心度。
pub const CONFIDENCES: [&str; 3] = [CONFIDENCE_HIGH, CONFIDENCE_MEDIUM, CONFIDENCE_LOW];

/// 达成率口径 ①：按订单金额（`APPROVED` 及之后状态）。
pub const BASIS_ORDER: &str = "ORDER";
/// 达成率口径 ②：按已收款金额（财务口径）。
pub const BASIS_RECEIPT: &str = "RECEIPT";

/// 全部口径。
pub const BASIS_TYPES: [&str; 2] = [BASIS_ORDER, BASIS_RECEIPT];

/// 计入「订单口径」的订单状态：`APPROVED` 及之后（卡面原话）。
///
/// `DRAFT` / `SUBMITTED` 还没批，`CANCELLED` 作废 —— 三者都不算业绩。
/// `PARTIAL_OUT` / `OUTBOUND` / `CLOSED` 是已批之后的履约态，都算。
pub const ORDER_STATUSES: [&str; 4] = ["APPROVED", "PARTIAL_OUT", "OUTBOUND", "CLOSED"];

/// 计入「收款口径」的收款单状态：确认及之后（`DRAFT` 还没确认，`CANCELLED` 作废）。
pub const RECEIPT_STATUSES: [&str; 2] = ["CONFIRMED", "POSTED"];

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 期间工具：`period_code` ⇄ 左闭右开区间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Period {
    /// 归一化后的期间码（`Q` 大写）。
    pub code: String,
    /// 起（含）。
    pub from: NaiveDate,
    /// 止（不含）。
    pub to: NaiveDate,
}

impl Period {
    /// 解析期间码。
    ///
    /// 形状先卡死再算日期（`chrono` 的宽松解析放过 `2026-1` 这种写法，
    /// 一旦放进来，库里的 `period_code` 会和前端回显不一致）。
    pub fn from_code(raw: &str) -> std::result::Result<Self, ApiError> {
        let code = raw.trim().to_ascii_uppercase();
        let bytes = code.as_bytes();
        if bytes.len() != 7 || bytes[4] != b'-' || !bytes[..4].iter().all(u8::is_ascii_digit) {
            return Err(bad_request(format!(
                "period_code 只认 yyyy-MM 或 yyyy-QN（收到「{raw}」）"
            )));
        }
        let year: i32 = code[..4]
            .parse()
            .map_err(|_| bad_request(format!("period_code 年份不合法（收到「{raw}」）")))?;
        let tail = &code[5..];
        let (from, to) = if let Some(quarter) = tail.strip_prefix('Q') {
            let quarter: u32 = quarter
                .parse()
                .map_err(|_| bad_request(format!("period_code 季度不合法（收到「{raw}」）")))?;
            if !(1..=4).contains(&quarter) {
                return Err(bad_request(format!(
                    "period_code 季度只能是 Q1..Q4（收到「{raw}」）"
                )));
            }
            let start_month = (quarter - 1) * 3 + 1;
            let from = NaiveDate::from_ymd_opt(year, start_month, 1)
                .ok_or_else(|| bad_request(format!("period_code 日期不存在（收到「{raw}」）")))?;
            let (next_year, next_month) = if start_month == 10 {
                (year + 1, 1)
            } else {
                (year, start_month + 3)
            };
            let to = NaiveDate::from_ymd_opt(next_year, next_month, 1)
                .ok_or_else(|| bad_request(format!("period_code 日期不存在（收到「{raw}」）")))?;
            (from, to)
        } else {
            let month: u32 = tail.parse().map_err(|_| {
                bad_request(format!(
                    "period_code 只认 yyyy-MM 或 yyyy-QN（收到「{raw}」）"
                ))
            })?;
            if !(1..=12).contains(&month) {
                return Err(bad_request(format!(
                    "period_code 月份只能是 01..12（收到「{raw}」）"
                )));
            }
            let from = NaiveDate::from_ymd_opt(year, month, 1)
                .ok_or_else(|| bad_request(format!("period_code 日期不存在（收到「{raw}」）")))?;
            let (next_year, next_month) = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            let to = NaiveDate::from_ymd_opt(next_year, next_month, 1)
                .ok_or_else(|| bad_request(format!("period_code 日期不存在（收到「{raw}」）")))?;
            (from, to)
        };
        Ok(Self { code, from, to })
    }

    /// 只校验形状、不要求能算出日期（列表筛选用的宽松版：期间码照样要合法）。
    pub fn validate_code(raw: &str) -> std::result::Result<String, ApiError> {
        Self::from_code(raw).map(|period| period.code)
    }
}

/// 金额保留 2 位（目标 / 实际 / 缺口都是 `NUMERIC(18,2)` 口径）。
pub fn money2(value: Decimal) -> Decimal {
    value.round_dp(2)
}

/// 达成率 = 实际 / 目标 × 100（2 位）。`target = 0` ⇒ 0（见模块头注释）。
pub fn achievement_rate(actual: Decimal, target: Decimal) -> Decimal {
    if target.is_zero() {
        return Decimal::ZERO;
    }
    (actual / target * Decimal::from(100)).round_dp(2)
}

/// 缺口 = 目标 − 实际（正数 = 还差多少，负数 = 超额完成）。
pub fn gap(target: Decimal, actual: Decimal) -> Decimal {
    money2(target - actual)
}

/// 范围类型 → 中文（前端下拉 / 看板行首列）。
pub fn scope_label(scope_type: &str) -> &'static str {
    match scope_type {
        SCOPE_SALESPERSON => "销售员",
        SCOPE_DEPT => "部门",
        SCOPE_MATERIAL => "物料",
        SCOPE_ALL => "全公司",
        _ => "未知范围",
    }
}

/// 期间码归一化（`2026-q1` → `2026-Q1`）。
pub fn normalize_period(raw: &str) -> std::result::Result<String, ApiError> {
    Period::validate_code(raw)
}

/// 范围归一化：`ALL` 不收 `scope_id`；其余三种必须给。
pub fn normalize_scope(
    scope_type: &str,
    scope_id: Option<&str>,
) -> std::result::Result<(String, Option<Uuid>), ApiError> {
    let scope_type = scope_type.trim().to_ascii_uppercase();
    if !SCOPE_TYPES.contains(&scope_type.as_str()) {
        return Err(bad_request(format!(
            "scope_type 只认 {}（收到「{scope_type}」）",
            SCOPE_TYPES.join(" / ")
        )));
    }
    let raw = scope_id.map(str::trim).filter(|text| !text.is_empty());
    if scope_type == SCOPE_ALL {
        if raw.is_some() {
            return Err(bad_request(
                "scope_type=ALL 时不能带 scope_id（全公司目标没有范围对象）".to_string(),
            ));
        }
        return Ok((scope_type, None));
    }
    let Some(raw) = raw else {
        return Err(bad_request(format!(
            "scope_type={scope_type} 必须给 scope_id（{}）",
            scope_target_hint(&scope_type)
        )));
    };
    let id = Uuid::parse_str(raw)
        .map_err(|_| bad_request(format!("scope_id 不是合法的 UUID（收到「{raw}」）")))?;
    Ok((scope_type, Some(id)))
}

/// 范围对象是哪张表（错误提示里点名，避免前端把物料 id 填到销售员上）。
pub fn scope_target_hint(scope_type: &str) -> &'static str {
    match scope_type {
        SCOPE_SALESPERSON => "sys_user.id",
        SCOPE_DEPT => "sys_dept.id",
        SCOPE_MATERIAL => "biz_material.id",
        _ => "无",
    }
}

/// 状态归一化。
pub fn normalize_status(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let status = raw.map(str::trim).filter(|text| !text.is_empty());
    let Some(status) = status else {
        return Ok(TARGET_ACTIVE.to_string());
    };
    let status = status.to_ascii_uppercase();
    if !TARGET_STATUSES.contains(&status.as_str()) {
        return Err(bad_request(format!(
            "status 只认 {}（收到「{status}」）",
            TARGET_STATUSES.join(" / ")
        )));
    }
    Ok(status)
}

/// 信心度归一化。
pub fn normalize_confidence(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let confidence = raw.map(str::trim).filter(|text| !text.is_empty());
    let Some(confidence) = confidence else {
        return Ok(CONFIDENCE_MEDIUM.to_string());
    };
    let confidence = confidence.to_ascii_uppercase();
    if !CONFIDENCES.contains(&confidence.as_str()) {
        return Err(bad_request(format!(
            "confidence 只认 {}（收到「{confidence}」）",
            CONFIDENCES.join(" / ")
        )));
    }
    Ok(confidence)
}

/// 口径归一化（缺省 = 订单口径）。
pub fn normalize_basis(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let basis = raw.map(str::trim).filter(|text| !text.is_empty());
    let Some(basis) = basis else {
        return Ok(BASIS_ORDER.to_string());
    };
    let basis = basis.to_ascii_uppercase();
    if !BASIS_TYPES.contains(&basis.as_str()) {
        return Err(bad_request(format!(
            "basis 只认 {}（收到「{basis}」）",
            BASIS_TYPES.join(" / ")
        )));
    }
    Ok(basis)
}

/// 金额校验：非负 + 装得进 `NUMERIC(18,2)`（小数位超了会被库静默四舍五入）。
pub fn check_amount(value: Decimal, field: &str) -> std::result::Result<(), ApiError> {
    if value < Decimal::ZERO {
        return Err(bad_request(format!("{field} 不能为负（收到 {value}）")));
    }
    if value.scale() > 2 {
        return Err(bad_request(format!(
            "{field} 最多 2 位小数（收到 {value}）"
        )));
    }
    let limit = Decimal::from(10_i64.pow(16));
    if value.abs() >= limit {
        return Err(bad_request(format!(
            "{field} 超出 NUMERIC(18,2) 范围（收到 {value}）"
        )));
    }
    Ok(())
}

/// 数量校验：非负 + 装得进 `NUMERIC(18,4)`。
pub fn check_qty(value: Decimal, field: &str) -> std::result::Result<(), ApiError> {
    if value < Decimal::ZERO {
        return Err(bad_request(format!("{field} 不能为负（收到 {value}）")));
    }
    if value.scale() > 4 {
        return Err(bad_request(format!(
            "{field} 最多 4 位小数（收到 {value}）"
        )));
    }
    Ok(())
}

/// 三态字段反序列化：**字段缺失** ⇒ `None`（不改）；**显式 `null`** ⇒ `Some(None)`（清空）。
fn double_option<'de, T, D>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ==================== 手机目标：出参 / 入参 ====================

/// 目标出参（列表行 / 新建 / 编辑回单同构）。
#[derive(Debug, Clone, Serialize)]
pub struct TargetRes {
    /// 主键。
    pub id: String,
    /// 目标编号 `STG-yyyyMMdd-000001`。
    pub target_no: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围对象 id（`ALL` 为 `null`）。
    pub scope_id: Option<String>,
    /// 范围显示名（销售员姓名 / 部门名 / 物料名；`ALL` 为 `null`）。
    pub scope_name: Option<String>,
    /// 期间码 `yyyy-MM` / `yyyy-QN`。
    pub period_code: String,
    /// 目标销售额。
    pub target_amount: Decimal,
    /// 目标数量（0 = 没定）。
    pub target_qty: Decimal,
    /// 状态 `ACTIVE` / `INACTIVE`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 新建目标入参。
#[derive(Debug, Clone, Deserialize)]
pub struct TargetCreateReq {
    /// 范围类型（必填）。
    pub scope_type: String,
    /// 范围对象 id（`ALL` 留空）。
    #[serde(default)]
    pub scope_id: Option<String>,
    /// 期间码（必填）。
    pub period_code: String,
    /// 目标销售额（必填，>= 0，最多 2 位小数）。
    pub target_amount: Decimal,
    /// 目标数量（缺省 0）。
    #[serde(default)]
    pub target_qty: Option<Decimal>,
    /// 状态（缺省 `ACTIVE`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 编辑目标入参（**部分更新**：只改传了的字段；`null` = 清空三态字段）。
#[derive(Debug, Clone, Deserialize)]
pub struct TargetUpdateReq {
    /// 范围类型。
    #[serde(default)]
    pub scope_type: Option<String>,
    /// 范围对象：缺失 = 不改；`null` = 清空（只有 `ALL` 成立）。
    #[serde(default, deserialize_with = "double_option")]
    pub scope_id: Option<Option<String>>,
    /// 期间码。
    #[serde(default)]
    pub period_code: Option<String>,
    /// 目标销售额。
    #[serde(default)]
    pub target_amount: Option<Decimal>,
    /// 目标数量。
    #[serde(default)]
    pub target_qty: Option<Decimal>,
    /// 状态。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注：缺失 = 不改；`null` = 清空。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

/// 目标分页入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TargetPageQuery {
    /// 页码（缺省 1）。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    #[serde(default)]
    pub page_size: Option<u64>,
    /// 期间码精确过滤。
    #[serde(default)]
    pub period_code: Option<String>,
    /// 范围类型精确过滤。
    #[serde(default)]
    pub scope_type: Option<String>,
    /// 状态精确过滤。
    #[serde(default)]
    pub status: Option<String>,
    /// 关键字：目标编号 / 备注 / 范围对象名 模糊。
    #[serde(default)]
    pub keyword: Option<String>,
}

/// 目标数据层筛选条件（空白串 = 没传）。
#[derive(Debug, Clone, Default)]
pub struct TargetFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 期间码。
    pub period_code: Option<String>,
    /// 范围类型。
    pub scope_type: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 关键字。
    pub keyword: Option<String>,
}

impl TargetPageQuery {
    /// 入参校验 + 归一化（域值乱码一律 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<TargetFilter, ApiError> {
        let period_code = match self
            .period_code
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => Some(normalize_period(raw)?),
            None => None,
        };
        let scope_type = match self
            .scope_type
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => {
                let normalized = raw.to_ascii_uppercase();
                if !SCOPE_TYPES.contains(&normalized.as_str()) {
                    return Err(bad_request(format!(
                        "scope_type 只认 {}（收到「{raw}」）",
                        SCOPE_TYPES.join(" / ")
                    )));
                }
                Some(normalized)
            }
            None => None,
        };
        let status = match self
            .status
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => Some(normalize_status(Some(raw))?),
            None => None,
        };
        Ok(TargetFilter {
            page: self.page.unwrap_or(0),
            page_size: self.page_size.unwrap_or(0),
            period_code,
            scope_type,
            status,
            keyword: self
                .keyword
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
        })
    }
}

// ==================== 销售预测 ====================

/// 预测出参。
#[derive(Debug, Clone, Serialize)]
pub struct ForecastRes {
    /// 主键。
    pub id: String,
    /// 预测编号 `SFC-yyyyMMdd-000001`。
    pub forecast_no: String,
    /// 客户 id（可空）。
    pub customer_id: Option<String>,
    /// 客户名（派生列）。
    pub customer_name: Option<String>,
    /// 物料 id（可空）。
    pub material_id: Option<String>,
    /// 物料名（派生列）。
    pub material_name: Option<String>,
    /// 期间码。
    pub period_code: String,
    /// 预测金额。
    pub forecast_amount: Decimal,
    /// 信心度 `HIGH` / `MEDIUM` / `LOW`。
    pub confidence: String,
    /// 版本号。
    pub version_no: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 新建预测入参。
#[derive(Debug, Clone, Deserialize)]
pub struct ForecastCreateReq {
    /// 客户 id（可空；与 material_id 同时为空 = 全局预测）。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 物料 id（可空）。
    #[serde(default)]
    pub material_id: Option<String>,
    /// 期间码（必填）。
    pub period_code: String,
    /// 预测金额（必填，>= 0）。
    pub forecast_amount: Decimal,
    /// 信心度（缺省 `MEDIUM`）。
    #[serde(default)]
    pub confidence: Option<String>,
    /// 版本号（缺省 1）。
    #[serde(default)]
    pub version_no: Option<i32>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 编辑预测入参（部分更新）。
#[derive(Debug, Clone, Deserialize)]
pub struct ForecastUpdateReq {
    /// 客户：缺失 = 不改；`null` = 清空。
    #[serde(default, deserialize_with = "double_option")]
    pub customer_id: Option<Option<String>>,
    /// 物料：缺失 = 不改；`null` = 清空。
    #[serde(default, deserialize_with = "double_option")]
    pub material_id: Option<Option<String>>,
    /// 期间码。
    #[serde(default)]
    pub period_code: Option<String>,
    /// 预测金额。
    #[serde(default)]
    pub forecast_amount: Option<Decimal>,
    /// 信心度。
    #[serde(default)]
    pub confidence: Option<String>,
    /// 版本号（新版本请新建行，这里只允许更正录错的版本号）。
    #[serde(default)]
    pub version_no: Option<i32>,
    /// 备注：缺失 = 不改；`null` = 清空。
    #[serde(default, deserialize_with = "double_option")]
    pub remark: Option<Option<String>>,
}

/// 预测分页入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ForecastPageQuery {
    /// 页码。
    #[serde(default)]
    pub page: Option<u64>,
    /// 每页条数。
    #[serde(default)]
    pub page_size: Option<u64>,
    /// 期间码。
    #[serde(default)]
    pub period_code: Option<String>,
    /// 客户 id。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 物料 id。
    #[serde(default)]
    pub material_id: Option<String>,
    /// 信心度。
    #[serde(default)]
    pub confidence: Option<String>,
}

/// 预测数据层筛选条件。
#[derive(Debug, Clone, Default)]
pub struct ForecastFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 期间码。
    pub period_code: Option<String>,
    /// 客户 id。
    pub customer_id: Option<Uuid>,
    /// 物料 id。
    pub material_id: Option<Uuid>,
    /// 信心度。
    pub confidence: Option<String>,
}

impl ForecastPageQuery {
    /// 入参校验 + 归一化。
    pub fn filter(&self) -> std::result::Result<ForecastFilter, ApiError> {
        let period_code = match self
            .period_code
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => Some(normalize_period(raw)?),
            None => None,
        };
        let customer_id = match self
            .customer_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => Some(Uuid::parse_str(raw).map_err(|_| {
                bad_request(format!("customer_id 不是合法的 UUID（收到「{raw}」）"))
            })?),
            None => None,
        };
        let material_id = match self
            .material_id
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => Some(Uuid::parse_str(raw).map_err(|_| {
                bad_request(format!("material_id 不是合法的 UUID（收到「{raw}」）"))
            })?),
            None => None,
        };
        let confidence = match self
            .confidence
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(raw) => Some(normalize_confidence(Some(raw))?),
            None => None,
        };
        Ok(ForecastFilter {
            page: self.page.unwrap_or(0),
            page_size: self.page_size.unwrap_or(0),
            period_code,
            customer_id,
            material_id,
            confidence,
        })
    }
}

// ==================== 达成率看板 ====================

/// 达成率入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AchievementQuery {
    /// 期间码（**必填**）。
    #[serde(default)]
    pub period_code: Option<String>,
    /// 范围类型（缺省 `ALL`：看板只展示一种范围，避免把销售员和部门的目标混在一个列表里）。
    #[serde(default)]
    pub scope_type: Option<String>,
    /// 口径 `ORDER`（缺省）/ `RECEIPT`：决定 `actual_amount` 用哪一份。
    #[serde(default)]
    pub basis: Option<String>,
}

/// 达成率一行（同一范围目标 + 两个口径的实际数）。
#[derive(Debug, Clone, Serialize)]
pub struct AchievementRow {
    /// 范围显示名（销售员姓名 / 部门名 / 物料名 / 全公司）。
    pub scope: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围对象 id（`ALL` 为 `null`）。
    pub scope_id: Option<String>,
    /// 目标编号（点行能查到是哪条目标）。
    pub target_no: String,
    /// 期间码。
    pub period_code: String,
    /// 目标金额。
    pub target_amount: Decimal,
    /// 目标数量。
    pub target_qty: Decimal,
    /// 实际金额（按请求的 `basis` 选中口径）。
    pub actual_amount: Decimal,
    /// 达成率（%，按请求的 `basis`）。
    pub achievement_rate: Decimal,
    /// 缺口（目标 − 实际，按请求的 `basis`；负数 = 超额）。
    pub gap: Decimal,
    /// 订单口径明细。
    pub by_order: AchievementBasis,
    /// 收款口径明细。
    pub by_receipt: AchievementBasis,
}

/// 单口径结果。
#[derive(Debug, Clone, Serialize)]
pub struct AchievementBasis {
    /// 实际金额。
    pub actual_amount: Decimal,
    /// 达成率（%）。
    pub achievement_rate: Decimal,
    /// 缺口。
    pub gap: Decimal,
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期可解析")
    }

    // ---------- 期间码 → 左闭右开区间 ----------

    #[test]
    fn period_month_is_half_open() {
        let period = Period::from_code("2026-11").expect("月度期间可解析");
        assert_eq!(period.code, "2026-11");
        assert_eq!(period.from, date("2026-11-01"));
        // 右开：12-01 00:00 的单属于 12 月，不属于 11 月
        assert_eq!(period.to, date("2026-12-01"));
    }

    #[test]
    fn period_december_rolls_into_next_year() {
        let period = Period::from_code("2026-12").expect("12 月可解析");
        assert_eq!(period.from, date("2026-12-01"));
        assert_eq!(period.to, date("2027-01-01"));
    }

    #[test]
    fn period_quarter_boundaries() {
        let q1 = Period::from_code("2026-Q1").expect("Q1 可解析");
        assert_eq!((q1.from, q1.to), (date("2026-01-01"), date("2026-04-01")));
        let q2 = Period::from_code("2026-Q2").expect("Q2 可解析");
        assert_eq!((q2.from, q2.to), (date("2026-04-01"), date("2026-07-01")));
        let q3 = Period::from_code("2026-Q3").expect("Q3 可解析");
        assert_eq!((q3.from, q3.to), (date("2026-07-01"), date("2026-10-01")));
        let q4 = Period::from_code("2026-Q4").expect("Q4 可解析");
        assert_eq!((q4.from, q4.to), (date("2026-10-01"), date("2027-01-01")));
    }

    #[test]
    fn period_quarter_lowercase_is_normalized() {
        // 前端传 `2026-q4` 也要认，落库统一大写（与 ck_biz_st_target_period 的 `[Qq]` 一致）
        let period = Period::from_code(" 2026-q4 ").expect("小写季度可解析");
        assert_eq!(period.code, "2026-Q4");
        assert_eq!(normalize_period("2026-q1").expect("归一化"), "2026-Q1");
    }

    #[test]
    fn period_rejects_bad_shapes() {
        // 宽松解析（chrono 放过 2026-1）与库侧 CHECK 不一致的写法必须在这里挡住
        for bad in [
            "",
            "2026",
            "2026-1",
            "2026-13",
            "2026-00",
            "2026-Q0",
            "2026-Q5",
            "2026Q1",
            "2026-AA",
            "abcd-11",
            "2026-11-01",
        ] {
            let error = Period::from_code(bad).expect_err(&format!("「{bad}」应被拒"));
            assert_eq!(error.status, 400, "「{bad}」");
            assert_eq!(error.code, error_code::PARAM_INVALID, "「{bad}」");
        }
    }

    // ---------- 达成率 / 缺口 ----------

    #[test]
    fn achievement_rate_matches_dashboard_expectation() {
        // 看板验收值：目标 20000 / 实际 11300 ⇒ 56.50%
        assert_eq!(
            achievement_rate(dec("11300.00"), dec("20000.00")),
            dec("56.50")
        );
        // 目标 5000 / 实际 6780 ⇒ 135.60%（超额）
        assert_eq!(
            achievement_rate(dec("6780.00"), dec("5000.00")),
            dec("135.60")
        );
        // 目标 50000 / 实际 11300 ⇒ 22.60%
        assert_eq!(
            achievement_rate(dec("11300.00"), dec("50000.00")),
            dec("22.60")
        );
        // 刚好达成
        assert_eq!(
            achievement_rate(dec("11300.00"), dec("11300.00")),
            dec("100.00")
        );
        // 四舍五入到 2 位（1/3 ⇒ 33.33）
        assert_eq!(achievement_rate(dec("1"), dec("3")), dec("33.33"));
    }

    #[test]
    fn achievement_rate_target_zero_is_zero_not_infinity() {
        // 目标 0 = 没定目标，报 100% 会让看板撒谎
        assert_eq!(
            achievement_rate(dec("11300.00"), Decimal::ZERO),
            Decimal::ZERO
        );
        assert_eq!(
            achievement_rate(Decimal::ZERO, Decimal::ZERO),
            Decimal::ZERO
        );
    }

    #[test]
    fn gap_is_target_minus_actual() {
        assert_eq!(gap(dec("20000.00"), dec("11300.00")), dec("8700.00"));
        // 负 = 超额完成
        assert_eq!(gap(dec("5000.00"), dec("6780.00")), dec("-1780.00"));
        assert_eq!(gap(dec("11300.00"), dec("11300.00")), Decimal::ZERO);
        // 与达成率的符号互相印证：gap < 0 ⇔ rate > 100
        let (target, actual) = (dec("5000.00"), dec("6780.00"));
        assert!(gap(target, actual) < Decimal::ZERO);
        assert!(achievement_rate(actual, target) > dec("100"));
    }

    #[test]
    fn money2_rounds_to_two_places() {
        assert_eq!(money2(dec("11300.004")), dec("11300.00"));
        assert_eq!(money2(dec("11300.0055")), dec("11300.01"));
        assert_eq!(money2(dec("0.1")), dec("0.10"));
        // rust_decimal 默认舍入是「银行家舍入」（MidpointNearestEven）：
        // 正好卡在中间时取偶数位 ⇒ .005 落到 .00、.015 进位到 .02。
        // 这里钉住的是**现行行为**，改舍入口径要连带改看板的期待值。
        assert_eq!(money2(dec("11300.005")), dec("11300.00"));
        assert_eq!(money2(dec("11300.015")), dec("11300.02"));
    }

    // ---------- 域值归一化 ----------

    #[test]
    fn normalize_scope_enforces_scope_id_rule() {
        let id = Uuid::now_v7();
        // ALL 不许带 scope_id（库侧 ck_biz_st_target_scope_id 同口径）
        let error =
            normalize_scope(SCOPE_ALL, Some(&id.to_string())).expect_err("ALL 带 id 应被拒");
        assert_eq!(error.status, 400);
        assert_eq!(
            normalize_scope(SCOPE_ALL, None).expect("ALL 不带 id"),
            (SCOPE_ALL.to_string(), None)
        );
        // 其余三种必须带
        let error = normalize_scope(SCOPE_SALESPERSON, None).expect_err("缺 scope_id 应被拒");
        assert_eq!(error.code, error_code::PARAM_INVALID);
        let (scope, scope_id) =
            normalize_scope("salesperson", Some(&format!(" {} ", id))).expect("小写 + 空白可解析");
        assert_eq!(scope, SCOPE_SALESPERSON);
        assert_eq!(scope_id, Some(id));
        // 域值外的写法不静默忽略
        assert!(normalize_scope("REGION", Some(&id.to_string())).is_err());
    }

    #[test]
    fn normalize_status_and_confidence_defaults() {
        assert_eq!(normalize_status(None).expect("默认"), TARGET_ACTIVE);
        assert_eq!(
            normalize_status(Some(" inactive ")).expect("小写"),
            TARGET_INACTIVE
        );
        assert!(normalize_status(Some("DELETED")).is_err());

        assert_eq!(normalize_confidence(None).expect("默认"), CONFIDENCE_MEDIUM);
        assert_eq!(
            normalize_confidence(Some("high")).expect("小写"),
            CONFIDENCE_HIGH
        );
        assert_eq!(
            normalize_confidence(Some("LOW")).expect("大写"),
            CONFIDENCE_LOW
        );
        assert!(normalize_confidence(Some("SURE")).is_err());
    }

    #[test]
    fn normalize_basis_defaults_to_order() {
        assert_eq!(normalize_basis(None).expect("默认"), BASIS_ORDER);
        assert_eq!(
            normalize_basis(Some(" receipt ")).expect("小写"),
            BASIS_RECEIPT
        );
        assert!(normalize_basis(Some("CASH")).is_err());
    }

    #[test]
    fn domain_values_match_v0318_checks() {
        // 与迁移里的 CHECK 逐字一致：改这里必须同步改 V0318（否则库拒而接口放行）
        assert_eq!(
            SCOPE_TYPES,
            [SCOPE_SALESPERSON, SCOPE_DEPT, SCOPE_MATERIAL, SCOPE_ALL]
        );
        assert_eq!(TARGET_STATUSES, [TARGET_ACTIVE, TARGET_INACTIVE]);
        assert_eq!(
            CONFIDENCES,
            [CONFIDENCE_HIGH, CONFIDENCE_MEDIUM, CONFIDENCE_LOW]
        );
        assert_eq!(BASIS_TYPES, [BASIS_ORDER, BASIS_RECEIPT]);
        // 看板取数的状态白名单：作废单绝不进业绩
        assert!(!ORDER_STATUSES.contains(&"CANCELLED"));
        assert!(!ORDER_STATUSES.contains(&"DRAFT"));
        // 收款只认已确认 / 已过账
        assert_eq!(RECEIPT_STATUSES, ["CONFIRMED", "POSTED"]);
    }

    #[test]
    fn scope_label_covers_all_domains() {
        assert_eq!(scope_label(SCOPE_SALESPERSON), "销售员");
        assert_eq!(scope_label(SCOPE_DEPT), "部门");
        assert_eq!(scope_label(SCOPE_MATERIAL), "物料");
        assert_eq!(scope_label(SCOPE_ALL), "全公司");
        assert_eq!(scope_label("REGION"), "未知范围");
    }

    // ---------- 金额 / 数量校验 ----------

    #[test]
    fn check_amount_rejects_negative_and_over_precision() {
        assert!(check_amount(dec("0"), "target_amount").is_ok());
        assert!(check_amount(dec("11300.00"), "target_amount").is_ok());
        // 负数：达成率符号会翻转，必须挡
        assert_eq!(
            check_amount(dec("-1"), "target_amount")
                .expect_err("负金额应被拒")
                .status,
            400
        );
        // 3 位小数会被库静默四舍五入，宁可在入口 400
        assert!(check_amount(dec("1.234"), "target_amount").is_err());
        // 超出 NUMERIC(18,2)（>= 10^16）
        assert!(check_amount(dec("10000000000000000"), "target_amount").is_err());
    }

    #[test]
    fn check_qty_allows_four_decimals() {
        assert!(check_qty(dec("0"), "target_qty").is_ok());
        assert!(check_qty(dec("60.1234"), "target_qty").is_ok());
        assert!(check_qty(dec("60.12345"), "target_qty").is_err());
        assert!(check_qty(dec("-1"), "target_qty").is_err());
    }

    // ---------- 分页筛选归一化 ----------

    #[test]
    fn target_page_filter_normalizes_and_rejects() {
        let query = TargetPageQuery {
            page: None,
            page_size: None,
            period_code: Some(" 2026-q4 ".to_string()),
            scope_type: Some(" salesperson ".to_string()),
            status: Some("active".to_string()),
            keyword: Some("   ".to_string()),
        };
        let filter = query.filter().expect("合法筛选条件");
        assert_eq!(filter.period_code.as_deref(), Some("2026-Q4"));
        assert_eq!(filter.scope_type.as_deref(), Some(SCOPE_SALESPERSON));
        assert_eq!(filter.status.as_deref(), Some(TARGET_ACTIVE));
        // 空白关键字 = 没传（否则会拼出 LIKE '%%' 或 LIKE '   '）
        assert!(filter.keyword.is_none());
        assert_eq!((filter.page, filter.page_size), (0, 0));

        let bad = TargetPageQuery {
            page: None,
            page_size: None,
            period_code: Some("2026-13".to_string()),
            scope_type: None,
            status: None,
            keyword: None,
        };
        assert!(bad.filter().is_err());
    }
}

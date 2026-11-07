//! 预算接口契约（ARCH-6 §3.4）：编制 / 修改 / 调整 / 分页 / 详情 / 执行报表 / 科目映射。
//!
//! 与 `contract_dto` / `purchase_dto` 同一套约定：
//! 主键与外键一律 `String`（`Uuid` 没开 serde）、日期 `NaiveDate`、
//! 金额 `Decimal`（**JSON 里是字符串**，定点四位）、枚举一律大写字符串且与 DDL 的
//! `CHECK` 逐字一致。
//!
//! # 校验分工
//!
//! * **只依赖请求本身**的规则在本文件（纯函数、可单测）：12 行分解齐全 / 每月一次 /
//!   金额非负 / `Σ 分解 = 年度额度` / 调整必填原因（[`validate_periods`]、
//!   [`validate_adjust`]）；
//! * 依赖库的规则在 handler：部门/科目存在且科目末级、槽唯一、在审调整判重；
//! * 落在 DDL 的规则两边都做：应用层先拦成 400，DB 的 CHECK 是最后一道闸。
//!
//! # 月度分解的「三条填充」（ARCH-6 §3.4.1）
//!
//! 前端给「平均分摊 / 按上年比例 / 手工」三种初值，**后端只校验 Σ**；
//! 但后端也提供 [`even_split`] 供「只给年度额度、不给分解」的调用方（脚本 / 集成）
//! 使用 —— 免得为了调一次 API 手工算 12 个数。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::budget_guard::{BudgetVerdict, ControlMode, UsageRow};
use crate::business::erp::budget_model::{self, BudgetRow, MapRow, PeriodRow, ReportRow};
use crate::business::erp::dto::MATERIAL_TYPES;
use crate::shared::error_code;

/// 默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;
/// 每页条数硬上限。
pub const MAX_PAGE_SIZE: u64 = 100;
/// 备注长度上限。
pub const MAX_REMARK_LEN: usize = 2000;
/// 调整原因长度上限（`change_reason VARCHAR(500)`）。
pub const MAX_REASON_LEN: usize = 500;

/// 预算状态白名单（与 `state_machine::Entity::Budget` 的边表同源）。
pub const STATUSES: [&str; 6] = [
    "DRAFT",
    "SUBMITTED",
    "ACTIVE",
    "SUPERSEDED",
    "CLOSED",
    "CANCELLED",
];

/// 参数错误（400 + `10001`）。
pub fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, msg.into())
}

/// 冲突错误（409）。
pub fn conflict(msg: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, msg.into())
}

/// 分页参数归一化。
pub fn normalize_page(page: u64, page_size: u64) -> (u64, u64) {
    let page = page.max(1);
    let page_size = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size.min(MAX_PAGE_SIZE)
    };
    (page, page_size)
}

/// 分页筛选（列表与报表共用）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BudgetFilter {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 年度。
    pub fiscal_year: Option<i16>,
    /// 部门（`String`：与其它 ERP DTO 同口径，`Uuid` 没开 serde feature）。
    pub dept_id: Option<String>,
    /// 科目。
    pub account_id: Option<String>,
    /// 状态（列表用）。
    pub status: Option<String>,
    /// ORIGINAL / ADJUST。
    pub change_type: Option<String>,
    /// 单号 / 科目 / 部门模糊。
    pub keyword: Option<String>,
}

impl BudgetFilter {
    /// 归一化后的分页参数。
    pub fn paging(&self) -> (u64, u64) {
        normalize_page(
            self.page.unwrap_or(1),
            self.page_size.unwrap_or(DEFAULT_PAGE_SIZE),
        )
    }
}

/// 一条月度分解（入参）。
#[derive(Debug, Clone, Deserialize)]
pub struct PeriodReq {
    /// 期间 `yyyy-MM`（可选：不给则按 `month` + 年度拼）。
    pub period_code: Option<String>,
    /// 月份 1..12。
    pub month: Option<i16>,
    /// 额度（字符串定点；JSON 数字也吃）。
    pub amount: Option<Decimal>,
    /// 备注。
    pub remark: Option<String>,
}

/// 新建预算。
#[derive(Debug, Clone, Deserialize)]
pub struct CreateBudgetReq {
    /// 年度。
    pub fiscal_year: i16,
    /// 部门 id。
    pub dept_id: String,
    /// 预算科目 id（必须末级）。
    pub account_id: String,
    /// 年度额度。
    pub total_amount: Decimal,
    /// 槽级控制策略（空 = 继承全局）。
    pub control_mode: Option<String>,
    /// 12 个月分解（空数组 = 平均分摊）。
    pub periods: Option<Vec<PeriodReq>>,
    /// 备注。
    pub remark: Option<String>,
}

/// 修改草稿（整单覆盖）。
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBudgetReq {
    /// 年度额度。
    pub total_amount: Decimal,
    /// 槽级控制策略。
    pub control_mode: Option<String>,
    /// 12 个月分解。
    pub periods: Option<Vec<PeriodReq>>,
    /// 备注。
    pub remark: Option<String>,
}

/// 调整（新增版本）。
#[derive(Debug, Clone, Deserialize)]
pub struct AdjustBudgetReq {
    /// 新的年度额度。
    pub total_amount: Decimal,
    /// 槽级控制策略。
    pub control_mode: Option<String>,
    /// 12 个月分解。
    pub periods: Option<Vec<PeriodReq>>,
    /// **调整原因（必填）**：调低到「已用 > 新额度」时更要说清为什么。
    pub change_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 校验通过后的分解行（模型层直接用）。
pub fn validate_periods(
    fiscal_year: i16,
    total_amount: Decimal,
    periods: Option<&[PeriodReq]>,
) -> std::result::Result<Vec<budget_model::NewPeriod>, ApiError> {
    if total_amount < Decimal::ZERO {
        return Err(invalid(format!(
            "年度额度不能为负（当前 {total_amount}）：预算额度是「批准多少钱」，减额度请走调整"
        )));
    }
    // 没给分解 → 平均分摊（尾差记 12 月）
    let rows: Vec<(String, i16, Decimal, Option<String>)> = match periods {
        None => even_split(fiscal_year, total_amount),
        Some([]) => even_split(fiscal_year, total_amount),
        Some(list) => {
            if list.len() != 12 {
                return Err(invalid(format!(
                    "月度分解必须正好 12 行（当前 {} 行）：预算按自然年编制，缺月会让「月度闸」失效",
                    list.len()
                )));
            }
            let mut month_seen = [false; 13];
            let mut out = Vec::with_capacity(12);
            for item in list {
                let month = match (item.month, item.period_code.as_deref()) {
                    (Some(month), _) => month,
                    (None, Some(code)) => code
                        .get(5..7)
                        .and_then(|text| text.parse::<i16>().ok())
                        .ok_or_else(|| invalid(format!("期间码 {code} 非法（应为 yyyy-MM）")))?,
                    (None, None) => {
                        return Err(invalid("每行分解必须给 month 或 period_code"));
                    }
                };
                if !(1..=12).contains(&month) {
                    return Err(invalid(format!("month={month} 越界（1..12）")));
                }
                if month_seen[usize::try_from(month).unwrap_or(0)] {
                    return Err(invalid(format!(
                        "期间重复：{fiscal_year}-{month:02} 出现了不止一次（一个月只能一行额度）"
                    )));
                }
                month_seen[usize::try_from(month).unwrap_or(0)] = true;
                let code = format!("{fiscal_year}-{month:02}");
                if let Some(given) = item.period_code.as_deref()
                    && given.trim() != code
                {
                    return Err(invalid(format!(
                        "期间码 {} 与年度 {fiscal_year} / 月份 {month} 不符（应为 {code}）",
                        given.trim()
                    )));
                }
                let amount = item.amount.unwrap_or(Decimal::ZERO);
                if amount < Decimal::ZERO {
                    return Err(invalid(format!(
                        "{code} 的月度额度不能为负（当前 {amount}）：要减额度请整体调整"
                    )));
                }
                out.push((code, month, amount, item.remark.clone()));
            }
            // 缺月：12 行都在但月份不全是可能的（重复被上面拦住后，这里只可能是漏月）
            if let Some(missing) = (1..=12).find(|m| !month_seen[usize::try_from(*m).unwrap_or(0)])
            {
                return Err(invalid(format!(
                    "缺少 {fiscal_year}-{missing:02} 的额度（12 个月必须齐全）"
                )));
            }
            out
        }
    };
    // Σ 校验（尾差记 12 月是**前端/调用方**的责任；后端只认「必须相等」）
    let sum: Decimal = rows.iter().map(|(_, _, amount, _)| *amount).sum();
    if sum != total_amount {
        return Err(invalid(format!(
            "12 个月合计 {sum} ≠ 年度额度 {total_amount}（差额 {}）：请把尾差记到 12 月",
            total_amount - sum
        )));
    }
    Ok(rows
        .into_iter()
        .map(
            |(period_code, period_month, amount, remark)| budget_model::NewPeriod {
                period_code,
                period_month,
                amount,
                remark,
            },
        )
        .collect())
}

/// 平均分摊（尾差记 12 月）。
pub fn even_split(fiscal_year: i16, total: Decimal) -> Vec<(String, i16, Decimal, Option<String>)> {
    let months = Decimal::from(12);
    let base = (total / months).round_dp(2);
    let mut out = Vec::with_capacity(12);
    let mut assigned = Decimal::ZERO;
    for month in 1..=12i16 {
        let amount = if month == 12 { total - assigned } else { base };
        assigned += amount;
        out.push((format!("{fiscal_year}-{month:02}"), month, amount, None));
    }
    out
}

/// 调整校验：原因必填 + 分解校验。
pub fn validate_adjust(
    fiscal_year: i16,
    total_amount: Decimal,
    periods: Option<&[PeriodReq]>,
    change_reason: Option<&str>,
) -> std::result::Result<(Vec<budget_model::NewPeriod>, String), ApiError> {
    let reason = change_reason.map(str::trim).unwrap_or_default();
    if reason.is_empty() {
        return Err(invalid(
            "预算调整必须填写调整原因（change_reason）：调整 = 改本部门额度，审计要能说清为什么",
        ));
    }
    if reason.chars().count() > MAX_REASON_LEN {
        return Err(invalid(format!(
            "调整原因超过 {MAX_REASON_LEN} 字（当前 {} 字）",
            reason.chars().count()
        )));
    }
    let periods = validate_periods(fiscal_year, total_amount, periods)?;
    Ok((periods, reason.to_string()))
}

/// 控制策略入参校验（空 = 继承）。
pub fn validate_control_mode(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => ControlMode::parse(text)
            .map(|mode| Some(mode.as_str().to_string()))
            .ok_or_else(|| {
                invalid(format!(
                    "control_mode={text} 非法（只能是 OFF / WARN / BLOCK，留空 = 继承全局 erp.budget.control_mode）"
                ))
            }),
    }
}

// ==================== 出参 ====================

/// 列表行。
#[derive(Debug, Clone, Serialize)]
pub struct BudgetRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub budget_no: String,
    /// 年度。
    pub fiscal_year: i16,
    /// 部门 id。
    pub dept_id: String,
    /// 部门名。
    pub dept_name: String,
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名。
    pub account_name: String,
    /// 年度额度。
    pub total_amount: Decimal,
    /// 槽级策略。
    pub control_mode: Option<String>,
    /// 版本号。
    pub version_no: i32,
    /// ORIGINAL / ADJUST。
    pub change_type: String,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 审批实例 id。
    pub approval_instance_id: Option<String>,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 通过时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl From<&BudgetRow> for BudgetRes {
    fn from(row: &BudgetRow) -> Self {
        Self {
            id: row.id.to_string(),
            budget_no: row.budget_no.clone(),
            fiscal_year: row.fiscal_year,
            dept_id: row.dept_id.to_string(),
            dept_name: row.dept_name.clone(),
            account_id: row.account_id.to_string(),
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            total_amount: row.total_amount,
            control_mode: row.control_mode.clone(),
            version_no: row.version_no,
            change_type: row.change_type.clone(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            submitted_at: row.submitted_at,
            approved_at: row.approved_at,
            remark: row.remark.clone(),
            created_at: row.created_at,
        }
    }
}

/// 状态中文名。
pub fn status_label(status: &str) -> &'static str {
    match status {
        budget_model::STATUS_DRAFT => "草稿",
        budget_model::STATUS_SUBMITTED => "审批中",
        budget_model::STATUS_ACTIVE => "已生效",
        budget_model::STATUS_SUPERSEDED => "已被取代",
        budget_model::STATUS_CLOSED => "已关闭",
        budget_model::STATUS_CANCELLED => "已作废",
        _ => "未知",
    }
}

/// 月度分解行（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct PeriodRes {
    /// 期间。
    pub period_code: String,
    /// 月份。
    pub period_month: i16,
    /// 额度。
    pub amount: Decimal,
    /// 已用（现算）。
    pub used: Decimal,
    /// 可用（额度 − 已用，负数 = 该月超支）。
    pub available: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

impl PeriodRes {
    /// 从模型行 + 当月已用组装。
    pub fn of(row: &PeriodRow, used: Decimal) -> Self {
        Self {
            period_code: row.period_code.clone(),
            period_month: row.period_month,
            amount: row.amount,
            used,
            available: row.amount - used,
            remark: row.remark.clone(),
        }
    }
}

/// 详情。
#[derive(Debug, Clone, Serialize)]
pub struct BudgetDetailRes {
    /// 主表。
    pub budget: BudgetRes,
    /// 12 个月分解（含当月已用）。
    pub periods: Vec<PeriodRes>,
    /// 占用 / 执行记录。
    pub usages: Vec<UsageRes>,
    /// 年度已用（OCCUPIED + CONSUMED）。
    pub year_used: Decimal,
    /// 年度可用（额度 − 已用，现算）。
    pub year_available: Decimal,
    /// 同槽版本链。
    pub versions: Vec<BudgetRes>,
    /// 下一步可流转状态（状态机给，不硬编码）。
    pub next_statuses: Vec<String>,
}

/// 占用记录（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct UsageRes {
    /// 主键。
    pub id: String,
    /// 期间。
    pub period_code: String,
    /// 金额。
    pub amount: Decimal,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 来源类型（`EXPENSE_REPORT` / `PURCHASE_ORDER`，按占用行实际值展示）。
    pub source_type: String,
    /// 来源类型中文。
    pub source_type_label: String,
    /// 来源单据 id。
    pub source_id: String,
    /// 来源单号。
    pub source_no: String,
    /// 提交时是否超预算。
    pub over_budget: bool,
    /// 提交时超支额。
    pub over_amount: Decimal,
    /// 释放原因。
    pub release_reason: Option<String>,
}

impl UsageRes {
    /// 从模型行组装。
    ///
    /// `source_type` **取自占用行本身**（FIX-2 问题 3：原来写死 `PURCHASE_ORDER`，
    /// 导致按槽查出来的报销占用被标成采购单）。
    pub fn of(row: &UsageRow) -> Self {
        Self {
            id: row.id.to_string(),
            period_code: row.period_code.clone(),
            amount: row.amount,
            status: row.status.clone(),
            status_label: match row.status.as_str() {
                "OCCUPIED" => "占用中",
                "CONSUMED" => "已执行",
                "RELEASED" => "已释放",
                _ => "未知",
            }
            .to_string(),
            source_type: row.source_type.clone(),
            source_type_label: source_type_label(&row.source_type).to_string(),
            source_id: row.source_id.to_string(),
            source_no: row.source_no.clone(),
            over_budget: row.over_budget,
            over_amount: row.over_amount,
            release_reason: row.release_reason.clone(),
        }
    }
}

/// 占用来源的中文名（未知来源原样回显，不吞掉）。
pub fn source_type_label(source_type: &str) -> &str {
    match source_type {
        crate::business::erp::budget_guard::SOURCE_EXPENSE => "费用报销单",
        crate::business::erp::budget_guard::SOURCE_PURCHASE => "采购订单",
        other => other,
    }
}

/// 「该科目预算尚未生效」提示（出参，报销单 / 采购单详情共用）。
///
/// 只提示、不阻断：控制口径仍是「只认本期生效的 ACTIVE 预算」（见 `budget_guard` 模块文档）。
#[derive(Debug, Clone, Serialize)]
pub struct BudgetWarningRes {
    /// 提示码（`BUDGET_NOT_EFFECTIVE`）。
    pub code: String,
    /// 预算科目 id。
    pub account_id: String,
    /// 预算科目编码。
    pub account_code: String,
    /// 预算单号。
    pub budget_no: String,
    /// 预算状态（`DRAFT` / `SUBMITTED` / `ACTIVE`）。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 一句话说明（前端直接展示）。
    pub message: String,
}

impl From<&crate::business::erp::budget_guard::BudgetWarning> for BudgetWarningRes {
    fn from(warning: &crate::business::erp::budget_guard::BudgetWarning) -> Self {
        Self {
            code: warning.code.clone(),
            account_id: warning.account_id.to_string(),
            account_code: warning.account_code.clone(),
            budget_no: warning.budget_no.clone(),
            status: warning.status.clone(),
            status_label: warning.status_label.clone(),
            message: warning.message.clone(),
        }
    }
}

/// 执行报表行。
#[derive(Debug, Clone, Serialize)]
pub struct ReportRes {
    /// 预算版本 id。
    pub budget_id: String,
    /// 年度。
    pub fiscal_year: i16,
    /// 部门。
    pub dept_id: String,
    /// 部门名。
    pub dept_name: String,
    /// 科目。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名。
    pub account_name: String,
    /// 年度额度。
    pub total_amount: Decimal,
    /// 占用中。
    pub occupied: Decimal,
    /// 已执行。
    pub consumed: Decimal,
    /// 已用合计。
    pub used: Decimal,
    /// 可用（负数 = 超支）。
    pub available: Decimal,
    /// 执行率（百分比，两位小数）。
    pub exec_ratio: Decimal,
    /// 是否超支。
    pub over: bool,
    /// 生效起始日（`effective_from`，NULL = 历史数据没有生效日）。
    pub effective_from: Option<chrono::NaiveDate>,
    /// 报表时点是否已生效（`effective_from <= 本月月末`；与控制的期间月末口径一致）。
    pub effective: bool,
    /// 分月：期间 / 额度 / 已用。
    pub months: Vec<MonthRes>,
}

/// 分月明细。
#[derive(Debug, Clone, Serialize)]
pub struct MonthRes {
    /// 期间。
    pub period_code: String,
    /// 额度。
    pub amount: Decimal,
    /// 已用。
    pub used: Decimal,
    /// 可用。
    pub available: Decimal,
}

impl From<&ReportRow> for ReportRes {
    fn from(row: &ReportRow) -> Self {
        let used = row.used();
        let available = row.available();
        Self {
            budget_id: row.budget_id.to_string(),
            fiscal_year: row.fiscal_year,
            dept_id: row.dept_id.to_string(),
            dept_name: row.dept_name.clone(),
            account_id: row.account_id.to_string(),
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            total_amount: row.total_amount,
            occupied: row.occupied,
            consumed: row.consumed,
            used,
            available,
            exec_ratio: row.exec_ratio(),
            over: available < Decimal::ZERO,
            effective_from: row.effective_from,
            effective: row.effective,
            months: row
                .months
                .iter()
                .map(|(period_code, amount, used)| MonthRes {
                    period_code: period_code.clone(),
                    amount: *amount,
                    used: *used,
                    available: *amount - *used,
                })
                .collect(),
        }
    }
}

/// 映射行（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct MappingRes {
    /// 主键。
    pub id: String,
    /// 映射类型。
    pub map_type: String,
    /// 目标 id。
    pub ref_id: Option<String>,
    /// 目标编码。
    pub ref_code: Option<String>,
    /// 预算科目。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名。
    pub account_name: String,
    /// 部门覆盖。
    pub dept_id: Option<String>,
    /// 优先级。
    pub priority: i32,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
}

impl From<&MapRow> for MappingRes {
    fn from(row: &MapRow) -> Self {
        Self {
            id: row.id.to_string(),
            map_type: row.map_type.clone(),
            ref_id: row.ref_id.map(|id| id.to_string()),
            ref_code: row.ref_code.clone(),
            account_id: row.account_id.to_string(),
            account_code: row.account_code.clone(),
            account_name: row.account_name.clone(),
            dept_id: row.dept_id.map(|id| id.to_string()),
            priority: row.priority,
            status: row.status,
            remark: row.remark.clone(),
        }
    }
}

/// 映射入参。
///
/// # `ref_id` / `ref_code` 的语义（**一张表两种目标形态**，别混用）
///
/// 映射表 `biz_budget_subject_map` 的目标有两种落法（DDL 见
/// `V0240__erp_budget_mapping_material_type`）：
///
/// | map_type | `ref_id` | `ref_code` |
/// |---|---|---|
/// | `MATERIAL` | **必填**：`biz_material.id` | 可选：物料编码（展示 / 对账用） |
/// | `WAREHOUSE` | **必填**：`biz_warehouse.id` | 可选：仓库编码（展示 / 对账用） |
/// | `MATERIAL_TYPE` | **必须为空** —— 物料类型是 `biz_material.material_type` 上的字符串枚举（[`MATERIAL_TYPES`]，7 值），全库没有 `biz_material_type` 表、没有 uuid | **必填**：类型编码，`budget_model::resolve_for_po` 就是拿采购单明细里物料的 `material_type` 与它比对 |
/// | `DEFAULT` | 必须为空（全公司兜底） | 必须为空（兜底不挂任何目标编码） |
///
/// 为什么 `MATERIAL_TYPE` 走编码而不是 id：物料类型从来没有独立主数据表
/// （`ck_biz_material_type` 是 `biz_material` 上的 CHECK 枚举），造一张表属于
/// 主数据化功能，不在本缺陷范围内。唯一索引
/// `uk_biz_bsm_target (map_type, COALESCE(ref_id::text, upper(btrim(ref_code))), dept)`
/// 负责「同一部门同一目标只能一条」，所以不写 `ref_id` 也不会绕过唯一性。
#[derive(Debug, Clone, Deserialize)]
pub struct MappingReq {
    /// MATERIAL / MATERIAL_TYPE / WAREHOUSE / DEFAULT。
    pub map_type: String,
    /// 目标 id：MATERIAL=物料 id / WAREHOUSE=仓库 id / MATERIAL_TYPE 与 DEFAULT 必须为空。
    pub ref_id: Option<String>,
    /// 目标编码：MATERIAL_TYPE=物料类型编码（必填）；MATERIAL/WAREHOUSE=展示用编码（可选）。
    pub ref_code: Option<String>,
    /// 预算科目 id。
    pub account_id: String,
    /// 部门覆盖（空 = 全公司）。
    pub dept_id: Option<String>,
    /// 优先级。
    pub priority: Option<i32>,
    /// 状态（0/1，默认 1）。
    pub status: Option<i16>,
    /// 备注。
    pub remark: Option<String>,
}

/// 映射入参校验（纯函数）。
///
/// 目标语义见 [`MappingReq`] 的表格 —— 一句话：**有 id 的类型（MATERIAL / WAREHOUSE）要
/// `ref_id`；没有 id 的类型（MATERIAL_TYPE，物料类型是字符串枚举）要 `ref_code`**；
/// `DEFAULT` 两个都不要。这条口径与 DDL 的 `ck_biz_bsm_ref`（V0240 三档）逐字对应，
/// 应用层先拦成 400（带可读文案），库级 CHECK 是最后一道闸。
///
/// 库级唯一性：`uk_biz_bsm_target (map_type, COALESCE(ref_id::text, upper(btrim(ref_code))),
/// COALESCE(dept_id, 0-uuid))`，所以 `MATERIAL_TYPE` 不写 `ref_id` 时唯一性落在
/// `ref_code` 上（大小写不敏感）。撞键由 handler 翻成 409。
pub fn validate_mapping(req: &MappingReq) -> std::result::Result<(), ApiError> {
    let map_type = req.map_type.trim();
    if !matches!(
        map_type,
        "MATERIAL" | "MATERIAL_TYPE" | "WAREHOUSE" | "DEFAULT"
    ) {
        return Err(invalid(format!(
            "map_type={} 非法（只能是 MATERIAL / MATERIAL_TYPE / WAREHOUSE / DEFAULT）",
            req.map_type
        )));
    }
    let ref_id = trimmed(req.ref_id.as_deref());
    let ref_code = trimmed(req.ref_code.as_deref());
    match map_type {
        "DEFAULT" => {
            if ref_id.is_some() {
                return Err(invalid(
                    "DEFAULT 映射不许给 ref_id（它是全公司兜底，不指向任何目标）",
                ));
            }
            if ref_code.is_some() {
                return Err(invalid(
                    "DEFAULT 映射不许给 ref_code（全公司兜底不挂任何目标编码；同一部门只能有一条）",
                ));
            }
        }
        "MATERIAL_TYPE" => {
            if ref_id.is_some() {
                return Err(invalid(
                    "MATERIAL_TYPE 映射不许给 ref_id：物料类型是 biz_material.material_type 上的\
                     编码枚举（RAW / SEMI / …），没有独立 id；请用 ref_code 指定类型编码",
                ));
            }
            let Some(code) = ref_code else {
                return Err(invalid(format!(
                    "MATERIAL_TYPE 映射必须给 ref_code（物料类型编码，只能是 {}）",
                    MATERIAL_TYPES.join(" / ")
                )));
            };
            let upper = code.to_ascii_uppercase();
            if !MATERIAL_TYPES.contains(&upper.as_str()) {
                return Err(invalid(format!(
                    "ref_code={code} 非法（物料类型只能是 {}）",
                    MATERIAL_TYPES.join(" / ")
                )));
            }
        }
        _ => {
            // MATERIAL / WAREHOUSE：必须有真实目标 id；ref_code 只是展示/对账用，可空。
            if ref_id.is_none() {
                let target = if map_type == "MATERIAL" {
                    "物料 id"
                } else {
                    "仓库 id"
                };
                return Err(invalid(format!("{map_type} 映射必须给 ref_id（{target}）")));
            }
        }
    }
    if let Some(status) = req.status
        && !matches!(status, 0 | 1)
    {
        return Err(invalid(format!(
            "status={status} 非法（只能是 0 停用 / 1 启用）"
        )));
    }
    Ok(())
}

/// 去空白后为空的字符串统一当 `None`（前端 `''` 与 `null` 语义相同）。
fn trimmed(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|value| !value.is_empty())
}

/// 把 `ref_code` 归一化成**入库形态**。
///
/// * `MATERIAL_TYPE`：去空白 + 转大写 —— 与 [`MATERIAL_TYPES`] 值域、以及唯一索引里的
///   `upper(btrim(ref_code))` 同一口径，配置侧填 `raw` 落库就是 `RAW`；
/// * 其余类型（`MATERIAL` / `WAREHOUSE` / `DEFAULT`）：只去空白 —— 物料/仓库编码的大小写
///   由主数据决定，映射表不改写它。
pub fn normalize_ref_code(map_type: &str, ref_code: Option<&str>) -> Option<String> {
    let code = trimmed(ref_code)?;
    if map_type.trim() == "MATERIAL_TYPE" {
        Some(code.to_ascii_uppercase())
    } else {
        Some(code.to_string())
    }
}

/// 把「请求里的目标对」归一化成**可以入库的目标对**。
///
/// `MATERIAL_TYPE` / `DEFAULT` 的 `ref_id` 一律丢成 `None`（它们没有目标 id），
/// `ref_code` 按 [`normalize_ref_code`] 归一化。写路径只走这一处，
/// 校验通过后就不需要调用方再各判一次 map_type。
pub fn normalize_target(
    map_type: &str,
    ref_id: Option<Uuid>,
    ref_code: Option<&str>,
) -> (Option<Uuid>, Option<String>) {
    let kind = map_type.trim();
    let ref_id = if matches!(kind, "MATERIAL_TYPE" | "DEFAULT") {
        None
    } else {
        ref_id
    };
    (ref_id, normalize_ref_code(kind, ref_code))
}

/// 预算判定的对外视图（详情页 / 提交前预估都能用）。
#[derive(Debug, Clone, Serialize)]
pub struct VerdictRes {
    /// 模式。
    pub mode: String,
    /// 年度额度。
    pub year_limit: Decimal,
    /// 年度已用。
    pub year_used: Decimal,
    /// 年度可用。
    pub year_available: Decimal,
    /// 月度额度。
    pub month_limit: Decimal,
    /// 月度已用。
    pub month_used: Decimal,
    /// 月度可用。
    pub month_available: Decimal,
    /// 本次金额。
    pub this_amount: Decimal,
    /// 是否超预算。
    pub over: bool,
    /// 超支额。
    pub over_amount: Decimal,
    /// 是否缺预算。
    pub missing_budget: bool,
    /// 中文结论。
    pub message: String,
}

impl From<&BudgetVerdict> for VerdictRes {
    fn from(v: &BudgetVerdict) -> Self {
        Self {
            mode: v.mode.as_str().to_string(),
            year_limit: v.year_limit,
            year_used: v.year_used,
            year_available: v.year_available(),
            month_limit: v.month_limit,
            month_used: v.month_used,
            month_available: v.month_available(),
            this_amount: v.this_amount,
            over: v.over,
            over_amount: v.over_amount(),
            missing_budget: v.missing_budget,
            message: v.describe(),
        }
    }
}

/// 页面 / 详情用的 `Uuid` 文本解析。
pub fn parse_uuid_text(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| invalid(format!("{field} 不是合法的 id：{raw}")))
}

/// 可空的 `Uuid` 文本解析（空串 → `None`）。
pub fn parse_optional_uuid(
    raw: Option<&str>,
    field: &str,
) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid_text(text, field)?)),
    }
}

#[cfg(test)]
#[path = "budget_dto_tests.rs"]
mod budget_dto_tests;

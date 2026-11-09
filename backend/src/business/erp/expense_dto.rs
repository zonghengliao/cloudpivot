//! 费用报销接口契约（ARCH-6 第四章）：费用类型 / 报销单 / 明细 / 付款 / 作废。
//!
//! 与 `asset_dto` / `budget_dto` 同一套约定：主键与外键一律 `String`
//! （`Uuid` 没开 serde）、日期 `NaiveDate`、金额 `Decimal`（JSON 里是字符串，定点四位）、
//! 枚举一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 校验分工
//!
//! * **只依赖请求本身**的规则在本文件（纯函数、可单测）：必填 / 明细 ≥1 /
//!   含税金额 > 0 / 税率区间 / 期间格式 / 作废原因长度 / 付款金额 > 0；
//! * **依赖库**的规则在 handler：费用类型存在且启用、单笔上限、
//!   必填发票号、报销人 / 部门、期间可写、预算守卫；
//! * **跨表恒等式**（Σ 明细 = 单头）由 `expense_compute::Summary::check()` +
//!   写库前复算保证（库侧三道 CHECK 只保证表内自洽）。
//!
//! # 为什么入参都用 `Option`
//!
//! 「必填缺」必须回 **400 + 指名缺哪个字段**，而不是 axum 的 422 裸报文
//! （前端拿不到字段名，只能猜）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::result::Result;

use crate::business::erp::expense_compute::{self, Split, Tier};
use crate::shared::error_code;

/// 默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;
/// 每页条数硬上限。
pub const MAX_PAGE_SIZE: u64 = 100;
/// 事由长度上限（`biz_expense_report.title VARCHAR(200)`）。
pub const MAX_TITLE_LEN: usize = 200;
/// 行摘要长度上限（`biz_expense_item.summary VARCHAR(255)`）。
pub const MAX_SUMMARY_LEN: usize = 255;
/// 作废原因最短字数（ARCH-6 §4.3.4 内控口径：原因要能追溯到为什么）。
pub const MIN_CANCEL_REASON_LEN: usize = 10;

/// 报销单状态：草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 报销单状态：审批中。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 报销单状态：已审批（已入账）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 报销单状态：已付清。
pub const STATUS_PAID: &str = "PAID";
/// 报销单状态：已作废。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 付款状态：未付。
pub const PAY_UNPAID: &str = "UNPAID";
/// 付款状态：部分付款。
pub const PAY_PARTIAL: &str = "PARTIAL";
/// 付款状态：付清。
pub const PAY_PAID: &str = "PAID";

/// 发票类型：增值税专用发票。
pub const INVOICE_VAT_SPECIAL: &str = "VAT_SPECIAL";
/// 发票类型：增值税普通发票。
pub const INVOICE_VAT_NORMAL: &str = "VAT_NORMAL";
/// 发票类型：电子发票。
pub const INVOICE_E_INVOICE: &str = "E_INVOICE";
/// 发票类型：无发票（补贴 / 罚款等）。
pub const INVOICE_NONE: &str = "NONE";

/// 发票类型取值域（与 `ck_biz_exp_item_inv` 逐字一致）。
pub const INVOICE_TYPES: [&str; 4] = [INVOICE_VAT_SPECIAL, INVOICE_VAT_NORMAL, INVOICE_E_INVOICE, INVOICE_NONE];

/// 付款方式：银行转账（缺省）。
pub const PAY_METHOD_BANK: &str = "BANK_TRANSFER";
/// 付款方式：现金。
pub const PAY_METHOD_CASH: &str = "CASH";
/// 付款方式：冲抵（员工欠款对冲）。
pub const PAY_METHOD_OFFSET: &str = "OFFSET";

/// 付款方式取值域（只进 `biz_expense_payment.pay_method`，DDL 不设 CHECK，应用层收口）。
pub const PAY_METHODS: [&str; 3] = [PAY_METHOD_BANK, PAY_METHOD_CASH, PAY_METHOD_OFFSET];

/// 状态中文名（前端 / 报文共用；未知值原样返回，便于暴露脏数据）。
pub fn status_label(status: &str) -> &str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "审批中",
        STATUS_APPROVED => "已审批（待付款）",
        STATUS_PAID => "已付清",
        STATUS_CANCELLED => "已作废",
        other => other,
    }
}

/// 付款状态中文名。
pub fn pay_status_label(status: &str) -> &str {
    match status {
        PAY_UNPAID => "未付款",
        PAY_PARTIAL => "部分付款",
        PAY_PAID => "已付清",
        other => other,
    }
}

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

/// DTO 里的 id 是 `String`，落库前必须显式解析（非法 UUID → 400，不静默跳过条件）。
pub fn parse_uuid_text(raw: &str, field: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|err| invalid(format!("{field} 不是合法 UUID（{raw}）：{err}")))
}

/// 可选文本：去空白后为空 → `None`。
fn trimmed(value: Option<&String>, field: &str, max: usize) -> Result<Option<String>, ApiError> {
    let Some(text) = value.map(|text| text.trim()).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if text.chars().count() > max {
        return Err(invalid(format!(
            "{field} 长度 {} 超过上限 {max} 个字符",
            text.chars().count()
        )));
    }
    Ok(Some(text.to_string()))
}

/// 必填文本。
fn required(value: Option<&String>, field: &str, max: usize) -> Result<String, ApiError> {
    let text = trimmed(value, field, max)?.ok_or_else(|| invalid(format!("{field} 必填（不能为空）")))?;
    Ok(text)
}

// ==================== ① 费用类型 ====================

/// 费用类型分页筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TypeFilter {
    /// 关键字（编码 / 名称）。
    pub keyword: Option<String>,
    /// 状态：1 启用 / 0 停用。
    pub status: Option<i16>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 新建 / 编辑费用类型入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TypeReq {
    /// 编码（`VARCHAR(40)`，未删除内唯一）。
    ///
    /// **卡 CODE-04**：留空（`null` / `""` / 纯空白）⇒ 新建时后端取号 **`ET-01` 形态**
    /// （`biz_seq` 的 ET 号段：纯流水 2 位、不计日期、永不重置；撞号自动顺延，
    /// 超过 2 位按实际位数输出，如 `ET-100`）；有值 ⇒ **原样保留**（不转大写），
    /// 只校验 ≤ 40 + 字符集（字母 / 数字 / `-` `_` `.`），不强制 `ET-xx` 形态。
    pub type_code: Option<String>,
    /// 名称。
    pub type_name: Option<String>,
    /// 借方费用科目 id（必须末级）。
    pub account_id: Option<String>,
    /// 进项税是否可抵扣：`Some(true)` 可 / `Some(false)` 不可。
    pub tax_deductible: Option<bool>,
    /// 是否受预算控制。
    pub budget_controlled: Option<bool>,
    /// 是否必填发票号。
    pub need_invoice_no: Option<bool>,
    /// 单笔金额上限（0 = 不限）。
    pub amount_limit: Option<Decimal>,
    /// 排序。
    pub sort_order: Option<i32>,
    /// 状态：1 启用 / 0 停用。
    pub status: Option<i16>,
    /// 备注。
    pub remark: Option<String>,
}

/// 校验后的费用类型（纯数据，库相关校验在 handler）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSpec {
    /// 编码。
    ///
    /// **空串 = 用户没填**：新建时由 handler 取号（`ET-01` 形态）；修改时编码不可改（不落库）。
    pub type_code: String,
    /// 名称。
    pub type_name: String,
    /// 借方科目 id。
    pub account_id: Uuid,
    /// 可抵扣。
    pub tax_deductible: bool,
    /// 受预算。
    pub budget_controlled: bool,
    /// 必填发票号。
    pub need_invoice_no: bool,
    /// 单笔上限。
    pub amount_limit: Decimal,
    /// 排序。
    pub sort_order: i32,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
}

/// 校验费用类型入参。`creating = true` 时名称 / 科目必填；**编码可以留空**
/// （卡 CODE-04：留空则由 handler 取号 `ET-01` 形态）。
pub fn validate_type(req: &TypeReq, creating: bool) -> Result<TypeSpec, ApiError> {
    // 编码（卡 CODE-04）：
    // * 留空（`null` / `""` / 纯空白）⇒ 空串，新建时由 handler 取号（`ET-01` 形态）；
    // * 有值 ⇒ **原样保留**（不转大写，与 SUP/CUS/MAT/WH/AC 同一口径），
    //   只校验长度 ≤ 40 + 字符集（字母 / 数字 / `-` `_` `.`），
    //   **不强制 `ET-xx` 形态**（企业已有编码体系要能对接）。
    let type_code = crate::business::erp::seq::resolve_manual_code(
        req.type_code.as_deref(),
        crate::business::erp::seq::MAX_MANUAL_CODE_LEN,
    )
    .map_err(|reason| invalid(format!("费用类型编码不合法：{reason}")))?
    // 大小写原样保留（E2E08-FIX-2 ①）：六类主数据统一「用户填什么就存什么」。
    // 取号器产出的 ET-01 本来是大写，不受影响。
    .unwrap_or_default();
    let type_name = match creating {
        true => required(req.type_name.as_ref(), "费用类型名称", 120)?,
        false => trimmed(req.type_name.as_ref(), "费用类型名称", 120)?.unwrap_or_default(),
    };
    let account_id = match req.account_id.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => parse_uuid_text(text, "借方科目 account_id")?,
        _ if creating => return Err(invalid("借方费用科目 account_id 必填（末级科目）")),
        _ => Uuid::nil(),
    };
    let amount_limit = req.amount_limit.unwrap_or(Decimal::ZERO);
    if amount_limit < Decimal::ZERO {
        return Err(invalid(format!(
            "单笔金额上限不能为负（收到 {amount_limit}；0 表示不限）"
        )));
    }
    let status = req.status.unwrap_or(1);
    if !matches!(status, 0 | 1) {
        return Err(invalid(format!("状态只能是 1（启用）或 0（停用），收到 {status}")));
    }
    Ok(TypeSpec {
        type_code,
        type_name,
        account_id,
        tax_deductible: req.tax_deductible.unwrap_or(true),
        budget_controlled: req.budget_controlled.unwrap_or(true),
        need_invoice_no: req.need_invoice_no.unwrap_or(true),
        amount_limit: expense_compute::money(amount_limit),
        sort_order: req.sort_order.unwrap_or(0),
        status,
        remark: trimmed(req.remark.as_ref(), "备注", 2000)?,
    })
}

/// 费用类型出参。
#[derive(Debug, Clone, Serialize)]
pub struct TypeRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub type_code: String,
    /// 名称。
    pub type_name: String,
    /// 借方科目 id。
    pub account_id: String,
    /// 借方科目编码。
    pub account_code: String,
    /// 借方科目名称。
    pub account_name: String,
    /// 可抵扣。
    pub tax_deductible: bool,
    /// 受预算控制。
    pub budget_controlled: bool,
    /// 必填发票号。
    pub need_invoice_no: bool,
    /// 单笔上限（0 = 不限）。
    pub amount_limit: Decimal,
    /// 排序。
    pub sort_order: i32,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
}

// ==================== ② 报销单与明细 ====================

/// 一条明细的入参（**含税录入**，服务端拆价税）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ItemReq {
    /// 费用发生日期（缺省取单头 `expense_date`）。
    pub expense_date: Option<NaiveDate>,
    /// 费用类型 id。
    pub expense_type_id: Option<String>,
    /// **含税金额**（前端「含税录入 → 自动拆」，ARCH-6 §4.10 细节 1）。
    pub with_tax_amount: Option<Decimal>,
    /// 税率（0~1；缺省 0）。
    pub tax_rate: Option<Decimal>,
    /// 发票号（类型要求必填时不能空）。
    pub invoice_no: Option<String>,
    /// 开票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 发票类型：VAT_SPECIAL / VAT_NORMAL / E_INVOICE / NONE。
    pub invoice_type: Option<String>,
    /// 行摘要（必填）。
    pub summary: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 校验后的明细（价税已拆、类型快照待 handler 从库里取）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemSpec {
    /// 费用发生日期。
    pub expense_date: NaiveDate,
    /// 费用类型 id。
    pub expense_type_id: Uuid,
    /// 含税金额。
    pub with_tax: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 开票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 发票类型。
    pub invoice_type: Option<String>,
    /// 行摘要。
    pub summary: String,
    /// 行备注。
    pub remark: Option<String>,
}

impl ItemSpec {
    /// 按「类型是否可抵扣」拆分价税（`deductible` 来自 `biz_expense_type.tax_deductible`）。
    pub fn split(&self, deductible: bool) -> Split {
        expense_compute::split(self.with_tax, self.tax_rate, deductible)
    }
}

/// 新建 / 编辑报销单入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReportReq {
    /// 事由（列表页显示 + 凭证摘要）。
    pub title: Option<String>,
    /// 费用发生日（缺省取明细最早的那天）。
    pub expense_date: Option<NaiveDate>,
    /// 报销人（缺省 = 当前登录用户；财务代填时可指定）。
    pub reporter_id: Option<String>,
    /// 归属部门（缺省 = 报销人当前部门；预算维度）。
    pub dept_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细（≥1 条）。
    pub items: Vec<ItemReq>,
}

/// 校验后的报销单头（明细已校验）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSpec {
    /// 事由。
    pub title: String,
    /// 费用发生日。
    pub expense_date: NaiveDate,
    /// 报销人（`None` = 当前用户）。
    pub reporter_id: Option<Uuid>,
    /// 部门（`None` = 报销人当前部门）。
    pub dept_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 明细。
    pub items: Vec<ItemSpec>,
}

/// 校验报销单入参（**明细 ≥1、每条含税 > 0、税率 0~1、摘要必填**）。
///
/// `today` 只用于「费用发生日不能晚于今天」这条常识性校验（可关：
/// 预提 / 跨月补录允许未来日期的业务版本改这里即可）。
pub fn validate_report(req: &ReportReq) -> Result<ReportSpec, ApiError> {
    let title = required(req.title.as_ref(), "事由 title", MAX_TITLE_LEN)?;
    if req.items.is_empty() {
        return Err(invalid("报销单至少要一条明细（没有明细的报销单在会计上无意义）"));
    }
    let mut items = Vec::with_capacity(req.items.len());
    for (index, item) in req.items.iter().enumerate() {
        let no = index + 1;
        let Some(type_id) = item.expense_type_id.as_deref().map(str::trim) else {
            return Err(invalid(format!("第 {no} 行明细缺费用类型 expense_type_id")));
        };
        let expense_type_id = parse_uuid_text(type_id, &format!("第 {no} 行 expense_type_id"))?;
        let with_tax = item
            .with_tax_amount
            .ok_or_else(|| invalid(format!("第 {no} 行明细缺含税金额 with_tax_amount")))?;
        let with_tax = expense_compute::money(with_tax);
        if with_tax <= Decimal::ZERO {
            return Err(invalid(format!(
                "第 {no} 行明细的含税金额必须大于 0（收到 {with_tax}）：\
                 负数或零金额明细会让「明细合计 = 单头总额」这道恒等式失去意义"
            )));
        }
        let tax_rate = item.tax_rate.unwrap_or(Decimal::ZERO);
        if tax_rate < Decimal::ZERO || tax_rate > Decimal::ONE {
            return Err(invalid(format!(
                "第 {no} 行明细的税率必须在 0~1 之间（收到 {tax_rate}；6% 请填 0.06）"
            )));
        }
        let invoice_type = match item.invoice_type.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => {
                let upper = raw.to_ascii_uppercase();
                if !INVOICE_TYPES.contains(&upper.as_str()) {
                    return Err(invalid(format!(
                        "第 {no} 行明细的发票类型「{raw}」不合法，只能是 {}",
                        INVOICE_TYPES.join(" / ")
                    )));
                }
                Some(upper)
            }
        };
        let summary = required(
            item.summary.as_ref(),
            &format!("第 {no} 行明细摘要 summary"),
            MAX_SUMMARY_LEN,
        )?;
        items.push(ItemSpec {
            expense_date: item
                .expense_date
                .unwrap_or_else(|| req.expense_date.unwrap_or_else(|| Utc::now().date_naive())),
            expense_type_id,
            with_tax,
            tax_rate,
            invoice_no: trimmed(item.invoice_no.as_ref(), &format!("第 {no} 行发票号"), 50)?,
            invoice_date: item.invoice_date,
            invoice_type,
            summary,
            remark: trimmed(item.remark.as_ref(), &format!("第 {no} 行备注"), 255)?,
        });
    }
    let expense_date = req
        .expense_date
        .or_else(|| items.iter().map(|item| item.expense_date).min())
        .ok_or_else(|| invalid("费用发生日 expense_date 必填（或用明细里的最早日期）"))?;
    let reporter_id = match req.reporter_id.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Some(parse_uuid_text(text, "报销人 reporter_id")?),
        _ => None,
    };
    let dept_id = match req.dept_id.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => Some(parse_uuid_text(text, "部门 dept_id")?),
        _ => None,
    };
    Ok(ReportSpec {
        title,
        expense_date,
        reporter_id,
        dept_id,
        remark: trimmed(req.remark.as_ref(), "备注", 2000)?,
        items,
    })
}

/// 报销单筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReportFilter {
    /// 关键字（单号 / 事由 / 报销人 / 部门）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 付款状态。
    pub pay_status: Option<String>,
    /// 报销人（「我的报销」传自己）。
    pub reporter_id: Option<String>,
    /// 部门。
    pub dept_id: Option<String>,
    /// 会计期间（`yyyy-MM`）。
    pub period_code: Option<String>,
    /// 费用发生日起。
    pub date_from: Option<NaiveDate>,
    /// 费用发生日止。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
}

/// 明细出参。
#[derive(Debug, Clone, Serialize)]
pub struct ItemRes {
    /// 主键。
    pub id: String,
    /// 行号。
    pub line_no: i32,
    /// 费用发生日。
    pub expense_date: NaiveDate,
    /// 费用类型 id。
    pub expense_type_id: String,
    /// 类型编码快照。
    pub type_code: String,
    /// 类型名快照。
    pub type_name: String,
    /// 借方科目 id 快照。
    pub account_id: String,
    /// 借方科目编码快照。
    pub account_code: String,
    /// 借方科目名快照。
    pub account_name: String,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税率。
    pub tax_rate: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 开票日期。
    pub invoice_date: Option<NaiveDate>,
    /// 发票类型。
    pub invoice_type: Option<String>,
    /// 行摘要。
    pub summary: String,
    /// 行备注。
    pub remark: Option<String>,
}

/// 付款记录出参。
#[derive(Debug, Clone, Serialize)]
pub struct PaymentRes {
    /// 主键。
    pub id: String,
    /// 付款单号。
    pub payment_no: String,
    /// 金额。
    pub amount: Decimal,
    /// 付款日期。
    pub pay_date: NaiveDate,
    /// 付款方式。
    pub pay_method: Option<String>,
    /// 付款账户科目编码。
    pub pay_account_code: Option<String>,
    /// 凭证 id。
    pub voucher_id: Option<String>,
    /// 凭证号。
    pub voucher_no: Option<String>,
    /// 会计期间。
    pub period_code: String,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
}

/// 预算占用出参（详情页「预算占用」区块，ARCH-6 §4.10 细节 4）。
#[derive(Debug, Clone, Serialize)]
pub struct UsageRes {
    /// 占用 id。
    pub id: String,
    /// 部门 id。
    pub dept_id: String,
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 会计年度。
    pub fiscal_year: i16,
    /// 期间。
    pub period_code: String,
    /// 占用金额（含税）。
    pub amount: Decimal,
    /// 状态。
    pub status: String,
    /// 超预算标记。
    pub over_budget: bool,
    /// 超支额（`null` = 提交时没超支，含「未编制预算」）。
    pub over_amount: Option<Decimal>,
    /// 释放原因。
    pub release_reason: Option<String>,
}

/// 报销单列表行 / 卡片。
#[derive(Debug, Clone, Serialize)]
pub struct ReportRes {
    /// 主键。
    pub id: String,
    /// 单号。
    pub report_no: String,
    /// 报销人 id。
    pub reporter_id: String,
    /// 报销人姓名。
    pub reporter_name: String,
    /// 部门 id。
    pub dept_id: String,
    /// 部门名。
    pub dept_name: String,
    /// 事由。
    pub title: String,
    /// 费用发生日。
    pub expense_date: NaiveDate,
    /// 会计期间（**记账期间**：审批通过时按审批日所属月份重算并回写；未入账时是建单期间）。
    pub period_code: String,
    /// 费用归属期间（`period_of(expense_date)`；**预算占用按这个期间归集**，E2E06-FIX-3 ②）。
    ///
    /// 与 [`Self::accounting_period_code`] 可能不同：跨期报销时（例如 2027-02 的费用在
    /// 2026-11 审批）预算占用记费用期间、凭证记审批期间 —— 这是设计口径（ARCH-6 §4.5.4），
    /// 两个值都回给前端，界面上必须同时显示，否则用户看不懂「为什么凭证在 11 月、占用在 2 月」。
    pub expense_period_code: String,
    /// 记账期间（凭证期间）；`None` = 尚未入账（没有计提凭证），前端显示「未记账」。
    pub accounting_period_code: Option<String>,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计。
    pub total_with_tax: Decimal,
    /// 明细条数。
    pub item_count: i32,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 付款状态。
    pub pay_status: String,
    /// 付款状态中文。
    pub pay_status_label: String,
    /// 已付金额。
    pub paid_amount: Decimal,
    /// 未付金额。
    pub unpaid_amount: Decimal,
    /// 最近付款日。
    pub last_pay_date: Option<NaiveDate>,
    /// 超预算标记。
    pub over_budget: bool,
    /// 未编制预算标记（该槽没有本期生效的预算；与 `over_budget` 互斥，FIX-5）。
    pub missing_budget: bool,
    /// 提交时点的超支额（`null` = 没超支）。
    pub over_amount: Option<Decimal>,
    /// 提交时**实际生效的槽级控制策略**快照（`OFF` / `WARN` / `BLOCK`；`null` = 从未提交过）。
    ///
    /// FIX-2 问题 4：审计要能看出「这单被哪条策略管过」——槽级 BLOCK、全局 WARN 时必须记 BLOCK。
    pub budget_mode: Option<String>,
    /// 审批实例 id。
    pub approval_instance_id: Option<String>,
    /// 计提凭证 id。
    pub voucher_id: Option<String>,
    /// 计提凭证号。
    pub voucher_no: Option<String>,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 审批通过时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

/// 报销单详情。
#[derive(Debug, Clone, Serialize)]
pub struct ReportDetailRes {
    /// 单头。
    pub report: ReportRes,
    /// 明细。
    pub items: Vec<ItemRes>,
    /// 付款记录。
    pub payments: Vec<PaymentRes>,
    /// 预算占用。
    pub budget_usages: Vec<UsageRes>,
    /// 预算尚未生效的提示（FIX-2：DRAFT / 审批中 / 未到生效日），前端标黄、不阻断。
    pub budget_warnings: Vec<crate::business::erp::budget_dto::BudgetWarningRes>,
    /// 金额分级（当前档位中文，便于详情页提示「还要几道审批」）。
    pub tier_label: String,
}

/// 新建 / 编辑成功后回详情（单一入口，前端不用再拉一次）。
#[derive(Debug, Clone, Serialize)]
pub struct IdRes {
    /// 主键。
    pub id: String,
}

/// 付款登记入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PayReq {
    /// 本次付款金额（> 0，且 ≤ 未付金额）。
    pub amount: Option<Decimal>,
    /// 付款日期（缺省今天）。
    pub pay_date: Option<NaiveDate>,
    /// 付款方式（BANK_TRANSFER / CASH / OFFSET，缺省银行转账）。
    pub pay_method: Option<String>,
    /// 付款账户科目编码（缺省 1002 银行存款）。
    pub pay_account_code: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 校验后的付款单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaySpec {
    /// 金额。
    pub amount: Decimal,
    /// 付款日期。
    pub pay_date: NaiveDate,
    /// 付款方式。
    pub pay_method: String,
    /// 付款账户科目编码。
    pub pay_account_code: String,
    /// 备注。
    pub remark: Option<String>,
}

/// 校验付款入参。
pub fn validate_pay(req: &PayReq, today: NaiveDate) -> Result<PaySpec, ApiError> {
    let amount = req.amount.ok_or_else(|| invalid("付款金额 amount 必填"))?;
    let amount = expense_compute::money(amount);
    if amount <= Decimal::ZERO {
        return Err(invalid(format!(
            "付款金额必须大于 0（收到 {amount}）：冲销已有付款请走「作废」整单红冲"
        )));
    }
    let pay_method = match req.pay_method.as_deref().map(str::trim) {
        None | Some("") => PAY_METHOD_BANK.to_string(),
        Some(raw) => {
            let upper = raw.to_ascii_uppercase();
            if !PAY_METHODS.contains(&upper.as_str()) {
                return Err(invalid(format!(
                    "付款方式「{raw}」不合法，只能是 {}",
                    PAY_METHODS.join(" / ")
                )));
            }
            upper
        }
    };
    let pay_account_code = match req.pay_account_code.as_deref().map(str::trim) {
        None | Some("") => "1002".to_string(),
        Some(raw) => raw.to_string(),
    };
    Ok(PaySpec {
        amount,
        pay_date: req.pay_date.unwrap_or(today),
        pay_method,
        pay_account_code,
        remark: trimmed(req.remark.as_ref(), "备注", 2000)?,
    })
}

/// 作废入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CancelReq {
    /// 作废原因（≥ [`MIN_CANCEL_REASON_LEN`] 个字）。
    pub reason: Option<String>,
}

/// 校验作废原因。
pub fn validate_cancel(req: &CancelReq) -> Result<String, ApiError> {
    let reason = required(req.reason.as_ref(), "作废原因 reason", 255)?;
    if reason.chars().count() < MIN_CANCEL_REASON_LEN {
        return Err(invalid(format!(
            "作废原因至少 {MIN_CANCEL_REASON_LEN} 个字（收到 {} 个字）：\
             作废会红冲已入账凭证，原因要能追溯到为什么",
            reason.chars().count()
        )));
    }
    Ok(reason)
}

/// 详情页用的分级文案（与流程种子的条件边同口径：1000 / 10000）。
pub fn tier_label(total_with_tax: Decimal) -> &'static str {
    let tier: Tier = expense_compute::tier_of(total_with_tax);
    tier.label()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量非法")
    }

    fn item(with_tax: &str, rate: &str, summary: &str) -> ItemReq {
        ItemReq {
            expense_date: NaiveDate::from_ymd_opt(2026, 11, 3),
            expense_type_id: Some("20000000-0000-4000-8000-000000000901".to_string()),
            with_tax_amount: Some(dec(with_tax)),
            tax_rate: Some(dec(rate)),
            invoice_no: Some("INV-001".to_string()),
            invoice_date: None,
            invoice_type: Some("VAT_SPECIAL".to_string()),
            summary: Some(summary.to_string()),
            remark: None,
        }
    }

    /// ① 正常单：3 条明细通过校验，费用发生日取单头。
    #[test]
    fn valid_report_passes() {
        let req = ReportReq {
            title: Some("10 月上海客户拜访".to_string()),
            expense_date: Some(NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()),
            reporter_id: None,
            dept_id: None,
            remark: None,
            items: vec![
                item("3180", "0.06", "高铁 + 机票"),
                item("2120", "0.06", "住宿"),
                item("240", "0.13", "打印耗材"),
            ],
        };
        let spec = validate_report(&req).expect("合法单");
        assert_eq!(spec.items.len(), 3);
        assert_eq!(spec.title, "10 月上海客户拜访");
    }

    /// ② 明细为空 → 400（「ERP 大忌」的第一道闸）。
    #[test]
    fn empty_items_rejected() {
        let req = ReportReq {
            title: Some("空单".to_string()),
            ..Default::default()
        };
        let err = validate_report(&req).expect_err("空明细必须被拒");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("至少要一条明细"));
    }

    /// ③ 金额为负 → 400，且报文点名第几行。
    #[test]
    fn negative_amount_rejected() {
        let req = ReportReq {
            title: Some("负数".to_string()),
            items: vec![item("-1", "0.06", "冲减")],
            ..Default::default()
        };
        let err = validate_report(&req).expect_err("负金额必须被拒");
        assert!(err.msg.contains("第 1 行"), "报文要点名行号：{}", err.msg);
    }

    /// ④ 税率越界 → 400（前端常见错误：填 6 而不是 0.06）。
    #[test]
    fn tax_rate_out_of_range_rejected() {
        let req = ReportReq {
            title: Some("税率".to_string()),
            items: vec![item("100", "6", "把 6% 填成 6")],
            ..Default::default()
        };
        let err = validate_report(&req).expect_err("税率越界必须被拒");
        assert!(err.msg.contains("0~1"), "{}", err.msg);
    }

    /// ⑤ 发票类型取值域（与 DDL CHECK 逐字一致）。
    #[test]
    fn invoice_type_domain_enforced() {
        let mut bad = item("100", "0.06", "x");
        bad.invoice_type = Some("VAT".to_string());
        let req = ReportReq {
            title: Some("发票".to_string()),
            items: vec![bad],
            ..Default::default()
        };
        assert!(validate_report(&req).is_err());

        let mut good = item("100", "0.06", "x");
        good.invoice_type = Some("e_invoice".to_string());
        let req = ReportReq {
            title: Some("发票".to_string()),
            items: vec![good],
            ..Default::default()
        };
        assert_eq!(
            validate_report(&req).unwrap().items[0].invoice_type.as_deref(),
            Some(INVOICE_E_INVOICE),
            "小写要归一成大写"
        );
    }

    /// ⑥ 作废原因不足 10 字 → 400。
    #[test]
    fn cancel_reason_needs_ten_chars() {
        let short = CancelReq {
            reason: Some("开错了".to_string()),
        };
        assert!(validate_cancel(&short).is_err());
        let long = CancelReq {
            reason: Some("报销单填错部门，财务要求整单作废重提".to_string()),
        };
        assert!(validate_cancel(&long).is_ok());
    }

    /// ⑦ 付款金额必须为正，付款方式取值域。
    #[test]
    fn pay_amount_and_method_validated() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let zero = PayReq {
            amount: Some(Decimal::ZERO),
            ..Default::default()
        };
        assert!(validate_pay(&zero, today).is_err());
        let ok = PayReq {
            amount: Some(dec("6600")),
            pay_method: Some("cash".to_string()),
            ..Default::default()
        };
        let spec = validate_pay(&ok, today).expect("合法付款");
        assert_eq!(spec.pay_method, PAY_METHOD_CASH);
        assert_eq!(spec.pay_account_code, "1002", "缺省付款账户是银行存款");
    }

    /// ⑧ 单笔上限为负 → 400。
    #[test]
    fn type_amount_limit_non_negative() {
        let req = TypeReq {
            type_code: Some("travel".to_string()),
            type_name: Some("差旅费".to_string()),
            account_id: Some("20000000-0000-4000-8000-000000000902".to_string()),
            amount_limit: Some(dec("-1")),
            ..Default::default()
        };
        assert!(validate_type(&req, true).is_err());
        let mut ok = req.clone();
        ok.amount_limit = Some(dec("5000"));
        let spec = validate_type(&ok, true).expect("合法类型");
        assert_eq!(spec.type_code, "travel", "编码大小写原样保留（E2E08-FIX-2 ①）");
    }

    /// ⑨ CODE-04：编码留空 = 后端取号（`ET-01` 形态），手工编码保留（含企业自有形态）。
    #[test]
    fn type_code_blank_means_auto_and_manual_is_kept() {
        let base = TypeReq {
            type_code: Some("travel".to_string()),
            type_name: Some("差旅费".to_string()),
            account_id: Some("20000000-0000-4000-8000-000000000902".to_string()),
            ..Default::default()
        };

        // 三种「没填」都放行，且 spec 里是空串（handler 据此取号）
        for blank in [None, Some(String::new()), Some("   ".to_string())] {
            let mut req = base.clone();
            req.type_code = blank.clone();
            assert_eq!(
                validate_type(&req, true)
                    .expect("编码留空不能拒（要自动取号）")
                    .type_code,
                "",
                "留空（{blank:?}）应归一成空串"
            );
        }

        // 取号器产出的形态（ET-01）与手工自有编码（如 ADMIN-FEE）都要能通过
        for manual in ["ET-01", "ADMIN-FEE", "FY_2026.01", "travel-fee"] {
            let mut req = base.clone();
            req.type_code = Some(manual.to_string());
            assert_eq!(
                validate_type(&req, true).expect("合法手工编码").type_code,
                manual,
                "手工编码大小写原样保留（E2E08-FIX-2 ①）"
            );
        }

        // 字符集外的输入仍然要拒（否则 DDL 后才炸，还拿不到字段名）
        let mut bad = base.clone();
        bad.type_code = Some("差旅费".to_string());
        let err = validate_type(&bad, true).expect_err("中文编码必须拒绝");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("费用类型编码"), "文案要点名是编码的问题：{}", err.msg);
    }
}

//! 固定资产接口契约（ARCH-6 第五章）：分类 / 卡片 / 变动 / 处置 / 折旧批次。
//!
//! 与 `budget_dto` / `purchase_dto` 同一套约定：主键与外键一律 `String`
//! （`Uuid` 没开 serde）、日期 `NaiveDate`、金额 `Decimal`（JSON 里是字符串，
//! 定点四位）、枚举一律大写字符串且与 DDL 的 `CHECK` 逐字一致。
//!
//! # 校验分工
//!
//! * **只依赖请求本身**的规则在本文件（纯函数、可单测）：必填 / 金额为正 /
//!   税小于原值 / 残值率区间 / 处置类型取值域 / 出售价与销项税只在 SALE 允许非零
//!   （[`validate_create`]、[`validate_dispose`]、[`validate_category`]）；
//! * 依赖库的规则在 handler：分类 / 部门 / 用户 / 来源单存在、来源单未被转过固、
//!   分类科目是末级、期间是否可写；
//! * 落在 DDL 的规则两边都做：应用层先拦成 400，DB 的 CHECK 是最后一道闸。
//!
//! # 为什么入参都用 `Option`
//!
//! 「必填缺」必须回 **400 + 指名缺哪个字段**，而不是 axum 的 422 裸报文
//! （前端拿不到字段名，只能猜）。所以 DTO 全 `Option`，缺什么由
//! [`validate_create`] 说清楚。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
// `nik::prelude::Result<T>` 是单参数别名（内部固定 `Error`），承载不了 `ApiError`；
// 显式引入标准库的 `Result` 覆盖 glob 导入，本模块的业务函数才能写 `Result<T, ApiError>`。
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::result::Result;

use crate::business::erp::asset_compute::{self, DisposalKind, depreciable_base, monthly_depreciation, residual_value};
use crate::business::erp::asset_model::{self, AssetChangeRow, AssetRow, CategoryRow, DepBatchRow, DepDetailRow};
use crate::shared::error_code;

/// 默认每页条数。
pub const DEFAULT_PAGE_SIZE: u64 = 20;
/// 每页条数硬上限。
pub const MAX_PAGE_SIZE: u64 = 100;
/// 名称长度上限（`asset_name VARCHAR(200)`）。
pub const MAX_NAME_LEN: usize = 200;
/// 变动原因长度上限（`biz_fixed_asset_change.reason VARCHAR(255)`）。
pub const MAX_REASON_LEN: usize = 255;
/// 备注长度上限。
pub const MAX_REMARK_LEN: usize = 2000;

/// 资产状态：在役。
pub const STATUS_IN_USE: &str = "IN_USE";
/// 资产状态：闲置（照提折旧）。
pub const STATUS_IDLE: &str = "IDLE";
/// 资产状态：已处置。
pub const STATUS_DISPOSED: &str = "DISPOSED";
/// 资产状态：未启用即作废（本卡不提供写入入口）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 变动类型：部门转移。
pub const CHANGE_TRANSFER: &str = "TRANSFER";
/// 变动类型：使用人变更。
pub const CHANGE_CUSTODIAN: &str = "CUSTODIAN";
/// 变动类型：处置。
pub const CHANGE_DISPOSAL: &str = "DISPOSAL";
/// 变动类型：启用（含「作废处置后恢复在役」）。
pub const CHANGE_ENABLE: &str = "ENABLE";
/// 变动类型：停用。
pub const CHANGE_IDLE: &str = "IDLE";

/// 购入口径：直接购入（借 1601 / 贷 2202 或 1002）。
pub const BUY_DIRECT: &str = "DIRECT_BUY";
/// 购入口径：由收货单转入（借 1601 / 贷 1405）。
pub const BUY_FROM_STOCK: &str = "FROM_STOCK";

/// 来源：手工录入。
pub const SOURCE_MANUAL: &str = "MANUAL";
/// 来源：采购单。
pub const SOURCE_PURCHASE_ORDER: &str = "PURCHASE_ORDER";
/// 来源：收货单。
pub const SOURCE_STOCK_IN: &str = "STOCK_IN";

/// 付款方式：赊购（贷 2202 应付账款）。
pub const PAY_CREDIT: &str = "CREDIT";
/// 付款方式：现付（贷 1002 银行存款）。
pub const PAY_CASH: &str = "CASH";

/// 折旧批次状态：已计算（草稿，未生成凭证）。
pub const DEP_CALCULATED: &str = "CALCULATED";
/// 折旧批次状态：已过账。
pub const DEP_POSTED: &str = "POSTED";
/// 折旧批次状态：已红冲。
pub const DEP_REVERSED: &str = "REVERSED";
/// 折旧批次状态：已跳过。
pub const DEP_SKIPPED: &str = "SKIPPED";

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

/// 可选 id 解析（空串 / 未传 → `None`）。
pub fn parse_opt_uuid(raw: Option<&String>, field: &str) -> Result<Option<Uuid>, ApiError> {
    match raw.map(|text| text.trim()).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Uuid::parse_str(text)
            .map(Some)
            .map_err(|err| invalid(format!("{field} 不是合法 UUID（{text}）：{err}"))),
    }
}

/// 资产状态中文名。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_IN_USE => "在役",
        STATUS_IDLE => "闲置",
        STATUS_DISPOSED => "已处置",
        STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

/// 变动类型中文名。
pub fn change_label(change_type: &str) -> &'static str {
    match change_type {
        CHANGE_TRANSFER => "部门转移",
        CHANGE_CUSTODIAN => "使用人变更",
        CHANGE_DISPOSAL => "处置",
        CHANGE_ENABLE => "启用",
        CHANGE_IDLE => "停用",
        _ => "未知变动",
    }
}

// ==================== 资产分类 ====================

/// 分类筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CategoryFilter {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 编码 / 名称模糊。
    pub keyword: Option<String>,
    /// 状态（1 启用 / 0 停用）。
    pub status: Option<i16>,
}

/// 新建 / 修改分类。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CategoryReq {
    /// 编码（字母 / 数字 / `-` `_` `.`，≤ 40；大小写原样保留，留空 = 自动取号）。
    pub category_code: Option<String>,
    /// 名称。
    pub category_name: Option<String>,
    /// 使用月数（1..=600）。
    pub useful_life_months: Option<i32>,
    /// 净残值率（0 ≤ r < 1）。
    pub salvage_rate: Option<Decimal>,
    /// 原值科目（默认 1601）。
    pub asset_account_id: Option<String>,
    /// 累计折旧科目（默认 1602）。
    pub accum_account_id: Option<String>,
    /// 折旧费用科目（默认 5602.01）。
    pub expense_account_id: Option<String>,
    /// 状态（1 / 0，默认 1）。
    pub status: Option<i16>,
    /// 排序。
    pub sort_order: Option<i32>,
    /// 备注。
    pub remark: Option<String>,
}

/// 校验通过后的分类参数（纯函数产物，测试直接断言它）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategorySpec {
    /// 编码。
    ///
    /// **空串 = 用户没填**：新建时由 handler 取号（`AC-01` 形态），修改时表示「不改编码」。
    pub category_code: String,
    /// 名称。
    pub category_name: String,
    /// 使用月数。
    pub useful_life_months: i32,
    /// 净残值率。
    pub salvage_rate: Decimal,
    /// 原值科目 id（`None` = 用默认 1601）。
    pub asset_account_id: Option<Uuid>,
    /// 累计折旧科目 id（`None` = 用默认 1602）。
    pub accum_account_id: Option<Uuid>,
    /// 折旧费用科目 id（`None` = 用默认 5602.01）。
    pub expense_account_id: Option<Uuid>,
    /// 状态。
    pub status: i16,
    /// 排序。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
}

/// 分类校验（编码格式 / 年限区间 / 残值率区间）。
///
/// # 编码（卡 CODE-04）
///
/// * **留空**（`null` / `""` / 纯空白）⇒ `CategorySpec.category_code` 返回**空串**，
///   由 handler 用 `erp::seq::next_master_code_free(&tx, "AC")` 生成 `AC-01` 形态的编码；
/// * **有值** ⇒ **原样保留**（不转大写，与 SUP/CUS/MAT/WH 四类同一口径），
///   只校验长度 ≤ 40 + 字符集（字母 / 数字 / `-` `_` `.`），
///   **不强制 `AC-xx` 形态**（企业已有编码体系如 `CAT-BUILDING` 要能对接）。
///
/// 两条目的区别只在调用方怎么用这个空串：新建时取号，修改时「不改编码」。
pub fn validate_category(req: &CategoryReq) -> Result<CategorySpec, ApiError> {
    let code = crate::business::erp::seq::resolve_manual_code(
        req.category_code.as_deref(),
        crate::business::erp::seq::MAX_MANUAL_CODE_LEN,
    )
    .map_err(|reason| invalid(format!("分类编码不合法：{reason}")))?
    // 大小写**原样保留**（E2E08-FIX-2 ①）：六类主数据统一「用户填什么就存什么」。
    // 企业自有编码有的系统区分大小写，替用户改大小写等于改他的编码；取号器产出的
    // `AC-01` 本来是大写，不受影响。
    .unwrap_or_default();
    let name = req
        .category_name
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("分类名称必填"))?;
    if name.len() > 120 {
        return Err(invalid("分类名称最长 120 字符"));
    }
    let months = req
        .useful_life_months
        .ok_or_else(|| invalid("使用月数必填（1..=600）"))?;
    if !(1..=600).contains(&months) {
        return Err(invalid(format!(
            "使用月数必须在 1..=600 之间（当前 {months}）：这是「1 个月 ~ 50 年」的库级约束"
        )));
    }
    let salvage_rate = req.salvage_rate.unwrap_or_else(|| Decimal::new(5, 2));
    if salvage_rate < Decimal::ZERO || salvage_rate >= Decimal::ONE {
        return Err(invalid(format!(
            "净残值率必须在 [0, 1) 之间（当前 {salvage_rate}）：0.05 = 5%"
        )));
    }
    let status = req.status.unwrap_or(1);
    if status != 0 && status != 1 {
        return Err(invalid("分类状态只能是 1（启用）或 0（停用）"));
    }
    Ok(CategorySpec {
        category_code: code.to_string(),
        category_name: name.to_string(),
        useful_life_months: months,
        salvage_rate,
        asset_account_id: parse_opt_uuid(req.asset_account_id.as_ref(), "原值科目")?,
        accum_account_id: parse_opt_uuid(req.accum_account_id.as_ref(), "累计折旧科目")?,
        expense_account_id: parse_opt_uuid(req.expense_account_id.as_ref(), "折旧费用科目")?,
        status,
        sort_order: req.sort_order.unwrap_or(0),
        remark: req.remark.clone(),
    })
}

/// 分类响应行。
#[derive(Debug, Clone, Serialize)]
pub struct CategoryRes {
    /// id。
    pub id: String,
    /// 编码。
    pub category_code: String,
    /// 名称。
    pub category_name: String,
    /// 折旧方法（本期恒为 `STRAIGHT_LINE`）。
    pub depreciation_method: String,
    /// 使用月数。
    pub useful_life_months: i32,
    /// 净残值率。
    pub salvage_rate: Decimal,
    /// 原值科目。
    pub asset_account_id: String,
    /// 累计折旧科目。
    pub accum_account_id: String,
    /// 折旧费用科目。
    pub expense_account_id: String,
    /// 状态。
    pub status: i16,
    /// 排序。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
}

impl From<&CategoryRow> for CategoryRes {
    fn from(row: &CategoryRow) -> Self {
        Self {
            id: row.id.to_string(),
            category_code: row.category_code.clone(),
            category_name: row.category_name.clone(),
            depreciation_method: row.depreciation_method.clone(),
            useful_life_months: row.useful_life_months,
            salvage_rate: row.salvage_rate,
            asset_account_id: row.asset_account_id.to_string(),
            accum_account_id: row.accum_account_id.to_string(),
            expense_account_id: row.expense_account_id.to_string(),
            status: row.status,
            sort_order: row.sort_order,
            remark: row.remark.clone(),
        }
    }
}

// ==================== 资产卡片 ====================

/// 卡片筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AssetFilter {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 编号 / 名称 / 规格模糊。
    pub keyword: Option<String>,
    /// 分类。
    pub category_id: Option<String>,
    /// 使用部门。
    pub use_dept_id: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 来源类型。
    pub source_type: Option<String>,
}

/// 新建卡片。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateAssetReq {
    /// 名称。
    pub asset_name: Option<String>,
    /// 分类。
    pub category_id: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 使用部门。
    pub use_dept_id: Option<String>,
    /// 使用人 / 保管人。
    pub custodian_id: Option<String>,
    /// 存放地点。
    pub location: Option<String>,
    /// 原值（可抵扣时**不含税**，不可抵扣时含税 —— ARCH-6 §5.3.1）。
    pub original_value: Option<Decimal>,
    /// 可抵扣进项税（只用于凭证拆分，不减少原值）。
    pub tax_amount: Option<Decimal>,
    /// 购入 / 入账日期。
    pub purchase_date: Option<NaiveDate>,
    /// 来源：MANUAL / PURCHASE_ORDER / STOCK_IN。
    pub source_type: Option<String>,
    /// 来源单 id（非 MANUAL 必填）。
    pub source_id: Option<String>,
    /// 来源单号（快照）。
    pub source_no: Option<String>,
    /// 购入口径：DIRECT_BUY / FROM_STOCK（收货单来源强制 FROM_STOCK）。
    pub buy_mode: Option<String>,
    /// 付款方式：CREDIT（赊购，贷 2202）/ CASH（现付，贷 1002）。
    pub pay_mode: Option<String>,
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 校验通过后的建卡参数（纯函数产物）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAssetSpec {
    /// 名称。
    pub asset_name: String,
    /// 分类 id。
    pub category_id: Uuid,
    /// 规格。
    pub spec: Option<String>,
    /// 使用部门 id。
    pub use_dept_id: Uuid,
    /// 使用人 id。
    pub custodian_id: Option<Uuid>,
    /// 存放地点。
    pub location: Option<String>,
    /// 原值。
    pub original_value: Decimal,
    /// 可抵扣进项税。
    pub tax_amount: Decimal,
    /// 购入日期。
    pub purchase_date: NaiveDate,
    /// 来源类型。
    pub source_type: String,
    /// 来源单 id。
    pub source_id: Option<Uuid>,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 购入口径。
    pub buy_mode: String,
    /// 付款方式。
    pub pay_mode: String,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
}

/// 建卡校验（必填 / 金额为正 / 税小于原值 / 来源一致性 / 购入口径）。
pub fn validate_create(req: &CreateAssetReq) -> Result<CreateAssetSpec, ApiError> {
    let name = req
        .asset_name
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("资产名称必填"))?;
    if name.chars().count() > MAX_NAME_LEN {
        return Err(invalid(format!(
            "资产名称最长 {MAX_NAME_LEN} 字符（当前 {}）",
            name.chars().count()
        )));
    }
    let category_id = req
        .category_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("资产分类必填：折旧年限与残值率都从分类带出"))?;
    let category_id = parse_uuid_text(category_id, "资产分类")?;
    let use_dept_id = req
        .use_dept_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("使用部门必填：折旧费用要按部门核算（5602.01 的 DEPT 辅助）"))?;
    let use_dept_id = parse_uuid_text(use_dept_id, "使用部门")?;
    let original_value = req.original_value.ok_or_else(|| invalid("原值必填"))?;
    if original_value <= Decimal::ZERO {
        return Err(invalid(format!(
            "原值必须大于 0（当前 {original_value}）：负值 / 零值资产不构成固定资产，\
             也过不了库里的 ck_biz_fa_value"
        )));
    }
    let tax_amount = req.tax_amount.unwrap_or(Decimal::ZERO);
    if tax_amount < Decimal::ZERO {
        return Err(invalid(format!("可抵扣进项税不能为负（当前 {tax_amount}）")));
    }
    if tax_amount >= original_value {
        return Err(invalid(format!(
            "可抵扣进项税 {tax_amount} 必须小于原值 {original_value}：\
             原值口径是「可抵扣时不含税」，税额不可能吃掉全部原值"
        )));
    }
    let purchase_date = req
        .purchase_date
        .ok_or_else(|| invalid("购入 / 入账日期必填（它决定开始计提日）"))?;

    let source_type = req
        .source_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(SOURCE_MANUAL)
        .to_string();
    if ![SOURCE_MANUAL, SOURCE_PURCHASE_ORDER, SOURCE_STOCK_IN].contains(&source_type.as_str()) {
        return Err(invalid(format!(
            "来源类型只能是 MANUAL / PURCHASE_ORDER / STOCK_IN（当前 {source_type}）"
        )));
    }
    let source_id = parse_opt_uuid(req.source_id.as_ref(), "来源单")?;
    match (source_type.as_str(), source_id) {
        (SOURCE_MANUAL, Some(_)) => {
            return Err(invalid("来源为 MANUAL 时不能给来源单 id（手工建卡没有来源单）"));
        }
        (SOURCE_MANUAL, None) => {}
        (_, None) => {
            return Err(invalid(format!(
                "来源为 {source_type} 时必须给来源单 id（否则查不到「这台是从哪张单转来的」）"
            )));
        }
        (_, Some(_)) => {}
    }

    // 购入口径：收货单来源**强制** FROM_STOCK（ARCH-6 §5.3.3 —— 收货单已过账意味着
    // 1405 已增加，不冲回就是重复计价）。
    let buy_mode = match req.buy_mode.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        Some(mode) if mode == BUY_DIRECT || mode == BUY_FROM_STOCK => mode.to_string(),
        Some(mode) => {
            return Err(invalid(format!(
                "购入口径只能是 DIRECT_BUY / FROM_STOCK（当前 {mode}）"
            )));
        }
        None => {
            if source_type == SOURCE_STOCK_IN {
                BUY_FROM_STOCK.to_string()
            } else {
                BUY_DIRECT.to_string()
            }
        }
    };
    if source_type == SOURCE_STOCK_IN && buy_mode != BUY_FROM_STOCK {
        return Err(invalid(
            "来源是收货单时必须走 FROM_STOCK（借 1601 / 贷 1405）：\
             收货单已过账意味着存货科目已经增加，不冲回就是重复计价",
        ));
    }
    if buy_mode == BUY_FROM_STOCK && tax_amount > Decimal::ZERO {
        return Err(invalid(
            "FROM_STOCK 不能带可抵扣进项税：资产已经从存货科目转出，进项税在收货时就抵扣过了",
        ));
    }

    let pay_mode = req
        .pay_mode
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(PAY_CREDIT)
        .to_string();
    if ![PAY_CREDIT, PAY_CASH].contains(&pay_mode.as_str()) {
        return Err(invalid(format!(
            "付款方式只能是 CREDIT（赊购，贷 2202）/ CASH（现付，贷 1002）（当前 {pay_mode}）"
        )));
    }

    Ok(CreateAssetSpec {
        asset_name: name.to_string(),
        category_id,
        spec: req.spec.clone().filter(|text| !text.trim().is_empty()),
        use_dept_id,
        custodian_id: parse_opt_uuid(req.custodian_id.as_ref(), "使用人")?,
        location: req.location.clone().filter(|text| !text.trim().is_empty()),
        original_value: asset_compute::money(original_value),
        tax_amount: asset_compute::money(tax_amount),
        purchase_date,
        source_type,
        source_id,
        source_no: req.source_no.clone().filter(|text| !text.trim().is_empty()),
        buy_mode,
        pay_mode,
        supplier_id: parse_opt_uuid(req.supplier_id.as_ref(), "供应商")?,
        remark: req.remark.clone(),
    })
}

/// 修改卡片（只允许改「不改变金额与折旧口径」的字段）。
///
/// 金额 / 分类 / 年限 / 残值率 **不许改**：它们决定了已计提折旧的口径，
/// 改了就会让「累计折旧 = Σ明细」这条不变量（K2）永久失真。
/// 要改金额只能作废重来（处置 → 作废处置 → 建新卡），这是会计上的常规做法。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAssetReq {
    /// 名称。
    pub asset_name: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 存放地点。
    pub location: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 修改校验的输出（四个可空字段，用结构体而不是 4 元组：
/// 元组在调用处会退化成 `(name, spec, location, remark)` 这种靠位置记忆的代码）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdateAssetFields {
    /// 资产名称。
    pub asset_name: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 存放地点。
    pub location: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 修改校验。
pub fn validate_update(req: &UpdateAssetReq) -> Result<UpdateAssetFields, ApiError> {
    let name = match req.asset_name.as_deref().map(str::trim) {
        Some("") => {
            return Err(invalid("资产名称不能改成空字符串"));
        }
        Some(text) => {
            if text.chars().count() > MAX_NAME_LEN {
                return Err(invalid(format!("资产名称最长 {MAX_NAME_LEN} 字符")));
            }
            Some(text.to_string())
        }
        None => None,
    };
    Ok(UpdateAssetFields {
        asset_name: name,
        spec: req.spec.clone(),
        location: req.location.clone(),
        remark: req.remark.clone(),
    })
}

/// 部门转移。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TransferReq {
    /// 目标部门。
    pub to_dept_id: Option<String>,
    /// 变动日期（默认今天）。
    pub change_date: Option<NaiveDate>,
    /// 原因（必填，内控留痕）。
    pub reason: Option<String>,
}

/// 使用人变更。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CustodianReq {
    /// 新使用人（必填；清空请用「停用」而不是把人设成空）。
    pub to_user_id: Option<String>,
    /// 变动日期（默认今天）。
    pub change_date: Option<NaiveDate>,
    /// 原因（必填）。
    pub reason: Option<String>,
}

/// 校验通过后的部门转移参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSpec {
    /// 目标部门。
    pub to_dept_id: Uuid,
    /// 变动日期。
    pub change_date: NaiveDate,
    /// 原因。
    pub reason: String,
}

/// 部门转移校验。
pub fn validate_transfer(req: &TransferReq, today: NaiveDate) -> Result<TransferSpec, ApiError> {
    let to_dept_id = req
        .to_dept_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("目标部门必填"))?;
    let to_dept_id = parse_uuid_text(to_dept_id, "目标部门")?;
    Ok(TransferSpec {
        to_dept_id,
        change_date: req.change_date.unwrap_or(today),
        reason: require_reason(req.reason.as_deref(), "转移原因")?,
    })
}

/// 校验通过后的使用人变更参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodianSpec {
    /// 新使用人。
    pub to_user_id: Uuid,
    /// 变动日期。
    pub change_date: NaiveDate,
    /// 原因。
    pub reason: String,
}

/// 使用人变更校验。
pub fn validate_custodian(req: &CustodianReq, today: NaiveDate) -> Result<CustodianSpec, ApiError> {
    let to_user_id = req
        .to_user_id
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("新使用人必填（CCUSTODIAN 变动必须有承接人）"))?;
    Ok(CustodianSpec {
        to_user_id: parse_uuid_text(to_user_id, "新使用人")?,
        change_date: req.change_date.unwrap_or(today),
        reason: require_reason(req.reason.as_deref(), "变更原因")?,
    })
}

/// 停用 / 启用（`IN_USE ⇄ IDLE`）的请求体。
///
/// 与 `transfer` / `custodian` 同形（`change_date` 可空 = 今天、`reason` 必填）：
/// 三个接口改的都是「卡片状态 / 归属」，不该有两套字段口径。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StatusChangeReq {
    /// 目标状态：只接受 `IDLE`（停用）/ `IN_USE`（启用）。
    pub target_status: Option<String>,
    /// 变动日期（默认今天；**只进留痕，不参与任何计算**，与折旧口径无关）。
    pub change_date: Option<NaiveDate>,
    /// 原因（必填，内控留痕）。
    pub reason: Option<String>,
}

/// 校验通过后的停用 / 启用参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChangeSpec {
    /// 目标状态（只可能是 `STATUS_IDLE` / `STATUS_IN_USE`）。
    pub target_status: &'static str,
    /// 变动日期。
    pub change_date: NaiveDate,
    /// 原因。
    pub reason: String,
}

/// 停用 / 启用校验：目标态只接受 `IDLE` / `IN_USE`。
///
/// **`DISPOSED` 明确拒绝**：处置要出凭证、要审批（`asset_disposal` 流程），
/// 放它进来就等于给「无凭证、无审批的处置」开后门（FIN-ASSET-ST02 D2）。
/// 报文直接点名该走哪个接口，别让调用方猜。
pub fn validate_status_change(req: &StatusChangeReq, today: NaiveDate) -> Result<StatusChangeSpec, ApiError> {
    let raw = req
        .target_status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            invalid(format!(
                "目标状态必填：只能是 {STATUS_IDLE}（停用）/ {STATUS_IN_USE}（启用）"
            ))
        })?;
    let target_status = match raw {
        STATUS_IDLE => STATUS_IDLE,
        STATUS_IN_USE => STATUS_IN_USE,
        STATUS_DISPOSED => {
            return Err(invalid(
                "处置不能走本接口：请调用 POST /business/erp/asset/{id}/dispose 提交处置审批 \
                 （处置要落凭证、要走审批，本接口只做「在役 ⇄ 闲置」）",
            ));
        }
        other => {
            return Err(invalid(format!(
                "目标状态只能是 {STATUS_IDLE}（停用）/ {STATUS_IN_USE}（启用）（当前 {other}）"
            )));
        }
    };
    Ok(StatusChangeSpec {
        target_status,
        change_date: req.change_date.unwrap_or(today),
        reason: require_reason(req.reason.as_deref(), "停用 / 启用原因")?,
    })
}

/// 停用 / 启用对应的变动类型（`biz_fixed_asset_change.change_type` 的词表值）。
///
/// `IDLE` → `CHANGE_IDLE`（停用）、`IN_USE` → `CHANGE_ENABLE`（启用）：
/// 时间线按 `from_status/to_status` 渲染文案，`change_type` 只做分类统计，
/// 所以这里不加新词表值（`doc_kinds.ts` 与巡检脚本都按既有 5 个值对拍）。
pub fn change_type_of(target_status: &str) -> &'static str {
    debug_assert!(
        target_status == STATUS_IDLE || target_status == STATUS_IN_USE,
        "change_type_of 只接受 IDLE / IN_USE：调用点必须先过 validate_status_change"
    );
    if target_status == STATUS_IDLE {
        CHANGE_IDLE
    } else {
        CHANGE_ENABLE
    }
}

fn require_reason(raw: Option<&str>, field: &str) -> Result<String, ApiError> {
    let text = raw.map(str::trim).filter(|text| !text.is_empty()).ok_or_else(|| {
        invalid(format!(
            "{field}必填：变动留痕要能回答「为什么变」（内控要求，库里的 reason 是 NOT NULL）"
        ))
    })?;
    if text.chars().count() > MAX_REASON_LEN {
        return Err(invalid(format!("{field}最长 {MAX_REASON_LEN} 字符")));
    }
    Ok(text.to_string())
}

/// 提交处置（走审批）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DisposeReq {
    /// 处置类型：SALE / SCRAP / LOSS。
    pub disposal_type: Option<String>,
    /// 处置日期（默认今天）。
    pub disposal_date: Option<NaiveDate>,
    /// 处置原因（必填）／出售说明。
    pub reason: Option<String>,
    /// 出售价（不含税；仅 SALE）。
    pub sale_price: Option<Decimal>,
    /// 销项税（仅 SALE 且可开票时）。
    pub output_tax: Option<Decimal>,
}

/// 校验通过后的处置申请（`form_data` 的取值来源，字段名不许漂移）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposeSpec {
    /// 处置类型。
    pub kind: DisposalKind,
    /// 处置日期。
    pub disposal_date: NaiveDate,
    /// 原因。
    pub reason: String,
    /// 出售价。
    pub sale_price: Decimal,
    /// 销项税。
    pub output_tax: Decimal,
}

/// 处置校验（类型取值域 / 出售价与销项税只在 SALE 允许非零）。
pub fn validate_dispose(req: &DisposeReq, today: NaiveDate) -> Result<DisposeSpec, ApiError> {
    let raw = req
        .disposal_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid("处置类型必填：SALE（出售）/ SCRAP（报废）/ LOSS（盘亏）"))?;
    let kind = DisposalKind::parse(raw).ok_or_else(|| {
        invalid(format!(
            "处置类型只能是 SALE / SCRAP / LOSS（当前 {raw}）：本期只有这三种处置（ARCH-6 §5.6.3）"
        ))
    })?;
    let sale_price = req.sale_price.unwrap_or(Decimal::ZERO);
    let output_tax = req.output_tax.unwrap_or(Decimal::ZERO);
    if sale_price < Decimal::ZERO || output_tax < Decimal::ZERO {
        return Err(invalid(
            "出售价与销项税不能为负（红字调整请走凭证红冲，不要在处置里填负数）",
        ));
    }
    if kind != DisposalKind::Sale && (sale_price != Decimal::ZERO || output_tax != Decimal::ZERO) {
        return Err(invalid(format!(
            "{}处置不能有出售价 / 销项税（当前 售价 {sale_price} / 税额 {output_tax}）：\
             只有 SALE 有收入，SCRAP 与 LOSS 都是无收入处置",
            kind.label()
        )));
    }
    if kind == DisposalKind::Sale && sale_price == Decimal::ZERO && output_tax == Decimal::ZERO {
        return Err(invalid(
            "出售处置必须给出售价（可以为 0 吗？不可以 —— 零价出售请走「报废」或「盘亏」，\
             否则处置损益无从计算）",
        ));
    }
    Ok(DisposeSpec {
        kind,
        disposal_date: req.disposal_date.unwrap_or(today),
        reason: require_reason(req.reason.as_deref(), "处置原因")?,
        sale_price: asset_compute::money(sale_price),
        output_tax: asset_compute::money(output_tax),
    })
}

/// 卡片响应行（列表）。
#[derive(Debug, Clone, Serialize)]
pub struct AssetRes {
    /// id。
    pub id: String,
    /// 资产编号。
    pub asset_no: String,
    /// 名称。
    pub asset_name: String,
    /// 分类 id。
    pub category_id: String,
    /// 分类编码（快照）。
    pub category_code: String,
    /// 规格。
    pub spec: Option<String>,
    /// 使用部门。
    pub use_dept_id: String,
    /// 使用部门名称（快照）。
    pub use_dept_name: String,
    /// 使用人。
    pub custodian_id: Option<String>,
    /// 使用人姓名。
    pub custodian_name: Option<String>,
    /// 存放地点。
    pub location: Option<String>,
    /// 原值。
    pub original_value: Decimal,
    /// 可抵扣进项税。
    pub tax_amount: Decimal,
    /// 预计净残值。
    pub residual_value: Decimal,
    /// 可折旧总额。
    pub depreciable_base: Decimal,
    /// 月折旧额。
    pub monthly_depreciation: Decimal,
    /// 开始计提日。
    pub start_depreciation_date: NaiveDate,
    /// 累计折旧。
    pub accumulated_depreciation: Decimal,
    /// 净值。
    pub net_value: Decimal,
    /// 已提月数。
    pub depreciation_months_done: i32,
    /// 使用月数。
    pub useful_life_months: i32,
    /// 最后计提期间。
    pub last_depreciation_period: Option<String>,
    /// 购入日期。
    pub purchase_date: NaiveDate,
    /// 来源类型。
    pub source_type: String,
    /// 来源单号。
    pub source_no: Option<String>,
    /// 购入口径。
    pub buy_mode: Option<String>,
    /// 供应商名称。
    pub supplier_name: Option<String>,
    /// 购入凭证。
    pub voucher_id: Option<String>,
    /// 状态。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 处置类型。
    pub disposal_type: Option<String>,
    /// 处置日期。
    pub disposal_date: Option<NaiveDate>,
    /// 处置凭证。
    pub disposal_voucher_id: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl From<&AssetRow> for AssetRes {
    fn from(row: &AssetRow) -> Self {
        Self {
            id: row.id.to_string(),
            asset_no: row.asset_no.clone(),
            asset_name: row.asset_name.clone(),
            category_id: row.category_id.to_string(),
            category_code: row.category_code.clone(),
            spec: row.spec.clone(),
            use_dept_id: row.use_dept_id.to_string(),
            use_dept_name: row.use_dept_name.clone(),
            custodian_id: row.custodian_id.map(|id| id.to_string()),
            custodian_name: row.custodian_name.clone(),
            location: row.location.clone(),
            original_value: row.original_value,
            tax_amount: row.tax_amount,
            residual_value: row.residual_value,
            depreciable_base: row.depreciable_base,
            monthly_depreciation: row.monthly_depreciation,
            start_depreciation_date: row.start_depreciation_date,
            accumulated_depreciation: row.accumulated_depreciation,
            net_value: row.net_value,
            depreciation_months_done: row.depreciation_months_done,
            useful_life_months: row.useful_life_months,
            last_depreciation_period: row.last_depreciation_period.clone(),
            purchase_date: row.purchase_date,
            source_type: row.source_type.clone(),
            source_no: row.source_no.clone(),
            buy_mode: row.buy_mode.clone(),
            supplier_name: row.supplier_name.clone(),
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            disposal_type: row.disposal_type.clone(),
            disposal_date: row.disposal_date,
            disposal_voucher_id: row.disposal_voucher_id.map(|id| id.to_string()),
            remark: row.remark.clone(),
            created_at: row.created_at,
        }
    }
}

/// 变动记录响应行。
#[derive(Debug, Clone, Serialize)]
pub struct ChangeRes {
    /// id。
    pub id: String,
    /// 变动类型。
    pub change_type: String,
    /// 变动类型中文。
    pub change_label: String,
    /// 变动日期。
    pub change_date: NaiveDate,
    /// 原部门。
    pub from_dept_name: Option<String>,
    /// 新部门。
    pub to_dept_name: Option<String>,
    /// 原使用人。
    pub from_user_name: Option<String>,
    /// 新使用人。
    pub to_user_name: Option<String>,
    /// 原状态。
    pub from_status: Option<String>,
    /// 新状态。
    pub to_status: Option<String>,
    /// 原因。
    pub reason: String,
    /// 凭证。
    pub voucher_id: Option<String>,
    /// 审批实例。
    pub approval_instance_id: Option<String>,
    /// 操作人。
    pub operator_name: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 时间。
    pub created_at: DateTime<Utc>,
}

impl From<&AssetChangeRow> for ChangeRes {
    fn from(row: &AssetChangeRow) -> Self {
        Self {
            id: row.id.to_string(),
            change_type: row.change_type.clone(),
            change_label: change_label(&row.change_type).to_string(),
            change_date: row.change_date,
            from_dept_name: row.from_dept_name.clone(),
            to_dept_name: row.to_dept_name.clone(),
            from_user_name: row.from_user_name.clone(),
            to_user_name: row.to_user_name.clone(),
            from_status: row.from_status.clone(),
            to_status: row.to_status.clone(),
            reason: row.reason.clone(),
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
            operator_name: row.operator_name.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
        }
    }
}

/// 卡片详情（含变动时间线 + 折旧明细 + 在跑审批）。
#[derive(Debug, Clone, Serialize)]
pub struct AssetDetailRes {
    /// 卡片。
    pub asset: AssetRes,
    /// 变动时间线（倒序）。
    pub changes: Vec<ChangeRes>,
    /// 折旧明细（倒序）。
    pub depreciations: Vec<DepDetailRes>,
    /// 可去的下一个状态。
    pub next_statuses: Vec<String>,
    /// 在跑的处置审批实例（`None` = 没有）。
    pub pending_disposal: Option<String>,
    /// 剩余可提月数（人性化展示：处置决策要看「还能提多少」）。
    pub months_left: i32,
    /// 剩余可提金额。
    pub amount_left: Decimal,
    /// **本次操作的放行警告**（`None` = 无；E2E05-FIX-2 方案 B）。
    ///
    /// 只有「启用（`IDLE → IN_USE`）但缺发票 / 验收单」这一条路径会给值：
    /// 卡片状态照常改成在役，但审计里落一条「无附件启用」，
    /// 前端据此弹一条 warning（不是错误，操作已经成功）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// 建卡 / 变更后的实体 id。
#[derive(Debug, Clone, Serialize)]
pub struct IdRes {
    /// 主键。
    pub id: String,
    /// 单据号（资产编号 / 批次号）。
    pub no: String,
    /// 附带说明（凭证号之类）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ==================== 折旧 ====================

/// 折旧批次筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DepBatchFilter {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 期间。
    pub period_code: Option<String>,
    /// 状态。
    pub status: Option<String>,
}

/// 手动计提请求。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunDepreciationReq {
    /// 目标期间（`yyyy-MM`）。不传 = 上一个会计月（与定时任务同口径）。
    pub period_code: Option<String>,
    /// 备注。
    pub remark: Option<String>,
}

/// 红冲请求。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReverseDepreciationReq {
    /// 红冲原因（必填）。
    pub reason: Option<String>,
}

/// 折旧批次响应行。
#[derive(Debug, Clone, Serialize)]
pub struct DepBatchRes {
    /// id。
    pub id: String,
    /// 批次号。
    pub batch_no: String,
    /// 期间。
    pub period_code: String,
    /// 状态。
    pub status: String,
    /// 资产台数。
    pub asset_count: i32,
    /// 合计金额。
    pub total_amount: Decimal,
    /// 计提凭证。
    pub voucher_id: Option<String>,
    /// 红冲凭证。
    pub reverse_voucher_id: Option<String>,
    /// 跳过原因。
    pub skip_reason: Option<String>,
    /// 运行时间。
    pub run_at: DateTime<Utc>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 红冲时间。
    pub reversed_at: Option<DateTime<Utc>>,
    /// 备注。
    pub remark: Option<String>,
}

impl From<&DepBatchRow> for DepBatchRes {
    fn from(row: &DepBatchRow) -> Self {
        Self {
            id: row.id.to_string(),
            batch_no: row.batch_no.clone(),
            period_code: row.period_code.clone(),
            status: row.status.clone(),
            asset_count: row.asset_count,
            total_amount: row.total_amount,
            voucher_id: row.voucher_id.map(|id| id.to_string()),
            reverse_voucher_id: row.reverse_voucher_id.map(|id| id.to_string()),
            skip_reason: row.skip_reason.clone(),
            run_at: row.run_at,
            posted_at: row.posted_at,
            reversed_at: row.reversed_at,
            remark: row.remark.clone(),
        }
    }
}

/// 折旧明细响应行。
#[derive(Debug, Clone, Serialize)]
pub struct DepDetailRes {
    /// id。
    pub id: String,
    /// 资产 id。
    pub asset_id: String,
    /// 资产编号。
    pub asset_no: String,
    /// 资产名称。
    pub asset_name: String,
    /// 期间。
    pub period_code: String,
    /// 部门。
    pub dept_id: String,
    /// 部门名。
    pub dept_name: String,
    /// 费用科目编码。
    pub expense_account_code: String,
    /// 本月折旧。
    pub amount: Decimal,
    /// 计提前累计。
    pub accumulated_before: Decimal,
    /// 计提后累计。
    pub accumulated_after: Decimal,
    /// 计提后净值。
    pub net_value_after: Decimal,
    /// 是否尾差补齐期。
    pub is_last_month: bool,
    /// 状态。
    pub status: String,
}

impl From<&DepDetailRow> for DepDetailRes {
    fn from(row: &DepDetailRow) -> Self {
        Self {
            id: row.id.to_string(),
            asset_id: row.asset_id.to_string(),
            asset_no: row.asset_no.clone(),
            asset_name: row.asset_name.clone(),
            period_code: row.period_code.clone(),
            dept_id: row.dept_id.to_string(),
            dept_name: row.dept_name.clone(),
            expense_account_code: row.expense_account_code.clone(),
            amount: row.amount,
            accumulated_before: row.accumulated_before,
            accumulated_after: row.accumulated_after,
            net_value_after: row.net_value_after,
            is_last_month: row.is_last_month != 0,
            status: row.status.clone(),
        }
    }
}

/// 折旧批次详情（批次 + 明细 + 汇总）。
#[derive(Debug, Clone, Serialize)]
pub struct DepBatchDetailRes {
    /// 批次。
    pub batch: DepBatchRes,
    /// 明细。
    pub details: Vec<DepDetailRes>,
    /// 明细合计（必须等于 `batch.total_amount`，巡检 K4）。
    pub detail_total: Decimal,
    /// 可去的下一个状态。
    pub next_statuses: Vec<String>,
}

/// 计提结果（handler 与定时任务共用）。
#[derive(Debug, Clone, Serialize)]
pub struct RunDepreciationRes {
    /// 结果类型：`POSTED` / `SKIPPED` / `EXISTS` / `REVERSED_RERUN`。
    pub outcome: String,
    /// 批次（`SKIPPED` 时可能没有行）。
    pub batch: Option<DepBatchRes>,
    /// 说明。
    pub message: String,
    /// 本次提了多少钱。
    pub total_amount: Decimal,
    /// 本次提了多少台。
    pub asset_count: i32,
}

/// 计提结果类型（`depreciation_model::run_batch` 的返回值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum DepRunKind {
    /// 本次计提成功并过账。
    Posted,
    /// 本期无应提折旧（合法空跑，批次 SKIPPED）。
    Skipped,
    /// 该期间已有批次（幂等，不重提）。
    Exists,
    /// 红冲后重跑（复用同一批次行）。
    ReversedRerun,
}

impl DepRunKind {
    /// 落日志 / 报文用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            DepRunKind::Posted => "POSTED",
            DepRunKind::Skipped => "SKIPPED",
            DepRunKind::Exists => "EXISTS",
            DepRunKind::ReversedRerun => "REVERSED_RERUN",
        }
    }
}

/// 计提结果（服务层结构，`Serialize` 供 handler 直接返回）。
#[derive(Debug, Clone, Serialize)]
pub struct DepRunOutcome {
    /// 结果类型。
    pub outcome: DepRunKind,
    /// 批次（可能没有行：期间不可写的拒绝不落行）。
    pub batch: Option<DepBatchRes>,
    /// 说明。
    pub message: String,
    /// 本次提了多少钱。
    pub total_amount: Decimal,
    /// 本次提了多少台。
    pub asset_count: i32,
}

impl From<DepRunOutcome> for RunDepreciationRes {
    fn from(value: DepRunOutcome) -> Self {
        Self {
            outcome: value.outcome.as_str().to_string(),
            batch: value.batch,
            message: value.message,
            total_amount: value.total_amount,
            asset_count: value.asset_count,
        }
    }
}

/// 资产在役状态集合（折旧扫描与「可处置」判定共用）。
pub const LIVE_STATUSES: [&str; 2] = [STATUS_IN_USE, STATUS_IDLE];

/// 卡片是否可处置（在役 / 闲置）。
pub fn disposable(status: &str) -> bool {
    LIVE_STATUSES.contains(&status)
}

/// 由分类参数算出三项派生金额（建卡时算死，快照到卡片上）。
///
/// **分类改了不影响已建卡**：这是 §5.3.2 的硬要求（否则「改一个分类参数 =
/// 改所有历史资产的折旧额」，会计上是灾难）。
pub fn derive_amounts(
    original: Decimal,
    salvage_rate: Decimal,
    useful_life_months: i32,
) -> (Decimal, Decimal, Decimal) {
    let residual = residual_value(original, salvage_rate);
    let base = depreciable_base(original, residual);
    let monthly = monthly_depreciation(base, useful_life_months);
    (residual, base, monthly)
}

/// 折旧扫描口径的一句话说明（同时用于「本月应提 / 已提 / 差异」页脚文案）。
pub fn scan_caption() -> &'static str {
    "在役 + 闲置、已到计提起始日、未提足、本期处置的也算（当月减少当月照提）"
}

/// 表格里「已提 / 应提」的差异（折旧计提页的三行汇总）。
pub fn diff_of(should: Decimal, already: Decimal) -> Decimal {
    asset_compute::money(should - already)
}

/// 资产模型里的表名（供巡检脚本与错误文案引用）。
pub fn table_of(kind: &str) -> &'static str {
    match kind {
        "asset" => asset_model::TABLE,
        "category" => asset_model::CATEGORY_TABLE,
        "change" => asset_model::CHANGE_TABLE,
        "batch" => asset_model::DEP_BATCH_TABLE,
        "depreciation" => asset_model::DEP_TABLE,
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 金额字面量（同上：自己造 `dec!`，不引 `rust_decimal_macros`）。
    macro_rules! dec {
        ($v:literal) => {{
            use std::str::FromStr as _;
            Decimal::from_str(stringify!($v)).expect("金额字面量非法")
        }};
    }

    fn base_create() -> CreateAssetReq {
        CreateAssetReq {
            asset_name: Some("研发用笔记本".to_string()),
            category_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
            spec: Some("ThinkPad X1".to_string()),
            use_dept_id: Some("22222222-2222-4222-8222-222222222222".to_string()),
            custodian_id: None,
            location: Some("A 座 3 层".to_string()),
            original_value: Some(dec!(100000)),
            tax_amount: Some(Decimal::ZERO),
            purchase_date: NaiveDate::from_ymd_opt(2026, 10, 15),
            source_type: None,
            source_id: None,
            source_no: None,
            buy_mode: None,
            pay_mode: None,
            supplier_id: None,
            remark: None,
        }
    }

    /// ① 正常建卡：默认 MANUAL + DIRECT_BUY + 赊购；金额四位规整。
    #[test]
    fn create_ok_defaults() {
        let spec = validate_create(&base_create()).expect("正常建卡必须通过");
        assert_eq!(spec.source_type, SOURCE_MANUAL);
        assert_eq!(spec.buy_mode, BUY_DIRECT);
        assert_eq!(spec.pay_mode, PAY_CREDIT);
        assert_eq!(spec.original_value, dec!(100000));
        assert_eq!(spec.tax_amount, Decimal::ZERO);
    }

    /// ② 金额为负 → 400（库里的 ck_biz_fa_value 也会挡，但要先给可读报文）。
    #[test]
    fn create_rejects_non_positive_amount() {
        let mut req = base_create();
        req.original_value = Some(dec!(-1));
        let err = validate_create(&req).expect_err("负金额必须拒绝");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("原值必须大于 0"), "报文要指名问题：{}", err.msg);

        req.original_value = Some(Decimal::ZERO);
        assert!(validate_create(&req).is_err(), "零金额同样拒绝");
    }

    /// ③ 必填缺（名称 / 分类 / 部门 / 原值 / 购入日期）→ 400 且指名缺哪个。
    #[test]
    fn create_requires_mandatory_fields() {
        for (mutate, keyword) in [
            (0, "资产名称必填"),
            (1, "资产分类必填"),
            (2, "使用部门必填"),
            (3, "原值必填"),
            (4, "购入 / 入账日期必填"),
        ] {
            let mut req = base_create();
            match mutate {
                0 => req.asset_name = Some("   ".to_string()),
                1 => req.category_id = None,
                2 => req.use_dept_id = Some(String::new()),
                3 => req.original_value = None,
                4 => req.purchase_date = None,
                _ => unreachable!(),
            }
            let err = validate_create(&req).expect_err("缺必填必须拒绝");
            assert_eq!(err.status, 400);
            assert!(err.msg.contains(keyword), "报文应含「{keyword}」，实际：{}", err.msg);
        }
    }

    /// ④ 税额必须小于原值（原值口径是「可抵扣时不含税」）。
    #[test]
    fn create_rejects_tax_not_less_than_original() {
        let mut req = base_create();
        req.tax_amount = Some(dec!(100000));
        let err = validate_create(&req).expect_err("税额吃掉全部原值必须拒绝");
        assert!(err.msg.contains("必须小于原值"), "{}", err.msg);
    }

    /// ⑤ 收货单来源强制 FROM_STOCK，且不许带进项税。
    #[test]
    fn from_stock_is_forced_by_source() {
        let mut req = base_create();
        req.source_type = Some(SOURCE_STOCK_IN.to_string());
        req.source_id = Some("33333333-3333-4333-8333-333333333333".to_string());
        let spec = validate_create(&req).expect("收货单来源 + 来源 id 合法");
        assert_eq!(spec.buy_mode, BUY_FROM_STOCK, "收货单来源必须自动判 FROM_STOCK");

        let mut with_tax = req.clone();
        with_tax.tax_amount = Some(dec!(1300));
        assert!(
            validate_create(&with_tax).is_err(),
            "FROM_STOCK 不能带进项税（收货时已经抵扣过）"
        );

        let mut wrong_mode = base_create();
        wrong_mode.source_type = Some(SOURCE_STOCK_IN.to_string());
        wrong_mode.source_id = Some("33333333-3333-4333-8333-333333333333".to_string());
        wrong_mode.buy_mode = Some(BUY_DIRECT.to_string());
        assert!(validate_create(&wrong_mode).is_err(), "收货单来源不许显式选 DIRECT_BUY");
    }

    /// ⑥ 非 MANUAL 来源必须给来源单 id；MANUAL 不许给。
    #[test]
    fn source_reference_consistency() {
        let mut missing = base_create();
        missing.source_type = Some(SOURCE_PURCHASE_ORDER.to_string());
        assert!(validate_create(&missing).is_err(), "采购单来源缺 source_id");

        let mut extra = base_create();
        extra.source_id = Some("33333333-3333-4333-8333-333333333333".to_string());
        assert!(validate_create(&extra).is_err(), "MANUAL 不许带 source_id");
    }

    /// ⑦ 派生金额：100000 / 5% / 60 月 → 残值 5000、可折旧 95000、月折旧 1583.3333。
    #[test]
    fn derive_amounts_matches_design() {
        let (residual, base, monthly) = derive_amounts(dec!(100000), dec!(0.05), 60);
        assert_eq!(residual, dec!(5000.0000));
        assert_eq!(base, dec!(95000.0000));
        assert_eq!(monthly, dec!(1583.3333));
    }

    /// ⑧ 分类校验：年限越界 / 残值率越界 / 编码字符集 / 大小写原样保留。
    #[test]
    fn category_validation() {
        let ok = CategoryReq {
            category_code: Some("ELEC".to_string()),
            category_name: Some("电子设备".to_string()),
            useful_life_months: Some(36),
            salvage_rate: Some(dec!(0.05)),
            ..Default::default()
        };
        let spec = validate_category(&ok).expect("合法分类");
        assert_eq!(spec.status, 1, "默认启用");
        assert_eq!(spec.sort_order, 0);

        let mut bad = ok.clone();
        bad.useful_life_months = Some(601);
        assert!(validate_category(&bad).is_err());

        let mut bad_rate = ok.clone();
        bad_rate.salvage_rate = Some(Decimal::ONE);
        assert!(validate_category(&bad_rate).is_err(), "残值率 1 必须拒绝（<1）");

        let mut bad_code = ok.clone();
        bad_code.category_code = Some("CAT BUILDING".to_string());
        let err = validate_category(&bad_code).expect_err("编码含空格必须拒绝");
        assert!(err.msg.contains("分类编码"), "文案要点名是编码的问题：{}", err.msg);

        // CODE-04：编码留空 = 交给 handler 取号（`AC-01` 形态），不能拒
        let mut blank = ok.clone();
        blank.category_code = Some("   ".to_string());
        assert_eq!(
            validate_category(&blank).expect("留空应放行").category_code,
            "",
            "留空 → 空串（handler 据此取号）"
        );
        blank.category_code = None;
        assert_eq!(validate_category(&blank).expect("缺席同样放行").category_code, "");

        // CODE-04：自动编码形态（AC-01）与企业自有编码（CAT-BUILDING）都得能存
        let mut auto_like = ok.clone();
        auto_like.category_code = Some("AC-01".to_string());
        assert_eq!(
            validate_category(&auto_like).expect("AC-01 合法").category_code,
            "AC-01",
            "取号器产出的编码必须能通过校验"
        );
        let mut legacy = ok.clone();
        legacy.category_code = Some("cat-building".to_string());
        assert_eq!(
            validate_category(&legacy).expect("企业自有编码合法").category_code,
            "cat-building",
            "大小写原样保留（六类主数据统一口径，E2E08-FIX-2 ①）"
        );
    }

    /// ⑨ 处置校验：类型取值域 / 无收入处置不许填售价 / 出售必须有价。
    #[test]
    fn dispose_validation() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let ok = DisposeReq {
            disposal_type: Some("SALE".to_string()),
            disposal_date: None,
            reason: Some("闲置设备对外出售".to_string()),
            sale_price: Some(dec!(50000)),
            output_tax: Some(dec!(6500)),
        };
        let spec = validate_dispose(&ok, today).expect("合法出售处置");
        assert_eq!(spec.kind, DisposalKind::Sale);
        assert_eq!(spec.disposal_date, today, "不传处置日 → 今天");

        let mut bad_type = ok.clone();
        bad_type.disposal_type = Some("DONATE".to_string());
        assert!(validate_dispose(&bad_type, today).is_err(), "捐赠不在取值域");

        let mut scrap_with_price = ok.clone();
        scrap_with_price.disposal_type = Some("SCRAP".to_string());
        assert!(validate_dispose(&scrap_with_price, today).is_err(), "报废不许有售价");

        let mut sale_without_price = ok.clone();
        sale_without_price.sale_price = Some(Decimal::ZERO);
        sale_without_price.output_tax = Some(Decimal::ZERO);
        assert!(
            validate_dispose(&sale_without_price, today).is_err(),
            "出售必须有价（零价请走报废 / 盘亏）"
        );

        let mut no_reason = ok;
        no_reason.reason = Some("  ".to_string());
        assert!(validate_dispose(&no_reason, today).is_err(), "原因必填");
    }

    /// ⑩ 转移 / 使用人变更：目标必填、原因必填。
    #[test]
    fn transfer_and_custodian_validation() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        let transfer = TransferReq {
            to_dept_id: Some("22222222-2222-4222-8222-222222222222".to_string()),
            change_date: None,
            reason: Some("组织调整：研发并入产品部".to_string()),
        };
        let spec = validate_transfer(&transfer, today).expect("合法转移");
        assert_eq!(spec.change_date, today);

        let mut no_dept = transfer.clone();
        no_dept.to_dept_id = None;
        assert!(validate_transfer(&no_dept, today).is_err());

        let mut no_reason = transfer;
        no_reason.reason = None;
        assert!(validate_transfer(&no_reason, today).is_err(), "转移必须有原因");

        let custodian = CustodianReq {
            to_user_id: Some("44444444-4444-4444-8444-444444444444".to_string()),
            change_date: None,
            reason: Some("原使用人离职，转交".to_string()),
        };
        assert!(validate_custodian(&custodian, today).is_ok());

        let empty = CustodianReq::default();
        assert!(validate_custodian(&empty, today).is_err(), "新使用人必填");
    }

    /// ⑪ 停用 / 启用：目标态白名单（`DISPOSED` 必须拒绝并指向 `/dispose`）、原因必填、日期默认今天。
    #[test]
    fn status_change_validation() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 7).expect("日期");
        let idle = StatusChangeReq {
            target_status: Some("IDLE".to_string()),
            change_date: None,
            reason: Some("  生产线搬迁，设备暂不外借  ".to_string()),
        };
        let spec = validate_status_change(&idle, today).expect("停用必须通过");
        assert_eq!(spec.target_status, STATUS_IDLE);
        assert_eq!(spec.change_date, today, "不传变动日期 → 今天（只留痕）");
        assert_eq!(spec.reason, "生产线搬迁，设备暂不外借", "原因要去首尾空白");
        assert_eq!(
            change_type_of(spec.target_status),
            CHANGE_IDLE,
            "停用的变动类型必须是 IDLE（时间线与统计按它分类）"
        );

        let enable = StatusChangeReq {
            target_status: Some("IN_USE".to_string()),
            change_date: NaiveDate::from_ymd_opt(2026, 11, 1),
            reason: Some("恢复使用".to_string()),
        };
        let spec = validate_status_change(&enable, today).expect("启用必须通过");
        assert_eq!(spec.target_status, STATUS_IN_USE);
        assert_eq!(
            spec.change_date,
            NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期"),
            "传了变动日期就用传的"
        );
        assert_eq!(change_type_of(spec.target_status), CHANGE_ENABLE);

        // DISPOSED 必须明确拒绝，且报文点名走 /dispose（只写「非法」等于把人晾在原地）
        let disposed = StatusChangeReq {
            target_status: Some("DISPOSED".to_string()),
            ..idle.clone()
        };
        let err = validate_status_change(&disposed, today).expect_err("处置不许走本接口");
        assert_eq!(err.status, 400, "处置走错接口是参数问题，不是状态冲突");
        assert!(err.msg.contains("/dispose"), "报文必须指向处置接口：{}", err.msg);

        for bad in ["", "   ", "CANCELLED", "in_use"] {
            let req = StatusChangeReq {
                target_status: Some(bad.to_string()),
                ..idle.clone()
            };
            assert!(
                validate_status_change(&req, today).is_err(),
                "`{bad}` 不在白名单里，必须 400"
            );
        }
        let missing = StatusChangeReq {
            target_status: None,
            ..idle.clone()
        };
        assert!(validate_status_change(&missing, today).is_err(), "目标状态必填");

        let no_reason = StatusChangeReq {
            reason: Some("  ".to_string()),
            ..idle.clone()
        };
        assert!(
            validate_status_change(&no_reason, today).is_err(),
            "停用 / 启用必须有原因（内控留痕）"
        );
        let too_long = StatusChangeReq {
            reason: Some("停".repeat(MAX_REASON_LEN + 1)),
            ..idle
        };
        assert!(
            validate_status_change(&too_long, today).is_err(),
            "原因最长 {MAX_REASON_LEN} 字符"
        );
    }

    /// ⑫ 分页归一化：页码 ≥1、每页上限 100。
    #[test]
    fn page_normalization() {
        assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(3, 10_000), (3, MAX_PAGE_SIZE));
        assert_eq!(normalize_page(2, 50), (2, 50));
    }
}

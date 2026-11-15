//! 币种 / 汇率维护接口层（销售链 · SO-42 / ARCH-7 §15.6）：10 个接口。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/currency/list` | `business:erp:currency:list` | 币种列表（不分页，4~10 行） |
//! | GET | `/business/erp/currency/options` | `business:erp:currency:list` | 下拉选项（只 ACTIVE，含本位币标记） |
//! | POST | `/business/erp/currency` | `business:erp:currency:create` | 新建币种（可选直接立为本位币） |
//! | PUT | `/business/erp/currency/{code}` | `business:erp:currency:update` | 编辑（名称/符号/小数位/状态/备注） |
//! | POST | `/business/erp/currency/{code}/base` | `business:erp:currency:base` | 立为本位币（唯一） |
//! | GET | `/business/erp/exchange-rate/page` | `business:erp:fx-rate:list` | 汇率分页（币对/类型/生效日区间） |
//! | POST | `/business/erp/exchange-rate` | `business:erp:fx-rate:create` | 单条新建（同键已存在 409） |
//! | PUT | `/business/erp/exchange-rate/{id}` | `business:erp:fx-rate:update` | 编辑（撞同键 409） |
//! | DELETE | `/business/erp/exchange-rate/{id}` | `business:erp:fx-rate:delete` | 物理删 |
//! | POST | `/business/erp/exchange-rate/batch-month` | `business:erp:fx-rate:batch` | 按月批量录入（upsert 到月末日） |
//! | GET | `/business/erp/exchange-rate/quote` | `business:erp:fx-rate:quote` | 汇率预览（`from_currency` + `to_currency` + 日期 → 最近 SPOT；未知参数 400） |
//!
//! # 与建单锁定的分工（别混）
//!
//! * 本文件是**主数据维护**（写）：维护「1 外币 = 多少本位币」的牌价；
//! * [`crate::business::erp::currency`] 是**建单锁定**（读）：按单据日取最近 SPOT 并锁进单据行。
//!   报价预览接口 [`quote_rate`] 复用它的 `lock`，所以页面上看到的预览值与落库值**同一把尺**。
//!
//! # 六条入口校验（DDL 的 CHECK / FK 只是兜底）
//!
//! 1. `code` 规范化后必须 `^[A-Z]{3}$`（小写会被转大写，不静默接受 `US`）；
//! 2. 本位币行**不能停用**（`currency::base_currency_code` 只认 `is_base AND ACTIVE`，
//!    停掉本位币 = 全仓开单取不到本位币，回落 CNY 但日志只是 warn）；
//! 3. 立本位币要求该币种 `ACTIVE`（立一个停用币种当本位币同样取不到）；
//! 4. 汇率 `> 0`（0 会把整单折成 0 元）、`from <> to`（自反汇率恒 1，落库只会污染取数）；
//! 5. 汇率的 `from` / `to` 必须在 `biz_currency` 里存在（FK 兜底，但先查给人话）；
//! 6. 唯一键冲突（币种码 / 币对+类型+生效日）先查一次给人话 **409**。
//!
//! # 不碰的东西
//!
//! 币种与汇率是**主数据**不是单据：不生成凭证、不写库存三本账、不进流程引擎、不注册
//! `DocKind` / `LEGAL_EDGES`（`status` 只是启用/停用开关，没有状态机迁移边）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::business::erp::currency;
use crate::business::erp::currency_admin_model::{self as model, CurrencyRow, RateRow};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0316 的 sys_menu.auth_code 逐字一致） ====================

/// 币种列表 / 下拉选项。
pub const PERM_CURRENCY_LIST: &str = "business:erp:currency:list";
/// 币种新建。
pub const PERM_CURRENCY_CREATE: &str = "business:erp:currency:create";
/// 币种编辑（含启用 / 停用）。
pub const PERM_CURRENCY_UPDATE: &str = "business:erp:currency:update";
/// 立本位币。
pub const PERM_CURRENCY_BASE: &str = "business:erp:currency:base";
/// 汇率列表。
pub const PERM_RATE_LIST: &str = "business:erp:fx-rate:list";
/// 汇率新建。
pub const PERM_RATE_CREATE: &str = "business:erp:fx-rate:create";
/// 汇率编辑。
pub const PERM_RATE_UPDATE: &str = "business:erp:fx-rate:update";
/// 汇率删除。
pub const PERM_RATE_DELETE: &str = "business:erp:fx-rate:delete";
/// 按月批量录入。
pub const PERM_RATE_BATCH: &str = "business:erp:fx-rate:batch";
/// 汇率预览（开单选币种时查）。
pub const PERM_RATE_QUOTE: &str = "business:erp:fx-rate:quote";

/// 币种汇率域全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const CURRENCY_ADMIN_PERMISSIONS: [&str; 10] = [
    PERM_CURRENCY_LIST,
    PERM_CURRENCY_CREATE,
    PERM_CURRENCY_UPDATE,
    PERM_CURRENCY_BASE,
    PERM_RATE_LIST,
    PERM_RATE_CREATE,
    PERM_RATE_UPDATE,
    PERM_RATE_DELETE,
    PERM_RATE_BATCH,
    PERM_RATE_QUOTE,
];

// ==================== 错误出口 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404 + 10002`。
fn not_found(what: &str) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, format!("{what}不存在"))
}

/// `409 + 10003`：唯一键冲突 / 被引用不能改。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 写路径 DB 错误收口：唯一索引 / 外键 / CHECK 兜底也翻成人话，其余仍是 500。
fn write_error(error: DbError, ctx: impl AsRef<str>) -> ApiError {
    let ctx = ctx.as_ref();
    match error {
        DbError::UniqueViolation { constraint, .. } => conflict(format!(
            "{ctx}：同一组键已存在{}",
            constraint
                .as_deref()
                .map(|name| format!("（{name}）"))
                .unwrap_or_default()
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "{ctx}：数据不满足约束{}（币种码三位大写 / 小数位 0~4 / 汇率 > 0 / 币对不能自反）",
            constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
        )),
        DbError::ForeignKeyViolation { constraint, .. } => bad_request(format!(
            "{ctx}：引用的币种不存在{}",
            constraint
                .as_deref()
                .map(|name| format!("（{name}）"))
                .unwrap_or_default()
        )),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 入参解析 ====================

/// 日期入参 `YYYY-MM-DD`。
fn parse_date(text: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|error| bad_request(format!("{field} 不是合法日期（YYYY-MM-DD）：{error}")))
}

/// 北京时间今天（与全仓 `voucher::today_cst` 同一把尺）。
fn today_cst() -> NaiveDate {
    crate::business::erp::voucher::today_cst(Utc::now())
}

/// 币种码入参：规范化后必须三位大写。
fn parse_code(raw: &str, field: &str) -> std::result::Result<String, ApiError> {
    model::normalize_code(raw).ok_or_else(|| {
        bad_request(format!(
            "{field} 必须是三位 ISO 4217 大写字母（如 CNY / USD；当前 `{raw}`）"
        ))
    })
}

/// 状态入参（缺省 = `ACTIVE`）。
fn resolve_status(status: Option<&str>) -> std::result::Result<String, ApiError> {
    let value = status
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(model::STATUS_ACTIVE)
        .to_string();
    if !model::is_valid_status(&value) {
        return Err(bad_request(format!(
            "status 必须是 ACTIVE（启用）/ INACTIVE（停用）之一（当前 `{value}`）"
        )));
    }
    Ok(value)
}

/// 汇率类型入参（缺省 = `SPOT`）。
fn resolve_rate_type(kind: Option<&str>) -> std::result::Result<String, ApiError> {
    let value = kind
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(model::RATE_TYPE_SPOT)
        .to_string();
    if !model::is_valid_rate_type(&value) {
        return Err(bad_request(format!(
            "rate_type 必须是 SPOT（即时）/ MONTH_END（月末）之一（当前 `{value}`）"
        )));
    }
    Ok(value)
}

/// 汇率值校验 + 定标。
fn resolve_rate(raw: Decimal) -> std::result::Result<Decimal, ApiError> {
    if !model::rate_allowed(raw) {
        return Err(bad_request(format!(
            "汇率必须 > 0（当前 `{raw}`）：0 会把整单折成 0 元，负数会造出反向应收，\
             都比「没录汇率」危险得多"
        )));
    }
    Ok(model::scale_rate(raw))
}

/// 名称 / 符号 / 备注校验（长度上限来自 DDL）。
fn resolve_profile(
    name: &str,
    symbol: &str,
    remark: Option<String>,
) -> std::result::Result<(String, String, Option<String>), ApiError> {
    if !model::name_allowed(name) {
        return Err(bad_request(format!(
            "名称不能为空且不超过 {} 个字符（当前 `{}`）",
            model::NAME_MAX_LEN,
            name.trim()
        )));
    }
    if !model::symbol_allowed(symbol) {
        return Err(bad_request(format!(
            "符号不能为空且不超过 {} 个字符（当前 `{}`）",
            model::SYMBOL_MAX_LEN,
            symbol.trim()
        )));
    }
    let remark = model::normalize_remark(remark);
    if let Some(text) = &remark
        && text.chars().count() > model::REMARK_MAX_LEN
    {
        return Err(bad_request(format!(
            "备注不超过 {} 个字符（当前 {} 个）",
            model::REMARK_MAX_LEN,
            text.chars().count()
        )));
    }
    Ok((name.trim().to_string(), symbol.trim().to_string(), remark))
}

// ==================== 契约：币种 ====================

/// 币种列表筛选（**不分页**：极小主数据）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CurrencyFilter {
    /// 状态（`ACTIVE` / `INACTIVE`）。
    pub status: Option<String>,
    /// 关键字（币种码 / 名称，模糊）。
    pub keyword: Option<String>,
}

/// 币种出参（列表 / 新建 / 编辑同构）。
#[derive(Debug, Clone, Serialize)]
pub struct CurrencyRes {
    /// 币种码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 符号。
    pub symbol: String,
    /// 小数位。
    pub decimal_places: i32,
    /// 是否本位币。
    pub is_base: bool,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 该币种作为原币的汇率条数。
    pub rate_count: i64,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

impl From<&CurrencyRow> for CurrencyRes {
    fn from(row: &CurrencyRow) -> Self {
        Self {
            code: row.code.clone(),
            name: row.name.clone(),
            symbol: row.symbol.clone(),
            decimal_places: row.decimal_places,
            is_base: row.is_base,
            status: row.status.clone(),
            remark: row.remark.clone(),
            rate_count: row.rate_count,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

/// 下拉选项（**只 ACTIVE**：停用币种新单据不可选；本位币永远只有一条）。
#[derive(Debug, Clone, Serialize)]
pub struct CurrencyOptionRes {
    /// 币种码（下拉 value）。
    pub code: String,
    /// 显示名（`人民币（CNY）`）。
    pub label: String,
    /// 符号。
    pub symbol: String,
    /// 小数位（前端金额展示按它格式化）。
    pub decimal_places: i32,
    /// 是否本位币（前端默认选中它）。
    pub is_base: bool,
}

/// 新建币种。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateCurrencyReq {
    /// 币种码（三位字母，大小写不敏感）。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 符号。
    pub symbol: String,
    /// 小数位（缺省 2）。
    #[serde(default)]
    pub decimal_places: Option<i32>,
    /// 状态（缺省 `ACTIVE`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
    /// 是否顺手立为本位币（缺省 `false`；`true` 时同一请求内接管本位币）。
    #[serde(default)]
    pub set_base: Option<bool>,
}

/// 编辑币种（**部分更新**：不传 = 不改）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateCurrencyReq {
    /// 名称。
    #[serde(default)]
    pub name: Option<String>,
    /// 符号。
    #[serde(default)]
    pub symbol: Option<String>,
    /// 小数位。
    #[serde(default)]
    pub decimal_places: Option<i32>,
    /// 状态（本位币不能停用）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

// ==================== 契约：汇率 ====================

/// 汇率分页筛选。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RateFilter {
    /// 原币（精确）。
    pub from_currency: Option<String>,
    /// 目标币种（精确）。
    pub to_currency: Option<String>,
    /// 汇率类型（`SPOT` / `MONTH_END`）。
    pub rate_type: Option<String>,
    /// 生效日 ≥（`YYYY-MM-DD`）。
    pub date_from: Option<String>,
    /// 生效日 ≤（`YYYY-MM-DD`）。
    pub date_to: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

impl Default for RateFilter {
    fn default() -> Self {
        Self {
            from_currency: None,
            to_currency: None,
            rate_type: None,
            date_from: None,
            date_to: None,
            page: 1,
            page_size: model::DEFAULT_PAGE_SIZE,
        }
    }
}

/// 汇率出参。
#[derive(Debug, Clone, Serialize)]
pub struct RateRes {
    /// 主键。
    pub id: String,
    /// 原币。
    pub from_currency: String,
    /// 目标币种。
    pub to_currency: String,
    /// `1 原币 = rate 目标币`。
    pub rate: Decimal,
    /// `SPOT` / `MONTH_END`。
    pub rate_type: String,
    /// 生效日（含）。
    pub effective_date: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
    /// 更新时间（RFC3339）。
    pub updated_at: String,
}

impl From<&RateRow> for RateRes {
    fn from(row: &RateRow) -> Self {
        Self {
            id: row.id.to_string(),
            from_currency: row.from_currency.clone(),
            to_currency: row.to_currency.clone(),
            rate: row.rate,
            rate_type: row.rate_type.clone(),
            effective_date: row.effective_date.to_string(),
            remark: row.remark.clone(),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

/// 新建汇率。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateRateReq {
    /// 原币（必填，非本位币）。
    pub from_currency: String,
    /// 目标币种（缺省 = 账套本位币）。
    #[serde(default)]
    pub to_currency: Option<String>,
    /// 汇率（> 0）。
    pub rate: Decimal,
    /// 类型（缺省 `SPOT`）。
    #[serde(default)]
    pub rate_type: Option<String>,
    /// 生效日（缺省 = 今天 CST；**不会**按月末推算）。
    #[serde(default)]
    pub effective_date: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 编辑汇率（部分更新）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateRateReq {
    /// 原币。
    #[serde(default)]
    pub from_currency: Option<String>,
    /// 目标币种。
    #[serde(default)]
    pub to_currency: Option<String>,
    /// 汇率。
    #[serde(default)]
    pub rate: Option<Decimal>,
    /// 类型。
    #[serde(default)]
    pub rate_type: Option<String>,
    /// 生效日。
    #[serde(default)]
    pub effective_date: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 月度批量录入的单行。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BatchRateItem {
    /// 原币（三位码）。
    pub from_currency: String,
    /// 汇率（> 0）。
    pub rate: Decimal,
    /// 备注（缺省沿用整批备注）。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 按月批量录入。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BatchMonthReq {
    /// 期间 `YYYY-MM`（生效日 = 该月月末）。
    pub month: String,
    /// 类型（缺省 `MONTH_END`：按月录入的天然语义是月末牌价）。
    #[serde(default)]
    pub rate_type: Option<String>,
    /// 整批备注（来源：中国银行 / 央行中间价 …）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 各行汇率（1~50 行，原币不可重复）。
    #[serde(default)]
    pub rates: Vec<BatchRateItem>,
}

/// 批量录入结果里的一行。
#[derive(Debug, Clone, Serialize)]
pub struct BatchRateRes {
    /// 原币。
    pub from_currency: String,
    /// 落库汇率。
    pub rate: Decimal,
    /// `true` = 新插；`false` = 覆盖了同键既有行。
    pub created: bool,
}

/// 批量录入结果。
#[derive(Debug, Clone, Serialize)]
pub struct BatchMonthRes {
    /// 期间 `YYYY-MM`。
    pub month: String,
    /// 生效日（该月月末）。
    pub effective_date: String,
    /// 类型。
    pub rate_type: String,
    /// 目标币种（本位币）。
    pub to_currency: String,
    /// 新插入条数。
    pub created_count: u64,
    /// 覆盖条数。
    pub updated_count: u64,
    /// 逐行结果。
    pub saved: Vec<BatchRateRes>,
}

/// 汇率预览（开单选币种时用）：**只读**，不落库。
#[derive(Debug, Clone, Serialize)]
pub struct QuoteRes {
    /// 要折的币种（= 老字段 `currency_code` 的新名字）。
    pub from_currency: String,
    /// 折成的币种（= 老字段 `base_currency` 的新名字；本接口恒为本位币）。
    pub to_currency: String,
    /// 请求币种（**废弃别名**，值恒等于 `from_currency`；老前端 / 老脚本仍在读）。
    pub currency_code: String,
    /// 账套本位币（**废弃别名**，值恒等于 `to_currency`）。
    pub base_currency: String,
    /// 锁定汇率（`1 原币 = exchange_rate 本位币`；本位币恒 1）。
    pub exchange_rate: Decimal,
    /// 取数日期（含）。
    pub on_date: String,
    /// 汇率来源：`BASE`（本位币恒 1）/ `SPOT`（取到牌价）/ `MISSING`（外币但无可用汇率）。
    pub source: String,
    /// 取到的那条汇率行的生效日（`SPOT` 时有值）。
    pub effective_date: Option<String>,
}

// ==================== 业务编排（无 service 层） ====================

/// 币种列表。
pub async fn list_currency(filter: CurrencyFilter) -> std::result::Result<Vec<CurrencyRes>, ApiError> {
    if let Some(status) = filter.status.as_deref().map(str::trim).filter(|text| !text.is_empty())
        && !model::is_valid_status(status)
    {
        return Err(bad_request(format!(
            "status 必须是 ACTIVE / INACTIVE 之一（当前 `{status}`）"
        )));
    }
    let rows = model::currency_list(filter.status.as_deref(), filter.keyword.as_deref())
        .await
        .map_err(|error| utils::db_error(error, "查询币种列表"))?;
    Ok(rows.iter().map(CurrencyRes::from).collect())
}

/// 下拉选项（只 ACTIVE）。
pub async fn currency_options() -> std::result::Result<Vec<CurrencyOptionRes>, ApiError> {
    let rows = model::currency_list(Some(model::STATUS_ACTIVE), None)
        .await
        .map_err(|error| utils::db_error(error, "查询币种下拉"))?;
    Ok(rows
        .iter()
        .map(|row| CurrencyOptionRes {
            code: row.code.clone(),
            label: format!("{}（{}）", row.name, row.code),
            symbol: row.symbol.clone(),
            decimal_places: row.decimal_places,
            is_base: row.is_base,
        })
        .collect())
}

/// 新建币种（可选顺手立本位币）。
pub async fn create_currency(
    operator: Option<Uuid>,
    req: CreateCurrencyReq,
) -> std::result::Result<CurrencyRes, ApiError> {
    let code = parse_code(&req.code, "code")?;
    let (name, symbol, remark) = resolve_profile(&req.name, &req.symbol, req.remark)?;
    let decimal_places = req.decimal_places.unwrap_or(2);
    if !model::decimal_places_allowed(decimal_places) {
        return Err(bad_request(format!(
            "decimal_places 必须是 0~{} 的整数（当前 {decimal_places}）：本仓金额列统一 \
             NUMERIC(18,4)，超过 4 位会在「原币 × 汇率 = 本位币」链路上静默丢分",
            model::DECIMAL_PLACES_MAX
        )));
    }
    let status = resolve_status(req.status.as_deref())?;
    let set_base = req.set_base.unwrap_or(false);
    if set_base && status != model::STATUS_ACTIVE {
        return Err(bad_request(
            "要立为本位币的币种必须是 ACTIVE：本位币取数只认 `is_base AND status = 'ACTIVE'`，\
             立一个停用币种等于让全仓开单取不到本位币",
        ));
    }

    if model::currency_find(&code)
        .await
        .map_err(|error| utils::db_error(error, "按币种码查重"))?
        .is_some()
    {
        return Err(conflict(format!(
            "币种 `{code}` 已存在：要改名称 / 符号 / 状态请编辑原行（币种码是主键，不可重复）"
        )));
    }

    let write = model::CurrencyWrite {
        code: code.clone(),
        name,
        symbol,
        decimal_places,
        status,
        remark,
    };
    model::currency_insert(&write, operator)
        .await
        .map_err(|error| write_error(error, format!("新建币种 `{code}`")))?;

    if set_base {
        model::currency_set_base(&code, operator)
            .await
            .map_err(|error| write_error(error, format!("把 `{code}` 立为本位币")))?;
    }

    let row = model::currency_find(&code)
        .await
        .map_err(|error| utils::db_error(error, "回读新建币种"))?
        .ok_or_else(|| not_found(&format!("币种 `{code}`")))?;
    Ok(CurrencyRes::from(&row))
}

/// 编辑币种（部分更新）。
pub async fn update_currency(
    operator: Option<Uuid>,
    code_raw: &str,
    req: UpdateCurrencyReq,
) -> std::result::Result<CurrencyRes, ApiError> {
    let code = parse_code(code_raw, "code")?;
    let current = model::currency_find(&code)
        .await
        .map_err(|error| utils::db_error(error, "查询币种"))?
        .ok_or_else(|| not_found(&format!("币种 `{code}`")))?;

    let name = match req.name.as_deref() {
        None => current.name.clone(),
        Some(text) => text.trim().to_string(),
    };
    let symbol = match req.symbol.as_deref() {
        None => current.symbol.clone(),
        Some(text) => text.trim().to_string(),
    };
    let remark = match req.remark.clone() {
        None => current.remark.clone(),
        Some(value) => model::normalize_remark(Some(value)),
    };
    let (name, symbol, remark) = resolve_profile(&name, &symbol, remark)?;

    let decimal_places = req.decimal_places.unwrap_or(current.decimal_places);
    if !model::decimal_places_allowed(decimal_places) {
        return Err(bad_request(format!(
            "decimal_places 必须是 0~{} 的整数（当前 {decimal_places}）",
            model::DECIMAL_PLACES_MAX
        )));
    }

    let status = match req.status.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => current.status.clone(),
        Some(_) => resolve_status(req.status.as_deref())?,
    };
    // 本位币不能停用：全仓 base_currency_code 只认 is_base AND ACTIVE
    if current.is_base && status != model::STATUS_ACTIVE {
        return Err(conflict(format!(
            "`{code}` 是当前账套的本位币，不能停用：要换本位币请先把别的币种「设为本位币」\
             （本位币唯一，且必须 ACTIVE）"
        )));
    }
    if let Some(text) = &remark
        && text.chars().count() > model::REMARK_MAX_LEN
    {
        return Err(bad_request(format!(
            "备注不超过 {} 个字符（当前 {} 个）",
            model::REMARK_MAX_LEN,
            text.chars().count()
        )));
    }

    let write = model::CurrencyWrite {
        code: code.clone(),
        name,
        symbol,
        decimal_places,
        status,
        remark,
    };
    model::currency_update(&code, &write, operator)
        .await
        .map_err(|error| write_error(error, format!("编辑币种 `{code}`")))?;

    let row = model::currency_find(&code)
        .await
        .map_err(|error| utils::db_error(error, "回读编辑后的币种"))?
        .ok_or_else(|| not_found(&format!("币种 `{code}`")))?;
    Ok(CurrencyRes::from(&row))
}

/// 立本位币（唯一）。
pub async fn set_base_currency(operator: Option<Uuid>, code_raw: &str) -> std::result::Result<CurrencyRes, ApiError> {
    let code = parse_code(code_raw, "code")?;
    let current = model::currency_find(&code)
        .await
        .map_err(|error| utils::db_error(error, "查询币种"))?
        .ok_or_else(|| not_found(&format!("币种 `{code}`")))?;
    if current.is_base {
        return Ok(CurrencyRes::from(&current));
    }
    if current.status != model::STATUS_ACTIVE {
        return Err(conflict(format!(
            "`{code}` 已停用，不能立为本位币：本位币取数只认 `is_base AND status = 'ACTIVE'`，\
             请先启用该币种"
        )));
    }
    model::currency_set_base(&code, operator)
        .await
        .map_err(|error| write_error(error, format!("把 `{code}` 立为本位币")))?;
    let row = model::currency_find(&code)
        .await
        .map_err(|error| utils::db_error(error, "回读本位币"))?
        .ok_or_else(|| not_found(&format!("币种 `{code}`")))?;
    Ok(CurrencyRes::from(&row))
}

/// 汇率分页。
pub async fn page_rate(filter: RateFilter) -> std::result::Result<PageData<Vec<RateRes>>, ApiError> {
    if let Some(kind) = filter
        .rate_type
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        && !model::is_valid_rate_type(kind)
    {
        return Err(bad_request(format!(
            "rate_type 必须是 SPOT / MONTH_END 之一（当前 `{kind}`）"
        )));
    }
    let from_currency = match filter
        .from_currency
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(text) => Some(parse_code(text, "from_currency")?),
    };
    let to_currency = match filter
        .to_currency
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(text) => Some(parse_code(text, "to_currency")?),
    };
    let date_from = match filter
        .date_from
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => None,
        Some(text) => Some(parse_date(text, "date_from")?),
    };
    let date_to = match filter.date_to.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => None,
        Some(text) => Some(parse_date(text, "date_to")?),
    };
    if let (Some(from), Some(to)) = (date_from, date_to)
        && from > to
    {
        return Err(bad_request(format!(
            "生效日区间反了：date_from {from} 晚于 date_to {to}"
        )));
    }

    let (rows, total) = model::rate_page(
        from_currency.as_deref(),
        to_currency.as_deref(),
        filter.rate_type.as_deref(),
        date_from,
        date_to,
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询汇率"))?;
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list: rows.iter().map(RateRes::from).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 币种存在性检查（`from` / `to` 都要在 `biz_currency` 里）。
async fn ensure_currency_exists(code: &str, field: &str) -> std::result::Result<(), ApiError> {
    let found = model::currency_find(code)
        .await
        .map_err(|error| utils::db_error(error, "查询币种是否存在"))?;
    if found.is_none() {
        return Err(bad_request(format!(
            "{field} `{code}` 不在币种主数据里：请先在「币种管理」里新增该币种，\
             否则月末重估会静默跳过这条汇率"
        )));
    }
    Ok(())
}

/// 新建汇率。
pub async fn create_rate(operator: Option<Uuid>, req: CreateRateReq) -> std::result::Result<RateRes, ApiError> {
    let from_currency = parse_code(&req.from_currency, "from_currency")?;
    let to_currency = match req
        .to_currency
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => currency::base_currency_code(db())
            .await
            .map_err(|error| utils::db_error(error, "取账套本位币"))?,
        Some(text) => parse_code(text, "to_currency")?,
    };
    if model::is_self_pair(&from_currency, &to_currency) {
        return Err(bad_request(format!(
            "币对不能自反（{from_currency} → {to_currency}）：自反汇率恒为 1，\
             落库只会让「取最近一条」多一条噪声行"
        )));
    }
    let rate = resolve_rate(req.rate)?;
    let rate_type = resolve_rate_type(req.rate_type.as_deref())?;
    let effective_date = match req
        .effective_date
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => today_cst(),
        Some(text) => parse_date(text, "effective_date")?,
    };
    let remark = model::normalize_remark(req.remark.clone());
    ensure_currency_exists(&from_currency, "from_currency").await?;
    ensure_currency_exists(&to_currency, "to_currency").await?;

    if let Some(existing) = model::rate_taken(&from_currency, &to_currency, &rate_type, effective_date)
        .await
        .map_err(|error| utils::db_error(error, "汇率查重"))?
    {
        return Err(conflict(format!(
            "{from_currency} → {to_currency} 在 {effective_date} 已有 {rate_type} 汇率（id `{existing}`）：\
             同一组键只能一条，要改价请编辑原行，或换一个生效日"
        )));
    }

    let write = model::RateWrite {
        from_currency: from_currency.clone(),
        to_currency: to_currency.clone(),
        rate,
        rate_type: rate_type.clone(),
        effective_date,
        remark,
    };
    let id = Uuid::new_v4();
    model::rate_insert(id, &write, operator)
        .await
        .map_err(|error| write_error(error, format!("新建 {from_currency} → {to_currency} 汇率")))?;

    let row = model::rate_find(id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建汇率"))?
        .ok_or_else(|| not_found("新建的汇率行"))?;
    Ok(RateRes::from(&row))
}

/// 解析 `id` 路径参数。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|_| bad_request(format!("{field} 不是合法 UUID：`{text}`")))
}

/// 编辑汇率（部分更新）。
pub async fn update_rate(
    operator: Option<Uuid>,
    id_raw: &str,
    req: UpdateRateReq,
) -> std::result::Result<RateRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::rate_find(id)
        .await
        .map_err(|error| utils::db_error(error, "查询汇率"))?
        .ok_or_else(|| not_found("汇率行"))?;

    let from_currency = match req
        .from_currency
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => current.from_currency.clone(),
        Some(text) => parse_code(text, "from_currency")?,
    };
    let to_currency = match req
        .to_currency
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => current.to_currency.clone(),
        Some(text) => parse_code(text, "to_currency")?,
    };
    if model::is_self_pair(&from_currency, &to_currency) {
        return Err(bad_request(format!("币对不能自反（{from_currency} → {to_currency}）")));
    }
    let rate = match req.rate {
        None => current.rate,
        Some(value) => resolve_rate(value)?,
    };
    let rate_type = match req.rate_type.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => current.rate_type.clone(),
        Some(_) => resolve_rate_type(req.rate_type.as_deref())?,
    };
    let effective_date = match req
        .effective_date
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => current.effective_date,
        Some(text) => parse_date(text, "effective_date")?,
    };
    let remark = match req.remark.clone() {
        None => current.remark.clone(),
        Some(value) => model::normalize_remark(Some(value)),
    };
    ensure_currency_exists(&from_currency, "from_currency").await?;
    ensure_currency_exists(&to_currency, "to_currency").await?;

    // 编辑后若撞上另一行的同键 ⇒ 409（不许把两行改成同一组键）
    if let Some(other) = model::rate_taken(&from_currency, &to_currency, &rate_type, effective_date)
        .await
        .map_err(|error| utils::db_error(error, "汇率查重"))?
        && other != id
    {
        return Err(conflict(format!(
            "{from_currency} → {to_currency} 在 {effective_date} 已有 {rate_type} 汇率（id `{other}`）：\
             同一组键只能一条"
        )));
    }

    let write = model::RateWrite {
        from_currency,
        to_currency,
        rate,
        rate_type,
        effective_date,
        remark,
    };
    model::rate_update(id, &write, operator)
        .await
        .map_err(|error| write_error(error, "编辑汇率"))?;

    let row = model::rate_find(id)
        .await
        .map_err(|error| utils::db_error(error, "回读编辑后的汇率"))?
        .ok_or_else(|| not_found("汇率行"))?;
    Ok(RateRes::from(&row))
}

/// 删除汇率行。
pub async fn delete_rate(id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = model::rate_delete(id)
        .await
        .map_err(|error| utils::db_error(error, "删除汇率"))?;
    if affected == 0 {
        return Err(not_found("汇率行"));
    }
    Ok(())
}

/// 按月批量录入（upsert 到月末日）。
///
/// 语义：`month = 2026-11` ⇒ `effective_date = 2026-11-30`；同币对同类型同日已存在则**改值**，
/// 否则新插。整批在同一事务语义下逐行 upsert（每行一条 `ON CONFLICT`，失败即整批报错）。
pub async fn batch_month_rates(
    operator: Option<Uuid>,
    req: BatchMonthReq,
) -> std::result::Result<BatchMonthRes, ApiError> {
    let (year, month) = model::parse_month(&req.month).ok_or_else(|| {
        bad_request(format!(
            "month 必须是 `YYYY-MM`（如 2026-11，当前 `{}`）",
            req.month.trim()
        ))
    })?;
    let effective_date = model::month_end(year, month)
        .ok_or_else(|| bad_request(format!("{} 不是一个合法月份，算不出月末日", req.month.trim())))?;
    let rate_type = resolve_rate_type(req.rate_type.as_deref())?;
    let batch_remark = model::normalize_remark(req.remark.clone());
    if req.rates.is_empty() {
        return Err(bad_request("rates 不能为空：至少要录一条汇率（空批次不写库）"));
    }
    if req.rates.len() > model::BATCH_MAX_ROWS {
        return Err(bad_request(format!(
            "一次最多录 {} 条汇率（当前 {} 条）",
            model::BATCH_MAX_ROWS,
            req.rates.len()
        )));
    }
    let to_currency = currency::base_currency_code(db())
        .await
        .map_err(|error| utils::db_error(error, "取账套本位币"))?;

    // 先整批校验（任一非法 ⇒ 一条都不写，不做半批）
    let mut seen: Vec<String> = Vec::with_capacity(req.rates.len());
    let mut writes: Vec<model::RateWrite> = Vec::with_capacity(req.rates.len());
    for (index, item) in req.rates.iter().enumerate() {
        let field = format!("rates[{index}].from_currency");
        let from_currency = parse_code(&item.from_currency, &field)?;
        if model::is_self_pair(&from_currency, &to_currency) {
            return Err(bad_request(format!(
                "rates[{index}]：`{from_currency}` 就是本位币，不需要录入「{from_currency} → {to_currency}」\
                 的自反汇率（本位币汇率恒为 1）"
            )));
        }
        if seen.contains(&from_currency) {
            return Err(bad_request(format!(
                "rates[{index}]：币种 `{from_currency}` 在本批里重复出现（同一币种一个月只能一条）"
            )));
        }
        seen.push(from_currency.clone());
        ensure_currency_exists(&from_currency, &field).await?;
        let rate = resolve_rate(item.rate)?;
        writes.push(model::RateWrite {
            from_currency,
            to_currency: to_currency.clone(),
            rate,
            rate_type: rate_type.clone(),
            effective_date,
            remark: model::normalize_remark(item.remark.clone()).or_else(|| batch_remark.clone()),
        });
    }

    let mut saved = Vec::with_capacity(writes.len());
    let mut created_count: u64 = 0;
    let mut updated_count: u64 = 0;
    for write in &writes {
        let existed = model::rate_taken(
            &write.from_currency,
            &write.to_currency,
            &write.rate_type,
            write.effective_date,
        )
        .await
        .map_err(|error| utils::db_error(error, "批量录入前查重"))?
        .is_some();
        model::rate_upsert(write, operator)
            .await
            .map_err(|error| write_error(error, format!("批量录入 {} 汇率", write.from_currency)))?;
        if existed {
            updated_count += 1;
        } else {
            created_count += 1;
        }
        saved.push(BatchRateRes {
            from_currency: write.from_currency.clone(),
            rate: write.rate,
            created: !existed,
        });
    }

    Ok(BatchMonthRes {
        month: format!("{year:04}-{month:02}"),
        effective_date: effective_date.to_string(),
        rate_type,
        to_currency,
        created_count,
        updated_count,
        saved,
    })
}

/// 汇率预览（开单选币种时查；**只读**）。
///
/// 复用 [`currency::lock`]：页面上看到的预览值 = 建单时锁进单据行的值，不会「预览 7.1、
/// 落库 7.0」。
///
/// # 参数口径（E2E-02 / E2E-FIX-8 ③）
///
/// * `from_currency`：要折的币种（缺省 = 本位币）；
/// * `to_currency`：折成什么（缺省 = 本位币，且**只能是本位币** —— 交叉汇率本接口不做，
///   给了别的币种 400 而不是硬算一个没人核过的倒数）；
/// * `date`：取数日期（缺省 = 今天 CST）；
/// * `currency_code`：**已废弃别名**（等价于 `from_currency`，老脚本还能用；
///   同时给两个且不一致 → 400）。
///
/// 未知 / 拼错的参数由 [`parse_quote_query`] 拦成 400 —— 历史实现是 serde 静默忽略，
/// 于是 `?from_currency=USD` 会按「未指定币种」回本位币 1，排查时被误判成「汇率没维护」。
pub async fn quote_rate(
    from_currency_raw: Option<&str>,
    to_currency_raw: Option<&str>,
    on_date_raw: Option<&str>,
) -> std::result::Result<QuoteRes, ApiError> {
    let on_date = match on_date_raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => today_cst(),
        Some(text) => parse_date(text, "date")?,
    };
    let requested = match from_currency_raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => None,
        Some(text) => Some(parse_code(text, "from_currency")?),
    };
    let target = match to_currency_raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => None,
        Some(text) => Some(parse_code(text, "to_currency")?),
    };
    // 本位币由 `currency::lock` 一并返回（它内部取账套本位币），这里不重复查一遍
    let outcome = currency::lock(db(), requested.as_deref(), on_date)
        .await
        .map_err(|error| utils::db_error(error, "预览汇率"))?;
    match outcome {
        currency::LockOutcome::Locked(locked) => {
            ensure_target_currency(target.as_deref(), &locked.base_currency)?;
            let is_base = locked.currency_code == locked.base_currency;
            let effective_date = if is_base {
                None
            } else {
                model::spot_row(&locked.currency_code, &locked.base_currency, on_date)
                    .await
                    .map_err(|error| utils::db_error(error, "预览汇率的生效日"))?
                    .map(|(date, _)| date.to_string())
            };
            Ok(QuoteRes {
                from_currency: locked.currency_code.clone(),
                to_currency: locked.base_currency.clone(),
                currency_code: locked.currency_code,
                base_currency: locked.base_currency,
                exchange_rate: locked.exchange_rate,
                on_date: on_date.to_string(),
                source: if is_base { "BASE" } else { "SPOT" }.to_string(),
                effective_date,
            })
        }
        currency::LockOutcome::MissingRate {
            currency_code,
            base_currency,
            ..
        } => {
            ensure_target_currency(target.as_deref(), &base_currency)?;
            Ok(QuoteRes {
                from_currency: currency_code.clone(),
                to_currency: base_currency.clone(),
                currency_code,
                base_currency,
                exchange_rate: Decimal::ZERO,
                on_date: on_date.to_string(),
                source: "MISSING".to_string(),
                effective_date: None,
            })
        }
    }
}

/// `to_currency` 只能是账套本位币（交叉汇率本接口不做）。
///
/// 为什么不做倒数：汇率取数是「1 外币 = 多少本位币」，本系统没有任何地方锁定过
/// 本位币→外币的牌价；用 `1/rate` 现算会在 4 位小数舍入上与财务手工维护的牌价对不上，
/// 报价页显示的数与建单落库的数就不再是「同一把尺」（本模块的核心承诺）。
fn ensure_target_currency(target: Option<&str>, base_currency: &str) -> std::result::Result<(), ApiError> {
    match target {
        Some(code) if code != base_currency => Err(bad_request(format!(
            "to_currency 只支持账套本位币 `{base_currency}`（当前 `{code}`）：\
             本接口不做交叉汇率，请先按 from_currency → {base_currency} 折算再换第二段"
        ))),
        _ => Ok(()),
    }
}

/// 预览查询串（**参数名对齐**，见 [`quote_rate`] 的口径）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuoteQuery {
    /// 要折的币种（缺省 = 本位币）。
    pub from_currency: Option<String>,
    /// 折成的币种（缺省 / 只能是本位币）。
    pub to_currency: Option<String>,
    /// 取数日期（`yyyy-MM-dd`，缺省 = 今天 CST）。
    pub date: Option<String>,
}

/// 预览接口接受的查询参数（错误文案 / 单测共用一份，改这里就是改契约）。
pub const QUOTE_QUERY_PARAMS: [&str; 4] = ["from_currency", "to_currency", "date", "currency_code"];

/// 把原始查询串解析成 [`QuoteQuery`]：**未知参数一律 400**（不静默忽略）。
///
/// 为什么不用 `serde(deny_unknown_fields)`：serde 的拒绝走 axum 默认 rejection，
/// 返回体不是本项目的 `{code, message}` 信封，前端要另写一套解析。这里是显式校验，
/// 报文能点名「哪个参数不认识 + 允许哪些」。
pub fn parse_quote_query(params: &HashMap<String, String>) -> std::result::Result<QuoteQuery, ApiError> {
    let mut unknown: Vec<&str> = params
        .keys()
        .map(String::as_str)
        .filter(|name| !QUOTE_QUERY_PARAMS.contains(name))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        let listed = unknown
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join("、");
        return Err(bad_request(format!(
            "未知查询参数 {listed}：汇率预览只接受 from_currency / to_currency / date\
             （`currency_code` 是已废弃别名，等价于 from_currency）"
        )));
    }
    let alias = clean_query_value(params.get("currency_code"));
    let primary = clean_query_value(params.get("from_currency"));
    let from_currency = match (primary, alias) {
        (Some(primary), Some(alias)) if !primary.eq_ignore_ascii_case(&alias) => {
            return Err(bad_request(format!(
                "from_currency（`{primary}`）与废弃别名 currency_code（`{alias}`）不一致：\
                 只传 from_currency 即可"
            )));
        }
        (Some(primary), _) => Some(primary),
        (None, Some(alias)) => Some(alias),
        (None, None) => None,
    };
    Ok(QuoteQuery {
        from_currency,
        to_currency: clean_query_value(params.get("to_currency")),
        date: clean_query_value(params.get("date")),
    })
}

/// 查询串取值：去空白，空白等同「没传」。
fn clean_query_value(raw: Option<&String>) -> Option<String> {
    raw.map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

// ==================== HTTP 入口 ====================

/// 币种列表。
#[nik::permission(
    path = "/business/erp/currency/list",
    method = "GET",
    permission = "business:erp:currency:list"
)]
#[nik::get("/business/erp/currency/list")]
pub async fn currency_list_api(Query(filter): Query<CurrencyFilter>) -> ResResult<Vec<CurrencyRes>> {
    Ok(Res::ok(list_currency(filter).await?))
}

/// 币种下拉选项（只 ACTIVE）。
#[nik::permission(
    path = "/business/erp/currency/options",
    method = "GET",
    permission = "business:erp:currency:list"
)]
#[nik::get("/business/erp/currency/options")]
pub async fn currency_options_api() -> ResResult<Vec<CurrencyOptionRes>> {
    Ok(Res::ok(currency_options().await?))
}

/// 新建币种。
#[nik::permission(
    path = "/business/erp/currency",
    method = "POST",
    permission = "business:erp:currency:create"
)]
#[nik::post("/business/erp/currency")]
pub async fn currency_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateCurrencyReq>,
) -> ResResult<CurrencyRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_currency(operator, req).await?))
}

/// 编辑币种。
#[nik::permission(
    path = "/business/erp/currency/{code}",
    method = "PUT",
    permission = "business:erp:currency:update"
)]
#[nik::put("/business/erp/currency/{code}")]
pub async fn currency_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(code): Path<String>,
    Json(req): Json<UpdateCurrencyReq>,
) -> ResResult<CurrencyRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_currency(operator, &code, req).await?))
}

/// 立本位币。
#[nik::permission(
    path = "/business/erp/currency/{code}/base",
    method = "POST",
    permission = "business:erp:currency:base"
)]
#[nik::post("/business/erp/currency/{code}/base")]
pub async fn currency_set_base_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(code): Path<String>,
) -> ResResult<CurrencyRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(set_base_currency(operator, &code).await?))
}

/// 汇率分页。
#[nik::permission(
    path = "/business/erp/exchange-rate/page",
    method = "GET",
    permission = "business:erp:fx-rate:list"
)]
#[nik::get("/business/erp/exchange-rate/page")]
pub async fn rate_page_api(Query(filter): Query<RateFilter>) -> ResResult<PageData<Vec<RateRes>>> {
    Ok(Res::ok(page_rate(filter).await?))
}

/// 新建汇率。
#[nik::permission(
    path = "/business/erp/exchange-rate",
    method = "POST",
    permission = "business:erp:fx-rate:create"
)]
#[nik::post("/business/erp/exchange-rate")]
pub async fn rate_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateRateReq>,
) -> ResResult<RateRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_rate(operator, req).await?))
}

/// 编辑汇率。
#[nik::permission(
    path = "/business/erp/exchange-rate/{id}",
    method = "PUT",
    permission = "business:erp:fx-rate:update"
)]
#[nik::put("/business/erp/exchange-rate/{id}")]
pub async fn rate_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRateReq>,
) -> ResResult<RateRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_rate(operator, &id, req).await?))
}

/// 删除汇率。
#[nik::permission(
    path = "/business/erp/exchange-rate/{id}",
    method = "DELETE",
    permission = "business:erp:fx-rate:delete"
)]
#[nik::delete("/business/erp/exchange-rate/{id}")]
pub async fn rate_delete_api(Path(id): Path<String>) -> ResResult<()> {
    delete_rate(&id).await?;
    Ok(Res::ok(()))
}

/// 按月批量录入。
#[nik::permission(
    path = "/business/erp/exchange-rate/batch-month",
    method = "POST",
    permission = "business:erp:fx-rate:batch"
)]
#[nik::post("/business/erp/exchange-rate/batch-month")]
pub async fn rate_batch_month_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<BatchMonthReq>,
) -> ResResult<BatchMonthRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(batch_month_rates(operator, req).await?))
}

/// 汇率预览（只读）。**未知参数一律 400**（见 [`parse_quote_query`]）。
#[nik::permission(
    path = "/business/erp/exchange-rate/quote",
    method = "GET",
    permission = "business:erp:fx-rate:quote"
)]
#[nik::get("/business/erp/exchange-rate/quote")]
pub async fn rate_quote_api(Query(params): Query<HashMap<String, String>>) -> ResResult<QuoteRes> {
    let query = parse_quote_query(&params)?;
    Ok(Res::ok(
        quote_rate(
            query.from_currency.as_deref(),
            query.to_currency.as_deref(),
            query.date.as_deref(),
        )
        .await?,
    ))
}

// ==================== 单测（纯逻辑；真库行为由 curl 实测留证） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(raw: &str) -> Decimal {
        raw.parse().expect("测试用十进制字面量合法")
    }

    #[test]
    fn parse_code_normalizes_and_rejects_bad_input() {
        assert_eq!(parse_code(" usd ", "x").expect("合法"), "USD");
        assert!(parse_code("US", "x").is_err());
        assert!(parse_code("", "x").is_err());
    }

    #[test]
    fn resolve_status_defaults_to_active() {
        assert_eq!(resolve_status(None).expect("缺省"), model::STATUS_ACTIVE);
        assert_eq!(resolve_status(Some("  ")).expect("空串等同缺省"), model::STATUS_ACTIVE);
        assert_eq!(resolve_status(Some("INACTIVE")).expect("合法"), model::STATUS_INACTIVE);
        assert!(resolve_status(Some("DISABLED")).is_err());
    }

    #[test]
    fn resolve_rate_type_defaults_to_spot() {
        assert_eq!(resolve_rate_type(None).expect("缺省"), model::RATE_TYPE_SPOT);
        assert_eq!(
            resolve_rate_type(Some("MONTH_END")).expect("合法"),
            model::RATE_TYPE_MONTH_END
        );
        assert!(resolve_rate_type(Some("WEEKLY")).is_err());
    }

    #[test]
    fn resolve_rate_rejects_zero_and_scales() {
        assert!(resolve_rate(Decimal::ZERO).is_err());
        assert!(resolve_rate(dec("-1")).is_err());
        assert_eq!(
            resolve_rate(dec("7.123456789")).expect("合法").to_string(),
            "7.12345679"
        );
    }

    #[test]
    fn resolve_profile_trims_and_validates_length() {
        let (name, symbol, remark) = resolve_profile(" 美元 ", " $ ", Some("  央行中间价 ".to_string())).expect("合法");
        assert_eq!(name, "美元");
        assert_eq!(symbol, "$");
        assert_eq!(remark.as_deref(), Some("央行中间价"));

        assert!(resolve_profile("", "$", None).is_err());
        assert!(resolve_profile("美元", "", None).is_err());
        assert!(resolve_profile("美元", "$", Some("x".repeat(model::REMARK_MAX_LEN + 1))).is_err());
    }

    #[test]
    fn self_pair_and_positive_rate_are_enforced() {
        assert!(model::is_self_pair("CNY", "CNY"));
        assert!(!model::rate_allowed(Decimal::ZERO));
        assert!(model::rate_allowed(dec("7.1")));
    }

    #[test]
    fn permission_codes_are_the_ten_declared_ones() {
        // 与 V0316 的 sys_menu.auth_code 逐字对应：多一个 / 少一个都意味着有码授不出去
        assert_eq!(CURRENCY_ADMIN_PERMISSIONS.len(), 10);
        for code in CURRENCY_ADMIN_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "权限码必须是 {{模块}}:{{资源}}:{{动作}} 四段：{code}");
            assert_eq!(parts[0], "business");
            assert_eq!(parts[1], "erp");
        }
    }

    #[test]
    fn parse_uuid_accepts_valid_and_rejects_garbage() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string(), "id").expect("合法"), id);
        assert!(parse_uuid("not-a-uuid", "id").is_err());
        assert!(parse_uuid("  ", "id").is_err());
    }

    // ---------- 汇率预览参数（E2E-FIX-8 ③：参数名对齐 + 未知参数 400） ----------

    fn query(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn quote_query_accepts_aligned_param_names() {
        let parsed = parse_quote_query(&query(&[
            ("from_currency", " usd "),
            ("to_currency", "CNY"),
            ("date", "2026-11-09"),
        ]))
        .expect("对齐后的参数名必须被接受");
        // 这一层只去空白；大小写规范化由 `quote_rate` 的 `parse_code` 做（错误文案要回显原文）
        assert_eq!(
            parsed,
            QuoteQuery {
                from_currency: Some("usd".to_string()),
                to_currency: Some("CNY".to_string()),
                date: Some("2026-11-09".to_string()),
            }
        );
    }

    /// 卡面靶心：历史实现把 `?from_currency=USD` 静默忽略 → 回本位币 1。
    #[test]
    fn quote_query_rejects_unknown_or_misspelled_params() {
        let err = parse_quote_query(&query(&[("from_currnecy", "USD")])).expect_err("拼错必须 400");
        assert_eq!(err.status, 400, "未知参数必须是 400");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(
            err.msg.contains("from_currnecy") && err.msg.contains("from_currency"),
            "报文要点名「哪个不认识」+「允许哪些」：{}",
            err.msg
        );
        // 只有白名单里的名字才放行；`currency` / `to` 这类近似名同样 400
        for name in ["currency", "to", "exchange_rate"] {
            let rejected = parse_quote_query(&query(&[(name, "USD")]));
            assert!(
                rejected.is_err(),
                "`{name}` 不是合法参数，必须 400（当前返回 {rejected:?}）"
            );
        }
    }

    #[test]
    fn quote_query_keeps_currency_code_as_deprecated_alias() {
        // 老前端 / 老脚本还传 currency_code：等价于 from_currency
        let parsed = parse_quote_query(&query(&[("currency_code", "hkd")])).expect("别名仍可用");
        assert_eq!(parsed.from_currency.as_deref(), Some("hkd"));

        // 两个都给且一致 → 走 from_currency；不一致 → 400（不猜用户想要哪个）
        let same = parse_quote_query(&query(&[("from_currency", "USD"), ("currency_code", "usd")]))
            .expect("大小写不同但语义相同");
        assert_eq!(same.from_currency.as_deref(), Some("USD"));

        let err = parse_quote_query(&query(&[("from_currency", "USD"), ("currency_code", "HKD")]))
            .expect_err("两个名字给了不同币种，必须 400");
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("不一致"), "报文要说清冲突：{}", err.msg);
    }

    #[test]
    fn quote_query_treats_blank_as_absent() {
        let parsed = parse_quote_query(&query(&[("from_currency", "   "), ("to_currency", ""), ("date", " ")]))
            .expect("空白值等同没传（沿用老口径）");
        assert_eq!(parsed, QuoteQuery::default());
    }

    #[test]
    fn empty_query_is_allowed_and_defaults_to_base_currency() {
        let parsed = parse_quote_query(&HashMap::new()).expect("不带参数 = 本位币报价");
        assert_eq!(parsed, QuoteQuery::default());
    }
}

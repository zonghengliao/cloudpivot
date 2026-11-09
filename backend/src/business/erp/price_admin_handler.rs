//! 价格维护接口层（销售链 · SO-34 / ARCH-7 §15.3.2）：协议价 + 阶梯价 CRUD，共 8 个接口。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/price/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/price/agreed/page` | `business:erp:cust-price:list` | 协议价分页（客户 / 物料 / 状态 / 关键字） |
//! | POST | `/business/erp/price/agreed` | `business:erp:cust-price:create` | 新建协议价 |
//! | PUT | `/business/erp/price/agreed/{id}` | `business:erp:cust-price:update` | 编辑（部分更新） |
//! | DELETE | `/business/erp/price/agreed/{id}` | `business:erp:cust-price:delete` | 软删 |
//! | GET | `/business/erp/price/tier/page` | `business:erp:price-tier:list` | 阶梯 / 促销价分页 |
//! | POST | `/business/erp/price/tier` | `business:erp:price-tier:create` | 新建档位 |
//! | PUT | `/business/erp/price/tier/{id}` | `business:erp:price-tier:update` | 编辑档位 |
//! | DELETE | `/business/erp/price/tier/{id}` | `business:erp:price-tier:delete` | 软删档位 |
//!
//! # 与 SO-33 取价接口的分工（别混）
//!
//! * 本文件是**配置维护**（写）：维护「该收多少」的规则；
//! * [`crate::business::erp::price_handler`] 是**取价解析**（读）：按 5 级优先级回答「此刻该多少」。
//!   两者读同一批表、共享同一套域值常量（[`price_admin_model::STATUSES`] 等），
//!   但**不共享 SQL**：维护侧要 joins 名称、要分页，取价侧只做窄列粗筛。
//!
//! # 四条入口校验（都在本文件，DDL 的 CHECK 只是兜底）
//!
//! 1. `price > 0`（`0` 元行是单据能力，价格表里出现 0 会让取价静默命中 0 元价）；
//! 2. `min_qty >= 0`；档位 `max_qty` 为空或**严格大于** `min_qty`（右开区间，相等即空区间）；
//! 3. 有效期 `effective_to >= effective_from`（两端都含，与取价侧 `date_covered` 同口径）；
//! 4. 唯一键冲突先查一次给人话 **409**：
//!    协议价 `(客户, 物料, 起订量, 生效日)`、档位 `(物料, 客户含 NULL, 类型, 起订量, 生效日)`。
//!
//! # 不碰的东西
//!
//! 价格是**配置**不是单据：不生成凭证、不写库存三本账、不进流程引擎、不注册
//! `DocKind` / `LEGAL_EDGES`（`status` 只是「启用 / 停用」开关，没有状态机迁移边）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::price_admin_model::{self as model, AgreedRow, AgreedWrite, TierRow, TierWrite};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0308 的 sys_menu.auth_code 逐字一致） ====================

/// 协议价列表。
pub const PERM_AGREED_LIST: &str = "business:erp:cust-price:list";
/// 协议价新建。
pub const PERM_AGREED_CREATE: &str = "business:erp:cust-price:create";
/// 协议价编辑。
pub const PERM_AGREED_UPDATE: &str = "business:erp:cust-price:update";
/// 协议价删除。
pub const PERM_AGREED_DELETE: &str = "business:erp:cust-price:delete";
/// 档位列表。
pub const PERM_TIER_LIST: &str = "business:erp:price-tier:list";
/// 档位新建。
pub const PERM_TIER_CREATE: &str = "business:erp:price-tier:create";
/// 档位编辑。
pub const PERM_TIER_UPDATE: &str = "business:erp:price-tier:update";
/// 档位删除。
pub const PERM_TIER_DELETE: &str = "business:erp:price-tier:delete";

/// 价格维护域全部权限码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const PRICE_ADMIN_PERMISSIONS: [&str; 8] = [
    PERM_AGREED_LIST,
    PERM_AGREED_CREATE,
    PERM_AGREED_UPDATE,
    PERM_AGREED_DELETE,
    PERM_TIER_LIST,
    PERM_TIER_CREATE,
    PERM_TIER_UPDATE,
    PERM_TIER_DELETE,
];

// ==================== 错误出口 ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// `404 + 10002`：价格行不存在 / 已软删。
fn not_found(what: &str) -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, format!("{what}不存在或已删除"))
}

/// `409 + 10003`：唯一键冲突（人话点名是哪组键）。
fn duplicate(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 写路径 DB 错误收口：唯一索引 / CHECK 兜底也翻成人话，其余仍是 500。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::UniqueViolation { constraint, .. } => duplicate(format!(
            "{ctx}：同一组键（客户 / 物料 / 起订量 / 生效日）已存在{}",
            constraint
                .as_deref()
                .map(|name| format!("（{name}）"))
                .unwrap_or_default()
        )),
        DbError::CheckViolation { constraint, .. } => bad_request(format!(
            "{ctx}：数据不满足约束{}（价格 > 0 / 起订量 >= 0 / 档位上界 > 下界 / 有效期不减）",
            constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
        )),
        other => utils::db_error(other, ctx),
    }
}

/// UUID 入参：空 → 400；非法 → 400（点名是哪个参数）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    Uuid::parse_str(text).map_err(|_| bad_request(format!("{field} 不是合法 UUID：`{text}`")))
}

/// 可选 UUID（查询串）：空串 / 缺省 → `None`（= 该维度不筛）。
fn parse_optional_uuid(raw: Option<&str>, field: &str) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_uuid(text, field)?)),
    }
}

/// 日期入参 `YYYY-MM-DD`。
fn parse_date(text: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|error| bad_request(format!("{field} 不是合法日期（YYYY-MM-DD）：{error}")))
}

/// 北京时间今天（与全仓 `voucher::today_cst` 同一把尺）。
fn today_cst() -> NaiveDate {
    crate::business::erp::voucher::today_cst(Utc::now())
}

/// 状态域校验（缺省 = `ACTIVE`）。
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

/// 分页筛选里的 `status`：非法值 → 400（静默忽略会把「停用价」筛成「全部」，是假数据）。
fn check_status_filter(status: Option<&str>) -> std::result::Result<(), ApiError> {
    match status.map(str::trim).filter(|text| !text.is_empty()) {
        Some(value) if !model::is_valid_status(value) => Err(bad_request(format!(
            "status 必须是 ACTIVE / INACTIVE 之一（当前 `{value}`）"
        ))),
        _ => Ok(()),
    }
}

// ==================== 契约：协议价 ====================

/// 协议价分页筛选。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgreedFilter {
    /// 客户 id（精确）。
    pub customer_id: Option<String>,
    /// 物料 id（精确）。
    pub material_id: Option<String>,
    /// 状态（`ACTIVE` / `INACTIVE`）。
    pub status: Option<String>,
    /// 关键字（客户编码/名称、物料编码/名称，模糊）。
    pub keyword: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

impl Default for AgreedFilter {
    fn default() -> Self {
        Self {
            customer_id: None,
            material_id: None,
            status: None,
            keyword: None,
            page: 1,
            page_size: model::DEFAULT_PAGE_SIZE,
        }
    }
}

/// 档位分页筛选。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TierFilter {
    /// 物料 id（精确）。
    pub material_id: Option<String>,
    /// 客户 id（精确；`NULL` 通用档位只能靠不传本参数看到）。
    pub customer_id: Option<String>,
    /// 档位类型（`TIER` / `PROMO`）。
    pub tier_type: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 关键字。
    pub keyword: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

impl Default for TierFilter {
    fn default() -> Self {
        Self {
            material_id: None,
            customer_id: None,
            tier_type: None,
            status: None,
            keyword: None,
            page: 1,
            page_size: model::DEFAULT_PAGE_SIZE,
        }
    }
}

/// 新建协议价。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateAgreedReq {
    /// 客户 id（必填）。
    pub customer_id: String,
    /// 物料 id（必填）。
    pub material_id: String,
    /// 含税协议单价（> 0）。
    pub price: Decimal,
    /// 起订量（缺省 0 = 不限制）。
    #[serde(default)]
    pub min_qty: Option<Decimal>,
    /// 生效日（缺省 = 今天 CST）。
    #[serde(default)]
    pub effective_from: Option<String>,
    /// 失效日（缺省 = 长期有效）。
    #[serde(default)]
    pub effective_to: Option<String>,
    /// 状态（缺省 `ACTIVE`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateAgreedReq {
    /// 入参 → 写入值（校验全在这一步）。
    fn to_write(&self) -> std::result::Result<AgreedWrite, ApiError> {
        let customer_id = parse_uuid(&self.customer_id, "customer_id")?;
        let material_id = parse_uuid(&self.material_id, "material_id")?;
        let min_qty = self.min_qty.unwrap_or(Decimal::ZERO);
        check_price(self.price)?;
        check_min_qty(min_qty)?;
        let effective_from = match self.effective_from.as_deref() {
            Some(text) if !text.trim().is_empty() => parse_date(text, "effective_from")?,
            _ => today_cst(),
        };
        let effective_to = optional_date(self.effective_to.as_deref(), "effective_to")?;
        check_dates(effective_from, effective_to)?;
        Ok(AgreedWrite {
            customer_id,
            material_id,
            price: self.price,
            min_qty,
            effective_from,
            effective_to,
            status: resolve_status(self.status.as_deref())?,
            remark: model::normalize_remark(self.remark.clone()),
        })
    }
}

/// 编辑协议价（**部分更新**：不传 = 不改；可空列传空串 = 清空）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAgreedReq {
    /// 客户 id。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 物料 id。
    #[serde(default)]
    pub material_id: Option<String>,
    /// 单价。
    #[serde(default)]
    pub price: Option<Decimal>,
    /// 起订量。
    #[serde(default)]
    pub min_qty: Option<Decimal>,
    /// 生效日。
    #[serde(default)]
    pub effective_from: Option<String>,
    /// 失效日（传空串 = 清空 = 长期有效）。
    #[serde(default)]
    pub effective_to: Option<String>,
    /// 状态。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

impl UpdateAgreedReq {
    /// 与当前行合并（不传的沿用当前值），再整体校验。
    fn merge(&self, current: &AgreedRow) -> std::result::Result<AgreedWrite, ApiError> {
        let customer_id = match self.customer_id.as_deref().map(str::trim) {
            None | Some("") => current.customer_id,
            Some(text) => parse_uuid(text, "customer_id")?,
        };
        let material_id = match self.material_id.as_deref().map(str::trim) {
            None | Some("") => current.material_id,
            Some(text) => parse_uuid(text, "material_id")?,
        };
        let price = self.price.unwrap_or(current.price);
        let min_qty = self.min_qty.unwrap_or(current.min_qty);
        check_price(price)?;
        check_min_qty(min_qty)?;
        let effective_from = match self.effective_from.as_deref().map(str::trim) {
            None | Some("") => current.effective_from,
            Some(text) => parse_date(text, "effective_from")?,
        };
        let effective_to = match self.effective_to.as_deref() {
            None => current.effective_to,
            Some(text) => optional_date(Some(text), "effective_to")?,
        };
        check_dates(effective_from, effective_to)?;
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => current.status.clone(),
            Some(_) => resolve_status(self.status.as_deref())?,
        };
        Ok(AgreedWrite {
            customer_id,
            material_id,
            price,
            min_qty,
            effective_from,
            effective_to,
            status,
            remark: match self.remark.clone() {
                None => current.remark.clone(),
                Some(value) => model::normalize_remark(Some(value)),
            },
        })
    }
}

/// 协议价出参（列表 / 新建 / 编辑同构）。
#[derive(Debug, Clone, Serialize)]
pub struct AgreedRes {
    /// 主键。
    pub id: String,
    /// 客户 id。
    pub customer_id: String,
    /// 客户编码（客户被物理删时 `null`）。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 物料规格。
    pub material_spec: Option<String>,
    /// 含税单价。
    pub price: Decimal,
    /// 起订量。
    pub min_qty: Decimal,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 失效日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: nik::prelude::chrono::DateTime<Utc>,
    /// 更新时间。
    pub updated_at: nik::prelude::chrono::DateTime<Utc>,
}

impl From<&AgreedRow> for AgreedRes {
    fn from(row: &AgreedRow) -> Self {
        Self {
            id: row.id.to_string(),
            customer_id: row.customer_id.to_string(),
            customer_code: row.customer_code.clone(),
            customer_name: row.customer_name.clone(),
            material_id: row.material_id.to_string(),
            material_code: row.material_code.clone(),
            material_name: row.material_name.clone(),
            material_spec: row.material_spec.clone(),
            price: row.price,
            min_qty: row.min_qty,
            effective_from: row.effective_from,
            effective_to: row.effective_to,
            status: row.status.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 契约：阶梯价 ====================

/// 新建档位。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateTierReq {
    /// 物料 id（必填）。
    pub material_id: String,
    /// 客户 id（缺省 / 空串 = 通用档位）。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 档位类型（缺省 `TIER`；促销用 `PROMO`）。
    #[serde(default)]
    pub tier_type: Option<String>,
    /// 区间下界（含，必填）。
    pub min_qty: Decimal,
    /// 区间上界（不含，缺省 = 无上限）。
    #[serde(default)]
    pub max_qty: Option<Decimal>,
    /// 含税单价（> 0）。
    pub price: Decimal,
    /// 生效日（缺省 = 今天 CST）。
    #[serde(default)]
    pub effective_from: Option<String>,
    /// 失效日。
    #[serde(default)]
    pub effective_to: Option<String>,
    /// 状态。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateTierReq {
    /// 入参 → 写入值。
    fn to_write(&self) -> std::result::Result<TierWrite, ApiError> {
        let material_id = parse_uuid(&self.material_id, "material_id")?;
        let customer_id = parse_optional_uuid(self.customer_id.as_deref(), "customer_id")?;
        let tier_type = resolve_tier_type(self.tier_type.as_deref())?;
        check_min_qty(self.min_qty)?;
        check_max_qty(self.min_qty, self.max_qty)?;
        check_price(self.price)?;
        let effective_from = match self.effective_from.as_deref() {
            Some(text) if !text.trim().is_empty() => parse_date(text, "effective_from")?,
            _ => today_cst(),
        };
        let effective_to = optional_date(self.effective_to.as_deref(), "effective_to")?;
        check_dates(effective_from, effective_to)?;
        Ok(TierWrite {
            material_id,
            customer_id,
            tier_type,
            min_qty: self.min_qty,
            max_qty: self.max_qty,
            price: self.price,
            effective_from,
            effective_to,
            status: resolve_status(self.status.as_deref())?,
            remark: model::normalize_remark(self.remark.clone()),
        })
    }
}

/// 编辑档位（部分更新）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateTierReq {
    /// 物料 id。
    #[serde(default)]
    pub material_id: Option<String>,
    /// 客户 id（传空串 = 改成通用档位）。
    #[serde(default)]
    pub customer_id: Option<String>,
    /// 档位类型。
    #[serde(default)]
    pub tier_type: Option<String>,
    /// 区间下界。
    #[serde(default)]
    pub min_qty: Option<Decimal>,
    /// 区间上界（传空串 → 前端发 `null` 表示无上限；`null` 与「不传」在 JSON 里无法区分，
    /// 故清空请传字符串 `"none"`，见 [`UpdateTierReq::merge`] 的说明）。
    #[serde(default)]
    pub max_qty: Option<Decimal>,
    /// 单价。
    #[serde(default)]
    pub price: Option<Decimal>,
    /// 生效日。
    #[serde(default)]
    pub effective_from: Option<String>,
    /// 失效日（空串 = 清空）。
    #[serde(default)]
    pub effective_to: Option<String>,
    /// 状态。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注（空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

impl UpdateTierReq {
    /// 与当前行合并。
    ///
    /// `max_qty` 的「清空」语义：JSON 的 `null` 与「字段缺省」反序列化后都是 `None`，
    /// 故本接口约定 — `max_qty` **缺省 = 不改**；要清空成「无上限」请显式传 `"0"`？
    /// 不 —— `0` 会被 `max_qty > min_qty` 拒。最终口径：**传 `-1` 表示清空**（负数在
    /// `min_qty >= 0` 下永不是合法上界，不会与真实值混淆），合并时转 `None`。
    /// 前端在编辑弹窗里勾「无上限」时按此发送。
    fn merge(&self, current: &TierRow) -> std::result::Result<TierWrite, ApiError> {
        let material_id = match self.material_id.as_deref().map(str::trim) {
            None | Some("") => current.material_id,
            Some(text) => parse_uuid(text, "material_id")?,
        };
        // 客户：缺省 = 不改；空串 = 清空成通用档位
        let customer_id = match self.customer_id.as_deref() {
            None => current.customer_id,
            Some(text) => parse_optional_uuid(Some(text), "customer_id")?,
        };
        let tier_type = match self.tier_type.as_deref().map(str::trim) {
            None | Some("") => current.tier_type.clone(),
            Some(_) => resolve_tier_type(self.tier_type.as_deref())?,
        };
        let min_qty = self.min_qty.unwrap_or(current.min_qty);
        check_min_qty(min_qty)?;
        let max_qty = match self.max_qty {
            None => current.max_qty,
            Some(value) if value < Decimal::ZERO => None, // -1 = 清空成无上限
            Some(value) => Some(value),
        };
        check_max_qty(min_qty, max_qty)?;
        let price = self.price.unwrap_or(current.price);
        check_price(price)?;
        let effective_from = match self.effective_from.as_deref().map(str::trim) {
            None | Some("") => current.effective_from,
            Some(text) => parse_date(text, "effective_from")?,
        };
        let effective_to = match self.effective_to.as_deref() {
            None => current.effective_to,
            Some(text) => optional_date(Some(text), "effective_to")?,
        };
        check_dates(effective_from, effective_to)?;
        let status = match self.status.as_deref().map(str::trim) {
            None | Some("") => current.status.clone(),
            Some(_) => resolve_status(self.status.as_deref())?,
        };
        Ok(TierWrite {
            material_id,
            customer_id,
            tier_type,
            min_qty,
            max_qty,
            price,
            effective_from,
            effective_to,
            status,
            remark: match self.remark.clone() {
                None => current.remark.clone(),
                Some(value) => model::normalize_remark(Some(value)),
            },
        })
    }
}

/// 档位出参。
#[derive(Debug, Clone, Serialize)]
pub struct TierRes {
    /// 主键。
    pub id: String,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 物料规格。
    pub material_spec: Option<String>,
    /// 客户 id；`null` = 通用档位。
    pub customer_id: Option<String>,
    /// 客户编码。
    pub customer_code: Option<String>,
    /// 客户名称。
    pub customer_name: Option<String>,
    /// `TIER` / `PROMO`。
    pub tier_type: String,
    /// 区间下界。
    pub min_qty: Decimal,
    /// 区间上界。
    pub max_qty: Option<Decimal>,
    /// 含税单价。
    pub price: Decimal,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 失效日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: nik::prelude::chrono::DateTime<Utc>,
    /// 更新时间。
    pub updated_at: nik::prelude::chrono::DateTime<Utc>,
}

impl From<&TierRow> for TierRes {
    fn from(row: &TierRow) -> Self {
        Self {
            id: row.id.to_string(),
            material_id: row.material_id.to_string(),
            material_code: row.material_code.clone(),
            material_name: row.material_name.clone(),
            material_spec: row.material_spec.clone(),
            customer_id: row.customer_id.map(|id| id.to_string()),
            customer_code: row.customer_code.clone(),
            customer_name: row.customer_name.clone(),
            tier_type: row.tier_type.clone(),
            min_qty: row.min_qty,
            max_qty: row.max_qty,
            price: row.price,
            effective_from: row.effective_from,
            effective_to: row.effective_to,
            status: row.status.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 校验原语（纯函数，可单测） ====================

/// 单价 `> 0`。
fn check_price(price: Decimal) -> std::result::Result<(), ApiError> {
    if model::price_allowed(price) {
        return Ok(());
    }
    Err(bad_request(format!(
        "price 必须大于 0（当前 `{price}`）；免费品请在单据行填 0 元，不要写进价格表"
    )))
}

/// 起订量 `>= 0`。
fn check_min_qty(min_qty: Decimal) -> std::result::Result<(), ApiError> {
    if model::min_qty_allowed(min_qty) {
        return Ok(());
    }
    Err(bad_request(format!(
        "min_qty 不能为负（当前 `{min_qty}`）；0 表示不限制起订量"
    )))
}

/// 档位上界：空 = 无上限；非空必须严格大于下界。
fn check_max_qty(min_qty: Decimal, max_qty: Option<Decimal>) -> std::result::Result<(), ApiError> {
    if model::max_qty_allowed(min_qty, max_qty) {
        return Ok(());
    }
    Err(bad_request(format!(
        "max_qty 必须大于 min_qty（当前 min_qty=`{min_qty}`、max_qty=`{}`）；无上限请留空",
        max_qty.map(|v| v.to_string()).unwrap_or_default()
    )))
}

/// 有效期：两端都含，`to < from` 拒。
fn check_dates(from: NaiveDate, to: Option<NaiveDate>) -> std::result::Result<(), ApiError> {
    if model::date_range_allowed(from, to) {
        return Ok(());
    }
    Err(bad_request(format!(
        "effective_to 不能早于 effective_from（{from} > {}）",
        to.map(|v| v.to_string()).unwrap_or_default()
    )))
}

/// 档位类型（缺省 `TIER`）。
fn resolve_tier_type(value: Option<&str>) -> std::result::Result<String, ApiError> {
    let text = value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or(model::TIER_TYPE_TIER)
        .to_string();
    if !model::is_valid_tier_type(&text) {
        return Err(bad_request(format!(
            "tier_type 必须是 TIER（阶梯）/ PROMO（促销）之一（当前 `{text}`）"
        )));
    }
    Ok(text)
}

/// 可空日期：缺省 / 空串 → `None`。
fn optional_date(value: Option<&str>, field: &str) -> std::result::Result<Option<NaiveDate>, ApiError> {
    match value.map(str::trim).filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => Ok(Some(parse_date(text, field)?)),
    }
}

// ==================== 业务编排（无 service 层） ====================

/// 协议价分页。
pub async fn page_agreed(filter: AgreedFilter) -> std::result::Result<PageData<Vec<AgreedRes>>, ApiError> {
    let customer_id = parse_optional_uuid(filter.customer_id.as_deref(), "customer_id")?;
    let material_id = parse_optional_uuid(filter.material_id.as_deref(), "material_id")?;
    check_status_filter(filter.status.as_deref())?;
    let (rows, total) = model::agreed_page(
        customer_id,
        material_id,
        filter.status.as_deref(),
        filter.keyword.as_deref(),
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询客户协议价"))?;
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list: rows.iter().map(AgreedRes::from).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 新建协议价。
pub async fn create_agreed(operator: Option<Uuid>, req: CreateAgreedReq) -> std::result::Result<AgreedRes, ApiError> {
    let write = req.to_write()?;
    ensure_material(write.material_id).await?;
    ensure_customer(write.customer_id).await?;
    if model::agreed_taken(
        write.customer_id,
        write.material_id,
        write.min_qty,
        write.effective_from,
        None,
    )
    .await
    .map_err(|error| utils::db_error(error, "校验协议价唯一性"))?
    {
        return Err(agreed_duplicate(&write));
    }
    let id = Uuid::now_v7();
    model::agreed_insert(id, &write, operator)
        .await
        .map_err(|error| write_error(error, "新建客户协议价"))?;
    let row = model::agreed_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读客户协议价"))?
        .ok_or_else(|| not_found("客户协议价"))?;
    Ok(AgreedRes::from(&row))
}

/// 编辑协议价。
pub async fn update_agreed(
    operator: Option<Uuid>,
    id_raw: &str,
    req: UpdateAgreedReq,
) -> std::result::Result<AgreedRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::agreed_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑协议价"))?
        .ok_or_else(|| not_found("客户协议价"))?;
    let write = req.merge(&current)?;
    ensure_material(write.material_id).await?;
    ensure_customer(write.customer_id).await?;
    if model::agreed_taken(
        write.customer_id,
        write.material_id,
        write.min_qty,
        write.effective_from,
        Some(id),
    )
    .await
    .map_err(|error| utils::db_error(error, "校验协议价唯一性"))?
    {
        return Err(agreed_duplicate(&write));
    }
    let affected = model::agreed_update(id, &write, operator)
        .await
        .map_err(|error| write_error(error, "编辑客户协议价"))?;
    if affected == 0 {
        return Err(not_found("客户协议价"));
    }
    let row = model::agreed_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读客户协议价"))?
        .ok_or_else(|| not_found("客户协议价"))?;
    Ok(AgreedRes::from(&row))
}

/// 软删协议价。
pub async fn delete_agreed(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = model::agreed_soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "软删客户协议价"))?;
    if affected == 0 {
        return Err(not_found("客户协议价"));
    }
    Ok(())
}

/// 档位分页。
pub async fn page_tier(filter: TierFilter) -> std::result::Result<PageData<Vec<TierRes>>, ApiError> {
    let material_id = parse_optional_uuid(filter.material_id.as_deref(), "material_id")?;
    let customer_id = parse_optional_uuid(filter.customer_id.as_deref(), "customer_id")?;
    if let Some(tier_type) = filter.tier_type.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        resolve_tier_type(Some(tier_type))?;
    }
    check_status_filter(filter.status.as_deref())?;
    let (rows, total) = model::tier_page(
        material_id,
        customer_id,
        filter.tier_type.as_deref(),
        filter.status.as_deref(),
        filter.keyword.as_deref(),
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询阶梯价"))?;
    let (page, page_size) = model::normalize_page(filter.page, filter.page_size);
    Ok(PageData {
        list: rows.iter().map(TierRes::from).collect(),
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 新建档位。
pub async fn create_tier(operator: Option<Uuid>, req: CreateTierReq) -> std::result::Result<TierRes, ApiError> {
    let write = req.to_write()?;
    ensure_material(write.material_id).await?;
    if let Some(customer_id) = write.customer_id {
        ensure_customer(customer_id).await?;
    }
    if model::tier_taken(
        write.material_id,
        write.customer_id,
        &write.tier_type,
        write.min_qty,
        write.effective_from,
        None,
    )
    .await
    .map_err(|error| utils::db_error(error, "校验阶梯价唯一性"))?
    {
        return Err(tier_duplicate(&write));
    }
    let id = Uuid::now_v7();
    model::tier_insert(id, &write, operator)
        .await
        .map_err(|error| write_error(error, "新建阶梯价"))?;
    let row = model::tier_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读阶梯价"))?
        .ok_or_else(|| not_found("阶梯价"))?;
    Ok(TierRes::from(&row))
}

/// 编辑档位。
pub async fn update_tier(
    operator: Option<Uuid>,
    id_raw: &str,
    req: UpdateTierReq,
) -> std::result::Result<TierRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let current = model::tier_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑阶梯价"))?
        .ok_or_else(|| not_found("阶梯价"))?;
    let write = req.merge(&current)?;
    ensure_material(write.material_id).await?;
    if let Some(customer_id) = write.customer_id {
        ensure_customer(customer_id).await?;
    }
    if model::tier_taken(
        write.material_id,
        write.customer_id,
        &write.tier_type,
        write.min_qty,
        write.effective_from,
        Some(id),
    )
    .await
    .map_err(|error| utils::db_error(error, "校验阶梯价唯一性"))?
    {
        return Err(tier_duplicate(&write));
    }
    let affected = model::tier_update(id, &write, operator)
        .await
        .map_err(|error| write_error(error, "编辑阶梯价"))?;
    if affected == 0 {
        return Err(not_found("阶梯价"));
    }
    let row = model::tier_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读阶梯价"))?
        .ok_or_else(|| not_found("阶梯价"))?;
    Ok(TierRes::from(&row))
}

/// 软删档位。
pub async fn delete_tier(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = model::tier_soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "软删阶梯价"))?;
    if affected == 0 {
        return Err(not_found("阶梯价"));
    }
    Ok(())
}

/// 物料必须存在（软删不算）：否则硬外键会以 500 收场，前端看不出「选错了物料」。
async fn ensure_material(material_id: Uuid) -> std::result::Result<(), ApiError> {
    if model::material_exists(material_id)
        .await
        .map_err(|error| utils::db_error(error, "校验物料存在性"))?
    {
        return Ok(());
    }
    Err(ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "物料不存在或已删除，无法配置价格".to_string(),
    ))
}

/// 客户必须存在（软删不算）。
async fn ensure_customer(customer_id: Uuid) -> std::result::Result<(), ApiError> {
    if model::customer_exists(customer_id)
        .await
        .map_err(|error| utils::db_error(error, "校验客户存在性"))?
    {
        return Ok(());
    }
    Err(ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "客户不存在或已删除，无法配置协议价".to_string(),
    ))
}

/// 协议价撞唯一键的人话（点名四列，让用户知道该改哪个）。
fn agreed_duplicate(write: &AgreedWrite) -> ApiError {
    duplicate(format!(
        "该客户在此物料上已有起订量 `{}`、生效日 `{}` 的协议价（同一组键只能一条；\
         要改价请编辑原行，或换一个起订量 / 生效日）",
        write.min_qty, write.effective_from
    ))
}

/// 档位撞唯一键的人话。
fn tier_duplicate(write: &TierWrite) -> ApiError {
    duplicate(format!(
        "同物料同档位（{}）上已有起订量 `{}`、生效日 `{}` 的档位价（同一组键只能一条）",
        write.tier_type, write.min_qty, write.effective_from
    ))
}

// ==================== HTTP 入口 ====================

/// 协议价分页。
#[nik::permission(
    path = "/business/erp/price/agreed/page",
    method = "GET",
    permission = "business:erp:cust-price:list"
)]
#[nik::get("/business/erp/price/agreed/page")]
pub async fn agreed_page_api(Query(filter): Query<AgreedFilter>) -> ResResult<PageData<Vec<AgreedRes>>> {
    Ok(Res::ok(page_agreed(filter).await?))
}

/// 新建协议价。
#[nik::permission(
    path = "/business/erp/price/agreed",
    method = "POST",
    permission = "business:erp:cust-price:create"
)]
#[nik::post("/business/erp/price/agreed")]
pub async fn agreed_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateAgreedReq>,
) -> ResResult<AgreedRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_agreed(operator, req).await?))
}

/// 编辑协议价。
#[nik::permission(
    path = "/business/erp/price/agreed/{id}",
    method = "PUT",
    permission = "business:erp:cust-price:update"
)]
#[nik::put("/business/erp/price/agreed/{id}")]
pub async fn agreed_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAgreedReq>,
) -> ResResult<AgreedRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_agreed(operator, &id, req).await?))
}

/// 软删协议价。
#[nik::permission(
    path = "/business/erp/price/agreed/{id}",
    method = "DELETE",
    permission = "business:erp:cust-price:delete"
)]
#[nik::delete("/business/erp/price/agreed/{id}")]
pub async fn agreed_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_agreed(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 档位分页。
#[nik::permission(
    path = "/business/erp/price/tier/page",
    method = "GET",
    permission = "business:erp:price-tier:list"
)]
#[nik::get("/business/erp/price/tier/page")]
pub async fn tier_page_api(Query(filter): Query<TierFilter>) -> ResResult<PageData<Vec<TierRes>>> {
    Ok(Res::ok(page_tier(filter).await?))
}

/// 新建档位。
#[nik::permission(
    path = "/business/erp/price/tier",
    method = "POST",
    permission = "business:erp:price-tier:create"
)]
#[nik::post("/business/erp/price/tier")]
pub async fn tier_create_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateTierReq>,
) -> ResResult<TierRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_tier(operator, req).await?))
}

/// 编辑档位。
#[nik::permission(
    path = "/business/erp/price/tier/{id}",
    method = "PUT",
    permission = "business:erp:price-tier:update"
)]
#[nik::put("/business/erp/price/tier/{id}")]
pub async fn tier_update_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateTierReq>,
) -> ResResult<TierRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_tier(operator, &id, req).await?))
}

/// 软删档位。
#[nik::permission(
    path = "/business/erp/price/tier/{id}",
    method = "DELETE",
    permission = "business:erp:price-tier:delete"
)]
#[nik::delete("/business/erp/price/tier/{id}")]
pub async fn tier_delete_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_tier(operator, &id).await?;
    Ok(Res::ok(()))
}

// ==================== 单测（纯逻辑；真库行为由 curl 实测留证） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    fn create_agreed_req(price: &str) -> CreateAgreedReq {
        CreateAgreedReq {
            customer_id: "5b000000-0000-4000-8000-000000000001".to_string(),
            material_id: "5b000000-0000-4000-8000-000000000002".to_string(),
            price: dec(price),
            min_qty: None,
            effective_from: Some("2026-11-01".to_string()),
            effective_to: None,
            status: None,
            remark: None,
        }
    }

    /// 权限码：8 个、无重复、四段式 `business:erp:<资源>:<动作>`。
    #[test]
    fn permission_codes_shape() {
        assert_eq!(PRICE_ADMIN_PERMISSIONS.len(), 8);
        let mut sorted: Vec<&str> = PRICE_ADMIN_PERMISSIONS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 8, "权限码不能重复");
        for code in PRICE_ADMIN_PERMISSIONS {
            let parts: Vec<&str> = code.split(':').collect();
            assert_eq!(parts.len(), 4, "{code} 必须是 business:erp:<资源>:<动作>");
            assert_eq!(parts[0], "business");
            assert_eq!(parts[1], "erp");
            assert!(
                code.starts_with("business:erp:cust-price:") || code.starts_with("business:erp:price-tier:"),
                "{code} 资源段只能是 cust-price / price-tier"
            );
        }
    }

    /// 路由路径不带 `/api` 前缀（由 `[router].prefix` 注入）。
    #[test]
    fn route_paths_have_no_api_prefix() {
        for path in [
            "/business/erp/price/agreed/page",
            "/business/erp/price/agreed",
            "/business/erp/price/agreed/{id}",
            "/business/erp/price/tier/page",
            "/business/erp/price/tier",
            "/business/erp/price/tier/{id}",
        ] {
            assert!(!path.starts_with("/api"), "{path} 不能带 /api");
            assert!(path.starts_with("/business/erp/price/"));
        }
    }

    /// 单价：0 / 负数 → 400；正数通过。
    #[test]
    fn price_must_be_positive_at_entry() {
        assert!(create_agreed_req("12.5").to_write().is_ok());
        let zero = create_agreed_req("0").to_write().unwrap_err();
        assert_eq!(zero.status, 400);
        assert!(zero.msg.contains("price"));
        assert!(create_agreed_req("-1").to_write().is_err());
    }

    /// 起订量负数 → 400；0 合法；缺省 = 0。
    #[test]
    fn min_qty_defaults_to_zero_and_rejects_negative() {
        let ok = create_agreed_req("1").to_write().unwrap();
        assert_eq!(ok.min_qty, Decimal::ZERO);
        let mut negative = create_agreed_req("1");
        negative.min_qty = Some(dec("-0.5"));
        assert!(negative.to_write().is_err());
    }

    /// 有效期：to < from → 400；to == from 合法（只当天有效）。
    #[test]
    fn effective_dates_order_is_enforced() {
        let mut bad = create_agreed_req("1");
        bad.effective_to = Some("2026-10-31".to_string());
        let err = bad.to_write().unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("effective_to"));

        let mut same = create_agreed_req("1");
        same.effective_to = Some("2026-11-01".to_string());
        assert!(same.to_write().is_ok());
    }

    /// 状态域：缺省 ACTIVE；非法值 400。
    #[test]
    fn status_defaults_to_active() {
        assert_eq!(resolve_status(None).unwrap(), "ACTIVE");
        assert_eq!(resolve_status(Some(" ")).unwrap(), "ACTIVE");
        assert_eq!(resolve_status(Some("INACTIVE")).unwrap(), "INACTIVE");
        assert!(resolve_status(Some("DISABLED")).is_err());
    }

    /// 非法 UUID → 400 且点名参数（不能变成 500 外键错）。
    #[test]
    fn invalid_uuid_is_bad_request() {
        let mut bad = create_agreed_req("1");
        bad.material_id = "not-a-uuid".to_string();
        let err = bad.to_write().unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("material_id"));
    }

    /// 档位：`max_qty <= min_qty` → 400；为空 = 无上限。
    #[test]
    fn tier_max_qty_must_exceed_min_qty() {
        let base = CreateTierReq {
            material_id: "5b000000-0000-4000-8000-000000000002".to_string(),
            customer_id: None,
            tier_type: None,
            min_qty: dec("10"),
            max_qty: Some(dec("10")),
            price: dec("9"),
            effective_from: Some("2026-11-01".to_string()),
            effective_to: None,
            status: None,
            remark: None,
        };
        assert!(base.to_write().is_err());

        let mut ok = base.clone();
        ok.max_qty = Some(dec("10.0001"));
        let write = ok.to_write().unwrap();
        assert_eq!(write.tier_type, "TIER", "tier_type 缺省 = TIER");
        assert_eq!(write.customer_id, None, "客户缺省 = 通用档位");

        let mut unlimited = base.clone();
        unlimited.max_qty = None;
        assert!(unlimited.to_write().is_ok());
    }

    /// 档位类型：只认 TIER / PROMO。
    #[test]
    fn tier_type_domain() {
        assert_eq!(resolve_tier_type(None).unwrap(), "TIER");
        assert_eq!(resolve_tier_type(Some("PROMO")).unwrap(), "PROMO");
        assert!(resolve_tier_type(Some("DISCOUNT")).is_err());
    }

    /// 编辑档位：`max_qty = -1` 表示清空成「无上限」，`None` 表示不改。
    #[test]
    fn tier_update_max_qty_clear_semantics() {
        let current = TierRow {
            id: Uuid::now_v7(),
            material_id: Uuid::now_v7(),
            material_code: None,
            material_name: None,
            material_spec: None,
            customer_id: None,
            customer_code: None,
            customer_name: None,
            tier_type: "TIER".to_string(),
            min_qty: dec("10"),
            max_qty: Some(dec("100")),
            price: dec("8"),
            effective_from: parse_date("2026-11-01", "d").unwrap(),
            effective_to: None,
            status: "ACTIVE".to_string(),
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let keep = UpdateTierReq::default().merge(&current).unwrap();
        assert_eq!(keep.max_qty, Some(dec("100")), "缺省 = 不改");

        let clear = UpdateTierReq {
            max_qty: Some(dec("-1")),
            ..Default::default()
        }
        .merge(&current)
        .unwrap();
        assert_eq!(clear.max_qty, None, "-1 = 清空成无上限");
        assert_eq!(clear.price, dec("8"), "未传的字段沿用当前值");
    }

    /// 编辑协议价：不传的字段沿用当前行；空串清空可选列。
    #[test]
    fn agreed_update_merges_partial_fields() {
        let current = AgreedRow {
            id: Uuid::now_v7(),
            customer_id: Uuid::now_v7(),
            customer_code: Some("C001".to_string()),
            customer_name: Some("测试客户".to_string()),
            material_id: Uuid::now_v7(),
            material_code: Some("M001".to_string()),
            material_name: Some("测试物料".to_string()),
            material_spec: None,
            price: dec("12.5"),
            min_qty: dec("5"),
            effective_from: parse_date("2026-11-01", "d").unwrap(),
            effective_to: Some(parse_date("2026-12-31", "d").unwrap()),
            status: "ACTIVE".to_string(),
            remark: Some("原备注".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let merged = UpdateAgreedReq {
            price: Some(dec("13")),
            remark: Some("  ".to_string()),
            effective_to: Some(String::new()),
            ..Default::default()
        }
        .merge(&current)
        .unwrap();
        assert_eq!(merged.price, dec("13"));
        assert_eq!(merged.min_qty, dec("5"), "未传的起订量沿用");
        assert_eq!(merged.remark, None, "空白备注 = 清空");
        assert_eq!(merged.effective_to, None, "空串失效日 = 长期有效");

        // 合并后仍然要过价格校验：把价格改成 0 必须被拒（不能靠"只改一个字段"绕开）
        let bad = UpdateAgreedReq {
            price: Some(dec("0")),
            ..Default::default()
        }
        .merge(&current);
        assert!(bad.is_err());
    }

    /// 查询串里的非法 uuid → 400（静默忽略会把「按客户筛」变成「全部客户」）。
    #[test]
    fn filter_invalid_uuid_is_rejected() {
        let err = parse_optional_uuid(Some("abc"), "customer_id").unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("customer_id"));
        assert_eq!(parse_optional_uuid(Some("  "), "customer_id").unwrap(), None);
        assert_eq!(parse_optional_uuid(None, "customer_id").unwrap(), None);
    }

    /// 分页筛选的 status：非法值 400（不能静默当"全部"），空值放过。
    #[test]
    fn filter_status_is_validated() {
        assert!(check_status_filter(None).is_ok());
        assert!(check_status_filter(Some("  ")).is_ok());
        assert!(check_status_filter(Some("ACTIVE")).is_ok());
        let err = check_status_filter(Some("SUSPENDED")).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("status"));
    }

    /// 重复键的人话里必须带上「起订量 + 生效日」，否则用户不知道该改哪一列。
    #[test]
    fn duplicate_messages_name_the_key() {
        let write = create_agreed_req("5").to_write().unwrap();
        let err = agreed_duplicate(&write);
        assert_eq!(err.status, 409);
        assert!(err.msg.contains("2026-11-01"), "{}", err.msg);
        assert!(err.msg.contains("起订量"), "{}", err.msg);
    }
}

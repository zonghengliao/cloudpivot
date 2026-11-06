//! 客户档案接口层（销售链 · SO-02）：5 个接口。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/customer/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/customer` | `business:erp:customer:create` | 新建（编码未删内唯一，信用额度 `>= 0`） |
//! | PUT | `/business/erp/customer/{id}` | `business:erp:customer:update` | 编辑（部分更新：只改传了的字段） |
//! | DELETE | `/business/erp/customer/{id}` | `business:erp:customer:delete` | **软删**（`deleted_at`，编码随之释放可复用） |
//! | GET | `/business/erp/customer/{id}` | `business:erp:customer:list` | 详情（软删行 404） |
//! | GET | `/business/erp/customer/page` | `business:erp:customer:list` | 分页（`code` / `name` 模糊，`status` 精确） |
//!
//! # 为什么业务逻辑是普通 `async fn`（与采购 / 序列号同一范式）
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；把「校验 + 落库」
//! 收进 [`create_customer`] / [`update_customer`] / [`delete_customer`] /
//! [`get_customer`] / [`page_customer`] 之后，单测走的是**同一条代码路径**（只跳过认证）。
//! 操作人用 `Option<Uuid>` 传，不传 `TokenClaims` —— 单测不必伪造令牌。
//!
//! # 三条硬约束
//!
//! ① **编码唯一性**：先按 `(code, deleted_at IS NULL)` 查一次（人话 409），DDL 的部分唯一索引
//!    `uk_biz_customer_code` 是并发下的最后一道，撞上同样翻 `409 + 66001`；
//!    软删后编码**可以复用**（部分索引只管存活行）。
//! ② **信用额度 `>= 0`**：入口先拒（`400 + 10001`），`ck_biz_customer_credit_limit` 只是兜底。
//!    `0` 合法，语义是「不控额度」（与供应商 `credit_limit` 同口径）。
//! ③ **状态域 `ACTIVE` / `FROZEN`**：字符串域值，不是 1/0。`status` 是「正常 / 冻结」，
//!    冻结只影响开新单，历史单据与引用不受影响（`contract_model.rs:31` 的 `status = 1`
//!    是旧占位注释之误，客户分支写 `status = 'ACTIVE'`）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_customer 是全局主数据（V0260 无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:customer:* 权限码控制。
//! 分页走 customer_model::customer_page → scope::paginate_unscoped（闸①口径）。
//! ```
//!
//! # 不碰的东西
//!
//! 本卡只做客户主档 CRUD：**不写** `biz_customer_contact`（联系人子表，后续卡）、
//! **不写** 凭证 / 库存三本账 / 流程实例（客户档案不是业务单据，没有状态机和审批）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::customer_model::{
    self, CustomerRow, CustomerWrite, MAX_CODE_LEN, MAX_NAME_LEN, STATUS_ACTIVE, is_valid_status,
    normalize_code,
};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0261 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 / 详情。
pub const PERM_LIST: &str = "business:erp:customer:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:customer:create";
/// 编辑。
pub const PERM_UPDATE: &str = "business:erp:customer:update";
/// 软删。
pub const PERM_DELETE: &str = "business:erp:customer:delete";

/// 客户域全部权限码（顺序 = 迁移里的 VALUES 顺序；`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const CUSTOMER_PERMISSIONS: [&str; 4] = [PERM_LIST, PERM_CREATE, PERM_UPDATE, PERM_DELETE];

// ==================== 校验原语（口径同 `erp::dto`，本模块自持一份避免改公共件） ====================

/// `400 + 10001`。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 必填文本：去空白后非空，且 `chars().count() <= max`（与 `VARCHAR(n)` 同口径）。
fn require_text(value: &str, field: &str, max: usize) -> std::result::Result<(), ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    if trimmed.chars().count() > max {
        return Err(bad_request(format!(
            "{field} 最长 {max} 个字符（当前 {} 个）",
            trimmed.chars().count()
        )));
    }
    Ok(())
}

/// 选填文本：`None` / 空白放过；有值则限长（超长同样拒，避免被 `VARCHAR(n)` 静默截断）。
fn check_optional_text(
    value: Option<&str>,
    field: &str,
    max: usize,
) -> std::result::Result<(), ApiError> {
    match value.map(str::trim) {
        Some(text) if !text.is_empty() && text.chars().count() > max => Err(bad_request(format!(
            "{field} 最长 {max} 个字符（当前 {} 个）",
            text.chars().count()
        ))),
        _ => Ok(()),
    }
}

/// 状态域：只认 `ACTIVE` / `FROZEN`。
fn require_status(value: &str) -> std::result::Result<(), ApiError> {
    if is_valid_status(value) {
        return Ok(());
    }
    Err(bad_request(format!(
        "status 必须是 ACTIVE（正常）/ FROZEN（冻结）之一（当前 `{value}`）"
    )))
}

/// 信用额度：`>= 0`（`0` = 不控额度）。
fn require_credit_limit(value: Decimal) -> std::result::Result<(), ApiError> {
    if customer_model::credit_limit_allowed(value) {
        return Ok(());
    }
    Err(bad_request(format!(
        "credit_limit 不能为负数（当前 {value}）；0 表示不控制额度"
    )))
}

/// 路径里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 客户不存在 / 已软删（404）。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "客户不存在或已删除".to_string(),
    )
}

/// 编码重复（409 + `66001`）：文案与供应商 / 物料同一套口径（已软删的编码可复用）。
fn code_conflict(code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_CODE_CONFLICT,
        format!("客户编码 `{code}` 已存在（已软删的编码可以复用）"),
    )
}

/// 写路径的数据库错误收口。
///
/// - `DbError::UniqueViolation`（PG `23505`）→ `409 + 66001`（并发下撞部分唯一索引）；
/// - `DbError::CheckViolation`（PG `23514`）→ `400 + 10001`（DDL 的 CHECK 兜住了入口没校验的组合）；
/// - 其余仍是 500（细节只进日志）。
///
/// 判据用**变体**而不是文案（`DbError` 的 Display 是 `unique constraint violated constraint=…`，
/// 不含 `duplicate key`，按文案匹配会把唯一冲突判成 500 —— 仓里踩过）。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "客户编码已存在（唯一约束{}）：同一范围内不允许重复编码，已软删的编码可以复用",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足数据库约束{}（信用额度 >= 0 / 状态域 ACTIVE|FROZEN）",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
}

// ==================== 契约 ====================

/// 分页筛选：`code` / `name` 模糊，`status` 精确（`ACTIVE` / `FROZEN`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CustomerFilter {
    /// 客户编码（模糊）。
    pub code: Option<String>,
    /// 客户名称（模糊）。
    pub name: Option<String>,
    /// 状态（精确：`ACTIVE` / `FROZEN`）。
    pub status: Option<String>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数（0 → 默认 20，上限 200）。
    pub page_size: u64,
}

impl Default for CustomerFilter {
    fn default() -> Self {
        Self {
            code: None,
            name: None,
            status: None,
            page: 1,
            page_size: customer_model::DEFAULT_PAGE_SIZE,
        }
    }
}

/// 新建客户。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CreateCustomerReq {
    /// 客户编码（`VARCHAR(40)`，未删除内唯一）。
    pub code: String,
    /// 客户全称（`VARCHAR(200)`）。
    pub name: String,
    /// 纳税人识别号（`VARCHAR(50)`）。
    #[serde(default)]
    pub tax_no: Option<String>,
    /// 默认联系人姓名（`VARCHAR(50)`）。
    #[serde(default)]
    pub contact_name: Option<String>,
    /// 默认联系人电话（`VARCHAR(30)`）。
    #[serde(default)]
    pub contact_phone: Option<String>,
    /// 地址（`VARCHAR(255)`）。
    #[serde(default)]
    pub address: Option<String>,
    /// 信用额度（`>= 0`，**`0` / 留空 = 不控额度**）。
    #[serde(default)]
    pub credit_limit: Option<Decimal>,
    /// 状态（`ACTIVE` / `FROZEN`，留空 = `ACTIVE`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注。
    #[serde(default)]
    pub remark: Option<String>,
}

impl CreateCustomerReq {
    /// 校验（`400 + 10001`）。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        require_text(&self.code, "code", MAX_CODE_LEN)?;
        require_text(&self.name, "name", MAX_NAME_LEN)?;
        check_optional_text(self.tax_no.as_deref(), "tax_no", 50)?;
        check_optional_text(self.contact_name.as_deref(), "contact_name", 50)?;
        check_optional_text(self.contact_phone.as_deref(), "contact_phone", 30)?;
        check_optional_text(self.address.as_deref(), "address", 255)?;
        if let Some(credit_limit) = self.credit_limit {
            require_credit_limit(credit_limit)?;
        }
        if let Some(status) = self
            .status
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            require_status(status)?;
        }
        Ok(())
    }
}

/// 编辑客户（**部分更新**：只改传过来的字段；可选文本传空串 = 清空）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateCustomerReq {
    /// 客户编码（不传 = 不改）。
    #[serde(default)]
    pub code: Option<String>,
    /// 客户全称（不传 = 不改）。
    #[serde(default)]
    pub name: Option<String>,
    /// 纳税人识别号（传空串 = 清空）。
    #[serde(default)]
    pub tax_no: Option<String>,
    /// 默认联系人姓名（传空串 = 清空）。
    #[serde(default)]
    pub contact_name: Option<String>,
    /// 默认联系人电话（传空串 = 清空）。
    #[serde(default)]
    pub contact_phone: Option<String>,
    /// 地址（传空串 = 清空）。
    #[serde(default)]
    pub address: Option<String>,
    /// 信用额度（不传 = 不改）。
    #[serde(default)]
    pub credit_limit: Option<Decimal>,
    /// 状态（不传 = 不改）。
    #[serde(default)]
    pub status: Option<String>,
    /// 备注（传空串 = 清空）。
    #[serde(default)]
    pub remark: Option<String>,
}

impl UpdateCustomerReq {
    /// 校验（`400 + 10001`）：**只校验传了的字段**，没传的不算错。
    pub fn validate(&self) -> std::result::Result<(), ApiError> {
        if let Some(code) = self.code.as_deref() {
            require_text(code, "code", MAX_CODE_LEN)?;
        }
        if let Some(name) = self.name.as_deref() {
            require_text(name, "name", MAX_NAME_LEN)?;
        }
        check_optional_text(self.tax_no.as_deref(), "tax_no", 50)?;
        check_optional_text(self.contact_name.as_deref(), "contact_name", 50)?;
        check_optional_text(self.contact_phone.as_deref(), "contact_phone", 30)?;
        check_optional_text(self.address.as_deref(), "address", 255)?;
        if let Some(credit_limit) = self.credit_limit {
            require_credit_limit(credit_limit)?;
        }
        if let Some(status) = self
            .status
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            require_status(status)?;
        }
        Ok(())
    }
}

/// 客户出参（详情 / 列表行同构）。
#[derive(Debug, Clone, Serialize)]
pub struct CustomerRes {
    /// 主键。
    pub id: String,
    /// 客户编码。
    pub code: String,
    /// 客户全称。
    pub name: String,
    /// 纳税人识别号。
    pub tax_no: Option<String>,
    /// 默认联系人姓名。
    pub contact_name: Option<String>,
    /// 默认联系人电话。
    pub contact_phone: Option<String>,
    /// 地址。
    pub address: Option<String>,
    /// 信用额度（`0` = 不控额度）。
    pub credit_limit: Decimal,
    /// 状态：`ACTIVE` / `FROZEN`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: nik::prelude::chrono::DateTime<nik::prelude::chrono::Utc>,
    /// 更新时间。
    pub updated_at: nik::prelude::chrono::DateTime<nik::prelude::chrono::Utc>,
}

impl From<&CustomerRow> for CustomerRes {
    fn from(row: &CustomerRow) -> Self {
        Self {
            id: row.id.to_string(),
            code: row.code.clone(),
            name: row.name.clone(),
            tax_no: row.tax_no.clone(),
            contact_name: row.contact_name.clone(),
            contact_phone: row.contact_phone.clone(),
            address: row.address.clone(),
            credit_limit: row.credit_limit,
            status: row.status.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 业务逻辑（单测直接调这些） ====================

/// 新建客户。
pub async fn create_customer(
    operator: Option<Uuid>,
    req: CreateCustomerReq,
) -> std::result::Result<CustomerRes, ApiError> {
    req.validate()?;
    let code = normalize_code(&req.code);
    if customer_model::customer_code_taken(&code, None)
        .await
        .map_err(|error| utils::db_error(error, "校验客户编码唯一性"))?
    {
        return Err(code_conflict(&code));
    }
    let write = CustomerWrite {
        code,
        name: req.name.trim().to_string(),
        tax_no: clean_optional(req.tax_no),
        contact_name: clean_optional(req.contact_name),
        contact_phone: clean_optional(req.contact_phone),
        address: clean_optional(req.address),
        credit_limit: req.credit_limit.unwrap_or(Decimal::ZERO),
        status: req
            .status
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| STATUS_ACTIVE.to_string()),
        remark: clean_optional(req.remark),
    };
    let id = Uuid::now_v7();
    customer_model::customer_insert(id, &write, operator)
        .await
        .map_err(|error| write_error(error, "新建客户"))?;
    let row = customer_model::customer_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建客户"))?
        .ok_or_else(not_found)?;
    Ok(CustomerRes::from(&row))
}

/// 编辑客户（部分更新）。
pub async fn update_customer(
    operator: Option<Uuid>,
    id_raw: &str,
    req: UpdateCustomerReq,
) -> std::result::Result<CustomerRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    req.validate()?;
    let current = customer_model::customer_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待编辑客户"))?
        .ok_or_else(not_found)?;
    let write = merge_write(&current, req);
    if write.code != current.code
        && customer_model::customer_code_taken(&write.code, Some(id))
            .await
            .map_err(|error| utils::db_error(error, "校验客户编码唯一性"))?
    {
        return Err(code_conflict(&write.code));
    }
    let affected = customer_model::customer_update(id, &write, operator)
        .await
        .map_err(|error| write_error(error, "编辑客户"))?;
    if affected == 0 {
        return Err(not_found());
    }
    let row = customer_model::customer_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "回读客户"))?
        .ok_or_else(not_found)?;
    Ok(CustomerRes::from(&row))
}

/// 软删客户（已删 / 不存在 → 404）。
pub async fn delete_customer(
    operator: Option<Uuid>,
    id_raw: &str,
) -> std::result::Result<(), ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let affected = customer_model::customer_soft_delete(id, operator)
        .await
        .map_err(|error| utils::db_error(error, "软删客户"))?;
    if affected == 0 {
        return Err(not_found());
    }
    Ok(())
}

/// 客户详情（软删行 404）。
pub async fn get_customer(id_raw: &str) -> std::result::Result<CustomerRes, ApiError> {
    let id = parse_uuid(id_raw, "id")?;
    let row = customer_model::customer_find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询客户详情"))?
        .ok_or_else(not_found)?;
    Ok(CustomerRes::from(&row))
}

/// 客户分页。
pub async fn page_customer(
    filter: CustomerFilter,
) -> std::result::Result<PageData<Vec<CustomerRes>>, ApiError> {
    // `status` 传了非法值 → 400（静默忽略会把「冻结客户」筛成「全部客户」，是假数据）。
    if let Some(status) = filter
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        require_status(status)?;
    }
    let (rows, total) = customer_model::customer_page(
        filter.code.as_deref(),
        filter.name.as_deref(),
        filter.status.as_deref(),
        filter.page,
        filter.page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "分页查询客户"))?;
    let (page, page_size) = customer_model::normalize_page(filter.page, filter.page_size);
    let list = rows.iter().map(CustomerRes::from).collect();
    Ok(PageData {
        list,
        total: u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    })
}

/// 部分更新合并：未传的字段沿用当前值；可选文本 `Some("")` → `None`（清空）。
fn merge_write(current: &CustomerRow, req: UpdateCustomerReq) -> CustomerWrite {
    let code = req
        .code
        .as_deref()
        .map(normalize_code)
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| current.code.clone());
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| current.name.clone());
    let status = req
        .status
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| current.status.clone());
    CustomerWrite {
        code,
        name,
        tax_no: merge_optional(req.tax_no, current.tax_no.clone()),
        contact_name: merge_optional(req.contact_name, current.contact_name.clone()),
        contact_phone: merge_optional(req.contact_phone, current.contact_phone.clone()),
        address: merge_optional(req.address, current.address.clone()),
        credit_limit: req.credit_limit.unwrap_or(current.credit_limit),
        status,
        remark: merge_optional(req.remark, current.remark.clone()),
    }
}

/// 可选文本：`None` = 不改；`Some(text)` = 覆盖（空白 → `None`，即清空）。
fn merge_optional(incoming: Option<String>, fallback: Option<String>) -> Option<String> {
    match incoming {
        None => fallback,
        Some(value) => clean_optional(Some(value)),
    }
}

/// 落库前的可选文本清洗：trim 后空串 → `None`。
fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

// ==================== HTTP 入口 ====================

/// 客户分页。
#[nik::permission(
    path = "/business/erp/customer/page",
    method = "GET",
    permission = "business:erp:customer:list"
)]
#[nik::get("/business/erp/customer/page")]
pub async fn customer_page(
    Query(filter): Query<CustomerFilter>,
) -> ResResult<PageData<Vec<CustomerRes>>> {
    Ok(Res::ok(page_customer(filter).await?))
}

/// 客户详情。
#[nik::permission(
    path = "/business/erp/customer/{id}",
    method = "GET",
    permission = "business:erp:customer:list"
)]
#[nik::get("/business/erp/customer/{id}")]
pub async fn customer_detail(Path(id): Path<String>) -> ResResult<CustomerRes> {
    Ok(Res::ok(get_customer(&id).await?))
}

/// 新建客户。
#[nik::permission(
    path = "/business/erp/customer",
    method = "POST",
    permission = "business:erp:customer:create"
)]
#[nik::post("/business/erp/customer")]
pub async fn customer_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateCustomerReq>,
) -> ResResult<CustomerRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(create_customer(operator, req).await?))
}

/// 编辑客户。
#[nik::permission(
    path = "/business/erp/customer/{id}",
    method = "PUT",
    permission = "business:erp:customer:update"
)]
#[nik::put("/business/erp/customer/{id}")]
pub async fn customer_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<UpdateCustomerReq>,
) -> ResResult<CustomerRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(update_customer(operator, &id, req).await?))
}

/// 软删客户。
#[nik::permission(
    path = "/business/erp/customer/{id}",
    method = "DELETE",
    permission = "business:erp:customer:delete"
)]
#[nik::delete("/business/erp/customer/{id}")]
pub async fn customer_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = utils::current_user_uuid(&claims);
    delete_customer(operator, &id).await?;
    Ok(Res::ok(()))
}

/// 权限码自检：4 个码都在 `CUSTOMER_PERMISSIONS` 里、无重复、前缀正确。
pub fn all_permissions() -> Vec<&'static str> {
    CUSTOMER_PERMISSIONS.to_vec()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `SO2T-` 前缀夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 本模块夹具的统一编码前缀 —— 清场只按它删，**不扫全表**（并行线程会删到兄弟用例的行）。
    const CODE_PREFIX: &str = "SO2T-";

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 拿锁 + 连库 + 清掉上一轮本模块的夹具（联系人先删，外键是 RESTRICT）。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场：只删 `<CODE_PREFIX>*` 的客户及其联系人（客户只能软删，测试行直接物理删）。
    async fn cleanup() {
        let pattern = format!("{CODE_PREFIX}%");
        let deletes = [
            "DELETE FROM biz_customer_contact WHERE customer_id IN \
             (SELECT id FROM biz_customer WHERE code LIKE ?)",
            "DELETE FROM biz_customer WHERE code LIKE ?",
        ];
        for sql in deletes {
            if let Err(err) = db()
                .execute_with_params(sql, vec![Value::from(pattern.clone())])
                .await
            {
                panic!("清理夹具失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 造一个请求（默认 `ACTIVE` / 额度 0）。
    fn req(code: &str, name: &str) -> CreateCustomerReq {
        CreateCustomerReq {
            code: code.to_string(),
            name: name.to_string(),
            credit_limit: Some(Decimal::from(1000)),
            ..Default::default()
        }
    }

    /// 创建：字段落库、`status` 默认 `ACTIVE`、`code` 去空白、额度定点保真。
    #[tokio::test]
    async fn create_customer_persists_all_fields() {
        let Some(_guard) = setup().await else { return };
        let code = format!("{CODE_PREFIX}C-0001");
        let mut request = req(&format!("  {code}  "), " 北京云枢测试客户 ");
        request.tax_no = Some("91110108TEST0001XY".to_string());
        request.contact_name = Some("张伟".to_string());
        request.contact_phone = Some("010-88886666".to_string());
        request.address = Some("北京市海淀区中关村大街 1 号".to_string());
        request.credit_limit = Some(Decimal::from_str("123456.7890").expect("定点可解析"));
        request.remark = Some("SO-02 单测".to_string());
        let res = create_customer(None, request).await.expect("创建应成功");
        assert_eq!(res.code, code, "code 应去掉首尾空白");
        assert_eq!(res.name, "北京云枢测试客户", "name 应 trim");
        assert_eq!(res.status, "ACTIVE", "未传 status 应落默认 ACTIVE");
        assert_eq!(res.credit_limit, Decimal::from_str("123456.7890").unwrap());
        assert_eq!(res.contact_name.as_deref(), Some("张伟"));
        assert_eq!(res.tax_no.as_deref(), Some("91110108TEST0001XY"));
        // 回读走详情接口，确认真的落库（不是只在返回值里拼出来的）
        let detail = get_customer(&res.id).await.expect("详情应可读");
        assert_eq!(detail.id, res.id);
        assert_eq!(
            detail.address.as_deref(),
            Some("北京市海淀区中关村大街 1 号")
        );
        cleanup().await;
    }

    /// 重复 code：409 + 66001；软删之后同一编码可以复用（部分唯一索引语义）。
    #[tokio::test]
    async fn duplicate_code_rejected_and_reusable_after_soft_delete() {
        let Some(_guard) = setup().await else { return };
        let code = format!("{CODE_PREFIX}DUP-0001");
        let first = create_customer(None, req(&code, "重复编码一号"))
            .await
            .expect("首次创建应成功");
        let err = create_customer(None, req(&code, "重复编码二号"))
            .await
            .expect_err("重复编码必须被拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_CODE_CONFLICT, "应为 66001");
        // 软删后编码释放 → 可复用
        delete_customer(None, &first.id).await.expect("软删应成功");
        let reused = create_customer(None, req(&code, "重复编码三号"))
            .await
            .expect("软删后同编码应可复用");
        assert_ne!(reused.id, first.id);
        cleanup().await;
    }

    /// 信用额度 < 0：400 + 10001；`0` 合法（= 不控额度）。
    #[tokio::test]
    async fn negative_credit_limit_rejected_zero_allowed() {
        // 纯校验，不需要库
        let mut bad = req("SO2T-NEG-0001", "负额度");
        bad.credit_limit = Some(Decimal::from_str("-0.0001").unwrap());
        let err = bad.validate().expect_err("负额度必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);

        let mut zero = req("SO2T-ZERO-0001", "零额度");
        zero.credit_limit = Some(Decimal::ZERO);
        zero.validate().expect("0 额度合法（= 不控额度）");

        // 状态域也钉一下
        let mut weird = req("SO2T-STATUS-0001", "非法状态");
        weird.status = Some("FROZEN_X".to_string());
        let err = weird.validate().expect_err("非法状态必须被拒");
        assert_eq!(err.code, error_code::PARAM_INVALID);
    }

    /// 分页：code / name 模糊 + status 精确；非法 status → 400（不静默忽略）。
    #[tokio::test]
    async fn page_filters_by_code_name_and_status() {
        let Some(_guard) = setup().await else { return };
        let mut frozen = req(&format!("{CODE_PREFIX}PG-0003"), "分页测试冻结客户");
        frozen.status = Some("FROZEN".to_string());
        create_customer(
            None,
            req(&format!("{CODE_PREFIX}PG-0001"), "分页测试客户甲"),
        )
        .await
        .expect("建甲");
        create_customer(
            None,
            req(&format!("{CODE_PREFIX}PG-0002"), "分页测试客户乙"),
        )
        .await
        .expect("建乙");
        create_customer(None, frozen).await.expect("建丙（冻结）");

        let all = page_customer(CustomerFilter {
            code: Some(CODE_PREFIX.to_string()),
            page_size: 100,
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(all.total, 3, "前缀过滤应命中 3 行");
        let codes: Vec<&str> = all.list.iter().map(|row| row.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                format!("{CODE_PREFIX}PG-0001"),
                format!("{CODE_PREFIX}PG-0002"),
                format!("{CODE_PREFIX}PG-0003")
            ],
            "应按编码升序"
        );

        let frozen_only = page_customer(CustomerFilter {
            code: Some(CODE_PREFIX.to_string()),
            status: Some("FROZEN".to_string()),
            ..Default::default()
        })
        .await
        .expect("按状态筛应成功");
        assert_eq!(frozen_only.total, 1);
        assert_eq!(frozen_only.list[0].code, format!("{CODE_PREFIX}PG-0003"));

        let by_name = page_customer(CustomerFilter {
            name: Some("客户乙".to_string()),
            ..Default::default()
        })
        .await
        .expect("按名称筛应成功");
        assert_eq!(by_name.total, 1);
        assert_eq!(by_name.list[0].name, "分页测试客户乙");

        let err = page_customer(CustomerFilter {
            status: Some("DELETED".to_string()),
            ..Default::default()
        })
        .await
        .expect_err("非法状态必须 400");
        assert_eq!(err.code, error_code::PARAM_INVALID);

        cleanup().await;
    }

    /// 软删：详情 404、分页不含、二次删除 404；物理行仍在（联系人外键 RESTRICT 才挡得住）。
    #[tokio::test]
    async fn soft_delete_hides_row_but_keeps_it() {
        let Some(_guard) = setup().await else { return };
        let code = format!("{CODE_PREFIX}DEL-0001");
        let created = create_customer(None, req(&code, "软删测试客户"))
            .await
            .expect("创建应成功");
        delete_customer(None, &created.id)
            .await
            .expect("软删应成功");

        let err = get_customer(&created.id)
            .await
            .expect_err("软删后详情应 404");
        assert_eq!(err.status, 404);
        assert_eq!(err.code, error_code::RESOURCE_NOT_FOUND);

        let page = page_customer(CustomerFilter {
            code: Some(code.clone()),
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(page.total, 0, "软删行不得出现在分页里");

        let err = delete_customer(None, &created.id)
            .await
            .expect_err("重复软删应 404");
        assert_eq!(err.status, 404);

        // 物理行仍在（软删不是删除）
        let alive: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_customer WHERE code = ? AND deleted_at IS NOT NULL",
                vec![Value::from(code.as_str())],
            )
            .await
            .expect("计数应成功");
        assert_eq!(alive, 1, "软删后物理行必须还在");

        cleanup().await;
    }

    /// 编辑：只改传了的字段；可选文本传空串 = 清空；编码撞别人 → 409。
    #[tokio::test]
    async fn update_is_partial_and_checks_code_uniqueness() {
        let Some(_guard) = setup().await else { return };
        let a = create_customer(None, req(&format!("{CODE_PREFIX}UP-0001"), "改名前"))
            .await
            .expect("建 A");
        let b = create_customer(None, req(&format!("{CODE_PREFIX}UP-0002"), "对手 B"))
            .await
            .expect("建 B");

        let updated = update_customer(
            None,
            &a.id,
            UpdateCustomerReq {
                name: Some("改名后".to_string()),
                remark: Some(String::new()), // 空串 = 清空
                ..Default::default()
            },
        )
        .await
        .expect("编辑应成功");
        assert_eq!(updated.name, "改名后");
        assert_eq!(
            updated.code,
            format!("{CODE_PREFIX}UP-0001"),
            "没传 code 不应改动"
        );
        assert_eq!(
            updated.credit_limit,
            Decimal::from(1000),
            "没传额度不应改动"
        );
        assert_eq!(updated.status, "ACTIVE", "没传状态不应改动");
        assert_eq!(updated.remark, None, "空串应清空为 NULL");

        let err = update_customer(
            None,
            &a.id,
            UpdateCustomerReq {
                code: Some(format!("{CODE_PREFIX}UP-0002")),
                ..Default::default()
            },
        )
        .await
        .expect_err("改成别人的编码必须 409");
        assert_eq!(err.code, error_code::ERP_CODE_CONFLICT);
        assert_eq!(b.code, format!("{CODE_PREFIX}UP-0002"));

        cleanup().await;
    }

    /// 权限码自检：4 个码、无重复、前缀 `business:erp:customer:`（V0261 的菜单行逐字对应）。
    #[test]
    fn permission_codes_are_unique_and_prefixed() {
        let codes = all_permissions();
        assert_eq!(codes.len(), 4);
        for (index, code) in codes.iter().enumerate() {
            assert!(
                code.starts_with("business:erp:customer:"),
                "权限码前缀不对：{code}"
            );
            assert!(!codes[index + 1..].contains(code), "权限码重复：{code}");
        }
    }
}

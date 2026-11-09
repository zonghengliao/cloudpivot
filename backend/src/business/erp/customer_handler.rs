//! 客户档案接口层（销售链 · SO-02）：5 个接口。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/customer/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/customer` | `business:erp:customer:create` | 新建（编码未删内唯一，信用额度 `>= 0`；**编码留空自动取号**） |
//! | PUT | `/business/erp/customer/{id}` | `business:erp:customer:update` | 编辑（部分更新：只改传了的字段） |
//! | DELETE | `/business/erp/customer/{id}` | `business:erp:customer:delete` | **软删**（`deleted_at`，编码随之释放可复用） |
//! | GET | `/business/erp/customer/{id}` | `business:erp:customer:list` | 详情（软删行 404） |
//! | GET | `/business/erp/customer/page` | `business:erp:customer:list` | 分页（`code` / `name` 模糊，`status` 精确） |
//! | GET | `/business/erp/customer/next-code` | `business:erp:customer:create` | 编码预览（**只读不占号**，前端「生成编码」按钮用） |
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
//! ④ **编码建后不可改**：编辑时传了非空且与现值不同的编码 → `400 + 10001`
//!    「客户编码不可修改」（口径与资产分类 `asset_handler::asset_category_update` 一致）。
//!    前端编辑态本就是只读，这条是接口侧兜底 —— 否则绕过界面直接 PUT 就能改，
//!    历史单据 / 审计日志里按编码回溯时会串。
//!    不传编码 = 不改；传原值 = 放行（前端整体提交表单的两种常见形态）；
//!    传空串 / 纯空白仍按既有入参校验 400「code 不能为空」（编码是必填列，不存在「清空」）。
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
use serde_json::json;

use crate::business::erp::code_policy;
use crate::business::erp::customer_model::{
    self, CustomerRow, CustomerWrite, MAX_CODE_LEN, MAX_NAME_LEN, STATUS_ACTIVE, is_valid_status, normalize_code,
};
use crate::business::erp::seq;
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
fn check_optional_text(value: Option<&str>, field: &str, max: usize) -> std::result::Result<(), ApiError> {
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

/// 账期天数：`>= 0`（`0` = 现结，到期日 = 过账日）。
fn require_credit_days(value: i32) -> std::result::Result<(), ApiError> {
    if customer_model::credit_days_allowed(value) {
        return Ok(());
    }
    Err(bad_request(format!(
        "credit_days 不能为负数（当前 {value}）；0 表示现结（到期日 = 过账日）"
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
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "客户不存在或已删除".to_string())
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
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足数据库约束{}（信用额度 >= 0 / 状态域 ACTIVE|FROZEN）",
                constraint.as_deref().map(|name| format!(" {name}")).unwrap_or_default()
            ),
        ),
        _ => utils::db_error(err, ctx),
    }
}

/// 建客户写路径的 DB 错误收口（CODE-02）：唯一索引冲突翻成**带编码**的 409。
///
/// 与 [`write_error`] 的区别只有一点 —— 唯一冲突时文案点名具体编码，
/// 运维 / 用户一眼能看出撞的是哪个号（`uk_biz_customer_code`）。
fn customer_write_error(err: DbError, code: &str) -> ApiError {
    match &err {
        DbError::UniqueViolation { .. } => code_conflict(code),
        _ => write_error(err, "新建客户"),
    }
}

/// 自动取号失败（`biz_seq` 没有可用规则 / 连号全被占）→ `500 + 10004`。
///
/// 对外只说「找管理员」，真实原因进 `internal_detail`（与 [`utils::db_error`] 同口径）。
fn seq_error(error: DbError, ctx: &str) -> ApiError {
    ApiError {
        status: 500,
        code: error_code::INTERNAL_ERROR,
        msg: format!("{ctx}：自动取号失败，请联系管理员检查取号规则"),
        internal_detail: Some(format!("{ctx}: {error}")),
        data: None,
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
    ///
    /// **CODE-02**：留空（`null` / `""` / 纯空白）⇒ 后端自动取号 `CUS-0001` 形态；
    /// 有值 ⇒ 原样保留（企业自有编码体系要能对接），只校验长度与字符集，
    /// 解析口径见 `erp::seq::resolve_manual_code`。
    #[serde(default)]
    pub code: Option<String>,
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
    /// 账期天数（`>= 0`，**留空 = 0 = 现结**）。
    #[serde(default)]
    pub credit_days: Option<i32>,
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
        // 编码可留空（CODE-02：留空则由后端取号）；填了才校验「可用性」——
        // 长度 ≤ 40（VARCHAR(40)）+ 字符集，**不要求 CUS-xxx 形态**。
        crate::business::erp::seq::resolve_manual_code(self.code.as_deref(), MAX_CODE_LEN)
            .map_err(|reason| bad_request(format!("code 不合法：{reason}")))?;
        require_text(&self.name, "name", MAX_NAME_LEN)?;
        check_optional_text(self.tax_no.as_deref(), "tax_no", 50)?;
        check_optional_text(self.contact_name.as_deref(), "contact_name", 50)?;
        check_optional_text(self.contact_phone.as_deref(), "contact_phone", 30)?;
        check_optional_text(self.address.as_deref(), "address", 255)?;
        if let Some(credit_days) = self.credit_days {
            require_credit_days(credit_days)?;
        }
        if let Some(credit_limit) = self.credit_limit {
            require_credit_limit(credit_limit)?;
        }
        if let Some(status) = self.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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
    /// 账期天数（不传 = 不改）。
    #[serde(default)]
    pub credit_days: Option<i32>,
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
        if let Some(credit_days) = self.credit_days {
            require_credit_days(credit_days)?;
        }
        if let Some(credit_limit) = self.credit_limit {
            require_credit_limit(credit_limit)?;
        }
        if let Some(status) = self.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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
    /// 账期天数（`0` = 现结）。
    pub credit_days: i32,
    /// 信用额度（`0` = 不控额度）。
    pub credit_limit: Decimal,
    /// 已用额度（SO-38：有效订单含税 − 已核销应收 − 已过账退货，下限 0）。
    pub credit_used: Decimal,
    /// 可用额度（`None` = 不控额度；否则 `credit_limit − credit_used`，不出负数）。
    pub credit_available: Option<Decimal>,
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
            credit_days: row.credit_days,
            credit_limit: row.credit_limit,
            credit_used: row.credit_used,
            // 0 = 不控额度（与供应商同口径）：这时「可用额度」没有意义，返回 None
            credit_available: if row.credit_limit <= Decimal::ZERO {
                None
            } else {
                let rest = row.credit_limit - row.credit_used;
                Some(if rest < Decimal::ZERO { Decimal::ZERO } else { rest })
            },
            status: row.status.clone(),
            remark: row.remark.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ==================== 业务逻辑（单测直接调这些） ====================

/// 新建客户（CODE-02：编码留空则自动取号 `CUS-0001` 形态）。
///
/// # 编码口径（用户已拍板，不许改）
///
/// | 入参 `code` | 行为 |
/// |---|---|
/// | `null` / `""` / 纯空白 | 自动取号：`biz_seq` 的 CUS 号段（纯流水 4 位、不计日期、永不重置）；撞号自动顺延 |
/// | 有值 | **原样保留**（企业自有编码体系，`KH-001` 也行），只校验长度 ≤ 40 + 字符集 |
///
/// 手工编码不强制 `CUS-xxx` 形态，只挡真正没法用的输入（空白 / 控制字符 / 中文标点）。
///
/// # 取号与落库同一事务
///
/// 取号是 `UPDATE biz_seq … RETURNING`（行锁），插入失败整体回滚、号一起退回，不留空号；
/// 并发建两个客户也不会拿到同一个号。
pub async fn create_customer(
    operator: Option<Uuid>,
    req: CreateCustomerReq,
) -> std::result::Result<CustomerRes, ApiError> {
    req.validate()?;
    // ① 手工编码：有值就保留（先去重，给出「编码 XXX 已存在」的人话错误）
    // CODE-08：开关关闭时填了编码直接 400（读在 begin 之前，被拒不占号）
    code_policy::guard_manual(code_policy::allow_manual_code().await, req.code.as_deref())?;
    let manual = seq::resolve_manual_code(req.code.as_deref(), MAX_CODE_LEN)
        .map_err(|reason| bad_request(format!("code 不合法：{reason}")))?;
    if let Some(code) = manual.as_deref()
        && customer_model::customer_code_taken(code, None)
            .await
            .map_err(|error| utils::db_error(error, "校验客户编码唯一性"))?
    {
        return Err(code_conflict(code));
    }
    let write = CustomerWrite {
        // 自动生成时先占位空串，真正编码在事务里取（`CustomerWrite` 是值类型，
        // 下面 begin 之后再填，避免「事务外取号」导致的空号）
        code: manual.clone().unwrap_or_default(),
        name: req.name.trim().to_string(),
        tax_no: clean_optional(req.tax_no),
        contact_name: clean_optional(req.contact_name),
        contact_phone: clean_optional(req.contact_phone),
        address: clean_optional(req.address),
        credit_days: req.credit_days.unwrap_or(0),
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
    // ② 编码 + INSERT 同一事务
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启新建客户事务"))?;
    let mut write = write;
    if manual.is_none() {
        write.code = seq::next_master_code_free(&tx, "CUS")
            .await
            .map_err(|error| seq_error(error, "新建客户"))?;
    }
    let code = write.code.clone();
    customer_model::customer_insert_ex(&tx, id, &write, operator)
        .await
        .map_err(|error| customer_write_error(error, &code))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交新建客户事务"))?;
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
    // 编码**不可修改**（口径与资产分类 `asset_handler::asset_category_update` 一致）：
    // 客户编码是历史单据、审计日志与对外对账的引用口径，改了会让老单据上的编码与新档案对不上。
    // 前端编辑态本就把它做成只读，这里是接口侧兜底 —— 绕过界面直接 PUT 也必须被拒。
    // 留空（没传 / 空串 / 纯空白）= 不改编码，不报错（前端整体提交表单时会带空值）。
    if let Some(requested) = req.code.as_deref() {
        let requested = normalize_code(requested);
        if !requested.is_empty() && requested != current.code {
            return Err(bad_request(format!(
                "客户编码不可修改（当前 {} / 请求 {}）：编码是历史单据与审计的引用口径，\
                 改了会让老单据上的客户编码与新档案对不上",
                current.code, requested
            )));
        }
    }
    let write = merge_write(&current, req);
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
pub async fn delete_customer(operator: Option<Uuid>, id_raw: &str) -> std::result::Result<(), ApiError> {
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
pub async fn page_customer(filter: CustomerFilter) -> std::result::Result<PageData<Vec<CustomerRes>>, ApiError> {
    // `status` 传了非法值 → 400（静默忽略会把「冻结客户」筛成「全部客户」，是假数据）。
    if let Some(status) = filter.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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
        credit_days: req.credit_days.unwrap_or(current.credit_days),
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
pub async fn customer_page(Query(filter): Query<CustomerFilter>) -> ResResult<PageData<Vec<CustomerRes>>> {
    Ok(Res::ok(page_customer(filter).await?))
}

/// 客户编码预览（卡 CODE-05 · 交互方式 B：前端「生成编码」按钮先看号，
/// 保存时把这个号当**手工编码**提交）。
///
/// 与供应商 `preview_supplier_code` 同一口径：**只读、不占号**（方案 B），
/// 并发撞号由保存时的 `409 + 66001` 兜底，前端提示重新生成即可。
#[nik::permission(
    path = "/business/erp/customer/next-code",
    method = "GET",
    permission = "business:erp:customer:create"
)]
#[nik::get("/business/erp/customer/next-code")]
pub async fn customer_next_code(Extension(AuthClaims(_claims)): Extension<AuthClaims>) -> ResResult<serde_json::Value> {
    Ok(Res::ok(json!({ "code": preview_customer_code().await? })))
}

/// 客户编码预览的**业务主体**（单测直接调本函数，不走认证）。
///
/// 号段口径：`biz_seq` 的 CUS 规则 —— 前缀 `CUS`、分隔符 `-`、纯流水 4 位、不计日期、
/// 永不重置；连号被占（如历史数据留着 `CUS-0001..0003`）则顺延到下一个空号。
pub async fn preview_customer_code() -> std::result::Result<String, ApiError> {
    seq::peek_master_code(db(), "CUS")
        .await
        .map_err(|error| seq_error(error, "预览客户编码"))
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
            if let Err(err) = db().execute_with_params(sql, vec![Value::from(pattern.clone())]).await {
                panic!("清理夹具失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 造一个请求（默认 `ACTIVE` / 额度 0）。
    fn req(code: &str, name: &str) -> CreateCustomerReq {
        CreateCustomerReq {
            code: Some(code.to_string()),
            name: name.to_string(),
            credit_limit: Some(Decimal::from(1000)),
            ..Default::default()
        }
    }

    /// 造一个**留空编码**的请求（CODE-02：交给后端自动取号）。
    fn auto_req(name: &str) -> CreateCustomerReq {
        CreateCustomerReq {
            code: None,
            name: name.to_string(),
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
        assert_eq!(detail.address.as_deref(), Some("北京市海淀区中关村大街 1 号"));
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
        create_customer(None, req(&format!("{CODE_PREFIX}PG-0001"), "分页测试客户甲"))
            .await
            .expect("建甲");
        create_customer(None, req(&format!("{CODE_PREFIX}PG-0002"), "分页测试客户乙"))
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
        delete_customer(None, &created.id).await.expect("软删应成功");

        let err = get_customer(&created.id).await.expect_err("软删后详情应 404");
        assert_eq!(err.status, 404);
        assert_eq!(err.code, error_code::RESOURCE_NOT_FOUND);

        let page = page_customer(CustomerFilter {
            code: Some(code.clone()),
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(page.total, 0, "软删行不得出现在分页里");

        let err = delete_customer(None, &created.id).await.expect_err("重复软删应 404");
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

    /// 编辑：只改传了的字段；可选文本传空串 = 清空；**编码不可改（400）**。
    #[tokio::test]
    async fn update_is_partial_and_locks_code() {
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
        assert_eq!(updated.code, format!("{CODE_PREFIX}UP-0001"), "没传 code 不应改动");
        assert_eq!(updated.credit_limit, Decimal::from(1000), "没传额度不应改动");
        assert_eq!(updated.status, "ACTIVE", "没传状态不应改动");
        assert_eq!(updated.remark, None, "空串应清空为 NULL");

        // 改成别人的编码 / 改成任何新编码：一律 400「编码不可修改」，不再是 409 唯一键冲突
        let err = update_customer(
            None,
            &a.id,
            UpdateCustomerReq {
                code: Some(format!("{CODE_PREFIX}UP-0002")),
                ..Default::default()
            },
        )
        .await
        .expect_err("改成别人的编码必须 400");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(
            err.msg.contains("客户编码不可修改"),
            "文案必须点名不可修改：{}",
            err.msg
        );
        assert_eq!(b.code, format!("{CODE_PREFIX}UP-0002"), "别人的编码不得被动过");

        cleanup().await;
    }

    /// 编码锁定（E2E-08 ①）：非空 + 与现值不同 → 400；**与现值相同 / 不传 → 放行**，
    /// 且其他字段照常可改（改编码被拒时不得顺带写坏别的列）。
    #[tokio::test]
    async fn customer_code_cannot_be_changed() {
        let Some(_guard) = setup().await else { return };
        let code = format!("{CODE_PREFIX}LOCK-0001");
        let created = create_customer(None, req(&code, "编码锁定客户"))
            .await
            .expect("创建应成功");

        // ① 换成新编码 → 400 + 10001，库里编码不变
        let err = update_customer(
            None,
            &created.id,
            UpdateCustomerReq {
                code: Some(format!("{CODE_PREFIX}LOCK-CHG")),
                name: Some("改名也算改了".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("改编码必须被拒");
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("客户编码不可修改"), "文案：{}", err.msg);
        let detail = get_customer(&created.id).await.expect("详情");
        assert_eq!(detail.code, code, "被拒后编码必须原样");
        assert_eq!(
            detail.name, "编码锁定客户",
            "被拒的请求不得夹带改名（整体 400，什么都没写）"
        );

        // ② 传同一个编码（前端整体提交表单的形态）→ 放行，其他字段照改
        let same = update_customer(
            None,
            &created.id,
            UpdateCustomerReq {
                code: Some(code.clone()),
                name: Some("只改名字".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("传原编码不应报错");
        assert_eq!(same.code, code);
        assert_eq!(same.name, "只改名字");

        // ③ 不传 code（前端「只改其他字段」的形态）= 不改编码；空串 / 纯空白走的是
        //    既有入参校验（`validate` 对 `code` 用 require_text）→ 400「code 不能为空」，
        //    不是本卡的锁（锁只管「换成另一个编码」）。
        let untouched = update_customer(
            None,
            &created.id,
            UpdateCustomerReq {
                name: Some("再改名".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("不传编码应放行");
        assert_eq!(untouched.code, code, "不传不得把编码改掉");
        assert_eq!(untouched.name, "再改名");

        for blank in [String::new(), "   ".to_string()] {
            let err = update_customer(
                None,
                &created.id,
                UpdateCustomerReq {
                    code: Some(blank.clone()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("空编码本来就是非法入参（400）");
            assert_eq!(err.status, 400);
            assert_eq!(err.code, error_code::PARAM_INVALID);
            assert!(err.msg.contains("code"), "文案：{}", err.msg);
            let detail = get_customer(&created.id).await.expect("详情");
            assert_eq!(detail.code, code, "被拒后编码不得被清空（{blank:?}）");
        }

        cleanup().await;
    }

    // ==================== CODE-02：客户编码自动取号 ====================
    //
    // 说明：这几条用例真的会**消耗 CUS 号段**（自动取号是业务要验证的行为本身），
    // 所以只按 id 物理删掉自己建的行，不动 `biz_seq.current_no`：
    //   ① 测试库号段被推高是正常现象，生产库不会被测试碰到（ERP_DB_URL 指向测试库）；
    //   ② 回滚号只有在 insert 失败时才发生，成功建档案就该永久占用一个号（主数据是档案）。

    /// `CUS-0001` 形态（前缀 + `-` + ≥4 位纯流水）。
    fn is_auto_code(code: &str, prefix: &str) -> bool {
        let Some(rest) = code.strip_prefix(prefix) else {
            return false;
        };
        let Some(digits) = rest.strip_prefix('-') else {
            return false;
        };
        digits.len() >= 4 && digits.chars().all(|ch| ch.is_ascii_digit())
    }

    /// 流水号（取最后一段数字）。
    fn seq_no(code: &str) -> i64 {
        code.rsplit('-')
            .next()
            .and_then(|digits| digits.parse::<i64>().ok())
            .unwrap_or_else(|| panic!("编码里没有流水段：{code}"))
    }

    /// 物理删掉本用例造的行（`cleanup()` 只清 `SO2T-` 前缀，自动取号的行要单独删）。
    async fn hard_delete(id: &str) {
        // `?::uuid` 只收 UUID 值，绑 `&str` 会撞「cannot bind parameter to column type uuid」
        let uuid = Uuid::parse_str(id).unwrap_or_else(|_| panic!("id 不是 UUID：{id}"));
        let affected = db()
            .execute_with_params("DELETE FROM biz_customer WHERE id = ?::uuid", vec![Value::from(&uuid)])
            .await
            .expect("物理删客户夹具");
        assert_eq!(affected, 1, "夹具行必须删掉（id={id}）");
    }

    /// ① `null` / `""` / 纯空白三种「没填」都自动取号，且真落库（详情回读一致）。
    #[tokio::test]
    async fn auto_code_generated_when_blank() {
        let Some(_guard) = setup().await else { return };

        // null（字段缺席）
        let by_none = create_customer(None, auto_req("自动编码客户（未传）"))
            .await
            .expect("code 缺席应自动取号");
        assert!(
            is_auto_code(&by_none.code, "CUS"),
            "自动编码必须是 CUS-0001 形态，实际 {}",
            by_none.code
        );
        let detail = get_customer(&by_none.id).await.expect("详情回读");
        assert_eq!(detail.code, by_none.code, "自动编码必须真落库");

        // 空串（前端表单常见形态）
        let mut empty = auto_req("自动编码客户（空串）");
        empty.code = Some(String::new());
        let by_empty = create_customer(None, empty).await.expect("空串应自动取号");
        assert!(is_auto_code(&by_empty.code, "CUS"), "{}", by_empty.code);
        assert_ne!(by_empty.code, by_none.code, "两次自动取号不得同号");

        // 纯空白（用户随手敲了空格）
        let mut blank = auto_req("自动编码客户（空白）");
        blank.code = Some("   ".to_string());
        let by_blank = create_customer(None, blank).await.expect("纯空白应自动取号");
        assert!(is_auto_code(&by_blank.code, "CUS"), "{}", by_blank.code);
        assert_ne!(by_blank.code, by_empty.code, "三次自动取号两两不同");

        hard_delete(&by_none.id).await;
        hard_delete(&by_empty.id).await;
        hard_delete(&by_blank.id).await;
    }

    /// ② 手工编码原样保留（不强制 `CUS-xxx` 形态，企业自有体系要能对接），只 trim。
    #[tokio::test]
    async fn manual_code_is_preserved() {
        let Some(_guard) = setup().await else { return };
        let manual = format!("{CODE_PREFIX}KH-001");
        let created = create_customer(None, req(&format!("  {manual}  "), "手工编码客户"))
            .await
            .expect("手工编码应保留");
        assert_eq!(created.code, manual, "手工编码只 trim，不改成 CUS-xxx");
        assert!(!is_auto_code(&created.code, "CUS"), "不得被改成自动编码");

        hard_delete(&created.id).await;
    }

    /// ③ 连续两次自动取号 → 流水连续（+1）。
    #[tokio::test]
    async fn auto_code_increments_consecutively() {
        let Some(_guard) = setup().await else { return };
        let first = create_customer(None, auto_req("连续取号甲")).await.expect("建甲");
        let second = create_customer(None, auto_req("连续取号乙")).await.expect("建乙");
        assert_eq!(
            seq_no(&second.code),
            seq_no(&first.code) + 1,
            "连续自动取号必须 +1（{} → {}）",
            first.code,
            second.code
        );

        hard_delete(&first.id).await;
        hard_delete(&second.id).await;
    }

    /// ④ 手工编码重复 → `409 + 66001`，文案点名具体编码。
    #[tokio::test]
    async fn duplicate_manual_code_rejected_with_message() {
        let Some(_guard) = setup().await else { return };
        let manual = format!("{CODE_PREFIX}DUP-CODE");
        let created = create_customer(None, req(&manual, "重复编码一号"))
            .await
            .expect("首次创建应成功");
        let err = create_customer(None, req(&manual, "重复编码二号"))
            .await
            .expect_err("重复手工编码必须被拒");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(err.msg.contains(&manual), "错误文案要点名具体编码：{}", err.msg);

        hard_delete(&created.id).await;
    }

    /// ㊴ CODE-05：预览**只读不占号** —— 连点两次拿到同一个号；真正取号发生在保存时。
    #[tokio::test]
    async fn preview_code_does_not_consume_sequence() {
        let Some(_guard) = setup().await else { return };

        let first = preview_customer_code().await.expect("预览应成功");
        assert!(is_auto_code(&first, "CUS"), "预览必须是 CUS-0001 形态，实际 {first}");
        let second = preview_customer_code().await.expect("再预览应成功");
        assert_eq!(first, second, "预览不得推进 biz_seq（方案 B：点了又取消不能浪费号）");

        // 保存时才真取号：留空编码建单拿到的正是刚才预览的号
        let created = create_customer(None, auto_req("预览后建单客户"))
            .await
            .expect("留空编码应自动取号");
        assert_eq!(
            created.code, first,
            "自动取号必须用掉预览的那个号（说明预览没占号、取号在保存时）"
        );

        // 用掉之后，下一个预览必须往后走
        let third = preview_customer_code().await.expect("预览应成功");
        assert_ne!(
            third, first,
            "号已被保存用掉，再预览必须给出新的号（{first} → {third}）"
        );

        hard_delete(&created.id).await;
    }

    /// ㊵ CODE-05：预览号当**手工编码**提交 —— 原样保留（交互方式 B 的保存路径）。
    ///
    /// 这条钉住「点生成 → 填入编码框 → 保存」时后端不能偷偷把它当自动号再取一次。
    #[tokio::test]
    async fn previewed_code_is_used_verbatim_on_create() {
        let Some(_guard) = setup().await else { return };

        let previewed = preview_customer_code().await.expect("预览应成功");
        let created = create_customer(None, req(&previewed, "用预览号建单客户"))
            .await
            .expect("预览号当手工编码应创建成功");
        assert_eq!(created.code, previewed, "手工提交的编码必须原样保留");

        // 该号已被占用 ⇒ 下一个预览必须跳过它（撞号顺延，不会给出重号）
        let after = preview_customer_code().await.expect("预览应成功");
        assert_ne!(after, previewed, "预览必须跳过已占用的号");

        // 同一号再建一次 ⇒ 409 + 66001（并发抢号的兜底口径）
        let conflict = create_customer(None, req(&previewed, "预览号重复客户"))
            .await
            .expect_err("已占用的编码必须被拒");
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.code, error_code::ERP_CODE_CONFLICT, "66001");
        assert!(conflict.msg.contains(&previewed), "文案要点名编码：{}", conflict.msg);

        hard_delete(&created.id).await;
    }

    /// 权限码自检：4 个码、无重复、前缀 `business:erp:customer:`（V0261 的菜单行逐字对应）。
    #[test]
    fn permission_codes_are_unique_and_prefixed() {
        let codes = all_permissions();
        assert_eq!(codes.len(), 4);
        for (index, code) in codes.iter().enumerate() {
            assert!(code.starts_with("business:erp:customer:"), "权限码前缀不对：{code}");
            assert!(!codes[index + 1..].contains(code), "权限码重复：{code}");
        }
    }
}

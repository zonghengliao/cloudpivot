//! 列级脱敏规则的管理接口（批次 B · B6）。
//!
//! # 接口一览（前缀 `/api` 由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/system/field-mask/page` | `system:field-mask:list` | 规则分页（按表 / 列 / 作用域 / 状态） |
//! | POST | `/system/field-mask` | `system:field-mask:create` | 新建规则 |
//! | PUT | `/system/field-mask/{id}` | `system:field-mask:update` | 修改规则（类型 / 参数 / 状态 / 理由） |
//! | DELETE | `/system/field-mask/{id}` | `system:field-mask:delete` | 软删规则 |
//! | GET | `/system/field-mask/preview` | `system:field-mask:list` | 七种类型的试运行 |
//! | GET | `/system/field-mask/effective` | `system:field-mask:list` | 某用户实际生效的规则 |
//!
//! 五个权限码与 `V0065__seed_batchb_permissions` 的 `sys_menu.auth_code` **逐字一致**
//! （不一致 = 接口 403；批次 A 的 V0045 头注释记过同一个坑）。
//!
//! # 写接口的四道校验（每一道都有对应的错误码，不做静默兜底）
//!
//! 1. 标识符格式（`^[a-z][a-z0-9_]{1,63}$`，与表上 `ck_sys_field_mask_ident` 同一条正则）；
//! 2. 表 / 列真实存在于 `information_schema` → 否则 `404 + 61006`（规则指向不存在的列
//!    等于写着好看但永不生效，是运维最常见的"配了没用"）；
//! 3. `mask_type ∈ 1..7` 且**参数能真的跑通**（用该类型的样例值试运行一次）→ 否则
//!    `400 + 61005` / `422 + 61007`；
//! 4. 唯一性 `(table_name, column_name, role_id)`（含全局规则的 `NULL`）→ 冲突 `409`。
//!
//! 写成功后一律 `model::invalidate_all()`：规则是全局配置，改一条会影响所有用户。

use axum::Json;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::net::SocketAddr;

use crate::business::masking::dto::{
    EffectiveMaskQuery, EffectiveMaskRes, EffectiveRuleRes, FieldMaskCreateReq, FieldMaskPageQuery, FieldMaskRes,
    FieldMaskUpdateReq, MaskPreviewQuery, MaskPreviewRes, sample_value,
};
use crate::business::masking::masker::{self, MaskType};
use crate::business::masking::model::{self, NewFieldMaskRule, UpdateFieldMaskRule};
use crate::business::masking::registry::{self, MaskSource, Maskable};
use crate::shared::error_code;
use crate::support::log;
use crate::utils;

/// 标识符正则（与 `V0062` 的 `ck_sys_field_mask_ident` **逐字一致**）。
static IDENT_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[a-z][a-z0-9_]{1,63}$").expect("标识符正则必须可编译"));

// ==================== 分页 ====================

/// 脱敏规则分页。
///
/// # 参数
///
/// - `query`：页码 / 每页条数 / 表名 / 列名 / 作用域 / 状态。
///
/// # 返回值
///
/// `{list, total, page, page_size}`，`list` 每项带「实际生效参数」与角色名。
///
/// # 异常情况
///
/// - 500：数据库异常。
#[nik::permission(
    path = "/system/field-mask/page",
    method = "GET",
    permission = "system:field-mask:list"
)]
#[nik::get("/system/field-mask/page")]
pub async fn page(Query(query): Query<FieldMaskPageQuery>) -> ResResult<PageData<Vec<FieldMaskRes>>> {
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let (rows, total) = model::page_rules(&query, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询脱敏规则"))?;
    let list: Vec<FieldMaskRes> = rows.iter().map(FieldMaskRes::from_row).collect();
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page_no, page_size))
}

// ==================== 新建 ====================

/// 新建脱敏规则。
///
/// # 参数
///
/// - `headers` / `connect_info` / `claims`：审计字段来源；
/// - `req`：规则内容。
///
/// # 返回值
///
/// 建好的规则（含角色名与生效参数）。
///
/// # 异常情况
///
/// - 400 + `61005`：`mask_type` 非法 / 参数格式不符 / 标识符格式非法 / 理由为空；
/// - 403：无权限码；
/// - 404 + `10002`：角色不存在；404 + `61006`：表或列不存在；
/// - 409：同 `(表, 列, 角色)` 的规则已存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/system/field-mask",
    method = "POST",
    permission = "system:field-mask:create"
)]
#[nik::post("/system/field-mask")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<FieldMaskCreateReq>,
) -> ResResult<FieldMaskRes> {
    let operator = current_user_id(&claims)?;
    let table_name = valid_ident("table_name", &req.table_name)?;
    let column_name = valid_ident("column_name", &req.column_name)?;
    let ty = MaskType::from_i16(req.mask_type).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::FIELD_MASK_TYPE_INVALID,
            format!("mask_type 必须是 1..7（当前 {}）", req.mask_type),
        )
    })?;
    let param = normalize_param(ty, req.mask_param.as_deref())?;
    let status = normalize_status(req.status)?;
    let reason = require_reason(&req.reason)?;
    let role_id = match req.role_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => {
            let role_id = utils::require_uuid(raw, "role_id")?;
            let role = model::find_role(&role_id)
                .await
                .map_err(|error| utils::db_error(error, "查询角色"))?;
            if role.is_none() {
                return Err(ApiError::new(
                    404,
                    error_code::RESOURCE_NOT_FOUND,
                    format!("角色不存在或已删除：{role_id}"),
                ));
            }
            Some(role_id)
        }
    };

    // 表 / 列存在性（404 + 61006）
    let exists = model::column_exists(&table_name, &column_name)
        .await
        .map_err(|error| utils::db_error(error, "校验脱敏目标列"))?;
    if !exists {
        return Err(ApiError::new(
            404,
            error_code::FIELD_MASK_TARGET_NOT_FOUND,
            format!("表 {table_name} 上不存在列 {column_name}（规则会永不生效，已拒绝）"),
        ));
    }

    // 唯一性（先查后写；并发下仍可能撞唯一索引，写失败时按 409 兜底）
    if find_same_rule(&table_name, &column_name, role_id.as_ref())
        .await?
        .is_some()
    {
        return Err(duplicate_rule_error(&table_name, &column_name, role_id.as_ref()));
    }

    model::insert_rule(&NewFieldMaskRule {
        table_name: &table_name,
        column_name: &column_name,
        mask_type: ty.as_i16(),
        mask_param: param.as_deref(),
        role_id,
        status,
        reason: &reason,
        operator,
    })
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            duplicate_rule_error(&table_name, &column_name, role_id.as_ref())
        } else {
            utils::db_error(error, "新建脱敏规则")
        }
    })?;

    let invalidated = model::invalidate_all().await;
    tracing::info!(
        %table_name, %column_name, mask_type = ty.as_i16(), role_id = ?role_id,
        status, invalidated_keys = invalidated, operator = %operator,
        "脱敏规则已新建（缓存已清）"
    );
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "FIELD_MASK_CREATE",
            "field_mask",
            format!("{table_name}.{column_name}"),
            None,
            Some(serde_json::json!({
                "table_name": table_name,
                "column_name": column_name,
                "mask_type": ty.as_i16(),
                "mask_param": param,
                "role_id": role_id.map(|id| id.to_string()),
                "status": status,
                "reason": reason,
            })),
        ),
    );

    let created = find_same_rule(&table_name, &column_name, role_id.as_ref())
        .await?
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "规则写入成功但回读失败"))?;
    Ok(Res::ok(FieldMaskRes::from_row(&created)))
}

// ==================== 修改 ====================

/// 修改脱敏规则（表 / 列 / 角色不可改）。
///
/// # 参数
///
/// - `id`：规则 ID；`headers` / `connect_info` / `claims`：审计来源；`req`：新值。
///
/// # 返回值
///
/// 修改后的规则。
///
/// # 异常情况
///
/// - 400 + `10001`：ID 格式非法；400 + `61005`/`422 + 61007`：类型或参数非法；
/// - 404 + `10002`：规则不存在或已删除；
/// - 500：数据库异常。
#[nik::permission(
    path = "/system/field-mask/{id}",
    method = "PUT",
    permission = "system:field-mask:update"
)]
#[nik::put("/system/field-mask/{id}")]
pub async fn update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<FieldMaskUpdateReq>,
) -> ResResult<FieldMaskRes> {
    let id = utils::require_uuid(&id, "id")?;
    let operator = current_user_id(&claims)?;
    let existing = require_rule(&id).await?;
    let ty = MaskType::from_i16(req.mask_type).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::FIELD_MASK_TYPE_INVALID,
            format!("mask_type 必须是 1..7（当前 {}）", req.mask_type),
        )
    })?;
    let param = normalize_param(ty, req.mask_param.as_deref())?;
    let status = normalize_status(req.status)?;
    let reason = require_reason(&req.reason)?;

    let affected = model::update_rule(
        &id,
        &UpdateFieldMaskRule {
            mask_type: ty.as_i16(),
            mask_param: param.as_deref(),
            status,
            reason: &reason,
            operator,
        },
    )
    .await
    .map_err(|error| utils::db_error(error, "修改脱敏规则"))?;
    if affected == 0 {
        return Err(rule_not_found(&id));
    }
    let invalidated = model::invalidate_all().await;
    tracing::info!(
        rule_id = %id, mask_type = ty.as_i16(), status, invalidated_keys = invalidated,
        operator = %operator, "脱敏规则已修改（缓存已清）"
    );
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "FIELD_MASK_UPDATE",
            "field_mask",
            id.to_string(),
            Some(serde_json::json!({
                "mask_type": existing.mask_type,
                "mask_param": existing.mask_param,
                "status": existing.status,
                "reason": existing.reason,
            })),
            Some(serde_json::json!({
                "mask_type": ty.as_i16(),
                "mask_param": param,
                "status": status,
                "reason": reason,
            })),
        ),
    );
    let updated = require_rule(&id).await?;
    Ok(Res::ok(FieldMaskRes::from_row(&updated)))
}

// ==================== 删除 ====================

/// 软删脱敏规则。
///
/// # 参数
///
/// - `id`：规则 ID；`headers` / `connect_info` / `claims`：审计来源。
///
/// # 返回值
///
/// `{ "id": "...", "deleted": true }`。
///
/// # 异常情况
///
/// - 400 + `10001`：ID 格式非法；404 + `10002`：规则不存在或已删除；500：数据库异常。
#[nik::permission(
    path = "/system/field-mask/{id}",
    method = "DELETE",
    permission = "system:field-mask:delete"
)]
#[nik::delete("/system/field-mask/{id}")]
pub async fn delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<serde_json::Value> {
    let id = utils::require_uuid(&id, "id")?;
    let operator = current_user_id(&claims)?;
    let existing = require_rule(&id).await?;
    let affected = model::soft_delete_rule(&id, &operator)
        .await
        .map_err(|error| utils::db_error(error, "删除脱敏规则"))?;
    if affected == 0 {
        return Err(rule_not_found(&id));
    }
    let invalidated = model::invalidate_all().await;
    tracing::info!(
        rule_id = %id, invalidated_keys = invalidated, operator = %operator,
        "脱敏规则已软删（缓存已清）"
    );
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "FIELD_MASK_DELETE",
            "field_mask",
            id.to_string(),
            Some(serde_json::json!({
                "table_name": existing.table_name,
                "column_name": existing.column_name,
                "mask_type": existing.mask_type,
                "role_id": existing.role_id.map(|r| r.to_string()),
            })),
            None,
        ),
    );
    Ok(Res::ok(serde_json::json!({
        "id": id.to_string(),
        "deleted": true,
        "invalidated_keys": invalidated,
    })))
}

// ==================== 试运行 ====================

/// 七种类型的试运行（配置界面「测试一下」按钮）。
///
/// # 参数
///
/// - `query`：`mask_type` + `mask_param` + `value`。
///
/// # 返回值
///
/// 原文 / 脱敏后 / 是否变化。**脱敏报错时返回明确错误**，不返回明文。
///
/// # 异常情况
///
/// - 400 + `61005`：类型或参数非法；422 + `61007`：正则非法。
#[nik::permission(
    path = "/system/field-mask/preview",
    method = "GET",
    permission = "system:field-mask:list"
)]
#[nik::get("/system/field-mask/preview")]
pub async fn preview(Query(query): Query<MaskPreviewQuery>) -> ResResult<MaskPreviewRes> {
    let ty = MaskType::from_i16(query.mask_type).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::FIELD_MASK_TYPE_INVALID,
            format!("mask_type 必须是 1..7（当前 {}）", query.mask_type),
        )
    })?;
    let param = normalize_param(ty, query.mask_param.as_deref())?;
    let input = query.value.unwrap_or_else(|| sample_value(ty).to_string());
    let output = masker::apply(ty, param.as_deref(), &input).map_err(mask_error)?;
    Ok(Res::ok(MaskPreviewRes {
        mask_type: ty.as_i16(),
        mask_type_code: ty.code().to_string(),
        mask_type_label: ty.label().to_string(),
        mask_param: param.unwrap_or_default(),
        changed: output != input,
        input,
        output,
    }))
}

// ==================== 某用户实际生效的规则 ====================

/// 某用户实际生效的脱敏规则（含来源：全局 / 角色名 + 试运行样例）。
///
/// # 参数
///
/// - `claims`：当前用户（缺省查自己）；
/// - `query.user_id`：目标用户。
///
/// # 返回值
///
/// [`EffectiveMaskRes`]：是否豁免、豁免原因、生效规则清单。
///
/// # 异常情况
///
/// - 400 + `10001`：`user_id` 格式非法；404 + `10002`：用户不存在；500：数据库异常。
#[nik::permission(
    path = "/system/field-mask/effective",
    method = "GET",
    permission = "system:field-mask:list"
)]
#[nik::get("/system/field-mask/effective")]
pub async fn effective(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<EffectiveMaskQuery>,
) -> ResResult<EffectiveMaskRes> {
    let target = match query.user_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => current_user_id(&claims)?,
        Some(raw) => utils::require_uuid(raw, "user_id")?,
    };
    let user = model::find_user_brief(&target)
        .await
        .map_err(|error| utils::db_error(error, "查询用户"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("用户不存在或已删除：{target}"),
            )
        })?;
    let is_super = model::is_super_admin(&target)
        .await
        .map_err(|error| utils::db_error(error, "查询用户角色"))?;
    // 目标用户的解析：不传 claims（这是「他」会看到什么，不是「我」的明版权限）
    let set = if is_super {
        registry::resolve_super()
    } else {
        registry::load_user_set(&target)
            .await
            .map_err(|error| utils::db_error(error, "解析生效脱敏规则"))?
    };
    let rules = set
        .rules()
        .iter()
        .map(|rule| EffectiveRuleRes {
            rule_id: rule.rule_id.clone(),
            table_name: rule.table.clone(),
            column_name: rule.column.clone(),
            mask_type: rule.mask_type,
            mask_type_label: rule.ty().map(|ty| ty.label().to_string()).unwrap_or_default(),
            mask_param: rule.effective_param().unwrap_or_default().to_string(),
            source: rule.source.code().to_string(),
            source_name: rule.role_name.clone(),
            sample: rule
                .ty()
                .and_then(|ty| masker::apply(ty, rule.effective_param(), sample_value(ty)).ok())
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    Ok(Res::ok(EffectiveMaskRes {
        user_id: user.id.to_string(),
        username: user.username,
        is_super_admin: is_super,
        exempt: set.is_exempt(),
        exempt_reason: if is_super {
            registry::ExemptReason::SuperAdmin.code().to_string()
        } else {
            registry::ExemptReason::None.code().to_string()
        },
        total: rules.len(),
        rules,
    }))
}

// ==================== 公共辅助 ====================

/// 取令牌里的用户 ID；缺失 / 非法 → `401 + 20005`。
fn current_user_id(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "当前令牌缺少有效的用户标识，请重新登录",
        )
    })
}

/// 校验标识符（与表上的 CHECK 同一条正则）。返回 trim 后的值。
fn valid_ident(field: &str, raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if !IDENT_RE.is_match(text) {
        return Err(ApiError::new(
            400,
            error_code::FIELD_MASK_TYPE_INVALID,
            format!("{field} 必须是「小写字母开头、仅含小写字母/数字/下划线、长度 2-64」：{text}"),
        ));
    }
    Ok(text.to_string())
}

/// 规范化并**试运行**参数：跑不通的参数当场拒绝（不留到运行期再炸）。
///
/// # 参数
///
/// - `ty`：脱敏类型；
/// - `raw`：请求里的 `mask_param`。
///
/// # 返回值
///
/// `None` = 该类型无参数；`Some(text)` = 落库的参数文本。
fn normalize_param(ty: MaskType, raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let text = raw.map(str::trim).filter(|item| !item.is_empty());
    if matches!(ty, MaskType::Email) {
        if text.is_some() {
            return Err(ApiError::new(
                400,
                error_code::FIELD_MASK_TYPE_INVALID,
                "邮箱脱敏没有参数，mask_param 请留空",
            ));
        }
        return Ok(None);
    }
    let candidate = text.map(str::to_string);
    // 用样例值试运行：参数坏掉在这里就报错（判据与运行时完全一致）
    masker::apply(ty, candidate.as_deref(), sample_value(ty)).map_err(mask_error)?;
    Ok(candidate)
}

/// 状态只允许 0 / 1。
fn normalize_status(raw: Option<i16>) -> std::result::Result<i16, ApiError> {
    let status = raw.unwrap_or(1);
    if status != 0 && status != 1 {
        return Err(ApiError::new(
            400,
            error_code::FIELD_MASK_TYPE_INVALID,
            format!("status 只能是 0（停用）或 1（启用），当前 {status}"),
        ));
    }
    Ok(status)
}

/// 脱敏理由必填（合规留痕：审计要能回答「为什么要脱这一列」）。
fn require_reason(raw: &str) -> std::result::Result<String, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::FIELD_MASK_TYPE_INVALID,
            "脱敏理由（reason）不能为空：这是合规留痕字段",
        ));
    }
    if text.chars().count() > 255 {
        return Err(ApiError::new(
            400,
            error_code::FIELD_MASK_TYPE_INVALID,
            format!("脱敏理由最多 255 个字符（当前 {}）", text.chars().count()),
        ));
    }
    Ok(text.to_string())
}

/// 把 [`masker::MaskError`] 转成接口错误（码值与文案都由它给出）。
fn mask_error(error: masker::MaskError) -> ApiError {
    ApiError::new(error.http_status(), error.error_code(), error.message())
}

/// 按 ID 取未删除规则；不存在 → `404 + 10002`。
async fn require_rule(id: &Uuid) -> std::result::Result<model::FieldMaskRow, ApiError> {
    model::find_rule(id)
        .await
        .map_err(|error| utils::db_error(error, "查询脱敏规则"))?
        .ok_or_else(|| rule_not_found(id))
}

/// 规则不存在错误。
fn rule_not_found(id: &Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("脱敏规则不存在或已删除：{id}"),
    )
}

/// 按 `(表, 列, 角色)` 找未删除规则（唯一性判定 + 写入后回读共用）。
async fn find_same_rule(
    table_name: &str,
    column_name: &str,
    role_id: Option<&Uuid>,
) -> std::result::Result<Option<model::FieldMaskRow>, ApiError> {
    let query = FieldMaskPageQuery {
        table_name: Some(table_name.to_string()),
        column_name: Some(column_name.to_string()),
        scope: Some(if role_id.is_some() { "role" } else { "global" }.to_string()),
        ..Default::default()
    };
    let (rows, _) = model::page_rules(&query, 1, 50)
        .await
        .map_err(|error| utils::db_error(error, "查询同列规则"))?;
    Ok(rows.into_iter().find(|row| row.role_id.as_ref() == role_id))
}

/// 规则重复错误（唯一索引 `uk_sys_field_mask_rule`，全局规则的 `NULL` 也算冲突）。
fn duplicate_rule_error(table_name: &str, column_name: &str, role_id: Option<&Uuid>) -> ApiError {
    let scope = match role_id {
        Some(id) => format!("角色 {id}"),
        None => "全局".to_string(),
    };
    ApiError::new(
        409,
        error_code::RESOURCE_CONFLICT,
        format!("{table_name}.{column_name} 已存在{scope}脱敏规则，请直接修改那一条"),
    )
}

/// 是否唯一约束冲突（并发下先查后写仍可能撞索引）。
fn is_unique_violation(error: &DbError) -> bool {
    let text = error.to_string();
    text.contains("uk_sys_field_mask_rule") || text.contains("duplicate key")
}

/// 供 `effective` 接口展示用的「超管豁免」集合。
///
/// # 返回值
///
/// 一个 `MaskSet`，其 `is_exempt()` 为 true（超管看明文）。
pub fn super_exempt_set() -> registry::MaskSet {
    registry::resolve_super()
}

/// 判据：`MaskSource` 的展示名（日志 / 自检接口用）。
pub fn source_label(source: MaskSource) -> &'static str {
    match source {
        MaskSource::Global => "全局",
        MaskSource::Role => "角色",
    }
}

/// 该 DTO 的表名（编译期契约：`UserRes` 必须能说出自己属于哪张表）。
pub fn user_dto_table() -> &'static str {
    <crate::system::user::dto::UserRes as Maskable>::mask_table()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ident_regex_matches_database_check() {
        assert!(IDENT_RE.is_match("sys_user"));
        assert!(IDENT_RE.is_match("a1"));
        assert!(!IDENT_RE.is_match("SysUser"), "大写应拒绝");
        assert!(!IDENT_RE.is_match("1user"), "数字开头应拒绝");
        assert!(!IDENT_RE.is_match("sys-user"), "连字符应拒绝");
        assert!(!IDENT_RE.is_match("sys_user; drop table x"), "注入形态应拒绝");
        assert!(
            !IDENT_RE.is_match("a"),
            "长度 1 应拒绝（与 CHECK 的 2..64 字符约定一致）"
        );
    }

    #[test]
    fn email_type_rejects_param() {
        let error = normalize_param(MaskType::Email, Some("1,2")).expect_err("邮箱不接受参数");
        assert_eq!(error.code, error_code::FIELD_MASK_TYPE_INVALID);
        assert!(normalize_param(MaskType::Email, None).expect("应通过").is_none());
    }

    #[test]
    fn param_is_validated_by_dry_run() {
        assert!(normalize_param(MaskType::Mobile, Some("3")).is_err());
        assert!(normalize_param(MaskType::Mobile, Some("3,4")).is_ok());
        assert!(normalize_param(MaskType::Regex, Some("^(x|1")).is_err());
        assert!(normalize_param(MaskType::Regex, Some(r"^(\w{2}).*$|1")).is_ok());
        // 缺参数的自定义正则在配置侧就应被拒绝（不留到运行期）
        assert!(normalize_param(MaskType::Regex, None).is_err());
    }

    #[test]
    fn status_and_reason_rules() {
        assert_eq!(normalize_status(None).expect("缺省启用"), 1);
        assert_eq!(normalize_status(Some(0)).expect("允许停用"), 0);
        assert!(normalize_status(Some(2)).is_err());
        assert!(require_reason("  ").is_err());
        assert_eq!(require_reason(" 合规要求 ").expect("应通过"), "合规要求");
        assert!(require_reason(&"很".repeat(256)).is_err());
    }

    #[test]
    fn empty_set_is_not_exempt() {
        let set = registry::MaskSet::empty();
        assert!(!set.is_exempt());
        assert!(set.is_empty());
        // 超管豁免集合
        assert!(super_exempt_set().is_exempt());
    }

    #[test]
    fn dto_table_contract() {
        assert_eq!(user_dto_table(), "sys_user");
        assert_eq!(source_label(MaskSource::Global), "全局");
    }
}

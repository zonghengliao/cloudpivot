//! 存储配置管理接口（批次 A · A1）。
//!
//! # 接口清单（前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/storage/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/storage/config/page` | `business:storage:config:list` | 后端配置分页（AK 掩码） |
//! | GET | `/storage/config/{id}` | `business:storage:config:list` | 配置详情（AK 掩码） |
//! | POST | `/storage/config` | `business:storage:config:create` | 新建（明文入参，落库前加密） |
//! | PUT | `/storage/config/{id}` | `business:storage:config:update` | 修改（不传密钥表示不改） |
//! | DELETE | `/storage/config/{id}` | `business:storage:config:delete` | 删除（默认后端 / 被引用时 409） |
//! | POST | `/storage/config/{id}/default` | `business:storage:config:update` | 设为默认 |
//! | POST | `/storage/config/{id}/test` | `business:storage:config:test` | 连通性探测（写 1 KB 探针） |
//! | GET | `/storage/policy/page` | `business:storage:policy:list` | 上传策略分页 |
//! | PUT | `/storage/policy/{id}` | `business:storage:policy:update` | 修改上传策略 |
//!
//! # 安全约束（逐条对应架构文档 §5.5）
//!
//! - **R3 AK 泄露**：入参收明文、落库即加密；**响应永不含明文**（见
//!   [`StorageConfigRes::from_model`]），审计日志也只记编码 / 类型 / 是否默认；
//! - **R2 SSRF**：`endpoint` 保存前过 [`crate::business::storage::store::ensure_endpoint_allowed`]，
//!   拒绝回环 / 私网 / 链路本地字面量地址（如 `169.254.169.254`）；
//! - **默认后端唯一**：改默认走事务 + `uk_sys_storage_config_default` 兜底；
//! - **删除保护**：默认后端、被 `sys_file.storage_code` 引用的后端一律 409。
//!
//! # 文件顺序约定
//!
//! 路由方法在前，公共辅助函数在后（与底座其它 handler 一致）。

use alun::prelude::*;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;

use std::net::SocketAddr;

use crate::business::storage::cipher::{self, MasterKey};
use crate::business::storage::dto::{
    StorageConfigPageQuery, StorageConfigRes, StorageConfigSaveReq, StoragePolicyPageQuery,
    StoragePolicyRes, StoragePolicySaveReq, StorageTestRes,
};
use crate::business::storage::model::{
    self, StorageConfigFilter, StoragePolicyFilter, SysStorageConfigModel, SysStoragePolicyModel,
};
use crate::business::storage::{reload_router_best_effort, router as storage_router, store};
use crate::shared::constants::EnableStatus;
use crate::shared::error_code;
use crate::support::log;
use crate::utils;

// ==================== 存储配置 ====================

/// 分页查询存储后端配置。
///
/// # 参数
///
/// - `query`：分页与过滤条件（关键词 / 后端类型 / 状态）。
///
/// # 返回值
///
/// 分页数据，`ak` 字段为掩码（如 `CPAK****9f2c`），**不含任何明文凭据**。
///
/// # 异常情况
///
/// - 400：状态值非法；
/// - 403：无 `business:storage:config:list`；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/storage/config/page",
    method = "GET",
    permission = "business:storage:config:list"
)]
#[alun::get("/business/storage/config/page")]
pub async fn config_page(
    Query(query): Query<StorageConfigPageQuery>,
) -> ResResult<PageData<Vec<StorageConfigRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let status = query.status.map(EnableStatus::from_i16).transpose()?;
    let filter = StorageConfigFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        backend_type: utils::trimmed(query.backend_type.as_deref()).map(str::to_string),
        status,
    };

    let result = model::page_configs(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询存储配置"))?;

    // 主密钥缺失时列表照常返回（掩码降级为 ****），不因运维漏配 env 就打不开页面
    let key = MasterKey::from_env().ok();
    let list = result
        .list
        .iter()
        .map(|config| StorageConfigRes::from_model(config, key.as_ref()))
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 存储后端配置详情。
///
/// # 参数
///
/// - `id`：配置 ID。
///
/// # 返回值
///
/// 配置详情（AK 掩码）。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：配置不存在。
#[alun::permission(
    path = "/business/storage/config/{id}",
    method = "GET",
    permission = "business:storage:config:list"
)]
#[alun::get("/business/storage/config/{id}")]
pub async fn config_detail(Path(id): Path<String>) -> ResResult<StorageConfigRes> {
    let id = utils::require_uuid(&id, "id")?;
    let config = SysStorageConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|error| utils::db_error(error, "查询存储配置详情"))?
        .filter(|config| config.deleted_at.is_none())
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "存储配置不存在"))?;
    let key = MasterKey::from_env().ok();
    Ok(Res::ok(StorageConfigRes::from_model(&config, key.as_ref())))
}

/// 新建存储后端配置。
///
/// # 参数
///
/// - `headers` / `connect_info`：审计日志上下文。
/// - `claims`：当前用户（审计字段）。
/// - `req`：配置请求体（S3 必须带 AK/SK 明文）。
///
/// # 返回值
///
/// 新建的配置（AK 掩码）。
///
/// # 异常情况
///
/// - 400：编码格式非法 / 本地后端带凭据 / 只给了半个凭据 / endpoint 指向内网；
/// - 409：编码已存在；
/// - 500：主密钥缺失（无法加密）或数据库异常。
#[alun::permission(
    path = "/business/storage/config",
    method = "POST",
    permission = "business:storage:config:create"
)]
#[alun::post("/business/storage/config")]
pub async fn config_create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<StorageConfigSaveReq>,
) -> ResResult<StorageConfigRes> {
    let key = master_key()?;
    let config_code = req.config_code.trim().to_string();
    validate_config_code(&config_code)?;

    if model::count_config_code(&config_code, None)
        .await
        .map_err(|error| utils::db_error(error, "校验后端编码唯一性"))?
        > 0
    {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "后端编码已存在",
        ));
    }

    let make_default = req.is_default.unwrap_or(false);
    // 首个后端自动成为默认（否则「建好了却用不了」）
    let is_first = model::load_enabled_configs()
        .await
        .map_err(|error| utils::db_error(error, "查询现有存储后端"))?
        .is_empty();

    let mut config = build_config(&req, None, utils::current_user_uuid(&claims), &key)?;
    if make_default || is_first {
        // 先不置默认，插入后再走「先清后设」事务，避免唯一索引与插入顺序打架
        config.is_default = false;
    }
    config
        .insert(db())
        .await
        .map_err(|error| utils::db_error(error, "新建存储配置"))?;

    if make_default || is_first {
        model::set_default(&config.id)
            .await
            .map_err(|error| utils::db_error(error, "设置默认存储后端"))?;
        config.is_default = true;
    }

    reload_router_best_effort("新建存储配置").await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_CONFIG_CREATE",
            "StorageConfig",
            config.id.to_string(),
            None,
            // 审计只记非敏感元数据，绝不写 AK/SK 与其密文
            Some(audit_snapshot(&config)),
        ),
    );

    Ok(Res::ok(StorageConfigRes::from_model(&config, Some(&key))))
}

/// 修改存储后端配置。
///
/// # 参数
///
/// - `id`：配置 ID。
/// - `req`：修改请求（`ak`/`sk` 都为空表示不修改密钥）。
///
/// # 返回值
///
/// 修改后的配置（AK 掩码）。
///
/// # 异常情况
///
/// - 400：试图修改编码 / 后端类型与现有凭据冲突 / endpoint 指向内网；
/// - 404：配置不存在；
/// - 500：主密钥缺失或数据库异常。
#[alun::permission(
    path = "/business/storage/config/{id}",
    method = "PUT",
    permission = "business:storage:config:update"
)]
#[alun::put("/business/storage/config/{id}")]
pub async fn config_update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<StorageConfigSaveReq>,
) -> ResResult<StorageConfigRes> {
    let id = utils::require_uuid(&id, "id")?;
    let key = master_key()?;
    let existing = SysStorageConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|error| utils::db_error(error, "查询待修改存储配置"))?
        .filter(|config| config.deleted_at.is_none())
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "存储配置不存在"))?;

    // 编码是 sys_file.storage_code 的引用值，建后不可改（改了历史文件就找不到后端）
    if req.config_code.trim() != existing.config_code {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "后端编码不允许修改（已被文件记录引用），如需变更请新建后端",
        ));
    }

    let mut updated = build_config(
        &req,
        Some(&existing),
        utils::current_user_uuid(&claims),
        &key,
    )?;
    updated.is_default = existing.is_default;

    updated
        .update(db())
        .await
        .map_err(|error| utils::db_error(error, "修改存储配置"))?;

    if req.is_default == Some(true) && !existing.is_default {
        model::set_default(&updated.id)
            .await
            .map_err(|error| utils::db_error(error, "设置默认存储后端"))?;
        updated.is_default = true;
    }

    reload_router_best_effort("修改存储配置").await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_CONFIG_UPDATE",
            "StorageConfig",
            updated.id.to_string(),
            Some(audit_snapshot(&existing)),
            Some(audit_snapshot(&updated)),
        ),
    );

    Ok(Res::ok(StorageConfigRes::from_model(&updated, Some(&key))))
}

/// 删除存储后端配置（软删）。
///
/// # 参数
///
/// - `id`：配置 ID。
///
/// # 返回值
///
/// 成功返回 `code = 0`。
///
/// # 异常情况
///
/// - 404：配置不存在；
/// - 409：默认后端不可删除 / 仍被文件记录引用。
#[alun::permission(
    path = "/business/storage/config/{id}",
    method = "DELETE",
    permission = "business:storage:config:delete"
)]
#[alun::delete("/business/storage/config/{id}")]
pub async fn config_delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = SysStorageConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|error| utils::db_error(error, "查询待删除存储配置"))?
        .filter(|config| config.deleted_at.is_none())
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "存储配置不存在"))?;

    if existing.is_default {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "该后端是当前默认后端，请先把其它后端设为默认",
        ));
    }

    let used = model::count_files_using(&existing.config_code)
        .await
        .map_err(|error| utils::db_error(error, "统计后端引用"))?;
    if used > 0 {
        return Err(ApiError::new(
            409,
            error_code::STORAGE_CONFIG_IN_USE,
            format!("该后端仍被 {used} 个文件记录引用，不能删除"),
        ));
    }

    SysStorageConfigModel::of(id)
        .delete(db())
        .await
        .map_err(|error| utils::db_error(error, "删除存储配置"))?;

    reload_router_best_effort("删除存储配置").await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_CONFIG_DELETE",
            "StorageConfig",
            id.to_string(),
            Some(audit_snapshot(&existing)),
            None,
        ),
    );

    Ok(Res::ok(()))
}

/// 把指定后端设为默认（先清后设，事务 + 唯一索引兜底）。
///
/// # 参数
///
/// - `id`：配置 ID。
///
/// # 返回值
///
/// 成功返回 `code = 0`。
///
/// # 异常情况
///
/// - 404：配置不存在；
/// - 409：配置已停用（60002）。
#[alun::permission(
    path = "/business/storage/config/{id}/default",
    method = "POST",
    permission = "business:storage:config:update"
)]
#[alun::post("/business/storage/config/{id}/default")]
pub async fn config_set_default(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = SysStorageConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|error| utils::db_error(error, "查询存储配置"))?
        .filter(|config| config.deleted_at.is_none())
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "存储配置不存在"))?;

    if existing.status != EnableStatus::enabled {
        return Err(ApiError::new(
            409,
            error_code::STORAGE_BACKEND_DISABLED,
            "该后端已停用，不能设为默认",
        ));
    }

    model::set_default(&id)
        .await
        .map_err(|error| utils::db_error(error, "设置默认存储后端"))?;
    reload_router_best_effort("设置默认存储后端").await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_CONFIG_SET_DEFAULT",
            "StorageConfig",
            id.to_string(),
            None,
            Some(serde_json::json!({ "config_code": existing.config_code })),
        ),
    );

    Ok(Res::ok(()))
}

/// 连通性测试（本地后端写 1 KB 探针对象再删；S3 后端发已签名的桶列举请求）。
///
/// # 参数
///
/// - `id`：配置 ID。
///
/// # 返回值
///
/// 探测结果，同时把 `health_status` / `last_check_at` / `last_error` 回写到配置行。
///
/// # 异常情况
///
/// - 404：配置不存在；
/// - 500：主密钥缺失（S3 后端无法解密凭据）。
#[alun::permission(
    path = "/business/storage/config/{id}/test",
    method = "POST",
    permission = "business:storage:config:test"
)]
#[alun::post("/business/storage/config/{id}/test")]
pub async fn config_test(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<StorageTestRes> {
    let id = utils::require_uuid(&id, "id")?;
    let key = master_key()?;
    let config = SysStorageConfigModel::find_or_none(db(), &id)
        .await
        .map_err(|error| utils::db_error(error, "查询存储配置"))?
        .filter(|config| config.deleted_at.is_none())
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "存储配置不存在"))?;

    let started = std::time::Instant::now();
    let probed = match storage_router::build_store(&config, &key) {
        Ok(store) => store.health().await,
        Err(error) => Err(store::StoreError::Invalid(error.msg)),
    };
    let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let (healthy, message) = match probed {
        Ok(()) => (true, "连接成功".to_string()),
        Err(error) => (false, error.message()),
    };
    // 先绑定再取 &str：直接写 `Some(truncate(...).as_str())` 会在语句结束时释放临时值
    let error_text = truncate(&message, 500);

    if let Err(error) = model::update_health(
        &id,
        if healthy { "HEALTHY" } else { "DOWN" },
        if healthy {
            None
        } else {
            Some(error_text.as_str())
        },
    )
    .await
    {
        // 回写失败不影响探测结论（用户已经拿到结果）
        tracing::warn!(error = %error, "回写存储健康状态失败");
    }

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_CONFIG_TEST",
            "StorageConfig",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "config_code": config.config_code,
                "healthy": healthy,
                "latency_ms": latency_ms,
            })),
        ),
    );

    Ok(Res::ok(StorageTestRes {
        config_code: config.config_code,
        backend_type: config.backend_type,
        healthy,
        latency_ms,
        message,
        checked_at: chrono::Utc::now(),
    }))
}

// ==================== 上传策略 ====================

/// 分页查询上传策略。
///
/// # 参数
///
/// - `query`：分页与过滤条件。
///
/// # 返回值
///
/// 策略分页数据。
///
/// # 异常情况
///
/// - 400：状态值非法；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/storage/policy/page",
    method = "GET",
    permission = "business:storage:policy:list"
)]
#[alun::get("/business/storage/policy/page")]
pub async fn policy_page(
    Query(query): Query<StoragePolicyPageQuery>,
) -> ResResult<PageData<Vec<StoragePolicyRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let status = query.status.map(EnableStatus::from_i16).transpose()?;
    let filter = StoragePolicyFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        status,
    };

    let result = model::page_policies(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询上传策略"))?;
    let list = result
        .list
        .iter()
        .map(StoragePolicyRes::from_model)
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 修改上传策略。
///
/// # 参数
///
/// - `id`：策略 ID。
/// - `req`：策略请求体（大小上限 / 扩展名 / MIME / 直传 / TTL）。
///
/// # 返回值
///
/// 修改后的策略。
///
/// # 异常情况
///
/// - 400：配置值非法（大小 0 / TTL 越界 / 扩展名带点）；
/// - 404：策略不存在；
/// - 422：`storage_code` 指向不存在的后端。
#[alun::permission(
    path = "/business/storage/policy/{id}",
    method = "PUT",
    permission = "business:storage:policy:update"
)]
#[alun::put("/business/storage/policy/{id}")]
pub async fn policy_update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<StoragePolicySaveReq>,
) -> ResResult<StoragePolicyRes> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = SysStoragePolicyModel::find_or_none(db(), &id)
        .await
        .map_err(|error| utils::db_error(error, "查询上传策略"))?
        .filter(|policy| policy.deleted_at.is_none())
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "上传策略不存在"))?;

    let allowed_extensions = req.allowed_extensions.clone().unwrap_or_default();
    let allowed_mimes = req.allowed_mimes.clone().unwrap_or_default();
    model::validate_policy_fields(
        req.max_size_mb,
        req.presign_ttl_secs,
        &allowed_extensions,
        &allowed_mimes,
    )
    .map_err(|message| ApiError::new(400, error_code::PARAM_INVALID, message))?;

    let storage_code = utils::trimmed(req.storage_code.as_deref()).map(str::to_string);
    if let Some(code) = storage_code.as_deref()
        && model::find_by_code(code)
            .await
            .map_err(|error| utils::db_error(error, "校验策略指定后端"))?
            .is_none()
    {
        return Err(ApiError::new(
            422,
            error_code::STORAGE_BACKEND_UNAVAILABLE,
            format!("策略指定的后端 {code} 不存在"),
        ));
    }

    let mut policy = existing.clone();
    policy.category_name = req.category_name.trim().to_string();
    policy.max_size_mb = req.max_size_mb;
    policy.allowed_extensions = allowed_extensions;
    policy.allowed_mimes = allowed_mimes;
    policy.allow_overwrite = req.allow_overwrite.unwrap_or(existing.allow_overwrite);
    policy.direct_upload = req.direct_upload.unwrap_or(existing.direct_upload);
    policy.presign_ttl_secs = req.presign_ttl_secs;
    policy.storage_code = storage_code;
    policy.status = match req.status {
        Some(value) => EnableStatus::from_i16(value)?,
        None => existing.status,
    };
    policy.sort_order = req.sort_order.unwrap_or(existing.sort_order);
    policy.remark = req.remark.clone().or(existing.remark.clone());
    policy.updated_at = chrono::Utc::now();
    policy.update_by = utils::current_user_uuid(&claims);

    policy
        .update(db())
        .await
        .map_err(|error| utils::db_error(error, "修改上传策略"))?;

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_POLICY_UPDATE",
            "StoragePolicy",
            policy.id.to_string(),
            Some(serde_json::json!({
                "category": existing.category,
                "max_size_mb": existing.max_size_mb,
                "allowed_extensions": existing.allowed_extensions,
                "direct_upload": existing.direct_upload,
                "presign_ttl_secs": existing.presign_ttl_secs,
            })),
            Some(serde_json::json!({
                "category": policy.category,
                "max_size_mb": policy.max_size_mb,
                "allowed_extensions": policy.allowed_extensions,
                "direct_upload": policy.direct_upload,
                "presign_ttl_secs": policy.presign_ttl_secs,
            })),
        ),
    );

    Ok(Res::ok(StoragePolicyRes::from_model(&policy)))
}

// ==================== 公共辅助 ====================

/// 读取主密钥（缺失 / 非法时返回 500，消息含修复步骤但不含密钥）。
///
/// # 返回值
///
/// 主密钥实例。
///
/// # 异常情况
///
/// 环境变量缺失或格式非法返回 500。
fn master_key() -> std::result::Result<MasterKey, ApiError> {
    MasterKey::from_env().map_err(|error| {
        tracing::error!(error = %error, "存储主密钥不可用");
        ApiError::new(
            500,
            error_code::STORAGE_CREDENTIAL_INVALID,
            error.to_string(),
        )
    })
}

/// 校验后端编码格式（与数据库 `ck_sys_storage_config_code` 同款规则）。
///
/// # 参数
///
/// - `code`：后端编码。
///
/// # 返回值
///
/// 合法返回 `Ok(())`。
///
/// # 异常情况
///
/// 不满足 `^[a-z][a-z0-9_-]{1,63}$` 时返回 400。
fn validate_config_code(code: &str) -> std::result::Result<(), ApiError> {
    let valid = {
        let bytes = code.as_bytes();
        (2..=64).contains(&bytes.len())
            && bytes[0].is_ascii_lowercase()
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
    };
    if valid {
        Ok(())
    } else {
        Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "后端编码只能是 2-64 位小写字母开头的 [a-z0-9_-]",
        ))
    }
}

/// 由请求体构造模型（新建 / 修改共用）。
///
/// # 参数
///
/// - `req`：请求体。
/// - `existing`：修改时的既有记录；新建传 `None`。
/// - `operator`：当前用户 ID（审计字段）。
/// - `key`：主密钥（加密 AK/SK）。
///
/// # 返回值
///
/// 待落库的模型。
///
/// # 异常情况
///
/// - 400：后端类型非法、本地后端带凭据、S3 缺 endpoint/bucket、只有半个凭据；
/// - 500：加密失败（主密钥不可用）。
///
/// # 密钥语义
///
/// 修改时未提供新凭据 → **沿用原密文**（不解密再加密，避免无谓的主密钥依赖）。
fn build_config(
    req: &StorageConfigSaveReq,
    existing: Option<&SysStorageConfigModel>,
    operator: Option<uuid::Uuid>,
    key: &MasterKey,
) -> std::result::Result<SysStorageConfigModel, ApiError> {
    let backend_type = req.backend_type.trim().to_ascii_uppercase();
    if backend_type != "LOCAL" && backend_type != "S3" {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "后端类型只能是 LOCAL 或 S3",
        ));
    }

    let endpoint = utils::trimmed(req.endpoint.as_deref()).map(str::to_string);
    let bucket = utils::trimmed(req.bucket.as_deref()).map(str::to_string);
    let credentials = req.credentials()?;

    if backend_type == "LOCAL" {
        // 本地后端不允许带凭据 / 端点（DB 有 ck_sys_storage_config_remote 兜底）
        if credentials.is_some() {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "本地后端不需要 AccessKey / SecretKey",
            ));
        }
        if endpoint.is_some() || bucket.is_some() {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "本地后端不需要 endpoint / bucket",
            ));
        }
    } else {
        // S3：endpoint / bucket 必填，凭据要么新给、要么沿用原密文
        let Some(endpoint) = endpoint.as_deref() else {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "S3 后端必须填写 endpoint",
            ));
        };
        if bucket.is_none() {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "S3 后端必须填写 bucket",
            ));
        }
        // SSRF 防护：拒绝回环 / 私网 / 链路本地字面量地址
        store::ensure_endpoint_allowed(endpoint, req.use_tls.unwrap_or(true)).map_err(|error| {
            ApiError::new(400, error_code::STORAGE_ENDPOINT_FORBIDDEN, error.message())
        })?;
        if credentials.is_none() && existing.is_none() {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "S3 后端必须填写 AccessKey / SecretKey",
            ));
        }
        if credentials.is_none()
            && existing
                .is_some_and(|config| config.ak_cipher.is_none() || config.sk_cipher.is_none())
        {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "S3 后端必须填写 AccessKey / SecretKey",
            ));
        }
    }

    let now = chrono::Utc::now();
    let mut model = SysStorageConfigModel {
        id: existing
            .map(|config| config.id)
            .unwrap_or_else(uuid::Uuid::now_v7),
        config_code: existing
            .map(|config| config.config_code.clone())
            .unwrap_or_else(|| req.config_code.trim().to_string()),
        config_name: req.config_name.trim().to_string(),
        backend_type,
        endpoint: if req.backend_type.trim().eq_ignore_ascii_case("LOCAL") {
            None
        } else {
            endpoint
        },
        bucket: if req.backend_type.trim().eq_ignore_ascii_case("LOCAL") {
            None
        } else {
            bucket
        },
        region: utils::trimmed(req.region.as_deref())
            .map(str::to_string)
            .or_else(|| existing.map(|config| config.region.clone()))
            .unwrap_or_else(|| "us-east-1".to_string()),
        base_path: utils::trimmed(req.base_path.as_deref())
            .map(|path| path.trim_matches('/').to_string())
            .unwrap_or_default(),
        use_tls: req
            .use_tls
            .or_else(|| existing.map(|config| config.use_tls))
            .unwrap_or(true),
        path_style: req
            .path_style
            .or_else(|| existing.map(|config| config.path_style))
            .unwrap_or(true),
        ak_cipher: existing.and_then(|config| config.ak_cipher.clone()),
        ak_nonce: existing.and_then(|config| config.ak_nonce.clone()),
        sk_cipher: existing.and_then(|config| config.sk_cipher.clone()),
        sk_nonce: existing.and_then(|config| config.sk_nonce.clone()),
        key_version: existing.map(|config| config.key_version).unwrap_or(1),
        is_default: req.is_default.unwrap_or(false),
        status: match req.status {
            Some(value) => EnableStatus::from_i16(value)?,
            None => existing
                .map(|config| config.status)
                .unwrap_or(EnableStatus::enabled),
        },
        health_status: existing
            .map(|config| config.health_status.clone())
            .unwrap_or_else(|| "UNKNOWN".to_string()),
        last_check_at: existing.and_then(|config| config.last_check_at),
        last_error: existing.and_then(|config| config.last_error.clone()),
        sort_order: req
            .sort_order
            .or_else(|| existing.map(|config| config.sort_order))
            .unwrap_or(0),
        remark: req
            .remark
            .clone()
            .or_else(|| existing.and_then(|config| config.remark.clone())),
        created_at: existing.map(|config| config.created_at).unwrap_or(now),
        updated_at: now,
        create_by: existing.and_then(|config| config.create_by).or(operator),
        update_by: operator,
        deleted_at: None,
    };

    // 新凭据：加密后覆盖四列（绝不明文落库）
    if let Some((ak, sk)) = credentials {
        let (ak_cipher, ak_nonce) = cipher::encrypt(key, &ak).ok_or_else(|| {
            ApiError::new(
                500,
                error_code::STORAGE_CREDENTIAL_INVALID,
                "AccessKey 加密失败（主密钥不可用）",
            )
        })?;
        let (sk_cipher, sk_nonce) = cipher::encrypt(key, &sk).ok_or_else(|| {
            ApiError::new(
                500,
                error_code::STORAGE_CREDENTIAL_INVALID,
                "SecretKey 加密失败（主密钥不可用）",
            )
        })?;
        model.ak_cipher = Some(ak_cipher);
        model.ak_nonce = Some(ak_nonce);
        model.sk_cipher = Some(sk_cipher);
        model.sk_nonce = Some(sk_nonce);
    }

    // 切换成 LOCAL 时清空凭据（DB 约束要求）
    if model.backend_type == "LOCAL" {
        model.ak_cipher = None;
        model.ak_nonce = None;
        model.sk_cipher = None;
        model.sk_nonce = None;
    }

    Ok(model)
}

/// 审计快照：只含非敏感元数据（**绝不**包含 AK/SK 与其密文）。
///
/// # 参数
///
/// - `config`：配置。
///
/// # 返回值
///
/// 可直接写入 `audit_logs` 的 JSON。
fn audit_snapshot(config: &SysStorageConfigModel) -> serde_json::Value {
    serde_json::json!({
        "config_code": config.config_code,
        "config_name": config.config_name,
        "backend_type": config.backend_type,
        "endpoint": config.endpoint,
        "bucket": config.bucket,
        "region": config.region,
        "is_default": config.is_default,
        "status": config.status.as_i16(),
        "health_status": config.health_status,
    })
}

/// 把文本截断到指定字符数（数据库列为 `VARCHAR(500)`）。
///
/// # 参数
///
/// - `text`：原文。
/// - `limit`：最大字符数。
///
/// # 返回值
///
/// 截断后的文本。
fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正常/错误分支：后端编码格式校验（与 DB CHECK 同款）。
    #[test]
    fn handler_validates_config_code() {
        assert!(validate_config_code("local-default").is_ok());
        assert!(validate_config_code("minio_a1").is_ok());
        assert!(validate_config_code("a1").is_ok());
        // 太短
        assert!(validate_config_code("a").is_err());
        // 大写 / 数字开头 / 非法字符 / 中文
        assert!(validate_config_code("Minio").is_err());
        assert!(validate_config_code("1minio").is_err());
        assert!(validate_config_code("minio.a").is_err());
        assert!(validate_config_code("minio a").is_err());
        assert!(validate_config_code("存储").is_err());
        // 超长
        assert!(validate_config_code(&"a".repeat(65)).is_err());
        assert!(validate_config_code(&"a".repeat(64)).is_ok());
    }

    /// 边界：审计快照不含凭据字段（防止有人后续把密文塞进审计）。
    #[test]
    fn handler_audit_snapshot_has_no_credentials() {
        let now = chrono::Utc::now();
        let config = SysStorageConfigModel {
            id: uuid::Uuid::now_v7(),
            config_code: "minio-a".to_string(),
            config_name: "MinIO".to_string(),
            backend_type: "S3".to_string(),
            endpoint: Some("http://minio.local:9000".to_string()),
            bucket: Some("mybucket".to_string()),
            region: "us-east-1".to_string(),
            base_path: String::new(),
            use_tls: false,
            path_style: true,
            ak_cipher: Some("AK-CIPHER-SENTINEL".to_string()),
            ak_nonce: Some("AK-NONCE-SENTINEL".to_string()),
            sk_cipher: Some("SK-CIPHER-SENTINEL".to_string()),
            sk_nonce: Some("SK-NONCE-SENTINEL".to_string()),
            key_version: 1,
            is_default: true,
            status: EnableStatus::enabled,
            health_status: "UNKNOWN".to_string(),
            last_check_at: None,
            last_error: None,
            sort_order: 0,
            remark: None,
            created_at: now,
            updated_at: now,
            create_by: None,
            update_by: None,
            deleted_at: None,
        };
        let snapshot = audit_snapshot(&config).to_string();
        assert!(!snapshot.contains("SENTINEL"), "{snapshot}");
        assert!(snapshot.contains("minio-a"));
    }

    /// 边界：错误消息截断按字符（不是字节），中文不会被截半个。
    #[test]
    fn handler_truncate_is_char_based() {
        assert_eq!(truncate("存储连接失败", 3), "存储连");
        assert_eq!(truncate("abcdef", 10), "abcdef");
    }
}

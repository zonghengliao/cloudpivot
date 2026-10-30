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

use axum::body::Body;
use axum::extract::{ConnectInfo, Multipart};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nik::prelude::*;
use tokio_util::io::ReaderStream;

use std::net::SocketAddr;

use crate::business::shared::principal;
use crate::business::storage::cipher::{self, MasterKey};
use crate::business::storage::dto::{
    StorageCompleteReq, StorageConfigPageQuery, StorageConfigRes, StorageConfigSaveReq,
    StorageFilePageQuery, StorageFileRes, StorageHeader, StoragePolicyPageQuery, StoragePolicyRes,
    StoragePolicySaveReq, StoragePresignReq, StoragePresignRes, StorageRebindReq, StorageTestRes,
    StorageUploadQuery,
};
use crate::business::storage::model::{
    self, PolicyViolation, StorageConfigFilter, StorageFileFilter, StorageFileModel,
    StoragePolicyFilter, SysStorageConfigModel, SysStoragePolicyModel,
};
use crate::business::storage::store::{ObjectStore, StoreError};
use crate::business::storage::{
    ensure_router, global_router, reload_router_best_effort, router as storage_router, sigv4, store,
};
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
#[nik::permission(
    path = "/business/storage/config/page",
    method = "GET",
    permission = "business:storage:config:list"
)]
#[nik::get("/business/storage/config/page")]
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
#[nik::permission(
    path = "/business/storage/config/{id}",
    method = "GET",
    permission = "business:storage:config:list"
)]
#[nik::get("/business/storage/config/{id}")]
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
#[nik::permission(
    path = "/business/storage/config",
    method = "POST",
    permission = "business:storage:config:create"
)]
#[nik::post("/business/storage/config")]
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
#[nik::permission(
    path = "/business/storage/config/{id}",
    method = "PUT",
    permission = "business:storage:config:update"
)]
#[nik::put("/business/storage/config/{id}")]
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
#[nik::permission(
    path = "/business/storage/config/{id}",
    method = "DELETE",
    permission = "business:storage:config:delete"
)]
#[nik::delete("/business/storage/config/{id}")]
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
#[nik::permission(
    path = "/business/storage/config/{id}/default",
    method = "POST",
    permission = "business:storage:config:update"
)]
#[nik::post("/business/storage/config/{id}/default")]
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
#[nik::permission(
    path = "/business/storage/config/{id}/test",
    method = "POST",
    permission = "business:storage:config:test"
)]
#[nik::post("/business/storage/config/{id}/test")]
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
#[nik::permission(
    path = "/business/storage/policy/page",
    method = "GET",
    permission = "business:storage:policy:list"
)]
#[nik::get("/business/storage/policy/page")]
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
#[nik::permission(
    path = "/business/storage/policy/{id}",
    method = "PUT",
    permission = "business:storage:policy:update"
)]
#[nik::put("/business/storage/policy/{id}")]
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

// ==================== 文件接口（A2：上传 / 直传 / 下载 / 软删 / 重新登记） ====================

/// 缺省业务分类。
pub const DEFAULT_CATEGORY: &str = "attachment";

/// 下载链接默认有效期（秒）：S3 后端 302 到预签名 URL 时使用。
pub const DOWNLOAD_TTL_SECS: u64 = 900;

/// 直传完成时做内容哈希校验的大小上限（字节）：超过则不整读回校验
/// （大文件读回会同时吃掉带宽与内存，架构文档 §2.6 已把它列为可配置取舍）。
pub const HASH_VERIFY_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 服务端上传（`multipart`，字段名 `file`，`?category=` 可选）。
///
/// # 语义
///
/// ① 按 `category` 取策略校验大小 / 扩展名 / MIME；② 内容 SHA-256 命中已有记录
/// 直接秒传（**不写对象、不插行**）；③ 否则按策略选后端写入对象，再落 `sys_file`
/// （带 `storage_code` 与 `upload_mode='SERVER'`）。
///
/// # 返回值
///
/// 文件记录（字段是旧 `FileRes` 的超集，多 `storage_code` / `upload_mode`）。
///
/// # 异常情况
///
/// - 400：表单非法 / 扩展名或 MIME 不符；
/// - 422：超过策略大小上限；
/// - 409：策略指定的后端不可用；
/// - 500 / 502：落对象或入库失败（对象失败时回滚：不插行）。
#[nik::permission(
    path = "/business/storage/upload",
    method = "POST",
    permission = "business:storage:upload"
)]
#[nik::post("/business/storage/upload")]
pub async fn upload(
    Query(query): Query<StorageUploadQuery>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    multipart: Multipart,
) -> ResResult<StorageFileRes> {
    // 顺序很重要（修复「110 MB 上传报 10006 上传文件内容读取失败」）：
    // ① 先取分类策略（只查库，不碰 body）；
    // ② 再用 `Content-Length` 做**读流之前**的大小校验；
    // ③ 最后才读 multipart。
    //
    // 框架的 `DefaultBodyLimit`（由 `[upload].max_size_mb` 推导，见 nik-web
    // `app.rs`「请求体上限就绪」）会在读取 body 的过程中掐断超限请求，此时
    // `field.bytes()` 只会给出一个笼统的读取错误 —— 若把策略校验放在读完之后，
    // 用户看到的永远是「上传文件内容读取失败 / 10006」而不是「超过分类上限」。
    // 因此大小判定必须发生在读取之前（Content-Length 可用时），并且读取失败时
    // 再按「是否命中 body 上限」映射成 `60003`（见 [`upload_read_error`]）。
    let category = category_of(query.category.as_deref());
    let policy = resolve_policy(&category).await?;
    let server_limit_bytes = server_upload_limit_bytes();
    if let Some(declared) = declared_content_length(&headers) {
        ensure_size_allowed(&policy, declared, server_limit_bytes)?;
    }

    let (original_name, mime_type, bytes) =
        read_single_file(multipart, &policy, server_limit_bytes).await?;
    let operator = utils::current_user_uuid(&claims);
    // 策略校验 → 秒传 → 选后端 → 落 `sys_file`：与任务附件 / AI 产出回传**共用同一套**
    // （抽成 store_server_upload，避免「AI 传的文件绕过人类上传那套白名单」）。
    let record =
        store_server_upload(&category, &original_name, &mime_type, &bytes, operator).await?;

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_FILE_UPLOAD",
            "StorageFile",
            record.id.to_string(),
            None,
            Some(serde_json::json!({
                "original_name": record.original_name,
                "file_size": record.file_size,
                "category": record.category,
                "storage_code": record.storage_code,
                "upload_mode": record.upload_mode,
            })),
        ),
    );

    Ok(Res::ok(StorageFileRes::from_model(&record)))
}

/// 预签名直传初始化（`POST /business/storage/presign`）。
///
/// # 语义
///
/// 策略校验（含「该分类是否允许直传」）→ 命中 `file_hash` 秒传短路 →
/// 否则签一个 `PUT` URL 交给浏览器直传，直传完成后调
/// [`complete`] 登记记录。
///
/// # 异常情况
///
/// - 400：策略不允许该扩展名 / MIME；
/// - 409：目标后端不可用；
/// - 422：超大小上限、该分类未开直传、或后端是本地磁盘（无法直传）。
#[nik::permission(
    path = "/business/storage/presign",
    method = "POST",
    permission = "business:storage:upload"
)]
#[nik::post("/business/storage/presign")]
pub async fn presign(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<StoragePresignReq>,
) -> ResResult<StoragePresignRes> {
    let category = category_of(Some(&req.category));
    let policy = resolve_policy(&category).await?;
    model::check_upload(
        &policy,
        &req.file_name,
        req.file_size.unwrap_or_default(),
        req.mime_type.as_deref(),
    )
    .map_err(violation_error)?;
    model::check_direct_upload(&policy).map_err(violation_error)?;

    // 给了哈希就先查秒传：命中时不必签 URL，也就不会产生一个空对象
    let file_hash = utils::trimmed(req.file_hash.as_deref()).map(str::to_lowercase);
    if let Some(hash) = file_hash.as_deref()
        && let Some(existing) = model::find_reusable_file(hash)
            .await
            .map_err(|error| utils::db_error(error, "查询可复用文件"))?
    {
        return Ok(Res::ok(StoragePresignRes {
            file_key: existing.file_key.clone(),
            upload_url: String::new(),
            method: "PUT".to_string(),
            headers: Vec::new(),
            expire_at: chrono::Utc::now(),
            storage_code: existing.storage_code.clone().unwrap_or_default(),
            category,
            duplicated: true,
            file: Some(StorageFileRes::from_model(&existing)),
        }));
    }

    ensure_router().await?;
    let backend = global_router().pick(Some(&policy))?;
    if backend.backend_type() != "S3" {
        return Err(ApiError::new(
            422,
            error_code::STORAGE_DIRECT_UPLOAD_DISABLED,
            "本地后端不支持浏览器直传，请改用服务端上传接口",
        ));
    }

    let file_key = build_object_key(&req.file_name);
    let ttl = u64::try_from(policy.presign_ttl_secs).unwrap_or(DOWNLOAD_TTL_SECS);
    let presigned = backend
        .presign_put(&file_key, ttl, req.mime_type.as_deref())
        .await
        .map_err(store_error)?;

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_FILE_PRESIGN",
            "StorageFile",
            file_key.clone(),
            None,
            Some(serde_json::json!({
                "category": category,
                "file_key": file_key,
                "storage_code": backend.config_code(),
                "expire_at": presigned.expire_at,
            })),
        ),
    );

    Ok(Res::ok(StoragePresignRes {
        file_key,
        upload_url: presigned.url,
        method: presigned.method.to_string(),
        headers: presigned
            .headers
            .iter()
            .map(|(name, value)| StorageHeader {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        expire_at: presigned.expire_at,
        storage_code: backend.config_code().to_string(),
        category,
        duplicated: false,
        file: None,
    }))
}

/// 直传完成登记（`POST /business/storage/complete`）。
///
/// # 语义
///
/// 后端 `stat()` 复核「对象真的存在 + 大小一致」，必要时读回算 SHA-256；
/// 校验通过才写 `sys_file`（`upload_mode='DIRECT'`）。**不信任前端声明**。
///
/// # 异常情况
///
/// - 404：对象不存在（直传没真正完成）；
/// - 422：大小 / 哈希与声明不一致；
/// - 409：后端不可用。
#[nik::permission(
    path = "/business/storage/complete",
    method = "POST",
    permission = "business:storage:upload"
)]
#[nik::post("/business/storage/complete")]
pub async fn complete(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<StorageCompleteReq>,
) -> ResResult<StorageFileRes> {
    let category = category_of(Some(&req.category));
    let policy = resolve_policy(&category).await?;
    model::check_upload(
        &policy,
        &req.file_name,
        req.file_size,
        req.mime_type.as_deref(),
    )
    .map_err(violation_error)?;

    ensure_router().await?;
    let backend = global_router().pick(Some(&policy))?;
    let stat = backend.stat(&req.file_key).await.map_err(store_error)?;
    let Some(stat) = stat else {
        return Err(ApiError::new(
            404,
            error_code::STORAGE_OBJECT_MISSING,
            "对象不存在，直传可能未完成或已被清理",
        ));
    };
    if stat.size != req.file_size {
        return Err(ApiError::new(
            422,
            error_code::STORAGE_OBJECT_MISSING,
            format!(
                "对象大小 {} 与声明 {} 不一致，请重新直传",
                stat.size, req.file_size
            ),
        ));
    }

    // 完整性：小文件读回算哈希（大文件跳过，靠 S3 的 x-amz-content-sha256 保证）
    let declared_hash = utils::trimmed(req.file_hash.as_deref()).map(str::to_lowercase);
    if let Some(hash) = declared_hash.as_deref()
        && stat.size <= HASH_VERIFY_MAX_BYTES
    {
        let bytes = backend.get(&req.file_key).await.map_err(store_error)?;
        let actual = sigv4::hex_sha256(&bytes);
        if actual != hash {
            return Err(ApiError::new(
                422,
                error_code::STORAGE_OBJECT_MISSING,
                "对象内容哈希与声明不一致，请重新直传",
            ));
        }
    }

    if let Some(hash) = declared_hash.as_deref()
        && let Some(existing) = model::find_reusable_file(hash)
            .await
            .map_err(|error| utils::db_error(error, "查询可复用文件"))?
    {
        return Ok(Res::ok(StorageFileRes::from_model(&existing)));
    }

    let operator = utils::current_user_uuid(&claims);
    let mime_type = utils::trimmed(req.mime_type.as_deref()).unwrap_or("application/octet-stream");
    let mut record = build_file_record(FileRecordInput {
        original_name: &req.file_name,
        file_key: &req.file_key,
        file_size: i64::try_from(stat.size).unwrap_or(i64::MAX),
        file_hash: declared_hash.as_deref().unwrap_or_default(),
        mime_type,
        category: &category,
        storage_code: backend.config_code(),
        storage_path: storage_path_for(backend.as_ref(), &req.file_key),
        upload_mode: "DIRECT",
        operator,
    });
    if let Err(error) = record.insert(db()).await {
        if let Some(hash) = declared_hash.as_deref()
            && let Some(existing) = model::find_reusable_file(hash).await.unwrap_or(None)
        {
            return Ok(Res::ok(StorageFileRes::from_model(&existing)));
        }
        return Err(utils::db_error(error, "写入文件元数据"));
    }

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_FILE_COMPLETE",
            "StorageFile",
            record.id.to_string(),
            None,
            Some(serde_json::json!({
                "file_key": record.file_key,
                "file_size": record.file_size,
                "storage_code": record.storage_code,
                "upload_mode": record.upload_mode,
            })),
        ),
    );

    Ok(Res::ok(StorageFileRes::from_model(&record)))
}

/// 文件分页（`GET /business/storage/file/page`）。
///
/// # 过滤
///
/// `original_name` 模糊、`category` / `storage_code` / `file_hash` 精确。
///
/// # 异常情况
///
/// 500：数据库异常。
#[nik::permission(
    path = "/business/storage/file/page",
    method = "GET",
    permission = "business:storage:list"
)]
#[nik::get("/business/storage/file/page")]
pub async fn file_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<StorageFilePageQuery>,
) -> ResResult<PageData<Vec<StorageFileRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    // 批次 B · B5（附录 C #11）：`/business/storage/file/page` 是 sys_file 的第二个
    // 列表入口，与 `/support/file/page` 同口径收口（同一 ScopeTarget::SYS_FILE）。
    let principal = principal::resolve_for_claims(&claims).await?;
    let filter = StorageFileFilter {
        original_name: utils::trimmed(query.original_name.as_deref()).map(str::to_string),
        category: utils::trimmed(query.category.as_deref()).map(str::to_string),
        storage_code: utils::trimmed(query.storage_code.as_deref()).map(str::to_string),
        file_hash: utils::trimmed(query.file_hash.as_deref()).map(str::to_string),
    };
    let result = model::page_storage_files(&filter, page, page_size, &principal)
        .await
        .map_err(|error| utils::db_error(error, "分页查询文件"))?;
    let list = result
        .list
        .iter()
        .map(StorageFileRes::from_model)
        .collect::<Vec<_>>();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 下载文件（流式；S3 后端 302 到预签名 URL）。
///
/// # 行为
///
/// | 后端 | 行为 |
/// |------|------|
/// | `LOCAL` | `tokio::fs::File` + `ReaderStream` **流式**下发（不整文件进内存） |
/// | `S3` | 302 到现签的预签名 GET URL（`ttl = 900 s`），后端零流量 |
///
/// # 返回值
///
/// 二进制响应（非统一响应体）：`Content-Type` 取记录 MIME，
/// `Content-Disposition` 恒为 `attachment`（防 inline 执行），
/// `X-Content-Type-Options: nosniff`。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：记录不存在 / 已软删 / 物理对象丢失（**不泄露对象是否存在**）；
/// - 409：记录所在后端已停用；
/// - 502：远端后端访问失败。
#[nik::permission(
    path = "/business/storage/download/{file_id}",
    method = "GET",
    permission = "business:storage:download"
)]
#[nik::get("/business/storage/download/{file_id}")]
pub async fn download(Path(file_id): Path<String>) -> Response {
    match download_response(&file_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 软删文件（物理对象延迟回收）。
///
/// # 语义
///
/// 只写 `deleted_at`：秒传复用者手里的 `file_id` 不会因别人删除而立刻失效，
/// 物理对象由 [`crate::business::storage::cleanup`] 在软删 7 天后回收。
///
/// # 异常情况
///
/// - 404：记录不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/storage/{file_id}",
    method = "DELETE",
    permission = "business:storage:delete"
)]
#[nik::delete("/business/storage/{file_id}")]
pub async fn remove(
    Path(file_id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let id = utils::require_uuid(&file_id, "file_id")?;
    let record = model::find_any_file(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询文件"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "文件不存在"))?;

    // 幂等：已删除直接返回成功（重试 / 并发删除不报错）
    if record.deleted_at.is_none() {
        let operator = utils::current_user_uuid(&claims);
        let affected = model::soft_delete_file(&id, operator)
            .await
            .map_err(|error| utils::db_error(error, "软删文件"))?;
        if affected == 0 {
            return Err(ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                "文件不存在或已删除",
            ));
        }
        log::record_audit(
            &headers,
            &connect_info,
            log::audit_input(
                &claims,
                "STORAGE_FILE_DELETE",
                "StorageFile",
                id.to_string(),
                Some(serde_json::json!({
                    "file_key": record.file_key,
                    "storage_code": record.storage_code,
                })),
                Some(serde_json::json!({ "deleted": true })),
            ),
        );
    }
    Ok(Res::ok_empty())
}

/// 重新登记（`POST /business/storage/{file_id}/rebind`）。
///
/// # 语义
///
/// 用**同一个对象键**生成一条新记录（新 ID），供「共享记录被别人删了」的业务自救；
/// 不复制物理对象，也不改变原记录。若同内容已有有效记录，直接返回那一条（幂等）。
///
/// # 异常情况
///
/// - 404：原记录不存在；
/// - 409：原记录所在后端已停用；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/storage/{file_id}/rebind",
    method = "POST",
    permission = "business:storage:upload"
)]
#[nik::post("/business/storage/{file_id}/rebind")]
pub async fn rebind(
    Path(file_id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    body: Option<axum::Json<StorageRebindReq>>,
) -> ResResult<StorageFileRes> {
    let id = utils::require_uuid(&file_id, "file_id")?;
    let request = body.map(|axum::Json(value)| value).unwrap_or_default();
    let origin = model::find_any_file(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询文件"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "文件不存在"))?;

    // 同内容已有有效记录 → 直接复用（避免 uk_sys_file_hash 撞车）
    if let Some(hash) = origin.file_hash.as_deref()
        && let Some(existing) = model::find_reusable_file(hash)
            .await
            .map_err(|error| utils::db_error(error, "查询可复用文件"))?
    {
        return Ok(Res::ok(StorageFileRes::from_model(&existing)));
    }

    let operator = utils::current_user_uuid(&claims);
    let original_name = utils::trimmed(request.original_name.as_deref())
        .map(str::to_string)
        .or_else(|| origin.original_name.clone())
        .unwrap_or_else(|| origin.file_name.clone());
    let category = utils::trimmed(request.category.as_deref())
        .map(str::to_string)
        .or_else(|| origin.category.clone())
        .unwrap_or_else(|| DEFAULT_CATEGORY.to_string());

    // 后端必须仍在路由表里：不在则 409，不做自动跨后端复制
    ensure_router().await?;
    if let Some(code) = origin.storage_code.as_deref()
        && !code.is_empty()
        && global_router().by_code(code).is_none()
    {
        return Err(ApiError::new(
            409,
            error_code::STORAGE_BACKEND_UNAVAILABLE,
            format!("文件所在存储后端 {code} 已停用或删除，无法重新登记"),
        ));
    }

    let mut record = build_file_record(FileRecordInput {
        original_name: &original_name,
        file_key: &origin.file_key,
        file_size: origin.file_size,
        file_hash: origin.file_hash.as_deref().unwrap_or_default(),
        mime_type: origin
            .mime_type
            .as_deref()
            .unwrap_or("application/octet-stream"),
        category: &category,
        storage_code: origin.storage_code.as_deref().unwrap_or_default(),
        storage_path: origin.storage_path.clone(),
        upload_mode: &origin.upload_mode,
        operator,
    });
    record
        .insert(db())
        .await
        .map_err(|error| utils::db_error(error, "重新登记文件"))?;

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "STORAGE_FILE_REBIND",
            "StorageFile",
            record.id.to_string(),
            Some(serde_json::json!({ "source_id": id.to_string() })),
            Some(serde_json::json!({
                "file_key": record.file_key,
                "storage_code": record.storage_code,
            })),
        ),
    );

    Ok(Res::ok(StorageFileRes::from_model(&record)))
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

// ==================== 文件接口辅助（A2） ====================

/// 归一化业务分类（去空白，空值回落 [`DEFAULT_CATEGORY`]）。
///
/// # 参数
///
/// - `raw`：查询参数 / 请求体里的分类。
///
/// # 返回值
///
/// 可直接用于查策略的分类名。
fn category_of(raw: Option<&str>) -> String {
    utils::trimmed(raw).unwrap_or(DEFAULT_CATEGORY).to_string()
}

/// 取分类策略：有策略行用策略行，没有则用内置默认策略。
///
/// # 异常情况
///
/// - 400：策略行存在但已停用；
/// - 500：数据库异常。
async fn resolve_policy(category: &str) -> std::result::Result<SysStoragePolicyModel, ApiError> {
    let found = model::find_policy_by_category(category)
        .await
        .map_err(|error| utils::db_error(error, "查询上传策略"))?;
    match found {
        Some(policy) => {
            if policy.status.as_i16() == 0 {
                return Err(ApiError::new(
                    400,
                    error_code::PARAM_INVALID,
                    format!("分类 {category} 的上传策略已停用，请联系管理员"),
                ));
            }
            Ok(policy)
        }
        None => Ok(model::default_policy(category)),
    }
}

/// 策略违规 → 统一错误（HTTP 状态码与业务码都来自 [`PolicyViolation`]）。
///
/// # 参数
///
/// - `violation`：策略校验失败原因。
///
/// # 返回值
///
/// 带中文消息的 [`ApiError`]。
fn violation_error(violation: PolicyViolation) -> ApiError {
    ApiError::new(
        violation.http_status(),
        violation.code(),
        violation.message(),
    )
}

/// 存储层错误 → 统一错误（**只透出脱敏后的中文消息**）。
///
/// # 参数
///
/// - `error`：存储层错误。
///
/// # 返回值
///
/// | 错误 | HTTP | 业务码 |
/// |------|------|--------|
/// | `NotFound` | 404 | `STORAGE_OBJECT_MISSING` |
/// | `Invalid` | 400 | `PARAM_INVALID` |
/// | `Unsupported` | 422 | `STORAGE_DIRECT_UPLOAD_DISABLED` |
/// | `Io` | 500 | `INTERNAL_ERROR` |
/// | `Backend` | 502 | `INTERNAL_ERROR` |
fn store_error(error: StoreError) -> ApiError {
    let (status, code) = match &error {
        StoreError::NotFound(_) => (404, error_code::STORAGE_OBJECT_MISSING),
        StoreError::Invalid(_) => (400, error_code::PARAM_INVALID),
        StoreError::Unsupported(_) => (422, error_code::STORAGE_DIRECT_UPLOAD_DISABLED),
        StoreError::Io(_) => (500, error_code::INTERNAL_ERROR),
        StoreError::Backend(_) => (502, error_code::INTERNAL_ERROR),
    };
    tracing::warn!(status, error = %error, "存储后端操作失败");
    ApiError::new(status, code, error.message())
}

/// 解析文件所在后端（读路径）。
///
/// # 参数
///
/// - `storage_code`：`sys_file.storage_code`；`None` / 空串表示历史文件。
///
/// # 返回值
///
/// 后端实例。
///
/// # 异常情况
///
/// - 409：记录指向的后端已停用 / 删除（**不做自动跨后端复制**）。
///
/// # 兼容语义
///
/// `storage_code IS NULL` ⇒ **恒按本地默认目录**（`upload_path()`）读取，
/// 不看当前默认后端是谁：否则把默认后端切成 MinIO 后，老文件会全部 404
/// （架构文档 §2.7 兼容矩阵第 3 行）。
async fn resolve_file_store(
    storage_code: Option<&str>,
) -> std::result::Result<std::sync::Arc<dyn ObjectStore>, ApiError> {
    if let Some(code) = storage_code.map(str::trim).filter(|code| !code.is_empty()) {
        ensure_router().await?;
        return global_router().by_code(code).ok_or_else(|| {
            ApiError::new(
                409,
                error_code::STORAGE_BACKEND_UNAVAILABLE,
                format!("文件所在存储后端 {code} 已停用或删除，请重新上传"),
            )
        });
    }
    Ok(std::sync::Arc::new(store::LocalStore::new(
        "local-default",
        "",
    )))
}

/// 生成对象键：`{yyyy}/{MM}/{dd}/{uuidv7}.{ext}`。
///
/// # 参数
///
/// - `original_name`：原始文件名（只取扩展名）。
///
/// # 返回值
///
/// 相对后端根的对象键（无扩展名时没有点号后缀）。
fn build_object_key(original_name: &str) -> String {
    let now = chrono::Utc::now();
    let extension = model::extension_of(original_name).unwrap_or_default();
    let suffix = if extension.is_empty() {
        String::new()
    } else {
        format!(".{extension}")
    };
    format!(
        "{}/{}/{}/{}{}",
        now.format("%Y"),
        now.format("%m"),
        now.format("%d"),
        uuid::Uuid::now_v7(),
        suffix
    )
}

/// `storage_path` 列取值（本地存绝对路径，S3 存后端内相对键）。
///
/// # 说明
///
/// 历史行的语义是「磁盘绝对路径」；新行沿用同一列但语义按后端区分。
/// 读取一律以 `file_key` + 后端根为准，`storage_path` 仅供排查。
fn storage_path_for(backend: &dyn ObjectStore, file_key: &str) -> Option<String> {
    if backend.backend_type() == "LOCAL" {
        Some(format!("{}/{}", nik::upload_path(), file_key))
    } else {
        Some(file_key.to_string())
    }
}

/// 构造 `sys_file` 记录用的入参（字段多，避免 `too_many_arguments`）。
struct FileRecordInput<'a> {
    /// 原始文件名。
    original_name: &'a str,
    /// 对象键。
    file_key: &'a str,
    /// 字节数。
    file_size: i64,
    /// 内容哈希（空串 = 未知，落库为 NULL）。
    file_hash: &'a str,
    /// MIME。
    mime_type: &'a str,
    /// 分类。
    category: &'a str,
    /// 后端编码（空串 = 历史本地语义，落库为 NULL）。
    storage_code: &'a str,
    /// 物理路径 / 后端内键。
    storage_path: Option<String>,
    /// `SERVER` / `DIRECT`。
    upload_mode: &'a str,
    /// 操作人。
    operator: Option<uuid::Uuid>,
}

/// 由入参构造待落库的文件记录。
///
/// # 参数
///
/// - `input`：见 [`FileRecordInput`]。
///
/// # 返回值
///
/// 主键已生成（UUID v7）、时间戳为当前时间的记录。
fn build_file_record(input: FileRecordInput<'_>) -> StorageFileModel {
    let now = chrono::Utc::now();
    StorageFileModel {
        id: uuid::Uuid::now_v7(),
        file_name: input.file_key.to_string(),
        original_name: Some(input.original_name.to_string()),
        file_key: input.file_key.to_string(),
        file_size: input.file_size,
        file_hash: if input.file_hash.is_empty() {
            None
        } else {
            Some(input.file_hash.to_string())
        },
        mime_type: Some(input.mime_type.to_string()),
        extension: model::extension_of(input.original_name),
        storage_path: input.storage_path,
        category: Some(input.category.to_string()),
        status: EnableStatus::enabled,
        uploaded_by: input.operator,
        created_at: now,
        updated_at: now,
        create_by: input.operator,
        update_by: input.operator,
        deleted_at: None,
        storage_code: if input.storage_code.is_empty() {
            None
        } else {
            Some(input.storage_code.to_string())
        },
        upload_mode: input.upload_mode.to_string(),
    }
}

/// multipart 表单本身的开销余量（字节）。
///
/// `Content-Length` 是**整个 body** 的长度（边界串 + 各段头 + 文件名），比文件
/// 本身多出几百字节。前置校验若不留余量，恰好等于上限的文件会被误判超限。
/// 这里给 1 MiB（远大于任何真实 multipart 头，且不会放过任何真实超限）。
pub const MULTIPART_OVERHEAD_ALLOWANCE: u64 = 1024 * 1024;

/// 服务端「已读到内存」的上传核心（**任务附件 / AI 产出回传复用**）。
///
/// # 为什么从 [`upload`] 里抽出来
///
/// T5b 有三处需要「把一段字节写进云枢存储并落 `sys_file`」：
///
/// 1. `POST /business/storage/upload`（人类上传，走 multipart）；
/// 2. `POST /business/task/{id}/attachment`（任务附件，人类与 **AI 令牌**两条路径）；
/// 3. 轮询回传的**路径 B**：AI 在 `metadata.new_files` 里声明了本机路径，
///    云枢自己读盘再上传。
///
/// 三处的策略校验 / 秒传 / 选后端 / 落库必须**逐字节同一套**——否则「AI 传的文件
/// 绕过了人类上传那套白名单」就是一个静默的安全缺口。抽成一个函数，调用方只管
/// 「字节从哪来」。
///
/// # 参数
///
/// - `category`：上传分类（决定策略：`task` / `task_ai` / …）；
/// - `original_name`：原始文件名（决定扩展名白名单与对象键后缀）；
/// - `mime_type`：MIME（策略 `allowed_mimes` 为空时不校验）；
/// - `bytes`：文件内容；
/// - `operator`：操作人（`sys_file.uploaded_by` / `create_by`；AI 产出传 AI 用户的 id）。
///
/// # 返回值
///
/// 落库后的文件记录（命中秒传时是**已有**的那条，id 可能属于别的任务 ——
/// 调用方按 `(task_id, file_id)` 唯一约束绑定附件即可）。
///
/// # 异常情况
///
/// 与 [`upload`] 一致：400 策略/表单非法、422 超限、409 后端不可用、500/502 落库或写对象失败。
pub(crate) async fn store_server_upload(
    category: &str,
    original_name: &str,
    mime_type: &str,
    bytes: &[u8],
    operator: Option<uuid::Uuid>,
) -> std::result::Result<StorageFileModel, ApiError> {
    let policy = resolve_policy(category).await?;
    model::check_upload(&policy, original_name, bytes.len() as u64, Some(mime_type))
        .map_err(violation_error)?;

    // 秒传：内容相同直接复用已有记录（跨后端天然成立，记录里带 storage_code）
    let file_hash = sigv4::hex_sha256(bytes);
    if let Some(existing) = model::find_reusable_file(&file_hash)
        .await
        .map_err(|error| utils::db_error(error, "查询可复用文件"))?
    {
        tracing::info!(file_hash = %file_hash, file_id = %existing.id, "命中秒传，复用已有文件记录");
        return Ok(existing);
    }

    ensure_router().await?;
    let backend = global_router().pick(Some(&policy))?;
    let file_key = build_object_key(original_name);
    backend
        .put(&file_key, bytes, mime_type)
        .await
        .map_err(store_error)?;

    let mut record = build_file_record(FileRecordInput {
        original_name,
        file_key: &file_key,
        file_size: bytes.len() as i64,
        file_hash: &file_hash,
        mime_type,
        category,
        storage_code: backend.config_code(),
        storage_path: storage_path_for(backend.as_ref(), &file_key),
        upload_mode: "SERVER",
        operator,
    });

    if let Err(error) = record.insert(db()).await {
        // 并发首传同内容：`uk_sys_file_hash` 命中后转查旧记录（不冒 500）
        if let Some(existing) = model::find_reusable_file(&file_hash).await.unwrap_or(None) {
            tracing::info!(file_hash = %file_hash, file_id = %existing.id, "并发秒传冲突，复用已有记录");
            return Ok(existing);
        }
        return Err(utils::db_error(error, "写入文件元数据"));
    }
    Ok(record)
}

/// 服务端上传的单文件上限（字节）：与框架 `DefaultBodyLimit` 同源
/// （nik-web `app.rs` 用 `[upload].max_size_mb` 推导请求体硬顶）。
///
/// # 返回值
///
/// `[upload].max_size_mb` × 1 MiB；配置为 0 时返回 0（表示不限）。
pub(crate) fn server_upload_limit_bytes() -> u64 {
    cfg().upload.max_size_mb as u64 * 1024 * 1024
}

/// 取请求声明的 `Content-Length`（缺失或非法 → `None`，如 chunked 上传）。
///
/// # 参数
///
/// - `headers`：请求头。
///
/// # 返回值
///
/// 声明的字节数。
pub(crate) fn declared_content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// 「超过该分类大小上限」错误（HTTP 422 + `60003`）。
///
/// # 参数
///
/// - `policy_max_mb`：该分类策略上限（MB）。
/// - `size_bytes`：已知的实际大小（`None` 表示只在读流失败时推断，大小未知）。
pub(crate) fn category_size_error(policy_max_mb: i32, size_bytes: Option<u64>) -> ApiError {
    let message = match size_bytes {
        Some(size) => format!(
            "文件大小 {:.2} MB 超过该分类大小上限 {policy_max_mb} MB",
            size as f64 / (1024.0 * 1024.0)
        ),
        None => format!("上传内容超过该分类大小上限 {policy_max_mb} MB"),
    };
    ApiError::new(422, error_code::STORAGE_SIZE_EXCEEDED, message)
}

/// 「超过服务端单文件上限」错误（HTTP 422 + `60003`）。
///
/// 语义：分类策略允许更大的文件，但**服务端中转上传**（整包读进内存）有硬顶，
/// 此时必须引导用户改用预签名直传，而不是让人以为策略配错了。
///
/// # 参数
///
/// - `server_limit_bytes`：服务端单文件上限（字节）。
pub(crate) fn server_size_error(server_limit_bytes: u64) -> ApiError {
    ApiError::new(
        422,
        error_code::STORAGE_SIZE_EXCEEDED,
        format!(
            "上传内容超过服务端单文件上限 {} MB，请改用预签名直传（POST /business/storage/presign）",
            server_limit_bytes / (1024 * 1024)
        ),
    )
}

/// 读流之前的整体大小校验（`Content-Length` 可用时调用）。
///
/// # 判定顺序（与语义一致性有关，不要调换）
///
/// 1. 分类策略上限（业务权威口径）→ `60003`「超过该分类大小上限 N MB」；
/// 2. 服务端单文件上限（框架 body 硬顶）→ `60003`「超过服务端单文件上限 N MB」。
///
/// 两者都留 [`MULTIPART_OVERHEAD_ALLOWANCE`] 余量；真实字节数还要在读完后再由
/// `model::check_upload` 用精确长度判定一次（本条只是把「超大请求」提前拒掉）。
///
/// # 异常情况
///
/// 超任一上限 → 422 + `60003`。
pub(crate) fn ensure_size_allowed(
    policy: &SysStoragePolicyModel,
    size_bytes: u64,
    server_limit_bytes: u64,
) -> std::result::Result<(), ApiError> {
    let policy_max_mb = policy.max_size_mb.max(0) as u64;
    if policy_max_mb > 0 && size_bytes > policy_max_mb * 1024 * 1024 + MULTIPART_OVERHEAD_ALLOWANCE
    {
        return Err(category_size_error(policy.max_size_mb, Some(size_bytes)));
    }
    if server_limit_bytes > 0 && size_bytes > server_limit_bytes + MULTIPART_OVERHEAD_ALLOWANCE {
        return Err(server_size_error(server_limit_bytes));
    }
    Ok(())
}

/// 读取 multipart 失败 → 对外错误。
///
/// axum 在 body 超过 `DefaultBodyLimit` 时给出 `413 PAYLOAD_TOO_LARGE`；如果不翻译，
/// 用户只会看到「上传文件内容读取失败 / 10006」，完全看不出是「文件太大」。
/// 这里把 413 翻译成 `60003`：
///
/// - 分类策略上限 **小于** 服务端上限 → 一定是超了分类上限 → 报分类口径；
/// - 否则（分类放宽但服务端扛不住）→ 报服务端上限 + 引导走预签名直传。
///
/// # 参数
///
/// - `status`：读取失败时 axum 给出的状态码。
/// - `policy`：该分类策略。
/// - `server_limit_bytes`：服务端单文件上限（字节）。
pub(crate) fn upload_read_error(
    status: StatusCode,
    policy: &SysStoragePolicyModel,
    server_limit_bytes: u64,
) -> ApiError {
    if status == StatusCode::PAYLOAD_TOO_LARGE {
        let policy_max_mb = policy.max_size_mb.max(0) as u64;
        let policy_max_bytes = policy_max_mb * 1024 * 1024;
        return if policy_max_bytes > 0
            && (server_limit_bytes == 0 || policy_max_bytes < server_limit_bytes)
        {
            category_size_error(policy.max_size_mb, None)
        } else {
            server_size_error(server_limit_bytes)
        };
    }
    ApiError::new(400, error_code::PAYLOAD_INVALID, "上传文件内容读取失败")
}

/// 读取 multipart 里的第一个 `file` 字段（服务端上传只要一个文件）。
///
/// # 参数
///
/// - `multipart`：axum 表单提取器（须为最后一个提取器参数）。
/// - `policy`：该分类策略（读取失败时用于翻译「太大」的文案）。
/// - `server_limit_bytes`：服务端单文件上限（字节）。
///
/// # 返回值
///
/// `(原始文件名, MIME, 字节)`。
///
/// # 异常情况
///
/// - 400：表单解析失败 / 缺少 `file` 字段 / 文件名为空；
/// - 422 + `60003`：body 超过上限（框架掐断，见 [`upload_read_error`]）。
async fn read_single_file(
    mut multipart: Multipart,
    policy: &SysStoragePolicyModel,
    server_limit_bytes: u64,
) -> std::result::Result<(String, String, Vec<u8>), ApiError> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| upload_read_error(error.status(), policy, server_limit_bytes))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let original_name = field.file_name().unwrap_or("").trim().to_string();
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = field
            .bytes()
            .await
            .map_err(|error| upload_read_error(error.status(), policy, server_limit_bytes))?;
        if original_name.is_empty() {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "缺少文件名（multipart 的 filename）",
            ));
        }
        return Ok((original_name, content_type, bytes.to_vec()));
    }
    Err(ApiError::new(
        400,
        error_code::PARAM_INVALID,
        "缺少文件字段 file",
    ))
}

/// 构造 `Content-Disposition`（ASCII 回退名 + RFC 5987 编码名，恒 `attachment`）。
///
/// # 参数
///
/// - `original`：原始文件名。
///
/// # 返回值
///
/// 合法的头值；构造失败时退化为 `attachment`。
fn content_disposition(original: &str) -> HeaderValue {
    let ascii: String = original
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' && !c.is_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in original.as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if keep {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// 组装下载响应（本地流式 / S3 302）。
///
/// # 为什么是 `pub(crate)`
///
/// 任务附件（C+ · T6b）的下载入口 `GET /business/task/{id}/attachments/{att_id}/download`
/// 必须先校验**任务可见性**，再复用本函数出流 —— 这样「出流」只有一份实现
/// （`LOCAL` 流式 / `S3` 302 预签名 / Range / nosniff 都在这里），
/// 任务侧只需要在前面加一道 `att_id ↔ task_id` 的归属校验。
/// 若任务侧自己写一遍 `stream()`，两处的响应头（`Content-Disposition` / `nosniff`）
/// 迟早会不一致 —— 而那两个头正是「浏览器不 inline 执行上传物」的防线。
///
/// # 参数
///
/// - `raw_id`：路径里的文件 ID。
///
/// # 返回值
///
/// 二进制响应。
///
/// # 异常情况
///
/// 见 [`download`] 的「异常情况」段。
pub(crate) async fn download_response(raw_id: &str) -> std::result::Result<Response, ApiError> {
    let id = utils::require_uuid(raw_id, "file_id")?;
    let record = model::find_active_file(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询下载文件"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "文件不存在或已删除"))?;

    let backend = resolve_file_store(record.storage_code.as_deref()).await?;
    let mime = record
        .mime_type
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let original = record
        .original_name
        .clone()
        .unwrap_or_else(|| record.file_name.clone());

    // S3 后端：302 到现签 URL（后端零流量，且天然支持 Range / 断点续传）
    if backend.backend_type() == "S3"
        && let Some(url) = backend
            .presign_get(&record.file_key, DOWNLOAD_TTL_SECS)
            .await
            .map_err(store_error)?
    {
        let location = HeaderValue::from_str(&url).map_err(|_| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "预签名下载链接生成失败（含非法字符）",
            )
        })?;
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::FOUND;
        let headers = response.headers_mut();
        headers.insert(header::LOCATION, location);
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        return Ok(response);
    }

    // 本地后端（以及 S3 无法预签名的兜底）：服务端流式下发
    let stat = backend.stat(&record.file_key).await.map_err(store_error)?;
    let Some(stat) = stat else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "文件已丢失（记录存在但对象被清理）",
        ));
    };
    let reader = backend
        .stream(&record.file_key, 0)
        .await
        .map_err(store_error)?;
    let mut response = Response::new(Body::from_stream(ReaderStream::new(reader)));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&stat.size.to_string())
            .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "响应头构造失败"))?,
    );
    headers.insert(header::CONTENT_DISPOSITION, content_disposition(&original));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A2 正常分支：对象键格式 `{yyyy}/{MM}/{dd}/{uuidv7}.{ext}`。
    #[test]
    fn handler_build_object_key_shape() {
        let key = build_object_key("合同扫描件.PDF");
        let parts: Vec<&str> = key.split('/').collect();
        assert_eq!(parts.len(), 4, "{key}");
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
        assert!(parts[3].ends_with(".pdf"), "{key}");
        assert_eq!(parts[3].trim_end_matches(".pdf").len(), 36, "{key}");
        // 两次生成不重复（UUID v7）
        assert_ne!(key, build_object_key("合同扫描件.PDF"));
    }

    /// A2 边界分支：无扩展名时对象键没有点号后缀。
    #[test]
    fn handler_build_object_key_without_extension() {
        let key = build_object_key("README");
        let leaf = key.rsplit('/').next().unwrap_or_default();
        assert!(!leaf.contains('.'), "{key}");
        assert_eq!(leaf.len(), 36, "{key}");
    }

    /// A2 正常/边界：分类归一化（空值 / 空白回落默认，其余去空白）。
    #[test]
    fn handler_category_of_normalizes() {
        assert_eq!(category_of(None), "attachment");
        assert_eq!(category_of(Some("   ")), "attachment");
        assert_eq!(category_of(Some(" avatar ")), "avatar");
    }

    /// A2 安全：中文文件名走 RFC 5987，ASCII 回退名不出现非 ASCII 字节。
    #[test]
    fn handler_content_disposition_encodes_chinese() {
        let value = content_disposition("报价单 2026.pdf");
        let text = value.to_str().expect("头值应为可见 ASCII");
        assert!(text.starts_with("attachment;"));
        assert!(text.contains("filename*=UTF-8''"), "{text}");
        assert!(text.contains("%20"), "{text}");
        assert!(
            text.split("filename*")
                .next()
                .unwrap_or_default()
                .is_ascii(),
            "{text}"
        );
        // 引号被替换，不会破坏头语法
        assert!(
            !content_disposition("a\"b.txt")
                .to_str()
                .unwrap_or_default()
                .contains("a\"b")
        );
    }

    /// A2 错误分支：存储错误 → HTTP / 业务码映射齐全（不漏任何一种）。
    #[test]
    fn handler_store_error_mapping() {
        let cases = [
            (
                StoreError::NotFound("k".into()),
                404u16,
                error_code::STORAGE_OBJECT_MISSING,
            ),
            (
                StoreError::Invalid("k".into()),
                400u16,
                error_code::PARAM_INVALID,
            ),
            (
                StoreError::Unsupported("k".into()),
                422u16,
                error_code::STORAGE_DIRECT_UPLOAD_DISABLED,
            ),
            (
                StoreError::Io("k".into()),
                500u16,
                error_code::INTERNAL_ERROR,
            ),
            (
                StoreError::Backend("k".into()),
                502u16,
                error_code::INTERNAL_ERROR,
            ),
        ];
        for (error, status, code) in cases {
            let api = store_error(error);
            assert_eq!(api.status, status);
            assert_eq!(api.code, code);
        }
    }

    /// A2 正常分支：记录字段映射（`upload_mode` / `storage_code` / 空哈希降级 NULL）。
    #[test]
    fn handler_build_file_record_fields() {
        let record = build_file_record(FileRecordInput {
            original_name: "报表.xlsx",
            file_key: "2026/10/28/abc.xlsx",
            file_size: 42,
            file_hash: "",
            mime_type: "application/vnd.ms-excel",
            category: "import",
            storage_code: "",
            storage_path: None,
            upload_mode: "DIRECT",
            operator: None,
        });
        assert_eq!(record.file_name, "2026/10/28/abc.xlsx");
        assert_eq!(record.extension.as_deref(), Some("xlsx"));
        assert_eq!(record.upload_mode, "DIRECT");
        // 空哈希 / 空后端 → NULL（历史本地语义）
        assert!(record.file_hash.is_none());
        assert!(record.storage_code.is_none());
        assert_eq!(record.status.as_i16(), 1);
        assert!(record.deleted_at.is_none());

        let res = StorageFileRes::from_model(&record);
        assert_eq!(
            res.url,
            format!("/api/business/storage/download/{}", record.id)
        );
        assert_eq!(res.storage_code, "");
        assert_eq!(res.upload_mode, "DIRECT");
    }
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

    /// 修复项 A-遗留①：110 MB 打 `attachment`（上限 100 MB）→ 422 + 60003，
    /// 文案必须点明「该分类大小上限 100 MB」（修复前是 10006「上传文件内容读取失败」）。
    #[test]
    fn upload_size_guard_rejects_over_category_limit() {
        let mut policy = model::default_policy("attachment");
        policy.max_size_mb = 100;
        let mb = 1024 * 1024;
        let error = ensure_size_allowed(&policy, 110 * mb, 128 * mb)
            .expect_err("110 MB 超过 100 MB 上限必须被拒");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, error_code::STORAGE_SIZE_EXCEEDED);
        assert!(
            error.msg.contains("该分类大小上限 100 MB"),
            "文案应指明分类上限：{}",
            error.msg
        );
        assert!(
            error.msg.contains("110.00 MB"),
            "文案应带实际大小：{}",
            error.msg
        );
    }

    /// 边界分支：分类策略放宽（5 GB）但服务端中转上限 128 MB → 报服务端上限并引导直传
    /// （不能报「超过分类上限」，否则运维会去改策略，改多少都没用）。
    #[test]
    fn upload_size_guard_rejects_over_server_limit() {
        let mut policy = model::default_policy("attachment");
        policy.max_size_mb = 5120;
        let mb = 1024 * 1024;
        let error =
            ensure_size_allowed(&policy, 200 * mb, 128 * mb).expect_err("超过服务端上限必须被拒");
        assert_eq!(error.status, 422);
        assert_eq!(error.code, error_code::STORAGE_SIZE_EXCEEDED);
        assert!(
            error.msg.contains("服务端单文件上限 128 MB"),
            "文案应指明服务端上限：{}",
            error.msg
        );
        assert!(
            error.msg.contains("presign"),
            "应引导预签名直传：{}",
            error.msg
        );
    }

    /// 正常分支：50 MB（< 100 MB 上限、< 128 MB 服务端上限）必须放行；
    /// 恰好等于上限的文件也要放行（Content-Length 含 multipart 开销，靠余量吸收）。
    #[test]
    fn upload_size_guard_allows_within_limits() {
        let mut policy = model::default_policy("attachment");
        policy.max_size_mb = 100;
        let mb = 1024 * 1024;
        assert!(ensure_size_allowed(&policy, 50 * mb, 128 * mb).is_ok());
        assert!(ensure_size_allowed(&policy, 100 * mb + 4096, 128 * mb).is_ok());
        // 策略 0 = 不限：只要不超服务端上限就放行
        policy.max_size_mb = 0;
        assert!(ensure_size_allowed(&policy, 120 * mb, 128 * mb).is_ok());
        assert!(ensure_size_allowed(&policy, 200 * mb, 128 * mb).is_err());
    }

    /// 读取阶段被框架掐断（413）：分类上限更严时报分类口径，否则报服务端口径；
    /// 非 413 的读取失败仍沿用 400 + 10006（不掩盖真正的表单错误）。
    #[test]
    fn upload_read_error_translates_body_limit() {
        let mb = 1024 * 1024;
        let mut strict = model::default_policy("attachment");
        strict.max_size_mb = 100;
        let error = upload_read_error(StatusCode::PAYLOAD_TOO_LARGE, &strict, 128 * mb);
        assert_eq!(error.status, 422);
        assert_eq!(error.code, error_code::STORAGE_SIZE_EXCEEDED);
        assert!(error.msg.contains("该分类大小上限 100 MB"), "{}", error.msg);

        let mut loose = model::default_policy("attachment");
        loose.max_size_mb = 5120;
        let error = upload_read_error(StatusCode::PAYLOAD_TOO_LARGE, &loose, 128 * mb);
        assert_eq!(error.code, error_code::STORAGE_SIZE_EXCEEDED);
        assert!(error.msg.contains("服务端单文件上限"), "{}", error.msg);

        let error = upload_read_error(StatusCode::BAD_REQUEST, &strict, 128 * mb);
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PAYLOAD_INVALID);
    }

    /// 边界：`Content-Length` 缺失 / 非数字 → `None`（chunked 上传走读完后的精确校验）。
    #[test]
    fn declared_content_length_parsing() {
        let mut headers = HeaderMap::new();
        assert_eq!(declared_content_length(&headers), None);
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("1048576"));
        assert_eq!(declared_content_length(&headers), Some(1_048_576));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("abc"));
        assert_eq!(declared_content_length(&headers), None);
    }
}

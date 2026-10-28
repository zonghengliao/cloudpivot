//! 存储配置与上传策略的强类型模型、数据访问与策略校验。
//!
//! 对应迁移 `V0040__create_storage_config`。
//!
//! # 分层
//!
//! 需要编写 SQL 的查询（分页、按键查找、唯一性统计、事务内改默认）与**策略校验**
//! 领域逻辑集中在**本文件**；仅调用框架快捷方法（`find_or_none` / `insert` /
//! `update` / `delete`）的简单读写由 `handler.rs` 直接执行。
//!
//! # 数据库侧的三道保险（`V0040` 已建，应用层不重复实现）
//!
//! | 约束 | 拦住什么 |
//! |------|---------|
//! | `uk_sys_storage_config_default` | 第二个默认后端（并发改默认也不会破） |
//! | `ck_sys_storage_config_remote` | 本地行带密钥 / S3 行缺密钥 |
//! | `ck_sys_storage_policy_size` / `_ttl` / `_ext` | 大小上限 0、TTL 越界、扩展名带点 |
//!
//! 应用层仍然先校验一遍（返回 400 的中文文案比数据库错误友好），数据库约束是兜底。

use alun::prelude::*;

use crate::shared::constants::EnableStatus;
use crate::shared::error_code;
use crate::utils;

/// `sys_storage_config` 表对应的强类型模型（存储后端配置）。
///
/// # 凭据存储
///
/// AK/SK **只存密文**：`ak_cipher/ak_nonce` 与 `sk_cipher/sk_nonce` 四列同写同读，
/// `key_version` 记录主密钥版本（支持轮换）。解密只发生在
/// [`crate::business::storage::router`] 构造后端实例时，且明文只在内存中存活。
#[derive(Debug, Clone, Model)]
#[table("sys_storage_config")]
pub struct SysStorageConfigModel {
    /// 主键（UUID v7）。
    pub id: uuid::Uuid,
    /// 后端编码（小写字母开头，路由键，写入 `sys_file.storage_code`）。
    pub config_code: String,
    /// 后端名称（展示用）。
    pub config_name: String,
    /// 后端类型：`LOCAL` / `S3`。
    pub backend_type: String,
    /// 端点（S3 必填，LOCAL 必须为空）。
    pub endpoint: Option<String>,
    /// 桶名（S3 必填）。
    pub bucket: Option<String>,
    /// 区域（S3 签名用，MinIO 可填 `us-east-1`）。
    pub region: String,
    /// 对象键前缀（空串表示桶根 / 上传根）。
    pub base_path: String,
    /// 是否使用 TLS。
    pub use_tls: bool,
    /// 是否 path 风格寻址（MinIO 必须 `true`）。
    pub path_style: bool,
    /// AccessKey 密文（Base64）。
    pub ak_cipher: Option<String>,
    /// AccessKey nonce（Base64）。
    pub ak_nonce: Option<String>,
    /// SecretKey 密文（Base64）。
    pub sk_cipher: Option<String>,
    /// SecretKey nonce（Base64）。
    pub sk_nonce: Option<String>,
    /// 主密钥版本（轮换用）。
    pub key_version: i16,
    /// 是否默认后端（全库最多一个有效默认）。
    pub is_default: bool,
    /// 状态（1 启用 / 0 停用）。
    pub status: EnableStatus,
    /// 健康状态：`UNKNOWN` / `HEALTHY` / `DOWN`。
    pub health_status: String,
    /// 最近一次探测时间。
    pub last_check_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 最近一次探测失败原因（脱敏后）。
    pub last_error: Option<String>,
    /// 排序号。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 创建人。
    pub create_by: Option<uuid::Uuid>,
    /// 更新人。
    pub update_by: Option<uuid::Uuid>,
    /// 软删标记。
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl SysStorageConfigModel {
    /// 该后端是否「远端对象存储」（需要凭据）。
    ///
    /// # 返回值
    ///
    /// `backend_type = "S3"` 时返回 `true`。
    pub fn is_remote(&self) -> bool {
        self.backend_type == "S3"
    }

    /// 配置是否可用于路由（启用且未删除）。
    ///
    /// # 返回值
    ///
    /// `status = 1` 且 `deleted_at IS NULL` 时返回 `true`。
    pub fn is_usable(&self) -> bool {
        self.status == EnableStatus::enabled && self.deleted_at.is_none()
    }
}

/// `sys_storage_policy` 表对应的强类型模型（按业务分类的上传策略）。
#[derive(Debug, Clone, Model)]
#[table("sys_storage_policy")]
pub struct SysStoragePolicyModel {
    /// 主键（UUID v7）。
    pub id: uuid::Uuid,
    /// 业务分类（与 `sys_file.category` 对应）。
    pub category: String,
    /// 分类名称（展示用）。
    pub category_name: String,
    /// 单文件大小上限（MB）。
    pub max_size_mb: i32,
    /// 扩展名白名单（小写逗号分隔、不带点；空串表示沿用全局默认）。
    pub allowed_extensions: String,
    /// MIME 白名单（逗号分隔；空串表示不校验）。
    pub allowed_mimes: String,
    /// 是否允许覆盖同名对象。
    pub allow_overwrite: bool,
    /// 是否允许浏览器直传。
    pub direct_upload: bool,
    /// 预签名有效期（秒）。
    pub presign_ttl_secs: i32,
    /// 指定后端编码（`None` 表示用默认后端）。
    pub storage_code: Option<String>,
    /// 状态（1 启用 / 0 停用）。
    pub status: EnableStatus,
    /// 排序号。
    pub sort_order: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 创建人。
    pub create_by: Option<uuid::Uuid>,
    /// 更新人。
    pub update_by: Option<uuid::Uuid>,
    /// 软删标记。
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 存储配置分页过滤条件。
pub struct StorageConfigFilter {
    /// 关键词（编码 / 名称模糊匹配）。
    pub keyword: Option<String>,
    /// 后端类型（`LOCAL` / `S3`）。
    pub backend_type: Option<String>,
    /// 状态。
    pub status: Option<EnableStatus>,
}

/// 上传策略分页过滤条件。
pub struct StoragePolicyFilter {
    /// 关键词（分类 / 名称模糊匹配）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<EnableStatus>,
}

// ==================== 存储配置：数据访问 ====================

/// 分页查询存储后端配置（AK/SK 密文列也一并取出，响应层只回掩码）。
///
/// # 参数
///
/// - `filter`：过滤条件。
/// - `page`：页码（从 1 开始）。
/// - `page_size`：每页条数。
///
/// # 返回值
///
/// 按 `sort_order, created_at` 升序的分页结果。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn page_configs(
    filter: &StorageConfigFilter,
    page: u64,
    page_size: u64,
) -> std::result::Result<Page<SysStorageConfigModel>, DbError> {
    let mut sql = String::from("SELECT * FROM sys_storage_config WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();

    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (config_code LIKE ? OR config_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(backend_type) = filter.backend_type.as_deref() {
        sql.push_str(" AND backend_type = ?");
        params.push(Value::from(backend_type));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    sql.push_str(" ORDER BY sort_order ASC, created_at ASC");

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = db()
        .paginate_with_params(&sql, params, page, page_size)
        .await?;
    let list = list
        .into_iter()
        .map(SysStorageConfigModel::from_row)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 统计后端编码在未删除记录中的数量（唯一性校验）。
///
/// # 参数
///
/// - `config_code`：后端编码。
/// - `exclude_id`：需要排除的主键（更新时排除自身）。
///
/// # 返回值
///
/// 命中条数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_config_code(
    config_code: &str,
    exclude_id: Option<&uuid::Uuid>,
) -> std::result::Result<i64, DbError> {
    match exclude_id {
        Some(id) => {
            SysStorageConfigModel::count_by(
                db(),
                "config_code = ? AND deleted_at IS NULL AND id <> ?",
                (config_code, id),
            )
            .await
        }
        None => {
            SysStorageConfigModel::count_by(
                db(),
                "config_code = ? AND deleted_at IS NULL",
                (config_code,),
            )
            .await
        }
    }
}

/// 按编码查询可用（未删除）的后端配置。
///
/// # 参数
///
/// - `config_code`：后端编码。
///
/// # 返回值
///
/// 命中返回模型，未命中返回 `None`。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn find_by_code(
    config_code: &str,
) -> std::result::Result<Option<SysStorageConfigModel>, DbError> {
    let rows = db()
        .find_with_params(
            "SELECT * FROM sys_storage_config WHERE config_code = ? AND deleted_at IS NULL LIMIT 1",
            vec![Value::from(config_code)],
        )
        .await?;
    match rows.into_iter().next() {
        Some(row) => Ok(Some(SysStorageConfigModel::from_row(row)?)),
        None => Ok(None),
    }
}

/// 装载全部启用中的后端配置（路由表重建用）。
///
/// # 返回值
///
/// `status = 1` 且未软删的配置，按 `sort_order` 升序。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn load_enabled_configs() -> std::result::Result<Vec<SysStorageConfigModel>, DbError> {
    let rows = db()
        .find_with_params(
            "SELECT * FROM sys_storage_config WHERE deleted_at IS NULL AND status = 1 \
             ORDER BY sort_order ASC, created_at ASC",
            Vec::new(),
        )
        .await?;
    rows.into_iter()
        .map(SysStorageConfigModel::from_row)
        .collect()
}

/// 统计引用某后端的有效文件数（删除前的占用校验）。
///
/// # 参数
///
/// - `config_code`：后端编码（`sys_file.storage_code`）。
///
/// # 返回值
///
/// 未软删文件数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_files_using(config_code: &str) -> std::result::Result<i64, DbError> {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM sys_file WHERE storage_code = ? AND deleted_at IS NULL",
        vec![Value::from(config_code)],
    )
    .await
}

/// 事务内把指定后端设为唯一默认（先清后设，唯一索引兜底）。
///
/// # 参数
///
/// - `id`：目标后端主键。
///
/// # 返回值
///
/// 成功返回 `Ok(())`。
///
/// # 异常情况
///
/// 目标不存在返回 [`DbError::InvalidValue`]；数据库异常返回 [`DbError`]。
///
/// # 为什么用事务
///
/// 「先清空旧默认，再设新默认」两条语句之间若被并发请求插入第三个默认，
/// `uk_sys_storage_config_default` 会拒绝第二条语句——错误可控，不会出现两个默认。
pub async fn set_default(id: &uuid::Uuid) -> std::result::Result<(), DbError> {
    let target = *id;
    db().transaction(|tx| async move {
        tx.execute_with_params(
            "UPDATE sys_storage_config SET is_default = FALSE, updated_at = CURRENT_TIMESTAMP \
             WHERE is_default AND deleted_at IS NULL AND id <> ?",
            vec![Value::from(target)],
        )
        .await?;
        let affected = tx
            .execute_with_params(
                "UPDATE sys_storage_config SET is_default = TRUE, updated_at = CURRENT_TIMESTAMP \
                 WHERE id = ? AND deleted_at IS NULL",
                vec![Value::from(target)],
            )
            .await?;
        if affected == 0 {
            return Err(DbError::InvalidValue("存储后端不存在或已删除".to_string()));
        }
        Ok(())
    })
    .await
}

/// 回写健康探测结果。
///
/// # 参数
///
/// - `id`：后端主键。
/// - `health_status`：`HEALTHY` / `DOWN`。
/// - `last_error`：失败原因（已脱敏），成功传 `None`。
///
/// # 返回值
///
/// 成功返回 `Ok(())`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn update_health(
    id: &uuid::Uuid,
    health_status: &str,
    last_error: Option<&str>,
) -> std::result::Result<(), DbError> {
    db().execute_with_params(
        "UPDATE sys_storage_config SET health_status = ?, last_check_at = CURRENT_TIMESTAMP, \
         last_error = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        vec![
            Value::from(health_status),
            match last_error {
                Some(message) => Value::from(message),
                None => Value::Null,
            },
            Value::from(*id),
        ],
    )
    .await?;
    Ok(())
}

// ==================== 上传策略：数据访问 ====================

/// 分页查询上传策略。
///
/// # 参数
///
/// - `filter`：过滤条件。
/// - `page`：页码。
/// - `page_size`：每页条数。
///
/// # 返回值
///
/// 按 `sort_order, category` 升序的分页结果。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn page_policies(
    filter: &StoragePolicyFilter,
    page: u64,
    page_size: u64,
) -> std::result::Result<Page<SysStoragePolicyModel>, DbError> {
    let mut sql = String::from("SELECT * FROM sys_storage_policy WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();

    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(" AND (category LIKE ? OR category_name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND status = ?");
        params.push(TypeMapper::to_value(&status));
    }
    sql.push_str(" ORDER BY sort_order ASC, category ASC");

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = db()
        .paginate_with_params(&sql, params, page, page_size)
        .await?;
    let list = list
        .into_iter()
        .map(SysStoragePolicyModel::from_row)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Page {
        list,
        total,
        page,
        size,
        pages,
    })
}

/// 按分类查询启用中的上传策略（上传时取策略用）。
///
/// # 参数
///
/// - `category`：业务分类。
///
/// # 返回值
///
/// 命中返回模型，未命中返回 `None`（调用方回退内置默认策略）。
///
/// # 异常情况
///
/// 数据库异常或行映射失败返回 [`DbError`]。
pub async fn find_policy_by_category(
    category: &str,
) -> std::result::Result<Option<SysStoragePolicyModel>, DbError> {
    let rows = db()
        .find_with_params(
            "SELECT * FROM sys_storage_policy WHERE category = ? AND deleted_at IS NULL LIMIT 1",
            vec![Value::from(category)],
        )
        .await?;
    match rows.into_iter().next() {
        Some(row) => Ok(Some(SysStoragePolicyModel::from_row(row)?)),
        None => Ok(None),
    }
}

/// 统计分类在未删除策略中的数量（唯一性校验）。
///
/// # 参数
///
/// - `category`：业务分类。
/// - `exclude_id`：更新时排除自身。
///
/// # 返回值
///
/// 命中条数。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn count_policy_category(
    category: &str,
    exclude_id: Option<&uuid::Uuid>,
) -> std::result::Result<i64, DbError> {
    match exclude_id {
        Some(id) => {
            SysStoragePolicyModel::count_by(
                db(),
                "category = ? AND deleted_at IS NULL AND id <> ?",
                (category, id),
            )
            .await
        }
        None => {
            SysStoragePolicyModel::count_by(
                db(),
                "category = ? AND deleted_at IS NULL",
                (category,),
            )
            .await
        }
    }
}

// ==================== 策略校验（纯函数，便于单测） ====================

/// 上传校验失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyViolation {
    /// 超过大小上限。
    SizeExceeded {
        /// 实际字节数。
        size_bytes: u64,
        /// 策略上限（MB）。
        max_size_mb: i32,
    },
    /// 扩展名不在白名单。
    ExtensionNotAllowed {
        /// 实际扩展名（无扩展名时为 `(无)`）。
        extension: String,
        /// 白名单原文。
        allowed: String,
    },
    /// MIME 不在白名单。
    MimeNotAllowed {
        /// 实际 MIME。
        mime: String,
        /// 白名单原文。
        allowed: String,
    },
    /// 该分类未开启直传。
    DirectUploadDisabled {
        /// 业务分类。
        category: String,
    },
}

impl PolicyViolation {
    /// 对应的 HTTP 状态码。
    ///
    /// # 返回值
    ///
    /// 超限 / 未开直传 → 422；扩展名 / MIME 不符 → 400。
    pub fn http_status(&self) -> u16 {
        match self {
            Self::SizeExceeded { .. } | Self::DirectUploadDisabled { .. } => 422,
            Self::ExtensionNotAllowed { .. } | Self::MimeNotAllowed { .. } => 400,
        }
    }

    /// 对应的业务错误码（见 [`crate::shared::error_code`]）。
    ///
    /// # 返回值
    ///
    /// `60003` / `60004` / `60005` / `60006`。
    pub fn code(&self) -> i32 {
        match self {
            Self::SizeExceeded { .. } => error_code::STORAGE_SIZE_EXCEEDED,
            Self::ExtensionNotAllowed { .. } => error_code::STORAGE_TYPE_NOT_ALLOWED,
            Self::MimeNotAllowed { .. } => error_code::STORAGE_MIME_NOT_ALLOWED,
            Self::DirectUploadDisabled { .. } => error_code::STORAGE_DIRECT_UPLOAD_DISABLED,
        }
    }

    /// 面向用户的中文消息（文案见 PRD，不含内部路径与凭据）。
    ///
    /// # 返回值
    ///
    /// 中文提示。
    pub fn message(&self) -> String {
        match self {
            Self::SizeExceeded {
                size_bytes,
                max_size_mb,
            } => format!(
                "文件大小 {:.2} MB 超出该分类上限 {max_size_mb} MB",
                *size_bytes as f64 / (1024.0 * 1024.0)
            ),
            Self::ExtensionNotAllowed { extension, allowed } => {
                format!("不支持的文件类型 .{extension}，允许的类型：{allowed}")
            }
            Self::MimeNotAllowed { mime, allowed } => {
                format!("文件内容类型 {mime} 与允许的类型不符（{allowed}）")
            }
            Self::DirectUploadDisabled { category } => {
                format!("分类 {category} 未开启直传，请改用服务端上传接口")
            }
        }
    }
}

/// 取文件扩展名（小写，不含点）。
///
/// # 参数
///
/// - `file_name`：文件名（可含路径，只看最后一段）。
///
/// # 返回值
///
/// 有扩展名返回 `Some("pdf")`；无扩展名或点号在首位返回 `None`。
pub fn extension_of(file_name: &str) -> Option<String> {
    let leaf = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file_name)
        .trim();
    match leaf.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => Some(ext.to_ascii_lowercase()),
        _ => None,
    }
}

/// 把逗号分隔的白名单拆成条目（去空白、转小写、丢空项）。
///
/// # 参数
///
/// - `raw`：原始文本。
///
/// # 返回值
///
/// 条目列表。
pub fn parse_whitelist(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

/// 校验一次上传是否符合策略（扩展名 / 大小 / MIME）。
///
/// # 参数
///
/// - `policy`：分类策略。
/// - `file_name`：原始文件名。
/// - `size_bytes`：文件字节数。
/// - `mime`：客户端声明的 MIME（`None` 表示未知，跳过 MIME 校验）。
///
/// # 返回值
///
/// 合规返回 `Ok(())`。
///
/// # 异常情况
///
/// 超限 / 类型不允许时返回 [`PolicyViolation`]。
///
/// # 说明
///
/// MIME 白名单支持 `image/*` 形态的通配（只比较主类型），空串表示不校验
/// （这是设计取舍：服务端要靠「直传完成」阶段的 `stat()` 复核真实类型，
/// 见架构文档 §2.6 第 5 点）。
pub fn check_upload(
    policy: &SysStoragePolicyModel,
    file_name: &str,
    size_bytes: u64,
    mime: Option<&str>,
) -> std::result::Result<(), PolicyViolation> {
    // 1. 大小
    let max_bytes = (policy.max_size_mb.max(0) as u64) * 1024 * 1024;
    if max_bytes > 0 && size_bytes > max_bytes {
        return Err(PolicyViolation::SizeExceeded {
            size_bytes,
            max_size_mb: policy.max_size_mb,
        });
    }

    // 2. 扩展名
    let allowed_extensions = parse_whitelist(&policy.allowed_extensions);
    if !allowed_extensions.is_empty() {
        let extension = extension_of(file_name).unwrap_or_else(|| "(无)".to_string());
        if !allowed_extensions.contains(&extension) {
            return Err(PolicyViolation::ExtensionNotAllowed {
                extension,
                allowed: allowed_extensions.join(","),
            });
        }
    }

    // 3. MIME
    let allowed_mimes = parse_whitelist(&policy.allowed_mimes);
    if let (Some(actual), false) = (mime.map(str::trim), allowed_mimes.is_empty()) {
        let actual_lower = actual.to_ascii_lowercase();
        let matched = allowed_mimes.iter().any(|allowed| {
            allowed == &actual_lower
                || allowed
                    .strip_suffix("/*")
                    .is_some_and(|prefix| actual_lower.starts_with(&format!("{prefix}/")))
        });
        if !matched {
            return Err(PolicyViolation::MimeNotAllowed {
                mime: actual_lower,
                allowed: allowed_mimes.join(","),
            });
        }
    }

    Ok(())
}

/// 校验直传是否被该分类允许。
///
/// # 参数
///
/// - `policy`：分类策略。
///
/// # 返回值
///
/// 允许返回 `Ok(())`。
///
/// # 异常情况
///
/// `direct_upload = false` 时返回 [`PolicyViolation::DirectUploadDisabled`]。
pub fn check_direct_upload(
    policy: &SysStoragePolicyModel,
) -> std::result::Result<(), PolicyViolation> {
    if policy.direct_upload {
        Ok(())
    } else {
        Err(PolicyViolation::DirectUploadDisabled {
            category: policy.category.clone(),
        })
    }
}

/// 校验策略的**配置值**本身是否合法（保存前拦截，返回中文文案）。
///
/// # 参数
///
/// - `max_size_mb`：大小上限（1 ~ 5120）。
/// - `presign_ttl_secs`：预签名有效期（60 ~ 604800）。
/// - `allowed_extensions`：扩展名白名单原文（小写、逗号分隔、不带点）。
/// - `allowed_mimes`：MIME 白名单原文（逗号分隔）。
///
/// # 返回值
///
/// 合法返回 `Ok(())`。
///
/// # 异常情况
///
/// 任一项不合法返回 `Err(中文消息)`。数据库侧还有
/// `ck_sys_storage_policy_size` / `_ttl` / `_ext` 兜底。
pub fn validate_policy_fields(
    max_size_mb: i32,
    presign_ttl_secs: i32,
    allowed_extensions: &str,
    allowed_mimes: &str,
) -> std::result::Result<(), String> {
    if !(1..=5120).contains(&max_size_mb) {
        return Err("大小上限必须在 1 ~ 5120 MB 之间".to_string());
    }
    if !(60..=604800).contains(&presign_ttl_secs) {
        return Err("预签名有效期必须在 60 ~ 604800 秒（7 天）之间".to_string());
    }
    for item in allowed_extensions.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item.starts_with('.') {
            return Err(format!("扩展名不允许带点：{item}（应写成 jpg,png,pdf）"));
        }
        if item.contains(' ') || item.contains('/') || item.contains('\\') {
            return Err(format!("扩展名含非法字符：{item}"));
        }
        if item != item.to_ascii_lowercase() {
            return Err(format!("扩展名必须小写：{item}"));
        }
    }
    for item in allowed_mimes.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if !item.contains('/') {
            return Err(format!(
                "MIME 白名单项缺少主类型：{item}（应写成 image/png）"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一条测试策略（不落库）。
    fn sample_policy() -> SysStoragePolicyModel {
        let now = chrono::Utc::now();
        SysStoragePolicyModel {
            id: uuid::Uuid::now_v7(),
            category: "attachment".to_string(),
            category_name: "附件".to_string(),
            max_size_mb: 50,
            allowed_extensions: "jpg,png,pdf".to_string(),
            allowed_mimes: "image/png,image/jpeg,application/pdf".to_string(),
            allow_overwrite: false,
            direct_upload: true,
            presign_ttl_secs: 900,
            storage_code: None,
            status: EnableStatus::enabled,
            sort_order: 0,
            remark: None,
            created_at: now,
            updated_at: now,
            create_by: None,
            update_by: None,
            deleted_at: None,
        }
    }

    /// 正常路径：合规文件通过校验。
    #[test]
    fn policy_accepts_valid_upload() {
        let policy = sample_policy();
        assert_eq!(
            check_upload(&policy, "合同.pdf", 1024 * 1024, Some("application/pdf")),
            Ok(())
        );
        assert_eq!(check_direct_upload(&policy), Ok(()));
    }

    /// 错误分支：超过大小上限 → 422 / 60003。
    #[test]
    fn policy_rejects_oversize() {
        let policy = sample_policy();
        let violation = check_upload(
            &policy,
            "big.pdf",
            51 * 1024 * 1024,
            Some("application/pdf"),
        )
        .expect_err("超限必须被拒绝");
        assert_eq!(violation.http_status(), 422);
        assert_eq!(violation.code(), error_code::STORAGE_SIZE_EXCEEDED);
        assert!(violation.message().contains("51.00 MB"));
        // 恰好等于上限应放行（off-by-one 回归）
        assert_eq!(
            check_upload(
                &policy,
                "edge.pdf",
                50 * 1024 * 1024,
                Some("application/pdf")
            ),
            Ok(())
        );
    }

    /// 错误分支：扩展名不在白名单（含无扩展名、大写扩展名）→ 400 / 60004。
    #[test]
    fn policy_rejects_bad_extension() {
        let policy = sample_policy();

        let violation = check_upload(&policy, "evil.exe", 10, None).expect_err("exe 必须被拒绝");
        assert_eq!(violation.http_status(), 400);
        assert_eq!(violation.code(), error_code::STORAGE_TYPE_NOT_ALLOWED);
        assert!(violation.message().contains(".exe"));

        // 无扩展名
        let violation = check_upload(&policy, "noext", 10, None).expect_err("无扩展名必须被拒绝");
        assert!(violation.message().contains(".(无)"));

        // 大写扩展名按小写比对（PDF → pdf 合法）
        assert_eq!(check_upload(&policy, "A.PDF", 10, None), Ok(()));
    }

    /// 错误分支：扩展名与 MIME 不一致 → 400 / 60005；通配 MIME 支持。
    #[test]
    fn policy_rejects_mime_mismatch() {
        let policy = sample_policy();
        let violation =
            check_upload(&policy, "a.pdf", 10, Some("text/html")).expect_err("MIME 不符必须被拒绝");
        assert_eq!(violation.http_status(), 400);
        assert_eq!(violation.code(), error_code::STORAGE_MIME_NOT_ALLOWED);

        // image/* 通配：image/webp 在允许范围内
        let mut wildcard = policy.clone();
        wildcard.allowed_mimes = "image/*".to_string();
        assert_eq!(
            check_upload(&wildcard, "a.png", 10, Some("image/webp")),
            Ok(())
        );
        assert!(check_upload(&wildcard, "a.png", 10, Some("text/plain")).is_err());

        // 未声明 MIME 时跳过该校验（真实类型由 complete 阶段 stat 复核）
        assert_eq!(check_upload(&policy, "a.pdf", 10, None), Ok(()));
    }

    /// 错误分支：白名单为空表示不限制扩展名（全局默认策略语义）。
    #[test]
    fn policy_empty_whitelist_allows_anything() {
        let mut policy = sample_policy();
        policy.allowed_extensions = String::new();
        policy.allowed_mimes = String::new();
        assert_eq!(
            check_upload(&policy, "anything.xyz", 10, Some("a/b")),
            Ok(())
        );
    }

    /// 错误分支：未开直传时明确拒绝（60006）。
    #[test]
    fn policy_rejects_direct_upload_disabled() {
        let mut policy = sample_policy();
        policy.direct_upload = false;
        let violation = check_direct_upload(&policy).expect_err("未开直传必须被拒绝");
        assert_eq!(violation.http_status(), 422);
        assert_eq!(violation.code(), error_code::STORAGE_DIRECT_UPLOAD_DISABLED);
    }

    /// 错误分支：策略配置值本身非法时必须拒绝保存。
    #[test]
    fn policy_rejects_invalid_config() {
        // 大小上限 0 / 超 5120
        assert!(validate_policy_fields(0, 900, "jpg", "image/jpeg").is_err());
        assert!(validate_policy_fields(5121, 900, "jpg", "image/jpeg").is_err());
        // TTL 越界
        assert!(validate_policy_fields(50, 59, "jpg", "image/jpeg").is_err());
        assert!(validate_policy_fields(50, 604_801, "jpg", "image/jpeg").is_err());
        // 扩展名带点（DB 约束 ck_sys_storage_policy_ext 的同款规则）
        assert!(validate_policy_fields(50, 900, ".jpg,.png", "image/jpeg").is_err());
        // 扩展名含空格 / 大写
        assert!(validate_policy_fields(50, 900, "j pg", "image/jpeg").is_err());
        assert!(validate_policy_fields(50, 900, "JPG", "image/jpeg").is_err());
        // MIME 缺主类型
        assert!(validate_policy_fields(50, 900, "jpg", "png").is_err());
        // 合法值（含空串白名单）
        assert!(validate_policy_fields(50, 900, "jpg,png", "image/jpeg,image/png").is_ok());
        assert!(validate_policy_fields(1, 60, "", "").is_ok());
    }

    /// 边界：扩展名解析（点号在首位 / 无扩展名 / 路径）。
    #[test]
    fn policy_extension_parsing() {
        assert_eq!(extension_of("a.PDF").as_deref(), Some("pdf"));
        assert_eq!(extension_of("dir.x/a.tar.gz").as_deref(), Some("gz"));
        assert_eq!(extension_of(".hidden"), None);
        assert_eq!(extension_of("noext"), None);
        assert_eq!(extension_of(""), None);
    }

    /// 边界：白名单解析（去空白、转小写、丢空项）。
    #[test]
    fn policy_whitelist_parsing() {
        assert_eq!(
            parse_whitelist(" JPG, png ,, Pdf "),
            vec!["jpg".to_string(), "png".to_string(), "pdf".to_string()]
        );
        assert!(parse_whitelist("").is_empty());
        assert!(parse_whitelist(" , , ").is_empty());
    }
}

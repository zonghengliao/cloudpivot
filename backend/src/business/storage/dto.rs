//! 存储配置 / 上传策略的请求与响应 DTO。
//!
//! # 铁律：AK/SK 只写不读
//!
//! 响应结构里**没有** `ak` / `sk` 字段，只有 [`StorageConfigRes::ak_masked`]
//! （如 `CPAK****9f2c`）与 [`StorageConfigRes::has_secret`]。即便调用方拿到完整
//! JSON，也取不到任何明文凭据——这不是「前端不展示」，而是「后端不给」。
//!
//! 单测 `dto_masked_ak_never_leaks_plaintext` 断言序列化结果里不含明文 AK/SK。

use alun::prelude::*;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::business::storage::cipher::{self, MasterKey};
use crate::business::storage::model::{
    StorageFileModel, SysStorageConfigModel, SysStoragePolicyModel,
};

/// 存储配置分页查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfigPageQuery {
    /// 页码（从 1 开始）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（编码 / 名称模糊）。
    pub keyword: Option<String>,
    /// 后端类型（`LOCAL` / `S3`）。
    pub backend_type: Option<String>,
    /// 状态（1 启用 / 0 停用）。
    pub status: Option<i16>,
}

/// 存储配置新增 / 编辑请求。
///
/// # 密钥语义（编辑时）
///
/// - `ak` / `sk` 都不传或都为空串：**保持原密钥不变**（前端「只写不读」的必然结果）；
/// - 只传其中一个：400（避免出现「AK 换了、SK 还是旧的」这种半个凭据）；
/// - 本地后端（`LOCAL`）不允许带密钥：400（数据库 `ck_sys_storage_config_remote` 同样拦）。
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct StorageConfigSaveReq {
    /// 后端编码（小写字母开头，2-64 位；写入 `sys_file.storage_code`，建后不可改）。
    #[validate(length(min = 2, max = 64, message = "后端编码长度需为 2-64 个字符"))]
    pub config_code: String,
    /// 后端名称。
    #[validate(length(min = 1, max = 100, message = "后端名称长度需为 1-100 个字符"))]
    pub config_name: String,
    /// 后端类型：`LOCAL` / `S3`。
    #[validate(length(min = 1, max = 16, message = "后端类型长度需为 1-16 个字符"))]
    pub backend_type: String,
    /// 端点（S3 必填，如 `http://minio.local:9000`）。
    #[validate(length(max = 255, message = "端点长度不能超过 255 个字符"))]
    pub endpoint: Option<String>,
    /// 桶名（S3 必填）。
    #[validate(length(max = 128, message = "桶名长度不能超过 128 个字符"))]
    pub bucket: Option<String>,
    /// 区域（缺省 `us-east-1`）。
    #[validate(length(max = 64, message = "区域长度不能超过 64 个字符"))]
    pub region: Option<String>,
    /// 对象键前缀（缺省空串）。
    #[validate(length(max = 255, message = "对象键前缀不能超过 255 个字符"))]
    pub base_path: Option<String>,
    /// 是否使用 TLS（缺省 true）。
    pub use_tls: Option<bool>,
    /// 是否 path 风格寻址（MinIO 必须 true；缺省 true）。
    pub path_style: Option<bool>,
    /// AccessKey 明文（仅在内存使用，落库前加密）。
    pub ak: Option<String>,
    /// SecretKey 明文（仅在内存使用，落库前加密）。
    pub sk: Option<String>,
    /// 是否设为默认后端（缺省 false）。
    pub is_default: Option<bool>,
    /// 状态（1 启用 / 0 停用，缺省启用）。
    pub status: Option<i16>,
    /// 排序号（缺省 0）。
    pub sort_order: Option<i32>,
    /// 备注。
    #[validate(length(max = 500, message = "备注长度不能超过 500 个字符"))]
    pub remark: Option<String>,
}

impl StorageConfigSaveReq {
    /// 请求里是否携带了新凭据（两个字段都非空白）。
    ///
    /// # 返回值
    ///
    /// 携带返回 `Some((ak, sk))`；都未携带返回 `None`。
    ///
    /// # 异常情况
    ///
    /// 只携带其中一个返回 400（不允许半个凭据）。
    pub fn credentials(&self) -> std::result::Result<Option<(String, String)>, ApiError> {
        let ak = self.ak.as_deref().map(str::trim).filter(|v| !v.is_empty());
        let sk = self.sk.as_deref().map(str::trim).filter(|v| !v.is_empty());
        match (ak, sk) {
            (None, None) => Ok(None),
            (Some(ak), Some(sk)) => Ok(Some((ak.to_string(), sk.to_string()))),
            _ => Err(ApiError::new(
                400,
                crate::shared::error_code::PARAM_INVALID,
                "AccessKey 与 SecretKey 必须同时提供（或都不提供表示不修改）",
            )),
        }
    }
}

/// 存储配置响应（**不含任何明文凭据**）。
#[derive(Debug, Clone, Serialize)]
pub struct StorageConfigRes {
    /// 主键。
    pub id: String,
    /// 后端编码。
    pub config_code: String,
    /// 后端名称。
    pub config_name: String,
    /// 后端类型。
    pub backend_type: String,
    /// 端点。
    pub endpoint: String,
    /// 桶名。
    pub bucket: String,
    /// 区域。
    pub region: String,
    /// 对象键前缀。
    pub base_path: String,
    /// 是否 TLS。
    pub use_tls: bool,
    /// 是否 path 风格。
    pub path_style: bool,
    /// AccessKey 掩码（如 `CPAK****9f2c`；未配置时为 `****`）。
    pub ak_masked: String,
    /// 是否已配置密钥。
    pub has_secret: bool,
    /// 主密钥版本。
    pub key_version: i16,
    /// 是否默认后端。
    pub is_default: bool,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 健康状态（`UNKNOWN` / `HEALTHY` / `DOWN`）。
    pub health_status: String,
    /// 最近探测时间。
    pub last_check_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 最近探测失败原因。
    pub last_error: String,
    /// 排序号。
    pub sort_order: i32,
    /// 备注。
    pub remark: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl StorageConfigRes {
    /// 由模型构造响应。
    ///
    /// # 参数
    ///
    /// - `model`：后端配置。
    /// - `master_key`：主密钥；`None` 或解密失败时掩码降级为 `****`（仍不泄露）。
    ///
    /// # 返回值
    ///
    /// 响应对象。
    pub fn from_model(model: &SysStorageConfigModel, master_key: Option<&MasterKey>) -> Self {
        let (ak_masked, has_secret) = match (
            model.ak_cipher.as_deref(),
            model.ak_nonce.as_deref(),
            master_key,
        ) {
            (Some(cipher_text), Some(nonce), Some(key)) => {
                match cipher::decrypt(key, cipher_text, nonce) {
                    Some(ak) => (cipher::mask_secret(&ak), true),
                    // 主密钥不匹配：不暴露「解不开」的细节，只回掩码 + has_secret=true
                    None => ("****".to_string(), true),
                }
            }
            (Some(_), Some(_), None) => ("****".to_string(), true),
            _ => ("****".to_string(), false),
        };

        Self {
            id: model.id.to_string(),
            config_code: model.config_code.clone(),
            config_name: model.config_name.clone(),
            backend_type: model.backend_type.clone(),
            endpoint: model.endpoint.clone().unwrap_or_default(),
            bucket: model.bucket.clone().unwrap_or_default(),
            region: model.region.clone(),
            base_path: model.base_path.clone(),
            use_tls: model.use_tls,
            path_style: model.path_style,
            ak_masked,
            has_secret,
            key_version: model.key_version,
            is_default: model.is_default,
            status: model.status.as_i16(),
            health_status: model.health_status.clone(),
            last_check_at: model.last_check_at,
            last_error: model.last_error.clone().unwrap_or_default(),
            sort_order: model.sort_order,
            remark: model.remark.clone().unwrap_or_default(),
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

/// 连通性测试响应。
#[derive(Debug, Clone, Serialize)]
pub struct StorageTestRes {
    /// 后端编码。
    pub config_code: String,
    /// 后端类型。
    pub backend_type: String,
    /// 是否可用。
    pub healthy: bool,
    /// 探测耗时（毫秒）。
    pub latency_ms: u64,
    /// 结果说明（失败时是脱敏后的原因）。
    pub message: String,
    /// 探测时间。
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

/// 上传策略分页查询参数。
#[derive(Debug, Clone, Deserialize)]
pub struct StoragePolicyPageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（分类 / 名称模糊）。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<i16>,
}

/// 上传策略编辑请求（策略只允许改，不允许增删——分类由业务定义）。
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct StoragePolicySaveReq {
    /// 分类名称。
    #[validate(length(min = 1, max = 100, message = "分类名称长度需为 1-100 个字符"))]
    pub category_name: String,
    /// 单文件大小上限（MB，1 ~ 5120）。
    pub max_size_mb: i32,
    /// 扩展名白名单（小写逗号分隔、不带点；空串表示不限制）。
    #[validate(length(max = 1000, message = "扩展名白名单不能超过 1000 个字符"))]
    pub allowed_extensions: Option<String>,
    /// MIME 白名单（逗号分隔；空串表示不校验）。
    #[validate(length(max = 1000, message = "MIME 白名单不能超过 1000 个字符"))]
    pub allowed_mimes: Option<String>,
    /// 是否允许覆盖同名对象。
    pub allow_overwrite: Option<bool>,
    /// 是否允许直传。
    pub direct_upload: Option<bool>,
    /// 预签名有效期（秒，60 ~ 604800）。
    pub presign_ttl_secs: i32,
    /// 指定后端编码（空表示默认后端）。
    #[validate(length(max = 64, message = "后端编码不能超过 64 个字符"))]
    pub storage_code: Option<String>,
    /// 状态。
    pub status: Option<i16>,
    /// 排序号。
    pub sort_order: Option<i32>,
    /// 备注。
    #[validate(length(max = 500, message = "备注长度不能超过 500 个字符"))]
    pub remark: Option<String>,
}

/// 上传策略响应。
#[derive(Debug, Clone, Serialize)]
pub struct StoragePolicyRes {
    /// 主键。
    pub id: String,
    /// 业务分类。
    pub category: String,
    /// 分类名称。
    pub category_name: String,
    /// 单文件大小上限（MB）。
    pub max_size_mb: i32,
    /// 扩展名白名单。
    pub allowed_extensions: String,
    /// MIME 白名单。
    pub allowed_mimes: String,
    /// 是否允许覆盖。
    pub allow_overwrite: bool,
    /// 是否允许直传。
    pub direct_upload: bool,
    /// 预签名有效期（秒）。
    pub presign_ttl_secs: i32,
    /// 指定后端编码（空表示默认后端）。
    pub storage_code: String,
    /// 状态。
    pub status: i16,
    /// 排序号。
    pub sort_order: i32,
    /// 备注。
    pub remark: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl StoragePolicyRes {
    /// 由模型构造响应。
    ///
    /// # 参数
    ///
    /// - `model`：上传策略。
    ///
    /// # 返回值
    ///
    /// 响应对象。
    pub fn from_model(model: &SysStoragePolicyModel) -> Self {
        Self {
            id: model.id.to_string(),
            category: model.category.clone(),
            category_name: model.category_name.clone(),
            max_size_mb: model.max_size_mb,
            allowed_extensions: model.allowed_extensions.clone(),
            allowed_mimes: model.allowed_mimes.clone(),
            allow_overwrite: model.allow_overwrite,
            direct_upload: model.direct_upload,
            presign_ttl_secs: model.presign_ttl_secs,
            storage_code: model.storage_code.clone().unwrap_or_default(),
            status: model.status.as_i16(),
            sort_order: model.sort_order,
            remark: model.remark.clone().unwrap_or_default(),
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

// ==================== 文件接口（A2：上传 / 直传 / 下载 / 软删 / 重新登记） ====================

/// 上传查询参数（`?category=`，缺省 `attachment`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageUploadQuery {
    /// 业务分类。
    pub category: Option<String>,
}

/// 预签名直传初始化请求（`POST /business/storage/presign`）。
///
/// # 语义
///
/// ① 按 `category` 取策略校验大小 / 扩展名 / MIME；② 命中 `file_hash` 直接秒传
/// （返回 `duplicated=true` + 已有记录，**不签 URL**）；③ 未命中则签一个 PUT URL，
/// 前端直传对象存储，完成后调 `POST /business/storage/complete` 登记。
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct StoragePresignReq {
    /// 业务分类。
    #[validate(length(min = 1, max = 50, message = "分类长度 1-50"))]
    pub category: String,
    /// 原始文件名（决定扩展名校验与对象键后缀）。
    #[validate(length(min = 1, max = 255, message = "文件名长度 1-255"))]
    pub file_name: String,
    /// 文件字节数（可选：给了就先做大小校验，避免白签一个必然失败的 URL）。
    pub file_size: Option<u64>,
    /// 内容 SHA-256（可选：给了就能秒传短路）。
    pub file_hash: Option<String>,
    /// MIME（可选：给了就做 MIME 白名单校验，并写入对象元数据）。
    pub mime_type: Option<String>,
}

/// 预签名直传初始化响应。
#[derive(Debug, Clone, Serialize)]
pub struct StoragePresignRes {
    /// 对象键（`complete` 阶段原样回传）。
    pub file_key: String,
    /// 直传 URL（已签名）。
    pub upload_url: String,
    /// HTTP 方法（恒 `PUT`）。
    pub method: String,
    /// 客户端应当携带的请求头。
    pub headers: Vec<StorageHeader>,
    /// 链接过期时间。
    pub expire_at: chrono::DateTime<chrono::Utc>,
    /// 目标后端编码。
    pub storage_code: String,
    /// 业务分类。
    pub category: String,
    /// 是否命中秒传（命中时 `upload_url` 为空串、`file` 为已有记录）。
    pub duplicated: bool,
    /// 秒传命中的已有记录。
    pub file: Option<StorageFileRes>,
}

/// 一个 HTTP 请求头（`name` / `value`）。
#[derive(Debug, Clone, Serialize)]
pub struct StorageHeader {
    /// 头名。
    pub name: String,
    /// 头值。
    pub value: String,
}

/// 直传完成登记请求（`POST /business/storage/complete`）。
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct StorageCompleteReq {
    /// 业务分类。
    #[validate(length(min = 1, max = 50, message = "分类长度 1-50"))]
    pub category: String,
    /// `presign` 阶段返回的对象键。
    #[validate(length(min = 1, max = 1024, message = "对象键长度 1-1024"))]
    pub file_key: String,
    /// 原始文件名。
    #[validate(length(min = 1, max = 255, message = "文件名长度 1-255"))]
    pub file_name: String,
    /// 客户端声明的字节数（后端以 `stat()` 复核）。
    pub file_size: u64,
    /// 内容 SHA-256（可选，给了就做完整性校验）。
    pub file_hash: Option<String>,
    /// MIME（可选）。
    pub mime_type: Option<String>,
}

/// 文件记录响应（新接口；字段是 `FileRes` 的超集，前端可复用类型）。
#[derive(Debug, Clone, Serialize)]
pub struct StorageFileRes {
    /// 文件 ID。
    pub id: String,
    /// 存储文件名（与 `file_key` 相同）。
    pub file_name: String,
    /// 原始文件名。
    pub original_name: String,
    /// 对象键。
    pub file_key: String,
    /// 字节数。
    pub file_size: i64,
    /// MIME。
    pub mime_type: String,
    /// 扩展名。
    pub extension: String,
    /// 分类。
    pub category: String,
    /// 状态（1 / 0）。
    pub status: i16,
    /// 上传人。
    pub uploaded_by: String,
    /// 上传时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 下载地址（新接口）。
    pub url: String,
    /// 所在后端编码（历史文件为空串，视为本地）。
    pub storage_code: String,
    /// 上传方式：`SERVER` / `DIRECT`。
    pub upload_mode: String,
}

impl StorageFileRes {
    /// 由模型构造响应（`url` 指向新下载接口）。
    ///
    /// # 参数
    ///
    /// - `model`：文件记录。
    ///
    /// # 返回值
    ///
    /// 文件响应（可空文本列降级为空串，与底座 `FileRes` 约定一致）。
    pub fn from_model(model: &StorageFileModel) -> Self {
        Self {
            id: model.id.to_string(),
            file_name: model.file_name.clone(),
            original_name: model.original_name.clone().unwrap_or_default(),
            file_key: model.file_key.clone(),
            file_size: model.file_size,
            mime_type: model.mime_type.clone().unwrap_or_default(),
            extension: model.extension.clone().unwrap_or_default(),
            category: model.category.clone().unwrap_or_default(),
            status: model.status.as_i16(),
            uploaded_by: model
                .uploaded_by
                .map(|id| id.to_string())
                .unwrap_or_default(),
            created_at: model.created_at,
            url: format!("/api/business/storage/download/{}", model.id),
            storage_code: model.storage_code.clone().unwrap_or_default(),
            upload_mode: model.upload_mode.clone(),
        }
    }
}

/// 文件分页查询参数。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageFilePageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 原始文件名（模糊）。
    pub original_name: Option<String>,
    /// 分类（精确）。
    pub category: Option<String>,
    /// 后端编码（精确）。
    pub storage_code: Option<String>,
    /// 内容哈希（精确，查秒传用）。
    pub file_hash: Option<String>,
}

/// 重新登记请求（`POST /business/storage/{id}/rebind`）。
///
/// 把一条（多为已软删的）记录的**同一个对象键**重新登记为新记录，
/// 供「秒传共享记录被他人删除」的业务自救（架构文档 §2.8 第 5 点）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageRebindReq {
    /// 覆盖原始文件名（不传则沿用原记录）。
    pub original_name: Option<String>,
    /// 覆盖分类（不传则沿用原记录）。
    pub category: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::constants::EnableStatus;

    /// 构造一条已配置密钥的 S3 后端配置。
    fn s3_config(key: &MasterKey) -> SysStorageConfigModel {
        let (ak_cipher, ak_nonce) =
            cipher::encrypt(key, "CPAKFixtureValue9f2c").expect("加密应成功");
        let (sk_cipher, sk_nonce) =
            cipher::encrypt(key, "PlainSecretKeyValueNeverLeak").expect("加密应成功");
        let now = chrono::Utc::now();
        SysStorageConfigModel {
            id: uuid::Uuid::now_v7(),
            config_code: "minio-a".to_string(),
            config_name: "MinIO 主桶".to_string(),
            backend_type: "S3".to_string(),
            endpoint: Some("http://minio.local:9000".to_string()),
            bucket: Some("mybucket".to_string()),
            region: "us-east-1".to_string(),
            base_path: String::new(),
            use_tls: false,
            path_style: true,
            ak_cipher: Some(ak_cipher),
            ak_nonce: Some(ak_nonce),
            sk_cipher: Some(sk_cipher),
            sk_nonce: Some(sk_nonce),
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
        }
    }

    fn test_key() -> MasterKey {
        MasterKey::parse("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .expect("测试主密钥应合法")
    }

    /// 安全（§5.4 `mask_ak_never_leaks`）：序列化后的响应体不含任何明文凭据。
    #[test]
    fn dto_masked_ak_never_leaks_plaintext() {
        let key = test_key();
        let model = s3_config(&key);
        let res = StorageConfigRes::from_model(&model, Some(&key));

        assert_eq!(res.ak_masked, "CPAK****9f2c");
        assert!(res.has_secret);

        let json = serde_json::to_string(&res).expect("序列化应成功");
        assert!(!json.contains("CPAKFixtureValue9f2c"), "{json}");
        assert!(!json.contains("PlainSecretKeyValueNeverLeak"), "{json}");
        // 响应里根本没有 ak / sk 字段
        assert!(!json.contains("\"ak\""), "{json}");
        assert!(!json.contains("\"sk\""), "{json}");
    }

    /// 边界：主密钥不匹配时掩码降级为 `****`，仍然不泄露、且不报错。
    #[test]
    fn dto_masked_ak_falls_back_when_key_mismatch() {
        let key = test_key();
        let other =
            MasterKey::parse("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
                .expect("测试主密钥应合法");
        let model = s3_config(&key);
        let res = StorageConfigRes::from_model(&model, Some(&other));
        assert_eq!(res.ak_masked, "****");
        assert!(res.has_secret);
        let json = serde_json::to_string(&res).expect("序列化应成功");
        assert!(!json.contains("CPAKFixtureValue9f2c"), "{json}");
    }

    /// 边界：本地后端没有凭据 → `has_secret = false`，掩码为 `****`。
    #[test]
    fn dto_local_backend_has_no_secret() {
        let mut model = s3_config(&test_key());
        model.backend_type = "LOCAL".to_string();
        model.endpoint = None;
        model.bucket = None;
        model.ak_cipher = None;
        model.ak_nonce = None;
        model.sk_cipher = None;
        model.sk_nonce = None;
        let res = StorageConfigRes::from_model(&model, None);
        assert!(!res.has_secret);
        assert_eq!(res.ak_masked, "****");
        assert_eq!(res.endpoint, "");
        assert_eq!(res.bucket, "");
    }

    /// 错误分支：只提供 AK 或只提供 SK 必须 400（不允许半个凭据）。
    #[test]
    fn dto_rejects_half_credentials() {
        let mut req = StorageConfigSaveReq {
            config_code: "minio-a".to_string(),
            config_name: "MinIO".to_string(),
            backend_type: "S3".to_string(),
            endpoint: Some("http://minio.local:9000".to_string()),
            bucket: Some("mybucket".to_string()),
            region: None,
            base_path: None,
            use_tls: None,
            path_style: None,
            ak: Some("only-ak".to_string()),
            sk: None,
            is_default: None,
            status: None,
            sort_order: None,
            remark: None,
        };
        assert!(req.credentials().is_err());

        req.ak = Some("  ".to_string());
        req.sk = Some("sk".to_string());
        assert!(req.credentials().is_err());

        req.ak = Some("ak".to_string());
        req.sk = Some("sk".to_string());
        assert_eq!(
            req.credentials().expect("应成功"),
            Some(("ak".to_string(), "sk".to_string()))
        );

        req.ak = None;
        req.sk = None;
        assert_eq!(req.credentials().expect("应成功"), None);
    }
}

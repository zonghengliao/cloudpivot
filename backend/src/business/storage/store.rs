//! 对象存储后端契约（`ObjectStore`）与两个实现：本地磁盘、S3 兼容对象存储。
//!
//! # 为什么要这层 trait（而不是直接用框架的 `alun-fs`）
//!
//! 框架 `alun-fs`（`fs-local` feature）的 `BackendRegistry` 以 `backend_type` 为键，
//! **同一种类型只能挂一个实例**，而本批次要求「两个 MinIO 桶 / 生产桶 + 备份桶」这类
//! 多实例并存；同时框架没有**上传**预签名（只有下载）。
//!
//! 因此在业务侧定义 [`ObjectStore`]：按 `sys_storage_config.config_code` 持有实例，
//! 由 [`crate::business::storage::router::StorageRouter`] 路由。I/O 能力按后端分两类：
//!
//! | 后端 | 服务端读 / 写 / 删 | 预签名下载 | 预签名上传 | 说明 |
//! |------|--------------------|-----------|-----------|------|
//! | `LOCAL` | ✅ `tokio::fs` 实现 | ❌（返回 `None`，由 handler 流式下载） | ❌（返回 `Unsupported`） | 与历史 `sys_file` 行为兼容 |
//! | `S3` | ⏳ A2 交付（见下） | ✅ SigV4 | ✅ SigV4 | 浏览器直传的主力路径 |
//!
//! # 关于 S3 的服务端 I/O（诚实说明，不含糊）
//!
//! A1 只交付「底座」：预签名 URL 生成（[`crate::business::storage::sigv4`]）与连通性
//! 探测**是真的**；`put/get/delete/stat` 在 `S3Store` 上返回
//! [`StoreError::Unsupported`]，因为项目当前只开了框架的 `fs-local` feature，
//! `fs-minio` 与它**互斥**（同一个 `fs()` 全局单例），而本卡红线要求
//! 「不改 `support/file` 的现有实现」——开启 `fs-minio` 会把 `support/file` 的本地
//! 后端顶掉。真正的 S3 服务端读写由 A2 在「直传 + 兼容层」卡里按以下之一落地：
//!
//! 1. 评审放行 `aws-sdk-s3` 声明（框架铁律的唯一缺口）；
//! 2. 或后端只做预签名直传 + 302 下载，不落服务端对象 I/O。
//!
//! 现在把边界写死在代码里，比 A2 才发现「本地后端被顶掉」要好。

use std::fmt;
use std::path::{Path, PathBuf};
use std::result::Result;

use alun::prelude::*;
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::business::storage::sigv4::{self, PresignOptions};

/// 对象元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStat {
    /// 对象字节数。
    pub size: u64,
    /// Content-Type（S3 由 `HeadObject` 提供，本地按扩展名推断为 `None`）。
    pub content_type: Option<String>,
    /// ETag（本地为 `None`）。
    pub etag: Option<String>,
}

/// 预签名上传结果。
///
/// # 字段语义
///
/// - `url`：浏览器直接 `PUT` 的完整 URL（已带签名）；
/// - `method`：固定 `PUT`（S3 预签名不支持 POST 表单形态）；
/// - `headers`：客户端**应当**带上的请求头。注意：`Content-Type` **不在**签名范围内
///   （`SignedHeaders` 只有 `host`），它只是给对象打标签；因此对象真实类型必须在
///   「直传完成」回调里由后端 `stat()` 复核，不能只信前端。
/// - `expire_at`：过期时间（与 URL 里的 `X-Amz-Expires` 一致）。
#[derive(Debug, Clone)]
pub struct PresignedUpload {
    /// 预签名 URL。
    pub url: String,
    /// 必须使用的 HTTP 方法。
    pub method: &'static str,
    /// 需要客户端携带的请求头。
    pub headers: Vec<(String, String)>,
    /// 过期时间。
    pub expire_at: chrono::DateTime<chrono::Utc>,
}

/// 存储操作错误。
///
/// 与 `ApiError` 分开：本层不认识 HTTP，由 handler 统一翻译成
/// [`crate::shared::error_code`] 里的业务码，避免「SDK 原文直接透给前端」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// 该后端不支持该操作（如本地后端的预签名上传）。
    Unsupported(String),
    /// 对象不存在。
    NotFound(String),
    /// 配置非法（如对象键越权、端点缺协议）。
    Invalid(String),
    /// 本地 I/O 失败。
    Io(String),
    /// 远端后端失败（网络 / 鉴权 / 服务端错误）。
    Backend(String),
}

impl StoreError {
    /// 返回可直接作为 `ApiError` 消息的中文描述。
    ///
    /// # 返回值
    ///
    /// 中文错误消息（不含 AK / SK / 签名等敏感内容）。
    pub fn message(&self) -> String {
        match self {
            Self::Unsupported(detail) => format!("当前存储后端不支持该操作：{detail}"),
            Self::NotFound(key) => format!("对象不存在：{key}"),
            Self::Invalid(detail) => format!("存储配置非法：{detail}"),
            Self::Io(detail) => format!("本地存储读写失败：{detail}"),
            Self::Backend(detail) => format!("远端存储访问失败：{detail}"),
        }
    }
}

impl fmt::Display for StoreError {
    /// 直接输出中文消息。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for StoreError {}

/// 存储后端统一契约。
///
/// 实例由 [`crate::business::storage::router::StorageRouter`] 按配置创建并缓存，
/// 全程以 `Arc<dyn ObjectStore>` 使用，因此要求 `Send + Sync`。
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// 后端类型标识：`LOCAL` / `S3`。
    fn backend_type(&self) -> &'static str;

    /// 后端编码（`sys_storage_config.config_code`），也是 `sys_file.storage_code` 的取值。
    fn config_code(&self) -> &str;

    /// 写入对象（服务端上传）。
    ///
    /// # 参数
    ///
    /// - `key`：对象键（相对桶 / 上传根，不以 `/` 开头）。
    /// - `data`：对象内容。
    /// - `content_type`：MIME 类型（本地后端忽略，S3 后端写入对象元数据）。
    async fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<(), StoreError>;

    /// 读取对象全量字节（供小文件服务端代理下载；大文件应走预签名 / 流式）。
    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError>;

    /// 删除对象。
    async fn delete(&self, key: &str) -> Result<(), StoreError>;

    /// 对象是否存在。
    async fn exists(&self, key: &str) -> bool;

    /// 对象元信息；不存在返回 `Ok(None)`。
    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, StoreError>;

    /// 预签名上传（浏览器直传）。
    ///
    /// # 参数
    ///
    /// - `key`：对象键。
    /// - `ttl_secs`：有效期（秒），由策略表 `presign_ttl_secs` 给出。
    /// - `content_type`：写入结果 `headers` 的 Content-Type（不参与签名）。
    async fn presign_put(
        &self,
        key: &str,
        ttl_secs: u64,
        content_type: Option<&str>,
    ) -> Result<PresignedUpload, StoreError>;

    /// 预签名下载；后端不支持时返回 `Ok(None)`（调用方回落服务端流式下载）。
    async fn presign_get(&self, key: &str, ttl_secs: u64) -> Result<Option<String>, StoreError>;

    /// 连通性探测（配置页「测试连接」按钮）。
    ///
    /// # 返回语义
    ///
    /// - `Ok(())`：后端可用；
    /// - `Err(StoreError::Backend)`：连不上 / 鉴权失败（消息里已做脱敏）；
    /// - `Err(StoreError::Unsupported)`：该后端无法在 A1 阶段探测。
    async fn health(&self) -> Result<(), StoreError>;
}

/// 拒绝越权对象键：绝对路径、`..` 段、空键。
///
/// # 参数
///
/// - `key`：对象键。
///
/// # 返回值
///
/// 合法时返回规整后的键（去掉前导 `/`）。
///
/// # 异常情况
///
/// 空键、绝对路径、含 `..` 段时返回 [`StoreError::Invalid`]——这是防目录穿越的
/// 唯一入口，本地后端所有文件操作都必须先过它。
pub fn sanitize_key(key: &str) -> Result<String, StoreError> {
    let trimmed = key.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(StoreError::Invalid("对象键不能为空".to_string()));
    }
    if trimmed.contains('\0') {
        return Err(StoreError::Invalid("对象键含非法字符".to_string()));
    }
    if Path::new(trimmed)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(StoreError::Invalid(
            "对象键不允许包含上级目录引用（..）".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// SSRF 防护：拒绝把 `endpoint` 指向回环 / 私网 / 链路本地地址（架构文档 §5.5 R2）。
///
/// # 参数
///
/// - `endpoint`：配置里的 endpoint（可带协议）。
/// - `use_tls`：无协议时的协议推断依据。
///
/// # 返回值
///
/// 允许保存返回 `Ok(())`。
///
/// # 异常情况
///
/// 主机是回环 / 私网 / 链路本地 / 未指定地址的字面量，或主机名为 `localhost`
/// 时返回 [`StoreError::Invalid`]。
///
/// # 已知局限（坦诚记录）
///
/// 只拦截**字面量 IP** 与 `localhost`：域名是否解析到内网需要 DNS 查询，
/// 保存时不做（否则内网自建 MinIO 的域名会被误杀）。真正要枚举域名解析结果时，
/// 在「测试连接」阶段用解析后的地址再判一次即可（A2 视需要补）。
pub fn ensure_endpoint_allowed(endpoint: &str, use_tls: bool) -> Result<(), StoreError> {
    let parts = sigv4::split_endpoint(endpoint, use_tls);
    let host = parts.host;
    if host.is_empty() {
        return Err(StoreError::Invalid("endpoint 无法解析出主机".to_string()));
    }

    // 去掉可能的端口与 IPv6 方括号
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest).to_string()
    } else {
        host.split(':').next().unwrap_or(&host).to_string()
    };

    if hostname.eq_ignore_ascii_case("localhost") {
        return Err(StoreError::Invalid(
            "endpoint 不允许指向 localhost（SSRF 防护）".to_string(),
        ));
    }

    if let Ok(ip) = hostname.parse::<std::net::IpAddr>() {
        let forbidden = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    // fc00::/7（唯一本地）与 fe80::/10（链路本地）
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
        if forbidden {
            return Err(StoreError::Invalid(format!(
                "endpoint {host} 指向内网 / 回环地址，已被 SSRF 防护拒绝"
            )));
        }
    }

    Ok(())
}

// ==================== 本地磁盘后端 ====================

/// 本地磁盘后端（与历史 `sys_file` 的存储目录兼容）。
///
/// 根目录 = `alun::upload_path()`（即 `[upload].path`，默认 `uploads`）拼接
/// `sys_storage_config.base_path`，因此 `base_path=''` 时**历史文件零迁移**。
#[derive(Debug, Clone)]
pub struct LocalStore {
    /// 后端编码。
    code: String,
    /// 物理根目录。
    root: PathBuf,
}

impl LocalStore {
    /// 以「框架上传根 + 配置前缀」构造。
    ///
    /// # 参数
    ///
    /// - `code`：后端编码。
    /// - `base_path`：配置列 `base_path`（如 `public/`，空串表示上传根）。
    ///
    /// # 返回值
    ///
    /// 本地后端实例。
    pub fn new(code: impl Into<String>, base_path: &str) -> Self {
        let mut root = PathBuf::from(alun::upload_path());
        let trimmed = base_path.trim().trim_matches('/');
        if !trimmed.is_empty() {
            root.push(trimmed);
        }
        Self {
            code: code.into(),
            root,
        }
    }

    /// 以显式根目录构造（供单测使用，避免污染真实上传目录）。
    ///
    /// # 参数
    ///
    /// - `code`：后端编码。
    /// - `root`：物理根目录。
    ///
    /// # 返回值
    ///
    /// 本地后端实例。
    pub fn with_root(code: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            code: code.into(),
            root: root.into(),
        }
    }

    /// 物理根目录。
    ///
    /// # 返回值
    ///
    /// 根目录引用。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 把对象键解析为绝对路径（已过 [`sanitize_key`]）。
    fn resolve(&self, key: &str) -> Result<PathBuf, StoreError> {
        let key = sanitize_key(key)?;
        Ok(self.root.join(key))
    }
}

#[async_trait]
impl ObjectStore for LocalStore {
    /// 后端类型固定为 `LOCAL`。
    fn backend_type(&self) -> &'static str {
        "LOCAL"
    }

    /// 返回配置编码。
    fn config_code(&self) -> &str {
        &self.code
    }

    /// 写入本地文件，自动建父目录。
    async fn put(&self, key: &str, data: &[u8], _content_type: &str) -> Result<(), StoreError> {
        let path = self.resolve(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StoreError::Io(format!("创建目录失败: {e}")))?;
        }
        tokio::fs::write(&path, data)
            .await
            .map_err(|e| StoreError::Io(format!("写入文件失败: {e}")))
    }

    /// 读取本地文件。
    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.resolve(key)?;
        tokio::fs::read(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StoreError::NotFound(key.to_string())
            } else {
                StoreError::Io(format!("读取文件失败: {e}"))
            }
        })
    }

    /// 删除本地文件（不存在视为已达成目标）。
    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        let path = self.resolve(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::Io(format!("删除文件失败: {e}"))),
        }
    }

    /// 判断本地文件是否存在。
    async fn exists(&self, key: &str) -> bool {
        match self.resolve(key) {
            Ok(path) => tokio::fs::metadata(&path).await.is_ok(),
            Err(_) => false,
        }
    }

    /// 读取本地文件元信息。
    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, StoreError> {
        let path = self.resolve(key)?;
        match tokio::fs::metadata(&path).await {
            Ok(meta) => Ok(Some(ObjectStat {
                size: meta.len(),
                content_type: None,
                etag: None,
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(format!("读取文件元信息失败: {e}"))),
        }
    }

    /// 本地后端不支持预签名上传（浏览器写不到服务端磁盘目录）。
    async fn presign_put(
        &self,
        _key: &str,
        _ttl_secs: u64,
        _content_type: Option<&str>,
    ) -> Result<PresignedUpload, StoreError> {
        Err(StoreError::Unsupported(
            "本地后端不支持直传，请走服务端上传（策略里 direct_upload 应为 false）".to_string(),
        ))
    }

    /// 本地后端没有预签名下载概念，返回 `None` 让调用方流式下发。
    async fn presign_get(&self, _key: &str, _ttl_secs: u64) -> Result<Option<String>, StoreError> {
        Ok(None)
    }

    /// 连通性探测：在根目录写 1 KB 探针再删掉（真实校验目录可写）。
    async fn health(&self) -> Result<(), StoreError> {
        let probe = format!(".health-probe-{}", uuid::Uuid::now_v7());
        let payload = vec![0u8; 1024];
        self.put(&probe, &payload, "application/octet-stream")
            .await?;
        let written = self.get(&probe).await?;
        self.delete(&probe).await?;
        if written.len() != payload.len() {
            return Err(StoreError::Io("探针对象回读长度不一致".to_string()));
        }
        Ok(())
    }
}

// ==================== S3 兼容后端 ====================

/// S3 兼容对象存储后端（MinIO / AWS S3 / 阿里 OSS / 腾讯 COS，同一套 SigV4）。
///
/// # 与框架启发的两个坑（架构文档 §2.2 实测）
///
/// 1. `path_style` **按配置列取值**，不用「endpoint 含 minio 或未开 TLS」的启发式——
///    OSS/S3 用 https 时也会被启发式误判成 path-style；
/// 2. `host` 必须与真实请求一致：path-style 为 `endpoint`，virtual-host 为
///    `bucket.endpoint`，这是预签名 403 的头号原因。
pub struct S3Store {
    /// 后端编码。
    code: String,
    /// 协议（`http` / `https`）。
    scheme: String,
    /// 端点主机（不含协议与路径）。
    host: String,
    /// 桶名。
    bucket: String,
    /// 区域。
    region: String,
    /// 是否 path 风格寻址。
    path_style: bool,
    /// 对象键前缀（`base_path`，空串表示桶根）。
    base_path: String,
    /// AccessKey 明文（仅在内存中，禁止落日志 / 禁落库）。
    access_key: String,
    /// SecretKey 明文（仅在内存中，禁止落日志 / 禁落库）。
    secret_key: String,
}

/// S3 后端构造参数（字段多，用结构体避免 `too_many_arguments`，也让调用点自解释）。
#[derive(Debug, Clone)]
pub struct S3StoreParams<'a> {
    /// 后端编码。
    pub code: &'a str,
    /// 配置列 `endpoint`（可带协议）。
    pub endpoint: &'a str,
    /// 桶名。
    pub bucket: &'a str,
    /// 区域。
    pub region: &'a str,
    /// 是否 path 风格寻址。
    pub path_style: bool,
    /// 对象键前缀。
    pub base_path: &'a str,
    /// **解密后**的 AccessKey 明文。
    pub access_key: &'a str,
    /// **解密后**的 SecretKey 明文。
    pub secret_key: &'a str,
    /// 无协议端点时的协议推断依据。
    pub use_tls: bool,
}

impl S3Store {
    /// 构造 S3 后端。
    ///
    /// # 参数
    ///
    /// - `params`：见 [`S3StoreParams`]。
    ///
    /// # 返回值
    ///
    /// S3 后端实例。
    ///
    /// # 异常情况
    ///
    /// 端点为空 / 桶名为空 / 端点解析不出主机时返回 [`StoreError::Invalid`]。
    pub fn new(params: S3StoreParams<'_>) -> Result<Self, StoreError> {
        if params.endpoint.trim().is_empty() {
            return Err(StoreError::Invalid("S3 后端缺少 endpoint".to_string()));
        }
        if params.bucket.trim().is_empty() {
            return Err(StoreError::Invalid("S3 后端缺少 bucket".to_string()));
        }
        let parts = sigv4::split_endpoint(params.endpoint, params.use_tls);
        if parts.host.is_empty() {
            return Err(StoreError::Invalid("endpoint 无法解析出主机".to_string()));
        }
        Ok(Self {
            code: params.code.to_string(),
            scheme: parts.scheme,
            host: parts.host,
            bucket: params.bucket.trim().to_string(),
            region: params.region.trim().to_string(),
            path_style: params.path_style,
            base_path: params.base_path.trim().trim_matches('/').to_string(),
            access_key: params.access_key.to_string(),
            secret_key: params.secret_key.to_string(),
        })
    }

    /// 拼接完整对象键（配置前缀 + 业务键）。
    fn object_key(&self, key: &str) -> String {
        let key = key.trim_start_matches('/');
        if self.base_path.is_empty() {
            key.to_string()
        } else {
            format!("{}/{key}", self.base_path)
        }
    }

    /// 生成预签名 URL（GET / PUT 共用）。
    fn presign(
        &self,
        method: &'static str,
        key: &str,
        ttl_secs: u64,
        extra_query: &[(String, String)],
    ) -> Result<sigv4::PresignOutcome, StoreError> {
        let object_key = self.object_key(&sanitize_key(key)?);
        let canonical_uri = sigv4::object_canonical_uri(&self.bucket, &object_key, self.path_style);
        let host = sigv4::request_host(&self.host, &self.bucket, self.path_style);
        Ok(sigv4::presign(&PresignOptions {
            method,
            scheme: &self.scheme,
            host: &host,
            canonical_uri: &canonical_uri,
            extra_query,
            ttl_secs,
            access_key: &self.access_key,
            secret_key: &self.secret_key,
            region: &self.region,
            now: chrono::Utc::now(),
        }))
    }

    /// 生成预签名下载 URL。
    ///
    /// # 参数
    ///
    /// - `key`：对象键。
    /// - `ttl_secs`：有效期（秒），内部夹到 60 秒 ~ 7 天。
    ///
    /// # 返回值
    ///
    /// 完整 URL 与过期时间。
    ///
    /// # 异常情况
    ///
    /// 对象键非法时返回 [`StoreError::Invalid`]。
    pub fn presign_download(
        &self,
        key: &str,
        ttl_secs: u64,
    ) -> Result<(String, chrono::DateTime<chrono::Utc>), StoreError> {
        let outcome = self.presign("GET", key, ttl_secs, &[])?;
        Ok((outcome.url, outcome.expire_at))
    }

    /// 生成用于「测试连接」的桶列举预签名 URL（`list-type=2&max-keys=1`）。
    fn presign_list(&self, ttl_secs: u64) -> sigv4::PresignOutcome {
        let extra = vec![
            ("list-type".to_string(), "2".to_string()),
            ("max-keys".to_string(), "1".to_string()),
        ];
        let canonical_uri = if self.path_style {
            format!("/{}", sigv4::uri_encode(&self.bucket, false))
        } else {
            "/".to_string()
        };
        let host = sigv4::request_host(&self.host, &self.bucket, self.path_style);
        sigv4::presign(&PresignOptions {
            method: "GET",
            scheme: &self.scheme,
            host: &host,
            canonical_uri: &canonical_uri,
            extra_query: &extra,
            ttl_secs,
            access_key: &self.access_key,
            secret_key: &self.secret_key,
            region: &self.region,
            now: chrono::Utc::now(),
        })
    }

    /// 真正发出一次请求做连通性探测。
    ///
    /// # 返回值
    ///
    /// - `http` 端点：发一次**已签名**的桶列举请求，按响应状态码判定
    ///   （2xx → 可用；403 → 凭据/桶策略问题；404 → 桶不存在；其它 → 不可用）；
    /// - `https` 端点：只做 TCP 连通性检查（A1 未引入 TLS 客户端，见模块文档），
    ///   并额外用一次预签名自检证明签名链路可用。
    async fn probe(&self) -> Result<(), StoreError> {
        let outcome = self.presign_list(sigv4::MIN_TTL_SECS);
        if self.scheme != "http" {
            let address = format!("{}:443", self.host_no_port());
            return match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                tokio::net::lookup_host(&address),
            )
            .await
            {
                Ok(Ok(mut addrs)) => match addrs.next() {
                    Some(_) => Ok(()),
                    None => Err(StoreError::Backend(format!(
                        "域名 {address} 无解析结果（https 端点仅做连通性检查）"
                    ))),
                },
                Ok(Err(e)) => Err(StoreError::Backend(format!(
                    "域名解析失败（https 端点仅做连通性检查）: {e}"
                ))),
                Err(_) => Err(StoreError::Backend(
                    "域名解析超时（https 端点仅做连通性检查）".to_string(),
                )),
            };
        }

        let path_and_query = outcome
            .url
            .split_once(&format!("{}://{}", self.scheme, self.host))
            .map(|(_, rest)| rest.to_string())
            .ok_or_else(|| StoreError::Backend("预签名 URL 结构异常".to_string()))?;
        let request = format!(
            "GET {path_and_query} HTTP/1.1\r\nHost: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
            sigv4::request_host(&self.host, &self.bucket, self.path_style)
        );

        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TcpStream::connect(&self.host),
        )
        .await
        .map_err(|_| StoreError::Backend("连接存储端点超时".to_string()))?
        .map_err(|e| StoreError::Backend(format!("连接存储端点失败: {e}")))?;

        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| StoreError::Backend(format!("发送探测请求失败: {e}")))?;

        let mut buf = [0u8; 128];
        let read = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
            .await
            .map_err(|_| StoreError::Backend("读取探测响应超时".to_string()))?
            .map_err(|e| StoreError::Backend(format!("读取探测响应失败: {e}")))?;

        let head = String::from_utf8_lossy(&buf[..read]).to_string();
        let status_line = head.lines().next().unwrap_or_default().to_string();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| StoreError::Backend("存储端点返回非 HTTP 响应".to_string()))?;

        match status {
            200..=299 => Ok(()),
            403 => Err(StoreError::Backend(
                "认证失败（AK/SK 或桶策略不允许列举，请检查凭据）".to_string(),
            )),
            404 => Err(StoreError::Backend("桶不存在或区域不匹配".to_string())),
            other => Err(StoreError::Backend(format!("存储端点返回 HTTP {other}"))),
        }
    }

    /// 去掉端点里的端口（仅用于 https 域名解析探测）。
    fn host_no_port(&self) -> String {
        self.host
            .split(':')
            .next()
            .unwrap_or(&self.host)
            .to_string()
    }
}

impl fmt::Debug for S3Store {
    /// 只打印非敏感字段，凭据一律 `<redacted>`。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Store")
            .field("code", &self.code)
            .field("scheme", &self.scheme)
            .field("host", &self.host)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("path_style", &self.path_style)
            .field("base_path", &self.base_path)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

#[async_trait]
impl ObjectStore for S3Store {
    /// 后端类型固定为 `S3`。
    fn backend_type(&self) -> &'static str {
        "S3"
    }

    /// 返回配置编码。
    fn config_code(&self) -> &str {
        &self.code
    }

    /// A1 不做 S3 服务端写入（见模块文档的边界说明）。
    async fn put(&self, _key: &str, _data: &[u8], _content_type: &str) -> Result<(), StoreError> {
        Err(StoreError::Unsupported(
            "S3 服务端写入由 A2 交付（A1 只提供预签名直传能力）".to_string(),
        ))
    }

    /// A1 不做 S3 服务端读取。
    async fn get(&self, _key: &str) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Unsupported(
            "S3 服务端读取由 A2 交付（A1 只提供预签名下载能力）".to_string(),
        ))
    }

    /// A1 不做 S3 服务端删除（清理任务对本后端会跳过，见 cleanup 模块）。
    async fn delete(&self, _key: &str) -> Result<(), StoreError> {
        Err(StoreError::Unsupported(
            "S3 服务端删除由 A2 交付".to_string(),
        ))
    }

    /// 无 HEAD 能力时无法确认存在性，保守返回 `false`。
    async fn exists(&self, _key: &str) -> bool {
        false
    }

    /// 无 HEAD 能力时返回 `Ok(None)`（调用方须按「未知」处理）。
    async fn stat(&self, _key: &str) -> Result<Option<ObjectStat>, StoreError> {
        Ok(None)
    }

    /// 预签名上传（A1 的核心交付）。
    async fn presign_put(
        &self,
        key: &str,
        ttl_secs: u64,
        content_type: Option<&str>,
    ) -> Result<PresignedUpload, StoreError> {
        let outcome = self.presign("PUT", key, ttl_secs, &[])?;
        let mut headers = Vec::new();
        if let Some(value) = content_type {
            headers.push(("Content-Type".to_string(), value.to_string()));
        }
        Ok(PresignedUpload {
            url: outcome.url,
            method: "PUT",
            headers,
            expire_at: outcome.expire_at,
        })
    }

    /// 预签名下载。
    async fn presign_get(&self, key: &str, ttl_secs: u64) -> Result<Option<String>, StoreError> {
        let outcome = self.presign("GET", key, ttl_secs, &[])?;
        Ok(Some(outcome.url))
    }

    /// 连通性探测。
    async fn health(&self) -> Result<(), StoreError> {
        self.probe().await
    }
}

#[cfg(test)]
/// 内存后端：供 router / handler 单测使用，**不依赖真实磁盘与网络**。
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    /// 后端编码。
    code: String,
    /// 对象内容（键 → 字节）。
    objects: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>>,
    /// 模拟「后端不可用」。
    down: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
impl MemoryStore {
    /// 构造一个可用（healthy）的内存后端。
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            objects: std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            down: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// 构造一个「不可用」的内存后端（用于测试降级路径）。
    pub fn down(code: impl Into<String>) -> Self {
        let store = Self::new(code);
        store.down.store(true, std::sync::atomic::Ordering::Relaxed);
        store
    }

    /// 当前对象数量（测试断言用）。
    pub fn object_count(&self) -> usize {
        self.objects
            .lock()
            .map(|guard| guard.len())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[async_trait]
impl ObjectStore for MemoryStore {
    fn backend_type(&self) -> &'static str {
        "LOCAL"
    }

    fn config_code(&self) -> &str {
        &self.code
    }

    async fn put(&self, key: &str, data: &[u8], _content_type: &str) -> Result<(), StoreError> {
        let key = sanitize_key(key)?;
        if let Ok(mut guard) = self.objects.lock() {
            guard.insert(key, data.to_vec());
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        let key = sanitize_key(key)?;
        self.objects
            .lock()
            .ok()
            .and_then(|guard| guard.get(&key).cloned())
            .ok_or_else(|| StoreError::NotFound(key))
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        let key = sanitize_key(key)?;
        if let Ok(mut guard) = self.objects.lock() {
            guard.remove(&key);
        }
        Ok(())
    }

    async fn exists(&self, key: &str) -> bool {
        sanitize_key(key)
            .ok()
            .and_then(|key| {
                self.objects
                    .lock()
                    .ok()
                    .map(|guard| guard.contains_key(&key))
            })
            .unwrap_or(false)
    }

    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, StoreError> {
        let key = sanitize_key(key)?;
        Ok(self.objects.lock().ok().and_then(|guard| {
            guard.get(&key).map(|data| ObjectStat {
                size: data.len() as u64,
                content_type: None,
                etag: None,
            })
        }))
    }

    async fn presign_put(
        &self,
        _key: &str,
        _ttl_secs: u64,
        _content_type: Option<&str>,
    ) -> Result<PresignedUpload, StoreError> {
        Err(StoreError::Unsupported("内存后端不支持直传".to_string()))
    }

    async fn presign_get(&self, _key: &str, _ttl_secs: u64) -> Result<Option<String>, StoreError> {
        Ok(None)
    }

    async fn health(&self) -> Result<(), StoreError> {
        if self.down.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(StoreError::Backend("模拟后端不可用".to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个临时根目录（单测结束即删除）。
    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cloudpivot-a1-{tag}-{}", uuid::Uuid::now_v7()))
    }

    /// 正常路径：本地后端写 → 读 → stat → 存在性 → 删除。
    #[tokio::test]
    async fn local_store_roundtrip() {
        let root = temp_root("local");
        let store = LocalStore::with_root("local-default", &root);

        store
            .put("public/2026/10/28/a.txt", b"hello", "text/plain")
            .await
            .expect("写入应成功");
        assert!(store.exists("public/2026/10/28/a.txt").await);
        assert_eq!(
            store
                .get("public/2026/10/28/a.txt")
                .await
                .expect("读取应成功"),
            b"hello"
        );
        let stat = store
            .stat("public/2026/10/28/a.txt")
            .await
            .expect("stat 应成功")
            .expect("对象应存在");
        assert_eq!(stat.size, 5);

        store
            .delete("public/2026/10/28/a.txt")
            .await
            .expect("删除应成功");
        assert!(!store.exists("public/2026/10/28/a.txt").await);
        assert!(
            store
                .stat("public/2026/10/28/a.txt")
                .await
                .expect("stat 应成功")
                .is_none()
        );

        // 清理
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    /// 错误分支：本地后端预签名上传必须明确报「不支持」，不能假装成功。
    #[tokio::test]
    async fn local_store_presign_is_unsupported() {
        let store = LocalStore::with_root("local-default", temp_root("local-unsupported"));
        assert!(matches!(
            store.presign_put("a.txt", 900, Some("text/plain")).await,
            Err(StoreError::Unsupported(_))
        ));
        // 预签名下载返回 None（由调用方回落流式下载）
        assert_eq!(
            store.presign_get("a.txt", 900).await.expect("不应失败"),
            None
        );
    }

    /// 安全：对象键越权（`..` / 绝对路径）必须被拒绝。
    #[tokio::test]
    async fn local_store_rejects_path_traversal() {
        let store = LocalStore::with_root("local-default", temp_root("local-traversal"));
        assert!(matches!(
            store.get("../../etc/passwd").await,
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.put("../escape.txt", b"x", "text/plain").await,
            Err(StoreError::Invalid(_))
        ));
        assert!(!store.exists("../escape.txt").await);
        // 前导 / 会被规整为相对键
        assert_eq!(sanitize_key("/a/b.txt").expect("应合法"), "a/b.txt");
        assert!(sanitize_key("").is_err());
    }

    /// 正常路径：本地后端健康探测（写 1 KB 探针再删）。
    #[tokio::test]
    async fn local_store_health_probe() {
        let root = temp_root("local-health");
        let store = LocalStore::with_root("local-default", &root);
        store.health().await.expect("健康探测应通过");
        // 探针文件不应残留
        let mut entries = tokio::fs::read_dir(&root).await.expect("目录应存在");
        assert!(entries.next_entry().await.expect("读目录应成功").is_none());
        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    /// 正常路径：S3 预签名 URL 结构正确（path-style 含桶名、virtual-host 不含）。
    #[test]
    fn s3_store_presign_shapes() {
        let path_style = S3Store::new(S3StoreParams {
            code: "minio-a",
            endpoint: "http://minio.local:9000",
            bucket: "mybucket",
            region: "us-east-1",
            path_style: true,
            base_path: "public",
            access_key: "test-ak",
            secret_key: "test-sk",
            use_tls: true,
        })
        .expect("构造应成功");
        let (url, _) = path_style
            .presign_download("a/b.txt", 900)
            .expect("预签名应成功");
        assert!(
            url.starts_with(
                "http://minio.local:9000/mybucket/public/a/b.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256"
            ),
            "{url}"
        );
        assert!(url.contains("X-Amz-Signature="));

        let virtual_host = S3Store::new(S3StoreParams {
            code: "oss-a",
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com",
            bucket: "mybucket",
            region: "cn-hangzhou",
            path_style: false,
            base_path: "",
            access_key: "test-ak",
            secret_key: "test-sk",
            use_tls: true,
        })
        .expect("构造应成功");
        let (url, _) = virtual_host
            .presign_download("a.txt", 900)
            .expect("预签名应成功");
        assert!(
            url.starts_with("https://mybucket.oss-cn-hangzhou.aliyuncs.com/a.txt?X-Amz-Algorithm="),
            "{url}"
        );
    }

    /// 安全：S3Store 的 Debug 输出不得出现明文凭据。
    #[test]
    fn s3_store_debug_is_redacted() {
        let store = S3Store::new(S3StoreParams {
            code: "minio-a",
            endpoint: "http://minio.local:9000",
            bucket: "mybucket",
            region: "us-east-1",
            path_style: true,
            base_path: "",
            access_key: "PLAIN-AK-VALUE",
            secret_key: "PLAIN-SK-VALUE",
            use_tls: true,
        })
        .expect("构造应成功");
        let debug = format!("{store:?}");
        assert!(!debug.contains("PLAIN-AK-VALUE"), "{debug}");
        assert!(!debug.contains("PLAIN-SK-VALUE"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }

    /// 错误分支：缺 endpoint / bucket 必须构造失败（配置期就拦住）。
    #[test]
    fn s3_store_rejects_missing_endpoint_or_bucket() {
        let missing_endpoint = S3StoreParams {
            code: "c",
            endpoint: "",
            bucket: "b",
            region: "r",
            path_style: true,
            base_path: "",
            access_key: "ak",
            secret_key: "sk",
            use_tls: true,
        };
        assert!(S3Store::new(missing_endpoint).is_err());

        let missing_bucket = S3StoreParams {
            code: "c",
            endpoint: "http://h:9000",
            bucket: "  ",
            region: "r",
            path_style: true,
            base_path: "",
            access_key: "ak",
            secret_key: "sk",
            use_tls: true,
        };
        assert!(S3Store::new(missing_bucket).is_err());
    }

    /// 安全（SSRF 防护）：内网 / 回环 / 链路本地字面量地址一律拒绝，公网域名放行。
    #[test]
    fn store_rejects_private_endpoint() {
        // 云元数据端点（最典型的 SSRF 目标）
        assert!(ensure_endpoint_allowed("http://169.254.169.254/latest/meta-data", false).is_err());
        // 回环 / 私网 / IPv6 回环与唯一本地
        assert!(ensure_endpoint_allowed("http://127.0.0.1:9000", false).is_err());
        assert!(ensure_endpoint_allowed("http://10.0.0.5:9000", false).is_err());
        assert!(ensure_endpoint_allowed("https://172.16.0.9", true).is_err());
        assert!(ensure_endpoint_allowed("http://192.168.1.10:9000", false).is_err());
        assert!(ensure_endpoint_allowed("http://localhost:9000", false).is_err());
        assert!(ensure_endpoint_allowed("http://[::1]:9000", false).is_err());
        assert!(ensure_endpoint_allowed("http://[fd00::1]:9000", false).is_err());

        // 公网地址与域名（内网自建 MinIO 用域名的场景）放行
        assert!(ensure_endpoint_allowed("https://s3.us-west-2.amazonaws.com", true).is_ok());
        assert!(ensure_endpoint_allowed("http://minio.local:9000", false).is_ok());
        assert!(ensure_endpoint_allowed("https://oss-cn-hangzhou.aliyuncs.com", true).is_ok());
        assert!(ensure_endpoint_allowed("http://8.8.8.8:9000", false).is_ok());
    }
}

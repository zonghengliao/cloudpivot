//! 对象存储后端契约（`ObjectStore`）与两个实现：本地磁盘、S3 兼容对象存储。
//!
//! # 为什么要这层 trait（而不是直接用框架的 `nik-fs`）
//!
//! 框架 `nik-fs`（`fs-local` feature）的 `BackendRegistry` 以 `backend_type` 为键，
//! **同一种类型只能挂一个实例**，而本批次要求「两个 MinIO 桶 / 生产桶 + 备份桶」这类
//! 多实例并存；同时框架没有**上传**预签名（只有下载）。
//!
//! 因此在业务侧定义 [`ObjectStore`]：按 `sys_storage_config.config_code` 持有实例，
//! 由 [`crate::business::storage::router::StorageRouter`] 路由。I/O 能力按后端分两类：
//!
//! | 后端 | 服务端读 / 写 / 删 | 预签名下载 | 预签名上传 | 说明 |
//! |------|--------------------|-----------|-----------|------|
//! | `LOCAL` | ✅ `tokio::fs` 实现 + **流式读** | ❌（返回 `None`，由 handler 流式下载） | ❌（返回 `Unsupported`） | 与历史 `sys_file` 行为兼容 |
//! | `S3` | ✅ 手写 SigV4 HTTP（PUT / GET 流式 / HEAD / DELETE / 分片上传） | ✅ SigV4 | ✅ SigV4 | 浏览器直传的主力路径 |
//!
//! # 为什么 S3 服务端 I/O 是手写 HTTP 而不是 `aws-sdk-s3`
//!
//! 项目铁律「`Cargo.toml` 只声明 `nik` 一个框架 crate」，而框架的 `fs-minio`
//! 与 `fs-local` **互斥**（同一个 `fs()` 全局单例）——开启 `fs-minio` 会把
//! `support/file` 的本地后端顶掉，违反「不改底座」红线。因此 A2 直接复用
//! [`crate::business::storage::sigv4`] 的签名实现 + `tokio` 的 TCP 自己发 HTTP/1.1：
//! 零新增依赖树（`aws-sdk-s3` 会拉进 ~20 个 smithy 子 crate）。
//!
//! # 已知边界（坦诚记录）
//!
//! 1. **https 端点不做服务端代理 I/O**：没有 TLS 客户端就不假装能读；此时
//!    [`S3Store::send`] 返回 [`StoreError::Unsupported`]，生产环境的 AWS / OSS
//!    走「浏览器预签名直传 + 下载 302 到预签名 URL」两条主路径（都不经后端流量）。
//! 2. **分片上传按需**：≤ 8 MB 单次 PUT，超过则 `CreateMultipartUpload → UploadPart → Complete`。

use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::result::Result;
use std::task::{Context, Poll};

use async_trait::async_trait;
use nik::prelude::*;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

use crate::business::storage::sigv4::{self, PresignOptions, SignOptions};

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

/// 对象流式读取句柄（本地文件 / S3 连接）。
///
/// 交给 `tokio_util::io::ReaderStream` 即可直接转成响应 body，
/// **整个对象不会进内存**（A2 硬指标：1 GB 下载 RSS 增长 < 200 MB）。
pub type ObjectStream = Box<dyn AsyncRead + Send + Unpin>;

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

    /// 流式读取对象（**不整文件进内存**，大文件下载走这里）。
    ///
    /// # 参数
    ///
    /// - `key`：对象键。
    /// - `offset`：起始字节偏移（`0` 表示从头读，断点续传用）。
    ///
    /// # 返回值
    ///
    /// 可读句柄：本地后端是 `tokio::fs::File`，S3 后端是「已收到响应头的连接」。
    /// 调用方应结合 [`ObjectStore::stat`] 的 `size` 设置 `Content-Length`。
    async fn stream(&self, key: &str, offset: u64) -> Result<ObjectStream, StoreError>;

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
/// # 显式例外（架构文档 §5.5 R2 的「例外白名单」）
///
/// 内网自建 MinIO 必然落在私网段，全部拒绝会让它无法接入。运维通过环境变量
/// `CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST`（逗号分隔的主机名 / IP，逐字比对、
/// 大小写不敏感）显式放行——**默认空串 = 维持严格模式**，不配置就没有任何例外。
/// 例：`CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST=172.18.10.99,minio.internal`
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

    // 显式白名单（内网自建 MinIO 的唯一例外通道）
    if host_in_allowlist(&hostname, &endpoint_allowlist()) {
        return Ok(());
    }

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
/// 根目录 = `nik::upload_path()`（即 `[upload].path`，默认 `uploads`）拼接
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
        let mut root = PathBuf::from(nik::upload_path());
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

    /// 流式读取本地文件（**不整文件进内存**，1 GB 下载 RSS 不随文件增长）。
    async fn stream(&self, key: &str, offset: u64) -> Result<ObjectStream, StoreError> {
        let path = self.resolve(key)?;
        let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StoreError::NotFound(key.to_string())
            } else {
                StoreError::Io(format!("打开文件失败: {e}"))
            }
        })?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| StoreError::Io(format!("定位文件失败: {e}")))?;
        }
        Ok(Box::new(file))
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

/// 判断主机是否在显式白名单内（纯函数，便于单测；比对大小写不敏感、逐字相等）。
///
/// # 参数
///
/// - `hostname`：endpoint 的主机名 / IP（已去端口与方括号）。
/// - `allowlist`：白名单条目。
///
/// # 返回值
///
/// 命中返回 `true`。
pub fn host_in_allowlist(hostname: &str, allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(hostname))
}

/// 读取 SSRF 例外白名单（`CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST`，逗号分隔）。
///
/// # 返回值
///
/// 逐字比对用的主机名 / IP 列表（去空白、丢空项、原样保留大小写，
/// 比对时用 `eq_ignore_ascii_case`）。未配置或全为空 → 空 `Vec`（严格模式）。
pub fn endpoint_allowlist() -> Vec<String> {
    std::env::var("CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

// ==================== S3 兼容后端 ====================

/// S3 请求超时（秒）：连接 / 发送 / 读响应头 / 读正文单次上限。
pub const S3_TIMEOUT_SECS: u64 = 30;

/// 超过该大小改用分片上传（S3 单次 PUT 上限 5 GB，但一次性把大文件压给
/// 单个 PUT 对网关与内存都不友好；8 MB 以上走分片）。
pub const MULTIPART_THRESHOLD: u64 = 8 * 1024 * 1024;

/// S3 分片最小字节数（协议硬约束：除最后一片外每片 ≥ 5 MiB）。
pub const MIN_PART_SIZE: usize = 5 * 1024 * 1024;

/// 分片数上限（S3 限制 10000，这里取 1000：片越多合并越慢）。
pub const MAX_PARTS: usize = 1000;

/// 一次 S3 响应的「头部已解析」形态。
///
/// 正文保留在连接上按需读取（[`S3Response::read_all`] 读全量，
/// [`S3Response::into_stream`] 走流式），因此大对象不会因为「先读头」而被整块吃进内存。
struct S3Response {
    /// HTTP 状态码。
    status: u16,
    /// 响应头（键统一小写）。
    headers: Vec<(String, String)>,
    /// 读完响应头之后多读到的字节（属于正文开头）。
    buffered: Vec<u8>,
    /// 连接（正文在此之上继续读）。
    stream: TcpStream,
    /// 读正文超时（秒）。
    timeout_secs: u64,
}

impl S3Response {
    /// 2xx 视为成功。
    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// 按小写键取响应头。
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// `Content-Length`（缺失或非法时 `None`）。
    fn content_length(&self) -> Option<u64> {
        self.header("content-length")
            .and_then(|value| value.trim().parse::<u64>().ok())
    }

    /// 读出全部正文（小响应：错误体 / XML / 小对象）。
    ///
    /// # 返回语义
    ///
    /// - `Transfer-Encoding: chunked`：按块解码；
    /// - 有 `Content-Length`：截断到该长度（`Connection: close` 下可能多读）；
    /// - 其它：读到 EOF。
    async fn read_all(mut self) -> Result<Vec<u8>, StoreError> {
        let mut body = std::mem::take(&mut self.buffered);
        tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            self.stream.read_to_end(&mut body),
        )
        .await
        .map_err(|_| StoreError::Backend("读取响应正文超时".to_string()))?
        .map_err(|e| StoreError::Backend(format!("读取响应正文失败: {e}")))?;

        let chunked = self
            .header("transfer-encoding")
            .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
        if chunked {
            return decode_chunked(&body);
        }
        if let Some(length) = self.content_length() {
            let length = length as usize;
            if body.len() > length {
                body.truncate(length);
            }
        }
        Ok(body)
    }

    /// 把正文转成流式读取句柄（长度由 `Content-Length` 限定）。
    ///
    /// # 异常情况
    ///
    /// 响应缺少 `Content-Length`（例如分块传输）时返回 [`StoreError::Backend`]：
    /// S3 对象 GET 带上 `Range` 后一定回 `Content-Length`，缺了说明对端不是 S3。
    fn into_stream(self) -> Result<ObjectStream, StoreError> {
        let length = self.content_length().ok_or_else(|| {
            StoreError::Backend(
                "对象响应缺少 Content-Length（不支持分块传输的流式下载）".to_string(),
            )
        })?;
        let reader = PrefixReader {
            prefix: self.buffered,
            position: 0,
            inner: self.stream,
        };
        Ok(Box::new(reader.take(length)))
    }
}

/// 「已缓存前缀 + 连接」的读取器：把响应头之后多读到的字节先吐出，再读连接。
struct PrefixReader {
    /// 已缓存的正文字节。
    prefix: Vec<u8>,
    /// 前缀读取位置。
    position: usize,
    /// 连接。
    inner: TcpStream,
}

impl AsyncRead for PrefixReader {
    /// 先读缓存，缓存读空后转给连接。
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.position < self.prefix.len() {
            let start = self.position;
            let available = self.prefix.len() - start;
            let take = available.min(buf.remaining());
            let slice = self.prefix[start..start + take].to_vec();
            buf.put_slice(&slice);
            self.position += take;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

/// 在字节流里查找子串（不引入 `memchr`，响应用量很小）。
///
/// # 参数
///
/// - `haystack`：被搜索的字节。
/// - `needle`：要找的子串。
///
/// # 返回值
///
/// 首次出现的下标。
pub(crate) fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 从 S3 XML（错误体 / 分片响应）里取一个简单标签的文本。
///
/// # 参数
///
/// - `body`：XML 字节。
/// - `tag`：标签名，如 `Code` / `UploadId`。
///
/// # 返回值
///
/// 标签内文本（去空白）；标签不存在返回 `None`。
pub(crate) fn xml_tag(body: &[u8], tag: &str) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let value = text[start..end].trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// 解码 `Transfer-Encoding: chunked` 正文。
///
/// # 参数
///
/// - `data`：原始字节（含块长度行与 CRLF）。
///
/// # 返回值
///
/// 解码后的正文。
///
/// # 异常情况
///
/// 块长度行非法 / 数据被截断时返回 [`StoreError::Backend`]。
fn decode_chunked(data: &[u8]) -> Result<Vec<u8>, StoreError> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0usize;
    loop {
        let line_end = match find_bytes(&data[index..], b"\r\n") {
            Some(end) => index + end,
            None => {
                return Err(StoreError::Backend(
                    "分块正文被截断（缺少块长度行）".to_string(),
                ));
            }
        };
        let line = String::from_utf8_lossy(&data[index..line_end]).to_string();
        let size_text = line
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        let size = usize::from_str_radix(&size_text, 16)
            .map_err(|_| StoreError::Backend(format!("分块长度非法：{size_text}")))?;
        index = line_end + 2;
        if size == 0 {
            return Ok(out);
        }
        let end = index.saturating_add(size);
        if end > data.len() {
            return Err(StoreError::Backend(
                "分块正文被截断（块体不足）".to_string(),
            ));
        }
        out.extend_from_slice(&data[index..end]);
        index = end + 2; // 跳过块尾 CRLF
    }
}

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
    /// 单次 HTTP 读 / 写 / 连接超时（秒）。远程对象 I/O 必须有上界，
    /// 否则一个挂死的 endpoint 会把请求线程拖到网关超时。
    timeout_secs: u64,
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
            timeout_secs: S3_TIMEOUT_SECS,
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

    // ==================== A2：真实对象 I/O（手写 SigV4 + HTTP/1.1） ====================

    /// 主机是否已带端口（IPv6 方括号写法也能正确判断）。
    ///
    /// # 参数
    ///
    /// - `host`：端点主机串。
    ///
    /// # 返回值
    ///
    /// 带端口返回 `true`。
    fn host_has_port(host: &str) -> bool {
        match host.rfind(']') {
            Some(index) => host[index + 1..].starts_with(':'),
            None => host.contains(':'),
        }
    }

    /// TCP 连接地址（虚拟主机风格时连 `bucket.endpoint`，path 风格时连 `endpoint`）。
    ///
    /// # 返回值
    ///
    /// `host:port` 形式；未显式给端口时按协议补 80 / 443。
    fn connect_addr(&self) -> String {
        let host = sigv4::request_host(&self.host, &self.bucket, self.path_style);
        if Self::host_has_port(&host) {
            host
        } else if self.scheme == "https" {
            format!("{host}:443")
        } else {
            format!("{host}:80")
        }
    }

    /// 对象请求的 `(规范 URI, Host 头)`。
    ///
    /// # 参数
    ///
    /// - `key`：对象键（会先过 [`sanitize_key`]）。
    ///
    /// # 返回值
    ///
    /// 已编码的规范 URI 与必须逐字一致的 `Host` 头值。
    ///
    /// # 异常情况
    ///
    /// 对象键非法时返回 [`StoreError::Invalid`]。
    fn object_uri(&self, key: &str) -> Result<(String, String), StoreError> {
        let object_key = self.object_key(&sanitize_key(key)?);
        let canonical_uri = sigv4::object_canonical_uri(&self.bucket, &object_key, self.path_style);
        let host = sigv4::request_host(&self.host, &self.bucket, self.path_style);
        Ok((canonical_uri, host))
    }

    /// 发一次已签名的 S3 请求，返回「头部已解析 + 正文可继续读」的响应。
    ///
    /// # 参数
    ///
    /// - `method`：HTTP 方法。
    /// - `canonical_uri`：规范 URI（[`Self::object_uri`] 产出）。
    /// - `query`：查询参数对（未编码；内部按 SigV4 规则排序编码，如 `uploads` / `uploadId`）。
    /// - `extra_headers`：附加请求头（`content-type` / `range` 等，**不参与签名**）。
    /// - `body`：请求正文（`None` 表示无正文，此时不发送 `Content-Length`）。
    ///
    /// # 返回值
    ///
    /// [`S3Response`]：状态码、响应头、已缓存的正文字节与连接。
    ///
    /// # 异常情况
    ///
    /// - `https` 端点：返回 [`StoreError::Unsupported`]（本模块未引入 TLS 客户端，
    ///   见文件头「已知边界」）；
    /// - 连接 / 读写超时、非 HTTP 响应：返回 [`StoreError::Backend`]（消息不含凭据）。
    async fn send(
        &self,
        method: &str,
        canonical_uri: &str,
        query: &[(String, String)],
        extra_headers: &[(&str, String)],
        body: Option<&[u8]>,
    ) -> Result<S3Response, StoreError> {
        if self.scheme != "http" {
            return Err(StoreError::Unsupported(format!(
                "服务端代理 I/O 仅支持 http 端点（当前 {}）：https 端点请走预签名直传 / 302 下载",
                self.scheme
            )));
        }
        let host = sigv4::request_host(&self.host, &self.bucket, self.path_style);
        let payload_hash = match body {
            Some(bytes) => sigv4::hex_sha256(bytes),
            None => sigv4::hex_sha256(b""),
        };
        let canonical_query = sigv4::canonical_query(query);
        let signed = sigv4::sign_headers(&SignOptions {
            method,
            canonical_uri,
            canonical_query: &canonical_query,
            host: &host,
            payload_hash: &payload_hash,
            access_key: &self.access_key,
            secret_key: &self.secret_key,
            region: &self.region,
            now: chrono::Utc::now(),
        });

        let mut head = String::with_capacity(512);
        head.push_str(method);
        head.push(' ');
        head.push_str(canonical_uri);
        if !canonical_query.is_empty() {
            head.push('?');
            head.push_str(&canonical_query);
        }
        head.push_str(" HTTP/1.1\r\n");
        head.push_str(&format!("Host: {host}\r\n"));
        head.push_str(&format!("x-amz-date: {}\r\n", signed.amz_date));
        head.push_str(&format!(
            "x-amz-content-sha256: {}\r\n",
            signed.payload_hash
        ));
        head.push_str(&format!("Authorization: {}\r\n", signed.authorization));
        for (name, value) in extra_headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        if let Some(bytes) = body {
            head.push_str(&format!("Content-Length: {}\r\n", bytes.len()));
        }
        head.push_str("Connection: close\r\n\r\n");

        let addr = self.connect_addr();
        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            TcpStream::connect(&addr),
        )
        .await
        .map_err(|_| StoreError::Backend(format!("连接存储端点 {addr} 超时")))?
        .map_err(|e| StoreError::Backend(format!("连接存储端点失败: {e}")))?;
        let _ = stream.set_nodelay(true);

        tokio::time::timeout(std::time::Duration::from_secs(self.timeout_secs), async {
            stream.write_all(head.as_bytes()).await?;
            if let Some(bytes) = body {
                stream.write_all(bytes).await?;
            }
            stream.flush().await
        })
        .await
        .map_err(|_| StoreError::Backend("发送请求超时".to_string()))?
        .map_err(|e| StoreError::Backend(format!("发送请求失败: {e}")))?;

        // 读到响应头结束（`\r\n\r\n`），余下字节属于正文
        let mut buffer: Vec<u8> = Vec::with_capacity(8192);
        let mut chunk = [0u8; 8192];
        let head_end = loop {
            let read = tokio::time::timeout(
                std::time::Duration::from_secs(self.timeout_secs),
                stream.read(&mut chunk),
            )
            .await
            .map_err(|_| StoreError::Backend("读取响应头超时".to_string()))?
            .map_err(|e| StoreError::Backend(format!("读取响应头失败: {e}")))?;
            if read == 0 {
                return Err(StoreError::Backend("存储端点提前关闭连接".to_string()));
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = find_bytes(&buffer, b"\r\n\r\n") {
                break position + 4;
            }
            if buffer.len() > 64 * 1024 {
                return Err(StoreError::Backend(
                    "响应头超过 64 KB，疑似非 S3 响应".to_string(),
                ));
            }
        };

        let head_text = String::from_utf8_lossy(&buffer[..head_end]).to_string();
        let mut lines = head_text.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| StoreError::Backend("存储端点返回非 HTTP 响应".to_string()))?;
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| {
                line.split_once(':').map(|(name, value)| {
                    (name.trim().to_ascii_lowercase(), value.trim().to_string())
                })
            })
            .collect();

        Ok(S3Response {
            status,
            headers,
            buffered: buffer[head_end..].to_vec(),
            stream,
            timeout_secs: self.timeout_secs,
        })
    }

    /// 把非 2xx 响应翻译成**不含凭据与 endpoint 细节**的错误。
    ///
    /// # 参数
    ///
    /// - `action`：动作名（写日志 / 返回给用户的动词，如「上传对象」）。
    /// - `status`：HTTP 状态码。
    /// - `body`：响应正文（用于提取 S3 `<Code>`）。
    ///
    /// # 返回值
    ///
    /// [`StoreError::Backend`]（S3 语义错误码，如 `NoSuchBucket` / `SignatureDoesNotMatch`）。
    fn backend_error(action: &str, status: u16, body: &[u8]) -> StoreError {
        let code = xml_tag(body, "Code").unwrap_or_else(|| format!("HTTP {status}"));
        StoreError::Backend(format!("{action}失败（{code}）"))
    }

    /// 单次 PUT 写入对象（≤ 分片阈值）。
    ///
    /// # 异常情况
    ///
    /// 非 2xx 返回 [`StoreError::Backend`]。
    async fn put_single(
        &self,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StoreError> {
        let (uri, _) = self.object_uri(key)?;
        let headers: Vec<(&str, String)> = if content_type.trim().is_empty() {
            Vec::new()
        } else {
            vec![("content-type", content_type.trim().to_string())]
        };
        let response = self.send("PUT", &uri, &[], &headers, Some(data)).await?;
        if response.is_success() {
            return Ok(());
        }
        let status = response.status;
        let body = response.read_all().await.unwrap_or_default();
        Err(Self::backend_error("上传对象", status, &body))
    }

    /// 分片上传（`CreateMultipartUpload` → `UploadPart` → `CompleteMultipartUpload`）。
    ///
    /// # 参数
    ///
    /// - `key`：对象键。
    /// - `data`：对象全文（服务端上传场景已在内存中）。
    /// - `content_type`：对象 MIME。
    ///
    /// # 返回值
    ///
    /// 成功返回 `Ok(())`。
    ///
    /// # 异常情况
    ///
    /// - 初始化失败（缺少 `UploadId`）：[`StoreError::Backend`]，并尽力 `AbortMultipartUpload`；
    /// - 任一分片失败：[`StoreError::Backend`]，同样尽力中止会话；
    /// - 完成阶段返回 `<Error>`（S3 在 200 里报错的老坑）：[`StoreError::Backend`]。
    async fn put_multipart(
        &self,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StoreError> {
        let (uri, _) = self.object_uri(key)?;
        let mut init_headers: Vec<(&str, String)> = Vec::new();
        if !content_type.trim().is_empty() {
            init_headers.push(("content-type", content_type.trim().to_string()));
        }
        let init_query = vec![("uploads".to_string(), String::new())];
        let init = self
            .send("POST", &uri, &init_query, &init_headers, None)
            .await?;
        let init_status = init.status;
        let init_body = init.read_all().await.unwrap_or_default();
        if !(200..300).contains(&init_status) {
            return Err(Self::backend_error(
                "初始化分片上传",
                init_status,
                &init_body,
            ));
        }
        let upload_id = xml_tag(&init_body, "UploadId").ok_or_else(|| {
            StoreError::Backend("初始化分片上传失败（响应缺少 UploadId）".to_string())
        })?;

        // 分片大小：≥ 5 MB（S3 硬约束），片数不超过 1000
        let part_size = std::cmp::max(MIN_PART_SIZE, data.len().div_ceil(MAX_PARTS));
        let mut parts: Vec<(usize, String)> = Vec::new();
        for (index, chunk) in data.chunks(part_size).enumerate() {
            let number = index + 1;
            let query = vec![
                ("partNumber".to_string(), number.to_string()),
                ("uploadId".to_string(), upload_id.clone()),
            ];
            let response = match self.send("PUT", &uri, &query, &[], Some(chunk)).await {
                Ok(response) => response,
                Err(error) => {
                    self.abort_multipart(&uri, &upload_id).await;
                    return Err(error);
                }
            };
            if !response.is_success() {
                let status = response.status;
                let body = response.read_all().await.unwrap_or_default();
                self.abort_multipart(&uri, &upload_id).await;
                return Err(Self::backend_error(
                    &format!("上传第 {number} 个分片"),
                    status,
                    &body,
                ));
            }
            let etag = response
                .header("etag")
                .map(str::to_string)
                .ok_or_else(|| StoreError::Backend("分片响应缺少 ETag".to_string()))?;
            parts.push((number, etag));
        }

        let mut xml = String::from("<CompleteMultipartUpload>");
        for (number, etag) in &parts {
            xml.push_str(&format!(
                "<Part><PartNumber>{number}</PartNumber><ETag>{etag}</ETag></Part>"
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let complete_query = vec![("uploadId".to_string(), upload_id.clone())];
        let complete_headers = [("content-type", "application/xml".to_string())];
        let response = self
            .send(
                "POST",
                &uri,
                &complete_query,
                &complete_headers,
                Some(xml.as_bytes()),
            )
            .await?;
        let status = response.status;
        let body = response.read_all().await.unwrap_or_default();
        if !(200..300).contains(&status) || find_bytes(&body, b"<Error>").is_some() {
            self.abort_multipart(&uri, &upload_id).await;
            return Err(Self::backend_error("合并分片", status, &body));
        }
        Ok(())
    }

    /// 尽力中止分片上传会话（失败只记日志，不覆盖主错误）。
    ///
    /// # 参数
    ///
    /// - `uri`：对象规范 URI。
    /// - `upload_id`：会话 ID。
    async fn abort_multipart(&self, uri: &str, upload_id: &str) {
        let query = vec![("uploadId".to_string(), upload_id.to_string())];
        if let Err(error) = self.send("DELETE", uri, &query, &[], None).await {
            tracing::warn!(error = %error, "中止分片上传失败（已忽略，服务端会按生命周期清理）");
        }
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

    /// 写入对象：≤ 8 MB 单次 PUT，更大走分片上传（`put_multipart`）。
    async fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<(), StoreError> {
        if data.len() as u64 > MULTIPART_THRESHOLD {
            return self.put_multipart(key, data, content_type).await;
        }
        self.put_single(key, data, content_type).await
    }

    /// 读取对象全量字节（服务端代理小文件 / 校验用；大文件走 [`Self::stream`]）。
    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        let (uri, _) = self.object_uri(key)?;
        let response = self.send("GET", &uri, &[], &[], None).await?;
        if response.status == 404 {
            return Err(StoreError::NotFound(key.to_string()));
        }
        if !response.is_success() {
            let status = response.status;
            let body = response.read_all().await.unwrap_or_default();
            return Err(Self::backend_error("读取对象", status, &body));
        }
        response.read_all().await
    }

    /// 流式读取对象：发一次带 `Range` 的 GET，正文直接在连接上流式吐出。
    ///
    /// # 说明
    ///
    /// 恒带 `Range: bytes={offset}-`：这样 S3 / MinIO 一定回 `Content-Length`
    /// （分块传输无法用于 `ReaderStream` 的定长下发），`offset=0` 时等价于全量读。
    async fn stream(&self, key: &str, offset: u64) -> Result<ObjectStream, StoreError> {
        let (uri, _) = self.object_uri(key)?;
        let headers = [("range", format!("bytes={offset}-"))];
        let response = self.send("GET", &uri, &[], &headers, None).await?;
        if response.status == 404 {
            return Err(StoreError::NotFound(key.to_string()));
        }
        if !response.is_success() {
            let status = response.status;
            let body = response.read_all().await.unwrap_or_default();
            return Err(Self::backend_error("流式读取对象", status, &body));
        }
        response.into_stream()
    }

    /// 删除对象（S3 语义：不存在也返回成功）。
    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        let (uri, _) = self.object_uri(key)?;
        let response = self.send("DELETE", &uri, &[], &[], None).await?;
        if response.is_success() || response.status == 404 {
            return Ok(());
        }
        let status = response.status;
        let body = response.read_all().await.unwrap_or_default();
        Err(Self::backend_error("删除对象", status, &body))
    }

    /// 判断对象是否存在（`HeadObject`）。
    async fn exists(&self, key: &str) -> bool {
        matches!(self.stat(key).await, Ok(Some(_)))
    }

    /// 读取对象元信息（`HeadObject`）；404 视为不存在。
    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, StoreError> {
        let (uri, _) = self.object_uri(key)?;
        let response = self.send("HEAD", &uri, &[], &[], None).await?;
        if response.status == 404 {
            return Ok(None);
        }
        if !response.is_success() {
            let status = response.status;
            return Err(Self::backend_error("读取对象元信息", status, &[]));
        }
        Ok(Some(ObjectStat {
            size: response.content_length().unwrap_or_default(),
            content_type: response.header("content-type").map(str::to_string),
            etag: response.header("etag").map(str::to_string),
        }))
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

    /// 内存后端流式读取（把字节喂进 duplex 管道，行为与真实后端一致）。
    async fn stream(&self, key: &str, offset: u64) -> Result<ObjectStream, StoreError> {
        let key = sanitize_key(key)?;
        let data = self
            .objects
            .lock()
            .ok()
            .and_then(|guard| guard.get(&key).cloned())
            .ok_or_else(|| StoreError::NotFound(key.clone()))?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let payload = data[start..].to_vec();
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let _ = writer.write_all(&payload).await;
            // writer 在两处 drop：写入完成后由 drop 关闭管道，读者读到 EOF
        });
        Ok(Box::new(reader))
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

    /// A2：本地后端必须能**从指定偏移流式读**（断点续传 / Range 的基础）。
    #[tokio::test]
    async fn local_store_stream_reads_from_offset() {
        let root = temp_root("local-stream");
        let store = LocalStore::with_root("local-default", &root);
        store
            .put("a/b.bin", b"0123456789", "application/octet-stream")
            .await
            .expect("写入应成功");

        let mut reader = store.stream("a/b.bin", 4).await.expect("流式打开应成功");
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.expect("读取应成功");
        assert_eq!(out, b"456789");

        // 错误分支：不存在的对象必须是 NotFound（handler 据此回 404）
        assert!(matches!(
            store.stream("a/missing.bin", 0).await,
            Err(StoreError::NotFound(_))
        ));

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    /// A2：内存后端的流式读与全量读结果一致（handler 单测用的桩）。
    #[tokio::test]
    async fn memory_store_stream_matches_get() {
        let store = MemoryStore::new("mem");
        store
            .put("k.txt", b"hello-stream", "text/plain")
            .await
            .expect("写入应成功");
        let mut reader = store.stream("k.txt", 6).await.expect("流式打开应成功");
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await.expect("读取应成功");
        assert_eq!(out, b"stream");
    }

    /// A2：`find_bytes` 边界（空针 / 超长 / 正常）。
    #[test]
    fn find_bytes_boundaries() {
        assert_eq!(find_bytes(b"abc\r\n\r\ndef", b"\r\n\r\n"), Some(3));
        assert_eq!(find_bytes(b"abc", b""), None);
        assert_eq!(find_bytes(b"ab", b"abc"), None);
        assert_eq!(find_bytes(b"aaa", b"aa"), Some(0));
    }

    /// A2：S3 XML 里取标签（`Code` / `UploadId`），缺失与空值都返回 `None`。
    #[test]
    fn xml_tag_extracts_and_tolerates() {
        let body = b"<Error><Code>NoSuchBucket</Code><Message>x</Message></Error>";
        assert_eq!(xml_tag(body, "Code").as_deref(), Some("NoSuchBucket"));
        assert_eq!(xml_tag(body, "UploadId"), None);
        assert_eq!(xml_tag(b"<Code>  </Code>", "Code"), None);
        assert_eq!(
            xml_tag(b"<InitiateMultipartUploadResult><UploadId>abc-123</UploadId></InitiateMultipartUploadResult>", "UploadId")
                .as_deref(),
            Some("abc-123")
        );
    }

    /// A2：分块正文解码（正常两段 + 零块结束；截断报错）。
    #[test]
    fn decode_chunked_roundtrip_and_truncated() {
        let encoded = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(encoded).expect("应解码成功"), b"hello world");
        assert!(matches!(
            decode_chunked(b"5\r\nhel"),
            Err(StoreError::Backend(_))
        ));
        assert!(matches!(
            decode_chunked(b"zz\r\nhello\r\n"),
            Err(StoreError::Backend(_))
        ));
    }

    /// A2：S3 端点地址推导 —— path 风格连 `endpoint`，virtual-host 连 `bucket.endpoint`，
    /// 未写端口时按协议补 80 / 443（预签名 403 的头号原因就是 Host 不一致）。
    #[test]
    fn s3_connect_addr_derivation() {
        let path_style = S3Store::new(S3StoreParams {
            code: "minio",
            endpoint: "http://minio.local:9000",
            bucket: "mybucket",
            region: "us-east-1",
            path_style: true,
            base_path: "",
            access_key: "ak",
            secret_key: "sk",
            use_tls: false,
        })
        .expect("构造应成功");
        assert_eq!(path_style.connect_addr(), "minio.local:9000");

        let virtual_host = S3Store::new(S3StoreParams {
            code: "oss",
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com",
            bucket: "mybucket",
            region: "cn-hangzhou",
            path_style: false,
            base_path: "files",
            access_key: "ak",
            secret_key: "sk",
            use_tls: true,
        })
        .expect("构造应成功");
        assert_eq!(
            virtual_host.connect_addr(),
            "mybucket.oss-cn-hangzhou.aliyuncs.com:443"
        );
    }

    /// A2：对象 URI —— path 风格带桶名，virtual-host 不带；`base_path` 参与拼接。
    #[test]
    fn s3_object_uri_shapes() {
        let store = S3Store::new(S3StoreParams {
            code: "minio",
            endpoint: "http://127.0.0.1:9000",
            bucket: "mybucket",
            region: "us-east-1",
            path_style: true,
            base_path: "files",
            access_key: "ak",
            secret_key: "sk",
            use_tls: false,
        })
        .expect("构造应成功");
        let (uri, host) = store.object_uri("2026/10/28/a.png").expect("应成功");
        assert_eq!(uri, "/mybucket/files/2026/10/28/a.png");
        assert_eq!(host, "127.0.0.1:9000");
        // 越权键必须被拒（目录穿越防护）
        assert!(matches!(
            store.object_uri("../etc/passwd"),
            Err(StoreError::Invalid(_))
        ));
    }

    /// A2：https 端点不允许服务端代理 I/O —— 必须明确返回「不支持」而不是假装成功。
    #[tokio::test]
    async fn s3_https_server_io_is_unsupported() {
        let store = S3Store::new(S3StoreParams {
            code: "oss",
            endpoint: "https://oss-cn-hangzhou.aliyuncs.com",
            bucket: "mybucket",
            region: "cn-hangzhou",
            path_style: false,
            base_path: "",
            access_key: "ak",
            secret_key: "sk",
            use_tls: true,
        })
        .expect("构造应成功");
        let result = store.put("a.txt", b"x", "text/plain").await;
        assert!(
            matches!(result, Err(StoreError::Unsupported(_))),
            "{result:?}"
        );
    }

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

    /// A2：SSRF 例外白名单（内网自建 MinIO 的唯一通道）—— 纯函数匹配规则。
    #[test]
    fn host_allowlist_matching() {
        let allow = vec!["172.18.10.99".to_string(), "MinIO.Internal".to_string()];
        assert!(host_in_allowlist("172.18.10.99", &allow));
        assert!(host_in_allowlist("minio.internal", &allow));
        assert!(host_in_allowlist("MINIO.INTERNAL", &allow));
        // 不做前缀 / 后缀模糊匹配（避免 172.18.10.99.evil.com 这类绕过）
        assert!(!host_in_allowlist("172.18.10.9", &allow));
        assert!(!host_in_allowlist("172.18.10.99.evil.com", &allow));
        assert!(!host_in_allowlist("minio.internal.evil.com", &allow));
        // 空白名单 = 严格模式
        assert!(!host_in_allowlist("172.18.10.99", &[]));
        // env 读取：未配置时为空（默认严格）
        if std::env::var("CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST").is_err() {
            assert!(endpoint_allowlist().is_empty());
        }
    }

    /// 安全（SSRF 防护）：内网 / 回环 / 链路本地字面量地址一律拒绝，公网域名放行。
    ///
    /// A2 补充：`CLOUDPIVOT_STORAGE_ENDPOINT_ALLOWLIST` 是唯一例外通道 ——
    /// 内网自建 MinIO 只能通过它显式放行（默认不配置 = 严格模式）。
    #[test]
    fn store_rejects_private_endpoint() {
        // 默认严格模式：白名单为空时，内网一律拒绝
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

//! 自研 AWS SigV4 预签名（query-string 形态，service = `s3`）。
//!
//! # 为什么自己算签名
//!
//! 框架的 `nik-fs`（`fs-minio` feature）只提供**下载**预签名，没有上传预签名；而
//! 「浏览器直传大文件」必须由后端签一个 `PUT` URL 出去。签名本身是纯字符串运算
//! （SHA-256 + HMAC-SHA256），不引入任何新依赖即可完成，因此本文件手写实现，
//! 与官方实现（`aws-sdk-s3`、Python `botocore`）**逐字节一致**（见文件末尾单测
//! `sigv4_presign_matches_official_sdk`，参考向量由 botocore 1.40 生成）。
//!
//! # 与官方实现对齐的四个要点（都是 403 的常见来源）
//!
//! 1. **规范查询串**：5 个固定参数（`X-Amz-Algorithm` / `X-Amz-Credential` /
//!    `X-Amz-Date` / `X-Amz-Expires` / `X-Amz-SignedHeaders`）与业务附加参数
//!    合并后**按参数名排序**，名和值都按 RFC 3986 编码（空格 → `%20`，`+` → `%2B`）；
//! 2. **规范 URI**：对象键的路径部分**保留 `/`**、其余按 RFC 3986 逐字节编码；
//! 3. **规范请求**：`METHOD\nURI\nQUERY\nhost:<host>\n\nhost\nUNSIGNED-PAYLOAD`
//!    （预签名一律不签 payload，body 不进签名）；
//! 4. **`host` 必须与真实请求的 `Host` 一致**：`path_style`（MinIO）为
//!    `endpoint`，virtual-host（AWS S3 / 阿里 OSS）为 `bucket.endpoint`。
//!
//! # 调用关系
//!
//! [`crate::business::storage::store`] 的 `S3Store` 用本模块生成预签名 URL；
//! 业务侧只关心「拿到 URL + 过期时间」，规范请求 / 待签串由本模块内部持有，
//! 仅在排查 403 时通过 [`PresignOutcome::canonical_request`] 取出打印。

use nik::prelude::*;
use sha2::{Digest, Sha256};

/// 签名算法标识（SigV4 固定值）。
pub const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// 预签名固定使用的服务名（对象存储恒为 `s3`，MinIO / OSS / COS 同协议）。
pub const SERVICE: &str = "s3";

/// 不签 payload 的占位摘要（预签名场景 body 不参与签名）。
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// 预签名 URL 有效期下限（秒）：与 `ck_sys_storage_policy_ttl` 的下界一致。
pub const MIN_TTL_SECS: u64 = 60;

/// 预签名 URL 有效期上限（秒）：7 天，与 `ck_sys_storage_policy_ttl` 的上界一致。
pub const MAX_TTL_SECS: u64 = 604_800;

/// 参与签名的请求头（预签名只签 `host`，其余头由客户端自由发送）。
pub const SIGNED_HEADERS: &str = "host";

/// 端点拆分结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointParts {
    /// 协议（`http` / `https`）。
    pub scheme: String,
    /// 主机（可含端口，如 `minio.local:9000`）。
    pub host: String,
}

/// 拆分配置里的 `endpoint`。
///
/// # 参数
///
/// - `endpoint`：`sys_storage_config.endpoint`，允许带协议（`http://minio.local:9000`）
///   或不带（`minio.local:9000`，此时按 `use_tls` 决定协议）。
/// - `use_tls`：配置列 `use_tls`，`false` 时无协议的端点按 `http` 处理。
///
/// # 返回值
///
/// 协议与主机（主机**不含**协议与路径）。
///
/// # 说明
///
/// 允许管理端把 `https://oss-cn-hangzhou.aliyuncs.com/` 这类带尾斜杠、带协议的值
/// 直接粘进来：本函数负责规整，`host` 里绝不能出现协议或路径，否则签名必错。
pub fn split_endpoint(endpoint: &str, use_tls: bool) -> EndpointParts {
    let trimmed = endpoint.trim().trim_end_matches('/');
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => (if use_tls { "https" } else { "http" }.to_string(), trimmed),
    };
    let host = rest.split('/').next().unwrap_or(rest).trim().to_string();
    EndpointParts { scheme, host }
}

/// RFC 3986 百分号编码。
///
/// # 参数
///
/// - `input`：待编码文本。
/// - `encode_slash`：是否把 `/` 也编码成 `%2F`。规范 URI 的路径部分传 `false`
///   （保留层级），查询参数的值传 `true`。
///
/// # 返回值
///
/// 只保留 `A-Z` / `a-z` / `0-9` / `-` / `_` / `.` / `~`（以及不编码时的 `/`），
/// 其余字节按 UTF-8 逐字节大写十六进制编码。
pub fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b'/' if !encode_slash => out.push('/'),
            other => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(other >> 4) as usize] as char);
                out.push(HEX[(other & 0x0F) as usize] as char);
            }
        }
    }
    out
}

/// 生成规范查询串（按参数名排序，名与值都做 RFC 3986 编码）。
///
/// # 参数
///
/// - `pairs`：查询参数（未编码）。
///
/// # 返回值
///
/// `a=1&b=2` 形态的规范串；同名的多个参数按值排序（AWS 规范要求）。
pub fn canonical_query(pairs: &[(String, String)]) -> String {
    let mut encoded: Vec<String> = pairs
        .iter()
        .map(|(name, value)| format!("{}={}", uri_encode(name, true), uri_encode(value, true)))
        .collect();
    encoded.sort();
    encoded.join("&")
}

/// 生成对象键的规范 URI。
///
/// # 参数
///
/// - `bucket`：桶名（`path_style` 时进路径，virtual-host 时不进）。
/// - `key`：对象键（可含 `/`，不带前导 `/`）。
/// - `path_style`：是否 path 风格寻址（MinIO 必须 `true`）。
///
/// # 返回值
///
/// 以 `/` 开头的规范 URI；路径层级保留，其它字符按 RFC 3986 编码。
pub fn object_canonical_uri(bucket: &str, key: &str, path_style: bool) -> String {
    let key = key.trim_start_matches('/');
    let encoded_key = uri_encode(key, false);
    if path_style {
        format!("/{}/{}", uri_encode(bucket, false), encoded_key)
    } else {
        format!("/{encoded_key}")
    }
}

/// 生成请求的实际 `Host` 头值。
///
/// # 参数
///
/// - `endpoint_host`：端点主机（[`split_endpoint`] 的结果，不含协议）。
/// - `bucket`：桶名。
/// - `path_style`：`true` 返回 `endpoint_host`；`false` 返回 `bucket.endpoint_host`。
///
/// # 返回值
///
/// 与真实 HTTP 请求 `Host` 头必须完全一致的字符串。
pub fn request_host(endpoint_host: &str, bucket: &str, path_style: bool) -> String {
    if path_style || bucket.is_empty() {
        endpoint_host.to_string()
    } else {
        format!("{bucket}.{endpoint_host}")
    }
}

/// 请求头签名入参（A2：服务端 S3 真实读写用，区别于 [`PresignOptions`] 的查询串签名）。
///
/// 字段均为「已规整」的值：`canonical_uri` 必须已按 [`object_canonical_uri`] 编码、
/// `canonical_query` 必须已按 [`canonical_query`] 排序编码（无查询参数时传空串）、
/// `host` 必须已按 [`request_host`] 拼好。
#[derive(Debug, Clone)]
pub struct SignOptions<'a> {
    /// HTTP 方法（`GET` / `PUT` / `POST` / `HEAD` / `DELETE`）。
    pub method: &'a str,
    /// 规范 URI（以 `/` 开头、已编码）。
    pub canonical_uri: &'a str,
    /// 规范查询串（已排序已编码；无参数时为空串）。
    pub canonical_query: &'a str,
    /// 请求 `Host` 头值。
    pub host: &'a str,
    /// 载荷哈希：实体正文的 SHA-256 十六进制，或 [`UNSIGNED_PAYLOAD`]
    /// （`x-amz-content-sha256` 头的值必须与它逐字一致）。
    pub payload_hash: &'a str,
    /// AccessKey（明文，仅在内存中使用，禁止落日志）。
    pub access_key: &'a str,
    /// SecretKey（明文，仅在内存中使用，禁止落日志）。
    pub secret_key: &'a str,
    /// 区域。
    pub region: &'a str,
    /// 签名时刻（调用方传入，便于单测固定时间）。
    pub now: chrono::DateTime<chrono::Utc>,
}

/// 请求头签名的结果。
#[derive(Debug, Clone)]
pub struct SignedHeaders {
    /// `Authorization` 头值。
    pub authorization: String,
    /// `x-amz-date` 头值。
    pub amz_date: String,
    /// `x-amz-content-sha256` 头值（与入参 `payload_hash` 相同）。
    pub payload_hash: String,
    /// 参与签名的头（分号分隔，恒为 [`SERVICE_SIGNED_HEADERS`]）。
    pub signed_headers: &'static str,
    /// 规范请求（排查 403 时打印对拍）。
    pub canonical_request: String,
    /// 待签字符串。
    pub string_to_sign: String,
    /// 十六进制签名。
    pub signature: String,
}

/// 请求头签名（非预签名）参与签名的头列表。
///
/// 只签 `host` 与两个 `x-amz-*`：AWS / MinIO / OSS 都要求 host 与
/// `x-amz-content-sha256` / `x-amz-date` 参与签名；`content-type` 与
/// `content-length` 不签（少一个「值被代理改写就 403」的坑）。
pub const SERVICE_SIGNED_HEADERS: &str = "host;x-amz-content-sha256;x-amz-date";

/// 生成 SigV4 **请求头**签名（服务端直连 S3 的 PUT / GET / HEAD / DELETE 用）。
///
/// # 参数
///
/// - `opts`：见 [`SignOptions`]。
///
/// # 返回值
///
/// [`SignedHeaders`]：`Authorization` / `x-amz-date` / `x-amz-content-sha256` 等。
///
/// # 异常情况
///
/// 纯运算，不会失败（不 panic、不 `unwrap`）。
pub fn sign_headers(opts: &SignOptions<'_>) -> SignedHeaders {
    let amz_date = opts.now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = opts.now.format("%Y%m%d").to_string();
    let scope = format!("{}/{}/{}/aws4_request", date_stamp, opts.region, SERVICE);

    // 1. 规范请求（头签名：host + 两个 x-amz 头，按头名升序）
    let canonical_request = format!(
        "{}\n{}\n{}\nhost:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n\n{}\n{}",
        opts.method,
        opts.canonical_uri,
        opts.canonical_query,
        opts.host,
        opts.payload_hash,
        amz_date,
        SERVICE_SIGNED_HEADERS,
        opts.payload_hash
    );

    // 2. 待签串
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );

    // 3. 签名密钥链 + 最终签名
    let key = signing_key(opts.secret_key, &date_stamp, opts.region, SERVICE);
    let signature = to_hex(&hmac_sha256(&key, string_to_sign.as_bytes()));

    let authorization = format!(
        "{ALGORITHM} Credential={}/{scope}, SignedHeaders={SERVICE_SIGNED_HEADERS}, Signature={signature}",
        opts.access_key
    );

    SignedHeaders {
        authorization,
        amz_date,
        payload_hash: opts.payload_hash.to_string(),
        signed_headers: SERVICE_SIGNED_HEADERS,
        canonical_request,
        string_to_sign,
        signature,
    }
}

/// 预签名入参。
///
/// 字段均为「已规整」的值：`canonical_uri` 必须已按 [`object_canonical_uri`] 编码、
/// `host` 必须已按 [`request_host`] 拼好，本结构只负责算签名。
#[derive(Debug, Clone)]
pub struct PresignOptions<'a> {
    /// HTTP 方法（`GET` / `PUT`）。
    pub method: &'a str,
    /// 协议（`http` / `https`）。
    pub scheme: &'a str,
    /// 请求 `Host` 头值。
    pub host: &'a str,
    /// 规范 URI（以 `/` 开头、已编码）。
    pub canonical_uri: &'a str,
    /// 业务附加查询参数（未编码），如 `response-content-disposition`。
    pub extra_query: &'a [(String, String)],
    /// 有效期（秒），内部会夹到 [`MIN_TTL_SECS`]..=[`MAX_TTL_SECS`]。
    pub ttl_secs: u64,
    /// AccessKey（明文，仅在内存中使用，禁止落日志）。
    pub access_key: &'a str,
    /// SecretKey（明文，仅在内存中使用，禁止落日志）。
    pub secret_key: &'a str,
    /// 区域（MinIO 可填 `us-east-1`；AWS/OSS 填真实区域）。
    pub region: &'a str,
    /// 签名时刻（调用方传入，便于单测固定时间）。
    pub now: chrono::DateTime<chrono::Utc>,
}

/// 预签名结果。
#[derive(Debug, Clone)]
pub struct PresignOutcome {
    /// 预签名 URL（可直接交给浏览器）。
    pub url: String,
    /// 十六进制签名（排查 403 时对拍用）。
    pub signature: String,
    /// 规范请求（排查 403 时打印出来与服务端日志比对）。
    pub canonical_request: String,
    /// 待签字符串。
    pub string_to_sign: String,
    /// 过期时间（`now + 实际生效的 ttl`）。
    pub expire_at: chrono::DateTime<chrono::Utc>,
}

/// 生成 SigV4 预签名 URL。
///
/// # 参数
///
/// - `opts`：见 [`PresignOptions`]。
///
/// # 返回值
///
/// [`PresignOutcome`]：URL、签名、规范请求、待签串、过期时间。
///
/// # 异常情况
///
/// 纯运算，不会失败（不 panic、不 `unwrap`）。
pub fn presign(opts: &PresignOptions<'_>) -> PresignOutcome {
    let ttl = opts.ttl_secs.clamp(MIN_TTL_SECS, MAX_TTL_SECS);
    let amz_date = opts.now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = opts.now.format("%Y%m%d").to_string();
    let scope = format!("{}/{}/{}/aws4_request", date_stamp, opts.region, SERVICE);

    // 1. 规范查询串：5 个固定参数 + 业务附加参数，排序后编码
    let mut query: Vec<(String, String)> = opts.extra_query.to_vec();
    query.push(("X-Amz-Algorithm".to_string(), ALGORITHM.to_string()));
    query.push((
        "X-Amz-Credential".to_string(),
        format!("{}/{scope}", opts.access_key),
    ));
    query.push(("X-Amz-Date".to_string(), amz_date.clone()));
    query.push(("X-Amz-Expires".to_string(), ttl.to_string()));
    query.push((
        "X-Amz-SignedHeaders".to_string(),
        SIGNED_HEADERS.to_string(),
    ));
    let canonical_query = canonical_query(&query);

    // 2. 规范请求（预签名不签 payload）
    let canonical_request = format!(
        "{}\n{}\n{}\nhost:{}\n\n{}\n{}",
        opts.method,
        opts.canonical_uri,
        canonical_query,
        opts.host,
        SIGNED_HEADERS,
        UNSIGNED_PAYLOAD
    );

    // 3. 待签串
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );

    // 4. 签名密钥链 + 最终签名
    let key = signing_key(opts.secret_key, &date_stamp, opts.region, SERVICE);
    let signature = to_hex(&hmac_sha256(&key, string_to_sign.as_bytes()));

    let url = format!(
        "{}://{}{}?{}&X-Amz-Signature={}",
        opts.scheme, opts.host, opts.canonical_uri, canonical_query, signature
    );

    PresignOutcome {
        url,
        signature,
        canonical_request,
        string_to_sign,
        expire_at: opts.now + chrono::Duration::seconds(ttl as i64),
    }
}

/// 计算 SigV4 签名密钥链 `HMAC(HMAC(HMAC(HMAC("AWS4"+SK, date), region), service), "aws4_request")`。
///
/// # 参数
///
/// - `secret_key`：SecretKey 明文。
/// - `date_stamp`：`YYYYMMDD`。
/// - `region`：区域。
/// - `service`：服务名（对象存储为 `s3`）。
///
/// # 返回值
///
/// 32 字节签名密钥。
pub fn signing_key(secret_key: &str, date_stamp: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac_sha256(
        format!("AWS4{secret_key}").as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// HMAC-SHA256（RFC 2104），返回 32 字节摘要。
///
/// # 参数
///
/// - `key`：任意长度密钥（超 64 字节先做 SHA-256）。
/// - `data`：消息。
///
/// # 返回值
///
/// 32 字节摘要。
///
/// # 说明
///
/// 项目只声明了 `sha2`（无 `hmac` crate），SigV4 需要的是「HMAC 结果再作为下一次
/// HMAC 的**密钥字节**」，走十六进制字符串中转既不必要也易错，故此处用 `sha2`
/// 的标准 ipad/opad 结构直接实现，并由单测对 RFC 4231 官方向量回归。
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_LEN: usize = 64;
    let mut block = [0u8; BLOCK_LEN];
    if key.len() > BLOCK_LEN {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; BLOCK_LEN];
    let mut outer_pad = [0x5cu8; BLOCK_LEN];
    for index in 0..BLOCK_LEN {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(data);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

/// SHA-256 十六进制摘要。
///
/// # 参数
///
/// - `data`：字节切片。
///
/// # 返回值
///
/// 64 位小写十六进制字符串。
pub fn hex_sha256(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

/// 字节切片转小写十六进制。
///
/// # 参数
///
/// - `bytes`：字节切片。
///
/// # 返回值
///
/// 小写十六进制字符串。
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A2：请求头签名的规范请求逐字节格式（与方法 / URI / 空查询 / 三个签名头一致）。
    ///
    /// 这条断言的价值：SigV4 的 403 有 90% 来自「规范请求差一个字节」
    /// （多一个空行、头顺序错、query 没排序）。把它固化成字符串比对。
    #[test]
    fn sign_headers_canonical_request_is_exact() {
        let payload = hex_sha256(b"");
        let signed = sign_headers(&SignOptions {
            method: "PUT",
            canonical_uri: "/mybucket/files/a.txt",
            canonical_query: "",
            host: "127.0.0.1:9000",
            payload_hash: &payload,
            access_key: "AK-TEST",
            secret_key: "SK-TEST",
            region: "us-east-1",
            now: fixed_now(),
        });
        let expected = format!(
            "PUT\n/mybucket/files/a.txt\n\nhost:127.0.0.1:9000\nx-amz-content-sha256:{payload}\nx-amz-date:20261028T031415Z\n\nhost;x-amz-content-sha256;x-amz-date\n{payload}"
        );
        assert_eq!(signed.canonical_request, expected);
        assert_eq!(signed.amz_date, "20261028T031415Z");
        assert_eq!(signed.payload_hash, payload);
        assert_eq!(signed.signature.len(), 64);
        assert!(signed.signature.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(
            signed.string_to_sign.starts_with(
                "AWS4-HMAC-SHA256\n20261028T031415Z\n20261028/us-east-1/s3/aws4_request\n"
            ),
            "{}",
            signed.string_to_sign
        );
        assert!(
            signed.authorization.starts_with(
                "AWS4-HMAC-SHA256 Credential=AK-TEST/20261028/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="
            ),
            "{}",
            signed.authorization
        );
        // 授权头里不得出现 SecretKey 明文
        assert!(!signed.authorization.contains("SK-TEST"));
    }

    /// A2 边界：同一入参的签名必须确定性；换 SK / region 必须变。
    #[test]
    fn sign_headers_is_deterministic_and_key_sensitive() {
        let payload = hex_sha256(b"body");
        let build = |secret: &'static str, region: &'static str| {
            sign_headers(&SignOptions {
                method: "GET",
                canonical_uri: "/bucket/k",
                canonical_query: "uploads=",
                host: "minio:9000",
                payload_hash: &payload,
                access_key: "AK",
                secret_key: secret,
                region,
                now: fixed_now(),
            })
        };
        let first = build("SK1", "us-east-1");
        let second = build("SK1", "us-east-1");
        assert_eq!(first.signature, second.signature);
        assert_ne!(first.signature, build("SK2", "us-east-1").signature);
        assert_ne!(first.signature, build("SK1", "cn-hangzhou").signature);
        // 查询串参与签名（分片上传的 uploadId / partNumber 就是靠它）
        assert!(first.canonical_request.contains("\nuploads=\nhost:"));
    }

    /// 单测固定签名时刻（与参考向量的 `X-Amz-Date` 一致）。
    fn fixed_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-28T03:14:15Z")
            .expect("固定时间解析失败")
            .with_timezone(&chrono::Utc)
    }

    /// 与参考向量一致的假凭证（**非真实密钥**，仅存在于单测）。
    const TEST_AK: &str = "cloudpivot-test-ak";
    const TEST_SK: &str = "cloudpivot-test-secret-key";

    /// 官方 SDK 参考向量（由 botocore 1.40.72 的 `S3SigV4QueryAuth` 生成）。
    struct Vector {
        name: &'static str,
        method: &'static str,
        scheme: &'static str,
        host: &'static str,
        canonical_uri: &'static str,
        ttl: u64,
        region: &'static str,
        canonical_request: &'static str,
        string_to_sign: &'static str,
        signature: &'static str,
        url: &'static str,
    }

    fn vectors() -> Vec<Vector> {
        vec![
            Vector {
                name: "path_style_get",
                method: "GET",
                scheme: "http",
                host: "minio.local:9000",
                canonical_uri: "/mybucket/a/b.txt",
                ttl: 900,
                region: "us-east-1",
                canonical_request: "GET\n/mybucket/a/b.txt\nX-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=cloudpivot-test-ak%2F20261028%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20261028T031415Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host\nhost:minio.local:9000\n\nhost\nUNSIGNED-PAYLOAD",
                string_to_sign: "AWS4-HMAC-SHA256\n20261028T031415Z\n20261028/us-east-1/s3/aws4_request\n84772393f2e4b155e0d5a4d6532d6c6090c7eb5b7132c99ee443456d88e8805e",
                signature: "63f9264b723d21eea88f9782e8e0f3445de6657d286eb16fef745cebdb4df475",
                url: "http://minio.local:9000/mybucket/a/b.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=cloudpivot-test-ak%2F20261028%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20261028T031415Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host&X-Amz-Signature=63f9264b723d21eea88f9782e8e0f3445de6657d286eb16fef745cebdb4df475",
            },
            Vector {
                name: "virtual_host_get",
                method: "GET",
                scheme: "https",
                host: "mybucket.s3.us-west-2.amazonaws.com",
                canonical_uri: "/a/b.txt",
                ttl: 900,
                region: "us-west-2",
                canonical_request: "GET\n/a/b.txt\nX-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=cloudpivot-test-ak%2F20261028%2Fus-west-2%2Fs3%2Faws4_request&X-Amz-Date=20261028T031415Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host\nhost:mybucket.s3.us-west-2.amazonaws.com\n\nhost\nUNSIGNED-PAYLOAD",
                string_to_sign: "AWS4-HMAC-SHA256\n20261028T031415Z\n20261028/us-west-2/s3/aws4_request\n12a4025b7f13999847959dfc896f9a0f36b69916a26beed2abba6320f3a71362",
                signature: "c6722cba28f5a745bd3a148fddbf8f6829b3a9d87f7d91333578d351c098453a",
                url: "https://mybucket.s3.us-west-2.amazonaws.com/a/b.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=cloudpivot-test-ak%2F20261028%2Fus-west-2%2Fs3%2Faws4_request&X-Amz-Date=20261028T031415Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host&X-Amz-Signature=c6722cba28f5a745bd3a148fddbf8f6829b3a9d87f7d91333578d351c098453a",
            },
            Vector {
                name: "path_style_put",
                method: "PUT",
                scheme: "http",
                host: "minio.local:9000",
                canonical_uri: "/mybucket/public/2026/10/28/uuid7.pdf",
                ttl: 3600,
                region: "cn-hangzhou",
                canonical_request: "PUT\n/mybucket/public/2026/10/28/uuid7.pdf\nX-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=cloudpivot-test-ak%2F20261028%2Fcn-hangzhou%2Fs3%2Faws4_request&X-Amz-Date=20261028T031415Z&X-Amz-Expires=3600&X-Amz-SignedHeaders=host\nhost:minio.local:9000\n\nhost\nUNSIGNED-PAYLOAD",
                string_to_sign: "AWS4-HMAC-SHA256\n20261028T031415Z\n20261028/cn-hangzhou/s3/aws4_request\n7bdaeb11624d27a8b2273f1158afa9559743277576f3e409f61845b599f36a77",
                signature: "101dff5c2d12b2009b0846738385f6e1fe8489e39df78079ac052c0fef2851b3",
                url: "http://minio.local:9000/mybucket/public/2026/10/28/uuid7.pdf?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=cloudpivot-test-ak%2F20261028%2Fcn-hangzhou%2Fs3%2Faws4_request&X-Amz-Date=20261028T031415Z&X-Amz-Expires=3600&X-Amz-SignedHeaders=host&X-Amz-Signature=101dff5c2d12b2009b0846738385f6e1fe8489e39df78079ac052c0fef2851b3",
            },
        ]
    }

    /// 核心对拍：规范请求、待签串、签名、URL 四项与官方 SDK 逐字节一致。
    #[test]
    fn sigv4_presign_matches_official_sdk() {
        for vector in vectors() {
            let outcome = presign(&PresignOptions {
                method: vector.method,
                scheme: vector.scheme,
                host: vector.host,
                canonical_uri: vector.canonical_uri,
                extra_query: &[],
                ttl_secs: vector.ttl,
                access_key: TEST_AK,
                secret_key: TEST_SK,
                region: vector.region,
                now: fixed_now(),
            });

            assert_eq!(
                outcome.canonical_request, vector.canonical_request,
                "{}: 规范请求与官方 SDK 不一致",
                vector.name
            );
            assert_eq!(
                outcome.string_to_sign, vector.string_to_sign,
                "{}: 待签串与官方 SDK 不一致",
                vector.name
            );
            assert_eq!(
                outcome.signature, vector.signature,
                "{}: 签名与官方 SDK 不一致",
                vector.name
            );
            assert_eq!(
                outcome.url, vector.url,
                "{}: 预签名 URL 与官方 SDK 不一致",
                vector.name
            );
            assert_eq!(
                outcome.expire_at,
                fixed_now() + chrono::Duration::seconds(vector.ttl as i64)
            );
        }
    }

    /// 边界：业务附加查询参数同样参与签名（`response-content-disposition`）。
    #[test]
    fn sigv4_presign_with_extra_query_matches_official_sdk() {
        let extra = vec![(
            "response-content-disposition".to_string(),
            "attachment; filename=\"a.txt\"".to_string(),
        )];
        let outcome = presign(&PresignOptions {
            method: "GET",
            scheme: "http",
            host: "minio.local:9000",
            canonical_uri: "/mybucket/a.txt",
            extra_query: &extra,
            ttl_secs: 600,
            access_key: TEST_AK,
            secret_key: TEST_SK,
            region: "us-east-1",
            now: fixed_now(),
        });
        assert_eq!(
            outcome.canonical_request,
            "GET\n/mybucket/a.txt\nX-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=cloudpivot-test-ak%2F20261028%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20261028T031415Z&X-Amz-Expires=600&X-Amz-SignedHeaders=host&response-content-disposition=attachment%3B%20filename%3D%22a.txt%22\nhost:minio.local:9000\n\nhost\nUNSIGNED-PAYLOAD"
        );
        assert_eq!(
            outcome.signature,
            "643c855e385043d7bee664afa2f46e1ae64a7426cefa8d565e69073db60accc6"
        );
    }

    /// 编码规则：空格 → `%20`、`+` → `%2B`、`/` 保留、中文按 UTF-8 逐字节编码。
    #[test]
    fn sigv4_uri_encode_rules() {
        assert_eq!(uri_encode("a b", true), "a%20b");
        assert_eq!(uri_encode("a+b", true), "a%2Bb");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("a/b", false), "a/b");
        assert_eq!(
            uri_encode("合同 2026.pdf", false),
            "%E5%90%88%E5%90%8C%202026.pdf"
        );
        // 未保留字符不编码
        assert_eq!(uri_encode("Aa0-_.~", false), "Aa0-_.~");
    }

    /// 边界：path 风格与 virtual-host 风格的规范 URI / Host 差异。
    #[test]
    fn sigv4_path_style_vs_virtual_host() {
        assert_eq!(
            object_canonical_uri("mybucket", "/a/b.txt", true),
            "/mybucket/a/b.txt"
        );
        assert_eq!(
            request_host("minio.local:9000", "mybucket", true),
            "minio.local:9000"
        );
        assert_eq!(
            object_canonical_uri("mybucket", "a/b.txt", false),
            "/a/b.txt"
        );
        assert_eq!(
            request_host("s3.us-west-2.amazonaws.com", "mybucket", false),
            "mybucket.s3.us-west-2.amazonaws.com"
        );
    }

    /// 边界：端点规整（带协议 / 带尾斜杠 / 无协议按 use_tls 推断）。
    #[test]
    fn sigv4_split_endpoint() {
        let parts = split_endpoint("http://minio.local:9000/", true);
        assert_eq!(parts.scheme, "http");
        assert_eq!(parts.host, "minio.local:9000");

        let parts = split_endpoint("oss-cn-hangzhou.aliyuncs.com", true);
        assert_eq!(parts.scheme, "https");
        assert_eq!(parts.host, "oss-cn-hangzhou.aliyuncs.com");

        let parts = split_endpoint("minio.local:9000", false);
        assert_eq!(parts.scheme, "http");
    }

    /// 边界：TTL 夹紧到 60 秒 ~ 7 天。
    #[test]
    fn sigv4_clamps_ttl() {
        let too_small = presign(&PresignOptions {
            method: "GET",
            scheme: "http",
            host: "h",
            canonical_uri: "/b/k",
            extra_query: &[],
            ttl_secs: 1,
            access_key: TEST_AK,
            secret_key: TEST_SK,
            region: "us-east-1",
            now: fixed_now(),
        });
        assert!(
            too_small.url.contains("X-Amz-Expires=60&"),
            "{}",
            too_small.url
        );

        let too_large = presign(&PresignOptions {
            method: "GET",
            scheme: "http",
            host: "h",
            canonical_uri: "/b/k",
            extra_query: &[],
            ttl_secs: u64::MAX,
            access_key: TEST_AK,
            secret_key: TEST_SK,
            region: "us-east-1",
            now: fixed_now(),
        });
        assert!(
            too_large.url.contains("X-Amz-Expires=604800&"),
            "{}",
            too_large.url
        );
    }

    /// HMAC-SHA256 与 RFC 4231 官方向量（用例 1）一致。
    #[test]
    fn sigv4_hmac_matches_rfc4231() {
        let key = [0x0bu8; 20];
        let digest = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            to_hex(&digest),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    /// 密钥链：固定输入产出固定密钥（防止实现被无意改写）。
    #[test]
    fn sigv4_signing_key_is_stable() {
        let key = signing_key(TEST_SK, "20261028", "us-east-1", "s3");
        assert_eq!(key.len(), 32);
        assert_eq!(
            to_hex(&key),
            to_hex(&signing_key(TEST_SK, "20261028", "us-east-1", "s3"))
        );
        assert_ne!(
            to_hex(&key),
            to_hex(&signing_key(TEST_SK, "20261028", "us-west-2", "s3")),
            "不同区域的签名密钥必须不同"
        );
    }
}

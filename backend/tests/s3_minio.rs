//! S3 后端真实读写集成测试（**需要真实 S3 兼容服务**：MinIO / AWS S3 / OSS）。
//!
//! # 为什么在 `tests/` 而不是单测
//!
//! 单测必须「零外部依赖」，而这条链路的价值恰恰在于**真的签对 SigV4、真的发 HTTP、
//! 真的被服务端接受**。所以本文件走集成测试，且：
//!
//! - **无环境变量时直接 return**（不是 `#[ignore]`，`cargo test` 仍会跑它并秒过）；
//! - 有环境变量时跑完整的 PUT / GET / HEAD(stat) / 流式 GET / 分片上传 / DELETE。
//!
//! # 用法
//!
//! ```bash
//! export CLOUDPIVOT_MINIO_ENDPOINT=http://127.0.0.1:9000
//! export CLOUDPIVOT_MINIO_BUCKET=cloudpivot-test
//! export CLOUDPIVOT_MINIO_AK=...        # 仅测试进程内可见
//! export CLOUDPIVOT_MINIO_SK=...
//! cargo test --test s3_minio -- --nocapture
//! ```
//!
//! 缺任何一个必要变量 → 打印 `SKIP` 并返回（CI 无外部依赖，不会红）。

use cloudpivot::business::storage::store::{ObjectStore, S3Store, S3StoreParams};
use tokio::io::AsyncReadExt;

/// 读取环境变量（缺失时返回 `None`）。
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// 生成唯一后缀（不引入额外依赖：纳秒时间戳 + 进程内计数器）。
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{seq}")
}

/// 计算字节的 SHA-256（断言内容一致性）。
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 构造直连 MinIO 的后端实例（path-style，http）。
fn minio_store() -> Option<S3Store> {
    let endpoint = env("CLOUDPIVOT_MINIO_ENDPOINT")?;
    let bucket = env("CLOUDPIVOT_MINIO_BUCKET")?;
    let access_key = env("CLOUDPIVOT_MINIO_AK")?;
    let secret_key = env("CLOUDPIVOT_MINIO_SK")?;
    let store = S3Store::new(S3StoreParams {
        code: "minio-it",
        endpoint: &endpoint,
        bucket: &bucket,
        region: &env("CLOUDPIVOT_MINIO_REGION").unwrap_or_else(|| "us-east-1".to_string()),
        path_style: true,
        base_path: "it",
        access_key: &access_key,
        secret_key: &secret_key,
        use_tls: false,
    })
    .expect("MinIO 后端构造应成功");
    Some(store)
}

/// S3 后端：PUT → GET → stat → exists → 流式读（含偏移）→ DELETE 全链路。
#[tokio::test]
async fn s3_minio_real_roundtrip() {
    let Some(store) = minio_store() else {
        println!("SKIP s3_minio_real_roundtrip（未配置 CLOUDPIVOT_MINIO_* 环境变量）");
        return;
    };
    let key = format!("it/roundtrip-{}.txt", unique_suffix());
    let payload = b"cloudpivot a2 s3 real io";

    store
        .put(&key, payload, "text/plain; charset=utf-8")
        .await
        .expect("PUT 应成功");

    let fetched = store.get(&key).await.expect("GET 应成功");
    assert_eq!(fetched, payload);
    assert_eq!(sha256_hex(&fetched), sha256_hex(payload));

    let stat = store.stat(&key).await.expect("HEAD 应成功").expect("对象应存在");
    assert_eq!(stat.size, payload.len() as u64);
    assert!(stat.etag.is_some(), "HEAD 应返回 ETag");
    assert!(store.exists(&key).await);

    // 流式读 + 偏移（Range）
    let mut reader = store.stream(&key, 10).await.expect("流式 GET 应成功");
    let mut tail = Vec::new();
    reader.read_to_end(&mut tail).await.expect("流式读取应成功");
    assert_eq!(tail, &payload[10..]);

    // DELETE 之后必须真的不存在（这是 A1 遗留的「Unsupported」缺口）
    store.delete(&key).await.expect("DELETE 应成功");
    assert!(!store.exists(&key).await, "DELETE 后对象不应存在");
    assert!(
        store.stat(&key).await.expect("HEAD 应成功").is_none(),
        "DELETE 后 HEAD 应 404"
    );
    // 重复删除幂等
    store.delete(&key).await.expect("重复 DELETE 应成功");
    println!("OK s3_minio_real_roundtrip key={key} sha256={}", sha256_hex(payload));
}

/// S3 分片上传（≥ 8 MB 走 mutipart）：内容哈希必须逐字节一致。
#[tokio::test]
async fn s3_minio_multipart_upload_matches() {
    let Some(store) = minio_store() else {
        println!("SKIP s3_minio_multipart_upload_matches（未配置 CLOUDPIVOT_MINIO_* 环境变量）");
        return;
    };
    // 12 MB：超过 8 MB 阈值、需要 3 个分片（5 MB + 5 MB + 2 MB）
    let size = 12 * 1024 * 1024;
    let payload: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
    let expected = sha256_hex(&payload);
    let key = format!("it/multipart-{}.bin", unique_suffix());

    store
        .put(&key, &payload, "application/octet-stream")
        .await
        .expect("分片 PUT 应成功");

    let stat = store.stat(&key).await.expect("HEAD 应成功").expect("对象应存在");
    assert_eq!(stat.size, size as u64);

    let fetched = store.get(&key).await.expect("GET 应成功");
    assert_eq!(fetched.len(), size);
    assert_eq!(sha256_hex(&fetched), expected);

    store.delete(&key).await.expect("DELETE 应成功");
    println!("OK s3_minio_multipart_upload_matches size={size} sha256={expected}");
}

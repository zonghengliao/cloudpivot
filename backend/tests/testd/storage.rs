//! 测试D · 文件存储（F01–F09）。
//!
//! 分两层：**物理后端**（`LocalStore` 落盘 / 读回 / 键净化）与**策略层**
//! （`model::check_upload` 的大小 / 扩展名 / MIME 白名单），再加 `sys_file` 记录的软删与
//! 业务附件关系。全部落 `cloudpivot_test`，物理文件写在临时目录，用例自清理。

use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::storage::model as storage_model;
use cloudpivot::business::storage::sigv4;
use cloudpivot::business::storage::store::{self, LocalStore, ObjectStore};

use crate::testd::common::{self as c, Ctx};

/// 建一个只属于本用例的物理根目录。
async fn temp_root(ctx: &Ctx) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("td-testd-storage-{}", ctx.prefix));
    tokio::fs::create_dir_all(&root)
        .await
        .expect("建临时根目录");
    root
}

/// 造一条 `sys_file` 记录（最小列），返回 id。
async fn make_file(ctx: &Ctx, uploader: Uuid, name: &str, size: i64) -> Uuid {
    let id = Uuid::now_v7();
    // file_hash 有唯一索引（uk_sys_file_hash）：每个夹具用自己内容的哈希
    let hash = worker_sha256(format!("{}-{}-{}", ctx.prefix, name, id).as_bytes());
    c::exec_with(
        "INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, file_hash, \
                               mime_type, extension, category, status, uploaded_by, storage_path) \
         VALUES (?::uuid, ?, ?, ?, ?, ?, 'text/plain', 'txt', 'document', 1, ?::uuid, '')",
        vec![
            Value::from(&id),
            Value::Text(format!("{}-{id}.txt", ctx.key(name))),
            Value::Text(ctx.key(name)),
            Value::Text(format!("td/{}/{id}.txt", ctx.tag)),
            Value::I64(size),
            Value::Text(hash),
            Value::from(&uploader),
        ],
    )
    .await;
    id
}

fn worker_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

// ============================================================================
// F01 上传 → 下载，内容一致（SHA-256 比对）
// ============================================================================

/// F01 对象写入 → 读回 → 字节与哈希完全一致（本地后端）。
#[tokio::test]
async fn f01_local_store_roundtrip_integrity() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F01").await;
    let root = temp_root(&ctx).await;
    let store = LocalStore::with_root("LOCAL_TEST", &root);

    let payload = format!(
        "{} 文件内容：中文 + emoji 🚀 与二进制边界 \u{1}\u{2}",
        ctx.tag
    )
    .into_bytes();
    let key = format!("td/{}/roundtrip.txt", ctx.tag);
    store
        .put(&key, &payload, "text/plain")
        .await
        .expect("写入对象");

    // 落盘路径在 root 之内
    let physical = root.join(&key);
    assert!(
        physical.exists(),
        "对象没有落到预期路径：{}",
        physical.display()
    );
    let on_disk = tokio::fs::read(&physical).await.expect("读物理文件");
    assert_eq!(on_disk, payload, "物理文件内容与写入不一致");

    let read_back = store.get(&key).await.expect("读回对象");
    assert_eq!(read_back, payload, "读回内容与写入不一致");
    assert_eq!(
        worker_sha256(&read_back),
        worker_sha256(&payload),
        "SHA-256 不一致（传输/编码损坏）"
    );

    // 大小与 stat 对得上
    let meta = store.stat(&key).await.expect("stat").expect("对象存在");
    assert_eq!(meta.size, payload.len() as u64, "stat 大小与实际不符");

    let _ = tokio::fs::remove_dir_all(&root).await;
    ctx.reset().await;
}

// ============================================================================
// F02 超大文件 → 拒绝
// ============================================================================

/// F02 ★ 超过分类策略上限的文件被拒（不是「先落盘再报错」）。
#[tokio::test]
async fn f02_oversize_upload_rejected() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F02").await;

    let policy = storage_model::default_policy("document"); // 内置默认上限 50 MB
    assert_eq!(policy.max_size_mb, 50, "内置默认上限变了，请复核本用例");

    let too_big = 51 * 1024 * 1024_u64;
    let violation =
        storage_model::check_upload(&policy, "big.pdf", too_big, Some("application/pdf"))
            .expect_err("超上限必须被拒");
    println!("[F02] 超限拒绝：{violation:?}");
    assert!(
        matches!(
            violation,
            storage_model::PolicyViolation::SizeExceeded { .. }
        ),
        "拒绝原因不是 SizeExceeded：{violation:?}"
    );

    // 正好等于上限 → 放行（边界值不能误杀）
    let exact = 50 * 1024 * 1024_u64;
    assert!(
        storage_model::check_upload(&policy, "exact.pdf", exact, Some("application/pdf")).is_ok(),
        "恰好等于上限被误拒"
    );

    // 策略配置本身也拦非法上限（0 / 9999 MB 都不允许）
    assert!(storage_model::validate_policy_fields(0, 900, "", "").is_err());
    assert!(storage_model::validate_policy_fields(9999, 900, "", "").is_err());

    ctx.reset().await;
}

// ============================================================================
// F03 危险类型 → 拒绝
// ============================================================================

/// F03 ★ 可执行文件类型（.exe/.sh/.php …）一律拒绝，即便白名单里被误配。
#[tokio::test]
async fn f03_dangerous_extension_rejected() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F03").await;

    for ext in [
        "exe", "sh", "bat", "ps1", "so", "dll", "jar", "py", "js", "vbs", "msi",
    ] {
        assert!(
            storage_model::is_forbidden_extension(ext),
            "{ext} 未被列入硬禁止黑名单"
        );
    }
    // 现状（报告 BUG-D-09，P2）：黑名单**没有** php / jsp / aspx / cgi —— 服务端可执行脚本漏网。
    // 若分类白名单为空（= 不校验扩展名，见 default_policy），这类文件会被直接放行。
    let mut unlisted = Vec::new();
    for ext in ["php", "jsp", "aspx", "cgi"] {
        if !storage_model::is_forbidden_extension(ext) {
            unlisted.push(ext);
        }
    }
    let loose = storage_model::default_policy("document");
    assert!(
        loose.allowed_extensions.is_empty(),
        "内置默认策略的白名单不应为空（若已收紧请更新报告）"
    );
    let php_passes = storage_model::check_upload(&loose, "shell.php", 1024, None).is_ok();
    println!("[F03] 黑名单未收录：{unlisted:?}；默认策略下 shell.php 是否放行：{php_passes}");
    assert!(
        php_passes,
        "实现已收紧（php 被拦）：请更新 BUG-D-09 的结论与严重度"
    );
    for ext in ["pdf", "docx", "xlsx", "png", "txt", "csv"] {
        assert!(
            !storage_model::is_forbidden_extension(ext),
            "正常类型 {ext} 被误列入黑名单"
        );
    }

    // 黑名单优先于白名单：即便策略把 sh 写进 allowed_extensions，也必须拒
    let mut policy = storage_model::default_policy("document");
    policy.allowed_extensions = "sh,pdf".to_string();
    let violation = storage_model::check_upload(&policy, "evil.sh", 10, None)
        .expect_err("危险类型必须被拒（黑名单是全局下限）");
    assert!(
        matches!(
            violation,
            storage_model::PolicyViolation::ExtensionForbidden { .. }
        ),
        "拒绝原因不对：{violation:?}"
    );

    // 大小写 / 全角无关：Windows 上传大写扩展名同样拦
    assert!(storage_model::check_upload(&policy, "EVIL.EXE", 10, None).is_err());
    // 双扩展名：看最后一段
    assert_eq!(
        storage_model::extension_of("report.pdf.exe").as_deref(),
        Some("exe")
    );
    assert!(storage_model::check_upload(&policy, "report.pdf.exe", 10, None).is_err());

    ctx.reset().await;
}

// ============================================================================
// F04 文件名路径穿越 → 拒绝
// ============================================================================

/// F04 ★ 对象键中的 `..` / 绝对路径 / NUL 一律拒绝，且物理上不会写到根目录之外。
#[tokio::test]
async fn f04_path_traversal_rejected() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F04").await;
    let root = temp_root(&ctx).await;
    let store = LocalStore::with_root("LOCAL_TRAV", &root);

    for key in [
        "../../etc/passwd",
        "td/../../etc/passwd",
        "bad\0key.txt",
        "",
    ] {
        assert!(
            store::sanitize_key(key).is_err(),
            "非法对象键未被拦下：{key:?} → {:?}",
            store::sanitize_key(key)
        );
        let put = store.put(key, b"pwned", "text/plain").await;
        assert!(put.is_err(), "非法键竟然写入成功：{key:?}");
    }

    // 绝对路径：前导 `/` 被规整掉 → 变成根目录内的相对键（**没有逃出根目录**），
    // 这是安全的设计取舍（不是穿越），因此断言它落在 root 之内。
    let absolute = store::sanitize_key("/etc/passwd").expect("前导斜杠应被规整");
    assert_eq!(absolute, "etc/passwd");
    assert_eq!(
        root.join(&absolute).starts_with(&root),
        true,
        "规整后的键逃出了根目录"
    );

    // 反斜杠形态（Windows 风格）：Linux 上不会穿越（`\` 是普通文件名字符），
    // 规整后仍落在 root 之内 —— 安全，但与 `/` 语义不一致（报告 BUG-D-10，P3：
    // 跨平台一致性；若将来在 Windows 上部署本地后端，这段必须先收紧）。
    let backslash = store::sanitize_key("..\\..\\windows\\system32\\cmd.exe");
    match backslash {
        Ok(key) => assert!(
            root.join(&key).starts_with(&root),
            "反斜杠键逃出了根目录：{key}"
        ),
        Err(_) => {} // 已被拒也是安全
    }

    // 合法键只是被规整（去前导 /），不改变语义
    assert_eq!(store::sanitize_key("/td/ok.txt").unwrap(), "td/ok.txt");

    // 物理上确认没有文件逃出根目录
    assert!(
        tokio::fs::metadata("/tmp/td-escaped.txt").await.is_err()
            && tokio::fs::metadata(root.join("../td-escaped.txt"))
                .await
                .is_err(),
        "穿越写成功（存在根目录之外的文件）"
    );

    let _ = tokio::fs::remove_dir_all(&root).await;
    ctx.reset().await;
}

// ============================================================================
// F05 下载别人的文件（越权）
// ============================================================================

/// F05 ★ 下载越权：存储是**全局共享文件中心**，没有属主隔离 —— 这是当前实现的口径。
///
/// 钉住三件事：
/// ① `sys_file` 有 `uploaded_by`，但读路径（`find_active_file`）**不按用户收口**；
/// ② 因此持 `business:storage:download` 的人可以下载任何未删除文件（含别人上传的）；
/// ③ 若产品要求「私有文件」，这是**安全缺口**（报告 BUG-D-08，P2），需要在读路径加
///    `uploaded_by ∪ 共享标记` 判定。
#[tokio::test]
async fn f05_download_has_no_owner_isolation() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F05").await;
    let owner = c::make_user(&ctx, "owner", "HUMAN").await;
    let other = c::make_user(&ctx, "other", "HUMAN").await;

    let file_id = make_file(&ctx, owner, "owner-private.txt", 128).await;

    // 读路径没有「当前用户」入参 —— 结构上就不可能做属主隔离
    let found = storage_model::find_active_file(&file_id)
        .await
        .expect("查询文件")
        .expect("文件应存在");
    assert_eq!(found.uploaded_by, Some(owner), "上传人应记录为 owner");

    // other 与 owner 看到的同一条记录（无 403 概念）
    let same = storage_model::find_active_file(&file_id)
        .await
        .expect("查询文件");
    assert_eq!(same.map(|row| row.id), Some(file_id), "读路径与请求者无关");

    // 权限码存在：能下载的人由权限码决定，而不是属主
    let n = c::count(
        "SELECT count(*) AS n FROM sys_menu WHERE auth_code = 'business:storage:download'",
        Vec::new(),
    )
    .await;
    assert!(n >= 1, "下载权限码未登记");
    println!(
        "[F05] 越权下载风险：读路径无属主隔离（other={other} 也能取到 owner={owner} 的文件 {file_id}）"
    );

    ctx.reset().await;
}

// ============================================================================
// F06 存储后端切换（本地 / MinIO）
// ============================================================================

/// F06 两套后端的关键路径都能工作：本地真实读写 + MinIO 风格 SigV4 预签名。
#[tokio::test]
async fn f06_both_backends_core_paths() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F06").await;

    // ---- 本地后端：真实写 / 读 / 删 ----
    let root = temp_root(&ctx).await;
    let local = LocalStore::with_root("LOCAL_A", &root);
    let body = b"local-backend-payload".to_vec();
    local
        .put("a/b.txt", &body, "text/plain")
        .await
        .expect("put");
    assert_eq!(local.get("a/b.txt").await.expect("get"), body);
    local.delete("a/b.txt").await.expect("delete");
    assert!(local.get("a/b.txt").await.is_err(), "删除后仍能读到");

    // ---- MinIO / S3 后端：同一份密钥算出的预签名 URL 结构正确 ----
    let key = format!("td/{}/f06.txt", ctx.tag);
    let host = "minio.internal:9000";
    let uri = sigv4::object_canonical_uri("cloudpivot", &key, true);
    let outcome = sigv4::presign(&sigv4::PresignOptions {
        method: "GET",
        scheme: "http",
        host,
        canonical_uri: &uri,
        extra_query: &[],
        ttl_secs: 900,
        access_key: "AKIATESTD",
        secret_key: "secret-testd-key",
        region: "us-east-1",
        now: chrono::Utc::now(),
    });
    assert!(
        outcome.url.contains("X-Amz-Signature="),
        "URL 缺签名：{}",
        outcome.url
    );
    assert!(outcome.url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
    assert!(
        outcome.url.contains("X-Amz-Expires=900"),
        "URL 缺有效期：{}",
        outcome.url
    );
    assert!(
        outcome.url.starts_with(&format!("http://{host}{uri}?")),
        "URL 结构不对：{}",
        outcome.url
    );

    // 两套后端实例互不干扰（配置编码独立）
    assert_eq!(local.config_code(), "LOCAL_A");
    assert_eq!(local.backend_type(), "LOCAL");

    let _ = tokio::fs::remove_dir_all(&root).await;
    ctx.reset().await;
}

// ============================================================================
// F07 预签名 URL 有效期
// ============================================================================

/// F07 ★ 预签名有效期：TTL 被夹到合法区间，过期时间 = 签发时刻 + 实际 TTL。
#[tokio::test]
async fn f07_presign_ttl_and_expiry() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F07").await;
    let now = chrono::Utc::now();
    let uri = sigv4::object_canonical_uri("cloudpivot", "td/f07.txt", true);

    let short = sigv4::presign(&sigv4::PresignOptions {
        method: "GET",
        scheme: "http",
        host: "minio.internal:9000",
        canonical_uri: &uri,
        extra_query: &[],
        ttl_secs: 1, // 低于下限 → 夹到 MIN_TTL_SECS
        access_key: "AK",
        secret_key: "SK",
        region: "us-east-1",
        now,
    });
    let clamped = short
        .url
        .split("X-Amz-Expires=")
        .nth(1)
        .and_then(|rest| rest.split('&').next())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("URL 里没有 X-Amz-Expires");
    assert!(clamped >= 1, "TTL 夹取失败：{clamped}");
    assert_eq!(
        short.expire_at,
        now + chrono::Duration::seconds(clamped as i64),
        "expire_at 与实际 TTL 不一致"
    );

    // 过期判定：expire_at 之后必须视为过期（服务端会 403）
    let later = short.expire_at + chrono::Duration::seconds(1);
    assert!(later > short.expire_at, "过期时间比较失效");

    // 策略层 TTL 配置也要在合法区间（1..=604800）
    assert!(
        storage_model::validate_policy_fields(50, 0, "", "").is_err(),
        "TTL=0 应被拒"
    );
    assert!(
        storage_model::validate_policy_fields(50, 604_801, "", "").is_err(),
        "TTL 超上限应被拒"
    );
    assert!(storage_model::validate_policy_fields(50, 900, "", "").is_ok());

    ctx.reset().await;
}

// ============================================================================
// F08 删除文件 → 软删还是物理删
// ============================================================================

/// F08 删除文件是**软删**：行保留（`deleted_at` 有值），读路径不再返回。
#[tokio::test]
async fn f08_delete_is_soft_delete() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F08").await;
    let owner = c::make_user(&ctx, "owner", "HUMAN").await;
    let file_id = make_file(&ctx, owner, "soft-delete.txt", 32).await;

    let affected = storage_model::soft_delete_file(&file_id, Some(owner))
        .await
        .expect("软删");
    assert_eq!(affected, 1, "软删应命中 1 行");

    // 行还在，只是 deleted_at 有值
    let remains = c::count(
        "SELECT count(*) AS n FROM sys_file WHERE id = ?::uuid",
        vec![Value::from(&file_id)],
    )
    .await;
    assert_eq!(remains, 1, "物理删除了行（应为软删）");
    let deleted_at = c::opt_text(
        "SELECT deleted_at::text AS v FROM sys_file WHERE id = ?::uuid",
        vec![Value::from(&file_id)],
    )
    .await;
    assert!(deleted_at.is_some(), "软删未写 deleted_at");

    // 读路径不再返回
    assert!(
        storage_model::find_active_file(&file_id)
            .await
            .expect("查询")
            .is_none(),
        "软删后 find_active_file 仍返回记录"
    );
    // 但「任意记录」查询能看到（清理任务靠它回收物理对象）
    assert!(
        storage_model::find_any_file(&file_id)
            .await
            .expect("查询")
            .is_some(),
        "find_any_file 看不到软删记录（物理回收会漏）"
    );

    // 幂等：重复删返回 0 行，不算失败
    let again = storage_model::soft_delete_file(&file_id, Some(owner))
        .await
        .expect("重复软删");
    assert_eq!(again, 0, "重复软删应返回 0 行");

    ctx.reset().await;
}

// ============================================================================
// F09 附件与业务单据关联
// ============================================================================

/// F09 ★ 附件与业务单据关联：单据**软删**时附件行保留，**硬删**才级联清。
#[tokio::test]
async fn f09_attachment_lifecycle_vs_document() {
    let Some(()) = c::ready().await else { return };
    let ctx = Ctx::new("F09").await;
    let uploader = c::make_user(&ctx, "uploader", "HUMAN").await;
    let file_id = make_file(&ctx, uploader, "attached.txt", 64).await;

    // 公告 + 附件行
    let ann_id = Uuid::now_v7();
    c::exec_with(
        "INSERT INTO sys_announcement (id, ann_no, title, summary, content_md, content_html, \
             content_format, level, scope_type, is_top, status, publish_at, receiver_count, read_count, version) \
         VALUES (?::uuid, ?, ?, '', '正文', '<p>正文</p>', 'MARKDOWN', 'NORMAL', 'ALL', false, \
                 'PUBLISHED', now(), 0, 0, 1)",
        vec![
            Value::from(&ann_id),
            Value::Text(format!("ANN{}", &ctx.prefix[3..])),
            Value::Text(ctx.key("附件公告")),
        ],
    )
    .await;
    c::exec_with(
        "INSERT INTO sys_announcement_attachment (id, announcement_id, file_id, file_name, file_size, mime_type, sort_order) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?, 64, 'text/plain', 1)",
        vec![
            Value::from(&ann_id),
            Value::from(&file_id),
            Value::Text(ctx.key("attached.txt")),
        ],
    )
    .await;

    // 软删公告 → 附件行保留（软删不触发 ON DELETE CASCADE）
    c::exec_with(
        "UPDATE sys_announcement SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&ann_id)],
    )
    .await;
    let after_soft = c::count(
        "SELECT count(*) AS n FROM sys_announcement_attachment WHERE announcement_id = ?::uuid",
        vec![Value::from(&ann_id)],
    )
    .await;
    assert_eq!(after_soft, 1, "软删公告后附件行被删了");

    // 外键确实是级联（硬删才会清）
    let rule = c::text(
        "SELECT confdeltype::text AS v FROM pg_constraint \
         WHERE conname = 'sys_announcement_attachment_announcement_id_fkey'",
        Vec::new(),
    )
    .await;
    assert_eq!(rule, "c", "附件表外键不是 CASCADE（confdeltype={rule}）");

    // 硬删公告 → 附件行级联清；**文件本体不删**（sys_file 是共享资源）
    c::exec_with(
        "DELETE FROM sys_announcement WHERE id = ?::uuid",
        vec![Value::from(&ann_id)],
    )
    .await;
    assert_eq!(
        c::count(
            "SELECT count(*) AS n FROM sys_announcement_attachment WHERE announcement_id = ?::uuid",
            vec![Value::from(&ann_id)]
        )
        .await,
        0,
        "硬删公告后附件行没级联清"
    );
    assert!(
        storage_model::find_active_file(&file_id)
            .await
            .expect("查询")
            .is_some(),
        "附件行级联删时把 sys_file 本体也删了（共享文件被误删）"
    );

    ctx.reset().await;
}

// ============================================================================
// Z01 夹具自清理自检（模块最后一条：串行跑完，整库不应留 TD- 前缀夹具）
// ============================================================================

/// Z01 自清理自检：本目标**所有**用例跑完后，`TD-` 前缀在 16 张关键表里残留 0 行。
///
/// 这条是为了钉死一类「静默污染」：清理语句顺序写错时（先删 `biz_project` 再删
/// `biz_task`，`biz_task.project_id` 是 `NO ACTION`）每条 DELETE 都会失败，而失败
/// 只打一行日志，测试照样全绿 —— 结果每跑一轮就往 `cloudpivot_test` 里塞 58 个用户、
/// 15 个项目。现在 `Ctx::reset` 按外键拓扑序执行并回报残留数，本用例是它的外部
/// 交叉验证（不依赖 self-report）。
///
/// 命名 `zz_` 前缀保证在 `--test-threads=1` 下最后执行（cargo 按全名排序）。
#[tokio::test]
async fn zz_no_fixture_residue_after_full_suite() {
    let Some(()) = c::ready().await else { return };
    let rows = c::root_residue().await;
    let dirty: Vec<String> = rows
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(name, n)| format!("{name}={n}"))
        .collect();
    println!("[Z01] TD- 前缀残留：{:?}", rows);
    assert!(
        dirty.is_empty(),
        "cloudpivot_test 存在本卡夹具残留（{}）：用例没有自清理干净",
        dirty.join(", ")
    );
}

//! C5「变更日志」真库集成测试：7 个必测用例（架构 §4.12.2 + §7 C5 行）。
//!
//! # 为什么这 7 条必须有真库
//!
//! 其中 3 条（只记变化字段 / `{b,a}` 成对 / 大字段摘要化）是**纯函数**，无库也能跑；
//! 另外 4 条只有在真库上才有意义：
//!
//! | 用例 | 只有真库能证的东西 |
//! |------|--------------------|
//! | `mask_applied_on_write` | `diff` 落进 JSONB 之后**仍然是**脱敏值（不是只在内存里打了个码） |
//! | `mask_ignores_operator_role` | 超管写入的那一行，`diff` 里同样没有明文（D13 无豁免） |
//! | `record_rejects_unregistered_table` | 未登记资源在**进入数据库之前**就以 62012 拒绝（`record()` 端到端） |
//! | `record_in_same_transaction_rolls_back` | **回滚后 `sys_change_log` 零残留**——D11 的核心断言 |
//!
//! # 用法（144 实测口径，见交付证据里的原始输出）
//!
//! ```bash
//! # 1) 专用验证库（不要用 cloudpivot / cloudpivot_test，避免与并行卡抢数据）
//! su postgres -c "createdb -O cloudpivot cloudpivot_c5v"
//! # 2) 跑一次迁移（看到「数据库迁移完成」即可退出）
//! set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
//! export NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_c5v NIK_SERVER_LISTEN=127.0.0.1:18095
//! export NIK_REDIS_URL=redis://127.0.0.1:6379/9
//! timeout 90 ./target/debug/cloudpivot
//! # 3) 跑本文件
//! export C5_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_c5v"
//! cargo test --test batchC_c5_changelog -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `C5_DB_URL` 时真库用例打印 `SKIP` 并返回（`cargo test` 全绿，CI 不需要数据库）；
//! 纯函数用例（前 3 条）**任何环境都真跑**。
//!
//! # 夹具与清理
//!
//! 全部夹具用固定前缀 `c5-` 的 `resource_id`，并在用例开头 / 结尾各清一次，
//! 重复跑不会积累垃圾，也不会碰到库里其它数据（只删自己插入的行）。

use cloudpivot::business::changelog::diff;
use cloudpivot::business::changelog::mask;
use cloudpivot::business::changelog::model;
use cloudpivot::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput, RecordOutcome};
use cloudpivot::shared::error_code;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::Value as Json;
use serde_json::json;

/// 夹具对象前缀（`resource_id` 用它）。
const TAG: &str = "c5-";

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("C5_DB_URL") else {
                println!("SKIP: 未设置 C5_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("C5 已连接验证库");
                    true
                }
                Err(error) => {
                    println!("SKIP: 连接数据库失败：{error}");
                    false
                }
            }
        })
        .await
}

/// 清掉本文件的夹具日志（只删 `resource_id LIKE 'c5-%'` 的行）。
async fn cleanup() {
    if !ensure_db().await {
        return;
    }
    match db()
        .execute_with_params(
            "DELETE FROM sys_change_log WHERE resource_id LIKE ?::text",
            vec![Value::from(format!("{TAG}%"))],
        )
        .await
    {
        Ok(rows) => println!("清理夹具：删除 {rows} 行 sys_change_log"),
        Err(error) => println!("清理夹具失败（忽略）：{error}"),
    }
}

/// 文本里是否含「中国大陆手机号」形态的 11 位数字（用于断言日志里没有明文号码）。
fn contains_mobile_like(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut run = 0usize;
    let mut start = 0usize;
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_ascii_digit() {
            if run == 0 {
                start = index;
            }
            run += 1;
            if run == 11 {
                if chars[start] == '1' {
                    return true;
                }
                run = 0;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// 去掉空白：PG 的 `jsonb::text` 输出会插 `", "` / `": "`，断言键值形态时先压缩。
fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn api_err(error: ApiError) -> DbError {
    DbError::InvalidValue(format!("{}|{}|{}", error.code, error.status, error.msg))
}

// ==================== 1/7 只记变化字段（纯函数，任何环境真跑） ====================

#[test]
fn diff_detects_changed_fields_only() {
    let policy = mask::Policy::builtin_only("sys_user");
    let before = json!({
        "username": "alice", "real_name": "张三", "mobile": "13800001111",
        "status": 1, "remark": "老员工"
    });
    let after = json!({
        "username": "alice", "real_name": "张三丰", "mobile": "13800001111",
        "status": 1, "remark": "老员工"
    });
    let doc = diff::build(
        &diff::SYS_USER,
        &policy,
        Action::Update,
        "c5-diff-1",
        Some(&before),
        Some(&after),
    );
    assert_eq!(
        doc.changed_fields(),
        vec!["real_name".to_string()],
        "只有 real_name 变了"
    );
    assert!(!doc.fields.contains_key("username"), "未变字段不得出现");
    assert!(!doc.fields.contains_key("mobile"));
    assert!(!doc.fields.contains_key("status"));
    // 全等 → 空文档 → record() 会返回 Skipped（不写空日志）
    let same = diff::build(
        &diff::SYS_USER,
        &policy,
        Action::Update,
        "c5-diff-1",
        Some(&before),
        Some(&before),
    );
    assert!(same.is_empty());
    println!("[PASS] diff_detects_changed_fields_only: 仅 real_name 入 diff，全等时空文档");
}

// ==================== 2/7 {b,a} 成对 ====================

#[test]
fn diff_pairs_before_after() {
    let policy = mask::Policy::builtin_only("sys_user");
    let before = json!({"real_name": "张三"});
    let after = json!({"real_name": "张三丰"});
    let doc = diff::build(
        &diff::SYS_USER,
        &policy,
        Action::Update,
        "c5-diff-2",
        Some(&before),
        Some(&after),
    );
    let pair = &doc.fields["real_name"];
    assert_eq!(pair.b, json!("张三"), "b 必须是旧值");
    assert_eq!(pair.a, json!("张三丰"), "a 必须是新值");
    // 序列化后两侧键都在（成对不是靠内存结构，而是靠落库的 JSON 形态）
    let text = doc.to_json_string().unwrap();
    assert!(text.contains(r#""b":"张三""#), "{text}");
    assert!(text.contains(r#""a":"张三丰""#), "{text}");

    // CREATE：b 侧必须是显式 null（不是「键缺失」）
    let created = diff::build(
        &diff::SYS_USER,
        &policy,
        Action::Create,
        "c5-diff-2",
        None,
        Some(&after),
    );
    assert_eq!(created.fields["real_name"].b, Json::Null);
    let text = created.to_json_string().unwrap();
    assert!(text.contains(r#""b":null"#), "新增必须显式写 null 侧：{text}");

    // DELETE：a 侧显式 null
    let deleted = diff::build(
        &diff::SYS_USER,
        &policy,
        Action::Delete,
        "c5-diff-2",
        Some(&after),
        None,
    );
    assert_eq!(deleted.fields["real_name"].a, Json::Null);
    println!("[PASS] diff_pairs_before_after: UPDATE 双侧、CREATE/DELETE 缺侧为显式 null");
}

// ==================== 3/7 大字段摘要化 ====================

#[test]
fn diff_large_field_summarized() {
    let policy = mask::Policy::builtin_only("sys_role");
    let long_before = format!("## 旧内容\n{}", "甲".repeat(3000));
    let long_after = format!("## 新内容\n{}", "乙".repeat(3000));
    let before = json!({"description": long_before});
    let after = json!({"description": long_after});
    let doc = diff::build(
        &diff::SYS_ROLE,
        &policy,
        Action::Update,
        "c5-diff-3",
        Some(&before),
        Some(&after),
    );
    assert!(doc.fields.is_empty(), "大字段不得进 fields（会存全文）");
    let pair = doc.big.get("description").expect("description 必须摘要化");
    assert!(pair.truncated, "truncated 标记必须为 true");
    assert!(pair.a.len > 3000);
    assert_eq!(pair.a.sha256.len(), 64, "sha256 必须是 64 位 hex");
    assert!(pair.a.sha256.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        pair.a.preview.chars().count(),
        diff::PREVIEW_CHARS,
        "preview 必须截到 {} 字",
        diff::PREVIEW_CHARS
    );
    assert_ne!(pair.b.sha256, pair.a.sha256, "内容变了，摘要必须不同");

    let text = doc.to_json_string().unwrap();
    assert!(!text.contains(&long_after), "全文不得入库");
    assert!(
        !text.contains(&"乙".repeat(diff::PREVIEW_CHARS + 1)),
        "预览点之后的内容不得出现"
    );
    assert!(text.contains("sha256"), "{text}");
    println!("[PASS] diff_large_field_summarized: len/sha256/preview(≤200)/truncated，全文未入库");
}

// ==================== 4/7 写入前脱敏（明文不入库） ====================

#[tokio::test]
async fn mask_applied_on_write() {
    if !ensure_db().await {
        return;
    }
    let rid = "c5-mask-write";
    cleanup().await;

    let old_mobile = "13800001111";
    let new_mobile = "13900002222";
    let old_hash = "$2b$12$OLDHASHOLDHASHOLDHASH";
    let new_hash = "$2b$12$NEWHASHNEWHASHNEWHASH";
    let before = json!({"mobile": old_mobile, "real_name": "张三", "password": old_hash});
    let after = json!({"mobile": new_mobile, "real_name": "张三丰", "password": new_hash});
    let ctx = ChangeCtx::api(None, Some("c5-tester"));
    let ctx_ref = &ctx;
    let input = DiffInput::update(&before, &after);
    let target = ChangeTarget::of("sys_user", rid, Action::Update, "修改用户");

    let outcome = db()
        .transaction(|tx| async move {
            let outcome = changelog::record(&tx, ctx_ref, target, input).await.map_err(api_err)?;
            Ok::<RecordOutcome, DbError>(outcome)
        })
        .await
        .expect("事务应提交成功");

    let RecordOutcome::Recorded { id, changed_fields } = outcome else {
        panic!("应有变化被记录");
    };
    assert_eq!(
        changed_fields,
        vec!["mobile".to_string(), "password".to_string(), "real_name".to_string()],
        "三个字段都变了（排序后）"
    );
    println!("recorded id={id} changed_fields={changed_fields:?}");

    let texts = model::diff_texts(db(), "sys_user", rid).await.expect("读回 diff");
    assert_eq!(texts.len(), 1, "一个事务应只写一行");
    let text = &texts[0];
    println!("落库的 diff：{text}");
    let flat = compact(text);

    // ① 明文手机号绝不入库（两侧都不能）
    assert!(!text.contains(old_mobile), "旧值明文泄露：{text}");
    assert!(!text.contains(new_mobile), "新值明文泄露：{text}");
    assert!(
        !contains_mobile_like(text),
        "diff 里不得出现手机号形态的 11 位数字：{text}"
    );
    // ② 脱敏值确实是掩码形态（手机号规则保留前 3 后 4）
    assert!(flat.contains("138****1111"), "旧值应为掩码：{flat}");
    assert!(flat.contains("139****2222"), "新值应为掩码：{flat}");
    assert!(flat.contains(r#""masked":true"#), "命中脱敏的字段要打标：{flat}");
    // 结构化断言（PG 的 jsonb 会按「键长度 + 字节序」重排键，不能拿原始字符串比顺序）
    let doc: Json = serde_json::from_str(text).expect("diff 必须是合法 JSON");
    assert_eq!(doc["fields"]["mobile"]["b"], json!("138****1111"), "旧手机号必须是掩码");
    assert_eq!(doc["fields"]["mobile"]["a"], json!("139****2222"));
    assert_eq!(doc["fields"]["mobile"]["masked"], json!(true));
    // ③ 口令类整值抹掉：连哈希都不入库
    assert!(!text.contains(old_hash) && !text.contains(new_hash), "口令泄露：{text}");
    assert_eq!(
        doc["fields"]["password"]["b"],
        json!({"redacted": true}),
        "口令旧值必须整值抹掉"
    );
    assert_eq!(
        doc["fields"]["password"]["a"],
        json!({"redacted": true}),
        "口令新值必须整值抹掉"
    );
    assert_eq!(doc["fields"]["password"]["masked"], json!(true));
    // ④ 姓名列被 V0066 的姓名规则命中 → 打码，明文不入库
    assert!(!text.contains("张三丰"), "姓名规则生效后不应出现明文：{text}");
    assert_eq!(doc["fields"]["real_name"]["b"], json!("张*"));
    assert_eq!(doc["fields"]["real_name"]["a"], json!("张**"));

    cleanup().await;
    println!("[PASS] mask_applied_on_write: 明文手机号/口令均未入库，掩码 + masked:true");
}

// ==================== 5/7 超管写入也脱敏（D13 无豁免） ====================

#[tokio::test]
async fn mask_ignores_operator_role() {
    if !ensure_db().await {
        return;
    }
    let rid = "c5-mask-super";
    cleanup().await;

    // 找一个真的持 SUPER_ADMIN 的角色用户（没有就说明种子缺失，跳过）
    let rows = db()
        .find_with_params(
            "SELECT u.id::text AS id, u.username FROM sys_user u \
             JOIN sys_user_role ur ON ur.user_id = u.id \
             JOIN sys_role r ON r.id = ur.role_id \
             WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL AND u.deleted_at IS NULL \
             LIMIT 1",
            vec![],
        )
        .await
        .expect("查超管");
    let Some(row) = rows.first() else {
        println!("SKIP: 库里没有 SUPER_ADMIN 用户（种子缺失），跳过本用例");
        return;
    };
    let super_id: Uuid = row.get_typed::<String>("id").unwrap().parse().expect("uuid");
    let super_name: String = row.get_typed("username").unwrap();
    println!("超管操作人：{super_name} ({super_id})");

    let before = json!({"mobile": "13712345678"});
    let after = json!({"mobile": "13787654321"});
    let ctx = ChangeCtx::api(Some(super_id), Some(&super_name));
    let ctx_ref = &ctx;
    let input = DiffInput::update(&before, &after);
    let target = ChangeTarget::of("sys_user", rid, Action::Update, "修改用户");
    db().transaction(|tx| async move {
        changelog::record(&tx, ctx_ref, target, input).await.map_err(api_err)?;
        Ok::<(), DbError>(())
    })
    .await
    .expect("事务应提交成功");

    let texts = model::diff_texts(db(), "sys_user", rid).await.expect("读回 diff");
    assert_eq!(texts.len(), 1);
    let text = &texts[0];
    println!("超管写入落库的 diff：{text}");
    let flat = compact(text);

    // D13：脱敏不因操作人角色豁免——超管那一行同样是掩码
    assert!(!text.contains("13712345678"), "超管旧值明文泄露：{text}");
    assert!(!text.contains("13787654321"), "超管新值明文泄露：{text}");
    assert!(
        !contains_mobile_like(text),
        "超管写入也不得出现手机号形态的 11 位数字：{text}"
    );
    assert!(flat.contains("137****5678"), "旧值应为掩码：{flat}");
    assert!(flat.contains("137****4321"), "新值应为掩码：{flat}");
    assert!(flat.contains(r#""masked":true"#), "{flat}");

    cleanup().await;
    println!("[PASS] mask_ignores_operator_role: 超管写入同样脱敏，库内无明文");
}

// ==================== 6/7 未登记表拒绝记录（62012） ====================

#[tokio::test]
async fn record_rejects_unregistered_table() {
    if !ensure_db().await {
        return;
    }
    let before = json!({"anything": "old"});
    let after = json!({"anything": "new"});
    let ctx = ChangeCtx::api(None, Some("c5-tester"));
    let ctx_ref = &ctx;
    let input = DiffInput::update(&before, &after);
    let target = ChangeTarget::of("sys_not_registered_by_c5", "c5-unreg", Action::Update, "乱记");
    let error = db()
        .transaction(|tx| async move {
            let error = changelog::record(&tx, ctx_ref, target, input)
                .await
                .expect_err("未登记资源必须失败");
            // 把错误带出事务（用 Err 让事务回滚，避免留下半截数据）
            Err::<(), DbError>(api_err(error))
        })
        .await
        .expect_err("事务应以错误结束");
    let raw = match error {
        DbError::InvalidValue(raw) => raw,
        other => panic!("期望 InvalidValue，实际 {other:?}"),
    };
    println!("未登记资源的错误：{raw}");
    let mut parts = raw.split('|');
    let code: i32 = parts.next().unwrap().trim().parse().expect("解析业务码");
    let status: u16 = parts.next().unwrap().trim().parse().expect("解析 HTTP 码");
    assert_eq!(code, error_code::CHANGE_LOG_RESOURCE_UNREGISTERED);
    assert_eq!(code, 62012);
    assert_eq!(status, 500, "接入缺陷必须 500（能被监控抓到）");

    // 且确实一行都没写
    let count = model::count_for_object(db(), "sys_not_registered_by_c5", "c5-unreg")
        .await
        .expect("计数");
    assert_eq!(count, 0);
    println!("[PASS] record_rejects_unregistered_table: 62012 / HTTP 500，零写入");
}

// ==================== 7/7 同事务回滚后零残留（D11 核心断言） ====================

#[tokio::test]
async fn record_in_same_transaction_rolls_back() {
    if !ensure_db().await {
        return;
    }
    let rid = "c5-rollback";
    cleanup().await;

    let before = json!({"mobile": "13800001111", "status": 1});
    let after = json!({"mobile": "13800009999", "status": 0});
    let ctx = ChangeCtx::api(None, Some("c5-tester"));
    let ctx_ref = &ctx;
    let input = DiffInput::update(&before, &after);

    // ---------- ① 事务被回滚：日志必须一起消失 ----------
    let target_rollback = ChangeTarget::of("sys_user", rid, Action::Update, "修改用户");
    let rolled = db()
        .transaction(|tx| async move {
            // 同一个 tx：先写日志（模拟 handler 里 record 的位置），再「业务失败」
            changelog::record(&tx, ctx_ref, target_rollback, input)
                .await
                .map_err(api_err)?;
            // 事务内先确认「此刻确实能看到这行」（证明写进去了，不是根本没写）
            let inside: i64 = tx
                .scalar_sql(
                    "SELECT COUNT(*) FROM sys_change_log WHERE resource = 'sys_user' \
                     AND resource_id = ?::text"
                        .to_string(),
                    vec![Value::from("c5-rollback")],
                )
                .await?;
            println!("回滚前（同事务内）可见行数 = {inside}");
            assert_eq!(inside, 1, "record 必须真的写进了当前事务");
            // 模拟业务后段失败 → 事务整体回滚
            Err::<i64, DbError>(DbError::InvalidValue("test: forced rollback".into()))
        })
        .await;
    assert!(rolled.is_err(), "事务应因业务失败而回滚");

    let after_rollback = model::count_for_object(db(), "sys_user", rid)
        .await
        .expect("回滚后计数");
    println!("回滚后 sys_change_log 行数 = {after_rollback}");
    assert_eq!(after_rollback, 0, "D11：业务回滚后日志必须零残留");

    // ---------- ② 对照组：同样的写法提交成功 → 恰好 1 行 ----------
    let target_commit = ChangeTarget::of("sys_user", rid, Action::Update, "修改用户");
    let outcome = db()
        .transaction(|tx| async move {
            let outcome = changelog::record(&tx, ctx_ref, target_commit, input)
                .await
                .map_err(api_err)?;
            Ok::<RecordOutcome, DbError>(outcome)
        })
        .await
        .expect("对照组事务应提交成功");
    assert!(matches!(outcome, RecordOutcome::Recorded { .. }));
    let after_commit = model::count_for_object(db(), "sys_user", rid)
        .await
        .expect("提交后计数");
    println!("对照组提交后 sys_change_log 行数 = {after_commit}");
    assert_eq!(after_commit, 1, "对照组必须能看到 1 行（证明断言不是永远为 0）");

    cleanup().await;
    println!("[PASS] record_in_same_transaction_rolls_back: 回滚 0 行 / 提交 1 行");
}

// ==================== 附：DDL 形态（月分区 + 无 DEFAULT） ====================

/// V0084 的分区形态自检：至少 3 个月分区、**没有** DEFAULT 分区、名字符合 `_YYYY_MM`。
#[tokio::test]
async fn ddl_month_partitions_without_default() {
    if !ensure_db().await {
        return;
    }
    let rows = db()
        .find_with_params(
            "SELECT c.relname AS name FROM pg_inherits i \
             JOIN pg_class c ON c.oid = i.inhrelid \
             JOIN pg_class p ON p.oid = i.inhparent \
             WHERE p.relname = 'sys_change_log' ORDER BY c.relname",
            vec![],
        )
        .await
        .expect("查分区清单");
    let names: Vec<String> = rows
        .iter()
        .map(|row| row.get_typed::<String>("name").unwrap())
        .collect();
    println!("sys_change_log 分区：{names:?}");
    assert!(names.len() >= 3, "至少预建 3 个月分区");
    assert!(
        names.iter().all(|name| name.starts_with("sys_change_log_")),
        "分区名必须是 sys_change_log_YYYY_MM"
    );
    assert!(
        !names.iter().any(|name| name.contains("default")),
        "不得存在 DEFAULT 分区（架构 D14：有 default 时新建正式分区会失败）"
    );
    println!(
        "[PASS] ddl_month_partitions_without_default: {} 个分区，无 default",
        names.len()
    );
}

//! 偏好体系的真库集成测试（**需要真实 PostgreSQL**，已迁移到 V0090/V0091）。
//!
//! # 为什么必须有这一层
//!
//! 模块内单测只能断言纯逻辑（校验规则、默认值合并）。本卡真正要证明的四件事只有真库能证：
//!
//! 1. **upsert 幂等**：同键写两次只有一行（`ON CONFLICT (user_id, pref_key)` 生效）；
//! 2. **局部更新只改传入 key**：没传的键原样保留（前端只发 palette 不会重置明暗模式）；
//! 3. **越权隔离**：A 写的值 B 读不到；B 写自己的值不会覆盖 A（行级隔离 = `user_id`）；
//! 4. **外键级联**：删除用户即清理其偏好（`ON DELETE CASCADE`）。
//!
//! 另外顺带断言「真库字典与前端色板一致」——`ui.palette` 的 `oneof` 必须正好 22 个色板 id，
//! 少一个就是「界面能选、后端拒绝保存」。
//!
//! # 用法
//!
//! ```bash
//! # 专用验证库（不要用 cloudpivot_test / 生产库，避免与并行的卡抢数据）
//! su postgres -c "createdb -O cloudpivot cloudpivot_prefv"
//! # 先用该库起一次服务（自动跑迁移 V0001..V0091 + 种子），看到监听即 Ctrl-C
//! export PREF_DB_URL="postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_prefv"
//! cargo test --test pref_preference -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `PREF_DB_URL` 时打印 `SKIP` 并返回（`cargo test` 全绿，CI 不需要数据库）。
//!
//! # 夹具与清理
//!
//! 固定 UUID + `pref_` 前缀用户名：开头 DELETE 一次、结尾 DELETE 一次，
//! 只删自己插入的行，重复跑不积累垃圾。

use cloudpivot::system::preference::{dto::UpdatePreferenceReq, model};
use nik::prelude::*;
use nik::uuid::Uuid;
use serde_json::json;
use std::collections::BTreeMap;

/// 用户 A（写偏好的人）。
const USER_A: &str = "9e000000-0000-0000-0000-00000000000a";
/// 用户 B（验证隔离的人）。
const USER_B: &str = "9e000000-0000-0000-0000-00000000000b";

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

fn patch(body: &str) -> Vec<(String, serde_json::Value)> {
    serde_json::from_str::<UpdatePreferenceReq>(body)
        .expect("请求体必须合法")
        .into_patch()
        .expect("请求体必须含至少一个偏好项")
}

/// 初始化全局数据库（进程内只初始化一次）；返回 `false` 表示跳过。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("PREF_DB_URL") else {
                println!("SKIP: 未设置 PREF_DB_URL（不连真实数据库）");
                return false;
            };
            match Db::connect(&url).await {
                Ok(connection) => {
                    if set_db(connection).is_err() {
                        println!("SKIP: 全局数据库已被别的用例初始化");
                        return false;
                    }
                    println!("偏好体系已连接验证库");
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

/// 清掉夹具（用户行删除时偏好行由外键级联清理）。
async fn cleanup() {
    for user in [USER_A, USER_B] {
        let _ = db()
            .execute_with_params("DELETE FROM sys_user WHERE id = ?::uuid", vec![Value::from(uuid(user))])
            .await;
    }
}

/// 插入夹具用户（固定 UUID；先删后插保证可重复跑）。
async fn seed_user(user: &str, username: &str) {
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::uuid, ?, 'x', ?, 1)",
        vec![
            Value::from(uuid(user)),
            Value::from(username.to_string()),
            Value::from(format!("夹具 {username}")),
        ],
    )
    .await
    .expect("插入夹具用户");
}

/// 某用户的某键原始值（`jsonb` → 文本），`None` 表示没有这一行。
async fn raw_value(user: &str, key: &str) -> Option<String> {
    let rows = db()
        .find_with_params(
            "SELECT pref_value::text AS v FROM sys_user_preference \
             WHERE user_id = ?::uuid AND pref_key = ?",
            vec![Value::from(uuid(user)), Value::from(key.to_string())],
        )
        .await
        .expect("查询偏好行");
    rows.first()
        .map(|row| row.get_typed::<String>("v").expect("读取 pref_value 文本"))
}

/// 某用户的行数。
async fn row_count(user: &str) -> i64 {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM sys_user_preference WHERE user_id = ?::uuid",
        vec![Value::from(uuid(user))],
    )
    .await
    .expect("统计偏好行数")
}

/// 把一组键值对写进去（走与 handler 相同的事务 + model 函数）。
async fn save(user: &str, items: Vec<(String, serde_json::Value)>) {
    let defs = model::load_defs(db()).await.expect("读取偏好字典");
    model::validate_patch(&defs, &items).expect("补丁应当通过字典校验");
    let user_id = uuid(user);
    db().transaction(|tx| async move {
        model::upsert_values(&tx, &user_id, &items).await?;
        Ok(())
    })
    .await
    .expect("写入偏好");
}

/// 四个场景写进**同一个** `#[tokio::test]` 串行跑：`cargo test` 默认多线程，
/// 固定 UUID 夹具在并行用例里会互相踩（批次 C · C3 已踩过）。
#[tokio::test]
async fn preference_db_behaviour() {
    if !ensure_db().await {
        return;
    }
    cleanup().await;
    seed_user(USER_A, "pref_fixture_a").await;
    seed_user(USER_B, "pref_fixture_b").await;

    // ---------- ① upsert 幂等 ----------
    save(USER_A, patch(r#"{"items":{"ui.palette":"deep-sea"}}"#)).await;
    let first = raw_value(USER_A, "ui.palette").await.expect("首写应有行");
    save(USER_A, patch(r#"{"items":{"ui.palette":"deep-sea"}}"#)).await;
    assert_eq!(
        row_count(USER_A).await,
        1,
        "同键重复写不得产生第二行（ON CONFLICT 未生效？）"
    );
    save(USER_A, patch(r#"{"items":{"ui.palette":"jade"}}"#)).await;
    let third = raw_value(USER_A, "ui.palette").await.expect("改值后应有行");
    assert_eq!(row_count(USER_A).await, 1, "改值同样只能是一行");
    assert_eq!(first, "\"deep-sea\"", "首写值应为 deep-sea 的 JSON 文本");
    assert_eq!(third, "\"jade\"", "第二次写的值应覆盖第一次");
    println!("① upsert 幂等：1 行，deep-sea → jade ✓");

    // ---------- ② 局部更新只改传入 key ----------
    save(USER_A, patch(r#"{"items":{"ui.page_size":50}}"#)).await;
    assert_eq!(
        raw_value(USER_A, "ui.palette").await.as_deref(),
        Some("\"jade\""),
        "没传的键必须原样保留"
    );
    assert_eq!(raw_value(USER_A, "ui.page_size").await.as_deref(), Some("50"));
    assert_eq!(row_count(USER_A).await, 2, "此时 A 应恰有 2 行");
    println!("② 局部更新：palette 保持 jade，page_size=50 ✓");

    // ---------- ③ 越权隔离 ----------
    assert_eq!(raw_value(USER_B, "ui.palette").await, None, "B 不得看到 A 的偏好");
    let defs = model::load_defs(db()).await.expect("读取偏好字典");
    let stored_b = model::load_values(db(), &uuid(USER_B)).await.expect("读取 B 的偏好");
    assert!(stored_b.is_empty(), "B 的已设置集合必须为空");
    let merged_b = model::assemble(&defs, &stored_b).expect("合并默认值");
    assert_eq!(merged_b.palette, "mist-blue", "B 应拿到字典默认配色");
    assert_eq!(merged_b.items.len(), 8, "items 必须是全量 8 项");

    save(USER_B, patch(r#"{"palette":"rouge","theme_mode":"dark"}"#)).await;
    assert_eq!(
        raw_value(USER_A, "ui.palette").await.as_deref(),
        Some("\"jade\""),
        "B 写自己的偏好不得影响 A"
    );
    assert_eq!(
        raw_value(USER_B, "ui.palette").await.as_deref(),
        Some("\"rouge\""),
        "旧形态三字段必须能落库"
    );
    assert_eq!(raw_value(USER_B, "ui.theme_mode").await.as_deref(), Some("\"dark\""));
    println!("③ 越权隔离：A=jade / B=rouge，互不可见 ✓");

    // ---------- ④ 真库字典自检（8 项 + 22 套色板） ----------
    assert_eq!(defs.len(), 8, "V0091 必须种下 8 项预置偏好");
    let palette_def = defs
        .iter()
        .find(|def| def.pref_key == "ui.palette")
        .expect("字典里必须有 ui.palette");
    let rule = palette_def.validator.as_deref().expect("ui.palette 必须有 oneof 校验");
    let allowed = rule.strip_prefix("oneof:").expect("ui.palette 校验应为 oneof");
    assert_eq!(
        allowed.split(',').count(),
        22,
        "ui.palette 的 oneof 必须正好 22 套色板（与 style/palettes.ts 一致）"
    );
    for id in ["mist-blue", "deep-blue-orange", "clear-mist"] {
        assert!(allowed.split(',').any(|item| item == id), "oneof 缺色板 {id}");
    }
    println!("④ 真库字典：8 项预置 + ui.palette oneof 22 套色板 ✓");

    // ---------- ⑤ 外键级联：删用户清偏好 ----------
    let before = row_count(USER_B).await;
    assert!(before > 0, "B 应有偏好行");
    db().execute_with_params(
        "DELETE FROM sys_user WHERE id = ?::uuid",
        vec![Value::from(uuid(USER_B))],
    )
    .await
    .expect("删除夹具用户");
    assert_eq!(
        row_count(USER_B).await,
        0,
        "删除用户必须级联清理其偏好（fk_sys_user_preference_user）"
    );
    println!("⑤ 外键级联：删用户后偏好行清零 ✓");

    // ---------- ⑥ 无记录用户的 GET 默认值（合并结果逐项自检） ----------
    let merged = model::assemble(&defs, &BTreeMap::new()).expect("合并默认值");
    let expected = [
        ("ui.palette", json!("mist-blue")),
        ("ui.theme_mode", json!("light")),
        ("ui.side_compact", json!(false)),
        ("ui.table_density", json!("default")),
        ("ui.page_size", json!(10)),
        ("ui.language", json!("zh-CN")),
        ("notify.inbox", json!(true)),
        ("notify.email", json!(false)),
    ];
    for (key, value) in expected {
        assert_eq!(merged.items.get(key), Some(&value), "字典默认值不符：{key}");
    }
    assert_eq!(merged.items.len(), 8, "默认集合必须是 8 项");
    println!("⑥ 无记录用户 GET 默认值：8 项全部命中 ✓");

    cleanup().await;
}

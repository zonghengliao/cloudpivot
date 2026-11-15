//! C7「回滚零残留」门禁的 Rust 侧（**仅** `--features test-hooks` 编译）。
//!
//! 依据 `docs/批次C-技术架构.md` §4.12 与 §7 C7 行。这里只有两条用例，但它们是整批
//! 「审计可信度」的支点：
//!
//! | 用例 | 证明什么 | 只有真库能证的部分 |
//! |------|----------|--------------------|
//! | `rollback_leaves_zero_residue` | 事务回滚后 `sys_change_log` **零残留**（`0|1|0`） | 回滚后的计数走连接池，是「另一条连接看不到那行」，不是同一事务的快照错觉 |
//! | `commit_control_group_leaves_one_row` | 同样的写法**提交**后恰好 1 行 | 证明上一条不是「怎么写都 0 行」的假绿 |
//!
//! # 用法（144 实测口径）
//!
//! ```bash
//! set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
//! cargo test --features test-hooks --test batchC_c7_rollback -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `C7_DB_URL` / `NIK_DATABASE_*` 时打印 `SKIP` 并返回（CI 无库时仍全绿）：
//! 这类用例的**结论**必须来自真库，所以宁可在无库环境明确跳过，也不 mock 一个假库自欺。

#![cfg(feature = "test-hooks")]

use cloudpivot::business::changelog::model;
use cloudpivot::business::changelog::test_hooks;
use cloudpivot::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 夹具前缀：清理时只删自己插入的行（`resource_id LIKE 'c7-%'`）。
const TAG: &str = "c7-";

/// 连接验证库（每个用例独立连接；`Db` 自身带连接池，够用且不依赖全局初始化）。
async fn connect() -> Option<Db> {
    let url = if let Ok(url) = std::env::var("C7_DB_URL") {
        url
    } else if let (Ok(user), Ok(name)) = (std::env::var("NIK_DATABASE_USER"), std::env::var("NIK_DATABASE_NAME")) {
        let host = std::env::var("NIK_DATABASE_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let port = std::env::var("NIK_DATABASE_PORT").unwrap_or_else(|_| "5432".into());
        let password = std::env::var("NIK_DATABASE_PASSWORD").unwrap_or_default();
        if password.is_empty() {
            format!("postgres://{user}@{host}:{port}/{name}")
        } else {
            format!("postgres://{user}:{password}@{host}:{port}/{name}")
        }
    } else {
        println!("SKIP: 未设置 C7_DB_URL / NIK_DATABASE_*（不连真实数据库）");
        return None;
    };
    match Db::connect(&url).await {
        Ok(connection) => Some(connection),
        Err(error) => {
            println!("SKIP: 连接数据库失败：{error}");
            None
        }
    }
}

/// 清掉本文件的夹具行（只按 `resource_id LIKE 'c7-%'` 删）。
async fn cleanup(db: &Db) {
    match db
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

fn before_after() -> (serde_json::Value, serde_json::Value) {
    (
        serde_json::json!({"username": "c7-user", "mobile": "13800001111", "status": 1}),
        serde_json::json!({"username": "c7-user", "mobile": "13800009999", "status": 0}),
    )
}

/// ① 回滚零残留：探针必须给出 `0|1|0`。
#[tokio::test]
async fn rollback_leaves_zero_residue() {
    let Some(db) = connect().await else {
        return;
    };
    cleanup(&db).await;
    let resource_id = format!("{TAG}rollback-{}", Uuid::now_v7());
    let (before, after) = before_after();
    let ctx = ChangeCtx::api(None, Some("c7-tester"));

    let probe = test_hooks::record_then_rollback(
        &db,
        &ctx,
        ChangeTarget::of("sys_user", resource_id.as_str(), Action::Update, "修改用户"),
        DiffInput::update(&before, &after),
    )
    .await
    .expect("探针执行");

    println!("ROLLBACK_PROBE={}", probe.flat());
    assert_eq!(
        probe.flat(),
        "0|1|0",
        "回滚零残留必须是 0（回滚前）|1（事务内）|0（回滚后），实际 {}",
        probe.flat()
    );
    assert!(
        probe.recorded,
        "record 必须真的写入（Skipped 说明输入前后相等，探针无效）"
    );
    assert!(probe.is_zero_residue(), "is_zero_residue 与 flat() 必须同口径");

    // 独立再查一次，排除「探针自己算错了」的可能
    let residue = model::count_for_object(&db, "sys_user", &resource_id)
        .await
        .expect("回滚后计数");
    assert_eq!(residue, 0, "D11：业务回滚后日志必须零残留");

    cleanup(&db).await;
    println!("[PASS] rollback_leaves_zero_residue: {}", probe.flat());
}

/// ② 对照组：同样的写法提交成功 → 恰好 1 行（证明上一条不是假绿）。
#[tokio::test]
async fn commit_control_group_leaves_one_row() {
    let Some(db) = connect().await else {
        return;
    };
    cleanup(&db).await;
    let resource_id = format!("{TAG}commit-{}", Uuid::now_v7());
    let rid_for_tx = resource_id.clone();
    let (before, after) = before_after();
    let ctx = ChangeCtx::api(None, Some("c7-tester"));

    db.transaction(|tx| async move {
        changelog::record_in(
            &tx,
            &ctx,
            ChangeTarget::of("sys_user", rid_for_tx.as_str(), Action::Update, "修改用户"),
            DiffInput::update(&before, &after),
        )
        .await?;
        Ok::<(), DbError>(())
    })
    .await
    .expect("对照组事务应提交成功");

    let residue = model::count_for_object(&db, "sys_user", &resource_id)
        .await
        .expect("提交后计数");
    assert_eq!(residue, 1, "对照组必须能看到 1 行（证明断言不是永远为 0）");

    cleanup(&db).await;
    println!("[PASS] commit_control_group_leaves_one_row: 1 行");
}

//! A4「异步任务与 worker」的真实数据库集成测试（认领互斥 / 僵尸回收 / 过期产物清理）。
//!
//! # 为什么在 `tests/` 而不是单测
//!
//! 这三条能力的地基是 PostgreSQL 的**行锁语义**（`FOR UPDATE SKIP LOCKED`）与
//! `CASE WHEN` 状态机，纯 Rust 单测只能验证 SQL 字符串长得对（见 `model` 的
//! `claim_sql_uses_skip_locked_and_guards_state`），验证不了「两个连接抢同一行时只有一个拿到」。
//!
//! # 用法
//!
//! ```bash
//! set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
//! export A4_DB_URL="postgres://${NIK_DATABASE_USER}:${NIK_DATABASE_PASSWORD}@\
//! ${NIK_DATABASE_HOST}:${NIK_DATABASE_PORT}/${NIK_DATABASE_NAME}"
//! cargo test --test impexp_worker -- --nocapture --test-threads=1
//! ```
//!
//! 缺 `A4_DB_URL` 时每个用例打印 `SKIP` 并直接返回（`cargo test` 仍全绿，CI 不需要数据库）。
//!
//! # 并发用例要跑两个进程
//!
//! `claim_is_exclusive_across_processes` 需要**两个操作系统进程**在同一个绝对时刻认领同一条任务
//! （单进程内两个协程也能测，但「多副本部署」的安全性只有跨进程才说服力足）。
//! 因此由 `tests/impexp/verify_a4_worker.sh` 起两个进程，用 `A4_CLAIM_AT`（Unix 毫秒）
//! 做墙钟屏障：

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cloudpivot::business::impexp::model::{self, NewExportTask};
use cloudpivot::business::impexp::{cleanup, worker};
use nik::prelude::*;

/// 初始化全局数据库（进程内只初始化一次）。
///
/// # 返回值
///
/// `false` 表示没配 `A4_DB_URL`（用例按 SKIP 处理）。
async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("A4_DB_URL") else {
                println!("SKIP: 未设置 A4_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_none() {
                // 注意：`nik::prelude::db()` 是 **nik-web 的资源层**（`resources::set_db`），
                // 与 `nik_db::init()` 的全局不是同一个 OnceLock。集成测试没有 `App::start()`，
                // 因此要自己连一个 `Db` 再 `set_db()` 注册进去，否则会 panic「数据库未初始化」。
                match Db::connect(&url).await {
                    Ok(connection) => {
                        if set_db(connection).is_err() {
                            println!("SKIP: 全局数据库已被别的用例初始化");
                            return false;
                        }
                        println!("已连接测试库");
                    }
                    Err(error) => {
                        println!("SKIP: 连接测试库失败: {error}");
                        return false;
                    }
                }
            }
            true
        })
        .await
}

/// 当前 Unix 毫秒。
fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default()
}

/// 插一条 `PENDING` 导出任务（`create_by` 为空，便于用例清理时精准定位自己造的行）。
async fn insert_task(biz_type: &str, export_name: &str) -> uuid::Uuid {
    let id = uuid::Uuid::now_v7();
    let task_no = model::build_export_task_no(&id);
    model::insert_export_task(&NewExportTask {
        id,
        task_no: &task_no,
        biz_type,
        biz_name: "A4 集成测试",
        export_name,
        format: "XLSX",
        query_params: Some("{}"),
        columns_snapshot: Some("[]"),
        max_attempts: worker::DEFAULT_MAX_ATTEMPTS,
        operator: None,
    })
    .await
    .expect("插入导出任务失败");
    id
}

/// 删掉本用例造的行（按 `export_name` 前缀）。
async fn drop_tasks(export_name_prefix: &str) {
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_export_task WHERE export_name LIKE ? AND create_by IS NULL",
            vec![Value::from(format!("{export_name_prefix}%"))],
        )
        .await;
}

/// 清掉所有「无创建人且 PENDING」的任务（worker 用例要独占认领，避免串味）。
async fn clear_pending() {
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_export_task WHERE status = 'PENDING' AND create_by IS NULL",
            vec![],
        )
        .await;
}

// ==================== 验收 2：并发认领互斥 ====================

/// 两个进程在同一时刻认领同一条任务：只有一个拿到。
///
/// # 环境变量
///
/// - `A4_DB_URL`：数据库 URL（必需，否则 SKIP）
/// - `A4_TASK_ID`：被抢的任务 ID（必需）
/// - `A4_CLAIM_AT`：墙钟屏障（Unix 毫秒），到点同时发认领
/// - `A4_WORKER_ID`：本进程的认领者标识
#[tokio::test]
async fn claim_is_exclusive_across_processes() {
    if !ensure_db().await {
        return;
    }
    let Some(task_id) = std::env::var("A4_TASK_ID").ok() else {
        println!("SKIP: 未设置 A4_TASK_ID（本用例由 verify_a4_worker.sh 起两个进程跑）");
        return;
    };
    let claim_at: u128 = std::env::var("A4_CLAIM_AT")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or_else(now_millis);
    let worker_id =
        std::env::var("A4_WORKER_ID").unwrap_or_else(|_| format!("proc-{}", std::process::id()));

    // 屏障：两个进程都等到同一个绝对时刻才发认领，才算「同时」
    loop {
        let now = now_millis();
        if now >= claim_at {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    let started = now_millis();
    let claimed = model::claim_export_task(&worker_id)
        .await
        .expect("认领查询失败");
    let elapsed = now_millis().saturating_sub(started);
    match claimed {
        Some(task) => println!(
            "[{worker_id}] CLAIMED task_id={} task_no={} elapsed_ms={elapsed}",
            task.id, task.task_no
        ),
        None => println!("[{worker_id}] NO_TASK 被另一进程抢先认领 elapsed_ms={elapsed}"),
    }
    // 任务行必须是「RUNNING 且 locked_by = 赢家」，这一断言两个进程都会跑
    let row = db()
        .find_first_with_params(
            "SELECT status, locked_by, attempt_count FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(
                uuid::Uuid::parse_str(&task_id).expect("任务 ID 非法"),
            )],
        )
        .await
        .expect("查询任务失败")
        .expect("任务不存在");
    let status: String = row.get_typed("status").expect("status");
    let locked_by: Option<String> = row.get_typed("locked_by").expect("locked_by");
    let attempt_count: i16 = row.get_typed("attempt_count").expect("attempt_count");
    println!(
        "[{worker_id}] DB status={status} locked_by={locked_by:?} attempt_count={attempt_count}"
    );
    assert_eq!(status, "RUNNING", "认领后状态必须是 RUNNING");
    assert_eq!(attempt_count, 0, "认领不增加尝试次数");
    assert!(
        locked_by.is_some(),
        "必须有一个 worker 名写在 locked_by 上（不可能两个都是空）"
    );
}

// ==================== 验收 3：僵尸回收 ====================

/// `RUNNING` 且心跳超 30 分钟的任务被清理任务回收。
#[tokio::test]
async fn zombie_running_task_is_reclaimed() {
    if !ensure_db().await {
        return;
    }
    drop_tasks("A4-zombie").await;
    let id = insert_task("system.dict_item", "A4-zombie-回收用例").await;
    // 手工造成「进程已死」：RUNNING + 31 分钟前的心跳
    db().execute_with_params(
        "UPDATE sys_export_task SET status = 'RUNNING', locked_by = 'ghost-worker', \
             heartbeat_at = CURRENT_TIMESTAMP - (31 * 60 * INTERVAL '1 second'), \
             started_at = CURRENT_TIMESTAMP - (31 * 60 * INTERVAL '1 second'), progress = 60 \
         WHERE id = ?::uuid",
        vec![Value::from(id)],
    )
    .await
    .expect("制造僵尸任务失败");
    let before = db()
        .find_first_with_params(
            "SELECT status, locked_by, progress, attempt_count, max_attempts, \
                    heartbeat_at::text AS heartbeat_at FROM sys_export_task WHERE id = ?::uuid",
            vec![Value::from(id)],
        )
        .await
        .expect("查询失败")
        .expect("任务不存在");
    println!(
        "回收前: status={} locked_by={:?} progress={:?} attempt={:?}/{:?} heartbeat={:?}",
        before.get_typed::<String>("status").unwrap(),
        before.get_typed::<Option<String>>("locked_by").unwrap(),
        before.get_typed::<i16>("progress").unwrap(),
        before.get_typed::<i16>("attempt_count").unwrap(),
        before.get_typed::<i16>("max_attempts").unwrap(),
        before.get_typed::<Option<String>>("heartbeat_at").unwrap(),
    );

    let reclaimed = cleanup::reclaim_stale(1_800, 10).await.expect("回收失败");
    let mine = reclaimed
        .iter()
        .find(|task| task.id == id.to_string())
        .expect("本用例的僵尸任务必须被回收");
    println!(
        "回收结果: task_no={} status={} attempt={}/{} previous_worker={:?} previous_heartbeat={:?}",
        mine.task_no,
        mine.status,
        mine.attempt_count,
        mine.max_attempts,
        mine.previous_worker,
        mine.previous_heartbeat
    );

    let after = model::find_export_task_unscoped(&id)
        .await
        .expect("查询失败")
        .expect("任务不存在");
    println!(
        "回收后: status={} locked_by={:?} progress={} attempt={}/{} message={:?}",
        after.status,
        after.locked_by,
        after.progress,
        after.attempt_count,
        after.max_attempts,
        after.message
    );
    assert_eq!(after.status, "PENDING", "未达上限应回到 PENDING 等再执行");
    assert_eq!(after.attempt_count, 1, "回收要算一次尝试");
    assert_eq!(after.progress, 0, "重新排队后进度归零（下次从 0 重新推进）");
    assert!(after.locked_by.is_none(), "认领者必须被清空");
    assert!(after.heartbeat_at.is_none(), "心跳必须被清空");
    assert!(
        after.message.as_deref().unwrap_or("").contains("心跳超时"),
        "message 要写明回收原因: {:?}",
        after.message
    );
    drop_tasks("A4-zombie").await;
}

/// 僵尸回收也算一次尝试：达到上限直接判 `FAILED`，不会无限崩溃重排。
#[tokio::test]
async fn zombie_reclaim_marks_failed_when_attempts_exhausted() {
    if !ensure_db().await {
        return;
    }
    drop_tasks("A4-zombie-max").await;
    let id = insert_task("system.dict_item", "A4-zombie-max-终态用例").await;
    db().execute_with_params(
        "UPDATE sys_export_task SET status = 'RUNNING', locked_by = 'ghost-worker', \
             attempt_count = max_attempts - 1, \
             heartbeat_at = CURRENT_TIMESTAMP - (45 * 60 * INTERVAL '1 second') \
         WHERE id = ?::uuid",
        vec![Value::from(id)],
    )
    .await
    .expect("制造僵尸任务失败");
    let reclaimed = cleanup::reclaim_stale(1_800, 10).await.expect("回收失败");
    assert!(
        reclaimed.iter().any(|task| task.id == id.to_string()),
        "应被回收"
    );
    let after = model::find_export_task_unscoped(&id)
        .await
        .expect("查询失败")
        .expect("任务不存在");
    assert_eq!(after.status, "FAILED");
    assert_eq!(after.attempt_count, after.max_attempts);
    assert!(after.finished_at.is_some(), "终态必须写完成时间");
    drop_tasks("A4-zombie-max").await;
}

/// 心跳新鲜的 `RUNNING` 任务**不能**被回收（别把正在跑的任务抢走）。
#[tokio::test]
async fn fresh_running_task_is_not_reclaimed() {
    if !ensure_db().await {
        return;
    }
    drop_tasks("A4-alive").await;
    let id = insert_task("system.dict_item", "A4-alive-新鲜心跳用例").await;
    db().execute_with_params(
        "UPDATE sys_export_task SET status = 'RUNNING', locked_by = 'alive-worker', \
             heartbeat_at = CURRENT_TIMESTAMP, started_at = CURRENT_TIMESTAMP - (2 * 60 * INTERVAL '1 hour') \
         WHERE id = ?::uuid",
        vec![Value::from(id)],
    )
    .await
    .expect("制造任务失败");
    // started_at 是 2 小时前，但心跳是刚刚（模拟「跑得久但活着」）
    let reclaimed = cleanup::reclaim_stale(1_800, 50).await.expect("回收失败");
    assert!(
        !reclaimed.iter().any(|task| task.id == id.to_string()),
        "心跳新鲜的任务不该被回收"
    );
    drop_tasks("A4-alive").await;
}

// ==================== 验收 4：过期产物清理 ====================

/// 过期产物：删文件 + 置 `EXPIRED`（下载接口据此返回 410）。
#[tokio::test]
async fn expired_artifact_file_is_purged() {
    if !ensure_db().await {
        return;
    }
    drop_tasks("A4-expire").await;
    let id = insert_task("system.dict_item", "A4-expire-过期用例").await;
    let dir = std::env::temp_dir().join("a4-expire-test");
    tokio::fs::create_dir_all(&dir).await.expect("建目录失败");
    let path = dir.join(format!("{id}.xlsx"));
    tokio::fs::write(&path, b"PK\x03\x04 fake xlsx")
        .await
        .expect("写临时产物失败");
    db().execute_with_params(
        "UPDATE sys_export_task SET status = 'SUCCESS', progress = 100, \
             file_path = ?, file_name = '过期用例.xlsx', file_size = 16, file_sha256 = 'deadbeef', \
             expire_at = CURRENT_TIMESTAMP - INTERVAL '1 minute' \
         WHERE id = ?::uuid",
        vec![
            Value::from(path.to_string_lossy().to_string()),
            Value::from(id),
        ],
    )
    .await
    .expect("制造过期产物失败");
    let before = model::find_export_task_unscoped(&id)
        .await
        .expect("查询失败")
        .expect("任务不存在");
    println!(
        "清理前: status={} expire_at={:?} file_exists={}",
        before.status,
        before.expire_at,
        path.exists()
    );
    assert!(before.is_expired(chrono::Utc::now()), "应被判为过期");

    let expired = cleanup::expire_artifacts(50).await.expect("清理失败");
    let mine = expired
        .iter()
        .find(|artifact| artifact.id == id.to_string())
        .expect("本用例的过期产物必须被处理");
    println!(
        "清理结果: task_no={} file_path={:?} file_deleted={}",
        mine.task_no, mine.file_path, mine.file_deleted
    );
    let after = model::find_export_task_unscoped(&id)
        .await
        .expect("查询失败")
        .expect("任务不存在");
    println!(
        "清理后: status={} file_path={:?}",
        after.status, after.file_path
    );
    assert_eq!(after.status, "EXPIRED");
    assert!(after.file_path.is_none(), "路径要被清空");
    assert!(!path.exists(), "物理文件必须被删除");
    drop_tasks("A4-expire").await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// ==================== worker 失败收尾 ====================

/// 定义未注册 => 终态失败（不进退避重试），`message` 写明原因。
#[tokio::test]
async fn worker_marks_failed_when_definition_missing() {
    if !ensure_db().await {
        return;
    }
    // 认领是「按 created_at 取最老的一条」，先清掉本用例以外可能存在的 PENDING 行
    clear_pending().await;
    let id = insert_task("a4.no_such_definition", "A4-nodef-未注册定义用例").await;
    let dir = std::env::temp_dir().join("a4-nodef-test");
    let options = worker::WorkerOptions::with_dir("a4-test-worker", &dir);
    let options = worker::WorkerOptions {
        claim_limit: 1,
        ..options
    };
    let report = worker::run_once_with(&options)
        .await
        .expect("worker 执行失败");
    println!("worker 报告: {}", report.summary());
    assert_eq!(report.claimed, 1);
    assert_eq!(report.failed, 1, "未注册定义应判终态失败");
    let after = model::find_export_task_unscoped(&id)
        .await
        .expect("查询失败")
        .expect("任务不存在");
    println!(
        "任务终态: status={} attempt={}/{} message={:?}",
        after.status, after.attempt_count, after.max_attempts, after.message
    );
    assert_eq!(after.status, "FAILED");
    assert_eq!(
        after.attempt_count, after.max_attempts,
        "终态失败要把尝试次数顶到上限，避免下次又被认领"
    );
    assert!(
        after.message.as_deref().unwrap_or("").contains("未注册"),
        "message 要写明原因: {:?}",
        after.message
    );
    drop_tasks("A4-nodef").await;
}

/// 退避重试：第一次失败回 `PENDING` 且 `next_run_at` 在未来（退避生效）。
#[tokio::test]
async fn failure_schedules_backoff_and_stays_pending() {
    if !ensure_db().await {
        return;
    }
    drop_tasks("A4-backoff").await;
    let id = insert_task("system.dict_item", "A4-backoff-退避用例").await;
    db().execute_with_params(
        "UPDATE sys_export_task SET status = 'RUNNING', locked_by = 'w', heartbeat_at = CURRENT_TIMESTAMP \
         WHERE id = ?::uuid",
        vec![Value::from(id)],
    )
    .await
    .expect("制造 RUNNING 失败");
    let outcome = model::fail_export_task(&id, "数据库抖动（用例）", 30)
        .await
        .expect("回写失败")
        .expect("任务应处于 RUNNING 才能收尾");
    println!("失败收尾结果: {outcome:?}");
    assert_eq!(outcome.0, "PENDING");
    let after = model::find_export_task_unscoped(&id)
        .await
        .expect("查询失败")
        .expect("任务不存在");
    assert_eq!(after.status, "PENDING");
    assert_eq!(after.attempt_count, 1);
    let next_run_at = after.next_run_at.expect("必须写入 next_run_at");
    let delta = next_run_at - chrono::Utc::now();
    println!("退避 {} 秒", delta.num_seconds());
    assert!(delta.num_seconds() >= 25, "退避应约 30 秒，实际 {delta:?}");
    drop_tasks("A4-backoff").await;
}

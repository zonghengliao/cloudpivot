// ============================================================================
// task_model_repair_tests.rs —— 父任务状态巡检
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::state::{ChildSnapshot, TaskStatus};

use super::*;

/// 夹具用户（`biz_task.owner_id / creator_id` 是 NOT NULL + FK `sys_user`）。
const U_ID: &str = "7a8a0000-0000-4000-8000-0000000000e1";
/// 真库用例**串行**执行。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("TASK_DB_URL") else {
                println!("SKIP: 未设置 TASK_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                },
                Err(err) => {
                    println!("SKIP: TASK_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn reset(me: Uuid) {
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password) \
         VALUES (?::uuid, 't13_repair_user', 't13-not-a-real-hash') \
         ON CONFLICT DO NOTHING",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具用户写入失败");
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = NULL WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具清理（断父指针）失败");
    db().execute_with_params(
        "DELETE FROM biz_task WHERE creator_id = ?::uuid",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具清理失败");
}

async fn insert_task(id: Uuid, tag: &str, parent: Option<Uuid>, status: &str, progress: i16, me: Uuid) {
    let affected = db()
        .execute_with_params(
            "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, parent_id, \
                                   status, priority, progress, need_review) \
             VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, ?::uuid, ?, 'P2', ?, false)",
            vec![
                Value::from(&id),
                Value::Text(format!("T13-{tag}-{}", &id.simple().to_string()[..8])),
                Value::Text(format!("T13 巡检 {tag}")),
                Value::from(&me),
                Value::from(&me),
                Value::from(parent.as_ref()),
                Value::Text(status.to_string()),
                Value::I16(progress),
            ],
        )
        .await;
    if let Err(error) = affected {
        panic!("夹具写入失败（tag={tag}）：{error}");
    }
}

async fn status_of(id: Uuid) -> TaskStatus {
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_task WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await
        .expect("查状态失败");
    let raw: String = rows
        .first()
        .expect("任务应存在")
        .get_typed("status")
        .expect("取 status");
    TaskStatus::parse(&raw).expect("状态必须是 5 态之一")
}

/// 只保留本夹具的行（按任务号前缀；巡检是全库扫描，别的模块夹具会混进来）。
fn only_fixture(gaps: Vec<ParentStatusGap>) -> Vec<ParentStatusGap> {
    gaps.into_iter()
        .filter(|gap| gap.task_no.starts_with("T13-R"))
        .collect()
}

// ---------- 纯逻辑：SQL 字面量必须与枚举同口径 ----------
/// `TERMINAL_STATUS_CODES` 是扫描 SQL 里唯一的终态来源 —— 枚举变了这里必须跟着变。
#[test]
fn terminal_codes_match_enum() {
    let mut from_enum: Vec<&str> = TaskStatus::ALL
        .iter()
        .filter(|status| status.is_terminal())
        .map(|status| status.code())
        .collect();
    from_enum.sort_unstable();
    let mut from_const = TERMINAL_STATUS_CODES.to_vec();
    from_const.sort_unstable();
    assert_eq!(
        from_enum, from_const,
        "扫描 SQL 认的终态必须与 TaskStatus::is_terminal 完全一致（否则静默漏扫）"
    );
}

/// 巡检把 `target` 写死成 `REVIEW` 的前提：规则③在这两种父状态下都判 `REVIEW`。
#[test]
fn scan_target_matches_cascade_rule_three() {
    let closed = |status: TaskStatus| ChildSnapshot {
        status,
        progress: 100,
        estimate_hours: None,
    };
    for parent in [TaskStatus::Todo, TaskStatus::Doing] {
        let children = [closed(TaskStatus::Done), closed(TaskStatus::Cancelled)];
        assert_eq!(
            cascade_target(parent, &children),
            Some(TaskStatus::Review),
            "子任务全终态时 {parent:?} 该被推到 REVIEW —— 巡检的 target 依赖这一条"
        );
    }
    // 反向对照：父任务本来就是 REVIEW / 终态 → 不该被算成不一致
    let children = [closed(TaskStatus::Done)];
    assert_eq!(cascade_target(TaskStatus::Review, &children), None);
    assert_eq!(cascade_target(TaskStatus::Done, &children), None);
    // 反向对照：还有一个子任务在跑 → 不该被算成不一致
    assert_eq!(
        cascade_target(
            TaskStatus::Doing,
            &[closed(TaskStatus::Done), closed(TaskStatus::Doing)]
        ),
        None
    );
}

/// `limit` 归一化：缺省 200；0 / 负数 / 超上限都夹进合法区间（巡检不该因参数失效）。
#[test]
fn repair_limit_is_clamped() {
    assert_eq!(normalize_repair_limit(None), REPAIR_SCAN_LIMIT);
    assert_eq!(normalize_repair_limit(Some(0)), 1);
    assert_eq!(normalize_repair_limit(Some(-5)), 1);
    assert_eq!(normalize_repair_limit(Some(50)), 50);
    assert_eq!(normalize_repair_limit(Some(REPAIR_SCAN_LIMIT * 100)), REPAIR_SCAN_LIMIT);
}

// ---------- ⑥ 真库：造 3 个不一致 → 干跑报 3 条 → 实修后都正确 ----------
#[tokio::test]
async fn scan_reports_then_repairs_three_gaps() {
    let _guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return;
    }
    let me = uuid(U_ID);
    reset(me).await;

    // 三个不一致的父任务：子任务全终态，父任务却停在 TODO / DOING
    let gap_parents = [
        "7a8a0000-0000-4000-8000-0000000003a1",
        "7a8a0000-0000-4000-8000-0000000003a2",
        "7a8a0000-0000-4000-8000-0000000003a3",
    ];
    let gap_status = ["DOING", "TODO", "DOING"];
    for (index, parent) in gap_parents.iter().enumerate() {
        insert_task(uuid(parent), &format!("R{index}-父"), None, gap_status[index], 0, me).await;
        insert_task(
            uuid(&format!("7a8a0000-0000-4000-8000-0000000003b{index}")),
            &format!("R{index}-子完成"),
            Some(uuid(parent)),
            "DONE",
            100,
            me,
        )
        .await;
        insert_task(
            uuid(&format!("7a8a0000-0000-4000-8000-0000000003c{index}")),
            &format!("R{index}-子取消"),
            Some(uuid(parent)),
            "CANCELLED",
            0,
            me,
        )
        .await;
    }
    // 两个**不该**被报的对照：父任务已经 REVIEW、父任务还有子任务在跑
    let ok_review = "7a8a0000-0000-4000-8000-0000000003d1";
    insert_task(uuid(ok_review), "R3-父已REVIEW", None, "REVIEW", 100, me).await;
    insert_task(
        uuid("7a8a0000-0000-4000-8000-0000000003e1"),
        "R3-子完成",
        Some(uuid(ok_review)),
        "DONE",
        100,
        me,
    )
    .await;
    let ok_running = "7a8a0000-0000-4000-8000-0000000003d2";
    insert_task(uuid(ok_running), "R4-父有子在跑", None, "DOING", 50, me).await;
    insert_task(
        uuid("7a8a0000-0000-4000-8000-0000000003e2"),
        "R4-子完成",
        Some(uuid(ok_running)),
        "DONE",
        100,
        me,
    )
    .await;
    insert_task(
        uuid("7a8a0000-0000-4000-8000-0000000003e3"),
        "R4-子在跑",
        Some(uuid(ok_running)),
        "DOING",
        30,
        me,
    )
    .await;

    // 干跑：只报不修
    let gaps = only_fixture(
        scan_parent_status_gaps(db(), REPAIR_SCAN_LIMIT)
            .await
            .expect("巡检查询失败"),
    );
    assert_eq!(gaps.len(), 3, "⑥ 干跑必须报出 3 个不一致的父任务");
    let reported: Vec<Uuid> = gaps.iter().map(|gap| gap.task_id).collect();
    for index in 0..gap_parents.len() {
        let parent = uuid(gap_parents[index]);
        assert!(reported.contains(&parent), "⑥ 漏报 {}", gap_parents[index]);
        assert_eq!(
            status_of(parent).await,
            TaskStatus::parse(gap_status[index]).expect("夹具状态非法"),
            "⑥ 干跑不许改库"
        );
    }
    assert!(
        gaps.iter()
            .all(|gap| gap.target == TaskStatus::Review && gap.children == 2),
        "⑥ 每条都该给出 target=REVIEW 与子任务数 2"
    );
    assert!(
        !reported.contains(&uuid(ok_review)) && !reported.contains(&uuid(ok_running)),
        "⑥ 对照行不该被报（父已 REVIEW / 还有子任务在跑）"
    );

    // 实修：从子任务往上跑生产同款级联（巡检接口就是这么修的）
    for gap in &gaps {
        let tx = db().begin().await.expect("开启事务失败");
        let cascades = cascade_parent_status(&tx, gap.sample_child, me)
            .await
            .expect("修复级联失败");
        tx.commit().await.expect("提交事务失败");
        assert!(
            cascades.iter().any(|item| item.task_id == gap.task_id),
            "⑥ 修复必须真的改到父任务 {}（级联返回值里没有它）",
            gap.task_id
        );
    }
    for parent in gap_parents {
        assert_eq!(
            status_of(uuid(parent)).await,
            TaskStatus::Review,
            "⑥ 实修后 {parent} 必须是 REVIEW（落库值）"
        );
    }

    // 再扫一次：同前缀的不一致必须归零
    let after = only_fixture(
        scan_parent_status_gaps(db(), REPAIR_SCAN_LIMIT)
            .await
            .expect("巡检查询失败"),
    );
    assert!(after.is_empty(), "⑥ 修完再扫必须为 0，实际 {}", after.len());
    // 对照行也没被误改
    assert_eq!(status_of(uuid(ok_review)).await, TaskStatus::Review);
    assert_eq!(status_of(uuid(ok_running)).await, TaskStatus::Doing);
}

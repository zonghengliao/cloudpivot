// ============================================================================
// task_model_cascade_tests.rs —— 子任务状态 → 父任务状态级联（规则①②⑨）
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::state::{ChildSnapshot, TaskStatus};

use super::*;

/// 夹具用户（`biz_task.owner_id / creator_id` 是 NOT NULL + FK `sys_user`）。
pub(super) const U_ID: &str = "7a8a0000-0000-4000-8000-0000000000b1";
/// 真库用例**串行**执行：共用一个库，并行跑会互删夹具（还会抢 `DB_LOCK` 之外的全局状态）。
pub(super) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(super) fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 初始化全局数据库（进程内只做一次）；`false` = 没配 `TASK_DB_URL`，用例直接跳过。
pub(super) async fn ensure_db() -> bool {
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
            // 每个 `#[tokio::test]` 都是独立 runtime，而 `db()` 是进程级全局池：
            // 池里的空闲连接绑在创建它的 runtime 上 → 用完即死。见 W1b 的实测。
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

/// 拿锁 + 连库 + 清上一轮夹具（幂等；返回的 guard 必须活到用例结束）。
pub(super) async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    let me = uuid(U_ID);
    // 上一轮若在「级联失败回滚」用例里挂了，触发器会残留 → 先清
    db().exec_sql(
        "DROP TRIGGER IF EXISTS t10_block_review_trg ON biz_task".to_string(),
        Vec::new(),
    )
    .await
    .expect("清触发器失败");
    db().exec_sql(
        "DROP FUNCTION IF EXISTS t10_block_review()".to_string(),
        Vec::new(),
    )
    .await
    .expect("清触发器函数失败");
    db().execute_with_params(
        "INSERT INTO sys_user (id, username, password) \
             VALUES (?::uuid, 't10_cascade_user', 't10-cascade-not-a-real-hash') \
             ON CONFLICT DO NOTHING",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具用户写入失败");
    // 评论先删（`biz_task_comment.task_id` → `biz_task` 是硬外键）
    db().execute_with_params(
        "DELETE FROM biz_task_comment \
              WHERE task_id IN (SELECT id FROM biz_task WHERE creator_id = ?::uuid)",
        vec![Value::from(&me)],
    )
    .await
    .expect("夹具清理（评论）失败");
    // 再断开父指针（自引用 FK 不是 deferrable，直接 DELETE 会被拦）
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
    Some(guard)
}

/// 写一行夹具任务（`task_no` 唯一索引 ⇒ 每个夹具自己的号）。
pub(super) async fn insert_task(id: Uuid, tag: &str, parent: Option<Uuid>, status: &str, me: Uuid) {
    let affected = db()
        .execute_with_params(
            "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, parent_id, \
                                   status, priority, progress, need_review) \
             VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, ?::uuid, ?, 'P2', 0, false)",
            vec![
                Value::from(&id),
                Value::Text(format!("T10-{tag}-{}", &id.simple().to_string()[..8])),
                Value::Text(format!("T10 级联 {tag}")),
                Value::from(&me),
                Value::from(&me),
                Value::from(parent.as_ref()),
                Value::Text(status.to_string()),
            ],
        )
        .await;
    if let Err(error) = affected {
        panic!("夹具写入失败（tag={tag}）：{error}");
    }
}

/// 走生产同款写路径的最小子集：**同一事务**里「改子任务状态 → 级联父任务状态」。
pub(super) async fn change_child(child: Uuid, to: TaskStatus, me: Uuid) -> Vec<StatusCascade> {
    let tx = db().begin().await.expect("开启事务失败");
    let update = StatusUpdate {
        to: to.code().to_string(),
        progress: None,
        done_at: if to == TaskStatus::Done {
            Some(Some(Utc::now()))
        } else {
            None
        },
        reject_reason: None,
        cancel_reason: if to == TaskStatus::Cancelled {
            Some("T10 单测取消".to_string())
        } else {
            None
        },
    };
    let affected = apply_status(&tx, child, &update, me)
        .await
        .expect("改子任务状态失败");
    assert_eq!(affected, 1, "子任务必须存在");
    let out = cascade_parent_status(&tx, child, me)
        .await
        .expect("状态级联失败");
    tx.commit().await.expect("提交事务失败");
    out
}

/// 读库里的状态（证明断言看的是**落库值**，不是内存里的返回值）。
pub(super) async fn status_of(id: Uuid) -> TaskStatus {
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

// ---------- 纯逻辑（不连库）：规则①②⑨的判别 ----------
pub(super) fn snap(status: TaskStatus) -> ChildSnapshot {
    ChildSnapshot {
        status,
        progress: 0,
        estimate_hours: None,
    }
}

/// 规则判别（纯逻辑）：①活动子任务 → DOING、②全部终态 → REVIEW、⑨终态不回退。
#[test]
pub(super) fn cascade_target_follows_the_three_rules() {
    // 规则①：父 TODO + 子任务动起来
    assert_eq!(
        cascade_target(TaskStatus::Todo, &[snap(TaskStatus::Doing)]),
        Some(TaskStatus::Doing)
    );
    assert_eq!(
        cascade_target(TaskStatus::Todo, &[snap(TaskStatus::Review)]),
        Some(TaskStatus::Doing)
    );
    // 规则②：全部终态（DONE / CANCELLED 都算）
    assert_eq!(
        cascade_target(TaskStatus::Todo, &[snap(TaskStatus::Done)]),
        Some(TaskStatus::Review),
        "单子任务完成 = 全部终态 → 进 REVIEW（不是 DOING：100% 的父任务挂在「待开始/进行中」就是老大报的缺陷）"
    );
    assert_eq!(
        cascade_target(
            TaskStatus::Doing,
            &[snap(TaskStatus::Done), snap(TaskStatus::Cancelled)]
        ),
        Some(TaskStatus::Review),
        "CANCELLED 也算终态"
    );
    // 规则⑨：终态不回退
    assert_eq!(
        cascade_target(TaskStatus::Done, &[snap(TaskStatus::Doing)]),
        None,
        "父任务已完成，子任务被拉回进行中 → 父任务不动"
    );
    assert_eq!(
        cascade_target(TaskStatus::Cancelled, &[snap(TaskStatus::Doing)]),
        None
    );
    // 目标 == 现状 / 没有子任务 / 子任务全是 TODO / 父任务 REVIEW 但还没全结束
    assert_eq!(
        cascade_target(TaskStatus::Review, &[snap(TaskStatus::Done)]),
        None,
        "已在 REVIEW，不用重复改"
    );
    assert_eq!(cascade_target(TaskStatus::Todo, &[]), None);
    assert_eq!(
        cascade_target(
            TaskStatus::Todo,
            &[snap(TaskStatus::Todo), snap(TaskStatus::Todo)]
        ),
        None,
        "子任务全在 TODO → 父任务还是待开始"
    );
    assert_eq!(
        cascade_target(
            TaskStatus::Review,
            &[snap(TaskStatus::Done), snap(TaskStatus::Doing)]
        ),
        None
    );
    assert_eq!(
        cascade_target(TaskStatus::Doing, &[snap(TaskStatus::Doing)]),
        None
    );
}

// ---------- ① 单子任务：父 TODO + 子 TODO→DOING → 父 DOING ----------
#[tokio::test]
pub(super) async fn first_active_child_moves_todo_parent_to_doing() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "R1-P", None, "TODO", me).await;
    insert_task(child, "R1-C", Some(parent), "TODO", me).await;

    let out = change_child(child, TaskStatus::Doing, me).await;
    assert_eq!(
        status_of(parent).await,
        TaskStatus::Doing,
        "规则①：父任务必须自动进进行中"
    );
    assert_eq!(out.len(), 1, "只有一层祖先");
    assert_eq!(out[0].task_id, parent);
    assert_eq!(out[0].from, TaskStatus::Todo);
    assert_eq!(out[0].to, TaskStatus::Doing);
    assert_eq!(out[0].reason, CASCADE_REASON_FIRST_ACTIVE);
    assert!(!out[0].all_terminal, "子任务还在进行中");
    // 时间线留痕：父任务上必须有一条「系统自动级联」的 STATUS_CHANGE 评论。
    // 注：`sys_change_log` 只有 `business/changelog/` 能读（C7 G1 门禁），
    // 变更日志的端到端断言放在 `scripts/verify/t10_cascade.sh`（走 HTTP 接口）。
    let comments = db()
        .find_with_params(
            "SELECT content FROM biz_task_comment \
              WHERE task_id = ?::uuid AND comment_type = 'STATUS_CHANGE'",
            vec![Value::from(&parent)],
        )
        .await
        .expect("查父任务评论失败");
    assert_eq!(comments.len(), 1, "级联改状态必须留一条 STATUS_CHANGE 评论");
    let content: String = comments[0].get_typed("content").expect("取 content");
    assert!(
        content.contains("TODO") && content.contains("DOING") && content.contains("自动级联"),
        "评论要写清 from → to 与触发原因：{content}"
    );
}

// ---------- ② 单子任务：父 TODO + 子 TODO→DONE → 全部终态 → 父 REVIEW ----------
//
// 卡面用例②写的是「父变 DOING（不是 REVIEW）」，但它与卡面规则②「所有子任务都到终态
// → 父任务进 REVIEW」在「只有 1 个子任务」时直接冲突（1 个子任务完成 = 全部完成）。
// 本卡按**规则②**落地：单子任务完成 → 父任务进 REVIEW —— 否则会出现「父任务进度 100%
// 却挂在待开始/进行中」，正是老大这次报的缺陷本身。已在看板留言说明。
#[tokio::test]
pub(super) async fn single_child_done_sends_parent_to_review() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "R2-P", None, "TODO", me).await;
    insert_task(child, "R2-C", Some(parent), "TODO", me).await;

    let out = change_child(child, TaskStatus::Done, me).await;
    assert_eq!(
        status_of(parent).await,
        TaskStatus::Review,
        "唯一子任务完成 = 全部终态 → 父任务进待验收（人工点 DONE 才结项）"
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].to, TaskStatus::Review);
    assert_eq!(out[0].reason, CASCADE_REASON_ALL_TERMINAL);
    assert!(out[0].all_terminal);
}

// ---------- ③ 双子任务：子1 DONE、子2 仍 TODO → 父 DOING（不进 REVIEW） ----------
#[tokio::test]
pub(super) async fn partially_done_children_keep_parent_in_doing() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let c1 = Uuid::now_v7();
    let c2 = Uuid::now_v7();
    insert_task(parent, "R3-P", None, "TODO", me).await;
    insert_task(c1, "R3-C1", Some(parent), "TODO", me).await;
    insert_task(c2, "R3-C2", Some(parent), "TODO", me).await;

    let out = change_child(c1, TaskStatus::Done, me).await;
    assert_eq!(
        status_of(parent).await,
        TaskStatus::Doing,
        "还有子任务没结束 → 不进 REVIEW"
    );
    assert_eq!(out[0].to, TaskStatus::Doing);
    assert_eq!(out[0].reason, CASCADE_REASON_FIRST_ACTIVE);
    assert!(!out[0].all_terminal);
    assert_eq!(
        status_of(c2).await,
        TaskStatus::Todo,
        "另一条子任务不该被动"
    );
}

// ---------- ④ 双子任务全部 DONE → 父进 REVIEW ----------
#[tokio::test]
pub(super) async fn all_children_done_sends_parent_to_review() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let c1 = Uuid::now_v7();
    let c2 = Uuid::now_v7();
    insert_task(parent, "R4-P", None, "TODO", me).await;
    insert_task(c1, "R4-C1", Some(parent), "TODO", me).await;
    insert_task(c2, "R4-C2", Some(parent), "TODO", me).await;

    let first = change_child(c1, TaskStatus::Done, me).await;
    assert_eq!(
        first[0].to,
        TaskStatus::Doing,
        "第一个子任务完成时父任务先进进行中"
    );
    let second = change_child(c2, TaskStatus::Done, me).await;
    assert_eq!(
        status_of(parent).await,
        TaskStatus::Review,
        "两个子任务都完成 → 进待验收"
    );
    assert_eq!(second[0].from, TaskStatus::Doing);
    assert_eq!(second[0].to, TaskStatus::Review);
}

// ---------- ⑤ 三子任务全 DONE → 父进 REVIEW ----------
#[tokio::test]
pub(super) async fn three_children_all_done_sends_parent_to_review() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let c1 = Uuid::now_v7();
    let c2 = Uuid::now_v7();
    let c3 = Uuid::now_v7();
    insert_task(parent, "R5-P", None, "TODO", me).await;
    insert_task(c1, "R5-C1", Some(parent), "TODO", me).await;
    insert_task(c2, "R5-C2", Some(parent), "TODO", me).await;
    insert_task(c3, "R5-C3", Some(parent), "TODO", me).await;

    for child in [c1, c2] {
        change_child(child, TaskStatus::Done, me).await;
        assert_eq!(
            status_of(parent).await,
            TaskStatus::Doing,
            "还有子任务没结束 → 只能在进行中"
        );
    }
    change_child(c3, TaskStatus::Done, me).await;
    assert_eq!(status_of(parent).await, TaskStatus::Review);
    assert_eq!(status_of(c3).await, TaskStatus::Done);
}

// ---------- ⑥ 子任务 CANCELLED 也算终态 → 全部终态后父进 REVIEW ----------
#[tokio::test]
pub(super) async fn cancelled_child_counts_as_terminal() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let c1 = Uuid::now_v7();
    let c2 = Uuid::now_v7();
    insert_task(parent, "R6-P", None, "TODO", me).await;
    insert_task(c1, "R6-C1", Some(parent), "TODO", me).await;
    insert_task(c2, "R6-C2", Some(parent), "TODO", me).await;

    change_child(c1, TaskStatus::Done, me).await;
    assert_eq!(status_of(parent).await, TaskStatus::Doing);
    change_child(c2, TaskStatus::Cancelled, me).await;
    assert_eq!(
        status_of(parent).await,
        TaskStatus::Review,
        "取消也是终态：DONE + CANCELLED = 全部结束 → 进待验收"
    );
}

// ---------- ⑦ 三层：孙 → 子 → 父（祖父跟着更新） ----------
#[tokio::test]
pub(super) async fn three_levels_cascade_upward() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let grand = Uuid::now_v7();
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(grand, "R7-G", None, "TODO", me).await;
    insert_task(parent, "R7-P", Some(grand), "TODO", me).await;
    insert_task(child, "R7-C", Some(parent), "TODO", me).await;

    let out = change_child(child, TaskStatus::Done, me).await;
    assert_eq!(out.len(), 2, "两层祖先都要被评估：{out:?}");
    // 第一层：子任务全部终态 → 子任务（父）进 REVIEW
    assert_eq!(out[0].task_id, parent);
    assert_eq!(out[0].to, TaskStatus::Review);
    // 第二层：父任务（祖父）的子任务动了 → 从 TODO 进 DOING
    assert_eq!(out[1].task_id, grand);
    assert_eq!(out[1].from, TaskStatus::Todo);
    assert_eq!(out[1].to, TaskStatus::Doing);
    assert_eq!(status_of(parent).await, TaskStatus::Review);
    assert_eq!(status_of(grand).await, TaskStatus::Doing);
}

// ---------- ⑧ 防环：手工造脏数据 A.parent=B / B.parent=A → 不死循环 ----------
#[tokio::test]
pub(super) async fn dirty_cycle_stops_cascade() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(a, "R8-A", None, "TODO", me).await;
    insert_task(b, "R8-B", Some(a), "TODO", me).await;
    insert_task(child, "R8-C", Some(a), "TODO", me).await;
    // A → B 之后再回填 B → A（`ck_biz_task_parent_not_self` 只拦自指，拦不住环）
    db().execute_with_params(
        "UPDATE biz_task SET parent_id = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&b), Value::from(&a)],
    )
    .await
    .expect("夹具造环失败");

    // 这条用例能跑完本身就是断言（成环时它会不返回）
    let out = change_child(child, TaskStatus::Doing, me).await;
    assert_eq!(out.len(), 2, "环上两个任务各被评估一次就该停：{out:?}");
    assert_eq!(status_of(a).await, TaskStatus::Doing);
    assert_eq!(status_of(b).await, TaskStatus::Doing);
}

// ---------- ⑨ 父任务已 DONE，子任务又被改成 DOING → 父不动（终态不回退） ----------
#[tokio::test]
pub(super) async fn terminal_parent_never_rolls_back() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "R9-P", None, "DONE", me).await;
    insert_task(child, "R9-C", Some(parent), "DONE", me).await;

    let out = change_child(child, TaskStatus::Doing, me).await;
    assert!(out.is_empty(), "父任务已是终态 → 不做任何级联：{out:?}");
    assert_eq!(status_of(parent).await, TaskStatus::Done, "终态不回退");
}

// ---------- ⑩ 事务性：级联失败 → 子任务状态也回滚 ----------
#[tokio::test]
pub(super) async fn cascade_failure_rolls_back_child_change() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let me = uuid(U_ID);
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    insert_task(parent, "R10-P", None, "TODO", me).await;
    insert_task(child, "R10-C", Some(parent), "TODO", me).await;

    // 让「父任务进 REVIEW」这一步必失败：BEFORE UPDATE 触发器只挂在父任务那一行
    db().exec_sql(
        "CREATE OR REPLACE FUNCTION t10_block_review() RETURNS trigger AS $$ \
             BEGIN \
               IF NEW.status = 'REVIEW' THEN \
                 RAISE EXCEPTION 'T10 单测：禁止父任务进 REVIEW'; \
               END IF; \
               RETURN NEW; \
             END; \
             $$ LANGUAGE plpgsql"
            .to_string(),
        Vec::new(),
    )
    .await
    .expect("建触发器函数失败");
    db().exec_sql(
        format!(
            "CREATE TRIGGER t10_block_review_trg BEFORE UPDATE ON biz_task \
                 FOR EACH ROW WHEN (OLD.id = '{parent}'::uuid) \
                 EXECUTE FUNCTION t10_block_review()"
        ),
        Vec::new(),
    )
    .await
    .expect("建触发器失败");

    let tx = db().begin().await.expect("开启事务失败");
    let update = StatusUpdate {
        to: TaskStatus::Done.code().to_string(),
        progress: None,
        done_at: Some(Some(Utc::now())),
        reject_reason: None,
        cancel_reason: None,
    };
    apply_status(&tx, child, &update, me)
        .await
        .expect("子任务状态先写成功");
    let error = cascade_parent_status(&tx, child, me)
        .await
        .expect_err("父任务进 REVIEW 必须失败");
    println!("级联失败（预期）：{error}");
    tx.rollback().await.expect("回滚事务失败");

    assert_eq!(
        status_of(child).await,
        TaskStatus::Todo,
        "级联失败 → 子任务状态必须一起回滚（宁可没改成，也不接受父任务状态错着）"
    );
    assert_eq!(status_of(parent).await, TaskStatus::Todo, "父任务保持原状");

    db().exec_sql(
        "DROP TRIGGER IF EXISTS t10_block_review_trg ON biz_task".to_string(),
        Vec::new(),
    )
    .await
    .expect("删触发器失败");
    db().exec_sql(
        "DROP FUNCTION IF EXISTS t10_block_review()".to_string(),
        Vec::new(),
    )
    .await
    .expect("删触发器函数失败");
}

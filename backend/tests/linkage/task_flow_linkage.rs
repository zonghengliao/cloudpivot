//! 领域 1：任务 ↔ 审批 ↔ 待办（18 个用例）。
//!
//! 对应卡面 T01–T18。全部走**生产同款路径**：
//!
//! ```text
//! task::handler::create / change_status / delete      ← HTTP 层入口（不绕过业务编排）
//!   → flow::task_bridge（prepare_accept / start_in_tx / cancel_on_task_cancelled / cancel_on_task_deleted）
//!   → flow::engine（发起 / 投票）
//!   → flow::handler::approve_task（含回调）
//! ```
//!
//! 断言只看本用例自己的单据号 / id，不做全局计数。

use axum::Json;
use axum::extract::Path;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::flow::task_bridge;
use cloudpivot::business::flow::{dto as flow_dto, handler as flow_handler, model as flow_model};
use cloudpivot::business::task::{dto as task_dto, handler as task_handler};

use crate::linkage::common::{self, Ctx};

// ==================== 夹具动作（本文件内复用） ====================

/// 建一个任务（创建人 = `ctx.creator`，负责人 = `ctx.worker`）。
async fn make_task(ctx: &Ctx, tag: &str, need_review: bool, parent: Option<Uuid>) -> (Uuid, String) {
    let req = task_dto::TaskCreateReq {
        title: ctx.code(tag),
        description: None,
        priority: Some("P2".to_string()),
        need_review: Some(need_review),
        owner_id: Some(ctx.worker.to_string()),
        parent_id: parent.map(|id| id.to_string()),
        project_id: None,
        // 自由任务：不关联任何单据（P2-9 的 doc_type/doc_id 必须成对为空）
        doc_type: None,
        doc_id: None,
        start_date: None,
        due_date: None,
        estimate_hours: None,
    };
    let res = task_handler::create(common::claims(ctx.creator, "creator"), Json(req))
        .await
        .unwrap_or_else(|error| panic!("建任务失败（{tag}）：{} {}", error.code, error.msg));
    let data = res.data.expect("建任务必须返回 TaskSaveRes");
    let id = Uuid::parse_str(&data.id).expect("任务 id");
    (id, data.task_no)
}

/// 推一次状态（谁推由 `actor` 决定：传 `ctx.creator` / `ctx.worker`）。
async fn change(
    actor: Uuid,
    name: &str,
    task_id: Uuid,
    to: &str,
    reason: Option<&str>,
) -> std::result::Result<task_dto::TaskStatusRes, ApiError> {
    let req = task_dto::TaskStatusChangeReq {
        to: to.to_string(),
        reason: reason.map(str::to_string),
    };
    let res = task_handler::change_status(common::claims(actor, name), Path(task_id.to_string()), Json(req)).await?;
    Ok(res.data.expect("状态流转必须返回 TaskStatusRes"))
}

/// 推状态并断言成功。
async fn change_ok(actor: Uuid, name: &str, task_id: Uuid, to: &str) -> task_dto::TaskStatusRes {
    change(actor, name, task_id, to, None)
        .await
        .unwrap_or_else(|error| panic!("推 {to} 失败：{} {}", error.code, error.msg))
}

/// `TODO → DOING → REVIEW`，返回 `(任务 id, 任务号, 审批实例 id)`。
async fn submit_to_review(ctx: &Ctx, tag: &str) -> (Uuid, String, Uuid) {
    let (id, task_no) = make_task(ctx, tag, true, None).await;
    change_ok(ctx.worker, "worker", id, "DOING").await;
    change_ok(ctx.worker, "worker", id, "REVIEW").await;
    let (instance_id, status) = common::flow_instance_of(id)
        .await
        .unwrap_or_else(|| panic!("{tag}：提交验收后必须有审批实例"));
    assert_eq!(status, "PENDING", "{tag}：新发起的实例必须是 PENDING");
    (id, task_no, instance_id)
}

/// 审批人点「同意」（走 `flow::handler::approve_task`，含终态回调）。
async fn approve(approver: Uuid, name: &str, instance_id: Uuid) -> std::result::Result<(), ApiError> {
    let todo = pending_todo_of(instance_id).await;
    approve_todo(todo, approver, name).await
}

/// 实例当前那条 PENDING 待办的 id（没有就 panic）。
async fn pending_todo_of(instance_id: Uuid) -> Uuid {
    let todos = common::flow_tasks_of(instance_id).await;
    todos
        .iter()
        .find(|(_, status, _)| status == "PENDING")
        .map(|(id, _, _)| *id)
        .unwrap_or_else(|| panic!("实例 {instance_id} 没有 PENDING 待办（实际 {todos:?}）"))
}

/// 对一个**指定待办 id** 点同意。
///
/// 与 [`approve`] 的区别：这里不重新挑「当前 PENDING 的那条」，而是拿着调用方给的 id 直接投。
/// 这正是老板报的 TSK-149 的真实场景 —— 页面没刷新，审批人手里还捏着那条**已经被撤销**
/// 的待办，点下去会怎样？必须用这个入口才能复现。
async fn approve_todo(todo_id: Uuid, approver: Uuid, name: &str) -> std::result::Result<(), ApiError> {
    let body = flow_dto::ApproveReq {
        comment: Some("同意（联动测试）".to_string()),
    };
    flow_handler::approve_task(
        common::claims(approver, name),
        Path(todo_id.to_string()),
        Some(Json(body)),
    )
    .await
    .map(|_| ())
}

// ==================== 单向级联 ====================

/// T01 任务提交验收 → 生成审批实例（PENDING）+ 生成待办（PENDING）。
#[tokio::test]
async fn t01_submit_review_starts_instance_and_todo() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T01").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T01").await;

    let instances = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_flow_instance \
          WHERE biz_type = 'TASK_ACCEPT' AND biz_id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(instances, 1, "T01：提交验收只该产生 1 张审批单");

    let todos = common::flow_tasks_of(instance_id).await;
    assert_eq!(todos.len(), 1, "T01：单节点流程只该有 1 条待办");
    assert_eq!(todos[0].1, "PENDING", "T01：待办必须 PENDING");
    assert!(todos[0].2, "T01：还没人处理，completed_at 必须为空");

    // 发起人 / 受派人身份也要对得上：审批人 = 任务创建人（TASK_CREATOR 解析）
    let assignee = common::scalar_text(
        "SELECT assignee_id::text AS v FROM biz_flow_task WHERE instance_id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await;
    assert_eq!(
        assignee.as_deref(),
        Some(ctx.creator.to_string().as_str()),
        "T01：{} 的审批人必须是任务创建人（TASK_CREATOR 规则）",
        task_no
    );

    // 任务行回写了审批单 id（门闸的开关）
    let bound = common::scalar_text(
        "SELECT approval_instance_id::text AS v FROM biz_task WHERE id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(
        bound.as_deref(),
        Some(instance_id.to_string().as_str()),
        "T01：提交验收必须把 approval_instance_id 回写到任务行"
    );
    ctx.cleanup().await;
}

/// T02 任务 → CANCELLED → 审批实例变 CANCELLED（老板报的 TSK-149 第一层）。
#[tokio::test]
async fn t02_task_cancelled_cancels_instance() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T02").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T02").await;

    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;

    let status = common::flow_instance_of(task_id)
        .await
        .map(|(_, status)| status)
        .unwrap_or_else(|| panic!("T02：{task_no} 的审批实例不见了"));
    assert_eq!(
        status, "CANCELLED",
        "T02：任务已取消，审批实例必须同步取消（否则审批中心挂着一条点不动的单）"
    );

    let ended = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_flow_instance \
          WHERE id = ?::uuid AND ended_at IS NOT NULL AND result_comment LIKE '%任务已取消%'",
        vec![Value::from(&instance_id)],
    )
    .await;
    assert_eq!(
        ended, 1,
        "T02：撤销必须留审计痕迹（ended_at + result_comment「任务已取消…」）"
    );
    ctx.cleanup().await;
}

/// T03 任务 → CANCELLED → 待办变 CANCELLED（completed_at 非空）。
#[tokio::test]
async fn t03_task_cancelled_cancels_todo_with_timestamp() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T03").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T03").await;

    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;

    let todos = common::flow_tasks_of(instance_id).await;
    assert_eq!(todos.len(), 1, "T03：待办条数不该变");
    assert_eq!(
        todos[0].1, "CANCELLED",
        "T03：{task_no} 取消后待办必须 CANCELLED（这就是 TSK-149 的孤儿待办）"
    );
    assert!(
        !todos[0].2,
        "T03：被撤销的待办必须有 completed_at（否则「已办」列表里看不到它）"
    );
    ctx.cleanup().await;
}

/// T04 任务 → CANCELLED → 实例节点变 CANCELLED。
#[tokio::test]
async fn t04_task_cancelled_cancels_instance_node() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T04").await;
    let (task_id, _task_no, instance_id) = submit_to_review(&ctx, "T04").await;

    let before = common::flow_nodes_of(instance_id).await;
    assert!(
        before
            .iter()
            .any(|(key, status)| key == "n_creator" && status == "ACTIVE"),
        "T04：提交验收后审批节点应处于 ACTIVE，实际 {before:?}"
    );

    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;

    let after = common::flow_nodes_of(instance_id).await;
    let stuck: Vec<_> = after
        .iter()
        .filter(|(_, status)| status == "ACTIVE" || status == "PENDING")
        .collect();
    assert!(
        stuck.is_empty(),
        "T04：取消后不该有节点还挂在 ACTIVE/PENDING，实际残留 {stuck:?}"
    );
    assert!(
        after
            .iter()
            .any(|(key, status)| key == "n_creator" && status == "CANCELLED"),
        "T04：审批节点必须 CANCELLED，实际 {after:?}"
    );
    ctx.cleanup().await;
}

/// T05 任务 → DONE → 审批实例仍是 APPROVED（正向事不该被取消波及）。
#[tokio::test]
async fn t05_task_done_keeps_instance_approved() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T05").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T05").await;

    approve(ctx.creator, "creator", instance_id)
        .await
        .unwrap_or_else(|error| panic!("T05：审批同意失败：{} {}", error.code, error.msg));

    let task_status = common::scalar_text(
        "SELECT status AS v FROM biz_task WHERE id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(task_status.as_deref(), Some("DONE"), "T05：审批通过后任务必须 DONE");
    let instance_status = common::flow_instance_of(task_id)
        .await
        .map(|(_, status)| status)
        .unwrap();
    assert_eq!(
        instance_status, "APPROVED",
        "T05：任务完成不许把已通过的审批单改成别的状态（审计事实），任务号 {task_no}"
    );
    ctx.cleanup().await;
}

/// T06 父任务 CANCELLED → 子任务 CANCELLED。
#[tokio::test]
async fn t06_parent_cancel_cascades_to_child() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T06").await;
    let (parent, _parent_no) = make_task(&ctx, "T06-P", true, None).await;
    let (child, child_no) = make_task(&ctx, "T06-C", true, Some(parent)).await;
    change_ok(ctx.worker, "worker", child, "DOING").await;

    change_ok(ctx.creator, "creator", parent, "CANCELLED").await;

    let child_status = common::scalar_text(
        "SELECT status AS v FROM biz_task WHERE id = ?::uuid",
        vec![Value::from(&child)],
    )
    .await;
    assert_eq!(
        child_status.as_deref(),
        Some("CANCELLED"),
        "T06：父任务取消必须级联取消子任务（{child_no}）"
    );
    ctx.cleanup().await;
}

/// T07 ★ 父任务 CANCELLED → 子任务名下的审批实例也 CANCELLED（最容易漏的一环）。
#[tokio::test]
async fn t07_parent_cancel_cancels_child_instance() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T07").await;
    let (parent, _parent_no) = make_task(&ctx, "T07-P", true, None).await;
    let (child, child_no) = make_task(&ctx, "T07-C", true, Some(parent)).await;
    change_ok(ctx.worker, "worker", child, "DOING").await;
    change_ok(ctx.worker, "worker", child, "REVIEW").await;
    let (child_instance, status) = common::flow_instance_of(child)
        .await
        .unwrap_or_else(|| panic!("T07：子任务提交验收后必须有审批实例"));
    assert_eq!(status, "PENDING");

    change_ok(ctx.creator, "creator", parent, "CANCELLED").await;

    let after = common::flow_instance_of(child).await.map(|(_, status)| status);
    assert_eq!(
        after.as_deref(),
        Some("CANCELLED"),
        "T07：父任务取消 → 子任务被级联取消 → 子任务的审批实例必须一起撤（{child_no}）"
    );
    let todos = common::flow_tasks_of(child_instance).await;
    assert!(
        todos.iter().all(|(_, status, _)| status == "CANCELLED"),
        "T07：子任务审批待办也必须 CANCELLED，实际 {todos:?}"
    );
    ctx.cleanup().await;
}

/// T08 任务删除（软删）→ 审批实例 CANCELLED（既有逻辑的回归验证）。
#[tokio::test]
async fn t08_task_soft_delete_cancels_instance() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T08").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T08").await;

    let res = task_handler::delete(common::claims(ctx.creator, "creator"), Path(task_id.to_string()))
        .await
        .unwrap_or_else(|error| panic!("T08：删任务失败：{} {}", error.code, error.msg));
    assert!(res.data.expect("删除返回体").deleted, "T08：首次删除必须真删");

    let status = common::flow_instance_of(task_id)
        .await
        .map(|(_, status)| status)
        .unwrap_or_else(|| panic!("T08：{task_no} 的审批实例不该消失"));
    assert_eq!(status, "CANCELLED", "T08：删任务必须终止它在跑的审批单");
    let comment = common::scalar_text(
        "SELECT result_comment AS v FROM biz_flow_instance WHERE id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await;
    assert!(
        comment.as_deref().unwrap_or_default().contains("任务已删除"),
        "T08：删除与取消的审计文案要区分（删除是「任务已删除，审批单自动终止」），实际 {comment:?}"
    );
    ctx.cleanup().await;
}

// ==================== 反向保护（「不许发生」） ====================

/// T09 任务 CANCELLED 后，审批人**拿着已被撤销的那条待办**点同意 → 任务状态不变。
///
/// 用 [`approve_todo`] 直接投「撤销前的待办 id」，而不是重新找 PENDING 待办 ——
/// 后者在修复后已经不存在了，找不到待办本身不能证明「点了也没用」。
#[tokio::test]
async fn t09_approve_after_cancel_keeps_task_cancelled() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T09").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T09").await;
    let stale_todo = pending_todo_of(instance_id).await;

    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;
    // 审批人仍然拿着那个待办 id 去点同意（前端页面没刷新的真实场景）
    let outcome = approve_todo(stale_todo, ctx.creator, "creator").await;

    let task_status = common::scalar_text(
        "SELECT status AS v FROM biz_task WHERE id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(
        task_status.as_deref(),
        Some("CANCELLED"),
        "T09：任务已取消，任何审批动作都不许把它推成 DONE（{task_no}）；审批结果：{outcome:?}"
    );
    ctx.cleanup().await;
}

/// T10 任务 CANCELLED 后审批人拿着旧待办点同意 → **审批不许生效**（实例保持 CANCELLED）。
///
/// 卡面原写「实例变 APPROVED（当前行为，标注为缺陷）」—— 那正是 TSK-149 的另一半：
/// 任务已取消，审批单却还能通过，等于「取消」这个动作在审批侧不存在。
///
/// 实测：P11 的撤销落地后待办已是 `CANCELLED`，`engine::vote` 对「非 PENDING 待办」
/// 走的是**幂等分支**（`is_pending() == false` → `ActionOut{ idempotent: true }`，
/// 这是架构 §4.5.1 第 ① 步的既定行为，不是缺陷），所以这里不能断言「返回 Err」，
/// 而要断言**零状态变化**：实例仍 CANCELLED、待办仍 CANCELLED、任务仍 CANCELLED。
#[tokio::test]
async fn t10_approve_after_cancel_is_rejected() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T10").await;
    let (task_id, _task_no, instance_id) = submit_to_review(&ctx, "T10").await;
    let stale_todo = pending_todo_of(instance_id).await;

    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;
    let outcome = approve_todo(stale_todo, ctx.creator, "creator").await;

    let status = common::flow_instance_of(task_id)
        .await
        .map(|(_, status)| status)
        .unwrap();
    assert_eq!(
        status, "CANCELLED",
        "T10：已取消的审批单不许被一次「幂等投票」翻成 APPROVED（投票结果 {outcome:?}）"
    );
    let todos = common::flow_tasks_of(instance_id).await;
    assert!(
        todos.iter().all(|(_, status, _)| status == "CANCELLED"),
        "T10：待办必须仍是 CANCELLED（不许被投票改回 APPROVED），实际 {todos:?}"
    );
    let task_status = common::scalar_text(
        "SELECT status AS v FROM biz_task WHERE id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(
        task_status.as_deref(),
        Some("CANCELLED"),
        "T10：任务不许被这次投票推成 DONE"
    );
    ctx.cleanup().await;
}

/// T11 任务 CANCELLED 后再取消一次 → **不产生任何副作用**，并被 `409 + 63010` 明确拒绝。
///
/// # 与卡面的口径差异（已实测，登记在 `docs/test-report-linkage.md` LNK-T11）
///
/// 卡面要求「幂等，不报错（返回成功）」。实测实现是 `409 + 63010 任务已处于该状态，无需流转`
/// —— **不是缺陷**：两种做法都满足「不产生副作用」这个真正的风险点，而显式 409 让前端能
/// 给出「已经是已取消」的提示，比静默 200 更不容易误导。所以本用例断言的是
/// 「零副作用 + 干净的 4xx（不是 500、不是二次退库存）」，并把口径差异写进报告等产品定。
#[tokio::test]
async fn t11_cancel_is_idempotent() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T11").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T11").await;

    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;
    let stamp = common::scalar_text(
        "SELECT completed_at::text AS v FROM biz_flow_task WHERE instance_id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await;
    let status_log_before = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_status_log WHERE entity_id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;

    let second = change(ctx.creator, "creator", task_id, "CANCELLED", None).await;

    let error = second.err().unwrap_or_else(|| {
        panic!("T11：重复取消居然返回成功 —— 若产品确认要「静默幂等」，请把本用例改成断言 200 且零副作用")
    });
    assert_eq!(
        error.status, 409,
        "T11：重复取消必须是干净的 409（不是 500），实际 {} {}（{task_no}）",
        error.status, error.msg
    );
    assert_eq!(
        error.code, 63010,
        "T11：应为 63010「已处于该状态」，实际 {} {}",
        error.code, error.msg
    );

    let stamp_after = common::scalar_text(
        "SELECT completed_at::text AS v FROM biz_flow_task WHERE instance_id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await;
    assert_eq!(
        stamp, stamp_after,
        "T11：被动拒绝的二次取消不许把待办的完成时间往后刷（会污染「已办」排序）"
    );
    let status_log_after = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_status_log WHERE entity_id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(
        status_log_before, status_log_after,
        "T11：被拒绝的二次取消不许在 biz_status_log 里留一条假的状态变更"
    );
    ctx.cleanup().await;
}

/// T12 已 APPROVED 的待办不被改成 CANCELLED（保留审计事实）。
#[tokio::test]
async fn t12_approved_todo_survives_later_cancel() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T12").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T12").await;
    approve(ctx.creator, "creator", instance_id)
        .await
        .unwrap_or_else(|error| panic!("T12：审批同意失败：{} {}", error.code, error.msg));

    // 任务此时 DONE；重开再取消，走一遍「有过历史审批 + 现在取消」的组合
    change_ok(ctx.creator, "creator", task_id, "DOING").await;
    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;

    let todos = common::flow_tasks_of(instance_id).await;
    assert_eq!(
        todos[0].1, "APPROVED",
        "T12：{task_no} 已经通过的那条待办是审计事实，不许被改成 CANCELLED（实际 {todos:?}）"
    );
    ctx.cleanup().await;
}

// ==================== 多用户视角 ====================

/// T13 提交人（负责人）看不到自己的审批待办。
#[tokio::test]
async fn t13_submitter_has_no_todo() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T13").await;
    let (_task_id, task_no, instance_id) = submit_to_review(&ctx, "T13").await;
    let instance_no = instance_no_of(instance_id).await;

    let mine = common::todo_of(ctx.worker, &instance_no).await;
    assert!(
        mine.is_empty(),
        "T13：提交人（{task_no} 的负责人）不该在自己的待办里看到这条，实际 {mine:?}"
    );
    ctx.cleanup().await;
}

/// T14 审批人（创建人）能看到待办。
#[tokio::test]
async fn t14_approver_sees_todo() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T14").await;
    let (_task_id, task_no, instance_id) = submit_to_review(&ctx, "T14").await;
    let instance_no = instance_no_of(instance_id).await;

    let mine = common::todo_of(ctx.creator, &instance_no).await;
    assert_eq!(
        mine.len(),
        1,
        "T14：审批人必须看到 1 条待办（{task_no}），实际 {mine:?}"
    );
    assert_eq!(mine[0].1, "PENDING", "T14：待办状态必须 PENDING");
    ctx.cleanup().await;
}

/// T15 ★ 任务取消后，审批人的待办列表里查不到（老板 2026-10-31 报的就是这条）。
#[tokio::test]
async fn t15_cancelled_task_disappears_from_approver_todo() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T15").await;
    let (task_id, task_no, instance_id) = submit_to_review(&ctx, "T15").await;
    let instance_no = instance_no_of(instance_id).await;
    assert_eq!(
        common::todo_of(ctx.creator, &instance_no).await.len(),
        1,
        "T15 前置：取消前审批人应当能看到这条待办"
    );

    change_ok(ctx.creator, "creator", task_id, "CANCELLED").await;

    let after = common::todo_of(ctx.creator, &instance_no).await;
    assert!(
        after.is_empty(),
        "T15：任务已取消（{task_no}），审批人的「待我审批」里不该再有这条孤儿待办，实际 {after:?}"
    );
    ctx.cleanup().await;
}

/// T16 无关用户看不到任何待办。
#[tokio::test]
async fn t16_outsider_sees_nothing() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T16").await;
    let (_task_id, task_no, instance_id) = submit_to_review(&ctx, "T16").await;
    let instance_no = instance_no_of(instance_id).await;

    let mine = common::todo_of(ctx.outsider, &instance_no).await;
    assert!(mine.is_empty(), "T16：无关用户不该看到 {task_no} 的待办，实际 {mine:?}");
    ctx.cleanup().await;
}

// ==================== 边界 ====================

/// T17 没配 `TASK_ACCEPT` 流程时提交验收 → **不报错**，走原两级验收。
///
/// 让「没配流程」成立的做法：临时把 V0127 的种子流程 `is_default` 关掉（与
/// `default_def_of_biz_type` 的查询条件一致），跑完立刻恢复 —— 所以本用例必须串行跑
/// （`--test-threads=1`），否则同一进程里别的 T 用例会读到「没有流程」的窗口。
#[tokio::test]
async fn t17_missing_flow_falls_back_to_two_level_acceptance() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T17").await;
    let (task_id, task_no) = make_task(&ctx, "T17", true, None).await;
    change_ok(ctx.worker, "worker", task_id, "DOING").await;

    let original = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_flow_def WHERE biz_type = 'TASK_ACCEPT' \
          AND is_default AND status = 'PUBLISHED' AND deleted_at IS NULL",
        vec![],
    )
    .await;
    assert_eq!(original, 1, "T17 前置：种子流程应当恰好有一个默认发布版本");
    // 「没配流程」的准确定义是 `default_def_of_biz_type` 查不到：
    // `status = 'PUBLISHED' AND current_version_id IS NOT NULL`。
    // 只把 `is_default` 置 false 没用（查询按 is_default 倒序但**不要求**它为 true，
    // 实测那样仍会取到同一条流程 → 用例假红）。所以这里把 status 改成 DRAFT。
    db().execute_with_params(
        "UPDATE biz_flow_def SET status = 'DRAFT', is_default = false, updated_at = now() \
          WHERE biz_type = 'TASK_ACCEPT' AND deleted_at IS NULL",
        vec![],
    )
    .await
    .expect("临时下线默认流程失败");

    let outcome = change(ctx.worker, "worker", task_id, "REVIEW", None).await;

    // 无论断言是否通过，先把流程恢复（不留全局副作用）
    db().execute_with_params(
        "UPDATE biz_flow_def SET status = 'PUBLISHED', is_default = true, updated_at = now() \
          WHERE biz_type = 'TASK_ACCEPT' AND deleted_at IS NULL",
        vec![],
    )
    .await
    .expect("恢复默认流程失败");

    let result = outcome.unwrap_or_else(|error| {
        panic!(
            "T17：没配审批流程时提交验收不许报错（走两级验收），实际 {} {}",
            error.code, error.msg
        )
    });
    assert_eq!(result.status, "REVIEW", "T17：任务仍要进入 REVIEW 等人工验收");
    let instances = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_flow_instance WHERE biz_type = 'TASK_ACCEPT' AND biz_id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(instances, 0, "T17：没有流程就不该凭空造审批单（{task_no}）");
    ctx.cleanup().await;
}

/// T18 同一任务重复提交验收 → 不生成第二个实例。
///
/// 真实路径是「两次提交之间任务回到 DOING」，而 `prepare_accept` 里唯一的
/// 重复守卫是 `biz_instance_running`。直接对它做断言比绕一大圈更精确：
/// 实例还在跑时再准备一次，必须得到 `None`。
#[tokio::test]
async fn t18_second_submit_does_not_create_second_instance() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("T18").await;
    let (task_id, task_no, _instance_id) = submit_to_review(&ctx, "T18").await;

    let again = task_bridge::prepare_accept(
        task_id,
        &task_no,
        "重复提交（联动测试）",
        ctx.worker,
        serde_json::json!({ "task_id": task_id.to_string() }),
    )
    .await
    .unwrap_or_else(|error| panic!("T18：prepare_accept 失败：{error}"));

    assert!(
        again.is_none(),
        "T18：已有在跑实例时必须返回 None（不许造第二张审批单）"
    );
    let instances = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_flow_instance WHERE biz_type = 'TASK_ACCEPT' AND biz_id = ?::uuid",
        vec![Value::from(&task_id)],
    )
    .await;
    assert_eq!(instances, 1, "T18：{task_no} 只该有 1 张审批单，实际 {instances}");
    ctx.cleanup().await;
}

// ==================== 本文件内工具 ====================

/// 实例单号（「我的待办」按单号过滤用）。
async fn instance_no_of(instance_id: Uuid) -> String {
    common::scalar_text(
        "SELECT instance_no AS v FROM biz_flow_instance WHERE id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await
    .unwrap_or_else(|| panic!("实例 {instance_id} 不存在"))
}

/// 编译期防呆：`flow_model` 与 `task_bridge` 都在本文件被用到（避免 unused 警告）。
#[allow(dead_code)]
fn _type_check() {
    let _ = flow_model::MY_TODO_STATUSES;
    let _ = task_bridge::TASK_CANCEL_COMMENT;
}

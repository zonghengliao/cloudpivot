// ============================================================================
// flow_model_cancel.rs —— 终止链路：按节点/实例/业务单撤销待办、流程任务终止（含系统操作人与终止文案）
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::flow_model_base::*;
use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 把某节点其余 `PENDING` 待办置 `CANCELLED`（节点通过 / 驳回 / 退回时）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cancel_pending_of_node(
    ex: &Transaction,
    node_id: Uuid,
    except_task_id: Option<Uuid>,
    comment: &str,
) -> DbResult<u64> {
    let mut sql = String::from(
        "UPDATE biz_flow_task \
            SET status = 'CANCELLED', comment = COALESCE(comment, ?), completed_at = now() \
          WHERE instance_node_id = ?::uuid AND status = 'PENDING'",
    );
    let mut params = vec![Value::Text(comment.to_string()), Value::from(&node_id)];
    if let Some(task_id) = except_task_id {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&task_id));
    }
    ex.execute_with_params(&sql, params).await
}

/// 把整个实例的 `PENDING` 待办置 `CANCELLED`（撤回 / 终止）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn cancel_pending_of_instance(ex: &Transaction, instance_id: Uuid, comment: &str) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_flow_task \
            SET status = 'CANCELLED', comment = COALESCE(comment, ?), completed_at = now() \
          WHERE instance_id = ?::uuid AND status = 'PENDING'",
        vec![Value::Text(comment.to_string()), Value::from(&instance_id)],
    )
    .await
}

/// 业务单据被删除时，终止它名下**所有在跑（`PENDING`）的审批单**（T12）。
///
/// # 为什么需要它
///
/// 任务删除是**软删**（`biz_task.deleted_at`），而 `biz_flow_instance.biz_id` 上
/// 只有 `biz_flow_task` / `biz_flow_instance_node` 的子表 CASCADE，**没有任何东西**
/// 会因为任务软删去动实例 —— 实例于是变成孤儿：`PENDING` 的孤儿一直挂在
/// 「审批待办」里，点进去任务已不存在（线上实测存量 27 条，其中 5 条 PENDING）。
///
/// # 匹配口径（精确到一个业务单据，**绝不批量清全库**）
///
/// `biz_id = $1`（实例创建时写的就是业务主键 UUID）**或** `biz_no = $2`
/// （业务单号，兜住 `biz_id` 为空的历史行）。两个值都由调用方从**同一条**
/// 业务行取，因此不会波及别的单据 —— 单测 `other_biz_instances_untouched` 钉住这一点。
///
/// # 三条批量语句（不是 N 条单行）
///
/// 顺序是**子表 → 节点 → 主表**，且每条 UPDATE 都**同时写结束时间**：
///
/// | 表 | 库约束 | 所以必须同写 |
/// |---|---|---|
/// | `biz_flow_task` | `ck_biz_flow_task_done`：`(status='PENDING') = (completed_at IS NULL)` | `completed_at = now()` |
/// | `biz_flow_instance_node` | 无同名约束（`duration_ms` 由 `activated_at` 现算，口径同 [`cancel_active_nodes`]） | `ended_at = now()` |
/// | `biz_flow_instance` | `ck_biz_flow_instance_end`：`(status='PENDING') = (ended_at IS NULL)` | `ended_at = now()` |
///
/// **只动 `PENDING`**：`APPROVED` / `REJECTED` / `WITHDRAWN` 是已经发生的事实，
/// 保留作审计（卡面红线 4）。`revision + 1` 与 [`bump_instance`] 的 CAS 口径一致，
/// 免得在跑的别的操作拿着旧 revision 把状态覆盖回去。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（调用方的事务整笔回滚：宁可这次删不掉任务，
/// 也不留一个孤儿待办）。
pub async fn cancel_pending_of_biz(
    ex: &impl SqlExecutor,
    biz_id: Uuid,
    biz_no: &str,
    comment: &str,
) -> DbResult<CancelledOfBiz> {
    // ① 审批待办（子表先动：主表翻成 CANCELLED 之后再改待办，语义上就「迟了」）
    let tasks = ex
        .exec_sql(
            "UPDATE biz_flow_task t \
                SET status = 'CANCELLED', comment = COALESCE(t.comment, ?), completed_at = now() \
               FROM biz_flow_instance i \
              WHERE t.instance_id = i.id AND t.status = 'PENDING' \
                AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND (i.biz_id = ?::uuid OR i.biz_no = ?)"
                .to_string(),
            vec![
                Value::Text(comment.to_string()),
                Value::from(&biz_id),
                Value::Text(biz_no.to_string()),
            ],
        )
        .await?;

    // ② 节点（`ACTIVE` / `PENDING` 都是「还没走完」，与 cancel_active_nodes 同口径）
    let nodes = ex
        .exec_sql(
            "UPDATE biz_flow_instance_node n \
                SET status = 'CANCELLED', ended_at = now(), \
                    duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(n.activated_at, now()))) * 1000)::bigint, \
                    updated_at = now() \
               FROM biz_flow_instance i \
              WHERE n.instance_id = i.id AND n.status IN ('ACTIVE','PENDING') \
                AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND (i.biz_id = ?::uuid OR i.biz_no = ?)"
                .to_string(),
            vec![Value::from(&biz_id), Value::Text(biz_no.to_string())],
        )
        .await?;

    // ③ 主表（最后翻状态；`ended_at` 必须与状态同写，否则撞 ck_biz_flow_instance_end）
    let instances = ex
        .exec_sql(
            "UPDATE biz_flow_instance \
                SET status = 'CANCELLED', ended_at = now(), result_comment = ?, \
                    revision = revision + 1, updated_at = now() \
              WHERE status = 'PENDING' AND deleted_at IS NULL \
                AND (biz_id = ?::uuid OR biz_no = ?)"
                .to_string(),
            vec![
                Value::Text(comment.to_string()),
                Value::from(&biz_id),
                Value::Text(biz_no.to_string()),
            ],
        )
        .await?;

    Ok(CancelledOfBiz {
        instances,
        tasks,
        nodes,
    })
}

// ==================== 实例终态 → 未完成任务批量取消（P-FLOW-1B） ====================

/// 流程侧「系统身份」写变更日志时用的操作人名字（卡面指定文案）。
pub const FLOW_CANCEL_OPERATOR_NAME: &str = "流程终止（系统）";

// SCOPE-EXEMPT: 按 `flow_instance_id` 精确取**某一张流程实例**名下的任务（入参来自引擎内部的
// SCOPE-EXEMPT: `biz_flow_instance.id`），是系统身份的级联写，不是面向用户的列表查询。
/// 实例走到终态时，把它名下**没干完**的任务批量置 `CANCELLED`（设计 §4.5 Q2 的第 ④ 个耦合点）。
///
/// # 为什么只动 `TODO / DOING / REVIEW`（**不许动 `DONE`**）
///
/// `task::state::TRANSITIONS` 的 10 条边里**没有 `DONE → CANCELLED`**。已完成的活是业务事实，
/// 行政终止（撤回 / 管理员终止 / 驳回终止）不能把它改写成"取消" —— 那会让完成率统计凭空下降。
/// 这条不是偷懒，是状态机本身的约束（设计 §7.2 硬结论 1）。
///
/// # 为什么走批量 `UPDATE` 而不是 `state::check_transition`
///
/// 三条 cancel 边的 `actors` 只有 `OWNER|CREATOR|ASSIGNER|ADMIN`，`callback_transition`
/// 也只开 `REVIEW → DONE|DOING`。若走 `ActorKind::FlowCallback`，每条都会被 `63010` 挡住，
/// 整笔终止事务回滚 —— 症状是「流程终止不了，因为有一张任务取消不掉」（设计 §7.2 硬结论 2/3）。
/// 所以这里与 [`cancel_pending_of_biz`] 同款：一条 `UPDATE … WHERE status IN (…)`，
/// 变更日志在同一事务里逐条补，`operator_name = [`FLOW_CANCEL_OPERATOR_NAME`]`。
///
/// # 为什么先 `SELECT` 再 `UPDATE`
///
/// PG 的 `RETURNING` 只给**新值**（`status` 已经是 `CANCELLED`），而变更日志的 diff 必须有
/// `from`。两条语句都在调用方的事务里，中间不会被别人插队（`biz_task` 行锁由 `UPDATE` 持有）。
///
/// # 异常情况
///
/// 数据库异常 / 变更日志写不进去 → [`DbError`]（调用方整笔回滚：宁可这次终止不了，
/// 也不接受「实例已终止、任务还挂着」）。
pub async fn cancel_flow_tasks(
    ex: &Transaction,
    instance_id: Uuid,
    instance_no: &str,
    comment: &str,
) -> DbResult<Vec<CancelledFlowTask>> {
    // ① 先取旧状态（`RETURNING` 拿不到 from）
    let rows = ex
        .find_with_params(
            "SELECT t.id, t.task_no, t.status FROM biz_task t \
              WHERE t.flow_instance_id = ?::uuid AND t.deleted_at IS NULL \
                AND t.status IN ('TODO','DOING','REVIEW') \
              ORDER BY t.task_no",
            vec![Value::from(&instance_id)],
        )
        .await?;
    let victims: Vec<CancelledFlowTask> = rows
        .iter()
        .map(|row| {
            Ok(CancelledFlowTask {
                task_id: row.get_typed("id")?,
                task_no: row.get_typed("task_no")?,
                from: row.get_typed("status")?,
            })
        })
        .collect::<DbResult<Vec<_>>>()?;
    if victims.is_empty() {
        return Ok(victims);
    }

    // ② 一条语句批量翻状态（与 `cancel_pending_of_biz` 同款；`DONE` 不在过滤条件里）
    ex.execute_with_params(
        "UPDATE biz_task SET status = 'CANCELLED', cancel_reason = ?, updated_at = now() \
          WHERE flow_instance_id = ?::uuid AND deleted_at IS NULL \
            AND status IN ('TODO','DOING','REVIEW')",
        vec![Value::Text(comment.to_string()), Value::from(&instance_id)],
    )
    .await?;

    // ③ 同事务逐条写变更日志（审计口径：每条任务都要能回答「谁、为什么取消的」）
    for victim in &victims {
        let before = serde_json::json!({ "status": victim.from });
        let after = serde_json::json!({
            "status": "CANCELLED",
            "cancel_reason": comment,
        });
        changelog::record(
            ex,
            &ChangeCtx::api(None, Some(FLOW_CANCEL_OPERATOR_NAME)),
            ChangeTarget::of(
                "biz_task",
                victim.task_id.to_string(),
                Action::Status,
                &format!("流程 {} 已终止，未完成任务被系统取消", instance_no),
            ),
            DiffInput::update(&before, &after),
        )
        .await
        .map_err(|error| DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg)))?;
    }

    tracing::info!(
        %instance_id,
        instance_no,
        cancelled = victims.len() as i64,
        comment,
        "流程实例已终止：未完成任务已批量取消"
    );
    Ok(victims)
}

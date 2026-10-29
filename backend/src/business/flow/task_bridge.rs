//! 任务 ↔ 审批流 的桥（批次 D · D3b）。
//!
//! 依据：`docs/批次D-工作流审批-技术架构.md` §5.2.2（任务与审批的三种关系）。
//!
//! # 为什么需要一个独立的桥文件
//!
//! `task` 与 `flow` 两个模块互不依赖（各自 `mod.rs` 的「与其它模块的边界」都写着「不碰对方」）。
//! D3a 把门闸写成了纯函数（`task::state::flow_gate_blocked`），但**全仓没有任何地方写
//! `biz_task.approval_instance_id`**，于是门闸在生产上恒不触发。本文件是唯一一处的接线口：
//!
//! | 方向 | 时机 | 函数 |
//! |---|---|---|
//! | 任务 → 流程 | `DOING → REVIEW` 提交验收 | [`prepare_accept`] + [`start_in_tx`]（发起 `TASK_ACCEPT` 审批单并回写 `approval_instance_id`） |
//! | 流程 → 任务 | 审批单落终态（`APPROVED` / `REJECTED`） | [`on_instance_settled`]（`FlowCallback` 身份推进任务） |
//! | 任务 → 流程（读） | 每次人工流转 | [`instance_status_of`]（把实例状态喂给门闸） |
//!
//! # 三条不变式
//!
//! 1. **没配流程 ≠ 出错**：`flow_def` 里没有 `TASK_ACCEPT` 的发布流程时，[`prepare_accept`]
//!    返回 `Ok(None)`，任务照老规矩走两级验收（T2 的 `REVIEW → DONE` / `→ DOING` 人工判定）。
//!    这是存量行为，不许因为「没审批」把提交验收变成 500。
//! 2. **发起与状态流转同事务**：[`start_in_tx`] 只在 `task::handler::change_status` 的事务里调用，
//!    审批单建不出来就整笔回滚 —— 否则会出现「任务已待验收、审批单没建、门闸也永远不响」
//!    这种比不接审批更坏的状态。
//! 3. **回调只推进，不反向影响审批**：[`on_instance_settled`] 在审批事务**提交之后**调用，
//!    失败只记 `warn` 日志（审批结论已经落库，不能因为任务侧推进失败而回滚审批单）。
//!
//! # 红线（卡面）
//!
//! `ActorKind::FlowCallback` 只在本文件出现 —— HTTP handler 一律 `ActorKind::User`，
//! 人类请求没有伪造「我是流程回调」的入口。

use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::flow::{engine, model};
use crate::business::task::model as task_model;
use crate::business::task::state::{self, ActorKind, TaskStatus};

/// 任务验收类流程的业务类型（`biz_flow_def.biz_type`，`flow::dto::normalize_biz_type` 归一化后全大写）。
pub const TASK_ACCEPT_BIZ_TYPE: &str = "TASK_ACCEPT";

/// 审批单进入「已通过」时任务该去的地方。
const INSTANCE_APPROVED: &str = "APPROVED";
/// 审批单进入「已驳回」时任务该去的地方。
const INSTANCE_REJECTED: &str = "REJECTED";

/// 审批回调时的操作人名字（写进变更日志，让审计能一眼看出「这条不是人点的」）。
const CALLBACK_OPERATOR_NAME: &str = "审批流程（系统回调）";

// ==================== 读：实例状态 ====================

/// 任务上的 `approval_instance_id` → 审批单状态（门闸的输入）。
///
/// # 为什么不报错
///
/// `None` / 查不到实例 → `Ok(None)`：D3a 的门闸判据是「读不到实例状态 → 不拦」
/// （让任务永久卡在 `REVIEW` 比放行更坏）。实例本身的合法性归 flow 引擎
/// （`65024 FLOW_INSTANCE_NOT_PENDING` 等）自己判定，本函数只负责取状态。
///
/// 真实数据库异常仍然 `Err`（调用方转 500），不把「库挂了」伪装成「没有审批」。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn instance_status_of(approval_instance_id: Option<Uuid>) -> DbResult<Option<String>> {
    let Some(instance_id) = approval_instance_id else {
        return Ok(None);
    };
    Ok(model::find_instance(instance_id)
        .await?
        .map(|row| row.status))
}

/// 审批结论 → 任务目标状态（**纯函数**，便于单测）。
///
/// | 实例状态 | 任务 | 语义 |
/// |---|---|---|
/// | `APPROVED` | `DONE` | 审批通过 = 验收通过 |
/// | `REJECTED` | `DOING` | 审批驳回 = 打回返工 |
/// | 其它（`PENDING` / `CANCELLED` / `WITHDRAWN`） | `None` | 不推进 |
pub fn settle_target(instance_status: &str) -> Option<TaskStatus> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        INSTANCE_APPROVED => Some(TaskStatus::Done),
        INSTANCE_REJECTED => Some(TaskStatus::Doing),
        _ => None,
    }
}

// ==================== 写：任务 → 流程（发起） ====================

/// 一次「提交验收即发起审批」的完整参数（在事务外准备好，事务内只做写入）。
#[derive(Debug)]
pub struct AcceptStart {
    /// 预生成的实例 id（事务内 `engine::start` 用它建实例，同时回写任务行）。
    pub instance_id: Uuid,
    /// 引擎发起参数。
    pub req: engine::StartReq,
}

/// 任务要提交验收时，准备一张 `TASK_ACCEPT` 审批单（**不写库**）。
///
/// # 返回值
///
/// - `Ok(Some(..))`：配了已发布的 `TASK_ACCEPT` 流程 → 调用方在事务里跑 [`start_in_tx`]；
/// - `Ok(None)`：**没配流程**（或流程没发布 / 没有可用版本 / 该任务已有在跑的审批单）
///   → 调用方照原样走两级验收，**不报错**。
///
/// # 参数
///
/// - `task_id` / `task_no` / `title`：任务身份（审批单标题 = `任务号 标题`，审批中心能对上）；
/// - `initiator`：发起人（提交验收的那个人）；
/// - `form_data`：发起时冻结的表单快照（条件节点的表达式作用域）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（本函数只在真库故障时才失败；「没配流程」不算失败）。
pub async fn prepare_accept(
    task_id: Uuid,
    task_no: &str,
    title: &str,
    initiator: Uuid,
    form_data: JsonValue,
) -> DbResult<Option<AcceptStart>> {
    // ① 有没有配 TASK_ACCEPT 的默认发布流程（没有 → 走原两级验收，不算错）
    let Some(def) = model::default_def_of_biz_type(TASK_ACCEPT_BIZ_TYPE).await? else {
        tracing::debug!(%task_id, "没有配置 TASK_ACCEPT 审批流程，走原两级验收");
        return Ok(None);
    };
    let Some(version_id) = def.current_version_id else {
        tracing::warn!(flow_code = %def.flow_code, "TASK_ACCEPT 流程没有当前发布版本，跳过自动发起");
        return Ok(None);
    };
    let Some(version) = model::find_version(version_id).await? else {
        tracing::warn!(%version_id, "TASK_ACCEPT 流程的发布版本行不存在，跳过自动发起");
        return Ok(None);
    };

    // ② 同一任务已有在跑的审批单 → 不重复发起（重提验收不该撞 10003）
    if model::biz_instance_running(&def.biz_type, task_id).await? {
        tracing::info!(%task_id, "该任务已有在跑的审批单，本次提交验收不再重复发起");
        return Ok(None);
    }

    // ③ 发起人快照（姓名 / 部门，审批中心列表要显示「谁提的」）
    let (initiator_name, initiator_dept_id) = model::user_profile(initiator)
        .await?
        .unwrap_or((String::new(), None));
    let title = format!("{task_no} {title}")
        .trim()
        .chars()
        .take(200)
        .collect::<String>();

    let instance_id = Uuid::now_v7();
    Ok(Some(AcceptStart {
        instance_id,
        req: engine::StartReq {
            instance_id,
            def_id: def.id,
            version_id,
            version_no: version.version_no,
            flow_code: def.flow_code.clone(),
            flow_name: def.flow_name.clone(),
            biz_type: def.biz_type.clone(),
            biz_id: Some(task_id),
            biz_no: Some(task_no.to_string()),
            title,
            initiator_id: initiator,
            initiator_name: Some(initiator_name),
            initiator_dept_id,
            form_data,
        },
    }))
}

/// **事务内**发起审批单并把实例 id 回写到 `biz_task.approval_instance_id`。
///
/// 与 `change_status` 的状态写入同一个事务：审批单建不出来 → 整笔回滚（任务回到 `DOING`）。
///
/// # 异常情况
///
/// - 版本快照缺失 / 解析失败 → [`DbError::InvalidValue`]（整单回滚）；
/// - 同一业务单据已有在跑实例（`uk_biz_flow_instance_biz_active`）→ [`DbError::UniqueViolation`]。
pub async fn start_in_tx(tx: &Transaction, start: &AcceptStart, task_id: Uuid) -> DbResult<Uuid> {
    let out = engine::start(tx, &start.req).await?;
    let instance_id = out.instance_id.unwrap_or(start.instance_id);

    // 回写任务行：门闸的开关就是这一列（D3a 的判据）
    tx.exec_sql(
        "UPDATE biz_task SET approval_instance_id = ?::uuid, updated_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&instance_id), Value::from(&task_id)],
    )
    .await?;

    tracing::info!(
        %task_id,
        %instance_id,
        instance_status = %out.instance_status,
        flow_code = %start.req.flow_code,
        "提交验收已自动发起审批单"
    );
    Ok(instance_id)
}

// ==================== 写：流程 → 任务（回调） ====================

/// 任务行上回调需要的四列（一次查询取全，避免在事务里再查）。
struct CallbackTaskRow {
    status: String,
    progress: i16,
    need_review: bool,
    approval_instance_id: Option<Uuid>,
}

/// 审批单落终态后推进任务（`FlowCallback` 身份）。
///
/// # 语义
///
/// | 实例状态 | 任务 | 依据 |
/// |---|---|---|
/// | `APPROVED` | `REVIEW → DONE` | 审批通过 = 验收通过 |
/// | `REJECTED` | `REVIEW → DOING` | 审批驳回 = 打回返工（审批意见写进 `reject_reason`） |
/// | 其它 | 不动 | 审批还在跑 / 被撤回 |
///
/// 判定走 [`state::check_transition`] 的 `ActorKind::FlowCallback` 分支：**绕过门闸与角色**
/// （系统身份不带 `role_mask`，也不该被「审批中还 PENDING」挡自己），但**仍要求**任务当前
/// 处于 `REVIEW` 态 —— 否则一条审批通过会把任意任务跳级推到 `DONE`。
///
/// # 返回值
///
/// 被推进的任务 id（没有推进 / 不该推进 → `None`）。**本函数不返回错误**：
/// 它跑在审批事务**提交之后**，失败只记 `warn`（审批结论已落库，不许因任务侧失败而回滚）。
///
/// # 幂等
///
/// 重复回调（同一条审批单被两次 settle）时，任务已不在 `REVIEW` →
/// `callback_transition` 返回 `IllegalPair` → 记一条 `debug` 后返回 `None`，不报错。
pub async fn on_instance_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let target = settle_target(instance_status)?;

    match settle(instance_id, target).await {
        Ok(task_id) => {
            if let Some(task_id) = task_id {
                tracing::info!(
                    %instance_id,
                    %task_id,
                    to = target.code(),
                    "审批结论已回调任务状态"
                );
            }
            task_id
        }
        Err(error) => {
            tracing::warn!(%instance_id, %error, "审批结论回调任务失败（不影响审批单）");
            None
        }
    }
}

/// [`on_instance_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(
    instance_id: Uuid,
    target: TaskStatus,
) -> std::result::Result<Option<Uuid>, DbError> {
    // ① 审批单必须是任务验收类的（别的业务类型与任务无关）
    let Some(instance) = model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过任务回调");
        return Ok(None);
    };
    if instance.biz_type != TASK_ACCEPT_BIZ_TYPE {
        return Ok(None);
    }
    let Some(task_id) = instance.biz_id else {
        return Ok(None);
    };

    // ② 任务当前态（回调不推进已不在 REVIEW 的任务）
    let Some(row) = callback_task_row(task_id).await? else {
        tracing::warn!(%task_id, "任务不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    let from = TaskStatus::parse(&row.status).ok_or_else(|| {
        DbError::InvalidValue(format!("库里的任务状态不是 5 态之一：{}", row.status))
    })?;
    // 驳回意见即打回原因（审批单的 `result_comment`；不足 5 字也放行 —— 系统身份不看这条）
    // 引擎把「驳回意见」落在待办行（`biz_flow_task.comment`）而不是实例行，实例行的
    // `result_comment` 常为空 → 读不到就回退到最近一条有意见的待办行。
    let reason = match instance
        .result_comment
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(text) => Some(text.to_string()),
        None => latest_verdict_comment(instance_id).await?,
    };
    let transition = match state::check_transition(
        ActorKind::FlowCallback,
        from,
        target,
        0,
        row.need_review,
        reason.as_deref(),
        row.approval_instance_id,
        Some(instance.status.as_str()),
    ) {
        Ok(transition) => transition,
        Err(error) => {
            tracing::debug!(%task_id, message = %error.message(), "任务当前不满足回调推进条件，跳过");
            return Ok(None);
        }
    };

    // ③ 有子任务的任务，进度由子任务汇总（与 handler 同口径：不许回调手改）
    let children = task_model::children_of(task_id).await?;
    let locked = state::progress_locked(&task_model::snapshots_of(&children));
    let new_progress = if locked {
        None
    } else {
        transition.progress.apply(row.progress)
    };
    let after_progress = i32::from(new_progress.unwrap_or(row.progress));
    let done_at = match transition.done_at {
        state::DoneAtEffect::Keep => None,
        state::DoneAtEffect::Clear => Some(None),
        // 时间点在 Rust 侧算好：变更日志 diff 与库里落的是同一个值
        state::DoneAtEffect::SetNow => Some(Some(chrono::Utc::now())),
    };

    let update = task_model::StatusUpdate {
        to: target.code().to_string(),
        progress: new_progress,
        done_at,
        reject_reason: if target == TaskStatus::Doing {
            reason.clone()
        } else {
            None
        },
        cancel_reason: None,
    };

    // ④ 评论正文：让任务时间线能看出「这条是审批结论推的」
    let mut content = format!(
        "审批结论：{}（审批单 {}）→ 状态变更：{}（{}）→ {}（{}）",
        if target == TaskStatus::Done {
            "通过"
        } else {
            "驳回"
        },
        instance.instance_no,
        from.label(),
        from.code(),
        target.label(),
        target.code()
    );
    if let Some(reason) = reason.as_deref() {
        content.push_str(&format!("；审批意见：{reason}"));
    }
    if locked {
        content.push_str("；本条有子任务，进度由子任务汇总");
    }

    let before = serde_json::json!({
        "status": from.code(),
        "progress": i32::from(row.progress),
        "done_at": JsonValue::Null,
        "reject_reason": JsonValue::Null,
        "cancel_reason": JsonValue::Null,
    });
    let after = serde_json::json!({
        "status": target.code(),
        "progress": after_progress,
        "done_at": update.done_at.as_ref().and_then(|at| at.as_ref()).map(|at| at.to_rfc3339()),
        "reject_reason": update.reject_reason,
        "cancel_reason": JsonValue::Null,
    });
    let operator = instance.initiator_id;
    let action_label = transition.label;

    // ⑤ 同事务：状态 → 评论 → 变更日志
    let affected = db()
        .transaction(|tx| async move {
            let affected = task_model::apply_status(&tx, task_id, &update, operator).await?;
            if affected == 0 {
                return Ok(0_u64);
            }
            task_model::insert_comment(
                &tx,
                task_id,
                operator,
                &content,
                task_model::COMMENT_TYPE_STATUS_CHANGE,
                operator,
            )
            .await?;
            changelog::record(
                &tx,
                &ChangeCtx::api(None, Some(CALLBACK_OPERATOR_NAME)),
                ChangeTarget::of(
                    "biz_task",
                    task_id.to_string(),
                    Action::Status,
                    action_label,
                ),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| {
                DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg))
            })?;
            Ok(affected)
        })
        .await?;

    if affected == 0 {
        // 事务里被并发软删
        return Ok(None);
    }
    Ok(Some(task_id))
}

/// 最近一条「有意见的待办行」的审批意见（驳回原因的回退来源）。
///
/// 引擎把同意 / 驳回意见写在 `biz_flow_task.comment`（待办行），实例行的 `result_comment`
/// 往往为空。回调要拿「为什么被驳回」当任务打回原因，所以按 `completed_at` 倒序取第一条非空。
async fn latest_verdict_comment(instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT t.comment FROM biz_flow_task t \
              WHERE t.instance_id = ?::uuid AND t.comment IS NOT NULL AND t.comment <> '' \
              ORDER BY coalesce(t.completed_at, t.created_at) DESC LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let comment: String = row.get_typed("comment")?;
            Ok(Some(comment))
        }
    }
}

/// 读回调需要的任务列（`REVIEW` 判定 / 进度 / 验收开关 / 当前审批单）。
async fn callback_task_row(task_id: Uuid) -> DbResult<Option<CallbackTaskRow>> {
    let rows = db()
        .find_with_params(
            "SELECT t.status, t.progress, t.need_review, t.approval_instance_id \
               FROM biz_task t WHERE t.id = ?::uuid AND t.deleted_at IS NULL",
            vec![Value::from(&task_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(CallbackTaskRow {
            status: row.get_typed("status")?,
            progress: row.get_typed("progress")?,
            need_review: row.get_typed("need_review")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
        })),
    }
}

// ==================== 单元测试（纯逻辑部分） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settle_target_maps_only_final_verdicts() {
        assert_eq!(settle_target("APPROVED"), Some(TaskStatus::Done));
        assert_eq!(settle_target(" rejected "), Some(TaskStatus::Doing));
        // 还在跑 / 被撤回 / 走错地方的状态都不推进任务
        for status in ["PENDING", "CANCELLED", "WITHDRAWN", "", "approved2"] {
            assert_eq!(settle_target(status), None, "{status} 不该推进任务");
        }
    }

    #[test]
    fn accept_biz_type_is_normalized() {
        // 与 flow::dto::normalize_biz_type 的口径一致（全大写下划线）：否则按 biz_type
        // 查默认流程会静默查不到 → 门闸恒不触发（D3a 复审踩过的坑）
        assert_eq!(TASK_ACCEPT_BIZ_TYPE, "TASK_ACCEPT");
        assert_eq!(TASK_ACCEPT_BIZ_TYPE, TASK_ACCEPT_BIZ_TYPE.to_uppercase());
        assert!(!TASK_ACCEPT_BIZ_TYPE.contains('-'));
    }
}

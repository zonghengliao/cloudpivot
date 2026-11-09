// ============================================================================
// task_handler_status.rs —— 状态机与进度：改状态 / 改进度（含审批门闸、进度联动、事务编排）
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::Json;
use axum::extract::Path;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_handler_common::*;
use super::task_handler_dep::*;
use super::task_handler_notice::*;
use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::flow::task_bridge;
use crate::business::shared::principal::{self};
use crate::business::task::dto::{
    ParentProgressRes, TaskProgressReq, TaskProgressRes, TaskStatusChangeReq, TaskStatusRes,
};
use crate::business::task::model::{self};
use crate::business::task::state::{self, TaskStatus};
use crate::shared::error_code;
use crate::utils;

/// 改状态（`PATCH /business/task/{id}/status`）。
///
/// # 四段式判定（顺序不可换，见 [`crate::business::task::state::check_transition`]）
///
/// 0. **审批门闸**：任务挂了审批单且实例 `PENDING` 时的 `REVIEW → DONE / DOING` → `409 + 65030`
///    （**先于**白名单与角色：审批中换谁来点都是同一个答案）；
/// 1. **可见性**：看不见的任务 → `404 + 63001`（不暴露「存在但无权」）；
/// 2. **白名单**：`from → to` 不在 [`state::TRANSITIONS`] 里 → `409 + 63010`；
/// 3. **角色 + 附加条件**：无权 → `403 + 63011`；打回缺原因 → `400 + 63013`；
///    `need_review=true` 却想直接完成 → `409 + 63010`。
///
/// # 副作用（全部在同一个事务里）
///
/// - 改 `status` / `progress`（无子任务才联动）/ `done_at` / 打回或取消原因；
/// - 写一条 `comment_type = STATUS_CHANGE` 的评论（含 from/to/reason）；
/// - 写 `sys_change_log`（`action = STATUS`，白名单字段 diff）；
/// - 若本条是子任务 → 沿 `parent_id` 逐级重算父任务进度（架构 D4/D5）；
/// - 若本条是子任务 → 沿 `parent_id` 逐级**级联父任务状态**（T10：首个活动子任务 →
///   父任务进 `DOING`；子任务全部到终态 → 父任务进 `REVIEW`，见
///   [`model::cascade_parent_status`]）；
/// - **本条推到 `CANCELLED` → 同一事务里级联取消全部非终态子孙**（T11：父任务取消的
///   语义就是「这条线不要了」，见 [`model::cascade_cancel_descendants`]）；
/// - **本条推到 `CANCELLED` → 同一事务里撤掉本人与全部级联子孙的在跑审批单**
///   （P11 铁律①「正向必配反向」：实例 `CANCELLED` + 待办 `CANCELLED` + 节点 `CANCELLED`，
///   见 [`crate::business::flow::task_bridge::cancel_on_task_cancelled`]；撤不掉 → 整笔回滚）；
/// - **`DOING → REVIEW` 且配了 `TASK_ACCEPT` 审批流程 → 自动发起审批单并回写
///   `approval_instance_id`**（没配流程则什么都不做，保持原两级验收，见
///   [`crate::business::flow::task_bridge::prepare_accept`]）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 id 不是 UUID；400 + `63004`：`to` 不是 5 态之一；
/// - 400 + `63013`：打回没填原因 / 不足 5 字；
/// - 403 + `63011`：当前身份无权推这个流转；
/// - 404 + `63001`：任务不存在 / 不可见；
/// - 409 + `63010`：非法流转（跳级 / 终态直取 / 同状态）；
/// - 409 + `65030`：审批单还在跑，`REVIEW` 出边一律不许人工推（等审批结论）；
/// - 409 + `63021`：**父任务已取消**，子任务不能继续推进（子任务自己 → `CANCELLED` 除外）；
/// - 409 + `63006`：还有未完成子任务，不能完成；
/// - 500：数据库异常 / 变更日志未登记 / 审批单发起失败（整笔回滚）。
#[nik::permission(
    path = "/business/task/{id}/status",
    method = "PATCH",
    permission = "business:task:status"
)]
#[nik::patch("/business/task/{id}/status")]
pub async fn change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskStatusChangeReq>,
) -> ResResult<TaskStatusRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;

    // ① 可见性：只有看得见的人才能推（协作人看得见 → 下一步会被角色判定挡在 403）
    let current = model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;

    // ② 目标状态取值
    let to = TaskStatus::parse(&req.to).ok_or_else(|| {
        param_invalid(format!(
            "状态取值非法：{}（应为 {}）",
            req.to,
            TaskStatus::ALL
                .iter()
                .map(|status| status.code())
                .collect::<Vec<_>>()
                .join(" / ")
        ))
    })?;
    let from = TaskStatus::parse(&current.card.status).ok_or_else(|| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("库里的任务状态不是 5 态之一：{}", current.card.status),
        )
    })?;

    // ③ 前守卫（T11 规则②）：父任务已取消 → 子任务一律不许推进。
    //    为什么放在状态机之前：父已取消时前端仍可能把「开始/提交验收」按钮露出来，
    //    此时回 63021（父任务已取消）比回 63010（非法流转）准确 —— 流转本身是合法的，
    //    作废的是整条线。守卫只看父任务，`CANCELLED` 自己幂等放行。
    let parent_status = model::parent_status(db(), task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询父任务状态"))?;
    if model::blocks_child_advance(parent_status, to) {
        return Err(parent_cancelled());
    }

    // ③ 审批门闸 + 白名单 + 角色 + 附加条件（D3b：门闸在状态机之前）
    let people = state::TaskPeople {
        owner_id: current.card.owner_id,
        creator_id: current.creator_id,
        assigner_id: current.assigner_id,
    };
    let role_mask = people.role_mask(&state::Actor::new(me, mine.is_super()));
    let reason = normalize_reason(req.reason.as_deref())?;
    // 门闸输入：任务上挂了审批单就现查实例状态（读不到 → 不拦，见 state::flow_gate_blocked）
    let instance_status = task_bridge::instance_status_of(current.approval_instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单状态"))?;
    let transition = state::check_transition(
        // 红线：HTTP 路径永远是「人类」身份，不从请求读这个值
        state::ActorKind::User,
        from,
        to,
        role_mask,
        current.card.need_review,
        reason.as_deref(),
        current.approval_instance_id,
        instance_status.as_deref(),
    )
    .map_err(transition_check_error)?;

    // ④ 进入 DONE：有未完成子任务 → 409（设计 §三「父任务 DONE 前置检查」）
    if to == TaskStatus::Done {
        let open = model::count_open_children(task_id)
            .await
            .map_err(|error| utils::db_error(error, "统计未完成子任务"))?;
        if open > 0 {
            return Err(ApiError::new(
                409,
                error_code::TASK_HAS_OPEN_CHILDREN,
                format!("还有 {open} 个未完成子任务，不能直接完成父任务"),
            ));
        }
    }

    // ⑤ 前置依赖校验（H-DEP）：T4 落地「所有 FS 前置必须 DONE」。
    //    这里保留调用点与注释，T4 只需把 `check_dependencies_ready` 换成真实实现，
    //    本函数的其余部分不用改。
    if from == TaskStatus::Todo && to == TaskStatus::Doing {
        check_dependencies_ready(task_id).await?;
    }

    // ⑤b 提交验收（DOING → REVIEW）且 `flow_def` 里配了 `TASK_ACCEPT` 流程 → 自动发起审批单。
    //    没配流程 → `prepare_accept` 返回 `None`，照原两级验收走（**不报错**，存量行为不变）。
    //    真正的入库在 ⑧ 的事务里（`start_in_tx`）：审批单建不出来就整笔回滚。
    let accept = if from == TaskStatus::Doing && to == TaskStatus::Review {
        let form_data = serde_json::json!({
            "task_id": task_id.to_string(),
            "task_no": current.card.task_no,
            "title": current.card.title,
            "priority": current.card.priority,
        });
        task_bridge::prepare_accept(task_id, &current.card.task_no, &current.card.title, me, form_data)
            .await
            .map_err(|error| utils::db_error(error, "准备验收审批单"))?
    } else {
        None
    };

    // ⑥ 进度：有子任务的任务由汇总决定，**任何流转都不许手改**
    let children = model::children_of(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    let locked = state::progress_locked(&model::snapshots_of(&children));
    let new_progress = if locked {
        None
    } else {
        transition.progress.apply(current.card.progress)
    };

    // ⑦ 评论正文（from/to/reason 三件套，前端时间线按它渲染）
    let mut content = format!(
        "状态变更：{}（{}）→ {}（{}）",
        from.label(),
        from.code(),
        to.label(),
        to.code()
    );
    if let Some(reason) = reason.as_deref() {
        content.push_str(&format!("；原因：{reason}"));
    }
    if locked {
        content.push_str("；本条有子任务，进度由子任务汇总");
    } else if let Some(progress) = new_progress
        && progress != current.card.progress
    {
        content.push_str(&format!("；进度 {}% → {progress}%", current.card.progress));
    }

    let update = model::StatusUpdate {
        to: to.code().to_string(),
        progress: new_progress,
        done_at: match transition.done_at {
            state::DoneAtEffect::Keep => None,
            state::DoneAtEffect::Clear => Some(None),
            // 时间点在 Rust 侧算好：变更日志的 diff 与库里的值必须是同一个
            state::DoneAtEffect::SetNow => Some(Some(Utc::now())),
        },
        reject_reason: if from == TaskStatus::Review && to == TaskStatus::Doing {
            reason.clone()
        } else {
            None
        },
        cancel_reason: if to == TaskStatus::Cancelled {
            reason.clone()
        } else {
            None
        },
    };
    let after_progress = i32::from(new_progress.unwrap_or(current.card.progress));
    let after_done_at = match update.done_at.as_ref() {
        Some(value) => value.as_ref().map(|at| at.to_rfc3339()),
        None => current.done_at.map(|at| at.to_rfc3339()),
    };

    // ⑧ 同事务：写状态 → 写评论 → 写变更日志 → 重算父任务
    let before = serde_json::json!({
        "status": from.code(),
        "progress": i32::from(current.card.progress),
        "done_at": current.done_at.map(|at| at.to_rfc3339()),
        "reject_reason": current.reject_reason,
        "cancel_reason": current.cancel_reason,
    });
    let after = serde_json::json!({
        "status": to.code(),
        "progress": after_progress,
        "done_at": after_done_at,
        "reject_reason": if update.reject_reason.is_some() { reason.clone() } else { current.reject_reason.clone() },
        "cancel_reason": if update.cancel_reason.is_some() { reason.clone() } else { current.cancel_reason.clone() },
    });
    let operator_name = display_name(&claims);
    let operator_name_tx = operator_name.clone();
    let action_label = transition.label;
    let task_no = current.card.task_no.clone();
    let task_no_tx = task_no.clone();
    // ⑧a P-FLOW-1B：流程任务的标识（在事务外取好，闭包里只用拥有所有权的副本 ——
    //     `current` 在事务之后还要用（⑨ 通知），不能被 move 进闭包）。
    // `source != 'FLOW'` 时下面这条分支整个不进：
    // **普通任务的 `change_status` 一次查询都不多、逐字节不变**（验收 7）。
    let is_flow_task = current.card.source == task_bridge::FLOW_SOURCE;
    let flow_instance_id = current.card.flow_instance_id;
    let flow_node_key = current.card.flow_node_key.clone();
    let flow_node_key_tx = flow_node_key.clone();
    let flow_round = current.card.flow_round;
    let reason_tx = reason.clone();
    // `ApiError` 既不能进 `DbError` 也不能 `Clone`：冲突用「哨兵 + 槽位」带出事务
    let flow_conflict: std::sync::Arc<std::sync::Mutex<Option<ApiError>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let flow_conflict_tx = flow_conflict.clone();
    // 验收即发起审批（D3b）同理：引擎业务拒绝（65018 派不出人 / 65019 自审）必须整笔回滚，
    // 原始 `ApiError` 也带不出事务，同样走「哨兵 + 槽位」（见 `engine::START_REJECTED_MARKER`）。
    let start_rejected: std::sync::Arc<std::sync::Mutex<Option<ApiError>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let start_rejected_tx = start_rejected.clone();
    let (comment_id, affected, recalcs, cascades, cancels, approvals, flow_outcome) = db()
        .transaction(|tx| async move {
            // T11 守卫（**事务内**再判一次）：③ 前那次读与这次写之间，父任务可能被
            // 并发取消 —— 那种情况下子任务照样不许推进（漏洞 2 的并发变体）。
            // 错误用哨兵回传，闭包外的 `change_status_tx_error` 把它翻成 409 + 63021。
            if model::blocks_child_advance(model::parent_status(&tx, task_id).await?, to) {
                return Err(DbError::InvalidValue(PARENT_CANCELLED_SENTINEL.to_string()));
            }
            let affected = model::apply_status(&tx, task_id, &update, me).await?;
            if affected == 0 {
                // 事务里被并发软删：评论与日志都不该落
                return Ok((
                    Uuid::nil(),
                    0,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    crate::business::flow::model::CancelledOfBiz::default(),
                    task_bridge::FlowTaskOutcome::NotFlowTask,
                ));
            }
            // ⑧a P-FLOW-1B（设计 §3.4 的唯一扩展点）：任务状态已落库 → 流程侧同事务跟上。
            //     `source != 'FLOW'` 时返回 `NotFlowTask`，**一次查询都不发**。
            //     冲突（任务已完成但流程已终止 / 并发重复完成）→ 整笔回滚：
            //     `ApiError` 带不出事务，用「哨兵 + 槽位」交给闭包外的 `change_status_tx_error`。
            let flow_outcome = if is_flow_task {
                let outcome = task_bridge::on_task_status_changed(
                    &tx,
                    task_bridge::TaskStatusChanged {
                        task_id,
                        instance_id: flow_instance_id,
                        node_key: flow_node_key_tx.as_deref(),
                        round: flow_round,
                        from,
                        to,
                        operator: me,
                        operator_name: &operator_name_tx,
                        reason: reason_tx.as_deref(),
                    },
                )
                .await?;
                if let task_bridge::FlowTaskOutcome::Conflict(error) = outcome {
                    *flow_conflict_tx.lock().unwrap() = Some(error);
                    return Err(DbError::InvalidValue(FLOW_CONFLICT_SENTINEL.to_string()));
                }
                outcome
            } else {
                task_bridge::FlowTaskOutcome::NotFlowTask
            };
            // T11 规则①：本条推到 CANCELLED → 同一事务里级联取消全部非终态子孙
            // （一条递归 CTE；DONE 不回退；失败 → 整笔回滚，不接受「父取消、子还在跑」）
            let cancels = if to == TaskStatus::Cancelled {
                model::cascade_cancel_descendants(&tx, task_id, me, model::CASCADE_CANCEL_REASON).await?
            } else {
                Vec::new()
            };
            // P11 · 铁律①「正向必配反向」：任务取消 → 它的审批实例与待办必须一起撤，
            // 否则审批人的「待我审批」里挂着一条点不动的孤儿待办（老板报的 TSK-149）。
            // 必须与状态写入同一事务：撤不掉就整笔回滚，不接受「任务 CANCELLED、待办还 PENDING」。
            // 级联取消的子孙也是「任务已取消」，各自的审批同样要撤（逐个按自己的 task_no 精确匹配）。
            let mut approvals = if to == TaskStatus::Cancelled {
                task_bridge::cancel_on_task_cancelled(&tx, task_id, &task_no_tx).await?
            } else {
                crate::business::flow::model::CancelledOfBiz::default()
            };
            for cancel in &cancels {
                let sub = task_bridge::cancel_on_task_cancelled(&tx, cancel.task_id, &cancel.task_no).await?;
                approvals.instances += sub.instances;
                approvals.tasks += sub.tasks;
                approvals.nodes += sub.nodes;
            }
            // D3b：提交验收即发起审批单（与状态写入同事务：建不出审批单 → 整笔回滚）
            if let Some(start) = accept.as_ref() {
                task_bridge::start_in_tx(&tx, start, task_id, &start_rejected_tx).await?;
            }
            let comment_id =
                model::insert_comment(&tx, task_id, me, &content, model::COMMENT_TYPE_STATUS_CHANGE, me).await?;
            changelog::record(
                &tx,
                &ChangeCtx::api(Some(me), Some(&operator_name_tx)),
                ChangeTarget::of("biz_task", task_id.to_string(), Action::Status, action_label),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg)))?;
            // T11：每个被级联取消的子孙各记一条变更日志（审计口径：不能只留在父任务
            // 那条汇总里 —— 事后要能回答「这条子任务是谁、为什么取消的」），
            // 并在父任务时间线上补一条汇总评论。
            for cancel in &cancels {
                let before = serde_json::json!({ "status": cancel.from.code() });
                let after = serde_json::json!({ "status": TaskStatus::Cancelled.code() });
                changelog::record(
                    &tx,
                    &ChangeCtx::api(Some(me), Some(operator_name_tx.as_str())),
                    ChangeTarget::of(
                        "biz_task",
                        cancel.task_id.to_string(),
                        Action::Status,
                        model::CASCADE_CANCEL_REASON,
                    ),
                    DiffInput::update(&before, &after),
                )
                .await
                .map_err(|error| DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg)))?;
            }
            if !cancels.is_empty() {
                model::insert_comment(
                    &tx,
                    task_id,
                    me,
                    &format!(
                        "级联取消：本任务取消后，{} 个未完成的子孙任务已随之取消（{}）",
                        cancels.len(),
                        model::CASCADE_CANCEL_REASON
                    ),
                    model::COMMENT_TYPE_STATUS_CHANGE,
                    me,
                )
                .await?;
            }
            let recalcs = model::recalc_ancestors(&tx, task_id, me).await?;
            // T10：父任务状态随子任务级联（**同事务**：级联失败 → 子任务状态一起回滚，
            // 红线是「宁可这次没改成，也不接受父任务状态错着」）
            let cascades = model::cascade_parent_status(&tx, task_id, me).await?;
            // 级联的变更日志也在这个闭包里写：状态改了而日志没写就是审计缺口，
            // 两者必须同生共死（G5 门禁：`record` 的调用点只能在事务闭包内）。
            for cascade in &cascades {
                let before = serde_json::json!({ "status": cascade.from.code() });
                let after = serde_json::json!({ "status": cascade.to.code() });
                changelog::record(
                    &tx,
                    &ChangeCtx::api(Some(me), Some(operator_name_tx.as_str())),
                    ChangeTarget::of("biz_task", cascade.task_id.to_string(), Action::Status, cascade.reason),
                    DiffInput::update(&before, &after),
                )
                .await
                .map_err(|error| DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg)))?;
            }
            Ok((
                comment_id,
                affected,
                recalcs,
                cascades,
                cancels,
                approvals,
                flow_outcome,
            ))
        })
        .await
        .map_err(|error| change_status_tx_error(error, &flow_conflict, &start_rejected))?;
    if affected == 0 {
        return Err(not_found());
    }

    // ⑨ 通知（事务外：通知失败不该回滚状态）
    notify_status_change(&current, from, to, me, &operator_name, reason.as_deref()).await;
    notify_ready_to_close(&recalcs, me, &operator_name).await;
    // ⑨b 级联结果留痕（改动已在事务里落库，这里只补一条可 grep 的汇总行）
    for cascade in &cascades {
        tracing::info!(
            task_id = %cascade.task_id,
            from = cascade.from.code(),
            to = cascade.to.code(),
            reason = cascade.reason,
            "父任务状态级联"
        );
    }
    // ⑨c T11：级联取消留痕（每条已在事务里各写一条变更日志，这里补一条可 grep 的汇总行）
    if !cancels.is_empty() {
        tracing::info!(
            task_id = %task_id,
            cancelled = cancels.len() as i64,
            reason = model::CASCADE_CANCEL_REASON,
            "父任务取消：子任务已级联取消"
        );
    }
    // ⑨d P11：撤销审批的留痕（孤儿待办是「正向没有配套反向」的典型，日志要能一眼查到）
    if to == TaskStatus::Cancelled && approvals != crate::business::flow::model::CancelledOfBiz::default() {
        tracing::info!(
            task_id = %task_id,
            task_no = %task_no,
            instances = approvals.instances,
            flow_tasks = approvals.tasks,
            nodes = approvals.nodes,
            comment = task_bridge::TASK_CANCEL_COMMENT,
            "任务取消：在跑审批实例与待办已一并撤销"
        );
    }
    // ⑨e P-FLOW-1B：流程侧推进结果留痕（推进已在事务里落库，这里只补可 grep 的汇总行）。
    //     `Ignored` 一律是 debug（普通任务、等验收、重开这些高频路径不该刷 info 日志）。
    match &flow_outcome {
        task_bridge::FlowTaskOutcome::Advanced { next_nodes } => tracing::info!(
            task_id = %task_id,
            from = from.code(),
            to = to.code(),
            node_key = flow_node_key.as_deref().unwrap_or(""),
            next_nodes = ?next_nodes,
            "任务状态变更已推进流程节点"
        ),
        task_bridge::FlowTaskOutcome::Ignored { instance_status } => tracing::debug!(
            task_id = %task_id,
            from = from.code(),
            to = to.code(),
            instance_status,
            "任务状态变更未推进流程（映射表判定「不动」）"
        ),
        task_bridge::FlowTaskOutcome::NotFlowTask => {}
        // 冲突一定会整笔回滚并走 `return Err`，到这里说明事务已成功 —— 不可达，留防御
        task_bridge::FlowTaskOutcome::Conflict(_) => {}
    }
    // ⑩ 依赖联动（T4）：前置 DONE → 强制通知后继负责人；前置 CANCELLED → 尊重偏好提醒
    if matches!(to, TaskStatus::Done | TaskStatus::Cancelled) {
        notify_dependents(&current, to, me, &operator_name).await;
    }

    let updated = require_task(task_id).await?;
    tracing::info!(
        task_id = %task_id,
        from = from.code(),
        to = to.code(),
        operator = %me,
        "任务状态已流转"
    );
    Ok(Res::ok(TaskStatusRes {
        id: task_id.to_string(),
        task_no: updated.card.task_no.clone(),
        from: from.code().to_string(),
        status: updated.card.status.clone(),
        progress: i32::from(updated.card.progress),
        done_at: updated.done_at,
        comment_id: comment_id.to_string(),
        parent_progress: recalcs
            .iter()
            .map(|recalc| ParentProgressRes {
                id: recalc.task_id.to_string(),
                progress: i32::from(recalc.new),
                previous: i32::from(recalc.old),
                mode: recalc.mode.to_string(),
                all_done: recalc.all_done,
            })
            .collect(),
    }))
}

/// 改进度（`PATCH /business/task/{id}/progress`）。
///
/// # 两条硬规则
///
/// 1. **有子任务 → `409 + 63012`**（红线：父任务的进度由子任务汇总，手改无效且必须报错，
///    不许「静默改成想要的值然后被下次重算覆盖」）；
/// 2. **状态联动**：`DOING → max(进度, 10)`、`REVIEW` / `DONE → 100`、`CANCELLED` 冻结
///    （取消中的任务改进度没有意义，也不该被状态机之外的操作改）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 id 不是 UUID；400 + `63004`：进度不在 0~100；
/// - 403 + `63002`：既不是负责人 / 创建人，也不是管理员；
/// - 404 + `63001`：任务不存在 / 不可见；
/// - 409 + `63012`：有子任务，进度由子任务汇总；
/// - 409 + `63010`：已取消的任务进度被冻结；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/progress",
    method = "PATCH",
    permission = "business:task:progress"
)]
#[nik::patch("/business/task/{id}/progress")]
pub async fn change_progress(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskProgressReq>,
) -> ResResult<TaskProgressRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let current = model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;

    // 写权限：负责人 / 创建人 / 管理员（协作人看得见但改不了）
    if current.card.owner_id != me && current.creator_id != me && !mine.is_super() {
        return Err(ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的负责人、创建人或管理员可以修改进度",
        ));
    }
    if !(0..=100).contains(&req.progress) {
        return Err(param_invalid(format!(
            "进度必须在 0 ~ 100 之间（收到 {}）",
            req.progress
        )));
    }

    let children = model::children_of(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    if state::progress_locked(&model::snapshots_of(&children)) {
        return Err(ApiError::new(
            409,
            error_code::TASK_PROGRESS_LOCKED,
            format!(
                "进度由子任务汇总，不能手动修改（当前 {} 个子任务，汇总值已写入任务）",
                children.len()
            ),
        ));
    }

    let status = TaskStatus::parse(&current.card.status).ok_or_else(|| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("库里的任务状态不是 5 态之一：{}", current.card.status),
        )
    })?;
    let value = requested_progress(status, req.progress)?;

    let (affected, recalcs) = db()
        .transaction(|tx| async move {
            let affected = model::set_progress(&tx, task_id, value, me).await?;
            let recalcs = if affected == 0 {
                Vec::new()
            } else {
                model::recalc_ancestors(&tx, task_id, me).await?
            };
            Ok((affected, recalcs))
        })
        .await
        .map_err(|error| utils::db_error(error, "修改任务进度"))?;
    if affected == 0 {
        return Err(not_found());
    }
    let operator_name = display_name(&claims);
    notify_ready_to_close(&recalcs, me, &operator_name).await;
    let updated = require_task(task_id).await?;
    tracing::info!(task_id = %task_id, progress = value, operator = %me, "任务进度已修改");
    Ok(Res::ok(TaskProgressRes {
        id: task_id.to_string(),
        progress: i32::from(updated.card.progress),
        requested: req.progress,
    }))
}

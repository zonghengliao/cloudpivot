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

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::erp::transfer_linkage;
use crate::business::erp::transfer_model as erp_transfer;
use crate::business::flow::graph;
use crate::business::flow::node_config::{self, OnCancel, OnComplete, OnReject, TaskNodeConfig};
use crate::business::flow::task_node;
use crate::business::flow::{engine, linkage, model, settle_guard};
use crate::business::task::model as task_model;
use crate::business::task::state::{self, ActorKind, TaskStatus};
use crate::shared::error_code;

/// 任务验收类流程的业务类型（`biz_flow_def.biz_type`，`flow::dto::normalize_biz_type` 归一化后全大写）。
pub const TASK_ACCEPT_BIZ_TYPE: &str = "TASK_ACCEPT";

/// 流程生成的任务 `source` 值（`flow::task_node::FLOW_SOURCE` 的再导出）。
///
/// # 为什么要再导出
///
/// `task` 模块对 `flow` 只认 `task_bridge` 这一个口（`task/handler.rs` 已经
/// `use crate::business::flow::task_bridge`）。让它直接 `use flow::task_node`
/// 等于在模块边界上又开一条线，下一次重构就分叉了。
pub use crate::business::flow::task_node::FLOW_SOURCE;

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
    let task_id = match settle_target(instance_status) {
        None => None,
        Some(target) => match settle(instance_id, target).await {
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
        },
    };

    // ★ P-FLOW-1B：审批单落终态后**推进流程节点**（`on_complete=APPROVE` 与「验收通过」两条路
    //   共用这一处）。必须排在 `settle` 之后：`REVIEW → DONE` 刚落库，节点才知道该通过。
    //   与 `settle` 同口径——失败只 `warn`，不回滚审批结论。
    on_task_approval_settled(Some(instance_id), instance_status).await;

    task_id
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

    // ★ C1（t_1e174431）：写目标状态**之前**先校验【目标单据此刻的状态】还在不在
    //   「能接受这个审批结论」的合法路径上。实测事故 TSK-202610-000813：
    //   20:00:26 任务 → CANCELLED，20:09:06 审批实例 → APPROVED —— 回调在任务侧被静默跳过，
    //   既没 WARN 也没留痕。现在不在路径上就：不落状态 + WARN + 审批实例留痕
    //   （实例本身仍是 APPROVED，红线：不改审批实例状态）。
    //   判定走任务状态机的 `callback_transition`（`FlowCallback` 只开 `REVIEW → DONE|DOING`），
    //   这里不写任何 if-else 硬判。
    let biz_no = instance
        .biz_no
        .clone()
        .unwrap_or_else(|| task_id.to_string());
    if !settle_guard::gate_task(&instance, &biz_no, &row.status, target.code()).await {
        return Ok(None);
    }
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

    // ⑥ T13：任务状态已落库（上面那笔事务已提交）→ 父任务链实时跟上。
    //    为什么不开在那个闭包里：那是**任务状态**的事务，而审批单的结论早就落了库
    //    （settle 跑在审批事务提交之后）；同事务回滚会连带吃掉「任务已 DONE」，
    //    而审批单已是 APPROVED —— 两者永久矛盾，且红线不许回滚审批结论。
    //    独立事务 + 失败只 warn：正常路径秒级跟上，极端失败由巡检接口兜底。
    match cascade_after_flow(task_id, operator).await {
        Ok(out) => {
            tracing::info!(
                %task_id,
                cascaded = out.cascades.len() as i64,
                "审批回调后父任务已级联（独立事务）"
            );
        }
        Err(error) => {
            tracing::warn!(
                %task_id,
                %error,
                "审批回调后级联父任务失败（不影响审批单，巡检接口会兜底）"
            );
        }
    }
    Ok(Some(task_id))
}

// ==================== 审批回调后的父任务级联（T13） ====================

/// 一次「审批回调后级联」的结果（日志与单测断言用）。
#[derive(Debug, Clone, Default)]
pub struct CascadeAfterFlow {
    /// 父链上每一级的进度重算（由近及远）。
    pub recalcs: Vec<task_model::ProgressRecalc>,
    /// 真正改了状态的层级（由近及远）。
    pub cascades: Vec<task_model::StatusCascade>,
}

/// 单测专用的「级联失败」注入开关（`cfg(test)` 编译进 lib 单测，**生产二进制里没有这段**）。
///
/// 存在的理由：卡面要求证明「级联失败不阻塞审批（审批结论仍然落库）」，
/// 而级联失败在生产里只有「库故障 / 进程被杀 / 并发冲突」这些不可控来源 ——
/// 没有开关就只能靠运气复现。置 true 后下一次 [`cascade_after_flow`] 直接返回错误。
#[cfg(test)]
static CASCADE_FAIL_INJECTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 审批结论推进过任务之后，把父任务链跟上（**独立事务**）。
///
/// # 为什么是独立事务（不是塞进 [`settle`] 的那个闭包）
///
/// [`settle`] 写任务状态的那笔事务已经提交，而审批单的结论**更早**就落了库
/// （本函数跑在审批事务提交之后，见 [`on_instance_settled`] 的注释）。把级联塞进
/// 任务状态那段闭包有两个问题：
///
/// 1. 语义错位：T10 的级联红线是「级联失败与**子任务状态**一起回滚」，而这里
///    同事务回滚会连带吃掉「任务已 DONE」这个事实 —— 但审批单已经是 `APPROVED`，
///    两者会永久矛盾（审批通过、任务还在 `REVIEW`）；
/// 2. 红线：审批结论不许因任务侧失败而回滚。
///
/// 所以级联单独开一个事务：
///
/// - 成功 → 父任务实时跟上（正常路径，秒级）；
/// - 失败 → 调用方只记 `warn`，由巡检接口
///   `GET /business/task/admin/repair-parent-status` 兜底（方案 A + 方案 C）。
///
/// # 顺序（与 `handler::change_status` 完全同序）
///
/// 先 [`task_model::recalc_ancestors`]（沿父链重算进度）再
/// [`task_model::cascade_parent_status`]（沿父链判定状态）—— 状态判定因此看得到
/// 最新进度，两级也不会各判一次。
///
/// # 参数
///
/// - `start`：刚被审批结论推进的任务 id；
/// - `operator`：写进变更日志 / 时间线的操作人（与 [`settle`] 同口径 = 审批单发起人）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（事务已回滚，父任务保持原状，等巡检兜底）。
pub async fn cascade_after_flow(start: Uuid, operator: Uuid) -> DbResult<CascadeAfterFlow> {
    // 单测专用：注入一次「级联事务失败」（证明审批结论不会被级联拖回滚）
    #[cfg(test)]
    if CASCADE_FAIL_INJECTED.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(DbError::InvalidValue("T13 单测注入：级联失败".to_string()));
    }
    db().transaction(|tx| async move {
        let recalcs = task_model::recalc_ancestors(&tx, start, operator).await?;
        let cascades = task_model::cascade_parent_status(&tx, start, operator).await?;
        // 级联改了状态就必须留下变更日志（C7 G5：`record` 的调用点在事务闭包内）
        for cascade in &cascades {
            let before = serde_json::json!({ "status": cascade.from.code() });
            let after = serde_json::json!({ "status": cascade.to.code() });
            changelog::record(
                &tx,
                &ChangeCtx::api(None, Some(CALLBACK_OPERATOR_NAME)),
                ChangeTarget::of(
                    "biz_task",
                    cascade.task_id.to_string(),
                    Action::Status,
                    cascade.reason,
                ),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| {
                DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg))
            })?;
        }
        Ok(CascadeAfterFlow { recalcs, cascades })
    })
    .await
}

/// 最近一条「已完成待办」的处理人（审批通过时写进 `biz_stock_transfer.approver_id`）。
///
/// 多节点流程取 `completed_at` 最新的一票（就是让实例落终态的那个人）。
/// 查不到（历史数据/被清理）→ `None`，调用方按「保留原值」处理。
// SCOPE-EXEMPT: 按 `instance_id` 精确取**单条**待办（`LIMIT 1`），不是列表查询。
// 调用链与 `latest_verdict_comment` 相同：入参来自审批回调的 `biz_flow_instance.id`。
async fn latest_completed_assignee(instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT t.assignee_id FROM biz_flow_task t \
              WHERE t.instance_id = ?::uuid AND t.status = 'APPROVED' AND t.assignee_id IS NOT NULL \
              ORDER BY coalesce(t.completed_at, t.created_at) DESC LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id")?),
    }
}

/// 最近一条「有意见的待办行」的审批意见（驳回原因的回退来源）。
///
/// 引擎把同意 / 驳回意见写在 `biz_flow_task.comment`（待办行），实例行的 `result_comment`
/// 往往为空。回调要拿「为什么被驳回」当任务打回原因，所以按 `completed_at` 倒序取第一条非空。
// SCOPE-EXEMPT: 按 `instance_id` 精确取**单条**审批意见（`LIMIT 1`），不是列表查询。
// 调用链：`on_instance_settled` → 本函数，入参 instance_id 来自回调的 `biz_flow_instance.id`，
// 而该实例的可见性已由发起侧（`prepare_accept` 校验 `task_id` 归属 + `assert_task_visible`）保证。
// 这里再套数据范围反而会「因为审批人被调岗而查不到自己经手的驳回意见」，把业务打回原因弄丢。
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
// SCOPE-EXEMPT: 按 `task_id` 精确取**单条**任务（主键命中），不是列表查询。
// 调用链：流程回调 → 本函数，task_id 来自 `instance.biz_id`，该值在发起时由
// `prepare_accept` 从**已通过可见性校验**的任务行写入。
// 数据范围在这里没有意义：回调是「系统身份」，它推进的就是这一条任务。
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

// ==================== 写：任务 → 流程（删除即终止，T12） ====================

/// 任务删除时写进审批单 `result_comment` / 待办 `comment` 的文案。
///
/// 这句文案是**审计口径**的一部分（用户在「审批中心」看到的就是它），
/// 验收脚本按它做精确断言 —— 集中在这里定义，不许在别处另写一份。
pub const TASK_DELETE_CANCEL_COMMENT: &str = "任务已删除，审批单自动终止";

/// 任务被**软删**时，终止它名下所有在跑的审批单（T12）。
///
/// # 必须与删任务同一事务
///
/// 卡面决策：终止失败 → 整笔回滚。「留一个孤儿待办」比「这个任务这次删不掉」更麻烦
/// —— 孤儿 `PENDING` 实例会污染待办数、审批统计与工作量报表（用户已经踩过），
/// 而删不掉只是重试一次。
///
/// # 返回值
///
/// 三张表各被终止多少行（实例 / 待办 / 节点），用于结构化日志与 e2e 断言。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（调用方事务回滚，任务不会被删掉）。
pub async fn cancel_on_task_deleted(
    tx: &Transaction,
    task_id: Uuid,
    task_no: &str,
) -> DbResult<model::CancelledOfBiz> {
    let out =
        model::cancel_pending_of_biz(tx, task_id, task_no, TASK_DELETE_CANCEL_COMMENT).await?;
    tracing::info!(
        %task_id,
        task_no,
        instances = out.instances,
        tasks = out.tasks,
        nodes = out.nodes,
        "任务已删除，在跑审批单已终止"
    );
    Ok(out)
}

/// 任务被**推 `CANCELLED`**（不是删除）时写进审批单 `result_comment` / 待办 `comment` 的文案。
///
/// 与 [`TASK_DELETE_CANCEL_COMMENT`] 分开是为了审计口径：用户看到的是「任务已取消」，
/// 而不是「任务已删除」——两者在业务上是同一件事的两种入口，但在证据链上是不同的动作。
pub const TASK_CANCEL_COMMENT: &str = "任务已取消，审批单自动终止";

/// 任务被推 `CANCELLED` 时，撤掉它名下所有在跑的审批单（P11）。
///
/// # 为什么必须与状态流转同一事务
///
/// 铁律①「正向必配反向」：任务一旦是 `CANCELLED`，它的审批实例和待办就失去了业务依据。
/// 只改任务不留撤销，审批人的「待我审批」里会挂着一条点不动的孤儿待办
/// （老板 2026-10-31 报的 TSK-202610-000149 就是这条路径的产物）。
///
/// 与 [`cancel_on_task_deleted`] 的关系：删任务和取消任务是同一个语义的两种入口，
/// 都必须撤审批；区别只是取消任务还能反悔（若产品允许 `CANCELLED → TODO`），
/// 所以文案写「任务已取消」而不是「任务已删除」。
///
/// # 幂等
///
/// 底层 [`model::cancel_pending_of_biz`] 只动 `PENDING`：任务已是 `CANCELLED` 再取消一次
/// 不会报错、也不会把 `APPROVED` / `REJECTED` 的历史待办改成 `CANCELLED`（那是审计事实）。
///
/// # 返回值
///
/// 三张表各被终止多少行（实例 / 待办 / 节点），供 handler 结构化日志与 e2e 断言。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（调用方事务整笔回滚，任务不会停在 `CANCELLED`）。
pub async fn cancel_on_task_cancelled(
    tx: &Transaction,
    task_id: Uuid,
    task_no: &str,
) -> DbResult<model::CancelledOfBiz> {
    // 单测专用的故障注入（生产二进制里被 cfg 掉）：证明「撤审批失败 → 任务状态一起回滚」。
    #[cfg(test)]
    if CANCEL_INJECT_FAIL.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Err(DbError::InvalidValue(P11_FAIL_SENTINEL.to_string()));
    }
    let out = model::cancel_pending_of_biz(tx, task_id, task_no, TASK_CANCEL_COMMENT).await?;
    tracing::info!(
        %task_id,
        task_no,
        instances = out.instances,
        tasks = out.tasks,
        nodes = out.nodes,
        "任务已取消，在跑审批单已终止"
    );
    Ok(out)
}

/// 单测故障注入开关（`#[cfg(test)]` 关掉的生产路径见 [`cancel_on_task_cancelled`]）。
#[cfg(test)]
pub(crate) static CANCEL_INJECT_FAIL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// 注入失败时回传的哨兵文案（单测用它区分「注入的错」与「真库的错」）。
#[cfg(test)]
pub(crate) const P11_FAIL_SENTINEL: &str = "P11-FAIL-INJECTED";

// ==================== 写：任务状态变更 → 流程节点（P-FLOW-1B） ====================
//
// 本节落地设计 §3.4 / §4.5 Q2 的**四个耦合点**（有且只有这四个）：
//
// | # | 触发 | 任务侧 | 流程侧 | 谁写 |
// |---|---|---|---|---|
// | ① | 任务进入 `DONE` | `DONE` | 节点 `ACTIVE→APPROVED` + 驱动下游 | [`on_task_status_changed`] |
// | ② | 任务 `REVIEW→DOING`（打回） | `DOING` | 按 `on_reject` | 同上 |
// | ③ | 任务进入 `CANCELLED` | `CANCELLED` | 按 `on_cancel` | 同上 |
// | ④ | 实例进入终态 | 未完成任务 → `CANCELLED` | — | `flow::model::cancel_flow_tasks` |
//
// **红线（卡面）**：`ActorKind::FlowCallback` 只在本文件出现。HTTP handler 一律
// `ActorKind::User`；而本节的所有写入都**不经过** `task::state::check_transition` 的反向
// （任务 → 流程）—— 任务侧的状态已经由 handler 用 `ActorKind::User` 判过了，这里只动流程。

/// 任务状态变更回调的四种结果（设计 §3.4）。
///
/// # 为什么没有第五种「Nothing」
///
/// 设计只给了 4 个值，而映射表里「其它 → 不动」的行需要一种「什么都没发生但也不报错」的表达。
/// 这里用 [`FlowTaskOutcome::Ignored`] 承担它：语义是「本次变更不推进流程」，
/// `instance_status` 说明当时实例是什么状态（`"PENDING"` 就是「流程在跑，但这次不该动」）。
#[derive(Debug)]
pub enum FlowTaskOutcome {
    /// 不是流程任务（绝大多数请求走这里 —— 普通任务的 `change_status` 逐字节不变）。
    NotFlowTask,
    /// 流程侧已被本次变更推进：节点通过并驱动下游 / 退回上一节点 / 按 `on_cancel` 终止实例。
    ///
    /// `next_nodes` 是本次驱动到的下游节点（终止 / 挂起等审批的场景为空）。
    Advanced {
        /// 本次激活的下游节点 key。
        next_nodes: Vec<String>,
    },
    /// 本次变更**不推进**流程（映射表里「不动」的行；或实例已离开 `PENDING` 且无事可做）。
    Ignored {
        /// 当时实例的状态（读不到实例时为 `"MISSING"`）。
        instance_status: String,
    },
    /// 业务冲突：任务要求推进流程，但流程侧已经不允许 → 调用方**整笔回滚**并返回这个错误。
    ///
    /// 与 `Ignored` 的分界（设计 §7.2 验收 5 的取舍）：**任务已完成/已打回是用户该知道的事**，
    /// 静默成功会让前端显示「完成」而流程没动。所以这里选 `Conflict` 而不是 `Ignored`。
    Conflict(ApiError),
}

/// 一次任务状态变更的上下文（由 `task::handler::change_status` 在事务里构造）。
#[derive(Debug, Clone, Copy)]
pub struct TaskStatusChanged<'a> {
    /// 任务 id。
    pub task_id: Uuid,
    /// 生成它的流程实例（`biz_task.flow_instance_id`；非流程任务为 `None`）。
    pub instance_id: Option<Uuid>,
    /// 生成它的节点 key（`biz_task.flow_node_key`）。
    pub node_key: Option<&'a str>,
    /// 生成时的轮次（`biz_task.flow_round`）。
    pub round: Option<i32>,
    /// 变更前状态。
    pub from: TaskStatus,
    /// 变更后状态。
    pub to: TaskStatus,
    /// 操作人（写进流程侧日志 / 审批单发起人）。
    pub operator: Uuid,
    /// 操作人姓名（日志用）。
    pub operator_name: &'a str,
    /// 打回 / 取消原因（`REVIEW→DOING` 与 `→CANCELLED` 必填，见任务状态机）。
    pub reason: Option<&'a str>,
}

/// 任务状态变更在流程侧的**动作**（纯枚举，便于把映射表写成可断言的形式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeAction {
    /// 不动（映射表里「其它 / 等验收 / 重开」的行）。
    Nothing,
    /// 节点 `ACTIVE → APPROVED` 并驱动下游。
    Approve,
    /// `on_complete=APPROVE`：节点保持 `ACTIVE`，同事务发起审批单（`biz_type = approve_biz_type`）。
    StartApproval,
    /// 退回上一节点（`round + 1`，复用引擎 `apply_return`）。
    ReturnToPrev,
    /// 终止实例（`REJECTED` = 驳回终止；`CANCELLED` = 行政终止）。
    Terminate(&'static str),
}

/// 任务 `from→to` × 节点配置 → 流程动作（**纯函数**，设计 §3.4 的映射表逐行落地）。
///
/// | 任务 `from → to` | 节点动作 |
/// |---|---|
/// | `DOING → DONE`（`on_complete != APPROVE`） | `Approve` |
/// | `DOING → DONE`（`on_complete = APPROVE`） | `StartApproval`（节点仍 `ACTIVE`，等审批） |
/// | `REVIEW → DONE`（验收通过） | `Approve`（审批已在验收环节发生过，不再发起第二次） |
/// | `DOING → REVIEW` | `Nothing`（节点保持 `ACTIVE`，等验收） |
/// | `REVIEW → DOING`（打回） | 按 `on_reject` |
/// | `DONE → DOING`（重开） | `Nothing`（任务重开不改流程） |
/// | 任意 → `CANCELLED` | 按 `on_cancel` |
/// | 其它 | `Nothing`（保守） |
///
/// # 为什么 `REVIEW → DONE` 不看 `on_complete`
///
/// R18 禁止 `APPROVE + need_review=true`，所以「有 `REVIEW` 态」的节点必然不是 `APPROVE`；
/// 即便如此也不复用 `StartApproval` 分支 —— 万一是旧快照 / 绕过校验的数据，
/// 走 `Approve` 至少不会发起第二张审批单。
pub fn node_action_of(from: TaskStatus, to: TaskStatus, cfg: &TaskNodeConfig) -> NodeAction {
    match (from, to) {
        (TaskStatus::Doing, TaskStatus::Done) => match cfg.on_complete {
            OnComplete::Approve => NodeAction::StartApproval,
            OnComplete::Direct | OnComplete::Verify => NodeAction::Approve,
        },
        (TaskStatus::Review, TaskStatus::Done) => NodeAction::Approve,
        (TaskStatus::Review, TaskStatus::Doing) => match cfg.on_reject {
            OnReject::Stay => NodeAction::Nothing,
            OnReject::BackToPrev => NodeAction::ReturnToPrev,
            OnReject::Terminate => NodeAction::Terminate("REJECTED"),
        },
        // 取消：**只认** `TODO / DOING / REVIEW` 三个来源态。
        //
        // 设计文档写「任意 → CANCELLED」，但状态机的 10 条边里明确**没有**
        // `DONE → CANCELLED`（`state.rs` 实测，设计 §7.2 硬结论 1）。这里按可达集收窄：
        // 万一上层传进 `DONE → CANCELLED`（绕过状态机的脏调用），也不许把**已经干完的活**
        // 变成"流程被取消" —— 那等于用一条非法边把完成率改小。
        (TaskStatus::Todo | TaskStatus::Doing | TaskStatus::Review, TaskStatus::Cancelled) => {
            match cfg.on_cancel {
                OnCancel::Stay => NodeAction::Nothing,
                OnCancel::BackToPrev => NodeAction::ReturnToPrev,
                OnCancel::Terminate => NodeAction::Terminate("CANCELLED"),
            }
        }
        // `DOING→REVIEW`（等验收）/ `DONE→DOING`（重开）/ `TODO→DOING`（开始）/
        // `CANCELLED→TODO`（复活）/ 同状态 / 不可达组合：流程都不该动
        // （设计 §3.4「其它 → 不动」）
        _ => NodeAction::Nothing,
    }
}

/// 流程任务行上回调需要的列（一次查询取全）。
#[derive(Debug, Clone)]
struct FlowBridgeTaskRow {
    task_no: String,
    title: String,
    source: String,
    flow_instance_id: Option<Uuid>,
    flow_node_key: Option<String>,
    flow_round: Option<i32>,
    approval_instance_id: Option<Uuid>,
}

// SCOPE-EXEMPT: 按 `task_id` 精确取**单条**任务（主键命中），不是列表查询。
// 调用链：`change_status` 事务内（task_id 来自路径参数，已过 `find_detail` 可见性校验）
// 与审批回调（task_id 来自 `instance.biz_id`，发起时已校验归属）。
async fn flow_bridge_row(
    ex: &impl SqlExecutor,
    task_id: Uuid,
) -> DbResult<Option<FlowBridgeTaskRow>> {
    let Some(row) = ex
        .query_first_sql(
            "SELECT t.task_no, t.title, t.source, t.flow_instance_id, \
                    t.flow_node_key, t.flow_round, t.approval_instance_id \
               FROM biz_task t WHERE t.id = ?::uuid AND t.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&task_id)],
        )
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(FlowBridgeTaskRow {
        task_no: row.get_typed("task_no")?,
        title: row.get_typed("title")?,
        source: row.get_typed("source")?,
        flow_instance_id: row.get_typed("flow_instance_id")?,
        flow_node_key: row.get_typed("flow_node_key")?,
        flow_round: row.get_typed("flow_round")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
    }))
}

/// `on_reject` 等节点策略从「实例绑定的发布快照」里读（**运行时唯一依据**）。
///
/// # 为什么读快照而不是读定义行
///
/// 实例跑在 `instance.version_id` 那一版上（架构 D2）。发布之后定义被改、被回滚都不该影响
/// 在跑的实例 —— 与 `engine::load_snapshot` 同口径。
///
/// # 返回值
///
/// `Ok(None)` = 快照里没有这个节点（→ 调用方按「不动」处理）；
/// `Err` = 快照缺失 / JSON 解析失败（→ **响亮失败**，不静默降级成 DIRECT）。
async fn task_node_config_of(
    ex: &impl SqlExecutor,
    instance: &model::InstanceRow,
    node_key: &str,
) -> DbResult<Option<TaskNodeConfig>> {
    let Some(raw) = model::load_snapshot_in(ex, instance.version_id).await? else {
        tracing::warn!(
            instance_id = %instance.id,
            version_id = %instance.version_id,
            "流程实例绑定的版本没有发布快照，任务状态回调无法判定节点策略"
        );
        return Ok(None);
    };
    let snapshot = graph::parse_snapshot(&raw).map_err(|error| {
        DbError::InvalidValue(format!(
            "流程实例 {} 的发布快照无法解析：{error}",
            instance.instance_no
        ))
    })?;
    let Some(spec) = snapshot.nodes.iter().find(|node| node.key == node_key) else {
        return Ok(None);
    };
    node_config::parse_task_config(&spec.config["task"])
        .map(Some)
        .map_err(|error| {
            // 发布期 R18 已经校验过一遍；这里失败说明快照被绕过改过库。
            // **不许静默当成 DIRECT**（那就成了「配置写错了但看起来跑得动」）。
            DbError::InvalidValue(format!(
                "流程实例 {} 的节点 {node_key} 任务配置非法：{}",
                instance.instance_no, error.msg
            ))
        })
}

/// 任务状态变更 → 流程节点推进（**必须在 `change_status` 的事务内调用**）。
///
/// # 参数
///
/// - `tx`：任务状态写入所在的事务（同生共死：流程推不动 → 任务状态一起回滚）；
/// - `ev`：任务侧的身份与 `from/to`（见 [`TaskStatusChanged`]）。
///
/// # 返回值
///
/// [`FlowTaskOutcome`]。**本函数不返回 `Conflict` 的 `Err`**：冲突以
/// `FlowTaskOutcome::Conflict(ApiError)` 返回，由 handler 把它变成「哨兵 + 回滚」
/// （`ApiError` 既不能进 `DbError` 也不能 `Clone`）。
///
/// # 异常情况
///
/// - 快照缺失 / 配置非法 → [`DbError::InvalidValue`]（整笔回滚，**不静默跳过**）；
/// - 数据库异常 → [`DbError`]。
pub async fn on_task_status_changed(
    tx: &Transaction,
    ev: TaskStatusChanged<'_>,
) -> DbResult<FlowTaskOutcome> {
    // ① 三列缺任何一个都当「不是流程任务」：`source='FLOW'` 却缺列说明数据损坏，
    //    这时推进流程比不动更危险（不知道该推哪个节点）。
    let (Some(instance_id), Some(node_key), Some(round)) = (ev.instance_id, ev.node_key, ev.round)
    else {
        tracing::warn!(
            task_id = %ev.task_id,
            "任务标记为流程任务但 flow_instance_id / node_key / round 有缺失，跳过流程回调"
        );
        return Ok(FlowTaskOutcome::NotFlowTask);
    };

    // ② 实例行（**锁**：与引擎同一条锁序，保证并发点两次完成时串行判定）
    let Some(instance) = model::lock_instance(tx, instance_id).await? else {
        tracing::warn!(task_id = %ev.task_id, %instance_id, "任务所属的流程实例不存在，跳过流程回调");
        return Ok(FlowTaskOutcome::Ignored {
            instance_status: "MISSING".to_string(),
        });
    };

    // ③ 节点配置（决定 on_complete / on_reject / on_cancel）
    let Some(cfg) = task_node_config_of(tx, &instance, node_key).await? else {
        tracing::warn!(
            task_id = %ev.task_id,
            %instance_id,
            node_key,
            "发布快照里没有该任务节点，跳过流程回调"
        );
        return Ok(FlowTaskOutcome::Ignored {
            instance_status: instance.status.clone(),
        });
    };

    // ④ 映射表（纯函数）
    let action = node_action_of(ev.from, ev.to, &cfg);
    if action == NodeAction::Nothing {
        return Ok(FlowTaskOutcome::Ignored {
            instance_status: instance.status.clone(),
        });
    }

    // ⑤ 节点现状（只用于**选错误码**，真正的写仍由引擎在锁内判定）
    let node = model::find_node(tx, instance_id, node_key, round).await?;

    // ⑥ 需要实例还在跑的三种动作：「完成 → 通过/发起审批」必须 PENDING；
    //    「退回 / 终止」在实例已结束时**无事可做**（任务侧照常，不报错）。
    //
    //    实例不是 PENDING 时有两种不同的诊断，**不能都报 65038**：
    //
    //    | 节点现状 | 含义 | 码 |
    //    |---|---|---|
    //    | 本轮已 `APPROVED` | 刚才是同一个人 / 并发点的第二次完成 | `65020`（「已处理」）|
    //    | 其它（`CANCELLED` / `SKIPPED` / 无行） | 流程被终止 / 撤回，任务却要往前推 | `65038`（真冲突）|
    //
    //    只按「实例是否 PENDING」判会让并发重复完成也报 65038，前端就分不清
    //    「你的活已经算完了」和「这张流程没了，去找发起人」。
    let needs_pending = matches!(action, NodeAction::Approve | NodeAction::StartApproval);
    if !instance.is_pending() {
        if needs_pending {
            let node_approved = matches!(
                node.as_ref().map(|row| row.status.as_str()),
                Some("APPROVED")
            );
            if node_approved {
                return Ok(FlowTaskOutcome::Conflict(ApiError::new(
                    409,
                    error_code::FLOW_TASK_ALREADY_DONE,
                    format!(
                        "流程节点「{node_key}」第 {round} 轮已经完成过，本次重复提交不再推进流程"
                    ),
                )));
            }
            return Ok(FlowTaskOutcome::Conflict(ApiError::new(
                409,
                error_code::FLOW_TASK_STATE_CONFLICT,
                format!(
                    "流程「{}」已结束（{}），任务不能再推进流程；请联系流程发起人重新发起",
                    instance.flow_name, instance.status
                ),
            )));
        }
        tracing::info!(
            task_id = %ev.task_id,
            %instance_id,
            instance_status = %instance.status,
            "流程实例已结束，本次任务变更不再改动流程"
        );
        return Ok(FlowTaskOutcome::Ignored {
            instance_status: instance.status.clone(),
        });
    }

    // ⑥ 执行
    match action {
        NodeAction::Nothing => Ok(FlowTaskOutcome::Ignored {
            instance_status: instance.status.clone(),
        }),
        NodeAction::Approve => {
            let out =
                engine::complete_task_node(tx, instance_id, node_key, round, ev.operator).await?;
            finish_outcome(out, ev, node_key)
        }
        NodeAction::StartApproval => {
            let started = start_task_node_approval(tx, &ev, &cfg, node_key, round).await?;
            tracing::info!(
                task_id = %ev.task_id,
                %instance_id,
                node_key,
                approval_instance_id = ?started,
                biz_type = cfg.approve_biz_type.as_deref().unwrap_or(""),
                "任务完成（on_complete=APPROVE）：节点保持 ACTIVE，已发起审批单"
            );
            Ok(FlowTaskOutcome::Advanced {
                next_nodes: Vec::new(),
            })
        }
        NodeAction::ReturnToPrev => {
            let comment = ev.reason.unwrap_or("任务被打回，流程退回上一节点");
            let out =
                engine::return_task_node(tx, instance_id, node_key, round, ev.operator, comment)
                    .await?;
            match out.rejected {
                // 节点已经不是 ACTIVE（别人先动过）：流程不能后退，但任务打回本身是合法的
                // → 记 warn 后按 Ignored 处理，不把用户的「打回」变成 500/409。
                Some(error) => {
                    tracing::warn!(
                        task_id = %ev.task_id,
                        %instance_id,
                        code = error.code,
                        message = %error.msg,
                        "任务打回时流程节点已不可退回（保持不动）"
                    );
                    Ok(FlowTaskOutcome::Ignored {
                        instance_status: instance.status.clone(),
                    })
                }
                None => Ok(FlowTaskOutcome::Advanced {
                    next_nodes: out.next_nodes,
                }),
            }
        }
        NodeAction::Terminate(status) => {
            let comment = ev.reason.unwrap_or("任务被取消，流程按 on_cancel 策略终止");
            let out = engine::terminate_instance(tx, instance_id, status, comment).await?;
            match out.rejected {
                Some(error) => {
                    tracing::warn!(
                        task_id = %ev.task_id,
                        %instance_id,
                        code = error.code,
                        message = %error.msg,
                        "任务取消时流程实例已不可终止（保持不动）"
                    );
                    Ok(FlowTaskOutcome::Ignored {
                        instance_status: instance.status.clone(),
                    })
                }
                None => Ok(FlowTaskOutcome::Advanced {
                    next_nodes: Vec::new(),
                }),
            }
        }
    }
}

/// `engine::complete_task_node` 的 `ActionOut` → [`FlowTaskOutcome`]。
///
/// 引擎的「业务拒绝」在这里一律升级成 `Conflict`（任务已完成 / 实例已结束都是用户该知道的），
/// 引擎的「下游驱动失败」也一样 —— 那说明流程图本身有问题，静默成功会把问题埋掉。
fn finish_outcome(
    mut out: engine::ActionOut,
    ev: TaskStatusChanged<'_>,
    node_key: &str,
) -> DbResult<FlowTaskOutcome> {
    match out.rejected.take() {
        Some(error) => {
            tracing::warn!(
                task_id = %ev.task_id,
                node_key,
                code = error.code,
                message = %error.msg,
                "任务完成未能推进流程节点"
            );
            Ok(FlowTaskOutcome::Conflict(error))
        }
        None => {
            tracing::info!(
                task_id = %ev.task_id,
                node_key,
                instance_status = %out.instance_status,
                next_nodes = ?out.next_nodes,
                "任务完成已推进流程节点"
            );
            Ok(FlowTaskOutcome::Advanced {
                next_nodes: out.next_nodes,
            })
        }
    }
}

/// `on_complete=APPROVE`：**事务内**发起审批单并回写 `biz_task.approval_instance_id`。
///
/// 与 `TASK_ACCEPT` 那条（[`prepare_accept`] + [`start_in_tx`]）的区别只有两点：
///
/// | | `TASK_ACCEPT`（验收） | `APPROVE`（本次） |
/// |---|---|---|
/// | 发起时机 | `DOING → REVIEW` | `DOING → DONE` |
/// | `biz_type` | 固定 `TASK_ACCEPT` | `config.task.approve_biz_type`（可配） |
///
/// 回写列、发起引擎、后续回调路径（`approval_instance_id` + [`on_instance_settled`]）完全复用。
///
/// # 幂等
///
/// 同一任务已有在跑的审批单 → `Ok(None)`（重复点完成不撞 `uk_biz_flow_instance_biz_active`）。
///
/// # 异常情况
///
/// - `approve_biz_type` 没有已发布的默认流程 / 版本行缺失 / 发起被拒 →
///   [`DbError::InvalidValue`]（**整笔回滚**：任务不许停在「已完成但没发起审批」）；
/// - 数据库异常 → [`DbError`]。
async fn start_task_node_approval(
    tx: &Transaction,
    ev: &TaskStatusChanged<'_>,
    cfg: &TaskNodeConfig,
    node_key: &str,
    round: i32,
) -> DbResult<Option<Uuid>> {
    let Some(biz_type) = cfg
        .approve_biz_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(DbError::InvalidValue(
            "on_complete=APPROVE 的节点缺少 approve_biz_type（发布期 R18 应拦住）".to_string(),
        ));
    };
    // ① 幂等：已有在跑的审批单就不重复发起
    if model::biz_instance_running_in(tx, biz_type, ev.task_id).await? {
        tracing::info!(
            task_id = %ev.task_id,
            biz_type,
            "该任务已有在跑的审批单，本次完成不再重复发起"
        );
        return Ok(None);
    }
    // ② 流程定义（必须已发布且是默认）
    let Some(def) = model::default_def_of_biz_type_in(tx, biz_type).await? else {
        return Err(DbError::InvalidValue(format!(
            "on_complete=APPROVE 指定的审批流 {biz_type} 没有已发布的流程（请先发布并设为默认）"
        )));
    };
    let Some(version_id) = def.current_version_id else {
        return Err(DbError::InvalidValue(format!(
            "审批流 {biz_type} 没有当前发布版本"
        )));
    };
    let Some(version) = model::find_version_in(tx, version_id).await? else {
        return Err(DbError::InvalidValue(format!(
            "审批流 {biz_type} 的发布版本行不存在"
        )));
    };
    // ③ 任务行（标题 / 单号写进审批单，审批中心里能对上）
    let Some(row) = flow_bridge_row(tx, ev.task_id).await? else {
        return Err(DbError::InvalidValue(format!(
            "任务 {} 不存在或已删除，无法发起审批",
            ev.task_id
        )));
    };
    let (initiator_name, initiator_dept_id) = model::user_profile_in(tx, ev.operator)
        .await?
        .unwrap_or((String::new(), None));
    let title = format!("{} {}", row.task_no, row.title)
        .trim()
        .chars()
        .take(200)
        .collect::<String>();

    let instance_id = Uuid::now_v7();
    let req = engine::StartReq {
        instance_id,
        def_id: def.id,
        version_id,
        version_no: version.version_no,
        flow_code: def.flow_code.clone(),
        flow_name: def.flow_name.clone(),
        biz_type: def.biz_type.clone(),
        biz_id: Some(ev.task_id),
        biz_no: Some(row.task_no.clone()),
        title,
        initiator_id: ev.operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        // 条件节点的表达式作用域：与 TASK_ACCEPT 那条同口径（任务摘要，不外泄正文）
        form_data: serde_json::json!({
            "task_id": ev.task_id.to_string(),
            "task_no": row.task_no,
            "title": row.title,
            "flow_instance_id": ev.instance_id.map(|id| id.to_string()),
            "flow_node_key": node_key,
            "flow_round": round,
        }),
    };
    let out = engine::start(tx, &req).await?;
    if let Some(error) = out.rejected.as_ref() {
        // 发起被拒（派不出审批人 / 图上配置错）：整笔回滚，业务码原样返回
        return Err(DbError::InvalidValue(format!(
            "发起审批失败：{}",
            error.msg
        )));
    }
    let instance_id = out.instance_id.unwrap_or(instance_id);
    // ④ 回写任务行（门闸与回调都认这一列）
    tx.exec_sql(
        "UPDATE biz_task SET approval_instance_id = ?::uuid, updated_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&instance_id), Value::from(&ev.task_id)],
    )
    .await?;
    tracing::info!(
        task_id = %ev.task_id,
        %instance_id,
        biz_type = %def.biz_type,
        initiator = %ev.operator,
        operator_name = ev.operator_name,
        node_key,
        round,
        "任务完成（on_complete=APPROVE）已自动发起审批单"
    );
    Ok(Some(instance_id))
}

/// 审批单落终态后，**推进流程节点**（`on_complete=APPROVE` 与「验收通过」两条路共用）。
///
/// # 为什么不能只靠 [`settle`]
///
/// [`settle`] 只做「`REVIEW → DONE|DOING`」（`callback_transition` 的口径）。但流程节点
/// 还要往前推，而那是 `flow` 侧的事：
///
/// | 审批单 | 任务此时 | 节点该做什么 |
/// |---|---|---|
/// | `TASK_ACCEPT` 通过（`need_review=true`） | `REVIEW → DONE`（settle 刚做完） | `ACTIVE → APPROVED` + 驱动下游 |
/// | `APPROVE` 模式通过 | 已经是 `DONE`（完成时就 DONE 了） | `ACTIVE → APPROVED` + 驱动下游 |
/// | 驳回 | `REVIEW → DOING`（settle 做）或仍 `DONE` | 按 `on_reject` |
///
/// 三种情况都只认一件事：**任务行上 `approval_instance_id` 指向这张审批单**，且它是一张
/// `source='FLOW'` 的流程任务。
///
/// # 时序与失败口径
///
/// 跑在审批事务**提交之后**（与 [`cascade_after_flow`] 同款）：开独立事务，失败只记 `warn`。
/// 审批结论已经落库，不能因为它回滚；流程节点没推上去由「任务详情页的流程条」暴露出来，
/// 重新点一次完成 / 重新验收即可恢复。
///
/// # 返回值
///
/// 一段可直接进日志的说明（`None` = 与流程节点无关）。
pub async fn on_task_approval_settled(
    approval_instance_id: Option<Uuid>,
    instance_status: &str,
) -> Option<String> {
    let approval_instance_id = approval_instance_id?;
    match settle_flow_node(approval_instance_id, instance_status).await {
        Ok(Some(note)) => {
            tracing::info!(
                %approval_instance_id,
                instance_status,
                note,
                "审批结论已回调流程节点"
            );
            Some(note)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %approval_instance_id,
                instance_status,
                %error,
                "审批结论回调流程节点失败（不影响审批单；任务详情页仍显示节点未通过）"
            );
            None
        }
    }
}

/// [`on_task_approval_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle_flow_node(
    approval_instance_id: Uuid,
    instance_status: &str,
) -> DbResult<Option<String>> {
    // ① 审批单 → 业务 id（= 任务 id），且必须是流程任务 + 这张单就是它挂的那张
    let Some(approval) = model::find_instance(approval_instance_id).await? else {
        return Ok(None);
    };
    let Some(task_id) = approval.biz_id else {
        return Ok(None);
    };
    let Some(row) = flow_bridge_row(db(), task_id).await? else {
        return Ok(None);
    };
    if row.source != task_node::FLOW_SOURCE {
        return Ok(None);
    }
    if row.approval_instance_id != Some(approval_instance_id) {
        // 历史行 / 别的审批单：不动（`approval_instance_id` 是指针，不是猜测）
        return Ok(None);
    }
    let (Some(flow_instance_id), Some(node_key), Some(round)) = (
        row.flow_instance_id,
        row.flow_node_key.as_deref(),
        row.flow_round,
    ) else {
        return Ok(None);
    };

    // ② 落点：通过 → 节点通过；驳回 → 按 on_reject
    let (action, comment) = match instance_status.trim().to_ascii_uppercase().as_str() {
        INSTANCE_APPROVED => (NodeAction::Approve, "审批通过".to_string()),
        INSTANCE_REJECTED => {
            let reason = latest_verdict_comment(approval_instance_id)
                .await?
                .unwrap_or_else(|| "审批驳回".to_string());
            // `on_reject` 要从**流程实例**的快照里读（不是这张审批单的），
            // 因为策略属于「那个任务节点的配置」，审批单只提供结论。
            let Some(flow_instance) = model::find_instance(flow_instance_id).await? else {
                return Ok(None);
            };
            let Some(cfg) = task_node_config_of(db(), &flow_instance, node_key).await? else {
                return Ok(None);
            };
            match cfg.on_reject {
                OnReject::BackToPrev => (NodeAction::ReturnToPrev, reason),
                OnReject::Terminate => (NodeAction::Terminate("REJECTED"), reason),
                // `STAY`：节点保持 ACTIVE，等任务改完再走（「返工」本来就该等）
                OnReject::Stay => {
                    return Ok(Some("驳回：按 on_reject=STAY 保持节点不动".to_string()));
                }
            }
        }
        _ => return Ok(None),
    };

    // ③ 独立事务执行（审批已提交，这里失败不回滚审批）
    let note = db()
        .transaction(|tx| async move {
            let out = match action {
                NodeAction::Approve => {
                    engine::complete_task_node(
                        &tx,
                        flow_instance_id,
                        node_key,
                        round,
                        approval.initiator_id,
                    )
                    .await?
                }
                NodeAction::ReturnToPrev => {
                    engine::return_task_node(
                        &tx,
                        flow_instance_id,
                        node_key,
                        round,
                        approval.initiator_id,
                        &comment,
                    )
                    .await?
                }
                NodeAction::Terminate(status) => {
                    engine::terminate_instance(&tx, flow_instance_id, status, &comment).await?
                }
                NodeAction::Nothing | NodeAction::StartApproval => {
                    return Ok(None);
                }
            };
            if let Some(error) = out.rejected.as_ref() {
                return Err(DbError::InvalidValue(format!(
                    "流程节点推进被拒 [{}] {}",
                    error.code, error.msg
                )));
            }
            Ok(Some(format!(
                "节点 {node_key} 第 {round} 轮已按审批结论处理（实例 {}）",
                out.instance_status
            )))
        })
        .await?;
    Ok(note)
}

// ==================== 第二类业务单据：库存调拨单（批次 D · W3b） ====================

/// 库存调拨单的业务类型（`biz_flow_def.biz_type`，与
/// [`crate::business::erp::transfer_model::published_transfer_flow`] 的查询条件同口径）。
pub const STOCK_TRANSFER_BIZ_TYPE: &str = "STOCK_TRANSFER";

/// 审批单落终态后推进**库存调拨单**（`FlowCallback` 身份）。
///
/// 与任务那条（[`on_instance_settled`]）完全并列的第二类业务回调，**两者互不影响**：
///
/// | 实例状态 | 调拨单 | 依据 |
/// |---|---|---|
/// | `APPROVED` | `PENDING → APPROVED` | 审批通过 = 可以发货 |
/// | `REJECTED` | `PENDING → DRAFT`（打回） | 审批意见写 `reason`，`submitted_at` / `approved_at` 清回 NULL（`ck_biz_transfer_state` 的 DRAFT 分支要求时间戳全空） |
/// | 其它（`PENDING` / `CANCELLED` / `WITHDRAWN`） | 不动 | 还在跑 / 已撤回 |
///
/// # 为什么不让它走 [`settle`]
///
/// [`settle`] 的判据是 `biz_type == TASK_ACCEPT` 且目标状态是任务 5 态；
/// 调拨单是另一张表、另一套 6 态、另一套时间戳约束。把两者塞进一个函数只会让
/// 「谁的 biz_id 指到谁」变成运行期猜谜。共同点是**时序**：都跑在审批事务提交之后、
/// 失败只记 `warn`（审批结论已落库，不许因业务侧失败而回滚审批单）。
///
/// # 返回值
///
/// 被推进的调拨单 id（没推进 / 不该推进 → `None`）。本函数**不返回错误**。
///
/// # 幂等
///
/// 重复回调时调拨单已不在 `PENDING` → [`crate::business::erp::transfer_model::settle_approval`]
/// 的 `WHERE status = 'PENDING'` 命中 0 行 → 返回 `None`，不报错。
pub async fn on_transfer_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let approved = match instance_status.trim().to_ascii_uppercase().as_str() {
        INSTANCE_APPROVED => true,
        INSTANCE_REJECTED => false,
        _ => return None,
    };
    match settle_transfer(instance_id, approved).await {
        Ok(Some(transfer_id)) => {
            tracing::info!(
                %instance_id,
                %transfer_id,
                approved,
                "审批结论已回调调拨单状态"
            );
            Some(transfer_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(%instance_id, %error, "审批结论回调调拨单失败（不影响审批单）");
            None
        }
    }
}

/// [`on_transfer_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle_transfer(instance_id: Uuid, approved: bool) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是调拨类的（别的业务类型与本回调无关）
    let Some(instance) = model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过调拨单回调");
        return Ok(None);
    };
    if instance.biz_type != STOCK_TRANSFER_BIZ_TYPE {
        return Ok(None);
    }
    let Some(transfer_id) = instance.biz_id else {
        return Ok(None);
    };

    // ② 调拨单当前态（只推进 PENDING）
    let Some(row) = erp_transfer::find_by_id(transfer_id).await? else {
        tracing::warn!(%transfer_id, "调拨单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ★ C1（t_1e174431）：写目标状态前先校验调拨单当前状态还在不在「能接受这个审批结论」
    //   的合法路径上（状态机边 `PENDING → APPROVED|DRAFT`，不写 if-else 硬判）。
    //   不在路径上（已取消 / 已被别的路径推走）→ 不落状态 + WARN + 审批实例留痕。
    let target = if approved {
        erp_transfer::STATUS_APPROVED
    } else {
        erp_transfer::STATUS_DRAFT
    };
    if !settle_guard::gate(
        &instance,
        crate::business::shared::state_machine::Entity::Transfer,
        &row.transfer_no,
        &row.status,
        erp_transfer::STATUS_PENDING,
        target,
    )
    .await
    {
        return Ok(None);
    }

    // ③ 驳回意见：实例行的 `result_comment` 常为空（引擎把意见写在待办行），读不到就回退
    let opinion = match instance
        .result_comment
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(text) => Some(text.to_string()),
        None => latest_verdict_comment(instance_id).await?,
    };
    let opinion_for_tx = opinion.clone();
    // 审批人（`approver_id` 落库用）：引擎把「谁点的同意」写在待办行的 `assignee_id`，
    // 实例行没有这个字段 —— 从最近一条已完成的待办里取（多节点流程取最后一票）。
    let approver = latest_completed_assignee(instance_id).await?;
    // 时间点在 Rust 侧算好（与日志 / 详情同一个值）
    let now = chrono::Utc::now();
    // 联动上下文要用的快照（事务闭包里按值捕获，避免闭包内外借同一份 row）
    let transfer_no_for_linkage = row.transfer_no.clone();
    let transfer_no_in_tx = transfer_no_for_linkage.clone();
    let creator = row.create_by;
    let (affected, linkage_run) = db()
        .transaction(move |tx| async move {
            let affected = erp_transfer::settle_approval(
                &tx,
                transfer_id,
                approved,
                opinion_for_tx.as_deref(),
                approver,
                now,
            )
            .await?;
            if affected == 0 {
                // CAS 落空（单据已被别的路径推走）→ 不触发联动，与「状态没变就不通知」同口径
                return Ok((0u64, linkage::LinkageRun::default()));
            }
            // LNK-01：审批回调是**独立于 `change_transfer_status` 的状态变更点**。
            // 只接人工流转会让「配了已发布流程」的生产环境里 STOCK_TRANSFER/APPROVED
            // 的规则永远不触发（P1-FIX-5C 那类静默失效），所以这里同样走通用引擎，
            // 排除清单与人工流转**共用** `transfer_linkage` 里的那一份。
            let run = transfer_linkage::run_in_tx(
                &tx,
                transfer_id,
                &transfer_no_in_tx,
                target,
                approver,
                creator,
            )
            .await
            .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
            Ok((affected, run))
        })
        .await?;
    if affected == 0 {
        return Ok(None);
    }
    // 提交**之后**才跑 AfterCommit 动作（发通知 / 建任务）：失败只记日志，不回滚审批结论。
    transfer_linkage::run_deferred_after_commit(
        transfer_id,
        &transfer_no_for_linkage,
        target,
        approver,
        creator,
        linkage_run,
    )
    .await;
    if !approved && let Some(opinion) = opinion.as_deref() {
        tracing::info!(%transfer_id, opinion, "调拨单被审批驳回，已打回草稿");
    }
    Ok(Some(transfer_id))
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

// ==================== 单测：审批回调后的父任务级联（T13，真库 `TASK_DB_URL`） ====================
//
// 为什么必须真库：本卡交付的是「审批结论落库之后，父任务链跟不跟得上」——
// 纯逻辑单测证明不了「settle 写完任务后真的调了级联」「级联失败真的不阻塞审批」
// 「三层链路真的逐级爬」。所以这里走的是**生产同款路径**：
//
// ```text
// 夹具：插入一行已落终态的 biz_flow_instance（APPROVED / REJECTED）
//   → task_bridge::on_instance_settled(Some(instance_id), "<状态>")   ← 审批回调的入口
//   → settle() 写任务状态（自己一笔事务）
//   → cascade_after_flow() 写父任务（独立一笔事务）                    ← 本卡新增
// ```
//
// 用法：
// ```bash
// export TASK_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/<测试库>'
// cargo test --lib business::flow::task_bridge::flow_cascade_tests -- --nocapture --test-threads=1
// ```
// 缺 `TASK_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。
// 测试库必须是**迁移跑全**的库（夹具要读 V0127 种下的 `TASK_ACCEPT` 流程定义行）。
#[cfg(test)]
mod flow_cascade_tests {
    use super::*;

    /// 夹具用户（`biz_task.owner_id / creator_id` 与 `biz_flow_instance.initiator_id` 都是
    /// NOT NULL + FK `sys_user`）。
    const U_ID: &str = "7a8a0000-0000-4000-8000-0000000000d1";
    /// 真库用例**串行**执行（共用一个库、夹具是固定 UUID，并行会互删）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `TASK_DB_URL`，用例直接跳过。
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
                // 每个 `#[tokio::test]` 都是独立 runtime，而 `db()` 是进程级全局池：
                // 空闲连接绑在创建它的 runtime 上，runtime 一销毁就死（W1b 实测）。
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

    /// 让夹具保证干净：清审批单 → 断父指针 → 删任务 → 建用户。
    async fn reset(me: Uuid) {
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password) \
             VALUES (?::uuid, 't13_flow_user', 't13-not-a-real-hash') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(&me)],
        )
        .await
        .expect("夹具用户写入失败");
        db().execute_with_params(
            "DELETE FROM biz_flow_instance WHERE initiator_id = ?::uuid",
            vec![Value::from(&me)],
        )
        .await
        .expect("夹具审批单清理失败");
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

    /// 写一行夹具任务（`task_no` 全表唯一 ⇒ 每个夹具自己的号）。
    async fn insert_task(
        id: Uuid,
        tag: &str,
        parent: Option<Uuid>,
        status: &str,
        progress: i16,
        me: Uuid,
    ) {
        let affected = db()
            .execute_with_params(
                "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, parent_id, \
                                       status, priority, progress, need_review) \
                 VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, ?::uuid, ?, 'P2', ?, true)",
                vec![
                    Value::from(&id),
                    Value::Text(format!("T13-{tag}-{}", &id.simple().to_string()[..8])),
                    Value::Text(format!("T13 审批级联 {tag}")),
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

    /// 插入一张**已落终态**的 `TASK_ACCEPT` 审批单，并回写到任务的 `approval_instance_id`。
    ///
    /// 口径与生产完全一致：`biz_type = TASK_ACCEPT`、`biz_id = task.id`、
    /// `biz_no = task.task_no`；`status` 非 `PENDING` 时 `ended_at` 必须非空
    /// （`ck_biz_flow_instance_end`）。
    async fn insert_settled_instance(
        task_id: Uuid,
        task_no: &str,
        status: &str,
        comment: Option<&str>,
        me: Uuid,
    ) -> Uuid {
        let def = model::default_def_of_biz_type(TASK_ACCEPT_BIZ_TYPE)
            .await
            .expect("查流程定义失败")
            .expect("测试库必须有 PUBLISHED 的 TASK_ACCEPT 流程（V0127 种子）");
        let version_id = def
            .current_version_id
            .expect("TASK_ACCEPT 流程必须有当前版本");
        let version = model::find_version(version_id)
            .await
            .expect("查版本失败")
            .expect("发布版本行必须存在");
        let id = Uuid::now_v7();
        let instance_no = format!("T13FLW{}", &id.simple().to_string()[..12]);
        db().execute_with_params(
            "INSERT INTO biz_flow_instance (id, instance_no, def_id, version_id, version_no, \
                 flow_code, flow_name, biz_type, biz_id, biz_no, title, status, initiator_id, \
                 form_data, result_comment, ended_at) \
             VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?, ?, ?, ?, ?::uuid, ?, ?, ?, ?::uuid, \
                     '{}'::jsonb, ?, now())",
            vec![
                Value::from(&id),
                Value::Text(instance_no),
                Value::from(&def.id),
                Value::from(&version_id),
                Value::I32(version.version_no),
                Value::Text(def.flow_code.clone()),
                Value::Text(def.flow_name.clone()),
                Value::Text(TASK_ACCEPT_BIZ_TYPE.to_string()),
                Value::from(&task_id),
                Value::Text(task_no.to_string()),
                Value::Text(format!("{task_no} T13 夹具审批单")),
                Value::Text(status.to_string()),
                Value::from(&me),
                Value::Text(comment.unwrap_or_default().to_string()),
            ],
        )
        .await
        .expect("夹具审批单写入失败");
        db().execute_with_params(
            "UPDATE biz_task SET approval_instance_id = ?::uuid WHERE id = ?::uuid",
            vec![Value::from(&id), Value::from(&task_id)],
        )
        .await
        .expect("回写 approval_instance_id 失败");
        id
    }

    /// 给夹具实例补一个节点行。
    ///
    /// `biz_flow_task.instance_node_id` 是 **NOT NULL + 外键**：留痕要挂在节点上。
    /// 真实实例在引擎走图时必然有节点行（`engine` 到达即建行），夹具按同一形态补一行。
    async fn insert_node(instance_id: Uuid) -> Uuid {
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_flow_instance_node \
                 (id, instance_id, node_key, node_name, node_type, round, status, activated_at, ended_at) \
             VALUES (?::uuid, ?::uuid, 'approve', '审批', 'APPROVAL', 1, 'APPROVED', now(), now())",
            vec![Value::from(&id), Value::from(&instance_id)],
        )
        .await
        .expect("夹具节点写入失败");
        id
    }

    /// 读库里的状态（断言看的是**落库值**，不是内存返回值）。
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

    /// 读库里的进度。
    async fn progress_of(id: Uuid) -> i16 {
        let rows = db()
            .find_with_params(
                "SELECT progress FROM biz_task WHERE id = ?::uuid AND deleted_at IS NULL",
                vec![Value::from(&id)],
            )
            .await
            .expect("查进度失败");
        rows.first()
            .expect("任务应存在")
            .get_typed("progress")
            .expect("取 progress")
    }

    /// 夹具 id（固定 UUID，便于重复跑时互相对照）。
    const PARENT: &str = "7a8a0000-0000-4000-8000-0000000002a1";
    const CHILD1: &str = "7a8a0000-0000-4000-8000-0000000002b1";
    const CHILD2: &str = "7a8a0000-0000-4000-8000-0000000002b2";
    const GRAND: &str = "7a8a0000-0000-4000-8000-0000000002c0";
    const MIDDLE: &str = "7a8a0000-0000-4000-8000-0000000002c1";
    const LEAF: &str = "7a8a0000-0000-4000-8000-0000000002c2";

    // ---------- ① 审批通过后，父任务从 DOING → REVIEW ----------

    #[tokio::test]
    async fn approve_cascades_parent_doing_to_review() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        // 父 DOING + 两个子：一个待验收（等审批）、一个已完成
        insert_task(uuid(PARENT), "P1-父", None, "DOING", 0, me).await;
        insert_task(
            uuid(CHILD1),
            "P1-子1",
            Some(uuid(PARENT)),
            "REVIEW",
            100,
            me,
        )
        .await;
        insert_task(uuid(CHILD2), "P1-子2", Some(uuid(PARENT)), "DONE", 100, me).await;
        let instance =
            insert_settled_instance(uuid(CHILD1), "T13-P1-子1", "APPROVED", None, me).await;

        let advanced = on_instance_settled(Some(instance), "APPROVED").await;
        assert_eq!(advanced, Some(uuid(CHILD1)), "审批通过必须推进那个子任务");

        assert_eq!(
            status_of(uuid(CHILD1)).await,
            TaskStatus::Done,
            "子任务应已完成"
        );
        assert_eq!(
            status_of(uuid(PARENT)).await,
            TaskStatus::Review,
            "① 子任务全部终态 → 父任务必须由 DOING 级联到 REVIEW（本卡的核心修复点）"
        );
    }

    // ---------- ② 审批通过后，父任务进度重算正确 ----------

    #[tokio::test]
    async fn approve_recalculates_parent_progress() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        insert_task(uuid(PARENT), "P2-父", None, "DOING", 0, me).await;
        insert_task(
            uuid(CHILD1),
            "P2-子1",
            Some(uuid(PARENT)),
            "REVIEW",
            100,
            me,
        )
        .await;
        insert_task(uuid(CHILD2), "P2-子2", Some(uuid(PARENT)), "DONE", 100, me).await;
        assert_eq!(
            progress_of(uuid(PARENT)).await,
            0,
            "夹具前置：父任务进度为 0"
        );
        let instance =
            insert_settled_instance(uuid(CHILD1), "T13-P2-子1", "APPROVED", None, me).await;

        on_instance_settled(Some(instance), "APPROVED").await;

        assert_eq!(
            progress_of(uuid(CHILD1)).await,
            100,
            "验收通过的进度副作用是 Force(100)（借的是表项，不改公式）"
        );
        assert_eq!(
            progress_of(uuid(PARENT)).await,
            100,
            "② 两个子任务都 100 → 父任务进度必须重算成 100"
        );
    }

    // ---------- ③ 审批驳回后，父任务不跟着退 ----------

    #[tokio::test]
    async fn reject_does_not_roll_parent_back() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        // 父 DOING + 子1 待验收 + 子2 还在跑（所以父任务不该被推到 REVIEW）
        insert_task(uuid(PARENT), "P3-父", None, "DOING", 40, me).await;
        insert_task(
            uuid(CHILD1),
            "P3-子1",
            Some(uuid(PARENT)),
            "REVIEW",
            100,
            me,
        )
        .await;
        insert_task(uuid(CHILD2), "P3-子2", Some(uuid(PARENT)), "DOING", 20, me).await;
        let instance = insert_settled_instance(
            uuid(CHILD1),
            "T13-P3-子1",
            "REJECTED",
            Some("T13 单测：验收不通过，请返工"),
            me,
        )
        .await;

        let advanced = on_instance_settled(Some(instance), "REJECTED").await;
        assert_eq!(advanced, Some(uuid(CHILD1)), "审批驳回必须推进那个子任务");

        assert_eq!(
            status_of(uuid(CHILD1)).await,
            TaskStatus::Doing,
            "子任务应被打回 DOING"
        );
        assert_eq!(
            status_of(uuid(PARENT)).await,
            TaskStatus::Doing,
            "③ 驳回是子任务自己的事：父任务不该跟着退（也不该被推到 REVIEW，因为还有子任务在跑）"
        );
        assert_ne!(
            progress_of(uuid(PARENT)).await,
            100,
            "③ 驳回后父任务进度不该是 100（返工的进度要降回来）"
        );
    }

    // ---------- ④ 三层：孙任务审批通过 → 父、祖父都级联 ----------

    #[tokio::test]
    async fn three_levels_cascade_up_to_grandparent() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        // 祖父 DOING → 父 DOING → 孙 REVIEW
        insert_task(uuid(GRAND), "P4-祖父", None, "DOING", 0, me).await;
        insert_task(uuid(MIDDLE), "P4-父", Some(uuid(GRAND)), "DOING", 0, me).await;
        insert_task(uuid(LEAF), "P4-孙", Some(uuid(MIDDLE)), "REVIEW", 100, me).await;

        // 第一跳：孙任务审批通过 → 父任务（它的唯一子任务到终态）→ REVIEW
        let leaf_instance =
            insert_settled_instance(uuid(LEAF), "T13-P4-孙", "APPROVED", None, me).await;
        on_instance_settled(Some(leaf_instance), "APPROVED").await;
        assert_eq!(status_of(uuid(LEAF)).await, TaskStatus::Done);
        assert_eq!(
            status_of(uuid(MIDDLE)).await,
            TaskStatus::Review,
            "④ 第一跳：父任务（中间层）被级联到 REVIEW"
        );
        assert_eq!(
            progress_of(uuid(GRAND)).await,
            100,
            "④ 第一跳就已经爬到祖父：祖父的进度被重算成 100（状态仍是 DOING，因为父任务还没结束）"
        );

        // 第二跳：中间层也审批通过 → 祖父（它的唯一子任务到终态）→ REVIEW
        let middle_instance =
            insert_settled_instance(uuid(MIDDLE), "T13-P4-父", "APPROVED", None, me).await;
        on_instance_settled(Some(middle_instance), "APPROVED").await;
        assert_eq!(status_of(uuid(MIDDLE)).await, TaskStatus::Done);
        assert_eq!(
            status_of(uuid(GRAND)).await,
            TaskStatus::Review,
            "④ 第二跳：祖父被级联到 REVIEW —— 三层链路每一层都走的是审批回调这条主路径"
        );
    }

    // ---------- ⑤ 级联失败不阻塞审批（审批结论仍然落库） ----------

    #[tokio::test]
    async fn cascade_failure_does_not_block_approval() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        insert_task(uuid(PARENT), "P5-父", None, "DOING", 0, me).await;
        insert_task(
            uuid(CHILD1),
            "P5-子1",
            Some(uuid(PARENT)),
            "REVIEW",
            100,
            me,
        )
        .await;
        insert_task(uuid(CHILD2), "P5-子2", Some(uuid(PARENT)), "DONE", 100, me).await;
        let instance =
            insert_settled_instance(uuid(CHILD1), "T13-P5-子1", "APPROVED", None, me).await;

        // 注入一次级联失败（`cfg(test)` 专用开关，生产二进制里没有这段代码）
        CASCADE_FAIL_INJECTED.store(true, std::sync::atomic::Ordering::SeqCst);
        let advanced = on_instance_settled(Some(instance), "APPROVED").await;
        CASCADE_FAIL_INJECTED.store(false, std::sync::atomic::Ordering::SeqCst);

        assert_eq!(
            advanced,
            Some(uuid(CHILD1)),
            "⑤ 级联失败不能让审批回调失败（settle 已经把任务推进了）"
        );
        assert_eq!(
            status_of(uuid(CHILD1)).await,
            TaskStatus::Done,
            "⑤ 红线：审批结论已落库 → 任务必须还是 DONE"
        );
        assert_eq!(
            status_of(uuid(PARENT)).await,
            TaskStatus::Doing,
            "⑤ 级联这笔事务回滚了 → 父任务保持原状（这正是巡检接口要兜的形态）"
        );
        // 巡检能看见这条不一致（证明「级联失败 → 巡检兜底」这条链闭合）
        let gaps = task_model::scan_parent_status_gaps(db(), 500)
            .await
            .expect("巡检查询失败");
        assert!(
            gaps.iter().any(|gap| gap.task_id == uuid(PARENT)),
            "⑤ 级联失败后的不一致必须能被巡检扫出来"
        );
    }

    // ---------- ⑥ C1：审批通过前校验目标单据状态（t_1e174431） ----------

    /// 读库里那张实例的**状态**（红线：跳过落状态不许改审批实例）。
    async fn instance_status_of_id(instance_id: Uuid) -> String {
        let rows = db()
            .find_with_params(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查实例失败");
        rows.first()
            .expect("实例应存在")
            .get_typed("status")
            .expect("取实例 status")
    }

    /// 读实例上的留痕行（`SKIPPED` + `is_auto` + comment 前缀）。
    async fn skip_traces_of(instance_id: Uuid) -> Vec<(String, bool, String)> {
        let rows = db()
            .find_with_params(
                "SELECT t.status, t.is_auto, coalesce(t.comment,'') AS comment FROM biz_flow_task t \
                  WHERE t.instance_id = ?::uuid ORDER BY t.created_at",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查留痕失败");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<String>("status").expect("取 status"),
                    row.get_typed::<bool>("is_auto").expect("取 is_auto"),
                    row.get_typed::<String>("comment").expect("取 comment"),
                )
            })
            .collect()
    }

    /// 卡面头号形态：**任务已取消 + 审批通过 → 任务保持 CANCELLED**，且过程留痕、实例不动。
    #[tokio::test]
    async fn cancelled_task_keeps_status_on_approval() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        insert_task(uuid(CHILD1), "C1-已取消", None, "CANCELLED", 0, me).await;
        let instance =
            insert_settled_instance(uuid(CHILD1), "T13-C1-取消", "APPROVED", None, me).await;
        insert_node(instance).await;

        let advanced = on_instance_settled(Some(instance), "APPROVED").await;

        assert_eq!(advanced, None, "⑥ 已取消的任务不该被回调「推进」");
        assert_eq!(
            status_of(uuid(CHILD1)).await,
            TaskStatus::Cancelled,
            "⑥ 卡面硬要求：任务 CANCELLED + 审批 APPROVED → 任务保持 CANCELLED，不许改回 DONE"
        );
        assert_eq!(
            progress_of(uuid(CHILD1)).await,
            0,
            "⑥ 进度也不许被改写（审批通过 = 进度 100 的副作用同样不许落）"
        );
        assert_eq!(
            instance_status_of_id(instance).await,
            "APPROVED",
            "⑥ 红线：审批实例本身仍按自己的结论结束（GUE 走完就是 APPROVED）"
        );

        // 留痕：一条 SKIPPED + is_auto + comment 写明「目标已不可改，跳过落状态」
        let traces = skip_traces_of(instance).await;
        assert_eq!(
            traces.len(),
            1,
            "⑥ 跳过落状态必须留一条痕，实际 {traces:#?}"
        );
        let (status, is_auto, comment) = &traces[0];
        assert_eq!(status, "SKIPPED", "⑥ 留痕行必须是 SKIPPED（不占待办槽位）");
        assert!(is_auto, "⑥ 留痕必须标 is_auto（系统代写，不是人审的）");
        assert!(
            comment.starts_with(settle_guard::SKIP_TRACE_PREFIX),
            "⑥ 留痕正文口径统一：{comment}"
        );
        for needle in ["T13-C1-取消", "CANCELLED", "DONE", "T13FLW"] {
            assert!(comment.contains(needle), "⑥ 留痕必须含 {needle}：{comment}");
        }

        // 幂等：再回调一次不重复刷痕
        on_instance_settled(Some(instance), "APPROVED").await;
        assert_eq!(
            skip_traces_of(instance).await.len(),
            1,
            "⑥ 重复回调不许重复留痕"
        );
    }

    /// 不误伤：任务在等验收（REVIEW）+ 审批通过 → 正常变 DONE。
    #[tokio::test]
    async fn review_task_still_completes_on_approval() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        insert_task(uuid(CHILD1), "C1-待验收", None, "REVIEW", 80, me).await;
        let instance =
            insert_settled_instance(uuid(CHILD1), "T13-C1-正常", "APPROVED", None, me).await;
        insert_node(instance).await;

        let advanced = on_instance_settled(Some(instance), "APPROVED").await;

        assert_eq!(
            advanced,
            Some(uuid(CHILD1)),
            "⑥ 正常路径（REVIEW + APPROVED）不许被前置校验误伤"
        );
        assert_eq!(
            status_of(uuid(CHILD1)).await,
            TaskStatus::Done,
            "⑥ 正常路径：任务 REVIEW + 审批通过 → DONE"
        );
        assert_eq!(progress_of(uuid(CHILD1)).await, 100, "⑥ 进度随状态落 100");
        assert!(
            skip_traces_of(instance).await.is_empty(),
            "⑥ 正常落状态不留「跳过」痕迹"
        );
    }

    /// 跳级不落：任务还在 DOING（没走提交验收）+ 审批通过 → **不落 DONE**，只留痕。
    ///
    /// 口径与 `task::state::callback_transition` 一致（`FlowCallback` 只开 `REVIEW → DONE|DOING`）：
    /// 放开 `from` 就等于让一条审批通过把任意任务跳级推到 DONE。
    #[tokio::test]
    async fn doing_task_is_not_pushed_to_done_by_approval() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let me = uuid(U_ID);
        reset(me).await;
        insert_task(uuid(CHILD1), "C1-进行中", None, "DOING", 40, me).await;
        let instance =
            insert_settled_instance(uuid(CHILD1), "T13-C1-跳级", "APPROVED", None, me).await;
        insert_node(instance).await;

        assert_eq!(
            on_instance_settled(Some(instance), "APPROVED").await,
            None,
            "⑥ 不在 REVIEW 的任务不该被审批结论推进"
        );
        assert_eq!(
            status_of(uuid(CHILD1)).await,
            TaskStatus::Doing,
            "⑥ 任务 DOING + 审批通过 → 保持 DOING（审批回调不许跳级）"
        );
        assert_eq!(
            skip_traces_of(instance).await.len(),
            1,
            "⑥ 跳级被拦同样要留痕（否则又是一次静默跳过）"
        );
    }

    // ---------- 纯逻辑：settle_target 的映射没被本卡改动 ----------

    #[test]
    fn settle_target_still_only_maps_final_verdicts() {
        assert_eq!(settle_target("APPROVED"), Some(TaskStatus::Done));
        assert_eq!(settle_target("REJECTED"), Some(TaskStatus::Doing));
        assert_eq!(settle_target("PENDING"), None);
        assert_eq!(settle_target("WITHDRAWN"), None);
        assert_eq!(settle_target("CANCELLED"), None);
    }
}

// ==================== P-FLOW-1B：映射表（纯逻辑单测） ====================
//
// 设计 §3.4 的映射表有 7 行 + 一条兜底。这里逐行钉死 —— 表错了，整条回调链就错了，
// 而它**完全是纯函数**（`node_action_of`），没有理由不在单测里全量覆盖。
//
// `FlowTaskOutcome` 的 4 个分支在 `tests/pflow1b.rs` 里用真库各跑一遍（`NotFlowTask` /
// `Advanced` / `Ignored` / `Conflict`）；这里只钉住枚举本身不会退化（4 个变体齐全）。
#[cfg(test)]
mod pflow1b_mapping_tests {
    use super::*;
    use crate::business::flow::node_config::{OnCancel, OnComplete, OnReject, TaskNodeConfig};

    /// 造一份只改「三个策略」的节点配置（其余走默认）。
    fn cfg(on_complete: OnComplete, on_reject: OnReject, on_cancel: OnCancel) -> TaskNodeConfig {
        TaskNodeConfig {
            title_template: "t".to_string(),
            on_complete,
            on_reject,
            on_cancel,
            ..TaskNodeConfig::default()
        }
    }

    /// 表格第 1 行：`DOING→DONE` + `on_complete != APPROVE` → 节点通过。
    #[test]
    fn doing_to_done_direct_approves_node() {
        for on_complete in [OnComplete::Direct, OnComplete::Verify] {
            let cfg = cfg(on_complete, OnReject::Stay, OnCancel::Terminate);
            assert_eq!(
                node_action_of(TaskStatus::Doing, TaskStatus::Done, &cfg),
                NodeAction::Approve,
                "{on_complete:?} 走完就是通过"
            );
        }
    }

    /// 表格第 2 行：`DOING→DONE` + `on_complete = APPROVE` → 发起审批（节点仍 ACTIVE）。
    #[test]
    fn doing_to_done_approve_starts_approval() {
        let cfg = cfg(OnComplete::Approve, OnReject::Stay, OnCancel::Terminate);
        assert_eq!(
            node_action_of(TaskStatus::Doing, TaskStatus::Done, &cfg),
            NodeAction::StartApproval
        );
    }

    /// 表格第 3 行：`REVIEW→DONE`（验收通过）→ 节点通过。
    ///
    /// 即使配置是 `APPROVE`（R18 本该拒绝的组合）也只走「通过」，**不再发起第二张审批单**
    /// —— 旧快照 / 绕过校验的数据不能变成「点两次」。
    #[test]
    fn review_to_done_approves_node_without_second_approval() {
        for on_complete in [OnComplete::Direct, OnComplete::Verify, OnComplete::Approve] {
            let cfg = cfg(on_complete, OnReject::Stay, OnCancel::Terminate);
            assert_eq!(
                node_action_of(TaskStatus::Review, TaskStatus::Done, &cfg),
                NodeAction::Approve,
                "{on_complete:?} 在验收通过时都只推节点，不再发起审批"
            );
        }
    }

    /// 表格第 4 行：`DOING→REVIEW` → **不动**（节点保持 ACTIVE 等验收）。
    #[test]
    fn doing_to_review_keeps_node_active() {
        let cfg = cfg(OnComplete::Verify, OnReject::Stay, OnCancel::Terminate);
        assert_eq!(
            node_action_of(TaskStatus::Doing, TaskStatus::Review, &cfg),
            NodeAction::Nothing
        );
    }

    /// 表格第 5 行 · `on_reject=STAY`（默认策略之一）：打回不动流程。
    #[test]
    fn reject_stay_keeps_node_active() {
        let cfg = cfg(OnComplete::Direct, OnReject::Stay, OnCancel::Terminate);
        assert_eq!(
            node_action_of(TaskStatus::Review, TaskStatus::Doing, &cfg),
            NodeAction::Nothing
        );
    }

    /// 表格第 5 行 · `on_reject=BACK_TO_PREV`：退回上一节点。
    #[test]
    fn reject_back_to_prev_returns_flow() {
        let cfg = cfg(
            OnComplete::Direct,
            OnReject::BackToPrev,
            OnCancel::Terminate,
        );
        assert_eq!(
            node_action_of(TaskStatus::Review, TaskStatus::Doing, &cfg),
            NodeAction::ReturnToPrev
        );
    }

    /// 表格第 5 行 · `on_reject=TERMINATE`：实例 `REJECTED`。
    #[test]
    fn reject_terminate_ends_instance() {
        let cfg = cfg(OnComplete::Direct, OnReject::Terminate, OnCancel::Terminate);
        assert_eq!(
            node_action_of(TaskStatus::Review, TaskStatus::Doing, &cfg),
            NodeAction::Terminate("REJECTED")
        );
    }

    /// 表格第 6 行：`DONE→DOING`（重开）→ **不动**（任务重开不改流程）。
    #[test]
    fn reopen_done_to_doing_keeps_flow() {
        for on_reject in [OnReject::Stay, OnReject::BackToPrev, OnReject::Terminate] {
            let cfg = cfg(OnComplete::Direct, on_reject, OnCancel::Terminate);
            assert_eq!(
                node_action_of(TaskStatus::Done, TaskStatus::Doing, &cfg),
                NodeAction::Nothing,
                "重开任务不该退回流程（节点已 APPROVED 是既成事实）"
            );
        }
    }

    /// 表格第 7 行 · `on_cancel=TERMINATE`（默认）：实例 `CANCELLED`。
    #[test]
    fn cancel_terminate_ends_instance() {
        let cfg = cfg(OnComplete::Direct, OnReject::Stay, OnCancel::Terminate);
        for from in [TaskStatus::Todo, TaskStatus::Doing, TaskStatus::Review] {
            assert_eq!(
                node_action_of(from, TaskStatus::Cancelled, &cfg),
                NodeAction::Terminate("CANCELLED"),
                "{from:?} → CANCELLED 都要走 on_cancel"
            );
        }
    }

    /// 表格第 7 行 · `on_cancel=STAY`：不动（任务取消了但流程继续）。
    #[test]
    fn cancel_stay_keeps_flow() {
        let cfg = cfg(OnComplete::Direct, OnReject::Stay, OnCancel::Stay);
        for from in [TaskStatus::Todo, TaskStatus::Doing, TaskStatus::Review] {
            assert_eq!(
                node_action_of(from, TaskStatus::Cancelled, &cfg),
                NodeAction::Nothing
            );
        }
    }

    /// 表格第 7 行 · `on_cancel=BACK_TO_PREV`：退回上一节点。
    #[test]
    fn cancel_back_to_prev_returns_flow() {
        let cfg = cfg(OnComplete::Direct, OnReject::Stay, OnCancel::BackToPrev);
        assert_eq!(
            node_action_of(TaskStatus::Doing, TaskStatus::Cancelled, &cfg),
            NodeAction::ReturnToPrev
        );
    }

    /// 兜底行：`TODO→DOING`（开始干活）不推进流程。
    #[test]
    fn todo_to_doing_does_not_touch_flow() {
        let cfg = cfg(OnComplete::Direct, OnReject::Stay, OnCancel::Terminate);
        assert_eq!(
            node_action_of(TaskStatus::Todo, TaskStatus::Doing, &cfg),
            NodeAction::Nothing,
            "「开始」只是任务侧的事，流程完全不知道（设计 §4.5 Q2 的 ASCII 图）"
        );
    }

    /// 兜底行：`CANCELLED→TODO`（复活）不推进流程。
    #[test]
    fn revive_cancelled_to_todo_does_not_touch_flow() {
        let cfg = cfg(OnComplete::Direct, OnReject::Stay, OnCancel::Terminate);
        assert_eq!(
            node_action_of(TaskStatus::Cancelled, TaskStatus::Todo, &cfg),
            NodeAction::Nothing
        );
    }

    /// 全组合扫一遍：只有映射表点名的 6 类 `(from,to)` 会产生动作，其余一律 `Nothing`。
    ///
    /// 这条是「映射表没有被顺手扩大」的护栏 —— 将来有人想给 `TODO→DOING` 加个副作用，
    /// 必须先改这张表并说明理由。
    #[test]
    fn only_tabulated_transitions_act_on_flow() {
        use TaskStatus::{Cancelled, Doing, Done, Review, Todo};
        let all = [Todo, Doing, Review, Done, Cancelled];
        let cfg = cfg(
            OnComplete::Approve,
            OnReject::Terminate,
            OnCancel::Terminate,
        );
        let mut acting = Vec::new();
        for from in all {
            for to in all {
                if node_action_of(from, to, &cfg) != NodeAction::Nothing {
                    acting.push((from.code(), to.code()));
                }
            }
        }
        acting.sort_unstable();
        assert_eq!(
            acting,
            vec![
                ("DOING", "CANCELLED"),
                ("DOING", "DONE"),
                ("REVIEW", "CANCELLED"),
                ("REVIEW", "DOING"),
                ("REVIEW", "DONE"),
                ("TODO", "CANCELLED"),
            ],
            "映射表只该有这 6 类（`on_complete=APPROVE` + `on_reject=TERMINATE` + `on_cancel=TERMINATE` 下）"
        );
    }

    /// `FlowTaskOutcome` 的 4 个变体齐全，且 `Conflict` 原样携带 `ApiError`。
    #[test]
    fn flow_task_outcome_has_four_variants() {
        let not_flow = FlowTaskOutcome::NotFlowTask;
        let advanced = FlowTaskOutcome::Advanced {
            next_nodes: vec!["n_end".to_string()],
        };
        let ignored = FlowTaskOutcome::Ignored {
            instance_status: "CANCELLED".to_string(),
        };
        let conflict = FlowTaskOutcome::Conflict(ApiError::new(
            409,
            crate::shared::error_code::FLOW_TASK_STATE_CONFLICT,
            "流程已结束",
        ));
        assert!(matches!(not_flow, FlowTaskOutcome::NotFlowTask));
        assert!(matches!(
            advanced,
            FlowTaskOutcome::Advanced { ref next_nodes } if next_nodes == &["n_end".to_string()]
        ));
        assert!(matches!(
            ignored,
            FlowTaskOutcome::Ignored { ref instance_status } if instance_status == "CANCELLED"
        ));
        match conflict {
            FlowTaskOutcome::Conflict(error) => {
                assert_eq!(error.status, 409);
                assert_eq!(
                    error.code,
                    crate::shared::error_code::FLOW_TASK_STATE_CONFLICT
                );
            }
            other => panic!("必须是 Conflict：{other:?}"),
        }
    }
}

// ==================== P11：任务取消 → 审批实例与待办一起撤（单测） ====================
//
// 复现的缺陷（老板 2026-10-31 报 TSK-202610-000149）：
//   任务被推到 `CANCELLED` 时，`change_status` 只级联取消了**子孙任务**，
//   没有撤它的 `biz_flow_instance` 与 `biz_flow_task`，于是审批人的「待我审批」里
//   挂着一条点不动的孤儿待办（真库实测 8 条）。
//
// 这里的用例**按 handler 的真实编排**走：一个事务里
//   ① `apply_status(CANCELLED)` → ② `cascade_cancel_descendants` → ③ 逐个 `cancel_on_task_cancelled`
// 所以①③之间任何一步失败都必须整笔回滚（用例 ⑧ 用 `#[cfg(test)]` 注入证明）。
//
// 跑法（需要一个**已迁移到最新**的隔离库；别指 prod）：
//   su - postgres -c 'createdb -O cloudpivot cloudpivot_p11'
//   cd backend && TASK_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p11' \
//     cargo test --lib p11_cancel_flow_tests -j 1
//
// 缺 `TASK_DB_URL` 时每条用例打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍全绿）。
#[cfg(test)]
mod p11_cancel_flow_tests {
    use super::*;
    use crate::business::flow::model::CancelledOfBiz;
    use crate::business::task::model as task_model;
    use crate::business::task::state::TaskStatus;

    /// 夹具操作人 / 审批人（`biz_task.owner_id` / `biz_flow_task.assignee_id` 都是硬 FK）。
    const U_ID: &str = "7b110000-0000-4000-8000-0000000000c1";
    /// 父 / 子 / 对照组 / 没有任何审批单的任务。
    const TASK_PARENT: &str = "7b110000-0000-4000-8000-0000000001a1";
    const TASK_CHILD: &str = "7b110000-0000-4000-8000-0000000001a2";
    const TASK_OTHER: &str = "7b110000-0000-4000-8000-0000000001a3";
    const TASK_PLAIN: &str = "7b110000-0000-4000-8000-0000000001a4";
    const NO_PARENT: &str = "P11-PARENT";
    const NO_CHILD: &str = "P11-CHILD";
    const NO_OTHER: &str = "P11-OTHER";
    const NO_PLAIN: &str = "P11-PLAIN";
    /// 流程定义 / 版本（实例的 `def_id` / `version_id` 都是 `ON DELETE RESTRICT`，必须先造）。
    const DEF_ID: &str = "7b110000-0000-4000-8000-000000000d01";
    const VER_ID: &str = "7b110000-0000-4000-8000-000000000e01";
    /// 实例：父任务的在跑单 + 父任务上**已通过**的历史单 + 子任务的在跑单 + 对照组在跑单。
    const INST_PARENT: &str = "7b110000-0000-4000-8000-000000000f01";
    const INST_HIST: &str = "7b110000-0000-4000-8000-000000000f02";
    const INST_CHILD: &str = "7b110000-0000-4000-8000-000000000f03";
    const INST_OTHER: &str = "7b110000-0000-4000-8000-000000000f04";
    /// 实例节点（`biz_flow_task.instance_node_id` 是硬 FK）。
    const NODE_PARENT: &str = "7b110000-0000-4000-8000-000000001001";
    const NODE_HIST: &str = "7b110000-0000-4000-8000-000000001002";
    const NODE_CHILD: &str = "7b110000-0000-4000-8000-000000001003";
    const NODE_OTHER: &str = "7b110000-0000-4000-8000-000000001004";
    /// 审批待办。
    const FT_PARENT: &str = "7b110000-0000-4000-8000-000000001101";
    const FT_HIST: &str = "7b110000-0000-4000-8000-000000001102";
    const FT_CHILD: &str = "7b110000-0000-4000-8000-000000001103";
    const FT_OTHER: &str = "7b110000-0000-4000-8000-000000001104";
    /// `APPROVED` 待办上原有的人工意见（断言「不被改」时逐字相等 —— 审计事实）。
    const APPROVED_COMMENT: &str = "人工通过，保留审计（P11）";

    /// 真库用例**串行**执行（共用一个库 + 固定 UUID 夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `TASK_DB_URL`，用例直接跳过。
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

    async fn exec(sql: &str, params: Vec<Value>, what: &str) {
        if let Err(error) = db().execute_with_params(sql, params).await {
            panic!("夹具{what}失败：{error}");
        }
    }

    async fn insert_task(id: Uuid, task_no: &str, parent: Option<Uuid>, me: Uuid) {
        let parent_sql = if parent.is_some() {
            "?::uuid"
        } else {
            "NULL::uuid"
        };
        let mut params = vec![
            Value::from(&id),
            Value::Text(task_no.to_string()),
            Value::Text(format!("P11 {task_no}")),
            Value::from(&me),
            Value::from(&me),
        ];
        if let Some(parent) = parent.as_ref() {
            params.push(Value::from(parent));
        }
        exec(
            &format!(
                "INSERT INTO biz_task (id, task_no, title, owner_id, creator_id, \
                                      status, priority, progress, need_review, parent_id) \
                 VALUES (?::uuid, ?, ?, ?::uuid, ?::uuid, 'REVIEW', 'P2', 0, true, {parent_sql})"
            ),
            params,
            "任务",
        )
        .await;
    }

    async fn insert_instance(
        id: Uuid,
        instance_no: &str,
        status: &str,
        biz_id: Uuid,
        biz_no: &str,
        me: Uuid,
    ) {
        // ck_biz_flow_instance_end：`(status='PENDING') = (ended_at IS NULL)`。
        let ended_at = if status == "PENDING" { "NULL" } else { "now()" };
        let result_comment = if status == "APPROVED" {
            format!("'{APPROVED_COMMENT}'")
        } else {
            "NULL".to_string()
        };
        exec(
            &format!(
                "INSERT INTO biz_flow_instance \
                     (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, \
                      biz_type, biz_id, biz_no, title, status, initiator_id, ended_at, result_comment) \
                 VALUES (?::uuid, ?, ?::uuid, ?::uuid, 1, 'p11_cancel_flow', 'P11 取消撤审批', \
                         'TASK_ACCEPT', ?::uuid, ?, ?::text, ?, ?::uuid, {ended_at}, {result_comment})"
            ),
            vec![
                Value::from(&id),
                Value::Text(instance_no.to_string()),
                Value::from(&uuid(DEF_ID)),
                Value::from(&uuid(VER_ID)),
                Value::from(&biz_id),
                Value::Text(biz_no.to_string()),
                Value::Text(format!("P11 {instance_no}")),
                Value::Text(status.to_string()),
                Value::from(&me),
            ],
            "实例",
        )
        .await;
    }

    async fn insert_node(id: Uuid, instance_id: Uuid, status: &str) {
        let ended_at = if status == "PENDING" || status == "ACTIVE" {
            "NULL"
        } else {
            "now()"
        };
        exec(
            &format!(
                "INSERT INTO biz_flow_instance_node \
                     (id, instance_id, node_key, node_name, node_type, status, ended_at) \
                 VALUES (?::uuid, ?::uuid, 'n_creator', '创建人验收', 'APPROVAL', ?, {ended_at})"
            ),
            vec![
                Value::from(&id),
                Value::from(&instance_id),
                Value::Text(status.to_string()),
            ],
            "节点",
        )
        .await;
    }

    async fn insert_flow_task(id: Uuid, instance_id: Uuid, node_id: Uuid, status: &str, me: Uuid) {
        // ck_biz_flow_task_done：`(status='PENDING') = (completed_at IS NULL)`。
        let completed_at = if status == "PENDING" { "NULL" } else { "now()" };
        let comment = if status == "PENDING" {
            "NULL".to_string()
        } else {
            format!("'{APPROVED_COMMENT}'")
        };
        exec(
            &format!(
                "INSERT INTO biz_flow_task \
                     (id, instance_id, instance_node_id, node_key, assignee_id, assignee_source, \
                      status, completed_at, comment) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, 'n_creator', ?::uuid, 'USER', ?, {completed_at}, {comment})"
            ),
            vec![
                Value::from(&id),
                Value::from(&instance_id),
                Value::from(&node_id),
                Value::from(&me),
                Value::Text(status.to_string()),
            ],
            "审批待办",
        )
        .await;
    }

    /// 清掉上一次的夹具（实例 → 版本 → 定义 → 任务；顺序受 FK 约束）。
    async fn cleanup(me: Uuid) {
        exec(
            "DELETE FROM biz_flow_instance WHERE def_id = ?::uuid",
            vec![Value::from(&uuid(DEF_ID))],
            "清理实例",
        )
        .await;
        exec(
            "UPDATE biz_flow_def SET current_version_id = NULL WHERE id = ?::uuid",
            vec![Value::from(&uuid(DEF_ID))],
            "清理定义（摘当前版本）",
        )
        .await;
        exec(
            "DELETE FROM biz_flow_def_version WHERE def_id = ?::uuid",
            vec![Value::from(&uuid(DEF_ID))],
            "清理版本",
        )
        .await;
        exec(
            "DELETE FROM biz_flow_def WHERE id = ?::uuid",
            vec![Value::from(&uuid(DEF_ID))],
            "清理定义",
        )
        .await;
        // 自引用 FK 不是 deferrable：先断父指针再删。
        exec(
            "UPDATE biz_task SET parent_id = NULL WHERE creator_id = ?::uuid",
            vec![Value::from(&me)],
            "清理任务（断父指针）",
        )
        .await;
        exec(
            "DELETE FROM biz_task WHERE creator_id = ?::uuid",
            vec![Value::from(&me)],
            "清理任务",
        )
        .await;
    }

    /// 拿锁 + 连库 + 重建夹具（幂等）：父任务（在跑 + 已通过两张单）、子任务、对照组、无单任务。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        let me = uuid(U_ID);
        exec(
            "INSERT INTO sys_user (id, username, password) \
             VALUES (?::uuid, 'p11_flow_user', 'p11-not-a-real-hash') ON CONFLICT DO NOTHING",
            vec![Value::from(&me)],
            "用户",
        )
        .await;
        cleanup(me).await;
        insert_task(uuid(TASK_PARENT), NO_PARENT, None, me).await;
        insert_task(uuid(TASK_CHILD), NO_CHILD, Some(uuid(TASK_PARENT)), me).await;
        insert_task(uuid(TASK_OTHER), NO_OTHER, None, me).await;
        insert_task(uuid(TASK_PLAIN), NO_PLAIN, None, me).await;
        exec(
            "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, status) \
             VALUES (?::uuid, 'p11_cancel_flow', 'P11 取消撤审批', 'TASK_ACCEPT', 'DRAFT')",
            vec![Value::from(&uuid(DEF_ID))],
            "流程定义",
        )
        .await;
        exec(
            "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status) \
             VALUES (?::uuid, ?::uuid, 1, 'DRAFT')",
            vec![Value::from(&uuid(VER_ID)), Value::from(&uuid(DEF_ID))],
            "流程版本",
        )
        .await;
        insert_instance(
            uuid(INST_PARENT),
            "P11-INST-P",
            "PENDING",
            uuid(TASK_PARENT),
            NO_PARENT,
            me,
        )
        .await;
        insert_instance(
            uuid(INST_HIST),
            "P11-INST-H",
            "APPROVED",
            uuid(TASK_PARENT),
            NO_PARENT,
            me,
        )
        .await;
        insert_instance(
            uuid(INST_CHILD),
            "P11-INST-C",
            "PENDING",
            uuid(TASK_CHILD),
            NO_CHILD,
            me,
        )
        .await;
        insert_instance(
            uuid(INST_OTHER),
            "P11-INST-O",
            "PENDING",
            uuid(TASK_OTHER),
            NO_OTHER,
            me,
        )
        .await;
        insert_node(uuid(NODE_PARENT), uuid(INST_PARENT), "ACTIVE").await;
        insert_node(uuid(NODE_HIST), uuid(INST_HIST), "APPROVED").await;
        insert_node(uuid(NODE_CHILD), uuid(INST_CHILD), "ACTIVE").await;
        insert_node(uuid(NODE_OTHER), uuid(INST_OTHER), "ACTIVE").await;
        insert_flow_task(
            uuid(FT_PARENT),
            uuid(INST_PARENT),
            uuid(NODE_PARENT),
            "PENDING",
            me,
        )
        .await;
        insert_flow_task(
            uuid(FT_HIST),
            uuid(INST_HIST),
            uuid(NODE_HIST),
            "APPROVED",
            me,
        )
        .await;
        insert_flow_task(
            uuid(FT_CHILD),
            uuid(INST_CHILD),
            uuid(NODE_CHILD),
            "PENDING",
            me,
        )
        .await;
        insert_flow_task(
            uuid(FT_OTHER),
            uuid(INST_OTHER),
            uuid(NODE_OTHER),
            "PENDING",
            me,
        )
        .await;
        Some(guard)
    }

    /// 与 `handler::change_status` 同款的取消编排（① 写状态 → ② 级联子孙 → ③ 逐个撤审批）。
    ///
    /// 返回 `(三表行数合计, 被级联取消的子孙)`；任一步 `?` 失败 = 调用方回滚整笔。
    async fn cancel_task_in_tx(
        tx: &Transaction,
        task_id: Uuid,
        task_no: &str,
        me: Uuid,
    ) -> DbResult<(CancelledOfBiz, Vec<task_model::CancelCascade>)> {
        let update = task_model::StatusUpdate {
            to: TaskStatus::Cancelled.code().to_string(),
            progress: None,
            done_at: None,
            reject_reason: None,
            cancel_reason: Some(task_model::CASCADE_CANCEL_REASON.to_string()),
        };
        let affected = task_model::apply_status(tx, task_id, &update, me).await?;
        assert_eq!(affected, 1, "夹具任务必须存在");
        let cancels = task_model::cascade_cancel_descendants(
            tx,
            task_id,
            me,
            task_model::CASCADE_CANCEL_REASON,
        )
        .await?;
        let mut out = cancel_on_task_cancelled(tx, task_id, task_no).await?;
        for cancel in &cancels {
            let sub = cancel_on_task_cancelled(tx, cancel.task_id, &cancel.task_no).await?;
            out.instances += sub.instances;
            out.tasks += sub.tasks;
            out.nodes += sub.nodes;
        }
        Ok((out, cancels))
    }

    /// 提交式封装（正常路径）。
    async fn cancel_task(
        task_id: Uuid,
        task_no: &str,
        me: Uuid,
    ) -> (CancelledOfBiz, Vec<task_model::CancelCascade>) {
        let tx = db().begin().await.expect("开事务失败");
        let out = cancel_task_in_tx(&tx, task_id, task_no, me)
            .await
            .expect("取消任务失败");
        tx.commit().await.expect("提交事务失败");
        out
    }

    async fn task_status(id: Uuid) -> String {
        let rows = db()
            .find_with_params(
                "SELECT status FROM biz_task WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("读任务失败");
        rows.first()
            .expect("任务不存在")
            .get_typed("status")
            .expect("status")
    }

    /// 实例三元组：状态 / `ended_at` 是否非空 / `result_comment`。
    async fn instance_row(id: Uuid) -> (String, bool, Option<String>) {
        let rows = db()
            .find_with_params(
                "SELECT status, (ended_at IS NOT NULL) AS ended, result_comment \
                   FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("读实例失败");
        let row = rows.first().expect("实例不存在");
        (
            row.get_typed("status").expect("status"),
            row.get_typed("ended").expect("ended"),
            row.get_typed("result_comment").expect("result_comment"),
        )
    }

    /// 待办三元组：状态 / `completed_at` 是否非空 / `comment`。
    async fn flow_task_row(id: Uuid) -> (String, bool, Option<String>) {
        let rows = db()
            .find_with_params(
                "SELECT status, (completed_at IS NOT NULL) AS done, comment \
                   FROM biz_flow_task WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("读待办失败");
        let row = rows.first().expect("待办不存在");
        (
            row.get_typed("status").expect("status"),
            row.get_typed("done").expect("done"),
            row.get_typed("comment").expect("comment"),
        )
    }

    /// 节点三元组：状态 / `ended_at` 是否非空。
    async fn node_row(id: Uuid) -> (String, bool) {
        let rows = db()
            .find_with_params(
                "SELECT status, (ended_at IS NOT NULL) AS ended \
                   FROM biz_flow_instance_node WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("读节点失败");
        let row = rows.first().expect("节点不存在");
        (
            row.get_typed("status").expect("status"),
            row.get_typed("ended").expect("ended"),
        )
    }

    /// ① 任务推 `CANCELLED` → 它的审批实例变 `CANCELLED`（`ended_at` 有值 + 文案是「已取消」）。
    #[tokio::test]
    async fn cancel_task_settles_its_instance() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        let (out, _) = cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        assert_eq!(
            out.instances, 2,
            "父任务 + 被级联取消的子任务各一条在跑实例"
        );
        let (status, ended, comment) = instance_row(uuid(INST_PARENT)).await;
        assert_eq!(status, "CANCELLED");
        assert!(
            ended,
            "实例结束必须同写 ended_at（ck_biz_flow_instance_end）"
        );
        assert_eq!(comment.as_deref(), Some(TASK_CANCEL_COMMENT));
    }

    /// ② 任务推 `CANCELLED` → 它的 `PENDING` 待办变 `CANCELLED`（`completed_at` 有值）。
    #[tokio::test]
    async fn cancel_task_settles_its_pending_todo() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        let (status, done, comment) = flow_task_row(uuid(FT_PARENT)).await;
        assert_eq!(status, "CANCELLED");
        assert!(
            done,
            "待办结束必须同写 completed_at（ck_biz_flow_task_done）"
        );
        assert_eq!(comment.as_deref(), Some(TASK_CANCEL_COMMENT));
    }

    /// ③ 任务推 `CANCELLED` → 实例节点变 `CANCELLED`（`ended_at` 有值）。
    #[tokio::test]
    async fn cancel_task_settles_its_node() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        let (out, _) = cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        assert_eq!(out.nodes, 2, "父 + 子两个在跑节点各被终止 1 条");
        let (status, ended) = node_row(uuid(NODE_PARENT)).await;
        assert_eq!(status, "CANCELLED");
        assert!(ended, "节点结束必须同写 ended_at");
    }

    /// ④ 父任务取消 → 级联取消的子任务，各自的审批也被撤。
    #[tokio::test]
    async fn parent_cancel_settles_children_approvals() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        let (out, cancels) = cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        assert_eq!(cancels.len(), 1, "应有 1 个子任务被级联取消");
        assert_eq!(cancels[0].task_id, uuid(TASK_CHILD));
        assert_eq!(
            cancels[0].task_no, NO_CHILD,
            "级联结果必须带子任务单号（撤审批要按 biz_no 精确匹配）"
        );
        assert_eq!(task_status(uuid(TASK_CHILD)).await, "CANCELLED");
        assert_eq!(out.instances, 2, "父 + 子各一条在跑实例");
        assert_eq!(out.tasks, 2, "父 + 子各一条 PENDING 待办");
        assert_eq!(instance_row(uuid(INST_CHILD)).await.0, "CANCELLED");
        let (child_todo, child_done, _) = flow_task_row(uuid(FT_CHILD)).await;
        assert_eq!(child_todo, "CANCELLED");
        assert!(child_done);
    }

    /// ⑤ 已经是 `CANCELLED` 的任务再取消一次 → 幂等（0 行改动、不报错）。
    #[tokio::test]
    async fn cancel_twice_is_idempotent() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        let (first, _) = cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        assert_eq!(first.instances, 2, "父 + 子各一条在跑实例");
        let (second, cancels) = cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        assert_eq!(cancels.len(), 0, "第二次没有可级联的子孙（都已 CANCELLED）");
        assert_eq!(second, CancelledOfBiz::default(), "第二次取消应 0 行改动");
        // 文案仍是第一次那条（没有被第二次覆盖）
        assert_eq!(
            flow_task_row(uuid(FT_PARENT)).await.2.as_deref(),
            Some(TASK_CANCEL_COMMENT)
        );
    }

    /// ⑥ 撤销只动 `PENDING`：`APPROVED` 的历史待办与实例**一个字都不改**（审计事实）。
    #[tokio::test]
    async fn approved_history_is_untouched() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        let (status, done, comment) = flow_task_row(uuid(FT_HIST)).await;
        assert_eq!(status, "APPROVED", "历史待办不许被改成 CANCELLED");
        assert!(done, "历史待办的 completed_at 不许被重写");
        assert_eq!(comment.as_deref(), Some(APPROVED_COMMENT));
        let (inst_status, _, inst_comment) = instance_row(uuid(INST_HIST)).await;
        assert_eq!(inst_status, "APPROVED");
        assert_eq!(inst_comment.as_deref(), Some(APPROVED_COMMENT));
    }

    /// ⑦ 精确匹配：别的单据的实例与待办不受影响（`biz_id` / `biz_no` 都指向别的任务）。
    #[tokio::test]
    async fn other_biz_instances_untouched() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        cancel_task(uuid(TASK_PARENT), NO_PARENT, me).await;
        assert_eq!(instance_row(uuid(INST_OTHER)).await.0, "PENDING");
        let (todo, done, comment) = flow_task_row(uuid(FT_OTHER)).await;
        assert_eq!(todo, "PENDING");
        assert!(!done);
        assert!(comment.is_none(), "别人的待办连 comment 都不该被碰");
        assert_eq!(node_row(uuid(NODE_OTHER)).await.0, "ACTIVE");
        assert_eq!(task_status(uuid(TASK_OTHER)).await, "REVIEW");
    }

    /// ⑧ 撤审批失败 → **任务状态也没改**（同一事务回滚）。
    #[tokio::test]
    async fn cancel_failure_rolls_back_task_status() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        let tx = db().begin().await.expect("开事务失败");
        // 让第 ③ 步（撤审批）失败：状态已经写进事务了，必须由回滚抹掉。
        CANCEL_INJECT_FAIL.store(true, std::sync::atomic::Ordering::SeqCst);
        let result = cancel_task_in_tx(&tx, uuid(TASK_PARENT), NO_PARENT, me).await;
        let error = result.expect_err("注入失败后应当 Err");
        assert!(
            matches!(&error, DbError::InvalidValue(msg) if msg == P11_FAIL_SENTINEL),
            "应当是注入的那条哨兵错误，实际：{error}"
        );
        tx.rollback().await.expect("回滚失败");
        assert_eq!(
            task_status(uuid(TASK_PARENT)).await,
            "REVIEW",
            "撤审批失败必须整笔回滚：任务不许停在 CANCELLED"
        );
        assert_eq!(instance_row(uuid(INST_PARENT)).await.0, "PENDING");
        assert_eq!(flow_task_row(uuid(FT_PARENT)).await.0, "PENDING");
        assert_eq!(
            task_status(uuid(TASK_CHILD)).await,
            "REVIEW",
            "级联取消的子孙也要跟着回滚"
        );
    }

    /// ⑨ 没有审批单的任务取消 → 三表 0 行，不报错（「没配流程」不是错误）。
    #[tokio::test]
    async fn cancel_without_instance_is_noop() {
        let Some(_guard) = setup().await else {
            return;
        };
        let me = uuid(U_ID);
        let (out, cancels) = cancel_task(uuid(TASK_PLAIN), NO_PLAIN, me).await;
        assert!(cancels.is_empty());
        assert_eq!(out, CancelledOfBiz::default(), "没有审批单 → 0 行改动");
        assert_eq!(task_status(uuid(TASK_PLAIN)).await, "CANCELLED");
    }

    /// ⑩ 文案口径：取消与删除是两种动作，`result_comment` 不能混（纯逻辑，不连库）。
    #[test]
    fn cancel_and_delete_comments_are_distinct() {
        assert_ne!(TASK_CANCEL_COMMENT, TASK_DELETE_CANCEL_COMMENT);
        assert!(
            TASK_CANCEL_COMMENT.contains("取消"),
            "取消留痕必须写明「任务已取消」"
        );
        assert!(
            TASK_DELETE_CANCEL_COMMENT.contains("删除"),
            "删除留痕必须写明「任务已删除」"
        );
    }
}

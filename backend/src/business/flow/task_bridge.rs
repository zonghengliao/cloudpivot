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

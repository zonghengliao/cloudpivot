//! 请假单的**审批回调桥**（`hr_leave` 流程，ARCH-9 §4.3 / §8.1 HR-27）。
//!
//! # 三个入口（与 `erp/expense_bridge.rs` 同款分工）
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_leave_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance, terminate_instance}`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`（写 `biz_status_log`）；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT`（打回，意见进状态日志，可改可重提） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 发起人 / 请假员工本人不能批这一票 → 403 + 65019 |
//! | [`cancel_approval_of`] | `hr::leave_handler::cancel_leave`（撤回 / 作废，**同一事务**） | 终止在跑的实例，避免待办挂在「已经回到草稿」的单上 |
//!
//! # 为什么不是「第三个 field：实例终止」而是 `cancel_approval_of`
//!
//! 卡面写的是「审批回调 → 状态 + 实例终止 + 自审批守卫」三件事。实例终止**不属于回调**：
//! 回调发生在审批事务提交之后，实例已经是终态了，没有可终止的东西。
//! 真正需要终止实例的是**单据侧主动改状态**的两条路（撤回 / 作废），所以这一件放在
//! [`cancel_approval_of`] 里，由 `leave_handler::cancel_leave` 在同一事务里调用
//! （与 `erp/expense_bridge::cancel_approval_of` 同款）。
//!
//! # 四条硬口径（ARCH-9 §4.3，逐条落点）
//!
//! 1. **回调在审批事务提交之后跑** → 业务侧失败**不许**回滚审批单：只记日志、返回 `None`，
//!    单据停在 `SUBMITTED` 等 HR 重试（本桥没有凭证，最坏后果是「审批过了、单据显示审批中」，
//!    不会产生错账；但仍是 ERROR 级日志 —— 员工请假没落库 = 考勤口径错）。
//! 2. **自审批必须拦**：[`guard_self_approval`] 两条判据 —— `instance.initiator_id`（谁提的）
//!    与请假单的**员工本人**（`biz_employee.user_id`，HR 代提时发起人 ≠ 员工）。
//!    注意**只在点「同意」时拦**：流程快照里 `self_approval = ALLOW`（HR-26 的 V0393），
//!    `FORBID` 会让「部门负责人自己请假」的单**派发不出来** → 死单。
//! 3. **没流程 fail-closed**：在 `leave_handler::submit_leave`（409 + `66033`），本桥不参与。
//! 4. **单据作废必须终止在跑实例**：[`cancel_approval_of`]。
//!
//! # 幂等 / 陈旧回调（本桥比 ERP 几个桥多一道闸）
//!
//! 审批单可能在单据侧已经被处理过（HR 撤回 → 回草稿 → 重提，新实例在跑），
//! 此时**旧实例**的回调才姗姗来迟。若只按「状态是 SUBMITTED 就推」，会把
//! 新一轮审批中的单直接推成 `APPROVED`/`DRAFT`。所以 [`settle`] 要求
//! `biz_leave_request.approval_instance_id` **就是**这个实例；指向别的实例 → 跳过（记 WARN）。
//!
//! 例外：`approval_instance_id` 为 `NULL` 且状态是 `SUBMITTED` 时**照常推进**
//! （HR-25 之前的历史数据 / 人工改库把引用清空过）—— 这条兜底是为了不让老单变成死单。
//!
//! # 不改考勤
//!
//! 设计 §4.3 第 1 行说得很明确：请假审批通过**不改考勤**（考勤由导入 / 人工体现，
//! daemon 只做校验）。所以本桥只动 `biz_leave_request.status` + `biz_status_log`。

use nik::prelude::*;
use nik::uuid::Uuid;
use std::result::Result;

use crate::business::flow::{engine, model as flow_model};
use crate::business::hr::leave_dto::{self, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED};
use crate::business::hr::leave_model::{self as model, LeaveRow};
use crate::business::shared::state_machine::{self, Operator};
use crate::shared::error_code;

/// 请假审批的业务类型（与 `V0393` 种子 / `leave_handler::FLOW_BIZ_TYPE` 同口径）。
pub const BIZ_TYPE: &str = "HR_LEAVE";
/// 请假审批的流程编码（与 `V0393` 种子 / `leave_handler::FLOW_CODE` 同口径）。
pub const FLOW_CODE: &str = "hr_leave";

// ==================== ① 审批结论 → 业务语义 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveVerdict {
    /// 同意 → `SUBMITTED → APPROVED`。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`（打回，意见进状态日志）。
    BackToDraft,
}

impl LeaveVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            LeaveVerdict::Approve => "APPROVE",
            LeaveVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动请假单）。
///
/// ⚠️ 这里**白名单**列终态，不能写成「非 APPROVED 就打回」：
/// `approve_task` 在「多节点流程批完一站、实例还在跑」时回调传来的是 `PENDING`，
/// 用「非 APPROVED 就打回」会把刚过直属上级、还在等人事复核的**长假单直接打回草稿**
/// （`erp/expense_bridge` 的 FIN-3 真机首跑踩过同一个坑）。
///
/// `hr_leave` 种子是两段式（直属上级 → ≥3 天再加人事经理），这个坑在本模块是
/// **必然场景**而不是边缘场景，所以单测专门钉住 `PENDING`。
pub fn leave_verdict(instance_status: &str) -> Option<LeaveVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(LeaveVerdict::Approve),
        "REJECTED" | "REJECTED_TERMINATED" | "WITHDRAWN" | "CANCELLED" | "TERMINATED" => {
            Some(LeaveVerdict::BackToDraft)
        }
        _ => None,
    }
}

// ==================== ② 回调：审批结论 → 请假单 ====================

/// 审批单落终态 → 回写请假单状态（`FlowCallback` 身份）。
///
/// 返回值：被推进的请假单 id（`None` = 与本桥无关 / 中间态 / 失败已记日志）。
pub async fn on_leave_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let Some(verdict) = leave_verdict(instance_status) else {
        tracing::debug!(%instance_id, instance_status, "请假审批仍在进行：不回写单据");
        return None;
    };
    match settle(instance_id, verdict).await {
        Ok(Some(leave_id)) => {
            tracing::info!(
                %instance_id,
                %leave_id,
                verdict = verdict.as_str(),
                instance_status,
                "请假审批结论已回写单据"
            );
            Some(leave_id)
        }
        Ok(None) => None,
        Err(error) => {
            // 通过失败用 ERROR：审批过了但单据还在「审批中」= 员工请假没落库（考勤口径错）。
            // 打回失败用 WARN：单据仍是审批中，至少还有待办推它。
            if verdict == LeaveVerdict::Approve {
                tracing::error!(
                    %instance_id,
                    code = error.code,
                    msg = %error.msg,
                    "请假审批通过但单据回写失败（审批单不回滚）：单据仍停在 SUBMITTED，需人工重试"
                );
            } else {
                tracing::warn!(
                    %instance_id,
                    code = error.code,
                    msg = %error.msg,
                    "请假审批未通过但打回失败（审批单不回滚）"
                );
            }
            None
        }
    }
}

/// [`on_leave_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: LeaveVerdict) -> Result<Option<Uuid>, ApiError> {
    // ① 审批单必须是「请假审批」类的（别的业务类型与本回调无关 —— 全量调用不按 type 分流）
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询请假审批单"))?
    else {
        tracing::debug!(%instance_id, "审批单不存在，跳过请假回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(leave_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 操作人 = 最后一票的审批人（审计列要写「谁批的」，AGENTS.md 约定 13）
    let operator = approver_operator(&instance, instance_id).await?;
    let comment = latest_reject_comment(instance_id)
        .await
        .map_err(|error| internal(error, "查询驳回意见"))?;
    let (to_status, reason) = match verdict {
        LeaveVerdict::Approve => (STATUS_APPROVED, "请假审批通过".to_string()),
        LeaveVerdict::BackToDraft => (
            STATUS_DRAFT,
            match comment.as_deref().map(str::trim) {
                Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
                _ => "审批驳回（未填写意见）".to_string(),
            },
        ),
    };

    let tx = db()
        .begin()
        .await
        .map_err(|error| internal(error, "开启请假审批回写事务"))?;
    let outcome = async {
        // ③ 锁单：并发下「HR 撤回」与「审批通过」只能有一个生效
        let Some(row) = model::lock_in(&tx, leave_id)
            .await
            .map_err(|error| internal(error, "锁定请假单"))?
        else {
            tracing::warn!(%leave_id, "请假单不存在或已删除，跳过审批回调");
            return Ok::<Option<Uuid>, ApiError>(None);
        };
        // ④ 状态闸：只有审批中能接受结论（重复回调 / 已被撤回 → 幂等跳过）
        if row.status != STATUS_SUBMITTED {
            tracing::info!(
                leave_no = %row.leave_no,
                status = %row.status,
                verdict = verdict.as_str(),
                "请假单不在审批中（重复回调 / 已处理）：跳过"
            );
            return Ok(None);
        }
        // ⑤ 陈旧回调闸：这单已经挂在**另一张**实例上了（撤回后重提）→ 旧实例的结论不予采纳
        if let Some(current) = row.approval_instance_id
            && current != instance_id
        {
            tracing::warn!(
                leave_no = %row.leave_no,
                %current,
                %instance_id,
                "请假单已改挂新实例：忽略旧实例的审批结论"
            );
            return Ok(None);
        }
        // ⑥ 状态机（唯一入口：边校验 + 写 biz_status_log）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_LEAVE_REQUEST,
            leave_id,
            &row.leave_no,
            &state_machine::LEAVE_EDGES,
            STATUS_SUBMITTED,
            to_status,
            operator.clone(),
            Some(&reason),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // ⑦ CAS 回写业务表（状态机只管日志，不回写业务表 —— 与 leave_handler 同口径）
        let affected = model::set_status(&tx, leave_id, STATUS_SUBMITTED, to_status, None, operator.id)
            .await
            .map_err(|error| model::write_error(error, "回写请假单审批结论", &row.leave_no))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("请假单 {} 的状态在审批回调期间已被改动，请刷新后重试", row.leave_no),
            ));
        }
        Ok(Some(leave_id))
    }
    .await;
    match outcome {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|error| internal(error, "提交请假审批回写事务"))?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 这一票的操作人：最后一票的审批人；读不到退回**发起人**（审计列宁可写发起人也不留空）。
async fn approver_operator(instance: &flow_model::InstanceRow, instance_id: Uuid) -> Result<Operator, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await
        .map_err(|error| internal(error, "查询审批人"))?;
    let approver: Option<Uuid> = match rows.first() {
        None => None,
        Some(row) => row.get_typed("assignee_id").ok().flatten(),
    };
    let Some(approver) = approver else {
        // 免审直过（流程里没有审批节点）/ 待办已被清理：用发起人兜底。
        // 这条路径不会把 operator 留空 —— `biz_status_log.operator_id` 是审计锚点。
        return Ok(Operator::named(
            Some(instance.initiator_id),
            instance.initiator_name.clone().unwrap_or_default(),
        ));
    };
    let name = flow_model::user_profile(approver)
        .await
        .map_err(|error| internal(error, "查询审批人姓名"))?
        .map(|(name, _dept)| name)
        .unwrap_or_default();
    Ok(Operator::named(Some(approver), name))
}

/// 最近一条**驳回**意见（写进状态日志，让人看到被打回的理由）。
async fn latest_reject_comment(instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'REJECTED' \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("comment").ok().flatten()),
    }
}

// ==================== ③ 自审批守卫 ====================

/// 自审批判据（纯函数，单测逐条钉死）：返回**拒绝理由**，`None` = 放行。
///
/// 两条判据，任一命中即拦：
/// 1. `initiator_id == operator` —— 谁提交的谁不能批（与 `erp` 各桥同款）；
/// 2. `employee_user == Some(operator)` —— 请假单的**员工本人**不能批自己的假
///    （HR 代提时发起人是 HR，只判第 1 条会漏掉「员工自己也是这一签的审批人」）。
///
/// 为什么不把 `ALLOW` 改成 `FORBID`：`hr_leave` 种子的 `n_sup` 是
/// `INITIATOR_SUPERIOR`，部门负责人自己请假时**解析出来的审批人就是他本人**；
/// `FORBID` 会让待办派发不出来 → 单子既没有待办也没有审批人 = 死单。
/// 所以留给引擎正常派发，**点同意时**在这里拦（设计 §4.3 第 2 条）。
pub fn self_approval_reason(initiator_id: Uuid, employee_user: Option<Uuid>, operator: Uuid) -> Option<&'static str> {
    if initiator_id == operator {
        return Some("请假单的发起人不能审批自己提交的请假：请交给其他有审批权限的账号处理");
    }
    if employee_user == Some(operator) {
        return Some("内控要求：员工不能审批**自己**的请假单（请交给直属上级或人事处理）");
    }
    None
}

/// 审批入口的内控闸（**事务之前**）：发起人 / 请假员工本人不能批自己那一票 → 403 + 65019。
///
/// 与本业务无关的待办零影响：`biz_type != HR_LEAVE` 直接放行。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| internal(error, "查询待办"))?
    else {
        // 待办不存在交给后续流程报 404（这里不抢它的错）
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| internal(error, "查询审批单"))?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    let employee_user = match instance.biz_id {
        Some(leave_id) => employee_user_of(leave_id)
            .await
            .map_err(|error| internal(error, "查询请假员工账号"))?,
        None => None,
    };
    match self_approval_reason(instance.initiator_id, employee_user, operator) {
        None => Ok(()),
        Some(reason) => Err(ApiError::new(403, error_code::FLOW_SELF_APPROVAL_FORBIDDEN, reason)),
    }
}

/// 请假单对应的**员工登录账号**（`biz_leave_request.employee_id → biz_employee.user_id`）。
///
/// 悬空外键 / 未绑账号 → `None`（只按发起人判，不因为查不到就放行不了）。
async fn employee_user_of(leave_id: Uuid) -> DbResult<Option<Uuid>> {
    // SCOPE-EXEMPT: 内控按主键取 1 行（员工账号），不做数据范围裁剪。
    let row = db()
        .query_first_sql(
            "SELECT e.user_id FROM biz_leave_request l \
               JOIN biz_employee e ON e.id = l.employee_id \
              WHERE l.id = ?::uuid AND l.deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&leave_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => row.get_typed("user_id"),
    }
}

// ==================== ④ 实例终止（撤回 / 作废） ====================

/// 终止请假单上挂着的审批实例（在跑的才终止），返回终止后的实例状态
/// （`None` = 本来就没有实例）。
///
/// 由 `leave_handler::cancel_leave` 在**同一事务**里调用：
///
/// - **撤回**：不终止，审批人待办里会留一张「已经回到草稿」的单，点同意会推一个
///   不存在的状态；并且重提时新实例会撞 `uk_biz_flow_instance_biz_active`（`PENDING` 部分唯一索引）。
/// - **作废 / 销假**：已经批完的单没有在跑实例，这里返回 `None`（不动历史轨迹）。
///
/// **已落终态的实例不动**：例如审批人刚驳回、HR 同时在撤回 —— 单据状态机自己会挡住
/// 不合法的边，这里不必二次判定。
pub async fn cancel_approval_of(tx: &Transaction, leave: &LeaveRow) -> Result<Option<String>, ApiError> {
    let Some(instance_id) = leave.approval_instance_id else {
        return Ok(None);
    };
    let Some(instance) = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| internal(error, "查询请假审批实例"))?
    else {
        return Ok(None);
    };
    if !instance.is_pending() {
        return Ok(Some(instance.status));
    }
    let out = engine::terminate_instance(tx, instance_id, "CANCELLED", "请假单撤回 / 作废：审批终止")
        .await
        .map_err(|error| internal(error, "终止请假审批实例"))?;
    if let Some(rejected) = out.rejected {
        // FIX-1：原样透传（重建会把 65018 的结构化载荷丢掉）
        return Err(rejected);
    }
    Ok(Some("CANCELLED".to_string()))
}

// ==================== 工具 ====================

/// `DbError` → `ApiError`（内部错误）。
pub fn internal(error: DbError, ctx: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{ctx}失败：{error}"))
}

/// 请假单状态域里必须有本桥要写的两个状态（防止 `leave_dto` 改字面量后桥静默写错值）。
pub fn statuses_in_domain() -> bool {
    leave_dto::STATUS_VALUES.contains(&STATUS_APPROVED)
        && leave_dto::STATUS_VALUES.contains(&STATUS_SUBMITTED)
        && leave_dto::STATUS_VALUES.contains(&STATUS_DRAFT)
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 通过 / 驳回 / 中间态三条路径的映射（本卡验收项「单测覆盖三条路径」的映射层）。
    ///
    /// `PENDING` 那条是**必然场景**：`hr_leave` 是两段式流程，长假单在直属上级批完后
    /// `approve_task` 回调的就是 `PENDING`；如果这里映射成「打回」，长假单会在人事复核前
    /// 直接回草稿（FIN-3 的同一个坑）。
    #[test]
    fn verdict_maps_three_paths() {
        // 通过
        assert_eq!(leave_verdict("APPROVED"), Some(LeaveVerdict::Approve));
        assert_eq!(leave_verdict(" approved "), Some(LeaveVerdict::Approve));
        // 驳回 / 撤回 / 终止 → 打回草稿
        for status in [
            "REJECTED",
            "REJECTED_TERMINATED",
            "rejected_terminated",
            "WITHDRAWN",
            "CANCELLED",
            "TERMINATED",
        ] {
            assert_eq!(
                leave_verdict(status),
                Some(LeaveVerdict::BackToDraft),
                "{status} 必须打回草稿（不是关闭、不是作废）"
            );
        }
        // 中间态 / 不认识 → 一律不动单据
        for status in ["PENDING", "", "STARTED", "APPROVED2", "DRAFT"] {
            assert_eq!(leave_verdict(status), None, "{status} 不该回写请假单");
        }
    }

    /// ② 自审批守卫的两条判据 + 放行（本卡验收项「自审批被拦」的逻辑层）。
    #[test]
    fn self_approval_blocks_initiator_and_employee() {
        let initiator = Uuid::now_v7();
        let employee_user = Uuid::now_v7();
        let other = Uuid::now_v7();

        // 发起人自己批 → 拦
        assert!(self_approval_reason(initiator, Some(employee_user), initiator).is_some());
        // 员工本人批（HR 代提时发起人不是员工）→ 拦
        assert!(self_approval_reason(initiator, Some(employee_user), employee_user).is_some());
        // 员工档案没绑账号（employee_user = None）→ 只按发起人判，不误伤
        assert!(self_approval_reason(initiator, None, initiator).is_some());
        assert!(self_approval_reason(initiator, None, other).is_none());
        // 无关的人 → 放行
        assert!(self_approval_reason(initiator, Some(employee_user), other).is_none());
    }

    /// ③ 业务类型 / 流程编码必须与 `V0393` 种子、`leave_handler` 同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed_and_handler() {
        assert_eq!(BIZ_TYPE, "HR_LEAVE");
        assert_eq!(FLOW_CODE, "hr_leave");
        assert_eq!(BIZ_TYPE, super::super::leave_handler::FLOW_BIZ_TYPE);
        assert_eq!(FLOW_CODE, super::super::leave_handler::FLOW_CODE);
    }

    /// ④ 边表与状态机注册表逐条一致（防两处漂移）+ 本桥写的状态都在状态域里。
    #[test]
    fn edges_match_registry_and_statuses_in_domain() {
        assert_eq!(
            state_machine::LEAVE_EDGES.as_slice(),
            state_machine::Entity::LeaveRequest.edges(),
            "leave_bridge 用的边表必须与状态机注册表完全一致"
        );
        assert_eq!(
            state_machine::LEAVE_EDGES.as_slice(),
            state_machine::Entity::LeaveRequest.edges(),
        );
        let edges = state_machine::LEAVE_EDGES;
        assert!(
            edges.contains(&(STATUS_SUBMITTED, STATUS_APPROVED)),
            "SUBMITTED → APPROVED 必须是合法边"
        );
        assert!(
            edges.contains(&(STATUS_SUBMITTED, STATUS_DRAFT)),
            "SUBMITTED → DRAFT（打回）必须是合法边"
        );
        assert!(statuses_in_domain());
    }

    /// ⑤ 状态常量字面量（错一个字就是库层 `ck_biz_leave_status` 违约）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(STATUS_SUBMITTED, "SUBMITTED");
        assert_eq!(STATUS_APPROVED, "APPROVED");
        assert_eq!(STATUS_DRAFT, "DRAFT");
    }
}

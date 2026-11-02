//! 采购订单审批的**业务回调 + 反向兜底**（P-FLOW-3A · 入库链第一环）。
//!
//! 与 `stockin_bridge`（收货作废 / 收货过账）、`payable_bridge`（付款）并列的第四类业务回调：
//! 审批单落终态之后，把结论翻译成采购订单的状态。**彼此互不影响**。
//!
//! # 三个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_po_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT`（打回，意见写 `remark`） |
//! | [`cancel_approval_of`] | 采购单删除 / 作废等「其它路径」 | 把在跑的审批**终止**（`biz_flow_task` / 节点 / 主表一起翻 `CANCELLED`） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 |
//!
//! # 为什么要反向（P11 的教训）
//!
//! 任务被取消时没有撤审批单 → 审批中心留下孤儿待办，点进去 404。
//! 采购单这边同构的风险更大：孤儿实例会在 `uk_biz_flow_instance_biz_active` 上把
//! **下一次提交审批**永久堵死（`409` 且无法自愈）。所以本模块不只做「审批 → 单据」，
//! 还做「单据 → 审批」：
//!
//! ```text
//! · 采购单被删（软删）           → cancel_approval_of（delete_purchase_order 同一事务里调）
//! · 审批中作废（SUBMITTED→CANCELLED）→ cancel_approval_of（change_purchase_order_status 同一事务里调）
//! · 同一单重复提交               → 发起前查 biz_instance_running → 409（报文带 FLW 单号）
//! ```
//!
//! # 时序与失败口径（与 `stockin_bridge` / `payable_bridge` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。单据停在 `SUBMITTED` 由人工或下一轮对账处置
//! （采购单没有账，最坏情况是「审批过了、单据显示审批中」，不会产生错账）。

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase_model::{self, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED};
use crate::business::erp::separation;
use crate::business::flow::model as flow_model;
use crate::business::flow::settle_guard;
use crate::shared::error_code;

/// 采购订单审批的业务类型（与 V0182 种子 / `purchase_handler::submit_approval` 同口径）。
pub const BIZ_TYPE: &str = "PO_APPROVE";
/// 采购订单审批的流程编码。
pub const FLOW_CODE: &str = "po_approve";

/// 已发布的采购订单审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`PO_APPROVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的采购订单审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按「降级」处理 —— 直接 `DRAFT → SUBMITTED`
/// （保持 P-FLOW-3A 之前的行为）并打一条 `warn`，绝不因为没配流程就把提交锁死。
///
/// 排序口径与 `stockin_model::published_flow_of` 完全一致（`biz_type` 精确命中优先 →
/// `is_default` → 版本号 → 更新时间）。
pub async fn published_po_flow() -> DbResult<Option<PoFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'PO_APPROVE' OR lower(d.flow_code) = 'po_approve') \
              ORDER BY (d.biz_type = 'PO_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PoFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 回调：审批结论 → 采购单 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoVerdict {
    /// 同意 → `SUBMITTED → APPROVED`（可收货）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`（打回，附意见）。
    BackToDraft,
}

impl PoVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            PoVerdict::Approve => "APPROVE",
            PoVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动采购单）。
pub fn po_verdict(instance_status: &str) -> Option<PoVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(PoVerdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(PoVerdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进采购订单状态（`FlowCallback` 身份）。
///
/// | 实例状态 | 采购单 | 说明 |
/// |---|---|---|
/// | `APPROVED` | `SUBMITTED → APPROVED` | 之后才可以正式收货（`purchase_receivable` 收 APPROVED） |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | `SUBMITTED → DRAFT` | 打回草稿，审批意见追加到 `remark` |
/// | 其它（`PENDING` / 空） | 不动 | 还在跑 |
///
/// 返回受影响的采购单 id（没推进 / 不该推进 → `None`）。**不返回错误**：审批已落库，
/// 业务侧失败只记日志（与 `stockin_bridge::on_post_settled` 同口径）。
pub async fn on_po_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = po_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(order_id)) => {
            tracing::info!(
                %instance_id,
                %order_id,
                verdict = verdict.as_str(),
                "采购订单审批结论已回调单据"
            );
            Some(order_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "采购订单审批结论回调失败（不影响审批单，单据仍是已提交）"
            );
            None
        }
    }
}

/// [`on_po_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: PoVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「采购订单审批」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过采购订单审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(order_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 单据必须存在（当前状态能不能收这个结论交给 ③ 的统一前置校验）
    let Some(row) = purchase_model::find_order(order_id).await? else {
        tracing::warn!(%order_id, "采购订单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ③ 审批人（落库用）：引擎把「谁点的」写在待办行，实例行没有该字段
    let approver = latest_completed_assignee(instance_id).await?;
    // ④ 审批意见（驳回时写进 remark，让人一眼看到为什么被打回）
    let comment = latest_reject_comment(instance_id).await?;
    let (to_status, reason) = match verdict {
        PoVerdict::Approve => (STATUS_APPROVED, "采购订单审批通过".to_string()),
        PoVerdict::BackToDraft => (
            STATUS_DRAFT,
            match comment.as_deref().map(str::trim) {
                Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
                _ => "审批驳回（未填写意见）".to_string(),
            },
        ),
    };
    // ★ C1（t_1e174431）：写目标状态前先校验采购单当前状态还在不在「能接受这个审批结论」
    //   的合法路径上（状态机边 `SUBMITTED → APPROVED|DRAFT`）。单据已作废 / 已被别的
    //   路径推走 → 不落状态 + WARN + 审批实例留痕（审批单本身仍是它自己的结论）。
    if !settle_guard::gate(
        &instance,
        crate::business::shared::state_machine::Entity::PurchaseOrder,
        &row.order_no,
        &row.status,
        STATUS_SUBMITTED,
        to_status,
    )
    .await
    {
        return Ok(None);
    }
    let now = Utc::now();
    let tx = db().begin().await?;
    let outcome = async {
        crate::business::shared::state_machine::transition(
            &tx,
            crate::business::shared::state_machine::ENTITY_PURCHASE_ORDER,
            order_id,
            &row.order_no,
            &purchase_model::LEGAL_EDGES,
            STATUS_SUBMITTED,
            to_status,
            approver,
            Some(&reason),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        let affected = purchase_model::apply_status(
            &tx,
            order_id,
            STATUS_SUBMITTED,
            to_status,
            Some(&reason),
            approver,
            now,
        )
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("采购订单状态回写失败：{error}"),
            )
        })?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "采购订单状态在审批回调期间已被改动，请刷新后重试",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(order_id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            // 回调失败不回滚审批单：单据停在 SUBMITTED 等人工处置
            tracing::warn!(
                %order_id,
                code = error.code,
                msg = %error.msg,
                "采购订单审批结论回写失败"
            );
            Ok(None)
        }
    }
}

// ==================== 内控：制单人不能批自己的采购单 ====================

/// 内控用：这张采购单的**制单人**（`biz_purchase_order.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
/// 查不到单 / 单没有 `create_by`（历史行 / 外部导入）→ `None`：只是没人被排除，
/// **不影响**审批能不能发起（内控不能因为取不到数据就把流程锁死）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(order_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪（裁剪会让
    // 「看不到这张单的人」反而成为可审批人，与内控目的相反）。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => {
            let creator: Option<Uuid> = row.get_typed("create_by")?;
            Ok(creator)
        }
    }
}

/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
///
/// # 判据（任一命中即 403）
///
/// 1. `operator` == `biz_purchase_order.create_by`（**制单人**）；
/// 2. `operator` == 审批单的 `initiator_id`（发起人；代提场景下发起人 ≠ 制单人）。
///
/// # 为什么不放在流程节点的 `self_approval=FORBID`
///
/// 与 `payable_bridge::guard_self_approval` 同款：`FORBID` 在**发起时**就 `422 + 65019`
/// （整单起不来），而内控要的是「提交成功、但**这一票**点不动」。所以流程节点写 `ALLOW`
/// （待办照常可见、留痕），真正的否决在这里。
///
/// 与本业务无关的待办（其它 `biz_type`）直接放行，零影响。
pub async fn guard_self_approval(
    task_id: Uuid,
    operator: Uuid,
) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id).await.map_err(|error| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("查询待办失败：{error}"),
        )
    })?
    else {
        // 待办不存在交给后续流程报 404（这里不抢它的错）
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("查询审批单失败：{error}"),
            )
        })?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    // ERP-B2：「制单与审批分离」开关（默认开 = 改动前的内控闸；关掉 = 小团队允许本人自审）
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(order_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = purchase_model::find_order(order_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的采购订单：采购单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.order_no,
                separation::approval_hint(separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
}

// ==================== 反向：终止在跑的审批 ====================

/// 终止某张采购单的**在跑审批**（单据删除 / 审批中作废时用，与调用方同一事务）。
///
/// 幂等：没有在跑的实例返回 `Ok(None)`；返回 `Some(实例数)` 才表示真的终止了。
pub async fn cancel_approval_of(
    ex: &impl SqlExecutor,
    order_id: Uuid,
    order_no: &str,
    reason: &str,
) -> DbResult<Option<u64>> {
    let cancelled = flow_model::cancel_pending_of_biz(ex, order_id, order_no, reason).await?;
    if cancelled.instances > 0 {
        tracing::info!(
            %order_id,
            order_no = order_no,
            instances = cancelled.instances,
            tasks = cancelled.tasks,
            reason = reason,
            "采购单在跑的审批已终止（不再留孤儿待办）"
        );
        Ok(Some(cancelled.instances))
    } else {
        Ok(None)
    }
}

/// 最近一条**已完成**待办的审批人（多节点取最后一票）。读不到 → `None`。
async fn latest_completed_assignee(instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
              ORDER BY COALESCE(completed_at, created_at) DESC, created_at DESC \
              LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id").ok().flatten()),
    }
}

/// 最近一条**驳回**意见（写进采购单 `remark`，让人看到被打回的理由）。
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

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 实例状态 → 业务语义的映射口径（钉死：只有 APPROVED 会让单据进入可收货态）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(po_verdict("APPROVED"), Some(PoVerdict::Approve));
        assert_eq!(po_verdict(" approved "), Some(PoVerdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                po_verdict(status),
                Some(PoVerdict::BackToDraft),
                "{status} 必须打回草稿（不是关闭、不是取消）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(po_verdict(status), None, "{status} 不该推进采购单");
        }
    }

    /// ② 业务类型 / 流程编码必须与 V0182 种子同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed() {
        assert_eq!(BIZ_TYPE, "PO_APPROVE");
        assert_eq!(FLOW_CODE, "po_approve");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
    }

    /// ③ 状态常量不能写错（错一个字就是库层 check 违约 / 前端徽标空白）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(STATUS_SUBMITTED, "SUBMITTED");
        assert_eq!(STATUS_APPROVED, "APPROVED");
        assert_eq!(STATUS_DRAFT, "DRAFT");
    }

    /// ④ 状态机必须放行「审批通过」与「驳回打回」两条边（漏一条 = 回调 409）。
    #[test]
    fn state_machine_allows_approval_edges() {
        assert!(
            purchase_model::can_transition(STATUS_SUBMITTED, STATUS_APPROVED),
            "SUBMITTED → APPROVED 必须合法，否则审批通过的回调会 409"
        );
        assert!(
            purchase_model::can_transition(STATUS_SUBMITTED, STATUS_DRAFT),
            "SUBMITTED → DRAFT 必须合法，否则驳回打回会 409"
        );
        assert!(
            purchase_model::can_transition(STATUS_DRAFT, STATUS_SUBMITTED),
            "DRAFT → SUBMITTED 必须合法，否则打回后无法重新提交"
        );
        // 审批通过后可以收货、可以取消，但不能跳回草稿
        assert!(purchase_model::can_transition(
            STATUS_APPROVED,
            "PARTIAL_RECEIVED"
        ));
        assert!(purchase_model::can_transition(STATUS_APPROVED, "RECEIVED"));
        assert!(purchase_model::can_transition(STATUS_APPROVED, "CANCELLED"));
        assert!(
            !purchase_model::can_transition(STATUS_APPROVED, STATUS_DRAFT),
            "已审批的单据不能被打回草稿（否则已生效的承诺被悄悄撤销）"
        );
    }
}

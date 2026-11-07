//! 采购退货审批的**业务回调 + 内控**（P2-03）。
//!
//! 与 `requisition_bridge`（采购申请）、`purchase_bridge`（采购订单）并列的业务回调：
//! 审批单落终态之后，把结论翻译成退货单的状态。**彼此互不影响**。
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT`（打回，意见写 `remark`） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//!
//! # 时序与失败口径（与 `requisition_bridge` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。单据停在 `SUBMITTED` 由人工或下一轮对账处置
//! （退货单没有动账，最坏情况是「审批过了、单据显示审批中」，不会产生错账）。

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase_return_dto::{STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED};
use crate::business::erp::purchase_return_model as model;
use crate::business::erp::separation;
use crate::business::flow::model as flow_model;
use crate::shared::error_code;

/// 采购退货审批的业务类型（与 V0332 种子 / `purchase_return_handler::submit` 同口径）。
pub const BIZ_TYPE: &str = "PRT_APPROVE";
/// 采购退货审批的流程编码。
pub const FLOW_CODE: &str = "prt_approve";

/// 已发布的采购退货审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的采购退货审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按「默认拒绝 + 显式跳过」处理
/// （`approval_gate::authorize_missing_flow`：持 `skip-approval` 码 + 理由 ≥10 字才放行）。
pub async fn published_flow() -> DbResult<Option<ReturnFlow>> {
    // SCOPE-EXEMPT: 流程定义是全局配置，按 biz_type / flow_code 精确取 1 行，不分页、不裁剪。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'PRT_APPROVE' OR lower(d.flow_code) = 'prt_approve') \
              ORDER BY (d.biz_type = 'PRT_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(ReturnFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 回调：审批结论 → 退货单 ====================

/// 审批结论的业务语义（纯函数，单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 同意 → `SUBMITTED → APPROVED`（可以确认退货）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`（打回，附意见）。
    BackToDraft,
}

impl Verdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Approve => "APPROVE",
            Verdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动退货单）。
pub fn verdict_of(instance_status: &str) -> Option<Verdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(Verdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(Verdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进采购退货单状态（`FlowCallback` 身份）。
pub async fn on_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = verdict_of(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(return_id)) => {
            tracing::info!(
                %instance_id,
                %return_id,
                verdict = verdict.as_str(),
                "采购退货审批结论已回调单据"
            );
            Some(return_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "采购退货审批结论回调失败（不影响审批单，单据仍是已提交）"
            );
            None
        }
    }
}

/// [`on_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: Verdict) -> DbResult<Option<Uuid>> {
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过采购退货审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(return_id) = instance.biz_id else {
        return Ok(None);
    };
    let Some(row) = model::find_return(return_id).await? else {
        tracing::warn!(%return_id, "采购退货单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    if row.status != STATUS_SUBMITTED {
        tracing::debug!(
            %return_id,
            status = %row.status,
            verdict = verdict.as_str(),
            "采购退货当前状态不接受审批结论（跳过）"
        );
        return Ok(None);
    }
    let approver = latest_completed_assignee(instance_id).await?;
    let comment = latest_reject_comment(instance_id).await?;
    let (to_status, note) = match verdict {
        Verdict::Approve => (STATUS_APPROVED, "采购退货审批通过".to_string()),
        Verdict::BackToDraft => (
            STATUS_DRAFT,
            match comment.as_deref().map(str::trim) {
                Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
                _ => "审批驳回（未填写意见）".to_string(),
            },
        ),
    };
    let now = Utc::now();
    let tx = db().begin().await?;
    let outcome = async {
        let affected = if to_status == STATUS_APPROVED {
            model::mark_approved(&tx, return_id, Some(&note), approver, now).await?
        } else {
            model::mark_back_to_draft(&tx, return_id, Some(&note), approver, now).await?
        };
        if affected == 0 {
            return Err(DbError::InvalidValue(
                "采购退货状态在审批回调期间已被改动".to_string(),
            ));
        }
        Ok::<(), DbError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(return_id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::warn!(
                %return_id,
                %error,
                "采购退货审批结论回写失败"
            );
            Ok(None)
        }
    }
}

/// 最近一个已完成的审批人（写进状态日志的 `operator`）。
async fn latest_completed_assignee(instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status IN ('APPROVED','REJECTED') \
                AND assignee_id IS NOT NULL \
              ORDER BY completed_at DESC NULLS LAST LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("assignee_id").ok()),
    }
}

/// 最近一条驳回意见（拼进备注，审计据此知道为什么被打回）。
async fn latest_reject_comment(instance_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'REJECTED' AND comment IS NOT NULL \
              ORDER BY completed_at DESC NULLS LAST LIMIT 1",
            vec![Value::from(&instance_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("comment").ok()),
    }
}

// ==================== 内控：制单人不能批自己的退货 ====================

/// 内控用：这张退货单的**制单人**（`biz_purchase_return.create_by`）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(return_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_purchase_return WHERE id = ?::uuid",
            vec![Value::from(&return_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("create_by").ok()),
    }
}

/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
///
/// 判据（任一命中即 403）：操作人 == 退货单的 `create_by`（制单人），
/// 或操作人 == 审批单的 `initiator_id`（发起人）。与本业务无关的待办直接放行。
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
    // 「制单与审批分离」开关（默认开）
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(return_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_return(return_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己制单的采购退货：退货单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.return_no,
                separation::approval_hint(separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_mapping_is_strict() {
        assert_eq!(verdict_of("APPROVED"), Some(Verdict::Approve));
        assert_eq!(verdict_of("approved"), Some(Verdict::Approve));
        assert_eq!(verdict_of("REJECTED"), Some(Verdict::BackToDraft));
        assert_eq!(verdict_of("WITHDRAWN"), Some(Verdict::BackToDraft));
        assert_eq!(verdict_of("CANCELLED"), Some(Verdict::BackToDraft));
        assert_eq!(verdict_of("PENDING"), None, "在跑不动单据");
        assert_eq!(verdict_of(""), None);
    }

    #[test]
    fn constants_match_seed() {
        assert_eq!(BIZ_TYPE, "PRT_APPROVE");
        assert_eq!(FLOW_CODE, "prt_approve");
    }
}

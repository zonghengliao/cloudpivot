//! 采购计划审批的**业务回调 + 内控**（M4-02 · 采购计划 CRUD + 审批）。
//!
//! 与 `purchase_bridge`（采购订单审批）、`requisition_bridge`（采购申请审批）并列的业务回调：
//! 审批单落终态之后，把结论翻译成采购计划单的状态。**彼此互不影响**。
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_pp_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT`（打回，意见写 `remark`） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//!
//! # 为什么状态域只有三个值（与表结构口径一致）
//!
//! ```text
//! DRAFT（编制中，可改可删）→ SUBMITTED（审批中，必有实例）→ APPROVED（可转采购申请，M4-03）
//! 驳回 / 撤回由本回调打回 DRAFT，而不是造一个「已驳回」状态 ——
//! 打回后的人要做的事和草稿完全一样（改完再提），多一个状态只会让按钮判定分叉。
//! ```
//!
//! # 时序与失败口径（与 `requisition_bridge` / `purchase_bridge` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。计划单没有账（不入账、不占预算），
//! 最坏情况是「审批过了、单据显示审批中」，下一轮人工或对账能推进，不产生错账。
//!
//! # 「事务提交后失败必须有补偿」在本卡的应用（AGENTS 约定 11）
//!
//! 提交审批的免审直过分支（流程没有审批节点 → 实例当场 `APPROVED`）在事务提交后
//! **立即回调** [`on_pp_settled`]；它失败时单据停在 `SUBMITTED` 且没有待办 ——
//! 这正是约定 11 点名的最高危形态。所以提交侧在回调失败时**显式打回草稿**
//! （见 `purchase_plan_handler::submit_plan` 的 `auto_finished` 分支）：
//! 用户能重新提交，不需要工程师改库。

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase::purchase_plan_model::{
    self as model, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED,
};
use crate::business::flow::model as flow_model;
use cloudpivot_kernel::business::shared::separation;
use cloudpivot_kernel::shared::error_code;

/// 采购计划审批的业务类型（与 V0447 种子 / `purchase_plan_handler::submit_plan` 同口径）。
pub const BIZ_TYPE: &str = "PP_APPROVE";
/// 采购计划审批的流程编码。
pub const FLOW_CODE: &str = "pp_approve";

/// 已发布的采购计划审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PpFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`PP_APPROVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的采购计划审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按「默认拒绝 + 显式跳过」处理
/// （`approval_gate::authorize_missing_flow`：持 `skip-approval` 码 + 理由 ≥10 字才放行）。
///
/// 排序口径与 `requisition_bridge::published_pr_flow` 完全一致
/// （`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间）。
pub async fn published_pp_flow() -> DbResult<Option<PpFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'PP_APPROVE' OR lower(d.flow_code) = 'pp_approve') \
              ORDER BY (d.biz_type = 'PP_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PpFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 回调：审批结论 → 计划单 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PpVerdict {
    /// 同意 → `SUBMITTED → APPROVED`（可转采购申请 / 参与执行率统计）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`（打回，附意见）。
    BackToDraft,
}

impl PpVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            PpVerdict::Approve => "APPROVE",
            PpVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动计划单）。
pub fn pp_verdict(instance_status: &str) -> Option<PpVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(PpVerdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(PpVerdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进采购计划单状态（`DocReader::on_settled` 身份）。
pub async fn on_pp_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = pp_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(plan_id)) => {
            tracing::info!(
                %instance_id,
                %plan_id,
                verdict = verdict.as_str(),
                "采购计划审批结论已回调单据"
            );
            Some(plan_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "采购计划审批结论回调失败（不影响审批单，单据仍是已提交）"
            );
            None
        }
    }
}

/// [`on_pp_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: PpVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「采购计划审批」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过采购计划审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(plan_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 单据必须存在
    let Some(row) = model::find_plan(db(), plan_id).await? else {
        tracing::warn!(%plan_id, "采购计划不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ③ 状态闸：只有 SUBMITTED 能接受审批结论（已被别的路径推走 → 不动）
    if row.status != STATUS_SUBMITTED {
        tracing::debug!(
            %plan_id,
            status = %row.status,
            verdict = verdict.as_str(),
            "采购计划当前状态不接受审批结论（跳过）"
        );
        return Ok(None);
    }
    let approver = latest_completed_assignee(instance_id).await?;
    let comment = latest_reject_comment(instance_id).await?;
    let (to_status, reason) = match verdict {
        PpVerdict::Approve => (STATUS_APPROVED, "采购计划审批通过".to_string()),
        PpVerdict::BackToDraft => (
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
            model::mark_approved(&tx, plan_id, Some(&reason), approver, now).await?
        } else {
            model::mark_back_to_draft(&tx, plan_id, Some(&reason), approver, now).await?
        };
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "采购计划状态在审批回调期间已被改动，请刷新后重试",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(plan_id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::warn!(
                %plan_id,
                code = error.code,
                msg = %error.msg,
                "采购计划审批结论回写失败"
            );
            Ok(None)
        }
    }
}

// ==================== 内控：制单人不能批自己的计划 ====================

/// 内控用：这张计划单的**制单人**（`biz_purchase_plan.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(plan_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_purchase_plan WHERE id = ?::uuid",
            vec![Value::from(&plan_id)],
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
/// 判据（任一命中即 403）：操作人 == 计划单的 `create_by`（制单人），
/// 或操作人 == 审批单的 `initiator_id`（发起人）。
/// 与本业务无关的待办（其它 `biz_type`）直接放行，零影响。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("查询待办失败：{error}")))?
    else {
        // 待办不存在交给后续流程报 404（这里不抢它的错）
        return Ok(());
    };
    let Some(instance) = flow_model::find_instance(task.instance_id)
        .await
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("查询审批单失败：{error}")))?
    else {
        return Ok(());
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(());
    }
    // ERP-B2：「制单与审批分离」开关（默认开 = 原有内控闸）
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(plan_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_plan(db(), plan_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的采购计划：计划单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.plan_no,
                separation::approval_hint(separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
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

/// 最近一条**驳回**意见（写进计划单 `remark`，让人看到被打回的理由）。
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

    /// ① 实例状态 → 业务语义（只有 APPROVED 才能变已批准）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(pp_verdict("APPROVED"), Some(PpVerdict::Approve));
        assert_eq!(pp_verdict(" approved "), Some(PpVerdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                pp_verdict(status),
                Some(PpVerdict::BackToDraft),
                "{status} 必须打回草稿（不是关闭、不是取消）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(pp_verdict(status), None, "{status} 不该推进计划单");
        }
    }

    /// ② 业务类型 / 流程编码必须与 V0447 种子同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed() {
        assert_eq!(BIZ_TYPE, "PP_APPROVE");
        assert_eq!(FLOW_CODE, "pp_approve");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
        assert_eq!(FLOW_CODE, FLOW_CODE.to_lowercase());
    }

    /// ③ 状态常量不能写错（错一个字就是库层 check 违约 / 前端徽标空白）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(STATUS_DRAFT, "DRAFT");
        assert_eq!(STATUS_SUBMITTED, "SUBMITTED");
        assert_eq!(STATUS_APPROVED, "APPROVED");
    }
}

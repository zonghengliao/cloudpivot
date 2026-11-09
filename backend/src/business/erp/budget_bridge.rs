//! 预算审批的**业务回调**（`budget_approve`）：审批结论 → 预算版本状态。
//!
//! # 三个入口（与 `purchase_bridge` / `contract_bridge` 同款分工）
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_budget_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → ACTIVE` + 同槽旧版本 `ACTIVE → SUPERSEDED`（同一事务）；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT` |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 发起人 / 制单人不能批自己这一票 → 403 + 65019 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//!
//! # 为什么「通过」这一步必须在一个事务里做两件事
//!
//! `uk_biz_budget_slot_active`（`dept_id, account_id, fiscal_year WHERE status='ACTIVE'`）
//! 是库级兜底：新版置 ACTIVE 之前旧版没退位，就是 23505（ARCH-6 §3.3.2 硬规则 1）。
//! 顺序固定：**先退旧版**（`ACTIVE → SUPERSEDED` + `retired_at`）→ **再置新版 ACTIVE**。
//!
//! # 失败口径（与其它 bridge 一致）
//!
//! 回调跑在审批事务**提交之后**：审批结论已落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None` —— 预算单停在 `SUBMITTED`，人工或下一轮对账处置
//! （预算没有账，最坏是「批过了但没生效」，不会产生错账）。
//!
//! # 不产生凭证
//!
//! 预算审批本身**不落账**：预算是「额度控制」，不是经济业务发生（ARCH-6 §6.4 的边界 ①）。
//! 真正的凭证由执行侧产生 —— 报销入账（`EXPENSE_REPORT`）与采购入库（`PURCHASE_IN`）
//! 都走各自的既有链路，预算模块一行凭证代码都不需要写。

use nik::prelude::chrono::{Datelike, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::budget_model::{self, STATUS_ACTIVE, STATUS_DRAFT, STATUS_SUBMITTED};
use crate::business::flow::model as flow_model;
use crate::business::flow::settle_guard;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 预算审批的业务类型（与 V0228 种子 / `budget_handler::submit` 同口径）。
pub const BIZ_TYPE: &str = "BUDGET_APPROVE";
/// 预算审批的流程编码。
pub const FLOW_CODE: &str = "budget_approve";

/// 已发布的预算审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetFlow {
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

/// 查「已发布的预算审批流程」。`None` = 没配（调用方 409 并指名要去发布流程）。
pub async fn published_budget_flow() -> DbResult<Option<BudgetFlow>> {
    // SCOPE-EXEMPT: 流程定义是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'BUDGET_APPROVE' OR lower(d.flow_code) = 'budget_approve') \
              ORDER BY (d.biz_type = 'BUDGET_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(BudgetFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetVerdictKind {
    /// 同意 → `SUBMITTED → ACTIVE`（同槽旧版退位）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`。
    BackToDraft,
}

impl BudgetVerdictKind {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            BudgetVerdictKind::Approve => "APPROVE",
            BudgetVerdictKind::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动预算单）。
pub fn budget_verdict(instance_status: &str) -> Option<BudgetVerdictKind> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(BudgetVerdictKind::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(BudgetVerdictKind::BackToDraft),
        _ => None,
    }
}

/// 生效起始日（ARCH-6 §3.2.1 的 `effective_from`）：审批通过日的**次月 1 日**。
///
/// 为什么不是「当天」：预算按自然月管节奏，月中生效会让当月「月度额度」变成
/// 「半个月的额度却按整月校验」；次月 1 日生效与月度闸的口径一致。
pub fn effective_from_of(approved: nik::prelude::chrono::NaiveDate) -> nik::prelude::chrono::NaiveDate {
    let (year, month) = if approved.month() == 12 {
        (approved.year() + 1, 1)
    } else {
        (approved.year(), approved.month() + 1)
    };
    nik::prelude::chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(approved)
}

/// 审批单落终态 → 推进预算版本状态（`FlowCallback` 身份）。
pub async fn on_budget_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = budget_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(budget_id)) => {
            tracing::info!(
                %instance_id,
                %budget_id,
                verdict = verdict.as_str(),
                "预算审批结论已回调预算单"
            );
            Some(budget_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "预算审批结论回调失败（不影响审批单，预算单仍是审批中）"
            );
            None
        }
    }
}

async fn settle(instance_id: Uuid, verdict: BudgetVerdictKind) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「预算审批」类的
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(budget_id) = instance.biz_id else {
        return Ok(None);
    };
    let Some(row) = budget_model::find(budget_id).await? else {
        tracing::warn!(%budget_id, "预算单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    let to_status = match verdict {
        BudgetVerdictKind::Approve => STATUS_ACTIVE,
        BudgetVerdictKind::BackToDraft => STATUS_DRAFT,
    };
    // ② 单据还在不在「等这个审批」的状态上（状态机 + 留痕统一走 settle_guard）
    if !settle_guard::gate(
        &instance,
        state_machine::Entity::Budget,
        &row.budget_no,
        &row.status,
        STATUS_SUBMITTED,
        to_status,
    )
    .await
    {
        return Ok(None);
    }
    let approver = latest_completed_assignee(instance_id).await?;
    let comment = latest_reject_comment(instance_id).await?;
    let reason = match verdict {
        BudgetVerdictKind::Approve => "预算审批通过".to_string(),
        BudgetVerdictKind::BackToDraft => match comment.as_deref().map(str::trim) {
            Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
            _ => "审批驳回（未填写意见）".to_string(),
        },
    };
    let now = Utc::now();
    let effective_from = matches!(verdict, BudgetVerdictKind::Approve).then(|| effective_from_of(now.date_naive()));
    let tx = db().begin().await?;
    let outcome = async {
        // ③ 通过：同槽旧版先退位（顺序不能反：库级唯一索引会 23505）
        if matches!(verdict, BudgetVerdictKind::Approve) {
            budget_model::supersede_active(&tx, row.dept_id, row.account_id, row.fiscal_year, row.id).await?;
        }
        // ④ 状态机唯一入口（写 biz_status_log）
        state_machine::transition(
            &tx,
            state_machine::ENTITY_BUDGET,
            row.id,
            &row.budget_no,
            &BUDGET_LEGAL_EDGES,
            STATUS_SUBMITTED,
            to_status,
            approver,
            Some(&reason),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // ⑤ CAS 回写状态 + 审批痕迹
        let affected =
            budget_model::apply_status(&tx, row.id, STATUS_SUBMITTED, to_status, None, approver, effective_from)
                .await
                .map_err(|error| {
                    ApiError::new(500, error_code::INTERNAL_ERROR, format!("预算单状态回写失败：{error}"))
                })?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "预算单状态在审批回调期间已被改动，请刷新后重试",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(row.id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::warn!(
                budget_id = %row.id,
                code = error.code,
                msg = %error.msg,
                "预算审批结论回写失败"
            );
            Ok(None)
        }
    }
}

/// 预算状态机的合法边（与 `state_machine::BUDGET_EDGES` 同源）。
pub const BUDGET_LEGAL_EDGES: [(&str, &str); 7] = state_machine::BUDGET_EDGES;

// ==================== 内控：制单人不能批自己的预算 ====================

/// 内控用：这张预算单的制单人。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(budget_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 按主键取 1 行的 create_by。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_budget WHERE id = ?::uuid",
            vec![Value::from(&budget_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("create_by"),
    }
}

/// 审批入口的内控闸（**事务之前**）：制单人 / 发起人不能批自己那一票 → 403 + 65019。
///
/// 与 `purchase_bridge::guard_self_approval` 同口径：流程节点写 `ALLOW`（待办可见、留痕），
/// 真正的否决在这里；与预算无关的待办直接放行（零影响）。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    let Some(task) = flow_model::find_task(task_id)
        .await
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("查询待办失败：{error}")))?
    else {
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
    if instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            "内控要求：预算的发起人不能审批自己提交的预算（请换一位审批人，或由他人代提）",
        ));
    }
    if let Some(budget_id) = instance.biz_id
        && let Some(creator) = internal_control_creator(Some(budget_id)).await.ok().flatten()
        && creator == operator
    {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            "内控要求：预算的制单人不能审批自己编制的预算（请换一位审批人）",
        ));
    }
    Ok(())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    /// ① 实例状态 → 业务语义：只有终态才动单据，`PENDING` 一律 None。
    #[test]
    fn verdict_mapping() {
        assert_eq!(budget_verdict("APPROVED"), Some(BudgetVerdictKind::Approve));
        assert_eq!(
            budget_verdict("rejected"),
            Some(BudgetVerdictKind::BackToDraft),
            "大小写不敏感"
        );
        assert_eq!(budget_verdict("WITHDRAWN"), Some(BudgetVerdictKind::BackToDraft));
        assert_eq!(budget_verdict("CANCELLED"), Some(BudgetVerdictKind::BackToDraft));
        assert_eq!(budget_verdict("PENDING"), None);
        assert_eq!(budget_verdict(""), None);
    }

    /// ② 生效日 = 审批通过日的次月 1 日（跨年也要对）。
    #[test]
    fn effective_from_is_next_month_first_day() {
        assert_eq!(
            effective_from_of(NaiveDate::from_ymd_opt(2026, 11, 4).expect("日期")),
            NaiveDate::from_ymd_opt(2026, 12, 1).expect("日期")
        );
        assert_eq!(
            effective_from_of(NaiveDate::from_ymd_opt(2026, 12, 31).expect("日期")),
            NaiveDate::from_ymd_opt(2027, 1, 1).expect("日期"),
            "12 月跨年"
        );
    }

    /// ③ 合法边表与状态机注册表逐条一致（防两处漂移）。
    #[test]
    fn legal_edges_match_registry() {
        assert_eq!(
            BUDGET_LEGAL_EDGES.as_slice(),
            state_machine::Entity::Budget.edges(),
            "budget_bridge 的边表必须与状态机注册表完全一致"
        );
    }
}

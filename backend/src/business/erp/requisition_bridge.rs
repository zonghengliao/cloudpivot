//! 采购申请审批的**业务回调 + 预算前置判定**（P2-02 · 需求申请 → 审批 → 转采购订单）。
//!
//! 与 `purchase_bridge`（采购订单审批）、`expense_bridge`（报销）并列的业务回调：
//! 审批单落终态之后，把结论翻译成采购申请单的状态。**彼此互不影响**。
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_pr_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT`（打回，意见写 `remark`） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//! | [`check_for_requisition`] | `purchase_requisition_handler::submit`（**写状态之前**） | 预算**只判不占**：超预算 → 单据标 `over_budget`，审批流按条件边加一道财务经理特殊审批 |
//!
//! # 为什么申请阶段「只判不占」
//!
//! 申请只是**需求**，真正占额度的是采购订单提交（`budget_guard::occupy_for_purchase`）。
//! 申请阶段也占，同一笔需求会在申请 + 下单两个节点各占一次，预算执行数直接翻倍。
//! 所以这里只做只读判定，把结论落成单据快照（`over_budget / over_amount / budget_reason`），
//! 由**流程定义里的条件边**决定要不要加一道特殊审批 —— 阈值与岗位客户自己改，代码里没有 if/else。
//!
//! # 时序与失败口径（与 `purchase_bridge` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。单据停在 `SUBMITTED` 由人工或下一轮对账处置
//! （申请单没有账，最坏情况是「审批过了、单据显示审批中」，不会产生错账）。

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::budget_guard::{self, BudgetVerdict, ControlMode, Slot};
use crate::business::erp::purchase_requisition_dto::{
    STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED,
};
use crate::business::erp::purchase_requisition_model::{self as model, BudgetSnapshot};
use crate::business::erp::separation;
use crate::business::flow::model as flow_model;
use crate::shared::error_code;

/// 采购申请审批的业务类型（与 V0331 种子 / `purchase_requisition_handler::submit` 同口径）。
pub const BIZ_TYPE: &str = "PR_APPROVE";
/// 采购申请审批的流程编码。
pub const FLOW_CODE: &str = "pr_approve";

/// 已发布的采购申请审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`PR_APPROVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的采购申请审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按「默认拒绝 + 显式跳过」处理
/// （`approval_gate::authorize_missing_flow`：持 `skip-approval` 码 + 理由 ≥10 字才放行）。
///
/// 排序口径与 `purchase_bridge::published_po_flow` 完全一致
///（`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间）。
pub async fn published_pr_flow() -> DbResult<Option<PrFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'PR_APPROVE' OR lower(d.flow_code) = 'pr_approve') \
              ORDER BY (d.biz_type = 'PR_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PrFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 预算：只判不占 ====================

/// 申请提交时的预算判定结果（含「解析不到科目」这个分支）。
#[derive(Debug, Clone)]
pub struct RequisitionBudget {
    /// 生效策略。
    pub mode: ControlMode,
    /// 是否解析到了预算科目。
    pub mapped: bool,
    /// 命中的映射类型（MATERIAL / MATERIAL_TYPE / WAREHOUSE / DEFAULT）。
    pub map_type: Option<String>,
    /// 预算科目 id。
    pub account_id: Option<Uuid>,
    /// 预算槽（期间取申请日期）。
    pub slot: Option<Slot>,
    /// 判定结果（`None` = 没校验）。
    pub verdict: Option<BudgetVerdict>,
    /// 一句话说明（日志 / 报文 / 单据快照用）。
    pub reason: String,
}

impl RequisitionBudget {
    /// 是否超预算。
    pub fn over(&self) -> bool {
        self.verdict.as_ref().is_some_and(|verdict| verdict.over)
    }

    /// 是否应该 409 阻断（`BLOCK` + 超预算）。
    ///
    /// 与采购侧同口径：只有**显式 BLOCK** 才硬拦；`WARN` 放行进审批（由条件边加签）。
    pub fn blocks(&self) -> bool {
        self.verdict
            .as_ref()
            .is_some_and(|verdict| verdict.blocks())
    }

    /// 超出金额（没超 / 没校验 = 0）。
    pub fn over_amount(&self) -> Decimal {
        self.verdict
            .as_ref()
            .and_then(|verdict| verdict.over_amount_opt())
            .unwrap_or(Decimal::ZERO)
    }

    /// 落库快照。
    ///
    /// `over_amount` 列是 `NUMERIC(18,4)`，而判定结果可能更细（6 位）：
    /// 这里先 4 位舍入，**超了但舍入成 0** 时抬到 0.0001 —— 否则会撞库层
    /// `ck_biz_prq_over_consistency`（`over_budget = (over_amount > 0)`），
    /// 让一张确实超预算的单提交失败。
    pub fn snapshot(&self) -> BudgetSnapshot {
        let over = self.over();
        let over_amount = if over {
            let rounded = self.over_amount().round_dp(4);
            if rounded <= Decimal::ZERO {
                Decimal::new(1, 4)
            } else {
                rounded
            }
        } else {
            Decimal::ZERO
        };
        BudgetSnapshot {
            checked: self.verdict.is_some(),
            over_budget: over,
            over_amount,
            reason: Some(self.reason.clone()),
            account_id: self.account_id,
            dept_id: self.slot.as_ref().map(|slot| slot.dept_id),
            period_code: self.slot.as_ref().map(|slot| slot.period_code.clone()),
        }
    }
}

/// 采购申请提交时的预算判定（**只读**，不写 usage）。
///
/// 科目解析顺序见 [`crate::business::erp::budget_model::resolve_for_po`]（物料 → 物料类型
/// → 仓库 → 默认兜底）；部门取**申请部门**（申请单自己声明的部门，不是提交人部门 ——
/// 采购代提时也要记到需求部门头上）。
///
/// 解析不到科目时按 ARCH-6 §3.2.5 的口径处置：
/// `OFF` / `WARN` → **不占用 + 告警**（映射是运维配置，配不全不该让下单停摆）；
/// `BLOCK` → 调用方 409（强管控下「配置缺失」是硬错误）。
pub async fn check_for_requisition(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    warehouse_id: Uuid,
    material_id: Option<Uuid>,
    period_code: &str,
    amount: Decimal,
) -> DbResult<RequisitionBudget> {
    let global = budget_guard::global_mode(ex).await?;
    // 采购侧的全局开关：`erp.budget.enforce_on_po = false` → 整条链路退化成 OFF。
    // 申请是采购链的上游（同一笔需求的「先声」），跟着同一个开关走，
    // 否则会出现「下单不校验、提申请反而被拦」的荒唐口径。
    if !budget_guard::enforce_on_po(ex).await? {
        return Ok(RequisitionBudget {
            mode: ControlMode::Off,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: "erp.budget.enforce_on_po = false：采购链（含申请）不做预算校验".to_string(),
        });
    }
    let material_type = match material_id {
        Some(material) => budget_guard::material_type_code_of(ex, material).await?,
        None => None,
    };
    let resolved = crate::business::erp::budget_model::resolve_for_po(
        ex,
        Some(dept_id),
        Some(warehouse_id),
        material_id,
        material_type.as_deref(),
    )
    .await?;
    let Some((account_id, map_type)) = resolved else {
        return Ok(RequisitionBudget {
            mode: global,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: format!(
                "申请未匹配到预算科目（部门 {dept_id} / 仓库 {warehouse_id}）：本单不计入预算；\
                 请管理员在「预算科目映射」里配置（物料 / 物料类型 / 仓库 / 默认兜底）"
            ),
        });
    };
    let Some(slot) = Slot::new(dept_id, account_id, period_code) else {
        return Ok(RequisitionBudget {
            mode: global,
            mapped: false,
            map_type: Some(map_type),
            account_id: Some(account_id),
            slot: None,
            verdict: None,
            reason: format!("申请日期 {period_code} 不是合法期间，本单不计入预算"),
        });
    };
    // 槽级策略来自该槽的 ACTIVE 预算（没编预算 → 继承全局）
    let slot_mode = budget_guard::active_control_mode(ex, &slot).await?;
    let mode = ControlMode::resolve(slot_mode.as_deref(), global);
    let snapshot = budget_guard::slot_snapshot(ex, &slot).await?;
    let verdict = BudgetVerdict::evaluate(
        mode,
        snapshot.budget_id,
        snapshot.year_limit,
        snapshot.year_used,
        snapshot.month_limit,
        snapshot.month_used,
        amount,
    );
    Ok(RequisitionBudget {
        mode,
        mapped: true,
        map_type: Some(map_type),
        account_id: Some(account_id),
        slot: Some(slot),
        reason: verdict.describe(),
        verdict: Some(verdict),
    })
}

// ==================== 回调：审批结论 → 申请单 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrVerdict {
    /// 同意 → `SUBMITTED → APPROVED`（可以转采购订单）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`（打回，附意见）。
    BackToDraft,
}

impl PrVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            PrVerdict::Approve => "APPROVE",
            PrVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动申请单）。
pub fn pr_verdict(instance_status: &str) -> Option<PrVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(PrVerdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(PrVerdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进采购申请单状态（`FlowCallback` 身份）。
pub async fn on_pr_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = pr_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(req_id)) => {
            tracing::info!(
                %instance_id,
                %req_id,
                verdict = verdict.as_str(),
                "采购申请审批结论已回调单据"
            );
            Some(req_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "采购申请审批结论回调失败（不影响审批单，单据仍是已提交）"
            );
            None
        }
    }
}

/// [`on_pr_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: PrVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「采购申请审批」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过采购申请审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(req_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 单据必须存在
    let Some(row) = model::find_requisition(req_id).await? else {
        tracing::warn!(%req_id, "采购申请单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ③ 状态闸：只有 SUBMITTED 能接受审批结论（已被别的路径推走 → 不动）
    if row.status != STATUS_SUBMITTED {
        tracing::debug!(
            %req_id,
            status = %row.status,
            verdict = verdict.as_str(),
            "采购申请当前状态不接受审批结论（跳过）"
        );
        return Ok(None);
    }
    let approver = latest_completed_assignee(instance_id).await?;
    let comment = latest_reject_comment(instance_id).await?;
    let (to_status, reason) = match verdict {
        PrVerdict::Approve => (STATUS_APPROVED, "采购申请审批通过".to_string()),
        PrVerdict::BackToDraft => (
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
            model::mark_approved(&tx, req_id, Some(&reason), approver, now).await?
        } else {
            model::mark_back_to_draft(&tx, req_id, Some(&reason), approver, now).await?
        };
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "采购申请状态在审批回调期间已被改动，请刷新后重试",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(req_id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::warn!(
                %req_id,
                code = error.code,
                msg = %error.msg,
                "采购申请审批结论回写失败"
            );
            Ok(None)
        }
    }
}

// ==================== 内控：制单人不能批自己的申请 ====================

/// 内控用：这张申请单的**制单人**（`biz_purchase_requisition.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(req_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_purchase_requisition WHERE id = ?::uuid",
            vec![Value::from(&req_id)],
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
/// 判据（任一命中即 403）：操作人 == 申请单的 `create_by`（制单人），
/// 或操作人 == 审批单的 `initiator_id`（发起人）。
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
    // ERP-B2：「制单与审批分离」开关（默认开 = 改动前的内控闸）
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(req_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_requisition(req_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的采购申请：申请单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.req_no,
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

/// 最近一条**驳回**意见（写进申请单 `remark`，让人看到被打回的理由）。
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

    /// ① 实例状态 → 业务语义（只有 APPROVED 才能转单）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(pr_verdict("APPROVED"), Some(PrVerdict::Approve));
        assert_eq!(pr_verdict(" approved "), Some(PrVerdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                pr_verdict(status),
                Some(PrVerdict::BackToDraft),
                "{status} 必须打回草稿（不是关闭、不是取消）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(pr_verdict(status), None, "{status} 不该推进申请单");
        }
    }

    /// ② 业务类型 / 流程编码必须与 V0331 种子同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed() {
        assert_eq!(BIZ_TYPE, "PR_APPROVE");
        assert_eq!(FLOW_CODE, "pr_approve");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
        assert_eq!(FLOW_CODE, FLOW_CODE.to_lowercase());
    }

    /// ③ 状态常量不能写错（错一个字就是库层 check 违约 / 前端徽标空白）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(STATUS_SUBMITTED, "SUBMITTED");
        assert_eq!(STATUS_APPROVED, "APPROVED");
        assert_eq!(STATUS_DRAFT, "DRAFT");
    }

    /// ④ 预算快照的「超预算 ⇔ 超出额 > 0」口径与库层 CHECK 逐字一致。
    #[test]
    fn budget_snapshot_is_consistent() {
        let zero = RequisitionBudget {
            mode: ControlMode::Off,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: "off".to_string(),
        };
        let snapshot = zero.snapshot();
        assert!(!snapshot.checked, "没判定 = checked=false");
        assert!(!snapshot.over_budget);
        assert_eq!(snapshot.over_amount, Decimal::ZERO);
        assert!(!zero.over() && !zero.blocks());
    }
}

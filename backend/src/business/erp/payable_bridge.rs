//! 应付付款审批的**业务回调 + 内控闸 + 反向兜底**（批次 2 · P17）。
//!
//! 与 `stockin_bridge`（收货单作废审批）/ `flow::task_bridge`（任务与调拨）并列的第四类业务回调：
//! 审批单落终态之后，把结论翻译成**付款记录 / 应付账面**的动作。三者互不影响（按 `biz_type` 分流）。
//!
//! # 四个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_payment_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → 执行真付款（应付回写 + 凭证）；其余终态 → 标 `REJECTED`，账一分不动 |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**进事务之前**） | 内控：审批人是这笔付款的**制单人**（或发起人）→ `403`「不能审批自己提交的付款申请」 |
//! | [`cancel_pending_of_payable`] | 应付单作废（`payable_payment::cancel_in_tx`）/ 收货单作废联动（`payable::cancel_for_stock_in`） | 把在跑的付款审批**终止**（实例 / 节点 / 待办一起 `CANCELLED`），付款记录标 `CANCELLED` |
//! | [`reconcile_orphan_payments`] | 定时任务 `payable_payment_reconcile`（每 10 分钟） | 扫描「审批还在跑、付款记录却已不是 `PENDING`」的**孤儿实例**，逐个终止 |
//!
//! # 为什么必须有反向（P11 的教训）
//!
//! P11 实测：业务单据被删/改状态时没有撤审批单 → 审批中心留下孤儿待办，点进去 404。
//! 付款这边更严重：孤儿实例会顶住 `uk_biz_payment_pending`（同一应付单只许一笔审批中），
//! 让这条应付单**永远付不出第二笔钱**（409 且无法自愈）。所以三条反向路径都做了：
//!
//! ```text
//! · 应付单作废（付款审批中）       → cancel_pending_of_payable（同事务）
//! · 收货单作废联动作废应付单       → payable::cancel_for_stock_in → cancel_pending_of_payable
//! · 付款记录被别处删/改状态        → on_payment_settled 的兜底 + reconcile 定时兜扫
//! ```
//!
//! # 时序与失败口径（与 `stockin_bridge::on_cancel_settled` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。定时扫（[`reconcile_orphan_payments`]）按「付款记录实际状态」重算，
//! 天然幂等。
//!
//! # 与 P7 的关系（硬红线 5：复用，不重写）
//!
//! 记账一行都不重写：直接调 [`crate::business::erp::payable_payment::settle_payment_in_tx`]，
//! 它内部就是 P7 的「凭证 借 2202 / 贷 1002 → 付款记录回写 voucher_id → 应付重算 + 状态机」。
//! 本模块只负责「什么时候该调它」。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::separation;
use crate::business::flow::{engine, model as flow_model, settle_guard};
use crate::shared::error_code;

/// 付款审批的业务类型（与 V0175 种子 / `payable_payment::PAY_BIZ_TYPE` 同口径）。
pub const BIZ_TYPE: &str = crate::business::erp::payable_payment::PAY_BIZ_TYPE;
/// 付款审批的流程编码。
pub const FLOW_CODE: &str = crate::business::erp::payable_payment::PAY_FLOW_CODE;

/// 定时任务名（`sys_job.invoke_target` 与调度名同值）。
pub const TASK_NAME: &str = "payable_payment_reconcile";
/// 定时任务 cron（每 10 分钟，与收货作废回收错开 4 分钟）。
pub const CRON: &str = "7-59/10 * * * *";

/// 一条「已发布的付款审批流程」（发起审批要用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`PAYABLE_PAY`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的应付付款流程」。
///
/// **未配流程是合法状态**：返回 `None`，调用方按 P7 老行为**降级直付**（卡面 ② 单测）。
/// 不返回错误、不 500 —— 流程是可选增强，不是付款的前置条件。
pub async fn published_pay_flow() -> DbResult<Option<PayFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'PAYABLE_PAY' OR lower(d.flow_code) = 'payable_pay') \
              ORDER BY (d.biz_type = 'PAYABLE_PAY') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PayFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 内控：制单人不能批自己的付款 ====================

/// 内控用：这笔付款的**制单人**（`biz_payment.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**排除制单人（落 `SKIPPED` 留痕）。
/// 查不到付款记录 → `None`（没人被排除，不锁流程）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(payment_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 付款台账无归属列（见 payable_payment 模块文档），按主键取 1 行。
    let row = db()
        .query_first_sql(
            "SELECT create_by FROM biz_payment WHERE id = ?::uuid".to_string(),
            vec![Value::from(&payment_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let creator: Option<Uuid> = row.get_typed("create_by")?;
    Ok(creator)
}

/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
///
/// # 判据（任一命中即 403）
///
/// 1. `operator` == `biz_payment.create_by`（**制单人**）；
/// 2. `operator` == 审批单的 `initiator_id`（发起人；代提场景下发起人 ≠ 制单人，两条都要拦）。
///
/// # 为什么不放在流程节点的 `self_approval=FORBID`
///
/// `FORBID` 在**发起时**就 `422 + 65019`（整单起不来），而内控要的是「提交成功、但**这一票**
/// 点不动」—— 发起被拦住等于付款流程根本用不了。所以流程节点写 `ALLOW`（待办照常可见、留痕），
/// 真正的否决在这里，报文明确告诉他「找别人批」。
///
/// 与本业务无关的待办（其它 `biz_type`）直接放行，零影响。
pub async fn guard_self_approval(
    task_id: Uuid,
    operator: Uuid,
) -> std::result::Result<(), ApiError> {
    // SCOPE-EXEMPT: 审批待办 / 实例是流程运行时表，按主键精确取 1 行；数据范围由
    // 「这条待办是不是你的」这一层判定（engine::vote 的 403）负责，与本闸无关。
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
    let biz_type = instance.biz_type.trim().to_ascii_uppercase();
    if biz_type == CANCEL_BIZ_TYPE {
        // ERP-B2：作废审批走「制单与审批分离」（默认开 = 改动前行为）
        if !separation::enabled(separation::Duty::Approve).await {
            return Ok(());
        }
        // P-FLOW-3B：应付单作废审批 —— 制单人是**应付单**的 `create_by`（不是付款记录）
        let Some(payable_id) = instance.biz_id else {
            return Ok(());
        };
        let Ok(Some(row)) = crate::business::erp::payable_query::find_payable(payable_id).await
        else {
            return Ok(());
        };
        let creator = internal_control_creator_of_payable(Some(payable_id))
            .await
            .map_err(|error| {
                ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!("查询应付单制单人失败：{error}"),
                )
            })?;
        if creator == Some(operator) || instance.initiator_id == operator {
            return Err(ApiError::new(
                403,
                error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
                format!(
                    "不能审批自己提交的应付作废申请：应付单 {} 由你本人提交（内控要求制单与审批分离），\
                     请交给其他有审批权限的账号处理{}",
                    row.payable_no,
                    separation::approval_hint(separation::Duty::Approve)
                ),
            ));
        }
        return Ok(());
    }
    if biz_type != BIZ_TYPE {
        return Ok(());
    }
    // ERP-B2：「制单与付款分离」开关（默认开 = P17 改动前的行为）
    if !separation::enabled(separation::Duty::Pay).await {
        return Ok(());
    }
    let Some(payment_id) = instance.biz_id else {
        return Ok(());
    };
    let Some((creator, payment_no)) = payment_creator(payment_id).await? else {
        return Ok(());
    };
    if creator == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的付款申请：付款单 {payment_no} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                separation::approval_hint(separation::Duty::Pay)
            ),
        ));
    }
    Ok(())
}

/// 取一笔付款的**制单人**与单号（`create_by` 为空的历史行 → `None`）。
async fn payment_creator(
    payment_id: Uuid,
) -> std::result::Result<Option<(Option<Uuid>, String)>, ApiError> {
    // SCOPE-EXEMPT: 付款台账无归属列（见 payable_payment 模块文档）。
    let row = db()
        .query_first_sql(
            "SELECT create_by, payment_no FROM biz_payment WHERE id = ?::uuid".to_string(),
            vec![Value::from(&payment_id)],
        )
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("查询付款记录失败：{error}"),
            )
        })?;
    let Some(row) = row else {
        return Ok(None);
    };
    let creator: Option<Uuid> = row.get_typed("create_by").ok().flatten();
    let payment_no: String = row
        .get_typed("payment_no")
        .unwrap_or_else(|_| "(未知单号)".to_string());
    Ok(Some((creator, payment_no)))
}

// ==================== 回调：审批结论 → 付款 ====================

/// 审批单落终态 → 执行 / 放弃这笔付款（`FlowCallback` 身份）。
///
/// | 实例状态 | 付款记录 | 账 |
/// |---|---|---|
/// | `APPROVED` | `PENDING → APPROVED` | **真付款**：`paid += amount`、`unpaid -= amount`、凭证 借 2202 / 贷 1002 |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | `PENDING → REJECTED` | 一分没动 |
/// | 其它（`PENDING` / 空） | 不动 | 不动 |
///
/// 返回受影响的付款记录 id（没推进 / 不该推进 → `None`）。**不返回错误**：审批已落库，
/// 业务侧失败只记日志（与 `task_bridge::on_transfer_settled` 同口径）。
pub async fn on_payment_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = settle_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(payment_id)) => {
            tracing::info!(
                %instance_id,
                %payment_id,
                verdict = verdict.as_str(),
                "付款审批结论已回调付款记录"
            );
            Some(payment_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "付款审批结论回调失败（不影响审批单，等待 reconcile 兜扫）"
            );
            None
        }
    }
}

/// 审批结论的**业务语义**（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 同意 → 真付款（记账）。
    Execute,
    /// 驳回 / 撤回 / 终止 → 付款记录标 `REJECTED`，账不动。
    Abort,
}

impl Verdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            Verdict::Execute => "EXECUTE",
            Verdict::Abort => "ABORT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动付款）。
pub fn settle_verdict(instance_status: &str) -> Option<Verdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(Verdict::Execute),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(Verdict::Abort),
        _ => None,
    }
}

/// [`on_payment_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: Verdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「付款」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过付款回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(payment_id) = instance.biz_id else {
        return Ok(None);
    };

    let approver = latest_completed_assignee(instance_id).await?;
    match verdict {
        Verdict::Execute => {
            use crate::business::erp::payable_payment::{self, SettleOutcome};
            match payable_payment::settle_approved_payment(payment_id, approver).await {
                Ok(SettleOutcome::Paid) => {
                    // 审批通过后重复回调（同一实例被点两次 approve）→ 幂等：账已经记过
                    Ok(Some(payment_id))
                }
                Ok(SettleOutcome::Cancelled { reason }) => {
                    tracing::warn!(
                        %payment_id,
                        %instance_id,
                        reason = %reason,
                        "付款审批已通过，但应付单已作废 → 拒绝执行，付款记录标 CANCELLED（账一分没动）"
                    );
                    notify_stakeholders(
                        instance.initiator_id,
                        instance_id,
                        instance.instance_no.as_str(),
                        &format!(
                            "付款 {} 审批已通过，但应付单已作废，付款未执行（已终止）",
                            instance.title
                        ),
                    )
                    .await;
                    Ok(Some(payment_id))
                }
                Ok(SettleOutcome::Rejected { reason }) => {
                    tracing::warn!(
                        %payment_id,
                        %instance_id,
                        reason = %reason,
                        "付款审批已通过，但执行条件不再成立 → 付款记录标 REJECTED（账一分没动）"
                    );
                    Ok(Some(payment_id))
                }
                Ok(SettleOutcome::AlreadySettled) => {
                    tracing::info!(%payment_id, "付款记录已是终态（重复回调），跳过");
                    Ok(None)
                }
                Err(error) => {
                    tracing::warn!(
                        %payment_id,
                        code = error.code,
                        msg = %error.msg,
                        "付款审批已通过，但记账失败（付款记录仍是审批中，等待人工处置 / 下轮兜扫）"
                    );
                    Ok(None)
                }
            }
        }
        Verdict::Abort => {
            use crate::business::erp::payable_payment;
            match payable_payment::abort_payment(payment_id, "审批驳回 / 撤回，付款未执行").await
            {
                Ok(true) => Ok(Some(payment_id)),
                Ok(false) => Ok(None),
                Err(error) => {
                    tracing::warn!(
                        %payment_id,
                        status = error.status,
                        code = error.code,
                        msg = %error.msg,
                        "付款审批未通过，但付款记录打回失败（等待兜扫）"
                    );
                    Ok(None)
                }
            }
        }
    }
}

/// 「审批通过但单据已作废」这类**必须让人知道**的结论：给发起人发一条强制通知。
///
/// 通知失败只记 WARN（与 `engine::flush_effects` 同口径），不影响审批结论。
async fn notify_stakeholders(initiator: Uuid, instance_id: Uuid, instance_no: &str, content: &str) {
    let notice = engine::Notice {
        recipients: vec![initiator],
        title: "付款未执行（应付单已作废）".to_string(),
        content: format!("审批单 {instance_no}：{content}"),
        link_url: "/business/erp/payable".to_string(),
        instance_id,
        force: true,
    };
    engine::flush_effects(vec![notice]).await;
}

/// 最近一条**已完成**待办的审批人（会签取最后一票）。读不到 → `None`。
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

// ==================== 反向：终止在跑的付款审批 ====================

/// 终止某张应付单**在跑的付款审批**，并把对应的付款记录标 `CANCELLED`（账一分不动）。
///
/// 幂等：没有在跑的实例返回 `Ok(0)`。返回终止的实例数。
///
/// 调用点（同事务）：
/// * [`crate::business::erp::payable_payment::cancel_in_tx`]（应付单作废）；
/// * [`crate::business::erp::payable::cancel_for_stock_in`]（收货单作废联动）。
pub async fn cancel_pending_of_payable(
    ex: &impl SqlExecutor,
    payable_id: Uuid,
    reason: &str,
) -> DbResult<u64> {
    // 先取在跑的付款记录（拿单号给 cancel_pending_of_biz 兜住 biz_id 为空的历史行）
    let pending = ex
        .query_sql(
            "SELECT id, payment_no FROM biz_payment \
              WHERE payable_id = ?::uuid AND approval_status = 'PENDING' AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&payable_id)],
        )
        .await?;
    let mut cancelled = 0u64;
    for row in &pending {
        let payment_id: Uuid = row.get_typed("id")?;
        let payment_no: String = row.get_typed("payment_no")?;
        let outcome =
            flow_model::cancel_pending_of_biz(ex, payment_id, &payment_no, reason).await?;
        // 付款记录同步标 CANCELLED（账没动过，所以不需要任何冲销）；
        // 审批实例 id 保留作审计（约束只要求 PENDING ⟹ 有实例）
        ex.exec_sql(
            "UPDATE biz_payment \
                SET approval_status = 'CANCELLED', updated_at = now() \
              WHERE id = ?::uuid AND approval_status = 'PENDING'"
                .to_string(),
            vec![Value::from(&payment_id)],
        )
        .await?;
        cancelled += outcome.instances.max(1);
        tracing::info!(
            %payable_id,
            %payment_id,
            payment_no = %payment_no,
            instances = outcome.instances,
            tasks = outcome.tasks,
            reason = reason,
            "付款审批已终止（账一分没动）"
        );
    }
    // 应付单上的弱引用一并摘掉（与「PENDING ⟺ 有引用」同生同灭）
    ex.exec_sql(
        "UPDATE biz_payable SET payment_approval_instance_id = NULL, updated_at = now() \
          WHERE id = ?::uuid AND payment_approval_instance_id IS NOT NULL"
            .to_string(),
        vec![Value::from(&payable_id)],
    )
    .await?;
    Ok(cancelled)
}

/// 终止**单笔付款**在跑的审批（孤儿回收 / 直调）。
pub async fn cancel_approval_of(
    ex: &impl SqlExecutor,
    payment_id: Uuid,
    payment_no: &str,
    reason: &str,
) -> DbResult<u64> {
    let outcome = flow_model::cancel_pending_of_biz(ex, payment_id, payment_no, reason).await?;
    if outcome.instances > 0 {
        tracing::info!(
            %payment_id,
            payment_no = %payment_no,
            instances = outcome.instances,
            tasks = outcome.tasks,
            reason = reason,
            "付款在跑的审批已终止"
        );
    }
    Ok(outcome.instances)
}

/// 扫描并终止**孤儿付款审批实例**（P11 教训的机器化兜底）。
///
/// 判据（三条同时成立才算孤儿）：
/// 1. 实例 `biz_type = 'PAYABLE_PAY'` 且 `status = 'PENDING'`（还在跑）；
/// 2. 付款记录**已软删**，或 `approval_status` 已**不是** `PENDING`（被别处改过 / 已执行）；
/// 3. `biz_id` 可解析（为空说明实例本身没挂业务，交给上游维护）。
///
/// 返回终止的实例数。定时任务每 10 分钟跑一次；单测 / 运维脚本也可直接调。
pub async fn reconcile_orphan_payments() -> DbResult<u64> {
    let rows = db()
        .find_with_params(
            "SELECT i.id AS instance_id, i.biz_id AS payment_id, i.biz_no AS biz_no, \
                    p.approval_status AS approval_status, p.deleted_at AS payment_deleted_at \
               FROM biz_flow_instance i \
               LEFT JOIN biz_payment p ON p.id = i.biz_id \
              WHERE i.biz_type = ? AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND i.biz_id IS NOT NULL \
                AND (p.id IS NULL OR p.deleted_at IS NOT NULL OR p.approval_status <> 'PENDING') \
              ORDER BY i.started_at ASC \
              LIMIT 200",
            vec![Value::Text(BIZ_TYPE.to_string())],
        )
        .await?;

    let mut cancelled = 0u64;
    for row in rows {
        let instance_id: Uuid = row.get_typed("instance_id")?;
        let payment_id: Uuid = row.get_typed("payment_id")?;
        let payment_no: Option<String> = row.get_typed("biz_no")?;
        let payment_no = payment_no.unwrap_or_else(|| "(无单号)".to_string());
        let deleted: Option<chrono::DateTime<chrono::Utc>> =
            row.get_typed("payment_deleted_at").ok().flatten();
        let reason = if deleted.is_some() {
            "付款记录已删除，付款审批自动终止"
        } else {
            "付款记录已不处于审批中，付款审批自动终止"
        };
        match cancel_approval_of(db(), payment_id, &payment_no, reason).await {
            Ok(count) if count > 0 => cancelled += count,
            Ok(_) => tracing::debug!(%instance_id, "孤儿实例判定后被别处处理，跳过"),
            Err(error) => tracing::warn!(%instance_id, %error, "终止孤儿付款审批失败（下轮重试）"),
        }
    }
    if cancelled > 0 {
        tracing::info!(cancelled, "孤儿付款审批实例回收完成");
    }
    Ok(cancelled)
}

/// 在调度器上注册孤儿实例回收任务（与 `stockin_bridge::register_builtin` 同款）。
pub fn register_builtin(scheduler: &crate::task::Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        CRON,
        "系统任务：付款审批孤儿实例回收",
        || {
            tokio::spawn(async move {
                match reconcile_orphan_payments().await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!(count, "付款审批孤儿实例回收完成"),
                    Err(error) => tracing::error!(%error, "付款审批孤儿实例回收失败"),
                }
            })
        },
    );
}

/// 让引擎的 `ActionOut` 能直接喂给 [`on_payment_settled`]（只取「落终态」这一层语义）。
pub async fn on_action_settled(out: &engine::ActionOut) -> Option<Uuid> {
    on_payment_settled(out.instance_id, &out.instance_status).await
}

// ==================== P-FLOW-3B：应付单作废审批 ====================

/// 作废审批的业务类型（与 V0191 种子 / [`crate::business::erp::payable::CANCEL_BIZ_TYPE`] 同口径）。
pub const CANCEL_BIZ_TYPE: &str = crate::business::erp::payable::CANCEL_BIZ_TYPE;
/// 作废审批的流程编码。
pub const CANCEL_FLOW_CODE: &str = crate::business::erp::payable::CANCEL_FLOW_CODE;
/// 作废审批孤儿实例回收的定时任务名。
pub const CANCEL_TASK_NAME: &str = "payable_cancel_reconcile";
/// 作废审批孤儿实例回收的 cron（与付款回收 7-59/10、收货回收 3-59/10 错开）。
pub const CANCEL_CRON: &str = "9-59/10 * * * *";

/// 查「已发布的应付单作废审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按 P-FLOW-3B 之前的「直接作废」降级，
/// 并打一条 `warn`，绝不因为没配流程就把作废锁死。
///
/// 排序口径与 [`published_pay_flow`] / `stockin_model::published_post_flow` 完全一致
/// （`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间）。
pub async fn published_cancel_flow() -> DbResult<Option<PayFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'PAYABLE_CANCEL' OR lower(d.flow_code) = 'payable_cancel') \
              ORDER BY (d.biz_type = 'PAYABLE_CANCEL') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(PayFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 作废审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelVerdict {
    /// 同意 → 真作废（欠款消失、进项凭证红冲）。
    Cancel,
    /// 驳回 / 撤回 / 终止 → 回到 `OPEN`，账一分没动。
    Restore,
}

impl CancelVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            CancelVerdict::Cancel => "CANCEL",
            CancelVerdict::Restore => "RESTORE",
        }
    }
}

/// 实例状态 → 作废审批的业务语义。`None` = 还在跑 / 不认识（一律不动单据）。
pub fn cancel_verdict(instance_status: &str) -> Option<CancelVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(CancelVerdict::Cancel),
        // 驳回、发起人撤回、被系统终止 → 都是「作废没成」
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(CancelVerdict::Restore),
        _ => None,
    }
}

/// 作废审批单落终态 → 执行 / 撤销应付单作废（`FlowCallback` 身份）。
///
/// | 实例状态 | 应付单 | 账 |
/// |---|---|---|
/// | `APPROVED` | `PENDING_CANCEL → CANCELLED` | **真作废**：进项凭证红冲 + 欠款消失 |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | `PENDING_CANCEL → OPEN` | 一分没动 |
/// | 其它（`PENDING` / 空） | 不动 | 不动 |
///
/// 返回受影响的应付单 id（没推进 / 不该推进 → `None`）。**不返回错误**：审批已落库，
/// 业务侧失败只记日志（与 [`on_payment_settled`] 同口径，下一轮 reconcile 会兜）。
pub async fn on_cancel_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = cancel_verdict(instance_status)?;
    match settle_cancel(instance_id, verdict).await {
        Ok(Some(payable_id)) => {
            tracing::info!(
                %instance_id,
                %payable_id,
                verdict = verdict.as_str(),
                "应付作废审批结论已回调应付单"
            );
            Some(payable_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "应付作废审批结论回调失败（不影响审批单，单据仍是作废审批中）"
            );
            None
        }
    }
}

/// [`on_cancel_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle_cancel(instance_id: Uuid, verdict: CancelVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「应付单作废」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过应付作废回调");
        return Ok(None);
    };
    if instance.biz_type != CANCEL_BIZ_TYPE {
        return Ok(None);
    }
    let Some(payable_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 单据必须存在（当前状态能不能收这条结论交给 ③ 的统一前置校验）
    let Some(row) = crate::business::erp::payable_query::find_payable(payable_id).await? else {
        tracing::warn!(%payable_id, "应付单不存在或已删除，跳过作废回调");
        return Ok(None);
    };
    // ★ C1（t_1e174431）：写单据状态前先校验应付单还在不在「能接受这个审批结论」的合法路径上
    //   （状态机边 `PENDING_CANCEL → CANCELLED|OPEN`）。已作废 / 已被别的路径推走
    //   → 不落状态 + WARN + 审批实例留痕（审批单本身仍按自己的结论结束）。
    let target = match verdict {
        CancelVerdict::Cancel => crate::business::erp::payable::STATUS_CANCELLED,
        CancelVerdict::Restore => crate::business::erp::payable::STATUS_OPEN,
    };
    if !settle_guard::gate(
        &instance,
        crate::business::shared::state_machine::Entity::Payable,
        &row.payable_no,
        &row.status,
        crate::business::erp::payable::STATUS_PENDING_CANCEL,
        target,
    )
    .await
    {
        return Ok(None);
    }
    // ③ 审批人（落 `update_by` / 状态日志用）：引擎把「谁点的」写在待办行
    let approver = latest_completed_assignee(instance_id).await?;
    let creator = internal_control_creator_of_payable(Some(payable_id)).await?;
    match verdict {
        CancelVerdict::Cancel => {
            // 内控最后一道闸：**复核人不能是制单人**（节点 self_approval 已拦 + 生成待办前
            // 已排除，这里再核一次：真发生了就**不许动账**，留 WARN 让人看）
            if let (Some(approver), Some(creator)) = (approver, creator)
                && approver == creator
            {
                tracing::warn!(
                    %payable_id,
                    instance_id = %instance_id,
                    approver = %approver,
                    "内控拦截：作废复核人就是制单人本人，应付单**未**作废（请核查流程配置）"
                );
                return Ok(None);
            }
            match crate::business::erp::payable_payment::cancel_after_approval(payable_id, approver)
                .await
            {
                Ok(()) => Ok(Some(payable_id)),
                Err(error) => {
                    tracing::warn!(
                        %payable_id,
                        code = error.code,
                        msg = %error.msg,
                        "作废审批已通过，但作废执行失败（单据仍是作废审批中，等待人工处置）"
                    );
                    Ok(None)
                }
            }
        }
        CancelVerdict::Restore => {
            match crate::business::erp::payable_payment::restore_after_cancel_rejection(
                payable_id, verdict, approver,
            )
            .await
            {
                Ok(()) => Ok(Some(payable_id)),
                Err(error) => {
                    tracing::warn!(
                        %payable_id,
                        code = error.code,
                        msg = %error.msg,
                        "作废审批未通过，但应付单回位失败（等待 reconcile 兜扫）"
                    );
                    Ok(None)
                }
            }
        }
    }
}

/// 内控用：这张**应付单**的制单人（`biz_payable.create_by`）。
///
/// 与 [`internal_control_creator`]（按 `biz_payment.id` 取付款制单人）并列：
/// 作废审批的 `biz_id` 是**应付单 id**，两者不能混用（付款记录的 `create_by` 是财务，
/// 应付单的 `create_by` 是入库过账人，排除错人等于没排除）。
pub async fn internal_control_creator_of_payable(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(payable_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪（裁剪会让
    // 「看不到这张单的人」反而成为可审批人，与内控目的相反）。
    let row = db()
        .query_first_sql(
            "SELECT create_by FROM biz_payable WHERE id = ?::uuid".to_string(),
            vec![Value::from(&payable_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let creator: Option<Uuid> = row.get_typed("create_by")?;
    Ok(creator)
}

/// 终止某张应付单**在跑的作废审批**（单据被别处作废 / 删除 / 联动路径用，与调用方同一事务）。
///
/// 幂等：没有在跑的实例返回 `Ok(0)`。返回终止的实例数。
///
/// 同时把 `cancel_instance_id` 摘掉 —— 库侧 `ck_biz_payable_pending_cancel`
/// 要求「非 PENDING_CANCEL 状态 ⟹ 没有实例引用」，不摘就会 23514。
pub async fn cancel_approval_of_payable_cancel(
    ex: &impl SqlExecutor,
    payable_id: Uuid,
    payable_no: &str,
    reason: &str,
) -> DbResult<u64> {
    let outcome = flow_model::cancel_pending_of_biz(ex, payable_id, payable_no, reason).await?;
    ex.exec_sql(
        "UPDATE biz_payable SET cancel_instance_id = NULL, updated_at = now() \
          WHERE id = ?::uuid AND status <> 'PENDING_CANCEL' AND cancel_instance_id IS NOT NULL"
            .to_string(),
        vec![Value::from(&payable_id)],
    )
    .await?;
    if outcome.instances > 0 {
        tracing::info!(
            %payable_id,
            payable_no = %payable_no,
            instances = outcome.instances,
            tasks = outcome.tasks,
            reason = reason,
            "应付作废审批已终止（欠款没动）"
        );
    }
    Ok(outcome.instances)
}

/// 扫描并终止**孤儿作废审批实例**（P11 教训的机器化兜底）。
///
/// 判据（三条同时成立才算孤儿）：
/// 1. 实例 `biz_type = 'PAYABLE_CANCEL'` 且 `status = 'PENDING'`（还在跑）；
/// 2. 应付单**已软删**，或状态已**不是** `PENDING_CANCEL`；
/// 3. `biz_id` 可解析（为空说明实例本身没挂业务，交给上游维护）。
///
/// 返回终止的实例数。定时任务每 10 分钟跑一次；单测 / 运维脚本也可直接调。
pub async fn reconcile_orphan_payable_cancels() -> DbResult<u64> {
    let rows = db()
        .find_with_params(
            "SELECT i.id AS instance_id, i.biz_id AS payable_id, i.biz_no AS biz_no, \
                    p.status AS payable_status, p.deleted_at AS payable_deleted_at \
               FROM biz_flow_instance i \
               LEFT JOIN biz_payable p ON p.id = i.biz_id \
              WHERE i.biz_type = ? AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND i.biz_id IS NOT NULL \
                AND (p.id IS NULL OR p.deleted_at IS NOT NULL OR p.status <> ?) \
              ORDER BY i.started_at ASC \
              LIMIT 200",
            vec![
                Value::Text(CANCEL_BIZ_TYPE.to_string()),
                Value::Text(crate::business::erp::payable::STATUS_PENDING_CANCEL.to_string()),
            ],
        )
        .await?;

    let mut cancelled = 0u64;
    for row in rows {
        let instance_id: Uuid = row.get_typed("instance_id")?;
        let payable_id: Uuid = row.get_typed("payable_id")?;
        let payable_no: Option<String> = row.get_typed("biz_no")?;
        let payable_no = payable_no.unwrap_or_else(|| "(无单号)".to_string());
        let deleted: Option<chrono::DateTime<chrono::Utc>> =
            row.get_typed("payable_deleted_at").ok().flatten();
        let reason = if deleted.is_some() {
            "应付单已删除，作废审批自动终止"
        } else {
            "应付单已不处于作废审批中，作废审批自动终止"
        };
        match cancel_approval_of_payable_cancel(db(), payable_id, &payable_no, reason).await {
            Ok(count) if count > 0 => cancelled += count,
            Ok(_) => tracing::debug!(%instance_id, "孤儿实例判定后被别处处理，跳过"),
            Err(error) => tracing::warn!(%instance_id, %error, "终止孤儿作废审批失败（下轮重试）"),
        }
    }
    if cancelled > 0 {
        tracing::info!(cancelled, "孤儿应付作废审批实例回收完成");
    }
    Ok(cancelled)
}

/// 在调度器上注册「应付作废审批孤儿实例回收」（与 [`register_builtin`] 并列）。
pub fn register_cancel_builtin(scheduler: &crate::task::Scheduler) {
    scheduler.register_cron(
        CANCEL_TASK_NAME,
        CANCEL_CRON,
        "系统任务：应付作废审批孤儿实例回收",
        || {
            tokio::spawn(async move {
                match reconcile_orphan_payable_cancels().await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!(count, "应付作废审批孤儿实例回收完成"),
                    Err(error) => tracing::error!(%error, "应付作废审批孤儿实例回收失败"),
                }
            })
        },
    );
}

// ==================== 单测（P17：≥16 条，真库） ====================
//
// 真库用例（需要 `ERP_DB_URL` 指向「已应用 V0175 的隔离库」，缺省则全部 SKIP）：
//
// ```bash
// export ERP_DB_URL='postgres://p17role:***@127.0.0.1:5432/cloudpivot_p17'
// cargo test --lib p17_ -- --test-threads=1
// ```
//
// 夹具全部带 `P17` 前缀（应付单号 `AP-P17-…`、用户 `p17_*`），每次 `setup()` 先按前缀清掉上一轮。
// ⚠️ 本模块与 `payable_payment::tests`（P7）**共用一把锁**（都写同一批表：biz_payment / biz_voucher），
//    所以这里的 `DB_LOCK` 与 P7 的是两个不同的静态量 —— 靠 `--test-threads=1` 串行跑，
//    并且夹具前缀 / 外键链完全不重叠（`AP-P17-` vs `AP-P7-`）。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::approval_gate;
    use crate::business::erp::fixture_hygiene;
    use crate::business::erp::payable::{self as payable_mod, PayableRow};
    use crate::business::erp::payable_payment::{
        self as payment, APPROVAL_APPROVED, APPROVAL_CANCELLED, APPROVAL_PENDING,
        APPROVAL_REJECTED, OUTCOME_PAID, OUTCOME_PENDING, PayPayableReq,
    };
    use crate::business::erp::payable_query;
    use crate::business::erp::voucher::{self, entries_of, find_voucher};
    use crate::business::flow::graph;
    use nik::prelude::chrono::Duration;
    use nik::prelude::chrono::Utc;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    /// ARCH-3 DC-01：系统路径的审批前置上下文（不请求跳过、无权限声明）。
    ///
    /// 本模块的用例都在「种子流程已发布」的前提下跑（走审批）；把流程停掉的用例
    /// 现在拿到的是 **409 + 66033**（新的默认拒绝行为）。
    fn none_gate() -> crate::business::erp::approval_gate::ApprovalGate<'static> {
        crate::business::erp::approval_gate::ApprovalGate::none()
    }

    /// ARCH-3 DC-01：一个**持有** `business:erp:skip-approval` 的 claims（显式跳过审批要用）。
    ///
    /// 真实环境里这个码由管理员授予；这里直接造 claims，等价于「授权已生效」。
    fn skip_claims() -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: uid(CREATOR).to_string(),
            username: Some("p17_skip_case".to_string()),
            roles: Vec::new(),
            permissions: vec![crate::business::erp::approval_gate::PERM_SKIP_APPROVAL.to_string()],
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    /// 夹具供应商（合法 UUID）。
    const SUPPLIER_ID: &str = "e7e70001-0000-4000-8000-00000000f170";
    /// 制单人（**不是**超管：审批人来源是 ADMIN，他本来就不该出现在审批人里，
    /// 「正常付款能走完」的用例都用他提交）。
    const CREATOR: &str = "e7e70001-0000-4000-8000-0000000c0170";
    /// 复核人 1（超管）。
    const APPROVER1: &str = "e7e70001-0000-4000-8000-0000000a0171";
    /// 复核人 2（超管）。
    const APPROVER2: &str = "e7e70001-0000-4000-8000-0000000a0172";

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `biz_seq`）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 是独立 runtime，`db()` 是进程级全局池：
                // 把池子寿命压到 0，避免借到上一个 runtime 的死连接。
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
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn exec(sql: &str) {
        if let Err(err) = db().execute_with_params(sql, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    fn uid(text: &str) -> Uuid {
        Uuid::parse_str(text).expect("夹具 UUID 非法")
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。
    /// 清掉本模块夹具的**全部痕迹**（审批实例 / 通知 / 状态日志 / 凭证 / 付款 / 应付 / 夹具用户）。
    ///
    /// 两处调用：每个用例开头的 `setup()`，以及模块里名字排最后
    /// （`zz_` 之后）的 `zz_teardown_purges_payment_status_logs`。
    ///
    /// ⚠️ 状态日志必须**在删单据之前**按 id 清：`biz_status_log` 没有外键（`fixture_hygiene`）。
    async fn cleanup_fixtures() {
        let cleanups = [
            // 制单人不能是超管（上一轮旧夹具可能给他挂过角色）
            "DELETE FROM sys_user_role WHERE user_id = (SELECT id FROM sys_user WHERE username = 'p17_creator')"
                .to_string(),
            // 审批实例（子表 ON DELETE CASCADE；先删实例再删付款）
            "DELETE FROM biz_flow_instance WHERE biz_type = 'PAYABLE_PAY' AND biz_id IN \
             (SELECT id FROM biz_payment WHERE payable_id IN \
              (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P17-%'))"
                .to_string(),
            "DELETE FROM sys_notice WHERE biz_type = 'flow' AND biz_id IN \
             (SELECT i.id::text FROM biz_flow_instance i WHERE i.biz_type = 'PAYABLE_PAY')"
                .to_string(),
            // 凭证 / 分录：只删本夹具外键链上的行（`PM-` 是生产号段，绝不能按它模糊删）
            // 状态日志（无外键）：必须在删凭证 / 应付单之前按 id 清（`fixture_hygiene`）
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_id IN \
              (SELECT id FROM biz_payment WHERE payable_id IN \
               (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P17-%')))"
                .to_string(),
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_id IN \
              (SELECT id FROM biz_payment WHERE payable_id IN \
               (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P17-%')))"
                .to_string(),
            "DELETE FROM biz_voucher WHERE source_id IN \
             (SELECT id FROM biz_payment WHERE payable_id IN \
              (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P17-%'))"
                .to_string(),
            "DELETE FROM biz_status_log WHERE entity_type = 'PAYABLE' AND entity_id IN \
             (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P17-%')"
                .to_string(),
            "DELETE FROM biz_payment WHERE payable_id IN \
             (SELECT id FROM biz_payable WHERE payable_no LIKE 'AP-P17-%')"
                .to_string(),
            "DELETE FROM biz_payable WHERE payable_no LIKE 'AP-P17-%'".to_string(),
            // ⚠️ P17 的审批人夹具是**超管**，而「审批人 = 全部超管」：留着会污染兄弟模块
            // 的用例（实测 transfer_handler 的「1 条待办」断言被算成 3 条）。
            // 删之前必须把**指向这些人的流程行**清掉（`biz_flow_task.assignee_id` 是硬外键）：
            //   ① 先解开自引用（prev_instance_id 无级联）→ ② 删他们发起的实例（子表级联）
            //   → ③ 删残留待办 → ④ 删角色关联 → ⑤ 删用户
            "UPDATE biz_flow_instance SET prev_instance_id = NULL \
              WHERE initiator_id IN (SELECT id FROM sys_user \
                                      WHERE username IN ('p17_creator','p17_ap1','p17_ap2'))"
                .to_string(),
            "DELETE FROM biz_flow_instance \
              WHERE initiator_id IN (SELECT id FROM sys_user \
                                      WHERE username IN ('p17_creator','p17_ap1','p17_ap2'))"
                .to_string(),
            "DELETE FROM biz_flow_task \
              WHERE assignee_id IN (SELECT id FROM sys_user \
                                     WHERE username IN ('p17_creator','p17_ap1','p17_ap2'))"
                .to_string(),
            "DELETE FROM biz_flow_cc \
              WHERE receiver_id IN (SELECT id FROM sys_user \
                                     WHERE username IN ('p17_creator','p17_ap1','p17_ap2'))"
                .to_string(),
            "DELETE FROM sys_user_role WHERE user_id IN \
             (SELECT id FROM sys_user WHERE username IN ('p17_creator','p17_ap1','p17_ap2'))"
                .to_string(),
            "DELETE FROM sys_user WHERE username IN ('p17_creator','p17_ap1','p17_ap2')".to_string(),
        ];
        for sql in cleanups {
            exec(&sql).await;
        }
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup_fixtures().await;
        exec(&format!(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status, credit_days) \
             VALUES ('{SUPPLIER_ID}'::uuid, 'P17-SUP', 'P17 付款审批测试供应商', 1, 30) \
             ON CONFLICT DO NOTHING"
        ))
        .await;
        // 审批人来源是 ADMIN（运行时展开全部超管）：只有两个复核人挂 SUPER_ADMIN；
        // 制单人**故意不挂** —— 否则他自己那一票批不动（内控），ALL 会签永远凑不齐，
        // 「正常付款能走完」的用例就没法独立验证了（那条内控由 ⑰ 单独钉住）。
        let role: String = db()
            .scalar_with_params(
                "SELECT id::text FROM sys_role WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL LIMIT 1",
                vec![],
            )
            .await
            .expect("查询 SUPER_ADMIN 角色失败");
        for (id, name) in [
            (CREATOR, "p17_creator"),
            (APPROVER1, "p17_ap1"),
            (APPROVER2, "p17_ap2"),
        ] {
            exec(&format!(
                "INSERT INTO sys_user (id, username, password, real_name) \
                 VALUES ('{id}'::uuid, '{name}', 'p17-not-a-real-hash', 'P17 {name}') \
                 ON CONFLICT DO NOTHING"
            ))
            .await;
            if id == CREATOR {
                continue;
            }
            exec(&format!(
                "INSERT INTO sys_user_role (user_id, role_id) VALUES ('{id}'::uuid, '{role}'::uuid) \
                 ON CONFLICT DO NOTHING"
            ))
            .await;
        }
        // 会计期间必须存在且 OPEN（付款凭证记在「今天（CST）」）
        let today = voucher::today_cst(Utc::now());
        let period = format!("{}", today.format("%Y-%m"));
        exec(&format!(
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                                                start_date, end_date, status) \
             VALUES ({}, {}, '{period}', DATE '{}-01', (DATE '{}-01' + INTERVAL '1 month - 1 day')::date, 'OPEN') \
             ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'",
            today.format("%Y"),
            today.format("%m"),
            today.format("%Y-%m"),
            today.format("%Y-%m"),
        ))
        .await;
        exec("UPDATE biz_account SET status = 1 WHERE account_code IN ('2202', '1002')").await;
        // 流程必须是 PUBLISHED（前一个用例可能临时关过）
        exec("UPDATE biz_flow_def SET status = 'PUBLISHED', deleted_at = NULL WHERE flow_code = 'payable_pay'")
            .await;
        Some(guard)
    }

    /// 造一张应付单（直接落库，绕过收货流程）。
    async fn new_payable(amount: Decimal) -> Uuid {
        // ⚠️ 取 uuid 的**尾部** 8 位：v7 的高位是毫秒时间戳，同一毫秒内造两次会撞号
        let simple = Uuid::now_v7().simple().to_string();
        let no = format!("AP-P17-{}", &simple[simple.len() - 8..]);
        let due = voucher::today_cst(Utc::now()) + Duration::days(30);
        let period = format!("{}", voucher::today_cst(Utc::now()).format("%Y-%m"));
        let row = db()
            .query_first_sql(
                "INSERT INTO biz_payable (payable_no, supplier_id, source_type, source_id, source_no, \
                                          currency_code, amount, paid_amount, unpaid_amount, credit_days, \
                                          due_date, period_code, status) \
                 VALUES (?, ?::uuid, 'STOCK_IN', gen_random_uuid(), ?, 'CNY', \
                         NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric, 0, \
                         NULLIF(?::text,'')::date, ?, 'OPEN') \
                 RETURNING id"
                    .to_string(),
                vec![
                    Value::Text(no),
                    Value::from(&uid(SUPPLIER_ID)),
                    Value::Text("P17-IN".to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(amount.to_string()),
                    Value::Text(due.to_string()),
                    Value::Text(period),
                ],
            )
            .await
            .expect("造应付单失败");
        row.expect("INSERT 没有返回 id")
            .get_typed("id")
            .expect("读取应付单 id 失败")
    }

    fn pay_req(amount: &str) -> PayPayableReq {
        PayPayableReq {
            amount: Some(dec(amount)),
            pay_date: None,
            pay_method: Some("银行转账".to_string()),
            remark: Some("P17 单测".to_string()),
            ..Default::default()
        }
    }

    async fn payable_row(id: Uuid) -> PayableRow {
        let row = db()
            .query_first_sql(
                format!(
                    "SELECT {} FROM biz_payable WHERE id = ?::uuid",
                    payable_mod::COLUMNS
                ),
                vec![Value::from(&id)],
            )
            .await
            .expect("查应付单失败");
        let row = row.expect("应付单不存在");
        PayableRow {
            id: row.get_typed("id").unwrap(),
            payable_no: row.get_typed("payable_no").unwrap(),
            supplier_id: row.get_typed("supplier_id").unwrap(),
            source_type: row.get_typed("source_type").unwrap(),
            source_id: row.get_typed("source_id").unwrap(),
            source_no: row.get_typed("source_no").unwrap(),
            invoice_no: row.get_typed("invoice_no").unwrap(),
            invoice_date: row.get_typed("invoice_date").unwrap(),
            currency_code: row.get_typed("currency_code").unwrap(),
            amount: row.get_typed("amount").unwrap(),
            paid_amount: row.get_typed("paid_amount").unwrap(),
            unpaid_amount: row.get_typed("unpaid_amount").unwrap(),
            credit_days: row.get_typed("credit_days").unwrap(),
            due_date: row.get_typed("due_date").unwrap(),
            period_code: row.get_typed("period_code").unwrap(),
            status: row.get_typed("status").unwrap(),
            settled_at: row.get_typed("settled_at").unwrap(),
            voucher_id: row.get_typed("voucher_id").unwrap(),
            remark: row.get_typed("remark").unwrap(),
            cancel_instance_id: row.get_typed("cancel_instance_id").unwrap(),
            cancel_reason: row.get_typed("cancel_reason").unwrap(),
            created_at: row.get_typed("created_at").unwrap(),
            updated_at: row.get_typed("updated_at").unwrap(),
        }
    }

    /// 某付款记录的最新行（含审批状态）。
    async fn payment_row(payment_id: Uuid) -> payment::PaymentRow {
        let rows = db()
            .query_first_sql(
                "SELECT id, payment_no, payable_id, amount, pay_date, pay_method, voucher_id, \
                        remark, created_at, approval_status, approval_instance_id \
                   FROM biz_payment WHERE id = ?::uuid"
                    .to_string(),
                vec![Value::from(&payment_id)],
            )
            .await
            .expect("查付款记录失败");
        let row = rows.expect("付款记录不存在");
        payment::PaymentRow {
            id: row.get_typed("id").unwrap(),
            payment_no: row.get_typed("payment_no").unwrap(),
            payable_id: row.get_typed("payable_id").unwrap(),
            amount: row.get_typed("amount").unwrap(),
            pay_date: row.get_typed("pay_date").unwrap(),
            pay_method: row.get_typed("pay_method").unwrap(),
            voucher_id: row.get_typed("voucher_id").unwrap(),
            remark: row.get_typed("remark").unwrap(),
            created_at: row.get_typed("created_at").unwrap(),
            approval_status: row.get_typed("approval_status").unwrap(),
            approval_instance_id: row.get_typed("approval_instance_id").unwrap(),
        }
    }

    /// 某实例下的待办（`task_id`, `assignee`, `node_key`），只取 PENDING。
    async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid, String)> {
        let rows = db()
            .find_with_params(
                "SELECT id, assignee_id, node_key FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY node_key, assignee_id",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查待办失败");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed("id").unwrap(),
                    row.get_typed("assignee_id").unwrap(),
                    row.get_typed("node_key").unwrap(),
                )
            })
            .collect()
    }

    /// 某人在本实例上的待办（**不限状态**）：`(task_id, status, comment)`。
    ///
    /// P0-FIX 之后「制单人 / 发起人」不再拿 `PENDING`（生成时就被排除成 `SKIPPED`），
    /// 内控用例必须按「人」取行。
    async fn task_of(instance_id: Uuid, assignee: Uuid) -> Option<(Uuid, String, String)> {
        let rows = db()
            .find_with_params(
                "SELECT id, status, COALESCE(comment,'') AS comment FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND assignee_id = ?::uuid ORDER BY created_at LIMIT 1",
                vec![Value::from(&instance_id), Value::from(&assignee)],
            )
            .await
            .expect("查待办失败");
        rows.first().map(|row| {
            (
                row.get_typed("id").unwrap(),
                row.get_typed("status").unwrap(),
                row.get_typed("comment").unwrap(),
            )
        })
    }

    /// 审批一条待办，并在**事务提交之后**跑业务回调（与 `flow::handler::approve_task` 同序）。
    async fn approve(task_id: Uuid, operator: Uuid) -> String {
        let out = db()
            .transaction(|tx| async move {
                engine::approve(&tx, task_id, operator, Some("P17 同意")).await
            })
            .await
            .expect("审批失败");
        if let Some(rejected) = out.rejected {
            panic!("审批被拒：{} {}", rejected.code, rejected.msg);
        }
        let status = out.instance_status.clone();
        on_payment_settled(out.instance_id, &status).await;
        status
    }

    /// 驳回并终止（`REJECTED`），同样在提交后跑回调。
    async fn reject(task_id: Uuid, operator: Uuid) -> String {
        let out = db()
            .transaction(|tx| async move {
                engine::reject(
                    &tx,
                    task_id,
                    operator,
                    "P17 金额存疑，驳回",
                    engine::ReturnTo::Final,
                )
                .await
            })
            .await
            .expect("驳回失败");
        if let Some(rejected) = out.rejected {
            panic!("驳回被拒：{} {}", rejected.code, rejected.msg);
        }
        let status = out.instance_status.clone();
        on_payment_settled(out.instance_id, &status).await;
        status
    }

    /// 提交付款（制单人 = CREATOR），返回 `(付款 id, 实例 id, 出参)`。
    async fn submit(payable_id: Uuid, amount: &str) -> (Uuid, Option<Uuid>, payment::PaySubmitRes) {
        let outcome = payment::submit_payment(
            payable_id,
            &pay_req(amount),
            Some(uid(CREATOR)),
            &none_gate(),
        )
        .await
        .unwrap_or_else(|err| panic!("提交付款失败：{} {}", err.code, err.msg));
        let payment_id =
            Uuid::parse_str(outcome.payment_id.as_deref().expect("付款 id 缺失")).unwrap();
        let instance_id = outcome
            .approval_instance_id
            .as_deref()
            .map(|text| Uuid::parse_str(text).unwrap());
        (payment_id, instance_id, outcome)
    }

    // ---------- ① 发起：PENDING 付款 + 审批实例 ----------

    /// ① 付款提交 → 创建 `PENDING` 的付款记录 + 生成审批实例 + **账一分没动**。
    #[tokio::test]
    async fn p17_submit_creates_pending_payment_and_instance() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (payment_id, instance_id, outcome) = submit(payable_id, "5000").await;

        assert_eq!(outcome.outcome, OUTCOME_PENDING, "有流程时必须走审批");
        let instance_id = instance_id.expect("必须生成审批实例");
        let row = payment_row(payment_id).await;
        assert_eq!(row.approval_status, APPROVAL_PENDING);
        assert_eq!(row.approval_instance_id, Some(instance_id));
        assert!(row.voucher_id.is_none(), "审批中不能出凭证");
        assert_eq!(row.payment_no.chars().take(3).collect::<String>(), "PM-");

        // 应付账面一分没动
        let payable = payable_row(payable_id).await;
        assert_eq!(payable.paid_amount, Decimal::ZERO);
        assert_eq!(payable.unpaid_amount, dec("5000"));
        assert_eq!(payable.status, payable_mod::STATUS_OPEN);

        // 实例口径
        let instance = flow_model::find_instance(instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance.biz_type, BIZ_TYPE);
        assert_eq!(instance.biz_id, Some(payment_id));
        assert_eq!(instance.status, "PENDING");
        assert_eq!(instance.initiator_id, uid(CREATOR));
        assert!(
            !pending_tasks(instance_id).await.is_empty(),
            "必须有人有待办"
        );
    }

    // ---------- ② 未配流程 → 409（ARCH-3 DC-01：不再降级直付） ----------

    /// **DC-01 ④**：没有已发布的 `payable_pay` 流程 → **409 + 66033**。
    ///
    /// 改造前这里会**降级直付**（付款记录 + 凭证 + 应付结清一起落地，只留一条 `warn`）。
    /// 现在必须拒绝，且**一分钱都不许动**（应付仍 OPEN、没有付款记录、没有凭证）。
    #[tokio::test]
    async fn dc01_pay_without_published_flow_is_409() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        // 临时关掉流程（模拟「客户还没配审批」）
        exec("UPDATE biz_flow_def SET status = 'DISABLED' WHERE flow_code = 'payable_pay'").await;
        let result = payment::submit_payment(
            payable_id,
            &pay_req("5000"),
            Some(uid(CREATOR)),
            &none_gate(),
        )
        .await;
        // 先还原再断言（panic 也不会把流程留在 DISABLED 状态里）
        exec("UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'payable_pay'").await;

        let error = result.expect_err("没配流程必须 409，不能静默降级直付");
        assert_eq!(error.status, 409, "{}", error.msg);
        assert_eq!(
            error.code,
            error_code::APPROVAL_FLOW_NOT_CONFIGURED,
            "必须用 DC-01 的新码 66033：{}",
            error.msg
        );
        assert!(
            error.msg.contains("payable_pay"),
            "报文必须点名缺哪个 flow_code：{}",
            error.msg
        );
        // 一分钱都没动
        let payable = payable_row(payable_id).await;
        assert_eq!(payable.paid_amount, dec("0"), "被拒后不许记已付");
        assert_eq!(payable.status, payable_mod::STATUS_OPEN, "被拒后仍是 OPEN");
        let payments: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_payment WHERE payable_id = ?::uuid",
                vec![Value::from(&payable_id)],
            )
            .await
            .expect("查付款记录")
            .unwrap_or(0);
        assert_eq!(payments, 0, "被拒后不许落付款记录（更不许出凭证）");
    }

    // ---------- ③ 重复提交 → 409 ----------

    /// ③ 审批中重复提交 → 409，报文点名「在审批中（PM-xxx，金额 X）」。
    #[tokio::test]
    async fn p17_duplicate_submit_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (payment_id, _, _) = submit(payable_id, "1000").await;
        let payment_no = payment_row(payment_id).await.payment_no;

        let err = payment::submit_payment(
            payable_id,
            &pay_req("1000"),
            Some(uid(APPROVER1)),
            &none_gate(),
        )
        .await
        .expect_err("审批中再提交必须被拒");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("在审批中")
                && err.msg.contains(&payment_no)
                && err.msg.contains("1000"),
            "文案要点名单号与金额：{}",
            err.msg
        );
        // 只留一笔 PENDING（库侧 uk_biz_payment_pending 与这里同口径）
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_payment WHERE payable_id = ?::uuid AND approval_status = 'PENDING'",
                vec![Value::from(&payable_id)],
            )
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    // ---------- ④ 金额 > 未付余额 → 400 ----------

    /// ④ 提交金额超过未付余额 → 400（P7 的校验在审批路径上一条都不能丢）。
    #[tokio::test]
    async fn p17_amount_over_unpaid_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let err = payment::submit_payment(
            payable_id,
            &pay_req("5000.01"),
            Some(uid(CREATOR)),
            &none_gate(),
        )
        .await
        .expect_err("超过未付余额必须被拒");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(
            err.msg.contains("超过未付余额 5000"),
            "文案要点名未付余额：{}",
            err.msg
        );
        // 零落库：既没有付款记录，也没有审批实例
        let instances: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'PAYABLE_PAY' AND biz_id IN \
                 (SELECT id FROM biz_payment WHERE payable_id = ?::uuid)",
                vec![Value::from(&payable_id)],
            )
            .await
            .unwrap();
        assert_eq!(instances, 0);
        assert!(payment_row_count(payable_id).await == 0);
    }

    async fn payment_row_count(payable_id: Uuid) -> i64 {
        db().scalar_with_params(
            "SELECT count(*) FROM biz_payment WHERE payable_id = ?::uuid",
            vec![Value::from(&payable_id)],
        )
        .await
        .unwrap()
    }

    // ---------- ⑤ 对已结清的单据付款 → 409 ----------

    /// ⑤ 对 `SETTLED` 的应付单付款 → 409。
    #[tokio::test]
    async fn p17_pay_settled_payable_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        exec(&format!(
            "UPDATE biz_payable SET paid_amount = 5000, unpaid_amount = 0, status = 'SETTLED' \
              WHERE id = '{payable_id}'::uuid"
        ))
        .await;
        let err =
            payment::submit_payment(payable_id, &pay_req("1"), Some(uid(CREATOR)), &none_gate())
                .await
                .expect_err("已结清不能再付款");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已结清"), "{}", err.msg);
    }

    // ---------- ⑥⑦⑧ 金额分级（条件节点，不是代码 if/else） ----------

    /// ⑥ 5000 元（< 1 万）→ 只走「部门主管审批」节点。
    #[tokio::test]
    async fn p17_amount_5000_routes_to_dept_lead() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (_, instance_id, _) = submit(payable_id, "5000").await;
        let tasks = pending_tasks(instance_id.unwrap()).await;
        assert!(!tasks.is_empty());
        assert!(
            tasks.iter().all(|(_, _, node)| node == "n_dept"),
            "5000 元必须只落在 n_dept：{:?}",
            tasks
        );
    }

    /// ⑦ 50000 元（1 万 ≤ 金额 < 10 万）→ 只走「财务经理审批」节点。
    #[tokio::test]
    async fn p17_amount_50000_routes_to_finance_manager() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("50000")).await;
        let (_, instance_id, _) = submit(payable_id, "50000").await;
        let tasks = pending_tasks(instance_id.unwrap()).await;
        assert!(!tasks.is_empty());
        assert!(
            tasks.iter().all(|(_, _, node)| node == "n_fin"),
            "50000 元必须只落在 n_fin：{:?}",
            tasks
        );
    }

    /// ⑧ 500000 元（≥ 10 万）→ 走「财务总监+总经理会签」，且**每个审批人一条待办**（≥ 2）。
    #[tokio::test]
    async fn p17_amount_500000_routes_to_countersign() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("500000")).await;
        let (_, instance_id, _) = submit(payable_id, "500000").await;
        let tasks = pending_tasks(instance_id.unwrap()).await;
        assert!(!tasks.is_empty());
        assert!(
            tasks.iter().all(|(_, _, node)| node == "n_board"),
            "500000 元必须只落在 n_board：{:?}",
            tasks
        );
        assert!(
            tasks.len() >= 2,
            "会签节点必须给每个审批人一条待办（实测 {} 条）",
            tasks.len()
        );
    }

    /// ⑨ 会签：只批一个**不算通过**，全部批完才通过。
    #[tokio::test]
    async fn p17_countersign_needs_every_vote() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("500000")).await;
        let (payment_id, instance_id, _) = submit(payable_id, "500000").await;
        let instance_id = instance_id.unwrap();
        // 审批人 = 全部超管（制单人不是超管，所以他本来就不在审批人里）
        let mut legal: Vec<(Uuid, Uuid)> = pending_tasks(instance_id)
            .await
            .into_iter()
            .map(|(task, assignee, _)| (task, assignee))
            .collect();
        assert!(legal.len() >= 2, "会签至少要两个审批人：{}", legal.len());

        let last = legal.pop().unwrap();
        for (task, assignee) in &legal {
            let status = approve(*task, *assignee).await;
            assert_eq!(status, "PENDING", "只批一部分时实例必须还是 PENDING");
            assert_eq!(
                payment_row(payment_id).await.approval_status,
                APPROVAL_PENDING,
                "会签没批完不能记账"
            );
        }
        let status = approve(last.0, last.1).await;
        assert_eq!(status, "APPROVED", "全部批完必须通过");
        assert_eq!(
            payment_row(payment_id).await.approval_status,
            APPROVAL_APPROVED
        );
    }

    // ---------- ⑩⑪⑫⑬ 审批通过 → 记账 ----------

    /// ⑩ 通过 → `paid/unpaid` 正确更新，`OPEN → PARTIAL_PAID`。
    #[tokio::test]
    async fn p17_approval_marks_partial_paid() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (_, instance_id, _) = submit(payable_id, "2000").await;
        let instance_id = instance_id.unwrap();
        approve_all(instance_id).await;

        let payable = payable_row(payable_id).await;
        assert_eq!(payable.paid_amount, dec("2000"));
        assert_eq!(payable.unpaid_amount, dec("3000"));
        assert_eq!(payable.status, payable_mod::STATUS_PARTIAL_PAID);
        assert!(payable.settled_at.is_none());
    }

    /// ⑪ 通过 → 全额付款 → `SETTLED`。
    #[tokio::test]
    async fn p17_approval_full_amount_settles() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (_, instance_id, _) = submit(payable_id, "5000").await;
        approve_all(instance_id.unwrap()).await;

        let payable = payable_row(payable_id).await;
        assert_eq!(payable.paid_amount, dec("5000"));
        assert_eq!(payable.unpaid_amount, Decimal::ZERO);
        assert_eq!(payable.status, payable_mod::STATUS_SETTLED);
        assert!(payable.settled_at.is_some(), "结清必须写 settled_at");
    }

    /// ⑫ 通过 → 生成付款凭证，**借 2202 = 贷 1002 = 金额**，借贷平衡。
    #[tokio::test]
    async fn p17_approval_generates_balanced_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (payment_id, instance_id, _) = submit(payable_id, "5000").await;
        approve_all(instance_id.unwrap()).await;

        let row = payment_row(payment_id).await;
        let voucher_id = row.voucher_id.expect("审批通过必须生成付款凭证");
        let voucher = find_voucher(db(), voucher_id).await.unwrap().unwrap();
        assert_eq!(voucher.status, voucher::STATUS_POSTED);
        assert_eq!(voucher.source_type, voucher::SOURCE_PAYMENT);
        assert_eq!(voucher.total_debit, dec("5000"));
        assert_eq!(voucher.total_credit, dec("5000"));

        let entries = entries_of(db(), voucher_id).await.unwrap();
        assert_eq!(entries.len(), 2);
        let debit = entries
            .iter()
            .find(|line| line.account.code == payment::PAY_DEBIT_CODE)
            .expect("缺借方 2202");
        let credit = entries
            .iter()
            .find(|line| line.account.code == payment::PAY_CREDIT_CODE)
            .expect("缺贷方 1002");
        assert_eq!(debit.debit, dec("5000"));
        assert_eq!(credit.credit, dec("5000"));
        assert_eq!(debit.debit, credit.credit, "借贷必须平衡");
    }

    /// ⑬ 通过 → 凭证**挂在付款记录上**（`biz_payment.voucher_id` + `source_id` 双向可追）。
    ///
    /// ⚠️ 口径说明：`biz_payable.voucher_id` 是**进项凭证**（收货过账时的那张，借贷 1405/2202），
    /// 付款凭证不能覆盖它 —— 否则「应付单关联的是哪张凭证」就说不清了。所以付款凭证挂在
    /// `biz_payment.voucher_id`，与 P7 直付口径**完全一致**（同一段记账代码）。
    #[tokio::test]
    async fn p17_approval_backfills_payment_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (payment_id, instance_id, _) = submit(payable_id, "1000").await;
        approve_all(instance_id.unwrap()).await;

        let row = payment_row(payment_id).await;
        let voucher_id = row.voucher_id.expect("付款记录必须回写凭证");
        let voucher = find_voucher(db(), voucher_id).await.unwrap().unwrap();
        assert_eq!(
            voucher.source_id,
            Some(payment_id),
            "凭证的 source_id 指向付款记录"
        );
        assert_eq!(voucher.source_no.as_deref(), Some(row.payment_no.as_str()));
        // 凭证号能在详情里查到（前端 drawer 直接显示）
        let detail = payable_query::payable_detail(payable_id).await.unwrap();
        let shown = detail
            .payments
            .iter()
            .find(|item| item.id == payment_id.to_string())
            .expect("详情里必须有这条付款");
        assert_eq!(
            shown.voucher_id.as_deref(),
            Some(voucher_id.to_string().as_str())
        );
        assert_eq!(shown.approval_status, APPROVAL_APPROVED);
        assert!(!shown.pending);
    }

    /// 全部审批人都批一遍（制单人不是超管，不在审批人里）。
    async fn approve_all(instance_id: Uuid) {
        let tasks = pending_tasks(instance_id).await;
        for (task, assignee, _) in tasks {
            approve(task, assignee).await;
        }
    }

    // ---------- ⑭ 驳回 → 账不动，可重新提交 ----------

    /// ⑭ 驳回 → `paid` 一分不动、付款记录 `REJECTED`、**可以重新提交**。
    #[tokio::test]
    async fn p17_reject_keeps_books_and_allows_resubmit() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (payment_id, instance_id, _) = submit(payable_id, "5000").await;
        let instance_id = instance_id.unwrap();
        let tasks = pending_tasks(instance_id).await;
        let (task, assignee) = tasks
            .iter()
            .find(|(_, assignee, _)| *assignee != uid(CREATOR))
            .map(|(task, assignee, _)| (*task, *assignee))
            .expect("必须有可驳回的待办");
        assert_eq!(reject(task, assignee).await, "REJECTED");

        // 账一分不动
        let payable = payable_row(payable_id).await;
        assert_eq!(payable.paid_amount, Decimal::ZERO);
        assert_eq!(payable.unpaid_amount, dec("5000"));
        assert_eq!(payable.status, payable_mod::STATUS_OPEN);
        let row = payment_row(payment_id).await;
        assert_eq!(row.approval_status, APPROVAL_REJECTED);
        assert!(row.voucher_id.is_none(), "驳回不能出凭证");

        // 可以重新提交
        let (again, instance2, outcome) = submit(payable_id, "5000").await;
        assert_eq!(outcome.outcome, OUTCOME_PENDING);
        assert_ne!(again, payment_id);
        assert!(instance2.is_some());
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_payment WHERE payable_id = ?::uuid AND approval_status = 'REJECTED'",
                vec![Value::from(&payable_id)],
            )
            .await
            .unwrap();
        assert_eq!(count, 1, "被驳回的那笔要留痕（供审计）");
    }

    // ---------- ⑮ 审批中应付单被作废 → 撤审批 ----------

    /// ⑮ 付款审批中把应付单作废 → 审批实例 `CANCELLED`、待办消失、付款记录 `CANCELLED`。
    #[tokio::test]
    async fn p17_cancel_payable_terminates_pending_approval() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (payment_id, instance_id, _) = submit(payable_id, "5000").await;
        let instance_id = instance_id.unwrap();
        assert!(!pending_tasks(instance_id).await.is_empty());

        payment::cancel_payable(payable_id, Some("重复入库"), Some(uid(CREATOR)))
            .await
            .unwrap_or_else(|err| panic!("作废失败：{} {}", err.code, err.msg));

        let instance = flow_model::find_instance(instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance.status, "CANCELLED", "在跑的付款审批必须被终止");
        assert!(
            pending_tasks(instance_id).await.is_empty(),
            "终止后不能留孤儿待办（P11 教训）"
        );
        let row = payment_row(payment_id).await;
        assert_eq!(row.approval_status, APPROVAL_CANCELLED);
        assert!(row.voucher_id.is_none(), "作废路径不能出凭证");
        let payable = payable_row(payable_id).await;
        assert_eq!(payable.status, payable_mod::STATUS_CANCELLED);
        // 应付单上的弱引用必须清掉（与「PENDING ⟺ 有引用」同生同灭）
        let leaked: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_payable WHERE id = ?::uuid AND payment_approval_instance_id IS NOT NULL",
                vec![Value::from(&payable_id)],
            )
            .await
            .unwrap();
        assert_eq!(leaked, 0);
    }

    // ---------- ⑯ 审批通过但单据已作废 → 拒绝执行 + CANCELLED ----------

    /// ⑯ 审批走到一半单据被别处作废（绕过作废入口）→ 通过时**拒绝执行**，付款记录标 `CANCELLED`。
    #[tokio::test]
    async fn p17_approve_after_payable_cancelled_is_refused() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        let (payment_id, instance_id, _) = submit(payable_id, "5000").await;
        let instance_id = instance_id.unwrap();
        // 绕过作废入口直接改状态（模拟「别处把单据作废了」）：审批实例还在跑
        exec(&format!(
            "UPDATE biz_payable SET status = 'CANCELLED' WHERE id = '{payable_id}'::uuid"
        ))
        .await;

        approve_all(instance_id).await;

        let row = payment_row(payment_id).await;
        assert_eq!(
            row.approval_status, APPROVAL_CANCELLED,
            "单据已作废 → 拒绝执行，付款记录标 CANCELLED"
        );
        assert!(row.voucher_id.is_none(), "拒绝执行不能出凭证");
        let payable = payable_row(payable_id).await;
        assert_eq!(payable.paid_amount, Decimal::ZERO, "账一分不能动");
        assert_eq!(payable.unpaid_amount, dec("5000"));
    }

    // ---------- ⑰ 内控：制单人不能批自己 ----------

    /// ⑰ 制单人点自己那票 → 403「不能审批自己提交的付款申请」，且账不动。
    ///
    /// 场景：这一单由**超管（也是审批人）**自己提交。
    ///
    /// P0-FIX 之后语义是「**生成待办时就排除**」：他拿到的是一条 `SKIPPED` 留痕行
    /// （带内控原因），不是 `PENDING` —— 既看不到待办、也批不了；
    /// `guard_self_approval` 的 403 兜底仍在（转办 / 加签 / 老数据绕过时才会走到它）。
    #[tokio::test]
    async fn p17_creator_cannot_approve_own_payment() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payable_id = new_payable(dec("5000")).await;
        // 由超管 APPROVER1 自己提交（他就是制单人，也在审批人里）
        let outcome = payment::submit_payment(
            payable_id,
            &pay_req("5000"),
            Some(uid(APPROVER1)),
            &none_gate(),
        )
        .await
        .unwrap_or_else(|err| panic!("提交失败：{} {}", err.code, err.msg));
        let payment_id = Uuid::parse_str(outcome.payment_id.as_deref().unwrap()).unwrap();
        let instance_id =
            Uuid::parse_str(outcome.approval_instance_id.as_deref().unwrap()).unwrap();

        let mine = task_of(instance_id, uid(APPROVER1))
            .await
            .map(|(task, status, comment)| {
                assert_eq!(
                    status, "SKIPPED",
                    "P0-FIX：制单人的票在生成时就被内控排除，不进 PENDING"
                );
                assert!(comment.contains("内控"), "留痕要写原因：{comment}");
                task
            })
            .expect("超管制单人必须有留痕行（否则审计回答不了「他没批、为什么」）");
        assert!(
            pending_tasks(instance_id)
                .await
                .iter()
                .all(|(_, assignee, _)| *assignee != uid(APPROVER1)),
            "制单人不该出现在「待批」清单里（他点不动）"
        );

        let err = guard_self_approval(mine, uid(APPROVER1))
            .await
            .expect_err("制单人审批自己的付款必须被拒");
        assert_eq!(err.status, 403, "{}", err.msg);
        assert!(
            err.msg.contains("不能审批自己提交的付款申请"),
            "文案要按内控口径：{}",
            err.msg
        );

        // 别人可以批（对照）：非制单人的待办放行
        let other = pending_tasks(instance_id)
            .await
            .into_iter()
            .find(|(_, assignee, _)| *assignee != uid(APPROVER1))
            .map(|(task, assignee, _)| (task, assignee))
            .expect("必须有别人的待办");
        guard_self_approval(other.0, other.1)
            .await
            .expect("非制单人应放行");

        // 账一分没动（制单人那一票没生效，会签也没凑齐）
        assert_eq!(
            payment_row(payment_id).await.approval_status,
            APPROVAL_PENDING
        );
        assert_eq!(payable_row(payable_id).await.paid_amount, Decimal::ZERO);
    }

    // ---------- ⑱ 迁移自检：种子流程图与 hash 自洽 ----------

    /// ⑱ 种子流程：快照 hash 与行数据自洽、图能过发布校验、条件边与会签口径都在库里。
    #[tokio::test]
    async fn p17_seeded_flow_graph_is_consistent() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let flow = published_pay_flow()
            .await
            .unwrap()
            .expect("必须能查到 payable_pay 流程");
        assert_eq!(flow.flow_code, "payable_pay");
        assert_eq!(flow.biz_type, BIZ_TYPE);

        let graph = flow_model::load_graph(flow.version_id)
            .await
            .expect("加载流程图失败");
        let issues = graph.validate();
        assert!(
            issues.is_empty(),
            "种子流程图必须能过发布校验：{:?}",
            issues
                .iter()
                .map(|issue| &issue.message)
                .collect::<Vec<_>>()
        );

        // hash 自洽（快照 vs 行）
        let snapshot_text = flow_model::load_snapshot(flow.version_id)
            .await
            .unwrap()
            .expect("缺 graph_snapshot");
        let snapshot = graph::parse_snapshot(&snapshot_text).expect("快照解析失败");
        let recomputed = graph::canonical_json(&serde_json::to_value(&snapshot).unwrap());
        let digest = {
            use sha2::{Digest, Sha256};
            let mut out = String::new();
            for byte in Sha256::digest(recomputed.as_bytes()) {
                out.push_str(&format!("{byte:02x}"));
            }
            out
        };
        let stored: String = db()
            .scalar_with_params(
                "SELECT graph_hash FROM biz_flow_def_version WHERE id = ?::uuid",
                vec![Value::from(&flow.version_id)],
            )
            .await
            .unwrap();
        assert_eq!(stored, digest, "快照 hash 与库里的 graph_hash 不一致");
        assert_eq!(stored, graph.graph_hash(), "行数据重算的 hash 与库里不一致");

        // 金额分级真的落在条件边上（不是代码 if/else）
        let amount_edges = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.condition
                    .as_ref()
                    .and_then(|value| value.get("field"))
                    .and_then(|value| value.as_str())
                    == Some("form.amount")
            })
            .count();
        assert_eq!(amount_edges, 2, "必须有两条按 form.amount 分流的条件边");
        // 会签节点必须 ALL
        let board = graph.node("n_board").expect("缺会签节点");
        assert_eq!(board.node_type, graph::NodeType::Countersign);
        assert_eq!(board.approval_mode, Some(graph::ApprovalMode::All));
    }

    // ---------- 纯逻辑：状态映射 ----------

    /// ⑲ 实例状态 → 业务语义（只有 APPROVED 能动账）。
    #[test]
    fn p17_verdict_maps_only_final_states() {
        assert_eq!(settle_verdict("APPROVED"), Some(Verdict::Execute));
        assert_eq!(settle_verdict(" approved "), Some(Verdict::Execute));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                settle_verdict(status),
                Some(Verdict::Abort),
                "{status} 必须打回"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(settle_verdict(status), None, "{status} 不该推进付款");
        }
    }

    /// ⑳ 常量与种子同口径（写错一个字母就是「流程查不到 → 静默降级直付」）。
    #[test]
    fn p17_constants_match_seed() {
        assert_eq!(BIZ_TYPE, "PAYABLE_PAY");
        assert_eq!(FLOW_CODE, "payable_pay");
        assert_eq!(payment::PAY_BIZ_TYPE, BIZ_TYPE);
        assert_eq!(payment::PAY_FLOW_CODE, FLOW_CODE);
        assert_eq!(payment::approval_status_label(APPROVAL_PENDING), "审批中");
        assert_eq!(payment::approval_status_label(APPROVAL_APPROVED), "已通过");
        assert_eq!(payment::approval_status_label(APPROVAL_REJECTED), "已驳回");
    }

    /// ㉑ 金额转 JSON 数字（条件节点的 `cmp` 只认 Number，字符串会静默走兜底边）。
    #[test]
    fn p17_amount_number_is_json_number() {
        let value = payment::amount_number(dec("9999.9999"));
        assert!(value.is_number(), "金额必须是 JSON 数字：{value:?}");
        assert_eq!(value.as_f64().unwrap(), 9999.9999);
        let value = payment::amount_number(dec("10000"));
        assert_eq!(value.as_f64().unwrap(), 10000.0);
    }

    /// ㉒ 收尾：把本模块的夹具用户删净（**靠名字排在模块最后跑**）。
    ///
    /// 为什么必须有这一条：审批人来源是「全部超管」，而兄弟模块（transfer_handler /
    /// stockin_handler）的用例会断言「发起后审批中心有几条待办」—— 多出来的两个超管夹具
    /// 会把他们的断言从 1 变成 3。夹具不许留在库里。
    #[tokio::test]
    async fn p17_zzz_fixture_users_are_removed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        cleanup_fixture_users().await;
        let left: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM sys_user WHERE username IN ('p17_creator','p17_ap1','p17_ap2')",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(
            left, 0,
            "夹具用户必须删净（否则会污染「审批人 = 全部超管」的兄弟模块用例）"
        );
    }

    /// 删掉 P17 的夹具用户（先删角色关联，再删用户）。
    async fn cleanup_fixture_users() {
        exec(
            "DELETE FROM sys_user_role WHERE user_id IN \
             (SELECT id FROM sys_user WHERE username IN ('p17_creator','p17_ap1','p17_ap2'))",
        )
        .await;
        exec("DELETE FROM sys_user WHERE username IN ('p17_creator','p17_ap1','p17_ap2')").await;
    }

    // ---------- 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 本模块跑完**不许留** `PAYABLE` / `VOUCHER` 状态日志（含孤儿行）。
    ///
    /// 付款审批落账会同时写「应付流转」与「付款凭证」两条日志；`biz_status_log` 没有外键，
    /// 清场里漏掉「删单据前先删日志」时，每跑一遍全量多 6 条 VOUCHER 孤儿（本卡实测）。
    /// `zz_` 排在本模块最后（`p17_zzz_*` 之后），是这一块的收口。
    #[tokio::test]
    async fn zz_teardown_purges_payment_status_logs() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // ① 自造一张会写状态日志的单：临时关流程 + **显式跳过审批**（ARCH-3 DC-01 的真实直付路径）
        let payable_id = new_payable(dec("5000")).await;
        exec("UPDATE biz_flow_def SET status = 'DISABLED' WHERE flow_code = 'payable_pay'").await;
        let claims = skip_claims();
        let mut req = pay_req("5000");
        req.approval = approval_gate::SkipApprovalReq {
            skip_approval: true,
            skip_reason: Some("月底关账，供应商已现场验收".to_string()),
        };
        let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
        let result = payment::submit_payment(payable_id, &req, Some(uid(CREATOR)), &gate).await;
        exec("UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'payable_pay'").await;
        let outcome =
            result.unwrap_or_else(|err| panic!("显式跳过审批的直付失败：{} {}", err.code, err.msg));
        assert_eq!(
            outcome.outcome, OUTCOME_PAID,
            "显式跳过审批后必须直接生效（这就是「暂不审批」的合法出路）"
        );
        // 跳过审批的理由必须落进 biz_status_log（审计要能看出「这次没有复核、为什么」）
        let reasons: Vec<String> = db()
            .find_with_params(
                "SELECT reason FROM biz_status_log \
                  WHERE entity_type = 'PAYABLE' AND entity_id = ?::uuid AND reason IS NOT NULL",
                vec![Value::from(&payable_id)],
            )
            .await
            .expect("查应付状态日志")
            .iter()
            .map(|row| row.get_typed::<String>("reason").unwrap_or_default())
            .collect();
        assert!(
            reasons
                .iter()
                .any(|text| text.contains("跳过审批") && text.contains("payable_pay")),
            "跳过审批的理由必须写进 biz_status_log（实际：{reasons:?}）"
        );

        let voucher_id: Option<Uuid> = db()
            .query_first_sql(
                "SELECT id FROM biz_voucher WHERE source_id IN \
                  (SELECT id FROM biz_payment WHERE payable_id = ?::uuid) LIMIT 1"
                    .to_string(),
                vec![Value::from(&payable_id)],
            )
            .await
            .expect("查付款凭证")
            .map(|row| row.get_typed("id").expect("读凭证 id"));
        let voucher_id = voucher_id.expect("付款必须生成一张凭证（否则这条用例证明不了什么）");

        let count_logs = |entity_type: &'static str, target: Uuid| async move {
            db().scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = ? AND entity_id = ?::uuid",
                vec![Value::from(entity_type), Value::from(&target)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0)
        };
        for (label, target) in [("PAYABLE", payable_id), ("VOUCHER", voucher_id)] {
            assert!(
                count_logs(label, target).await > 0,
                "清场前必须有 {label} 状态日志，否则这条用例证明不了什么"
            );
        }

        // ② 走模块真实的清场
        cleanup_fixtures().await;

        for (label, target) in [("PAYABLE", payable_id), ("VOUCHER", voucher_id)] {
            assert_eq!(
                count_logs(label, target).await,
                0,
                "清场后 {label} 状态日志必须为 0（日志无外键，必须按 id 先删）"
            );
        }

        // ③ 兜底：整库不再有 PAYABLE / VOUCHER 的孤儿行
        fixture_hygiene::purge_orphans(fixture_hygiene::FINANCE).await;
        assert_eq!(
            fixture_hygiene::orphans(fixture_hygiene::FINANCE).await,
            0,
            "整库不许留 PAYABLE / VOUCHER 的孤儿状态日志"
        );
    }
}

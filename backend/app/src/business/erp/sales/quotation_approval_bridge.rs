//! 销售报价单审批的**业务回调 + 发出闸**（销售链 · M3-04）。
//!
//! 与 `receipt_refund_bridge`（收款退款）、`sales_approval_bridge`（销售订单）并列的一类业务回调：
//! 报价单本身**没有**「审批中」状态，审批是「发出」的前置条件。
//!
//! # 口径（沿用 t_b9840c8d 的路线 ②：不动状态域）
//!
//! ```text
//! DRAFT ──提交审批──▶ [biz_flow_instance: QUOTATION_APPROVE / PENDING]
//!   │                        ├── 审批通过 ──▶ SENT（回调**自动发出**，写状态日志）
//!   │                        └── 驳回 / 撤回 / 终止 ──▶ 仍是 DRAFT（可改完再提）
//!   └── 直接 PATCH status=SENT ──▶ 409 + 66088（必须先有 APPROVED 实例）
//! ```
//!
//! **审批不是一个状态**：`biz_quotation.status` 的五个值（DRAFT / SENT / ACCEPTED /
//! REJECTED / CANCELLED）语义一个字没改，`ck_biz_quotation_status` 也没动。
//! 理由与收款退款逐条相同（见 `receipt_refund_bridge` 模块注释），另加一条报价单特有的：
//!
//! - 报价单的「审批」是**发给客户之前**的内部复核（价格 / 折扣 / 税点），而 `SENT` 已经
//!   是「客户手里有一份」的事实；把「内部还没批」挤进 `status`，会让「客户到底收到没有」
//!   与「内部批了没有」两件事共用一个字段 —— 报表与状态日志都得再拆一次。
//!
//! # 为什么通过后**自动发出**（而不是留给人再点一次）
//!
//! 「审批通过后还要人再点发出」那一步没人点，就等于「内部批了但客户从没收到」：
//! 单据看着是草稿、销售以为报出去了、客户在等 —— 典型的静默漏发。本仓口径是回调直接执行
//! 那个不可逆动作（与 `payable_bridge` 通过即记账、`receipt_refund_bridge` 通过即过账同构）。
//!
//! 发出本体与人工 `PATCH /status` **同一条代码路径**（`quotation_handler::change_quotation_status`
//! → 三道守卫 → 状态机留痕 → CAS），不另写一套 —— 两条路径迟早会在守卫上走岔。
//!
//! # 五个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`published_quotation_flow`] | `quotation_handler::submit_approval` | 取已发布的 `quotation_approve` 流程（没配 → 409 默认拒绝） |
//! | [`approved_instance_of`] | `quotation_handler::change_quotation_status`（**发出闸**） | 这张报价单有没有 `APPROVED` 的审批实例 |
//! | [`on_quotation_settled`] | `flow::doc_reader::dispatch_settled`（**审批事务提交之后**） | `APPROVED` → 自动发出；`REJECTED` / `WITHDRAWN` / `CANCELLED` → 留在 DRAFT |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//!
//! # 时序与失败口径
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。此时报价单仍是 `DRAFT` 且**已有一个 APPROVED 实例**，
//! 发出闸对它放行 —— 销售修好阻塞原因（通常是有效期已过 / 客户被冻结）后手工
//! `PATCH /status target_status=SENT` 即可补发，不会出现「批了却永远发不出去」的死局。
//!
//! # 表单快照
//!
//! 发起时把报价单的关键字段冻结进 `biz_flow_instance.form_data`（[`form_data`]）。
//! `amount` 必须是 **JSON 数字**（= 价税合计）：`V0435` 的金额分级条件边写的是
//! `form.amount`，字符串会被 `cmp` 判成 `TypeMismatch` 并静默掉进兜底边
//! （与 `payable_payment::payment_form_data` / `receipt_refund_bridge::form_data` 同一个坑）。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::sales::quotation_model as model;
use crate::business::flow::model as flow_model;
use cloudpivot_kernel::business::shared::separation;
use cloudpivot_kernel::shared::error_code;

/// 报价单审批的业务类型（`biz_flow_def.biz_type`，与 `V0435` 种子逐字一致）。
///
/// 与其它审批同命名法：`biz_type` 大写（`QUOTATION_APPROVE`）、`flow_code` 小写
/// （`quotation_approve`，`ck_biz_flow_def_code` 要求 `^[a-z][a-z0-9_]{1,63}$`）。
pub const BIZ_TYPE: &str = "QUOTATION_APPROVE";
/// 报价单审批的流程编码。
pub const FLOW_CODE: &str = "quotation_approve";

/// 一条「已发布的报价单审批流程」（发起审批要用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotationFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`QUOTATION_APPROVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的报价单审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按 `approval_gate::authorize_missing_flow`
/// 处理（默认 409，只有持 `business:erp:skip-approval` 且写明理由 ≥ 10 字才放行）。
///
/// 排序口径与 `sales_approval_bridge::published_so_flow` 一致
/// （`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间）。
pub async fn published_quotation_flow() -> DbResult<Option<QuotationFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'QUOTATION_APPROVE' OR lower(d.flow_code) = 'quotation_approve') \
              ORDER BY (d.biz_type = 'QUOTATION_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(QuotationFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 审批实例查询（发出闸 + 详情页） ====================

/// 这张报价单**最近一条**审批实例的摘要（详情页 / 详情出参用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalBrief {
    /// 实例 id。
    pub instance_id: Uuid,
    /// 审批单号（引擎口径 `FLW-yyyyMMdd-XXXXXXXX`）。
    pub instance_no: String,
    /// 实例状态（`PENDING` / `APPROVED` / `REJECTED` / `WITHDRAWN` / `CANCELLED`）。
    pub status: String,
    /// 还在等谁批（`PENDING` 时的待办数；其它状态恒为 0）。
    pub pending_tasks: i64,
}

/// 查这张报价单最近一条审批实例（可放进事务）。
///
/// 「最近一条」按 `started_at DESC, created_at DESC`：一张报价单被驳回后重新提交会留下
/// 多条实例，前端要看的是**当前这一轮**。
pub async fn latest_approval_of(ex: &impl SqlExecutor, quotation_id: Uuid) -> DbResult<Option<ApprovalBrief>> {
    // SCOPE-EXEMPT: 审批实例是流程运行时表，按 (biz_type, biz_id) 精确取最近 1 行；
    // 可见性由报价单本身的 detail 权限控制，不在这里按部门裁剪。
    let row = ex
        .query_first_sql(
            "SELECT i.id, i.instance_no, i.status, \
                    (SELECT count(*) FROM biz_flow_task t \
                      WHERE t.instance_id = i.id AND t.status = 'PENDING') AS pending_tasks \
               FROM biz_flow_instance i \
              WHERE i.biz_type = 'QUOTATION_APPROVE' AND i.biz_id = ?::uuid \
                AND i.deleted_at IS NULL \
              ORDER BY i.started_at DESC, i.created_at DESC LIMIT 1"
                .to_string(),
            vec![Value::from(&quotation_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(ApprovalBrief {
            instance_id: row.get_typed::<Uuid>("id")?,
            instance_no: row.get_typed::<String>("instance_no")?,
            status: row.get_typed::<String>("status")?,
            pending_tasks: row.get_typed::<i64>("pending_tasks")?,
        })),
    }
}

/// 一张「已通过」的审批实例（发出闸的判据 + 状态日志里的单号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedInstance {
    /// 实例 id。
    pub id: Uuid,
    /// 审批单号（写进 `biz_status_log.reason`，人工补发时也能追溯是哪一轮批的）。
    pub instance_no: String,
}

/// 这张报价单有没有**已通过**的审批实例（可放进事务）。
///
/// 发出闸只认这一个判据：`biz_type = 'QUOTATION_APPROVE'` 且 `biz_id = 报价单 id` 且
/// `status = 'APPROVED'`。不认「最近一条实例是什么状态」—— 驳回后重新提交的场景里，
/// 最近一条可能还是 `PENDING`、而更早那条是 `APPROVED`：钱 / 价已经被批过，
/// 只是又提了一轮重复的，此时应当**放行**。宁可放过重复提交，也不要让已经批过的报价发不出去。
pub async fn approved_instance_of(ex: &impl SqlExecutor, quotation_id: Uuid) -> DbResult<Option<ApprovedInstance>> {
    // SCOPE-EXEMPT: 同上，按 (biz_type, biz_id, status) 精确取 1 行。
    let row = ex
        .query_first_sql(
            "SELECT id, instance_no FROM biz_flow_instance \
              WHERE biz_type = 'QUOTATION_APPROVE' AND biz_id = ?::uuid \
                AND status = 'APPROVED' AND deleted_at IS NULL \
              ORDER BY started_at DESC, created_at DESC LIMIT 1"
                .to_string(),
            vec![Value::from(&quotation_id)],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(ApprovedInstance {
            id: row.get_typed::<Uuid>("id")?,
            instance_no: row.get_typed::<String>("instance_no")?,
        })),
    }
}

// ==================== 发起：单据表单快照 ====================

/// 发起审批时冻结进实例的 `form_data`（流程条件表达式的**唯一作用域**）。
///
/// **必须有 `amount`**：`V0435` 的金额分级条件边写的是 `form.amount`
/// （`< 100000` 一档），键名对不上 → 条件恒不命中 → 所有单都掉进兜底边（多批一道）。
/// `amount` = **价税合计**（客户看到的那个数，也是对外的承诺额），与销售订单同口径。
///
/// `create_by` 是内控留痕用的（真正的拦截在 [`guard_self_approval`]），
/// 制单人已被清空的历史行写 `null`，不写空串冒充「查到了」。
pub fn form_data(row: &model::QuotationRow) -> serde_json::Value {
    let amount = row.total_with_tax.round_dp(4).to_string();
    serde_json::json!({
        "quotation_id": row.id.to_string(),
        "quote_no": row.quote_no,
        // 条件边的作用域键（金额分级的唯一依据）
        "amount": json_number(&amount),
        "total_with_tax": amount,
        "total_amount": row.total_amount.to_string(),
        "total_tax": row.total_tax.to_string(),
        "customer_id": row.customer_id.to_string(),
        "quote_date": row.quote_date.to_string(),
        "valid_until": row.valid_until.to_string(),
        "create_by": row.create_by.map(|value| value.to_string()),
    })
}

/// 定点字符串 → JSON 数字（条件表达式要按数值比较；`"169.50"` 字符串会被当成字符串）。
fn json_number(decimal_text: &str) -> serde_json::Value {
    match decimal_text.parse::<f64>() {
        Ok(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::json!(0)),
        Err(_) => serde_json::json!(0),
    }
}

// ==================== 回调：审批结论 → 报价单 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotationVerdict {
    /// 同意 → **自动发出**（`DRAFT → SENT`，三道守卫与人工发出同一条路径）。
    Issue,
    /// 驳回 / 撤回 / 终止 → 留在 `DRAFT`（可改完再提）。
    KeepDraft,
}

impl QuotationVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            QuotationVerdict::Issue => "ISSUE",
            QuotationVerdict::KeepDraft => "KEEP_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动报价单）。
pub fn quotation_verdict(instance_status: &str) -> Option<QuotationVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(QuotationVerdict::Issue),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(QuotationVerdict::KeepDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进报价单（`DocReader::on_settled` 身份）。
///
/// | 实例状态 | 报价单 | 说明 |
/// |---|---|---|
/// | `APPROVED` | `DRAFT → SENT` | 回调**自动发出**（写状态日志，reason 带审批单号） |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | 不动（仍是 `DRAFT`） | 价格一分没对外，可改完再提 |
/// | 其它（`PENDING` / 空） | 不动 | 还在跑 |
///
/// 返回受影响的报价单 id（没推进 / 不该推进 → `None`）。**不返回错误**：
/// 审批已落库，业务侧失败只记日志（与 `receipt_refund_bridge::on_refund_settled` 同口径）。
pub async fn on_quotation_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = quotation_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(quotation_id)) => {
            tracing::info!(
                %instance_id,
                %quotation_id,
                verdict = verdict.as_str(),
                "报价单审批结论已回调单据"
            );
            Some(quotation_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "报价单审批结论回调失败（不影响审批单，报价单仍是草稿）"
            );
            None
        }
    }
}

/// [`on_quotation_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: QuotationVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「报价单审批」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过报价单审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(quotation_id) = instance.biz_id else {
        return Ok(None);
    };
    match verdict {
        QuotationVerdict::Issue => {
            // 审批人落进状态日志（`operator`）；发出本体与人工发出是**同一条代码路径**
            // （`change_quotation_status` → 三道守卫 → 状态机留痕 → CAS），不另写一套。
            let approver = latest_completed_assignee(instance_id).await?;
            let reason = format!("审批通过（{}）自动发出", instance.instance_no);
            match crate::business::erp::sales::quotation_handler::issue_after_approval(quotation_id, approver, &reason)
                .await
            {
                Ok(crate::business::erp::sales::quotation_handler::IssueOutcome::Issued) => Ok(Some(quotation_id)),
                Ok(crate::business::erp::sales::quotation_handler::IssueOutcome::AlreadyIssued) => {
                    tracing::info!(%quotation_id, "报价单已发出（重复回调），跳过");
                    Ok(None)
                }
                Ok(crate::business::erp::sales::quotation_handler::IssueOutcome::NotDraftable { status }) => {
                    tracing::warn!(
                        %quotation_id,
                        %instance_id,
                        %status,
                        "审批已通过，但报价单不是草稿（可能已被别的路径改过）→ 不自动发出，等人工处置"
                    );
                    Ok(None)
                }
                Err(error) => {
                    tracing::warn!(
                        %quotation_id,
                        code = error.code,
                        msg = %error.msg,
                        "审批已通过，但自动发出失败（报价单仍是草稿；修好阻塞原因后可手工 PATCH /status 补发）"
                    );
                    Ok(None)
                }
            }
        }
        QuotationVerdict::KeepDraft => {
            // 不做任何状态迁移：`DRAFT → DRAFT` 不是合法边，硬写状态日志会污染状态机审计。
            // 驳回意见在审批中心的实例详情里，发起人也会收到流程引擎的通知。
            tracing::info!(
                %quotation_id,
                %instance_id,
                "审批未通过（驳回 / 撤回 / 终止）：报价单仍是草稿，价格一分没对外"
            );
            Ok(Some(quotation_id))
        }
    }
}

// ==================== 反向：终止在跑的审批 ====================

/// 终止某张报价单的**在跑审批**（作废 / 删除时用，与调用方同一事务）。
///
/// 幂等：没有在跑的实例返回 `Ok(None)`；返回 `Some(实例数)` 才表示真的终止了。
///
/// 为什么要撤而不是留：待办挂在审批人手里、单据却已作废 —— 审批人点进去发现无可审
/// （最典型的孤儿待办），且「作废单还在审批中」会让列表页的审批状态永远是 `PENDING`。
pub async fn cancel_approval_of(
    ex: &impl SqlExecutor,
    quotation_id: Uuid,
    quote_no: &str,
    reason: &str,
) -> DbResult<Option<u64>> {
    let cancelled = flow_model::cancel_pending_of_biz(ex, quotation_id, quote_no, reason).await?;
    if cancelled.instances > 0 {
        tracing::info!(
            %quotation_id,
            quote_no = quote_no,
            instances = cancelled.instances,
            tasks = cancelled.tasks,
            reason = reason,
            "报价单在跑的审批已终止（不再留孤儿待办）"
        );
        Ok(Some(cancelled.instances))
    } else {
        Ok(None)
    }
}

/// 这张报价单是不是**被审批人占着**（有 `PENDING` 的 `APPROVAL` 待办）。
///
/// 与 `flow_model::biz_instance_running`（实例 `status='PENDING'`）的区别很重要：
/// 驳回选「退发起人」后实例**仍是 `PENDING`**，但票在**发起人自己**手里（`RESUBMIT` 待办）。
/// 那种状态下单据必须可改（改完点「重新提交」），否则「退回发起人」= 把单据锁死
/// —— 用户既改不了也提交不了，只能找工程师改库。
///
/// 所以「能不能改」的判据是「有没有审批人在等这一票」，不是「实例还在不在跑」。
pub async fn held_by_approver(quotation_id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 按 (biz_type, biz_id) 精确取待办条数，不产出用户可见行集。
    let total: i64 = db()
        .scalar_sql(
            "SELECT count(*) AS total FROM biz_flow_task t \
               JOIN biz_flow_instance i ON i.id = t.instance_id \
              WHERE i.biz_type = 'QUOTATION_APPROVE' AND i.biz_id = ?::uuid \
                AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND t.status = 'PENDING' AND t.task_kind = 'APPROVAL'"
                .to_string(),
            vec![Value::from(&quotation_id)],
        )
        .await?;
    Ok(total > 0)
}

/// 发起人手里有没有「重新提交」待办（退回发起人之后由 `RESUBMIT` 承担）。
///
/// 返回待办 id（报文里点出来「去审批中心点哪一条」）。没有 → `None`。
pub async fn pending_resubmit_of(quotation_id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT t.id FROM biz_flow_task t \
               JOIN biz_flow_instance i ON i.id = t.instance_id \
              WHERE i.biz_type = 'QUOTATION_APPROVE' AND i.biz_id = ?::uuid \
                AND i.status = 'PENDING' AND i.deleted_at IS NULL \
                AND t.status = 'PENDING' AND t.task_kind = 'RESUBMIT' \
              ORDER BY t.created_at DESC LIMIT 1",
            vec![Value::from(&quotation_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("id").ok()),
    }
}

// ==================== 内控：制单人不能批自己的报价 ====================

/// 内控用：这张报价单的**制单人**（`biz_quotation.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
/// 查不到单 / 单没有 `create_by` → `None`：只是没人被排除，**不影响**审批能不能发起。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(quotation_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪（裁剪会让
    // 「看不到这张单的人」反而成为可审批人，与内控目的相反）。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_quotation WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&quotation_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("create_by"),
    }
}

/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
///
/// # 判据（任一命中即 403）
///
/// 1. `operator` == `biz_quotation.create_by`（**制单人**）；
/// 2. `operator` == 审批单的 `initiator_id`（发起人；代提场景下发起人 ≠ 制单人）。
///
/// # 为什么复用 `separation::Duty::Approve`
///
/// 「制单 ≠ 审批」在 ERP 里是**同一把内控开关**（`erp.separation_of_duties.approve`，默认开）：
/// 采购 / 收货 / 凭证红冲 / 应付付款 / 销售订单都用它，报价单不另造一个键 ——
/// 否则管理员关掉内控时会出现「销售订单能自审、报价单不能」的半开半关状态。
/// 待办生成侧的排除（[`internal_control_creator`]）读的也是同一个开关，两处不会自相矛盾。
///
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
    // **重新提交不是审批票**：驳回选「退发起人」后，发起人手里那条 `RESUBMIT` 待办本来就是
    // 他自己点的（改完单据点「重新提交」）。内控管的是「制单人给自己的价格投票」，
    // 不是「制单人把自己的单再交一遍」—— 在这里 403 会把「退回发起人」变成死锁。
    if task.task_kind == "RESUBMIT" {
        return Ok(());
    }
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(quotation_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_quotation(quotation_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的报价单：报价单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.quote_no,
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

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// ① 实例状态 → 业务语义的映射口径（钉死：只有 APPROVED 会让单据发出）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(quotation_verdict("APPROVED"), Some(QuotationVerdict::Issue));
        assert_eq!(quotation_verdict(" approved "), Some(QuotationVerdict::Issue));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                quotation_verdict(status),
                Some(QuotationVerdict::KeepDraft),
                "{status} 必须留在草稿（不是关闭、不是取消）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT", "SENT"] {
            assert_eq!(quotation_verdict(status), None, "{status} 不该推进报价单");
        }
    }

    /// ② 业务类型 / 流程编码必须与 V0435 种子同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed() {
        assert_eq!(BIZ_TYPE, "QUOTATION_APPROVE");
        assert_eq!(FLOW_CODE, "quotation_approve");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
        // `ck_biz_flow_def_code` 只认小写：flow_code 写大写会被库直接拒
        assert!(
            FLOW_CODE
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
            "flow_code 必须满足 ^[a-z][a-z0-9_]{{1,63}}$（大写会被 ck_biz_flow_def_code 拒）"
        );
        // 词表登记（`FlowBizType::ALL`）—— 没登记的话 `biz_type_domain.py --strict` 必红
        assert_eq!(
            cloudpivot_doc::FlowBizType::parse(BIZ_TYPE),
            Some(cloudpivot_doc::FlowBizType::QuotationApprove),
            "BIZ_TYPE 必须在 FlowBizType 词表里登记"
        );
        assert_eq!(
            cloudpivot_doc::FlowBizType::QuotationApprove.flow_code(),
            FLOW_CODE,
            "词表的默认 flow_code 必须与本 bridge 的常量一致"
        );
    }

    /// ③ 状态常量不能写错（错一个字就是库层 check 违约 / 前端徽标空白）。
    #[test]
    fn status_constants_are_exact() {
        assert_eq!(model::STATUS_DRAFT, "DRAFT");
        assert_eq!(model::STATUS_SENT, "SENT");
    }

    /// ④ 发出闸依赖的状态机边必须存在（`DRAFT → SENT` 是回调唯一的写入口）。
    #[test]
    fn state_machine_allows_issue_edge() {
        assert!(
            model::can_transition(model::STATUS_DRAFT, model::STATUS_SENT),
            "DRAFT → SENT 必须合法，否则审批通过后的自动发出会 409"
        );
    }

    /// ⑤ 条件边的作用域键是 **JSON 数字**（字符串会 TypeMismatch → 静默掉进兜底边）。
    #[test]
    fn form_data_amount_is_json_number() {
        let row = model::QuotationRow {
            id: Uuid::now_v7(),
            quote_no: "QT-20261117-000001".to_string(),
            customer_id: Uuid::now_v7(),
            quote_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap(),
            valid_until: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 12, 17).unwrap(),
            status: model::STATUS_DRAFT.to_string(),
            total_amount: nik::prelude::rust_decimal::Decimal::new(100000, 2),
            total_tax: nik::prelude::rust_decimal::Decimal::new(13000, 2),
            total_with_tax: nik::prelude::rust_decimal::Decimal::new(113000, 2),
            converted_so_id: None,
            converted_at: None,
            approval_instance_id: None,
            remark: None,
            created_at: nik::prelude::chrono::Utc::now(),
            updated_at: nik::prelude::chrono::Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
        };
        let form = form_data(&row);
        assert!(
            form["amount"].is_number(),
            "form.amount 必须是 JSON 数字（字符串会让条件边 TypeMismatch：{}）",
            form["amount"]
        );
        assert_eq!(form["quote_no"], "QT-20261117-000001");
        // 金额与价税合计同一分钱（分级阈值按客户看到的数算）
        assert_eq!(form["amount"], serde_json::json!(1130.0));
    }
}

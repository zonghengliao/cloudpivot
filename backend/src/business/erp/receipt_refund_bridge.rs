//! 收款退款审批的**业务回调 + 过账闸**（销售链 · SO-14 遗留卡 t_573bfe5b 的 D10 补丁）。
//!
//! 与 `payable_bridge`（应付付款）、`sales_approval_bridge`（销售订单）并列的一类业务回调，
//! 职责只有一个：**审批通过 → 退款单直接过账**；其余结论只留痕、不动账。
//!
//! # 口径（卡 t_b9840c8d 定的是路线 ②：不动状态域）
//!
//! ```text
//! DRAFT ──确认──▶ CONFIRMED ──提交审批──▶ [biz_flow_instance: RECEIPT_REFUND / PENDING]
//!                     │                              ├── 审批通过 ──▶ POSTED（回调直接过账，生成凭证）
//!                     │                              └── 驳回/撤回/终止 ──▶ 仍是 CONFIRMED（账一分没动，可改完再提）
//!                     └── 直接 PATCH status=POSTED ──▶ 409 + 66044（必须先有 APPROVED 实例）
//! ```
//!
//! **审批不是一个状态**：`biz_receipt_refund.status` 的四个值（DRAFT / CONFIRMED /
//! POSTED / CANCELLED）语义一个字没改，`ck_biz_receipt_refund_status` 也没动。
//! 审批是 `CONFIRMED → POSTED` 的**前置条件**，与凭证红冲审批（`VOUCHER_REVERSE`）
//! 同思路：被审对象的状态不变，审批通过才执行那个不可逆动作。
//!
//! 为什么不让「审批通过后还要人再点一次过账」：那一步没人点就等于「审批过了但没记账」，
//! 单据看起来是「已确认」、账上没有凭证 —— 典型的静默漏账。本仓的口径是回调直接执行
//! （与 `payable_bridge::settle` 通过即记账完全同构）。
//!
//! # 五个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`published_refund_flow`] | `receipt_refund::submit_approval` | 取已发布的 `receipt_refund` 流程（没配 → 409 默认拒绝） |
//! | [`approved_instance_of`] | `receipt_refund::change_status`（**过账闸**） | 这张退款单有没有 `APPROVED` 的审批实例（返回实例 id + 单号） |
//! | [`on_refund_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → 过账；`REJECTED` / `WITHDRAWN` / `CANCELLED` → 留在 CONFIRMED |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 + 65019 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//!
//! # 时序与失败口径
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。此时退款单仍是 `CONFIRMED` 且**已有一个 APPROVED 实例**，
//! 过账闸对它放行 —— 财务修好阻塞原因（通常是期间已结账）后手工 `PATCH status=POSTED`
//! 即可补记，不会出现「审批过了却永远不能记账」的死局。
//!
//! # 表单快照
//!
//! 发起时把退款单的关键字段冻结进 `biz_flow_instance.form_data`
//! （[`form_data`]）。`amount` 必须是 **JSON 数字**：客户以后在流程定义里加金额分级条件边时，
//! `cmp` 对字符串会 `TypeMismatch` 并静默走兜底边 —— 与 `payable_payment::payment_form_data`
//! 同一个坑，同一个写法。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::receipt_refund::{self, RefundRow};
use crate::business::erp::separation;
use crate::business::flow::model as flow_model;
use crate::shared::error_code;

/// 收款退款审批的业务类型（`biz_flow_def.biz_type`，与 `V0297` 种子逐字一致）。
///
/// 与其它审批同命名法：`biz_type` 大写（`RECEIPT_REFUND`）、`flow_code` 小写
/// （`receipt_refund`，`ck_biz_flow_def_code` 要求 `^[a-z][a-z0-9_]{1,63}$`）。
pub const BIZ_TYPE: &str = "RECEIPT_REFUND";
/// 收款退款审批的流程编码。
pub const FLOW_CODE: &str = "receipt_refund";

/// 一条「已发布的收款退款流程」（发起审批要用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefundFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`RECEIPT_REFUND`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的收款退款流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按 `approval_gate::authorize_missing_flow`
/// 处理（默认 409，只有持 `business:erp:skip-approval` 且写明理由 ≥ 10 字才放行）。
/// 排序口径与 `payable_bridge::published_pay_flow` 一致
/// （`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间）。
pub async fn published_refund_flow() -> DbResult<Option<RefundFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'RECEIPT_REFUND' OR lower(d.flow_code) = 'receipt_refund') \
              ORDER BY (d.biz_type = 'RECEIPT_REFUND') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(RefundFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 过账闸：必须有已通过的审批实例 ====================

/// 这张退款单**最近一条**审批实例的摘要（详情页用）。
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

/// 查这张退款单最近一条审批实例（可放进事务）。
///
/// 「最近一条」按 `started_at DESC, created_at DESC`：一张退款单被驳回后重新提交会留下
/// 多条实例，前端要看的是**当前这一轮**。
pub async fn latest_approval_of(ex: &impl SqlExecutor, refund_id: Uuid) -> DbResult<Option<ApprovalBrief>> {
    // SCOPE-EXEMPT: 审批实例是流程运行时表，按 (biz_type, biz_id) 精确取最近 1 行；
    // 可见性由退款单本身的 detail 权限控制，不在这里按部门裁剪。
    let row = ex
        .query_first_sql(
            "SELECT i.id, i.instance_no, i.status, \
                    (SELECT count(*) FROM biz_flow_task t \
                      WHERE t.instance_id = i.id AND t.status = 'PENDING') AS pending_tasks \
               FROM biz_flow_instance i \
              WHERE i.biz_type = 'RECEIPT_REFUND' AND i.biz_id = ?::uuid \
                AND i.deleted_at IS NULL \
              ORDER BY i.started_at DESC, i.created_at DESC LIMIT 1"
                .to_string(),
            vec![Value::from(&refund_id)],
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

/// 一张「已通过」的审批实例（过账闸的判据 + 状态日志里的单号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedInstance {
    /// 实例 id。
    pub id: Uuid,
    /// 审批单号（写进 `biz_status_log.reason`，人工补记时也能追溯是哪一轮批的）。
    pub instance_no: String,
}

/// 这张退款单有没有**已通过**的审批实例（可放进事务）。
///
/// 过账闸只认这一个判据：`biz_type = 'RECEIPT_REFUND'` 且 `biz_id = 退款单 id` 且
/// `status = 'APPROVED'`。不认「最近一条实例是什么状态」—— 驳回后重新提交的场景里，
/// 最近一条可能还是 `PENDING`、而更早那条是 `APPROVED`，此时应当**放行**（钱已经被批过，
/// 只是又提了一轮重复的）。宁可放过重复提交，也不要让已经批过的款过不了账。
pub async fn approved_instance_of(ex: &impl SqlExecutor, refund_id: Uuid) -> DbResult<Option<ApprovedInstance>> {
    // SCOPE-EXEMPT: 同上，按 (biz_type, biz_id, status) 精确取 1 行。
    let row = ex
        .query_first_sql(
            "SELECT id, instance_no FROM biz_flow_instance \
              WHERE biz_type = 'RECEIPT_REFUND' AND biz_id = ?::uuid \
                AND status = 'APPROVED' AND deleted_at IS NULL \
              ORDER BY started_at DESC, created_at DESC LIMIT 1"
                .to_string(),
            vec![Value::from(&refund_id)],
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

/// 发起审批时冻结进实例的 `form_data`。
///
/// `amount` 用 [`json_number`] 转成 **JSON 数字**（不是 `"100.0000"` 这样的字符串）：
/// 客户以后在流程定义里加 `form.amount` 条件边时，`cmp` 对字符串是 `TypeMismatch`，
/// 会静默掉进兜底边（`payable_payment::payment_form_data` 同一个坑）。
///
/// `create_by` 是内控留痕用的（真正的拦截在 [`guard_self_approval`]），
/// 制单人已被清空的历史行写 `null`，不写空串冒充「查到了」。
pub fn form_data(row: &RefundRow) -> serde_json::Value {
    let mut form = serde_json::json!({
        "refund_id": row.id.to_string(),
        "refund_no": row.refund_no,
        "customer_id": row.customer_id.to_string(),
        "amount": json_number(&row.amount.to_string()),
        "amount_text": row.amount.to_string(),
        "allocated_amount": json_number(&row.allocated_amount.to_string()),
        "refund_date": row.refund_date.to_string(),
        "reason": row.reason,
        "create_by": row.create_by.map(|value| value.to_string()),
    });
    if let Some(receipt_id) = row.source_receipt_id
        && let Some(object) = form.as_object_mut()
    {
        object.insert(
            "source_receipt_id".to_string(),
            serde_json::Value::String(receipt_id.to_string()),
        );
    }
    form
}

/// 定点字符串 → JSON 数字（`"100.0000"` → `100.0`）。
///
/// 金额是定点数，4 位小数在「比较阈值」这个用途上转 `f64` 没有精度问题；
/// 解析失败给 `0`（与 `payable_payment::amount_number` 同口径：宁可看起来是 0，
/// 也不要让 `form_data` 里出现字符串把条件边打偏）。
fn json_number(decimal_text: &str) -> serde_json::Value {
    match decimal_text.parse::<f64>() {
        Ok(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|| serde_json::json!(0)),
        Err(_) => serde_json::json!(0),
    }
}

// ==================== 回调：审批结论 → 退款单 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefundVerdict {
    /// 同意 → 直接过账（生成 借 1122 / 贷 1002 凭证）。
    Post,
    /// 驳回 / 撤回 / 终止 → 退款单留在 `CONFIRMED`（账一分没动，可改完再提）。
    KeepConfirmed,
}

impl RefundVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            RefundVerdict::Post => "POST",
            RefundVerdict::KeepConfirmed => "KEEP_CONFIRMED",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动退款单）。
pub fn refund_verdict(instance_status: &str) -> Option<RefundVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(RefundVerdict::Post),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(RefundVerdict::KeepConfirmed),
        _ => None,
    }
}

/// 审批单落终态 → 推进退款单（`FlowCallback` 身份）。
///
/// | 实例状态 | 退款单 | 说明 |
/// |---|---|---|
/// | `APPROVED` | `CONFIRMED → POSTED` | 回调**直接过账**（生成退款凭证），不需要人再点一次 |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | 不动（仍是 `CONFIRMED`） | 账一分没动，可改完再提 |
/// | 其它（`PENDING` / 空） | 不动 | 还在跑 |
///
/// 返回受影响的退款单 id（没推进 / 不该推进 → `None`）。**不返回错误**：
/// 审批已落库，业务侧失败只记日志（与 `payable_bridge::on_payment_settled` 同口径）。
pub async fn on_refund_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = refund_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(refund_id)) => {
            tracing::info!(
                %instance_id,
                %refund_id,
                verdict = verdict.as_str(),
                "收款退款审批结论已回调退款单"
            );
            Some(refund_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "收款退款审批结论回调失败（不影响审批单，退款单仍是已确认）"
            );
            None
        }
    }
}

/// [`on_refund_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: RefundVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「收款退款」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过收款退款回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(refund_id) = instance.biz_id else {
        return Ok(None);
    };
    let approver = latest_completed_assignee(instance_id).await?;
    match verdict {
        RefundVerdict::Post => {
            // 审批人落进状态日志（`operator`）；过账本体与人工过账是**同一条代码路径**
            // （`change_status` → 过账闸 → 凭证），不另写一套记账逻辑。
            let reason = format!("审批通过（{}）自动过账", instance.instance_no);
            match receipt_refund::post_after_approval(refund_id, approver, &reason).await {
                Ok(receipt_refund::PostOutcome::Posted) => Ok(Some(refund_id)),
                Ok(receipt_refund::PostOutcome::AlreadyPosted) => {
                    tracing::info!(%refund_id, "退款单已过账（重复回调），跳过");
                    Ok(None)
                }
                Ok(receipt_refund::PostOutcome::NotPostable { status }) => {
                    tracing::warn!(
                        %refund_id,
                        %instance_id,
                        %status,
                        "审批已通过，但退款单不是「已确认」（可能已被别的路径改过）→ 不过账，等人工处置"
                    );
                    Ok(None)
                }
                Err(error) => {
                    tracing::warn!(
                        %refund_id,
                        code = error.code,
                        msg = %error.msg,
                        "审批已通过，但过账失败（退款单仍是已确认；修好阻塞原因后可手工过账补记）"
                    );
                    Ok(None)
                }
            }
        }
        RefundVerdict::KeepConfirmed => {
            // 不做任何状态迁移：`CONFIRMED → CONFIRMED` 不是合法边，硬写状态日志会污染
            // 状态机审计。驳回意见在审批中心的实例详情里，发起人也会收到流程引擎的通知。
            tracing::info!(
                %refund_id,
                %instance_id,
                "审批未通过（驳回 / 撤回 / 终止）：退款单仍是已确认，账一分没动"
            );
            Ok(Some(refund_id))
        }
    }
}

// ==================== 内控：制单人不能批自己的退款 ====================

/// 内控用：这张退款单的**制单人**（`biz_receipt_refund.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
/// 查不到单 / 单没有 `create_by` → `None`：只是没人被排除，**不影响**审批能不能发起。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(refund_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪（裁剪会让
    // 「看不到这张单的人」反而成为可审批人，与内控目的相反）。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_receipt_refund WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&refund_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("create_by"),
    }
}

/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）。
///
/// # 判据（任一命中即 403 + 65019）
///
/// 1. `operator` == `biz_receipt_refund.create_by`（**制单人**）；
/// 2. `operator` == 审批单的 `initiator_id`（发起人；代提场景下发起人 ≠ 制单人）。
///
/// 复用 `separation::Duty::Approve`（`erp.separation_of_duties.approve`，默认开）：
/// 与采购 / 收货 / 凭证红冲 / 付款 / 销售订单是**同一把内控开关**，
/// 待办生成侧的排除（[`internal_control_creator`]）读的也是它，两处不可能自相矛盾。
/// 与本业务无关的待办（其它 `biz_type`）直接放行，零影响。
pub async fn guard_self_approval(task_id: Uuid, operator: Uuid) -> std::result::Result<(), ApiError> {
    // SCOPE-EXEMPT: 审批待办 / 实例是流程运行时表，按主键精确取 1 行；数据范围由
    // 「这条待办是不是你的」这一层判定（engine 投票时的 403）负责，与本闸无关。
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
    // 「制单与审批分离」开关（默认开 = 内控闸；关掉 = 小团队允许本人自审）
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(refund_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = receipt_refund::find_by_id(db(), refund_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的收款退款：退款单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.refund_no,
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

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(raw: &str) -> Decimal {
        Decimal::from_str(raw).expect("夹具金额非法")
    }

    fn refund_row() -> RefundRow {
        RefundRow {
            id: Uuid::now_v7(),
            refund_no: "RR-20261106-000001".to_string(),
            customer_id: Uuid::now_v7(),
            source_receipt_id: None,
            refund_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 6).expect("日期非法"),
            amount: dec("100"),
            allocated_amount: dec("100"),
            method: Some("BANK_TRANSFER".to_string()),
            bank_account: None,
            status: receipt_refund::STATUS_CONFIRMED.to_string(),
            voucher_id: None,
            reason: "客户退货，货款原路退回".to_string(),
            remark: None,
            create_by: Some(Uuid::now_v7()),
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn biz_type_matches_the_doc_kind_registry() {
        // 审批轴 ↔ 对象轴：`RECEIPT_REFUND` 必须映射到 `DocKind::ReceiptRefund`
        // （`doc_kind_tests::cross_axis_mappings_are_consistent` 也会再核一遍）
        use crate::business::shared::doc_kind::{DocKind, FlowBizType};
        let parsed = FlowBizType::parse(BIZ_TYPE).expect("FlowBizType 必须登记 RECEIPT_REFUND");
        assert_eq!(parsed, FlowBizType::ReceiptRefund);
        assert_eq!(parsed.flow_code(), FLOW_CODE);
        assert_eq!(parsed.doc_kind(), Some(DocKind::ReceiptRefund));
    }

    #[test]
    fn instance_status_maps_to_the_two_verdicts() {
        assert_eq!(refund_verdict("APPROVED"), Some(RefundVerdict::Post));
        assert_eq!(refund_verdict(" approved "), Some(RefundVerdict::Post));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                refund_verdict(status),
                Some(RefundVerdict::KeepConfirmed),
                "{status} 必须只留痕、不动账"
            );
        }
        // 还在跑 / 不认识 → 一律不动
        assert_eq!(refund_verdict("PENDING"), None);
        assert_eq!(refund_verdict(""), None);
        assert_eq!(refund_verdict("WHATEVER"), None);
        assert_eq!(RefundVerdict::Post.as_str(), "POST");
        assert_eq!(RefundVerdict::KeepConfirmed.as_str(), "KEEP_CONFIRMED");
    }

    #[test]
    fn form_data_carries_amount_as_json_number() {
        let row = refund_row();
        let form = form_data(&row);
        assert!(
            form["amount"].is_number(),
            "amount 必须是 JSON 数字（字符串会让条件边 TypeMismatch 掉兜底）：{:?}",
            form["amount"]
        );
        assert_eq!(form["amount"].as_f64(), Some(100.0));
        assert_eq!(form["refund_no"], row.refund_no);
        assert_eq!(form["reason"], row.reason);
        assert_eq!(form["create_by"], row.create_by.unwrap().to_string());
        // 没有来源收款单时不写这个键（不写空串冒充「查到了」）
        assert!(form.get("source_receipt_id").is_none());

        let mut with_source = refund_row();
        with_source.source_receipt_id = Some(Uuid::now_v7());
        let form = form_data(&with_source);
        assert!(form.get("source_receipt_id").is_some());
    }

    #[test]
    fn json_number_handles_fixed_point_text() {
        assert_eq!(json_number("100.0000").as_f64(), Some(100.0));
        assert_eq!(json_number("0.5").as_f64(), Some(0.5));
        // 解析不了给 0，不漏成字符串
        assert!(json_number("not-a-number").is_number());
    }

    #[test]
    fn posted_and_confirmed_constants_come_from_the_module() {
        // 过账闸与回调都按这两个值比对；在这里钉住，防止有人改了常量而两边不同步
        assert_eq!(receipt_refund::STATUS_CONFIRMED, "CONFIRMED");
        assert_eq!(receipt_refund::STATUS_POSTED, "POSTED");
    }
}

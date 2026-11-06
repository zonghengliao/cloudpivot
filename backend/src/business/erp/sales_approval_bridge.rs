//! 销售订单审批的**业务回调 + 反向兜底**（销售链 SO-06）。
//!
//! 与 `purchase_bridge`（采购订单审批）、`stockin_bridge`（收货）、`payable_bridge`（付款）并列的
//! 一类业务回调：审批单落终态之后，把结论翻译成销售订单的状态。
//!
//! # 四个入口
//!
//! | 函数 | 谁调 | 干什么 |
//! |------|------|--------|
//! | [`on_so_settled`] | `flow::handler::{approve_task, reject_task, withdraw_instance}`（**审批事务已提交之后**） | `APPROVED` → `SUBMITTED → APPROVED`；`REJECTED` / `WITHDRAWN` / `CANCELLED` → `SUBMITTED → DRAFT`（打回，意见写 `remark`） |
//! | [`cancel_approval_of`] | 销售订单作废（`SUBMITTED → CANCELLED`）等「其它路径」 | 把在跑的审批**终止**（`biz_flow_task` / 节点 / 主表一起翻 `CANCELLED`） |
//! | [`guard_self_approval`] | `flow::handler::approve_task`（**事务之前**） | 制单人 / 发起人不能批自己那一票 → 403 |
//! | [`internal_control_creator`] | `flow::internal_control`（**生成待办之前**） | 把制单人从审批人列表里排除（落 `SKIPPED` 留痕） |
//!
//! # 状态口径（与 `state_machine::SALES_ORDER_EDGES` 逐条对应）
//!
//! ```text
//! DRAFT ──提交审批──▶ SUBMITTED ──审批通过──▶ APPROVED（此后才能出库）
//!                          └──驳回 / 撤回──▶ DRAFT（清实例与提交痕迹，意见追加 remark）
//!                          └──审批中作废──▶ CANCELLED（撤审批单）
//! ```
//!
//! `SUBMITTED → APPROVED` **只由本文件的回调写**；人工 `PATCH /status` 在
//! `sales_order_handler` 里被显式拒绝（400 + 指向审批中心）—— 否则「审批」只是画在纸上。
//!
//! # 时序与失败口径（与 `purchase_bridge` / `stockin_bridge` 同款）
//!
//! 回调跑在**审批事务提交之后**：审批结论已经落库，业务侧失败**不许**回滚审批单，
//! 只记 `warn` 并返回 `None`。单据停在 `SUBMITTED` 由人工或下一轮对账处置
//! （销售订单没有账，最坏情况是「审批过了、单据显示审批中」，不会产生错账）。
//!
//! # 金额分级在哪
//!
//! **不在本文件**：三档金额分流是流程定义里的条件节点（`V0281__sales_order_flow` 的
//! `c_low` / `c_mid` 两条 `form.amount` 条件边 + `biz_flow_node_assignee` 的角色指派）。
//! 本文件只负责「发起时把金额冻结进 `form_data`」与「结论怎么翻译成单据状态」。

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::sales_order_model::{
    self as model, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED,
};
use crate::business::erp::separation;
use crate::business::flow::model as flow_model;
use crate::business::flow::settle_guard;
use crate::shared::error_code;

/// 销售订单审批的业务类型（与 `V0281__sales_order_flow` 种子 / handler 同口径）。
///
/// 与采购单（`PO_APPROVE`）同一命名法：`biz_type` 大写、`flow_code` 小写
/// （`ck_biz_flow_def_code` 要求 `^[a-z][a-z0-9_]{1,63}$`，写不了大写，
/// 所以卡面的 `SO_APPROVE` 落成 **biz_type**，流程编码落 `so_approve`）。
pub const BIZ_TYPE: &str = "SO_APPROVE";
/// 销售订单审批的流程编码。
pub const FLOW_CODE: &str = "so_approve";

/// 已发布的销售订单审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`SO_APPROVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

/// 查「已发布的销售订单审批流程」。
///
/// `None` = **没配流程**（不是错误）：调用方按 `approval_gate::authorize_missing_flow`
/// 处理（默认 409，只有持 `business:erp:skip-approval` 且写明理由才放行）。
///
/// 排序口径与 `purchase_bridge::published_po_flow` 一致（`biz_type` 精确命中优先 →
/// `is_default` → 版本号 → 更新时间）。
pub async fn published_so_flow() -> DbResult<Option<SoFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行，
    // 不分页、不按部门裁剪（裁剪会导致「看不到自己的单据该走哪个流程」）。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'SO_APPROVE' OR lower(d.flow_code) = 'so_approve') \
              ORDER BY (d.biz_type = 'SO_APPROVE') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(SoFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

// ==================== 发起：单据表单快照 ====================

/// 发起审批时冻结进实例的 `form_data`（流程条件表达式的**唯一作用域**）。
///
/// **必须有 `amount`**：`so_approve` 的两条金额条件边写的是 `form.amount`
/// （`< 10000` / `< 100000`），键名对不上 → 条件恒不命中 → 所有单都掉进最后一档。
pub fn form_data(row: &model::SalesOrderRow) -> serde_json::Value {
    let amount = row.total_with_tax.round_dp(4).to_string();
    serde_json::json!({
        "order_id": row.id.to_string(),
        "order_no": row.so_no,
        // 条件边的作用域键（金额分级的唯一依据）
        "amount": json_number(&amount),
        "total_with_tax": row.total_with_tax.to_string(),
        "total_amount": row.total_amount.to_string(),
        "total_tax": row.total_tax.to_string(),
        "customer_id": row.customer_id.to_string(),
        // 内控留痕：审批人 ≠ 制单人（`guard_self_approval` 兜底）
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

// ==================== 回调：审批结论 → 销售订单 ====================

/// 审批结论的业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoVerdict {
    /// 同意 → `SUBMITTED → APPROVED`（可出库）。
    Approve,
    /// 驳回 / 撤回 / 终止 → `SUBMITTED → DRAFT`（打回，附意见）。
    BackToDraft,
}

impl SoVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            SoVerdict::Approve => "APPROVE",
            SoVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动销售订单）。
pub fn so_verdict(instance_status: &str) -> Option<SoVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(SoVerdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(SoVerdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进销售订单状态（`FlowCallback` 身份）。
///
/// | 实例状态 | 销售订单 | 说明 |
/// |---|---|---|
/// | `APPROVED` | `SUBMITTED → APPROVED` | 之后才可以出库 |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | `SUBMITTED → DRAFT` | 打回草稿，审批意见追加到 `remark` |
/// | 其它（`PENDING` / 空） | 不动 | 还在跑 |
///
/// 返回受影响的销售订单 id（没推进 / 不该推进 → `None`）。**不返回错误**：审批已落库，
/// 业务侧失败只记日志（与 `purchase_bridge::on_po_settled` 同口径）。
pub async fn on_so_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = so_verdict(instance_status)?;
    match settle(instance_id, verdict).await {
        Ok(Some(order_id)) => {
            tracing::info!(
                %instance_id,
                %order_id,
                verdict = verdict.as_str(),
                "销售订单审批结论已回调单据"
            );
            Some(order_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "销售订单审批结论回调失败（不影响审批单，单据仍是已提交）"
            );
            None
        }
    }
}

/// [`on_so_settled`] 的实现体（可失败，由调用方吞掉错误）。
async fn settle(instance_id: Uuid, verdict: SoVerdict) -> DbResult<Option<Uuid>> {
    // ① 审批单必须是「销售订单审批」类的（别的业务类型与本回调无关）
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过销售订单审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(order_id) = instance.biz_id else {
        return Ok(None);
    };
    // ② 单据必须存在（当前状态能不能收这个结论交给 ③ 的统一前置校验）
    let Some(row) = model::find_order(order_id).await? else {
        tracing::warn!(%order_id, "销售订单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    // ③ 审批人（落库用）：引擎把「谁点的」写在待办行，实例行没有该字段
    let approver = latest_completed_assignee(instance_id).await?;
    // ④ 审批意见（驳回时写进 remark，让人一眼看到为什么被打回）
    let comment = latest_reject_comment(instance_id).await?;
    let (to_status, reason) = match verdict {
        SoVerdict::Approve => (STATUS_APPROVED, "销售订单审批通过".to_string()),
        SoVerdict::BackToDraft => (
            STATUS_DRAFT,
            match comment.as_deref().map(str::trim) {
                Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
                _ => "审批驳回（未填写意见）".to_string(),
            },
        ),
    };
    // 销 C1（与 purchase_bridge 同款）：写目标状态前先校验销售订单当前状态还在不在
    // 「能接受这个审批结论」的合法路径上（状态机边 `SUBMITTED → APPROVED|DRAFT`）。
    if !settle_guard::gate(
        &instance,
        crate::business::shared::state_machine::Entity::SaleOrder,
        &row.so_no,
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
            crate::business::shared::state_machine::ENTITY_SALES_ORDER,
            order_id,
            &row.so_no,
            &model::LEGAL_EDGES,
            STATUS_SUBMITTED,
            to_status,
            crate::business::shared::state_machine::Operator::of(approver),
            Some(&reason),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        // 审批通过：实例 id 保留（审计）；驳回打回草稿：apply_status_with_instance 会清空
        let affected = model::apply_status_with_instance(
            &tx,
            order_id,
            STATUS_SUBMITTED,
            to_status,
            Some(&reason),
            approver,
            None,
            now,
        )
        .await
        .map_err(|error| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("销售订单状态回写失败：{error}"),
            )
        })?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "销售订单状态在审批回调期间已被改动，请刷新后重试",
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
                "销售订单审批结论回写失败"
            );
            Ok(None)
        }
    }
}

// ==================== 内控：制单人不能批自己的销售订单 ====================

/// 内控用：这张销售订单的**制单人**（`biz_sales_order.create_by`）。
///
/// 供 `flow::internal_control` 在**生成待办前**把制单人从审批人列表里排除
/// （落 `SKIPPED` 留痕，而不是让人看到待办再 403）。
/// 查不到单 / 单没有 `create_by` → `None`：只是没人被排除，**不影响**审批能不能发起。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(order_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪（裁剪会让
    // 「看不到这张单的人」反而成为可审批人，与内控目的相反）。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
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
/// 1. `operator` == `biz_sales_order.create_by`（**制单人**）；
/// 2. `operator` == 审批单的 `initiator_id`（发起人；代提场景下发起人 ≠ 制单人）。
///
/// # 为什么复用 `separation::Duty::Approve`
///
/// 「制单 ≠ 审批」在 ERP 里是**同一把内控开关**（`erp.separation_of_duties.approve`，
/// 默认开）：采购 / 收货 / 凭证红冲 / 应付付款与作废都用它，销售订单不另造一个键 ——
/// 否则管理员关掉内控时会出现「采购能自审、销售不能」的半开半关状态。
/// 待办生成侧的排除（`internal_control_creator`）读的也是同一个开关，两处不可能自相矛盾。
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
    // 「制单与审批分离」开关（默认开 = 内控闸；关掉 = 小团队允许本人自审）
    if !separation::enabled(separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(order_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find_order(order_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的销售订单：销售订单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.so_no,
                separation::approval_hint(separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
}

// ==================== 反向：终止在跑的审批 ====================

/// 终止某张销售订单的**在跑审批**（作废 / 删除时用，与调用方同一事务）。
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
            "销售订单在跑的审批已终止（不再留孤儿待办）"
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

/// 最近一条**驳回**意见（写进销售订单 `remark`，让人看到被打回的理由）。
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

    /// ① 实例状态 → 业务语义的映射口径（钉死：只有 APPROVED 会让单据进入可出库态）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(so_verdict("APPROVED"), Some(SoVerdict::Approve));
        assert_eq!(so_verdict(" approved "), Some(SoVerdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                so_verdict(status),
                Some(SoVerdict::BackToDraft),
                "{status} 必须打回草稿（不是关闭、不是取消）"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(so_verdict(status), None, "{status} 不该推进销售订单");
        }
    }

    /// ② 业务类型 / 流程编码必须与 V0281 种子同口径（错一个字就查不到流程）。
    #[test]
    fn biz_type_and_flow_code_match_seed() {
        assert_eq!(BIZ_TYPE, "SO_APPROVE");
        assert_eq!(FLOW_CODE, "so_approve");
        assert_eq!(BIZ_TYPE, BIZ_TYPE.to_uppercase());
        // `ck_biz_flow_def_code` 只认小写：flow_code 写大写会被库直接拒
        assert!(
            FLOW_CODE
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
            "flow_code 必须满足 ^[a-z][a-z0-9_]{{1,63}}$（大写会被 ck_biz_flow_def_code 拒）"
        );
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
            model::can_transition(STATUS_SUBMITTED, STATUS_APPROVED),
            "SUBMITTED → APPROVED 必须合法，否则审批通过的回调会 409"
        );
        assert!(
            model::can_transition(STATUS_SUBMITTED, STATUS_DRAFT),
            "SUBMITTED → DRAFT 必须合法，否则驳回打回会 409"
        );
        assert!(
            model::can_transition(STATUS_DRAFT, STATUS_SUBMITTED),
            "DRAFT → SUBMITTED 必须合法，否则打回后无法重新提交"
        );
        assert!(
            !model::can_transition(STATUS_APPROVED, STATUS_DRAFT),
            "已审批的单据不能被打回草稿（否则已生效的承诺被悄悄撤销）"
        );
    }

    /// ⑤ `form.amount` 是流程条件边的唯一作用域键：`form_data` 必须把它写成 JSON 数字。
    #[test]
    fn form_data_exposes_numeric_amount() {
        let row = model::SalesOrderRow {
            id: Uuid::now_v7(),
            so_no: "SO-20261106-000001".to_string(),
            customer_id: Uuid::now_v7(),
            order_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(),
            delivery_date: None,
            status: STATUS_DRAFT.to_string(),
            total_amount: "88495.5752".parse().unwrap(),
            total_tax: "11504.4248".parse().unwrap(),
            total_with_tax: "100000.0000".parse().unwrap(),
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            create_by: None,
            update_by: None,
            deleted_at: None,
        };
        let form = form_data(&row);
        assert!(
            form["amount"].is_number(),
            "form.amount 必须是 JSON 数字（字符串会让条件边按字符串比较）：{}",
            form["amount"]
        );
        assert_eq!(form["amount"].as_f64(), Some(100000.0));
        assert_eq!(form["order_no"], "SO-20261106-000001");
        assert_eq!(form["total_with_tax"], "100000.0000");
        // 条件边的阈值（与 V0281 逐字一致，改流程必须同时改这里）
        assert_eq!(form["amount"].as_f64(), Some(100000.0));
    }
}

// ==================== 真库单测（三档金额分别发起流程 + 审批通过 / 驳回） ====================

/// 验收口径（卡面）：迁移 APPLY_OK + 三档金额分别发起流程 + 审批通过状态正确流转。
///
/// 没设 `ERP_DB_URL` 时**逐条 SKIP**（打印原因，不假绿）：
/// `ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so06' cargo test --lib -- \
///  business::erp::sales_approval_bridge --test-threads=1`
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::business::erp::approval_gate::ApprovalGate;
    use crate::business::erp::sales_order_handler;
    use crate::business::flow::engine;
    use nik::prelude::chrono::NaiveDate;

    const CUST_CODE: &str = "SO6HT-CUST";
    const MAKER: &str = "d6281000-0000-4000-8000-000000000001";

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

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场：审批单（待办 / 节点 / 主表）→ 状态日志 → 单据（明细 CASCADE）→ 夹具客户 / 用户。
    async fn cleanup() {
        let statements = [
            "DELETE FROM biz_flow_task WHERE instance_id IN ( \
                 SELECT id FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE')",
            "DELETE FROM biz_flow_instance_node WHERE instance_id IN ( \
                 SELECT id FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE')",
            "DELETE FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE'",
            "DELETE FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id IN ( \
                 SELECT o.id FROM biz_sales_order o JOIN biz_customer c ON c.id = o.customer_id \
                  WHERE c.code = 'SO6HT-CUST')",
            "DELETE FROM biz_sales_order WHERE customer_id IN \
                 (SELECT id FROM biz_customer WHERE code = 'SO6HT-CUST')",
            "DELETE FROM biz_customer WHERE code = 'SO6HT-CUST'",
            "DELETE FROM sys_user_role WHERE user_id = 'd6281000-0000-4000-8000-000000000001'::uuid",
            "DELETE FROM sys_user WHERE id = 'd6281000-0000-4000-8000-000000000001'::uuid",
        ];
        for sql in statements {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("清理夹具失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 夹具：客户（不控额度）+ 制单人（**不是**超管，避免与 fallback 审批人重合）+ 一张单。
    async fn fixture_order(amount: &str) -> Uuid {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, 'SO-06 审批夹具客户', 0, 'ACTIVE') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(CUST_CODE)],
        )
        .await
        .expect("插入夹具客户");
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES ('d6281000-0000-4000-8000-000000000001'::uuid, 'so06_maker', 'x', \
                     'SO-06 制单人', 1) ON CONFLICT (id) DO NOTHING",
            vec![],
        )
        .await
        .expect("插入夹具用户");
        let customer_id: Uuid = db()
            .scalar_with_params(
                "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
                vec![Value::from(CUST_CODE)],
            )
            .await
            .expect("查夹具客户");
        let order_id = Uuid::now_v7();
        let so_no = format!("SO6HT-{}", &order_id.to_string()[24..]);
        db().execute_with_params(
            "INSERT INTO biz_sales_order \
                 (id, so_no, customer_id, order_date, status, total_amount, total_tax, \
                  total_with_tax, create_by) \
             VALUES (?::uuid, ?, ?::uuid, ?::date, 'DRAFT', ?::text::numeric, 0, ?::text::numeric, ?::uuid)",
            vec![
                Value::from(&order_id),
                Value::from(so_no),
                Value::from(&customer_id),
                Value::from(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()),
                Value::from(amount),
                Value::from(amount),
                Value::from(Uuid::parse_str(MAKER).unwrap()),
            ],
        )
        .await
        .expect("插入夹具销售订单");
        db().execute_with_params(
            "INSERT INTO biz_sales_order_item \
                 (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount) \
             VALUES (?::uuid, 1, gen_random_uuid(), 1, ?::text::numeric, ?::text::numeric, 0, 0)",
            vec![
                Value::from(&order_id),
                Value::from(amount),
                Value::from(amount),
            ],
        )
        .await
        .expect("插入夹具明细");
        order_id
    }

    fn maker() -> Uuid {
        Uuid::parse_str(MAKER).unwrap()
    }

    async fn admin_id() -> Uuid {
        db().scalar::<Uuid>(
            "SELECT id FROM sys_user WHERE username = 'admin' AND deleted_at IS NULL LIMIT 1",
        )
        .await
        .expect("查 admin")
    }

    /// 当前在跑的待办（按创建顺序）→ `(task_id, node_key)`。
    async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, String)> {
        let rows = db()
            .find_with_params(
                "SELECT t.id AS task_id, t.node_key \
                   FROM biz_flow_task t \
                  WHERE t.instance_id = ?::uuid AND t.status = 'PENDING' \
                  ORDER BY t.created_at, t.node_key",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查待办");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed::<Uuid>("task_id").expect("task id"),
                    row.get_typed::<String>("node_key").expect("node key"),
                )
            })
            .collect()
    }

    async fn approve_task(task_id: Uuid, admin: Uuid) -> String {
        let out = db()
            .transaction(
                |tx| async move { engine::approve(&tx, task_id, admin, Some("同意")).await },
            )
            .await
            .expect("审批同意");
        // 与 flow::handler::approve_task 同款：审批事务提交后再回调业务
        on_so_settled(out.instance_id, &out.instance_status).await;
        out.instance_status
    }

    async fn status_of(order_id: Uuid) -> (String, Option<Uuid>) {
        let row = db()
            .find_with_params(
                "SELECT status, approval_instance_id FROM biz_sales_order WHERE id = ?::uuid",
                vec![Value::from(&order_id)],
            )
            .await
            .expect("查销售订单");
        let row = row.first().expect("单据存在");
        (
            row.get_typed("status").expect("status"),
            row.get_typed("approval_instance_id").expect("instance"),
        )
    }

    /// ★ 卡面验收：三档金额分别发起流程，审批通过后 → APPROVED。
    #[tokio::test]
    async fn three_tiers_start_flow_and_approve_to_approved() {
        let Some(_guard) = setup().await else {
            return;
        };
        let admin = admin_id().await;
        // (金额, 期望串行节点数) —— < 1 万 / 1–10 万 / ≥ 10 万
        for (amount, hops) in [("5000.0000", 1usize), ("50000.0000", 2), ("150000.0000", 3)] {
            let order_id = fixture_order(amount).await;
            let res = sales_order_handler::submit_approval(
                Some(maker()),
                &order_id.to_string(),
                &ApprovalGate::none(),
            )
            .await
            .expect("提交审批成功（配了 so_approve 流程就该建实例）");
            assert_eq!(
                res.status, STATUS_SUBMITTED,
                "金额 {amount} 提交后必须是 SUBMITTED"
            );
            let (status, instance_id) = status_of(order_id).await;
            assert_eq!(status, STATUS_SUBMITTED);
            let instance_id = instance_id.expect("提交后必须挂审批实例");

            // 逐节点审批：每次只应有 1 条 PENDING（串行链，不是会签）
            for hop in 0..hops {
                let tasks = pending_tasks(instance_id).await;
                assert_eq!(
                    tasks.len(),
                    1,
                    "金额 {amount} 第 {} 跳应只有 1 条待办（串行审批），实际 {:?}",
                    hop + 1,
                    tasks
                );
                let (task_id, node_key) = tasks[0].clone();
                let expected_node = match hop {
                    0 => "n_sales",
                    1 => "n_finance",
                    _ => "n_gm",
                };
                assert_eq!(
                    node_key,
                    expected_node,
                    "金额 {amount} 第 {} 跳节点不对",
                    hop + 1
                );
                let instance_status = approve_task(task_id, admin).await;
                let expect_final = hop + 1 == hops;
                if expect_final {
                    assert_eq!(instance_status, "APPROVED");
                } else {
                    assert_eq!(instance_status, "PENDING", "未走完的实例必须还在跑");
                }
            }
            let (status, instance_id) = status_of(order_id).await;
            assert_eq!(
                status, STATUS_APPROVED,
                "金额 {amount} 审批通过后必须 APPROVED"
            );
            assert!(
                instance_id.is_some(),
                "APPROVED 后实例 id 保留作审计（详情页要能显示审批单号）"
            );
        }
    }

    /// ★ 驳回 → 退回 DRAFT（清实例 + 意见写 remark）。
    #[tokio::test]
    async fn reject_returns_order_to_draft() {
        let Some(_guard) = setup().await else {
            return;
        };
        let admin = admin_id().await;
        let order_id = fixture_order("150000.0000").await;
        sales_order_handler::submit_approval(
            Some(maker()),
            &order_id.to_string(),
            &ApprovalGate::none(),
        )
        .await
        .expect("提交审批成功");
        let (_, instance_id) = status_of(order_id).await;
        let instance_id = instance_id.expect("实例 id");
        let tasks = pending_tasks(instance_id).await;
        let (task_id, node_key) = tasks.first().cloned().expect("第一跳待办");
        assert_eq!(node_key, "n_sales");
        let out = db()
            .transaction(|tx| async move {
                engine::reject(
                    &tx,
                    task_id,
                    admin,
                    "金额与合同不符，退回重报",
                    engine::ReturnTo::Final,
                )
                .await
            })
            .await
            .expect("审批驳回");
        assert_eq!(out.instance_status, "REJECTED");
        on_so_settled(out.instance_id, &out.instance_status).await;
        let (status, instance_id) = status_of(order_id).await;
        assert_eq!(status, STATUS_DRAFT, "驳回必须退回 DRAFT（不是 CANCELLED）");
        assert!(
            instance_id.is_none(),
            "回草稿要清 approval_instance_id（否则下次提交撞 uk_biz_flow_instance_biz_active）"
        );
        let remark: Option<String> = db()
            .scalar_with_params(
                "SELECT remark FROM biz_sales_order WHERE id = ?::uuid",
                vec![Value::from(&order_id)],
            )
            .await
            .expect("查备注");
        let remark = remark.unwrap_or_default();
        assert!(
            remark.contains("审批驳回") && remark.contains("金额与合同不符"),
            "驳回意见必须写进 remark（实际：{remark}）"
        );
    }

    /// ★ 内控：制单人不能批自己的销售订单（403 + 65019）。
    #[tokio::test]
    async fn creator_cannot_approve_own_order() {
        let Some(_guard) = setup().await else {
            return;
        };
        let order_id = fixture_order("5000.0000").await;
        sales_order_handler::submit_approval(
            Some(maker()),
            &order_id.to_string(),
            &ApprovalGate::none(),
        )
        .await
        .expect("提交审批成功");
        let (_, instance_id) = status_of(order_id).await;
        let tasks = pending_tasks(instance_id.expect("实例 id")).await;
        let (task_id, _) = tasks.first().cloned().expect("待办");
        let err = guard_self_approval(task_id, maker())
            .await
            .expect_err("制单人批自己的单必须被拒");
        assert_eq!(err.status, 403);
        assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
    }

    /// ★ 没配流程 → 409 默认拒绝（本卡种了流程，所以直接把流程下线再试，测完恢复）。
    #[tokio::test]
    async fn missing_flow_is_rejected_by_default() {
        let Some(_guard) = setup().await else {
            return;
        };
        let order_id = fixture_order("5000.0000").await;
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'DRAFT' WHERE flow_code = 'so_approve'",
            vec![],
        )
        .await
        .expect("临时下线流程");
        let err = sales_order_handler::submit_approval(
            Some(maker()),
            &order_id.to_string(),
            &ApprovalGate::none(),
        )
        .await
        .expect_err("没配流程必须 409 默认拒绝");
        db().execute_with_params(
            "UPDATE biz_flow_def SET status = 'PUBLISHED' WHERE flow_code = 'so_approve'",
            vec![],
        )
        .await
        .expect("恢复流程");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::APPROVAL_FLOW_NOT_CONFIGURED);
        let (status, instance_id) = status_of(order_id).await;
        assert_eq!(status, STATUS_DRAFT, "被拒的提交不能把单据推成 SUBMITTED");
        assert!(instance_id.is_none());
    }
}

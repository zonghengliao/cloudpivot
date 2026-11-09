//! 盘点单**审批**的业务回调 + 内控 + 反向兜底（ARCH-3 §5 **DC-14**）。
//!
//! 与 `purchase_bridge` / `contract_bridge` / `stockin_bridge` 并列的一类业务回调：
//! 审批单落终态之后，把结论翻译成盘点单的状态。**彼此互不影响**（同一条实例按
//! `biz_type` 分流，别的业务的桥直接 `return`）。
//!
//! # 状态语义（本卡的关键差别）
//!
//! 合同是「审批通过即生效」，盘点是「审批通过**才允许过账**」：
//!
//! ```text
//! DRAFT --提交审批--> PENDING_POST --审批通过(实例 APPROVED)--> PENDING_POST（等过账）
//!                          │                                      │
//!                          │                                      └─过账--> POSTED
//!                          └--驳回/撤回/终止--> DRAFT（清实例，可改实盘数后重提）
//! ```
//!
//! 所以 `APPROVED` 这一票**不改单据状态**（单据本来就在 `PENDING_POST`），只留一条
//! `tracing::info!`；真正拦住「没过审就过账」的是过账入口对实例状态的硬校验
//! （`stocktake_handler::ensure_approved`）—— 审批结论是**证据**，不是**触发**。
//!
//! # 反向兜底
//!
//! 草稿删单 / 待过账撤回 / 待过账作废时，`approval_instance_id` 是弱引用：
//! 实例还在跑就会留下**永远等不到回调的孤儿实例**，并且 `uk_biz_flow_instance_biz_active`
//! 会把这张单**下一次提交审批**永久堵死（409 且无法自愈）。所以这三条路径都必须
//! 调 [`cancel_approval_of`] 终止实例。

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::stocktake_model::{self as model, STATUS_DRAFT, STATUS_PENDING_POST};
use crate::business::flow::model as flow_model;
use crate::business::flow::settle_guard;
use crate::business::shared::state_machine;
use crate::shared::error_code;

/// 盘点审批的业务类型（与 `V0211` 种子 / 提交入口同口径）。
pub const BIZ_TYPE: &str = "STOCK_TAKE_POST";
/// 盘点审批的流程编码（**一个字都不能错**：查不到流程 = 提交入口按 gate 口径处理）。
pub const FLOW_CODE: &str = "stock_take_post";

/// 已发布的盘点审批流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockTakeFlow {
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

/// 查「已发布的盘点审批流程」。`None` = **没配流程**（由调用方按 `approval_gate` 处理）。
///
/// 排序口径与 `purchase_bridge::published_po_flow` / `contract_bridge` 完全一致。
pub async fn published_stock_take_flow() -> DbResult<Option<StockTakeFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = 'STOCK_TAKE_POST' OR lower(d.flow_code) = 'stock_take_post') \
              ORDER BY (d.biz_type = 'STOCK_TAKE_POST') DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(StockTakeFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 实例状态 → 业务语义（纯函数，便于单测钉住映射口径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeVerdict {
    /// 同意：单据留在 `PENDING_POST`（等过账），不改状态。
    Approve,
    /// 驳回 / 撤回 / 终止：`PENDING_POST → DRAFT`（清实例，可改后重提）。
    BackToDraft,
}

impl TakeVerdict {
    /// 落日志用的短名。
    pub const fn as_str(self) -> &'static str {
        match self {
            TakeVerdict::Approve => "APPROVE",
            TakeVerdict::BackToDraft => "BACK_TO_DRAFT",
        }
    }
}

/// 实例状态 → 业务语义。`None` = 还在跑 / 不认识（一律不动单据）。
pub fn take_verdict(instance_status: &str) -> Option<TakeVerdict> {
    match instance_status.trim().to_ascii_uppercase().as_str() {
        "APPROVED" => Some(TakeVerdict::Approve),
        "REJECTED" | "WITHDRAWN" | "CANCELLED" => Some(TakeVerdict::BackToDraft),
        _ => None,
    }
}

/// 审批单落终态 → 推进盘点单状态（`FlowCallback` 身份）。
///
/// | 实例状态 | 盘点单 | 说明 |
/// |---|---|---|
/// | `APPROVED` | 不动（仍是 `PENDING_POST`） | 审批通过只是**允许过账**，过账要另一次点击 |
/// | `REJECTED` / `WITHDRAWN` / `CANCELLED` | `PENDING_POST → DRAFT` | 回位草稿，实例清空，可改实盘数后重提 |
/// | 其它 | 不动 | 还在跑 |
///
/// 返回受影响的盘点单 id。**不返回错误**：审批已落库，业务侧失败只记日志
/// （与 `purchase_bridge::on_po_settled` 同口径），单据停在 `PENDING_POST` 由人处理。
pub async fn on_stock_take_settled(instance_id: Option<Uuid>, instance_status: &str) -> Option<Uuid> {
    let instance_id = instance_id?;
    let verdict = take_verdict(instance_status)?;
    if verdict == TakeVerdict::Approve {
        // 通过不落状态：单据已在 PENDING_POST，过账入口会校验实例是 APPROVED。
        tracing::info!(
            %instance_id,
            "盘点审批通过：单据保持「待过账」，等待持过账权限的人过账"
        );
        return None;
    }
    match back_to_draft(instance_id).await {
        Ok(Some(take_id)) => {
            tracing::info!(
                %instance_id,
                %take_id,
                verdict = verdict.as_str(),
                "盘点审批结论已回调单据（打回草稿）"
            );
            Some(take_id)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                %instance_id,
                verdict = verdict.as_str(),
                %error,
                "盘点审批结论回调失败（不影响审批单，盘点单仍是待过账）"
            );
            None
        }
    }
}

/// 打回草稿的实现体（可失败，由调用方吞掉错误）。
async fn back_to_draft(instance_id: Uuid) -> DbResult<Option<Uuid>> {
    let Some(instance) = flow_model::find_instance(instance_id).await? else {
        tracing::debug!(%instance_id, "审批单不存在，跳过盘点审批回调");
        return Ok(None);
    };
    if instance.biz_type != BIZ_TYPE {
        return Ok(None);
    }
    let Some(take_id) = instance.biz_id else {
        return Ok(None);
    };
    let Some(row) = model::find(db(), take_id).await? else {
        tracing::warn!(%take_id, "盘点单不存在或已删除，跳过审批回调");
        return Ok(None);
    };
    let approver = latest_completed_assignee(instance_id).await?;
    let comment = latest_reject_comment(instance_id).await?;
    let reason = match comment.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => format!("审批驳回：{text}"),
        _ => "审批驳回 / 撤回（未填写意见）".to_string(),
    };
    // 「现在还能不能收这个结论」交给统一前置校验（状态机 + 留痕 + WARN）
    if !settle_guard::gate(
        &instance,
        state_machine::Entity::StockTake,
        &row.take_no,
        &row.status,
        STATUS_PENDING_POST,
        STATUS_DRAFT,
    )
    .await
    {
        return Ok(None);
    }
    let tx = db().begin().await?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_TAKE,
            take_id,
            &row.take_no,
            &model::LEGAL_EDGES,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            approver,
            Some(&reason),
            Some((BIZ_TYPE, instance.instance_no.as_str())),
        )
        .await?;
        let affected = model::apply_status(
            &tx,
            take_id,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            Some(&reason),
            approver,
            Utc::now(),
        )
        .await
        .map_err(|error| ApiError::new(500, error_code::INTERNAL_ERROR, format!("盘点单状态回写失败：{error}")))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "盘点单状态在审批回调期间已被改动，请刷新后重试",
            ));
        }
        // 回位必须清空实例，否则下次提交会撞 uk_biz_flow_instance_biz_active
        model::link_approval_instance(&tx, take_id, None, Utc::now())
            .await
            .map_err(|error| {
                ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!("清空盘点审批实例失败：{error}"),
                )
            })?;
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit().await?;
            Ok(Some(take_id))
        }
        Err(error) => {
            let _ = tx.rollback().await;
            tracing::warn!(
                %take_id,
                code = error.code,
                msg = %error.msg,
                "盘点审批结论回写失败"
            );
            Ok(None)
        }
    }
}

/// 把某张盘点单**在跑的审批**终止掉（`biz_flow_task` / 节点 / 主表一起翻 `CANCELLED`）。
///
/// 谁调：待过账撤回 / 待过账作废 / 草稿删单（三处都必须终止，否则留孤儿实例）。
pub async fn cancel_approval_of(
    ex: &impl SqlExecutor,
    take_id: Uuid,
    take_no: &str,
    reason: &str,
) -> DbResult<Option<u64>> {
    let cancelled = flow_model::cancel_pending_of_biz(ex, take_id, take_no, reason).await?;
    if cancelled.instances > 0 {
        tracing::info!(
            %take_id,
            take_no = take_no,
            instances = cancelled.instances,
            tasks = cancelled.tasks,
            reason = reason,
            "盘点单在跑的审批已终止（不再留孤儿待办）"
        );
        Ok(Some(cancelled.instances))
    } else {
        Ok(None)
    }
}

/// 审批入口的内控闸（**事务之前**调，越早拒绝越好）：制单人不能批自己提交的盘点单。
///
/// 与 `contract_bridge::guard_self_approval` 同款：流程节点写 `self_approval=ALLOW`
/// （待办照常可见、留痕），真正的否决在这里（403 + 65019）。
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
    if !crate::business::erp::separation::enabled(crate::business::erp::separation::Duty::Approve).await {
        return Ok(());
    }
    let Some(take_id) = instance.biz_id else {
        return Ok(());
    };
    let Ok(Some(row)) = model::find(db(), take_id).await else {
        return Ok(());
    };
    if row.create_by == Some(operator) || instance.initiator_id == operator {
        return Err(ApiError::new(
            403,
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN,
            format!(
                "不能审批自己提交的盘点单：盘点单 {} 由你本人提交（内控要求制单与审批分离），\
                 请交给其他有审批权限的账号处理{}",
                row.take_no,
                crate::business::erp::separation::approval_hint(crate::business::erp::separation::Duty::Approve)
            ),
        ));
    }
    Ok(())
}

/// 内控用：这张盘点单的**制单人**（供 `flow::internal_control` 生成待办前排除）。
pub async fn internal_control_creator(biz_id: Option<Uuid>) -> DbResult<Option<Uuid>> {
    let Some(take_id) = biz_id else {
        return Ok(None);
    };
    // SCOPE-EXEMPT: 内控按主键取 1 行的 create_by，不做数据范围裁剪。
    let rows = db()
        .find_with_params(
            "SELECT create_by FROM biz_stock_take WHERE id = ?::uuid",
            vec![Value::from(&take_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(row.get_typed("create_by")?),
    }
}

// ==================== 小工具：审批人 / 驳回意见 ====================

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

/// 最近一条**驳回**意见（写进 `biz_status_log.reason`）。
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

    /// ① 实例状态 → 业务语义（钉死：只有 REJECTED/WITHDRAWN/CANCELLED 会把单子打回草稿）。
    #[test]
    fn verdict_maps_only_final_states() {
        assert_eq!(take_verdict("APPROVED"), Some(TakeVerdict::Approve));
        assert_eq!(take_verdict(" approved "), Some(TakeVerdict::Approve));
        for status in ["REJECTED", "WITHDRAWN", "CANCELLED"] {
            assert_eq!(
                take_verdict(status),
                Some(TakeVerdict::BackToDraft),
                "{status} 必须打回草稿"
            );
        }
        for status in ["PENDING", "", "APPROVED2", "DRAFT"] {
            assert_eq!(take_verdict(status), None, "{status} 不该推进盘点单");
        }
    }

    /// ② 业务类型 / 流程编码 / 回位边必须与状态机**逐字对齐**。
    #[test]
    fn verdict_target_is_legal_stock_take_edge() {
        assert_eq!(BIZ_TYPE, "STOCK_TAKE_POST");
        assert_eq!(FLOW_CODE, "stock_take_post");
        let entity = state_machine::Entity::StockTake;
        assert!(
            entity.can_transition(STATUS_PENDING_POST, STATUS_DRAFT),
            "PENDING_POST → DRAFT 必须在盘点边表里（回调会 409）"
        );
        // 「待过账 → 已过账」这条边**存在**，但只有过账入口能走（要过审批结论闸 + 三本账）
        assert!(entity.can_transition(STATUS_PENDING_POST, model::STATUS_POSTED));
        // 草稿不能直接过账：盘点必须先审批再过账（DC-14 要堵的正是「没人批就改账」）
        assert!(!entity.can_transition(STATUS_DRAFT, model::STATUS_POSTED));
    }
}

// ============================================================================
// transfer_handler_status.rs —— 状态流转（六态状态机）：审核 PENDING / 发出 APPROVED→IN_TRANSIT / 在途 IN_TRANSIT / 收货 DONE（含结算）/ 作废 CANCELLED —— 六个目标态都在 change_transfer_status 一张函数里，按目标态切函数会改逻辑，故整条状态机落在本部件
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::transfer_handler_common::*;
use super::transfer_handler_query::*;
use super::transfer_linkage;
use crate::business::erp::form_snapshot;
use crate::business::erp::separation;
use crate::business::erp::transfer_dto::{TransferDetailRes, TransferStatusReq};
use crate::business::erp::transfer_model::{
    self, ALL_STATUSES, STATUS_PENDING, SettleError, TransferRow, transition_effect,
    transition_rejected_message,
};
use crate::business::flow::linkage;
use crate::business::flow::{engine, model as flow_model, task_bridge as flow_task_bridge};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;
use axum::Json;
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 一次流转的全部上下文（事务内用；字段多，收成结构体而不是 8 个参数）。
pub(super) struct TransitionContext<'a> {
    id: Uuid,
    from: &'a str,
    transfer_no: &'a str,
    effect: &'a transfer_model::TransitionEffect,
    cancel_reason: Option<&'a str>,
    transit_warehouse_id: Option<Uuid>,
    operator: Uuid,
    now: DateTime<Utc>,
    /// 调出仓库（`DONE` 时结算要读它：库存扣减 + 流水挂仓）。
    from_warehouse_id: Uuid,
    /// 调入仓库（`DONE` 时结算要读它：库存增加 + 流水挂仓）。
    to_warehouse_id: Uuid,
    /// 制单人（联动上下文的 `creator`：通知默认收件人 / `recipient = CREATOR` 策略）。
    creator: Option<Uuid>,
    /// 提交审批时要发起的审批单参数（`None` = 没配流程 / 其它目标态）。
    start: Option<engine::StartReq>,
}

/// 一次流转的落库结果（`apply_transition` → `change_transfer_status`）。
///
/// `linkage.deferred` 是**提交后**才跑的联动动作（发通知 / 建任务），必须由调用方在
/// `commit()` 成功之后交给 [`transfer_linkage::run_deferred_after_commit`]：
/// 在事务里发通知，一旦回滚就是「用户看到通知、单子没动」。
pub(super) struct TransitionOutcome {
    /// 受影响行数（`0` = CAS 失败，由调用方翻 409）。
    pub affected: u64,
    /// 本次状态变更触发的联动结果。
    pub linkage: linkage::LinkageRun,
}

/// 事务内的状态写入（`affected = 0` 表示 CAS 失败，由调用方翻 409）。
///
/// `IN_TRANSIT → DONE` 时**同事务**再做库存双写（[`transfer_model::settle_transfer`]）：
/// 结算失败就把整笔（含刚写的 `DONE`）一起回滚，调拨单仍是 `IN_TRANSIT`。
///
/// 最后一步（⑥ 跨单据联动，LNK-01）：事务内跑通用引擎但**排除调拨专用落账动作**
/// （库存 / 流水 / 凭证由 `settle_transfer` 或多个转移路径自己负责，
/// 见 `linkage::spec::TRANSFER_EXCLUDED_ACTIONS`），提交后动作交给调用方。
pub(super) async fn apply_transition(
    tx: &Transaction,
    ctx: &TransitionContext<'_>,
) -> std::result::Result<TransitionOutcome, SettleError> {
    // P10 铁律 ③：状态变更先过唯一入口（边表 + reason 校验 + 写 biz_status_log），
    // 与「写状态 / 结算库存 / 发起审批」同一事务。CAS 落空时上层整笔回滚，日志不留下。
    state_machine::transition(
        tx,
        state_machine::ENTITY_TRANSFER,
        ctx.id,
        ctx.transfer_no,
        &transfer_model::LEGAL_EDGES,
        ctx.from,
        ctx.effect.status,
        ctx.operator,
        ctx.cancel_reason,
        None,
    )
    .await
    .map_err(|error| {
        SettleError::Db(DbError::InvalidValue(format!(
            "[{}] {}",
            error.code, error.msg
        )))
    })?;
    let mut approval_instance_id = None;
    if let Some(start) = ctx.start.as_ref() {
        // ⚠️ 引擎的业务拒绝（65018 派不出审批人 / 65019 自审被禁）必须**当硬错误**：
        //    `engine::start` 拒绝时已经写了一张 PENDING 实例却 0 行待办，正常返回后
        //    本函数会把调拨单落 PENDING 并挂上这张批不动的实例 —— 批不了（无待办）、
        //    重提不了（门闸认为审批在跑）、撤不干净（卡 t_0127714a 同根因）。
        //    `start_in_tx` 把拒绝翻成 `SettleError::Approval`（带原始 ApiError + 65018
        //    结构化载荷）⇒ 调用方整笔回滚，调拨单保持提交前状态。
        let out = engine::start_in_tx(tx, start)
            .await
            .map_err(|failure| match failure {
                engine::StartFailure::Rejected(error) => SettleError::Approval(error),
                engine::StartFailure::Db(error) => SettleError::Db(error),
            })?;
        approval_instance_id = Some(out.instance_id.unwrap_or(start.instance_id));
        tracing::info!(
            transfer_id = %ctx.id,
            transfer_no = %ctx.transfer_no,
            instance_id = ?approval_instance_id,
            instance_status = %out.instance_status,
            flow_code = %start.flow_code,
            "提交调拨单已自动发起审批单"
        );
    }
    let affected = transfer_model::apply_status(
        tx,
        ctx.id,
        ctx.from,
        ctx.effect,
        ctx.cancel_reason,
        ctx.transit_warehouse_id,
        Some(ctx.operator),
        approval_instance_id,
        Some(ctx.operator),
        ctx.now,
    )
    .await?;
    // 撤回：调拨单从 PENDING 变 CANCELLED 时把在跑的审批单一起终止（否则
    // uk_biz_flow_instance_biz_active 会让「取消后重新提交」永久 409）
    if affected > 0 && ctx.effect.cancelled && ctx.from == transfer_model::STATUS_PENDING {
        let cancelled = flow_model::cancel_pending_of_biz(
            tx,
            ctx.id,
            ctx.transfer_no,
            "调拨单已撤回，审批单自动终止",
        )
        .await?;
        if cancelled.instances > 0 {
            tracing::info!(
                transfer_id = %ctx.id,
                instances = cancelled.instances,
                "调拨单撤回，在跑审批单已终止"
            );
        }
    }
    // 收货完成：库存双写 + 成本原价转移（**同一事务**，失败整笔回滚，状态留在 IN_TRANSIT）
    if affected > 0 && ctx.effect.status == transfer_model::STATUS_DONE {
        let outcome = transfer_model::settle_transfer(
            tx,
            ctx.id,
            ctx.transfer_no,
            ctx.from_warehouse_id,
            ctx.to_warehouse_id,
            Some(ctx.operator),
            ctx.now,
        )
        .await?;
        if outcome.settled {
            tracing::info!(
                transfer_id = %ctx.id,
                transfer_no = %ctx.transfer_no,
                lines = outcome.lines,
                qty = %outcome.qty,
                amount = %outcome.amount,
                serials = outcome.serials,
                "调拨单收货完成：库存双写 + 成本原价转移成功"
            );
        }
    }
    // ⑥ 跨单据联动（LNK-01）：状态已经落库（`affected > 0`）才跑，CAS 落空不触发。
    //    调拨的库存 / 流水由 ④ 的 `settle_transfer` 自己落账，通用引擎必须**显式排除**
    //    那几类动作（否则同一行明细扣两次库存、一张单四条流水）；排除清单在
    //    `spec::TRANSFER_EXCLUDED_ACTIONS`，不是写死在引擎里（换个触发点仍可用通用路线）。
    //
    //    读规则失败 = 空规则集 = 行为与配置前逐字相同（`model::rules_for` fail-safe）；
    //    关键动作失败 → `Err` → 整笔回滚（与本函数其它步骤同一口径）。
    //    提交后动作（发通知 / 建任务）放进 `deferred`，由 `change_transfer_status` 在 commit 之后跑。
    let mut linkage = linkage::LinkageRun::default();
    if affected > 0 {
        linkage = transfer_linkage::run_in_tx(
            tx,
            ctx.id,
            ctx.transfer_no,
            ctx.effect.status,
            Some(ctx.operator),
            ctx.creator,
        )
        .await
        .map_err(SettleError::Linkage)?;
    }
    Ok(TransitionOutcome { affected, linkage })
}

/// 提交审批前准备好审批单参数（**不写库**）。
///
/// - `Ok(None)`：没配已发布的调拨流程（降级：允许人工推 `APPROVED`）；
/// - `Err(409)`：已经有在跑的审批单（避免撞 `uk_biz_flow_instance_biz_active`）。
pub(super) async fn prepare_start(
    row: &TransferRow,
    operator: Uuid,
) -> std::result::Result<Option<engine::StartReq>, ApiError> {
    let Some(flow) = transfer_model::published_transfer_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询调拨审批流程"))?
    else {
        tracing::debug!(
            transfer_id = %row.id,
            "没有配置 STOCK_TRANSFER 审批流程，提交后允许人工推 APPROVED（降级）"
        );
        return Ok(None);
    };
    if flow_model::biz_instance_running(&flow.biz_type, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        return Err(approval_in_progress(Some(&row.transfer_no)));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 库存调拨", row.transfer_no)
        .chars()
        .take(200)
        .collect::<String>();
    Ok(Some(engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code,
        flow_name: flow.flow_name,
        biz_type: flow.biz_type,
        biz_id: Some(row.id),
        biz_no: Some(row.transfer_no.clone()),
        title,
        initiator_id: operator,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: transfer_form_data(row).await,
    }))
}

/// 调拨审批的**表单变量**（P1-FIX：源 / 目的仓库名与明细一起冻结）。
///
/// 审批岗位角色没有 `business:erp:*` 读权限（生产实测四个角色 0 条），前端「ID→名称」必然
/// 降级成 `(ID: …)`；调拨审批人最需要看的恰恰是「从哪个仓到哪个仓、搬了哪些货」。
/// 键名契约见 [`crate::business::erp::form_snapshot`]；查询失败降级为不写该键，不阻断提交。
pub(super) async fn transfer_form_data(row: &TransferRow) -> serde_json::Value {
    let mut form = serde_json::json!({
        "transfer_id": row.id.to_string(),
        "transfer_no": row.transfer_no,
        "from_warehouse_id": row.from_warehouse_id.to_string(),
        "to_warehouse_id": row.to_warehouse_id.to_string(),
        "total_qty": row.total_qty.to_string(),
    });
    let from = form_snapshot::warehouse_name(db(), row.from_warehouse_id).await;
    let to = form_snapshot::warehouse_name(db(), row.to_warehouse_id).await;
    let items = form_snapshot::transfer_items(db(), row.id).await;
    if let Some(object) = form.as_object_mut() {
        match from {
            Ok(Some(name)) => {
                object.insert(
                    "from_warehouse_name".to_string(),
                    serde_json::Value::String(name),
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(transfer_id = %row.id, %error, "审批快照：调出仓库名查询失败，降级");
            }
        }
        match to {
            Ok(Some(name)) => {
                object.insert(
                    "to_warehouse_name".to_string(),
                    serde_json::Value::String(name),
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(transfer_id = %row.id, %error, "审批快照：调入仓库名查询失败，降级");
            }
        }
        match items {
            Ok(items) => {
                object.insert("items".to_string(), items);
            }
            Err(error) => {
                tracing::warn!(transfer_id = %row.id, %error, "审批快照：明细查询失败，降级");
            }
        }
    }
    form
}

/// 状态流转核心逻辑（handler 与单测共用）。
///
/// 顺序：读单 → 目标态解析 → 状态机（非法边 409）→ 引用准备（在途仓 / 审批单）→
/// 一个事务里「发起审批 + 写状态（+ 撤回终止审批）」→ 回读。
pub async fn change_transfer_status(
    id: Uuid,
    req: &TransferStatusReq,
    operator: Uuid,
) -> std::result::Result<(), ApiError> {
    let target = req.target_status()?;
    let cancel_reason = req.cancel_reason()?;
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    let Some(effect) = transition_effect(&row.status, target) else {
        return Err(conflict(transition_rejected_message(&row.status, target)));
    };

    // ⓘ 审批门闸（P5）：挂着在跑的审批单时，人工不许把单据直推 APPROVED（409 + 65030）。
    //   判据是纯函数 `flow_gate_blocked`（三条同时成立才拦），实例状态从审批单现查 ——
    //   单据行的 `approval_instance_id` 是弱引用（无外键），查不到实例就按「不拦」处理
    //   （与 D3a 的任务门闸同口径：读不到状态时宁可放行，也不要把单据锁死）。
    let instance_status = flow_task_bridge::instance_status_of(row.approval_instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单审批单状态"))?;
    if transfer_model::flow_gate_blocked(
        &row.status,
        target,
        row.approval_instance_id,
        instance_status.as_deref(),
    ) {
        tracing::info!(
            transfer_id = %row.id,
            transfer_no = %row.transfer_no,
            approval_instance_id = ?row.approval_instance_id,
            instance_status = ?instance_status,
            operator = %operator,
            "调拨单审批中，人工直推 APPROVED 被门闸拦截（65030）"
        );
        return Err(ApiError::new(
            409,
            error_code::TRANSFER_FLOW_GATE_BLOCKED,
            transfer_model::FLOW_GATE_BLOCKED_MESSAGE,
        ));
    }

    let mut transit_warehouse_id = None;
    // ★ ERP-B2：制单 ≠ 过账（可配开关，默认关闭）——调拨的「发货 / 收货」就是动库存那两步，
    //   与收货单过账同一口径：制单人不能自己把货发出去、也不能自己确认收到。
    if effect.status == transfer_model::STATUS_IN_TRANSIT
        || effect.status == transfer_model::STATUS_DONE
    {
        separation::guard(
            separation::Duty::Post,
            row.create_by,
            Some(operator),
            &format!("调拨单 {}", row.transfer_no),
        )
        .await?;
    }
    if effect.needs_transit_warehouse {
        transit_warehouse_id = transfer_model::transit_warehouse_id()
            .await
            .map_err(|error| utils::db_error(error, "查询在途仓"))?;
        if transit_warehouse_id.is_none() {
            return Err(reference_missing(format!(
                "发货需要「在途仓」（warehouse_code = {}）：W3a 的 V0139 本应种好这一行，\
                 请检查 biz_warehouse 是否被删",
                transfer_model::TRANSIT_WAREHOUSE_CODE
            )));
        }
    }
    let start = if effect.status == STATUS_PENDING {
        prepare_start(&row, operator).await?
    } else {
        None
    };

    let now = Utc::now();
    let ctx = TransitionContext {
        id,
        from: &row.status,
        transfer_no: &row.transfer_no,
        effect: &effect,
        cancel_reason: cancel_reason.as_deref(),
        transit_warehouse_id,
        operator,
        now,
        from_warehouse_id: row.from_warehouse_id,
        to_warehouse_id: row.to_warehouse_id,
        creator: row.create_by,
        start,
    };
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    match apply_transition(&tx, &ctx).await {
        Ok(TransitionOutcome {
            affected: 0,
            linkage: _,
        }) => {
            let _ = tx.rollback().await;
            Err(conflict(format!(
                "调拨单状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.status
            )))
        }
        Ok(outcome) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            // 提交**之后**才跑 AfterCommit 动作（发通知 / 建任务）：它们在独立事务里，
            // 失败只记日志，不回滚已经落定的状态与库存。
            transfer_linkage::run_deferred_after_commit(
                id,
                &row.transfer_no,
                effect.status,
                Some(operator),
                row.create_by,
                outcome.linkage,
            )
            .await;
            Ok(())
        }
        Err(SettleError::Db(error)) => {
            let _ = tx.rollback().await;
            // 单号在这里才拿得到 → 优先用它拼可执行文案（比 write_error 的通用兜底更好读）
            if is_biz_active_duplicate(&error) {
                tracing::warn!(
                    transfer_id = %id,
                    transfer_no = %row.transfer_no,
                    error = %error,
                    "提交审批撞 uk_biz_flow_instance_biz_active：已有在跑实例，翻 409 + 单号"
                );
                Err(approval_in_progress(Some(&row.transfer_no)))
            } else {
                Err(write_error(error, "调拨单状态流转"))
            }
        }
        Err(other) => {
            let _ = tx.rollback().await;
            let api = settle_error(other);
            tracing::warn!(
                transfer_id = %id,
                transfer_no = %row.transfer_no,
                status = %api.status,
                code = api.code,
                msg = %api.msg,
                "调拨单结算失败，已整笔回滚（状态仍为 IN_TRANSIT）"
            );
            Err(api)
        }
    }
}

/// 状态流转（6 条合法边，其余 409）。
#[nik::permission(
    path = "/business/erp/transfer/{id}/status",
    method = "PATCH",
    permission = "business:erp:transfer:status"
)]
#[nik::patch("/business/erp/transfer/{id}/status")]
pub async fn transfer_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TransferStatusReq>,
) -> ResResult<TransferDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let operator = operator_of(&claims).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "无法从令牌解析当前用户：状态流转需要记录操作人（申请人 / 审批人）",
        )
    })?;
    change_transfer_status(id, &req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== 编译期契约（随状态机搬家） ====================

const _: () = assert!(ALL_STATUSES.len() == 6);

/// 权限码对外只读引用（`scripts/verify/w3b_transfer_api.sh` 用它和 V0144 逐字比对）。
pub use crate::business::erp::transfer_dto::TRANSFER_PERMISSIONS as TRANSFER_PERMISSIONS_PUB;

// ============================================================================
// stockin_handler_status.rs —— 状态流转与其余 HTTP 入口（改状态 / 提交过账 / 撤回作废）
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::{Extension, Json};
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_cancel::*;
use super::stockin_handler_common::*;
use super::stockin_handler_post::*;
use crate::business::erp::approval_gate;
use crate::business::erp::separation;
use crate::business::erp::stockin_dto::{MAX_REASON_LEN, StockInDetailRes, StockInStatusReq};
use crate::business::erp::stockin_model::{
    self, STATUS_CANCELLED, STATUS_PENDING_CANCEL, STATUS_PENDING_POST, STATUS_POSTED, can_transition,
    transition_rejected_message,
};
use crate::business::shared::state_machine;
use crate::utils;

/// 状态流转核心逻辑（handler 与单测共用的**系统路径**）。
///
/// 与 [`change_stock_in_status_with`] 完全等价，只是**没有跳过审批的能力**
/// （`ApprovalGate::none()`）—— 单测 / 内部走查用它，「没配 `stock_in_cancel` 流程」时一律 409。
///
/// `POSTED` 走 [`post_stock_in`]（一个事务里做完六件事），`CANCELLED` 只改状态。
pub async fn change_stock_in_status(
    id: Uuid,
    req: &StockInStatusReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let gate = approval_gate::ApprovalGate::none();
    change_stock_in_status_with(id, req, operator, &gate).await
}

/// 状态流转核心逻辑（**HTTP 入口**）：额外接受「显式跳过审批」的上下文（ARCH-3 DC-01）。
///
/// # 与改造前的差别（作废分支）
///
/// 已过账单据的作废：配了已发布的 `stock_in_cancel` → 只发起审批（`POSTED → PENDING_CANCEL`）；
/// **没配 → 409 + `APPROVAL_FLOW_NOT_CONFIGURED`**（改造前是静默直接作废、六本账立刻回滚）。
/// 只有持 `business:erp:skip-approval` 且写明理由（≥10 字）的账号才能显式跳过，
/// 跳过理由与作废理由合并写进 `biz_status_log.reason`。
pub async fn change_stock_in_status_with(
    id: Uuid,
    req: &StockInStatusReq,
    operator: impl Into<state_machine::Operator>,
    gate: &approval_gate::ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let target = req.target_status()?;
    // 作废分支可能把「跳过审批的理由」合进来（`reason` 因此要是可变的 owned 字符串）
    let mut reason: Option<String> = req.reason_text().map(str::to_string);
    if let Some(text) = reason.as_deref()
        && text.chars().count() > MAX_REASON_LEN
    {
        return Err(bad_request(format!(
            "reason 超过 {MAX_REASON_LEN} 字（当前 {} 字）",
            text.chars().count()
        )));
    }

    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if !can_transition(&row.status, target) {
        if row.status == STATUS_POSTED && target == STATUS_POSTED {
            return Err(conflict(format!(
                "收货入库单 {} 已过账（POSTED，时间 {}），不能重复过账：库存只加一次，\
                 若要重收请先作废后重开",
                row.in_no,
                row.posted_at
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string())
            )));
        }
        // 幂等（卡面 §一 ⑦）：已作废再作废 → 409，绝不二次退库存
        if row.status == STATUS_CANCELLED && target == STATUS_CANCELLED {
            return Err(conflict(format!(
                "收货入库单 {} 已经是「已作废」（CANCELLED），不能重复作废：库存只退一次",
                row.in_no
            )));
        }
        return Err(conflict(transition_rejected_message(&row.status, target)));
    }

    if target == STATUS_POSTED {
        // ★ ERP-B2：制单 ≠ 过账（可配开关，默认关闭）。
        //   这里是「没有配过账审批流程」时的降级直过路径；配了流程的另一条路
        //   （免审直过 / 审批通过回调）在 [`post_after_approval`] 里再闸一次，
        //   两条路都不会漏 —— 真正动账的是那一个函数。
        separation::guard(
            separation::Duty::Post,
            row.create_by,
            op.id,
            &format!("收货入库单 {}", row.in_no),
        )
        .await?;
        return post_stock_in(id, &row, req, &op, reason.as_deref()).await;
    }

    // 作废：DRAFT 从未加过库存；POSTED 的库存/库位账/流水在同一事务里反向冲减（P9）
    if row.status == STATUS_POSTED && reason.is_none() {
        return Err(bad_request(
            "已过账的收货单作废必须给出原因（reason）：库存已增加，红冲要能追溯到为什么",
        ));
    }

    // ★ P-FLOW-3A：过账审批**中**的单据不能被「状态流转」直接过账
    //   （那等于绕过审批）。合法边 `PENDING_POST → POSTED` 只给审批回调用。
    if row.status == STATUS_PENDING_POST && target == STATUS_POSTED {
        let instance_no = pending_post_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "收货入库单 {} 已在过账审批中（{}）：审批通过会自动过账（库存/凭证/应付一起动）；\
             想取消本次提交请用「撤回」",
            row.in_no, instance_no
        )));
    }

    // ★ P16：作废审批**中**的单据不能被「状态流转」直接作废（那等于绕过复核）。
    //   合法边 `PENDING_CANCEL → CANCELLED` 只给**审批回调**用，外部 API 想走必须等结论。
    if row.status == STATUS_PENDING_CANCEL && target == STATUS_CANCELLED {
        return Err(conflict(format!(
            "收货入库单 {} 的作废申请正在审批中（{}）：审批通过会自动作废并回滚六本账；\
             想取消本次申请请用「撤回」",
            row.in_no, STATUS_PENDING_CANCEL
        )));
    }

    // ★ P16 + ARCH-3 DC-01：已过账单据的作废先走**复核**（一次作废同时改六本账，不许一个人说了算）。
    //   配了已发布的 `stock_in_cancel` 流程 → 只发起审批（POSTED → PENDING_CANCEL，账不动）；
    //   **没配 → 409（默认拒绝）**；只有「持 business:erp:skip-approval + 理由 ≥10 字」
    //   才能显式跳过（理由与作废理由合并进 `biz_status_log`）。
    if row.status == STATUS_POSTED {
        match stockin_model::published_cancel_flow()
            .await
            .map_err(|error| utils::db_error(error, "查询收货单作废审批流程"))?
        {
            Some(flow) => {
                approval_gate::ensure_not_skipped(stockin_model::CANCEL_FLOW_CODE, gate)?;
                return submit_cancel_approval(&row, reason.as_deref().unwrap_or_default(), &op, &flow).await;
            }
            None => {
                let skip_reason = approval_gate::authorize_missing_flow(
                    stockin_model::CANCEL_BIZ_TYPE,
                    stockin_model::CANCEL_FLOW_CODE,
                    gate,
                )?;
                tracing::warn!(
                    stock_in_id = %row.id,
                    in_no = %row.in_no,
                    %skip_reason,
                    "收货单作废：没有已发布的 stock_in_cancel 流程，按「显式跳过审批」放行（六本账立即回滚）"
                );
                reason = Some(approval_gate::merge_reason(reason.as_deref(), &skip_reason));
            }
        }
    }

    let now = Utc::now();
    // LNK-02：作废会重算采购单状态，那条边要触发联动（同 `cancel_in_tx` 的口径）
    let mut linkage_outbox = crate::business::erp::purchase_model::LinkageOutbox::default();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    match cancel_in_tx(&tx, id, &row, reason.as_deref(), &op, now, &mut linkage_outbox).await {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            linkage_outbox.flush().await;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 收货入库单状态流转（`DRAFT→POSTED` 过账 / `DRAFT→CANCELLED` / `POSTED→CANCELLED`）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/status",
    method = "PATCH",
    permission = "business:erp:stock-in:status"
)]
#[nik::patch("/business/erp/stock-in/{id}/status")]
pub async fn stock_in_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StockInStatusReq>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
    submit_post(id, &req, &claims, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③c 提交过账审批（P-FLOW-3A） ====================

/// 提交收货单过账（`DRAFT → PENDING_POST`，或未配流程时降级直接过账）。
///
/// 与 `PATCH /status` 走**同一个函数**（[`submit_post`]）：
/// 只要配了已发布的 `stock_in_post` 流程，两条路都必须先送审 —— 不存在换 URL 绕过审批。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/submit-post",
    method = "POST",
    permission = "business:erp:stock-in:submit-post-approval"
)]
#[nik::post("/business/erp/stock-in/{id}/submit-post")]
pub async fn stock_in_submit_post(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<StockInStatusReq>>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let mut req = body.map(|Json(item)| item).unwrap_or_default();
    // `/submit-post` 的语义就是「提交过账」：body 可省，缺省目标态即 POSTED
    if req.status.is_none() {
        req.status = Some(STATUS_POSTED.to_string());
    }
    let gate = approval_gate::ApprovalGate::new(&req.approval, &claims);
    submit_post(id, &req, &claims, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③d 重新过账（BUG-02） ====================

/// **重新过账**：审批已通过、但上次过账失败的单据（`PENDING_POST` + 实例 `APPROVED`）原地重试。
///
/// 为什么需要这个接口：审批通过回调已经跑过一次（失败即返回），`PATCH /status` 又明确
/// 拒绝 `PENDING_POST → POSTED`（防绕过审批）—— 没有这个入口，这类单据只能删单重建，
/// 单号与历史全丢（生产事故 `IN-20261031-000002`）。
///
/// **权限码沿用 `submit-post`**：能提交过账的人就能在审批通过后重试同一次过账；
/// 「制单 ≠ 过账」「审批 ≠ 执行」这类职责分离仍由 `separation::guard` 按开关拦截，
/// 不靠权限码表达（权限码只回答「这个岗位能不能碰这张单」）。
/// **不放松任何校验**：实现直接复用审批通过回调的过账函数（全部 guard 重跑）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/repost",
    method = "POST",
    permission = "business:erp:stock-in:submit-post-approval"
)]
#[nik::post("/business/erp/stock-in/{id}/repost")]
pub async fn stock_in_repost(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    repost_stock_in(id, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③b 撤回作废申请（P16） ====================

/// 撤回收货单的作废申请（`PENDING_CANCEL → POSTED`）。
///
/// 权限码沿用 `business:erp:stock-in:status`：撤回是「状态流转」的一种，
/// 且引擎侧还会校验「只有发起人能撤」（其余人 403 + 65004）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/cancel-withdraw",
    method = "POST",
    permission = "business:erp:stock-in:status"
)]
#[nik::post("/business/erp/stock-in/{id}/cancel-withdraw")]
pub async fn stock_in_cancel_withdraw(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    withdraw_cancel_approval(id, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 删除 ====================

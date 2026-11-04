// ============================================================================
// stockin_handler_post.rs —— 过账：提交审批 / 审批通过后过账 / 事务内扣库与成本
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_common::*;
use super::stockin_handler_purchase::*;
use super::stockin_handler_status::*;
use crate::business::erp::approval_gate;
use crate::business::erp::payable;
use crate::business::erp::separation;
use crate::business::erp::stockin_dto::StockInStatusReq;
use crate::business::erp::stockin_model::{
    self, CostBasis, STATUS_DRAFT, STATUS_PENDING_POST, STATUS_POSTED, status_label,
};
use crate::business::erp::voucher;
use crate::business::flow::linkage;
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

/// 过账所需的**输入快照**（明细 / 一物一码开关 / 序列号请求 / 采购未收快照）。
///
/// `req = None` 表示**审批回调路径**（[`post_after_approval`]）：审批单不携带序列号，
/// 一物一码走 [`stockin_model::generate_serials`] 的「入参为空 → 自动取号」分支
/// （与直接过账「操作员不填序列号」走的是同一条路，不另写一套）。
pub(super) async fn posting_inputs(
    id: Uuid,
    source_order_id: Option<Uuid>,
    req: Option<&StockInStatusReq>,
) -> std::result::Result<
    (
        Vec<stockin_model::StockInItemRow>,
        HashMap<Uuid, bool>,
        HashMap<i32, Vec<String>>,
        HashMap<Uuid, (Decimal, Decimal)>,
    ),
    ApiError,
> {
    let items = stockin_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单明细"))?;
    if items.is_empty() {
        return Err(bad_request("收货入库单没有任何明细，无法过账"));
    }
    // 一物一码开关：事务外查（少占锁）
    let mut serial_required: HashMap<Uuid, bool> = HashMap::new();
    for item in &items {
        let flag = stockin_model::material_serial_required(item.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料一物一码开关"))?
            .unwrap_or(false);
        serial_required.insert(item.material_id, flag);
    }
    let provided = match req {
        Some(req) => request_serials(req, &items)?,
        None => HashMap::new(),
    };
    let po_snapshot = purchase_item_snapshot(source_order_id).await?;
    Ok((items, serial_required, provided, po_snapshot))
}

/// 走「过账」的**唯一对外入口**（P-FLOW-3A + ARCH-3 DC-01）：配了已发布的 `stock_in_post`
/// 流程就先送审；**没配流程 → 409 + `APPROVAL_FLOW_NOT_CONFIGURED`（默认拒绝）**，
/// 只有持 `business:erp:skip-approval` 且写明理由（≥10 字）的账号才能显式跳过
/// （理由写进 `biz_status_log`）。
///
/// 为什么入口收在这里、而不是散在路由里：`PATCH /status`（下游老前端）与
/// `POST /submit-post`（本卡新按钮）**必须同一条路** —— 否则审批可以被换一个 URL 绕过。
pub async fn submit_post(
    id: Uuid,
    req: &StockInStatusReq,
    operator: impl Into<state_machine::Operator>,
    gate: &approval_gate::ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    let target = req.target_status()?;
    if target != STATUS_POSTED {
        return change_stock_in_status_with(id, req, op, gate).await;
    }
    match stockin_model::published_post_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询收货单过账审批流程"))?
    {
        Some(flow) => {
            approval_gate::ensure_not_skipped(stockin_model::POST_FLOW_CODE, gate)?;
            submit_post_approval(&row, &op, &flow, req).await
        }
        None => {
            // ARCH-3 DC-01：**不再静默降级**。默认 409（点名 stock_in_post）；
            // 跳过审批时把理由合进 `req.reason`，由 `post_in_tx` 的状态机入口写进 biz_status_log。
            let skip_reason = approval_gate::authorize_missing_flow(
                stockin_model::POST_BIZ_TYPE,
                stockin_model::POST_FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                stock_in_id = %row.id,
                in_no = %row.in_no,
                %skip_reason,
                "收货单过账：没有已发布的 stock_in_post 流程，按「显式跳过审批」放行（六本账立即变）"
            );
            let mut next = req.clone();
            next.reason = Some(approval_gate::merge_reason(req.reason_text(), &skip_reason));
            change_stock_in_status_with(id, &next, op, gate).await
        }
    }
}

/// **发起过账审批**（P-FLOW-3A）：`DRAFT → PENDING_POST`，只挂审批单，六本账一动没动。
///
/// 一个事务里做完三件事：状态机过闸（边表 + `biz_status_log`）→ `engine::start` 发起审批单 →
/// CAS 回写单据状态与 `approval_instance_id`。任一步失败整笔回滚：
/// **不会出现「单据在审批中但审批单没了」**。
pub(super) async fn submit_post_approval(
    row: &stockin_model::StockInRow,
    operator: &state_machine::Operator,
    flow: &stockin_model::PublishedFlow,
    req: &StockInStatusReq,
) -> std::result::Result<(), ApiError> {
    // ① 单据必须还是草稿（审批中 / 已过账 / 已作废都不能再提交）
    if row.status != STATUS_DRAFT {
        if row.status == STATUS_PENDING_POST {
            let instance_no = pending_post_no_of(row.approval_instance_id).await?;
            return Err(conflict(format!(
                "收货入库单 {} 已在过账审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
                row.in_no, instance_no
            )));
        }
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交过账审批",
            row.in_no,
            status_label(&row.status),
            row.status
        )));
    }
    // ② 明细 / 金额闸：空单、零金额不许送审（审批人批了也过不了账）
    let items = stockin_model::items_of(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "收货入库单 {} 没有任何明细，不能提交过账审批",
            row.in_no
        )));
    }
    if row.total_with_tax <= Decimal::ZERO {
        return Err(bad_request(format!(
            "收货入库单 {} 的含税金额为 {}，不能提交过账审批（金额必须 > 0）",
            row.in_no, row.total_with_tax
        )));
    }
    // 序列号在提交时就校验一遍（数量与号码必须对齐），失败别送去给审批人批
    let _ = request_serials(req, &items)?;
    let Some(operator_id) = operator.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交过账审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    // ③ 幂等：同一单已有在跑的过账审批 → 409（先查，别等库层 uk_biz_flow_instance_biz_active）
    if flow_model::biz_instance_running(stockin_model::POST_BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_post_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "收货入库单 {} 已在过账审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.in_no, instance_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 收货过账", row.in_no)
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.in_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: stock_in_form_data(row).await,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启过账审批事务"))?;
    let outcome = submit_post_tx(&tx, row, operator, &start, now).await;
    let mut out = match outcome {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let instance = out.instance_id.unwrap_or_default();
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交过账审批事务"))?;
    // 锁外副作用：小额免审分支的 NOTIFY_NODE 知会消息就在这批草稿里，必须在提交之后发
    flush_effects(&mut out).await;
    tracing::info!(
        stock_in_id = %row.id,
        in_no = %row.in_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "收货单已提交过账审批（状态 PENDING_POST，六本账未动）"
    );
    // 「小额免审」分支会在同一个事务里走完（NOTIFY 节点不阻塞）→ 提交后**立即执行过账**，
    // 否则单据会停在 PENDING_POST 且没有任何人会来推进它（没有待办）。
    if auto_finished {
        match post_after_approval(row.id, Some(operator_id)).await {
            Ok(()) => return Ok(()),
            // ★ ERP-B2：免审直过被「制单 ≠ 过账」拦下 —— 单据已经在 PENDING_POST，
            //   而流程没有留下任何待办（免审），必须当场打回 DRAFT，否则就是一张死单。
            //   打回后由**别人**来提交即可过账（换人这条路正是内控想要的）。
            Err(error) if error.code == error_code::ERP_SEPARATION_OF_DUTIES => {
                if let Err(restore_error) = restore_after_post_rejection(
                    row.id,
                    row,
                    "制单与过账分离：免审过账被内控拦截，已打回草稿等待他人过账",
                    Some(operator_id),
                )
                .await
                {
                    tracing::error!(
                        stock_in_id = %row.id,
                        in_no = %row.in_no,
                        error = %restore_error.msg,
                        "免审过账被内控拦截后打回草稿失败：单据可能停在 PENDING_POST"
                    );
                    return Err(restore_error);
                }
                tracing::warn!(
                    stock_in_id = %row.id,
                    in_no = %row.in_no,
                    "免审过账被「制单与过账分离」拦截，单据已打回 DRAFT（需他人提交过账）"
                );
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// [`submit_post_approval`] 的事务体：返回引擎动作结果（调用方提交后负责发锁外副作用）。
pub(super) async fn submit_post_tx(
    tx: &Transaction,
    row: &stockin_model::StockInRow,
    operator: &state_machine::Operator,
    start: &engine::StartReq,
    now: DateTime<Utc>,
) -> std::result::Result<engine::ActionOut, ApiError> {
    // ① 状态机唯一入口：边表校验 + 写 biz_status_log
    state_machine::transition(
        tx,
        state_machine::ENTITY_STOCK_IN,
        row.id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        STATUS_DRAFT,
        STATUS_PENDING_POST,
        operator.clone(),
        Some("提交过账审批"),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一个事务里建实例 + 待办 + 节点）
    let mut out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, "发起过账审批单", &row.in_no))?;
    if let Some(rejected) = out.rejected.take() {
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!(
                "{}（内控要求：过账复核人不能是发起人本人。请由他人提交，或让另一个账号具备复核权限后再审）",
                rejected.msg
            ),
        ));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 实例 id（两者同一笔 UPDATE，否则撞 ck_biz_si_pending_approval）
    let affected = stockin_model::apply_status_with_instance(
        tx,
        row.id,
        STATUS_DRAFT,
        STATUS_PENDING_POST,
        None,
        operator.id,
        Some(instance_id),
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货单进入过账审批中"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
            row.in_no
        )));
    }
    Ok(out)
}

/// 审批**通过**之后执行过账（由 [`stockin_bridge::on_post_settled`] / 提交后的「免审」分支调用）。
///
/// 复用 [`post_in_tx`]（六本账唯一实现），只把起始状态换成 `PENDING_POST`：
/// 库存 / 库位账 / 流水 / 序列号 / 采购已收数量 / 凭证 / 应付全在同一个事务里，
/// 任一步失败整笔回滚（单据仍是过账审批中，等人工或下一轮重试）。
pub async fn post_after_approval(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if row.status != STATUS_PENDING_POST {
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），不是过账审批中：不能执行过账",
            row.in_no,
            status_label(&row.status),
            row.status
        )));
    }
    // ★ ERP-B2：制单 ≠ 过账（可配开关，默认关闭）。
    //
    // 这里是**真正动六本账**的地方，两条路都汇到它：
    // ① 免审直过（[`submit_post_approval`] 的 `auto_finished` 分支，`operator` = 提交人本人）
    //    —— 种子流程 `stock_in_post` 对「< 1000 元」的单据只发知会不排审批，
    //    过账人就是制单人本人，这是「收货单制单人 = 过账人 8 条」的真正来源；
    // ② 审批通过回调（`stockin_bridge`，`operator` = 审批人）—— 通常天然分流。
    //
    // 放在 `post_in_tx` 之前：六本账一动没动就被拦下，调用方（bridge / 免审分支）
    // 负责把单据打回草稿，不会留下「停在审批中却没有任何待办」的死单。
    // ★ A4：审批通过的过账路径同样复校一次（两条入口同一把尺子）
    guard_posting_supplier(&row).await?;
    let op = state_machine::Operator::of(operator.or(row.create_by));
    separation::guard(
        separation::Duty::Post,
        row.create_by,
        op.id,
        &format!("收货入库单 {}", row.in_no),
    )
    .await?;
    let (items, serial_required, provided, po_snapshot) =
        posting_inputs(id, row.source_order_id, None).await?;
    // ★ DC-09 补口：审批通过这条过账入口同样按明细快照的 unit_id 复检（与直接过账同一把尺子）
    guard_posting_units(&items).await?;
    let mut ordered: Vec<&stockin_model::StockInItemRow> = items.iter().collect();
    ordered.sort_by(|a, b| {
        (a.material_id, a.location_id, a.line_no).cmp(&(b.material_id, b.location_id, b.line_no))
    });
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启审批过账事务"))?;
    let outcome = post_in_tx(
        &tx,
        id,
        &row,
        &ordered,
        &serial_required,
        &provided,
        &po_snapshot,
        &op,
        STATUS_PENDING_POST,
        None,
        now,
    )
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交审批过账事务"))?;
            tracing::info!(
                stock_in_id = %id,
                in_no = %row.in_no,
                "过账审批通过：库存/库位账/流水/序列号/凭证/应付已在一个事务里落库"
            );
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 审批**未通过**之后把单据打回草稿（驳回 / 撤回 / 终止）。
///
/// 账一动不动 —— 这里只有状态 + 清理 `approval_instance_id`。
pub async fn restore_after_post_rejection(
    id: Uuid,
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启过账打回事务"))?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_IN,
            id,
            &row.in_no,
            &stockin_model::LEGAL_EDGES,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            operator,
            Some(reason),
            None,
        )
        .await?;
        let affected = stockin_model::apply_status(
            &tx,
            id,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            None,
            operator,
            now,
        )
        .await
        .map_err(|error| write_error(error, "收货单过账审批打回草稿"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "收货入库单 {} 的状态在本次打回期间已被改动（期望 {STATUS_PENDING_POST}），请刷新后重试",
                row.in_no
            )));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交过账打回事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// **过账**（本卡核心）：一个事务里改状态 + 加库存 + 写流水 + 生成序列号 + 回写采购单。
pub(super) async fn post_stock_in(
    id: Uuid,
    header: &stockin_model::StockInRow,
    req: &StockInStatusReq,
    operator: &state_machine::Operator,
    reason: Option<&str>,
) -> std::result::Result<(), ApiError> {
    // ★ A4：过账前复校「单据头供应商 = 来源采购单供应商」。
    //   建单期的校验挡不住绕过接口的 UPDATE，而应付/凭证直接取单据头 `supplier_id`。
    guard_posting_supplier(header).await?;
    let (items, serial_required, provided, po_snapshot) =
        posting_inputs(id, header.source_order_id, Some(req)).await?;
    // ★ DC-09 补口：过账期按明细快照的 unit_id 复检（建单期校验挡不住 UPDATE / 存量草稿）
    guard_posting_units(&items).await?;

    // PRD §3.2.6：多行按 material_id 升序加锁（再按库位 / 行号，保证全序），避免两个收货单互相死锁
    let mut ordered: Vec<&stockin_model::StockInItemRow> = items.iter().collect();
    ordered.sort_by(|a, b| {
        (a.material_id, a.location_id, a.line_no).cmp(&(b.material_id, b.location_id, b.line_no))
    });

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启过账事务"))?;
    let outcome = post_in_tx(
        &tx,
        id,
        header,
        &ordered,
        &serial_required,
        &provided,
        &po_snapshot,
        operator,
        STATUS_DRAFT,
        reason,
        now,
    )
    .await;
    match outcome {
        Ok(()) => {
            // 逐行写失败时由 drop 回滚（不 commit 就等于回滚），但显式 rollback 更清楚
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交过账事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 过账事务体（抽出来便于「失败即回滚」的统一收口）。
///
/// `from_status` 是**唯一**与入口相关的差别（P-FLOW-3A 加的）：
/// * `DRAFT` —— 直接过账（未配过账流程 / 内部调用 / 老用例）；
/// * `PENDING_POST` —— 过账审批通过后执行（[`post_after_approval`]）。
///
/// 其余六本账动作（库存 / 库位账 / 流水 / 序列号 / 采购已收 / 凭证 + 应付）
/// **一行都没重写**：审批通过只换了入口状态，"审 A 记 B" 的风险由
/// `STOCK_IN_LOCKED`（PENDING_POST 锁单据）兜住。
#[allow(clippy::too_many_arguments)]
pub(super) async fn post_in_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    header: &stockin_model::StockInRow,
    ordered: &[&stockin_model::StockInItemRow],
    serial_required: &HashMap<Uuid, bool>,
    provided: &HashMap<i32, Vec<String>>,
    po_snapshot: &HashMap<Uuid, (Decimal, Decimal)>,
    operator: &state_machine::Operator,
    from_status: &str,
    reason: Option<&str>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 锁单头（并发重复过账在这里串行化）
    let locked = stockin_model::lock_stock_in(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if locked.status != from_status {
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），不能过账：库存只加一次",
            locked.in_no,
            status_label(&locked.status),
            locked.status
        )));
    }

    // ①b DC-03：过账复核来源采购单（状态 + 待收额度）。
    //    必须在 ② 改状态与 ③ 扣减库存**之前**：否则「采购单已取消」也会把三本账、
    //    应付与进项凭证写出去。锁在事务内，失败整笔回滚（不留半条状态日志）。
    guard_purchase_before_post(ex, &locked, ordered).await?;

    // ② CAS 写状态（`WHERE status = from_status`）
    //    P10 铁律 ③：先过唯一入口（边表 + reason 校验 + 写 biz_status_log），
    //    再 CAS 改状态。两步同一事务：CAS 落空 → 整笔回滚，日志不会半条落地。
    //    ARCH-3 DC-01：`reason` 只在「显式跳过审批的直过路径」非空（写进 biz_status_log，
    //    审计据此看出这次过账没走复核）；审批回调用 `None`（审批单本身就是留痕）。
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_IN,
        id,
        &header.in_no,
        &stockin_model::LEGAL_EDGES,
        from_status,
        STATUS_POSTED,
        operator.clone(),
        reason,
        None,
    )
    .await?;
    let affected =
        stockin_model::apply_status(ex, id, from_status, STATUS_POSTED, reason, operator.id, now)
            .await
            .map_err(|error| write_error(error, "收货入库单过账改状态"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 已被过账（并发），本次未生效",
            header.in_no
        )));
    }

    // ③ 逐行：库存加法 + 流水（成本口径 = 不含税金额）
    for item in ordered {
        let cost = CostBasis::of(item.qty, item.amount);
        let applied = stockin_model::apply_cost(
            ex,
            item.material_id,
            header.warehouse_id,
            item.location_id,
            cost,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：库存加法"))?;
        // ③b 库位账（数量账）：与成本账**同一事务**同步加，否则违反不变式③
        //     （SUM(库位账) = 成本账 qty）。锁顺序：成本账已锁 → 这里只锁库位账一行，
        //     明细在调用方已按 material_id 升序排好，故两个收货单并发不会交叉等待。
        stockin_model::apply_location_qty(
            ex,
            item.material_id,
            header.warehouse_id,
            item.location_id,
            item.qty,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：库位账加法"))?;
        stockin_model::write_txn(
            ex,
            item.material_id,
            header.warehouse_id,
            item.line_no,
            cost,
            &applied,
            id,
            &header.in_no,
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：写库存流水"))?;

        // ④ 一物一码：落 / 生成序列号
        if serial_required
            .get(&item.material_id)
            .copied()
            .unwrap_or(false)
        {
            let serial_ids = provided.get(&item.line_no).cloned().unwrap_or_default();
            stockin_model::generate_serials(
                ex,
                &stockin_model::SerialBatch {
                    stock_in_id: id,
                    material_id: item.material_id,
                    warehouse_id: header.warehouse_id,
                    location_id: item.location_id,
                    qty: item.qty,
                    serial_ids: &serial_ids,
                    operator: operator.id,
                    now,
                },
            )
            .await
            .map_err(|error| match error {
                DbError::UniqueViolation { constraint, .. } => ApiError::new(
                    409,
                    error_code::ERP_CODE_CONFLICT,
                    format!(
                        "序列号重复（唯一约束{}）：这个码已经入过库，不能重复入库",
                        constraint
                            .as_deref()
                            .map(|name| format!(" {name}"))
                            .unwrap_or_default()
                    ),
                ),
                other => write_error(other, "过账：生成序列号"),
            })?;
        }

        // ⑤ 回写采购明细已收数量（条件更新，0 行 = 超收）
        if let Some(source_item_id) = item.source_item_id {
            let affected = stockin_model::add_received_qty(ex, source_item_id, item.qty, now)
                .await
                .map_err(|error| write_error(error, "过账：回写采购已收数量"))?;
            if affected == 0 {
                let (po_qty, received) = po_snapshot
                    .get(&source_item_id)
                    .copied()
                    .unwrap_or_default();
                return Err(bad_request(format!(
                    "第 {} 行收货数量超过采购单未收数量：采购 {po_qty}、已收 {received}，\
                     本次再收 {} → 超出 {}（并发下别的收货单可能刚收走了）",
                    item.line_no,
                    item.qty,
                    received + item.qty - po_qty
                )));
            }
        }
    }

    // ⑥ 推进采购单状态（PARTIAL_RECEIVED / RECEIVED）—— P10：走状态机唯一入口
    if let Some(order_id) = header.source_order_id {
        stockin_model::sync_purchase_status(
            ex,
            order_id,
            &header.in_no,
            "收货过账",
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：推进采购单状态"))?;
    }

    // ⑦ 应付单 + 凭证（P2-C1，方案 A：入库即生成；**同一事务**，失败整单回滚）
    //    金额全部取自单头（header.total_*），不重算（硬红线 5）。
    //    顺序：先凭证（应付要引用 voucher_id）→ 再应付 → 最后回写收货单两列。
    //
    //    【D3】凭证这一步现在由**联动配置路由**（`linkage::engine::stockin_voucher`）：
    //      · 没有 GENERATE_VOUCHER 规则 → 老路 `voucher::create_for_stock_in`（逐字不变）；
    //      · mode=SHADOW → 配置路径在 SAVEPOINT 里试算并回滚，老路照旧落账，事后逐字段对比；
    //      · mode=TAKEOVER → 配置路径落账，老路**不再生成**（否则一张单两张凭证）；
    //      · 配置写脏 / 模板停用 / 单据不全 → 退回老路（凭证不能因为配置写错而漏记）；
    //      · 关键动作失败（借贷不平 / 期间已结账）→ 引擎返回 Err → 整笔过账回滚。
    //    两条路走的是**同一个** `voucher::create_by_rule`，取数与取号只有一份实现。
    // 按物料聚合的存货行（P2-1b §4.2）：粒度 = 去重物料，按 `material_id` 升序；
    // 同一物料的多行合并（金额 / 数量相加、行号取区间端点）。口径与配置路径
    // （`stockin_query::material_lines_of` 的一条聚合 SQL）逐字一致 —— 两条路生成的凭证
    // 行数、行序、行金额必须一样，否则 D3 影子对比会报差异。
    let material_lines = voucher::VoucherMaterialLine::normalize(ordered.iter().map(|item| {
        voucher::VoucherMaterialLine {
            material_id: item.material_id,
            qty: item.qty,
            amount: item.amount,
            line_from: item.line_no,
            line_to: item.line_no,
        }
    }));
    tracing::info!(
        stock_in_id = %id,
        in_no = %locked.in_no,
        material_count = material_lines.len(),
        "收货过账：按物料聚合出 {} 行存货分录（每个物料一行）",
        material_lines.len()
    );
    let voucher_ctx = linkage::engine::LinkageContext::new(
        state_machine::ENTITY_STOCK_IN,
        STATUS_POSTED,
        id,
        &locked.in_no,
        operator.id,
    );
    let voucher_id = match linkage::engine::stockin_voucher(ex, &voucher_ctx).await {
        Ok(linkage::engine::VoucherRoute::Configured { voucher_id, .. }) => voucher_id,
        Ok(linkage::engine::VoucherRoute::Legacy { reason, shadow }) => {
            let legacy_id =
                voucher::create_for_stock_in(ex, &locked, material_lines, operator.id, now)
                    .await
                    .map_err(|error| write_error(error, "过账：生成凭证"))?;
            if let Some(shadow) = shadow {
                linkage::engine::finish_stockin_voucher_shadow(
                    ex,
                    &voucher_ctx,
                    &shadow,
                    legacy_id,
                )
                .await;
            } else {
                tracing::debug!(reason = %reason, "收货过账：凭证仍走旧逻辑（联动未接管）");
            }
            legacy_id
        }
        Err(error) => {
            // 关键动作失败：`post_in_tx` 的调用方会 rollback，整笔状态变更一起回退
            return Err(error);
        }
    };
    let payable_id = payable::create_for_stock_in(ex, &locked, Some(voucher_id), operator.id, now)
        .await
        .map_err(|error| write_error(error, "过账：生成应付单"))?;
    stockin_model::link_voucher_and_payable(ex, id, voucher_id, payable_id, now)
        .await
        .map_err(|error| write_error(error, "过账：回写凭证/应付引用"))?;
    tracing::info!(
        stock_in_id = %id,
        in_no = %locked.in_no,
        voucher_id = %voucher_id,
        payable_id = %payable_id,
        total_with_tax = %locked.total_with_tax,
        "收货过账：已生成应付单与凭证（借存货 + 借进项税 / 贷应付）"
    );
    Ok(())
}

/// 校验 PATCH body 里的序列号（`line_no → 号码`），并把数量与明细 qty 对齐（卡面 ⑩）。
pub(super) fn request_serials(
    req: &StockInStatusReq,
    items: &[stockin_model::StockInItemRow],
) -> std::result::Result<HashMap<i32, Vec<String>>, ApiError> {
    let mut map: HashMap<i32, Vec<String>> = HashMap::new();
    if req.serials.is_empty() {
        return Ok(map);
    }
    let by_line: HashMap<i32, &stockin_model::StockInItemRow> =
        items.iter().map(|item| (item.line_no, item)).collect();
    for entry in &req.serials {
        let Some(item) = by_line.get(&entry.line_no) else {
            return Err(bad_request(format!(
                "serials 里的 line_no = {} 在本单明细里不存在（本单行号：{}）",
                entry.line_no,
                items
                    .iter()
                    .map(|it| it.line_no.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        let cleaned: Vec<String> = entry
            .serial_ids
            .iter()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .collect();
        let count = cleaned.len();
        let expected: i64 = item.qty.try_into().map_err(|_| {
            bad_request(format!(
                "第 {} 行 qty = {} 不是整数：一物一码物料按件入库",
                item.line_no, item.qty
            ))
        })?;
        if i64::try_from(count).unwrap_or(-1) != expected {
            return Err(bad_request(format!(
                "第 {} 行的序列号条数与数量不符：qty = {}（{expected} 件），serial_ids 给了 {count} 条",
                entry.line_no, item.qty
            )));
        }
        let mut seen = std::collections::HashSet::with_capacity(cleaned.len());
        if let Some(dup) = cleaned.iter().find(|serial_no| !seen.insert(*serial_no)) {
            return Err(bad_request(format!(
                "第 {} 行的序列号重复：`{dup}`",
                entry.line_no
            )));
        }
        if map.insert(entry.line_no, cleaned).is_some() {
            return Err(bad_request(format!(
                "serials 里 line_no = {} 出现了两次",
                entry.line_no
            )));
        }
    }
    Ok(map)
}

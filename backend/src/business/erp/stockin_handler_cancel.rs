// ============================================================================
// stockin_handler_cancel.rs —— 作废：提交/撤回作废审批 / 审批后反冲
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_common::*;
use crate::business::erp::payable;
use crate::business::erp::stockin_bridge;
use crate::business::erp::stockin_model::{
    self, STATUS_CANCELLED, STATUS_PENDING_CANCEL, STATUS_PENDING_POST, STATUS_POSTED, status_label,
};
use crate::business::erp::voucher;
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

/// **发起作废审批**（P16）：`POSTED → PENDING_CANCEL`，只挂审批单，六本账一动没动。
///
/// 一个事务里做完三件事：状态机过闸（边表 + reason + `biz_status_log`）→
/// `engine::start` 发起审批单 → CAS 回写单据状态与 `approval_instance_id`。
/// 任一步失败整笔回滚：**不会出现「单据在审批中但审批单没了」**。
///
/// # 错误
///
/// * `409`：已有在跑的审批单（撞 `uk_biz_flow_instance_biz_active` 之前先拦，报文可执行）；
/// * `409` + `65019`：内控拦下自审（流程节点 `self_approval=FORBID` 且发起人就是审批人）；
/// * `409`：CAS 落空（并发重复发起 / 单据已被别处改状态）。
pub(super) async fn submit_cancel_approval(
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: &state_machine::Operator,
    flow: &stockin_model::PublishedFlow,
) -> std::result::Result<(), ApiError> {
    // ④ 可行性预检：货已部分出库 / 序列号已出仓的单**不许送审**
    //    （否则审批人会「批成功但账退不掉」，单据永远卡在作废审批中）
    stockin_model::ensure_cancellable(db(), row.id, &row.in_no, row.warehouse_id)
        .await
        .map_err(|error| cancel_write_error(error, "发起作废审批：可行性预检"))?;
    let Some(operator_id) = operator.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "发起作废审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    // ③ 重复发起 → 409（先查，别等库层的 uk_biz_flow_instance_biz_active 抛 23505）
    if flow_model::biz_instance_running(&flow.biz_type, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        return Err(conflict(format!(
            "收货入库单 {} 已有在跑的作废审批：请等审批结束，或先「撤回」再重新发起",
            row.in_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 收货单作废", row.in_no)
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
        form_data: serde_json::json!({
            "stock_in_id": row.id.to_string(),
            "in_no": row.in_no,
            "cancel_reason": reason,
            "total_qty": row.total_qty.to_string(),
            "total_amount": row.total_amount.to_string(),
            // 内控留痕：审批人 ≠ 制单人（流程节点的 self_approval=FORBID 兜底）
            "create_by": row.create_by.map(|v| v.to_string()),
        }),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废审批事务"))?;
    let outcome = submit_cancel_tx(&tx, row, reason, operator, &start, now).await;
    match outcome {
        Ok(instance) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废审批事务"))?;
            tracing::info!(
                stock_in_id = %row.id,
                in_no = %row.in_no,
                instance_id = %instance,
                flow_code = %start.flow_code,
                "收货单已发起作废审批（状态 PENDING_CANCEL，六本账未动）"
            );
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// [`submit_cancel_approval`] 的事务体（抽出来便于统一收口回滚）。
///
/// 收 `&Transaction` 而不是 `&impl SqlExecutor`：`engine::start` 的签名要求真事务
/// （引擎要靠它在同一个事务里建实例 + 待办 + 节点）。
pub(super) async fn submit_cancel_tx(
    tx: &Transaction,
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: &state_machine::Operator,
    start: &engine::StartReq,
    now: DateTime<Utc>,
) -> std::result::Result<Uuid, ApiError> {
    // ① 状态机唯一入口：边表校验 + reason 校验 + 写 biz_status_log
    state_machine::transition(
        tx,
        state_machine::ENTITY_STOCK_IN,
        row.id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        STATUS_POSTED,
        STATUS_PENDING_CANCEL,
        operator.clone(),
        Some(reason),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一个事务里建实例 + 待办 + 节点）
    let out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, "发起作废审批单", &row.in_no))?;
    if let Some(rejected) = out.rejected {
        // 作废是**反向记账**（六本账回滚 + 红冲），复核岗位没配好就不许发起。
        // 两类失败的出路完全不同，报文必须点名岗位与配置路径 —— 只说「不许自审」等于把人堵死：
        //   · 65019 `self_approval=FORBID` 命中：发起人本人就在复核岗位上；
        //   · 65018 复核岗位一个人都解析不出来（角色没绑人 / 绑的人被停用）。
        // （t_e63b600f 实测：生产 4 个超管当复核人、发起人也是超管 ⇒ 作废永远 422 走不通。）
        let hint = match rejected.code {
            error_code::FLOW_SELF_APPROVAL_FORBIDDEN => {
                "（内控要求：作废复核人不能是发起人本人。本流程的复核岗位是「仓管主管（WAREHOUSE_MANAGER）」，\
                 而你本人就在这个岗位上：请让另一位复核人发起本单，或把一位非发起人的同事加入「仓管主管」角色后再发起。\
                 配置路径：系统管理 → 角色管理 → 仓管主管 → 成员）"
            }
            error_code::FLOW_NO_ASSIGNEE => {
                "（作废复核岗位没有任何人可派：请在「系统管理 → 角色管理 → 仓管主管（WAREHOUSE_MANAGER）→ 成员」\
                 绑定至少一名在职的非制单人；或在「审批中心 → 流程定义 → 收货单作废审批 → 作废复核」把审批人改成具体人员。\
                 作废会反向回滚六本账，不允许无人复核，也不会自动兜底给管理员）"
            }
            _ => "",
        };
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!("{}{hint}", rejected.msg),
        ));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 申请理由 + 实例 id（三者同一笔 UPDATE，否则撞 ck_biz_si_pending_cancel）
    let affected = stockin_model::apply_status_with_instance(
        tx,
        row.id,
        STATUS_POSTED,
        STATUS_PENDING_CANCEL,
        Some(reason),
        operator.id,
        Some(instance_id),
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货单进入作废审批中"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 的状态在本次发起期间已被改动（期望 POSTED），请刷新后重试",
            row.in_no
        )));
    }
    Ok(instance_id)
}

/// **申请人撤回**在跑的作废审批：`PENDING_CANCEL → POSTED`，审批单落 `WITHDRAWN`。
///
/// 与「驳回」的区别只在审批单元数据的来源（谁撤的 / 为什么），单据侧动作完全一样：
/// 回到已过账，六本账一动没动，且**必须把实例终止**（否则 `uk_biz_flow_instance_biz_active`
/// 会让下一次发起永久 409 —— 这正是 P11 踩过的坑）。
pub async fn withdraw_cancel_approval(
    id: Uuid,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if row.status != STATUS_PENDING_CANCEL {
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），只有「作废审批中」的单子可以撤回作废申请",
            row.in_no,
            status_label(&row.status),
            row.status
        )));
    }
    let Some(operator_id) = op.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "撤回作废申请需要登录用户",
        ));
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启撤回事务"))?;
    let outcome = withdraw_cancel_tx(&tx, id, &row, operator_id, now).await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交撤回事务"))?;
            tracing::info!(
                stock_in_id = %id,
                in_no = %row.in_no,
                operator = %operator_id,
                "收货单作废申请已撤回（单据回到已过账，账未动）"
            );
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// [`withdraw_cancel_approval`] 的事务体。
///
/// 顺序：**先撤审批单**（`engine::withdraw` → 实例 `WITHDRAWN`，会校验「只有发起人能撤」），
/// 再改单据状态。反过来的话，撤审批失败时单据已经回到 POSTED —— 那就是
/// 「单据说没事、审批中心还挂着待办」的孤儿（卡面 ⑭ 的整笔回滚要求）。
pub(super) async fn withdraw_cancel_tx(
    tx: &Transaction,
    id: Uuid,
    row: &stockin_model::StockInRow,
    operator: Uuid,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 撤审批单：有在跑的实例走引擎（留 WITHDRAWN 轨迹 + 校验发起人身份）
    if let Some(instance_id) = row.approval_instance_id {
        let status = flow_model::find_instance(instance_id)
            .await
            .map_err(|error| utils::db_error(error, "查询作废审批单状态"))?
            .map(|instance| instance.status);
        if status.as_deref().map(str::to_ascii_uppercase).as_deref() == Some("PENDING") {
            let out = engine::withdraw(tx, instance_id, operator)
                .await
                .map_err(|error| write_error(error, "撤回作废审批单"))?;
            if let Some(rejected) = out.rejected {
                return Err(rejected);
            }
        } else {
            // 实例已不在跑（可能刚被审批推进）→ 用「按业务终止」兜底，保证不留孤儿
            stockin_bridge::cancel_approval_of(tx, id, &row.in_no, "申请人撤回作废申请")
                .await
                .map_err(|error| write_error(error, "终止作废审批单"))?;
        }
    }
    // ② 单据回到已过账（CAS，清空实例与申请理由）
    state_machine::transition(
        tx,
        state_machine::ENTITY_STOCK_IN,
        id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        STATUS_PENDING_CANCEL,
        STATUS_POSTED,
        operator,
        Some("申请人撤回作废申请"),
        None,
    )
    .await?;
    let affected = stockin_model::apply_status(
        tx,
        id,
        STATUS_PENDING_CANCEL,
        STATUS_POSTED,
        None,
        Some(operator),
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货单撤回作废"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 的状态在本次撤回期间已被改动（期望 {STATUS_PENDING_CANCEL}），请刷新后重试",
            row.in_no
        )));
    }
    Ok(())
}

/// 审批**通过**之后执行真作废（由 [`stockin_bridge::on_cancel_settled`] 调用）。
///
/// 复用 [`cancel_in_tx`]（P9 的六本账回滚），只把「起始状态」换成 `PENDING_CANCEL`：
/// 库存 / 库位账 / 流水（取流水里的 amount，不按现价）/ 红字凭证 / 应付作废 / 序列号软删，
/// 全在调用方开的一个事务里，任一步失败整笔回滚（单据仍是作废审批中）。
pub async fn cancel_after_approval(
    id: Uuid,
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let op = state_machine::Operator::of(operator);
    let now = Utc::now();
    // LNK-02：作废会把采购单状态重算（可能是 RECEIVED → PARTIAL_RECEIVED 的回退），
    // 那条边同样要触发联动；事务内动作随 tx 回滚，通知在 commit 之后 flush。
    let mut linkage_outbox = crate::business::erp::purchase_model::LinkageOutbox::default();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启审批作废事务"))?;
    match cancel_in_tx(&tx, id, row, Some(reason), &op, now, &mut linkage_outbox).await {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交审批作废事务"))?;
            linkage_outbox.flush().await;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 审批**未通过**之后把单据打回已过账（驳回 / 撤回 / 终止）。
///
/// 账一动没动 —— 这里只有状态 + 清理 `approval_instance_id`。
pub async fn restore_after_cancel_approval(
    id: Uuid,
    row: &stockin_model::StockInRow,
    verdict: stockin_bridge::Verdict,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let reason = match verdict {
        stockin_bridge::Verdict::Cancel => "作废审批通过（不应走到打回）",
        stockin_bridge::Verdict::Restore => "作废审批未通过，单据回到已过账",
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废打回事务"))?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_IN,
            id,
            &row.in_no,
            &stockin_model::LEGAL_EDGES,
            STATUS_PENDING_CANCEL,
            STATUS_POSTED,
            operator,
            Some(reason),
            None,
        )
        .await?;
        let affected = stockin_model::apply_status(
            &tx,
            id,
            STATUS_PENDING_CANCEL,
            STATUS_POSTED,
            None,
            operator,
            now,
        )
        .await
        .map_err(|error| write_error(error, "收货单作废打回"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "收货入库单 {} 的状态在本次打回期间已被改动（期望 {STATUS_PENDING_CANCEL}），请刷新后重试",
                row.in_no
            )));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废打回事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 作废事务体（P9 补齐库存回滚 + P2-C1 的凭证红冲 + P10 补序列号退账）：
/// CAS 改状态 → **退序列号 + 库存/库位账反向冲减 + 反向流水** → 红冲凭证 → 应付单作废，
/// **同一个事务**。
///
/// ⚠️ P9 之前这里只做凭证红冲，库存与流水**没退**（不变式 E4 挂在 2 组数据上）。
/// 现在按卡面 §一 的 7 步补齐：锁库存 → 校验可退 → 退成本账 → 退库位账 → 写
/// `OUT / PURCHASE_IN_CANCEL` 流水 → 红字凭证 → 应付作废。任一步失败 → 整单回滚
/// （handler 的 `tx.rollback()`），库存 / 凭证 / 流水三处同时不变。
///
/// ⚠️ P10 补上第 ② 步「退序列号」：一物一码物料的序列号行挂在
/// `source_type='PURCHASE' AND source_id = 本单` 上，作废不退会让
/// `biz_material_serial` 里留下 `IN_STOCK` 的幽灵号，与 `biz_inventory.qty`
/// （作废后已减）对不上 —— 就是 W2c 口径的账实不符。口径见
/// [`stockin_model::void_serials_of`]（未出库 → 软删；已出库/报废 → `409`）。
pub(super) async fn cancel_in_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    row: &stockin_model::StockInRow,
    reason: Option<&str>,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
    linkage_outbox: &mut crate::business::erp::purchase_model::LinkageOutbox,
) -> std::result::Result<(), ApiError> {
    // ⓪b P-FLOW-3A：过账审批中作废 → **先终止在跑的审批单**（同一事务）。
    //     不终止就是 P11 的老坑：单据作废了，审批中心还挂着一条待办，点进去 404；
    //     更糟的是 `uk_biz_flow_instance_biz_active` 会让这张单以后再也发不起审批。
    if row.status == STATUS_PENDING_POST {
        stockin_bridge::cancel_approval_of(ex, id, &row.in_no, "收货单在过账审批中作废")
            .await
            .map_err(|error| write_error(error, "作废：终止在跑的过账审批"))?;
    }
    // ⓪ P10 铁律 ③：状态变更先过唯一入口（边表 + reason 校验 + 写 biz_status_log）。
    //    `POSTED → CANCELLED` 的 reason 必填由状态机注册表统一把关（400），
    //    handler 那层的人性化文案保留，两处不会漂移（同一份 reason_edges）。
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_IN,
        id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        &row.status,
        STATUS_CANCELLED,
        operator.clone(),
        reason,
        None,
    )
    .await?;

    let affected = stockin_model::apply_status(
        ex,
        id,
        &row.status,
        STATUS_CANCELLED,
        reason,
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货入库单状态流转"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单状态在本次流转期间已被改动（期望 {}），请刷新后重试",
            row.status
        )));
    }

    // 作废的六本账回滚：`POSTED`（无流程降级 / DRAFT 之外的直接作废）与
    // `PENDING_CANCEL`（审批通过）走**同一段代码** —— 硬红线 5：不许为审批另写一套回滚。
    if row.status == STATUS_POSTED || row.status == STATUS_PENDING_CANCEL {
        // ⓪ 加锁顺序统一（P2 · 40P01）：**先锁采购单头，再碰库存行**。
        //
        // 过账路径是「收货单头 → 采购单头(FOR UPDATE) → 库存行」（`post_in_tx` ①b → ③），
        // 作废路径原本是「收货单头 → 库存行 → 采购单头」—— 采购单头要等到最后
        // [`stockin_model::sync_purchase_status`] 才锁。两条路同时处理**同一物料 + 同一仓库**
        // 就构成环：过账持采购单行等库存行、作废持库存行等采购单行，PG 在 `deadlock_timeout`
        // 内判 `40P01` 并中止其中一笔（整笔回滚、可重试，不会留半笔账），但用户看到的是
        // `500 服务器内部错误`。
        //
        // 提前锁不会削弱任何互斥力：`sync_purchase_status` 稍后再锁同一行时是**同一事务的
        // 重入**（不再等待），「并发取消采购单 / 另一张收货单过账」照样要等本事务结束。
        // 代价只是持锁时间从「作废尾部」变成「作废全程」—— 毫秒级。
        //
        // 证据：单测 `lockorder_concurrent_post_and_cancel_no_deadlock`（20 轮确定性交错：
        // 改前 PG 判死锁 20 次、改后 0 次），见 `docs/evidence/20261103-p2-收货过账作废加锁顺序.md`。
        if let Some(order_id) = row.source_order_id {
            let locked = stockin_model::lock_purchase_order(ex, order_id)
                .await
                .map_err(|error| write_error(error, "作废：锁定来源采购单"))?;
            if locked.is_none() {
                // 采购单已被软删（历史数据）：维持原口径 —— 后面 `sync_purchase_status`
                // 同样会「查不到单 → 返回 0」，不额外新增失败分支。
                tracing::warn!(
                    stock_in_id = %id,
                    in_no = %row.in_no,
                    %order_id,
                    "收货单作废：来源采购单不存在或已删除，跳过采购单头加锁（已收数量回退仍照常执行）"
                );
            }
        }

        // ① 读作废计划：数量取明细、金额取过账流水（不按现价估算）
        let plan = stockin_model::cancel_plan_of(ex, id, &row.in_no, row.warehouse_id)
            .await
            .map_err(|error| cancel_write_error(error, "作废：读取过账流水"))?;
        let doc = stockin_model::CancelDoc {
            id,
            in_no: &row.in_no,
        };
        // ② 退序列号（一物一码）：本单生成的号必须全部仍在库，否则 409；在库则软删
        //    （`deleted_at` 置位、行保留 —— 审计痕迹不丢，同号可被重新贴用）。
        //    放在库存冲减**之前**：一物一码物料的「货已出库」最硬的证据是序列号状态，
        //    先判它才能给出「哪一件已经出库」这种可执行报文，而不是笼统的「库存不足」。
        let serials_voided = stockin_model::void_serials_of(ex, id, &row.in_no, operator.id, now)
            .await
            .map_err(|error| cancel_write_error(error, "作废：退回一物一码序列号"))?;
        // ③ 逐行反向冲减（成本账 → 库位账 → 反向流水）；锁顺序 = 计划的排序（material_id 升序）
        let mut reversed_qty = Decimal::ZERO;
        let mut reversed_amount = Decimal::ZERO;
        for line in &plan {
            let applied = stockin_model::reverse_cost(
                ex,
                line.material_id,
                row.warehouse_id,
                line.location_id,
                line,
                now,
            )
            .await
            .map_err(|error| cancel_write_error(error, "作废：库存反向冲减"))?;
            stockin_model::reverse_location_qty(
                ex,
                line.material_id,
                row.warehouse_id,
                line.location_id,
                line.qty,
                now,
            )
            .await
            .map_err(|error| cancel_write_error(error, "作废：库位账反向冲减"))?;
            stockin_model::write_cancel_txn(ex, &doc, line, &applied, operator.id, now)
                .await
                .map_err(|error| write_error(error, "作废：写反向流水"))?;
            reversed_qty += line.qty;
            reversed_amount += line.amount;
        }

        // ④ 红字凭证（借贷互换，原凭证保持 POSTED 并打 reversed_by）
        let red = voucher::reverse_for_stock_in(ex, row, reason, operator.id, now)
            .await
            .map_err(|error| write_error(error, "作废：生成红字凭证"))?;
        // ⑤ 应付单 → CANCELLED
        payable::cancel_for_stock_in(ex, id, operator.id, now)
            .await
            .map_err(|error| write_error(error, "作废：应付单作废"))?;
        // ⑥ 重算采购单状态（D4）—— 已收数量**不用退**（P2-3）
        //
        // 本单在 ⓪ 步已经变成 `CANCELLED`，派生口径（`POSTED_RECEIVED_SQL`，
        // 只算 `status='POSTED'`）立刻不再把它算进「已收」，所以这里只要重算状态：
        // 采购单该从 `RECEIVED` 退回 `PARTIAL_RECEIVED` / 从 `PARTIAL_RECEIVED`
        // 退回可继续收的态。以前这里要先逐行 `revert_received_qty` 再重算，
        // 那是维护第二份数；现在数只有一份，重算读到的就是退完之后的真实值。
        if let Some(order_id) = row.source_order_id {
            stockin_model::sync_purchase_status(
                ex,
                order_id,
                &row.in_no,
                "收货作废",
                operator.id,
                now,
                linkage_outbox,
            )
            .await
            .map_err(|error| write_error(error, "作废：回退采购单状态"))?;
        }
        tracing::info!(
            stock_in_id = %id,
            in_no = %row.in_no,
            lines = plan.len(),
            reversed_qty = %reversed_qty,
            reversed_amount = %reversed_amount,
            serials_voided = serials_voided,
            red_voucher_id = %red.map(|v| v.to_string()).unwrap_or_else(|| "(无原凭证)".to_string()),
            "收货单作废：库存/库位账已反向冲减 + 反向流水已写 + 序列号已退回 + 红字凭证已生成 + 应付单已作废"
        );
        if red.is_none() {
            tracing::warn!(
                stock_in_id = %id,
                in_no = %row.in_no,
                "收货单作废：没有找到原凭证（历史数据），只作废了应付单"
            );
        }
    }
    Ok(())
}

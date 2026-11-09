// ============================================================================
// transfer_linkage.rs —— 库存调拨单（STOCK_TRANSFER）状态变更的**跨单据联动**接线（LNK-01）
//
// 从 `transfer_handler_status.rs` / `flow/task_bridge.rs` 抽出成一个 thin hook：
// 两处状态变更点（人工流转 + 审批回调）必须用**同一份**上下文与**同一份**排除清单，
// 各写一份迟早漂移（一处改了另一处不改 = 通知文案/收件人/幂等键全对不上）。
//
// # 为什么调拨要「排除清单」而不是直接 `engine::run`
//
// 调拨的库存落账不在通用引擎里，而在 `transfer_model::settle_transfer`
// （`IN_TRANSIT → DONE` 同事务：调出仓 − / 调入仓 + / 两条流水 / 成本原价转移）。
// 通用引擎再跑一次同类动作就是**重复落账**：
//
// | 动作码 | 调拨侧的专用落账点 | 通用引擎再跑一次的后果 |
// |--------|--------------------|------------------------|
// | `WRITE_INVENTORY` | `settle_transfer` ③ 两仓库存条件增减 | 同一行明细扣两次 / 加两次 |
// | `WRITE_INVENTORY_TXN` | `settle_transfer` ③ `TRANSFER_OUT` / `TRANSFER_IN` 流水 | 一张单四条流水，报表按流水汇总就翻倍 |
// | `GENERATE_VOUCHER` | 调拨**不抛凭证**（只搬成本），该动作也只接管 `STOCK_IN` 取数 | 空跑一条噪音规则 |
//
// 排除清单在 `linkage::spec::TRANSFER_EXCLUDED_ACTIONS`（调用方传入、不写死在引擎里），
// 漂移守卫：`tests_wiring::transfer_status_wires_generic_engine_and_excludes_inventory`。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔回滚（不留半条状态变更）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**（账已落定，一封站内信失败不该把状态退回去）。
//
// # 触发点覆盖
//
// | 状态变更点 | 调用方 | 目标态 |
// |-----------|--------|--------|
// | 提交 / 审核 / 发运 / 收货 / 作废（人工流转） | `transfer_handler_status::apply_transition` | PENDING / APPROVED / IN_TRANSIT / DONE / CANCELLED |
// | 审批通过 / 驳回（流程回调） | `flow::task_bridge::settle_transfer` | APPROVED / DRAFT |
//
// ⚠️ 审批回调那条路**绕开了** `change_transfer_status`（它直接 `erp_transfer::settle_approval`
// 写状态），只接人工流转会让「配了已发布流程的生产环境」里 `STOCK_TRANSFER/APPROVED`
// 的规则**永远不触发** —— 正是 P1-FIX-5C 那类「看起来配好了」的静默失效。
// ============================================================================

use crate::business::erp::transfer_base::status_label;
use crate::business::flow::linkage;
use crate::business::notice::{self, dto::NoticeLevel, dto::NoticeType};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 调拨单状态变更的联动上下文（人工流转与审批回调**同形**）。
///
/// 收件人 / 文案 / 幂等键都取自单据本身：配置只在 `action_params` 里说「发给谁、覆盖哪些字段」，
/// 不在第二处维护文案（与出库过账 `stockout_linkage::post_linkage_ctx` 同一口径）。
pub(crate) fn status_ctx<'a>(
    doc_id: Uuid,
    doc_no: &'a str,
    status: &'a str,
    operator: Option<Uuid>,
    creator: Option<Uuid>,
) -> linkage::LinkageContext<'a> {
    let label = status_label(status);
    // 默认收件人 = 制单人（「我的单子动了一步」）；没有制单人（历史脏单）退到操作人。
    // 两者都没有 → 空收件人，`SEND_NOTICE` 会自己判 Skipped，不会静默发错人。
    let recipient = creator.or(operator);
    let mut draft = notice::NoticeDraft::text(
        NoticeType::Workflow,
        NoticeLevel::Info,
        format!("调拨单{label}"),
        format!("调拨单「{doc_no}」状态已变更为「{label}」"),
        notice::NoticeTarget::User(recipient.into_iter().collect()),
    );
    draft.link_url = Some(format!("/business/erp/transfer?id={doc_id}"));
    draft.biz_type = Some("STOCK_TRANSFER".to_string());
    draft.biz_id = Some(doc_id.to_string());
    draft.sender_name = Some("库存调拨".to_string());
    // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
    draft.force = false;
    draft.idempotency_key = Some(format!("LNK:STOCK_TRANSFER:{status}:{doc_id}"));
    linkage::LinkageContext::new(
        crate::business::shared::state_machine::ENTITY_TRANSFER,
        status,
        doc_id,
        doc_no,
        operator,
    )
    .with_people(operator, creator, None)
    .with_notice(draft)
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除调拨专用落账动作）。
///
/// 返回的 [`linkage::LinkageRun`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    doc_id: Uuid,
    doc_no: &str,
    status: &str,
    operator: Option<Uuid>,
    creator: Option<Uuid>,
) -> std::result::Result<linkage::LinkageRun, ApiError> {
    let ctx = status_ctx(doc_id, doc_no, status, operator, creator);
    let run = linkage::engine::run_excluding(ex, &ctx, linkage::spec::TRANSFER_EXCLUDED_ACTIONS).await?;
    if !run.is_empty() {
        tracing::info!(
            transfer_id = %doc_id,
            transfer_no = doc_no,
            status,
            in_tx_rules = run.runs.len(),
            deferred_rules = run.deferred.len(),
            "调拨单状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(run)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 状态已经在库里落定，
/// 因为一封站内信写失败就把状态变更回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(
    doc_id: Uuid,
    doc_no: &str,
    status: &str,
    operator: Option<Uuid>,
    creator: Option<Uuid>,
    run: linkage::LinkageRun,
) {
    if run.deferred.is_empty() {
        return;
    }
    let ctx = status_ctx(doc_id, doc_no, status, operator, creator);
    for item in linkage::engine::run_deferred(&ctx, run.deferred).await {
        tracing::info!(
            transfer_id = %doc_id,
            transfer_no = doc_no,
            status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "调拨单状态变更：提交后的联动动作执行结果"
        );
    }
}

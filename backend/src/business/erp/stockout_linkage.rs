// ============================================================================
// stockout_linkage.rs —— 出库过账的**跨单据联动**接线（P1-FIX-5C）
//
// 从 `stockout_handler.rs` 抽出来是为了守住单文件 ≤1500 行的门禁，不是新分层：
// 这两个函数只被 `stockout_handler::{post_stock_out, post_out_tx}` 调用，
// 通过 `#[path = "stockout_linkage.rs"] mod linkage_hook;` 挂在 handler 下面。
//
// # 为什么出库要「排除清单」而不是直接 `engine::run`
//
// 出库过账自己就把三本账 + 成本结转凭证写完了（`post_out_tx` ③④⑤）：
//
// | 动作码 | 出库侧的专用落账点 | 通用引擎再跑一次的后果 |
// |--------|--------------------|------------------------|
// | `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` | ③ 逐行 `deduct_cost` / `deduct_location_qty` / `write_txn` | 同一行明细扣两次库存 |
// | `UPDATE_COST` | ③ 明细回写 `avg_cost` / `amount` | 成本快照被二次覆盖 |
// | `GENERATE_VOUCHER` | ⑤ `voucher::create_for_stock_out` | 一张单两张成本结转凭证 |
//
// 所以调用方必须**显式**把这四类交给专用路径（`spec::STOCKOUT_EXCLUDED_ACTIONS`），
// 而不是把「排除」写死进引擎 —— 换个触发点（如 `PAYABLE/SETTLED`）通用路线仍然要能跑这些动作。
// ============================================================================

use crate::business::flow::linkage;
use crate::business::notice::{self, dto::NoticeLevel, dto::NoticeType};
use crate::business::shared::state_machine;

// 注意：本文件是 `stockout_handler::linkage_hook` 子模块（由 handler 里的
// `#[path = "stockout_linkage.rs"] mod linkage_hook;` 挂上），`super` 是 handler 而非 `erp`，
// 所以兄弟模块一律走绝对路径。
use crate::business::erp::stockout_base::{STATUS_POSTED, StockOutRow};

/// 出库过账触发点（`STOCK_OUT/POSTED`）的联动上下文。
///
/// 构造两处（事务内 `run_excluding`、提交后 `run_deferred`）必须同形，所以收成一个函数：
/// 收件人 / 文案 / 幂等键都取自单据本身，配置只需要在 `action_params` 里说
/// 「发给谁、覆盖哪些字段」，不重复维护第二份文案。
pub(super) fn post_linkage_ctx<'a>(
    row: &'a StockOutRow,
    operator: &state_machine::Operator,
) -> linkage::LinkageContext<'a> {
    let mut draft = notice::NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        "出库单已过账",
        format!("出库单「{}」已过账：库存与成本结转凭证已在同一事务里落库", row.out_no),
        notice::NoticeTarget::User(row.create_by.into_iter().collect()),
    );
    draft.link_url = Some(format!("/business/erp/stock-out?id={}", row.id));
    draft.biz_type = Some("STOCK_OUT".to_string());
    draft.biz_id = Some(row.id.to_string());
    draft.sender_name = Some("出库管理".to_string());
    // 「已过账」是知情类通知，尊重 notify.inbox（漏看不造成损失）
    draft.force = false;
    draft.idempotency_key = Some(format!("LNK:STOCK_OUT:POSTED:{}", row.id));
    linkage::LinkageContext::new(
        state_machine::ENTITY_STOCK_OUT,
        STATUS_POSTED,
        row.id,
        &row.out_no,
        operator.id,
    )
    .with_people(operator.id, row.create_by, None)
    .with_notice(draft)
}

/// 提交**之后**执行联动的 `AfterCommit` 动作（发通知 / 建任务这类）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 账已经在库里落定，
/// 因为一封站内信写失败就把「已经过账」回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(super) async fn run_deferred_linkage(
    row: &StockOutRow,
    operator: &state_machine::Operator,
    run: linkage::LinkageRun,
) {
    if run.deferred.is_empty() {
        return;
    }
    let ctx = post_linkage_ctx(row, operator);
    for item in linkage::engine::run_deferred(&ctx, run.deferred).await {
        tracing::info!(
            stock_out_id = %row.id,
            out_no = %row.out_no,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "出库过账：提交后的联动动作执行结果"
        );
    }
}

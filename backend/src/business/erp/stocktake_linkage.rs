// ============================================================================
// stocktake_linkage.rs —— 盘点单（STOCK_TAKE）状态变更的**跨单据联动**接线（LNK-04）
//
// 盘点的两个状态变更点都在 `erp/stocktake_handler.rs`：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 过账 | `post_stock_take` → `post_tx` | `PENDING_POST → POSTED` |
// | 作废 | `change_status` / `cancel_stock_take` → `cancel_tx` | `DRAFT / PENDING_POST / POSTED → CANCELLED` |
//
// 两条路都在**同一个事务**里改状态并落账，所以联动只在这里写一份上下文
// （单据号 / 收件人 / 文案 / 幂等键），两处各写一份迟早漂移 —— 与
// `stockin_handler_post.rs` 的 ⑦⑧ 步、`payable_linkage.rs` 同一口径。
//
// # 为什么盘点要「排除清单」而不是直接 `engine::run`
//
// 过账事务**自己已经写完了**下面三类账，通用引擎再跑一次就是重复落账：
//
// | 动作码 | 盘点侧的专用落账点 | 通用引擎再跑一次的后果 |
// |--------|--------------------|------------------------|
// | `WRITE_INVENTORY` | `post_tx` ③ 的 `apply_gain` / `apply_loss`（成本账） | 同一物料加减两次，账面数量 ≠ 实盘数 |
// | `WRITE_INVENTORY_TXN` | `apply_gain` / `apply_loss` 里的 `biz_inventory_txn` 流水 | 一张单四条流水，进销存台账翻倍 |
// | `GENERATE_VOUCHER` | `post_tx` ⑤ 的 `create_voucher`（借 1405 / 贷 1901 成对） | 一张盘点单两张差异凭证，总账翻倍 |
//
// 排除清单在 `linkage::spec::STOCKTAKE_EXCLUDED_ACTIONS`（调用方传入、不写死在引擎里），
// 漂移守卫：`linkage::tests_wiring::stocktake_wires_generic_engine_and_excludes_inventory`
// + 真库反证 `erp::stocktake_tests::excluded_inventory_actions_do_not_double_write`。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔回滚
//   （不会留下「状态推了、账动了、联动没跑」的半截单）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**（账已落定，一封站内信失败不该把盘盈盘亏退回去）。
//
// # 幂等
//
// 过账 / 作废都是**一次性**动作：过账 `affected == 0`（状态已被改）直接 409，
// 作废 `CANCELLED → *` 不在合法边里，重复点击不会第二次进 `post_tx` / `cancel_tx`，
// 所以联动天然只跑一次；通知另有幂等键 `LNK:STOCK_TAKE:<状态>:<单据 id>` 兜底。
// ============================================================================

use crate::business::erp::stocktake_model::{self, StockTakeRow};
use crate::business::flow::linkage;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self};
use crate::business::shared::state_machine;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 一次盘点单状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份 `take_no` / 状态 / 收件人），调用方只要把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给
/// [`run_deferred_after_commit`] 即可 —— 不需要第二次读库（那时可能已被别人改过）。
pub(crate) struct StockTakeStatusChange {
    /// 盘点单 id（幂等键 / 日志）。
    pub(crate) take_id: Uuid,
    /// 盘点单号（通知文案 / 日志）。
    pub(crate) take_no: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 操作人。
    pub(crate) operator: Option<Uuid>,
    /// 制单人（`recipient = CREATOR` 时的默认收件人）。
    pub(crate) creator: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl StockTakeStatusChange {
    /// 由单据 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自单据本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与收货 `stockin_post_linkage_ctx` / 付款 `payable_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        let label = stocktake_model::status_label(&self.status);
        // 默认收件人 = 制单人（「我的单子动了一步」）；没有制单人（历史脏单）退到操作人。
        // 两者都没有 → 空收件人，`SEND_NOTICE` 自己判 Skipped，不会静默发错人。
        let recipient = self.creator.or(self.operator);
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Todo,
            NoticeLevel::Info,
            format!("盘点单{label}"),
            format!("盘点单「{}」状态已变更为「{label}」", self.take_no),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/stock-take?id={}", self.take_id));
        draft.biz_type = Some("STOCK_TAKE".to_string());
        draft.biz_id = Some(self.take_id.to_string());
        draft.sender_name = Some("库存盘点".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        draft.idempotency_key = Some(format!("LNK:STOCK_TAKE:{}:{}", self.status, self.take_id));
        linkage::LinkageContext::new(
            state_machine::ENTITY_STOCK_TAKE,
            &self.status,
            self.take_id,
            &self.take_no,
            self.operator,
        )
        .with_people(self.operator, self.creator, None)
        .with_notice(draft)
    }
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除盘点专用落账动作）。
///
/// 返回的 [`StockTakeStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    header: &StockTakeRow,
    status: &str,
    operator: Option<Uuid>,
) -> std::result::Result<StockTakeStatusChange, ApiError> {
    let mut change = StockTakeStatusChange {
        take_id: header.id,
        take_no: header.take_no.clone(),
        status: status.to_string(),
        operator,
        creator: header.create_by,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run =
        linkage::engine::run_excluding(ex, &ctx, linkage::spec::STOCKTAKE_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            take_id = %change.take_id,
            take_no = change.take_no,
            status,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "盘点单状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 账已经在库里落定，
/// 因为一封站内信写失败就把盘盈盘亏回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: StockTakeStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            take_id = %change.take_id,
            take_no = change.take_no,
            status = change.status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "盘点单状态变更：提交后的联动动作执行结果"
        );
    }
}

// ============================================================================
// payable_linkage.rs —— 应付单（PAYABLE）状态变更的**跨单据联动**接线（LNK-03）
//
// 从 `payable_payment.rs::settle_payment_in_tx` 抽出成一个 thin hook：应付单的状态在
// **两条付款路径**上推进（直付 `pay_in_tx` / 审批通过 `settle_approved_in_tx`），
// 但两条路最终都汇到同一个记账函数 `settle_payment_in_tx`（P7 的唯一实现），
// 所以联动上下文与排除清单只在这里写一份 —— 否则两处各写一份迟早漂移
// （一处改了另一处不改 = 通知文案 / 收件人 / 幂等键全对不上）。
//
// # 为什么应付要「排除清单」而不是直接 `engine::run`
//
// 付款事务**自己已经写完了**下面两类账，通用引擎再跑一次就是重复落账：
//
// | 动作码 | 付款侧的专用落账点 | 通用引擎再跑一次的后果 |
// |--------|--------------------|------------------------|
// | `UPDATE_PAYABLE_BALANCE` | `settle_payment_in_tx` ⑤ 的 `paid_amount / unpaid_amount / status` 条件回写 | 同一笔钱扣两次：已付翻倍、未付变负数、状态原地跳 |
// | `GENERATE_VOUCHER` | `settle_payment_in_tx` ② 的 `voucher::post_voucher`（借 2202 / 贷 1002） | 一笔付款两张付款凭证，总账按凭证汇总就翻倍 |
//
// 排除清单在 `linkage::spec::PAYABLE_EXCLUDED_ACTIONS`（调用方传入、不写死在引擎里），
// 漂移守卫：`tests_wiring::payable_status_wires_generic_engine_and_excludes_balance_and_voucher`。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔付款回滚
//   （不会留下「钱登记了、状态没推、联动没跑」的半截账）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**（付款已落定，一封站内信失败不该把钱退回去）。
//
// # 触发点覆盖
//
// | 状态变更点 | 调用方 | 目标态 |
// |-----------|--------|--------|
// | 直付（无流程 / 显式跳过审批） | `payable_payment::pay_in_tx` → `settle_payment_in_tx` | `OPEN → PARTIAL_PAID` / `→ SETTLED` |
// | 审批通过后执行 | `payable_payment::settle_approved_in_tx` → `settle_payment_in_tx` | 同上 |
//
// ⚠️ 状态**没变**时不触发：第二次部分付款是 `PARTIAL_PAID → PARTIAL_PAID`，
// `state_machine::transition_if_changed` 返回 `None`（原地流转不写状态日志），
// 这种情况下联动也不许再跑一次 —— 否则「配一条通知规则，客户付几笔就收几条一模一样的信」。
// ============================================================================

use crate::business::erp::payable;
use crate::business::erp::payable_query;
use crate::business::flow::linkage;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 一次应付单状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份 `payable_no` / 状态 / 收件人），所以调用方
/// 只要把 `run_in_tx` 的返回值留着，`commit()` 之后原样交给
/// [`run_deferred_after_commit`] 即可 —— 不需要第二次读库，也不会出现
/// 「提交后用一份新取的单据重算文案」导致的漂移。
pub(crate) struct PayableStatusChange {
    /// 应付单 id（幂等键 / 日志）。
    pub(crate) payable_id: Uuid,
    /// 应付单号（通知文案 / 日志）。
    pub(crate) payable_no: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 操作人。
    pub(crate) operator: Option<Uuid>,
    /// 制单人（`recipient = CREATOR` 时的默认收件人）。
    pub(crate) creator: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl PayableStatusChange {
    /// 由单据 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自单据本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与出库过账 `stockout_linkage` / 调拨 `transfer_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        let label = payable_query::status_label(&self.status);
        // 默认收件人 = 制单人（「我的单子动了一步」）；没有制单人（历史脏单）退到操作人。
        // 两者都没有 → 空收件人，`SEND_NOTICE` 自己判 Skipped，不会静默发错人。
        let recipient = self.creator.or(self.operator);
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            format!("应付单{label}"),
            format!("应付单「{}」状态已变更为「{label}」", self.payable_no),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/payable?id={}", self.payable_id));
        draft.biz_type = Some("PAYABLE".to_string());
        draft.biz_id = Some(self.payable_id.to_string());
        draft.sender_name = Some("应付付款".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        draft.idempotency_key = Some(format!("LNK:PAYABLE:{}:{}", self.status, self.payable_id));
        linkage::LinkageContext::new(
            crate::business::shared::state_machine::ENTITY_PAYABLE,
            &self.status,
            self.payable_id,
            &self.payable_no,
            self.operator,
        )
        .with_people(self.operator, self.creator, None)
        .with_notice(draft)
    }
}

/// 读制单人（`biz_payable.create_by`；`payable::PayableRow` 不映射该列，这里补一次主键查询）。
///
/// 只在 `run_in_tx` 里读一次，读到的值随 `PayableStatusChange` 带到提交之后 ——
/// 提交后不再读库（那时事务已结束，读到的可能是别人改过的值）。
async fn creator_of(ex: &impl SqlExecutor, payable_id: Uuid) -> DbResult<Option<Uuid>> {
    // SCOPE-EXEMPT: 应付台账无归属列（见 payable 模块文档）。
    let row = ex
        .query_first_sql(
            format!(
                "SELECT create_by FROM {} WHERE id = ?::uuid AND deleted_at IS NULL",
                payable::TABLE
            ),
            vec![Value::from(&payable_id)],
        )
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let creator: Option<Uuid> = row.get_typed("create_by")?;
    Ok(creator)
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除应付专用落账动作）。
///
/// 返回的 [`PayableStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    row: &payable::PayableRow,
    status: &str,
    operator: Option<Uuid>,
) -> std::result::Result<PayableStatusChange, ApiError> {
    let creator = creator_of(ex, row.id)
        .await
        .map_err(|error| ApiError::internal(format!("读取应付单制单人失败：{error}")))?;
    let mut change = PayableStatusChange {
        payable_id: row.id,
        payable_no: row.payable_no.clone(),
        status: status.to_string(),
        operator,
        creator,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run =
        linkage::engine::run_excluding(ex, &ctx, linkage::spec::PAYABLE_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            payable_id = %change.payable_id,
            payable_no = change.payable_no,
            status,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "应付单状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 付款已经在库里落定，
/// 因为一封站内信写失败就把付款回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: PayableStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            payable_id = %change.payable_id,
            payable_no = change.payable_no,
            status = change.status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "应付单状态变更：提交后的联动动作执行结果"
        );
    }
}

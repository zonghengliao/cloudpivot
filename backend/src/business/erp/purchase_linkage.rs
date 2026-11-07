// ============================================================================
// purchase_linkage.rs —— 采购订单（PURCHASE_ORDER）状态变更的跨单据联动接线（LNK-02）
//
// 从两处状态写入点抽出成**一份** hook：「CAS 改状态之后跑联动」这件事只在这里写一遍：
//
// | 状态变更点 | 调用方 | 目标态 |
// |-----------|--------|--------|
// | 提交 / 审批通过 / 驳回 / 关闭 / 作废（含联动作废） | `purchase_model::apply_status[_with_instance]` | SUBMITTED / APPROVED / DRAFT / CLOSED / CANCELLED |
// | 部分收货 / 全部收货 / 收货作废回退 | `erp/stockin_purchase.rs::sync_purchase_status` | PARTIAL_RECEIVED / RECEIVED |
//
// 两处**都要接**：收货推进走的是 `state_machine::transition_if_changed`，
// 完全不经过 `apply_status`。只接一处，「部分收货 → 全部收货」这条边就永远不触发联动
// （规则能保存、能启用、永远不响 —— P1-FIX-5C 那类静默失效）。
//
// # 为什么调拨要「排除清单」而不是直接 `engine::run`
//
// 采购单自己的状态变更**不落任何账**：库存 / 流水 / 凭证 / 已收数量全部由
// **收货单过账**（`stockin_handler_post.rs`）在同一个事务里写，采购单只是被动推状态。
// 所以通用引擎在这个触发点上必须把下面 4 类动作交给专用路径：
//
// | 动作码 | 采购侧的真实落账点 | 通用引擎再跑一次的后果 |
// |--------|--------------------|------------------------|
// | `WRITE_INVENTORY` | `post_in_tx` ③ 库存成本账 / 库位账 | 同一物料加两次库存 |
// | `WRITE_INVENTORY_TXN` | `post_in_tx` ② 流水 | 一张收货单两条流水，报表翻倍 |
// | `GENERATE_VOUCHER` | `post_in_tx` ⑦ 进项凭证 | 一张收货单两张凭证 |
// | `UPDATE_RECEIVED_QTY` | **不需要写**：已收数量是派生值（`POSTED_RECEIVED_SQL`） | 累计量被二次推进（口径彻底分叉） |
//
// 排除清单在 `linkage::spec::PURCHASE_EXCLUDED_ACTIONS`（调用方传入、不写死在引擎里），
// 漂移守卫：`tests_wiring::purchase_order_wires_generic_engine_and_excludes_stock_actions`。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔回滚
//   （不留半条状态变更）。**CAS 成功（affected > 0）才跑**：状态没变就不该通知；
// - 提交后：`LinkageOutbox::flush` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**。
//
// 为什么用 outbox（而不是让 hook 返回一个 run）：采购单状态变更的调用方有 6 处
// （PATCH 状态 / 提交审批 / 审批回调 / 联动作废 / 收货过账 / 收货作废），其中联动作废
// 会**在一次事务里推多次**状态（逐张收货单作废都会重算采购单状态）。
// 调用方各自持有一个 outbox、提交后一次 `flush`，比给每处改返回值更不容易漏。
// ============================================================================

use crate::business::erp::purchase_model::status_label;
use crate::business::flow::linkage;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeTarget};
use nik::prelude::*;
use nik::uuid::Uuid;

/// 一条「已跑完事务内动作、待提交后执行」的联动记录。
///
/// 携带触发点的完整上下文（单号 / 目标态 / 人），因为 `engine::run_deferred`
/// 要用**同一个** ctx 去构造通知草稿与收件人 —— 复用别的触发点的 ctx 会把通知发错人。
pub struct PendingLinkage {
    /// 触发单据主键。
    pub(crate) doc_id: Uuid,
    /// 触发单据编号。
    pub(crate) doc_no: String,
    /// 触发状态（变更**之后**的状态）。
    pub(crate) status: String,
    /// 操作人。
    pub(crate) operator: Option<Uuid>,
    /// 制单人（默认收件人）。
    pub(crate) creator: Option<Uuid>,
    /// 事务内执行结果（`deferred` 待提交后执行）。
    pub(crate) run: linkage::LinkageRun,
}

/// 一次业务操作里累积的待提交后联动（提交之后一次 `flush`）。
#[derive(Default)]
pub struct LinkageOutbox {
    pending: Vec<PendingLinkage>,
}

impl LinkageOutbox {
    /// 收一条（空 run 直接丢：没命中任何规则 = 行为与配置前逐字相同）。
    pub(crate) fn push(&mut self, item: PendingLinkage) {
        if !item.run.is_empty() {
            self.pending.push(item);
        }
    }

    /// 提交**之后**执行全部 `AfterCommit` 动作。
    ///
    /// 铁律：永不 panic、永不回滚业务 —— 账已经在库里落定，
    /// 因为一封站内信写失败就把状态变更回滚掉，用户点第二次还会失败第二次。
    pub(crate) async fn flush(self) {
        for item in self.pending {
            item.run_after_commit().await;
        }
    }
}

impl PendingLinkage {
    /// 提交后执行本条的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
    async fn run_after_commit(self) {
        let PendingLinkage {
            doc_id,
            doc_no,
            status,
            operator,
            creator,
            run,
        } = self;
        if run.deferred.is_empty() {
            return;
        }
        let ctx = status_ctx(doc_id, &doc_no, &status, operator, creator);
        for item in linkage::engine::run_deferred(&ctx, run.deferred).await {
            tracing::info!(
                purchase_order_id = %doc_id,
                order_no = doc_no,
                status,
                rule_id = %item.rule_id,
                action_code = %item.action_code,
                outcome = item.outcome.label(),
                reason = item.outcome.detail(),
                "采购单状态变更：提交后的联动动作执行结果"
            );
        }
    }
}

/// 采购单状态变更的联动上下文（两处状态写入点**同形**）。
///
/// 收件人 / 文案 / 幂等键都取自单据本身：配置只在 `action_params` 里说「发给谁、覆盖哪些字段」，
/// 不在第二处维护文案（与出库 `stockout_linkage` / 调拨 `transfer_linkage` 同一口径）。
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
        format!("采购订单{label}"),
        format!("采购订单「{doc_no}」状态已变更为「{label}」"),
        NoticeTarget::User(recipient.into_iter().collect()),
    );
    draft.link_url = Some(format!("/business/erp/purchase-order?id={doc_id}"));
    draft.biz_type = Some("PURCHASE_ORDER".to_string());
    draft.biz_id = Some(doc_id.to_string());
    draft.sender_name = Some("采购订单".to_string());
    // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
    draft.force = false;
    draft.idempotency_key = Some(format!("LNK:PURCHASE_ORDER:{status}:{doc_id}"));
    linkage::LinkageContext::new(
        crate::business::shared::state_machine::ENTITY_PURCHASE_ORDER,
        status,
        doc_id,
        doc_no,
        operator,
    )
    .with_people(operator, creator, None)
    .with_notice(draft)
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除采购侧由收货单落账的动作）。
///
/// `status` 是**变更之后**的目标态（调用方刚从 CAS 里拿到 `affected > 0` 才进来）。
/// 结果（含 `deferred`）塞进调用方的 [`LinkageOutbox`]，由它在 `commit()` 之后 flush。
///
/// 返回 `DbResult`（而不是 `ApiError`）：三处调用方（`purchase_model` /
/// `stockin_purchase`）本身就在数据层、返回 `DbResult`，把引擎的 `ApiError`
/// 折成 `DbError::InvalidValue` 可以让它们直接 `?`，调用方的事务体一行都不用改写法。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    doc_id: Uuid,
    status: &str,
    operator: Option<Uuid>,
    outbox: &mut LinkageOutbox,
) -> DbResult<()> {
    // 单号 / 制单人：通知文案与默认收件人都要，一次按主键读（不额外索引扫描）
    let rows = ex
        .query_sql(
            "SELECT order_no, create_by FROM biz_purchase_order WHERE id = ?::uuid".to_string(),
            vec![Value::from(&doc_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        // 单据不存在（正常路径走不到：CAS 刚成功过）。不阻断状态变更。
        return Ok(());
    };
    let order_no: String = row.get_typed("order_no")?;
    let creator: Option<Uuid> = row.get_typed("create_by")?;
    let ctx = status_ctx(doc_id, &order_no, status, operator, creator);
    let run = linkage::engine::run_excluding(ex, &ctx, linkage::spec::PURCHASE_EXCLUDED_ACTIONS)
        .await
        .map_err(to_db_error)?;
    if !run.is_empty() {
        tracing::info!(
            purchase_order_id = %doc_id,
            order_no,
            status,
            in_tx_rules = run.runs.len(),
            deferred_rules = run.deferred.len(),
            "采购单状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    outbox.push(PendingLinkage {
        doc_id,
        doc_no: order_no,
        status: status.to_string(),
        operator,
        creator,
        run,
    });
    Ok(())
}

/// 引擎的 `ApiError` → `DbError`（数据层的错误出口只有 `DbError`）。
///
/// 关键动作失败（`run_excluding` 返回 `Err`）必须让整笔状态变更回滚，
/// 所以这里**保留**引擎的业务码与文案，handler 层 `utils::db_error` 会原样透出。
fn to_db_error(error: ApiError) -> DbError {
    DbError::InvalidValue(format!("[{}] {}", error.code, error.msg))
}

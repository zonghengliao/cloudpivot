// ============================================================================
// voucher_linkage.rs —— 凭证（VOUCHER）状态变更的**跨单据联动**接线（LNK-06）
//
// 凭证的三个状态（`DRAFT` / `AUDITED` / `POSTED`）全部是**能走到的**，所以每个推进点
// 都必须调引擎（只接「过账」= `DRAFT` / `AUDITED` 上的规则能存能启永不触发，
// 正是 P1-FIX-5C 记录的那个病）：
//
// | 触发点 | 入口（事务体） | 目标态 |
// |--------|----------------|--------|
// | 手工凭证落草稿 | `voucher_write.rs::manual_in_tx`（`create_manual_voucher` 的事务体） | 新建 → `DRAFT` |
// | 审核 | `voucher_write.rs::transition_voucher_in_tx`（`audit_voucher`） | `DRAFT → AUDITED` |
// | 反审核 | 同上（`un_audit_voucher`） | `AUDITED → DRAFT` |
// | 记账 | 同上（`post_audited_voucher`） | `AUDITED → POSTED` |
// | 红冲 | `voucher_write.rs::reverse_in_tx`（`reverse_voucher`） | 红字凭证新建 → `POSTED` |
// | 红冲审批通过 | `voucher_write.rs::reverse_after_approval`（`voucher_bridge` 回调） | 红字凭证新建 → `POSTED` |
//
// 三条状态边共用 `transition_voucher_in_tx`（审核链的唯一事务体），所以「一条接线点覆盖
// 三条边」不是偷懒：那里就是状态列的唯一写入点。
//
// # ⚠️ 为什么会死循环，以及两道闸
//
// `GENERATE_VOUCHER` 是「按联动规则生成一张凭证」的动作。把它配在**凭证自己**的状态上，
// 一条规则就能自我复现：凭证过账 → 触发规则 → 生成新凭证（`POSTED`）→ 又触发 → …
// 这不是「重复落账」，是不收敛的递归（每一轮都落一张新凭证 + 取一次号）。
//
// 两道闸，缺一不可：
//
// 1. **运行时**：本模块一律 `run_excluding(linkage::spec::VOUCHER_EXCLUDED_ACTIONS)`，
//    命中的规则不产生 `RuleRun`、交给专用路径（凭证只能由来源单据的记账路径生成）；
// 2. **配置期**：`linkage::spec::VOUCHER_FORBIDDEN_ACTIONS` 让写入口（`model::upsert_rule`）
//    与配置页预览（`admin::check_draft`）直接 **400**。运行时排除只兜历史脏行 / 绕过 API
//    直接写库的行 —— 不允许管理员配得上一条永远不执行的规则。
//
// 漂移守卫：`linkage::tests_wiring::voucher_wires_generic_engine_excludes_and_forbids_generate`
// + 真库用例 `erp::voucher_linkage_tests::{lnk06_generate_voucher_rule_is_rejected_on_save,
// lnk06_excluded_generate_voucher_never_runs}`。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔凭证状态变更回滚
//   （不会留下「状态推了、联动没跑」的半截凭证）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**（凭证已经记账落定，一封站内信失败不该把
//   过账退回去）。
//
// # 幂等
//
// - 审核 / 反审核 / 记账：`transition_voucher_in_tx` 对「起点态不符」直接 409，
//   重复点击不会第二次进事务 ⇒ 联动也不会跑第二次；
// - 手工凭证：每次新增都是新凭证（新 id），幂等键含凭证 id；
// - 红冲：`reverse_in_tx` 在行锁内查 `reversed_by`，第二次红冲 409 ⇒ 不重复触发。
//
// # 口径（写进 spec::TRIGGER_SPECS 的 wiring）
//
// 只接「凭证自身的人工 / 红冲状态流转」。来源单据（收货 / 付款 / 报销 …）在
// `voucher::post_voucher` 里自动落 `POSTED` 的凭证**不再触发一次**：那条路径本身跑在
// 别的单据的联动事务里（`actions::generate_voucher` → `create_by_rule` → `post_voucher`），
// 在它内部再调一次引擎等于嵌套联动，同一次业务动作会跑两遍规则。
// ============================================================================

use crate::business::erp::voucher::{self, VoucherRow};
use crate::business::flow::linkage;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;
use crate::utils;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 凭证联动通知的 `biz_type`（与流程引擎发的待办区分开）。
const NOTICE_BIZ_TYPE: &str = "VOUCHER";

/// 一次凭证状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份 `voucher_no` / 状态 / 收件人）：调用方把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给 [`run_deferred_after_commit`] 即可，
/// 不需要第二次读库（那时凭证可能已被别人改过）。
pub(crate) struct VoucherStatusChange {
    /// 凭证 id（幂等键 / 日志）。红冲时是**红字凭证**的 id。
    pub(crate) voucher_id: Uuid,
    /// 凭证号（通知文案 / 日志）。
    pub(crate) voucher_no: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 操作人。
    pub(crate) operator: Option<Uuid>,
    /// 制单人（`recipient = CREATOR`）。
    pub(crate) creator: Option<Uuid>,
    /// 记账人（`recipient = OWNER`：凭证的「负责人」就是把它推进总账的人）。
    pub(crate) posted_by: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl VoucherStatusChange {
    /// 由凭证 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自凭证本身）。
    ///
    /// 状态中文名取自 `spec::status_label_of`（后端唯一一份编码 → 中文映射），
    /// 不在这里另写一套。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        let label = linkage::spec::status_label_of(state_machine::ENTITY_VOUCHER, &self.status)
            .unwrap_or(self.status.as_str());
        // 默认收件人 = 操作人（谁动的凭证谁关心），退制单人，再退记账人；
        // 都没有 → 空收件人，`SEND_NOTICE` 自己判 Skipped，不会静默发错人。
        let recipient = self.operator.or(self.creator).or(self.posted_by);
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            format!("凭证{label}"),
            format!("凭证「{}」状态已变更为「{label}」", self.voucher_no),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/voucher?id={}", self.voucher_id));
        draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
        draft.biz_id = Some(self.voucher_id.to_string());
        draft.sender_name = Some("凭证".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        draft.idempotency_key = Some(format!("LNK:VOUCHER:{}:{}", self.status, self.voucher_id));
        linkage::LinkageContext::new(
            state_machine::ENTITY_VOUCHER,
            &self.status,
            self.voucher_id,
            &self.voucher_no,
            self.operator,
        )
        .with_people(self.posted_by.or(self.operator), self.creator, None)
        .with_notice(draft)
    }
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除凭证侧的重复落账动作）。
///
/// 返回的 [`VoucherStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
///
/// `status` 是**变更之后**的状态（`DRAFT` / `AUDITED` / `POSTED`）。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    row: &VoucherRow,
    status: &str,
    operator: Option<Uuid>,
) -> std::result::Result<VoucherStatusChange, ApiError> {
    let creator = voucher::creator_of(ex, row.id)
        .await
        .map_err(|error| utils::db_error(error, "读取凭证制单人"))?;
    let mut change = VoucherStatusChange {
        voucher_id: row.id,
        voucher_no: row.voucher_no.clone(),
        status: status.to_string(),
        operator,
        creator,
        posted_by: row.posted_by,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run =
        linkage::engine::run_excluding(ex, &ctx, linkage::spec::VOUCHER_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            voucher_id = %change.voucher_id,
            voucher_no = change.voucher_no,
            status,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "凭证状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 凭证已经记账落定，
/// 因为一封站内信写失败就把过账回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify` / `erp::expense_linkage` 同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: VoucherStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            voucher_id = %change.voucher_id,
            voucher_no = change.voucher_no,
            status = change.status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "凭证状态变更：提交后的联动动作执行结果"
        );
    }
}

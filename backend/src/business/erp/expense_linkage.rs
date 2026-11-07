// ============================================================================
// expense_linkage.rs —— 费用报销（EXPENSE_REPORT）状态变更的**跨单据联动**接线（LNK-05）
//
// 报销单的四个状态推进点分布在两个文件里，但联动上下文 / 排除清单只在这里写一份：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 提交 | `expense_handler.rs::report_submit` 事务内（`engine::start` 之前） | `DRAFT → SUBMITTED` |
// | 通过 | `expense_bridge.rs::execute_approval` 事务内（`state_machine::transition` 之后） | `SUBMITTED → APPROVED` |
// | 付清 | `expense_handler.rs::report_pay` 事务内（仅 `new_status == PAID`） | `APPROVED → PAID` |
// | 作废 | `expense_handler.rs::report_cancel` 事务内（CAS ④ 之后） | `SUBMITTED / APPROVED / DRAFT → CANCELLED` |
// | 打回 | `expense_bridge.rs::revert_to_draft` 事务内（驳回 / 撤回） | `SUBMITTED → DRAFT` |
//
// 打回这一条不是卡面的四条之一，但它必须接：`DRAFT` 是**能走到的状态**（驳回 / 撤回都会回到
// 它），只接四条的话 `EXPENSE_REPORT/DRAFT` 上配的规则又能保存、又能启用、**永远不触发**
// —— 正是 P1-FIX-5C 记录的那个病。所以两条 bridge 路径（通过 / 打回）一起接。
//
// # 为什么报销要「排除清单」而不是直接 `engine::run`
//
// 报销的状态推进事务**自己已经写完了**凭证，通用引擎再跑一次就是重复落账：
//
// | 动作码 | 报销侧的专用落账点 | 通用引擎再跑一次的后果 |
// |--------|--------------------|------------------------|
// | `GENERATE_VOUCHER` | `expense_bridge::post_report_voucher`（审批通过，借 费用+进项税 / 贷 2241.01）/ `post_payment_voucher`（付款，借 2241.01 / 贷 1002） | 一张报销单两张计提凭证、一笔付款两张付款凭证，总账翻倍 |
// | `REVERSE_VOUCHER` | `expense_bridge::reverse_report_vouchers`（作废已入账的单） | 同一张蓝字凭证被红冲两次（第二次撞「已红冲」直接 500，或者红字流水翻倍） |
//
// 排除清单在 `linkage::spec::EXPENSE_EXCLUDED_ACTIONS`（调用方传入、不写死在引擎里），
// 漂移守卫：`linkage::tests_wiring::expense_wires_generic_engine_and_excludes_voucher`
// + 真库反证 `erp::expense_linkage_tests::lnk05_approve_keeps_single_voucher_when_generate_voucher_rule_configured`。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔状态变更回滚
//   （不会留下「状态推了、账动了、联动没跑」的半截单）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**（账已落定，一封站内信失败不该把报销审批退回去）。
//
// # 幂等
//
// - 提交：`report_submit` 对非 DRAFT 直接 409；重复点击不会第二次进事务。
// - 通过：`execute_approval` 对 `status != SUBMITTED` 幂等返回（重复回调不重复记账）⇒ 联动也不跑。
// - 付清：付款入口对 `pay_status = PAID` 直接 409；部分付款**不改状态**（`APPROVED → APPROVED`），
//   所以部分付款不触发，只有真正付清那一次触发。
// - 作废：`CANCELLED` 再作废直接 409。
// - 通知另有幂等键 `LNK:EXPENSE:<状态>:<单据 id>` 兜底。
// ============================================================================

use crate::business::erp::expense_dto;
use crate::business::erp::expense_model::ReportRow;
use crate::business::flow::linkage;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 报销单通知的 `biz_type`（与审批用例的 `EXPENSE` 区分开：一个是被联动发的状态通知，
/// 一个是流程引擎发的待办）。
const NOTICE_BIZ_TYPE: &str = "EXPENSE_REPORT";

/// 一次报销单状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份 `report_no` / 状态 / 收件人），调用方只要把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给 [`run_deferred_after_commit`]
/// 即可 —— 不需要第二次读库（那时单子可能已被别人改过）。
pub(crate) struct ExpenseStatusChange {
    /// 报销单 id（幂等键 / 日志）。
    pub(crate) report_id: Uuid,
    /// 报销单号（通知文案 / 日志）。
    pub(crate) report_no: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 操作人。
    pub(crate) operator: Option<Uuid>,
    /// 报销人（`recipient = OWNER` / 默认收件人）。
    pub(crate) reporter: Option<Uuid>,
    /// 制单人（`recipient = CREATOR`）。
    pub(crate) creator: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl ExpenseStatusChange {
    /// 由单据 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自单据本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与应付 `payable_linkage` / 盘点 `stocktake_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        let label = expense_dto::status_label(&self.status);
        // 默认收件人 = 报销人（「我报的账动了一步」）：报销人才是真正关心这一步的人
        // （财务代填时报销人 ≠ 制单人 ≠ 操作人）。没有报销人（历史脏单）退制单人，
        // 再退操作人；都没有 → 空收件人，`SEND_NOTICE` 自己判 Skipped，不会静默发错人。
        let recipient = self.reporter.or(self.creator).or(self.operator);
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            format!("报销单{label}"),
            format!("报销单「{}」状态已变更为「{label}」", self.report_no),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/expense?id={}", self.report_id));
        draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
        draft.biz_id = Some(self.report_id.to_string());
        draft.sender_name = Some("费用报销".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        draft.idempotency_key = Some(format!("LNK:EXPENSE:{}:{}", self.status, self.report_id));
        linkage::LinkageContext::new(
            state_machine::ENTITY_EXPENSE,
            &self.status,
            self.report_id,
            &self.report_no,
            self.operator,
        )
        .with_people(self.reporter.or(self.creator), self.creator, None)
        .with_notice(draft)
    }
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除报销专用落账动作）。
///
/// 返回的 [`ExpenseStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    report: &ReportRow,
    status: &str,
    operator: Option<Uuid>,
) -> std::result::Result<ExpenseStatusChange, ApiError> {
    let mut change = ExpenseStatusChange {
        report_id: report.id,
        report_no: report.report_no.clone(),
        status: status.to_string(),
        operator,
        reporter: Some(report.reporter_id),
        creator: report.create_by,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run =
        linkage::engine::run_excluding(ex, &ctx, linkage::spec::EXPENSE_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            report_id = %change.report_id,
            report_no = change.report_no,
            status,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "报销单状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 账已经在库里落定，
/// 因为一封站内信写失败就把报销审批回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: ExpenseStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            report_id = %change.report_id,
            report_no = change.report_no,
            status = change.status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "报销单状态变更：提交后的联动动作执行结果"
        );
    }
}

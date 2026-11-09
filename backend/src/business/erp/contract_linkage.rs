// ============================================================================
// contract_linkage.rs —— 合同（CONTRACT）状态变更的**跨单据联动**接线（LNK-07）
//
// 合同的状态推进点分布在两个文件里，但联动上下文 / 排除清单 / 幂等口径只在这里写一份：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 提交审批 | `contract_handler.rs::contract_change_status`（目标态 PENDING_APPROVAL 分支，`tx.commit()` 之前） | `DRAFT → PENDING_APPROVAL` |
// | 审批通过 | `contract_bridge.rs::settle`（`ContractVerdict::Approve`，状态 CAS 成功之后、`tx.commit()` 之前） | `PENDING_APPROVAL → SIGNED` |
// | 审批驳回 / 撤回 | 同上（`ContractVerdict::BackToDraft`）+ `contract_handler.rs::contract_change_status` 通用分支 | `PENDING_APPROVAL → DRAFT` |
// | 生效 / 完成 / 终止 | `contract_handler.rs::contract_change_status` 通用分支（状态机过闸 + CAS 成功之后） | `SIGNED → ACTIVE` / `ACTIVE → COMPLETED` / `SIGNED|ACTIVE → TERMINATED` |
//
// # 「审批通过有两条路径」怎么防重复触发（卡面的重点）
//
// 合同从 `PENDING_APPROVAL` 走到 `SIGNED` 确实有两条路：
//
// ① **flow 审批回调**：`flow::handler::{approve_task,…}` → `contract_bridge::on_contract_settled`
//    → `settle()`（本文件在 `settle` 的事务里触发）；
// ② **handler 直接改状态**：`PATCH /status` 目标态 `SIGNED`（状态机允许
//    `PENDING_APPROVAL → SIGNED` 这条边，用于「审批中心之外的单据侧推进」）。
//
// 两条路都只写**同一列** `biz_contract.status`，而且都带 CAS（`WHERE status = <旧状态>`）：
// 先到的那条把状态推成 `SIGNED`，后到的那条：
//
// * 路径 ② 再进来：`contract_lock` 读到 `SIGNED`，`can_transition("SIGNED","SIGNED")` = false
//   → 409，**根本进不到本文件的调用点**；
// * 路径 ① 再进来（重复回调 / 双击 approve 重放）：`settle_guard::gate` 看到「单据已是目标状态」
//   → `AlreadySettled` → `settle()` 返回 `Ok(None)`，也进不到本文件的调用点。
//
// 也就是说**幂等的判据是状态列本身（CAS）**，不是「我记得触发过没有」的旁路标记：
// 状态没推进 = 联动不跑；状态推进了且只推进一次 = 联动只跑一次。
// 另外通知还有一道幂等键 `LNK:CONTRACT:<状态>:<合同 id>`（`uk_sys_notice_idem` 唯一索引），
// 即便将来有人加出第三条推进路径，同一状态的通知也不会发两条。
// 真库反证：`erp::contract_linkage_tests::lnk07_approval_triggers_once_and_repeat_is_suppressed`
// 连续调两次审批回调，断言通知只有一条、状态只前进一次。
//
// # 为什么合同也要「排除清单」
//
// 合同**不产生任何账**：它没有库存、没有余额、没有凭证（付款是采购单 / 应付单的事）。
// 所以通用引擎在合同触发点上跑 `GENERATE_VOUCHER` 只会有两种结局：
//
// * 取数失败 → 一条 ERROR 日志 + 跳过（管理员配了「看起来生效」的规则，账上一分没动）；
// * 或者更糟：按来源单据兜出一个「合同凭证」——合同号没有借贷科目口径，
//   这种凭证进总账就是脏账（凭证明细对不上业务实质，巡检 L5 会红）。
//
// 所以 `GENERATE_VOUCHER` 进 `linkage::spec::CONTRACT_EXCLUDED_ACTIONS`（运行时排除，
// 由 `run_excluding` 交给「凭证只能由真来源单据生成」的专用口径）。
// 库存 / 余额类动作（`WRITE_INVENTORY` / `UPDATE_PAYABLE_BALANCE`）不列进来：合同上没有
// 这两类单据字段，它们在本触发点上本来就没有可写的东西，列进去只是噪音。
// 漂移守卫：`linkage::tests_wiring::contract_wires_generic_engine_and_excludes_voucher`。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔状态变更回滚
//   （不会留下「状态推了、联动没跑」的半截单）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**（合同状态已落定，一封站内信失败不该
//   把审批结论退回去 —— 与 `flow::notify` 同一口径）。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::contract_model;
use crate::business::flow::linkage;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;

/// 合同状态通知的 `biz_type`（与审批用例的 `CONTRACT_APPROVE` 区分开：一个是被联动发的
/// 状态通知，一个是流程引擎发的待办）。
const NOTICE_BIZ_TYPE: &str = "CONTRACT";

/// 一次合同状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份 `contract_no` / 状态 / 收件人），调用方只要把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给 [`run_deferred_after_commit`] 即可
/// —— 不需要第二次读库（那时合同可能已被别人改过）。
pub(crate) struct ContractStatusChange {
    /// 合同 id（幂等键 / 日志）。
    pub(crate) contract_id: Uuid,
    /// 合同号（通知文案 / 日志）。
    pub(crate) contract_no: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 操作人。
    pub(crate) operator: Option<Uuid>,
    /// 制单人（`biz_contract.create_by`，`recipient = CREATOR` / 默认收件人）。
    pub(crate) creator: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl ContractStatusChange {
    /// 由合同 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自合同本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与应付 `payable_linkage` / 报销 `expense_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        let label = contract_model::status_label(&self.status);
        // 默认收件人 = 制单人（「我提的合同动了一步」）：合同没有「负责人」字段，
        // 制单人是唯一稳定的人。没有制单人（历史行 / 外部导入）退操作人；
        // 都没有 → 空收件人，`SEND_NOTICE` 自己判 Skipped，不会静默发错人。
        let recipient = self.creator.or(self.operator);
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            format!("合同{label}"),
            format!("合同「{}」状态已变更为「{label}」", self.contract_no),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/contract?id={}", self.contract_id));
        draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
        draft.biz_id = Some(self.contract_id.to_string());
        draft.sender_name = Some("合同管理".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        // 幂等键：同一合同 + 同一状态只发一条（CAS 之外的第二道闸，见文件头）
        draft.idempotency_key = Some(format!("LNK:CONTRACT:{}:{}", self.status, self.contract_id));
        linkage::LinkageContext::new(
            state_machine::ENTITY_CONTRACT,
            &self.status,
            self.contract_id,
            &self.contract_no,
            self.operator,
        )
        // 合同没有 assigner 概念；`owner` 也取制单人（配置里 `recipient = OWNER` 时不至于空）
        .with_people(self.creator, self.creator, None)
        .with_notice(draft)
    }
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除「合同上不可能成立」的凭证动作）。
///
/// 返回的 [`ContractStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    contract_id: Uuid,
    contract_no: &str,
    status: &str,
    operator: Option<Uuid>,
) -> std::result::Result<ContractStatusChange, ApiError> {
    let mut change = ContractStatusChange {
        contract_id,
        contract_no: contract_no.to_string(),
        status: status.to_string(),
        operator,
        creator: creator_of(ex, contract_id).await,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run = linkage::engine::run_excluding(ex, &ctx, linkage::spec::CONTRACT_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            contract_id = %change.contract_id,
            contract_no = change.contract_no,
            status,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "合同状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 合同状态已经在库里落定，
/// 因为一封站内信写失败就把审批结论回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: ContractStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            contract_id = %change.contract_id,
            contract_no = change.contract_no,
            status = change.status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "合同状态变更：提交后的联动动作执行结果"
        );
    }
}

/// 合同的制单人（`biz_contract.create_by`）。读不到 → `None`（不放宽也不报错：
/// 收件人为空时 `SEND_NOTICE` 自己判 Skipped，不会把状态变更推不上去）。
///
/// 为什么单独查一次而不是让调用方传：两个调用点的入参类型不同
/// （`ContractStateRow` 只有 id/no/status/dates，没有 `create_by`），
/// 在这里查一次（主键 1 行）比给两个模型各加一个字段干净。
///
/// SCOPE-EXEMPT: 按主键取 1 行的 `create_by`，不做数据范围裁剪（联动发通知与
/// 「谁能看到这张合同」无关；裁剪会让代填场景下的制单人收不到自己的通知）。
async fn creator_of(ex: &impl SqlExecutor, contract_id: Uuid) -> Option<Uuid> {
    ex.scalar_sql::<Option<Uuid>>(
        "SELECT create_by FROM biz_contract WHERE id = ?::uuid".to_string(),
        vec![Value::from(&contract_id)],
    )
    .await
    .ok()
    .flatten()
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn change(status: &str) -> ContractStatusChange {
        ContractStatusChange {
            contract_id: Uuid::parse_str("018f0000-0000-7000-8000-000000000001").expect("uuid"),
            contract_no: "HT-LNK07-0001".to_string(),
            status: status.to_string(),
            operator: None,
            creator: None,
            run: linkage::LinkageRun::default(),
        }
    }

    /// ① 通知幂等键必须是「状态 + 合同」维度：同一合同的不同状态各一条，
    /// 同一状态重复推进只落一条（CAS 之外的第三道保险）。
    #[test]
    fn notice_idempotency_key_is_per_status_and_contract() {
        let signed_change = change(contract_model::STATUS_SIGNED);
        let active_change = change(contract_model::STATUS_ACTIVE);
        let signed_again_change = change(contract_model::STATUS_SIGNED);
        let signed = signed_change.ctx();
        let active = active_change.ctx();
        let signed_again = signed_again_change.ctx();
        let key = |ctx: &linkage::LinkageContext<'_>| {
            ctx.notice
                .as_ref()
                .and_then(|draft| draft.idempotency_key.clone())
                .expect("联动上下文必须带通知幂等键")
        };
        assert_ne!(key(&signed), key(&active), "不同状态必须是不同幂等键");
        assert_eq!(
            key(&signed),
            key(&signed_again),
            "同一合同同一状态重复推进必须撞同一个幂等键（只发一条）"
        );
        assert!(
            key(&signed).starts_with("LNK:CONTRACT:"),
            "幂等键前缀是清理夹具与排障的判据：{}",
            key(&signed)
        );
    }

    /// ② 通知文案 / 链接 / 业务类型取自合同本身（不在第二处维护文案）。
    #[test]
    fn notice_draft_carries_contract_context() {
        let terminated = change(contract_model::STATUS_TERMINATED);
        let ctx = terminated.ctx();
        let draft = ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(draft.biz_type.as_deref(), Some(NOTICE_BIZ_TYPE));
        assert_eq!(draft.biz_id.as_deref(), Some("018f0000-0000-7000-8000-000000000001"));
        assert_eq!(
            draft.link_url.as_deref(),
            Some("/business/erp/contract?id=018f0000-0000-7000-8000-000000000001")
        );
        assert!(
            draft.title.contains("已终止"),
            "标题必须用人读的中文状态名：{}",
            draft.title
        );
        assert!(
            draft.content.contains("HT-LNK07-0001"),
            "正文必须带合同号：{}",
            draft.content
        );
        assert_eq!(ctx.entity, state_machine::ENTITY_CONTRACT);
        assert_eq!(ctx.status, contract_model::STATUS_TERMINATED);
    }
}

// ============================================================================
// depreciation_linkage.rs —— 折旧批次（DEPRECIATION）状态变更的**跨单据联动**接线（LNK-10）
//
// 折旧批次的三个状态推进点都在 `erp/depreciation_model.rs`，但能配联动规则、且真的
// 值得触发通知的只有两个（`CALCULATED → POSTED` 过账 / `POSTED → REVERSED` 红冲）；
// 联动上下文 / 排除清单 / 幂等口径只在本文件写一份：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 过账（手动计提 / 定时任务共用 `run_batch`） | `depreciation_model.rs::run_batch` ⑤步（明细 + 多借方凭证 + 回写卡片 + 批次置 POSTED 之后、`tx.commit()` 之前） | `CALCULATED \| REVERSED → POSTED` |
// | 红冲 | `depreciation_model.rs::reverse_batch` ⑤步（红字凭证 + 明细 REVERSED + 卡片回滚 + 批次 REVERSED 之后、`tx.commit()` 之前） | `POSTED → REVERSED` |
//
// # 为什么「SKIPPED」「CALCULATED」不接
//
// * `CALCULATED` 是一次计提的**中间态**：同一事务里紧接着就置 `POSTED`
//   （红冲后重跑是 `REVERSED → CALCULATED → POSTED`）。挂在 CALCULATED 上，
//   一次计提会发两条通知（CALCULATED 一条 + POSTED 一条），正是卡面要防的「刷屏」；
//   所以只在**终态** POSTED 上触发一次，重跑路径也只在最终 POSTED 时触发；
// * `SKIPPED` 是**合法空跑**（本期没有应提折旧：0 台资产、0 金额、不出凭证），
//   与「建卡 / 开账不是触发点」同一口径：没有账要通知谁？这一条写进
//   `linkage::spec::TRIGGER_SPECS` 的 `wiring` 文案，配置页直接看得到。
//
// # 大批量：为什么通知只发一条（卡面重点）
//
// 一次计提可能涉及几百台资产，但联动的**触发单据是批次**（`biz_dep_batch` 一行），
// 不是每台资产：`ctx.doc_id = batch_id`，引擎对这条批次只跑一遍规则 —— 300 台资产
// 的批次和 1 台资产的批次发的通知条数**一样**（都是 1 条）。通知正文里带台数
// （「涉及 N 台资产」），用户点进去看明细，而不是收 300 条。
// 加上幂等键 = 状态 + 批次 + 本次事件凭证，重复过账 / 重放都不会翻倍。
//
// # 为什么折旧也要「排除清单」（卡面重点：凭证同事务生成）
//
// 过账自己就落凭证：`run_batch` 第 ④ 步 `voucher_multi::post_multi_debit` 出一张
// 多借方凭证（借 费用科目×N + 贷 1602 汇总）；红冲由 `reverse_batch` 第 ② 步
// `voucher::reverse_posted` 原路红冲。通用引擎在同一触发点上再跑一次就是**重复落账**：
//
// * `GENERATE_VOUCHER` → 同一期间第二张折旧凭证：总账 / 累计折旧科目直接翻倍，
//   巡检「累计折旧 = 明细合计」当场红；
// * `REVERSE_VOUCHER` → 同一张蓝字凭证被红冲两次（第二次撞「已经红冲」→ 500，
//   或红字流水翻倍把累计折旧冲成负数）。
//
// 库存 / 余额类动作（`WRITE_INVENTORY` / `UPDATE_COST` / `UPDATE_PAYABLE_BALANCE`）
// 不列进来：折旧批次的明细行只有资产 / 金额 / 部门，没有物料 / 余额列，列进来只是噪音
// （与 `FIXED_ASSET` / `CONTRACT` 同口径）。
//
// # 事务边界（卡面铁律 ④：批次是大批量操作，通知必须走 commit 后）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔计提 / 红冲回滚
//   （不会留下「批次已过账、联动没跑」的半截状态）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**。这里格外重要：计提事务是长事务
//   （扫几百台资产 + 逐台回写卡片 + 出多借凭证），事务内发通知一旦回滚就是
//   「财务收到某月已计提，实际那一期没提」——之后在没提的月份上对账。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::linkage;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;

/// 折旧批次通知的 `biz_type`（与凭证来源类型 `DEPRECIATION` 同字，便于按批次追通知）。
const NOTICE_BIZ_TYPE: &str = "DEPRECIATION";

/// 一次折旧批次状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份批次号 / 期间 / 状态 / 台数 / 收件人），调用方只要把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给 [`run_deferred_after_commit`] 即可
/// —— 不需要第二次读库（那时批次可能已被别人红冲）。
pub(crate) struct DepBatchStatusChange {
    /// 批次 id（幂等键 / 日志 / 通知跳转）。
    pub(crate) batch_id: Uuid,
    /// 批次号 `DEP-yyyyMM-0001`（通知文案）。
    pub(crate) batch_no: String,
    /// 计提期间 `yyyy-MM`（通知文案）。
    pub(crate) period_code: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 本次批次涉及的资产台数（聚合口径：通知只发一条，台数写在正文里）。
    pub(crate) asset_count: i32,
    /// 操作人（计提 / 红冲的人；批次表只有 `run_by`，没有 `create_by`）。
    pub(crate) operator: Option<Uuid>,
    /// 本次事件的凭证 id（过账 = 蓝字计提凭证，红冲 = 红字凭证），进幂等键：
    /// 同一笔过账反复重放只发一条；红冲后重跑是**新的一笔**（新凭证），会再发一条。
    pub(crate) event_ref: Uuid,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl DepBatchStatusChange {
    /// 由批次 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自批次本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与合同 `contract_linkage` / 期间 `period_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        // 状态中文名只从 `spec::TRIGGER_SPECS` 取一份（配置页下拉用的就是它），
        // 取不到再退回状态编码（新状态忘了登记时也能发出可排障的文案）。
        let label = linkage::spec::status_label_of(state_machine::ENTITY_DEPRECIATION, &self.status)
            .unwrap_or(self.status.as_str());
        // `biz_dep_batch` 只有 `run_by`，没有 create_by：唯一稳定的人是**操作人**
        // （谁点的计提 / 红冲就通知谁）。`owner` 也取操作人，使配置里
        // `recipient = OWNER` 时不至于空收件人。
        let recipient = self.operator;
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            format!("折旧批次{label}"),
            format!(
                "折旧批次「{}」（期间 {}，涉及 {} 台资产）状态已变更为「{label}」",
                self.batch_no, self.period_code, self.asset_count
            ),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/depreciation?id={}", self.batch_id));
        draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
        draft.biz_id = Some(self.batch_id.to_string());
        draft.sender_name = Some("折旧计提".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        // 幂等键：状态 + 批次 + 本次事件凭证（见结构体 `event_ref` 的说明）
        draft.idempotency_key = Some(format!(
            "LNK:DEPRECIATION:{}:{}:{}",
            self.status, self.batch_id, self.event_ref
        ));
        linkage::LinkageContext::new(
            state_machine::ENTITY_DEPRECIATION,
            &self.status,
            self.batch_id,
            &self.batch_no,
            self.operator,
        )
        .with_people(self.operator, self.operator, None)
        .with_notice(draft)
    }
}

/// [`run_in_tx`] 的入参（`ex` 之外的 7 项）：一次批次状态变更的完整输入。
#[derive(Debug, Clone, Copy)]
pub struct DepLinkageInput<'a> {
    /// 折旧批次主键（联动上下文的业务单据 id）。
    pub batch_id: Uuid,
    /// 批次号（通知 / 任务文案里带它）。
    pub batch_no: &'a str,
    /// 计提期间编码（`YYYY-MM`）。
    pub period_code: &'a str,
    /// 变更后的目标状态（`POSTED` / `REVERSED` 等，规则匹配按它）。
    pub status: &'a str,
    /// 本次批次涉及的资产台数（报文里用）。
    pub asset_count: i32,
    /// 本次事件凭证 id（幂等键的一部分：同一笔过账重放只发一条通知）。
    pub event_ref: Uuid,
    /// 操作人（通知受众 / 上下文）。
    pub operator: Option<Uuid>,
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除「本次事务已经自己落账」的凭证动作）。
///
/// 返回的 [`DepBatchStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
///
/// 入参 `input` 是「一次批次状态变更的完整输入」（见 [`DepLinkageInput`]）：
/// 批次身份 + 期间 + 目标态 + 台数 + 事件凭证 + 操作人，一次给全，避免逐参数拼错。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    input: DepLinkageInput<'_>,
) -> std::result::Result<DepBatchStatusChange, ApiError> {
    let DepLinkageInput {
        batch_id,
        batch_no,
        period_code,
        status,
        asset_count,
        event_ref,
        operator,
    } = input;
    let mut change = DepBatchStatusChange {
        batch_id,
        batch_no: batch_no.to_string(),
        period_code: period_code.to_string(),
        status: status.to_string(),
        asset_count,
        operator,
        event_ref,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run = linkage::engine::run_excluding(ex, &ctx, linkage::spec::DEPRECIATION_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            batch_id = %change.batch_id,
            batch_no = change.batch_no,
            period_code = change.period_code,
            status,
            asset_count = change.asset_count,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "折旧批次状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 批次状态已经在库里落定，
/// 因为一封站内信写失败就把整批计提 / 红冲回滚掉，财务点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: DepBatchStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            batch_id = %change.batch_id,
            batch_no = change.batch_no,
            status = change.status,
            asset_count = change.asset_count,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "折旧批次状态变更：提交后的联动动作执行结果"
        );
    }
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn change(status: &str, event_ref: Uuid) -> DepBatchStatusChange {
        DepBatchStatusChange {
            batch_id: Uuid::parse_str("018f0000-0000-7000-8000-000000000004").expect("uuid"),
            batch_no: "DEP-203304-0001".to_string(),
            period_code: "2033-04".to_string(),
            status: status.to_string(),
            asset_count: 300,
            operator: None,
            event_ref,
            run: linkage::LinkageRun::default(),
        }
    }

    fn event(seed: u128) -> Uuid {
        Uuid::from_u128(seed)
    }

    /// ① 幂等键 = 状态 + 批次 + 本次事件凭证：重复重放同一笔过账只发一条；
    /// 红冲后重跑是**新的一笔**（新凭证 id），必须能再发一条。
    #[test]
    fn notice_idempotency_key_is_per_status_batch_and_event() {
        let posted = change("POSTED", event(1));
        let posted_again = change("POSTED", event(1));
        let reversed = change("REVERSED", event(2));
        let reposted = change("POSTED", event(3));
        let key = |item: &DepBatchStatusChange| {
            item.ctx()
                .notice
                .as_ref()
                .and_then(|draft| draft.idempotency_key.clone())
                .expect("联动上下文必须带通知幂等键")
        };
        assert_eq!(
            key(&posted),
            key(&posted_again),
            "同一笔过账重放必须撞同一个幂等键（只发一条）"
        );
        assert_ne!(key(&posted), key(&reversed), "过账与红冲必须是不同幂等键");
        assert_ne!(
            key(&posted),
            key(&reposted),
            "红冲后重跑是新的一笔（新凭证）：必须能再发一条，否则财务看不到「又提了一次」"
        );
        assert!(
            key(&posted).starts_with("LNK:DEPRECIATION:POSTED:"),
            "幂等键前缀是清理夹具与排障的判据：{}",
            key(&posted)
        );
        assert!(
            key(&posted).len() <= 128,
            "sys_notice.idempotency_key 是 VARCHAR(128)，当前 {} 字符",
            key(&posted).len()
        );
    }

    /// ② 聚合口径：通知只带**批次**，台数只出现在正文里 —— 300 台资产不是 300 条通知。
    #[test]
    fn notice_is_per_batch_not_per_asset() {
        let posted = change("POSTED", event(1));
        let ctx = posted.ctx();
        let draft = ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(ctx.doc_id, change("POSTED", event(1)).batch_id);
        assert_eq!(ctx.doc_no, "DEP-203304-0001");
        assert_eq!(ctx.entity, state_machine::ENTITY_DEPRECIATION);
        assert!(
            draft.content.contains("300 台资产"),
            "正文必须写清本批次涉及多少台资产（聚合口径的用户可见证据）：{}",
            draft.content
        );
        assert_eq!(
            draft.link_url.as_deref(),
            Some("/business/erp/depreciation?id=018f0000-0000-7000-8000-000000000004")
        );
        assert_eq!(draft.biz_type.as_deref(), Some(NOTICE_BIZ_TYPE));
        assert!(
            draft.title.contains("已过账"),
            "标题必须用人读的中文状态名：{}",
            draft.title
        );
    }

    /// ③ 状态中文名与 `spec::TRIGGER_SPECS` 逐字一致（配置页下拉 / 通知文案不许分叉）。
    #[test]
    fn status_labels_come_from_spec() {
        for (status, label) in [
            ("CALCULATED", "已计算"),
            ("POSTED", "已过账"),
            ("REVERSED", "已红冲"),
            ("SKIPPED", "已跳过"),
        ] {
            assert_eq!(
                linkage::spec::status_label_of(state_machine::ENTITY_DEPRECIATION, status),
                Some(label),
                "`{status}` 的中文名在 spec 与 depreciation_linkage 之间分叉了"
            );
        }
    }
}

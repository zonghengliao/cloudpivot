// ============================================================================
// period_linkage.rs —— 会计期间（ACCOUNTING_PERIOD）状态变更的**跨单据联动**接线（LNK-08）
//
// 期间的三个状态推进点都在 `erp/period.rs`，但真正有意义、且能配联动规则的只有两个
// （`OPEN → CLOSED` 结账 / `CLOSED → OPEN` 反结账）；联动上下文 / 排除清单 / 幂等口径
// 只在本文件写一份：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 结账 | `period.rs::close_period`（四项校验 + 汇兑重估 + 状态 CAS 成功之后、`tx.commit()` 之前） | `OPEN → CLOSED` |
// | 反结账 | `period.rs::reopen_period`（后续期间校验 + 状态 CAS 成功之后、`tx.commit()` 之前） | `CLOSED → OPEN` |
//
// # 为什么开账（`open_period` / `batch_open`）不接
//
// 开账不是「一张单据走了一步状态」，而是**建行 / 补行**：
//
// * `ensure_year_periods` 在**每个环境启动时**把当年 12 期补齐（`status = OPEN`）；
// * `batch_open` 一次最多建 120 期。
//
// 把引擎挂在开账上，等于「启动时 / 一次批量操作里扇出 N 条通知」，而且新行是
// **创建**不是**状态迁移**（与别的单据「新建不算触发点」同一口径）。
// 所以 `PERIOD/OPEN` 上配的规则目前可以保存、但不会触发 —— 这一点写进
// `linkage::spec::TRIGGER_SPECS` 的 `wiring` 文案里，配置页直接看得到
// （宁可承认，也不要把「批量开账」悄悄变成本仓最讨厌的「配了永远不响」）。
// 结账 / 反结账是**单期间、人工触发、事务短**的两条边，才是通知类联动的正经用处：
// 「月结完成 → 通知财务全组」「反结账 → 上报财务负责人」。
//
// # 为什么期间也要「排除清单」（卡面重点）
//
// 结账事务自己就**生成凭证**：第 ⑤ 步 `fx_revaluation::revalue_period` 把未核销外币应收
// 按月末汇率重估，差额记 5603 / 1122，凭证落在本期间（幂等：同期间第二次 `skipped`）。
// 通用引擎在同一个触发点上再跑一次 `GENERATE_VOUCHER` 只会是重复落账 / 脏账
// （该动作的取数口径只接管收货入库，挂在期间上一般是 `Skipped`；一旦将来接管了别的
// 来源单据，就是「一张结账两张重估凭证」）。所以 `GENERATE_VOUCHER` 进
// `linkage::spec::PERIOD_EXCLUDED_ACTIONS`（运行时排除，交给「凭证只能由真来源单据 /
// 结账自己的重估路径生成」的专用口径）。
//
// 库存 / 余额类动作（`WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` / `UPDATE_COST` /
// `UPDATE_PAYABLE_BALANCE`）同样排除：期间**没有明细行、没有余额列**，这些动作在月结
// 事务里要么取数失败（一条 ERROR 噪音），要么（将来被接管后）把库存 / 应付台账当成
// 「结账的副作用」写一遍 —— 封账是只读开关，不是业务单据，不该产生业务落账。
//
// 漂移守卫：`linkage::tests_wiring::period_wires_generic_engine_and_excludes_ledger_actions`。
//
// # 事务边界（卡面：月结事务内 + commit 后）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔结账回滚
//   （不会留下「期间已封、联动没跑」的半截状态）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**。
//
// 为什么这条边界在结账上格外重要（卡面原话）：结账是个**长事务**（四项校验 + 全期间
// 凭证聚合 + 汇兑重估），事务内发通知万一回滚就是「通知已发、结账失败」——
// 财务收到「某月已结账」，实际那个月还开着，随后在开着的月份里补账。通知必须在
// commit 之后才发。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::linkage;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;

/// 期间状态通知的 `biz_type`（与催办 `period_remind` 的 `biz_type` 区分开：
/// 一个是被联动发的状态通知，一个是结账预检的催办）。
const NOTICE_BIZ_TYPE: &str = "ACCOUNTING_PERIOD";

/// 一次会计期间状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份 `period_code` / 状态 / 收件人），调用方只要把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给 [`run_deferred_after_commit`] 即可
/// —— 不需要第二次读库（那时期间可能已被别人反结账）。
pub(crate) struct PeriodStatusChange {
    /// 期间 id（幂等键 / 日志）。
    pub(crate) period_id: Uuid,
    /// 期间编码 `YYYY-MM`（通知文案 / 日志）。
    pub(crate) period_code: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 操作人（结账 / 反结账的人）。
    pub(crate) operator: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl PeriodStatusChange {
    /// 由期间 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自期间本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与合同 `contract_linkage` / 应付 `payable_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        // 状态中文名只从 `spec::TRIGGER_SPECS` 取一份（配置页下拉用的就是它）
        let label =
            linkage::spec::status_label_of(state_machine::ENTITY_PERIOD, &self.status).unwrap_or(self.status.as_str());
        // `biz_accounting_period` 没有 create_by / owner 列（全局会计字典，见 period.rs
        // 头注释「数据权限」），唯一稳定的人是**操作人**：谁结的账就通知谁。
        // `owner` 也取操作人，使配置里 `recipient = OWNER` 时不至于空收件人。
        let recipient = self.operator;
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            format!("会计期间{label}"),
            format!("会计期间「{}」状态已变更为「{label}」", self.period_code),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/period?id={}", self.period_id));
        draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
        draft.biz_id = Some(self.period_id.to_string());
        draft.sender_name = Some("会计期间".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        // 幂等键：同一期间 + 同一状态只发一条（CAS 之外的第二道闸，见文件头）
        draft.idempotency_key = Some(format!("LNK:PERIOD:{}:{}", self.status, self.period_id));
        linkage::LinkageContext::new(
            state_machine::ENTITY_PERIOD,
            &self.status,
            self.period_id,
            &self.period_code,
            self.operator,
        )
        .with_people(self.operator, self.operator, None)
        .with_notice(draft)
    }
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除库存 / 余额 / 凭证三类）。
///
/// 返回的 [`PeriodStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    period_id: Uuid,
    period_code: &str,
    status: &str,
    operator: Option<Uuid>,
) -> std::result::Result<PeriodStatusChange, ApiError> {
    let mut change = PeriodStatusChange {
        period_id,
        period_code: period_code.to_string(),
        status: status.to_string(),
        operator,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run = linkage::engine::run_excluding(ex, &ctx, linkage::spec::PERIOD_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            period_id = %change.period_id,
            period_code = change.period_code,
            status,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "会计期间状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 期间状态已经在库里落定，
/// 因为一封站内信写失败就把月结回滚掉，财务点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: PeriodStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            period_id = %change.period_id,
            period_code = change.period_code,
            status = change.status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "会计期间状态变更：提交后的联动动作执行结果"
        );
    }
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn change(status: &str) -> PeriodStatusChange {
        PeriodStatusChange {
            period_id: Uuid::parse_str("018f0000-0000-7000-8000-000000000002").expect("uuid"),
            period_code: "2033-12".to_string(),
            status: status.to_string(),
            operator: None,
            run: linkage::LinkageRun::default(),
        }
    }

    /// ① 通知幂等键必须是「状态 + 期间」维度：同一期间的结账 / 反结账各一条，
    /// 同一状态重复推进只落一条（CAS 之外的第三道保险）。
    #[test]
    fn notice_idempotency_key_is_per_status_and_period() {
        let closed = change("CLOSED");
        let opened = change("OPEN");
        let closed_again = change("CLOSED");
        let key = |ctx: &linkage::LinkageContext<'_>| {
            ctx.notice
                .as_ref()
                .and_then(|draft| draft.idempotency_key.clone())
                .expect("联动上下文必须带通知幂等键")
        };
        assert_ne!(key(&closed.ctx()), key(&opened.ctx()), "结账与反结账必须是不同幂等键");
        assert_eq!(
            key(&closed.ctx()),
            key(&closed_again.ctx()),
            "同一期间同一状态重复推进必须撞同一个幂等键（只发一条）"
        );
        assert!(
            key(&closed.ctx()).starts_with("LNK:PERIOD:"),
            "幂等键前缀是清理夹具与排障的判据：{}",
            key(&closed.ctx())
        );
    }

    /// ② 通知文案 / 链接 / 业务类型取自期间本身（状态中文名来自 `spec::TRIGGER_SPECS`）。
    #[test]
    fn notice_draft_carries_period_context() {
        let closed = change("CLOSED");
        let ctx = closed.ctx();
        let draft = ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(draft.biz_type.as_deref(), Some(NOTICE_BIZ_TYPE));
        assert_eq!(draft.biz_id.as_deref(), Some("018f0000-0000-7000-8000-000000000002"));
        assert_eq!(
            draft.link_url.as_deref(),
            Some("/business/erp/period?id=018f0000-0000-7000-8000-000000000002")
        );
        assert!(
            draft.title.contains("已结账"),
            "标题必须用人读的中文状态名：{}",
            draft.title
        );
        assert!(
            draft.content.contains("2033-12"),
            "正文必须带期间编码：{}",
            draft.content
        );
        assert_eq!(ctx.entity, state_machine::ENTITY_PERIOD);
        assert_eq!(ctx.status, "CLOSED");
    }
}

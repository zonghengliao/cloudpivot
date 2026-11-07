// ============================================================================
// year_end_linkage.rs —— 会计年度（ACCOUNTING_YEAR）状态变更的**跨单据联动**接线（LNK-11）
//
// 年结（`erp/year_end.rs::close_year`）与反年结（`reopen_year`）各自是一个**长事务**：
// 年结要扫全年的凭证聚合损益、出一张 `YEAR_END` 结转凭证、写下一年度期初台账、写状态日志；
// 反年结要删结转凭证 + 删下年期初 + 解封 12 月 + 留痕。两条边都是「一张单据走一步状态」，
// 是两个能配联动规则、也真的值得通知财务的触发点：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 年结 | `year_end.rs::close_year`（① 结转凭证 → ② 下年期初 → ③ 留痕之后、`tx.commit()` 之前） | `NOT_CLOSED → CLOSED` |
// | 反年结 | `year_end.rs::reopen_year`（① 删凭证 → ② 删期初 → ③ 解封 12 月 → ④ 留痕之后、`tx.commit()` 之前） | `CLOSED → NOT_CLOSED` |
//
// # 为什么年结的接线要格外小心（卡面原话：失败回滚代价最大）
//
// 年结是一个**不可轻率重放**的动作：前置条件是「12 个月全部结账 + 没有未完成单据 + 全年度
// 试算平衡 + 下一年度已开账」，跑一次要动全年账（结转凭证 + 24 个期间的状态 + 下年期初）。
// 一旦在事务里做了**不可回滚的副作用**（发通知 / 建待办 / 发外部消息），回滚就等于
// 「财务收到『2027 年已结转』，实际那个年度还开着」——之后在没结的年度上继续补账，
// 到了明年才发现期初是错的。这条卡面的红线在本文件里落成三条硬约束：
//
// 1. **事务内只放 `Phase::InTx`（关键）动作**：`engine::run_excluding` 只执行
//    `severity = Critical` 的那些，失败即 `Err` → 调用方整笔回滚（`?` 在 `commit` 之前）；
// 2. **通知一律走 commit 之后**：`SEND_NOTICE` / `CREATE_TASK` 是 `Phase::AfterCommit`，
//    事务内只排队进 `run.deferred`，`commit()` 成功之后才由
//    [`run_deferred_after_commit`] 交给 `engine::run_deferred` 真正发出；
// 3. **排除会造成重复落账的动作**（见下面的排除清单）。
//
// # 为什么年结也要「排除清单」（卡面：排除库存 / 余额 / 凭证）
//
// 年结事务自己就把账写完了一整轮：
//
// * 第 ① 步 `voucher::post_voucher` 出一张 `YEAR_END` 结转凭证（借收入 / 贷费用 +
//   3103→3104）；反年结第 ① 步把这张凭证整张删掉（并把分录 CASCADE 掉）；
// * 第 ② 步 `opening::write_year_end_opening_in` 写下一年度期初台账。
//
// 通用引擎在同一个触发点上再跑一遍就是**重复落账 / 口径打架**：
//
// - `GENERATE_VOUCHER` → 同一 12 月第二张结转凭证：损益科目被结平两次、3103/3104 直接翻倍，
//   而且库层的部分唯一索引 `uk_biz_voucher_year_end_period` 会当场报唯一冲突（500）；
//   该动作目前的取数口径只接管收货入库，挂在年度上必然是 `Skipped` ——
//   「配了看起来生效、账上一分没动」同样是本仓最讨厌的样子；
// - `REVERSE_VOUCHER` → 结转凭证被红冲一次后，反年结再整张删一次：红字流水成了孤儿，
//   12 月余额与「已结转」状态对不上；
// - `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` / `UPDATE_COST` / `UPDATE_PAYABLE_BALANCE` →
//   年度**没有明细行、没有物料 / 余额列**：这些动作在年结事务里要么取数失败（一条 ERROR 噪音），
//   要么（将来被接管后）把库存 / 应付台账当成「年结的副作用」写一遍 —— 年结是会计期间的封口，
//   不是业务单据，不该产生业务落账。
//
// `SEND_NOTICE` / `CREATE_TASK` **不能排除**：年结完成 / 反年结之后的「通知财务全组」与
// 「建次年审计待办」就靠通用引擎的 `Phase::AfterCommit` 动作（卡面的正经用处），
// 排掉等于接线了但永远不响。
//
// 排除清单放在 `linkage::spec::YEAR_END_EXCLUDED_ACTIONS`（调用方传入，不写死在引擎里）。
// 漂移守卫：`linkage::tests_wiring::year_end_wires_generic_engine_and_excludes_ledger_actions`。
//
// # 幂等口径：为什么幂等键带「本次事件凭证」
//
// 年结本身有 CAS 级的闸（`ALREADY_CLOSED` 409），重复年结根本到不了这里；但**反年结之后重结**
// 是合法操作（凭证重算，金额可能不同），财务应该收到第二封通知。所以幂等键取
// `LNK:ACCOUNTING_YEAR:<状态>:<年度>:<本次事件凭证 id>`：
//
// * 同一笔年结重放（规则被启用两次 / `run_deferred` 重试）→ 撞同一个键，只发一条；
// * 反年结后重结（新凭证）→ 新键，再发一条（与 `depreciation_linkage` 同一口径）。
//
// 空年度没有结转凭证（卡面 §七 ⑭），`event_ref` 退回**年度稳定主键**（见 [`year_doc_id`]），
// 也仍然是确定值 —— 不会因为「没凭证」就变成每次都发一条。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::year_end::STATUS_CLOSED;
use crate::business::flow::linkage;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;

/// 会计年度通知的 `biz_type`（与状态日志 `entity_type = 'ACCOUNTING_YEAR'` 同字，
/// 便于按年度追通知）。
const NOTICE_BIZ_TYPE: &str = "ACCOUNTING_YEAR";

/// 「年度单据」的稳定主键。
///
/// 年结的状态日志 `entity_id` 存的是**结转凭证 id**（空年度是一次性随机 id）—— 直接拿它当
/// 联动 `doc_id`，反年结后重结就会换一个 id，幂等键跟着变，通知翻倍；空年度更是每次都变。
/// 这里固定按年度生成一个确定值（UUID 形状：前 12 字节固定，后 12 位十进制年度），
/// 同一个年度永远是同一个「单据」。
///
/// 只用于联动上下文 / 通知跳转 / 幂等键，**不是外键**（`biz_status_log.entity_id` 仍是凭证 id）。
pub(crate) fn year_doc_id(year: i16) -> Uuid {
    Uuid::parse_str(&format!("018f0000-0000-7000-8000-{year:012}"))
        .expect("会计年度的稳定主键生成失败（年度必须是 0..=9999 的十进制）")
}

/// 一次年度状态变更的输入（两个调用方各拼一次容易拼错的字段都收在这里）。
pub(crate) struct YearEndEvent<'a> {
    /// 会计年度（如 2037）。
    pub(crate) year: i16,
    /// 变更**之后**的状态（`CLOSED` / `NOT_CLOSED`）。
    pub(crate) status: &'a str,
    /// 结转凭证号：年结 = 本次生成的凭证（空年度 `None`）；反年结 = 被删掉的凭证。
    pub(crate) voucher_no: Option<&'a str>,
    /// 结转凭证分录数（通知正文的「账做了多少」）。
    pub(crate) entry_count: i64,
    /// 本年净利润（结转进 3104 的金额；亏损为负）。
    pub(crate) net_profit: Decimal,
    /// 年结备注 / 反年结原因（通知正文带上，收件人不用再点进去查为什么）。
    pub(crate) reason: Option<&'a str>,
    /// 本次事件的凭证 id（进幂等键；空年度退回落款 [`year_doc_id`]）。
    pub(crate) event_ref: Uuid,
    /// 操作人（年结 / 反年结的人）。
    pub(crate) operator: Option<Uuid>,
}

/// 一次年度状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份年度 / 状态 / 凭证号 / 金额 / 收件人），调用方只要把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给 [`run_deferred_after_commit`] 即可
/// —— 不需要第二次读库（那时年度可能已被别人反年结）。
pub(crate) struct YearEndStatusChange {
    /// 会计年度。
    pub(crate) year: i16,
    /// 年度单据号（`"2037"`；`ctx.doc_no` 的持有者 —— 不能临时 `to_string()`，
    /// `LinkageContext` 借它活到整个上下文结束）。
    pub(crate) doc_no: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 结转凭证号（通知文案）。
    pub(crate) voucher_no: Option<String>,
    /// 结转凭证分录数。
    pub(crate) entry_count: i64,
    /// 本年净利润。
    pub(crate) net_profit: Decimal,
    /// 年结备注 / 反年结原因。
    pub(crate) reason: Option<String>,
    /// 本次事件凭证 id（幂等键）。
    pub(crate) event_ref: Uuid,
    /// 操作人。
    pub(crate) operator: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl YearEndStatusChange {
    /// 通知标题：年结 / 反年结用各自的动词（状态中文名只用于正文与配置页）。
    fn title(&self) -> String {
        if self.status == STATUS_CLOSED {
            "会计年度已结转".to_string()
        } else {
            "会计年度已反年结".to_string()
        }
    }

    /// 通知正文：年度 + 结转凭证 + 分录数 + 净利润（反年结时换成被删凭证 + 原因）。
    fn body(&self) -> String {
        let voucher = self.voucher_no.as_deref().unwrap_or("（空年度无结转凭证）");
        if self.status == STATUS_CLOSED {
            let profit = format!("{:.2}", self.net_profit);
            format!(
                "{} 年度已完成年结：结转凭证 {voucher}，共 {} 条分录，本年净利润 {profit}。\
                 下一年度期初已生成。",
                self.year, self.entry_count
            )
        } else {
            let reason = self.reason.as_deref().unwrap_or("-");
            format!(
                "{} 年度已反年结：结转凭证 {voucher}（{} 条分录）已删除，12 月期间已解封。\
                 原因：{reason}",
                self.year, self.entry_count
            )
        }
    }

    /// 由年度 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自本次事件本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与合同 `contract_linkage` / 期间 `period_linkage` / 折旧 `depreciation_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        // 状态中文名只从 `spec::TRIGGER_SPECS` 取一份（配置页下拉用的就是它），
        // 取不到再退回状态编码（新状态忘了登记时也能发出可排障的文案）。
        let label = linkage::spec::status_label_of(state_machine::ENTITY_YEAR_END, &self.status)
            .unwrap_or(self.status.as_str());
        // 年度的状态日志没有 create_by / owner 列：唯一稳定的人是**操作人**
        // （谁结的账就通知谁）。`owner` 也取操作人，使配置里 `recipient = OWNER`
        // 时不至于空收件人。
        let recipient = self.operator;
        let doc_id = year_doc_id(self.year);
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            self.title(),
            format!("{}\n\n当前状态：{label}", self.body()),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/year-end?year={}", self.year));
        draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
        draft.biz_id = Some(doc_id.to_string());
        draft.sender_name = Some("年度结转".to_string());
        // 状态变更是**知情类**通知：尊重 `notify.inbox` 偏好（与期间 `period_linkage` /
        // 折旧 `depreciation_linkage` 同口径）—— 年度已结转这件事在页面上、状态日志里都查得到，
        // 漏看一条站内信不至于造成损失，不该强行绕过收件人的订阅偏好。
        draft.force = false;
        // 幂等键：状态 + 年度 + 本次事件凭证（见文件头「幂等口径」）
        draft.idempotency_key = Some(format!(
            "LNK:ACCOUNTING_YEAR:{}:{}:{}",
            self.status, self.year, self.event_ref
        ));
        linkage::LinkageContext::new(
            state_machine::ENTITY_YEAR_END,
            &self.status,
            doc_id,
            &self.doc_no,
            self.operator,
        )
        .with_people(self.operator, self.operator, None)
        .with_notice(draft)
    }
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除库存 / 余额 / 凭证三类）。
///
/// 返回的 [`YearEndStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    event: YearEndEvent<'_>,
) -> std::result::Result<YearEndStatusChange, ApiError> {
    let mut change = YearEndStatusChange {
        year: event.year,
        doc_no: event.year.to_string(),
        status: event.status.to_string(),
        voucher_no: event.voucher_no.map(str::to_string),
        entry_count: event.entry_count,
        net_profit: event.net_profit,
        reason: event.reason.map(str::to_string),
        event_ref: event.event_ref,
        operator: event.operator,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run =
        linkage::engine::run_excluding(ex, &ctx, linkage::spec::YEAR_END_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            year = change.year,
            status = change.status,
            voucher = change.voucher_no.as_deref().unwrap_or("-"),
            entry_count = change.entry_count,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "会计年度状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 年度状态已经在库里落定，
/// 因为一封站内信写失败就把整年的结转回滚掉，财务点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: YearEndStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            year = change.year,
            status = change.status,
            voucher = change.voucher_no.as_deref().unwrap_or("-"),
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "会计年度状态变更：提交后的联动动作执行结果"
        );
    }
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::year_end::STATUS_NOT_CLOSED;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量非法")
    }

    fn change(status: &str, year: i16, event_ref: Uuid) -> YearEndStatusChange {
        YearEndStatusChange {
            year,
            doc_no: year.to_string(),
            status: status.to_string(),
            voucher_no: Some(format!("PZ-{year}12-000001")),
            entry_count: 6,
            net_profit: dec("12345.67"),
            reason: Some("年度审计调整".to_string()),
            event_ref,
            operator: None,
            run: linkage::LinkageRun::default(),
        }
    }

    fn key_of(item: &YearEndStatusChange) -> String {
        item.ctx()
            .notice
            .as_ref()
            .and_then(|draft| draft.idempotency_key.clone())
            .expect("联动上下文必须带通知幂等键")
    }

    /// ① 幂等键 = 状态 + 年度 + 本次事件凭证：
    /// 同一笔年结重放只发一条；反年结后重结（新凭证）必须能再发一条。
    #[test]
    fn notice_idempotency_key_is_per_status_year_and_event() {
        let first = Uuid::from_u128(0x018f_0000_0000_7000_8000_0000_0000_0001);
        let second = Uuid::from_u128(0x018f_0000_0000_7000_8000_0000_0000_0002);
        let closed = change(STATUS_CLOSED, 2037, first);
        let closed_again = change(STATUS_CLOSED, 2037, first);
        let reopened = change(STATUS_NOT_CLOSED, 2037, first);
        let reclosed = change(STATUS_CLOSED, 2037, second);
        let other_year = change(STATUS_CLOSED, 2038, first);
        assert_eq!(
            key_of(&closed),
            key_of(&closed_again),
            "同一笔年结重放必须撞同一个幂等键（只发一条）"
        );
        assert_ne!(
            key_of(&closed),
            key_of(&reopened),
            "年结与反年结必须是不同幂等键"
        );
        assert_ne!(
            key_of(&closed),
            key_of(&reclosed),
            "反年结后重结是新的一笔（新凭证）：必须能再发一条"
        );
        assert_ne!(
            key_of(&closed),
            key_of(&other_year),
            "年度不同必须是不同幂等键"
        );
        assert!(
            key_of(&closed).starts_with("LNK:ACCOUNTING_YEAR:CLOSED:2037:"),
            "幂等键前缀是清理夹具与排障的判据：{}",
            key_of(&closed)
        );
        assert!(
            key_of(&reopened).len() <= 128,
            "sys_notice.idempotency_key 是 VARCHAR(128)，当前 {} 字符",
            key_of(&reopened).len()
        );
    }

    /// ② 年结与反年结的通知文案各说各的事，且都带年度 / 凭证号（用户点进去就对得上账）。
    #[test]
    fn notice_draft_carries_year_end_context() {
        let event = year_doc_id(2037);
        let closed = change(STATUS_CLOSED, 2037, event);
        let ctx = closed.ctx();
        let draft = ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(ctx.entity, state_machine::ENTITY_YEAR_END);
        assert_eq!(ctx.status, STATUS_CLOSED);
        assert_eq!(ctx.doc_no, "2037");
        assert_eq!(
            ctx.doc_id,
            year_doc_id(2037),
            "doc_id 必须是年度的稳定主键（不是随机凭证 id）"
        );
        assert_eq!(
            draft.link_url.as_deref(),
            Some("/business/erp/year-end?year=2037")
        );
        assert_eq!(draft.biz_type.as_deref(), Some(NOTICE_BIZ_TYPE));
        let expected_biz_id = year_doc_id(2037).to_string();
        assert_eq!(draft.biz_id.as_deref(), Some(expected_biz_id.as_str()));
        assert_eq!(draft.title, "会计年度已结转");
        for needle in ["2037", "PZ-203712-000001", "6 条分录", "12345.67"] {
            assert!(
                draft.content.contains(needle),
                "年结通知正文必须带 {needle}（用户不点进去也能知道结了哪年、结了多少）：{}",
                draft.content
            );
        }
        assert!(
            !draft.force,
            "状态变更是知情类通知：必须尊重收件人的 notify.inbox 偏好（与期间 / 折旧同口径）"
        );

        let reopened = change(STATUS_NOT_CLOSED, 2037, event);
        let reopened_ctx = reopened.ctx();
        let reopened_draft = reopened_ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(reopened_draft.title, "会计年度已反年结");
        assert!(
            reopened_draft.content.contains("年度审计调整"),
            "反年结通知必须带原因（收件人不用再问为什么拆账）：{}",
            reopened_draft.content
        );
        assert!(
            reopened_draft.content.contains("12 月期间已解封"),
            "反年结的副作用要写清（12 月解封）：{}",
            reopened_draft.content
        );
    }

    /// ③ 状态中文名与 `spec::TRIGGER_SPECS` 逐字一致（配置页下拉 / 通知文案不许分叉）。
    #[test]
    fn status_labels_come_from_spec() {
        for (status, label) in [(STATUS_CLOSED, "已结转"), (STATUS_NOT_CLOSED, "未结转")] {
            assert_eq!(
                linkage::spec::status_label_of(state_machine::ENTITY_YEAR_END, status),
                Some(label),
                "`{status}` 的中文名在 spec 与 year_end_linkage 之间分叉了"
            );
        }
    }

    /// ④ 年度主键必须是「同一个年度同一个值」且能被两个调用方独立算出来（幂等键依赖它）。
    #[test]
    fn year_doc_id_is_stable_per_year() {
        assert_eq!(year_doc_id(2037), year_doc_id(2037));
        assert_ne!(year_doc_id(2037), year_doc_id(2038));
        assert_eq!(
            year_doc_id(2037).to_string(),
            "018f0000-0000-7000-8000-000000002037"
        );
    }
}

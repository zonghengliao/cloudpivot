// ============================================================================
// asset_linkage.rs —— 固定资产（FIXED_ASSET）状态变更的**跨单据联动**接线（LNK-09）
//
// 资产的联动上下文 / 排除清单 / 幂等口径只在本文件写一份，触发点只有两个**真实存在**
// 的状态推进（第三个「建卡」是建行，不接；「停用 / 启用」目前没有 HTTP 入口，见文末）：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 处置（审批通过） | `asset_bridge.rs::execute` 第 ⑤ 步（凭证 + 状态机 + 卡片回填 + 留痕之后、`tx.commit()` 之前） | `IN_USE\|IDLE → DISPOSED` |
// | 作废处置 | `asset_handler.rs::asset_dispose_cancel` 第 ⑤ 步（红冲 + 状态机 + 恢复在役 + 留痕之后、`tx.commit()` 之前） | `DISPOSED → IN_USE` |
//
// # 为什么「建卡」不接
//
// 建卡是 `NULL → IN_USE` 的**建行**动作（`asset_handler::asset_create` 写的是
// `log_event` 而不是状态流转），与「新建单据不算触发点」同一口径：挂上就是
// 「导入 N 张卡 = 扇出 N 条通知」。`FIXED_ASSET/IN_USE` 上的规则因此只由**作废处置**
// （`DISPOSED → IN_USE`）触发，这一点写进 `linkage::spec::TRIGGER_SPECS` 的 `wiring` 文案。
//
// # 为什么「停用 / 启用」这两条边没有接线（卡面「四处状态推进」的实况）
//
// `state_machine::FIXED_ASSET_EDGES` 里确实有 `IN_USE ↔ IDLE` 两条 mandatory 边，
// `doc_kind` 的变动类型词表里也有 `IDLE` / `ENABLE`，但**当前代码里没有任何 HTTP 入口
// 会走这两条边**（`grep -rn "STATUS_IDLE" backend/src` 只命中 `asset_dto.rs` 的常量与
// `LIVE_STATUSES`；`asset_handler.rs` 的 14 个接口里没有状态变更接口，
// `asset_model::apply_asset_status` 的调用方只有单测）。也就是说：
//
// * 不是「接线漏了」，而是**功能本身没有入口**（前端资产页同样只有转移 / 换人 / 处置 /
//   作废处置四个按钮）；
// * 在本卡里凭空虚造一个 `POST /asset/{id}/status` 属于新功能，不是接线 —— 会把
//   「联动接线卡」变成「资产模块二次开发卡」，而且新增接口还要动权限种子 / 前端，
//   与本卡 1.5 小时的边界冲突。
//
// 所以本文件只接**真实存在的两处**状态推进，并把这个缺口写进 `wiring` 文案与证据文档
// （宁可承认「IDLE 上的规则目前没有触发源」，也不要在 `wired = true` 里塞一条假调用）。
// 漂移守卫：`linkage::tests_wiring::asset_wires_generic_engine_and_excludes_voucher`。
//
// # 为什么资产也要「排除清单」（卡面重点：凭证不重复）
//
// 处置**自己就落凭证**：`asset_bridge::post_disposal_vouchers` 按处置类型出 1~3 张
// （出售：1606 清理 + 1002 收款 + 2221.02 销项；报废 / 盘亏：1606 + 1901 或 5711），
// 作废处置走 `asset_bridge::reverse_disposal_vouchers` 原路红冲。通用引擎在同一触发点上
// 再跑一次就是**重复落账**：
//
// * `GENERATE_VOUCHER` → 一张卡两套处置凭证（总账 / 固定资产清理科目直接翻倍，
//   巡检 K7「1606 / 1901 必须归零」当场红）；
// * `REVERSE_VOUCHER` → 同一张蓝字处置凭证被红冲两次（第二次撞「已经红冲」→ 500）。
//
// 库存 / 余额类动作（`WRITE_INVENTORY` / `UPDATE_COST` / `UPDATE_PAYABLE_BALANCE`）
// 不列进来：资产卡片上没有物料 / 余额列，列进来只是噪音（与合同 `CONTRACT` 同口径）。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔处置 / 作废回滚
//   （不会留下「卡已处置、联动没跑」的半截状态）；
// - 提交后：`run_deferred_after_commit` → `engine::run_deferred`，通知 / 建任务这类
//   `Phase::AfterCommit` 动作**永不回滚业务**（资产状态已落定，一封站内信失败不该把
//   审批结论 / 红冲退回去 —— 与 `flow::notify` 同一口径）。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::asset_dto;
use crate::business::flow::linkage;
use crate::business::notice;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::shared::state_machine;

/// 资产状态通知的 `biz_type`（与处置审批的 `ASSET_DISPOSAL` 区分开：一个是被联动发的
/// 状态通知，一个是流程引擎发的待办）。
const NOTICE_BIZ_TYPE: &str = "FIXED_ASSET";

/// 一次固定资产状态变更的联动上下文 + 事务内执行结果。
///
/// 事务内与提交后**同形**（同一份 `asset_no` / 状态 / 收件人），调用方只要把
/// `run_in_tx` 的返回值留着，`commit()` 之后原样交给 [`run_deferred_after_commit`] 即可
/// —— 不需要第二次读库（那时卡片可能已被别人处置 / 作废）。
pub(crate) struct AssetStatusChange {
    /// 资产卡片 id（幂等键 / 日志）。
    pub(crate) asset_id: Uuid,
    /// 资产编号 `FA-yyyyMMdd-000001`（通知文案 / 日志）。
    pub(crate) asset_no: String,
    /// 变更**之后**的状态（触发点状态）。
    pub(crate) status: String,
    /// 操作人（处置发起人 / 作废处置的人）。
    pub(crate) operator: Option<Uuid>,
    /// 制单人（`biz_fixed_asset.create_by`，`recipient = CREATOR` / 默认收件人）。
    pub(crate) creator: Option<Uuid>,
    /// 事务内动作的执行结果（`deferred` 待提交后跑）。
    pub(crate) run: linkage::LinkageRun,
}

impl AssetStatusChange {
    /// 由资产卡片 + 目标状态构造上下文（收件人 / 文案 / 幂等键都取自卡片本身）。
    ///
    /// 配置只覆盖「发给谁、覆盖哪些字段」，不在第二处维护文案
    /// （与合同 `contract_linkage` / 报销 `expense_linkage` 同一口径）。
    fn ctx(&self) -> linkage::LinkageContext<'_> {
        // 状态中文名只从 `spec::TRIGGER_SPECS` 取一份（配置页下拉用的就是它），
        // 取不到再退回 `asset_dto::status_label`（两处本来就逐字一致，有单测对拍）。
        let label = linkage::spec::status_label_of(state_machine::ENTITY_FIXED_ASSET, &self.status)
            .unwrap_or_else(|| asset_dto::status_label(&self.status));
        // 默认收件人 = 制单人（「我建的这张卡被处置了」）：资产卡片没有负责人字段，
        // 制单人是唯一稳定的人。没有制单人（历史行 / 外部导入）退操作人；
        // 都没有 → 空收件人，`SEND_NOTICE` 自己判 Skipped，不会静默发错人。
        let recipient = self.creator.or(self.operator);
        let mut draft = notice::NoticeDraft::text(
            NoticeType::Workflow,
            NoticeLevel::Info,
            format!("资产{label}"),
            format!("资产「{}」状态已变更为「{label}」", self.asset_no),
            notice::NoticeTarget::User(recipient.into_iter().collect()),
        );
        draft.link_url = Some(format!("/business/erp/asset?id={}", self.asset_id));
        draft.biz_type = Some(NOTICE_BIZ_TYPE.to_string());
        draft.biz_id = Some(self.asset_id.to_string());
        draft.sender_name = Some("固定资产".to_string());
        // 状态变更是知情类通知，尊重 notify.inbox（漏看不造成损失）
        draft.force = false;
        // 幂等键：同一卡片 + 同一状态只发一条（CAS 之外的第二道闸，见文件头）
        draft.idempotency_key = Some(format!("LNK:FIXED_ASSET:{}:{}", self.status, self.asset_id));
        linkage::LinkageContext::new(
            state_machine::ENTITY_FIXED_ASSET,
            &self.status,
            self.asset_id,
            &self.asset_no,
            self.operator,
        )
        // 卡片没有 assigner 概念；`owner` 也取制单人（配置里 `recipient = OWNER` 时不至于空）
        .with_people(self.creator, self.creator, None)
        .with_notice(draft)
    }
}

/// 事务内跑联动的 `Phase::InTx` 动作（排除「已由 asset_bridge 专用路径落账」的凭证动作）。
///
/// 返回的 [`AssetStatusChange::run`] 里的 `deferred` 必须在调用方 `commit()` **之后**
/// 交给 [`run_deferred_after_commit`]。关键动作失败 → `Err(ApiError)` → 调用方整笔回滚。
pub(crate) async fn run_in_tx(
    ex: &impl SqlExecutor,
    asset_id: Uuid,
    asset_no: &str,
    status: &str,
    operator: Option<Uuid>,
) -> std::result::Result<AssetStatusChange, ApiError> {
    let mut change = AssetStatusChange {
        asset_id,
        asset_no: asset_no.to_string(),
        status: status.to_string(),
        operator,
        creator: creator_of(ex, asset_id).await,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run =
        linkage::engine::run_excluding(ex, &ctx, linkage::spec::FIXED_ASSET_EXCLUDED_ACTIONS)
            .await?;
    if !change.run.is_empty() {
        tracing::info!(
            asset_id = %change.asset_id,
            asset_no = change.asset_no,
            status,
            in_tx_rules = change.run.runs.len(),
            deferred_rules = change.run.deferred.len(),
            "固定资产状态变更：跨单据联动已执行（事务内动作已落库，提交后动作待 commit）"
        );
    }
    Ok(change)
}

/// 提交**之后**执行联动的 `Phase::AfterCommit` 动作（发通知 / 建任务）。
///
/// 铁律：这里**永不返回 Err**、永不回滚业务 —— 卡片状态已经在库里落定，
/// 因为一封站内信写失败就把处置 / 作废回滚掉，用户点第二次还会失败第二次
/// （与 `flow::notify`「通知失败不回滚业务」同一口径）。
pub(crate) async fn run_deferred_after_commit(mut change: AssetStatusChange) {
    if change.run.deferred.is_empty() {
        return;
    }
    // 先把 deferred 取出来（`ctx` 要借 `change` 的字符串，不能同时把字段 move 走）
    let deferred = std::mem::take(&mut change.run.deferred);
    let ctx = change.ctx();
    for item in linkage::engine::run_deferred(&ctx, deferred).await {
        tracing::info!(
            asset_id = %change.asset_id,
            asset_no = change.asset_no,
            status = change.status,
            rule_id = %item.rule_id,
            action_code = %item.action_code,
            outcome = item.outcome.label(),
            reason = item.outcome.detail(),
            "固定资产状态变更：提交后的联动动作执行结果"
        );
    }
}

/// 资产卡片的制单人（`biz_fixed_asset.create_by`）。读不到 → `None`
/// （收件人为空时 `SEND_NOTICE` 自己判 Skipped，不会把状态变更推不上去）。
///
/// 为什么单独查一次而不是让调用方传：两个调用点的入参不同
/// （`asset_bridge` 只有 `AssetRow`、`asset_handler` 只有 `row.asset_no` + id），
/// 在这里查一次（主键 1 行）比给两个模型各加一个字段干净。
///
/// SCOPE-EXEMPT: 按主键取 1 行的 `create_by`，不做数据范围裁剪（联动发通知与
/// 「谁能看到这张卡」无关；裁剪会让代填场景下的制单人收不到自己的通知）。
async fn creator_of(ex: &impl SqlExecutor, asset_id: Uuid) -> Option<Uuid> {
    ex.scalar_sql::<Option<Uuid>>(
        "SELECT create_by FROM biz_fixed_asset WHERE id = ?::uuid".to_string(),
        vec![Value::from(&asset_id)],
    )
    .await
    .ok()
    .flatten()
}

// ==================== 单测：纯逻辑（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn change(status: &str) -> AssetStatusChange {
        AssetStatusChange {
            asset_id: Uuid::parse_str("018f0000-0000-7000-8000-000000000003").expect("uuid"),
            asset_no: "FA-20990115-000001".to_string(),
            status: status.to_string(),
            operator: None,
            creator: None,
            run: linkage::LinkageRun::default(),
        }
    }

    /// ① 通知幂等键必须是「状态 + 卡片」维度：同一张卡先处置后作废各一条，
    /// 同一状态重复推进只落一条（CAS 之外的第三道保险）。
    #[test]
    fn notice_idempotency_key_is_per_status_and_asset() {
        let disposed = change(asset_dto::STATUS_DISPOSED);
        let restored = change(asset_dto::STATUS_IN_USE);
        let disposed_again = change(asset_dto::STATUS_DISPOSED);
        let key = |ctx: &linkage::LinkageContext<'_>| {
            ctx.notice
                .as_ref()
                .and_then(|draft| draft.idempotency_key.clone())
                .expect("联动上下文必须带通知幂等键")
        };
        assert_ne!(
            key(&disposed.ctx()),
            key(&restored.ctx()),
            "处置与作废处置必须是不同幂等键"
        );
        assert_eq!(
            key(&disposed.ctx()),
            key(&disposed_again.ctx()),
            "同一卡片同一状态重复推进必须撞同一个幂等键（只发一条）"
        );
        assert!(
            key(&disposed.ctx()).starts_with("LNK:FIXED_ASSET:"),
            "幂等键前缀是清理夹具与排障的判据：{}",
            key(&disposed.ctx())
        );
    }

    /// ② 通知文案 / 链接 / 业务类型取自卡片本身（状态中文名来自 `spec::TRIGGER_SPECS`）。
    #[test]
    fn notice_draft_carries_asset_context() {
        let disposed = change(asset_dto::STATUS_DISPOSED);
        let ctx = disposed.ctx();
        let draft = ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(draft.biz_type.as_deref(), Some(NOTICE_BIZ_TYPE));
        assert_eq!(
            draft.biz_id.as_deref(),
            Some("018f0000-0000-7000-8000-000000000003")
        );
        assert_eq!(
            draft.link_url.as_deref(),
            Some("/business/erp/asset?id=018f0000-0000-7000-8000-000000000003")
        );
        assert!(
            draft.title.contains("已处置"),
            "标题必须用人读的中文状态名：{}",
            draft.title
        );
        assert!(
            draft.content.contains("FA-20990115-000001"),
            "正文必须带资产编号：{}",
            draft.content
        );
        assert_eq!(ctx.entity, state_machine::ENTITY_FIXED_ASSET);
        assert_eq!(ctx.status, asset_dto::STATUS_DISPOSED);
    }

    /// ③ 状态中文名与单据模块的 `status_label` 逐字一致（配置页下拉 / 通知文案不许分叉）。
    #[test]
    fn status_labels_match_asset_dto() {
        for status in [
            asset_dto::STATUS_IN_USE,
            asset_dto::STATUS_IDLE,
            asset_dto::STATUS_DISPOSED,
        ] {
            assert_eq!(
                linkage::spec::status_label_of(state_machine::ENTITY_FIXED_ASSET, status),
                Some(asset_dto::status_label(status)),
                "`{status}` 的中文名在 spec 与 asset_dto 之间分叉了"
            );
        }
    }
}

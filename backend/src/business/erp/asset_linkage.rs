// ============================================================================
// asset_linkage.rs —— 固定资产（FIXED_ASSET）状态变更的**跨单据联动**接线（LNK-09 / ST02）
//
// 资产的联动上下文 / 排除清单 / 幂等口径只在本文件写一份，触发点是三处**真实存在**的
// 状态推进（「建卡」是建行，不接）：
//
// | 触发点 | 入口 | 目标态 |
// |--------|------|--------|
// | 处置（审批通过） | `asset_bridge.rs::execute` 第 ⑤ 步（凭证 + 状态机 + 卡片回填 + 留痕之后、`tx.commit()` 之前） | `IN_USE\|IDLE → DISPOSED` |
// | 作废处置 | `asset_handler.rs::asset_dispose_cancel` 第 ⑤ 步（红冲 + 状态机 + 恢复在役 + 留痕之后、`tx.commit()` 之前） | `DISPOSED → IN_USE` |
// | 停用 / 启用 | `asset_handler.rs::asset_status_change` 第 ⑦ 步（状态机 `IN_USE ⇄ IDLE` + CAS + 留痕之后、`tx.commit()` 之前） | `IN_USE ⇄ IDLE` |
//
// # 为什么「建卡」不接
//
// 建卡是 `NULL → IN_USE` 的**建行**动作（`asset_handler::asset_create` 写的是
// `log_event` 而不是状态流转），与「新建单据不算触发点」同一口径：挂上就是
// 「导入 N 张卡 = 扇出 N 条通知」。
//
// # 停用 / 启用的口径（FIN-ASSET-ST02 D4/D7，别顺手改）
//
// 停用只改状态标签与动作可见性：**折旧照提**（`scan_assets_for_period` 的
// `status IN ('IN_USE','IDLE')` / `in_depreciation_scope` / 边标签「停用（闲置，照提折旧）」
// 一律不动），不落凭证、不走审批。所以本 hook 的排除清单与处置完全一致（见下）。
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
// 停用 / 启用本来不落凭证，为什么也要排除？—— **一个实体一张排除清单**：否则
// 「停用会不会落凭证」就取决于运营怎么配规则；统一排除 = 资产域任何状态变更都不由通用引擎落账。
//
// 库存 / 余额类动作（`WRITE_INVENTORY` / `UPDATE_COST` / `UPDATE_PAYABLE_BALANCE`）
// 不列进来：资产卡片上没有物料 / 余额列，列进来只是噪音（与合同 `CONTRACT` 同口径）。
//
// # 通知幂等键 = 事件维度（FIN-ASSET-ST02 D5，修 LNK-09 遗留 bug）
//
// 键 = `LNK:FIXED_ASSET:<目标态>:<卡片 id>:<event_ref>`，`event_ref` = 本次写入的
// `biz_fixed_asset_change.id`（append-only 事件本体）。
//
// 旧键只有「状态 + 卡片」，而 `uk_sys_notice_idem` 是**单列唯一**（V0049）⇒ 可逆边第二
// 次事件必然被吞：停用 → 启用 → 再停用（第二次停用不发），处置 → 作废处置 → 再处置
// （第二次处置不发，LNK-09 遗留）。同型先例是 LNK-10 的 `depreciation_linkage::event_ref`
// （= 本次凭证 id），字段名沿用同一个，别造第二个概念。
//
// # 事务边界（铁律 ④）
//
// - 事务内：`run_in_tx` → `engine::run_excluding`，关键动作失败 → 整笔处置 / 作废 / 停用回滚
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
    /// 操作人（处置发起人 / 作废处置的人 / 停用启用的人）。
    pub(crate) operator: Option<Uuid>,
    /// **本次事件标识** = 本次写入的 `biz_fixed_asset_change.id`（通知幂等键用它）。
    ///
    /// 为什么要它：可逆边（停用 ⇄ 启用、处置 ⇄ 作废处置）会**重复落到同一个目标态**，
    /// 只按「状态 + 卡片」去重会把第二次真实事件吞掉（LNK-09 遗留 bug）。字段名与
    /// LNK-10 的 `depreciation_linkage::event_ref` 同名同义。
    pub(crate) event_ref: Uuid,
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
        // 幂等键 = **事件维度**（状态 + 卡片 + 本次变动行 id）：同一事件重放只发一条
        // （CAS 之外的第二道闸），不同事件（可逆边的第二次）各发一条。见文件头 §幂等键。
        let key = format!("LNK:FIXED_ASSET:{}:{}:{}", self.status, self.asset_id, self.event_ref);
        // `sys_notice.idempotency_key` 是 VARCHAR(128)（V0043）；本键最长 98 字符。
        // 钉住它，免得将来有人把状态名写长之后才在库里撞列宽（LNK-10 同款断言）。
        assert!(key.len() <= 128, "通知幂等键超长（{}/128）：{key}", key.len());
        draft.idempotency_key = Some(key);
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
    // 本次事件标识（= 调用方刚写入的变动行 id）：进通知幂等键，**必填**
    event_ref: Uuid,
) -> std::result::Result<AssetStatusChange, ApiError> {
    let mut change = AssetStatusChange {
        asset_id,
        asset_no: asset_no.to_string(),
        status: status.to_string(),
        operator,
        event_ref,
        creator: creator_of(ex, asset_id).await,
        run: linkage::LinkageRun::default(),
    };
    let ctx = change.ctx();
    change.run = linkage::engine::run_excluding(ex, &ctx, linkage::spec::FIXED_ASSET_EXCLUDED_ACTIONS).await?;
    if !change.run.is_empty() {
        tracing::info!(
            asset_id = %change.asset_id,
            asset_no = change.asset_no,
            status,
            event_ref = %change.event_ref,
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

    fn change(status: &str, event_ref: Uuid) -> AssetStatusChange {
        AssetStatusChange {
            asset_id: Uuid::parse_str("018f0000-0000-7000-8000-000000000003").expect("uuid"),
            asset_no: "FA-20990115-000001".to_string(),
            status: status.to_string(),
            operator: None,
            event_ref,
            creator: None,
            run: linkage::LinkageRun::default(),
        }
    }

    /// 两次事件的两条变动行 id（模拟「处置一次 / 再处置一次」两个 append-only 行）。
    fn event_a() -> Uuid {
        Uuid::parse_str("018f0000-0000-7000-8000-0000000000a1").expect("uuid")
    }

    fn event_b() -> Uuid {
        Uuid::parse_str("018f0000-0000-7000-8000-0000000000b2").expect("uuid")
    }

    /// ① 通知幂等键必须是**事件**维度：同一事件重放只发一条，不同事件各发一条。
    ///
    /// 这条同时是 LNK-09 遗留 bug 的回归守卫：旧键只有「状态 + 卡片」，
    /// 「处置 → 作废 → 再处置」的第二次处置与第一次同状态 ⇒ 撞 `uk_sys_notice_idem`
    /// 被 `ON CONFLICT DO NOTHING` 吞掉，用户永远收不到第二条。可逆边（停用 / 启用）
    /// 更是必然第二次失效。
    #[test]
    fn notice_idempotency_key_is_per_event_and_asset() {
        let disposed = change(asset_dto::STATUS_DISPOSED, event_a());
        let restored = change(asset_dto::STATUS_IN_USE, event_b());
        let disposed_again = change(asset_dto::STATUS_DISPOSED, event_b());
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
        assert_ne!(
            key(&disposed.ctx()),
            key(&disposed_again.ctx()),
            "同一张卡两次处置（不同变动行）必须是不同幂等键：否则第二次真实事件被静默吞掉"
        );
        assert_eq!(
            key(&disposed.ctx()),
            key(&change(asset_dto::STATUS_DISPOSED, event_a()).ctx()),
            "同一事件重放（同一变动行 id）必须撞同一个幂等键（重试不重复发）"
        );
        let sample = key(&disposed.ctx());
        assert!(
            sample.starts_with("LNK:FIXED_ASSET:"),
            "幂等键前缀是清理夹具与排障的判据：{sample}"
        );
        assert!(
            sample.ends_with(&event_a().to_string()),
            "幂等键必须以本次变动行 id 结尾（事件维度）：{sample}"
        );
        assert!(
            sample.len() <= 128,
            "`sys_notice.idempotency_key` 是 VARCHAR(128)，当前键长 {}：{sample}",
            sample.len()
        );
    }

    /// ② 通知文案 / 链接 / 业务类型取自卡片本身（状态中文名来自 `spec::TRIGGER_SPECS`）。
    #[test]
    fn notice_draft_carries_asset_context() {
        let disposed = change(asset_dto::STATUS_DISPOSED, event_a());
        let ctx = disposed.ctx();
        let draft = ctx.notice.as_ref().expect("必须带通知草稿");
        assert_eq!(draft.biz_type.as_deref(), Some(NOTICE_BIZ_TYPE));
        assert_eq!(draft.biz_id.as_deref(), Some("018f0000-0000-7000-8000-000000000003"));
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

    // ==================== 预置规则（V0323 · FIX-3 问题 1） ====================
    //
    // 「接线了但没规则」的回归守卫：`spec::TRIGGER_SPECS` 标 wired、三处 hook 都在跑，
    // 但迁移里 0 条 FIXED_ASSET 规则 ⇒ 停用 / 启用 / 处置一条通知都不发且不报错。
    // 迁移一旦应用就不能改（指纹校验），所以这里用 `include_str!` 读**迁移原文**，
    // 与代码侧的契约（触发点 / 状态 / 动作 / 参数 / TAKEOVER / 幂等写法）逐条对拍：
    // 任何一处漂移 → 用例红，且文案直接说是哪条规则。

    /// 迁移原文（up / down）。
    const PRESET_UP: &str = include_str!("../../../migrations/business/V0323__asset_linkage_rules.up.sql");
    const PRESET_DOWN: &str = include_str!("../../../migrations/business/V0323__asset_linkage_rules.down.sql");

    /// 3 条预置规则（规则名 + 触发状态；触发状态 = 变更**之后**的目标态）。
    const PRESETS: &[(&str, &str)] = &[
        ("资产停用 → 通知制单人", asset_dto::STATUS_IDLE),
        ("资产启用 / 作废处置 → 通知制单人", asset_dto::STATUS_IN_USE),
        ("资产处置 → 通知制单人", asset_dto::STATUS_DISPOSED),
    ];

    /// ① 迁移里正好 3 条规则，且**覆盖资产的每一个合法状态**（少一条 = 那个状态变更不发通知）。
    #[test]
    fn presets_cover_every_fixed_asset_status() {
        assert_eq!(PRESETS.len(), 3, "卡面要求 3 条 FIXED_ASSET 规则");
        let inserts = PRESET_UP.matches("gen_random_uuid()").count();
        assert_eq!(inserts, 3, "迁移 up.sql 里应有 3 行 INSERT（实际 {inserts}）");
        let known = linkage::model::known_statuses(state_machine::ENTITY_FIXED_ASSET);
        let mut preset_statuses: Vec<&str> = PRESETS.iter().map(|(_, status)| *status).collect();
        preset_statuses.sort_unstable();
        let mut sorted_known = known.clone();
        sorted_known.sort_unstable();
        assert_eq!(
            preset_statuses, sorted_known,
            "预置规则的状态集必须等于资产的合法状态集（否则哪个状态漏了就永远不发通知）"
        );
        for (name, status) in PRESETS {
            assert!(
                known.contains(status),
                "规则「{name}」的触发状态 {status} 不是 FIXED_ASSET 的合法状态"
            );
        }
    }

    /// ② 迁移原文逐条包含规则名 + 触发键 + 动作码（文本与代码对拍）。
    #[test]
    fn migration_seeds_every_preset_row() {
        for (name, status) in PRESETS {
            assert!(PRESET_UP.contains(name), "迁移里找不到规则名「{name}」");
            assert!(
                PRESET_UP.contains(&format!("'FIXED_ASSET', '{status}', 'SEND_NOTICE'")),
                "迁移里找不到触发键 ('FIXED_ASSET', '{status}', 'SEND_NOTICE')"
            );
        }
        assert!(
            PRESET_UP.contains(&format!("trigger_entity = 'FIXED_ASSET'")),
            "迁移的自检块必须按 FIXED_ASSET 过滤"
        );
    }

    /// ③ 参数能过第 2 层校验，且**必须是 TAKEOVER**。
    ///
    /// `SHADOW` 只做影子对比、不发通知（`engine` 里非 TAKEOVER 直接跳过投递）；
    /// 资产通知**没有旧代码在发**（`AssetStatusChange::ctx` 只把草稿塞进上下文，
    /// 投递完全靠引擎）⇒ 写成 SHADOW 等于规则配了也一条不发。
    #[test]
    fn preset_params_are_takeover_and_valid() {
        let params_literal = r#"{"mode": "TAKEOVER"}'::jsonb"#;
        assert_eq!(
            PRESET_UP.matches(params_literal).count(),
            3,
            "三条规则的参数必须都是 {params_literal}（资产通知没有旧代码兜底，只能 TAKEOVER）"
        );
        let action = linkage::spec::spec_of(linkage::spec::ACTION_SEND_NOTICE).expect("SEND_NOTICE 必须在动作白名单里");
        assert!(action.implemented, "SEND_NOTICE 必须是已接管动作");
        let params: serde_json::Value = serde_json::from_str(r#"{"mode": "TAKEOVER"}"#).expect("合法 JSON");
        if let Err(reason) = linkage::spec::validate_params(action, &params) {
            panic!("预置规则的参数过不了第 2 层校验：{reason}");
        }
        assert_eq!(params.get("mode").and_then(|value| value.as_str()), Some("TAKEOVER"));
        for key in params.as_object().expect("参数必须是对象").keys() {
            assert!(
                !linkage::spec::is_forbidden_key(key),
                "预置规则参数里出现禁用键 `{key}`（禁配 SQL）"
            );
        }
    }

    /// ④ 幂等写法：`ON CONFLICT (...) DO NOTHING`，**不许** DO UPDATE。
    ///
    /// 生产上可能已有运维手工配的规则（改过收件人 / 停用过 / 软删过），迁移重跑
    /// 不能把它们刷回预置值 —— 那是替运维改配置。
    #[test]
    fn presets_are_idempotent_without_overwriting_manual_rules() {
        assert_eq!(
            PRESET_UP
                .matches("ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING")
                .count(),
            1,
            "预置迁移必须按唯一键 ON CONFLICT DO NOTHING（幂等且不覆盖手工配置）"
        );
        assert!(
            !PRESET_UP.contains("DO UPDATE"),
            "预置迁移不许 DO UPDATE：会把运维手工配的规则（收件人 / 启停 / 软删）刷回预置值"
        );
    }

    /// ⑤ down 段删的是**同一批键**（不是按名字），并自检删干净。
    #[test]
    fn preset_down_removes_the_same_keys() {
        for (_, status) in PRESETS {
            assert!(
                PRESET_DOWN.contains(&format!("('FIXED_ASSET', '{status}', 'SEND_NOTICE')")),
                "down.sql 没有删 ('FIXED_ASSET', '{status}', 'SEND_NOTICE')：回滚会留下半套规则"
            );
        }
        assert!(
            PRESET_DOWN.contains("DELETE FROM biz_flow_linkage_rule"),
            "down.sql 必须删规则行"
        );
    }

    /// ⑥ 触发点必须真的接线（wired + 三处 hook 的存在性由 `flow::linkage::tests_wiring` 钉住；
    ///    这里再确认「配了规则一定能命中」：FIXED_ASSET 在 `TRIGGER_SPECS` 里且 wired）。
    #[test]
    fn fixed_asset_trigger_is_wired_so_presets_can_fire() {
        let trigger = linkage::spec::TRIGGER_SPECS
            .iter()
            .find(|spec| spec.entity == state_machine::ENTITY_FIXED_ASSET)
            .expect("FIXED_ASSET 必须在 TRIGGER_SPECS 里（否则配置页都选不到它）");
        assert!(
            trigger.wired,
            "FIXED_ASSET 必须标 wired=true（未接线的触发点配了规则也永远不触发）"
        );
        for (_, status) in PRESETS {
            assert!(
                linkage::spec::status_label_of(state_machine::ENTITY_FIXED_ASSET, status).is_some(),
                "`{status}` 不在 FIXED_ASSET 的状态标签表里：规则永远等不到这个状态"
            );
        }
    }

    /// ⑦ 处置闸必须真的接在 handler 的**提交步**（FIX-3 方案 ①）。
    ///
    /// 纯函数判据再好，调用点被删一行就静默失效 —— 这条与 `flow::linkage::tests_wiring`
    /// 的「调用点漂移」口径一致：用 `include_str!` 读 handler 原文断关键片段与顺序。
    #[test]
    fn disposal_dep_gate_is_wired_into_dispose_handler() {
        let handler = include_str!("asset_handler.rs");
        let dispose = handler
            .find("pub async fn asset_dispose(")
            .expect("asset_handler.rs 必须有 asset_dispose");
        let body = &handler[dispose..];
        // ① 判据必须来自纯函数（不许在 handler 里重写一套「当月有没有明细」的 if）
        let gate = body
            .find("asset_compute::disposal_dep_gate(")
            .expect("处置入口必须调 asset_compute::disposal_dep_gate（判据只有一份实现）");
        // ② 未过账必须回 409 + 66048（不许只打日志放行）
        assert!(
            body.contains("error_code::ERP_ASSET_DEPRECIATION_PENDING"),
            "处置闸未过账必须回 ERP_ASSET_DEPRECIATION_PENDING（66048）"
        );
        assert!(
            body.contains("409,"),
            "处置闸未过账必须是 409（业务冲突），不是 400/500"
        );
        // ③ 顺序：闸门必须在「查流程 / 起审批」之前（fail fast，失败在提交前）
        let flow = body
            .find("asset_bridge::published_disposal_flow()")
            .expect("处置入口必须查处置审批流程");
        assert!(
            gate < flow,
            "处置闸必须排在查流程 / 起审批**之前**（当前 gate@{gate} / flow@{flow}）"
        );
        // ④ 不许把闸门挪到审批落定后的执行步（那时审批已通过，半路 409 会卡住处置单）
        let bridge = include_str!("asset_bridge.rs");
        let execute = bridge
            .find("async fn execute(instance_id: Uuid)")
            .expect("asset_bridge.rs 必须有 execute");
        let exec_body = &bridge[execute..];
        assert!(
            !exec_body.contains("disposal_dep_gate("),
            "处置闸不许放在审批落定后的执行步：审批已通过时半路 409 只会把处置单卡在中间态"
        );
    }
}

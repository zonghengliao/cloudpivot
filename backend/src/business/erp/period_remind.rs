//! 结账催办（PERIOD-CLOSE · 「一键催办直达」）。
//!
//! # 它解决什么
//!
//! 结账预检把挡路单据列出来了（见 [`crate::business::erp::period_items`]），
//! 但「知道是哪 3 张」还不够 —— 财务得**通知到人**。本模块提供那一按：
//!
//! ```text
//! POST /business/erp/period/{id}/remind
//! body: { "blocker_kind": "VOUCHER_DRAFT", "item_ids": ["uuid1", "uuid2"] }   // item_ids 空 = 该类全部
//! →   { sent, failed, skipped_no_owner, skipped_recent, total, message }
//! ```
//!
//! # 收件人怎么定（不猜）
//!
//! 每条明细在 [`period_items`] 里已经算好了 owner：
//!
//! | 明细里的 owner | 发给谁 |
//! |----------------|--------|
//! | `owner_user_id` 有值 | 发给**那一个人**（`NoticeTarget::User`） |
//! | 只有 `owner_role_id` | 发给**该角色全体启用用户**（`NoticeTarget::Role`） |
//! | 都没有 | 计入 `skipped_no_owner`，**不发**（不往全员推：催全员等于没人负责） |
//!
//! **明细不信任前端传的收件人**：请求只带「哪一类 + 哪几张单」，
//! 收件人在服务端重新跑一遍 `collect_blockers` 现算 —— 前端改不动收件人，
//! 也不会因为页面停留期间单据被人处理了而催错人（那类明细会被判「已不在挡路清单」
//! 而跳过）。
//!
//! # 防骚扰：同一张单 24 小时内不重复催（两层判据 + 幂等键）
//!
//! 1. **台账**：`biz_reminder_log` 里 `remind_kind = 'PERIOD_REMIND'`（年结是
//!    `YEAR_END_REMIND`）且 `doc_id = 单据 id` 且 `sent_at > now() - 24h` →
//!    计入 `skipped_recent`，不再发。台账是 append-only 的事实表，**不随通知生命周期
//!    消失**（`notice_expire_cleanup` 会按 `expire_at` 硬删过期通知）；
//! 2. **通知记录**：`sys_notice` 里 `biz_type = 'period_remind'` 且 `biz_id = 单据 id`
//!    且 `created_at > now() - 24h` → 同样计入 `skipped_recent`。这一层保留是为了
//!    **兼容 P1-FIX-10 之前已经发出去的历史通知**（那时还没有台账行），否则升级当天
//!    同一张单会被再催一次；
//! 3. **幂等键**：`PERIOD:REMIND:{期间}:{单据类型}:{单据 id}:{日期}`，
//!    并发点两次时第二次命中 `uk_sys_notice_idem` → `duplicated = true`，同样计入
//!    `skipped_recent`（不会两个人各发一遍）。
//!
//! 判据是「同一张单」而不是「同一个人」：一张单只有一个下一步动作，
//! 24h 内换个人再催一遍等于骚扰同一个处理组 —— 比卡面口径更严，**有意为之**。
//!
//! # 通知内容（卡面模板逐字）
//!
//! ```text
//! 标题：{期间} 月结账待处理：{单据号}
//! 正文：{制单人} 的 {单据类型} {单号}（{金额}）还是 {状态}，挡住了 {期间} 的结账。
//!       请 {action_label}。已等待 {N} 天。
//! ```
//!
//! 制单人未知时写「制单人未知」，金额为空时整段金额括号省略（不写 `（-）`）。
//!
//! # 为什么复用 `notice::dispatch`
//!
//! 模板渲染 / 扇出 / 偏好过滤 / 投递记录 / SSE 推送全在那一个口子里
//! （批次 A 的唯一发送口径）。本模块的产品只是一份 [`NoticeDraft`]。
//! `force = true`：结账被挡是「漏了就结不了账」的一类，跳过 `notify.inbox` 偏好。
//!
//! # 年结催办为什么也走这里（YEAR-END）
//!
//! 年结的催办与期间催办**只差四样**：标题里的「月 / 年」、正文里的口径、
//! `sys_notice.biz_type`（消息中心分组）、幂等键前缀。收件人怎么算、24h 防重、
//! 扇出、结果计数**全共用** —— 所以抽成 [`RemindScope`] + [`dispatch_all`]，
//! 年结那边（[`crate::business::erp::year_end_remind`]）只写「上下文 + 取数 + 权限码」。
//!
//! | | 期间催办 | 年结催办 |
//! |---|---|---|
//! | 标题 | `2036-03 月结账待处理：{单号}` | `2026 年结账待处理：{单号}` |
//! | 正文口径 | `挡住了 2036-03 的结账` | `挡住了 2026 年度的结账` |
//! | `biz_type` | `period_remind` | `year_end_remind` |
//! | 幂等键 | `PERIOD:REMIND:…` | `YEAREND:REMIND:…` |
//! | 权限码 | `business:erp:period:remind` | `business:erp:year-end:remind` |
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 催办的对象是「会计期间的四项前置校验结果」（全局台账，无 dept_id / 归属人列），
//! 可见性与操作性由 business:erp:period:remind 控制。
//! ```

use axum::Json;
use nik::prelude::chrono::{DateTime, Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::result::Result;

use crate::business::erp::period;
use crate::business::erp::period_items::{self, BlockingItem};
use crate::business::erp::reminder::{self, ReminderLedgerRow};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 口径常量 ====================

/// 催办权限码（新增：能结账 ≠ 能催人）。
pub const PERM_REMIND: &str = "business:erp:period:remind";

/// 通知的业务类型（`sys_notice.biz_type`，通知中心按它分组）。
pub const BIZ_TYPE: &str = "period_remind";

/// 年结催办的通知业务类型（与期间催办分开：两边 24h 防重与消息中心分组互不干扰）。
pub const BIZ_TYPE_YEAR_END: &str = "year_end_remind";

/// 发送人展示名（调用方是登录用户时会被替换成他的名字）。
pub const SENDER_NAME: &str = "会计期间";

/// 同一张单的重复催办间隔（小时）—— 防骚扰。
pub const REMIND_WINDOW_HOURS: i64 = 24;

/// 台账 `remind_kind`：月结账催办（`biz_reminder_log.remind_kind`，V0257 起入值域）。
pub const LEDGER_KIND_PERIOD: &str = "PERIOD_REMIND";

/// 台账 `remind_kind`：年结账催办。
pub const LEDGER_KIND_YEAR_END: &str = "YEAR_END_REMIND";

/// 一次催办最多处理多少条明细（与 `period_items::ITEM_LIMIT` 同量级；
/// 单次请求超过这个数直接 400：既不是人点得出来的，也会把通知系统打爆）。
pub const REMIND_MAX_ITEMS: usize = 50;

// ==================== 入参 / 出参 ====================

/// 催办入参。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RemindReq {
    /// 催哪一类（`DRAFT_VOUCHER` / `DOC_UNFINISHED` / `INVARIANT` …）。
    ///
    /// 空 = 这一期间**所有**有处理人的明细一起催（前端「全部催办」按类循环调用，
    /// 但接口本身也接受空值，少一次往返）。
    #[serde(default)]
    pub blocker_kind: Option<String>,
    /// 催哪几张（明细里的 `doc_id`）。空 = 该类全部。
    #[serde(default)]
    pub item_ids: Vec<String>,
}

/// 催办结果。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct RemindRes {
    /// 发出去了几条（= 通知条数，不是收件人数：发角色是 1 条通知 N 个收件人）。
    pub sent: i64,
    /// 发送失败的条数（可重试）。
    pub failed: i64,
    /// 没有处理人而跳过的条数（前端提示「N 张未指定处理人」）。
    pub skipped_no_owner: i64,
    /// 因为 24 小时内已经催过（或并发重复）而跳过的条数。
    pub skipped_recent: i64,
    /// 本次命中的明细条数（`sent + failed + skipped_no_owner + skipped_recent`）。
    pub total: i64,
    /// 一句给人看的结论（前端 toast 直接用）。
    pub message: String,
}

// ==================== 错误收口 ====================

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 404：期间不存在。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        "会计期间不存在（期间行不可删除，请确认 id）".to_string(),
    )
}

/// 数据库错误 → 500（带上下文）。
fn db_error(error: DbError, ctx: &str) -> ApiError {
    utils::db_error(error, ctx)
}

// ==================== 催办上下文（期间 / 年结共用） ====================

/// 一次催办的**上下文**：期间催办与年结催办只有这几处不同，其余全部共用。
///
/// 显式写出每个串，而不是拼「{code} {单位}」，是因为中文里
/// 「2036-03 月结账」与「2026 年度结账」的词形不一样（标题要「2026 年结账」、
/// 正文要「2026 年度的结账」），靠一个单位字拼不出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemindScope {
    /// 标题前缀：「2036-03 月」/「2026 年」（拼成「{前缀}结账待处理：{单号}」）。
    pub title_prefix: String,
    /// 正文里的口径：「2036-03 的结账」/「2026 年度的结账」（拼成「挡住了{口径}」）。
    pub blocked_phrase: String,
    /// 结果摘要前缀：「2036-03：」/「2026 年度：」。
    pub summary_prefix: String,
    /// 幂等键里的定位串：「2036-03」/「2026」。
    pub key_label: String,
    /// `sys_notice.biz_type`（消息中心分组 + 24h 防重的判据）。
    pub biz_type: &'static str,
    /// 幂等键前缀（`PERIOD:REMIND` / `YEAREND:REMIND`）。
    pub idem_prefix: &'static str,
    /// `biz_reminder_log.remind_kind`（台账类型段：`PERIOD_REMIND` / `YEAR_END_REMIND`）。
    pub ledger_kind: &'static str,
}

impl RemindScope {
    /// 期间催办的上下文（`2036-03`）。
    pub fn period(period_code: &str) -> Self {
        Self {
            title_prefix: format!("{period_code} 月"),
            blocked_phrase: format!("{period_code} 的结账"),
            summary_prefix: format!("{period_code}："),
            key_label: period_code.to_string(),
            biz_type: BIZ_TYPE,
            idem_prefix: "PERIOD:REMIND",
            ledger_kind: LEDGER_KIND_PERIOD,
        }
    }

    /// 年结催办的上下文（`2026`）。
    pub fn year_end(year: i16) -> Self {
        Self {
            title_prefix: format!("{year} 年"),
            blocked_phrase: format!("{year} 年度的结账"),
            summary_prefix: format!("{year} 年度："),
            key_label: year.to_string(),
            biz_type: BIZ_TYPE_YEAR_END,
            idem_prefix: "YEAREND:REMIND",
            ledger_kind: LEDGER_KIND_YEAR_END,
        }
    }
}

/// 通知标题：「{前缀}结账待处理：{单据号}」。
pub fn title_of(scope: &RemindScope, item: &BlockingItem) -> String {
    format!("{}结账待处理：{}", scope.title_prefix, item.doc_no)
}

/// 通知正文（卡面模板逐字）。
pub fn content_of(scope: &RemindScope, item: &BlockingItem, now: DateTime<Utc>) -> String {
    let creator = item
        .created_by_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("制单人未知");
    let amount = match item.amount {
        Some(value) => format!("（{}）", money_text(value)),
        None => String::new(),
    };
    format!(
        "{creator} 的 {} {}{amount} 还是 {}，挡住了 {}。请{}。已等待 {} 天。",
        period_items::doc_type_label(&item.doc_type),
        item.doc_no,
        item.status_label,
        scope.blocked_phrase,
        item.action_label,
        item.waiting_days(now),
    )
}

/// 幂等键：`{前缀}:{定位}:{类型}:{id}:{日期}`（同一天同一张单只发一条）。
pub fn idempotency_key_of(scope: &RemindScope, item: &BlockingItem, now: DateTime<Utc>) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        scope.idem_prefix,
        scope.key_label,
        item.doc_type,
        item.doc_id,
        now.format("%Y%m%d")
    )
}

/// 通知标题：「{期间} 月结账待处理：{单据号}」（期间催办的便捷入口）。
pub fn remind_title(period_code: &str, item: &BlockingItem) -> String {
    title_of(&RemindScope::period(period_code), item)
}

/// 通知正文（期间催办的便捷入口）。
pub fn remind_content(period_code: &str, item: &BlockingItem, now: DateTime<Utc>) -> String {
    content_of(&RemindScope::period(period_code), item, now)
}

/// 金额文案：去掉无意义的尾零（`1234.0000` → `1234`，`1234.5000` → `1234.5`）。
pub fn money_text(value: Decimal) -> String {
    value.round_dp(4).normalize().to_string()
}

/// 幂等键：`PERIOD:REMIND:{期间}:{类型}:{id}:{日期}`（同一天同一张单只发一条）。
pub fn remind_idempotency_key(
    period_code: &str,
    item: &BlockingItem,
    now: DateTime<Utc>,
) -> String {
    idempotency_key_of(&RemindScope::period(period_code), item, now)
}

/// 这条明细的发送目标；`None` = 没有处理人（不发，计入 `skipped_no_owner`）。
pub fn target_of(item: &BlockingItem) -> Option<NoticeTarget> {
    if let Some(user_id) = item.owner_user_id {
        return Some(NoticeTarget::User(vec![user_id]));
    }
    item.owner_role_id
        .map(|role_id| NoticeTarget::Role(vec![role_id]))
}

/// 把明细组装成一条通知草稿（`None` = 没有处理人）。
///
/// # 参数
///
/// - `scope`：催办上下文（期间 / 年结），决定标题、正文口径、`biz_type` 与幂等键；
/// - `item`：挡路明细（已带 owner）；
/// - `now`：时间基准（「已等待 N 天」与幂等键的日期）；
/// - `operator`：催办人（进 `sender_id / sender_name`，通知里能看出是谁催的）。
pub fn build_draft_in(
    scope: &RemindScope,
    item: &BlockingItem,
    now: DateTime<Utc>,
    operator: &state_machine::Operator,
) -> Option<NoticeDraft> {
    let target = target_of(item)?;
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Warn,
        title_of(scope, item),
        content_of(scope, item, now),
        target,
    );
    draft.link_url = Some(item.route.clone());
    draft.biz_type = Some(scope.biz_type.to_string());
    draft.biz_id = Some(item.doc_id.to_string());
    draft.idempotency_key = Some(idempotency_key_of(scope, item, now));
    draft.sender_id = operator.id;
    draft.sender_name = Some(
        operator
            .name
            .clone()
            .unwrap_or_else(|| SENDER_NAME.to_string()),
    );
    // 结账被挡 = 漏了就直接造成「结不了账」的一类，强制送达（跳过 notify.inbox 偏好）；
    // 与 flow::notify::FlowEvent::Urge / transfer_overdue 同分级。
    draft.force = true;
    Some(draft)
}

/// 把明细组装成一条通知草稿（期间催办的便捷入口）。
pub fn build_draft(
    period_code: &str,
    item: &BlockingItem,
    now: DateTime<Utc>,
    operator: &state_machine::Operator,
) -> Option<NoticeDraft> {
    build_draft_in(&RemindScope::period(period_code), item, now, operator)
}

/// 一条催办对应的台账行（`biz_reminder_log`）。
///
/// # 列口径
///
/// | 列 | 值 | 为什么 |
/// |----|----|--------|
/// | `doc_type` | 明细的 `doc_type`（`VOUCHER` / `STOCK_IN` / `PURCHASE_ORDER` / `STOCK_TRANSFER` / `INVENTORY`） | 与明细同一套单据类型，`REMINDER_DOC_KINDS` 已按 V0257 放宽 |
/// | `doc_id` / `doc_no` | 明细的单据主键 / 单号快照 | 与 4 类提醒台账同口径，可按单追「催过几次」 |
/// | `remind_kind` | `PERIOD_REMIND` / `YEAR_END_REMIND` | 期间与年结分开统计；也与 `sys_notice.biz_type` 一一对应 |
/// | `stage` | 期间码（`2036-03`）或年度（`2036`） | 台账里一眼看出「为哪个月催的」，不必回查单据期间 |
/// | `remind_date` | 业务日（Asia/Shanghai） | 与 4 类提醒共用幂等键 `(doc_type, doc_id, remind_kind, remind_date)` |
///
/// 写成纯函数（不碰库）是为了让「列取哪个值」能被单测钉住 —— 台账写错列不会报错，
/// 只会在半年后查不到数据时才发现。
pub fn ledger_row_of<'a>(
    scope: &'a RemindScope,
    item: &'a BlockingItem,
    notice_id: Uuid,
    receiver_count: i64,
    now: DateTime<Utc>,
) -> ReminderLedgerRow<'a> {
    ReminderLedgerRow {
        doc_type: item.doc_type.as_str(),
        doc_id: item.doc_id,
        doc_no: Some(item.doc_no.as_str()),
        remind_kind: scope.ledger_kind,
        stage: scope.key_label.as_str(),
        remind_date: reminder::business_day(now),
        notice_id: Some(notice_id),
        receiver_count,
    }
}

/// 从明细里挑出本次要催的那些（`kind` 空 = 全部；`ids` 空 = 该类全部）。
///
/// `sources` 是 `(blocker 类型, 该类明细)` 的序列：期间催办与年结催办都能直接传，
/// 「哪一类 + 哪几张」的筛选逻辑只此一份。
pub fn pick_from<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a [BlockingItem])>,
    kind: Option<&str>,
    ids: &[String],
) -> Result<Vec<BlockingItem>, ApiError> {
    let wanted_kind = kind.map(str::trim).filter(|value| !value.is_empty());
    let mut picked = Vec::new();
    for (blocker_kind, items) in sources {
        if let Some(wanted) = wanted_kind
            && blocker_kind != wanted
        {
            continue;
        }
        picked.extend(items.iter().cloned());
    }
    if ids.is_empty() {
        return Ok(picked);
    }
    let wanted_ids = ids
        .iter()
        .map(|raw| {
            Uuid::parse_str(raw.trim()).map_err(|_| {
                bad_request(format!(
                    "item_ids 里有非法 UUID：{raw}（应为明细里的 doc_id）"
                ))
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let filtered = picked
        .into_iter()
        .filter(|item| wanted_ids.contains(&item.doc_id))
        .collect::<Vec<_>>();
    Ok(filtered)
}

/// 从期间预检结果里挑出本次要催的那些（期间催办的便捷入口）。
pub fn pick_items(
    blockers: &[period::Blocker],
    kind: Option<&str>,
    ids: &[String],
) -> Result<Vec<BlockingItem>, ApiError> {
    pick_from(
        blockers
            .iter()
            .map(|blocker| (blocker.kind.as_str(), blocker.items.as_slice())),
        kind,
        ids,
    )
}

// ==================== 催办主流程 ====================

/// 催办：按明细的 owner 发站内信（24 小时内不重复）。
///
/// # 异常情况
///
/// - `400`：`item_ids` 有非法 UUID / 条数超过 [`REMIND_MAX_ITEMS`] / 期间 id 非法；
/// - `404`：期间不存在；
/// - 单条 `dispatch` 失败只计入 `failed` 并继续下一条（一条失败不该让 49 条都发不出去）。
pub async fn remind_period(
    id: Uuid,
    req: &RemindReq,
    operator: state_machine::Operator,
) -> Result<RemindRes, ApiError> {
    if req.item_ids.len() > REMIND_MAX_ITEMS {
        return Err(bad_request(format!(
            "一次最多催 {REMIND_MAX_ITEMS} 张（当前 {} 张）：勾选少一些再催",
            req.item_ids.len()
        )));
    }
    let now = Utc::now();
    let row = period::load_period_row(db(), id)
        .await?
        .ok_or_else(not_found)?;
    let outcome = period::collect_blockers(db(), &row).await?;
    let picked = pick_items(
        &outcome.blockers,
        req.blocker_kind.as_deref(),
        &req.item_ids,
    )?;

    if picked.is_empty() {
        return Ok(RemindRes {
            total: 0,
            message: "没有可催办的单据：该类挡路项已被处理（请重新点「结账」刷新）".to_string(),
            ..RemindRes::default()
        });
    }

    let scope = RemindScope::period(&row.period_code);
    let result = dispatch_all(&scope, &picked, now, &operator).await?;
    tracing::info!(
        scope = %row.period_code,
        sent = result.sent,
        failed = result.failed,
        skipped_no_owner = result.skipped_no_owner,
        skipped_recent = result.skipped_recent,
        "结账催办完成"
    );
    Ok(result)
}

/// 逐条发送（**期间 / 年结共用的唯一发送循环**）。
///
/// 四条行为在两边必须一致：
/// * 没有处理人 → 计入 `skipped_no_owner`，不发（不往全员推）；
/// * 24 小时内催过 → 计入 `skipped_recent`，不再打扰；
/// * 单条 `dispatch` 失败 → 计入 `failed` 并继续（一条失败不该让 49 条都发不出去）；
/// * 发送成功 → **先发信、再记台账**（`biz_reminder_log`，append-only）。记账失败只打
///   WARN 不抛错：信已经发出去了，把整次催办判红会让用户以为没催成、再点一次。
///   记账失败的那张单下一轮会被重复催一次 —— 由通知幂等键兜住（同键第二次 dispatch
///   返回 `duplicated`），不会真的多发一条。
pub async fn dispatch_all(
    scope: &RemindScope,
    items: &[BlockingItem],
    now: DateTime<Utc>,
    operator: &state_machine::Operator,
) -> Result<RemindRes, ApiError> {
    let mut result = RemindRes {
        total: i64::try_from(items.len()).unwrap_or(i64::MAX),
        ..RemindRes::default()
    };
    for item in items {
        let Some(draft) = build_draft_in(scope, item, now, operator) else {
            result.skipped_no_owner += 1;
            continue;
        };
        if reminded_recently(scope, item.doc_id, now).await? {
            result.skipped_recent += 1;
            continue;
        }
        match notice::dispatch(draft).await {
            Ok(reply) if reply.duplicated => result.skipped_recent += 1,
            Ok(reply) => {
                record_ledger(scope, item, &reply, now).await;
                result.sent += 1;
            }
            Err(error) => {
                result.failed += 1;
                tracing::warn!(
                    scope = %scope.key_label,
                    doc_no = %item.doc_no,
                    error = %error,
                    "结账催办发送失败（计入 failed，可重试）"
                );
            }
        }
    }
    result.message = summary_in(scope, &result);
    Ok(result)
}

/// 记台账（失败只 WARN：信已经发出去了，不能因为记账失败把 `sent` 判成失败）。
async fn record_ledger(
    scope: &RemindScope,
    item: &BlockingItem,
    reply: &notice::DispatchResult,
    now: DateTime<Utc>,
) {
    let row = ledger_row_of(scope, item, reply.notice_id, reply.receiver_count, now);
    if let Err(error) = reminder::ledger_insert(row).await {
        tracing::warn!(
            scope = %scope.key_label,
            doc_no = %item.doc_no,
            error = %error,
            "结账催办台账写入失败（通知已发出，仅少一条审计记录）"
        );
    }
}

/// 24 小时内是否已经催过这张单。
///
/// 两层**或**关系（任一命中即视为催过）：
/// * `biz_reminder_log`（V0257 起）：`remind_kind` + `doc_id` + `sent_at > now()-24h`。
///   这是主判据 —— 台账不随 `notice_expire_cleanup` 的硬删消失；
/// * `sys_notice`：`biz_type` + `biz_id` + `created_at > now()-24h`。
///   保留它是为了兼容升级前已经发出的历史通知（那些没有台账行）。
///
/// 判据带 `biz_type` / `remind_kind`：期间催办与年结催办互不干扰（同一个单据 id
/// 在两个上下文里各催一次）。
async fn reminded_recently(
    scope: &RemindScope,
    doc_id: Uuid,
    now: DateTime<Utc>,
) -> Result<bool, ApiError> {
    let since = now - Duration::hours(REMIND_WINDOW_HOURS);

    // ① 台账（append-only，通知被清理也还在）
    let in_ledger: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*)::bigint FROM biz_reminder_log \
              WHERE remind_kind = ? AND doc_id = ? AND sent_at > ?",
            vec![
                Value::Text(scope.ledger_kind.to_string()),
                Value::from(doc_id),
                Value::from(since),
            ],
        )
        .await
        .map_err(|error| db_error(error, "查询最近催办台账"))?;
    if in_ledger > 0 {
        return Ok(true);
    }

    // ② 通知表（兼容升级前的历史通知）
    let count: i64 = db()
        .scalar_sql(
            "SELECT COUNT(*) FROM sys_notice \
              WHERE biz_type = ? AND biz_id = ? AND created_at > ?"
                .to_string(),
            vec![
                Value::Text(scope.biz_type.to_string()),
                Value::Text(doc_id.to_string()),
                Value::from(since),
            ],
        )
        .await
        .map_err(|error| db_error(error, "查询最近催办记录"))?;
    Ok(count > 0)
}

/// 一句给人看的结论。
pub fn summary_in(scope: &RemindScope, result: &RemindRes) -> String {
    if result.total == 0 {
        return "没有可催办的单据".to_string();
    }
    let mut parts = vec![format!("已催办 {} 张", result.sent)];
    if result.skipped_recent > 0 {
        parts.push(format!(
            "{} 张{}小时内已催过（不重复打扰）",
            result.skipped_recent, REMIND_WINDOW_HOURS
        ));
    }
    if result.skipped_no_owner > 0 {
        parts.push(format!("{} 张未指定处理人", result.skipped_no_owner));
    }
    if result.failed > 0 {
        parts.push(format!("{} 张发送失败（可重试）", result.failed));
    }
    format!("{}{}", scope.summary_prefix, parts.join("，"))
}

/// 一句给人看的结论（期间催办的便捷入口）。
pub fn summary_of(period_code: &str, result: &RemindRes) -> String {
    summary_in(&RemindScope::period(period_code), result)
}

// ==================== 接口层 ====================

/// 催办（把挡路单据的待办动作发给处理人）。
#[nik::permission(
    path = "/business/erp/period/{id}/remind",
    method = "POST",
    permission = "business:erp:period:remind"
)]
#[nik::post("/business/erp/period/{id}/remind")]
pub async fn period_remind_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<RemindReq>,
) -> ResResult<RemindRes> {
    period::ensure_permission(&claims, PERM_REMIND, "催办结账待办")?;
    let id = period::parse_period_id(&id)?;
    Ok(Res::ok(
        remind_period(id, &req, state_machine::Operator::from(&claims)).await?,
    ))
}

#[cfg(test)]
#[path = "period_remind_tests.rs"]
mod tests;

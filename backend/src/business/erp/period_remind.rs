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
//! # 防骚扰：同一张单 24 小时内不重复催
//!
//! 两层：
//! 1. **查库**：`sys_notice` 里 `biz_type = 'period_remind'` 且 `biz_id = 单据 id`
//!    且 `created_at > now() - 24h` → 计入 `skipped_recent`，不再发
//!    （判据是「最近通知记录」，与卡面要求一致，不建新表）；
//! 2. **幂等键**：`PERIOD:REMIND:{期间}:{单据类型}:{单据 id}:{日期}`，
//!    并发点两次时第二次命中 `uk_sys_notice_idem` → `duplicated = true`，同样计入
//!    `skipped_recent`（不会两个人各发一遍）。
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

/// 发送人展示名（调用方是登录用户时会被替换成他的名字）。
pub const SENDER_NAME: &str = "会计期间";

/// 同一张单的重复催办间隔（小时）—— 防骚扰。
pub const REMIND_WINDOW_HOURS: i64 = 24;

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

// ==================== 纯逻辑（不连库，可单测） ====================

/// 通知标题：「{期间} 月结账待处理：{单据号}」。
pub fn remind_title(period_code: &str, item: &BlockingItem) -> String {
    format!("{period_code} 月结账待处理：{}", item.doc_no)
}

/// 通知正文（卡面模板逐字）。
pub fn remind_content(period_code: &str, item: &BlockingItem, now: DateTime<Utc>) -> String {
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
        "{creator} 的 {} {}{amount} 还是 {}，挡住了 {period_code} 的结账。请{}。已等待 {} 天。",
        period_items::doc_type_label(&item.doc_type),
        item.doc_no,
        item.status_label,
        item.action_label,
        item.waiting_days(now),
    )
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
    format!(
        "PERIOD:REMIND:{period_code}:{}:{}:{}",
        item.doc_type,
        item.doc_id,
        now.format("%Y%m%d")
    )
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
/// - `period_code`：期间编码（`2026-10`），标题与正文都用它；
/// - `item`：挡路明细（已带 owner）；
/// - `now`：时间基准（「已等待 N 天」与幂等键的日期）；
/// - `operator`：催办人（进 `sender_id / sender_name`，通知里能看出是谁催的）。
pub fn build_draft(
    period_code: &str,
    item: &BlockingItem,
    now: DateTime<Utc>,
    operator: &state_machine::Operator,
) -> Option<NoticeDraft> {
    let target = target_of(item)?;
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Warn,
        remind_title(period_code, item),
        remind_content(period_code, item, now),
        target,
    );
    draft.link_url = Some(item.route.clone());
    draft.biz_type = Some(BIZ_TYPE.to_string());
    draft.biz_id = Some(item.doc_id.to_string());
    draft.idempotency_key = Some(remind_idempotency_key(period_code, item, now));
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

/// 从明细里挑出本次要催的那些（`kind` 空 = 全部；`ids` 空 = 该类全部）。
pub fn pick_items(
    blockers: &[period::Blocker],
    kind: Option<&str>,
    ids: &[String],
) -> Result<Vec<BlockingItem>, ApiError> {
    let wanted_kind = kind.map(str::trim).filter(|value| !value.is_empty());
    let mut picked = Vec::new();
    for blocker in blockers {
        if let Some(wanted) = wanted_kind
            && blocker.kind != wanted
        {
            continue;
        }
        for item in &blocker.items {
            picked.push(item.clone());
        }
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

    let mut result = RemindRes {
        total: i64::try_from(picked.len()).unwrap_or(i64::MAX),
        ..RemindRes::default()
    };
    for item in &picked {
        let Some(draft) = build_draft(&row.period_code, item, now, &operator) else {
            result.skipped_no_owner += 1;
            continue;
        };
        if reminded_recently(item.doc_id, now).await? {
            result.skipped_recent += 1;
            continue;
        }
        match notice::dispatch(draft).await {
            Ok(reply) if reply.duplicated => result.skipped_recent += 1,
            Ok(_) => result.sent += 1,
            Err(error) => {
                result.failed += 1;
                tracing::warn!(
                    period = %row.period_code,
                    doc_no = %item.doc_no,
                    error = %error,
                    "结账催办发送失败（计入 failed，可重试）"
                );
            }
        }
    }
    result.message = summary_of(&row.period_code, &result);
    tracing::info!(
        period = %row.period_code,
        sent = result.sent,
        failed = result.failed,
        skipped_no_owner = result.skipped_no_owner,
        skipped_recent = result.skipped_recent,
        "结账催办完成"
    );
    Ok(result)
}

/// 24 小时内是否已经催过这张单（`sys_notice` 里最近一条 `period_remind`）。
async fn reminded_recently(doc_id: Uuid, now: DateTime<Utc>) -> Result<bool, ApiError> {
    let since = now - Duration::hours(REMIND_WINDOW_HOURS);
    let count: i64 = db()
        .scalar_sql(
            "SELECT COUNT(*) FROM sys_notice \
              WHERE biz_type = ? AND biz_id = ? AND created_at > ?"
                .to_string(),
            vec![
                Value::Text(BIZ_TYPE.to_string()),
                Value::Text(doc_id.to_string()),
                Value::from(since),
            ],
        )
        .await
        .map_err(|error| db_error(error, "查询最近催办记录"))?;
    Ok(count > 0)
}

/// 一句给人看的结论。
pub fn summary_of(period_code: &str, result: &RemindRes) -> String {
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
    format!("{period_code}：{}", parts.join("，"))
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

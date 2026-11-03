//! 年结催办（YEAR-END · 「一键把挡路的单据催到处理人」）。
//!
//! # 它解决什么
//!
//! 年结预检现在会把挡路单据列成表（见 [`crate::business::erp::period_items`] 与
//! [`crate::business::erp::year_end`] 的 `collect_blockers`），但「知道是哪 3 张」
//! 还不够 —— 财务得**通知到人**。本模块提供那一按：
//!
//! ```text
//! POST /business/erp/year-end/remind
//! body: { "year": 2026, "blocker_kind": "DOC_UNFINISHED", "item_ids": ["uuid1"] }  // item_ids 空 = 该类全部
//! →   { sent, failed, skipped_no_owner, skipped_recent, total, message }
//! ```
//!
//! # 为什么几乎是空的（复用，不照抄）
//!
//! 结账催办（[`crate::business::erp::period_remind`]）已经把那套东西写好了：
//! 收件人按明细的 owner 现算、复用 `notice::dispatch` 扇出、同一张单 24h 防重、
//! 结果四个计数 + 一句人话。年结与结账**只差四样**（标题里的「年」、正文口径、
//! `sys_notice.biz_type`、幂等键前缀），所以那边把这四样抽成了
//! [`period_remind::RemindScope`] + [`period_remind::dispatch_all`]，
//! 本模块只做三件事：
//!
//! ```text
//! ① 上下文：RemindScope::year_end(year)   → 标题「{年度} 年结账待处理：{单据号}」
//! ② 取数：year_end::collect_blockers(全年) → pick_from 挑出要催的那几张
//! ③ 权限：business:erp:year-end:remind（另立一码，见 V0208）
//! ```
//!
//! **权限码为什么另立**：能年结 ≠ 该被允许去催人。只读的财务助理可以催人，
//! 但不该拿到「结转整年」的能力；反过来有年结权的人也不一定愿意被催办消息打扰。
//! 与期间催办（`business:erp:period:remind`，V0207）同一套理由。
//!
//! # 命名说明（照卡面）
//!
//! 卡面点名 `business:erp:year-end:remind`，而 V0190 已有的三个年结码是
//! `business:erp:yearend:{read,close,reopen}`（`yearend` 连写）。这里**照卡面**用
//! `year-end`，并在 V0208 的自检里把「一码一节点」钉住：前端 / 后端 / 迁移三处
//! 必须是同一个串，拼错任何一个都会 403 —— 单测 `year_end_remind_permission_code_is_pinned`
//! 与迁移自检各挡一层。
//!
//! # 收件人怎么定（不猜）
//!
//! | 明细里的 owner | 发给谁 |
//! |----------------|--------|
//! | `owner_user_id` 有值 | 发给**那一个人**（`NoticeTarget::User`） |
//! | 只有 `owner_role_id` | 发给**该角色全体启用用户**（`NoticeTarget::Role`） |
//! | 都没有 | 计入 `skipped_no_owner`，**不发**（催全员等于没人负责） |
//!
//! 收件人**不信任前端**：请求只带「哪一类 + 哪几张单」，服务端重新跑一遍
//! `collect_blockers` 现算（页面停留期间单据被人处理了，那一类明细会消失 → 跳过）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 催办的对象是「年度结转的五项前置校验结果」（全局会计台账，
//! 无 dept_id / 归属人列），可见性与操作性由 business:erp:year-end:remind 控制。
//! ```

use axum::Json;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use serde::Deserialize;
use std::result::Result;

use crate::business::erp::period;
use crate::business::erp::period_items::BlockingItem;
use crate::business::erp::period_remind::{
    self, REMIND_MAX_ITEMS, RemindReq, RemindRes, RemindScope,
};
use crate::business::erp::year_end::{self, YearEndBlocker};
use crate::business::shared::state_machine;
use crate::shared::error_code;

// ==================== 口径常量 ====================

/// 催办权限码（照卡面 `year-end`；与 V0190 的 `yearend:` 三码并存，理由见模块文档）。
pub const PERM_REMIND: &str = "business:erp:year-end:remind";

/// 通知业务类型（`sys_notice.biz_type`，通知中心按它分组；与期间催办互不干扰）。
pub const BIZ_TYPE: &str = period_remind::BIZ_TYPE_YEAR_END;

/// 发送人展示名（调用方是登录用户时会被替换成他的名字）。
pub const SENDER_NAME: &str = "年度结转";

// ==================== 入参 ====================

/// 年结催办入参（`year` 必填：年结的「上下文」是年度，不是期间 id）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct YearEndRemindReq {
    /// 会计年度（如 2026）。
    pub year: i16,
    /// 催哪一类（`DOC_UNFINISHED` / `INVARIANT`）。
    /// 空 = 本次预检里**所有**有处理人的明细一起催（前端「全部催办」用）。
    #[serde(default)]
    pub blocker_kind: Option<String>,
    /// 催哪几张（明细里的 `doc_id`）。空 = 该类全部。
    #[serde(default)]
    pub item_ids: Vec<String>,
}

impl YearEndRemindReq {
    /// 转成期间催办同款的筛选参数（筛选逻辑只此一份）。
    fn as_remind(&self) -> RemindReq {
        RemindReq {
            blocker_kind: self.blocker_kind.clone(),
            item_ids: self.item_ids.clone(),
        }
    }
}

// ==================== 纯逻辑（不连库，可单测） ====================

/// 年结催办的上下文：标题「{年度} 年结账待处理：{单据号}」，正文「挡住了 {年度} 年度的结账」。
pub fn scope_of(year: i16) -> RemindScope {
    RemindScope::year_end(year)
}

/// 从年结阻塞项里挑出本次要催的明细（`kind` 空 = 全部；`ids` 空 = 该类全部）。
///
/// 与期间催办共用 [`period_remind::pick_from`]：**「哪一类 + 哪几张」的筛选不许有两套实现**。
pub fn pick(
    blockers: &[YearEndBlocker],
    kind: Option<&str>,
    ids: &[String],
) -> Result<Vec<BlockingItem>, ApiError> {
    period_remind::pick_from(
        blockers
            .iter()
            .map(|blocker| (blocker.kind.as_str(), blocker.items.as_slice())),
        kind,
        ids,
    )
}

/// 400：入参不合法。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 催办主流程 ====================

/// 年结催办：按明细的 owner 发站内信（同一张单 24 小时内不重复）。
///
/// 实现上**没有一行发送逻辑**：取数 → 挑选 → [`period_remind::dispatch_all`]。
///
/// # 异常情况
///
/// - `400`：`year` 不在 2000–2998 / `item_ids` 有非法 UUID / 条数超过上限；
/// - 单条 `dispatch` 失败只计入 `failed` 并继续下一条。
pub async fn remind_year_end(
    req: &YearEndRemindReq,
    operator: state_machine::Operator,
) -> Result<RemindRes, ApiError> {
    let year = year_end::validate_year(Some(req.year))?;
    if req.item_ids.len() > REMIND_MAX_ITEMS {
        return Err(bad_request(format!(
            "一次最多催 {REMIND_MAX_ITEMS} 张（当前 {} 张）：勾选少一些再催",
            req.item_ids.len()
        )));
    }
    let now = Utc::now();
    let blockers = year_end::collect_blockers(db(), year).await?;
    let remind = req.as_remind();
    let picked = pick(&blockers, remind.blocker_kind.as_deref(), &remind.item_ids)?;

    if picked.is_empty() {
        return Ok(RemindRes {
            total: 0,
            message: "没有可催办的单据：该类挡路项已被处理（请重新点「刷新」）".to_string(),
            ..RemindRes::default()
        });
    }

    let scope = scope_of(year);
    let result = period_remind::dispatch_all(&scope, &picked, now, &operator).await?;
    tracing::info!(
        year,
        sent = result.sent,
        failed = result.failed,
        skipped_no_owner = result.skipped_no_owner,
        skipped_recent = result.skipped_recent,
        "年结催办完成"
    );
    Ok(result)
}

/// 接口层：催办（把年结挡路单据的待办动作发给处理人）。
#[nik::permission(
    path = "/business/erp/year-end/remind",
    method = "POST",
    permission = "business:erp:year-end:remind"
)]
#[nik::post("/business/erp/year-end/remind")]
pub async fn year_end_remind_handler(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<YearEndRemindReq>,
) -> ResResult<RemindRes> {
    period::ensure_permission(&claims, PERM_REMIND, "催办年结待办")?;
    Ok(Res::ok(
        remind_year_end(&req, state_machine::Operator::from(&claims)).await?,
    ))
}

#[cfg(test)]
#[path = "year_end_remind_tests.rs"]
mod tests;

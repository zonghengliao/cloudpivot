//! 公告的三个定时任务 + 发布扇出的**唯一**实现（批次 C · C3）。
//!
//! # 任务清单（cron 与 `task/mod.rs` 的 `BATCH_C_TASKS` 逐字一致）
//!
//! | 任务 | cron | 做什么 | 依据 |
//! |------|------|--------|------|
//! | [`TASK_AUTO_EXPIRE`] | `0 5 0 * * *` | `PUBLISHED` 且 `expire_at <= now()` → `EXPIRED` | 架构 §2.6、PRD A1-05 |
//! | [`TASK_TOP_EXPIRE`] | `0 10 0 * * *` | `is_top` 且 `top_expire_at <= now()` → 取消置顶 | PRD A1-06 / §3.6 |
//! | [`TASK_DISPATCH_RETRY`] | `0 */10 * * * *` | `PUBLISHED` 且 `notice_id IS NULL` → 重放扇出（**幂等**） | 架构 §2.2、§7 C3 验收② |
//!
//! C0 已把这三个任务以「空实现」注册进调度器（让启动日志能看到排程）；
//! 本卡把任务体接到真实业务函数上，注册点仍是
//! [`crate::task::Scheduler::register_builtin`]（框架调度器启动时对注册表做一次性快照，
//! 写在 `bootstrap.rs` 里会被随后的 `reload()` 清空 —— 批次 A 已踩过）。
//!
//! # 扇出为什么只能有一个实现
//!
//! 发布接口（[`crate::business::announcement::handler::publish`]）与补偿任务都必须
//! 走 [`dispatch_announcement`]：两处各写一份「构造 NoticeDraft」的代码，就会出现
//! 「补偿发出去的站内信点进去是另一个链接」这类漂移。幂等键
//! `announcement:{id}:v{version}` 也只在那一处生成。
//!
//! # 失败语义
//!
//! 单个公告补偿失败**不影响**同批其它公告：逐条捕获、记 warn、继续下一条；
//! 任务本身只在数据库层面失败时返回 `Err`（由调度器记 error 日志）。

use alun::prelude::*;

use crate::business::announcement::dto::truncate_chars;
use crate::business::announcement::model::{self, PendingDispatchRow};
use crate::business::notice::dto::{CHANNEL_INBOX, ContentFormat, NoticeLevel, NoticeType};
use crate::business::notice::{self, DispatchResult, NoticeDraft, NoticeTarget};
use crate::task::Scheduler;

/// 公告到期自动下线（每天 00:05）。
pub const TASK_AUTO_EXPIRE: &str = "announcement_auto_expire";
/// 公告到期自动下线的 cron。
pub const CRON_AUTO_EXPIRE: &str = "0 5 0 * * *";

/// 公告置顶到期取消（每天 00:10）。
pub const TASK_TOP_EXPIRE: &str = "announcement_top_expire";
/// 置顶到期取消的 cron。
pub const CRON_TOP_EXPIRE: &str = "0 10 0 * * *";

/// 公告扇出补偿重试（每 10 分钟）。
pub const TASK_DISPATCH_RETRY: &str = "announcement_dispatch_retry";
/// 扇出补偿的 cron。
pub const CRON_DISPATCH_RETRY: &str = "0 */10 * * * *";

/// 补偿任务一次运行的统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetryReport {
    /// 扫描到的待补偿条数。
    pub scanned: usize,
    /// 补偿成功条数（含幂等重放）。
    pub repaired: usize,
    /// 补偿失败条数。
    pub failed: usize,
    /// 全局开关关闭而跳过的条数。
    pub skipped: usize,
}

impl RetryReport {
    /// 一行可读摘要（写日志用，运维按它 grep）。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_DISPATCH_RETRY}: scanned={} repaired={} failed={} skipped={}",
            self.scanned, self.repaired, self.failed, self.skipped
        )
    }
}

/// 在调度器上注册三个公告任务（由 `task::Scheduler::register_builtin` 调用）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_AUTO_EXPIRE,
        CRON_AUTO_EXPIRE,
        "系统任务：公告到期自动下线",
        || {
            tokio::spawn(async move {
                match run_auto_expire().await {
                    Ok(affected) => {
                        tracing::info!(task = TASK_AUTO_EXPIRE, affected, "公告到期下线完成")
                    }
                    Err(error) => {
                        tracing::error!(task = TASK_AUTO_EXPIRE, %error, "公告到期下线失败")
                    }
                }
            })
        },
    );
    scheduler.register_cron(
        TASK_TOP_EXPIRE,
        CRON_TOP_EXPIRE,
        "系统任务：公告置顶到期取消",
        || {
            tokio::spawn(async move {
                match run_top_expire().await {
                    Ok(affected) => {
                        tracing::info!(task = TASK_TOP_EXPIRE, affected, "公告置顶到期取消完成")
                    }
                    Err(error) => {
                        tracing::error!(task = TASK_TOP_EXPIRE, %error, "置顶到期取消失败")
                    }
                }
            })
        },
    );
    scheduler.register_cron(
        TASK_DISPATCH_RETRY,
        CRON_DISPATCH_RETRY,
        "系统任务：公告扇出补偿重试",
        || {
            tokio::spawn(async move {
                match run_dispatch_retry().await {
                    Ok(report) => {
                        tracing::info!(summary = %report.summary(), "公告扇出补偿完成")
                    }
                    Err(error) => {
                        tracing::error!(task = TASK_DISPATCH_RETRY, %error, "公告扇出补偿失败")
                    }
                }
            })
        },
    );
}

/// 执行一次「到期自动下线」。
///
/// # 返回值
///
/// 被置为 `EXPIRED` 的行数。
///
/// # 异常情况
///
/// 数据库异常返回中文错误消息（调用方记日志，不影响服务运行）。
pub async fn run_auto_expire() -> std::result::Result<u64, String> {
    model::expire_due()
        .await
        .map_err(|error| format!("公告到期下线失败: {error}"))
}

/// 执行一次「置顶到期取消」。
///
/// # 返回值
///
/// 被取消置顶的行数。
///
/// # 异常情况
///
/// 数据库异常返回中文错误消息。
pub async fn run_top_expire() -> std::result::Result<u64, String> {
    model::top_expire_due()
        .await
        .map_err(|error| format!("公告置顶到期取消失败: {error}"))
}

/// 执行一次扇出补偿（单批）。
///
/// # 返回值
///
/// [`RetryReport`]。
///
/// # 幂等（架构 §7 C3 验收②）
///
/// 幂等键 = `announcement:{id}:v{version}`，与首次发布完全一致 →
/// `uk_sys_notice_idem` 命中后 `dispatch` 直接回吐首次的 `notice_id`，
/// 不再插通知、不再扇出。因此本函数**跑多少次都只有一条 `sys_notice`**，
/// 只是把 `notice_id` 回填到公告上。
///
/// # 异常情况
///
/// 数据库异常返回中文错误消息（单条扇出失败只记日志，计入 `failed`）。
pub async fn run_dispatch_retry() -> std::result::Result<RetryReport, String> {
    let pending = model::pending_dispatch(model::DISPATCH_RETRY_BATCH)
        .await
        .map_err(|error| format!("扫描待补偿公告失败: {error}"))?;
    let mut report = RetryReport {
        scanned: pending.len(),
        ..Default::default()
    };
    if pending.is_empty() {
        return Ok(report);
    }

    // 全局止血开关：关闭时**一条都不补发**（否则「止血」会被补偿任务无声推翻）
    let enabled = model::fanout_enabled().await;
    if !enabled {
        report.skipped = pending.len();
        tracing::warn!(
            skipped = report.skipped,
            config = model::CONFIG_FANOUT_ENABLED,
            "公告扇出全局开关为关闭：本轮补偿整体跳过"
        );
        return Ok(report);
    }

    for row in &pending {
        match dispatch_announcement(row).await {
            Ok(result) => {
                if let Err(error) =
                    model::backfill_notice(&row.id, &result.notice_id, result.receiver_count).await
                {
                    tracing::warn!(
                        announcement_id = %row.id,
                        %error,
                        "扇出成功但回填 notice_id 失败，下一轮会重放（幂等）"
                    );
                    report.failed += 1;
                    continue;
                }
                report.repaired += 1;
                tracing::info!(
                    announcement_id = %row.id,
                    version = row.version,
                    notice_id = %result.notice_id,
                    receiver_count = result.receiver_count,
                    duplicated = result.duplicated,
                    "公告扇出补偿成功"
                );
            }
            Err(error) => {
                report.failed += 1;
                tracing::warn!(
                    announcement_id = %row.id,
                    version = row.version,
                    error = %error,
                    "公告扇出补偿失败，下一轮重试"
                );
            }
        }
    }
    Ok(report)
}

/// **唯一的**扇出实现：把一条公告按发布范围派成站内信。
///
/// # 参数
///
/// - `row`：待扇出公告（含范围明细，已由 [`model::pending_dispatch`] 一次查全）。
///
/// # 返回值
///
/// [`DispatchResult`]（含 `notice_id` / `receiver_count` / `duplicated`）。
///
/// # 异常情况
///
/// 目标为空 / 超 5 万 / 数据库异常等 → [`notice::DispatchError`]（由调用方决定是回给
/// 用户还是记日志）。
///
/// # 红线
///
/// 扇出**必须**走 [`notice::dispatch`]（批次 A 的唯一发送口径），不允许在这里直接
/// 往 `sys_notice` / `sys_notice_receiver` 插行（架构 §2.2、§5.4.3）。
pub async fn dispatch_announcement(
    row: &PendingDispatchRow,
) -> std::result::Result<DispatchResult, notice::DispatchError> {
    let target = notice_target(&row.scope_type, &row.scope_ids)?;
    let content = notice_content(row);
    let draft = NoticeDraft {
        notice_type: NoticeType::Circular,
        level: notice_level(&row.level),
        title: row.title.clone(),
        content,
        content_format: ContentFormat::Text,
        link_url: Some(format!("/announcement/{}", row.id)),
        biz_type: Some("announcement".to_string()),
        biz_id: Some(row.id.to_string()),
        template_code: None,
        template_vars: serde_json::Value::Null,
        target,
        channel_mask: CHANNEL_INBOX,
        idempotency_key: Some(idempotency_key(&row.id, row.version)),
        expire_at: row.expire_at,
        scheduled_at: None,
        sender_id: row.publisher_id,
        sender_name: Some(if row.publisher_name.trim().is_empty() {
            "公告".to_string()
        } else {
            row.publisher_name.clone()
        }),
    };
    notice::dispatch(draft).await
}

/// 扇出幂等键：`announcement:{id}:v{version}`（架构 §2.8.2 / 附录 A.1 的列注释逐字一致）。
///
/// # 参数
///
/// - `id`：公告主键；
/// - `version`：**发布后**的内容版本（每次发布 +1 → 编辑再发布必然产生第二行通知）。
pub fn idempotency_key(id: &uuid::Uuid, version: i32) -> String {
    format!("announcement:{id}:v{version}")
}

/// 公告范围 → 通知目标。
///
/// # 异常情况
///
/// 范围类型未知 / 非 `ALL` 但明细为空 → `400 + 10002`（由 `NoticeTarget` 的校验给出）。
pub fn notice_target(
    scope_type: &str,
    scope_ids: &[uuid::Uuid],
) -> std::result::Result<NoticeTarget, notice::DispatchError> {
    let target = match scope_type {
        "ALL" => NoticeTarget::All,
        "ROLE" => NoticeTarget::Role(scope_ids.to_vec()),
        "DEPT" => NoticeTarget::Dept(scope_ids.to_vec()),
        "USER" => NoticeTarget::User(scope_ids.to_vec()),
        other => {
            return Err(notice::DispatchError::Api(ApiError::new(
                400,
                crate::shared::error_code::PARAM_INVALID,
                format!("公告范围类型非法：{other}"),
            )));
        }
    };
    target.validate()?;
    Ok(target)
}

/// 公告级别 → 通知级别（`NORMAL→INFO` / `IMPORTANT→WARN` / `URGENT→ERROR`）。
pub fn notice_level(level: &str) -> NoticeLevel {
    match level {
        "URGENT" => NoticeLevel::Error,
        "IMPORTANT" => NoticeLevel::Warn,
        _ => NoticeLevel::Info,
    }
}

/// 通知正文：优先摘要，摘要为空时截取正文纯文本前 200 个字符。
///
/// # 为什么不用 HTML 正文
///
/// 站内信的正文是**纯文本**（`ContentFormat::Text`）：把渲染后的 HTML 塞进通知，
/// 通知中心就得再净化一次（两个渲染点 = 两套 bug）。公告详情通过 `link_url` 跳转。
pub fn notice_content(row: &PendingDispatchRow) -> String {
    let summary = row.summary.trim();
    if !summary.is_empty() {
        return truncate_chars(summary, 200);
    }
    let plain = crate::business::shared::content::to_plain_text(&row.content_md);
    let plain = plain.trim();
    if plain.is_empty() {
        row.title.clone()
    } else {
        truncate_chars(plain, 200)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(scope_type: &str, scope_ids: Vec<uuid::Uuid>) -> PendingDispatchRow {
        PendingDispatchRow {
            id: uuid::Uuid::now_v7(),
            version: 2,
            title: "关于 11 月 1 日系统维护的通知".into(),
            summary: String::new(),
            content_md: "## 维护时间\n\n22:00~24:00 停机\n".into(),
            level: "IMPORTANT".into(),
            scope_type: scope_type.into(),
            scope_ids,
            expire_at: None,
            publisher_id: None,
            publisher_name: "运维".into(),
        }
    }

    /// 幂等键形态固定（改格式 = 老公告会被重复扇出一次，属于事故）。
    #[test]
    fn idempotency_key_shape() {
        let id = uuid::Uuid::parse_str("018f4b2c-1111-7222-8333-444455556666").expect("uuid");
        assert_eq!(idempotency_key(&id, 3), format!("announcement:{id}:v3"));
        // A-05：编辑后再发布 = 新版本 = 新幂等键（第二行通知）
        assert_ne!(idempotency_key(&id, 1), idempotency_key(&id, 2));
    }

    /// 四种范围类型都能映射；非 ALL 空明细 → 400。
    #[test]
    fn notice_target_mapping() {
        let ids = vec![uuid::Uuid::now_v7()];
        assert!(matches!(
            notice_target("ALL", &[]).expect("ALL"),
            NoticeTarget::All
        ));
        assert!(matches!(
            notice_target("ROLE", &ids).expect("ROLE"),
            NoticeTarget::Role(_)
        ));
        assert!(matches!(
            notice_target("DEPT", &ids).expect("DEPT"),
            NoticeTarget::Dept(_)
        ));
        assert!(matches!(
            notice_target("USER", &ids).expect("USER"),
            NoticeTarget::User(_)
        ));

        let error = notice_target("USER", &[]).expect_err("空明细必须拒绝");
        assert_eq!(error.into_api().status, 400);

        let error = notice_target("WORLD", &ids).expect_err("未知类型必须拒绝");
        assert_eq!(error.into_api().status, 400);
    }

    /// 级别映射：URGENT → ERROR，IMPORTANT → WARN，其余 INFO。
    #[test]
    fn level_mapping() {
        assert_eq!(notice_level("URGENT"), NoticeLevel::Error);
        assert_eq!(notice_level("IMPORTANT"), NoticeLevel::Warn);
        assert_eq!(notice_level("NORMAL"), NoticeLevel::Info);
        assert_eq!(notice_level(""), NoticeLevel::Info);
    }

    /// 正文：摘要优先；摘要为空时从 Markdown 取纯文本；都空时回落标题。
    #[test]
    fn notice_content_fallbacks() {
        let mut row = pending("ALL", Vec::new());
        row.summary = "10 月 1 日至 7 日放假".into();
        assert_eq!(notice_content(&row), "10 月 1 日至 7 日放假");

        row.summary = "   ".into();
        let content = notice_content(&row);
        assert!(content.contains("维护时间"), "{content}");
        assert!(
            !content.contains("##"),
            "通知正文里不应残留 Markdown 标记：{content}"
        );

        row.content_md = String::new();
        assert_eq!(notice_content(&row), row.title);
    }

    /// 截断按字符（不切 UTF-8），超长补省略号。
    #[test]
    fn truncate_chars_is_utf8_safe() {
        assert_eq!(truncate_chars("云枢公告", 2), "云枢…");
        assert_eq!(truncate_chars("云枢公告", 4), "云枢公告");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
    }

    /// 补偿统计摘要格式稳定（运维按它 grep）。
    #[test]
    fn retry_report_summary() {
        let report = RetryReport {
            scanned: 3,
            repaired: 2,
            failed: 1,
            skipped: 0,
        };
        assert_eq!(
            report.summary(),
            "announcement_dispatch_retry: scanned=3 repaired=2 failed=1 skipped=0"
        );
    }

    /// cron 与任务名必须与 `task/mod.rs` 的 BATCH_C_TASKS 逐字一致。
    #[test]
    fn task_constants_are_stable() {
        assert_eq!(TASK_AUTO_EXPIRE, "announcement_auto_expire");
        assert_eq!(TASK_TOP_EXPIRE, "announcement_top_expire");
        assert_eq!(TASK_DISPATCH_RETRY, "announcement_dispatch_retry");
        assert_eq!(CRON_AUTO_EXPIRE, "0 5 0 * * *");
        assert_eq!(CRON_TOP_EXPIRE, "0 10 0 * * *");
        assert_eq!(CRON_DISPATCH_RETRY, "0 */10 * * * *");
    }
}

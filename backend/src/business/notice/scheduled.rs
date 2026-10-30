//! 定时通知投递巡逻任务（批次 A 遗留修复 · 项③，`scheduled_at`）。
//!
//! # 它解决什么
//!
//! [`crate::business::notice::dispatch`] 收到 `scheduled_at` 为**未来时间**的请求时，
//! 只落一条 `dispatch_status = 'PENDING'` 的 `sys_notice` 行，**不写** `sys_notice_receiver`。
//! 于是用户侧「我的通知 / 未读数」在到点前天然看不到这条通知（不是靠前端过滤隐藏）。
//! 本任务每 [`TASK_CRON`] 扫一次「到点待发」队列，把每条通知补齐成一次正常投递：
//!
//! ```text
//! PENDING ──(本任务：扇出 → 回写计数 → 通道投递 → 置 SENT)──▶ SENT
//!    │                                                        └─ 事务提交后实时推送（SSE）
//!    ├─ 失败 → dispatch_attempts+1；达 MAX_DISPATCH_ATTEMPTS 置 FAILED（不再重试）
//!    └─ 到点前被软删（deleted_at 非空）→ 置 CANCELED
//! ```
//!
//! # 并发安全
//!
//! 每条通知在自己的事务里用 `SELECT … FOR UPDATE SKIP LOCKED` 抢占：
//! 多实例 / 多进程同时扫同一批 ID 时，只有抢到行锁的那一个会真正投递，
//! 其余的看到的是「已被抢走」直接跳过（与 A4 导出的认领方式同源）。
//!
//! # 失败语义
//!
//! 单条失败只回滚该条事务并记一次尝试次数，不影响同批其它通知；计数器在**失败事务
//! 之外**单独累加（否则回滚会把计数一起丢掉，同一行会被每个周期重试到天荒地老）。
//!
//! # 注册位置
//!
//! 必须由 [`crate::task::Scheduler::register_builtin`] 注册（框架调度器对注册表做一次性
//! 快照，写在 `bootstrap.rs` 里会被随后的 `reload()` 清空 —— A1 已踩过）。

use nik::prelude::*;

use crate::business::notice::model::{ChannelSettings, Executor};
use crate::business::notice::sse_hub;
use crate::business::notice::{NoticeTarget, delivery, fanout, model, push_realtime};
use crate::task::Scheduler;

/// 调度任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "notice_scheduled_dispatch";

/// cron 表达式：每 30 秒扫一次（6 段写法，第 1 段是秒）。
///
/// 取 30 秒的权衡：定时发送的业务精度是「分钟级」，30 秒的抖动对用户无感；
/// 而 5 秒一次会把「待发队列」的空扫描频率抬到无意义的程度。
pub const TASK_CRON: &str = "*/30 * * * * *";

/// 单轮最多处理的通知条数（防止一次积压把调度线程拖住）。
pub const BATCH_SIZE: i64 = 50;

/// 单条定时通知的投递失败上限（含首次）。达到后置 `FAILED`，不再重试。
pub const MAX_DISPATCH_ATTEMPTS: i16 = 3;

/// 一轮巡逻的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// 扫描到的「到点待发」通知数。
    pub scanned: usize,
    /// 成功投递（置 `SENT`）的条数。
    pub dispatched: u64,
    /// 抢锁失败 / 已被取消而跳过的条数。
    pub skipped: u64,
    /// 投递失败（已累计尝试次数）的条数。
    pub failed: u64,
    /// 本轮顺带置为 `CANCELED` 的条数（到点前被软删）。
    pub canceled: u64,
}

impl SweepReport {
    /// 一行可读摘要（写日志用，运维 grep 依赖它）。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: scanned={} dispatched={} skipped={} failed={} canceled={}",
            self.scanned, self.dispatched, self.skipped, self.failed, self.canceled
        )
    }
}

/// 在调度器上注册本任务（由 `task::Scheduler::register_builtin` 调用）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
pub fn register_builtin(scheduler: &Scheduler) {
    // runner 必须返回 JoinHandle：框架调度器每次触发都会接管该句柄
    scheduler.register_cron(
        TASK_NAME,
        TASK_CRON,
        "系统任务：定时通知投递",
        || {
            tokio::spawn(async move {
                match run_once().await {
                    Ok(report) => tracing::info!(
                        summary = %report.summary(),
                        "定时通知投递完成"
                    ),
                    Err(error) => tracing::error!(error = %error, "定时通知投递失败"),
                }
            })
        },
    );
}

/// 执行一轮巡逻（可被单测 / 运维脚本直接调用）。
///
/// # 返回值
///
/// 本轮统计。
///
/// # 异常情况
///
/// 只返回「读队列」这一层的错误；单条投递失败计入 `failed` 并记日志，不冒泡。
pub async fn run_once() -> std::result::Result<SweepReport, String> {
    let mut report = SweepReport::default();

    // 到点前被软删的定时通知先收尾（否则它们永远留在 PENDING 里被反复扫到）
    match model::cancel_soft_deleted_scheduled(BATCH_SIZE).await {
        Ok(count) => report.canceled = count,
        Err(error) => tracing::warn!(error = %error, "清理已取消的定时通知失败"),
    }

    let ids = model::find_due_notice_ids(BATCH_SIZE)
        .await
        .map_err(|error| format!("查询到点通知失败: {error}"))?;
    report.scanned = ids.len();
    if ids.is_empty() {
        return Ok(report);
    }

    // 通道配置只需读一次（未配置的通道记 SKIPPED，不影响站内信）
    let settings = model::channel_settings_best_effort().await;

    for id in ids {
        match dispatch_one(id, &settings).await {
            Ok(true) => report.dispatched += 1,
            Ok(false) => report.skipped += 1,
            Err(error) => {
                report.failed += 1;
                tracing::error!(notice_id = %id, error = %error, "定时通知投递失败");
                if let Err(counter_error) =
                    model::record_dispatch_failure(&id, MAX_DISPATCH_ATTEMPTS).await
                {
                    tracing::error!(
                        notice_id = %id,
                        error = %counter_error,
                        "记录定时投递失败次数失败"
                    );
                }
            }
        }
    }

    Ok(report)
}

/// 投递一条到点通知（独立事务：抢占 → 扇出 → 计数 → 通道 → 置 SENT）。
///
/// # 参数
///
/// - `id`：通知 ID。
/// - `settings`：通道配置快照（在事务外读一次，避免长事务里再连池）。
///
/// # 返回值
///
/// - `Ok(true)`：本条已投递；
/// - `Ok(false)`：行已不是 `PENDING`（被别的实例抢走 / 已被取消），本轮跳过。
///
/// # 异常情况
///
/// 目标数据损坏 / 扇出 / 投递 / 状态回写失败 → `Err`（事务回滚，通知留在
/// `PENDING`，本轮会计一次尝试次数，下轮重试）。
pub async fn dispatch_one(
    id: uuid::Uuid,
    settings: &ChannelSettings,
) -> std::result::Result<bool, String> {
    // 目标先解析（纯内存校验）：坏数据在这里就暴露，不必把 ApiError 塞进事务的 DbError
    let pending = model::find_notice(&id)
        .await
        .map_err(|error| format!("查询通知失败: {error}"))?;
    let Some(notice) = pending else {
        return Ok(false);
    };
    if notice.dispatch_status != "PENDING" || notice.deleted_at.is_some() {
        return Ok(false);
    }
    let target = NoticeTarget::parse(&notice.target_type, &notice.target_ids)
        .map_err(|error| format!("通知目标非法: {}", error.msg))?;

    let pushed = db()
        .transaction(|tx| async move {
            let ex = Executor::Tx(&tx);
            // 重新抢占（`FOR UPDATE SKIP LOCKED`）：并发实例只有一个能拿到
            let Some(locked) = model::lock_due_notice(&ex, &id).await? else {
                return Ok(None);
            };
            let rows = fanout::fanout(&tx, &locked.id, &target).await?;
            model::refresh_receiver_count(&ex, &locked.id).await?;
            delivery::deliver(
                &ex,
                &locked.id,
                locked.channel_mask.max(0) as u16,
                settings,
                rows,
            )
            .await?;
            model::mark_notice_dispatched(&ex, &locked.id).await?;
            Ok(Some((
                locked.id,
                locked.notice_no,
                locked.notice_type,
                locked.level,
                locked.title,
                locked.link_url,
                locked.biz_type,
                locked.biz_id,
            )))
        })
        .await
        .map_err(|error| format!("{error}"))?;

    let Some((notice_id, notice_no, notice_type, level, title, link_url, biz_type, biz_id)) =
        pushed
    else {
        return Ok(false);
    };
    let seed = sse_hub::NoticeEvent {
        notice_id: notice_id.to_string(),
        notice_no,
        notice_type,
        level,
        title,
        link_url,
        biz_type,
        biz_id,
        receiver_id: String::new(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    // 事务提交后再推送：推送失败不影响已落库的投递结果（与立即发送同语义）
    push_realtime(&notice_id, &seed).await;
    tracing::info!(notice_id = %notice_id, "定时通知已到点投递");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 摘要格式稳定（运维 grep 依赖它）。
    #[test]
    fn sweep_report_summary_format() {
        let report = SweepReport {
            scanned: 4,
            dispatched: 2,
            skipped: 1,
            failed: 1,
            canceled: 3,
        };
        assert_eq!(
            report.summary(),
            "notice_scheduled_dispatch: scanned=4 dispatched=2 skipped=1 failed=1 canceled=3"
        );
        assert!(SweepReport::default().summary().contains("scanned=0"));
    }

    /// 任务名 / cron / 批大小 / 重试上限不得随意改动（改 cron 要同步运维文档）。
    #[test]
    fn sweep_constants_are_stable() {
        assert_eq!(TASK_NAME, "notice_scheduled_dispatch");
        assert_eq!(TASK_CRON, "*/30 * * * * *");
        assert_eq!(BATCH_SIZE, 50);
        assert_eq!(MAX_DISPATCH_ATTEMPTS, 3);
        // 6 段 cron 的第 1 段必须是秒，否则调度精度会从 30 秒掉到 30 分钟
        assert!(
            TASK_CRON.split_whitespace().count() == 6,
            "必须是 6 段 cron"
        );
        assert!(TASK_CRON.starts_with("*/30"), "必须每 30 秒触发");
    }
}

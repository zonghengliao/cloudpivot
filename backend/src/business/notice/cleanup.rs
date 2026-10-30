//! 通知清理任务（cron `0 50 4 * * *`，即每天 04:50，与存储回收错峰）。
//!
//! # 清理什么
//!
//! | 对象 | 条件 | 原因 |
//! |------|------|------|
//! | `sys_notice` | `expire_at < now()` | 过期的通知对用户无意义，硬删后接收行 / 投递记录由外键 `ON DELETE CASCADE` 一并清掉 |
//! | `sys_notice_receiver` | `is_deleted = TRUE AND deleted_at < now() - 30d` | 用户侧删除只是「我这儿不看了」，保留 30 天供送达率统计，之后物理清理 |
//!
//! # 注册方式（A1 已实测的坑）
//!
//! 由 [`crate::task::Scheduler::register_builtin`] 与既有日志 / 存储清理任务在同一处注册。
//! **不能**放 `bootstrap.rs`：框架调度器对注册表做一次性快照，`bootstrap` 之后还会跑
//! `reload()`，在里面注册的任务会被清空（A1 已踩，见 `business/storage/cleanup.rs` 头注释）。
//!
//! # 失败语义
//!
//! 单批失败只记日志、不 panic；任务下一轮会重试（删除是幂等的，重复执行无副作用）。

use nik::prelude::*;

use crate::task::Scheduler;

/// 调度任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "notice_expire_cleanup";

/// cron 表达式：每天 04:50（错开 04:30 的存储对象回收）。
pub const TASK_CRON: &str = "0 50 4 * * *";

/// 用户侧删除的接收行保留天数（期间仍参与送达率统计）。
pub const USER_DELETED_RETENTION_DAYS: i64 = 30;

/// 清理结果汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// 硬删的过期通知数（接收行 / 投递记录级联删除）。
    pub expired_notices: u64,
    /// 物理清理的用户删除接收行数。
    pub purged_receivers: u64,
}

impl CleanupReport {
    /// 生成一行可读摘要（写日志用）。
    ///
    /// # 返回值
    ///
    /// 形如 `notice_expire_cleanup: expired_notices=3 purged_receivers=12`。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: expired_notices={} purged_receivers={}",
            self.expired_notices, self.purged_receivers
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
        "系统任务：过期通知清理",
        || {
            tokio::spawn(async move {
                match run_once().await {
                    Ok(report) => {
                        tracing::info!(summary = %report.summary(), "过期通知清理完成")
                    }
                    Err(error) => tracing::error!(error = %error, "过期通知清理失败"),
                }
            })
        },
    );
}

/// 执行一次清理（可被单测与运维脚本直接调用）。
///
/// # 返回值
///
/// 清理统计。
///
/// # 异常情况
///
/// 数据库异常返回错误消息（由调用方记日志，不影响服务运行）。
///
/// SCOPE-EXEMPT: 过期通知清理是**后台定时任务**（无请求者身份），按 `expire_at`
/// 全局硬删；与「某个用户能看到哪些数据」无关（注入范围会让别人的过期通知永不清理）。
pub async fn run_once() -> std::result::Result<CleanupReport, String> {
    // 1) 过期通知：硬删（`sys_notice_receiver` / `sys_notice_delivery` 有 ON DELETE CASCADE）
    let expired_notices = db()
        .execute_with_params(
            "DELETE FROM sys_notice \
             WHERE expire_at IS NOT NULL AND expire_at < CURRENT_TIMESTAMP",
            Vec::new(),
        )
        .await
        .map_err(|error| format!("清理过期通知失败: {error}"))?;

    // 2) 用户侧删除的接收行：超过保留期后物理清理
    let cutoff = chrono::Utc::now() - chrono::Duration::days(USER_DELETED_RETENTION_DAYS);
    let purged_receivers = db()
        .execute_with_params(
            "DELETE FROM sys_notice_receiver \
             WHERE is_deleted = TRUE AND deleted_at IS NOT NULL AND deleted_at < ?",
            vec![Value::from(cutoff)],
        )
        .await
        .map_err(|error| format!("清理已删除接收行失败: {error}"))?;

    Ok(CleanupReport {
        expired_notices,
        purged_receivers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 摘要格式稳定（运维 grep 依赖它）。
    #[test]
    fn cleanup_report_summary_format() {
        let report = CleanupReport {
            expired_notices: 3,
            purged_receivers: 12,
        };
        assert_eq!(
            report.summary(),
            "notice_expire_cleanup: expired_notices=3 purged_receivers=12"
        );
    }

    /// 任务名 / cron / 保留期不得随意改动（改 cron 要同步运维文档）。
    #[test]
    fn cleanup_constants_are_stable() {
        assert_eq!(TASK_NAME, "notice_expire_cleanup");
        assert_eq!(TASK_CRON, "0 50 4 * * *");
        assert_eq!(USER_DELETED_RETENTION_DAYS, 30);
        assert_ne!(TASK_CRON, "0 30 4 * * *", "必须与存储对象回收（04:30）错峰");
    }
}

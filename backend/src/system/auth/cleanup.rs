//! 令牌吊销表（`sys_token_blacklist`）的日清任务（cron `0 50 3 * * *`，即每天 03:50）。
//!
//! # 为什么需要
//!
//! 自 ADR-20261101 起「哪条令牌被吊销」是库侧事实，每行只在令牌有效期内有意义。
//! 不清就会单调增长；**而清理任务同时是读路径查询计划的隐式前提**：表里过期行
//! 大量堆积时，规划器会从「主键点查」退化成「按 `expire_at` 范围扫 + `jti` 过滤」
//! （ADR §6 实测）。所以它挂了必须有人知道 —— 本任务发
//! `cloudpivot_token_blacklist_rows` / `cloudpivot_token_blacklist_cleanup_deleted`
//! 两个指标（未装 recorder 时是 no-op）。
//!
//! # 保留 1 小时宽限（**别改成 `now()`**）
//!
//! 判据是 `expire_at < now() - interval '1 hour'`：
//!
//! - 刚过期就删会让「已过期」与「已删除」之间出现边界抖动，多实例时钟偏差下
//!   可能出现「行没了但令牌其实还没到期」→ 已登出的令牌复活；
//! - 而 `is_revoked` 本身带了 `expire_at > now()`，过期行即使还在也不会被判成吊销
//!   （ADR §6 实测 ④），所以多留一小时不改变任何判定。
//!
//! # 注册方式
//!
//! 由 [`crate::task::Scheduler::register_builtin`] 注册（框架调度器对注册表做
//! **一次性快照**，必须在 `start()` 之前注册；放 `bootstrap.rs` 末尾会被随后的
//! `reload()` 清空——`business/storage/cleanup.rs` 记载的实测坑）。
//!
//! # 错峰
//!
//! 03:50，与五张日志表（03:00 / 03:10 / 03:20 / 03:30 / 03:40）和存储对象回收
//! （04:30）全部错开，避免同时发起批量删除抬高数据库瞬时压力。

use nik::prelude::*;

use crate::task::Scheduler;

/// 调度任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "token_blacklist_cleanup";

/// cron 表达式：每天 03:50（与 03:20/03:30/03:40 三张日志表错峰）。
pub const TASK_CRON: &str = "0 50 3 * * *";

/// 过期宽限秒数（`expire_at < now() - 1h` 才删，吸收时钟偏差）。
pub const GRACE_SECS: i64 = 3600;

/// 清理结果汇总。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// 本次删除的行数。
    pub deleted: u64,
    /// 清理后表内剩余行数（含刚过期未到宽限期的行）。
    pub remaining: i64,
}

impl CleanupReport {
    /// 生成一行可读摘要（写日志用）。
    ///
    /// # 返回值
    ///
    /// 形如 `token_blacklist_cleanup: deleted=12 remaining=4301`。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: deleted={} remaining={}",
            self.deleted, self.remaining
        )
    }
}

/// 在调度器上注册本任务（由 `task::scheduler` 的内置任务注册处调用）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
pub fn register_builtin(scheduler: &Scheduler) {
    // runner 必须返回 JoinHandle：框架调度器每次触发都会接管该句柄
    scheduler.register_cron(
        TASK_NAME,
        TASK_CRON,
        "系统任务：令牌吊销表清理",
        || {
            tokio::spawn(async move {
                match run_once().await {
                    Ok(report) => tracing::info!(summary = %report.summary(), "令牌吊销表清理完成"),
                    Err(error) => {
                        tracing::error!(error = %error, "令牌吊销表清理失败（表增长与点查计划会退化）")
                    }
                }
            })
        },
    );
}

/// 执行一次清理（可被单测与运维脚本直接调用）。
///
/// # 返回值
///
/// 删除行数与剩余行数。
///
/// # 异常情况
///
/// 数据库异常返回错误消息（任务体只记日志，不中断调度）。
///
/// SCOPE-EXEMPT: 后台定时任务是**全局卫生动作**（无请求者身份）：按到期时间删
/// 过期吊销行，注入数据范围既无意义（表里没有 tenant 维度）又会让清理漏做。
pub async fn run_once() -> std::result::Result<CleanupReport, String> {
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(GRACE_SECS);
    let deleted = db()
        .execute_with_params(
            "DELETE FROM sys_token_blacklist WHERE expire_at < ?::timestamptz",
            vec![Value::from(cutoff)],
        )
        .await
        .map_err(|error| format!("删除过期吊销行失败: {error}"))?;

    let remaining = crate::shared::token_revocation::row_count()
        .await
        .map_err(|error| format!("统计吊销行数失败: {error}"))?;

    // ADR §5.4 指标：未装 recorder 时 no-op（编译期 feature 未开也照样能编译）
    metrics::counter!("cloudpivot_token_blacklist_cleanup_deleted").increment(deleted);
    metrics::gauge!("cloudpivot_token_blacklist_rows").set(remaining as f64);

    Ok(CleanupReport { deleted, remaining })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 摘要格式稳定（运维 grep 依赖该格式）。
    #[test]
    fn cleanup_report_summary_format() {
        let report = CleanupReport {
            deleted: 12,
            remaining: 4301,
        };
        assert_eq!(
            report.summary(),
            "token_blacklist_cleanup: deleted=12 remaining=4301"
        );
    }

    /// 常量符合设计（防误改：宽限期必须是 1 小时，cron 必须与日志表错峰）。
    #[test]
    fn cleanup_constants_are_stable() {
        assert_eq!(TASK_NAME, "token_blacklist_cleanup");
        assert_eq!(TASK_CRON, "0 50 3 * * *");
        assert_eq!(GRACE_SECS, 3600);
    }
}

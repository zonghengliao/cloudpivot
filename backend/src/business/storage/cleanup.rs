//! 软删文件的对象回收任务（cron `0 30 4 * * *`，即每天 04:30）。
//!
//! # 为什么需要
//!
//! 删除文件只做 `sys_file.deleted_at = now()`（软删），**不立刻删物理对象**：
//! 秒传（同 `file_hash` 复用同一条记录、同一份对象）下，别人手里的 `file_id` 可能
//! 还指向那份对象；立刻删对象会让「记录在、文件没了」。因此物理回收延迟 7 天，
//! 且只在「该 hash 已无任何有效记录」时才删（架构文档 §2.8 第 4 点）。
//!
//! # 注册方式
//!
//! 由 [`crate::task::scheduler::Scheduler::register_builtin`] 在与既有 5 个日志清理任务
//! 同一处注册（框架调度器对注册表做**一次性快照**，必须在 `start()` 之前注册；
//! 放在 `bootstrap.rs` 末尾追加反而会被随后的 `reload()` 清空——这是实测踩到的点）。
//!
//! # 失败语义
//!
//! - 后端不存在（配置被删）：跳过并计数，不报错（运维在配置页能看到引用告警）；
//! - 后端不支持删除（A1 的 S3 后端）：跳过并计数（A2 交付 S3 服务端删除后自动生效）；
//! - 单条失败不中断整批，最终汇总一条日志。

use nik::prelude::*;

use crate::business::storage::{ensure_router, global_router};
use crate::task::Scheduler;

/// 调度任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "storage_object_cleanup";

/// cron 表达式：每天 04:30（与日志清理任务错峰）。
pub const TASK_CRON: &str = "0 30 4 * * *";

/// 软删记录保留天数（期间物理对象不删，保证秒传复用者不丢文件）。
pub const RETENTION_DAYS: i64 = 7;

/// 单次运行最多处理的对象数（防止一次扫描拖住数据库）。
pub const BATCH_LIMIT: i64 = 1000;

/// 回收结果汇总。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// 扫描到的候选记录数。
    pub scanned: usize,
    /// 成功删除的物理对象数。
    pub deleted: usize,
    /// 因后端缺失 / 不支持而跳过的数量。
    pub skipped: usize,
    /// 删除失败的数量。
    pub failed: usize,
}

impl CleanupReport {
    /// 生成一行可读摘要（写日志用）。
    ///
    /// # 返回值
    ///
    /// 形如 `storage_object_cleanup: scanned=3 deleted=3 skipped=0 failed=0`。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: scanned={} deleted={} skipped={} failed={}",
            self.scanned, self.deleted, self.skipped, self.failed
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
        "系统任务：存储对象回收",
        || {
            tokio::spawn(async {
                match run_once().await {
                    Ok(report) => tracing::info!(summary = %report.summary(), "存储对象回收完成"),
                    Err(error) => tracing::error!(error = %error, "存储对象回收失败"),
                }
            })
        },
    );
}

/// 执行一次回收（可被单测与运维脚本直接调用）。
///
/// # 返回值
///
/// 回收统计。
///
/// # 异常情况
///
/// 数据库异常返回错误消息；单条对象删除失败计入 `failed`，不中断整批。
///
/// SCOPE-EXEMPT: 孤儿文件回收是**后台定时任务**（无请求者身份）：筛「软删超保留期且
/// 无同哈希有效记录」的行做物理回收；按 `uploaded_by` 注入会让别人的孤儿文件
/// 永久留在对象存储里（磁盘泄漏），与用户数据权限无关。
pub async fn run_once() -> std::result::Result<CleanupReport, String> {
    ensure_router().await.map_err(|error| error.msg)?;

    let cutoff = chrono::Utc::now() - chrono::Duration::days(RETENTION_DAYS);
    let rows = db()
        .find_with_params(
            "SELECT f.storage_code, f.file_key FROM sys_file f \
             WHERE f.deleted_at IS NOT NULL AND f.deleted_at < ? \
               AND NOT EXISTS ( \
                     SELECT 1 FROM sys_file live \
                     WHERE live.file_hash = f.file_hash AND live.deleted_at IS NULL) \
             ORDER BY f.deleted_at ASC LIMIT ?",
            vec![Value::from(cutoff), Value::from(BATCH_LIMIT)],
        )
        .await
        .map_err(|error| format!("查询待回收文件失败: {error}"))?;

    let router = global_router();
    let default_code = router.default_code();
    let mut report = CleanupReport {
        scanned: rows.len(),
        ..CleanupReport::default()
    };

    for row in rows {
        let storage_code: Option<String> = row
            .get_typed("storage_code")
            .map_err(|error| format!("读取 storage_code 失败: {error}"))?;
        let file_key: String = row
            .get_typed("file_key")
            .map_err(|error| format!("读取 file_key 失败: {error}"))?;

        // storage_code 为 NULL 视为本地后端（历史数据），与路由规则一致
        let code = storage_code
            .as_deref()
            .map(str::trim)
            .filter(|code| !code.is_empty())
            .map(str::to_string)
            .or_else(|| default_code.clone());

        let Some(code) = code else {
            report.skipped += 1;
            continue;
        };
        let Some(store) = router.by_code(&code) else {
            tracing::warn!(config_code = %code, file_key = %file_key, "存储后端缺失，跳过对象回收");
            report.skipped += 1;
            continue;
        };

        match store.delete(&file_key).await {
            Ok(()) => report.deleted += 1,
            Err(crate::business::storage::store::StoreError::Unsupported(_)) => {
                report.skipped += 1;
            }
            Err(error) => {
                tracing::warn!(
                    config_code = %code,
                    file_key = %file_key,
                    error = %error,
                    "对象回收失败"
                );
                report.failed += 1;
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 报告摘要格式稳定（运维 grep 依赖该格式）。
    #[test]
    fn cleanup_report_summary_format() {
        let report = CleanupReport {
            scanned: 5,
            deleted: 3,
            skipped: 1,
            failed: 1,
        };
        assert_eq!(
            report.summary(),
            "storage_object_cleanup: scanned=5 deleted=3 skipped=1 failed=1"
        );
    }

    /// 保留期与 cron 常量符合设计（防误改）。
    #[test]
    fn cleanup_constants_are_stable() {
        assert_eq!(RETENTION_DAYS, 7);
        assert_eq!(TASK_CRON, "0 30 4 * * *");
        assert_eq!(TASK_NAME, "storage_object_cleanup");
    }
}

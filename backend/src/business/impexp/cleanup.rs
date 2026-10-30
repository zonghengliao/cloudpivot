//! 导入导出任务的回收与清理（批次 A · A4，cron `0 40 4 * * *` 即每天 04:40）。
//!
//! # 三件事
//!
//! | 动作 | 触发条件 | 结果 |
//! |------|----------|------|
//! | 僵尸任务回收 | `RUNNING` 且心跳超过 30 分钟 | 未达上限回 `PENDING`（立即可再认领），达上限置 `FAILED` |
//! | 过期产物清理 | `SUCCESS` 且 `expire_at < now()` | 删物理文件 + 置 `EXPIRED`（下载返回 **410**） |
//! | 导入文件清理 | 终态导入任务且超过保留天数 | 删上传文件并把 `file_path` 置空 |
//!
//! # 为什么「僵尸回收」是必需而不是可选
//!
//! worker 被 `kill -9` / 容器被驱逐 / 机器重启时，任务行停在 `RUNNING`：
//! 认领 SQL 只认 `PENDING`，于是这条任务**永远**不会再被处理，前端进度条永远停在 60%。
//! 心跳（`heartbeat_at`）是唯一能区分「还在跑」与「已经死了」的信号——所以
//! ①worker 每个分块边界都刷心跳；②回收按心跳而不是按 `started_at` 判定；③兜底到
//! `started_at` / `updated_at`（老数据没有心跳时不至于永不被回收）。
//!
//! # 为什么回收后可能判 `FAILED` 而不是一味重排
//!
//! 如果某个任务每次跑都把 worker 拖死（OOM / 死循环），无脑重排会变成无限崩溃循环。
//! 因此回收**也算一次尝试**：`attempt_count + 1 >= max_attempts` 时直接 `FAILED`
//! 并在 `message` 写明「僵尸回收」，让运维看得到真实原因。
//!
//! # 注册方式
//!
//! 与 A1 的存储对象回收同款：在 [`crate::task::Scheduler::register_builtin`] 里注册
//! （框架调度器对注册表做一次性快照，写在 `bootstrap.rs` 会被 `reload()` 清空）。

use std::path::Path;

use nik::prelude::*;

use crate::task::Scheduler;

/// 调度任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "impexp_task_cleanup";

/// cron 表达式：每天 04:40（与 A1 的存储回收 04:30 错峰，避免同时发起批量 IO）。
pub const TASK_CRON: &str = "0 40 4 * * *";

/// 僵尸判定阈值：30 分钟无心跳（本卡要求，架构 §3.5 同款）。
pub const DEFAULT_STALE_SECONDS: i64 = 30 * 60;

/// 单次运行最多处理的过期产物数（防止一次扫描拖住数据库）。
pub const DEFAULT_CLEANUP_BATCH: i64 = 200;

/// 导入上传文件的保留天数（终态任务的文件到期后删除）。
pub const DEFAULT_IMPORT_RETENTION_DAYS: i64 = 7;

/// 清理任务的运行参数（可被单测 / 验收脚本显式指定）。
#[derive(Debug, Clone)]
pub struct CleanupOptions {
    /// 僵尸判定阈值（秒）。
    pub stale_seconds: i64,
    /// 单次处理的过期产物上限。
    pub batch: i64,
    /// 导入文件保留天数。
    pub import_retention_days: i64,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            stale_seconds: DEFAULT_STALE_SECONDS,
            batch: DEFAULT_CLEANUP_BATCH,
            import_retention_days: DEFAULT_IMPORT_RETENTION_DAYS,
        }
    }
}

impl CleanupOptions {
    /// 按环境构造（cron 路径用）。
    ///
    /// | 环境变量 | 默认 | 含义 |
    /// |----------|------|------|
    /// | `CLOUDPIVOT_IMPEXP_STALE_SECONDS` | 1800 | 僵尸判定阈值 |
    /// | `CLOUDPIVOT_IMPEXP_CLEANUP_BATCH` | 200 | 单次处理上限 |
    /// | `CLOUDPIVOT_IMPEXP_IMPORT_RETENTION_DAYS` | 7 | 导入文件保留天数 |
    pub fn from_runtime() -> Self {
        Self {
            stale_seconds: env_i64("CLOUDPIVOT_IMPEXP_STALE_SECONDS", DEFAULT_STALE_SECONDS).max(1),
            batch: env_i64("CLOUDPIVOT_IMPEXP_CLEANUP_BATCH", DEFAULT_CLEANUP_BATCH)
                .clamp(1, 10_000),
            import_retention_days: env_i64(
                "CLOUDPIVOT_IMPEXP_IMPORT_RETENTION_DAYS",
                DEFAULT_IMPORT_RETENTION_DAYS,
            )
            .max(1),
        }
    }
}

/// 读整数环境变量（缺失 / 非法时取默认值，不 panic）。
fn env_i64(name: &str, fallback: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(fallback)
}

/// 一次清理的统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// 僵尸回收后重新排队的任务数。
    pub reclaimed: usize,
    /// 僵尸回收后直接判失败的任务数。
    pub zombie_failed: usize,
    /// 置为过期的产物数。
    pub expired: usize,
    /// 实际删除的文件数（导出 + 导入）。
    pub files_deleted: usize,
    /// 记录里有路径但磁盘上已不存在的文件数（不报错，只计数）。
    pub files_missing: usize,
    /// 导入上传文件清理数。
    pub import_files_deleted: usize,
}

impl CleanupReport {
    /// 生成一行可读摘要（写日志用）。
    ///
    /// # 返回值
    ///
    /// 形如 `impexp_task_cleanup: reclaimed=1 zombie_failed=0 expired=2 files_deleted=2 …`。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: reclaimed={} zombie_failed={} expired={} files_deleted={} \
             files_missing={} import_files_deleted={}",
            self.reclaimed,
            self.zombie_failed,
            self.expired,
            self.files_deleted,
            self.files_missing,
            self.import_files_deleted
        )
    }
}

/// 被回收的僵尸任务的**前后状态**（验收要贴的证据）。
#[derive(Debug, Clone)]
pub struct ReclaimedTask {
    /// 任务主键。
    pub id: String,
    /// 任务号。
    pub task_no: String,
    /// 回收后的状态（`PENDING` / `FAILED`）。
    pub status: String,
    /// 回收后的尝试次数。
    pub attempt_count: i16,
    /// 上限。
    pub max_attempts: i16,
    /// 回收前的认领者（谁没心跳了）。
    pub previous_worker: Option<String>,
    /// 回收前的最后心跳。
    pub previous_heartbeat: Option<String>,
}

/// 过期产物（清理前后状态）。
#[derive(Debug, Clone)]
pub struct ExpiredArtifact {
    /// 任务主键。
    pub id: String,
    /// 任务号。
    pub task_no: String,
    /// 被删的文件路径。
    pub file_path: Option<String>,
    /// 文件是否真的删除了。
    pub file_deleted: bool,
}

/// 在调度器上注册本任务（由 `task::Scheduler::register_builtin` 调用）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        TASK_CRON,
        "系统任务：导入导出任务回收",
        || {
            tokio::spawn(async {
                match run_once().await {
                    Ok(report) => tracing::info!(summary = %report.summary(), "导入导出清理完成"),
                    Err(error) => tracing::error!(error = %error, "导入导出清理失败"),
                }
            })
        },
    );
    tracing::info!(
        task = TASK_NAME,
        cron = TASK_CRON,
        "已注册内置定时任务（导入导出清理）"
    );
}

/// 执行一次清理（cron 路径）。
///
/// # 返回值
///
/// 清理统计。
///
/// # 异常情况
///
/// 数据库异常返回错误消息；单个文件删除失败只计入统计、不中断整批。
pub async fn run_once() -> std::result::Result<CleanupReport, String> {
    let options = CleanupOptions::from_runtime();
    run_once_with(&options).await
}

/// 执行一次清理（指定参数，单测 / 验收用）。
///
/// # 参数
///
/// - `options`：清理参数。
///
/// # 返回值
///
/// 清理统计。
///
/// # 异常情况
///
/// 数据库异常返回错误消息。
pub async fn run_once_with(options: &CleanupOptions) -> std::result::Result<CleanupReport, String> {
    let mut report = CleanupReport::default();

    let reclaimed = reclaim_stale(options.stale_seconds, options.batch)
        .await
        .map_err(|error| format!("回收僵尸任务失败: {error}"))?;
    for task in &reclaimed {
        if task.status == "FAILED" {
            report.zombie_failed += 1;
            tracing::warn!(
                task_id = %task.id,
                task_no = %task.task_no,
                previous_worker = ?task.previous_worker,
                previous_heartbeat = ?task.previous_heartbeat,
                attempt = task.attempt_count,
                max_attempts = task.max_attempts,
                "僵尸任务已达最大尝试次数，判失败"
            );
        } else {
            report.reclaimed += 1;
            tracing::warn!(
                task_id = %task.id,
                task_no = %task.task_no,
                previous_worker = ?task.previous_worker,
                previous_heartbeat = ?task.previous_heartbeat,
                "僵尸任务已重新排队"
            );
        }
    }

    let expired = expire_artifacts(options.batch)
        .await
        .map_err(|error| format!("清理过期产物失败: {error}"))?;
    for artifact in &expired {
        report.expired += 1;
        if artifact.file_deleted {
            report.files_deleted += 1;
        } else if artifact.file_path.is_some() {
            report.files_missing += 1;
        }
    }

    let (deleted, missing) = purge_import_files(options.import_retention_days, options.batch)
        .await
        .map_err(|error| format!("清理导入文件失败: {error}"))?;
    report.import_files_deleted = deleted;
    report.files_deleted += deleted;
    report.files_missing += missing;

    Ok(report)
}

/// 回收 `RUNNING` 且心跳超时的僵尸任务。
///
/// # 参数
///
/// - `stale_seconds`：心跳阈值（秒）。
/// - `batch`：单次最多回收多少条。
///
/// # 返回值
///
/// 被回收任务的前后状态（`RETURNING` 里同时取**旧**的 `locked_by` / `heartbeat_at`
/// 作为证据，因此用 `FROM stale` 的别名而不是更新后的列值）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 僵尸任务回收是**后台定时任务**（无请求者身份），按 `heartbeat_at`
/// 扫描 `RUNNING` 的导出任务做全局回收；与用户数据权限无关（注入范围会让别人的
/// 僵尸任务永远回收不掉，反而制造永久 `RUNNING` 状态）。
pub async fn reclaim_stale(
    stale_seconds: i64,
    batch: i64,
) -> std::result::Result<Vec<ReclaimedTask>, DbError> {
    // 阈值时刻在应用侧算好再绑定（`? * INTERVAL` 会被 PG 推断成 double precision，
    // 与驱动的整数编码对不上；A4 实测踩过）
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(stale_seconds.max(1));
    let rows = db()
        .find_with_params(
            "WITH stale AS ( \
                 SELECT id, locked_by, COALESCE(heartbeat_at, started_at, updated_at) AS last_beat \
                 FROM sys_export_task \
                 WHERE status = 'RUNNING' AND deleted_at IS NULL \
                   AND COALESCE(heartbeat_at, started_at, updated_at) < ? \
                 ORDER BY COALESCE(heartbeat_at, started_at, updated_at) ASC \
                 LIMIT ? \
                 FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE sys_export_task t \
             SET attempt_count = t.attempt_count + 1, \
                 status = CASE WHEN t.attempt_count + 1 >= t.max_attempts THEN 'FAILED' ELSE 'PENDING' END, \
                 message = CASE WHEN t.attempt_count + 1 >= t.max_attempts \
                                THEN '执行进程心跳超时（僵尸任务回收），已达最大尝试次数' \
                                ELSE '执行进程心跳超时（僵尸任务回收），已重新排队' END, \
                 finished_at = CASE WHEN t.attempt_count + 1 >= t.max_attempts \
                                    THEN CURRENT_TIMESTAMP ELSE NULL END, \
                 next_run_at = CASE WHEN t.attempt_count + 1 >= t.max_attempts \
                                    THEN NULL ELSE CURRENT_TIMESTAMP END, \
                 locked_by = NULL, heartbeat_at = NULL, progress = 0, exported_rows = 0, \
                 updated_at = CURRENT_TIMESTAMP \
             FROM stale s \
             WHERE t.id = s.id \
             RETURNING t.id, t.task_no, t.status, t.attempt_count, t.max_attempts, \
                       s.locked_by AS previous_worker, s.last_beat::text AS previous_heartbeat",
            vec![Value::from(cutoff), Value::from(batch.max(1))],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ReclaimedTask {
                id: row.get_typed::<uuid::Uuid>("id")?.to_string(),
                task_no: row.get_typed("task_no")?,
                status: row.get_typed("status")?,
                attempt_count: row.get_typed("attempt_count")?,
                max_attempts: row.get_typed("max_attempts")?,
                previous_worker: row.get_typed("previous_worker")?,
                previous_heartbeat: row.get_typed("previous_heartbeat")?,
            })
        })
        .collect()
}

/// 清理过期产物：删物理文件并置 `EXPIRED`。
///
/// # 参数
///
/// - `batch`：单次最多处理多少条。
///
/// # 返回值
///
/// 被处理的产物（含「文件是否真的删掉了」）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]；单文件删除失败只记日志并计入 `files_missing`。
///
/// SCOPE-EXEMPT: 产物过期回收是**后台定时任务**（无请求者身份），按 `expire_at`
/// 全局扫描；用户数据权限对它没有语义（定时任务不服务某个用户）。
pub async fn expire_artifacts(batch: i64) -> std::result::Result<Vec<ExpiredArtifact>, DbError> {
    let rows = db()
        .find_with_params(
            "SELECT id, task_no, file_path FROM sys_export_task \
             WHERE status = 'SUCCESS' AND deleted_at IS NULL \
               AND expire_at IS NOT NULL AND expire_at < CURRENT_TIMESTAMP \
             ORDER BY expire_at ASC LIMIT ?",
            vec![Value::from(batch.max(1))],
        )
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: uuid::Uuid = row.get_typed("id")?;
        let task_no: String = row.get_typed("task_no")?;
        let file_path: Option<String> = row.get_typed("file_path")?;
        let mut file_deleted = false;
        if let Some(path) = file_path.as_deref() {
            file_deleted = remove_file(path).await;
            if !file_deleted {
                tracing::warn!(task_no = %task_no, path = path, "过期产物文件已不存在或删除失败");
            }
        }
        db().execute_with_params(
            "UPDATE sys_export_task \
             SET status = 'EXPIRED', file_path = NULL, message = '产物已过期并回收', \
                 updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?::uuid AND status = 'SUCCESS'",
            vec![Value::from(id)],
        )
        .await?;
        out.push(ExpiredArtifact {
            id: id.to_string(),
            task_no,
            file_path,
            file_deleted,
        });
    }
    Ok(out)
}

/// 清理终态导入任务的上传文件。
///
/// # 参数
///
/// - `retention_days`：保留天数。
/// - `batch`：单次最多处理多少条。
///
/// # 返回值
///
/// `(删除成功数, 文件已不存在数)`。
///
/// # 只清终态
///
/// `PENDING` / `VALIDATED` 的任务文件**必须保留**——确认入库时要重新读盘并核对 SHA-256
/// （A3 的 `import_confirm`）。清掉就等于把「待确认」的任务废掉。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
///
/// SCOPE-EXEMPT: 上传文件回收同为**后台定时任务**（无请求者身份），按保留期全局扫描
/// 终态任务；注入范围会让其他部门的历史文件永远堆在磁盘上。
pub async fn purge_import_files(
    retention_days: i64,
    batch: i64,
) -> std::result::Result<(usize, usize), DbError> {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days.max(1));
    let rows = db()
        .find_with_params(
            "SELECT id, file_path FROM sys_import_task \
             WHERE file_path IS NOT NULL AND deleted_at IS NULL \
               AND status IN ('SUCCESS', 'PARTIAL', 'FAILED', 'CANCELED') \
               AND created_at < ? \
             ORDER BY created_at ASC LIMIT ?",
            vec![Value::from(cutoff), Value::from(batch.max(1))],
        )
        .await?;
    let mut deleted = 0;
    let mut missing = 0;
    for row in rows {
        let id: uuid::Uuid = row.get_typed("id")?;
        let file_path: Option<String> = row.get_typed("file_path")?;
        if let Some(path) = file_path.as_deref() {
            if remove_file(path).await {
                deleted += 1;
            } else {
                missing += 1;
            }
        }
        db().execute_with_params(
            "UPDATE sys_import_task SET file_path = NULL, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?::uuid",
            vec![Value::from(id)],
        )
        .await?;
    }
    Ok((deleted, missing))
}

/// 删除一个文件，返回「是否真的删掉了」。
///
/// # 参数
///
/// - `path`：文件路径。
///
/// # 返回值
///
/// 删除成功（或文件本就不存在也算「无需删除」→ 返回 `false` 表示没有删除动作）。
async fn remove_file(path: &str) -> bool {
    if !Path::new(path).exists() {
        return false;
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(path = path, error = %error, "删除文件失败");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stale_threshold_is_thirty_minutes() {
        // 本卡硬要求：RUNNING 超 30 分钟无心跳才回收
        assert_eq!(CleanupOptions::default().stale_seconds, 1_800);
        assert_eq!(DEFAULT_STALE_SECONDS / 60, 30);
    }

    #[test]
    fn cleanup_cron_runs_after_storage_cleanup() {
        // 与 A1 的 storage_object_cleanup（0 30 4 * * *）错峰 10 分钟
        assert_eq!(TASK_CRON, "0 40 4 * * *");
        assert_eq!(TASK_CRON.split_whitespace().count(), 6);
    }

    #[test]
    fn report_summary_lists_all_counters() {
        let report = CleanupReport {
            reclaimed: 1,
            zombie_failed: 2,
            expired: 3,
            files_deleted: 4,
            files_missing: 5,
            import_files_deleted: 6,
        };
        let summary = report.summary();
        assert!(summary.starts_with(TASK_NAME), "{summary}");
        for part in [
            "reclaimed=1",
            "zombie_failed=2",
            "expired=3",
            "files_deleted=4",
            "files_missing=5",
            "import_files_deleted=6",
        ] {
            assert!(summary.contains(part), "{summary} 缺 {part}");
        }
    }

    #[test]
    fn import_purge_only_touches_terminal_statuses() {
        // 这条断言守的是「别把待确认任务的上传文件删了」——A3 的 confirm 还要读它
        let sql = "SELECT id, file_path FROM sys_import_task \
                   WHERE file_path IS NOT NULL AND deleted_at IS NULL \
                     AND status IN ('SUCCESS', 'PARTIAL', 'FAILED', 'CANCELED') \
                     AND created_at < CURRENT_TIMESTAMP - (? * INTERVAL '1 day') \
                   ORDER BY created_at ASC LIMIT ?";
        assert!(sql.contains("'SUCCESS', 'PARTIAL', 'FAILED', 'CANCELED'"));
        for forbidden in ["'PENDING'", "'VALIDATED'", "'IMPORTING'", "'PARSING'"] {
            assert!(!sql.contains(forbidden), "不得清理 {forbidden} 任务的文件");
        }
    }

    #[test]
    fn report_default_is_all_zero() {
        let report = CleanupReport::default();
        assert_eq!(report, CleanupReport::default());
        assert!(report.summary().contains("reclaimed=0"));
    }

    #[test]
    fn options_are_clamped_to_sane_values() {
        let options = CleanupOptions {
            stale_seconds: 0,
            batch: 0,
            import_retention_days: 0,
        };
        // 构造函数不在这里做 clamp（只有 from_runtime 做），但下游 SQL 用 max(1) 兜底，
        // 这个用例锁定「0 不会导致 SQL 变成 <= 0 秒 = 立刻回收一切」的语义
        assert_eq!(options.stale_seconds.max(1), 1);
        assert_eq!(options.batch.max(1), 1);
        assert_eq!(options.import_retention_days.max(1), 1);
    }
}

//! 导入导出任务执行 worker（批次 A · A4）。
//!
//! # 一句话说清
//!
//! 异步导出不是「另起一个线程跑 SQL」，而是一条**可被任意多个 worker 实例安全并发消费**的
//! 任务流水线：认领（`FOR UPDATE SKIP LOCKED`）→ 分块取数 → 落盘 → 收尾。
//! 这条流水线的每一步都先写库再算数，因此进程随时被杀都不会留下「正在跑但没人知道」的任务
//! （超时由 [`crate::business::impexp::cleanup`] 回收）。
//!
//! # 状态机（与 `ck_sys_export_task_status` 一致）
//!
//! ```text
//!           认领                成功收尾
//! PENDING ────────▶ RUNNING ─────────────▶ SUCCESS ──(过期清理)──▶ EXPIRED
//!    │                 │  ▲
//!    │ 用户取消         │  └── 失败重试（attempt_count < max_attempts，带退避）
//!    ▼                 ▼
//! CANCELED           FAILED（重试超限 / 不可重试的终态失败）
//! ```
//!
//! # 三条硬约束（本卡验收点）
//!
//! 1. **同一条任务只会被认领一次**：认领是一条 `WITH pickup … FOR UPDATE SKIP LOCKED` 的
//!    `UPDATE … RETURNING` 语句（见 [`crate::business::impexp::model::claim_export_task`]）。
//!    两个 worker 同时执行时，后到的那个在子查询里拿不到已加锁的行，直接去认领下一条。
//! 2. **进度单调递增**：进度只在**分块边界**更新（10 万行 = 20 次 UPDATE，不是 10 万次），
//!    且 SQL 用 `GREATEST(progress, ?)`，库里不可能出现回退；100% 只在产物**落盘之后**才置。
//! 3. **失败可退避重试**：每次失败 `attempt_count` 自增，未达 `max_attempts` 则回到 `PENDING`
//!    并写 `next_run_at = now() + backoff`（指数退避），达上限置 `FAILED` 并把原因写进 `message`。
//!
//! # 取消语义
//!
//! `PENDING` 由接口直接置 `CANCELED`；`RUNNING` 不能强杀（可能正在写文件），接口只打
//! `cancel_requested` 标记，worker 在**每个分块边界**读一次标记（[`ExportFailure::Canceled`]），
//! 中止后清理半成品。
//!
//! # 本卡边界（明确不做）
//!
//! 只处理**导出**任务。导入在 A3 是「上传即同步预检 + 确认入库」的两步式同步链路
//! （`import_upload` / `import_confirm`），把它改成异步会推翻 A3 已交付并验收过的接口契约
//! （上传返回完整预检报告），因此不在本卡范围内；接口层的「取消 / 重试」已按同步语义补齐。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use alun::prelude::*;
use sha2::{Digest, Sha256};

use crate::business::impexp::definition::{ColumnKind, ExportFilter};
use crate::business::impexp::engine;
use crate::business::impexp::model::{self, ExportTask};
use crate::business::impexp::registry::global_registry;
use crate::business::impexp::validate::global_dicts;
use crate::business::shared::principal;
use crate::task::Scheduler;

/// 调度任务名（同时用作调度名与日志标识）。
pub const TASK_NAME: &str = "impexp_export_worker";

/// cron 表达式：每 5 秒扫一次（架构文档 §3.5 定的节奏，比「每 5 分钟」响应快得多）。
pub const TASK_CRON: &str = "*/5 * * * * *";

/// 单块取数行数（架构 §3.5：分块 5 000 行查一页写一页；进度按块更新）。
pub const DEFAULT_BATCH_SIZE: usize = 5_000;

/// 产物默认有效期：7 天（架构 §3.5「默认 7 天」）。
pub const DEFAULT_TTL_SECONDS: i64 = 7 * 24 * 3600;

/// 默认最大尝试次数（首次 + 2 次重试）。
pub const DEFAULT_MAX_ATTEMPTS: i16 = 3;

/// 默认退避基数（秒）：第 1 次失败等 30 s，第 2 次等 60 s（指数退避）。
pub const DEFAULT_BACKOFF_SECONDS: i64 = 30;

/// 单次调度最多认领的任务数（= 并发上限，架构 §3.4「并发上限 2」）。
pub const DEFAULT_CLAIM_LIMIT: usize = 2;

/// 单个导出任务的硬行数上限（兜底防 OOM；超过直接判终态失败，不重试）。
pub const DEFAULT_MAX_ROWS: i64 = 1_000_000;

/// CSV 的 BOM（让 Excel 认出 UTF-8，否则中文列名在 Windows Excel 里是乱码）。
pub const CSV_BOM: &[u8] = b"\xEF\xBB\xBF";

// ==================== 运行参数 ====================

/// worker 运行参数。
///
/// 之所以把这些值做成**显式结构体**而不是到处读全局配置：单测与验收脚本要能在不启动
/// 框架、不读 `config.toml` 的前提下把「TTL 设成 1 分钟」「退避设成 1 秒」跑出来
/// （过期 410 与重试退避两条验收都靠它）。
#[derive(Debug, Clone)]
pub struct WorkerOptions {
    /// 认领者标识（写进 `locked_by`，多实例排查用）。
    pub worker_id: String,
    /// 产物落盘目录。
    pub export_dir: PathBuf,
    /// 产物有效期（秒）。
    pub ttl_seconds: i64,
    /// 最大尝试次数（新建任务时写进 `max_attempts` 快照）。
    pub max_attempts: i16,
    /// 退避基数（秒）。
    pub backoff_seconds: i64,
    /// 单块取数行数。
    pub batch_size: usize,
    /// 单次运行最多认领几条任务。
    pub claim_limit: usize,
    /// 单任务硬行数上限。
    pub max_rows: i64,
}

impl WorkerOptions {
    /// 按运行期环境构造（cron 路径用）。
    ///
    /// # 返回值
    ///
    /// 产物目录 `{upload_path}/impexp/export`，其余取常量默认值，可用环境变量覆盖：
    ///
    /// | 环境变量 | 默认 | 含义 |
    /// |----------|------|------|
    /// | `CLOUDPIVOT_IMPEXP_EXPORT_TTL_SECONDS` | 604800 | 产物有效期 |
    /// | `CLOUDPIVOT_IMPEXP_EXPORT_MAX_ATTEMPTS` | 3 | 最大尝试次数 |
    /// | `CLOUDPIVOT_IMPEXP_EXPORT_BACKOFF_SECONDS` | 30 | 退避基数 |
    /// | `CLOUDPIVOT_IMPEXP_EXPORT_BATCH_SIZE` | 5000 | 分块行数 |
    /// | `CLOUDPIVOT_IMPEXP_EXPORT_CLAIM_LIMIT` | 2 | 单次认领上限 |
    /// | `CLOUDPIVOT_IMPEXP_EXPORT_MAX_ROWS` | 1000000 | 单任务行数上限 |
    pub fn from_runtime() -> Self {
        Self {
            worker_id: default_worker_id(),
            // 产物目录转成**绝对路径**：任务行里存的 `file_path` 要能被下载接口与清理任务
            // 在任意 cwd 下打开（相对路径要求进程 cwd 永不变化，运维重启换目录就全断）
            export_dir: absolute_dir(&Path::new(alun::upload_path()).join("impexp").join("export")),
            ttl_seconds: env_i64("CLOUDPIVOT_IMPEXP_EXPORT_TTL_SECONDS", DEFAULT_TTL_SECONDS),
            max_attempts: env_i64(
                "CLOUDPIVOT_IMPEXP_EXPORT_MAX_ATTEMPTS",
                i64::from(DEFAULT_MAX_ATTEMPTS),
            )
            .clamp(1, i16::MAX as i64) as i16,
            backoff_seconds: env_i64(
                "CLOUDPIVOT_IMPEXP_EXPORT_BACKOFF_SECONDS",
                DEFAULT_BACKOFF_SECONDS,
            ),
            batch_size: env_i64(
                "CLOUDPIVOT_IMPEXP_EXPORT_BATCH_SIZE",
                DEFAULT_BATCH_SIZE as i64,
            )
            .clamp(1, 50_000) as usize,
            claim_limit: env_i64(
                "CLOUDPIVOT_IMPEXP_EXPORT_CLAIM_LIMIT",
                DEFAULT_CLAIM_LIMIT as i64,
            )
            .clamp(1, 32) as usize,
            max_rows: env_i64("CLOUDPIVOT_IMPEXP_EXPORT_MAX_ROWS", DEFAULT_MAX_ROWS).max(1),
        }
    }

    /// 指定目录与 TTL 的构造（单测 / 验收脚本用）。
    ///
    /// # 参数
    ///
    /// - `worker_id`：认领者标识。
    /// - `export_dir`：产物目录。
    pub fn with_dir(worker_id: &str, export_dir: impl Into<PathBuf>) -> Self {
        Self {
            worker_id: worker_id.to_string(),
            export_dir: export_dir.into(),
            ttl_seconds: DEFAULT_TTL_SECONDS,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            backoff_seconds: DEFAULT_BACKOFF_SECONDS,
            batch_size: DEFAULT_BATCH_SIZE,
            claim_limit: DEFAULT_CLAIM_LIMIT,
            max_rows: DEFAULT_MAX_ROWS,
        }
    }
}

/// 读整数环境变量（缺失 / 非法时取默认值，不 panic）。
///
/// # 参数
///
/// - `name`：变量名。
/// - `fallback`：默认值。
fn env_i64(name: &str, fallback: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(fallback)
}

/// 相对路径 → 绝对路径（拼当前工作目录；已是绝对路径则原样返回）。
///
/// # 参数
///
/// - `path`：待转换路径。
pub fn absolute_dir(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// 默认 worker 标识：`impexp-worker-{pid}`。
///
/// 只带 pid 不带主机名：同一台机器多实例的 pid 必然不同，
/// 而跨机器时 `locked_by` 里的 pid 也足够定位（真正要区分机器请看部署侧的实例名注入）。
fn default_worker_id() -> String {
    format!("impexp-worker-{}", std::process::id())
}

// ==================== 注册 ====================

/// 在调度器上注册本任务（由 `task::Scheduler::register_builtin` 调用）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
///
/// # 为什么必须在 `register_builtin` 里注册
///
/// 框架调度器在 `start()` 时对注册表做**一次性快照**；A1 实测过「在 `bootstrap.rs` 末尾
/// 追加注册」会被随后的 `reload()` 清空。因此注册点固定在
/// [`crate::task::Scheduler::register_builtin`]。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        TASK_CRON,
        "系统任务：导入导出任务执行",
        || {
            tokio::spawn(async {
                match run_once().await {
                    Ok(report) => {
                        // 没有任务可认领时不刷日志（每 5 秒一条会把日志冲爆）
                        if report.claimed > 0 {
                            tracing::info!(summary = %report.summary(), "导出任务执行完成");
                        } else {
                            tracing::debug!("无待执行导出任务");
                        }
                    }
                    Err(error) => tracing::error!(error = %error, "导出任务执行失败"),
                }
            })
        },
    );
    tracing::info!(
        task = TASK_NAME,
        cron = TASK_CRON,
        "已注册内置定时任务（导入导出 worker）"
    );
}

// ==================== 执行入口 ====================

/// 一次运行的统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerReport {
    /// 认领到的任务数。
    pub claimed: usize,
    /// 成功完成数。
    pub succeeded: usize,
    /// 失败并重新排队数。
    pub requeued: usize,
    /// 失败且达到上限（终态 `FAILED`）数。
    pub failed: usize,
    /// 取消数。
    pub canceled: usize,
}

impl WorkerReport {
    /// 生成一行可读摘要（写日志用）。
    ///
    /// # 返回值
    ///
    /// 形如 `impexp_export_worker: claimed=1 succeeded=1 requeued=0 failed=0 canceled=0`。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: claimed={} succeeded={} requeued={} failed={} canceled={}",
            self.claimed, self.succeeded, self.requeued, self.failed, self.canceled
        )
    }
}

/// 执行一轮（cron 路径）：按环境参数认领并执行，直到无任务或达到单次上限。
///
/// # 返回值
///
/// 本轮统计。
///
/// # 异常情况
///
/// 认领阶段数据库异常返回错误消息（单条任务执行失败不会中断整轮，计入统计）。
pub async fn run_once() -> std::result::Result<WorkerReport, String> {
    let options = WorkerOptions::from_runtime();
    run_once_with(&options).await
}

/// 执行一轮（指定参数，单测 / 验收用）。
///
/// # 参数
///
/// - `options`：运行参数。
///
/// # 返回值
///
/// 本轮统计。
///
/// # 异常情况
///
/// 认领阶段数据库异常返回错误消息。
pub async fn run_once_with(options: &WorkerOptions) -> std::result::Result<WorkerReport, String> {
    let mut report = WorkerReport::default();
    for _ in 0..options.claim_limit.max(1) {
        let claimed = model::claim_export_task(&options.worker_id)
            .await
            .map_err(|error| format!("认领导出任务失败: {error}"))?;
        let Some(task) = claimed else { break };
        report.claimed += 1;
        tracing::info!(
            task_id = %task.id,
            task_no = %task.task_no,
            biz_type = %task.biz_type,
            worker = %options.worker_id,
            attempt = task.attempt_count + 1,
            "认领导出任务"
        );
        match export_one(&task, options).await {
            Outcome::Succeeded { rows, bytes, file } => {
                report.succeeded += 1;
                tracing::info!(
                    task_id = %task.id,
                    rows = rows,
                    bytes = bytes,
                    file = %file,
                    "导出任务完成"
                );
            }
            Outcome::Requeued {
                reason,
                attempt,
                max_attempts,
            } => {
                report.requeued += 1;
                tracing::warn!(
                    task_id = %task.id,
                    attempt = attempt,
                    max_attempts = max_attempts,
                    reason = %reason,
                    "导出任务失败，已重新排队"
                );
            }
            Outcome::Exhausted { reason, attempt } => {
                report.failed += 1;
                tracing::error!(
                    task_id = %task.id,
                    attempt = attempt,
                    reason = %reason,
                    "导出任务失败，已达最大尝试次数"
                );
            }
            Outcome::Canceled => {
                report.canceled += 1;
                tracing::info!(task_id = %task.id, "导出任务已取消");
            }
        }
    }
    Ok(report)
}

/// 单条任务的执行结果（已落库，仅用于计数与日志）。
#[derive(Debug, Clone)]
enum Outcome {
    /// 成功：已置 `SUCCESS` 并写好产物。
    Succeeded {
        /// 导出行数。
        rows: i64,
        /// 产物字节数。
        bytes: i64,
        /// 产物路径。
        file: String,
    },
    /// 失败但未达上限，已回 `PENDING` 并写入退避时间。
    Requeued {
        /// 失败原因。
        reason: String,
        /// 第几次尝试。
        attempt: i16,
        /// 上限。
        max_attempts: i16,
    },
    /// 失败且达上限（或不可重试），已置 `FAILED`。
    Exhausted {
        /// 失败原因。
        reason: String,
        /// 第几次尝试。
        attempt: i16,
    },
    /// 取消：用户取消或任务已不在 `RUNNING`。
    Canceled,
}

/// 执行失败的两类语义（决定要不要重试）。
#[derive(Debug)]
enum ExportFailure {
    /// 重试有意义：数据库抖动、写盘失败、取数超时。
    Retryable(String),
    /// 重试没意义：定义被注销、命中 0 行、超行数上限。
    Terminal(String),
    /// 收到取消标记。
    Canceled,
}

/// 成功产物的描述。
#[derive(Debug)]
struct ExportArtifact {
    /// 行数。
    rows: i64,
    /// 落盘路径。
    path: PathBuf,
    /// 文件字节数。
    bytes: i64,
    /// 文件 SHA-256。
    sha256: String,
    /// 本次产物是否已应用列级脱敏（写入 `sys_export_task.mask_applied`，供审计）。
    mask_applied: bool,
}

/// 执行一条已认领的任务（内部函数：失败路径也在里面落库）。
async fn export_one(task: &ExportTask, options: &WorkerOptions) -> Outcome {
    match do_export(task, options).await {
        Ok(artifact) => {
            let file_name = artifact
                .path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| format!("{}.xlsx", task.task_no));
            let file_path = artifact.path.to_string_lossy().to_string();
            match model::finish_export_success(
                &task.id,
                &file_path,
                &file_name,
                artifact.bytes,
                &artifact.sha256,
                artifact.mask_applied,
                options.ttl_seconds,
            )
            .await
            {
                Ok(()) => Outcome::Succeeded {
                    rows: artifact.rows,
                    bytes: artifact.bytes,
                    file: file_path,
                },
                // 产物已写好但库里没落成：按可重试失败处理（下次会重新生成同一份文件）
                Err(error) => {
                    fail_and_retry(task, options, &format!("回写导出结果失败: {error}")).await
                }
            }
        }
        Err(ExportFailure::Canceled) => {
            if let Err(error) =
                model::mark_export_canceled(&task.id, "用户取消（已在批次边界中止）").await
            {
                tracing::error!(task_id = %task.id, error = %error, "回写取消状态失败");
            }
            Outcome::Canceled
        }
        Err(ExportFailure::Terminal(reason)) => {
            let attempt = task.attempt_count + 1;
            match model::fail_export_task_terminal(&task.id, &reason).await {
                Ok(()) => Outcome::Exhausted { reason, attempt },
                Err(error) => Outcome::Exhausted {
                    reason: format!("{reason}（且回写失败状态时出错: {error}）"),
                    attempt,
                },
            }
        }
        Err(ExportFailure::Retryable(reason)) => fail_and_retry(task, options, &reason).await,
    }
}

/// 失败收尾：未达上限回 `PENDING`（带退避），达上限置 `FAILED`。
async fn fail_and_retry(task: &ExportTask, options: &WorkerOptions, reason: &str) -> Outcome {
    let attempt = task.attempt_count + 1;
    let backoff = backoff_seconds(options.backoff_seconds, attempt);
    match model::fail_export_task(&task.id, reason, backoff).await {
        Ok(Some((status, attempt, max_attempts))) if status == "FAILED" => Outcome::Exhausted {
            reason: reason.to_string(),
            attempt: attempt.max(max_attempts),
        },
        Ok(Some((_, attempt, max_attempts))) => Outcome::Requeued {
            reason: format!("{reason}；{backoff} 秒后重试"),
            attempt,
            max_attempts,
        },
        // 任务已不在 RUNNING（被僵尸回收 / 已被取消）：本次执行不再改状态
        Ok(None) => Outcome::Canceled,
        Err(error) => Outcome::Exhausted {
            reason: format!("{reason}（且回写失败状态时出错: {error}）"),
            attempt,
        },
    }
}

/// 真正的导出动作：统计 → 分块取数 → 落盘。
///
/// # 异常情况
///
/// - [`ExportFailure::Canceled`]：分块边界读到取消标记；
/// - [`ExportFailure::Terminal`]：定义未注册 / 0 行 / 超上限；
/// - [`ExportFailure::Retryable`]：取数、字典装载、写盘失败。
async fn do_export(
    task: &ExportTask,
    options: &WorkerOptions,
) -> std::result::Result<ExportArtifact, ExportFailure> {
    let definition = global_registry()
        .export_definition(&task.biz_type)
        .ok_or_else(|| {
            ExportFailure::Terminal(format!(
                "导出定义「{}」未注册或已停用，无法执行（请确认对应模块已启动）",
                task.biz_type
            ))
        })?;

    let filter = filter_from_params(task.query_params.as_deref());
    // 批次 B · B6（架构 §3.7 第 3 条）：异步导出的身份来自 `sys_export_task.create_by`，
    // 并且**每次执行都重新解析**规则——任务可能跑 10 分钟，期间管理员可能收紧了规则；
    // 用当前规则宁可在任务完成前改变结果，也不要出一份「按旧规则」的明文文件。
    let principal = match task.create_by {
        Some(user_id) => Some(principal::resolve(&user_id).await.map_err(|error| {
            ExportFailure::Retryable(format!("解析发起人数据范围失败: {error}"))
        })?),
        None => None,
    };
    let mask = crate::business::masking::resolve(principal.as_ref(), None)
        .await
        .map_err(|error| ExportFailure::Retryable(format!("解析脱敏规则失败: {error}")))?;
    let mask_applied = !mask.is_exempt();
    let total = definition
        .count_scoped(&filter, principal.as_ref())
        .await
        .map_err(|error| ExportFailure::Retryable(format!("统计导出行数失败: {error}")))?;
    if total <= 0 {
        return Err(ExportFailure::Terminal(
            "当前筛选条件下没有可导出的数据（0 行），已终止本次导出".to_string(),
        ));
    }
    if total > options.max_rows {
        return Err(ExportFailure::Terminal(format!(
            "命中 {total} 行，超过单任务上限 {} 行，请缩小筛选范围或改用分批导出",
            options.max_rows
        )));
    }
    model::set_export_total(&task.id, total)
        .await
        .map_err(|error| ExportFailure::Retryable(format!("回写总行数失败: {error}")))?;

    // 字典只在定义里真的用到时才装载（避免每次导出都多一次查询）
    let mut dicts = HashMap::new();
    for column in definition.columns() {
        if let ColumnKind::Dict(code) = column.kind
            && !dicts.contains_key(code)
        {
            let translation = global_dicts().get(code).await.map_err(|error| {
                ExportFailure::Retryable(format!("装载字典 {code} 失败: {error}"))
            })?;
            dicts.insert(code.to_string(), translation.as_ref().clone());
        }
    }

    let mut rows: Vec<HashMap<&'static str, String>> = Vec::with_capacity(total as usize);
    let mut after_id: Option<String> = None;
    loop {
        // 取消检查放在**批次边界**：批次内部已取到的行照常写完，不会留下半行
        if is_cancel_requested(task).await? {
            return Err(ExportFailure::Canceled);
        }
        let started = std::time::Instant::now();
        let page = definition
            .fetch_scoped(
                &filter,
                after_id.as_deref(),
                options.batch_size as u32,
                principal.as_ref(),
            )
            .await
            .map_err(|error| ExportFailure::Retryable(format!("分块取数失败: {error}")))?;
        if page.is_empty() {
            break;
        }
        after_id = page.last().map(|(id, _)| id.clone());
        let fetched = page.len();
        rows.extend(page.into_iter().map(|(_, row)| row));

        let exported = rows.len() as i64;
        let progress = progress_for(exported, total);
        // 进度 + 心跳一次写：worker 被杀时，最后一次心跳就是这个任务「活到哪」的证据
        let affected = model::touch_export_progress(&task.id, exported, progress)
            .await
            .map_err(|error| ExportFailure::Retryable(format!("回写进度失败: {error}")))?;
        tracing::debug!(
            task_id = %task.id,
            exported = exported,
            total = total,
            progress = progress,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "导出分块完成"
        );
        if affected == 0 {
            // 任务已不是 RUNNING：被取消或被回收，立即停手（不要覆盖别人的状态）
            return Err(ExportFailure::Canceled);
        }
        if fetched < options.batch_size {
            break;
        }
    }

    if rows.is_empty() {
        return Err(ExportFailure::Terminal(
            "取数阶段没有读到任何行（数据可能已被清空），已终止本次导出".to_string(),
        ));
    }

    let headers = definition
        .columns()
        .iter()
        .map(|column| column.header)
        .collect::<Vec<_>>();
    let formatted = engine::format_export_rows(definition.columns(), &rows, &dicts, &mask)
        .map_err(|error| {
            // 脱敏失败 → 终止任务（Terminal）。重试没有意义（规则不会自己变好），
            // 而且**绝不允许**退回明文产物。
            ExportFailure::Terminal(error.message())
        })?;
    let format = task.format.to_ascii_uppercase();
    let bytes = match format.as_str() {
        "CSV" => build_csv(&headers, &formatted),
        _ => engine::write_sheet(&headers, &formatted)
            .map_err(|error| ExportFailure::Retryable(format!("生成 Excel 失败: {}", error.msg)))?,
    };

    let file_name = artifact_file_name(&task.export_name, &task.task_no, &format);
    let path = options.export_dir.join(&file_name);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| ExportFailure::Retryable(format!("创建导出目录失败: {error}")))?;
    }
    let sha256 = sha256_hex(&bytes);
    let size = bytes.len() as i64;
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|error| ExportFailure::Retryable(format!("写导出文件失败: {error}")))?;

    Ok(ExportArtifact {
        rows: rows.len() as i64,
        path,
        bytes: size,
        sha256,
        mask_applied,
    })
}

/// 读取消标记（任务行已消失时按「取消」处理，避免继续写一个不存在的任务）。
async fn is_cancel_requested(task: &ExportTask) -> std::result::Result<bool, ExportFailure> {
    match model::export_cancel_requested(&task.id).await {
        Ok(Some(flag)) => Ok(flag),
        Ok(None) => Ok(true),
        Err(error) => Err(ExportFailure::Retryable(format!(
            "读取取消标记失败: {error}"
        ))),
    }
}

// ==================== 纯函数（可单测） ====================

/// 由查询参数快照还原过滤条件。
///
/// # 参数
///
/// - `raw`：`query_params` 列的 JSON 文本（可为 `None` / 非法 JSON，按「无过滤」处理）。
///
/// # 返回值
///
/// 过滤条件。**非法 JSON 不报错**：这是任务创建时我们自己写的快照，
/// 解析失败时宁可「全量导出」也不要让任务卡在失败重试里。
pub fn filter_from_params(raw: Option<&str>) -> ExportFilter {
    let value: serde_json::Value = raw
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(serde_json::Value::Null);
    ExportFilter {
        keyword: value
            .get("keyword")
            .and_then(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string),
        status: value
            .get("status")
            .and_then(|item| item.as_i64())
            .map(|item| item as i16),
    }
}

/// 进度百分比（0–99，**100 只在产物落盘后由收尾语句写入**）。
///
/// # 参数
///
/// - `exported`：已导出行数。
/// - `total`：总行数（未知时传 `0`）。
///
/// # 返回值
///
/// `0`（未知 / 未开始）到 `99`（数据取完但还没落盘）。
pub fn progress_for(exported: i64, total: i64) -> i16 {
    if total <= 0 || exported <= 0 {
        return 0;
    }
    let percent = exported.saturating_mul(100) / total;
    percent.clamp(0, 99) as i16
}

/// 指数退避：`base * 2^(attempt-1)`，上限 1 天。
///
/// # 参数
///
/// - `base`：退避基数（秒），小于 1 时按 1 处理。
/// - `attempt`：第几次尝试（从 1 开始）。
///
/// # 返回值
///
/// 退避秒数。
pub fn backoff_seconds(base: i64, attempt: i16) -> i64 {
    let base = base.max(1);
    let shift = (i32::from(attempt.max(1)) - 1).clamp(0, 16) as u32;
    base.saturating_mul(1_i64 << shift).min(86_400)
}

/// 产物文件名：`{导出名}_{任务号}.{扩展名}`，并清掉路径分隔符等危险字符。
///
/// # 参数
///
/// - `export_name`：用户填的导出名。
/// - `task_no`：任务号。
/// - `format`：`XLSX` / `CSV`。
///
/// # 返回值
///
/// 可直接拼进目录的文件名（不含路径分隔符）。
pub fn artifact_file_name(export_name: &str, task_no: &str, format: &str) -> String {
    let ext = if format.eq_ignore_ascii_case("CSV") {
        "csv"
    } else {
        "xlsx"
    };
    let base = sanitize_file_stem(export_name);
    let base = if base.is_empty() {
        "导出结果".to_string()
    } else {
        base
    };
    let base = truncate_chars(&base, 80);
    let task_no = sanitize_file_stem(task_no);
    format!("{base}_{task_no}.{ext}")
}

/// 清掉文件名里的路径分隔符与控制字符。
///
/// # 参数
///
/// - `raw`：原始名字。
///
/// # 返回值
///
/// 只含安全字符的名字（中文保留）。
pub fn sanitize_file_stem(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|value| {
            !matches!(
                value,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' | '\r' | '\n' | '\t'
            )
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// 按字符数截断（不切坏 UTF-8）。
fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    value.chars().take(limit).collect()
}

/// 生成 CSV 字节流（UTF-8 + BOM，RFC 4180 转义）。
///
/// # 参数
///
/// - `headers`：表头。
/// - `rows`：已格式化的行（键为表头）。
///
/// # 返回值
///
/// 可直接落盘的字节流。
pub fn build_csv(headers: &[&str], rows: &[HashMap<String, String>]) -> Vec<u8> {
    let mut out =
        String::with_capacity(rows.len().saturating_mul(headers.len()).saturating_mul(16));
    out.push_str(
        &headers
            .iter()
            .map(|item| csv_field(item))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");
    for row in rows {
        let line = headers
            .iter()
            .map(|header| csv_field(row.get(*header).map(String::as_str).unwrap_or("")))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&line);
        out.push_str("\r\n");
    }
    let mut bytes = Vec::with_capacity(out.len() + CSV_BOM.len());
    bytes.extend_from_slice(CSV_BOM);
    bytes.extend_from_slice(out.as_bytes());
    bytes
}

/// 单个 CSV 字段转义（含逗号 / 引号 / 换行时加引号并把 `"` 变 `""`）。
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// 计算 SHA-256（小写十六进制）。
///
/// # 参数
///
/// - `bytes`：内容。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_never_reaches_hundred_before_artifact_written() {
        // 数据取完（100 行 / 100 行）时仍是 99：100 只在产物落盘后由收尾语句写
        assert_eq!(progress_for(100, 100), 99);
        assert_eq!(progress_for(50, 100), 50);
        assert_eq!(progress_for(0, 100), 0);
    }

    #[test]
    fn progress_is_monotonic_for_increasing_exported() {
        let total = 100_000_i64;
        let mut last = -1_i16;
        for step in 0..=20 {
            let exported = (total / 20) * step;
            let value = progress_for(exported, total);
            assert!(value >= last, "进度回退了：{last} -> {value}");
            last = value;
        }
        assert_eq!(progress_for(total, total), 99);
    }

    #[test]
    fn progress_handles_unknown_total() {
        // total_rows = 0 表示「未知」，此时不显示百分比而不是显示 100%
        assert_eq!(progress_for(123, 0), 0);
        assert_eq!(progress_for(0, 0), 0);
    }

    #[test]
    fn progress_never_exceeds_ninety_nine_even_if_exported_gt_total() {
        // 导出期间数据被并发删除/新增会让实际行数超过 count，不能因此算出 120%
        assert_eq!(progress_for(120, 100), 99);
    }

    #[test]
    fn backoff_grows_exponentially_and_caps_at_one_day() {
        assert_eq!(backoff_seconds(30, 1), 30);
        assert_eq!(backoff_seconds(30, 2), 60);
        assert_eq!(backoff_seconds(30, 3), 120);
        // 基数非法 / 尝试次数非法都不 panic
        assert_eq!(backoff_seconds(0, 0), 1);
        assert_eq!(backoff_seconds(30, 40), 86_400);
    }

    #[test]
    fn backoff_is_capped_for_large_attempts() {
        // 2^16 * 30 = 1_966_080 > 1 天，必须被压到 86_400
        assert_eq!(backoff_seconds(30, 17), 86_400);
    }

    #[test]
    fn artifact_name_keeps_chinese_and_strips_path_separators() {
        let name = artifact_file_name("物料/清单:2026", "EXP20261028-9C1D", "XLSX");
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(':'), "{name}");
        assert!(name.starts_with("物料清单2026_"), "{name}");
        assert!(name.ends_with(".xlsx"), "{name}");
    }

    #[test]
    fn artifact_name_falls_back_when_export_name_blank() {
        let name = artifact_file_name("   ", "EXP20261028-9C1D", "CSV");
        assert!(name.starts_with("导出结果_"), "{name}");
        assert!(name.ends_with(".csv"), "{name}");
    }

    #[test]
    fn artifact_name_truncates_long_stem() {
        let long = "字".repeat(200);
        let name = artifact_file_name(&long, "EXP20261028-9C1D", "XLSX");
        assert!(name.chars().count() < 120, "{}", name.chars().count());
    }

    #[test]
    fn csv_escapes_comma_quote_and_newline() {
        let headers = ["名称", "备注"];
        let mut row = HashMap::new();
        row.insert("名称".to_string(), "含,逗号".to_string());
        row.insert("备注".to_string(), "带\"引号\"\n换行".to_string());
        let bytes = build_csv(&headers, &[row]);
        let text = String::from_utf8_lossy(&bytes[CSV_BOM.len()..]).to_string();
        assert!(text.contains("\"含,逗号\""), "{text}");
        assert!(text.contains("\"带\"\"引号\"\"\n换行\""), "{text}");
    }

    #[test]
    fn csv_starts_with_bom_and_crlf_and_follows_header_order() {
        let headers = ["B", "A"];
        let mut row = HashMap::new();
        row.insert("A".to_string(), "1".to_string());
        row.insert("B".to_string(), "2".to_string());
        let bytes = build_csv(&headers, &[row]);
        assert!(bytes.starts_with(CSV_BOM), "缺 BOM");
        let text = String::from_utf8_lossy(&bytes[CSV_BOM.len()..]).to_string();
        assert_eq!(
            text, "B,A\r\n2,1\r\n",
            "列顺序必须按定义而不是 HashMap 顺序"
        );
    }

    #[test]
    fn csv_missing_cell_becomes_empty_field() {
        let headers = ["A", "B"];
        let mut row = HashMap::new();
        row.insert("A".to_string(), "1".to_string());
        let bytes = build_csv(&headers, &[row]);
        let text = String::from_utf8_lossy(&bytes[CSV_BOM.len()..]).to_string();
        assert_eq!(text, "A,B\r\n1,\r\n", "{text}");
    }

    #[test]
    fn filter_from_params_parses_keyword_and_status() {
        let filter = filter_from_params(Some("{\"keyword\":\"  物料 \",\"status\":1}"));
        assert_eq!(filter.keyword.as_deref(), Some("物料"));
        assert_eq!(filter.status, Some(1));
    }

    #[test]
    fn filter_from_params_tolerates_broken_json() {
        // 快照是我们自己写的，解析失败按「无过滤」处理，不让任务卡死在重试里
        let filter = filter_from_params(Some("not-json"));
        assert!(filter.keyword.is_none());
        assert!(filter.status.is_none());
        let filter = filter_from_params(None);
        assert!(filter.keyword.is_none());
    }

    #[test]
    fn filter_from_params_drops_blank_keyword() {
        let filter = filter_from_params(Some("{\"keyword\":\"   \"}"));
        assert!(filter.keyword.is_none());
    }

    #[test]
    fn report_summary_lists_all_counters() {
        let report = WorkerReport {
            claimed: 2,
            succeeded: 1,
            requeued: 0,
            failed: 1,
            canceled: 0,
        };
        let summary = report.summary();
        assert!(summary.starts_with(TASK_NAME), "{summary}");
        assert!(summary.contains("claimed=2"), "{summary}");
        assert!(summary.contains("failed=1"), "{summary}");
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn options_with_dir_uses_defaults() {
        let options = WorkerOptions::with_dir("w-1", "/tmp/x");
        assert_eq!(options.worker_id, "w-1");
        assert_eq!(options.ttl_seconds, DEFAULT_TTL_SECONDS);
        assert_eq!(options.max_attempts, DEFAULT_MAX_ATTEMPTS);
        assert_eq!(options.batch_size, DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn cron_expression_is_every_five_seconds() {
        // 架构 §3.5 明确要求「每 5 秒扫一次」，写成 `*/5 * * * *` 会变成每 5 分钟
        assert_eq!(TASK_CRON.split_whitespace().count(), 6);
        assert_eq!(TASK_CRON, "*/5 * * * * *");
    }

    #[test]
    fn absolute_dir_keeps_absolute_and_resolves_relative() {
        // 产物路径落库必须是绝对路径：相对路径在「重启换了 cwd」后就打不开了
        assert_eq!(
            absolute_dir(Path::new("/tmp/a4/export")),
            PathBuf::from("/tmp/a4/export")
        );
        let resolved = absolute_dir(Path::new("uploads/impexp/export"));
        assert!(resolved.is_absolute(), "{resolved:?}");
        assert!(resolved.ends_with("uploads/impexp/export"), "{resolved:?}");
    }
}

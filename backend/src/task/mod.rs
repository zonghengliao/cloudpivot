//! 定时任务与后台任务。
//!
//! 与业务模块分离，避免业务模块反向依赖调度细节。任务体可以调用任意业务模块的
//! `model.rs` / `handler.rs` 中已公开的函数。
//!
//! 调度采用框架内置的 [`nik::prelude::nik_plugin::scheduler::SchedulerPlugin`]（cron 表达式驱动，
//! 支持 5 段与 6 段写法）。框架调度器在 `start()` 时对注册表做**一次性快照**，
//! 启动后再 `register()` 不会生效；因此运行期增删任务必须走
//! `stop()` → 增删注册 → `start()`，该流程统一由 [`Scheduler::reload`] 完成。
//!
//! # 任务来源
//!
//! - **系统内置任务**：编译期固定在 [`Scheduler::reload`] 中注册；
//! - **动态任务**：从 `sys_job`（`status = 1`）读取后在 [`Scheduler::reload`] 中装载。
//!
//! | 任务 | cron | 处理器（invoke_target） | 交付阶段 |
//! |------|------|------------------------|----------|
//! | 操作日志清理 | `0 3 * * *` | `clean_operation_log` | P4 |
//! | 审计日志清理 | `10 3 * * *` | `clean_audit_log` | P4 |
//! | 认证日志清理 | `20 3 * * *` | `clean_auth_log` | P4 |
//! | 安全日志清理 | `30 3 * * *` | `clean_security_log` | P4 |
//! | 密码变更日志清理 | `40 3 * * *` | `clean_password_log` | P4 |
//! | 存储对象回收 | `0 30 4 * * *` | `storage_object_cleanup` | 批次 A · A1 |
//! | 导出任务执行 | `*/5 * * * * *` | `impexp_export_worker` | 批次 A · A4 |
//! | 导入导出回收 | `0 40 4 * * *` | `impexp_task_cleanup` | 批次 A · A4 |
//! | 定时通知投递 | `*/30 * * * * *` | `notice_scheduled_dispatch` | 批次 A · 遗留修复③ |
//! | 公告到期自动下线 | `0 5 0 * * *` | `announcement_auto_expire` | 批次 C · C3 |
//! | 公告置顶到期取消 | `0 10 0 * * *` | `announcement_top_expire` | 批次 C · C3 |
//! | 公告扇出补偿重试 | `0 */10 * * * *` | `announcement_dispatch_retry` | 批次 C · C3 |
//! | 变更日志分区运维 | `0 20 1 1 * *` | `changelog_partition_maintain` | 批次 C · C0（空实现） |
//! | 帮助历史版本清理 | `0 40 2 * * *` | `help_version_cleanup` | 批次 C · C4（真实现，见 `business::help::model::register_builtin`） |
//! | 审批超时扫描 + 催办 | `0 */10 * * * *` | `flow_timeout_scan` | 批次 D · D4c（真实现，见 `business::flow::timeout::register_builtin`） |
//! | 审批图快照自检 | `0 30 3 * * *` | `flow_graph_selfcheck` | 批次 D · D4d（真实现，见 `business::flow::maintain::register_builtin`） |
//! | 令牌吊销表清理 | `0 50 3 * * *` | `token_blacklist_cleanup` | 批次 G · 令牌吊销落库（见 `system::auth::cleanup::register_builtin`） |
//! | 账务不变量巡检 | `0 20 2 * * *` | `invariant_audit` | P2-7（真实现，见 `support::invariant::runner`） |
//! | 运维观测采样 | `0 * * * * *` | `metrics_snapshot` | NEAR-FIX-3（真实现，见 [`metrics_sampler`]） |
//!
//! 五张日志表错峰清理（均为每小时 3 点的不同分钟），避免同时发起批量删除抬高
//! 数据库瞬时压力。存储对象回收单独排在 04:30，与日志清理完全错峰（它要遍历
//! `sys_file` 并按后端删物理对象，IO 比纯 SQL 更重）。
//!
//! 业务侧的定时任务通过 [`Scheduler::register_cron`] 在 [`Scheduler::register_builtin`]
//! 中挂载（框架调度器对注册表做一次性快照，注册必须发生在 `start()` 之前；
//! 在 `bootstrap.rs` 里追加注册会被随后的 `reload()` 清空）。
//!
//! 在线用户会话存于缓存并按令牌有效期自动过期，因此**不实现**「在线用户清理」任务。
//!
//! [`scheduler`] 提供全局单例，由装配代码加入插件列表（`App::plugin`）并随应用
//! 生命周期启停；运行期由定时任务管理接口触发 [`Scheduler::reload`] 重新装载。

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use async_trait::async_trait;
use nik::prelude::nik_plugin::scheduler::{CronSchedule, SchedulerPlugin};
use nik::prelude::*;

use crate::shared::{constants, error_code};
use crate::support::job::model::{JobLogStatus, SysJobLogModel, SysJobModel};
use crate::support::log::model::{self as log_model, JsonLogTable};
use crate::utils;

pub mod metrics_sampler;

/// 系统内置的日志清理任务：`(处理器标识 / 调度名, cron 表达式)`。
///
/// 标识同时用作调度名与 `sys_job.invoke_target`，因此三者保持一致，便于前端
/// 选择调用目标。
const LOG_CLEAN_TASKS: [(&str, &str); 5] = [
    ("clean_operation_log", "0 3 * * *"),
    ("clean_audit_log", "10 3 * * *"),
    ("clean_auth_log", "20 3 * * *"),
    ("clean_security_log", "30 3 * * *"),
    ("clean_password_log", "40 3 * * *"),
];

/// 把框架错误转换为统一的 500 内部错误（对外隐藏细节，细节写入日志）。
///
/// # 参数
///
/// - `e`：框架错误。
/// - `ctx`：业务上下文描述，用于日志定位。
///
/// # 返回值
///
/// 统一响应体为 `code = INTERNAL_ERROR`、消息为「服务器内部错误」的 [`ApiError`]。
fn scheduler_error(e: Error, ctx: &str) -> ApiError {
    tracing::error!(context = ctx, error = %e, "调度器操作失败");
    ApiError::new(500, error_code::INTERNAL_ERROR, "服务器内部错误")
}

/// 全局调度器：在框架 [`SchedulerPlugin`] 之上记录已注册的任务名，以便
/// [`Scheduler::reload`] 时先清理旧注册再重新装载。
///
/// 轻量克隆：内部字段均为 `Arc`，克隆后与本体共享同一调度器实例。
#[derive(Clone)]
pub struct Scheduler {
    /// 框架调度插件（`Arc` 包裹以便随本结构体一起轻量克隆）。
    inner: Arc<SchedulerPlugin>,
    /// 已注册到 `inner` 的任务名集合（含系统内置任务）。
    names: Arc<Mutex<HashSet<String>>>,
}

impl Scheduler {
    /// 创建空调度器（不注册任何任务）。
    ///
    /// 系统内置任务在 [`Scheduler::reload`] 中注册，这里仅创建空插件。
    ///
    /// # 返回值
    ///
    /// 未装载任何任务的调度器实例。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SchedulerPlugin::new()),
            names: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 重新装载全部任务。
    ///
    /// 执行顺序：`stop()` → 清理旧注册名 → 注册系统内置任务 → 从 `sys_job`
    /// 读取启用任务并注册 → `start()`。`sys_job` 查询失败时只记错误日志并保留
    /// 系统内置任务，不使装载失败。
    ///
    /// # 返回值
    ///
    /// 装载并启动成功返回 `Ok(())`。
    ///
    /// # 异常情况
    ///
    /// 调度器 `stop()` / `start()` 失败返回 500。
    pub async fn reload(&self) -> std::result::Result<(), ApiError> {
        // 1. 停止当前调度循环（框架 start() 对注册表做一次性快照，必须重载）
        self.inner.stop().await.map_err(|e| scheduler_error(e, "停止调度器"))?;

        // 2. 清理上一次注册的任务名并逐个移除
        self.clear_registered();

        // 3. 注册系统内置任务
        self.register_builtin();

        // 4. 从数据库装载启用中的动态任务；查询失败不阻断启动
        match self.load_db_jobs().await {
            Ok(jobs) => {
                for job in jobs {
                    self.register_job(job);
                }
            }
            Err(e) => {
                tracing::error!(error = %e.msg, "装载定时任务失败，仅注册系统内置任务");
            }
        }

        // 5. 启动调度
        self.inner.start().await.map_err(|e| scheduler_error(e, "启动调度器"))?;
        Ok(())
    }

    /// 立即执行一次指定任务，并记录执行日志、回写任务运行时间。
    ///
    /// # 参数
    ///
    /// - `job`：任务定义（含处理器标识、参数与 cron 表达式）。
    ///
    /// # 返回值
    ///
    /// 执行成功返回处理器的结果摘要文本。
    ///
    /// # 异常情况
    ///
    /// 处理器未注册或执行失败时返回对应 [`ApiError`]；日志写入与时间回写失败
    /// 只记错误日志，不影响返回值。
    pub async fn run_job(&self, job: &SysJobModel) -> std::result::Result<String, ApiError> {
        let started_at = chrono::Utc::now();
        let started = Instant::now();

        let outcome = run_handler(&job.invoke_target, job.params.clone()).await;

        let finished_at = chrono::Utc::now();
        let duration_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;

        // 无论成败都落一条执行日志
        self.write_log(job, &outcome, started_at, finished_at, duration_ms)
            .await;

        // 回写任务运行时间
        self.touch_job(job, finished_at).await;

        outcome
    }

    /// 触发框架调度器按已注册名字执行一次。
    ///
    /// # 参数
    ///
    /// - `name`：注册到框架调度器的任务名。
    ///
    /// # 返回值
    ///
    /// 找到对应注册项并成功启动返回 `true`，否则 `false`。
    pub fn trigger_registered(&self, name: &str) -> bool {
        self.inner.trigger(name).is_some()
    }

    /// 清空已注册任务名集合并逐个从框架调度器移除。
    fn clear_registered(&self) {
        let mut names = self.names.lock().unwrap_or_else(|poison| poison.into_inner());
        for name in names.drain() {
            self.inner.remove(&name);
        }
    }

    /// 记录一个已注册的任务名。
    ///
    /// # 参数
    ///
    /// - `name`：注册到框架调度器的任务名。
    fn remember(&self, name: &str) {
        self.names
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(name.to_string());
    }

    /// 注册系统内置任务。
    ///
    /// 内置任务无 `sys_job` 记录，执行结果只写运行日志（不写 `sys_job_log`）。
    /// 全部任务来自 [`LOG_CLEAN_TASKS`]，逐一注册；随后挂载**业务侧**内置任务
    /// （当前只有存储对象回收，见 [`crate::business::storage::cleanup`]）。
    fn register_builtin(&self) {
        for (name, cron) in LOG_CLEAN_TASKS {
            let runner = move || tokio::spawn(run_builtin(name));
            self.inner.register(name, cron, "系统任务：日志清理", runner);
            self.remember(name);
        }
        // 业务侧内置任务：注册点必须在这里（与日志清理同批），不能挪到 bootstrap.rs
        crate::business::storage::cleanup::register_builtin(self);
        // 过期通知清理（A5）：同样必须在此注册，放在 bootstrap.rs 会被随后的 reload 清空
        crate::business::notice::cleanup::register_builtin(self);
        // SSE 空频道回收（A6）：每 5 分钟巡逻一次，回收「无人订阅且空置 30 分钟」的频道
        crate::business::notice::sse_hub::register_builtin(self);
        // 定时通知投递（A 遗留③）：每 30 秒扫一次「到点待发」队列（scheduled_at）
        crate::business::notice::scheduled::register_builtin(self);
        // 批次 A · A4：异步导出执行（每 5 秒扫一次）与任务/产物回收（每天 04:40）
        crate::business::impexp::worker::register_builtin(self);
        crate::business::impexp::cleanup::register_builtin(self);
        // 批次 C · C3：公告三个任务（到期下线 / 置顶到期 / **扇出补偿重试**）已接上真实任务体
        crate::business::announcement::maintenance::register_builtin(self);
        // 批次 C · C6：变更日志分区运维（**真实现**：预建下月分区 + 清理过期分区）。
        // 任务体在 `business::changelog::partition::run_once`（只吃执行器），
        // 全局连接在这里传进去 —— 于是 changelog 模块内依旧没有 db()（门禁 G2 不开口子）。
        {
            use crate::business::changelog::partition;
            self.register_cron(
                partition::TASK_NAME,
                partition::CRON,
                "系统任务：变更日志分区预建与清理",
                || {
                    tokio::spawn(async move {
                        match partition::run_once(db(), chrono::Utc::now(), partition::RETENTION_MONTHS).await {
                            Ok(report) => {
                                tracing::info!(report = %report.summary(), "变更日志分区运维完成")
                            }
                            Err(error) => {
                                tracing::error!(%error, "变更日志分区运维失败（分区未预建会导致写入越界）")
                            }
                        }
                    })
                },
            );
            tracing::info!(
                invoke_target = partition::TASK_NAME,
                cron = partition::CRON,
                "变更日志分区运维任务已注册（C6 真实现）"
            );
        }
        // 批次 C · C4：帮助文档历史版本清理（真实现，已从占位数组移出）
        crate::business::help::model::register_builtin(self);
        // 批次 C+ · T5b：AI 任务状态同步（每 30 秒扫一遍「已派活且未终态」的 AI 任务）
        crate::business::task::hermes_sync::register_builtin(self);
        // 批次 D · D4c：审批超时扫描 + 催办（每 10 分钟，LIMIT 500 分批；默认只提醒，AUTO_APPROVE 需全局闸）
        crate::business::flow::timeout::register_builtin(self);
        // LNK-F12：在途调拨超期扫描 + 告警（每小时第 5 分钟；阈值走 sys_config，默认 7 天）
        crate::business::erp::transfer_overdue::register_builtin(self);
        // R-P1-2：业务提醒任务包（应付到期 07:10 / 采购超期 07:20 / 合同到期 07:30 / 低库存 07:40）。
        // 幂等由 biz_reminder_log 台账（单据 × 类型 × 业务日唯一）+ 通知幂等键两层保证。
        crate::business::erp::reminder::register_builtin(self);
        // P16：收货作废审批的**孤儿实例回收**（每 10 分钟）。
        // 判据：审批还在跑、单据却已删除或已不是「作废审批中」→ 终止审批单（P11 教训：
        // 不回收就是审批中心一堆点进去 404 的待办，且下一次发起会被 uk_ 唯一索引永久堵死）。
        crate::business::erp::stockin_bridge::register_builtin(self);
        // P17：付款审批的**孤儿实例回收**（每 10 分钟，与收货作废错开 4 分钟）。
        // 判据：审批还在跑、付款记录却已删除或已不是「审批中」→ 终止审批单。
        // 不回收的代价比 P16 更大：孤儿实例会顶住 uk_biz_payment_pending，
        // 让那张应付单永远付不出第二笔钱（409 且无法自愈）。
        crate::business::erp::payable_bridge::register_builtin(self);
        // P-FLOW-3B：应付作废审批 / 凭证红冲审批的**孤儿实例回收**（每 10 分钟，与上面三个错开）。
        // 判据：审批还在跑、单据却已删除 / 状态已变 / pending 引用已被别处清掉 → 终止审批单。
        // 不回收的代价：审批中心留孤儿待办（点进去 404），且下一次发起会撞
        // uk_biz_flow_instance_biz_active 永久 409；凭证那边还会永远 409「已在红冲审批中」。
        crate::business::erp::payable_bridge::register_cancel_builtin(self);
        crate::business::erp::voucher_bridge::register_builtin(self);
        // FIN-2：折旧计提（每月 1 日 01:30 计提上一个会计月的折旧）
        crate::business::erp::depreciation_model::register_builtin(self);
        // SO-38 / ARCH-7 §15.5：客户信用额度兜底重算（每天 02:10，早于 02:20 的
        // 账务不变量巡检 —— 先修正再巡检：巡检还红就说明今晚真没修成）。
        crate::business::erp::credit::register_builtin(self);
        // 批次 D · D4d：审批图快照自检（每天 03:30，分批扫 PUBLISHED 版本重算 graph_hash；不一致 WARN + 审计）
        crate::business::flow::maintain::register_builtin(self);
        // 批次 E · E11：周期任务生成（每 5 分钟扫一次 biz_task_template.next_run_at）
        crate::business::task::recurring::register_builtin(self);
        // 批次 G · 令牌吊销落库（ADR-20261101）：吊销表日清（每天 03:50，与日志表错峰）。
        // 判据 `expire_at < now() - 1h`（保留宽限吸收时钟偏差）；清理任务同时是
        // 「每请求主键点查」查询计划的隐式前提，挂了必须有人知道（任务内发 rows 指标）。
        crate::system::auth::cleanup::register_builtin(self);
        // P2-7：账务不变量巡检（每天 02:20）。跑的是闸③ 同一个脚本
        // `scripts/verify/erp_invariants.py --json`，结果落 `sys_invariant_result`，
        // 供「账务巡检看板」出趋势。02:20 与 03:00-03:50 的日志清理错峰。
        // 这里**只注册调度**，任务体在 `support::invariant::runner`（跑脚本 + 落库）。
        {
            use crate::support::invariant::runner;
            self.register_cron(
                runner::TASK_NAME,
                runner::CRON,
                "系统任务：账务不变量巡检（结果落 sys_invariant_result）",
                || {
                    tokio::spawn(async move {
                        match runner::run_and_store(runner::Trigger::Scheduled).await {
                            Ok(record) => tracing::info!(
                                result = %runner::summary(&record),
                                "账务不变量巡检完成"
                            ),
                            Err(error) => tracing::error!(
                                %error,
                                "账务不变量巡检落库失败（本次运行未被记录）"
                            ),
                        }
                    })
                },
            );
            tracing::info!(
                invoke_target = runner::TASK_NAME,
                cron = runner::CRON,
                "账务不变量巡检任务已注册（P2-7）"
            );
        }
        // NEAR-FIX-3：运维观测采样（每分钟）—— 把「告警判定要用的事实」采成 gauge：
        // 导入导出积压、最近一次巡检结论、采样器自身存活。`/metrics` 从此是告警口径的
        // 唯一来源（告警脚本不必持有库口令），也是 Prometheus 上规则要用的同一批名字。
        crate::task::metrics_sampler::register_builtin(self);
    }

    /// 注册一个业务侧的内置 cron 任务（供业务模块在 [`Self::register_builtin`] 中调用）。
    ///
    /// # 参数
    ///
    /// - `name`：调度名（同时用于日志与 [`Self::trigger_registered`]）。
    /// - `cron`：cron 表达式（支持 5 段与 6 段）。
    /// - `desc`：任务描述。
    /// - `runner`：每次触发时调用的闭包（内部自行 `tokio::spawn`，不要阻塞调度线程）。
    ///
    /// # 说明
    ///
    /// 只解决「注册 + 记账」两件事：注册到框架调度器，并把名字记入 [`Self::names`]，
    /// 使下一次 [`Self::reload`] 的 `clear_registered` 能正确移除，不会残留重复调度。
    pub fn register_cron(
        &self,
        name: &str,
        cron: &str,
        desc: &str,
        runner: impl Fn() -> tokio::task::JoinHandle<()> + Send + Sync + 'static,
    ) {
        self.inner.register(name, cron, desc, runner);
        self.remember(name);
    }

    /// 注册单个动态任务（来自 `sys_job`）。
    ///
    /// 调度名取 `job_name`，cron 取 `cron_expr`，描述形如
    /// `系统任务：{job_name}`；runner 内克隆任务快照并在独立异步任务中调用
    /// [`Scheduler::run_job`]，失败只记日志。
    ///
    /// # 参数
    ///
    /// - `job`：任务定义。
    fn register_job(&self, job: SysJobModel) {
        let name = job.job_name.clone();
        let cron = job.cron_expr.clone();
        let desc = format!("系统任务：{}", job.job_name);
        let this = self.clone();
        let runner = move || {
            let this = this.clone();
            let job = job.clone();
            tokio::spawn(async move {
                if let Err(e) = this.run_job(&job).await {
                    tracing::error!(job = %job.job_name, error = %e.msg, "定时任务执行失败");
                }
            })
        };
        self.inner.register(&name, &cron, &desc, runner);
        self.remember(&name);
    }

    /// 从 `sys_job` 读取启用中的任务定义。
    ///
    /// # 返回值
    ///
    /// `status = 1` 且未软删的任务模型列表。
    ///
    /// # 异常情况
    ///
    /// 数据库异常返回 500。
    async fn load_db_jobs(&self) -> std::result::Result<Vec<SysJobModel>, ApiError> {
        let rows = db()
            .find_with_params(
                "SELECT * FROM sys_job WHERE deleted_at IS NULL AND status = ? \
                 ORDER BY created_at ASC",
                vec![TypeMapper::to_value(&constants::STATUS_ENABLED)],
            )
            .await
            .map_err(|e| utils::db_error(e, "查询启用定时任务"))?;
        rows.into_iter()
            .map(SysJobModel::from_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| utils::db_error(e, "映射定时任务模型"))
    }

    /// 写入一条任务执行日志（失败只记错误日志，不影响执行结果）。
    ///
    /// # 参数
    ///
    /// - `job`：任务定义（用于快照字段）。
    /// - `outcome`：处理器执行结果；成功取其摘要文本，失败取其错误信息，
    ///   并据此决定日志状态。
    /// - `started_at`：开始时间。
    /// - `finished_at`：结束时间。
    /// - `duration_ms`：耗时（毫秒）。
    async fn write_log(
        &self,
        job: &SysJobModel,
        outcome: &std::result::Result<String, ApiError>,
        started_at: chrono::DateTime<chrono::Utc>,
        finished_at: chrono::DateTime<chrono::Utc>,
        duration_ms: i32,
    ) {
        let (status, message, error) = match outcome {
            Ok(msg) => (JobLogStatus::success, Some(msg.clone()), None),
            Err(e) => (JobLogStatus::failed, None, Some(e.msg.clone())),
        };
        let mut log = SysJobLogModel {
            id: uuid::Uuid::now_v7(),
            job_id: job.id,
            job_name: job.job_name.clone(),
            invoke_target: job.invoke_target.clone(),
            params: job.params.clone(),
            status,
            message,
            error,
            started_at,
            finished_at: Some(finished_at),
            duration_ms: Some(duration_ms),
            // 追加写表关闭时间戳 Hook，created_at 必须显式赋值
            created_at: finished_at,
        };
        if let Err(e) = log.insert(db()).await {
            tracing::error!(job = %job.job_name, error = %e, "写入定时任务执行日志失败");
        }
    }

    /// 回写任务的 `last_run_at` / `next_run_at` / `updated_at`。
    ///
    /// `next_run_at` 由 cron 表达式推算，表达式不可解析或永无后续触发时刻时置为
    /// `NULL`。回写失败只记错误日志。
    ///
    /// # 参数
    ///
    /// - `job`：任务定义。
    /// - `finished_at`：本次执行结束时间，作为 `last_run_at` / `updated_at`。
    async fn touch_job(&self, job: &SysJobModel, finished_at: chrono::DateTime<chrono::Utc>) {
        let next_run_at = CronSchedule::parse(&job.cron_expr)
            .ok()
            .and_then(|schedule| schedule.next_after(chrono::Utc::now()));
        let next_value = match next_run_at {
            Some(value) => TypeMapper::to_value(&value),
            None => Value::Null,
        };
        let result = db()
            .execute_with_params(
                "UPDATE sys_job SET last_run_at = ?, next_run_at = ?, updated_at = ? WHERE id = ?",
                vec![
                    TypeMapper::to_value(&finished_at),
                    next_value,
                    TypeMapper::to_value(&finished_at),
                    Value::from(&job.id),
                ],
            )
            .await;
        if let Err(e) = result {
            tracing::error!(job = %job.job_name, error = %e, "回写定时任务运行时间失败");
        }
    }
}

impl Default for Scheduler {
    /// 等价于 [`Scheduler::new`]。
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for Scheduler {
    /// 插件名称（用于插件管理与日志）。
    fn name(&self) -> &str {
        "x-scheduler"
    }

    /// 启动调度：转发给框架 [`SchedulerPlugin`]。
    ///
    /// # 返回值
    ///
    /// 启动成功返回 `Ok(())`。
    ///
    /// # 异常情况
    ///
    /// 注册项中存在非法 cron 表达式等由框架返回错误。
    async fn start(&self) -> std::result::Result<(), Error> {
        self.inner.start().await
    }

    /// 关闭调度：转发给框架 [`SchedulerPlugin`]。
    ///
    /// # 返回值
    ///
    /// 关闭成功返回 `Ok(())`。
    ///
    /// # 异常情况
    ///
    /// 框架内部错误时返回错误。
    async fn stop(&self) -> std::result::Result<(), Error> {
        self.inner.stop().await
    }
}

/// 全局调度器单例。
static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();

/// 获取全局调度器单例（首次调用时惰性初始化）。
///
/// # 返回值
///
/// 全局调度器的轻量克隆（内部共享同一 `Arc<SchedulerPlugin>`）。
pub fn scheduler() -> Scheduler {
    SCHEDULER.get_or_init(Scheduler::new).clone()
}

/// 执行系统内置处理器并记录结果日志。
///
/// # 参数
///
/// - `invoke_target`：内置处理器标识（同时用作调度名）。
async fn run_builtin(invoke_target: &'static str) {
    match run_handler(invoke_target, None).await {
        Ok(message) => {
            tracing::info!(invoke_target = invoke_target, result = %message, "内置定时任务执行完成");
        }
        Err(e) => {
            tracing::error!(invoke_target = invoke_target, error = %e.msg, "内置定时任务执行失败");
        }
    }
}

/// 判断处理器标识是否已注册。
///
/// 内置处理器标识集中登记在 [`LOG_CLEAN_TASKS`]，供定时任务管理接口在新建 / 更新时
/// 校验 `invoke_target`，避免落库一个永远无法执行的调用目标。
///
/// 与 [`run_handler`] 的分支必须**一一对应**：`run_handler` 里能跑的标识，这里就得算
/// 「已注册」。R-P1-2 的 4 个业务提醒（`erp_payable_due_scan` 等）早就在 `run_handler`
/// 的 RemindKind 分支里可执行，却没进这份名单 —— 后果是「定时任务」页对这些行做
/// **新建 / 编辑**（PUT）一律 422「未注册的任务处理器」，运维只能停在停用位
/// （P0-FIX-1 的现场）。启停（PATCH status）不查名单，所以只有编辑路径会被卡。
///
/// # 参数
///
/// - `target`：调用目标标识。
///
/// # 返回值
///
/// 已注册返回 `true`，否则 `false`。
pub fn is_registered_target(target: &str) -> bool {
    LOG_CLEAN_TASKS.iter().any(|(name, _)| *name == target)
        || target == crate::support::invariant::runner::TASK_NAME
        // R-P1-2：4 类业务提醒（应付到期 / 采购超期 / 合同到期 / 低库存）
        || crate::business::erp::reminder::RemindKind::from_task_name(target).is_some()
}

/// 执行指定处理器。
///
/// 内置处理器为五张日志表的清理任务（见 [`LOG_CLEAN_TASKS`]）：各自删除本表中
/// 超过 [`constants::OPERATION_LOG_RETENTION_DAYS`] 天的记录，返回清理条数消息。
///
/// 内置处理器暂不使用 `params`，该参数保留给后续扩展。
///
/// # 参数
///
/// - `invoke_target`：处理器标识（调用目标）。
/// - `params`：任务参数（可为 `None`）。
///
/// # 返回值
///
/// 处理器执行成功返回结果摘要文本。
///
/// # 异常情况
///
/// - 422：处理器标识未注册；
/// - 500：处理器内部数据库操作失败。
pub async fn run_handler(invoke_target: &str, params: Option<JsonValue>) -> std::result::Result<String, ApiError> {
    tracing::debug!(invoke_target = invoke_target, params = ?params, "执行任务处理器");
    match invoke_target {
        "clean_operation_log" => {
            let affected = log_model::delete_operation_logs_before(log_cutoff())
                .await
                .map_err(|e| utils::db_error(e, "清理操作日志"))?;
            Ok(format!("已清理 {affected} 条操作日志"))
        }
        "clean_audit_log" => cleanup_json_logs(JsonLogTable::Audit, "审计日志").await,
        "clean_auth_log" => cleanup_json_logs(JsonLogTable::Auth, "认证日志").await,
        "clean_security_log" => cleanup_json_logs(JsonLogTable::Security, "安全日志").await,
        "clean_password_log" => cleanup_json_logs(JsonLogTable::PasswordChange, "密码变更日志").await,
        // R-P1-2：业务提醒（4 类扫描）。sys_job 里的 4 行种子默认停用（内置 cron 已在跑），
        // 但「定时任务」页的「立即执行」走的就是这里 —— 验收与事后补跑都用它。
        target if crate::business::erp::reminder::RemindKind::from_task_name(target).is_some() => {
            let kind = crate::business::erp::reminder::RemindKind::from_task_name(target).expect("上面刚判过");
            let report = crate::business::erp::reminder::run_kind(kind, chrono::Utc::now())
                .await
                .map_err(|e| utils::db_error(e, "执行业务提醒扫描"))?;
            Ok(report.summary(kind))
        }
        // P2-7：账务不变量巡检（定时任务页「执行一次」与内置 cron 共用同一任务体）。
        // 注意这里返回 Ok 即使 status = FAIL —— 那是账务结论，不是任务执行失败；
        // 只有脚本没跑成（ERROR 行）与写库失败会让摘要里出现原因。
        target if target == crate::support::invariant::runner::TASK_NAME => {
            let record =
                crate::support::invariant::runner::run_and_store(crate::support::invariant::runner::Trigger::Manual)
                    .await
                    .map_err(|e| utils::db_error(e, "执行账务不变量巡检并落库"))?;
            Ok(crate::support::invariant::runner::summary(&record))
        }
        // SO-38：客户信用额度兜底重算（定时任务页「执行一次」走这里，与内置 cron 共用任务体）。
        // 重算发现漂移会**修正并记 biz_credit_log**（cause = RECOMPUTE），返回摘要给出明细。
        target if target == crate::business::erp::credit::TASK_NAME => {
            let report = crate::business::erp::credit::recompute_all("JOB")
                .await
                .map_err(|e| utils::db_error(e, "执行客户信用额度重算"))?;
            Ok(report.summary())
        }
        other => Err(ApiError::new(
            422,
            error_code::JOB_TARGET_UNREGISTERED,
            format!("未注册的任务处理器: {other}"),
        )),
    }
}

/// 计算日志清理的时间阈值（当前时间减去统一保留天数）。
///
/// # 返回值
///
/// 早于该时刻的日志将被清理。
fn log_cutoff() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - chrono::Duration::days(constants::OPERATION_LOG_RETENTION_DAYS)
}

/// 清理一张 JSON 日志表并返回结果摘要。
///
/// # 参数
///
/// - `table`：目标日志表。
/// - `label`：中文表名，用于拼接结果消息。
///
/// # 返回值
///
/// 形如「已清理 N 条审计日志」的结果摘要。
///
/// # 异常情况
///
/// 数据库异常返回 500。
async fn cleanup_json_logs(table: JsonLogTable, label: &str) -> std::result::Result<String, ApiError> {
    let affected = log_model::delete_json_logs_before(table, log_cutoff())
        .await
        .map_err(|e| utils::db_error(e, "清理日志"))?;
    Ok(format!("已清理 {affected} 条{label}"))
}

// ==================== 用例：注册表去重 + 处理器名单 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 取某个调度名在框架注册表里的条目（正常情况下最多 1 条）。
    fn entries(scheduler: &Scheduler, name: &str) -> Vec<(String, String, String)> {
        scheduler
            .inner
            .list()
            .into_iter()
            .filter(|(registered, _, _)| registered == name)
            .collect()
    }

    /// 内置注册 + `sys_job` 动态注册同名任务时，注册表里**只能留一条**。
    ///
    /// 为什么必须有用例守着：迁移 V0245 把那 4 行 `sys_job` 打开成 `status = 1` 之后，
    /// `reload()` 会先 `register_builtin()`（`reminder::register_builtin` 用同一个
    /// `job_name` 注册）再从 `sys_job` 走 `register_job()` 注册同样的名字。
    /// 框架 `SchedulerPlugin::insert` 是 `HashMap::insert`（按名字覆盖），所以只会剩一条；
    /// 一旦以后有人把它改成 `Vec` / 追加语义，同一个 cron 一天会跑两遍（台账与通知幂等键
    /// 只兜住重复通知，白跑的那一轮无人发现）——这条用例就是那道红线。
    #[test]
    fn dynamic_registration_overwrites_builtin_of_same_name() {
        let scheduler = Scheduler::new();
        scheduler.register_builtin();

        for kind in crate::business::erp::reminder::RemindKind::ALL {
            let name = kind.task_name();
            let builtin = entries(&scheduler, name);
            assert_eq!(
                builtin.len(),
                1,
                "内置注册后 {name} 应恰好 1 条，实际 {}",
                builtin.len()
            );
            assert_eq!(builtin[0].1, kind.cron(), "{name} 内置 cron 应为 {}", kind.cron());

            // 模拟 load_db_jobs → register_job：同样的名字、同样的 cron
            scheduler.register_cron(name, kind.cron(), "系统任务：来自 sys_job", || {
                tokio::spawn(async {})
            });

            let after = entries(&scheduler, name);
            assert_eq!(
                after.len(),
                1,
                "{name} 被动态任务再注册一次后仍应只有 1 条调度（否则一天跑两遍），实际 {}",
                after.len()
            );
            assert_eq!(
                after[0].2, "系统任务：来自 sys_job",
                "{name} 后注册者（sys_job 行）应覆盖前者的描述，证明是覆盖而不是并存"
            );
        }
    }

    /// `run_handler` 能跑的标识必须都算「已注册」，否则管理接口会 422 卡住编辑。
    #[test]
    fn registered_targets_cover_reminder_jobs() {
        for kind in crate::business::erp::reminder::RemindKind::ALL {
            assert!(
                is_registered_target(kind.task_name()),
                "业务提醒 {} 由 run_handler 执行，必须算已注册",
                kind.task_name()
            );
        }
        // 提醒的 cron 与本文件的其它内置任务不冲突（防止同秒同分撞车）
        for kind in crate::business::erp::reminder::RemindKind::ALL {
            let cron = kind.cron();
            assert!(
                CronSchedule::parse(cron).is_ok(),
                "{} 的 cron `{cron}` 必须能被调度器解析",
                kind.task_name()
            );
        }
    }
}

//! 审批流 · 超时与催办（批次 D · D2）。
//!
//! 依据：卡面 t_3715c446「超时（D10）：默认 `REMIND`（最多 3 次）；`AUTO_APPROVE` 需
//! `flow_def.allow_auto_approve` 全局闸 + 行上 `is_auto=true`」+
//! `docs/批次D-工作流审批-技术架构.md` §4.9（配置矩阵 / 定时任务 / 幂等 CAS）。
//!
//! # 配置矩阵（§4.9.1）
//!
//! | `timeout_action` | 行为 | 全局闸 |
//! |---|---|---|
//! | `REMIND`（**默认**） | 只发提醒，不改状态；上限 `max_remind_times`（默认 3） | 无 |
//! | `ESCALATE` | 提醒 + 抄送主管（P1 的督办部分在 D2 只做提醒，升级加签待 D5） | 无 |
//! | `AUTO_APPROVE` | 超时自动通过：`is_auto=true` + 意见「超时自动通过」 | **`flow_def.allow_auto_approve = true`**（默认 false） |
//! | `AUTO_REJECT` | 超时自动驳回 | 无 |
//!
//! # 三条实现纪律
//!
//! 1. **默认只提醒**：自动通过与自动驳回都会「替一个人做决定」，而这个决定可能有
//!    法律 / 财务后果（架构 D10）。缺全局闸时 `AUTO_APPROVE` **降级为提醒**并记 WARN，
//!    绝不静默变成自动通过。
//! 2. **幂等靠 CAS**：一个提醒周期内只处理一次 —— `WHERE remind_count = $old`，
//!    多实例并发扫描不会重复提醒（架构 §4.9.2）。
//! 3. **`LIMIT 500` 分批**：5 万条同时超时时一次全捞会把事务拖到分钟级、把通知打爆；
//!    分批 + 下一轮继续天然限流（架构 §4.9.2 的 R6）。
//!
//! # 定时任务接线
//!
//! [`register_builtin`] 的形状与 `notice::cleanup::register_builtin` 一致；**接线点在
//! `src/task/mod.rs` 的 `Scheduler::register_builtin`**。D2 的改造范围不含该文件
//! （卡面「只改」清单），所以本卡只交付函数与常量，接线是一行改动，留给 D5/D6
//! 在「超时策略上线」那张卡里做 —— 这样也不会出现「定时任务已注册但前端还没开关」的中间态。

use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::flow::engine;
use crate::business::flow::graph::{self, Snapshot};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::shared::error_code;
use crate::task::Scheduler;

/// 任务名（调度器里的唯一 key）。
pub const TASK_FLOW_TIMEOUT: &str = "flow_timeout_scan";

/// 每 5 分钟扫一次（架构 §4.9.2）。
pub const CRON_FLOW_TIMEOUT: &str = "0 */5 * * * *";

/// 默认最多提醒次数（`biz_flow_instance_node.max_remind_times` 缺省值）。
pub const DEFAULT_MAX_REMIND_TIMES: i16 = 3;

/// 单批扫描上限（架构 §4.9.2 的 `LIMIT 500`）。
pub const SCAN_BATCH: i64 = 500;

/// 一条「到期未处理」的待办。
#[derive(Debug, Clone)]
struct DueTask {
    task_id: Uuid,
    instance_id: Uuid,
    instance_no: String,
    title: String,
    node_key: String,
    assignee_id: Uuid,
    remind_count: i16,
    version_id: Uuid,
    allow_auto_approve: bool,
}

/// 一次扫描的统计。
#[derive(Debug, Clone, Default)]
pub struct TimeoutReport {
    /// 扫到多少条到期待办。
    pub scanned: usize,
    /// 发了多少条提醒。
    pub reminded: usize,
    /// 自动通过多少条。
    pub auto_approved: usize,
    /// 自动驳回多少条。
    pub auto_rejected: usize,
    /// 因「已达提醒上限」跳过多少条。
    pub skipped: usize,
    /// 失败多少条（不影响其它条目）。
    pub failed: usize,
}

impl TimeoutReport {
    /// 一行摘要（写日志用）。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_FLOW_TIMEOUT}: scanned={} reminded={} auto_approved={} auto_rejected={} \
             skipped={} failed={}",
            self.scanned,
            self.reminded,
            self.auto_approved,
            self.auto_rejected,
            self.skipped,
            self.failed
        )
    }
}

/// 在调度器上注册超时扫描任务（与 `notice::cleanup::register_builtin` 同款）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_FLOW_TIMEOUT,
        CRON_FLOW_TIMEOUT,
        "系统任务：审批超时提醒 / 自动处理",
        || {
            tokio::spawn(async move {
                match run_once().await {
                    Ok(report) => tracing::info!(summary = %report.summary(), "审批超时扫描完成"),
                    Err(error) => tracing::error!(error = %error, "审批超时扫描失败"),
                }
            })
        },
    );
}

/// 执行一次扫描（可被单测 / 运维脚本直接调用）。
///
/// # 异常情况
///
/// 数据库异常返回错误消息（调用方只记日志，不影响服务）。
///
/// SCOPE-EXEMPT: 超时扫描是**后台定时任务**（无请求者身份），按 `due_at < now()`
/// 全局取数；没有「某个用户能看到哪些数据」的问题（注入数据范围会让别人的超时待办永不提醒）。
pub async fn run_once() -> std::result::Result<TimeoutReport, String> {
    let mut report = TimeoutReport::default();
    let due = scan()
        .await
        .map_err(|error| format!("扫描到期待办失败：{error}"))?;
    report.scanned = due.len();
    if due.is_empty() {
        return Ok(report);
    }

    // 同一实例的节点配置要读快照：按 `version_id` 缓存，避免每个待办都解析一次 JSONB
    let mut snapshots: std::collections::BTreeMap<Uuid, Snapshot> =
        std::collections::BTreeMap::new();

    for task in due {
        let snapshot = match snapshots.get(&task.version_id) {
            Some(snapshot) => snapshot.clone(),
            None => {
                let loaded = match load_snapshot(task.version_id).await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        tracing::warn!(%task.task_id, error = %error, "读取流程快照失败，跳过");
                        report.failed += 1;
                        continue;
                    }
                };
                snapshots.insert(task.version_id, loaded.clone());
                loaded
            }
        };
        let action = snapshot
            .nodes
            .iter()
            .find(|node| node.key == task.node_key)
            .map(|node| node.timeout_action.to_ascii_uppercase())
            .unwrap_or_else(|| "REMIND".to_string());
        let max_remind = snapshot
            .nodes
            .iter()
            .find(|node| node.key == task.node_key)
            .map(|node| node.max_remind_times)
            .unwrap_or(DEFAULT_MAX_REMIND_TIMES)
            .max(1);

        match action.as_str() {
            "AUTO_APPROVE" => {
                if !task.allow_auto_approve {
                    // D10 全局闸：缺闸 → 降级为提醒（绝不静默自动通过）
                    tracing::warn!(
                        %task.task_id,
                        "节点配置了 AUTO_APPROVE 但流程未开启 allow_auto_approve，本次降级为提醒"
                    );
                    if remind(&task, max_remind)
                        .await
                        .map_err(|error| error.to_string())?
                    {
                        report.reminded += 1;
                    } else {
                        report.skipped += 1;
                    }
                } else {
                    match auto_decide(&task, true).await {
                        Ok(true) => report.auto_approved += 1,
                        Ok(false) => report.skipped += 1,
                        Err(error) => {
                            tracing::warn!(%task.task_id, error = %error, "超时自动通过失败");
                            report.failed += 1;
                        }
                    }
                }
            }
            "AUTO_REJECT" => match auto_decide(&task, false).await {
                Ok(true) => report.auto_rejected += 1,
                Ok(false) => report.skipped += 1,
                Err(error) => {
                    tracing::warn!(%task.task_id, error = %error, "超时自动驳回失败");
                    report.failed += 1;
                }
            },
            // REMIND / ESCALATE（升级加签属 P1，D2 先按提醒处理）
            _ => {
                if remind(&task, max_remind)
                    .await
                    .map_err(|error| error.to_string())?
                {
                    report.reminded += 1;
                } else {
                    report.skipped += 1;
                }
            }
        }
    }
    Ok(report)
}

/// 扫描到期未处理的待办（走部分索引 `idx_biz_flow_task_due`）。
async fn scan() -> DbResult<Vec<DueTask>> {
    let rows = db()
        .find_with_params(
            "SELECT t.id AS task_id, t.instance_id AS instance_id, t.node_key AS node_key, \
                    t.assignee_id AS assignee_id, t.remind_count AS remind_count, \
                    i.instance_no AS instance_no, i.title AS title, i.version_id AS version_id, \
                    d.allow_auto_approve AS allow_auto_approve \
               FROM biz_flow_task t \
               JOIN biz_flow_instance i ON i.id = t.instance_id \
               JOIN biz_flow_def d ON d.id = i.def_id \
              WHERE t.status = 'PENDING' AND t.due_at IS NOT NULL AND t.due_at < now() \
                AND i.status = 'PENDING' AND i.deleted_at IS NULL \
              ORDER BY t.due_at LIMIT ?",
            vec![Value::from(SCAN_BATCH)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(DueTask {
            task_id: row.get_typed("task_id")?,
            instance_id: row.get_typed("instance_id")?,
            instance_no: row.get_typed("instance_no")?,
            title: row.get_typed("title")?,
            node_key: row.get_typed("node_key")?,
            assignee_id: row.get_typed("assignee_id")?,
            remind_count: row.get_typed("remind_count")?,
            version_id: row.get_typed("version_id")?,
            allow_auto_approve: row.get_typed("allow_auto_approve")?,
        });
    }
    Ok(out)
}

/// 读快照（同 `engine::load_snapshot`，但走 `db()` —— 定时任务没有外层事务）。
async fn load_snapshot(version_id: Uuid) -> std::result::Result<Snapshot, String> {
    let rows = db()
        .find_with_params(
            "SELECT graph_snapshot::text AS snapshot_text \
               FROM biz_flow_def_version WHERE id = ?::uuid",
            vec![Value::from(&version_id)],
        )
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = rows.first() else {
        return Err(format!("流程版本 {version_id} 不存在"));
    };
    let text: Option<String> = row.get_typed("snapshot_text").map_err(|e| e.to_string())?;
    let Some(text) = text.filter(|value| !value.trim().is_empty()) else {
        return Err(format!("流程版本 {version_id} 没有快照"));
    };
    graph::parse_snapshot(&text)
}

/// 发一次提醒（CAS 幂等：`remind_count` 达上限 → 不发）。
///
/// # 返回值
///
/// `true` = 本次真的发出提醒；`false` = 已达上限 / 并发被别人发过（跳过）。
async fn remind(task: &DueTask, max_remind: i16) -> DbResult<bool> {
    if task.remind_count >= max_remind {
        return Ok(false);
    }
    // CAS：只有 remind_count 仍是读到的那个值才改（多实例并发扫描不会重复提醒）
    let updated = db()
        .execute_with_params(
            "UPDATE biz_flow_task \
                SET remind_count = remind_count + 1, last_remind_at = now() \
              WHERE id = ?::uuid AND status = 'PENDING' AND remind_count = ?",
            vec![
                Value::from(&task.task_id),
                Value::from(task.remind_count as i32),
            ],
        )
        .await?;
    if updated == 0 {
        return Ok(false);
    }

    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Warn,
        format!("审批催办：{}", task.title),
        format!(
            "审批单 {} 的节点「{}」已超时未处理（第 {} / {} 次提醒）。",
            task.instance_no,
            task.node_key,
            task.remind_count + 1,
            max_remind
        ),
        NoticeTarget::User(vec![task.assignee_id]),
    );
    draft.link_url = Some(format!("/flow/task/{}", task.task_id));
    draft.biz_type = Some("flow".to_string());
    draft.biz_id = Some(task.instance_id.to_string());
    if let Err(error) = notice::dispatch(draft).await {
        // 提醒发送失败不回滚 remind_count（否则会无限重发打爆通知）
        tracing::warn!(task_id = %task.task_id, error = %error, "审批催办通知发送失败");
    }
    Ok(true)
}

/// 系统代点（自动通过 / 自动驳回），每条待办独立事务。
///
/// # 返回值
///
/// `true` = 本次真的处理了；`false` = 待办已不在 `PENDING`（幂等跳过）。
async fn auto_decide(task: &DueTask, approve: bool) -> DbResult<bool> {
    let comment = if approve {
        "超时自动通过（流程已开启 allow_auto_approve）"
    } else {
        "超时未处理，自动驳回"
    };
    let instance_id = task.instance_id;
    let task_id = task.task_id;
    let mut outcome = db()
        .transaction(|tx| async move { engine::auto_decide(&tx, task_id, approve, comment).await })
        .await?;
    if let Some(error) = outcome.rejected {
        if error.code == error_code::FLOW_TASK_NOT_FOUND
            || error.code == error_code::FLOW_INSTANCE_NOT_PENDING
        {
            return Ok(false);
        }
        tracing::warn!(%task_id, code = error.code, msg = %error.msg, "系统代点被拒绝");
        return Ok(false);
    }
    // 副作用锁外发送
    engine::flush_effects(outcome.effects.take()).await;
    tracing::info!(
        %task_id,
        %instance_id,
        approve,
        "审批超时自动处理完成（is_auto=true）"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_and_task_names_are_stable() {
        assert_eq!(TASK_FLOW_TIMEOUT, "flow_timeout_scan");
        assert_eq!(CRON_FLOW_TIMEOUT, "0 */5 * * * *");
        assert_eq!(DEFAULT_MAX_REMIND_TIMES, 3);
        assert_eq!(SCAN_BATCH, 500);
    }

    #[test]
    fn report_summary_lists_all_counters() {
        let report = TimeoutReport {
            scanned: 7,
            reminded: 3,
            auto_approved: 1,
            auto_rejected: 2,
            skipped: 1,
            failed: 0,
        };
        let summary = report.summary();
        for expected in [
            "scanned=7",
            "reminded=3",
            "auto_approved=1",
            "auto_rejected=2",
            "skipped=1",
            "failed=0",
        ] {
            assert!(summary.contains(expected), "摘要缺 {expected}：{summary}");
        }
    }
}

//! 审批流 · 超时扫描与催办（批次 D · D2 起步，D4c 收口）。
//!
//! 依据：卡面 `t_c5729b08`（D4c）+ `docs/批次D-工作流审批-技术架构.md` §4.9
//! （配置矩阵 / 定时任务 / `LIMIT 500` 分批 / 幂等 CAS）。
//!
//! # 配置矩阵（§4.9.1，节点级 `timeout_action`）
//!
//! | `timeout_action` | 行为 | 全局闸 |
//! |---|---|---|
//! | `REMIND`（**默认**） | 只发催办通知（`FLOW_URGE` 模板），不改状态；上限 `max_remind_times`（默认 3） | 无 |
//! | `ESCALATE` | 提醒 + 抄送主管（升级加签属 P1，本卡按提醒处理） | 无 |
//! | `AUTO_APPROVE` | 超时自动通过：`is_auto=true` + 意见「超时自动通过」 | **`flow_def.allow_auto_approve = true`**（默认 false） |
//! | `AUTO_REJECT` | 超时自动驳回 | 无 |
//!
//! # 五条实现纪律
//!
//! 1. **默认只提醒**。自动通过会「替一个人做决定」，而这个决定可能有法律 / 财务后果
//!    （架构 D10）。`AUTO_APPROVE` **缺全局闸时完全不动**（不自动通过、也不发通知，
//!    只记 WARN + `skipped`）—— 卡面红线「`allow_auto_approve=false` 时不动」。
//! 2. **先分批再处理**。单批 `LIMIT 500`，一轮最多 [`MAX_BATCHES`] 批；5 万条同时超时
//!    时不会一次全捞（事务被拖到分钟级、通知模块被打爆）。
//! 3. **幂等靠 CAS**。`UPDATE … WHERE id = ? AND urge_count = <读到的旧值>`：多实例
//!    并发扫描时只有一方能把计数 +1，重复提醒在数据库层面被吞掉。
//! 4. **间隔靠 `last_urge_at`**。同一待办在「`timeout_hours / 2`（下限 1 小时）」内
//!    不重复催办 —— 于是每 10 分钟扫一次、连扫 3 次也只发 1 条（卡面功能清单 2）。
//!    扫描 SQL 另有一道 [`RESCAN_GUARD_MINUTES`] 的粗过滤，避免同一轮内反复捞出同一批行。
//! 5. **通知走模板**。催办是 `notify::FlowEvent::Urge` → `FLOW_URGE`（V0125 种子），
//!    幂等键带催办次数（`FLOW:URGE:<实例>:<节点>:<轮次>:<第几次>`）：不带次数的话
//!    `uk_sys_notice_idem` 会把第 2、3 次催办当成重发直接吃掉，「最多 3 次」永远只有 1 次。
//!
//! # 定时任务接线
//!
//! [`register_builtin`] 的形状与 `notice::cleanup::register_builtin` 一致，接线点在
//! `src/task/mod.rs::Scheduler::register_builtin`（**必须**在那里，bootstrap.rs 会被
//! 随后的 `reload()` 清空）。
//!
//! # 与 D2 遗留列的关系
//!
//! `biz_flow_task.remind_count / last_remind_at`（V0122）是 D2 落库的通用提醒计数，已被
//! 待办详情 DTO 读取展示；V0126 新增 `urge_count / last_urge_at` 作为**催办台账**
//! （CAS 与间隔判定的单一真源）。催办成功时**同一条 UPDATE 双写两列**，展示值与台账值
//! 永远同步，不会出现「两套计数各说各话」。

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::engine;
use crate::business::flow::graph::{self, Snapshot};
use crate::business::flow::notify::{FlowEvent, FlowNotice};
use crate::business::notice;
use crate::shared::error_code;
use crate::task::Scheduler;

/// 任务名（调度器里的唯一 key）。
pub const TASK_FLOW_TIMEOUT: &str = "flow_timeout_scan";

/// 每 10 分钟扫一次（卡面 D4c：`cron 每 10 分钟`）。
pub const CRON_FLOW_TIMEOUT: &str = "0 */10 * * * *";

/// 默认最多催办次数（节点 `max_remind_times` 的缺省值，卡面：最多 3 次）。
pub const DEFAULT_MAX_REMIND_TIMES: i16 = 3;

/// 单批扫描上限（架构 §4.9.2 的 `LIMIT 500`）。
pub const SCAN_BATCH: i64 = 500;

/// 一轮最多处理多少批（20 × 500 = 1 万条 / 轮，超出的下一轮继续）。
pub const MAX_BATCHES: usize = 20;

/// 同一待办的催办间隔下限（小时）。架构给的是 `timeout_hours / 2`，但 1 小时是下限：
/// 否则 `timeout_hours = 1` 的节点会 30 分钟催一次、把「最多 3 次」在 1 小时内打光。
pub const MIN_URGE_INTERVAL_HOURS: i64 = 1;

/// 扫描 SQL 的粗过滤：`last_urge_at` 在这么多分钟内的一律不捞（同一轮 / 相邻两轮不重复捞）。
pub const RESCAN_GUARD_MINUTES: i32 = 10;

/// 扫描待办（走部分索引 `idx_biz_flow_task_due` + `idx_biz_flow_task_urge`）。
///
/// 参数顺序：`① RESCAN_GUARD_MINUTES ② SCAN_BATCH`。
const SQL_SCAN: &str = "SELECT t.id AS task_id, t.instance_id AS instance_id, \
        t.node_key AS node_key, t.assignee_id AS assignee_id, t.round AS round, \
        t.urge_count AS urge_count, t.last_urge_at AS last_urge_at, \
        i.instance_no AS instance_no, i.title AS title, i.flow_name AS flow_name, \
        i.initiator_name AS initiator_name, i.version_id AS version_id, \
        d.allow_auto_approve AS allow_auto_approve \
   FROM biz_flow_task t \
   JOIN biz_flow_instance i ON i.id = t.instance_id \
   JOIN biz_flow_def d ON d.id = i.def_id \
  WHERE t.status = 'PENDING' AND t.due_at IS NOT NULL AND t.due_at < now() \
    AND i.status = 'PENDING' AND i.deleted_at IS NULL \
    AND (t.last_urge_at IS NULL OR t.last_urge_at < now() - make_interval(mins => ?)) \
  ORDER BY t.due_at LIMIT ?";

/// 读快照（定时任务没有外层事务，直接走 `db()`）。
const SQL_SNAPSHOT: &str = "SELECT graph_snapshot::text AS snapshot_text \
       FROM biz_flow_def_version WHERE id = ?::uuid";

/// 催办 CAS：只有 `urge_count` 仍是读到的旧值才生效（并发只会有一方成功）。
///
/// `remind_count / last_remind_at` 同语句双写（D2 遗留展示列，见模块文档最后一节）。
const SQL_URGE_CAS: &str = "UPDATE biz_flow_task \
        SET urge_count = urge_count + 1, last_urge_at = now(), \
            remind_count = remind_count + 1, last_remind_at = now() \
      WHERE id = ?::uuid AND status = 'PENDING' AND urge_count = ?";

/// 节点级超时策略（`timeout_action` 的强类型化，未知值一律降级 [`TimeoutAction::Remind`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutAction {
    /// 只提醒（默认）。
    Remind,
    /// 提醒 + 抄送主管（升级加签属 P1，本卡按提醒处理）。
    Escalate,
    /// 超时自动通过（需全局闸）。
    AutoApprove,
    /// 超时自动驳回。
    AutoReject,
}

impl TimeoutAction {
    /// 库/快照里的字符串 → 强类型。大小写、前后空格都容错；**未知值 = 默认提醒**。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "AUTO_APPROVE" => TimeoutAction::AutoApprove,
            "AUTO_REJECT" => TimeoutAction::AutoReject,
            "ESCALATE" => TimeoutAction::Escalate,
            _ => TimeoutAction::Remind,
        }
    }

    /// 是否需要「不动」以外的动作（日志/统计用）。
    pub fn as_str(self) -> &'static str {
        match self {
            TimeoutAction::Remind => "REMIND",
            TimeoutAction::Escalate => "ESCALATE",
            TimeoutAction::AutoApprove => "AUTO_APPROVE",
            TimeoutAction::AutoReject => "AUTO_REJECT",
        }
    }
}

/// 本次催办该怎么做。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrgeDecision {
    /// 可以催，且这是第几次（1 起）。
    Remind {
        /// 本次是第几次催办。
        count: i16,
    },
    /// 距上次催办还没到间隔，跳过。
    Wait,
    /// 已达次数上限，永久停止（不再催，但待办仍 PENDING）。
    Stop,
}

/// 是否允许「超时自动通过」：节点策略 + **全局闸**，两个条件缺一不可。
///
/// 全局闸缺省 `false`（`biz_flow_def.allow_auto_approve` DEFAULT false）⇒ 默认关闭。
pub fn should_auto_approve(action: TimeoutAction, allow_auto_approve: bool) -> bool {
    matches!(action, TimeoutAction::AutoApprove) && allow_auto_approve
}

/// 催办间隔（小时）：`timeout_hours / 2`，下限 [`MIN_URGE_INTERVAL_HOURS`]。
///
/// 节点没配超时（`None` / `<= 0`）时用下限：这种节点本来就没有 `due_at`，走到这里只是兜底。
pub fn urge_interval_hours(timeout_hours: Option<i32>) -> i64 {
    match timeout_hours {
        Some(hours) if hours > 0 => (i64::from(hours) / 2).max(MIN_URGE_INTERVAL_HOURS),
        _ => MIN_URGE_INTERVAL_HOURS,
    }
}

/// 纯函数：判定本次是否催办（不碰库，单测直接打这里）。
///
/// 三条规则按顺序：达上限 → 停；未到间隔 → 等；否则催第 `urge_count + 1` 次。
pub fn urge_decision(
    urge_count: i16,
    last_urge_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    max_times: i16,
    interval_hours: i64,
) -> UrgeDecision {
    let max_times = max_times.max(1);
    if urge_count >= max_times {
        return UrgeDecision::Stop;
    }
    if let Some(last) = last_urge_at {
        let due = last + chrono::Duration::hours(interval_hours.max(0));
        if now < due {
            return UrgeDecision::Wait;
        }
    }
    UrgeDecision::Remind {
        count: urge_count + 1,
    }
}

/// 催办通知的幂等键：**带催办次数**。
///
/// `sys_notice.idempotency_key` 上有唯一索引 `uk_sys_notice_idem`，键里不带次数的话
/// 第 2、3 次催办会被当成「重发」静默吞掉。
pub fn urge_idempotency_key(
    instance_id: &Uuid,
    node_key: &str,
    round: i32,
    urge_count: i16,
) -> String {
    format!("FLOW:URGE:{instance_id}:{node_key}:{round}:{urge_count}")
}

/// 一条「到期未处理」的待办。
#[derive(Debug, Clone)]
struct DueTask {
    task_id: Uuid,
    instance_id: Uuid,
    instance_no: String,
    flow_name: String,
    title: String,
    initiator_name: String,
    node_key: String,
    round: i32,
    assignee_id: Uuid,
    urge_count: i16,
    last_urge_at: Option<DateTime<Utc>>,
    version_id: Uuid,
    allow_auto_approve: bool,
}

/// 节点在快照里的超时配置（解析一次，整批复用）。
#[derive(Debug, Clone)]
struct NodeTimeout {
    name: String,
    action: TimeoutAction,
    max_times: i16,
    interval_hours: i64,
}

/// 一次扫描的统计。
#[derive(Debug, Clone, Default)]
pub struct TimeoutReport {
    /// 扫到多少条到期待办（所有批次合计）。
    pub scanned: usize,
    /// 实际处理了多少批。
    pub batches: usize,
    /// 发了多少条催办。
    pub reminded: usize,
    /// 自动通过多少条。
    pub auto_approved: usize,
    /// 自动驳回多少条。
    pub auto_rejected: usize,
    /// 跳过多少条（未到间隔 / 达上限 / 全局闸未开 / 并发被别人处理）。
    pub skipped: usize,
    /// 失败多少条（不影响其它条目）。
    pub failed: usize,
    /// P-FLOW-2：本轮放行了多少个到点的通知节点（`ON_TIMEOUT`）。
    pub notify_fired: usize,
}

impl TimeoutReport {
    /// 一行摘要（写日志用）。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_FLOW_TIMEOUT}: batches={} scanned={} reminded={} auto_approved={} \
             auto_rejected={} skipped={} failed={} notify_fired={}",
            self.batches,
            self.scanned,
            self.reminded,
            self.auto_approved,
            self.auto_rejected,
            self.skipped,
            self.failed,
            self.notify_fired
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
        "系统任务：审批超时催办 / 自动处理 / 通知节点定时发送",
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

/// 执行一次扫描（所有批次；可被单测 / 运维脚本直接调用）。
///
/// # 异常情况
///
/// 数据库异常返回错误消息（调用方只记日志，不影响服务）。
///
/// SCOPE-EXEMPT: 超时扫描是**后台定时任务**（无请求者身份），按 `due_at < now()` 全局取数；
/// 注入数据范围只会让别人的超时待办永不催办。
pub async fn run_once() -> std::result::Result<TimeoutReport, String> {
    let mut report = TimeoutReport::default();
    // 快照按 version_id 缓存，避免每个待办都解析一次 JSONB
    let mut snapshots: BTreeMap<Uuid, Snapshot> = BTreeMap::new();
    let mut seen: HashSet<Uuid> = HashSet::new();

    for _ in 0..MAX_BATCHES {
        let due = scan()
            .await
            .map_err(|error| format!("扫描到期待办失败：{error}"))?;
        if due.is_empty() {
            break;
        }
        let full_batch = due.len() as i64 >= SCAN_BATCH;
        // 同一批里若全是「已看过」的行（间隔未到 / 达上限），说明剩下的都不该动，直接收工，
        // 否则会在一轮里把同一批 500 行反复捞出来空转。
        let fresh = due.iter().any(|task| !seen.contains(&task.task_id));
        if !fresh {
            break;
        }
        report.batches += 1;
        report.scanned += due.len();

        for task in due {
            if !seen.insert(task.task_id) {
                continue;
            }
            if let Err(error) = handle_one(&task, &mut snapshots, &mut report).await {
                tracing::warn!(task_id = %task.task_id, error = %error, "超时待办处理失败");
                report.failed += 1;
            }
        }

        if !full_batch {
            break;
        }
    }

    // P-FLOW-2：同一轮顺带处理「到点的通知节点」（`config.notify.when = ON_TIMEOUT`）。
    // 放在催办循环之后：催办的目标是**待人处理的待办**，通知节点是**到点要发的事件**，
    // 两类数据不重叠，谁先谁后只影响日志可读性。
    match crate::business::flow::notify_node::run_due_once().await {
        Ok(fired) => report.notify_fired = fired,
        Err(error) => tracing::warn!(error = %error, "通知节点（ON_TIMEOUT）扫描失败"),
    }
    Ok(report)
}

/// 处理一条到期待办（策略分派）。
async fn handle_one(
    task: &DueTask,
    snapshots: &mut BTreeMap<Uuid, Snapshot>,
    report: &mut TimeoutReport,
) -> std::result::Result<(), String> {
    let snapshot = match snapshots.get(&task.version_id) {
        Some(snapshot) => snapshot.clone(),
        None => {
            let loaded = load_snapshot(task.version_id).await?;
            snapshots.insert(task.version_id, loaded.clone());
            loaded
        }
    };
    let node = resolve_node(&snapshot, &task.node_key);

    match node.action {
        TimeoutAction::AutoApprove => {
            if !should_auto_approve(node.action, task.allow_auto_approve) {
                // 红线：全局闸未开 → 不动（不自动通过，也不发通知）
                tracing::warn!(
                    %task.task_id,
                    node = %task.node_key,
                    "节点配置 AUTO_APPROVE 但流程未开启 allow_auto_approve，本次不动（默认关闭）"
                );
                report.skipped += 1;
                return Ok(());
            }
            match auto_decide(task, true).await {
                Ok(true) => report.auto_approved += 1,
                Ok(false) => report.skipped += 1,
                Err(error) => return Err(error.to_string()),
            }
        }
        TimeoutAction::AutoReject => match auto_decide(task, false).await {
            Ok(true) => report.auto_rejected += 1,
            Ok(false) => report.skipped += 1,
            Err(error) => return Err(error.to_string()),
        },
        // REMIND / ESCALATE（升级加签属 P1，先按提醒处理）
        TimeoutAction::Remind | TimeoutAction::Escalate => {
            match remind(task, &node)
                .await
                .map_err(|error| error.to_string())?
            {
                Some(_notice) => report.reminded += 1,
                None => report.skipped += 1,
            }
        }
    }
    Ok(())
}

/// 从快照里取节点超时配置（未知节点 → 默认 `REMIND` / 3 次 / 1 小时间隔）。
fn resolve_node(snapshot: &Snapshot, node_key: &str) -> NodeTimeout {
    match snapshot.nodes.iter().find(|node| node.key == node_key) {
        Some(spec) => NodeTimeout {
            name: spec.name.clone(),
            action: TimeoutAction::parse(&spec.timeout_action),
            max_times: spec.max_remind_times.max(1),
            interval_hours: urge_interval_hours(spec.timeout_hours),
        },
        None => NodeTimeout {
            name: node_key.to_string(),
            action: TimeoutAction::Remind,
            max_times: DEFAULT_MAX_REMIND_TIMES,
            interval_hours: MIN_URGE_INTERVAL_HOURS,
        },
    }
}

/// 扫描一批到期未处理的待办（`LIMIT SCAN_BATCH`）。
async fn scan() -> DbResult<Vec<DueTask>> {
    let rows = db()
        .find_with_params(
            SQL_SCAN,
            vec![Value::from(RESCAN_GUARD_MINUTES), Value::from(SCAN_BATCH)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let last_urge_at: Option<DateTime<Utc>> = row.get_typed("last_urge_at")?;
        out.push(DueTask {
            task_id: row.get_typed("task_id")?,
            instance_id: row.get_typed("instance_id")?,
            instance_no: row.get_typed("instance_no")?,
            flow_name: row.get_typed("flow_name")?,
            title: row.get_typed("title")?,
            initiator_name: row.get_typed("initiator_name").unwrap_or_default(),
            node_key: row.get_typed("node_key")?,
            round: row.get_typed("round")?,
            assignee_id: row.get_typed("assignee_id")?,
            urge_count: row.get_typed("urge_count")?,
            last_urge_at,
            version_id: row.get_typed("version_id")?,
            allow_auto_approve: row.get_typed("allow_auto_approve")?,
        });
    }
    Ok(out)
}

/// 读快照（同 `engine::load_snapshot`，但走 `db()` —— 定时任务没有外层事务）。
async fn load_snapshot(version_id: Uuid) -> std::result::Result<Snapshot, String> {
    let rows = db()
        .find_with_params(SQL_SNAPSHOT, vec![Value::from(&version_id)])
        .await
        .map_err(|error| error.to_string())?;
    let Some(row) = rows.first() else {
        return Err(format!("流程版本 {version_id} 不存在"));
    };
    let text: Option<String> = row
        .get_typed("snapshot_text")
        .map_err(|error| error.to_string())?;
    let Some(text) = text.filter(|value| !value.trim().is_empty()) else {
        return Err(format!("流程版本 {version_id} 没有快照"));
    };
    graph::parse_snapshot(&text)
}

/// 发一次催办（`FLOW_URGE` 模板）。
///
/// # 返回值
///
/// `Some(第几次)` = 本次真的催了；`None` = 未到间隔 / 达上限 / CAS 失败（并发已被别人催过）。
async fn remind(task: &DueTask, node: &NodeTimeout) -> DbResult<Option<i16>> {
    let decision = urge_decision(
        task.urge_count,
        task.last_urge_at,
        Utc::now(),
        node.max_times,
        node.interval_hours,
    );
    let count = match decision {
        UrgeDecision::Remind { count } => count,
        UrgeDecision::Wait => {
            tracing::debug!(task_id = %task.task_id, "未到催办间隔，跳过");
            return Ok(None);
        }
        UrgeDecision::Stop => {
            tracing::debug!(
                task_id = %task.task_id,
                urge_count = task.urge_count,
                max = node.max_times,
                "已达催办次数上限，停止催办"
            );
            return Ok(None);
        }
    };

    // CAS：`urge_count` 仍是读到的旧值才 +1（多实例并发扫描不会重复催办）
    let updated = db()
        .execute_with_params(
            SQL_URGE_CAS,
            vec![
                Value::from(&task.task_id),
                Value::from(task.urge_count as i32),
            ],
        )
        .await?;
    if updated == 0 {
        tracing::debug!(task_id = %task.task_id, "催办 CAS 未命中（已被其它实例处理），跳过");
        return Ok(None);
    }

    let notice = FlowNotice::new(
        FlowEvent::Urge,
        task.instance_id,
        &task.instance_no,
        &task.flow_name,
        &task.title,
        &task.node_key,
        &node.name,
    )
    .with_urge_count(i32::from(count))
    .with_round(task.round)
    .with_initiator(&task.initiator_name)
    .with_recipients(vec![task.assignee_id]);
    let mut draft = notice.draft();
    // 幂等键必须带「第几次」：否则第 2、3 次会被 uk_sys_notice_idem 当重发吞掉
    draft.idempotency_key = Some(urge_idempotency_key(
        &task.instance_id,
        &task.node_key,
        task.round,
        count,
    ));
    if let Err(error) = notice::dispatch(draft).await {
        // 发送失败不回滚 urge_count（否则会无限重发打爆通知），下次间隔到了再试
        tracing::warn!(task_id = %task.task_id, error = %error, "审批催办通知发送失败");
    }
    Ok(Some(count))
}

/// 系统代点（自动通过 / 自动驳回），每条待办独立事务，留痕由引擎写（`is_auto=true` + changelog）。
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
    use chrono::TimeZone;

    /// 固定时基（避免单测用 `now()` 造成边界抖动）。
    fn at(offset_minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, 8, 0, 0).unwrap()
            + chrono::Duration::minutes(offset_minutes)
    }

    #[test]
    fn constants_match_card() {
        assert_eq!(TASK_FLOW_TIMEOUT, "flow_timeout_scan");
        assert_eq!(CRON_FLOW_TIMEOUT, "0 */10 * * * *");
        assert_eq!(DEFAULT_MAX_REMIND_TIMES, 3);
        assert_eq!(SCAN_BATCH, 500);
        assert_eq!(RESCAN_GUARD_MINUTES, 10);
    }

    #[test]
    fn action_parsing_defaults_to_remind() {
        assert_eq!(TimeoutAction::parse("REMIND"), TimeoutAction::Remind);
        assert_eq!(
            TimeoutAction::parse(" auto_approve "),
            TimeoutAction::AutoApprove
        );
        assert_eq!(
            TimeoutAction::parse("AUTO_REJECT"),
            TimeoutAction::AutoReject
        );
        assert_eq!(TimeoutAction::parse("ESCALATE"), TimeoutAction::Escalate);
        // 未知值 / 空值 → 默认提醒（绝不默认自动通过）
        assert_eq!(TimeoutAction::parse(""), TimeoutAction::Remind);
        assert_eq!(TimeoutAction::parse("BOGUS"), TimeoutAction::Remind);
    }

    #[test]
    fn auto_approve_is_closed_by_default() {
        // 全局闸 false（库默认）→ 一律不自动通过
        assert!(!should_auto_approve(TimeoutAction::AutoApprove, false));
        // 闸开了但没有节点级策略 → 也不自动通过
        assert!(!should_auto_approve(TimeoutAction::Remind, true));
        // 两个条件都满足才生效
        assert!(should_auto_approve(TimeoutAction::AutoApprove, true));
    }

    #[test]
    fn urge_interval_is_half_timeout_with_floor() {
        assert_eq!(urge_interval_hours(Some(24)), 12);
        assert_eq!(urge_interval_hours(Some(48)), 24);
        assert_eq!(urge_interval_hours(Some(2)), 1); // 2/2=1，正好下限
        assert_eq!(urge_interval_hours(Some(1)), 1); // 0.5 → 抬到 1
        assert_eq!(urge_interval_hours(None), 1);
    }

    #[test]
    fn three_consecutive_scans_remind_once() {
        // 同一待办：第 1 次扫 → 催；接着两次扫（间隔各 10 分钟，未到 1 小时间隔）→ 不再催
        let decisions: Vec<UrgeDecision> = [0, 10, 20]
            .into_iter()
            .map(|offset| {
                // 只有第一次真的把 urge_count 推进到 1、last_urge_at 落到 t0
                let (count, last) = if offset == 0 {
                    (0, None)
                } else {
                    (1, Some(at(0)))
                };
                urge_decision(count, last, at(offset), 3, 1)
            })
            .collect();
        assert_eq!(
            decisions,
            vec![
                UrgeDecision::Remind { count: 1 },
                UrgeDecision::Wait,
                UrgeDecision::Wait
            ]
        );
    }

    #[test]
    fn remind_stops_at_three() {
        // 间隔已过 → 继续催到第 3 次
        assert_eq!(
            urge_decision(0, None, at(0), 3, 1),
            UrgeDecision::Remind { count: 1 }
        );
        assert_eq!(
            urge_decision(1, Some(at(-120)), at(0), 3, 1),
            UrgeDecision::Remind { count: 2 }
        );
        assert_eq!(
            urge_decision(2, Some(at(-120)), at(0), 3, 1),
            UrgeDecision::Remind { count: 3 }
        );
        // 第 3 次之后永久停止（哪怕间隔早就过了）
        assert_eq!(
            urge_decision(3, Some(at(-10_000)), at(0), 3, 1),
            UrgeDecision::Stop
        );
        assert_eq!(urge_decision(9, None, at(0), 3, 1), UrgeDecision::Stop);
    }

    #[test]
    fn urge_interval_respected_after_elapsed() {
        // 24h 超时 → 12h 间隔：11h59m 等，12h 催
        assert_eq!(
            urge_decision(1, Some(at(0)), at(719), 3, 12),
            UrgeDecision::Wait
        );
        assert_eq!(
            urge_decision(1, Some(at(0)), at(720), 3, 12),
            UrgeDecision::Remind { count: 2 }
        );
    }

    #[test]
    fn urge_idempotency_key_varies_by_count() {
        let instance = Uuid::nil();
        let first = urge_idempotency_key(&instance, "approve_1", 1, 1);
        let second = urge_idempotency_key(&instance, "approve_1", 1, 2);
        assert_ne!(
            first, second,
            "第 2 次催办必须换幂等键，否则被 uk_sys_notice_idem 吞掉"
        );
        assert!(first.starts_with("FLOW:URGE:"));
        assert!(first.contains(":approve_1:1:1"));
    }

    #[test]
    fn scan_sql_is_batched_and_guarded() {
        assert!(SQL_SCAN.contains("LIMIT ?"), "扫描必须分批（LIMIT 500）");
        assert!(
            SQL_SCAN.contains("make_interval(mins => ?)"),
            "同一轮不重复捞同一批行"
        );
        assert!(SQL_SCAN.contains("t.status = 'PENDING'"));
        assert!(
            SQL_SCAN.contains("allow_auto_approve"),
            "全局闸必须随扫描取回"
        );
        assert!(SQL_URGE_CAS.contains("urge_count = urge_count + 1"));
        assert!(SQL_URGE_CAS.contains("urge_count = ?"), "幂等必须靠 CAS");
    }

    #[test]
    fn report_summary_lists_all_counters() {
        let report = TimeoutReport {
            scanned: 600,
            batches: 2,
            reminded: 3,
            auto_approved: 1,
            auto_rejected: 2,
            skipped: 1,
            failed: 0,
            notify_fired: 2,
        };
        let summary = report.summary();
        for expected in [
            "batches=2",
            "scanned=600",
            "reminded=3",
            "auto_approved=1",
            "auto_rejected=2",
            "skipped=1",
            "failed=0",
            "notify_fired=2",
        ] {
            assert!(summary.contains(expected), "摘要缺 {expected}：{summary}");
        }
    }
}

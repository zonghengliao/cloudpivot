//! 可编辑审批流 · 维护侧（批次 D · D4d）：`graph_hash` 每日自检。
//!
//! 依据：卡面 `t_80edbf3e`；`docs/批次D-工作流审批-技术架构.md` §1.5（调度器接线点）/
//! §6.5（定时任务元数据 `flow_graph_selfcheck` / `0 30 3 * * *`）/ §X5（全库零触发器）/
//! §10 风险 R1（`graph_snapshot` 与行数据不一致）。
//!
//! # 为什么需要这个自检
//!
//! 本库**零触发器**（既有约定，`pg_dump -s` 里 `CREATE TRIGGER` 计数 = 0），而流程引擎
//! **不 JOIN 行**、只读 `graph_snapshot`（L2）。于是「发布后不可改」这条不变式在库里
//! 没有任何强制手段：有人 `UPDATE biz_flow_node SET ...` 直接改库，正在跑的实例不会被
//! 拦下，却会在**下一次**从行重建快照（或页面回显）时走岔路——这是最隐蔽的版本漂移
//! （比换版本隐蔽得多，因为行是「可改」的）。
//!
//! 能发现它的唯一办法就是**对账**：
//!
//! ```text
//! 行（L1） --load_graph--> FlowGraph --snapshot()->snapshot_json()->sha256--> 重算 hash
//!                                                                              ↕ 比对
//!                                     biz_flow_def_version.graph_hash（发布时写死）
//! ```
//!
//! 两条判据、两个告警类别：
//!
//! | 类别 | 谁能造出来 | 判据 |
//! |------|-----------|------|
//! | [`DriftKind::RowsEdited`] | 直接改 `biz_flow_node` / `biz_flow_edge` / `biz_flow_node_assignee` | `hash(行)` ≠ `version.graph_hash` |
//! | [`DriftKind::SnapshotEdited`] | 直接改 `version.graph_snapshot`（行没动） | `hash(snapshot)` ≠ `version.graph_hash` |
//!
//! # 三条口径（写代码前定死，避免「同一张图算出两个 hash」）
//!
//! 1. **重算只用 [`FlowGraph::graph_hash`]**，即 `snapshot()`（确定性排序）→
//!    `snapshot_json()`（递归按键升序的 canonical JSON）→ sha256。任何自己拼 JSON 的
//!    写法都会让正常数据天天报警（验收「正常数据不报警」会当场挂掉）。
//! 2. **快照侧走 [`graph::canonical_json`]**：`graph_snapshot::text` 是 **jsonb**，
//!    PG 会按「键长度 + 字节序」重排键并加 `", "` 空格，逐字节比对必假警——先 parse 回
//!    `Value` 再 canonical 化，与发布时那份逐字节相同。
//! 3. **只读，不修**：本模块**绝不**回写 `graph_hash` 或快照。自检发现不一致说明
//!    「有人越过应用层改了库」，正确的处置是人工判断（行对还是快照对），自动「修好」
//!    等于把证据抹掉。所以这里只有 WARN + 审计。
//!
//! # 分批处理
//!
//! 版本多时**不一次性加载**：按主键 keyset 分页（`v.id > ?` 升序，[`SCAN_BATCH`] 条 / 批，
//! 每批内逐版本处理完即可释放），一轮最多 [`MAX_BATCHES`] 批（200 × 50 = 1 万个版本）。
//! 注意 keyset 用 `v.id` 而非 `version_no`：后者在 def 内唯一、全局重复，翻页会漏。
//!
//! # 核对范围：所有「发布过」的版本（`PUBLISHED` + `ARCHIVED`）
//!
//! 卡面写的是「扫描所有 PUBLISHED 版本」。落地时发现它**不足以覆盖风险**：发布新版本时
//! [`model::apply_publish`] 会把旧的 `PUBLISHED` 降为 `ARCHIVED`，所以任一时刻每个流程
//! 至多 1 行 `PUBLISHED` —— 只扫 PUBLISHED 等于「每个流程只核对当前生效版本」，而**在跑的
//! 实例钉的是它发起时的那个版本**（很可能已经是 ARCHIVED）。旧版本的行被直接改，同样会让
//! 「行 ↔ 快照」失配。因此扫描条件取 `version_status IN ('PUBLISHED','ARCHIVED')`：
//! 它是卡面要求的**超集**（PUBLISHED 一条不少），且 ARCHIVED 只在「发布过」之后出现，
//! 快照与 `graph_hash` 都在。DRAFT 没有任何 hash 可对账，天然被排除。
//!
//! # 为什么不用 `sys_change_log` 而用 `audit_logs`
//!
//! 变更日志（`business::changelog`）的语义是「**某个操作人**改了某行」，写入前要过白名单
//! 登记表、脱敏规则，并且 diff 必须是真实的前后值。自检告警不是一次数据变更（我们**没有**
//! 改动任何行），硬塞进去等于伪造一条操作记录、还会污染「谁改的」这条最关键的信息。
//! 所以落 `audit_logs`（`operation = FLOW_GRAPH_HASH_DRIFT`，`username = system`）：
//! 它是审计表，允许记录「系统观察到的事实」，且按时间清理，与其它审计查询同一入口可见。
//!
//! # 与其它模块的边界
//!
//! - 只读 `biz_flow_def` / `biz_flow_def_version` / `biz_flow_node` / `biz_flow_edge` /
//!   `biz_flow_node_assignee`，写 `audit_logs`。零迁移（表都已在 `V0120` / `V0014`）。
//! - 不注册接口（卡面「接口 0」）：触发点只有调度器，运维要手动跑就调 [`run_once`]。
//! - 实例编号回收属架构 §1.5 同一文件、但**不在本卡范围**（卡面只要求 graph_hash 自检）。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::graph;
use crate::business::flow::model;
use crate::support::log::model::AuditLogModel;
use crate::task::Scheduler;

/// 任务名（调度器里的唯一 key，与 `sys_job.invoke_target` 一致）。
pub const TASK_FLOW_GRAPH_SELFCHECK: &str = "flow_graph_selfcheck";

/// 每天 03:30 跑一次（架构 §6.5 登记的同一条 cron）。
pub const CRON_FLOW_GRAPH_SELFCHECK: &str = "0 30 3 * * *";

/// 单批版本数（分批处理的粒度；每批处理完才取下一批）。
pub const SCAN_BATCH: i64 = 200;

/// 一轮最多处理多少批（200 × 50 = 1 万个版本 / 轮）。
pub const MAX_BATCHES: usize = 50;

/// 审计动作码（`audit_logs.operation`，长度 ≤ 50）。
pub const AUDIT_OPERATION: &str = "FLOW_GRAPH_HASH_DRIFT";

/// 审计对象类型（`audit_logs.target_type`，与其它流程审计保持同一套命名）。
pub const AUDIT_TARGET_TYPE: &str = "FlowDefVersion";

/// 审计里的操作者名（自检没有请求者，如实记成 `system`）。
pub const AUDIT_OPERATOR: &str = "system";

/// 扫描第一页（不带 keyset 游标）。
///
/// 参数顺序：`① LIMIT`。
///
/// # 为什么不止 `PUBLISHED`（与卡面字面的差异，已写进交付说明）
///
/// 发布一个新版本时 [`model::apply_publish`] 会把**旧的 PUBLISHED 降成 `ARCHIVED`**，因此
/// 「`status = 'PUBLISHED'`」在任何时刻**每个流程最多只有 1 行**——只扫 PUBLISHED 等于只核对
/// 「当前生效版本」，而**在跑的实例钉的是发布时的那个版本**（可能是 ARCHIVED），旧版本的
/// 行被直接改同样会让「行 ↔ 快照」失配。所以口径取「所有**发布过**的版本」：
/// `version_status IN ('PUBLISHED','ARCHIVED')`（`ARCHIVED` 只在发布过之后出现，快照与
/// `graph_hash` 都还在）。DRAFT 天然被排除（它们没有 hash 可对账）。
const SQL_SCAN_FIRST: &str = "SELECT v.id AS id, v.def_id AS def_id, v.version_no AS version_no, \
        v.graph_hash AS graph_hash, d.flow_code AS flow_code \
   FROM biz_flow_def_version v \
   JOIN biz_flow_def d ON d.id = v.def_id \
  WHERE v.version_status IN ('PUBLISHED', 'ARCHIVED') \
  ORDER BY v.id \
  LIMIT ?";

/// 扫描后续页（keyset：`v.id > 上一批最后一个 id`）。
///
/// 参数顺序：`① after_id（uuid）② LIMIT`。**不能**用 `Value::Null` 表达「没有游标」——
/// uuid 位绑 NULL 会 `error serializing parameter`，所以首尾两页分成两条常量 SQL。
const SQL_SCAN_NEXT: &str = "SELECT v.id AS id, v.def_id AS def_id, v.version_no AS version_no, \
        v.graph_hash AS graph_hash, d.flow_code AS flow_code \
   FROM biz_flow_def_version v \
   JOIN biz_flow_def d ON d.id = v.def_id \
  WHERE v.version_status IN ('PUBLISHED', 'ARCHIVED') AND v.id > ?::uuid \
  ORDER BY v.id \
  LIMIT ?";

/// 不一致的类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftKind {
    /// 行被改（`node/edge/assignee` 与发布时算出的 hash 对不上）。
    RowsEdited,
    /// 快照被改（行没动，`graph_snapshot` 被直接写）。
    SnapshotEdited,
}

impl DriftKind {
    /// 审计 / 日志里的机器可读码。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RowsEdited => "ROWS_EDITED",
            Self::SnapshotEdited => "SNAPSHOT_EDITED",
        }
    }

    /// 中文说明（直接进审计 `detail`，审计员不需要查代码）。
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::RowsEdited => "发布版本的 node/edge 行被直接修改，与发布时的 graph_hash 不一致",
            Self::SnapshotEdited => {
                "发布版本的 graph_snapshot 被直接修改（行未变），与发布时的 graph_hash 不一致"
            }
        }
    }
}

/// 一条不一致记录（自检结论的载体：日志 / 审计 / 返回值三处同一份数据）。
#[derive(Debug, Clone)]
pub struct Drift {
    /// 版本 ID（`biz_flow_def_version.id`）。
    pub version_id: Uuid,
    /// 定义 ID。
    pub def_id: Uuid,
    /// 流程编码（审计里给人看的标识）。
    pub flow_code: String,
    /// 版本号。
    pub version_no: i32,
    /// 类别。
    pub kind: DriftKind,
    /// 库里记的 hash（发布时写死的那份）。
    pub stored_hash: String,
    /// 从行重算出的 hash（`RowsEdited` 时是行的 hash，`SnapshotEdited` 时等于 `stored_hash`）。
    pub recomputed_hash: String,
    /// 快照自身的 hash（`None` = PUBLISHED 但快照为空，理论上被 CHECK 拦住）。
    pub snapshot_hash: Option<String>,
}

/// 一轮自检的统计。
#[derive(Debug, Clone, Default)]
pub struct SelfCheckReport {
    /// 实际处理了多少批。
    pub batches: usize,
    /// 核对了多少个 PUBLISHED 版本。
    pub versions: usize,
    /// 不一致的版本（逐条，已写审计的也在这里）。
    pub drifted: Vec<Drift>,
    /// PUBLISHED 却缺 `graph_hash` 的版本数（`ck_biz_flow_def_version_published` 说不该有）。
    pub missing_hash: usize,
    /// 读行失败（脏枚举 / 非法 jsonb）的版本数。
    pub failed: usize,
    /// 审计写入失败的条数（日志报警照发，不影响其它版本）。
    pub audit_failed: usize,
}

impl SelfCheckReport {
    /// 一行摘要（写日志用）。
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{TASK_FLOW_GRAPH_SELFCHECK}: batches={} versions={} drifted={} missing_hash={} \
             failed={} audit_failed={}",
            self.batches,
            self.versions,
            self.drifted.len(),
            self.missing_hash,
            self.failed,
            self.audit_failed
        )
    }

    /// 是否全部一致（正常数据 → `true`，检查项「正常数据不报警」）。
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.drifted.is_empty() && self.missing_hash == 0 && self.failed == 0
    }
}

/// 单个版本的检查结论。
enum Outcome {
    /// 一致。
    Clean,
    /// PUBLISHED 但没有 `graph_hash`（库侧 CHECK 的漏网之鱼）。
    MissingHash,
    /// 不一致。
    Drift {
        /// 类别。
        kind: DriftKind,
        /// 从行重算的 hash。
        rows_hash: String,
        /// 快照自身的 hash。
        snapshot_hash: Option<String>,
    },
}

/// 在调度器上注册自检任务（与 `flow::timeout::register_builtin` 同款形状）。
///
/// # 参数
///
/// - `scheduler`：全局调度器。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_FLOW_GRAPH_SELFCHECK,
        CRON_FLOW_GRAPH_SELFCHECK,
        "系统任务：审批图快照自检（graph_hash 与 node/edge 行核对）",
        || {
            tokio::spawn(async move {
                match run_once().await {
                    Ok(report) if report.is_clean() => {
                        tracing::info!(summary = %report.summary(), "审批图快照自检完成：全部一致")
                    }
                    Ok(report) => {
                        // 逐条 WARN 已在 check_one 里打过，这里只补一行汇总
                        tracing::warn!(
                            summary = %report.summary(),
                            "审批图快照自检完成：发现不一致（逐条见上方 WARN 与 audit_logs）"
                        );
                    }
                    Err(error) => tracing::error!(error = %error, "审批图快照自检失败"),
                }
            })
        },
    );
}

/// 执行一轮完整的自检（默认批大小 [`SCAN_BATCH`]；可被单测 / 运维脚本直接调用）。
///
/// # 异常情况
///
/// 扫描失败返回错误消息（调用方只记日志）；单个版本失败不中断整轮（计入 `failed`）。
///
/// SCOPE-EXEMPT: 这是**后台定时任务**（无请求者身份），核对的是全局配置的发布版本；
/// 注入数据范围会让别人的版本永远不被核对，等于把这条红线关掉。
pub async fn run_once() -> std::result::Result<SelfCheckReport, String> {
    run_with_batch(SCAN_BATCH).await
}

/// 执行一轮自检，显式指定批大小（单测用它证明「分批」真的分批）。
///
/// # 参数
///
/// - `batch`：单批版本数（`< 1` 时抬到 1）。
///
/// # 异常情况
///
/// 同 [`run_once`]。
pub async fn run_with_batch(batch: i64) -> std::result::Result<SelfCheckReport, String> {
    let batch = batch.max(1);
    // 同一轮的所有审计行共用一个 request_id：审计员按它能把一次自检的结论捞齐。
    let request_id = Uuid::now_v7().to_string();
    let mut report = SelfCheckReport::default();
    let mut after: Option<Uuid> = None;

    for _ in 0..MAX_BATCHES {
        let versions = scan_batch(after, batch)
            .await
            .map_err(|error| format!("扫描 PUBLISHED 版本失败：{error}"))?;
        if versions.is_empty() {
            break;
        }
        let full_batch = versions.len() as i64 >= batch;
        report.batches += 1;

        for version in versions {
            // 先推进游标再处理：本版本处理失败也要往下走，否则整轮卡在同一页
            after = Some(version.id);
            report.versions += 1;
            match check_one(&version).await {
                Ok(Outcome::Clean) => {}
                Ok(Outcome::MissingHash) => {
                    report.missing_hash += 1;
                    tracing::warn!(
                        version_id = %version.id,
                        flow_code = %version.flow_code,
                        version_no = i64::from(version.version_no),
                        "审批图快照自检：PUBLISHED 版本缺 graph_hash（库侧 CHECK 本不该允许）"
                    );
                }
                Ok(Outcome::Drift {
                    kind,
                    rows_hash,
                    snapshot_hash,
                }) => {
                    let drift = Drift {
                        version_id: version.id,
                        def_id: version.def_id,
                        flow_code: version.flow_code.clone(),
                        version_no: version.version_no,
                        kind,
                        stored_hash: version.graph_hash.trim().to_string(),
                        recomputed_hash: rows_hash,
                        snapshot_hash,
                    };
                    tracing::warn!(
                        version_id = %drift.version_id,
                        flow_code = %drift.flow_code,
                        version_no = i64::from(drift.version_no),
                        kind = kind.as_str(),
                        stored_hash = %drift.stored_hash,
                        recomputed_hash = %drift.recomputed_hash,
                        snapshot_hash = drift.snapshot_hash.as_deref().unwrap_or("-"),
                        "审批图快照自检：{}",
                        kind.label()
                    );
                    match write_audit(&request_id, &drift).await {
                        Ok(()) => {}
                        Err(error) => {
                            report.audit_failed += 1;
                            tracing::error!(
                                version_id = %drift.version_id,
                                error = %error,
                                "审批图快照自检：审计写入失败（告警日志已发）"
                            );
                        }
                    }
                    report.drifted.push(drift);
                }
                Err(error) => {
                    report.failed += 1;
                    tracing::warn!(
                        version_id = %version.id,
                        flow_code = %version.flow_code,
                        error = %error,
                        "审批图快照自检：读行失败（跳过该版本，其余照跑）"
                    );
                }
            }
        }

        if !full_batch {
            break;
        }
        if report.batches >= MAX_BATCHES {
            tracing::warn!(
                batches = report.batches,
                checked = report.versions,
                "审批图快照自检：达到本轮批数上限，剩余版本下一轮继续"
            );
        }
    }
    Ok(report)
}

/// 检查一个版本（读行 → 重算 hash → 与库里的 hash 及快照三方对账）。
///
/// 注意：`load_graph` 读的是**行**，`load_snapshot` 读的是**JSONB**，两条路径都是
/// 发布时用过的同一套序列化入口（见模块文档「三条口径」）。
async fn check_one(version: &PublishedVersion) -> DbResult<Outcome> {
    let stored = version.graph_hash.trim();
    if stored.is_empty() {
        return Ok(Outcome::MissingHash);
    }
    let graph = model::load_graph(version.id).await?;
    let rows_hash = graph.graph_hash();
    let snapshot_hash = model::load_snapshot(version.id)
        .await?
        .as_deref()
        .and_then(snapshot_text_hash);

    match evaluate(stored, &rows_hash, snapshot_hash.as_deref()) {
        Some(kind) => Ok(Outcome::Drift {
            kind,
            rows_hash,
            snapshot_hash,
        }),
        None => Ok(Outcome::Clean),
    }
}

/// 对账判据（纯函数 —— 单测用它钉住「正常不报警 / 改行报警 / 改快照报警」）。
///
/// # 参数
///
/// - `stored`：`biz_flow_def_version.graph_hash`（CHAR(64)，调用方已 trim）；
/// - `rows_hash`：从 node/edge/assignee 行重算的 hash；
/// - `snapshot_hash`：`graph_snapshot` 自身 canonical 化后的 hash（`None` = 快照为空）；
///
/// # 返回值
///
/// `None` = 一致；`Some(RowsEdited)` **优先于** `Some(SnapshotEdited)`（行是源头，
/// 两份都脏时先报源头，修法完全不同）。
#[must_use]
pub fn evaluate(stored: &str, rows_hash: &str, snapshot_hash: Option<&str>) -> Option<DriftKind> {
    let stored = stored.trim();
    if stored.is_empty() {
        return None;
    }
    if stored != rows_hash {
        return Some(DriftKind::RowsEdited);
    }
    match snapshot_hash {
        Some(snapshot) if !snapshot.is_empty() && snapshot != stored => {
            Some(DriftKind::SnapshotEdited)
        }
        _ => None,
    }
}

/// 快照文本自身的 hash（`graph_snapshot::text` → `Value` → canonical JSON → sha256）。
///
/// 为什么不能直接对 `graph_snapshot` 的文本做 sha256：jsonb 出库的文本会**按键长度 + 字节序
/// 重排键**并带 `", "` 空格，与发布时 `snapshot_json()` 那份逐字节不同。先 parse 回 `Value`
/// 再过 [`graph::canonical_json`]（发布时的同一入口）才是可比口径。
///
/// 解析失败（库里是非法 JSON）→ `None`：此时不报「快照被改」而是让上限判据（行）说话，
/// 避免把「读不出来」误报成「被篡改」。
#[must_use]
fn snapshot_text_hash(text: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let canonical = graph::canonical_json(&value);
    let digest = Sha256::digest(canonical.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Some(out)
}

/// 把一条不一致写进 `audit_logs`（追加写；失败由调用方记日志，不影响其它版本）。
async fn write_audit(request_id: &str, drift: &Drift) -> std::result::Result<(), String> {
    audit_row(request_id, drift)
        .insert(db())
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// 构造审计行（纯函数，便于单测断言审计内容）。
fn audit_row(request_id: &str, drift: &Drift) -> AuditLogModel {
    AuditLogModel {
        id: Uuid::now_v7(),
        request_id: request_id.to_string(),
        user_id: None,
        username: Some(AUDIT_OPERATOR.to_string()),
        operation: AUDIT_OPERATION.to_string(),
        target_type: Some(AUDIT_TARGET_TYPE.to_string()),
        target_id: Some(drift.version_id.to_string()),
        // before = 库里的权威值；after = 重算值。都放 hash，不放整张图（审计表不存业务大字段）
        before_data: Some(serde_json::json!({ "graph_hash": drift.stored_hash })),
        after_data: Some(serde_json::json!({ "graph_hash": drift.recomputed_hash })),
        detail: Some(serde_json::json!({
            "kind": drift.kind.as_str(),
            "reason": drift.kind.label(),
            "def_id": drift.def_id.to_string(),
            "flow_code": drift.flow_code.clone(),
            "version_no": drift.version_no,
            "snapshot_hash": drift.snapshot_hash.clone(),
            "checker": TASK_FLOW_GRAPH_SELFCHECK,
        })),
        ip: None,
        user_agent: None,
        created_at: chrono::Utc::now(),
    }
}

/// 一条待核对的 PUBLISHED 版本（只取判据需要的列，不拉快照大字段）。
#[derive(Debug, Clone)]
struct PublishedVersion {
    id: Uuid,
    def_id: Uuid,
    version_no: i32,
    graph_hash: String,
    flow_code: String,
}

/// 取一批 PUBLISHED 版本（keyset 分页：`after` 为上一批最后一个 `id`）。
async fn scan_batch(after: Option<Uuid>, batch: i64) -> DbResult<Vec<PublishedVersion>> {
    let rows = match after {
        Some(cursor) => {
            db().find_with_params(SQL_SCAN_NEXT, vec![Value::from(cursor), Value::from(batch)])
                .await?
        }
        None => {
            db().find_with_params(SQL_SCAN_FIRST, vec![Value::from(batch)])
                .await?
        }
    };
    let mut versions = Vec::with_capacity(rows.len());
    for row in &rows {
        // CHAR(64) → 驱动回填可能带补齐空格，统一在 check_one 里 trim（判据只看 64 位 hex）
        let graph_hash: Option<String> = row.get_typed("graph_hash")?;
        versions.push(PublishedVersion {
            id: row.get_typed("id")?,
            def_id: row.get_typed("def_id")?,
            version_no: row.get_typed("version_no")?,
            graph_hash: graph_hash.unwrap_or_default(),
            flow_code: row.get_typed("flow_code")?,
        });
    }
    Ok(versions)
}

/// 分页批数（纯函数：`total` 个版本、每批 `batch` 个 → 需要几批）。单测钉住边界。
#[must_use]
pub fn batches_for(total: i64, batch: i64) -> i64 {
    if total <= 0 || batch <= 0 {
        return 0;
    }
    (total + batch - 1) / batch
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条不一致记录（审计断言用）。
    fn sample_drift(kind: DriftKind) -> Drift {
        Drift {
            version_id: Uuid::now_v7(),
            def_id: Uuid::now_v7(),
            flow_code: "leave_apply".to_string(),
            version_no: 3,
            kind,
            stored_hash: "a".repeat(64),
            recomputed_hash: "b".repeat(64),
            snapshot_hash: Some("a".repeat(64)),
        }
    }

    #[test]
    fn constants_match_architecture() {
        // 名字与 cron 必须与架构 §6.5 的 sys_job 种子逐字一致（运维按它查任务）
        assert_eq!(TASK_FLOW_GRAPH_SELFCHECK, "flow_graph_selfcheck");
        assert_eq!(CRON_FLOW_GRAPH_SELFCHECK, "0 30 3 * * *");
        assert_eq!(SCAN_BATCH, 200);
        assert_eq!(MAX_BATCHES, 50);
        assert_eq!(AUDIT_OPERATION, "FLOW_GRAPH_HASH_DRIFT");
        // audit_logs.operation 是 VARCHAR(50)
        assert!(AUDIT_OPERATION.len() <= 50);
    }

    #[test]
    fn clean_data_does_not_alarm() {
        let hash = "c".repeat(64);
        // 正常：行 hash == 库 hash == 快照 hash
        assert_eq!(evaluate(&hash, &hash, Some(&hash)), None);
        // 快照为空（历史数据 / PUBLISHED 但快照未回填）→ 不因此报警
        assert_eq!(evaluate(&hash, &hash, None), None);
        assert_eq!(evaluate(&hash, &hash, Some("")), None);
        // CHAR(64) 补齐空格要能容忍（调用方约定已 trim，这里再钉一次语义）
        assert_eq!(evaluate(&format!("{hash}   "), &hash, None), None);
        // 没有 hash 的版本不走对账（由 MissingHash 单独计数）
        assert_eq!(evaluate("", &hash, None), None);
    }

    #[test]
    fn row_edit_is_detected() {
        let stored = "a".repeat(64);
        let rows = "b".repeat(64);
        // 行被改：即使快照还是好的，也必须报 ROWS_EDITED（源头优先）
        assert_eq!(
            evaluate(&stored, &rows, Some(&stored)),
            Some(DriftKind::RowsEdited)
        );
        assert_eq!(evaluate(&stored, &rows, None), Some(DriftKind::RowsEdited));
        assert_eq!(DriftKind::RowsEdited.as_str(), "ROWS_EDITED");
        assert!(DriftKind::RowsEdited.label().contains("node/edge"));
    }

    #[test]
    fn snapshot_edit_is_detected() {
        let stored = "a".repeat(64);
        let snapshot = "d".repeat(64);
        // 行没动、快照被直接写 → SNAPSHOT_EDITED
        assert_eq!(
            evaluate(&stored, &stored, Some(&snapshot)),
            Some(DriftKind::SnapshotEdited)
        );
        // 行和快照都脏 → 仍报源头（行）
        assert_eq!(
            evaluate(&stored, &"b".repeat(64), Some(&snapshot)),
            Some(DriftKind::RowsEdited)
        );
        assert_eq!(DriftKind::SnapshotEdited.as_str(), "SNAPSHOT_EDITED");
    }

    #[test]
    fn batching_plan_is_bounded() {
        assert_eq!(batches_for(0, 200), 0);
        assert_eq!(batches_for(1, 200), 1);
        assert_eq!(batches_for(200, 200), 1);
        assert_eq!(batches_for(201, 200), 2);
        assert_eq!(batches_for(400, 200), 2);
        // 一轮上限覆盖的版本数（10000）；批大小非正时不吃除零
        assert!(batches_for(MAX_BATCHES as i64 * SCAN_BATCH, SCAN_BATCH) <= MAX_BATCHES as i64);
        assert_eq!(batches_for(10, 0), 0);
    }

    #[test]
    fn audit_payload_carries_both_hashes() {
        let request_id = "0192f0aa-0000-7000-8000-000000000001";
        let drift = sample_drift(DriftKind::RowsEdited);
        let row = audit_row(request_id, &drift);
        assert_eq!(row.operation, AUDIT_OPERATION);
        assert_eq!(row.target_type.as_deref(), Some(AUDIT_TARGET_TYPE));
        assert_eq!(
            row.target_id.as_deref(),
            Some(drift.version_id.to_string().as_str())
        );
        assert_eq!(row.request_id, request_id);
        assert_eq!(row.username.as_deref(), Some("system"));
        // 自检没有请求者身份，user_id / ip 必须为空（不许伪造一个操作人）
        assert!(row.user_id.is_none());
        assert!(row.ip.is_none());
        let before = row.before_data.clone().expect("before_data");
        let after = row.after_data.clone().expect("after_data");
        assert_eq!(before["graph_hash"], serde_json::json!(drift.stored_hash));
        assert_eq!(
            after["graph_hash"],
            serde_json::json!(drift.recomputed_hash)
        );
        let detail = row.detail.clone().expect("detail");
        assert_eq!(detail["kind"], serde_json::json!("ROWS_EDITED"));
        assert_eq!(detail["flow_code"], serde_json::json!("leave_apply"));
        assert_eq!(detail["version_no"], serde_json::json!(3));
    }
}

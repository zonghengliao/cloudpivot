//! 运维观测采样器：每分钟把「告警判定要用的事实」采成 Prometheus gauge。
//!
//! # 为什么要有这个任务
//!
//! ADR-20261101 §5.4 只装了 exporter（`/metrics` 能访问），但上报的指标只有框架自带的
//! 请求/DB/缓存计数与 3 个 `cloudpivot_*` 身份相关 gauge —— **告警要看的三件事一件都没人采**：
//! 「导入导出有没有积压」「昨晚巡检是不是 FAIL」「采样器自己是不是还活着」。
//! 于是运维只能靠人肉登库查，`/metrics` 有了也等于没有（评审 5.8 原文：无指标、无告警规则）。
//!
//! 本任务把这三件事采成 gauge，口径只有一处：
//!
//! - **告警脚本**（`scripts/ops/alert_check.sh`）优先读 `/metrics`，不持有库口令；
//! - **Prometheus**（ARCH9-4 若上）直接抓同样的名字，不需要另写 recording rule；
//! - 人看板 / 排障直接 curl，不用连库。
//!
//! # 采样口径
//!
//! | 指标 | 含义 | 告警用法 |
//! |------|------|----------|
//! | `cloudpivot_impexp_backlog_pending` | 导入+导出「未终态」任务数 | 持续 > 阈值即积压 |
//! | `cloudpivot_impexp_backlog_stuck_seconds` | 最早一张未终态任务已存在多久（秒） | 长期 PENDING 判据 |
//! | `cloudpivot_impexp_import_open` / `_export_open` | 分方向的未终态数 | 定位是哪一侧堵了 |
//! | `cloudpivot_impexp_import_failed_24h` / `_export_failed_24h` | 近 24h 失败数 | 批量导入炸了要看出来 |
//! | `cloudpivot_invariant_ok` | 最近一次巡检是否 OK（1/0，`-1` = 从未跑过） | 巡检 FAIL 告警 |
//! | `cloudpivot_invariant_blockers` | 最近一次巡检的 BLOCKER 数（`-1` = 未知） | 账已错 |
//! | `cloudpivot_invariant_age_seconds` | 最近一次巡检距今多久（`-1` = 从未跑过） | 巡检任务死了也要告警 |
//! | `cloudpivot_metrics_sampler_last_success_timestamp` | 本次采样成功的 epoch 秒 | 采样器自己挂了能被看出来 |
//! | `cloudpivot_metrics_sampler_runs_total` | 采样累计次数（counter） | 观察任务是否在跑 |
//!
//! # 失败语义
//!
//! **单条查询失败不拖垮整次采样**：该项置 `-1`（未知）并 WARN，其余照常上报。
//! 理由：库里某些表可能因历史原因缺失（老库 / 演练库），而「积压查询失败」不该连带把
//! 「巡检结论」也丢掉 —— 告警侧对 `-1` 的处置是「说出来」，不是「当 0 看」。
//!
//! # 只读
//!
//! 全部是 `SELECT count(*)` / `min(created_at)` / 单行读取，不写任何表，可安全每分钟跑。

use nik::prelude::*;

use super::Scheduler;

/// 内置任务名（与 `sys_job.invoke_target` 口径一致，便于以后做成可调度任务）。
pub const TASK_NAME: &str = "metrics_snapshot";

/// 内置 cron：每分钟一次（6 段写法：秒 分 时 日 月 周）。
///
/// 1 分钟是「排障时刷新够快」与「count(*) 成本够低」的折中：这几张表都是
/// 小体量运营表（导入导出任务、巡检结果），全量 count 走索引几十微秒。
pub const CRON: &str = "0 * * * * *";

/// 未知值的表示（查询失败 / 从未跑过）。
///
/// 用 `-1` 而不是 `0`：Prometheus 侧 `-1` 与真实值 0 必须能分开看，
/// 否则「巡检没跑过」会被显示成「巡检通过」——这就是典型的假绿。
pub const UNKNOWN: f64 = -1.0;

/// 一次采样的事实快照。
///
/// 每个字段都是 `Option<i64>`：`None` = 该项查询失败（上报 `-1`），
/// 而不是「查到 0」。这个区分是刻意的：告警脚本对 `-1` 出「未知/降级」，
/// 对真实 0 出「正常」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// 导入任务里状态为 `PENDING` 的条数（尚未开始解析）。
    pub import_pending: Option<i64>,
    /// 导入任务里「未终态」条数（`PENDING` / `PARSING` / `VALIDATED` / `IMPORTING`）。
    pub import_open: Option<i64>,
    /// 最早的未终态导入任务已存在多少秒。
    pub import_stuck_seconds: Option<i64>,
    /// 近 24 小时失败的导入任务数。
    pub import_failed_24h: Option<i64>,
    /// 导出任务里状态为 `PENDING` 的条数（尚未被 worker 认领）。
    pub export_pending: Option<i64>,
    /// 导出任务里「未终态」条数（`PENDING` / `RUNNING`）。
    pub export_open: Option<i64>,
    /// 最早的未终态导出任务已存在多少秒。
    pub export_stuck_seconds: Option<i64>,
    /// 近 24 小时失败的导出任务数。
    pub export_failed_24h: Option<i64>,
    /// 最近一次巡检结论（`None` = 从未跑过或查询失败）。
    pub invariant: Option<InvariantRow>,
}

/// 最近一次账务不变量巡检的结论行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantRow {
    /// `OK` / `FAIL` / `ERROR`。
    pub status: String,
    /// 失败项里 BLOCKER 级别数。
    pub blockers: i64,
    /// 失败项合计（BLOCKER + WARN + INFO 中 `ok = false` 的）。
    pub failed: i64,
    /// 这次运行距今多少秒（negatives 说明库时钟比应用快，按 0 处理）。
    pub age_seconds: i64,
}

impl InvariantRow {
    /// 是否是「账没问题」的结论。
    ///
    /// 只有 `status = OK` 才算通过：`ERROR` 是「脚本没跑成」——**没有结论不等于通过**，
    /// 把它算绿正是巡检类告警最常见的事故姿势（脚本挂了半年没人发现）。
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.status.eq_ignore_ascii_case("OK")
    }

    /// 转成 gauge 值：`1` 通过 / `0` 未通过。
    #[must_use]
    pub fn ok_flag(&self) -> f64 {
        if self.is_ok() { 1.0 } else { 0.0 }
    }
}

impl Snapshot {
    /// 导入 + 导出未终态任务合计（任一方向未知则视为未知）。
    #[must_use]
    pub fn backlog_pending(&self) -> Option<i64> {
        match (self.import_open, self.export_open) {
            (Some(import), Some(export)) => Some(import + export),
            _ => None,
        }
    }

    /// 两个方向里最早那张未终态任务已存在多少秒（取最大）。
    ///
    /// 只要有一侧堵了很久，整体就堵了很久 —— 取 max 才能让告警抓住「导出堵死、导入正常」。
    #[must_use]
    pub fn backlog_stuck_seconds(&self) -> Option<i64> {
        match (self.import_stuck_seconds, self.export_stuck_seconds) {
            (Some(import), Some(export)) => Some(import.max(export)),
            (Some(one), None) | (None, Some(one)) => Some(one),
            (None, None) => None,
        }
    }

    /// 快照 → `(指标名, 值)` 列表。
    ///
    /// 纯函数（不碰全局 recorder、不碰库）：单测就是对着这张表断言，
    /// 「指标名改了但脚本还在读旧名」这类漂移在 CI 里就会被抓住。
    #[must_use]
    pub fn gauges(&self) -> Vec<(&'static str, f64)> {
        let value = |v: Option<i64>| v.map_or(UNKNOWN, |v| v as f64);
        let mut out = vec![
            ("cloudpivot_impexp_import_pending", value(self.import_pending)),
            ("cloudpivot_impexp_import_open", value(self.import_open)),
            (
                "cloudpivot_impexp_import_stuck_seconds",
                value(self.import_stuck_seconds),
            ),
            ("cloudpivot_impexp_import_failed_24h", value(self.import_failed_24h)),
            ("cloudpivot_impexp_export_pending", value(self.export_pending)),
            ("cloudpivot_impexp_export_open", value(self.export_open)),
            (
                "cloudpivot_impexp_export_stuck_seconds",
                value(self.export_stuck_seconds),
            ),
            ("cloudpivot_impexp_export_failed_24h", value(self.export_failed_24h)),
            ("cloudpivot_impexp_backlog_pending", value(self.backlog_pending())),
            (
                "cloudpivot_impexp_backlog_stuck_seconds",
                value(self.backlog_stuck_seconds()),
            ),
        ];
        let (ok, blockers, failed, age) = match self.invariant.as_ref() {
            Some(row) => (
                row.ok_flag(),
                row.blockers as f64,
                row.failed as f64,
                row.age_seconds as f64,
            ),
            None => (UNKNOWN, UNKNOWN, UNKNOWN, UNKNOWN),
        };
        out.push(("cloudpivot_invariant_ok", ok));
        out.push(("cloudpivot_invariant_blockers", blockers));
        out.push(("cloudpivot_invariant_failed", failed));
        out.push(("cloudpivot_invariant_age_seconds", age));
        out
    }

    /// 一行日志摘要（采样完成时打，排障时不用 curl 也能看趋势）。
    #[must_use]
    pub fn summary(&self) -> String {
        let show = |v: Option<i64>| v.map_or_else(|| "未知".to_string(), |v| v.to_string());
        format!(
            "导入 open={} pending={} stuck={}s failed24h={}；导出 open={} pending={} stuck={}s failed24h={}；巡检 {}",
            show(self.import_open),
            show(self.import_pending),
            show(self.import_stuck_seconds),
            show(self.import_failed_24h),
            show(self.export_open),
            show(self.export_pending),
            show(self.export_stuck_seconds),
            show(self.export_failed_24h),
            match self.invariant.as_ref() {
                Some(row) => format!(
                    "{}（blockers={} failed={} age={}s）",
                    row.status, row.blockers, row.failed, row.age_seconds
                ),
                None => "无记录".to_string(),
            }
        )
    }
}

/// 未终态导入任务的状态集合（与 `ck_sys_import_task_status` 一致）。
const IMPORT_OPEN_STATUSES: &str = "'PENDING', 'PARSING', 'VALIDATED', 'IMPORTING'";

/// 未终态导出任务的状态集合（与 `ck_sys_export_task_status` 一致）。
const EXPORT_OPEN_STATUSES: &str = "'PENDING', 'RUNNING'";

/// 读一个标量并把失败降级成 `None`（WARN 一条，带上 SQL 便于定位）。
async fn scalar_or_unknown(sql: &str, what: &str) -> Option<i64> {
    match db().scalar::<i64>(sql).await {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, sql, what, "运维观测采样查询失败（该项上报 -1 未知）");
            None
        }
    }
}

/// 采一次快照（只读，不写库、不 panic）。
///
/// # 返回值
///
/// 快照。单条查询失败只会让对应字段为 `None`，不会让整个函数失败 ——
/// 部分指标远比没有指标有用。
pub async fn read_snapshot() -> Snapshot {
    let mut snapshot = Snapshot {
        import_pending: scalar_or_unknown(
            "SELECT count(*) FROM sys_import_task \
             WHERE deleted_at IS NULL AND status = 'PENDING'",
            "导入待处理",
        )
        .await,
        import_open: scalar_or_unknown(
            &format!(
                "SELECT count(*) FROM sys_import_task \
                 WHERE deleted_at IS NULL AND status IN ({IMPORT_OPEN_STATUSES})"
            ),
            "导入未终态",
        )
        .await,
        import_stuck_seconds: scalar_or_unknown(
            &format!(
                "SELECT COALESCE(EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - min(created_at)))::bigint, 0) \
                 FROM sys_import_task \
                 WHERE deleted_at IS NULL AND status IN ({IMPORT_OPEN_STATUSES})"
            ),
            "导入最早未终态时长",
        )
        .await,
        import_failed_24h: scalar_or_unknown(
            "SELECT count(*) FROM sys_import_task \
             WHERE deleted_at IS NULL AND status = 'FAILED' \
               AND finished_at >= CURRENT_TIMESTAMP - INTERVAL '24 hours'",
            "导入近 24h 失败",
        )
        .await,
        export_pending: scalar_or_unknown(
            "SELECT count(*) FROM sys_export_task \
             WHERE deleted_at IS NULL AND status = 'PENDING'",
            "导出待处理",
        )
        .await,
        export_open: scalar_or_unknown(
            &format!(
                "SELECT count(*) FROM sys_export_task \
                 WHERE deleted_at IS NULL AND status IN ({EXPORT_OPEN_STATUSES})"
            ),
            "导出未终态",
        )
        .await,
        export_stuck_seconds: scalar_or_unknown(
            &format!(
                "SELECT COALESCE(EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - min(created_at)))::bigint, 0) \
                 FROM sys_export_task \
                 WHERE deleted_at IS NULL AND status IN ({EXPORT_OPEN_STATUSES})"
            ),
            "导出最早未终态时长",
        )
        .await,
        export_failed_24h: scalar_or_unknown(
            "SELECT count(*) FROM sys_export_task \
             WHERE deleted_at IS NULL AND status = 'FAILED' \
               AND finished_at >= CURRENT_TIMESTAMP - INTERVAL '24 hours'",
            "导出近 24h 失败",
        )
        .await,
        invariant: None,
    };
    // SCOPE-EXEMPT：运维采样读的是「全库事实」（导入导出队列 / 巡检结论），
    // 没有部门或归属人列，不属于架构 §2.5 的数据权限范围；它不上报任何业务明细，
    // 只是一组 count / 时长。与 `support::invariant::model` 的读口径一致。
    snapshot.invariant = read_invariant().await;
    snapshot
}

/// 读最近一次巡检结论。
///
/// 用一条 `row_to_json` 拼成单标量读取，避免为一个「只有一行」的查询牵出
/// 一整套 Model 映射（这张表由 `support::invariant` 管，采样本不该反向依赖它的 DTO）。
async fn read_invariant() -> Option<InvariantRow> {
    let sql = "SELECT row_to_json(t)::text FROM ( \
                   SELECT status, \
                          blockers, \
                          failed, \
                          GREATEST(EXTRACT(EPOCH FROM (CURRENT_TIMESTAMP - run_at))::bigint, 0) AS age_seconds \
                     FROM sys_invariant_result \
                 ORDER BY run_at DESC \
                    LIMIT 1 \
               ) t";
    let raw: Option<String> = match db().scalar::<Option<String>>(sql).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "运维观测采样：巡检结论查询失败（上报 -1 未知）");
            return None;
        }
    };
    parse_invariant(raw.as_deref())
}

/// 解析巡检行 JSON（单独抽出来是为了能脱离数据库单测）。
///
/// # 参数
///
/// - `raw`：`row_to_json` 的输出；`None` = 库里一条记录都没有。
#[must_use]
pub fn parse_invariant(raw: Option<&str>) -> Option<InvariantRow> {
    let raw = raw?;
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|error| tracing::warn!(%error, raw, "巡检结论 JSON 解析失败（按未知处理）"))
        .ok()?;
    let status = value.get("status")?.as_str()?.to_string();
    Some(InvariantRow {
        status,
        blockers: value.get("blockers").and_then(serde_json::Value::as_i64).unwrap_or(0),
        failed: value.get("failed").and_then(serde_json::Value::as_i64).unwrap_or(0),
        age_seconds: value
            .get("age_seconds")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
            .max(0),
    })
}

/// 执行一次采样：读快照 → 上报 gauge → 打日志。
///
/// # 返回值
///
/// 本次快照（供任务日志与单测使用；单条查询失败不影响返回）。
pub async fn run_once() -> Snapshot {
    let snapshot = read_snapshot().await;
    for (name, value) in snapshot.gauges() {
        metrics::gauge!(name).set(value);
    }
    // 采样器自身的存活信号：`/metrics` 上一旦这条不再前进，说明任务停了
    // （调度器没起 / 进程卡住 / 库连不上），而业务指标看起来可能还是「0 很正常」。
    let now = chrono::Utc::now().timestamp() as f64;
    metrics::gauge!("cloudpivot_metrics_sampler_last_success_timestamp").set(now);
    metrics::counter!("cloudpivot_metrics_sampler_runs_total").increment(1);
    tracing::info!(summary = %snapshot.summary(), "运维观测采样完成");
    snapshot
}

/// 在内置调度器上注册本任务（由 [`crate::task::Scheduler::register_builtin`] 调用）。
pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(
        TASK_NAME,
        CRON,
        "系统任务：运维观测采样（每分钟刷新 /metrics 告警指标）",
        || {
            tokio::spawn(async move {
                // run_once 自身不会失败（单项降级为 -1），不会把调度线程带崩
                let _ = run_once().await;
            })
        },
    );
    tracing::info!(
        invoke_target = TASK_NAME,
        cron = CRON,
        "运维观测采样任务已注册（NEAR-FIX-3 · 告警指标）"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_snapshot() -> Snapshot {
        Snapshot {
            import_pending: Some(2),
            import_open: Some(3),
            import_stuck_seconds: Some(120),
            import_failed_24h: Some(1),
            export_pending: Some(4),
            export_open: Some(5),
            export_stuck_seconds: Some(900),
            export_failed_24h: Some(0),
            invariant: Some(InvariantRow {
                status: "OK".to_string(),
                blockers: 0,
                failed: 0,
                age_seconds: 3600,
            }),
        }
    }

    fn gauge_of<'a>(gauges: &'a [(&'static str, f64)], name: &str) -> &'a f64 {
        &gauges
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("指标 {name} 不存在"))
            .1
    }

    #[test]
    fn gauges_cover_every_documented_name() {
        let gauges = full_snapshot().gauges();
        let names: Vec<&str> = gauges.iter().map(|(name, _)| *name).collect();
        // 名单就是告警脚本与 Prometheus 的契约，改名必须同步改脚本
        assert_eq!(
            names,
            vec![
                "cloudpivot_impexp_import_pending",
                "cloudpivot_impexp_import_open",
                "cloudpivot_impexp_import_stuck_seconds",
                "cloudpivot_impexp_import_failed_24h",
                "cloudpivot_impexp_export_pending",
                "cloudpivot_impexp_export_open",
                "cloudpivot_impexp_export_stuck_seconds",
                "cloudpivot_impexp_export_failed_24h",
                "cloudpivot_impexp_backlog_pending",
                "cloudpivot_impexp_backlog_stuck_seconds",
                "cloudpivot_invariant_ok",
                "cloudpivot_invariant_blockers",
                "cloudpivot_invariant_failed",
                "cloudpivot_invariant_age_seconds",
            ]
        );
        assert_eq!(names.len(), 14, "采样器应上报 14 个指标名");
    }

    #[test]
    fn backlog_sums_both_directions_and_takes_worst_age() {
        let snapshot = full_snapshot();
        assert_eq!(snapshot.backlog_pending(), Some(8));
        // 导出堵了 900s 比导入的 120s 更严重 → 取 900
        assert_eq!(snapshot.backlog_stuck_seconds(), Some(900));
    }

    #[test]
    fn unknown_query_reports_minus_one_not_zero() {
        let snapshot = Snapshot::default();
        let gauges = snapshot.gauges();
        assert_eq!(*gauge_of(&gauges, "cloudpivot_impexp_backlog_pending"), UNKNOWN);
        assert_eq!(*gauge_of(&gauges, "cloudpivot_invariant_ok"), UNKNOWN);
        assert_eq!(*gauge_of(&gauges, "cloudpivot_invariant_blockers"), UNKNOWN);
    }

    #[test]
    fn one_side_unknown_still_reports_the_other() {
        let snapshot = Snapshot {
            import_open: None,
            export_open: Some(7),
            export_stuck_seconds: Some(60),
            ..Snapshot::default()
        };
        // 合计未知（不能把「导入查不到」当 0），但时长仍能给出已知的一侧
        assert_eq!(snapshot.backlog_pending(), None);
        assert_eq!(snapshot.backlog_stuck_seconds(), Some(60));
    }

    #[test]
    fn invariant_ok_only_for_ok_status() {
        let ok = InvariantRow {
            status: "OK".into(),
            blockers: 0,
            failed: 0,
            age_seconds: 10,
        };
        assert!(ok.is_ok());
        assert_eq!(ok.ok_flag(), 1.0);

        // ERROR = 脚本没跑成，没有结论 ≠ 通过
        for status in ["FAIL", "ERROR", "ok_maybe"] {
            let row = InvariantRow {
                status: status.into(),
                blockers: 3,
                failed: 4,
                age_seconds: 10,
            };
            assert!(!row.is_ok(), "{status} 不能算通过");
            assert_eq!(row.ok_flag(), 0.0);
        }
    }

    #[test]
    fn parse_invariant_reads_row_json_and_tolerates_missing_row() {
        let row = parse_invariant(Some(r#"{"status":"FAIL","blockers":2,"failed":5,"age_seconds":7200}"#))
            .expect("应解析出巡检行");
        assert_eq!(row.status, "FAIL");
        assert_eq!(row.blockers, 2);
        assert_eq!(row.failed, 5);
        assert_eq!(row.age_seconds, 7200);
        assert_eq!(row.ok_flag(), 0.0);

        // 库里一条记录都没有：`row_to_json` 返回 NULL
        assert_eq!(parse_invariant(None), None);
        // 坏 JSON 不能让采样任务炸掉
        assert_eq!(parse_invariant(Some("not json")), None);
        assert_eq!(parse_invariant(Some(r#"{"blockers":1}"#)), None);
    }

    #[test]
    fn negative_age_is_clamped_to_zero() {
        // 库时钟比应用快时会算出负数，负的「多久以前」在告警侧无意义
        let row = parse_invariant(Some(r#"{"status":"OK","blockers":0,"failed":0,"age_seconds":-42}"#))
            .expect("应解析出巡检行");
        assert_eq!(row.age_seconds, 0);
    }

    #[test]
    fn summary_never_hides_unknown() {
        let summary = Snapshot::default().summary();
        assert!(summary.contains("未知"), "未知必须显式说出来：{summary}");
        assert!(summary.contains("无记录"), "从未巡检必须说出来：{summary}");

        let summary = full_snapshot().summary();
        assert!(summary.contains("OK"), "{summary}");
        assert!(summary.contains("blockers=0"), "{summary}");
    }
}

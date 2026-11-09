//! 变更日志的分区运维（批次 C · C6）。
//!
//! 依据 `docs/批次C-技术架构.md` §4.9（保留策略与分区运维）：
//!
//! | 项 | 决策 |
//! |---|---|
//! | 分区粒度 | 按月 `RANGE (created_at)`，表名 `sys_change_log_YYYY_MM` |
//! | 预建策略 | **提前 1 个月**建下个月（`changelog_partition_maintain`，每月 1 日 00:20） |
//! | 是否建 DEFAULT 分区 | **不建**（POC-10：default 一堆积，新建正式分区会直接失败） |
//! | 保留期 | **36 个月**（`sys_config: changelog.retention_months`，本卡用常量默认值） |
//! | 清理方式 | `DROP TABLE`（O(1)，实测 1.5 ms；`DELETE` 3.3 万行要 37.9 ms 且留死元组） |
//!
//! # 为什么这个文件里没有 `db()`
//!
//! 与 [`crate::business::changelog::model`] 同一个理由（门禁 G2）：所有函数只吃
//! `&impl SqlExecutor`。定时任务的 runner 在 [`crate::task::Scheduler::register_builtin`]
//! 里把全局连接传进来（那里不属于本模块），于是「谁开了连接、谁负责生命周期」一目了然，
//! 也让本模块的每个动作都能在集成测试里跑在**测试库的执行器**上。
//!
//! # 三个运维坑（架构 §4.9 要求写进 runbook，这里同样写一遍）
//!
//! 1. `CREATE TABLE ... PARTITION OF` 会取父表 `AccessExclusiveLock`（毫秒级），
//!    必须在低峰执行 —— 这就是 cron 排在 00:20 而不是 03:xx 日志清理时段的原因；
//! 2. 主键含分区键，任何「按 id 单查」都必须带 `created_at`（见
//!    [`crate::business::changelog::query::find`]）；
//! 3. 分区清单只能查 `pg_inherits`：`pg_total_relation_size('sys_change_log')`
//!    对分区表返回 **0**（实测过），巡检脚本要用 `sum(pg_total_relation_size(inhrelid))`。
//!
//! # DDL 为什么用字符串拼接
//!
//! `CREATE TABLE ... PARTITION OF` 的表名是**标识符**，PG 不允许用绑定参数替换；
//! 因此本文件对表名做了两道防守：① 名字**只**由 [`partition_name`] 生成（不接受外部输入）；
//! ② 删除前用 [`is_managed_partition`] 逐字符校验（前缀 + `YYYY_MM` 数字形态）。
//! `DROP TABLE` 是唯一不可逆的操作，多一道字符校验不算冗余。

use nik::prelude::*;

/// 调度名（与 `sys_job.invoke_target` 同名，前端按它选调用目标）。
pub const TASK_NAME: &str = "changelog_partition_maintain";

/// cron：每月 1 日 00:20。
///
/// 错峰纪律（`task/mod.rs` 的注释）：03:00–03:40 是五张日志表的清理窗口，
/// 04:30 是存储回收；00:05 / 00:10 是公告的到期下线 —— 00:20 是空的。
pub const CRON: &str = "0 20 1 1 * *";

/// 分区表名前缀。
pub const PARTITION_PREFIX: &str = "sys_change_log_";

/// 父表名。
pub const PARENT_TABLE: &str = "sys_change_log";

/// 预建月数（**提前 1 个月**建下个月，架构 §4.9）。
pub const PREBUILD_MONTHS_AHEAD: i32 = 1;

/// 保留月数（36 个月，架构 §4.9）。
pub const RETENTION_MONTHS: i32 = 36;

/// 一次分区运维的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// 本次新建的分区名。
    pub created: Vec<String>,
    /// 已存在（无需新建）的分区名。
    pub existing: Vec<String>,
    /// 本次删除（过期）的分区名。
    pub dropped: Vec<String>,
    /// 保留期内的分区总数（运维巡检用）。
    pub kept: usize,
    /// 实际生效的保留月数。
    pub retention_months: i32,
}

impl MaintenanceReport {
    /// 一行可读摘要（定时任务日志按它 grep）。
    pub fn summary(&self) -> String {
        format!(
            "{TASK_NAME}: created={:?} dropped={:?} kept={} retention_months={}",
            self.created, self.dropped, self.kept, self.retention_months
        )
    }

    /// 本次是否有任何结构变更（用于决定日志级别：无变更打 info，有变更打 warn 级 attention）。
    pub fn changed(&self) -> bool {
        !self.created.is_empty() || !self.dropped.is_empty()
    }
}

/// 一个分区的巡检信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionInfo {
    /// 分区表名。
    pub name: String,
    /// 分区边界表达式（`FOR VALUES FROM ('2026-10-01') TO ('2026-11-01')`）。
    pub bound: String,
    /// 占用字节数（含索引）。
    pub bytes: i64,
}

/// 某个月份的月初日期（`2026-10-29` → `2026-10-01`）。
///
/// # 参数
///
/// - `now`：任意时刻（按其 UTC 年月取整）。
pub fn month_start(now: chrono::DateTime<chrono::Utc>) -> chrono::NaiveDate {
    use chrono::Datelike;
    chrono::NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap_or_else(|| {
        // 只有年月非法才可能走到这里（chrono 的 year 范围保证不会），保守兜底
        chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 一定合法")
    })
}

/// 月份偏移（`shift_month(2026-01-01, -1)` → `2025-12-01`）。
///
/// # 参数
///
/// - `month`：月初日期（不要求一定是 1 号，函数只取年月）；
/// - `delta`：偏移月数，可正可负。
pub fn shift_month(month: chrono::NaiveDate, delta: i32) -> chrono::NaiveDate {
    use chrono::Datelike;
    let total = month.year() * 12 + (month.month() as i32 - 1) + delta;
    let year = total.div_euclid(12);
    let month_index = total.rem_euclid(12) + 1;
    chrono::NaiveDate::from_ymd_opt(year, month_index as u32, 1)
        .unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 一定合法"))
}

/// 分区表名（`2026-10-01` → `sys_change_log_2026_10`）。
///
/// # 参数
///
/// - `month`：月初日期。
pub fn partition_name(month: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    format!("{PARTITION_PREFIX}{:04}_{:02}", month.year(), month.month())
}

/// 从分区名反解月份（`sys_change_log_2026_10` → `Some(2026-10-01)`）。
///
/// # 参数
///
/// - `name`：分区表名。
///
/// # 返回值
///
/// 名字不符合 `sys_change_log_YYYY_MM` 形态时返回 `None`。
pub fn parse_partition_month(name: &str) -> Option<chrono::NaiveDate> {
    let rest = name.strip_prefix(PARTITION_PREFIX)?;
    let (year_text, month_text) = rest.split_once('_')?;
    if year_text.len() != 4 || month_text.len() != 2 {
        return None;
    }
    if !year_text.bytes().all(|b| b.is_ascii_digit()) || !month_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = year_text.parse().ok()?;
    let month: u32 = month_text.parse().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, 1)
}

/// 是否是本模块管理的分区名（`DROP` 之前的最后一道校验）。
///
/// 只认 `sys_change_log_YYYY_MM` 且月份合法 —— 名字里带任何其它字符（引号、分号、
/// schema 前缀）都不通过，杜绝 `DROP TABLE` 被拼出意料之外的目标。
///
/// # 参数
///
/// - `name`：候选表名。
pub fn is_managed_partition(name: &str) -> bool {
    parse_partition_month(name).is_some()
}

/// 保留期内最早该保留的月份（`2026-10` 保留 36 个月 → `2023-11`）。
///
/// # 参数
///
/// - `current_month`：当前月月初；
/// - `retention_months`：保留月数（≤ 0 视为 1，防止把当前月也算过期）。
pub fn oldest_kept_month(current_month: chrono::NaiveDate, retention_months: i32) -> chrono::NaiveDate {
    let retention = retention_months.max(1);
    shift_month(current_month, -(retention - 1))
}

/// 纯函数：从现有分区名里挑出**该删的**（早于保留期）。
///
/// 抽成纯函数的理由与 C5 的 `preflight` 相同：`DROP TABLE` 这种不可逆动作必须在
/// **无库环境**下逐条断言（单测里把各种脏名字都喂一遍）。
///
/// # 参数
///
/// - `existing`：现有分区名；
/// - `current_month`：当前月月初；
/// - `retention_months`：保留月数。
///
/// # 返回值
///
/// 需要删除的分区名（升序）。不合命名规范的名字**不会被删除**（返回里没有），
/// 由调用方另行告警 —— 宁可少删，不可误删。
pub fn expired_partitions(existing: &[String], current_month: chrono::NaiveDate, retention_months: i32) -> Vec<String> {
    let cutoff = oldest_kept_month(current_month, retention_months);
    let mut out: Vec<String> = existing
        .iter()
        .filter(|name| match parse_partition_month(name) {
            Some(month) => month < cutoff,
            None => false,
        })
        .cloned()
        .collect();
    out.sort_unstable();
    out
}

/// 查询分区清单（`pg_inherits` 才是真相，`pg_total_relation_size(父表)` 恒 0）。
///
/// # 参数
///
/// - `ex`：执行器。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn list_partitions(ex: &impl SqlExecutor) -> DbResult<Vec<PartitionInfo>> {
    let rows = ex
        .query_sql(
            "SELECT c.relname::text AS name, \
                    pg_get_expr(c.relpartbound, c.oid)::text AS bound, \
                    pg_total_relation_size(c.oid)::bigint AS bytes \
               FROM pg_inherits i \
               JOIN pg_class c ON c.oid = i.inhrelid \
               JOIN pg_class p ON p.oid = i.inhparent \
              WHERE p.relname = ?::text \
              ORDER BY c.relname ASC"
                .to_string(),
            vec![Value::from(PARENT_TABLE)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(PartitionInfo {
            name: row.get_typed::<String>("name")?,
            bound: row.get_typed::<String>("bound")?,
            bytes: row.get_typed::<i64>("bytes")?,
        });
    }
    Ok(out)
}

/// 预建分区：当前月 → 当前月 + [`PREBUILD_MONTHS_AHEAD`]（缺哪个建哪个）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `now`：当前时刻（显式传入，便于验收脚本固定到任意月份测「跨年」）。
///
/// # 返回值
///
/// `(本次新建的分区名, 已存在的分区名)`。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]（父表加锁失败 / 权限不足都会原样上抛）。
pub async fn ensure_partitions(
    ex: &impl SqlExecutor,
    now: chrono::DateTime<chrono::Utc>,
) -> DbResult<(Vec<String>, Vec<String>)> {
    let base = month_start(now);
    let mut created = Vec::new();
    let mut existing = Vec::new();
    for offset in 0..=PREBUILD_MONTHS_AHEAD {
        let month = shift_month(base, offset);
        let name = partition_name(month);
        // 名字只可能来自 partition_name()，仍是标识符，只能用拼接；见文件头「DDL 为什么用字符串拼接」。
        debug_assert!(is_managed_partition(&name));
        let next = shift_month(month, 1);
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {name} PARTITION OF {PARENT_TABLE} \
             FOR VALUES FROM ('{from}') TO ('{to}')",
            from = month.format("%Y-%m-%d"),
            to = next.format("%Y-%m-%d"),
        );
        let affected = ex.exec_sql(sql, Vec::new()).await?;
        // `CREATE TABLE IF NOT EXISTS` 在表已存在时返回 0 行影响数
        if affected == 0 {
            existing.push(name);
        } else {
            created.push(name);
        }
    }
    Ok((created, existing))
}

/// 删除过期分区（早于保留期的那些）。
///
/// # 参数
///
/// - `ex`：执行器；
/// - `current_month`：当前月月初；
/// - `retention_months`：保留月数。
///
/// # 返回值
///
/// 本次删除的分区名（升序）。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn drop_expired_partitions(
    ex: &impl SqlExecutor,
    current_month: chrono::NaiveDate,
    retention_months: i32,
) -> DbResult<Vec<String>> {
    let all: Vec<String> = list_partitions(ex).await?.into_iter().map(|info| info.name).collect();
    let expired = expired_partitions(&all, current_month, retention_months);
    for name in &expired {
        // 不可逆操作前的最后一道校验（见文件头）
        if !is_managed_partition(name) {
            tracing::error!(partition = %name, "分区名不合规范，跳过 DROP");
            continue;
        }
        ex.exec_sql(format!("DROP TABLE IF EXISTS {name}"), Vec::new()).await?;
        tracing::warn!(partition = %name, "已删除过期变更日志分区");
    }
    Ok(expired)
}

/// 完整跑一轮分区运维（预建 + 清理），定时任务与手工触发共用这一个入口。
///
/// # 参数
///
/// - `ex`：执行器（定时任务传全局连接，集成测试传测试库）；
/// - `now`：当前时刻；
/// - `retention_months`：保留月数。
///
/// # 返回值
///
/// 本次运维报告。
///
/// # 异常情况
///
/// 数据库异常返回 [`DbError`]。
pub async fn run_once(
    ex: &impl SqlExecutor,
    now: chrono::DateTime<chrono::Utc>,
    retention_months: i32,
) -> DbResult<MaintenanceReport> {
    let current = month_start(now);
    let (created, existing) = ensure_partitions(ex, now).await?;
    let dropped = drop_expired_partitions(ex, current, retention_months).await?;
    let kept = list_partitions(ex)
        .await?
        .into_iter()
        .filter(|info| {
            parse_partition_month(&info.name)
                .map(|month| month >= oldest_kept_month(current, retention_months))
                .unwrap_or(false)
        })
        .count();
    Ok(MaintenanceReport {
        created,
        existing,
        dropped,
        kept,
        retention_months,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("测试日期应合法")
    }

    fn at(text: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(text)
            .expect("测试时间应合法")
            .with_timezone(&chrono::Utc)
    }

    /// 语义与 V0084 迁移里的动态建分区保持一致（`sys_change_log_YYYY_MM`）。
    #[test]
    fn partition_name_matches_ddl_convention() {
        assert_eq!(partition_name(date("2026-10-01")), "sys_change_log_2026_10");
        assert_eq!(partition_name(date("2027-01-01")), "sys_change_log_2027_01");
        assert_eq!(
            partition_name(date("2026-12-15")),
            "sys_change_log_2026_12",
            "非 1 号也只取年月"
        );
    }

    /// 名字 ⇄ 月份往返一致，且脏名字一律不通过。
    #[test]
    fn partition_name_roundtrip_and_rejects_dirty_names() {
        for text in ["2026-01-01", "2026-10-01", "2029-12-01"] {
            let month = date(text);
            let name = partition_name(month);
            assert_eq!(parse_partition_month(&name), Some(month));
            assert!(is_managed_partition(&name));
        }
        for dirty in [
            "sys_change_log",
            "sys_change_log_2026",
            "sys_change_log_2026_1",
            "sys_change_log_2026_13",
            "sys_change_log_2026_10;DROP TABLE sys_user",
            "sys_change_log_2026_10 ",
            "public.sys_change_log_2026_10",
            "other_table_2026_10",
            "sys_change_log_abc_10",
        ] {
            assert_eq!(parse_partition_month(dirty), None, "{dirty} 不该被解析");
            assert!(!is_managed_partition(dirty), "{dirty} 不该被判为受管分区");
        }
    }

    /// 跨年偏移必须正确（`2026-01` 往前一个月是 `2025-12`）。
    #[test]
    fn shift_month_handles_year_boundary() {
        assert_eq!(shift_month(date("2026-01-01"), -1), date("2025-12-01"));
        assert_eq!(shift_month(date("2026-12-01"), 1), date("2027-01-01"));
        assert_eq!(shift_month(date("2026-06-01"), 0), date("2026-06-01"));
        assert_eq!(shift_month(date("2026-06-01"), -36), date("2023-06-01"));
    }

    /// 保留 36 个月：当前月是 2026-10 → 最早保留 2023-11（含当月共 36 个）。
    #[test]
    fn oldest_kept_month_is_36_months_inclusive() {
        let current = date("2026-10-01");
        assert_eq!(oldest_kept_month(current, 36), date("2023-11-01"));
        // 边界：2023-11 保留，2023-10 过期
        let existing = vec![
            "sys_change_log_2023_09".to_string(),
            "sys_change_log_2023_10".to_string(),
            "sys_change_log_2023_11".to_string(),
            "sys_change_log_2026_09".to_string(),
            "sys_change_log_2026_10".to_string(),
        ];
        assert_eq!(
            expired_partitions(&existing, current, 36),
            vec![
                "sys_change_log_2023_09".to_string(),
                "sys_change_log_2023_10".to_string()
            ]
        );
    }

    /// 保留月数 ≤ 0 时不能把当前月当过期（否则一次配置写错就丢掉当月审计）。
    #[test]
    fn non_positive_retention_never_drops_current_month() {
        let current = date("2026-10-01");
        let existing = vec!["sys_change_log_2026_10".to_string()];
        for retention in [0, -5, 1] {
            assert!(
                expired_partitions(&existing, current, retention).is_empty(),
                "retention={retention} 时当前月不该被删"
            );
        }
    }

    /// 脏名字（不归本模块管）绝不进删除清单。
    #[test]
    fn dirty_names_are_never_expired() {
        let current = date("2026-10-01");
        let existing = vec![
            "sys_change_log_2020_01".to_string(),
            "sys_change_log_backup".to_string(),
            "sys_user".to_string(),
        ];
        assert_eq!(
            expired_partitions(&existing, current, 36),
            vec!["sys_change_log_2020_01".to_string()],
            "只删规范命名且过期的分区"
        );
    }

    /// 预建 + 清理两个动作的「提前量」与 YYYY_MM 形态。
    #[test]
    fn prebuild_targets_current_and_next_month() {
        let now = at("2026-12-31T23:59:00Z");
        let base = month_start(now);
        let names: Vec<String> = (0..=PREBUILD_MONTHS_AHEAD)
            .map(|offset| partition_name(shift_month(base, offset)))
            .collect();
        assert_eq!(
            names,
            vec![
                "sys_change_log_2026_12".to_string(),
                "sys_change_log_2027_01".to_string()
            ],
            "跨年时也要建出下个月"
        );
    }

    /// 报告摘要必须能一眼看出做了什么事（运维按它 grep 日志）。
    #[test]
    fn report_summary_and_changed_flag() {
        let report = MaintenanceReport {
            created: vec!["sys_change_log_2026_11".to_string()],
            existing: vec!["sys_change_log_2026_10".to_string()],
            dropped: vec!["sys_change_log_2023_10".to_string()],
            kept: 36,
            retention_months: 36,
        };
        let summary = report.summary();
        assert!(summary.contains("created=[\"sys_change_log_2026_11\"]"), "{summary}");
        assert!(summary.contains("dropped=[\"sys_change_log_2023_10\"]"), "{summary}");
        assert!(summary.contains("kept=36"), "{summary}");
        assert!(report.changed());
        assert!(!MaintenanceReport::default().changed());
    }

    /// 常量与架构 §4.9 逐字对齐（改这里之前先改文档）。
    #[test]
    fn constants_match_architecture() {
        assert_eq!(RETENTION_MONTHS, 36);
        assert_eq!(PREBUILD_MONTHS_AHEAD, 1);
        assert_eq!(TASK_NAME, "changelog_partition_maintain");
        assert_eq!(CRON, "0 20 1 1 * *");
    }
}

// ============================================================================
// flow_model_stats.rs —— 审批统计（总览/效率/卡点）：时间窗、口径常量、效率与卡点行、汇总与最慢项、两条统计查询
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

// SCOPE-EXEMPT: 机制登记（**不是**放弃数据权限）—— 本文件的两条统计查询（`efficiency_rows` /
// SCOPE-EXEMPT: `bottleneck_rows`）已经过滤可见性：它们调用 `instance_visibility()`（见
// SCOPE-EXEMPT: `flow_model_inbox.rs:115`，内部用 `scope::filter_for(INSTANCE_TARGET)` 生成数据范围片段，
// SCOPE-EXEMPT: 再与「我发起的 ∪ 我待办/已办」取并集 —— 与实例列表接口同一口径，架构 D16）。
// SCOPE-EXEMPT: 闸② 是按**文件**搜 `scope::` 字面量，而片段在本文件拼进 SQL、字面量留在 inbox 部件里，
// SCOPE-EXEMPT: 故按口径显式登记。真正违规的动作是删掉 `instance_visibility()` 调用，不是这条注释。

// 可见性与待办列表同一口径（函数住在 inbox 部件，跨部件引用）
use super::flow_model_inbox::instance_visibility;
use nik::prelude::*;

/// 统计默认回看天数（卡面：`start` / `end` 缺省 = 近 30 天）。
pub const STATS_DEFAULT_DAYS: i64 = 30;

/// 统计时间窗跨度上限（防止一次拖全表）。
pub const STATS_MAX_SPAN_DAYS: i64 = 366;

/// 卡点榜默认条数。
pub const BOTTLENECK_TOP: i64 = 10;

/// 统计时间窗（**左闭右开** `[start, end)`，UTC）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsWindow {
    /// 起始时间（含）。
    pub start: chrono::DateTime<chrono::Utc>,
    /// 结束时间（不含）。
    pub end: chrono::DateTime<chrono::Utc>,
}

impl StatsWindow {
    /// 归一化时间窗：`end` 缺省 = 现在、`start` 缺省 = `end - 30 天`。
    ///
    /// # 异常情况
    ///
    /// `start >= end` / 跨度超上限 → `Err(中文原因)`（handler 翻成 `400 + 65005`）。
    pub fn normalize(
        start: Option<chrono::DateTime<chrono::Utc>>,
        end: Option<chrono::DateTime<chrono::Utc>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> std::result::Result<Self, String> {
        let end = end.unwrap_or(now);
        let start = start.unwrap_or_else(|| end - chrono::Duration::days(STATS_DEFAULT_DAYS));
        if start >= end {
            return Err("时间范围非法：start 必须早于 end".to_string());
        }
        if end - start > chrono::Duration::days(STATS_MAX_SPAN_DAYS) {
            return Err(format!(
                "时间范围非法：跨度不得超过 {STATS_MAX_SPAN_DAYS} 天"
            ));
        }
        Ok(Self { start, end })
    }
}

/// 安全整除（分母 ≤ 0 → 0，绝不返回 NaN / null）。
pub(super) fn divide_round(sum: i64, count: i64) -> i64 {
    if count <= 0 {
        return 0;
    }
    ((sum as f64) / (count as f64)).round() as i64
}

/// 效率统计行（按 `flow_code` 聚合）。
///
/// **总览由这些行折出来**（[`fold_overview`]），保证总览与效率两个接口的数字同源，
/// 也避免同一张表被扫两遍。
#[derive(Debug, Clone, Default)]
pub struct EfficiencyRow {
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 实例数。
    pub total: i64,
    /// 通过数。
    pub approved: i64,
    /// 驳回数。
    pub rejected: i64,
    /// 在跑数（`PENDING`）。
    pub running: i64,
    /// 撤回数。
    pub cancelled: i64,
    /// 发起人撤回数（`WITHDRAWN`）。
    pub withdrawn: i64,
    /// 已结束数（`ended_at IS NOT NULL`）。
    pub finished: i64,
    /// 已结束实例的时长总和（秒）。
    pub sum_duration_seconds: i64,
    /// 最长单个实例时长（秒）。
    pub max_duration_seconds: i64,
}

impl EfficiencyRow {
    /// 平均审批时长（秒；无已结束实例 → `0`）。
    pub fn avg_duration_seconds(&self) -> i64 {
        divide_round(self.sum_duration_seconds, self.finished)
    }
}

/// 总览行（跨流程汇总）。
#[derive(Debug, Clone, Default)]
pub struct OverviewRow {
    /// 实例数。
    pub total: i64,
    /// 通过数。
    pub approved: i64,
    /// 驳回数。
    pub rejected: i64,
    /// 在跑数。
    pub running: i64,
    /// 撤回数。
    pub cancelled: i64,
    /// 发起人撤回数。
    pub withdrawn: i64,
    /// 已结束数。
    pub finished: i64,
    /// 已结束实例的时长总和（秒）。
    pub sum_duration_seconds: i64,
}

impl OverviewRow {
    /// 平均审批时长（秒；无已结束实例 → `0`）。
    pub fn avg_duration_seconds(&self) -> i64 {
        divide_round(self.sum_duration_seconds, self.finished)
    }

    /// 通过率（0–1，保留 4 位；无实例 → `0.0`）。
    pub fn approved_rate(&self) -> f64 {
        if self.total <= 0 {
            return 0.0;
        }
        let rate = self.approved as f64 / self.total as f64;
        (rate * 10000.0).round() / 10000.0
    }
}

/// 跨流程汇总（**纯函数**：单测直接喂行，不需要数据库）。
pub fn fold_overview(rows: &[EfficiencyRow]) -> OverviewRow {
    let mut out = OverviewRow::default();
    for row in rows {
        out.total += row.total;
        out.approved += row.approved;
        out.rejected += row.rejected;
        out.running += row.running;
        out.cancelled += row.cancelled;
        out.withdrawn += row.withdrawn;
        out.finished += row.finished;
        out.sum_duration_seconds += row.sum_duration_seconds;
    }
    out
}

/// 卡点行（按 `node_key` 聚合的平均停留时长）。
#[derive(Debug, Clone, Default)]
pub struct BottleneckRow {
    /// 节点 key。
    pub node_key: String,
    /// 节点名（同一 key 取一个即可）。
    pub node_name: String,
    /// 样本数（已结束的节点轮次）。
    pub samples: i64,
    /// 平均停留（秒，四舍五入）。
    pub avg_dwell_seconds: i64,
    /// 最长停留（秒）。
    pub max_dwell_seconds: i64,
}

/// 最慢节点（平均停留最长；同平均则样本多的优先，再同则 `node_key` 升序）。
///
/// 空集 → `None`（handler 会下发空串 + 0，不下发 null）。
pub fn slowest_of(rows: &[BottleneckRow]) -> Option<&BottleneckRow> {
    let mut best: Option<&BottleneckRow> = None;
    for row in rows {
        best = match best {
            None => Some(row),
            Some(current) => {
                let better = row.avg_dwell_seconds > current.avg_dwell_seconds
                    || (row.avg_dwell_seconds == current.avg_dwell_seconds
                        && row.samples > current.samples)
                    || (row.avg_dwell_seconds == current.avg_dwell_seconds
                        && row.samples == current.samples
                        && row.node_key.as_str() < current.node_key.as_str());
                if better { Some(row) } else { Some(current) }
            }
        };
    }
    best
}

/// 效率统计：按流程聚合窗口内**当前用户可见**的实例。
///
/// 可见性复用 [`instance_visibility`]（与列表接口同一口径）。
/// 平均审批时长 = `ended_at - started_at` 的均值（卡面口径：实例结束时间 − 发起时间）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn efficiency_rows(
    principal: &crate::business::shared::principal::ScopedPrincipal,
    window: StatsWindow,
) -> DbResult<Vec<EfficiencyRow>> {
    let vis = instance_visibility(principal).await?;
    let sql = format!(
        "SELECT i.flow_code AS flow_code, \
                max(i.flow_name) AS flow_name, \
                count(*)::bigint AS total, \
                count(*) FILTER (WHERE i.status = 'APPROVED')::bigint AS approved, \
                count(*) FILTER (WHERE i.status = 'REJECTED')::bigint AS rejected, \
                count(*) FILTER (WHERE i.status = 'PENDING')::bigint AS running, \
                count(*) FILTER (WHERE i.status = 'CANCELLED')::bigint AS cancelled, \
                count(*) FILTER (WHERE i.status = 'WITHDRAWN')::bigint AS withdrawn, \
                count(*) FILTER (WHERE i.ended_at IS NOT NULL)::bigint AS finished, \
                COALESCE(sum(EXTRACT(EPOCH FROM (i.ended_at - i.started_at))) \
                         FILTER (WHERE i.ended_at IS NOT NULL), 0)::bigint AS sum_seconds, \
                COALESCE(max(EXTRACT(EPOCH FROM (i.ended_at - i.started_at))) \
                         FILTER (WHERE i.ended_at IS NOT NULL), 0)::bigint AS max_seconds \
           FROM biz_flow_instance i \
          WHERE i.deleted_at IS NULL AND {vis_sql} \
            AND i.started_at >= ?::text::timestamptz \
            AND i.started_at < ?::text::timestamptz \
          GROUP BY i.flow_code \
          ORDER BY sum_seconds DESC, i.flow_code ASC",
        vis_sql = vis.sql
    );
    let mut params = vis.params;
    params.push(Value::Text(window.start.to_rfc3339()));
    params.push(Value::Text(window.end.to_rfc3339()));
    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(EfficiencyRow {
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            total: row.get_typed("total")?,
            approved: row.get_typed("approved")?,
            rejected: row.get_typed("rejected")?,
            running: row.get_typed("running")?,
            cancelled: row.get_typed("cancelled")?,
            withdrawn: row.get_typed("withdrawn")?,
            finished: row.get_typed("finished")?,
            sum_duration_seconds: row.get_typed("sum_seconds")?,
            max_duration_seconds: row.get_typed("max_seconds")?,
        });
    }
    Ok(out)
}

/// 卡点统计：窗口内可见实例的**各节点平均停留时长**（`biz_flow_instance_node.duration_ms`）。
///
/// 只统计已结束的节点轮次（`duration_ms IS NOT NULL`）：在跑节点没有停留时长，
/// 混进去会把卡点均值拉低（「哪个节点最慢」必须拿完成样本说话）。
/// 聚合走 `idx_biz_flow_instance_node_stat (node_key, status, duration_ms)`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn bottleneck_rows(
    principal: &crate::business::shared::principal::ScopedPrincipal,
    window: StatsWindow,
    limit: i64,
) -> DbResult<Vec<BottleneckRow>> {
    let vis = instance_visibility(principal).await?;
    let sql = format!(
        "SELECT n.node_key AS node_key, \
                max(n.node_name) AS node_name, \
                count(*)::bigint AS samples, \
                COALESCE(avg(n.duration_ms), 0)::float8 AS avg_ms, \
                COALESCE(max(n.duration_ms), 0)::bigint AS max_ms \
           FROM biz_flow_instance_node n \
           JOIN biz_flow_instance i ON i.id = n.instance_id \
          WHERE i.deleted_at IS NULL AND {vis_sql} \
            AND i.started_at >= ?::text::timestamptz \
            AND i.started_at < ?::text::timestamptz \
            AND n.duration_ms IS NOT NULL \
          GROUP BY n.node_key \
          ORDER BY avg_ms DESC, n.node_key ASC \
          LIMIT ?",
        vis_sql = vis.sql
    );
    let mut params = vis.params;
    params.push(Value::Text(window.start.to_rfc3339()));
    params.push(Value::Text(window.end.to_rfc3339()));
    params.push(Value::from(limit.max(1)));
    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let avg_ms: f64 = row.get_typed("avg_ms")?;
        let max_ms: i64 = row.get_typed("max_ms")?;
        out.push(BottleneckRow {
            node_key: row.get_typed("node_key")?,
            node_name: row.get_typed("node_name")?,
            samples: row.get_typed("samples")?,
            avg_dwell_seconds: (avg_ms / 1000.0).round() as i64,
            max_dwell_seconds: (max_ms as f64 / 1000.0).round() as i64,
        });
    }
    Ok(out)
}

// ==================== 单测：任务删除即终止在跑审批单（T12，真库 `TASK_DB_URL`） ====================
//
// 跑法（隔离库，**不要**指 `cloudpivot_test` —— 那张库半死，见 T10/T11 记录）：
//
//   su - postgres -c 'createdb -O cloudpivot cloudpivot_t12'
//   cd backend && TASK_DB_URL='postgres://cloudpivot:<pwd>@127.0.0.1:5432/cloudpivot_t12' \
//     cargo test --lib t12_delete_flow_tests -j 1
//
// 缺 `TASK_DB_URL` 时每条用例打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍全绿）。

// ============================================================================
// task_model_stats.rs —— 统计：按执行模式的完成率统计
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;

use super::task_model_base::*;
use super::task_model_query::*;
use super::task_model_rows::*;
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;

/// 按执行模式统计任务（**可见性 = 数据范围 ∪ 参与 id 数组**，与列表页同一谓词）。
///
/// # 口径（前端卡片直接照这四条解释）
///
/// 1. **只统计顶层任务**（`parent_id IS NULL`）：子任务是父任务的拆解，两边都算会让
///    「完成率」随任务拆解粗细漂移（拆得越细，完成率越好看）；
/// 2. **窗口按 `created_at`** 的 CST 日区间（闭区间）：`created_at < end + 1 天`
///    的写法能吃到 `idx_biz_task_created`，写成 `created_at::date <= end` 会退化成全表扫；
/// 3. `avg_done_hours` 只算 `status = 'DONE' AND done_at IS NOT NULL`；
/// 4. `avg_progress` 排除已取消（取消的任务谈进度没有意义）。
///
/// # 异常情况
///
/// - 片段形态异常 → `DbError::InvalidValue`（`500 + 63008`）；
/// - 数据库异常 → [`DbError`]。
pub async fn task_stats(
    principal: &ScopedPrincipal,
    filter: &StatsFilter,
) -> DbResult<Vec<ExecModeStat>> {
    assert_scope_fragment(principal)?;
    let fragment = scope::filter_for(principal, TASK_TARGET);
    let tail = VISIBLE_OR_TAIL.replacen(scope::FILTER_MARKER, fragment.sql().trim(), 1);
    let scope_ids = my_member_task_ids(principal.user_id()).await?;

    let mut sql = String::with_capacity(768);
    sql.push_str(
        "SELECT t.exec_mode, \
                count(*) AS total, \
                count(*) FILTER (WHERE t.status = 'TODO') AS todo, \
                count(*) FILTER (WHERE t.status = 'DOING') AS doing, \
                count(*) FILTER (WHERE t.status = 'REVIEW') AS review, \
                count(*) FILTER (WHERE t.status = 'DONE') AS done, \
                count(*) FILTER (WHERE t.status = 'CANCELLED') AS cancelled, \
                (avg(EXTRACT(EPOCH FROM (t.done_at - t.created_at)) / 3600.0) \
                    FILTER (WHERE t.status = 'DONE' AND t.done_at IS NOT NULL))::float8 \
                    AS avg_done_hours, \
                (avg(t.progress) FILTER (WHERE t.status <> 'CANCELLED'))::float8 \
                    AS avg_progress \
           FROM biz_task t \
          WHERE t.deleted_at IS NULL \
            AND t.parent_id IS NULL \
            AND t.created_at >= ?::date \
            AND t.created_at < (?::date + 1)",
    );
    sql.push_str(&tail);
    let mut params: Vec<Value> = vec![Value::from(filter.start), Value::from(filter.end)];
    params.extend(fragment.params().to_vec());
    params.extend(visible_params(&scope_ids));
    if let Some(project_id) = filter.project_id {
        sql.push_str(" AND t.project_id = ?::uuid");
        params.push(Value::from(&project_id));
    }
    if let Some(owner_id) = filter.mine {
        sql.push_str(" AND t.owner_id = ?::uuid");
        params.push(Value::from(&owner_id));
    }
    sql.push_str(" GROUP BY t.exec_mode ORDER BY t.exec_mode");

    let rows = db().find_with_params(&sql, params).await?;
    let mut stats = Vec::with_capacity(rows.len());
    for row in &rows {
        stats.push(ExecModeStat {
            exec_mode: row.get_typed("exec_mode")?,
            total: row.get_typed("total")?,
            todo: row.get_typed("todo")?,
            doing: row.get_typed("doing")?,
            review: row.get_typed("review")?,
            done: row.get_typed("done")?,
            cancelled: row.get_typed("cancelled")?,
            avg_done_hours: row.get_typed("avg_done_hours")?,
            avg_progress: row.get_typed("avg_progress")?,
        });
    }
    Ok(stats)
}

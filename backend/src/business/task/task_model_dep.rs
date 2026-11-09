// ============================================================================
// task_model_dep.rs —— 依赖与甘特：前置/后继/环检测/甘特取数
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_model_base::*;
use super::task_model_query::*;
use super::task_model_rows::*;
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::business::task::dep;

/// 我依赖谁（前置列表，按截止日升序、无期限垫底）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_prerequisites(task_id: Uuid) -> DbResult<Vec<DependencyRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {DEP_PEER_COLUMNS} \
                   FROM biz_task_dependency d \
                   JOIN biz_task p ON p.id = d.depends_on_id AND p.deleted_at IS NULL \
                   LEFT JOIN sys_user u ON u.id = p.owner_id \
                  WHERE d.task_id = ?::uuid \
                  ORDER BY p.due_date ASC NULLS LAST, p.id ASC"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(DependencyRow::from_row).collect()
}

/// 谁依赖我（后继列表）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_successors(task_id: Uuid) -> DbResult<Vec<DependencyRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {DEP_PEER_COLUMNS} \
                   FROM biz_task_dependency d \
                   JOIN biz_task p ON p.id = d.task_id AND p.deleted_at IS NULL \
                   LEFT JOIN sys_user u ON u.id = p.owner_id \
                  WHERE d.depends_on_id = ?::uuid \
                  ORDER BY p.due_date ASC NULLS LAST, p.id ASC"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(DependencyRow::from_row).collect()
}

/// 仍然阻塞本任务的前置（`DONE` / `CANCELLED` 已解除，不算）。
///
/// 口徑与 [`crate::business::task::dep::is_blocking_status`] 是同一套：
/// 这里的 SQL 只是把「已解除」的两态前置**先过滤掉**（少传数据），
/// 最终判定仍由纯逻辑做 —— 两处口径不会分叉。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn blocking_prerequisites(task_id: Uuid) -> DbResult<Vec<DependencyRow>> {
    let rows = list_prerequisites(task_id).await?;
    Ok(rows
        .into_iter()
        .filter(|row| dep::is_blocking_status(&row.status))
        .collect())
}

/// 全量 FS 依赖边（环检测的输入）。
///
/// # 为什么一次全取
///
/// 环检测要在**事务内**看到最新边集合（并发加依赖时，读旧集合会漏判）。
/// 依赖表规模远小于任务表（一条任务最多几条依赖），一次取全表再在内存里
/// 做 BFS，比「递归 CTE 一层层问库」简单且可单测（[`dep::find_cycle`]）。
/// 深度由 [`dep::MAX_CYCLE_DEPTH`] 兜住，不会因为链长而爆栈。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn dependency_edges(ex: &impl SqlExecutor) -> DbResult<Vec<dep::DepEdge>> {
    let rows = ex
        .query_sql(
            "SELECT task_id, depends_on_id FROM biz_task_dependency WHERE dep_type = 'FS'".to_string(),
            vec![],
        )
        .await?;
    let mut edges = Vec::with_capacity(rows.len());
    for row in &rows {
        edges.push(dep::DepEdge::new(
            row.get_typed("task_id")?,
            row.get_typed("depends_on_id")?,
        ));
    }
    Ok(edges)
}

/// 两个任务之间是否已有 FS 依赖（幂等判定 / 删除定位）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_dependency(ex: &impl SqlExecutor, task_id: Uuid, depends_on_id: Uuid) -> DbResult<Option<i32>> {
    let rows = ex
        .query_sql(
            "SELECT lag_days FROM biz_task_dependency \
              WHERE task_id = ?::uuid AND depends_on_id = ?::uuid AND dep_type = 'FS'"
                .to_string(),
            vec![Value::from(&task_id), Value::from(&depends_on_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row.get_typed("lag_days")?)),
        None => Ok(None),
    }
}

/// 插入一条 FS 依赖。
///
/// # 参数
///
/// - `ex`：事务句柄（**必须与环检测在同一个事务里**，否则并发能绕过检测）；
/// - `task_id` / `depends_on_id`：后继 / 前置；
/// - `lag_days`：滞后天数（>= 0，本版只存不算排程）；
/// - `operator`：操作人（写 `create_by`）。
///
/// # 返回值
///
/// 受影响行数（`ON CONFLICT DO NOTHING` 命中时为 0 —— handler 会把它解释成幂等）。
///
/// # 异常情况
///
/// 数据库异常（含 `ck_biz_task_dependency_not_self` / 唯一索引违约）→ [`DbError`]。
pub async fn insert_dependency(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    depends_on_id: Uuid,
    lag_days: i32,
    operator: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "INSERT INTO biz_task_dependency (task_id, depends_on_id, dep_type, lag_days, created_at, create_by) \
         VALUES (?::uuid, ?::uuid, 'FS', ?::int4, now(), ?::uuid) \
         ON CONFLICT (task_id, depends_on_id) DO NOTHING"
            .to_string(),
        vec![
            Value::from(&task_id),
            Value::from(&depends_on_id),
            Value::I32(lag_days),
            Value::from(&operator),
        ],
    )
    .await
}

/// 删除两个任务之间的依赖（**双向匹配**：谁是前置谁是后继都删得到）。
///
/// # 返回值
///
/// 受影响行数（0 = 这条依赖不存在 → handler 回 404）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn delete_dependency(ex: &impl SqlExecutor, task_id: Uuid, peer_id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM biz_task_dependency \
          WHERE (task_id = ?::uuid AND depends_on_id = ?::uuid) \
             OR (task_id = ?::uuid AND depends_on_id = ?::uuid)"
            .to_string(),
        vec![
            Value::from(&task_id),
            Value::from(&peer_id),
            Value::from(&peer_id),
            Value::from(&task_id),
        ],
    )
    .await
}

/// 读取两条任务（均未软删）的父任务 / 项目；任一不存在 → `None`。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn task_relation(id_a: Uuid, id_b: Uuid) -> DbResult<Option<TaskRelation>> {
    let rows = db()
        .find_with_params(
            "SELECT id, parent_id, project_id FROM biz_task \
              WHERE id IN (?::uuid, ?::uuid) AND deleted_at IS NULL",
            vec![Value::from(&id_a), Value::from(&id_b)],
        )
        .await?;
    let mut relation = TaskRelation::default();
    let mut found = 0;
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let parent_id: Option<Uuid> = row.get_typed("parent_id")?;
        let project_id: Option<Uuid> = row.get_typed("project_id")?;
        if id == id_a {
            relation.parent_a = parent_id;
            relation.project_a = project_id;
            found += 1;
        } else if id == id_b {
            relation.parent_b = parent_id;
            relation.project_b = project_id;
            found += 1;
        }
    }
    if found == 2 { Ok(Some(relation)) } else { Ok(None) }
}

/// 一批任务的编号 / 标题（环路径文案用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn dependency_labels(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<Vec<dep::DepLabel>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let array = Value::Text(format!(
        "{{{}}}",
        ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
    ));
    let rows = ex
        .query_sql(
            "SELECT id, task_no, title FROM biz_task WHERE id = ANY(?::text::uuid[])".to_string(),
            vec![array],
        )
        .await?;
    let mut labels = Vec::with_capacity(rows.len());
    for row in &rows {
        labels.push(dep::DepLabel {
            id: row.get_typed("id")?,
            task_no: row.get_typed("task_no")?,
            title: row.get_typed("title")?,
        });
    }
    Ok(labels)
}

/// 甘特图任务集合（**可见性 = 数据范围 ∪ 参与 id 数组**，与列表页同一谓词）。
///
/// # 返回
///
/// [`GanttResult`]：超过 [`dep::MAX_GANTT_TASKS`] 时按排期升序截断并置 `truncated`。
///
/// # 异常情况
///
/// - 片段形态异常 → `DbError::InvalidValue`（`500 + 63008`）；
/// - 数据库异常 → [`DbError`]。
pub async fn gantt_tasks(
    principal: &ScopedPrincipal,
    filter: &GanttFilter,
    range: &dep::GanttRange,
) -> DbResult<GanttResult> {
    assert_scope_fragment(principal)?;
    let fragment = scope::filter_for(principal, TASK_TARGET);
    let tail = VISIBLE_OR_TAIL.replacen(scope::FILTER_MARKER, fragment.sql().trim(), 1);
    let scope_ids = my_member_task_ids(principal.user_id()).await?;

    let mut sql = String::with_capacity(1024);
    sql.push_str(
        "SELECT t.id, t.task_no, t.title, t.status, t.priority, t.progress, t.owner_id, \
         COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
         t.start_date, t.due_date, t.parent_id, t.created_at \
           FROM biz_task t LEFT JOIN sys_user u ON u.id = t.owner_id \
          WHERE t.deleted_at IS NULL \
            AND (t.start_date IS NOT NULL OR t.due_date IS NOT NULL) \
            AND COALESCE(t.start_date, t.due_date) <= ?::date \
            AND COALESCE(t.due_date, t.start_date) >= ?::date",
    );
    sql.push_str(&tail);
    let mut params: Vec<Value> = vec![Value::from(range.end), Value::from(range.start)];
    params.extend(fragment.params().to_vec());
    params.extend(visible_params(&scope_ids));
    if let Some(owner_id) = filter.owner_id {
        sql.push_str(" AND t.owner_id = ?::uuid");
        params.push(Value::from(&owner_id));
    }
    if let Some(project_id) = filter.project_id {
        sql.push_str(" AND t.project_id = ?::uuid");
        params.push(Value::from(&project_id));
    }
    if let Some(me) = filter.mine {
        sql.push_str(" AND t.owner_id = ?::uuid");
        params.push(Value::from(&me));
    }
    sql.push_str(" ORDER BY COALESCE(t.start_date, t.due_date) ASC, t.due_date ASC NULLS LAST, t.id ASC");
    sql.push_str(&format!(" LIMIT {}", dep::MAX_GANTT_TASKS + 1));

    let rows = db().find_with_params(&sql, params).await?;
    let truncated = rows.len() as i64 > dep::MAX_GANTT_TASKS;
    let rows = if truncated {
        &rows[..dep::MAX_GANTT_TASKS as usize]
    } else {
        &rows[..]
    };

    // 一次算出「被未完成前置阻塞」的集合（避免 N+1）
    let mut ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut tasks = Vec::with_capacity(rows.len());
    let me = principal.user_id();
    let now = Utc::now();
    for row in rows {
        let status: String = row.get_typed("status")?;
        let owner_id: Uuid = row.get_typed("owner_id")?;
        let created_at: chrono::DateTime<Utc> = row.get_typed("created_at")?;
        let is_new = is_new_task_at(&status, owner_id, created_at, me, now);
        let task = GanttTaskRow {
            id: row.get_typed("id")?,
            task_no: row.get_typed("task_no")?,
            title: row.get_typed("title")?,
            status,
            priority: row.get_typed("priority")?,
            progress: row.get_typed("progress")?,
            owner_id,
            owner_name: row.get_typed("owner_name")?,
            start_date: row.get_typed("start_date")?,
            due_date: row.get_typed("due_date")?,
            parent_id: row.get_typed("parent_id")?,
            blocked: false,
            is_new,
        };
        ids.push(task.id);
        tasks.push(task);
    }
    if !ids.is_empty() {
        let blocked = blocked_task_ids(&ids).await?;
        for task in &mut tasks {
            task.blocked = blocked.contains(&task.id);
        }
    }
    Ok(GanttResult {
        tasks,
        truncated,
        member_scope_truncated: scope_ids.truncated,
    })
}

/// 一批任务里「有未完成前置」的那些 id（甘特条画斜纹用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub(super) async fn blocked_task_ids(ids: &[Uuid]) -> DbResult<Vec<Uuid>> {
    let array = Value::Text(format!(
        "{{{}}}",
        ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
    ));
    let rows = db()
        .find_with_params(
            "SELECT DISTINCT d.task_id AS id \
               FROM biz_task_dependency d \
               JOIN biz_task p ON p.id = d.depends_on_id AND p.deleted_at IS NULL \
              WHERE d.task_id = ANY(?::text::uuid[]) \
                AND d.dep_type = 'FS' \
                AND p.status NOT IN ('DONE', 'CANCELLED')",
            vec![array],
        )
        .await?;
    let mut blocked = Vec::with_capacity(rows.len());
    for row in &rows {
        blocked.push(row.get_typed("id")?);
    }
    Ok(blocked)
}

/// 甘特图里要画的依赖边（**两端都在给定集合里**才画）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn gantt_edges(ids: &[Uuid]) -> DbResult<Vec<dep::DepEdge>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let array = Value::Text(format!(
        "{{{}}}",
        ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",")
    ));
    let rows = db()
        .find_with_params(
            "SELECT task_id, depends_on_id FROM biz_task_dependency \
              WHERE dep_type = 'FS' \
                AND task_id = ANY(?::text::uuid[]) \
                AND depends_on_id = ANY(?::text::uuid[])",
            vec![array.clone(), array],
        )
        .await?;
    let mut edges = Vec::with_capacity(rows.len());
    for row in &rows {
        edges.push(dep::DepEdge::new(
            row.get_typed("task_id")?,
            row.get_typed("depends_on_id")?,
        ));
    }
    Ok(edges)
}

// ==================== T5b：Hermes kanban 集成 ====================

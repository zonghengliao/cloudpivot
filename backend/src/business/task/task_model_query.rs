// ============================================================================
// task_model_query.rs —— 只读查询：列表/详情/我的任务/协作人/附件/用户校验/AI 轮询目标
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_model_base::*;
use super::task_model_rows::*;
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::business::task::state::{self, ChildSnapshot};

/// 返回「我参与」的 task_id 集合（决策 D6）。
///
/// 口径 = 负责人 ∪ 创建人 ∪ 分派人 ∪ 协作人/关注者：
/// - 三个列各自有索引（`idx_biz_task_owner_status_due` / `idx_biz_task_creator` /
///   `idx_biz_task_assigner_status`）→ 单条查询即可 `BitmapOr` 三路；
/// - 协作人走 `idx_biz_task_member_user`（`user_id` 在前的复合索引），
///   **刻意不 join `biz_task`**：软删由外层主查询的 `t.deleted_at IS NULL` 兜住，
///   而「按 user_id 取协作行」一旦带 join，规划器会退化成 `biz_task` 的全表 Hash Join
///   （实测 5 万行时 6.8 ms、20 万行会线性上升）——这正是 D6 要避免的形态；
/// - 只取 `MEMBER_SCOPE_LIMIT + 1` 条，够判断是否越限（[`MyScopeIds::truncated`]）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn my_member_task_ids(me: Uuid) -> DbResult<MyScopeIds> {
    let rows = db()
        .find_with_params(
            "SELECT t.id FROM biz_task t \
              WHERE t.deleted_at IS NULL \
                AND (t.owner_id = ?::uuid OR t.creator_id = ?::uuid OR t.assigner_id = ?::uuid) \
             UNION \
             SELECT m.task_id FROM biz_task_member m WHERE m.user_id = ?::uuid \
             LIMIT ?",
            vec![
                Value::from(&me),
                Value::from(&me),
                Value::from(&me),
                Value::from(&me),
                Value::I64(MEMBER_SCOPE_LIMIT + 1),
            ],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len().min(MEMBER_SCOPE_LIMIT as usize));
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        ids.push(id);
    }
    let truncated = ids.len() as i64 > MEMBER_SCOPE_LIMIT;
    if truncated {
        ids.truncate(MEMBER_SCOPE_LIMIT as usize);
    }
    Ok(MyScopeIds { ids, truncated })
}

/// 分页业务过滤条件构造：返回 SQL 片段，并把参数按**文本顺序**追加进 `params`。
///
/// 调用顺序固定为：先 `visible_params`（4 个「我」），再本函数追加的筛选参数 ——
/// 与 SQL 里 `?` 出现的先后完全一致（`{FILTER}` 片段参数由 `paginate` 前置到最前）。
pub(super) fn build_filters(filter: &TaskFilter, params: &mut Vec<Value>) -> String {
    let mut sql = String::new();
    if let Some(list) = filter.statuses.as_ref().filter(|v| !v.is_empty()) {
        // T9-A：状态多选。用 `IN (?,?,…)` 而不是 `ANY(?)` —— `params` 是
        // `Vec<Value>`，逐个 push `Value::Text` 最稳妥（数组类型要确认 nik 的
        // `Value` 支不支持），且占位符个数与 `list` 长度严格一致。
        //
        // 参数顺序红线：本片段位于业务过滤**最前**，`?` 的文本顺序与 push 顺序
        // 一一对应；`statuses` 非空时直接跳过下面的单值 `status`（优先，不是叠加）。
        let holders = std::iter::repeat_n("?", list.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND t.status IN ({holders})"));
        for status in list {
            params.push(Value::Text(status.clone()));
        }
    } else if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(priority) = filter.priority.as_deref() {
        sql.push_str(" AND t.priority = ?");
        params.push(Value::Text(priority.to_string()));
    }
    if let Some(owner_id) = filter.owner_id {
        sql.push_str(" AND t.owner_id = ?::uuid");
        params.push(Value::from(&owner_id));
    }
    if let Some(project_id) = filter.project_id {
        // T7c：项目分组筛选。**只做过滤，不做可见性** —— 可见性由 VISIBLE_OR_TAIL 决定，
        // 因此「按项目筛选」永远不会绕过 T1（项目不是权限边界）。
        sql.push_str(" AND t.project_id = ?::uuid");
        params.push(Value::from(&project_id));
    }
    if let Some(source) = filter.source.as_deref() {
        // P-FLOW-1A：`source=FLOW` 只看流程生成的任务。**只做过滤，不做可见性**。
        sql.push_str(" AND t.source::text = ?");
        params.push(Value::Text(source.to_string()));
    }
    if let Some(from) = filter.due_from {
        sql.push_str(" AND t.due_date >= ?::date");
        params.push(Value::from(from));
    }
    if let Some(to) = filter.due_to {
        sql.push_str(" AND t.due_date <= ?::date");
        params.push(Value::from(to));
    }
    if let Some(keyword) = filter.keyword.as_deref() {
        let pattern = format!("%{}%", escape_like(keyword));
        sql.push_str(" AND (t.title ILIKE ? OR COALESCE(t.description, '') ILIKE ?)");
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if filter.root_only {
        // T8-C：只返回顶层任务。**无参数的常量片段** —— 不 push 任何 `Value`，
        // 因此调用方（`page_visible`）的参数顺序、`scope::paginate` 前置的片段参数
        // 以及 `$1/$2…` 的编号全部不变（`root_only_tests` 里有硬断言）。
        // 这里同一份 SQL 既用于数据页也用于 `SELECT COUNT(*)`（`scope::paginate`
        // 用去掉 ORDER BY 的前缀做 count），所以 `total` 天然与 `list` 同口径。
        sql.push_str(" AND t.parent_id IS NULL");
    }
    sql
}

/// 任务分页（**可见性 = 数据范围 ∪ 参与 id 数组**）。
///
/// # 参数
///
/// - `principal`：当前用户的数据范围（`ScopedPrincipal`，只能由 `principal::resolve` 构造）；
/// - `filter`：状态 / 优先级 / 负责人 / 截止范围 / 关键词；
/// - `page` / `page_size`：分页（已规范化）。
///
/// # 返回值
///
/// [`VisiblePage`]，排序 `截止日升序（NULL 最后）→ 创建时间倒序`。
/// `member_scope_truncated = true` 表示「我参与」id 超过 2000、本次只按范围过滤。
///
/// # 异常情况
///
/// - 片段形态异常 → `DbError::InvalidValue`（见 [`assert_scope_fragment`]）；
/// - 数据库异常 → [`DbError`]。
pub async fn page_visible(
    principal: &ScopedPrincipal,
    filter: &TaskFilter,
    page: u64,
    page_size: u64,
) -> DbResult<VisiblePage> {
    assert_scope_fragment(principal)?;
    // ① 先解析「我参与」id 数组（一次索引查询；D6 的关键：把它算成集合，
    //    而不是让优化器在 OR 里现场判断）。
    let scope_ids = my_member_task_ids(principal.user_id()).await?;

    let mut sql = String::with_capacity(1024);
    sql.push_str("SELECT ");
    sql.push_str(CARD_COLUMNS);
    sql.push_str(CARD_FROM);
    sql.push_str(" WHERE t.deleted_at IS NULL");
    sql.push_str(VISIBLE_OR_TAIL);

    // 参数顺序：参与 id 数组在前（`{FILTER}` 片段参数由 paginate 前置），
    // 业务过滤参数在后 —— 与 SQL 里 `?` 的文本顺序一致。
    let mut params = visible_params(&scope_ids);
    let filters = build_filters(filter, &mut params);
    sql.push_str(&filters);

    let Page {
        list,
        total,
        page,
        size,
        pages,
    } = scope::paginate(
        TASK_TARGET,
        &sql,
        params,
        Some("ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC"),
        page,
        page_size,
        principal,
    )
    .await?;

    let rows = mark_is_new(
        list.iter()
            .map(TaskCardRow::from_row)
            .collect::<DbResult<Vec<_>>>()?,
        principal.user_id(),
    );
    Ok(VisiblePage {
        list: rows,
        total,
        page,
        size,
        pages,
        member_scope_truncated: scope_ids.truncated,
    })
}

/// 任务详情（同样受可见性约束：看不见的任务一律 404，**不暴露「存在但无权」**）。
///
/// # 异常情况
///
/// 数据库异常 / 片段形态异常 → [`DbError`]。
pub async fn find_detail(principal: &ScopedPrincipal, id: Uuid) -> DbResult<Option<TaskDetailRow>> {
    assert_scope_fragment(principal)?;
    let row = find_detail_with_visibility(principal, Some(id), None).await?;
    Ok(row)
}

/// 详情查询内核：`id` 与 `parent_id` 二者传一个（`None` 表示该维不加条件）。
pub(super) async fn find_detail_with_visibility(
    principal: &ScopedPrincipal,
    id: Option<Uuid>,
    parent_id: Option<Uuid>,
) -> DbResult<Option<TaskDetailRow>> {
    let fragment = scope::filter_for(principal, TASK_TARGET);
    let tail = VISIBLE_OR_TAIL.replacen(scope::FILTER_MARKER, fragment.sql().trim(), 1);

    let mut sql = String::with_capacity(1024);
    sql.push_str("SELECT ");
    sql.push_str(CARD_COLUMNS);
    sql.push_str(
        ", t.description, t.creator_id, \
         COALESCE(NULLIF(cu.real_name, ''), cu.username, '') AS creator_name, \
         t.assigner_id, \
         COALESCE(NULLIF(au.real_name, ''), au.username, '') AS assigner_name, \
         COALESCE(d.dept_name, '') AS dept_name, \
         t.parent_id, pt.task_no AS parent_task_no, t.project_id, t.done_at, \
         t.estimate_hours::float8 AS estimate_hours, t.actual_hours::float8 AS actual_hours, \
         t.cancel_reason, t.reject_reason, t.approval_instance_id, \
         t.exec_mode, t.hermes_board, t.hermes_task_id, t.hermes_status, \
         t.ai_summary, t.ai_result::text AS ai_result ",
    );
    sql.push_str(CARD_FROM);
    sql.push_str(" LEFT JOIN sys_user cu ON cu.id = t.creator_id");
    sql.push_str(" LEFT JOIN sys_user au ON au.id = t.assigner_id");
    sql.push_str(" LEFT JOIN sys_dept d ON d.id = t.dept_id");
    sql.push_str(" LEFT JOIN biz_task pt ON pt.id = t.parent_id");
    sql.push_str(" WHERE t.deleted_at IS NULL");
    sql.push_str(&tail);

    // 参数顺序必须与 SQL 文本顺序一致：先 `id`（若在可见性之前）——这里刻意把
    // id / parent_id 放在可见性谓词**之后**，于是参数也是「片段 + 参与 id 数组 + 定位」。
    let scope_ids = my_member_task_ids(principal.user_id()).await?;
    let mut params = fragment.params().to_vec();
    params.extend(visible_params(&scope_ids));
    if let Some(id) = id {
        sql.push_str(" AND t.id = ?::uuid");
        params.push(Value::from(&id));
    }
    if let Some(parent_id) = parent_id {
        sql.push_str(" AND t.parent_id = ?::uuid");
        params.push(Value::from(&parent_id));
    }
    sql.push_str(" LIMIT 1");

    let rows = db().find_with_params(&sql, params).await?;
    match rows.first() {
        Some(row) => Ok(Some(detail_from_row(row)?)),
        None => Ok(None),
    }
}

/// 详情行构造（卡片列 + 详情列）。
pub(super) fn detail_from_row(row: &Row) -> DbResult<TaskDetailRow> {
    Ok(TaskDetailRow {
        card: TaskCardRow::from_row(row)?,
        description: row.get_typed("description")?,
        creator_id: row.get_typed("creator_id")?,
        creator_name: row.get_typed("creator_name")?,
        assigner_id: row.get_typed("assigner_id")?,
        assigner_name: row.get_typed("assigner_name")?,
        dept_name: row.get_typed("dept_name")?,
        parent_id: row.get_typed("parent_id")?,
        parent_task_no: row.get_typed("parent_task_no")?,
        project_id: row.get_typed("project_id")?,
        done_at: row.get_typed("done_at")?,
        estimate_hours: row.get_typed("estimate_hours")?,
        actual_hours: row.get_typed("actual_hours")?,
        cancel_reason: row.get_typed("cancel_reason")?,
        reject_reason: row.get_typed("reject_reason")?,
        approval_instance_id: row.get_typed("approval_instance_id")?,
        exec_mode: row.get_typed("exec_mode")?,
        hermes_board: row.get_typed("hermes_board")?,
        hermes_task_id: row.get_typed("hermes_task_id")?,
        hermes_status: row.get_typed("hermes_status")?,
        ai_summary: row.get_typed("ai_summary")?,
        ai_result: row.get_typed("ai_result")?,
    })
}

/// 子任务列表（**同样按可见性过滤**：看不见的子任务不该出现在详情页左栏）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_children(
    principal: &ScopedPrincipal,
    parent_id: Uuid,
) -> DbResult<Vec<TaskCardRow>> {
    assert_scope_fragment(principal)?;
    let fragment = scope::filter_for(principal, TASK_TARGET);
    let tail = VISIBLE_OR_TAIL.replacen(scope::FILTER_MARKER, fragment.sql().trim(), 1);

    let mut sql = String::with_capacity(768);
    sql.push_str("SELECT ");
    sql.push_str(CARD_COLUMNS);
    sql.push_str(CARD_FROM);
    sql.push_str(" WHERE t.deleted_at IS NULL");
    sql.push_str(&tail);
    sql.push_str(" AND t.parent_id = ?::uuid ORDER BY t.created_at ASC, t.id ASC");

    let scope_ids = my_member_task_ids(principal.user_id()).await?;
    let mut params = fragment.params().to_vec();
    params.extend(visible_params(&scope_ids));
    params.push(Value::from(&parent_id));

    let rows = db().find_with_params(&sql, params).await?;
    rows.iter().map(TaskCardRow::from_row).collect()
}

/// 协作人 / 关注者列表（详情页右栏）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_members(task_id: Uuid) -> DbResult<Vec<TaskMemberRow>> {
    let rows = db()
        .find_with_params(
            "SELECT m.user_id, \
                    COALESCE(NULLIF(u.real_name, ''), u.username, '') AS user_name, \
                    m.role, m.created_at \
               FROM biz_task_member m LEFT JOIN sys_user u ON u.id = m.user_id \
              WHERE m.task_id = ?::uuid \
              ORDER BY m.created_at ASC, m.user_id ASC",
            vec![Value::from(&task_id)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(TaskMemberRow {
            user_id: row.get_typed("user_id")?,
            user_name: row.get_typed("user_name")?,
            role: row.get_typed("role")?,
            created_at: row.get_typed("created_at")?,
        });
    }
    Ok(list)
}

/// 附件列表（详情页右栏）。
///
/// `file_name` 由 [`display_file_name`] 从「原始名 / 存储名」算出，**不带路径**。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_attachments(task_id: Uuid) -> DbResult<Vec<TaskAttachmentRow>> {
    let rows = db()
        .find_with_params(
            "SELECT a.id, a.file_id, \
                    COALESCE(f.original_name, '') AS original_name, \
                    COALESCE(f.file_name, '') AS file_name, \
                    COALESCE(f.file_size, 0) AS file_size, a.sort_order, \
                    a.source, a.ai_profile \
               FROM biz_task_attachment a LEFT JOIN sys_file f ON f.id = a.file_id \
              WHERE a.task_id = ?::uuid \
              ORDER BY a.sort_order ASC, a.id ASC",
            vec![Value::from(&task_id)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        let stored: String = row.get_typed("file_name")?;
        let original: Option<String> = row.get_typed("original_name")?;
        list.push(TaskAttachmentRow {
            id: row.get_typed("id")?,
            file_id: row.get_typed("file_id")?,
            file_name: display_file_name(original.as_deref(), &stored),
            file_size: row.get_typed("file_size")?,
            sort_order: row.get_typed("sort_order")?,
            source: row.get_typed("source")?,
            ai_profile: row.get_typed("ai_profile")?,
        });
    }
    Ok(list)
}

/// 「我的任务」取数：我负责的、未终态（`DONE` / `CANCELLED` 不进「我的任务」）的任务。
///
/// 排序 `due_date ASC NULLS LAST` 让分组在 Rust 侧只需按桶归类，不需要二次排序。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn my_tasks(me: Uuid) -> DbResult<Vec<TaskCardRow>> {
    let mut sql = String::with_capacity(512);
    sql.push_str("SELECT ");
    sql.push_str(CARD_COLUMNS);
    sql.push_str(CARD_FROM);
    sql.push_str(&format!(
        " WHERE t.owner_id = ?::uuid AND t.deleted_at IS NULL \
           AND t.status IN ({}) \
           ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC",
        open_states_sql()
    ));
    let rows = db().find_with_params(&sql, vec![Value::from(&me)]).await?;
    let rows = rows
        .iter()
        .map(TaskCardRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    // F2：口径与「全部」列表 / 菜单徽标一致（同一张卡片、同一个 NEW 标记）。
    Ok(mark_is_new(rows, me))
}

/// 逾期扫描（未终态 ∧ `due_date < today`）。
///
/// # 为什么这个函数本卡就写、但接口属 T7
///
/// 它是**部分索引 `idx_biz_task_due_open` 的唯一使用者**，而「索引生效」是本卡的验收项：
/// 索引的 WHERE 谓词必须与这里的谓词逐字一致，否则 PG 不会选它。T7 的临期/逾期提醒
/// 定时任务直接复用本函数（`limit` 控制单批扫描量），所以它不是死代码，
/// 而是「索引 ↔ 查询形态」这一对契约的锚点。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn scan_overdue(today: NaiveDate, limit: i64) -> DbResult<Vec<TaskCardRow>> {
    let mut sql = String::with_capacity(512);
    sql.push_str("SELECT ");
    sql.push_str(CARD_COLUMNS);
    sql.push_str(CARD_FROM);
    sql.push_str(
        " WHERE t.status NOT IN ('DONE', 'CANCELLED') \
           AND t.due_date < ?::date \
           AND t.deleted_at IS NULL \
           ORDER BY t.due_date ASC, t.id ASC \
           LIMIT ?",
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(today), Value::I64(limit)])
        .await?;
    rows.iter().map(TaskCardRow::from_row).collect()
}

/// 按主键取任务（**不做可见性过滤**，只给写路径判断权限用；读接口不许用它）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_raw(id: Uuid) -> DbResult<Option<TaskDetailRow>> {
    let row = db()
        .find_with_params(
            "SELECT t.id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review, \
                    t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
                    t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at, \
                    (SELECT count(*) FROM biz_task c \
                      WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count, \
                    (SELECT count(*) FROM biz_task c \
                      WHERE c.parent_id = t.id AND c.deleted_at IS NULL \
                        AND c.status = 'DONE') AS child_done_count, \
                    t.description, t.creator_id, \
                    COALESCE(NULLIF(cu.real_name, ''), cu.username, '') AS creator_name, \
                    t.assigner_id, \
                    COALESCE(NULLIF(au.real_name, ''), au.username, '') AS assigner_name, \
                    COALESCE(d.dept_name, '') AS dept_name, \
                    t.parent_id, pt.task_no AS parent_task_no, t.project_id, \
                    COALESCE(pj.project_no, '') AS project_code, \
                    COALESCE(pj.project_name, '') AS project_name, t.done_at, \
                    t.estimate_hours::float8 AS estimate_hours, \
                    t.actual_hours::float8 AS actual_hours, \
                    t.cancel_reason, t.reject_reason, t.approval_instance_id, \
                    t.exec_mode, t.hermes_board, t.hermes_task_id, t.hermes_status, \
                    t.ai_summary, t.ai_result::text AS ai_result \
               FROM biz_task t \
               LEFT JOIN sys_user u ON u.id = t.owner_id \
               LEFT JOIN sys_user cu ON cu.id = t.creator_id \
               LEFT JOIN sys_user au ON au.id = t.assigner_id \
               LEFT JOIN sys_dept d ON d.id = t.dept_id \
               LEFT JOIN biz_task pt ON pt.id = t.parent_id \
               LEFT JOIN biz_project pj ON pj.id = t.project_id AND pj.deleted_at IS NULL \
              WHERE t.id = ?::uuid AND t.deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match row.first() {
        Some(row) => Ok(Some(detail_from_row(row)?)),
        None => Ok(None),
    }
}

/// 未完成子任务数（删除父任务前的闸；也用于 T2 的「父任务完成前置检查」）。
///
/// 这里保留 `NOT IN ('DONE','CANCELLED')` 写法（不用 [`open_states_sql`]）：本查询靠
/// `idx_biz_task_parent`（`parent_id` 等值）定位，状态只是行过滤，索引选型与它无关；
/// 而「未完成」这个说法在可读性上更直观。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn count_open_children(id: Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) AS n FROM biz_task \
          WHERE parent_id = ?::uuid AND deleted_at IS NULL \
            AND status NOT IN ('DONE', 'CANCELLED')",
        vec![Value::from(&id)],
    )
    .await
}

/// 用户所属部门（创建任务时写 `dept_id` 快照）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_dept(user_id: Uuid) -> DbResult<Option<Uuid>> {
    let row = db()
        .find_with_params(
            "SELECT dept_id FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&user_id)],
        )
        .await?;
    match row.first() {
        Some(row) => row.get_typed("dept_id"),
        None => Ok(None),
    }
}

/// 用户是否存在且可用（负责人 / 协作人校验）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_exists(user_id: Uuid) -> DbResult<bool> {
    let row = db()
        .find_with_params(
            "SELECT id, status FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&user_id)],
        )
        .await?;
    match row.first() {
        Some(row) => {
            let status: i16 = row.get_typed("status")?;
            Ok(status == 1)
        }
        None => Ok(false),
    }
}

// ==================== 流程来源（P-FLOW-1A） ====================

/// 子任务清单（按创建顺序；`deleted_at IS NULL`）。
///
/// 通用执行器版本：既能吃 `&Transaction`（写路径重算），也能吃 `db()`（读路径）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub(super) async fn child_rows_with(
    ex: &impl SqlExecutor,
    parent_id: Uuid,
) -> DbResult<Vec<ChildRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, status, progress, estimate_hours::float8 AS estimate_hours \
               FROM biz_task \
              WHERE parent_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY created_at ASC, id ASC"
                .to_string(),
            vec![Value::from(&parent_id)],
        )
        .await?;
    rows.iter().map(ChildRow::from_row).collect()
}

/// 子任务清单（读路径：`GET /{id}/subtasks`、进度锁定判定）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn children_of(parent_id: Uuid) -> DbResult<Vec<ChildRow>> {
    child_rows_with(db(), parent_id).await
}

/// 把子任务行投影成纯逻辑层的快照（聚合 / 锁定判定的入参）。
pub fn snapshots_of(children: &[ChildRow]) -> Vec<ChildSnapshot> {
    children.iter().map(ChildRow::snapshot).collect()
}

/// 任务的三列「人」（状态机判权用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn people_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<state::TaskPeople>> {
    let rows = ex
        .query_sql(
            "SELECT owner_id, creator_id, assigner_id FROM biz_task \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(state::TaskPeople {
        owner_id: row.get_typed("owner_id")?,
        creator_id: row.get_typed("creator_id")?,
        assigner_id: row.get_typed("assigner_id")?,
    }))
}

/// 账号类型（`sys_user.user_type`：`HUMAN` / `ROBOT`）；用户不存在 / 已软删 → `None`。
///
/// # 用途边界（红线）
///
/// 只用于**文案**与「两条硬拒」（AI 用户不能删任务）。
/// 分派 / 协作人的准入判定**不许**看它 —— 修正 11 要求「默认不能、可按角色授予」，
/// 在这里硬拒 `user_type` 会把「授予 AI 团队主管角色后就能分派」这条路彻底焊死。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_type_of(user_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT user_type FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&user_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("user_type"),
        None => Ok(None),
    }
}

/// 用户显示名（`real_name` 优先，回落 `username`）；不存在返回空串。
///
/// 只用于**通知文案 / 评论正文**（「分派：张三 → 李四」比一串 UUID 有用得多），
/// 不参与任何判定。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn user_display_name(user_id: Uuid) -> DbResult<String> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(real_name, ''), username, '') AS name FROM sys_user \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&user_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("name"),
        None => Ok(String::new()),
    }
}

/// 从一批 id 里筛出**有效用户**（未软删且 `status = 1`）的 id。
///
/// 一条查询解决「这批人里谁不能加」：比逐个 `user_exists` 少 N 次往返，也保证
/// 判定口径只有一处（`deleted_at IS NULL AND status = 1`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn valid_users(user_ids: &[Uuid]) -> DbResult<Vec<Uuid>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT u.id FROM sys_user u \
              WHERE u.id = ANY(?::text::uuid[]) AND u.deleted_at IS NULL AND u.status = 1",
            vec![ids_array_param(user_ids)],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed("id")?);
    }
    Ok(ids)
}

/// 任务现有协作人 id（按加入时间升序，与 [`list_members`] 同序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn member_ids(task_id: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT user_id FROM biz_task_member WHERE task_id = ?::uuid \
              ORDER BY created_at ASC, user_id ASC",
            vec![Value::from(&task_id)],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed("user_id")?);
    }
    Ok(ids)
}

/// 轮询目标：已派活、未终态的任务（按 `hermes_synced_at` 升序，最久没同步的先看）。
///
/// # 参数
///
/// - `limit`：单轮最多同步多少个（防止一次轮询把 30 秒的调度窗口撑爆）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn sync_targets(limit: i64) -> DbResult<Vec<SyncTargetRow>> {
    let rows = db()
        .find_with_params(
            "SELECT id, COALESCE(hermes_board, '') AS board, COALESCE(hermes_task_id, '') AS hermes_task_id, \
                    status, hermes_status, owner_id, creator_id, assigner_id, title, task_no \
               FROM biz_task \
              WHERE deleted_at IS NULL \
                AND exec_mode = 'AI' \
                AND hermes_task_id IS NOT NULL \
                AND status NOT IN ('DONE', 'CANCELLED') \
              ORDER BY hermes_synced_at ASC NULLS FIRST, updated_at ASC \
              LIMIT ?",
            vec![Value::I64(limit)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(SyncTargetRow {
            id: row.get_typed("id")?,
            board: row.get_typed("board")?,
            hermes_task_id: row.get_typed("hermes_task_id")?,
            status: row.get_typed("status")?,
            hermes_status: row.get_typed("hermes_status").unwrap_or(None),
            owner_id: row.get_typed("owner_id")?,
            creator_id: row.get_typed("creator_id")?,
            assigner_id: row.get_typed("assigner_id").unwrap_or(None),
            title: row.get_typed("title")?,
            task_no: row.get_typed("task_no")?,
        });
    }
    Ok(list)
}

/// 该任务是否已经有 AI 产出的附件（路径 A 的判据）。
pub async fn count_ai_attachments(task_id: Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_task_attachment \
          WHERE task_id = ?::uuid AND source = 'AI'",
        vec![Value::from(&task_id)],
    )
    .await
}

/// AI 进度回写要用的最小上下文：任务是否还有子任务（有子任务时 `percent` 无效）。
pub async fn has_open_or_any_children(task_id: Uuid) -> DbResult<bool> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_task WHERE parent_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&task_id)],
        )
        .await?;
    Ok(count > 0)
}

// ==================== 单元测试（纯逻辑） ====================

// ==================== 统计（T5b · b4：exec_mode 维度） ====================

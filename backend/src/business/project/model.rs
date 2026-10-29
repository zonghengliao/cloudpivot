//! 项目分组 · 数据访问层（批次 C+ · T7c）。
//!
//! # 三条不变式（本模块的全部读路径都受它们约束）
//!
//! 1. **项目只是逻辑分组，不是权限边界**：`biz_project` 自己的可见性 = 数据范围 ∪
//!    负责人 ∪ 创建人 ∪ 项目成员（[`PROJECT_VISIBLE_TAIL`]）；而**项目下的任务**一律走
//!    [`crate::business::task::model::page_visible`]（T1 的唯一可见性实现），
//!    本模块**没有**任何「按 project_id 直接列任务」的 SQL —— 卡面红线逐字落地。
//! 2. **统计也按 T1 可见性裁剪**（[`stats_for_projects`]）：项目详情 / 列表的「任务数 /
//!    完成率 / 工时」只统计**当前用户看得见**的任务，否则 B 当项目成员就能靠数字推断
//!    「项目里还有 N 个我看不到的任务」—— 这就是借项目扩权。
//! 3. **删项目不删任务**（[`soft_delete_project`] + [`unbind_tasks`] 同事务）：任务行一行不动，
//!    只把 `project_id` 置 NULL；成员行同事务物理删（卡面规则 6）。
//!
//! # 为什么锚点是本地常量而不是登记进 `SCOPED_TABLES`
//!
//! 与 `business/task/model.rs::TASK_TARGET` 同一决策：`SCOPED_TABLES` 是「数据权限页 +
//! 启动自检」的登记清单，任务表与项目表都**不登记**（它们的可见性是 OR 语义：
//! 「范围 ∪ 参与关系」，登记表语义是纯过滤，登记了反而会让自检误判「该注入没注入」）。
//! `ScopeTarget::new` 是 `pub const fn`，本地声明的锚点同样能过 `filter_for`。
//!
//! # 闸②（`scripts/check.sh`）
//!
//! 本文件里 `FROM biz_project` / `FROM biz_project_member` 的每个查询都经
//! `scope::paginate` 或 `scope::filter_for`（片段拼进 SQL），因此门禁命中「已接入」。

use alun::prelude::chrono::NaiveDate;
use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::business::task::model as task_model;

// ==================== 数据权限锚点 ====================

/// 项目表的 scope 锚点：部门列 `dept_id`（归属部门快照），归属列 `owner_id`（负责人）。
///
/// 别名固定为 `p`：`{FILTER}` 片段会以 `p.dept_id` / `p.owner_id` 的形式拼进来，
/// 本模块的 SQL **不得**给 `biz_project` 用别的别名（`shared/scope.rs` §2.6 规则 1）。
pub const PROJECT_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::new("biz_project", "p", Some("dept_id"), Some("owner_id"));

/// 项目可见性谓词：**数据范围 ∪ 负责人 ∪ 创建人 ∪ 项目成员 ∪ 我在该项目下有可见任务**。
///
/// 占位符顺序（文本顺序，必须与 [`visible_params`] 一致）：
/// ① `{FILTER}` 里的 scope 片段参数（由 [`scope::paginate`] 前置）；
/// ② 当前用户 id（`p.owner_id = ?`）；
/// ③ 当前用户 id（`p.create_by = ?`）；
/// ④ 「我参与的项目」id 数组（`?::text::uuid[]`）；
/// ⑤ 「我参与的任务」id 数组（`?::text::uuid[]`，用于「我在这项目里有任务」这一支）。
///
/// # 为什么成员也算「可见」
///
/// 成员看得见**项目本身**（否则「项目成员」这个标签毫无意义），但**看不到项目下的任务**
/// —— 任务可见性仍走 T1。这正是验收 6 要证明的分界：B 是成员 → 能打开项目、能看到
/// 「任务数 0」；A（负责人）与 C（任务负责人）看到的是 1。
///
/// # 为什么第 5 支是必须的
///
/// 任务详情抽屉要显示「所属项目」且**可点击跳到项目**（卡面二·任务侧）。任务的负责人
/// 可能既不是项目负责人也不是项目成员（跨部门协作的常态）。没有第 5 支，点进去就是
/// 404 —— 用户视角的「坏链接」。第 5 支只放开**项目壳**的可见性：项目下任务列表与统计
/// 依旧各自过 T1，所以它不是扩权（C 打开项目只会看到自己那 1 条任务）。
pub const PROJECT_VISIBLE_TAIL: &str = " AND ( (1 = 1 {FILTER}) OR p.owner_id = ?::uuid OR p.create_by = ?::uuid \
     OR p.id = ANY(?::text::uuid[]) \
     OR EXISTS (SELECT 1 FROM biz_task t \
                 WHERE t.project_id = p.id AND t.deleted_at IS NULL \
                   AND t.id = ANY(?::text::uuid[])) )";

/// 把 id 集合编码成 PG 文本数组字面量 `{uuid,uuid}`（空集合 = `{}`）。
///
/// 与 `scope.rs::array_param` 同一套路：本栈驱动不支持 `Value::Array` 绑 `uuid[]`，
/// 「文本字面量 + `?::text::uuid[]`」是唯一稳写法（T1 已验证）。
pub fn ids_array_param(ids: &[Uuid]) -> Value {
    if ids.is_empty() {
        return Value::Text("{}".to_string());
    }
    let joined = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Value::Text(format!("{{{joined}}}"))
}

/// [`PROJECT_VISIBLE_TAIL`] 的参数（顺序 = `?` 的文本顺序，`{FILTER}` 参数由调用方前置）。
///
/// `my_task_ids` 传「我参与的任务」id 集合（[`task_model::my_member_task_ids`]）；
/// 降级（超 `MEMBER_SCOPE_LIMIT`）时调用方传空集合 → 第 5 支恒假，
/// 可见性退回「范围 ∪ 负责人 ∪ 创建人 ∪ 成员」（安全侧降级，不扩大）。
pub fn visible_params(me: Uuid, member_project_ids: &[Uuid], my_task_ids: &[Uuid]) -> Vec<Value> {
    vec![
        Value::from(&me),
        Value::from(&me),
        ids_array_param(member_project_ids),
        ids_array_param(my_task_ids),
    ]
}

// ==================== 行结构 ====================

/// 项目行（列表 / 详情共用）。
#[derive(Debug, Clone)]
pub struct ProjectRow {
    /// 主键。
    pub id: Uuid,
    /// 项目编码（DB 列 `project_no`，API 字段 `code`）。
    pub code: String,
    /// 项目名称。
    pub name: String,
    /// 说明（可空）。
    pub description: Option<String>,
    /// 负责人（可空）。
    pub owner_id: Option<Uuid>,
    /// 负责人姓名（`real_name` 优先，退化为账号，最后空串）。
    pub owner_name: String,
    /// 归属部门（可空）。
    pub dept_id: Option<Uuid>,
    /// 归属部门名（可空 → 空串）。
    pub dept_name: String,
    /// 状态：`ACTIVE` / `ARCHIVED`。
    pub status: String,
    /// 开始日。
    pub start_date: Option<NaiveDate>,
    /// 结束日（API 字段 `due_date`）。
    pub end_date: Option<NaiveDate>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 创建人（可空；写权限判定用）。
    pub create_by: Option<Uuid>,
}

impl ProjectRow {
    /// 从结果行解析（列名见 [`PROJECT_COLUMNS`]）。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            code: row.get_typed("project_no")?,
            name: row.get_typed("project_name")?,
            description: row.get_typed("description")?,
            owner_id: row.get_typed("owner_id")?,
            owner_name: row.get_typed("owner_name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            status: row.get_typed("status")?,
            start_date: row.get_typed("start_date")?,
            end_date: row.get_typed("end_date")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
        })
    }
}

/// 项目 SELECT 列清单（列表与详情共用，避免两套形状漂移）。
const PROJECT_COLUMNS: &str = "\
    p.id, COALESCE(p.project_no, '') AS project_no, p.project_name, p.description, \
    p.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
    p.dept_id, COALESCE(d.dept_name, '') AS dept_name, p.status, \
    p.start_date, p.end_date, p.created_at, p.updated_at, p.create_by";

/// 项目 FROM 子句（负责人姓名 / 部门名只做展示，数据权限片段只锚定 `p`）。
const PROJECT_FROM: &str = " FROM biz_project p \
     LEFT JOIN sys_user u ON u.id = p.owner_id \
     LEFT JOIN sys_dept d ON d.id = p.dept_id ";

// ==================== 过滤条件 ====================

/// 项目分页过滤（由 handler 从 DTO 归一化而来）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFilter {
    /// 名称 / 编码关键词（ILIKE）。
    pub name: Option<String>,
    /// 状态（`ACTIVE` / `ARCHIVED`）。
    pub status: Option<String>,
    /// 负责人。
    pub owner_id: Option<Uuid>,
}

/// 追加业务过滤片段，并把参数按**文本顺序**推进 `params`。
fn build_filters(filter: &ProjectFilter, params: &mut Vec<Value>) -> String {
    let mut sql = String::new();
    if let Some(name) = filter.name.as_deref() {
        let pattern = format!("%{}%", task_model::escape_like(name));
        // 卡面筛选维度只有 `name`，但「按编码找项目」是高频操作（编码就是给人记的），
        // 所以同一个关键词同时匹配名称与编码 —— 与任务页 keyword 的口径一致。
        sql.push_str(" AND (p.project_name ILIKE ? OR COALESCE(p.project_no, '') ILIKE ?)");
        params.push(Value::Text(pattern.clone()));
        params.push(Value::Text(pattern));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND p.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(owner_id) = filter.owner_id {
        sql.push_str(" AND p.owner_id = ?::uuid");
        params.push(Value::from(&owner_id));
    }
    sql
}

// ==================== 读路径 ====================

/// 项目分页结果。
#[derive(Debug, Clone)]
pub struct ProjectPage {
    /// 当前页。
    pub list: Vec<ProjectRow>,
    /// 可见总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub size: u64,
}

/// 「我参与的项目」id 集合（项目成员关系）。
///
/// 上层（项目数）不可能像任务那样到 2000 条量级，**不设上限**；
/// 若将来真的膨胀，再按 T1 的 D7 降级口径处理。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn my_project_ids(me: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = db()
        .find_with_params(
            "SELECT m.project_id FROM biz_project_member m \
              WHERE m.user_id = ?::uuid AND m.project_id IN ( \
                    SELECT p.id FROM biz_project p WHERE p.deleted_at IS NULL )",
            vec![Value::from(&me)],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed::<Uuid>("project_id")?);
    }
    Ok(ids)
}

/// 项目分页（**可见性 = 数据范围 ∪ 负责人 ∪ 创建人 ∪ 成员**）。
///
/// # 参数
///
/// - `principal`：当前用户数据范围（只能由 `principal::resolve` 构造）；
/// - `filter`：名称/编码关键词 + 状态 + 负责人；
/// - `page` / `page_size`：已由 `utils::normalize_page` 归一化。
///
/// # 返回值
///
/// [`ProjectPage`]，排序 = 创建时间倒序（同秒再用主键倒序，保证翻页稳定）。
///
/// # 异常情况
///
/// 数据库异常 / 片段形态异常 → [`DbError`]。
pub async fn page_visible(
    principal: &ScopedPrincipal,
    filter: &ProjectFilter,
    page: u64,
    page_size: u64,
) -> DbResult<ProjectPage> {
    let member_ids = my_project_ids(principal.user_id()).await?;
    let my_tasks = task_model::my_member_task_ids(principal.user_id()).await?;

    let mut sql = String::with_capacity(512);
    sql.push_str("SELECT ");
    sql.push_str(PROJECT_COLUMNS);
    sql.push_str(PROJECT_FROM);
    sql.push_str(" WHERE p.deleted_at IS NULL");
    sql.push_str(PROJECT_VISIBLE_TAIL);

    // 参数顺序：可见性参数（成员数组）在前，业务过滤在后 —— 与 SQL 里 `?` 的文本顺序一致
    // （`{FILTER}` 里 scope 片段的参数由 `paginate` 前置到最前）。
    let mut params = visible_params(principal.user_id(), &member_ids, &my_tasks.ids);
    let filters = build_filters(filter, &mut params);
    sql.push_str(&filters);

    let Page {
        list,
        total,
        page,
        size,
        pages: _,
    } = scope::paginate(
        PROJECT_TARGET,
        &sql,
        params,
        Some("ORDER BY p.created_at DESC, p.id DESC"),
        page,
        page_size,
        principal,
    )
    .await?;

    let rows = list
        .iter()
        .map(ProjectRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(ProjectPage {
        list: rows,
        total,
        page,
        size,
    })
}

/// 按 id 取项目（**带可见性**：看不见 / 已软删 → `None`，上层统一 404）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_visible(principal: &ScopedPrincipal, id: Uuid) -> DbResult<Option<ProjectRow>> {
    let member_ids = my_project_ids(principal.user_id()).await?;
    let my_tasks = task_model::my_member_task_ids(principal.user_id()).await?;
    let fragment = scope::filter_for(principal, PROJECT_TARGET);
    let tail = PROJECT_VISIBLE_TAIL.replacen(scope::FILTER_MARKER, fragment.sql().trim(), 1);

    let mut sql = String::with_capacity(512);
    sql.push_str("SELECT ");
    sql.push_str(PROJECT_COLUMNS);
    sql.push_str(PROJECT_FROM);
    sql.push_str(" WHERE p.id = ?::uuid AND p.deleted_at IS NULL");
    sql.push_str(&tail);

    // 文本顺序：p.id 在前，然后 `{FILTER}` 片段的参数，最后是可见性参数。
    let mut params = Vec::with_capacity(1 + fragment.params().len() + 4);
    params.push(Value::from(&id));
    params.extend(fragment.params().iter().cloned());
    params.extend(visible_params(
        principal.user_id(),
        &member_ids,
        &my_tasks.ids,
    ));

    let rows = db().find_with_params(&sql, params).await?;
    rows.first().map(ProjectRow::from_row).transpose()
}

/// 按 id 取项目（**不带可见性**，只给写路径用：改 / 删前先确认存在）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_raw(id: Uuid) -> DbResult<Option<ProjectRow>> {
    let sql = format!(
        "SELECT {PROJECT_COLUMNS}{PROJECT_FROM} WHERE p.id = ?::uuid AND p.deleted_at IS NULL"
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(ProjectRow::from_row).transpose()
}

// ==================== 统计（按 T1 可见性裁剪） ====================

/// 单个项目的任务统计（**只统计当前用户可见的任务**）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectStats {
    /// 项目 id。
    pub project_id: Uuid,
    /// 任务总数（可见）。
    pub total: i64,
    /// 待开始。
    pub todo: i64,
    /// 进行中。
    pub doing: i64,
    /// 待验收。
    pub review: i64,
    /// 已完成。
    pub done: i64,
    /// 已取消。
    pub cancelled: i64,
    /// 总预估工时（小时）。
    pub estimate_hours: f64,
    /// 总实际工时（小时）。
    pub actual_hours: f64,
}

impl ProjectStats {
    /// 完成率（0–100，保留 1 位小数）。
    ///
    /// 口径与 T5b 的统计接口一致：**分母剔掉 `CANCELLED`**（取消是分派人的动作，
    /// 不该拉低团队完成率）；分母为 0 时完成率记 0（不是 100 —— 「没有任务」不等于「做完了」）。
    pub fn completion_rate(&self) -> f64 {
        let denominator = self.total - self.cancelled;
        if denominator <= 0 {
            return 0.0;
        }
        let rate = self.done as f64 * 100.0 / denominator as f64;
        (rate * 10.0).round() / 10.0
    }
}

/// 批量统计多个项目的任务数 / 各状态 / 工时。
///
/// # 为什么和任务可见性共用一条谓词
///
/// 项目下任务的「数量」也是信息：若统计不过 visibility，项目成员 B 会看到
/// 「任务数 1、完成率 0%」而列表里空空如也 —— 等于告诉他「有个你看不到的任务」。
/// 所以这里注入的片段与 [`crate::business::task::model::VISIBLE_OR_TAIL`] 同源
/// （`scope::filter_for(principal, TASK_TARGET)` + 「我参与」id 数组），只查一次、
/// 按 project_id 聚合。
///
/// # 参数
///
/// - `principal`：当前用户；
/// - `project_ids`：要统计的项目（空集合直接返回空 Vec，不发 SQL）。
///
/// # 返回值
///
/// 每个**有可见任务**的项目一条记录；没有可见任务的项目不出现在结果里（上层补零）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn stats_for_projects(
    principal: &ScopedPrincipal,
    project_ids: &[Uuid],
) -> DbResult<Vec<ProjectStats>> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let me = principal.user_id();
    let my_tasks = task_model::my_member_task_ids(me).await?;
    let fragment = scope::filter_for(principal, task_model::TASK_TARGET);

    // 占位符文本顺序：① 项目 id 数组（WHERE）② scope 片段（WHERE 内后续）③ 我参与的 id 数组
    let mut sql = String::with_capacity(640);
    sql.push_str(
        "SELECT t.project_id AS project_id, \
                count(*) AS total, \
                count(*) FILTER (WHERE t.status = 'TODO') AS todo, \
                count(*) FILTER (WHERE t.status = 'DOING') AS doing, \
                count(*) FILTER (WHERE t.status = 'REVIEW') AS review, \
                count(*) FILTER (WHERE t.status = 'DONE') AS done, \
                count(*) FILTER (WHERE t.status = 'CANCELLED') AS cancelled, \
                COALESCE(sum(t.estimate_hours), 0)::float8 AS estimate_hours, \
                COALESCE(sum(t.actual_hours), 0)::float8 AS actual_hours \
           FROM biz_task t \
          WHERE t.project_id = ANY(?::text::uuid[]) \
            AND t.deleted_at IS NULL \
            AND ( (1 = 1 ",
    );
    sql.push_str(fragment.sql().trim());
    sql.push_str(") OR t.id = ANY(?::text::uuid[]) ) GROUP BY t.project_id");

    let mut params = Vec::with_capacity(2 + fragment.params().len());
    params.push(ids_array_param(project_ids));
    params.extend(fragment.params().iter().cloned());
    params.push(my_tasks.param());

    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ProjectStats {
            project_id: row.get_typed("project_id")?,
            total: row.get_typed("total")?,
            todo: row.get_typed("todo")?,
            doing: row.get_typed("doing")?,
            review: row.get_typed("review")?,
            done: row.get_typed("done")?,
            cancelled: row.get_typed("cancelled")?,
            estimate_hours: row.get_typed("estimate_hours")?,
            actual_hours: row.get_typed("actual_hours")?,
        });
    }
    Ok(out)
}

/// 项目成员数（仅用于列表上的「成员」列；不做可见性裁剪 —— 成员名单本身不是敏感数据）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn member_counts(project_ids: &[Uuid]) -> DbResult<Vec<(Uuid, i64)>> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT m.project_id AS project_id, count(*) AS total \
               FROM biz_project_member m \
              WHERE m.project_id = ANY(?::text::uuid[]) \
              GROUP BY m.project_id",
            vec![ids_array_param(project_ids)],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("project_id")?;
        let total: i64 = row.get_typed("total")?;
        out.push((id, total));
    }
    Ok(out)
}

// ==================== 写路径 ====================

/// 新建项目入参。
#[derive(Debug, Clone)]
pub struct NewProject {
    /// 主键（应用侧 `Uuid::now_v7()`）。
    pub id: Uuid,
    /// 编码（API `code` → DB `project_no`）。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 说明。
    pub description: Option<String>,
    /// 负责人。
    pub owner_id: Option<Uuid>,
    /// 归属部门（取负责人当时的部门，作为快照 —— 与 `biz_task.dept_id` 同口径）。
    pub dept_id: Option<Uuid>,
    /// 开始日。
    pub start_date: Option<NaiveDate>,
    /// 结束日。
    pub end_date: Option<NaiveDate>,
}

/// 新建项目（事务内）。
///
/// # 异常情况
///
/// 数据库异常（含 `uk_biz_project_no` 冲突）→ [`DbError`]。
pub async fn create_project(ex: &Transaction, new: &NewProject, operator: Uuid) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_project \
             (id, project_no, project_name, description, owner_id, dept_id, status, \
              start_date, end_date, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, ?::uuid, ?::uuid, 'ACTIVE', \
                 ?::text::date, ?::text::date, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&new.id),
            Value::Text(new.code.clone()),
            Value::Text(new.name.clone()),
            Value::from(new.description.clone()),
            Value::from(new.owner_id.as_ref()),
            Value::from(new.dept_id.as_ref()),
            date_param(new.start_date),
            date_param(new.end_date),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(())
}

/// 项目可改字段（`None` = 不改）。
#[derive(Debug, Clone, Default)]
pub struct ProjectPatch {
    /// 编码。
    pub code: Option<String>,
    /// 名称。
    pub name: Option<String>,
    /// 说明（`Some(None)` = 清空）。
    pub description: Option<Option<String>>,
    /// 负责人 + 其部门快照（`Some(None)` = 清空负责人）。
    pub owner: Option<Option<(Uuid, Option<Uuid>)>>,
    /// 状态。
    pub status: Option<String>,
    /// 开始日（`Some(None)` = 清空）。
    pub start_date: Option<Option<NaiveDate>>,
    /// 结束日（`Some(None)` = 清空）。
    pub end_date: Option<Option<NaiveDate>>,
}

impl ProjectPatch {
    /// 是否一个可改字段都没传。
    pub fn is_empty(&self) -> bool {
        self.code.is_none()
            && self.name.is_none()
            && self.description.is_none()
            && self.owner.is_none()
            && self.status.is_none()
            && self.start_date.is_none()
            && self.end_date.is_none()
    }
}

/// 编辑项目（事务内）。
///
/// # 返回值
///
/// 受影响行数（0 = 项目已被删除）。
///
/// # 异常情况
///
/// 数据库异常（含唯一索引冲突）→ [`DbError`]。
pub async fn update_project(
    ex: &Transaction,
    id: Uuid,
    patch: &ProjectPatch,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(code) = patch.code.as_ref() {
        sets.push("project_no = ?");
        params.push(Value::Text(code.clone()));
    }
    if let Some(name) = patch.name.as_ref() {
        sets.push("project_name = ?");
        params.push(Value::Text(name.clone()));
    }
    if let Some(description) = patch.description.as_ref() {
        sets.push("description = ?");
        params.push(Value::from(description.clone()));
    }
    if let Some(owner) = patch.owner.as_ref() {
        sets.push("owner_id = ?::uuid");
        params.push(Value::from(owner.as_ref().map(|(id, _)| *id).as_ref()));
        sets.push("dept_id = ?::uuid");
        params.push(Value::from(
            owner.as_ref().and_then(|(_, dept)| *dept).as_ref(),
        ));
    }
    if let Some(status) = patch.status.as_ref() {
        sets.push("status = ?");
        params.push(Value::Text(status.clone()));
    }
    if let Some(start_date) = patch.start_date.as_ref() {
        sets.push("start_date = ?::text::date");
        params.push(date_param(*start_date));
    }
    if let Some(end_date) = patch.end_date.as_ref() {
        sets.push("end_date = ?::text::date");
        params.push(date_param(*end_date));
    }
    sets.push("updated_at = now()");
    sets.push("update_by = ?::uuid");
    params.push(Value::from(&operator));

    let sql = format!(
        "UPDATE biz_project SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    params.push(Value::from(&id));
    ex.execute_with_params(&sql, params).await
}

/// 软删项目。
///
/// # 返回值
///
/// 受影响行数（0 = 已被删过）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn soft_delete_project(ex: &Transaction, id: Uuid, operator: Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_project SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

/// 把项目下的任务**解绑**（`project_id = NULL`，任务本身不删）。
///
/// # 返回值
///
/// 被解绑的任务数（验收要贴这个数字）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn unbind_tasks(ex: &Transaction, project_id: Uuid, operator: Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_task SET project_id = NULL, updated_at = now(), update_by = ?::uuid \
          WHERE project_id = ?::uuid",
        vec![Value::from(&operator), Value::from(&project_id)],
    )
    .await
}

/// 物理删除项目的成员行（卡面规则 6：成员记录级联删，不影响任务）。
///
/// 项目是软删的，`ON DELETE CASCADE` 不会触发，所以必须显式删；
/// 成员表没有软删列 —— 成员关系是「当前状态」而不是历史（加入/移出的留痕在变更日志维度）。
///
/// # 返回值
///
/// 删掉的成员行数。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn delete_members(ex: &Transaction, project_id: Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "DELETE FROM biz_project_member WHERE project_id = ?::uuid",
        vec![Value::from(&project_id)],
    )
    .await
}

/// 编码是否已被占用（软删的项目不占号）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn code_taken(code: &str, exclude: Option<Uuid>) -> DbResult<bool> {
    let rows = db()
        .find_with_params(
            "SELECT 1 AS hit FROM biz_project p \
              WHERE p.deleted_at IS NULL AND p.project_no = ? \
                AND (?::uuid IS NULL OR p.id <> ?::uuid) LIMIT 1",
            vec![
                Value::Text(code.to_string()),
                Value::from(exclude.as_ref()),
                Value::from(exclude.as_ref()),
            ],
        )
        .await?;
    Ok(!rows.is_empty())
}

// ==================== 成员 ====================

/// 项目成员行（含用户展示字段）。
#[derive(Debug, Clone)]
pub struct MemberRow {
    /// 成员用户 id。
    pub user_id: Uuid,
    /// 账号。
    pub username: String,
    /// 姓名（可空 → 空串）。
    pub real_name: String,
    /// 角色：`MEMBER` / `VIEWER`。
    pub role: String,
    /// 加入时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl MemberRow {
    /// 从结果行解析。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            user_id: row.get_typed("user_id")?,
            username: row.get_typed("username")?,
            real_name: row.get_typed("real_name")?,
            role: row.get_typed("role")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 项目成员列表。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_members(project_id: Uuid) -> DbResult<Vec<MemberRow>> {
    let rows = db()
        .find_with_params(
            "SELECT m.user_id, u.username, COALESCE(u.real_name, '') AS real_name, \
                    m.role, m.created_at \
               FROM biz_project_member m \
               JOIN sys_user u ON u.id = m.user_id \
              WHERE m.project_id = ?::uuid \
              ORDER BY m.created_at ASC, m.user_id ASC",
            vec![Value::from(&project_id)],
        )
        .await?;
    rows.iter().map(MemberRow::from_row).collect()
}

/// 加成员（幂等：已存在则不改 role）。
///
/// # 返回值
///
/// 受影响行数（0 = 已经是成员）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn add_member(
    ex: &Transaction,
    project_id: Uuid,
    user_id: Uuid,
    role: &str,
    operator: Uuid,
) -> DbResult<u64> {
    ex.execute_with_params(
        "INSERT INTO biz_project_member (id, project_id, user_id, role, created_at, create_by) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, now(), ?::uuid) \
         ON CONFLICT (project_id, user_id) DO NOTHING",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&project_id),
            Value::from(&user_id),
            Value::Text(role.to_string()),
            Value::from(&operator),
        ],
    )
    .await
}

/// 移除成员。
///
/// # 返回值
///
/// 受影响行数（0 = 本来就不是成员）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn remove_member(ex: &Transaction, project_id: Uuid, user_id: Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "DELETE FROM biz_project_member WHERE project_id = ?::uuid AND user_id = ?::uuid",
        vec![Value::from(&project_id), Value::from(&user_id)],
    )
    .await
}

/// 是否是项目成员。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn is_member(project_id: Uuid, user_id: Uuid) -> DbResult<bool> {
    let rows = db()
        .find_with_params(
            "SELECT 1 AS hit FROM biz_project_member \
              WHERE project_id = ?::uuid AND user_id = ?::uuid LIMIT 1",
            vec![Value::from(&project_id), Value::from(&user_id)],
        )
        .await?;
    Ok(!rows.is_empty())
}

// ==================== 任务归入 / 移出 ====================

/// 把任务归入 / 移出项目（`project_id = None` = 移出）。
///
/// 只改 `biz_task.project_id`（+ 审计列），**不动**状态、进度、编号 ——
/// 归入项目是「分组」而不是「状态变更」，不该触发状态机与通知。
///
/// # 返回值
///
/// 受影响行数（0 = 任务已被删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn set_task_project(
    ex: &Transaction,
    task_id: Uuid,
    project_id: Option<Uuid>,
    operator: Uuid,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_task SET project_id = ?::uuid, updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::from(project_id.as_ref()),
            Value::from(&operator),
            Value::from(&task_id),
        ],
    )
    .await
}

/// 日期参数：`?::text::date` 形态（T6b 实测：直接 `?::date` 绑文本会 500）。
fn date_param(value: Option<NaiveDate>) -> Value {
    match value {
        Some(date) => Value::Text(date.to_string()),
        None => Value::Null,
    }
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(total: i64, done: i64, cancelled: i64) -> ProjectStats {
        ProjectStats {
            project_id: Uuid::now_v7(),
            total,
            done,
            cancelled,
            ..ProjectStats::default()
        }
    }

    /// 验收 5 的口径：5 个任务、2 个完成 → 40%。
    #[test]
    fn completion_rate_matches_acceptance_numbers() {
        let s = stats(5, 2, 0);
        assert_eq!(s.completion_rate(), 40.0);
    }

    /// 取消不算分母（与 T5b 统计接口同口径）。
    #[test]
    fn completion_rate_excludes_cancelled_from_denominator() {
        // 5 个任务里 1 个取消、2 个完成 → 2 / 4 = 50%
        let s = stats(5, 2, 1);
        assert_eq!(s.completion_rate(), 50.0);
    }

    /// 没有任务时完成率是 0，不是 100。
    #[test]
    fn completion_rate_is_zero_without_tasks() {
        assert_eq!(stats(0, 0, 0).completion_rate(), 0.0);
        // 全部取消：分母 0 → 仍是 0（不能因为「都取消了」显示 100%）
        assert_eq!(stats(3, 0, 3).completion_rate(), 0.0);
    }

    /// 完成率保留 1 位小数（1/3 → 33.3，不是 33.333333）。
    #[test]
    fn completion_rate_rounds_to_one_decimal() {
        assert_eq!(stats(3, 1, 0).completion_rate(), 33.3);
        assert_eq!(stats(7, 2, 0).completion_rate(), 28.6);
    }

    /// 可见性谓词的占位符个数必须与参数个数一致（S8 不变量的本地版本）。
    #[test]
    fn visible_tail_placeholders_match_params() {
        let id = Uuid::now_v7();
        let params = visible_params(id, &[Uuid::now_v7(), Uuid::now_v7()], &[Uuid::now_v7()]);
        // 谓词里的 `?`：owner / create_by / 成员数组 / 任务数组 = 4 个；
        // `{FILTER}` 的参数由 paginate 前置。
        assert_eq!(PROJECT_VISIBLE_TAIL.matches('?').count(), 4);
        assert_eq!(params.len(), 4);
    }

    /// 空集合编码成 `{}`（PG 合法空数组），非空编码成 `{uuid,uuid}`。
    #[test]
    fn ids_array_param_encodes_pg_literal() {
        match ids_array_param(&[]) {
            Value::Text(text) => assert_eq!(text, "{}"),
            other => panic!("空集合应为 Text(\"{{}}\")，实际：{other:?}"),
        }
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        match ids_array_param(&[a, b]) {
            Value::Text(text) => {
                assert_eq!(text, format!("{{{a},{b}}}"));
                assert_eq!(text.matches(',').count(), 1);
            }
            other => panic!("非空集合应为 Text，实际：{other:?}"),
        }
    }

    /// 业务过滤片段：`?` 个数与追加的参数个数一致，且文本顺序 = 参数顺序。
    #[test]
    fn build_filters_appends_params_in_text_order() {
        let filter = ProjectFilter {
            name: Some("支付".to_string()),
            status: Some("ACTIVE".to_string()),
            owner_id: Some(Uuid::now_v7()),
        };
        let mut params = Vec::new();
        let sql = build_filters(&filter, &mut params);
        assert_eq!(
            sql.matches('?').count(),
            params.len(),
            "占位符与参数个数必须一致"
        );
        // 名称过滤是「名称 OR 编码」两个占位符，因此总数是 2 + 1(状态) + 1(负责人)
        assert_eq!(params.len(), 4);
        assert!(sql.find("project_name ILIKE").unwrap() < sql.find("p.status").unwrap());
        assert!(sql.find("p.status").unwrap() < sql.find("p.owner_id").unwrap());
    }

    /// 空过滤不产生任何片段与参数（避免 `WHERE … AND` 悬空）。
    #[test]
    fn build_filters_is_empty_without_conditions() {
        let mut params = Vec::new();
        assert_eq!(build_filters(&ProjectFilter::default(), &mut params), "");
        assert!(params.is_empty());
    }

    /// 通配符转义：用户搜 `%` 不该变成「匹配一切」。
    #[test]
    fn build_filters_escapes_like_wildcards() {
        let filter = ProjectFilter {
            name: Some("100%_done".to_string()),
            ..ProjectFilter::default()
        };
        let mut params = Vec::new();
        build_filters(&filter, &mut params);
        match &params[0] {
            Value::Text(text) => {
                assert!(text.contains("\\%"), "百分号必须被转义：{text}");
                assert!(text.contains("\\_"), "下划线必须被转义：{text}");
            }
            other => panic!("应为 Text，实际：{other:?}"),
        }
    }

    /// 日期参数走 `?::text::date` 形态：有值 = 文本日期，无值 = NULL。
    #[test]
    fn date_param_uses_text_form() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 29).expect("日期合法");
        match date_param(Some(date)) {
            Value::Text(text) => assert_eq!(text, "2026-10-29"),
            other => panic!("应为 Text，实际：{other:?}"),
        }
        assert!(matches!(date_param(None), Value::Null));
    }

    /// `ProjectPatch::is_empty`：全缺席才算空（空 PUT 要回 400，而不是假成功）。
    #[test]
    fn patch_is_empty_only_when_no_field_present() {
        assert!(ProjectPatch::default().is_empty());
        let patch = ProjectPatch {
            status: Some("ARCHIVED".to_string()),
            ..ProjectPatch::default()
        };
        assert!(!patch.is_empty());
    }
}

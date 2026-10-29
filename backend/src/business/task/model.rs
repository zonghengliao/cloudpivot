//! 工作任务管理 · 数据访问与领域行为。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md`
//! §七（可见性）、§十（创建默认值）、§十二（数据模型 7 张表 / 索引）、§十三 T1 行。
//!
//! # 本卡唯一的「非标准」之处：可见性是 **scope ∪ 参与关系（OR）**
//!
//! 批次 B 的 [`scope::paginate`] 注入的数据权限片段是**AND 关系**（`… AND dept_id IN (…)`），
//! 而任务管理的可见性设计文档写的是**并集**：
//!
//! ```text
//! 可见 = 部门数据范围 ∪ 我是负责人 ∪ 我是创建人 ∪ 我是分派人 ∪ 我是协作人/关注者
//! ```
//!
//! 这是跨部门协作能成立的唯一原因：把市场部的人拉进研发部的任务后，他必须能看见
//! **那一条**，而不是「因为他不在研发部所以看不见」。
//!
//! ## 实现方式（不改 `shared/scope.rs`，也不复制它的逻辑）
//!
//! `scope.rs` 是批次 B 的门禁核心（闸①/闸② 都盯着它），本卡红线不许动。所以这里
//! 用一个**不改变片段语义**的拼法把 AND 片段包进 OR 组：
//!
//! ```sql
//! WHERE t.deleted_at IS NULL
//!   AND ( (1 = 1 {FILTER})                    -- ← paginate 把 ` AND …` 片段原位注入这里
//!         OR t.owner_id = ?::uuid
//!         OR t.creator_id = ?::uuid
//!         OR t.assigner_id = ?::uuid
//!         OR EXISTS (SELECT 1 FROM biz_task_member m
//!                     WHERE m.task_id = t.id AND m.user_id = ?::uuid) )
//! ```
//!
//! `{FILTER}` 会被替换成 ` AND <scope 条件>`，于是左半边等价于 `(1 = 1 AND <scope 条件>)`
//! = `<scope 条件>`；整体就是 `scope 条件 OR 参与关系`。片段本身、参数顺序、count 剥离
//! ORDER BY 的能力全部仍由 [`scope::paginate`] 提供 —— 本模块只负责「放在哪儿」。
//!
//! **这条拼法依赖一个前提**：`RowFilter::sql()` 生成的片段一定以 ` AND ` 开头。
//! 该前提由 [`assert_scope_fragment`] 在每次查询前运行时校验：将来若 `scope.rs` 改了
//! 片段形态（比如去掉前导 AND、改成 `OR`），这里会立刻 `500 + 63008` 报警，
//! 而不是静默变成「全表可见」——「静默越权」是这类拼接最危险的失败形态。
//!
//! # 为什么 `biz_task` 不登记进 `SCOPED_TABLES`
//!
//! 登记表清单（`scope.rs` 的 `SCOPED_TABLES`）服务的是「纯 AND 注入」的存量表，且
//! 启动自检会按 `information_schema` 校验列名。任务的可见性不是纯 AND，登记进去只会
//! 让 `filter_for` 的语义和实际用法不一致；本模块用**本地声明的** [`TASK_TARGET`]
//! 锚点（`ScopeTarget::new` 是 `pub const`，不需要改 `scope.rs`），
//! 因此启动自检也不会把 `biz_task` 当成「该注入却没注入」的表。

use alun::prelude::chrono::{Datelike, Duration, NaiveDate, Utc};
use alun::prelude::*;
use alun::uuid::Uuid;

use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;

// ==================== 数据权限锚点 ====================

/// 任务表的 scope 锚点：部门列 `dept_id`（归属部门快照），归属列 `owner_id`（负责人）。
///
/// 别名固定为 `t`：`{FILTER}` 片段会以 `t.dept_id` / `t.owner_id` 的形式拼进来，
/// SQL 里**不得**给 `biz_task` 用别的别名（`shared/scope.rs` 的 §2.6 规则 1：
/// 片段只锚定主体表）。
pub const TASK_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::new("biz_task", "t", Some("dept_id"), Some("owner_id"));

/// 可见性谓词（OR 合并）。`{FILTER}` 是 `scope::FILTER_MARKER` 的字面量形态。
///
/// 占位符顺序（**文本顺序**，必须与参数顺序一致）：
/// ① `{FILTER}` 内的 scope 片段参数（由 `paginate` 前置）
/// ② owner_id ③ creator_id ④ assigner_id ⑤ 协作人 user_id —— 见 [`visible_params`]。
pub const VISIBLE_OR_TAIL: &str = " AND ( (1 = 1 {FILTER}) \
      OR t.owner_id = ?::uuid \
      OR t.creator_id = ?::uuid \
      OR t.assigner_id = ?::uuid \
      OR EXISTS (SELECT 1 FROM biz_task_member m \
                  WHERE m.task_id = t.id AND m.user_id = ?::uuid) )";

/// 可见性谓词的 4 个「我」参数（顺序：owner / creator / assigner / member）。
pub fn visible_params(me: Uuid) -> Vec<Value> {
    vec![
        Value::from(&me),
        Value::from(&me),
        Value::from(&me),
        Value::from(&me),
    ]
}

/// 校验 `scope.rs` 生成的片段仍然是可以被 `(1 = 1 …)` 包住注入的形态。
///
/// # 为什么必须有这个运行时断言
///
/// 上面的拼法把「片段是 ` AND <cond>`」当成前提。若将来 `scope.rs` 改成生成
/// `OR <cond>` 或裸条件，SQL 会**依然语法合法**：
/// - `(1 = 1 OR <cond>)` → 恒真 → 全表可见（静默越权，最坏的结果）；
/// - `(1 = 1 <裸条件>)` → 语法错（这个反而安全，直接 500）。
///
/// 所以宁可在每次查询前用一次字符串判断把它变成显式报错。
///
/// # 异常情况
///
/// 片段形态不符合预期 → `DbError::InvalidValue`（handler 侧 → `500 + TASK_SCOPE_UNEXPECTED`）。
pub fn assert_scope_fragment(principal: &ScopedPrincipal) -> DbResult<()> {
    let fragment = scope::filter_for(principal, TASK_TARGET);
    check_fragment_shape(fragment.sql())
}

/// [`assert_scope_fragment`] 的纯函数内核（便于单测，不需要构造 `ScopedPrincipal`）。
///
/// 接受三种形态：
/// - `AND <cond>`（部门 / 本人 / 降级形态）；
/// - `1 = 1`（`allow_all`：超管 / ALL 范围 / 未登记表）；
/// - `1 = 0`（`deny_all`：无部门又无归属列）。
pub fn check_fragment_shape(sql: &str) -> DbResult<()> {
    let trimmed = sql.trim();
    let accepted = trimmed.eq_ignore_ascii_case("1 = 1")
        || trimmed.eq_ignore_ascii_case("1 = 0")
        || trimmed
            .get(..3)
            .is_some_and(|head| head.eq_ignore_ascii_case("AND"));
    if accepted {
        return Ok(());
    }
    Err(DbError::InvalidValue(format!(
        "数据权限片段形态不符合任务可见性拼法的前提（必须为 ` AND …` / `1 = 1` / `1 = 0`），\
         实际为：{trimmed}。见 business/task/model.rs 的 VISIBLE_OR_TAIL 说明"
    )))
}

// ==================== 时间口径（CST） ====================

/// UTC 时间 → CST（UTC+8）下的日期。
///
/// 全系统的「今天」都用这一口径：老大在北京，「逾期」必须按北京时间算，
/// 用 UTC 会让每天的 00:00–08:00 出现「昨天到期今天还算没逾期」的错乱。
pub fn cst_date_of(instant: chrono::DateTime<Utc>) -> NaiveDate {
    (instant + Duration::hours(8)).date_naive()
}

/// 现在（UTC）对应的 CST 日期。
pub fn today_cst() -> NaiveDate {
    cst_date_of(Utc::now())
}

/// 创建任务时的默认截止日 = **本周五**（设计文档 §十）。
///
/// 规则：周一至周五 → 本周五（周五当天就是今天）；周六 / 周日 → 下一个周五。
/// 理由：周末建的任务不该默认「昨天到期」，否则一创建就是逾期。
pub fn default_due_date(today: NaiveDate) -> NaiveDate {
    let days_from_monday = today.weekday().num_days_from_monday() as i64;
    let to_friday = (4 - days_from_monday).rem_euclid(7);
    today + Duration::days(to_friday)
}

/// 「我的任务」分组桶（顺序 = 接口出参顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyBucket {
    /// 逾期：`due_date < 今天`。
    Overdue,
    /// 今日：`due_date = 今天`。
    Today,
    /// 本周：`今天 < due_date <= 本周日`。
    ThisWeek,
    /// 以后：`due_date > 本周日`。
    Later,
    /// 无期限：`due_date IS NULL`。
    NoDue,
}

/// 把一条任务分到「我的任务」的某个桶。
///
/// 「本周」= 周一 00:00 至**本周日** 23:59（ISO 周，与前端日历一致）。
pub fn bucket_of(due_date: Option<NaiveDate>, today: NaiveDate) -> MyBucket {
    let Some(due) = due_date else {
        return MyBucket::NoDue;
    };
    if due < today {
        return MyBucket::Overdue;
    }
    if due == today {
        return MyBucket::Today;
    }
    let days_from_monday = today.weekday().num_days_from_monday() as i64;
    let week_end = today + Duration::days(6 - days_from_monday);
    if due <= week_end {
        MyBucket::ThisWeek
    } else {
        MyBucket::Later
    }
}

/// 月内编号的任务号：`TSK-{YYYYMM}-{NNNNNN}`。
pub fn format_task_no(month: &str, seq: i64) -> String {
    format!("TSK-{month}-{seq:06}")
}

/// 当前 CST 月份键（`YYYYMM`）。
pub fn month_key(today: NaiveDate) -> String {
    today.format("%Y%m").to_string()
}

// ==================== 行结构 ====================

/// 任务卡行（列表 / 子任务 / 我的任务共用）。
#[derive(Debug, Clone)]
pub struct TaskCardRow {
    /// 主键。
    pub id: Uuid,
    /// 任务编号。
    pub task_no: String,
    /// 标题。
    pub title: String,
    /// 状态。
    pub status: String,
    /// 优先级。
    pub priority: String,
    /// 进度。
    pub progress: i16,
    /// 是否需验收。
    pub need_review: bool,
    /// 负责人。
    pub owner_id: Uuid,
    /// 负责人姓名。
    pub owner_name: String,
    /// 部门。
    pub dept_id: Option<Uuid>,
    /// 来源。
    pub source: String,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日。
    pub due_date: Option<NaiveDate>,
    /// 子任务数。
    pub child_count: i64,
    /// 创建时间。
    pub created_at: chrono::DateTime<Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<Utc>,
}

impl TaskCardRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            task_no: row.get_typed("task_no")?,
            title: row.get_typed("title")?,
            status: row.get_typed("status")?,
            priority: row.get_typed("priority")?,
            progress: row.get_typed("progress")?,
            need_review: row.get_typed("need_review")?,
            owner_id: row.get_typed("owner_id")?,
            owner_name: row.get_typed("owner_name")?,
            dept_id: row.get_typed("dept_id")?,
            source: row.get_typed("source")?,
            start_date: row.get_typed("start_date")?,
            due_date: row.get_typed("due_date")?,
            child_count: row.get_typed("child_count")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 任务详情行（详情接口 + 更新/删除前的权限判断都需要它）。
#[derive(Debug, Clone)]
pub struct TaskDetailRow {
    /// 任务卡字段。
    pub card: TaskCardRow,
    /// 描述。
    pub description: Option<String>,
    /// 创建人。
    pub creator_id: Uuid,
    /// 创建人姓名。
    pub creator_name: String,
    /// 分派人。
    pub assigner_id: Option<Uuid>,
    /// 分派人姓名。
    pub assigner_name: String,
    /// 部门名称。
    pub dept_name: String,
    /// 父任务。
    pub parent_id: Option<Uuid>,
    /// 父任务编号。
    pub parent_task_no: Option<String>,
    /// 项目。
    pub project_id: Option<Uuid>,
    /// 完成时间。
    pub done_at: Option<chrono::DateTime<Utc>>,
    /// 预估工时。
    pub estimate_hours: Option<f64>,
    /// 实际工时。
    pub actual_hours: Option<f64>,
    /// 取消原因。
    pub cancel_reason: Option<String>,
    /// 打回原因。
    pub reject_reason: Option<String>,
    /// 审批实例（预留）。
    pub approval_instance_id: Option<Uuid>,
}

/// 协作人行。
#[derive(Debug, Clone)]
pub struct TaskMemberRow {
    /// 用户。
    pub user_id: Uuid,
    /// 姓名。
    pub user_name: String,
    /// 角色。
    pub role: String,
    /// 加入时间。
    pub created_at: chrono::DateTime<Utc>,
}

/// 附件行。
#[derive(Debug, Clone)]
pub struct TaskAttachmentRow {
    /// 绑定行。
    pub id: Uuid,
    /// 文件。
    pub file_id: Uuid,
    /// 文件名。
    pub file_name: String,
    /// 文件大小。
    pub file_size: i64,
    /// 排序号。
    pub sort_order: i32,
}

// ==================== SQL 片段 ====================

/// 任务卡列清单（所有列表查询共用，避免两处 select 列不一致导致的 `get_typed` 失败）。
const CARD_COLUMNS: &str = "\
    t.id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review, \
    t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
    t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at, \
    (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count";

/// 任务卡 FROM 子句（负责人姓名来自 `sys_user`：SELECT 里没有部门类过滤，
/// 数据权限片段只锚定 `t`，见 §2.6 规则 1）。
const CARD_FROM: &str = " FROM biz_task t LEFT JOIN sys_user u ON u.id = t.owner_id ";

/// 分页业务过滤条件构造：返回 SQL 片段，并把参数按**文本顺序**追加进 `params`。
///
/// 调用顺序固定为：先 `visible_params`（4 个「我」），再本函数追加的筛选参数 ——
/// 与 SQL 里 `?` 出现的先后完全一致（`{FILTER}` 片段参数由 `paginate` 前置到最前）。
fn build_filters(filter: &TaskFilter, params: &mut Vec<Value>) -> String {
    let mut sql = String::new();
    if let Some(status) = filter.status.as_deref() {
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
    sql
}

/// 转义 `ILIKE` 通配符（用户搜 `%` / `_` 时不该变成「匹配一切」）。
pub fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 分页过滤条件（由 handler 从 DTO 归一化而来）。
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    /// 状态。
    pub status: Option<String>,
    /// 优先级。
    pub priority: Option<String>,
    /// 负责人。
    pub owner_id: Option<Uuid>,
    /// 截止日下界（含）。
    pub due_from: Option<NaiveDate>,
    /// 截止日上界（含）。
    pub due_to: Option<NaiveDate>,
    /// 关键词。
    pub keyword: Option<String>,
}

// ==================== 读路径 ====================

/// 任务分页（**可见性 = scope ∪ 参与关系**）。
///
/// # 参数
///
/// - `principal`：当前用户的数据范围（`ScopedPrincipal`，只能由 `principal::resolve` 构造）；
/// - `filter`：状态 / 优先级 / 负责人 / 截止范围 / 关键词；
/// - `page` / `page_size`：分页（已规范化）。
///
/// # 返回值
///
/// [`Page<TaskCardRow>`]，排序 `截止日升序（NULL 最后）→ 创建时间倒序`。
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
) -> DbResult<Page<TaskCardRow>> {
    assert_scope_fragment(principal)?;

    let mut sql = String::with_capacity(1024);
    sql.push_str("SELECT ");
    sql.push_str(CARD_COLUMNS);
    sql.push_str(CARD_FROM);
    sql.push_str(" WHERE t.deleted_at IS NULL");
    sql.push_str(VISIBLE_OR_TAIL);

    // 参数顺序：可见性 4 个「我」在前（`{FILTER}` 片段参数由 paginate 前置），
    // 业务过滤参数在后 —— 与 SQL 里 `?` 的文本顺序一致。
    let mut params = visible_params(principal.user_id());
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

    let rows = list
        .iter()
        .map(TaskCardRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(Page {
        list: rows,
        total,
        page,
        size,
        pages,
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
async fn find_detail_with_visibility(
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
         t.cancel_reason, t.reject_reason, t.approval_instance_id ",
    );
    sql.push_str(CARD_FROM);
    sql.push_str(" LEFT JOIN sys_user cu ON cu.id = t.creator_id");
    sql.push_str(" LEFT JOIN sys_user au ON au.id = t.assigner_id");
    sql.push_str(" LEFT JOIN sys_dept d ON d.id = t.dept_id");
    sql.push_str(" LEFT JOIN biz_task pt ON pt.id = t.parent_id");
    sql.push_str(" WHERE t.deleted_at IS NULL");
    sql.push_str(&tail);

    // 参数顺序必须与 SQL 文本顺序一致：先 `id`（若在可见性之前）——这里刻意把
    // id / parent_id 放在可见性谓词**之后**，于是参数也是「片段 + 4 个我 + 定位」。
    let mut params = fragment.params().to_vec();
    params.extend(visible_params(principal.user_id()));
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
fn detail_from_row(row: &Row) -> DbResult<TaskDetailRow> {
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

    let mut params = fragment.params().to_vec();
    params.extend(visible_params(principal.user_id()));
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
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn list_attachments(task_id: Uuid) -> DbResult<Vec<TaskAttachmentRow>> {
    let rows = db()
        .find_with_params(
            "SELECT a.id, a.file_id, \
                    COALESCE(f.original_name, f.file_name, '') AS file_name, \
                    COALESCE(f.file_size, 0) AS file_size, a.sort_order \
               FROM biz_task_attachment a LEFT JOIN sys_file f ON f.id = a.file_id \
              WHERE a.task_id = ?::uuid \
              ORDER BY a.sort_order ASC, a.id ASC",
            vec![Value::from(&task_id)],
        )
        .await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(TaskAttachmentRow {
            id: row.get_typed("id")?,
            file_id: row.get_typed("file_id")?,
            file_name: row.get_typed("file_name")?,
            file_size: row.get_typed("file_size")?,
            sort_order: row.get_typed("sort_order")?,
        });
    }
    Ok(list)
}

/// 「我的任务」/「未完成子任务」共同使用的**未终态状态集**。
///
/// # 为什么用 `IN (...)` 而不是 `NOT IN ('DONE','CANCELLED')`
///
/// 两者在 5 态 CHECK 约束下语义完全等价（状态只有 5 个），但执行计划差 3 倍：
/// 实测 5 万行（t1_emp 名下 16667 / 未终态 5001）——
///
/// | 写法 | 计划 | 耗时 |
/// |------|------|------|
/// | `status NOT IN ('DONE','CANCELLED')` | Bitmap Index Scan on **idx_biz_task_due_open** + Filter(owner_id) → 4998 行被过滤 | 6.795 ms |
/// | `status IN ('TODO','DOING','REVIEW')` | Bitmap Index Scan on **idx_biz_task_owner_status_due**，Index Cond 同时含 owner_id 与 status | **2.179 ms** |
///
/// 原因：`NOT IN` 与部分索引 `idx_biz_task_due_open` 的谓词**逐字相同**，PG 认为那条
/// 更便宜，于是把 `owner_id` 降级成 Filter（扫 30037 条索引项、过滤掉 10004 行）。
/// 写成 `IN` 后部分索引的谓词不再匹配，(owner_id, status, due_date) 这条组合索引才被选中。
///
/// 代价：状态集合多了一处字面量。它与 `biz_task.status` 的 CHECK 约束、`dto::STATUSES`
/// 是同一份事实的三处表达 —— 新增状态时三处都要改（CHECK 改不了就说明本版不该加状态）。
pub const OPEN_STATES: [&str; 3] = ["TODO", "DOING", "REVIEW"];

/// 未终态状态的 SQL 字面量（`'TODO', 'DOING', 'REVIEW'`，唯一来源是 [`OPEN_STATES`]）。
pub fn open_states_sql() -> String {
    OPEN_STATES
        .iter()
        .map(|state| format!("'{state}'"))
        .collect::<Vec<_>>()
        .join(", ")
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
    rows.iter().map(TaskCardRow::from_row).collect()
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
                    t.description, t.creator_id, \
                    COALESCE(NULLIF(cu.real_name, ''), cu.username, '') AS creator_name, \
                    t.assigner_id, \
                    COALESCE(NULLIF(au.real_name, ''), au.username, '') AS assigner_name, \
                    COALESCE(d.dept_name, '') AS dept_name, \
                    t.parent_id, pt.task_no AS parent_task_no, t.project_id, t.done_at, \
                    t.estimate_hours::float8 AS estimate_hours, \
                    t.actual_hours::float8 AS actual_hours, \
                    t.cancel_reason, t.reject_reason, t.approval_instance_id \
               FROM biz_task t \
               LEFT JOIN sys_user u ON u.id = t.owner_id \
               LEFT JOIN sys_user cu ON cu.id = t.creator_id \
               LEFT JOIN sys_user au ON au.id = t.assigner_id \
               LEFT JOIN sys_dept d ON d.id = t.dept_id \
               LEFT JOIN biz_task pt ON pt.id = t.parent_id \
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

// ==================== 写路径 ====================

/// 新建任务的入参（避免 `clippy::too_many_arguments`，且字段语义集中在一处）。
#[derive(Debug, Clone)]
pub struct NewTask {
    /// 主键（handler 用 `Uuid::now_v7()` 生成，保证有序）。
    pub id: Uuid,
    /// 标题。
    pub title: String,
    /// 描述。
    pub description: Option<String>,
    /// 优先级。
    pub priority: String,
    /// 是否需验收。
    pub need_review: bool,
    /// 负责人。
    pub owner_id: Uuid,
    /// 创建人。
    pub creator_id: Uuid,
    /// 分派人（自建为空）。
    pub assigner_id: Option<Uuid>,
    /// 归属部门快照。
    pub dept_id: Option<Uuid>,
    /// 父任务。
    pub parent_id: Option<Uuid>,
    /// 项目。
    pub project_id: Option<Uuid>,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日。
    pub due_date: Option<NaiveDate>,
    /// 预估工时。
    pub estimate_hours: Option<f64>,
    /// 来源。
    pub source: String,
    /// 是否把创建人自动加成协作人（分派场景：分派人要能看到进度，§八）。
    pub add_creator_as_collaborator: bool,
}

/// 新建任务（同事务内生成 `task_no`）。
///
/// # 编号生成（`TSK-YYYYMM-NNNNNN`）
///
/// 1. `pg_advisory_xact_lock(7700000000 + YYYYMM)` —— 事务级咨询锁，**按月串行化**，
///    提交/回滚自动释放。并发创建时后到者等待，不会拿到同一个序号；
/// 2. `max(月内已有最大序号) + 1` —— 含软删行（编号不复用）；
/// 3. 落库。
///
/// 不用「全局 sequence」是因为编号要求按月重置；不用「先查后插不加锁」是因为并发下
/// 两个请求会算出同一个序号（虽然唯一索引会拦，但用户看到的是 500 而不是成功）。
/// 也不额外建计数表 —— 7 张表的模型是老大定稿的，不为编号再开一张表。
///
/// # 返回值
///
/// 生成的任务编号。
///
/// # 异常情况
///
/// - 数据库异常（含 `uk_biz_task_task_no` 冲突）→ [`DbError`]。
pub async fn create_task(
    ex: &Transaction,
    new: &NewTask,
    operator: Uuid,
    month: &str,
) -> DbResult<String> {
    let task_no = next_task_no(ex, month).await?;

    ex.execute_with_params(
        "INSERT INTO biz_task \
                 (id, task_no, title, description, status, priority, progress, need_review, \
                  owner_id, creator_id, assigner_id, dept_id, parent_id, project_id, \
                  start_date, due_date, done_at, estimate_hours, actual_hours, source, \
                  cancel_reason, reject_reason, approval_instance_id, \
                  created_at, updated_at, create_by, update_by) \
             VALUES (?::uuid, ?, ?, ?, 'TODO', ?, 0, ?, \
                     ?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::uuid, \
                     ?::date, ?::date, NULL, ?::numeric, NULL, ?, \
                     NULL, NULL, NULL, \
                     now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&new.id),
            Value::Text(task_no.clone()),
            Value::Text(new.title.clone()),
            Value::from(new.description.clone()),
            Value::Text(new.priority.clone()),
            Value::Bool(new.need_review),
            Value::from(&new.owner_id),
            Value::from(&new.creator_id),
            Value::from(new.assigner_id.as_ref()),
            Value::from(new.dept_id.as_ref()),
            Value::from(new.parent_id.as_ref()),
            Value::from(new.project_id.as_ref()),
            Value::from(new.start_date),
            Value::from(new.due_date),
            Value::from(new.estimate_hours),
            Value::Text(new.source.clone()),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;

    if new.add_creator_as_collaborator {
        add_member(ex, new.id, new.creator_id, "COLLABORATOR", operator).await?;
    }
    Ok(task_no)
}

/// 生成下一个任务编号（**必须在事务内调用**：依赖 `pg_advisory_xact_lock`）。
async fn next_task_no(ex: &Transaction, month: &str) -> DbResult<String> {
    // 咨询锁键：7700000000 + YYYYMM（固定命名空间，避免与其它模块的 advisory lock 撞号）
    let month_number: i64 = month.parse().unwrap_or(0);
    let lock_key = 7_700_000_000_i64 + month_number;
    ex.find_with_params(
        "SELECT 1 AS locked FROM pg_advisory_xact_lock(?) AS l",
        vec![Value::I64(lock_key)],
    )
    .await?;

    let pattern = format!("TSK-{month}-%");
    let rows = ex
        .find_with_params(
            "SELECT COALESCE(max(substring(task_no from 12)::bigint), 0) + 1 AS next_no \
               FROM biz_task \
              WHERE task_no LIKE ? AND substring(task_no from 12) ~ '^[0-9]+$'",
            vec![Value::Text(pattern)],
        )
        .await?;
    let next_no: i64 = match rows.first() {
        Some(row) => row.get_typed("next_no")?,
        None => 1,
    };
    Ok(format_task_no(month, next_no))
}

/// 协作人 / 关注者入库（`ON CONFLICT DO NOTHING`：重复添加幂等）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn add_member(
    ex: &Transaction,
    task_id: Uuid,
    user_id: Uuid,
    role: &str,
    operator: Uuid,
) -> DbResult<()> {
    ex.execute_with_params(
        "INSERT INTO biz_task_member (task_id, user_id, role, created_at, create_by) \
              VALUES (?::uuid, ?::uuid, ?, now(), ?::uuid) \
         ON CONFLICT (task_id, user_id) DO NOTHING",
        vec![
            Value::from(&task_id),
            Value::from(&user_id),
            Value::Text(role.to_string()),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(())
}

/// 编辑任务的可改字段集合（**只描述变化**，未变的字段不写 SQL）。
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    /// 标题。
    pub title: Option<String>,
    /// 描述（`Some(None)` = 清空）。
    pub description: Option<Option<String>>,
    /// 优先级。
    pub priority: Option<String>,
    /// 是否需验收。
    pub need_review: Option<bool>,
    /// 负责人 + 新的部门快照（改负责人时一起刷新 `dept_id`）。
    pub owner: Option<(Uuid, Option<Uuid>)>,
    /// 父任务（`Some(None)` = 摘成顶层）。
    pub parent_id: Option<Option<Uuid>>,
    /// 项目（`Some(None)` = 移出项目）。
    pub project_id: Option<Option<Uuid>>,
    /// 计划开始日（`Some(None)` = 清空）。
    pub start_date: Option<Option<NaiveDate>>,
    /// 截止日（`Some(None)` = 清空）。
    pub due_date: Option<Option<NaiveDate>>,
    /// 预估工时（`Some(None)` = 清空）。
    pub estimate_hours: Option<Option<f64>>,
}

impl TaskPatch {
    /// 是否没有任何改动。
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.priority.is_none()
            && self.need_review.is_none()
            && self.owner.is_none()
            && self.parent_id.is_none()
            && self.project_id.is_none()
            && self.start_date.is_none()
            && self.due_date.is_none()
            && self.estimate_hours.is_none()
    }
}

/// 编辑任务（只更新传入的字段）。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删，调用方转 404）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn update_task(
    ex: &Transaction,
    id: Uuid,
    patch: &TaskPatch,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets: Vec<&'static str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if let Some(title) = patch.title.as_ref() {
        sets.push("title = ?");
        params.push(Value::Text(title.clone()));
    }
    if let Some(description) = patch.description.as_ref() {
        sets.push("description = ?");
        params.push(Value::from(description.clone()));
    }
    if let Some(priority) = patch.priority.as_ref() {
        sets.push("priority = ?");
        params.push(Value::Text(priority.clone()));
    }
    if let Some(need_review) = patch.need_review {
        sets.push("need_review = ?");
        params.push(Value::Bool(need_review));
    }
    if let Some((owner_id, dept_id)) = patch.owner.as_ref() {
        sets.push("owner_id = ?::uuid");
        params.push(Value::from(owner_id));
        sets.push("dept_id = ?::uuid");
        params.push(Value::from(dept_id.as_ref()));
    }
    if let Some(parent_id) = patch.parent_id.as_ref() {
        sets.push("parent_id = ?::uuid");
        params.push(Value::from(parent_id.as_ref()));
    }
    if let Some(project_id) = patch.project_id.as_ref() {
        sets.push("project_id = ?::uuid");
        params.push(Value::from(project_id.as_ref()));
    }
    if let Some(start_date) = patch.start_date.as_ref() {
        sets.push("start_date = ?::date");
        params.push(Value::from(*start_date));
    }
    if let Some(due_date) = patch.due_date.as_ref() {
        sets.push("due_date = ?::date");
        params.push(Value::from(*due_date));
    }
    if let Some(estimate_hours) = patch.estimate_hours.as_ref() {
        sets.push("estimate_hours = ?::numeric");
        params.push(Value::from(*estimate_hours));
    }

    if sets.is_empty() {
        return Ok(0);
    }
    sets.push("updated_at = now()");
    sets.push("update_by = ?::uuid");
    params.push(Value::from(&operator));
    params.push(Value::from(&id));

    let sql = format!(
        "UPDATE biz_task SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    ex.execute_with_params(&sql, params).await
}

/// 软删除任务。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn soft_delete(ex: &Transaction, id: Uuid, operator: Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_task SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&operator), Value::from(&id)],
    )
    .await
}

/// 父任务校验：存在、未软删、深度不超过 2 层、不是自己 / 不是自己的子孙。
///
/// # 规则（设计文档 §八「子任务深度限 2 层」）
///
/// - 目标父任务必须存在且未软删；
/// - 父任务自身**不能**已有 `parent_id`（否则新任务就是第 3 层）；
/// - 不能把自己设成父任务；
/// - 父任务不能是自己的**子任务**（否则成环，`parent_id` 成环会让详情页无限展开）。
///
/// # 异常情况
///
/// - 父任务不存在 → `DbError::InvalidValue`（handler 转 400 + `TASK_PARENT_INVALID`）；
/// - 深度 / 成环 → 同上。
pub async fn validate_parent(parent_id: Uuid, self_id: Option<Uuid>) -> DbResult<()> {
    if Some(parent_id) == self_id {
        return Err(DbError::InvalidValue("父任务不能是任务自己".to_string()));
    }
    let rows = db()
        .find_with_params(
            "SELECT id, parent_id, deleted_at FROM biz_task WHERE id = ?::uuid",
            vec![Value::from(&parent_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(DbError::InvalidValue(format!("父任务不存在：{parent_id}")));
    };
    let deleted_at: Option<chrono::DateTime<Utc>> = row.get_typed("deleted_at")?;
    if deleted_at.is_some() {
        return Err(DbError::InvalidValue(format!("父任务已删除：{parent_id}")));
    }
    let grand_parent: Option<Uuid> = row.get_typed("parent_id")?;
    if grand_parent.is_some() {
        return Err(DbError::InvalidValue(
            "子任务最多 2 层：目标父任务自身已是子任务".to_string(),
        ));
    }
    if let Some(self_id) = self_id {
        let children = db()
            .find_with_params(
                "SELECT 1 AS hit FROM biz_task \
                  WHERE parent_id = ?::uuid AND id = ?::uuid",
                vec![Value::from(&self_id), Value::from(&parent_id)],
            )
            .await?;
        if !children.is_empty() {
            return Err(DbError::InvalidValue(
                "父任务不能是自己的子任务（会成环）".to_string(),
            ));
        }
    }
    Ok(())
}

// ==================== 单元测试（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("合法日期")
    }

    #[test]
    fn visible_tail_keeps_the_scope_marker() {
        // 拼法的前提：片段标记必须在 OR 组的左半边，且只出现一次
        assert_eq!(
            VISIBLE_OR_TAIL.matches(scope::FILTER_MARKER).count(),
            1,
            "VISIBLE_OR_TAIL 必须且只能有一个 {{FILTER}} 标记"
        );
        assert!(
            VISIBLE_OR_TAIL.contains("(1 = 1 {FILTER})"),
            "scope 片段必须包在 (1 = 1 …) 里，否则 AND 片段会退化成裸条件"
        );
        // 4 个 OR 分支 + 1 个 EXISTS = 可见性并集的全部来源
        assert_eq!(VISIBLE_OR_TAIL.matches("t.owner_id = ?::uuid").count(), 1);
        assert_eq!(VISIBLE_OR_TAIL.matches("t.creator_id = ?::uuid").count(), 1);
        assert_eq!(
            VISIBLE_OR_TAIL.matches("t.assigner_id = ?::uuid").count(),
            1
        );
        assert!(VISIBLE_OR_TAIL.contains("FROM biz_task_member m"));
        // 占位符数量（不含 {FILTER} 内的）必须等于 visible_params 的长度
        assert_eq!(VISIBLE_OR_TAIL.matches("?::uuid").count(), 4);
    }

    #[test]
    fn fragment_shape_guard_accepts_known_shapes_and_rejects_or() {
        assert!(check_fragment_shape(" AND t.dept_id = ?").is_ok());
        assert!(check_fragment_shape("AND (t.dept_id = ANY(?::text::uuid[]))").is_ok());
        assert!(check_fragment_shape(" 1 = 1 ").is_ok());
        assert!(check_fragment_shape("1 = 0").is_ok());
        // 未来的 OR 片段必须被拦下（否则 (1 = 1 OR …) 恒真 → 全表可见）
        assert!(check_fragment_shape(" OR t.dept_id = ?").is_err());
        assert!(check_fragment_shape("t.dept_id = ?").is_err());
    }

    #[test]
    fn visible_params_are_all_me() {
        let me = Uuid::nil();
        let params = visible_params(me);
        assert_eq!(params.len(), 4);
        for param in params {
            assert_eq!(param, Value::from(&me));
        }
    }

    #[test]
    fn default_due_date_is_this_friday() {
        // 2026-10-29 是周四 → 本周五 = 10-30
        assert_eq!(default_due_date(date(2026, 10, 29)), date(2026, 10, 30));
        // 周五当天 → 今天
        assert_eq!(default_due_date(date(2026, 10, 30)), date(2026, 10, 30));
        // 周六 / 周日 → 下一个周五（周末建任务不该一创建就逾期）
        assert_eq!(default_due_date(date(2026, 10, 31)), date(2026, 11, 6));
        assert_eq!(default_due_date(date(2026, 11, 1)), date(2026, 11, 6));
        // 周一 → 本周五
        assert_eq!(default_due_date(date(2026, 11, 2)), date(2026, 11, 6));
    }

    #[test]
    fn buckets_cover_all_cases() {
        let today = date(2026, 10, 29); // 周四；本周日 = 11-01
        assert_eq!(bucket_of(None, today), MyBucket::NoDue);
        assert_eq!(
            bucket_of(Some(date(2026, 10, 28)), today),
            MyBucket::Overdue
        );
        assert_eq!(bucket_of(Some(today), today), MyBucket::Today);
        assert_eq!(
            bucket_of(Some(date(2026, 10, 30)), today),
            MyBucket::ThisWeek
        );
        assert_eq!(
            bucket_of(Some(date(2026, 11, 1)), today),
            MyBucket::ThisWeek,
            "周日仍算本周"
        );
        assert_eq!(
            bucket_of(Some(date(2026, 11, 2)), today),
            MyBucket::Later,
            "下周一算以后"
        );
    }

    #[test]
    fn task_no_is_zero_padded_and_month_scoped() {
        assert_eq!(format_task_no("202610", 1), "TSK-202610-000001");
        assert_eq!(format_task_no("202610", 123456), "TSK-202610-123456");
        assert_eq!(month_key(date(2026, 10, 29)), "202610");
        // substring(task_no from 12) 的口径必须与编号格式一致：前缀 TSK-YYYYMM- 共 11 字符
        let sample = format_task_no("202610", 42);
        assert_eq!(&sample[10..], "-000042");
        assert_eq!(&sample[11..], "000042");
        assert_eq!(sample.chars().take(11).collect::<String>(), "TSK-202610-");
    }

    #[test]
    fn cst_date_shifts_by_eight_hours() {
        let utc = chrono::DateTime::parse_from_rfc3339("2026-10-29T16:30:00Z")
            .expect("合法 RFC3339")
            .with_timezone(&Utc);
        // UTC 16:30 = CST 次日 00:30
        assert_eq!(cst_date_of(utc), date(2026, 10, 30));
        let utc_early = chrono::DateTime::parse_from_rfc3339("2026-10-29T01:00:00Z")
            .expect("合法 RFC3339")
            .with_timezone(&Utc);
        assert_eq!(cst_date_of(utc_early), date(2026, 10, 29));
    }

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("plain"), "plain");
    }

    #[test]
    fn patch_empty_detection() {
        assert!(TaskPatch::default().is_empty());
        let patch = TaskPatch {
            priority: Some("P0".to_string()),
            ..TaskPatch::default()
        };
        assert!(!patch.is_empty());
    }
}

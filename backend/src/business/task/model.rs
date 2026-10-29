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
//! ## 实现方式：**参与 id 数组**（`docs/批次C+-工作任务管理-详细架构.md` §4.4 决策 D6）
//!
//! `scope.rs` 是批次 B 的门禁核心（闸①/闸② 都盯着它），本卡红线不许动。所以这里
//! 用一个**不改变片段语义**的拼法把 AND 片段包进 OR 组：
//!
//! ```sql
//! WHERE t.deleted_at IS NULL
//!   AND ( (1 = 1 {FILTER})                     -- ← paginate 把 ` AND …` 片段原位注入这里
//!         OR t.id = ANY(?::text::uuid[]) )     -- ← 我参与的 task_id 数组（含负责人/创建人/分派人）
//! ```
//!
//! `{FILTER}` 会被替换成 ` AND <scope 条件>`，于是左半边等价于 `(1 = 1 AND <scope 条件>)`
//! = `<scope 条件>`；整体就是 `scope 条件 OR 参与关系`。片段本身、参数顺序、count 剥离
//! ORDER BY 的能力全部仍由 [`scope::paginate`] 提供 —— 本模块只负责「放在哪儿」。
//!
//! ### 为什么是数组而不是 `OR EXISTS(...)`（D6 的实测依据）
//!
//! 第一版写的是「4 个列 OR + 1 个 `EXISTS` 子查询」。20 万任务下实测：
//!
//! | 写法 | `DeptAndChild` 范围耗时 | 计划 |
//! |---|---|---|
//! | 多 OR + `EXISTS` | **539 ms** | `Seq Scan on biz_task`（5 个 OR 把索引全废） |
//! | **参与 id 数组 `t.id = ANY(?)`** | **40 ms** | `BitmapOr(idx_biz_task_dept, biz_task_pkey)` |
//!
//! 优化器无法在「列 OR 子查询 EXISTS」之上做位图合并，只能退化成顺序扫描；换成
//! 一个「已经算好的 id 集合」后，`t.id = ANY(…)` 是纯粹的 pkey 条件，可以和部门条件
//! 组成 `BitmapOr`。id 集合由 [`my_member_task_ids`] 用一条索引查询算好（一次请求一次），
//! 把「我负责 / 我创建 / 我分派 / 我是协作人」四种身份**全部 OR 进同一个数组**，
//! 于是谓词里只剩一个 `ANY(?)`。
//!
//! ### 上限与降级（D7）
//!
//! 数组不是免费的：每个 id 一次索引探测。实测 6.6 万个 id 时查询 >15 s 被
//! `statement_timeout` 打断。所以 [`my_member_task_ids`] 有 **2000 硬上限**（[`MEMBER_SCOPE_LIMIT`]）：
//! 超过就返回 `truncated = true`，谓词降级成「只按范围」（数组参数传 `{}`，`ANY('{}')`
//! 恒假），并在分页响应里带 `member_scope_truncated: true` 让前端提示
//! 「我参与的任务过多，本视图只显示范围内的；『我参与的』请走独立 tab」。
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
use crate::business::task::state::{self, ChildSnapshot, TaskStatus};

// ==================== 数据权限锚点 ====================

/// 任务表的 scope 锚点：部门列 `dept_id`（归属部门快照），归属列 `owner_id`（负责人）。
///
/// 别名固定为 `t`：`{FILTER}` 片段会以 `t.dept_id` / `t.owner_id` 的形式拼进来，
/// SQL 里**不得**给 `biz_task` 用别的别名（`shared/scope.rs` 的 §2.6 规则 1：
/// 片段只锚定主体表）。
pub const TASK_TARGET: scope::ScopeTarget =
    scope::ScopeTarget::new("biz_task", "t", Some("dept_id"), Some("owner_id"));

/// 可见性谓词：**数据范围 ∪ 参与 id 数组**（决策 D6）。
///
/// 占位符顺序（**文本顺序**，必须与参数顺序一致）：
/// ① `{FILTER}` 内的 scope 片段参数（由 [`scope::paginate`] 前置）；
/// ② 参与 id 数组（`?::text::uuid[]`）—— 见 [`visible_params`]。
///
/// # 不许在这里再加 `OR` 分支
///
/// 多 OR（尤其带 `EXISTS` 子查询）会让优化器放弃 OR 上的位图合并、退化成
/// `Seq Scan on biz_task`（20 万任务实测 539 ms，见模块文档）。
/// 新的「我参与」身份要并进 [`my_member_task_ids`] 的 id 集合，而不是加 OR。
pub const VISIBLE_OR_TAIL: &str = " AND ( (1 = 1 {FILTER}) OR t.id = ANY(?::text::uuid[]) )";

/// 「我参与」id 数组的硬上限（决策 D7：6.6 万个 id 时查询 >15 s 被 statement_timeout 打断）。
pub const MEMBER_SCOPE_LIMIT: i64 = 2000;

/// 「我参与的 task_id 集合」+ 降级标记（决策 D6 / D7）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MyScopeIds {
    /// 参与 id：负责人 ∪ 创建人 ∪ 分派人 ∪ 协作人/关注者（已去重）。
    pub ids: Vec<Uuid>,
    /// 是否超过 [`MEMBER_SCOPE_LIMIT`]；`true` = 谓词已降级为「只按范围」。
    pub truncated: bool,
}

impl MyScopeIds {
    /// 绑定给 `?::text::uuid[]` 的参数。
    ///
    /// 降级（`truncated`）时**刻意传空数组**：`t.id = ANY('{}')` 恒假，谓词自动退化成
    /// 「只按范围」（设计文档 D7 的降级口径），且参数个数恒定、不会错位。
    pub fn param(&self) -> Value {
        if self.truncated {
            return ids_array_param(&[]);
        }
        ids_array_param(&self.ids)
    }
}

/// 把 id 集合编码成 PostgreSQL 文本数组字面量 `{uuid,uuid}`（空集合 = `{}`）。
///
/// 与 `scope.rs::array_param` 同一套路：本栈驱动不支持 `Value::Array` 绑 `uuid[]`
/// （`shared/scope.rs` 开头记录了该限制），「文本字面量 + `?::text::uuid[]`」是唯一稳写法。
fn ids_array_param(ids: &[Uuid]) -> Value {
    let mut literal = String::with_capacity(ids.len() * 37 + 2);
    literal.push('{');
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(&id.to_string());
    }
    literal.push('}');
    Value::Text(literal)
}

/// 可见性谓词的参数（顺序必须与 [`VISIBLE_OR_TAIL`] 里 `?` 的文本顺序一致）。
pub fn visible_params(scope: &MyScopeIds) -> Vec<Value> {
    vec![scope.param()]
}

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

/// 任务分页结果（分页元数据 + D7 降级标记）。
///
/// 为什么不用框架的 `Page<T>`：降级标记必须随分页一起下发（前端据此提示
/// 「我参与的任务过多，本视图只显示范围内的；『我参与的』请走独立 tab」），
/// 而 `Page<T>` 没有扩展位。字段与 `Page<T>` 一一对应，出参 `list/total/page/page_size`
/// 的形态不变（`PageData` 的 JSON 契约对前端保持兼容）。
#[derive(Debug, Clone)]
pub struct VisiblePage {
    /// 当前页任务卡。
    pub list: Vec<TaskCardRow>,
    /// 可见总数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数。
    pub size: u64,
    /// 总页数。
    pub pages: u64,
    /// 「我参与」id 超过 [`MEMBER_SCOPE_LIMIT`] → 本次查询已降级为「只按范围」（D7）。
    pub member_scope_truncated: bool,
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

    let rows = list
        .iter()
        .map(TaskCardRow::from_row)
        .collect::<DbResult<Vec<_>>>()?;
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

// ==================== 状态机 / 进度（T2） ====================
//
// 本段的每个函数都只做「一件事」，状态机规则本身全部在 [`crate::business::task::state`]
// （纯函数、可无库单测）。这里只负责把它翻译成 SQL。
//
// 三条写路径的边界（改这里之前先读）：
//   ① **状态流转** [`apply_status`]：只写 `biz_task` 的 5 个列，不碰子任务、不碰父任务；
//   ② **进度汇总** [`recalc_ancestors`]：**只由子任务变更触发**，沿 `parent_id` 向上逐级；
//   ③ **进度直改** [`set_progress`]：**有子任务时必须先被 handler 拦下来**（409 + 63012），
//      本函数不做这项判断 —— 它拿不到「谁在调」，把闸放在 handler 是刻意的（一处判定）。

/// 父任务链向上重算的最大深度（防御性上限：`parent_id` 成环时不会死循环）。
///
/// 业务上限是 2 层（T1 的 `validate_parent`），这里给 8 层是留给将来放宽深度的余量，
/// 同时保证「数据被手工改环」时也能停下来（配合 [`recalc_ancestors`] 的 visited 集合）。
pub const MAX_PARENT_DEPTH: usize = 8;

/// 父任务重算后是否写一条 `PROGRESS_AUTO` 评论（「可配置静默」的开关）。
///
/// 默认 `true`：进度是任务卡上最显眼的数字，被自动改动必须留痕（谁在什么时候
/// 把 60% 变成 80%、因为哪个子任务）。**只有进度真的变了才写**，所以不会刷屏。
/// 将来接 `sys_config` 时把这里换成配置读取即可，调用方不用改。
pub const PROGRESS_AUTO_COMMENT: bool = true;

/// 子任务快照行（聚合口径只需要 `status` / `progress` / `estimate_hours`）。
#[derive(Debug, Clone)]
pub struct ChildRow {
    /// 子任务 ID。
    pub id: Uuid,
    /// 状态字面量。
    pub status: String,
    /// 进度 0~100。
    pub progress: i16,
    /// 预估工时（小时，可空）。
    pub estimate_hours: Option<f64>,
}

impl ChildRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            status: row.get_typed("status")?,
            progress: row.get_typed("progress")?,
            estimate_hours: row.get_typed("estimate_hours")?,
        })
    }

    /// 转成纯逻辑层的快照（状态字符串解析失败按 `TODO` 处理，不 panic）。
    pub fn snapshot(&self) -> ChildSnapshot {
        ChildSnapshot {
            status: TaskStatus::parse(&self.status).unwrap_or(TaskStatus::Todo),
            progress: self.progress,
            estimate_hours: self.estimate_hours,
        }
    }
}

/// 子任务清单（按创建顺序；`deleted_at IS NULL`）。
///
/// 通用执行器版本：既能吃 `&Transaction`（写路径重算），也能吃 `db()`（读路径）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
async fn child_rows_with(ex: &impl SqlExecutor, parent_id: Uuid) -> DbResult<Vec<ChildRow>> {
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

/// 状态流转的写库载荷（由 handler 依 [`crate::business::task::state::Transition`] 构造）。
#[derive(Debug, Clone, Default)]
pub struct StatusUpdate {
    /// 目标状态字面量。
    pub to: String,
    /// 新进度（`None` = 不动；有子任务的任务恒为 `None`）。
    pub progress: Option<i16>,
    /// `done_at` 的三态写法：`None` = 不动 / `Some(None)` = 清空 / `Some(Some(t))` = 写 t。
    ///
    /// 时间点在 **Rust 侧算好**再绑定（不写 SQL 的 `now()`）：变更日志的 diff 必须与
    /// 库里落的是**同一个**值，否则审计里会出现「日志说 12:00:00.001、库里 12:00:00.002」。
    pub done_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    /// 打回原因（`REVIEW → DOING` 时写）。
    pub reject_reason: Option<String>,
    /// 取消原因（可选）。
    pub cancel_reason: Option<String>,
}

/// 落库一次状态流转（**只碰本行**，副作用由调用方另行处理）。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删）。
///
/// # 异常情况
///
/// 数据库异常（含 `ck_biz_task_status` 违约）→ [`DbError`]。
pub async fn apply_status(
    ex: &Transaction,
    id: Uuid,
    update: &StatusUpdate,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets: Vec<&'static str> = vec!["status = ?"];
    let mut params: Vec<Value> = vec![Value::Text(update.to.clone())];

    if let Some(progress) = update.progress {
        sets.push("progress = ?::smallint");
        params.push(Value::I16(progress));
    }
    match update.done_at {
        None => {}
        // 清空与写值分成两条**静态** SQL 片段：`?::timestamptz` 绑 NULL 在驱动层不可靠
        // （alun-db 的参数编码按类型推断，NULL 会被推断成 text），所以清空直接写 `NULL`。
        Some(None) => sets.push("done_at = NULL"),
        Some(Some(stamp)) => {
            sets.push("done_at = ?::timestamptz");
            params.push(Value::from(stamp));
        }
    }
    if let Some(reason) = update.reject_reason.as_ref() {
        sets.push("reject_reason = ?");
        params.push(Value::Text(reason.clone()));
    }
    if let Some(reason) = update.cancel_reason.as_ref() {
        sets.push("cancel_reason = ?");
        params.push(Value::Text(reason.clone()));
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

/// 直接改进度（**无子任务的任务才允许**；有子任务的闸在 handler 侧）。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删）。
///
/// # 异常情况
///
/// 数据库异常（含 `ck_biz_task_progress` 越界）→ [`DbError`]。
pub async fn set_progress(
    ex: &Transaction,
    id: Uuid,
    progress: i16,
    operator: Uuid,
) -> DbResult<u64> {
    ex.execute_with_params(
        "UPDATE biz_task SET progress = ?::smallint, updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::I16(progress),
            Value::from(&operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 写一条任务评论（状态变更 / 进度自动汇总都走它）。
///
/// `comment_type` 是 `V0108` 新增的列：`PLAIN` / `STATUS_CHANGE` / `PROGRESS_AUTO`。
/// 状态流转的评论**必须**用 `STATUS_CHANGE`（前端详情页的时间线按它画箭头），
/// 否则「谁把任务改成了什么」会和普通闲聊混在一起。
///
/// # 返回值
///
/// 新评论 ID（UUIDv7，便于按时间排序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn insert_comment(
    ex: &Transaction,
    task_id: Uuid,
    user_id: Uuid,
    content: &str,
    comment_type: &str,
    operator: Uuid,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    ex.execute_with_params(
        "INSERT INTO biz_task_comment \
             (id, task_id, user_id, content, mentions, comment_type, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, '[]'::jsonb, ?, now(), now(), ?::uuid, ?::uuid)",
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&user_id),
            Value::Text(content.to_string()),
            Value::Text(comment_type.to_string()),
            Value::from(&operator),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(id)
}

/// 一次父任务重算的结果（通知与日志用）。
#[derive(Debug, Clone)]
pub struct ProgressRecalc {
    /// 被重算的任务（父 / 祖父 …）。
    pub task_id: Uuid,
    /// 重算前。
    pub old: i16,
    /// 重算后。
    pub new: i16,
    /// 参与聚合的子任务数。
    pub children: usize,
    /// 子任务是否已全部完成（「待汇总」通知的触发条件）。
    pub all_done: bool,
    /// 聚合口径（「工时加权」/「等权平均」/「子任务全部完成」）。
    pub mode: &'static str,
}

/// 沿 `parent_id` 向上逐级重算进度（**写路径**：子任务变更后调用）。
///
/// # 为什么是「写路径重算」而不是「读时计算」
///
/// 读路径（列表 / 详情 / 甘特图）每次都要聚合子任务 = 每条父任务多一次聚合查询，
/// 一个 20 行的看板就是 20 次额外往返；而写操作是少数（一次状态变更 = 1 次重算）。
/// 代价是「父任务进度可能短暂过期」—— 所以**所有**会改子任务进度的写路径
/// （状态流转、进度直改、建子任务）都必须调用本函数，这是本段的唯一契约。
///
/// # 行为
///
/// - 从 `start` 的父任务开始，逐级向上（深度上限 [`MAX_PARENT_DEPTH`] + visited 防环）；
/// - 每级用 [`state::aggregate_progress`] 算出新进度，**变了才写**；
/// - 变了就写 `biz_task.progress` + 一条 `PROGRESS_AUTO` 评论（[`PROGRESS_AUTO_COMMENT`]）；
/// - **绝不改状态**（架构 D5：父任务进度到 100% 也不自动 DONE）。
///
/// # 返回值
///
/// 每一级的重算结果（顺序：由近及远）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（由调用方的事务整体回滚）。
pub async fn recalc_ancestors(
    ex: &Transaction,
    start: Uuid,
    operator: Uuid,
) -> DbResult<Vec<ProgressRecalc>> {
    let mut results = Vec::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut cursor = parent_of(ex, start).await?;
    while let Some(parent_id) = cursor {
        if visited.len() >= MAX_PARENT_DEPTH {
            tracing::warn!(
                task_id = %start,
                depth = visited.len() as i64,
                "父任务链过深（疑似成环），停止向上重算"
            );
            break;
        }
        if !visited.insert(parent_id) {
            tracing::warn!(
                task_id = %start,
                parent_id = %parent_id,
                "父任务链成环，停止向上重算"
            );
            break;
        }
        let children = child_rows_with(ex, parent_id).await?;
        let snapshots: Vec<ChildSnapshot> = children.iter().map(ChildRow::snapshot).collect();
        let Some(new_progress) = state::aggregate_progress(&snapshots) else {
            // 没有（未删除的）子任务：这一级由手动进度维护，继续向上没有意义
            break;
        };
        let old_progress = scalar_progress(ex, parent_id).await?;
        let all_done = state::ready_to_close(&snapshots);
        let mode = state::aggregation_label(&snapshots);
        if old_progress != new_progress {
            set_progress(ex, parent_id, new_progress, operator).await?;
            if PROGRESS_AUTO_COMMENT {
                insert_comment(
                    ex,
                    parent_id,
                    operator,
                    &format!(
                        "进度自动汇总：{old_progress}% → {new_progress}%（{} 个子任务，{mode}）",
                        snapshots.len()
                    ),
                    COMMENT_TYPE_PROGRESS_AUTO,
                    operator,
                )
                .await?;
            }
            tracing::info!(
                task_id = %parent_id,
                old = i64::from(old_progress),
                new = i64::from(new_progress),
                children = snapshots.len() as i64,
                "父任务进度已重算"
            );
        }
        results.push(ProgressRecalc {
            task_id: parent_id,
            old: old_progress,
            new: new_progress,
            children: snapshots.len(),
            all_done,
            mode,
        });
        cursor = parent_of(ex, parent_id).await?;
    }
    Ok(results)
}

/// 评论类型：状态变更（时间线箭头）。
pub const COMMENT_TYPE_STATUS_CHANGE: &str = "STATUS_CHANGE";

/// 评论类型：进度自动汇总。
pub const COMMENT_TYPE_PROGRESS_AUTO: &str = "PROGRESS_AUTO";

/// 任务的父任务 ID（`None` = 顶层任务 / 不存在）。
async fn parent_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT parent_id FROM biz_task WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("parent_id"),
        None => Ok(None),
    }
}

/// 任务的当前进度（重算前读旧值，只用于「变了才写」与评论）。
async fn scalar_progress(ex: &impl SqlExecutor, id: Uuid) -> DbResult<i16> {
    let rows = ex
        .query_sql(
            "SELECT progress FROM biz_task WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("progress"),
        // 行不存在时按 0 处理：随后 `set_progress` 的 WHERE 会命中 0 行，
        // 不会凭空造出一条进度。
        None => Ok(0),
    }
}

/// 用户是否开启站内信（`notify.inbox` 偏好，缺省 `true`）。
///
/// 通知分级（修正 8）：**强制**通知忽略本开关；普通通知尊重它。
///
/// # 为什么读偏好失败时返回 `true`（fail-open）
///
/// 这是「多收一条站内信」和「该知道的事没收到」之间的取舍：偏好表在、但读失败
/// （缓存抖动 / 连接打满）时，静默丢掉通知的代价远大于多一条噪音。真正的
/// fail-closed 场景（越权读数据）不在这条路径上。
///
/// # 异常情况
///
/// 不返回错误（数据库异常降级为 `true` 并记 WARN）。
pub async fn inbox_enabled(user_id: Uuid) -> bool {
    match crate::system::preference::model::load_values(db(), &user_id).await {
        Ok(values) => values
            .get("notify.inbox")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        Err(error) => {
            tracing::warn!(
                user_id = %user_id,
                error = %error,
                "读取 notify.inbox 偏好失败，按开启处理"
            );
            true
        }
    }
}

// ==================== 单元测试（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("合法日期")
    }

    #[test]
    fn visible_tail_uses_member_id_array_only() {
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
        // D6：参与关系只允许用一个 id 数组表达
        assert!(
            VISIBLE_OR_TAIL.contains("t.id = ANY(?::text::uuid[])"),
            "参与关系必须用 id 数组（D6），不能改回 EXISTS / 多 OR"
        );
        assert!(
            !VISIBLE_OR_TAIL.contains("EXISTS"),
            "EXISTS 子查询会让优化器放弃 BitmapOr、退化成 Seq Scan（实测 539 ms）"
        );
        for banned in ["t.owner_id", "t.creator_id", "t.assigner_id"] {
            assert!(
                !VISIBLE_OR_TAIL.contains(banned),
                "{banned} 的 OR 分支必须并入 my_member_task_ids 的 id 数组，不得留在谓词里"
            );
        }
        // 占位符数量（不含 {FILTER} 内的）必须等于 visible_params 的长度
        assert_eq!(VISIBLE_OR_TAIL.matches('?').count(), 1);
    }

    #[test]
    fn visible_params_are_one_uuid_array() {
        let ids = [
            Uuid::parse_str("018f0000-0000-7000-8000-000000000001").expect("合法 uuid"),
            Uuid::parse_str("018f0000-0000-7000-8000-000000000002").expect("合法 uuid"),
        ];
        let scope_ids = MyScopeIds {
            ids: ids.to_vec(),
            truncated: false,
        };
        let params = visible_params(&scope_ids);
        assert_eq!(
            params.len(),
            VISIBLE_OR_TAIL.matches('?').count(),
            "参数个数必须与 SQL 里 `?` 的文本顺序一一对应"
        );
        assert_eq!(
            params[0],
            Value::Text(
                "{018f0000-0000-7000-8000-000000000001,018f0000-0000-7000-8000-000000000002}"
                    .to_string()
            ),
            "uuid[] 必须编码成 PG 文本数组字面量 {{uuid,uuid}}"
        );
    }

    #[test]
    fn truncated_scope_degrades_to_empty_array() {
        // D7 降级：超过 2000 个 id → 传空数组（ANY('{}') 恒假 = 只按范围），
        // 且参数个数不变（降级不会让绑定错位）。
        let oversized = MyScopeIds {
            ids: vec![Uuid::nil()],
            truncated: true,
        };
        assert_eq!(
            visible_params(&oversized),
            vec![Value::Text("{}".to_string())]
        );
        // 空集合本身的编码就是 `{}`（没有任何参与关系时同样安全）
        assert_eq!(
            visible_params(&MyScopeIds::default()),
            vec![Value::Text("{}".to_string())]
        );
        assert_eq!(MEMBER_SCOPE_LIMIT, 2000);
    }

    #[test]
    fn member_limit_truncation_rule() {
        // 纯逻辑镜像 my_member_task_ids 的截断判定（2000 允许、2001 降级）
        let check = |n: usize| n as i64 > MEMBER_SCOPE_LIMIT;
        assert!(!check(0));
        assert!(!check(2000));
        assert!(check(2001));
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

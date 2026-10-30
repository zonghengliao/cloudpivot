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

use nik::prelude::chrono::{Datelike, Duration, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;
use crate::business::task::dep;
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

// ==================== 「新任务」标记（F2 · A 口径） ====================

/// 「新任务」的时间窗（天）。
///
/// # 口径（老大 2026-10-30 选 A：与菜单徽标**同一口径**）
///
/// `status = 'TODO'` ∧ `owner_id = 我` ∧ `created_at >= now() - 7 天`。
///
/// 必须与 `system::badge::model::TASK_TODO_WINDOW_DAYS`
/// （`GET /system/badge/counts` 的 `task_todo` 桶）**同值**，否则会出现
/// 「菜单徽标 ●2、列表里一个 NEW 都没有」——用户只会认为红点是假的。
///
/// 为什么不直接复用 badge 的那个常量：`system::badge` 已经单向依赖 `business::*`
/// （它要复用公告/通知的可见性谓词），反过来让 `business::task` 引用它就成了模块环。
/// 两个常量由 `tests::new_window_matches_badge` 钉死，任一边改动立刻红。
pub const NEW_TASK_WINDOW_DAYS: i32 = 7;

/// 是否「新任务」（A 口径）：`TODO` ∧ 负责人是我 ∧ 7 天内创建。
///
/// # 为什么不写成 `CARD_COLUMNS` 里的 SQL 派生列（卡面原方案，实测否决）
///
/// 1. **参数顺序不可能对上**。`CARD_COLUMNS` 在 5 条查询里共用，而
///    [`scope::paginate`] 把数据范围片段参数**恒前置**（架构 §2.6 契约）。
///    选择列表出现在所有 `WHERE` 占位符之前，放一个 `?` 进去，它的文本位置
///    与任何调用方的参数表都不一致 —— `paginate` 会**静默绑错值**
///    （这正是 §2.6 明文禁止的形态）。
/// 2. **不需要**。口径的三个输入（`status` / `owner_id` / `created_at`）本来就
///    在 [`TaskCardRow`] 里，派生不产生任何新查询、不改 SQL、不改 `EXPLAIN`
///    （性能不可能退化）。
///
/// 口径只在这里定义一份，[`page_visible`]（列表 + 看板）、[`my_tasks`]（只看我的）
/// 与 [`gantt_tasks`]（甘特）三处调用。
pub fn is_new_task(
    status: &str,
    owner_id: Uuid,
    created_at: chrono::DateTime<Utc>,
    viewer: Uuid,
) -> bool {
    is_new_task_at(status, owner_id, created_at, viewer, Utc::now())
}

/// [`is_new_task`] 的可注入时钟版本：单测钉边界时用固定 `now`，避免跨秒抖动。
pub fn is_new_task_at(
    status: &str,
    owner_id: Uuid,
    created_at: chrono::DateTime<Utc>,
    viewer: Uuid,
    now: chrono::DateTime<Utc>,
) -> bool {
    status == TaskStatus::Todo.code()
        && owner_id == viewer
        && created_at >= now - Duration::days(i64::from(NEW_TASK_WINDOW_DAYS))
}

/// 给一批任务卡打「新任务」标记（列表 / 看板 / 我的任务共用）。
///
/// 时钟只取一次：同一页里的任务不会因为跨越 7 天边界的那一秒而口径不一。
fn mark_is_new(rows: Vec<TaskCardRow>, viewer: Uuid) -> Vec<TaskCardRow> {
    let now = Utc::now();
    rows.into_iter()
        .map(|mut row| {
            row.is_new = is_new_task_at(&row.status, row.owner_id, row.created_at, viewer, now);
            row
        })
        .collect()
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
    /// 已完成的子任务数（E6a：列表 / 详情页「子任务 3/1」用）。
    pub child_done_count: i64,
    /// 父任务（E6b：列表树形表格据此判定「顶层 / 子行」，`None` = 顶层）。
    pub parent_id: Option<Uuid>,
    /// 所属项目（T7c：可空，`None` = 未归入任何项目）。
    pub project_id: Option<Uuid>,
    /// 项目编码（空串 = 无项目，或项目已被软删）。
    pub project_code: String,
    /// 项目名称（空串 = 无项目，或项目已被软删）。
    pub project_name: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<Utc>,
    /// 是否为「新任务」（F2，A 口径：`TODO` ∧ 负责人是我 ∧ 7 天内创建）。
    ///
    /// **不是数据库列**：由 [`is_new_task`] 在取数后派生，只有列表 / 看板 /
    /// 「我的任务」会填它；详情与逾期扫描等路径保持 `false`（卡面要求详情不做「新」标记）。
    pub is_new: bool,
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
            child_done_count: row.get_typed("child_done_count")?,
            parent_id: row.get_typed("parent_id")?,
            project_id: row.get_typed("project_id")?,
            project_code: row.get_typed("project_code")?,
            project_name: row.get_typed("project_name")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            is_new: false,
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
    /// 执行模式：`HUMAN` / `AI`（T5b）。
    pub exec_mode: String,
    /// Hermes 看板（可空）。
    pub hermes_board: Option<String>,
    /// Hermes 任务 id（可空）。
    pub hermes_task_id: Option<String>,
    /// Hermes 原始状态（可空）。
    pub hermes_status: Option<String>,
    /// AI 执行摘要（可空）。
    pub ai_summary: Option<String>,
    /// AI 结构化结果（jsonb 原文本，可空）；handler 侧按需 parse 成 JSON。
    pub ai_result: Option<String>,
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
    /// 来源：`HUMAN` / `AI`（T5b）。
    pub source: String,
    /// 产出该附件的 AI profile（仅 `source = 'AI'` 时有值）。
    pub ai_profile: Option<String>,
}

// ==================== SQL 片段 ====================

/// 任务卡列清单（所有列表查询共用，避免两处 select 列不一致导致的 `get_typed` 失败）。
const CARD_COLUMNS: &str = "\
    t.id, t.parent_id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review, \
    t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
    t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at, \
    (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count, \
    (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL \
        AND c.status = 'DONE') AS child_done_count, \
    t.project_id, COALESCE(pj.project_no, '') AS project_code, \
    COALESCE(pj.project_name, '') AS project_name";

/// 任务卡 FROM 子句（负责人姓名来自 `sys_user`：SELECT 里没有部门类过滤，
/// 数据权限片段只锚定 `t`，见 §2.6 规则 1）。
///
/// `biz_project` 的 JOIN 是 **T7c 追加的展示字段**（任务列表的「项目」列 / 详情抽屉的
/// 项目名）：`LEFT JOIN` + `deleted_at IS NULL` 保证项目被删后任务仍在列表里（`project_id`
/// 仍指向软删行 —— 由 T7c 的删除流程置空，这里再兜一层，历史脏数据也不会让任务消失）。
const CARD_FROM: &str = " FROM biz_task t \
     LEFT JOIN sys_user u ON u.id = t.owner_id \
     LEFT JOIN biz_project pj ON pj.id = t.project_id AND pj.deleted_at IS NULL ";

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
    if let Some(project_id) = filter.project_id {
        // T7c：项目分组筛选。**只做过滤，不做可见性** —— 可见性由 VISIBLE_OR_TAIL 决定，
        // 因此「按项目筛选」永远不会绕过 T1（项目不是权限边界）。
        sql.push_str(" AND t.project_id = ?::uuid");
        params.push(Value::from(&project_id));
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
    /// 所属项目（T7c：项目分组筛选；项目下任务列表也用它，**但可见性仍走 VISIBLE_OR_TAIL**）。
    pub project_id: Option<Uuid>,
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

/// 附件显示名（人可读）：优先 `sys_file.original_name`，否则取 `file_name` 的 basename。
///
/// # 为什么不能直接把 `sys_file.file_name` 下发给前端
///
/// `sys_file.file_name` 是**存储名**：LOCAL / 对象存储后端下它是带日期的相对路径
/// （如 `2026/10/29/01a1874d-….md`）。附件列表把它当 `file_name` 下发后，前端会
/// 拿它当 `<a download>` 的保存名 —— 路径分隔符不该出现在保存名里（Chromium 会把
/// `/` 替换成 `_`，其它浏览器实现可能直接丢弃这次下载），用户看到的名字也不是
/// 自己认识的那个。所以显示名统一在这里收口：`original_name` 缺失 / 空串时退回
/// `file_name` 的 basename（去掉 `2026/10/29/` 这类目录前缀）。
///
/// # 参数
///
/// - `original`：`sys_file.original_name`（可空、可能是空串）。
/// - `stored`：`sys_file.file_name`（存储名，可能带 `/` 或 `\`）。
///
/// # 返回值
///
/// 两侧都空时返回空串（前端有 `|| 'attachment'` 兜底），绝不返回带分隔符的名字。
pub(crate) fn display_file_name(original: Option<&str>, stored: &str) -> String {
    if let Some(name) = original.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    stored
        .trim()
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or_default()
        .to_string()
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
    ex.exec_sql(sql, params).await
}

/// 软删除任务 (+ 清理它的依赖行)。
///
/// # 为什么这里还要删依赖（软删不触发 CASCADE）
///
/// `biz_task_dependency` 的两个外键都写了 `ON DELETE CASCADE`，但任务是**软删**
/// （`deleted_at`），CASCADE 只在**物理**删除时触发 —— 所以软删会把依赖行留成孤儿
/// （指向一条已删任务）。孤儿不影响读路径（所有依赖查询都 `JOIN biz_task … AND
/// deleted_at IS NULL`），但它会永久累积，且让启动自检（[`crate::shared::startup_guard`]）
/// 报警。
///
/// 依赖关系本身**没有历史价值**（谁依赖谁随时可按业务重建），且该表没有 `deleted_at`
/// 列，所以这里直接 `DELETE`，并与任务软删**同一事务**（要么都成、要么都回滚）。
///
/// # 返回值
///
/// 任务受影响行数（0 = 任务不存在 / 已软删；此时依赖清理照做，重复删除因此幂等）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]（事务由调用方回滚）。
pub async fn soft_delete(ex: &Transaction, id: Uuid, operator: Uuid) -> DbResult<u64> {
    let affected = ex
        .execute_with_params(
            "UPDATE biz_task SET deleted_at = now(), updated_at = now(), update_by = ?::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&operator), Value::from(&id)],
        )
        .await?;
    purge_dependencies(ex, id).await?;
    Ok(affected)
}

/// 清掉与某个任务相关的**全部**依赖行（双向：我作为后继 + 我作为前置）。
///
/// # 为什么双向
///
/// 只删一侧会留下另一半孤儿：A→B 在库里是一行 `(task_id=A, depends_on_id=B)`，
/// 删 A 要看 `task_id`、删 B 要看 `depends_on_id`。
///
/// # 返回值
///
/// 被删掉的依赖行数（0 = 该任务没有依赖，属正常）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn purge_dependencies(ex: &impl SqlExecutor, id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM biz_task_dependency WHERE task_id = ?::uuid OR depends_on_id = ?::uuid"
            .to_string(),
        vec![Value::from(&id), Value::from(&id)],
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
        // （nik-db 的参数编码按类型推断，NULL 会被推断成 text），所以清空直接写 `NULL`。
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
    ex.exec_sql(sql, params).await
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

/// 写一条任务评论（状态变更 / 进度自动汇总 / AI 报告都走它）。
///
/// `comment_type` 是 `V0108` 新增的列：`PLAIN` / `STATUS_CHANGE` / `PROGRESS_AUTO`
/// （`V0109` 加 `ASSIGN`、`V0112` 加 `AI_REPORT` / `AI_PROGRESS`）。
/// 状态流转的评论**必须**用 `STATUS_CHANGE`（前端详情页的时间线按它画箭头），
/// 否则「谁把任务改成了什么」会和普通闲聊混在一起。
///
/// # 为什么吃 `&impl SqlExecutor` 而不是 `&Transaction`
///
/// T5b 的轮询回传里，评论有三处是「独立写一条」的（失败说明 / 阻塞说明 / 任务丢失），
/// 它们不需要与状态更新同事务；而状态流转那几处仍然要进事务。`SqlExecutor` 同时覆盖
/// `&Db` 与 `&Transaction`（与 `recalc_ancestors` / `add_member` 同一口径），
/// 于是「进不进事务」由调用方决定，写评论的实现只有一份。
///
/// # 返回值
///
/// 新评论 ID（UUIDv7，便于按时间排序）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn insert_comment(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    user_id: Uuid,
    content: &str,
    comment_type: &str,
    operator: Uuid,
) -> DbResult<Uuid> {
    insert_comment_full(
        ex,
        task_id,
        user_id,
        content,
        comment_type,
        CommentExtra::default(),
        operator,
    )
    .await
}

/// 完整版写评论的附加项（回复目标 + `@` 名单）。
#[derive(Debug, Clone, Copy, Default)]
pub struct CommentExtra<'a> {
    /// 回复的目标评论（`None` = 顶层评论）。
    pub parent_id: Option<Uuid>,
    /// 被 @ 的用户 ID 列表。
    pub mentions: &'a [Uuid],
}

/// 完整版写评论：额外支持「回复某条评论」与 `@` 名单（T5b 的评论接口用）。
///
/// # 参数
///
/// - `extra.parent_id`：回复的目标评论。数据库外键会保证它存在，
///   但**不校验它属于同一个任务** —— 跨任务回复在 UI 上不会出现（详情页只按
///   `task_id` 取评论），因此这里不做额外查询（少一次往返）。
/// - `extra.mentions`：被 @ 的用户 ID（写进 `mentions` jsonb；空数组写 `'[]'`）。
///
/// 附加项收成 [`CommentExtra`] 而不是两个平铺参数：`clippy` 的
/// `too_many_arguments` 阈值是 7，平铺会直接编译失败（`-D warnings`）。
pub async fn insert_comment_full(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    user_id: Uuid,
    content: &str,
    comment_type: &str,
    extra: CommentExtra<'_>,
    operator: Uuid,
) -> DbResult<Uuid> {
    let id = Uuid::now_v7();
    let mentions_json = serde_json::Value::Array(
        extra
            .mentions
            .iter()
            .map(|user_id| serde_json::Value::String(user_id.to_string()))
            .collect(),
    );
    ex.exec_sql(
        "INSERT INTO biz_task_comment \
             (id, task_id, user_id, content, mentions, parent_comment_id, comment_type, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?, ?::jsonb, ?::uuid, ?, now(), now(), ?::uuid, ?::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&user_id),
            Value::Text(content.to_string()),
            Value::Json(mentions_json.to_string()),
            match extra.parent_id {
                Some(parent) => Value::from(&parent),
                None => Value::Null,
            },
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

/// 评论类型：普通评论（人类手写 / 系统留痕）。`V0108` 的默认值。
pub const COMMENT_TYPE_PLAIN: &str = "PLAIN";

/// 评论类型：AI 执行报告（T5b 三层回传的第一层）。
///
/// 与 `PLAIN` 分开的理由：前端详情页把 AI 报告渲染成**折叠卡片**（长文本 + 交付清单），
/// 和人类闲聊放在同一时间线样式里会把验收人淹掉。
pub const COMMENT_TYPE_AI_REPORT: &str = "AI_REPORT";

/// 评论类型：AI 进度回写（T5b 的 `PATCH /ai-progress`）。
pub const COMMENT_TYPE_AI_PROGRESS: &str = "AI_PROGRESS";

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

// 说明（T3）：T2 在这里放过一个 `inbox_enabled(user_id)`（Rust 侧逐个判断偏好），
// 已**删除**。理由：T3 加分派 / 协作人通知后，通知偏好会变成「任务通知看这一份、
// 公告 / 系统通知看另一份」的双实现，迟早不一致（实测过的教训：两处实现 = 两个
// 真相源）。现在偏好过滤**只有一处**：`business/notice` 的
// `fanout::respecting_preference`，由 `notice::dispatch(draft.force = false)` 触发。
// 要改偏好口径，改 `notice`；**不要在本文件加回本地过滤**。

// ==================== 分派 / 转派 / 协作人（T3） ====================

/// 分派 / 转派评论的类型（`biz_task_comment.comment_type`）。
///
/// 与 `STATUS_CHANGE` / `PROGRESS_AUTO` 并列：前端时间线按类型画不同的行（分派是
/// 「箭头 + 两个人的头像」，状态变更是「状态徽章前后对比」）。
pub const COMMENT_TYPE_ASSIGN: &str = "ASSIGN";

/// 转派写库的口径（**唯一**的负责人变更入口）。
///
/// `WHERE owner_id = expected_owner` 是**乐观并发**：两个人同时转派同一条任务时，
/// 后到的那次命中 0 行 → handler 回 409（`63018`）让调用者刷新。
/// 不加这个条件的话，「原负责人自动变协作人」会记在读到旧值的那个错觉上
/// （A 把任务从甲转给乙的同时 B 转给丙，最终丙是负责人、乙却是协作人）。
pub const ASSIGN_OWNER_SQL: &str = "UPDATE biz_task \
     SET owner_id = ?::uuid, dept_id = ?::uuid, assigner_id = ?::uuid, \
         source = ?, updated_at = now(), update_by = ?::uuid \
   WHERE id = ?::uuid AND owner_id = ?::uuid AND deleted_at IS NULL";

/// 协作人批量入库的 SQL（**集合式**：一条 `INSERT ... SELECT`）。
///
/// 铁律（notice 模块 §4.2 D8 同款）：一次加 N 个人只许一条语句，不许在 Rust 侧
/// 循环单条 insert（50 人 = 50 次往返，且失败时留下半批数据）。
/// `ON CONFLICT ... DO NOTHING` + `RETURNING user_id` 同时解决两件事：
/// 幂等（重复添加不报错）与「本次真正新增了谁」（回执里的 `added`）。
pub const MEMBERS_INSERT_SQL: &str = "INSERT INTO biz_task_member (task_id, user_id, role, created_at, create_by) \
     SELECT ?::uuid, m.user_id, ?, now(), ?::uuid \
       FROM (SELECT DISTINCT unnest(?::text::uuid[]) AS user_id) m \
     ON CONFLICT (task_id, user_id) DO NOTHING \
     RETURNING user_id";

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

/// 负责人变更的入参（分派 / 转派共用；`dept_id` 是负责人部门的新快照）。
#[derive(Debug, Clone)]
pub struct OwnerChange {
    /// 任务。
    pub task_id: Uuid,
    /// 新负责人。
    pub owner_id: Uuid,
    /// 新负责人的部门（部门看板按它过滤，所以必须跟着换）。
    pub dept_id: Option<Uuid>,
    /// 落库后的分派人。
    pub assigner_id: Option<Uuid>,
    /// 落库后的来源（转派 = `TRANSFERRED`）。
    pub source: String,
    /// 操作人（写 `update_by`）。
    pub operator: Uuid,
    /// 读到的旧负责人（乐观并发条件）。
    pub expected_owner: Uuid,
}

/// 改负责人（分派 / 转派，见 [`ASSIGN_OWNER_SQL`]）。
///
/// # 返回值
///
/// 受影响行数：`0` = 任务已软删 **或** 负责人已被别人改掉（handler 回 409）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn assign_owner(ex: &Transaction, change: &OwnerChange) -> DbResult<u64> {
    ex.execute_with_params(
        ASSIGN_OWNER_SQL,
        vec![
            Value::from(&change.owner_id),
            Value::from(change.dept_id.as_ref()),
            Value::from(change.assigner_id.as_ref()),
            Value::Text(change.source.clone()),
            Value::from(&change.operator),
            Value::from(&change.task_id),
            Value::from(&change.expected_owner),
        ],
    )
    .await
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

/// 批量加协作人（集合式，见 [`MEMBERS_INSERT_SQL`]）。
///
/// # 返回值
///
/// **本次真正新增**的 user_id（幂等命中的不在其中，用于回执里的 `added`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn insert_members(
    ex: &Transaction,
    task_id: Uuid,
    user_ids: &[Uuid],
    operator: Uuid,
) -> DbResult<Vec<Uuid>> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = ex
        .query_sql(
            MEMBERS_INSERT_SQL.to_string(),
            vec![
                Value::from(&task_id),
                Value::Text("COLLABORATOR".to_string()),
                Value::from(&operator),
                ids_array_param(user_ids),
            ],
        )
        .await?;
    let mut ids = Vec::with_capacity(rows.len());
    for row in &rows {
        ids.push(row.get_typed("user_id")?);
    }
    Ok(ids)
}

/// 移除一个协作人。
///
/// # 返回值
///
/// 受影响行数（`0` = 该用户本来就不是协作人 → handler 回 404）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn remove_member(ex: &Transaction, task_id: Uuid, user_id: Uuid) -> DbResult<u64> {
    ex.execute_with_params(
        "DELETE FROM biz_task_member WHERE task_id = ?::uuid AND user_id = ?::uuid",
        vec![Value::from(&task_id), Value::from(&user_id)],
    )
    .await
}

// ==================== 依赖 / 甘特（T4） ====================

/// 依赖列表项（**前置组 / 后继组共用**）。
///
/// `peer_id` 是「对端任务」的 id：
/// - 前置组（我依赖谁）→ `peer_id = depends_on_id`；
/// - 后继组（谁依赖我）→ `peer_id = task_id`。
///
/// 前端拿它直接调 `DELETE /{id}/dependencies/{peer_id}`，不需要区分方向
/// —— 删除本来就是「解除这两个任务之间的依赖」，方向由服务端按 pair 匹配。
#[derive(Debug, Clone)]
pub struct DependencyRow {
    /// 对端任务 id。
    pub peer_id: Uuid,
    /// 对端任务编号。
    pub task_no: String,
    /// 对端任务标题。
    pub title: String,
    /// 对端任务状态。
    pub status: String,
    /// 对端任务优先级。
    pub priority: String,
    /// 对端任务进度。
    pub progress: i16,
    /// 对端任务负责人。
    pub owner_id: Uuid,
    /// 负责人姓名。
    pub owner_name: String,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日。
    pub due_date: Option<NaiveDate>,
    /// 依赖类型（本版恒 `FS`）。
    pub dep_type: String,
    /// 滞后天数（`FS+lag`：前置完成后还要等 N 天）。本版只存不算排程。
    pub lag_days: i32,
    /// 依赖建立时间。
    pub created_at: chrono::DateTime<Utc>,
}

impl DependencyRow {
    /// 由查询行构造。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            peer_id: row.get_typed("peer_id")?,
            task_no: row.get_typed("task_no")?,
            title: row.get_typed("title")?,
            status: row.get_typed("status")?,
            priority: row.get_typed("priority")?,
            progress: row.get_typed("progress")?,
            owner_id: row.get_typed("owner_id")?,
            owner_name: row.get_typed("owner_name")?,
            start_date: row.get_typed("start_date")?,
            due_date: row.get_typed("due_date")?,
            dep_type: row.get_typed("dep_type")?,
            lag_days: row.get_typed("lag_days")?,
            created_at: row.get_typed("created_at")?,
        })
    }
}

/// 依赖列表的列清单：对端任务 `p` + 依赖行 `d`（两处查询共用，避免列漂移）。
const DEP_PEER_COLUMNS: &str = "\
    p.id AS peer_id, p.task_no, p.title, p.status, p.priority, p.progress, p.owner_id, \
    COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
    p.start_date, p.due_date, d.dep_type, d.lag_days, d.created_at";

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
            "SELECT task_id, depends_on_id FROM biz_task_dependency WHERE dep_type = 'FS'"
                .to_string(),
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
pub async fn find_dependency(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    depends_on_id: Uuid,
) -> DbResult<Option<i32>> {
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
pub async fn delete_dependency(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    peer_id: Uuid,
) -> DbResult<u64> {
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

/// 两条任务的结构关系（**跨父子 / 跨项目校验**的输入）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskRelation {
    /// 任务 A 的父任务。
    pub parent_a: Option<Uuid>,
    /// 任务 B 的父任务。
    pub parent_b: Option<Uuid>,
    /// 任务 A 的项目。
    pub project_a: Option<Uuid>,
    /// 任务 B 的项目。
    pub project_b: Option<Uuid>,
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
    if found == 2 {
        Ok(Some(relation))
    } else {
        Ok(None)
    }
}

/// 一批任务的编号 / 标题（环路径文案用）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn dependency_labels(
    ex: &impl SqlExecutor,
    ids: &[Uuid],
) -> DbResult<Vec<dep::DepLabel>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let array = Value::Text(format!(
        "{{{}}}",
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
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

/// 甘特图任务行。
#[derive(Debug, Clone)]
pub struct GanttTaskRow {
    /// 主键。
    pub id: Uuid,
    /// 编号。
    pub task_no: String,
    /// 标题。
    pub title: String,
    /// 状态。
    pub status: String,
    /// 优先级。
    pub priority: String,
    /// 进度。
    pub progress: i16,
    /// 负责人。
    pub owner_id: Uuid,
    /// 负责人姓名。
    pub owner_name: String,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日。
    pub due_date: Option<NaiveDate>,
    /// 父任务（前端按它折叠子任务）。
    pub parent_id: Option<Uuid>,
    /// 是否被未完成前置阻塞（一条 SQL 批量算，避免 N+1）。
    pub blocked: bool,
    /// 是否为「新任务」（F2，A 口径，与列表 `TaskCardRow.is_new` 同一判据）。
    pub is_new: bool,
}

/// 甘特查询的筛选维度（`board_id` 不在内：本版没有看板列，接口层直接 400）。
#[derive(Debug, Clone)]
pub struct GanttFilter {
    /// 只看某人负责的（`assignee_id`）。
    pub owner_id: Option<Uuid>,
    /// 只看某项目的。
    pub project_id: Option<Uuid>,
    /// 只看我负责的（`scope=my`）。
    pub mine: Option<Uuid>,
}

/// 甘特查询结果（任务 + 两个降级标记）。
#[derive(Debug, Clone, Default)]
pub struct GanttResult {
    /// 任务行（已按窗口过滤 + 排序 + 截断）。
    pub tasks: Vec<GanttTaskRow>,
    /// 任务数超过 [`dep::MAX_GANTT_TASKS`]，本次已截断。
    pub truncated: bool,
    /// 「我参与」id 超 [`MEMBER_SCOPE_LIMIT`] → 可见性已降级为「只按范围」（D7）。
    pub member_scope_truncated: bool,
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
    sql.push_str(
        " ORDER BY COALESCE(t.start_date, t.due_date) ASC, t.due_date ASC NULLS LAST, t.id ASC",
    );
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
async fn blocked_task_ids(ids: &[Uuid]) -> DbResult<Vec<Uuid>> {
    let array = Value::Text(format!(
        "{{{}}}",
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
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
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
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

/// 一条「需要与 Hermes 同步」的任务快照。
///
/// 只取轮询要用的列：Hermes 三件套 + 云枢侧的状态 / 相关人（通知要用人）。
#[derive(Debug, Clone)]
pub struct SyncTargetRow {
    /// 云枢任务 id。
    pub id: Uuid,
    /// Hermes 看板。
    pub board: String,
    /// Hermes 任务 id。
    pub hermes_task_id: String,
    /// 云枢当前状态。
    pub status: String,
    /// 上一次同步到的 Hermes 状态（用于去重：状态没变就不再写评论 / 不再通知）。
    pub hermes_status: Option<String>,
    /// 负责人（AI 账号）。
    pub owner_id: Uuid,
    /// 创建人（验收人之一）。
    pub creator_id: Uuid,
    /// 分派人（验收人之一，可空）。
    pub assigner_id: Option<Uuid>,
    /// 任务标题。
    pub title: String,
    /// 任务编号。
    pub task_no: String,
}

/// 登记 / 覆盖任务级令牌（一任务一令牌：重复派活用 `ON CONFLICT DO UPDATE` 覆盖）。
///
/// # 为什么重派要覆盖而不是新增
///
/// 主键是 `task_id`（见 V0112 的设计说明）。重派时覆盖 hash 并**把 `used_count` 归零**，
/// 于是旧令牌立刻失效 —— 它的明文还躺在上一张 kanban body 里，但它再也换不出任何东西。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn upsert_ai_token(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    token_hash: &str,
    expires_at: chrono::DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        "INSERT INTO biz_task_ai_token (task_id, token_hash, expires_at, used_count, created_at) \
         VALUES (?::uuid, ?, ?::timestamptz, 0, now()) \
         ON CONFLICT (task_id) DO UPDATE \
            SET token_hash = EXCLUDED.token_hash, \
                expires_at = EXCLUDED.expires_at, \
                used_count = 0, \
                created_at = now()"
            .to_string(),
        vec![
            Value::from(&task_id),
            Value::Text(token_hash.to_string()),
            TypeMapper::to_value(&expires_at),
        ],
    )
    .await
}

/// 撤销任务令牌（派活失败 / 任务终结时调用）。
pub async fn delete_ai_token(ex: &impl SqlExecutor, task_id: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM biz_task_ai_token WHERE task_id = ?::uuid".to_string(),
        vec![Value::from(&task_id)],
    )
    .await
}

/// 派活成功：写 Hermes 三件套 + `exec_mode='AI'`。
///
/// **只由派活接口在 CLI 成功之后调用**（派活失败绝不允许写这一笔，否则云枢会显示
/// 「已派出」而 Hermes 侧什么都没有 —— 卡面红线）。
pub async fn mark_dispatched(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    board: &str,
    hermes_task_id: &str,
    hermes_status: &str,
    operator: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task \
            SET exec_mode = 'AI', hermes_board = ?, hermes_task_id = ?, hermes_status = ?, \
                hermes_synced_at = now(), updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(board.to_string()),
            Value::Text(hermes_task_id.to_string()),
            Value::Text(hermes_status.to_string()),
            Value::from(&operator),
            Value::from(&task_id),
        ],
    )
    .await
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
// SCOPE-EXEMPT: 轮询目标是**系统级扫描**（所有 AI 任务都必须同步），按 exec_mode /
// hermes_task_id 过滤，没有「当前用户」语义；注入数据范围会让别人的 AI 任务永远停在 DOING。
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

/// 记录 Hermes 原始状态（**不改云枢状态**），用于去重与排障。
pub async fn touch_hermes(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    hermes_status: &str,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task SET hermes_status = ?, hermes_synced_at = now() \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(hermes_status.to_string()),
            Value::from(&task_id),
        ],
    )
    .await
}

/// 同步推进云枢状态（`running → DOING` 等），同时记 Hermes 原始状态。
///
/// # 参数
///
/// - `cloud_status`：要推进到的云枢状态；`None` = 只同步时间戳与 Hermes 状态。
/// - `progress`：同时写进度（`running` 时给 10%），`None` = 不动进度。
pub async fn sync_progress_of_ai(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    hermes_status: &str,
    cloud_status: Option<&str>,
    progress: Option<i16>,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets = vec![
        "hermes_status = ?".to_string(),
        "hermes_synced_at = now()".to_string(),
        "updated_at = now()".to_string(),
    ];
    let mut params = vec![Value::Text(hermes_status.to_string())];
    if let Some(status) = cloud_status {
        sets.push("status = ?".to_string());
        params.push(Value::Text(status.to_string()));
    }
    if let Some(value) = progress {
        sets.push("progress = ?::smallint".to_string());
        params.push(Value::I16(value));
    }
    sets.push("update_by = ?::uuid".to_string());
    params.push(Value::from(&operator));
    params.push(Value::from(&task_id));
    let sql = format!(
        "UPDATE biz_task SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    ex.exec_sql(sql, params).await
}

/// 记录 AI 的活跃信号：`TODO → DOING`（其余状态不动）。
///
/// # 为什么进度回写要顺手推进状态
///
/// `PATCH /ai-progress` 意味着 AI **已经在干活了**。若云枢还显示「待开始」，
/// 看板上看不到「AI 执行中」（设计文档 §三 running 那一行要求 DOING）。
/// 这里只做**单向**推进：已经是 `DOING` / `REVIEW` 的一律不动（不后退）。
///
/// 注意**不改** `hermes_status`：那个列是「最近一次从 Hermes 读到的原始状态」，
/// 让进度回写去改写它，会让轮询的去重判断（`hermes_status` 有没有变）失真。
pub async fn mark_ai_active(ex: &impl SqlExecutor, task_id: Uuid, operator: Uuid) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task \
            SET status = CASE WHEN status = 'TODO' THEN 'DOING' ELSE status END, \
                hermes_synced_at = now(), \
                updated_at = now(), \
                update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![Value::from(&operator), Value::from(&task_id)],
    )
    .await
}

/// AI 完成回传：写摘要 / 结构化结果，并把状态推进到 **REVIEW**（老大 2A 拍板）。
///
/// # 为什么「即使 `need_review = false` 也走 REVIEW」
///
/// AI 的活必须有人点头。`need_review = FALSE` 的语义是「人类自己干的活不用别人验收」，
/// 它不该变成「AI 可以直接把任务结掉」的旁路。所以这里**不读** `need_review`。
///
/// # 参数
///
/// - `summary`：执行摘要（`latest_summary`），空则把 `ai_summary` 置 NULL；
/// - `result`：`runs[..].metadata` 原样（jsonb）；`None` 时保留原值（不覆盖已有结果）。
pub async fn apply_ai_done(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    summary: Option<&str>,
    result: Option<&serde_json::Value>,
    operator: Uuid,
) -> DbResult<u64> {
    let mut sets = vec![
        "status = 'REVIEW'".to_string(),
        "progress = 100".to_string(),
        "hermes_status = 'done'".to_string(),
        "hermes_synced_at = now()".to_string(),
        "ai_summary = ?".to_string(),
        "updated_at = now()".to_string(),
    ];
    let mut params = vec![match summary {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }];
    if let Some(json) = result {
        sets.push("ai_result = ?::jsonb".to_string());
        // JSONB 列必须用 Value::Json(JSON 文本)：绑 Value::Text / Value::from 会报
        // `error serializing parameter`（本仓库 A5 卡实测踩过）
        params.push(Value::Json(json.to_string()));
    }
    sets.push("update_by = ?::uuid".to_string());
    params.push(Value::from(&operator));
    params.push(Value::from(&task_id));
    let sql = format!(
        "UPDATE biz_task SET {} WHERE id = ?::uuid AND deleted_at IS NULL",
        sets.join(", ")
    );
    ex.exec_sql(sql, params).await
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

/// 绑定一个 AI 产出的附件（幂等：`(task_id, file_id)` 唯一，重复绑定不报错）。
///
/// # 返回值
///
/// 新绑定的附件 id；已绑定过则返回既有 id。
pub async fn bind_ai_attachment(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    file_id: Uuid,
    ai_profile: Option<&str>,
    operator: Uuid,
) -> DbResult<Uuid> {
    if let Some(existing) = ex
        .query_sql(
            "SELECT id FROM biz_task_attachment WHERE task_id = ?::uuid AND file_id = ?::uuid"
                .to_string(),
            vec![Value::from(&task_id), Value::from(&file_id)],
        )
        .await?
        .first()
    {
        let id: Uuid = existing.get_typed("id")?;
        // T6b：`uk_biz_task_attachment (task_id, file_id)` 是**全表**唯一，
        // 所以「删掉附件后重新上传同一个文件」必须走**复活**这条路
        // （清 `deleted_at`），否则 bind 会返回一条已删行、列表里永远看不到它。
        ex.exec_sql(
            "UPDATE biz_task_attachment \
                SET source = 'AI', ai_profile = ?, deleted_at = NULL \
              WHERE id = ?::uuid"
                .to_string(),
            vec![
                match ai_profile {
                    Some(profile) => Value::Text(profile.to_string()),
                    None => Value::Null,
                },
                Value::from(&id),
            ],
        )
        .await?;
        return Ok(id);
    }
    let id = Uuid::now_v7();
    let sort_order: i64 = ex
        .scalar_sql(
            "SELECT COALESCE(max(sort_order), -1) + 1 FROM biz_task_attachment WHERE task_id = ?::uuid"
                .to_string(),
            vec![Value::from(&task_id)],
        )
        .await
        .unwrap_or(0);
    ex.exec_sql(
        "INSERT INTO biz_task_attachment \
             (id, task_id, file_id, sort_order, source, ai_profile, created_at, create_by) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::int4, 'AI', ?, now(), ?::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&file_id),
            Value::I32(sort_order as i32),
            match ai_profile {
                Some(profile) => Value::Text(profile.to_string()),
                None => Value::Null,
            },
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(id)
}

/// 绑定一个人工上传的附件（幂等）。
pub async fn bind_human_attachment(
    ex: &impl SqlExecutor,
    task_id: Uuid,
    file_id: Uuid,
    operator: Uuid,
) -> DbResult<Uuid> {
    if let Some(existing) = ex
        .query_sql(
            "SELECT id FROM biz_task_attachment WHERE task_id = ?::uuid AND file_id = ?::uuid"
                .to_string(),
            vec![Value::from(&task_id), Value::from(&file_id)],
        )
        .await?
        .first()
    {
        let id: Uuid = existing.get_typed("id")?;
        // 同 `bind_ai_attachment`：全表唯一 → 重新上传同一个文件走「复活」。
        ex.exec_sql(
            "UPDATE biz_task_attachment SET deleted_at = NULL WHERE id = ?::uuid".to_string(),
            vec![Value::from(&id)],
        )
        .await?;
        return Ok(id);
    }
    let id = Uuid::now_v7();
    let sort_order: i64 = ex
        .scalar_sql(
            "SELECT COALESCE(max(sort_order), -1) + 1 FROM biz_task_attachment WHERE task_id = ?::uuid"
                .to_string(),
            vec![Value::from(&task_id)],
        )
        .await
        .unwrap_or(0);
    ex.exec_sql(
        "INSERT INTO biz_task_attachment \
             (id, task_id, file_id, sort_order, source, created_at, create_by) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::int4, 'HUMAN', now(), ?::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&task_id),
            Value::from(&file_id),
            Value::I32(sort_order as i32),
            Value::from(&operator),
        ],
    )
    .await?;
    Ok(id)
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

/// 统计窗口上限（天）。
///
/// 与甘特的 180 天不同：统计是**聚合**查询（一次 GROUP BY），扫一年也不会拖页面；
/// 但再宽就失去对比意义（AI 员工是分批接入的），而且会让误传的 `start=1970-01-01`
/// 变成一次全表扫描。超过上限一律 400，不做静默裁剪 —— 裁掉用户给的窗口比报错更坏。
pub const MAX_STATS_WINDOW_DAYS: i64 = 366;

/// 统计窗口（按 `biz_task.created_at` 落在 `[start, end]` 的 **CST 日区间**内计入）。
#[derive(Debug, Clone)]
pub struct StatsFilter {
    /// 窗口开始日（CST，含）。
    pub start: NaiveDate,
    /// 窗口结束日（CST，含）。
    pub end: NaiveDate,
    /// 只看某个项目（可空）。
    pub project_id: Option<Uuid>,
    /// 只看某个负责人（`scope=my` 时传当前用户 id；可空）。
    pub mine: Option<Uuid>,
}

/// 单个执行模式的统计行（`exec_mode` = `HUMAN` / `AI`）。
#[derive(Debug, Clone, Default)]
pub struct ExecModeStat {
    /// 执行模式。
    pub exec_mode: String,
    /// 窗口内任务总数（顶层任务）。
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
    /// 平均耗时（小时）= `avg(done_at - created_at)`，只算已完成且有 `done_at` 的行。
    pub avg_done_hours: Option<f64>,
    /// 平均进度（0–100，排除已取消）。
    pub avg_progress: Option<f64>,
}

/// 完成率 = `done / (total - cancelled)`，结果保留 4 位小数、钳在 `0..=1`。
///
/// 分母剔除已取消：取消是**分派人**的动作，不是执行者的失败；把取消算进分母，
/// 「派了活又撤销」就会拉低该 AI 员工的完成率，统计就变成了考核分派人的指标。
///
/// 分母为 0（窗口内没有有效任务）返回 `0.0` 而不是 `NaN`：后者会让前端 `toFixed`
/// 渲染成 `NaN%`。
pub fn completion_rate(done: i64, total: i64, cancelled: i64) -> f64 {
    let denominator = total - cancelled;
    if denominator <= 0 {
        return 0.0;
    }
    let rate = done.max(0) as f64 / denominator as f64;
    (rate.clamp(0.0, 1.0) * 10_000.0).round() / 10_000.0
}

/// 统计默认窗口：`end` 往前 29 天 到 `end`（含），即 **30 天**。
pub fn stats_default_window(end: NaiveDate) -> (NaiveDate, NaiveDate) {
    (end - Duration::days(29), end)
}

/// 窗口合法性校验；合法返回 `None`，非法返回 400 的白话原因。
///
/// 抽成纯函数：`start > end` 和「跨年算错天数」这两类错误都只会在 HTTP 层表现为
/// 「没数据」，靠人眼看统计卡片是查不出来的。
pub fn stats_window_error(start: NaiveDate, end: NaiveDate) -> Option<String> {
    if start > end {
        return Some(format!("统计窗口非法：开始日 {start} 晚于结束日 {end}"));
    }
    let days = (end - start).num_days() + 1;
    if days > MAX_STATS_WINDOW_DAYS {
        return Some(format!(
            "统计窗口过长：{days} 天，最多 {MAX_STATS_WINDOW_DAYS} 天"
        ));
    }
    None
}

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

#[cfg(test)]
mod stats_tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("合法日期")
    }

    #[test]
    fn completion_rate_excludes_cancelled_and_clamps() {
        assert_eq!(completion_rate(0, 0, 0), 0.0, "空窗口给 0.0 而不是 NaN");
        assert_eq!(completion_rate(0, 5, 5), 0.0, "全取消 → 分母 0 → 0.0");
        assert_eq!(completion_rate(3, 4, 0), 0.75);
        assert_eq!(completion_rate(4, 5, 1), 1.0, "取消剔除后 4/4 = 100%");
        assert_eq!(completion_rate(1, 3, 0), 0.3333, "保留 4 位小数");
        assert_eq!(completion_rate(9, 3, 0), 1.0, "脏数据也不返回 >1");
    }

    #[test]
    fn default_stats_window_is_thirty_days_inclusive() {
        let end = date(2026, 10, 29);
        let (start, same_end) = stats_default_window(end);
        assert_eq!(same_end, end);
        assert_eq!(start, date(2026, 9, 30));
        assert_eq!((end - start).num_days() + 1, 30);
        // 跨月 / 跨年边界
        let (start, _) = stats_default_window(date(2026, 1, 1));
        assert_eq!(start, date(2025, 12, 3));
    }

    #[test]
    fn stats_window_error_covers_reversed_and_too_long() {
        let start = date(2026, 10, 1);
        assert!(stats_window_error(start, date(2026, 10, 29)).is_none());
        assert!(stats_window_error(start, start).is_none(), "单日窗口合法");
        assert!(
            stats_window_error(date(2026, 10, 2), start)
                .expect("倒置必须报错")
                .contains("开始日")
        );
        let too_long = stats_window_error(date(2025, 1, 1), date(2026, 10, 29));
        assert!(
            too_long.expect("超 366 天必须报错").contains("366"),
            "文案必须点名上限"
        );
        assert_eq!(MAX_STATS_WINDOW_DAYS, 366);
    }
}

// ==================== T7a：甘特拖拽改期（读快照 + 落库） ====================

/// 改期评论的 `comment_type`（V0114 扩进 `ck_biz_task_comment_type`）。
///
/// 为什么单开一个类型而不是复用 `PLAIN`：改期是**系统产生的排期事实**，
/// 与「谁在什么时候把起止日从 X 改到 Y」是同一类留痕；复用 `PLAIN` 会让它在
/// 评论列表里和人的随手发言混在一起，也没法按类型筛（与 `STATUS_CHANGE` 同理）。
pub const COMMENT_TYPE_SCHEDULE: &str = "SCHEDULE";

/// 改期用的本任务排期快照（**事务内读**，防「读到的旧日期算出错误的平移」）。
///
/// 为什么不在事务外读一次就够：`shift_days` 的语义是「在当前日期上平移 N 天」，
/// 两个人同时把同一条任务平移 +3，正确结果是 +6 或第二次基于新值 +3 —— 两者都要求
/// 「读到的旧值」与「写入」在同一个事务里（配合 `pg_advisory_xact_lock` 串行化）。
#[derive(Debug, Clone)]
pub struct ScheduleRow {
    /// 任务主键。
    pub id: Uuid,
    /// 任务编号。
    pub task_no: String,
    /// 标题。
    pub title: String,
    /// 状态（5 态）。
    pub status: String,
    /// 当前计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 当前截止日。
    pub due_date: Option<NaiveDate>,
    /// 创建时间（「不得早于创建时间前 1 年」的下界由它推）。
    pub created_at: chrono::DateTime<Utc>,
    /// 父任务。
    pub parent_id: Option<Uuid>,
}

impl ScheduleRow {
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
            start_date: row.get_typed("start_date")?,
            due_date: row.get_typed("due_date")?,
            created_at: row.get_typed("created_at")?,
            parent_id: row.get_typed("parent_id")?,
        })
    }
}

/// 改期校验用到的「对端任务」排期快照（前置 / 后继 / 父 / 子四种关系共用）。
///
/// 四种关系只需要「编号 + 标题 + 状态 + 区间」这五个字段就能把冲突讲清楚，
/// 所以不按关系各建一个结构：文案里的点名格式只有一处实现
/// （见 [`crate::business::task::schedule`]），四个拷贝迟早不同步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerSchedule {
    /// 对端任务主键。
    pub peer_id: Uuid,
    /// 对端任务编号。
    pub task_no: String,
    /// 对端任务标题。
    pub title: String,
    /// 对端任务状态（5 态；前置是否「已解除」由纯逻辑判）。
    pub status: String,
    /// 对端任务计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 对端任务截止日。
    pub due_date: Option<NaiveDate>,
}

impl PeerSchedule {
    /// 由查询行构造（四个关系查询共用同一列清单 [`PEER_SCHEDULE_COLUMNS`]）。
    ///
    /// # 异常情况
    ///
    /// 列缺失 / 类型不符 → [`DbError`]。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            peer_id: row.get_typed("peer_id")?,
            task_no: row.get_typed("task_no")?,
            title: row.get_typed("title")?,
            status: row.get_typed("status")?,
            start_date: row.get_typed("start_date")?,
            due_date: row.get_typed("due_date")?,
        })
    }
}

/// 排期快照的列清单（四种关系共用，避免列名漂移）。
const PEER_SCHEDULE_COLUMNS: &str = "p.id AS peer_id, p.task_no, p.title, p.status, \
                                     p.start_date, p.due_date";

/// 事务内读本任务的排期快照（不存在 / 已软删 → `None`）。
///
/// # 异常情况
///
/// 数据库异常 → [`DbError`]。
pub async fn find_schedule_row(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<ScheduleRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, task_no, title, status, start_date, due_date, created_at, parent_id \
               FROM biz_task WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(ScheduleRow::from_row(row)?)),
        None => Ok(None),
    }
}

/// 未完成 / 未取消的前置（FS 边，「我依赖谁」）。
///
/// SQL 只做「关系筛选」（`d.task_id = 我` + `dep_type = 'FS'` + 对端未软删），
/// **不在这里过滤状态**：`DONE` / `CANCELLED` 是否算「已解除」的唯一判据是
/// [`crate::business::task::dep::is_blocking_status`]（纯逻辑，可单测）。
pub async fn schedule_prerequisites(
    ex: &impl SqlExecutor,
    task_id: Uuid,
) -> DbResult<Vec<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task_dependency d \
                   JOIN biz_task p ON p.id = d.depends_on_id AND p.deleted_at IS NULL \
                  WHERE d.task_id = ?::uuid AND d.dep_type = 'FS' \
                  ORDER BY p.id"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(PeerSchedule::from_row).collect()
}

/// 后继（谁依赖我；`depends_on_id = 我`）。
pub async fn schedule_successors(
    ex: &impl SqlExecutor,
    task_id: Uuid,
) -> DbResult<Vec<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task_dependency d \
                   JOIN biz_task p ON p.id = d.task_id AND p.deleted_at IS NULL \
                  WHERE d.depends_on_id = ?::uuid AND d.dep_type = 'FS' \
                  ORDER BY p.id"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(PeerSchedule::from_row).collect()
}

/// 父任务的排期（本任务没有父任务 / 父任务已软删 → `None`）。
pub async fn schedule_parent(
    ex: &impl SqlExecutor,
    parent_id: Uuid,
) -> DbResult<Option<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task p \
                  WHERE p.id = ?::uuid AND p.deleted_at IS NULL"
            ),
            vec![Value::from(&parent_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(PeerSchedule::from_row(row)?)),
        None => Ok(None),
    }
}

/// 子任务的排期（本卡**不级联**：父任务改期时用它做「别把子任务甩出去」的校验）。
pub async fn schedule_children(
    ex: &impl SqlExecutor,
    task_id: Uuid,
) -> DbResult<Vec<PeerSchedule>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT {PEER_SCHEDULE_COLUMNS} \
                   FROM biz_task p \
                  WHERE p.parent_id = ?::uuid AND p.deleted_at IS NULL \
                  ORDER BY p.id"
            ),
            vec![Value::from(&task_id)],
        )
        .await?;
    rows.iter().map(PeerSchedule::from_row).collect()
}

/// 落库新的起止日（只写这两列 + 更新时间）。
///
/// # 为什么复用 `TaskPatch` 的绑法而不是自己拼
///
/// `start_date = ?::date` 配 `Value::from(Option<NaiveDate>)` 是编辑接口已经验证
/// 过的组合（`Value::Text` 绑 `?::date` 会在驱动层报
/// `error serializing parameter`，T6b 踩过）。这里只写两列，`sets` 固定非空，
/// 不走 `update_task` 的「字段缺席」分支 —— 改期一定会改至少一个日期。
///
/// # 返回值
///
/// 受影响行数（0 = 任务不存在 / 已软删，调用方转 404）。
pub async fn update_schedule(
    ex: &Transaction,
    id: Uuid,
    start_date: Option<NaiveDate>,
    due_date: Option<NaiveDate>,
    operator: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_task SET start_date = ?::date, due_date = ?::date, \
                updated_at = now(), update_by = ?::uuid \
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::from(start_date),
            Value::from(due_date),
            Value::from(&operator),
            Value::from(&id),
        ],
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("合法日期")
    }

    /// T3：负责人变更只许一条 SQL，且必须带**乐观并发**条件（否则并发转派会串味）。
    #[test]
    fn assign_owner_sql_is_guarded() {
        assert!(
            ASSIGN_OWNER_SQL.starts_with("UPDATE biz_task"),
            "负责人变更只能是 biz_task 的单条 UPDATE"
        );
        assert_eq!(
            ASSIGN_OWNER_SQL.matches("?::uuid").count(),
            6,
            "owner_id / dept_id / assigner_id / update_by / id / expected_owner 六个 uuid 参数"
        );
        assert!(
            ASSIGN_OWNER_SQL.contains("AND owner_id = ?::uuid"),
            "必须按读到的旧负责人做乐观并发（并发转派 → 0 行 → 409）"
        );
        assert!(
            ASSIGN_OWNER_SQL.contains("deleted_at IS NULL"),
            "已软删的任务不许被分派"
        );
        assert!(
            ASSIGN_OWNER_SQL.contains("dept_id = ?::uuid"),
            "部门快照要跟着换"
        );
        assert!(
            ASSIGN_OWNER_SQL.contains("source = ?"),
            "来源要能改成 TRANSFERRED"
        );
    }

    /// T3：加协作人必须是**一条**集合式 INSERT（不许 N+1），且幂等 + 回吐新增行。
    #[test]
    fn members_insert_sql_is_set_based() {
        assert_eq!(
            MEMBERS_INSERT_SQL.matches("INSERT INTO").count(),
            1,
            "50 个人也必须只有一条 INSERT"
        );
        assert!(MEMBERS_INSERT_SQL.contains("unnest(?::text::uuid[])"));
        assert!(MEMBERS_INSERT_SQL.contains("ON CONFLICT (task_id, user_id) DO NOTHING"));
        assert!(
            MEMBERS_INSERT_SQL.contains("RETURNING user_id"),
            "要靠 RETURNING 区分「新增」与「本来就存在」"
        );
    }

    /// T3：协作人 id 数组参数沿用 `{uuid,uuid}` 文本字面量（驱动不支持 uuid[] 直绑）。
    #[test]
    fn ids_array_param_is_pg_literal() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        match ids_array_param(&[a, b]) {
            Value::Text(text) => assert_eq!(text, format!("{{{a},{b}}}")),
            other => panic!("必须是文本字面量，实际 {other:?}"),
        }
        match ids_array_param(&[]) {
            Value::Text(text) => assert_eq!(text, "{}"),
            other => panic!("空集合必须是 {{}}，实际 {other:?}"),
        }
    }

    /// F2：A 口径的四个判别用例 + 7 天边界两侧各 1 秒。
    #[test]
    fn is_new_task_matches_a_criteria() {
        let me = Uuid::parse_str("018f0000-0000-7000-8000-00000000000a").expect("合法 uuid");
        let other = Uuid::parse_str("018f0000-0000-7000-8000-00000000000b").expect("合法 uuid");
        let now = Utc::now();
        let day = Duration::days(1);
        assert!(
            is_new_task_at("TODO", me, now - day, me, now),
            "TODO + 我 + 1 天前 → 新"
        );
        assert!(
            !is_new_task_at("TODO", me, now - Duration::days(8), me, now),
            "TODO + 我 + 8 天前 → 不是新（出窗口）"
        );
        assert!(
            !is_new_task_at("DOING", me, now - day, me, now),
            "DOING + 我 → 不是新（口径只认 TODO）"
        );
        assert!(
            !is_new_task_at("TODO", other, now - day, me, now),
            "TODO + 别人 → 对我不新"
        );
        let window = Duration::days(i64::from(NEW_TASK_WINDOW_DAYS));
        assert!(
            is_new_task_at("TODO", me, now - window + Duration::seconds(1), me, now),
            "窗口内 1 秒 → 仍是新"
        );
        assert!(
            !is_new_task_at("TODO", me, now - window - Duration::seconds(1), me, now),
            "窗口外 1 秒 → 不再新"
        );
    }

    /// F2：列表/看板/甘特用的窗口必须与菜单徽标 `task_todo` 的窗口同值，
    /// 否则 A 口径退化成「徽标 2、列表 0 NEW」。
    #[test]
    fn new_window_matches_badge() {
        assert_eq!(
            NEW_TASK_WINDOW_DAYS,
            crate::system::badge::model::TASK_TODO_WINDOW_DAYS,
            "is_new 与 /system/badge/counts 的 task_todo 必须同窗口"
        );
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

    /// E6a：任务卡列清单必须同时下发「子任务总数」与「已完成数」。
    ///
    /// 少任一列 → [`TaskCardRow::from_row`] 运行期 `get_typed` 失败（列表 / 详情直接 500），
    /// 而现有一批用例都不连库，只有这条能在 `cargo test` 里提前拦住。
    #[test]
    fn card_columns_expose_child_stats() {
        assert!(CARD_COLUMNS.contains("AS child_count"));
        assert!(CARD_COLUMNS.contains("AS child_done_count"));
        // E6b：树形表格靠任务卡上的 parent_id 判定层级，缺列 → 列表 / 详情 500
        assert!(CARD_COLUMNS.contains("t.id, t.parent_id, t.task_no"));
        // 分子口径：未软删 + 终态 DONE（软删子任务不能算进完成数）
        assert!(CARD_COLUMNS.contains("c.deleted_at IS NULL"));
        assert!(CARD_COLUMNS.contains("c.status = 'DONE'"));
        // 子查询必须按 parent_id 等值锚定（走 idx_biz_task_parent），不许退化成全表扫描
        assert!(CARD_COLUMNS.contains("c.parent_id = t.id"));
        // 反向断言：CANCELLED 不是「完成」，别把取消也算进去
        assert!(!CARD_COLUMNS.contains("CANCELLED"));
    }
}

/// E12：附件显示名的纯逻辑回归（不连库，`cargo test` 里必然跑到）。
#[cfg(test)]
mod display_file_name_tests {
    use super::display_file_name;

    /// 正常路径：有 `original_name` 时一律用它（AI 产出的 `recheck.md` 就靠这条）。
    #[test]
    fn prefers_original_name() {
        assert_eq!(
            display_file_name(Some("recheck.md"), "2026/10/29/01a1874d.md"),
            "recheck.md"
        );
        // 原始名带空白要 trim，不能把空格写进保存名
        assert_eq!(
            display_file_name(Some("  report.md "), "2026/10/29/x.md"),
            "report.md"
        );
    }

    /// 边界 1：`original_name` 为 NULL → 退回存储名的 basename（去掉日期目录）。
    #[test]
    fn falls_back_to_stored_basename() {
        assert_eq!(
            display_file_name(None, "2026/10/29/01a1874d.md"),
            "01a1874d.md"
        );
        // Windows 风格分隔符同样要剥掉（备份 / 迁移数据里出现过）
        assert_eq!(
            display_file_name(None, "2026\\10\\29\\01a1874d.md"),
            "01a1874d.md"
        );
        // 没有分隔符 → 原样返回
        assert_eq!(display_file_name(None, "plain.md"), "plain.md");
    }

    /// 边界 2：`original_name` 是空串 / 全空白（上传时文件名缺失）→ 仍要 basename，不能下发空名或路径。
    #[test]
    fn empty_original_name_is_treated_as_missing() {
        assert_eq!(display_file_name(Some(""), "2026/10/29/x.md"), "x.md");
        assert_eq!(display_file_name(Some("   "), "2026/10/29/x.md"), "x.md");
    }

    /// 边界 3：两侧都空 → 空串（前端 `|| 'attachment'` 兜底），且绝不返回带分隔符的名字。
    #[test]
    fn never_returns_a_path() {
        assert_eq!(display_file_name(None, ""), "");
        // 尾部带分隔符时不能返回空串（取最后一个非空段）
        assert_eq!(display_file_name(None, "2026/10/29/"), "29");
        for case in [
            display_file_name(None, "2026/10/29/x.md"),
            display_file_name(Some(""), "2026/10/29/x.md"),
            display_file_name(Some("y.md"), "2026/10/29/x.md"),
        ] {
            assert!(!case.contains('/'), "显示名不许含路径分隔符：{case}");
        }
    }
}

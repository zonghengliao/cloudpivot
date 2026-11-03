// ============================================================================
// task_model_base.rs —— 底座：数据权限锚点 / 常量 / 状态机纯逻辑 / 时间与编号纯函数
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{Datelike, Duration, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_model_rows::*;
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
pub(super) fn ids_array_param(ids: &[Uuid]) -> Value {
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
pub(super) fn mark_is_new(rows: Vec<TaskCardRow>, viewer: Uuid) -> Vec<TaskCardRow> {
    let now = Utc::now();
    rows.into_iter()
        .map(|mut row| {
            row.is_new = is_new_task_at(&row.status, row.owner_id, row.created_at, viewer, now);
            row
        })
        .collect()
}

// ==================== 行结构 ====================

/// 任务卡列清单（所有列表查询共用，避免两处 select 列不一致导致的 `get_typed` 失败）。
pub(super) const CARD_COLUMNS: &str = "\
    t.id, t.parent_id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review, \
    t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
    t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at, \
    (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count, \
    (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL \
        AND c.status = 'DONE') AS child_done_count, \
    t.project_id, COALESCE(pj.project_no, '') AS project_code, \
    COALESCE(pj.project_name, '') AS project_name, \
    t.flow_instance_id, t.flow_node_key, t.flow_round";

/// 任务卡 FROM 子句（负责人姓名来自 `sys_user`：SELECT 里没有部门类过滤，
/// 数据权限片段只锚定 `t`，见 §2.6 规则 1）。
///
/// `biz_project` 的 JOIN 是 **T7c 追加的展示字段**（任务列表的「项目」列 / 详情抽屉的
/// 项目名）：`LEFT JOIN` + `deleted_at IS NULL` 保证项目被删后任务仍在列表里（`project_id`
/// 仍指向软删行 —— 由 T7c 的删除流程置空，这里再兜一层，历史脏数据也不会让任务消失）。
pub(super) const CARD_FROM: &str = " FROM biz_task t \
     LEFT JOIN sys_user u ON u.id = t.owner_id \
     LEFT JOIN biz_project pj ON pj.id = t.project_id AND pj.deleted_at IS NULL ";

/// 转义 `ILIKE` 通配符（用户搜 `%` / `_` 时不该变成「匹配一切」）。
pub fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
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

/// 父任务链最大深度（**老大拍板：8 层**）。
///
/// # 为什么是 8 而不是无限
///
/// 前端树形表格每层缩进 16px，8 层 = 128px，再深就把标题列吃掉了；业务上
/// 「总仓 → 分仓 → 库房 → 任务 → 子任务 → 子子任务」到 8 层已经够用。
/// 与 [`MAX_PARENT_DEPTH`]（进度向上重算的防御上限）同源，后者 = 本值 + 1。
pub const MAX_TASK_DEPTH: usize = 8;

/// 父任务链向上重算的最大深度（防御性上限：`parent_id` 成环时不会死循环）。
///
/// 业务上限是 [`MAX_TASK_DEPTH`]（8 层，由 [`validate_parent`] 把关），所以最深的
/// 任务也只有 7 级祖先；这里取「8 + 1」是给历史数据 / 手工改库造出的更深链留余量，
/// 同时保证「数据被手工改环」时也能停下来（配合 [`recalc_ancestors`] 的 visited 集合）。
pub const MAX_PARENT_DEPTH: usize = MAX_TASK_DEPTH + 1;

/// 父任务重算后是否写一条 `PROGRESS_AUTO` 评论（「可配置静默」的开关）。
///
/// 默认 `true`：进度是任务卡上最显眼的数字，被自动改动必须留痕（谁在什么时候
/// 把 60% 变成 80%、因为哪个子任务）。**只有进度真的变了才写**，所以不会刷屏。
/// 将来接 `sys_config` 时把这里换成配置读取即可，调用方不用改。
pub const PROGRESS_AUTO_COMMENT: bool = true;

/// 级联原因（规则①）：父任务因首个活动子任务自动进入进行中。
pub const CASCADE_REASON_FIRST_ACTIVE: &str = "父任务因首个活动子任务自动进入进行中";
/// 级联原因（规则②）：子任务全部到终态，父任务自动提交验收。
pub const CASCADE_REASON_ALL_TERMINAL: &str = "子任务已全部结束（完成 / 取消），父任务自动提交验收";

/// 纯逻辑：父任务该不该随子任务改状态（规则①②⑨，**碰不到数据库**）。
///
/// # 判定顺序（不可换）
///
/// 1. 没有子任务 → 不动（手动维护的任务不受级联影响）；
/// 2. 父任务已是终态 → 不动（**终态不回退**：父任务 DONE 之后子任务被拉回 `DOING`
///    属于脏数据修整，不该把已完成的任务重新打开）；
/// 3. 子任务全部到终态 → `REVIEW`；
/// 4. 父任务 `TODO` 且至少一个子任务不是 `TODO` → `DOING`；
/// 5. 其余（父任务 `DOING` / `REVIEW`，子任务还没全结束）→ 不动。
///
/// # 返回值
///
/// - `Some(target)`：需要改，且保证 `target != parent`；
/// - `None`：不动。
pub fn cascade_target(parent: TaskStatus, children: &[ChildSnapshot]) -> Option<TaskStatus> {
    if children.is_empty() || parent.is_terminal() {
        return None;
    }
    let all_terminal = state::open_children(children).is_empty();
    let target = if all_terminal {
        TaskStatus::Review
    } else if parent == TaskStatus::Todo
        && children
            .iter()
            .any(|child| child.status != TaskStatus::Todo)
    {
        TaskStatus::Doing
    } else {
        return None;
    };
    (target != parent).then_some(target)
}

/// SQL 里认的「终态」字面量（[`TaskStatus::is_terminal`] 的同口径副本）。
///
/// 扫描 SQL 只能写字面量，枚举将来若多一个终态而这里没跟上，就会**静默漏扫** ——
/// 所以单测 `terminal_codes_match_enum` 把两者钉死（扫描 SQL 本身由这里的常量拼出来，
/// 不重复写第二份字面量）。
pub const TERMINAL_STATUS_CODES: [&str; 2] = ["DONE", "CANCELLED"];

/// 一次巡检最多扫 / 修的条数（`handler` 侧同样按这个上限夹 `limit` 入参）。
pub const REPAIR_SCAN_LIMIT: i64 = 200;

/// 归一化巡检条数：缺省 [`REPAIR_SCAN_LIMIT`]，越界夹到 `[1, REPAIR_SCAN_LIMIT]`。
///
/// 为什么夹而不是报 400：这是运维兜底接口，`limit=0` / `limit=-1` / 手滑多打几个 0
/// 都不该让巡检失效 —— 夹到合法区间比拒绝更安全（拒绝等于「这次不一致没人管」）。
pub fn normalize_repair_limit(limit: Option<i64>) -> i64 {
    match limit {
        None => REPAIR_SCAN_LIMIT,
        Some(raw) => raw.clamp(1, REPAIR_SCAN_LIMIT),
    }
}

/// 级联取消写进子任务 `cancel_reason` 与变更日志的原因（审计要能一眼看出「不是我点的取消」）。
pub const CASCADE_CANCEL_REASON: &str = "父任务已取消，级联取消";

/// 纯逻辑：父任务已取消时，子任务的流转要不要拦（规则②，**碰不到数据库**）。
///
/// # 判定
///
/// - 父任务不是 `CANCELLED`（含没有父任务）→ 放行；
/// - 子任务自己就是去 `CANCELLED` → 放行（**幂等**：父任务已取消时再点一次取消
///   不该报错，那是收敛而不是推进）；
/// - 其余一律拒绝（`409 + 63021`）。
pub fn blocks_child_advance(parent: Option<TaskStatus>, to: TaskStatus) -> bool {
    to != TaskStatus::Cancelled && parent == Some(TaskStatus::Cancelled)
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

/// 依赖列表的列清单：对端任务 `p` + 依赖行 `d`（两处查询共用，避免列漂移）。
pub(super) const DEP_PEER_COLUMNS: &str = "\
    p.id AS peer_id, p.task_no, p.title, p.status, p.priority, p.progress, p.owner_id, \
    COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name, \
    p.start_date, p.due_date, d.dep_type, d.lag_days, d.created_at";

/// 统计窗口上限（天）。
///
/// 与甘特的 180 天不同：统计是**聚合**查询（一次 GROUP BY），扫一年也不会拖页面；
/// 但再宽就失去对比意义（AI 员工是分批接入的），而且会让误传的 `start=1970-01-01`
/// 变成一次全表扫描。超过上限一律 400，不做静默裁剪 —— 裁掉用户给的窗口比报错更坏。
pub const MAX_STATS_WINDOW_DAYS: i64 = 366;

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

/// 改期评论的 `comment_type`（V0114 扩进 `ck_biz_task_comment_type`）。
///
/// 为什么单开一个类型而不是复用 `PLAIN`：改期是**系统产生的排期事实**，
/// 与「谁在什么时候把起止日从 X 改到 Y」是同一类留痕；复用 `PLAIN` 会让它在
/// 评论列表里和人的随手发言混在一起，也没法按类型筛（与 `STATUS_CHANGE` 同理）。
pub const COMMENT_TYPE_SCHEDULE: &str = "SCHEDULE";

/// 排期快照的列清单（四种关系共用，避免列名漂移）。
pub(super) const PEER_SCHEDULE_COLUMNS: &str = "p.id AS peer_id, p.task_no, p.title, p.status, \
                                     p.start_date, p.due_date";

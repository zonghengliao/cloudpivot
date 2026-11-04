// ============================================================================
// task_model_rows.rs —— 行结构与入参契约：所有查询行 / 写库载荷 / 过滤条件结构体
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::task::state::{ChildSnapshot, TaskStatus};
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
    /// 关联单据类型（P2-9 / DC-19；`None` = 自由任务）。
    pub doc_type: Option<String>,
    /// 关联单据主键（弱引用；与 `doc_type` 成对，库侧 `ck_biz_task_doc_pair` 兜底）。
    pub doc_id: Option<Uuid>,
    /// 创建时间。
    pub created_at: chrono::DateTime<Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<Utc>,
    /// 生成这张任务的流程实例（P-FLOW-1A；非流程任务为 `None`）。
    pub flow_instance_id: Option<Uuid>,
    /// 生成它的节点 key（非流程任务为 `None`）。
    pub flow_node_key: Option<String>,
    /// 生成时的轮次（非流程任务为 `None`）。
    pub flow_round: Option<i32>,
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
            doc_type: row.get_typed("doc_type")?,
            doc_id: row.get_typed("doc_id")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            flow_instance_id: row.get_typed("flow_instance_id").ok().flatten(),
            flow_node_key: row.get_typed("flow_node_key").ok().flatten(),
            flow_round: row.get_typed("flow_round").ok().flatten(),
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

/// 分页过滤条件（由 handler 从 DTO 归一化而来）。
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    /// 状态（单值，向后兼容）。
    pub status: Option<String>,
    /// 状态过滤（多值；`Some(空 vec)` 等价于不筛）。
    ///
    /// 由 handler 从 `TaskPageQuery.statuses`（逗号分隔）归一化而来，**优先于单值
    /// `status`**：`IN (?,?,…)` 的占位符个数 = 本 vec 长度，绑定参数按 vec 顺序
    /// 逐个 push（`statuses_filter_tests` 里有硬断言）。
    pub statuses: Option<Vec<String>>,
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
    /// 来源过滤（P-FLOW-1A：`source=FLOW` 只看流程生成的任务）。
    ///
    /// **无绑定参数之外的一切与 `project_id` 同款**：只往 WHERE 追加一个 `t.source::text = ?`，
    /// 参数按文本顺序 push（`build_filters` 的红线：`?` 的文本顺序 = push 顺序）。
    pub source: Option<String>,
    /// 只返回顶层任务（`parent_id IS NULL`，T8-C）。
    ///
    /// 由 handler 从 `TaskPageQuery.root_only` 归一化而来（缺省 `false`）。
    /// **无绑定参数**：只往 WHERE 里追加一个常量片段，`scope::paginate` 的
    /// 「片段参数恒前置」约定与 `$1/$2` 位置完全不变（见 `root_only_tests`）。
    pub root_only: bool,
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

/// 任务 → 流程实例的详情（`GET /business/task/{id}/flow` 的出参来源）。
#[derive(Debug, Clone)]
pub struct TaskFlowDetailRow {
    /// 实例 id。
    pub instance_id: Uuid,
    /// 审批单号。
    pub instance_no: String,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务单号。
    pub biz_no: Option<String>,
    /// 实例状态。
    pub instance_status: String,
    /// 发起人。
    pub initiator_id: Uuid,
    /// 发起人姓名。
    pub initiator_name: String,
    /// 发起时间。
    pub started_at: chrono::DateTime<Utc>,
    /// 结束时间。
    pub ended_at: Option<chrono::DateTime<Utc>>,
    /// 生成它的节点 key。
    pub node_key: String,
    /// 节点中文名（快照里的 `node_name`；节点行缺失 → 空串）。
    pub node_name: String,
    /// 生成时的轮次。
    pub round: i32,
    /// 该节点本轮的实例状态（`ACTIVE` / `APPROVED` …；节点行缺失 → 空串）。
    pub node_status: String,
    /// 该节点 `config.task.on_complete`（快照值；没有 `config.task` → `None`）。
    pub on_complete: Option<String>,
    /// 该节点 `config.task.on_reject`（同上）。
    pub on_reject: Option<String>,
    /// 该节点 `config.task.on_cancel`（同上）。
    pub on_cancel: Option<String>,
    /// 本任务当前挂着的审批单（`biz_task.approval_instance_id` → `biz_flow_instance`
    /// + 其 `PENDING` 待办人；没有 → `None`）。
    pub approval: Option<TaskFlowApprovalRow>,
}

/// 「这张任务在等谁审批」的行结构（P-FLOW-1B 收尾）。
#[derive(Debug, Clone)]
pub struct TaskFlowApprovalRow {
    /// 审批实例 id。
    pub instance_id: Uuid,
    /// 审批单号。
    pub instance_no: String,
    /// 实例状态。
    pub status: String,
    /// 待办人姓名（按到达顺序，同名去重；非 `PENDING` → 空）。
    pub pending_approvers: Vec<String>,
}

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
    /// 关联单据类型（P2-9 / DC-19；`None` = 自由任务）。
    pub doc_type: Option<String>,
    /// 关联单据主键（与 `doc_type` 成对；handler 已校验存在性）。
    pub doc_id: Option<Uuid>,
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
    /// 关联单据（P2-9：`Some((Some(type), Some(id)))` = 整对替换；
    /// `Some((None, None))` = 清空挂接关系）。
    ///
    /// 用**一对**而不是两个独立 `Option`：`biz_task` 上有 `ck_biz_task_doc_pair`
    /// （类型与 id 必须同时有值或同时为空），两个独立字段能表达出「只改一个」的
    /// 非法中间态，一对则从类型上就不可能写坏。
    pub doc: Option<(Option<String>, Option<Uuid>)>,
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
            && self.doc.is_none()
    }
}

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
    pub(super) fn from_row(row: &Row) -> DbResult<Self> {
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

/// 完整版写评论的附加项（回复目标 + `@` 名单）。
#[derive(Debug, Clone, Copy, Default)]
pub struct CommentExtra<'a> {
    /// 回复的目标评论（`None` = 顶层评论）。
    pub parent_id: Option<Uuid>,
    /// 被 @ 的用户 ID 列表。
    pub mentions: &'a [Uuid],
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

/// 一次父任务状态级联的结果（日志 / 单测断言用）。
#[derive(Debug, Clone)]
pub struct StatusCascade {
    /// 被改状态的任务（父 / 祖父 …）。
    pub task_id: Uuid,
    /// 改前。
    pub from: TaskStatus,
    /// 改后（只会是 `DOING` 或 `REVIEW`）。
    pub to: TaskStatus,
    /// 参与判定的子任务数。
    pub children: usize,
    /// 子任务是否已全部到终态。
    pub all_terminal: bool,
    /// 触发原因（写进评论与变更日志）。
    pub reason: &'static str,
}

/// 巡检命中：父任务的状态落后于它的子任务（子任务全终态，父任务还没到 `REVIEW`）。
#[derive(Debug, Clone)]
pub struct ParentStatusGap {
    /// 父任务 id。
    pub task_id: Uuid,
    /// 父任务号（对账展示用）。
    pub task_no: String,
    /// 父任务标题。
    pub title: String,
    /// 父任务当前状态（只可能是 `TODO` / `DOING`）。
    pub status: TaskStatus,
    /// 父任务当前进度（巡检**不动**进度 —— 进度归 [`recalc_ancestors`]）。
    pub progress: i16,
    /// 未删除子任务数。
    pub children: i64,
    /// 应该到的状态（[`cascade_target`] 规则③：子任务全终态 → `REVIEW`）。
    pub target: TaskStatus,
    /// 修复起点：从这里往上跑 [`cascade_parent_status`]，修到的就是本父任务。
    pub sample_child: Uuid,
}

/// 一次级联取消的结果（`handler` 用它逐条写变更日志，单测断言用）。
#[derive(Debug, Clone)]
pub struct CancelCascade {
    /// 被取消的任务。
    pub task_id: Uuid,
    /// 被取消任务的业务单号（P11：撤审批实例时 `biz_flow_instance.biz_no` 要按它匹配，
    /// 兜住 `biz_id` 为空的历史实例行；顺带让日志能直接打出单号）。
    pub task_no: String,
    /// 取消前的状态（只可能是 `TODO` / `DOING` / `REVIEW`）。
    pub from: TaskStatus,
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

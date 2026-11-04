//! 工作任务管理 · DTO（请求 / 响应契约）。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md` §二（状态机取值）、§三（进度）、
//! §七（可见性）、§十（两秒建任务 → 创建默认值）、§十一（页面出参需要哪些字段）。
//!
//! # 命名与格式约定（与批次 A/B/C 一致，前端不为任务管理单独适配）
//!
//! - 主键 / 外键一律 `String`（`nik::uuid::Uuid` 没有 serde，序列化不了）；
//! - 日期字段一律 `chrono::NaiveDate`，JSON 形态是 `"2026-10-30"`（`DATE` 列，无时区）；
//! - 时间戳字段一律 `chrono::DateTime<Utc>`，JSON 形态是 RFC3339（`TIMESTAMPTZ` 列）；
//! - 枚举一律大写字符串（`TODO` / `P2` / `SELF`），与库里的 CHECK 约束逐字一致 ——
//!   前端的色标 / 文案映射按这些字面量写，**大小写不许改**。
//!
//! # 编辑接口的「字段缺席 ≠ 清空」
//!
//! `TaskUpdateReq` 的可清空字段用 `Option<Option<T>>`（`#[serde(default)]` +
//! 自定义反序列化）：字段缺席 → 保持原值；显式 `null` → 清空；有值 → 赋值。
//! 这条规则在公告（批次 C）上踩过一次坑：「漏传 = 清空」会把截止日默默抹掉，
//! 任务列表的逾期分组会当场错乱，所以这里从一开始就用双重 Option。

use nik::prelude::chrono::{self, NaiveDate};
use serde::{Deserialize, Deserializer, Serialize};

// ==================== 常量与枚举取值 ====================

/// 标题最大字符数（与 `biz_task.title VARCHAR(200)` 对齐）。
pub const MAX_TITLE_CHARS: usize = 200;
/// 描述最大字符数（TEXT 列，但接口层限长，避免一次请求塞进 10 MB）。
pub const MAX_DESCRIPTION_CHARS: usize = 20_000;

/// 原因（打回 / 取消）最大字符数（与 `biz_task.reject_reason VARCHAR(500)` 对齐）。
pub const MAX_REASON_CHARS: usize = 500;

/// 状态：待开始。
pub const STATUS_TODO: &str = "TODO";
/// 状态：进行中。
pub const STATUS_DOING: &str = "DOING";
/// 状态：待验收。
pub const STATUS_REVIEW: &str = "REVIEW";
/// 状态：已完成。
pub const STATUS_DONE: &str = "DONE";
/// 状态：已取消。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 全部状态（顺序 = 状态机推进顺序，前端看板列按此排列）。
pub const STATUSES: [&str; 5] = [
    STATUS_TODO,
    STATUS_DOING,
    STATUS_REVIEW,
    STATUS_DONE,
    STATUS_CANCELLED,
];

/// 优先级四级（老大 v2 拍板：P0 最高 → P3 最低）。
pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];

/// 创建任务时的默认优先级（设计文档 §十：两秒建任务，默认 P2）。
pub const DEFAULT_PRIORITY: &str = "P2";

/// 来源：自建。
pub const SOURCE_SELF: &str = "SELF";
/// 来源：被分派。
pub const SOURCE_ASSIGNED: &str = "ASSIGNED";

/// 来源：被转派（T3：把已有任务改派给别人）。
pub const SOURCE_TRANSFERRED: &str = "TRANSFERRED";

/// 来源：由流程节点 `TASK_NODE` 生成（P-FLOW-1A，`V0177` 扩的第 5 个值）。
///
/// 与 `SOURCE_SELF` / `SOURCE_ASSIGNED` 的区别不是「谁建了它」，而是**它属于一条流程实例**：
/// `flow_instance_id` / `flow_node_key` / `flow_round` 三列非空，且「同一节点同一轮只有一张」。
/// 任务完成会回推流程（P-FLOW-1B），这是手工任务没有的行为。
pub const SOURCE_FLOW: &str = "FLOW";

/// 任务来源的全部取值（`biz_task_source_check` 与筛选参数共用一份口径）。
pub const SOURCES: [&str; 5] = [
    SOURCE_SELF,
    SOURCE_ASSIGNED,
    SOURCE_TRANSFERRED,
    "RECURRING",
    SOURCE_FLOW,
];

/// 协作人角色（`biz_task_member.role`）：协作人 / 关注者两档中的协作人。
pub const MEMBER_ROLE_COLLABORATOR: &str = "COLLABORATOR";

/// 单任务协作人上限（T3 硬上限，`biz_task_member` 行数）。
///
/// 50 是一个**滥用防线**而不是业务规则：协作人直接进可见性 id 数组，人数越多
/// 「我参与」数组越大（决策 D7 的 2000 上限就是这个放大链的终点）。
pub const MAX_MEMBERS_PER_TASK: i64 = 50;

/// 权限码：分派 / 转派（V0109 建的菜单节点 `…0768`）。
///
/// **它不走中间件硬校验**，而是 handler 里的「加项」：创建人 / 当前负责人 / 分派人
/// 这三类**行级关系**天然可派，权限码只解决「管理员 / AI 主管要能跨任务派活」。
/// 若把它写成 `#[nik::permission(...)]`，创建人（没有这个码）会被中间件挡在门外。
/// 读权限码（详情 / 评论 / 附件的人类侧入口闸）。
///
/// T5b 把详情端点纳入「任务令牌白名单」后，权限层不再覆盖该路径，
/// 人类侧的这条判定改由 handler 显式执行（`ensure_task_read_permission`）。
pub const PERM_TASK_READ: &str = "business:task:read";

pub const PERM_ASSIGN: &str = "business:task:assign";

/// 权限码：管理协作人（V0109 建的菜单节点 `…0769`），语义同 [`PERM_ASSIGN`]。
pub const PERM_MEMBER: &str = "business:task:member";

/// 校验优先级取值。
///
/// # 异常情况
///
/// 不在 `P0`–`P3` → 返回 `None`（由调用方转 400 + `TASK_FIELD_INVALID`）。
pub fn normalize_priority(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    let upper = value.to_ascii_uppercase();
    PRIORITIES.contains(&upper.as_str()).then_some(upper)
}

/// 校验状态取值。
///
/// # 异常情况
///
/// 不在 5 态之内 → 返回 `None`。
pub fn normalize_status(raw: &str) -> Option<String> {
    let upper = raw.trim().to_ascii_uppercase();
    STATUSES.contains(&upper.as_str()).then_some(upper)
}

/// 截断到指定字符数（按 `char` 计，不是字节 —— 中文标题按字节算会砍半）。
pub fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars().take(limit).collect()
}

/// 折叠空白并去首尾空格（标题 / 关键词统一走它）。
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ==================== 请求：分页 ====================

/// 任务分页查询（`GET /business/task/page`）。
///
/// 六个筛选维度对应设计文档 §十一「任务板」的列头筛选。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskPageQuery {
    /// 页码（从 1 开始，框架 `normalize_page` 兜底）。
    pub page: Option<u64>,
    /// 每页条数（框架上限 `MAX_PAGE_SIZE`）。
    pub page_size: Option<u64>,
    /// 状态过滤（`TODO` / `DOING` / `REVIEW` / `DONE` / `CANCELLED`）。
    ///
    /// **单值**写法，保留向后兼容（项目下任务列表等调用方还在用它）；
    /// 多选请用 [`statuses`](Self::statuses)。
    pub status: Option<String>,
    /// 状态过滤（多选，逗号分隔，如 `TODO,DOING`）。
    ///
    /// 与单值的 `status` 并存：`statuses` 非空时**优先**，`status` 保留向后兼容
    /// （项目下任务列表等调用方还在用单值）。空字符串 / 不传 = 不筛状态。
    pub statuses: Option<String>,
    /// 优先级过滤（`P0`–`P3`）。
    pub priority: Option<String>,
    /// 负责人过滤（用户 UUID）。
    pub owner_id: Option<String>,
    /// 项目过滤（`biz_task.project_id`；T7c 项目分组）。
    ///
    /// 只是**过滤条件**，不改变可见性 —— 可见性恒为 `数据范围 ∪ 参与关系`。
    pub project_id: Option<String>,
    /// 来源过滤（`SELF` / `ASSIGNED` / `TRANSFERRED` / `RECURRING` / `FLOW`，P-FLOW-1A）。
    ///
    /// `source=FLOW` 是「只看流程生成的任务」的唯一入口：与项目过滤一样，它**只是过滤条件**，
    /// 不改变可见性（可见性恒为 `数据范围 ∪ 参与关系`，见 `model::page_visible`）。
    pub source: Option<String>,
    /// 截止日下界（含）。
    pub due_from: Option<NaiveDate>,
    /// 截止日上界（含）。
    pub due_to: Option<NaiveDate>,
    /// 关键词（标题 / 描述模糊，ILIKE）。
    pub keyword: Option<String>,
    /// 只返回顶层任务（`parent_id IS NULL`，T8-C）。
    ///
    /// # 为什么需要
    ///
    /// 列表是**分页**的，父子任务混在一起返回时，一棵 3 层的树会被拆到两页
    /// （父在第 2 页、子在第 1 页），前端无论如何排序都无法呈现层级。前端树形视图
    /// 默认传 `true`，子任务通过 `GET /business/task/{id}/subtasks` 逐层懒加载 ——
    /// 每层展开才拉，天然支持 T8-A 的 8 层深度。
    ///
    /// # 契约
    ///
    /// - 不传 / `false` → 行为与 T8-B 之前**完全一致**（向后兼容，看板 / 甘特 / 燃尽图都不传）；
    /// - `true` → `list` 与 `total` **同一个 WHERE**（都只算顶层），否则分页器会显示
    ///   「43 条 / 5 页」而实际只有 28 条顶层，第 6 页空白。
    #[serde(default)]
    pub root_only: Option<bool>,
}

impl TaskPageQuery {
    /// 归一化关键词：折叠空白 + 去空 → `None`。
    pub fn normalized_keyword(&self) -> Option<String> {
        let raw = self.keyword.as_deref()?;
        let collapsed = collapse_whitespace(raw);
        (!collapsed.is_empty()).then_some(collapsed)
    }
}

// ==================== 请求：创建 ====================

/// 创建任务（`POST /business/task`）。
///
/// **只有 `title` 是必填项** —— 设计文档 §十「两秒建任务」：输入标题敲回车即可，
/// 其余字段全部有默认值（负责人=自己、状态=TODO、优先级=P2、截止=本周五、需验收=关）。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskCreateReq {
    /// 标题（必填，1–200 字符）。
    pub title: String,
    /// 描述（可空）。
    pub description: Option<String>,
    /// 优先级（缺省 `P2`）。
    pub priority: Option<String>,
    /// 是否需验收（缺省 `false`）。
    pub need_review: Option<bool>,
    /// 负责人（缺省 = 创建人自己；传他人 = 直接分派）。
    pub owner_id: Option<String>,
    /// 父任务（可空；父子深度上限 8 层，见 [`crate::business::task::model::MAX_TASK_DEPTH`]）。
    pub parent_id: Option<String>,
    /// 所属项目（可空，P1）。
    pub project_id: Option<String>,
    /// 计划开始日（可空）。
    pub start_date: Option<NaiveDate>,
    /// 截止日（缺省 = 本周五，CST）。
    pub due_date: Option<NaiveDate>,
    /// 预估工时（小时，可空）。
    pub estimate_hours: Option<f64>,
    /// 关联单据类型（P2-9 / DC-19；白名单见
    /// [`crate::business::task::doc_ref::DOC_TARGETS`]，类型词表取自 `DocKind`）。
    ///
    /// 与 `doc_id` **必须成对**（`None` + `None` = 自由任务）。采购单详情底部
    /// 「关联任务」区新建任务时就带这一对。
    pub doc_type: Option<String>,
    /// 关联单据主键（弱引用；handler 校验「存在且未软删」后落库）。
    pub doc_id: Option<String>,
}

// ==================== 请求：编辑 ====================

/// 编辑任务（`PUT /business/task/{id}`）。
///
/// # 本卡只改「非状态类」字段
///
/// `status` / `progress` / `done_at` / `cancel_reason` / `reject_reason` **不在本结构里**：
/// 它们属于状态机（§二 8 条迁移规则）与进度模型（§三），是 T2 的交付物。
/// 若这里开放状态直改，T2 的「8 条迁移规则」就得和一条后门共存 —— 两份真相。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskUpdateReq {
    /// 标题（缺席 = 不改）。
    pub title: Option<String>,
    /// 描述（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// 优先级（缺席 = 不改）。
    pub priority: Option<String>,
    /// 是否需验收（缺席 = 不改）。
    pub need_review: Option<bool>,
    /// 负责人（缺席 = 不改；传他人 = 直接改字段，**转派语义属 T3**）。
    pub owner_id: Option<String>,
    /// 父任务（缺席 = 不改；`null` = 摘成顶层任务）。
    #[serde(default, deserialize_with = "double_option")]
    pub parent_id: Option<Option<String>>,
    /// 所属项目（缺席 = 不改；`null` = 移出项目）。
    #[serde(default, deserialize_with = "double_option")]
    pub project_id: Option<Option<String>>,
    /// 计划开始日（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub start_date: Option<Option<NaiveDate>>,
    /// 截止日（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub due_date: Option<Option<NaiveDate>>,
    /// 预估工时（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub estimate_hours: Option<Option<f64>>,
    /// 关联单据类型（P2-9；缺席 = 不改；`null` = 与 `doc_id` 一起清空）。
    ///
    /// **必须与 `doc_id` 同时出现或同时为 `null`**（一个传一个不传 → 400 + 63004）：
    /// 「只改类型不改 id」在语义上不存在（换了类型，原 id 指向的是另一张表）。
    #[serde(default, deserialize_with = "double_option")]
    pub doc_type: Option<Option<String>>,
    /// 关联单据主键（同上；换了单据类型 / 单据 → 走这里整对替换）。
    #[serde(default, deserialize_with = "double_option")]
    pub doc_id: Option<Option<String>>,
}

impl TaskUpdateReq {
    /// 是否一个可改字段都没传（全缺席 → 400，避免「空 PUT 假成功」）。
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.priority.is_none()
            && self.need_review.is_none()
            && self.owner_id.is_none()
            && self.parent_id.is_none()
            && self.project_id.is_none()
            && self.start_date.is_none()
            && self.due_date.is_none()
            && self.estimate_hours.is_none()
            && self.doc_type.is_none()
            && self.doc_id.is_none()
    }
}

/// `Option<Option<T>>` 的反序列化：字段缺席 → `None`，`null` → `Some(None)`。
fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ==================== 响应：任务卡（列表 / 子任务 / 我的任务共用） ====================

/// 任务卡上的「来源流程」段（P-FLOW-1A）。
///
/// 只有 `source = 'FLOW'` 的任务才有值 —— `flow_instance_id` 非空 ⇔ `source='FLOW'`
/// 是库侧 `ck_biz_task_source_flow` 的双向硬约束，所以「有 flow 段」与「是流程任务」等价。
/// 更细的流程信息（单号 / 名称 / 状态）走 `GET /business/task/{id}/flow`，列表不为此多 JOIN 一次。
#[derive(Debug, Clone, Serialize)]
pub struct TaskFlowRef {
    /// 生成这张任务的流程实例（`biz_flow_instance.id`）。
    pub instance_id: String,
    /// 生成它的节点 key（`biz_flow_node.node_key`）。
    pub node_key: String,
    /// 生成时的轮次（退回重进同一节点时 `round+1`）。
    pub round: i32,
}

/// `GET /business/task/{id}/flow` 的回执（P-FLOW-1A）。
///
/// 手工任务（`source != 'FLOW'`）也能 200 调用：`flow` 为 `null`。
/// **不返回 404** —— 「这张任务没有流程」不是错误，前端据此隐藏卡片即可。
#[derive(Debug, Clone, Serialize)]
pub struct TaskFlowRes {
    /// 任务 id。
    pub task_id: String,
    /// 任务编号。
    pub task_no: String,
    /// 流程来源（非流程任务 → `null`）。
    pub flow: Option<TaskFlowDetailRes>,
}

/// 任务当前的「审批进度」（P-FLOW-1B 收尾 · 设计 §4.5 Q4 第 3 条）。
///
/// # 它回答的问题
///
/// 「这张任务 **现在** 在等谁审批？」——`on_complete=APPROVE` 的节点，任务
/// `DOING → DONE` 时节点保持 `ACTIVE` 并同事务发起审批单；此时前端只看到
/// 「任务已完成 + 节点还在跑」，**没有这个结构就只能靠猜**。
///
/// # 数据来源
///
/// 只认 `biz_task.approval_instance_id`（P-FLOW-1B 写入的指针，验收审批与节点审批
/// 共用一列）+ 该实例下的 `PENDING` 待办，不做任何推断：
/// 指针为空 / 指向的实例不存在 → 整个 `approval` 为 `null`。
#[derive(Debug, Clone, Serialize)]
pub struct TaskFlowApprovalRes {
    /// 审批实例 id（前端跳 `/flow/instance/{id}` 用）。
    pub instance_id: String,
    /// 审批单号。
    pub instance_no: String,
    /// 实例状态（`PENDING` / `APPROVED` / `REJECTED` / `CANCELLED` / `WITHDRAWN`）。
    pub status: String,
    /// 待办人姓名（`status = PENDING` 时的待办人；其它状态恒为 `[]`）。
    ///
    /// 会签时是多人，按待办到达顺序；同名只保留一次（会签节点不允许同一人占两个槽位，
    /// 但同一实例的并行节点可能让同一个人有多条待办，直接拼接会出现「王五、王五」）。
    pub pending_approvers: Vec<String>,
}

/// 任务所属流程的详情（任务详情页「所属审批单」卡片）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskFlowDetailRes {
    /// 实例 id（前端跳 `/flow/instance/{id}` 用）。
    pub instance_id: String,
    /// 审批单号。
    pub instance_no: String,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务单号（空串 = 没有关联单号）。
    pub biz_no: String,
    /// 实例状态（`PENDING` / `APPROVED` / `REJECTED` / `CANCELLED` / `WITHDRAWN`）。
    pub instance_status: String,
    /// 发起人 id。
    pub initiator_id: String,
    /// 发起人姓名。
    pub initiator_name: String,
    /// 发起时间。
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// 结束时间（在跑 → `null`）。
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 生成这张任务的节点 key。
    pub node_key: String,
    /// 节点中文名。
    pub node_name: String,
    /// 生成时的轮次。
    pub round: i32,
    /// 该节点本轮的实例状态（`ACTIVE` = 正在等这张任务完成）。
    pub node_status: String,
    /// 任务完成后流程怎么走（`config.task.on_complete`：`DIRECT` / `VERIFY` / `APPROVE`）。
    ///
    /// 取自**实例生成当时的发布快照**（不是定义行），后续改版本不会让历史任务「换说法」。
    /// `null` = 该节点的 `config.task` 里没有这一项（非 `TASK_NODE` 生成的流程任务、
    /// 或快照被人工改坏）—— 前端按 `need_review` 推断，**不会**收到空串。
    pub on_complete: Option<String>,
    /// 任务被打回时流程怎么走（`config.task.on_reject`：`BACK_TO_PREV` / `TERMINATE` / `STAY`）。
    pub on_reject: Option<String>,
    /// 任务被取消时流程怎么走（`config.task.on_cancel`：`TERMINATE` / `BACK_TO_PREV` / `STAY`）。
    pub on_cancel: Option<String>,
    /// 本任务当前挂着的审批单进度（P-FLOW-1B 收尾）。
    ///
    /// - `null` = 这张任务没挂审批单（`biz_task.approval_instance_id IS NULL`，例如
    ///   `on_complete=DIRECT` 的节点生成的任务），或指针指向的实例已不存在；
    /// - `Some(status = "PENDING")` + 非空 `pending_approvers` = **正在等这些人审批**
    ///   （`on_complete=APPROVE` 的节点，任务已完成但节点还 `ACTIVE`）；
    /// - 终态（`APPROVED` / `REJECTED` / `CANCELLED` / `WITHDRAWN`）时 `pending_approvers` 为空数组。
    ///
    /// 前端只在 `task.status = DONE` 且 `approval.status = PENDING` 时把「任务」那栏渲染成
    /// 「已完成 ✓ 审批中（王五）」——**不要**再用 `node_status` 猜。
    pub approval: Option<TaskFlowApprovalRes>,
}

/// 任务卡（列表、子任务列表、我的任务分组统一用它，避免三套形状）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskCardRes {
    /// 主键。
    pub id: String,
    /// 任务编号 `TSK-YYYYMM-NNNNNN`。
    pub task_no: String,
    /// 标题。
    pub title: String,
    /// 状态（5 态之一）。
    pub status: String,
    /// 优先级（`P0`–`P3`）。
    pub priority: String,
    /// 进度 0~100。
    pub progress: i32,
    /// 是否需要验收。
    pub need_review: bool,
    /// 负责人 ID。
    pub owner_id: String,
    /// 负责人姓名（`real_name` 优先，退化为账号）。
    pub owner_name: String,
    /// 归属部门 ID（可空）。
    pub dept_id: Option<String>,
    /// 来源（`SELF` / `ASSIGNED` / `TRANSFERRED`）。
    pub source: String,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日。
    pub due_date: Option<NaiveDate>,
    /// 子任务数（未软删）。
    pub child_count: i64,
    /// 已完成子任务数（E6a：`status = 'DONE'` 且未软删；列表页「3/1」与详情页进度条用）。
    pub child_done_count: i64,
    /// 父任务 ID（E6b：列表树形表格靠它判定「顶层 / 子行」，`null` = 顶层任务）。
    pub parent_id: Option<String>,
    /// 所属项目 ID（T7c；`null` = 未归入项目）。
    pub project_id: Option<String>,
    /// 项目编码（空串 = 无项目 / 项目已删）。
    pub project_code: String,
    /// 项目名称（空串 = 无项目 / 项目已删）。
    pub project_name: String,
    /// 关联单据类型（P2-9 / DC-19；`null` = 自由任务）。
    ///
    /// 前端按它对上「单据类型 → 路由 / 文案」（`PURCHASE_ORDER` → 采购单详情），
    /// 列表也能直接显示「挂：采购单」角标，**不需要**再查一次接口。
    pub doc_type: Option<String>,
    /// 关联单据主键（弱引用；`null` = 自由任务）。
    pub doc_id: Option<String>,
    /// 来源段（P-FLOW-1A）：`source != 'FLOW'` 时为 `null`。
    ///
    /// 前端据此在任务卡上标「流程任务」并给出「查看审批单」入口，**不需要**再查一次接口。
    pub flow: Option<TaskFlowRef>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 最后更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// 是否为「新任务」（F2 · A 口径：`TODO` ∧ 负责人是我 ∧ 7 天内创建）。
    ///
    /// 与 `GET /system/badge/counts` 的 `task_todo` **同一口径**（同一时间窗常量，
    /// 单测 `model::tests::new_window_matches_badge` 钉死），前端三视图（列表红点 /
    /// 看板 NEW 角标 / 甘特圆点）只看这个字段，不在前端重算。
    ///
    /// 详情接口（`TaskDetailRes` 扁平复用了本结构）恒为 `false`：卡面要求详情不做「新」标记。
    pub is_new: bool,
}

// ==================== 响应：分页 ====================

/// 任务分页出参（`GET /business/task/page`）。
///
/// 是框架 `PageData<Vec<TaskCardRes>>` 的**超集**：前四个字段与它逐字一致
/// （前端 `request.ts` 的 `PageData<T>` 照旧可用），多一个 `member_scope_truncated`。
///
/// 为什么需要这个标记（决策 D7）：可见性 = 数据范围 ∪ 我参与的任务 id 数组，
/// 而数组有 2000 硬上限。超限时后端**降级为只按范围**过滤，必须让前端知道
/// 「这不是你的全部任务」，否则用户会以为任务丢了。
#[derive(Debug, Clone, Serialize)]
pub struct TaskPageRes {
    /// 当前页任务卡。
    pub list: Vec<TaskCardRes>,
    /// 可见总数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 「我参与」的任务 id 超过上限（2000）→ 本视图只按数据范围过滤，
    /// 前端应提示「我参与的任务过多，本视图只显示范围内的；『我参与的』请走独立 tab」。
    pub member_scope_truncated: bool,
}

// ==================== 响应：详情 ====================

/// 任务协作人。
#[derive(Debug, Clone, Serialize)]
pub struct TaskMemberRes {
    /// 用户 ID。
    pub user_id: String,
    /// 用户姓名。
    pub user_name: String,
    /// 角色（`COLLABORATOR` / `FOLLOWER`）。
    pub role: String,
    /// 加入时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 任务附件。
#[derive(Debug, Clone, Serialize)]
pub struct TaskAttachmentRes {
    /// 绑定行 ID。
    pub id: String,
    /// 文件 ID（`sys_file.id`）。
    pub file_id: String,
    /// 原始文件名。
    pub file_name: String,
    /// 文件大小（字节）。
    pub file_size: i64,
    /// 展示顺序。
    pub sort_order: i32,
    /// 来源：`HUMAN` 人类上传 / `AI` AI 产出（T5b）。
    pub source: String,
    /// 产出该附件的 AI profile（仅 `source = 'AI'` 时有值）。
    pub ai_profile: Option<String>,
}

// ==================== T5b：评论 / 附件 / 派活 / AI 进度 ====================

/// 写一条任务评论（`POST /business/task/{id}/comment`）。
///
/// 人类（JWT）与 AI（任务令牌）走同一份 DTO：AI 的评论内容由它自己决定，
/// 云枢只约束长度与类型。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskCommentReq {
    /// 评论正文（纯文本，1–5000 字符）。
    pub content: String,
    /// 回复的上级评论 ID（可空）。
    pub parent_comment_id: Option<String>,
    /// 被 @ 的用户 ID 数组（可空；**每个人都收到强制通知**，不存在的账号会被忽略）。
    #[serde(default)]
    pub mentions: Vec<String>,
}

/// 人类发表评论（`POST /business/task/{id}/comments`，T6b 补漏）。
///
/// # 为什么单独一个 DTO（而不是复用 [`TaskCommentReq`]）
///
/// 区别只有一处：**显式收下 `comment_type`**。用户伪造 `AI_REPORT` / `STATUS_CHANGE`
/// 时要给出 400（红线：用户只能发 `PLAIN`），而不是靠 serde 静默丢弃 —— 静默丢弃
/// 在验收与排障时看不出「模型被拒了」还是「字段名写错了」。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskPostCommentReq {
    /// 正文（trim 后 1–5000 字符）。
    pub content: String,
    /// 回复的上级评论 ID（可空；**必须属于同一任务**，否则 400）。
    pub parent_comment_id: Option<String>,
    /// 被 @ 的用户 ID（不存在 / 已停用的账号**忽略**，回执里见 `mentions_ignored`）。
    #[serde(default)]
    pub mentions: Vec<String>,
    /// 只允许 `PLAIN`（缺省 = `PLAIN`）；其它值一律 400。
    #[serde(default)]
    pub comment_type: Option<String>,
}

/// 评论写的回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskCommentRes {
    /// 评论 ID。
    pub id: String,
    /// 评论类型（`PLAIN` / `AI_REPORT` / …）。
    pub comment_type: String,
    /// 评论人。
    pub user_id: String,
    /// 评论人姓名。
    pub user_name: String,
    /// 落库时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 实际生效的被 @ 用户（去重后；只有真正收到通知的账号）。
    pub mentions: Vec<String>,
    /// 被忽略的 `mentions`（账号不存在 / 已停用 / 已删除）。
    ///
    /// 卡面 §一 的方案是「忽略而不是报错」：一次 @ 里有一个人已离职，不该让整条评论发不出去；
    /// 但也不能假装成功，所以逐个回传，前端弹一条提示。
    pub mentions_ignored: Vec<String>,
}

/// 附件上传的回执（`POST /business/task/{id}/attachment`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskUploadRes {
    /// 绑定行 ID。
    pub attachment_id: String,
    /// 文件 ID（`sys_file.id`，下载走 `/business/storage/file/{file_id}/download`）。
    pub file_id: String,
    /// 原始文件名。
    pub file_name: String,
    /// 字节数。
    pub file_size: i64,
    /// 来源：`HUMAN` / `AI`。
    pub source: String,
    /// 上传分类（`task` / `task_ai`）。
    pub category: String,
}

/// 派活给 AI（`POST /business/task/{id}/dispatch-ai`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskDispatchReq {
    /// 派活说明（写进评论，可空）。
    pub reason: Option<String>,
    /// 传给 Hermes 的工作区（`dir:<绝对路径>`）；空 = 用 AI profile 的默认 workspace。
    pub workspace: Option<String>,
}

/// 派活的回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskDispatchRes {
    /// 云枢任务 id。
    pub task_id: String,
    /// Hermes 任务 id（形如 `t_4b7abf18`）。
    pub hermes_task_id: String,
    /// Hermes 看板 slug。
    pub hermes_board: String,
    /// Hermes 侧回报的状态（正常是 `ready`）。
    pub hermes_status: String,
    /// 云枢执行模式（派活成功后恒为 `AI`）。
    pub exec_mode: String,
    /// 云枢状态（**派活不改状态**，仍为调用前的值）。
    pub status: String,
    /// 任务令牌的过期时间（前端展示「AI 的凭据到什么时候有效」）。
    pub token_expires_at: chrono::DateTime<chrono::Utc>,
}

/// AI 进度回写（`PATCH /business/task/{id}/ai-progress`，任务令牌认证）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskAiProgressReq {
    /// 百分比 0–100（**有子任务时被忽略**：进度由子任务汇总）。
    pub percent: Option<i32>,
    /// 进度说明（写一条 `AI_PROGRESS` 评论）。
    pub note: Option<String>,
}

/// AI 进度回写的回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskAiProgressRes {
    /// 落库后的进度（`ignored_percent = true` 时是任务当前进度，未被本次请求改动）。
    pub progress: i32,
    /// 是否忽略了 `percent`（任务有子任务 → 进度由子任务汇总，D4 规则）。
    pub ignored_percent: bool,
    /// 评论 ID（写了 note 才有）。
    pub comment_id: Option<String>,
}

/// 任务详情（`GET /business/task/{id}`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskDetailRes {
    /// 任务卡字段（扁平展开，前端详情页右栏直接复用列表字段）。
    #[serde(flatten)]
    pub card: TaskCardRes,
    /// 描述。
    pub description: Option<String>,
    /// 创建人 ID。
    pub creator_id: String,
    /// 创建人姓名。
    pub creator_name: String,
    /// 分派人 ID（自建任务为空）。
    pub assigner_id: Option<String>,
    /// 分派人姓名。
    pub assigner_name: String,
    /// 部门名称。
    pub dept_name: String,
    /// 父任务 ID。
    pub parent_id: Option<String>,
    /// 父任务编号（面包屑用）。
    pub parent_task_no: Option<String>,
    /// 关联单据单号（P2-9 / DC-19；`null` = 没挂单，或单据已被软删）。
    ///
    /// 由 handler 现查回填（`doc_ref::doc_no_of`）：`doc_type` / `doc_id` 随任务卡
    /// 扁平下发，单号是「另一张表里的展示字段」—— 查不到就 `null`（前端显示
    /// 「单据已删除」），**不回落 uuid**。
    pub doc_no: Option<String>,
    /// 完成时间。
    pub done_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 预估工时。
    pub estimate_hours: Option<f64>,
    /// 实际工时。
    pub actual_hours: Option<f64>,
    /// 取消原因。
    pub cancel_reason: Option<String>,
    /// 打回原因。
    pub reject_reason: Option<String>,
    /// 审批流实例（批次 D 预留，本批恒为 `null`）。
    pub approval_instance_id: Option<String>,
    /// 执行模式：`HUMAN` / `AI`（T5b）。
    pub exec_mode: String,
    /// Hermes 看板（未派活为 `null`）。
    pub hermes_board: Option<String>,
    /// Hermes 任务 id（未派活为 `null`）。
    pub hermes_task_id: Option<String>,
    /// Hermes 原始状态（未派活为 `null`；`MISSING` 表示关联任务已丢失）。
    pub hermes_status: Option<String>,
    /// AI 执行摘要（`latest_summary` 的冗余）。
    pub ai_summary: Option<String>,
    /// AI 结构化结果（`runs[..].metadata`，前端渲染「交付清单」）。
    pub ai_result: Option<serde_json::Value>,
    /// 子任务列表（**已按调用者的可见性过滤**，见 `model::list_children`）。
    pub children: Vec<TaskCardRes>,
    /// 协作人 / 关注者。
    pub members: Vec<TaskMemberRes>,
    /// 附件。
    pub attachments: Vec<TaskAttachmentRes>,
}

// ==================== 响应：按单据反查任务（P2-9） ====================

/// 按单据反查任务（`GET /business/task/by-doc`）的查询参数。
///
/// 两个都必填：`doc_type` 决定查哪张表、`doc_id` 决定查哪一行，
/// 少一个无法定位（handler 直接 400 + 63004）。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskByDocQuery {
    /// 单据类型（白名单见 [`crate::business::task::doc_ref::DOC_TARGETS`]）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
}

/// 按单据反查任务（`GET /business/task/by-doc`）的出参。
///
/// 形状与 [`TaskPageRes`] 的前几项一致（`list` / `total`），但**不分页**：
/// 一张单上挂的任务天然是十几条量级；同时多回 `doc_no`，前端在采购单详情
/// 底部直接显示「关联任务（PO-xxx）」，不必再查一次单据。
#[derive(Debug, Clone, Serialize)]
pub struct TaskByDocRes {
    /// 单据类型（归一化后的大写值；未登记的取值不会走到这里）。
    pub doc_type: String,
    /// 单据主键。
    pub doc_id: String,
    /// 单据单号（查不到 / 已软删 → `null`）。
    pub doc_no: Option<String>,
    /// 关联任务总数（= `list.len()`，保留字段便于将来加硬上限时提示）。
    pub total: i64,
    /// 关联任务卡（**已按任务可见性过滤**）。
    pub list: Vec<TaskCardRes>,
}

// ==================== 响应：写操作 ====================

/// 创建 / 编辑的回执（只回主键、编号与几个关键字段，前端拿它更新列表行）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskSaveRes {
    /// 主键。
    pub id: String,
    /// 任务编号。
    pub task_no: String,
    /// 状态。
    pub status: String,
    /// 优先级。
    pub priority: String,
    /// 进度。
    pub progress: i32,
    /// 截止日。
    pub due_date: Option<NaiveDate>,
    /// 负责人 ID。
    pub owner_id: String,
}

/// 删除回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskDeleteRes {
    /// 被软删的任务 ID。
    pub id: String,
    /// 是否真的删掉了一行（`false` = 早已软删 / 并发重复删除）。
    pub deleted: bool,
}

// ==================== 响应：我的任务 ====================

/// 「我的任务」五个分组（设计文档 §十一「我的任务」页面）。
///
/// 分组口径（`model::MyBucket`）：
/// - `overdue` 逾期：`due_date < 今天`；
/// - `today` 今日：`due_date = 今天`；
/// - `this_week` 本周：`今天 < due_date <= 本周日`；
/// - `later` 以后：`due_date > 本周日`；
/// - `no_due` 无期限：`due_date IS NULL`。
///
/// 「今天」按 **CST（UTC+8）** 计算 —— 老大的时区，也是本系统所有「今天」的口径。
#[derive(Debug, Clone, Serialize)]
pub struct MyTasksRes {
    /// 逾期。
    pub overdue: Vec<TaskCardRes>,
    /// 今日。
    pub today: Vec<TaskCardRes>,
    /// 本周。
    pub this_week: Vec<TaskCardRes>,
    /// 以后。
    pub later: Vec<TaskCardRes>,
    /// 无期限。
    pub no_due: Vec<TaskCardRes>,
    /// 各组条数（顺序与上面一致，前端顶部角标直接用）。
    pub counts: MyCounts,
    /// 参与统计的任务总数（= 五组之和）。
    pub total: i64,
}

/// 「我的任务」各组条数。
#[derive(Debug, Clone, Serialize)]
pub struct MyCounts {
    /// 逾期条数。
    pub overdue: i64,
    /// 今日条数。
    pub today: i64,
    /// 本周条数。
    pub this_week: i64,
    /// 以后条数。
    pub later: i64,
    /// 无期限条数。
    pub no_due: i64,
}

// ==================== 请求：状态机（T2） ====================

/// 改状态（`PATCH /business/task/{id}/status`）。
///
/// # `reason` 的三种用途（同一个字段，按流转解释）
///
/// | 流转 | `reason` 语义 | 必填 |
/// |---|---|---|
/// | `REVIEW → DOING`（打回） | 打回原因（进 `reject_reason` + 评论） | ✅ 至少 5 字 |
/// | `任意 → CANCELLED` | 取消原因（进 `cancel_reason` + 评论） | ❌ 可选 |
/// | 其它 | 备注（只进评论） | ❌ |
///
/// 一个字段而不是三个：前端只有一个「原因」输入框，多开字段会让「填了但没生效」的
/// 情况变多（卡面也只声明了 `{to, reason?}`）。
///
/// # 为什么 `to` 还接受 `status` 别名
///
/// 卡面定的字段名是 `to`（与「目标状态」对齐），但前端任务书（T6a）的草稿里写的是
/// `{ status, reason? }` —— 卡片与前端各写一套，最容易出现的结局是「接口 400、
/// 前端以为后端没实现」。两个名字都收（`to` 为主、`status` 为别名）成本为零，
/// 却把这类联调返工直接消灭。
///
/// **只能二选一**：serde 的 `alias` 是「同一个字段的另一个名字」，两个都传会被判
/// `duplicate field`（400）。这是刻意的 —— 「既传 to 又传 status」本身就是歧义请求，
/// 与其猜用户想要哪个，不如直接拒绝。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskStatusChangeReq {
    /// 目标状态（`TODO` / `DOING` / `REVIEW` / `DONE` / `CANCELLED`，大小写不敏感）。
    ///
    /// 别名 `status`：兼容前端草稿字段名（见上方说明）。
    #[serde(alias = "status")]
    pub to: String,
    /// 原因（打回必填，见上方表格）。
    ///
    /// 别名 `reject_reason`：打回场景前端可能直接叫这个名字。
    #[serde(alias = "reject_reason")]
    pub reason: Option<String>,
}

/// 改进度（`PATCH /business/task/{id}/progress`）。
///
/// `i32` 而不是 `i16`：越界值要回 `400 + 63004` 并带上原值提示，
/// 用 `i16` 反序列化时 serde 会先报「类型错误」，用户看到的是框架的英文错误。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskProgressReq {
    /// 目标进度 0~100（超出范围 → 400）。
    pub progress: i32,
}

/// 建子任务（`POST /business/task/{id}/subtask`）。
///
/// 只有 `title` 必填：其余字段缺省时**继承父任务**（`priority` / `project_id` /
/// `need_review` / `due_date`），负责人缺省 = 当前操作人。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskSubtaskCreateReq {
    /// 标题（必填，1–200 字符）。
    pub title: String,
    /// 描述（可空）。
    pub description: Option<String>,
    /// 优先级（缺省继承父任务）。
    pub priority: Option<String>,
    /// 负责人（缺省 = 当前操作人）。
    pub owner_id: Option<String>,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日（缺省继承父任务）。
    pub due_date: Option<NaiveDate>,
    /// 预估工时（小时）。
    pub estimate_hours: Option<f64>,
    /// 是否需验收（缺省继承父任务）。
    pub need_review: Option<bool>,
}

// ==================== 响应：状态机（T2） ====================

/// 父任务重算结果（子任务流转 / 改进度时回传，前端可直接刷新父卡片）。
#[derive(Debug, Clone, Serialize)]
pub struct ParentProgressRes {
    /// 被重算的任务 ID（父 / 祖父 …，顺序由近及远）。
    pub id: String,
    /// 重算前进度。
    pub previous: i32,
    /// 重算后进度。
    pub progress: i32,
    /// 聚合口径（`工时加权` / `等权平均` / `子任务全部完成`）。
    pub mode: String,
    /// 子任务是否已全部完成（前端可据此把父卡片标成「可结项」，但**不会**自动改状态）。
    pub all_done: bool,
}

/// 改状态回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskStatusRes {
    /// 任务 ID。
    pub id: String,
    /// 任务编号。
    pub task_no: String,
    /// 流转前的状态。
    pub from: String,
    /// 流转后的状态（= 库里的现值）。
    pub status: String,
    /// 流转后的进度（无子任务时可能被联动改动）。
    pub progress: i32,
    /// 完成时间（进入 `DONE` 后有值；重开后为 `null`）。
    pub done_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 本次写入的状态变更评论 ID（前端时间线可直接定位）。
    pub comment_id: String,
    /// 父任务重算结果（顶层任务为空数组）。
    pub parent_progress: Vec<ParentProgressRes>,
}

/// 改进度回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskProgressRes {
    /// 任务 ID。
    pub id: String,
    /// 落库后的进度（**可能不等于 `requested`**：状态联动会抬高）。
    pub progress: i32,
    /// 请求里提交的原值（前端据此提示「你要 5，实际落 10（开工至少 10%）」）。
    pub requested: i32,
}

/// 子任务列表（`GET /business/task/{id}/subtasks`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskSubtaskListRes {
    /// 父任务 ID。
    pub parent_id: String,
    /// 父任务状态（前端据此决定「能不能加子任务」）。
    pub parent_status: String,
    /// 父任务进度（**汇总值**，不是子任务的平均手算值）。
    pub progress: i32,
    /// 父任务进度是否被锁定（有子任务即 `true` → 前端隐藏滑块）。
    pub progress_locked: bool,
    /// 子任务总数（**全部**未软删子任务，不受可见性过滤影响）。
    pub total: i64,
    /// 子任务列表（**已按调用者可见性过滤**）。
    pub list: Vec<TaskCardRes>,
}

// ==================== 分派 / 协作人（T3） ====================

/// 分派 / 转派（`PATCH /business/task/{id}/assign`）。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskAssignReq {
    /// 新负责人（`sys_user.id`，必填）。
    ///
    /// 会被校验为**有效用户**：存在、未软删、`status = 1`；否则 `400 + 63009`。
    /// `ROBOT` 账号**可以**被分派（AI 是执行者，被派活是它的本职）。
    pub owner_id: String,
    /// 转派原因（可空，≤ 500 字）。会拼进 `ASSIGN` 评论正文。
    pub reason: Option<String>,
}

/// 分派 / 转派回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskAssignRes {
    /// 任务 ID。
    pub id: String,
    /// 任务编号。
    pub task_no: String,
    /// 新负责人 ID。
    pub owner_id: String,
    /// 原负责人 ID（前端可直接用来提示「已从 X 转给 Y」）。
    pub previous_owner_id: String,
    /// 落库后的分派人（操作人非创建人时 = 操作人）。
    pub assigner_id: Option<String>,
    /// 落库后的来源（转派恒为 `TRANSFERRED`）。
    pub source: String,
    /// 原负责人是否被**自动加为协作人**（分派给他人时恒 `true`）。
    pub auto_member: bool,
    /// 写入的 `ASSIGN` 评论 ID（详情页时间线用它定位这条记录）。
    pub comment_id: String,
    /// 实际收到**强制通知**的人（已剔除操作人自己；通知失败时为空，业务仍成功）。
    pub notified: Vec<String>,
}

/// 加协作人（`POST /business/task/{id}/members`）。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskMemberAddReq {
    /// 用户 ID 列表（去重后使用；空列表 → 400）。
    pub user_ids: Vec<String>,
}

/// 协作人列表（`GET /business/task/{id}/members`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskMembersRes {
    /// 任务 ID。
    pub task_id: String,
    /// 协作人列表（按加入时间升序）。
    pub list: Vec<TaskMemberRes>,
    /// 协作人总数。
    pub total: i64,
    /// 单任务上限（前端据此隐藏「再加一个」按钮）。
    pub limit: i64,
}

/// 加协作人回执（三份名单分开，前端可以分别提示）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskMemberAddRes {
    /// 任务 ID。
    pub task_id: String,
    /// 本次**新增**的用户 ID（幂等：已存在的不在这里）。
    pub added: Vec<String>,
    /// 本来就已经是协作人的用户 ID（幂等命中，不报错）。
    pub existing: Vec<String>,
    /// 被跳过的用户 ID：创建人 / 负责人（他们本来就能看，不落协作人行）。
    pub skipped: Vec<String>,
    /// 加完之后的完整列表。
    pub list: Vec<TaskMemberRes>,
    /// 加完之后的协作人总数。
    pub total: i64,
    /// 单任务上限。
    pub limit: i64,
}

/// 移除协作人回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskMemberRemoveRes {
    /// 任务 ID。
    pub task_id: String,
    /// 被移除的用户 ID。
    pub user_id: String,
    /// 是否真的删掉了一行（`false` 不会出现：不是协作人一律 404）。
    pub removed: bool,
    /// 移除之后的协作人总数。
    pub total: i64,
}

// ==================== 依赖 / 甘特（T4） ====================

/// 依赖权限码（**加项**：行级关系之外的授权）。
///
/// 与 `business:task:assign` / `member` 同一套路：中间件闸只要求
/// `business:task:read`（能看见任务的人才谈得上管依赖），真矩阵在 handler 里：
/// 「负责人 / 创建人 / 分派人」天然可管依赖，其他人要 `business:task:dependency`。
///
/// V0110 只把这个码授给 SUPER_ADMIN，其余角色按需勾选。
pub const PERM_DEPENDENCY: &str = "business:task:dependency";

/// 滞后天数上限（`FS+lag` 的 lag 部分，本版只存不算排程）。
pub const MAX_LAG_DAYS: i32 = 365;

/// 加前置依赖（`POST /business/task/{id}/dependencies`）。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskDependencyAddReq {
    /// 前置任务 id（必填，必须可见 / 未软删 / 不是自己 / 不是父子）。
    pub depends_on_id: String,
    /// 滞后天数（`FS+lag`，缺省 0，范围 0–365）。
    pub lag_days: Option<i32>,
}

/// 依赖项（前置组 / 后继组共用一条形状，`dep_id` 是对端任务 id）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskDependencyItemRes {
    /// 对端任务 id（前置组 = 前置 id，后继组 = 后继 id）；删除时用它。
    pub dep_id: String,
    /// 对端任务编号。
    pub task_no: String,
    /// 对端任务标题。
    pub title: String,
    /// 对端任务状态。
    pub status: String,
    /// 对端任务优先级。
    pub priority: String,
    /// 对端任务进度。
    pub progress: i32,
    /// 对端任务负责人。
    pub owner_id: String,
    /// 负责人姓名。
    pub owner_name: String,
    /// 计划开始日。
    pub start_date: Option<NaiveDate>,
    /// 截止日。
    pub due_date: Option<NaiveDate>,
    /// 依赖类型（本版恒 `FS`）。
    pub dep_type: String,
    /// 滞后天数。
    pub lag_days: i32,
    /// 对端是否**仍在阻塞**本任务（前置组：状态非 DONE/CANCELLED 时为 `true`；
    /// 后继组：恒 `false` —— 阈值只对「我要等的人」有意义）。
    pub blocking: bool,
    /// 依赖建立时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 依赖列表（`GET /business/task/{id}/dependencies`，两组分开）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskDependenciesRes {
    /// 任务 id。
    pub task_id: String,
    /// 前置任务（必须先完成）。
    pub prerequisites: Vec<TaskDependencyItemRes>,
    /// 后继任务（本任务完成后才能做）。
    pub successors: Vec<TaskDependencyItemRes>,
    /// 未完成前置数量（前端画「被 N 个前置阻塞」可直接用，不用自己数）。
    pub blocking_count: i64,
}

/// 加依赖回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskDependencyAddRes {
    /// 任务 id（后继）。
    pub task_id: String,
    /// 前置任务 id。
    pub depends_on_id: String,
    /// 是否**真的新增**了一行（`false` = 幂等命中：依赖已存在，未重复插入）。
    pub created: bool,
    /// 落库的滞后天数。
    pub lag_days: i32,
    /// 加完之后的完整两组（前端不必再请求一次）。
    pub prerequisites: Vec<TaskDependencyItemRes>,
    /// 后继组。
    pub successors: Vec<TaskDependencyItemRes>,
    /// 未完成前置数量。
    pub blocking_count: i64,
}

/// 删依赖回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskDependencyRemoveRes {
    /// 任务 id。
    pub task_id: String,
    /// 被解除的对端任务 id。
    pub dep_id: String,
    /// 恒 `true`（不存在时直接 404，不静默成功）。
    pub removed: bool,
    /// 删完之后的完整两组。
    pub prerequisites: Vec<TaskDependencyItemRes>,
    /// 后继组。
    pub successors: Vec<TaskDependencyItemRes>,
    /// 未完成前置数量。
    pub blocking_count: i64,
}

/// 甘特图查询（`GET /business/task/gantt`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskGanttQuery {
    /// 看板维度（**本版不支持**：没有自定义看板列，传了就 400）。
    pub board_id: Option<String>,
    /// 项目维度（`biz_task.project_id`，P1 表已预留）。
    pub project_id: Option<String>,
    /// 负责人维度（`biz_task.owner_id`）。
    pub assignee_id: Option<String>,
    /// 窗口起始日（缺省今天，CST）。
    pub start: Option<NaiveDate>,
    /// 窗口结束日（缺省 `start + 29` 天；窗口上限 180 天）。
    pub end: Option<NaiveDate>,
    /// `my` = 只看我负责的；缺省 / `all` = 我的可见范围。
    pub scope: Option<String>,
}

/// 甘特时间窗。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GanttRangeRes {
    /// 窗口第一天。
    pub start: NaiveDate,
    /// 窗口最后一天。
    pub end: NaiveDate,
    /// 总天数（含首尾）。
    pub days: i64,
    /// 今天（CST）。前端据此画「今天线」，不用本机时钟算（防时区漂移）。
    pub today: NaiveDate,
    /// 今天在窗口里的第几天（0 开始）；今天不在窗口内 → `null`（不画线）。
    pub today_offset: Option<i64>,
}

/// 甘特任务条的位置（**后端算好**，前端只按缩放比例乘列宽）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GanttBarRes {
    /// 距窗口第一天的天数（0 开始，已按窗口裁剪）。
    pub offset_days: i64,
    /// 条形长度（天，>= 1）。
    pub length_days: i64,
}

/// 甘特任务。
#[derive(Debug, Clone, Serialize)]
pub struct GanttTaskRes {
    /// 任务 id。
    pub id: String,
    /// 任务编号。
    pub task_no: String,
    /// 标题。
    pub title: String,
    /// 状态（5 态，前端按它上色）。
    pub status: String,
    /// 优先级。
    pub priority: String,
    /// 进度 0~100（前端画内条）。
    pub progress: i32,
    /// 负责人 id。
    pub owner_id: String,
    /// 负责人姓名。
    pub owner_name: String,
    /// 计划开始日（原值）。
    pub start_date: Option<NaiveDate>,
    /// 截止日（原值）。
    pub due_date: Option<NaiveDate>,
    /// 父任务（前端折叠子任务用）。
    pub parent_id: Option<String>,
    /// 里程碑（P1 才有，本版恒 `false`）。
    pub is_milestone: bool,
    /// 是否被未完成前置阻塞（前端画斜纹）。
    pub blocked: bool,
    /// 是否为「新任务」（F2，A 口径，与列表 `TaskCardRes.is_new` 同一判据）。
    pub is_new: bool,
    /// 是否逾期（`due_date < 今天` 且未终态）。
    pub overdue: bool,
    /// 条形位置（`null` = 排不进窗口，理论上不会出现：SQL 已按窗口过滤）。
    pub bar: Option<GanttBarRes>,
}

/// 甘特依赖边。`from` = 前置（先做完），`to` = 后继（被阻塞）。
#[derive(Debug, Clone, Serialize)]
pub struct GanttDepRes {
    /// 前置任务 id。
    pub from: String,
    /// 后继任务 id。
    pub to: String,
}

/// 甘特图数据（`GET /business/task/gantt`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskGanttRes {
    /// 时间窗。
    pub range: GanttRangeRes,
    /// 任务（只有排得上期的：`start_date` 或 `due_date` 至少有一个）。
    pub tasks: Vec<GanttTaskRes>,
    /// 依赖边（两端都在 `tasks` 里才下发）。
    pub dependencies: Vec<GanttDepRes>,
    /// 任务数超过上限（500）→ 已截断，前端应提示缩小区间或加筛选。
    pub truncated: bool,
    /// 「我参与」id 超 2000 → 可见性已降级为「只按范围」（D7，与列表页同一标记）。
    pub member_scope_truncated: bool,
}

// ==================== 单元测试（纯逻辑，不连库） ====================

// ==================== 统计（T5b · b4：exec_mode 维度） ====================

/// `GET /business/task/stats` 的查询参数。
///
/// 窗口按**创建时间**的 CST 日区间；默认最近 30 天（含今天）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskStatsQuery {
    /// 统计窗口开始日（`YYYY-MM-DD`，CST，含）。缺省 = `end` 往前 29 天。
    pub start: Option<NaiveDate>,
    /// 统计窗口结束日（`YYYY-MM-DD`，CST，含）。缺省 = 今天（CST）。
    pub end: Option<NaiveDate>,
    /// 项目 id（可空）。
    pub project_id: Option<String>,
    /// `my` / `all`（与甘特同口径）；缺省 = `all`。
    pub scope: Option<String>,
}

/// 单个执行模式（`HUMAN` / `AI`）的统计行。
#[derive(Debug, Clone, Serialize)]
pub struct ExecModeStatRes {
    /// 执行模式；合计行为 `TOTAL`。
    pub exec_mode: String,
    /// 任务总数。
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
    /// 完成率 = `done / (total - cancelled)`，`0.0–1.0`（4 位小数）。
    pub completion_rate: f64,
    /// 平均耗时（小时，1 位小数）；窗口内没有已完成任务时为 `null`。
    pub avg_done_hours: Option<f64>,
    /// 平均进度（0–100，1 位小数，排除已取消）。
    pub avg_progress: Option<f64>,
}

/// 统计数据（`GET /business/task/stats`）。
///
/// `by_exec_mode` **恒含 `HUMAN` 与 `AI` 两行**（窗口内没有任务就给 0）：前端是固定
/// 两列的对比卡片，返回变长数组会让前端再写一层「缺哪行」的分支。
#[derive(Debug, Clone, Serialize)]
pub struct TaskStatsRes {
    /// 窗口开始日（CST）。
    pub start: NaiveDate,
    /// 窗口结束日（CST）。
    pub end: NaiveDate,
    /// 合计行（各模式之和；平均耗时按已完成数加权）。
    pub total: ExecModeStatRes,
    /// 按执行模式分行（`HUMAN` / `AI`，可能追加历史脏值行）。
    pub by_exec_mode: Vec<ExecModeStatRes>,
}

/// 父任务状态巡检的查询参数（T13）。
///
/// 为什么 `dry_run` 缺省是 **true**：这是批量修数接口，默认行为必须是「只报不修」——
/// 手滑点一次就把全库父任务状态改掉是不可接受的。要真修必须显式传 `dry_run=false`。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ParentStatusRepairQuery {
    /// `true`（缺省）= 只报不修；`false` = 真修。
    pub dry_run: Option<bool>,
    /// 最多扫 / 修多少条（缺省 200，越界夹到 `[1, 200]`）。
    pub limit: Option<i64>,
}

/// 巡检命中的一条（父任务状态落后于它的子任务）。
#[derive(Debug, Clone, Serialize)]
pub struct ParentStatusGapRes {
    /// 父任务 id。
    pub id: String,
    /// 父任务号。
    pub task_no: String,
    /// 父任务标题。
    pub title: String,
    /// 命中时的状态（`TODO` / `DOING`）。
    pub status: String,
    /// 命中时的进度（巡检不动它）。
    pub progress: i16,
    /// 未删除子任务数。
    pub children: i64,
    /// 应到的状态（`REVIEW`）。
    pub target: String,
    /// 修复后回读的实际状态；`dry_run` / 修复失败时为 `null`。
    pub status_after: Option<String>,
}

/// 巡检 / 修复的结果。
#[derive(Debug, Clone, Serialize)]
pub struct ParentStatusRepairRes {
    /// 本次是不是只报不修。
    pub dry_run: bool,
    /// 命中条数（不一致的父任务数）。
    pub total: usize,
    /// 修好的条数（`dry_run` 恒为 0）。
    pub fixed: usize,
    /// 修复失败的条数（`dry_run` 恒为 0）。
    pub failed: usize,
    /// 逐条明细。
    pub items: Vec<ParentStatusGapRes>,
}

/// 权限码：删除他人评论（T6b）。
///
/// 作者本人删自己的评论**不需要**这个码（行级关系优先）；它解决的是
/// 「管理员 / 版主清理他人留言」这一类跨人操作。
pub const PERM_COMMENT_DELETE: &str = "business:task:comment:delete";

/// 权限码：删除他人上传的任务附件（T6b）。
pub const PERM_ATTACHMENT_DELETE: &str = "business:task:attachment:delete";

/// 权限码：给「非负责人 / 非协作人」登记与删除工时（T6b）。
pub const PERM_WORKLOG: &str = "business:task:worklog";

// ==================== T6b：评论 / 附件 / 工时 ====================

/// 评论列表查询（`GET /business/task/{id}/comments`）。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskCommentPageQuery {
    /// 页码（从 1 开始；缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 100 由 `utils::normalize_page` 收口）。
    pub page_size: Option<u64>,
}

/// 单条评论（出参）。
///
/// `readonly` 与 `can_delete` 都是**服务端算好的结论**：前端只负责渲染，
/// 不许自己按 `comment_type` 推导 —— 推导逻辑一漂移就会出现「按钮能点、
/// 点下去 403」的死路（卡面 §一 规则 7 的交互面）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskCommentItemRes {
    /// 评论 ID。
    pub id: String,
    /// 评论类型：`PLAIN`（人写的）/ `AI_REPORT` / `STATUS_CHANGE` / `ASSIGN` /
    /// `PROGRESS_AUTO` / `AI_PROGRESS`。
    ///
    /// **注意**：卡面把「人写的」写作 `COMMENT`，而库里（`V0108`）的类型值是
    /// `PLAIN`。以库为准：`PLAIN` 就是人类评论，`V0112` 的 CHECK 也只有这 6 个值。
    pub comment_type: String,
    /// 评论人 ID。
    pub user_id: String,
    /// 评论人姓名。
    pub user_name: String,
    /// 正文（Markdown 原文，前端渲染 + 净化）。
    pub content: String,
    /// 被 @ 的用户 ID 数组。
    pub mentions: Vec<String>,
    /// 回复的上级评论。
    pub parent_comment_id: Option<String>,
    /// 发表时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 是否系统生成（只读，不可编辑 / 删除）。
    pub readonly: bool,
    /// 当前用户能否删除（作者本人 / 管理员）。
    pub can_delete: bool,
}

/// 评论列表（`GET /business/task/{id}/comments`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskCommentsRes {
    /// 当前页评论（时间正序）。
    pub list: Vec<TaskCommentItemRes>,
    /// 未删评论总数。
    pub total: u64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 删除评论的回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskCommentDeleteRes {
    /// 评论 ID。
    pub id: String,
    /// 是否真的删掉了（软删；重复删除返回 `false` 且不报错之外的副作用）。
    pub deleted: bool,
}

/// 单条附件（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskAttachmentItemRes {
    /// 绑定行 ID。
    pub id: String,
    /// 文件 ID。
    pub file_id: String,
    /// 原始文件名。
    pub file_name: String,
    /// 大小（字节）。
    pub file_size: i64,
    /// MIME。
    pub mime_type: String,
    /// 来源：`HUMAN` / `AI`。
    pub source: String,
    /// 产出的 AI profile（仅 `AI` 有值）。
    pub ai_profile: Option<String>,
    /// 上传人 ID。
    pub uploader_id: Option<String>,
    /// 上传人姓名。
    pub uploader_name: String,
    /// 上传时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 当前用户能否删除（上传者 / 管理员）。
    pub can_delete: bool,
}

/// 附件列表（`GET /business/task/{id}/attachments`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskAttachmentsRes {
    /// 附件（未软删，按 `sort_order` 升序）。
    pub list: Vec<TaskAttachmentItemRes>,
    /// 当前数量。
    pub total: u64,
    /// 单任务上限（前端据此提示「还能传几个」）。
    pub max_count: i64,
}

/// 删除附件的回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskAttachmentDeleteRes {
    /// 绑定行 ID。
    pub id: String,
    /// 文件 ID（存储记录已同步软删）。
    pub file_id: String,
    /// 是否真的删掉了。
    pub deleted: bool,
}

/// 工时登记请求（`POST /business/task/{id}/worklogs`）。
///
/// **没有** `actual_hours` 字段：任务卡上的实际工时只由服务端 `SUM(hours)` 汇总
/// （卡面红线）。少一个字段就少一条造假路径。
#[derive(Debug, Clone, Deserialize)]
pub struct TaskWorklogCreateReq {
    /// 工时（小时，0.25–24）。
    pub hours: f64,
    /// 工时日期（`YYYY-MM-DD`，不得晚于今天）。
    pub work_date: String,
    /// 工作说明（可空；列名是 `description`，接口上用 `note` 更贴近前端文案）。
    #[serde(default, alias = "description")]
    pub note: Option<String>,
}

/// 单条工时（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskWorklogItemRes {
    /// 记录 ID。
    pub id: String,
    /// 填报人。
    pub user_id: String,
    /// 填报人姓名。
    pub user_name: String,
    /// 工时日期。
    pub work_date: NaiveDate,
    /// 工时（小时）。
    pub hours: f64,
    /// 工作说明。
    pub description: Option<String>,
    /// 记录时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 当前用户能否删除（本人 / 管理员）。
    pub can_delete: bool,
}

/// 工时列表（`GET /business/task/{id}/worklogs`）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskWorklogsRes {
    /// 明细（日期倒序）。
    pub list: Vec<TaskWorklogItemRes>,
    /// 记录条数。
    pub total: u64,
    /// 明细合计（`SUM(hours)`）。
    pub sum_hours: f64,
    /// 任务卡上的实际工时（服务端冗余；正常情况下 == `sum_hours`）。
    pub actual_hours: Option<f64>,
    /// 单任务记录上限。
    pub max_records: i64,
}

/// 登记工时的回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskWorklogSaveRes {
    /// 新记录 ID。
    pub id: String,
    /// 落库日期（归一后）。
    pub work_date: NaiveDate,
    /// 落库工时（按 `NUMERIC(5,1)` 归一后）。
    pub hours: f64,
    /// 明细合计。
    pub sum_hours: f64,
    /// 任务卡实际工时（**服务端汇总**）。
    pub actual_hours: Option<f64>,
}

/// 删除工时的回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskWorklogDeleteRes {
    /// 记录 ID。
    pub id: String,
    /// 是否删除成功。
    pub deleted: bool,
    /// 删除后的明细合计。
    pub sum_hours: f64,
    /// 删除后的任务卡实际工时。
    pub actual_hours: Option<f64>,
}

// ==================== T7a：甘特拖拽改期 ====================

/// 平移天数的绝对值上限（10 年）。
///
/// 不是业务规则，是**防误输**：前端算出来的 `shift_days` 不该超过一屏（180 天），
/// 但接口是公开契约，直接 curl 一个 `shift_days=999999` 会把日期推到公元 4799 年，
/// 随后的「不得早于 created_at 前 1 年」也拦不住它。上限取 10 年，够任何真实场景。
pub const MAX_SHIFT_DAYS: i64 = 3650;

/// 改期请求（`PATCH /api/business/task/{id}/schedule`）。
///
/// # 两种形态**二选一**（都传 / 都不传 → 400）
///
/// | 形态 | 字段 | 语义 |
/// |------|------|------|
/// | 整体平移 | `shift_days` | `start_date + N`、`due_date + N`（两个非空日期一起动） |
/// | 显式日期 | `start_date` / `due_date` | 只改传了的那个；两个都传就都改 |
///
/// 前端拖「条形中部」发 `shift_days`，拖「左边缘」发 `start_date`，拖「右边缘」发
/// `due_date` —— 三种拖法与三种请求形态一一对应，语义不会串。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskScheduleReq {
    /// 新的计划开始日（显式形态）。
    pub start_date: Option<NaiveDate>,
    /// 新的截止日（显式形态）。
    pub due_date: Option<NaiveDate>,
    /// 平移天数（平移形态；可负）。
    pub shift_days: Option<i64>,
}

impl TaskScheduleReq {
    /// 判定本次请求属于哪种形态。
    ///
    /// # 返回值
    ///
    /// - `Ok(ScheduleMode::Shift(n))`：`shift_days = n`（`n != 0`）；
    /// - `Ok(ScheduleMode::Explicit)`：至少传了一个显式日期；
    ///
    /// # 异常情况
    ///
    /// 返回 `Err(中文消息)`（handler 统一映射 `400 + 63004`）：
    /// - 两个形态同时出现（歧义：到底按哪个算）；
    /// - 一个都没传（空请求）；
    /// - `shift_days = 0`（等价于没改，前端应在拖动 0 格时根本不发请求）；
    /// - `|shift_days| > MAX_SHIFT_DAYS`。
    pub fn mode(&self) -> std::result::Result<ScheduleMode, String> {
        let has_explicit = self.start_date.is_some() || self.due_date.is_some();
        match (self.shift_days, has_explicit) {
            (Some(_), true) => Err(
                "shift_days 与 start_date / due_date 只能二选一（同时传无法判断以哪个为准）"
                    .to_string(),
            ),
            (None, false) => {
                Err("请求体必须给 shift_days 或 start_date / due_date 之一".to_string())
            }
            (None, true) => Ok(ScheduleMode::Explicit),
            (Some(days), false) => {
                if days == 0 {
                    return Err(
                        "shift_days = 0 等于没改期（前端应在拖动 0 格时不发请求）".to_string()
                    );
                }
                if days.abs() > MAX_SHIFT_DAYS {
                    return Err(format!(
                        "shift_days 超出范围：{days}（绝对值上限 {MAX_SHIFT_DAYS} 天）"
                    ));
                }
                Ok(ScheduleMode::Shift(days))
            }
        }
    }
}

/// 改期请求的两种形态（见 [`TaskScheduleReq::mode`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleMode {
    /// 整体平移 N 天。
    Shift(i64),
    /// 显式日期（只改传了的那些）。
    Explicit,
}

/// 改期结果（`PATCH /business/task/{id}/schedule`）。
///
/// 只回「改期前 / 改期后 + 留痕 id」，不回整张任务卡：甘特图手上的其余字段没变，
/// 回全量反而让前端拿旧快照覆盖新快照（T7b 的回弹逻辑也依赖「只信这一条的日期」）。
#[derive(Debug, Clone, Serialize)]
pub struct TaskScheduleRes {
    /// 任务 id。
    pub id: String,
    /// 任务编号。
    pub task_no: String,
    /// 改期前的计划开始日。
    pub old_start_date: Option<NaiveDate>,
    /// 改期前的截止日。
    pub old_due_date: Option<NaiveDate>,
    /// 改期后的计划开始日（**贴库值**）。
    pub start_date: Option<NaiveDate>,
    /// 改期后的截止日（**贴库值**）。
    pub due_date: Option<NaiveDate>,
    /// 本次实际平移天数（显式形态下按开始日位移算；无法算出时为 0）。
    pub shift_days: i64,
    /// `SCHEDULE` 评论 id（时间线上点得进去）。
    pub comment_id: String,
    /// 变更日志是否落库（`false` = diff 为空被跳过，理论上不会发生）。
    pub changelog_written: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_and_status_are_whitelisted() {
        assert_eq!(normalize_priority(Some("p1")).as_deref(), Some("P1"));
        assert_eq!(normalize_priority(Some(" P3 ")).as_deref(), Some("P3"));
        assert!(normalize_priority(Some("P4")).is_none());
        assert!(normalize_priority(Some("")).is_none());
        assert!(normalize_priority(None).is_none());

        assert_eq!(normalize_status("doing").as_deref(), Some("DOING"));
        assert!(normalize_status("ARCHIVED").is_none());
        assert_eq!(STATUSES.len(), 5);
        assert_eq!(PRIORITIES.len(), 4);
    }

    #[test]
    fn truncate_counts_chars_not_bytes() {
        // 中文标题按 char 截断：200 个汉字必须原样保留（按字节会砍成 66 个）
        let title = "任".repeat(MAX_TITLE_CHARS);
        assert_eq!(truncate_chars(&title, MAX_TITLE_CHARS).chars().count(), 200);
        let longer = "任".repeat(MAX_TITLE_CHARS + 10);
        assert_eq!(
            truncate_chars(&longer, MAX_TITLE_CHARS).chars().count(),
            200
        );
    }

    #[test]
    fn update_req_distinguishes_absent_and_null() {
        // 缺席：外层 None（保持原值）
        let absent: TaskUpdateReq = serde_json::from_str(r#"{"title":"改名"}"#).expect("合法 JSON");
        assert_eq!(absent.title.as_deref(), Some("改名"));
        assert!(absent.due_date.is_none(), "缺省字段必须是 None（保持原值）");

        // 显式 null：Some(None)（清空）
        let cleared: TaskUpdateReq =
            serde_json::from_str(r#"{"due_date":null}"#).expect("合法 JSON");
        assert_eq!(cleared.due_date, Some(None), "显式 null 必须表达清空");

        // 有值：Some(Some(..))
        let set: TaskUpdateReq =
            serde_json::from_str(r#"{"due_date":"2026-11-06"}"#).expect("合法 JSON");
        assert_eq!(
            set.due_date,
            Some(Some(
                NaiveDate::from_ymd_opt(2026, 11, 6).expect("合法日期")
            ))
        );
    }

    #[test]
    fn empty_update_is_detected() {
        let empty: TaskUpdateReq = serde_json::from_str("{}").expect("合法 JSON");
        assert!(empty.is_empty());
        let one: TaskUpdateReq = serde_json::from_str(r#"{"priority":"P0"}"#).expect("合法 JSON");
        assert!(!one.is_empty());
    }

    #[test]
    fn keyword_is_collapsed() {
        let query = TaskPageQuery {
            keyword: Some("  订单   审批  ".to_string()),
            ..TaskPageQuery::default()
        };
        assert_eq!(query.normalized_keyword().as_deref(), Some("订单 审批"));
        let blank = TaskPageQuery {
            keyword: Some("   ".to_string()),
            ..TaskPageQuery::default()
        };
        assert!(blank.normalized_keyword().is_none());
    }

    /// 改状态请求同时接受 `to` 与前端草稿里的 `status`（别名），`to` 优先。
    ///
    /// 这条测试守的是「卡片写的 `{to}` ↔ 前端任务书草稿写的 `{status}`」这个联调缝隙：
    /// 两个名字都收，前端怎么写都不会 400。
    #[test]
    fn status_change_req_accepts_both_to_and_status() {
        let canonical: TaskStatusChangeReq =
            serde_json::from_str(r#"{"to":"DOING","reason":"开始做"}"#).expect("合法 JSON");
        assert_eq!(canonical.to, "DOING");
        assert_eq!(canonical.reason.as_deref(), Some("开始做"));

        let aliased: TaskStatusChangeReq =
            serde_json::from_str(r#"{"status":"REVIEW"}"#).expect("别名也应被接受");
        assert_eq!(aliased.to, "REVIEW");
        assert!(aliased.reason.is_none());

        let both = serde_json::from_str::<TaskStatusChangeReq>(r#"{"to":"DOING","status":"DONE"}"#);
        assert!(
            both.is_err(),
            "同时传 to 与 status 是歧义请求：serde 判 duplicate field（400），不猜用户意图"
        );

        let reject_alias: TaskStatusChangeReq =
            serde_json::from_str(r#"{"to":"DOING","reject_reason":"单测没跑"}"#)
                .expect("打回原因别名");
        assert_eq!(reject_alias.reason.as_deref(), Some("单测没跑"));
    }
}

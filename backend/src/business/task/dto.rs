//! 工作任务管理 · DTO（请求 / 响应契约）。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md` §二（状态机取值）、§三（进度）、
//! §七（可见性）、§十（两秒建任务 → 创建默认值）、§十一（页面出参需要哪些字段）。
//!
//! # 命名与格式约定（与批次 A/B/C 一致，前端不为任务管理单独适配）
//!
//! - 主键 / 外键一律 `String`（`alun::uuid::Uuid` 没有 serde，序列化不了）；
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

use alun::prelude::chrono::{self, NaiveDate};
use serde::{Deserialize, Deserializer, Serialize};

// ==================== 常量与枚举取值 ====================

/// 标题最大字符数（与 `biz_task.title VARCHAR(200)` 对齐）。
pub const MAX_TITLE_CHARS: usize = 200;
/// 描述最大字符数（TEXT 列，但接口层限长，避免一次请求塞进 10 MB）。
pub const MAX_DESCRIPTION_CHARS: usize = 20_000;

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
    pub status: Option<String>,
    /// 优先级过滤（`P0`–`P3`）。
    pub priority: Option<String>,
    /// 负责人过滤（用户 UUID）。
    pub owner_id: Option<String>,
    /// 截止日下界（含）。
    pub due_from: Option<NaiveDate>,
    /// 截止日上界（含）。
    pub due_to: Option<NaiveDate>,
    /// 关键词（标题 / 描述模糊，ILIKE）。
    pub keyword: Option<String>,
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
    /// 父任务（可空；深度限 2 层）。
    pub parent_id: Option<String>,
    /// 所属项目（可空，P1）。
    pub project_id: Option<String>,
    /// 计划开始日（可空）。
    pub start_date: Option<NaiveDate>,
    /// 截止日（缺省 = 本周五，CST）。
    pub due_date: Option<NaiveDate>,
    /// 预估工时（小时，可空）。
    pub estimate_hours: Option<f64>,
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
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 最后更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
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
    /// 所属项目 ID。
    pub project_id: Option<String>,
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
    /// 子任务列表（**已按调用者的可见性过滤**，见 `model::list_children`）。
    pub children: Vec<TaskCardRes>,
    /// 协作人 / 关注者。
    pub members: Vec<TaskMemberRes>,
    /// 附件。
    pub attachments: Vec<TaskAttachmentRes>,
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

// ==================== 单元测试（纯逻辑，不连库） ====================

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
}

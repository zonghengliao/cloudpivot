//! 项目分组 · DTO（请求 / 响应契约）· 批次 C+ · T7c。
//!
//! # 字段映射（与卡面逐字对齐，库列名不同处在此说明）
//!
//! | 卡面字段 | 库列 | 说明 |
//! |----------|------|------|
//! | `code` | `biz_project.project_no` | V0100 建表时就叫 `project_no`（「项目编号」），不新增列 |
//! | `name` | `biz_project.project_name` | 同上，避免同义两列 |
//! | `due_date` | `biz_project.end_date` | 项目用「起止日期」，语义一致 |
//! | `status` | `biz_project.status` | `ACTIVE` / `ARCHIVED`（V0100 的 CHECK 取值） |
//!
//! # 命名与格式约定（与任务 / 公告 / 通知模块一致，前端不为项目单独适配）
//!
//! - 主键 / 外键一律 `String`（`Uuid` 没有 serde）；
//! - 日期 `chrono::NaiveDate`（JSON `"2026-10-30"`），时间戳 RFC3339；
//! - 枚举大写字符串（`ACTIVE` / `MEMBER`），与库里的 CHECK 逐字一致。
//!
//! # 编辑接口的「字段缺席 ≠ 清空」
//!
//! 可清空字段（描述 / 负责人 / 起止日期）用 `Option<Option<T>>`：
//! 缺席 = 保持原值，显式 `null` = 清空，有值 = 赋值。这条规则在公告上踩过坑
//! （漏传 = 清空会把截止日默默抹掉），项目从一开始就用双重 Option。

use alun::prelude::chrono::{self, NaiveDate};
use serde::{Deserialize, Deserializer, Serialize};

// ==================== 常量 ====================

/// 名称最大字符数（与 `biz_project.project_name VARCHAR(100)` 对齐）。
pub const MAX_NAME_CHARS: usize = 100;
/// 编码最大字符数（与 `biz_project.project_no VARCHAR(32)` 对齐）。
pub const MAX_CODE_CHARS: usize = 32;
/// 编码最小字符数。
pub const MIN_CODE_CHARS: usize = 2;
/// 说明最大字符数（TEXT 列，但接口层限长）。
pub const MAX_DESCRIPTION_CHARS: usize = 20_000;

/// 状态：进行中。
pub const STATUS_ACTIVE: &str = "ACTIVE";
/// 状态：已归档。
pub const STATUS_ARCHIVED: &str = "ARCHIVED";
/// 全部状态（入库 / 出参顺序）。
pub const STATUSES: [&str; 2] = [STATUS_ACTIVE, STATUS_ARCHIVED];

/// 成员角色：成员。
pub const MEMBER_ROLE_MEMBER: &str = "MEMBER";
/// 成员角色：只读。
pub const MEMBER_ROLE_VIEWER: &str = "VIEWER";
/// 全部成员角色。
pub const MEMBER_ROLES: [&str; 2] = [MEMBER_ROLE_MEMBER, MEMBER_ROLE_VIEWER];

// ==================== 归一化（纯函数，可单测） ====================

/// 编码归一化：折叠空白 → 转大写 → 校验 `^[A-Z0-9_-]{2,32}$`。
///
/// 为什么强制大写：编码唯一性是按字面比较的，若允许 `proj-1` 与 `PROJ-1` 并存，
/// 用户会觉得「明明重名了却没拦住」。统一大写后库里的值就是最终形态，
/// 前端也不用再纠结大小写。
///
/// # 错误
///
/// 返回中文原因（handler 直接拼进 `400 + 63403`）。
pub fn normalize_code(raw: &str) -> std::result::Result<String, String> {
    let collapsed: String = raw
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_uppercase();
    if collapsed.chars().count() < MIN_CODE_CHARS {
        return Err(format!("项目编码至少要 {MIN_CODE_CHARS} 个字符"));
    }
    if collapsed.chars().count() > MAX_CODE_CHARS {
        return Err(format!("项目编码不能超过 {MAX_CODE_CHARS} 个字符"));
    }
    if !collapsed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err("项目编码只能包含字母、数字、下划线和短横线".to_string());
    }
    Ok(collapsed)
}

/// 名称归一化：折叠行内空白 → 非空 → ≤100 字符。
///
/// # 错误
///
/// 返回中文原因。
pub fn normalize_name(raw: &str) -> std::result::Result<String, String> {
    let collapsed = collapse_whitespace(raw);
    if collapsed.is_empty() {
        return Err("项目名称不能为空".to_string());
    }
    if collapsed.chars().count() > MAX_NAME_CHARS {
        return Err(format!("项目名称不能超过 {MAX_NAME_CHARS} 个字符"));
    }
    Ok(collapsed)
}

/// 说明归一化：折叠空白，空串 → `None`；超长 → 错误。
///
/// # 错误
///
/// 超长时返回中文原因。
pub fn normalize_description(raw: Option<&str>) -> std::result::Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!("项目说明不能超过 {MAX_DESCRIPTION_CHARS} 个字符"));
    }
    Ok(Some(trimmed.to_string()))
}

/// 状态归一化（大小写不敏感；非法值 → `None`）。
pub fn normalize_status(raw: &str) -> Option<String> {
    let upper = raw.trim().to_uppercase();
    STATUSES.contains(&upper.as_str()).then_some(upper)
}

/// 成员角色归一化（缺省 `MEMBER`；非法值 → `None`）。
pub fn normalize_member_role(raw: Option<&str>) -> Option<String> {
    match raw {
        None => Some(MEMBER_ROLE_MEMBER.to_string()),
        Some(text) if text.trim().is_empty() => Some(MEMBER_ROLE_MEMBER.to_string()),
        Some(text) => {
            let upper = text.trim().to_uppercase();
            MEMBER_ROLES.contains(&upper.as_str()).then_some(upper)
        }
    }
}

/// 折叠连续空白（含中文全角空格）成单个半角空格并 trim。
pub fn collapse_whitespace(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out.trim_end().to_string()
}

/// 双重 Option 反序列化：字段缺席 → `None`，显式 `null` → `Some(None)`，有值 → `Some(Some(v))`。
fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// ==================== 请求 ====================

/// 项目分页（`GET /business/project/page`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectPageQuery {
    /// 页码（从 1 开始）。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 名称 / 编码关键词（ILIKE）。
    pub name: Option<String>,
    /// 状态（`ACTIVE` / `ARCHIVED`）。
    pub status: Option<String>,
    /// 负责人（用户 UUID）。
    pub owner_id: Option<String>,
}

impl ProjectPageQuery {
    /// 归一化关键词：折叠空白 + 去空 → `None`。
    pub fn normalized_name(&self) -> Option<String> {
        let raw = self.name.as_deref()?;
        let collapsed = collapse_whitespace(raw);
        (!collapsed.is_empty()).then_some(collapsed)
    }
}

/// 新建项目（`POST /business/project`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectCreateReq {
    /// 名称（必填，1–100 字符）。
    pub name: String,
    /// 编码（必填，2–32 字符，`[A-Z0-9_-]`，未删唯一）。
    pub code: String,
    /// 说明（可空）。
    pub description: Option<String>,
    /// 负责人（缺省 = 创建人自己）。
    pub owner_id: Option<String>,
    /// 开始日（可空）。
    pub start_date: Option<NaiveDate>,
    /// 结束日（可空）。
    pub due_date: Option<NaiveDate>,
}

/// 编辑项目（`PUT /business/project/{id}`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectUpdateReq {
    /// 名称（缺席 = 不改）。
    pub name: Option<String>,
    /// 编码（缺席 = 不改）。
    pub code: Option<String>,
    /// 说明（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    /// 负责人（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub owner_id: Option<Option<String>>,
    /// 状态（缺席 = 不改；`ACTIVE` / `ARCHIVED`）。
    pub status: Option<String>,
    /// 开始日（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub start_date: Option<Option<NaiveDate>>,
    /// 结束日（缺席 = 不改；`null` = 清空）。
    #[serde(default, deserialize_with = "double_option")]
    pub due_date: Option<Option<NaiveDate>>,
}

impl ProjectUpdateReq {
    /// 是否一个可改字段都没传（全缺席 → 400，避免「空 PUT 假成功」）。
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.code.is_none()
            && self.description.is_none()
            && self.owner_id.is_none()
            && self.status.is_none()
            && self.start_date.is_none()
            && self.due_date.is_none()
    }
}

/// 加项目成员（`POST /business/project/{id}/members`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProjectMemberAddReq {
    /// 成员用户 id。
    pub user_id: String,
    /// 角色（缺省 `MEMBER`；`MEMBER` / `VIEWER`）。
    pub role: Option<String>,
}

/// 任务归入 / 移出项目（`PUT /business/task/{id}/project`）。
///
/// `{ "project_id": "<uuid>" }` 归入；`{ "project_id": null }` 移出。
/// **字段缺席 → 400**（分不清「没传」和「清空」，必须显式表达意图）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TaskProjectReq {
    /// 目标项目（`null` = 移出项目）。
    #[serde(default, deserialize_with = "double_option")]
    pub project_id: Option<Option<String>>,
}

// ==================== 响应 ====================

/// 项目（列表 / 详情头部共用）。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectRes {
    /// 主键。
    pub id: String,
    /// 编码（库列 `project_no`）。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 说明（可空文本列降级空串）。
    pub description: String,
    /// 负责人（可空）。
    pub owner_id: Option<String>,
    /// 负责人姓名（空串 = 未指定）。
    pub owner_name: String,
    /// 归属部门（可空）。
    pub dept_id: Option<String>,
    /// 归属部门名（空串 = 无部门）。
    pub dept_name: String,
    /// 状态（`ACTIVE` / `ARCHIVED`）。
    pub status: String,
    /// 开始日。
    pub start_date: Option<NaiveDate>,
    /// 结束日（库列 `end_date`）。
    pub due_date: Option<NaiveDate>,
    /// **我可见的**任务总数（与 T1 可见性同口径）。
    pub task_total: i64,
    /// **我可见的**已完成任务数。
    pub task_done: i64,
    /// 完成率（0–100，1 位小数）。
    pub completion_rate: f64,
    /// 成员数。
    pub member_count: i64,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 项目分页出参（`GET /business/project/page`）。
///
/// 前四个字段与框架 `PageData<T>` 逐字一致（前端 `request.ts` 的 `PageData<T>` 直接可用）。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectPageRes {
    /// 当前页。
    pub list: Vec<ProjectRes>,
    /// 可见总数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 单个项目的任务统计（详情出参）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProjectStatsRes {
    /// 任务总数（我可见）。
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
    /// 完成率（0–100，1 位小数；分母剔除已取消）。
    pub completion_rate: f64,
    /// 总预估工时（小时）。
    pub estimate_hours: f64,
    /// 总实际工时（小时）。
    pub actual_hours: f64,
}

/// 项目成员。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectMemberRes {
    /// 用户 id。
    pub user_id: String,
    /// 账号。
    pub username: String,
    /// 姓名（可空 → 空串）。
    pub real_name: String,
    /// 角色（`MEMBER` / `VIEWER`）。
    pub role: String,
    /// 加入时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// 项目详情（`GET /business/project/{id}`）：头部字段 + 统计 + 成员。
///
/// 用 `flatten` 把 [`ProjectRes`] 摊平进顶层 —— 前端拿到的形状与列表项**逐字段相同**，
/// 只是多了 `stats` / `members`，不用写两套类型。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDetailRes {
    /// 项目头部字段（摊平）。
    #[serde(flatten)]
    pub project: ProjectRes,
    /// 任务统计（按 T1 可见性裁剪）。
    pub stats: ProjectStatsRes,
    /// 成员列表。
    pub members: Vec<ProjectMemberRes>,
}

/// 写操作回执（新建 / 编辑 / 归入 / 移出）。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSaveRes {
    /// 主键。
    pub id: String,
    /// 编码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 状态。
    pub status: String,
}

/// 删除项目回执。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectDeleteRes {
    /// 恒 `true`（项目不存在时直接 404，不静默成功）。
    pub deleted: bool,
    /// **被解绑的任务数**（任务本身没删，只把 `project_id` 置空）。
    pub unbound_tasks: u64,
    /// 被清掉的成员行数。
    pub removed_members: u64,
}

/// 任务归入 / 移出回执。
#[derive(Debug, Clone, Serialize)]
pub struct TaskProjectRes {
    /// 任务 id。
    pub task_id: String,
    /// 归入后所属项目（`null` = 已移出）。
    pub project_id: Option<String>,
    /// 项目编码（`null` = 无项目）。
    pub project_code: Option<String>,
    /// 项目名称（`null` = 无项目）。
    pub project_name: Option<String>,
}

/// 成员增删回执。
#[derive(Debug, Clone, Serialize)]
pub struct ProjectMemberChangeRes {
    /// `false` = 幂等命中（已经是成员 / 本来就不是成员）。
    pub changed: bool,
    /// 变更后的完整成员列表。
    pub members: Vec<ProjectMemberRes>,
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 编码：小写自动转大写、空白被折叠掉。
    #[test]
    fn normalize_code_uppercases_and_strips_space() {
        assert_eq!(normalize_code(" pay-2026 ").as_deref(), Ok("PAY-2026"));
        assert_eq!(normalize_code("a b_c").as_deref(), Ok("AB_C"));
    }

    /// 编码：长度与字符集校验。
    #[test]
    fn normalize_code_rejects_invalid() {
        assert!(normalize_code("x").is_err(), "1 个字符太短");
        assert!(normalize_code("项目一").is_err(), "中文不允许");
        assert!(normalize_code("a/b").is_err(), "斜杠不允许");
        assert!(normalize_code("").is_err());
        assert!(
            normalize_code(&"A".repeat(MAX_CODE_CHARS + 1)).is_err(),
            "超长必须拒绝"
        );
        assert!(
            normalize_code(&"A".repeat(MAX_CODE_CHARS)).is_ok(),
            "边界长度应通过"
        );
    }

    /// 名称：折叠空白、拒绝空、拒绝超长。
    #[test]
    fn normalize_name_rules() {
        assert_eq!(normalize_name("  支付  中台 ").as_deref(), Ok("支付 中台"));
        assert!(normalize_name("   ").is_err());
        assert!(normalize_name(&"名".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(normalize_name(&"名".repeat(MAX_NAME_CHARS)).is_ok());
    }

    /// 说明：空串降级成 None，超长报错。
    #[test]
    fn normalize_description_rules() {
        assert_eq!(normalize_description(None), Ok(None));
        assert_eq!(normalize_description(Some("   ")), Ok(None));
        assert_eq!(
            normalize_description(Some("  hi ")),
            Ok(Some("hi".to_string()))
        );
        assert!(normalize_description(Some(&"x".repeat(MAX_DESCRIPTION_CHARS + 1))).is_err());
    }

    /// 状态：大小写不敏感，非法值返回 None（handler 转 400）。
    #[test]
    fn normalize_status_rules() {
        assert_eq!(normalize_status("active").as_deref(), Some("ACTIVE"));
        assert_eq!(normalize_status(" ARCHIVED ").as_deref(), Some("ARCHIVED"));
        assert_eq!(normalize_status("DONE"), None);
    }

    /// 成员角色：缺省 MEMBER，非法值 None。
    #[test]
    fn normalize_member_role_rules() {
        assert_eq!(
            normalize_member_role(None).as_deref(),
            Some(MEMBER_ROLE_MEMBER)
        );
        assert_eq!(
            normalize_member_role(Some(" viewer ")).as_deref(),
            Some(MEMBER_ROLE_VIEWER)
        );
        assert_eq!(normalize_member_role(Some("OWNER")), None);
    }

    /// 分页关键词：空白折叠后为空 → None（否则 `ILIKE '%%'` 等于不过滤）。
    #[test]
    fn page_query_keyword_is_optional() {
        let mut q = ProjectPageQuery::default();
        assert_eq!(q.normalized_name(), None);
        q.name = Some("   ".to_string());
        assert_eq!(q.normalized_name(), None);
        q.name = Some("  支付  中台 ".to_string());
        assert_eq!(q.normalized_name().as_deref(), Some("支付 中台"));
    }

    /// 编辑请求：全缺席才是空（空 PUT 要 400）。
    #[test]
    fn update_req_is_empty_only_when_nothing_present() {
        assert!(ProjectUpdateReq::default().is_empty());
        let req = ProjectUpdateReq {
            description: Some(None),
            ..ProjectUpdateReq::default()
        };
        assert!(!req.is_empty(), "显式清空描述也算一次有效修改");
    }

    /// 双重 Option 的三种 JSON 形态：缺席 / null / 有值。
    #[test]
    fn update_req_distinguishes_absent_and_null() {
        let absent: ProjectUpdateReq =
            serde_json::from_str(r#"{"name":"改名"}"#).expect("JSON 合法");
        assert_eq!(absent.description, None, "缺席 = 不改");
        assert_eq!(absent.owner_id, None);

        let cleared: ProjectUpdateReq =
            serde_json::from_str(r#"{"description":null,"owner_id":null}"#).expect("JSON 合法");
        assert_eq!(cleared.description, Some(None), "显式 null = 清空");
        assert_eq!(cleared.owner_id, Some(None));

        let valued: ProjectUpdateReq =
            serde_json::from_str(r#"{"description":"有值","status":"ARCHIVED"}"#)
                .expect("JSON 合法");
        assert_eq!(valued.description, Some(Some("有值".to_string())));
        assert_eq!(valued.status.as_deref(), Some("ARCHIVED"));
    }

    /// 任务归入接口：缺席 → `None`（handler 转 400）；`null` → `Some(None)`（移出）。
    #[test]
    fn task_project_req_distinguishes_absent_and_null() {
        let absent: TaskProjectReq = serde_json::from_str("{}").expect("JSON 合法");
        assert!(absent.project_id.is_none(), "缺席必须与 null 区分开");

        let removed: TaskProjectReq =
            serde_json::from_str(r#"{"project_id":null}"#).expect("JSON 合法");
        assert_eq!(removed.project_id, Some(None));

        let assigned: TaskProjectReq =
            serde_json::from_str(r#"{"project_id":"01920000-0000-7000-8000-000000000001"}"#)
                .expect("JSON 合法");
        assert!(matches!(assigned.project_id, Some(Some(_))));
    }
}

//! 消息通知的请求 / 响应 DTO 与领域枚举。
//!
//! # 为什么枚举放这里
//!
//! `NoticeType` / `NoticeLevel` / `ContentFormat` 都直接出现在 API 入参与出参上，
//! 且需要「字符串 ⇄ 枚举」的严格转换（数据库侧是 `VARCHAR` + CHECK 约束）。
//! 放 DTO 层可让 `render.rs` / `fanout.rs` 共用而不产生循环依赖：
//! `dto` 只依赖 `model`，`render` 依赖 `dto`，`handler` 依赖全部。
//!
//! # 可空文本列一律降级空串
//!
//! 与底座约定一致：`*_Res` 里可空文本列下发空串（前端不判 null），可空时间保持 `Option`。

use nik::prelude::*;
use serde::{Deserialize, Serialize};
use validator::Validate;

use crate::shared::constants::EnableStatus;
use crate::shared::error_code;

use crate::business::notice::model::{DeliverySummaryRow, MyNoticeRow, SysNoticeModel, SysNoticeTemplateModel};

// ==================== 领域枚举 ====================

/// 通知类型（对应 `sys_notice.notice_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NoticeType {
    /// 系统通知（导入导出完成等）。
    System,
    /// 待办提醒（工作流）。
    Todo,
    /// 告警。
    Alert,
    /// 流程消息（审批结果）。
    Workflow,
    /// 公告。
    Circular,
}

impl NoticeType {
    /// 数据库字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            NoticeType::System => "SYSTEM",
            NoticeType::Todo => "TODO",
            NoticeType::Alert => "ALERT",
            NoticeType::Workflow => "WORKFLOW",
            NoticeType::Circular => "CIRCULAR",
        }
    }

    /// 解析数据库字面量（大小写不敏感，前后空白忽略）。
    ///
    /// # 异常情况
    ///
    /// 未知取值 → `400 + 10002`。
    pub fn parse(raw: &str) -> std::result::Result<Self, ApiError> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "SYSTEM" => Ok(NoticeType::System),
            "TODO" => Ok(NoticeType::Todo),
            "ALERT" => Ok(NoticeType::Alert),
            "WORKFLOW" => Ok(NoticeType::Workflow),
            "CIRCULAR" => Ok(NoticeType::Circular),
            other => Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("通知类型非法：{other}（允许 SYSTEM/TODO/ALERT/WORKFLOW/CIRCULAR）"),
            )),
        }
    }
}

/// 通知级别（对应 `sys_notice.level`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NoticeLevel {
    /// 普通信息。
    Info,
    /// 成功。
    Success,
    /// 警告。
    Warn,
    /// 错误。
    Error,
}

impl NoticeLevel {
    /// 数据库字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            NoticeLevel::Info => "INFO",
            NoticeLevel::Success => "SUCCESS",
            NoticeLevel::Warn => "WARN",
            NoticeLevel::Error => "ERROR",
        }
    }

    /// 解析数据库字面量（大小写不敏感）。
    ///
    /// # 异常情况
    ///
    /// 未知取值 → `400 + 10002`。
    pub fn parse(raw: &str) -> std::result::Result<Self, ApiError> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "INFO" => Ok(NoticeLevel::Info),
            "SUCCESS" => Ok(NoticeLevel::Success),
            "WARN" => Ok(NoticeLevel::Warn),
            "ERROR" => Ok(NoticeLevel::Error),
            other => Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("通知级别非法：{other}（允许 INFO/SUCCESS/WARN/ERROR）"),
            )),
        }
    }
}

/// 正文格式（对应 `content_format`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ContentFormat {
    /// 纯文本。
    Text,
    /// HTML（渲染前净化骨架、变量值转义）。
    Html,
    /// Markdown（本批不渲染，按文本存）。
    Markdown,
}

impl ContentFormat {
    /// 数据库字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            ContentFormat::Text => "TEXT",
            ContentFormat::Html => "HTML",
            ContentFormat::Markdown => "MARKDOWN",
        }
    }

    /// 解析数据库字面量（大小写不敏感）。
    ///
    /// # 异常情况
    ///
    /// 未知取值 → `400 + 10002`。
    pub fn parse(raw: &str) -> std::result::Result<Self, ApiError> {
        match raw.trim().to_ascii_uppercase().as_str() {
            "TEXT" => Ok(ContentFormat::Text),
            "HTML" => Ok(ContentFormat::Html),
            "MARKDOWN" => Ok(ContentFormat::Markdown),
            other => Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("正文格式非法：{other}（允许 TEXT/HTML/MARKDOWN）"),
            )),
        }
    }

    /// 渲染时是否需要对变量值做 HTML 转义（仅 `HTML`）。
    pub fn escape_values(self) -> bool {
        matches!(self, ContentFormat::Html)
    }
}

// ==================== 通道位掩码 ====================

/// 站内信（必做通道）。
pub const CHANNEL_INBOX: u16 = 1;
/// 邮件（本批只落投递记录）。
pub const CHANNEL_EMAIL: u16 = 2;
/// Webhook（本批只落投递记录）。
pub const CHANNEL_WEBHOOK: u16 = 4;
/// 短信（本批只落投递记录）。
pub const CHANNEL_SMS: u16 = 8;
/// 合法掩码上限（`ck_sys_notice_channel` 同样限制 1~15）。
pub const CHANNEL_MAX: u16 = 15;

/// 掩码 → 通道名列表（顺序固定：INBOX / EMAIL / WEBHOOK / SMS）。
pub fn channel_names(mask: u16) -> Vec<&'static str> {
    let mut names = Vec::new();
    if mask & CHANNEL_INBOX != 0 {
        names.push("INBOX");
    }
    if mask & CHANNEL_EMAIL != 0 {
        names.push("EMAIL");
    }
    if mask & CHANNEL_WEBHOOK != 0 {
        names.push("WEBHOOK");
    }
    if mask & CHANNEL_SMS != 0 {
        names.push("SMS");
    }
    names
}

/// 逗号分隔的通道名（模板 `channels` 列）→ 掩码。
///
/// 未识别的名字忽略；`INBOX` 恒置位（站内信是本批唯一真实通道，模板不可能关掉它）。
pub fn channel_mask_of_csv(raw: &str) -> u16 {
    let mut mask = CHANNEL_INBOX;
    for name in raw.split(',') {
        match name.trim().to_ascii_uppercase().as_str() {
            "INBOX" => mask |= CHANNEL_INBOX,
            "EMAIL" => mask |= CHANNEL_EMAIL,
            "WEBHOOK" => mask |= CHANNEL_WEBHOOK,
            "SMS" => mask |= CHANNEL_SMS,
            _ => {}
        }
    }
    mask
}

/// 校验掩码合法（1~15）。
///
/// # 异常情况
///
/// 越界 → `400 + 10002`（数据库 CHECK 同样会拦，应用层提前给出中文文案）。
pub fn ensure_channel_mask(mask: u16) -> std::result::Result<(), ApiError> {
    if mask == 0 || mask > CHANNEL_MAX {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("通道掩码非法：{mask}（允许 1~15，1 表示站内信）"),
        ));
    }
    Ok(())
}

// ==================== 模板变量声明 ====================

/// 模板变量声明（`sys_notice_template.variables` 数组元素）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVarDecl {
    /// 变量名（与模板里的 `{{name}}` 逐字一致）。
    pub name: String,
    /// 展示用标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 是否必填（仅用于编辑页提示；渲染时一律必须提供）。
    #[serde(default)]
    pub required: bool,
    /// 示例值（保存模板时用它试渲染）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
}

/// 解析 `variables` JSON 列（容忍 `null` / 非数组 / 脏数据）。
///
/// # 参数
///
/// - `raw`：数据库列的文本形式（`variables::text`）；`None` 视为空声明。
///
/// # 返回值
///
/// 变量声明列表；无法解析或元素不合法时返回空列表（模板仍可展示，只是编辑页没有提示）。
pub fn parse_var_decls(raw: Option<&str>) -> Vec<TemplateVarDecl> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| serde_json::from_value::<TemplateVarDecl>(item.clone()).ok())
        .filter(|decl| !decl.name.trim().is_empty())
        .collect()
}

// ==================== 请求 DTO ====================

/// 发送通知请求（管理端手工 / 业务模块转发）。
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct NoticeSendReq {
    /// 通知类型（缺省 `SYSTEM`）。
    pub notice_type: Option<String>,
    /// 级别（缺省 `INFO`；用模板时以模板 `default_level` 兜底）。
    pub level: Option<String>,
    /// 标题（不使用模板时必填）。
    #[validate(length(max = 255, message = "标题不能超过 255 个字符"))]
    pub title: Option<String>,
    /// 正文（不使用模板时必填）。
    pub content: Option<String>,
    /// 正文格式（缺省 `TEXT`）。
    pub content_format: Option<String>,
    /// 跳转链接。
    #[validate(length(max = 500, message = "链接不能超过 500 个字符"))]
    pub link_url: Option<String>,
    /// 业务类型（如 `workflow` / `announcement`）。
    #[validate(length(max = 64, message = "业务类型不能超过 64 个字符"))]
    pub biz_type: Option<String>,
    /// 业务主键。
    #[validate(length(max = 64, message = "业务主键不能超过 64 个字符"))]
    pub biz_id: Option<String>,
    /// 模板编码（给了就走模板渲染，`title` / `content` 忽略）。
    #[validate(length(max = 64, message = "模板编码不能超过 64 个字符"))]
    pub template_code: Option<String>,
    /// 模板变量。
    pub template_vars: Option<serde_json::Value>,
    /// 目标类型：`USER` / `ROLE` / `DEPT` / `ALL`。
    pub target_type: String,
    /// 目标 ID 列表（`ALL` 时忽略）。
    pub target_ids: Option<Vec<String>>,
    /// 通道掩码（缺省 1 = 仅站内信）。
    pub channel_mask: Option<u16>,
    /// 幂等键（同一键只发一次）。
    #[validate(length(max = 128, message = "幂等键不能超过 128 个字符"))]
    pub idempotency_key: Option<String>,
    /// 过期时间（过期后由清理任务硬删）。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 计划发送时间（V0051）：未来时间 → 定时发送（到点前用户侧不可见）；
    /// 空或已过 → 立即发送。
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// SSE 订阅的查询参数（A6 / 遗留修复⑥定口径）。
///
/// # 允许什么
///
/// - `token`：浏览器 `EventSource` 无法设置 `Authorization` 头时的**唯一**降级通道；
///   只有 `NIK_AUTH_QUERY_TOKEN_PATHS` 白名单里的端点才会被认证层采信（默认空 = 关闭）。
///
/// # 禁止什么（显式拒绝而不是静默忽略）
///
/// - `user_id` / `receiver_id`：任何「客户端指定频道归属」的参数一律 400。
///   频道归属**只**来自令牌声明（`AuthClaims`），否则 A 用户塞一个 `?user_id=B`
///   就能订阅 B 的通知 —— 这是 A6 验收里明确要拒的攻击面。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StreamQuery {
    /// 访问令牌（仅 EventSource 降级场景；等价于 `Authorization: Bearer`）。
    pub token: Option<String>,
    /// 试图指定用户（**应被拒**，仅用于给出明确错误信息）。
    pub user_id: Option<String>,
    /// 试图指定接收行（**应被拒**）。
    pub receiver_id: Option<String>,
}

/// 通知主体分页查询（管理视角）。
#[derive(Debug, Clone, Deserialize)]
pub struct NoticePageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（标题 / 编号模糊）。
    pub keyword: Option<String>,
    /// 通知类型。
    pub notice_type: Option<String>,
    /// 级别。
    pub level: Option<String>,
    /// 业务类型。
    pub biz_type: Option<String>,
}

/// 我的通知分页查询。
#[derive(Debug, Clone, Deserialize)]
pub struct MyNoticeQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 已读筛选（`true` 已读 / `false` 未读 / 不传全部）。
    pub read_flag: Option<bool>,
}

/// 模板分页查询。
#[derive(Debug, Clone, Deserialize)]
pub struct TemplatePageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（编码 / 名称模糊）。
    pub keyword: Option<String>,
    /// 通知类型。
    pub notice_type: Option<String>,
    /// 状态。
    pub status: Option<i16>,
}

/// 模板新增 / 编辑请求。
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct TemplateSaveReq {
    /// 模板编码（`^[A-Za-z][A-Za-z0-9_]{1,63}$`）。
    #[validate(length(min = 2, max = 64, message = "模板编码长度需为 2-64 个字符"))]
    pub template_code: String,
    /// 模板名称。
    #[validate(length(min = 1, max = 100, message = "模板名称长度需为 1-100 个字符"))]
    pub template_name: String,
    /// 通知类型（缺省 `SYSTEM`）。
    pub notice_type: Option<String>,
    /// 默认级别（缺省 `INFO`）。
    pub default_level: Option<String>,
    /// 标题模板。
    #[validate(length(min = 1, max = 255, message = "标题模板长度需为 1-255 个字符"))]
    pub title_template: String,
    /// 正文模板。
    #[validate(length(min = 1, message = "正文模板不能为空"))]
    pub content_template: String,
    /// 正文格式（缺省 `TEXT`）。
    pub content_format: Option<String>,
    /// 链接模板。
    #[validate(length(max = 500, message = "链接模板不能超过 500 个字符"))]
    pub link_template: Option<String>,
    /// 默认投递通道（逗号分隔，缺省 `INBOX`）。
    #[validate(length(max = 64, message = "通道列表不能超过 64 个字符"))]
    pub channels: Option<String>,
    /// 变量声明（数组）。
    pub variables: Option<serde_json::Value>,
    /// 状态（1 启用 / 0 停用，缺省启用）。
    pub status: Option<i16>,
    /// 备注。
    #[validate(length(max = 500, message = "备注不能超过 500 个字符"))]
    pub remark: Option<String>,
}

/// 模板试渲染请求。
#[derive(Debug, Clone, Deserialize)]
pub struct TemplatePreviewReq {
    /// 变量表（缺省用声明的 `sample` 值）。
    pub vars: Option<serde_json::Value>,
}

// ==================== 响应 DTO ====================

/// 通知主体响应（管理视角）。
#[derive(Debug, Clone, Serialize)]
pub struct NoticeRes {
    /// 主键。
    pub id: String,
    /// 通知编号。
    pub notice_no: String,
    /// 通知类型。
    pub notice_type: String,
    /// 级别。
    pub level: String,
    /// 标题。
    pub title: String,
    /// 正文。
    pub content: String,
    /// 正文格式。
    pub content_format: String,
    /// 跳转链接。
    pub link_url: String,
    /// 业务类型。
    pub biz_type: String,
    /// 业务主键。
    pub biz_id: String,
    /// 模板编码。
    pub template_code: String,
    /// 发送人 ID。
    pub sender_id: String,
    /// 发送人名称。
    pub sender_name: String,
    /// 目标类型。
    pub target_type: String,
    /// 目标 ID 列表。
    pub target_ids: Vec<String>,
    /// 通道掩码。
    pub channel_mask: i16,
    /// 通道名列表。
    pub channels: Vec<String>,
    /// 接收人数。
    pub receiver_count: i32,
    /// 已读人数。
    pub read_count: i32,
    /// 阅读率（百分比，`receiver_count = 0` 时为 0）。
    pub read_rate: f64,
    /// 状态（1 正常 / 0 停用）。
    pub status: i16,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 计划发送时间（`null` = 立即发送；V0051）。
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 投递状态：`PENDING` 待发（定时未到点，用户侧不可见）/ `SENT` 已投递 /
    /// `FAILED` 重试耗尽 / `CANCELED` 到点前被取消（V0051）。
    pub dispatch_status: String,
    /// 投递尝试次数（定时投递失败重试用；V0051）。
    pub dispatch_attempts: i16,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl NoticeRes {
    /// 由模型构造响应。
    pub fn from_model(model: &SysNoticeModel) -> Self {
        let read_rate = if model.receiver_count > 0 {
            (f64::from(model.read_count) * 100.0 / f64::from(model.receiver_count)).round() / 1.0
        } else {
            0.0
        };
        NoticeRes {
            id: model.id.to_string(),
            notice_no: model.notice_no.clone(),
            notice_type: model.notice_type.clone(),
            level: model.level.clone(),
            title: model.title.clone(),
            content: model.content.clone(),
            content_format: model.content_format.clone(),
            link_url: model.link_url.clone().unwrap_or_default(),
            biz_type: model.biz_type.clone().unwrap_or_default(),
            biz_id: model.biz_id.clone().unwrap_or_default(),
            template_code: model.template_code.clone().unwrap_or_default(),
            sender_id: model.sender_id.map(|id| id.to_string()).unwrap_or_default(),
            sender_name: model.sender_name.clone().unwrap_or_default(),
            target_type: model.target_type.clone(),
            target_ids: model.target_ids.clone(),
            channel_mask: model.channel_mask,
            channels: channel_names(model.channel_mask.max(0) as u16)
                .into_iter()
                .map(str::to_string)
                .collect(),
            receiver_count: model.receiver_count,
            read_count: model.read_count,
            read_rate,
            status: model.status,
            expire_at: model.expire_at,
            scheduled_at: model.scheduled_at,
            dispatch_status: model.dispatch_status.clone(),
            dispatch_attempts: model.dispatch_attempts,
            created_at: model.created_at,
        }
    }
}

/// 通知详情（含接收与投递统计）。
#[derive(Debug, Clone, Serialize)]
pub struct NoticeDetailRes {
    /// 通知主体。
    pub notice: NoticeRes,
    /// 已读人数（实时统计，与冗余计数交叉校验）。
    pub read_count: i64,
    /// 未读人数（实时统计）。
    pub unread_count: i64,
    /// 已被用户侧删除的行数（仍算送达）。
    pub deleted_count: i64,
    /// 投递记录汇总（按通道 + 状态）。
    pub deliveries: Vec<DeliverySummaryRes>,
}

/// 投递记录汇总行。
#[derive(Debug, Clone, Serialize)]
pub struct DeliverySummaryRes {
    /// 通道（EMAIL / WEBHOOK / SMS）。
    pub channel: String,
    /// 状态（PENDING / SENDING / SENT / FAILED / SKIPPED）。
    pub status: String,
    /// 行数。
    pub count: i64,
}

impl DeliverySummaryRes {
    /// 由 `GROUP BY` 行构造。
    pub fn from_row(row: &DeliverySummaryRow) -> Self {
        DeliverySummaryRes {
            channel: row.channel.clone(),
            status: row.status.clone(),
            count: row.count,
        }
    }
}

/// 我的通知列表项。
#[derive(Debug, Clone, Serialize)]
pub struct MyNoticeRes {
    /// 接收行主键（已读 / 删除都用它）。
    pub receiver_id: String,
    /// 通知主键。
    pub notice_id: String,
    /// 通知编号。
    pub notice_no: String,
    /// 通知类型。
    pub notice_type: String,
    /// 级别。
    pub level: String,
    /// 标题。
    pub title: String,
    /// 正文。
    pub content: String,
    /// 正文格式。
    pub content_format: String,
    /// 跳转链接。
    pub link_url: String,
    /// 是否已读。
    pub read_flag: bool,
    /// 已读时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 送达时间（接收行创建时间）。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl MyNoticeRes {
    /// 由联表行构造。
    pub fn from_row(row: &MyNoticeRow) -> Self {
        MyNoticeRes {
            receiver_id: row.receiver_id.to_string(),
            notice_id: row.notice_id.to_string(),
            notice_no: row.notice_no.clone(),
            notice_type: row.notice_type.clone(),
            level: row.level.clone(),
            title: row.title.clone(),
            content: row.content.clone(),
            content_format: row.content_format.clone(),
            link_url: row.link_url.clone().unwrap_or_default(),
            read_flag: row.read_flag,
            read_at: row.read_at,
            created_at: row.created_at,
        }
    }
}

/// 未读数响应。
#[derive(Debug, Clone, Serialize)]
pub struct UnreadCountRes {
    /// 未读条数。
    pub unread_count: i64,
}

/// 模板响应。
#[derive(Debug, Clone, Serialize)]
pub struct TemplateRes {
    /// 主键。
    pub id: String,
    /// 模板编码。
    pub template_code: String,
    /// 模板名称。
    pub template_name: String,
    /// 通知类型。
    pub notice_type: String,
    /// 默认级别。
    pub default_level: String,
    /// 标题模板。
    pub title_template: String,
    /// 正文模板。
    pub content_template: String,
    /// 正文格式。
    pub content_format: String,
    /// 链接模板。
    pub link_template: String,
    /// 默认通道（原始 CSV）。
    pub channels: String,
    /// 默认通道掩码。
    pub channel_mask: i16,
    /// 变量声明。
    pub variables: Vec<TemplateVarDecl>,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: String,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl TemplateRes {
    /// 由模型构造响应。
    pub fn from_model(model: &SysNoticeTemplateModel) -> Self {
        TemplateRes {
            id: model.id.to_string(),
            template_code: model.template_code.clone(),
            template_name: model.template_name.clone(),
            notice_type: model.notice_type.clone(),
            default_level: model.default_level.clone(),
            title_template: model.title_template.clone(),
            content_template: model.content_template.clone(),
            content_format: model.content_format.clone(),
            link_template: model.link_template.clone().unwrap_or_default(),
            channels: model.channels.clone(),
            channel_mask: channel_mask_of_csv(&model.channels) as i16,
            variables: parse_var_decls(model.variables_json.as_deref()),
            status: model.status,
            remark: model.remark.clone().unwrap_or_default(),
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

/// 模板试渲染响应。
#[derive(Debug, Clone, Serialize)]
pub struct TemplatePreviewRes {
    /// 渲染后的标题。
    pub title: String,
    /// 渲染后的正文。
    pub content: String,
}

/// 通道配置响应（本批只读，不含任何密文）。
#[derive(Debug, Clone, Serialize)]
pub struct ChannelConfigRes {
    /// 站内信（恒为 true）。
    pub inbox: bool,
    /// 邮件是否已配置。
    pub email: bool,
    /// Webhook 是否已配置。
    pub webhook: bool,
    /// 短信是否已配置。
    pub sms: bool,
    /// Webhook 地址掩码（形如 `https://hooks.****`；未配置为空串）。
    pub webhook_masked: String,
}

impl ChannelConfigRes {
    /// 判断某个通道是否已配置（未配置的通道投递记录记 `SKIPPED`）。
    pub fn is_configured(&self, channel: &str) -> bool {
        match channel {
            "INBOX" => self.inbox,
            "EMAIL" => self.email,
            "WEBHOOK" => self.webhook,
            "SMS" => self.sms,
            _ => false,
        }
    }
}

/// 单个用户的 SSE 频道统计（运维排障 / 验收取证用）。
#[derive(Debug, Clone, Serialize)]
pub struct StreamUserStat {
    /// 用户主键。
    pub user_id: String,
    /// 该用户当前的订阅端数量（断连后应回到 0，且该用户从列表里消失）。
    pub receivers: usize,
}

/// SSE 频道表统计响应（A6：`GET /business/notice/stream/stats`）。
#[derive(Debug, Clone, Serialize)]
pub struct StreamStatsRes {
    /// 频道数（含尚无订阅端的空频道；正常情况下等于在线用户数）。
    pub total_users: usize,
    /// 全部订阅端总数。
    pub total_receivers: usize,
    /// 每个用户的订阅端数量（按用户主键升序）。
    pub users: Vec<StreamUserStat>,
    /// 累计 `publish` 调用次数。
    pub published_total: u64,
    /// 累计入队到订阅端的消息条数（一次推送命中 N 个连接即 +N）。
    pub delivered_total: u64,
    /// 累计建立的订阅连接数。
    pub subscribe_total: u64,
    /// 心跳间隔（秒）：每 N 秒发一条 `: keep-alive` 注释帧。
    pub heartbeat_seconds: u64,
}

// ==================== 过滤器 ====================

/// 通知主体分页过滤。
#[derive(Debug, Default)]
pub struct NoticeFilter {
    /// 关键词（标题 / 编号模糊）。
    pub keyword: Option<String>,
    /// 通知类型。
    pub notice_type: Option<String>,
    /// 级别。
    pub level: Option<String>,
    /// 业务类型。
    pub biz_type: Option<String>,
}

/// 模板分页过滤。
#[derive(Debug, Default)]
pub struct TemplateFilter {
    /// 关键词（编码 / 名称模糊）。
    pub keyword: Option<String>,
    /// 通知类型。
    pub notice_type: Option<String>,
    /// 状态。
    pub status: Option<EnableStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 通道掩码 ⇄ 通道名双向一致（投递记录与详情展示都靠它）。
    #[test]
    fn channel_mask_roundtrip() {
        assert_eq!(channel_mask_of_csv("INBOX"), CHANNEL_INBOX);
        assert_eq!(
            channel_mask_of_csv("EMAIL, WEBHOOK"),
            CHANNEL_INBOX | CHANNEL_EMAIL | CHANNEL_WEBHOOK,
            "站内信恒置位"
        );
        assert_eq!(channel_names(CHANNEL_INBOX | CHANNEL_SMS), vec!["INBOX", "SMS"]);
        // 未识别的名字忽略，不报错（历史数据容错）
        assert_eq!(channel_mask_of_csv("FOO,bar"), CHANNEL_INBOX);
    }

    /// 掩码越界必须报 400（数据库 CHECK 是兜底，应用层先给中文文案）。
    #[test]
    fn channel_mask_out_of_range_rejected() {
        assert!(ensure_channel_mask(1).is_ok());
        assert!(ensure_channel_mask(15).is_ok());
        let error = ensure_channel_mask(16).unwrap_err();
        assert_eq!(error.status, 400);
        let error = ensure_channel_mask(0).unwrap_err();
        assert_eq!(error.status, 400);
    }

    /// 枚举解析大小写不敏感，未知值报 400（避免脏字符串落库）。
    #[test]
    fn enums_parse_strictly() {
        assert_eq!(NoticeType::parse("todo").unwrap(), NoticeType::Todo);
        assert_eq!(NoticeLevel::parse(" warn ").unwrap(), NoticeLevel::Warn);
        assert_eq!(ContentFormat::parse("html").unwrap(), ContentFormat::Html);
        assert!(NoticeType::parse("MAIL").is_err());
        assert!(NoticeLevel::parse("FATAL").is_err());
        assert!(ContentFormat::parse("RICH").is_err());
        assert!(ContentFormat::Html.escape_values());
        assert!(!ContentFormat::Text.escape_values());
    }

    /// `variables` 列的脏数据必须降级为空声明，而不是让列表接口 500。
    #[test]
    fn parse_var_decls_tolerates_dirty_json() {
        assert!(parse_var_decls(None).is_empty());
        assert!(parse_var_decls(Some("")).is_empty());
        assert!(parse_var_decls(Some("not-json")).is_empty());
        assert!(parse_var_decls(Some(r#"{"a":1}"#)).is_empty());

        let decls = parse_var_decls(Some(r#"[{"name":"a","required":true},{"name":""},{"label":"无名字"}]"#));
        assert_eq!(decls.len(), 1, "空名 / 缺 name 的元素被丢弃");
        assert_eq!(decls[0].name, "a");
        assert!(decls[0].required);
    }

    /// 阅读率：0 接收人时不得除零；正常值四舍五入。
    #[test]
    fn read_rate_is_safe() {
        let mut model = SysNoticeModel {
            id: uuid::Uuid::now_v7(),
            notice_no: "NT1".into(),
            notice_type: "SYSTEM".into(),
            level: "INFO".into(),
            title: "t".into(),
            content: "c".into(),
            content_format: "TEXT".into(),
            link_url: None,
            biz_type: None,
            biz_id: None,
            template_code: None,
            sender_id: None,
            sender_name: None,
            target_type: "ALL".into(),
            target_ids: Vec::new(),
            channel_mask: 1,
            receiver_count: 0,
            read_count: 0,
            status: 1,
            expire_at: None,
            scheduled_at: None,
            dispatch_status: "SENT".into(),
            dispatch_attempts: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
        };
        assert_eq!(NoticeRes::from_model(&model).read_rate, 0.0);

        model.receiver_count = 3;
        model.read_count = 1;
        assert_eq!(NoticeRes::from_model(&model).read_rate, 33.0);
        assert_eq!(NoticeRes::from_model(&model).channels, vec!["INBOX"]);
    }
}

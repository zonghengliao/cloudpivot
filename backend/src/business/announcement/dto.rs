//! 公告用户侧（批次 C · C2）的请求 / 响应 DTO。
//!
//! 契约来源：`docs/批次C-技术架构.md` §2.8.1（用户侧接口与字段名）。
//!
//! # 三条约定
//!
//! 1. **可空文本列降级为空串**（与底座其它 DTO 同口径）：`summary` / `publisher_name`
//!    在库里可空，前端不判 null；
//! 2. **时间一律 `Option<DateTime<Utc>>`**（RFC3339 序列化），可空时间由前端渲染 `-`；
//! 3. **附件不给存储细节**：出参里只有 `download_url`（本模块的代理接口），
//!    **绝不**出现 bucket / object key / 预签名 URL（架构 §2.5.3）。

use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::announcement::model::{
    AnnouncementCardRow, AnnouncementDetailRow, AttachmentRow, ManageCardRow, ManageDetailRow,
    ReaderRow,
};
use crate::shared::error_code;

/// 我的可见公告列表查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VisibleQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（标题 / 摘要模糊）。
    pub keyword: Option<String>,
    /// 只看未读。
    pub only_unread: Option<bool>,
}

impl VisibleQuery {
    /// 归一化后的关键词（空白串视为未传）。
    ///
    /// # 为什么不用 `utils::like_pattern`
    ///
    /// 它只做 `%{k}%` 包装、**不转义**通配符：用户搜 `100%` 会变成 `LIKE '%100%%'`
    /// （语义漂移），搜 `a_b` 会把 `_` 当单字符通配（多命中）。这里显式转义
    /// `\` / `%` / `_`（PG `LIKE` 的默认转义符是反斜杠）。
    pub fn normalized_keyword(&self) -> Option<String> {
        let keyword = self.keyword.as_deref().map(str::trim).unwrap_or_default();
        if keyword.is_empty() {
            return None;
        }
        let escaped = keyword
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        Some(format!("%{escaped}%"))
    }
}

/// 我的可见公告卡片（列表项）。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementCardRes {
    /// 主键。
    pub id: String,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// 级别（`NORMAL` / `IMPORTANT` / `URGENT`）。
    pub level: String,
    /// 是否置顶。
    pub is_top: bool,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 我是否已读。
    pub read: bool,
    /// 附件数。
    pub attachment_count: i64,
    /// 发布人姓名。
    pub publisher_name: String,
}

impl AnnouncementCardRes {
    /// 由列表行构造。
    pub fn from_row(row: &AnnouncementCardRow) -> Self {
        Self {
            id: row.id.to_string(),
            ann_no: row.ann_no.clone(),
            title: row.title.clone(),
            summary: row.summary.clone(),
            level: row.level.clone(),
            is_top: row.is_top,
            publish_at: row.publish_at,
            read: row.read_flag,
            attachment_count: row.attachment_count,
            publisher_name: row.publisher_name.clone(),
        }
    }
}

/// 公告附件（出参）。
#[derive(Debug, Clone, Serialize)]
pub struct AttachmentRes {
    /// 绑定行主键（下载地址里的 `att_id`）。
    pub id: String,
    /// `sys_file.id`（前端只在需要「复用/秒传」时用，不用于下载）。
    pub file_id: String,
    /// 文件名。
    pub file_name: String,
    /// 字节数。
    pub file_size: i64,
    /// MIME（未知时为空串）。
    pub mime_type: String,
    /// 排序号。
    pub sort_order: i32,
    /// **代理下载地址**（唯一可下载入口；不含 bucket / AK / 预签名）。
    pub download_url: String,
}

impl AttachmentRes {
    /// 由附件行构造。
    ///
    /// # 参数
    ///
    /// - `announcement_id`：所属公告（拼进下载地址，服务端据此校验「附件属于该公告」）；
    /// - `row`：附件行。
    pub fn from_row(announcement_id: &uuid::Uuid, row: &AttachmentRow) -> Self {
        Self {
            id: row.id.to_string(),
            file_id: row.file_id.to_string(),
            file_name: row.file_name.clone(),
            file_size: row.file_size,
            mime_type: row.mime_type.clone().unwrap_or_default(),
            sort_order: row.sort_order,
            download_url: download_url(announcement_id, &row.id),
        }
    }
}

/// 附件代理下载地址（**唯一**可下载入口）。
///
/// # 参数
///
/// - `announcement_id`：公告主键；
/// - `attachment_id`：附件绑定行主键。
///
/// # 返回值
///
/// 形如 `/api/business/announcement/{id}/attachment/{att_id}`（含 `/api` 前缀，
/// 前端可直接用于 `<a href>`；后端注解里**不带** `/api`，前缀由 `[router].prefix` 注入）。
pub fn download_url(announcement_id: &uuid::Uuid, attachment_id: &uuid::Uuid) -> String {
    format!("/api/business/announcement/{announcement_id}/attachment/{attachment_id}")
}

/// 公告详情。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementDetailRes {
    /// 主键。
    pub id: String,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// **渲染 + 净化后的 HTML**（出参前已再净化：闸 3；前端 `v-html` 前仍应过一遍 DOMPurify：闸 4）。
    pub content_html: String,
    /// 级别。
    pub level: String,
    /// 发布范围类型（`ALL` / `ROLE` / `DEPT` / `USER`）。
    pub scope_type: String,
    /// 状态。
    pub status: String,
    /// 是否置顶。
    pub is_top: bool,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 过期时间（`null` = 永不过期）。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人姓名。
    pub publisher_name: String,
    /// 我是否已读。
    pub read: bool,
    /// 我读到的时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 附件列表（下载走代理地址）。
    pub attachments: Vec<AttachmentRes>,
}

impl AnnouncementDetailRes {
    /// 由详情行 + 附件列表构造。
    ///
    /// # 参数
    ///
    /// - `row`：详情行；
    /// - `content_html`：**已经过闸 3 净化**的 HTML（调用方负责，不在这里渲染）；
    /// - `attachments`：附件行。
    pub fn from_row(
        row: &AnnouncementDetailRow,
        content_html: String,
        attachments: &[AttachmentRow],
    ) -> Self {
        Self {
            id: row.id.to_string(),
            ann_no: row.ann_no.clone(),
            title: row.title.clone(),
            summary: row.summary.clone(),
            content_html,
            level: row.level.clone(),
            scope_type: row.scope_type.clone(),
            status: row.status.clone(),
            is_top: row.is_top,
            publish_at: row.publish_at,
            expire_at: row.expire_at,
            publisher_name: row.publisher_name.clone(),
            read: row.read_flag,
            read_at: row.read_at,
            attachments: attachments
                .iter()
                .map(|att| AttachmentRes::from_row(&row.id, att))
                .collect(),
        }
    }
}

/// 未读数响应。
///
/// 字段名**刻意**叫 `announcement_unread`（不是 `unread_count`）：前端顶栏有两个角标
/// （通知 / 公告），同名字段会在聚合时互相覆盖（架构 §2.7）。
#[derive(Debug, Clone, Serialize)]
pub struct UnreadCountRes {
    /// 公告未读数（可见 ∧ 未过期 − 已读）。
    pub announcement_unread: i64,
}

/// 标记已读响应。
#[derive(Debug, Clone, Serialize)]
pub struct MarkReadRes {
    /// 当前公告的已读数。
    pub read_count: i32,
    /// 本次是否为首次已读（幂等：重复调用为 `false`，计数不变）。
    pub first_read: bool,
}

/// 全部已读响应。
#[derive(Debug, Clone, Serialize)]
pub struct ReadAllRes {
    /// 本次新增的已读回执行数。
    pub affected: u64,
}

// ==================== 管理侧（批次 C · C3）====================
//
// 契约来源：`docs/批次C-技术架构.md` §2.8.2（管理侧 10 个接口）+ §2.8.3（新建请求与
// 校验规则表）+ PRD A1-01 ~ A1-10（列表列 / 附件上限 / 置顶默认期）。
//
// # 三个校验原则
//
// 1. **后端强校验，不依赖前端**（§2.8.3）：所有规则在 [`normalize_save`] 里做一次，
//    错误码逐条对应架构表格（`62001` / `62006` / `62007` / `62008` / `62003`）；
// 2. **纯函数优先**：[`normalize_save`] / [`parse_rfc3339`] / [`normalize_top`] 不碰数据库，
//    因此可以在单测里把边界全跑一遍（长度 / 数量 / 时间先后 / 危险扩展名）；
// 3. **入参字符串集中解析**：时间统一走 [`parse_rfc3339`]（RFC3339 / 带时区偏移都接受，
//    统一转 UTC），UUID 统一走 `utils::require_uuid` —— 不让它们在 handler 里散落。

/// 标题长度上限（字符数，PRD A1-02「标题 1–200」）。
pub const MAX_TITLE_CHARS: usize = 200;
/// 摘要长度上限（`sys_announcement.summary` 是 `VARCHAR(500)`）。
pub const MAX_SUMMARY_CHARS: usize = 500;
/// 正文字节上限（128 KiB，与 `ck_sys_announcement_body` 的 `octet_length` 一致）。
pub const MAX_CONTENT_BYTES: usize = 131_072;
/// 单公告附件数量上限（PRD A1-10「≤10 个」）。
pub const MAX_ATTACHMENTS: usize = 10;
/// 单公告附件合计大小上限（PRD A1-10「合计 50MB」）。
pub const MAX_ATTACHMENT_TOTAL_BYTES: i64 = 50 * 1024 * 1024;
/// 发布范围明细上限（§2.8.3「≤ 500 个」）。
pub const MAX_SCOPE_IDS: usize = 500;
/// 默认置顶天数（PRD §3.6「界面默认填 7 天」）。
pub const DEFAULT_TOP_DAYS: i64 = 7;

/// 三态可选字段的反序列化辅助（区分「未传」与「显式 null」）。
///
/// # 为什么需要
///
/// `#[serde(default)] Option<T>` 把「字段缺席」和「字段为 null」都变成 `None`，
/// 于是编辑接口无法表达「清空有效期」—— 更糟的是反过来：**漏传字段会被当成清空**。
/// 公告的范围 / 有效期一旦被「漏传即清空」，轻则公告永不过期，重则定向公告变全员可见。
/// 因此可清空字段一律用 `Option<Option<T>>`：
///
/// - 字段缺席 → `None`（外层）→ 编辑时保持原值；
/// - `null` → `Some(None)` → 清空；
/// - 有值 → `Some(Some(v))` → 赋值。
///
/// # 异常情况
///
/// 反序列化失败由 serde 报错（框架统一转 400）。
fn deserialize_double_option<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// 管理列表查询（`GET /business/announcement/page`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ManagePageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 关键词（标题 / 编号 / 摘要模糊）。
    pub keyword: Option<String>,
    /// 状态过滤（`DRAFT` / `PUBLISHED` / `OFFLINE` / `EXPIRED`，可逗号分隔多选）。
    pub status: Option<String>,
    /// 范围类型过滤（`ALL` / `ROLE` / `DEPT` / `USER`）。
    pub scope_type: Option<String>,
    /// 级别过滤（`NORMAL` / `IMPORTANT` / `URGENT`）。
    pub level: Option<String>,
    /// 发布时间窗起（RFC3339）。
    pub publish_from: Option<String>,
    /// 发布时间窗止（RFC3339）。
    pub publish_to: Option<String>,
}

/// 管理列表行。
#[derive(Debug, Clone, Serialize)]
pub struct ManageCardRes {
    /// 主键。
    pub id: String,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// 级别。
    pub level: String,
    /// 发布范围类型。
    pub scope_type: String,
    /// 状态（`DRAFT` / `PUBLISHED` / `OFFLINE` / `EXPIRED`）。
    pub status: String,
    /// 是否置顶（含**未过期**判定：`is_top` 已过期时为 `false`）。
    pub is_top: bool,
    /// 置顶到期时间（`null` = 永久置顶）。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人姓名。
    pub publisher_name: String,
    /// 扇出接收人数（未发布 / 扇出失败为 0）。
    pub receiver_count: i64,
    /// 已读数。
    pub read_count: i64,
    /// 阅读率（`read_count / receiver_count`，接收人为 0 时为 0.0）。
    pub read_rate: f64,
    /// 附件数。
    pub attachment_count: i64,
    /// 内容版本（每次发布 +1）。
    pub version: i32,
    /// 扇出的通知 ID（`null` = 尚未扇出 / 扇出失败待补偿）。
    pub notice_id: Option<String>,
    /// 最后更新时间。
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ManageCardRes {
    /// 由管理列表行构造。
    pub fn from_row(row: &ManageCardRow) -> Self {
        let receiver_count = i64::from(row.receiver_count);
        let read_count = i64::from(row.read_count);
        Self {
            id: row.id.to_string(),
            ann_no: row.ann_no.clone(),
            title: row.title.clone(),
            summary: row.summary.clone(),
            level: row.level.clone(),
            scope_type: row.scope_type.clone(),
            status: row.status.clone(),
            is_top: row.top_active,
            top_expire_at: row.top_expire_at,
            publish_at: row.publish_at,
            expire_at: row.expire_at,
            publisher_name: row.publisher_name.clone(),
            receiver_count,
            read_count,
            read_rate: read_rate(read_count, receiver_count),
            attachment_count: row.attachment_count,
            version: row.version,
            notice_id: row.notice_id.map(|id| id.to_string()),
            updated_at: row.updated_at,
        }
    }
}

/// 阅读率（保留 4 位小数；接收人为 0 时返回 0.0，避免 `NaN` 进 JSON）。
///
/// # 参数
///
/// - `read_count`：已读数；
/// - `receiver_count`：扇出接收人数。
pub fn read_rate(read_count: i64, receiver_count: i64) -> f64 {
    if receiver_count <= 0 || read_count <= 0 {
        return 0.0;
    }
    let rate = (read_count as f64 / receiver_count as f64 * 10_000.0).round() / 10_000.0;
    rate.clamp(0.0, 1.0)
}

/// 按**字符**截断（不切断 UTF-8 字节边界），超出部分补 `…`。
///
/// # 参数
///
/// - `text`：原文；
/// - `max_chars`：保留的最大字符数。
///
/// # 为什么不用 `&text[..n]`
///
/// 中文字符占 3 字节，按字节切会 panic 在非字符边界；公告标题 / 摘要 / 通知正文
/// 全是中文，这条必须按字符算。
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 附件入参（新建 / 编辑请求里的 `attachments[]`）。
#[derive(Debug, Clone, Deserialize)]
pub struct AnnouncementAttachmentReq {
    /// `sys_file.id`（文件本体由 `/business/storage/upload` 上传，本接口只绑定）。
    pub file_id: String,
    /// 排序号（默认按数组下标）。
    #[serde(default)]
    pub sort_order: Option<i32>,
}

/// 新建 / 编辑公告请求（`POST /business/announcement`、`PUT /business/announcement/{id}`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnnouncementSaveReq {
    /// 标题（1–200 字符）。
    pub title: String,
    /// 摘要（可空 → 由正文纯文本自动截取）。
    #[serde(default)]
    pub summary: Option<String>,
    /// 正文 Markdown（1–128 KiB）。
    #[serde(default)]
    pub content_md: String,
    /// 级别（`NORMAL` / `IMPORTANT` / `URGENT`，默认 `NORMAL`）。
    #[serde(default)]
    pub level: Option<String>,
    /// 范围类型（`ALL` / `ROLE` / `DEPT` / `USER`，默认 `ALL`）。
    #[serde(default)]
    pub scope_type: Option<String>,
    /// 范围明细（`scope_type != ALL` 时必填，≤500 个）。
    #[serde(default)]
    pub scope_ids: Option<Vec<String>>,
    /// 是否置顶。
    #[serde(default)]
    pub is_top: Option<bool>,
    /// 置顶到期时间（`is_top=true` 时必填，RFC3339）。
    ///
    /// 三态：`None` = 未传（编辑时保持原值）/ `Some(None)` = 显式 `null`（清空）/
    /// `Some(Some(v))` = 赋值。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub top_expire_at: Option<Option<String>>,
    /// 有效期止（RFC3339，`null` = 永不过期）。
    ///
    /// 三态语义同 `top_expire_at`：编辑时**未传 = 保持原值**，显式 `null` = 清空。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub expire_at: Option<Option<String>>,
    /// 备注（≤500）。三态语义同 `expire_at`。
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub remark: Option<Option<String>>,
    /// 附件列表（≤10；`None` = 保持原样，`Some([])` = 清空）。
    #[serde(default)]
    pub attachments: Option<Vec<AnnouncementAttachmentReq>>,
}

/// 校验并归一化后的公告内容（[`normalize_save`] 的产物，供 [`crate::business::announcement::model`] 落库）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedAnnouncement {
    /// 标题（已去空白）。
    pub title: String,
    /// 摘要（已去空白；空串表示「由正文自动生成」）。
    pub summary: String,
    /// 正文 Markdown（已去首尾空白）。
    pub content_md: String,
    /// 级别（大写，已校验）。
    pub level: String,
    /// 范围类型（大写，已校验）。
    pub scope_type: String,
    /// 范围明细（已去重；`ALL` 时为空）。
    pub scope_ids: Vec<uuid::Uuid>,
    /// 是否置顶。
    pub is_top: bool,
    /// 置顶到期时间。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 有效期止。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 备注。
    pub remark: Option<String>,
    /// 附件（`None` = 不修改；`Some` = 全量替换）。
    pub attachments: Option<Vec<(uuid::Uuid, i32)>>,
}

/// 解析 RFC3339 时间字符串（统一转 UTC）。
///
/// # 参数
///
/// - `raw`：形如 `2026-11-01T22:00:00Z` / `2026-11-01T22:00:00+08:00`；
/// - `field`：字段名（错误文案里用）。
///
/// # 返回值
///
/// `None`（未传 / 空白串）或 `Some(UTC 时间)`。
///
/// # 异常情况
///
/// 格式非法 → `400 + 62001`（公告内容参数非法段）。
pub fn parse_rfc3339(
    raw: Option<&str>,
    field: &str,
) -> std::result::Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|value| Some(value.with_timezone(&chrono::Utc)))
        .map_err(|_| {
            ApiError::new(
                400,
                error_code::ANNOUNCEMENT_CONTENT_INVALID,
                format!("{field} 必须是 RFC3339 时间（如 2026-11-01T22:00:00Z）"),
            )
        })
}

/// 取出三态字段的「值」（`None` 与 `Some(None)` 都视为无值）。
///
/// # 参数
///
/// - `raw`：三态字段的引用。
///
/// # 返回值
///
/// `Option<&String>`：`Some(None)`（显式 null）与 `None`（未传）都返回 `None`。
fn present(raw: Option<&Option<String>>) -> Option<String> {
    raw.and_then(|value| value.clone())
}

/// 编辑时的字段合并：**缺席的可选字段沿用原值**。
///
/// # 参数
///
/// - `req`：请求体（`PUT`）；
/// - `existing`：当前公告（管理详情行）。
///
/// # 返回值
///
/// 合并后的请求体（可空字段的「缺席」被填成原值；显式 `null` 保持 `Some(None)` = 清空）。
///
/// # 为什么不在 handler 里手写
///
/// 六个字段 × 三种状态 = 一处写错就静默改数据（最坏是定向公告变全员可见）。
/// 放在 dto 里可以单测全覆盖。
pub fn merge_with_existing(
    req: &AnnouncementSaveReq,
    existing: &ManageDetailRow,
) -> AnnouncementSaveReq {
    let mut merged = req.clone();
    if merged.level.is_none() {
        merged.level = Some(existing.level.clone());
    }
    if merged.scope_type.is_none() && merged.scope_ids.is_none() {
        merged.scope_type = Some(existing.scope_type.clone());
        merged.scope_ids = Some(
            existing
                .scope_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
        );
    }
    if merged.is_top.is_none() {
        merged.is_top = Some(existing.is_top_flag);
    }
    if merged.top_expire_at.is_none() {
        merged.top_expire_at = Some(existing.top_expire_at.map(|when| when.to_rfc3339()));
    }
    if merged.expire_at.is_none() {
        merged.expire_at = Some(existing.expire_at.map(|when| when.to_rfc3339()));
    }
    if merged.remark.is_none() {
        let remark = existing.remark.trim();
        merged.remark = Some(if remark.is_empty() {
            None
        } else {
            Some(remark.to_string())
        });
    }
    merged
}

/// 校验并归一化新建 / 编辑请求（**唯一的校验入口**）。
///
/// # 参数
///
/// - `req`：请求体；
/// - `now`：当前时间（显式传入便于单测固定时钟）。
///
/// # 返回值
///
/// [`NormalizedAnnouncement`]。
///
/// # 异常情况
///
/// 逐条对应架构 §2.8.3 的校验表：
///
/// - 标题空 / 超长、正文空 / 超 128 KiB、摘要超长、附件超数量 → `400 + 62001`；
/// - `scope_type != ALL` 而 `scope_ids` 为空 / 超 500 → `400 + 62006`；
/// - `is_top=true` 缺 `top_expire_at` 或已过期 → `400 + 62007`；
/// - `expire_at <= now` → `400 + 62008`；
/// - 附件 `file_id` 非法 → `400 + 62003`。
pub fn normalize_save(
    req: &AnnouncementSaveReq,
    now: chrono::DateTime<chrono::Utc>,
) -> std::result::Result<NormalizedAnnouncement, ApiError> {
    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err(content_invalid("标题不能为空"));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(content_invalid(format!(
            "标题最多 {MAX_TITLE_CHARS} 个字符（当前 {}）",
            title.chars().count()
        )));
    }

    let content_md = req.content_md.trim().to_string();
    if content_md.is_empty() {
        return Err(content_invalid("正文不能为空"));
    }
    if content_md.len() > MAX_CONTENT_BYTES {
        return Err(content_invalid(format!(
            "正文过大（{} 字节 > {} KiB 上限），请精简或改用附件",
            content_md.len(),
            MAX_CONTENT_BYTES / 1024
        )));
    }

    let summary = req.summary.as_deref().map(str::trim).unwrap_or_default();
    if summary.chars().count() > MAX_SUMMARY_CHARS {
        return Err(content_invalid(format!(
            "摘要最多 {MAX_SUMMARY_CHARS} 个字符（当前 {}）",
            summary.chars().count()
        )));
    }

    let level = normalize_enum(
        req.level.as_deref(),
        &["NORMAL", "IMPORTANT", "URGENT"],
        "NORMAL",
        "级别",
    )?;
    let scope_type = normalize_enum(
        req.scope_type.as_deref(),
        &["ALL", "ROLE", "DEPT", "USER"],
        "ALL",
        "发布范围类型",
    )?;

    let mut scope_ids: Vec<uuid::Uuid> = Vec::new();
    if scope_type != "ALL" {
        for raw in req.scope_ids.as_deref().unwrap_or_default() {
            let text = raw.trim();
            if text.is_empty() {
                continue;
            }
            let id = uuid::Uuid::parse_str(text).map_err(|_| {
                ApiError::new(
                    400,
                    error_code::ANNOUNCEMENT_SCOPE_INVALID,
                    format!("scope_ids 含非法 UUID：{text}"),
                )
            })?;
            if !scope_ids.contains(&id) {
                scope_ids.push(id);
            }
        }
        if scope_ids.is_empty() {
            return Err(ApiError::new(
                400,
                error_code::ANNOUNCEMENT_SCOPE_INVALID,
                format!("发布范围为 {scope_type} 时 scope_ids 不能为空"),
            ));
        }
        if scope_ids.len() > MAX_SCOPE_IDS {
            return Err(ApiError::new(
                400,
                error_code::ANNOUNCEMENT_SCOPE_INVALID,
                format!(
                    "scope_ids 最多 {MAX_SCOPE_IDS} 个（当前 {}）",
                    scope_ids.len()
                ),
            ));
        }
    }

    let is_top = req.is_top.unwrap_or(false);
    let top_expire_at = parse_rfc3339(
        present(req.top_expire_at.as_ref()).as_deref(),
        "top_expire_at",
    )?;
    if is_top {
        let Some(when) = top_expire_at else {
            return Err(ApiError::new(
                400,
                error_code::ANNOUNCEMENT_TOP_INVALID,
                format!("置顶必须给出 top_expire_at（默认 {DEFAULT_TOP_DAYS} 天后到期）"),
            ));
        };
        if when <= now {
            return Err(ApiError::new(
                400,
                error_code::ANNOUNCEMENT_TOP_INVALID,
                "置顶到期时间必须晚于当前时间",
            ));
        }
    }

    let expire_at = parse_rfc3339(present(req.expire_at.as_ref()).as_deref(), "expire_at")?;
    if let Some(when) = expire_at
        && when <= now
    {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_EXPIRE_INVALID,
            "有效期必须晚于当前时间",
        ));
    }

    let remark = present(req.remark.as_ref())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    if let Some(text) = remark.as_deref()
        && text.chars().count() > MAX_SUMMARY_CHARS
    {
        return Err(content_invalid(format!(
            "备注最多 {MAX_SUMMARY_CHARS} 个字符（当前 {}）",
            text.chars().count()
        )));
    }

    let attachments = match req.attachments.as_deref() {
        None => None,
        Some(items) => {
            if items.len() > MAX_ATTACHMENTS {
                return Err(content_invalid(format!(
                    "最多上传 {MAX_ATTACHMENTS} 个附件（当前 {}）",
                    items.len()
                )));
            }
            let mut normalized = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let file_id = uuid::Uuid::parse_str(item.file_id.trim()).map_err(|_| {
                    ApiError::new(
                        400,
                        error_code::ANNOUNCEMENT_ATTACHMENT_NOT_FOUND,
                        format!("附件 file_id 非法：{}", item.file_id),
                    )
                })?;
                let sort_order = item.sort_order.unwrap_or(index as i32);
                normalized.push((file_id, sort_order));
            }
            Some(normalized)
        }
    };

    Ok(NormalizedAnnouncement {
        title,
        summary: summary.to_string(),
        content_md,
        level,
        scope_type,
        scope_ids,
        is_top,
        top_expire_at,
        expire_at,
        remark,
        attachments,
    })
}

/// 置顶请求（`PUT /business/announcement/{id}/top`）。
#[derive(Debug, Clone, Deserialize)]
pub struct AnnouncementTopReq {
    /// 是否置顶。
    pub is_top: bool,
    /// 置顶到期时间（置顶时必填；取消置顶时忽略）。
    #[serde(default)]
    pub top_expire_at: Option<String>,
}

/// 置顶归一化结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTop {
    /// 是否置顶。
    pub is_top: bool,
    /// 置顶到期时间（取消置顶时恒为 `None`）。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 校验并归一化置顶请求。
///
/// # 异常情况
///
/// - 置顶但缺 `top_expire_at` 或时间已过 → `400 + 62007`；
/// - 时间格式非法 → `400 + 62001`。
pub fn normalize_top(
    req: &AnnouncementTopReq,
    now: chrono::DateTime<chrono::Utc>,
) -> std::result::Result<NormalizedTop, ApiError> {
    if !req.is_top {
        // 取消置顶必须同时清 `top_expire_at`：`ck_sys_announcement_top` 要求
        // 「未置顶 ⇒ top_expire_at IS NULL」，留着时间会被数据库打回 500。
        return Ok(NormalizedTop {
            is_top: false,
            top_expire_at: None,
        });
    }
    let when = parse_rfc3339(req.top_expire_at.as_deref(), "top_expire_at")?.ok_or_else(|| {
        ApiError::new(
            400,
            error_code::ANNOUNCEMENT_TOP_INVALID,
            format!("置顶必须给出 top_expire_at（默认 {DEFAULT_TOP_DAYS} 天后到期）"),
        )
    })?;
    if when <= now {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_TOP_INVALID,
            "置顶到期时间必须晚于当前时间",
        ));
    }
    Ok(NormalizedTop {
        is_top: true,
        top_expire_at: Some(when),
    })
}

/// 默认置顶到期时间（`now + 7 天`，PRD §3.6）。
pub fn default_top_expire_at(now: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    now + chrono::Duration::days(DEFAULT_TOP_DAYS)
}

/// 新建 / 编辑 / 发布的结果。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementSaveRes {
    /// 主键。
    pub id: String,
    /// 公告编号。
    pub ann_no: String,
    /// 状态。
    pub status: String,
    /// 内容版本。
    pub version: i32,
}

/// 管理详情（含 Markdown 原文与范围明细，编辑器回填用）。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementManageDetailRes {
    /// 主键。
    pub id: String,
    /// 公告编号。
    pub ann_no: String,
    /// 标题。
    pub title: String,
    /// 摘要。
    pub summary: String,
    /// 正文 Markdown 原文（**唯一真源**）。
    pub content_md: String,
    /// 渲染 + 净化后的 HTML 快照（出参前已再净化一次）。
    pub content_html: String,
    /// 级别。
    pub level: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围明细 ID（`ALL` 时为空数组）。
    pub scope_ids: Vec<String>,
    /// 状态。
    pub status: String,
    /// 是否置顶（含未过期判定）。
    pub is_top: bool,
    /// 置顶到期时间。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布时间。
    pub publish_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 过期时间。
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 发布人姓名。
    pub publisher_name: String,
    /// 扇出的通知 ID。
    pub notice_id: Option<String>,
    /// 接收人数。
    pub receiver_count: i64,
    /// 已读数。
    pub read_count: i64,
    /// 阅读率。
    pub read_rate: f64,
    /// 内容版本。
    pub version: i32,
    /// 备注。
    pub remark: String,
    /// 附件列表。
    pub attachments: Vec<AttachmentRes>,
    /// 创建时间。
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 更新时间。
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AnnouncementManageDetailRes {
    /// 由管理详情行 + 附件行构造。
    ///
    /// # 参数
    ///
    /// - `row`：管理详情行；
    /// - `content_html`：**已净化**的 HTML（调用方负责，不在这里渲染）；
    /// - `attachments`：附件行。
    pub fn from_row(
        row: &ManageDetailRow,
        content_html: String,
        attachments: &[AttachmentRow],
    ) -> Self {
        let receiver_count = i64::from(row.receiver_count);
        let read_count = i64::from(row.read_count);
        Self {
            id: row.id.to_string(),
            ann_no: row.ann_no.clone(),
            title: row.title.clone(),
            summary: row.summary.clone(),
            content_md: row.content_md.clone(),
            content_html,
            level: row.level.clone(),
            scope_type: row.scope_type.clone(),
            scope_ids: row.scope_ids.iter().map(|id| id.to_string()).collect(),
            status: row.status.clone(),
            is_top: row.top_active,
            top_expire_at: row.top_expire_at,
            publish_at: row.publish_at,
            expire_at: row.expire_at,
            publisher_name: row.publisher_name.clone(),
            notice_id: row.notice_id.map(|id| id.to_string()),
            receiver_count,
            read_count,
            read_rate: read_rate(read_count, receiver_count),
            version: row.version,
            remark: row.remark.clone(),
            attachments: attachments
                .iter()
                .map(|att| AttachmentRes::from_row(&row.id, att))
                .collect(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 预览请求（`POST /business/announcement/preview`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AnnouncementPreviewReq {
    /// 正文 Markdown（不入库）。
    #[serde(default)]
    pub content_md: String,
}

/// 预览响应。
///
/// 与发布走**同一条**渲染 + 净化管线（`business::shared::content::render` +
/// `Recipe::Announcement`）；`warnings` 把「被净化掉了什么」如实告诉作者，
/// 而不是静默改写（架构 §2.4.2）。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementPreviewRes {
    /// 净化后的 HTML（可直接贴 `v-html`，前端仍应过 DOMPurify：闸 4）。
    pub content_html: String,
    /// 自动摘要（纯文本截断；手工填摘要时以手工为准）。
    pub summary: String,
    /// 提示信息（图片被移除 / 链接被降级 / 原始 HTML 被转义 / 正文为空）。
    pub warnings: Vec<String>,
}

/// 发布结果。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementPublishRes {
    /// 主键。
    pub id: String,
    /// 状态（恒为 `PUBLISHED`）。
    pub status: String,
    /// 发布后的版本（每次发布 +1）。
    pub version: i32,
    /// 扇出的通知 ID（扇出失败 / 全局关闭时为 `null`，由补偿任务补齐）。
    pub notice_id: Option<String>,
    /// 通知编号。
    pub notice_no: Option<String>,
    /// 扇出接收人数。
    pub receiver_count: i64,
    /// 扇出方式：`DISPATCHED` 新发 / `DUPLICATED` 幂等重放 / `FAILED` 失败待补偿 / `SKIPPED` 全局开关关闭。
    pub fanout: String,
    /// 提示文案（非空时前端应弹出提示，例如「已发布，但站内信扇出失败，补偿任务将在 10 分钟内重试」）。
    pub warning: Option<String>,
}

/// 下线请求（`POST /business/announcement/{id}/offline`）。
#[derive(Debug, Clone, Deserialize)]
pub struct AnnouncementOfflineReq {
    /// 下线原因（必填，≤200 字符；写审计日志）。
    #[serde(default)]
    pub reason: String,
}

/// 下线结果。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementOfflineRes {
    /// 主键。
    pub id: String,
    /// 状态（恒为 `OFFLINE`）。
    pub status: String,
    /// 内容版本（下线不改版本）。
    pub version: i32,
}

/// 置顶结果。
#[derive(Debug, Clone, Serialize)]
pub struct AnnouncementTopRes {
    /// 主键。
    pub id: String,
    /// 是否置顶。
    pub is_top: bool,
    /// 置顶到期时间。
    pub top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 附件绑定请求（`POST /business/announcement/{id}/attachment`）。
///
/// # 为什么不接收 multipart
///
/// 文件本体走批次 A 已有的 `POST /api/business/storage/upload?category=announcement`
/// （策略校验 / 秒传 / 多后端路由都在那里），本接口只写
/// `sys_announcement_attachment` 关联行 —— 与架构 §2.8.3 的
/// `attachments: [{file_id, sort_order}]` 口径一致，也避免复制 storage 的上传实现。
#[derive(Debug, Clone, Deserialize)]
pub struct AnnouncementAttachmentBindReq {
    /// `sys_file.id`。
    pub file_id: String,
    /// 排序号（默认追加到末尾）。
    #[serde(default)]
    pub sort_order: Option<i32>,
}

/// 阅读名单查询（`GET /business/announcement/{id}/readers`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReadersQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 状态过滤：`ALL`（默认）/ `READ` / `UNREAD`。
    pub state: Option<String>,
    /// 关键词（账号 / 姓名模糊）。
    pub keyword: Option<String>,
}

/// 阅读名单状态过滤（解析后的枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderState {
    /// 全部（接收人 ∪ 已读回执）。
    All,
    /// 只列已读。
    Read,
    /// 只列未读。
    Unread,
}

impl ReaderState {
    /// 解析（大小写不敏感，非法值 → `400 + 10001`）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        match raw
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_uppercase()
            .as_str()
        {
            "" | "ALL" => Ok(ReaderState::All),
            "READ" => Ok(ReaderState::Read),
            "UNREAD" => Ok(ReaderState::Unread),
            other => Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("state 只能是 ALL / READ / UNREAD（收到 {other}）"),
            )),
        }
    }
}

/// 阅读名单行。
#[derive(Debug, Clone, Serialize)]
pub struct ReaderRes {
    /// 用户 ID。
    pub user_id: String,
    /// 登录账号。
    pub username: String,
    /// 真实姓名。
    pub real_name: String,
    /// 部门名。
    pub dept_name: String,
    /// 是否已读。
    pub read: bool,
    /// 已读时间。
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ReaderRes {
    /// 由名单行构造。
    pub fn from_row(row: &ReaderRow) -> Self {
        Self {
            user_id: row.user_id.to_string(),
            username: row.username.clone(),
            real_name: row.real_name.clone(),
            dept_name: row.dept_name.clone(),
            read: row.read_at.is_some(),
            read_at: row.read_at,
        }
    }
}

/// 阅读名单响应（含统计信息，前端一次请求拿全）。
#[derive(Debug, Clone, Serialize)]
pub struct ReadersRes {
    /// 公告主键。
    pub announcement_id: String,
    /// 扇出接收人数（`sys_announcement.receiver_count`）。
    pub receiver_count: i64,
    /// 已读数（`sys_announcement.read_count`）。
    pub read_count: i64,
    /// 阅读率。
    pub read_rate: f64,
    /// 名单总行数（受 `state` / `keyword` 过滤影响）。
    pub total: u64,
    /// 当前页。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 名单（分页）。
    pub list: Vec<ReaderRes>,
}

/// 构造「内容非法」错误（`400 + 62001`）。
fn content_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(
        400,
        error_code::ANNOUNCEMENT_CONTENT_INVALID,
        message.into(),
    )
}

/// 把可空字符串归一化成白名单枚举值（大小写不敏感；缺省用 `default`）。
///
/// # 异常情况
///
/// 不在白名单内 → `400 + 62001`。
fn normalize_enum(
    raw: Option<&str>,
    allowed: &[&str],
    default: &str,
    label: &str,
) -> std::result::Result<String, ApiError> {
    let value = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| default.to_string());
    if allowed.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(content_invalid(format!(
            "{label}非法：{value}（允许 {}）",
            allowed.join(" / ")
        )))
    }
}

/// 判定附件扩展名是否为**可执行 / 可渲染**的危险类型（公告侧硬拦截）。
///
/// # 为什么策略白名单之外还要再拦一层
///
/// PRD §3.10：即使 `sys_storage_policy` 的白名单里出现 `.html`，公告也**不许**挂 ——
/// 富文本 + HTML 附件是钓鱼组合。策略是运维可改的种子数据，代码这一层是硬约束。
///
/// # 参数
///
/// - `file_name`：文件名（大小写不敏感）。
pub fn is_forbidden_attachment(file_name: &str) -> bool {
    const BLOCKED: [&str; 16] = [
        "html", "htm", "xhtml", "shtml", "svg", "xml", "js", "mjs", "exe", "msi", "bat", "cmd",
        "sh", "jar", "vbs", "ps1",
    ];
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .unwrap_or_default();
    BLOCKED.contains(&extension.as_str())
}

#[cfg(test)]
mod manage_tests {
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-29T10:00:00Z")
            .expect("固定时钟")
            .with_timezone(&chrono::Utc)
    }

    fn request() -> AnnouncementSaveReq {
        AnnouncementSaveReq {
            title: "关于 11 月 1 日系统维护的通知".into(),
            summary: Some("停机维护".into()),
            content_md: "## 维护时间\n\n- 22:00~24:00\n".into(),
            ..Default::default()
        }
    }

    /// 正常路径：缺省值全部落默认（NORMAL / ALL / 不置顶 / 无有效期）。
    #[test]
    fn normalize_save_defaults() {
        let normalized = normalize_save(&request(), now()).expect("合法请求应通过");
        assert_eq!(normalized.level, "NORMAL");
        assert_eq!(normalized.scope_type, "ALL");
        assert!(!normalized.is_top);
        assert!(normalized.top_expire_at.is_none());
        assert!(normalized.expire_at.is_none());
        assert!(normalized.scope_ids.is_empty());
        assert!(normalized.attachments.is_none());
        assert_eq!(normalized.title, "关于 11 月 1 日系统维护的通知");
    }

    /// 错误分支：标题空 / 正文空 → 62001。
    #[test]
    fn normalize_save_rejects_empty_title_and_body() {
        let mut req = request();
        req.title = "   ".into();
        let error = normalize_save(&req, now()).expect_err("空标题必须拒绝");
        assert_eq!((error.status, error.code), (400, 62001));

        let mut req = request();
        req.content_md = "\n\t ".into();
        let error = normalize_save(&req, now()).expect_err("空正文必须拒绝");
        assert_eq!((error.status, error.code), (400, 62001));
    }

    /// 错误分支：正文超 128 KiB → 62001（边界 128 KiB 本身放行）。
    #[test]
    fn normalize_save_body_limit() {
        let mut req = request();
        req.content_md = "a".repeat(MAX_CONTENT_BYTES);
        assert!(normalize_save(&req, now()).is_ok(), "恰好 128 KiB 应放行");

        req.content_md = "a".repeat(MAX_CONTENT_BYTES + 1);
        let error = normalize_save(&req, now()).expect_err("超 1 字节必须拒绝");
        assert_eq!(error.code, 62001);
        assert!(error.msg.contains("128 KiB"), "{}", error.msg);
    }

    /// 错误分支：范围非 ALL 但 scope_ids 为空 / 超 500 → 62006；重复 ID 去重。
    #[test]
    fn normalize_save_scope_rules() {
        let mut req = request();
        req.scope_type = Some("role".into());
        let error = normalize_save(&req, now()).expect_err("缺 scope_ids 必须拒绝");
        assert_eq!((error.status, error.code), (400, 62006));

        let duplicated = uuid::Uuid::now_v7().to_string();
        req.scope_ids = Some(vec![duplicated.clone(), duplicated.clone()]);
        let normalized = normalize_save(&req, now()).expect("重复 ID 应去重后放行");
        assert_eq!(normalized.scope_type, "ROLE");
        assert_eq!(normalized.scope_ids.len(), 1);

        let many: Vec<String> = (0..=MAX_SCOPE_IDS)
            .map(|_| uuid::Uuid::now_v7().to_string())
            .collect();
        req.scope_ids = Some(many);
        let error = normalize_save(&req, now()).expect_err("超 500 个必须拒绝");
        assert_eq!(error.code, 62006);
    }

    /// 错误分支：置顶缺 top_expire_at / 时间已过 → 62007；合法置顶放行。
    #[test]
    fn normalize_save_top_rules() {
        let mut req = request();
        req.is_top = Some(true);
        let error = normalize_save(&req, now()).expect_err("置顶必须带到期时间");
        assert_eq!((error.status, error.code), (400, 62007));

        req.top_expire_at = Some(Some("2026-10-29T09:00:00Z".into()));
        let error = normalize_save(&req, now()).expect_err("过去的置顶时间必须拒绝");
        assert_eq!(error.code, 62007);

        req.top_expire_at = Some(Some(default_top_expire_at(now()).to_rfc3339()));
        let normalized = normalize_save(&req, now()).expect("7 天后到期应放行");
        assert!(normalized.is_top);
        assert_eq!(normalized.top_expire_at, Some(default_top_expire_at(now())));
    }

    /// 错误分支：有效期已过 → 62008；带时区偏移的时间被转成 UTC。
    #[test]
    fn normalize_save_expire_rules() {
        let mut req = request();
        req.expire_at = Some(Some("2026-10-29T09:59:59Z".into()));
        let error = normalize_save(&req, now()).expect_err("过去的时间必须拒绝");
        assert_eq!((error.status, error.code), (400, 62008));

        req.expire_at = Some(Some("2026-11-08T08:00:00+08:00".into()));
        let normalized = normalize_save(&req, now()).expect("合法的未来时间应放行");
        assert_eq!(
            normalized.expire_at.map(|t| t.to_rfc3339()),
            Some("2026-11-08T00:00:00+00:00".to_string()),
            "必须统一转成 UTC"
        );
    }

    /// 错误分支：附件超 10 个 → 62001；file_id 非法 → 62003。
    #[test]
    fn normalize_save_attachment_rules() {
        let mut req = request();
        req.attachments = Some(
            (0..=MAX_ATTACHMENTS)
                .map(|_| AnnouncementAttachmentReq {
                    file_id: uuid::Uuid::now_v7().to_string(),
                    sort_order: None,
                })
                .collect(),
        );
        let error = normalize_save(&req, now()).expect_err("11 个附件必须拒绝");
        assert_eq!(error.code, 62001);
        assert!(error.msg.contains("最多上传 10 个附件"), "{}", error.msg);

        req.attachments = Some(vec![AnnouncementAttachmentReq {
            file_id: "not-a-uuid".into(),
            sort_order: None,
        }]);
        let error = normalize_save(&req, now()).expect_err("非法 file_id 必须拒绝");
        assert_eq!((error.status, error.code), (400, 62003));
    }

    /// 正常路径：附件排序号缺省取数组下标（前端不需要自己算）。
    #[test]
    fn normalize_save_attachment_sort_order_default() {
        let mut req = request();
        req.attachments = Some(vec![
            AnnouncementAttachmentReq {
                file_id: uuid::Uuid::now_v7().to_string(),
                sort_order: None,
            },
            AnnouncementAttachmentReq {
                file_id: uuid::Uuid::now_v7().to_string(),
                sort_order: Some(9),
            },
        ]);
        let normalized = normalize_save(&req, now()).expect("两个附件应放行");
        let items = normalized.attachments.expect("Some = 全量替换");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].1, 0, "缺省排序号 = 数组下标");
        assert_eq!(items[1].1, 9, "显式排序号优先");
    }

    /// 边界分支：取消置顶时强制清空 `top_expire_at`（否则撞 CHECK 约束）。
    #[test]
    fn normalize_top_clears_expire_when_unset() {
        let normalized = normalize_top(
            &AnnouncementTopReq {
                is_top: false,
                top_expire_at: Some("2026-12-01T00:00:00Z".into()),
            },
            now(),
        )
        .expect("取消置顶应放行");
        assert!(!normalized.is_top);
        assert!(
            normalized.top_expire_at.is_none(),
            "取消置顶必须清空到期时间"
        );
    }

    /// 错误分支：置顶缺时间 → 62007。
    #[test]
    fn normalize_top_requires_expire() {
        let error = normalize_top(
            &AnnouncementTopReq {
                is_top: true,
                top_expire_at: None,
            },
            now(),
        )
        .expect_err("置顶必须带时间");
        assert_eq!((error.status, error.code), (400, 62007));
    }

    /// 错误分支：时间格式非法 → 62001（不是 panic，也不是 500）。
    #[test]
    fn parse_rfc3339_rejects_garbage() {
        let error = parse_rfc3339(Some("2026-13-45"), "expire_at").expect_err("非法时间必须报错");
        assert_eq!((error.status, error.code), (400, 62001));
        assert_eq!(
            parse_rfc3339(Some("   "), "expire_at").expect("空白视为未传"),
            None
        );
    }

    /// 阅读率：接收人为 0 时不得出现 NaN（JSON 里 NaN 不合法）。
    #[test]
    fn read_rate_never_nan() {
        assert_eq!(read_rate(0, 0), 0.0);
        assert_eq!(read_rate(5, 0), 0.0);
        assert_eq!(read_rate(1, 4), 0.25);
        assert!(!read_rate(0, 0).is_nan());
    }

    /// 阅读名单状态过滤：非法值 → 400 + 10001。
    #[test]
    fn reader_state_parse() {
        assert_eq!(
            ReaderState::parse(None).expect("缺省 ALL"),
            ReaderState::All
        );
        assert_eq!(
            ReaderState::parse(Some("read")).expect("大小写不敏感"),
            ReaderState::Read
        );
        assert!(ReaderState::parse(Some("SOMETHING")).is_err());
    }

    /// 危险附件扩展名硬拦截（策略白名单之外的第二道闸）。
    #[test]
    fn forbidden_attachment_extensions() {
        for name in [
            "攻击.HTML",
            "x.htm",
            "logo.svg",
            "run.js",
            "tool.exe",
            "a.bat",
            "补丁.jar",
        ] {
            assert!(is_forbidden_attachment(name), "{name} 必须被拦截");
        }
        for name in ["通知.pdf", "名单.xlsx", "说明.docx", "归档.zip", "no-ext"] {
            assert!(!is_forbidden_attachment(name), "{name} 应放行");
        }
    }

    /// 默认置顶期 = 7 天（PRD 故事 1「置顶 7 天」）。
    #[test]
    fn default_top_expire_is_seven_days() {
        assert_eq!(
            default_top_expire_at(now()) - now(),
            chrono::Duration::days(7)
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn existing_row(
        scope_type: &str,
        scope_ids: Vec<uuid::Uuid>,
        is_top: bool,
        top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
        expire_at: Option<chrono::DateTime<chrono::Utc>>,
        remark: &str,
        level: &str,
    ) -> ManageDetailRow {
        ManageDetailRow {
            id: uuid::Uuid::now_v7(),
            ann_no: "AN20261029000001".into(),
            title: "旧标题".into(),
            summary: "旧摘要".into(),
            content_md: "旧正文".into(),
            content_html: "<p>旧正文</p>".into(),
            level: level.into(),
            scope_type: scope_type.into(),
            scope_ids,
            status: "DRAFT".into(),
            top_active: is_top,
            is_top_flag: is_top,
            top_expire_at,
            publish_at: None,
            expire_at,
            publisher_id: None,
            publisher_name: "运维".into(),
            notice_id: None,
            receiver_count: 0,
            read_count: 0,
            version: 1,
            remark: remark.into(),
            created_at: None,
            updated_at: None,
        }
    }

    /// **PUT 漏传范围**必须保持原范围（否则定向公告静默变全员可见）。
    #[test]
    fn merge_keeps_scope_when_omitted() {
        let role = uuid::Uuid::now_v7();
        let existing = existing_row("ROLE", vec![role], false, None, None, "", "IMPORTANT");
        let req = AnnouncementSaveReq {
            title: "新标题".into(),
            content_md: "新正文".into(),
            ..Default::default()
        };
        let merged = merge_with_existing(&req, &existing);
        assert_eq!(merged.scope_type.as_deref(), Some("ROLE"));
        assert_eq!(
            merged.scope_ids.as_deref(),
            Some([role.to_string()].as_slice())
        );
        // 级别 / 置顶 / 有效期的「未传」同样保持原值
        assert_eq!(merged.level.as_deref(), Some("IMPORTANT"));
        assert_eq!(merged.is_top, Some(false));
        assert_eq!(merged.remark, Some(None), "原备注为空 → 合并后仍为空");
        let normalized = normalize_save(&merged, now()).expect("合并后应通过校验");
        assert_eq!(normalized.scope_type, "ROLE");
        assert_eq!(normalized.scope_ids, vec![role]);
    }

    /// 显式 `null` 才是「清空有效期」（与「未传」严格区分）。
    #[test]
    fn merge_distinguishes_null_from_missing() {
        let expire = now() + chrono::Duration::days(30);
        let existing = existing_row(
            "ALL",
            Vec::new(),
            false,
            None,
            Some(expire),
            "旧备注",
            "NORMAL",
        );

        // 未传 → 保持原有效期
        let req = AnnouncementSaveReq {
            title: "标题".into(),
            content_md: "正文".into(),
            ..Default::default()
        };
        let merged = merge_with_existing(&req, &existing);
        assert_eq!(merged.expire_at, Some(Some(expire.to_rfc3339())));

        // 显式 null → 清空
        let req = AnnouncementSaveReq {
            title: "标题".into(),
            content_md: "正文".into(),
            expire_at: Some(None),
            remark: Some(None),
            ..Default::default()
        };
        let merged = merge_with_existing(&req, &existing);
        assert_eq!(merged.expire_at, Some(None), "显式 null 不得被原值覆盖");
        assert_eq!(merged.remark, Some(None));
        let normalized = normalize_save(&merged, now()).expect("清空后应通过校验");
        assert!(normalized.expire_at.is_none());
        assert!(normalized.remark.is_none());
    }

    /// 三态反序列化：字段缺席 / null / 有值 三种 JSON 形态各归其位。
    #[test]
    fn triple_state_deserialization() {
        let missing: AnnouncementSaveReq =
            serde_json::from_str(r#"{"title":"t","content_md":"c"}"#).expect("合法 JSON");
        assert_eq!(missing.expire_at, None, "缺席 = None（保持原值）");

        let explicit_null: AnnouncementSaveReq =
            serde_json::from_str(r#"{"title":"t","content_md":"c","expire_at":null}"#)
                .expect("合法 JSON");
        assert_eq!(explicit_null.expire_at, Some(None), "null = 清空");
        assert_eq!(explicit_null.top_expire_at, None);

        let valued: AnnouncementSaveReq = serde_json::from_str(
            r#"{"title":"t","content_md":"c","expire_at":"2026-11-08T00:00:00Z"}"#,
        )
        .expect("合法 JSON");
        assert_eq!(
            valued.expire_at,
            Some(Some("2026-11-08T00:00:00Z".to_string()))
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正常路径：下载地址带 `/api` 前缀，且**不含**任何存储细节。
    #[test]
    fn download_url_is_proxy_only() {
        let ann = uuid::Uuid::now_v7();
        let att = uuid::Uuid::now_v7();
        let url = download_url(&ann, &att);
        assert_eq!(
            url,
            format!("/api/business/announcement/{ann}/attachment/{att}")
        );
        assert!(
            url.starts_with("/api/"),
            "前端 `<a href>` 需要完整前缀：{url}"
        );
        for forbidden in ["bucket", "http://", "https://", "X-Amz", "minio", "storage"] {
            assert!(
                !url.contains(forbidden),
                "下载地址不得暴露存储后端（命中 {forbidden}）：{url}"
            );
        }
    }

    /// 边界分支：关键词只有空白 → 视为未传（不产生 `ILIKE '%%'` 全表命中）。
    #[test]
    fn keyword_blank_is_none() {
        let query = VisibleQuery {
            keyword: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(query.normalized_keyword(), None);
    }

    /// 安全：关键词里的 SQL 通配符被转义（`%` / `_` 不当作通配符）。
    #[test]
    fn keyword_escapes_like_wildcards() {
        let query = VisibleQuery {
            keyword: Some("100%_x".into()),
            ..Default::default()
        };
        let pattern = query.normalized_keyword().expect("非空关键词应有模式");
        assert!(pattern.contains("\\%"), "下划线/百分号必须转义：{pattern}");
        assert!(pattern.contains("\\_"), "下划线/百分号必须转义：{pattern}");
    }
}

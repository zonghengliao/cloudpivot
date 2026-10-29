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

use alun::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::announcement::model::{
    AnnouncementCardRow, AnnouncementDetailRow, AttachmentRow,
};

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

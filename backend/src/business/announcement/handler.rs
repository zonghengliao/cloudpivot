//! 公告**用户侧**接口（批次 C · C2）。
//!
//! 依据 `docs/批次C-技术架构.md` §2.8.1（用户侧契约）+ §2.5.3（附件代理下载的判定顺序）
//! + §2.4.3（XSS 四道闸）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏里只写 `/business/announcement/...`）
//!
//! ## 本卡（C2）的主契约
//!
//! | 方法 | 路径 | 说明 |
//! |------|------|------|
//! | GET | `/business/announcement/visible` | 我能看的公告列表（分页，置顶优先） |
//! | GET | `/business/announcement/visible/page` | 同上（兼容分页变体，语义完全一致） |
//! | GET | `/business/announcement/{id}` | 详情（**含 `content_html`**，出参前再净化） |
//! | POST | `/business/announcement/{id}/read` | 标已读（幂等） |
//! | GET | `/business/announcement/unread-count` | 未读数 |
//! | PUT | `/business/announcement/read-all` | 全部已读（只影响我的可见集合） |
//! | GET | `/business/announcement/{id}/attachment/{att_id}` | 附件代理下载 |
//!
//! ## 架构 §2.8.1 的路径别名（同一实现，仅为契约兼容）
//!
//! 架构文档把用户侧接口统一挂在 `/mine/*` 下（`mine/page`、`mine/{id}`、
//! `mine/{id}/read`、`mine/read-all`、`mine/unread-count`、
//! `{id}/attachments/{file_id}/download`）。C4/C5 的前端若按架构文档写，
//! 打到的是另一组路径。为避免「前端照文档写、后端照票面写」这种两套路径的扯皮，
//! 这里把架构的 6 条路径**作为别名**一并注册，**实现只有一份**（转发到同一批内部函数）。
//!
//! # 两条硬约束
//!
//! 1. **`content_html` 只能来自净化管线**：详情出参前必须走
//!    [`crate::business::shared::content::sanitize_html`]（闸 3）；快照缺失时从
//!    `content_md` 用 [`crate::business::shared::content::render`] 现渲染（闸 1 + 闸 2）。
//!    任何情况下都**不允许**把 Markdown 原文拼进 HTML。
//! 2. **附件下载必须过三道校验**：公告可见 → 附件属于该公告 → 文件存在，然后才由服务端
//!    流式代理。只走 `sys_file` 直链等于「任何登录用户可下载任意文件」（架构 §2.5.1 的 a 方案，
//!    已被否决）。
//!
//! # 权限
//!
//! 全部 7 个接口统一 `business:announcement:read`。该权限由 `V0087` 授予**所有**未软删角色
//! （公告栏是人人可见的入口，只授超管等于没发公告）。

use alun::prelude::*;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use crate::business::announcement::dto::{
    AnnouncementCardRes, AnnouncementDetailRes, MarkReadRes, ReadAllRes, UnreadCountRes,
    VisibleQuery,
};
use crate::business::announcement::model::{
    self, AnnouncementDetailRow, Audience, Visibility, VisibleFilter,
};
use crate::business::shared::content::{self, Recipe};
use crate::business::storage::model as storage_model;
use crate::business::storage::store::{LocalStore, ObjectStore, StoreError};
use crate::business::storage::{ensure_router, global_router};
use crate::shared::error_code;
use crate::utils;

// ==================== 列表 ====================

/// 我的可见公告分页（主契约）。
///
/// # 参数
///
/// - `claims`：当前用户（受众身份从库里现查，不信令牌快照）；
/// - `query`：`keyword` / `only_unread` / 分页。
///
/// # 返回值
///
/// `PageData<Vec<AnnouncementCardRes>>`，置顶优先 + 发布时间倒序。
///
/// # 异常情况
///
/// - 401：令牌缺用户标识；
/// - 403：无 `business:announcement:read`（`V0087` 已授予所有角色）；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/announcement/visible",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/visible")]
pub async fn visible(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<VisibleQuery>,
) -> ResResult<PageData<Vec<AnnouncementCardRes>>> {
    page_visible(&claims, query).await
}

/// 我的可见公告分页（兼容变体：`/visible/page` 与 `/visible` 语义完全一致）。
///
/// # 说明
///
/// 票面把「兼容分页变体」列为交付项；架构文档的分页惯例是 `.../page` 后缀。
/// 两者共用 [`page_visible`]，不存在第二套查询逻辑（避免「换个路径少一个条件」）。
#[alun::permission(
    path = "/business/announcement/visible/page",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/visible/page")]
pub async fn visible_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<VisibleQuery>,
) -> ResResult<PageData<Vec<AnnouncementCardRes>>> {
    page_visible(&claims, query).await
}

/// 我的可见公告分页（架构 §2.8.1 别名：`/business/announcement/mine/page`）。
#[alun::permission(
    path = "/business/announcement/mine/page",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/mine/page")]
pub async fn mine_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<VisibleQuery>,
) -> ResResult<PageData<Vec<AnnouncementCardRes>>> {
    page_visible(&claims, query).await
}

/// 列表查询的公共实现（三个路径共用）。
async fn page_visible(
    claims: &TokenClaims,
    query: VisibleQuery,
) -> ResResult<PageData<Vec<AnnouncementCardRes>>> {
    let user_id = current_user(claims)?;
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = VisibleFilter {
        keyword: query.normalized_keyword(),
        only_unread: query.only_unread.unwrap_or(false),
    };
    let audience = load_audience(&user_id).await?;
    let result = model::page_visible(&audience, &filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询我的公告"))?;
    let list = result
        .list
        .iter()
        .map(AnnouncementCardRes::from_row)
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

// ==================== 未读数 ====================

/// 公告未读数。
///
/// # 返回值
///
/// `{ "announcement_unread": n }`（字段名与通知未读 `unread_count` **刻意不同**，
/// 前端顶栏两个角标不会互相覆盖）。
///
/// # 口径
///
/// 未读 = 可见 ∧ 未过期 ∧ 无已读回执；与 `visible?only_unread=true` 的 `total` **恒等**
/// （A-10 交叉断言）。
#[alun::permission(
    path = "/business/announcement/unread-count",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/unread-count")]
pub async fn unread_count(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<UnreadCountRes> {
    count_unread(&claims).await
}

/// 公告未读数（架构 §2.8.1 别名：`/business/announcement/mine/unread-count`）。
#[alun::permission(
    path = "/business/announcement/mine/unread-count",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/mine/unread-count")]
pub async fn mine_unread_count(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<UnreadCountRes> {
    count_unread(&claims).await
}

async fn count_unread(claims: &TokenClaims) -> ResResult<UnreadCountRes> {
    let user_id = current_user(claims)?;
    let audience = load_audience(&user_id).await?;
    let announcement_unread = model::count_unread(&audience)
        .await
        .map_err(|error| utils::db_error(error, "统计公告未读数"))?;
    Ok(Res::ok(UnreadCountRes {
        announcement_unread,
    }))
}

// ==================== 详情 ====================

/// 公告详情（含净化后的 `content_html` 与附件列表）。
///
/// # 异常情况
///
/// - 404 + 62002：公告不存在 / 已软删；
/// - 403 + 62005：未发布 / 已下线 / 已过有效期；
/// - 403 + 62004：发布范围不含当前用户。
#[alun::permission(
    path = "/business/announcement/{id}",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/{id}")]
pub async fn detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<AnnouncementDetailRes> {
    announcement_detail(&claims, &id).await
}

/// 公告详情（架构 §2.8.1 别名：`/business/announcement/mine/{id}`）。
#[alun::permission(
    path = "/business/announcement/mine/{id}",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/mine/{id}")]
pub async fn mine_detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<AnnouncementDetailRes> {
    announcement_detail(&claims, &id).await
}

async fn announcement_detail(
    claims: &TokenClaims,
    raw_id: &str,
) -> ResResult<AnnouncementDetailRes> {
    let user_id = current_user(claims)?;
    let announcement_id = utils::require_uuid(raw_id, "id")?;
    let audience = load_audience(&user_id).await?;
    let visibility = model::check_visibility(&audience, &announcement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询公告详情"))?;
    let row = match visibility {
        Visibility::Visible(row) => row,
        other => return Err(visibility_error(&other)),
    };
    let attachments = model::list_attachments(&announcement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询公告附件"))?;
    let html = detail_html(&row);
    Ok(Res::ok(AnnouncementDetailRes::from_row(
        &row,
        html,
        &attachments,
    )))
}

/// 详情出参的 HTML（**闸 3**：出参前再净化一次）。
///
/// # 参数
///
/// - `row`：详情行（`content_html` 是写入时的渲染快照）。
///
/// # 返回值
///
/// 可直接下发的 HTML：
///
/// 1. 快照为空（历史数据 / 写路径异常）→ 用 [`content::render`] 从 `content_md`
///    **现渲染**（`Recipe::Announcement`，`ENABLE_HTML=false`）；
/// 2. 否则用快照，但**一律再净化一次** —— 即使有人绕过发布接口手工改库塞了
///    `<script>` / `<img onerror>`，出参也是干净的（防御纵深）。
///
/// # 为什么不在列表接口也做
///
/// 列表不返回 HTML（只返回摘要纯文本），净化在这里只对详情付出一次 0.6 ms/20 KB 的成本。
fn detail_html(row: &AnnouncementDetailRow) -> String {
    let snapshot = if row.content_html.trim().is_empty() {
        content::render(&row.content_md, Recipe::Announcement).content_html
    } else {
        row.content_html.clone()
    };
    content::sanitize_html(&snapshot, Recipe::Announcement)
}

// ==================== 已读 ====================

/// 标记单条已读（幂等）。
///
/// # 返回值
///
/// `{ read_count, first_read }`：重复点击不会重复计数（`uk_sys_announcement_read`）。
///
/// # 异常情况
///
/// 同 [`detail`]（不可见的公告不允许写已读回执）。
#[alun::permission(
    path = "/business/announcement/{id}/read",
    method = "POST",
    permission = "business:announcement:read"
)]
#[alun::post("/business/announcement/{id}/read")]
pub async fn mark_read(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<MarkReadRes> {
    announcement_read(&claims, &id).await
}

/// 标记单条已读（架构 §2.8.1 别名：`POST /business/announcement/mine/{id}/read`）。
#[alun::permission(
    path = "/business/announcement/mine/{id}/read",
    method = "POST",
    permission = "business:announcement:read"
)]
#[alun::post("/business/announcement/mine/{id}/read")]
pub async fn mine_mark_read(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<MarkReadRes> {
    announcement_read(&claims, &id).await
}

async fn announcement_read(claims: &TokenClaims, raw_id: &str) -> ResResult<MarkReadRes> {
    let user_id = current_user(claims)?;
    let announcement_id = utils::require_uuid(raw_id, "id")?;
    let audience = load_audience(&user_id).await?;
    // 先判可见：不可见的公告不允许留下已读回执（否则未读数会被外部噪声污染）
    let visibility = model::check_visibility(&audience, &announcement_id)
        .await
        .map_err(|error| utils::db_error(error, "校验公告可见性"))?;
    if !matches!(visibility, Visibility::Visible(_)) {
        return Err(visibility_error(&visibility));
    }
    let (first_read, read_count) = model::mark_read(&announcement_id, &user_id)
        .await
        .map_err(|error| utils::db_error(error, "标记公告已读"))?;
    Ok(Res::ok(MarkReadRes {
        read_count,
        first_read,
    }))
}

/// 全部已读（只影响「我的可见集合」，不越权写别人的回执）。
///
/// # 返回值
///
/// `{ affected }`：本次新增的已读回执行数（重复调用为 0）。
#[alun::permission(
    path = "/business/announcement/read-all",
    method = "PUT",
    permission = "business:announcement:read"
)]
#[alun::put("/business/announcement/read-all")]
pub async fn read_all(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<ReadAllRes> {
    mark_all_read(&claims).await
}

/// 全部已读（架构 §2.8.1 别名：`PUT /business/announcement/mine/read-all`）。
#[alun::permission(
    path = "/business/announcement/mine/read-all",
    method = "PUT",
    permission = "business:announcement:read"
)]
#[alun::put("/business/announcement/mine/read-all")]
pub async fn mine_read_all(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<ReadAllRes> {
    mark_all_read(&claims).await
}

async fn mark_all_read(claims: &TokenClaims) -> ResResult<ReadAllRes> {
    let user_id = current_user(claims)?;
    let audience = load_audience(&user_id).await?;
    let affected = model::mark_all_read(&audience)
        .await
        .map_err(|error| utils::db_error(error, "全部标记公告已读"))?;
    Ok(Res::ok(ReadAllRes { affected }))
}

// ==================== 附件代理下载 ====================

/// 附件代理下载（**唯一**可下载入口）。
///
/// # 判定顺序（架构 §2.5.3，逐条对应一个错误码）
///
/// 1. 公告不存在 / 已软删 → `404 + 62002`；
/// 2. 未发布 / 已下线 / 已过有效期 → `403 + 62005`；
/// 3. 发布范围不含当前用户 → `403 + 62004`；
/// 4. 附件不属于该公告 → `404 + 62003`（防「拿别的公告的附件 id」）；
/// 5. 文件记录不存在 / 已软删 → `404 + 62003`；对象丢失 → `404 + 62003`。
///
/// # 响应头（防存储型 XSS）
///
/// `Content-Disposition: attachment`（**强制**下载，绝不 inline）、
/// `X-Content-Type-Options: nosniff`、`Cache-Control: private, max-age=300`。
/// `text/html` 的 MIME 会被降级成 `application/octet-stream`（双保险）。
///
/// # 为什么不 302 到预签名 URL
///
/// 预签名 URL 的有效期内**任何人拿到都能下载**（无身份），而公告的可见性会变化
/// （下线 / 过期 / 改范围）。代理接口每次请求都重新判定，语义正确（架构 §2.5.3 末段）。
#[alun::permission(
    path = "/business/announcement/{id}/attachment/{att_id}",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/{id}/attachment/{att_id}")]
pub async fn download_attachment(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, att_id)): Path<(String, String)>,
) -> Response {
    match attachment_response(&claims, &id, &att_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 附件代理下载（架构 §2.8.1 别名：`/business/announcement/{id}/attachments/{file_id}/download`）。
///
/// # 参数说明
///
/// 架构文档把路径参数叫 `file_id`，但**语义上是附件绑定行主键**（`sys_announcement_attachment.id`）：
/// 只有绑定行 id 才能同时满足「属于这条公告」与「不暴露 `sys_file` 的内部 id」。
/// 两个路径参数在这里按同一口径处理（`{file_id}` 视作 `att_id`）。
#[alun::permission(
    path = "/business/announcement/{id}/attachments/{file_id}/download",
    method = "GET",
    permission = "business:announcement:read"
)]
#[alun::get("/business/announcement/{id}/attachments/{file_id}/download")]
pub async fn download_attachment_alias(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, file_id)): Path<(String, String)>,
) -> Response {
    match attachment_response(&claims, &id, &file_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn attachment_response(
    claims: &TokenClaims,
    raw_announcement_id: &str,
    raw_attachment_id: &str,
) -> std::result::Result<Response, ApiError> {
    let user_id = current_user(claims)?;
    let announcement_id = utils::require_uuid(raw_announcement_id, "id")?;
    let attachment_id = utils::require_uuid(raw_attachment_id, "att_id")?;

    let audience = load_audience(&user_id).await?;
    let visibility = model::check_visibility(&audience, &announcement_id)
        .await
        .map_err(|error| utils::db_error(error, "校验公告可见性"))?;
    if !matches!(visibility, Visibility::Visible(_)) {
        return Err(visibility_error(&visibility));
    }

    let attachment = model::find_attachment(&announcement_id, &attachment_id)
        .await
        .map_err(|error| utils::db_error(error, "查询公告附件"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::ANNOUNCEMENT_ATTACHMENT_NOT_FOUND,
                "附件不存在或不属于该公告",
            )
        })?;

    let file = storage_model::find_active_file(&attachment.file_id)
        .await
        .map_err(|error| utils::db_error(error, "查询附件文件记录"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::ANNOUNCEMENT_ATTACHMENT_NOT_FOUND,
                "附件文件不存在或已删除",
            )
        })?;

    let backend = resolve_file_store(file.storage_code.as_deref()).await?;
    let stat = backend
        .stat(&file.file_key)
        .await
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::ANNOUNCEMENT_ATTACHMENT_NOT_FOUND,
                "附件对象已丢失（记录存在但文件被清理）",
            )
        })?;
    let reader = backend
        .stream(&file.file_key, 0)
        .await
        .map_err(map_store_error)?;

    let mime = download_mime(attachment.mime_type.as_deref(), file.mime_type.as_deref());
    let mut response = Response::new(Body::from_stream(ReaderStream::new(reader)));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&stat.size.to_string())
            .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "响应头构造失败"))?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&attachment.file_name),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=300"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// 解析附件所在存储后端（读路径）。
///
/// # 参数
///
/// - `storage_code`：`sys_file.storage_code`；`None` / 空串表示 A1 之前的历史文件。
///
/// # 说明
///
/// 与 `business/storage/handler.rs::resolve_file_store` 同语义（那是私有函数，
/// 本卡不能改批次 A 的模块，故在此按同一口径实现）：
/// `storage_code IS NULL` ⇒ **恒按本地默认目录**读（否则把默认后端切成 MinIO 后，
/// 老附件会全部 404）。
///
/// # 异常情况
///
/// - 409：记录指向的后端已停用 / 删除；
/// - 500：路由表装载失败（主密钥缺失等）。
async fn resolve_file_store(
    storage_code: Option<&str>,
) -> std::result::Result<Arc<dyn ObjectStore>, ApiError> {
    if let Some(code) = storage_code.map(str::trim).filter(|code| !code.is_empty()) {
        ensure_router().await?;
        return global_router().by_code(code).ok_or_else(|| {
            ApiError::new(
                409,
                error_code::STORAGE_BACKEND_UNAVAILABLE,
                format!("附件所在存储后端 {code} 已停用或删除，请重新上传"),
            )
        });
    }
    Ok(Arc::new(LocalStore::new("local-default", "")))
}

/// 下载用的 MIME：优先附件绑定行，其次文件记录，最后兜底 `application/octet-stream`。
///
/// # 安全
///
/// `text/html`（以及 `application/xhtml`、`image/svg+xml` 这类可执行标记语言）一律降级为
/// `application/octet-stream`：即使某天有人去掉 `Content-Disposition`，
/// 浏览器也不会把附件当页面渲染（架构 §2.4.3 加固项、A-12 断言）。
fn download_mime(attachment_mime: Option<&str>, file_mime: Option<&str>) -> String {
    let raw = attachment_mime
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| file_mime.map(str::trim).filter(|value| !value.is_empty()))
        .unwrap_or("");
    if raw.is_empty() {
        return "application/octet-stream".to_string();
    }
    let lowered = raw.to_ascii_lowercase();
    if lowered.starts_with("text/html")
        || lowered.starts_with("application/xhtml")
        || lowered.contains("svg")
    {
        return "application/octet-stream".to_string();
    }
    lowered
}

/// `Content-Disposition: attachment`（RFC 6266 / RFC 5987）。
///
/// # 参数
///
/// - `original`：原始文件名（可能是中文）。
///
/// # 返回值
///
/// 恒为 `attachment; filename="<ASCII 回退名>"; filename*=UTF-8''<百分号编码>`：
/// ASCII 部分把非 ASCII 与引号 / 反斜杠替换成 `_`（不破坏头语法），
/// 中文走 `filename*`（浏览器优先采用）。
fn content_disposition(original: &str) -> HeaderValue {
    let ascii: String = original
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' && !c.is_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in original.as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if keep {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// 存储错误 → 统一出口（状态码 + 业务码）。
fn map_store_error(error: StoreError) -> ApiError {
    let (status, code) = match &error {
        StoreError::NotFound(_) => (404, error_code::STORAGE_OBJECT_MISSING),
        StoreError::Invalid(_) => (400, error_code::PARAM_INVALID),
        StoreError::Unsupported(_) => (422, error_code::STORAGE_DIRECT_UPLOAD_DISABLED),
        StoreError::Io(_) => (500, error_code::INTERNAL_ERROR),
        StoreError::Backend(_) => (502, error_code::INTERNAL_ERROR),
    };
    tracing::warn!(status, error = %error, "公告附件下载：存储后端操作失败");
    ApiError::new(status, code, error.message())
}

// ==================== 公共辅助 ====================

/// 可见性判定结果 → 错误（判定顺序已在 SQL 里固定，这里只做映射）。
fn visibility_error(visibility: &Visibility) -> ApiError {
    match visibility {
        Visibility::NotFound => ApiError::new(
            404,
            error_code::ANNOUNCEMENT_NOT_FOUND,
            "公告不存在或已删除",
        ),
        Visibility::NotAvailable => ApiError::new(
            403,
            error_code::ANNOUNCEMENT_NOT_AVAILABLE,
            "公告未发布、已下线或已过有效期",
        ),
        Visibility::Forbidden => ApiError::new(
            403,
            error_code::ANNOUNCEMENT_FORBIDDEN,
            "该公告的发布范围不包含当前用户",
        ),
        // Visible 分支在调用方已提前返回；走到这里说明调用顺序写错了（接入缺陷，报 500）
        Visibility::Visible(_) => ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            "可见性判定内部错误（Visible 不应进入错误映射）",
        ),
    }
}

/// 取当前用户 ID（`claims.sub`）。
fn current_user(claims: &TokenClaims) -> std::result::Result<uuid::Uuid, ApiError> {
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))
}

/// 加载受众身份（`Audience`），DB 错误转成统一出口。
async fn load_audience(user_id: &uuid::Uuid) -> std::result::Result<Audience, ApiError> {
    model::load_audience(user_id)
        .await
        .map_err(|error| utils::db_error(error, "解析公告受众身份"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail_row(content_md: &str, content_html: &str) -> AnnouncementDetailRow {
        AnnouncementDetailRow {
            id: uuid::Uuid::now_v7(),
            ann_no: "AN202610290001".into(),
            title: "维护通知".into(),
            summary: String::new(),
            content_md: content_md.into(),
            content_html: content_html.into(),
            level: "IMPORTANT".into(),
            scope_type: "ALL".into(),
            status: "PUBLISHED".into(),
            is_top: false,
            publish_at: None,
            expire_at: None,
            publisher_name: "运维".into(),
            read_flag: false,
            read_at: None,
            attachment_count: 0,
            scope_ok: true,
            live: true,
        }
    }

    /// 正常路径：Markdown → HTML 走净化管线，表格 / 标题都保留。
    #[test]
    fn detail_html_renders_markdown() {
        let row = detail_row(
            "## 维护时间\n\n| 系统 | 影响 |\n|---|---|\n| 云枢 | 不可用 |\n",
            "",
        );
        let html = detail_html(&row);
        // 标题层级强制偏移 +1（`##` → `<h3>`），页面已有 `<h1>`，正文不再出现 h1
        assert!(html.contains("<h3>维护时间</h3>"), "{html}");
        assert!(html.contains("<table>"), "{html}");
        assert!(!html.contains("<h1>"), "标题层级必须偏移：{html}");
    }

    /// A-13 XSS（脏数据路径）：库里被手工塞了 `<img onerror>` → 出参被净化。
    #[test]
    fn detail_html_sanitizes_dirty_snapshot() {
        let row = detail_row(
            "正文",
            "<p>看图</p><img src=\"x\" onerror=\"alert(1)\"><script>alert(2)</script>",
        );
        let html = detail_html(&row);
        assert!(!html.contains("onerror"), "闸 3 失效：{html}");
        assert!(!html.contains("<script"), "闸 3 失效：{html}");
        assert!(!html.to_ascii_lowercase().contains("alert"), "{html}");
        // 合法部分保留（净化不是「全删」）
        assert!(html.contains("<p>看图</p>"), "{html}");
    }

    /// A-14 XSS（Markdown 内嵌 HTML）：`ENABLE_HTML=false` → 原始 HTML 降级为转义文本。
    ///
    /// 断言口径要精确：转义后的文本里**字符串** `onerror=alert(1)` 仍然存在，但它是
    /// **文本节点**（`&lt;img …&gt;`），浏览器不会解析成元素，因此不构成可执行 XSS。
    /// 要断言的是「没有真实的 `<img` 标签」，而不是「没有这个子串」。
    #[test]
    fn detail_html_escapes_raw_html_in_markdown() {
        let row = detail_row("<img src=x onerror=alert(1)>", "");
        let html = detail_html(&row);
        assert!(!html.contains("<img"), "ENABLE_HTML 必须为 false：{html}");
        assert!(html.contains("&lt;img"), "应降级为转义的纯文本：{html}");
        assert!(
            !html.contains("<img src=x"),
            "转义不彻底会形成真实标签：{html}"
        );
    }

    /// 边界分支：快照为空 → 从 Markdown 现渲染（不返回空串，也不拼原文）。
    #[test]
    fn detail_html_falls_back_to_markdown_when_snapshot_empty() {
        let row = detail_row("**加粗**", "   ");
        let html = detail_html(&row);
        assert!(html.contains("<strong>加粗</strong>"), "{html}");
    }

    /// A-12：下载 MIME 不允许 `text/html` / SVG（防存储型 XSS）。
    #[test]
    fn download_mime_downgrades_active_content() {
        assert_eq!(
            download_mime(Some("text/html"), None),
            "application/octet-stream"
        );
        assert_eq!(
            download_mime(Some("image/svg+xml"), None),
            "application/octet-stream"
        );
        assert_eq!(
            download_mime(Some("application/pdf"), None),
            "application/pdf"
        );
        // 绑定行没有 MIME → 回落文件记录
        assert_eq!(download_mime(None, Some("text/plain")), "text/plain");
        // 都没有 → octet-stream
        assert_eq!(download_mime(None, None), "application/octet-stream");
    }

    /// A-12：`Content-Disposition` 恒以 `attachment` 开头，中文走 RFC 5987。
    #[test]
    fn content_disposition_is_attachment() {
        let value = content_disposition("运维手册 2026.pdf");
        let text = value.to_str().expect("头值应为可见 ASCII");
        assert!(text.starts_with("attachment;"), "{text}");
        assert!(text.contains("filename*=UTF-8''"), "{text}");
        assert!(text.contains("%20"), "{text}");
        assert!(
            !text.contains("运维"),
            "ASCII 回退名里不能出现中文（会破坏头语法）：{text}"
        );
        // 引号 / 反斜杠不能破坏头语法
        let quoted = content_disposition("a\"b\\c.txt");
        let text = quoted.to_str().expect("头值应为可见 ASCII");
        assert!(!text.contains("a\"b"), "{text}");
    }

    /// 可见性 → 错误码映射（A-11 的三个码各就各位）。
    #[test]
    fn visibility_errors_map_to_codes() {
        let not_found = visibility_error(&Visibility::NotFound);
        assert_eq!((not_found.status, not_found.code), (404, 62002));

        let not_available = visibility_error(&Visibility::NotAvailable);
        assert_eq!((not_available.status, not_available.code), (403, 62005));

        let forbidden = visibility_error(&Visibility::Forbidden);
        assert_eq!((forbidden.status, forbidden.code), (403, 62004));
    }

    /// `claims.sub` 非法时按 401 处理，不能 panic。
    #[test]
    fn current_user_rejects_bad_subject() {
        let claims = TokenClaims {
            jti: None,
            sub: "not-a-uuid".to_string(),
            username: None,
            roles: Vec::new(),
            permissions: Vec::new(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        };
        let error = current_user(&claims).expect_err("非法 sub 必须报错");
        assert_eq!(error.status, 401);
    }
}

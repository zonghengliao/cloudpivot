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

use axum::Json;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nik::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use crate::business::announcement::dto::{
    AnnouncementAttachmentBindReq, AnnouncementCardRes, AnnouncementDetailRes,
    AnnouncementManageDetailRes, AnnouncementOfflineReq, AnnouncementOfflineRes,
    AnnouncementPreviewReq, AnnouncementPreviewRes, AnnouncementPublishRes, AnnouncementSaveReq,
    AnnouncementSaveRes, AnnouncementTopReq, AnnouncementTopRes, AttachmentRes, ManageCardRes,
    ManagePageQuery, MarkReadRes, ReadAllRes, ReaderRes, ReaderState, ReadersQuery, ReadersRes,
    UnreadCountRes, VisibleQuery,
};
use crate::business::announcement::model::{
    self, AnnouncementDetailRow, Audience, ManageFilter, ReaderStateFilter, Visibility,
    VisibleFilter,
};
use crate::business::announcement::{dto, maintenance};
use crate::business::changelog::{self, Action, ChangeTarget, DiffInput};
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
#[nik::permission(
    path = "/business/announcement/visible",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/visible")]
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
#[nik::permission(
    path = "/business/announcement/visible/page",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/visible/page")]
pub async fn visible_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<VisibleQuery>,
) -> ResResult<PageData<Vec<AnnouncementCardRes>>> {
    page_visible(&claims, query).await
}

/// 我的可见公告分页（架构 §2.8.1 别名：`/business/announcement/mine/page`）。
#[nik::permission(
    path = "/business/announcement/mine/page",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/mine/page")]
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
#[nik::permission(
    path = "/business/announcement/unread-count",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/unread-count")]
pub async fn unread_count(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<UnreadCountRes> {
    count_unread(&claims).await
}

/// 公告未读数（架构 §2.8.1 别名：`/business/announcement/mine/unread-count`）。
#[nik::permission(
    path = "/business/announcement/mine/unread-count",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/mine/unread-count")]
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
#[nik::permission(
    path = "/business/announcement/{id}",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/{id}")]
pub async fn detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<AnnouncementDetailRes> {
    announcement_detail(&claims, &id).await
}

/// 公告详情（架构 §2.8.1 别名：`/business/announcement/mine/{id}`）。
#[nik::permission(
    path = "/business/announcement/mine/{id}",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/mine/{id}")]
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
#[nik::permission(
    path = "/business/announcement/{id}/read",
    method = "POST",
    permission = "business:announcement:read"
)]
#[nik::post("/business/announcement/{id}/read")]
pub async fn mark_read(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<MarkReadRes> {
    announcement_read(&claims, &id).await
}

/// 标记单条已读（架构 §2.8.1 别名：`POST /business/announcement/mine/{id}/read`）。
#[nik::permission(
    path = "/business/announcement/mine/{id}/read",
    method = "POST",
    permission = "business:announcement:read"
)]
#[nik::post("/business/announcement/mine/{id}/read")]
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
#[nik::permission(
    path = "/business/announcement/read-all",
    method = "PUT",
    permission = "business:announcement:read"
)]
#[nik::put("/business/announcement/read-all")]
pub async fn read_all(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<ReadAllRes> {
    mark_all_read(&claims).await
}

/// 全部已读（架构 §2.8.1 别名：`PUT /business/announcement/mine/read-all`）。
#[nik::permission(
    path = "/business/announcement/mine/read-all",
    method = "PUT",
    permission = "business:announcement:read"
)]
#[nik::put("/business/announcement/mine/read-all")]
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
#[nik::permission(
    path = "/business/announcement/{id}/attachment/{att_id}",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/{id}/attachment/{att_id}")]
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
#[nik::permission(
    path = "/business/announcement/{id}/attachments/{file_id}/download",
    method = "GET",
    permission = "business:announcement:read"
)]
#[nik::get("/business/announcement/{id}/attachments/{file_id}/download")]
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

// ==================== 管理侧接口（批次 C · C3）====================
//
// 依据 `docs/批次C-技术架构.md` §2.8.2（管理侧契约）+ §2.8.3（校验规则表）+ §2.2（发布扇出）
// + §2.6（有效期与自动下线）；PRD A1-01 ~ A1-10。
//
// # 接口一览（`/api` 前缀由 `[router].prefix` 注入）
//
// | # | 方法 | 路径 | 权限码 | 说明 |
// |---|------|------|--------|------|
// | 1 | GET | `/business/announcement/page` | `business:announcement:list` | 管理列表（含草稿 / 已发布 / 已下线 / 已过期） |
// | 2 | POST | `/business/announcement` | `business:announcement:create` | 新建（`DRAFT`，可带附件与置顶） |
// | 3 | PUT | `/business/announcement/{id}` | `business:announcement:update` | 编辑（仅 `DRAFT` / `OFFLINE` / `EXPIRED`） |
// | 4 | DELETE | `/business/announcement/{id}` | `business:announcement:delete` | 软删 |
// | 5 | POST | `/business/announcement/{id}/publish` | `business:announcement:publish` | 发布（状态机 + `version+1` + 扇出 + 补偿兜底） |
// | 6 | POST | `/business/announcement/{id}/offline` | `business:announcement:offline` | 下线（必填 `reason`） |
// | 7 | POST | `/business/announcement/preview` | `business:announcement:create` | 预览（Markdown → 净化 HTML，不入库） |
// | 8 | POST | `/business/announcement/{id}/attachment` | `business:announcement:update` | 绑定附件（文件本体走 `business/storage`） |
// | 9 | GET | `/business/announcement/{id}/readers` | `business:announcement:list` | 阅读名单（谁读了 / 谁没读） |
// | 10 | PUT | `/business/announcement/{id}/top` | `business:announcement:update` | 置顶 / 取消置顶 |
//
// 另有 3 个**配套**路径（不是新增能力，只为契约兼容 / 编辑器可用）：
//
// | 方法 | 路径 | 说明 |
// |------|------|------|
// | GET | `/business/announcement/{id}/manage` | 管理详情（`content_md` 原文 + 范围明细 + 附件）—— 架构 §2.8.2 的 `GET /{id}` 已被 C2 的用户侧占用同一路径，管理侧只能换个段名 |
// | GET | `/business/announcement/{id}/read-stats` | 阅读统计（架构 §2.8.2 的名称，与 `readers` 同源同实现） |
// | POST | `/business/announcement/{id}/top` | 置顶（架构 §2.8.2 写的是 POST，票面写的是 PUT，两个动词都注册，同一实现） |
//
// # 三条硬约束
//
// 1. **渲染 + 净化只走 `business::shared::content`**（与 C2 同一条管线，`Recipe::Announcement`）：
//    预览、发布快照、管理详情出参三处都过它，前端不自行渲染（§2.4.2）；
// 2. **扇出只走 `maintenance::dispatch_announcement` → `notice::dispatch()`**，幂等键
//    `announcement:{id}:v{version}`（§2.2）；
// 3. **状态机只有 `model::ensure_*` 四个判定**，不在这里散写 `if status == ...`。

/// 管理列表。
///
/// # 参数
///
/// - `query`：`keyword` / `status` / `scope_type` / `level` / 发布时间窗 / 分页。
///
/// # 返回值
///
/// `PageData<Vec<ManageCardRes>>`：置顶生效优先 → 发布时间倒序。
///
/// # 异常情况
///
/// - 400 + `10001`：状态 / 范围 / 级别取值非法、时间格式非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/announcement/page",
    method = "GET",
    permission = "business:announcement:list"
)]
#[nik::get("/business/announcement/page")]
pub async fn manage_page(
    Query(query): Query<ManagePageQuery>,
) -> ResResult<PageData<Vec<ManageCardRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = manage_filter(&query)?;
    let result = model::page_manage(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询公告（管理端）"))?;
    let list = result.list.iter().map(ManageCardRes::from_row).collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 把查询参数解析成 [`ManageFilter`]（纯函数，便于单测）。
///
/// # 异常情况
///
/// 状态 / 范围 / 级别取值非法 → `400 + 10001`；时间格式非法 → `400 + 62001`。
fn manage_filter(query: &ManagePageQuery) -> std::result::Result<ManageFilter, ApiError> {
    let mut statuses = Vec::new();
    for raw in query.status.as_deref().unwrap_or_default().split(',') {
        let value = raw.trim().to_ascii_uppercase();
        if value.is_empty() {
            continue;
        }
        if ![
            model::STATUS_DRAFT,
            model::STATUS_PUBLISHED,
            model::STATUS_OFFLINE,
            model::STATUS_EXPIRED,
        ]
        .contains(&value.as_str())
        {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("status 非法：{value}（允许 DRAFT/PUBLISHED/OFFLINE/EXPIRED）"),
            ));
        }
        if !statuses.contains(&value) {
            statuses.push(value);
        }
    }
    let scope_type = enum_param(
        query.scope_type.as_deref(),
        &["ALL", "ROLE", "DEPT", "USER"],
        "scope_type",
    )?;
    let level = enum_param(
        query.level.as_deref(),
        &["NORMAL", "IMPORTANT", "URGENT"],
        "level",
    )?;
    let publish_from = dto::parse_rfc3339(query.publish_from.as_deref(), "publish_from")?;
    let publish_to = dto::parse_rfc3339(query.publish_to.as_deref(), "publish_to")?;
    if let (Some(from), Some(to)) = (publish_from, publish_to)
        && from > to
    {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            "发布时间窗起点晚于终点",
        ));
    }
    Ok(ManageFilter {
        keyword: utils::like_pattern(query.keyword.as_deref()),
        statuses,
        scope_type,
        level,
        publish_from,
        publish_to,
    })
}

/// 归一化可空枚举查询参数（空白视为未传）。
fn enum_param(
    raw: Option<&str>,
    allowed: &[&str],
    field: &str,
) -> std::result::Result<Option<String>, ApiError> {
    let Some(value) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let upper = value.to_ascii_uppercase();
    if allowed.contains(&upper.as_str()) {
        Ok(Some(upper))
    } else {
        Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("{field} 非法：{value}（允许 {}）", allowed.join(" / ")),
        ))
    }
}

/// 新建公告（草稿）。
///
/// # 参数
///
/// - `claims`：操作人（`publisher_id` / 审计列）；
/// - `req`：见 `AnnouncementSaveReq`（§2.8.3 的请求体）。
///
/// # 返回值
///
/// `{ id, ann_no, status: "DRAFT", version: 1 }`。
///
/// # 异常情况
///
/// - 400 + `62001`：标题 / 正文 / 摘要 / 备注不合规、附件超数量；
/// - 400 + `62003`：附件 `file_id` 不存在或已删；
/// - 400 + `62006`：范围明细缺失 / 超 500；
/// - 400 + `62007`：置顶缺到期时间；
/// - 400 + `62008`：有效期已过；
/// - 400 + `60003`~`60006`：存储策略不允许该附件（大小 / 扩展名 / MIME）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/announcement",
    method = "POST",
    permission = "business:announcement:create"
)]
#[nik::post("/business/announcement")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<AnnouncementSaveReq>,
) -> ResResult<AnnouncementSaveRes> {
    let operator = current_user(&claims)?;
    let now = chrono::Utc::now();
    let normalized = dto::normalize_save(&req, now)?;
    let scope_ids = model::resolve_scope_ids(&normalized.scope_type, &normalized.scope_ids)
        .await
        .map_err(|error| utils::db_error(error, "展开公告发布范围"))?;
    if normalized.scope_type != "ALL" && scope_ids.is_empty() {
        // 部门被删 / 不存在时，展开结果会变空 —— 此时发布出去「没人能看到」
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_SCOPE_INVALID,
            "发布范围内的部门 / 角色已全部失效，请重新选择",
        ));
    }
    let rendered = content::render(&normalized.content_md, Recipe::Announcement);
    let summary = resolve_summary(&normalized.summary, &rendered.plain_text);
    let attachments = normalized.attachments.clone().unwrap_or_default();
    ensure_attachments(&attachments).await?;

    let publisher_name = model::user_display_name(&operator)
        .await
        .map_err(|error| utils::db_error(error, "查询发布人姓名"))?;
    let id = uuid::Uuid::now_v7();
    let ann_no = model::build_ann_no(now, &id);
    let new = model::NewAnnouncement {
        id,
        ann_no: &ann_no,
        title: &normalized.title,
        summary: &summary,
        content_md: &normalized.content_md,
        content_html: &rendered.content_html,
        level: &normalized.level,
        scope_type: &normalized.scope_type,
        scope_ids: &scope_ids,
        is_top: normalized.is_top,
        top_expire_at: normalized.top_expire_at,
        expire_at: normalized.expire_at,
        remark: normalized.remark.as_deref(),
        publisher_id: operator,
        publisher_name: &publisher_name,
        attachments: &attachments,
    };
    // 批次 C · C6：业务写 + 变更日志同事务（D11）。传的是 &tx，不是全局连接。
    let snapshot = AnnouncementSnapshot {
        ann_no: &ann_no,
        title: &normalized.title,
        summary: &summary,
        content_md: &normalized.content_md,
        level: &normalized.level,
        scope_type: &normalized.scope_type,
        is_top: normalized.is_top,
        top_expire_at: normalized.top_expire_at,
        status: model::STATUS_DRAFT,
        expire_at: normalized.expire_at,
        remark: normalized.remark.as_deref(),
    }
    .to_json();
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        model::insert_announcement(&tx, &new, operator).await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of(
                "sys_announcement",
                id.to_string(),
                Action::Create,
                "新建公告",
            ),
            DiffInput::create(&snapshot),
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|error| utils::db_error(error, "新建公告"))?;
    tracing::info!(
        announcement_id = %id,
        ann_no = %ann_no,
        operator = %operator,
        scope_type = normalized.scope_type,
        attachments = attachments.len(),
        "公告已建立（草稿）"
    );
    Ok(Res::ok(AnnouncementSaveRes {
        id: id.to_string(),
        ann_no,
        status: model::STATUS_DRAFT.to_string(),
        version: 1,
    }))
}

/// 编辑公告（仅 `DRAFT` / `OFFLINE` / `EXPIRED`）。
///
/// # 返回值
///
/// `{ id, ann_no, status, version }`（版本不在这里 +1，发布时才 +1）。
///
/// # 异常情况
///
/// - 404 + `62002`：公告不存在 / 已软删；
/// - 409 + `62009`：公告已发布（必须先下线，见 §2.8.3 校验表）；
/// - 其余同 [`create`]。
#[nik::permission(
    path = "/business/announcement/{id}",
    method = "PUT",
    permission = "business:announcement:update"
)]
#[nik::put("/business/announcement/{id}")]
pub async fn update(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AnnouncementSaveReq>,
) -> ResResult<AnnouncementSaveRes> {
    let operator = current_user(&claims)?;
    let announcement_id = utils::require_uuid(&id, "id")?;
    let existing = require_manage(&announcement_id).await?;
    model::ensure_editable(&existing.status)?;

    let now = chrono::Utc::now();
    // PUT 是全量更新，但**缺席的可选字段沿用原值**：漏传范围会把定向公告静默改成全员可见，
    // 漏传有效期会让公告永不过期 —— 这两类事故比 500 更难发现，所以在 dto 里统一合并后再校验。
    let merged = dto::merge_with_existing(&req, &existing);
    let normalized = dto::normalize_save(&merged, now)?;
    model::ensure_expire_after_publish(existing.publish_at, normalized.expire_at)?;
    let scope_ids = model::resolve_scope_ids(&normalized.scope_type, &normalized.scope_ids)
        .await
        .map_err(|error| utils::db_error(error, "展开公告发布范围"))?;
    if normalized.scope_type != "ALL" && scope_ids.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_SCOPE_INVALID,
            "发布范围内的部门 / 角色已全部失效，请重新选择",
        ));
    }
    let rendered = content::render(&normalized.content_md, Recipe::Announcement);
    let summary = resolve_summary(&normalized.summary, &rendered.plain_text);
    if let Some(attachments) = normalized.attachments.as_deref() {
        ensure_attachments(attachments).await?;
    }

    // 批次 C · C6：前后快照 + 业务写 + 日志同事务
    let before_snapshot = AnnouncementSnapshot::from_row(&existing).to_json();
    let after_snapshot = AnnouncementSnapshot {
        ann_no: &existing.ann_no,
        title: &normalized.title,
        summary: &summary,
        content_md: &normalized.content_md,
        level: &normalized.level,
        scope_type: &normalized.scope_type,
        is_top: normalized.is_top,
        top_expire_at: normalized.top_expire_at,
        status: &existing.status,
        expire_at: normalized.expire_at,
        remark: normalized.remark.as_deref(),
    }
    .to_json();
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        model::update_announcement(
            &tx,
            &announcement_id,
            &normalized.title,
            &summary,
            &normalized.content_md,
            &rendered.content_html,
            &normalized.level,
            &normalized.scope_type,
            &scope_ids,
            normalized.is_top,
            normalized.top_expire_at,
            normalized.expire_at,
            normalized.remark.as_deref(),
            normalized.attachments.as_deref(),
            operator,
        )
        .await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of(
                "sys_announcement",
                announcement_id.to_string(),
                Action::Update,
                "修改公告",
            ),
            DiffInput::update(&before_snapshot, &after_snapshot),
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|error| utils::db_error(error, "编辑公告"))?;
    tracing::info!(
        announcement_id = %announcement_id,
        operator = %operator,
        status = existing.status,
        "公告已编辑"
    );
    Ok(Res::ok(AnnouncementSaveRes {
        id: announcement_id.to_string(),
        ann_no: existing.ann_no,
        status: existing.status,
        version: existing.version,
    }))
}

/// 删除公告（软删；已发布需先下线）。
///
/// # 异常情况
///
/// - 404 + `62002`：不存在；
/// - 409 + `62009`：已发布（先下线）；
/// - 500：数据库异常。
///
/// # 附件为什么不删
///
/// `sys_announcement_attachment` 没有 `deleted_at` 列（V0080 DDL 不在本卡可改范围）；
/// 主体软删后附件行随可见性判定一起失效，`sys_file` 里的文件本体是共享资产不删
/// （PRD §3.10「删除公告 → 附件关联软删；`sys_file` 里的文件不删」）。
#[nik::permission(
    path = "/business/announcement/{id}",
    method = "DELETE",
    permission = "business:announcement:delete"
)]
#[nik::delete("/business/announcement/{id}")]
pub async fn delete(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let operator = current_user(&claims)?;
    let announcement_id = utils::require_uuid(&id, "id")?;
    let existing = require_manage(&announcement_id).await?;
    model::ensure_deletable(&existing.status)?;
    // 批次 C · C6：软删与日志同事务；只有真的删到行（affected > 0）才留凭证
    let before_snapshot = AnnouncementSnapshot::from_row(&existing).to_json();
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    let affected = db()
        .transaction(|tx| async move {
            let affected = model::soft_delete(&tx, &announcement_id, operator).await?;
            if affected > 0 {
                changelog::record_in(
                    &tx,
                    &ctx.ctx(),
                    ChangeTarget::of(
                        "sys_announcement",
                        announcement_id.to_string(),
                        Action::Delete,
                        "删除公告",
                    ),
                    DiffInput::delete(&before_snapshot),
                )
                .await?;
            }
            Ok(affected)
        })
        .await
        .map_err(|error| utils::db_error(error, "删除公告"))?;
    if affected == 0 {
        return Err(not_found());
    }
    tracing::info!(announcement_id = %announcement_id, operator = %operator, "公告已软删");
    Ok(Res::ok(()))
}

/// 发布公告（状态机 + `version+1` + 扇出 + 补偿兜底）。
///
/// # 流程（架构 §2.2）
///
/// 1. 状态机校验（`DRAFT` / `OFFLINE` / `EXPIRED` → `PUBLISHED`；已是 `PUBLISHED` → `400 + 62009`）；
/// 2. 事务内落 `PUBLISHED` / `version+1` / `publish_at=now()` / `notice_id=NULL`；
/// 3. 事务**外**调 `notice::dispatch()`（幂等键 `announcement:{id}:v{version}`）；
/// 4. 成功回填 `notice_id` + `receiver_count`；失败只记日志（`notice_id` 留空），
///    由 `announcement_dispatch_retry` 每 10 分钟补一次。
///
/// # 返回值
///
/// `{ id, status, version, notice_id, receiver_count, fanout, warning }`。
/// `fanout` 取值：`DISPATCHED` 新发 / `DUPLICATED` 幂等重放 / `FAILED` 失败待补偿 /
/// `SKIPPED` 全局开关关闭。**发布本身永远返回成功**（公告已对外可见），
/// 扇出失败只通过 `fanout` + `warning` 告知前端 —— 这是 A-07 的语义：
/// 「公告 `PUBLISHED` 且 `notice_id IS NULL`」是一个合法的中间态。
///
/// # 异常情况
///
/// - 400 + `62009`：状态不允许发布；
/// - 400 + `62008`：有效期已过（重新发布前必须先改有效期）；
/// - 400 + `62001`：正文为空（历史脏数据）；
/// - 404 + `62002`：不存在。
#[nik::permission(
    path = "/business/announcement/{id}/publish",
    method = "POST",
    permission = "business:announcement:publish"
)]
#[nik::post("/business/announcement/{id}/publish")]
pub async fn publish(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<AnnouncementPublishRes> {
    let operator = current_user(&claims)?;
    let announcement_id = utils::require_uuid(&id, "id")?;
    // 批次 C · C6：发布要留「谁在什么时候把它发出去了」，因此把请求上下文传进去
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    publish_inner(&announcement_id, operator, &ctx).await
}

/// 发布的内部实现（便于将来做「批量发布」复用，逻辑只有一份）。
async fn publish_inner(
    announcement_id: &uuid::Uuid,
    operator: uuid::Uuid,
    req_ctx: &changelog::RequestCtx,
) -> ResResult<AnnouncementPublishRes> {
    let row = require_manage(announcement_id).await?;
    model::ensure_publishable(&row.status)?;
    if row.content_md.trim().is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            "正文为空，不能发布",
        ));
    }
    let now = chrono::Utc::now();
    if let Some(expire_at) = row.expire_at
        && expire_at <= now
    {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_EXPIRE_INVALID,
            "公告已过有效期，请先调整有效期再发布",
        ));
    }

    // 发布时**重算**渲染快照：确保快照与当前 Markdown 一致（编辑后未发布的中间态可能不一致）
    let rendered = content::render(&row.content_md, Recipe::Announcement);
    let summary = resolve_summary(&row.summary, &rendered.plain_text);
    // 批次 C · C6：状态落库与「发布」这条凭证同事务（扇出仍在提交后，见下方注释）
    let before_snapshot = AnnouncementSnapshot::from_row(&row).to_json();
    let mut after_snapshot = before_snapshot.clone();
    after_snapshot["status"] = serde_json::json!(model::STATUS_PUBLISHED);
    // 单独克隆一份给闭包：`summary` 在扇出分支里还要用（发布成功的站内信正文）
    let snapshot_summary = summary.clone();
    let version = db()
        .transaction(|tx| async move {
            let version = model::apply_publish(
                &tx,
                announcement_id,
                &rendered.content_html,
                &snapshot_summary,
                operator,
            )
            .await?;
            changelog::record_in(
                &tx,
                &req_ctx.ctx(),
                ChangeTarget::of(
                    "sys_announcement",
                    announcement_id.to_string(),
                    Action::Publish,
                    "发布公告",
                ),
                DiffInput::update(&before_snapshot, &after_snapshot),
            )
            .await?;
            Ok(version)
        })
        .await
        .map_err(|error| utils::db_error(error, "发布公告（状态落库）"))?;

    let mut response = AnnouncementPublishRes {
        id: announcement_id.to_string(),
        status: model::STATUS_PUBLISHED.to_string(),
        version,
        notice_id: None,
        notice_no: None,
        receiver_count: 0,
        fanout: FANOUT_SKIPPED.to_string(),
        warning: None,
    };

    // ---------- 扇出（提交后调用；失败不回滚发布）----------
    if !model::fanout_enabled().await {
        response.warning = Some(format!(
            "全局扇出开关已关闭（{}），本次只发布、不发送站内信；打开开关后补偿任务会自动补发",
            model::CONFIG_FANOUT_ENABLED
        ));
        tracing::warn!(
            announcement_id = %announcement_id,
            "公告已发布，但全局扇出开关关闭，跳过站内信"
        );
        return Ok(Res::ok(response));
    }

    let pending = model::PendingDispatchRow {
        id: *announcement_id,
        version,
        title: row.title.clone(),
        summary: summary.clone(),
        content_md: row.content_md.clone(),
        level: row.level.clone(),
        scope_type: row.scope_type.clone(),
        scope_ids: row.scope_ids.clone(),
        expire_at: row.expire_at,
        publisher_id: Some(operator),
        publisher_name: row.publisher_name.clone(),
    };
    match maintenance::dispatch_announcement(&pending).await {
        Ok(result) => {
            if let Err(error) =
                model::backfill_notice(announcement_id, &result.notice_id, result.receiver_count)
                    .await
            {
                tracing::warn!(
                    announcement_id = %announcement_id,
                    %error,
                    "扇出成功但回填 notice_id 失败（补偿任务会重放，幂等）"
                );
                response.fanout = FANOUT_FAILED.to_string();
                response.warning = Some(
                    "公告已发布，站内信已发出但回执未能落库；补偿任务会自动补齐（不会重复发送）"
                        .to_string(),
                );
                return Ok(Res::ok(response));
            }
            response.notice_id = Some(result.notice_id.to_string());
            response.notice_no = Some(result.notice_no.clone());
            response.receiver_count = result.receiver_count;
            response.fanout = if result.duplicated {
                FANOUT_DUPLICATED.to_string()
            } else {
                FANOUT_DISPATCHED.to_string()
            };
            tracing::info!(
                announcement_id = %announcement_id,
                version,
                notice_id = %result.notice_id,
                receiver_count = result.receiver_count,
                duplicated = result.duplicated,
                "公告已发布并完成扇出"
            );
        }
        Err(error) => {
            tracing::warn!(
                announcement_id = %announcement_id,
                version,
                error = %error,
                "公告已发布，但站内信扇出失败（等待补偿任务重试）"
            );
            response.fanout = FANOUT_FAILED.to_string();
            response.warning = Some(format!(
                "公告已发布，但站内信扇出失败（{error}）；补偿任务将在 10 分钟内自动重试"
            ));
        }
    }
    Ok(Res::ok(response))
}

/// 扇出方式：新发。
pub const FANOUT_DISPATCHED: &str = "DISPATCHED";
/// 扇出方式：幂等重放（同一版本重复调用）。
pub const FANOUT_DUPLICATED: &str = "DUPLICATED";
/// 扇出方式：失败待补偿。
pub const FANOUT_FAILED: &str = "FAILED";
/// 扇出方式：全局开关关闭而跳过。
pub const FANOUT_SKIPPED: &str = "SKIPPED";

/// 下线公告（`PUBLISHED` → `OFFLINE`）。
///
/// # 参数
///
/// - `req.reason`：下线原因（必填，≤200 字符；写审计日志 —— `sys_announcement`
///   没有下线原因列，V0080 DDL 不在本卡可改范围，原因只能进日志）。
///
/// # 异常情况
///
/// - 400 + `62009`：非已发布状态；
/// - 400 + `10001`：`reason` 为空 / 超长；
/// - 404 + `62002`：不存在。
#[nik::permission(
    path = "/business/announcement/{id}/offline",
    method = "POST",
    permission = "business:announcement:offline"
)]
#[nik::post("/business/announcement/{id}/offline")]
pub async fn offline(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AnnouncementOfflineReq>,
) -> ResResult<AnnouncementOfflineRes> {
    let operator = current_user(&claims)?;
    let announcement_id = utils::require_uuid(&id, "id")?;
    let reason = require_offline_reason(&req.reason)?;
    let row = require_manage(&announcement_id).await?;
    model::ensure_offlinable(&row.status)?;
    // 批次 C · C6：下线与日志同事务。下线原因进 action_label（`sys_announcement`
    // 没有下线原因列，V0080 DDL 不在本卡可改范围 —— 原因只能进日志）。
    let before_snapshot = AnnouncementSnapshot::from_row(&row).to_json();
    let mut after_snapshot = before_snapshot.clone();
    after_snapshot["status"] = serde_json::json!(model::STATUS_OFFLINE);
    let mut action_label = "下线公告".to_string();
    if !reason.is_empty() {
        action_label = format!("下线公告：{reason}");
    }
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        model::apply_offline(&tx, &announcement_id, operator).await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of(
                "sys_announcement",
                announcement_id.to_string(),
                Action::Offline,
                action_label.as_str(),
            ),
            DiffInput::update(&before_snapshot, &after_snapshot),
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|error| utils::db_error(error, "下线公告"))?;
    // 已发出的站内信**不撤回**：它已经进了用户信箱，撤回会让「他点进来 404」。
    // 下线后用户侧详情返回 403 + 62005（公告未发布 / 已下线）。
    tracing::info!(
        announcement_id = %announcement_id,
        operator = %operator,
        reason = %reason,
        "公告已下线"
    );
    Ok(Res::ok(AnnouncementOfflineRes {
        id: announcement_id.to_string(),
        status: model::STATUS_OFFLINE.to_string(),
        version: row.version,
    }))
}

/// 下线原因校验（必填 + 长度）。
///
/// # 异常情况
///
/// 空 / 超 200 字符 → `400 + 10001`。
fn require_offline_reason(raw: &str) -> std::result::Result<String, ApiError> {
    let reason = raw.trim();
    if reason.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "下线原因不能为空（审计要求）",
        ));
    }
    if reason.chars().count() > 200 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "下线原因最多 200 个字符",
        ));
    }
    Ok(reason.to_string())
}

/// 预览（Markdown → 净化 HTML，**不入库**）。
///
/// # 参数
///
/// - `req.content_md`：正文 Markdown（1–128 KiB）。
///
/// # 返回值
///
/// `{ content_html, summary, warnings[] }`：与发布走同一条渲染 + 净化管线，
/// 因此「预览看到的」就是「发布后落库的」（架构 §2.4.2）。
///
/// # 异常情况
///
/// - 400 + `62001`：正文超 128 KiB。
///
/// # 为什么不校验「正文为空」
///
/// 编辑器打开新公告时正文就是空的，预览必须能返回空 HTML（而不是 400），
/// 只给一条 `warnings` 提示 —— 这与「保存」时的强校验（空正文 → 62001）不冲突。
#[nik::permission(
    path = "/business/announcement/preview",
    method = "POST",
    permission = "business:announcement:create"
)]
#[nik::post("/business/announcement/preview")]
pub async fn preview(Json(req): Json<AnnouncementPreviewReq>) -> ResResult<AnnouncementPreviewRes> {
    let markdown = req.content_md.trim();
    if markdown.len() > dto::MAX_CONTENT_BYTES {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            format!(
                "正文过大（{} 字节 > {} KiB 上限），请精简或改用附件",
                markdown.len(),
                dto::MAX_CONTENT_BYTES / 1024
            ),
        ));
    }
    let rendered = content::render(markdown, Recipe::Announcement);
    // 闸 3 的提前演练：发布时落库的也是「再净化一次」的结果，预览与落库必须同形
    let content_html = content::sanitize_html(&rendered.content_html, Recipe::Announcement);
    Ok(Res::ok(AnnouncementPreviewRes {
        content_html,
        summary: auto_summary(&rendered.plain_text),
        warnings: preview_warnings(markdown, &rendered),
    }))
}

/// 预览的提示信息（**纯函数**，把「被净化掉了什么」如实告诉作者）。
///
/// # 参数
///
/// - `markdown`：原文（用于判断是否为空）；
/// - `rendered`：渲染产物（含三个移除计数）。
pub fn preview_warnings(markdown: &str, rendered: &content::Rendered) -> Vec<String> {
    let mut warnings = Vec::new();
    if markdown.trim().is_empty() {
        warnings.push("正文为空：这是一条空公告，保存前请补充内容".to_string());
    }
    if rendered.removed_images > 0 {
        warnings.push(format!(
            "{} 张外链图片已被移除（公告只允许站内存储的图片）",
            rendered.removed_images
        ));
    }
    if rendered.removed_links > 0 {
        warnings.push(format!(
            "{} 个不安全链接已降级为纯文本（只允许 http / https / 站内相对路径）",
            rendered.removed_links
        ));
    }
    if rendered.escaped_html > 0 {
        warnings.push(format!(
            "{} 段原始 HTML 已转义为文本（公告正文不支持内联 HTML）",
            rendered.escaped_html
        ));
    }
    warnings
}

/// 绑定附件（文件本体走 `business/storage`，这里只写关联行）。
///
/// # 参数
///
/// - `req.file_id`：`sys_file.id`（必须已通过 `/api/business/storage/upload?category=announcement` 上传）；
/// - `req.sort_order`：排序号（缺省追加到末尾）。
///
/// # 返回值
///
/// 绑定后的附件（`download_url` 是公告侧的代理下载地址）。
///
/// # 异常情况
///
/// - 404 + `62002`：公告不存在；
/// - 400 + `62003`：`file_id` 不存在 / 已删；
/// - 400 + `62001`：附件超 10 个 / 合计超 50MB / 扩展名为可执行类型；
/// - 400 + `60003`~`60006`：存储策略不允许；
/// - 409 + `62009`：公告已发布（先下线再改附件）。
#[nik::permission(
    path = "/business/announcement/{id}/attachment",
    method = "POST",
    permission = "business:announcement:update"
)]
#[nik::post("/business/announcement/{id}/attachment")]
pub async fn bind_attachment(
    Path(id): Path<String>,
    Json(req): Json<AnnouncementAttachmentBindReq>,
) -> ResResult<AttachmentRes> {
    let announcement_id = utils::require_uuid(&id, "id")?;
    let file_id = utils::require_uuid(&req.file_id, "file_id").map_err(|_| {
        ApiError::new(
            400,
            error_code::ANNOUNCEMENT_ATTACHMENT_NOT_FOUND,
            "file_id 不是合法 UUID",
        )
    })?;
    let row = require_manage(&announcement_id).await?;
    model::ensure_editable(&row.status)?;

    let (count, total_bytes) = model::attachment_totals(&announcement_id)
        .await
        .map_err(|error| utils::db_error(error, "统计公告附件"))?;
    let already = model::find_attachment(&announcement_id, &file_id)
        .await
        .map_err(|error| utils::db_error(error, "查询公告附件"))?;
    // 已绑定的重复调用不该因为「已经 10 个」被拒（幂等语义）
    if already.is_none() && count >= dto::MAX_ATTACHMENTS as i64 {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            format!("最多上传 {} 个附件", dto::MAX_ATTACHMENTS),
        ));
    }

    let file = ensure_attachment_file(&file_id).await?;
    if already.is_none() && total_bytes + file.file_size > dto::MAX_ATTACHMENT_TOTAL_BYTES {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            format!(
                "附件合计大小不能超过 {} MB",
                dto::MAX_ATTACHMENT_TOTAL_BYTES / (1024 * 1024)
            ),
        ));
    }

    let sort_order = req
        .sort_order
        .unwrap_or(i32::try_from(count).unwrap_or(i32::MAX));
    let attachment_id = model::bind_attachment(&announcement_id, &file_id, sort_order)
        .await
        .map_err(|error| utils::db_error(error, "绑定公告附件"))?
        .ok_or_else(attachment_not_found)?;
    let attachment = model::find_attachment(&announcement_id, &attachment_id)
        .await
        .map_err(|error| utils::db_error(error, "查询公告附件"))?
        .ok_or_else(attachment_not_found)?;
    tracing::info!(
        announcement_id = %announcement_id,
        file_id = %file_id,
        sort_order,
        "公告附件已绑定"
    );
    Ok(Res::ok(AttachmentRes::from_row(
        &announcement_id,
        &attachment,
    )))
}

/// 阅读名单 / 阅读统计（谁读了、谁没读）。
///
/// # 参数
///
/// - `query.state`：`ALL`（默认）/ `READ` / `UNREAD`；
/// - `query.keyword`：账号 / 姓名模糊。
///
/// # 返回值
///
/// `{ receiver_count, read_count, read_rate, total, page, page_size, list[] }`。
///
/// # 异常情况
///
/// - 404 + `62002`：公告不存在；
/// - 400 + `10001`：`state` 非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/announcement/{id}/readers",
    method = "GET",
    permission = "business:announcement:list"
)]
#[nik::get("/business/announcement/{id}/readers")]
pub async fn readers(
    Path(id): Path<String>,
    Query(query): Query<ReadersQuery>,
) -> ResResult<ReadersRes> {
    readers_inner(&id, &query).await
}

/// 阅读统计（架构 §2.8.2 的路径名；与 [`readers`] 同源同实现）。
#[nik::permission(
    path = "/business/announcement/{id}/read-stats",
    method = "GET",
    permission = "business:announcement:list"
)]
#[nik::get("/business/announcement/{id}/read-stats")]
pub async fn read_stats(
    Path(id): Path<String>,
    Query(query): Query<ReadersQuery>,
) -> ResResult<ReadersRes> {
    readers_inner(&id, &query).await
}

async fn readers_inner(raw_id: &str, query: &ReadersQuery) -> ResResult<ReadersRes> {
    let announcement_id = utils::require_uuid(raw_id, "id")?;
    let row = require_manage(&announcement_id).await?;
    let state = ReaderState::parse(query.state.as_deref())?;
    let filter = ReaderStateFilter::from(state);
    let keyword = utils::like_pattern(query.keyword.as_deref());
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_readers(
        &announcement_id,
        row.notice_id.as_ref(),
        filter,
        keyword.as_deref(),
        page,
        page_size,
    )
    .await
    .map_err(|error| utils::db_error(error, "查询公告阅读名单"))?;
    let receiver_count = i64::from(row.receiver_count);
    let read_count = i64::from(row.read_count);
    Ok(Res::ok(ReadersRes {
        announcement_id: announcement_id.to_string(),
        receiver_count,
        read_count,
        read_rate: dto::read_rate(read_count, receiver_count),
        total: result.total,
        page: result.page,
        page_size: result.size,
        list: result.list.iter().map(ReaderRes::from_row).collect(),
    }))
}

impl From<ReaderState> for ReaderStateFilter {
    fn from(state: ReaderState) -> Self {
        match state {
            ReaderState::All => ReaderStateFilter::All,
            ReaderState::Read => ReaderStateFilter::Read,
            ReaderState::Unread => ReaderStateFilter::Unread,
        }
    }
}

/// 置顶 / 取消置顶（票面动词 `PUT`）。
///
/// # 异常情况
///
/// - 404 + `62002`：不存在；
/// - 400 + `62007`：置顶缺 `top_expire_at` 或时间已过；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/announcement/{id}/top",
    method = "PUT",
    permission = "business:announcement:update"
)]
#[nik::put("/business/announcement/{id}/top")]
pub async fn set_top(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AnnouncementTopReq>,
) -> ResResult<AnnouncementTopRes> {
    set_top_inner(&claims, &id, req).await
}

/// 置顶 / 取消置顶（架构 §2.8.2 的动词是 `POST`，同一实现）。
#[nik::permission(
    path = "/business/announcement/{id}/top",
    method = "POST",
    permission = "business:announcement:update"
)]
#[nik::post("/business/announcement/{id}/top")]
pub async fn set_top_alias(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<AnnouncementTopReq>,
) -> ResResult<AnnouncementTopRes> {
    set_top_inner(&claims, &id, req).await
}

async fn set_top_inner(
    claims: &TokenClaims,
    raw_id: &str,
    req: AnnouncementTopReq,
) -> ResResult<AnnouncementTopRes> {
    let operator = current_user(claims)?;
    let announcement_id = utils::require_uuid(raw_id, "id")?;
    let now = chrono::Utc::now();
    let normalized = dto::normalize_top(&req, now)?;
    require_manage(&announcement_id).await?;
    model::apply_top(
        &announcement_id,
        normalized.is_top,
        normalized.top_expire_at,
        operator,
    )
    .await
    .map_err(|error| utils::db_error(error, "置顶公告"))?;
    tracing::info!(
        announcement_id = %announcement_id,
        is_top = normalized.is_top,
        top_expire_at = ?normalized.top_expire_at,
        operator = %operator,
        "公告置顶状态已更新"
    );
    Ok(Res::ok(AnnouncementTopRes {
        id: announcement_id.to_string(),
        is_top: normalized.is_top,
        top_expire_at: normalized.top_expire_at,
    }))
}

/// 管理详情（编辑器回填：`content_md` 原文 + 范围明细 + 附件）。
///
/// # 说明
///
/// 架构 §2.8.2 把管理详情写在 `GET /business/announcement/{id}`，但该路径已被 C2 的
/// **用户侧**详情占用（权限码 `business:announcement:read`，且带可见性判定）。
/// matchit 不允许同一路径注册两次，管理侧只能换段名 `/manage`。
///
/// # 异常情况
///
/// - 404 + `62002`：不存在 / 已软删；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/announcement/{id}/manage",
    method = "GET",
    permission = "business:announcement:list"
)]
#[nik::get("/business/announcement/{id}/manage")]
pub async fn manage_detail(Path(id): Path<String>) -> ResResult<AnnouncementManageDetailRes> {
    let announcement_id = utils::require_uuid(&id, "id")?;
    let row = require_manage(&announcement_id).await?;
    let attachments = model::list_attachments(&announcement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询公告附件"))?;
    let html = purified_html(&row.content_html, &row.content_md);
    Ok(Res::ok(AnnouncementManageDetailRes::from_row(
        &row,
        html,
        &attachments,
    )))
}

// ==================== 管理侧公共辅助 ====================

/// 出参 HTML：快照为空则从 Markdown 现渲染，**两种情况都再净化一次**（闸 3）。
///
/// # 参数
///
/// - `snapshot`：库里存的渲染快照；
/// - `content_md`：Markdown 原文。
pub fn purified_html(snapshot: &str, content_md: &str) -> String {
    let html = if snapshot.trim().is_empty() {
        content::render(content_md, Recipe::Announcement).content_html
    } else {
        snapshot.to_string()
    };
    content::sanitize_html(&html, Recipe::Announcement)
}

/// 最终摘要：手工填的优先，否则从纯文本自动截取。
///
/// # 参数
///
/// - `manual`：请求里填的摘要（可为空）；
/// - `plain_text`：渲染产物的纯文本。
pub fn resolve_summary(manual: &str, plain_text: &str) -> String {
    let manual = manual.trim();
    if manual.is_empty() {
        auto_summary(plain_text)
    } else {
        dto::truncate_chars(manual, dto::MAX_SUMMARY_CHARS)
    }
}

/// 自动摘要：折叠空白后截取前 200 个字符（超出补 `…`）。
///
/// # 参数
///
/// - `plain_text`：正文纯文本。
pub fn auto_summary(plain_text: &str) -> String {
    let collapsed = plain_text.split_whitespace().collect::<Vec<_>>().join(" ");
    dto::truncate_chars(&collapsed, 200)
}

/// 加载管理详情行，不存在直接 404。
async fn require_manage(
    announcement_id: &uuid::Uuid,
) -> std::result::Result<model::ManageDetailRow, ApiError> {
    model::find_manage(announcement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询公告（管理端）"))?
        .ok_or_else(not_found)
}

/// 公告不存在 / 已软删 → `404 + 62002`。
fn not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::ANNOUNCEMENT_NOT_FOUND,
        "公告不存在或已删除",
    )
}

/// 附件不存在 → `400 + 62003`（§2.8.3 校验表：附件 file_id 不存在为 400）。
fn attachment_not_found() -> ApiError {
    ApiError::new(
        400,
        error_code::ANNOUNCEMENT_ATTACHMENT_NOT_FOUND,
        "附件文件不存在或已删除，请重新上传",
    )
}

/// 校验一批附件（存在性 + 危险扩展名 + 存储策略 + 数量 / 合计大小）。
///
/// # 异常情况
///
/// 见 [`ensure_attachment_file`] 与数量 / 大小校验的中文错误。
async fn ensure_attachments(
    attachments: &[(uuid::Uuid, i32)],
) -> std::result::Result<(), ApiError> {
    if attachments.len() > dto::MAX_ATTACHMENTS {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            format!("最多上传 {} 个附件", dto::MAX_ATTACHMENTS),
        ));
    }
    let mut total: i64 = 0;
    for (file_id, _) in attachments {
        let file = ensure_attachment_file(file_id).await?;
        total += file.file_size;
    }
    if total > dto::MAX_ATTACHMENT_TOTAL_BYTES {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            format!(
                "附件合计大小不能超过 {} MB",
                dto::MAX_ATTACHMENT_TOTAL_BYTES / (1024 * 1024)
            ),
        ));
    }
    Ok(())
}

/// 校验单个附件：文件存在 → 扩展名不是可执行类型 → 通过存储策略。
///
/// # 为什么分类回退到 `document`
///
/// 架构 §5.3 把 `sys_storage_policy` 的 `announcement` 分类策略放在 **V0086**（C8 交付）；
/// 本卡先按「`announcement` → `document` → 内置默认策略」三级回退，V0086 落地后自动生效，
/// 无需再改代码（回退顺序写在这里，是唯一的一处定义）。
///
/// # 异常情况
///
/// 见各分支的中文错误码。
async fn ensure_attachment_file(
    file_id: &uuid::Uuid,
) -> std::result::Result<storage_model::StorageFileModel, ApiError> {
    let file = storage_model::find_active_file(file_id)
        .await
        .map_err(|error| utils::db_error(error, "查询附件文件记录"))?
        .ok_or_else(attachment_not_found)?;
    let file_name = file
        .original_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&file.file_name)
        .to_string();
    if dto::is_forbidden_attachment(&file_name) {
        return Err(ApiError::new(
            400,
            error_code::ANNOUNCEMENT_CONTENT_INVALID,
            format!("公告不允许挂载可执行文件：{file_name}"),
        ));
    }
    let policy = resolve_upload_policy().await?;
    storage_model::check_upload(
        &policy,
        &file_name,
        u64::try_from(file.file_size).unwrap_or(0),
        file.mime_type.as_deref(),
    )
    .map_err(|violation| {
        ApiError::new(
            violation.http_status(),
            violation.code(),
            violation.message(),
        )
    })?;
    Ok(file)
}

/// 附件分类策略（`announcement` → `document` → 内置默认）。
async fn resolve_upload_policy()
-> std::result::Result<storage_model::SysStoragePolicyModel, ApiError> {
    for category in ["announcement", "document"] {
        let found = storage_model::find_policy_by_category(category)
            .await
            .map_err(|error| utils::db_error(error, "查询附件上传策略"))?;
        if let Some(policy) = found
            && policy.status.as_i16() != 0
        {
            return Ok(policy);
        }
    }
    Ok(storage_model::default_policy("announcement"))
}

#[cfg(test)]
mod manage_tests {
    use super::*;

    fn query() -> ManagePageQuery {
        ManagePageQuery::default()
    }

    /// 正常路径：状态多选去重 + 大写化，其余过滤器原样透传。
    #[test]
    fn manage_filter_parses_multi_status() {
        let mut q = query();
        q.status = Some("draft, PUBLISHED,draft".into());
        q.scope_type = Some("role".into());
        q.level = Some("urgent".into());
        let filter = manage_filter(&q).expect("合法参数");
        assert_eq!(filter.statuses, vec!["DRAFT", "PUBLISHED"]);
        assert_eq!(filter.scope_type.as_deref(), Some("ROLE"));
        assert_eq!(filter.level.as_deref(), Some("URGENT"));
        assert!(filter.keyword.is_none());
    }

    /// 错误分支：非法状态 → 400 + 10001。
    #[test]
    fn manage_filter_rejects_unknown_status() {
        let mut q = query();
        q.status = Some("DELETED".into());
        let error = manage_filter(&q).expect_err("非法状态必须拒绝");
        assert_eq!((error.status, error.code), (400, 10001));
    }

    /// 错误分支：发布窗口起点晚于终点 → 62001。
    #[test]
    fn manage_filter_rejects_reversed_window() {
        let mut q = query();
        q.publish_from = Some("2026-11-02T00:00:00Z".into());
        q.publish_to = Some("2026-11-01T00:00:00Z".into());
        let error = manage_filter(&q).expect_err("倒置时间窗必须拒绝");
        assert_eq!(error.code, 62001);
    }

    /// 摘要：手工优先；自动摘要折叠空白并截断到 200 字符。
    #[test]
    fn summary_resolution() {
        assert_eq!(resolve_summary("  手工摘要 ", "正文"), "手工摘要");
        let long = "字".repeat(300);
        let auto = resolve_summary("", &long);
        assert_eq!(auto.chars().count(), 201, "200 字 + 省略号");
        assert!(auto.ends_with('…'));
        assert_eq!(
            auto_summary("  多   空格\n换行 "),
            "多 空格 换行",
            "空白应折叠"
        );
    }

    /// A-18 XSS 口径（管理侧）：脏快照 / Markdown 内嵌 HTML 都不可执行。
    #[test]
    fn purified_html_blocks_scripts() {
        let dirty = purified_html(
            "<p>看图</p><img src=x onerror=alert(1)><script>alert(2)</script>",
            "x",
        );
        assert!(!dirty.contains("<script"), "{dirty}");
        assert!(!dirty.contains("onerror"), "{dirty}");

        let escaped = purified_html("", "<img src=x onerror=alert(1)>");
        assert!(
            !escaped.contains("<img"),
            "ENABLE_HTML=false 必须转义：{escaped}"
        );
        assert!(escaped.contains("&lt;img"), "{escaped}");
    }

    /// 预览提示：空正文 / 外链图片 / 危险链接 / 内联 HTML 都要如实告知。
    #[test]
    fn preview_warnings_report_sanitizing() {
        let rendered = content::render(
            "![](https://evil.example/x.png)\n\n[x](javascript:alert(1))\n\n<div onclick=\"x\">y</div>\n",
            Recipe::Announcement,
        );
        let warnings = preview_warnings("非空正文", &rendered);
        assert!(
            warnings.iter().any(|w| w.contains("外链图片")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("不安全链接")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("原始 HTML")),
            "{warnings:?}"
        );

        let empty = content::render("", Recipe::Announcement);
        let warnings = preview_warnings("", &empty);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("正文为空"));
    }

    /// 下线原因必填（审计要求）。
    #[test]
    fn offline_reason_required() {
        let error = require_offline_reason("   ").expect_err("空原因必须拒绝");
        assert_eq!((error.status, error.code), (400, 10001));
        assert_eq!(
            require_offline_reason(" 内容有误 ").expect("合法"),
            "内容有误"
        );
        let too_long = "字".repeat(201);
        assert!(require_offline_reason(&too_long).is_err());
    }

    /// 扇出方式常量稳定（前端按它分支）。
    #[test]
    fn fanout_labels_are_stable() {
        assert_eq!(FANOUT_DISPATCHED, "DISPATCHED");
        assert_eq!(FANOUT_DUPLICATED, "DUPLICATED");
        assert_eq!(FANOUT_FAILED, "FAILED");
        assert_eq!(FANOUT_SKIPPED, "SKIPPED");
    }
}

/// 公告审计快照的字段集合（批次 C · C6）。
///
/// # 为什么用一个结构而不是 11 个参数的函数
///
/// 公告有 5 个写操作要记日志（建 / 改 / 删 / 发 / 下线），每处手写一份 `json!`
/// 必然漂移（漏一个字段就是「这个字段改了没人知道」）。结构体 + 两个构造器让
/// 「快照包含哪些字段」只有一处定义，且 clippy 不会因为参数过多报警。
struct AnnouncementSnapshot<'a> {
    ann_no: &'a str,
    title: &'a str,
    summary: &'a str,
    content_md: &'a str,
    level: &'a str,
    scope_type: &'a str,
    is_top: bool,
    top_expire_at: Option<chrono::DateTime<chrono::Utc>>,
    status: &'a str,
    expire_at: Option<chrono::DateTime<chrono::Utc>>,
    remark: Option<&'a str>,
}

impl<'a> AnnouncementSnapshot<'a> {
    /// 从管理详情行构造（「变更前」一侧的主力）。
    fn from_row(row: &'a model::ManageDetailRow) -> Self {
        Self {
            ann_no: &row.ann_no,
            title: &row.title,
            summary: &row.summary,
            content_md: &row.content_md,
            level: &row.level,
            scope_type: &row.scope_type,
            is_top: row.is_top_flag,
            top_expire_at: row.top_expire_at,
            status: &row.status,
            expire_at: row.expire_at,
            remark: Some(row.remark.as_str()),
        }
    }

    /// 序列化为 diff 输入。
    ///
    /// 时间统一转 RFC3339 字符串：`json!` 里混进 `DateTime` 会让「同一时刻的两种写法」
    /// 被判成变化（`+00:00` vs `Z`），而 `content_html` / `receiver_count` / `read_count`
    /// 这些派生与统计列**有意不进快照**（它们每次都变，等于把审计表当访问日志）。
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ann_no": self.ann_no,
            "title": self.title,
            "summary": self.summary,
            "content_md": self.content_md,
            "level": self.level,
            "scope_type": self.scope_type,
            "is_top": self.is_top,
            "top_expire_at": self.top_expire_at.map(|value| value.to_rfc3339()),
            "status": self.status,
            "expire_at": self.expire_at.map(|value| value.to_rfc3339()),
            "remark": self.remark,
        })
    }
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

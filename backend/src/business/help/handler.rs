//! 帮助文档（批次 C · C4）的 HTTP 接口。
//!
//! 依据：票面接口表（`/api/business/help/*`，`/api` 前缀由 `[router].prefix` 注入，
//! 宏里只写 `/business/help/...`）+ 架构 §3.6（契约字段）+ §3.4（权限）。
//!
//! # 接口一览
//!
//! | 方法 | 路径（宏内） | 权限码 | 说明 |
//! |------|--------------|--------|------|
//! | GET | `/business/help/category/tree` | `system:help:read` | 分类树（用户侧剪空分支） |
//! | GET | `/business/help/category/list` | `system:help:category:list` | 分类扁平列表（管理侧，含停用） |
//! | POST | `/business/help/category` | `system:help:category:create` | 新建分类（校验深度 ≤ 3） |
//! | PUT | `/business/help/category/{id}` | `system:help:category:update` | 改名 / 移动（移动也校验深度） |
//! | DELETE | `/business/help/category/{id}` | `system:help:category:delete` | 删除（有文档 409 + 62011） |
//! | GET | `/business/help/doc/page` | `system:help:read` | 文档列表（用户侧只看已发布） |
//! | GET | `/business/help/doc/{id}` | `system:help:read` | 详情（净化 HTML + TOC；`{id}` 也收 slug） |
//! | POST | `/business/help/doc` | `system:help:doc:create` | 新建草稿 |
//! | PUT | `/business/help/doc/{id}` | `system:help:doc:update` | 保存（每次保存 +1 版） |
//! | DELETE | `/business/help/doc/{id}` | `system:help:doc:delete` | 软删 |
//! | POST | `/business/help/doc/{id}/publish` | `system:help:doc:publish` | 发布（+1 版并留快照） |
//! | GET | `/business/help/doc/{id}/versions` | `system:help:doc:list` | 版本列表 |
//! | POST | `/business/help/doc/{id}/versions/{ver}/rollback` | `system:help:doc:publish` | 回滚到某版本（生成新版本） |
//! | GET | `/business/help/search` | `system:help:read` | 检索（标题 + 正文，纯文本片段） |
//! | PUT | `/business/help/doc/{id}/read` | `system:help:read` | 阅读量 +1（按用户/日去重） |
//!
//! # 三条硬约束
//!
//! 1. **`content_html` 只能来自 `shared/content`**：详情出参走闸 3（[`detail_html`]），
//!    写入走 [`model::render_content`]；这里**没有**任何 `format!("<h2>…")`。
//! 2. **检索必须转义**：关键词进 SQL 前一律 `model::like_pattern`（`\` / `%` / `_`），
//!    SQL 侧再 `ESCAPE '\'` 双保险（H-08）。
//! 3. **阅读量去重按「用户 + 自然日」**：Redis 键 `help:read:{doc}:{user}:{yyyymmdd}`，
//!    命中即不计数；缓存不可用时**退化计一次**（宁可多算一个阅读量，也不要因缓存故障
//!    让阅读量彻底不涨）。
//!
//! # 管理侧可见性（不走第二套接口）
//!
//! 用户侧与管理侧共用同一条路径，靠**权限码**分流：持 `system:help:doc:list` 的人
//! 可以看 `DRAFT` / `OFFLINE`、可以按 `status` 过滤；其他人一律强制
//! `published_only`（H-09 的两侧断言）。

use alun::prelude::*;
use axum::Json;

use crate::business::help::dto::{
    CategorySaveReq, CategoryTreeQuery, ChangeNoteReq, DocPageQuery, DocSaveReq,
    HelpCategoryNodeRes, HelpDocCardRes, HelpDocDetailRes, HelpReadRes, HelpSearchHitRes,
    HelpSearchQuery, HelpTocRes, HelpVersionRes,
};
use crate::business::help::model::{self, DocFilter, DocInput, HelpError, SearchFilter};
use crate::business::shared::content::{self, Recipe};
use crate::shared::error_code;
use crate::utils;

/// 管理侧列表权限码（决定能否看见未发布文档）。
const PERM_DOC_LIST: &str = "system:help:doc:list";

/// 管理侧分类权限码（决定分类树是否含停用 / 空分支）。
const PERM_CATEGORY_LIST: &str = "system:help:category:list";

/// 阅读量去重键的存活时间：26 小时（覆盖一整个自然日 + 跨时区余量）。
const READ_DEDUP_TTL_SECS: u64 = 26 * 60 * 60;

// ==================== 分类 ====================

/// 分类树。
///
/// # 参数
///
/// - `claims`：当前用户；
/// - `query`：`include_empty` / `include_disabled`（只有管理侧生效）。
///
/// # 返回值
///
/// `Res<Vec<HelpCategoryNodeRes>>` 树形结构，节点 `doc_count` 含子树合计。
///
/// # 口径
///
/// - 用户侧：只返回 `status = 1` 的分类，并**剪掉整棵都没有已发布文档的分支**
///   （架构 §3.6.1「只返回有已发布文档的分支」）；
/// - 管理侧（持 `system:help:category:list`）：两个开关都生效，用于分类管理页。
#[alun::permission(
    path = "/business/help/category/tree",
    method = "GET",
    permission = "system:help:read"
)]
#[alun::get("/business/help/category/tree")]
pub async fn category_tree(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<CategoryTreeQuery>,
) -> ResResult<Vec<HelpCategoryNodeRes>> {
    let admin = can_manage_category(&claims) || can_manage(&claims);
    let include_empty = admin && query.include_empty.unwrap_or(false);
    let include_disabled = admin && query.include_disabled.unwrap_or(false);

    let rows = model::list_categories(!include_disabled)
        .await
        .map_err(|error| utils::db_error(error, "查询帮助分类"))?;
    let nodes: Vec<HelpCategoryNodeRes> = rows.iter().map(HelpCategoryNodeRes::from_row).collect();
    let tree = utils::build_tree(
        nodes,
        |node| node.id.clone(),
        |node| node.parent_id.clone(),
        |node| node.sort_order,
        |node, children| node.children = children,
    );
    let tree = if include_empty {
        accumulate_counts(tree)
    } else {
        prune_empty(tree)
    };
    Ok(Res::ok(tree))
}

/// 分类扁平列表（管理侧：含停用、含空分类）。
#[alun::permission(
    path = "/business/help/category/list",
    method = "GET",
    permission = "system:help:category:list"
)]
#[alun::get("/business/help/category/list")]
pub async fn category_list() -> ResResult<Vec<HelpCategoryNodeRes>> {
    let rows = model::list_categories(false)
        .await
        .map_err(|error| utils::db_error(error, "查询帮助分类"))?;
    Ok(Res::ok(
        rows.iter().map(HelpCategoryNodeRes::from_row).collect(),
    ))
}

/// 新建分类（H-01：第 4 层 → 422 + 62010）。
#[alun::permission(
    path = "/business/help/category",
    method = "POST",
    permission = "system:help:category:create"
)]
#[alun::post("/business/help/category")]
pub async fn category_create(Json(req): Json<CategorySaveReq>) -> ResResult<HelpCategoryNodeRes> {
    let input = to_category_input(&req)?;
    let row = model::create_category(&input).await.map_err(help_error)?;
    Ok(Res::ok(HelpCategoryNodeRes::from_row(&row)))
}

/// 编辑 / 移动分类（H-01：移动导致超深 → 同样 422 + 62010）。
#[alun::permission(
    path = "/business/help/category/{id}",
    method = "PUT",
    permission = "system:help:category:update"
)]
#[alun::put("/business/help/category/{id}")]
pub async fn category_update(
    Path(id): Path<String>,
    Json(req): Json<CategorySaveReq>,
) -> ResResult<HelpCategoryNodeRes> {
    let category_id = utils::require_uuid(&id, "id")?;
    let input = to_category_input(&req)?;
    let row = model::update_category(&category_id, &input)
        .await
        .map_err(help_error)?;
    Ok(Res::ok(HelpCategoryNodeRes::from_row(&row)))
}

/// 删除分类（H-02：有文档 → 409 + 62011，且分类仍在）。
#[alun::permission(
    path = "/business/help/category/{id}",
    method = "DELETE",
    permission = "system:help:category:delete"
)]
#[alun::delete("/business/help/category/{id}")]
pub async fn category_delete(Path(id): Path<String>) -> ResResult<()> {
    let category_id = utils::require_uuid(&id, "id")?;
    model::delete_category(&category_id)
        .await
        .map_err(help_error)?;
    Ok(Res::ok(()))
}

// ==================== 文档：列表 / 详情 ====================

/// 文档列表（用户侧只看已发布；管理侧可按 `status` 过滤）。
#[alun::permission(
    path = "/business/help/doc/page",
    method = "GET",
    permission = "system:help:read"
)]
#[alun::get("/business/help/doc/page")]
pub async fn doc_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<DocPageQuery>,
) -> ResResult<PageData<Vec<HelpDocCardRes>>> {
    let admin = can_manage(&claims);
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let category_id = match query.category_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(utils::require_uuid(raw, "category_id")?),
        _ => None,
    };
    let status = if admin {
        query
            .status
            .as_deref()
            .map(|value| value.trim().to_uppercase())
    } else {
        None
    };
    let filter = DocFilter {
        category_id,
        include_children: query.include_children.unwrap_or(true),
        keyword: query.keyword_pattern(),
        status,
        published_only: !admin,
    };
    let result = model::page_docs(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询帮助文档"))?;
    let list: Vec<HelpDocCardRes> = result.list.iter().map(HelpDocCardRes::from_row).collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 文档详情（**净化 HTML + TOC**）。`{id}` 既可以是 UUID，也可以是 slug。
///
/// # 返回值
///
/// `content_html` 已过闸 3（出参前再净化）；`toc` 由 `shared/content` 的
/// `Recipe::Help` 生成（heading 带 `id="h-2-1"` 锚点，H-11）。
///
/// # 异常情况
///
/// - 404 + 62015：文档不存在 / 已软删；
/// - 404 + 62015：文档未发布且当前用户无 `system:help:doc:list`（**不泄露存在性**）。
#[alun::permission(
    path = "/business/help/doc/{id}",
    method = "GET",
    permission = "system:help:read"
)]
#[alun::get("/business/help/doc/{id}")]
pub async fn doc_detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<HelpDocDetailRes> {
    let admin = can_manage(&claims);
    let row = load_doc(&id, admin).await?;
    let html = detail_html(&row);
    let toc: Vec<HelpTocRes> = content::render_help(&row.content_md)
        .toc
        .iter()
        .map(HelpTocRes::from_node)
        .collect();
    Ok(Res::ok(HelpDocDetailRes::from_row(&row, html, toc)))
}

// ==================== 文档：写 ====================

/// 新建文档（草稿，`version = 0`，不产生版本快照）。
#[alun::permission(
    path = "/business/help/doc",
    method = "POST",
    permission = "system:help:doc:create"
)]
#[alun::post("/business/help/doc")]
pub async fn doc_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<DocSaveReq>,
) -> ResResult<HelpDocDetailRes> {
    let input = to_doc_input(&req)?;
    let row = model::create_doc(&input, utils::current_user_uuid(&claims))
        .await
        .map_err(help_error)?;
    Ok(Res::ok(detail_res(&row)))
}

/// 保存文档（**每次保存生成版本快照**，`version + 1`）。
///
/// # 状态机
///
/// 编辑一条 `PUBLISHED` 文档会退回 `DRAFT` 并清空 `published_at`
/// （架构 §3.6.2：未复核的内容不该继续对外可见）。
#[alun::permission(
    path = "/business/help/doc/{id}",
    method = "PUT",
    permission = "system:help:doc:update"
)]
#[alun::put("/business/help/doc/{id}")]
pub async fn doc_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<DocSaveReq>,
) -> ResResult<HelpDocDetailRes> {
    let doc = load_doc(&id, true).await?;
    let input = to_doc_input(&req)?;
    let row = model::save_doc(
        &doc.id,
        &input,
        utils::current_user_uuid(&claims),
        Some("保存"),
    )
    .await
    .map_err(help_error)?;
    Ok(Res::ok(detail_res(&row)))
}

/// 软删文档（版本快照随外键 `ON DELETE CASCADE` 保留，软删不触发）。
#[alun::permission(
    path = "/business/help/doc/{id}",
    method = "DELETE",
    permission = "system:help:doc:delete"
)]
#[alun::delete("/business/help/doc/{id}")]
pub async fn doc_delete(Path(id): Path<String>) -> ResResult<()> {
    let doc = load_doc(&id, true).await?;
    model::delete_doc(&doc.id).await.map_err(help_error)?;
    Ok(Res::ok(()))
}

/// 发布文档（H-05：连发 3 次 → 3 条快照，版本 1/2/3）。
#[alun::permission(
    path = "/business/help/doc/{id}/publish",
    method = "POST",
    permission = "system:help:doc:publish"
)]
#[alun::post("/business/help/doc/{id}/publish")]
pub async fn doc_publish(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<ChangeNoteReq>>,
) -> ResResult<HelpDocDetailRes> {
    let doc = load_doc(&id, true).await?;
    let note = body.and_then(|Json(req)| req.change_note);
    let row = model::publish_doc(&doc.id, utils::current_user_uuid(&claims), note.as_deref())
        .await
        .map_err(help_error)?;
    Ok(Res::ok(detail_res(&row)))
}

/// 版本列表（新 → 旧）。
#[alun::permission(
    path = "/business/help/doc/{id}/versions",
    method = "GET",
    permission = "system:help:doc:list"
)]
#[alun::get("/business/help/doc/{id}/versions")]
pub async fn doc_versions(Path(id): Path<String>) -> ResResult<Vec<HelpVersionRes>> {
    let doc = load_doc(&id, true).await?;
    let rows = model::list_versions(&doc.id)
        .await
        .map_err(|error| utils::db_error(error, "查询帮助文档版本"))?;
    Ok(Res::ok(rows.iter().map(HelpVersionRes::from_row).collect()))
}

/// 回滚到指定版本（H-06：内容回到 v1，`version` = 原版本 + 1，历史一行不少）。
#[alun::permission(
    path = "/business/help/doc/{id}/versions/{ver}/rollback",
    method = "POST",
    permission = "system:help:doc:publish"
)]
#[alun::post("/business/help/doc/{id}/versions/{ver}/rollback")]
pub async fn doc_rollback(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, version)): Path<(String, i32)>,
    body: Option<Json<ChangeNoteReq>>,
) -> ResResult<HelpDocDetailRes> {
    let doc = load_doc(&id, true).await?;
    let note = body.and_then(|Json(req)| req.change_note);
    let row = model::rollback_doc(
        &doc.id,
        version,
        utils::current_user_uuid(&claims),
        note.as_deref(),
    )
    .await
    .map_err(help_error)?;
    Ok(Res::ok(detail_res(&row)))
}

// ==================== 检索 ====================

/// 全文检索（标题 + 正文，`ILIKE` + 转义，多词 AND）。
///
/// # 参数
///
/// - `q`：检索串（空格分隔多词，最多 5 个词，AND 连接）；
/// - `status`：管理侧可传 `DRAFT`（H-09 的"管理员命中"一侧）。
///
/// # 返回值
///
/// `PageData<Vec<HelpSearchHitRes>>`：`snippet` 是**纯文本**（无 `<` / `>`），
/// 高亮用 `matched_terms` 交给前端渲染。
///
/// # 空关键词
///
/// `q` 为空 → 返回空页（**不查库**）：`%%` 会全表命中，而"没关键词"的正确语义是
/// "没有结果"。
#[alun::permission(
    path = "/business/help/search",
    method = "GET",
    permission = "system:help:read"
)]
#[alun::get("/business/help/search")]
pub async fn search(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<HelpSearchQuery>,
) -> ResResult<PageData<Vec<HelpSearchHitRes>>> {
    let admin = can_manage(&claims);
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let terms = model::split_terms(query.q.as_deref().unwrap_or_default());
    let category_id = match query.category_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(utils::require_uuid(raw, "category_id")?),
        _ => None,
    };
    let status = if admin {
        query
            .status
            .as_deref()
            .map(|value| value.trim().to_uppercase())
    } else {
        None
    };
    let filter = SearchFilter {
        terms: terms.clone(),
        status,
        published_only: !admin,
        category_id,
    };
    let result = model::search(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "检索帮助文档"))?;
    let list: Vec<HelpSearchHitRes> = result
        .list
        .iter()
        .map(|row| HelpSearchHitRes::from_row(row, &terms))
        .collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

// ==================== 阅读量 ====================

/// 阅读量 +1（**按用户/自然日去重**）。
///
/// # 返回值
///
/// `{ view_count, counted }`：`counted = false` 表示今天已经计过一次（H-13：连点
/// 10 次只 +1）。
///
/// # 去重键
///
/// Redis `help:read:{doc_id}:{user_id}:{yyyyMMdd}`，TTL 26 小时。缓存不可用时
/// 退化计一次（宁可多算，也不要因缓存故障让阅读量彻底不涨）。
///
/// # 异常情况
///
/// - 404 + 62015：文档不存在 / 无权限阅读；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/help/doc/{id}/read",
    method = "PUT",
    permission = "system:help:read"
)]
#[alun::put("/business/help/doc/{id}/read")]
pub async fn doc_read(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<HelpReadRes> {
    let admin = can_manage(&claims);
    let doc = load_doc(&id, admin).await?;
    let user_id = utils::current_user_uuid(&claims).ok_or_else(|| {
        ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识")
    })?;
    let counted = claim_read_once(&doc.id.to_string(), &user_id.to_string()).await;
    if !counted {
        // 去重命中：直接回报当前阅读量，不写库
        return Ok(Res::ok(HelpReadRes {
            view_count: doc.view_count,
            counted: false,
        }));
    }
    let view_count = model::increment_view(&doc.id)
        .await
        .map_err(|error| utils::db_error(error, "累加帮助文档阅读量"))?
        .ok_or_else(|| ApiError::new(404, error_code::HELP_DOC_NOT_FOUND, "文档不存在或已删除"))?;
    Ok(Res::ok(HelpReadRes {
        view_count,
        counted: true,
    }))
}

// ==================== 内部辅助 ====================

/// 详情出参的 HTML（**闸 3**：出参前再净化一次）。
///
/// # 参数
///
/// - `row`：详情行（`content_html` 是写入时的渲染快照）。
///
/// # 返回值
///
/// 1. 快照为空（历史数据 / 写路径异常）→ 用 [`content::render`] 从 `content_md`
///    **现渲染**（`Recipe::Help`）；
/// 2. 否则用快照，但**一律再净化一次** —— 即使有人绕过接口手工改库塞了
///    `<script>`，出参也是干净的（防御纵深，XSS 验收项）。
fn detail_html(row: &model::DocDetailRow) -> String {
    let snapshot = if row.content_html.trim().is_empty() {
        content::render(&row.content_md, Recipe::Help).content_html
    } else {
        row.content_html.clone()
    };
    content::sanitize_html(&snapshot, Recipe::Help)
}

/// 构造详情响应（写接口统一出口：HTML 过闸 3 + TOC 现算）。
fn detail_res(row: &model::DocDetailRow) -> HelpDocDetailRes {
    let html = detail_html(row);
    let toc: Vec<HelpTocRes> = content::render_help(&row.content_md)
        .toc
        .iter()
        .map(HelpTocRes::from_node)
        .collect();
    HelpDocDetailRes::from_row(row, html, toc)
}

/// 载入文档（`{id}` 收 UUID 或 slug；未发布时按 `admin` 决定是否可见）。
///
/// # 参数
///
/// - `raw`：路径参数；
/// - `admin`：当前用户是否持 `system:help:doc:list`。
///
/// # 异常情况
///
/// 统一 404 + 62015：不存在、已软删、或未发布且无管理权限（**不泄露存在性**）。
async fn load_doc(raw: &str, admin: bool) -> std::result::Result<model::DocDetailRow, ApiError> {
    let not_found = || ApiError::new(404, error_code::HELP_DOC_NOT_FOUND, "文档不存在或已删除");
    let row = match utils::parse_uuid(raw) {
        Some(id) => model::find_doc(&id)
            .await
            .map_err(|error| utils::db_error(error, "查询帮助文档"))?,
        None => model::find_doc_by_slug(raw)
            .await
            .map_err(|error| utils::db_error(error, "按 slug 查询帮助文档"))?,
    };
    let row = row.ok_or_else(not_found)?;
    if !admin && row.status != "PUBLISHED" {
        return Err(not_found());
    }
    Ok(row)
}

/// 领域错误 → HTTP 状态 + 业务码（**单一出口**）。
fn help_error(error: HelpError) -> ApiError {
    let message = error.message().to_string();
    match error {
        HelpError::Db(inner) => utils::db_error(inner, "帮助文档"),
        HelpError::DepthExceeded => {
            ApiError::new(422, error_code::HELP_CATEGORY_DEPTH_EXCEEDED, message)
        }
        HelpError::CategoryNotEmpty => {
            ApiError::new(409, error_code::HELP_CATEGORY_NOT_EMPTY, message)
        }
        HelpError::CategoryHasChildren => {
            ApiError::new(409, error_code::RESOURCE_CONFLICT, message)
        }
        HelpError::CategoryNotFound => {
            ApiError::new(404, error_code::HELP_CATEGORY_NOT_FOUND, message)
        }
        HelpError::CategoryCodeConflict => {
            ApiError::new(409, error_code::RESOURCE_CONFLICT, message)
        }
        HelpError::CategoryMoveCycle => ApiError::new(400, error_code::PARAM_INVALID, message),
        HelpError::DocNotFound => ApiError::new(404, error_code::HELP_DOC_NOT_FOUND, message),
        HelpError::DocSlugConflict => {
            ApiError::new(409, error_code::HELP_DOC_SLUG_CONFLICT, message)
        }
        HelpError::DocStatusConflict => {
            ApiError::new(409, error_code::HELP_DOC_STATUS_CONFLICT, message)
        }
        HelpError::DocContentInvalid => {
            ApiError::new(400, error_code::HELP_DOC_CONTENT_INVALID, message)
        }
        HelpError::VersionNotFound => {
            ApiError::new(404, error_code::HELP_DOC_VERSION_NOT_FOUND, message)
        }
    }
}

/// 当前用户是否具备"看见未发布内容"的能力（持任一文档管理权限即可）。
///
/// # 为什么是"任一"而不是只要 `doc:list`
///
/// 编辑一条草稿需要 `system:help:doc:update`，但**不需要** `doc:list`；若只认
/// `doc:list`，一个只有编辑权的运营点开文档会拿到 404（自己的草稿"不见了"）。
/// 框架的 [`TokenClaims::has_any_permission`] 已经包含 `*` / `*:*:*` 通配短路，
/// 与路由层 `#[alun::permission]` 的判定口径一致。
fn can_manage(claims: &TokenClaims) -> bool {
    claims.has_any_permission(&[
        PERM_DOC_LIST,
        "system:help:doc:create",
        "system:help:doc:update",
        "system:help:doc:delete",
        "system:help:doc:publish",
    ])
}

/// 当前用户是否具备分类管理能力（决定分类树能否看到停用 / 空分支）。
fn can_manage_category(claims: &TokenClaims) -> bool {
    claims.has_any_permission(&[
        PERM_CATEGORY_LIST,
        "system:help:category:create",
        "system:help:category:update",
        "system:help:category:delete",
    ])
}

/// 分类请求 → 领域输入。
fn to_category_input(req: &CategorySaveReq) -> std::result::Result<model::CategoryInput, ApiError> {
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "分类名称不能为空且不超过 100 字",
        ));
    }
    let code = req.code.trim();
    if !model::is_valid_category_code(code) {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "分类编码格式非法（^[a-z][a-z0-9_-]{1,63}$）",
        ));
    }
    let parent_id = match req.parent_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(utils::require_uuid(raw, "parent_id")?),
        _ => None,
    };
    let status = req.status.unwrap_or(1);
    if status != 0 && status != 1 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "status 只能是 0（停用）或 1（启用）",
        ));
    }
    Ok(model::CategoryInput {
        parent_id,
        name: name.to_string(),
        code: code.to_string(),
        icon: req
            .icon
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        description: req
            .description
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        sort_order: req.sort_order.unwrap_or(0),
        status,
    })
}

/// 文档请求 → 领域输入。
fn to_doc_input(req: &DocSaveReq) -> std::result::Result<DocInput, ApiError> {
    let category_id = utils::require_uuid(req.category_id.trim(), "category_id")?;
    Ok(DocInput {
        category_id,
        title: req.title.trim().to_string(),
        slug: req.slug.trim().to_string(),
        summary: req
            .summary
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        content_md: req.content_md.clone(),
        keywords: req
            .keywords
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        sort_order: req.sort_order.unwrap_or(0),
    })
}

/// 本自然日首次阅读返回 `true`（并占位），重复阅读返回 `false`。
///
/// # 参数
///
/// - `doc_id`：文档 ID 字符串；
/// - `user_id`：用户 ID 字符串。
///
/// # 返回值
///
/// `true` = 需要计数；`false` = 今天已计过。
///
/// # 缓存不可用时
///
/// 退化返回 `true`（计一次）。宁可多一个阅读量，也不要因为 Redis 抖动让阅读量
/// 彻底不涨——阅读量是展示型指标，不是账务数据。
async fn claim_read_once(doc_id: &str, user_id: &str) -> bool {
    let Some(store) = try_cache() else {
        return true;
    };
    let key = read_dedup_key(doc_id, user_id);
    match Cache::exists(store, &key).await {
        Ok(true) => false,
        Ok(false) => {
            if let Err(error) = Cache::set_ex(store, &key, &true, READ_DEDUP_TTL_SECS).await {
                tracing::warn!(key = %key, error = %error, "写入阅读量去重键失败（本轮按首次计）");
            }
            true
        }
        Err(error) => {
            tracing::warn!(key = %key, error = %error, "读取阅读量去重键失败（本轮按首次计）");
            true
        }
    }
}

/// 去重键：`help:read:{doc}:{user}:{yyyyMMdd}`（UTC 自然日）。
///
/// # 为什么带日期而不是纯 TTL
///
/// 纯 TTL 的语义是"距今 N 小时内只算一次"，跨零点后仍会被旧键挡住；带日期才是
/// 票面要求的「同一天只 +1」。
fn read_dedup_key(doc_id: &str, user_id: &str) -> String {
    let day = chrono::Utc::now().format("%Y%m%d");
    format!("help:read:{doc_id}:{user_id}:{day}")
}

/// 剪掉"整棵子树都没有已发布文档"的分支（用户侧分类树）。
fn prune_empty(nodes: Vec<HelpCategoryNodeRes>) -> Vec<HelpCategoryNodeRes> {
    let mut kept = Vec::new();
    for mut node in nodes {
        let total = node.prune_and_sum();
        if total > 0 {
            kept.push(node);
        }
    }
    kept
}

/// 保留全部分支，但把父节点角标改成子树合计（管理侧分类树）。
fn accumulate_counts(mut nodes: Vec<HelpCategoryNodeRes>) -> Vec<HelpCategoryNodeRes> {
    for node in &mut nodes {
        node.accumulate();
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一份带指定权限的令牌声明（角色与时间字段无需真实值）。
    fn claims_with(permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "00000000-0000-0000-0000-000000000001".into(),
            username: Some("tester".into()),
            roles: Vec::new(),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    fn doc_row(content_md: &str, content_html: &str) -> model::DocDetailRow {
        model::DocDetailRow {
            id: uuid::Uuid::now_v7(),
            title: "安装手册".into(),
            slug: "install-guide".into(),
            summary: String::new(),
            category_id: uuid::Uuid::now_v7(),
            category_name: "入门".into(),
            content_md: content_md.into(),
            content_html: content_html.into(),
            keywords: String::new(),
            version: 3,
            status: "PUBLISHED".into(),
            sort_order: 0,
            view_count: 7,
            published_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// 详情 HTML 必须保留 heading 锚点（H-11：`add_generic_attributes(&["id"])` 生效）。
    ///
    /// 层级偏移：`shared/content` 把正文标题统一 +1（页面已有 `<h1>`），所以 `##`
    /// 是 `<h3 id="h-3-1">`（架构 §3.2 举例写的是 `h-2-1`，那是未偏移的旧口径）。
    /// 真正的不变量是 **TOC 里的 id 与 HTML 里的 id 一致**——见
    /// `tests/batchC_c4_help.rs` 的 `toc_anchors_exist_in_html`。
    #[test]
    fn detail_html_keeps_toc_anchor() {
        let row = doc_row("## 维护时间\n\n### 步骤\n", "");
        let html = detail_html(&row);
        assert!(html.contains("<h3 id=\"h-3-1\">"), "{html}");
        assert!(html.contains("<h4 id=\"h-4-1\">"), "{html}");
    }

    /// XSS（脏快照路径）：库里被手工塞了 `<script>` → 出参被净化。
    #[test]
    fn detail_html_sanitizes_dirty_snapshot() {
        let row = doc_row(
            "正文",
            "<p>看图</p><script>alert(1)</script><img src=x onerror=alert(2)>",
        );
        let html = detail_html(&row);
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("onerror"), "{html}");
        assert!(html.contains("<p>看图</p>"), "净化不是全删：{html}");
    }

    /// XSS（Markdown 内嵌 HTML 路径）：`ENABLE_HTML = false` → 降级为转义文本。
    #[test]
    fn detail_html_escapes_raw_html_in_markdown() {
        let row = doc_row("<script>alert(1)</script>", "");
        let html = detail_html(&row);
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("&lt;script"), "应降级为转义文本：{html}");
    }

    /// 未发布文档对普通用户不可见（H-09 的详情一侧）：判定条件是"无管理权 + 非 PUBLISHED"。
    #[test]
    fn draft_is_hidden_without_manage_permission() {
        let claims = claims_with(&["system:help:read"]);
        assert!(!can_manage(&claims));
        // 有读权限但没有管理权限 → 草稿不可见（load_doc 的分支条件）
        assert!(!can_manage(&claims) && "DRAFT" != "PUBLISHED");
    }

    /// 去重键必须带自然日（否则跨零点仍被旧键挡住）。
    #[test]
    fn read_dedup_key_contains_day() {
        let key = read_dedup_key("doc-1", "user-1");
        let today = chrono::Utc::now().format("%Y%m%d").to_string();
        assert_eq!(key, format!("help:read:doc-1:user-1:{today}"));
        assert_eq!(key.matches(':').count(), 4);
    }

    /// 领域错误 → 状态码映射（H-01 / H-02 / H-03 的码各就各位）。
    #[test]
    fn help_errors_map_to_status_and_code() {
        let depth = help_error(HelpError::DepthExceeded);
        assert_eq!((depth.status, depth.code), (422, 62010));

        let not_empty = help_error(HelpError::CategoryNotEmpty);
        assert_eq!((not_empty.status, not_empty.code), (409, 62011));

        let slug = help_error(HelpError::DocSlugConflict);
        assert_eq!((slug.status, slug.code), (409, 62016));

        let invalid = help_error(HelpError::DocContentInvalid);
        assert_eq!((invalid.status, invalid.code), (400, 62018));

        let version = help_error(HelpError::VersionNotFound);
        assert_eq!((version.status, version.code), (404, 62019));
    }

    /// 分类输入校验（编码正则与 DB 一致）。
    #[test]
    fn category_input_validation() {
        let ok = CategorySaveReq {
            name: "入门指南".into(),
            code: "getting_started".into(),
            ..Default::default()
        };
        assert!(to_category_input(&ok).is_ok());

        let bad_code = CategorySaveReq {
            name: "入门指南".into(),
            code: "Getting-Started".into(),
            ..Default::default()
        };
        assert!(to_category_input(&bad_code).is_err());

        let blank_name = CategorySaveReq {
            name: "   ".into(),
            code: "ok_code".into(),
            ..Default::default()
        };
        assert!(to_category_input(&blank_name).is_err());
    }

    /// 超级管理员（`*` 通配）短路权限判断（种子若漏授也仍然可用）。
    ///
    /// 框架口径（`alun_web::TokenClaims::is_super_admin`）是**通配权限**而不是
    /// 角色码：`permissions` 里含 `*` 或 `*:*:*` 即视为超管，与路由层
    /// `#[alun::permission]` 的判定完全一致（这也是不自己写角色判断的原因）。
    #[test]
    fn super_admin_shortcuts_permission() {
        let claims = claims_with(&["*"]);
        assert!(can_manage(&claims));
        assert!(can_manage_category(&claims));
    }

    /// 权限分流：只有编辑权的运营也能看到草稿（否则点开自己的草稿是 404）。
    #[test]
    fn editor_can_manage_even_without_list_permission() {
        let editor = claims_with(&["system:help:doc:update"]);
        assert!(can_manage(&editor));
        assert!(!can_manage_category(&editor));
    }
}

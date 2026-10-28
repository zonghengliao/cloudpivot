//! 消息通知接口（批次 A · A5）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入，宏内只写 `/business/notice/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/notice` | `business:notice:send` | 发送通知（管理端手工 / 批量） |
//! | GET | `/notice/page` | `business:notice:list` | 通知主体分页（管理视角，含送达 / 已读统计） |
//! | GET | `/notice/{id}` | `business:notice:list` | 通知详情（含接收与投递统计） |
//! | DELETE | `/notice/{id}` | `business:notice:delete` | 删除通知（软删，接收行同步不可见） |
//! | GET | `/notice/mine` | `business:notice:mine` | 我的通知分页（`?read_flag=`） |
//! | GET | `/notice/unread-count` | `business:notice:mine` | 未读数（高频，命中部分索引） |
//! | PUT | `/notice/read/{receiver_id}` | `business:notice:mine` | 单条已读 |
//! | PUT | `/notice/read-all` | `business:notice:mine` | 全部已读 |
//! | DELETE | `/notice/mine/{receiver_id}` | `business:notice:mine` | 删除我的这条 |
//! | GET | `/notice/template/page` | `business:notice:template:list` | 模板分页 |
//! | POST | `/notice/template` | `business:notice:template:create` | 新建模板（保存前校验 + 试渲染） |
//! | PUT | `/notice/template/{id}` | `business:notice:template:update` | 修改模板 |
//! | DELETE | `/notice/template/{id}` | `business:notice:template:delete` | 删除模板（被通知引用时 409） |
//! | POST | `/notice/template/{id}/preview` | `business:notice:template:list` | 试渲染 |
//! | GET | `/notice/channel/config` | `business:notice:channel:config` | 通道配置（邮件 / Webhook 掩码） |
//! | GET | `/notice/stream` | `business:notice:mine` | **SSE 实时订阅（A6）**：per-user 频道，断连即退订 |
//! | GET | `/notice/stream/stats` | `business:notice:list` | **SSE 频道表统计（A6）**：排障 / 验收取证 |
//!
//! > SSE 按架构文档 §4.5 实现：走普通 HTTP + 现有 `AuthLayer`（`Authorization` 头），
//! > 每用户一个独立频道（A 用户物理上收不到 B 的通知）。见 [`crate::business::notice::sse_hub`]。
//!
//! # 权限
//!
//! 「我的通知 / 未读数 / 已读 / 删除我的」也声明权限码（底座实测没有「登录即可访问」
//! 的豁免口子），`V0045` 已把 `business:notice:mine` 授予**所有角色**。
//!
//! # 文件顺序约定
//!
//! 路由方法在前，公共辅助函数在后（与底座其它 handler 一致）。

use alun::prelude::*;
use axum::Json;
use axum::response::{IntoResponse, Response};

use crate::business::notice::dto::{
    ChannelConfigRes, ContentFormat, DeliverySummaryRes, MyNoticeQuery, MyNoticeRes,
    NoticeDetailRes, NoticeFilter, NoticeLevel, NoticePageQuery, NoticeRes, NoticeSendReq,
    NoticeType, StreamStatsRes, TemplateFilter, TemplatePageQuery, TemplatePreviewReq,
    TemplatePreviewRes, TemplateRes, TemplateSaveReq, TemplateVarDecl, UnreadCountRes,
    parse_var_decls,
};
use crate::business::notice::model::{self, TemplateFields};
use crate::business::notice::render;
use crate::business::notice::sse_hub;
use crate::business::notice::{DispatchResult, NoticeDraft, NoticeTarget, dispatch};
use crate::shared::constants::EnableStatus;
use crate::shared::error_code;
use crate::utils;

// ==================== 发送与查询 ====================

/// 发送通知（管理端手工 / 业务模块转发）。
///
/// # 定时发送（V0051）
///
/// `scheduled_at` 给了**未来时间** → 只建一条 `dispatch_status='PENDING'` 的通知，
/// 不写接收行、不投递、不推送：用户侧「我的通知 / 未读数」到点前看不到它；
/// 到点后由 [`crate::business::notice::scheduled`] 巡逻任务（每 30 秒）扇出并投递。
/// `scheduled_at` 为空或已过 → 立即发送（正常链路）。
///
/// # 参数
///
/// - `claims`：当前登录用户（写入 `sender_id` / `sender_name`）。
/// - `req`：发送请求（目标 / 模板 / 通道 / 幂等键 / 计划发送时间）。
///
/// # 返回值
///
/// 发送结果（`notice_id`、接收人数、各通道状态、是否幂等重放）。
///
/// # 异常情况
///
/// - 400：目标为空 / 类型非法 / 通道越界 / 缺标题正文 / 计划时间超过 366 天；
/// - 404：模板不存在或停用；
/// - 422：渲染失败（缺变量 / 语法 / 超长）、接收人超上限；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/notice",
    method = "POST",
    permission = "business:notice:send"
)]
#[alun::post("/business/notice")]
pub async fn send(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<NoticeSendReq>,
) -> ResResult<DispatchResult> {
    let sender_id = utils::current_user_uuid(&claims);
    let target = NoticeTarget::parse(&req.target_type, req.target_ids.as_deref().unwrap_or(&[]))?;
    let notice_type = match utils::trimmed(req.notice_type.as_deref()) {
        Some(raw) => NoticeType::parse(raw)?,
        None => NoticeType::System,
    };
    let level = match utils::trimmed(req.level.as_deref()) {
        Some(raw) => NoticeLevel::parse(raw)?,
        None => NoticeLevel::Info,
    };
    let content_format = match utils::trimmed(req.content_format.as_deref()) {
        Some(raw) => ContentFormat::parse(raw)?,
        None => ContentFormat::Text,
    };

    let draft = NoticeDraft {
        notice_type,
        level,
        title: req.title.unwrap_or_default(),
        content: req.content.unwrap_or_default(),
        content_format,
        link_url: utils::trimmed(req.link_url.as_deref()).map(str::to_string),
        biz_type: utils::trimmed(req.biz_type.as_deref()).map(str::to_string),
        biz_id: utils::trimmed(req.biz_id.as_deref()).map(str::to_string),
        template_code: utils::trimmed(req.template_code.as_deref()).map(str::to_string),
        template_vars: req.template_vars.unwrap_or(serde_json::Value::Null),
        target,
        channel_mask: req.channel_mask.unwrap_or(1),
        idempotency_key: utils::trimmed(req.idempotency_key.as_deref()).map(str::to_string),
        expire_at: req.expire_at,
        // 定时发送（V0051）：未来时间 → 建 PENDING 任务，到点由巡逻任务投递
        scheduled_at: req.scheduled_at,
        sender_id,
        sender_name: claims.username.clone(),
    };

    let result = dispatch(draft).await.map_err(|error| error.into_api())?;
    Ok(Res::ok(result))
}

/// 通知主体分页（管理视角）。
///
/// # 参数
///
/// - `query`：关键词 / 类型 / 级别 / 业务类型 + 分页。
///
/// # 返回值
///
/// 分页数据（含 `receiver_count` / `read_count` / `read_rate`）。
///
/// # 异常情况
///
/// - 400：类型 / 级别取值非法；
/// - 403：无 `business:notice:list`；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/notice/page",
    method = "GET",
    permission = "business:notice:list"
)]
#[alun::get("/business/notice/page")]
pub async fn page(Query(query): Query<NoticePageQuery>) -> ResResult<PageData<Vec<NoticeRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = NoticeFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        notice_type: match utils::trimmed(query.notice_type.as_deref()) {
            Some(raw) => Some(NoticeType::parse(raw)?.as_str().to_string()),
            None => None,
        },
        level: match utils::trimmed(query.level.as_deref()) {
            Some(raw) => Some(NoticeLevel::parse(raw)?.as_str().to_string()),
            None => None,
        },
        biz_type: utils::trimmed(query.biz_type.as_deref()).map(str::to_string),
    };

    let result = model::page_notices(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询通知"))?;
    let list = result.list.iter().map(NoticeRes::from_model).collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 通知详情（含接收人实时统计与投递记录汇总）。
///
/// # 参数
///
/// - `id`：通知主键。
///
/// # 返回值
///
/// 通知 + 已读 / 未读 / 已删除人数 + 投递汇总。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：通知不存在或已删除。
#[alun::permission(
    path = "/business/notice/{id}",
    method = "GET",
    permission = "business:notice:list"
)]
#[alun::get("/business/notice/{id}")]
pub async fn detail(Path(id): Path<String>) -> ResResult<NoticeDetailRes> {
    let id = utils::require_uuid(&id, "id")?;
    let notice = model::find_notice(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询通知详情"))?
        .ok_or_else(|| ApiError::new(404, error_code::NOTICE_NOT_FOUND, "通知不存在或已删除"))?;

    let stats = model::receiver_stats(&id)
        .await
        .map_err(|error| utils::db_error(error, "统计通知接收人"))?;
    let deliveries = model::delivery_summary(&id)
        .await
        .map_err(|error| utils::db_error(error, "统计通知投递"))?;

    Ok(Res::ok(NoticeDetailRes {
        notice: NoticeRes::from_model(&notice),
        read_count: stats.read_count,
        unread_count: stats.total - stats.read_count,
        deleted_count: stats.deleted_count,
        deliveries: deliveries
            .iter()
            .map(DeliverySummaryRes::from_row)
            .collect(),
    }))
}

/// 删除通知（软删；用户侧列表随即不可见）。
///
/// # 参数
///
/// - `id`：通知主键。
///
/// # 异常情况
///
/// - 400：ID 非法；
/// - 404：通知不存在或已删除；
/// - 403：无 `business:notice:delete`。
#[alun::permission(
    path = "/business/notice/{id}",
    method = "DELETE",
    permission = "business:notice:delete"
)]
#[alun::delete("/business/notice/{id}")]
pub async fn delete(Path(id): Path<String>) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let affected = model::soft_delete_notice(&id)
        .await
        .map_err(|error| utils::db_error(error, "删除通知"))?;
    if affected == 0 {
        return Err(ApiError::new(
            404,
            error_code::NOTICE_NOT_FOUND,
            "通知不存在或已删除",
        ));
    }
    Ok(Res::ok(()))
}

// ==================== 我的通知（人人可用） ====================

/// 我的通知分页。
///
/// # 参数
///
/// - `claims`：当前用户（只查自己的接收行）。
/// - `query`：`read_flag` 筛选 + 分页。
///
/// # 返回值
///
/// 分页数据，按送达时间倒序（命中 `idx_sys_notice_receiver_list`）。
///
/// # 异常情况
///
/// - 403：无 `business:notice:mine`（`V0045` 已授予所有角色）；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/notice/mine",
    method = "GET",
    permission = "business:notice:mine"
)]
#[alun::get("/business/notice/mine")]
pub async fn mine(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<MyNoticeQuery>,
) -> ResResult<PageData<Vec<MyNoticeRes>>> {
    let user_id = current_user(&claims)?;
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = model::page_my_notices(&user_id, query.read_flag, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询我的通知"))?;
    let list = result.list.iter().map(MyNoticeRes::from_row).collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 未读数（红点，高频接口）。
///
/// # 返回值
///
/// `{ "unread_count": n }`；查询固定写 `user_id = ? AND read_flag = FALSE AND is_deleted = FALSE`，
/// 命中部分索引 `idx_sys_notice_receiver_unread`（0.5 ms 级）。
#[alun::permission(
    path = "/business/notice/unread-count",
    method = "GET",
    permission = "business:notice:mine"
)]
#[alun::get("/business/notice/unread-count")]
pub async fn unread_count(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<UnreadCountRes> {
    let user_id = current_user(&claims)?;
    let unread_count = model::count_unread(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "统计未读通知"))?;
    Ok(Res::ok(UnreadCountRes { unread_count }))
}

/// 单条已读。
///
/// # 参数
///
/// - `receiver_id`：接收行主键（来自「我的通知」列表的 `receiver_id`）。
///
/// # 异常情况
///
/// - 404：接收行不存在 / 不属于本人 / 已删除。
#[alun::permission(
    path = "/business/notice/read/{receiver_id}",
    method = "PUT",
    permission = "business:notice:mine"
)]
#[alun::put("/business/notice/read/{receiver_id}")]
pub async fn mark_read(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(receiver_id): Path<String>,
) -> ResResult<()> {
    let user_id = current_user(&claims)?;
    let receiver_id = utils::require_uuid(&receiver_id, "receiver_id")?;
    let changed = model::mark_read(&receiver_id, &user_id)
        .await
        .map_err(|error| utils::db_error(error, "标记通知已读"))?;
    if !changed {
        return Err(ApiError::new(
            404,
            error_code::NOTICE_RECEIVER_NOT_FOUND,
            "通知不存在、已读或不属于当前用户",
        ));
    }
    Ok(Res::ok(()))
}

/// 全部已读。
///
/// # 返回值
///
/// `{ "affected": n }`（本次置为已读的条数）。
#[alun::permission(
    path = "/business/notice/read-all",
    method = "PUT",
    permission = "business:notice:mine"
)]
#[alun::put("/business/notice/read-all")]
pub async fn mark_all_read(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<serde_json::Value> {
    let user_id = current_user(&claims)?;
    let affected = model::mark_all_read(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "全部标记已读"))?;
    Ok(Res::ok(serde_json::json!({ "affected": affected })))
}

/// 删除「我的这条」（用户侧软删，不影响其他接收人）。
///
/// # 异常情况
///
/// - 404：接收行不存在 / 不属于本人 / 已删除。
#[alun::permission(
    path = "/business/notice/mine/{receiver_id}",
    method = "DELETE",
    permission = "business:notice:mine"
)]
#[alun::delete("/business/notice/mine/{receiver_id}")]
pub async fn delete_mine(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(receiver_id): Path<String>,
) -> ResResult<()> {
    let user_id = current_user(&claims)?;
    let receiver_id = utils::require_uuid(&receiver_id, "receiver_id")?;
    let affected = model::delete_my_receiver(&receiver_id, &user_id)
        .await
        .map_err(|error| utils::db_error(error, "删除我的通知"))?;
    if affected == 0 {
        return Err(ApiError::new(
            404,
            error_code::NOTICE_RECEIVER_NOT_FOUND,
            "通知不存在或已删除",
        ));
    }
    Ok(Res::ok(()))
}

// ==================== 模板 ====================

/// 模板分页。
#[alun::permission(
    path = "/business/notice/template/page",
    method = "GET",
    permission = "business:notice:template:list"
)]
#[alun::get("/business/notice/template/page")]
pub async fn template_page(
    Query(query): Query<TemplatePageQuery>,
) -> ResResult<PageData<Vec<TemplateRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let status = query.status.map(EnableStatus::from_i16).transpose()?;
    let filter = TemplateFilter {
        keyword: utils::trimmed(query.keyword.as_deref()).map(str::to_string),
        notice_type: match utils::trimmed(query.notice_type.as_deref()) {
            Some(raw) => Some(NoticeType::parse(raw)?.as_str().to_string()),
            None => None,
        },
        status,
    };
    let result = model::page_templates(&filter, page, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询通知模板"))?;
    let list = result.list.iter().map(TemplateRes::from_model).collect();
    Ok(Res::page(list, result.total, result.page, result.size))
}

/// 新建模板（保存前做语法校验 + 占位符声明校验 + 示例值试渲染）。
///
/// # 异常情况
///
/// - 400：编码格式非法；
/// - 409：编码重复；
/// - 422：模板语法错误 / 占位符未声明 / 试渲染失败。
#[alun::permission(
    path = "/business/notice/template",
    method = "POST",
    permission = "business:notice:template:create"
)]
#[alun::post("/business/notice/template")]
pub async fn template_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<TemplateSaveReq>,
) -> ResResult<TemplateRes> {
    let operator = utils::current_user_uuid(&claims);
    let fields = build_template_fields(&req, operator)?;

    let existed = model::count_template_code(fields.template_code, None)
        .await
        .map_err(|error| utils::db_error(error, "校验模板编码唯一性"))?;
    utils::ensure_unique(
        existed,
        error_code::NOTICE_TEMPLATE_DUPLICATE,
        "模板编码已存在",
    )?;

    let id = uuid::Uuid::now_v7();
    model::insert_template(&id, &fields)
        .await
        .map_err(|error| utils::db_error(error, "新建通知模板"))?;
    let created = model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "读取新建的通知模板"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::NOTICE_TEMPLATE_NOT_FOUND,
                "模板创建后未读到记录",
            )
        })?;
    Ok(Res::ok(TemplateRes::from_model(&created)))
}

/// 修改模板（同样先校验再落库）。
///
/// # 异常情况
///
/// - 404：模板不存在；
/// - 409：编码与其它模板重复。
#[alun::permission(
    path = "/business/notice/template/{id}",
    method = "PUT",
    permission = "business:notice:template:update"
)]
#[alun::put("/business/notice/template/{id}")]
pub async fn template_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<TemplateSaveReq>,
) -> ResResult<TemplateRes> {
    let id = utils::require_uuid(&id, "id")?;
    let operator = utils::current_user_uuid(&claims);
    model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询待修改的通知模板"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::NOTICE_TEMPLATE_NOT_FOUND,
                "通知模板不存在或已删除",
            )
        })?;

    let fields = build_template_fields(&req, operator)?;
    let existed = model::count_template_code(fields.template_code, Some(&id))
        .await
        .map_err(|error| utils::db_error(error, "校验模板编码唯一性"))?;
    utils::ensure_unique(
        existed,
        error_code::NOTICE_TEMPLATE_DUPLICATE,
        "模板编码已存在",
    )?;

    let affected = model::update_template(&id, &fields)
        .await
        .map_err(|error| utils::db_error(error, "修改通知模板"))?;
    if affected == 0 {
        return Err(ApiError::new(
            404,
            error_code::NOTICE_TEMPLATE_NOT_FOUND,
            "通知模板不存在或已删除",
        ));
    }
    let updated = model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "读取修改后的通知模板"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::NOTICE_TEMPLATE_NOT_FOUND,
                "通知模板不存在或已删除",
            )
        })?;
    Ok(Res::ok(TemplateRes::from_model(&updated)))
}

/// 删除模板（被通知引用时 409，避免历史通知「模板对不上」）。
#[alun::permission(
    path = "/business/notice/template/{id}",
    method = "DELETE",
    permission = "business:notice:template:delete"
)]
#[alun::delete("/business/notice/template/{id}")]
pub async fn template_delete(Path(id): Path<String>) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let template = model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询待删除的通知模板"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::NOTICE_TEMPLATE_NOT_FOUND,
                "通知模板不存在或已删除",
            )
        })?;

    let used = model::count_notices_using_template(&template.template_code)
        .await
        .map_err(|error| utils::db_error(error, "统计模板引用"))?;
    if used > 0 {
        return Err(ApiError::new(
            409,
            error_code::NOTICE_TEMPLATE_IN_USE,
            format!("模板已被 {used} 条通知引用，不能删除"),
        ));
    }

    let affected = model::soft_delete_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "删除通知模板"))?;
    if affected == 0 {
        return Err(ApiError::new(
            404,
            error_code::NOTICE_TEMPLATE_NOT_FOUND,
            "通知模板不存在或已删除",
        ));
    }
    Ok(Res::ok(()))
}

/// 试渲染（编辑页即时反馈：传变量 JSON，返回渲染后的标题与正文）。
///
/// # 参数
///
/// - `id`：模板主键。
/// - `req.vars`：变量表；不传则用模板声明的 `sample` 值。
///
/// # 异常情况
///
/// - 404：模板不存在；
/// - 422：缺变量 / 语法错误 / 超长。
#[alun::permission(
    path = "/business/notice/template/{id}/preview",
    method = "POST",
    permission = "business:notice:template:list"
)]
#[alun::post("/business/notice/template/{id}/preview")]
pub async fn template_preview(
    Path(id): Path<String>,
    Json(req): Json<TemplatePreviewReq>,
) -> ResResult<TemplatePreviewRes> {
    let id = utils::require_uuid(&id, "id")?;
    let template = model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询待预览的通知模板"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::NOTICE_TEMPLATE_NOT_FOUND,
                "通知模板不存在或已删除",
            )
        })?;

    let vars = req
        .vars
        .filter(|value| !value.is_null())
        .unwrap_or_else(|| {
            let decls = parse_var_decls(template.variables_json.as_deref());
            render::sample_vars(&decls)
        });
    let format = ContentFormat::parse(&template.content_format)?;
    let (title, content) = render::render_notice(
        &template.title_template,
        &template.content_template,
        &vars,
        format,
    )?;
    Ok(Res::ok(TemplatePreviewRes { title, content }))
}

// ==================== 通道配置 ====================

/// 通道配置（本批只读：邮件是否已配置、Webhook 地址掩码、短信是否已配置）。
#[alun::permission(
    path = "/business/notice/channel/config",
    method = "GET",
    permission = "business:notice:channel:config"
)]
#[alun::get("/business/notice/channel/config")]
pub async fn channel_config() -> ResResult<ChannelConfigRes> {
    let settings = model::channel_settings_best_effort().await;
    Ok(Res::ok(settings.config))
}

// ==================== 实时推送（SSE，A6） ====================

/// 订阅实时通知（SSE 长连接）。
///
/// # 鉴权
///
/// 走现有 `AuthLayer`，用户身份只从 `AuthClaims` 取。
///
/// # 返回值
///
/// `text/event-stream`：
///
/// | 帧 | 内容 |
/// |----|------|
/// | 数据帧 | `event: notice` + `data: {notice_id, notice_no, notice_type, level, title, link_url, biz_type, biz_id, receiver_id, created_at}` |
/// | 心跳帧 | 每 [`sse_hub::HEARTBEAT_SECS`] 秒一条注释帧 `: keep-alive`（不触发 `onmessage`，只防代理超时断连） |
///
/// # 生命周期
///
/// 连接断开（客户端关闭 / 网络中断 / 服务端关停）→ 订阅端立即退订，该用户
/// `receiver_count` 归零且频道从内存表摘除（见 [`sse_hub`]，防内存泄漏）。
///
/// # 异常情况
///
/// - 401：凭证缺少有效的用户标识；
/// - 403：无 `business:notice:mine`。
#[alun::permission(
    path = "/business/notice/stream",
    method = "GET",
    permission = "business:notice:mine"
)]
#[alun::get("/business/notice/stream")]
pub async fn stream(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> Response {
    let user_id = match utils::current_user_uuid(&claims) {
        Some(user_id) => user_id,
        None => {
            return ApiError::new(
                401,
                error_code::AUTH_TOKEN_INVALID,
                "当前凭证缺少有效的用户标识",
            )
            .into_response();
        }
    };
    tracing::info!(user_id = %user_id, "SSE 订阅请求已受理");
    sse_hub::subscribe(user_id).into_sse().into_response()
}

/// SSE 频道表统计（排障 / 验收取证：可直接看到每个用户的 `receiver_count`）。
///
/// # 返回值
///
/// 频道数 / 订阅端总数 / 每用户订阅端数 / 累计推送量 / 心跳间隔。
///
/// # 异常情况
///
/// - 403：无 `business:notice:list`。
#[alun::permission(
    path = "/business/notice/stream/stats",
    method = "GET",
    permission = "business:notice:list"
)]
#[alun::get("/business/notice/stream/stats")]
pub async fn stream_stats() -> ResResult<StreamStatsRes> {
    Ok(Res::ok(sse_hub::stats()))
}

// ==================== 内部辅助 ====================

/// 取当前登录用户 UUID。
///
/// # 异常情况
///
/// `claims.sub` 非法 → 401（正常签发的令牌不会走到这里）。
fn current_user(claims: &TokenClaims) -> std::result::Result<uuid::Uuid, ApiError> {
    utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "登录状态无效，请重新登录",
        )
    })
}

/// 校验模板编码格式（`^[A-Za-z][A-Za-z0-9_]{1,63}$`，与迁移 CHECK 一致）。
///
/// # 异常情况
///
/// 非法 → 400。
fn validate_template_code(code: &str) -> std::result::Result<(), ApiError> {
    let mut chars = code.chars();
    let first = chars.next().unwrap_or('\0');
    let valid_len = (2..=64).contains(&code.chars().count());
    let valid_first = first.is_ascii_alphabetic();
    let valid_rest = chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if !valid_len || !valid_first || !valid_rest {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "模板编码需字母开头、2-64 位，只允许字母 / 数字 / 下划线",
        ));
    }
    Ok(())
}

/// 解析模板保存请求 → 数据访问层入参，并在落库前完成**三道校验**。
///
/// 1. 编码格式（与迁移 CHECK 一致）；
/// 2. 标题 / 正文模板语法（`{{name}}` 合法）；
/// 3. 占位符必须在 `variables` 声明中列出，且用 `sample` 值能试渲染成功。
///
/// # 异常情况
///
/// 任一校验失败 → 400 / 422（带中文原因）。
fn build_template_fields(
    req: &TemplateSaveReq,
    operator: Option<uuid::Uuid>,
) -> std::result::Result<TemplateFields<'_>, ApiError> {
    let code = req.template_code.trim();
    validate_template_code(code)?;

    let title_template = req.title_template.trim();
    let content_template = req.content_template.trim();
    render::validate_syntax(title_template)?;
    render::validate_syntax(content_template)?;

    let decls: Vec<TemplateVarDecl> = match &req.variables {
        Some(value) if !value.is_null() => {
            serde_json::from_value::<Vec<TemplateVarDecl>>(value.clone()).map_err(|error| {
                ApiError::new(
                    400,
                    error_code::PARAM_INVALID,
                    format!("variables 格式非法：{error}（应为数组）"),
                )
            })?
        }
        _ => Vec::new(),
    };
    render::ensure_declared(title_template, &decls)?;
    render::ensure_declared(content_template, &decls)?;

    let notice_type = match utils::trimmed(req.notice_type.as_deref()) {
        Some(raw) => NoticeType::parse(raw)?,
        None => NoticeType::System,
    };
    let default_level = match utils::trimmed(req.default_level.as_deref()) {
        Some(raw) => NoticeLevel::parse(raw)?,
        None => NoticeLevel::Info,
    };
    let content_format = match utils::trimmed(req.content_format.as_deref()) {
        Some(raw) => ContentFormat::parse(raw)?,
        None => ContentFormat::Text,
    };
    let status = req
        .status
        .map(EnableStatus::from_i16)
        .transpose()?
        .unwrap_or(EnableStatus::enabled);
    let channels = utils::trimmed(req.channels.as_deref()).unwrap_or("INBOX");

    // 保存前试渲染：变量值取声明的 sample / label，缺省用变量名（保证「保存即能发」）
    let sample_vars = render::sample_vars(&decls);
    render::render_notice(
        title_template,
        content_template,
        &sample_vars,
        content_format,
    )?;

    Ok(TemplateFields {
        template_code: code,
        template_name: req.template_name.trim(),
        notice_type: notice_type.as_str(),
        default_level: default_level.as_str(),
        title_template,
        content_template,
        content_format: content_format.as_str(),
        link_template: utils::trimmed(req.link_template.as_deref()),
        channels,
        variables_json: if decls.is_empty() {
            None
        } else {
            serde_json::to_string(&decls).ok()
        },
        status: status.as_i16(),
        remark: utils::trimmed(req.remark.as_deref()),
        operator,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模板编码格式与迁移里的 CHECK 约束逐字一致（不一致会导致「接口放行、落库报错」）。
    #[test]
    fn template_code_validation_matches_check_constraint() {
        assert!(validate_template_code("NOTICE_IMPORT_DONE").is_ok());
        assert!(validate_template_code("Ab1").is_ok());
        assert!(validate_template_code("A_").is_ok());

        // 太短 / 非法首字符 / 非法字符 / 过长
        assert!(validate_template_code("A").is_err());
        assert!(validate_template_code("1ABC").is_err());
        assert!(validate_template_code("BAD-CODE").is_err());
        assert!(validate_template_code("中文编码").is_err());
        assert!(validate_template_code(&"A".repeat(65)).is_err());
    }

    /// 保存请求 → 入参：语法 / 声明 / 试渲染三道校验都要生效。
    #[test]
    fn build_template_fields_validates_before_save() {
        let mut req = TemplateSaveReq {
            template_code: "NOTICE_TEST".into(),
            template_name: "测试模板".into(),
            notice_type: Some("todo".into()),
            default_level: Some("warn".into()),
            title_template: "待办：{{title}}".into(),
            content_template: "{{content}}".into(),
            content_format: Some("text".into()),
            link_template: Some("/workflow/task/{{taskId}}".into()),
            channels: Some("INBOX,EMAIL".into()),
            variables: Some(serde_json::json!([
                {"name": "title", "required": true, "sample": "订单审批"},
                {"name": "content", "required": true, "sample": "请处理"},
                {"name": "taskId", "required": true, "sample": "42"}
            ])),
            status: Some(1),
            remark: None,
        };
        let fields = build_template_fields(&req, None).expect("合法模板应通过");
        assert_eq!(fields.notice_type, "TODO");
        assert_eq!(fields.default_level, "WARN");
        assert_eq!(fields.content_format, "TEXT");
        assert!(fields.variables_json.is_some());

        // 占位符未声明 → 422
        req.variables = Some(serde_json::json!([{"name": "title"}]));
        let error = build_template_fields(&req, None).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_SYNTAX);

        // 语法错误 → 422
        req.variables = None;
        req.title_template = "待办 {{title".into();
        let error = build_template_fields(&req, None).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_SYNTAX);

        // 非法状态 → 400（先把内容模板也改成无占位符，避免先命中声明校验）
        req.title_template = "标题".into();
        req.content_template = "正文".into();
        req.status = Some(9);
        let error = build_template_fields(&req, None).unwrap_err();
        assert_eq!(error.status, 400);
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
        let error = current_user(&claims).unwrap_err();
        assert_eq!(error.status, 401);
    }
}

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

use axum::Json;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use nik::prelude::*;
use std::net::SocketAddr;

use crate::business::notice::alert_dedup::{self, AlertAction};
use crate::business::notice::dto::{
    ALERT_BIZ_TYPE, ChannelConfigRes, ContentFormat, DEFAULT_ALERT_SOURCE, DeliverySummaryRes, InternalAlertReq,
    InternalAlertRes, MyNoticeQuery, MyNoticeRes, NoticeDetailRes, NoticeFilter, NoticeLevel, NoticePageQuery,
    NoticeRes, NoticeSendReq, NoticeType, StreamQuery, StreamStatsRes, TemplateFilter, TemplatePageQuery,
    TemplatePreviewReq, TemplatePreviewRes, TemplateRes, TemplateSaveReq, TemplateVarDecl, UnreadCountRes,
    alert_level_of, parse_var_decls,
};
use crate::business::notice::model::{self, TemplateFields};
use crate::business::notice::render;
use crate::business::notice::sse_hub;
use crate::business::notice::{DispatchError, DispatchResult, NoticeDraft, NoticeTarget, dispatch};
use cloudpivot_kernel::business::shared::principal;
use cloudpivot_kernel::shared::constants;
use cloudpivot_kernel::shared::constants::EnableStatus;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

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
#[nik::permission(path = "/business/notice", method = "POST", permission = "business:notice:send")]
#[nik::post("/business/notice")]
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
        // 人工发送（管理端「发通知」接口）保持 T3 之前的行为：不看收件人偏好。
        // 分级只对**业务事件**（任务分派 / 打回等）有意义，人工公告不在此列。
        force: true,
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
#[nik::permission(path = "/business/notice/page", method = "GET", permission = "business:notice:list")]
#[nik::get("/business/notice/page")]
pub async fn page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<NoticePageQuery>,
) -> ResResult<PageData<Vec<NoticeRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    // 批次 B · B5（附录 C #14）：管理视角公告列表按数据范围收口（`sys_notice` 无部门列，
    // 部门类范围在 filter_for 里降级为「仅本人」并置 is_degraded()）。
    let principal = principal::resolve_for_claims(&claims).await?;
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

    let result = model::page_notices(&filter, page, page_size, &principal)
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
#[nik::permission(path = "/business/notice/{id}", method = "GET", permission = "business:notice:list")]
#[nik::get("/business/notice/{id}")]
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
        deliveries: deliveries.iter().map(DeliverySummaryRes::from_row).collect(),
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
#[nik::permission(
    path = "/business/notice/{id}",
    method = "DELETE",
    permission = "business:notice:delete"
)]
#[nik::delete("/business/notice/{id}")]
pub async fn delete(Path(id): Path<String>) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let affected = model::soft_delete_notice(&id)
        .await
        .map_err(|error| utils::db_error(error, "删除通知"))?;
    if affected == 0 {
        return Err(ApiError::new(404, error_code::NOTICE_NOT_FOUND, "通知不存在或已删除"));
    }
    Ok(Res::ok(()))
}

// ==================== 我的通知（人人可用） ====================

/// 我的通知分页。
///
/// # 参数
///
/// - `claims`：当前用户（只查自己的接收行）。
/// - `query`：`read_flag` 筛选 + `pin_critical`（严重级别置顶，缺省开）+ 分页。
///
/// # 返回值
///
/// 分页数据。缺省按送达时间倒序；`pin_critical` 为真（缺省）时 `level = ERROR`
/// 的严重告警排在最前 —— 每条都带 `pinned` 标记供前端打「置顶」角标。
///
/// # 异常情况
///
/// - 403：无 `business:notice:mine`（`V0045` 已授予所有角色）；
/// - 500：数据库异常。
#[nik::permission(path = "/business/notice/mine", method = "GET", permission = "business:notice:mine")]
#[nik::get("/business/notice/mine")]
pub async fn mine(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<MyNoticeQuery>,
) -> ResResult<PageData<Vec<MyNoticeRes>>> {
    let user_id = current_user(&claims)?;
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let pin_critical = query.pin_critical.unwrap_or(true);
    let result = model::page_my_notices(&user_id, query.read_flag, pin_critical, page, page_size)
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
#[nik::permission(
    path = "/business/notice/unread-count",
    method = "GET",
    permission = "business:notice:mine"
)]
#[nik::get("/business/notice/unread-count")]
pub async fn unread_count(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<UnreadCountRes> {
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
#[nik::permission(
    path = "/business/notice/read/{receiver_id}",
    method = "PUT",
    permission = "business:notice:mine"
)]
#[nik::put("/business/notice/read/{receiver_id}")]
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
#[nik::permission(
    path = "/business/notice/read-all",
    method = "PUT",
    permission = "business:notice:mine"
)]
#[nik::put("/business/notice/read-all")]
pub async fn mark_all_read(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<serde_json::Value> {
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
#[nik::permission(
    path = "/business/notice/mine/{receiver_id}",
    method = "DELETE",
    permission = "business:notice:mine"
)]
#[nik::delete("/business/notice/mine/{receiver_id}")]
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
#[nik::permission(
    path = "/business/notice/template/page",
    method = "GET",
    permission = "business:notice:template:list"
)]
#[nik::get("/business/notice/template/page")]
pub async fn template_page(Query(query): Query<TemplatePageQuery>) -> ResResult<PageData<Vec<TemplateRes>>> {
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
#[nik::permission(
    path = "/business/notice/template",
    method = "POST",
    permission = "business:notice:template:create"
)]
#[nik::post("/business/notice/template")]
pub async fn template_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<TemplateSaveReq>,
) -> ResResult<TemplateRes> {
    let operator = utils::current_user_uuid(&claims);
    let fields = build_template_fields(&req, operator)?;

    let existed = model::count_template_code(fields.template_code, None)
        .await
        .map_err(|error| utils::db_error(error, "校验模板编码唯一性"))?;
    utils::ensure_unique(existed, error_code::NOTICE_TEMPLATE_DUPLICATE, "模板编码已存在")?;

    let id = uuid::Uuid::now_v7();
    model::insert_template(&id, &fields)
        .await
        .map_err(|error| utils::db_error(error, "新建通知模板"))?;
    let created = model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "读取新建的通知模板"))?
        .ok_or_else(|| ApiError::new(500, error_code::NOTICE_TEMPLATE_NOT_FOUND, "模板创建后未读到记录"))?;
    Ok(Res::ok(TemplateRes::from_model(&created)))
}

/// 修改模板（同样先校验再落库）。
///
/// # 异常情况
///
/// - 404：模板不存在；
/// - 409：编码与其它模板重复。
#[nik::permission(
    path = "/business/notice/template/{id}",
    method = "PUT",
    permission = "business:notice:template:update"
)]
#[nik::put("/business/notice/template/{id}")]
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
        .ok_or_else(|| ApiError::new(404, error_code::NOTICE_TEMPLATE_NOT_FOUND, "通知模板不存在或已删除"))?;

    let fields = build_template_fields(&req, operator)?;
    let existed = model::count_template_code(fields.template_code, Some(&id))
        .await
        .map_err(|error| utils::db_error(error, "校验模板编码唯一性"))?;
    utils::ensure_unique(existed, error_code::NOTICE_TEMPLATE_DUPLICATE, "模板编码已存在")?;

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
        .ok_or_else(|| ApiError::new(404, error_code::NOTICE_TEMPLATE_NOT_FOUND, "通知模板不存在或已删除"))?;
    Ok(Res::ok(TemplateRes::from_model(&updated)))
}

/// 删除模板（被通知引用时 409，避免历史通知「模板对不上」）。
#[nik::permission(
    path = "/business/notice/template/{id}",
    method = "DELETE",
    permission = "business:notice:template:delete"
)]
#[nik::delete("/business/notice/template/{id}")]
pub async fn template_delete(Path(id): Path<String>) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let template = model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询待删除的通知模板"))?
        .ok_or_else(|| ApiError::new(404, error_code::NOTICE_TEMPLATE_NOT_FOUND, "通知模板不存在或已删除"))?;

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
#[nik::permission(
    path = "/business/notice/template/{id}/preview",
    method = "POST",
    permission = "business:notice:template:list"
)]
#[nik::post("/business/notice/template/{id}/preview")]
pub async fn template_preview(
    Path(id): Path<String>,
    Json(req): Json<TemplatePreviewReq>,
) -> ResResult<TemplatePreviewRes> {
    let id = utils::require_uuid(&id, "id")?;
    let template = model::find_template(&id)
        .await
        .map_err(|error| utils::db_error(error, "查询待预览的通知模板"))?
        .ok_or_else(|| ApiError::new(404, error_code::NOTICE_TEMPLATE_NOT_FOUND, "通知模板不存在或已删除"))?;

    let vars = req.vars.filter(|value| !value.is_null()).unwrap_or_else(|| {
        let decls = parse_var_decls(template.variables_json.as_deref());
        render::sample_vars(&decls)
    });
    let format = ContentFormat::parse(&template.content_format)?;
    let (title, content) = render::render_notice(&template.title_template, &template.content_template, &vars, format)?;
    Ok(Res::ok(TemplatePreviewRes { title, content }))
}

// ==================== 通道配置 ====================

/// 通道配置（本批只读：邮件是否已配置、Webhook 地址掩码、短信是否已配置）。
#[nik::permission(
    path = "/business/notice/channel/config",
    method = "GET",
    permission = "business:notice:channel:config"
)]
#[nik::get("/business/notice/channel/config")]
pub async fn channel_config() -> ResResult<ChannelConfigRes> {
    let settings = model::channel_settings_best_effort().await;
    Ok(Res::ok(settings.config))
}

// ==================== 实时推送（SSE，A6） ====================

/// 订阅实时通知（SSE 长连接）。
///
/// # 鉴权口径（遗留修复⑥定稿，文档见 `docs/批次A-SSE鉴权口径.md`）
///
/// **优先** `Authorization: Bearer <token>`（与其它接口完全一致，令牌不进 URL）。
///
/// **降级**：浏览器原生 `EventSource` 不能自定义请求头，此时才允许 `?token=<token>`：
///
/// | 约束 | 实现 |
/// |------|------|
/// | 仅此端点 | 认证层只对 `NIK_AUTH_QUERY_TOKEN_PATHS` 白名单里的路径采信查询令牌（默认空 = 关闭） |
/// | 与头认证等价 | 走同一条 JWT 校验（签名 / 过期 / 类型 / 黑名单），不是「另一套放行逻辑」 |
/// | 短期有效 | 只接受 access token（`access_token_expire_secs` 决定寿命），refresh token 过不了类型校验 |
/// | 日志脱敏 | 认证层只记来源 header/query；`request_log` 只记 path 不记 query；令牌本体从不打印 |
/// | 不可伪造他人频道 | 身份**只**取自令牌声明；`?user_id=` / `?receiver_id=` 一律 400 |
///
/// 前端也可用 `fetch` + `ReadableStream` 带头读流（示例见 `docs/批次A-A6-验证证据.md`），
/// 那条路不受白名单约束，是更推荐的姿势。用户身份**只**从 `AuthClaims` 取。
///
/// # 返回值
///
/// `text/event-stream`：
///
/// | 帧 | 内容 |
/// |----|------|
/// | 首帧 | 建连即刻一条注释帧 `: connected`（让浏览器 `fetch` 的 Promise 立刻 resolve，见 [`sse_hub`]） |
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
#[nik::permission(
    path = "/business/notice/stream",
    method = "GET",
    permission = "business:notice:mine"
)]
#[nik::get("/business/notice/stream")]
pub async fn stream(
    Query(query): Query<StreamQuery>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> Response {
    // 频道归属只认令牌：客户端若显式指定身份，直接 400 而不是「偷偷忽略」——
    // 静默忽略会让攻击者以为成功，排障时也难分辨「前端传错了」还是「后端没校验」。
    if query.user_id.is_some() || query.receiver_id.is_some() {
        return ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "不接受客户端指定频道归属（user_id / receiver_id），用户身份只取自令牌",
        )
        .into_response();
    }
    // `query.token` 已由认证层校验（白名单端点才会采信）；此处**不打印其值**，
    // 避免令牌进入 tracing 日志。
    let user_id = match utils::current_user_uuid(&claims) {
        Some(user_id) => user_id,
        None => {
            return ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前凭证缺少有效的用户标识").into_response();
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
#[nik::permission(
    path = "/business/notice/stream/stats",
    method = "GET",
    permission = "business:notice:list"
)]
#[nik::get("/business/notice/stream/stats")]
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
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "登录状态无效，请重新登录"))
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
    render::render_notice(title_template, content_template, &sample_vars, content_format)?;

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

// ==================== 内部告警入口（POST /internal/alert，P1-07） ====================

/// 内网 token 的请求头名（对端不在回环时必须带对）。
pub const INTERNAL_TOKEN_HEADER: &str = "x-internal-token";

/// 内网 token 的环境变量名；**未注入 = 只接受回环调用**（fail-closed）。
///
/// 为什么不放 `config.toml`：这是部署期注入的口令类值，与 JWT / 存储主密钥同款处理
/// （env 注入、不进仓库、不落日志）。
pub const INTERNAL_TOKEN_ENV: &str = "NIK_ALERT_INTERNAL_TOKEN";

/// 调用方准入判定（纯函数，不碰库不读 env，便于单测）。
///
/// # 判据
///
/// 1. 对端是回环（`127.0.0.0/8` / `::1`）→ 放行：巡检脚本与后端同机；
/// 2. 否则必须带 `X-Internal-Token` 且与环境变量 `NIK_ALERT_INTERNAL_TOKEN` 逐字节相等；
/// 3. 环境变量为空时**只认回环**，绝不「空 token 也算过」。
///
/// # 为什么只看对端地址，不看 `X-Forwarded-For` / `X-Real-IP`
///
/// 那两个头客户端可以自己写。本接口在根路径（nginx 只反代 `/api/` 与 `/metrics`），
/// 正常永远不经过代理；一旦按头判来源，任何能直连 8023 的人在头里写 `127.0.0.1`
/// 就绕过了闸。对端地址由内核给，伪造不了。
pub(crate) fn internal_caller_allowed(peer: std::net::IpAddr, provided: Option<&str>, expected: Option<&str>) -> bool {
    if peer.is_loopback() {
        return true;
    }
    let (Some(provided), Some(expected)) = (
        provided.map(str::trim).filter(|value| !value.is_empty()),
        expected.map(str::trim).filter(|value| !value.is_empty()),
    ) else {
        return false;
    };
    constant_time_eq(provided.as_bytes(), expected.as_bytes())
}

/// 定长比较（避免按字节短路泄漏 token 前缀）。长度不同直接判否（只泄漏长度，可接受）。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 内部告警入口：把运维巡检检测到的异常落成一条**站内消息**，收件人是超管角色。
///
/// # 装配（**为什么不走 `#[nik::post]` 宏路由**）
///
/// 本接口挂在**根路径** `/internal/alert`，不是 `/api/internal/alert`：
///
/// - nginx 只反代 `location /api/` 与 `location /metrics`（见 `frontend/deploy/nginx.conf`），
///   根路径下的 `/internal/*` 因此**天然不对外**；放进 `/api/` 就等于把它挂到公网，
///   而它的准入闸只是「对端是不是回环」——经过 nginx 之后对端恒为 `127.0.0.1`，闸就废了；
/// - 免认证靠本函数上的 `#[nik::no_auth("!/internal/alert")]`（`!` = 绝对路径，不拼前缀），
///   路由体由 [`crate::run`] 的 `App::post` 手动注册（宏路由会被 `[router].prefix` 嵌套）。
///
/// # 请求
///
/// `{"level":"critical|warning","title":"…","content":"…","source":"alert_check"}`
/// （`source` / `idempotency_key` 可选）。
///
/// # 返回值
///
/// 发送结果（`notice_id` / `notice_no` / `receiver_count` / `duplicated`）。
///
/// # 异常情况
///
/// - `403 + 30001`：非回环且没带对内网 token；
/// - `400 + 10001`：级别非法，或标题 / 正文只有空白（空白串能过 `min=1`，由 handler 兜）；
/// - `400 + 400` / `422 + 422`：请求体缺字段 / 字段长度越界 —— `ValidatedJson` 在进 handler 前就拦；
/// - `500 + 10004`：库异常，或内置 `SUPER_ADMIN` 角色缺失（迁移未跑全）。
#[nik::no_auth("!/internal/alert")]
pub async fn internal_alert(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ValidatedJson(req): ValidatedJson<InternalAlertReq>,
) -> ResResult<InternalAlertRes> {
    let provided = headers.get(INTERNAL_TOKEN_HEADER).and_then(|value| value.to_str().ok());
    let expected = std::env::var(INTERNAL_TOKEN_ENV).ok();
    if !internal_caller_allowed(peer.ip(), provided, expected.as_deref()) {
        tracing::warn!(
            peer = %peer,
            "内部告警入口拒绝了非回环调用（未携带正确的 X-Internal-Token）"
        );
        return Err(ApiError::new(
            403,
            error_code::PERMISSION_DENIED,
            "内部告警接口只允许本机调用（或携带正确的内网 token）",
        ));
    }
    let result = deliver_internal_alert(req).await?;
    Ok(Res::ok(result))
}

/// 告警落库阶段（与准入判定分开：准入是纯函数，这一段才碰库、才发通知）。
///
/// 收件人 = 超管角色（`SUPER_ADMIN`）下的**全部可用账号**，由
/// [`crate::business::notice::dispatch`] 的 `ROLE` 扇出在一条 `INSERT … SELECT` 里算出来
/// （排除软删 / 停用 / 授权过期），不在 Rust 侧循环。
///
/// # 这是薄封装
///
/// 真正的实现在 [`deliver_internal_alert_at`]（时刻是入参，可 mock）。这里只取
/// `Utc::now()` + 环境变量窗口，让线上路径与单测走**同一段代码**。
pub(crate) async fn deliver_internal_alert(req: InternalAlertReq) -> std::result::Result<InternalAlertRes, ApiError> {
    deliver_internal_alert_at(req, chrono::Utc::now(), alert_dedup::window()).await
}

/// [`deliver_internal_alert`] 的可注入版本：**时刻与窗口都是入参**（P1-08）。
///
/// # 参数
///
/// - `req`：请求体（`level` / `title` / `content` / `source` / `idempotency_key`）；
/// - `now`：本次调用时刻。生产传 `Utc::now()`；单测传 mock 时间就能在毫秒内
///   把「等 11 分钟」这一段跑完（卡面验收 ② 要求 mock 时间）；
/// - `window`：去重窗口（默认 600 秒，见 [`alert_dedup::window`]）。
///
/// # 三件事的顺序（顺序本身就是设计）
///
/// 1. **幂等键先查**：给了 `idempotency_key` 且库里已有该键 → 直接返回首次结果
///    （`duplicated = true`），**不进**去重窗口 —— 否则一次重放会被判成"窗口内重复"
///    而拿不到首次的 `notice_id`，脚本会把重放当成失败；
/// 2. **去重窗口认领**：窗口内重复 → 直接返回 `deduped = true`，**不落库**；
/// 3. **发布 + 回填**：`dispatch` 成功后把 `notice_id` 写回窗口行（见
///    [`model::bind_alert_dedup_notice`]）。回填失败只 `warn`：通知已经发出去了，
///    最坏后果是下一轮多发一条，不会静默。
///
/// # 返回值
///
/// [`InternalAlertRes`]：三个结局见该类型注释（发布 / 合并 / 幂等重放）。
///
/// # 异常情况
///
/// - `400 + 10001`：级别非法 / 标题正文只有空白；
/// - `422 + 422`：字段长度越界（`ValidatedJson` 在进 handler 前拦）；
/// - `500 + 10004`：库异常，或超管角色缺失。
pub(crate) async fn deliver_internal_alert_at(
    req: InternalAlertReq,
    now: chrono::DateTime<chrono::Utc>,
    window: chrono::Duration,
) -> std::result::Result<InternalAlertRes, ApiError> {
    let level = alert_level_of(&req.level)?;
    let title = req.title.trim();
    let content = req.content.trim();
    if title.is_empty() {
        return Err(ApiError::new(400, error_code::PARAM_INVALID, "标题不能为空"));
    }
    if content.is_empty() {
        return Err(ApiError::new(400, error_code::PARAM_INVALID, "正文不能为空"));
    }
    let source = utils::trimmed(req.source.as_deref())
        .unwrap_or(DEFAULT_ALERT_SOURCE)
        .to_string();
    let window_seconds = window.num_seconds();
    let idempotency_key = utils::trimmed(req.idempotency_key.as_deref()).map(str::to_string);

    // ---------- 1. 永久幂等键优先：重放直接回吐首次结果，不进去重窗口 ----------
    if let Some(key) = idempotency_key.as_deref()
        && let Some(existing) = model::find_notice_by_idem_summary(key)
            .await
            .map_err(|error| utils::db_error(error, "按幂等键查告警通知"))?
    {
        tracing::info!(
            notice_id = %existing.0,
            source = %source,
            "内部告警命中幂等键：返回首次结果（不新增、不计入去重窗口）"
        );
        return Ok(InternalAlertRes::delivered(
            &DispatchResult {
                notice_id: existing.0,
                notice_no: existing.1,
                receiver_count: existing.2,
                duplicated: true,
                channels: Vec::new(),
                scheduled: false,
                scheduled_at: None,
            },
            level,
            0,
            window_seconds,
        ));
    }

    // ---------- 2. 去重窗口：窗口内重复只累加计数，不落库 ----------
    let verdict = model::claim_alert_dedup(&source, title, now, window)
        .await
        .map_err(|error| utils::db_error(error, "告警去重窗口认领"))?;
    if let AlertAction::Suppress { merged } = verdict.action {
        tracing::info!(
            source = %source,
            title = %title,
            merged,
            window_seconds,
            level = level.as_str(),
            "内部告警在去重窗口内重复：已合并（不落库）"
        );
        return Ok(InternalAlertRes::suppressed(level, merged, window_seconds));
    }
    let merged_in_previous_window = verdict.action.merged();

    // ---------- 3. 发布 ----------
    let role_id = model::super_admin_role_id()
        .await
        .map_err(|error| utils::db_error(error, "查询超级管理员角色"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!(
                    "内置超级管理员角色缺失（role_code = {}），告警无处投递",
                    constants::SUPER_ADMIN_ROLE_CODE
                ),
            )
        })?;

    // 上一窗口被合并过 ⇒ 正文带「近 10 分钟已重复 N 次」（卡面口径）
    let content = alert_dedup::with_repeat_note(content, merged_in_previous_window, window);

    let mut draft = NoticeDraft::text(
        NoticeType::Alert,
        level,
        title,
        &content,
        NoticeTarget::Role(vec![role_id]),
    );
    draft.biz_type = Some(ALERT_BIZ_TYPE.to_string());
    draft.sender_name = Some(source.clone());
    draft.idempotency_key = idempotency_key;

    let result = dispatch(draft).await.map_err(DispatchError::into_api)?;

    // 回填「本窗口真的发出去了」的唯一凭据（判据 ② 的输入）。失败只 warn：
    // 通知已在消息中心，最坏后果是下一轮多发一条 —— 宁可多响，不可少响。
    match model::bind_alert_dedup_notice(&source, title, &result.notice_id).await {
        Ok(0) => tracing::warn!(
            source = %source,
            title = %title,
            "告警已发出，但去重窗口行不见了（被清理？）：下一轮同类告警会再发一条"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            error = %error,
            source = %source,
            title = %title,
            "告警已发出，但回填去重窗口失败：下一轮同类告警会再发一条"
        ),
    }

    if result.receiver_count == 0 {
        tracing::warn!(
            notice_id = %result.notice_id,
            source = %source,
            title = %title,
            "内部告警已落库，但超管角色下没有可用账号（receiver_count = 0）：告警没人看得到"
        );
    } else {
        tracing::info!(
            notice_id = %result.notice_id,
            notice_no = %result.notice_no,
            receiver_count = result.receiver_count,
            duplicated = result.duplicated,
            source = %source,
            level = level.as_str(),
            merged_in_previous_window,
            window_seconds,
            "内部告警已转为站内消息"
        );
    }
    Ok(InternalAlertRes::delivered(
        &result,
        level,
        merged_in_previous_window,
        window_seconds,
    ))
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

    // ==================== 内部告警入口（P1-07） ====================

    /// 级别映射：巡检口径 → 通知级别（`critical` 必须是 ERROR，不能退化成 INFO）。
    #[test]
    fn alert_level_mapping_is_strict() {
        assert_eq!(alert_level_of("critical").expect("critical 应通过"), NoticeLevel::Error);
        assert_eq!(
            alert_level_of("WARNING").expect("warning 大小写不敏感"),
            NoticeLevel::Warn
        );
        assert_eq!(
            alert_level_of("  Critical ").expect("前后空白应被忽略"),
            NoticeLevel::Error
        );

        let error = alert_level_of("fatal").expect_err("未知级别必须拒绝");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("critical"), "错误文案要点名允许值：{}", error.msg);
    }

    /// 准入闸：回环放行；非回环必须带对内网 token；服务端没配 token 时非回环一律拒。
    #[test]
    fn internal_caller_gate_only_trusts_peer_address() {
        let loopback: std::net::IpAddr = "127.0.0.1".parse().expect("常量应可解析");
        let loopback_v6: std::net::IpAddr = "::1".parse().expect("常量应可解析");
        let lan: std::net::IpAddr = "172.18.10.145".parse().expect("常量应可解析");

        // 本机巡检脚本：不带任何头也放行
        assert!(internal_caller_allowed(loopback, None, None));
        assert!(internal_caller_allowed(loopback_v6, None, Some("s3cret")));

        // 非回环：无 token / 错 token / 服务端没配 token / 空 token —— 全部拒绝
        assert!(!internal_caller_allowed(lan, None, Some("s3cret")));
        assert!(!internal_caller_allowed(lan, Some("wrong"), Some("s3cret")));
        assert!(!internal_caller_allowed(lan, Some("s3cret"), None));
        assert!(!internal_caller_allowed(lan, Some(""), Some("")));
        assert!(!internal_caller_allowed(lan, Some("s3"), Some("s3cret")));

        // 非回环 + 对 token → 放行（跨机调用），前后空白忽略
        assert!(internal_caller_allowed(lan, Some(" s3cret "), Some("s3cret")));
    }

    /// 真库用例的串行锁（清理按前缀删，并行会互删夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 夹具账号前缀：清理只删自己造的行。
    const FIXTURE_USER_PREFIX: &str = "p107.alert.";

    /// 连库（进程内只做一次）；`false` = 没配 `ERP_DB_URL` 或连不上（SKIP，不假绿）。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(error) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{error}）");
                        false
                    }
                }
            })
            .await
    }

    /// 清本用例的夹具：通知（级联收件人 / 投递）→ 去重窗口 → 角色绑定 → 用户。
    ///
    /// 去重窗口按**标题**删（两个真库用例的标题都含「真库用例」），不按 `source` 整片删：
    /// 同一个库上跑真实巡检时 `source = 'alert_check'` 的行不能顺手清掉。
    async fn cleanup_alert_fixtures() {
        db().execute_with_params("DELETE FROM sys_notice WHERE idempotency_key LIKE 'p107test:%'", vec![])
            .await
            .unwrap_or_else(|error| panic!("清理 sys_notice 失败：{error}"));
        db().execute_with_params(
            "DELETE FROM sys_notice WHERE biz_type = 'ALERT' AND title LIKE '%真库用例%'",
            vec![],
        )
        .await
        .expect("清理告警通知失败");
        db().execute_with_params(
            "DELETE FROM sys_notice_alert_dedup WHERE title LIKE '%真库用例%'",
            vec![],
        )
        .await
        .expect("清理去重窗口失败");
        db().execute_with_params(
            "DELETE FROM sys_user_role WHERE user_id IN \
             (SELECT id FROM sys_user WHERE username LIKE 'p107.alert.%')",
            vec![],
        )
        .await
        .expect("清理 sys_user_role 失败");
        db().execute_with_params("DELETE FROM sys_user WHERE username LIKE 'p107.alert.%'", vec![])
            .await
            .expect("清理 sys_user 失败");
    }

    /// 造一个挂在超管角色下的夹具账号，返回用户主键。
    async fn fixture_super_admin_user(role: uuid::Uuid) -> uuid::Uuid {
        let user = uuid::Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?, ?, ?, ?, 1)",
            vec![
                Value::from(user),
                Value::from(format!("{FIXTURE_USER_PREFIX}{}", &user.to_string()[..8])),
                Value::from("x"),
                Value::from("P1-07 告警夹具"),
            ],
        )
        .await
        .expect("插入夹具用户失败");
        db().execute_with_params(
            "INSERT INTO sys_user_role (user_id, role_id) VALUES (?, ?)",
            vec![Value::from(user), Value::from(role)],
        )
        .await
        .expect("绑定超管角色失败");
        user
    }

    /// 真库用例的告警请求（`source = alert_check`，与巡检脚本同口径）。
    fn alert_req(title: &str, level: &str) -> InternalAlertReq {
        InternalAlertReq {
            level: level.into(),
            title: title.into(),
            content: "告警通道验收：巡检 → /internal/alert → 站内消息".into(),
            source: Some("alert_check".into()),
            idempotency_key: None,
        }
    }

    /// 内部告警落库：1 条 `sys_notice`（ALERT / ERROR / ROLE 目标）+ 收件人含超管用户。
    ///
    /// 跑法（缺 `ERP_DB_URL` 时 SKIP，不假绿）：
    ///
    /// ```bash
    /// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<已迁移的库>'
    /// cargo test --lib business::notice::handler::tests::internal_alert -- --test-threads=1
    /// ```
    ///
    /// 标题带随机后缀：去重窗口是按 `(source, title)` 存的，标题固定会让**第二次跑**
    /// 直接命中上一轮留下的窗口而被合并（用例假红）。
    #[tokio::test]
    async fn internal_alert_persists_one_notice_to_super_admin() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        let Some(role) = model::super_admin_role_id().await.expect("查询超管角色不应失败") else {
            println!("SKIP: 测试库没有 SUPER_ADMIN 角色（迁移未跑全）");
            return;
        };
        cleanup_alert_fixtures().await;
        let user = fixture_super_admin_user(role).await;

        let key = format!("p107test:{}", uuid::Uuid::now_v7());
        let title = format!(
            "磁盘使用率 99%（P1-07 真库用例 {}）",
            &uuid::Uuid::now_v7().to_string()[..8]
        );
        let mut req = alert_req(&title, "critical");
        req.idempotency_key = Some(key.clone());

        let result = deliver_internal_alert(req.clone()).await.expect("告警应落库");

        assert!(!result.duplicated, "首次调用不应命中幂等");
        assert!(result.published && !result.deduped, "首次调用应真的落库");
        assert_eq!(result.level, "ERROR", "critical → ERROR");
        assert_eq!(result.merged_count, 0, "首发不带合并提示");
        assert!(
            result.receiver_count >= 1,
            "超管角色下至少有夹具用户，receiver_count={}",
            result.receiver_count
        );

        // ① 落库 1 条 sys_notice，字段与契约一致
        let rows = db()
            .find_with_params(
                "SELECT id, notice_type, level, title, sender_name, target_type, content \
                 FROM sys_notice WHERE idempotency_key = ?",
                vec![Value::from(key.as_str())],
            )
            .await
            .expect("查询 sys_notice 失败");
        assert_eq!(rows.len(), 1, "同一幂等键只应落 1 条 sys_notice");
        let row = &rows[0];
        let notice_id: uuid::Uuid = row.get_typed("id").expect("id 应为 UUID");
        assert_eq!(
            result.notice_id.as_deref(),
            Some(notice_id.to_string().as_str()),
            "返回的 notice_id 必须与库里一致"
        );
        assert_eq!(row.get_typed::<String>("notice_type").expect("notice_type"), "ALERT");
        assert_eq!(
            row.get_typed::<String>("level").expect("level"),
            "ERROR",
            "critical → ERROR"
        );
        assert_eq!(row.get_typed::<String>("target_type").expect("target_type"), "ROLE");
        assert_eq!(
            row.get_typed::<String>("sender_name").expect("sender_name"),
            "alert_check"
        );
        assert!(
            row.get_typed::<String>("content")
                .expect("content")
                .contains("站内消息"),
            "正文必须原样保留"
        );

        // ② 收件人落在 sys_notice_receiver，且包含夹具超管账号
        let receivers: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM sys_notice_receiver WHERE notice_id = ?",
                vec![Value::from(notice_id)],
            )
            .await
            .expect("统计收件人失败");
        assert_eq!(
            receivers, result.receiver_count,
            "落库收件人数必须等于回写的 receiver_count"
        );
        let hit: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM sys_notice_receiver WHERE notice_id = ? AND user_id = ?",
                vec![Value::from(notice_id), Value::from(user)],
            )
            .await
            .expect("查询夹具收件人失败");
        assert_eq!(hit, 1, "超管账号必须收到这条告警");

        // ③ 幂等：同一键再打一次 → duplicated 且不新增行（脚本重试不会刷屏）
        //
        // 注意这里**不能**被去重窗口吃掉：同键重放走的是「幂等键预查」分支
        // （`find_notice_by_idem_summary`），在进窗口判定之前就返回了首次结果。
        let again = deliver_internal_alert(req).await.expect("重放应返回首次结果");
        assert!(again.duplicated, "同键重放必须判为重复");
        assert!(again.published && !again.deduped, "幂等重放仍是「已落库」结局");
        assert_eq!(again.notice_id, result.notice_id);
        let total: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM sys_notice WHERE idempotency_key = ?",
                vec![Value::from(key.as_str())],
            )
            .await
            .expect("重放后统计失败");
        assert_eq!(total, 1, "重放不得新增 sys_notice 行");

        cleanup_alert_fixtures().await;
    }

    /// **P1-08 卡面验收（真库 + mock 时间）**：同一 `(source,title)` 连发 5 条只落 1 条；
    /// 11 分钟后补发的第 2 条正文带「近 10 分钟已重复 4 次」。
    ///
    /// # 为什么必须真库
    ///
    /// 卡面要检的三件事都在 SQL 侧：`FOR UPDATE` + `ON CONFLICT DO UPDATE` 的窗口写回、
    /// `sys_notice` 的实际行数、正文的实际落库内容。纯函数单测（`alert_dedup::tests`）
    /// 只证明判据对，证明不了"判据真的接到了这两张表上"。
    ///
    /// 时间用 `deliver_internal_alert_at` 注入，所以「等 11 分钟」是毫秒级的事。
    ///
    /// 跑法：
    ///
    /// ```bash
    /// export ERP_DB_URL='postgres://…/<已迁移的库>'
    /// cargo test --lib business::notice::handler::tests::alert_dedup -- --test-threads=1
    /// ```
    #[tokio::test]
    async fn alert_dedup_ten_minute_window() {
        let _guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return;
        }
        if model::super_admin_role_id()
            .await
            .expect("查询超管角色不应失败")
            .is_none()
        {
            println!("SKIP: 测试库没有 SUPER_ADMIN 角色（迁移未跑全）");
            return;
        }
        cleanup_alert_fixtures().await;

        let title = format!(
            "磁盘使用率 92%（P1-08 真库用例 {}）",
            &uuid::Uuid::now_v7().to_string()[..8]
        );
        let window = crate::chrono::Duration::seconds(crate::business::notice::alert_dedup::DEFAULT_WINDOW_SECONDS);
        let t0 = crate::chrono::Utc::now();

        // ---------- ① 连发 5 条同 (source,title)：只落 1 条，其余 4 条计入 count ----------
        let mut published = 0;
        let mut suppressed = 0;
        let mut last_merged = 0;
        for i in 0..5 {
            let now = t0 + crate::chrono::Duration::seconds(i);
            let res = deliver_internal_alert_at(alert_req(&title, "critical"), now, window)
                .await
                .expect("告警处置不应失败");
            if res.published {
                published += 1;
                assert_eq!(res.merged_count, 0, "窗口内首发不带合并提示");
            } else {
                assert!(res.deduped, "第 {} 条应判为窗口内重复", i + 1);
                suppressed += 1;
                last_merged = res.merged_count;
            }
        }
        assert_eq!(published, 1, "5 条同源同标题只允许落 1 条");
        assert_eq!(suppressed, 4, "其余 4 条必须被合并");
        assert_eq!(last_merged, 4, "第 5 条时累计合并次数应为 4");

        let notice_rows: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM sys_notice WHERE biz_type = 'ALERT' AND title = ?",
                vec![Value::from(title.as_str())],
            )
            .await
            .expect("统计告警通知失败");
        assert_eq!(notice_rows, 1, "窗口内只应有 1 条 sys_notice");

        let state = model::find_alert_dedup("alert_check", &title)
            .await
            .expect("读去重窗口失败")
            .expect("去重窗口行应存在");
        assert_eq!(state.repeat_count, 4, "窗口行 repeat_count 应为 4");
        let first_notice_id = state.notice_id.expect("发布后必须回填 notice_id（判据 ② 的输入）");
        // PG 的 timestamptz 只到微秒，而 `Utc::now()` 带纳秒 —— 用秒级容差比较，
        // 免得用例因为 0.0000001 秒的精度差假红。
        assert!(
            (state.window_start - t0).num_seconds().abs() <= 1,
            "窗口起点 = 首次发布的时刻（差 {}s）",
            (state.window_start - t0).num_seconds()
        );

        // ---------- ② 等 11 分钟再发 1 条：落第 2 条，正文含「已重复 4 次」 ----------
        let later = t0 + crate::chrono::Duration::minutes(11);
        let res = deliver_internal_alert_at(alert_req(&title, "critical"), later, window)
            .await
            .expect("窗口过后的告警应发布");
        assert!(res.published, "窗口过后必须发布");
        assert_eq!(res.merged_count, 4, "要把上一窗口的合并次数带出来");
        let second_notice_id = res.notice_id.clone().expect("第二窗口应落库");

        let content: String = db()
            .scalar_with_params(
                "SELECT content FROM sys_notice WHERE id = ?::uuid",
                vec![Value::from(second_notice_id.parse::<uuid::Uuid>().expect("UUID"))],
            )
            .await
            .expect("读第二条正文失败");
        assert!(
            content.contains("近 10 分钟已重复 4 次"),
            "正文必须带合并次数，实际：{content}"
        );

        let total: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM sys_notice WHERE biz_type = 'ALERT' AND title = ?",
                vec![Value::from(title.as_str())],
            )
            .await
            .expect("统计第二次发布后的通知失败");
        assert_eq!(total, 2, "两个窗口各落 1 条，共 2 条");

        let state = model::find_alert_dedup("alert_check", &title)
            .await
            .expect("读去重窗口失败")
            .expect("去重窗口行应存在");
        assert_eq!(state.repeat_count, 0, "新窗口计数归零");
        assert!(
            (state.window_start - later).num_seconds().abs() <= 1,
            "窗口起点挪到第二次发布时刻（差 {}s）",
            (state.window_start - later).num_seconds()
        );
        assert_ne!(state.notice_id, Some(first_notice_id), "窗口行要指向新窗口的那条通知");

        cleanup_alert_fixtures().await;
    }

    /// 「我的通知」排序子句：置顶开关必须真的改 ORDER BY，且非置顶时不带 CASE。
    ///
    /// 这条是防「参数加了但没接进 SQL」——那类 bug 在界面上表现为「置顶开关毫无作用」，
    /// 只有对比 SQL 文本才抓得稳。
    #[test]
    fn my_notice_order_puts_errors_first_when_pinned() {
        let pinned = model::my_notice_order_by(true);
        assert!(
            pinned.contains("CASE WHEN n.level = 'ERROR' THEN 0 ELSE 1 END"),
            "置顶必须把 ERROR 排最前：{pinned}"
        );
        assert!(pinned.contains("r.created_at DESC"), "同级别仍按时间倒序：{pinned}");
        assert!(pinned.ends_with("r.id DESC"), "主键兜底保证翻页不重不漏：{pinned}");

        let plain = model::my_notice_order_by(false);
        assert_eq!(plain, "ORDER BY r.created_at DESC, r.id DESC", "关闭置顶 = 纯时间倒序");
        assert!(!plain.contains("CASE"), "非置顶不该带 CASE 分支");
    }
}

//! 审批委托 · 接口层（P3-04 / ARCH-8 第 ⑤ 层 · 场景 E）。
//!
//! # 四个接口（卡面「3. 接口」）
//!
//! | 方法 | 路径 | 权限码 | 行级范围 |
//! |------|------|--------|----------|
//! | `GET` | `/business/flow/delegate/page` | `business:flow:delegate:list` | 缺省只看自己；`all` 需 `business:flow:admin` |
//! | `POST` | `/business/flow/delegate` | `business:flow:delegate:save` | 委托人只能是本人（管理员可代设） |
//! | `PUT` | `/business/flow/delegate/{id}` | `business:flow:delegate:save` | 只能改自己的（管理员可改任意） |
//! | `DELETE` | `/business/flow/delegate/{id}` | `business:flow:delegate:delete` | 只能删自己的（管理员可删任意） |
//!
//! # 为什么 `:save` / `:delete` 还要做行级判定
//!
//! 权限码是**功能级**的：「能配委托」不等于「能替别人配委托」。受托人是谁、
//! 委托到哪一天，直接决定「谁能拿到别人的审批权」—— 不判归属就是提权漏洞。
//! 判据只有一条：`row.delegator_id == 当前用户 || 有 business:flow:admin`（4003 拒绝用 403 + `65004`）。
//!
//! # 错误码
//!
//! | 码 | HTTP | 场景 |
//! |----|------|------|
//! | `65043` | 422 | 区间非法 / 自委托 / 与已有委托重叠（保存期） |
//! | `65005` | 400 | 字段格式非法（id / 时间 / 范围 / 事由）、受托人不可用 |
//! | `65004` | 403 | 越权：看别人的 / 改别人的 / 替别人配 |
//! | `10002` | 404 | 委托不存在 / 已删除 |
//!
//! 保存走**事务**：`pg_advisory_xact_lock`（按委托人串行）→ 重叠检查 → 写入，
//! 见 [`delegate::lock_delegator`] 的文档（并发窗口在那里收口）。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::flow::{delegate, dto};
use crate::shared::error_code;
use crate::utils;

/// 当前用户 id。
fn me_of(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))
}

/// 解析 UUID 字段（非法 → `400 + 65005`，不静默当 404）。
fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim())
        .map_err(|_| ApiError::new(400, error_code::FLOW_FIELD_INVALID, format!("{label}格式非法")))
}

/// 字段非法（`400 + 65005`）。
fn field_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::FLOW_FIELD_INVALID, message)
}

/// 越权（`403 + 65004`）。
fn forbidden(message: impl Into<String>) -> ApiError {
    ApiError::new(403, error_code::FLOW_FORBIDDEN, message)
}

/// 库内行 → 回执。
fn to_res(row: &delegate::DelegateRow) -> dto::DelegateRes {
    dto::DelegateRes {
        id: row.id.to_string(),
        delegator_id: row.delegator_id.to_string(),
        delegator_name: row.delegator_name.clone(),
        delegate_id: row.delegate_id.to_string(),
        delegate_name: row.delegate_name.clone(),
        start_at: row.start_at,
        end_at: row.end_at,
        scope: row.scope.clone().unwrap_or_default(),
        reason: row.reason.clone().unwrap_or_default(),
        enabled: row.enabled,
        is_active_now: row.is_active_now(),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// 保存期的公共校验（建 / 改共用）。
///
/// 顺序不可换：「格式」→「区间纯逻辑」→「受托人可用性」→「重叠」。
/// 先判纯逻辑能省掉一次库查询；重叠放最后因为它最贵（要走索引 + 锁）。
async fn validate_save(
    delegator_id: Uuid,
    delegate_id: Uuid,
    start_at: chrono::DateTime<chrono::Utc>,
    end_at: chrono::DateTime<chrono::Utc>,
) -> std::result::Result<(), ApiError> {
    delegate::validate_window(delegator_id, delegate_id, start_at, end_at)?;
    if let Some(reason) = delegate::delegate_unusable_reason(db(), delegate_id)
        .await
        .map_err(|error| utils::db_error(error, "校验受托人"))?
    {
        return Err(field_invalid(format!("{reason}，请另选受托人")));
    }
    Ok(())
}

// ==================== 1. 分页（我的委托 / 全部委托） ====================

/// 委托分页（`GET /business/flow/delegate/page`）。
///
/// # 参数
///
/// - `delegator_id`：缺省 = 自己；`all` = 全部（需 `business:flow:admin`）；
/// - `only_active`：只看此刻生效的；
/// - `only_enabled`：只看启用中的（默认含已停用，前端要能重新启用）；
/// - `page` / `page_size`：框架 `normalize_page` 归一化。
///
/// # 异常情况
///
/// - `400 + 65005`：`delegator_id` 不是 UUID 也不是 `all`；
/// - `403 + 65004`：看别人的委托但没有 `business:flow:admin`；
/// - `500`：数据库异常。
#[nik::permission(
    path = "/business/flow/delegate/page",
    method = "GET",
    permission = "business:flow:delegate:list"
)]
#[nik::get("/business/flow/delegate/page")]
pub async fn page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::DelegatePageQuery>,
) -> ResResult<dto::DelegatePageRes> {
    let me = me_of(&claims)?;
    let is_admin = claims.has_permission(delegate::PERM_ADMIN);

    let delegator_id = if query.wants_all() {
        if !is_admin {
            return Err(forbidden(format!(
                "查看全部委托需要 {permission} 权限；你自己的委托请用默认视角",
                permission = delegate::PERM_ADMIN
            )));
        }
        None
    } else {
        match query.delegator_id.as_deref() {
            None => Some(me),
            Some(raw) if raw.trim().is_empty() => Some(me),
            Some(raw) => {
                let target = parse_uuid_field(raw, "委托人 id")?;
                if target != me && !is_admin {
                    return Err(forbidden("只能查看自己的委托（查看他人委托需要 business:flow:admin）"));
                }
                Some(target)
            }
        }
    };

    let filter = delegate::DelegateFilter {
        delegator_id,
        only_active: query.only_active.unwrap_or(false),
        only_enabled: query.only_enabled.unwrap_or(false),
    };
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = delegate::page(db(), &filter, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询审批委托"))?;
    Ok(Res::ok(dto::DelegatePageRes {
        list: result.list.iter().map(to_res).collect(),
        total: result.total,
        page: result.page,
        page_size: result.size,
    }))
}

// ==================== 2. 新建 ====================

/// 新建委托（`POST /business/flow/delegate`）。
///
/// 委托不需要审批（ARCH-8 §8 建议口径）—— 它是本人对自己审批权的临时让渡；
/// 生效后**立刻**参与分配（无需重启 / 无需发布流程）。
///
/// # 异常情况
///
/// - `400 + 65005`：id / 时间 / 范围 / 事由非法，或受托人不存在 / 已停用 / 是机器人；
/// - `403 + 65004`：替别人配委托却没有 `business:flow:admin`；
/// - `422 + 65043`：自委托 / 区间非法 / 与本人已有委托区间重叠；
/// - `500`：数据库异常。
#[nik::permission(
    path = "/business/flow/delegate",
    method = "POST",
    permission = "business:flow:delegate:save"
)]
#[nik::post("/business/flow/delegate")]
pub async fn create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::DelegateCreateReq>,
) -> ResResult<dto::DelegateRes> {
    let me = me_of(&claims)?;
    let is_admin = claims.has_permission(delegate::PERM_ADMIN);

    let delegator_id = match req.delegator_id.as_deref() {
        None => me,
        Some(raw) if raw.trim().is_empty() => me,
        Some(raw) => {
            let target = parse_uuid_field(raw, "委托人 id")?;
            if target != me && !is_admin {
                return Err(forbidden("只能给自己配委托（替他人配置需要 business:flow:admin）"));
            }
            target
        }
    };
    let delegate_id = parse_uuid_field(&req.delegate_id, "受托人 id")?;
    let start_at = delegate::parse_time(&req.start_at, "委托开始时间")?;
    let end_at = delegate::parse_time(&req.end_at, "委托结束时间")?;
    let scope = delegate::normalize_scope(req.scope.as_deref())?;
    let reason = delegate::normalize_reason(req.reason.as_deref())?;
    validate_save(delegator_id, delegate_id, start_at, end_at).await?;

    let new = delegate::NewDelegate {
        id: Uuid::now_v7(),
        delegator_id,
        delegate_id,
        start_at,
        end_at,
        scope,
        reason,
        enabled: req.enabled.unwrap_or(true),
    };
    let id = new.id;
    // 事务内顺序：**串行锁 → 重叠检查 → 写入**。
    // 重叠冲突不在这里拼 ApiError（闭包只能返回 `DbResult`），而是把冲突区间带出来，
    // 出事务后再用 `check_overlap` 生成同一条 422/65043 —— 报文与单测逐字一致。
    let conflict = db()
        .transaction(|tx| async move {
            delegate::lock_delegator(&tx, delegator_id).await?;
            let conflict = delegate::overlap_of(&tx, delegator_id, start_at, end_at, None).await?;
            if conflict.is_some() {
                // 有冲突就不写（空事务提交，锁随事务释放）
                return Ok(conflict);
            }
            delegate::create(&tx, &new).await?;
            Ok(conflict)
        })
        .await
        .map_err(|error| utils::db_error(error, "新建审批委托"))?;
    delegate::check_overlap(conflict)?;

    let row = delegate::find(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "回读新建的审批委托"))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::RESOURCE_NOT_FOUND,
                "委托已写入但回读不到，请刷新列表确认",
            )
        })?;
    tracing::info!(
        delegate_id = %id,
        delegator = %delegator_id,
        delegate = %delegate_id,
        scope = row.scope.as_deref().unwrap_or("ALL"),
        "审批委托已创建"
    );
    Ok(Res::ok(to_res(&row)))
}

// ==================== 3. 修改 / 停用 ====================

/// 修改或停用委托（`PUT /business/flow/delegate/{id}`）。
///
/// 全量覆盖式；`delegator_id` 不可改（委托人是这行的归属，改归属 = 换个人）。
/// 停用 = `enabled=false`（保留配置与历史；要腾出时间给别的委托请改区间或删除）。
///
/// # 异常情况
///
/// - `400 + 65005`：字段非法 / 受托人不可用；
/// - `403 + 65004`：改别人的委托且没有 `business:flow:admin`；
/// - `404 + 10002`：委托不存在 / 已删除；
/// - `422 + 65043`：区间非法 / 自委托 / 与本人其它委托重叠；
/// - `500`：数据库异常。
#[nik::permission(
    path = "/business/flow/delegate/{id}",
    method = "PUT",
    permission = "business:flow:delegate:save"
)]
#[nik::put("/business/flow/delegate/{id}")]
pub async fn update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::DelegateUpdateReq>,
) -> ResResult<dto::DelegateRes> {
    let me = me_of(&claims)?;
    let is_admin = claims.has_permission(delegate::PERM_ADMIN);
    let delegate_id_row = parse_uuid_field(&id, "委托 id")?;
    let row = delegate::find(db(), delegate_id_row)
        .await
        .map_err(|error| utils::db_error(error, "查询审批委托"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "委托不存在或已删除"))?;
    if row.delegator_id != me && !is_admin {
        return Err(forbidden("只能修改自己的委托（修改他人委托需要 business:flow:admin）"));
    }

    let delegate_id = parse_uuid_field(&req.delegate_id, "受托人 id")?;
    let start_at = delegate::parse_time(&req.start_at, "委托开始时间")?;
    let end_at = delegate::parse_time(&req.end_at, "委托结束时间")?;
    let scope = delegate::normalize_scope(req.scope.as_deref())?;
    let reason = delegate::normalize_reason(req.reason.as_deref())?;
    validate_save(row.delegator_id, delegate_id, start_at, end_at).await?;

    let patch = delegate::DelegatePatch {
        delegate_id,
        start_at,
        end_at,
        scope,
        reason,
        enabled: req.enabled.unwrap_or(row.enabled),
    };
    let delegator_id = row.delegator_id;
    let conflict = db()
        .transaction(|tx| async move {
            delegate::lock_delegator(&tx, delegator_id).await?;
            // 排除自己这一行 —— 否则「什么都不改」的保存会被自己判成重叠
            let conflict = delegate::overlap_of(&tx, delegator_id, start_at, end_at, Some(delegate_id_row)).await?;
            if conflict.is_some() {
                return Ok(conflict);
            }
            delegate::update(&tx, delegate_id_row, &patch).await?;
            Ok(conflict)
        })
        .await
        .map_err(|error| utils::db_error(error, "修改审批委托"))?;
    delegate::check_overlap(conflict)?;

    let row = delegate::find(db(), delegate_id_row)
        .await
        .map_err(|error| utils::db_error(error, "回读审批委托"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "委托不存在或已删除"))?;
    tracing::info!(
        delegate_id = %delegate_id_row,
        enabled = row.enabled,
        delegate = %row.delegate_id,
        "审批委托已更新"
    );
    Ok(Res::ok(to_res(&row)))
}

// ==================== 4. 软删 ====================

/// 软删委托（`DELETE /business/flow/delegate/{id}`）。
///
/// 软删而不是硬删：审计要能回答「A 曾经把票委托给谁、到哪天」。
/// 已按这条委托派出去的待办**不受影响**（解析期已冻结成行）——
/// 删除只影响之后新发起的单据。
///
/// # 异常情况
///
/// - `400 + 65005`：id 非法；
/// - `403 + 65004`：删别人的委托且没有 `business:flow:admin`；
/// - `404 + 10002`：委托不存在 / 已删除；
/// - `500`：数据库异常。
#[nik::permission(
    path = "/business/flow/delegate/{id}",
    method = "DELETE",
    permission = "business:flow:delegate:delete"
)]
#[nik::delete("/business/flow/delegate/{id}")]
pub async fn remove(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::DelegateRes> {
    let me = me_of(&claims)?;
    let is_admin = claims.has_permission(delegate::PERM_ADMIN);
    let row_id = parse_uuid_field(&id, "委托 id")?;
    let row = delegate::find(db(), row_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批委托"))?
        .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "委托不存在或已删除"))?;
    if row.delegator_id != me && !is_admin {
        return Err(forbidden("只能删除自己的委托（删除他人委托需要 business:flow:admin）"));
    }
    delegate::soft_delete(db(), row_id)
        .await
        .map_err(|error| utils::db_error(error, "删除审批委托"))?;
    tracing::info!(delegate_id = %row_id, delegator = %row.delegator_id, "审批委托已软删");
    Ok(Res::ok(to_res(&row)))
}

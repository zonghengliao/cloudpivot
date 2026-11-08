//! 角色互斥规则的 HTTP 接口（批次 B · B7）。
//!
//! # 接口一览（前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/mutex/...`）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/mutex/page` | `system:role-mutex:list` | 规则分页（按角色 / 关键词 / 状态） |
//! | POST | `/business/mutex` | `system:role-mutex:create` | 新建（对称落库 + 反自反 + 存量冲突阻断） |
//! | PUT | `/business/mutex/{id}` | `system:role-mutex:create` | 改理由 / 启停（启用时同样阻断存量冲突） |
//! | DELETE | `/business/mutex/{id}` | `system:role-mutex:delete` | 停用（软删；放松约束，不查存量） |
//! | GET | `/business/mutex/conflict` | `system:role-mutex:conflict` | 单用户互斥体检 |
//! | POST | `/business/mutex/scan` | `system:role-mutex:conflict` | 全量存量违规扫描 |
//!
//! 四个权限码与 `V0065__seed_batchb_permissions` 的 `sys_menu.auth_code` **逐字一致**
//! （不一致 = 接口 403，批次 A 的 `V0045` 头注释记过同一个坑）。本卡不新增第 13 个码：
//! `PUT` 复用 `:create`（同为写权限）。
//!
//! # 校验点 3 的两种表现
//!
//! - 默认：`POST` / `PUT ?enabled=true` 发现存量双持 → `409 + 61010`，**不落库**，
//!   消息里给用户数与示例用户名（架构 §4.3 的产品决策：阻止配置，不做「允许但告警」）；
//! - `?force=true`：显式跳过阻断（运维已确认要接受历史违规），落库 + `WARN` 日志 + 审计
//!   里记 `force=true`。给口子是有意的：不给口子，运维只能绕过 API 直接写库（更糟）。
//!
//! # 删除 / 停用为什么不需要校验
//!
//! 删除互斥规则是**放松**约束，不存在「删完才违规」的可能（架构 §4.2 校验点 3 的第 3 行）。
//! 唯一的反向风险是「删错了」，那是审计与可恢复性的问题（软删 + 审计留痕），不是校验问题。

use axum::Json;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::net::SocketAddr;

use crate::business::mutex::checker;
use crate::business::mutex::dto::{
    MutexConflictQuery, MutexConflictRes, MutexCreateReq, MutexForceQuery, MutexHolderRes,
    MutexPageQuery, MutexRes, MutexScanItemRes, MutexScanRes, MutexUpdateReq,
};
use crate::business::mutex::guard;
use crate::business::mutex::model::{self, MutexRow, RoleBrief};
use crate::shared::error_code;
use crate::support::log;
use crate::utils;

// ==================== 分页 ====================

/// 单条出参补创建人姓名（**AUD-02**）：写接口回读时也带 `create_by_name`。
///
/// 失败只降级为空（前端显示 `—`），不因为一句人名把写操作打成 500。
async fn filled(mut res: MutexRes) -> MutexRes {
    crate::business::shared::user_name::fill_person_names(std::slice::from_mut(&mut res))
        .await
        .unwrap_or_default();
    res
}

/// 互斥规则分页。
///
/// # 参数
///
/// - `query`：页码 / 每页条数 / 角色 ID / 关键词 / 状态。
///
/// # 返回值
///
/// `{list, total, page, page_size}`；`list` 每项的 `role_a` / `role_b` 已按 `role_code`
/// 字典序规范化。
///
/// # 异常情况
///
/// - 400 + `10001`：`role_id` 不是合法 UUID / `status` 不是 0、1；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/mutex/page",
    method = "GET",
    permission = "system:role-mutex:list"
)]
#[nik::get("/business/mutex/page")]
pub async fn page(Query(query): Query<MutexPageQuery>) -> ResResult<PageData<Vec<MutexRes>>> {
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let role_id = match utils::trimmed(query.role_id.as_deref()) {
        Some(raw) => Some(utils::require_uuid(raw, "role_id")?),
        None => None,
    };
    let status = match query.status {
        Some(value @ (0 | 1)) => Some(value),
        Some(_) => {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                "status 只能为 0（停用）或 1（生效）",
            ));
        }
        None => None,
    };
    let keyword = utils::like_pattern(query.keyword.as_deref());
    let (rows, total) =
        model::page_mutexes(role_id, keyword.as_deref(), status, page_no, page_size)
            .await
            .map_err(|error| utils::db_error(error, "分页查询互斥规则"))?;
    let mut list: Vec<MutexRes> = rows.iter().map(MutexRes::from_row).collect();
    // AUD-02：创建人姓名，整页一次
    crate::business::shared::user_name::fill_person_names(&mut list)
        .await
        .unwrap_or_default();
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page_no,
        page_size,
    ))
}

// ==================== 新建 ====================

/// 新建互斥规则（`A 互斥 B` 与 `B 互斥 A` 是同一条，自动对称落库）。
///
/// # 参数
///
/// - `headers` / `connect_info` / `claims`：审计字段来源；
/// - `force_query`：`?force=true` 跳过存量双持阻断（默认 false）；
/// - `req`：`{ role_a_id, role_b_id, reason }`，`role_a_id == role_b_id` 被拒。
///
/// # 返回值
///
/// 建好的规则（`role_a` / `role_b` 已规范化）。
///
/// # 异常情况
///
/// - 400 + `61012`：角色与自身互斥；400 + `10001`：ID 格式非法 / 理由为空；
/// - 404 + `10002`：角色不存在或已删除；
/// - 409 + `61011`：该对的互斥关系已存在（**含反向**，不是 500）；
/// - 409 + `61010`：已有用户同时持有这两个角色（消息含用户数与示例用户名）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/mutex",
    method = "POST",
    permission = "system:role-mutex:create"
)]
#[nik::post("/business/mutex")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(force_query): Query<MutexForceQuery>,
    Json(req): Json<MutexCreateReq>,
) -> ResResult<MutexRes> {
    let role_a_id = utils::require_uuid(&req.role_a_id, "role_a_id")?;
    let role_b_id = utils::require_uuid(&req.role_b_id, "role_b_id")?;
    guard::ensure_not_self_pair(role_a_id, role_b_id)?;
    let role_a = require_role(&role_a_id).await?;
    let role_b = require_role(&role_b_id).await?;
    let reason = require_reason(&req.reason)?;

    // 重复（含反向）：先查再插，插的时候还有 `uk_sys_role_mutex_pair` 兜底（见 insert_error）
    let existing = model::find_active_pair(&role_a_id, &role_b_id)
        .await
        .map_err(|error| utils::db_error(error, "查询互斥关系是否存在"))?;
    if let Some(row) = existing {
        return Err(duplicate_error(&role_a, &role_b, &row));
    }

    let force = force_query.force.unwrap_or(false);
    if force {
        let (total, _) = checker::pair_holders(&role_a_id, &role_b_id)
            .await
            .map_err(|error| utils::db_error(error, "查询互斥对的双持用户"))?;
        tracing::warn!(
            role_a = %role_a_id, role_b = %role_b_id, holders = total,
            "force=true：跳过存量双持阻断（配置以存在违规为前提落库）"
        );
    } else {
        guard::ensure_pair_has_no_holders(
            &role_a_id,
            &role_b_id,
            &role_a.role_name,
            &role_b.role_name,
        )
        .await?;
    }

    let operator = current_user_id(&claims)?;
    let id = model::insert_mutex(&role_a_id, &role_b_id, &reason, Some(operator))
        .await
        .map_err(|error| insert_error(error, &role_a, &role_b))?;
    let row = require_mutex(&id).await?;
    tracing::info!(
        mutex_id = %id, role_a = %role_a.role_code, role_b = %role_b.role_code, force,
        operator = %operator, "互斥规则已建立"
    );
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ROLE_MUTEX_CREATE",
            "role_mutex",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "role_a": role_a.role_code,
                "role_b": role_b.role_code,
                "reason": reason,
                "force": force,
            })),
        ),
    );
    Ok(Res::ok(filled(MutexRes::from_row(&row)).await))
}

// ==================== 编辑（理由 / 启停） ====================

/// 编辑互斥规则：只能改**理由**与**启停**（换角色对请删除后重建）。
///
/// # 参数
///
/// - `id`：规则 ID（路径参数）；
/// - `force_query`：恢复启用时 `?force=true` 跳过存量双持阻断；
/// - `req`：`{ reason?, enabled? }`，至少给一个。
///
/// # 返回值
///
/// 修改后的规则。
///
/// # 异常情况
///
/// - 400 + `10001`：ID 非法 / 理由为空 / 两个字段都没给；
/// - 404 + `10002`：规则或它引用的角色不存在；
/// - 409 + `61011`：恢复启用时该对已有另一条生效规则；
/// - 409 + `61010`：恢复启用时已有用户双持；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/mutex/{id}",
    method = "PUT",
    permission = "system:role-mutex:create"
)]
#[nik::put("/business/mutex/{id}")]
pub async fn update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(force_query): Query<MutexForceQuery>,
    Json(req): Json<MutexUpdateReq>,
) -> ResResult<MutexRes> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = require_mutex(&id).await?;
    if req.reason.is_none() && req.enabled.is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "请至少提供一个要修改的字段（reason / enabled）",
        ));
    }
    let role_a = require_role(&existing.role_a_id).await?;
    let role_b = require_role(&existing.role_b_id).await?;
    let operator = current_user_id(&claims)?;

    let mut reason_after = existing.reason.clone();
    if let Some(raw) = req.reason.as_deref() {
        let reason = require_reason(raw)?;
        model::update_reason(&id, &reason)
            .await
            .map_err(|error| utils::db_error(error, "修改互斥理由"))?;
        reason_after = reason;
    }

    let mut enabled_after = existing.enabled;
    if let Some(enabled) = req.enabled {
        if enabled && !existing.enabled {
            // 恢复启用 = 重新收紧约束，与新建同口径：先查重复，再查存量
            let active = model::find_active_pair(&existing.role_a_id, &existing.role_b_id)
                .await
                .map_err(|error| utils::db_error(error, "查询互斥关系是否存在"))?;
            if let Some(row) = active {
                return Err(duplicate_error(&role_a, &role_b, &row));
            }
            if !force_query.force.unwrap_or(false) {
                guard::ensure_pair_has_no_holders(
                    &existing.role_a_id,
                    &existing.role_b_id,
                    &role_a.role_name,
                    &role_b.role_name,
                )
                .await?;
            }
            model::set_enabled(&id, true)
                .await
                .map_err(|error| utils::db_error(error, "启用互斥规则"))?;
        } else if !enabled && existing.enabled {
            model::set_enabled(&id, false)
                .await
                .map_err(|error| utils::db_error(error, "停用互斥规则"))?;
        }
        enabled_after = enabled;
    }

    let row = require_mutex(&id).await?;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ROLE_MUTEX_UPDATE",
            "role_mutex",
            id.to_string(),
            Some(serde_json::json!({
                "reason": existing.reason,
                "enabled": existing.enabled,
            })),
            Some(serde_json::json!({
                "reason": reason_after,
                "enabled": enabled_after,
            })),
        ),
    );
    tracing::info!(
        mutex_id = %id, enabled = enabled_after, operator = %operator,
        "互斥规则已更新"
    );
    Ok(Res::ok(filled(MutexRes::from_row(&row)).await))
}

// ==================== 删除（停用） ====================

/// 删除互斥规则（软删 = 停用）。放松约束，不查存量。
///
/// # 返回值
///
/// `{ "id": "...", "deleted": true }`。
///
/// # 异常情况
///
/// - 400 + `10001`：ID 格式非法；
/// - 404 + `10002`：规则不存在或已停用；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/mutex/{id}",
    method = "DELETE",
    permission = "system:role-mutex:delete"
)]
#[nik::delete("/business/mutex/{id}")]
pub async fn delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<serde_json::Value> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = require_mutex(&id).await?;
    if !existing.enabled {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("互斥关系不存在或已停用：{id}"),
        ));
    }
    let affected = model::set_enabled(&id, false)
        .await
        .map_err(|error| utils::db_error(error, "删除互斥规则"))?;
    if affected == 0 {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("互斥关系不存在或已停用：{id}"),
        ));
    }
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ROLE_MUTEX_DELETE",
            "role_mutex",
            id.to_string(),
            Some(serde_json::json!({
                "role_a": existing.role_a_code,
                "role_b": existing.role_b_code,
                "reason": existing.reason,
            })),
            None,
        ),
    );
    tracing::info!(mutex_id = %id, "互斥规则已删除（软删）");
    Ok(Res::ok(serde_json::json!({
        "id": id.to_string(),
        "deleted": true,
    })))
}

// ==================== 单用户体检 ====================

/// 查某用户当前是否已违反互斥（他持有的角色里有没有互斥对）。
///
/// # 参数
///
/// - `query.user_id`：目标用户 ID（必填）。
///
/// # 返回值
///
/// `{ user_id, username, display_name, conflicts: [...] }`；`conflicts` 为空数组 = 未违规。
///
/// # 异常情况
///
/// - 400 + `10001`：缺 `user_id` 或格式非法；
/// - 404 + `10002`：用户不存在或已删除；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/mutex/conflict",
    method = "GET",
    permission = "system:role-mutex:conflict"
)]
#[nik::get("/business/mutex/conflict")]
pub async fn conflict(Query(query): Query<MutexConflictQuery>) -> ResResult<MutexConflictRes> {
    let raw = utils::trimmed(query.user_id.as_deref()).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "请提供 user_id 参数（要体检的用户 ID）",
        )
    })?;
    let user_id = utils::require_uuid(raw, "user_id")?;
    let user = model::find_user_brief(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "查询用户"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("用户不存在或已删除：{user_id}"),
            )
        })?;
    let hits = checker::user_conflicts(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "体检用户互斥关系"))?;
    Ok(Res::ok(MutexConflictRes::new(&user, &hits)))
}

// ==================== 全量扫描 ====================

/// 全量存量违规扫描：逐条生效规则列出双持用户。
///
/// # 返回值
///
/// `{ scanned_pairs, violated_pairs, violated_holders, items: [{…, total, sample}] }`。
/// `sample` 每条最多 [`checker::SAMPLE_LIMIT`] 个用户，`total` 是真实总数。
///
/// # 异常情况
///
/// - 500：数据库异常（扫描是只读接口，不落库）。
#[nik::permission(
    path = "/business/mutex/scan",
    method = "POST",
    permission = "system:role-mutex:conflict"
)]
#[nik::post("/business/mutex/scan")]
pub async fn scan() -> ResResult<MutexScanRes> {
    let result = checker::scan()
        .await
        .map_err(|error| utils::db_error(error, "全量扫描互斥违规"))?;
    let (violated_pairs, violated_holders) = checker::summarize(&result.items);
    let items: Vec<MutexScanItemRes> = result
        .items
        .iter()
        .map(|group| MutexScanItemRes {
            mutex_id: group.rule.id.to_string(),
            reason: group.rule.reason.clone(),
            role_a_code: group.rule.role_a_code.clone(),
            role_a_name: group.rule.role_a_name.clone(),
            role_b_code: group.rule.role_b_code.clone(),
            role_b_name: group.rule.role_b_name.clone(),
            total: u64::try_from(group.total).unwrap_or(0),
            sample: group.sample.iter().map(MutexHolderRes::from_row).collect(),
        })
        .collect();
    tracing::info!(
        scanned_pairs = result.scanned_pairs,
        violated_pairs,
        violated_holders,
        "互斥存量扫描完成"
    );
    Ok(Res::ok(MutexScanRes {
        scanned_pairs: u64::try_from(result.scanned_pairs).unwrap_or(0),
        violated_pairs,
        violated_holders,
        items,
    }))
}

// ==================== 内部工具 ====================

/// 取当前操作者 ID。
fn current_user_id(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "当前令牌缺少有效的用户标识，请重新登录",
        )
    })
}

/// 校验并返回角色（不存在 / 已软删 → 404）。
async fn require_role(role_id: &Uuid) -> std::result::Result<RoleBrief, ApiError> {
    let role = model::find_role(role_id)
        .await
        .map_err(|error| utils::db_error(error, "查询角色"))?
        .filter(|role| !role.deleted)
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("角色不存在或已删除：{role_id}"),
            )
        })?;
    Ok(role)
}

/// 取规则（不存在 → 404）。
async fn require_mutex(id: &Uuid) -> std::result::Result<MutexRow, ApiError> {
    model::find_mutex(id)
        .await
        .map_err(|error| utils::db_error(error, "查询互斥规则"))?
        .ok_or_else(|| {
            ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!("互斥关系不存在：{id}"),
            )
        })
}

/// 校验互斥理由（必填，最长 255 与列宽一致）。
fn require_reason(raw: &str) -> std::result::Result<String, ApiError> {
    let reason = raw.trim();
    if reason.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "请填写互斥理由（用于合规留痕）。",
        ));
    }
    if reason.chars().count() > 255 {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "互斥理由不能超过 255 个字符",
        ));
    }
    Ok(reason.to_string())
}

/// 互斥关系已存在（含反向）。消息里点名两个角色，并说明「无方向」。
fn duplicate_error(role_a: &RoleBrief, role_b: &RoleBrief, row: &MutexRow) -> ApiError {
    ApiError::new(
        409,
        error_code::ROLE_MUTEX_DUPLICATE,
        format!(
            "「{}」与「{}」的互斥关系已存在（互斥关系无方向，A 与 B 视为同一条；\
             现有规则 {} 于 {}）。",
            role_a.role_name,
            role_b.role_name,
            row.id,
            row.created_at.format("%Y-%m-%d %H:%M:%S")
        ),
    )
}

/// 把插入期的数据库错误翻译成对外错误。
///
/// # 为什么不能只靠「先查再插」
///
/// 两个管理员同时提交同一对规则时，「先查」都返回「不存在」，第二次插入由
/// `uk_sys_role_mutex_pair` 拒绝。此时若把 `UniqueViolation` 当 500 报，前端只能显示
/// 「服务器内部错误」，运维会以为是 bug。这里翻译成与「先查命中」完全一致的 409。
fn insert_error(error: DbError, role_a: &RoleBrief, role_b: &RoleBrief) -> ApiError {
    if let DbError::UniqueViolation { .. } = &error {
        tracing::warn!(
            role_a = %role_a.role_code, role_b = %role_b.role_code,
            "并发插入同一互斥对被唯一索引拒绝，翻译为 409"
        );
        return ApiError::new(
            409,
            error_code::ROLE_MUTEX_DUPLICATE,
            format!(
                "「{}」与「{}」的互斥关系已存在（互斥关系无方向，A 与 B 视为同一条）。",
                role_a.role_name, role_b.role_name
            ),
        );
    }
    utils::db_error(error, "新建互斥规则")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(name: &str) -> RoleBrief {
        RoleBrief {
            id: Uuid::from_u128(1),
            role_code: name.to_ascii_uppercase(),
            role_name: name.to_string(),
            status: 1,
            deleted: false,
        }
    }

    /// 理由：空白 / 全空格被拒，两侧空格被 trim。
    #[test]
    fn reason_is_trimmed_and_blank_rejected() {
        assert_eq!(
            require_reason("  合规要求  ").expect("有效理由"),
            "合规要求"
        );
        let error = require_reason("   ").expect_err("空理由必须被拒");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert_eq!(error.msg, "请填写互斥理由（用于合规留痕）。");
    }

    /// 理由：255 个字符通过，256 个被拒（与列宽 `VARCHAR(255)` 对齐，避免库侧 500）。
    #[test]
    fn reason_length_boundary() {
        let exactly = "字".repeat(255);
        assert!(require_reason(&exactly).is_ok());
        let too_long = "字".repeat(256);
        let error = require_reason(&too_long).expect_err("超长必须被拒");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("255"));
    }

    /// 重复规则文案：必须点名两个角色 + 说明「无方向」+ 带现有规则 ID。
    #[test]
    fn duplicate_error_mentions_both_roles_and_directionless() {
        let row = MutexRow {
            id: Uuid::from_u128(0xAB),
            role_a_id: Uuid::from_u128(1),
            role_a_code: "A".to_string(),
            role_a_name: "采购员".to_string(),
            role_b_id: Uuid::from_u128(2),
            role_b_code: "B".to_string(),
            role_b_name: "付款审批".to_string(),
            reason: "合规".to_string(),
            enabled: true,
            created_at: chrono::Utc::now(),
            create_by: None,
        };
        let error = duplicate_error(&role("采购员"), &role("付款审批"), &row);
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ROLE_MUTEX_DUPLICATE);
        assert!(error.msg.contains("「采购员」与「付款审批」"));
        assert!(error.msg.contains("互斥关系无方向"));
        assert!(error.msg.contains(&row.id.to_string()));
    }

    /// 并发插入被唯一索引拒绝：必须翻译成 409 而不是 500。
    #[test]
    fn unique_violation_maps_to_conflict_not_500() {
        let error = insert_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_sys_role_mutex_pair".to_string()),
                source: None,
            },
            &role("采购员"),
            &role("付款审批"),
        );
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ROLE_MUTEX_DUPLICATE);
        assert!(error.msg.contains("「采购员」与「付款审批」"));
    }
}

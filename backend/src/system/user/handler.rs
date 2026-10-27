//! 用户管理处理器：分页、详情、增删改、启停、重置密码与角色分配。
//!
//! 处理器只做**参数校验与响应组装**：SQL、行映射与关联表读写全部下沉到
//! [`crate::system::user::model`]；日志一律通过 [`crate::support::log`] 的
//! `record_*` 函数以**非阻塞**方式投递。
//!
//! 文件顺序遵循项目约定：**路由方法在前，公共辅助函数在后**。

use alun::prelude::*;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::shared::constants::EnableStatus;
use crate::shared::{constants, error_code};
use crate::support::log::{self, PasswordChangeLogInput};
use crate::system::dept::model::SysDeptModel;
use crate::system::role::model::SysRoleModel;
use crate::system::user::dto::{
    RoleBriefRes, UserBatchDeptReq, UserCreateReq, UserDetailRes, UserPageQuery, UserPasswordReq,
    UserRes, UserRoleAssignReq, UserStatusReq, UserUpdateReq,
};
use crate::system::user::guard;
use crate::system::user::model::{self, SysUserModel, SysUserRoleModel};
use crate::utils;

// ==================== 接口 ====================

/// 用户分页查询（支持用户名 / 姓名模糊、状态与部门过滤）。
///
/// # 参数
///
/// - `query`：分页与过滤条件。
///
/// # 返回值
///
/// 成功返回 `{list, total, page, page_size}`，`list` 元素附带部门名。
///
/// # 异常情况
///
/// - 400：分页或部门 ID 参数非法；
/// - 500：数据库异常。
#[alun::permission(
    path = "/system/user/page",
    method = "GET",
    permission = "system:user:list"
)]
#[alun::get("/system/user/page")]
pub async fn page(Query(query): Query<UserPageQuery>) -> ResResult<PageData<Vec<UserRes>>> {
    // 部门过滤：include_children 时展开为整棵部门子树，否则仅本部门
    let dept_ids = match utils::parse_optional_uuid(query.dept_id.as_deref(), "dept_id")? {
        Some(dept_id) => {
            if query.include_children.unwrap_or(false) {
                Some(
                    crate::system::dept::model::find_dept_subtree_ids(&dept_id)
                        .await
                        .map_err(|e| utils::db_error(e, "查询部门子树"))?,
                )
            } else {
                Some(vec![dept_id])
            }
        }
        None => None,
    };

    let filter = model::UserPageFilter {
        username: query.username,
        real_name: query.real_name,
        email: query.email,
        mobile: query.mobile,
        status: query.status,
        dept_ids,
        create_begin: query.create_begin,
        create_end: query.create_end,
    };
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let page_data = model::page_users(&filter, page, page_size)
        .await
        .map_err(|e| utils::db_error(e, "分页查询用户"))?;

    // 单次 IN 查询批量取回本页用户的角色，按 user_id 聚合，避免 N+1
    let page_user_ids: Vec<uuid::Uuid> = page_data.list.iter().map(|i| i.user.id).collect();
    let role_map = role_map_of(&page_user_ids).await?;

    let mut list: Vec<UserRes> = page_data
        .list
        .iter()
        .map(|item| UserRes::from_model(&item.user, item.dept_name.clone()))
        .collect();
    for item in &mut list {
        if let Some(id) = utils::parse_uuid(&item.id)
            && let Some(roles) = role_map.get(&id)
        {
            item.roles = roles.clone();
        }
    }
    Ok(Res::page(
        list,
        page_data.total,
        page_data.page,
        page_data.size,
    ))
}

/// 用户详情。
///
/// # 参数
///
/// - `id`：用户 ID。
///
/// # 返回值
///
/// 成功返回用户详情（含部门名与备注）。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：用户不存在；
/// - 500：数据库异常。
#[alun::permission(
    path = "/system/user/{id}",
    method = "GET",
    permission = "system:user:list"
)]
#[alun::get("/system/user/{id}")]
pub async fn detail(Path(id): Path<String>) -> ResResult<UserDetailRes> {
    let id = utils::require_uuid(&id, "id")?;
    let user = load_user(&id).await?;
    let dept_name = dept_name_of(user.dept_id).await?;
    let mut res = UserDetailRes::from_model(&user, dept_name);
    let role_map = role_map_of(std::slice::from_ref(&id)).await?;
    if let Some(roles) = role_map.get(&id) {
        res.roles = roles.clone();
    }
    Ok(Res::ok(res))
}

/// 新建用户。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（写入审计字段）。
/// - `req`：用户请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`USER_CREATE`）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回新建用户列表项。
///
/// # 异常情况
///
/// - 400：参数非法；
/// - 403：无权授予超级管理员角色（权限上限，见 [`guard`]）；
/// - 409：用户名已存在；
/// - 500：密码加密或数据库异常。
#[alun::permission(
    path = "/system/user",
    method = "POST",
    permission = "system:user:create"
)]
#[alun::post("/system/user")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<UserCreateReq>,
) -> ResResult<UserRes> {
    let username = req.username.trim().to_string();
    ensure_username_unique(&username, None).await?;

    // 先解析并校验角色全部存在，避免事务内才失败
    let role_ids = parse_and_check_roles(&req.role_ids).await?;
    // 权限上限：非超管不得在新建账号时携带 SUPER_ADMIN 角色（堵「建号即提权」）
    guard::ensure_role_grant_allowed(&claims, &role_ids, None).await?;

    let status = EnableStatus::from_optional_i16(req.status)?;
    let dept_id = utils::parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;
    let password = Crypto::hash_password(&req.password)
        .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "密码加密失败"))?;

    let now = chrono::Utc::now();
    let operator = utils::current_user_uuid(&claims);
    let new_user = SysUserModel {
        id: uuid::Uuid::now_v7(),
        username,
        password,
        real_name: req.real_name.map(|v| v.trim().to_string()),
        email: req.email.map(|v| v.trim().to_string()),
        mobile: req.mobile.map(|v| v.trim().to_string()),
        dept_id,
        status,
        // 新建账号：登录失败计数清零、未锁定、无登录时间
        login_fail_count: 0,
        is_locked: false,
        lock_time: None,
        last_login_at: None,
        remark: req.remark.map(|v| v.trim().to_string()),
        // 管理员代建的账号不强制改密（口令由建号人安全渠道告知）；
        // 只有「种子管理员」在初始口令来自 env / 随机生成时才置 true（见 bootstrap.rs）
        must_change_password: false,
        token_version: 0,
        created_at: now,
        updated_at: now,
        create_by: operator,
        update_by: operator,
        deleted_at: None,
    };

    // 事务：插入用户与角色绑定原子完成，任一失败整体回滚
    let tx_user = new_user.clone();
    db().transaction(|tx| async move {
        let mut u = tx_user;
        u.insert(&tx).await?;
        model::insert_user_roles(&tx, &u.id, &role_ids).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "新建用户"))?;

    let dept_name = dept_name_of(new_user.dept_id).await?;
    let mut res = UserRes::from_model(&new_user, dept_name);
    let role_map = role_map_of(std::slice::from_ref(&new_user.id)).await?;
    if let Some(roles) = role_map.get(&new_user.id) {
        res.roles = roles.clone();
    }
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "USER_CREATE",
            "User",
            new_user.id.to_string(),
            None,
            Some(serde_json::json!({
                "username": new_user.username,
                "real_name": new_user.real_name,
                "dept_id": new_user.dept_id.map(|v| v.to_string()),
                "status": new_user.status.as_i16(),
            })),
        ),
    );
    Ok(Res::ok(res))
}

/// 编辑用户（全量业务字段，用户名与密码不在此接口修改）。
///
/// # 参数
///
/// - `id`：用户 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（写入 `update_by`）。
/// - `req`：用户请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`USER_UPDATE`，含变更前 / 后快照）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回更新后的用户详情。
///
/// # 异常情况
///
/// - 400：ID 或状态参数非法；
/// - 403：无权操作超级管理员账号（权限上限，见 [`guard`]）；
/// - 404：用户不存在；
/// - 500：数据库异常。
#[alun::permission(
    path = "/system/user/{id}",
    method = "PUT",
    permission = "system:user:update"
)]
#[alun::put("/system/user/{id}")]
pub async fn update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<UserUpdateReq>,
) -> ResResult<UserDetailRes> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = load_user(&id).await?;
    // 权限上限：非超管不得修改超管账号的资料 / 状态（堵「改超管资料」）
    guard::ensure_role_grant_allowed(&claims, &[], Some(id)).await?;

    // 基于原记录派生，天然保留 created_at / create_by（仿 menu 的 update 写法）
    let mut to_save = existing.clone();
    to_save.real_name = req.real_name.map(|v| v.trim().to_string());
    to_save.email = req.email.map(|v| v.trim().to_string());
    to_save.mobile = req.mobile.map(|v| v.trim().to_string());
    to_save.dept_id = utils::parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;
    to_save.status = match req.status {
        Some(value) => EnableStatus::from_i16(value)?,
        None => to_save.status,
    };
    // 与 `change_status()` 的保护保持一致：不能把自己停用（否则当即自锁）
    if to_save.status != EnableStatus::enabled && utils::current_user_uuid(&claims) == Some(id) {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "不能停用当前登录账号",
        ));
    }
    to_save.remark = req.remark.map(|v| v.trim().to_string());
    to_save.updated_at = chrono::Utc::now();
    to_save.update_by = utils::current_user_uuid(&claims);

    to_save
        .update(db())
        .await
        .map_err(|e| utils::db_error(e, "更新用户"))?;

    // 状态由启用改为停用时，旧访问令牌在自然过期前仍然有效（认证中间件只验
    // JWT 与黑名单、不查状态），需失效主体缓存并强制下线，与 change_status
    // 专用接口的行为保持一致；已是停用态则无需重复踢号。
    if to_save.status != EnableStatus::enabled && existing.status == EnableStatus::enabled {
        // 令牌版本号自增：被停用账号已签发的令牌（含 refresh）整体作废，
        // 不依赖「在线会话表里恰好还有记录」（安全审计 CODE-05 / CODE-08）
        revoke_user_tokens(&id, "update_disable").await;
        crate::system::auth::model::invalidate_user_principal(&id).await;
        crate::support::online::model::force_logout_user(&id.to_string()).await;
    }

    let dept_name = dept_name_of(to_save.dept_id).await?;
    let mut res = UserDetailRes::from_model(&to_save, dept_name);
    let role_map = role_map_of(std::slice::from_ref(&id)).await?;
    if let Some(roles) = role_map.get(&id) {
        res.roles = roles.clone();
    }
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "USER_UPDATE",
            "User",
            id.to_string(),
            Some(serde_json::json!({
                "real_name": existing.real_name,
                "dept_id": existing.dept_id.map(|v| v.to_string()),
                "status": existing.status.as_i16(),
            })),
            Some(serde_json::json!({
                "real_name": to_save.real_name,
                "dept_id": to_save.dept_id.map(|v| v.to_string()),
                "status": to_save.status.as_i16(),
            })),
        ),
    );
    Ok(Res::ok(res))
}

/// 删除用户（同时物理清理其角色绑定；内置管理员账号不可删除）。
///
/// # 参数
///
/// - `id`：用户 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（审计操作者）。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`USER_DELETE`）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 403：无权操作超级管理员账号（权限上限，见 [`guard`]）；
/// - 404：用户不存在；
/// - 409：目标为内置管理员账号；
/// - 500：数据库异常。
#[alun::permission(
    path = "/system/user/{id}",
    method = "DELETE",
    permission = "system:user:delete"
)]
#[alun::delete("/system/user/{id}")]
pub async fn delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let user = load_user(&id).await?;
    // 权限上限：非超管不得删除超管账号（超管删除内置 admin 仍由下方 409 拦截）
    guard::ensure_role_grant_allowed(&claims, &[], Some(id)).await?;
    if user.username == constants::ADMIN_USERNAME {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "内置管理员账号不可删除",
        ));
    }

    // 事务：软删用户 + 物理清理角色绑定，避免用户已删而授权残留
    db().transaction(|tx| async move {
        SysUserModel::of(id).delete(&tx).await?;
        // 关联表为硬删表：物理删除该用户的全部角色绑定
        SysUserRoleModel::delete_by(&tx, "user_id", (&id,)).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "删除用户"))?;

    // 用户删除后其令牌应立即失效（JWT 无状态，需主动拉黑其在线会话）
    crate::system::auth::model::invalidate_user_principal(&id).await;
    crate::support::online::model::force_logout_user(&id.to_string()).await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "USER_DELETE",
            "User",
            id.to_string(),
            Some(serde_json::json!({
                "username": user.username,
                "dept_id": user.dept_id.map(|v| v.to_string()),
            })),
            None,
        ),
    );
    Ok(Res::ok_empty())
}

/// 启用 / 停用用户。
///
/// # 参数
///
/// - `id`：用户 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（保护规则判定与审计字段）。
/// - `req`：状态请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`USER_STATUS`，含变更前 / 后状态）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 或状态取值非法；
/// - 403：无权操作超级管理员账号（权限上限，见 [`guard`]）；
/// - 404：用户不存在；
/// - 409：试图停用内置管理员或当前登录账号；
/// - 500：数据库异常。
///
/// # 权限失效
///
/// 停用用户会强制下线其全部在线令牌（JWT 权限在签发时固化，需主动拉黑），
/// 避免被停用账号仍可凭旧令牌访问。
#[alun::permission(
    path = "/system/user/{id}/status",
    method = "PATCH",
    permission = "system:user:status"
)]
#[alun::patch("/system/user/{id}/status")]
pub async fn change_status(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<UserStatusReq>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let user = load_user(&id).await?;
    // 权限上限：非超管不得启停超管账号（堵「禁用超管」）
    guard::ensure_role_grant_allowed(&claims, &[], Some(id)).await?;
    let status = EnableStatus::from_i16(req.status)?;

    // 停用场景的保护：内置管理员不可停用，也不能停用当前登录的自己
    if status != EnableStatus::enabled {
        if user.username == constants::ADMIN_USERNAME {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "内置管理员账号不可停用",
            ));
        }
        if utils::current_user_uuid(&claims) == Some(id) {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                "不能停用当前登录账号",
            ));
        }
    }

    let operator = utils::current_user_uuid(&claims);
    SysUserModel::of(id)
        .set(SysUserModel::status(), status)
        // ChangeSet 不自动刷新 updated_at / update_by，需显式设置
        .set(SysUserModel::updated_at(), chrono::Utc::now())
        .set(SysUserModel::update_by(), operator)
        .update(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "修改用户状态"))?;

    // 停用即强制下线，使旧令牌在下次请求时被黑名单拦截
    if status != EnableStatus::enabled {
        // 令牌版本号自增：该账号已签发的全部令牌（access + refresh）整体作废
        // （安全审计 CODE-05 / CODE-08）
        revoke_user_tokens(&id, "change_status_disable").await;
        crate::system::auth::model::invalidate_user_principal(&id).await;
        crate::support::online::model::force_logout_user(&id.to_string()).await;
    }
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "USER_STATUS",
            "User",
            id.to_string(),
            Some(serde_json::json!({ "status": user.status.as_i16() })),
            Some(serde_json::json!({ "status": status.as_i16() })),
        ),
    );
    Ok(Res::ok_empty())
}

/// 重置用户密码。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：认证中间件注入的令牌声明（判定操作者）。
/// - `id`：用户 ID。
/// - `req`：密码请求体（明文，服务端哈希后存储）。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 格式非法或密码长度不合规；
/// - 403：无权操作超级管理员账号（权限上限，见 [`guard`]）；
/// - 404：用户不存在；
/// - 500：密码加密或数据库异常。
/// # 日志
///
/// 写入 `password_change_logs`（`RESET`，操作者为管理员或本人）与 `audit_logs`
/// （`USER_PASSWORD_RESET`）；写入失败不影响改密结果。
#[alun::permission(
    path = "/system/user/{id}/password",
    method = "PATCH",
    permission = "system:user:password"
)]
#[alun::patch("/system/user/{id}/password")]
pub async fn change_password(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    ValidatedJson(req): ValidatedJson<UserPasswordReq>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let user = load_user(&id).await?;
    // 权限上限：非超管不得改超管密码（堵「改超管密码即接管」）；本人改自己密码不受限
    guard::ensure_role_grant_allowed(&claims, &[], Some(id)).await?;
    let password = Crypto::hash_password(&req.password)
        .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "密码加密失败"))?;

    // 操作者：本人改密记 SELF，管理员代改记 ADMIN
    let operator_id = utils::current_user_uuid(&claims);
    let reset_by_admin = operator_id != Some(id);
    SysUserModel::of(id)
        .set(SysUserModel::password(), password)
        // 管理员代改的口令属于「一次性凭据」：目标账号下次登录必须自行改密
        // （安全审计 CODE-06）
        .set(SysUserModel::must_change_password(), reset_by_admin)
        // ChangeSet 不自动刷新 updated_at / update_by，需显式设置
        .set(SysUserModel::updated_at(), chrono::Utc::now())
        .set(SysUserModel::update_by(), operator_id)
        .update(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "重置用户密码"))?;

    // 旧令牌整体作废：重置口令后原持有者的 access / refresh 一律失效
    // （安全审计 CODE-05 / CODE-08）
    revoke_user_tokens(&id, "admin_reset_password").await;
    crate::system::auth::model::invalidate_user_principal(&id).await;

    log::record_password_change(
        &headers,
        &connect_info,
        PasswordChangeLogInput {
            user_id: id,
            username: user.username.clone(),
            change_type: "RESET".to_string(),
            operator_type: if operator_id == Some(id) {
                "SELF"
            } else {
                "ADMIN"
            }
            .to_string(),
            operator_id,
            change_status: "SUCCESS".to_string(),
            error_message: None,
        },
    );

    // 管理员重置他人密码后，强制其下线，须用新密码重新登录；本人改密不踢自己
    if operator_id != Some(id) {
        crate::support::online::model::force_logout_user(&id.to_string()).await;
    }

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "USER_PASSWORD_RESET",
            "User",
            id.to_string(),
            None,
            Some(serde_json::json!({ "username": user.username })),
        ),
    );
    Ok(Res::ok_empty())
}

/// 查询用户已分配的角色 id 列表。
///
/// # 参数
///
/// - `id`：用户 ID。
///
/// # 返回值
///
/// 成功返回角色 id 字符串列表。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：用户不存在；
/// - 500：数据库异常。
#[alun::permission(
    path = "/system/user/{id}/roles",
    method = "GET",
    permission = "system:user:list"
)]
#[alun::get("/system/user/{id}/roles")]
pub async fn roles(Path(id): Path<String>) -> ResResult<Vec<String>> {
    let id = utils::require_uuid(&id, "id")?;
    load_user(&id).await?;
    let bindings = SysUserRoleModel::find_by(db(), "user_id", (&id,))
        .await
        .map_err(|e| utils::db_error(e, "查询用户角色"))?;
    Ok(Res::ok(
        bindings
            .into_iter()
            .map(|b| b.role_id.to_string())
            .collect(),
    ))
}

/// 分配用户角色（事务内先清空全部绑定再重建；空数组表示清空）。
///
/// # 参数
///
/// - `id`：用户 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（审计操作者）。
/// - `req`：角色分配请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`USER_ASSIGN_ROLE`，含变更前 / 后角色 ID 列表）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 或角色 ID 格式非法；
/// - 403：无权操作超管账号 / 无权授予超管角色 / 无权自我提权（权限上限，见 [`guard`]）；
/// - 404：用户或某个角色不存在；
/// - 409：目标为内置管理员账号（角色固定，不可修改）；
/// - 500：数据库异常（事务回滚）。
///
/// # 权限失效
///
/// 角色变化直接改变用户权限，成功后强制下线该用户全部在线令牌，令其重新登录
/// 获取最新权限。
#[alun::permission(
    path = "/system/user/{id}/roles",
    method = "PUT",
    permission = "system:user:role"
)]
#[alun::put("/system/user/{id}/roles")]
pub async fn assign_roles(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<UserRoleAssignReq>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let user = load_user(&id).await?;
    if user.username == constants::ADMIN_USERNAME {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "内置管理员账号的角色不可修改",
        ));
    }

    // 校验所有角色均存在，再进入事务重建
    let role_ids = parse_and_check_roles(&req.role_ids).await?;
    // 权限上限：非超管不得授予/剥夺超管角色，也不得给自己新增未持有的角色
    guard::ensure_role_grant_allowed(&claims, &role_ids, Some(id)).await?;
    // 记录替换前的既有授权与替换后的目标授权，用于审计 before / after 快照
    let before_role_ids = SysUserRoleModel::find_by(db(), "user_id", (&id,))
        .await
        .map_err(|e| utils::db_error(e, "查询用户原有角色"))?
        .into_iter()
        .map(|b| b.role_id.to_string())
        .collect::<Vec<_>>();
    let after_role_ids = role_ids.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    db().transaction(|tx| async move {
        model::replace_user_roles(&tx, &id, &role_ids).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "分配用户角色"))?;

    // 权限变更立即生效：清理主体上下文缓存并强制下线该用户，旧令牌内固化的角色 /
    // 权限快照随之作废
    // 令牌版本号自增：即便在线会话记录已被清理，旧令牌也无法再用（CODE-08）
    revoke_user_tokens(&id, "assign_roles").await;
    crate::system::auth::model::invalidate_user_principal(&id).await;
    crate::support::online::model::force_logout_user(&id.to_string()).await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "USER_ASSIGN_ROLE",
            "User",
            id.to_string(),
            Some(serde_json::json!({ "role_ids": before_role_ids })),
            Some(serde_json::json!({ "role_ids": after_role_ids })),
        ),
    );
    Ok(Res::ok_empty())
}

/// 批量调整用户所属部门（单条 `UPDATE ... WHERE id IN (...)`，事务执行）。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（写入 `update_by`）。
/// - `req`：用户 ID 列表与目标部门 ID。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`USER_BATCH_DEPT`）；日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：用户 ID 或部门 ID 格式非法、未选择用户；
/// - 404：目标部门不存在；
/// - 500：数据库异常（事务回滚）。
#[alun::permission(
    path = "/system/user/batch-dept",
    method = "PUT",
    permission = "system:user:update"
)]
#[alun::put("/system/user/batch-dept")]
pub async fn batch_update_dept(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<UserBatchDeptReq>,
) -> ResResult<()> {
    let dept_id = utils::require_uuid(&req.dept_id, "dept_id")?;
    // 校验目标部门存在
    let dept_exists = SysDeptModel::find_or_none(db(), &dept_id)
        .await
        .map_err(|e| utils::db_error(e, "查询目标部门"))?
        .is_some();
    if !dept_exists {
        return Err(ApiError::new(
            404,
            error_code::DEPT_NOT_FOUND,
            "目标部门不存在",
        ));
    }

    let mut user_ids = Vec::with_capacity(req.user_ids.len());
    for raw in &req.user_ids {
        user_ids.push(utils::require_uuid(raw, "user_ids")?);
    }
    let operator = utils::current_user_uuid(&claims);
    // 审计快照用的用户 ID 字符串列表（user_ids 随后会移入事务闭包）
    let user_id_strs = user_ids.iter().map(|v| v.to_string()).collect::<Vec<_>>();

    db().transaction(|tx| async move {
        model::batch_update_dept(&tx, &user_ids, &dept_id, operator).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "批量调整用户部门"))?;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "USER_BATCH_DEPT",
            "User",
            user_id_strs.join(","),
            None,
            Some(serde_json::json!({
                "user_ids": user_id_strs,
                "dept_id": dept_id.to_string(),
            })),
        ),
    );
    Ok(Res::ok_empty())
}

// ==================== 公共辅助 ====================

/// 按主键加载用户，不存在时返回业务错误。
///
/// # 参数
///
/// - `id`：用户 ID。
///
/// # 返回值
///
/// 用户模型。
///
/// # 异常情况
///
/// - 404：用户不存在（`USER_NOT_FOUND`）；
/// - 500：数据库异常。
async fn load_user(id: &uuid::Uuid) -> std::result::Result<SysUserModel, ApiError> {
    SysUserModel::find_or_none(db(), id)
        .await
        .map_err(|e| utils::db_error(e, "查询用户"))?
        .ok_or_else(|| ApiError::new(404, error_code::USER_NOT_FOUND, "用户不存在"))
}

/// 令指定用户的全部已签发令牌失效（令牌版本号自增）。
///
/// 与 [`crate::support::online::model::force_logout_user`] 的分工：
///
/// - `force_logout_user` 只清理「还在在线会话表里」的连接，且依赖缓存里的黑名单；
///   会话已被清理（或换了实例/缓存重启）时，旧令牌仍可能通过；
/// - 令牌版本号写在库里，认证层逐请求比对，是**不依赖缓存**的兜底
///   （安全审计 CODE-05 / CODE-08）。
///
/// # 参数
///
/// - `user_id`：目标用户 ID。
/// - `context`：调用场景（写入日志便于排查）。
async fn revoke_user_tokens(user_id: &uuid::Uuid, context: &'static str) {
    if let Err(e) = model::bump_token_version(user_id).await {
        // 失败不回滚业务：强制下线与黑名单仍在，只是少了一层兜底
        tracing::error!(error = %e, user_id = %user_id, context, "自增令牌版本号失败");
    }
}

/// 校验用户名唯一（软删表上仅比较未删除行）。
///
/// # 参数
///
/// - `username`：待校验的用户名。
/// - `exclude`：更新场景需排除的自身 ID；新建场景传 `None`。
///
/// # 返回值
///
/// 用户名未被占用时返回 `Ok(())`。
///
/// # 异常情况
///
/// - 409：用户名已存在（`USERNAME_EXISTS`）；
/// - 500：数据库异常。
async fn ensure_username_unique(
    username: &str,
    exclude: Option<uuid::Uuid>,
) -> std::result::Result<(), ApiError> {
    let count = model::count_users_by_username(username, exclude)
        .await
        .map_err(|e| utils::db_error(e, "校验用户名唯一性"))?;
    utils::ensure_unique(count, error_code::USERNAME_EXISTS, "用户名已存在")
}

/// 查询部门名称（无部门或部门已删除时为空串）。
///
/// # 参数
///
/// - `dept_id`：部门 ID，可为空。
///
/// # 返回值
///
/// 部门名称；`dept_id` 为空时返回空串。
///
/// # 异常情况
///
/// 数据库异常返回 500。
async fn dept_name_of(dept_id: Option<uuid::Uuid>) -> std::result::Result<String, ApiError> {
    match dept_id {
        Some(id) => Ok(SysDeptModel::find_or_none(db(), &id)
            .await
            .map_err(|e| utils::db_error(e, "查询用户所属部门"))?
            .map(|dept| dept.dept_name)
            .unwrap_or_default()),
        None => Ok(String::new()),
    }
}

/// 解析角色 ID 字符串列表并校验每个角色均存在。
///
/// # 参数
///
/// - `raw_ids`：角色 ID 字符串列表（可空，空列表返回空 `Vec`）。
///
/// # 返回值
///
/// 解析并校验通过的角色 UUID 列表（保持入参顺序、不去重）。
///
/// # 异常情况
///
/// - 400：角色 ID 格式非法；
/// - 404：某个角色不存在或已删除（`ROLE_NOT_FOUND`）；
/// - 500：数据库异常。
async fn parse_and_check_roles(
    raw_ids: &[String],
) -> std::result::Result<Vec<uuid::Uuid>, ApiError> {
    let mut role_ids = Vec::with_capacity(raw_ids.len());
    for raw in raw_ids {
        let role_id = utils::require_uuid(raw, "role_ids")?;
        let exists = SysRoleModel::find_or_none(db(), &role_id)
            .await
            .map_err(|e| utils::db_error(e, "校验角色"))?
            .is_some();
        if !exists {
            return Err(ApiError::new(
                404,
                error_code::ROLE_NOT_FOUND,
                "角色不存在或已删除",
            ));
        }
        role_ids.push(role_id);
    }
    Ok(role_ids)
}

/// 批量查询用户角色并按用户 ID 聚合为响应结构（供列表 / 详情填充 `roles`）。
///
/// # 参数
///
/// - `user_ids`：用户 ID 列表；空切片返回空映射。
///
/// # 返回值
///
/// `user_id -> 角色简要列表` 的映射。
///
/// # 异常情况
///
/// 数据库异常返回 500。
async fn role_map_of(
    user_ids: &[uuid::Uuid],
) -> std::result::Result<HashMap<uuid::Uuid, Vec<RoleBriefRes>>, ApiError> {
    let rows = model::find_user_roles_for_users(user_ids)
        .await
        .map_err(|e| utils::db_error(e, "查询用户角色"))?;
    let mut map: HashMap<uuid::Uuid, Vec<RoleBriefRes>> = HashMap::new();
    for row in rows {
        map.entry(row.user_id).or_default().push(RoleBriefRes {
            role_id: row.role_id.to_string(),
            role_code: row.role_code,
            role_name: row.role_name,
        });
    }
    Ok(map)
}

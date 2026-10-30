//! 用户管理处理器：分页、详情、增删改、启停、重置密码与角色分配。
//!
//! 处理器只做**参数校验与响应组装**：SQL、行映射与关联表读写全部下沉到
//! [`crate::system::user::model`]；日志一律通过 [`crate::support::log`] 的
//! `record_*` 函数以**非阻塞**方式投递。
//!
//! 文件顺序遵循项目约定：**路由方法在前，公共辅助函数在后**。

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use nik::prelude::*;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::business::changelog::{self, Action, ChangeTarget, DiffInput};
use crate::business::mutex;
use crate::business::shared::{guard as scope_guard, principal};
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
#[nik::permission(
    path = "/system/user/page",
    method = "GET",
    permission = "system:user:list"
)]
#[nik::get("/system/user/page")]
pub async fn page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<UserPageQuery>,
) -> ResResult<PageData<Vec<UserRes>>> {
    // 数据权限（批次 B · B3，报批 §5.3-#7）：本接口是首个真实接入的样板。
    // 一次请求只解析一次（`resolve` 内部带 Redis 缓存 + epoch 失效），
    // 之后把 `&ScopedPrincipal` 传给 `page_users` —— 想查分页就必须先解析范围，
    // 这条由类型系统强制（`ScopedPrincipal` 字段私有，见架构 §2.4 闸 1）。
    let user_id = utils::current_user_uuid(&claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "当前令牌缺少用户标识，无法解析数据范围",
        )
    })?;
    let principal = principal::resolve(&user_id)
        .await
        .map_err(|e| utils::db_error(e, "解析当前用户数据范围"))?;

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
    let page_data = model::page_users(&filter, page, page_size, &principal)
        .await
        .map_err(|e| utils::db_error(e, "分页查询用户"))?;

    // 单次 IN 查询批量取回本页用户的角色，按 user_id 聚合，避免 N+1
    let page_user_ids: Vec<uuid::Uuid> = page_data.list.iter().map(|i| i.user.id).collect();
    let role_map = role_map_of(&page_user_ids).await?;

    let mut list: Vec<UserRes> = page_data
        .list
        .iter()
        .map(|item| UserRes::from_model_typed(&item.user, item.dept_name.clone(), &item.user_type))
        .collect();
    for item in &mut list {
        if let Some(id) = utils::parse_uuid(&item.id)
            && let Some(roles) = role_map.get(&id)
        {
            item.roles = roles.clone();
        }
    }
    // 列级脱敏（批次 B · B6）：DTO 出参前脱敏（架构 §3.3 的 D6）。
    // 规则按「角色 ∪ 全局」解析（§3.5：数据范围不参与脱敏判定），超管 / 明版权限
    // 持有者拿到的是豁免集合，`mask` 为 no-op。
    let mask = crate::business::masking::resolve(Some(&principal), Some(&claims))
        .await
        .map_err(|error| utils::db_error(error, "解析列级脱敏规则"))?;
    for item in &mut list {
        crate::business::masking::Maskable::mask(item, &mask);
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
#[nik::permission(
    path = "/system/user/{id}",
    method = "GET",
    permission = "system:user:list"
)]
#[nik::get("/system/user/{id}")]
pub async fn detail(
    Path(id): Path<String>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<UserDetailRes> {
    let id = utils::require_uuid(&id, "id")?;
    let user = load_user(&id).await?;
    let dept_name = dept_name_of(user.dept_id).await?;
    // 账号类型：AI 用户（ROBOT）走独立编辑页，前端需要这个字段决定渲染哪套表单
    let user_type = model::find_user_type(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询用户账号类型"))?
        .unwrap_or_else(|| "HUMAN".to_string());
    let mut res = UserDetailRes::from_model_typed(&user, dept_name, &user_type);
    let role_map = role_map_of(std::slice::from_ref(&id)).await?;
    if let Some(roles) = role_map.get(&id) {
        res.roles = roles.clone();
    }
    // 列级脱敏（批次 B · B6）：详情与列表必须同口径，否则「列表打码、详情明文」就是绕过
    let user_id = utils::current_user_uuid(&claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "当前令牌缺少用户标识，无法解析脱敏规则",
        )
    })?;
    let principal = principal::resolve(&user_id)
        .await
        .map_err(|error| utils::db_error(error, "解析当前用户数据范围"))?;
    let mask = crate::business::masking::resolve(Some(&principal), Some(&claims))
        .await
        .map_err(|error| utils::db_error(error, "解析列级脱敏规则"))?;
    crate::business::masking::Maskable::mask(&mut res, &mask);
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
#[nik::permission(
    path = "/system/user",
    method = "POST",
    permission = "system:user:create"
)]
#[nik::post("/system/user")]
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
    // 角色互斥（批次 B · B7，校验点 1）：新账号「已持有」为空，并集退化为「本次授予」，
    // 于是「同一次请求里同时带上两个互斥角色」同样被拒（架构 D3 的防绕过用例）。
    // 顺序不可交换：先 cap（能不能授）再互斥（能不能共存）。
    let new_user_id = uuid::Uuid::now_v7();
    mutex::ensure_no_mutex(&new_user_id, &role_ids).await?;

    let status = EnableStatus::from_optional_i16(req.status)?;
    let dept_id = utils::parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;
    let password = Crypto::hash_password(&req.password)
        .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "密码加密失败"))?;

    let now = chrono::Utc::now();
    let operator = utils::current_user_uuid(&claims);
    let new_user = SysUserModel {
        id: new_user_id,
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
    //
    // 批次 C · C6：**变更日志在同一事务内**写（D11）。顺序是「业务行 → 日志 → 角色绑定」，
    // 于是「日志之后任何一步失败」都会连日志一起回滚 —— 这正是 T-CL-01/12 要证明的形态。
    let tx_user = new_user.clone();
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        let mut u = tx_user;
        u.insert(&tx).await?;
        let snapshot = user_snapshot(&u);
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of("sys_user", u.id.to_string(), Action::Create, "新增用户"),
            DiffInput::create(&snapshot),
        )
        .await?;
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
#[nik::permission(
    path = "/system/user/{id}",
    method = "PUT",
    permission = "system:user:update"
)]
#[nik::put("/system/user/{id}")]
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
    // 不变式（L1）：AI 用户（`user_type=ROBOT`）的账号与 `sys_ai_agent` 配置行是一对，
    // 只能走 AI 用户接口维护 —— 通用接口改不了类型（`SysUserModel` 无该字段），
    // 但放行会让前端以为「在这里改就行」，从而绕过 SOUL.md / profile 的同步。
    let user_type = model::find_user_type(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询用户账号类型"))?
        .unwrap_or_else(|| "HUMAN".to_string());
    if user_type == "ROBOT" {
        return Err(ApiError::new(
            409,
            error_code::AI_AGENT_USER_TYPE_LOCKED,
            "AI 用户（ROBOT）请通过 /api/system/user/ai/{id} 维护（职责 / SOUL.md / 模型）",
        ));
    }

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

    // 批次 C · C6：前后快照是纯函数（事务外算好），写库与日志同事务提交
    let before_snapshot = user_snapshot(&existing);
    let after_snapshot = user_snapshot(&to_save);
    let tx_user = to_save.clone();
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        tx_user.update(&tx).await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of("sys_user", id.to_string(), Action::Update, "修改用户"),
            DiffInput::update(&before_snapshot, &after_snapshot),
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "更新用户"))?;

    // 数据权限（批次 B · B3，架构 §2.8 失效矩阵「用户换部门」行）：
    // 换了部门 → 该用户的可见部门集合整片变化（`Dept` / `DeptAndChild` / `Custom`
    // 都依赖它），必须立刻删掉其范围缓存；否则最长 300 s（TTL）内仍按旧部门过滤。
    // 注意范围缓存键带 epoch，是**与权限快照不同的另一套键**，`invalidate_user_principal`
    // 不会顺带清它。
    if to_save.dept_id != existing.dept_id {
        tracing::info!(%id, "用户所属部门变更 → 失效数据范围缓存");
        scope_guard::invalidate_user_scope(&id).await;
    }

    // 状态由启用改为停用时，旧访问令牌在自然过期前仍然有效（认证中间件只验
    // JWT 与黑名单、不查状态），需失效主体缓存并强制下线，与 change_status
    // 专用接口的行为保持一致；已是停用态则无需重复踢号。
    if to_save.status != EnableStatus::enabled && existing.status == EnableStatus::enabled {
        // 令牌版本号自增：被停用账号已签发的令牌（含 refresh）整体作废，
        // 不依赖「在线会话表里恰好还有记录」（安全审计 CODE-05 / CODE-08）
        revoke_user_tokens(&id, "update_disable").await;
        crate::system::auth::model::invalidate_user_principal(&id).await;
        revoke_online_sessions(&id, "update_disable").await;
    }

    let dept_name = dept_name_of(to_save.dept_id).await?;
    // 上面已查过账号类型（此处必为 HUMAN：ROBOT 在函数开头就被 409 拦下）
    let mut res = UserDetailRes::from_model_typed(&to_save, dept_name, &user_type);
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
#[nik::permission(
    path = "/system/user/{id}",
    method = "DELETE",
    permission = "system:user:delete"
)]
#[nik::delete("/system/user/{id}")]
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
    // 不变式（L1）：删 AI 用户必须同时清理 Hermes profile / workspace / `sys_ai_agent`，
    // 通用删除只软删 `sys_user` —— 走它会留下孤立配置行与磁盘目录（不变式立刻破）。
    let user_type = model::find_user_type(&id)
        .await
        .map_err(|e| utils::db_error(e, "查询用户账号类型"))?
        .unwrap_or_else(|| "HUMAN".to_string());
    if user_type == "ROBOT" {
        return Err(ApiError::new(
            409,
            error_code::AI_AGENT_USER_TYPE_LOCKED,
            "AI 用户（ROBOT）请通过 DELETE /api/system/user/ai/{id} 删除（会先备份 SOUL.md 再清理 profile）",
        ));
    }
    if user.username == constants::ADMIN_USERNAME {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "内置管理员账号不可删除",
        ));
    }

    // 事务：软删用户 + 物理清理角色绑定，避免用户已删而授权残留
    //
    // 批次 C · C6：删除也留一条字段级凭证（`a` 侧全 null、`b` 侧是删除前的值），
    // 否则「谁删了谁」只剩 `audit_logs` 的粗快照可查。
    let before_snapshot = user_snapshot(&user);
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        SysUserModel::of(id).delete(&tx).await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of("sys_user", id.to_string(), Action::Delete, "删除用户"),
            DiffInput::delete(&before_snapshot),
        )
        .await?;
        // 关联表为硬删表：物理删除该用户的全部角色绑定
        SysUserRoleModel::delete_by(&tx, "user_id", (&id,)).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "删除用户"))?;

    // 用户删除后其令牌应立即失效（JWT 无状态，需主动拉黑其在线会话）
    crate::system::auth::model::invalidate_user_principal(&id).await;
    revoke_online_sessions(&id, "delete_user").await;
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
#[nik::permission(
    path = "/system/user/{id}/status",
    method = "PATCH",
    permission = "system:user:status"
)]
#[nik::patch("/system/user/{id}/status")]
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
    // 批次 C · C6：启停是**独立动作**（Action::Status），与普通编辑分成两条日志，
    // 而不是合并成一条 UPDATE —— 一个操作一条凭证（架构 T-CL-05 的口径）。
    let before_snapshot = user_snapshot(&user);
    let mut after_snapshot = before_snapshot.clone();
    after_snapshot["status"] = serde_json::json!(status.as_i16());
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        SysUserModel::of(id)
            .set(SysUserModel::status(), status)
            // ChangeSet 不自动刷新 updated_at / update_by，需显式设置
            .set(SysUserModel::updated_at(), chrono::Utc::now())
            .set(SysUserModel::update_by(), operator)
            .update(&tx)
            .await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of("sys_user", id.to_string(), Action::Status, "修改用户状态"),
            DiffInput::update(&before_snapshot, &after_snapshot),
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "修改用户状态"))?;

    // 停用即强制下线，使旧令牌在下次请求时被黑名单拦截
    if status != EnableStatus::enabled {
        // 令牌版本号自增：该账号已签发的全部令牌（access + refresh）整体作废
        // （安全审计 CODE-05 / CODE-08）
        revoke_user_tokens(&id, "change_status_disable").await;
        crate::system::auth::model::invalidate_user_principal(&id).await;
        revoke_online_sessions(&id, "change_status_disable").await;
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
#[nik::permission(
    path = "/system/user/{id}/password",
    method = "PATCH",
    permission = "system:user:password"
)]
#[nik::patch("/system/user/{id}/password")]
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
        revoke_online_sessions(&id, "admin_reset_password").await;
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
#[nik::permission(
    path = "/system/user/{id}/roles",
    method = "GET",
    permission = "system:user:list"
)]
#[nik::get("/system/user/{id}/roles")]
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
#[nik::permission(
    path = "/system/user/{id}/roles",
    method = "PUT",
    permission = "system:user:role"
)]
#[nik::put("/system/user/{id}/roles")]
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
    // 角色互斥（批次 B · B7，校验点 1）：校验集合 = `已持有 ∪ 本次授予` 两两配对
    // （架构 D9 —— 只比「本次 vs 已持有」会被同次授予绕过）。
    // 顺序不可交换：② cap 先报越权（安全审计要优先看到的信号），③ 互斥后报共存冲突。
    mutex::ensure_no_mutex(&id, &role_ids).await?;
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
    // 数据权限（批次 B · B3，架构 §2.8 失效矩阵第一行「用户角色变更」）：
    // 角色集合决定 `data_scope` 的并集结果，换角色即换可见范围 → 删该用户范围缓存。
    scope_guard::invalidate_user_scope(&id).await;
    revoke_online_sessions(&id, "assign_roles").await;
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
#[nik::permission(
    path = "/system/user/batch-dept",
    method = "PUT",
    permission = "system:user:update"
)]
#[nik::put("/system/user/batch-dept")]
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
    // 批次 B · B5（附录 C #4）：写操作受数据范围约束 —— 不能把「看不见的用户」挪到别的部门。
    // 判据：目标集合里落在 `filter_for(SYS_USER)` 可见范围内的条数与入参去重后长度相等。
    // 不足时只报差集条数，不逐个点名（避免用本接口探测「谁存在」）。
    let principal = principal::resolve_for_claims(&claims).await?;
    let visible = model::count_visible_users(&user_ids, &principal)
        .await
        .map_err(|e| utils::db_error(e, "校验目标用户数据范围"))?;
    let requested = {
        let mut unique = user_ids.clone();
        unique.sort_unstable();
        unique.dedup();
        unique.len() as u64
    };
    if visible != requested {
        return Err(ApiError::new(
            403,
            error_code::DATA_SCOPE_EXCEED_LIMIT,
            format!(
                "有 {} 个用户不在你的数据范围内（或已删除），批量调整部门已拒绝",
                requested.saturating_sub(visible)
            ),
        ));
    }
    let operator = utils::current_user_uuid(&claims);
    // 审计快照用的用户 ID 字符串列表（user_ids 随后会移入事务闭包）
    let user_id_strs = user_ids.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    // 事务闭包会 move `user_ids`，数据范围缓存失效在事务后执行，先留一份副本
    let scope_targets = user_ids.clone();

    db().transaction(|tx| async move {
        model::batch_update_dept(&tx, &user_ids, &dept_id, operator).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "批量调整用户部门"))?;
    // 数据权限（批次 B · B3，架构 §2.8 失效矩阵「用户换部门」行）：批量换部门
    // 等于批量换可见集合，逐用户删范围缓存（键带用户 ID，不能整模式删）。
    scope_guard::invalidate_user_scopes(&scope_targets).await;
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

/// 清理指定用户的在线会话（access + refresh，`online:token:*` / `online:refresh:*`）。
///
/// 与 [`revoke_user_tokens`] 的分工见上；两者一起构成「库侧兜底 + 缓存侧即时」。
///
/// # 失败处理：可接受降级（不返回 500）
///
/// 本仓库用户管理侧的全部调用点（停用 / 删除 / 重置口令 / 改角色）都在**同一处**
/// 先自增 `token_version`（或已把用户软删，认证层查不到会话直接 401），因此库侧
/// 吊销不依赖缓存；缓存枚举失败只意味着少一层即时性，旧令牌最迟在 access TTL
/// （15 分钟）内自然过期，或在下一次请求被 `ver` 比对拦下。
///
/// 反过来若这里回 500，前端会把「其实已经成功」的停用 / 改密显示成失败，诱发
/// 重复操作——所以**只留痕、不改结果**，但日志必须是 `error` 级，不许静默
/// （评审 §1.3）。
///
/// # 参数
///
/// - `user_id`：目标用户 ID。
/// - `context`：调用场景（写入日志便于排查）。
async fn revoke_online_sessions(user_id: &uuid::Uuid, context: &'static str) {
    if let Err(e) = crate::support::online::model::force_logout_user(&user_id.to_string()).await {
        tracing::error!(
            error = %e,
            user_id = %user_id,
            context,
            "清理在线会话失败（令牌版本号 / 软删已在库侧生效）"
        );
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

/// `sys_user` 的审计快照（批次 C · C6）。
///
/// # 为什么手写而不是 `serde_json::to_value(user)`
///
/// ① `SysUserModel` 的派生宏不保证 `Serialize`（`#[derive(Model)]` 只管表映射）；
/// ② 即使能序列化，整行会有 `login_fail_count` / `last_login_at` / `token_version`
///    这类**每次登录都变**的运行时列，它们进 diff 只会把审计表变成访问日志。
///    所以快照按 `business/changelog/diff.rs::SYS_USER.fields` 白名单逐列构造 ——
///    白名单增删时这里会漏列，而 `tests/batchC_c6_changelog.rs` 有一条断言盯着
///    「登记的白名单字段必须全部出现在快照里」（宁可立刻红，也不要静默少记）。
fn user_snapshot(user: &SysUserModel) -> serde_json::Value {
    serde_json::json!({
        "username": user.username,
        "real_name": user.real_name,
        "email": user.email,
        "mobile": user.mobile,
        "dept_id": user.dept_id.map(|value| value.to_string()),
        "status": user.status.as_i16(),
        "is_locked": user.is_locked,
        "remark": user.remark,
        // 口令只放哈希（bcrypt）：diff 的 L1 会把它整值抹成 {"redacted":true}，
        // 但「改过密码」这件事必须留痕（changed_fields 含 password）
        "password": user.password,
    })
}

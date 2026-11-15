//! 角色管理处理器：分页、下拉、详情、增删改与菜单授权。
//!
//! 处理器承担**参数校验、业务编排与响应组装**，并直接调用框架快捷方法
//! （`find_or_none` / `find_by` / `count_by` / `delete_by` / `insert` / `update` /
//! `delete` 等）；需要编写 SQL 的查询（分页、下拉、编码唯一性校验）与领域行为
//! （菜单授权重建）集中在 [`crate::system::role::model`]。
//!
//! 文件顺序遵循项目约定：**路由方法在前，公共辅助函数在后**。

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use nik::prelude::*;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::business::changelog::{self, Action, ChangeTarget, DiffInput};
use crate::business::mutex;
use crate::business::shared::guard as scope_guard;
use crate::business::shared::principal;
use crate::business::shared::scope::ScopeKind;
use crate::shared::constants::EnableStatus;
use crate::shared::{constants, error_code};
use crate::support::log;
use crate::system::menu::model::SysMenuModel;
use crate::system::role::dto::{
    RoleMenuAssignReq, RolePageQuery, RoleRes, RoleSaveReq, RoleUserPageQuery, RoleUserRes,
};
use crate::system::role::model::{self, SysRoleMenuModel, SysRoleModel};
use crate::system::user::model::SysUserRoleModel;
use crate::utils;

// ==================== 接口 ====================

/// 角色分页查询（支持角色名 / 编码模糊与状态过滤）。
///
/// # 参数
///
/// - `query`：分页与过滤条件。
///
/// # 返回值
///
/// 成功返回 `{list, total, page, page_size}`。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::permission(path = "/system/role/page", method = "GET", permission = "system:role:list")]
#[nik::get("/system/role/page")]
pub async fn page(Query(query): Query<RolePageQuery>) -> ResResult<PageData<Vec<RoleRes>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = model::RolePageFilter {
        role_name: query.role_name,
        role_code: query.role_code,
        status: query.status,
    };

    let page_data = model::page_roles(&filter, page, page_size)
        .await
        .map_err(|e| utils::db_error(e, "分页查询角色"))?;

    let list: Vec<RoleRes> = page_data.list.iter().map(RoleRes::from).collect();
    Ok(Res::page(list, page_data.total, page_data.page, page_data.size))
}

/// 查询全部启用角色（按排序值升序，供下拉选择）。
///
/// # 返回值
///
/// 成功返回启用角色列表。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[nik::permission(path = "/system/role/list", method = "GET", permission = "system:role:list")]
#[nik::get("/system/role/list")]
pub async fn list() -> ResResult<Vec<RoleRes>> {
    // 启用角色下拉是高频热点查询，优先读缓存
    if let Some(cached) = model::load_cached_enabled_roles().await {
        return Ok(Res::ok(cached));
    }
    let roles = model::list_enabled_roles()
        .await
        .map_err(|e| utils::db_error(e, "查询启用角色"))?;
    let list: Vec<RoleRes> = roles.iter().map(RoleRes::from).collect();
    model::cache_enabled_roles(&list).await;
    Ok(Res::ok(list))
}

/// 角色详情。
///
/// # 参数
///
/// - `id`：角色 ID。
///
/// # 返回值
///
/// 成功返回角色详情。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：角色不存在；
/// - 500：数据库异常。
#[nik::permission(path = "/system/role/{id}", method = "GET", permission = "system:role:list")]
#[nik::get("/system/role/{id}")]
pub async fn detail(Path(id): Path<String>) -> ResResult<RoleRes> {
    let id = utils::require_uuid(&id, "id")?;
    let role = load_role(&id).await?;
    Ok(Res::ok(RoleRes::from(&role)))
}

/// 新建角色。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（写入审计字段）。
/// - `req`：角色请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`ROLE_CREATE`，`after_data` 记录角色编码 / 名称）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回新建角色详情。
///
/// # 异常情况
///
/// - 400：参数非法；
/// - 409：角色编码已存在；
/// - 500：数据库异常。
#[nik::permission(path = "/system/role", method = "POST", permission = "system:role:create")]
#[nik::post("/system/role")]
pub async fn create(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<RoleSaveReq>,
) -> ResResult<RoleRes> {
    let role_code = req.role_code.trim().to_string();
    ensure_role_code_unique(&role_code, None).await?;
    // 数据范围（报批 §5.3-#5 ①）：缺省 = 1「全部」，与迁移默认值逐字一致；
    // 非超管新建角色时同样走「范围上限」校验，否则「建一个新角色」就是提权通道。
    let data_scope = resolve_data_scope(req.data_scope, ScopeKind::All.as_i16())?;
    ensure_data_scope_grantable(&claims, None, data_scope, &[]).await?;

    let now = chrono::Utc::now();
    let operator = utils::current_user_uuid(&claims);
    let role_id = uuid::Uuid::now_v7();
    let escalation_role_id = resolve_escalation_role(req.escalation_role_id.as_deref(), role_id, None).await?;
    let role = SysRoleModel {
        id: role_id,
        role_code,
        role_name: req.role_name.trim().to_string(),
        description: req.description.map(|v| v.trim().to_string()),
        status: EnableStatus::from_optional_i16(req.status)?,
        data_scope,
        sort_order: req.sort_order.unwrap_or(0),
        escalation_role_id,
        remark: req.remark.map(|v| v.trim().to_string()),
        created_at: now,
        updated_at: now,
        create_by: operator,
        update_by: operator,
        deleted_at: None,
    };
    // 批次 C · C6：写库与变更日志同事务（D11），失败一起回滚
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    let snapshot = role_snapshot(&role);
    let mut tx_role = role.clone();
    db().transaction(|tx| async move {
        tx_role.insert(&tx).await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of("sys_role", tx_role.id.to_string(), Action::Create, "新增角色"),
            DiffInput::create(&snapshot),
        )
        .await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "新建角色"))?;
    model::invalidate_enabled_roles_cache().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ROLE_CREATE",
            "Role",
            role.id.to_string(),
            None,
            Some(serde_json::json!({
                "role_name": role.role_name,
                "role_code": role.role_code,
            })),
        ),
    );
    Ok(Res::ok(RoleRes::from(&role)))
}

/// 更新角色（全量业务字段；内置超级管理员角色不可禁用）。
///
/// # 参数
///
/// - `id`：角色 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（写入 `update_by`）。
/// - `req`：角色请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`ROLE_UPDATE`，含角色编码 / 名称的变更前、后快照）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回更新后的角色详情。
///
/// # 异常情况
///
/// - 400：ID 或状态参数非法；
/// - 404：角色不存在；
/// - 409：角色编码已存在，或试图禁用内置超级管理员角色；
/// - 500：数据库异常。
#[nik::permission(path = "/system/role/{id}", method = "PUT", permission = "system:role:update")]
#[nik::put("/system/role/{id}")]
pub async fn update(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<RoleSaveReq>,
) -> ResResult<RoleRes> {
    let id = utils::require_uuid(&id, "id")?;
    let existing = load_role(&id).await?;

    let role_code = req.role_code.trim().to_string();
    ensure_role_code_unique(&role_code, Some(id)).await?;

    let status = match req.status {
        Some(value) => EnableStatus::from_i16(value)?,
        None => existing.status,
    };
    // 内置角色（SUPER_ADMIN）短路全部权限校验，禁止禁用
    if existing.role_code == constants::SUPER_ADMIN_ROLE_CODE && status != EnableStatus::enabled {
        return Err(ApiError::new(
            409,
            error_code::ROLE_IN_USE,
            "内置超级管理员角色不可禁用",
        ));
    }

    // 角色互斥（批次 B · B7，校验点 2）：角色由停用变启用时，原本「休眠」的绑定会变成
    // 有效持有（停用角色不计入持有，见 `business/mutex/model.rs::HELD_ROLE_SQL`），
    // 可能让互斥关系真正成立 —— 不在这里复核，互斥就会被「先停用 → 授完 → 再启用」绕过。
    // 改 `role_code` / `role_name` 不需要校验：互斥规则按角色 **ID** 定义，名称变更不改变
    // 互斥拓扑。必须发生在写库之前，否则拦截时库里已经处于违规状态。
    if existing.status != status && status == EnableStatus::enabled {
        mutex::ensure_role_enable_no_mutex(&id, &role_code, req.role_name.trim()).await?;
    }

    // 数据范围（报批 §5.3-#4/#5）：`None` = **保留现值**——这是架构点名的坑：
    // `RoleSaveReq` 是 PUT 全量更新，若把 None 当默认值处理，「只改角色名」就会把
    // data_scope 重置成「全部」（数据破坏）。所以缺省取 `existing.data_scope`。
    let data_scope = resolve_data_scope(req.data_scope, existing.data_scope)?;
    if data_scope != existing.data_scope {
        ensure_data_scope_grantable(&claims, Some(existing.data_scope), data_scope, &[]).await?;
    }

    let role = SysRoleModel {
        id,
        role_code,
        role_name: req.role_name.trim().to_string(),
        description: req.description.map(|v| v.trim().to_string()),
        status,
        data_scope,
        sort_order: req.sort_order.unwrap_or(existing.sort_order),
        escalation_role_id: resolve_escalation_role(req.escalation_role_id.as_deref(), id, existing.escalation_role_id)
            .await?,
        remark: req.remark.map(|v| v.trim().to_string()),
        // 保留创建审计字段，仅更新业务字段
        created_at: existing.created_at,
        updated_at: chrono::Utc::now(),
        create_by: existing.create_by,
        update_by: utils::current_user_uuid(&claims),
        deleted_at: None,
    };
    // 批次 C · C6：前后快照 + 写库 + 日志同事务
    //
    // BUG-SYS-03：同一事务里推进**该角色持有者**的 `token_version`（库侧兜底）。
    // 角色名 / 状态 / `data_scope` 变更会改变持有者的权限快照，旧令牌必须立即作废；
    // 这一步与 Redis 是否可用无关，缓存侧吊销因此可以降级为卫生动作。
    let before_snapshot = role_snapshot(&existing);
    let after_snapshot = role_snapshot(&role);
    let tx_role = role.clone();
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        tx_role.update(&tx).await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of("sys_role", tx_role.id.to_string(), Action::Update, "修改角色"),
            DiffInput::update(&before_snapshot, &after_snapshot),
        )
        .await?;
        crate::system::user::model::bump_token_version_of_role_holders_in(&tx, tx_role.id, constants::ADMIN_USERNAME)
            .await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "更新角色"))?;

    model::invalidate_enabled_roles_cache().await;
    // FIX-05：角色名 / 状态 / 数据范围变更推进了**全体持有者**的版本号
    // （事务内 `bump_token_version_of_role_holders_in`），提交后清空会话字段缓存，
    // 否则旧版本号最多还会被缓存命中 60 s。
    crate::shared::session_cache::invalidate_all().await;
    // 角色名称 / 状态变更会影响持有者权限快照，清理其主体上下文缓存
    crate::system::auth::model::invalidate_role_holders(&id).await;
    if data_scope != existing.data_scope {
        // 数据范围缓存（`perm:user_scope:{epoch}:{uid}`）与权限快照是**两套键**：
        // 上面只清了权限，范围缓存必须单独按持有者清理，否则最长 300 s（TTL）内
        // 这些用户仍按旧范围过滤 —— 放宽时是「看不到」，收紧时是**越权可见**。
        invalidate_role_scope_cache(&id).await;
    }
    // 缓存侧的在线会话清理只是卫生动作：持有者的 `token_version` 已在上面的事务里推进
    // （库侧失效），这里失败不回 500 —— 角色确实改完了，回 500 会让管理员重试
    // （BUG-SYS-03）。失败以 warn + pending_compensation 留痕。
    crate::support::online::model::revoke_after_permission_change("role_update", &[constants::ADMIN_USERNAME]).await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ROLE_UPDATE",
            "Role",
            role.id.to_string(),
            Some(serde_json::json!({
                "role_name": existing.role_name,
                "role_code": existing.role_code,
                "data_scope": existing.data_scope,
            })),
            Some(serde_json::json!({
                "role_name": role.role_name,
                "role_code": role.role_code,
                "data_scope": role.data_scope,
            })),
        ),
    );
    Ok(Res::ok(RoleRes::from(&role)))
}

/// 删除角色（内置角色与已分配给用户的角色不可删除）。
///
/// # 参数
///
/// - `id`：角色 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（审计操作者）。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`ROLE_DELETE`，`before_data` 记录被删角色编码 / 名称）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：角色不存在；
/// - 409：内置角色或角色已分配给用户；
/// - 500：数据库异常。
#[nik::permission(path = "/system/role/{id}", method = "DELETE", permission = "system:role:delete")]
#[nik::delete("/system/role/{id}")]
pub async fn delete(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let role = load_role(&id).await?;
    if role.role_code == constants::SUPER_ADMIN_ROLE_CODE {
        return Err(ApiError::new(
            409,
            error_code::ROLE_IN_USE,
            "内置超级管理员角色不可删除",
        ));
    }

    let assigned = SysUserRoleModel::count_by(db(), "role_id", (&id,))
        .await
        .map_err(|e| utils::db_error(e, "统计角色分配"))?;
    if assigned > 0 {
        return Err(ApiError::new(409, error_code::ROLE_IN_USE, "角色已分配给用户"));
    }

    // 事务：软删角色 + 物理清理其菜单授权绑定，避免角色已删而授权残留
    //
    // 批次 C · C6：同事务补一条字段级凭证（`data_scope` 在内 —— 删掉一个宽范围角色
    // 本身就是权限收缩动作，值得可追溯）。
    let before_snapshot = role_snapshot(&role);
    let ctx = changelog::RequestCtx::from_request(&claims, &headers, Some(&connect_info));
    db().transaction(|tx| async move {
        SysRoleModel::of(id).delete(&tx).await?;
        changelog::record_in(
            &tx,
            &ctx.ctx(),
            ChangeTarget::of("sys_role", id.to_string(), Action::Delete, "删除角色"),
            DiffInput::delete(&before_snapshot),
        )
        .await?;
        // 关联表为硬删表：物理删除该角色的全部菜单授权
        SysRoleMenuModel::delete_by(&tx, "role_id", (&id,)).await?;
        Ok(())
    })
    .await
    .map_err(|e| utils::db_error(e, "删除角色"))?;

    // 删除前已校验该角色未分配给任何用户，故无在线用户持有它，无需强制下线
    model::invalidate_enabled_roles_cache().await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ROLE_DELETE",
            "Role",
            id.to_string(),
            Some(serde_json::json!({
                "role_name": role.role_name,
                "role_code": role.role_code,
            })),
            None,
        ),
    );
    Ok(Res::ok_empty())
}

/// 查询角色已授权的菜单 id 列表。
///
/// # 参数
///
/// - `id`：角色 ID。
///
/// # 返回值
///
/// 成功返回菜单 id 字符串列表。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：角色不存在；
/// - 500：数据库异常。
#[nik::permission(path = "/system/role/{id}/menus", method = "GET", permission = "system:role:list")]
#[nik::get("/system/role/{id}/menus")]
pub async fn menus(Path(id): Path<String>) -> ResResult<Vec<String>> {
    let id = utils::require_uuid(&id, "id")?;
    load_role(&id).await?;
    let bindings = SysRoleMenuModel::find_by(db(), "role_id", (&id,))
        .await
        .map_err(|e| utils::db_error(e, "查询角色菜单授权"))?;
    Ok(Res::ok(bindings.into_iter().map(|b| b.menu_id.to_string()).collect()))
}

/// 保存角色菜单授权（先清空全部授权，再逐条重建；空数组表示清空）。
///
/// # 参数
///
/// - `id`：角色 ID。
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：当前用户声明（审计操作者）。
/// - `req`：菜单授权请求体。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`ROLE_ASSIGN_MENU`，含授权前后的菜单 ID 列表）；
/// 日志写入失败不影响业务结果。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：ID 或菜单 ID 格式非法；
/// - 404：角色不存在；
/// - 500：数据库异常。
#[nik::permission(path = "/system/role/{id}/menus", method = "PUT", permission = "system:role:menu")]
#[nik::put("/system/role/{id}/menus")]
pub async fn assign_menus(
    Path(id): Path<String>,
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<RoleMenuAssignReq>,
) -> ResResult<()> {
    let id = utils::require_uuid(&id, "id")?;
    let role = load_role(&id).await?;
    // 内置超级管理员角色的授权集是「全部菜单」的语义，改动它等于公开改超管权限：
    // 只允许超管操作（安全审计 CODE-04 的同类边界）。
    if role.role_code == constants::SUPER_ADMIN_ROLE_CODE && !claims.is_super_admin() {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            "内置超级管理员角色的菜单授权只能由超级管理员修改",
        ));
    }

    let mut menu_ids = Vec::with_capacity(req.menu_ids.len());
    for raw in &req.menu_ids {
        menu_ids.push(utils::require_uuid(raw, "menu_ids")?);
    }
    // 授予上限：只能授予自己已持有的权限码（菜单的 `auth_code` 必须在自己的
    // 权限集合内），否则「角色授权」就成了提权通道（安全审计 CODE-04 / CODE-12）。
    ensure_menus_grantable(&claims, &menu_ids).await?;

    // 记录授权变更前的菜单 id 列表，供审计 before 快照
    let before_menu_ids: Vec<String> = SysRoleMenuModel::find_by(db(), "role_id", (&id,))
        .await
        .map_err(|e| utils::db_error(e, "查询角色菜单授权"))?
        .into_iter()
        .map(|b| b.menu_id.to_string())
        .collect();
    let after_menu_ids: Vec<String> = menu_ids.iter().map(|v| v.to_string()).collect();

    model::replace_role_menus(&id, &menu_ids, constants::ADMIN_USERNAME)
        .await
        .map_err(|e| utils::db_error(e, "保存角色菜单授权"))?;

    // 角色菜单授权变更直接改变该角色用户的可访问权限，清理其主体上下文缓存
    crate::system::auth::model::invalidate_role_holders(&id).await;
    // 缓存侧的在线会话清理只是卫生动作：授权写在上面的事务里，且同一事务已推进
    // 持有者的 `token_version`（库侧失效，`middleware::session_guard` 逐请求比对）。
    // 因此缓存不可用不再回 500（BUG-SYS-03），失败以 warn + pending_compensation 留痕。
    crate::support::online::model::revoke_after_permission_change("role_assign_menu", &[constants::ADMIN_USERNAME])
        .await;
    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "ROLE_ASSIGN_MENU",
            "Role",
            id.to_string(),
            Some(serde_json::json!({ "menu_ids": before_menu_ids })),
            Some(serde_json::json!({ "menu_ids": after_menu_ids })),
        ),
    );
    Ok(Res::ok_empty())
}

/// 查询持有该角色的用户（分页，供角色管理「角色用户列表」页签使用）。
///
/// # 参数
///
/// - `id`：角色 ID。
/// - `query`：分页条件。
///
/// # 返回值
///
/// 成功返回 `{list, total, page, page_size}`，列表项附带该用户的角色名称列表。
///
/// # 异常情况
///
/// - 400：ID 格式非法；
/// - 404：角色不存在；
/// - 500：数据库异常。
#[nik::permission(path = "/system/role/{id}/users", method = "GET", permission = "system:role:list")]
#[nik::get("/system/role/{id}/users")]
pub async fn users(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(query): Query<RoleUserPageQuery>,
) -> ResResult<PageData<Vec<RoleUserRes>>> {
    let id = utils::require_uuid(&id, "id")?;
    load_role(&id).await?;
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    // 批次 B · B5（附录 C #7）：角色用户页签是「人员列表」的第二个入口，
    // 必须按数据范围收口，否则本部门主管能从角色页签翻出全公司人员。
    let principal = principal::resolve_for_claims(&claims).await?;
    let page_data = model::page_role_users(&id, page, page_size, &principal)
        .await
        .map_err(|e| utils::db_error(e, "分页查询角色用户"))?;

    // 单次 IN 查询批量取回本页用户的角色，按 user_id 聚合，避免 N+1
    let page_user_ids: Vec<uuid::Uuid> = page_data.list.iter().map(|u| u.id).collect();
    let role_rows = crate::system::user::model::find_user_roles_for_users(&page_user_ids)
        .await
        .map_err(|e| utils::db_error(e, "查询用户角色"))?;
    let mut role_names: HashMap<uuid::Uuid, Vec<String>> = HashMap::new();
    for row in role_rows {
        role_names.entry(row.user_id).or_default().push(row.role_name);
    }

    let list: Vec<RoleUserRes> = page_data
        .list
        .iter()
        .map(|u| RoleUserRes {
            id: u.id.to_string(),
            username: u.username.clone(),
            real_name: u.real_name.clone().unwrap_or_default(),
            mobile: u.mobile.clone().unwrap_or_default(),
            status: u.status.as_i16(),
            roles: role_names.get(&u.id).cloned().unwrap_or_default(),
        })
        .collect();
    Ok(Res::page(list, page_data.total, page_data.page, page_data.size))
}

// ==================== 公共辅助 ====================

/// 按主键加载角色，不存在时返回业务错误。
///
/// # 参数
///
/// - `id`：角色 ID。
///
/// # 返回值
///
/// 角色模型。
///
/// # 异常情况
///
/// - 404：角色不存在（`ROLE_NOT_FOUND`）；
/// - 500：数据库异常。
async fn load_role(id: &uuid::Uuid) -> std::result::Result<SysRoleModel, ApiError> {
    SysRoleModel::find_or_none(db(), id)
        .await
        .map_err(|e| utils::db_error(e, "查询角色"))?
        .ok_or_else(|| ApiError::new(404, error_code::ROLE_NOT_FOUND, "角色不存在"))
}

/// 校验操作者能否把给定菜单授权给角色（授予上限）。
///
/// # 规则
///
/// - 菜单不存在 → 404（顺带补上原实现缺失的存在性校验）；
/// - 操作者为超管 → 放行；
/// - 菜单的 `auth_code` 非空且不在操作者权限集合内 → 403（不能授予自己没有的权限点，
///   否则「角色授权」= 提权通道，安全审计 CODE-04 / CODE-12）；
/// - `auth_code` 为空（目录 / 纯展示菜单）不构成额外权限，放行。
///
/// # 参数
///
/// - `claims`：当前请求的令牌声明（权限集合取自 claims）。
/// - `menu_ids`：待授予的菜单 ID 列表。
///
/// # 返回值
///
/// 全部判定通过时返回 `Ok(())`。
///
/// # 异常情况
///
/// - 403：授予了操作者未持有的权限码；
/// - 404：某个菜单不存在或已删除；
/// - 500：数据库异常。
async fn ensure_menus_grantable(claims: &TokenClaims, menu_ids: &[uuid::Uuid]) -> std::result::Result<(), ApiError> {
    let super_admin = claims.is_super_admin();
    for menu_id in menu_ids {
        let menu = SysMenuModel::find_or_none(db(), menu_id)
            .await
            .map_err(|e| utils::db_error(e, "校验菜单"))?
            .ok_or_else(|| ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "菜单不存在或已删除".to_string()))?;
        if super_admin {
            continue;
        }
        let auth_code = menu.auth_code.trim();
        if !auth_code.is_empty() && !claims.has_permission(auth_code) {
            return Err(ApiError::new(
                403,
                error_code::PERMISSION_DENIED,
                format!("不能授予自己未持有的权限点: {auth_code}"),
            ));
        }
    }
    Ok(())
}

/// 校验角色编码唯一（软删表上仅比较未删除行）。
///
/// # 参数
///
/// - `role_code`：待校验的角色编码。
/// - `exclude`：更新场景需排除的自身 ID；新建场景传 `None`。
///
/// # 返回值
///
/// 编码未被占用时返回 `Ok(())`。
///
/// # 异常情况
///
/// - 409：角色编码已存在（`ROLE_CODE_EXISTS`）；
/// - 500：数据库异常。
async fn ensure_role_code_unique(role_code: &str, exclude: Option<uuid::Uuid>) -> std::result::Result<(), ApiError> {
    let count = model::count_roles_by_code(role_code, exclude)
        .await
        .map_err(|e| utils::db_error(e, "校验角色编码唯一性"))?;
    utils::ensure_unique(count, error_code::ROLE_CODE_EXISTS, "角色编码已存在")
}

// ==================== 上级角色（审批降级链，P3-02） ====================

/// 请求里的 `escalation_role_id` 三态（P3-02）。
///
/// `RoleSaveReq` 是 **PUT 全量更新**，`Option<String>` 无法同时表达「没带这个字段」与
/// 「明确清空」—— 所以这里显式分三态：
///
/// - [`EscalationInput::Keep`]：字段缺失 ⇒ 保留库里现值（老前端不带该字段时不会误清）；
/// - [`EscalationInput::Clear`]：空串 / `"null"` ⇒ 清空；
/// - [`EscalationInput::Set`]：uuid ⇒ 设为该角色（要过自引用 / 存在性 / 成环三道校验）。
enum EscalationInput {
    /// 保留现值。
    Keep,
    /// 清空（无上级）。
    Clear,
    /// 指向某个角色。
    Set(uuid::Uuid),
}

fn parse_escalation_input(raw: Option<&str>) -> std::result::Result<EscalationInput, ApiError> {
    match raw {
        None => Ok(EscalationInput::Keep),
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
                return Ok(EscalationInput::Clear);
            }
            let id = uuid::Uuid::parse_str(trimmed)
                .map_err(|_| ApiError::new(400, error_code::ROLE_INVALID, format!("上级角色 id 非法：{trimmed}")))?;
            Ok(EscalationInput::Set(id))
        }
    }
}

/// 校验上级角色：不能是自己、必须存在、**不能成环**。
///
/// # 为什么成环要在应用层拦（库里只挡得住自引用）
///
/// `A→B→A` 在 DB 层是两个合法外键。只靠运行时检测的后果是：配错那一刻没人管，
/// 直到某张单提交时才发现 65040 —— 而那时流程已经卡住了。所以写入口就沿链走一遍
/// （最多 32 跳），能回到自己就拒绝。
///
/// 历史脏数据里的环（比如直接改库造出来的）走到重复节点即停：那是运行时要报 65040 的事，
/// 不应该让「改个角色名」也失败。
async fn ensure_escalation_valid(target: uuid::Uuid, self_id: uuid::Uuid) -> std::result::Result<(), ApiError> {
    if target == self_id {
        return Err(ApiError::new(400, error_code::ROLE_INVALID, "上级角色不能是本角色自己"));
    }
    if model::escalation_parent_of(target)
        .await
        .map_err(|e| utils::db_error(e, "校验上级角色"))?
        .is_none()
    {
        return Err(ApiError::new(
            400,
            error_code::ROLE_INVALID,
            "上级角色不存在（可能已被删除）",
        ));
    }
    let mut current = target;
    let mut visited: std::collections::BTreeSet<uuid::Uuid> = std::collections::BTreeSet::new();
    for _ in 0..32 {
        if current == self_id {
            return Err(ApiError::new(
                400,
                error_code::ROLE_INVALID,
                "上级角色会形成环（沿该角色的升级链会绕回本角色）",
            ));
        }
        if !visited.insert(current) {
            // 已有环（历史脏数据）：不在这里判死，交运行时 65040
            break;
        }
        match model::escalation_parent_of(current)
            .await
            .map_err(|e| utils::db_error(e, "校验上级角色"))?
        {
            Some(Some(parent)) => current = parent,
            _ => break,
        }
    }
    Ok(())
}

/// 解析并校验请求里的上级角色，得到**要写库的值**。
///
/// # 参数
///
/// - `raw`：请求体里的 `escalation_role_id`；
/// - `self_id`：本角色 id（新建时是刚生成的新 id）；
/// - `existing`：库里现值（更新时；新建传 `None`）。
async fn resolve_escalation_role(
    raw: Option<&str>,
    self_id: uuid::Uuid,
    existing: Option<uuid::Uuid>,
) -> std::result::Result<Option<uuid::Uuid>, ApiError> {
    match parse_escalation_input(raw)? {
        EscalationInput::Keep => Ok(existing),
        EscalationInput::Clear => Ok(None),
        EscalationInput::Set(target) => {
            ensure_escalation_valid(target, self_id).await?;
            Ok(Some(target))
        }
    }
}

// ==================== 数据范围（批次 B · B3，报批 §5.3-#5） ====================

/// 解析并校验请求里的 `data_scope`。
///
/// # 参数
///
/// - `raw`：请求体里的取值（`None` = 请求未带该字段）；
/// - `fallback`：`raw` 为 `None` 时的取值（新建取 `1`「全部」，更新取**库中现值**）。
///
/// # 返回值
///
/// 校验通过的 `i16`（1..5）。
///
/// # 异常情况
///
/// 取值不在 1..5 → `400 + DATA_SCOPE_INVALID(61001)`。校验走
/// [`ScopeKind::from_i16`]，与写入 / 读取 / 缓存反序列化**同一个**转换口，
/// 不允许在这里另写一份 `match 1 => …`（架构 §2.1：任何 `data_scope` 都必须经它）。
fn resolve_data_scope(raw: Option<i16>, fallback: i16) -> std::result::Result<i16, ApiError> {
    let value = raw.unwrap_or(fallback);
    Ok(ScopeKind::from_i16(value)?.as_i16())
}

/// 数据范围的「宽窄等级」——用于非超管的**范围上限**校验（纯函数，可单测）。
///
/// # 分级理由
///
/// 部门类范围（`DEPT` / `DEPT_AND_CHILD` / `CUSTOM`）在 B3 阶段没有
/// `custom_dept_ids` 入参（该字段由 B4 的专用接口提供），其最终部门集合取决于
/// **持有者**所在部门，无法在授权时刻静态判定，因此三者同属一级：同级可互授
/// （管理员按部门维度调整），但**不得**越过等级。
///
/// | 等级 | 范围 | 语义 |
/// |---|---|---|
/// | 0 | `SELF` | 只能看自己 |
/// | 1 | `DEPT` / `DEPT_AND_CHILD` / `CUSTOM` | 部门维度（自己可见的那片） |
/// | 2 | `ALL` | 全公司 |
///
/// 「授出比自己更宽的范围」=「把自己看不见的数据授权出去」，这是与批次 A
/// delegation cap 同源的提权路径（架构 §2.7 末条）。
const fn scope_class(kind: ScopeKind) -> u8 {
    match kind {
        ScopeKind::SelfOnly => 0,
        ScopeKind::Dept | ScopeKind::DeptAndChild | ScopeKind::Custom => 1,
        ScopeKind::All => 2,
    }
}

/// 数据范围的「宽度」——用于判定「这一次修改是收紧还是放宽」（纯函数，可单测）。
///
/// # 分级理由（与 [`scope_class`] 是两个不同问题，别合并）
///
/// [`scope_class`] 比的是「操作者自己能看多远」，只有三档；这里比的是「两个范围谁更宽」，
/// 需要把部门类再细分，否则 `DEPT → DEPT_AND_CHILD` 会被判成「同级 = 不算放宽」——
/// 而它对持有者本人**就是**扩大可见范围（`b3_mgr` 改完立刻能看到原先看不见的后端组）。
///
/// | 宽度 | 范围 | 说明 |
/// |---|---|---|
/// | 0 | `SELF` | 仅本人，最窄 |
/// | 1 | `DEPT` | 单个部门 |
/// | 2 | `CUSTOM` / `DEPT_AND_CHILD` | 一组部门 / 一棵子树，**互不包含**，同宽（互相切换一律按放宽处理，保守） |
/// | 3 | `ALL` | 全公司 |
///
/// 判定式：`new != current && width(new) >= width(current)` → 放宽 / 平级换形 → 拒。
const fn scope_width(kind: ScopeKind) -> u8 {
    match kind {
        ScopeKind::SelfOnly => 0,
        ScopeKind::Dept => 1,
        ScopeKind::Custom | ScopeKind::DeptAndChild => 2,
        ScopeKind::All => 3,
    }
}

/// 校验「操作者能否把这个数据范围授给角色」（范围上限，架构 §2.7 / S14 的第二个入口）。
///
/// # 规则
///
/// | 操作者 | 结果 |
/// |---|---|
/// | 超管（**库侧**判定，不用令牌快照） | 恒 `Ok` |
/// | 范围 = `ALL` 的非超管 | 恒 `Ok`（他的可见集合本来就是全部） |
/// | 其他 | 新范围等级 ≤ 操作者自身等级 **且** ≤ 角色现值等级，见下 |
///
/// # 为什么还要比「角色现值」（`current_scope`）
///
/// 只比操作者等级是不够的：`DEPT → DEPT_AND_CHILD` 在等级上同级（都是部门维度），
/// 但**对持有者本人就是可见范围的放宽**——操作者 `b3_mgr`（本部门 = 研发部）
/// 把自己持有的角色改成「本部门及以下」后，立刻能看到原先看不见的后端组用户。
/// 这是「通过改角色配置给自己扩权」，与直接提权没有区别。
///
/// 但部门类范围的最终集合取决于**持有者**所在部门（B3 阶段没有 `custom_dept_ids`
/// 入参，无法在授权时刻静态判定 N 个持有者各自的子树），因此取更保守、可判定的口径：
///
/// - **非超管（范围 ≠ ALL）不得把任何角色的数据范围放宽**（放宽只能由超管或持
///   「全部数据」范围的管理员执行）；
/// - 收紧（如 `3 → 2`、`2 → 4`）与「不改」一律放行。
///
/// B4 的专用接口（`PUT /system/role/{id}/data-scope`）拿到 `custom_dept_ids` 后，
/// 可再补「逐部门集合包含」的精细判定；那一步不会放宽本处结论。
///
/// # 参数
///
/// - `claims`：当前请求声明（只用来定位用户，**不用** `is_super_admin()` 快照判超管：
///   §2.7 要求按库侧 `role_code` 判定，令牌里的权限快照可能过期）；
/// - `current_scope`：角色**库中现值**；新建角色传 `None`（没有可放宽的基准）；
/// - `new_scope`：待写入的范围（1..5）；
/// - `custom_dept_ids`：`data_scope = 5` 时的自定义部门集合；B3 传空切片（B4 接线）。
///
/// # 异常情况
///
/// - 令牌里取不到用户 ID → `401`（理论上不会发生，属防御）；
/// - 超出上限 → `403 + DATA_SCOPE_EXCEED_LIMIT(61002)`；
/// - 解析范围失败（查库失败）→ `500`。
async fn ensure_data_scope_grantable(
    claims: &TokenClaims,
    current_scope: Option<i16>,
    new_scope: i16,
    custom_dept_ids: &[uuid::Uuid],
) -> std::result::Result<(), ApiError> {
    let user_id = utils::current_user_uuid(claims).ok_or_else(|| {
        ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "当前令牌缺少用户标识，无法校验数据范围上限",
        )
    })?;
    let operator = principal::resolve(&user_id)
        .await
        .map_err(|e| utils::db_error(e, "解析当前用户数据范围"))?;

    ensure_data_scope_grantable_with(&operator, current_scope, new_scope, custom_dept_ids)
}

/// [`ensure_data_scope_grantable`] 的纯逻辑部分（不连库，可单测）。
///
/// 抽出来的原因：规则本身是「等级比较 + 集合包含」，与查库无关；
/// 混在一起就只能靠真库测，而这类提权防护必须能在 `cargo test` 里穷举。
fn ensure_data_scope_grantable_with(
    operator: &principal::ScopedPrincipal,
    current_scope: Option<i16>,
    new_scope: i16,
    custom_dept_ids: &[uuid::Uuid],
) -> std::result::Result<(), ApiError> {
    // 超管不受配置上限约束（架构 §2.7：与批次 A 的 delegation cap 同口径）
    if operator.is_super() {
        return Ok(());
    }
    let new_kind = ScopeKind::from_i16(new_scope)?;
    // 操作者自己的可见集合就是「全部」时，任何部门类范围都不构成扩权
    if operator.kind() == ScopeKind::All {
        return scope_guard::ensure_scope_grant_allowed(operator, custom_dept_ids);
    }
    if scope_class(new_kind) > scope_class(operator.kind()) {
        return Err(ApiError::new(
            403,
            error_code::DATA_SCOPE_EXCEED_LIMIT,
            format!(
                "不能把数据范围设为「{}」：超出你自身的可见范围（当前为「{}」）",
                new_kind.label(),
                operator.kind().label()
            ),
        ));
    }
    if let Some(current) = current_scope {
        let current_kind = ScopeKind::from_i16(current)?;
        if new_kind != current_kind && scope_width(new_kind) >= scope_width(current_kind) {
            return Err(ApiError::new(
                403,
                error_code::DATA_SCOPE_EXCEED_LIMIT,
                format!(
                    "不能把数据范围从「{}」调整为「{}」：非超管只能收紧或保持不变（放宽由超管执行）",
                    current_kind.label(),
                    new_kind.label()
                ),
            ));
        }
    }
    // 逐部门比对（`CUSTOM` 的部门集合必须是自己看得见的那片）
    scope_guard::ensure_scope_grant_allowed(operator, custom_dept_ids)
}

/// 失效「持有该角色的全部用户」的数据范围缓存（角色 `data_scope` 变更后调用）。
///
/// # 说明
///
/// 反查失败只告警不阻断：授权已经落库，缓存脏读最长 300 s 由 TTL 兜底
/// （架构 §2.8「缓存不可用 → 回退查库」的同一取向，这里是「失效失败 → TTL 兜底」）。
async fn invalidate_role_scope_cache(role_id: &uuid::Uuid) {
    match model::find_role_holder_ids(role_id).await {
        Ok(holders) => {
            if holders.is_empty() {
                tracing::debug!(%role_id, "角色无持有者，跳过数据范围缓存失效");
            } else {
                tracing::info!(
                    %role_id,
                    holders = holders.len(),
                    "角色数据范围变更 → 失效持有者的数据范围缓存"
                );
                scope_guard::invalidate_user_scopes(&holders).await;
            }
        }
        Err(e) => {
            tracing::warn!(%role_id, error = %e, "反查角色持有者失败，数据范围缓存靠 TTL 兜底");
        }
    }
}

// ==================== 单元测试（范围上限的纯逻辑） ====================

/// `sys_role` 的审计快照（批次 C · C6）。
///
/// 白名单（`diff::SYS_ROLE.fields`）+ `data_scope`：数据范围本身是**提权面**
/// （批次 B · B3），改了必须留字段级凭证，否则「谁把哪个角色放宽到全部」无从追查。
fn role_snapshot(role: &SysRoleModel) -> serde_json::Value {
    serde_json::json!({
        "role_code": role.role_code,
        "role_name": role.role_name,
        "description": role.description,
        "status": role.status.as_i16(),
        "data_scope": role.data_scope,
        "sort_order": role.sort_order,
        "escalation_role_id": role.escalation_role_id.map(|id| id.to_string()),
        "remark": role.remark,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::shared::principal::ScopedPrincipal;
    use nik::uuid::Uuid;

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn principal(kind: ScopeKind, dept_ids: Vec<Uuid>, self_only: bool, is_super: bool) -> ScopedPrincipal {
        ScopedPrincipal::for_test(
            uid(0xB3_0001),
            dept_ids.first().copied(),
            is_super,
            kind,
            dept_ids,
            self_only,
        )
    }

    /// `resolve_data_scope`：`None` 取兜底值；非法取值 → 61001；合法值原样返回。
    ///
    /// 这条守的是「改角色名不得重置 data_scope」的第一层：兜底值由调用方传入
    /// （更新场景传**库中现值**），函数本身绝不自作主张回退到 1。
    #[test]
    fn data_scope_resolution_keeps_fallback() {
        assert_eq!(resolve_data_scope(None, 3).unwrap(), 3);
        assert_eq!(resolve_data_scope(None, 1).unwrap(), 1);
        assert_eq!(resolve_data_scope(Some(4), 1).unwrap(), 4);
        for bad in [0_i16, 6, -1] {
            let error = resolve_data_scope(Some(bad), 1).expect_err("非法取值必须报错");
            assert_eq!(error.code, error_code::DATA_SCOPE_INVALID);
            assert_eq!(error.status, 400);
        }
        println!("[范围] None→兜底(3)；Some(4)→4；0/6/-1→61001 ✓");
    }

    /// `scope_class` 分级：SELF(0) < 部门类(1) < ALL(2)。
    #[test]
    fn scope_class_ordering() {
        assert_eq!(scope_class(ScopeKind::SelfOnly), 0);
        for kind in [ScopeKind::Dept, ScopeKind::DeptAndChild, ScopeKind::Custom] {
            assert_eq!(scope_class(kind), 1, "{kind:?} 应属部门类");
        }
        assert_eq!(scope_class(ScopeKind::All), 2);
        println!("[上限] 分级 SELF=0 < DEPT/DEPT_AND_CHILD/CUSTOM=1 < ALL=2 ✓");
    }

    /// `scope_width` 分级：SELF(0) < DEPT(1) < CUSTOM/DEPT_AND_CHILD(2) < ALL(3)。
    ///
    /// 这一档与 [`scope_class`] 分开是刻意的：只按 class 比，`DEPT → DEPT_AND_CHILD`
    /// 会被判成「同级不算放宽」，而那正是「改角色配置给自己扩权」的真实路径。
    #[test]
    fn scope_width_ordering() {
        assert_eq!(scope_width(ScopeKind::SelfOnly), 0);
        assert_eq!(scope_width(ScopeKind::Dept), 1);
        assert_eq!(scope_width(ScopeKind::Custom), 2);
        assert_eq!(scope_width(ScopeKind::DeptAndChild), 2);
        assert_eq!(scope_width(ScopeKind::All), 3);
        println!("[宽度] SELF=0 < DEPT=1 < CUSTOM=DEPT_AND_CHILD=2 < ALL=3 ✓");
    }

    /// S14 的授权入口：非超管既不得授出比自身更宽的范围，也不得把角色的范围**放宽**
    /// （含验收用例 ④ 的路径）。
    #[test]
    fn non_super_cannot_escalate_scope() {
        let dept = uid(0xB3_0101);

        // ① 部门类操作者：保持同级 / 收紧 可授；ALL 与「放宽到本部门及以下」→ 403 / 61002
        let dept_manager = principal(ScopeKind::Dept, vec![dept], false, false);
        assert!(
            ensure_data_scope_grantable_with(
                &dept_manager,
                Some(ScopeKind::Dept.as_i16()),
                ScopeKind::Dept.as_i16(),
                &[],
            )
            .is_ok()
        );
        // 收紧：本部门 → 仅本人
        assert!(
            ensure_data_scope_grantable_with(
                &dept_manager,
                Some(ScopeKind::Dept.as_i16()),
                ScopeKind::SelfOnly.as_i16(),
                &[],
            )
            .is_ok()
        );
        // 放宽（同级的本部门 → 本部门及以下，对持有者本人就是扩大可见范围）
        let error = ensure_data_scope_grantable_with(
            &dept_manager,
            Some(ScopeKind::Dept.as_i16()),
            ScopeKind::DeptAndChild.as_i16(),
            &[],
        )
        .expect_err("同级放宽必须被拒");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);
        assert!(error.msg.contains("放宽"), "消息={}", error.msg);

        // 越级到 ALL
        let error = ensure_data_scope_grantable_with(
            &dept_manager,
            Some(ScopeKind::Dept.as_i16()),
            ScopeKind::All.as_i16(),
            &[],
        )
        .expect_err("部门类操作者授出 ALL 必须被拒");
        assert_eq!(error.status, 403);
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);
        assert!(error.msg.contains("全部数据"), "消息={}", error.msg);

        // ② 仅本人操作者：任何部门类都与「自己只能看自己」不符 → 拒
        let self_only = principal(ScopeKind::SelfOnly, vec![], true, false);
        for widen in [
            ScopeKind::Dept,
            ScopeKind::DeptAndChild,
            ScopeKind::Custom,
            ScopeKind::All,
        ] {
            let error =
                ensure_data_scope_grantable_with(&self_only, Some(ScopeKind::SelfOnly.as_i16()), widen.as_i16(), &[])
                    .expect_err("仅本人操作者不得授出更宽范围");
            assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT, "{widen:?}");
        }
        // 收紧（自己 → 自己）允许
        assert!(
            ensure_data_scope_grantable_with(
                &self_only,
                Some(ScopeKind::SelfOnly.as_i16()),
                ScopeKind::SelfOnly.as_i16(),
                &[],
            )
            .is_ok()
        );

        // ③ 超管不受限（放宽也允许）
        let super_admin = principal(ScopeKind::SelfOnly, vec![], true, true);
        assert!(
            ensure_data_scope_grantable_with(
                &super_admin,
                Some(ScopeKind::SelfOnly.as_i16()),
                ScopeKind::All.as_i16(),
                &[],
            )
            .is_ok()
        );

        // ④ 范围 = ALL 的非超管同样不受限（他的可见集合本来就是全部）
        let all_scope = principal(ScopeKind::All, vec![], false, false);
        assert!(
            ensure_data_scope_grantable_with(
                &all_scope,
                Some(ScopeKind::SelfOnly.as_i16()),
                ScopeKind::All.as_i16(),
                &[],
            )
            .is_ok()
        );

        // ⑤ 新建角色（无现值基准）：只看操作者自身等级
        assert!(ensure_data_scope_grantable_with(&dept_manager, None, ScopeKind::DeptAndChild.as_i16(), &[],).is_ok());
        let error = ensure_data_scope_grantable_with(&dept_manager, None, ScopeKind::All.as_i16(), &[])
            .expect_err("新建角色也不得授出 ALL");
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);

        // ⑥ 自定义部门集合越界 → 61002（B4 传 custom_dept_ids 后即生效）
        let intruder = uid(0xB3_0199);
        let error = ensure_data_scope_grantable_with(
            &dept_manager,
            Some(ScopeKind::Custom.as_i16()),
            ScopeKind::Custom.as_i16(),
            &[intruder],
        )
        .expect_err("自定义部门越界必须被拒");
        assert_eq!(error.code, error_code::DATA_SCOPE_EXCEED_LIMIT);
        // 集合内 → 允许
        assert!(
            ensure_data_scope_grantable_with(
                &dept_manager,
                Some(ScopeKind::Custom.as_i16()),
                ScopeKind::Custom.as_i16(),
                &[dept],
            )
            .is_ok()
        );

        println!("[上限] 部门类→ALL 拒；同级放宽拒；SELF→部门类拒；收紧放行；超管与 ALL 放行；自定义部门越界拒 ✓");
    }

    /// 非法 `data_scope` 直接写库前就被拦（不经 `resolve_data_scope` 的路径也必须拦）。
    #[test]
    fn invalid_scope_rejected_by_grant_check() {
        let dept_manager = principal(ScopeKind::Dept, vec![uid(0xB3_0201)], false, false);
        let error = ensure_data_scope_grantable_with(&dept_manager, Some(1), 9, &[]).expect_err("非法范围必须报错");
        assert_eq!(error.code, error_code::DATA_SCOPE_INVALID);
        assert_eq!(error.status, 400);
        println!("[上限] data_scope=9 → 400/61001 ✓");
    }
}

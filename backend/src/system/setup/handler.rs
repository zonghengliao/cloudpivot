//! 首次启动向导 · HTTP 接口（FIX-1 · 新环境开箱即用）。
//!
//! # 接口一览
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/system/setup/check` | `system:setup:read` | 关键角色谁还没人（向导清单） |
//! | POST | `/system/setup/assign` | `system:setup:manage` | 把一批用户加进某个角色（只增不减） |
//! | POST | `/system/setup/demo-users` | `system:setup:manage` | 一键创建演示账号（需 `NIK_SEED_DEMO_USERS=true`） |
//!
//! # 权限口径（卡面 ④「普通用户看到请联系管理员」）
//!
//! 三个接口都挂了权限码：无权限 = **403**，不是「前端把按钮藏起来」。
//! - `system:setup:read`：超管（`*:*:*`）+ 显式授权的管理员；
//! - `system:setup:manage`：指派 / 建演示账号是**改权限**的动作，比只读更严。
//!
//! 普通用户提交单据撞上 `65018` 时看到的是「请联系管理员为该角色指派审批人」——
//! 那句文案由**错误载荷**驱动（前端判断自己有没有 `system:setup:manage`），
//! 与本模块的 403 是同一条口径的两个出口。

use axum::Json;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;
use crate::system::setup::dto;
use crate::system::setup::model;
use crate::utils;

// ==================== 1. 自检 ====================

/// 关键角色自检：哪些角色现在一个人都没有。
///
/// # 返回值
///
/// - `all_ready`：6 个关键角色都至少有一名在职成员；
/// - `missing`：缺人的角色（含「该角色卡住哪条链」「建议用哪个演示账号」）；
/// - `roles`：全部关键角色现状（含已就绪的）；
/// - `demo_users_enabled`：演示账号开关（前端据此决定要不要显示「一键建演示账号」）。
///
/// # 异常情况
///
/// - 500：数据库异常。
#[nik::permission(
    path = "/system/setup/check",
    method = "GET",
    permission = "system:setup:read"
)]
#[nik::get("/system/setup/check")]
pub async fn check() -> ResResult<dto::SetupCheckRes> {
    let roles = model::key_role_status()
        .await
        .map_err(|error| utils::db_error(error, "查询关键角色成员数"))?;
    let missing = roles
        .iter()
        .filter(|role| !role.ready)
        .cloned()
        .collect::<Vec<_>>();
    Ok(Res::ok(dto::SetupCheckRes {
        all_ready: missing.is_empty(),
        demo_users_enabled: model::demo_users_enabled(),
        demo_password_env: model::ENV_DEMO_PASSWORD.to_string(),
        missing,
        roles,
    }))
}

// ==================== 2. 指派成员 ====================

/// 给某个角色指派（追加）成员。
///
/// # 行为
///
/// 1. 解析角色（`role_code` 优先，其次 `role_id`）；
/// 2. 过滤掉不存在 / 已软删的用户；
/// 3. `INSERT ... WHERE NOT EXISTS` 逐条追加（**只增不减**，不动已有的成员与过期策略）；
/// 4. 对被加进去的人**作废旧令牌**（权限快照变了，旧 JWT 里的权限码是错的）；
/// 5. 返回「谁是新加的 / 谁本来就在 / 现在几个人」。
///
/// # 为什么「只增不减」
///
/// 这个接口是**救火**用的（新环境跑不通），不是权限管理页的替代品。
/// 「只增不减」意味着它不可能因为一次误调用把别人的权限收走；要收权限请走
/// `PUT /system/user/{id}/roles`（那里有权限上限、互斥、审计三重保护）。
///
/// # 异常情况
///
/// - 400 + `10001`：角色 / 用户 id 不是 UUID，或一个可用用户都没给；
/// - 404 + `10002`：角色不存在 / 已删除；
/// - 500：数据库异常。
#[nik::permission(
    path = "/system/setup/assign",
    method = "POST",
    permission = "system:setup:manage"
)]
#[nik::post("/system/setup/assign")]
pub async fn assign(Json(req): Json<dto::SetupAssignReq>) -> ResResult<dto::SetupAssignRes> {
    // ① 角色
    let role =
        match (req.role_code.as_deref(), req.role_id.as_deref()) {
            (Some(code), _) if !code.trim().is_empty() => model::find_role_by_code(code.trim())
                .await
                .map_err(|error| utils::db_error(error, "按编码查询角色"))?,
            (_, Some(raw_id)) if !raw_id.trim().is_empty() => {
                let role_id = utils::require_uuid(raw_id, "role_id")?;
                model::find_role_by_id(role_id)
                    .await
                    .map_err(|error| utils::db_error(error, "按 id 查询角色"))?
            }
            _ => None,
        };
    let Some((role_id, role_code, role_name)) = role else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "角色不存在或已删除：请先在「角色管理」里确认角色编码（如 SALES_SUPERVISOR）",
        ));
    };

    // ② 用户（去重 + 过滤已删）
    let mut user_ids: Vec<Uuid> = Vec::with_capacity(req.user_ids.len());
    for raw in &req.user_ids {
        let id = utils::require_uuid(raw, "user_ids")?;
        if !user_ids.contains(&id) {
            user_ids.push(id);
        }
    }
    let existing = model::existing_user_ids(&user_ids)
        .await
        .map_err(|error| utils::db_error(error, "校验用户是否存在"))?;
    if existing.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "没有可用的用户：请从用户列表里选择至少一名在职成员",
        ));
    }

    // ③ 追加绑定（幂等）
    let added = model::add_members(role_id, &existing)
        .await
        .map_err(|error| utils::db_error(error, "为角色追加成员"))?;
    let skipped = existing
        .iter()
        .filter(|id| !added.contains(id))
        .map(|id| id.to_string())
        .collect::<Vec<_>>();

    // ④ 权限变更立即生效：清主体缓存 + 推进令牌版本（旧令牌里的权限码已过期）
    for user_id in &existing {
        crate::system::auth::model::invalidate_user_principal(user_id).await;
        crate::business::shared::guard::invalidate_user_scope(user_id).await;
        if let Err(error) = crate::system::user::model::bump_token_version(user_id).await {
            // 失效失败不该让「已经加进角色」这件事回滚：下一次登录自然拿到新权限
            tracing::warn!(user_id = %user_id, error = %error, "指派成员后推进令牌版本失败");
        }
    }

    let member_count = model::member_count(role_id)
        .await
        .map_err(|error| utils::db_error(error, "统计角色成员数"))?;

    tracing::info!(
        role_code = %role_code,
        added = added.len(),
        skipped = skipped.len(),
        member_count,
        "首次启动向导：已为角色指派成员"
    );

    Ok(Res::ok(dto::SetupAssignRes {
        role_code,
        role_name,
        added: added.iter().map(|id| id.to_string()).collect(),
        skipped,
        member_count,
    }))
}

// ==================== 3. 一键建演示账号 ====================

/// 一键创建演示账号（开发 / 测试环境）。
///
/// # 前置条件
///
/// `NIK_SEED_DEMO_USERS=true` —— 开关没开时返回 **409**（不是静默成功），
/// 免得有人以为「点了就算建了」。
///
/// # 幂等
///
/// 已存在的账号整条跳过（不覆盖密码 / 姓名 / 部门），角色绑定只增不减。
///
/// # 异常情况
///
/// - 409 + `10003`：演示账号开关未打开；
/// - 500：口令哈希或数据库异常。
#[nik::permission(
    path = "/system/setup/demo-users",
    method = "POST",
    permission = "system:setup:manage"
)]
#[nik::post("/system/setup/demo-users")]
pub async fn demo_users(
    Json(_req): Json<dto::SetupDemoUsersReq>,
) -> ResResult<dto::SetupDemoUsersRes> {
    if !model::demo_users_enabled() {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "演示账号开关未打开（{}）：请在启动环境里显式设置后再重试",
                model::ENV_SEED_DEMO_USERS
            ),
        ));
    }
    let outcome = model::seed_demo_users(&model::demo_password())
        .await
        .map_err(|error| utils::db_error(error, "创建演示账号"))?;
    Ok(Res::ok(dto::SetupDemoUsersRes {
        enabled: outcome.enabled,
        created: outcome.created,
        existed: outcome.existed,
        password_env: model::ENV_DEMO_PASSWORD.to_string(),
        bindings_added: outcome.bindings_added,
    }))
}

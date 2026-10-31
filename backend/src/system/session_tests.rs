//! FIX-05 / BUG-SYS-04 回归防线：**权限撤销后旧令牌必须立刻 401**。
//!
//! # 被保护的行为（卡面验收 ①②③④⑤⑥⑦）
//!
//! | 用例 | 触发场景 | 断言 |
//! |------|----------|------|
//! | S01 | 判定契约 | `ver` 不等 → 401 + `20008` +「您的权限已变更，请重新登录」；相等放行 |
//! | S02 | **改用户角色**（`PUT /system/user/{id}/roles`） | 目标用户 `token_version` +1，旧 `ver` 立刻判过期，新 `ver` 通过 |
//! | S03 | **改角色权限**（`PUT /system/role/{id}/menus`） | 角色持有者 +1，无关用户不变 |
//! | S04 | **停用用户**（`PATCH /system/user/{id}/status`） | 版本 +1 且状态落库为停用 |
//! | S05 | **管理员重置密码**（`PATCH /system/user/{id}/password`） | 版本 +1、置首登改密 |
//! | S06 | 无 Redis 降级 | 无缓存后端时读到的就是库里的当前版本（没有旧值残留） |
//! | S07 | 删除用户 | 会话字段读不到 → 守卫 401（不是放行） |
//! | S08 | 超管豁免的边界 | 推进角色持有者时不动内置管理员，其他角色持有者一律被动到 |
//!
//! # 为什么直接调 handler
//!
//! `#[nik::permission]` 的路由闸在中间件层，用例绕不过去；但**版本号推进发生在
//! handler 的业务分支里**（事务提交之后的 `revoke_user_tokens` / 模型层 SQL），
//! 直接调 handler 是唯一能证明「这条触发真的接上了」的方式 —— 只调模型层函数等于
//! 自证假设。判定侧用与中间件**同一份**实现（[`crate::middleware::session_guard::version_is_current`]
//! 与 [`crate::middleware::session_guard::unauthorized_with`]），不会出现「测的是一份、
//! 跑的是另一份」。
//!
//! 真实库用例要求 `SYS_DB_URL`（未设置时整组 SKIP，与 O 系列同一约定）。

use axum::extract::{Extension, Path};
use nik::prelude::*;

use crate::middleware::session_guard::{unauthorized_with, version_is_current};
use crate::shared::{constants, error_code, session_cache};
use crate::system::role::dto::RoleMenuAssignReq;
use crate::system::tests_support as ts;
use crate::system::user::dto::{UserPasswordReq, UserRoleAssignReq, UserStatusReq};
use crate::system::user::handler as user_handler;

// ==================== 夹具 ====================

/// 读一个用户的 `sys_user.token_version`。
async fn version_of(user_id: &uuid::Uuid) -> i64 {
    ts::scalar_i64(
        "SELECT token_version AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await
}

/// 内置管理员的 UUID（守卫里的权限上限会先查「操作者账号是否存在 / 是否超管」，
/// 因此测试里的操作者必须是**库里真实存在的** admin，不能是随手编的 sub）。
async fn admin_id() -> uuid::Uuid {
    ts::scalar_text(
        "SELECT id::text AS v FROM sys_user WHERE username = ?",
        vec![Value::from(constants::ADMIN_USERNAME)],
    )
    .await
    .expect("库里必须有内置管理员 admin（隔离库要先起一次实例让 bootstrap 种数据）")
    .parse()
    .expect("admin id 应是 UUID")
}

/// 造一份「带令牌版本号」的操作者声明（内置管理员，`*:*:*`）。
async fn admin_claims(ver: i64) -> AuthClaims {
    let mut claims = ts::super_claims();
    claims.0.sub = admin_id().await.to_string();
    claims.0.extra.insert(
        constants::CLAIM_TOKEN_VERSION.to_string(),
        serde_json::json!(ver),
    );
    claims
}

/// 造一个夹具用户（真库行，密码是合法哈希，可被 `change_password` 覆盖）。
async fn create_user(username: &str) -> uuid::Uuid {
    let user_id = uuid::Uuid::now_v7();
    let hash = Crypto::hash_password("Passw0rd@fix05").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, ?, 'FIX05 用户', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.to_string()),
            Value::from(hash),
        ],
    )
    .await;
    user_id
}

/// 造一个夹具角色。
async fn create_role(role_code: &str, role_name: &str, data_scope: i16) -> uuid::Uuid {
    let role_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::text::uuid, ?, ?, 1, ?)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(role_code.to_string()),
            Value::from(role_name.to_string()),
            Value::from(i64::from(data_scope)),
        ],
    )
    .await;
    role_id
}

/// 造一个夹具按钮菜单（权限点）。
async fn create_menu(name: &str, auth_code: &str) -> uuid::Uuid {
    let menu_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_menu (id, name, menu_type, auth_code, title, sort_order, status) \
         VALUES (?::text::uuid, ?, 3, ?, ?, 0, 1)",
        vec![
            Value::from(menu_id.to_string()),
            Value::from(name.to_string()),
            Value::from(auth_code.to_string()),
            Value::from(name.to_string()),
        ],
    )
    .await;
    menu_id
}

/// 给用户绑一个角色。
async fn bind_role(user_id: &uuid::Uuid, role_id: &uuid::Uuid) {
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(role_id.to_string()),
        ],
    )
    .await;
}

/// 清理一整套夹具（按外键依赖顺序：授权绑定 → 实体）。
async fn cleanup(prefix: &str, extra_roles: &[uuid::Uuid], extra_menus: &[uuid::Uuid]) {
    ts::cleanup_sql(
        &format!("FIX05→sys_user_role({prefix})"),
        "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE ?) \
         OR role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
        vec![
            Value::from(format!("{prefix}%")),
            Value::from(format!("{prefix}%")),
        ],
    )
    .await;
    ts::cleanup_sql(
        &format!("FIX05→sys_role_menu({prefix})"),
        "DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
        vec![Value::from(format!("{prefix}%"))],
    )
    .await;
    for role_id in extra_roles {
        ts::cleanup_sql(
            &format!("FIX05→sys_role({role_id})"),
            "DELETE FROM sys_role WHERE id = ?::text::uuid",
            vec![Value::from(role_id.to_string())],
        )
        .await;
    }
    for menu_id in extra_menus {
        ts::cleanup_sql(
            &format!("FIX05→sys_menu({menu_id})"),
            "DELETE FROM sys_menu WHERE id = ?::text::uuid",
            vec![Value::from(menu_id.to_string())],
        )
        .await;
    }
    ts::cleanup_table("sys_role", "role_code", prefix).await;
    ts::cleanup_table("sys_menu", "name", prefix).await;
    let users = ts::sweep_users(prefix).await;
    if !users.is_clean() {
        println!("WARN[cleanup] FIX05 用户清理未完全成功：{:?}", users.failed);
    }
}

// ==================== S01 判定契约（纯逻辑，不连库） ====================

/// S01：版本号判定 + 401 响应体的**契约**（卡面 ③ 的文案与码）。
///
/// 中间件用 [`version_is_current`] 判「令牌是否过期」，用 [`unauthorized_with`] 造响应；
/// 这里钉死这两个出口，避免后续有人把码改回 `401` 或把文案改掉（前端按码跳登录页）。
#[tokio::test]
async fn s01_version_mismatch_returns_20008() {
    assert!(
        version_is_current(0, 0),
        "0 == 0 应放行（整改前的旧令牌平滑过渡）"
    );
    assert!(version_is_current(7, 7), "相等必须放行");
    assert!(!version_is_current(6, 7), "旧版本必须被判过期");
    assert!(!version_is_current(7, 6), "库比令牌新也必须判过期");

    let response = unauthorized_with(
        error_code::AUTH_PERMISSION_CHANGED,
        "您的权限已变更，请重新登录",
    );
    assert_eq!(
        response.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "权限变更必须是 401"
    );
    let body = axum::body::to_bytes(response.into_body(), 8 * 1024)
        .await
        .expect("读响应体失败");
    let parsed: serde_json::Value = serde_json::from_slice(&body).expect("响应体不是 JSON");
    assert_eq!(
        parsed["code"],
        serde_json::json!(error_code::AUTH_PERMISSION_CHANGED),
        "业务码必须是 20008（AUTH_PERMISSION_CHANGED）"
    );
    assert_eq!(
        parsed["msg"],
        serde_json::json!("您的权限已变更，请重新登录"),
        "文案必须与卡面一致（前端据此提示并跳登录页）"
    );
    println!("[S01] 版本不符 → 401/20008 +「您的权限已变更，请重新登录」✓");
}

// ==================== S02 改用户角色 ====================

/// S02：**改用户角色**后旧令牌立刻失效（卡面验收 ①②③④）。
#[tokio::test]
async fn s02_assign_roles_invalidates_old_token() {
    if !ts::ensure_db().await {
        return;
    }
    let prefix = format!("FIX05S02_{}", ts::uniq());
    let role_id = create_role(&format!("{prefix}_ROLE"), "FIX05-S02 角色", 1).await;
    let user_id = create_user(&format!("{prefix}_u")).await;

    // ① 用户拿到令牌：`ver` = 登录时刻库里的版本号
    let ver_login = version_of(&user_id).await;
    assert!(
        version_is_current(ver_login, ver_login as i32),
        "刚登录时令牌版本应与库里一致"
    );

    // ② 管理员撤掉（这里：第一次授予再撤掉，两条路径都会被走到）该用户的角色
    let admin = admin_claims(0).await;
    user_handler::assign_roles(
        Path(user_id.to_string()),
        ts::headers(),
        ts::connect_info(),
        Extension(admin.clone()),
        ValidatedJson(UserRoleAssignReq {
            role_ids: vec![role_id.to_string()],
        }),
    )
    .await
    .expect("分配角色应成功");
    let ver_after_grant = version_of(&user_id).await;
    assert!(
        ver_after_grant > ver_login,
        "授予角色也必须推进版本号（{ver_login} → {ver_after_grant}）"
    );

    user_handler::assign_roles(
        Path(user_id.to_string()),
        ts::headers(),
        ts::connect_info(),
        Extension(admin),
        ValidatedJson(UserRoleAssignReq {
            role_ids: Vec::new(),
        }),
    )
    .await
    .expect("撤销角色应成功");
    let ver_after_revoke = version_of(&user_id).await;
    assert!(
        ver_after_revoke > ver_after_grant,
        "撤销角色必须再推进一次（{ver_after_grant} → {ver_after_revoke}）"
    );

    // ③ 旧令牌（ver_login / ver_after_grant）一律被判过期 → 中间件回 401 + 20008
    assert!(
        !version_is_current(ver_login, ver_after_revoke as i32),
        "撤销角色后，登录时那张令牌必须失效"
    );
    // ④ 重新登录：新令牌带新版本号 → 放行
    assert!(
        version_is_current(ver_after_revoke, ver_after_revoke as i32),
        "重新登录拿到的新令牌必须可用"
    );
    // 缓存侧也不能留下旧值（无 Redis 时这里就是直查库）
    let flags = session_cache::load_session_flags(&user_id)
        .await
        .expect("读会话字段失败")
        .expect("用户应存在");
    assert_eq!(
        i64::from(flags.0),
        ver_after_revoke,
        "会话字段读到的必须是最新版本号（没有旧值残留）"
    );

    println!(
        "[S02] 改角色：{ver_login} → {ver_after_grant} → {ver_after_revoke}，旧令牌 ver={ver_login} 已作废 ✓"
    );
    cleanup(&prefix, &[role_id], &[]).await;
}

// ==================== S03 改角色权限 ====================

/// S03：**改角色权限**（`PUT /system/role/{id}/menus`）也要触发（卡面验收 ⑤）——
/// 这里改的不是「用户有哪些角色」，而是「角色有哪些权限」。
#[tokio::test]
async fn s03_assign_role_menus_invalidates_holders() {
    if !ts::ensure_db().await {
        return;
    }
    let prefix = format!("FIX05S03_{}", ts::uniq());
    let role_id = create_role(&format!("{prefix}_ROLE"), "FIX05-S03 角色", 1).await;
    let holder = create_user(&format!("{prefix}_u")).await;
    let bystander = create_user(&format!("{prefix}_b")).await;
    bind_role(&holder, &role_id).await;
    let menu_id = create_menu(&format!("{prefix}-按钮"), &format!("{prefix}:btn")).await;

    let holder_before = version_of(&holder).await;
    let bystander_before = version_of(&bystander).await;
    assert_eq!(holder_before, bystander_before, "夹具起点应同为 0");

    crate::system::role::handler::assign_menus(
        axum::extract::Path(role_id.to_string()),
        ts::headers(),
        ts::connect_info(),
        Extension(admin_claims(0).await),
        ValidatedJson(RoleMenuAssignReq {
            menu_ids: vec![menu_id.to_string()],
        }),
    )
    .await
    .expect("角色授权菜单应成功");

    let holder_after = version_of(&holder).await;
    let bystander_after = version_of(&bystander).await;
    assert!(
        holder_after > holder_before,
        "角色获得新权限后，持有者的旧令牌必须作废（{holder_before} → {holder_after}）"
    );
    assert_eq!(
        bystander_after, bystander_before,
        "没有该角色的用户不该被牵动（{bystander_before} → {bystander_after}）"
    );
    let granted = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role_menu WHERE role_id = ?::text::uuid AND menu_id = ?::text::uuid",
        vec![
            Value::from(role_id.to_string()),
            Value::from(menu_id.to_string()),
        ],
    )
    .await;
    assert_eq!(granted, 1, "授权绑定必须真的落库");
    println!(
        "[S03] 改角色权限：持有者 {holder_before} → {holder_after}，无关用户保持 {bystander_after} ✓"
    );
    cleanup(&prefix, &[role_id], &[menu_id]).await;
}

// ==================== S04 停用用户 ====================

/// S04：**停用用户**必须推进版本号（卡面触发场景 3）。
#[tokio::test]
async fn s04_disable_user_invalidates_token() {
    if !ts::ensure_db().await {
        return;
    }
    let prefix = format!("FIX05S04_{}", ts::uniq());
    let user_id = create_user(&format!("{prefix}_u")).await;
    let ver_before = version_of(&user_id).await;

    user_handler::change_status(
        Path(user_id.to_string()),
        ts::headers(),
        ts::connect_info(),
        Extension(admin_claims(0).await),
        ValidatedJson(UserStatusReq { status: 0 }),
    )
    .await
    .expect("停用用户应成功");

    let ver_after = version_of(&user_id).await;
    assert!(
        ver_after > ver_before,
        "停用必须推进版本号（{ver_before} → {ver_after}），否则旧令牌能一直用到过期"
    );
    assert!(
        !version_is_current(ver_before, ver_after as i32),
        "停用后旧令牌必须失效"
    );
    let status = ts::scalar_i64(
        "SELECT status AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await;
    assert_eq!(status, 0, "状态必须真的落库为停用");
    println!("[S04] 停用：token_version {ver_before} → {ver_after}，旧令牌已作废 ✓");
    cleanup(&prefix, &[], &[]).await;
}

// ==================== S05 重置密码 ====================

/// S05：**管理员重置口令**必须推进版本号（卡面触发场景 4）。
#[tokio::test]
async fn s05_reset_password_invalidates_token() {
    if !ts::ensure_db().await {
        return;
    }
    let prefix = format!("FIX05S05_{}", ts::uniq());
    let user_id = create_user(&format!("{prefix}_u")).await;
    let ver_before = version_of(&user_id).await;

    user_handler::change_password(
        ts::headers(),
        ts::connect_info(),
        Extension(admin_claims(0).await),
        Path(user_id.to_string()),
        ValidatedJson(UserPasswordReq {
            password: "Reset@fix05".to_string(),
        }),
    )
    .await
    .expect("管理员重置口令应成功");

    let ver_after = version_of(&user_id).await;
    assert!(
        ver_after > ver_before,
        "改密必须推进版本号（{ver_before} → {ver_after}）"
    );
    assert!(
        !version_is_current(ver_before, ver_after as i32),
        "改密后旧令牌必须失效"
    );
    let must_change = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user WHERE id = ?::text::uuid AND must_change_password",
        vec![Value::from(user_id.to_string())],
    )
    .await;
    assert_eq!(must_change, 1, "管理员代改的口令必须置「首登改密」");
    println!("[S05] 重置口令：token_version {ver_before} → {ver_after}，旧令牌已作废 ✓");
    cleanup(&prefix, &[], &[]).await;
}

// ==================== S06 无 Redis 降级 ====================

/// S06：**无 Redis 时也生效**（卡面验收 ⑦）。
///
/// lib 单测不跑 `App::start()`，因此 `try_cache()` 恒为 `None` —— 这正是「没有缓存」的
/// 形态。断言：会话字段读到的永远是**库里的当前值**（没有旧值残留），失效动作也不会
/// 把闸门误标成「欠账」。
#[tokio::test]
async fn s06_degrades_to_db_without_cache() {
    if !ts::ensure_db().await {
        return;
    }
    assert!(
        try_cache().is_none(),
        "lib 单测环境不应注册缓存（若变红，说明有用例注册了全局缓存，本用例的语义要重写）"
    );
    let prefix = format!("FIX05S06_{}", ts::uniq());
    let user_id = create_user(&format!("{prefix}_u")).await;

    let first = session_cache::load_session_flags(&user_id)
        .await
        .expect("读会话字段失败")
        .expect("用户应存在");
    assert_eq!(first.0, 0, "新用户版本号应为 0");

    // 直接推进库里的版本号（模拟另一个实例 / 手工运维改库）
    ts::exec(
        "UPDATE sys_user SET token_version = token_version + 5 WHERE id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await;
    let second = session_cache::load_session_flags(&user_id)
        .await
        .expect("读会话字段失败")
        .expect("用户应存在");
    assert_eq!(
        second.0, 5,
        "没有缓存时必须立刻读到库里的新版本号（无窗口）"
    );

    session_cache::invalidate(&user_id).await;
    session_cache::invalidate_all().await;
    println!("[S06] 无 Redis：逐步读到库里的版本号 0 → 5，无旧值窗口 ✓");
    cleanup(&prefix, &[], &[]).await;
}

// ==================== S07 删除用户 ====================

/// S07：**删除用户**后会话字段读不到 → 守卫必须 401（不是放行）。
#[tokio::test]
async fn s07_deleted_user_has_no_session() {
    if !ts::ensure_db().await {
        return;
    }
    let prefix = format!("FIX05S07_{}", ts::uniq());
    let user_id = create_user(&format!("{prefix}_u")).await;
    ts::exec(
        "UPDATE sys_user SET deleted_at = now() WHERE id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await;
    let flags = session_cache::load_session_flags(&user_id)
        .await
        .expect("读会话字段失败");
    assert!(
        flags.is_none(),
        "已删除用户必须读不到会话字段（守卫据此 401「账号不存在或已被删除」）"
    );
    println!("[S07] 删除用户：会话字段 None → 守卫 401 ✓");
    cleanup(&prefix, &[], &[]).await;
}

// ==================== S08 超管豁免的边界 ====================

/// S08：**超管改自己权限不影响其他用户**（卡面验收 ⑥）——反过来说：
/// 推进「角色持有者」时，内置管理员被显式豁免（超管走通配短路，不依赖令牌快照），
/// 而**其他**持有者必须一个不落地被推进。
#[tokio::test]
async fn s08_super_admin_exempt_but_other_holders_bumped() {
    if !ts::ensure_db().await {
        return;
    }
    let prefix = format!("FIX05S08_{}", ts::uniq());
    let role_id = create_role(&format!("{prefix}_ROLE"), "FIX05-S08 角色", 1).await;
    let holder = create_user(&format!("{prefix}_u")).await;
    let admin = admin_id().await;
    bind_role(&holder, &role_id).await;
    // 内置管理员也持有这个角色：改「这个角色」的授权 = 改超管自己的权限
    bind_role(&admin, &role_id).await;

    let holder_before = version_of(&holder).await;
    let admin_before = version_of(&admin).await;

    let bumped = crate::system::user::model::bump_token_version_of_role_holders_in(
        db(),
        role_id,
        constants::ADMIN_USERNAME,
    )
    .await
    .expect("批量推进版本号失败");
    assert_eq!(bumped, 1, "只应推进非超管的 1 个持有者（实际 {bumped}）");

    let holder_after = version_of(&holder).await;
    let admin_after = version_of(&admin).await;
    assert_eq!(
        holder_after,
        holder_before + 1,
        "普通持有者必须被推进（{holder_before} → {holder_after}）"
    );
    assert_eq!(
        admin_after, admin_before,
        "内置管理员豁免（超管走通配短路，推进它等于把管理员踢下线）"
    );
    println!("[S08] 超管豁免：持有者 {holder_before} → {holder_after}，admin 保持 {admin_after} ✓");

    // 收尾：先解掉给 admin 绑的夹具角色（不动 admin 自己原有角色）
    ts::cleanup_sql(
        &format!("FIX05→sys_user_role(admin,{role_id})"),
        "DELETE FROM sys_user_role WHERE user_id = ?::text::uuid AND role_id = ?::text::uuid",
        vec![
            Value::from(admin.to_string()),
            Value::from(role_id.to_string()),
        ],
    )
    .await;
    cleanup(&prefix, &[role_id], &[]).await;
}

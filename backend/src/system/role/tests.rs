//! 测试C · P1-2：`system/role` 用例集（R01–R06）。
//!
//! # 覆盖重点
//!
//! 角色是权限的「容器」，它的三条不变量一旦破了就是权限泄漏：
//!
//! 1. 角色授权变更必须**立刻**影响在线用户（否则撤权后旧令牌还能用）；
//! 2. 有用户的角色不能删（否则用户的授权悬空）；
//! 3. 内置超管角色不能删、也不能被非超管改授权（否则等于公开改超管权限）；
//! 4. 互斥角色不能双持（职责分离）。
//!
//! # 环境说明（BUG-SYS-03 已修，2026-10-31）
//!
//! 本验证库**没有 Redis**。修复前：`assign_menus` 落库后的「缓存侧强制下线」会失败 →
//! handler fail-closed 回 500（`会话吊销失败`），而授权其实已经写进库 → 语义矛盾、客户端
//! 重试不幂等。修复后：同一事务内推进持有者的 `sys_user.token_version`（库侧兜底，
//! `middleware::session_guard` 逐请求比对），缓存侧清理降级为 warn 日志，因此
//! **无 Redis 也回 200**，用例直接断言成功。
//!
//! 跑法：
//! ```bash
//! SYS_DB_URL="postgres://<user>:***@127.0.0.1:5432/cloudpivot_test" \
//!   cargo test --lib system::role::tests -- --nocapture
//! ```

use nik::prelude::*;

use crate::shared::constants;
use crate::shared::error_code;
use crate::system::role::dto::{RoleMenuAssignReq, RoleSaveReq};
use crate::system::role::handler;
use crate::system::tests_support as ts;

/// 夹具前缀。
const PREFIX: &str = "tc_r";

/// 清扫本卡历史残留。
///
/// # 为什么不再 `let _ =`
///
/// 之前每一步都是 `let _ = db().execute_with_params(...)`：删除失败被静默吞掉，
/// 结果 `system::` 每跑一遍就往 `cloudpivot_test` 里攒 30+ 行 `tc_r` 夹具（用户软删
/// 行为主）。现在：
///
/// - 依赖行（`sys_role_menu` → `sys_user_role` → `sys_role_mutex` → `sys_role`）
///   走 [`ts::cleanup_sql`]，失败打 `WARN[cleanup]` + SQL + 数据库原文；
/// - 实体行走 [`ts::sweep_users`] / [`ts::sweep_roles`]，**逐行**删：
///   整批 DELETE 只要有一行被业务表外键挡住就会整条回滚，一个脏行连累全部夹具；
///   逐行删把影响面收敛到那一行，并给自检用例留下可断言的明细。
async fn sweep() {
    for (label, sql, arity) in [
        (
            "sys_role_menu",
            "DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
            1,
        ),
        (
            "sys_user_role",
            "DELETE FROM sys_user_role WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
            1,
        ),
        (
            "sys_role_mutex",
            "DELETE FROM sys_role_mutex WHERE role_a_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?) \
             OR role_b_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
            2,
        ),
    ] {
        ts::cleanup_sql(
            &format!("role/sweep→{label}(LIKE '{PREFIX}%')"),
            sql,
            vec![Value::from(format!("{PREFIX}%")); arity],
        )
        .await;
    }

    let roles = ts::sweep_roles(PREFIX).await;
    let users = ts::sweep_users(PREFIX).await;
    if !roles.is_clean() || !users.is_clean() {
        println!(
            "WARN[cleanup] role/sweep 未完全成功：{:?} {:?}",
            roles.failed, users.failed
        );
    }

    // 本卡的菜单夹具也是 `tc_r` 前缀（R01/R02 的授权菜单、R07 的按钮），
    // 之前只按角色/用户清、漏了菜单——每跑一遍就留 1 行 `tc_r_r07_...-按钮`。
    let menus = ts::sweep_menus(PREFIX).await;
    if !menus.is_clean() {
        println!(
            "WARN[cleanup] role/sweep 菜单清理未完全成功：{:?}",
            menus.failed
        );
    }
}

/// 造一个「库里真实持有 SUPER_ADMIN」的操作者声明。
async fn super_operator() -> (uuid::Uuid, AuthClaims) {
    let username = format!("{PREFIX}_op_{}", ts::uniq());
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Operator@123456").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, ?, '角色测试操作者', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.clone()),
            Value::from(hash),
        ],
    )
    .await;
    let role = ts::scalar_text(
        "SELECT id::text AS v FROM sys_role WHERE role_code = ? AND deleted_at IS NULL",
        vec![Value::from(constants::SUPER_ADMIN_ROLE_CODE)],
    )
    .await
    .expect("库里必须有 SUPER_ADMIN 角色");
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(user_id.to_string()), Value::from(role)],
    )
    .await;
    (
        user_id,
        ts::claims(&user_id.to_string(), &username, &["*:*:*"]),
    )
}

/// 角色请求体。
fn role_req(code: &str, name: &str) -> RoleSaveReq {
    RoleSaveReq {
        role_code: code.to_string(),
        role_name: name.to_string(),
        description: Some("测试C".to_string()),
        status: Some(1),
        data_scope: Some(2),
        sort_order: Some(0),
        remark: None,
    }
}

/// 用真实 handler 建角色，返回角色 ID。
async fn create_role(claims: &AuthClaims, code: &str, name: &str) -> String {
    let created = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson(role_req(code, name)),
    )
    .await
    .unwrap_or_else(|e| panic!("建角色 {code} 失败：{}/{} {}", e.status, e.code, e.msg));
    created.data.expect("create 必须返回 data").id
}

/// 建一个菜单（按钮），返回 ID —— 用于验证「角色授权」链路。
async fn create_menu(claims: &AuthClaims, name: &str, auth_code: &str) -> String {
    use crate::system::menu::dto::MenuCreateReq;
    let created = crate::system::menu::handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson(MenuCreateReq {
            name: name.to_string(),
            menu_type: 3,
            auth_code: auth_code.to_string(),
            path: None,
            parent_id: None,
            component: None,
            sort_order: Some(0),
            status: Some(1),
            icon: None,
            title: Some(name.to_string()),
            is_frame: None,
            is_cache: None,
            is_visible: None,
            hidden_expression: None,
            module_code: Some("testc".to_string()),
        }),
    )
    .await
    .expect("建菜单失败");
    created.data.expect("create 必须返回 data").id
}

/// 授权菜单（**必须成功**）。
///
/// BUG-SYS-03 修复前：本验证库没有 Redis，`force_logout_all_except` 读不到在线会话表 →
/// handler fail-closed 回 500（授权却已落库），helper 因此只放行那一种 500。修复后
/// 「旧令牌失效」下沉到库侧（同事务推进持有者的 `sys_user.token_version`），缓存侧
/// 清理降级为告警，任何错误都是真缺陷。
async fn assign_menus(claims: &AuthClaims, role_id: &str, menu_ids: &[String]) {
    if let Err(error) = handler::assign_menus(
        Path(role_id.to_string()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson(RoleMenuAssignReq {
            menu_ids: menu_ids.to_vec(),
        }),
    )
    .await
    {
        panic!(
            "授权菜单不应失败（BUG-SYS-03 已修），实际 {}/{} {}",
            error.status, error.code, error.msg
        );
    }
}

/// 清理（本次用例的夹具）。
async fn cleanup() {
    sweep().await;
}

// ==================== R01 建角色 → 分配权限 ====================

/// R01：新建角色并授权菜单 → `sys_role_menu` 落库、`GET /role/{id}/menus` 反向读得出来。
#[tokio::test]
async fn r01_create_role_and_assign_menus() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let tag = ts::uniq();
    let code = format!("{PREFIX}_{tag}");
    let role_id = create_role(&claims, &code, "测试C 角色").await;

    let menu_a = create_menu(&claims, &format!("{code}-按钮A"), &format!("testc:{tag}:a")).await;
    let menu_b = create_menu(&claims, &format!("{code}-按钮B"), &format!("testc:{tag}:b")).await;
    assign_menus(&claims, &role_id, &[menu_a.clone(), menu_b.clone()]).await;

    let rows = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role_menu WHERE role_id = ?::text::uuid",
        vec![Value::from(role_id.clone())],
    )
    .await;
    assert_eq!(rows, 2, "两条菜单授权都应落库，实际 {rows}");

    // 反查接口：角色详情里的菜单 id 列表与授权一致
    let menus = handler::menus(Path(role_id.clone()))
        .await
        .expect("查角色菜单失败")
        .data
        .expect("menus 必须返回 data");
    assert_eq!(menus.len(), 2, "反查应得到 2 条授权");
    assert!(menus.contains(&menu_a) && menus.contains(&menu_b));

    // 收尾：先 `sys_role_menu`（引用 role + menu），再菜单行；失败打 WARN 不吞
    ts::cleanup_sql(
        &format!("R01→sys_role_menu({menu_a},{menu_b})"),
        "DELETE FROM sys_role_menu WHERE menu_id IN (?::text::uuid, ?::text::uuid)",
        vec![Value::from(menu_a), Value::from(menu_b)],
    )
    .await;
    let menus = ts::sweep_menus(&format!("{code}-")).await;
    if !menus.is_clean() {
        println!("WARN[cleanup] R01 菜单清理未完全成功：{:?}", menus.failed);
    }
    println!("[R01] 建角色 + 授权 2 条菜单，落库与反查一致 ✓");
    cleanup().await;
}

// ==================== R02 授权变更 → 在线用户立即生效 ====================

/// R02：角色授权变更后，**该角色持有的用户**权限立即变化（不需要用户重新登录/重启）。
///
/// 实际机制（按实现断言，不按猜测）：`assign_menus` 会
/// ①清主体缓存（`invalidate_role_holders`）②强制非超管在线用户下线（旧令牌失效）。
/// 因此「立即生效」的表现是：库侧权限重算立刻变化 + 旧令牌被吊销。
#[tokio::test]
async fn r02_role_permission_change_takes_effect_immediately() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let tag = ts::uniq();
    let code = format!("{PREFIX}_r02_{tag}");
    let role_id = create_role(&claims, &code, "测试C R02 角色").await;

    // 持该角色的用户
    let username = format!("{PREFIX}_r02_u_{tag}");
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Passw0rd@2").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, ?, 'R02 用户', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.clone()),
            Value::from(hash),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(role_id.clone()),
        ],
    )
    .await;

    let grant_auth = format!("testc:{tag}:grant");
    let revoke_auth = format!("testc:{tag}:revoke");
    let menu_grant = create_menu(&claims, &format!("{code}-授予"), &grant_auth).await;
    let menu_revoke = create_menu(&claims, &format!("{code}-撤销"), &revoke_auth).await;

    // 先授予「revoke」这条（之后撤掉它）
    assign_menus(&claims, &role_id, std::slice::from_ref(&menu_revoke)).await;
    let before = crate::system::auth::model::load_permissions(&user_id, false)
        .await
        .expect("装载权限失败");
    assert!(
        before.contains(&revoke_auth),
        "授权后该用户应立刻拿到 {revoke_auth}，实际 {before:?}"
    );

    // 再换成「grant」
    assign_menus(&claims, &role_id, std::slice::from_ref(&menu_grant)).await;
    let after = crate::system::auth::model::load_permissions(&user_id, false)
        .await
        .expect("装载权限失败");
    assert!(
        after.contains(&grant_auth),
        "新授权必须立刻生效，实际 {after:?}"
    );
    assert!(
        !after.contains(&revoke_auth),
        "被撤销的权限必须立刻消失，实际 {after:?}"
    );

    // 收尾：先 `sys_role_menu`（引用 role + menu），再菜单行；失败打 WARN 不吞
    ts::cleanup_sql(
        &format!("R02→sys_role_menu({menu_grant},{menu_revoke})"),
        "DELETE FROM sys_role_menu WHERE menu_id IN (?::text::uuid, ?::text::uuid)",
        vec![Value::from(menu_grant), Value::from(menu_revoke)],
    )
    .await;
    let menus = ts::sweep_menus(&format!("{code}-")).await;
    if !menus.is_clean() {
        println!("WARN[cleanup] R02 菜单清理未完全成功：{:?}", menus.failed);
    }
    println!("[R02] 授权变更后权限重算立即反映（{revoke_auth} → {grant_auth}）✓");
    cleanup().await;
}

// ==================== R03 有用户的角色不能删 ====================

/// R03：角色已分配给用户 → 删除 409 `ROLE_IN_USE`，角色仍在。
#[tokio::test]
async fn r03_role_in_use_cannot_be_deleted() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let tag = ts::uniq();
    let code = format!("{PREFIX}_r03_{tag}");
    let role_id = create_role(&claims, &code, "测试C R03 角色").await;

    let username = format!("{PREFIX}_r03_u_{tag}");
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Passw0rd@3").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, ?, 'R03 用户', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username),
            Value::from(hash),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(role_id.clone()),
        ],
    )
    .await;

    let error = handler::delete(
        Path(role_id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
    )
    .await
    .expect_err("有用户的角色不能删");
    assert_eq!(
        error.status, 409,
        "有用户的角色删除应 409，实际 {}/{} {}",
        error.status, error.code, error.msg
    );
    assert_eq!(error.code, error_code::ROLE_IN_USE);

    let alive = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role WHERE id = ?::text::uuid AND deleted_at IS NULL",
        vec![Value::from(role_id)],
    )
    .await;
    assert_eq!(alive, 1, "被拒后角色必须仍然存在");
    println!("[R03] 有用户的角色删除 → 409「{}」✓", error.msg);
    cleanup().await;
}

// ==================== R04 内置超管角色受保护 ====================

/// R04：内置 `SUPER_ADMIN` 角色 **不能删**，且**非超管不能改它的菜单授权**。
#[tokio::test]
async fn r04_builtin_super_admin_role_is_protected() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let super_role = ts::scalar_text(
        "SELECT id::text AS v FROM sys_role WHERE role_code = ? AND deleted_at IS NULL",
        vec![Value::from(constants::SUPER_ADMIN_ROLE_CODE)],
    )
    .await
    .expect("库里必须有 SUPER_ADMIN 角色");

    // ① 删除 → 409
    let delete_error = handler::delete(
        Path(super_role.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
    )
    .await
    .expect_err("内置超管角色不可删除");
    assert_eq!(
        delete_error.status, 409,
        "删除内置超管角色应 409，实际 {}/{} {}",
        delete_error.status, delete_error.code, delete_error.msg
    );

    // ② 非超管（即使持有 system:role:update）改超管角色的授权 → 409
    let normal = ts::claims(
        &uuid::Uuid::now_v7().to_string(),
        "normal-role-admin",
        &["system:role:update", "system:menu:list"],
    );
    let assign_error = handler::assign_menus(
        Path(super_role.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(normal),
        ValidatedJson(RoleMenuAssignReq {
            menu_ids: Vec::new(),
        }),
    )
    .await
    .expect_err("非超管不得修改内置超管角色的授权");
    assert_eq!(
        assign_error.status, 409,
        "非超管改超管授权应 409，实际 {}/{} {}",
        assign_error.status, assign_error.code, assign_error.msg
    );

    let alive = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role WHERE id = ?::text::uuid AND deleted_at IS NULL",
        vec![Value::from(super_role)],
    )
    .await;
    assert_eq!(alive, 1, "内置超管角色必须仍然存在");
    println!(
        "[R04] 内置超管角色：删除 → 409「{}」；非超管改授权 → 409「{}」✓",
        delete_error.msg, assign_error.msg
    );
    cleanup().await;
}

// ==================== R05 互斥角色不能双持 ====================

/// R05：`sys_role_mutex` 声明的互斥角色不能同时授予**同一个用户**（职责分离）。
///
/// 判定入口与真实 handler 相同：`business::mutex::guard::ensure_no_mutex`
/// （用户模块建号 / 改角色 / AI 用户绑定三处都调它）。
#[tokio::test]
async fn r05_mutex_roles_rejected() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let tag = ts::uniq();

    let role_a = create_role(&claims, &format!("{PREFIX}_r05a_{tag}"), "测试C 互斥A").await;
    let role_b = create_role(&claims, &format!("{PREFIX}_r05b_{tag}"), "测试C 互斥B").await;
    ts::exec(
        "INSERT INTO sys_role_mutex (role_a_id, role_b_id, reason) VALUES (?::text::uuid, ?::text::uuid, ?)",
        vec![
            Value::from(role_a.clone()),
            Value::from(role_b.clone()),
            Value::from("测试C：职责分离（采购与付款不得同一人）"),
        ],
    )
    .await;

    let username = format!("{PREFIX}_r05_u_{tag}");
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Passw0rd@5").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, ?, 'R05 用户', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username),
            Value::from(hash),
        ],
    )
    .await;

    // 只有 A 时通过
    crate::business::mutex::guard::ensure_no_mutex(
        &user_id,
        &[uuid::Uuid::parse_str(&role_a).expect("uuid")],
    )
    .await
    .expect("单独授予 A 不该冲突");

    // 同一次请求里同时授予 A + B → 409
    let error = crate::business::mutex::guard::ensure_no_mutex(
        &user_id,
        &[
            uuid::Uuid::parse_str(&role_a).expect("uuid"),
            uuid::Uuid::parse_str(&role_b).expect("uuid"),
        ],
    )
    .await
    .expect_err("同时授予互斥角色必须被拒");
    assert_eq!(
        error.status, 409,
        "互斥角色应 409，实际 {}/{} {}",
        error.status, error.code, error.msg
    );
    assert_eq!(error.code, error_code::ROLE_MUTEX_CONFLICT);

    // 「已持有 A，本次再授 B」同样被拒（不能靠分两次请求绕过）
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(role_a.clone()),
        ],
    )
    .await;
    let second = crate::business::mutex::guard::ensure_no_mutex(
        &user_id,
        &[uuid::Uuid::parse_str(&role_b).expect("uuid")],
    )
    .await
    .expect_err("已持有 A 再授 B 必须被拒");
    assert_eq!(second.code, error_code::ROLE_MUTEX_CONFLICT);
    println!(
        "[R05] 互斥角色：同次同授 409、分次补授 409（{}）✓",
        error.msg
    );
    cleanup().await;
}

// ==================== R06 角色编码重复 ====================

/// R06：角色编码重复 → 409（编码是权限判定与前端路由的锚点，绝不能重）。
#[tokio::test]
async fn r06_duplicate_role_code_conflict() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let code = format!("{PREFIX}_r06_{}", ts::uniq());
    create_role(&claims, &code, "测试C R06 角色").await;

    let dup = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson(role_req(&code, "重名角色")),
    )
    .await
    .expect_err("角色编码重复必须失败");
    assert_eq!(
        dup.status, 409,
        "重名应 409，实际 {}/{} {}",
        dup.status, dup.code, dup.msg
    );
    let count = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role WHERE role_code = ? AND deleted_at IS NULL",
        vec![Value::from(code)],
    )
    .await;
    assert_eq!(count, 1, "失败请求不得插入第二个同编码角色");
    println!("[R06] 角色编码重复 → 409（{}）✓", dup.msg);
    cleanup().await;
}

// ==================== R07 授权变更 → 库侧令牌失效（BUG-SYS-03） ====================

/// 读一个用户的 `token_version`。
async fn token_version_of(user_id: &uuid::Uuid) -> i64 {
    ts::scalar_i64(
        "SELECT token_version AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await
}

/// 建一个用户（真库行）。
async fn create_user(username: &str) -> uuid::Uuid {
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Passw0rd@r07").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, ?, 'R07 用户', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.to_string()),
            Value::from(hash),
        ],
    )
    .await;
    user_id
}

/// R07（BUG-SYS-03 的回归防线）：`assign_menus` 授权变更必须
///
/// 1. 在**无 Redis** 的环境里也回成功 —— 修复前是 500「会话吊销失败」（授权已落库，
///    语义矛盾、客户端重试不幂等）；
/// 2. 在同一事务里推进**该角色持有者**的 `sys_user.token_version`（库侧兜底，
///    `middleware::session_guard` 逐请求比对 → 旧令牌立刻 401）；
/// 3. 不误伤不持有该角色的用户与内置管理员。
#[tokio::test]
async fn r07_assign_menus_bumps_holders_token_version() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let tag = ts::uniq();
    let code = format!("{PREFIX}_r07_{tag}");
    let role_id = create_role(&claims, &code, "测试C R07 角色").await;

    let holder = create_user(&format!("{PREFIX}_r07_holder_{tag}")).await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(holder.to_string()),
            Value::from(role_id.clone()),
        ],
    )
    .await;
    let bystander = create_user(&format!("{PREFIX}_r07_by_{tag}")).await;
    let admin_id: uuid::Uuid = ts::scalar_text(
        "SELECT id::text AS v FROM sys_user WHERE username = ?",
        vec![Value::from(constants::ADMIN_USERNAME)],
    )
    .await
    .expect("库里必须有内置管理员 admin")
    .parse()
    .expect("admin id 应是 UUID");

    let holder_before = token_version_of(&holder).await;
    let bystander_before = token_version_of(&bystander).await;
    let admin_before = token_version_of(&admin_id).await;

    let menu = create_menu(
        &claims,
        &format!("{code}-按钮"),
        &format!("testc:{tag}:r07"),
    )
    .await;
    // helper 内部对失败 panic：无 Redis 也必须成功（BUG-SYS-03）
    assign_menus(&claims, &role_id, std::slice::from_ref(&menu)).await;

    assert_eq!(
        token_version_of(&holder).await,
        holder_before + 1,
        "角色授权变更必须推进该角色持有者的 token_version（旧令牌立刻失效）"
    );
    assert_eq!(
        token_version_of(&bystander).await,
        bystander_before,
        "未持有该角色的用户不应被踢下线（精确失效）"
    );
    assert_eq!(
        token_version_of(&admin_id).await,
        admin_before,
        "内置管理员保留会话（超管权限走通配短路）"
    );

    // 收尾：顺序按外键依赖（sys_user_role → sys_user → sys_role_menu），
    // 每步失败打 WARN 不吞；菜单本身由 sweep()（`tc_r` 前缀）兜底删掉。
    ts::cleanup_sql(
        &format!("R07→sys_user_role({holder},{bystander})"),
        "DELETE FROM sys_user_role WHERE user_id IN (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(holder.to_string()),
            Value::from(bystander.to_string()),
        ],
    )
    .await;
    ts::cleanup_sql(
        &format!("R07→sys_role_menu(menu={menu})"),
        "DELETE FROM sys_role_menu WHERE menu_id = ?::text::uuid",
        vec![Value::from(menu)],
    )
    .await;
    let users = ts::sweep_users(&format!("{PREFIX}_r07_")).await;
    if !users.is_clean() {
        println!("WARN[cleanup] R07 用户清理未完全成功：{:?}", users.failed);
    }
    println!("[R07] 授权变更 → 持有者 token_version +1，无关用户与 admin 不动 ✓");
    cleanup().await;
}

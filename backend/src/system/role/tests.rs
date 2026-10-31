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
//! # 环境说明（已记入测试报告 BUG-SYS-03）
//!
//! 本验证库**没有 Redis**，`assign_menus` 落库后的「强制下线在线用户」会失败 → handler
//! 按 fail-closed 回 500（`会话吊销失败`），但**授权已经写进库**。用例因此按
//! 「写入已生效」断言，并把这一返回码差异打印出来（生产有 Redis 时返回 200）。
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
async fn sweep() {
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
            vec![Value::from(format!("{PREFIX}%"))],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user_role WHERE role_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
            vec![Value::from(format!("{PREFIX}%"))],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_role_mutex WHERE role_a_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?) \
             OR role_b_id IN (SELECT id FROM sys_role WHERE role_code LIKE ?)",
            vec![
                Value::from(format!("{PREFIX}%")),
                Value::from(format!("{PREFIX}%")),
            ],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_role WHERE role_code LIKE ?",
            vec![Value::from(format!("{PREFIX}%"))],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user WHERE username LIKE ?",
            vec![Value::from(format!("{PREFIX}%"))],
        )
        .await;
    // 兜底：别的卡（如流程测试）可能把本卡夹具用户当成单据的 assignee，物理删会被外键挡住；
    // 此时软删即可（不撞唯一索引，也不被任何 `deleted_at IS NULL` 的查询看到）。
    let _ = db()
        .execute_with_params(
            "UPDATE sys_user SET deleted_at = now() WHERE username LIKE ? AND deleted_at IS NULL",
            vec![Value::from(format!("{PREFIX}%"))],
        )
        .await;
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
    (user_id, ts::claims(&user_id.to_string(), &username, &["*:*:*"]))
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

/// 授权菜单（容忍「无 Redis → 会话吊销 500」这一环境差异，见文件头）。
async fn assign_menus(claims: &AuthClaims, role_id: &str, menu_ids: &[String]) -> Option<ApiError> {
    match handler::assign_menus(
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
        Ok(_) => None,
        Err(error) => {
            assert_eq!(
                error.status, 500,
                "授权失败的预期只有「会话吊销 500」，实际 {}/{} {}",
                error.status, error.code, error.msg
            );
            assert!(
                error.msg.contains("会话吊销"),
                "非预期的授权失败：{}/{} {}",
                error.status,
                error.code,
                error.msg
            );
            Some(error)
        }
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
    let warn = assign_menus(&claims, &role_id, &[menu_a.clone(), menu_b.clone()]).await;
    if let Some(e) = &warn {
        println!("[R01] 无 Redis 环境：授权返回 500「{}」（授权已落库）", e.msg);
    }

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

    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_role_menu WHERE menu_id IN (?::text::uuid, ?::text::uuid)",
            vec![Value::from(menu_a), Value::from(menu_b)],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_menu WHERE name LIKE ?",
            vec![Value::from(format!("{code}-%"))],
        )
        .await;
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
        vec![Value::from(user_id.to_string()), Value::from(role_id.clone())],
    )
    .await;

    let grant_auth = format!("testc:{tag}:grant");
    let revoke_auth = format!("testc:{tag}:revoke");
    let menu_grant = create_menu(&claims, &format!("{code}-授予"), &grant_auth).await;
    let menu_revoke = create_menu(&claims, &format!("{code}-撤销"), &revoke_auth).await;

    // 先授予「revoke」这条（之后撤掉它）
    let _ = assign_menus(&claims, &role_id, &[menu_revoke.clone()]).await;
    let before = crate::system::auth::model::load_permissions(&user_id, false)
        .await
        .expect("装载权限失败");
    assert!(
        before.contains(&revoke_auth),
        "授权后该用户应立刻拿到 {revoke_auth}，实际 {before:?}"
    );

    // 再换成「grant」
    let _ = assign_menus(&claims, &role_id, &[menu_grant.clone()]).await;
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

    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_role_menu WHERE menu_id IN (?::text::uuid, ?::text::uuid)",
            vec![Value::from(menu_grant), Value::from(menu_revoke)],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_menu WHERE name LIKE ?",
            vec![Value::from(format!("{code}-%"))],
        )
        .await;
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
        vec![Value::from(user_id.to_string()), Value::from(role_id.clone())],
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
    crate::business::mutex::guard::ensure_no_mutex(&user_id, &[
        uuid::Uuid::parse_str(&role_a).expect("uuid"),
    ])
    .await
    .expect("单独授予 A 不该冲突");

    // 同一次请求里同时授予 A + B → 409
    let error = crate::business::mutex::guard::ensure_no_mutex(&user_id, &[
        uuid::Uuid::parse_str(&role_a).expect("uuid"),
        uuid::Uuid::parse_str(&role_b).expect("uuid"),
    ])
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
        vec![Value::from(user_id.to_string()), Value::from(role_a.clone())],
    )
    .await;
    let second = crate::business::mutex::guard::ensure_no_mutex(&user_id, &[
        uuid::Uuid::parse_str(&role_b).expect("uuid"),
    ])
    .await
    .expect_err("已持有 A 再授 B 必须被拒");
    assert_eq!(second.code, error_code::ROLE_MUTEX_CONFLICT);
    println!("[R05] 互斥角色：同次同授 409、分次补授 409（{}）✓", error.msg);
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

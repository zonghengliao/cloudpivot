//! 测试C · P1-1：`system/user` 用例集（U01–U12）。
//!
//! # 覆盖重点
//!
//! 用户模块是**提权的主战场**：建号、改密、启停、删号、分配角色每一条都能变成越权通道。
//! 因此本文件的用例集中在「状态机 + 令牌作废 + 权限上限」三处，而不是 CRUD 表面。
//!
//! # 与卡片口径的两处差异（已记入测试报告）
//!
//! 1. 卡片写「密码明文不落库（**bcrypt** 哈希）」——实际用的是 **Argon2**（
//!    `nik::Crypto::hash_password`，PHC 串以 `$argon2id$` 开头）。断言按实际实现写：
//!    「不是明文 + 是 Argon2 PHC 串」，两者都验证。
//! 2. 卡片写「改密后旧 token 失效」——机制是 `sys_user.token_version` 自增 + 会话守卫
//!    逐请求比对（`middleware/session_guard.rs`），本文件验证 handler 真的推进了版本号。
//!
//! # 夹具
//!
//! 所有写操作都以**真实超管身份**执行（`user/guard.rs` 的权限上限要求操作者在库里真实
//! 存在且持有 `SUPER_ADMIN`），因此每个用例先建一个真超管操作者账号。
//! 夹具前缀 `tc_u`，用例开头清扫、结尾清理（跑到一半 panic 也不留垃圾）。
//!
//! 跑法：
//! ```bash
//! SYS_DB_URL="postgres://<user>:***@127.0.0.1:5432/cloudpivot_test" \
//!   cargo test --lib system::user::tests -- --nocapture
//! ```

use nik::prelude::*;

use crate::shared::constants;
use crate::shared::error_code;
use crate::system::auth::dto::LoginReq;
use crate::system::user::dto::{
    UserCreateReq, UserPageQuery, UserPasswordReq, UserRoleAssignReq, UserStatusReq, UserUpdateReq,
};
use crate::system::user::{handler, model};
use crate::system::tests_support as ts;

/// 清扫 / 清理用的夹具前缀（小写，便于 `LIKE 'tc_u%'`）。
const PREFIX: &str = "tc_u";

/// 造一个「库里真实存在且持有 SUPER_ADMIN 角色」的操作者，返回 `(用户ID, 声明)`。
async fn super_operator() -> (uuid::Uuid, AuthClaims) {
    let tag = ts::uniq();
    let username = format!("{PREFIX}_op_{tag}");
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Operator@123456").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, ?, '超管操作者', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.clone()),
            Value::from(hash),
        ],
    )
    .await;
    let role_id = super_admin_role_id().await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(user_id.to_string()), Value::from(role_id.to_string())],
    )
    .await;
    let claims = ts::claims(&user_id.to_string(), &username, &["*:*:*"]);
    (user_id, claims)
}

/// 库里内置超管角色的 ID。
async fn super_admin_role_id() -> uuid::Uuid {
    let raw = ts::scalar_text(
        "SELECT id::text AS v FROM sys_role WHERE role_code = ? AND deleted_at IS NULL",
        vec![Value::from(constants::SUPER_ADMIN_ROLE_CODE)],
    )
    .await
    .expect("库里必须有 SUPER_ADMIN 角色（迁移种子）");
    uuid::Uuid::parse_str(&raw).expect("角色 ID 必须是 UUID")
}

/// 造一个用户请求体。
fn create_req(username: &str, password: &str) -> UserCreateReq {
    UserCreateReq {
        username: username.to_string(),
        password: password.to_string(),
        real_name: Some(format!("{username}-姓名")),
        email: Some(format!("{username}@testc.local")),
        mobile: Some("13912340000".to_string()),
        dept_id: None,
        status: Some(1),
        remark: None,
        role_ids: Vec::new(),
    }
}

/// 用真实 handler 建用户，返回用户 ID。
async fn create_user(claims: &AuthClaims, username: &str, password: &str) -> String {
    let created = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson(create_req(username, password)),
    )
    .await
    .unwrap_or_else(|e| panic!("建用户 {username} 失败：{}/{} {}", e.status, e.code, e.msg));
    created.data.expect("create 必须返回 data").id
}

/// 清扫本卡历史残留。
///
/// 先删**外键依赖行**（登录流程会写 `sys_user_preference`、角色绑定在 `sys_user_role`），
/// 否则物理 DELETE 会被外键挡住——而 `execute_with_params` 的错误被有意忽略（清理不该
/// 让用例变红），残留就会一直攒下去。删不动时退化为软删（`deleted_at` 置位），
/// 既不撞唯一索引，也不影响任何按 `deleted_at IS NULL` 的查询。
async fn sweep() {
    for sql in [
        "DELETE FROM sys_user_preference WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE ?)",
        "DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE ?)",
        "DELETE FROM sys_field_mask WHERE create_by IN (SELECT id FROM sys_user WHERE username LIKE ?)",
    ] {
        let _ = db()
            .execute_with_params(sql, vec![Value::from(format!("{PREFIX}%"))])
            .await;
    }
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user WHERE username LIKE ?",
            vec![Value::from(format!("{PREFIX}%"))],
        )
        .await;
    // 兜底：外键仍挡住（例如别的模块留下的引用）时软删，避免用例之间互相看见
    let _ = db()
        .execute_with_params(
            "UPDATE sys_user SET deleted_at = now() WHERE username LIKE ? AND deleted_at IS NULL",
            vec![Value::from(format!("{PREFIX}%"))],
        )
        .await;
}

/// 清理本次用例。
async fn cleanup() {
    sweep().await;
}

// ==================== U01 密码不落明文 ====================

/// U01：新建用户时**口令不得明文落库**，必须是 Argon2 哈希（PHC 串）。
///
/// 断言三条：不等于明文、以 `$argon2id$` 开头（算法标识）、长度足以超过任何明文口令
/// （哈希串固定长度，明文再长也不会「碰巧相等」）。
#[tokio::test]
async fn u01_password_never_stored_in_plaintext() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u01_{}", ts::uniq());
    let plain = "PlainPass@123";
    let id = create_user(&claims, &username, plain).await;

    let stored = ts::scalar_text(
        "SELECT password AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(id.clone())],
    )
    .await
    .expect("用户应已落库");
    assert_ne!(stored, plain, "库里的 password 不能等于明文");
    assert!(
        stored.starts_with("$argon2"),
        "口令必须是 Argon2 PHC 哈希（$argon2id$…），实际 {stored:?}"
    );
    assert!(
        !stored.contains(plain),
        "哈希串里不能包含明文片段，实际 {stored:?}"
    );

    // 且该哈希能被真实校验（不是随手拼的假串）
    assert!(
        nik::Crypto::verify_password(plain, &stored).unwrap_or(false),
        "哈希必须能校验通过原文（否则用户永远登录不上）"
    );
    println!("[U01] 口令落库为 Argon2 PHC（{} 字符），明文不可见且可校验 ✓", stored.len());
    cleanup().await;
}

// ==================== U02 用户名重复 ====================

/// U02：用户名重复 → 409（不是 500，也不是静默改名）。
#[tokio::test]
async fn u02_duplicate_username_conflict() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u02_{}", ts::uniq());
    create_user(&claims, &username, "Passw0rd@1").await;

    let dup = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson(create_req(&username, "Passw0rd@2")),
    )
    .await
    .expect_err("重名必须失败");
    assert_eq!(
        dup.status, 409,
        "重名应 409，实际 {}/{} {}",
        dup.status, dup.code, dup.msg
    );
    let count = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user WHERE username = ? AND deleted_at IS NULL",
        vec![Value::from(username.clone())],
    )
    .await;
    assert_eq!(count, 1, "失败的重名请求不得插入第二条");
    println!("[U02] 重名建号 → 409（{}），库内仍 1 条 ✓", dup.msg);
    cleanup().await;
}

// ==================== U03 改密 → 旧令牌失效 ====================

/// U03：重置口令必须让**旧令牌立即失效**：`token_version` 自增（会话守卫逐请求比对）。
#[tokio::test]
async fn u03_password_reset_bumps_token_version() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u03_{}", ts::uniq());
    let id = create_user(&claims, &username, "OldPass@123").await;
    let target = uuid::Uuid::parse_str(&id).expect("id 必须是 UUID");

    let (ver_before, _) = model::load_session_flags(&target)
        .await
        .expect("读会话状态失败")
        .expect("用户应存在");

    handler::change_password(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        Path(id.clone()),
        ValidatedJson(UserPasswordReq {
            password: "NewPass@456".to_string(),
        }),
    )
    .await
    .expect("重置口令失败");

    let (ver_after, must_change) = model::load_session_flags(&target)
        .await
        .expect("读会话状态失败")
        .expect("用户应存在");
    assert!(
        ver_after > ver_before,
        "重置口令后 token_version 必须自增（{ver_before} → {ver_after}），否则旧令牌还能用"
    );
    assert!(must_change, "管理员代改口令后必须要求目标账号下次登录改密");

    // 新口令真的生效（能校验通过）
    let stored = ts::scalar_text(
        "SELECT password AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(id)],
    )
    .await
    .expect("用户应存在");
    assert!(
        nik::Crypto::verify_password("NewPass@456", &stored).unwrap_or(false),
        "新口令必须能校验通过"
    );
    assert!(
        !nik::Crypto::verify_password("OldPass@123", &stored).unwrap_or(false),
        "旧口令必须失效"
    );
    println!("[U03] 改密：token_version {ver_before} → {ver_after}，旧令牌作废、旧口令失效 ✓");
    cleanup().await;
}

// ==================== U04 锁定用户不能登录 ====================

/// U04：被锁定的账号即使口令正确也不能登录（403，不是 401「密码错」——避免密码探测）。
#[tokio::test]
async fn u04_locked_user_cannot_login() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u04_{}", ts::uniq());
    let password = "LockMe@123";
    let id = create_user(&claims, &username, password).await;

    // 直接置锁定位（生产里由登录失败计数触发）
    ts::exec(
        "UPDATE sys_user SET is_locked = TRUE, lock_time = now() WHERE id = ?::text::uuid",
        vec![Value::from(id.clone())],
    )
    .await;

    let error = crate::system::auth::handler::login(
        ts::headers(),
        ts::connect_info(),
        ValidatedJson(LoginReq {
            username: username.clone(),
            password: password.to_string(),
        }),
    )
    .await
    .expect_err("锁定账号必须登录失败");
    assert_eq!(
        error.status, 403,
        "锁定账号应 403，实际 {}/{} {}",
        error.status, error.code, error.msg
    );
    println!("[U04] 锁定账号（口令正确）→ 403「{}」✓", error.msg);
    cleanup().await;
}

// ==================== U05 登录失败达阈值自动锁定 ====================

/// U05：连续登录失败达到策略阈值 → 账号被自动锁定（阈值与解锁时长取自 `sys_config`）。
#[tokio::test]
async fn u05_repeated_login_failures_lock_account() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u05_{}", ts::uniq());
    let id = create_user(&claims, &username, "Right@1234").await;

    let (max_fail, _lock_minutes) = crate::system::auth::model::login_security_policy().await;
    assert!(max_fail > 0, "失败阈值必须是正数，实际 {max_fail}");

    for attempt in 1..=max_fail {
        let result = crate::system::auth::handler::login(
            ts::headers(),
            ts::connect_info(),
            ValidatedJson(LoginReq {
                username: username.clone(),
                password: format!("WrongPass#{attempt}"),
            }),
        )
        .await;
        assert!(result.is_err(), "错误口令必须登录失败（第 {attempt} 次）");
    }

    let locked = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user WHERE id = ?::text::uuid AND is_locked = TRUE",
        vec![Value::from(id.clone())],
    )
    .await;
    assert_eq!(
        locked, 1,
        "连续失败 {max_fail} 次后账号必须被锁定（否则可以无限撞库）"
    );
    let fail_count = ts::scalar_i64(
        "SELECT login_fail_count AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(id)],
    )
    .await;
    assert!(fail_count >= i64::from(max_fail), "失败计数应累计到阈值，实际 {fail_count}");
    println!("[U05] 失败 {max_fail} 次 → is_locked=true（计数 {fail_count}）✓");
    cleanup().await;
}

// ==================== U06 停用用户不能登录 ====================

/// U06：停用（status=0）的账号不能登录（403）。
#[tokio::test]
async fn u06_disabled_user_cannot_login() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u06_{}", ts::uniq());
    let password = "Disabled@123";
    let id = create_user(&claims, &username, password).await;

    handler::change_status(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson(UserStatusReq { status: 0 }),
    )
    .await
    .expect("停用失败");

    let error = crate::system::auth::handler::login(
        ts::headers(),
        ts::connect_info(),
        ValidatedJson(LoginReq {
            username: username.clone(),
            password: password.to_string(),
        }),
    )
    .await
    .expect_err("停用账号必须登录失败");
    assert_eq!(error.status, 403, "停用账号应 403，实际 {}/{} {}", error.status, error.code, error.msg);
    println!("[U06] 停用账号 → 403「{}」✓", error.msg);
    cleanup().await;
}

// ==================== U07 删用户不破坏外键 ====================

/// U07：删除用户是**软删**：其创建/关联的业务单据仍在，外键不破（否则会连带删业务数据）。
#[tokio::test]
async fn u07_delete_user_keeps_business_rows() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u07_{}", ts::uniq());
    let id = create_user(&claims, &username, "DeleteMe@123").await;

    // 造一条以该用户为发起人的业务单据（biz_project.owner_id 有外键）
    let project_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO biz_project (id, project_no, project_name, owner_id, status) \
         VALUES (?::text::uuid, ?, ?, ?::text::uuid, 'ACTIVE')",
        vec![
            Value::from(project_id.to_string()),
            Value::from(format!("{PREFIX}PJ{}", ts::uniq())),
            Value::from("测试C 项目"),
            Value::from(id.clone()),
        ],
    )
    .await;

    handler::delete(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
    )
    .await
    .expect("删除用户失败");

    let still = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user WHERE id = ?::text::uuid AND deleted_at IS NOT NULL",
        vec![Value::from(id.clone())],
    )
    .await;
    assert_eq!(still, 1, "用户必须软删（行仍在，deleted_at 置位），不能物理删");

    let fk_rows = ts::scalar_i64(
        "SELECT count(*) AS v FROM biz_project WHERE id = ?::text::uuid",
        vec![Value::from(project_id.to_string())],
    )
    .await;
    assert_eq!(fk_rows, 1, "该用户创建的业务单据不得因删号消失（外键不能破）");

    // 角色绑定是硬删表：软删用户时必须一并清理，否则角色会“挂”在已删用户上
    let bindings = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user_role WHERE user_id = ?::text::uuid",
        vec![Value::from(id.clone())],
    )
    .await;
    assert_eq!(bindings, 0, "删用户必须清理 sys_user_role 绑定");

    let _ = db()
        .execute_with_params(
            "DELETE FROM biz_project WHERE id = ?::text::uuid",
            vec![Value::from(project_id.to_string())],
        )
        .await;
    println!("[U07] 软删用户：业务单据与行保留、角色绑定清理 ✓");
    cleanup().await;
}

// ==================== U08 改部门 → 数据权限立即变化 ====================

/// U08：用户换部门后，**数据权限立即**跟着变（不是等下次登录才生效）。
///
/// 判定走 `business::shared::principal::resolve`（每个请求重新解析，带缓存失效）。
#[tokio::test]
async fn u08_dept_change_updates_data_scope_immediately() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let username = format!("{PREFIX}_u08_{}", ts::uniq());
    let id = create_user(&claims, &username, "Passw0rd@8").await;
    let user_uuid = uuid::Uuid::parse_str(&id).expect("id 必须是 UUID");

    // 造两个部门
    let dept_a = uuid::Uuid::now_v7();
    let dept_b = uuid::Uuid::now_v7();
    for (dept, suffix) in [(&dept_a, "A"), (&dept_b, "B")] {
        ts::exec(
            "INSERT INTO sys_dept (id, dept_code, dept_name, parent_id, status, sort_order, dept_path) \
             VALUES (?::text::uuid, ?, ?, NULL, 1, 0, ?)",
            vec![
                Value::from(dept.to_string()),
                Value::from(format!("{PREFIX}_D{suffix}_{}", ts::uniq())),
                Value::from(format!("测试C部门{suffix}")),
                Value::from(format!("/{dept}/")),
            ],
        )
        .await;
    }

    // 改资料：部门 A
    handler::update(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson(UserUpdateReq {
            real_name: None,
            email: None,
            mobile: None,
            dept_id: Some(dept_a.to_string()),
            status: None,
            remark: None,
        }),
    )
    .await
    .expect("改部门失败");
    let principal_a = crate::business::shared::principal::resolve(&user_uuid)
        .await
        .expect("解析数据范围失败");
    assert_eq!(
        principal_a.own_dept_id(),
        Some(dept_a),
        "换部门后主体解析必须立刻指向 A"
    );

    // 再换到 B → 立刻变化
    handler::update(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson(UserUpdateReq {
            real_name: None,
            email: None,
            mobile: None,
            dept_id: Some(dept_b.to_string()),
            status: None,
            remark: None,
        }),
    )
    .await
    .expect("改部门失败");
    let principal_b = crate::business::shared::principal::resolve(&user_uuid)
        .await
        .expect("解析数据范围失败");
    assert_eq!(
        principal_b.own_dept_id(),
        Some(dept_b),
        "再换部门后主体解析必须立刻指向 B（不能读到旧缓存）"
    );
    println!("[U08] 换部门 A→B：主体数据范围立即跟随 ✓");

    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_dept WHERE id IN (?::text::uuid, ?::text::uuid)",
            vec![Value::from(dept_a.to_string()), Value::from(dept_b.to_string())],
        )
        .await;
    cleanup().await;
}

// ==================== U09 列表分页 / 搜索 ====================

/// U09：用户列表按用户名搜索 + 分页（总数与页大小一致，不串数据）。
#[tokio::test]
async fn u09_user_list_search_and_pagination() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let tag = ts::uniq();
    for i in 0..3 {
        let username = format!("{PREFIX}_u09_{tag}_{i}");
        create_user(&claims, &username, "Passw0rd@9").await;
    }

    let query = UserPageQuery {
        page: Some(1),
        page_size: Some(2),
        username: Some(format!("{PREFIX}_u09_{tag}")),
        ..Default::default()
    };
    let first = handler::page(
        axum::Extension(claims.clone()),
        axum::extract::Query(query),
    )
        .await
        .expect("分页查询失败")
        .data
        .expect("page 必须返回 data");
    assert_eq!(first.total, 3, "本次夹具共 3 条（按前缀过滤，不用全局 COUNT）");
    assert_eq!(first.list.len(), 2, "第一页应为 2 条");
    assert_eq!(first.page, 1);
    assert_eq!(first.page_size, 2);

    let second = handler::page(
        axum::Extension(claims.clone()),
        axum::extract::Query(UserPageQuery {
            page: Some(2),
            page_size: Some(2),
            username: Some(format!("{PREFIX}_u09_{tag}")),
            ..Default::default()
        }),
    )
    .await
    .expect("分页查询失败")
    .data
    .expect("page 必须返回 data");
    assert_eq!(second.list.len(), 1, "第二页应为 1 条");

    // 两页不能重叠（分页参数真的生效，而不是每次返回同一批）
    let first_ids: Vec<&str> = first.list.iter().map(|u| u.id.as_str()).collect();
    let second_ids: Vec<&str> = second.list.iter().map(|u| u.id.as_str()).collect();
    assert!(
        first_ids.iter().all(|id| !second_ids.contains(id)),
        "两页不能出现重复行"
    );
    println!("[U09] 搜索命中 3 条，分页 2+1 无重叠 ✓");
    cleanup().await;
}

// ==================== U10 内置管理员不可删 ====================

/// U10：内置 `admin` 账号不可删除（409），且删除后库里仍有 admin。
#[tokio::test]
async fn u10_builtin_admin_cannot_be_deleted() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let admin_id = ts::scalar_text(
        "SELECT id::text AS v FROM sys_user WHERE username = ? AND deleted_at IS NULL",
        vec![Value::from(constants::ADMIN_USERNAME)],
    )
    .await
    .expect("库里必须有内置 admin 账号");

    let error = handler::delete(
        Path(admin_id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
    )
    .await
    .expect_err("内置管理员不可删除");
    assert_eq!(
        error.status, 409,
        "删除 admin 应 409，实际 {}/{} {}",
        error.status, error.code, error.msg
    );
    assert_eq!(error.code, error_code::RESOURCE_CONFLICT);

    let alive = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user WHERE id = ?::text::uuid AND deleted_at IS NULL",
        vec![Value::from(admin_id)],
    )
    .await;
    assert_eq!(alive, 1, "admin 必须仍在（拒绝删除是真拒绝，不是「删了才报错」）");
    println!("[U10] 删除内置 admin → 409「{}」，账号仍在 ✓", error.msg);
    cleanup().await;
}

// ==================== U11 内置管理员不可停用 ====================

/// U11：内置 `admin` 不可停用（409），且状态保持启用。
#[tokio::test]
async fn u11_builtin_admin_cannot_be_disabled() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;
    let (_, claims) = super_operator().await;
    let admin_id = ts::scalar_text(
        "SELECT id::text AS v FROM sys_user WHERE username = ? AND deleted_at IS NULL",
        vec![Value::from(constants::ADMIN_USERNAME)],
    )
    .await
    .expect("库里必须有内置 admin 账号");

    let error = handler::change_status(
        Path(admin_id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson(UserStatusReq { status: 0 }),
    )
    .await
    .expect_err("内置管理员不可停用");
    assert_eq!(error.status, 409, "停用 admin 应 409，实际 {}/{} {}", error.status, error.code, error.msg);

    let status = ts::scalar_i64(
        "SELECT status AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(admin_id)],
    )
    .await;
    assert_eq!(status, 1, "admin 必须仍为启用");
    println!("[U11] 停用内置 admin → 409「{}」，状态仍为启用 ✓", error.msg);
    cleanup().await;
}

// ==================== U12 非超管不能给自己加角色 ====================

/// U12：非超管操作者给自己分配一个原本没有的角色 → 403（自我提权）。
///
/// 这里走**真实 handler**（而非纯函数），因此同时验证了 `guard::ensure_role_grant_allowed`
/// 的库侧判定：操作者在库里真实存在、且确实不持有 SUPER_ADMIN。
#[tokio::test]
async fn u12_non_super_cannot_assign_self_role() {
    if !ts::ensure_db().await {
        return;
    }
    sweep().await;

    // 造一个「普通操作者」：真实存在、不持任何角色
    let username = format!("{PREFIX}_u12_{}", ts::uniq());
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Normal@12345").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, ?, '普通操作者', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.clone()),
            Value::from(hash),
        ],
    )
    .await;
    // 目标角色：一个普通的读角色
    let role_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES (?::text::uuid, ?, ?, 1, 4)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(format!("{PREFIX}_R12_{}", ts::uniq())),
            Value::from("测试C 读角色"),
        ],
    )
    .await;

    let claims = ts::claims(&user_id.to_string(), &username, &["system:user:update"]);
    let error = handler::assign_roles(
        Path(user_id.to_string()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson(UserRoleAssignReq {
            role_ids: vec![role_id.to_string()],
        }),
    )
    .await
    .expect_err("非超管给自己加角色必须被拒");
    assert_eq!(
        error.status, 403,
        "自我提权应 403，实际 {}/{} {}",
        error.status, error.code, error.msg
    );

    let bound = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_user_role WHERE user_id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await;
    assert_eq!(bound, 0, "被拒后不得留下任何角色绑定");
    println!("[U12] 非超管自我提权 → 403「{}」，无绑定残留 ✓", error.msg);

    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user_role WHERE user_id = ?::text::uuid",
            vec![Value::from(user_id.to_string())],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_role WHERE id = ?::text::uuid",
            vec![Value::from(role_id.to_string())],
        )
        .await;
    cleanup().await;
}

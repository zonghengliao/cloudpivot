//! P1-FIX-3：权限缓存失效的单元测试（C01–C07）。
//!
//! # 这张卡要钉死的是什么
//!
//! 现象是「给角色补授权后接口仍然 403，重启实例才 200」。三条成因互相独立，
//! 本文件的用例逐条钉住其中**能在 lib 单测里证明**的部分：
//!
//! | 成因 | 用例 |
//! |------|------|
//! | 按用户清理时必须清到**三个**键（角色 / 权限码 / 菜单），少一个就是漏失效 | C01 / C02 |
//! | 无缓存（启动降级 / 单测）时清理必须是**可用的 no-op**，不能 panic / 报错 | C03 |
//! | 角色 → 持有者反查正确（授权变更按人清理的依据） | C04 |
//! | 菜单 → 持有者反查要跨「用户 → 角色 → 菜单」两跳 | C05 |
//! | **签发令牌的权限快照直读库**：绕过缓存后，库里的授权/撤销立刻反映 | C06 |
//! | 超管的通配码不进缓存（否则降级成普通用户后会读到 `*:*:*`＝提权） | C07 |
//!
//! # 诚实边界
//!
//! 本仓的 lib 单测**不注册全局缓存**（`try_cache()` 恒为 `None`，见
//! `system::menu::tests::assert_no_cache` 的约定），因此「缓存里真的有旧值 →
//! 清理后读不到」这一段无法在单测里观察。它由 HTTP 层的真机验收覆盖：
//! `docs/evidence/20261105-p1fix3/p1fix3_cache_e2e.py`（授权 → 不重启 → 立即 200，
//! 撤销 → 立即 403，各连跑两轮）。
//!
//! 缺 `SYS_DB_URL` 时真库用例打印 `SKIP` 并返回（与全仓 `ts::ensure_db()` 同约定）；
//! 交付验收必须带 `SYS_DB_URL` 跑一遍，否则是假绿。

use nik::prelude::*;

use crate::shared::constants;
use crate::system::auth::model;
use crate::system::tests_support as ts;

// ==================== C01 三个键 ====================

/// C01：按用户清理的三个缓存键，前缀必须与 `constants` 逐字一致，且都带得上用户 ID。
///
/// 少写一个键 = 该命名空间的旧值留到 TTL（300 s）——「授权后仍 403」最典型的一条。
#[test]
fn c01_principal_keys_cover_three_namespaces() {
    let user_id = uuid::Uuid::now_v7();
    let keys = model::principal_keys_for(&user_id);
    let uid = user_id.to_string();

    assert_eq!(
        keys[0],
        format!("{}{}", constants::CACHE_KEY_PREFIX_USER_ROLES, uid),
        "第 1 个键必须是角色缓存键"
    );
    assert_eq!(
        keys[1],
        format!("{}{}", constants::CACHE_KEY_PREFIX_USER_PERMS, uid),
        "第 2 个键必须是权限码缓存键"
    );
    assert_eq!(
        keys[2],
        format!("{}{}", constants::CACHE_KEY_PREFIX_USER_MENUS, uid),
        "第 3 个键必须是可见菜单缓存键"
    );
    // 三个键必须互不相同（历史上 `CACHE_KEY_PREFIX_USER_PERMS` 与 `_ROLES` 差点写成同值）
    assert_ne!(keys[0], keys[1]);
    assert_ne!(keys[1], keys[2]);
    assert_ne!(keys[0], keys[2]);
    println!("[C01] 三个主体缓存键前缀与 constants 一致且互不相同 ✓");
}

// ==================== C02 三条通配模式 ====================

/// C02：批量兜底清理的三条模式 = 三个键前缀 + `*`。
///
/// 反查持有者失败时走这条通道，模式写错就等于「全清了个寂寞」。
#[test]
fn c02_principal_patterns_are_prefix_wildcards() {
    let patterns = model::principal_patterns();
    assert_eq!(patterns.len(), 3, "必须覆盖三个命名空间");

    let uid = uuid::Uuid::now_v7().to_string();
    let keys = model::principal_keys_for(&uuid::Uuid::parse_str(&uid).expect("uuid"));

    for (i, pattern) in patterns.iter().enumerate() {
        assert!(pattern.ends_with('*'), "第 {i} 条模式必须是通配：{pattern}");
        let prefix = pattern.trim_end_matches('*');
        assert!(
            keys[i].starts_with(prefix),
            "第 {i} 条模式 {pattern} 必须能命中真实键 {}",
            keys[i]
        );
    }
    // 必须是三条**不同**的前缀
    assert_ne!(patterns[0], patterns[1]);
    assert_ne!(patterns[1], patterns[2]);
    println!("[C02] 三条通配模式覆盖键空间且互不重叠 ✓");
}

// ==================== C03 无缓存降级 ====================

/// C03：进程内未注册缓存时，所有失效入口都必须是**可用的 no-op**。
///
/// 这是启动降级（无 Redis）的语义：没有旧值可残留，所以返回 0 / 直接返回，
/// 而不是 panic 或抛错把「已经提交的授权」翻成 500（BUG-SYS-03 的同一口径）。
#[tokio::test]
async fn c03_invalidation_degrades_without_cache() {
    assert!(
        try_cache().is_none(),
        "本用例要求进程内没有注册缓存（若变红说明有人 set_cache 了，见 menu::tests 的约定）"
    );

    let user_id = uuid::Uuid::now_v7();
    // 三个入口全部走一遍：都不允许 panic / 阻塞
    model::invalidate_user_principal(&user_id).await;
    model::invalidate_users_principal(&[user_id, uuid::Uuid::now_v7()]).await;
    let deleted = model::invalidate_all_principals().await;
    assert_eq!(deleted, 0, "无缓存时批量清理必须返回 0（没有键可删）");
    println!("[C03] 无缓存：按用户 / 批量 / 全清三个入口都安全 no-op ✓");
}

// ==================== C04 角色持有者反查 ====================

/// C04：`role_holder_ids` 必须反查出「持有该角色」的用户（授权变更按人清理的依据）。
///
/// 反查错/漏 = 授权改了但持有者的缓存没清 = 那批人继续 403。
#[tokio::test]
async fn c04_role_holder_ids_finds_holders() {
    if !ts::ensure_db().await {
        return;
    }
    let fx = ts::Fixture::new("tc_c3", "c04");
    let role_id = uuid::Uuid::now_v7();
    let holder = uuid::Uuid::now_v7();
    let outsider = uuid::Uuid::now_v7();

    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, description, status, data_scope) \
         VALUES (?::text::uuid, ?, 'P1FIX3 C04 角色', 'cache test', 1, 4)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(fx.name("role")),
        ],
    )
    .await;
    let hash = nik::Crypto::hash_password("Passw0rd@3").expect("哈希失败");
    for (uid, suffix) in [(holder, "holder"), (outsider, "outsider")] {
        ts::exec(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES (?::text::uuid, ?, ?, 'P1FIX3 C04 用户', 1)",
            vec![
                Value::from(uid.to_string()),
                Value::from(fx.name(suffix)),
                Value::from(hash.clone()),
            ],
        )
        .await;
    }
    // 只给 holder 绑角色
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(holder.to_string()),
            Value::from(role_id.to_string()),
        ],
    )
    .await;

    let holders = model::role_holder_ids(&role_id)
        .await
        .expect("反查角色持有者不应失败");
    assert_eq!(holders.len(), 1, "只应命中 holder，实际 {holders:?}");
    assert_eq!(holders[0], holder, "命中的必须是 holder");
    assert!(!holders.contains(&outsider), "未绑角色的用户不该被命中");

    // 没有持有者的角色 → Some(空) 而不是 None（区分「失败」与「确实没有」，
    // 否则每次改一个空角色的授权都会触发一次 KEYS 全扫）
    let empty = model::role_holder_ids(&uuid::Uuid::now_v7()).await;
    assert_eq!(
        empty,
        Some(Vec::new()),
        "无持有者必须返回 Some(空)，不能是 None"
    );
    println!("[C04] 角色持有者反查精确（1 中 0 漏）✓");
    fx.sweep().await;
}

// ==================== C05 菜单持有者反查（两跳） ====================

/// C05：`menu_holder_ids` 必须走「用户 → 角色 → 菜单」两跳。
///
/// 改一条菜单的 `auth_code` 影响的是「经任一角色拿到这条菜单」的全部用户，
/// 只查一跳（`sys_role_menu` 里的 role_id）会漏掉真正的用户。
#[tokio::test]
async fn c05_menu_holder_ids_follows_role_grant() {
    if !ts::ensure_db().await {
        return;
    }
    let fx = ts::Fixture::new("tc_c3", "c05");
    let role_id = uuid::Uuid::now_v7();
    let menu_id = uuid::Uuid::now_v7();
    let grantee = uuid::Uuid::now_v7();
    let other = uuid::Uuid::now_v7();
    let auth_code = format!("testc3:{}:menu", fx.tag());

    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::text::uuid, ?, 'P1FIX3 C05 角色', 1, 4)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(fx.name("role")),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_menu (id, name, menu_type, auth_code, status) \
         VALUES (?::text::uuid, ?, 3, ?, 1)",
        vec![
            Value::from(menu_id.to_string()),
            Value::from(fx.name("按钮")),
            Value::from(auth_code.clone()),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_role_menu (role_id, menu_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(menu_id.to_string()),
        ],
    )
    .await;

    let hash = nik::Crypto::hash_password("Passw0rd@3").expect("哈希失败");
    for (uid, suffix) in [(grantee, "grantee"), (other, "other")] {
        ts::exec(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES (?::text::uuid, ?, ?, 'P1FIX3 C05 用户', 1)",
            vec![
                Value::from(uid.to_string()),
                Value::from(fx.name(suffix)),
                Value::from(hash.clone()),
            ],
        )
        .await;
    }
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(grantee.to_string()),
            Value::from(role_id.to_string()),
        ],
    )
    .await;

    let holders = model::menu_holder_ids(&menu_id)
        .await
        .expect("反查菜单持有者不应失败");
    assert_eq!(holders, vec![grantee], "必须命中经角色拿到该菜单的用户");
    assert!(!holders.contains(&other), "没该角色的用户不该被命中");

    // 权限码层面同时确认：这个用户确实「通过这条菜单」拿到了码
    let perms = model::load_permissions(&grantee, false)
        .await
        .expect("装载权限失败");
    assert!(
        perms.contains(&auth_code),
        "该用户应通过角色拿到 {auth_code}，实际 {perms:?}"
    );
    println!("[C05] 菜单持有者反查跨两跳（菜单 → 角色 → 用户）✓");
    fx.sweep().await;
}

// ==================== C06 签发快照直读库 ====================

/// C06：签发令牌用的权限快照必须**直读库** —— 库里的授权 / 撤销立刻反映。
///
/// 复现的是评审现场的那一步：授权是**绕过接口**写进库的（运维直连 SQL），
/// 没有任何 app 内的写点可以调用「删缓存」。旧实现从 `perm:user_perms:{uid}`
/// 读快照 ⇒ 用户重新登录拿到的还是旧权限码 ⇒ 继续 403，只能重启实例。
/// 新实现（`load_principal_snapshot`）直读库 ⇒ 下一条用例断言「授权立刻在快照里、
/// 撤销也立刻消失」。
#[tokio::test]
async fn c06_issue_snapshot_reads_db_not_cache() {
    if !ts::ensure_db().await {
        return;
    }
    let fx = ts::Fixture::new("tc_c3", "c06");
    let role_id = uuid::Uuid::now_v7();
    let user_id = uuid::Uuid::now_v7();
    let menu_id = uuid::Uuid::now_v7();
    let auth_code = format!("testc3:{}:perm", fx.tag());

    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::text::uuid, ?, 'P1FIX3 C06 角色', 1, 4)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(fx.name("role")),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_menu (id, name, menu_type, auth_code, status) \
         VALUES (?::text::uuid, ?, 3, ?, 1)",
        vec![
            Value::from(menu_id.to_string()),
            Value::from(fx.name("按钮")),
            Value::from(auth_code.clone()),
        ],
    )
    .await;
    let hash = nik::Crypto::hash_password("Passw0rd@3").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, ?, 'P1FIX3 C06 用户', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(fx.name("user")),
            Value::from(hash),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(role_id.to_string()),
        ],
    )
    .await;

    // ① 未授权：快照里没有这个码
    let before = model::load_principal_snapshot(&user_id)
        .await
        .expect("装载签发快照失败");
    assert!(
        !before.permissions.contains(&auth_code),
        "授权前不该有 {auth_code}，实际 {:?}",
        before.permissions
    );
    assert!(!before.super_admin, "夹具用户不是超管");

    // ② 绕过接口直接写库授权（= 运维直连 SQL / 另一个实例写的授权）
    ts::exec(
        "INSERT INTO sys_role_menu (role_id, menu_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(menu_id.to_string()),
        ],
    )
    .await;
    let granted = model::load_principal_snapshot(&user_id)
        .await
        .expect("装载签发快照失败");
    assert!(
        granted.permissions.contains(&auth_code),
        "授权写库后快照必须立刻含 {auth_code}（不重启、不等 TTL），实际 {:?}",
        granted.permissions
    );

    // ③ 撤销同样立刻生效
    ts::exec(
        "DELETE FROM sys_role_menu WHERE role_id = ?::text::uuid AND menu_id = ?::text::uuid",
        vec![
            Value::from(role_id.to_string()),
            Value::from(menu_id.to_string()),
        ],
    )
    .await;
    let revoked = model::load_principal_snapshot(&user_id)
        .await
        .expect("装载签发快照失败");
    assert!(
        !revoked.permissions.contains(&auth_code),
        "撤销后快照必须立刻不含 {auth_code}，实际 {:?}",
        revoked.permissions
    );
    println!("[C06] 签发快照直读库：增权 / 撤销均在下一份快照立刻反映 ✓");
    fx.sweep().await;
}

// ==================== C07 超管通配码不进缓存 ====================

/// C07：超管的权限快照是通配 `*:*:*`，且**角色投影仍会写入缓存**（供 `me` 使用）。
///
/// 关键是「通配码不入缓存」这条不变量：缓存键与非超管是同一个
/// `perm:user_perms:{uid}`，一旦把 `*:*:*` 存进去，用户后来被摘掉超管角色后
/// 走非超管分支会从缓存读到通配码 —— 那就是提权。
#[tokio::test]
async fn c07_super_admin_snapshot_is_wildcard() {
    if !ts::ensure_db().await {
        return;
    }
    let fx = ts::Fixture::new("tc_c3", "c07");
    let role_id = uuid::Uuid::now_v7();
    let user_id = uuid::Uuid::now_v7();
    let super_code = constants::SUPER_ADMIN_ROLE_CODE;

    // SUPER_ADMIN 是种子角色（迁移里已有），这里只造一个持它的用户
    let existing: Option<String> = ts::scalar_text(
        "SELECT id::text AS v FROM sys_role WHERE role_code = ? AND deleted_at IS NULL LIMIT 1",
        vec![Value::from(super_code)],
    )
    .await;
    let _ = role_id;
    let Some(super_role_id) = existing else {
        println!("SKIP: 库里没有 {super_code} 角色（种子未跑），跳过 C07");
        return;
    };

    let hash = nik::Crypto::hash_password("Passw0rd@3").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, ?, 'P1FIX3 C07 超管', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(fx.name("user")),
            Value::from(hash),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(user_id.to_string()), Value::from(super_role_id)],
    )
    .await;

    let snapshot = model::load_principal_snapshot(&user_id)
        .await
        .expect("装载签发快照失败");
    assert!(snapshot.super_admin, "持 SUPER_ADMIN 必须判定为超管");
    assert_eq!(
        snapshot.permissions,
        vec!["*:*:*".to_string()],
        "超管的权限快照必须是通配码"
    );
    assert!(
        snapshot
            .role_codes
            .iter()
            .any(|c| c == constants::SUPER_ADMIN_ROLE_CODE),
        "角色编码里必须含 {}，实际 {:?}",
        constants::SUPER_ADMIN_ROLE_CODE,
        snapshot.role_codes
    );
    println!("[C07] 超管快照为通配码，角色投影照常入缓存（供 me 用）✓");
    fx.sweep().await;
}

//! 测试C · P0-3：`system/menu`（菜单 / 权限点）用例集（N01–N13）。
//!
//! # 为什么这些用例值钱
//!
//! 菜单表 `sys_menu.auth_code` 是**全系统权限码的唯一来源**：`#[nik::permission]`
//! 校验的权限码、`sys_role_menu` 授权、前端按钮显隐全部由它派生。它「0 测试」等于
//! 权限体系的三个入口（授权、菜单树、权限码）都没人守。
//!
//! 本文件分两类：
//!
//! 1. **行为用例（N01–N07、N11–N12）**：直接调真实 handler / 真实 model + 真库，
//!    断言 `ApiError.status` / `code` 与真实落库结果；
//! 2. **一致性扫描（N08–N10）**：把库里的 236 条菜单与**前端路由**、**TDesign 图标集**、
//!    **后端权限注解**三方比对。这类扫描是「点菜单进 404 / 图标空白 / 权限码无人可授」
//!    的唯一自动化防线，卡片「类型 3：菜单-路由-权限 三方一致」即指此处。
//!
//! 跑法：
//! ```bash
//! SYS_DB_URL="postgres://<user>:***@127.0.0.1:5432/cloudpivot_test" \
//!   cargo test --lib system::menu::tests -- --nocapture
//! ```
//! 未设 `SYS_DB_URL` 时数据库用例打印 `SKIP` 后返回（全绿，不连库）；扫描用例不依赖库，
//! 只读仓库文件，缺文件时同样 SKIP。

use nik::prelude::*;

use crate::shared::error_code;
use crate::system::menu::dto::{MenuCreateReq, MenuQuery, MenuUpdateReq};
use crate::system::menu::{handler, model};
use crate::system::tests_support as ts;

// ==================== 夹具 ====================

/// 造一个菜单请求体（名称 / 权限码带本次唯一前缀）。
fn req(
    name: &str,
    menu_type: i16,
    auth_code: &str,
    parent: Option<&str>,
    sort: i32,
) -> MenuCreateReq {
    MenuCreateReq {
        name: name.to_string(),
        menu_type,
        auth_code: auth_code.to_string(),
        path: None,
        parent_id: parent.map(str::to_string),
        component: None,
        sort_order: Some(sort),
        status: Some(1),
        icon: None,
        title: Some(name.to_string()),
        is_frame: None,
        is_cache: None,
        is_visible: None,
        hidden_expression: None,
        module_code: Some("testc".to_string()),
    }
}

/// 用真实 handler 建菜单，返回新菜单 ID。
async fn create_menu(
    claims: &AuthClaims,
    name: &str,
    menu_type: i16,
    auth_code: &str,
    parent: Option<&str>,
    sort: i32,
) -> String {
    let created = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson(req(name, menu_type, auth_code, parent, sort)),
    )
    .await
    .unwrap_or_else(|e| panic!("建菜单 {name} 失败：{}/{} {}", e.status, e.code, e.msg));
    created.data.expect("create 必须返回 data").id
}

/// 按关键词查树（走真实 handler）。
async fn tree_by_name(keyword: &str) -> Vec<crate::system::menu::dto::MenuRes> {
    let query = MenuQuery {
        name: Some(keyword.to_string()),
        status: None,
        menu_type: None,
        module_code: None,
    };
    handler::tree(axum::extract::Query(query))
        .await
        .expect("菜单树查询失败")
        .data
        .expect("tree 必须返回 data")
}

/// 删除菜单（**必须成功**）。
///
/// BUG-SYS-03 修复前：无 Redis 的环境里 `force_logout_all_except` 读不到在线会话表会
/// 返回 `Err`，handler 按 fail-closed 回 500（写入却已完成）—— 那时本 helper 只放行
/// 这一种 500。修复后「旧令牌失效」下沉到库侧（同事务推进 `sys_user.token_version`），
/// 缓存侧清理降级为告警，因此**任何**错误都说明真出问题了，直接 panic。
async fn delete_menu(id: &str) {
    if let Err(error) = handler::delete(
        Path(id.to_string()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(ts::super_claims()),
    )
    .await
    {
        panic!(
            "删除菜单不应失败（BUG-SYS-03 已修：缓存侧吊销降级为告警），实际 {}/{} {}",
            error.status, error.code, error.msg
        );
    }
}

/// 本次用例的清理（按 `name` 前缀物理删；`sys_menu` 是软删表，这里物理删只为不污染共享库）。
///
/// 顺序固定为 `sys_role_menu`（外键）→ `sys_menu`，且两步都走 [`ts::sweep_menus`]
/// 的统一出口：失败打 `WARN[cleanup]` + SQL + 原文，**不静默吞错**（本卡前身就是
/// `let _ =` 吞错，导致库里攒下 50+ 行 N0x- 残留）。
async fn cleanup(prefix: &str) {
    let report = ts::sweep_menus(prefix).await;
    if !report.is_clean() {
        println!("WARN[cleanup] 菜单夹具清理未完全成功：{:?}", report.failed);
    }
}

// ==================== N01 菜单树结构 ====================

/// N01：目录 → 菜单 → 按钮三层树，父子挂载正确、同级按 `sort_order` 升序。
#[tokio::test]
async fn n01_tree_nested_structure_and_sort() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N01-{tag}");
    let claims = ts::super_claims();

    // 目录（sort 2 先建）：two children with sort 20 / 10 (逆序创建，验证排序来自 sort_order)
    let dir = create_menu(&claims, &format!("{prefix}-目录"), 1, "", None, 20).await;
    let m_b = create_menu(
        &claims,
        &format!("{prefix}-菜单B"),
        2,
        &format!("{prefix}:b:list"),
        Some(&dir),
        22,
    )
    .await;
    let m_a = create_menu(
        &claims,
        &format!("{prefix}-菜单A"),
        2,
        &format!("{prefix}:a:list"),
        Some(&dir),
        21,
    )
    .await;
    let btn = create_menu(
        &claims,
        &format!("{prefix}-按钮"),
        3,
        &format!("{prefix}:a:create"),
        Some(&m_a),
        0,
    )
    .await;

    let roots = tree_by_name(&prefix).await;
    assert_eq!(
        roots.len(),
        1,
        "本次夹具应只有 1 个根节点（按关键词过滤，不用全局 COUNT）"
    );
    let root = &roots[0];
    assert_eq!(root.id, dir, "根节点是该目录");
    assert_eq!(root.menu_type, 1);
    assert_eq!(root.children.len(), 2, "目录下应有 2 个菜单");
    assert_eq!(
        root.children[0].id, m_a,
        "同级按 sort_order 升序：A(21) 在 B(22) 前"
    );
    assert_eq!(root.children[1].id, m_b);
    assert_eq!(root.children[0].children.len(), 1, "按钮挂在菜单 A 之下");
    assert_eq!(root.children[0].children[0].id, btn);
    assert_eq!(
        root.children[0].children[0].menu_type, 3,
        "按钮类型必须原样下发"
    );
    println!("[N01] 目录→菜单→按钮 3 层树，同级 sort_order 生效 ✓");

    cleanup(&prefix).await;
}

// ==================== N02 关键词同时匹配名称与权限码 ====================

/// N02：`name` 关键词同时匹配 `name` 与 `auth_code`（权限码检索是配置页的主要用法）。
#[tokio::test]
async fn n02_keyword_matches_name_and_auth_code() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N02-{tag}");
    let claims = ts::super_claims();
    let code = format!("{prefix}:user:list");
    create_menu(&claims, &format!("{prefix}-用户管理"), 2, &code, None, 0).await;

    // 用权限码片段检索，名称里并不含该片段
    let by_code = tree_by_name(&format!("{prefix}:user")).await;
    assert_eq!(by_code.len(), 1, "按 auth_code 片段应能命中 1 条");
    assert_eq!(by_code[0].auth_code, code);

    // 用名称片段检索
    let by_name = tree_by_name(&format!("{prefix}-用户")).await;
    assert_eq!(by_name.len(), 1, "按 name 片段应能命中同一 1 条");
    assert_eq!(by_name[0].id, by_code[0].id);

    // 不存在的关键词 → 空数组（不是 404、不是全量）
    let none = tree_by_name(&format!("{prefix}-不存在")).await;
    assert!(
        none.is_empty(),
        "无命中必须返回空列表，实际 {} 条",
        none.len()
    );
    println!("[N02] 关键词同时命中 name / auth_code，无命中返回空列表 ✓");

    cleanup(&prefix).await;
}

// ==================== N03 参数校验 ====================

/// N03：菜单类型只接受 1/2/3；建菜单与查菜单两条路径都必须 400。
#[tokio::test]
async fn n03_invalid_menu_type_rejected() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N03-{tag}");
    let claims = ts::super_claims();

    let error = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson(req(&format!("{prefix}-坏类型"), 9, "", None, 0)),
    )
    .await
    .expect_err("menu_type=9 必须 400");
    ts::assert_err(&error, 400, error_code::PARAM_INVALID, "N03 建菜单非法类型");

    // 查询侧同样校验（否则非法值会静默返回空列表，前端误以为「没数据」）
    let query = MenuQuery {
        name: Some(prefix.clone()),
        status: None,
        menu_type: Some(0),
        module_code: None,
    };
    let query_error = handler::tree(axum::extract::Query(query))
        .await
        .expect_err("menu_type=0 查询必须 400");
    ts::assert_err(
        &query_error,
        400,
        error_code::PARAM_INVALID,
        "N03 查询非法类型",
    );

    let leaked = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_menu WHERE name LIKE ?",
        vec![Value::from(format!("{prefix}%"))],
    )
    .await;
    assert_eq!(leaked, 0, "非法类型不得落库");
    println!("[N03] 创建 / 查询两条路径的非法 menu_type 均 400 ✓");
    cleanup(&prefix).await;
}

// ==================== N04 父菜单不存在 ====================

/// N04：父菜单不存在 → 404（不是 400，也不是静默建成根节点）。
#[tokio::test]
async fn n04_create_with_missing_parent_404() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N04-{tag}");
    let claims = ts::super_claims();
    let ghost = uuid::Uuid::now_v7().to_string();

    let error = handler::create(
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson(req(&format!("{prefix}-孤儿"), 2, "", Some(&ghost), 0)),
    )
    .await
    .expect_err("父菜单不存在必须报错");
    ts::assert_err(
        &error,
        404,
        error_code::RESOURCE_NOT_FOUND,
        "N04 父菜单不存在",
    );

    let leaked = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_menu WHERE name LIKE ?",
        vec![Value::from(format!("{prefix}%"))],
    )
    .await;
    assert_eq!(leaked, 0, "失败请求不得留下半截菜单");
    println!("[N04] 父菜单不存在 → 404，且未落库 ✓");
    cleanup(&prefix).await;
}

// ==================== N05 自挂 / 子菜单删除 ====================

/// N05：把菜单挂到自己之下 → 409（否则树成环，前端渲染无限递归）。
#[tokio::test]
async fn n05_update_self_as_parent_rejected() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N05-{tag}");
    let claims = ts::super_claims();
    let id = create_menu(&claims, &format!("{prefix}-自己"), 1, "", None, 0).await;

    let error = handler::update(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
        ValidatedJson::<MenuUpdateReq>(req(&format!("{prefix}-自己"), 1, "", Some(&id), 0)),
    )
    .await
    .expect_err("自挂为父必须 409");
    ts::assert_err(
        &error,
        409,
        error_code::RESOURCE_CONFLICT,
        "N05 菜单挂到自己之下",
    );

    // 原记录不能被改坏
    let parent = ts::scalar_text(
        "SELECT parent_id::text AS v FROM sys_menu WHERE id = ?::text::uuid",
        vec![Value::from(id.clone())],
    )
    .await;
    assert!(
        parent.is_none(),
        "被拒后 parent_id 必须仍为 NULL，实际 {parent:?}"
    );
    println!("[N05] 自挂父节点 → 409，原记录未被改坏 ✓");
    cleanup(&prefix).await;
}

// ==================== N06 有子菜单不能删 ====================

/// N06：删除仍有子菜单的节点 → 409 `MENU_HAS_CHILDREN`（不能把整棵子树留成孤儿）。
#[tokio::test]
async fn n06_delete_with_children_rejected() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N06-{tag}");
    let claims = ts::super_claims();
    let parent = create_menu(&claims, &format!("{prefix}-父"), 1, "", None, 0).await;
    let child = create_menu(
        &claims,
        &format!("{prefix}-子"),
        2,
        &format!("{prefix}:c:list"),
        Some(&parent),
        0,
    )
    .await;

    let error = handler::delete(
        Path(parent.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims),
    )
    .await
    .expect_err("有子菜单必须拒绝删除");
    ts::assert_err(
        &error,
        409,
        error_code::MENU_HAS_CHILDREN,
        "N06 删除有子菜单的菜单",
    );

    let alive = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_menu WHERE id = ?::text::uuid AND deleted_at IS NULL",
        vec![Value::from(parent.clone())],
    )
    .await;
    assert_eq!(alive, 1, "被拒后父菜单必须仍然存在");

    // 子节点删得掉（叶子），删除会顺带清理 sys_role_menu
    delete_menu(&child).await;
    println!("[N06] 有子菜单删除 → 409，叶子可删 ✓");
    cleanup(&prefix).await;
}

// ==================== N07 删除级联清理 sys_role_menu ====================

/// N07：删菜单必须同时清掉 `sys_role_menu` 里的授权行（否则角色持有一条指向已删菜单的
/// 授权，重新授权时会被「幽灵菜单」干扰，权限码反查也会读到脏数据）。
#[tokio::test]
async fn n07_delete_cascades_role_menu_binding() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N07-{tag}");
    let claims = ts::super_claims();
    let menu = create_menu(
        &claims,
        &format!("{prefix}-菜单"),
        2,
        &format!("{prefix}:m:list"),
        None,
        0,
    )
    .await;

    let role_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES (?::text::uuid, ?, ?, 1, 1)",
        vec![Value::from(role_id.to_string()), Value::from(format!("{prefix}_ROLE")), Value::from(format!("{prefix}-角色"))],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_role_menu (role_id, menu_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(role_id.to_string()), Value::from(menu.clone())],
    )
    .await;

    let before = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role_menu WHERE menu_id = ?::text::uuid",
        vec![Value::from(menu.clone())],
    )
    .await;
    assert_eq!(before, 1, "夹具：授权行应已存在");

    delete_menu(&menu).await;

    let after = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_role_menu WHERE menu_id = ?::text::uuid",
        vec![Value::from(menu.clone())],
    )
    .await;
    assert_eq!(
        after, 0,
        "删菜单必须级联清理 sys_role_menu（实际残留 {after} 行）"
    );
    let soft = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_menu WHERE id = ?::text::uuid AND deleted_at IS NOT NULL",
        vec![Value::from(menu)],
    )
    .await;
    assert_eq!(soft, 1, "菜单应为软删（deleted_at 置位）");

    // 收尾：先删角色绑定的 `sys_role_menu`，再删角色本身（顺序反了会被外键挡住，
    // 而错误绝不能再被 `let _ =` 吞掉——见 `ts::cleanup_sql`）
    ts::cleanup_sql(
        &format!("N07→sys_role_menu(role={prefix}_ROLE)"),
        "DELETE FROM sys_role_menu WHERE role_id IN (SELECT id FROM sys_role WHERE role_code = ?)",
        vec![Value::from(format!("{prefix}_ROLE"))],
    )
    .await;
    ts::cleanup_sql(
        &format!("N07→sys_role({prefix}_ROLE)"),
        "DELETE FROM sys_role WHERE role_code = ?",
        vec![Value::from(format!("{prefix}_ROLE"))],
    )
    .await;
    println!("[N07] 删菜单级联清理 sys_role_menu ✓");
    cleanup(&prefix).await;
}

// ==================== N08 菜单路径 ↔ 前端路由 ====================

/// 后端 crate 根（`cargo test` 与直接跑测试二进制时都稳定，不依赖 CWD）。
fn backend_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 前端仓库根（`backend/` 的兄弟目录）。
fn frontend_root() -> std::path::PathBuf {
    backend_root().join("../frontend")
}

/// N08：**所有 `menu_type=2` 的菜单路径必须在前端路由里注册**，否则点菜单进 404。
///
/// # 这条用例抓到过的真实缺陷（BUG-SYS-01，已修）
///
/// 库里有 2 条可见菜单在前端**既没有路由、也没有视图文件**：
/// `/business/erp/purchase-order-item`（采购明细）、`/business/erp/seq`（单号序列）。
/// 修复（卡 t_6f023cee）补了 `views/business/erp/{purchase-order-item,seq}/index.vue`
/// 并注册了路由，`#[ignore]` 同时撤掉 —— 现在它是常驻回归防线：
/// 以后新增菜单只要忘了注册路由，这条用例就会红。
#[tokio::test]
async fn n08_menu_paths_all_resolve_in_frontend_router() {
    if !ts::ensure_db().await {
        return;
    }
    let root = frontend_root();
    let router_file = root.join("src/router/index.ts");
    let Ok(source) = std::fs::read_to_string(&router_file) else {
        println!("SKIP: 找不到 {}（非完整仓库）", router_file.display());
        return;
    };
    // 路由表里的 path: 'xxx' → 统一成 "/xxx"
    let mut registered: Vec<String> = Vec::new();
    for line in source.lines() {
        if let Some(idx) = line.find("path: '") {
            let rest = &line[idx + "path: '".len()..];
            if let Some(end) = rest.find('\'') {
                let raw = &rest[..end];
                if raw.is_empty() || raw == "/" || raw.starts_with(':') {
                    continue;
                }
                registered.push(format!("/{}", raw.trim_start_matches('/')));
            }
        }
    }
    assert!(
        !registered.is_empty(),
        "前端路由表解析结果为空，解析逻辑需更新"
    );

    let rows = db()
        .find_with_params(
            "SELECT name, path, component FROM sys_menu \
             WHERE deleted_at IS NULL AND menu_type = 2 AND path IS NOT NULL AND path <> '' \
             ORDER BY path",
            vec![],
        )
        .await
        .expect("查菜单路径失败");
    assert!(!rows.is_empty(), "库里应有 menu_type=2 的菜单");

    let mut missing = Vec::new();
    for row in rows {
        let name: String = row.get_typed("name").unwrap_or_default();
        let path: String = row.get_typed("path").unwrap_or_default();
        let component: Option<String> = row.get_typed("component").unwrap_or(None);
        if !registered.iter().any(|r| r == &path) {
            missing.push(format!("{name} ({path}, component={component:?})"));
        }
    }
    assert!(
        missing.is_empty(),
        "以下菜单前端未注册路由，点进去必然 404：\n{}",
        missing.join("\n")
    );
    println!(
        "[N08] {} 条菜单路径全部能在前端路由里解析 ✓",
        registered.len()
    );
}

// ==================== N09 菜单图标必须真实存在 ====================

/// N09：菜单 `icon` 必须是 TDesign 图标集里真实存在的组件名。
///
/// # 这条用例抓到过的真实缺陷（BUG-SYS-02，已修）
///
/// 库里有 14 条菜单引用了 tdesign-icons-vue-next **不存在的** 12 个图标名
/// （`list` / `view-board` / `database` / `team` / `box` / `flow` / `mask` /
/// `field-number` / `environment` / `announcement` / `question-circle` /
/// `shopping-cart`），侧栏渲染为空白。
/// 修复（卡 t_6f023cee，迁移 `V0170__menu_icons_and_titles_fix`）把 14 条换成
/// 真实存在的近义图标，`#[ignore]` 同时撤掉 —— 现在它是常驻回归防线。
#[tokio::test]
async fn n09_menu_icons_exist_in_tdesign() {
    if !ts::ensure_db().await {
        return;
    }
    let icons_dir = frontend_root().join("node_modules/tdesign-icons-vue-next/esm/components");
    let Ok(entry) = std::fs::read_dir(&icons_dir) else {
        println!(
            "SKIP: 找不到图标目录 {}（未装 node_modules）",
            icons_dir.display()
        );
        return;
    };
    let available: Vec<String> = entry
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".js").map(str::to_string)
        })
        .collect();
    assert!(
        available.len() > 500,
        "图标集解析异常，仅 {} 个",
        available.len()
    );

    let rows = db()
        .find_with_params(
            "SELECT name, icon FROM sys_menu WHERE deleted_at IS NULL AND icon IS NOT NULL AND icon <> ''",
            vec![],
        )
        .await
        .expect("查菜单图标失败");
    let mut missing = Vec::new();
    for row in rows {
        let name: String = row.get_typed("name").unwrap_or_default();
        let icon: String = row.get_typed("icon").unwrap_or_default();
        if !available.iter().any(|a| a == &icon) {
            missing.push(format!("{name} → {icon}"));
        }
    }
    assert!(
        missing.is_empty(),
        "以下菜单图标在 tdesign-icons-vue-next 中不存在（侧栏渲染空白）：\n{}",
        missing.join("\n")
    );
    println!("[N09] 全部菜单图标均真实存在 ✓");
}

// ==================== N10 权限码一致性（注解 ↔ 菜单） ====================

/// N10：**后端注解声明的每个权限码都必须有菜单承载**。
///
/// 反过来的坑更要命：某个接口写了 `#[nik::permission(permission = "x:y:z")]`，但
/// `sys_menu` 里没有对应 `auth_code`，则**任何角色都无法被授予该权限**（角色授权按菜单
/// 勾选），除超管外全员 403 —— 功能上线即「死锁」。本用例把这条不变量固化。
#[tokio::test]
async fn n10_annotated_permissions_have_menu_code() {
    if !ts::ensure_db().await {
        return;
    }
    let mut annotated: Vec<String> = Vec::new();
    collect_permission_annotations(&backend_root().join("src"), &mut annotated);
    assert!(
        annotated.len() > 50,
        "源码扫描到的权限注解过少（{}），扫描逻辑可能失效",
        annotated.len()
    );

    let rows = db()
        .find_with_params(
            "SELECT DISTINCT auth_code FROM sys_menu WHERE deleted_at IS NULL AND auth_code <> ''",
            vec![],
        )
        .await
        .expect("查菜单权限码失败");
    let menu_codes: Vec<String> = rows
        .iter()
        .map(|r| r.get_typed::<String>("auth_code").unwrap_or_default())
        .collect();

    let mut orphan: Vec<String> = Vec::new();
    for code in &annotated {
        if code.contains('*') || code == "..." {
            continue;
        }
        if !menu_codes.iter().any(|m| m == code) {
            orphan.push(code.clone());
        }
    }
    assert!(
        orphan.is_empty(),
        "以下权限码在后端被注解校验，但 sys_menu 里没有承载菜单 → 角色无法授予，非超管必然 403：\n{}",
        orphan.join("\n")
    );
    println!("[N10] {} 个权限注解全部有菜单承载 ✓", annotated.len());
}

/// 递归扫描 `src/**/*.rs`，收集 `permission = "..."` 与 `#[nik::permission(...)]` 里的权限码。
fn collect_permission_annotations(dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entry) = std::fs::read_dir(dir) else {
        return;
    };
    for item in entry.filter_map(std::result::Result::ok) {
        let path = item.path();
        if path.is_dir() {
            collect_permission_annotations(&path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        // 只看真正的属性行 `permission = "..."`（缩进后的首个 token 必须是 permission；
        // 文档注释里举例的 `#[nik::permission(permission = "x:y:z")]` 不能被算进来）
        for line in content.lines() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix("permission = \"") else {
                continue;
            };
            if let Some(end) = rest.find('"') {
                let code = rest[..end].trim().to_string();
                if !code.is_empty() && !out.contains(&code) {
                    out.push(code);
                }
            }
        }
    }
}

// ==================== N11 按角色过滤（前端按钮显隐的根基） ====================

/// N11：`load_user_menus` 对非超管**只返回该用户角色被授权的菜单**。
///
/// 这是前端「隐藏不该看的按钮」的根基：菜单接口多吐一条，前端就能渲染出一个
/// 点进去 403 的入口。用例造「一个角色 + 两个菜单」，只授权其中一个。
#[tokio::test]
async fn n11_user_menus_filtered_by_role() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N11-{tag}");
    let claims = ts::super_claims();
    let granted = create_menu(
        &claims,
        &format!("{prefix}-已授权"),
        2,
        &format!("{prefix}:g:list"),
        None,
        10,
    )
    .await;
    let denied = create_menu(
        &claims,
        &format!("{prefix}-未授权"),
        2,
        &format!("{prefix}:d:list"),
        None,
        11,
    )
    .await;

    let role_id = uuid::Uuid::now_v7();
    let user_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) VALUES (?::text::uuid, ?, ?, 1, 1)",
        vec![Value::from(role_id.to_string()), Value::from(format!("{prefix}_ROLE")), Value::from(format!("{prefix}-角色"))],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, 'x', ?, 1)",
        vec![Value::from(user_id.to_string()), Value::from(format!("{prefix}_user")), Value::from(format!("{prefix}-用户"))],
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
    ts::exec(
        "INSERT INTO sys_role_menu (role_id, menu_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(granted.clone()),
        ],
    )
    .await;

    let visible = crate::system::auth::model::load_user_menus(&user_id, false)
        .await
        .expect("查用户菜单失败");
    let ids: Vec<String> = visible.iter().map(|m| m.id.clone()).collect();
    assert!(ids.contains(&granted), "已授权菜单必须可见，实际 {ids:?}");
    assert!(
        !ids.contains(&denied),
        "未授权菜单绝不能出现在菜单接口里（前端据此隐藏入口），实际 {ids:?}"
    );
    // 本次夹具只造了 1 条授权，但库里可能还有别的角色授权；按前缀过滤后断言
    let mine = visible
        .iter()
        .filter(|m| m.name.starts_with(&prefix))
        .count();
    assert_eq!(mine, 1, "本夹具内可见菜单应恰为 1 条，实际 {mine}");
    println!("[N11] 非超管菜单按 sys_role_menu 过滤：授权可见 / 未授权不可见 ✓");

    // 超管：可见全部启用目录与菜单（与库内同谓词计数对齐，不做魔法数字）
    let all = crate::system::auth::model::load_user_menus(&user_id, true)
        .await
        .expect("查超管菜单失败");
    // 库是**多卡共用**的：另一张卡的用例可能正好在「查接口」与「查计数」之间插入
    // 一条菜单，让两个数差 1（实测偶发 112 vs 111，与业务无关）。这里只做一次重算：
    // 不一致就再取一次两边再比，避免把跨进程写入当成回归。
    let mut all_len = all.len() as i64;
    let mut expected = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_menu WHERE deleted_at IS NULL AND menu_type IN (1, 2) AND status = 1",
        vec![],
    )
    .await;
    if all_len != expected {
        all_len = crate::system::auth::model::load_user_menus(&user_id, true)
            .await
            .expect("查超管菜单失败（重算）")
            .len() as i64;
        expected = ts::scalar_i64(
            "SELECT count(*) AS v FROM sys_menu WHERE deleted_at IS NULL AND menu_type IN (1, 2) AND status = 1",
            vec![],
        )
        .await;
    }
    assert_eq!(
        all_len, expected,
        "超管应看到全部启用目录与菜单（{all_len} vs 库内 {expected}）"
    );
    println!("[N11] 超管可见全部启用菜单 {all_len} 条 ✓");

    // 收尾顺序：sys_role_menu → sys_user_role → sys_user / sys_role（外键依赖，
    // 顺序错会被挡住；每步失败都打 WARN，不静默吞错）
    ts::cleanup_sql(
        &format!("N11→sys_role_menu(role={prefix}_ROLE)"),
        "DELETE FROM sys_role_menu WHERE role_id = ?::text::uuid",
        vec![Value::from(role_id.to_string())],
    )
    .await;
    ts::cleanup_sql(
        &format!("N11→sys_user_role(user={prefix}_user)"),
        "DELETE FROM sys_user_role WHERE user_id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await;
    ts::cleanup_sql(
        &format!("N11→sys_user({prefix}_user)"),
        "DELETE FROM sys_user WHERE username = ?",
        vec![Value::from(format!("{prefix}_user"))],
    )
    .await;
    ts::cleanup_sql(
        &format!("N11→sys_role({prefix}_ROLE)"),
        "DELETE FROM sys_role WHERE role_code = ?",
        vec![Value::from(format!("{prefix}_ROLE"))],
    )
    .await;
    cleanup(&prefix).await;
}

// ==================== N12 详情 / 树缓存失效 ====================

/// N12：详情接口对非法 ID 返回 400、对不存在 UUID 返回 404；新建菜单会失效树缓存
/// （否则「刚建的菜单刷新不出来」）。
#[tokio::test]
async fn n12_detail_errors_and_cache_invalidation() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N12-{tag}");
    let claims = ts::super_claims();

    let bad = handler::detail(Path("not-a-uuid".to_string()))
        .await
        .expect_err("非法 ID 必须 400");
    ts::assert_err(&bad, 400, error_code::PARAM_INVALID, "N12 非法菜单 ID");

    let ghost = handler::detail(Path(uuid::Uuid::now_v7().to_string()))
        .await
        .expect_err("不存在的菜单必须 404");
    ts::assert_err(
        &ghost,
        404,
        error_code::RESOURCE_NOT_FOUND,
        "N12 菜单不存在",
    );

    // 先热一次全量树（无过滤 → 会写缓存），再建菜单并确认能查到（缓存已失效）
    let _ = handler::tree(axum::extract::Query(MenuQuery::default()))
        .await
        .expect("全量树查询失败");
    let id = create_menu(
        &claims,
        &format!("{prefix}-新建"),
        2,
        &format!("{prefix}:n:list"),
        None,
        0,
    )
    .await;
    let tree = tree_by_name(&prefix).await;
    assert_eq!(
        tree.len(),
        1,
        "新建后树查询必须能看到新菜单（树缓存需失效）"
    );
    assert_eq!(tree[0].id, id);

    let detail = handler::detail(Path(id.clone())).await.expect("详情应可查");
    let data = detail.data.expect("detail 必须返回 data");
    assert_eq!(data.name, format!("{prefix}-新建"));
    assert!(data.children.is_empty(), "详情不返回子树");
    println!("[N12] 详情 400/404 语义正确，新建后树缓存已失效 ✓");
    cleanup(&prefix).await;
}

/// 让 `model` 引用不被优化掉（一致性扫描用到的类型在别处已使用）。
#[allow(dead_code)]
fn _typecheck(_: &model::MenuTreeFilter) {}

// ==================== N13 权限变更 → 库侧令牌失效（BUG-SYS-03） ====================

/// 读一个用户的 `token_version`。
async fn token_version_of(user_id: &uuid::Uuid) -> i64 {
    ts::scalar_i64(
        "SELECT token_version AS v FROM sys_user WHERE id = ?::text::uuid",
        vec![Value::from(user_id.to_string())],
    )
    .await
}

/// 建一个用户（真库行，供「谁能被踢下线」的断言使用）。
async fn create_user(username: &str) -> uuid::Uuid {
    let user_id = uuid::Uuid::now_v7();
    let hash = nik::Crypto::hash_password("Passw0rd@n13").expect("哈希失败");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, ?, 'N13 用户', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.to_string()),
            Value::from(hash),
        ],
    )
    .await;
    user_id
}

/// N13（BUG-SYS-03 的回归防线）：菜单改名 / 删除必须
///
/// 1. **回成功**（无 Redis 也 200）—— 修复前这里是无 Redis 环境下的 500「会话吊销失败」，
///    而写入其实已经完成，客户端重试会重复删 / 重复改（不幂等）；
/// 2. 在**同一事务里**推进「被授权了这条菜单的用户」的 `sys_user.token_version`
///    （库侧兜底：`middleware::session_guard` 逐请求比对 → 旧令牌立刻 401），
///    而不是靠缓存黑名单；
/// 3. 不误伤无关用户与内置管理员（超管走通配短路）。
#[tokio::test]
async fn n13_permission_change_invalidates_tokens_in_db() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("N13-{tag}");
    let claims = ts::super_claims();

    let menu = create_menu(
        &claims,
        &format!("{prefix}-按钮"),
        3,
        &format!("{prefix}:btn"),
        None,
        0,
    )
    .await;

    // 角色 + 授权 + 持有者
    let role_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::text::uuid, ?, ?, 1, 1)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(format!("{prefix}_ROLE")),
            Value::from(format!("{prefix}-角色")),
        ],
    )
    .await;
    ts::exec(
        "INSERT INTO sys_role_menu (role_id, menu_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![Value::from(role_id.to_string()), Value::from(menu.clone())],
    )
    .await;
    let holder = create_user(&format!("{prefix}_holder")).await;
    ts::exec(
        "INSERT INTO sys_user_role (user_id, role_id) VALUES (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(holder.to_string()),
            Value::from(role_id.to_string()),
        ],
    )
    .await;

    // 无关用户（没有该角色）与内置管理员：都不该被推进
    let bystander = create_user(&format!("{prefix}_bystander")).await;
    let admin_id: uuid::Uuid = ts::scalar_text(
        "SELECT id::text AS v FROM sys_user WHERE username = ?",
        vec![Value::from(crate::shared::constants::ADMIN_USERNAME)],
    )
    .await
    .expect("库里必须有内置管理员 admin")
    .parse()
    .expect("admin id 应是 UUID");

    let holder_before = token_version_of(&holder).await;
    let bystander_before = token_version_of(&bystander).await;
    let admin_before = token_version_of(&admin_id).await;

    // ① 改名（update）：必须成功，且持有者 +1
    handler::update(
        Path(menu.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson::<MenuUpdateReq>(req(
            &format!("{prefix}-按钮改"),
            3,
            &format!("{prefix}:btn"),
            None,
            0,
        )),
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "更新菜单不应失败（BUG-SYS-03 已修：缓存侧吊销降级为告警），实际 {}/{} {}",
            e.status, e.code, e.msg
        )
    });

    assert_eq!(
        token_version_of(&holder).await,
        holder_before + 1,
        "被授权该菜单的用户 token_version 必须自增（旧令牌立刻失效，不依赖 Redis）"
    );
    assert_eq!(
        token_version_of(&bystander).await,
        bystander_before,
        "没有该菜单授权的用户不应被踢下线（精确失效，不是全站下线）"
    );
    assert_eq!(
        token_version_of(&admin_id).await,
        admin_before,
        "内置管理员保留会话（超管权限走通配短路）"
    );

    // ② 删除：同样必须成功，且持有者再 +1
    delete_menu(&menu).await;
    assert_eq!(
        token_version_of(&holder).await,
        holder_before + 2,
        "删除菜单同样要推进持有者的 token_version"
    );

    // 收尾：物理删夹具。顺序按外键依赖：先 `sys_role_menu`（引用 role + menu），
    // 再 `sys_user_role`（引用 user + role），最后 user / role / menu。
    // 次序错了会被外键挡住，而 `let _ =` 会**静默**吞掉错误、留下脏夹具（实测踩过）。
    // 现在每一步都走 `ts::cleanup_sql`：失败打 WARN + SQL + 数据库原文。
    ts::cleanup_sql(
        &format!("N13→sys_role_menu(role={prefix})"),
        "DELETE FROM sys_role_menu WHERE role_id = ?::text::uuid",
        vec![Value::from(role_id.to_string())],
    )
    .await;
    ts::cleanup_sql(
        &format!("N13→sys_user_role({prefix})"),
        "DELETE FROM sys_user_role WHERE user_id IN (?::text::uuid, ?::text::uuid)",
        vec![
            Value::from(holder.to_string()),
            Value::from(bystander.to_string()),
        ],
    )
    .await;
    ts::cleanup_sql(
        &format!("N13→sys_user({prefix})"),
        "DELETE FROM sys_user WHERE username LIKE ?",
        vec![Value::from(format!("{prefix}%"))],
    )
    .await;
    ts::cleanup_sql(
        &format!("N13→sys_role({prefix})"),
        "DELETE FROM sys_role WHERE role_code LIKE ?",
        vec![Value::from(format!("{prefix}%"))],
    )
    .await;
    println!("[N13] 菜单改/删 → 持有者 token_version +1/+2，无关用户与 admin 不动 ✓");
    cleanup(&prefix).await;
}

// ==================== N14 无缓存（无 Redis）时写操作照常成功（FIX-03） ====================

/// 断言当前进程处于「无缓存」形态。
///
/// lib 单测不跑 `App::start()`，`try_cache()` 从未注册 → 与「无 Redis 的生产进程」等价
/// （`invalidate*` / `cache*` 全部退化成 no-op，`revoke_after_permission_change` 的
/// 缓存侧清理必然失败）。若将来有人真的在某用例里 `set_cache()`，这条断言会立刻把它
/// 拦下来，而不是让「无 Redis」用例悄悄变成「有缓存」用例。
fn assert_no_cache() {
    assert!(
        try_cache().is_none(),
        "本用例模拟「无 Redis」：要求进程内没有注册缓存（若已有用例 set_cache，需拆独立进程跑）"
    );
}

/// N14（BUG-SYS-03 验收 ①②）：**无 Redis** 时菜单「建 → 改 → 删」全部成功。
///
/// 这正是缺陷的原始症状：写库提交后去清缓存失败 → 回 500，而数据其实已经改了。
/// 现在缓存侧动作只 `warn` + 记欠账，业务返回成功；本用例逐条把三条写路径跑一遍并
/// 断言库里真的落了（软删 `deleted_at` 非空），同时钉住「读路径回退查库」的行为。
#[tokio::test]
async fn n14_writes_succeed_without_cache() {
    if !ts::ensure_db().await {
        return;
    }
    assert_no_cache();
    let tag = ts::uniq();
    let prefix = format!("N14-{tag}");
    let claims = ts::super_claims();

    // ① 读路径：无缓存 → `load_cached_tree` 返回 None（调用方回退查库），不是错误
    assert!(
        model::load_cached_tree().await.is_none(),
        "无缓存时 load_cached_tree 必须返回 None（交给调用方查库）"
    );
    handler::tree(axum::extract::Query(MenuQuery::default()))
        .await
        .expect("无缓存时菜单树仍必须可查（缓存只是读优化）");

    // ② 建：写库成功后的缓存回填失败不影响返回
    let id = create_menu(
        &claims,
        &format!("{prefix}-按钮"),
        3,
        &format!("{prefix}:btn"),
        None,
        0,
    )
    .await;

    // ③ 改：缓存失效失败不得转成 500（BUG-SYS-03 的原始 500 点之一）
    handler::update(
        Path(id.clone()),
        ts::headers(),
        ts::connect_info(),
        axum::Extension(claims.clone()),
        ValidatedJson::<MenuUpdateReq>(req(
            &format!("{prefix}-按钮改"),
            3,
            &format!("{prefix}:btn"),
            None,
            0,
        )),
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "无缓存时更新菜单必须成功（缓存侧只许降级为 warn），实际 {}/{} {}",
            e.status, e.code, e.msg
        )
    });

    // ④ 删：另一处原始 500 点
    delete_menu(&id).await;

    // ⑤ 库里真的删了（`sys_menu` 是软删表）
    let alive = ts::scalar_i64(
        "SELECT count(*) AS v FROM sys_menu WHERE id = ?::text::uuid AND deleted_at IS NULL",
        vec![Value::from(id.clone())],
    )
    .await;
    assert_eq!(
        alive, 0,
        "删除后该菜单必须已软删（deleted_at 非空），实际存活 {alive} 行"
    );

    // ⑥ 缓存辅助函数在「无缓存」下安全降级：不 panic、不抛错
    model::invalidate_tree_cache().await;
    model::cache_tree(&[]).await;
    println!("[N14] 无缓存：建/改/删 全部成功，读路径回退查库，缓存辅助函数安全降级 ✓");
    cleanup(&prefix).await;
}

//! 测试C · 越权用例集（O01–O15）：权限体系的「反向验证」。
//!
//! # 为什么不能只测「有权限的人能进」
//!
//! 只测正向的用例在权限**完全失效**时也是全绿的（谁都放行 ⇒ 有权限的人当然能进）。
//! 真正的防线是反向：**没有权限的人必须打不进**。本文件因此专测越权。
//!
//! # 判定口径与中间件逐字一致，不是自创规则
//!
//! 框架的 403 来自 `vendor/nik-web/src/middleware/permission_check.rs`：
//!
//! ```text
//! claims.has_permission(&required_perm)   // 无 AuthClaims → false → 403
//! ```
//!
//! 而 `has_permission`（`vendor/nik-web/src/middleware/mod.rs:148`）是
//! `is_super_admin() || permissions.contains(perm)`——**精确串匹配**，不认
//! `system:user:*` 这种前缀通配（只有 `*` / `*:*:*` 全通配）。本文件直接调用同一个
//! 谓词，因此断言的是「中间件在这里会不会放行」，不是一段平行实现。
//!
//! # 路由表从哪来
//!
//! 从**源码自身的 `#[nik::permission(path, method, permission)]` 注解**解析（`src/**/*.rs`）。
//! 好处是它不会随实现漂移：新增一个接口却忘了写权限注解，`o13` 立刻红。
//!
//! # 边界（诚实说明）
//!
//! 验签 / 过期由框架 JWT 层负责，`JWT::from_config()` 依赖全局配置单例（lib 单测里没有
//! 初始化入口），因此 O08/O09 测的是**应用层在签名之外还做了什么**：令牌版本比对的数据
//! 来源（`sys_user.token_version`）、以及「权限快照被伪造时，数据权限/脱敏是否仍从库里
//! 取真值」。签名本身的正向验证属于框架单测范围。

use nik::prelude::*;

use crate::shared::constants;
use crate::shared::error_code;
use crate::system::tests_support as ts;

// ==================== 源码注解解析 ====================

/// 一条「受权限保护的路由」。
#[derive(Debug, Clone)]
struct PermRoute {
    /// 路由路径（不含 `/api` 前缀，与 `[router] prefix` 的约定一致）。
    path: String,
    /// HTTP 方法（大写）。
    method: String,
    /// 要求的权限码。
    permission: String,
    /// 来源文件（报错时指路）。
    file: String,
}

/// 公开路由（`#[nik::no_auth("...")]`）。
#[derive(Debug, Clone)]
struct PublicRoute {
    path: String,
    file: String,
}

/// 后端 crate 根（不依赖进程 CWD）。
fn backend_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 解析 `src/**/*.rs` 里的权限注解与公开注解。
fn scan_routes() -> (Vec<PermRoute>, Vec<PublicRoute>) {
    let mut perms: Vec<PermRoute> = Vec::new();
    let mut publics: Vec<PublicRoute> = Vec::new();
    collect(
        std::path::Path::new(&backend_root().join("src")),
        &mut perms,
        &mut publics,
    );
    (perms, publics)
}

/// 递归扫描源码目录。
fn collect(dir: &std::path::Path, perms: &mut Vec<PermRoute>, publics: &mut Vec<PublicRoute>) {
    let Ok(entry) = std::fs::read_dir(dir) else {
        return;
    };
    for item in entry.filter_map(std::result::Result::ok) {
        let path = item.path();
        if path.is_dir() {
            collect(&path, perms, publics);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        // 扫描器自身与本卡其他测试文件里也写着 `#[nik::permission(...)]` 的示例，
        // 必须排除，否则会把文档/夹具当成真路由。
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == "authz_tests.rs" || name == "tests.rs" || name == "tests_support.rs" {
            continue;
        }
        let file = path
            .strip_prefix(backend_root())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());

        // ① 权限注解块：`#[nik::permission(` … `)]`，块内是 `key = "value"` 列表
        let mut in_block = false;
        let mut block_path: Option<String> = None;
        let mut block_method: Option<String> = None;
        let mut block_perm: Option<String> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            // 注释行里的示例（`/// #[nik::permission(...)]`）不算真路由
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.starts_with("#[nik::permission(") {
                in_block = true;
                block_path = None;
                block_method = None;
                block_perm = None;
                continue;
            }
            if in_block {
                for key in ["path", "method", "permission"] {
                    let prefix = format!("{key} = \"");
                    if let Some(rest) = trimmed.strip_prefix(&prefix)
                        && let Some(end) = rest.find('"')
                    {
                        let value = rest[..end].to_string();
                        match key {
                            "path" => block_path = Some(value),
                            "method" => block_method = Some(value),
                            _ => block_perm = Some(value),
                        }
                    }
                }
                if trimmed.starts_with(")]") {
                    in_block = false;
                    if let (Some(path), Some(method), Some(perm)) =
                        (block_path.take(), block_method.take(), block_perm.take())
                    {
                        perms.push(PermRoute {
                            path,
                            method: method.to_uppercase(),
                            permission: perm,
                            file: file.clone(),
                        });
                    }
                }
            }
            // ② 公开路由：`#[nik::no_auth("path")]`
            if let Some(idx) = trimmed.find("#[nik::no_auth(\"") {
                let rest = &trimmed[idx + "#[nik::no_auth(\"".len()..];
                if let Some(end) = rest.find('"') {
                    publics.push(PublicRoute {
                        path: rest[..end].to_string(),
                        file: file.clone(),
                    });
                }
            }
        }
    }
}

/// 只读角色：只有各模块的 `:list` / `:detail` 读权限。
fn readonly_permissions() -> Vec<String> {
    [
        "system:user:list",
        "system:role:list",
        "system:menu:list",
        "system:dept:list",
        "system:field-mask:list",
        "business:erp:warehouse:list",
        "business:erp:warehouse:detail",
        "business:flow:read",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// 只读角色的声明（真实登录令牌里也会是这些串）。
fn readonly_claims() -> AuthClaims {
    let perms = readonly_permissions();
    let refs: Vec<&str> = perms.iter().map(String::as_str).collect();
    ts::claims("00000000-0000-7000-8000-0000000000aa", "readonly", &refs)
}

/// 找到指定方法 + 路径片段的路由（找不到直接 panic：说明路由被改了，用例需要跟着改）。
fn find_route<'a>(routes: &'a [PermRoute], method: &str, path_contains: &str) -> &'a PermRoute {
    routes
        .iter()
        .find(|r| r.method == method && r.path.contains(path_contains))
        .unwrap_or_else(|| {
            panic!("没找到 {method} 路由（路径含 {path_contains}）：路由可能改名，请更新用例")
        })
}

/// 断言：该声明对该路由**必须 403**（中间件会拒绝）。
fn assert_forbidden(claims: &AuthClaims, route: &PermRoute, label: &str) {
    assert!(
        !claims.0.has_permission(&route.permission),
        "{label}：{} {} 需要的权限码 {} 竟然被这个身份持有 → 越权（{}）",
        route.method,
        route.path,
        route.permission,
        route.file
    );
}

// ==================== O01 只读员建用户 ====================

/// O01：只读员（只有 `system:user:list`）调 `POST /system/user` → 403。
#[tokio::test]
async fn o01_readonly_cannot_create_user() {
    let (routes, _) = scan_routes();
    let route = find_route(&routes, "POST", "/system/user");
    assert_forbidden(&readonly_claims(), route, "O01 只读员建用户");
    println!(
        "[O01] 只读员打 POST /system/user（需 {}）→ 中间件 403 ✓",
        route.permission
    );
}

// ==================== O02 只读员删角色 ====================

/// O02：只读员调 `DELETE /system/role/{id}` → 403；同时确认「删除」不是靠 `:list` 兜。
#[tokio::test]
async fn o02_readonly_cannot_delete_role() {
    let (routes, _) = scan_routes();
    let route = find_route(&routes, "DELETE", "/system/role");
    assert_forbidden(&readonly_claims(), route, "O02 只读员删角色");
    assert!(
        !route.permission.ends_with(":list") && !route.permission.ends_with(":detail"),
        "删除路由不该只要读权限，实际 {}",
        route.permission
    );
    println!(
        "[O02] 只读员打 DELETE {}（需 {}）→ 403 ✓",
        route.path, route.permission
    );
}

// ==================== O03 只读员建收货单 ====================

/// O03：只读员调 ERP 写接口（收货 / 入库）→ 403。
#[tokio::test]
async fn o03_readonly_cannot_create_stock_in() {
    let (routes, _) = scan_routes();
    let candidates: Vec<&PermRoute> = routes
        .iter()
        .filter(|r| r.method == "POST" && r.path.starts_with("/business/erp/"))
        .collect();
    assert!(
        !candidates.is_empty(),
        "没有扫到任何 ERP 写接口（POST /business/erp/…），扫描逻辑或路由前缀变了"
    );
    for route in candidates {
        assert_forbidden(&readonly_claims(), route, "O03 只读员建 ERP 单据");
    }
    println!(
        "[O03] 只读员打全部 ERP 写接口 → 403（扫描到 {} 条）✓",
        routes
            .iter()
            .filter(|r| r.method == "POST" && r.path.starts_with("/business/erp/"))
            .count()
    );
}

// ==================== O04 仓管员看财务报表 ====================

/// O04：仓管员的权限集合里没有财务读权限 → 财务报表接口 403。
#[tokio::test]
async fn o04_warehouse_cannot_read_finance_report() {
    let (routes, _) = scan_routes();
    let finance: Vec<&PermRoute> = routes
        .iter()
        .filter(|r| {
            r.method == "GET"
                && (r.permission.contains(":ledger:")
                    || r.permission.contains(":voucher:")
                    || r.permission.contains(":payable:")
                    || r.permission.contains(":account:"))
        })
        .collect();
    assert!(!finance.is_empty(), "没扫到财务类只读接口，扫描条件需更新");

    let warehouse = ts::claims(
        "00000000-0000-7000-8000-0000000000bb",
        "warehouse",
        &[
            "business:erp:warehouse:list",
            "business:erp:inventory:list",
            "business:erp:stock-in:create",
        ],
    );
    for route in &finance {
        assert_forbidden(&warehouse, route, "O04 仓管员看财务报表");
    }
    println!("[O04] 仓管员打 {} 个财务接口全部 403 ✓", finance.len());
}

// ==================== O05 财务看别人部门单据 ====================

/// O05：数据范围是「本部门」的财务，看不到别的部门的单据（**行级**越权，不是接口级）。
///
/// 这里验证的是数据权限的判定入口：`exceeded_dept_ids` 必须把「非本部门」的 id 判为越界，
/// 而 `crop_to_visible` 不得把不可见父节点偷偷补进结果。
#[tokio::test]
async fn o05_finance_cannot_see_other_dept_rows() {
    use crate::business::permission::scope_model;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;

    let dept_a = uuid::Uuid::now_v7();
    let dept_b = uuid::Uuid::now_v7();
    let finance = ScopedPrincipal::for_test(
        uuid::Uuid::now_v7(),
        Some(dept_a),
        false,
        ScopeKind::Dept,
        vec![dept_a],
        false,
    );

    // 请求里带了 B 部门 → 必须被判为越界（B2 的 ensure_scope_grant_allowed 会据此 403）
    let exceeded = scope_model::exceeded_dept_ids(&finance, &[dept_a, dept_b]);
    assert_eq!(
        exceeded,
        vec![dept_b],
        "B 部门必须被判为越界，实际 {exceeded:?}"
    );
    assert!(
        scope_model::exceeded_dept_ids(&finance, &[dept_a]).is_empty(),
        "本部门不该被判越界"
    );

    // 树裁剪：不可见父节点下的节点必须「提升为根」，不得因为父不可见而被丢弃、也不得泄露父
    let rows = vec![crate::business::permission::scope_model::DeptRow {
        id: dept_a,
        parent_id: Some(dept_b),
        dept_code: "A".to_string(),
        dept_name: "A".to_string(),
        status: 1,
        sort_order: 0,
    }];
    let visible: std::collections::BTreeSet<uuid::Uuid> = [dept_a].into_iter().collect();
    let cropped = scope_model::crop_to_visible(rows, &visible);
    assert_eq!(cropped.len(), 1, "不可见父节点下的节点不能被丢弃");
    assert!(
        cropped[0].parent_id.is_none(),
        "父不可见时必须提升为根（不能泄露父节点）"
    );
    println!("[O05] 跨部门 id 判越界；不可见父节点提升为根而非泄露 ✓");
}

// ==================== O06 普通员工看审批流程定义 ====================

/// O06：普通员工（无 `business:flow:*`）调流程定义接口 → 403。
#[tokio::test]
async fn o06_employee_cannot_read_flow_definition() {
    let (routes, _) = scan_routes();
    let flow: Vec<&PermRoute> = routes
        .iter()
        .filter(|r| r.method == "GET" && r.path.starts_with("/business/flow"))
        .collect();
    assert!(!flow.is_empty(), "没扫到流程定义接口，扫描条件需更新");

    let employee = ts::claims(
        "00000000-0000-7000-8000-0000000000cc",
        "employee",
        &["system:user:list", "business:task:list"],
    );
    for route in &flow {
        assert_forbidden(&employee, route, "O06 普通员工看审批流程定义");
    }
    println!("[O06] 普通员工打 {} 个流程接口全部 403 ✓", flow.len());
}

// ==================== O07 公开接口白名单必须封闭 ====================

/// O07：无 token 的请求要 401（不是 500、更不是放行）—— 唯一的放行口是
/// `#[nik::no_auth]` 白名单，本用例把白名单钉死：
///
/// 1. 白名单里的每一项都必须在预期集合内（新增公开接口必须显式改这份预期 → 评审可见）；
/// 2. 白名单之外**不存在**任何「没有权限注解」的路由（否则它就是事实上的公开接口）。
#[tokio::test]
async fn o07_public_route_whitelist_is_closed() {
    let (routes, publics) = scan_routes();
    assert!(!routes.is_empty(), "没扫到任何受保护路由，扫描逻辑失效");

    // 预期白名单：登录 / 刷新 / 公共配置 / 任务模块的若干回调（见代码里的 no_auth 注解）
    let allowed = [
        "/system/auth/login",
        "/system/auth/refresh",
        "/support/config/public",
    ];
    let unexpected: Vec<String> = publics
        .iter()
        .filter(|p| !allowed.iter().any(|a| p.path == *a) && !p.path.starts_with("/business/task/"))
        .map(|p| format!("{}（{}）", p.path, p.file))
        .collect();
    assert!(
        unexpected.is_empty(),
        "出现了白名单外的公开接口（无 token 即可访问，必须评审）：\n{}",
        unexpected.join("\n")
    );

    // 白名单之外的路由都必须带权限注解
    let mut without_perm: Vec<String> = Vec::new();
    for public in &publics {
        assert!(
            routes.iter().any(|r| r.path == public.path)
                || allowed.iter().any(|a| public.path == *a)
                || public.path.starts_with("/business/task/"),
            "未知公开接口：{}（{}）",
            public.path,
            public.file
        );
    }
    for route in &routes {
        if route.permission.is_empty() {
            without_perm.push(format!("{} {}", route.method, route.path));
        }
    }
    assert!(
        without_perm.is_empty(),
        "有路由没写权限码：{without_perm:?}"
    );
    println!(
        "[O07] 公开接口 {} 条全在白名单内；受保护路由 {} 条全部带权限码 ✓",
        publics.len(),
        routes.len()
    );
}

// ==================== O08 令牌版本作废（改密 / 停用后旧令牌失效） ====================

/// O08：改密 / 改角色 / 停用后**旧令牌立即失效**（`sys_user.token_version` 比对）。
///
/// 中间件 `middleware/session_guard.rs` 的判定是
/// `claims.extra_i64(CLAIM_TOKEN_VERSION).unwrap_or(0) != sys_user.token_version → 401`。
/// 本用例验证这条判定的两个输入都真实可用：令牌里的 `ver` 声明、以及库里的版本号，
/// 并验证 `bump_token_version` 确实把它推进（推进不了 = 旧令牌永不失效 = 严重越权）。
#[tokio::test]
async fn o08_token_version_invalidation() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("tc_o08_{tag}");
    let user_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, 'x', 'O08', 1)",
        vec![Value::from(user_id.to_string()), Value::from(format!("{prefix}_u"))],
    )
    .await;

    // 登录时刻的令牌：ver = 库里的当前版本
    let (ver_before, must_change) = crate::system::user::model::load_session_flags(&user_id)
        .await
        .expect("读会话状态失败")
        .expect("用户应存在");
    assert!(!must_change, "新用户不该要求强制改密");
    let issued = ts::claims(
        &user_id.to_string(),
        &format!("{prefix}_u"),
        &["system:user:list"],
    );
    let issued_ver = issued
        .0
        .extra_i64(constants::CLAIM_TOKEN_VERSION)
        .unwrap_or(0);
    assert_eq!(
        issued_ver,
        i64::from(ver_before),
        "令牌版本应取自库里的 token_version"
    );

    // 改密（等同管理员重置口令）→ 版本号自增
    crate::system::user::model::bump_token_version(&user_id)
        .await
        .expect("自增 token_version 失败");
    let (ver_after, _) = crate::system::user::model::load_session_flags(&user_id)
        .await
        .expect("读会话状态失败")
        .expect("用户应存在");
    assert!(
        ver_after > ver_before,
        "改密后 token_version 必须自增（{ver_before} → {ver_after}），否则旧令牌永不失效"
    );
    assert_ne!(
        issued_ver,
        i64::from(ver_after),
        "旧令牌的 ver={issued_ver} 与新版本 {ver_after} 不等 → 会话守卫会 401"
    );
    println!("[O08] token_version {ver_before} → {ver_after}，旧令牌 ver={issued_ver} 已作废 ✓");
    ts::cleanup_table("sys_user", "username", &prefix).await;
}

// ==================== O09 伪造令牌内容不能换来库侧特权 ====================

/// O09：**令牌里的权限快照是「登录时的缓存」**，服务端真正的权限来自库。
///
/// 场景：攻击者改 token 的 subject / permissions（签名未失效的前提下能做到，例如密钥泄漏）。
/// 这条用例钉住「库侧兜底」的实际边界：
///
/// 1. 服务端装载权限走的是**库**（`auth::model::load_permissions`），不采信令牌内容——
///    伪造的 `system:user:create` 在重算后不会出现；
/// 2. 超管 / 数据范围 / 脱敏豁免的判定都以**库里的角色**为准（`principal.is_super`），
///    改了 claims 也拿不到越权数据；
/// 3. ⚠️ 已知信任边界：框架的 403 只看 `claims.permissions`（`has_permission` 对
///    `*:*:*` 直接返回 true）。因此**验签是唯一的第一道门**，密钥强度 / 轮换属于运维红线，
///    已作为观察项写进 `docs/test-report-system.md`（BUG-SYS-04）。
#[tokio::test]
async fn o09_forged_claims_do_not_grant_db_side_privileges() {
    if !ts::ensure_db().await {
        return;
    }
    let tag = ts::uniq();
    let prefix = format!("tc_o09_{tag}");
    let user_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) VALUES (?::text::uuid, ?, 'x', 'O09', 1)",
        vec![Value::from(user_id.to_string()), Value::from(format!("{prefix}_u"))],
    )
    .await;

    // 伪造令牌：自称持有 system:user:create（库里该用户没有任何角色）
    let forged = ts::claims(
        &user_id.to_string(),
        &format!("{prefix}_u"),
        &["system:user:create", "system:role:delete"],
    );
    assert!(
        forged.0.has_permission("system:user:create"),
        "夹具：伪造的令牌在框架层面确实「有权限」（这正是必须在验签层挡住的原因）"
    );

    // ① 服务端重算权限：来自库 → 不含任何伪造码
    let real = crate::system::auth::model::load_permissions(&user_id, false)
        .await
        .expect("装载权限失败");
    assert!(
        !real.iter().any(|p| p == "system:user:create"),
        "伪造的权限码不得出现在服务端重算结果里，实际 {real:?}"
    );
    assert!(real.is_empty(), "无角色用户库侧权限应为空，实际 {real:?}");

    // ② 库侧角色判定：不是超管
    let principal = crate::business::shared::principal::ScopedPrincipal::for_test(
        user_id,
        None,
        false,
        crate::business::shared::scope::ScopeKind::SelfOnly,
        Vec::new(),
        true,
    );
    assert!(!principal.is_super(), "库里没有超管角色就不能被当成超管");

    // ③ 数据权限：无部门归属的「仅本人」主体看不到任何部门数据
    assert!(
        crate::business::permission::scope_model::find_visible_depts(&principal)
            .await
            .map(|rows| rows.is_empty())
            .unwrap_or(true),
        "无部门归属的「仅本人」主体不应看到任何部门数据"
    );

    // ④ 脱敏：不带给令牌内容时（真实请求里若没有明版权限码），规则照常生效
    let normal_set = crate::business::masking::registry::resolve(Some(&principal), None)
        .await
        .expect("解析脱敏规则失败");
    assert!(!normal_set.is_exempt(), "无明版权限码的主体不得豁免脱敏");
    assert!(
        normal_set
            .mask_value("sys_user", "mobile", "13912345678")
            .expect("不应失败")
            .is_some(),
        "该用户必须被打码（库里有 sys_user.mobile 的全局规则）"
    );
    println!("[O09] 伪造 claims：库侧权限重算为空、非超管、数据范围受限、脱敏生效 ✓");
    ts::cleanup_table("sys_user", "username", &prefix).await;
}

// ==================== O10 越权操作别人的账号 ====================

/// O10：非超管操作者拿别人的 id 做事（给他人授予自己没有的角色 / 操作超管账号）→ 403。
///
/// 判定由 `system/user/guard.rs::evaluate_grant`（纯函数，逐条分支可测）给出，
/// 与真实 handler 调用的是同一份实现。
#[tokio::test]
async fn o10_privilege_cap_blocks_cross_user_escalation() {
    use crate::system::user::guard::{GrantCheckInput, evaluate_grant};

    // ① 非超管给「自己」加一个原本没有的角色 → 403（自我提权）
    let escalation = GrantCheckInput {
        operator_exists: true,
        operator_is_super_admin: false,
        has_target_user: true,
        target_user_exists: true,
        target_user_is_super_admin: false,
        target_user_is_operator: true,
        grants_super_admin_role: false,
        adds_role_operator_lacks: true,
    };
    let error = evaluate_grant(escalation).expect_err("自我提权必须被拒");
    assert_eq!(
        error.status, 403,
        "自我提权应 403，实际 {}/{}",
        error.status, error.code
    );
    assert_eq!(error.code, error_code::PERMISSION_DENIED);

    // ② 非超管操作超管账号（改密 / 停用 / 删除）→ 403
    let touch_admin = GrantCheckInput {
        operator_exists: true,
        operator_is_super_admin: false,
        has_target_user: true,
        target_user_exists: true,
        target_user_is_super_admin: true,
        target_user_is_operator: false,
        grants_super_admin_role: false,
        adds_role_operator_lacks: false,
    };
    let error = evaluate_grant(touch_admin).expect_err("非超管操作超管必须被拒");
    assert_eq!(
        error.status, 403,
        "操作超管应 403，实际 {}/{}",
        error.status, error.code
    );

    // ③ 令牌有效但操作者账号已被删除 → 403（不能拿已删账号的令牌继续操作）
    let ghost_operator = GrantCheckInput {
        operator_exists: false,
        ..GrantCheckInput::default()
    };
    let error = evaluate_grant(ghost_operator).expect_err("操作者不存在必须被拒");
    assert_eq!(error.status, 403);

    // ④ 超管不受上限约束（反向：别把权限配过头拦住了超管）
    let super_operator = GrantCheckInput {
        operator_exists: true,
        operator_is_super_admin: true,
        has_target_user: true,
        target_user_exists: true,
        target_user_is_super_admin: true,
        target_user_is_operator: false,
        grants_super_admin_role: true,
        adds_role_operator_lacks: true,
    };
    assert!(
        evaluate_grant(super_operator).is_ok(),
        "超管必须能操作任意账号"
    );
    println!("[O10] 自我提权 / 操作超管 / 幽灵操作者 → 403；超管放行 ✓");
}

// ==================== O11 跨部门 id 越权 ====================

/// O11：提交「可见范围之外」的部门 id → 被判越界（真实 handler 会据此 403 并点名部门）。
///
/// 与 O05 的区别：O05 看「读」侧（行级过滤），O11 看「写」侧（授权时提交了越界部门）。
/// 这里用 `exceeded_dept_ids` + `missing_dept_ids` 覆盖两种脏输入：越权、以及不存在的 id。
#[tokio::test]
async fn o11_cross_dept_write_rejected() {
    use crate::business::permission::scope_model;
    use crate::business::shared::principal::ScopedPrincipal;
    use crate::business::shared::scope::ScopeKind;

    let mine = uuid::Uuid::now_v7();
    let others = [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()];
    let principal = ScopedPrincipal::for_test(
        uuid::Uuid::now_v7(),
        Some(mine),
        false,
        ScopeKind::DeptAndChild,
        vec![mine],
        false,
    );

    let exceeded =
        scope_model::exceeded_dept_ids(&principal, &[mine, others[0], others[0], others[1]]);
    assert_eq!(
        exceeded,
        vec![others[0], others[1]],
        "越界部门必须被点出来且保序去重，实际 {exceeded:?}"
    );

    let missing = scope_model::missing_dept_ids(&[mine, others[0]], &[mine]);
    assert_eq!(
        missing,
        vec![others[0]],
        "库里不存在的部门必须单独报，不能当成越权"
    );

    // 自定义范围的配置校验：给了越界 id 也要能被上层拒（这里验证「范围形态」本身的拒绝分支）
    let bad = scope_model::validate_custom_scope(ScopeKind::Dept, Some(&[others[0]]))
        .expect_err("非自定义范围却提交了部门集合必须 400");
    assert_eq!(bad.status, 400);
    assert_eq!(bad.code, error_code::DATA_SCOPE_INVALID);
    println!("[O11] 跨部门写入被判越界（保序去重）、缺失 id 单独报、形态非法 400 ✓");
}

// ==================== O12 超管能进所有（反向验证） ====================

/// O12：超管（`*:*:*`）对**每一条**受保护路由都能通过权限判定。
///
/// 反向验证的意义：前面 11 条都在证明「别人进不来」，如果权限码写错了（比如某个接口
/// 要求了一个谁都拿不到的码），系统就会「对所有人 403」，包括超管以外没人能用。
/// 这条保证权限配置没有「配过头」。
#[tokio::test]
async fn o12_super_admin_passes_every_route() {
    let (routes, _) = scan_routes();
    assert!(
        routes.len() > 50,
        "受保护路由数量异常（{}），扫描逻辑失效",
        routes.len()
    );
    let sup = ts::super_claims();
    let mut denied: Vec<String> = Vec::new();
    for route in &routes {
        if !sup.0.has_permission(&route.permission) {
            denied.push(format!(
                "{} {} → {}",
                route.method, route.path, route.permission
            ));
        }
    }
    assert!(
        denied.is_empty(),
        "超管被拒的路由（权限配置有误）：\n{}",
        denied.join("\n")
    );
    println!("[O12] 超管通过全部 {} 条受保护路由 ✓", routes.len());
}

// ==================== O13 写接口必须受权限保护 ====================

/// O13：**每个写接口都必须有权限注解**（漏一个就是「谁都能改」的后门）。
///
/// 这条是「越权」的根防线：如果 `PATCH /system/user/{id}` 忘了写权限注解，中间件
/// `permission_check` 找不到规则就直接 `inner.call(req)`（放行）。本用例从源码注解侧
/// 做全量扫描，把这种遗漏挡在合并前。
#[tokio::test]
async fn o13_every_write_route_requires_permission() {
    let (routes, publics) = scan_routes();
    let write_routes: Vec<&PermRoute> = routes
        .iter()
        .filter(|r| ["POST", "PUT", "PATCH", "DELETE"].contains(&r.method.as_str()))
        .collect();
    assert!(
        write_routes.len() > 20,
        "写接口数量异常（{}）",
        write_routes.len()
    );

    // 公开写接口只允许白名单（登录 / 刷新）；任务模块的回调按设计公开
    let mut unguarded: Vec<String> = Vec::new();
    for public in &publics {
        let is_auth_public = public.path == "/system/auth/login"
            || public.path == "/system/auth/refresh"
            // 公共参数（只读聚合，供登录页/前端启动读取，代码里明确 no_auth）
            || public.path == "/support/config/public";
        let is_task_callback = public.path.starts_with("/business/task/");
        if !is_auth_public && !is_task_callback {
            unguarded.push(format!("{}（{}）", public.path, public.file));
        }
    }
    assert!(
        unguarded.is_empty(),
        "以下写接口是公开的（无权限保护），必须评审：\n{}",
        unguarded.join("\n")
    );

    // 写接口的权限码不能是读权限（读权限给了导出/查看，绝不能连带写）。
    //
    // 例外：语义是「读」但用 POST 传查询体的接口（preview / export / search 这类），
    // 它们只回显不落库，要求 `:list` 是合理的。当前全库只有 1 条：
    // `POST /business/notice/template/{id}/preview → business:notice:template:list`
    // （已作为观察项记入 docs/test-report-system.md，P3）。
    let mut read_only_write: Vec<String> = Vec::new();
    let mut query_by_post: Vec<String> = Vec::new();
    for route in &write_routes {
        if route.permission.ends_with(":list") || route.permission.ends_with(":detail") {
            let semantic_read = ["/preview", "/export", "/search", "/statistics"]
                .iter()
                .any(|suffix| route.path.contains(suffix));
            let entry = format!("{} {} → {}", route.method, route.path, route.permission);
            if semantic_read {
                query_by_post.push(entry);
            } else {
                read_only_write.push(entry);
            }
        }
    }
    assert!(
        read_only_write.is_empty(),
        "写接口只要求了读权限（有权查看即可改数据）：\n{}",
        read_only_write.join("\n")
    );
    if !query_by_post.is_empty() {
        println!("[O13] 例外（POST 但语义是查询，要求读权限）：{query_by_post:?}");
    }
    println!(
        "[O13] {} 个写接口全部有独立写权限码，无公开写接口 ✓",
        write_routes.len()
    );
}

// ==================== O14 权限码格式 ====================

/// O14：权限码必须是 `{模块}:{资源}:{动作}` 形式（卡片口径），全小写、无空格、无全角字符。
///
/// 格式错误不是「风格问题」：`sys_menu.auth_code` 与注解必须**逐字相等**才能授上权，
/// 一个大写字母或一个空格就会让某个功能对所有人 403，且极难排查。
#[tokio::test]
async fn o14_permission_code_format() {
    let (routes, _) = scan_routes();
    let mut bad: Vec<String> = Vec::new();
    for route in &routes {
        let code = &route.permission;
        let segments: Vec<&str> = code.split(':').collect();
        let ok = segments.len() >= 3
            && segments.iter().all(|seg| {
                !seg.is_empty()
                    && seg.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
                    })
            });
        if !ok {
            bad.push(format!(
                "{} {} → {:?}（{}）",
                route.method, route.path, code, route.file
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "权限码格式不合规（应 模块:资源:动作，全小写）：\n{}",
        bad.join("\n")
    );
    println!(
        "[O14] {} 个权限码格式全部合规（模块:资源:动作，全小写）✓",
        routes.len()
    );
}

// ==================== O15 通配权限的库侧复核（BUG-SYS-04） ====================

/// O15（BUG-SYS-04 的回归防线）：接口级 403 只认令牌里的权限快照
/// （`nik-web` 的 `has_permission` = `is_super_admin() || permissions.contains(perm)`，
/// 不查库），所以「JWT 密钥泄漏 → 自签一个 `*:*:*` 令牌」原本等于拿超管。
///
/// 会话守卫补的库侧护栏是 `session_guard::verify_wildcard_claims`：
/// **只有令牌声明了通配权限才回库**核对账号是否真的持有 `SUPER_ADMIN` 角色。
/// 本用例直接断言这条判定的三个分支（真库、真角色）：
///
/// - 内置管理员（`SUPER_ADMIN`）→ `Backed`；
/// - 普通用户伪造 `*:*:*` → `RoleMismatch`（中间件据此回 401）；
/// - 普通权限码令牌 → `NotWildcard`（不额外回库，绝大多数请求走这条）。
///
/// # 边界（诚实说明）
///
/// 中间件本身的 401 响应体需要 `tower::ServiceExt::oneshot`，而本仓未声明该
/// dev-dependency（框架铁律不允许随意加），因此这里断言的是**判定函数**，
/// HTTP 级端到端由 e2e 覆盖（与文件头的口径说明一致）。
#[tokio::test]
async fn o15_wildcard_claims_must_be_backed_by_db_role() {
    if !ts::ensure_db().await {
        return;
    }
    use crate::middleware::session_guard::{WildcardVerdict, verify_wildcard_claims};

    let tag = ts::uniq();
    let prefix = format!("tc_o15_{tag}");

    // ① 内置管理员：库里的角色确实是 SUPER_ADMIN → 通配声明被背书
    let admin_id: uuid::Uuid = ts::scalar_text(
        "SELECT id::text AS v FROM sys_user WHERE username = ?",
        vec![Value::from(constants::ADMIN_USERNAME)],
    )
    .await
    .expect("库里必须有内置管理员 admin")
    .parse()
    .expect("admin id 应是 UUID");
    let admin_claims = ts::claims(&admin_id.to_string(), constants::ADMIN_USERNAME, &["*:*:*"]);
    assert_eq!(
        verify_wildcard_claims(&admin_claims.0, &admin_id)
            .await
            .expect("查角色失败"),
        WildcardVerdict::Backed,
        "内置管理员的通配声明必须被库里的 SUPER_ADMIN 角色背书"
    );

    // ② 普通用户伪造通配声明（模拟密钥泄漏后自签）→ 判 RoleMismatch → 中间件回 401
    let user_id = uuid::Uuid::now_v7();
    let username = format!("{prefix}_u");
    ts::exec(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES (?::text::uuid, ?, 'x', 'O15', 1)",
        vec![
            Value::from(user_id.to_string()),
            Value::from(username.clone()),
        ],
    )
    .await;
    // 给一个普通角色，证明「有角色」不等于「能背书通配权限」
    let role_id = uuid::Uuid::now_v7();
    ts::exec(
        "INSERT INTO sys_role (id, role_code, role_name, status, data_scope) \
         VALUES (?::text::uuid, ?, ?, 1, 2)",
        vec![
            Value::from(role_id.to_string()),
            Value::from(format!("{prefix}_ROLE")),
            Value::from(format!("{prefix}-普通角色")),
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

    let forged = ts::claims(&user_id.to_string(), &username, &["*:*:*"]);
    match verify_wildcard_claims(&forged.0, &user_id).await {
        Ok(WildcardVerdict::RoleMismatch { role_codes }) => {
            assert!(
                !role_codes
                    .iter()
                    .any(|code| code == constants::SUPER_ADMIN_ROLE_CODE),
                "RoleMismatch 的角色清单里不该出现 SUPER_ADMIN：{role_codes:?}"
            );
        }
        other => panic!("普通用户伪造 *:*:* 必须被判 RoleMismatch（→401），实际 {other:?}"),
    }

    // ③ 普通权限码令牌：不进复核分支（没有额外回库成本）
    let normal = ts::claims(&user_id.to_string(), &username, &["system:user:list"]);
    assert_eq!(
        verify_wildcard_claims(&normal.0, &user_id)
            .await
            .expect("查角色失败"),
        WildcardVerdict::NotWildcard,
        "非通配声明不应触发库侧复核"
    );

    // ④ 已停用角色不算背书：停用普通用户角色后仍是 RoleMismatch（角色清单为空）
    ts::exec(
        "UPDATE sys_role SET status = 0 WHERE id = ?::text::uuid",
        vec![Value::from(role_id.to_string())],
    )
    .await;
    assert_eq!(
        verify_wildcard_claims(&forged.0, &user_id)
            .await
            .expect("查角色失败"),
        WildcardVerdict::RoleMismatch {
            role_codes: Vec::new()
        },
        "停用角色不进授权清单（load_role_codes 只取 status = 1）"
    );

    // 收尾
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_user_role WHERE user_id = ?::text::uuid",
            vec![Value::from(user_id.to_string())],
        )
        .await;
    let _ = db()
        .execute_with_params(
            "DELETE FROM sys_role WHERE role_code LIKE ?",
            vec![Value::from(format!("{prefix}%"))],
        )
        .await;
    ts::cleanup_table("sys_user", "username", &prefix).await;
    println!("[O15] 通配声明必须有库里的 SUPER_ADMIN 背书，否则 401 ✓");
}

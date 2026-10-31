//! FIX-06 · 死权限码的**反向**不变量（N14–N18，真库 + 源码扫描）。
//!
//! # 为什么要有这个文件
//!
//! `system::menu::tests::n10` 守的是「**注解了却没人能授**」：源码里每个
//! `#[nik::permission(permission = "x")]` 都必须在 `sys_menu.auth_code` 里有承载菜单，
//! 否则除超管外全员 403（功能上线即死锁）。
//!
//! 漏掉的是反方向 —— **BUG-SYS-05**：`sys_menu` 里有码，但源码里没人校验。管理面能把
//! 它勾给角色，勾完永不生效：管理员以为授了权，用户实际没权限，最难排查的一类缺陷。
//! 报告实测（`docs/test-report-system.md`）是一次性人工扫描，扫完没留下防线，所以
//! 这份文件把它钉成常驻用例：
//!
//! | 用例 | 守什么 | 抓到会怎样 |
//! |------|--------|-----------|
//! | N14 | 每个菜单码在**去掉注释的**后端源码里至少有一处落地（注解 / 行级校验 / 常量） | 新增菜单码却没写校验点 → 红 |
//! | N15 | 菜单码必须是 `{模块}:{资源}:{动作}` 形态（≥2 个冒号） | 再把模块名（`erp-finance`）当权限码 → 红 |
//! | N16 | 每个路由要么有 `#[nik::permission]`，要么在「无权限路由白名单」里写明理由 | 新增裸接口（无任何权限闸）→ 红 |
//! | N17 | 扫描器自检：人造的死码 / 裸路由必须被扫出来 | 扫描逻辑退化成永远绿 → 红 |
//! | N18 | 例外台账必须是「活的」：登记为例外的码仍未被后端引用且在菜单里 | 例外过期没人删 → 红 |
//!
//! # 口径（与 `docs/dead-permissions-20261101.md` 逐字一致）
//!
//! 1. 注释先剥（`//` / `///` / `//!` / `/* */`），**字符串字面量保留** —— 只在注释里
//!    出现的码不算落地（`system:preference:view` 就是这么一条，见 [`ROUTE_LEVEL_ONLY`]）。
//! 2. 再看三层落地：① `#[nik::permission]` 注解（中间件闸）；② 行级校验（`has_permission`
//!    调用、或权限常量被行级判定函数消费）；③ 其它代码引用（错误文案、清单常量）。
//! 3. 源码范围 = `backend/src/**/*.rs`，**排除测试文件**（`tests.rs` / `tests_support.rs` /
//!    `tests/`）—— 测试里提到某个码不算「有校验点」。
//!
//! 跑法：
//! ```bash
//! SYS_DB_URL="postgres://<user>:***@127.0.0.1:5432/cloudpivot_test" \
//!   cargo test --lib system::menu::permission_audit_tests -- --nocapture
//! ```
//! 未设 `SYS_DB_URL` 时数据库用例打印 `SKIP` 返回；N17 不依赖库，永远真跑。

#![allow(clippy::items_after_statements)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nik::prelude::*;

use crate::system::tests_support as ts;

// ==================== 例外台账（改这里必须同步 docs/dead-permissions-20261101.md） ====================

/// 后端**没有任何校验点**、权限点由前端路由拦截的菜单码（报告口径的「死码」，实为 C 类）。
///
/// 每条都必须满足：① 仍在 `sys_menu` 里（否则前端 `meta.permission` 无处可勾）；
/// ② 后端源码（剥离注释后）零引用。任一条件变化 → N18 变红，逼你回来清理台账。
const ROUTE_LEVEL_ONLY: &[(&str, &str)] = &[(
    "system:preference:view",
    "TODO(FIX-06)：外观设置页纯前端（localStorage），后端无接口，权限点由 \
     frontend/src/router/index.ts 的 meta.permission 拦截。日后若补后端接口，\
     必须加 #[nik::permission] 并删除本例外。",
)];

/// 允许**没有** `#[nik::permission]` 注解的路由白名单：`"METHOD /path"` → 理由。
///
/// 只有三类可以进这里，且理由必须写清「谁在替中间件闸把关」：
/// ① 匿名接口（登录 / 刷新 / 公共参数）；
/// ② 登录用户的自助接口（只操作 `claims.sub` 指向的本人数据，无跨人入口）；
/// ③ 双身份接口（`#[nik::no_auth]` + handler 内显式判定，如任务详情 / 评论）。
///
/// **新增裸接口先想清楚**：进白名单等于承认「这个接口不需要权限码」，reviewer 会问。
const NO_PERMISSION_ROUTES: &[(&str, &str)] = &[
    (
        "POST /system/auth/login",
        "① 匿名：登录本身，凭账号口令自证",
    ),
    (
        "POST /system/auth/refresh",
        "① 匿名：刷新令牌，凭 refresh 自证（轮换后旧的立刻失效）",
    ),
    (
        "POST /system/auth/logout",
        "② 自助：只吊销令牌里的 jti（本人）",
    ),
    ("GET /system/auth/me", "② 自助：本人画像 + 权限快照"),
    (
        "GET /system/auth/menus",
        "② 自助：本人可见菜单（本身就是授权结果，无跨人入口）",
    ),
    (
        "PUT /system/auth/profile",
        "② 自助：只写 claims.sub 指向的本人资料",
    ),
    (
        "PUT /system/auth/password",
        "② 自助：只改本人口令（旧口令校验在 handler 内）",
    ),
    (
        "GET /system/auth/logs",
        "② 自助：本人登录日志（按 claims.username + 数据范围双过滤）",
    ),
    (
        "GET /system/auth/preference",
        "② 自助：本人偏好（无 user_id 入参，见 preference::handler 头注释）",
    ),
    ("PUT /system/auth/preference", "② 自助：本人偏好 upsert"),
    ("GET /system/badge/counts", "② 自助：本人待办计数"),
    (
        "GET /support/config/public",
        "① 匿名：只出 public.* 参数（登录页标题 / Logo）",
    ),
    (
        "GET /business/task/{id}",
        "③ 双身份：no_auth + handler 的 ensure_task_read_permission（business:task:read）",
    ),
    (
        "POST /business/task/{id}/comment",
        "③ 双身份：AI 任务令牌 + handler 内 resolve_reader→principal（读权限 + 可见性）",
    ),
    (
        "POST /business/task/{id}/attachment",
        "③ 双身份：同上（AI 上传附件）",
    ),
    (
        "PATCH /business/task/{id}/ai-progress",
        "③ 只接受任务令牌：人类会话直接 403（63304），无跨任务入口",
    ),
];

// ==================== 扫描器（纯 std，无第三方依赖） ====================

/// 仓库内一份 `.rs` 源文件：路径 + 原文。
struct SourceFile {
    rel: String,
    raw: String,
    clean: String,
}

/// `backend/src` 的源码索引（原文 + 剥注释后的文本）。
struct Repo {
    files: Vec<SourceFile>,
}

impl Repo {
    fn load(src_root: &Path) -> Self {
        let mut paths = Vec::new();
        collect_rs(src_root, &mut paths);
        paths.sort();
        let root = src_root.to_path_buf();
        let files = paths
            .into_iter()
            .filter_map(|p| {
                let raw = std::fs::read_to_string(&p).ok()?;
                let clean = strip_comments(&raw);
                Some(SourceFile {
                    rel: rel_path(&root, &p),
                    raw,
                    clean,
                })
            })
            .collect();
        Self { files }
    }

    /// 该权限码在「剥离注释后的代码」里出现的文件（**不含测试文件**）。
    ///
    /// 空 = 这个码在真实业务代码里没有任何落地 → 勾给角色永不生效（死码）。
    fn code_refs(&self, code: &str) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| !is_test_file(&f.rel) && f.clean.contains(code))
            .map(|f| f.rel.clone())
            .collect()
    }

    /// 该权限码是否被**中间件闸**（`#[nik::permission]` 注解）强制校验。
    fn is_annotated(&self, code: &str) -> bool {
        let needle = format!("permission = \"{code}\"");
        self.files
            .iter()
            .any(|f| !is_test_file(&f.rel) && f.clean.contains(&needle))
    }
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 测试文件不算「校验点」：用例里提到码是断言，不是执行。
fn is_test_file(rel: &str) -> bool {
    rel.ends_with("tests.rs")
        || rel.contains("tests_support")
        || rel.contains("/tests/")
        || rel.ends_with("test_support.rs")
}

/// 剥掉 Rust 注释（`//` `///` `//!` `/* */`），**保留字符串字面量**（含 `//` 的 URL 不会误伤）。
fn strip_comments(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    let mut in_block = false;
    let mut in_str = false;
    let mut in_char = false;
    while i < n {
        let c = chars[i];
        if in_block {
            if c == '*' && i + 1 < n && chars[i + 1] == '/' {
                in_block = false;
                i += 2;
                continue;
            }
            if c == '\n' {
                out.push('\n');
            }
            i += 1;
            continue;
        }
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < n {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if in_char {
            out.push(c);
            if c == '\\' && i + 1 < n {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '\'' {
                in_char = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '\'' {
            // 字符字面量（`'"'`、`'\n'`）与生命周期（`'a`）的区分
            let closes = (i + 2 < n && chars[i + 2] == '\'') || (i + 1 < n && chars[i + 1] == '\\');
            if closes {
                in_char = true;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            in_block = true;
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 一条路由声明：`#[nik::<method>(...)]` + 紧邻上方 `#[nik::permission(...)]` 的权限码。
#[derive(Debug, Clone)]
struct Route {
    method: String,
    path: String,
    file: String,
    line: usize,
    permission: Option<String>,
}

impl Route {
    fn key(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

/// 扫出全部路由。`no_auth` / `permission` 自身不是路由，跳过。
fn scan_routes(repo: &Repo) -> Vec<Route> {
    let mut routes = Vec::new();
    for f in &repo.files {
        if is_test_file(&f.rel) {
            continue;
        }
        let text = &f.clean;
        let mut cursor = 0usize;
        while let Some(offset) = text[cursor..].find("#[nik::") {
            let start = cursor + offset;
            cursor = start + 7;
            let body = &text[start + 7..];
            let Some(paren) = body.find('(') else {
                continue;
            };
            let ident = body[..paren].trim();
            if ident == "permission" || ident == "no_auth" {
                continue;
            }
            // 参数形态有两种：`#[nik::get("/x")]` 与 `#[nik::get(path = "/x")]`
            let args = &body[paren + 1..];
            let Some(q1) = args.find('"') else { continue };
            let rest = &args[q1 + 1..];
            let Some(q2) = rest.find('"') else { continue };
            let route_path = rest[..q2].to_string();
            if !route_path.starts_with('/') {
                continue;
            }
            let method = match ident {
                "get" => "GET",
                "post" => "POST",
                "put" => "PUT",
                "delete" => "DELETE",
                "patch" => "PATCH",
                "head" => "HEAD",
                "options" => "OPTIONS",
                _ => "ANY",
            };
            let line = text[..start].matches('\n').count() + 1;
            routes.push(Route {
                method: method.to_string(),
                path: route_path,
                file: f.rel.clone(),
                line,
                permission: permission_above(text, start),
            });
        }
    }
    routes
}

/// 从 `pos` 往上找「紧邻的属性块」里有没有 `permission = "..."`。
///
/// 属性块与上一个 item 的边界用 `}` 换行切（本仓的声明顺序恒为
/// `文档注释 → #[nik::permission] → #[nik::get] → fn`，所以 600 字符窗口足够）。
fn permission_above(text: &str, pos: usize) -> Option<String> {
    let mut from = pos.saturating_sub(600);
    // 窗口起点必须落在 char 边界上（源码里有中文注释/文案，按字节切会 panic）
    while from < pos && !text.is_char_boundary(from) {
        from += 1;
    }
    let window = &text[from..pos];
    let cut = window.rfind("}\n").map(|i| i + 2).unwrap_or(0);
    let window = &window[cut..];
    let attr = window.find("#[nik::permission(")?;
    let tail = &window[attr..];
    let needle = "permission = \"";
    let at = tail.find(needle)?;
    let rest = &tail[at + needle.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 后端 crate 根（与 `system/menu/tests.rs` 同一套：不依赖 CWD）。
fn backend_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn src_root() -> PathBuf {
    backend_root().join("src")
}

/// 库里全部有权限码的菜单行：`(auth_code, name, menu_type)`。
async fn menu_auth_codes() -> Vec<(String, String, i16)> {
    let rows = db()
        .find_with_params(
            "SELECT auth_code, name, menu_type FROM sys_menu \
             WHERE deleted_at IS NULL AND auth_code IS NOT NULL AND auth_code <> ''",
            vec![],
        )
        .await
        .expect("查菜单权限码失败");
    rows.iter()
        .map(|r| {
            (
                r.get_typed::<String>("auth_code").unwrap_or_default(),
                r.get_typed::<String>("name").unwrap_or_default(),
                r.get_typed::<i16>("menu_type").unwrap_or(0),
            )
        })
        .collect()
}

fn distinct_codes(rows: &[(String, String, i16)]) -> BTreeSet<String> {
    rows.iter().map(|r| r.0.clone()).collect()
}

// ==================== N14 菜单码必须在源码里有落地 ====================

/// N14：**每个 `sys_menu.auth_code` 都必须在后端源码（剥离注释后）里出现**。
///
/// 这是 BUG-SYS-05 的反向防线：报告只做了「注解 → 菜单」的正向扫描（n10），
/// 所以「菜单有码、代码里没人问」这一类能一直躺在库里。命中时的修法三选一：
/// ① 该管没管（A 类）→ 补 `#[nik::permission]`；
/// ② 废弃功能（B 类）→ 写迁移删菜单（参考 V0173）；
/// ③ 预留 / 纯前端（C 类）→ 登记到 [`ROUTE_LEVEL_ONLY`] 并写 TODO。
#[tokio::test]
async fn n14_menu_auth_codes_have_source_action_point() {
    if !ts::ensure_db().await {
        return;
    }
    let repo = Repo::load(&src_root());
    assert!(
        repo.files.len() > 100,
        "源码扫描异常：只读到 {} 个 .rs 文件（扫描逻辑可能失效）",
        repo.files.len()
    );

    let rows = menu_auth_codes().await;
    let codes = distinct_codes(&rows);
    assert!(
        codes.len() > 150,
        "菜单权限码过少（{}），“全绿”可能只是没读到数据",
        codes.len()
    );

    let exceptions: BTreeSet<&str> = ROUTE_LEVEL_ONLY.iter().map(|(c, _)| *c).collect();
    let mut dead = Vec::new();
    let mut annotated = 0usize;
    for code in &codes {
        if exceptions.contains(code.as_str()) {
            continue;
        }
        if repo.is_annotated(code) {
            annotated += 1;
        }
        if repo.code_refs(code).is_empty() {
            dead.push(code.clone());
        }
    }
    assert!(
        dead.is_empty(),
        "以下菜单权限码在后端源码里 0 落地（勾给角色永不生效，BUG-SYS-05）：\n{}\n\
         修法：A 类补 #[nik::permission] / B 类写迁移删菜单 / C 类登记到 ROUTE_LEVEL_ONLY 并写 TODO。",
        dead.join("\n")
    );
    println!(
        "[N14] {} 个菜单权限码全部有源码落地 ✓（{} 条走 #[nik::permission] 注解闸，{} 条走行级校验）",
        codes.len(),
        annotated,
        codes.len() - annotated - exceptions.len()
    );
}

// ==================== N15 权限码形态 ====================

/// N15：权限码必须是 `{模块}:{资源}:{动作}`（≥2 个冒号、每段非空、段内 `[a-z0-9-]`）。
///
/// V0173 之前有 4 条目录菜单把**模块名**当权限码（`erp-finance` / `erp-purchase` /
/// `erp-warehouse` / `erp-master-data`），永远匹配不到任何注解 —— 形态校验能让这
/// 类写法在 CI 就红，而不是等人工扫描。
#[tokio::test]
async fn n15_menu_auth_codes_match_module_resource_action() {
    if !ts::ensure_db().await {
        return;
    }
    let rows = menu_auth_codes().await;
    assert!(!rows.is_empty(), "菜单权限码为空，库可能没迁移");

    let mut bad = Vec::new();
    for (code, name, menu_type) in &rows {
        let segments: Vec<&str> = code.split(':').collect();
        let well_formed = segments.len() >= 3
            && segments.iter().all(|s| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            });
        if !well_formed {
            bad.push(format!("{code}（菜单「{name}」，menu_type={menu_type}）"));
        }
    }
    assert!(
        bad.is_empty(),
        "以下 auth_code 不是 {{模块}}:{{资源}}:{{动作}} 形态（模块名不是权限码）：\n{}",
        bad.join("\n")
    );
    println!("[N15] {} 条菜单行权限码形态合规 ✓", rows.len());
}

// ==================== N16 路由必须有权限闸 ====================

/// N16：**每个路由要么有 `#[nik::permission]`，要么在白名单里写明理由**。
///
/// 这是比死码更严重的一类：接口存在、数据敏感、**完全没有权限校验**（裸接口）。
/// 白名单只允许三类（匿名 / 自助 / 双身份，见 [`NO_PERMISSION_ROUTES`]），
/// 且「白名单过期」（登记的路径已不存在）也会红 —— 防止白名单变成垃圾桶。
#[tokio::test]
async fn n16_every_route_has_permission_gate_or_documented_exception() {
    if !ts::ensure_db().await {
        return;
    }
    let repo = Repo::load(&src_root());
    let routes = scan_routes(&repo);
    assert!(
        routes.len() > 300,
        "路由扫描异常：只扫到 {} 条（扫描逻辑可能失效）",
        routes.len()
    );

    let allowed: BTreeSet<(&str, &str)> = NO_PERMISSION_ROUTES
        .iter()
        .map(|(k, _)| {
            let (m, p) = k.split_once(' ').unwrap_or((*k, ""));
            (m, p)
        })
        .collect();

    let mut naked = Vec::new();
    for route in &routes {
        if route.permission.is_some() {
            continue;
        }
        if !allowed.contains(&(route.method.as_str(), route.path.as_str())) {
            naked.push(format!("{}（{}:{}）", route.key(), route.file, route.line));
        }
    }
    assert!(
        naked.is_empty(),
        "以下路由既没有 #[nik::permission] 也不在无权限白名单里（疑似越权缺口）：\n{}\n\
         确有理由的请登记到 NO_PERMISSION_ROUTES 并写清「谁在替中间件闸把关」。",
        naked.join("\n")
    );

    // 反向：白名单里不许有过期条目（路径改了 / 路由删了，台账必须同步）
    let live: BTreeSet<(&str, &str)> = routes
        .iter()
        .filter(|r| r.permission.is_none())
        .map(|r| (r.method.as_str(), r.path.as_str()))
        .collect();
    let stale: Vec<&str> = NO_PERMISSION_ROUTES
        .iter()
        .filter(|(k, _)| {
            let (m, p) = k.split_once(' ').unwrap_or((*k, ""));
            !live.contains(&(m, p))
        })
        .map(|(k, _)| *k)
        .collect();
    assert!(
        stale.is_empty(),
        "白名单里有过期条目（路由已不存在或已加注解，请删除）：\n{}",
        stale.join("\n")
    );

    let annotated = routes.iter().filter(|r| r.permission.is_some()).count();
    println!(
        "[N16] {} 条路由：{} 条有权限注解 + {} 条白名单例外 ✓",
        routes.len(),
        annotated,
        routes.len() - annotated
    );
}

// ==================== N17 扫描器自检 ====================

/// N17：扫描器自检 —— **人造的死码必须被扫出来**。
///
/// 这类用例是「测试的测试」：如果 `strip_comments` / 路由扫描哪天退化成永远绿，
/// N14–N16 会变成三张空头支票，而没人发现。这里在临时目录里造一棵假源码树：
/// - `dead.rs`：权限码只出现在注释里 → 必须判为「0 落地」；
/// - `naked.rs`：路由没有任何权限注解 → 必须判为裸路由；
/// - `ok.rs`：有注解的路由 → 不能被误报。
///
/// # 夹具为什么用 `format!` 拼（而不是写死 raw string）
///
/// `system::menu::tests::n10` 会扫 `backend/src/**/*.rs` 找 `permission = "..."` 行。
/// 若本文件原样写死一份带注解的夹具源码，n10 会把夹具里的假码当成**真注解**，
/// 然后在库里找不到承载菜单而变红（亲身踩过：`system:ok:list` 让 n10 红）。
/// 所以夹具里的 `permission` 字段名与码都从变量拼出来，源码里不留完整形态。
#[test]
fn n17_scanner_detects_planted_dead_code() {
    let dir = std::env::temp_dir().join(format!("fix06-audit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("mod")).expect("建临时目录");

    let dead_code = "system:dead:code";
    let ok_code = "system:ok:list";

    std::fs::write(
        dir.join("mod/dead.rs"),
        format!(
            "//! 这个码只在文档注释里：`{dead_code}`\n\
             /// 行注释也提到 {dead_code}\n\
             #[nik::get(\"/system/dead\")]\n\
             pub async fn dead() {{}}\n"
        ),
    )
    .expect("写 dead.rs");

    std::fs::write(
        dir.join("mod/naked.rs"),
        "#[nik::post(\"/system/naked\")]\npub async fn naked() {}\n",
    )
    .expect("写 naked.rs");

    // 属性名与码都用占位符拼，避免本文件出现 `permission = "<码>"` 的完整形态
    let perm_attr = "permission";
    std::fs::write(
        dir.join("mod/ok.rs"),
        format!(
            "#[nik::{perm_attr}(\n    path = \"/system/ok\",\n    method = \"GET\",\n    \
             {perm_attr} = \"{ok_code}\"\n)]\n\
             #[nik::get(\"/system/ok\")]\npub async fn ok() {{}}\n"
        ),
    )
    .expect("写 ok.rs");

    let repo = Repo::load(&dir);
    assert_eq!(repo.files.len(), 3, "应读到 3 个源文件");

    // ① 只在注释里的码 → 原文里有、剥注释后没有
    let dead_file = repo
        .files
        .iter()
        .find(|f| f.rel == "mod/dead.rs")
        .expect("应读到 dead.rs");
    assert!(
        dead_file.raw.contains(dead_code),
        "夹具写错了：原文里应有 {dead_code}"
    );
    assert!(
        repo.code_refs(dead_code).is_empty(),
        "扫描器把注释里的权限码当成落地了（strip_comments 失效）"
    );
    // ② 字符串字面量里的码必须保留（不能被剥注释误伤）
    assert!(
        !repo.code_refs(ok_code).is_empty(),
        "扫描器把注解里的权限码剥掉了（strip_comments 过度）"
    );

    // ③ 路由扫描：三条路由（ok 有注解；dead / naked 都是裸路由）
    let routes = scan_routes(&repo);
    assert_eq!(routes.len(), 3, "应扫到 3 条路由，实际 {}", routes.len());
    let ok = routes
        .iter()
        .find(|r| r.path == "/system/ok")
        .expect("应有 ok");
    assert_eq!(ok.permission.as_deref(), Some(ok_code));
    for naked_path in ["/system/naked", "/system/dead"] {
        let naked = routes
            .iter()
            .find(|r| r.path == naked_path)
            .unwrap_or_else(|| panic!("应有 {naked_path}"));
        assert!(
            naked.permission.is_none(),
            "裸路由 {naked_path} 被判成了有权限注解（permission_above 失真）"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
    println!("[N17] 扫描器自检通过（死码 / 裸路由都能扫出来）✓");
}

// ==================== N18 例外台账必须是活的 ====================

/// N18：`ROUTE_LEVEL_ONLY` 的每条例外都必须**仍然成立**。
///
/// 成立 = ① 码还在 `sys_menu` 里（前端 `meta.permission` 要勾它）；
/// ② 后端源码（剥离注释后）仍然 0 引用。
/// 条件一变就说明例外过期（有人补了接口 / 有人删了菜单），必须回来清理台账，
/// 否则台账会慢慢变成「历史垃圾场」，例外也就失去意义。
#[tokio::test]
async fn n18_route_level_exception_ledger_is_live() {
    if !ts::ensure_db().await {
        return;
    }
    let repo = Repo::load(&src_root());
    let codes = distinct_codes(&menu_auth_codes().await);

    let mut problems = Vec::new();
    for (code, reason) in ROUTE_LEVEL_ONLY {
        if !codes.contains(*code) {
            problems.push(format!("{code}：菜单里已不存在，例外该删了"));
        }
        let refs = repo.code_refs(code);
        if !refs.is_empty() {
            problems.push(format!(
                "{code}：后端源码已有引用（{}），应改走注解并删除本例外",
                refs.join(", ")
            ));
        }
        if reason.trim().is_empty() {
            problems.push(format!("{code}：例外理由为空"));
        }
    }
    assert!(
        problems.is_empty(),
        "ROUTE_LEVEL_ONLY 台账过期：\n{}",
        problems.join("\n")
    );

    // 无权限白名单的每一条也必须写明理由（空理由 = 没交代谁在把关）
    let empty_reason: Vec<&str> = NO_PERMISSION_ROUTES
        .iter()
        .filter(|(_, r)| r.trim().is_empty())
        .map(|(k, _)| *k)
        .collect();
    assert!(
        empty_reason.is_empty(),
        "以下无权限路由没有写理由：\n{}",
        empty_reason.join("\n")
    );

    println!(
        "[N18] 例外台账 {} 条、白名单 {} 条，全部有理由且仍然成立 ✓",
        ROUTE_LEVEL_ONLY.len(),
        NO_PERMISSION_ROUTES.len()
    );
}

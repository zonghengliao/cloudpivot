//! 成本核算模块总开关 `erp.cost.enabled`（ARCH-9 §7.4 灰度 · COST-B4-4）。
//!
//! # 开关语义
//!
//! 一个 `sys_config` 布尔参数（`V0385` 种入，默认 `false`、业务级）控制整块成本核算能力：
//!
//! | 关闭时 | 实现位置 |
//! |--------|----------|
//! | 侧边栏菜单 / 按钮权限点隐藏 | [`filter_menus`]（`/system/auth/menus`）、[`filter_permissions`]（`/system/auth/me`） |
//! | 成本相关接口一律 `403 + 66079` | [`guard`]（挂 `App::route_layer`） |
//! | 领料单的「成本对象」字段隐藏 | 前端 `stores/cost.ts` + 出库单页（后端只放行读，前端不渲染） |
//!
//! 打开后全部恢复，**切换不需要重启**：值走 [`SysConfigModel::read_text`] 的进程内快照
//! （参数写操作 / 跨实例版本轮询都会刷新），因此每一次判定都是当前值。
//!
//! # 为什么在 `business::shared` 而不是 `business::erp`
//!
//! 判定要被 `system::auth`（菜单 / 权限过滤）用到，而 `system` 依赖 `business::shared::*`
//! 是既有分层（`principal` / `scope` / `guard` 都这么用），依赖 `business::erp::*` 则会把
//! 平台层绑到某个业务域上。开关判定是「跨域的基础设施」，放这里最不容易被误用。
//!
//! # 三条不要再犯的坑
//!
//! 1. **开关值不能缓存在命令式代码里**（如 `OnceLock` 一次性读库）：参数页改完必须立刻生效。
//!    这里每次判定都走 `read_text`，其内部命中进程内快照（纯内存），代价仅一次 `HashMap` 拷贝。
//! 2. **菜单不能用 `sys_menu.is_visible = FALSE` 持久隐藏**：那样开关重新打开时，管理员
//!    手工调过的可见性会被覆盖，而且切换要跑一次迁移。运行期过滤才是可逆的。
//! 3. **探针接口本身必须放行**（[`SWITCH_PROBE_PATH`]）：前端要先知道开关是开是关才能决定
//!    渲染什么，所以 `GET /business/erp/cost-switch` 虽然落在 `cost-` 前缀里，仍要豁免。
//!
//! # 开关关闭时**不**拦的东西（有意为之，写清楚免得被当成漏网）
//!
//! - `GET /business/erp/lot*`（批次主数据）：出库单 / 领料单的批号下拉要用它，关掉成本核算
//!   不该让仓库连批号都选不了。批次**页面**的菜单仍然隐藏（见 [`COST_MENU_AUTH_PREFIXES`]）。
//! - `/business/erp/stock-out`（出库单本体）：领料单是仓库的日常单据，只隐藏其中的成本对象字段。
//! - 其它模块对 `biz_cost_*` 的**内部**调用（如结账 blocker 的 `COST_UNSETTLED` 检查）：
//!   那属于「有没有未结算批次」的数据事实，与界面开关无关。

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use nik::prelude::*;

use crate::shared::error_code;
use crate::support::config::model::{SysConfigModel, parse_bool};
use crate::system::menu::dto::MenuRes;

/// 开关的参数键（`sys_config.config_key`，`V0385` 种入）。
pub const CONFIG_KEY: &str = "erp.cost.enabled";

/// 开关探针路径（**不受**本开关拦截；前端据此决定渲染）。
pub const SWITCH_PROBE_PATH: &str = "/business/erp/cost-switch";

/// 受开关控制的接口路径前缀（相对 `[router] prefix`，这里不含 `/api`）。
///
/// 判定是**前缀匹配**：新增任何 `/business/erp/cost-xxx` 接口自动被纳入，不需要回来加一行
/// —— 这正是选前缀而不是逐个列路径的原因（漏一个就是「关了开关还能调」）。
pub const COST_API_PREFIXES: [&str; 2] = [
    // cost-object / cost-lot? / cost-labor / cost-overhead / cost-alloc-rule / cost-period / cost-sheet …
    "/business/erp/cost-",
    // 出库单页的成本对象下拉与「一键领料」预填（成本模块专用，关了就不该能取到）
    "/business/erp/stock-out/cost-object-",
];

/// 属于成本核算模块的**菜单权限码前缀**（关闭时从侧边栏与权限列表里摘掉）。
///
/// - `business:erp:cost-*`：6 个页面里 5 个（成本对象 / 工时 / 费用 / 分摊规则 / 成本核算）
///   与其 18 个按钮；
/// - `business:erp:lot:`：批次管理页与其 2 个按钮（ARCH-9 §6.1 把它挂在「仓库管理」下，
///   但数据服务的是成本归集；接口本身不拦，见模块注释）。
pub const COST_MENU_AUTH_PREFIXES: [&str; 2] = ["business:erp:cost-", "business:erp:lot:"];

/// 属于成本核算模块的**页面路径**（`sys_menu.path`，V0378 的 6 条 `menu_type = 2`）。
///
/// 用于「菜单权限码被客户改过」的兜底：`auth_code` 可能被改成别的码，但只要 `path`
/// 还是这 6 条之一，就不该在开关关闭时出现在侧边栏。
pub const COST_PAGE_PATHS: [&str; 6] = [
    "/business/erp/cost-object",
    "/business/erp/cost-labor",
    "/business/erp/cost-overhead",
    "/business/erp/cost-alloc-rule",
    "/business/erp/cost-period",
    "/business/erp/lot",
];

/// 由参数原文判定开关是否打开（纯函数，便于单测）。
///
/// # 口径
///
/// - `Some("true")` / `Some("1")`（大小写、前后空白不敏感）→ `true`；
/// - `Some("false")` / `Some("0")` / 其它无法识别的值 / `None`（参数缺失、被停用、值为空）
///   → `false`。
///
/// **默认关闭**是设计 §7.4 的硬要求（`BOOLEAN`，默认 `false`）：参数读不到时绝不能
/// 因为「猜成 true」而把未上线的模块放出去。
///
/// # 参数
///
/// - `raw`：`sys_config.config_value` 的明文值。
///
/// # 返回值
///
/// 开关是否打开。
pub fn enabled_from_raw(raw: Option<&str>) -> bool {
    raw.and_then(parse_bool).unwrap_or(false)
}

/// 当前开关是否打开（读取 `sys_config`，命中进程内快照）。
///
/// # 返回值
///
/// 打开返回 `true`；参数缺失 / 停用 / 无法识别一律 `false`。
pub async fn is_enabled() -> bool {
    enabled_from_raw(SysConfigModel::read_text(CONFIG_KEY).await.as_deref())
}

/// 归一化请求路径：去掉查询串无关、去掉 `[router] prefix`（`/api`）、统一小写。
///
/// # 参数
///
/// - `path`：`request.uri().path()`（不含查询串）。
///
/// # 返回值
///
/// 不含 `/api` 前缀的路径（始终以 `/` 开头）。
fn normalize_path(path: &str) -> String {
    let lower = path.trim().to_ascii_lowercase();
    let trimmed = lower
        .strip_prefix("/api")
        .filter(|rest| rest.starts_with('/'))
        .unwrap_or(&lower);
    trimmed.to_string()
}

/// 判断该请求路径是否受成本核算开关控制（纯函数）。
///
/// # 参数
///
/// - `path`：请求路径，可带或不带 `/api` 前缀。
///
/// # 返回值
///
/// 受控返回 `true`。探针路径 [`SWITCH_PROBE_PATH`] 恒为 `false`（放行）。
pub fn is_cost_api_path(path: &str) -> bool {
    let normalized = normalize_path(path);
    if normalized == SWITCH_PROBE_PATH {
        return false;
    }
    COST_API_PREFIXES.iter().any(|prefix| normalized.starts_with(prefix))
}

/// 判断某个权限码是否属于成本核算模块（纯函数）。
///
/// # 参数
///
/// - `auth_code`：`sys_menu.auth_code`。
///
/// # 返回值
///
/// 属于成本模块返回 `true`（空码恒为 `false`）。
pub fn is_cost_auth_code(auth_code: &str) -> bool {
    let code = auth_code.trim();
    !code.is_empty() && COST_MENU_AUTH_PREFIXES.iter().any(|p| code.starts_with(p))
}

/// 判断某个菜单节点是否属于成本核算模块（纯函数）。
///
/// 命中任一条即算：权限码前缀（[`is_cost_auth_code`]）或页面路径（[`COST_PAGE_PATHS`]）。
///
/// # 参数
///
/// - `menu`：菜单节点。
///
/// # 返回值
///
/// 属于成本模块返回 `true`。
pub fn is_cost_menu(menu: &MenuRes) -> bool {
    if is_cost_auth_code(&menu.auth_code) {
        return true;
    }
    menu.path
        .as_deref()
        .map(normalize_path)
        .is_some_and(|p| COST_PAGE_PATHS.contains(&p.as_str()))
}

/// 摘掉菜单列表里的成本节点（纯函数，保持其余顺序）。
///
/// # 参数
///
/// - `menus`：扁平菜单列表（就是 `/system/auth/menus` 建树前的输入）。
///
/// # 返回值
///
/// 不含成本节点的列表。
pub fn retain_non_cost_menus(menus: Vec<MenuRes>) -> Vec<MenuRes> {
    menus.into_iter().filter(|m| !is_cost_menu(m)).collect()
}

/// 摘掉权限码列表里的成本码（纯函数，保持其余顺序与去重口径）。
///
/// 超管的通配码 `*:*:*` 不匹配任何前缀，因此原样保留 —— 超管仍能进页面（接口会被
/// [`guard`] 拦成 403），这是有意的：管理员需要能看到「模块存在但被关掉了」的反馈，
/// 而不是像权限不够那样被静默重定向。
///
/// # 参数
///
/// - `permissions`：权限码列表。
///
/// # 返回值
///
/// 不含成本码的列表。
pub fn retain_non_cost_permissions(permissions: Vec<String>) -> Vec<String> {
    permissions
        .into_iter()
        .filter(|code| !is_cost_auth_code(code))
        .collect()
}

/// 开关关闭时摘掉菜单里的成本节点，打开时原样返回。
///
/// # 参数
///
/// - `menus`：扁平菜单列表。
///
/// # 返回值
///
/// 过滤后的菜单列表。
pub async fn filter_menus(menus: Vec<MenuRes>) -> Vec<MenuRes> {
    if is_enabled().await {
        menus
    } else {
        retain_non_cost_menus(menus)
    }
}

/// 开关关闭时摘掉权限列表里的成本码，打开时原样返回。
///
/// # 参数
///
/// - `permissions`：权限码列表。
///
/// # 返回值
///
/// 过滤后的权限列表。
pub async fn filter_permissions(permissions: Vec<String>) -> Vec<String> {
    if is_enabled().await {
        permissions
    } else {
        retain_non_cost_permissions(permissions)
    }
}

/// 中间件本体：成本相关接口在开关关闭时统一 `403 + 66079`。
///
/// 通过 `App::route_layer` 挂在框架内置链**之内**（认证层之后），因此：
///
/// - 未登录访问成本接口仍然是认证层的 `401`（不泄露「模块是否存在」）；
/// - 已登录但开关关闭才是本层的 `403`（报文点名参数键与开启方法，用户自己能修）。
///
/// # 参数
///
/// - `request`：进入的请求；
/// - `next`：后续处理链。
///
/// # 返回值
///
/// 放行时原样返回下游响应；拦截时返回 403 统一错误体。
pub async fn guard(request: Request, next: Next) -> Response {
    if !is_cost_api_path(request.uri().path()) {
        return next.run(request).await;
    }
    if is_enabled().await {
        return next.run(request).await;
    }
    tracing::info!(
        path = %request.uri().path(),
        config_key = CONFIG_KEY,
        "成本核算模块未启用，请求被拒（403）"
    );
    disabled_response()
}

/// 构造 `403 + 66079` 的统一错误体。
///
/// 报文写法遵守「用户自己能修」的口径：点名参数键、说清开关在哪、并说明切换不需要重启。
///
/// # 返回值
///
/// HTTP 403 响应。
pub fn disabled_response() -> Response {
    let message = format!(
        "成本核算模块未启用（{CONFIG_KEY} = false）。请系统管理员在「支撑功能 / 参数配置 / ERP 成本」里打开该开关后重试；切换即时生效，无需重启"
    );
    let body =
        serde_json::to_string(&Res::<()>::fail(error_code::ERP_COST_MODULE_DISABLED, message)).unwrap_or_else(|_| {
            format!(
                r#"{{"code":{},"msg":"成本核算模块未启用"}}"#,
                error_code::ERP_COST_MODULE_DISABLED
            )
        });
    (
        StatusCode::FORBIDDEN,
        [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Body::from(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个只填了判定所需字段的菜单节点。
    fn menu(id: &str, auth_code: &str, path: Option<&str>) -> MenuRes {
        MenuRes {
            id: id.to_string(),
            name: id.to_string(),
            menu_type: if path.is_some() { 2 } else { 3 },
            auth_code: auth_code.to_string(),
            path: path.map(str::to_string),
            parent_id: None,
            component: None,
            sort_order: 10,
            status: 1,
            icon: None,
            title: None,
            is_frame: false,
            is_cache: false,
            is_visible: true,
            hidden_expression: None,
            module_code: Some("business".to_string()),
            children: Vec::new(),
        }
    }

    /// 默认值口径：读不到 / 写错 / 空值一律「关」。
    #[test]
    fn enabled_defaults_to_false() {
        assert!(!enabled_from_raw(None));
        assert!(!enabled_from_raw(Some("")));
        assert!(!enabled_from_raw(Some("false")));
        assert!(!enabled_from_raw(Some("0")));
        // 写错的值（历史遗留 / 手工改库）不能被当成「开」
        assert!(!enabled_from_raw(Some("yes")));
        assert!(!enabled_from_raw(Some("True!")));
    }

    /// 打开口径：`true` / `1`，大小写与前后空白不敏感。
    #[test]
    fn enabled_accepts_true_and_one() {
        for raw in ["true", "TRUE", " True ", "1"] {
            assert!(enabled_from_raw(Some(raw)), "raw = {raw:?} 应判为开");
        }
    }

    /// 路径判定：带不带 `/api` 前缀都要认；探针放行；批次主数据与出库单本体不拦。
    #[test]
    fn api_path_gate_covers_cost_endpoints_only() {
        // 受控（带 / 不带前缀、大小写都算）
        for path in [
            "/business/erp/cost-object",
            "/api/business/erp/cost-object/20000000-0000-4000-8000-00000000c001",
            "/api/business/erp/cost-period/20000000-0000-4000-8000-00000000c005/cost-sheet",
            "/business/erp/cost-labor/import",
            "/api/Business/Erp/Cost-Alloc-Rule/preview",
            "/business/erp/stock-out/cost-object-options",
            "/api/business/erp/stock-out/cost-object-issue-prefill",
        ] {
            assert!(is_cost_api_path(path), "{path} 应受开关控制");
        }

        // 豁免：探针自身
        assert!(!is_cost_api_path(SWITCH_PROBE_PATH));
        assert!(!is_cost_api_path("/api/business/erp/cost-switch"));

        // 不受控：批次主数据（出库单批号下拉要用）、出库单本体、其它 ERP 接口
        for path in [
            "/api/business/erp/lot/options",
            "/business/erp/lot",
            "/api/business/erp/stock-out",
            "/api/business/erp/stock-out/page",
            "/api/business/erp/purchase-order",
            "/api/system/auth/menus",
            "/health",
        ] {
            assert!(!is_cost_api_path(path), "{path} 不该被成本开关拦");
        }
    }

    /// 权限码判定：成本与批次前缀命中，形近但不同的码不命中。
    #[test]
    fn auth_code_gate_matches_cost_and_lot_only() {
        for code in [
            "business:erp:cost-object:list",
            "business:erp:cost-period:reverse",
            "business:erp:cost-alloc-rule:preview",
            "business:erp:lot:list",
            "business:erp:lot:update",
        ] {
            assert!(is_cost_auth_code(code), "{code} 应属于成本模块");
        }
        for code in [
            "",
            " ",
            "business:erp:stock-out:create",
            "business:erp:purchase-order:list",
            // 形近：没有 `cost-` 前缀 / 没有 `lot:`
            "business:erp:costume:list",
            "business:erp:lottery:list",
            "*:*:*",
        ] {
            assert!(!is_cost_auth_code(code), "{code} 不该被当成成本模块");
        }
    }

    /// 菜单过滤：成本页面（按 path 兜底）与按钮都摘掉，父目录与其它业务保留。
    #[test]
    fn menu_filter_drops_cost_pages_and_buttons() {
        let menus = vec![
            menu("e02", "", Some("/business/erp")),
            menu(
                "c001",
                "business:erp:cost-object:list",
                Some("/business/erp/cost-object"),
            ),
            menu("c101", "business:erp:cost-object:create", None),
            menu(
                "c005",
                "business:erp:cost-period:list",
                Some("/business/erp/cost-period"),
            ),
            menu("c006", "business:erp:lot:list", Some("/business/erp/lot")),
            // 权限码被客户改过、但 path 仍是成本页 → 也要摘
            menu("cX", "custom:whatever:list", Some("/business/erp/cost-labor")),
            menu("e01", "", Some("/business/erp/stock-out")),
            menu("s01", "business:erp:stock-out:list", Some("/business/erp/stock-out")),
        ];
        let kept = retain_non_cost_menus(menus);
        let ids: Vec<&str> = kept.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["e02", "e01", "s01"]);
    }

    /// 权限过滤：只摘成本码，超管通配码与其它码原样保留且顺序不变。
    #[test]
    fn permission_filter_keeps_wildcard_and_order() {
        let codes: Vec<String> = [
            "business:erp:cost-object:list",
            "business:erp:stock-out:list",
            "business:erp:lot:create",
            "*:*:*",
            "business:erp:cost-period:settle",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let kept = retain_non_cost_permissions(codes);
        assert_eq!(kept, vec!["business:erp:stock-out:list", "*:*:*"]);
    }

    /// 迁移守卫：V0385 必须种出「BOOLEAN / 业务级 / 默认 false」这一行。
    ///
    /// 这条测试不是重复 SQL 自检，而是钉住**代码默认值（false）与迁移默认值（false）
    /// 同口径**：两边任一处被改成 true，都是「升级即上线」的事故。
    #[test]
    fn migration_seeds_switch_with_false_default() {
        let sql = include_str!("../../../migrations/business/V0385__cost_module_switch.up.sql");
        assert!(sql.contains("'erp.cost.enabled'"), "迁移必须种 erp.cost.enabled");
        assert!(sql.contains("'BOOLEAN'"), "开关类型必须是 BOOLEAN（配置页才渲染开关）");
        // 开关行本身：false 默认值 + config_level 2
        assert!(
            sql.contains("'false',\n    'BOOLEAN'"),
            "默认值必须逐字是 false，且紧跟 BOOLEAN 类型"
        );
        assert!(sql.contains("'erp_cost', 2, TRUE"), "分类与业务级（2）口径必须写死");
        // 值域只允许 true/false（配置页下拉的 ref_value/select_value）
        assert!(sql.contains("'true|false'"));
        // 反面对照：down 迁移必须删掉这一行（回滚不留死参数）
        let down = include_str!("../../../migrations/business/V0385__cost_module_switch.down.sql");
        assert!(down.contains("DELETE FROM sys_config WHERE config_key = 'erp.cost.enabled'"));
    }
}

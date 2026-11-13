//! Nik web layer: 路由、中间件、提取器、响应、全局资源

/// 统一出口：响应体、API 错误、分页参数、错误码，以及「下层错误 → HTTP 语义」的映射
pub mod api;
pub mod app;
#[cfg(feature = "db")]
pub mod db_url;
pub mod extract;
pub mod jwt;
/// F10 可观测性：HTTP 自动埋点 +（可选）Prometheus 导出端点
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod middleware;
#[cfg(feature = "db")]
pub mod page;
pub mod plugin;

// Web 插件（F5）：插件作为单一扩展单元，贡献生命周期 + 路由 + 权限 + 免鉴权
pub use crate::plugin::WebPlugin;
pub mod resources;
pub mod response;
pub mod router;
#[cfg(feature = "sse")]
pub mod sse;

pub use api::{
    ApiError, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, PageData, PageQuery, Res, ResResult, SortOrder,
    SortQuery, codes,
};
pub use app::App;
pub use extract::ValidateExt;
pub use extract::ValidatedJson;
pub use extract::{
    validate_date, validate_date_or_datetime, validate_datetime, validate_email, validate_id_card,
    validate_mobile, validate_password_strength, validate_url, validate_uuid,
};
pub use jwt::JWT;
pub use middleware::{
    AuthClaims, AuthLayer, ClaimsBuilder, Principal, RequirePermissionLayer, RequireRoleLayer,
    TokenClaims, TokenType, UserId,
};
#[allow(deprecated)]
pub use response::ApiBody;
pub use router::NikRouter;

#[cfg(feature = "db")]
pub use crate::page::PageResponseExt;
#[cfg(all(feature = "cache", feature = "db"))]
pub use crate::resources::cache_tenant;
#[cfg(feature = "cache")]
pub use crate::resources::{cache, set_cache, try_cache};
pub use crate::resources::{cfg, config, set_config, try_config};
#[cfg(feature = "db")]
pub use crate::resources::{db, set_db, try_db};
#[cfg(feature = "db")]
pub use crate::resources::{db_named, set_db_named, try_db_named};
pub use crate::resources::{
    download_path, set_download_path, set_upload_path, try_download_path, try_upload_path,
    upload_path,
};
#[cfg(feature = "template")]
pub use crate::resources::{render_template, set_template, try_template};

/// 路由注册项 —— `#[nik::get]` / `#[nik::post]` … 宏在**编译期**提交，
/// [`App::scan`](crate::App::scan) 在装配阶段收集。
///
/// # 为什么用 `inventory` 而不是 `linkme`
///
/// `linkme::distributed_slice` 依赖链接器把自定义段完整保留。在
/// `x86_64-pc-windows-gnu`（mingw `ld` 默认 `--gc-sections`）下，未被常规符号
/// 引用的段会被**整段回收**，于是切片长度为 0：所有路由消失（请求全部 404）、
/// 权限层不挂载，且**编译与链接均无报错**（F19）。
///
/// `inventory` 不依赖自定义段，任何工具链都成立，因此**无需**
/// `.cargo/config.toml` 里的 `-Wl,--no-gc-sections` 全局兜底。
#[derive(Debug, Clone, Copy)]
pub struct RouteEntry(pub fn(&mut NikRouter));

inventory::collect!(RouteEntry);

/// 路径权限定义 —— 由 `#[permission("xxx")]` 宏注解生成
///
/// 每条记录描述一个接口路径所需的权限标识。
/// 运行时通过 `PermissionCheckLayer` 中间件校验。
#[derive(Debug, Clone)]
pub struct PermissionDef {
    /// URL 路径
    pub path: &'static str,
    /// HTTP 方法
    pub method: &'static str,
    /// 所需权限标识
    pub permission: &'static str,
}

inventory::collect!(PermissionDef);

/// 「**登录即可、不要权限码**」规则哨兵（`#[nik::auth_only]` 与配置规则共用）。
///
/// # 为什么需要它（cloudpivot-patch code-12）
///
/// 权限规则表只认识「需要某个码」的路由；**故意不要码**的路由（自助 `/my`、界面策略
/// 开关这类）在表里没有一行。而 `PermissionCheckLayer` 的选择算法是
/// 「完全相等(0) > 纯静态(1) > 含参数(2)」，于是兄弟模板规则会**静默吞掉**静态子路径：
///
/// ```text
/// 请求 GET /business/hr/leave/my        ← 只想要「登录即可」
/// 规则池：GET /business/hr/leave/{id}   permission = business:hr:leave:detail  (score 2)
///        （/leave/my 自己没有规则）
/// ⇒ 命中 score 2 的模板规则 → 唯一的合法调用方（没有 leave:detail 的普通员工）被判 403
/// ```
///
/// 实测在 2026-11-13 的 HR-47 权限矩阵里就是这样：`/salary/my`、`/attendance/my` 正常
/// （前者有自己的码，后者没有同形模板规则），只有 `/leave/my` 全员 403。
///
/// 修法：让「登录即可」成为一种**显式**规则 —— 路由自己挂一条 score 0 的精确规则，
/// 语义是「认证层已放过（401 由更外层保证），权限层不再要求任何码」。
/// 这样模板规则再也不会串味，且「忘了写码」仍然是**默认拒绝**（空串不等于本哨兵，
/// 见 [`PermissionRule`] 的判定），不会把漏写注解的路由悄悄放行。
pub const AUTH_ONLY: &str = "@auth";

/// 无需认证的路径定义 —— 由 `#[no_auth]` 宏注解生成
///
/// 在路径列表中指定的接口将绕过 AuthLayer 中间件。
#[derive(Debug, Clone)]
pub struct NoAuthDef {
    /// URL 路径
    pub path: &'static str,
}

inventory::collect!(NoAuthDef);

/// 遍历编译期注册的全部路由注册项（由 `#[nik::get]` 等宏提交）。
pub fn discovered_routes() -> impl Iterator<Item = fn(&mut NikRouter)> {
    inventory::iter::<RouteEntry>
        .into_iter()
        .map(|entry| entry.0)
}

/// 收集编译期注册的全部权限规则（由 `#[nik::permission]` 宏提交）。
pub fn discovered_permissions() -> Vec<PermissionDef> {
    inventory::iter::<PermissionDef>
        .into_iter()
        .cloned()
        .collect()
}

/// 收集编译期注册的全部免认证路径（由 `#[nik::no_auth]` 宏提交）。
pub fn discovered_no_auth() -> Vec<NoAuthDef> {
    inventory::iter::<NoAuthDef>.into_iter().cloned().collect()
}

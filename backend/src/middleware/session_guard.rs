//! 会话守卫中间件：令牌版本比对 + 首登强制改密。
//!
//! # 挂载位置（关键）
//!
//! 通过 [`alun::prelude::App::route_layer`] 注册，位于**框架内置中间件链之内**：
//! 认证层已经验签并注入 [`AuthClaims`]，因此本层直接读取声明，不必重复验签；
//! 同时又能拦下「已通过认证但会话已作废」的请求。
//!
//! > 若改用 `App::layer_from_fn`（链外），请求期读不到 `AuthClaims`（该层在认证层
//! > 之外），只能自行再验一次签名，且拿不到认证层的 401 语义。
//!
//! # 两条判定
//!
//! 1. **令牌版本**（安全审计 CODE-05 / CODE-08）：令牌 claim `ver` 必须等于
//!    `sys_user.token_version`。改密 / 改角色 / 停用 / 删除会使版本号自增，
//!    该用户已签发（含已登出的 refresh 之外的一切）令牌立即失效 → 401；
//! 2. **首登强制改密**（CODE-06）：`sys_user.must_change_password = TRUE` 时，
//!    仅放行 [`constants::MUST_CHANGE_PASSWORD_ALLOWED_PATHS`]（改密、登出），
//!    其余接口一律 403，使初始口令无法用于任何业务操作。
//!
//! # 为什么每次请求读库
//!
//! 会话字段用量是「一次主键点查」，换来的是：
//!
//! - 多实例部署下无需广播失效；
//! - 不存在「缓存未失效 → 旧令牌仍可用」的时间窗。
//!
//! 安全性优先于这一次索引查询的成本；若后续压测证明该点查成为瓶颈，
//! 再接入短 TTL 缓存 + 变更时失效（与 `system::auth::model` 的主体缓存同一套做法）。

use alun::prelude::*;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::shared::constants;
use crate::system::user::model;

/// 会话守卫本体（用 `axum::middleware::from_fn` 包一层后交给 `App::route_layer`）。
///
/// # 参数
///
/// - `request`：进入的请求（认证层的 `AuthClaims` 已在 extensions 中）。
/// - `next`：后续处理链。
///
/// # 返回值
///
/// 放行时原样返回下游响应；会话已作废返回 401，首登未改密返回 403。
pub async fn guard(request: Request, next: Next) -> Response {
    // CORS 预检不携带凭据，认证层亦已放行，这里同样不拦
    if request.method() == axum::http::Method::OPTIONS {
        return next.run(request).await;
    }

    // 未注入声明：匿名接口（登录 / 刷新 / 公共参数）或无令牌请求，交给认证层处理
    let Some(AuthClaims(claims)) = request.extensions().get::<AuthClaims>().cloned() else {
        return next.run(request).await;
    };

    let Some(user_id) = parse_uuid(&claims.sub) else {
        return unauthorized("令牌主体不合法，请重新登录");
    };

    // 读会话字段：用户已删除 / 不存在 → 401
    let flags = match model::load_session_flags(&user_id).await {
        Ok(flags) => flags,
        Err(e) => {
            // fail-closed：读不到会话状态时宁可让用户重登，也不放行
            tracing::error!(error = %e, user_id = %user_id, "会话状态查询失败，拒绝请求");
            return server_error("会话状态查询失败");
        }
    };
    let Some((token_version, must_change_password)) = flags else {
        return unauthorized("账号不存在或已被删除，请重新登录");
    };

    // 1) 令牌版本比对（缺省按 0 处理：整改上线前签发的旧令牌在版本号仍为 0 时平滑过渡）
    let ver = claims
        .extra_i64(constants::CLAIM_TOKEN_VERSION)
        .unwrap_or(0);
    if ver != i64::from(token_version) {
        tracing::info!(
            user_id = %user_id,
            token_version,
            token_ver = ver,
            "令牌版本已过期，拒绝请求"
        );
        return unauthorized("登录状态已失效，请重新登录");
    }

    // 2) 首登强制改密：仅放行改密 / 登出
    if must_change_password && !is_allowed_while_must_change(request.uri().path()) {
        tracing::info!(
            user_id = %user_id,
            path = %request.uri().path(),
            "首登未改密，拒绝访问"
        );
        return forbidden("首次登录必须先修改初始密码");
    }

    next.run(request).await
}

/// 首登未改密时是否允许访问该路径（含全局路由前缀）。
///
/// # 参数
///
/// - `path`：请求路径（含 `[router] prefix`，如 `/api/system/auth/password`）。
///
/// # 返回值
///
/// 命中白名单返回 `true`。
fn is_allowed_while_must_change(path: &str) -> bool {
    let prefix = cfg().router.prefix.trim_end_matches('/').to_string();
    constants::MUST_CHANGE_PASSWORD_ALLOWED_PATHS
        .iter()
        .any(|allowed| {
            let full = format!("{prefix}{allowed}");
            // 允许精确命中，也允许其子路径（如未来的 /system/auth/password/xxx）
            path == full || path.starts_with(&format!("{full}/"))
        })
}

/// 解析 `claims.sub` 为 UUID（主体非 UUID 视为会话无效）。
fn parse_uuid(raw: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(raw.trim()).ok()
}

/// 构造 401 响应（与框架认证层同样式，客户端可统一处理）。
fn unauthorized(msg: &str) -> Response {
    build(
        StatusCode::UNAUTHORIZED,
        serde_json::to_string(&Res::<()>::fail(401, msg))
            .unwrap_or_else(|_| r#"{"code":401,"msg":"请重新登录"}"#.to_string()),
    )
}

/// 构造 403 响应。
fn forbidden(msg: &str) -> Response {
    build(
        StatusCode::FORBIDDEN,
        serde_json::to_string(&Res::<()>::fail(403, msg))
            .unwrap_or_else(|_| r#"{"code":403,"msg":"无权限访问"}"#.to_string()),
    )
}

/// 构造 500 响应（会话状态读不到时 fail-closed）。
fn server_error(msg: &str) -> Response {
    build(
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::to_string(&Res::<()>::fail(500, msg))
            .unwrap_or_else(|_| r#"{"code":500,"msg":"服务器内部错误"}"#.to_string()),
    )
}

/// 把 JSON 文本包装成响应。
fn build(status: StatusCode, body: String) -> Response {
    (
        status,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        Body::from(body),
    )
        .into_response()
}

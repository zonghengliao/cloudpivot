//! 吊销复检中间件（fail-closed）。
//!
//! # 为什么需要它
//!
//! 框架认证层（`alun-web` 0.4.2 `middleware/auth.rs`）的黑名单检查是 **fail-open** 的：
//!
//! ```ignore
//! if let Ok(Some(_)) = Cache::get::<Value>(c, &key).await { /* 401 */ }   // Err(_) 直接放行
//! ```
//!
//! 也就是说，缓存读失败时「已登出 / 已被强制下线的令牌」照样能通过。缓存不可用
//! 期间吊销名单等于不存在（评审 §1.3）。而 `alun-web` 是 crates.io 依赖、仓库里
//! 没有副本，改不了它——只能在项目侧补一层复检（评审 §3）。
//!
//! # 挂载位置（关键）
//!
//! 通过 [`alun::prelude::App::route_layer`] 注册，位于**框架内置中间件链之内**
//! （`app.rs` 先应用 `route_layers`、再 `build_middleware_chain`），因此：
//!
//! - 认证层已经验签并注入 [`AuthClaims`]，本层直接读声明、不必重复验签；
//! - 权限层（`permission`）在本层**外层**，所以本层只做「复检 / 拒绝」，
//!   不注入身份、也不做权限判断。
//!
//! # 三条判定
//!
//! | 情形 | 动作 |
//! |------|------|
//! | extensions 里没有 `AuthClaims` | 放行（匿名 / no_auth 路径，由认证层与处理器各自负责） |
//! | 黑名单命中 | 401 |
//! | 查询失败 / 缓存缺失 / 令牌无 `jti` | **401**（fail-closed）+ `error` 日志 |
//!
//! 认证路径本来就必须有缓存（吊销状态就在缓存里），缺了就是配置错误，
//! 此时放行等于「吊销静默失效」，因此按拒绝处理。

use alun::prelude::*;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::shared::constants;

/// 复检令牌 `jti` 是否已被吊销（黑名单命中）。
///
/// # 参数
///
/// - `jti`：令牌唯一标识；`None` 表示令牌没有 `jti`（无法复检）。
///
/// # 返回值
///
/// - `Ok(true)`：已被吊销（登出 / 强制下线 / 轮换作废）；
/// - `Ok(false)`：未被吊销；
/// - `Err(原因)`：**无法判定**（缓存未初始化 / 查询失败 / 无 `jti`），
///   调用方必须按「已吊销」处理。
///
/// # 说明
///
/// 刷新路径（`/system/auth/refresh`）在框架 `ignore_paths` 里，框架中间件会跳过
/// 黑名单检查，因此它直接用本函数做 fail-closed 复检，与中间件保持同一语义。
pub(crate) async fn check_revoked(jti: Option<&str>) -> std::result::Result<bool, String> {
    let Some(jti) = jti.filter(|v| !v.is_empty()) else {
        return Err("令牌缺少 jti，无法复检吊销状态".to_string());
    };
    let Some(store) = try_cache() else {
        return Err("缓存未初始化（认证路径必须有缓存）".to_string());
    };
    let key = format!("{}{}", constants::CACHE_KEY_PREFIX_TOKEN_BLACKLIST, jti);
    match Cache::get::<serde_json::Value>(store, &key).await {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(e) => Err(format!("黑名单查询失败: {e}")),
    }
}

/// 吊销复检本体（用 `axum::middleware::from_fn` 包一层后交给 `App::route_layer`）。
///
/// # 参数
///
/// - `request`：进入的请求（认证层的 `AuthClaims` 已在 extensions 中）。
/// - `next`：后续处理链。
///
/// # 返回值
///
/// 放行时原样返回下游响应；已吊销或无法判定时返回 401。
pub async fn guard(request: Request, next: Next) -> Response {
    // CORS 预检不携带凭据，认证层亦已放行，这里同样不拦
    if request.method() == axum::http::Method::OPTIONS {
        return next.run(request).await;
    }

    // 未注入声明：匿名接口（登录 / 刷新 / 公共参数）或无令牌请求，交给认证层与处理器处理
    let Some(AuthClaims(claims)) = request.extensions().get::<AuthClaims>().cloned() else {
        return next.run(request).await;
    };

    match check_revoked(claims.jti.as_deref()).await {
        Ok(false) => next.run(request).await,
        Ok(true) => {
            tracing::debug!(
                user_id = %claims.sub,
                jti = ?claims.jti,
                "令牌黑名单复检命中，拒绝请求"
            );
            unauthorized("登录状态已失效，请重新登录")
        }
        Err(reason) => {
            // fail-closed：判定不了宁可让用户重登，也不放行已吊销的令牌
            tracing::error!(
                user_id = %claims.sub,
                jti = ?claims.jti,
                error = %reason,
                "吊销状态复检失败，按 fail-closed 拒绝请求"
            );
            unauthorized("登录状态无法校验，请重新登录")
        }
    }
}

/// 构造 401 响应（与框架认证层同样式，客户端可统一处理）。
fn unauthorized(msg: &str) -> Response {
    let body = serde_json::to_string(&Res::<()>::fail(401, msg))
        .unwrap_or_else(|_| r#"{"code":401,"msg":"请重新登录"}"#.to_string());
    (
        StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        Body::from(body),
    )
        .into_response()
}

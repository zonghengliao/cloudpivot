//! 会话守卫中间件：令牌版本比对 + 首登强制改密。
//!
//! # 挂载位置（关键）
//!
//! 通过 [`nik::prelude::App::route_layer`] 注册，位于**框架内置中间件链之内**：
//! 认证层已经验签并注入 [`AuthClaims`]，因此本层直接读取声明，不必重复验签；
//! 同时又能拦下「已通过认证但会话已作废」的请求。
//!
//! > 若改用 `App::layer_from_fn`（链外），请求期读不到 `AuthClaims`（该层在认证层
//! > 之外），只能自行再验一次签名，且拿不到认证层的 401 语义。
//!
//! # 四条判定
//!
//! 1. **令牌版本**（安全审计 CODE-05 / CODE-08 + BUG-SYS-04 / FIX-05）：令牌 claim `ver`
//!    必须等于 `sys_user.token_version`。改密 / 改角色 / 停用 / 删除会使版本号自增，
//!    该用户已签发（含已登出的 refresh 之外的一切）令牌立即失效 → `401 + 20008`
//!    「您的权限已变更，请重新登录」；
//!    **菜单 / 角色授权变更同样自增**（`user::model::bump_token_version_of_menu_holders_in`
//!    / `bump_token_version_of_role_holders_in`），这就是「Redis 不可用时也不失守」
//!    的库侧兜底（BUG-SYS-03）；
//! 2. **首登强制改密**（CODE-06）：`sys_user.must_change_password = TRUE` 时，
//!    仅放行 [`constants::MUST_CHANGE_PASSWORD_ALLOWED_PATHS`]（改密、登出），
//!    其余接口一律 403，使初始口令无法用于任何业务操作；
//! 3. **通配权限复核**（BUG-SYS-04）：令牌声明了 `*` / `*:*:*` 时必须真持有
//!    `SUPER_ADMIN` 角色，否则 401 —— 接口级 403 只认令牌快照不查库，
//!    这条是「JWT 密钥泄漏 = 可自签超管」的唯一库侧拦口。
//! 4. **空闲超时**（AUTO-LOGOUT，2026-11-10）：会话按 `sid` 记「最后一次用户操作」，
//!    超过 `sys_config.auth.idle_timeout_minutes` 未操作 → `401 + 20009`，并吊销该会话
//!    名下的 refresh 令牌。**只有用户操作（写方法 / 带 `x-user-active: 1` 的请求）才续期**，
//!    自动轮询（GET）不续期 —— 见 [`crate::shared::idle_session`]。
//!
//! # 值从哪里读（FIX-05 的性能改造）
//!
//! 会话字段经 [`crate::shared::session_cache`] 读取：优先 `uv:{user_id}` 缓存（TTL 60 s），
//! 未命中 / 缓存不可用 / 缓存失效欠账时回查 `sys_user`。**库是唯一真源**：
//! 所有推进 `token_version` 的写点都在事务提交后删除缓存键，所以「撤销权限 → 旧令牌立刻 401」
//! 不依赖 TTL；缓存只省掉「正常请求」的那一次主键点查。
//!
//! **本校验没有任何开关**：不存在「关掉校验」的配置项，也不允许加。

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use nik::prelude::*;

use crate::shared::{constants, error_code, idle_session, session_cache};
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

    // 读会话字段：用户已删除 / 不存在 → 401。
    // FIX-05：走 `uv:{user_id}` 短 TTL 缓存（60 s），未命中 / 缓存不可用 → 直接查库。
    let flags = match session_cache::load_session_flags(&user_id).await {
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
    let ver = claims.extra_i64(constants::CLAIM_TOKEN_VERSION).unwrap_or(0);
    if !version_is_current(ver, token_version) {
        tracing::info!(
            user_id = %user_id,
            token_version,
            token_ver = ver,
            "令牌版本已过期，拒绝请求"
        );
        // 401 + 20008：令牌本身有效，但它承载的权限快照已被撤销
        // （改密 / 改角色 / 改角色权限 / 改菜单授权 / 停用 / 删除都会推进版本号）
        return unauthorized_with(error_code::AUTH_PERMISSION_CHANGED, "您的权限已变更，请重新登录");
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

    // 3) 通配权限的库侧复核（BUG-SYS-04）
    //
    // 框架的接口级 403 判定（`nik-web` 的 `has_permission`）只认**令牌里的权限快照**：
    // `is_super_admin() || permissions.contains(perm)`，不查库。于是「JWT 密钥泄漏」
    // 等于「全部接口失守」——自签一个 `permissions = ["*:*:*"]` 的令牌即可绕过一切
    // 路由闸（库侧兜底只覆盖超管数据范围 / 脱敏 / 用户角色上限，不覆盖权限码）。
    //
    // 这里补一条**廉价**护栏：只有声明了通配权限的令牌才回库核对（超管是极少数请求，
    // 普通用户多一次点查的收益不足以抵消成本，且他们的权限码本来就来自库里的角色）。
    // 库里角色不含 `SUPER_ADMIN` → 401（不是 403：这个令牌本身就不该存在）。
    match verify_wildcard_claims(&claims, &user_id).await {
        Ok(WildcardVerdict::NotWildcard) | Ok(WildcardVerdict::Backed) => {}
        Ok(WildcardVerdict::RoleMismatch { role_codes }) => {
            tracing::warn!(
                user_id = %user_id,
                roles = ?role_codes,
                "令牌声明了通配权限，但账号在库里不是超管角色 → 拒绝请求"
            );
            return unauthorized("权限声明与账号角色不符，请重新登录");
        }
        Err(e) => {
            // fail-closed：查不出角色就不放行通配权限
            tracing::error!(error = %e, user_id = %user_id, "通配权限复核查询失败，拒绝请求");
            return server_error("权限声明校验失败");
        }
    }

    // 4) 空闲超时（AUTO-LOGOUT）：会话超过 `auth.idle_timeout_minutes` 没有用户操作 → 401。
    //
    // 这是「真的退出」的服务端一半：前端计时可以被绕过（改 localStorage / 禁 JS /
    // 直接 curl 旧 token），服务端时间戳才是硬约束。什么样的请求算「用户操作」由
    // [`idle_session::is_user_activity`] 判定 —— **自动轮询（GET）不续期**，
    // 否则打开首页挂机永远不会超时。
    if let Some(sid) = claims
        .extra_str(constants::CLAIM_SESSION_ID)
        .filter(|value| !value.is_empty())
    {
        let user_active = idle_session::is_user_activity(request.method(), request.headers());
        if let idle_session::Verdict::Idle { idle_secs } = idle_session::check_and_touch(&sid, user_active).await {
            tracing::info!(
                user_id = %user_id,
                sid = %sid,
                idle_secs,
                path = %request.uri().path(),
                "会话空闲超时，拒绝请求（自动退出登录）"
            );
            // 顺手吊销该会话名下的 refresh 令牌：否则客户端还能用 refresh 续出新
            // access token，把「已空闲超时」变成「每 15 分钟自动复活」。
            if let Err(e) = crate::support::online::model::revoke_refresh_by_sid(&sid).await {
                tracing::warn!(user_id = %user_id, sid = %sid, error = %e, "空闲超时吊销 refresh 会话失败");
            }
            return unauthorized_with(error_code::AUTH_IDLE_TIMEOUT, "因长时间未操作已自动退出，请重新登录");
        }
    }

    next.run(request).await
}

/// 通配权限复核的判定结果。
///
/// 单独成一个枚举，是为了让单测（`system::authz_tests::o15_*`）能直接断言**判定**，
/// 而不需要 up 一个 HTTP 服务（本仓未声明 `tower` dev-dependency，做不了 `oneshot`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WildcardVerdict {
    /// 令牌没有通配权限 —— 无需回库复核（绝大多数请求走这条）。
    NotWildcard,
    /// 声明了通配权限，且库里确实持有 `SUPER_ADMIN` 角色 —— 放行。
    Backed,
    /// 声明了通配权限，但库里的角色不含 `SUPER_ADMIN` —— 伪造/过期声明，必须 401。
    RoleMismatch {
        /// 该账号在库里的角色编码（只用于日志排障）。
        role_codes: Vec<String>,
    },
}

/// 复核「令牌里的通配权限」是否被库里的角色背书（BUG-SYS-04）。
///
/// # 为什么只查通配令牌
///
/// 精确权限码（`system:menu:update` 等）本来就来自库里的角色 → 菜单授权，且角色授权
/// 变更会推进 `token_version` 让旧令牌立刻失效（BUG-SYS-03）。真正的缺口是
/// `is_super_admin()` 这条**短路**分支：它让任意持有 `*` / `*:*:*` 的令牌跳过全部权限码
/// 判定。JWT 密钥一旦泄漏，攻击者自签一个通配令牌就是「超管」——本函数是唯一的库侧拦口。
///
/// # 参数
///
/// - `claims`：令牌声明。
/// - `user_id`：令牌主体（已解析为 UUID）。
///
/// # 返回值
///
/// [`WildcardVerdict`]；查询失败返回 [`DbError`]（调用方按 fail-closed 处理）。
pub(crate) async fn verify_wildcard_claims(
    claims: &TokenClaims,
    user_id: &uuid::Uuid,
) -> std::result::Result<WildcardVerdict, DbError> {
    if !claims.is_super_admin() {
        return Ok(WildcardVerdict::NotWildcard);
    }
    let role_codes = model::load_role_codes(user_id).await?;
    if role_codes.iter().any(|code| code == constants::SUPER_ADMIN_ROLE_CODE) {
        return Ok(WildcardVerdict::Backed);
    }
    Ok(WildcardVerdict::RoleMismatch { role_codes })
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
    constants::MUST_CHANGE_PASSWORD_ALLOWED_PATHS.iter().any(|allowed| {
        let full = format!("{prefix}{allowed}");
        // 允许精确命中，也允许其子路径（如未来的 /system/auth/password/xxx）
        path == full || path.starts_with(&format!("{full}/"))
    })
}

/// 令牌里的版本号是否等于库里的当前版本号（会话守卫第 1 条判定）。
///
/// 抽成独立函数是为了让单测能直接钉死这条判定的两个边界（相等放行 / 不等拒绝），
/// 而不必 up 一个 HTTP 服务（本仓未声明 `tower` dev-dependency，做不了 `oneshot`）。
///
/// # 参数
///
/// - `token_ver`：令牌 claim `ver`（缺失时调用方按 `0` 传入）；
/// - `db_ver`：`sys_user.token_version`。
///
/// # 返回值
///
/// 相等返回 `true`（会话有效），否则 `false`（权限已变更 → 401）。
pub(crate) fn version_is_current(token_ver: i64, db_ver: i32) -> bool {
    token_ver == i64::from(db_ver)
}

/// 解析 `claims.sub` 为 UUID（主体非 UUID 视为会话无效）。
fn parse_uuid(raw: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(raw.trim()).ok()
}

/// 构造 401 响应（与框架认证层同样式，客户端可统一处理）。
///
/// `code` 字段固定写 `401`（HTTP 语义码）——用于「令牌缺失 / 主体非法 / 账号不存在」
/// 这类**没有更具体业务语义**的拒绝。
fn unauthorized(msg: &str) -> Response {
    unauthorized_with(401, msg)
}

/// 构造带业务错误码的 401 响应。
///
/// # 参数
///
/// - `code`：业务错误码（如 [`error_code::AUTH_PERMISSION_CHANGED`]）；
/// - `msg`：面向用户的提示。
pub(crate) fn unauthorized_with(code: i32, msg: &str) -> Response {
    build(
        StatusCode::UNAUTHORIZED,
        serde_json::to_string(&Res::<()>::fail(code, msg))
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
        [(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")],
        Body::from(body),
    )
        .into_response()
}

//! 认证处理器：登录、登出、令牌刷新与当前用户信息。
//!
//! 处理器承担**业务编排**（参数校验、分支判定、响应组装）与框架快捷方法的直接
//! 调用（按用户名 / 主键查用户、`of(...).set(...).update` 解锁等）；需要编写 SQL
//! 的查询与领域逻辑集中在 [`crate::system::auth::model`]。日志一律通过
//! [`crate::support::log`] 的 `record_*` 函数以**非阻塞**方式投递，不拖慢主流程。
//!
//! 文件顺序遵循项目约定：**路由方法在前，公共辅助函数在后**。

use alun::prelude::*;
use axum::Json;
use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use std::net::SocketAddr;

use crate::middleware::auth_guard;
use crate::shared::constants::{self, EnableStatus};
use crate::shared::error_code;
use crate::support::log::dto::LogPageQuery;
use crate::support::log::model::{JsonLogTable, page_json_logs};
use crate::support::log::{
    self, AuditLogInput, AuthLogInput, AuthStatus, AuthType, PasswordChangeLogInput,
    SecurityLogInput,
};
use crate::support::online::model::{OnlineSession, OnlineSessionModel, RefreshSession};
use crate::system::auth::dto::{
    ChangePasswordReq, CurrentUserRes, LoginReq, LoginUserRes, LogoutReq, RefreshTokenReq,
    TokenRes, UpdateProfileReq,
};
use crate::system::auth::model;
use crate::system::menu::dto::MenuRes;
use crate::system::user::model::SysUserModel;
use crate::utils;

/// 令牌类型（固定 `Bearer`）。
const TOKEN_TYPE: &str = "Bearer";

// ==================== 接口 ====================

/// 账密登录。
///
/// # 参数
///
/// - `headers`：请求头（用于派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP 做限流）。
/// - `req`：登录请求体（用户名 / 密码）。
///
/// # 返回值
///
/// 成功返回访问令牌、刷新令牌与登录用户摘要。
///
/// # 异常情况
///
/// - 429：登录尝试过于频繁；
/// - 401：用户名或密码错误；
/// - 403：账号已禁用或被锁定；
/// - 500：内部错误。
///
/// # 日志
///
/// 成功写 `auth_logs`（`LOGIN/SUCCESS`）与 `audit_logs`（`LOGIN_SUCCESS`）；
/// 失败写 `auth_logs`（`LOGIN/FAIL`）与 `security_logs`（`LOGIN_FAIL`）。
/// 锁定阈值与解锁时长取自 `sys_config`（缺省回退常量）。日志以非阻塞方式投递，
/// 不参与登录耗时。
#[alun::no_auth("/system/auth/login")]
#[alun::post("/system/auth/login")]
pub async fn login(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    ValidatedJson(req): ValidatedJson<LoginReq>,
) -> ResResult<TokenRes> {
    // 账号密码统一去除首尾空白后再参与查库与校验：
    // 登录页已做同样处理，这里兜底直连 API / 其他客户端（如脚本、第三方系统）的场景。
    let req = LoginReq {
        username: req.username.trim().to_string(),
        password: req.password.trim().to_string(),
    };
    // 校验器只保证原始长度 >= 1，纯空白串 trim 后为空，需在此拦下
    if req.username.is_empty() || req.password.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "用户名或密码不能为空",
        ));
    }

    let mut timer = LoginTimer::new();
    let ip_text = utils::client_ip(&headers, Some(&connect_info))
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    check_login_rate(&req.username, &ip_text).await?;
    timer.mark("rate_limit");

    let (max_fail, lock_minutes) = model::login_security_policy().await;
    timer.mark("security_policy");

    let user = match SysUserModel::find_first_by(db(), "username", (req.username.as_str(),)).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            timer.mark("load_user");
            log_login_failure(
                &headers,
                &connect_info,
                &req.username,
                None,
                "用户不存在",
                &timer,
            );
            return Err(ApiError::new(
                401,
                error_code::AUTH_BAD_CREDENTIALS,
                "用户名或密码错误",
            ));
        }
        Err(e) => return Err(utils::db_error(e, "查询登录用户")),
    };

    // 锁定期内直接拒绝；锁定已到期则自动解锁（重置失败计数）
    if is_locked_now(&user, lock_minutes) {
        timer.mark("load_user");
        log_login_failure(
            &headers,
            &connect_info,
            &req.username,
            Some(user.id),
            "账号锁定中",
            &timer,
        );
        return Err(ApiError::new(
            403,
            error_code::AUTH_ACCOUNT_LOCKED,
            "账号已被锁定，请稍后再试",
        ));
    }
    if user.is_locked {
        SysUserModel::of(user.id)
            .set(SysUserModel::is_locked(), false)
            .set(SysUserModel::login_fail_count(), 0i16)
            .set(
                SysUserModel::lock_time(),
                None::<chrono::DateTime<chrono::Utc>>,
            )
            .update(db())
            .await
            .map(|_| ())
            .map_err(|e| utils::db_error(e, "解锁账号"))?;
    }

    if user.status != EnableStatus::enabled {
        timer.mark("load_user");
        log_login_failure(
            &headers,
            &connect_info,
            &req.username,
            Some(user.id),
            "账号已禁用",
            &timer,
        );
        return Err(ApiError::new(
            403,
            error_code::AUTH_ACCOUNT_DISABLED,
            "账号已被禁用，请联系管理员",
        ));
    }

    let password_ok = Crypto::verify_password(&req.password, &user.password).map_err(|e| {
        tracing::error!(error = %e, user_id = %user.id, "密码校验失败");
        ApiError::new(500, error_code::INTERNAL_ERROR, "服务器内部错误")
    })?;

    timer.mark("verify_password");

    if !password_ok {
        let fails = user.login_fail_count.saturating_add(1);
        let is_lock = fails >= max_fail;
        model::record_login_failure(&user.id, fails, is_lock, chrono::Utc::now())
            .await
            .map_err(|e| utils::db_error(e, "更新登录失败计数"))?;
        let reason = if is_lock {
            "密码错误，账号已锁定"
        } else {
            "密码错误"
        };
        timer.mark("record_failure");
        log_login_failure(
            &headers,
            &connect_info,
            &req.username,
            Some(user.id),
            reason,
            &timer,
        );
        return Err(ApiError::new(
            401,
            error_code::AUTH_BAD_CREDENTIALS,
            "用户名或密码错误",
        ));
    }

    // 登录成功：重置失败计数、清除锁定并记录最后登录时间
    timer.mark("reset_status");
    SysUserModel::of(user.id)
        .set(SysUserModel::login_fail_count(), 0i16)
        .set(SysUserModel::is_locked(), false)
        .set(
            SysUserModel::lock_time(),
            None::<chrono::DateTime<chrono::Utc>>,
        )
        .set(SysUserModel::last_login_at(), Some(chrono::Utc::now()))
        .update(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "更新登录状态"))?;

    timer.mark("reset_status_done");
    // 登录即开新会话：`sid` / `siat` **只在这里生成**，之后每次轮换一律继承，
    // 使 refresh 令牌的有效期有了不可滑动的绝对上限（CODE-16 B1）。
    let session = SessionSeed::new_login();
    let (token, issued) = issue_token(&user, Some(&mut timer), &session).await?;
    register_online_session(&headers, &connect_info, &issued.access).await;
    register_refresh_session(&issued.refresh).await;
    timer.mark("online_session");

    log::record_auth(
        &headers,
        &connect_info,
        AuthLogInput {
            user_id: Some(user.id),
            username: Some(user.username.clone()),
            auth_type: AuthType::Login,
            auth_status: AuthStatus::Success,
            error_message: None,
            login_duration: Some(timer.total_ms()),
        },
    );
    timer.report();
    log::record_audit(
        &headers,
        &connect_info,
        AuditLogInput {
            user_id: Some(user.id),
            username: Some(user.username.clone()),
            operation: "LOGIN_SUCCESS".to_string(),
            target_type: Some("User".to_string()),
            target_id: Some(user.id.to_string()),
            before_data: None,
            after_data: None,
            detail: None,
        },
    );

    Ok(Res::ok(token))
}

/// 用刷新令牌换取新的令牌对。
///
/// 旧刷新令牌会被加入黑名单（一次性使用），权限等声明重新从数据库加载，
/// 不直接复用 `JWT::refresh`（后者会丢失角色 / 权限）。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `req`：刷新令牌请求体。
///
/// # 返回值
///
/// 成功返回新的访问令牌与刷新令牌。
///
/// # 异常情况
///
/// - 401：刷新令牌无效、类型错误或已被撤销；
/// - 403：账号已被禁用；
/// - 500：内部错误。
#[alun::no_auth("/system/auth/refresh")]
#[alun::post("/system/auth/refresh")]
pub async fn refresh(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    ValidatedJson(req): ValidatedJson<RefreshTokenReq>,
) -> ResResult<TokenRes> {
    let jwt = JWT::from_config();
    let claims = jwt.validate(&req.refresh_token).map_err(|_| {
        ApiError::new(
            401,
            error_code::AUTH_REFRESH_TOKEN_INVALID,
            "刷新令牌无效或已过期",
        )
    })?;

    if claims.token_type != Some(TokenType::Refresh) {
        return Err(ApiError::new(
            401,
            error_code::AUTH_REFRESH_TOKEN_INVALID,
            "令牌类型不正确",
        ));
    }

    // 旧令牌 `jti` 是轮换血缘的主键：没有它就无法登记「已作废」与「幂等回放」，
    // 也无法判复用，只能拒绝。
    let Some(old_jti) = claims.jti.clone().filter(|v| !v.is_empty()) else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_REFRESH_TOKEN_INVALID,
            "刷新令牌缺少唯一标识",
        ));
    };

    // ① 会话绝对上限（CODE-16 B1）：refresh 的 `exp` 恒等于 `siat + refresh_ttl`，
    //    但 jsonwebtoken 验签带 60s leeway，所以必须再显式比一次 `siat`——
    //    否则绝对上限会被 leeway 放宽，而「绝对」正是这条约束的全部意义。
    //    这一步排在幂等回放之前：会话已到期的请求不该再换到一对新令牌。
    let session = SessionSeed::inherit(&claims);
    let refresh_ttl = jwt.refresh_token_expire_secs() as i64;
    if now_secs() >= session.siat + refresh_ttl {
        log::record_auth(
            &headers,
            &connect_info,
            AuthLogInput {
                user_id: utils::require_uuid(&claims.sub, "sub").ok(),
                username: claims.username.clone(),
                auth_type: AuthType::RefreshToken,
                auth_status: AuthStatus::Fail,
                error_message: Some("刷新令牌已超过会话绝对上限".to_string()),
                login_duration: None,
            },
        );
        return Err(ApiError::new(
            401,
            error_code::AUTH_REFRESH_TOKEN_INVALID,
            "登录会话已到最长有效期，请重新登录",
        ));
    }

    // ② 轮换幂等（60s 宽限，评审 §1.1）：跨标签页并发刷新时第二次请求拿到的是
    //    同一个旧令牌，这里直接回放第一次的结果——不写审计、不杀会话。
    if let Some(replayed) = load_replay(&old_jti).await? {
        // 回放前必须确认「该会话仍然有效」：登出 / 强制下线会删掉 refresh 登记，
        // 只按幂等缓存回放的话，登出后 60s 内旧令牌仍能换到一对可用的新令牌。
        if replay_session_alive(&replayed).await? {
            tracing::debug!(user_id = %claims.sub, "命中轮换幂等窗口，回放同一对令牌");
            return Ok(Res::ok(replayed));
        }
        tracing::info!(user_id = %claims.sub, "幂等缓存对应的会话已被吊销，拒绝回放");
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_REVOKED,
            "刷新令牌已被撤销",
        ));
    }

    // ③ 血缘检查：该 refresh 已经轮换过 → 有人在重放旧令牌（CODE-16 B4）
    if let Some(mark) = load_rotated(&old_jti).await? {
        return Err(reuse_detected(&headers, &connect_info, &claims, &mark).await);
    }

    // ④ 黑名单（登出 / 强制下线）：框架对该接口跳过检查，这里自己做且 **fail-closed**
    match auth_guard::check_revoked(Some(old_jti.as_str())).await {
        Ok(false) => {}
        Ok(true) => {
            return Err(ApiError::new(
                401,
                error_code::AUTH_TOKEN_REVOKED,
                "刷新令牌已被撤销",
            ));
        }
        Err(reason) => {
            tracing::error!(
                user_id = %claims.sub,
                error = %reason,
                "刷新路径的黑名单复检失败，按 fail-closed 拒绝"
            );
            return Err(cache_unavailable("校验刷新令牌吊销状态"));
        }
    }

    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let user = match SysUserModel::find_or_none(db(), &user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            log::record_auth(
                &headers,
                &connect_info,
                AuthLogInput {
                    user_id: Some(user_id),
                    username: claims.username.clone(),
                    auth_type: AuthType::RefreshToken,
                    auth_status: AuthStatus::Fail,
                    error_message: Some("刷新令牌对应的用户不存在".to_string()),
                    login_duration: None,
                },
            );
            return Err(ApiError::new(
                401,
                error_code::AUTH_REFRESH_TOKEN_INVALID,
                "刷新令牌对应的用户不存在",
            ));
        }
        Err(e) => return Err(utils::db_error(e, "查询刷新用户")),
    };

    if user.status != EnableStatus::enabled {
        log::record_auth(
            &headers,
            &connect_info,
            AuthLogInput {
                user_id: Some(user.id),
                username: Some(user.username.clone()),
                auth_type: AuthType::RefreshToken,
                auth_status: AuthStatus::Fail,
                error_message: Some("账号已禁用".to_string()),
                login_duration: None,
            },
        );
        return Err(ApiError::new(
            403,
            error_code::AUTH_ACCOUNT_DISABLED,
            "账号已被禁用，请联系管理员",
        ));
    }

    // 令牌版本号比对：改密 / 改角色 / 停用 / 删除会自增 token_version，
    // 旧 refresh token 即使没被逐个拉黑，也在此处整体失效（安全审计 CODE-05 / CODE-08）。
    let token_version = claims
        .extra_i64(constants::CLAIM_TOKEN_VERSION)
        .unwrap_or(0);
    if token_version != i64::from(user.token_version) {
        log::record_auth(
            &headers,
            &connect_info,
            AuthLogInput {
                user_id: Some(user.id),
                username: Some(user.username.clone()),
                auth_type: AuthType::RefreshToken,
                auth_status: AuthStatus::Fail,
                error_message: Some("刷新令牌的令牌版本号已过期".to_string()),
                login_duration: None,
            },
        );
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_REVOKED,
            "登录状态已失效，请重新登录",
        ));
    }

    // 旧刷新令牌一次性作废：必须在签发新令牌**之前**落地，且写失败即 500——
    // 「没记录作废就发新令牌」等于放过一次复用（CODE-16 B4 顺序第 5 步）。
    crate::support::online::model::blacklist_token(&old_jti, claims.exp as i64)
        .await
        .map_err(|e| {
            tracing::error!(user_id = %claims.sub, error = %e, "旧刷新令牌写黑名单失败");
            cache_unavailable("作废旧刷新令牌")
        })?;

    // 刷新路径不采集分段耗时：令牌已存在，慢点只在首次登录需要定位。
    let (token, issued) = issue_token(&user, None, &session).await?;

    // 血缘标记（判复用）+ 幂等回放（宽限）+ 新旧在线登记
    let mark = RotatedMark {
        sid: session.sid.clone(),
        at: now_secs(),
    };
    persist_rotation(&old_jti, &mark, &token, claims.exp).await?;
    if let Err(e) = OnlineSessionModel::forget_refresh(&old_jti).await {
        tracing::warn!(user_id = %claims.sub, error = %e, "移除旧 refresh 令牌登记失败");
    }
    register_online_session(&headers, &connect_info, &issued.access).await;
    register_refresh_session(&issued.refresh).await;

    log::record_auth(
        &headers,
        &connect_info,
        AuthLogInput {
            user_id: Some(user.id),
            username: Some(user.username.clone()),
            auth_type: AuthType::RefreshToken,
            auth_status: AuthStatus::Success,
            error_message: None,
            login_duration: None,
        },
    );
    Ok(Res::ok(token))
}

/// 登出：把当前访问令牌加入黑名单，并移除在线会话。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：认证中间件注入的令牌声明。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// 无（令牌缺失由认证中间件提前拦截为 401）。
///
/// # 日志
///
/// 写入 `auth_logs`（`LOGOUT/SUCCESS`）；写入失败不影响登出结果。
#[alun::post("/system/auth/logout")]
pub async fn logout(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    payload: Option<Json<LogoutReq>>,
) -> ResResult<()> {
    let jwt = JWT::from_config();
    jwt.logout(&claims).await;
    revoke_online_session(&claims).await;

    // 同一会话（`sid`）名下的 refresh 令牌一并吊销：客户端不必回传 refresh，
    // 「登出后旧 refresh 仍能续期」的窗口也关掉了（CODE-05 / CODE-16 B7）。
    // 失败要出声：吊销失败却回 200 就是静默 fail-open（评审 §1.3）。
    if let Some(sid) = claims.extra_str(constants::CLAIM_SESSION_ID)
        && let Err(e) = crate::support::online::model::revoke_refresh_by_sid(&sid).await
    {
        tracing::error!(
            user_id = %claims.sub,
            sid = %sid,
            error = %e,
            "登出时吊销本次会话的 refresh 令牌失败"
        );
        return Err(cache_unavailable("吊销本次会话的刷新令牌"));
    }

    // 同时吊销请求体带来的 refresh token（兼容只提交旧令牌的客户端）：
    // 按 jti 再兜底拉黑一次，失败同样必须让调用方看到。
    if let Some(Json(req)) = payload
        && let Some(raw) = req
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
    {
        match jwt.validate(raw) {
            Ok(refresh_claims)
                if refresh_claims.token_type == Some(TokenType::Refresh)
                    && refresh_claims.sub == claims.sub =>
            {
                if let Some(jti) = refresh_claims.jti.as_deref()
                    && let Err(e) = crate::support::online::model::blacklist_token(
                        jti,
                        refresh_claims.exp as i64,
                    )
                    .await
                {
                    tracing::error!(
                        user_id = %claims.sub,
                        error = %e,
                        "登出时拉黑客户端提交的 refresh 令牌失败"
                    );
                    return Err(cache_unavailable("吊销刷新令牌"));
                }
            }
            Ok(_) => tracing::warn!(
                user_id = %claims.sub,
                "登出时提交的 refresh token 与当前会话不匹配，已忽略"
            ),
            Err(e) => tracing::warn!(
                user_id = %claims.sub,
                error = %e,
                "登出时提交的 refresh token 无效，已忽略"
            ),
        }
    }
    log::record_auth(
        &headers,
        &connect_info,
        AuthLogInput {
            user_id: utils::current_user_uuid(&claims),
            username: claims.username.clone(),
            auth_type: AuthType::Logout,
            auth_status: AuthStatus::Success,
            error_message: None,
            login_duration: None,
        },
    );
    Ok(Res::ok_msg("已退出登录"))
}

/// 当前登录用户信息（含角色编码与权限码）。
///
/// # 参数
///
/// - `claims`：认证中间件注入的令牌声明。
///
/// # 返回值
///
/// 成功返回当前用户详情。
///
/// # 异常情况
///
/// - 401：令牌中的用户已不存在；
/// - 500：内部错误。
#[alun::get("/system/auth/me")]
pub async fn me(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<CurrentUserRes> {
    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let user = SysUserModel::find_or_none(db(), &user_id)
        .await
        .map_err(|e| utils::db_error(e, "查询当前用户"))?
        .ok_or_else(|| ApiError::new(401, error_code::USER_NOT_FOUND, "用户不存在"))?;

    let codes = model::load_role_codes(&user_id)
        .await
        .map_err(|e| utils::db_error(e, "查询用户角色"))?;
    let super_admin = model::is_super_admin(&codes);
    let permissions = model::load_permissions(&user_id, super_admin)
        .await
        .map_err(|e| utils::db_error(e, "查询用户权限"))?;

    Ok(Res::ok(CurrentUserRes {
        id: user.id.to_string(),
        username: user.username,
        real_name: user.real_name,
        email: user.email,
        mobile: user.mobile,
        dept_id: user.dept_id.map(|v| v.to_string()),
        roles: codes,
        permissions,
        must_change_password: user.must_change_password,
    }))
}

/// 当前登录用户的菜单树（仅目录与菜单，按 `sort_order` 升序）。
///
/// # 参数
///
/// - `claims`：认证中间件注入的令牌声明。
///
/// # 返回值
///
/// 成功返回菜单树根节点列表。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[alun::get("/system/auth/menus")]
pub async fn menus(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
) -> ResResult<Vec<MenuRes>> {
    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let super_admin = claims.is_super_admin();
    let nodes = model::load_user_menus(&user_id, super_admin)
        .await
        .map_err(|e| utils::db_error(e, "查询用户菜单"))?;
    Ok(Res::ok(build_menu_tree(nodes)))
}

/// 更新当前登录用户的个人资料（姓名 / 邮箱 / 手机号）。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：认证中间件注入的令牌声明。
/// - `req`：待更新的资料字段（`None` 表示保持原值，空串表示清空）。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：字段校验失败或邮箱格式非法；
/// - 401：令牌中的用户已不存在；
/// - 500：内部错误。
///
/// # 日志
///
/// 成功后写入 `audit_logs`（`PROFILE_UPDATE`）；日志写入失败不影响业务结果。
#[alun::put("/system/auth/profile")]
pub async fn update_profile(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<UpdateProfileReq>,
) -> ResResult<()> {
    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let mut user = SysUserModel::find_or_none(db(), &user_id)
        .await
        .map_err(|e| utils::db_error(e, "查询当前用户"))?
        .ok_or_else(|| ApiError::new(401, error_code::USER_NOT_FOUND, "用户不存在"))?;

    // 仅在提交了对应字段时覆盖，避免误清空其它资料
    if let Some(real_name) = req.real_name.as_deref() {
        user.real_name = utils::trimmed(Some(real_name)).map(str::to_string);
    }
    if let Some(email) = req.email.as_deref() {
        user.email = utils::trimmed(Some(email)).map(str::to_string);
    }
    if let Some(mobile) = req.mobile.as_deref() {
        user.mobile = utils::trimmed(Some(mobile)).map(str::to_string);
    }
    user.updated_at = chrono::Utc::now();
    user.update_by = utils::current_user_uuid(&claims);

    user.update(db())
        .await
        .map_err(|e| utils::db_error(e, "更新个人资料"))?;

    log::record_audit(
        &headers,
        &connect_info,
        log::audit_input(
            &claims,
            "PROFILE_UPDATE",
            "User",
            user_id.to_string(),
            None,
            None,
        ),
    );
    Ok(Res::ok_msg("个人资料已更新"))
}

/// 修改当前登录用户的密码（需校验原密码）。
///
/// # 参数
///
/// - `headers`：请求头（派生日志字段）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：认证中间件注入的令牌声明。
/// - `req`：原密码与新密码。
///
/// # 返回值
///
/// 成功返回空响应。
///
/// # 异常情况
///
/// - 400：新密码长度不足；
/// - 401：原密码错误，或令牌中的用户已不存在；
/// - 500：内部错误。
///
/// # 日志
///
/// 成功写 `password_change_logs`（`UPDATE/SELF/SUCCESS`）；原密码错误写
/// `password_change_logs`（`UPDATE/SELF/FAIL`）；日志写入失败不影响业务结果。
#[alun::put("/system/auth/password")]
pub async fn change_password(
    headers: HeaderMap,
    ConnectInfo(connect_info): ConnectInfo<SocketAddr>,
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    ValidatedJson(req): ValidatedJson<ChangePasswordReq>,
) -> ResResult<()> {
    let user_id = utils::require_uuid(&claims.sub, "sub")?;
    let user = SysUserModel::find_or_none(db(), &user_id)
        .await
        .map_err(|e| utils::db_error(e, "查询当前用户"))?
        .ok_or_else(|| ApiError::new(401, error_code::USER_NOT_FOUND, "用户不存在"))?;

    let password_ok = Crypto::verify_password(&req.old_password, &user.password).map_err(|e| {
        tracing::error!(error = %e, user_id = %user_id, "原密码校验失败");
        ApiError::new(500, error_code::INTERNAL_ERROR, "服务器内部错误")
    })?;
    if !password_ok {
        log::record_password_change(
            &headers,
            &connect_info,
            PasswordChangeLogInput {
                user_id,
                username: user.username.clone(),
                change_type: "UPDATE".to_string(),
                operator_type: "SELF".to_string(),
                operator_id: Some(user_id),
                change_status: "FAIL".to_string(),
                error_message: Some("原密码错误".to_string()),
            },
        );
        return Err(ApiError::new(
            401,
            error_code::AUTH_BAD_CREDENTIALS,
            "原密码错误",
        ));
    }

    let hashed = Crypto::hash_password(&req.new_password)
        .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "密码加密失败"))?;
    SysUserModel::of(user_id)
        .set(SysUserModel::password(), hashed)
        // 改密成功即解除「首登强制改密」标记（安全审计 CODE-06）
        .set(SysUserModel::must_change_password(), false)
        .set(SysUserModel::updated_at(), chrono::Utc::now())
        .set(SysUserModel::update_by(), utils::current_user_uuid(&claims))
        .update(db())
        .await
        .map(|_| ())
        .map_err(|e| utils::db_error(e, "修改密码"))?;

    // 改密即全端下线：令牌版本号自增，使该用户已签发的 access / refresh 令牌全部失效
    // （含本次请求所用的令牌），客户端须用新口令重新登录（安全审计 CODE-05 / CODE-08）。
    if let Err(e) = crate::system::user::model::bump_token_version(&user_id).await {
        tracing::error!(error = %e, user_id = %user_id, "改密后自增令牌版本号失败");
    }
    crate::system::auth::model::invalidate_user_principal(&user_id).await;
    // 缓存侧的会话吊销失败按**可接受降级**处理，不返回 500：上面已把库里的
    // `token_version` 自增（认证层逐请求比对、刷新路径也比对），旧 access / refresh
    // 在数据库这一侧已经作废；此时回 500 只会让「密码其实已改成功」的操作看起来失败，
    // 反而诱发无意义的重复改密。失败仍以 `error` 级日志留痕（不静默）。
    if let Err(e) = crate::support::online::model::force_logout_user(&user_id.to_string()).await {
        tracing::error!(
            user_id = %user_id,
            error = %e,
            "改密后清理在线会话失败（令牌版本号已自增，库侧吊销仍生效）"
        );
    }

    log::record_password_change(
        &headers,
        &connect_info,
        PasswordChangeLogInput {
            user_id,
            username: user.username.clone(),
            change_type: "UPDATE".to_string(),
            operator_type: "SELF".to_string(),
            operator_id: Some(user_id),
            change_status: "SUCCESS".to_string(),
            error_message: None,
        },
    );
    Ok(Res::ok_msg("密码已修改"))
}

/// 当前登录用户自己的登录（认证）日志分页。
///
/// 普通用户无需日志查看权限即可查看本人登录记录，因此按当前用户名校验，
/// 不额外声明权限码。
///
/// # 参数
///
/// - `claims`：认证中间件注入的令牌声明（取用户名做过滤）。
/// - `query`：分页参数（仅使用 `page` / `page_size`）。
///
/// # 返回值
///
/// 成功返回本人认证日志的 JSON 行分页（按时间倒序）。
///
/// # 异常情况
///
/// 数据库异常返回 500。
#[alun::get("/system/auth/logs")]
pub async fn login_logs(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<LogPageQuery>,
) -> ResResult<PageData<Vec<JsonValue>>> {
    let (page, page_size) = utils::normalize_page(query.page, query.page_size);
    let result = page_json_logs(
        JsonLogTable::Auth,
        claims.username.clone(),
        None,
        None,
        page,
        page_size,
    )
    .await
    .map_err(|e| utils::db_error(e, "查询登录日志"))?;
    Ok(Res::page(
        result.list,
        result.total,
        result.page,
        result.size,
    ))
}

// ==================== 公共辅助 ====================

/// 登录耗时分段采集器。
///
/// 用于把「登录耗时偏长」拆到具体环节：调用方按执行顺序依次
/// [`LoginTimer::mark`]，结束时 `report()` 一次性输出各段毫秒数。只做计时与
/// 一条 `tracing::info!`，不参与业务判定、不写库。
struct LoginTimer {
    /// 分段名称与自上一段以来的耗时（毫秒）。
    steps: Vec<(&'static str, u128)>,
    /// 登录开始的时刻。
    start: std::time::Instant,
    /// 上一次打点的时刻。
    last: std::time::Instant,
}

impl LoginTimer {
    /// 创建采集器（起点即当前时刻）。
    fn new() -> Self {
        let start = std::time::Instant::now();
        Self {
            steps: Vec::new(),
            start,
            last: start,
        }
    }

    /// 记录自上一段（或起点）以来消耗的毫秒数，并把后续阶段命名。
    fn mark(&mut self, label: &'static str) -> u128 {
        let now = std::time::Instant::now();
        let spent = now.saturating_duration_since(self.last).as_millis();
        self.last = now;
        self.steps.push((label, spent));
        spent
    }

    /// 本次登录自起点到此刻的总耗时（毫秒）。
    fn total_ms(&self) -> i32 {
        std::time::Instant::now()
            .saturating_duration_since(self.start)
            .as_millis()
            .min(i32::MAX as u128) as i32
    }

    /// 输出分段耗时日志。
    fn report(&self) {
        let detail: Vec<String> = self
            .steps
            .iter()
            .map(|(label, ms)| format!("{label}={ms}ms"))
            .collect();
        tracing::info!(total = ?self.total_ms(), steps = ?detail, "登录耗时分解");
    }
}

/// 会话种子：`sid`（uuid v4）+ `siat`（Unix 秒）。
///
/// 登录时新建，轮换时**从旧 refresh 令牌的 claims 继承**——两条规则合起来才让
/// refresh 令牌的有效期有了不可滑动的绝对上限（CODE-16 B1）：
/// 只要 `now >= siat + refresh_ttl`，无论轮换过多少次都必须重新登录。
struct SessionSeed {
    /// 会话标识。
    sid: String,
    /// 会话创建时刻（Unix 秒）。
    siat: i64,
}

impl SessionSeed {
    /// 登录：开一个新会话。
    fn new_login() -> Self {
        Self {
            sid: uuid::Uuid::new_v4().to_string(),
            siat: now_secs(),
        }
    }

    /// 刷新：继承旧 refresh 令牌的会话标识与创建时刻。
    ///
    /// 历史令牌（本整改上线前签发、没有 `sid` / `siat`）以 `iat` 兜底：它们的
    /// `exp` 本来就等于 `iat + refresh_ttl`，因此绝对上限既不会被延长，
    /// 也不会把正在使用的会话立刻踢下线。
    fn inherit(claims: &TokenClaims) -> Self {
        let sid = claims
            .extra_str(constants::CLAIM_SESSION_ID)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let siat = claims
            .extra_i64(constants::CLAIM_SESSION_ISSUED_AT)
            .unwrap_or(claims.iat as i64);
        Self { sid, siat }
    }
}

/// 一次签发的令牌对（access + refresh）及其声明。
struct IssuedTokens {
    /// 访问令牌声明（在线会话登记用）。
    access: TokenClaims,
    /// 刷新令牌声明（refresh 登记与血缘标记用）。
    refresh: TokenClaims,
}

/// 组装登录用户的角色编码、权限码与令牌对。
///
/// # 参数
///
/// - `user`：已通过身份校验的用户模型。
/// - `timer`：耗时分段采集器（仅登录路径传入）。
/// - `session`：会话种子（登录新建 / 刷新继承），决定 `sid` / `siat` 与 refresh 的 `exp`。
///
/// # 返回值
///
/// `(令牌响应, 令牌对声明)`。声明用于登记在线会话与 refresh 令牌（签发接口只返回字符串）。
///
/// # 异常情况
///
/// 角色 / 权限查询或令牌签发失败返回 500；签发后回读声明失败同样返回 500。
async fn issue_token(
    user: &SysUserModel,
    mut timer: Option<&mut LoginTimer>,
    session: &SessionSeed,
) -> std::result::Result<(TokenRes, IssuedTokens), ApiError> {
    // 计时器是可选的：登录路径传入采集器，刷新路径不采集。
    let mut mark_stage = |label: &'static str| {
        if let Some(timer) = timer.as_deref_mut() {
            timer.mark(label);
        }
    };

    let codes = model::load_role_codes(&user.id)
        .await
        .map_err(|e| utils::db_error(e, "查询用户角色"))?;
    mark_stage("load_roles");
    let super_admin = model::is_super_admin(&codes);
    let permissions = model::load_permissions(&user.id, super_admin)
        .await
        .map_err(|e| utils::db_error(e, "查询用户权限"))?;
    mark_stage("load_permissions");

    let jwt = JWT::from_config();
    let user_id = user.id.to_string();
    // Access 与 Refresh 由**同一份** ClaimsBuilder 派生（框架推荐的签发入口）。
    // 这样后续追加自定义声明（如企业版的 `tenant_id`）时，刷新链路不会丢失载荷——
    // `create_refresh_token` 只带 `sub`，刷新后自定义声明会全部蒸发。
    let builder = || {
        ClaimsBuilder::new(user_id.clone())
            .username(&user.username)
            .roles(&codes)
            .permissions(&permissions)
            // 令牌版本号：认证层（middleware/session_guard.rs）逐请求与库中值比对，
            // 改密 / 改角色 / 停用即整体作废旧令牌（安全审计 CODE-08）
            .claim_i64(
                constants::CLAIM_TOKEN_VERSION,
                i64::from(user.token_version),
            )
            // 会话标识与创建时刻：refresh 轮换时原样继承，构成绝对上限基准（CODE-16 B1）
            .claim_str(constants::CLAIM_SESSION_ID, session.sid.clone())
            .claim_i64(constants::CLAIM_SESSION_ISSUED_AT, session.siat)
    };
    let access_token = jwt.issue(builder()).map_err(|e| {
        tracing::error!(error = %e, "签发访问令牌失败");
        ApiError::new(500, error_code::INTERNAL_ERROR, "服务器内部错误")
    })?;

    // refresh 的 `exp` **固定**为 `siat + refresh_ttl`：轮换只换 jti，不推后上限。
    // `ClaimsBuilder::build(type, expire_secs)` 按「当前时刻 + expire_secs」算 exp，
    // 因此这里传「距绝对上限的剩余秒数」即得到 `exp = siat + refresh_ttl`。
    let refresh_ttl = jwt.refresh_token_expire_secs() as i64;
    let remaining = (session.siat + refresh_ttl - now_secs()).max(1) as u64;
    let refresh_claims = builder().build(TokenType::Refresh, remaining);
    let refresh_token = jwt.create_token_with(refresh_claims.clone()).map_err(|e| {
        tracing::error!(error = %e, "签发刷新令牌失败");
        ApiError::new(500, error_code::INTERNAL_ERROR, "服务器内部错误")
    })?;
    mark_stage("issue_jwt");

    // 签发接口只返回字符串，回读一次即可取得 jti / exp 供在线会话登记
    let claims = jwt.validate(&access_token).map_err(|e| {
        tracing::error!(error = %e, "回读访问令牌声明失败");
        ApiError::new(500, error_code::INTERNAL_ERROR, "服务器内部错误")
    })?;
    mark_stage("readback_claims");

    let token = TokenRes {
        access_token,
        refresh_token,
        token_type: TOKEN_TYPE.to_string(),
        expires_in: jwt.access_token_expire_secs(),
        user: Some(LoginUserRes {
            id: user_id,
            username: user.username.clone(),
            real_name: user.real_name.clone(),
        }),
        // 前端据此直接跳改密页，不必先撞一次 403（安全审计 CODE-06）
        must_change_password: user.must_change_password,
    };
    Ok((
        token,
        IssuedTokens {
            access: claims,
            refresh: refresh_claims,
        },
    ))
}

/// 当前 Unix 时间戳（秒）。
fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 缓存不可用时的统一 500 错误。
///
/// 吊销 / 轮换状态都落在缓存里：缓存不可用时**不能**假装成功（那就是 fail-open），
/// 一律回 500 让客户端重试（评审 §1.3）。
fn cache_unavailable(action: &str) -> ApiError {
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("会话状态暂不可用，无法{action}，请稍后重试"),
    )
}

/// 已轮换（作废）refresh 令牌的血缘标记。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RotatedMark {
    /// 会话标识。
    sid: String,
    /// 轮换时刻（Unix 秒）。
    at: i64,
}

/// 读取轮换幂等结果（`token:replay:{旧 jti}`）。
///
/// # 返回值
///
/// - `Ok(Some(_))`：宽限窗口内命中，直接回放同一对令牌；
/// - `Ok(None)`：未命中；
/// - `Err(_)`：缓存不可用（500）。
async fn load_replay(old_jti: &str) -> std::result::Result<Option<TokenRes>, ApiError> {
    let Some(store) = try_cache() else {
        return Err(cache_unavailable("校验令牌轮换状态"));
    };
    let key = format!("{}{}", constants::CACHE_KEY_PREFIX_TOKEN_REPLAY, old_jti);
    match Cache::get::<TokenRes>(store, &key).await {
        Ok(pair) => Ok(pair),
        Err(e) => {
            tracing::error!(error = %e, "读取轮换幂等缓存失败");
            Err(cache_unavailable("校验令牌轮换状态"))
        }
    }
}

/// 读取轮换血缘标记（`token:rotated:{旧 jti}`）。
///
/// # 返回值
///
/// - `Ok(Some(_))`：该 refresh 令牌已被轮换过；
/// - `Ok(None)`：未轮换过；
/// - `Err(_)`：缓存不可用（500）。
async fn load_rotated(old_jti: &str) -> std::result::Result<Option<RotatedMark>, ApiError> {
    let Some(store) = try_cache() else {
        return Err(cache_unavailable("校验令牌轮换状态"));
    };
    let key = format!("{}{}", constants::CACHE_KEY_PREFIX_TOKEN_ROTATED, old_jti);
    match Cache::get::<RotatedMark>(store, &key).await {
        Ok(mark) => Ok(mark),
        Err(e) => {
            tracing::error!(error = %e, "读取轮换血缘标记失败");
            Err(cache_unavailable("校验令牌轮换状态"))
        }
    }
}

/// 判断幂等回放出的令牌对所属会话是否仍然有效。
///
/// 判据：它携带的 refresh 令牌**仍在 `online:refresh:*` 注册表里**。登出
/// （`revoke_refresh_by_sid`）与强制下线（`force_logout_user`）都会删掉登记，
/// 因此「登记缺失」即视为已吊销——这是 60s 宽限窗口不会绕过登出的关键。
///
/// # 返回值
///
/// - `Ok(true)`：会话仍有效，可以回放；
/// - `Ok(false)`：会话已吊销 / 缓存内容无法验签，不可回放；
/// - `Err(_)`：缓存不可用（500，fail-closed）。
async fn replay_session_alive(pair: &TokenRes) -> std::result::Result<bool, ApiError> {
    let jwt = JWT::from_config();
    let claims = match jwt.validate(&pair.refresh_token) {
        Ok(claims) => claims,
        Err(e) => {
            // 回放的是自己刚签发的令牌；验不过说明缓存内容异常或密钥已轮换
            tracing::warn!(error = %e, "幂等缓存中的 refresh 令牌无法验签，按已吊销处理");
            return Ok(false);
        }
    };
    let Some(jti) = claims.jti.as_deref() else {
        return Ok(false);
    };
    match OnlineSessionModel::find_refresh(jti).await {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(e) => {
            tracing::error!(error = %e, "查询 refresh 令牌登记失败，回放按 fail-closed 拒绝");
            Err(cache_unavailable("校验令牌轮换状态"))
        }
    }
}

/// 记录一次成功轮换：写血缘标记（判复用）+ 幂等回放（宽限）。
///
/// # 参数
///
/// - `old_jti`：被换下的 refresh 令牌 `jti`。
/// - `mark`：血缘标记。
/// - `pair`：本次签发的新令牌对（幂等回放时原样返回）。
/// - `old_exp`：旧 refresh 令牌的 `exp`（血缘标记的存活上限）。
///
/// # 异常情况
///
/// 缓存不可用或写入失败返回 500——写不进去就意味着下次复用无法识别。
async fn persist_rotation(
    old_jti: &str,
    mark: &RotatedMark,
    pair: &TokenRes,
    old_exp: usize,
) -> std::result::Result<(), ApiError> {
    let Some(store) = try_cache() else {
        return Err(cache_unavailable("记录令牌轮换"));
    };
    let rotated_key = format!("{}{}", constants::CACHE_KEY_PREFIX_TOKEN_ROTATED, old_jti);
    let replay_key = format!("{}{}", constants::CACHE_KEY_PREFIX_TOKEN_REPLAY, old_jti);
    let ttl = (old_exp as i64 - now_secs()).max(1) as u64;
    Cache::set_ex(store, &rotated_key, mark, ttl)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, old_jti, "写轮换血缘标记失败");
            cache_unavailable("记录令牌轮换")
        })?;
    Cache::set_ex(
        store,
        &replay_key,
        pair,
        constants::TOKEN_ROTATION_REPLAY_GRACE_SECS,
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, old_jti, "写轮换幂等缓存失败");
        cache_unavailable("记录令牌轮换")
    })
}

/// 判定「refresh 令牌复用」并执行处置（CODE-16 B4）。
///
/// 处置三件事：
///
/// 1. **注销该用户全部会话**（access 与 refresh 都在注册表里，一次全杀）——
///    令牌被复用意味着它已经泄露，只作废这一个 jti 不够；
/// 2. 写 `auth_logs`（`REFRESH_TOKEN/FAIL`，`error_message` 带 `REUSE_DETECTED`）
///    与 `security_logs`（`REFRESH_TOKEN_REUSE`）；
/// 3. 返回 401 `AUTH_TOKEN_REVOKED`（复用现有错误码，不新增无语义码）。
///
/// # 返回值
///
/// 供调用方直接 `return Err(...)` 的 401 错误。
async fn reuse_detected(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
    mark: &RotatedMark,
) -> ApiError {
    let kicked = match crate::support::online::model::force_logout_user(&claims.sub).await {
        Ok(count) => count,
        Err(e) => {
            // 会话吊销失败也不能放行：仍按复用拒绝，并且必须留痕
            tracing::error!(user_id = %claims.sub, error = %e, "复用检测后吊销全部会话失败");
            0
        }
    };
    tracing::warn!(
        user_id = %claims.sub,
        sid = %mark.sid,
        rotated_at = mark.at,
        revoked_tokens = kicked,
        "检测到 refresh 令牌复用，已注销该用户全部会话"
    );

    let user_id = utils::require_uuid(&claims.sub, "sub").ok();
    log::record_auth(
        headers,
        connect_info,
        AuthLogInput {
            user_id,
            username: claims.username.clone(),
            auth_type: AuthType::RefreshToken,
            auth_status: AuthStatus::Fail,
            error_message: Some(format!(
                "REUSE_DETECTED: 刷新令牌在 {} 轮换后再次被使用，已注销该用户全部会话（吊销 {kicked} 个令牌）",
                mark.at
            )),
            login_duration: None,
        },
    );
    log::record_security(
        headers,
        connect_info,
        SecurityLogInput {
            user_id,
            username: claims.username.clone(),
            event_type: "REFRESH_TOKEN_REUSE".to_string(),
            detail: Some(serde_json::json!({
                "sid": mark.sid,
                "rotated_at": mark.at,
                "revoked_tokens": kicked,
            })),
        },
    );

    ApiError::new(
        401,
        error_code::AUTH_TOKEN_REVOKED,
        "登录状态已失效，请重新登录",
    )
}

/// 登记在线会话（登录 / 刷新成功后调用）。
///
/// # 参数
///
/// - `headers`：请求头（解析 User-Agent）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `claims`：本次签发的访问令牌声明。
///
/// # 说明
///
/// 归属地仅读缓存（不外呼），保证请求路径不被网络 I/O 阻塞；登记失败只记告警，
/// **不影响登录结果**（在线列表属于旁路观测能力）。
async fn register_online_session(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    claims: &TokenClaims,
) {
    let Some(token_id) = claims.jti.as_deref() else {
        tracing::warn!(user_id = %claims.sub, "访问令牌缺少 jti，跳过在线会话登记");
        return;
    };
    let ip = utils::client_ip(headers, Some(connect_info));
    let (browser, os_type) = match utils::user_agent(headers) {
        Some(raw) => {
            let info = alun_utils::parse_user_agent(&raw);
            (Some(info.browser_type), Some(info.os_type))
        }
        None => (None, None),
    };
    let location = match ip {
        Some(addr) => crate::support::ipgeo::cached(addr).await,
        None => None,
    };

    let now = chrono::Utc::now();
    let expire_at = claims.exp as i64;
    let ttl = (expire_at - now.timestamp()).max(1) as u64;
    let session = OnlineSession {
        token_id: token_id.to_string(),
        user_id: claims.sub.clone(),
        username: claims.username.clone().unwrap_or_default(),
        ip: ip.map(|addr| addr.to_string()),
        location,
        browser,
        os_type,
        login_time: now,
        expire_at,
    };

    if let Err(e) = OnlineSessionModel::record_login(&session, ttl).await {
        tracing::warn!(error = %e, "登记在线会话失败（不影响登录结果）");
    }
}

/// 移除在线会话（登出时调用）。
///
/// # 参数
///
/// - `claims`：当前访问令牌声明。
async fn revoke_online_session(claims: &TokenClaims) {
    if let Some(token_id) = claims.jti.as_deref() {
        OnlineSessionModel::record_logout(token_id).await;
    }
}

/// 登记 refresh 令牌（登录 / 轮换成功后调用，CODE-16 B7）。
///
/// # 参数
///
/// - `claims`：本次签发的 refresh 令牌声明（需含 `jti` / `sid` / `exp`）。
///
/// # 说明
///
/// 登记进 `online:refresh:{jti}` 后，`force_logout_user` / `force_logout_all_except`
/// 才能枚举到 refresh 令牌并按 `user_id` 清理——这是「改密 / 停用 / 改角色后旧 refresh
/// 立即失效」的底座。登记失败只记 `error` 日志：此时缓存本身已不可用，认证层会整体
/// fail-closed，不存在「静默放行」的窗口。
async fn register_refresh_session(claims: &TokenClaims) {
    let Some(jti) = claims.jti.clone().filter(|v| !v.is_empty()) else {
        tracing::warn!(user_id = %claims.sub, "refresh 令牌缺少 jti，跳过登记");
        return;
    };
    let Some(sid) = claims.extra_str(constants::CLAIM_SESSION_ID) else {
        tracing::warn!(user_id = %claims.sub, "refresh 令牌缺少 sid，跳过登记");
        return;
    };
    let session = RefreshSession {
        jti,
        user_id: claims.sub.clone(),
        sid,
        exp: claims.exp as i64,
    };
    let ttl = (session.exp - now_secs()).max(1) as u64;
    if let Err(e) = OnlineSessionModel::record_refresh(&session, ttl).await {
        tracing::error!(
            user_id = %claims.sub,
            error = %e,
            "登记 refresh 令牌失败（吊销将退回依赖令牌版本号与黑名单）"
        );
    }
}

/// 把扁平菜单列表组装为树。
///
/// # 参数
///
/// - `nodes`：扁平菜单投影列表（`MenuRes`）。
///
/// # 返回值
///
/// 根节点列表（已递归填充 `children`）。
fn build_menu_tree(nodes: Vec<MenuRes>) -> Vec<MenuRes> {
    utils::build_tree(
        nodes,
        |n| n.id.clone(),
        |n| n.parent_id.clone(),
        |n| n.sort_order,
        |n, children| n.children = children,
    )
}

/// 登录接口限流检查（按「用户名 + IP」维度）。
///
/// # 参数
///
/// - `username`：登录账号。
/// - `ip`：客户端 IP 文本（解析失败时为 `unknown`）。
///
/// # 返回值
///
/// 未超限返回 `Ok(())`。
///
/// # 异常情况
///
/// 超过窗口内最大尝试次数返回 429。缓存不可用时不限流（放行）。
async fn check_login_rate(username: &str, ip: &str) -> std::result::Result<(), ApiError> {
    let Some(store) = try_cache() else {
        return Ok(());
    };
    let key = format!("{}{}:{}", constants::CACHE_KEY_PREFIX_LOGIN, username, ip);
    let count = match Cache::incr(store, &key, 1).await {
        Ok(count) => count,
        Err(e) => {
            tracing::warn!(error = %e, "登录限流计数失败，本次放行");
            return Ok(());
        }
    };
    if count == 1
        && let Err(e) = Cache::set_ex(store, &key, &1i64, constants::LOGIN_RATE_WINDOW_SECS).await
    {
        tracing::warn!(error = %e, "设置登录限流窗口失败");
    }
    if count > constants::LOGIN_RATE_MAX_ATTEMPTS {
        return Err(ApiError::new(
            429,
            error_code::TOO_MANY_REQUESTS,
            "登录尝试过于频繁，请稍后再试",
        ));
    }
    Ok(())
}

/// 判断账号锁定是否已到期（到期视为可自动解锁）。
///
/// # 参数
///
/// - `user`：用户模型。
/// - `lock_minutes`：锁定后自动解锁的等待时长（分钟），取自 [`model::login_security_policy`]。
///
/// # 返回值
///
/// 处于锁定期内返回 `true`；未锁定或已过期返回 `false`。
fn is_locked_now(user: &SysUserModel, lock_minutes: i64) -> bool {
    if !user.is_locked {
        return false;
    }
    match user.lock_time {
        Some(lock_time) => chrono::Utc::now() < lock_time + chrono::Duration::minutes(lock_minutes),
        None => true,
    }
}

/// 记录一次登录失败：认证日志（`auth_logs` 失败行）+ 安全日志（`security_logs`）。
///
/// # 参数
///
/// - `headers`：请求头（派生 request_id / User-Agent）。
/// - `connect_info`：TCP 连接对端地址（解析真实客户端 IP）。
/// - `username`：尝试登录的账号。
/// - `user_id`：命中的用户 ID（用户不存在时为 `None`）。
/// - `reason`：失败原因（落 `auth_logs.error_message` 与安全日志详情）。
/// - `timer`：耗时分段采集器（落库前输出分段耗时，失败路径同样可定位慢点）。
///
/// # 说明
///
/// 投递本身无 I/O，同步返回；落库由日志总线后台批量完成。
fn log_login_failure(
    headers: &HeaderMap,
    connect_info: &SocketAddr,
    username: &str,
    user_id: Option<uuid::Uuid>,
    reason: &str,
    timer: &LoginTimer,
) {
    log::record_auth(
        headers,
        connect_info,
        AuthLogInput {
            user_id,
            username: Some(username.to_string()),
            auth_type: AuthType::Login,
            auth_status: AuthStatus::Fail,
            error_message: Some(reason.to_string()),
            login_duration: Some(timer.total_ms()),
        },
    );
    timer.report();
    log::record_security(
        headers,
        connect_info,
        SecurityLogInput {
            user_id,
            username: Some(username.to_string()),
            event_type: "LOGIN_FAIL".to_string(),
            detail: Some(serde_json::json!({ "reason": reason })),
        },
    );
}

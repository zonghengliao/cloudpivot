//! 认证中间件：JWT Bearer Token 验证 + 黑名单检查

use super::{AuthClaims, TokenClaims, TokenType, UserId};
use crate::response::{Res, codes};
use axum::http::StatusCode;
use axum::{body::Body, extract::Request, response::Response};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// JWT 认证中间件
///
/// 从 `Authorization: Bearer <token>` 中提取并验证 JWT，
/// 将解析出的 `UserContext` 挂载到 `request.extensions` 中。
/// 失败返回 401 Unauthorized。
#[derive(Clone)]
pub struct AuthLayer {
    /// JWT 密钥（HS256）
    pub jwt_secret: String,
    /// 跳过认证的路径列表（如 `/public/*`、`/api/login`）
    pub ignore_paths: Vec<String>,
    /// 缓存层引用（用于缓存用户信息）
    #[cfg(feature = "cache")]
    pub cache: Option<nik_cache::SharedCache>,
    /// [cloudpivot-patch code-10] 允许用 `?token=` 认证的路径白名单（含全局前缀）。
    ///
    /// 只给浏览器 `EventSource` 这类无法设置自定义请求头的端点用（当前仅 SSE）。
    /// 空列表 = 完全不接受查询参数令牌（fail-closed，与打补丁前行为一致）。
    pub query_token_paths: Vec<String>,
}

impl<S> Layer<S> for AuthLayer {
    type Service = AuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            jwt_secret: self.jwt_secret.clone(),
            ignore_paths: self.ignore_paths.iter().cloned().collect(),
            #[cfg(feature = "cache")]
            cache: self.cache.clone(),
            // [cloudpivot-patch code-10] svc
            query_token_paths: self.query_token_paths.iter().cloned().collect(),
        }
    }
}

#[derive(Clone)]
/// JWT 认证服务——验证 Bearer Token 并注入用户上下文到请求 extensions
pub struct AuthService<S> {
    /// 下游服务
    inner: S,
    /// JWT 密钥（HS256）
    jwt_secret: String,
    /// 跳过认证的路径集合
    ignore_paths: HashSet<String>,
    /// 缓存层引用（用于缓存用户信息）
    #[cfg(feature = "cache")]
    cache: Option<nik_cache::SharedCache>,
    /// [cloudpivot-patch code-10] 允许用 `?token=` 认证的路径白名单（与层共享）
    query_token_paths: HashSet<String>,
}

impl<S> AuthService<S> {
    /// [cloudpivot-patch code-10] 从查询串取令牌 —— **只**认白名单路径上的 `token` 参数。
    ///
    /// 安全边界（与业务侧文档 `docs/批次A-SSE鉴权口径.md` 一一对应）：
    /// * 仅白名单路径（默认空 → 此函数永远 `None`）；
    /// * 只读 `token`，**绝不**接受 `user_id` / `receiver_id` 之类「客户端指定身份」的参数；
    /// * 拒绝含 `%` 的取值（JWT 字符集不需要百分号编码，出现即视为异常输入）；
    /// * 令牌只用于校验，不写日志、不进事件（日志只记 header|query 来源）。
    fn query_token_of(&self, path: &str, query: Option<&str>) -> Option<String> {
        if !self
            .query_token_paths
            .iter()
            .any(|allowed| allowed.trim_end_matches('/') == path)
        {
            return None;
        }
        let query = query?;
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("token=") {
                let value = value.trim();
                if !value.is_empty() && !value.contains('%') {
                    return Some(value.to_string());
                }
            }
        }
        None
    }
}

impl<S> Service<Request<Body>> for AuthService<S>
where
    S: Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<Body>) -> Self::Future {
        // CORS 预检请求（OPTIONS）不携带凭据，跳过认证直接放行。
        // 浏览器发送跨域请求（带 Authorization 头）前会先发 OPTIONS 预检，
        // 若被认证中间件拦截返回 401，会导致浏览器阻止实际请求（ERR_FAILED）。
        // /auth/login 等公开路由的 OPTIONS 碰巧通过（在 ignore_paths 中），
        // 但 /auth/me、/auth/menus 等受保护路由的预检会被拦截。
        if req.method() == axum::http::Method::OPTIONS {
            let mut inner = self.inner.clone();
            return Box::pin(async move { inner.call(req).await });
        }

        let path = req.uri().path().to_string();
        // 检查路径是否在忽略列表中：
        // 1. 精确匹配
        // 2. 前缀匹配（如 /api 匹配 /api/xxx，/api/files/dl 匹配 /api/files/dl/xxx）
        // [cloudpivot-patch code-11] 忽略路径判定支持模板段（`{param}` / `*`）
        let is_ignore_path = is_ignored_path(self.ignore_paths.iter(), &path);

        // [cloudpivot-patch code-10] 令牌来源：**优先** `Authorization: Bearer`；
        // 仅当没有头、且路径在白名单时，才回落到 `?token=`（浏览器 EventSource 用）。
        // 两条来源后续走完全相同的校验路径（签名 / 过期 / 类型 / 黑名单），
        // 身份只来自令牌声明 —— 查询串里任何「身份类」参数一律不予采信。
        let header_token: Option<&str> = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let query_token: Option<String> = if header_token.is_some() {
            None
        } else {
            self.query_token_of(&path, req.uri().query())
        };
        let token_opt: Option<&str> = header_token.or(query_token.as_deref());

        match token_opt {
            Some(token) => match validate_and_extract_claims(&self.jwt_secret, token) {
                Ok(claims) => {
                    #[cfg(feature = "cache")]
                    let cache = self.cache.clone();
                    let mut inner = self.inner.clone();
                    #[allow(unused_variables)]
                    let is_ignore = is_ignore_path;
                    Box::pin(async move {
                        #[cfg(feature = "cache")]
                        {
                            // 黑名单检查：仅在非 ignore_path 时拒绝，ignore_path 应放行
                            if !is_ignore && let (Some(c), Some(jti)) = (&cache, &claims.jti) {
                                let key = format!("token:blacklist:{}", jti);
                                if let Ok(Some(_)) =
                                    nik_cache::Cache::get::<serde_json::Value>(c, &key).await
                                {
                                    let body = serde_json::to_string(&Res::<()>::fail(
                                        codes::UNAUTHORIZED,
                                        "Token 已登出，请重新登录",
                                    ))
                                    .unwrap_or_else(|_| {
                                        r#"{"code":401,"msg":"Token 已登出，请重新登录"}"#
                                            .to_string()
                                    });
                                    return Ok(Response::builder()
                                        .status(StatusCode::UNAUTHORIZED)
                                        .header("Content-Type", "application/json; charset=utf-8")
                                        .body(Body::from(body))
                                        .expect("response body build failed"));
                                }
                            }
                        }
                        // 有有效 Token，注入用户信息（ignore_path 也注入，以便业务可以获取用户信息）
                        req.extensions_mut().insert(UserId(claims.sub.clone()));
                        req.extensions_mut().insert(AuthClaims(claims.clone()));
                        let mut response = inner.call(req).await?;
                        response.extensions_mut().insert(AuthClaims(claims));
                        Ok(response)
                    })
                }
                Err(_) => {
                    if is_ignore_path {
                        // ignore_path 上的无效 Token，忽略错误继续处理，不注入用户信息
                        let mut inner = self.inner.clone();
                        Box::pin(async move { inner.call(req).await })
                    } else {
                        let body = serde_json::to_string(&Res::<()>::fail(
                            codes::UNAUTHORIZED,
                            "Token 无效或已过期",
                        ))
                        .unwrap_or_else(|_| {
                            r#"{"code":401,"msg":"Token 无效或已过期"}"#.to_string()
                        });
                        Box::pin(async move {
                            Ok(Response::builder()
                                .status(StatusCode::UNAUTHORIZED)
                                .header("Content-Type", "application/json; charset=utf-8")
                                .body(Body::from(body))
                                .expect("response body build failed"))
                        })
                    }
                }
            },
            None => {
                if is_ignore_path {
                    // ignore_path 上没有 Token，直接放行
                    let mut inner = self.inner.clone();
                    Box::pin(async move { inner.call(req).await })
                } else {
                    let body = serde_json::to_string(&Res::<()>::fail(
                        codes::UNAUTHORIZED,
                        "未授权访问，请先登录",
                    ))
                    .unwrap_or_else(|_| r#"{"code":401,"msg":"未授权访问，请先登录"}"#.to_string());
                    Box::pin(async move {
                        Ok(Response::builder()
                            .status(StatusCode::UNAUTHORIZED)
                            .header("Content-Type", "application/json; charset=utf-8")
                            .body(Body::from(body))
                            .expect("response body build failed"))
                    })
                }
            }
        }
    }
}

/// [cloudpivot-patch code-11] 路径是否命中忽略列表（**精确 / 前缀 / 模板段**）。
///
/// 语义：
/// 1. 完全相等 → 命中（原行为）；
/// 2. 作为前缀命中（`/api/files/dl` 命中 `/api/files/dl/xxx`，原行为）；
/// 3. **逐段比对**：忽略项里的一段写成 `{param}` 或 `*` 时匹配任意单段，
///    段数必须相同。于是 `/api/business/task/{id}/comment` 能命中
///    `/api/business/task/0199…/comment`，但**不会**命中
///    `/api/business/task/0199…/comment/extra`（段数不同）。
///
/// 只用于「免认证路径」这一处判定；权限规则匹配仍由 `permission_check::match_path`
/// 负责（那边有静态段优先的打分，语义更强）。两处共用本函数保证「认证放行的路径」
/// 与「权限层跳过的路径」永远是同一批 —— 否则「认证过了但权限层 403」会变成
/// 一类只在特定路径上出现的怪问题。
/// [cloudpivot-patch code-11b] 模板段是否「像 id」。
///
/// 只接受三种形态：UUID（36 位十六进制 + 短横）、纯数字、16–64 位不透明串。
/// **不接受任意单段**：`/api/business/task/{id}` 是把「任务详情」声明成免认证的模板，
/// 若 `{id}` 能匹配 `page` / `my` / `gantt` / `stats`，那些静态端点会连认证层一起跳过，
/// 401 变 500，且将来加在同前缀下的公开端点会静默失去鉴权（生产实测踩过）。
/// 新增免认证模板且参数不是 id 形态时，必须在这里显式扩展规则，而不是放宽成 `*`。
fn wildcard_segment_matches(got: &str) -> bool {
    if got.is_empty() {
        return false;
    }
    let uuid_like =
        got.len() == 36 && got.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let digits_only = got.chars().all(|c| c.is_ascii_digit());
    let opaque = (16..=64).contains(&got.len())
        && got
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    uuid_like || digits_only || opaque
}

pub(crate) fn is_ignored_path<'a, I>(paths: I, path: &str) -> bool
where
    I: IntoIterator<Item = &'a String>,
{
    paths.into_iter().any(|pattern| {
        let trimmed = pattern.trim_end_matches('/');
        if path == trimmed || path.starts_with(&format!("{trimmed}/")) {
            return true;
        }
        let mut want = trimmed.split('/');
        let mut got = path.split('/');
        loop {
            match (want.next(), got.next()) {
                (None, None) => return true,
                (Some(w), Some(g)) => {
                    let wildcard = (w.starts_with('{') && w.ends_with('}')) || w == "*";
                    if wildcard {
                        if !wildcard_segment_matches(g) {
                            return false;
                        }
                    } else if w != g {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    })
}

/// 校验 JWT 并提取声明 —— **认证的唯一真值实现**。
///
/// `AuthLayer` 与 `AuthClaims` 的提取器（`FromRequestParts` 通道 2）都调用它，
/// 因此两者对「Token 是否有效」的判定永远一致。新增校验规则（如受众 `aud`、
/// 签发方 `iss`、时钟偏移）只需改这里，两处自动同步。
///
/// # 错误
///
/// 返回 `Err` 的原因字符串仅用于日志/调试；对外一律映射为 401，不泄漏细节。
pub(crate) fn validate_and_extract_claims(
    secret: &str,
    token: &str,
) -> Result<TokenClaims, String> {
    use jsonwebtoken::{DecodingKey, Validation, decode};

    let token_data = decode::<TokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|e| format!("Token 验证失败: {}", e))?;

    // 安全修复 P1-3：拒绝 Refresh Token 被当作 Access Token 使用。
    // Refresh Token 有效期更长且携带空权限数组，若被接受为 Access Token，
    // 会延长攻击者对仅按 claims.sub 过滤的用户端点的访问窗口。
    if token_data.claims.token_type != Some(TokenType::Access) {
        return Err("Token 类型不正确，需要 Access Token".into());
    }

    Ok(token_data.claims)
}

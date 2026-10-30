//! 路径权限校验中间件（配置 + 宏注解联合校验）

use super::AuthClaims;
use crate::response::Res;
use axum::http::StatusCode;
use axum::{body::Body, extract::Request, response::Response};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// 将路径按 `/` 分段，逐段比对。`{param}` 占位符匹配任意非空段。
fn match_path(request_path: &str, rule_path: &str) -> bool {
    if request_path == rule_path {
        return true;
    }
    let req_segments: Vec<&str> = request_path.trim_matches('/').split('/').collect();
    let rule_segments: Vec<&str> = rule_path.trim_matches('/').split('/').collect();
    if req_segments.len() != rule_segments.len() {
        return false;
    }
    for (req, rule) in req_segments.iter().zip(rule_segments.iter()) {
        if rule.starts_with('{') && rule.ends_with('}') {
            continue;
        }
        if req != rule {
            return false;
        }
    }
    true
}

/// 把路径里的参数段归一化成 `{}`（`/x/{id}` 与 `/x/{uuid}` 视为同一模式）。
///
/// [cloudpivot-patch code-09b] 冲突自检要比较的是「路径模式是否相同」，
/// 参数名不同（`{id}` / `{uuid}`）的同一模式同样会互相遮蔽，必须视为同一路径。
fn normalized_pattern(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .map(|seg| {
            if seg.starts_with('{') && seg.ends_with('}') {
                "{}"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 路径级权限校验中间件（配置 + 宏注解联合校验）
///
/// 合并 `config.toml` 中 `[middleware.permission.rules]` 的静态规则
/// 和 `#[permission]` 宏注解的编译期规则。
#[derive(Clone)]
pub struct PermissionCheckLayer {
    /// 权限规则列表
    ///
    /// 以 `Arc` 共享：`Layer::layer()` 会为**每条路由**生成一个服务实例
    /// （且在生成前 axum 还会按路由克隆一次 layer），若每次直接克隆 `Vec`，
    /// 装配成本为 `O(路由数 × 规则数)` 的深克隆（每条规则含多个 `String`）。
    /// 实测在 ~200 路由 × ~200 规则时占启动期中间件装配的 90% 以上
    /// （155–213ms / 171–232ms，debug）；共享后每次生成服务只做引用计数递增。
    pub rules: Arc<Vec<PermissionRule>>,
    /// [cailu-patch code-09] 免认证路径（`#[nik::no_auth]` / 配置 / 插件三个来源，含全局前缀）。
    ///
    /// 这些路径上认证层放行且**不注入** `AuthClaims`；若仍参与权限规则匹配，
    /// 就会被判成「无权限」——正是 `GET /api/support/config/public` 被
    /// `GET /support/config/{id}` 规则拦成 403 的根因。
    pub ignore_paths: Arc<Vec<String>>,
}

/// 权限匹配规则
#[derive(Debug, Clone)]
pub struct PermissionRule {
    /// URL 路径（精确或前缀匹配）
    pub path: String,
    /// 适用的 HTTP 方法列表（空表示所有方法）
    pub methods: Vec<String>,
    /// 所需权限标识
    pub permission: String,
}

impl PermissionCheckLayer {
    /// 从配置文件规则创建
    pub fn from_config(config_rules: &[nik_config::PermissionRule]) -> Self {
        Self {
            rules: Arc::new(
                config_rules
                    .iter()
                    .map(|r| PermissionRule {
                        path: r.path.clone(),
                        methods: r.methods.clone(),
                        permission: r.permission.clone(),
                    })
                    .collect(),
            ),
            ignore_paths: Arc::new(Vec::new()),
        }
    }

    /// [cailu-patch code-09] 注入免认证路径（装配期调用一次）。
    pub fn with_ignore_paths(mut self, paths: Vec<String>) -> Self {
        self.ignore_paths = Arc::new(paths);
        self
    }

    /// [cailu-patch code-09] 该请求路径是否免认证（精确命中或作为前缀命中）。
    /// [cloudpivot-patch code-11] 增加模板段支持（`{param}` / `*`），实现与认证层同源。
    pub fn is_no_auth_path(&self, request_path: &str) -> bool {
        super::auth::is_ignored_path(self.ignore_paths.iter(), request_path)
    }

    /// [cloudpivot-patch code-09b] 规则冲突自检：返回「同一路径被声明了不同权限」的描述。
    ///
    /// 装配期调用并 `warn!` 出来，避免同类问题再次静默上线（CI 里可当 error）。
    ///
    /// 判定口径（09b 收紧，理由见下）：
    /// * **兄弟路由不算冲突**：`/x/page`（静态）与 `/x/{id}`（模板）天然会同时匹配到
    ///   `/x/page`，但匹配器已按「完全相等(0) > 纯静态(1) > 含参数(2)」打分（CODE-09），
    ///   静态段永远胜出，两者权限谁更严都不会串味。把这种成对路由都报成冲突，只会在
    ///   启动日志里刷几十条「请修正规则」的噪音（实测 22 条，全部是正常 REST 写法）。
    /// * **真正的冲突**是「同一路径模式被两条规则要求了不同权限」：此时最终用哪条权限
    ///   取决于注册顺序，属确定性缺陷 —— 这正是本自检要抓的东西。
    pub fn detect_conflicts(&self) -> Vec<String> {
        let mut conflicts = Vec::new();
        for (index, left) in self.rules.iter().enumerate() {
            for right in self.rules.iter().skip(index + 1) {
                if left.permission == right.permission {
                    continue;
                }
                let methods_overlap = left.methods.is_empty()
                    || right.methods.is_empty()
                    || left
                        .methods
                        .iter()
                        .any(|m| right.methods.iter().any(|p| p.eq_ignore_ascii_case(m)));
                if methods_overlap
                    && normalized_pattern(&left.path) == normalized_pattern(&right.path)
                {
                    conflicts.push(format!(
                        "同一路径被声明了不同权限：{} ({}) 与 {} ({})",
                        left.path, left.permission, right.path, right.permission
                    ));
                }
            }
        }
        conflicts
    }

    /// 合并 `#[permission]` 宏注解的编译期规则
    pub fn with_macro_rules(mut self, rules: &[crate::PermissionDef]) -> Self {
        let list = Arc::make_mut(&mut self.rules);
        for def in rules {
            list.push(PermissionRule {
                path: def.path.to_string(),
                methods: vec![def.method.to_string()],
                permission: def.permission.to_string(),
            });
        }
        self
    }

    /// 追加一条规则（装配阶段使用；`Arc` 共享下按写时复制处理）
    pub fn push_rule(&mut self, rule: PermissionRule) {
        Arc::make_mut(&mut self.rules).push(rule);
    }

    /// 是否有权限规则（无规则时中间件不生效）
    pub fn has_rules(&self) -> bool {
        !self.rules.is_empty()
    }
}

impl<S> Layer<S> for PermissionCheckLayer {
    type Service = PermissionCheckService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PermissionCheckService {
            inner,
            rules: Arc::clone(&self.rules),
            // [cailu-patch code-09] svc
            ignore_paths: Arc::clone(&self.ignore_paths),
        }
    }
}

#[derive(Clone)]
/// 路径权限校验服务——基于请求路径和方法匹配权限规则
pub struct PermissionCheckService<S> {
    /// 下游服务
    inner: S,
    /// 权限规则列表（与层共享，装配时零深克隆）
    rules: Arc<Vec<PermissionRule>>,
    /// [cailu-patch code-09] 免认证路径（与层共享）
    ignore_paths: Arc<Vec<String>>,
}

impl<S> Service<Request<Body>> for PermissionCheckService<S>
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

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let path = req.uri().path().to_string();
        let method = req.method().to_string();

        // [cailu-patch code-09] 免认证路径直接放行：认证层在这些路径上不注入
        // AuthClaims，若继续走规则匹配必然被判 403（`/support/config/public` 即此例）。
        // [cloudpivot-patch code-11] 与认证层同一套判定（支持模板段）
        if super::auth::is_ignored_path(self.ignore_paths.iter(), &path) {
            tracing::debug!("权限检查: path={} 属于免认证路径，跳过权限规则", path);
            let mut inner = self.inner.clone();
            return Box::pin(async move { inner.call(req).await });
        }

        // [cailu-patch code-09] 匹配优先级：完全相等(0) > 纯静态段(1) > 含 {param}(2)。
        // 原实现取「第一条命中」，语义取决于注册顺序，静态子路径会被模板规则遮蔽。
        let mut best: Option<(&PermissionRule, u8)> = None;
        for rule in self.rules.iter() {
            let path_matches = match_path(&path, &rule.path);
            let method_matches = rule.methods.is_empty()
                || rule.methods.iter().any(|m| m.eq_ignore_ascii_case(&method));
            if !path_matches || !method_matches {
                continue;
            }
            let score = if rule.path == path {
                0
            } else if !rule.path.contains('{') {
                1
            } else {
                2
            };
            match best {
                Some((_, best_score)) if best_score <= score => {}
                _ => best = Some((rule, score)),
            }
        }

        match best.map(|(rule, _)| rule) {
            Some(rule) => {
                let required_perm = rule.permission.clone();
                let claims_present = req.extensions().get::<AuthClaims>().cloned();

                let has_perm = match &claims_present {
                    Some(AuthClaims(claims)) => {
                        tracing::debug!(
                            "权限检查: path={}, required={}, user_perms={:?}",
                            path,
                            required_perm,
                            claims.permissions
                        );
                        claims.has_permission(&required_perm)
                    }
                    None => {
                        tracing::warn!(
                            "权限检查: path={}, required={}, AuthClaims 未注入 (auth 中间件未执行或配置有误)",
                            path,
                            required_perm
                        );
                        false
                    }
                };

                if has_perm {
                    let mut inner = self.inner.clone();
                    Box::pin(async move { inner.call(req).await })
                } else {
                    let body = serde_json::to_string(&Res::<()>::fail(
                        StatusCode::FORBIDDEN.as_u16() as i32,
                        format!("无权限访问，需要权限: {}", required_perm),
                    ))
                    .unwrap_or_else(|_| {
                        r#"{"code":403,"msg":"无权限访问，需要权限: xxx"}"#.to_string()
                    });
                    let resp = Response::builder()
                        .status(StatusCode::FORBIDDEN)
                        .header("Content-Type", "application/json; charset=utf-8")
                        .body(Body::from(body))
                        .expect("response body build failed");
                    Box::pin(async move { Ok(resp) })
                }
            }
            None => {
                let mut inner = self.inner.clone();
                Box::pin(async move { inner.call(req).await })
            }
        }
    }
}

//! 任务级短期令牌中间件（T5b · 设计修正 3 的硬边界）。
//!
//! # 为什么必须是**独立**中间件，而不是复用 JWT 的 `AuthMiddleware`
//!
//! AI 员工没有网页会话（`user_type = 'ROBOT'` 被 `63201` 硬拒登录），它能拿到的
//! 只有云枢派活时写进 kanban body 的一把**任务级令牌**。若把这把令牌做成 JWT，
//! 它就会走框架那条「验签 → 注入全量 `AuthClaims` → 权限层按码放行」的通路：
//! 一旦某个端点只要求 `business:task:read`，这把令牌就顺带能读**全部**任务 ——
//! 那就不是「任务级」了。所以本中间件：
//!
//! 1. **独立校验**：令牌是 32 字节随机数的十六进制串（前缀 `cpai_`），库内只存
//!    `sha256`，**不参与任何 JWT 验签**；
//! 2. **白名单**：只认 4 个端点（详情 / 评论 / 附件 / AI 进度），其它端点带着
//!    这把令牌来一律 `403 + 63304`（**不回落**成 JWT 语义，也不放行）；
//! 3. **task_id 绑定**：路径里的 `{id}` 必须与令牌登记的任务一致，否则 `403 + 63303`；
//! 4. **不做 scope 检查**：边界已经由「单任务」表达，再叠数据范围只会让 AI 连自己
//!    负责的任务都读不到（owner 就是它自己，范围过滤本来也不该在这里出现）。
//!
//! # 与框架认证层的关系（实现细节，评审会问）
//!
//! 4 个端点在路由上标了 `#[nik::no_auth("/business/task/{id}/…")]`：
//!
//! - **认证层**：忽略列表命中的路径遇到非法/缺失令牌时**继续放行**、遇到合法 JWT
//!   仍注入 `AuthClaims`（`nik-web` `middleware/auth.rs` 的行为）。因此人类用 JWT
//!   调同一批端点时，身份照旧由框架注入，本中间件不做任何事（分支见 [`guard`]）；
//! - **权限层**：同样跳过忽略列表路径（`cloudpivot-patch code-09`），所以这些端点的
//!   人类侧权限**必须在 handler 里显式判**（`handler::ensure_task_read_permission`），
//!   这一点在 `task/handler.rs` 的文档里逐条写明。
//!
//! # 挂载
//!
//! [`guard`] 由 `crate::run` 用 `App::layer_from_fn` 挂在**最外层**（框架认证层之外）。
//! 这一位置是刻意的：AI 的令牌不是 JWT，若让它先过框架认证层，任何非白名单端点都会
//! 先得到 `401 Token 无效或已过期`（而不是本中间件的 `403 + 63304`），
//! 「白名单外端点 404/403」这条验收就永远测不到。放在最外层后：
//!
//! - 带 `cpai_…` 的请求**只**由本中间件判定（白名单 + task_id 绑定 + 有效期）；
//! - 人类 JWT 请求原样透传（本层只看头前缀，不做任何 JWT 逻辑）。
//!
//! # 失效条件（逐请求判定，任一命中即 403）
//!
//! | 条件 | 错误码 |
//! |------|--------|
//! | 令牌不存在（hash 无命中） | `63301` |
//! | 已过期（`expires_at <= now()`） | `63302` |
//! | 路径任务 ≠ 令牌任务 | `63303` |
//! | 端点不在白名单 / 方法不符 | `63304` |
//! | 任务已 `DONE` / `CANCELLED` / 已软删 / `exec_mode <> 'AI'` | `63301`（令牌随任务终结） |

use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use nik::prelude::*;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::shared::error_code;

/// 任务级令牌的明文前缀。
///
/// 有前缀才能让本中间件「只看头就分流」：不是这个前缀的一律当人类 JWT，交给框架
/// 认证层处理（人类令牌是 `eyJ…` 的 JWT，绝不会误判）。
pub const TOKEN_PREFIX: &str = "cpai_";

/// 令牌随机部分的十六进制长度（32 字节 = 64 hex 字符）。
pub const TOKEN_HEX_LEN: usize = 64;

/// 令牌覆盖的端点（白名单，共 4 个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiTarget {
    /// `GET /business/task/{id}` —— 只读本任务。
    Detail,
    /// `POST /business/task/{id}/comment` —— 只能评本任务。
    Comment,
    /// `POST /business/task/{id}/attachment` —— 只能传本任务（`category='task_ai'`）。
    Attachment,
    /// `PATCH /business/task/{id}/ai-progress` —— 只能改进度。
    AiProgress,
}

impl AiTarget {
    /// 端点动作名（log / 错误文案用）。
    pub fn action(self) -> &'static str {
        match self {
            Self::Detail => "查看任务详情",
            Self::Comment => "写任务评论",
            Self::Attachment => "上传任务附件",
            Self::AiProgress => "回写任务进度",
        }
    }
}

/// 通过任务令牌认证后的执行主体（注入到请求 extensions）。
///
/// handler 用 `Option<Extension<AiTaskActor>>` 取它：有值 = 令牌路径（单任务语义），
/// 无值 = 人类 JWT 路径。两种身份**不会同时存在**（[`guard`] 在令牌路径上会摘掉
/// `Authorization` 头，框架认证层因此不会注入 `AuthClaims`）。
#[derive(Debug, Clone)]
pub struct AiTaskActor {
    /// 令牌登记的任务（= 路径里的 `{id}`，两者已比对一致）。
    pub task_id: Uuid,
    /// 该任务的负责人（`user_type='ROBOT'` 的 AI 账号）—— 评论 / 附件的作者。
    pub user_id: Uuid,
    /// AI 员工的 Hermes profile（`sys_ai_agent.hermes_profile`，可能为空）。
    pub profile: Option<String>,
    /// 本次命中的端点动作。
    pub target: AiTarget,
}

/// 判断一个 `Authorization: Bearer` 取值是不是任务级令牌。
pub fn is_ai_token(raw: &str) -> bool {
    raw.len() == TOKEN_PREFIX.len() + TOKEN_HEX_LEN
        && raw.starts_with(TOKEN_PREFIX)
        && raw[TOKEN_PREFIX.len()..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// 生成一把新的任务级令牌，返回 `(明文, sha256 十六进制)`。
///
/// # 随机源
///
/// `uuid::Uuid::new_v4()`（操作系统 CSPRNG）拼两段共 64 hex 字符 = 32 字节熵，
/// 与 `bootstrap::generate_password` 同源，不引入 `rand` 依赖。
///
/// # 约定（红线）
///
/// **明文只允许出现在派活时写进 kanban body 的文本里**，任何日志 / 评论 / 库列都
/// 只能出现 hash（[`hash_token`] 的返回值）。
pub fn new_token() -> (String, String) {
    let mut raw = String::with_capacity(TOKEN_PREFIX.len() + TOKEN_HEX_LEN);
    raw.push_str(TOKEN_PREFIX);
    while raw.len() < TOKEN_PREFIX.len() + TOKEN_HEX_LEN {
        raw.push_str(&Uuid::new_v4().simple().to_string());
    }
    raw.truncate(TOKEN_PREFIX.len() + TOKEN_HEX_LEN);
    let hash = hash_token(&raw);
    (raw, hash)
}

/// `sha256(明文)` 的小写十六进制，即入库值与查询值。
pub fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 从请求路径里认出「本中间件管的端点」。
///
/// # 匹配口径
///
/// 只按**尾部段**匹配（`business` / `task` 相邻），因此 `[router] prefix` 是 `/api`
/// 还是别的值都不影响判定。段数不符一律拒绝 —— 这条规则让
/// `/business/task/{id}/status`、`/business/task/page`、`/business/task/{id}/members`
/// 这些同前缀端点**无法**被令牌命中（白名单只有 4 个）。
///
/// # 返回值
///
/// 命中返回 `(路径里的任务 id 原文, 端点动作)`；不命中返回 `None`。
pub fn classify(method: &str, path: &str) -> Option<(String, AiTarget)> {
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let len = segments.len();
    // 尾部 4 段：[…, business, task, {id}, <action>]
    if len >= 4 && segments[len - 4] == "business" && segments[len - 3] == "task" {
        let id = segments[len - 2];
        let action = segments[len - 1];
        return match (method, action) {
            ("POST", "comment") => Some((id.to_string(), AiTarget::Comment)),
            ("POST", "attachment") => Some((id.to_string(), AiTarget::Attachment)),
            ("PATCH", "ai-progress") => Some((id.to_string(), AiTarget::AiProgress)),
            _ => None,
        };
    }
    // 尾部 3 段：[…, business, task, {id}]，只认 GET。
    // `{id}` **必须真的是 UUID**：否则 `/business/task/page`、`/my`、`/gantt` 这类
    // 同前缀的静态端点会被误判成「详情」，令牌就顺带能读列表了。
    if len >= 3
        && segments[len - 3] == "business"
        && segments[len - 2] == "task"
        && method == "GET"
        && Uuid::parse_str(segments[len - 1]).is_ok()
    {
        return Some((segments[len - 1].to_string(), AiTarget::Detail));
    }
    None
}

/// 令牌是否已过期（`expires_at <= now()` 即过期）。
///
/// 抽成纯函数是为了单测：时间比较写错方向（`<` / `<=`）会让「刚好到点」的令牌
/// 多活一个轮询周期，这种错误在 HTTP 层几乎测不出来。
pub fn is_expired(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    expires_at <= now
}

/// 任务是否已终结（令牌随之失效）。
///
/// `DONE` / `CANCELLED` 之后 AI 不该再写评论或传附件；软删（`deleted_at` 非空）同理。
pub fn task_closed(status: &str, deleted: bool) -> bool {
    deleted || matches!(status, "DONE" | "CANCELLED")
}

/// 中间件本体（`axum::middleware::from_fn` 包装后交给 `App::route_layer`）。
pub async fn guard(request: Request, next: Next) -> Response {
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }
    let header_token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| value.trim().to_string());
    let Some(raw) = header_token else {
        // 没有 Authorization：框架认证层已经给出 401（这 4 个端点虽是 no_auth，
        // 但 handler 会显式要求身份），这里不做二次判定。
        return next.run(request).await;
    };
    if !is_ai_token(&raw) {
        // 人类 JWT：身份由框架认证层注入，本中间件完全不介入
        return next.run(request).await;
    }

    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let Some((raw_task_id, target)) = classify(&method, &path) else {
        tracing::warn!(%method, %path, "任务令牌访问白名单外的端点，已拒绝");
        return fail(
            403,
            error_code::TASK_AI_ENDPOINT_FORBIDDEN,
            "任务令牌只能访问本任务的详情 / 评论 / 附件 / AI 进度四个端点",
        );
    };
    let Ok(path_task_id) = Uuid::parse_str(&raw_task_id) else {
        return fail(
            403,
            error_code::TASK_AI_ENDPOINT_FORBIDDEN,
            "任务令牌只能用于 /business/task/{任务id}/… 形态的端点",
        );
    };

    match authenticate(&raw, path_task_id, target).await {
        Ok(actor) => {
            let mut request = request;
            // 摘掉令牌头：下游（框架认证层已过、handler 内）不应再把这把令牌当凭据用，
            // 也避免它被任何日志 / 审计中间件顺手写出去。
            request.headers_mut().remove(header::AUTHORIZATION);
            request.extensions_mut().insert(actor);
            next.run(request).await
        }
        Err(error) => {
            tracing::warn!(
                task_id = %path_task_id,
                code = error.code,
                msg = %error.msg,
                "任务令牌校验失败"
            );
            fail(error.status, error.code, &error.msg)
        }
    }
}

/// 校验令牌并取出执行主体。
///
/// # 判定顺序（错误码即契约）
///
/// 1. hash 无命中 → `63301`（令牌不存在／已被重派覆盖／任务已删）
/// 2. 路径任务 ≠ 令牌任务 → `63303`（**越权访问别的任务**，验收项 3）
/// 3. 已过期 → `63302`
/// 4. 任务已 `DONE`/`CANCELLED`/软删 或 `exec_mode <> 'AI'` → `63301`
///
/// 顺序有意为之：先确认「这把令牌属于谁」，再谈「还能不能用」。反过来会把
/// 「拿 A 任务的令牌访问 B 任务」报成「令牌过期」，验收脚本就分不清越权与过期。
// SCOPE-EXEMPT: 任务令牌校验按 sha256 精确命中单行再回查该任务自身；令牌只可能属于
// 一个任务，不存在「按用户数据范围过滤」的语义（越权由 task_id 比对拦，见 63303）。
async fn authenticate(
    raw: &str,
    path_task_id: Uuid,
    target: AiTarget,
) -> std::result::Result<AiTaskActor, ApiError> {
    let hash = hash_token(raw);
    let rows = db()
        .find_with_params(
            "SELECT t.task_id, t.expires_at, b.status, b.deleted_at, b.owner_id, \
                    b.exec_mode, a.hermes_profile \
               FROM biz_task_ai_token t \
               JOIN biz_task b ON b.id = t.task_id \
               LEFT JOIN sys_ai_agent a ON a.user_id = b.owner_id AND a.deleted_at IS NULL \
              WHERE t.token_hash = ?",
            vec![Value::Text(hash.clone())],
        )
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "任务令牌查询失败（fail-closed）");
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "服务器内部错误".to_string(),
            )
        })?;
    let Some(row) = rows.first() else {
        return Err(token_invalid(
            "任务令牌无效（可能已被重新派活覆盖或任务已删除）",
        ));
    };
    let token_task_id: Uuid = row
        .get_typed("task_id")
        .map_err(|_| token_invalid("任务令牌登记信息损坏"))?;
    if token_task_id != path_task_id {
        return Err(ApiError::new(
            403,
            error_code::TASK_AI_TOKEN_SCOPE,
            "任务令牌只能操作它所属的那一个任务（路径任务与令牌任务不一致）".to_string(),
        ));
    }
    let expires_at: DateTime<Utc> = row
        .get_typed("expires_at")
        .map_err(|_| token_invalid("任务令牌缺少过期时间"))?;
    if is_expired(expires_at, Utc::now()) {
        return Err(ApiError::new(
            403,
            error_code::TASK_AI_TOKEN_EXPIRED,
            format!(
                "任务令牌已过期（有效期至 {}），请让分派人重新派活",
                expires_at.format("%Y-%m-%d %H:%M:%S UTC")
            ),
        ));
    }
    let status: String = row
        .get_typed("status")
        .map_err(|_| token_invalid("任务状态读取失败"))?;
    let deleted_at: Option<DateTime<Utc>> = row.get_typed("deleted_at").unwrap_or(None);
    if task_closed(&status, deleted_at.is_some()) {
        return Err(token_invalid(
            "任务已结束（已完成 / 已取消 / 已删除），令牌已失效",
        ));
    }
    let exec_mode: String = row.get_typed("exec_mode").unwrap_or_default();
    if exec_mode != "AI" {
        return Err(token_invalid("该任务不是 AI 执行模式，任务令牌不可用"));
    }
    let user_id: Uuid = row
        .get_typed("owner_id")
        .map_err(|_| token_invalid("任务负责人读取失败"))?;
    let profile: Option<String> = row.get_typed("hermes_profile").unwrap_or(None);

    // 使用计数只做观测（不限流）：失败不影响本次请求，也不该把正常请求打成 500
    if let Err(error) = db()
        .execute_with_params(
            "UPDATE biz_task_ai_token SET used_count = used_count + 1 WHERE token_hash = ?",
            vec![Value::Text(hash)],
        )
        .await
    {
        tracing::warn!(error = %error, "任务令牌使用计数更新失败（忽略）");
    }

    Ok(AiTaskActor {
        task_id: token_task_id,
        user_id,
        profile,
        target,
    })
}

/// 「令牌不可用」的统一错误（不区分「不存在 / 已终结」，避免探测）。
fn token_invalid(message: &str) -> ApiError {
    ApiError::new(403, error_code::TASK_AI_TOKEN_INVALID, message.to_string())
}

/// 构造与框架同款式的失败响应（`{code, msg}` JSON）。
fn fail(status: u16, code: i32, message: &str) -> Response {
    let body = serde_json::to_string(&Res::<()>::fail(code, message.to_string()))
        .unwrap_or_else(|_| format!(r#"{{"code":{code},"msg":""}}"#));
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN);
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 白名单只有 4 个端点：3 个写 + 1 个读，方法也必须对上。
    #[test]
    fn classify_accepts_only_the_four_whitelisted_endpoints() {
        let id = "0199f0a0-1111-7000-8000-000000000001";
        for (method, path, expected) in [
            (
                "POST",
                format!("/api/business/task/{id}/comment"),
                AiTarget::Comment,
            ),
            (
                "POST",
                format!("/api/business/task/{id}/attachment"),
                AiTarget::Attachment,
            ),
            (
                "PATCH",
                format!("/api/business/task/{id}/ai-progress"),
                AiTarget::AiProgress,
            ),
            ("GET", format!("/api/business/task/{id}"), AiTarget::Detail),
        ] {
            let (got_id, got_target) =
                classify(method, &path).unwrap_or_else(|| panic!("{method} {path} 应当命中白名单"));
            assert_eq!(got_id, id, "路径任务 id 解析不一致");
            assert_eq!(got_target, expected, "{path} 的动作判定不对");
        }
    }

    /// 白名单外一律拒绝：同前缀的兄弟端点、方法不匹配、多一层路径、别的模块。
    #[test]
    fn classify_rejects_everything_else() {
        let id = "0199f0a0-1111-7000-8000-000000000001";
        for (method, path) in [
            ("PATCH", format!("/api/business/task/{id}/status")),
            ("PATCH", format!("/api/business/task/{id}/progress")),
            ("PATCH", format!("/api/business/task/{id}/assign")),
            ("POST", format!("/api/business/task/{id}/members")),
            ("GET", format!("/api/business/task/{id}/members")),
            ("DELETE", format!("/api/business/task/{id}")),
            ("PUT", format!("/api/business/task/{id}")),
            ("POST", "/api/business/task/page".to_string()),
            ("GET", "/api/business/task/page".to_string()),
            ("GET", "/api/business/task/my".to_string()),
            ("GET", format!("/api/business/task/{id}/comment")),
            ("POST", format!("/api/business/task/{id}/comment/extra")),
            ("POST", format!("/api/business/comment/{id}")),
            ("POST", "/api/system/user".to_string()),
            ("GET", "/api/business/storage/file/page".to_string()),
        ] {
            assert!(
                classify(method, &path).is_none(),
                "{method} {path} 不该命中白名单"
            );
        }
    }

    /// 令牌格式：前缀 + 64 个小写 hex；JWT / 大写 / 长度不符都不算任务令牌。
    #[test]
    fn token_shape_is_strict() {
        let (raw, hash) = new_token();
        assert!(is_ai_token(&raw), "自产令牌必须自识别：{raw}");
        assert_eq!(raw.len(), TOKEN_PREFIX.len() + TOKEN_HEX_LEN);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        for bad in [
            "".to_string(),
            "cpai_".to_string(),
            format!("CPAI_{}", "a".repeat(TOKEN_HEX_LEN)),
            format!("cpai_{}", "A".repeat(TOKEN_HEX_LEN)),
            format!("cpai_{}", "a".repeat(TOKEN_HEX_LEN - 1)),
            format!("cpai_{}", "a".repeat(TOKEN_HEX_LEN + 1)),
            // 人类 JWT 的形态
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig".to_string(),
        ] {
            assert!(!is_ai_token(&bad), "{bad} 不该被当成任务令牌");
        }
    }

    /// 同一明文 hash 稳定；不同明文 hash 不同（令牌库按 hash 反查，撞了就串任务）。
    #[test]
    fn hash_is_stable_and_collision_free_for_distinct_tokens() {
        assert_eq!(hash_token("cpai_abc"), hash_token("cpai_abc"));
        assert_ne!(hash_token("cpai_abc"), hash_token("cpai_abd"));
        // 已知向量：sha256("abc") 的首 8 位
        assert!(hash_token("abc").starts_with("ba7816bf"));
    }

    /// 两次生成不重复（32 字节熵），且明文不进 hash 明文回显。
    #[test]
    fn tokens_are_unique_and_hash_differs_from_plaintext() {
        let (a, hash_a) = new_token();
        let (b, _) = new_token();
        assert_ne!(a, b, "两次生成的令牌不能相同");
        assert_ne!(hash_a, a, "入库值必须是 hash，不能是明文");
        assert!(!hash_a.starts_with(TOKEN_PREFIX));
    }

    /// 过期判定是「到点即失效」（`<=`），不是「过了才失效」。
    #[test]
    fn expiry_boundary_is_inclusive() {
        let now = Utc::now();
        assert!(is_expired(now, now), "恰好到点就该失效");
        assert!(is_expired(now - chrono::Duration::seconds(1), now));
        assert!(!is_expired(now + chrono::Duration::seconds(1), now));
    }

    /// 任务终结后令牌失效：DONE / CANCELLED / 软删。
    #[test]
    fn closed_tasks_invalidate_tokens() {
        assert!(task_closed("DONE", false));
        assert!(task_closed("CANCELLED", false));
        assert!(task_closed("DOING", true), "软删同样终结");
        for open in ["TODO", "DOING", "REVIEW"] {
            assert!(!task_closed(open, false), "{open} 阶段令牌应当可用");
        }
    }
}

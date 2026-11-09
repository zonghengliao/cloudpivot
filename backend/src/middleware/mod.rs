//! 自定义中间件。
//!
//! 存放框架未内建、且与业务域无关的 HTTP 请求前后处理逻辑。框架已由 `config.toml`
//! 驱动装配了安全头 / CORS / 请求日志 / 认证 / 权限 / 限流 / 压缩等标准中间件，
//! 本目录只补充业务特有需求（如操作日志落库）。
//!
//! 装配方式有两种，能力边界完全不同：
//!
//! - [`crate::run`] 中的 `App::layer_from_fn`：作用于框架默认中间件链与 404 / 静态资源
//!   兜底**之后**，位于最外层（能观测到 401 / 403 / 429 与真实端到端耗时），
//!   **读不到**认证层注入的 `AuthClaims`；
//! - [`crate::run`] 中的 `App::route_layer`：位于内置链**之内**、认证层之后，
//!   能读 `AuthClaims`，用于「已通过认证但会话已作废」这类复检。
//!
//! | 中间件 | 职责 | 挂载方式 |
//! |--------|------|----------|
//! | [`operation_log`] | 记录写操作（方法 / 路径 / 操作人 / 耗时 / 结果）到 `operation_logs` | `layer_from_fn`（链外层） |
//! | [`session_guard`] | 会话守卫：令牌版本比对 + 首登强制改密 | `route_layer`（链内层） |
//! | [`auth_guard`] | 吊销复检：黑名单查询失败即拒绝（fail-closed） | `route_layer`（链内层） |
//! | [`task_token`] | 任务级短期令牌（白名单 + task_id 绑定） | `layer_from_fn`（最外层） |
//! | [`json_rejection`] | 请求体反序列化失败 → 统一错误体（E2） | `route_layer`（链内层） |

pub mod auth_guard;
pub mod operation_log;
pub mod session_guard;
pub mod task_token;

/// 请求体反序列化失败（axum extractor 拒绝）的响应体标准化。
///
/// # 背景（E2 缺陷 2）
///
/// `axum::Json` 在反序列化失败时**自己**生成响应（`JsonRejection`），根本不进 handler：
///
/// | 拒绝类型 | 状态码 | 默认 body |
/// |----------|--------|-----------|
/// | `JsonDataError`（缺字段 / 类型不匹配） | `422` | `text/plain`：``Failed to deserialize the JSON body into the target type: edges[0]: missing field `key` at line 1 column …`` |
/// | `JsonSyntaxError`（JSON 语法错） | `400` | `text/plain`，同上形态 |
/// | `MissingJsonContentType`（没带 `Content-Type: application/json`） | `415` | `text/plain` |
///
/// 于是前端 / 脚本按统一结构解析时拿到 `code=None`：`PUT graph` 缺 `edges[].key` 只得到
/// 一句英文纯文本，看不到「缺哪个字段」。本模块把这类**纯 extractor 拒绝**改写成
/// ``{code: 10001, msg: "请求体参数校验失败：…missing field `key`…"}``。
///
/// # 只处理 extractor 拒绝（不改业务码）
///
/// 判定用**双重条件**：状态码 ∈ {400, 415, 422} **且** `Content-Type` 是 `text/plain`。
/// 业务错误一律经 `Res::fail` 序列化成 `application/json`，因此 65015 / 10001 之类业务码的
/// 状态码、`code`、`msg` 一个字都不变。
pub mod json_rejection {
    use axum::extract::Request;
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::middleware::Next;
    use axum::response::{IntoResponse, Response};
    use nik::prelude::*;

    use crate::shared::error_code;

    /// 拒绝体最大读取量（真实拒绝体 < 1 KB；设上限是为了不被畸形请求撑大内存）。
    const MAX_REJECTION_BYTES: usize = 8 * 1024;

    /// 中间件本体（`axum::middleware::from_fn` 包装后交给 `App::route_layer`）。
    ///
    /// # 参数
    ///
    /// - `request`：进入的请求；
    /// - `next`：后续处理链。
    ///
    /// # 返回值
    ///
    /// 下游响应；命中 extractor 拒绝时换成统一错误体，其余原样返回。
    pub async fn normalize(request: Request, next: Next) -> Response {
        normalize_response(next.run(request).await).await
    }

    /// 纯响应侧改写（抽出来便于单测：中间件本体在单测里造不出 `Next`）。
    async fn normalize_response(response: Response) -> Response {
        if !is_extractor_rejection(&response) {
            return response;
        }
        let status = response.status();
        let (parts, body) = response.into_parts();
        let detail = match axum::body::to_bytes(body, MAX_REJECTION_BYTES).await {
            Ok(bytes) => String::from_utf8_lossy(&bytes).trim().to_string(),
            // 读不出来也要给标准体：宁可丢细节，也不能让前端继续拿到纯文本
            Err(_) => String::new(),
        };
        let message = if detail.is_empty() {
            "请求体格式不合法".to_string()
        } else {
            format!("请求体参数校验失败：{detail}")
        };
        let payload = serde_json::to_string(&Res::<()>::fail(error_code::PARAM_INVALID, message))
            .unwrap_or_else(|_| format!(r#"{{"code":{},"msg":"请求体参数校验失败"}}"#, error_code::PARAM_INVALID));
        // 状态码保持不变（422 仍是 422：前端/监控的重试与告警口径不变），
        // 只把 body 换成统一结构并声明 JSON；其余响应头（如 `x-request-id`）原样保留。
        let mut response = (status, payload).into_response();
        // `(StatusCode, String)` 的 `IntoResponse` 会自己写 `text/plain`，
        // 这里必须**显式覆盖**（否则改完仍是纯文本，前端照样解析不出结构）。
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        for (name, value) in parts.headers.iter() {
            if name.as_str() != header::CONTENT_TYPE.as_str() && name.as_str() != header::CONTENT_LENGTH.as_str() {
                response.headers_mut().insert(name.clone(), value.clone());
            }
        }
        response
    }

    /// 是否「axum extractor 拒绝」——`text/plain` 的 `400` / `415` / `422`。
    ///
    /// 业务错误响应是 `application/json`（`Res::fail` 序列化），故不会被误改。
    fn is_extractor_rejection(response: &Response) -> bool {
        if !matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNSUPPORTED_MEDIA_TYPE | StatusCode::UNPROCESSABLE_ENTITY
        ) {
            return false;
        }
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_ascii_lowercase().starts_with("text/plain"))
            .unwrap_or(false)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 造一个指定状态码 / Content-Type / body 的响应。
        fn response_of(status: StatusCode, content_type: &str, body: &str) -> Response {
            let mut response = (status, body.to_string()).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(content_type).expect("头值非法"),
            );
            response
        }

        #[test]
        fn plain_text_422_is_extractor_rejection() {
            let response = response_of(
                StatusCode::UNPROCESSABLE_ENTITY,
                "text/plain; charset=utf-8",
                "Failed to deserialize",
            );
            assert!(is_extractor_rejection(&response));
        }

        #[test]
        fn json_422_is_business_error() {
            let response = response_of(
                StatusCode::UNPROCESSABLE_ENTITY,
                "application/json; charset=utf-8",
                r#"{"code":65015,"msg":"x"}"#,
            );
            assert!(!is_extractor_rejection(&response));
        }

        #[test]
        fn plain_text_401_is_not_touched() {
            let response = response_of(StatusCode::UNAUTHORIZED, "text/plain", "未登录");
            assert!(!is_extractor_rejection(&response));
        }

        #[test]
        fn normalize_keeps_422_and_standard_body() {
            let response = response_of(
                StatusCode::UNPROCESSABLE_ENTITY,
                "text/plain; charset=utf-8",
                "Failed to deserialize the JSON body into the target type: edges[0]: missing field `key` at line 1 column 88",
            );
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("建 runtime 失败");
            let rewritten = rt.block_on(normalize_response(response));
            assert_eq!(
                rewritten.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "状态码必须保持 422"
            );
            let content_type = rewritten
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(
                content_type.starts_with("application/json"),
                "content-type={content_type}"
            );
            let body = rt
                .block_on(axum::body::to_bytes(rewritten.into_body(), 64 * 1024))
                .expect("读响应体失败");
            let parsed: serde_json::Value = serde_json::from_slice(&body).expect("响应体不是 JSON");
            assert_eq!(parsed["code"], serde_json::json!(error_code::PARAM_INVALID));
            let message = parsed["msg"].as_str().unwrap_or_default().to_string();
            assert!(message.contains("missing field `key`"), "msg={message}");
        }

        #[test]
        fn empty_body_still_gets_standard_message() {
            let response = response_of(StatusCode::BAD_REQUEST, "text/plain", "   ");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("建 runtime 失败");
            let rewritten = rt.block_on(normalize_response(response));
            assert_eq!(rewritten.status(), StatusCode::BAD_REQUEST);
            let body = rt
                .block_on(axum::body::to_bytes(rewritten.into_body(), 64 * 1024))
                .expect("读响应体失败");
            let parsed: serde_json::Value = serde_json::from_slice(&body).expect("响应体不是 JSON");
            assert_eq!(parsed["code"], serde_json::json!(error_code::PARAM_INVALID));
            assert_eq!(parsed["msg"], serde_json::json!("请求体格式不合法"));
        }
    }
}

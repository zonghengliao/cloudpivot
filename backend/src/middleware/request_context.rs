//! 请求上下文中间件：`request_id` 的生成 / 贯穿 / 回传。
//!
//! # 解决什么问题（评审 5.8）
//!
//! 改造前：`journald` 里只有「时间 / 级别 / target / message」，访问日志里的
//! `request_id` 恒为 `-`，一次用户操作打出的多条 SQL / 下游调用**串不起来**，
//! 跨服务定位只能靠时间戳。
//!
//! 本层把「一个请求」变成 `tracing` 里的一段上下文（span），落三件事：
//!
//! | 出口 | 形态 | 谁用 |
//! |------|------|------|
//! | `tracing` span `request{request_id=…, method=…, path=…}` | 请求内**所有**日志（handler / model / SQL / 下游）自动携带 | 运维 grep 日志还原一次请求 |
//! | 请求头 `x-request-id` | 规范化后的同一个 ID | 框架 `RequestLog` / `utils::request_id` / 操作日志 / changelog |
//! | 响应头 `X-Request-Id` | 同一个 ID | 前端错误提示、运维按 ID 反查 |
//!
//! # 为什么在项目侧做，而不是改 vendored 的 `nik-web`
//!
//! 框架自带的 `RequestIdLayer` 只做「请求头 + 响应头」，**不建 span**，因此
//! handler / SQL 日志拿不到 ID；而框架的 `RequestLogLayer` 又比它更靠外
//! （`app.rs` 里先 `layer(RequestIdLayer)` 再 `layer(RequestLogLayer)`，后加的在外层），
//! 于是访问日志里读到的请求头永远是空的 → `request_id = "-"`。
//!
//! 本层挂在**链最外层**（`App::layer_from_fn` 最后注册，见 [`crate::run`]），
//! 在框架两层之前把规范化 ID 写进请求头：框内 `RequestLog` 读到的不再是 `-`，
//! 框架 `RequestIdLayer` 也会原样复用（它优先取客户端头，没有再生成），
//! 无需改动 `backend/vendor/nik-web`（vendored 框架只当依赖，不当补丁面）。
//!
//! # 客户端传入的 ID 会被校验（不盲信）
//!
//! `X-Request-Id` 是外部可控输入，直接写进日志与响应头有两个风险：
//!
//! 1. **日志注入**：值里混入 `\n` / 引号可以把结构化日志拆行、伪造日志行；
//! 2. **响应头注入 / 放大**：超长值或控制字符会让 `HeaderValue::from_str` 失败或撑爆日志。
//!
//! 因此只接受「≤ [`MAX_INCOMING_LEN`] 字节、且只含 `[A-Za-z0-9._:-]`」的值，
//! 其余一律丢弃并改用服务端生成的 UUID v7（与 [`crate::utils::request_id`] 同口径）。
//!
//! # 为什么是 UUID v7 而不是 v4
//!
//! v7 前 48 位是毫秒时间戳，**按字典序 = 按时间序**：日志检索时同一个 ID 天然相邻，
//! 也让 `operation_logs.request_id` 这类入库值具备时间局部性。仓内
//! [`crate::utils::request_id`] 与各单据 `request_id` 早已统一用 v7，此处保持一致。

use std::time::Instant;

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
// `use nik::prelude::*` 是仓内约定：它把 `uuid` / `serde_json` / `axum` 等转发模块
// 带进作用域（`utils::request_id` 同款用法），因此下面能直接写 `uuid::Uuid::now_v7()`。
use nik::prelude::*;
use tracing::Instrument;

/// 请求 ID 头名（请求头与响应头同名；`http` crate 的查找大小写不敏感）。
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// 接受客户端传入 `X-Request-Id` 的最大字节数。
///
/// 真实调用方（网关 / 前端 / 脚本）用 UUID 或短 traceid，36～64 字节足矣；
/// 上限只为挡住「把整段文本塞进头里」的用法。
pub const MAX_INCOMING_LEN: usize = 128;

/// 把请求上下文中间件挂到给定路由。
///
/// # 参数
///
/// - `router`：待包装的 axum 路由（`App::layer_from_fn` 传入整站路由）。
///
/// # 返回值
///
/// 叠加了请求上下文层的新路由。
pub fn layer(router: axum::Router) -> axum::Router {
    router.layer(axum::middleware::from_fn(attach))
}

/// 中间件本体：解析 / 生成 `request_id`，开 span，注入请求头，回写响应头。
///
/// # 参数
///
/// - `request`：进入的请求。
/// - `next`：后续处理链（框架中间件链 + 业务 handler）。
///
/// # 返回值
///
/// 下游产生的响应原样返回，仅补一个 `X-Request-Id` 响应头；**任何分支都不改写状态码与响应体**。
pub async fn attach(mut request: Request, next: Next) -> Response {
    let request_id = resolve(request.headers());
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    // 把规范化后的 ID 写回请求头：框架 RequestLog / 框架 RequestId / 操作日志 /
    // changelog 都从请求头取它，从而全链路同一个值（这也是 `request_id = "-"` 的修法）。
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        request
            .headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
    }

    // span 一经建立，请求内**所有**事件（handler / model / SQL / 下游调用）都会带上
    // request_id、method、path —— 这就是「一次用户操作」的可检索边界。
    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %method,
        path = %path,
    );

    let started = Instant::now();
    let mut response = next.run(request).instrument(span.clone()).await;
    let duration_ms = started.elapsed().as_millis().min(i32::MAX as u128) as i32;

    // 收尾日志：与访问日志（框架 RequestLog）互为备份 —— 前者在链最外层，
    // 404 / 静态资源 / 被框架短路（如非法头）的请求也能留下一条带 ID 的记录。
    // 探针端点（/health、/health/deps、/metrics）降到 DEBUG：它们每几秒一次，
    // 用 INFO 打会把真正的业务日志淹没（告警/metrics 采集要的是安静的健康检查）。
    // guard 只在同步块内存在，绝不跨 await。
    {
        let _entered = span.enter();
        let status = response.status().as_u16();
        if is_probe_path(&path) {
            tracing::debug!(status, duration_ms, "请求完成（探针端点）");
        } else {
            tracing::info!(status, duration_ms, "请求完成");
        }
    }

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(REQUEST_ID_HEADER), value);
    }
    response
}

/// 解析请求头里的 `X-Request-Id`，不合法或缺失时生成新的 UUID v7。
///
/// # 参数
///
/// - `headers`：请求头集合。
///
/// # 返回值
///
/// 非空且已通过 [`is_acceptable`] 校验的请求 ID；**绝不返回 `-` 或空串**。
pub fn resolve(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| is_acceptable(value))
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string())
}

/// 判断客户端传入的请求 ID 是否可采信。
///
/// 判定（全部满足才采信）：非空、长度 ≤ [`MAX_INCOMING_LEN`]、只含
/// `[A-Za-z0-9._:-]`（UUID / W3C traceparent 的 trace-id / 网关自定义 ID 都在集合内）。
///
/// # 参数
///
/// - `value`：已 `trim` 的候选值。
///
/// # 返回值
///
/// 可采信返回 `true`；含控制字符、空格、引号、非 ASCII 或超长返回 `false`。
pub fn is_acceptable(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_INCOMING_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// 判断路径是否是「探针端点」（收尾日志降级到 DEBUG，避免每几秒一条淹没业务日志）。
///
/// # 参数
///
/// - `path`：请求路径（不含 query）。
///
/// # 返回值
///
/// `/health`、`/health/deps`、`/metrics` 返回 `true`（框架 `RequestLog` 同样把 `/health`
/// 放进默认 `exclude_paths`，两处口径一致）。
pub fn is_probe_path(path: &str) -> bool {
    matches!(path, "/health" | "/health/deps" | "/metrics")
}

#[cfg(test)]
mod tests {
    //! 纯函数（`resolve` / `is_acceptable` / `is_probe_path`）的单测。
    //!
    //! 中间件本体（`attach`）需要 `tower::ServiceExt::oneshot` 才能驱动，
    //! 而本仓刻意不声明 `tower` dev-dependency（见 `middleware/session_guard.rs` 的说明）；
    //! 因此 `attach` 的行为由 145 真实 HTTP 实测兜底（响应头 + 日志 grep，见证据包）。

    use super::*;

    /// 构造一个带 `x-request-id` 的请求头
    fn headers_with(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static(REQUEST_ID_HEADER),
            HeaderValue::from_str(value).expect("测试值必须是合法头值"),
        );
        headers
    }

    /// 客户端带了合法 ID → 原样复用（网关 / 前端注入的链路不能断）
    #[test]
    fn reuses_acceptable_client_id() {
        let id = resolve(&headers_with("a1b2c3d4-e5f6-7a8b-9c0d-1e2f3a4b5c6d"));
        assert_eq!(id, "a1b2c3d4-e5f6-7a8b-9c0d-1e2f3a4b5c6d");
    }

    /// 前端 `crypto.randomUUID()` 的形态、W3C traceparent 的 trace-id 都要能被采信
    #[test]
    fn accepts_common_id_shapes() {
        assert!(is_acceptable("0af7651916cd43dd8448eb211c80319c"));
        assert!(is_acceptable("req-20261110_03:44:12.123"));
        assert!(is_acceptable(&"a".repeat(MAX_INCOMING_LEN)));
    }

    /// 缺失 → 生成 UUID v7（36 字符、带连字符），**不是占位符 `-`**
    #[test]
    fn generates_uuid_when_absent() {
        let id = resolve(&HeaderMap::new());
        assert_ne!(id, "-", "缺失时必须生成真实 ID，不能留占位符");
        assert_eq!(id.len(), 36, "UUID 的规范形态是 36 字符");
        assert_eq!(id.matches('-').count(), 4);
    }

    /// 两次生成必须不同（唯一性：不允许退化成常量）
    #[test]
    fn generated_ids_are_unique() {
        let first = resolve(&HeaderMap::new());
        let second = resolve(&HeaderMap::new());
        assert_ne!(first, second);
    }

    /// 空白 / 纯空格 → 生成新 ID（不能把空白写进日志）
    #[test]
    fn regenerates_for_blank_values() {
        for blank in ["", "   ", "\t"] {
            let id = resolve(&headers_with(blank));
            assert_ne!(id, "-");
            assert_eq!(id.len(), 36, "空白值必须被丢弃并换成 UUID：{blank:?}");
        }
    }

    /// 日志注入 / 头注入的载荷一律不采信
    #[test]
    fn rejects_injection_payloads() {
        // 换行、引号、空格、分号、非 ASCII、超长
        let payloads = [
            "abc\ndef",
            "abc\r\nX-Injected: 1",
            "abc\"def",
            "abc def",
            "abc;drop",
            "注入",
            &"a".repeat(MAX_INCOMING_LEN + 1),
        ];
        for payload in payloads {
            assert!(!is_acceptable(payload), "不该采信客户端传入的 {payload:?}");
        }
    }

    /// 注入载荷经 `resolve` 后必须变成服务端生成的 ID（而不是被截断后沿用）
    #[test]
    fn resolve_replaces_injection_with_generated() {
        // 控制字符（换行）在 HTTP 层就被 `HeaderValue` 拒了 —— 这是第一道闸，
        // 根本走不到本中间件；此处把这一层也钉进测试，防止有人改成 `from_bytes` 后失守。
        assert!(
            HeaderValue::from_str("abc\ndef").is_err(),
            "http crate 必须拒绝含控制字符的头值"
        );

        // 真正能到达本中间件的两类：含空格的值、以及 UTF-8 高字节值
        //（`HeaderValue::from_bytes` 接受 0x80-0xFF，代理层转发脏值时可能带上）。
        let with_space = headers_with("abc def");
        let id = resolve(&with_space);
        assert!(!id.contains(' '), "含空格的值必须被丢弃");
        assert_eq!(id.len(), 36);

        let mut high_bytes = HeaderMap::new();
        high_bytes.insert(
            HeaderName::from_static(REQUEST_ID_HEADER),
            HeaderValue::from_bytes("注入".as_bytes()).expect("高字节头值可通过 from_bytes"),
        );
        assert!(!is_acceptable("注入"));
        assert_eq!(resolve(&high_bytes).len(), 36, "非 ASCII 值必须被丢弃");
    }

    /// 探针端点判定：三个端点降级 DEBUG，业务路径保持 INFO
    #[test]
    fn probe_paths_are_recognised() {
        for path in ["/health", "/health/deps", "/metrics"] {
            assert!(is_probe_path(path), "{path} 应被识别为探针端点");
        }
        for path in ["/api/system/user/page", "/healthz", "/", "/api/metrics"] {
            assert!(!is_probe_path(path), "{path} 不该被当成探针端点");
        }
    }
}

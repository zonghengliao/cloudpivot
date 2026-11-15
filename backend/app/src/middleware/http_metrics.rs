//! HTTP 侧指标埋点（P1-06）：请求数 / 耗时 / 错误率。
//!
//! # 三条指标，一个问题
//!
//! | 指标 | 类型 | 标签 | 回答什么 |
//! |------|------|------|----------|
//! | [`observability::HTTP_REQUESTS_TOTAL`] | counter | `method` `path` `status`（**精确码**） | 有多少请求、各返回什么码 |
//! | [`observability::HTTP_REQUEST_DURATION_SECONDS`] | histogram | `path` | 每个端点多慢（`histogram_quantile` 算 p95/p99） |
//! | [`observability::HTTP_ERRORS_TOTAL`] | counter | 无标签（启动即为 0） | 5xx 错误率（`rate()` 直接告警） |
//!
//! # 为什么框架已有 `nik_http_*` 还要再加一套
//!
//! 标签集不同，回答的问题也不同，**不是同一序列写两遍**：
//!
//! - `nik_http_*` 的 `status` 只到类别（`2xx`/`4xx`/`5xx`），要把 `404` 和 `422` 分开只能翻日志；
//!   本层记**精确码**，排障时一眼看到「这个端点在回 422」。
//! - `nik_http_*` 把 `/health` 排除（探针不该污染 SLO）；本层**计入**
//!   （探针流量本来也是真实流量，且验收要压 health 看数值增长）。
//! - 5xx 错误在 `nik_*` 侧只能 `sum by (status="5xx")` 聚合，写告警规则容易写错；
//!   这里给一条专用 counter，`rate(cloudpivot_http_errors_total[5m])` 就是错误率分子。
//!   它**不带标签**并靠 [`observability::seed_zero_series`] 在启动时建出 0 序列：
//!   系统健康时 Prometheus 不导出「没有任何数据点」的指标，不 seed 就看不到这条，
//!   运维会误以为「错误指标没上线」。按端点的 5xx 明细在 `requests_total{status="500"}`。
//!
//! 代价：每请求多 2 次原子自增 + 1 次直方图观察（百纳秒量级），相对一次 PG 往返可忽略。
//!
//! # 标签基数是这里唯一会自伤的地方
//!
//! `path` 直接用原始 URI 会把 `/users/1` `/users/2` … 变成无限多条时间序列
//! （Prometheus 最典型的自杀姿势）。因此：
//!
//! 1. **归一化**：纯数字段 → `:id`、UUID → `:uuid`、长随机串 → `:token`（见 [`normalize_path`]）；
//! 2. **硬上限** [`MAX_PATH_LABELS`]：超过后新路径统一归 [`PATH_OTHER`]，绝不无限增长。
//!
//! `path` 标签值必须是 `'static`（`metrics` 不复制字符串），所以归一化后的串用
//! `Box::leak` 泄漏一次、由 [`PATH_LABELS`] 集合去重复用 —— 泄漏量被上限钉死。
//!
//! # 挂在哪
//!
//! `App::layer_from_fn`（链外层，见 [`crate::run`]）：只有在这里才能看到
//! **框架自己拒绝的请求**（401 未登录 / 403 无权限 / 429 限流 / 404 兜底）。
//! 挂在 handler 里或链内层会让这些请求从指标里消失，故障时会得出「成功率 100%」的错觉。
//!
//! `/metrics` 自身必须排除（抓取频率不能算成业务请求），路径取
//! `[metrics].path`（默认 `/metrics`）而不写死。
//!
//! 计入范围是**接口流量**：`[router].prefix`（`/api`）下的请求 + 根路径的
//! `/health` / `/health/deps`。静态资源与 SPA 回退不计数 —— 一体化部署时前端有
//! 400+ 个带 hash 的 chunk，全记会把 [`MAX_PATH_LABELS`] 的标签预算吃光，
//! 之后真正的接口路径反而被归并成 [`PATH_OTHER`]。

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant;

use axum::extract::Request;
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;

use cloudpivot_kernel::shared::observability;

/// `path` 标签的硬上限（超出归 [`PATH_OTHER`]）。
///
/// 与框架 `nik_web::metrics::MAX_ROUTE_LABELS` 取同值：正常应用的路由数远小于此，
/// 触发上限通常意味着有人在用通配路由承接任意路径（或正被路径遍历扫描），
/// 此时恰恰不该为每个新路径建序列。
pub const MAX_PATH_LABELS: usize = 512;

/// 标签超限时的归并值。
pub const PATH_OTHER: &str = "__other__";

/// 单个 path 标签的最大字节数（超过直接归 [`PATH_OTHER`]，防止超长 URI 撑爆标签）。
const MAX_PATH_LEN: usize = 256;

/// 已登记的 `path` 标签（值经 `Box::leak` 转 `'static`，数量被 [`MAX_PATH_LABELS`] 钉死）。
static PATH_LABELS: LazyLock<Mutex<HashSet<&'static str>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// 把指标埋点层挂到给定路由（`App::layer_from_fn` 的入参形态）。
pub fn layer(router: axum::Router) -> axum::Router {
    router.layer(axum::middleware::from_fn(track))
}

/// 中间件本体：进请求前记开始时间，出响应后按状态码记账。
///
/// # 参数
///
/// - `request`：进入的请求；
/// - `next`：后续处理链。
///
/// # 返回值
///
/// 下游响应原样返回（本层**只观测、不改写**）。
pub async fn track(request: Request, next: Next) -> Response {
    // 首个请求时把「只在异常时才累加」的 counter 建出 0 序列（幂等，之后只是一次原子读）
    observability::seed_zero_series();

    let path = request.uri().path().to_string();
    // 只统计接口流量：`/metrics` 自身（抓取频率不是业务量）、静态资源与 SPA 回退都不算
    if !is_counted_path(&path) {
        return next.run(request).await;
    }

    let method = method_label(request.method());
    let path = path_label(&path);
    let started = Instant::now();

    let response = next.run(request).await;

    let code = response.status().as_u16();
    observability::inc_counter_labeled(
        observability::HTTP_REQUESTS_TOTAL,
        &[("method", method), ("path", path), ("status", status_label(code))],
        1,
    );
    observability::observe_histogram(
        observability::HTTP_REQUEST_DURATION_SECONDS,
        &[("path", path)],
        started.elapsed().as_secs_f64(),
    );
    // 用**数值码**判错误，不用标签值：标签表里的冷门码会归成 `other`，
    // 拿标签判会让 599 这类 5xx 悄悄漏出错误率。
    if is_server_error(code) {
        observability::inc_counter(observability::HTTP_ERRORS_TOTAL, 1);
    }

    response
}

/// 是否是服务端错误（`status >= 500`）。
///
/// 只算 5xx：4xx 是调用方的问题（登录过期、参数错），把它算进错误率
/// 会让「前端版本不对」看起来像「后端挂了」。
#[must_use]
pub fn is_server_error(status: u16) -> bool {
    status >= 500
}

/// 请求路径是否就是指标端点（自身抓取不计入）。
///
/// 路径取 `[metrics].path`：`App::start` 在开始 serve **之前**就装好配置，
/// 因此首个请求到达时读得到；读不到（单测 / 极端早启动）才退回默认 `/metrics`。
fn is_metrics_path(path: &str) -> bool {
    static CONFIGURED: OnceLock<&'static str> = OnceLock::new();
    let configured = *CONFIGURED.get_or_init(|| match nik::try_config() {
        Some(manager) => manager.get().metrics.path.as_str(),
        None => "/metrics",
    });
    path == configured || path == "/metrics"
}

/// 全局路由前缀（`[router].prefix`，通常是 `/api`）；读不到配置时返回空串。
///
/// 空串的语义是「全量计数」——**保守方向**：宁可多记几条（静态资源），
/// 也不能因为读不到配置把真正的接口请求漏掉。
fn router_prefix() -> &'static str {
    static PREFIX: OnceLock<&'static str> = OnceLock::new();
    PREFIX.get_or_init(|| match nik::try_config() {
        Some(manager) => manager.get().router.prefix.as_str(),
        None => "",
    })
}

/// 是否计入 HTTP 指标：只统计「接口流量」。
///
/// # 为什么不是全量
///
/// 后端一旦开启 `[static_files].enabled`（单机一体化部署），前端 400+ 个带 hash
/// 的 chunk 会各自占一个 `path` 标签，把 [`MAX_PATH_LABELS`] 吃光 —— 之后真正的
/// 接口路径会被归并成 [`PATH_OTHER`]，指标等于废掉。静态资源与 SPA 回退也不是
/// 「接口流量」：它们的量与容量问题由 nginx 访问日志回答。
///
/// 保留根路径的 `/health` / `/health/deps`：它们豁免 `[router].prefix`
/// （约定 8），却是最常用的连通性压测目标。
fn is_counted_path(path: &str) -> bool {
    if is_metrics_path(path) {
        return false;
    }
    counted_under(path, router_prefix())
}

/// [`is_counted_path`] 的纯函数内核（便于脱离全局配置单测）。
fn counted_under(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    path.starts_with(prefix) || path == "/health" || path.starts_with("/health/")
}

/// HTTP 方法 → 静态标签值（`metrics` 的标签不接受非 `'static` 的 `&str`）。
fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

/// 状态码 → 静态标签值（**精确码**，这是与框架 `status="4xx"` 的关键差别）。
///
/// 只列举本服务实际会回的状态码：`Res::fail` 走 `200` + 业务码，认证/权限/限流
/// 由框架层回 `401/403/429`，参数校验回 `400/415/422`，兜底 `404`，未捕获异常 `500`，
/// 反向代理侧 `502/504`。表外的冷门码归 `other` —— 不让一条罕见状态码撑出无限标签。
fn status_label(code: u16) -> &'static str {
    match code {
        100 => "100",
        200 => "200",
        201 => "201",
        202 => "202",
        204 => "204",
        206 => "206",
        301 => "301",
        302 => "302",
        304 => "304",
        400 => "400",
        401 => "401",
        403 => "403",
        404 => "404",
        405 => "405",
        406 => "406",
        408 => "408",
        409 => "409",
        412 => "412",
        413 => "413",
        415 => "415",
        422 => "422",
        429 => "429",
        500 => "500",
        501 => "501",
        502 => "502",
        503 => "503",
        504 => "504",
        _ => "other",
    }
}

/// 归一化后的 `path` 标签（进程内复用同一个 `&'static str`）。
fn path_label(path: &str) -> &'static str {
    let mut labels = PATH_LABELS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    label_for(&mut labels, path)
}

/// 标签登记内核（抽成纯函数：单测能用独立集合验证「去重 + 上限」，
/// 不会把真实标签集塞满 —— 那会连带让同进程的其他用例全部归并到 [`PATH_OTHER`]）。
fn label_for(labels: &mut HashSet<&'static str>, path: &str) -> &'static str {
    let normalized = normalize_path(path);
    if let Some(existing) = labels.get(normalized.as_str()) {
        return existing;
    }
    if labels.len() >= MAX_PATH_LABELS {
        return PATH_OTHER;
    }
    let leaked: &'static str = Box::leak(normalized.into_boxed_str());
    labels.insert(leaked);
    leaked
}

/// 把具体路径归一化成有限标签：`/users/42` → `/users/:id`。
///
/// 规则（与框架 `route_label` 同口径，避免同一个 URL 在两个指标里长得不一样）：
///
/// | 段形态 | 归一化为 | 例 |
/// |--------|----------|-----|
/// | 纯数字 | `:id` | `/business/hr/employee/123` → `/business/hr/employee/:id` |
/// | UUID | `:uuid` | `/notice/0192f…` → `/notice/:uuid` |
/// | ≥24 字节的随机串 | `:token` | 分享链接 / 一次性令牌 |
/// | 其余 | 原样 | `/business/hr/salary/page` 保持可读 |
/// | 整体超长（> [`MAX_PATH_LEN`]） | [`PATH_OTHER`] | 路径遍历扫描 |
#[must_use]
pub fn normalize_path(path: &str) -> String {
    if path.len() > MAX_PATH_LEN {
        return PATH_OTHER.to_string();
    }
    let mut normalized = String::with_capacity(path.len());
    for segment in path.split('/') {
        if segment.is_empty() {
            normalized.push('/');
            continue;
        }
        normalized.push('/');
        if segment.bytes().all(|b| b.is_ascii_digit()) {
            normalized.push_str(":id");
        } else if is_uuid(segment) {
            normalized.push_str(":uuid");
        } else if segment.len() >= 24
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            normalized.push_str(":token");
        } else {
            normalized.push_str(segment);
        }
    }
    if normalized.is_empty() {
        "/".to_string()
    } else {
        // 去掉开头重复的 `/`（`split` 对前导 `/` 会先产出空段）
        let trimmed = normalized.trim_start_matches('/');
        format!("/{trimmed}")
    }
}

/// 形如 `8-4-4-4-12` 的十六进制串。
fn is_uuid(segment: &str) -> bool {
    segment.len() == 36
        && matches!(segment.as_bytes().get(8), Some(b'-'))
        && matches!(segment.as_bytes().get(13), Some(b'-'))
        && matches!(segment.as_bytes().get(18), Some(b'-'))
        && matches!(segment.as_bytes().get(23), Some(b'-'))
        && segment
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_hexdigit() || matches!(i, 8 | 13 | 18 | 23))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use nik::prelude::{App, AppConfig};
    use std::net::SocketAddr;
    use std::sync::OnceLock;

    #[test]
    fn numeric_and_uuid_segments_are_folded() {
        assert_eq!(normalize_path("/business/hr/employee/123"), "/business/hr/employee/:id");
        assert_eq!(
            normalize_path("/notice/0192f3a1-4b5c-7d8e-9f01-234567890abc"),
            "/notice/:uuid"
        );
        // 语义段保持原样：排障时要能看出是哪个接口
        assert_eq!(normalize_path("/business/hr/salary/page"), "/business/hr/salary/page");
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn long_opaque_segments_and_overlong_paths_are_folded() {
        assert_eq!(normalize_path("/share/AbCdEf0123456789AbCdEf01"), "/share/:token");
        // 路径遍历扫描 / 超长 URI：直接归并，不建序列
        let long = format!("/{}", "a".repeat(MAX_PATH_LEN));
        assert_eq!(normalize_path(&long), PATH_OTHER);
    }

    #[test]
    fn path_labels_are_reused_and_bounded() {
        let mut labels = HashSet::new();
        let first = label_for(&mut labels, "/business/hr/employee/1");
        let second = label_for(&mut labels, "/business/hr/employee/2");
        assert_eq!(first, second, "同形路径必须复用同一个标签值");
        assert!(std::ptr::eq(first, second), "复用的应是同一个静态串");
        assert_eq!(labels.len(), 1, "两个具体 id 只应占一个标签");

        // 上限兜底：达到上限后新路径归 __other__（用独立集合，不污染真实标签集）
        for i in 0..MAX_PATH_LABELS {
            label_for(&mut labels, &format!("/probe-{i}"));
        }
        assert_eq!(labels.len(), MAX_PATH_LABELS);
        assert_eq!(
            label_for(&mut labels, "/probe-after-limit"),
            PATH_OTHER,
            "超过上限后必须归并到 {PATH_OTHER}"
        );
    }

    #[test]
    fn only_interface_traffic_is_counted() {
        // 有前缀（生产形态）：接口与健康探针计数，静态资源 / SPA 回退不计数
        assert!(counted_under("/api/system/user/page", "/api"));
        assert!(counted_under("/api/health", "/api"));
        assert!(counted_under("/health", "/api"));
        assert!(counted_under("/health/deps", "/api"));
        assert!(!counted_under("/assets/index-a1b2c3.js", "/api"));
        assert!(!counted_under("/business/hr/employee", "/api"), "SPA 回退不算接口流量");

        // 无前缀（单测 / 特殊部署）：全量计数，宁可多记也不漏
        assert!(counted_under("/card-p106-ok", ""));
    }

    #[test]
    fn status_codes_keep_exact_codes_and_only_5xx_are_errors() {
        assert_eq!(status_label(200), "200");
        assert_eq!(status_label(404), "404");
        assert_eq!(status_label(422), "422");
        assert_eq!(status_label(500), "500");
        assert_eq!(status_label(503), "503");
        assert_eq!(status_label(599), "other", "冷门码不能撑出无限标签");

        assert!(!is_server_error(200));
        assert!(!is_server_error(404), "4xx 是调用方问题，不进错误率");
        assert!(is_server_error(500));
        assert!(is_server_error(503));
        // 冷门 5xx：标签虽归 other，错误数仍要算（判据取数值码，不取标签）
        assert!(is_server_error(599));
    }

    /// 进程内只装一次 recorder（全局只能有一个）。
    fn ensure_recorder() {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| {
            let cfg = AppConfig::default();
            let _ = nik::metrics::install_prometheus(&cfg.metrics);
        });
    }

    /// 验收主用例：起真服务 → 打 200 / 500 各一次 → `/metrics` 上三条指标都在且数值正确。
    #[tokio::test]
    async fn metrics_are_exported_after_real_requests() {
        ensure_recorder();

        let mut cfg = AppConfig::default();
        cfg.metrics.enabled = true;

        let app = App::with_config(cfg)
            .expect("build app")
            .get("/card-p106-ok", || async { "ok" })
            .get("/card-p106-boom", || async {
                (StatusCode::INTERNAL_SERVER_ERROR, "boom")
            })
            .layer_from_fn(layer)
            .into_router();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });

        // 2 次成功 —— 先只看「健康状态」下导出文本长什么样
        assert_eq!(http_get(addr, "/card-p106-ok").await.0, 200);
        assert_eq!(http_get(addr, "/card-p106-ok").await.0, 200);

        let (status, healthy) = http_get(addr, "/metrics").await;
        assert_eq!(status, 200, "回环抓取应成功：{healthy}");

        // ① 请求数：按 method + path + 精确码
        assert_eq!(
            metric_value(&healthy, "cloudpivot_http_requests_total", "/card-p106-ok", Some("200")),
            Some(2.0),
            "请求数应为 2：{healthy}"
        );

        // ② 延迟直方图：卡面要求的桶边界必须真的在导出文本里
        assert!(
            healthy.contains("# TYPE cloudpivot_http_request_duration_seconds histogram"),
            "直方图缺 TYPE：{healthy}"
        );
        for bucket in ["0.01", "0.05", "0.1", "0.5", "1", "5"] {
            let needle =
                format!("cloudpivot_http_request_duration_seconds_bucket{{path=\"/card-p106-ok\",le=\"{bucket}\"}}");
            assert!(healthy.contains(&needle), "缺 {bucket}s 桶：{needle}");
        }
        assert!(
            healthy.contains("cloudpivot_http_request_duration_seconds_count{path=\"/card-p106-ok\"}"),
            "直方图缺 _count：{healthy}"
        );

        // ③ 错误数：**没有 5xx 时也必须能看到这条**（seed 出 0 序列）。
        //    不 seed 的话 Prometheus 根本不导出「没有任何数据点」的指标，
        //    运维 grep errors_total 会以为指标没上线（P1-05 踩过的坑）。
        assert!(
            healthy.contains("# TYPE cloudpivot_http_errors_total counter"),
            "错误数缺 TYPE（未 seed 0 序列）：{healthy}"
        );
        assert_eq!(
            unlabeled_value(&healthy, "cloudpivot_http_errors_total"),
            Some(0.0),
            "健康状态下错误数应为 0：{healthy}"
        );

        // ④ 抓取自身不计数（否则每 15s 抓一次就给自己 +1）
        assert!(
            !healthy.contains("path=\"/metrics\""),
            "指标端点自身必须排除：{healthy}"
        );

        // 再来一次 5xx：错误数必须 +1，且请求数按精确码 500 单独成一条
        assert_eq!(http_get(addr, "/card-p106-boom").await.0, 500);
        let (status, after_error) = http_get(addr, "/metrics").await;
        server.abort();
        assert_eq!(status, 200, "回环抓取应成功：{after_error}");
        assert_eq!(
            unlabeled_value(&after_error, "cloudpivot_http_errors_total"),
            Some(1.0),
            "5xx 应计入错误数：{after_error}"
        );
        assert_eq!(
            metric_value(
                &after_error,
                "cloudpivot_http_requests_total",
                "/card-p106-boom",
                Some("500")
            ),
            Some(1.0),
            "500 也要按精确码计数：{after_error}"
        );
        // 4xx 不进错误数（这条路径没被请求过，值仍是 0 → 用 2xx 那条侧证）
        assert_eq!(
            metric_value(
                &after_error,
                "cloudpivot_http_requests_total",
                "/card-p106-ok",
                Some("200")
            ),
            Some(2.0),
            "成功请求不应被错误计数覆盖：{after_error}"
        );
    }

    /// 取一条**不带标签**的指标的数值。
    fn unlabeled_value(body: &str, name: &str) -> Option<f64> {
        body.lines()
            .find(|line| line.starts_with(&format!("{name} ")))
            .and_then(|line| line.rsplit_once(' ').map(|(_, value)| value))
            .and_then(|value| value.parse().ok())
    }

    /// 从导出文本里取一条序列的数值（找不到返回 `None`）。
    fn metric_value(body: &str, name: &str, path: &str, status: Option<&str>) -> Option<f64> {
        body.lines()
            .filter(|line| line.starts_with(name))
            .find(|line| {
                line.contains(&format!("path=\"{path}\""))
                    && status.is_none_or(|code| line.contains(&format!("status=\"{code}\"")))
            })
            .and_then(|line| line.rsplit_once(' ').map(|(_, value)| value))
            .and_then(|value| value.parse().ok())
    }

    /// 发一条最小 HTTP/1.1 请求（不引第三方客户端，直接走 TCP）。
    async fn http_get(addr: SocketAddr, path: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write request");

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.expect("read response");
        let text = String::from_utf8_lossy(&raw).to_string();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let status = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }
}

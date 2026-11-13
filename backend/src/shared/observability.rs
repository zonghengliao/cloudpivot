//! 业务指标目录（`cloudpivot_*`）：**名字 / 类型 / 单位 / 说明的唯一出处**。
//!
//! # 为什么需要这个模块
//!
//! 1. **端点归框架，说明归业务**：`GET /metrics` 由框架提供（编译期 `metrics-prometheus`
//!    feature + 运行时 `[metrics].enabled`，见 `vendor/nik-web/src/app.rs` 的
//!    `attach_metrics_endpoint`）。框架只 describe 它自己的指标（`nik_http_*` / DB / 缓存），
//!    业务侧 `cloudpivot_*` 若不 describe，导出的文本里就只有**裸序列**、没有
//!    `# HELP` / `# TYPE` —— 运维抓到一个 `cloudpivot_invariant_ok` 也无从知道 1 是好还是坏。
//! 2. **名字散落 4 个文件**（`shared/token_revocation`、`support/health`、`system/auth/cleanup`、
//!    `task/metrics_sampler`）时，改名只能靠 grep；收进 [`CATALOG`] 后由
//!    `every_sampler_metric_is_registered` 单测兜住「新加了名字却忘了登记」。
//! 3. **describe 的时序**：`metrics` 门面的 `describe_*` 在 recorder 未安装时是**静默丢弃**
//!    （不是缓存后再补，见 `vendor/nik-web/src/metrics.rs` 的 `install` 注释）。启动钩子
//!    （`App::on_startup`）跑在 recorder 安装完成**之前**，在那里 describe 会丢。
//!    因此这里把 describe 挂在**写入前**（[`set_gauge`] / [`inc_counter`] 内部），
//!    第一次真正写指标一定发生在启动之后，既不依赖钩子时序，也不在热路径上重复登记
//!    （[`describe_once`] 是 `Once`）。
//!
//! # 与 `metrics` crate 的关系
//!
//! 未装 recorder（本仓 feature 关掉）时 `metrics::gauge!` 全是 no-op，[`set_gauge`]
//! 也是 no-op —— 与既有埋点的语义一致，可安全常开。
//!
//! # 新增一个业务指标要改哪里
//!
//! 1. 在 [`CATALOG`] 加一行（名字 / 类型 / 单位 / 说明）；
//! 2. 在写入处用 `observability::set_gauge(&NAME, value)` / `inc_counter(&NAME, delta)`；
//! 3. 跑 `cargo test --lib shared::observability`（目录自检 + 端点渲染都覆盖到）。

use std::sync::Once;

/// 指标类型（只登记本仓真正用到的两种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// 瞬时值（可升可降）。
    Gauge,
    /// 单调累计值（进程重启归零）。
    Counter,
}

/// 一个业务指标的元数据。
#[derive(Debug, Clone, Copy)]
pub struct MetricDef {
    /// Prometheus 指标名（`cloudpivot_` 前缀，snake_case）。
    pub name: &'static str,
    /// 类型：决定渲染出的 `# TYPE` 行与写入用哪条宏。
    pub kind: MetricKind,
    /// 单位（`metrics::Unit` 的字符串形式，仅用于 `# HELP` 文案里的口头说明）。
    pub unit: &'static str,
    /// 一句话说明，渲染成 `# HELP`。**不要带换行 / 反斜杠**（Prometheus 文本格式要求转义）。
    pub help: &'static str,
}

impl MetricDef {
    const fn gauge(name: &'static str, unit: &'static str, help: &'static str) -> Self {
        Self {
            name,
            kind: MetricKind::Gauge,
            unit,
            help,
        }
    }

    const fn counter(name: &'static str, unit: &'static str, help: &'static str) -> Self {
        Self {
            name,
            kind: MetricKind::Counter,
            unit,
            help,
        }
    }
}

// ==================== 指标名常量（写入处引用这些，别手写字符串） ====================

/// 鉴权缓存是否降级（1 = 缓存写失败 / 不可用，吊销判定仍以库为准）。
pub const AUTH_CACHE_DEGRADED: &str = "cloudpivot_auth_cache_degraded";
/// `sys_token_blacklist` 当前行数。
pub const TOKEN_BLACKLIST_ROWS: &str = "cloudpivot_token_blacklist_rows";
/// 清理任务累计删除的过期吊销行数。
pub const TOKEN_BLACKLIST_CLEANUP_DELETED: &str = "cloudpivot_token_blacklist_cleanup_deleted";
/// 导入任务中尚未开始解析的条数。
pub const IMPEXP_IMPORT_PENDING: &str = "cloudpivot_impexp_import_pending";
/// 导入任务中未终态的条数。
pub const IMPEXP_IMPORT_OPEN: &str = "cloudpivot_impexp_import_open";
/// 最早的未终态导入任务已存在多少秒。
pub const IMPEXP_IMPORT_STUCK_SECONDS: &str = "cloudpivot_impexp_import_stuck_seconds";
/// 近 24h 失败的导入任务数。
pub const IMPEXP_IMPORT_FAILED_24H: &str = "cloudpivot_impexp_import_failed_24h";
/// 导出任务中尚未开始执行的条数。
pub const IMPEXP_EXPORT_PENDING: &str = "cloudpivot_impexp_export_pending";
/// 导出任务中未终态的条数。
pub const IMPEXP_EXPORT_OPEN: &str = "cloudpivot_impexp_export_open";
/// 最早的未终态导出任务已存在多少秒。
pub const IMPEXP_EXPORT_STUCK_SECONDS: &str = "cloudpivot_impexp_export_stuck_seconds";
/// 近 24h 失败的导出任务数。
pub const IMPEXP_EXPORT_FAILED_24H: &str = "cloudpivot_impexp_export_failed_24h";
/// 导入 + 导出未终态任务合计（积压总量）。
pub const IMPEXP_BACKLOG_PENDING: &str = "cloudpivot_impexp_backlog_pending";
/// 最早一张未终态任务已存在多少秒（两个方向取大）。
pub const IMPEXP_BACKLOG_STUCK_SECONDS: &str = "cloudpivot_impexp_backlog_stuck_seconds";
/// 最近一次账务不变量巡检是否 OK（1/0，-1 = 从未跑过）。
pub const INVARIANT_OK: &str = "cloudpivot_invariant_ok";
/// 最近一次巡检的 BLOCKER 数（-1 = 未知）。
pub const INVARIANT_BLOCKERS: &str = "cloudpivot_invariant_blockers";
/// 最近一次巡检的失败项数（-1 = 未知）。
pub const INVARIANT_FAILED: &str = "cloudpivot_invariant_failed";
/// 最近一次巡检距今多少秒（-1 = 从未跑过）。
pub const INVARIANT_AGE_SECONDS: &str = "cloudpivot_invariant_age_seconds";
/// 本次采样成功的 epoch 秒（不前进 = 采样器挂了）。
pub const SAMPLER_LAST_SUCCESS_TIMESTAMP: &str = "cloudpivot_metrics_sampler_last_success_timestamp";
/// 采样累计执行次数。
pub const SAMPLER_RUNS_TOTAL: &str = "cloudpivot_metrics_sampler_runs_total";

/// 全部业务指标的元数据目录（**必须与真正写入的名字一一对应**）。
///
/// 顺序只影响源码可读性，不影响导出结果（导出按名字排序）。
pub const CATALOG: &[MetricDef] = &[
    MetricDef::gauge(
        AUTH_CACHE_DEGRADED,
        "1|0",
        "鉴权缓存是否降级：1=缓存写失败或不可用（吊销判定仍以数据库为准），0=正常",
    ),
    MetricDef::gauge(
        TOKEN_BLACKLIST_ROWS,
        "行",
        "sys_token_blacklist 当前行数（短命表；持续增长说明清理任务没在跑）",
    ),
    MetricDef::counter(
        TOKEN_BLACKLIST_CLEANUP_DELETED,
        "行",
        "令牌吊销清理任务累计删除的过期行数",
    ),
    MetricDef::gauge(IMPEXP_IMPORT_PENDING, "个", "导入任务中尚未开始解析（PENDING）的条数"),
    MetricDef::gauge(
        IMPEXP_IMPORT_OPEN,
        "个",
        "导入任务中未终态（PENDING/PARSING/VALIDATED/IMPORTING）的条数",
    ),
    MetricDef::gauge(IMPEXP_IMPORT_STUCK_SECONDS, "秒", "最早的未终态导入任务已存在多少秒"),
    MetricDef::gauge(IMPEXP_IMPORT_FAILED_24H, "个", "近 24 小时失败的导入任务数"),
    MetricDef::gauge(IMPEXP_EXPORT_PENDING, "个", "导出任务中尚未开始执行（PENDING）的条数"),
    MetricDef::gauge(IMPEXP_EXPORT_OPEN, "个", "导出任务中未终态（PENDING/RUNNING）的条数"),
    MetricDef::gauge(IMPEXP_EXPORT_STUCK_SECONDS, "秒", "最早的未终态导出任务已存在多少秒"),
    MetricDef::gauge(IMPEXP_EXPORT_FAILED_24H, "个", "近 24 小时失败的导出任务数"),
    MetricDef::gauge(IMPEXP_BACKLOG_PENDING, "个", "导入与导出未终态任务合计（积压总量）"),
    MetricDef::gauge(
        IMPEXP_BACKLOG_STUCK_SECONDS,
        "秒",
        "最早一张未终态任务已存在多少秒（导入/导出取大）",
    ),
    MetricDef::gauge(
        INVARIANT_OK,
        "1|0|-1",
        "最近一次账务不变量巡检是否通过：1=通过，0=有失败，-1=从未跑过或未知",
    ),
    MetricDef::gauge(
        INVARIANT_BLOCKERS,
        "个",
        "最近一次账务不变量巡检的 BLOCKER 数（-1=未知）",
    ),
    MetricDef::gauge(INVARIANT_FAILED, "个", "最近一次账务不变量巡检的失败项数（-1=未知）"),
    MetricDef::gauge(
        INVARIANT_AGE_SECONDS,
        "秒",
        "最近一次账务不变量巡检距今多少秒（-1=从未跑过）",
    ),
    MetricDef::gauge(
        SAMPLER_LAST_SUCCESS_TIMESTAMP,
        "秒",
        "运维观测采样器最近一次成功采样的 Unix 时间戳（不前进=采样器已停）",
    ),
    MetricDef::counter(SAMPLER_RUNS_TOTAL, "次", "运维观测采样器累计执行次数"),
];

/// 按名字查目录（O(n)，n≈19；只在写指标时用，不在请求热路径上）。
#[must_use]
pub fn lookup(name: &str) -> Option<&'static MetricDef> {
    CATALOG.iter().find(|def| def.name == name)
}

/// 名字是否已登记（单测与 `debug_assert` 用）。
#[must_use]
pub fn is_registered(name: &str) -> bool {
    lookup(name).is_some()
}

static DESCRIBE: Once = Once::new();

/// 把 [`CATALOG`] 的元数据登记给当前 recorder（幂等，进程内只执行一次）。
///
/// 为什么不是启动钩子：`App::on_startup` 的钩子跑在 recorder 安装（`App::start` 的 2.5 步）
/// **之前**，那时 `describe_*` 会被静默丢弃。挂在写入前最稳。
pub fn describe_once() {
    DESCRIBE.call_once(|| {
        for def in CATALOG {
            match def.kind {
                MetricKind::Gauge => metrics::describe_gauge!(
                    def.name,
                    metrics::Unit::Count,
                    format!("{}（单位：{}）", def.help, def.unit)
                ),
                MetricKind::Counter => metrics::describe_counter!(
                    def.name,
                    metrics::Unit::Count,
                    format!("{}（单位：{}）", def.help, def.unit)
                ),
            }
        }
    });
}

/// 写入一个业务 gauge（自动带 `# HELP` / `# TYPE`）。
///
/// # Panics
///
/// debug 构建下名字未登记会 panic —— 宁可单测红，也不要线上导出一条没有说明的序列。
pub fn set_gauge(name: &'static str, value: f64) {
    debug_assert!(
        is_registered(name),
        "指标 {name} 未登记在 observability::CATALOG，导出文本会缺 # HELP / # TYPE"
    );
    describe_once();
    metrics::gauge!(name).set(value);
}

/// 累加一个业务 counter（自动带 `# HELP` / `# TYPE`）。
pub fn inc_counter(name: &'static str, delta: u64) {
    debug_assert!(
        is_registered(name),
        "指标 {name} 未登记在 observability::CATALOG，导出文本会缺 # HELP / # TYPE"
    );
    describe_once();
    metrics::counter!(name).increment(delta);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::{App, AppConfig};
    use std::net::SocketAddr;
    use std::sync::OnceLock;

    /// 目录自检：前缀 / 唯一性 / 文案非空。
    #[test]
    fn catalog_is_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for def in CATALOG {
            assert!(
                def.name.starts_with("cloudpivot_"),
                "业务指标必须以 cloudpivot_ 开头：{}",
                def.name
            );
            assert!(
                def.name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "指标名必须是 snake_case：{}",
                def.name
            );
            assert!(seen.insert(def.name), "指标名重复登记：{}", def.name);
            assert!(!def.help.is_empty(), "指标 {} 缺 # HELP 文案", def.name);
            // Prometheus 文本格式里 HELP 不许含换行 / 反斜杠（否则整行失效）
            assert!(
                !def.help.contains('\n') && !def.help.contains('\\'),
                "指标 {} 的 HELP 文案含非法字符",
                def.name
            );
        }
        assert_eq!(seen.len(), CATALOG.len());
        // 抽样钉住几个「运维真在看」的名字，防止被误改
        for name in [
            AUTH_CACHE_DEGRADED,
            TOKEN_BLACKLIST_ROWS,
            INVARIANT_OK,
            IMPEXP_BACKLOG_PENDING,
        ] {
            assert!(is_registered(name), "{name} 应从 CATALOG 删除前先改告警规则");
        }
    }

    /// 采样器真正写入的每一个名字都必须在目录里 —— 否则「暴露出来了但没有说明」。
    #[test]
    fn every_sampler_metric_is_registered() {
        let emitted = crate::task::metrics_sampler::Snapshot::default().gauges();
        assert!(!emitted.is_empty(), "采样器不应一个指标都不写");
        for (name, _) in emitted {
            assert!(
                is_registered(name),
                "采样器写入了未登记的指标 {name}：请补到 observability::CATALOG"
            );
        }
    }

    /// 进程内只装一次 recorder（全局只能有一个）。
    fn ensure_recorder() -> bool {
        static INSTALLED: OnceLock<bool> = OnceLock::new();
        *INSTALLED.get_or_init(|| {
            let cfg = AppConfig::default();
            // 已有 recorder 时返回 Err，忽略即可（句柄仍是同一个）
            let _ = nik::metrics::install_prometheus(&cfg.metrics);
            true
        })
    }

    /// 起一个只带 `/metrics` 的真实 HTTP 服务（回环随机端口）。
    async fn serve_metrics() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        ensure_recorder();

        let mut cfg = AppConfig::default();
        cfg.metrics.enabled = true;

        let app = App::with_config(cfg)
            .expect("build app")
            .get("/ping", || async { "pong" })
            .into_router();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        (addr, handle)
    }

    /// 发一条最小 HTTP/1.1 请求（不引第三方客户端，直接走 TCP）。
    async fn http_get(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for (key, value) in headers {
            request.push_str(&format!("{key}: {value}\r\n"));
        }
        request.push_str("\r\n");
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

    /// 验收主用例：回环抓取 → 200 + Prometheus 文本 + 至少两个 `cloudpivot_*` 指标带 HELP/TYPE。
    #[tokio::test]
    async fn metrics_endpoint_serves_prometheus_text() {
        // 先起服务（`serve_metrics` 内部会装 recorder）—— recorder 未装时
        // `metrics::gauge!` 是 no-op，先写后装等于白写。
        let (addr, server) = serve_metrics().await;

        // 模拟 /health 与清理任务已经写过一次的两条身份相关 gauge：
        // 「没有序列」的指标不会出现在导出文本里，必须真的写一次。
        set_gauge(AUTH_CACHE_DEGRADED, 0.0);
        set_gauge(TOKEN_BLACKLIST_ROWS, 7.0);

        let (status, body) = http_get(addr, "/metrics", &[]).await;
        server.abort();

        assert_eq!(status, 200, "回环来源应可抓取，实际响应：{body}");
        assert!(body.contains("# HELP"), "缺少 # HELP 行：{body}");
        assert!(body.contains("# TYPE"), "缺少 # TYPE 行：{body}");

        // 至少两个 cloudpivot_* 指标名（卡面验收口径）
        let cloudpivot_names: Vec<&str> = body
            .lines()
            .filter_map(|line| line.split('#').next())
            .filter_map(|line| line.split_whitespace().next())
            .filter(|name| name.starts_with("cloudpivot_"))
            .collect();
        assert!(
            cloudpivot_names.len() >= 2,
            "应至少导出两个 cloudpivot_* 指标，实际 {cloudpivot_names:?}"
        );

        // HELP / TYPE 必须跟着业务指标一起出来（这就是本模块存在的理由）
        assert!(
            body.contains(&format!("# HELP {AUTH_CACHE_DEGRADED} ")),
            "业务指标缺 HELP：{body}"
        );
        assert!(
            body.contains(&format!("# TYPE {AUTH_CACHE_DEGRADED} gauge")),
            "业务指标缺 TYPE：{body}"
        );
        assert!(
            body.contains(&format!("{TOKEN_BLACKLIST_ROWS} 7")),
            "gauge 值应出现在导出文本里：{body}"
        );
    }

    /// 访问控制：非白名单来源（这里用 X-Forwarded-For 伪造外部 IP）必须 403。
    #[tokio::test]
    async fn metrics_endpoint_rejects_untrusted_client() {
        let (addr, server) = serve_metrics().await;
        let (status, body) = http_get(addr, "/metrics", &[("X-Forwarded-For", "203.0.113.9")]).await;
        server.abort();

        assert_eq!(status, 403, "非白名单来源必须 403，实际响应：{body}");
        assert!(body.trim().is_empty(), "403 不应泄露端点内容：{body}");
    }
}

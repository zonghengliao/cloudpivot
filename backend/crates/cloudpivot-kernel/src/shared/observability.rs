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

/// 指标类型（只登记本仓真正用到的三种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// 瞬时值（可升可降）。
    Gauge,
    /// 单调累计值（进程重启归零）。
    Counter,
    /// 观测值分布（渲染成 `_bucket` / `_sum` / `_count` 三条序列）。
    Histogram,
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

    const fn histogram(name: &'static str, unit: &'static str, help: &'static str) -> Self {
        Self {
            name,
            kind: MetricKind::Histogram,
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

// ==================== HTTP 侧（P1-06，埋点见 `middleware::http_metrics`） ====================
//
// 为什么框架已有 `nik_http_*` 还要这四条：两者标签集**不同**，回答的问题也不同。
//
// | 需求 | `nik_http_*`（框架） | `cloudpivot_http_*`（本仓） |
// |------|----------------------|------------------------------|
// | 请求数 | `{method, route, status}`，`status` 只到**类别**（`2xx`/`4xx`/`5xx`） | `{method, path, status}`，`status` 是**精确码**（`404` / `422` / `500` 分开） |
// | 延迟 | `{method, route}`，桶 0.001~5s | `{path}`，桶同上（同一套 recorder 桶边界） |
// | 错误率 | 只能 `sum by (status="5xx")` 求和 | `cloudpivot_http_errors_total` 直接可用 `rate()` 告警 |
// | `/health` | **排除**（探针不污染 SLO） | **计入**（探针流量也是真实流量，验收要压它看增长） |
//
// 精确码是本卡真正多出来的信息：`nik_*` 把 400/401/403/404/422 混成一个 `4xx`，
// 排障时要靠翻日志才知道是哪个码。代价是每请求 2~3 次原子自增 + 1 次直方图观察
// （百纳秒量级，相对一次 PG 往返可忽略）。

/// HTTP 请求数（按 method + 归一化 path + **精确** status 码）。
pub const HTTP_REQUESTS_TOTAL: &str = "cloudpivot_http_requests_total";
/// HTTP 请求耗时（秒，直方图，按归一化 path）。
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "cloudpivot_http_request_duration_seconds";
/// HTTP 服务端错误数（`status >= 500`），**不带标签**。
///
/// 为什么不带标签：这条指标的用途是「错误率分子」（`rate(...)/rate(requests_total)`），
/// 带 `path` 只是把同一份信息再切一遍；而按端点的 5xx 明细在
/// [`HTTP_REQUESTS_TOTAL`] 里已经有了（`status="500"` / `"502"` …）。
/// 不带标签还有个直接好处：启动时就能存在一条 **0 序列**（见 [`seed_zero_series`]），
/// 运维 `grep errors_total` 不会因为「这一分钟刚好没有 5xx」而以为指标没上线。
pub const HTTP_ERRORS_TOTAL: &str = "cloudpivot_http_errors_total";

// ==================== DB 连接池（P1-06，每分钟由采样器刷新） ====================

/// 数据库连接数，按 `state` 拆分：`idle` 空闲 / `active` 执行中 / `total` 合计。
pub const DB_POOL_CONNECTIONS: &str = "cloudpivot_db_pool_connections";
/// 连接池配置的上限（`[database.pool].max_connections`），用于算使用率。
pub const DB_POOL_MAX_CONNECTIONS: &str = "cloudpivot_db_pool_max_connections";

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
    MetricDef::counter(
        HTTP_REQUESTS_TOTAL,
        "次",
        "HTTP 请求数，按 method / 归一化 path / 精确 status 码统计（中间件层观测，含 401/403/429 与 404 兜底）",
    ),
    MetricDef::histogram(
        HTTP_REQUEST_DURATION_SECONDS,
        "秒",
        "HTTP 请求端到端耗时（秒），按归一化 path 分桶；桶边界 0.01/0.05/0.1/0.5/1/5 秒均在其中",
    ),
    MetricDef::counter(
        HTTP_ERRORS_TOTAL,
        "次",
        "HTTP 服务端错误数（status >= 500），进程启动即为 0；按端点的 5xx 明细见 cloudpivot_http_requests_total 的 status 标签",
    ),
    MetricDef::gauge(
        DB_POOL_CONNECTIONS,
        "个",
        "数据库连接数，state=idle 空闲 / active 执行中 / total 合计（服务端 pg_stat_activity 口径，每分钟采样）",
    ),
    MetricDef::gauge(
        DB_POOL_MAX_CONNECTIONS,
        "个",
        "连接池配置的并发上限（database.pool.max_connections），与 cloudpivot_db_pool_connections 相除即使用率",
    ),
];

/// 按名字查目录（O(n)，n≈24；只在写指标时用，不在请求热路径上）。
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
                MetricKind::Histogram => metrics::describe_histogram!(
                    def.name,
                    metrics::Unit::Seconds,
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

/// 带标签的标签切片类型（**值**必须是 `'static` —— `metrics` 的标签不复制字符串；
/// 切片本身可以借用，因此热路径上可以直接写 `&[("state", state)]` 而不分配）。
pub type Labels<'a> = &'a [(&'static str, &'static str)];

/// 写入一个带标签的业务 gauge（自动带 `# HELP` / `# TYPE`）。
///
/// 用于「一条指标多种状态」的场景（如 `cloudpivot_db_pool_connections{state="idle"}`）：
/// 拆成多标签序列而不是多个指标名，Prometheus 侧才能用 `sum by (state)` 聚合。
pub fn set_gauge_labeled(name: &'static str, labels: Labels<'_>, value: f64) {
    debug_assert!(
        is_registered(name),
        "指标 {name} 未登记在 observability::CATALOG，导出文本会缺 # HELP / # TYPE"
    );
    describe_once();
    metrics::gauge!(name, labels).set(value);
}

/// 累加一个带标签的业务 counter（自动带 `# HELP` / `# TYPE`）。
pub fn inc_counter_labeled(name: &'static str, labels: Labels<'_>, delta: u64) {
    debug_assert!(
        is_registered(name),
        "指标 {name} 未登记在 observability::CATALOG，导出文本会缺 # HELP / # TYPE"
    );
    describe_once();
    metrics::counter!(name, labels).increment(delta);
}

/// 记录一次带标签的业务直方图观测值（自动带 `# HELP` / `# TYPE`）。
///
/// 桶边界由 recorder 全局设定（`nik-web/src/metrics.rs` 的 `BUCKETS`：
/// 0.001 / 0.005 / 0.01 / 0.025 / 0.05 / 0.1 / 0.25 / 0.5 / 1 / 2.5 / 5 秒），
/// 卡面要求的 0.01/0.05/0.1/0.5/1/5 全在其中。
pub fn observe_histogram(name: &'static str, labels: Labels<'_>, value: f64) {
    debug_assert!(
        is_registered(name),
        "指标 {name} 未登记在 observability::CATALOG，导出文本会缺 # HELP / # TYPE"
    );
    describe_once();
    metrics::histogram!(name, labels).record(value);
}

/// 让「只在异常时才累加」的 counter 从启动起就有一条 **0 序列**（幂等）。
///
/// # 为什么必须补这个
///
/// Prometheus 文本格式**不导出没有任何数据点的指标**，连 `# HELP` / `# TYPE` 都不出现。
/// 于是「5xx 计数」这类指标在系统健康时完全看不到 —— 运维 curl `/metrics` 会以为
/// 指标根本没上线（P1-05 的 `cloudpivot_token_blacklist_cleanup_deleted` 就是这样消失的）。
///
/// `increment(0)` 会在 recorder 里建出这条序列且不改变数值，因此对本指标语义零影响。
///
/// # 调用点
///
/// - `task::metrics_sampler::run_once`：启动后秒级必跑（P1-05 补的「注册时立即采样」）；
/// - `middleware::http_metrics::track`：首个请求（`Once` 后的原子读，零成本），
///   保证「进程起来但采样器还没跑」的窗口里也有序列。
///
/// 注意：**只能在 recorder 安装之后调用**，否则 `metrics` 会静默丢弃（P1-05 的坑）。
pub fn seed_zero_series() {
    static SEEDED: Once = Once::new();
    SEEDED.call_once(|| {
        inc_counter(HTTP_ERRORS_TOTAL, 0);
    });
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

    // 跨轴一致性用例（采样器 ⇄ 指标目录）已搬到应用侧：`src/kernel_axis_tests.rs`
    //（P2-59 · C1 —— 内核不能反向依赖 `task`，否则是包级环）。

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

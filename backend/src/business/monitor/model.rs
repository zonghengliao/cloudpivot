//! 服务器指标的采集与解析（数据访问 + 纯逻辑）。
//!
//! # 采集口径
//!
//! | 指标 | 来源 | 说明 |
//! |------|------|------|
//! | CPU | `/proc/stat` 首行 | 间隔 [`CPU_SAMPLE_INTERVAL`] 采两次，按时间片差值算使用率 |
//! | 内存 | `/proc/meminfo` | `MemTotal` / `MemAvailable`（缺失时退化为 Free+Buffers+Cached） |
//! | 磁盘 | `df -P -k /` | 不引入 `libc`，解析 POSIX 输出；失败降级为 0 |
//! | 数据库 | 现有连接 | `current_database()` / `pg_database_size()` / `pg_stat_activity` / `version()` |
//! | Redis | `INFO` | 使用与 `nik-cache` 同版本的 redis 客户端，连接失败降级为 `down` |
//!
//! 所有采集函数都**不返回错误**：单项失败只把该指标降级为 0 / `down`，接口整体保持 200。
//! 解析与格式化逻辑拆成纯函数，便于单测覆盖除零、空输入等边界。

use nik::prelude::*;

use std::time::{Duration, Instant};

use crate::business::monitor::dto::{
    AppRes, BusinessRes, CpuRes, DatabaseRes, HermesProbeRes, HostRes, RedisRes, ServerStatusRes, UsageRes,
};

/// `/proc/stat` 路径。
const PROC_STAT: &str = "/proc/stat";
/// `/proc/meminfo` 路径。
const PROC_MEMINFO: &str = "/proc/meminfo";
/// CPU 两次采样的间隔（决定使用率窗口宽度）。
const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// 数据库版本号下发时的最大字符数（`version()` 全文含编译参数，太长且无展示价值）。
const DB_VERSION_MAX_CHARS: usize = 40;
/// 统计「今日」时使用的时区（与 `support::dashboard::model` 的自然日口径一致）。
const REPORT_TIME_ZONE: &str = "Asia/Shanghai";
/// Hermes Agent 端口默认探测地址。
///
/// 当前生产形态里 Agent 不在本机，所以这里探测失败是**正常状态**（前端显示「未安装」）。
/// 可用环境变量 `CLOUDPIVOT_HERMES_PROBE_ADDR` 覆盖（例如 Agent 与云枢同机部署时改端口）。
const HERMES_PROBE_ADDR_DEFAULT: &str = "127.0.0.1:8100";
/// Hermes 探测超时：连接被防火墙丢弃时不至于把整页拖慢到秒级。
const HERMES_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

// ==================== 采集入口 ====================

/// 采集一次完整的服务器状态。
///
/// # 返回值
///
/// 始终返回 [`ServerStatusRes`]：任一子项采集失败时该子项为默认值（`down` / 0），
/// 由前端展示为 `-`，不会让接口失败。
pub async fn collect() -> ServerStatusRes {
    ServerStatusRes {
        cpu: collect_cpu().await,
        memory: collect_memory().await,
        disk: collect_disk(),
        database: collect_database().await,
        redis: collect_redis().await,
        collected_at: chrono::Utc::now().to_rfc3339(),
        host: collect_host().await,
        app: collect_app(),
        business: collect_business().await,
        hermes: probe_hermes().await,
    }
}

/// 采集 CPU 使用率：两次 `/proc/stat` 快照的差值。
async fn collect_cpu() -> CpuRes {
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get() as u32);
    let Some(prev) = read_cpu_times().await else {
        return CpuRes { percent: 0.0, cores };
    };
    tokio::time::sleep(CPU_SAMPLE_INTERVAL).await;
    let Some(curr) = read_cpu_times().await else {
        return CpuRes { percent: 0.0, cores };
    };
    CpuRes {
        percent: round1(cpu_percent(prev, curr)),
        cores,
    }
}

/// 读取并解析一次 `/proc/stat` 的 CPU 时间片快照。
async fn read_cpu_times() -> Option<CpuTimes> {
    tokio::fs::read_to_string(PROC_STAT)
        .await
        .ok()
        .and_then(|text| parse_proc_stat(&text))
}

/// 采集内存使用情况。
async fn collect_memory() -> UsageRes {
    let Ok(text) = tokio::fs::read_to_string(PROC_MEMINFO).await else {
        return UsageRes::default();
    };
    let Some(mem) = parse_meminfo(&text) else {
        return UsageRes::default();
    };
    let used = mem.total_bytes.saturating_sub(mem.available_bytes);
    usage(used, mem.total_bytes)
}

/// 采集根分区磁盘使用情况（`df -P -k /`，失败降级为 0）。
fn collect_disk() -> UsageRes {
    let Ok(output) = std::process::Command::new("df").args(["-P", "-k", "/"]).output() else {
        return UsageRes::default();
    };
    if !output.status.success() {
        return UsageRes::default();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(disk) = parse_df(&text) else {
        return UsageRes::default();
    };
    usage(disk.used_bytes, disk.total_bytes)
}

/// 采集数据库状态：库名 / 大小 / 连接数 / 版本。
///
/// 查询失败（连接不可用、SQL 出错）时返回 `state = "down"`，不向上抛错。
async fn collect_database() -> DatabaseRes {
    const SQL: &str = r#"
        SELECT current_database()                                             AS db_name,
               pg_database_size(current_database())::bigint                    AS db_size,
               (SELECT COUNT(*) FROM pg_stat_activity)::bigint                 AS db_conns,
               version()                                                       AS db_version,
               current_setting('server_version')                               AS server_version
    "#;

    let started = Instant::now();
    let row = match db().find_first(SQL).await {
        Ok(Some(row)) => row,
        Ok(None) | Err(_) => {
            return DatabaseRes {
                state: "down".to_string(),
                ..DatabaseRes::default()
            };
        }
    };
    let latency_ms = duration_ms(started.elapsed());

    let size_bytes = row.get_typed::<i64>("db_size").unwrap_or_default().max(0) as u64;
    // 迁移条数单独一条 SQL：`schema_migrations` 由迁移器自建，任何异常都只让这一格为 0，
    // 绝不能因为它把整个数据库状态判成 down（老库/只读副本上这张表可能存在但无权限）。
    let migrations = db()
        .scalar::<i64>("SELECT COUNT(*)::bigint FROM schema_migrations WHERE success")
        .await
        .unwrap_or(0);
    DatabaseRes {
        state: "up".to_string(),
        name: row.get_typed::<String>("db_name").unwrap_or_default(),
        size_bytes,
        size_text: format_bytes(size_bytes),
        connections: row.get_typed::<i64>("db_conns").unwrap_or_default(),
        version: truncate_chars(
            &row.get_typed::<String>("db_version").unwrap_or_default(),
            DB_VERSION_MAX_CHARS,
        ),
        server_version: row
            .get_typed::<String>("server_version")
            .unwrap_or_default()
            // `current_setting('server_version')` 在 Ubuntu 包上是 `18.6 (Ubuntu 18.6-0ubuntu0.26.04.1)`：
            // 环境信息卡只要「18.6」这一段（完整串在 `version` 里），故取第一个空白前的部分。
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
        latency_ms: Some(latency_ms),
        migrations,
    }
}

/// 采集 Redis 状态（`INFO`）；连接或命令失败返回 `state = "down"`。
async fn collect_redis() -> RedisRes {
    let started = Instant::now();
    let Some(info) = fetch_redis_info(&cfg().redis.url).await else {
        return RedisRes {
            state: "down".to_string(),
            ..RedisRes::default()
        };
    };
    let latency_ms = duration_ms(started.elapsed());
    let parsed = parse_redis_info(&info);
    RedisRes {
        state: "up".to_string(),
        version: parsed.version,
        used_memory_text: parsed.used_memory_text,
        used_memory_bytes: parsed.used_memory_bytes,
        connected_clients: parsed.connected_clients,
        uptime_seconds: parsed.uptime_seconds,
        keyspace_hits: parsed.keyspace_hits,
        keyspace_misses: parsed.keyspace_misses,
        latency_ms: Some(latency_ms),
    }
}

/// 对 Redis 执行 `INFO` 并取回原始文本。
///
/// 使用与 `nik-cache` 相同版本的 redis 客户端（见 `Cargo.toml` 注释）；
/// 任何失败都折叠成 `None`，由调用方降级。
async fn fetch_redis_info(url: &str) -> Option<String> {
    let client = redis::Client::open(url).ok()?;
    let mut conn = client.get_multiplexed_async_connection().await.ok()?;
    redis::cmd("INFO").query_async::<String>(&mut conn).await.ok()
}

// ==================== 主机 / 进程 / 业务量 ====================

/// 采集主机信息（hostname / 发行版 / 内核 / CPU 型号 / 负载 / 开机时间）。
///
/// 任何一项读不到都用空串 / 0 兜底，**不返回错误**：这台机器上 `/proc` 一定在，
/// 但容器里可能读不到 `/etc/os-release`，那也只该让那一格显示 `-`。
async fn collect_host() -> HostRes {
    let os_release = tokio::fs::read_to_string("/etc/os-release").await.unwrap_or_default();
    let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await.unwrap_or_default();
    let loadavg = tokio::fs::read_to_string("/proc/loadavg").await.unwrap_or_default();
    let uptime = tokio::fs::read_to_string("/proc/uptime").await.unwrap_or_default();
    let kernel = tokio::fs::read_to_string("/proc/sys/kernel/osrelease")
        .await
        .unwrap_or_default();
    let hostname_file = tokio::fs::read_to_string("/etc/hostname").await.unwrap_or_default();

    let hostname = if hostname_file.trim().is_empty() {
        std::env::var("HOSTNAME").unwrap_or_default()
    } else {
        hostname_file.trim().to_string()
    };
    let boot_epoch = read_boot_epoch();
    let (load1, load5, load15) = parse_loadavg(&loadavg);
    let cpu = parse_cpuinfo(&cpuinfo);

    HostRes {
        hostname,
        os_name: parse_os_release(&os_release),
        kernel: kernel.trim().to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_model: cpu.model,
        cpu_mhz: cpu.mhz,
        load1,
        load5,
        load15,
        uptime_seconds: parse_uptime(&uptime),
        boot_time: boot_epoch
            .and_then(|secs| chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
    }
}

/// 采集后端进程信息（版本 / commit / profile / 启动时间 / 运行时长）。
///
/// 启动时间取自 `/proc/self/stat` 的 `starttime` + `/proc/stat` 的 `btime`，
/// 而不是「进程内第一次访问的时间」—— 后者会把「服务已经跑了 3 天」显示成「刚刚启动」。
fn collect_app() -> AppRes {
    let started_epoch = process_start_epoch();
    let now = chrono::Utc::now();
    let uptime_seconds = started_epoch.map_or(0, |secs| (now.timestamp() - secs).max(0));
    AppRes {
        version: env!("CARGO_PKG_VERSION").to_string(),
        commit: option_env!("CLOUDPIVOT_GIT_COMMIT").unwrap_or("unknown").to_string(),
        rust_version: option_env!("CLOUDPIVOT_RUSTC_VERSION").unwrap_or("unknown").to_string(),
        profile: cfg().profile.clone(),
        started_at: started_epoch
            .and_then(|secs| chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| now.to_rfc3339()),
        uptime_seconds,
        pid: std::process::id(),
    }
}

/// 采集业务量（用户总数 / 在线会话 / 今日登录 / 今日操作）。
///
/// # 为什么不返回错误
///
/// 这是运维页面上的一块「概览」，数据库抖动时应该显示 `-` 而不是让整页 500：
/// 用户在服务器状态页最需要看到的信息恰恰是「数据库挂了」，此时把页面打成错误页
/// 等于隐藏了最关键的那条信息。因此任何一项查询失败都只让该项为 0。
async fn collect_business() -> BusinessRes {
    let sql = format!(
        r#"
WITH scope AS (
    SELECT (date_trunc('day', now() AT TIME ZONE '{tz}'))::timestamptz AS day_start,
           ((date_trunc('day', now() AT TIME ZONE '{tz}')) + interval '1 day')::timestamptz AS day_end
)
SELECT (SELECT COUNT(*) FROM sys_user WHERE deleted_at IS NULL)::bigint AS users_total,
       (SELECT COUNT(*) FROM auth_logs a, scope
         WHERE a.auth_type = 'LOGIN'
           AND a.created_at >= scope.day_start AND a.created_at < scope.day_end)::bigint AS login_today,
       (SELECT COUNT(*) FROM operation_logs o, scope
         WHERE o.created_at >= scope.day_start AND o.created_at < scope.day_end)::bigint AS op_log_today
"#,
        tz = REPORT_TIME_ZONE
    );

    let (users_total, login_today, op_log_today) = match db().find_first(&sql).await {
        Ok(Some(row)) => (
            row.get_typed::<i64>("users_total").unwrap_or_default(),
            row.get_typed::<i64>("login_today").unwrap_or_default(),
            row.get_typed::<i64>("op_log_today").unwrap_or_default(),
        ),
        _ => (0, 0, 0),
    };

    // 在线会话来自缓存（登录 / 刷新时登记，登出不落库）；缓存未启用时按 0 处理。
    let online_users = crate::support::online::model::OnlineSessionModel::list_all()
        .await
        .map(|list| list.len() as i64)
        .unwrap_or(0);

    BusinessRes {
        users_total,
        online_users,
        login_today,
        op_log_today,
    }
}

/// 探测 Hermes Agent 端口（TCP 连接，300 ms 超时）。
///
/// # 语义
///
/// - 连得上 → `state = up`（Agent 与本服务同机且已监听）；
/// - 连不上 → `state = not_installed`（**正常状态**：当前部署形态里 Agent 在别的主机）。
///
/// 探测地址可用 `CLOUDPIVOT_HERMES_PROBE_ADDR` 覆盖。
async fn probe_hermes() -> HermesProbeRes {
    let address = std::env::var("CLOUDPIVOT_HERMES_PROBE_ADDR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| HERMES_PROBE_ADDR_DEFAULT.to_string());

    let started = Instant::now();
    let outcome = tokio::time::timeout(HERMES_PROBE_TIMEOUT, tokio::net::TcpStream::connect(&address)).await;
    match outcome {
        Ok(Ok(_stream)) => HermesProbeRes {
            state: "up".to_string(),
            detail: "端口可连接".to_string(),
            latency_ms: Some(duration_ms(started.elapsed())),
            address,
        },
        Ok(Err(_)) => HermesProbeRes {
            state: "not_installed".to_string(),
            detail: format!("{address} 无服务监听（Agent 未部署在本机或未启动）"),
            latency_ms: None,
            address,
        },
        Err(_) => HermesProbeRes {
            state: "not_installed".to_string(),
            detail: format!("{address} 连接超时（{} ms）", HERMES_PROBE_TIMEOUT.as_millis()),
            latency_ms: None,
            address,
        },
    }
}

// ==================== 纯逻辑（含单测） ====================

/// 纳秒级耗时转毫秒（保留 1 位小数）。
fn duration_ms(elapsed: Duration) -> f64 {
    round1(elapsed.as_secs_f64() * 1000.0)
}

/// 读取系统开机时间戳（`/proc/stat` 的 `btime <epoch>`）。
fn read_boot_epoch() -> Option<i64> {
    let text = std::fs::read_to_string(PROC_STAT).ok()?;
    parse_btime(&text)
}

/// 解析 `/proc/stat` 的 `btime` 行。
pub fn parse_btime(text: &str) -> Option<i64> {
    text.lines()
        .find(|line| line.starts_with("btime "))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<i64>().ok())
}

/// 当前进程的启动时间戳（Unix 秒）。
///
/// `/proc/self/stat` 的第 22 个字段 `starttime` 是**自开机起的时间片数**
/// （单位 USER_HZ，内核固定为 100），加上 `btime` 就是绝对时间。
///
/// # 为什么不用 `SystemTime::now()` 缓存一个「首次调用时间」
///
/// 那样得到的只是「第一次访问本页的时间」，服务重启后并不重置、服务长期运行后
/// 又会显示成「刚刚启动」，与用户要看的「服务运行时长」正好相反。
fn process_start_epoch() -> Option<i64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let ticks = parse_proc_start_ticks(&stat)?;
    let boot = read_boot_epoch()?;
    Some(boot + ticks / 100)
}

/// 从 `/proc/<pid>/stat` 文本解析 `starttime`（第 22 个字段，单位 jiffies）。
///
/// 进程名可能包含空格与括号（如 `(my app)`），因此必须从**最后一个 `)` 之后**
/// 开始数字段，不能简单 `split_whitespace`。
pub fn parse_proc_start_ticks(stat: &str) -> Option<i64> {
    let close = stat.rfind(')')?;
    let rest = stat.get(close + 1..)?.trim_start();
    // 解构后第 1 个字段是 state（总第 3 个字段），故 starttime 的偏移是 22 - 3 = 19。
    rest.split_whitespace().nth(19)?.parse::<i64>().ok()
}

/// 解析 `/etc/os-release` 的 `PRETTY_NAME`（去掉包裹的引号）。
pub fn parse_os_release(text: &str) -> String {
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "PRETTY_NAME" {
            continue;
        }
        return value.trim().trim_matches('"').trim_matches('\'').to_string();
    }
    String::new()
}

/// `/proc/cpuinfo` 中要下发的那两列。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuInfo {
    /// CPU 型号（x86 的 `model name`；ARM 的 `Hardware` / `Model`）。
    pub model: String,
    /// 主频 MHz（取第一个 `cpu MHz`；ARM 上通常没有，为 0）。
    pub mhz: u32,
}

/// 解析 `/proc/cpuinfo`。
///
/// 型号优先 `model name`（x86），退回 `Hardware` / `Model`（ARM / 树莓派）。
/// 主频按第一行的 `cpu MHz` 四舍五入到整数；缺失时为 0（前端只显示核心数）。
pub fn parse_cpuinfo(text: &str) -> CpuInfo {
    let mut info = CpuInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "model name" if info.model.is_empty() => info.model = value.to_string(),
            "Hardware" | "Model" if info.model.is_empty() => info.model = value.to_string(),
            "cpu MHz" if info.mhz == 0 => {
                info.mhz = value.parse::<f64>().map(|mhz| mhz.round() as u32).unwrap_or(0);
            }
            _ => {}
        }
    }
    info
}

/// 解析 `/proc/loadavg` 的前三个数（1 / 5 / 15 分钟平均负载）。
///
/// 字段缺失或非数字时该位置为 0.0，不报错。
pub fn parse_loadavg(text: &str) -> (f64, f64, f64) {
    let values: Vec<f64> = text
        .split_whitespace()
        .take(3)
        .map(|field| field.parse::<f64>().unwrap_or(0.0))
        .collect();
    (
        values.first().copied().unwrap_or(0.0),
        values.get(1).copied().unwrap_or(0.0),
        values.get(2).copied().unwrap_or(0.0),
    )
}

/// 解析 `/proc/uptime` 的第一个数（秒，截断为整数）。
pub fn parse_uptime(text: &str) -> i64 {
    text.split_whitespace()
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .map(|secs| secs.max(0.0) as i64)
        .unwrap_or(0)
}

/// `/proc/stat` 首行的一条 CPU 时间片快照（单位：jiffies）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    /// 非空闲时间片（total - idle - iowait）。
    pub busy: u64,
    /// 全部时间片之和。
    pub total: u64,
}

/// 解析 `/proc/stat` 首行，得到 CPU 时间片快照。
///
/// # 返回值
///
/// 首行不是 `cpu` 汇总行、字段不足 8 个或含非数字字段时返回 `None`。
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.parse::<u64>().ok())
        .collect();
    // 至少要有 user nice system idle iowait irq softirq steal 八列，后面的 guest 列可缺
    if values.len() < 8 {
        return None;
    }
    let total: u64 = values.iter().take(8).sum();
    let idle = values[3].saturating_add(values[4]);
    Some(CpuTimes {
        busy: total.saturating_sub(idle),
        total,
    })
}

/// 按两次快照的时间片差值计算 CPU 使用率（0-100）。
///
/// # 边界
///
/// 两次快照相同时（total 差值为 0）返回 `0.0`，不做除法，避免除零 panic。
pub fn cpu_percent(prev: CpuTimes, curr: CpuTimes) -> f64 {
    let total_delta = curr.total.saturating_sub(prev.total);
    if total_delta == 0 {
        return 0.0;
    }
    let busy_delta = curr.busy.saturating_sub(prev.busy);
    (busy_delta as f64 * 100.0 / total_delta as f64).clamp(0.0, 100.0)
}

/// 内存快照（字节）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemSnapshot {
    /// 物理内存总量。
    pub total_bytes: u64,
    /// 可用内存。
    pub available_bytes: u64,
}

/// 解析 `/proc/meminfo`。
///
/// `MemAvailable` 缺失（老内核）时退化为 `MemFree + Buffers + Cached`。
///
/// # 返回值
///
/// 缺少 `MemTotal` 或可用内存三项全缺时返回 `None`。
pub fn parse_meminfo(text: &str) -> Option<MemSnapshot> {
    let mut total_kb: Option<u64> = None;
    let mut available_kb: Option<u64> = None;
    let mut fallback_kb = 0_u64;
    let mut fallback_hits = 0_u8;

    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        match key.trim() {
            "MemTotal" => total_kb = Some(value),
            "MemAvailable" => available_kb = Some(value),
            "MemFree" | "Buffers" | "Cached" => {
                fallback_kb = fallback_kb.saturating_add(value);
                fallback_hits = fallback_hits.saturating_add(1);
            }
            _ => {}
        }
    }

    let total_kb = total_kb?;
    let available_kb = match available_kb {
        Some(value) => value,
        // 三项必须都在才会有满 3 次命中，否则视为无法判定可用内存
        None if fallback_hits >= 3 => fallback_kb,
        None => return None,
    };
    Some(MemSnapshot {
        total_bytes: total_kb.saturating_mul(1024),
        available_bytes: available_kb.saturating_mul(1024),
    })
}

/// 磁盘快照（字节）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskSnapshot {
    /// 总容量。
    pub total_bytes: u64,
    /// 已用容量。
    pub used_bytes: u64,
}

/// 解析 `df -P -k` 输出（取最后一行数据行）。
///
/// # 返回值
///
/// 缺少数据行、列数不足或列值非数字时返回 `None`。
pub fn parse_df(text: &str) -> Option<DiskSnapshot> {
    let line = text.lines().rfind(|line| !line.trim().is_empty())?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 4 {
        return None;
    }
    let total_kb = cols[1].parse::<u64>().ok()?;
    let used_kb = cols[2].parse::<u64>().ok()?;
    Some(DiskSnapshot {
        total_bytes: total_kb.saturating_mul(1024),
        used_bytes: used_kb.saturating_mul(1024),
    })
}

/// Redis `INFO` 中需要下发的字段。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedisInfo {
    /// 版本号。
    pub version: String,
    /// 已用内存文本（Redis 自报单位）。
    pub used_memory_text: String,
    /// 已用内存字节数。
    pub used_memory_bytes: u64,
    /// 客户端连接数。
    pub connected_clients: i64,
    /// 运行时长（秒）。
    pub uptime_seconds: i64,
    /// 命中次数。
    pub keyspace_hits: i64,
    /// 未命中次数。
    pub keyspace_misses: i64,
}

/// 解析 Redis `INFO` 文本。
///
/// 只认 `key:value` 行，`# Section` 与空行直接跳过；缺失字段保留默认值（0 / 空串），
/// 因此空输入也返回全默认结构而不报错。
pub fn parse_redis_info(text: &str) -> RedisInfo {
    let mut info = RedisInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "redis_version" => info.version = value.to_string(),
            "used_memory_human" => info.used_memory_text = value.to_string(),
            "used_memory" => info.used_memory_bytes = value.parse().unwrap_or_default(),
            "connected_clients" => info.connected_clients = value.parse().unwrap_or_default(),
            "uptime_in_seconds" => info.uptime_seconds = value.parse().unwrap_or_default(),
            "keyspace_hits" => info.keyspace_hits = value.parse().unwrap_or_default(),
            "keyspace_misses" => info.keyspace_misses = value.parse().unwrap_or_default(),
            _ => {}
        }
    }
    info
}

/// 组装容量响应（百分比 + 字节 + GB 文本）。
pub fn usage(used_bytes: u64, total_bytes: u64) -> UsageRes {
    UsageRes {
        percent: round1(usage_percent(used_bytes, total_bytes)),
        used_bytes,
        total_bytes,
        used_text: format_bytes(used_bytes),
        total_text: format_bytes(total_bytes),
    }
}

/// 计算使用率百分比（0-100）。
///
/// # 边界
///
/// `total == 0`（未采集到容量）时返回 `0.0`，不做除法。
pub fn usage_percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (used as f64 * 100.0 / total as f64).clamp(0.0, 100.0)
}

/// 字节数转人类可读文本：≥ 1 GB 用 GB（1 位小数），否则用 MB（整数）。
pub fn format_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * MB;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else {
        format!("{:.0} MB", value / MB)
    }
}

/// 保留 1 位小数。
fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// 按字符数截断（避免在多字节边界切分）。
fn truncate_chars(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_string();
    }
    input.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proc_stat_normal() {
        // cpu user nice system idle iowait irq softirq steal
        let text = "cpu  100 20 30 800 50 10 5 5 0 0\ncpu0 1 2 3 4 5 6 7 8\n";
        let times = parse_proc_stat(text).expect("应能解析 CPU 汇总行");
        assert_eq!(times.total, 1020);
        // idle + iowait = 850
        assert_eq!(times.busy, 170);
    }

    #[test]
    fn cpu_percent_normal_and_zero_delta() {
        let prev = CpuTimes { busy: 100, total: 1000 };
        let curr = CpuTimes { busy: 200, total: 2000 };
        assert_eq!(cpu_percent(prev, curr), 10.0);

        // 边界：两次快照完全相同 → 不 panic，返回 0
        assert_eq!(cpu_percent(prev, prev), 0.0);
        // 边界：计数器回绕（curr < prev）→ 饱和为 0
        assert_eq!(cpu_percent(curr, prev), 0.0);
    }

    #[test]
    fn parse_proc_stat_invalid() {
        assert!(parse_proc_stat("intr 1 2 3\n").is_none());
        assert!(parse_proc_stat("cpu  1 2 3\n").is_none());
    }

    #[test]
    fn parse_meminfo_normal() {
        let text = "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    8192000 kB\nBuffers:          500000 kB\nCached:          4000000 kB\n";
        let mem = parse_meminfo(text).expect("应能解析内存");
        assert_eq!(mem.total_bytes, 16_384_000 * 1024);
        assert_eq!(mem.available_bytes, 8_192_000 * 1024);
    }

    #[test]
    fn parse_meminfo_fallback_and_missing() {
        // 边界：无 MemAvailable（老内核）→ Free + Buffers + Cached
        let fallback = "MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 150 kB\n";
        let mem = parse_meminfo(fallback).expect("回退口径应可用");
        assert_eq!(mem.available_bytes, 300 * 1024);
        // 边界：缺 MemTotal → None
        assert!(parse_meminfo("MemFree: 100 kB\n").is_none());
    }

    #[test]
    fn parse_df_normal_and_invalid() {
        let text = "Filesystem     1024-blocks    Used Available Capacity Mounted on\n/dev/sda1         1000000  250000    750000      25% /\n";
        let disk = parse_df(text).expect("应能解析 df 输出");
        assert_eq!(disk.total_bytes, 1_000_000 * 1024);
        assert_eq!(disk.used_bytes, 250_000 * 1024);
        // 边界：空输出 / 列数不足
        assert!(parse_df("").is_none());
        assert!(parse_df("Filesystem\n/dev/sda1\n").is_none());
    }

    #[test]
    fn parse_redis_info_normal_and_empty() {
        let text = "# Server\r\nredis_version:7.2.4\r\nused_memory:1048576\r\nused_memory_human:1.00M\r\nconnected_clients:3\r\nuptime_in_seconds:3600\r\nkeyspace_hits:100\r\nkeyspace_misses:7\r\n";
        let info = parse_redis_info(text);
        assert_eq!(info.version, "7.2.4");
        assert_eq!(info.used_memory_bytes, 1_048_576);
        assert_eq!(info.connected_clients, 3);
        assert_eq!(info.keyspace_misses, 7);
        // 边界：空输入返回全默认，不 panic
        assert_eq!(parse_redis_info(""), RedisInfo::default());
    }

    #[test]
    fn format_bytes_and_usage_boundaries() {
        assert_eq!(format_bytes(0), "0 MB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.0 GB");
        assert_eq!(format_bytes(64 * 1024 * 1024), "64 MB");
        // 边界：总量为 0 时不做除法
        assert_eq!(usage_percent(100, 0), 0.0);
        assert_eq!(usage_percent(1, 4), 25.0);
        // 已用大于总量（statvfs/df 口径差异）时饱和到 100
        assert_eq!(usage_percent(200, 100), 100.0);
    }

    #[test]
    fn truncate_chars_keeps_multibyte_boundary() {
        assert_eq!(truncate_chars("PostgreSQL 17.0", 11), "PostgreSQL ");
        assert_eq!(truncate_chars("数据库版本很长", 3), "数据库");
        assert_eq!(truncate_chars("short", 40), "short");
    }

    // ==================== 主机 / 进程解析 ====================

    #[test]
    fn parse_btime_and_proc_start_ticks() {
        let stat = "cpu  1 2 3 4\nbtime 1700000000\nprocesses 1234\n";
        assert_eq!(parse_btime(stat), Some(1_700_000_000));
        assert_eq!(parse_btime("cpu 1 2 3\n"), None);

        // 进程名里带空格与括号：必须从最后一个 ')' 之后数字段，
        // 否则 `(my app)` 的右括号会被当成起点，整行错位 1 段以上。
        // 构造一行真正的 stat：pid (comm) state ppid ... starttime 在第 22 个字段。
        let mut fields: Vec<String> = vec!["R".into(), "1".into()];
        // 解构后下标 0 = 第 3 个字段（state），故 starttime（第 22 个）落在下标 19：
        // 下标 2..=18 共 17 个占位，下标 19 才是 starttime。
        for _ in 0..17 {
            fields.push("0".into());
        }
        fields.push("12345".into()); // starttime（第 22 个字段）
        assert_eq!(fields.len(), 20, "占位数量必须让 starttime 落在下标 19");
        let line = format!("4242 (my app) {}", fields.join(" "));
        assert_eq!(parse_proc_start_ticks(&line), Some(12345));

        // 字段不足 / 非数字 → None，不 panic
        assert_eq!(parse_proc_start_ticks("4242 (x) R"), None);
        assert_eq!(
            parse_proc_start_ticks("4242 (x) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 abc"),
            None
        );
    }

    #[test]
    fn parse_os_release_strips_quotes() {
        let text = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.3 LTS\"\nID=ubuntu\n";
        assert_eq!(parse_os_release(text), "Ubuntu 24.04.3 LTS");
        // 单引号同样去掉；缺失返回空串（不是错误）
        assert_eq!(parse_os_release("PRETTY_NAME='Debian 12'\n"), "Debian 12");
        assert_eq!(parse_os_release("ID=debian\n"), "");
    }

    #[test]
    fn parse_cpuinfo_x86_and_arm() {
        let x86 = "processor\t: 0\nmodel name\t: Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz\ncpu MHz\t\t: 2397.123\n\nprocessor\t: 1\nmodel name\t: Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz\ncpu MHz\t\t: 2400.000\n";
        let info = parse_cpuinfo(x86);
        assert!(info.model.contains("Xeon"));
        // 取第一行主频并四舍五入，不取最后一核
        assert_eq!(info.mhz, 2397);

        // ARM：没有 model name / cpu MHz，退回 Hardware，主频为 0
        let arm = "processor\t: 0\nBogoMIPS\t: 48.00\nHardware\t: BCM2835\n";
        let info = parse_cpuinfo(arm);
        assert_eq!(info.model, "BCM2835");
        assert_eq!(info.mhz, 0);

        // 空输入 → 全默认
        assert_eq!(parse_cpuinfo(""), CpuInfo::default());
    }

    #[test]
    fn parse_loadavg_and_uptime() {
        assert_eq!(parse_loadavg("0.52 0.58 0.59 1/512 12345\n"), (0.52, 0.58, 0.59));
        // 缺字段 / 非数字：对应位置为 0，不 panic
        assert_eq!(parse_loadavg("1.5"), (1.5, 0.0, 0.0));
        assert_eq!(parse_loadavg(""), (0.0, 0.0, 0.0));

        assert_eq!(parse_uptime("12345.67 98765.43\n"), 12345);
        assert_eq!(parse_uptime(""), 0);
        // 负数（理论上不存在）按 0 处理
        assert_eq!(parse_uptime("-1.0 0.0\n"), 0);
    }

    #[test]
    fn duration_ms_rounds_to_one_decimal() {
        assert_eq!(duration_ms(Duration::from_millis(12)), 12.0);
        assert_eq!(duration_ms(Duration::from_micros(1234)), 1.2);
        assert_eq!(duration_ms(Duration::from_nanos(0)), 0.0);
    }
}

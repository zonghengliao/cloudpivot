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
//! | Redis | `INFO` | 使用与 `alun-cache` 同版本的 redis 客户端，连接失败降级为 `down` |
//!
//! 所有采集函数都**不返回错误**：单项失败只把该指标降级为 0 / `down`，接口整体保持 200。
//! 解析与格式化逻辑拆成纯函数，便于单测覆盖除零、空输入等边界。

use alun::prelude::*;

use std::time::Duration;

use crate::business::monitor::dto::{CpuRes, DatabaseRes, RedisRes, ServerStatusRes, UsageRes};

/// `/proc/stat` 路径。
const PROC_STAT: &str = "/proc/stat";
/// `/proc/meminfo` 路径。
const PROC_MEMINFO: &str = "/proc/meminfo";
/// CPU 两次采样的间隔（决定使用率窗口宽度）。
const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// 数据库版本号下发时的最大字符数（`version()` 全文含编译参数，太长且无展示价值）。
const DB_VERSION_MAX_CHARS: usize = 40;

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
    }
}

/// 采集 CPU 使用率：两次 `/proc/stat` 快照的差值。
async fn collect_cpu() -> CpuRes {
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get() as u32);
    let Some(prev) = read_cpu_times().await else {
        return CpuRes {
            percent: 0.0,
            cores,
        };
    };
    tokio::time::sleep(CPU_SAMPLE_INTERVAL).await;
    let Some(curr) = read_cpu_times().await else {
        return CpuRes {
            percent: 0.0,
            cores,
        };
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
    let Ok(output) = std::process::Command::new("df")
        .args(["-P", "-k", "/"])
        .output()
    else {
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
               version()                                                       AS db_version
    "#;

    let row = match db().find_first(SQL).await {
        Ok(Some(row)) => row,
        Ok(None) | Err(_) => {
            return DatabaseRes {
                state: "down".to_string(),
                ..DatabaseRes::default()
            };
        }
    };

    let size_bytes = row.get_typed::<i64>("db_size").unwrap_or_default().max(0) as u64;
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
    }
}

/// 采集 Redis 状态（`INFO`）；连接或命令失败返回 `state = "down"`。
async fn collect_redis() -> RedisRes {
    let Some(info) = fetch_redis_info(&cfg().redis.url).await else {
        return RedisRes {
            state: "down".to_string(),
            ..RedisRes::default()
        };
    };
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
    }
}

/// 对 Redis 执行 `INFO` 并取回原始文本。
///
/// 使用与 `alun-cache` 相同版本的 redis 客户端（见 `Cargo.toml` 注释）；
/// 任何失败都折叠成 `None`，由调用方降级。
async fn fetch_redis_info(url: &str) -> Option<String> {
    let client = redis::Client::open(url).ok()?;
    let mut conn = client.get_multiplexed_async_connection().await.ok()?;
    redis::cmd("INFO")
        .query_async::<String>(&mut conn)
        .await
        .ok()
}

// ==================== 纯逻辑（含单测） ====================

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
        let Some(value) = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        else {
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
        let prev = CpuTimes {
            busy: 100,
            total: 1000,
        };
        let curr = CpuTimes {
            busy: 200,
            total: 2000,
        };
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
}

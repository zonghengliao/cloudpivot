//! 服务器状态接口的响应结构。
//!
//! 所有字段均为**聚合数值或版本号**，不含主机地址、连接串、密码等敏感信息。

use serde::{Deserialize, Serialize};

/// CPU 使用情况。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CpuRes {
    /// 最近一次采样窗口（约 200ms）内的使用率，0-100，保留 1 位小数。
    ///
    /// 取不到 `/proc/stat` 时为 `0.0`（前端按 `-` 之外显示 0%，不报错）。
    pub percent: f64,
    /// 逻辑核心数（`available_parallelism`，取不到时 0）。
    pub cores: u32,
}

/// 内存 / 磁盘通用容量信息。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageRes {
    /// 使用率百分比（0-100，保留 1 位小数）。
    pub percent: f64,
    /// 已用字节数。
    pub used_bytes: u64,
    /// 总字节数。
    pub total_bytes: u64,
    /// 已用容量的人类可读文本（GB 口径）。
    pub used_text: String,
    /// 总容量的人类可读文本（GB 口径）。
    pub total_text: String,
}

/// 数据库（PostgreSQL）状态。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DatabaseRes {
    /// 连通状态：`up` / `down`。
    pub state: String,
    /// 当前连接的库名。
    pub name: String,
    /// 库大小（字节）。
    pub size_bytes: u64,
    /// 库大小的人类可读文本（MB / GB）。
    pub size_text: String,
    /// 当前数据库连接数（`pg_stat_activity` 行数）。
    pub connections: i64,
    /// 数据库版本（`version()` 截断至 40 字符）。
    pub version: String,
}

/// Redis 状态。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RedisRes {
    /// 连通状态：`up` / `down`。
    pub state: String,
    /// Redis 版本。
    pub version: String,
    /// 已用内存的人类可读文本（Redis 自报，如 `1.20M`）。
    pub used_memory_text: String,
    /// 已用内存（字节）。
    pub used_memory_bytes: u64,
    /// 当前客户端连接数。
    pub connected_clients: i64,
    /// 运行时长（秒）。
    pub uptime_seconds: i64,
    /// 命中次数。
    pub keyspace_hits: i64,
    /// 未命中次数。
    pub keyspace_misses: i64,
}

/// 服务器状态响应体。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerStatusRes {
    /// CPU 使用率与核心数。
    pub cpu: CpuRes,
    /// 内存使用情况。
    pub memory: UsageRes,
    /// 根分区磁盘使用情况。
    pub disk: UsageRes,
    /// 数据库状态。
    pub database: DatabaseRes,
    /// Redis 状态。
    pub redis: RedisRes,
    /// 采集时间（RFC 3339）。
    pub collected_at: String,
}

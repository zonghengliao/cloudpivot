//! 服务器状态接口的响应结构。
//!
//! 所有字段均为**聚合数值或版本号**，不含主机地址、连接串、密码等敏感信息。

use serde::{Deserialize, Serialize};

use nik::prelude::*;

use crate::shared::constants::SUPER_ADMIN_ROLE_CODE;
use crate::shared::error_code;

/// 超管判定（诊断包**只有超管能下**）。
///
/// # 判据
///
/// 角色里含 [`SUPER_ADMIN_ROLE_CODE`] **或** 权限里含通配 `*` / `*:*:*`。
///
/// # 为什么权限码之外还要这一层
///
/// 路由上的 `system:server-status:view` 只授给了超管，但「角色授权」页可以把任意
/// 权限码勾给别的角色。诊断包里含配置文件、环境变量、日志尾巴（即便已打码），
/// 属于「排障用的高敏材料」，因此按纵深防御再加一层：**日后谁把码勾给别人，
/// 也拿不到诊断包**（与 `attachment::rule_admin::dto::ensure_super_admin` 同一口径）。
///
/// # 异常情况
///
/// 403 + [`error_code::PERMISSION_DENIED`]：不是超管。
pub fn ensure_super_admin(claims: &TokenClaims, action: &str) -> std::result::Result<(), ApiError> {
    let is_super = claims.roles.iter().any(|role| role == SUPER_ADMIN_ROLE_CODE)
        || claims.permissions.iter().any(|code| code == "*" || code == "*:*:*");
    if is_super {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::PERMISSION_DENIED,
        format!(
            "{action}服务器状态需要超级管理员（{SUPER_ADMIN_ROLE_CODE}）：\
             诊断包含配置、环境变量与日志，属排障用的高敏材料"
        ),
    ))
}

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
    /// 服务端版本号（`current_setting('server_version')`，如 `18.6`）。
    ///
    /// 与 [`Self::version`] 分开：`version()` 是完整编译串（40 字符截断），
    /// 而环境信息卡要的是「PostgreSQL 18.6」这种短版本。
    pub server_version: String,
    /// 本次查询往返耗时（毫秒，保留 1 位小数）；`state = down` 时为 `None`。
    pub latency_ms: Option<f64>,
    /// 已应用迁移条数（`schema_migrations` 中 `success = true` 的行数）。
    pub migrations: i64,
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
    /// 本次 `INFO` 往返耗时（毫秒，保留 1 位小数）；`state = down` 时为 `None`。
    pub latency_ms: Option<f64>,
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
    /// 主机信息（hostname / 内核 / 负载 / 开机时间）。
    pub host: HostRes,
    /// 进程信息（版本 / commit / profile / 启动时间 / 运行时长）。
    pub app: AppRes,
    /// 业务量（用户 / 在线 / 今日登录 / 今日操作）。
    pub business: BusinessRes,
    /// Hermes Agent 探测结果（未安装是**正常**状态，不是错误）。
    pub hermes: HermesProbeRes,
}

/// 主机（操作系统层）信息。
///
/// 与 [`AppRes`] 分开：这里是「这台机器」的信息，`AppRes` 是「这个进程」的信息。
/// 进程重启不会改变 `uptime_seconds` / `boot_time`，两者的差值正是排障时最常看的
/// 「是机器重启了还是服务重启了」。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HostRes {
    /// 主机名（`/etc/hostname` 或 `HOSTNAME`）。
    pub hostname: String,
    /// 发行版名称（`/etc/os-release` 的 `PRETTY_NAME`）。
    pub os_name: String,
    /// 内核版本（`/proc/sys/kernel/osrelease`）。
    pub kernel: String,
    /// CPU 架构（`x86_64` / `aarch64`）。
    pub arch: String,
    /// CPU 型号（`/proc/cpuinfo` 的 `model name`，ARM 上取 `Hardware`）。
    pub cpu_model: String,
    /// CPU 主频（MHz，取 `/proc/cpuinfo` 的 `cpu MHz`；取不到为 0）。
    pub cpu_mhz: u32,
    /// 1 分钟平均负载（`/proc/loadavg`）。
    pub load1: f64,
    /// 5 分钟平均负载。
    pub load5: f64,
    /// 15 分钟平均负载。
    pub load15: f64,
    /// 主机运行时长（秒，`/proc/uptime`）。
    pub uptime_seconds: i64,
    /// 开机时间（RFC 3339；取不到 `/proc/stat` 的 `btime` 时为空串）。
    pub boot_time: String,
}

/// 后端进程信息。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppRes {
    /// 应用版本（`CARGO_PKG_VERSION`）。
    pub version: String,
    /// 构建时的 git commit 短哈希（`build.rs` 注入；无 `.git` 时为 `unknown`）。
    pub commit: String,
    /// 构建时的 rustc 版本（`build.rs` 注入）。
    pub rust_version: String,
    /// 当前生效的配置 profile（`dev` / `test` / `prod`）。
    pub profile: String,
    /// 进程启动时间（RFC 3339）。
    pub started_at: String,
    /// 进程运行时长（秒）。
    pub uptime_seconds: i64,
    /// 进程号。
    pub pid: u32,
}

/// 业务量快照（只读聚合计数，不含任何行级明细）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BusinessRes {
    /// 用户总数（未软删）。
    pub users_total: i64,
    /// 在线会话数（缓存中的登录令牌数；缓存未启用时为 0）。
    pub online_users: i64,
    /// 今日登录次数（`auth_logs` 中 `LOGIN`，按服务器时区切日）。
    pub login_today: i64,
    /// 今日操作次数（`operation_logs`，按服务器时区切日）。
    pub op_log_today: i64,
}

/// Hermes Agent 端口探测结果。
///
/// # 为什么「未监听」是正常状态
///
/// 本机不一定部署了 Hermes Agent（当前生产形态里 Agent 在另一台主机）。
/// 因此 `state = not_installed` 表示「该端口没有服务在听」，
/// 由前端渲染成灰色的「未安装」，**不是**故障，也不该让整页标红。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HermesProbeRes {
    /// `up`（端口可连）/ `not_installed`（连接被拒或超时）。
    pub state: String,
    /// 探测地址（`127.0.0.1:8100` 形态，不含凭据）。
    pub address: String,
    /// 连接耗时（毫秒，保留 1 位小数）；失败时为 `None`。
    pub latency_ms: Option<f64>,
    /// 一句话说明（失败原因已归一到「连接被拒绝 / 超时」两种，不含系统错误细节）。
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一份声明（只关心角色 / 权限两列，其余给固定值）。
    fn claims_of(roles: &[&str], permissions: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "3f1d0000-0000-4000-8000-000000000001".to_string(),
            username: Some("server_status_admin".to_string()),
            roles: roles.iter().map(|role| (*role).to_string()).collect(),
            permissions: permissions.iter().map(|code| (*code).to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn ensure_super_admin_accepts_role_or_wildcard() {
        ensure_super_admin(&claims_of(&[SUPER_ADMIN_ROLE_CODE], &[]), "下载").expect("超管角色必须通过");
        ensure_super_admin(&claims_of(&["user"], &["*:*:*"]), "下载").expect("通配权限必须通过");
        ensure_super_admin(&claims_of(&["user"], &["*"]), "下载").expect("通配 * 必须通过");
    }

    #[test]
    fn ensure_super_admin_rejects_plain_permission_holder() {
        // 即使被授了本页权限码，但不是超管 → 403（诊断包只给超管）
        let err = ensure_super_admin(&claims_of(&["AUDITOR"], &["system:server-status:view"]), "下载")
            .expect_err("非超管必须 403");
        assert_eq!(err.status, 403);
        assert_eq!(err.code, error_code::PERMISSION_DENIED);
        assert!(err.msg.contains(SUPER_ADMIN_ROLE_CODE), "{}", err.msg);
    }

    #[test]
    fn server_status_res_serializes_new_blocks() {
        let value = serde_json::to_value(ServerStatusRes::default()).expect("应能序列化");
        for key in ["host", "app", "business", "hermes", "database", "redis"] {
            assert!(value.get(key).is_some(), "缺少字段 {key}");
        }
        // 新增字段默认值可反序列化（前端契约稳定）
        let back: ServerStatusRes = serde_json::from_value(value).expect("应能反序列化");
        assert_eq!(back.hermes.state, "");
        assert_eq!(back.app.pid, 0);
    }
}

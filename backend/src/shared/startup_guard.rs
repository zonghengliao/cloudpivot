//! 启动守卫：认证已开启但 JWT 签名密钥缺失 / 过短时**拒绝启动**。
//!
//! # 为什么需要
//!
//! alun 0.4.2 的配置基线允许「`[middleware.auth] enabled = true` 且 `jwt_secret` 为空」
//! 这一组合：此时认证中间件照常挂载，但签名密钥退化成**空字符串**，任何人都能自行
//! 签发 `permissions = ["*:*:*"]` 的令牌绕过登录。dev 的密钥自 P0 修复起只由环境变量
//! `ALUN_MIDDLEWARE_AUTH_JWT_SECRET`（`/etc/cloudpivot/cloudpivot-dev.env`）注入，一旦有人绕过
//! `scripts/dev-up.sh` 直接 `cargo run`（不带 env），进程就会落进这个不安全状态。
//!
//! # 两层结构（为什么不是一个检查）
//!
//! | 层 | 位置 | 判据来源 | 作用 |
//! |----|------|----------|------|
//! | 预检 [`preflight`] | [`crate::run`] 开头，**`App::new()` 之前** | `config/config.toml` + `config/config-{profile}.toml` + 环境变量 | 在框架连接数据库 / 缓存之前拦下，裸 `cargo run` 也能看到真正原因 |
//! | 插件守卫 [`AuthSecretGuardPlugin`] | `App::start()` 的插件启动阶段 | 框架合并后的**权威配置** `cfg()` | 兜底：任何预检判定不到的情况（新 profile、自定义配置目录）最终都要过这一关 |
//!
//! 之所以需要预检：框架没有提供 `App::start()` 之前的配置读取入口（`ConfigManager`
//! 未在 `alun` 门面中重导出，按项目铁律又不能直接依赖 `alun-config`），而
//! `App::start()` 的顺序是「先初始化数据库/缓存 → 再跑启动钩子 → 再启插件」。
//! 实测裸 `env -u ALUN_MIDDLEWARE_AUTH_JWT_SECRET cargo run` 会先死在
//! 「数据库连接失败」，把「密钥没注入」这个真正原因完全掩盖——预检就是为消除这个
//! 掩盖而存在。
//!
//! 预检对配置的读取**刻意与框架同源**（见 [`auth_enabled_in_files`] / [`resolve_profile`] /
//! [`resolve_env_secret`] 的注释），且判定不到时**放行**（交插件守卫兜底），
//! 因此预检只影响「报错是否清晰」，不影响安全性。
//!
//! # 执行时序（预检）
//!
//! ```text
//! run() → 预检（本模块，读 config/*.toml + env）→ App::new() → … → App::start()
//!                                                              ├─ 初始化 DB / cache
//!                                                              ├─ 启动钩子
//!                                                              └─ 插件启动（插件守卫再次校验权威配置）
//! ```
//!
//! 两层都在**绑定监听端口之前**完成，因此不存在「认证层已对外提供服务、签名密钥却为空」
//! 的时间窗。
//!
//! # 第二道守卫：存储主密钥（批次 A · A1）
//!
//! 对象存储后端配置里的 AK/SK 用独立主密钥（`CLOUDPIVOT_STORAGE_MASTER_KEY`，
//! 64 位十六进制）做 AES-256-GCM 加密。主密钥缺失 / 长度不符 / 非十六进制时，
//! 存储配置解不开、S3 后端全部不可用——因此与 JWT 密钥同款处理：**拒绝启动**，
//! 绝不「先启动再用默认密钥」。校验逻辑复用
//! [`crate::business::storage::cipher::MasterKey`] 的解析实现（单一事实来源），
//! 本文件只负责「启动期拦下来 + 给出可操作的修复文案」。

use std::fmt;

use alun::prelude::*;
use async_trait::async_trait;

use crate::business::storage::cipher::{ENV_STORAGE_MASTER_KEY, MASTER_KEY_HEX_LEN, MasterKey};

/// JWT 签名密钥的最小长度（字符数）。
///
/// 取 32：HS256 的密钥不应短于哈希输出长度（256 bit，见 RFC 7518 §3.2）。本项目统一用
/// `openssl rand -hex 32` 生成 64 位十六进制串，长度天然满足；设下限是为了拦住
/// 「随手填个 `changeme`」这类弱密钥。
pub const MIN_JWT_SECRET_LEN: usize = 32;

/// 配置目录，必须与 `App::new()`（`App::from_config_dir("config")`）保持一致。
pub const DEFAULT_CONFIG_DIR: &str = "config";

/// 环境变量名：JWT 签名密钥（alun 专用别名，通用 `ALUN_` 前缀覆盖规则同样识别）。
pub const ENV_JWT_SECRET: &str = "ALUN_MIDDLEWARE_AUTH_JWT_SECRET";

/// 环境变量名：JWT 密钥的历史别名，优先级低于 [`ENV_JWT_SECRET`]。
///
/// 依据 alun-config 0.4.2 `env::merge_env_overrides`：先处理 `FACAI_JWT_SECRET`，
/// 再由通用 `ALUN_` 前缀循环覆盖，因此后者生效。
pub const ENV_JWT_SECRET_ALIAS: &str = "FACAI_JWT_SECRET";

/// 认证中间件在 TOML 中的段名。
const AUTH_SECTION: &str = "middleware.auth";

/// 必须使用 Redis 缓存的 profile。
///
/// 吊销状态（`token:blacklist:*` / `token:rotated:*` / `online:*`）全部落在缓存里：
///
/// - `local`（进程内 LRU）重启即清空 → 重启后所有黑名单与轮换标记一起消失，
///   「已登出的 refresh 仍可用」重新出现；
/// - 多实例部署时各进程各有一份 → A 实例吊销、B 实例照放行。
///
/// 因此 prod / dev 只接受 `type = "redis"`；`test` / 其它临时 profile 不强制。
pub const CACHE_REDIS_REQUIRED_PROFILES: &[&str] = &["prod", "dev"];

/// 缓存后端不合规（吊销状态无法可靠保存）时的拒绝原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBackendError {
    /// 当前运行的配置 profile（如 `dev` / `prod`）。
    pub profile: String,
    /// 实际生效的 `[cache] type`。
    pub cache_type: String,
}

impl fmt::Display for CacheBackendError {
    /// 输出可直接贴进终端 / 日志的中文错误，包含危害说明与修复步骤。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "安全守卫拒绝启动：profile={profile} 下 [cache] type = \"{cache_type}\"，必须为 \"redis\"。\n\
             危害：令牌黑名单 / 轮换血缘 / 在线会话全部存在缓存里；进程内 LRU 缓存重启即清空、\
             多实例之间不共享，吊销（登出、强制下线、改密）会静默失效——旧 refresh 令牌可继续续期。\n\
             修复：在 config/config-{profile}.toml 里写 [cache] type = \"redis\" 与 [redis] enabled = true，\
             并用 ALUN_REDIS_URL 指向真实 Redis；不要让基础配置的 [cache] type = \"local\" 兜底生效。",
            profile = self.profile,
            cache_type = self.cache_type
        )
    }
}

impl std::error::Error for CacheBackendError {}

/// 校验缓存后端（纯函数，不读全局状态与文件，便于单测）。
///
/// # 参数
///
/// - `profile`：当前配置 profile
/// - `cache_type`：`[cache].type` 的有效值
///
/// # 返回值
///
/// - `Ok(())`：profile 不在 [`CACHE_REDIS_REQUIRED_PROFILES`]，或已是 redis（大小写不敏感）
/// - `Err(CacheBackendError)`：prod / dev 下使用了非 redis 缓存
pub fn check_cache_backend(
    profile: &str,
    cache_type: &str,
) -> std::result::Result<(), CacheBackendError> {
    if !CACHE_REDIS_REQUIRED_PROFILES.contains(&profile) {
        return Ok(());
    }
    if cache_type.trim().eq_ignore_ascii_case("redis") {
        return Ok(());
    }
    Err(CacheBackendError {
        profile: profile.to_string(),
        cache_type: cache_type.trim().to_string(),
    })
}

/// 启动守卫的拒绝原因。
///
/// 枚举分支各自携带定位信息，**不包含密钥内容本身**，可安全写入日志与终端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSecretGuardError {
    /// 认证已开启，但密钥为空（含仅空白字符 / 未注入）。
    Empty {
        /// 当前运行的配置 profile（`cfg().profile`，如 `dev` / `prod`）
        profile: String,
    },
    /// 认证已开启，但密钥长度不足 [`MIN_JWT_SECRET_LEN`]。
    TooShort {
        /// 当前运行的配置 profile（`cfg().profile`，如 `dev` / `prod`）
        profile: String,
        /// 密钥实际长度（trim 后按字符计），用于排错，不泄露密钥内容
        len: usize,
    },
}

impl fmt::Display for AuthSecretGuardError {
    /// 输出可直接贴进终端 / 日志的中文错误，包含危害说明与修复步骤。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { profile } => write!(
                f,
                "安全守卫拒绝启动：profile={profile} 下 [middleware.auth] enabled = true，\
                 但签名密钥 jwt_secret 为空（{ENV_JWT_SECRET} 未注入）。\n\
                 危害：认证中间件仍会挂载，签名密钥退化为空字符串，任何人都能伪造令牌绕过登录。\n\
                 修复：① 用 scripts/dev-up.sh 启动（它会 source /etc/cloudpivot/cloudpivot-dev.env 并注入密钥）；\
                 ② 或显式注入环境变量 {ENV_JWT_SECRET}=\"$(openssl rand -hex 32)\"。\n\
                 禁止为了「能跑起来」而把 [middleware.auth] enabled 改成 false。"
            ),
            Self::TooShort { profile, len } => write!(
                f,
                "安全守卫拒绝启动：profile={profile} 下 [middleware.auth] enabled = true，\
                 但签名密钥 jwt_secret 太短（当前 {len} 个字符，要求至少 {MIN_JWT_SECRET_LEN} 个）。\n\
                 危害：弱密钥可被离线爆破，等价于认证失效。\n\
                 修复：重新生成强密钥并注入环境变量 {ENV_JWT_SECRET}=\"$(openssl rand -hex 32)\"；\
                 dev 环境请用 scripts/dev-up.sh 启动（密钥只写在 /etc/cloudpivot/cloudpivot-dev.env，不落 git 跟踪文件）。"
            ),
        }
    }
}

impl std::error::Error for AuthSecretGuardError {}

/// 校验认证密钥配置（纯函数，不读全局状态与文件，便于单测）。
///
/// # 参数
///
/// - `profile`：当前配置 profile（如 `dev` / `prod`）
/// - `auth_enabled`：`[middleware.auth].enabled`
/// - `jwt_secret`：`[middleware.auth].jwt_secret`（可能经环境变量覆盖后注入）
///
/// # 返回值
///
/// - `Ok(())`：认证未开启（不挂载认证层，无风险）；或已开启且密钥 trim 后长度达标
/// - `Err(AuthSecretGuardError)`：认证已开启而密钥为空 / 过短
///
/// # 说明
///
/// 判定按 `trim` 后的长度进行（环境变量被误写成空格时同样拦下），但**不改写**配置值：
/// 签名用的仍是框架读取到的原始字符串，守卫只做判定。
pub fn check_auth_secret(
    profile: &str,
    auth_enabled: bool,
    jwt_secret: &str,
) -> std::result::Result<(), AuthSecretGuardError> {
    if !auth_enabled {
        return Ok(());
    }

    let trimmed = jwt_secret.trim();
    if trimmed.is_empty() {
        return Err(AuthSecretGuardError::Empty {
            profile: profile.to_string(),
        });
    }

    let len = trimmed.chars().count();
    if len < MIN_JWT_SECRET_LEN {
        return Err(AuthSecretGuardError::TooShort {
            profile: profile.to_string(),
            len,
        });
    }

    Ok(())
}

/// 预检决策（纯函数）：只有「认证**明确**开启 + 密钥不合规」才拒绝启动。
///
/// # 参数
///
/// - `profile`：生效 profile
/// - `auth_enabled`：预检从配置文件读到的有效 `enabled`；`None` = 读不到（无法判定）
/// - `env_secret`：环境变量注入的密钥，`None` = 未注入
///
/// # 返回值
///
/// - `Ok(())`：认证明确关闭，或无法判定（放行，交插件守卫用权威配置兜底），或密钥合规
/// - `Err(AuthSecretGuardError)`：认证明确开启且密钥为空 / 过短
pub fn evaluate_preflight(
    profile: &str,
    auth_enabled: Option<bool>,
    env_secret: Option<&str>,
) -> std::result::Result<(), AuthSecretGuardError> {
    match auth_enabled {
        Some(true) => check_auth_secret(profile, true, env_secret.unwrap_or_default()),
        // 明确关闭：认证层不挂载，空密钥无风险
        // 无法判定：不放行也不能误杀，交给插件守卫按框架权威配置判定
        Some(false) | None => Ok(()),
    }
}

/// 解析生效 profile。
///
/// 与 alun-config 0.4.2 `env::detect_profile` 完全同源：命令行 `profile=xxx` >
/// `ALUN_PROFILE` > `ALUN_ENV` > 默认 `dev`。
pub fn resolve_profile() -> String {
    for arg in std::env::args() {
        if let Some(profile) = arg.strip_prefix("profile=") {
            return profile.to_string();
        }
    }

    std::env::var("ALUN_PROFILE")
        .or_else(|_| std::env::var("ALUN_ENV"))
        .unwrap_or_else(|_| "dev".to_string())
}

/// 解析环境变量注入的签名密钥。
///
/// 与 alun-config 0.4.2 `env::merge_env_overrides` 同源：[`ENV_JWT_SECRET_ALIAS`] 先赋值，
/// 空值忽略；[`ENV_JWT_SECRET`] 后赋值并覆盖前者。
pub fn resolve_env_secret() -> Option<String> {
    let mut secret = None;

    if let Ok(value) = std::env::var(ENV_JWT_SECRET_ALIAS)
        && !value.is_empty()
    {
        secret = Some(value);
    }
    if let Ok(value) = std::env::var(ENV_JWT_SECRET)
        && !value.is_empty()
    {
        secret = Some(value);
    }

    secret
}

/// 扫描一份 TOML 文本里 `[middleware.auth]` 段的 `enabled`。
///
/// # 参数
///
/// - `text`：TOML 文本（如 `config/config-dev.toml` 的内容）
///
/// # 返回值
///
/// - `Some(true)` / `Some(false)`：该段显式写了 `enabled`（同段多次出现时取最后一次）
/// - `None`：该段没有写 `enabled`，或整份文本没有该段
///
/// # 说明
///
/// 只做「段名 + enabled 键」的最小识别，不实现完整 TOML 解析：守卫不落任何业务数据，
/// 需要的仅这一个布尔值。行尾 `#` 注释会被剔除。
pub fn auth_enabled_in_toml(text: &str) -> Option<bool> {
    let mut section = String::new();
    let mut found = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') {
            section = line
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            continue;
        }

        if section != AUTH_SECTION {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "enabled" {
            continue;
        }

        let value = value.split('#').next().unwrap_or_default().trim();
        match value {
            "true" => found = Some(true),
            "false" => found = Some(false),
            _ => {}
        }
    }

    found
}

/// 读 `config.toml` 与 `config-{profile}.toml`，得出该 profile 下的**有效** `enabled`。
///
/// # 参数
///
/// - `config_dir`：配置目录（生产/开发均为 [`DEFAULT_CONFIG_DIR`]）
/// - `profile`：生效 profile
///
/// # 返回值
///
/// - `Some(bool)`：有效值
/// - `None`：两份文件都读不到 / 都没有该段（无法判定，预检放行）
///
/// # 合并语义（与 alun-config 0.4.2 `merge_middleware` 一致）
///
/// 框架的覆盖规则是「overlay 的值与**默认值**不同才覆盖」，`auth.enabled` 默认为
/// `false`，所以 profile 文件里的 `enabled = false` 等于默认值、会被丢弃，最终值只能是
/// 「基础配置的 true」或「profile 写的 true」，即 `基础 || profile`。
/// 文件读取失败时框架用 `AppConfig::default()` 兜底（auth 关闭），本函数同样按未配置处理。
pub fn auth_enabled_in_files(config_dir: &str, profile: &str) -> Option<bool> {
    let read = |name: &str| {
        let path = std::path::Path::new(config_dir).join(name);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| auth_enabled_in_toml(&text))
    };

    let base = read("config.toml");
    let overlay = read(&format!("config-{profile}.toml"));

    match (base, overlay) {
        (Some(base), Some(overlay)) => Some(base || overlay),
        (Some(base), None) => Some(base),
        (None, Some(overlay)) => Some(overlay),
        (None, None) => None,
    }
}

/// 是否为「只处理配置、不启动服务」的 CLI 模式（`gen-config` / `print-config`）。
///
/// 与 alun-config 0.4.2 `env::parse_args` 同源；这些模式不会绑定端口、不会挂载认证层，
/// 因此预检必须放行，否则 `cargo run -- print-config` 会被误杀。
fn is_config_only_mode() -> bool {
    std::env::args().any(|arg| {
        matches!(
            arg.as_str(),
            "gen-config" | "generate-config" | "print-config"
        )
    })
}

/// 存储主密钥校验错误。
///
/// 只携带「缺失 / 长度 / 非十六进制」的定位信息，**绝不包含密钥内容**，可安全落日志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageKeyGuardError {
    /// 环境变量未注入或为空。
    Missing {
        /// 当前运行的配置 profile。
        profile: String,
    },
    /// 长度或字符集不合法。
    Invalid {
        /// 当前运行的配置 profile。
        profile: String,
        /// 具体原因（来自主密钥解析，不含密钥内容）。
        reason: String,
    },
}

impl fmt::Display for StorageKeyGuardError {
    /// 输出可直接贴进终端 / 日志的中文错误，含危害说明与修复步骤。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { profile } => write!(
                f,
                "安全守卫拒绝启动：profile={profile} 下存储主密钥 {ENV_STORAGE_MASTER_KEY} 未注入。\n\
                 危害：存储后端配置里的 AK/SK 无法加解密，S3 兼容后端全部不可用（本地后端仍可用）。\n\
                 修复：在 /etc/cloudpivot/cloudpivot{profile_suffix}.env 中写入 \
                 {ENV_STORAGE_MASTER_KEY}=\"$(openssl rand -hex 32)\"（每个环境用不同的值，文件权限 600）。",
                profile_suffix = if profile == "prod" {
                    String::new()
                } else {
                    format!("-{profile}")
                }
            ),
            Self::Invalid { profile, reason } => write!(
                f,
                "安全守卫拒绝启动：profile={profile} 下存储主密钥不合法（{reason}）。\n\
                 要求：恰好 {MASTER_KEY_HEX_LEN} 位十六进制（32 字节，供 AES-256-GCM 使用）。\n\
                 修复：用 openssl rand -hex 32 重新生成后写入 {ENV_STORAGE_MASTER_KEY}，\
                 已加密的存储配置需要重新填写 AK/SK。"
            ),
        }
    }
}

impl std::error::Error for StorageKeyGuardError {}

/// 校验存储主密钥（纯函数，不读全局状态，便于单测）。
///
/// # 参数
///
/// - `profile`：当前 profile（仅用于错误文案）。
/// - `raw`：环境变量原文；`None` 表示未注入。
///
/// # 返回值
///
/// - `Ok(())`：密钥存在且为 64 位十六进制；
/// - `Err(StorageKeyGuardError::Missing)`：未注入或全空白；
/// - `Err(StorageKeyGuardError::Invalid)`：长度 / 字符集不合法。
pub fn check_storage_master_key(
    profile: &str,
    raw: Option<&str>,
) -> std::result::Result<(), StorageKeyGuardError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Err(StorageKeyGuardError::Missing {
            profile: profile.to_string(),
        });
    };
    MasterKey::parse(raw)
        .map(|_| ())
        .map_err(|error| StorageKeyGuardError::Invalid {
            profile: profile.to_string(),
            reason: error
                .to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
        })
}

/// 读取环境变量注入的存储主密钥（不校验）。
///
/// # 返回值
///
/// 未注入或空白时返回 `None`。
pub fn resolve_storage_master_key() -> Option<String> {
    std::env::var(ENV_STORAGE_MASTER_KEY)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// 启动期拒绝原因（JWT 密钥守卫 + 存储主密钥守卫的统一出口）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupViolation {
    /// JWT 签名密钥不合规。
    AuthSecret(AuthSecretGuardError),
    /// 存储主密钥不合规。
    StorageKey(StorageKeyGuardError),
}

impl fmt::Display for StartupViolation {
    /// 直接转发内部错误的中文文案。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthSecret(inner) => write!(f, "{inner}"),
            Self::StorageKey(inner) => write!(f, "{inner}"),
        }
    }
}

impl std::error::Error for StartupViolation {}

impl From<AuthSecretGuardError> for StartupViolation {
    /// 由 JWT 密钥错误转换。
    fn from(error: AuthSecretGuardError) -> Self {
        Self::AuthSecret(error)
    }
}

impl From<StorageKeyGuardError> for StartupViolation {
    /// 由存储主密钥错误转换。
    fn from(error: StorageKeyGuardError) -> Self {
        Self::StorageKey(error)
    }
}

/// 启动预检：在 `App::new()` **之前**判定并返回拒绝原因。
///
/// # 返回值
///
/// - `Ok(())`：放行（认证关闭 / 无法判定 / 密钥合规，且存储主密钥合规；或当前是
///   gen-config、print-config 模式）；
/// - `Err(StartupViolation)`：JWT 密钥为空 / 过短，或存储主密钥缺失 / 不合法。
///
/// # 检查顺序
///
/// 先 JWT 密钥（历史守卫，语义不变），再存储主密钥——两者的错误文案各自独立，
/// 一次只报第一个问题，避免运维面对两条互相干扰的提示。
pub fn preflight() -> std::result::Result<(), StartupViolation> {
    if is_config_only_mode() {
        return Ok(());
    }

    let profile = resolve_profile();
    let auth_enabled = auth_enabled_in_files(DEFAULT_CONFIG_DIR, &profile);
    let secret = resolve_env_secret();

    evaluate_preflight(&profile, auth_enabled, secret.as_deref())?;
    check_storage_master_key(&profile, resolve_storage_master_key().as_deref())?;
    Ok(())
}

/// 启动守卫插件：在插件启动阶段用框架**权威配置**再校验一次，不合格即中止启动。
///
/// 与 [`preflight`] 的分工见模块文档；插件层负责兜住预检判定不到的所有情况
/// （新 profile、自定义配置目录、预检读文件失败等）。
pub struct AuthSecretGuardPlugin;

impl AuthSecretGuardPlugin {
    /// 创建启动守卫插件（无状态）。
    pub fn new() -> Self {
        Self
    }
}

impl Default for AuthSecretGuardPlugin {
    /// 等价于 [`AuthSecretGuardPlugin::new`]。
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for AuthSecretGuardPlugin {
    /// 插件名称（用于插件管理与日志）。
    fn name(&self) -> &str {
        "startup-auth-secret-guard"
    }

    /// 校验 `[middleware.auth]` 与 `[cache]`，不通过则返回 `Err` 中止启动。
    ///
    /// # 返回值
    ///
    /// - `Ok(())`：认证未开启或密钥长度达标；且缓存后端符合该 profile 的要求
    /// - `Err(Error::Config)`：密钥为空 / 过短，或 prod / dev 下缓存不是 redis
    async fn start(&self) -> std::result::Result<(), Error> {
        let config = cfg();
        let auth = &config.middleware.auth;

        match check_auth_secret(&config.profile, auth.enabled, &auth.jwt_secret) {
            Ok(()) => {
                if auth.enabled {
                    // 只记录长度，绝不打印密钥内容
                    tracing::info!(
                        profile = %config.profile,
                        jwt_secret_len = auth.jwt_secret.trim().chars().count(),
                        "启动守卫通过：认证密钥长度满足下限"
                    );
                }
            }
            Err(violation) => {
                // 先落一条 error 日志（便于 journalctl 检索），再作为启动错误向上传播
                tracing::error!(
                    profile = %config.profile,
                    auth_enabled = auth.enabled,
                    "启动守卫拦截：{violation}"
                );
                return Err(Error::Config(violation.to_string()));
            }
        }

        // 缓存后端：吊销状态（黑名单 / 轮换血缘 / 在线会话）全在缓存里，
        // prod / dev 用进程内 LRU 等于吊销静默失效（CODE-16 B6）。
        match check_cache_backend(&config.profile, &config.cache.r#type) {
            Ok(()) => {
                tracing::info!(
                    profile = %config.profile,
                    cache_type = %config.cache.r#type,
                    "启动守卫通过：缓存后端满足吊销耐久性要求"
                );
            }
            Err(violation) => {
                tracing::error!(
                    profile = %config.profile,
                    cache_type = %config.cache.r#type,
                    "启动守卫拦截：{violation}"
                );
                return Err(Error::Config(violation.to_string()));
            }
        }

        // 存储主密钥（批次 A · A1）：缺失 / 长度不符 / 非十六进制即拒绝启动。
        // 只记录长度，绝不打印密钥内容。
        let master_key = resolve_storage_master_key();
        match check_storage_master_key(&config.profile, master_key.as_deref()) {
            Ok(()) => {
                tracing::info!(
                    profile = %config.profile,
                    storage_master_key_len = master_key.as_deref().map(|key| key.trim().len()).unwrap_or_default(),
                    "启动守卫通过：存储主密钥长度满足要求"
                );
                Ok(())
            }
            Err(violation) => {
                tracing::error!(
                    profile = %config.profile,
                    "启动守卫拦截：{violation}"
                );
                Err(Error::Config(violation.to_string()))
            }
        }
    }

    /// 停止插件：无资源需要释放。
    ///
    /// # 返回值
    ///
    /// 恒返回 `Ok(())`。
    async fn stop(&self) -> std::result::Result<(), Error> {
        Ok(())
    }
}

// ==================== 第三道守卫：AI 用户不变式（L1） ====================

/// AI 用户不变式被破坏的具体清单。
///
/// 不变式（laoliu 2026-10-29 追加硬要求）：
///
/// ```text
/// sys_user.user_type = 'ROBOT'  ⇔  sys_ai_agent 存在该 user_id 的记录
/// （两侧都只看「未软删」行：软删的账号与配置行不算存在）
/// ```
///
/// 破坏形态只有两种，各自带一份 id 清单，便于运维直接拿 id 去查：
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiAgentInvariantError {
    /// 「是 ROBOT 账号但没有配置行」的 `sys_user.id`（配置行被手工删 / 建号只写了一半）。
    pub robot_without_agent: Vec<String>,
    /// 「有配置行但账号不是 ROBOT（或已被软删）」的 `sys_ai_agent.user_id`。
    pub agent_without_robot: Vec<String>,
}

impl AiAgentInvariantError {
    /// 是否确实有违规（空 = 健康）。
    pub fn is_violated(&self) -> bool {
        !self.robot_without_agent.is_empty() || !self.agent_without_robot.is_empty()
    }

    /// 一行摘要（写进 ERROR 日志 / 启动失败原因）。
    pub fn summary(&self) -> String {
        format!(
            "AI 用户不变式被破坏：{} 个 ROBOT 账号缺少 sys_ai_agent 配置行（user_id: {}）；\
             {} 条 sys_ai_agent 配置行对应非 ROBOT / 已删除账号（user_id: {}）。\
             修复：删除孤立行，或走 /api/system/user/ai 接口重建 AI 用户",
            self.robot_without_agent.len(),
            join_ids(&self.robot_without_agent),
            self.agent_without_robot.len(),
            join_ids(&self.agent_without_robot),
        )
    }
}

impl fmt::Display for AiAgentInvariantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary())
    }
}

impl std::error::Error for AiAgentInvariantError {}

fn join_ids(ids: &[String]) -> String {
    const MAX: usize = 10;
    if ids.is_empty() {
        return "-".to_string();
    }
    let mut out = ids.iter().take(MAX).cloned().collect::<Vec<_>>().join(", ");
    if ids.len() > MAX {
        out.push_str(&format!(" …（共 {} 条）", ids.len()));
    }
    out
}

/// 启动自检：AI 用户不变式（`ROBOT` 账号 ⇔ `sys_ai_agent` 配置行）。
///
/// # 为什么放在启动路径上
///
/// 「账号是 ROBOT 但配置行没了」意味着 Hermes profile 无人管理（云枢既不能编辑它的
/// SOUL.md，也不会在删号时清理磁盘），而「配置行还在但账号不是 ROBOT」意味着
/// 数据权限/审计把「机器人」当成了普通账号。两种都是**静默错**，不查库永远发现不了，
/// 所以每次启动都各查一次（两条走 `deleted_at` 部分索引的轻量查询，毫秒级，最多各取 50 条 id）。
///
/// # 参数
///
/// 无。使用全局 `db()`（调用点必须在数据库初始化之后，即 `bootstrap` 阶段）。
///
/// # 返回值
///
/// 健康返回 `Ok(())`；有违规返回 [`AiAgentInvariantError`]（带 id 清单）。
///
/// # 异常情况
///
/// 查库失败（连接不可用 / 权限不足）时返回 [`Error`]（`Error::Config`），
/// 调用方与「违规」同样处理：prod 拒启动，其余告警。
pub async fn check_ai_agent_invariant() -> std::result::Result<(), Error> {
    // ① ROBOT 账号必须都有配置行
    // SCOPE-EXEMPT: 启动自检是全局计数（跨全部用户），注入数据范围会让它漏检——
    // 这里要的恰恰是「不受任何范围限制地看全库」。
    let robot_rows = db()
        .find_with_params(
            "SELECT u.id::text AS user_id FROM sys_user u \
             WHERE u.user_type = 'ROBOT' AND u.deleted_at IS NULL \
               AND NOT EXISTS (SELECT 1 FROM sys_ai_agent a \
                               WHERE a.user_id = u.id AND a.deleted_at IS NULL) \
             ORDER BY u.id LIMIT 50",
            vec![],
        )
        .await
        .map_err(|e| Error::Config(format!("AI 用户自检无法完成（查询 ROBOT 账号）：{e}")))?;

    // ② 配置行必须都对应未删的 ROBOT 账号
    // SCOPE-EXEMPT: 同 ①，启动自检是全局一致性校验，不做数据范围过滤。
    let agent_rows = db()
        .find_with_params(
            "SELECT a.user_id::text AS user_id FROM sys_ai_agent a \
             JOIN sys_user u ON u.id = a.user_id \
             WHERE a.deleted_at IS NULL \
               AND (u.deleted_at IS NOT NULL OR u.user_type <> 'ROBOT') \
             ORDER BY a.user_id LIMIT 50",
            vec![],
        )
        .await
        .map_err(|e| Error::Config(format!("AI 用户自检无法完成（查询 sys_ai_agent）：{e}")))?;

    let collect = |rows: Vec<Row>| -> Vec<String> {
        rows.iter()
            .filter_map(|row| row.get_typed::<String>("user_id").ok())
            .collect()
    };
    let violation = AiAgentInvariantError {
        robot_without_agent: collect(robot_rows),
        agent_without_robot: collect(agent_rows),
    };
    // E15：安全红线巡检（与不变式判定并列，只 WARN、不影响返回值）。
    // 放在 violated 判断之前：不变式坏了也要把「哪些 profile 没有红线」一起点出来。
    warn_missing_safety_block().await;
    if violation.is_violated() {
        return Err(Error::Config(violation.summary()));
    }
    tracing::info!("AI 用户不变式自检通过：ROBOT 账号与 sys_ai_agent 一一对应");
    Ok(())
}

/// 启动巡检（E15）：每个 `user_type='ROBOT'` 的 Hermes profile，其 `SOUL.md` 必须含
/// **当前版本**的安全红线区块（[`crate::business::aiagent::safety::SAFETY_BEGIN`]）。
///
/// # 为什么只 WARN、不阻启动
///
/// 缺失区块不会让任何接口报错、也不会让账号登不上 —— 它的危害是「这个 AI 员工这一轮
/// 上下文里没有红线」（典型成因：有人手工编辑了 `.99` 上的 `SOUL.md`、或 profile 从
/// 别处拷过来）。但 `SOUL.md` 在**另一台机器**上（Hermes 在 `172.18.10.99`，本服务通过
/// ssh 读写），网络抖动 / 免密失效都会让「读不到」变成「启动失败」，代价远大于收益。
/// 所以：**点名 WARN**，修复走编辑接口（`PUT /system/user/ai/{id}` 会自动补回）。
///
/// # 处置口径
///
/// | 情况 | 日志 |
/// |------|------|
/// | 文件存在但没有当前版本区块 | `WARN` + profile 名（逐个点名） |
/// | 文件读不到（404 / ssh 失败 / 名字非法） | `WARN` + profile 名 + 原因（**不能**当成「合规」） |
/// | 全部合规 | `INFO` + 检查了几个 profile |
/// | 查库失败 | `WARN`（不阻启动） |
///
/// # 异常情况
///
/// 本函数**不返回错误**：所有异常都被降级成日志，调用方（[`check_ai_agent_invariant`]）
/// 的语义不受影响。
async fn warn_missing_safety_block() {
    use crate::business::aiagent::hermes::Hermes;
    use crate::business::aiagent::safety;

    // SCOPE-EXEMPT: 启动巡检是全局检查（跨全部 ROBOT 账号），注入数据范围会漏检。
    let rows = match db()
        .find_with_params(
            "SELECT a.hermes_profile FROM sys_ai_agent a \
             JOIN sys_user u ON u.id = a.user_id \
             WHERE a.deleted_at IS NULL AND u.deleted_at IS NULL AND u.user_type = 'ROBOT' \
             ORDER BY a.hermes_profile LIMIT 50",
            vec![],
        )
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!("AI 员工安全红线巡检无法完成（查库失败，继续启动）：{error}");
            return;
        }
    };
    let profiles: Vec<String> = rows
        .iter()
        .filter_map(|row| row.get_typed::<String>("hermes_profile").ok())
        .collect();
    if profiles.is_empty() {
        tracing::info!("AI 员工安全红线巡检：当前没有 ROBOT profile，跳过");
        return;
    }

    let hermes = Hermes::from_env();
    let mut missing: Vec<String> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();
    for profile in &profiles {
        match hermes.read_soul(profile).await {
            Ok(soul) => {
                if !safety::has_safety_block(&soul) {
                    missing.push(profile.clone());
                }
            }
            Err(error) => unreadable.push(format!("{profile}（{}）", error.msg)),
        }
    }

    for profile in &missing {
        tracing::warn!(
            profile = %profile,
            "AI 员工 SOUL.md 缺少安全红线区块（E15）：该员工当前上下文里没有红线；\
             请通过 PUT /system/user/ai/{{id}} 保存一次以自动补回"
        );
    }
    for detail in &unreadable {
        tracing::warn!("AI 员工 SOUL.md 读取失败，无法确认安全红线（E15）：{detail}");
    }
    if missing.is_empty() && unreadable.is_empty() {
        tracing::info!(
            "AI 员工安全红线巡检通过：{} 个 ROBOT profile 均含安全红线区块",
            profiles.len()
        );
    } else {
        tracing::warn!(
            "AI 员工安全红线巡检：{} 个 profile 缺区块，{} 个读不到（共 {} 个 ROBOT profile）",
            missing.len(),
            unreadable.len(),
            profiles.len()
        );
    }
}

/// 启动自检：孤儿任务依赖的**数量**（后继或前置已被软删的 `biz_task_dependency` 行）。
///
/// # 为什么要有这一项
///
/// `biz_task_dependency` 的两个外键都是 `ON DELETE CASCADE`，但任务是**软删**
/// （`deleted_at`）：CASCADE 只在物理删除时触发，软删不会带走依赖行。写完删除路径的
/// 清理（`business::task::model::soft_delete` 在同一事务里 `DELETE`）之后，本项是
/// 那道清理的**兜底**：历史残留、绕过接口的手工改库、并发漏删，都能在启动日志里被点名。
///
/// # 处置
///
/// **只告警、不阻启动**（调用方 [`crate::bootstrap`] 决定级别）：孤儿依赖不影响任何
/// 读路径（列表 / 详情 / 甘特全带 `JOIN biz_task … AND deleted_at IS NULL`），
/// 它只是脏数据在累积 —— 为它拒绝上线代价远大于收益。
///
/// # 返回值
///
/// 孤儿依赖行数；`0` = 干净。
///
/// # 异常情况
///
/// 查库失败（连接不可用 / 权限不足）→ [`Error::Config`]，调用方同样只告警。
// SCOPE-EXEMPT: 启动自检要的是「不受任何数据范围限制地看全库」，注入范围会漏检。
pub async fn count_orphan_task_dependencies() -> std::result::Result<u64, Error> {
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_task_dependency d \
             WHERE NOT EXISTS (SELECT 1 FROM biz_task x \
                                WHERE x.id = d.task_id AND x.deleted_at IS NULL) \
                OR NOT EXISTS (SELECT 1 FROM biz_task y \
                                WHERE y.id = d.depends_on_id AND y.deleted_at IS NULL)",
            vec![],
        )
        .await
        .map_err(|e| Error::Config(format!("孤儿任务依赖自检无法完成（查库失败）：{e}")))?;
    Ok(count.max(0) as u64)
}

/// 孤儿依赖的告警文案：`0` 条 → `None`（正常，不打告警日志）；`> 0` → 点名数量。
///
/// 抽成纯函数是为了能单测「多少条才告警」这条判定，不必连库。
pub fn orphan_dependency_warning(count: u64) -> Option<String> {
    if count == 0 {
        return None;
    }
    Some(format!(
        "发现 {count} 条孤儿任务依赖（后继或前置已软删；ON DELETE CASCADE 对软删不生效）。\
         不阻断业务（查询已按 deleted_at 过滤），但属脏数据累积，建议清理：\
         自本版本起任务删除会在同事务里清理依赖，存量行请按同条件 DELETE。"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用 64 位十六进制密钥（形状与 `openssl rand -hex 32` 一致，非真实密钥）。
    const VALID_SECRET_HEX64: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // ────────────── check_auth_secret：规则本身 ──────────────

    /// 正常路径：64 位 hex 密钥必须放行。
    #[test]
    fn accepts_hex64_secret() {
        assert_eq!(VALID_SECRET_HEX64.len(), 64, "测试常量必须是 64 位");
        assert!(check_auth_secret("dev", true, VALID_SECRET_HEX64).is_ok());
    }

    /// 错误分支：空密钥必须被拦下，且报到 `Empty`。
    #[test]
    fn rejects_empty_secret() {
        match check_auth_secret("dev", true, "") {
            Err(AuthSecretGuardError::Empty { profile }) => assert_eq!(profile, "dev"),
            other => panic!("空密钥应返回 Empty，实际为 {other:?}"),
        }
    }

    /// 错误分支：纯空白密钥视同空密钥（环境变量被误写成空格）。
    #[test]
    fn rejects_whitespace_only_secret() {
        assert!(matches!(
            check_auth_secret("dev", true, "   \t\n "),
            Err(AuthSecretGuardError::Empty { .. })
        ));
    }

    /// 错误分支：长度不足下限必须被拦下，并报出实际长度。
    #[test]
    fn rejects_short_secret() {
        let short = "a".repeat(MIN_JWT_SECRET_LEN - 1);
        match check_auth_secret("dev", true, &short) {
            Err(AuthSecretGuardError::TooShort { profile, len }) => {
                assert_eq!(profile, "dev");
                assert_eq!(len, MIN_JWT_SECRET_LEN - 1);
            }
            other => panic!("短密钥应返回 TooShort，实际为 {other:?}"),
        }
    }

    /// 边界分支：恰好等于下限长度必须放行（避免 off-by-one 误杀）。
    #[test]
    fn accepts_secret_exactly_at_min_len() {
        let boundary = "b".repeat(MIN_JWT_SECRET_LEN);
        assert!(check_auth_secret("dev", true, &boundary).is_ok());
    }

    /// 正常路径：认证未开启时不校验密钥（不挂载认证层，空密钥无风险）。
    #[test]
    fn skips_check_when_auth_disabled() {
        assert!(check_auth_secret("dev", false, "").is_ok());
    }

    /// 错误消息必须带上可操作的修复指引，防止提示语被改坏后运维无迹可循。
    #[test]
    fn error_message_carries_remediation_hint() {
        let empty = AuthSecretGuardError::Empty {
            profile: "dev".to_string(),
        }
        .to_string();
        assert!(empty.contains(ENV_JWT_SECRET));
        assert!(empty.contains("dev-up.sh"));

        let short = AuthSecretGuardError::TooShort {
            profile: "prod".to_string(),
            len: 8,
        }
        .to_string();
        assert!(short.contains(ENV_JWT_SECRET));
        assert!(short.contains("至少 32"));
    }

    // ────────────── 预检决策 ──────────────

    /// 预检：认证明确开启且环境变量没注入密钥 → 拒绝（裸 `cargo run` 的主路径）。
    #[test]
    fn preflight_rejects_when_auth_enabled_without_env_secret() {
        assert_eq!(
            evaluate_preflight("dev", Some(true), None),
            Err(AuthSecretGuardError::Empty {
                profile: "dev".to_string()
            })
        );
    }

    /// 预检：认证明确开启但密钥太短 → 拒绝。
    #[test]
    fn preflight_rejects_short_env_secret() {
        assert!(matches!(
            evaluate_preflight("dev", Some(true), Some("changeme")),
            Err(AuthSecretGuardError::TooShort { len: 8, .. })
        ));
    }

    /// 预检：认证明确开启且密钥合规 → 放行。
    #[test]
    fn preflight_accepts_valid_env_secret() {
        assert!(evaluate_preflight("dev", Some(true), Some(VALID_SECRET_HEX64)).is_ok());
    }

    /// 预检：认证明确关闭 → 放行（即便密钥为空）。
    #[test]
    fn preflight_skips_when_auth_disabled() {
        assert!(evaluate_preflight("whatever", Some(false), None).is_ok());
    }

    /// 预检：读不到配置（无法判定）→ 放行，交插件守卫兜底，避免误杀自定义配置目录。
    #[test]
    fn preflight_skips_when_undeterminable() {
        assert!(evaluate_preflight("whatever", None, None).is_ok());
    }

    // ────────────── TOML 段扫描与合并 ──────────────

    /// 段扫描：只认 `[middleware.auth]` 段内的 `enabled`，其它段的同名键不得串味。
    #[test]
    fn toml_scan_is_section_scoped() {
        let text = "\
[middleware.cors]
enabled = true

[middleware.auth]
ignore_paths = [\"/system/auth/login\"]
enabled = true   # 开发环境开启

[middleware.rate_limit]
enabled = false
";
        assert_eq!(auth_enabled_in_toml(text), Some(true));
    }

    /// 段扫描：段内显式 `false` 时返回 false；同段多次出现以最后一次为准。
    #[test]
    fn toml_scan_takes_last_value_in_section() {
        let text = "\
[middleware.auth]
enabled = true
enabled = false
";
        assert_eq!(auth_enabled_in_toml(text), Some(false));
    }

    /// 段扫描：没有该段 / 段内没写 enabled → None（无法判定）。
    #[test]
    fn toml_scan_returns_none_when_absent() {
        assert_eq!(auth_enabled_in_toml("[server]\nlisten = \"8023\"\n"), None);
        assert_eq!(
            auth_enabled_in_toml("[middleware.auth]\nignore_paths = []\n"),
            None
        );
        assert_eq!(auth_enabled_in_toml(""), None);
    }

    /// 合并语义：基础 true + profile 未写 → 仍为 true（profile 无法把 true 关掉，
    /// 与 alun-config `merge_middleware` 的「与默认值相同则丢弃」一致）。
    #[test]
    fn merge_semantics_or_base_and_profile() {
        // 用仓库里真实的四份配置回归：config.toml 关闭，dev/prod/test 覆盖为开启
        assert_eq!(auth_enabled_in_files(DEFAULT_CONFIG_DIR, "dev"), Some(true));
        assert_eq!(
            auth_enabled_in_files(DEFAULT_CONFIG_DIR, "prod"),
            Some(true)
        );
        assert_eq!(
            auth_enabled_in_files(DEFAULT_CONFIG_DIR, "test"),
            Some(true)
        );
        // 未知 profile：只有基础配置（enabled = false）
        assert_eq!(
            auth_enabled_in_files(DEFAULT_CONFIG_DIR, "no-such-profile"),
            Some(false)
        );
        // 配置目录不存在：无法判定 → None（预检放行，插件守卫兜底）
        assert_eq!(auth_enabled_in_files("no-such-dir", "dev"), None);
    }

    /// 缓存后端守卫：prod / dev 必须是 redis，其它 profile 不强制。
    #[test]
    fn cache_backend_requires_redis_for_prod_and_dev() {
        assert_eq!(check_cache_backend("prod", "redis"), Ok(()));
        assert_eq!(check_cache_backend("dev", "redis"), Ok(()));
        // 大小写与空白不敏感（配置里写成 Redis / " redis " 也应通过）
        assert_eq!(check_cache_backend("dev", " Redis "), Ok(()));
        // test / 未知 profile：不强制（临时环境可以退回进程内缓存）
        assert_eq!(check_cache_backend("test", "local"), Ok(()));
        assert_eq!(check_cache_backend("no-such-profile", "local"), Ok(()));
    }

    /// 缓存后端守卫：prod / dev 用进程内缓存即拒绝启动，错误信息含危害与修复。
    #[test]
    fn cache_backend_rejects_local_for_prod_and_dev() {
        for profile in ["prod", "dev"] {
            let err = check_cache_backend(profile, "local")
                .expect_err("prod/dev 使用 local 缓存必须被拒绝");
            assert_eq!(err.profile, profile);
            assert_eq!(err.cache_type, "local");
            let text = err.to_string();
            assert!(
                text.contains("必须为 \"redis\""),
                "错误文案缺修复指引: {text}"
            );
            assert!(
                text.contains("吊销"),
                "错误文案缺危害说明（吊销会静默失效）: {text}"
            );
        }
    }

    // ────────────── 存储主密钥守卫（批次 A · A1） ──────────────

    /// 正常路径：64 位十六进制主密钥放行。
    #[test]
    fn storage_master_key_accepts_hex64() {
        assert_eq!(
            check_storage_master_key("dev", Some(VALID_SECRET_HEX64)),
            Ok(())
        );
        // 首尾空白容忍（env 文件里常带引号或换行）
        let padded = format!("  {VALID_SECRET_HEX64}\n");
        assert_eq!(check_storage_master_key("dev", Some(&padded)), Ok(()));
    }

    /// 错误分支：主密钥缺失 / 空白 → `Missing`（文案含环境变量名与修复命令）。
    #[test]
    fn storage_master_key_missing_blocks_startup() {
        for raw in [None, Some(""), Some("   ")] {
            let error = check_storage_master_key("dev", raw).expect_err("缺失必须被拒绝");
            match &error {
                StorageKeyGuardError::Missing { profile } => assert_eq!(profile, "dev"),
                other => panic!("应返回 Missing，实际为 {other:?}"),
            }
            let text = error.to_string();
            assert!(text.contains(ENV_STORAGE_MASTER_KEY), "{text}");
            assert!(text.contains("openssl rand -hex 32"), "{text}");
        }
    }

    /// 错误分支：长度不符 / 非十六进制 → `Invalid`，且文案里不含密钥内容。
    #[test]
    fn storage_master_key_invalid_rejected() {
        // 63 位
        let short = "a".repeat(MASTER_KEY_HEX_LEN - 1);
        let error = check_storage_master_key("prod", Some(&short)).expect_err("长度不符必须拒绝");
        assert!(matches!(error, StorageKeyGuardError::Invalid { .. }));
        assert!(error.to_string().contains("64"));

        // 64 位但含非法字符
        let bad = "z".repeat(MASTER_KEY_HEX_LEN);
        let error = check_storage_master_key("prod", Some(&bad)).expect_err("非十六进制必须拒绝");
        assert!(matches!(error, StorageKeyGuardError::Invalid { .. }));
        assert!(!error.to_string().contains(&bad), "错误文案泄露了密钥");
    }

    /// 边界：统一出口 `StartupViolation` 的文案与内层一致（预检报错不丢信息）。
    #[test]
    fn startup_violation_delegates_message() {
        let inner = StorageKeyGuardError::Missing {
            profile: "dev".to_string(),
        };
        let violation = StartupViolation::from(inner.clone());
        assert_eq!(violation.to_string(), inner.to_string());
        assert_eq!(violation, StartupViolation::StorageKey(inner));

        let auth = AuthSecretGuardError::Empty {
            profile: "dev".to_string(),
        };
        assert_eq!(
            StartupViolation::from(auth.clone()).to_string(),
            auth.to_string()
        );
    }

    /// 孤儿任务依赖的告警判定（E7）：0 条不告警，>0 告警且点名数量。
    #[test]
    fn orphan_dependency_warning_only_fires_when_positive() {
        assert!(
            orphan_dependency_warning(0).is_none(),
            "0 条孤儿不应产生告警文案"
        );

        let one = orphan_dependency_warning(1).expect("1 条孤儿必须告警");
        assert!(one.contains('1'), "{one}");

        let many = orphan_dependency_warning(17).expect("17 条孤儿必须告警");
        assert!(many.contains("17"), "{many}");
        assert!(many.contains("依赖"), "{many}");
    }
}

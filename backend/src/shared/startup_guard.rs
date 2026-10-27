//! 启动守卫：认证已开启但 JWT 签名密钥缺失 / 过短时**拒绝启动**。
//!
//! # 为什么需要
//!
//! alun 0.4.2 的配置基线允许「`[middleware.auth] enabled = true` 且 `jwt_secret` 为空」
//! 这一组合：此时认证中间件照常挂载，但签名密钥退化成**空字符串**，任何人都能自行
//! 签发 `permissions = ["*:*:*"]` 的令牌绕过登录。dev 的密钥自 P0 修复起只由环境变量
//! `ALUN_MIDDLEWARE_AUTH_JWT_SECRET`（`/etc/cailu/cailu-dev.env`）注入，一旦有人绕过
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

use std::fmt;

use alun::prelude::*;
use async_trait::async_trait;

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
                 修复：① 用 scripts/dev-up.sh 启动（它会 source /etc/cailu/cailu-dev.env 并注入密钥）；\
                 ② 或显式注入环境变量 {ENV_JWT_SECRET}=\"$(openssl rand -hex 32)\"。\n\
                 禁止为了「能跑起来」而把 [middleware.auth] enabled 改成 false。"
            ),
            Self::TooShort { profile, len } => write!(
                f,
                "安全守卫拒绝启动：profile={profile} 下 [middleware.auth] enabled = true，\
                 但签名密钥 jwt_secret 太短（当前 {len} 个字符，要求至少 {MIN_JWT_SECRET_LEN} 个）。\n\
                 危害：弱密钥可被离线爆破，等价于认证失效。\n\
                 修复：重新生成强密钥并注入环境变量 {ENV_JWT_SECRET}=\"$(openssl rand -hex 32)\"；\
                 dev 环境请用 scripts/dev-up.sh 启动（密钥只写在 /etc/cailu/cailu-dev.env，不落 git 跟踪文件）。"
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

/// 启动预检：在 `App::new()` **之前**判定并返回拒绝原因。
///
/// # 返回值
///
/// - `Ok(())`：放行（认证关闭 / 无法判定 / 密钥合规，或当前是 gen-config、print-config 模式）
/// - `Err(AuthSecretGuardError)`：认证明确开启而密钥为空 / 过短
pub fn preflight() -> std::result::Result<(), AuthSecretGuardError> {
    if is_config_only_mode() {
        return Ok(());
    }

    let profile = resolve_profile();
    let auth_enabled = auth_enabled_in_files(DEFAULT_CONFIG_DIR, &profile);
    let secret = resolve_env_secret();

    evaluate_preflight(&profile, auth_enabled, secret.as_deref())
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
                Ok(())
            }
            Err(violation) => {
                tracing::error!(
                    profile = %config.profile,
                    cache_type = %config.cache.r#type,
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
}

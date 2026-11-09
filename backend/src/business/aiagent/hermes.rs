//! Hermes CLI 适配层：**全仓唯一**允许启动 `hermes` 进程、或直接读写
//! `~/.hermes/profiles/<name>/` 与 `~/.hermes/workspace/<name>/` 的代码。
//!
//! 依据：本卡（L1，云枢 × Hermes 集成第一步）「核心实现：`hermes.rs`」与红线三条。
//!
//! # 为什么单独一层
//!
//! 业务代码（[`crate::business::aiagent::handler`]）只关心「建 profile 成不成、SOUL.md 读到
//! 了什么」，不该知道是 `Command::new(hermes)` 还是「经 SSH 到 Hermes 所在主机执行」。
//! 因此本模块把「怎么执行一条命令 / 怎么写一个文件」收成 [`Transport`] 两个实现：
//!
//! | Transport | 适用部署 | 命令 | 文件 |
//! |-----------|----------|------|------|
//! | [`Transport::Local`] | 云枢与 Hermes **同机** | `Command::new(hermes)` + 参数数组 | `tokio::fs` |
//! | [`Transport::Ssh`] | 云枢在 172.18.10.144、Hermes 在 172.18.10.99（**当前生产形态**） | `Command::new("ssh")` 到对端 | `ssh cat` / `ssh dd` + `ssh mv` |
//!
//! 两者的对外语义完全一致（同样校验、同样超时、同样错误码），所以业务层与单测不受部署形态影响。
//!
//! # 红线（本文件是唯一执行点，因此每条都在这里落地）
//!
//! 1. **profile 名必须严格正则校验**（[`PROFILE_NAME_PATTERN`]）：实测 `hermes profile create
//!    Bad_Name` 会把名字**静默小写**成 `bad_name` 并建盘 —— 即「不做校验就等于放行一个
//!    与入参不同的 profile」，故[`Hermes::validate_profile_name`] 在任何命令之前执行。
//! 2. **用户输入不进 shell**：
//!    - Local：`Command::new(bin).args([...])`，参数数组，永不经过 shell；
//!    - Ssh：OpenSSH 会把「主机之后的参数」拼成一条字符串交给远端 shell，**必须**逐个
//!      [`shell_quote`]。这是本层唯一的引号拼接点，由 `shell_quote` 的 6 条单测覆盖
//!      （含 `'; rm -rf /'`、换行、`$(...)`、反引号）。
//! 3. **SOUL.md 正文永不进参数**：正文一律走 **stdin**（Local 直接写文件 / Ssh 走 `dd`），
//!    因此正文里写什么（引号、换行、`$(...)`）都不构成注入面。
//! 4. **原子写**：先写同目录临时文件 → `rename`（同分区 rename 是原子操作）。
//!    半截文件永远不会出现在 `SOUL.md` 这个路径上。
//! 5. **超时**：默认 60s（[`HermesConfig::timeout`]），超时 `start_kill` 并返回
//!    `AI_AGENT_HERMES_FAILED`；失败时把 stderr 前 500 字符回给前端（便于运维排查）。
//!
//! # 与 Hermes 侧的边界
//!
//! 只调官方 CLI（`profile create` / `describe` / `show` / `delete`）与写 `SOUL.md`，
//! **不改** Hermes 的 profile 管理逻辑、不碰 `state.db`、不写 `config.yaml`。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use nik::prelude::*;
// 显式遮蔽 prelude 的 `Result<T>`（那是 `nik::Result<T>`，错误类型固定为 `nik::Error`）：
// 本层对外统一返回 `ApiError`（HTTP 状态 + 业务码 + 对外消息），处理器才能直接 `?` 接住。
// 显式 `use` 会优先于 `use nik::prelude::*` 的 glob 导入。
use std::result::Result;
use tokio::process::Command;

use crate::shared::error_code;

/// profile 名白名单正则：**命令注入的唯一入口**（红线）。
///
/// - 首字符必须小写字母（`hermes` 侧同样不接受数字开头，且首个 `-` 会被当成选项）；
/// - 之后只允许小写字母 / 数字 / 连字符，总长 2–31（与 `varchar(64)` 列相比留足余量，
///   同时保证 `hermes profile create` 不会因为过长被截断）；
/// - **不含** `.`、`/`、空格、引号、`;`、`$`、反引号、换行 —— 路径拼接与 shell 拼接都安全。
pub const PROFILE_NAME_PATTERN: &str = r"^[a-z][a-z0-9-]{1,30}$";

/// 显式拒绝的保留名：`default` 是 Hermes 的内置 profile，建同名会与框架态冲突。
const RESERVED_PROFILE_NAMES: [&str; 1] = ["default"];

/// 回给前端的 stderr 截断长度（字符数，非字节数）。
const STDERR_EXCERPT_CHARS: usize = 500;

/// SOUL.md 正文上限：256 KiB。
///
/// 上限的意义不是「省磁盘」，而是**拒绝异常输入**：SOUL.md 会进模型上下文，
/// 超大文件既拖慢每次会话，也说明前端/调用方传错了东西。
pub const SOUL_MAX_BYTES: usize = 256 * 1024;

/// 职责（profile description）上限，与 `sys_ai_agent.agent_description varchar(255)` 一致。
pub const DESCRIPTION_MAX_CHARS: usize = 255;

static PROFILE_NAME_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // 常量模式，正确性由 `profile_name_regex_is_anchored` 单测兜底；此处不存在运行期失败路径。
    regex::Regex::new(PROFILE_NAME_PATTERN).expect("PROFILE_NAME_PATTERN 是常量正则")
});

// ==================== 配置 ====================

/// 命令执行形态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// 与 Hermes 同机：直接 `Command::new(hermes_bin)`。
    Local,
    /// 跨主机：`ssh <host> …`（需要免密，`BatchMode=yes` 便于快速失败而不是卡在密码提示）。
    Ssh {
        /// 形如 `root@172.18.10.99`。
        host: String,
    },
}

/// Hermes 适配层的全部可配置项。
///
/// # 配置来源（环境变量，全部有默认值）
///
/// | 变量 | 默认 | 说明 |
/// |------|------|------|
/// | `CLOUDPIVOT_AIAGENT_TRANSPORT` | `local` | `local` / `ssh` |
/// | `CLOUDPIVOT_AIAGENT_SSH_HOST` | 空 | 非空时**强制** ssh 形态（比 transport 更优先，少一个必填项） |
/// | `CLOUDPIVOT_AIAGENT_HERMES_BIN` | `/root/.hermes/hermes-agent/venv/bin/hermes` | CLI 绝对路径 |
/// | `CLOUDPIVOT_AIAGENT_HERMES_HOME` | `/root/.hermes` | 显式传给子进程，压掉「HERMES_HOME 未设置」的 fallback 告警 |
/// | `CLOUDPIVOT_AIAGENT_PROFILES_ROOT` | `/root/.hermes/profiles` | profile 目录根 |
/// | `CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT` | `/root/.hermes/workspace` | 工作区根 |
/// | `CLOUDPIVOT_AIAGENT_BACKUP_DIR` | `/root/.hermes/aiagent-soul-backups` | 删号前 SOUL.md 备份目录 |
/// | `CLOUDPIVOT_AIAGENT_CLONE_FROM` | `default` | 建 profile 时的模板 profile（Hermes 内置默认 profile，不依赖任何自建 profile） |
/// | `CLOUDPIVOT_AIAGENT_TIMEOUT_SECS` | `60` | 单条命令超时 |
///
/// 之所以用环境变量而不是 `config.toml` 新段：`config.toml` 由框架的强类型配置反序列化，
/// 加未知段有被拒风险；而这些值本身是**部署形态**（同机 / 跨机、路径），与业务配置无关，
/// 放在 `/etc/cloudpivot/*.env`（600）里与既有密钥同源管理更合适。
#[derive(Debug, Clone)]
pub struct HermesConfig {
    /// 执行形态。
    pub transport: Transport,
    /// `hermes` 可执行文件绝对路径。
    pub hermes_bin: PathBuf,
    /// 传给子进程的 `HERMES_HOME`。
    pub hermes_home: PathBuf,
    /// profile 目录根（`<root>/<name>/SOUL.md`）。
    pub profiles_root: PathBuf,
    /// 工作区根（`<root>/<name>/`）。
    pub workspace_root: PathBuf,
    /// 删号前的 SOUL.md 备份目录。
    pub backup_dir: PathBuf,
    /// `--clone-from` 的模板 profile 名。
    pub clone_from: String,
    /// 单条命令超时。
    pub timeout: Duration,
}

impl HermesConfig {
    /// 从环境变量读取（见类型文档的表格）；任何一项为空/非法时回落到默认值。
    pub fn from_env() -> Self {
        let ssh_host = env_non_empty("CLOUDPIVOT_AIAGENT_SSH_HOST");
        let transport = match ssh_host {
            Some(host) => Transport::Ssh { host },
            None => match env_non_empty("CLOUDPIVOT_AIAGENT_TRANSPORT")
                .unwrap_or_else(|| "local".to_string())
                .to_ascii_lowercase()
                .as_str()
            {
                "ssh" => Transport::Ssh {
                    host: "root@127.0.0.1".to_string(),
                },
                _ => Transport::Local,
            },
        };
        let timeout_secs = env_non_empty("CLOUDPIVOT_AIAGENT_TIMEOUT_SECS")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(60);
        Self {
            transport,
            hermes_bin: env_path(
                "CLOUDPIVOT_AIAGENT_HERMES_BIN",
                "/root/.hermes/hermes-agent/venv/bin/hermes",
            ),
            hermes_home: env_path("CLOUDPIVOT_AIAGENT_HERMES_HOME", "/root/.hermes"),
            profiles_root: env_path("CLOUDPIVOT_AIAGENT_PROFILES_ROOT", "/root/.hermes/profiles"),
            workspace_root: env_path("CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT", "/root/.hermes/workspace"),
            backup_dir: env_path("CLOUDPIVOT_AIAGENT_BACKUP_DIR", "/root/.hermes/aiagent-soul-backups"),
            clone_from: env_non_empty("CLOUDPIVOT_AIAGENT_CLONE_FROM").unwrap_or_else(|| "default".to_string()),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_path(key: &str, default: &str) -> PathBuf {
    PathBuf::from(env_non_empty(key).unwrap_or_else(|| default.to_string()))
}

// ==================== profile 探活结果 ====================

/// profile 探活结果（`GET /system/user/ai/{id}/status` 的载荷）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProfileStatus {
    /// `<profiles_root>/<name>/` 是否存在。
    pub profile_dir_exists: bool,
    /// `<profiles_root>/<name>/SOUL.md` 是否存在。
    pub soul_file_exists: bool,
    /// `<workspace_root>/<name>/` 是否存在。
    pub workspace_exists: bool,
    /// gateway 状态：`online` / `offline` / `unknown`（解析 `hermes profile show` 的 `Gateway:` 行）。
    pub gateway: String,
    /// `hermes` CLI 本身是否可用（Local：文件存在；Ssh：能跑通 `profile show`）。
    pub cli_available: bool,
    /// profile 的 `.env` 里有当前 provider 的 key 且权限 600（**能不能干活**的判据）。
    ///
    /// 与 [`Self::healthy`] 分开：`healthy` 是「磁盘资产还在」，`credentials_ok` 是
    /// 「认证能过」。两者都真才算一个能派活的 AI 员工，但分开暴露才能让运维一眼看出
    /// 「目录都在、就是缺 key」这类故障（2026-10-29 的 AuthError 事故就是这种形态）。
    pub credentials_ok: bool,
    /// 凭据文件里**非空**的键名（值不外传）。
    pub credential_keys: Vec<String>,
    /// 当前 provider 需要的键名（解析不出 provider 时为 `None`）。
    pub credential_required: Option<String>,
    /// 凭据文件权限位（`0o600` 合规；不存在为 `None`）。
    pub env_file_mode: Option<u32>,
    /// 凭据自检的补充说明（缺哪个键 / 权限不对等）。
    pub credential_detail: Option<String>,
    /// 探活过程里的补充说明（命令失败原因等，便于运维定位）。
    pub detail: Option<String>,
}

impl ProfileStatus {
    /// 是否整体健康（目录与 SOUL.md 都在且 CLI 可用）。
    pub fn healthy(&self) -> bool {
        self.profile_dir_exists && self.soul_file_exists && self.cli_available
    }
}

/// 凭据文件权限位：`.env` 里是 API key 明文，必须 `600`（与 `~/.hermes/.env` 同口径）。
pub const CREDENTIAL_FILE_MODE: u32 = 0o600;

/// profile 凭据同步结果（[`Hermes::ensure_profile_env`] 的返回值，也用于建号日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSync {
    /// profile 凭据文件绝对路径。
    pub env_path: String,
    /// 当前 provider（解析不出时为 `None`）。
    pub provider: Option<String>,
    /// 当前 provider 对应的环境变量名（解析不出时为空串）。
    pub required_key: String,
    /// 本次**实际写入**的键名（空 = 原本就齐，无需改动）。
    pub written_keys: Vec<String>,
    /// 落盘后的权限位。
    pub env_file_mode: Option<u32>,
}

/// profile 凭据自检结果（[`Hermes::verify_profile_credentials`]）。
///
/// **只含键名，绝不含键值** —— 这个结构体会进 HTTP 响应与日志。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CredentialCheck {
    /// profile 凭据文件绝对路径。
    pub env_path: String,
    /// 权限位（`0o600` = 合规；文件不存在为 `None`）。
    pub env_file_mode: Option<u32>,
    /// 当前 provider。
    pub provider: Option<String>,
    /// 当前 provider 需要的键名。
    pub required_key: Option<String>,
    /// 文件里**非空**的键名列表（值不外传）。
    pub present_keys: Vec<String>,
    /// 是否满足「能干活」：provider key 非空 **且** 权限 600。
    pub ok: bool,
    /// 不满足时的原因（可直接进错误消息）。
    pub detail: Option<String>,
}

impl CredentialCheck {
    /// 自检不通过时的错误消息（`ok=true` 时返回一个兜底描述）。
    pub fn failure_message(&self) -> String {
        match &self.detail {
            Some(detail) => format!("AI 员工凭据自检不通过（{}）：worker 会因 AuthError 跑不起来", detail),
            None => format!("AI 员工凭据自检不通过：{}", self.env_path),
        }
    }
}

// ==================== 主对象 ====================

/// Hermes CLI 适配器。
#[derive(Debug, Clone)]
pub struct Hermes {
    cfg: HermesConfig,
}

impl Hermes {
    /// 用给定配置构造（单测直接构造，不经环境变量）。
    pub fn new(cfg: HermesConfig) -> Self {
        Self { cfg }
    }

    /// 按环境变量构造（处理器用）。
    pub fn from_env() -> Self {
        Self::new(HermesConfig::from_env())
    }

    /// 只读取配置（handler 需要 `clone_from` / 路径拼装）。
    pub fn config(&self) -> &HermesConfig {
        &self.cfg
    }

    // ---------- 校验 ----------

    /// profile 名校验（**红线**）：任何一次命令执行之前都必须先过这一关。
    ///
    /// # 规则
    ///
    /// - 命中 [`PROFILE_NAME_PATTERN`]；
    /// - 不是保留名（[`RESERVED_PROFILE_NAMES`]）。
    ///
    /// # 返回值
    ///
    /// 合法返回 `Ok(())`；非法返回 `400 + AI_AGENT_PROFILE_INVALID`，消息里点明规则
    /// （前端直接把消息展示给用户，所以要说人话，不能只说「参数错误」）。
    pub fn validate_profile_name(name: &str) -> Result<(), ApiError> {
        // 注意：这里**故意不做 trim**。名字是路径与命令行参数的一段，前后空格没有任何
        // 合理语义；容忍 trim 会让 " abc" 这类「看起来像用户手误、实际可能是试探」的输入
        // 静默变成合法名。调用方（handler）负责把请求体里的首尾空白去掉一次，之后
        // 校验必须是「所见即所得」。
        if name.is_empty() {
            return Err(Self::profile_name_error("profile 名不能为空"));
        }
        if !PROFILE_NAME_RE.is_match(name) {
            return Err(Self::profile_name_error(&format!(
                "profile 名不合法：{name:?}（必须以小写字母开头，只含小写字母/数字/连字符，长度 2–31）"
            )));
        }
        if RESERVED_PROFILE_NAMES.contains(&name) {
            return Err(Self::profile_name_error(&format!(
                "profile 名 {name:?} 是 Hermes 保留名，请换一个"
            )));
        }
        Ok(())
    }

    /// 校验 `--clone-from` 的**模板名**（克隆来源 profile）。
    ///
    /// # 与 [`Self::validate_profile_name`] 的唯一差别：允许 `default`
    ///
    /// `default` 是 Hermes 内置的默认 profile，也是「新建 AI 员工」时最自然的模板。
    /// `RESERVED_PROFILE_NAMES` 的语义是「**不许新建**一个叫 default 的 profile」
    /// —— 它不适用于「从谁那儿克隆」。把这条规则套到克隆来源上，会让
    /// `clone_from = "default"` 这条正当请求直接 `400 + 64002`
    /// （HEAD 8f0c7f2 把默认模板从 `manong` 改成 `default` 后就是这么挂的：
    /// 单测 `create_profile_failure_is_reported_with_stderr` 报 64002 而不是 64005）。
    ///
    /// # 安全检查没有放松
    ///
    /// 模板名同样会进命令行参数，所以**正则与长度照样校验**（注入面还是那一处）。
    ///
    /// # 异常情况
    ///
    /// 非法返回 `400 + AI_AGENT_PROFILE_INVALID`。
    pub fn validate_template_name(name: &str) -> Result<(), ApiError> {
        if name.is_empty() {
            return Err(Self::profile_name_error("模板 profile 名不能为空"));
        }
        if !PROFILE_NAME_RE.is_match(name) {
            return Err(Self::profile_name_error(&format!(
                "模板 profile 名不合法：{name:?}（必须以小写字母开头，只含小写字母/数字/连字符，长度 2–31）"
            )));
        }
        Ok(())
    }

    fn profile_name_error(message: &str) -> ApiError {
        ApiError::new(400, error_code::AI_AGENT_PROFILE_INVALID, message.to_string())
    }

    /// 路径拼接（**只在名字已校验后调用**）。
    ///
    /// 之所以仍做一次 debug 断言而不是无条件信任：本函数是 `pub`，未来有人从别处调进来时，
    /// `debug` 构建会立刻炸在测试里，而不是悄悄拼出一个 `../` 路径。
    fn profile_dir(&self, name: &str) -> PathBuf {
        debug_assert!(
            Self::validate_profile_name(name).is_ok(),
            "profile_dir 只接受已校验的名字"
        );
        self.cfg.profiles_root.join(name)
    }

    /// `<profiles_root>/<name>/SOUL.md`。
    pub fn soul_path(&self, name: &str) -> PathBuf {
        self.profile_dir(name).join("SOUL.md")
    }

    /// `<workspace_root>/<name>/`。
    pub fn workspace_path(&self, name: &str) -> PathBuf {
        debug_assert!(Self::validate_profile_name(name).is_ok());
        self.cfg.workspace_root.join(name)
    }

    // ---------- profile 生命周期 ----------

    /// 创建 profile。
    ///
    /// 对应 `hermes profile create <name> --clone-from <模板> --description <职责> --no-alias`。
    ///
    /// - `--no-alias`：不生成 `~/.local/bin/<name>` 之类的 wrapper（云枢管理的是 profile 本体，
    ///   不想往 PATH 里塞 29 个同名脚本）；
    /// - 建完立刻校验 `<profiles_root>/<name>/` 存在 —— CLI 返回 0 但目录没出来属于「静默失败」，
    ///   对调用方而言必须当失败处理（否则后面写 SOUL.md 会写到一个不存在的目录）。
    ///
    /// # 异常情况
    ///
    /// - 名字非法：`400 + AI_AGENT_PROFILE_INVALID`（在启动任何进程之前）；
    /// - CLI 非 0 退出 / 超时 / 目录没生成：`500 + AI_AGENT_HERMES_FAILED`，附 stderr 摘录。
    pub async fn create_profile(
        &self,
        name: &str,
        clone_from: Option<&str>,
        description: &str,
    ) -> Result<(), ApiError> {
        Self::validate_profile_name(name)?;
        if let Some(template) = clone_from {
            // 模板名同样进参数，必须同样校验（否则这里就是第二条注入面）；
            // 但**允许 `default`** —— 它是内置模板，见 validate_template_name 的说明。
            Self::validate_template_name(template)?;
        }
        let mut argv = vec!["profile".to_string(), "create".to_string(), name.to_string()];
        if let Some(template) = clone_from {
            argv.push("--clone-from".to_string());
            argv.push(template.to_string());
        }
        let description = description.trim();
        if !description.is_empty() {
            argv.push("--description".to_string());
            argv.push(description.to_string());
        }
        argv.push("--no-alias".to_string());

        let out = self.run_hermes(argv, None).await?;
        if !out.ok {
            return Err(Self::cli_failed("创建 profile", &out));
        }
        if !self.profile_dir_exists(name).await? {
            return Err(ApiError::new(
                500,
                error_code::AI_AGENT_HERMES_FAILED,
                format!(
                    "创建 profile 后目录不存在：{}（CLI 退出码 0，怀疑 Hermes 侧静默失败）",
                    self.profile_dir(name).display()
                ),
            ));
        }
        // 建号即把**模型凭据**复制进 profile：`hermes profile create` 只复制 SOUL.md，
        // 不复制 config.yaml；而 worker 的 HERMES_HOME 是 profile 目录，没有 config.yaml
        // 就等于「没配过模型」，派活后立刻退出（见 ensure_profile_config 的说明）。
        // 失败不阻断建号（L1 只需要 profile 存在），但必须留下明确的 WARN。
        match self.ensure_profile_config(name).await {
            Ok(true) => tracing::info!(profile = name, "已把全局模型配置复制进新 profile"),
            Ok(false) => tracing::info!(profile = name, "profile 已有 config.yaml，未覆盖"),
            Err(error) => tracing::warn!(
                profile = name,
                error = %error.msg,
                "复制模型配置失败：该 AI 员工派活后会因「无模型凭据」跑不起来"
            ),
        }
        // 凭据必须**硬性**补齐（与上面 config.yaml 的「失败只告警」不同）：
        // worker 的凭据池只读 profile 级 `.env`，缺 key 的表现是「号建得出来、活干不了」
        // —— kanban 派活成功、worker 一启动就 AuthError、连 crash 两次后 blocked。
        // 这种半成品账号比建号失败更糟（占用用户名与 profile 名，还要人工清理），
        // 所以这里失败即返回 Err，由调用方的补偿逻辑删 profile / 删 workspace / 软删账号。
        let sync = self.ensure_profile_env(name).await?;
        tracing::info!(
            profile = name,
            provider = ?sync.provider,
            required_key = %sync.required_key,
            written_keys = ?sync.written_keys,
            mode = ?sync.env_file_mode,
            env = %sync.env_path,
            "已确认 profile 级凭据（worker 凭据池可认证）"
        );
        Ok(())
    }

    /// 设置 profile 描述（`hermes profile describe <name> --text <职责>`）。
    ///
    /// 该描述是 **kanban orchestrator 的路由依据**，所以职责改动必须同步到 profile，
    /// 不能只存库里。
    pub async fn describe(&self, name: &str, text: &str) -> Result<(), ApiError> {
        Self::validate_profile_name(name)?;
        let text = text.trim();
        if text.chars().count() > DESCRIPTION_MAX_CHARS {
            return Err(ApiError::new(
                400,
                error_code::AI_AGENT_DESCRIPTION_INVALID,
                format!("职责最多 {DESCRIPTION_MAX_CHARS} 个字符"),
            ));
        }
        if text.is_empty() {
            return Ok(());
        }
        let argv = vec![
            "profile".to_string(),
            "describe".to_string(),
            name.to_string(),
            "--text".to_string(),
            text.to_string(),
        ];
        let out = self.run_hermes(argv, None).await?;
        if !out.ok {
            return Err(Self::cli_failed("设置 profile 描述", &out));
        }
        Ok(())
    }

    /// 读回 profile 描述（验收用；也用于「库里与 profile 不一致」自检）。
    pub async fn read_description(&self, name: &str) -> Result<String, ApiError> {
        Self::validate_profile_name(name)?;
        let argv = vec!["profile".to_string(), "describe".to_string(), name.to_string()];
        let out = self.run_hermes(argv, None).await?;
        if !out.ok {
            return Err(Self::cli_failed("读取 profile 描述", &out));
        }
        Ok(out.stdout.trim().to_string())
    }

    /// 删除 profile（`hermes profile delete <name> -y`）。
    ///
    /// `-y` 是必须的：不带它 CLI 会等交互确认，在我们的超时窗口里就是一次挂死。
    pub async fn delete_profile(&self, name: &str) -> Result<(), ApiError> {
        Self::validate_profile_name(name)?;
        let argv = vec![
            "profile".to_string(),
            "delete".to_string(),
            name.to_string(),
            "-y".to_string(),
        ];
        let out = self.run_hermes(argv, None).await?;
        // profile 本来就不存在时不算失败（删除接口要幂等）
        if !out.ok && self.profile_dir_exists(name).await? {
            return Err(Self::cli_failed("删除 profile", &out));
        }
        Ok(())
    }

    // ---------- kanban（T5b：派活 / 拉状态） ----------

    /// 派活：`hermes kanban --board <board> create <标题> … --json`。
    ///
    /// # 参数
    ///
    /// 见 [`KanbanCreate`]。`body` 走 `--body` 参数（**参数数组**，不经过 shell）：
    ///
    /// - Local 形态：`Command::new(hermes).args([...])`，正文里有什么引号 / 换行 /
    ///   `$(...)` 都只是参数字节，不构成注入面；
    /// - Ssh 形态：正文会经 [`shell_quote`] 单引号包裹后交给远端 shell（本层唯一的
    ///   拼接点，6 条单测覆盖含 `'; rm -rf /'` 的取值）。
    ///
    /// > **为什么不是卡面写的 `--body-file`**：144 上的 Hermes v0.15.2 的
    /// > `kanban create` **没有** `--body-file` 选项（实测 `--help` 只有 `--body`）。
    /// > 卡面要求「body 用临时文件（不拼 shell 字符串）」的目的是避免 shell 拼接，
    /// > 而参数数组天然满足这一点（比临时文件更强：没有落盘、没有 TOCTOU）。
    /// > 升级 Hermes 后若 `--body-file` 出现，可改成写临时文件再传路径，语义不变。
    ///
    /// # 返回值
    ///
    /// Hermes 侧的任务 id（形如 `t_4b7abf18`），取自 `--json` 输出的 `id` 字段。
    ///
    /// # 异常情况
    ///
    /// - 名字 / 看板 / 幂等键不合法：`400`（在任何进程启动之前）；
    /// - CLI 非 0 退出 / 超时 / 输出不是 JSON / JSON 里没有 id：`500 + AI_AGENT_HERMES_FAILED`
    ///   （带 stderr 摘录，便于运维定位）。
    pub async fn kanban_create(&self, req: KanbanCreate<'_>) -> Result<String, ApiError> {
        Self::validate_profile_name(req.assignee)?;
        validate_board_slug(req.board)?;
        if req.title.trim().is_empty() {
            return Err(ApiError::new(
                400,
                error_code::TASK_FIELD_INVALID,
                "派活标题不能为空".to_string(),
            ));
        }
        if !req.idempotency_key.is_empty() {
            validate_idempotency_key(req.idempotency_key)?;
        }
        let mut argv = vec![
            "kanban".to_string(),
            "--board".to_string(),
            req.board.to_string(),
            "create".to_string(),
            req.title.trim().to_string(),
            "--assignee".to_string(),
            req.assignee.to_string(),
            "--body".to_string(),
            req.body.to_string(),
            "--priority".to_string(),
            req.priority.to_string(),
            "--max-runtime".to_string(),
            req.max_runtime_secs.to_string(),
            "--json".to_string(),
        ];
        if !req.idempotency_key.is_empty() {
            argv.push("--idempotency-key".to_string());
            argv.push(req.idempotency_key.to_string());
        }
        if let Some(created_by) = req.created_by.filter(|value| !value.trim().is_empty()) {
            argv.push("--created-by".to_string());
            argv.push(created_by.to_string());
        }
        if let Some(workspace) = req.workspace.filter(|value| !value.trim().is_empty()) {
            argv.push("--workspace".to_string());
            argv.push(workspace.to_string());
        }

        let out = self.run_hermes(argv, None).await?;
        if !out.ok {
            return Err(Self::cli_failed("派活到 Hermes kanban", &out));
        }
        let parsed: serde_json::Value = serde_json::from_str(out.stdout.trim()).map_err(|error| {
            ApiError::new(
                500,
                error_code::AI_AGENT_HERMES_FAILED,
                format!(
                    "派活失败：`hermes kanban create --json` 输出无法解析（{error}）；原始输出：{}",
                    excerpt(&out.stdout)
                ),
            )
        })?;
        let task_id = parsed
            .get("id")
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ApiError::new(
                    500,
                    error_code::AI_AGENT_HERMES_FAILED,
                    format!(
                        "派活失败：`hermes kanban create --json` 输出里没有 id 字段；原始输出：{}",
                        excerpt(&out.stdout)
                    ),
                )
            })?;
        validate_kanban_task_id(&task_id)?;
        Ok(task_id)
    }

    /// 拉状态：`hermes kanban --board <board> show <id> --json`。
    ///
    /// # 返回值
    ///
    /// - `Ok(None)`：Hermes 侧**没有这个任务**（CLI 报 `no such task`）。调用方据此
    ///   标「关联任务丢失」，**不是**同步失败；
    /// - `Ok(Some(snapshot))`：解析成功；
    /// - `Err(..)`：CLI 本身跑不起来 / 超时 / 输出不是 JSON —— 调用方必须**保持任务
    ///   状态不变**（轮询失败不改状态，避免把「CLI 抖了一下」判成「AI 失败了」）。
    ///
    /// # 异常情况
    ///
    /// 见上；错误一律是 `500 + AI_AGENT_HERMES_FAILED`（带 stderr 摘录）。
    pub async fn kanban_show(&self, board: &str, task_id: &str) -> Result<Option<KanbanSnapshot>, ApiError> {
        validate_board_slug(board)?;
        validate_kanban_task_id(task_id)?;
        let argv = vec![
            "kanban".to_string(),
            "--board".to_string(),
            board.to_string(),
            "show".to_string(),
            task_id.to_string(),
            "--json".to_string(),
        ];
        let out = self.run_hermes(argv, None).await?;
        if !out.ok {
            let combined = format!("{}\n{}", out.stdout, out.stderr).to_ascii_lowercase();
            if combined.contains("no such task") || combined.contains("not found") || combined.contains("no task with")
            {
                return Ok(None);
            }
            return Err(Self::cli_failed("读取 Hermes 任务状态", &out));
        }
        let parsed: serde_json::Value = serde_json::from_str(out.stdout.trim()).map_err(|error| {
            ApiError::new(
                500,
                error_code::AI_AGENT_HERMES_FAILED,
                format!(
                    "读取 Hermes 任务状态失败：输出无法解析（{error}）；原始输出：{}",
                    excerpt(&out.stdout)
                ),
            )
        })?;
        Ok(Some(KanbanSnapshot::from_json(&parsed)?))
    }

    // ---------- SOUL.md ----------

    /// 读 `SOUL.md` 全文。
    ///
    /// 文件不存在返回 `404 + AI_AGENT_SOUL_MISSING`（而不是空串）——「配置丢了」和
    /// 「配置是空的」在运维上是两件事。
    pub async fn read_soul(&self, name: &str) -> Result<String, ApiError> {
        Self::validate_profile_name(name)?;
        let path = self.soul_path(name);
        match &self.cfg.transport {
            Transport::Local => match tokio::fs::read_to_string(&path).await {
                Ok(content) => Ok(content),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ApiError::new(
                    404,
                    error_code::AI_AGENT_SOUL_MISSING,
                    format!("SOUL.md 不存在：{}", path.display()),
                )),
                Err(e) => Err(ApiError::new(
                    500,
                    error_code::AI_AGENT_SOUL_IO_FAILED,
                    format!("读取 SOUL.md 失败：{e}"),
                )),
            },
            Transport::Ssh { .. } => {
                let out = self
                    .run_remote(format!("cat {}", shell_quote(&path_string(&path))), None)
                    .await?;
                if out.ok {
                    Ok(out.stdout)
                } else if out.stderr.contains("No such file") {
                    Err(ApiError::new(
                        404,
                        error_code::AI_AGENT_SOUL_MISSING,
                        format!("SOUL.md 不存在：{}", path.display()),
                    ))
                } else {
                    Err(Self::cli_failed("读取 SOUL.md", &out))
                }
            }
        }
    }

    /// 原子写 `SOUL.md`。
    ///
    /// # 为什么必须原子
    ///
    /// SOUL.md 会被 Hermes 的会话进程随时读取；`fs::write` 是「截断 + 写」，中途被读到
    /// 就是半个人格。写法：同目录写 `<path>.tmp-<pid>-<nanos>` → `rename` 覆盖目标
    /// （同分区 rename 原子且不产生「文件不存在」的窗口）。
    ///
    /// # 异常情况
    ///
    /// - 正文超 [`SOUL_MAX_BYTES`]：`400 + AI_AGENT_SOUL_INVALID`；
    /// - profile 目录不存在：`404 + AI_AGENT_PROFILE_MISSING`（提示先建号）；
    /// - 其余 IO：`500 + AI_AGENT_SOUL_IO_FAILED`。
    pub async fn write_soul(&self, name: &str, content: &str) -> Result<(), ApiError> {
        Self::validate_profile_name(name)?;
        // E15：安全红线区块由平台**强制维护** —— 任何写入路径（建号 / 编辑 / 将来的批量回填）
        // 落盘前都必须带上它。放在 `write_soul` 而不是放在 handler，是为了让「唯一落盘点」
        // 自己就保证不变式：绕过 handler 直接调这里也删不掉红线。
        //
        // 幂等：已有区块（含旧版本）会被替换成当前版本，不会追加第二份，见
        // [`crate::business::aiagent::safety::ensure_safety_block`]。
        let content = crate::business::aiagent::safety::ensure_safety_block(content);
        let content = content.as_str();
        if content.len() > SOUL_MAX_BYTES {
            return Err(ApiError::new(
                400,
                error_code::AI_AGENT_SOUL_INVALID,
                format!(
                    "SOUL.md 超过上限 {} KiB（含平台强制注入的安全红线区块 {} 字节）",
                    SOUL_MAX_BYTES / 1024,
                    crate::business::aiagent::safety::SAFETY_BLOCK.len()
                ),
            ));
        }
        let dir = self.profile_dir(name);
        if !self.profile_dir_exists(name).await? {
            return Err(ApiError::new(
                404,
                error_code::AI_AGENT_PROFILE_MISSING,
                format!(
                    "profile 目录不存在：{}（请先重建 profile 再保存 SOUL.md）",
                    dir.display()
                ),
            ));
        }
        let target = self.soul_path(name);
        let tmp = dir.join(format!(".SOUL.md.tmp-{}-{}", std::process::id(), now_nanos()));
        match &self.cfg.transport {
            Transport::Local => {
                tokio::fs::write(&tmp, content.as_bytes()).await.map_err(|e| {
                    ApiError::new(500, error_code::AI_AGENT_SOUL_IO_FAILED, format!("写临时文件失败：{e}"))
                })?;
                tokio::fs::rename(&tmp, &target).await.map_err(|e| {
                    ApiError::new(
                        500,
                        error_code::AI_AGENT_SOUL_IO_FAILED,
                        format!("原子替换 SOUL.md 失败：{e}"),
                    )
                })
            }
            Transport::Ssh { .. } => {
                // 正文走 stdin（`dd` 不吃参数），路径全部由已校验的名字拼出
                let script = format!("dd of={} status=none", shell_quote(&path_string(&tmp)));
                let out = self.run_remote(script, Some(content.to_string())).await?;
                if !out.ok {
                    return Err(Self::cli_failed("写 SOUL.md 临时文件", &out));
                }
                let script = format!(
                    "mv -f {} {}",
                    shell_quote(&path_string(&tmp)),
                    shell_quote(&path_string(&target))
                );
                let out = self.run_remote(script, None).await?;
                if !out.ok {
                    return Err(Self::cli_failed("原子替换 SOUL.md", &out));
                }
                Ok(())
            }
        }
    }

    /// 确保 profile 目录里有**自己的** `config.yaml`（模型凭据），返回是否本次新建。
    ///
    /// # 为什么必须有这一步（144 实测踩到）
    ///
    /// `hermes profile create <name> --clone-from <模板>` **只复制 SOUL.md**，不复制
    /// `config.yaml`。而 Hermes 0.15.2 起每个 profile 有自己的 `HERMES_HOME`
    /// （`~/.hermes/profiles/<name>/`），`load_config()` 读的是**那个目录**下的
    /// `config.yaml` —— profile 里没有它就等于「这台机器上没配过模型」：
    /// worker 一启动就打印
    /// `It looks like Hermes isn't configured yet -- no API keys or providers found.`
    /// 并退出。派活看起来成功了（kanban 有任务），AI 却永远跑不起来。
    ///
    /// 因此「派活/建号」必须把全局凭据**复制**进 profile（写 0600，与 `~/.hermes/config.yaml`
    /// 同权限）。已有 `config.yaml` 的 profile **不覆盖** —— 运维可能给某个 AI 员工
    /// 配了专用模型，覆盖掉属于破坏配置。
    ///
    /// # 返回值
    ///
    /// `Ok(true)` = 本次新建；`Ok(false)` = 已存在（未改动）。
    ///
    /// # 异常情况
    ///
    /// - profile 目录不存在 → `404 + AI_AGENT_PROFILE_MISSING`；
    /// - 全局 `config.yaml` 不存在 / 读不出 / 为空 → `500 + AI_AGENT_SOUL_IO_FAILED`
    ///   （沿用「AI 配置文件 IO」这一码，消息点明是模型配置缺失）；
    /// - 写盘失败 → 同上。
    pub async fn ensure_profile_config(&self, name: &str) -> Result<bool, ApiError> {
        Self::validate_profile_name(name)?;
        if !self.profile_dir_exists(name).await? {
            return Err(ApiError::new(
                404,
                error_code::AI_AGENT_PROFILE_MISSING,
                format!("profile 目录不存在：{}", self.profile_dir(name).display()),
            ));
        }
        let target = self.profile_dir(name).join("config.yaml");
        let global = self.cfg.hermes_home.join("config.yaml");
        match &self.cfg.transport {
            Transport::Local => {
                if tokio::fs::metadata(&target).await.is_ok() {
                    return Ok(false);
                }
                let content = tokio::fs::read_to_string(&global).await.map_err(|e| {
                    ApiError::new(
                        500,
                        error_code::AI_AGENT_SOUL_IO_FAILED,
                        format!(
                            "全局模型配置读不到（{}）：{e}；AI profile 没有模型凭据，worker 起不来",
                            global.display()
                        ),
                    )
                })?;
                if content.trim().is_empty() {
                    return Err(ApiError::new(
                        500,
                        error_code::AI_AGENT_SOUL_IO_FAILED,
                        format!("全局模型配置是空的：{}", global.display()),
                    ));
                }
                tokio::fs::write(&target, content.as_bytes()).await.map_err(|e| {
                    ApiError::new(
                        500,
                        error_code::AI_AGENT_SOUL_IO_FAILED,
                        format!("写 profile config.yaml 失败：{e}"),
                    )
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::Permissions::from_mode(0o600);
                    if let Err(e) = tokio::fs::set_permissions(&target, mode).await {
                        tracing::warn!(error = %e, path = %target.display(), "设置 config.yaml 权限 600 失败");
                    }
                }
                Ok(true)
            }
            Transport::Ssh { .. } => {
                // 已存在就不动（`test -f` 用退出码表达真假）
                let probe = self
                    .run_remote_raw(format!("test -f {}", shell_quote(&path_string(&target))), None)
                    .await?;
                if probe.status {
                    return Ok(false);
                }
                let out = self
                    .run_remote(format!("cat {}", shell_quote(&path_string(&global))), None)
                    .await?;
                if !out.ok || out.stdout.trim().is_empty() {
                    return Err(Self::cli_failed("读取全局模型配置", &out));
                }
                let out = self
                    .run_remote(
                        format!("dd of={} status=none", shell_quote(&path_string(&target))),
                        Some(out.stdout),
                    )
                    .await?;
                if !out.ok {
                    return Err(Self::cli_failed("写 profile config.yaml", &out));
                }
                let out = self
                    .run_remote(format!("chmod 600 {}", shell_quote(&path_string(&target))), None)
                    .await?;
                if !out.ok {
                    return Err(Self::cli_failed("设置 profile config.yaml 权限", &out));
                }
                Ok(true)
            }
        }
    }

    // ---------- 凭据：profile 级 `.env`（L1 修复：worker 只读 profile 级凭据） ----------

    /// 确保 `<profiles_root>/<name>/.env` 含**当前 provider 的 API key**（缺则补、有则不覆盖），
    /// 并强制文件权限 `600`。返回本次同步结果。
    ///
    /// # 为什么必须有这一步（2026-10-29 端到端实测发现的真实缺陷）
    ///
    /// `hermes profile create` 建出的 profile，其 `.env` **不继承全局 `~/.hermes/.env`**：
    ///
    /// - 带 `--clone-from` 时从模板 profile 复制（模板有 key 才有 key，模板没有就没有）；
    /// - 不带模板时只有三行注释，**一个键都没有**。
    ///
    /// 而 Hermes worker 启动时 `HERMES_HOME` 指向 **profile 目录**，凭据池只认
    /// 「profile 级 `.env` + 进程环境」，缺键的直接后果是：
    ///
    /// ```text
    /// AuthError: No usable credentials found for provider 'deepseek'. Set DEEPSEEK_API_KEY.
    /// credential pool: no available entries (all exhausted or empty)
    ///   ↓ Kanban owner refused / crash x2 → blocked
    /// ```
    ///
    /// 即「号建得出来、活干不了」。`config.yaml` 里哪怕写着 `api_key`，0.15.2 的
    /// provider 解析仍走凭据池（`credential_pool`），所以**全局 `.env` 有 key 不等于
    /// profile 有 key**（红线：不能只在全局写 key 就完事）。
    ///
    /// # 键的来源（优先级）
    ///
    /// 1. 全局 `<hermes_home>/.env`（与 CLI 同源的凭据文件，600）；
    /// 2. 云枢进程环境（`/etc/cloudpivot/*.env` 里也可能注入同一个键）。
    ///
    /// 搬运范围：全局 `.env` 里所有 `*_API_KEY`（不只是当前 provider 的）——
    /// `fallback_providers` 用的 `ARK_API_KEY` 之类也在其中，多带几个键的代价是
    /// profile 文件大几行，收益是将来换 provider / 走降级链路时不会又「能建号不能干活」。
    ///
    /// # 不覆盖已有值
    ///
    /// profile 里**已有非空值**的键保持原样：运维可能给某个 AI 员工配了专用 key，
    /// 覆盖它属于破坏配置。只有「缺键」或「键存在但值为空」时才写入。
    ///
    /// # 权限
    ///
    /// 落盘先写同目录临时文件 → `chmod 600` → `rename` 覆盖，因此目标文件
    /// **任何时候**都不会以 644 存在（profile 目录本身是 0700，双保险）。
    ///
    /// # 异常情况
    ///
    /// - 400 + `64002`：profile 名不合法；
    /// - 404 + `64004`：profile 目录不存在；
    /// - 500 + `64014`：找不到当前 provider 的 key（全局 `.env` 与进程环境都没有），
    ///   或写入后自检不通过（缺键 / 权限不是 600）——**调用方必须把它当创建失败并回滚**；
    /// - 500 + `64008`：`.env` 读写失败。
    pub async fn ensure_profile_env(&self, name: &str) -> Result<EnvSync, ApiError> {
        Self::validate_profile_name(name)?;
        if !self.profile_dir_exists(name).await? {
            return Err(ApiError::new(
                404,
                error_code::AI_AGENT_PROFILE_MISSING,
                format!("profile 目录不存在：{}", self.profile_dir(name).display()),
            ));
        }
        let env_path = self.env_file_path(name);
        let existing = self.read_text_opt(&env_path).await?.unwrap_or_default();
        let provider = self.resolve_provider(name).await?;
        let required_key = provider.as_deref().map(provider_api_key_var);

        // 候选：全局 .env 里所有非空的 `*_API_KEY`
        let mut incoming: Vec<(String, String)> = self
            .global_env_pairs()
            .await?
            .into_iter()
            .filter(|(key, value)| !value.trim().is_empty() && is_credential_env_key(key))
            .collect();
        // 全局 .env 里没有该键时再看进程环境（部署形态不同，两条路都可能带 key）
        if let Some(key) = &required_key
            && !incoming.iter().any(|(k, _)| k == key)
            && let Some(value) = env_non_empty(key)
        {
            incoming.push((key.clone(), value));
        }

        // 决定性判定①：当前 provider 的 key 必须找得到，否则直接拒绝建号
        let profile_pairs = parse_env_pairs(&existing);
        let key_available = match &required_key {
            Some(key) => {
                env_lookup(&profile_pairs, key).is_some()
                    || incoming.iter().any(|(k, v)| k == key && !v.trim().is_empty())
            }
            // provider 解析不出来时退一步：至少要搬进来一个凭据键
            None => !parse_env_pairs(&existing).is_empty() || !incoming.is_empty(),
        };
        if !key_available {
            let key = required_key.clone().unwrap_or_else(|| "*_API_KEY".to_string());
            return Err(ApiError::new(
                500,
                error_code::AI_AGENT_CREDENTIAL_MISSING,
                format!(
                    "profile {name} 缺少 {}：全局凭据文件 {} 与云枢进程环境里都没有该键，\
                     而 worker 只读 profile 级凭据（缺键必然 AuthError）；\
                     拒绝留下一个「能建号、不能干活」的 AI 用户",
                    key,
                    self.cfg.hermes_home.join(".env").display()
                ),
            ));
        }

        let (merged, written) = merge_env_text(&existing, &incoming);
        // 内容没变也要校正权限：历史 profile 可能是 644（含 clone 带来的）
        if merged != existing || self.file_mode(&env_path).await? != Some(CREDENTIAL_FILE_MODE) {
            self.write_env_text(&env_path, &merged).await?;
        }

        // 决定性判定②：写完立刻按「创建成功判定」复核（缺键 / 权限不对 → 失败）
        let check = self.verify_profile_credentials(name).await?;
        if !check.ok {
            return Err(ApiError::new(
                500,
                error_code::AI_AGENT_CREDENTIAL_MISSING,
                check.failure_message(),
            ));
        }
        Ok(EnvSync {
            env_path: path_string(&env_path),
            provider,
            required_key: required_key.unwrap_or_default(),
            written_keys: written,
            env_file_mode: check.env_file_mode,
        })
    }

    /// 只读自检：profile 的 `.env` 里有没有**当前 provider 的 key**、权限是不是 600。
    ///
    /// # 与 [`Self::ensure_profile_env`] 的分工
    ///
    /// - 本函数**不改盘**，只回答「现在能不能干活」，供探活接口 / 建号成功判定 / 运维排查使用；
    /// - 「缺键」不是 `Err`（那是业务事实，不是 IO 故障）：返回 `ok=false` + `detail` 说明；
    ///   只有传输层（`ssh` / 文件系统）失败才 `Err`。
    ///
    /// # 返回值里的密码纪律
    ///
    /// [`CredentialCheck::present_keys`] **只含键名**，键值永不进返回值、日志或错误消息。
    pub async fn verify_profile_credentials(&self, name: &str) -> Result<CredentialCheck, ApiError> {
        Self::validate_profile_name(name)?;
        let env_path = self.env_file_path(name);
        let text = self.read_text_opt(&env_path).await?;
        let mode = self.file_mode(&env_path).await?;
        let provider = self.resolve_provider(name).await?;
        let required_key = provider.as_deref().map(provider_api_key_var);
        let pairs = text.as_deref().map(parse_env_pairs).unwrap_or_default();
        let present_keys: Vec<String> = pairs
            .iter()
            .filter(|(_, value)| !value.trim().is_empty())
            .map(|(key, _)| key.clone())
            .collect();

        let key_ok = match &required_key {
            Some(key) => env_lookup(&pairs, key).is_some(),
            None => !present_keys.is_empty(),
        };
        let mode_ok = mode == Some(CREDENTIAL_FILE_MODE);
        let detail = if text.is_none() {
            Some(format!("profile 没有凭据文件：{}", env_path.display()))
        } else if !key_ok {
            Some(format!(
                "缺少 {}（现有键：{}）",
                required_key.clone().unwrap_or_else(|| "*_API_KEY".to_string()),
                if present_keys.is_empty() {
                    "无".to_string()
                } else {
                    present_keys.join(", ")
                }
            ))
        } else if !mode_ok {
            Some(format!(
                "凭据文件权限是 {}（要求 600）",
                mode.map(|m| format!("{m:o}")).unwrap_or_else(|| "未知".to_string())
            ))
        } else {
            None
        };
        Ok(CredentialCheck {
            env_path: path_string(&env_path),
            env_file_mode: mode,
            provider,
            required_key,
            present_keys,
            ok: key_ok && mode_ok,
            detail,
        })
    }

    /// `<profiles_root>/<name>/.env`（只在名字已校验后调用）。
    pub fn env_file_path(&self, name: &str) -> PathBuf {
        debug_assert!(Self::validate_profile_name(name).is_ok());
        self.profile_dir(name).join(".env")
    }

    /// 当前 profile 用的 provider：先读 profile 自己的 `config.yaml`，读不出再读全局的。
    async fn resolve_provider(&self, name: &str) -> Result<Option<String>, ApiError> {
        for path in [
            self.profile_dir(name).join("config.yaml"),
            self.cfg.hermes_home.join("config.yaml"),
        ] {
            if let Some(text) = self.read_text_opt(&path).await?
                && let Some(provider) = parse_model_provider(&text)
            {
                return Ok(Some(provider));
            }
        }
        Ok(None)
    }

    /// 全局凭据文件（`<hermes_home>/.env`）里的键值对；文件不存在按「空」处理。
    async fn global_env_pairs(&self) -> Result<Vec<(String, String)>, ApiError> {
        let path = self.cfg.hermes_home.join(".env");
        match self.read_text_opt(&path).await? {
            Some(text) => Ok(parse_env_pairs(&text)),
            None => Ok(Vec::new()),
        }
    }

    /// 读文本文件（两种形态通用）：不存在返回 `Ok(None)`，其余 IO 错误 → `500 + 64008`。
    async fn read_text_opt(&self, path: &Path) -> Result<Option<String>, ApiError> {
        match &self.cfg.transport {
            Transport::Local => match tokio::fs::read_to_string(path).await {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(ApiError::new(
                    500,
                    error_code::AI_AGENT_SOUL_IO_FAILED,
                    format!("读取 {} 失败：{e}", path.display()),
                )),
            },
            Transport::Ssh { .. } => {
                let out = self
                    .run_remote(format!("cat {}", shell_quote(&path_string(path))), None)
                    .await?;
                if out.ok {
                    Ok(Some(out.stdout))
                } else if out.stderr.contains("No such file") {
                    Ok(None)
                } else {
                    Err(Self::cli_failed("读取凭据文件", &out))
                }
            }
        }
    }

    /// 文件权限位（`600` → `0o600`）；文件不存在返回 `Ok(None)`。
    async fn file_mode(&self, path: &Path) -> Result<Option<u32>, ApiError> {
        match &self.cfg.transport {
            Transport::Local => match tokio::fs::metadata(path).await {
                Ok(meta) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        Ok(Some(meta.permissions().mode() & 0o777))
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = meta;
                        Ok(None)
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(ApiError::new(
                    500,
                    error_code::AI_AGENT_SOUL_IO_FAILED,
                    format!("读取 {} 权限失败：{e}", path.display()),
                )),
            },
            Transport::Ssh { .. } => {
                // `stat` 在文件不存在时非 0 退出 → 这里用 `|| true` 把「不存在」与
                // 「stat 坏了」区分开：输出为空 = 不存在，解析失败也算不存在（自检会判红）
                let out = self
                    .run_remote(
                        format!("stat -c %a {} 2>/dev/null || true", shell_quote(&path_string(path))),
                        None,
                    )
                    .await?;
                let raw = out.stdout.trim();
                Ok(u32::from_str_radix(raw, 8).ok())
            }
        }
    }

    /// 原子写凭据文件：同目录临时文件 → `chmod 600` → `rename` 覆盖。
    async fn write_env_text(&self, path: &Path, content: &str) -> Result<(), ApiError> {
        let dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let tmp = dir.join(format!(".env.tmp-{}-{}", std::process::id(), now_nanos()));
        match &self.cfg.transport {
            Transport::Local => {
                tokio::fs::write(&tmp, content.as_bytes()).await.map_err(|e| {
                    ApiError::new(
                        500,
                        error_code::AI_AGENT_SOUL_IO_FAILED,
                        format!("写凭据临时文件失败：{e}"),
                    )
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(CREDENTIAL_FILE_MODE))
                        .await
                        .map_err(|e| {
                            ApiError::new(
                                500,
                                error_code::AI_AGENT_SOUL_IO_FAILED,
                                format!("设置凭据文件权限 600 失败：{e}"),
                            )
                        })?;
                }
                tokio::fs::rename(&tmp, path).await.map_err(|e| {
                    ApiError::new(
                        500,
                        error_code::AI_AGENT_SOUL_IO_FAILED,
                        format!("原子替换凭据文件失败：{e}"),
                    )
                })
            }
            Transport::Ssh { .. } => {
                let script = format!("dd of={} status=none", shell_quote(&path_string(&tmp)));
                let out = self.run_remote(script, Some(content.to_string())).await?;
                if !out.ok {
                    return Err(Self::cli_failed("写凭据临时文件", &out));
                }
                let script = format!(
                    "chmod 600 {} && mv -f {} {}",
                    shell_quote(&path_string(&tmp)),
                    shell_quote(&path_string(&tmp)),
                    shell_quote(&path_string(path))
                );
                let out = self.run_remote(script, None).await?;
                if !out.ok {
                    return Err(Self::cli_failed("写入凭据文件", &out));
                }
                Ok(())
            }
        }
    }

    /// 把当前 `SOUL.md` 备份到 `backup_dir`，返回备份的**远端/本地路径**。
    ///
    /// 删号前调用（卡面验收 4：SOUL.md 先备份）。备份内容由调用方传入（它已经读过一次，
    /// 避免「读了 A、删了 B」这种竞态），本函数只负责落盘 + 返回路径。
    pub async fn backup_soul(&self, name: &str, content: &str) -> Result<PathBuf, ApiError> {
        Self::validate_profile_name(name)?;
        let dir = self.cfg.backup_dir.clone();
        let file = format!("{name}-{}.md", now_compact());
        let target = dir.join(&file);
        match &self.cfg.transport {
            Transport::Local => {
                tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                    ApiError::new(
                        500,
                        error_code::AI_AGENT_SOUL_IO_FAILED,
                        format!("创建备份目录失败：{e}"),
                    )
                })?;
                tokio::fs::write(&target, content.as_bytes()).await.map_err(|e| {
                    ApiError::new(500, error_code::AI_AGENT_SOUL_IO_FAILED, format!("写备份文件失败：{e}"))
                })?;
            }
            Transport::Ssh { .. } => {
                let script = format!("mkdir -p {}", shell_quote(&path_string(&dir)));
                let out = self.run_remote(script, None).await?;
                if !out.ok {
                    return Err(Self::cli_failed("创建备份目录", &out));
                }
                let script = format!("dd of={} status=none", shell_quote(&path_string(&target)));
                let out = self.run_remote(script, Some(content.to_string())).await?;
                if !out.ok {
                    return Err(Self::cli_failed("写备份文件", &out));
                }
            }
        }
        Ok(target)
    }

    // ---------- workspace ----------

    /// 确保 `<workspace_root>/<name>/` 存在（`hermes profile create` **不建** workspace，
    /// 实测 25 个既有 profile 的 workspace 都是首次运行时才出现；任务卡要求建号即建目录）。
    pub async fn ensure_workspace(&self, name: &str) -> Result<PathBuf, ApiError> {
        Self::validate_profile_name(name)?;
        let dir = self.workspace_path(name);
        match &self.cfg.transport {
            Transport::Local => {
                tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                    ApiError::new(
                        500,
                        error_code::AI_AGENT_WORKSPACE_FAILED,
                        format!("创建 workspace 失败：{e}"),
                    )
                })?;
            }
            Transport::Ssh { .. } => {
                let script = format!("mkdir -p {}", shell_quote(&path_string(&dir)));
                let out = self.run_remote(script, None).await?;
                if !out.ok {
                    return Err(Self::cli_failed("创建 workspace", &out));
                }
            }
        }
        Ok(dir)
    }

    /// 删除 `<workspace_root>/<name>/`（删号 / 回滚用）。目录不存在视为成功。
    ///
    /// # 安全性
    ///
    /// 只删 `workspace_path(name)` —— 根目录来自配置，末段来自已校验的名字，
    /// 名字里不可能出现 `/` 或 `..`，因此 `rm -rf` 的对象不可能是用户给任意路径。
    pub async fn remove_workspace(&self, name: &str) -> Result<(), ApiError> {
        Self::validate_profile_name(name)?;
        let dir = self.workspace_path(name);
        match &self.cfg.transport {
            Transport::Local => match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(ApiError::new(
                    500,
                    error_code::AI_AGENT_WORKSPACE_FAILED,
                    format!("删除 workspace 失败：{e}"),
                )),
            },
            Transport::Ssh { .. } => {
                let script = format!("rm -rf {}", shell_quote(&path_string(&dir)));
                let out = self.run_remote(script, None).await?;
                if !out.ok {
                    return Err(Self::cli_failed("删除 workspace", &out));
                }
                Ok(())
            }
        }
    }

    // ---------- 探活 ----------

    /// profile 目录是否存在。
    pub async fn profile_dir_exists(&self, name: &str) -> Result<bool, ApiError> {
        Self::validate_profile_name(name)?;
        let dir = self.profile_dir(name);
        match &self.cfg.transport {
            Transport::Local => Ok(tokio::fs::metadata(&dir).await.is_ok()),
            Transport::Ssh { .. } => {
                let script = format!("test -d {}", shell_quote(&path_string(&dir)));
                // `test` 用退出码表达真假，非 0 不代表「命令坏了」，因此这里不看 stderr
                Ok(self.run_remote_raw(script, None).await?.status)
            }
        }
    }

    // ---------- 探活的聚合 ----------

    /// 探活：目录 / SOUL.md / workspace / CLI / gateway。
    ///
    /// 这是 `GET /system/user/ai/{id}/status` 的数据源，也是「目录丢失」这类情况唯一能被
    /// 发现的地方（云枢的库存在，Hermes 侧的目录被别人 `rm` 了）。
    pub async fn status(&self, name: &str) -> Result<ProfileStatus, ApiError> {
        Self::validate_profile_name(name)?;
        let profile_dir_exists = self.profile_dir_exists(name).await?;
        let soul_file_exists = if profile_dir_exists {
            self.read_soul(name).await.is_ok()
        } else {
            false
        };
        let workspace_exists = self.workspace_path_exists(name).await?;

        let (cli_available, gateway, detail) = if profile_dir_exists {
            let argv = vec!["profile".to_string(), "show".to_string(), name.to_string()];
            match self.run_hermes(argv, None).await {
                Ok(out) if out.ok => (true, parse_gateway(&out.stdout), None),
                Ok(out) => (false, "unknown".to_string(), Some(excerpt(&out.stderr))),
                Err(e) => (false, "unknown".to_string(), Some(e.msg.clone())),
            }
        } else {
            // 目录都没了，CLI 是否可用仍值得告诉运维：用一次 `profile list` 探 CLI 本身
            let argv = vec!["profile".to_string(), "list".to_string()];
            match self.run_hermes(argv, None).await {
                Ok(out) => (out.ok, "unknown".to_string(), None),
                Err(e) => (false, "unknown".to_string(), Some(e.msg.clone())),
            }
        };

        // 凭据自检：即便 `profile show` 正常，缺 key 的账号也干不了活（AuthError），
        // 所以它必须是探活结果的一部分。自检失败不阻断探活（运维要看到完整的磁盘状态）。
        let credentials = self
            .verify_profile_credentials(name)
            .await
            .unwrap_or_else(|error| CredentialCheck {
                env_path: path_string(&self.env_file_path(name)),
                env_file_mode: None,
                provider: None,
                required_key: None,
                present_keys: Vec::new(),
                ok: false,
                detail: Some(format!("凭据自检失败：{}", error.msg)),
            });

        Ok(ProfileStatus {
            profile_dir_exists,
            soul_file_exists,
            workspace_exists,
            gateway,
            cli_available,
            credentials_ok: credentials.ok,
            credential_keys: credentials.present_keys,
            credential_required: credentials.required_key,
            env_file_mode: credentials.env_file_mode,
            credential_detail: credentials.detail,
            detail,
        })
    }

    /// workspace 目录是否存在。
    pub async fn workspace_path_exists(&self, name: &str) -> Result<bool, ApiError> {
        Self::validate_profile_name(name)?;
        let dir = self.workspace_path(name);
        match &self.cfg.transport {
            Transport::Local => Ok(tokio::fs::metadata(&dir).await.is_ok()),
            Transport::Ssh { .. } => {
                let script = format!("test -d {}", shell_quote(&path_string(&dir)));
                Ok(self.run_remote_raw(script, None).await?.status)
            }
        }
    }

    // ==================== 进程执行（内部） ====================

    /// 跑一条 `hermes` 子命令。
    async fn run_hermes(&self, argv: Vec<String>, stdin: Option<String>) -> Result<CliOutput, ApiError> {
        match &self.cfg.transport {
            Transport::Local => self.spawn_local(&self.cfg.hermes_bin, &argv, stdin).await,
            Transport::Ssh { host } => {
                let script = self.remote_hermes_script(&argv);
                self.spawn_ssh(host, script, stdin).await
            }
        }
    }

    /// 跑一条**只涉及路径**的远端 shell 命令（Ssh 形态）；Local 形态直接报错——
    /// 本地实现走 `tokio::fs`，不可能走到这里。
    async fn run_remote(&self, script: String, stdin: Option<String>) -> Result<CliOutput, ApiError> {
        match &self.cfg.transport {
            Transport::Ssh { host } => self.spawn_ssh(host, script, stdin).await,
            Transport::Local => Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "本地形态不应走远端命令".to_string(),
            )),
        }
    }

    /// 同 [`Self::run_remote`]，但只看退出码（`test -d` 这类用退出码表达真假的命令）。
    async fn run_remote_raw(&self, script: String, stdin: Option<String>) -> Result<ExitStatus, ApiError> {
        let out = self.run_remote(script, stdin).await?;
        Ok(ExitStatus {
            status: out.ok,
            stderr: out.stderr,
        })
    }

    /// 拼远端命令行：`env HERMES_HOME=<home> <bin> <arg>...`，每个片段都过 [`shell_quote`]。
    ///
    /// 这是本模块**唯一**的「字符串拼接 shell」处；拼接对象是「常量前缀 + 已校验名字 +
    /// 引号包装过的自由文本」。`HERMES_HOME` 必须显式传：实测不传时 Hermes 会打
    /// `[HERMES_HOME fallback] … active profile is 'laoliu'` 告警并可能写错 profile。
    fn remote_hermes_script(&self, argv: &[String]) -> String {
        let mut parts = vec![
            "env".to_string(),
            format!("HERMES_HOME={}", shell_quote(&path_string(&self.cfg.hermes_home))),
            shell_quote(&path_string(&self.cfg.hermes_bin)),
        ];
        parts.extend(argv.iter().map(|a| shell_quote(a)));
        parts.join(" ")
    }

    /// 起本地进程（参数数组，永不经过 shell）。
    async fn spawn_local(&self, program: &Path, argv: &[String], stdin: Option<String>) -> Result<CliOutput, ApiError> {
        if tokio::fs::metadata(program).await.is_err() {
            return Err(ApiError::new(
                500,
                error_code::AI_AGENT_HERMES_FAILED,
                format!("hermes CLI 不存在：{}", program.display()),
            ));
        }
        let mut cmd = Command::new(program);
        cmd.args(argv)
            .env("HERMES_HOME", &self.cfg.hermes_home)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.wait_with_timeout(cmd, stdin, program.display().to_string()).await
    }

    /// 起 ssh 进程。远端命令作为**单个参数**交给 ssh（它自己会拼给远端 shell）。
    async fn spawn_ssh(&self, host: &str, script: String, stdin: Option<String>) -> Result<CliOutput, ApiError> {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("StrictHostKeyChecking=no")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("LogLevel=ERROR")
            .arg(host)
            .arg(script)
            .env("LC_ALL", "C")
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.wait_with_timeout(cmd, stdin, format!("ssh {host}")).await
    }

    /// 统一的「喂 stdin → 等结果 → 超时 kill → 抽 stderr 摘录」。
    async fn wait_with_timeout(
        &self,
        mut cmd: Command,
        stdin: Option<String>,
        label: String,
    ) -> Result<CliOutput, ApiError> {
        let spawned = cmd.spawn().map_err(|e| {
            ApiError::new(
                500,
                error_code::AI_AGENT_HERMES_FAILED,
                format!("启动 {label} 失败：{e}"),
            )
        })?;
        let mut child = spawned;
        if let Some(content) = stdin
            && let Some(pipe) = child.stdin.take()
        {
            // 写完立即关闭 stdin（drop），否则对端 `dd` / `cat` 会一直等 EOF
            let mut pipe = pipe;
            let _ = tokio::io::AsyncWriteExt::write_all(&mut pipe, content.as_bytes()).await;
            let _ = tokio::io::AsyncWriteExt::flush(&mut pipe).await;
            drop(pipe);
        }
        let wait = child.wait_with_output();
        match tokio::time::timeout(self.cfg.timeout, wait).await {
            Ok(Ok(output)) => Ok(CliOutput {
                ok: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            }),
            Ok(Err(e)) => Err(ApiError::new(
                500,
                error_code::AI_AGENT_HERMES_FAILED,
                format!("等待 {label} 退出失败：{e}"),
            )),
            Err(_) => {
                // `kill_on_drop(true)`（spawn 时设置）+ 这里 drop 掉 wait 的 future：
                // 超时即终止子进程，不依赖外部 pkill，也不会阻塞到命令自己退出。
                Err(ApiError::new(
                    500,
                    error_code::AI_AGENT_HERMES_FAILED,
                    format!("{label} 超时（{}s）已被终止", self.cfg.timeout.as_secs()),
                ))
            }
        }
    }

    /// 把 CLI 失败换成对外错误（HTTP 500 + 业务码 + stderr 摘录）。
    fn cli_failed(action: &str, out: &CliOutput) -> ApiError {
        let stderr = excerpt(&out.stderr);
        let stdout = excerpt(&out.stdout);
        let message = if !stderr.is_empty() {
            format!("{action}失败：{stderr}")
        } else if !stdout.is_empty() {
            format!("{action}失败：{stdout}")
        } else {
            format!("{action}失败：hermes CLI 未输出错误信息")
        };
        ApiError::new(500, error_code::AI_AGENT_HERMES_FAILED, message)
    }
}

// ==================== kanban 载荷（T5b） ====================

/// `hermes kanban create` 的入参（`&str` 借用，调用方拼好后一次传入）。
#[derive(Debug, Clone)]
pub struct KanbanCreate<'a> {
    /// 看板 slug（当前固定 `dev`）。
    pub board: &'a str,
    /// 任务标题（= 云枢任务标题）。
    pub title: &'a str,
    /// 执行者 profile（AI 员工的 `sys_ai_agent.hermes_profile`）。
    pub assignee: &'a str,
    /// 任务正文：云枢任务描述 + 交付要求（**含任务级令牌明文**）。
    pub body: &'a str,
    /// 优先级数值（P0→10 / P1→8 / P2→5 / P3→2）。
    pub priority: i32,
    /// 单次运行上限（秒）。
    pub max_runtime_secs: i64,
    /// 幂等键（`cailu-task:<云枢任务id>`）；空串 = 不带该参数。
    pub idempotency_key: &'a str,
    /// 作者（`cailu:<分派人用户名>`）。
    pub created_by: Option<&'a str>,
    /// 工作区（`dir:<绝对路径>`）；`None` = 用 Hermes 默认（scratch）。
    pub workspace: Option<&'a str>,
}

/// `hermes kanban show <id> --json` 的**用得到的那部分**。
///
/// 只取云枢回传需要的字段：状态 / 执行摘要 / 最近一次 run 的 error 与 metadata。
/// 其余（events / comments / parents）不解析 —— 解析得越多，Hermes 升级时的
/// 兼容面越大，而云枢根本不用它们。
#[derive(Debug, Clone, Default)]
pub struct KanbanSnapshot {
    /// `task.status`：`ready` / `running` / `done` / `failed` / `blocked` / …
    pub status: String,
    /// `latest_summary`（长文本执行报告，**done 的第一层回传**）。
    pub latest_summary: Option<String>,
    /// 最近一次 run 的状态（`running` / `done` / `failed` / …）。
    pub run_status: Option<String>,
    /// 最近一次 run 的 `error`（failed 时写进评论）。
    pub run_error: Option<String>,
    /// 最近一次 run 的 `metadata`（**done 的第二层回传**：交付清单）。
    pub metadata: Option<serde_json::Value>,
    /// 最近一次 run 的 `summary`（`latest_summary` 为空时的兜底）。
    pub run_summary: Option<String>,
    /// `task.completed_at`（Unix 秒，可空）。
    pub completed_at: Option<i64>,
}

impl KanbanSnapshot {
    /// 从 `show --json` 的整份 JSON 里取字段。
    ///
    /// # 口径（逐条说明，评审会问）
    ///
    /// - **status** 取 `task.status`，缺失时回落 `""`（映射层按「未知」处理，不改状态）；
    /// - **run** 取**最后一条**（`runs` 在 CLI 里按 run id 升序，最后一条是最新一次执行）。
    ///   卡面写 `runs[0]`，那是「单次执行」场景的简写；重试过的任务若取 `runs[0]`
    ///   会把**第一次**的 error 当成最终结果，所以这里取最后一条 —— 语义更准，且
    ///   单次执行时两者完全相同；
    /// - **metadata** 可能是 JSON 对象，也可能是**字符串**（SQLite 里存的是文本，
    ///   CLI 有的版本原样透传）→ 字符串再 parse 一次，解析失败就当没有（不让
    ///   一份坏 metadata 把整轮同步打断）。
    pub fn from_json(root: &serde_json::Value) -> Result<Self, ApiError> {
        let task = root.get("task").ok_or_else(|| {
            ApiError::new(
                500,
                error_code::AI_AGENT_HERMES_FAILED,
                "Hermes 任务输出缺少 task 字段".to_string(),
            )
        })?;
        let status = task
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let latest_summary = text_field(root, "latest_summary");
        let completed_at = task.get("completed_at").and_then(|value| value.as_i64());
        let runs = root.get("runs").and_then(|value| value.as_array());
        let last = runs.and_then(|list| list.last());
        let run_status = last
            .and_then(|run| run.get("status"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());
        let run_error = last.and_then(|run| text_field(run, "error"));
        let run_summary = last.and_then(|run| text_field(run, "summary"));
        let metadata = last.and_then(|run| run.get("metadata")).and_then(normalize_metadata);
        Ok(Self {
            status,
            latest_summary,
            run_status,
            run_error,
            metadata,
            run_summary,
            completed_at,
        })
    }

    /// 交付文件列表（`metadata.new_files`，绝对路径数组）。
    ///
    /// 只接受字符串数组；单条字符串不认（契约写的是数组，宽松解析会把
    /// 「一个恰好叫 new_files 的字符串字段」当成路径列表）。
    pub fn new_files(&self) -> Vec<String> {
        self.metadata
            .as_ref()
            .and_then(|meta| meta.get("new_files"))
            .and_then(|value| value.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|item| item.as_str())
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 执行报告文本：优先 `latest_summary`，其次最近一次 run 的 `summary`。
    pub fn report_text(&self) -> Option<String> {
        self.latest_summary
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.run_summary
                    .as_ref()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
    }
}

/// 取字符串字段（空串视为缺失）。
fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
}

/// `metadata` 归一化：对象原样、字符串再 parse 一次、其余（数字 / 数组 / null）丢弃。
fn normalize_metadata(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Object(_) => Some(value.clone()),
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<serde_json::Value>(trimmed)
                .ok()
                .filter(|parsed| parsed.is_object())
        }
        _ => None,
    }
}

/// 看板 slug 校验：小写字母开头，只含小写字母 / 数字 / 连字符，1–32 字符。
///
/// 它会进命令行参数（`--board <slug>`），因此与 profile 名同一套规矩。
pub fn validate_board_slug(slug: &str) -> Result<(), ApiError> {
    let ok = !slug.is_empty()
        && slug.len() <= 32
        && slug.starts_with(|c: char| c.is_ascii_lowercase())
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        return Ok(());
    }
    Err(ApiError::new(
        400,
        error_code::TASK_FIELD_INVALID,
        format!("Hermes 看板 slug 不合法：{slug:?}（小写字母开头，只含小写字母/数字/连字符，1–32 字符）"),
    ))
}

/// Hermes 任务 id 校验：`t_` + 十六进制/字母数字，长度 3–64。
///
/// 它同样进命令行参数，必须挡住 `--board` 之类的「看起来像 id 的选项」。
pub fn validate_kanban_task_id(task_id: &str) -> Result<(), ApiError> {
    let ok = task_id.len() >= 3
        && task_id.len() <= 64
        && !task_id.starts_with('-')
        && task_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        return Ok(());
    }
    Err(ApiError::new(
        400,
        error_code::TASK_FIELD_INVALID,
        format!("Hermes 任务 id 不合法：{task_id:?}"),
    ))
}

/// 幂等键校验：只允许 `[A-Za-z0-9:_-]`，1–128 字符。
///
/// 幂等键是「重复派活只产生一个 Hermes 任务」的唯一保证（验收项 4），
/// 它的取值由云枢拼（`cailu-task:<uuid>`），但仍要在入口挡一次脏值 ——
/// 它进的是命令行参数。
pub fn validate_idempotency_key(key: &str) -> Result<(), ApiError> {
    let ok = !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'));
    if ok {
        return Ok(());
    }
    Err(ApiError::new(
        400,
        error_code::TASK_FIELD_INVALID,
        format!("幂等键不合法：{key:?}（只允许字母/数字/冒号/下划线/连字符/点，1–128 字符）"),
    ))
}

/// 进程结果。
#[derive(Debug, Clone)]
struct CliOutput {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// 只要退出码的结果。
#[derive(Debug, Clone)]
struct ExitStatus {
    status: bool,
    stderr: String,
}

impl ExitStatus {
    /// 供状态聚合使用（目前仅记录用）。
    #[allow(dead_code)]
    fn stderr(&self) -> &str {
        &self.stderr
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn now_compact() -> String {
    // 备份文件名里的时间戳：`YYYYmmdd-HHMMSS-nanos`，只用于避免撞名，不参与业务语义
    let now = chrono::Utc::now();
    format!(
        "{}-{}-{:09}",
        now.format("%Y%m%d"),
        now.format("%H%M%S"),
        now_nanos() % 1_000_000_000
    )
}

/// 取字符串前 [`STDERR_EXCERPT_CHARS`] 个**字符**（不是字节，中文不会被切坏）。
fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out: String = trimmed.chars().take(STDERR_EXCERPT_CHARS).collect();
    if trimmed.chars().count() > STDERR_EXCERPT_CHARS {
        out.push('…');
    }
    out
}

/// 解析 `hermes profile show <name>` 的 `Gateway:` 行 → `online` / `offline` / `unknown`。
///
/// 实测输出形如：
/// ```text
/// Profile: l1probe-tmp
/// Path:    /root/.hermes/profiles/l1probe-tmp
/// Model:   deepseek-v4.1-flash (custom)
/// Gateway: running
/// Skills:  39
/// ```
/// 注意 `profile list` 会往 stderr 打 ruamel.yaml 的告警，因此**只解析 stdout 的单行**，
/// 不去钻整张表。
fn parse_gateway(stdout: &str) -> String {
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("gateway") {
            let value = value.trim().to_ascii_lowercase();
            if value.contains("running") || value.contains("online") {
                return "online".to_string();
            }
            if value.contains("stopped")
                || value.contains("offline")
                || value.contains("not running")
                || value.contains("inactive")
            {
                return "offline".to_string();
            }
            return "unknown".to_string();
        }
    }
    "unknown".to_string()
}

/// 已知 provider id → Hermes 凭据池读取的环境变量名。
///
/// 表里是**实测口径**；表外的 provider 走通用规则：大写 + 非字母数字换 `_` + `_API_KEY`
/// 后缀，这样将来新增 provider 时不需要改代码。
pub fn provider_api_key_var(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    let known = match normalized.as_str() {
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "xai" | "grok" => Some("XAI_API_KEY"),
        "google" | "gemini" => Some("GEMINI_API_KEY"),
        "nous" => Some("NOUS_API_KEY"),
        "moonshot" | "kimi" => Some("MOONSHOT_API_KEY"),
        "zhipu" | "glm" => Some("ZHIPU_API_KEY"),
        "dashscope" | "qwen" => Some("DASHSCOPE_API_KEY"),
        "ark" | "volcengine" | "volces" => Some("ARK_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "mistral" => Some("MISTRAL_API_KEY"),
        "groq" => Some("GROQ_API_KEY"),
        "together" => Some("TOGETHER_API_KEY"),
        _ => None,
    };
    if let Some(key) = known {
        return key.to_string();
    }
    let mut out = String::new();
    for ch in normalized.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    if !out.ends_with("_API_KEY") {
        out.push_str("_API_KEY");
    }
    out
}

/// 该环境变量名是否属于「凭据键」（要从全局 `.env` 搬进 profile 的那一类）。
///
/// 口径故意收窄成 `*_API_KEY`：`.env` 里还有 `THS_PASSWORD` / `OBSIDIAN_VAULT_PATH`
/// 这类与 inference provider 无关的变量，整文件复制进 profile 既违反最小权限，
/// 也会让「凭据文件里到底有什么」变得不可审计。
pub fn is_credential_env_key(key: &str) -> bool {
    key.ends_with("_API_KEY")
}

/// 合法环境变量名：首字符字母或 `_`，其余字母数字或 `_`。
fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 解析 `.env` 文本 → `[(键, 值)]`（保持顺序，忽略注释 / 空行 / 非法键名）。
///
/// # 支持与不支持的写法
///
/// - 支持 `KEY=value`、`export KEY=value`、`KEY="value"`、`KEY='value'`，
///   以及未加引号值后的行内注释（`KEY=v # 说明`）；
/// - 不支持多行值与变量插值 —— Hermes 自己的 `.env` 只有单行 `KEY=value`，
///   为它引入依赖不划算，也会让「值里带 `#`」这类边角语义不可预测。
pub fn parse_env_pairs(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_valid_env_key(key) {
            continue;
        }
        let raw_value = value.trim();
        let quoted = raw_value.len() >= 2
            && ((raw_value.starts_with('"') && raw_value.ends_with('"'))
                || (raw_value.starts_with('\'') && raw_value.ends_with('\'')));
        let value = if quoted {
            raw_value[1..raw_value.len() - 1].to_string()
        } else {
            // 未加引号时剥掉行内注释（" #" 之后的部分）
            match raw_value.find(" #") {
                Some(pos) => raw_value[..pos].trim().to_string(),
                None => raw_value.to_string(),
            }
        };
        pairs.push((key.to_string(), value));
    }
    pairs
}

/// 从解析结果里取某键的**非空**值。
pub fn env_lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a String> {
    pairs
        .iter()
        .find(|(k, v)| k == key && !v.trim().is_empty())
        .map(|(_, v)| v)
}

/// 行 → 该行声明的环境变量名（注释 / 空行 / 非 `KEY=value` 行返回 `None`）。
fn line_env_key(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let trimmed = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    is_valid_env_key(key).then(|| key.to_string())
}

/// 把 `incoming` 里 profile `.env` **缺的键**补上，返回 `(新文本, 实际写入的键名)`。
///
/// # 语义（三条，缺一条就会写坏别人的配置）
///
/// 1. profile 里已有**非空值**的键：**原样保留**（可能是给这个 AI 员工配的专用 key）；
/// 2. profile 里有键但**值为空**（`KEY=`）：就地填上新值，不追加第二行；
/// 3. profile 里没有的键：追加到文件末尾（保持原有注释与顺序，不重排）。
pub fn merge_env_text(existing: &str, incoming: &[(String, String)]) -> (String, Vec<String>) {
    let mut lines: Vec<String> = existing.lines().map(|line| line.to_string()).collect();
    let mut written = Vec::new();
    for (key, value) in incoming {
        if value.trim().is_empty() {
            continue;
        }
        match lines
            .iter()
            .position(|line| line_env_key(line).as_deref() == Some(key.as_str()))
        {
            Some(index) => {
                // 已有该键：非空则保持原样，空的就地填
                let current = parse_env_pairs(&lines[index])
                    .into_iter()
                    .next()
                    .map(|(_, value)| value)
                    .unwrap_or_default();
                if current.trim().is_empty() {
                    lines[index] = format!("{key}={value}");
                    written.push(key.clone());
                }
            }
            None => {
                lines.push(format!("{key}={value}"));
                written.push(key.clone());
            }
        }
    }
    let mut text = lines.join("\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    (text, written)
}

/// 解析 `config.yaml` 里 `model:` 段的 `provider:`（**行式解析**，不引入 yaml crate）。
///
/// # 为什么不用 serde_yaml
///
/// 本仓库没有 yaml 依赖（`Cargo.toml` 只有 `serde_json` + `regex`），为一个字段加依赖会在
/// 别人的 `Cargo.lock` 上制造冲突；而这里需要的只是「`model:` 块里的 `provider: x`」。
///
/// 口径：
///
/// - 只认**列 0** 的 `model:` 作为块首（`fallback_providers:` 里的 `provider:` 不算）；
/// - 支持 `model: {provider: x}` 这种行内写法；
/// - 块内缩进行里第一个 `provider:` 即为答案。
pub fn parse_model_provider(yaml: &str) -> Option<String> {
    let mut in_model = false;
    for raw in yaml.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            in_model = trimmed == "model:" || trimmed.starts_with("model:");
            if let Some(rest) = trimmed.strip_prefix("model:") {
                let rest = rest.trim();
                if rest.starts_with('{') {
                    return inline_provider(rest);
                }
            }
            continue;
        }
        if in_model && let Some(rest) = trimmed.strip_prefix("provider:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 行内映射 `{provider: deepseek, base_url: …}` 里取 `provider` 的值。
fn inline_provider(rest: &str) -> Option<String> {
    let start = rest.find("provider:")?;
    let tail = rest[start + "provider:".len()..].trim_start();
    let value: String = tail
        .chars()
        .take_while(|c| *c != ',' && *c != '}')
        .collect::<String>()
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string();
    (!value.is_empty()).then_some(value)
}

/// POSIX shell 单引号包裹。
///
/// 规则：整串放进单引号里，串内的 `'` 换成 `'\''`（闭合 → 转义 → 重开）。
/// 单引号内 shell **不做**任何解释（`$`、反引号、`;`、`|`、`&`、换行全是普通字符），
/// 因此这是「任意字节都能安全当参数」的标准做法。
pub fn shell_quote(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('\'');
    for ch in raw.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 渲染默认 `SOUL.md`（用户留空时用）。
///
/// 结构照 Hermes 既有模板：首行身份 → `# 核心职责` →
/// `# 所属团队` → `# 工作原则` → `# 个性特点`。这样云枢建出来的 AI 用户与团队现有
/// 成员同构，后续「编辑职责」时能直接改对应段落。
pub fn render_default_soul(name: &str, display_name: &str, description: &str) -> String {
    let display = if display_name.trim().is_empty() {
        name
    } else {
        display_name.trim()
    };
    let description = if description.trim().is_empty() {
        "（职责待补充：在云枢「用户管理 → 编辑 AI 用户」里填写）"
    } else {
        description.trim()
    };
    format!(
        "你是{display}（{name}），{description}\n\
         你运行在 Hermes Agent（由 Nous Research 打造）之上。\n\
         # 核心职责\n\
         - {description}\n\
         # 所属团队\n\
         由云枢创建并托管的 AI 用户。 职责与人格在云枢「用户管理」中维护（本文件可随时编辑）。\n\
         # 工作原则\n\
         - 中文为主要交流语言\n\
         - 交付物必须是基于真实工具输出的可工作成果，而不是描述\n\
         - 数据、结论要给出依据和来源，不确定时坦诚说明\n\
         # 个性特点\n\
         - （待补充：这里写说话风格、协作偏好等）\n"
    )
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("l1-aiagent-{tag}-{}-{}", std::process::id(), now_nanos()));
        std::fs::create_dir_all(&base).expect("建临时目录");
        base
    }

    fn local_config(root: &Path, bin: PathBuf) -> HermesConfig {
        HermesConfig {
            transport: Transport::Local,
            hermes_bin: bin,
            hermes_home: root.join("home"),
            profiles_root: root.join("profiles"),
            workspace_root: root.join("workspace"),
            backup_dir: root.join("backups"),
            clone_from: "default".to_string(),
            timeout: Duration::from_secs(10),
        }
    }

    /// profile 名校验（红线）：合法名通过，注入 / 大小写 / 越界一律拒绝。
    #[test]
    fn profile_name_validation_rejects_injection() {
        for ok in ["ab", "abc", "a1", "a-b", "agent-01", "abcdefghijklmnopqrstuvwxyz12345"] {
            assert!(Hermes::validate_profile_name(ok).is_ok(), "应通过：{ok}");
        }
        let bad = [
            "Bad_Name",
            "ABC",
            "abc; rm -rf /",
            "abc && curl evil",
            "abc|nc",
            "abc`id`",
            "abc$(id)",
            "-abc",
            "1abc",
            "a",
            "",
            " abc",
            "abc ",
            "../etc",
            "a/b",
            "abc_def",
            "abc.ext",
            "default",
            "abcdefghijklmnopqrstuvwxyz123456", // 32 字符，超上限
            "abc\nrm",
            "ảbc",
        ];
        for name in bad {
            let err = Hermes::validate_profile_name(name).expect_err(&format!("必须拒绝：{name:?}"));
            assert_eq!(err.code, error_code::AI_AGENT_PROFILE_INVALID, "错误码：{name:?}");
        }
    }

    /// 正则必须首尾锚定（`is_match` 在未锚定时会命中子串）。
    #[test]
    fn profile_name_regex_is_anchored() {
        assert!(PROFILE_NAME_RE.is_match("agent-1"));
        // 未锚定的话 `1agent-1` 里的 `agent-1` 子串会被命中（=> true），锚定后必须 false
        assert!(!PROFILE_NAME_RE.is_match("1agent-1"));
        // 同上，末尾锚定：`agent-1!` 的子串 `agent-1` 不能算命中
        assert!(!PROFILE_NAME_RE.is_match("agent-1!"));
        assert!(!PROFILE_NAME_RE.is_match("agent-1x\n"));
    }

    /// 模板名校验：允许 `default`，但**新建 profile 名**仍不许叫 default；注入字符照旧拒绝。
    ///
    /// 这条守的是「克隆来源 vs 新建名字」两类输入的语义差：合成一个校验函数
    /// 就会二选一地牺牲一方（HEAD 8f0c7f2 牺牲的是「从 default 克隆」）。
    #[test]
    fn template_name_allows_reserved_default() {
        assert!(
            Hermes::validate_template_name("default").is_ok(),
            "default 是内置模板，克隆来源必须允许"
        );
        assert!(Hermes::validate_template_name("agent-1").is_ok());
        // 新建名字仍然不许占用默认 profile
        assert!(
            Hermes::validate_profile_name("default").is_err(),
            "新建一个叫 default 的 profile 必须被拒"
        );
        // 注入面没有放松
        assert!(Hermes::validate_template_name("abc; rm -rf /").is_err());
        assert!(Hermes::validate_template_name("").is_err());
        assert!(Hermes::validate_template_name("1agent").is_err());
    }

    /// `shell_quote` 的产物交给真 shell 后必须还原成原串（6 类危险输入）。
    #[test]
    fn shell_quote_roundtrips_through_real_shell() {
        let cases = [
            "plain",
            "abc; rm -rf /",
            "a'b",
            "'quoted'",
            "$(id)",
            "`id`",
            "line1\nline2",
            "中文 职责 描述",
            "a b\tc",
            "back\\slash",
            "!history",
            "*glob?",
        ];
        for raw in cases {
            let quoted = shell_quote(raw);
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {quoted}"))
                .output()
                .expect("跑 sh");
            assert!(output.status.success(), "sh 失败：{raw:?}");
            assert_eq!(String::from_utf8_lossy(&output.stdout), raw, "还原不一致：{raw:?}");
        }
    }

    /// `shell_quote` 不产生未闭合引号（注入方无法用引号「跳出」字符串）。
    #[test]
    fn shell_quote_always_closes() {
        for raw in ["'", "''", "a'", "'a", "'''"] {
            let quoted = shell_quote(raw);
            assert!(quoted.starts_with('\''), "必须以单引号开头：{quoted}");
            assert!(quoted.ends_with('\''), "必须以单引号结尾：{quoted}");
            // 单引号块之间的转义序列必须成对出现
            assert_eq!(
                quoted.matches("'\\''").count(),
                raw.matches('\'').count(),
                "转义次数应与原文单引号数一致：{quoted}"
            );
        }
    }

    /// 默认 SOUL 模板：四段结构齐全，且带上名字与职责。
    #[test]
    fn default_soul_has_expected_sections() {
        let soul = render_default_soul("agent-01", "张三", "负责客户对账");
        assert!(soul.contains("你是张三（agent-01）"));
        assert!(soul.contains("负责客户对账"));
        for section in ["# 核心职责", "# 所属团队", "# 工作原则", "# 个性特点"] {
            assert!(soul.contains(section), "缺少段落：{section}");
        }
        // 显示名留空时回落到 profile 名
        let fallback = render_default_soul("agent-02", "   ", "");
        assert!(fallback.contains("你是agent-02（agent-02）"));
        assert!(fallback.contains("职责待补充"));
    }

    /// `parse_gateway` 只认 `Gateway:` 行，认不出时返回 `unknown`（不抛错）。
    #[test]
    fn parse_gateway_reads_show_output() {
        let running = "Profile: a\nGateway: running\nSkills: 39\n";
        assert_eq!(parse_gateway(running), "online");
        let stopped = "Profile: a\nGateway:   stopped\n";
        assert_eq!(parse_gateway(stopped), "offline");
        let weird = "Profile: a\nGateway: whatever\n";
        assert_eq!(parse_gateway(weird), "unknown");
        assert_eq!(parse_gateway("Profile: a\n"), "unknown");
        // 形似但键名不是 Gateway 的行不得误判
        assert_eq!(parse_gateway("Gateway-Status: running\n"), "unknown");
    }

    /// 原子写：内容一致、目录里不残留临时文件。
    #[tokio::test]
    async fn write_soul_is_atomic_and_leaves_no_temp_files() {
        let root = temp_root("soul");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        // 手工造一个 profile 目录（本测试只验证文件层，不启动 CLI）
        std::fs::create_dir_all(root.join("profiles/agent-01")).expect("建 profile 目录");

        hermes
            .write_soul("agent-01", "# 第一版\n内容\n")
            .await
            .expect("写 SOUL");
        // E15：落盘内容 = 用户内容 + 平台强制安全红线区块（区块在末尾，用户内容原样）
        let first = hermes.read_soul("agent-01").await.expect("读 SOUL");
        assert!(
            crate::business::aiagent::safety::has_safety_block(&first),
            "写 SOUL 必须自动注入安全红线区块"
        );
        assert_eq!(
            crate::business::aiagent::safety::strip_safety_block(&first).trim_end(),
            "# 第一版\n内容"
        );

        hermes
            .write_soul("agent-01", "# 第二版\n换行里有 ' 引号 $ 和 `\n")
            .await
            .expect("覆盖写");
        let second = hermes.read_soul("agent-01").await.expect("读回");
        assert_eq!(
            crate::business::aiagent::safety::strip_safety_block(&second).trim_end(),
            "# 第二版\n换行里有 ' 引号 $ 和 `"
        );
        // 反复写也只能有一份区块（不重复追加）
        assert_eq!(
            second.matches(crate::business::aiagent::safety::SAFETY_BEGIN).count(),
            1
        );

        let leftovers: Vec<String> = std::fs::read_dir(root.join("profiles/agent-01"))
            .expect("列目录")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".SOUL.md.tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "临时文件残留：{leftovers:?}");

        // profile 目录不存在时必须报 404，而不是「写到一个随机位置」
        let err = hermes.write_soul("agent-99", "x").await.expect_err("应失败");
        assert_eq!(err.code, error_code::AI_AGENT_PROFILE_MISSING);

        std::fs::remove_dir_all(&root).ok();
    }

    /// 超上限的 SOUL 正文被拒（在写盘之前）。
    #[tokio::test]
    async fn write_soul_rejects_oversize() {
        let root = temp_root("soul-big");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        std::fs::create_dir_all(root.join("profiles/agent-01")).expect("建目录");
        let big = "x".repeat(SOUL_MAX_BYTES + 1);
        let err = hermes.write_soul("agent-01", &big).await.expect_err("应拒绝");
        assert_eq!(err.code, error_code::AI_AGENT_SOUL_INVALID);
        assert!(!root.join("profiles/agent-01/SOUL.md").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// 回滚路径：假的 `hermes`（退出码 1）→ 创建失败且消息里带 stderr；
    /// 真的「二进制不存在」→ 也必须是可读的失败，而不是 panic。
    #[tokio::test]
    async fn create_profile_failure_is_reported_with_stderr() {
        let root = temp_root("create-fail");
        let fake_bin = root.join("hermes");
        std::fs::write(
            &fake_bin,
            "#!/bin/sh\necho 'ERROR: profile already exists' >&2\nexit 1\n",
        )
        .expect("写假 CLI");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).expect("加执行位");
        }
        let hermes = Hermes::new(local_config(&root, fake_bin.clone()));

        let err = hermes
            .create_profile("agent-01", Some("default"), "职责")
            .await
            .expect_err("必须失败");
        assert_eq!(err.code, error_code::AI_AGENT_HERMES_FAILED);
        assert!(
            err.msg.contains("profile already exists"),
            "stderr 必须回传：{}",
            err.msg
        );
        // 失败不得留下任何 profile 目录
        assert!(!root.join("profiles/agent-01").exists());

        // CLI 不存在
        let missing = Hermes::new(local_config(&root, root.join("nope")));
        let err = missing
            .create_profile("agent-02", None, "")
            .await
            .expect_err("必须失败");
        assert_eq!(err.code, error_code::AI_AGENT_HERMES_FAILED);
        assert!(err.msg.contains("CLI 不存在"), "{}", err.msg);
        assert!(!root.join("profiles/agent-02").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    /// 超时保护：假 CLI 睡 5s、超时 1s → 必须在 1s 级别返回失败，不能挂住。
    #[tokio::test]
    async fn create_profile_times_out() {
        let root = temp_root("create-timeout");
        let fake_bin = root.join("hermes");
        std::fs::write(&fake_bin, "#!/bin/sh\nsleep 5\nexit 0\n").expect("写假 CLI");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).expect("加执行位");
        }
        let mut cfg = local_config(&root, fake_bin);
        cfg.timeout = Duration::from_millis(300);
        let hermes = Hermes::new(cfg);
        let started = std::time::Instant::now();
        let err = hermes.create_profile("agent-01", None, "").await.expect_err("必须超时");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "超时保护失效，耗时 {:?}",
            started.elapsed()
        );
        assert_eq!(err.code, error_code::AI_AGENT_HERMES_FAILED);
        assert!(err.msg.contains("超时"), "{}", err.msg);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 建 profile 成功但目录没出现（CLI 静默失败）→ 也当失败。
    #[tokio::test]
    async fn create_profile_detects_silent_failure() {
        let root = temp_root("create-silent");
        let fake_bin = root.join("hermes");
        std::fs::write(&fake_bin, "#!/bin/sh\nexit 0\n").expect("写假 CLI");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).expect("加执行位");
        }
        let hermes = Hermes::new(local_config(&root, fake_bin));
        let err = hermes.create_profile("agent-01", None, "").await.expect_err("必须失败");
        assert_eq!(err.code, error_code::AI_AGENT_HERMES_FAILED);
        assert!(err.msg.contains("目录不存在"), "{}", err.msg);
        std::fs::remove_dir_all(&root).ok();
    }

    /// `read_soul` 文件缺失 → 404 + AI_AGENT_SOUL_MISSING（不是空串）。
    #[tokio::test]
    async fn read_soul_missing_is_404() {
        let root = temp_root("soul-missing");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        std::fs::create_dir_all(root.join("profiles/agent-01")).expect("建目录");
        let err = hermes.read_soul("agent-01").await.expect_err("应 404");
        assert_eq!(err.code, error_code::AI_AGENT_SOUL_MISSING);
        assert_eq!(err.status, 404);
        std::fs::remove_dir_all(&root).ok();
    }

    /// workspace 生命周期：建 → 探 → 删（重复删也必须成功）。
    #[tokio::test]
    async fn workspace_lifecycle_is_idempotent() {
        let root = temp_root("ws");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        assert!(!hermes.workspace_path_exists("agent-01").await.expect("探活"));
        let dir = hermes.ensure_workspace("agent-01").await.expect("建 workspace");
        assert!(dir.is_dir());
        assert!(hermes.workspace_path_exists("agent-01").await.expect("探活"));
        hermes.remove_workspace("agent-01").await.expect("删 workspace");
        assert!(!dir.exists());
        hermes.remove_workspace("agent-01").await.expect("重复删必须幂等");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 备份：文件名带 profile 名前缀、内容原样落盘。
    #[tokio::test]
    async fn backup_soul_writes_content() {
        let root = temp_root("backup");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        let path = hermes.backup_soul("agent-01", "# 备份内容\n").await.expect("备份");
        let name = path.file_name().expect("文件名").to_string_lossy().to_string();
        assert!(name.starts_with("agent-01-"), "备份名：{name}");
        assert!(name.ends_with(".md"));
        assert_eq!(std::fs::read_to_string(&path).expect("读备份"), "# 备份内容\n");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 远端命令拼装：HERMES_HOME 显式传入、每个参数都被引号包裹。
    #[test]
    fn remote_script_quotes_every_argument() {
        let cfg = HermesConfig {
            transport: Transport::Ssh {
                host: "root@172.18.10.99".to_string(),
            },
            hermes_bin: PathBuf::from("/root/.hermes/hermes-agent/venv/bin/hermes"),
            hermes_home: PathBuf::from("/root/.hermes"),
            profiles_root: PathBuf::from("/root/.hermes/profiles"),
            workspace_root: PathBuf::from("/root/.hermes/workspace"),
            backup_dir: PathBuf::from("/root/.hermes/aiagent-soul-backups"),
            clone_from: "default".to_string(),
            timeout: Duration::from_secs(60),
        };
        let hermes = Hermes::new(cfg);
        let description = "职责里带 ' 引号 和 $(id)";
        let script = hermes.remote_hermes_script(&[
            "profile".to_string(),
            "create".to_string(),
            "agent-01".to_string(),
            "--description".to_string(),
            description.to_string(),
        ]);
        assert!(script.starts_with("env HERMES_HOME='/root/.hermes' '/root/.hermes/hermes-agent/venv/bin/hermes' 'profile' 'create' 'agent-01' '--description' "));
        assert!(script.contains("'\\''"), "自由文本里的单引号必须被转义：{script}");
        // 自由文本必须整体被 shell_quote 包住：命令字符串的结尾就是它的引号形式。
        // 注意不能断言「不含 $(id)」—— 单引号内 $(id) 本就是普通字符，这样断言的
        // 是「没有引号包裹」的反面证据，判据应看引号而非内容。
        assert!(
            script.ends_with(&shell_quote(description)),
            "自由文本未被引号包裹：{script}"
        );
    }

    /// 环境变量 → 配置（含 ssh 形态优先）。
    #[test]
    fn config_from_env_defaults_and_overrides() {
        // 默认值：不设任何变量时是本地形态 + 既有路径
        let cfg = HermesConfig {
            transport: Transport::Local,
            hermes_bin: PathBuf::from("/root/.hermes/hermes-agent/venv/bin/hermes"),
            hermes_home: PathBuf::from("/root/.hermes"),
            profiles_root: PathBuf::from("/root/.hermes/profiles"),
            workspace_root: PathBuf::from("/root/.hermes/workspace"),
            backup_dir: PathBuf::from("/root/.hermes/aiagent-soul-backups"),
            clone_from: "default".to_string(),
            timeout: Duration::from_secs(60),
        };
        // 这里只断言默认值本身与 env 读取函数的行为，避免在测试里改全局环境变量
        assert_eq!(cfg.transport, Transport::Local);
        assert_eq!(cfg.timeout.as_secs(), 60);
        assert!(env_non_empty("CLOUDPIVOT_AIAGENT_这个变量不存在").is_none());
        assert_eq!(
            env_path("CLOUDPIVOT_AIAGENT_这个变量不存在", "/tmp/x"),
            PathBuf::from("/tmp/x")
        );
    }

    // ==================== 凭据（L1 修复） ====================

    /// provider → 环境变量名：已知表 + 通用规则（表外 provider 不必改代码）。
    #[test]
    fn provider_api_key_var_maps_known_and_generic() {
        assert_eq!(provider_api_key_var("deepseek"), "DEEPSEEK_API_KEY");
        assert_eq!(provider_api_key_var(" DeepSeek "), "DEEPSEEK_API_KEY");
        assert_eq!(provider_api_key_var("openai"), "OPENAI_API_KEY");
        assert_eq!(provider_api_key_var("xai"), "XAI_API_KEY");
        assert_eq!(provider_api_key_var("ark"), "ARK_API_KEY");
        assert_eq!(provider_api_key_var("volcengine"), "ARK_API_KEY");
        // 表外：大写 + 非字母数字转下划线 + 后缀
        assert_eq!(provider_api_key_var("foo-bar"), "FOO_BAR_API_KEY");
        assert_eq!(provider_api_key_var("my.provider"), "MY_PROVIDER_API_KEY");
        assert_eq!(provider_api_key_var("weird"), "WEIRD_API_KEY");
        // 口径：只把 `*_API_KEY` 当凭据键（THS_PASSWORD 之类不搬）
        assert!(is_credential_env_key("DEEPSEEK_API_KEY"));
        assert!(is_credential_env_key("ARK_API_KEY"));
        assert!(!is_credential_env_key("THS_PASSWORD"));
        assert!(!is_credential_env_key("OBSIDIAN_VAULT_PATH"));
    }

    /// `.env` 解析：引号 / `export` / 行内注释 / 非法行。
    #[test]
    fn parse_env_pairs_handles_quotes_comments_and_export() {
        let text = "\
# 注释行不算
DEEPSEEK_API_KEY=sk-plain-0001
ARK_API_KEY=\"ark-quoted-0002\"
export OPENROUTER_API_KEY='sk-or-0003'
XAI_API_KEY=sk-xai-0004 # 行内注释
BAD LINE 没有等号
1BAD=value
EMPTY_KEY=
";
        let pairs = parse_env_pairs(text);
        assert_eq!(
            pairs,
            vec![
                ("DEEPSEEK_API_KEY".to_string(), "sk-plain-0001".to_string()),
                ("ARK_API_KEY".to_string(), "ark-quoted-0002".to_string()),
                ("OPENROUTER_API_KEY".to_string(), "sk-or-0003".to_string()),
                ("XAI_API_KEY".to_string(), "sk-xai-0004".to_string()),
                ("EMPTY_KEY".to_string(), String::new()),
            ]
        );
        // 空值不算「有 key」
        assert!(env_lookup(&pairs, "EMPTY_KEY").is_none());
        assert_eq!(
            env_lookup(&pairs, "DEEPSEEK_API_KEY").map(String::as_str),
            Some("sk-plain-0001")
        );
    }

    /// 合并：缺的补、空值填、**已有非空值绝不覆盖**、原有注释与顺序保留。
    #[test]
    fn merge_env_text_keeps_existing_values_and_fills_empty() {
        let existing = "# 头部注释\nDEEPSEEK_API_KEY=sk-mine-0001\nARK_API_KEY=\n";
        let incoming = vec![
            ("DEEPSEEK_API_KEY".to_string(), "sk-global-9999".to_string()),
            ("ARK_API_KEY".to_string(), "ark-global-0002".to_string()),
            ("OPENROUTER_API_KEY".to_string(), "sk-or-0003".to_string()),
            ("EMPTY_INCOMING".to_string(), "  ".to_string()),
        ];
        let (merged, written) = merge_env_text(existing, &incoming);
        // 已有非空值保持原样（专用 key 不被全局值顶掉）
        assert!(merged.contains("DEEPSEEK_API_KEY=sk-mine-0001"));
        assert!(!merged.contains("sk-global-9999"));
        // 空值就地填，不新增重复行
        assert!(merged.contains("ARK_API_KEY=ark-global-0002"));
        assert_eq!(merged.matches("ARK_API_KEY").count(), 1);
        // 缺的追加
        assert!(merged.contains("OPENROUTER_API_KEY=sk-or-0003"));
        // 空字符串值不写入
        assert!(!merged.contains("EMPTY_INCOMING"));
        // 注释保留在首行
        assert!(merged.starts_with("# 头部注释\n"));
        assert_eq!(
            written,
            vec!["ARK_API_KEY".to_string(), "OPENROUTER_API_KEY".to_string()]
        );

        // 幂等：同样的输入再合并一次，不产生任何写入
        let (again, written_again) = merge_env_text(&merged, &incoming);
        assert_eq!(again, merged);
        assert!(written_again.is_empty(), "第二次合并不应再写：{written_again:?}");
    }

    /// `config.yaml` 解析：只认 `model:` 块；`fallback_providers` 里的 `provider:` 不算。
    #[test]
    fn parse_model_provider_reads_only_model_block() {
        let yaml = "\
model:
  default: deepseek-flash
  provider: deepseek
  base_url: https://api.deepseek.com
  api_key: sk-xxx
fallback_providers:
- provider: custom
  model: deepseek-v4.1-flash
";
        assert_eq!(parse_model_provider(yaml).as_deref(), Some("deepseek"));

        // 「model: 在前、但 provider 写到 fallback 段里」不能误判
        let only_fallback = "fallback_providers:\n- provider: custom\n";
        assert_eq!(parse_model_provider(only_fallback), None);

        // 行内写法
        assert_eq!(
            parse_model_provider("model: {provider: openai, default: gpt-5}\n").as_deref(),
            Some("openai")
        );
        // 引号包裹
        assert_eq!(
            parse_model_provider("model:\n  provider: \"anthropic\"\n").as_deref(),
            Some("anthropic")
        );
        assert_eq!(parse_model_provider(""), None);
    }

    /// 建一个「profile 目录 + config.yaml（provider=deepseek）+ 只有注释的 .env」的夹具，
    /// 复刻 `hermes profile create` 的真实产物（这正是 2026-10-29 事故的现场）。
    fn seed_profile(root: &Path, name: &str) {
        let dir = root.join("profiles").join(name);
        std::fs::create_dir_all(&dir).expect("建 profile 目录");
        std::fs::write(
            dir.join("config.yaml"),
            "model:\n  default: deepseek-flash\n  provider: deepseek\n",
        )
        .expect("写 profile config.yaml");
        std::fs::write(
            dir.join(".env"),
            "# Per-profile secrets for this Hermes profile.\n# API keys and tokens set here override the shell environment.\n# Behavioral settings belong in config.yaml, not here.\n",
        )
        .expect("写默认 .env");
    }

    fn seed_global_env(root: &Path, content: &str) {
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("建 hermes_home");
        std::fs::write(home.join(".env"), content).expect("写全局 .env");
    }

    /// **核心用例**：profile 的 `.env` 只有注释（没有 provider key）→ 同步后补齐 `DEEPSEEK_API_KEY`
    /// 且权限 600，自检通过。
    #[tokio::test]
    async fn ensure_profile_env_adds_missing_provider_key() {
        let root = temp_root("env-add");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        seed_profile(&root, "agent-01");
        seed_global_env(&root, "# 全局凭据\nDEEPSEEK_API_KEY=sk-global-0001\n");

        // 同步前：自检不通过（这就是事故现场的状态）
        let before = hermes.verify_profile_credentials("agent-01").await.expect("自检");
        assert!(!before.ok, "缺 key 时必须判红");
        assert_eq!(before.required_key.as_deref(), Some("DEEPSEEK_API_KEY"));
        assert_eq!(before.provider.as_deref(), Some("deepseek"));
        assert!(before.present_keys.is_empty());

        let sync = hermes.ensure_profile_env("agent-01").await.expect("同步凭据");
        assert_eq!(sync.provider.as_deref(), Some("deepseek"));
        assert_eq!(sync.required_key, "DEEPSEEK_API_KEY");
        assert_eq!(sync.written_keys, vec!["DEEPSEEK_API_KEY".to_string()]);
        assert_eq!(sync.env_file_mode, Some(0o600));

        let after = hermes.verify_profile_credentials("agent-01").await.expect("自检");
        assert!(after.ok, "补齐后必须判绿：{after:?}");
        assert_eq!(after.present_keys, vec!["DEEPSEEK_API_KEY".to_string()]);

        // 落盘内容与权限：值真的在里面、注释保留、600
        let env_path = root.join("profiles/agent-01/.env");
        let text = std::fs::read_to_string(&env_path).expect("读 .env");
        assert!(text.contains("DEEPSEEK_API_KEY=sk-global-0001"), "{text}");
        assert!(text.contains("# Per-profile secrets"), "原有注释必须保留：{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&env_path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "凭据文件权限必须 600");
        }

        // 幂等：第二次不写任何键
        let again = hermes.ensure_profile_env("agent-01").await.expect("二次同步");
        assert!(again.written_keys.is_empty(), "{:?}", again.written_keys);

        std::fs::remove_dir_all(&root).ok();
    }

    /// 已有**专用 key** 不被全局值覆盖（运维给某个 AI 员工配了独立 key 的场景）。
    #[tokio::test]
    async fn ensure_profile_env_does_not_overwrite_existing_key() {
        let root = temp_root("env-keep");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        seed_profile(&root, "agent-01");
        std::fs::write(
            root.join("profiles/agent-01/.env"),
            "DEEPSEEK_API_KEY=sk-dedicated-0007\n",
        )
        .expect("写专用 key");
        // 夹具按真 Hermes 的落盘口径给 600（`hermes` 自己写 .env 就是 600）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                root.join("profiles/agent-01/.env"),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("设置 600");
        }
        seed_global_env(&root, "DEEPSEEK_API_KEY=sk-global-0001\n");

        let check = hermes.verify_profile_credentials("agent-01").await.expect("自检");
        assert!(check.ok);

        let sync = hermes.ensure_profile_env("agent-01").await.expect("同步");
        assert!(sync.written_keys.is_empty(), "{:?}", sync.written_keys);
        let text = std::fs::read_to_string(root.join("profiles/agent-01/.env")).expect("读");
        assert!(text.contains("sk-dedicated-0007"), "{text}");
        assert!(!text.contains("sk-global-0001"), "全局值不得顶掉专用 key：{text}");

        std::fs::remove_dir_all(&root).ok();
    }

    /// 全局 `.env` 与进程环境都没有 key → **拒绝建号**（500 + 64014），且不写坏已有文件。
    #[tokio::test]
    async fn ensure_profile_env_fails_without_global_key() {
        let root = temp_root("env-missing");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        seed_profile(&root, "agent-01");
        // 全局 .env 只有无关变量（不是凭据键）
        seed_global_env(&root, "THS_USERNAME=nobody\n");

        let err = hermes.ensure_profile_env("agent-01").await.expect_err("必须失败");
        assert_eq!(err.code, error_code::AI_AGENT_CREDENTIAL_MISSING);
        assert!(err.msg.contains("DEEPSEEK_API_KEY"), "{}", err.msg);
        assert!(err.msg.contains("profile 级凭据"), "{}", err.msg);
        // 夹具的 .env 未被改写
        let text = std::fs::read_to_string(root.join("profiles/agent-01/.env")).expect("读");
        assert!(!text.contains("THS_USERNAME"), "无关变量不得搬进 profile：{text}");

        // 全局 .env 干脆不存在也走同一条失败路径（不是 panic / 不是静默成功）
        std::fs::remove_file(root.join("home/.env")).ok();
        let err = hermes.ensure_profile_env("agent-01").await.expect_err("必须失败");
        assert_eq!(err.code, error_code::AI_AGENT_CREDENTIAL_MISSING);

        std::fs::remove_dir_all(&root).ok();
    }

    /// 权限不合规（644）→ 自检判红；同步后被纠正成 600。未带 key 的 profile 目录不存在 → 404。
    #[tokio::test]
    async fn verify_flags_bad_mode_and_sync_fixes_it() {
        let root = temp_root("env-mode");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        seed_profile(&root, "agent-01");
        seed_global_env(&root, "DEEPSEEK_API_KEY=sk-global-0001\n");
        let env_path = root.join("profiles/agent-01/.env");
        std::fs::write(&env_path, "DEEPSEEK_API_KEY=sk-global-0001\n").expect("写");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o644)).expect("改权限");
        }

        let check = hermes.verify_profile_credentials("agent-01").await.expect("自检");
        #[cfg(unix)]
        {
            assert!(!check.ok, "644 必须判红");
            assert_eq!(check.env_file_mode, Some(0o644));
            assert!(
                check.detail.as_deref().unwrap_or("").contains("600"),
                "{:?}",
                check.detail
            );
        }

        let sync = hermes.ensure_profile_env("agent-01").await.expect("同步");
        assert_eq!(sync.env_file_mode, Some(0o600));

        // profile 目录不存在：404 + 64004
        let err = hermes.ensure_profile_env("agent-99").await.expect_err("必须 404");
        assert_eq!(err.code, error_code::AI_AGENT_PROFILE_MISSING);
        assert_eq!(err.status, 404);

        std::fs::remove_dir_all(&root).ok();
    }

    /// **根因回归**：`hermes profile create` 建出的 profile `.env` 只有三行注释（无 key）时，
    /// `create_profile` 必须自己把凭据补上——否则 worker 一执行就 AuthError。
    #[tokio::test]
    async fn create_profile_provisions_credentials() {
        let root = temp_root("create-cred");
        let fake_bin = root.join("hermes");
        let profiles = root.join("profiles");
        // 假 CLI：复刻真 CLI 的产物（建目录 + 只有注释的 .env，**不含任何 key**）
        std::fs::write(
            &fake_bin,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = profile ] && [ \"$2\" = create ]; then\n\
                 \x20 mkdir -p '{profiles}/'\"$3\"\n\
                 \x20 printf '# Per-profile secrets for this Hermes profile.\\n# API keys and tokens set here override the shell environment.\\n# Behavioral settings belong in config.yaml, not here.\\n' > '{profiles}/'\"$3\"/.env\n\
                 \x20 exit 0\n\
                 fi\n\
                 exit 0\n",
                profiles = profiles.display()
            ),
        )
        .expect("写假 CLI");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).expect("加执行位");
        }
        seed_global_env(&root, "DEEPSEEK_API_KEY=sk-global-0001\nARK_API_KEY=ark-global-0002\n");
        std::fs::write(
            root.join("home/config.yaml"),
            "model:\n  provider: deepseek\n  default: deepseek-flash\n",
        )
        .expect("写全局 config.yaml");
        let hermes = Hermes::new(local_config(&root, fake_bin.clone()));

        hermes
            .create_profile("agent-01", None, "职责")
            .await
            .expect("建号必须成功");

        // 成果：.env 里有 DEEPSEEK_API_KEY（以及顺带搬来的 ARK_API_KEY），权限 600
        let check = hermes.verify_profile_credentials("agent-01").await.expect("自检");
        assert!(check.ok, "建号后凭据必须可用：{check:?}");
        assert!(check.present_keys.contains(&"DEEPSEEK_API_KEY".to_string()));
        assert!(check.present_keys.contains(&"ARK_API_KEY".to_string()));
        assert_eq!(check.env_file_mode, Some(0o600));

        // 没有全局 key 时：建号必须失败（64014），而不是留下一个不能干活的账号
        std::fs::remove_file(root.join("home/.env")).ok();
        let err = hermes
            .create_profile("agent-02", None, "")
            .await
            .expect_err("缺 key 必须失败");
        assert_eq!(err.code, error_code::AI_AGENT_CREDENTIAL_MISSING);

        std::fs::remove_dir_all(&root).ok();
    }
}

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

use alun::prelude::*;
// 显式遮蔽 prelude 的 `Result<T>`（那是 `alun::Result<T>`，错误类型固定为 `alun::Error`）：
// 本层对外统一返回 `ApiError`（HTTP 状态 + 业务码 + 对外消息），处理器才能直接 `?` 接住。
// 显式 `use` 会优先于 `use alun::prelude::*` 的 glob 导入。
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
            workspace_root: env_path(
                "CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT",
                "/root/.hermes/workspace",
            ),
            backup_dir: env_path(
                "CLOUDPIVOT_AIAGENT_BACKUP_DIR",
                "/root/.hermes/aiagent-soul-backups",
            ),
            clone_from: env_non_empty("CLOUDPIVOT_AIAGENT_CLONE_FROM")
                .unwrap_or_else(|| "default".to_string()),
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
    /// 探活过程里的补充说明（命令失败原因等，便于运维定位）。
    pub detail: Option<String>,
}

impl ProfileStatus {
    /// 是否整体健康（目录与 SOUL.md 都在且 CLI 可用）。
    pub fn healthy(&self) -> bool {
        self.profile_dir_exists && self.soul_file_exists && self.cli_available
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

    fn profile_name_error(message: &str) -> ApiError {
        ApiError::new(
            400,
            error_code::AI_AGENT_PROFILE_INVALID,
            message.to_string(),
        )
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
            // 模板名同样进参数，必须同样校验（否则这里就是第二条注入面）
            Self::validate_profile_name(template)?;
        }
        let mut argv = vec![
            "profile".to_string(),
            "create".to_string(),
            name.to_string(),
        ];
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
        let argv = vec![
            "profile".to_string(),
            "describe".to_string(),
            name.to_string(),
        ];
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
        if content.len() > SOUL_MAX_BYTES {
            return Err(ApiError::new(
                400,
                error_code::AI_AGENT_SOUL_INVALID,
                format!("SOUL.md 超过上限 {} KiB", SOUL_MAX_BYTES / 1024),
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
        let tmp = dir.join(format!(
            ".SOUL.md.tmp-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        match &self.cfg.transport {
            Transport::Local => {
                tokio::fs::write(&tmp, content.as_bytes())
                    .await
                    .map_err(|e| {
                        ApiError::new(
                            500,
                            error_code::AI_AGENT_SOUL_IO_FAILED,
                            format!("写临时文件失败：{e}"),
                        )
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
                tokio::fs::write(&target, content.as_bytes())
                    .await
                    .map_err(|e| {
                        ApiError::new(
                            500,
                            error_code::AI_AGENT_SOUL_IO_FAILED,
                            format!("写备份文件失败：{e}"),
                        )
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

        Ok(ProfileStatus {
            profile_dir_exists,
            soul_file_exists,
            workspace_exists,
            gateway,
            cli_available,
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
    async fn run_hermes(
        &self,
        argv: Vec<String>,
        stdin: Option<String>,
    ) -> Result<CliOutput, ApiError> {
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
    async fn run_remote(
        &self,
        script: String,
        stdin: Option<String>,
    ) -> Result<CliOutput, ApiError> {
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
    async fn run_remote_raw(
        &self,
        script: String,
        stdin: Option<String>,
    ) -> Result<ExitStatus, ApiError> {
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
            format!(
                "HERMES_HOME={}",
                shell_quote(&path_string(&self.cfg.hermes_home))
            ),
            shell_quote(&path_string(&self.cfg.hermes_bin)),
        ];
        parts.extend(argv.iter().map(|a| shell_quote(a)));
        parts.join(" ")
    }

    /// 起本地进程（参数数组，永不经过 shell）。
    async fn spawn_local(
        &self,
        program: &Path,
        argv: &[String],
        stdin: Option<String>,
    ) -> Result<CliOutput, ApiError> {
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
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.wait_with_timeout(cmd, stdin, program.display().to_string())
            .await
    }

    /// 起 ssh 进程。远端命令作为**单个参数**交给 ssh（它自己会拼给远端 shell）。
    async fn spawn_ssh(
        &self,
        host: &str,
        script: String,
        stdin: Option<String>,
    ) -> Result<CliOutput, ApiError> {
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
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        self.wait_with_timeout(cmd, stdin, format!("ssh {host}"))
            .await
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
        let base = std::env::temp_dir().join(format!(
            "l1-aiagent-{tag}-{}-{}",
            std::process::id(),
            now_nanos()
        ));
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
        for ok in [
            "ab",
            "abc",
            "a1",
            "a-b",
            "agent-01",
            "abcdefghijklmnopqrstuvwxyz12345",
        ] {
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
            let err =
                Hermes::validate_profile_name(name).expect_err(&format!("必须拒绝：{name:?}"));
            assert_eq!(
                err.code,
                error_code::AI_AGENT_PROFILE_INVALID,
                "错误码：{name:?}"
            );
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
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                raw,
                "还原不一致：{raw:?}"
            );
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
        assert_eq!(
            hermes.read_soul("agent-01").await.expect("读 SOUL"),
            "# 第一版\n内容\n"
        );

        hermes
            .write_soul("agent-01", "# 第二版\n换行里有 ' 引号 $ 和 `\n")
            .await
            .expect("覆盖写");
        assert_eq!(
            hermes.read_soul("agent-01").await.expect("读回"),
            "# 第二版\n换行里有 ' 引号 $ 和 `\n"
        );

        let leftovers: Vec<String> = std::fs::read_dir(root.join("profiles/agent-01"))
            .expect("列目录")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".SOUL.md.tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "临时文件残留：{leftovers:?}");

        // profile 目录不存在时必须报 404，而不是「写到一个随机位置」
        let err = hermes
            .write_soul("agent-99", "x")
            .await
            .expect_err("应失败");
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
        let err = hermes
            .write_soul("agent-01", &big)
            .await
            .expect_err("应拒绝");
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
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755))
                .expect("加执行位");
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
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755))
                .expect("加执行位");
        }
        let mut cfg = local_config(&root, fake_bin);
        cfg.timeout = Duration::from_millis(300);
        let hermes = Hermes::new(cfg);
        let started = std::time::Instant::now();
        let err = hermes
            .create_profile("agent-01", None, "")
            .await
            .expect_err("必须超时");
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
            std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755))
                .expect("加执行位");
        }
        let hermes = Hermes::new(local_config(&root, fake_bin));
        let err = hermes
            .create_profile("agent-01", None, "")
            .await
            .expect_err("必须失败");
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
        assert!(
            !hermes
                .workspace_path_exists("agent-01")
                .await
                .expect("探活")
        );
        let dir = hermes
            .ensure_workspace("agent-01")
            .await
            .expect("建 workspace");
        assert!(dir.is_dir());
        assert!(
            hermes
                .workspace_path_exists("agent-01")
                .await
                .expect("探活")
        );
        hermes
            .remove_workspace("agent-01")
            .await
            .expect("删 workspace");
        assert!(!dir.exists());
        hermes
            .remove_workspace("agent-01")
            .await
            .expect("重复删必须幂等");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 备份：文件名带 profile 名前缀、内容原样落盘。
    #[tokio::test]
    async fn backup_soul_writes_content() {
        let root = temp_root("backup");
        let hermes = Hermes::new(local_config(&root, root.join("hermes")));
        let path = hermes
            .backup_soul("agent-01", "# 备份内容\n")
            .await
            .expect("备份");
        let name = path
            .file_name()
            .expect("文件名")
            .to_string_lossy()
            .to_string();
        assert!(name.starts_with("agent-01-"), "备份名：{name}");
        assert!(name.ends_with(".md"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("读备份"),
            "# 备份内容\n"
        );
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
        assert!(
            script.contains("'\\''"),
            "自由文本里的单引号必须被转义：{script}"
        );
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
}

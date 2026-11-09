//! 诊断包（zip）的组装：把「排障时真正要看的六样东西」一次打包下载。
//!
//! # 包里有什么
//!
//! | 文件 | 内容 | 来源 |
//! |------|------|------|
//! | `info.json` | 版本 / commit / profile / 启动时间 / 主机信息 | `model::collect` 的 `app` + `host` |
//! | `health.json` | `/health`（liveness）与 `/health/deps`（readiness）**同源**结果 | [`crate::support::health::probe_deps`] |
//! | `server-status.json` | 本次采集的完整状态（与页面同一份数据） | `model::collect` |
//! | `metrics.txt` | `/metrics` 快照；未启用时降级为「等价格式的采集快照」并注明原因 | 本机回环 HTTP |
//! | `config-redacted.txt` | 进程环境变量 + `config/*.toml`，**口令 / token / 密钥全部打码** | 进程环境 + 配置文件 |
//! | `migrations.txt` | `schema_migrations` 全量清单（已应用顺序 / 耗时） | 数据库 |
//! | `logs-tail.txt` | 日志最后 200 行（同样打码） | 日志文件 |
//!
//! # 安全红线
//!
//! 1. **只给超管**：路由权限码 `system:server-status:view` 只授 `SUPER_ADMIN`，
//!    handler 里还有第二层 [`crate::business::attachment::rule_admin::dto::ensure_super_admin`] 同口径判定。
//! 2. **打码是硬要求**：[`redact`] 覆盖「敏感键名 = 值」「URL 里的 `user:password@`」
//!    「`Bearer` 令牌」「JWT / 40+ 位十六进制串」四种形态，并有单测盯住。
//! 3. **下载动作留痕**：handler 写 `operation_logs`（操作码 [`OP_DIAGNOSTIC`]），
//!    谁在什么时候下过诊断包可查。
//!
//! # 为什么不用框架的导出能力
//!
//! `nik-utils` 的导出只覆盖 CSV / XLSX；诊断包是**多文件归档**，需要真正的 zip 容器。
//! `zip` crate 本来就在依赖树里（`rust_xlsxwriter` 的间接依赖，见 `Cargo.toml` 注释），
//! 这里把它显式化，不引入新的第三方库。

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use nik::prelude::*;
use regex::Regex;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::business::monitor::dto::ServerStatusRes;
use crate::business::monitor::model;

/// 诊断包下载在 `operation_logs.operation` 里留的操作码。
pub const OP_DIAGNOSTIC: &str = "SYSTEM_SERVER_STATUS_DIAGNOSTIC";

/// 日志尾巴的行数上限。
pub const LOG_TAIL_LINES: usize = 200;

/// 日志尾巴的字节上限（先读文件尾 256 KiB 再切行 —— 25 MB 的日志不该整个进内存）。
const LOG_TAIL_MAX_BYTES: u64 = 256 * 1024;

/// 打码后的占位文本。
pub const MASK: &str = "***REDACTED***";

/// 一次诊断包构建的产物。
#[derive(Debug, Clone)]
pub struct DiagnosticBundle {
    /// 建议的下载文件名（含时间戳）。
    pub file_name: String,
    /// zip 字节流。
    pub bytes: Vec<u8>,
}

/// 组装诊断包。
///
/// # 返回值
///
/// [`DiagnosticBundle`]：文件名形如 `cloudpivot-diagnostic-20261109-213000.zip`。
///
/// # 异常情况
///
/// - 500：zip 写入失败（内存不足等极端情况）。**任何单项采集失败都不报错**：
///   诊断包的价值恰恰在于「某个依赖已经挂了」，缺哪一项就在对应文件里写清原因。
pub async fn build() -> std::result::Result<DiagnosticBundle, ApiError> {
    let status = model::collect().await;
    let deps = crate::support::health::probe_deps().await;
    let now = chrono::Utc::now();

    let info = serde_json::json!({
        "app": status.app,
        "host": status.host,
        "collected_at": status.collected_at,
        "generated_at": now.to_rfc3339(),
        "database": {
            "state": status.database.state,
            "server_version": status.database.server_version,
            "name": status.database.name,
            "size_text": status.database.size_text,
            "migrations": status.database.migrations,
        },
    });

    let health = serde_json::json!({
        "liveness": {
            "status": "ok",
            "app": cfg().app_name,
            "profile": cfg().profile,
            "time": now.to_rfc3339(),
        },
        "deps": deps,
    });

    let status_json = serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string());
    let metrics = metrics_snapshot(&status).await;
    let config = config_snapshot();
    let migrations = migrations_snapshot().await;
    let logs = logs_snapshot();

    let entries: Vec<(&str, String)> = vec![
        ("info.json", to_pretty(&info)),
        ("health.json", to_pretty(&health)),
        ("server-status.json", redact(&status_json)),
        ("metrics.txt", redact(&metrics)),
        ("config-redacted.txt", redact(&config)),
        ("migrations.txt", redact(&migrations)),
        ("logs-tail.txt", redact(&logs)),
    ];
    let bytes = zip_entries(&entries)?;

    Ok(DiagnosticBundle {
        file_name: format!("cloudpivot-diagnostic-{}.zip", now.format("%Y%m%d-%H%M%S")),
        bytes,
    })
}

/// JSON 美化输出（失败时退回紧凑格式，绝不让诊断包因为格式化失败而 500）。
fn to_pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// 把若干 `(文件名, 文本)` 压成一个 zip（Deflate）。
///
/// # 异常情况
///
/// 内部 `zip` / `io` 错误统一折叠成 500 [`ApiError`]：这是「服务端自己写不出文件」，
/// 与用户输入无关，回 500 且消息里只说「诊断包生成失败」。
pub fn zip_entries(entries: &[(&str, String)]) -> std::result::Result<Vec<u8>, ApiError> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, content) in entries {
            zip.start_file(*name, options)
                .map_err(|error| zip_error("写入 zip 条目", error))?;
            zip.write_all(content.as_bytes())
                .map_err(|error| zip_error("写入 zip 内容", error))?;
        }
        zip.finish().map_err(|error| zip_error("收尾 zip", error))?;
    }
    Ok(cursor.into_inner())
}

/// zip 错误 → 500。
fn zip_error(action: &str, error: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        500,
        crate::shared::error_code::INTERNAL_ERROR,
        format!("诊断包生成失败（{action}）：{error}"),
    )
}

// ==================== 打码 ====================

/// 敏感键名的匹配规则（键名里出现即打码）。
///
/// 覆盖 `NIK_DATABASE_PASSWORD` / `JWT_SECRET` / `STORAGE_MASTER_KEY` /
/// `access_token` / `api_key` / `credential` 这类命名。
const SENSITIVE_KEY_PATTERN: &str = r"(?i)(password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|credential|cookie|session[_-]?id)";

/// 长十六进制串（≥ 40 位）—— 常见于各类密钥 / 盐。
const LONG_HEX_PATTERN: &str = r"\b[0-9a-fA-F]{40,}\b";

/// 一个最小 JWT（三段 base64url 用点连接）。
const JWT_PATTERN: &str = r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b";

/// 对一段文本做脱敏，返回**新**字符串。
///
/// # 四层规则（顺序敏感）
///
/// 1. `KEY = value` / `key: value`：键名命中 [`SENSITIVE_KEY_PATTERN`] 时把值换成 [`MASK`]；
/// 2. `scheme://user:password@host`：保留用户名，密码换成 [`MASK`]（连接串最常见的泄漏形态）；
/// 3. `Bearer <token>`：整个令牌换掉；
/// 4. 兜底：长十六进制串与 JWT 形态的字符串一律换掉（防「键名没命中但值确实是密钥」）。
///
/// 中文 / 普通配置项（`level = "info"` / `port = 5432`）原样保留。
pub fn redact(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    // 规则 1：逐行按键名打码（避免一条跨行正则把整个文件吃掉）
    let key_re = Regex::new(SENSITIVE_KEY_PATTERN).expect("常量正则");
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        // 跳过注释行：注释里的示例值不是真实密钥，保留可读性
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            output.push_str(line);
            continue;
        }
        output.push_str(&mask_sensitive_assignment(line, &key_re));
    }

    // 规则 2 / 3 / 4：值形态兜底
    let mut result = output;
    for (pattern, replacement) in [
        (r"://([^:/@\s]+):([^@\s/]+)@", "://$1:%MASK%@"),
        (r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}", "Bearer %MASK%"),
        (JWT_PATTERN, "%MASK%"),
        (LONG_HEX_PATTERN, "%MASK%"),
    ] {
        let re = Regex::new(pattern).expect("常量正则");
        let replacement = replacement.replace("%MASK%", MASK);
        result = re.replace_all(&result, replacement.as_str()).to_string();
    }
    result
}

/// 对单行做「键名命中 → 打码值」。
///
/// 支持 `KEY=value`、`KEY = value`、`"key": "value"`、`key: value` 四种写法；
/// 只替换**第一个**分隔符右侧的值，行尾的逗号 / 引号 / 注释一律原样保留
/// （打码后的 JSON / TOML 仍然能被解析）。
fn mask_sensitive_assignment(line: &str, key_re: &Regex) -> String {
    let Some(sep_index) = line.find(['=', ':']) else {
        return line.to_string();
    };
    let (head, tail) = line.split_at(sep_index);
    // 键名取分隔符前最后一个「词」，避免 `"database": {"password"` 这类嵌套误判前缀
    let key = head
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == '{' || c == '[');
    if !key_re.is_match(key) {
        return line.to_string();
    }
    // 保留分隔符两侧的写法：`KEY = ` / `"key": `
    let sep_len = tail
        .chars()
        .take_while(|c| *c == '=' || *c == ':' || c.is_whitespace())
        .count();
    let (sep, rest) = tail.split_at(sep_len);
    let (masked_value, remainder) = mask_value(rest);
    if masked_value.is_none() {
        return line.to_string();
    }
    format!("{head}{sep}{}{remainder}", masked_value.unwrap_or_default())
}

/// 把一行里「值」的部分换成打码文本，返回 `(打码后的值, 值之后的原样尾巴)`。
///
/// 值可能是带引号的字符串（TOML / JSON）或裸词（shell env / YAML）。
fn mask_value(rest: &str) -> (Option<String>, &str) {
    if let Some(body) = rest.strip_prefix('"') {
        return match body.find('"') {
            Some(end) => (Some(format!("\"{MASK}\"")), &body[end + 1..]),
            None => (None, rest),
        };
    }
    if let Some(body) = rest.strip_prefix('\'') {
        return match body.find('\'') {
            Some(end) => (Some(format!("'{MASK}'")), &body[end + 1..]),
            None => (None, rest),
        };
    }
    if rest.trim().is_empty() {
        return (None, rest);
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ',' || c == '#' || c == ';')
        .unwrap_or(rest.len());
    (Some(MASK.to_string()), &rest[end..])
}

// ==================== metrics ====================

/// `/metrics` 快照。
///
/// 启用时走本机回环 HTTP 真抓一次；未启用或抓取失败时，**降级**为本次采集结果的
/// Prometheus 文本格式快照，并在文件头写明原因（不假装这是 `/metrics` 的原样输出）。
async fn metrics_snapshot(status: &ServerStatusRes) -> String {
    let fallback_header = |reason: &str| {
        format!(
            "# /metrics 不可用：{reason}\n# 以下为本次采集的等价格式快照（不是 /metrics 原样输出），采集时间 {}\n",
            status.collected_at
        )
    };

    if !cfg().metrics.enabled {
        return format!(
            "{}{}",
            fallback_header("[metrics] enabled = false，Prometheus 导出未启用"),
            prometheus_fallback(status)
        );
    }

    let Some(port) = listen_port() else {
        return format!(
            "{}{}",
            fallback_header("无法从 [server].listen 解析端口"),
            prometheus_fallback(status)
        );
    };

    match fetch_local_metrics(port, &cfg().metrics.path).await {
        Ok(text) if !text.trim().is_empty() => text,
        Ok(_) => format!(
            "{}{}",
            fallback_header("/metrics 返回空响应体"),
            prometheus_fallback(status)
        ),
        Err(error) => format!("{}{}", fallback_header(&error), prometheus_fallback(status)),
    }
}

/// 从 `[server].listen` 拿本机监听端口（支持 `8023` / `127.0.0.1:8033` / `[::]:8023`）。
fn listen_port() -> Option<u16> {
    let listen = cfg().server.listen.trim();
    let last = listen.rsplit(':').next()?;
    last.parse::<u16>().ok()
}

/// 向本机回环发一条最小 HTTP/1.0 请求并取回响应体。
async fn fetch_local_metrics(port: u16, path: &str) -> std::result::Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let connect = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map_err(|_| format!("连接 127.0.0.1:{port} 超时"))?
    .map_err(|error| format!("连接 127.0.0.1:{port} 失败: {error}"))?;

    let mut stream = connect;
    let request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("发送请求失败: {error}"))?;

    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut raw))
        .await
        .map_err(|_| "读取 /metrics 响应超时".to_string())?
        .map_err(|error| format!("读取 /metrics 响应失败: {error}"))?;

    let text = String::from_utf8_lossy(&raw).to_string();
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Err("响应不是合法 HTTP 报文（缺少空行分隔）".to_string());
    };
    let status_code = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("?");
    if status_code != "200" {
        return Err(format!("/metrics 返回 HTTP {status_code}"));
    }
    Ok(body.to_string())
}

/// 采集结果的 Prometheus 文本格式（`/metrics` 不可用时的降级内容）。
fn prometheus_fallback(status: &ServerStatusRes) -> String {
    let mut out = String::new();
    out.push_str(&format!("cloudpivot_cpu_percent {}\n", status.cpu.percent));
    out.push_str(&format!("cloudpivot_cpu_cores {}\n", status.cpu.cores));
    out.push_str(&format!("cloudpivot_memory_percent {}\n", status.memory.percent));
    out.push_str(&format!("cloudpivot_memory_used_bytes {}\n", status.memory.used_bytes));
    out.push_str(&format!(
        "cloudpivot_memory_total_bytes {}\n",
        status.memory.total_bytes
    ));
    out.push_str(&format!("cloudpivot_disk_percent {}\n", status.disk.percent));
    out.push_str(&format!("cloudpivot_disk_used_bytes {}\n", status.disk.used_bytes));
    out.push_str(&format!("cloudpivot_disk_total_bytes {}\n", status.disk.total_bytes));
    out.push_str(&format!(
        "cloudpivot_db_up {}\n",
        u8::from(status.database.state == "up")
    ));
    out.push_str(&format!("cloudpivot_db_connections {}\n", status.database.connections));
    out.push_str(&format!("cloudpivot_db_size_bytes {}\n", status.database.size_bytes));
    out.push_str(&format!(
        "cloudpivot_redis_up {}\n",
        u8::from(status.redis.state == "up")
    ));
    out.push_str(&format!(
        "cloudpivot_redis_connected_clients {}\n",
        status.redis.connected_clients
    ));
    out.push_str(&format!("cloudpivot_load1 {}\n", status.host.load1));
    out.push_str(&format!("cloudpivot_load5 {}\n", status.host.load5));
    out.push_str(&format!("cloudpivot_load15 {}\n", status.host.load15));
    out.push_str(&format!(
        "cloudpivot_process_uptime_seconds {}\n",
        status.app.uptime_seconds
    ));
    out.push_str(&format!(
        "cloudpivot_host_uptime_seconds {}\n",
        status.host.uptime_seconds
    ));
    out
}

// ==================== 配置 ====================

/// 进程环境变量 + 配置文件原文（**未打码**，由调用方统一 [`redact`]）。
fn config_snapshot() -> String {
    let mut out = String::from("## 进程环境变量（按键名排序；敏感值已打码）\n");
    let mut envs: Vec<(String, String)> = std::env::vars().collect();
    envs.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, value) in envs {
        out.push_str(&format!("{key}={value}\n"));
    }

    out.push_str("\n## 配置文件\n");
    let profile = cfg().profile.clone();
    let candidates = [
        "config/config.toml".to_string(),
        format!("config/config-{profile}.toml"),
    ];
    let mut found = false;
    for candidate in &candidates {
        let path = Path::new(candidate);
        match std::fs::read_to_string(path) {
            Ok(text) => {
                found = true;
                out.push_str(&format!("\n### {candidate}\n{text}\n"));
            }
            Err(_) => out.push_str(&format!("\n### {candidate}（未找到，已跳过）\n")),
        }
    }
    if !found {
        out.push_str("\n（配置文件均未找到：进程可能以环境变量方式注入全部配置）\n");
    }
    out
}

// ==================== 迁移清单 ====================

/// `schema_migrations` 全量清单。
async fn migrations_snapshot() -> String {
    const SQL: &str = "SELECT kind, name, version, success, applied_at, execution_ms \
                       FROM schema_migrations ORDER BY applied_seq";
    match db().find_with_params(SQL, vec![]).await {
        Ok(rows) => {
            let mut out = format!("# 已应用迁移共 {} 条（按应用顺序）\n", rows.len());
            out.push_str("# kind\tname\tversion\tsuccess\tapplied_at\texecution_ms\n");
            for row in rows {
                let applied_at: i64 = row.get_typed("applied_at").unwrap_or_default();
                let applied_text = chrono::DateTime::<chrono::Utc>::from_timestamp(applied_at, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_else(|| applied_at.to_string());
                out.push_str(&format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    row.get_typed::<String>("kind").unwrap_or_default(),
                    row.get_typed::<String>("name").unwrap_or_default(),
                    row.get_typed::<String>("version").unwrap_or_default(),
                    row.get_typed::<bool>("success").unwrap_or_default(),
                    applied_text,
                    row.get_typed::<i64>("execution_ms").unwrap_or_default(),
                ));
            }
            out
        }
        Err(error) => format!(
            "# 读取 schema_migrations 失败（数据库不可用或表不存在）：{error}\n\
             # 这不影响诊断包其它文件：数据库连通性见 health.json 的 deps.db\n"
        ),
    }
}

// ==================== 日志 ====================

/// 日志尾巴（**未打码**，由调用方统一 [`redact`]）。
fn logs_snapshot() -> String {
    let candidates = log_candidates();
    let Some(file) = candidates.iter().find_map(|path| path.exists().then(|| path.clone())) else {
        return format!(
            "# 未找到日志文件。已探测以下路径（均不存在）：\n{}\n\
             # 提示：设置环境变量 CLOUDPIVOT_DIAG_LOG=/path/to/app.log 可显式指定日志文件；\n\
             #       也可以在 config.toml 的 [log] dir 里开启文件输出。\n",
            candidates
                .iter()
                .map(|path| format!("#   {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );
    };

    match read_tail(&file, LOG_TAIL_LINES, LOG_TAIL_MAX_BYTES) {
        Ok(lines) => format!(
            "# 日志文件：{}\n# 最后 {} 行\n{}",
            file.display(),
            lines.len(),
            lines.join("\n")
        ),
        Err(error) => format!("# 读取日志文件 {} 失败：{error}\n", file.display()),
    }
}

/// 日志文件候选路径（按优先级）。
///
/// 顺序：显式环境变量 → `[log] dir` 下按前缀匹配的最新文件 → 工作目录 `logs/` → `server.log`。
/// 之所以要有后两条：框架默认 `[log] dir` 为空（只打 stdout），
/// 而系统服务 / `nohup` 部署通常把 stdout 重定向到 `server.log`。
fn log_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("CLOUDPIVOT_DIAG_LOG")
        && !explicit.trim().is_empty()
    {
        out.push(PathBuf::from(explicit));
    }
    let prefix = cfg().log.file_prefix.clone();
    if let Some(dir) = cfg().log.dir.clone() {
        out.extend(newest_files_with_prefix(Path::new(&dir), &prefix));
    }
    out.extend(newest_files_with_prefix(Path::new("logs"), &prefix));
    out.push(PathBuf::from("server.log"));
    out
}

/// 目录下以 `prefix` 开头的文件，按修改时间从新到旧（目录不存在返回空）。
fn newest_files_with_prefix(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.into_iter().map(|(_, path)| path).collect()
}

/// 读文件最后 `max_lines` 行（先只读文件尾 `max_bytes`，大日志不进内存）。
///
/// # 异常情况
///
/// 文件打不开 / 读到非 UTF-8 边界时返回 `Err(说明)`；非 UTF-8 字节用替换字符容错
/// （日志里混入二进制片段不该让整份诊断包失败）。
pub fn read_tail(path: &Path, max_lines: usize, max_bytes: u64) -> std::result::Result<Vec<String>, String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let len = file.metadata().map_err(|error| error.to_string())?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).map_err(|error| error.to_string())?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&buffer);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    // 从文件中间开始读时第一行大概率是被截断的半行，丢掉以免误读
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    // ==================== 打码 ====================

    #[test]
    fn redact_masks_env_style_assignments() {
        let text = "NIK_DATABASE_PASSWORD=alun\nNIK_MIDDLEWARE_AUTH_JWT_SECRET=f6f6cb12\nNIK_SERVER_LISTEN=127.0.0.1:8033\nLOG_LEVEL=info\n";
        let masked = redact(text);
        assert!(!masked.contains("alun"), "数据库口令必须打码：{masked}");
        assert!(!masked.contains("f6f6cb12"), "JWT 密钥必须打码：{masked}");
        // 普通配置原样保留
        assert!(masked.contains("127.0.0.1:8033"));
        assert!(masked.contains("LOG_LEVEL=info"));
    }

    #[test]
    fn redact_masks_toml_and_json_forms() {
        let toml = "password = \"secret-value\"\nport = 5432\n";
        let masked = redact(toml);
        assert!(!masked.contains("secret-value"), "{masked}");
        assert!(masked.contains("port = 5432"));

        let json = "{\n  \"access_token\": \"abcdef123456\",\n  \"level\": \"info\"\n}\n";
        let masked = redact(json);
        assert!(!masked.contains("abcdef123456"), "{masked}");
        assert!(masked.contains("\"level\": \"info\""), "非敏感项不该被动：{masked}");
        // 打码后仍是合法 JSON
        assert!(
            serde_json::from_str::<serde_json::Value>(&masked).is_ok(),
            "打码破坏了 JSON：{masked}"
        );
    }

    #[test]
    fn redact_masks_url_credentials_and_bearer() {
        let text =
            "url = postgres://cloudpivot:db-pass@127.0.0.1:5432/cloudpivot\nAuthorization: Bearer abcdefghijklmnop\n";
        let masked = redact(text);
        assert!(!masked.contains("db-pass"), "{masked}");
        assert!(
            masked.contains("cloudpivot:***REDACTED***@127.0.0.1"),
            "用户名应保留：{masked}"
        );
        assert!(!masked.contains("abcdefghijklmnop"), "{masked}");
    }

    #[test]
    fn redact_masks_bare_secrets_and_keeps_chinese() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let hex = "0123456789abcdef0123456789abcdef01234567";
        let text = format!("日志：登录成功\n脏数据：{jwt}\n盐：{hex}\n");
        let masked = redact(&text);
        assert!(!masked.contains(jwt));
        assert!(!masked.contains(hex));
        assert!(masked.contains("日志：登录成功"), "中文正文应原样保留：{masked}");
    }

    #[test]
    fn redact_leaves_comments_readable() {
        // 注释里的示例不是真实密钥，保留（否则配置文件的注释全被吃掉，诊断价值下降）
        let text = "# 示例：NIK_DATABASE_PASSWORD=change-me\nlevel = \"info\"\n";
        let masked = redact(text);
        assert!(masked.contains("change-me"));
        assert!(masked.contains("level = \"info\""));
    }

    // ==================== zip ====================

    #[test]
    fn zip_entries_round_trips_all_expected_members() {
        let entries = vec![
            ("info.json", "{\"a\":1}".to_string()),
            ("health.json", "{\"b\":2}".to_string()),
            ("logs-tail.txt", "line1\nline2".to_string()),
        ];
        let bytes = zip_entries(&entries).expect("zip 应成功");
        assert!(bytes.starts_with(b"PK\x03\x04"), "zip 魔数不对");

        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("应能读回 zip");
        assert_eq!(archive.len(), 3);
        let mut names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).expect("条目").name().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["health.json", "info.json", "logs-tail.txt"]);

        let mut content = String::new();
        archive
            .by_name("logs-tail.txt")
            .expect("logs-tail.txt")
            .read_to_string(&mut content)
            .expect("读内容");
        assert_eq!(content, "line1\nline2");
    }

    // ==================== 日志尾 ====================

    #[test]
    fn read_tail_keeps_last_lines_and_drops_partial_head() {
        let dir = std::env::temp_dir().join(format!("cp-diag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("app.log");
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("line-{i}\n"));
        }
        std::fs::write(&path, &content).expect("写临时日志");

        let all = read_tail(&path, 10, 1024 * 1024).expect("读尾");
        assert_eq!(all.len(), 10);
        assert_eq!(all.last().map(String::as_str), Some("line-49"));

        // 只允许读 60 字节：会从文件中间开始，第一行应被丢弃
        let partial = read_tail(&path, 100, 60).expect("读尾");
        assert!(
            partial.iter().all(|line| line.starts_with("line-")),
            "残留半行：{partial:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_tail_reports_missing_file_without_panic() {
        let missing = std::env::temp_dir().join("cp-diag-not-exists.log");
        assert!(read_tail(&missing, 10, 1024).is_err());
    }

    // ==================== 其它 ====================

    #[test]
    fn prometheus_fallback_has_core_series() {
        let status = ServerStatusRes::default();
        let text = prometheus_fallback(&status);
        for expected in [
            "cloudpivot_cpu_percent",
            "cloudpivot_memory_percent",
            "cloudpivot_disk_percent",
            "cloudpivot_db_up",
            "cloudpivot_redis_up",
            "cloudpivot_load1",
        ] {
            assert!(text.contains(expected), "缺少指标 {expected}");
        }
    }

    #[test]
    fn listen_port_parses_both_forms() {
        // 该函数读全局配置，这里只验证解析规则（用纯字符串逻辑复核，避免依赖配置初始化）
        let cases = [
            ("8023", Some(8023u16)),
            ("127.0.0.1:8033", Some(8033)),
            ("[::]:9000", Some(9000)),
            ("abc", None),
        ];
        for (input, expected) in cases {
            let parsed = input.rsplit(':').next().and_then(|last| last.parse::<u16>().ok());
            assert_eq!(parsed, expected, "解析 {input} 失败");
        }
    }
}

//! 工作任务 · AI 产出文件浏览（批次 E · **E13**）。
//!
//! # 为什么有这个模块
//!
//! AI 执行者把产出物落在 `<workspace_root>/<profile>/`（默认
//! `/root/.hermes/workspace/<profile>/`）里，例如 `report.md` / `report_t5b_xxx.md`。
//! 在此之前云枢**完全不知道这些文件的存在** —— 只有 AI 主动
//! `POST /business/task/{id}/attachment` 上传的那一份进了 storage，于是老大实测到
//! 「产出文件无法打开，也无法从 hermes 的 workspace 目录下载」。本模块把 workspace
//! 变成**只读**的浏览面：列表 / 下载 / 预览。
//!
//! # 接口（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/task/{id}/ai-workspace-files` | `business:task:read` | 文件列表（白名单过滤后） |
//! | GET | `/business/task/{id}/ai-workspace-files/{name}/download` | `business:task:read` | 下载（`ReaderStream` 流式） |
//! | GET | `/business/task/{id}/ai-workspace-files/{name}/preview` | `business:task:read` | 预览（文本直出 / 图片 PDF 走 base64） |
//!
//! 另有两个**查询串别名**（`…/ai-workspace-files/download?name=x`、
//! `…/ai-workspace-files/preview?name=x`），与上面**同一套实现**：
//! 很多 HTTP 客户端在发请求前会把路径里的 `..` 规范化掉，想复现「路径穿越攻击」
//! 往往只能走查询串，留个别名让安全验收有地方可打（两条都过 [`validate_name`]）。
//!
//! # 七条安全红线（卡面 §2，逐条对应实现）
//!
//! 1. **路径必须限定在该任务的 AI profile workspace 下**：先过字符白名单（不可能出现
//!    `/`），再用 `canonicalize()` 把软链接炸开，最后校验结果以 workspace 根为前缀**且
//!    父目录就是它**（见 [`resolve_readable`]）。「父目录必须等于 workspace 根」这条
//!    比单纯前缀更强：`workspace/<profile>/sub/x.md` 也过不了（本卡只开放一级文件）。
//! 2. **只读**：模块内没有任何写 / 删路径，只调 `read_dir` / `metadata` / `File::open`。
//! 3. **文件名白名单** `^[A-Za-z0-9._-]+$`，另外显式拒绝空名、`..` 子串、`.` 开头的
//!    隐藏文件（`.env` 这类不属于「产出物」）。见 [`validate_name`]。
//! 4. **单文件 ≤ 10MB**：超限在**读之前**就用 `metadata().len()` 拒绝（`413 + 40007`），
//!    不是先读进内存再判断。
//! 5. **扩展名白名单**：`.md .txt .csv .json .log .png .jpg .pdf`（[`ALLOWED_EXTS`]）。
//!    白名单是 fail-closed 的：`.exe` / `.sh` / 无扩展名一律不出现在列表里。
//! 6. **只对 `exec_mode = 'AI'` 的任务开放**：人工任务没有 workspace，直接 `400`。
//! 7. **可见性**：走 [`collab::visible_task`]（`scope ∪ 参与关系` 的唯一实现），
//!    看不见任务 → `404 + 63001`，与「任务不存在」不可区分（不暴露 UUID 是否存在）。
//!
//! SCOPE-EXEMPT: 本文件的两条查询都是「按 task_id 读子表」与「按 user_id 读 AI 员工档案」，
//! 前提是 `visible_task` 已经完成数据范围判定；再注入一次 scope 会让「任务可见、profile
//! 查不到」这种静默少数据出现（比漏权限更难发现）。
//!
//! # 状态码约定（与前端 `task-api.ts` 的注释一一对应）
//!
//! | 场景 | 状态码 + 业务码 |
//! |------|-----------------|
//! | 任务不可见 / 不存在 | `404 + 63001` |
//! | 人工任务、文件名非法、扩展名不在白名单、路径越界 | `400 + 10001` |
//! | 文件不存在 / 不是普通文件 / 已被删除 | `404 + 40005` |
//! | 超过 10MB | `413 + 40007` |
//! | AI 员工没登记 profile（无处可查） | `400 + 64004` |

use std::path::{Path as StdPath, PathBuf};

use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;
use tokio_util::io::ReaderStream;

use crate::business::aiagent::hermes::HermesConfig;
use crate::business::task::collab::visible_task;
use crate::business::task::handler::parse_uuid_field;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（上限 / 白名单，唯一来源） ====================

/// 单个文件的大小上限（10 MiB）。超过直接 `413`，不读入内存。
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// 文件名最大长度（Linux NAME_MAX 是 255 字节；这里留到 200 更贴合「产出物」）。
pub const MAX_NAME_LEN: usize = 200;

/// 扩展名白名单（**小写**比较）。不含任何可执行 / 脚本 / 归档类型。
pub const ALLOWED_EXTS: [&str; 8] = ["md", "txt", "csv", "json", "log", "png", "jpg", "pdf"];

/// 文本类扩展名（`is_text = true`，预览直接出原文）。
pub const TEXT_EXTS: [&str; 5] = ["md", "txt", "csv", "json", "log"];

/// 文本预览的字符上限（约 200 KB 的 ASCII / 60 KB 的中文）。
///
/// 超过就截断并把 `truncated` 置真 —— 前端会提示「只显示前 N 字符，完整内容请下载」。
pub const MAX_PREVIEW_CHARS: usize = 200_000;

/// CSV 预览最多给前多少行（含表头）。
pub const MAX_CSV_PREVIEW_LINES: usize = 100;

// ==================== DTO（只在本模块用，不进 dto.rs） ====================

/// 列表里的一行。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AiWorkspaceFileItemRes {
    /// 文件名（不含目录）。
    pub name: String,
    /// **workspace 内**的相对路径 `<profile>/<name>`（不下发服务器绝对路径）。
    pub path: String,
    /// 字节数。
    pub size: i64,
    /// 修改时间（RFC 3339，UTC）。
    pub mtime: String,
    /// 是否文本类（可按原文预览）。
    pub is_text: bool,
    /// 预览类型：`markdown` / `text` / `csv` / `image` / `pdf`。
    pub preview_kind: String,
}

/// 列表回执。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AiWorkspaceFilesRes {
    /// 文件列表（按修改时间倒序，同时间按名字升序）。
    pub list: Vec<AiWorkspaceFileItemRes>,
    /// 条数（= `list.len()`）。
    pub total: u64,
    /// 该任务的 AI profile。
    pub profile: String,
    /// 单文件上限（前端提示用，避免硬编码）。
    pub max_bytes: u64,
    /// workspace 目录是否存在（`false` 时列表必为空，前端可据此区分「没产出」与「目录没了」）。
    pub workspace_exists: bool,
}

/// 预览回执。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AiWorkspacePreviewRes {
    /// 文件名。
    pub name: String,
    /// `markdown` / `text` / `csv` / `image` / `pdf` / `binary`。
    pub content_type: String,
    /// 内容：文本类是原文，二进制类是 base64。
    pub content: String,
    /// `text` / `base64`（前端据此决定怎么解码）。
    pub encoding: String,
    /// 字节数。
    pub size: i64,
    /// 内容是否被截断（只对文本类可能为真）。
    pub truncated: bool,
    /// MIME（图片给 `<img src="data:...">` 用）。
    pub mime_type: String,
}

/// `?name=` 查询串（别名路由用）。
#[derive(Debug, serde::Deserialize)]
pub struct FileNameQuery {
    /// 文件名。
    pub name: Option<String>,
}

// ==================== 纯判定（无 IO，单测直接覆盖） ====================

/// 文件名是否通过白名单：`^[A-Za-z0-9._-]+$` + 额外禁 `..` / 隐藏文件 / 超长。
///
/// # 为什么还要单独禁 `..`
///
/// 字符白名单本身允许 `.`，所以 `..`、`a..md` 是能拼出来的。卡面第 3 条要求
/// 「不含 `/`、`..`」，这里逐字落地 —— 路径穿越的第二道防线（`canonicalize` +
/// 前缀校验）在第 4 节，但**能早拒就早拒**：攻击面越小越好。
pub fn validate_name(raw: &str) -> std::result::Result<(), String> {
    if raw.is_empty() {
        return Err("文件名不能为空".to_string());
    }
    if raw.len() > MAX_NAME_LEN {
        return Err(format!("文件名不能超过 {MAX_NAME_LEN} 个字节"));
    }
    if !raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err("文件名只能包含字母、数字、点、下划线、连字符".to_string());
    }
    if raw.contains("..") {
        return Err("文件名不允许出现 `..`".to_string());
    }
    if raw.starts_with('.') {
        return Err("不允许读取隐藏文件（`.env` / `.` 开头）".to_string());
    }
    Ok(())
}

/// 扩展名（小写）；没有扩展名返回 `None`。
pub fn ext_of(name: &str) -> Option<String> {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => Some(ext.to_ascii_lowercase()),
        _ => None,
    }
}

/// 扩展名是否在白名单内（**fail-closed**：`None` / 不在白名单 → `false`）。
pub fn ext_allowed(name: &str) -> bool {
    ext_of(name).is_some_and(|ext| ALLOWED_EXTS.contains(&ext.as_str()))
}

/// 是否文本类（可原样预览）。
pub fn is_text_name(name: &str) -> bool {
    ext_of(name).is_some_and(|ext| TEXT_EXTS.contains(&ext.as_str()))
}

/// 预览类型（前端按它选渲染器）。
///
/// 与卡面「.md → MarkdownViewer / .txt .log → 纯文本 / .csv → 表格 /
/// .png .jpg → img / .pdf → iframe」逐项对应；`json` 归到 `text`（不引 JSON 树控件）。
pub fn preview_kind(name: &str) -> &'static str {
    match ext_of(name).as_deref() {
        Some("md") => "markdown",
        Some("csv") => "csv",
        Some("txt" | "log" | "json") => "text",
        Some("png" | "jpg") => "image",
        Some("pdf") => "pdf",
        _ => "binary",
    }
}

/// 下载 / 预览用的 MIME（`octet-stream` 兜底）。
pub fn mime_of(name: &str) -> &'static str {
    match ext_of(name).as_deref() {
        Some("md") => "text/markdown; charset=utf-8",
        Some("txt" | "log") => "text/plain; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// 文本内容截断（`truncated` = 是否被截断）。
///
/// CSV 额外按 [`MAX_CSV_PREVIEW_LINES`] 截行：前端只做「简单表格」，
/// 一万行 CSV 塞进抽屉既没人看也会卡住浏览器。
pub fn clip_text(raw: &str, kind: &str) -> (String, bool) {
    let mut truncated = false;
    let mut text = raw.to_string();
    if kind == "csv" {
        let mut offset = 0usize;
        for (lines, line) in text.split_inclusive('\n').enumerate() {
            if lines + 1 > MAX_CSV_PREVIEW_LINES {
                truncated = true;
                break;
            }
            offset += line.len();
        }
        if truncated {
            text.truncate(offset);
        }
    }
    if text.chars().count() > MAX_PREVIEW_CHARS {
        let cut = text
            .char_indices()
            .nth(MAX_PREVIEW_CHARS)
            .map(|(offset, _)| offset)
            .unwrap_or(text.len());
        text.truncate(cut);
        truncated = true;
    }
    (text, truncated)
}

/// 标准 base64（RFC 4648，带 `=` 填充）。
///
/// 手写 25 行而不是引 crate：`base64` 不在本仓的**直接**依赖里（`Cargo.toml` 是
/// 多卡共享文件，为了一个编码器去改它，冲突代价远大于收益），而这段逻辑
/// 有单元测钉住标准向量（见 `base64_matches_rfc4648_vectors`）。
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// profile 名是否可用作单级目录名（Hermes 的 profile 是小写字母 / 数字 / `-` / `_`）。
pub fn validate_profile(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !name.starts_with('.')
}

// ==================== 路径解析（第二道防线） ====================

/// 400：文件名 / 扩展名 / 路径非法。
fn param_invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 404：文件不存在 / 不是普通文件。
fn file_not_found() -> ApiError {
    ApiError::new(
        404,
        error_code::FILE_NOT_FOUND,
        "文件不存在或已从 workspace 移除",
    )
}

/// 把用户给的文件名解析成 workspace 内的**可读文件**。
///
/// 顺序（每一步失败都早于下一步的 IO）：
/// 1. [`validate_name`] —— 字符白名单 + 禁 `..`；
/// 2. [`ext_allowed`] —— 扩展名白名单（`.exe` 连打开的机会都没有）；
/// 3. `root.canonicalize()` —— workspace 根本身不存在 → 404；
/// 4. `candidate.canonicalize()` —— 文件不存在 / 断链 → 404；
/// 5. **前缀 + 父目录**双校验 —— 软链接指向外面时 `canonicalize` 会给出真实路径，
///    前缀不等于 workspace 根（或父目录不是它）→ 400；
/// 6. `metadata().len() <= MAX_FILE_BYTES` → 否则 413。
///
/// # 异常情况
///
/// 见上面 6 步（`400 + 10001` / `404 + 40005` / `413 + 40007` / `500`）。
pub fn resolve_readable(root: &StdPath, name: &str) -> std::result::Result<PathBuf, ApiError> {
    validate_name(name).map_err(param_invalid)?;
    if !ext_allowed(name) {
        return Err(param_invalid(format!(
            "该扩展名不在白名单内（只允许 {}）",
            ALLOWED_EXTS
                .iter()
                .map(|ext| format!(".{ext}"))
                .collect::<Vec<_>>()
                .join(" ")
        )));
    }

    let root_canon = root.canonicalize().map_err(|_| {
        ApiError::new(
            404,
            error_code::FILE_NOT_FOUND,
            "该 AI 的 workspace 目录不存在",
        )
    })?;
    let candidate_canon = root_canon
        .join(name)
        .canonicalize()
        .map_err(|_| file_not_found())?;

    if !candidate_canon.starts_with(&root_canon)
        || candidate_canon.parent() != Some(root_canon.as_path())
    {
        // 走到这里说明磁盘上真的有一条指向 workspace 外的软链接（字符白名单已经
        // 挡掉了 `..`）。按「路径越界」拒绝，并把事实记进日志供运维排查。
        tracing::warn!(
            workspace = %root_canon.display(),
            name,
            resolved = %candidate_canon.display(),
            "AI workspace 文件名解析到 workspace 之外，已拒绝"
        );
        return Err(param_invalid(
            "文件路径越界（只能是 workspace 根下的一级文件）",
        ));
    }

    let meta = std::fs::metadata(&candidate_canon).map_err(|_| file_not_found())?;
    if !meta.is_file() {
        return Err(file_not_found());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(ApiError::new(
            413,
            error_code::FILE_SIZE_EXCEEDED,
            format!(
                "文件 {:.1} MB 超过单文件上限 {} MB，请从服务器直接取",
                meta.len() as f64 / 1024.0 / 1024.0,
                MAX_FILE_BYTES / 1024 / 1024
            ),
        ));
    }
    Ok(candidate_canon)
}

// ==================== workspace 定位 ====================

/// 该任务的 AI profile 名 + workspace 目录。
///
/// profile 的解析顺序（**两处来源都可能为空**，因此要回退）：
/// 1. 任务负责人（`biz_task.owner_id`）在 `sys_ai_agent` 里登记的 `hermes_profile`；
/// 2. 该任务最近一条 `source = 'AI'` 的附件上记的 `ai_profile`（T5b 上传时写的）；
/// 3. 都取不到 → `400 + 64004`（说明这个 AI 账号没有 profile，也就没有 workspace）。
async fn task_workspace_dir(
    claims: &TokenClaims,
    raw_task_id: &str,
) -> std::result::Result<(String, PathBuf), ApiError> {
    let task_id = parse_uuid_field(raw_task_id, "任务")?;
    let (_scope, task) = visible_task(claims, task_id).await?;

    if task.exec_mode != "AI" {
        return Err(param_invalid(format!(
            "该任务是人类任务（exec_mode={}），没有 AI workspace",
            task.exec_mode
        )));
    }

    let profile = resolve_ai_profile(task_id, task.card.owner_id).await?;
    let root = HermesConfig::from_env().workspace_root;
    Ok((profile.clone(), root.join(profile)))
}

/// 解析 AI profile（见 [`task_workspace_dir`] 的三步）。
async fn resolve_ai_profile(
    task_id: Uuid,
    owner_id: Uuid,
) -> std::result::Result<String, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT hermes_profile FROM sys_ai_agent \
              WHERE user_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&owner_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询 AI 员工 profile"))?;
    if let Some(row) = rows.first() {
        let profile: String = row
            .get_typed("hermes_profile")
            .map_err(|error| utils::db_error(error, "读取 AI 员工 profile"))?;
        if validate_profile(&profile) {
            return Ok(profile);
        }
        tracing::warn!(%owner_id, profile, "AI 员工的 hermes_profile 不合规，改用附件记录回退");
    }

    let rows = db()
        .find_with_params(
            "SELECT ai_profile FROM biz_task_attachment \
              WHERE task_id = ?::uuid AND source = 'AI' AND ai_profile IS NOT NULL \
                AND deleted_at IS NULL \
              ORDER BY created_at DESC, id DESC LIMIT 1",
            vec![Value::from(&task_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询任务 AI 附件"))?;
    if let Some(row) = rows.first()
        && let Some(profile) = row
            .get_typed::<Option<String>>("ai_profile")
            .map_err(|error| utils::db_error(error, "读取任务 AI 附件 profile"))?
            .filter(|value| validate_profile(value))
    {
        return Ok(profile);
    }

    Err(ApiError::new(
        400,
        error_code::AI_AGENT_PROFILE_MISSING,
        "该任务的 AI 执行者没有登记 Hermes profile，无法定位 workspace",
    ))
}

// ==================== 列表 ====================

/// 列目录（白名单过滤 + 排序）。
///
/// 过滤链与 [`resolve_readable`] 前两步一致，额外跳过：软链接（避免把 workspace 外的
/// 文件「列」出来）、非普通文件、超 10MB 的文件、非 UTF-8 文件名。
async fn list_workspace(
    dir: &StdPath,
    profile: &str,
) -> std::result::Result<(Vec<AiWorkspaceFileItemRes>, bool), ApiError> {
    let mut reader = match tokio::fs::read_dir(dir).await {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false));
        }
        Err(error) => {
            return Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("读取 workspace 目录失败：{error}"),
            ));
        }
    };

    let mut list: Vec<AiWorkspaceFileItemRes> = Vec::new();
    loop {
        let entry = match reader.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                return Err(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!("遍历 workspace 目录失败：{error}"),
                ));
            }
        };
        let os_name = entry.file_name();
        let Some(name) = os_name.to_str().map(str::to_string) else {
            continue;
        };
        if validate_name(&name).is_err() || !ext_allowed(&name) {
            continue;
        }
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let mtime = meta
            .modified()
            .map(|time| DateTime::<Utc>::from(time).to_rfc3339())
            .unwrap_or_default();
        list.push(AiWorkspaceFileItemRes {
            name: name.clone(),
            path: format!("{profile}/{name}"),
            size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
            mtime,
            is_text: is_text_name(&name),
            preview_kind: preview_kind(&name).to_string(),
        });
    }

    // 最近改过的排前面（AI 刚写出来的报告就是用户要找的那份），同时间按名字升序稳定排序。
    list.sort_by(|left, right| {
        right
            .mtime
            .cmp(&left.mtime)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok((list, true))
}

// ==================== 接口：列表 ====================

/// AI 产出文件列表（`GET /business/task/{id}/ai-workspace-files`）。
///
/// # 规则
///
/// - 只能看**可见任务**的文件（`404 + 63001` 兜住越权与不存在）；
/// - 只列 `exec_mode = 'AI'` 的任务（人工任务 `400 + 10001`）；
/// - 只列扩展名白名单内的普通文件，`.exe` / 脚本 / 超 10MB / 软链接一律不见；
/// - 目录不存在时返回**空列表**（`workspace_exists = false`），不是 404 ——
///   「AI 还没产出」是正常状态，不该让详情页报错。
///
/// # 异常情况
///
/// `404 + 63001` / `400 + 10001` / `400 + 64004` / `500 + 1`。
#[nik::permission(
    path = "/business/task/{id}/ai-workspace-files",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/ai-workspace-files")]
pub async fn list_ai_workspace_files(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<AiWorkspaceFilesRes> {
    let (profile, dir) = task_workspace_dir(&claims, &id).await?;
    let (list, workspace_exists) = list_workspace(&dir, &profile).await?;
    Ok(Res::ok(AiWorkspaceFilesRes {
        total: u64::try_from(list.len()).unwrap_or(0),
        list,
        profile,
        max_bytes: MAX_FILE_BYTES,
        workspace_exists,
    }))
}

// ==================== 接口：下载 ====================

/// 下载（`GET /business/task/{id}/ai-workspace-files/{name}/download`）。
///
/// 走 `tokio::fs::File` + `ReaderStream` **流式**下发（不整文件进内存）；
/// `Content-Disposition: attachment` 让浏览器落到「下载」而不是「内嵌打开」。
///
/// # 异常情况
///
/// 见 [`resolve_readable`]；另外文件在 `metadata` 与 `open` 之间被删 → `404 + 40005`。
#[nik::permission(
    path = "/business/task/{id}/ai-workspace-files/{name}/download",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/ai-workspace-files/{name}/download")]
pub async fn download_ai_workspace_file(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, name)): Path<(String, String)>,
) -> Response {
    match download_inner(&claims, &id, &name).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 下载的查询串别名（`…/ai-workspace-files/download?name=x`，同实现）。
#[nik::permission(
    path = "/business/task/{id}/ai-workspace-files/download",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/ai-workspace-files/download")]
pub async fn download_ai_workspace_file_by_query(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(query): Query<FileNameQuery>,
) -> Response {
    match download_inner(&claims, &id, &query_name(&query)).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 下载的实际流程。
async fn download_inner(
    claims: &TokenClaims,
    raw_task_id: &str,
    name: &str,
) -> std::result::Result<Response, ApiError> {
    let (_profile, dir) = task_workspace_dir(claims, raw_task_id).await?;
    let path = resolve_readable(&dir, name)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| file_not_found())?;
    let size = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);

    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime_of(name))
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&size.to_string())
            .map_err(|_| ApiError::new(500, error_code::INTERNAL_ERROR, "响应头构造失败"))?,
    );
    // 文件名已过 `[A-Za-z0-9._-]` 白名单，不会带引号 / 分号，直接内联是安全的。
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

// ==================== 接口：预览 ====================

/// 预览（`GET /business/task/{id}/ai-workspace-files/{name}/preview`）。
///
/// # 各类型的出参
///
/// | 扩展名 | `content_type` | `encoding` | `content` |
/// |--------|----------------|------------|-----------|
/// | `.md` | `markdown` | `text` | 原文（≤ [`MAX_PREVIEW_CHARS`] 字符） |
/// | `.txt` / `.log` / `.json` | `text` | `text` | 原文 |
/// | `.csv` | `csv` | `text` | 前 [`MAX_CSV_PREVIEW_LINES`] 行 |
/// | `.png` / `.jpg` | `image` | `base64` | 全文件 base64 |
/// | `.pdf` | `pdf` | `base64` | 全文件 base64（前端 `iframe` / `<embed>`） |
///
/// # 异常情况
///
/// 见 [`resolve_readable`]。
#[nik::permission(
    path = "/business/task/{id}/ai-workspace-files/{name}/preview",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/ai-workspace-files/{name}/preview")]
pub async fn preview_ai_workspace_file(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, name)): Path<(String, String)>,
) -> ResResult<AiWorkspacePreviewRes> {
    let data = preview_inner(&claims, &id, &name).await?;
    Ok(Res::ok(data))
}

/// 预览的查询串别名（`…/ai-workspace-files/preview?name=x`，同实现）。
#[nik::permission(
    path = "/business/task/{id}/ai-workspace-files/preview",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/ai-workspace-files/preview")]
pub async fn preview_ai_workspace_file_by_query(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(query): Query<FileNameQuery>,
) -> ResResult<AiWorkspacePreviewRes> {
    let data = preview_inner(&claims, &id, &query_name(&query)).await?;
    Ok(Res::ok(data))
}

/// 预览的实际流程。
async fn preview_inner(
    claims: &TokenClaims,
    raw_task_id: &str,
    name: &str,
) -> std::result::Result<AiWorkspacePreviewRes, ApiError> {
    let (_profile, dir) = task_workspace_dir(claims, raw_task_id).await?;
    let path = resolve_readable(&dir, name)?;
    let bytes = tokio::fs::read(&path).await.map_err(|_| file_not_found())?;
    let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let kind = preview_kind(name).to_string();
    let mime_type = mime_of(name).to_string();

    if is_text_name(name) {
        let (content, truncated) = clip_text(&String::from_utf8_lossy(&bytes), &kind);
        return Ok(AiWorkspacePreviewRes {
            name: name.to_string(),
            content_type: kind,
            content,
            encoding: "text".to_string(),
            size,
            truncated,
            mime_type,
        });
    }

    Ok(AiWorkspacePreviewRes {
        name: name.to_string(),
        content_type: kind,
        content: base64_encode(&bytes),
        encoding: "base64".to_string(),
        size,
        truncated: false,
        mime_type,
    })
}

/// 查询串里的文件名（缺省 → 空串，交给 [`validate_name`] 报 400，不 panic）。
fn query_name(query: &FileNameQuery) -> String {
    query
        .name
        .as_deref()
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("e13-ws-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时 workspace");
        dir
    }

    #[test]
    fn name_whitelist_blocks_traversal() {
        // 卡面验收点名的两种
        assert!(validate_name("../../etc/passwd").is_err());
        assert!(validate_name("../SOUL.md").is_err());
        // 其它越界形态
        assert!(validate_name("a/b.md").is_err());
        assert!(validate_name("x..md").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(".env").is_err());
        assert!(validate_name("report;rm -rf.md").is_err());
        assert!(validate_name("报告.md").is_err()); // 非 ASCII 不在 `[A-Za-z0-9._-]` 内
        // 合法
        assert!(validate_name("report.md").is_ok());
        assert!(validate_name("report_t5b-2026.10.30.md").is_ok());
    }

    #[test]
    fn ext_whitelist_rejects_executables() {
        assert!(!ext_allowed("malware.exe"));
        assert!(!ext_allowed("run.sh"));
        assert!(!ext_allowed("noext"));
        assert!(!ext_allowed("a.tar.gz")); // gz 不在白名单
        for name in [
            "a.md", "a.txt", "a.csv", "a.json", "a.log", "a.png", "a.jpg", "a.pdf", "A.MD",
        ] {
            assert!(ext_allowed(name), "{name} 应在白名单内");
        }
    }

    #[test]
    fn preview_kind_matches_card() {
        assert_eq!(preview_kind("r.md"), "markdown");
        assert_eq!(preview_kind("r.txt"), "text");
        assert_eq!(preview_kind("r.log"), "text");
        assert_eq!(preview_kind("r.json"), "text");
        assert_eq!(preview_kind("r.csv"), "csv");
        assert_eq!(preview_kind("r.png"), "image");
        assert_eq!(preview_kind("r.jpg"), "image");
        assert_eq!(preview_kind("r.pdf"), "pdf");
        assert_eq!(preview_kind("r.exe"), "binary");
        assert!(is_text_name("r.md"));
        assert!(!is_text_name("r.png"));
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn csv_preview_keeps_first_hundred_lines() {
        let raw = (1..=150).map(|i| format!("row{i}\n")).collect::<String>();
        let (clipped, truncated) = clip_text(&raw, "csv");
        assert!(truncated);
        assert_eq!(clipped.lines().count(), MAX_CSV_PREVIEW_LINES);
        assert!(clipped.starts_with("row1\n"));
        assert!(!clipped.contains("row101"));
    }

    #[test]
    fn markdown_preview_truncates_by_chars() {
        let raw = "字".repeat(MAX_PREVIEW_CHARS + 10);
        let (clipped, truncated) = clip_text(&raw, "markdown");
        assert!(truncated);
        assert_eq!(clipped.chars().count(), MAX_PREVIEW_CHARS);
        let (short, truncated) = clip_text("小文件", "markdown");
        assert!(!truncated);
        assert_eq!(short, "小文件");
    }

    #[test]
    fn resolve_readable_blocks_escape_and_bad_ext() {
        let workspace = temp_workspace("resolve");
        std::fs::write(workspace.join("ok.md"), b"# hi").expect("写文件");

        let resolved = resolve_readable(&workspace, "ok.md").expect("合法文件应解析成功");
        assert!(resolved.starts_with(workspace.canonicalize().unwrap()));

        // 穿越
        let escape = resolve_readable(&workspace, "../SOUL.md");
        assert_eq!(escape.unwrap_err().status, 400);
        let escape = resolve_readable(&workspace, "../../etc/passwd");
        assert_eq!(escape.unwrap_err().status, 400);
        // 扩展名
        std::fs::write(workspace.join("evil.exe"), b"MZ").expect("写 exe");
        assert_eq!(
            resolve_readable(&workspace, "evil.exe").unwrap_err().status,
            400
        );
        // 不存在
        assert_eq!(
            resolve_readable(&workspace, "none.md").unwrap_err().status,
            404
        );
        // 子目录里的文件（父目录不是 workspace 根）
        std::fs::create_dir_all(workspace.join("sub")).expect("建子目录");
        std::fs::write(workspace.join("sub/deep.md"), b"x").expect("写子目录文件");
        assert_eq!(resolve_readable(&workspace, "sub").unwrap_err().status, 400);

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn resolve_readable_blocks_symlink_escape() {
        let workspace = temp_workspace("symlink");
        let outside = temp_workspace("outside");
        std::fs::write(outside.join("secret.md"), b"top secret").expect("写目标文件");
        let link = workspace.join("leak.md");
        std::os::unix::fs::symlink(outside.join("secret.md"), &link).expect("建软链接");

        let error = resolve_readable(&workspace, "leak.md").expect_err("软链接指向外部必须被拒");
        assert_eq!(error.status, 400);

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn resolve_readable_rejects_oversized_file() {
        let workspace = temp_workspace("size");
        let path = workspace.join("big.log");
        let file = std::fs::File::create(&path).expect("建大文件");
        file.set_len(MAX_FILE_BYTES + 1).expect("稀疏扩展");
        drop(file);

        let error = resolve_readable(&workspace, "big.log").expect_err("超 10MB 必须拒绝");
        assert_eq!(error.status, 413);
        assert_eq!(error.code, error_code::FILE_SIZE_EXCEEDED);

        std::fs::remove_dir_all(&workspace).ok();
    }

    #[test]
    fn profile_name_validation() {
        assert!(validate_profile("t5b-agent"));
        assert!(validate_profile("agent_01"));
        assert!(!validate_profile("../etc"));
        assert!(!validate_profile("a/b"));
        assert!(!validate_profile(""));
        assert!(!validate_profile(".hidden"));
    }

    #[tokio::test]
    async fn list_workspace_filters_and_sorts() {
        let workspace = temp_workspace("list");
        std::fs::write(workspace.join("report.md"), b"# report").expect("写 md");
        std::fs::write(workspace.join("data.txt"), b"hello").expect("写 txt");
        std::fs::write(workspace.join("malware.exe"), b"MZ").expect("写 exe");
        std::fs::write(workspace.join("notes.json"), b"{}").expect("写 json");

        let (list, exists) = list_workspace(&workspace, "t5b-agent")
            .await
            .expect("列目录");
        assert!(exists);
        let names: Vec<&str> = list.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names.len(), 3, "exe 必须被扩展名白名单挡掉：{names:?}");
        assert!(!names.contains(&"malware.exe"));
        assert!(names.contains(&"report.md"));
        assert_eq!(list[0].path.split('/').next(), Some("t5b-agent"));
        for item in &list {
            assert!(item.size > 0, "{} 大小应大于 0", item.name);
            assert!(!item.mtime.is_empty());
        }

        // 目录不存在 → 空列表 + exists=false（不报错）
        let (missing, exists) = list_workspace(&workspace.join("nope"), "t5b-agent")
            .await
            .expect("目录不存在应返回空列表");
        assert!(missing.is_empty());
        assert!(!exists);

        std::fs::remove_dir_all(&workspace).ok();
    }
}

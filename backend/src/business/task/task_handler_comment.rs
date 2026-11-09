// ============================================================================
// task_handler_comment.rs —— 评论：加/发评论（任务令牌与人类双身份）、@提及与强制通知
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::Json;
use axum::extract::Path;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_handler_common::*;
use crate::business::task::dto::{self, TaskCommentReq, TaskCommentRes, TaskPostCommentReq};
use crate::business::task::model::{self, TaskDetailRow};
use crate::middleware::task_token::AiTaskActor;
use crate::shared::error_code;
use crate::utils;

/// 写一条任务评论（`POST /business/task/{id}/comment`）。
///
/// # 身份
///
/// 人类 JWT 或 AI 任务令牌（两者都能写，作者分别是账号本人 / 任务的 AI 负责人员）。
/// 人类侧要求 `business:task:read` + 任务可见；AI 侧由中间件保证「令牌 ↔ 任务一致」。
///
/// # `@` 提及（T6b 补齐）
///
/// `mentions` 里的每个人都会收到一条**强制**通知（修正 8：被 @ = 强制，不看偏好），
/// 内容含任务标题 + 评论摘要。实现在
/// [`crate::business::task::collab::notify_mentions`]（去重 / 剔除作者 / 剔除不存在账号都在那里）。
/// 通知失败**不回滚评论**：通知是评论写入之后的副作用。
///
/// # 异常情况
///
/// - 401：无身份；403：人类无读权限；404：任务不存在 / 看不见；
/// - 400 + `63004`：正文为空或超 5000 字；`parent_comment_id` 非法。
#[nik::no_auth("/business/task/{id}/comment")]
#[nik::post("/business/task/{id}/comment")]
pub async fn add_comment(
    claims: Option<Extension<AuthClaims>>,
    ai_actor: Option<Extension<AiTaskActor>>,
    Path(id): Path<String>,
    Json(req): Json<TaskCommentReq>,
) -> ResResult<TaskCommentRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let reader = resolve_reader(claims, ai_actor)?;
    let me = reader.principal().await?;
    // 人类侧：看不见就 404（不暴露 ID 是否存在）；AI 侧：中间件已确认任务存在
    let task = model::find_detail(&me, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务"))?
        .ok_or_else(not_found)?;

    // AI 入口沿用「超长截断」：AI 报告体积不可控，截断也比整条丢掉强。
    persist_comment(
        &task,
        reader.user_id(),
        &reader.display_name(),
        &req,
        CommentLimit::Truncate(MAX_COMMENT_CHARS),
    )
    .await
    .map(Res::ok)
}

/// 人类发表评论（`POST /business/task/{id}/comments`）—— T6b 补漏补的入口。
///
/// # 与 `POST /business/task/{id}/comment`（单数）的关系
///
/// 两个 handler **共用同一份写入实现** [`persist_comment`]（红线：不许出现第二份
/// `add_comment`）。单数那条是 T5b 给 AI 任务令牌开的（`no_auth` + 中间件校验
/// 「令牌 ↔ 任务」一致）；这条复数的是 REST 语义的人类入口 —— 与
/// `GET /comments`、`DELETE /comments/{id}` 同前缀。
///
/// # 权限
///
/// 中间件闸 = `business:task:read`（能看见任务就能评论，卡面 §一）。行级口径由
/// [`crate::business::task::collab::visible_task`] 兜住：看不见的任务一律 404，
/// 不暴露任务 ID 是否存在。
///
/// # 参数校验
///
/// - `content`：trim 后 1–5000 字，**超出即 400**（用户写的字不许被静默截断）；
/// - `mentions`：非法 UUID → 400；账号不存在 / 已停用 / 已删除 → **忽略**，
///   回执里逐个列在 `mentions_ignored`；
/// - `parent_comment_id`：必须属于**同一个任务**（跨任务引用一律 400）；
/// - `comment_type`：只允许 `PLAIN`，伪造 `AI_REPORT` / `STATUS_CHANGE` 一律 400。
///
/// # 异常情况
///
/// - 401：没登录（或令牌失效）；403：没有 `business:task:read`；
/// - 404 + `63001`：任务不存在 / 看不见；
/// - 400 + `63004`：正文空 / 超长、上级评论跨任务、`mentions` 非法、伪造类型。
#[nik::permission(
    path = "/business/task/{id}/comments",
    method = "POST",
    permission = "business:task:read"
)]
#[nik::post("/business/task/{id}/comments")]
pub async fn post_comment(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskPostCommentReq>,
) -> ResResult<TaskCommentRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let author = utils::current_user_uuid(&claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))?;
    // 能看见任务就能评论；看不见一律 404。
    let (_scope, task) = crate::business::task::collab::visible_task(&claims, task_id).await?;
    // 红线：用户不许伪造 `comment_type`。
    comment_type_verdict(req.comment_type.as_deref())?;

    let body = TaskCommentReq {
        content: req.content,
        parent_comment_id: req.parent_comment_id,
        mentions: req.mentions,
    };
    persist_comment(
        &task,
        author,
        &display_name(&claims),
        &body,
        CommentLimit::Strict(MAX_USER_COMMENT_CHARS),
    )
    .await
    .map(Res::ok)
}

/// 评论正文上限策略（两个入口口径不同，**写库逻辑只有一份**）。
pub(crate) enum CommentLimit {
    /// 超出即 400（人类入口：用户写的字不许被静默截断）。
    Strict(usize),
    /// 超长截断（AI 入口：回传体积不可控，截断也比整条丢掉强）。
    Truncate(usize),
}

/// 用户提交的 `comment_type` 判定：只允许 `PLAIN`（缺省 / 空串 = `PLAIN`）。
///
/// # 为什么要有这一手
///
/// `biz_task_comment.comment_type` 的值域里除了 `PLAIN`，还有 `AI_REPORT` /
/// `STATUS_CHANGE` / `ASSIGN` / `PROGRESS_AUTO` / `AI_PROGRESS` —— 那些是「这条评论
/// 的来历」（系统写的），不是「谁写的」。用户若能自由指定，就能伪造一条「AI 执行
/// 报告」或「状态变更」，绕过只读保护（`readonly` 类型连作者本人都删不掉）。
pub(super) fn comment_type_verdict(raw: Option<&str>) -> std::result::Result<(), ApiError> {
    match raw.map(str::trim).filter(|kind| !kind.is_empty()) {
        Some(kind) if !kind.eq_ignore_ascii_case(model::COMMENT_TYPE_PLAIN) => Err(ApiError::new(
            400,
            error_code::TASK_FIELD_INVALID,
            format!("用户只能发表 PLAIN 类型的评论，{kind} 只由系统写入"),
        )),
        _ => Ok(()),
    }
}

/// 评论写入的**唯一实现**：人类入口（复数路径）与 AI 入口（单数路径）都走这里。
///
/// 负责：正文校验 → 上级评论同任务校验 → `mentions` 规范化（不存在的账号忽略）
/// → 落库（`comment_type` 一律 `PLAIN`）→ **@ 提及强制通知**（`force = true`，
/// 见 [`crate::business::task::collab::notify_mentions`]）。
///
/// # 异常情况
///
/// - 400 + `63004`：正文空 / 超长、上级评论不是 UUID 或不属于本任务、
///   `mentions` 里有非法 UUID、一次 @ 超过 50 人；
/// - 500：写库失败或校验被 @ 用户失败（`utils::db_error` 带中文上下文）。
pub(super) async fn persist_comment(
    task: &TaskDetailRow,
    author: Uuid,
    author_name: &str,
    req: &TaskCommentReq,
    limit: CommentLimit,
) -> std::result::Result<TaskCommentRes, ApiError> {
    let task_id = task.card.id;
    let content = normalize_comment_content(&req.content, &limit)?;
    // 评论一律 `PLAIN`：`AI_REPORT` 只由轮询回传（Done 那一层）写，
    // `AI_PROGRESS` 只由 `/ai-progress` 写 —— 类型是「这条评论的来历」，不是「谁写的」。
    let comment_type = model::COMMENT_TYPE_PLAIN;
    let parent = match req
        .parent_comment_id
        .as_deref()
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
    {
        Some(raw) => Some(parse_uuid_field(raw, "上级评论")?),
        None => None,
    };
    // 上级评论必须属于**同一个任务**：否则能把 A 任务的评论挂到 B 任务的回复链上，
    // 回复链会指向一条在 B 里根本不存在的记录。
    if let Some(parent_id) = parent {
        let found = crate::business::task::collab::find_comment(task_id, parent_id)
            .await
            .map_err(|error| utils::db_error(error, "查询上级评论"))?;
        if found.is_none() {
            return Err(ApiError::new(
                400,
                error_code::TASK_FIELD_INVALID,
                "上级评论不存在，或不属于当前任务",
            ));
        }
    }

    let mentions = normalize_mentions(&req.mentions)?;
    // 账号不存在 / 已停用 / 已删除 → **忽略**（卡面 §一 方案），回执里列出来。
    let valid = model::valid_users(&mentions)
        .await
        .map_err(|error| utils::db_error(error, "校验被 @ 的用户"))?;
    let mut targets: Vec<Uuid> = Vec::new();
    let mut ignored: Vec<Uuid> = Vec::new();
    for user_id in mentions {
        if valid.contains(&user_id) {
            targets.push(user_id);
        } else {
            ignored.push(user_id);
        }
    }

    let comment_id = model::insert_comment_full(
        db(),
        task_id,
        author,
        &content,
        comment_type,
        model::CommentExtra {
            parent_id: parent,
            mentions: &targets,
        },
        author,
    )
    .await
    .map_err(|error| utils::db_error(error, "写入任务评论"))?;

    // T6b：@ 提及 = 强制通知（修正 8）。评论已落库，通知失败只记 warn。
    if !targets.is_empty() {
        let notified =
            crate::business::task::collab::notify_mentions(task, &content, &targets, author, author_name).await;
        tracing::info!(
            task_id = %task_id,
            comment_id = %comment_id,
            mentioned = targets.len(),
            notified = notified.len(),
            "评论提及通知已处理"
        );
    }

    Ok(TaskCommentRes {
        id: comment_id.to_string(),
        comment_type: comment_type.to_string(),
        user_id: author.to_string(),
        user_name: author_name.to_string(),
        created_at: Utc::now(),
        mentions: targets.iter().map(Uuid::to_string).collect(),
        mentions_ignored: ignored.iter().map(Uuid::to_string).collect(),
    })
}

/// AI 入口的评论正文上限（字符）。与 `biz_task_comment.content` 的 TEXT 列相比留足余量：
/// AI 回传的体积不可控，超长一律截断而不是拒绝（截断也比整条丢掉强）。
pub(crate) const MAX_COMMENT_CHARS: usize = 8_000;

/// 人类入口的评论正文上限（字符，卡面 §一：1–5000）。
///
/// 与 AI 的不同：**超出即 400，不截断** —— 用户是照着字数在写的，静默砍掉后半段
/// 比直接报错更糟（他看不到自己哪句话被吃了）。
pub(crate) const MAX_USER_COMMENT_CHARS: usize = 5_000;

/// 评论正文归一化：折叠首尾空白、拒绝空串，超长按 [`CommentLimit`] 处理。
///
/// - [`CommentLimit::Strict`]：超长 → 400（人类入口）；
/// - [`CommentLimit::Truncate`]：超长 → 截断（AI 入口）。
pub(super) fn normalize_comment_content(raw: &str, limit: &CommentLimit) -> std::result::Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::new(400, error_code::TASK_FIELD_INVALID, "评论正文不能为空"));
    }
    match limit {
        CommentLimit::Truncate(max) => Ok(dto::truncate_chars(trimmed, *max)),
        CommentLimit::Strict(max) => {
            let count = trimmed.chars().count();
            if count > *max {
                return Err(ApiError::new(
                    400,
                    error_code::TASK_FIELD_INVALID,
                    format!("评论正文最多 {max} 字，当前 {count} 字"),
                ));
            }
            Ok(trimmed.to_string())
        }
    }
}

/// 被 @ 的用户列表：逐个校验 UUID、去重、上限 50（防一次请求把通知扇出放大）。
pub(super) fn normalize_mentions(raw: &[String]) -> std::result::Result<Vec<Uuid>, ApiError> {
    let mut list = Vec::with_capacity(raw.len());
    for item in raw {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        let user_id = parse_uuid_field(trimmed, "被 @ 的用户")?;
        if !list.contains(&user_id) {
            list.push(user_id);
        }
    }
    if list.len() > 50 {
        return Err(param_invalid("单条评论最多 @ 50 人".to_string()));
    }
    Ok(list)
}

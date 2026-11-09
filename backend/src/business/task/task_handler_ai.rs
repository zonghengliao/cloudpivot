// ============================================================================
// task_handler_ai.rs —— AI 执行者入口：ai-progress 回写与 dispatch-ai 派活
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::Json;
use axum::extract::Path;
use nik::prelude::*;

use super::task_handler_assign::*;
use super::task_handler_comment::*;
use super::task_handler_common::*;
use crate::business::task::dto::{self, TaskAiProgressReq, TaskAiProgressRes, TaskDispatchReq, TaskDispatchRes};
use crate::business::task::hermes_sync;
use crate::business::task::model::{self};
use crate::middleware::task_token::AiTaskActor;
use crate::shared::error_code;
use crate::utils;

/// AI 进度回写（`PATCH /business/task/{id}/ai-progress`，**只接受任务令牌**）。
///
/// # 规则
///
/// - `percent` 0–100；任务**有子任务**时忽略它（进度由子任务汇总，D4 规则），
///   回执里 `ignored_percent = true`；
/// - `note` 写一条 `AI_PROGRESS` 评论（AI 的执行流水，验收人能看到进度节奏）；
/// - 两者都空 → `400 + 63308`（空请求不该产生一次写事务）。
///
/// # 异常情况
///
/// - 401 + `63304`：走的不是任务令牌（本端点不给人类会话用，人类改进度走
///   `PATCH /business/task/{id}/progress`）；
/// - 400 + `63308`：参数不合法。
#[nik::no_auth("/business/task/{id}/ai-progress")]
#[nik::patch("/business/task/{id}/ai-progress")]
pub async fn ai_progress(
    ai_actor: Option<Extension<AiTaskActor>>,
    Path(id): Path<String>,
    Json(req): Json<TaskAiProgressReq>,
) -> ResResult<TaskAiProgressRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let Some(Extension(actor)) = ai_actor else {
        return Err(ApiError::new(
            403,
            error_code::TASK_AI_ENDPOINT_FORBIDDEN,
            "AI 进度回写只接受任务令牌（人类请用 PATCH /business/task/{id}/progress）",
        ));
    };
    let actor = TaskReader::Ai(Box::new(actor));
    let author = actor.user_id();

    let note = req
        .note
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| dto::truncate_chars(text, MAX_COMMENT_CHARS));
    let percent = match req.percent {
        Some(value) if !(0..=100).contains(&value) => {
            return Err(ApiError::new(
                400,
                error_code::TASK_AI_PROGRESS_INVALID,
                format!("percent 必须在 0–100 之间，收到 {value}"),
            ));
        }
        other => other,
    };
    if percent.is_none() && note.is_none() {
        return Err(ApiError::new(
            400,
            error_code::TASK_AI_PROGRESS_INVALID,
            "percent 与 note 至少要给一个",
        ));
    }

    let has_children = model::has_open_or_any_children(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    let write_percent = percent.filter(|_| !has_children);
    let ignored_percent = percent.is_some() && has_children;

    let content = match (&note, percent) {
        (Some(text), Some(value)) if !has_children => format!("AI 进度 {value}%：{text}"),
        (Some(text), _) if has_children && percent.is_some() => {
            format!("AI 进度由子任务汇总（本次 percent 已忽略）：{text}")
        }
        (Some(text), _) => format!("AI 进度更新：{text}"),
        (None, Some(value)) => format!("AI 进度 {value}%"),
        (None, None) => unreachable!("上面已拦住「两者都空」"),
    };

    let progress_after = db()
        .transaction(|tx| {
            let content = content.clone();
            async move {
                if let Some(value) = write_percent {
                    model::set_progress(&tx, task_id, value as i16, author).await?;
                }
                model::insert_comment(&tx, task_id, author, &content, model::COMMENT_TYPE_AI_PROGRESS, author).await?;
                model::mark_ai_active(&tx, task_id, author).await?;
                // SCOPE-EXEMPT: 事务内回读**刚刚写入的那一行**（task_id 已由任务令牌
                // 绑定 + 令牌校验判定过归属），不是列表 / 导出查询；注入 scope 反而会
                // 在「任务刚被转派」的瞬间少读到一行，把进度静默写回 0。
                let rows = tx
                    .query_sql(
                        "SELECT progress FROM biz_task WHERE id = ?::uuid".to_string(),
                        vec![Value::from(&task_id)],
                    )
                    .await?;
                let progress: i16 = match rows.first() {
                    Some(row) => row.get_typed("progress")?,
                    None => 0,
                };
                Ok(i32::from(progress))
            }
        })
        .await
        .map_err(|error| utils::db_error(error, "回写 AI 进度"))?;

    Ok(Res::ok(TaskAiProgressRes {
        progress: progress_after,
        ignored_percent,
        comment_id: None,
    }))
}

/// 派活给 AI（`POST /business/task/{id}/dispatch-ai`，**人类会话调用**）。
///
/// # 流程（设计文档 §一）
///
/// 1. 权限：与分派同一套矩阵（行级关系 ∪ `business:task:assign`）——「派给 AI」
///    本质就是一次分派，只是执行者换成了机器人；
/// 2. 前置：负责人必须是一个 `user_type='ROBOT'` 且有 `sys_ai_agent` 登记的账号
///    （否则没有 Hermes profile 可指）；
/// 3. 已派活（`hermes_task_id` 非空且任务未终结）→ `409 + 63306`，
///    **不再创建第二个**（Hermes 侧幂等键也会返回同一个 id，但这里更早拦住：
///    重派会覆盖任务令牌，而旧令牌的明文已经写进上一张 kanban body）；
/// 4. 生成令牌 → 拼 body → `hermes kanban create` → 只有成功才回写三件套；
/// 5. 失败 → `502 + 63305`，任务状态**一字不改**，令牌当场删除。
///
/// # 异常情况
///
/// 见上；其余：404 任务不存在、400 负责人不是 AI 账号 / 缺 profile。
#[nik::permission(
    path = "/business/task/{id}/dispatch-ai",
    method = "POST",
    permission = "business:task:read"
)]
#[nik::post("/business/task/{id}/dispatch-ai")]
pub async fn dispatch_ai(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskDispatchReq>,
) -> ResResult<TaskDispatchRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let task = require_task(task_id).await?;

    // ① 权限矩阵与分派一致（行级关系 ∪ 权限码）
    let role = actor_role(me, task.creator_id, task.card.owner_id, task.assigner_id);
    if !may_assign(role, claims.has_permission(dto::PERM_ASSIGN)) {
        return Err(forbidden_assign(is_robot(me).await?));
    }

    // ② 任务必须还在可派活的状态（终态不允许派活）
    if matches!(task.card.status.as_str(), "DONE" | "CANCELLED") {
        return Err(ApiError::new(
            409,
            error_code::TASK_STATUS_TRANSITION_INVALID,
            format!(
                "任务当前是「{}」，不能派活给 AI（请先流转到待开始 / 进行中）",
                task.card.status
            ),
        ));
    }
    // ③ 已派活：拦住第二个 Hermes 任务
    if let Some(hermes_task_id) = task.hermes_task_id.as_deref() {
        let missing = task
            .hermes_status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("MISSING"));
        if !missing {
            return Err(ApiError::new(
                409,
                error_code::TASK_DISPATCH_CONFLICT,
                format!(
                    "任务已派给 AI（Hermes 任务 {hermes_task_id}，看板 {}），不会重复派活；\
                     如需重派请先处理该 Hermes 任务",
                    task.hermes_board.as_deref().unwrap_or("dev")
                ),
            ));
        }
        // 关联任务已丢失：允许重派（幂等键加 `:retry` 后缀，避免撞上那张已删的任务）
        tracing::warn!(
            task_id = %task_id,
            hermes_task_id,
            "关联的 Hermes 任务已丢失，本次按重派处理"
        );
    }

    // ④ 负责人必须是 AI 账号且登记了 Hermes profile
    let owner_id = task.card.owner_id;
    if !is_robot(owner_id).await? {
        return Err(ApiError::new(
            400,
            error_code::TASK_DISPATCH_OWNER_NOT_ROBOT,
            format!(
                "任务的负责人不是 AI 账号（{}），请先把负责人改成一个 AI 员工再派活",
                task.card.owner_name
            ),
        ));
    }
    let agent = crate::business::aiagent::model::find_by_user(&owner_id)
        .await
        .map_err(|error| utils::db_error(error, "查询 AI 员工配置"))?
        .ok_or_else(|| {
            ApiError::new(
                400,
                error_code::AI_AGENT_NOT_FOUND,
                "该账号标记为 AI 用户，但没有 sys_ai_agent 登记（缺少 Hermes profile）".to_string(),
            )
        })?;

    // ⑤ 工作区：显式传优先，否则用 AI profile 的默认 workspace（`dir:<绝对路径>`）
    let workspace = req
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(|| {
            format!(
                "dir:{}",
                crate::business::aiagent::hermes::Hermes::from_env()
                    .workspace_path(&agent.hermes_profile)
                    .display()
            )
        });

    let reason = req.reason.as_deref().map(str::trim).filter(|text| !text.is_empty());
    let operator_name = display_name(&claims);
    let retry = task.hermes_task_id.is_some();
    let outcome = hermes_sync::dispatch(
        &task,
        &agent.hermes_profile,
        Some(&workspace),
        me,
        &operator_name,
        reason,
        retry,
    )
    .await?;

    Ok(Res::ok(TaskDispatchRes {
        task_id: task_id.to_string(),
        hermes_task_id: outcome.hermes_task_id,
        hermes_board: outcome.board,
        hermes_status: outcome.hermes_status,
        exec_mode: "AI".to_string(),
        status: task.card.status.clone(),
        token_expires_at: outcome.expires_at,
    }))
}

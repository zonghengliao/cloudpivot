// ============================================================================
// task_handler_common.rs —— 底座：错误构造 / 入参归一 / 行转换 / 身份读取与权限闸 / 状态机错误映射
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_handler_crud::*;
use crate::business::flow::engine;
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::task::dto::{self, TaskCardRes, TaskSaveRes};
use crate::business::task::model::{self, TaskCardRow, TaskDetailRow};
use crate::business::task::state::{self, TaskStatus};
use crate::middleware::task_token::AiTaskActor;
use crate::shared::error_code;
use crate::utils;

// ==================== 分页 ====================

/// 端点身份的两种来源（人类 JWT / 任务令牌）。
///
/// # 为什么要有这层抽象
///
/// T5b 的 3 个写端点（评论 / 附件 / AI 进度）与 1 个读端点（详情）同时接受两种身份。
/// 若在每个 handler 里写「先看 actor、再看 claims、都没有就 401」，四份重复代码里
/// 只要有一处把「都没有」写成「按匿名放行」，就是一个静默越权。收成一个枚举 +
/// [`resolve_reader`] 之后，「身份从哪来」与「权限怎么判」各只有一处实现。
#[derive(Debug, Clone)]
pub(crate) enum TaskReader {
    /// 人类账号（框架认证层注入的 JWT 声明）。
    Human(Box<TokenClaims>),
    /// AI 任务令牌（[`crate::middleware::task_token::guard`] 注入）。
    Ai(Box<AiTaskActor>),
}

impl TaskReader {
    /// 解析数据范围用的主体。
    ///
    /// - 人类：`business:task:read` 权限码 + 数据范围（可见性照旧）；
    /// - AI：以任务负责人的身份解析范围，**不判权限码**（令牌的边界是「这一个任务」，
    ///   而不是「这个 AI 账号的角色」）。
    pub(crate) async fn principal(&self) -> std::result::Result<ScopedPrincipal, ApiError> {
        match self {
            Self::Human(claims) => {
                ensure_task_read_permission(claims)?;
                principal::resolve_for_claims(claims).await
            }
            Self::Ai(actor) => principal::resolve(&actor.user_id)
                .await
                .map_err(|error| utils::db_error(error, "解析 AI 任务数据范围")),
        }
    }

    /// 操作人（评论 / 附件的作者、审计列）。
    pub(crate) fn user_id(&self) -> Uuid {
        match self {
            Self::Human(claims) => utils::current_user_uuid(claims).unwrap_or(Uuid::nil()),
            Self::Ai(actor) => actor.user_id,
        }
    }

    /// 操作人姓名（评论回执 / 通知显示）。
    pub(crate) fn display_name(&self) -> String {
        match self {
            Self::Human(claims) => display_name(claims),
            Self::Ai(actor) => actor
                .profile
                .clone()
                .map(|profile| format!("AI 员工 {profile}"))
                .unwrap_or_else(|| "AI 员工".to_string()),
        }
    }

    /// 是不是 AI 令牌身份。
    pub(crate) fn is_ai(&self) -> bool {
        matches!(self, Self::Ai(_))
    }
}

/// 从两个可选身份里解析出唯一身份。
///
/// # 异常情况
///
/// 两者都缺 → `401 + AUTH_TOKEN_INVALID`（消息点明「需要登录或任务令牌」）。
pub(crate) fn resolve_reader(
    claims: Option<Extension<AuthClaims>>,
    ai_actor: Option<Extension<AiTaskActor>>,
) -> std::result::Result<TaskReader, ApiError> {
    if let Some(Extension(actor)) = ai_actor {
        // 令牌身份优先：中间件已在令牌路径上摘掉 Authorization 头，理论上不会同时出现
        // 两种身份；万一同现，令牌的边界更窄，按它处理更安全。
        return Ok(TaskReader::Ai(Box::new(actor)));
    }
    let Some(Extension(AuthClaims(claims))) = claims else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "需要登录会话或有效的任务令牌",
        ));
    };
    Ok(TaskReader::Human(Box::new(claims)))
}

/// 人类账号的读权限判定（`business:task:read`）。
///
/// 提成函数是因为详情路径进了 `#[nik::no_auth]` 白名单（AI 令牌要能读），
/// 权限层因此不再覆盖它 —— 这条判定必须由 handler 补上，且**只补人类侧**。
pub(super) fn ensure_task_read_permission(
    claims: &TokenClaims,
) -> std::result::Result<(), ApiError> {
    if claims.has_permission(dto::PERM_TASK_READ) {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::TASK_FORBIDDEN,
        "没有任务读取权限（business:task:read）",
    ))
}

/// 人类账号的建单权限判定（`business:task:create`）。
///
/// 复制 = 建单（D9 硬红线 5），所以复制走这一个码。与 [`ensure_task_read_permission`]
/// 同样提成函数：路由注解已拦一次，这里再判一次，单测才能直接证明「无码 → 403」。
pub(super) fn ensure_task_create_permission(
    claims: &TokenClaims,
) -> std::result::Result<(), ApiError> {
    if claims.has_permission(COPY_PERMISSION) {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::TASK_FORBIDDEN,
        "没有任务创建权限（business:task:create），不能复制任务",
    ))
}

/// 当前用户（令牌 `sub`）。
pub(crate) fn current_user(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))
}

/// 解析 UUID 字段（失败 → 400 + `10001`）。
pub(crate) fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    utils::parse_uuid(raw).ok_or_else(|| param_invalid(format!("{label} ID 不是合法 UUID：{raw}")))
}

/// 400 参数错误。
pub(crate) fn param_invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

/// 任务不存在 / 看不见 → `404 + 63001`。
pub(crate) fn not_found() -> ApiError {
    ApiError::new(404, error_code::TASK_NOT_FOUND, "任务不存在或已删除")
}

/// 父任务校验失败 → `400 + 63005`（原始原因保留在消息里，便于前端提示）。
pub(super) fn parent_invalid(error: DbError) -> ApiError {
    ApiError::new(400, error_code::TASK_PARENT_INVALID, format!("{error}"))
}

/// 取任务（写路径用；不做可见性过滤）。
pub(super) async fn require_task(id: Uuid) -> std::result::Result<TaskDetailRow, ApiError> {
    model::find_raw(id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务"))?
        .ok_or_else(not_found)
}

/// 标题归一化：折叠空白 + 非空 + ≤200 字符。
pub(super) fn normalize_title(raw: &str) -> std::result::Result<String, ApiError> {
    let collapsed = dto::collapse_whitespace(raw);
    if collapsed.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::TASK_TITLE_INVALID,
            "任务标题不能为空",
        ));
    }
    if collapsed.chars().count() > dto::MAX_TITLE_CHARS {
        return Err(ApiError::new(
            400,
            error_code::TASK_TITLE_INVALID,
            format!("任务标题不能超过 {} 个字符", dto::MAX_TITLE_CHARS),
        ));
    }
    Ok(collapsed)
}

/// 描述归一化：空白视为未填；超长截断（不报错，避免用户丢内容）。
pub(super) fn normalize_description(
    raw: Option<&str>,
) -> std::result::Result<Option<String>, ApiError> {
    match raw {
        None => Ok(None),
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            Ok(Some(dto::truncate_chars(
                trimmed,
                dto::MAX_DESCRIPTION_CHARS,
            )))
        }
    }
}

/// 工时校验（≥ 0 且 ≤ 9999.9，与 `NUMERIC(6,1)` 对齐）。
pub(super) fn validate_hours(raw: Option<f64>) -> std::result::Result<Option<f64>, ApiError> {
    match raw {
        None => Ok(None),
        Some(hours) => {
            if !hours.is_finite() || !(0.0..=9_999.9).contains(&hours) {
                return Err(ApiError::new(
                    400,
                    error_code::TASK_FIELD_INVALID,
                    "工时必须是 0 ~ 9999.9 之间的数字".to_string(),
                ));
            }
            Ok(Some((hours * 10.0).round() / 10.0))
        }
    }
}

/// 行 → 卡片出参。
pub(super) fn card_res(row: &TaskCardRow) -> TaskCardRes {
    TaskCardRes {
        id: row.id.to_string(),
        task_no: row.task_no.clone(),
        title: row.title.clone(),
        status: row.status.clone(),
        priority: row.priority.clone(),
        progress: i32::from(row.progress),
        need_review: row.need_review,
        owner_id: row.owner_id.to_string(),
        owner_name: row.owner_name.clone(),
        dept_id: row.dept_id.map(|id| id.to_string()),
        source: row.source.clone(),
        start_date: row.start_date,
        due_date: row.due_date,
        child_count: row.child_count,
        child_done_count: row.child_done_count,
        parent_id: row.parent_id.map(|id| id.to_string()),
        project_id: row.project_id.map(|id| id.to_string()),
        project_code: row.project_code.clone(),
        project_name: row.project_name.clone(),
        doc_type: row.doc_type.clone(),
        doc_id: row.doc_id.map(|id| id.to_string()),
        // P-FLOW-1A：`flow_instance_id` 非空 ⇔ `source='FLOW'`（库侧双向 CHECK 保证），
        // 所以这里只用三列拼段，不再多看 `source`（少一个可能漂移的判断口径）。
        flow: row.flow_instance_id.map(|instance_id| dto::TaskFlowRef {
            instance_id: instance_id.to_string(),
            node_key: row.flow_node_key.clone().unwrap_or_default(),
            round: row.flow_round.unwrap_or(1),
        }),
        created_at: row.created_at,
        updated_at: row.updated_at,
        is_new: row.is_new,
    }
}

/// 详情行 → 写操作回执。
pub(super) fn save_res(row: &TaskDetailRow) -> TaskSaveRes {
    TaskSaveRes {
        id: row.card.id.to_string(),
        task_no: row.card.task_no.clone(),
        status: row.card.status.clone(),
        priority: row.card.priority.clone(),
        progress: i32::from(row.card.progress),
        due_date: row.card.due_date,
        owner_id: row.card.owner_id.to_string(),
    }
}

/// 原因归一化：去首尾空白、空白视为未填、限长 `VARCHAR(500)`。
pub(super) fn normalize_reason(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > dto::MAX_REASON_CHARS {
        return Err(param_invalid(format!(
            "原因不能超过 {} 个字符",
            dto::MAX_REASON_CHARS
        )));
    }
    Ok(Some(trimmed.to_string()))
}

/// 门闸 + 状态机联合判定失败 → HTTP 错误（**门闸单独一档**）。
///
/// - 审批单还在跑 → `409 + 65030`（`TASK_FLOW_GATE_BLOCKED`，文案「任务正在审批中，请等待审批结果」）；
/// - 其余 → 转 [`transition_error`] 的三档（`400 + 63013` / `403 + 63011` / `409 + 63010`）。
///
/// 为什么不把 65030 并进 `transition_error`：两件事的下一步动作完全不同
/// （「去审批单里处理」vs「换个有权的人 / 补个原因」），合成一个码前端就没法给出正确引导。
pub(super) fn transition_check_error(error: state::TransitionCheckError) -> ApiError {
    match error {
        gate @ state::TransitionCheckError::ApprovalPending => {
            ApiError::new(409, error_code::TASK_FLOW_GATE_BLOCKED, gate.message())
        }
        state::TransitionCheckError::Transition(inner) => transition_error(inner),
    }
}

/// 父任务已取消 → 子任务不能推进（T11 规则②：`409 + 63021`）。
///
/// 文案按「用户下一步该干什么」写：不是「刷新」，而是「先恢复父任务」。
pub(super) fn parent_cancelled() -> ApiError {
    ApiError::new(
        409,
        error_code::TASK_PARENT_CANCELLED,
        "父任务已取消，子任务不能继续推进（如需继续请先恢复父任务）",
    )
}

/// 事务内 T11 守卫的哨兵：`DbError` 只有文本通道，业务码靠它回传。
///
/// 为什么不用 `ApiError`：事务闭包的返回类型是 `Db<…>`（`ApiError → DbError`
/// 没有反向 `From`，见 T2 实测），所以守卫在闭包里抛哨兵、在这里翻回 `63021`。
pub(super) const PARENT_CANCELLED_SENTINEL: &str = "TASK_PARENT_CANCELLED";

/// 事务内的「流程冲突」哨兵（P-FLOW-1B）。
///
/// # 为什么不直接把 `ApiError` 塞进 `DbError`
///
/// `DbError → ApiError` 是有损的（`utils::db_error` 会把业务码折成 500 文案），而
/// `FlowTaskOutcome::Conflict` 里的 `65038` / `65020` 必须原样到前端。`ApiError` 又不实现
/// `Clone`、不能进 `DbError` —— 与 1A「发起失败整笔回滚」同款：**哨兵 + `Arc<Mutex>` 槽位**。
pub(super) const FLOW_CONFLICT_SENTINEL: &str = "TASK_FLOW_CONFLICT";

/// 事务错误 → HTTP 错误：T11 守卫的哨兵翻成 `409 + 63021`，流程冲突翻成原样的业务码，
/// 发起审批被拒（[`engine::is_start_rejected`]）翻成槽位里的原始 `ApiError`，其余一律 500。
pub(super) fn change_status_tx_error(
    error: DbError,
    flow_conflict: &std::sync::Mutex<Option<ApiError>>,
    start_rejected: &std::sync::Mutex<Option<ApiError>>,
) -> ApiError {
    let blocked = matches!(&error, DbError::InvalidValue(msg) if msg == PARENT_CANCELLED_SENTINEL);
    if blocked {
        return parent_cancelled();
    }
    let is_flow_conflict =
        matches!(&error, DbError::InvalidValue(msg) if msg == FLOW_CONFLICT_SENTINEL);
    if is_flow_conflict
        && let Some(error) = flow_conflict.lock().ok().and_then(|mut slot| slot.take())
    {
        return error;
    }
    // 「验收即发起审批」被引擎业务拒绝（65018 派不出人 / 65019 自审）：原始码与结构化载荷
    // 只在槽位里（见 `task_bridge::start_in_tx`），事务已按哨兵整笔回滚，任务状态一行没动。
    if engine::is_start_rejected(&error)
        && let Some(error) = start_rejected.lock().ok().and_then(|mut slot| slot.take())
    {
        return error;
    }
    utils::db_error(error, "变更任务状态")
}

/// 状态机判定失败 → HTTP 错误（**三档**，不许统一成 500）。
///
/// - 打回缺原因 → `400 + 63013`（客户端补上就能成功）；
/// - 身份不够 → `403 + 63011`（换个有权限的人来做）；
/// - 其余（跳级 / 终态直取 / 同状态 / 需验收却直接完成）→ `409 + 63010`（状态冲突）。
pub(super) fn transition_error(error: state::TransitionError) -> ApiError {
    match &error {
        state::TransitionError::RejectReasonRequired { .. } => ApiError::new(
            400,
            error_code::TASK_REJECT_REASON_REQUIRED,
            error.message(),
        ),
        state::TransitionError::Forbidden { .. } => {
            ApiError::new(403, error_code::TASK_STATUS_FORBIDDEN, error.message())
        }
        state::TransitionError::SameStatus
        | state::TransitionError::IllegalPair
        | state::TransitionError::ReviewRequired => ApiError::new(
            409,
            error_code::TASK_STATUS_TRANSITION_INVALID,
            error.message(),
        ),
    }
}

/// 操作人显示名（`username` → `sub`）。
///
/// 取令牌里的 `username`：审计表要的是「人看得懂的名字」，`sub` 是兜底
/// （令牌缺 `username` 时至少不写空串）。
pub(crate) fn display_name(claims: &TokenClaims) -> String {
    claims
        .username
        .clone()
        .unwrap_or_else(|| claims.sub.clone())
}

/// 手动进度按状态联动（核心设计 §三）。
///
/// | 状态 | 落库值 |
/// |------|--------|
/// | `DOING` | `max(请求值, 10)`（开工至少 10%） |
/// | `REVIEW` / `DONE` | `100`（待验收 / 已完成一律满进度） |
/// | `TODO` | 请求值（0~100） |
/// | `CANCELLED` | 拒绝（进度已冻结，`409 + 63010`） |
///
/// # 异常情况
///
/// 已取消除 → `409 + 63010`。
pub(super) fn requested_progress(
    status: TaskStatus,
    requested: i32,
) -> std::result::Result<i16, ApiError> {
    let requested = i16::try_from(requested)
        .map_err(|_| param_invalid(format!("进度必须在 0 ~ 100 之间（收到 {requested}）")))?;
    match status {
        TaskStatus::Doing => Ok(state::clamp_progress(requested.max(10))),
        TaskStatus::Review | TaskStatus::Done => Ok(100),
        TaskStatus::Todo => Ok(state::clamp_progress(requested)),
        TaskStatus::Cancelled => Err(ApiError::new(
            409,
            error_code::TASK_STATUS_TRANSITION_INVALID,
            "已取消的任务进度已冻结，不能修改（先恢复为待开始）",
        )),
    }
}

/// 状态字面量 → 中文名（未知状态原样回显，不 panic）。
pub(super) fn status_label(status: &str) -> String {
    TaskStatus::parse(status)
        .map(|parsed| parsed.label().to_string())
        .unwrap_or_else(|| status.to_string())
}

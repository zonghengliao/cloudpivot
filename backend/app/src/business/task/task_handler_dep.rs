// ============================================================================
// task_handler_dep.rs —— 依赖与甘特：前置/后继读取、加/解依赖（环检测）、进 DOING 前置校验、甘特取数
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_handler_assign::*;
use super::task_handler_common::*;
use crate::business::shared::principal::{self};
use crate::business::task::dep;
use crate::business::task::dto::{
    self, GanttBarRes, GanttDepRes, GanttRangeRes, GanttTaskRes, TaskDependenciesRes, TaskDependencyAddReq,
    TaskDependencyAddRes, TaskDependencyItemRes, TaskDependencyRemoveRes, TaskGanttQuery, TaskGanttRes,
};
use crate::business::task::model::{self};
use crate::shared::error_code;
use crate::utils;

/// 依赖管理权的行级矩阵（与分派同一套 `ActorRole`）。
///
/// 口径（卡面「二、接口」的权限码 + 与 T3 一致的落地方式）：
/// 「负责人 / 创建人 / 分派人」天然可管依赖（他们本来就决定这条任务怎么做），
/// 其他人要 `business:task:dependency`（管理员 / 项目协调角色）。
pub(super) fn may_manage_dependencies(role: ActorRole, has_permission: bool) -> bool {
    has_permission || !matches!(role, ActorRole::Outsider)
}

/// 依赖无权的 403（`63002`，与编辑 / 分派同一出口）。
pub(super) fn forbidden_dependency() -> ApiError {
    ApiError::new(
        403,
        error_code::TASK_FORBIDDEN,
        "只有负责人 / 创建人 / 分派人（或持有 business:task:dependency 的角色）可以管理任务依赖",
    )
}

/// 依赖类型不合法 / 自依赖 / 父子互依赖 / 跨项目 的统一 400（`63103`）。
pub(super) fn dependency_invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::TASK_DEPENDENCY_INVALID, message)
}

/// 事务内向 handler 传「环检测拒绝」的哨兵前缀。
///
/// `DbError` 只能携带字符串（`InvalidValue(String)`），而环拒绝要带**渲染好的路径**。
/// 用固定前缀把「业务拒绝（→ 400 + 63101）」与「真正的数据库异常（→ 500）」分开，
/// 避免为了传一个路径把整套错误类型改一遍。
pub(super) const DEP_CYCLE_PREFIX: &str = "TASK_DEP_CYCLE:";

/// 依赖写操作的咨询锁 key（`pg_advisory_xact_lock`）。
///
/// 为什么需要：环检测是「读全量边 → 判断 → 插入」三步。两个并发请求各自读到
/// **不含对方**的边集合时都可能判定「不成环」，先后插入后却把圈闭上了。
/// 事务级咨询锁把这一段串行化 —— 依赖的写入频率远低于读取，串行化没有代价。
pub(super) const DEP_ADVISORY_LOCK: i64 = 7_340_105;

/// 依赖行 → 出参（`show_blocking = true` 只对前置组有意义）。
pub(super) fn dependency_item_res(row: &model::DependencyRow, show_blocking: bool) -> TaskDependencyItemRes {
    TaskDependencyItemRes {
        dep_id: row.peer_id.to_string(),
        task_no: row.task_no.clone(),
        title: row.title.clone(),
        status: row.status.clone(),
        priority: row.priority.clone(),
        progress: i32::from(row.progress),
        owner_id: row.owner_id.to_string(),
        owner_name: row.owner_name.clone(),
        start_date: row.start_date,
        due_date: row.due_date,
        dep_type: row.dep_type.clone(),
        lag_days: row.lag_days,
        blocking: show_blocking && dep::is_blocking_status(&row.status),
        created_at: row.created_at,
    }
}

/// 一次取齐「前置 + 后继 + 未完成前置数」（三个接口回执共用）。
pub(super) async fn dependency_groups(
    task_id: Uuid,
) -> std::result::Result<(Vec<TaskDependencyItemRes>, Vec<TaskDependencyItemRes>, i64), ApiError> {
    let prerequisites = model::list_prerequisites(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询前置任务"))?;
    let successors = model::list_successors(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询后继任务"))?;
    let blocking_count = prerequisites
        .iter()
        .filter(|row| dep::is_blocking_status(&row.status))
        .count() as i64;
    Ok((
        prerequisites.iter().map(|row| dependency_item_res(row, true)).collect(),
        successors.iter().map(|row| dependency_item_res(row, false)).collect(),
        blocking_count,
    ))
}

/// 依赖列表（`GET /business/task/{id}/dependencies`）。
///
/// # 返回
///
/// 两组分开：`prerequisites`（必须先完成）+ `successors`（本任务完成后可做），
/// 外加 `blocking_count`（前端画「被 N 个前置阻塞」，不用自己数）。
///
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 不可见（**不暴露「存在但无权」**）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/dependencies",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/dependencies")]
pub async fn dependencies(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskDependenciesRes> {
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    // 可见性：看不见的任务一律 404（依赖关系会泄露任务的存在与标题）
    model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let (prerequisites, successors, blocking_count) = dependency_groups(task_id).await?;
    Ok(Res::ok(TaskDependenciesRes {
        task_id: task_id.to_string(),
        prerequisites,
        successors,
        blocking_count,
    }))
}

/// 加前置依赖（`POST /business/task/{id}/dependencies`，**只做 FS**）。
///
/// # 五道校验（顺序固定）
///
/// 1. **可见性**：任务本身看不见 → 404；前置任务看不见 / 已删 → 404（不泄露存在性）；
/// 2. **自依赖**：`depends_on_id == task_id` → `400 + 63103`；
/// 3. **父子 / 跨项目**：父子任务之间不许建依赖（父进度由子汇总，加依赖语义混乱）；
///    两边都有 `project_id` 且不同 → 也拒绝（核心设计 §四「跨表校验」）；
/// 4. **幂等**：同一对 `(task, depends_on)` 已存在 → 直接返回 `created: false`（**不报错**，
///    前端重复点击不该弹红条）；
/// 5. **环检测**：事务内读全量边跑 [`dep::find_cycle`]，成环 → `400 + 63101` + **点名路径**。
///
/// # 权限
///
/// 行级关系（负责人 / 创建人 / 分派人）∪ `business:task:dependency`；否 → `403 + 63002`。
///
/// # 异常情况
///
/// - 400 + `10001` / `63103`：uuid 非法 / 自依赖 / 父子 / 跨项目 / lag 越界；
/// - 400 + `63101`：依赖成环（消息里带闭合路径）或依赖链超深；
/// - 403 + `63002`：无权管理依赖；404 + `63001`：任务或前置不存在 / 不可见；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/dependencies",
    method = "POST",
    permission = "business:task:read"
)]
#[nik::post("/business/task/{id}/dependencies")]
pub async fn add_dependency(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskDependencyAddReq>,
) -> ResResult<TaskDependencyAddRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let current = model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;

    // ① 权限矩阵
    let role = actor_role(me, current.creator_id, current.card.owner_id, current.assigner_id);
    if !may_manage_dependencies(role, claims.has_permission(dto::PERM_DEPENDENCY)) {
        return Err(forbidden_dependency());
    }

    let depends_on_id = parse_uuid_field(&req.depends_on_id, "前置任务")?;
    // ② 自依赖
    if depends_on_id == task_id {
        return Err(dependency_invalid(
            "任务不能依赖自己（自依赖会让「开始」永远等不到「完成」）".to_string(),
        ));
    }
    // ③ 前置任务必须可见且未软删
    let peer = model::find_detail(&mine, depends_on_id)
        .await
        .map_err(|error| utils::db_error(error, "查询前置任务"))?
        .ok_or_else(not_found)?;

    // ④ 父子 / 跨项目
    let relation = model::task_relation(task_id, depends_on_id)
        .await
        .map_err(|error| utils::db_error(error, "校验任务关系"))?
        .ok_or_else(not_found)?;
    if relation.parent_a == Some(depends_on_id) || relation.parent_b == Some(task_id) {
        return Err(dependency_invalid(format!(
            "「{}」与「{}」是父子任务，父子之间不能建依赖（父任务进度由子任务汇总，加依赖会语义混乱）",
            current.card.task_no, peer.card.task_no
        )));
    }
    if let (Some(project_a), Some(project_b)) = (relation.project_a, relation.project_b)
        && project_a != project_b
    {
        return Err(dependency_invalid(format!(
            "「{}」与「{}」分属不同项目，不能建依赖（跨项目依赖会让项目进度互相污染）",
            current.card.task_no, peer.card.task_no
        )));
    }

    // ⑤ lag_days
    let lag_days = req.lag_days.unwrap_or(0);
    if !(0..=dto::MAX_LAG_DAYS).contains(&lag_days) {
        return Err(dependency_invalid(format!(
            "滞后天数必须在 0–{} 之间（收到 {lag_days}）",
            dto::MAX_LAG_DAYS
        )));
    }

    // ⑥ 事务：串行化 → 幂等 → 环检测 → 插入
    let outcome = db()
        .transaction(|tx| async move {
            // 事务级咨询锁：**必须用 exec_sql**（`SELECT pg_advisory_xact_lock()` 返回
            // void 列，走 query_sql 会在驱动层 deserialize 失败 —— 实测 500/10004）
            tx.exec_sql(
                "SELECT pg_advisory_xact_lock(?::int8)".to_string(),
                vec![Value::I64(DEP_ADVISORY_LOCK)],
            )
            .await?;
            if let Some(existing) = model::find_dependency(&tx, task_id, depends_on_id).await? {
                return Ok(DepWriteOutcome {
                    created: false,
                    lag_days: existing,
                });
            }
            let edges = model::dependency_edges(&tx).await?;
            match dep::find_cycle(&edges, task_id, depends_on_id) {
                dep::CycleVerdict::Acyclic => {}
                dep::CycleVerdict::Cycle { path } => {
                    let oriented = dep::orient_cycle_from(&path);
                    let labels = model::dependency_labels(&tx, &oriented).await?;
                    return Err(DbError::InvalidValue(format!(
                        "{DEP_CYCLE_PREFIX}{}",
                        dep::render_cycle(&oriented, &labels)
                    )));
                }
                dep::CycleVerdict::TooDeep { depth } => {
                    return Err(DbError::InvalidValue(format!(
                        "{DEP_CYCLE_PREFIX}依赖链已超过 {} 层（本次走到第 {} 层），\
                         拒绝再叠加新依赖；请先梳理现有依赖链",
                        dep::MAX_CYCLE_DEPTH,
                        depth
                    )));
                }
            }
            model::insert_dependency(&tx, task_id, depends_on_id, lag_days, me).await?;
            Ok(DepWriteOutcome {
                created: true,
                lag_days,
            })
        })
        .await;
    let outcome = match outcome {
        Ok(value) => value,
        Err(DbError::InvalidValue(message)) if message.starts_with(DEP_CYCLE_PREFIX) => {
            let detail = message.trim_start_matches(DEP_CYCLE_PREFIX).to_string();
            return Err(ApiError::new(
                400,
                error_code::TASK_DEPENDENCY_CYCLE,
                format!("依赖成环，已拒绝：{detail}（箭头读作「依赖」）"),
            ));
        }
        Err(error) => return Err(utils::db_error(error, "新增任务依赖")),
    };

    let (prerequisites, successors, blocking_count) = dependency_groups(task_id).await?;
    tracing::info!(
        task_id = %task_id,
        depends_on_id = %depends_on_id,
        created = outcome.created,
        operator = %me,
        "任务依赖已保存"
    );
    Ok(Res::ok(TaskDependencyAddRes {
        task_id: task_id.to_string(),
        depends_on_id: depends_on_id.to_string(),
        created: outcome.created,
        lag_days: outcome.lag_days,
        prerequisites,
        successors,
        blocking_count,
    }))
}

/// 依赖写入的结果（幂等命中时 `created = false`）。
pub(super) struct DepWriteOutcome {
    /// 是否真的插入了一行。
    created: bool,
    /// 落库（或已存在）的滞后天数。
    lag_days: i32,
}

/// 解除依赖（`DELETE /business/task/{id}/dependencies/{dep_id}`）。
///
/// # `dep_id` 的含义
///
/// **对端任务 id**（前置组 = 前置 id，后继组 = 后继 id）。删除按「两个任务之间」
/// 双向匹配 —— 前端不需要知道当初是谁指向谁（依赖本来就是一条无向的约束）。
///
/// # 异常情况
///
/// - 403 + `63002`：无权管理依赖；404 + `63001`：任务不可见；
/// - 404 + `63104`：这条依赖不存在（**不静默成功**：否则前端会以为删掉了）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/dependencies/{dep_id}",
    method = "DELETE",
    permission = "business:task:read"
)]
#[nik::delete("/business/task/{id}/dependencies/{dep_id}")]
pub async fn remove_dependency(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, dep_id)): Path<(String, String)>,
) -> ResResult<TaskDependencyRemoveRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let peer_id = parse_uuid_field(&dep_id, "依赖")?;
    let current = model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let role = actor_role(me, current.creator_id, current.card.owner_id, current.assigner_id);
    if !may_manage_dependencies(role, claims.has_permission(dto::PERM_DEPENDENCY)) {
        return Err(forbidden_dependency());
    }

    let removed = db()
        .transaction(|tx| async move { model::delete_dependency(&tx, task_id, peer_id).await })
        .await
        .map_err(|error| utils::db_error(error, "解除任务依赖"))?;
    if removed == 0 {
        return Err(ApiError::new(
            404,
            error_code::TASK_DEPENDENCY_NOT_FOUND,
            "这两个任务之间没有依赖关系（可能已被他人解除，请刷新）",
        ));
    }

    let (prerequisites, successors, blocking_count) = dependency_groups(task_id).await?;
    tracing::info!(task_id = %task_id, peer_id = %peer_id, operator = %me, "任务依赖已解除");
    Ok(Res::ok(TaskDependencyRemoveRes {
        task_id: task_id.to_string(),
        dep_id: peer_id.to_string(),
        removed: true,
        prerequisites,
        successors,
        blocking_count,
    }))
}

/// 甘特图数据（`GET /business/task/gantt`，**只读**）。
///
/// # 查询参数
///
/// `start` / `end`（缺省今天起 30 天，**上限 180 天** → 超了 `400 + 63105`）、
/// `project_id` / `assignee_id` / `scope=my|all`；`board_id` 本版不支持（传了就 400，
/// 见 [`dto::TaskGanttQuery`] 的说明）。
///
/// # 返回
///
/// 时间窗（含 `today_offset`）+ 任务（**只有排得上期的**）+ 依赖边（两端都在任务里）。
/// 条形位置（`bar.offset_days` / `length_days`）由后端算好（[`dep::bar_span`]），
/// 前端只按缩放比例乘列宽 —— 拖拽改期是 T7，本版**只读**。
///
/// # 异常情况
///
/// - 400 + `10001` / `63004`：uuid / scope 取值非法；400 + `63105`：时间窗非法或超 180 天；
/// - 400 + `63103`：传了本版不支持的 `board_id`；
/// - 500：数据库异常。
#[nik::permission(path = "/business/task/gantt", method = "GET", permission = "business:task:read")]
#[nik::get("/business/task/gantt")]
pub async fn gantt(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<TaskGanttQuery>,
) -> ResResult<TaskGanttRes> {
    let mine = principal::resolve_for_claims(&claims).await?;
    if query.board_id.as_deref().is_some_and(|raw| !raw.trim().is_empty()) {
        return Err(dependency_invalid(
            "board_id 维度属于 P1（自定义看板列），本版请用 project_id / assignee_id 或 scope=my".to_string(),
        ));
    }
    let today = model::today_cst();
    let range = dep::resolve_range(query.start, query.end, today)
        .map_err(|error| ApiError::new(400, error_code::TASK_GANTT_RANGE_INVALID, error.message()))?;

    let owner_id = match query.assignee_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "负责人")?),
    };
    let project_id = match query.project_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "项目")?),
    };
    let mine_only = match query.scope.as_deref().map(str::trim) {
        None | Some("") | Some("all") => None,
        Some("my") => Some(mine.user_id()),
        Some(raw) => {
            return Err(param_invalid(format!("scope 取值非法：{raw}（应为 my / all）")));
        }
    };

    let filter = model::GanttFilter {
        owner_id,
        project_id,
        mine: mine_only,
    };
    let gantt = model::gantt_tasks(&mine, &filter, &range)
        .await
        .map_err(|error| utils::db_error(error, "查询甘特任务"))?;
    let ids: Vec<Uuid> = gantt.tasks.iter().map(|row| row.id).collect();
    let edges = model::gantt_edges(&ids)
        .await
        .map_err(|error| utils::db_error(error, "查询甘特依赖"))?;

    let tasks: Vec<GanttTaskRes> = gantt
        .tasks
        .iter()
        .map(|row| GanttTaskRes {
            id: row.id.to_string(),
            task_no: row.task_no.clone(),
            title: row.title.clone(),
            status: row.status.clone(),
            priority: row.priority.clone(),
            progress: i32::from(row.progress),
            owner_id: row.owner_id.to_string(),
            owner_name: row.owner_name.clone(),
            start_date: row.start_date,
            due_date: row.due_date,
            parent_id: row.parent_id.map(|id| id.to_string()),
            is_milestone: false,
            blocked: row.blocked,
            is_new: row.is_new,
            overdue: dep::is_overdue(row.due_date, &row.status, today),
            bar: dep::bar_span(row.start_date, row.due_date, &range).map(|span| GanttBarRes {
                offset_days: span.offset_days,
                length_days: span.length_days,
            }),
        })
        .collect();
    let dependencies: Vec<GanttDepRes> = edges
        .iter()
        .map(|edge| GanttDepRes {
            from: edge.depends_on_id.to_string(),
            to: edge.task_id.to_string(),
        })
        .collect();

    tracing::info!(
        tasks = tasks.len(),
        edges = dependencies.len(),
        start = %range.start,
        end = %range.end,
        truncated = gantt.truncated,
        "甘特图数据已生成"
    );
    Ok(Res::ok(TaskGanttRes {
        range: GanttRangeRes {
            start: range.start,
            end: range.end,
            days: range.days(),
            today,
            today_offset: dep::today_offset(&range, today),
        },
        tasks,
        dependencies,
        truncated: gantt.truncated,
        member_scope_truncated: gantt.member_scope_truncated,
    }))
}

// ==================== T5b：评论 / 附件 / 派活 / AI 进度 ====================

/// 前置依赖校验（**H-DEP**，T4 落地）。
///
/// 核心设计 §二 规则 1：`TODO → DOING` 要求「所有 FS 前置必须 DONE」。
///
/// # 口径
///
/// - 只算**仍然阻塞**的前置：`DONE` 已完成；`CANCELLED` 视为「已解除」（卡面「联动」表）；
/// - 有阻塞前置 → `409 + 63102`，**响应里逐个点名**（`task_no + title + 状态`）——
///   只回「有前置未完成」等于让用户自己去详情页翻依赖（红线）。
///
/// # 异常情况
///
/// - 409 + `63102`：存在未完成前置（附名单）；
/// - 500：数据库异常。
pub(super) async fn check_dependencies_ready(task_id: Uuid) -> std::result::Result<(), ApiError> {
    let blocking = model::blocking_prerequisites(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询前置依赖"))?;
    if blocking.is_empty() {
        return Ok(());
    }
    let names = blocking
        .iter()
        .map(|row| format!("{}「{}」（{}）", row.task_no, row.title, status_label(&row.status)))
        .collect::<Vec<_>>()
        .join("、");
    Err(ApiError::new(
        409,
        error_code::TASK_DEPENDENCY_BLOCKED,
        format!("前置任务未完成：{names}。等它们完成（或取消）后再开始；管理员可强开并留痕（T7）"),
    ))
}

//! 工作任务管理 · 接口层（批次 C+ · T1）。
//!
//! 依据：`docs/批次C+-工作任务管理-核心设计.md` §七（可见性）、§十（两秒建任务）、
//! §十一（页面）、§十三 T1 行（6 个接口）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/task/page` | `business:task:read` | 任务分页（**可见性过滤** + 6 个筛选维度） |
//! | POST | `/business/task` | `business:task:create` | 创建（默认值：负责人=自己 / TODO / P2 / 本周五 / 需验收=关） |
//! | PUT | `/business/task/{id}` | `business:task:update` | 编辑（**仅负责人 / 创建人**） |
//! | DELETE | `/business/task/{id}` | `business:task:delete` | 软删除（负责人 / 创建人 / 超管） |
//! | GET | `/business/task/{id}` | `business:task:read` | 详情（子任务 + 协作人 + 附件） |
//! | GET | `/business/task/my` | `business:task:read` | 我的任务（逾期 / 今日 / 本周 / 以后 / 无期限） |
//!
//! ## T2 追加的 4 个（状态机 + 进度 + 子任务）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | PATCH | `/business/task/{id}/status` | `business:task:status` | 改状态（**白名单流转**，非法一律 4xx） |
//! | PATCH | `/business/task/{id}/progress` | `business:task:progress` | 改进度（有子任务 → **409**） |
//! | POST | `/business/task/{id}/subtask` | `business:task:subtask` | 建子任务（继承父任务的项目 / 优先级） |
//! | GET | `/business/task/{id}/subtasks` | `business:task:read` | 子任务列表（含各自进度 / 状态） |
//!
//! ## T3 追加的 4 个（分派 / 转派 / 协作人）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | PATCH | `/business/task/{id}/assign` | `business:task:read` | 分派 / 转派（行级矩阵 + `business:task:assign`） |
//! | POST | `/business/task/{id}/members` | `business:task:read` | 加协作人（≤50，幂等，强制通知） |
//! | DELETE | `/business/task/{id}/members/{user_id}` | `business:task:read` | 移除协作人（创建人/负责人 400） |
//! | GET | `/business/task/{id}/members` | `business:task:read` | 协作人列表（**带可见性**，看不见 404） |
//!
//! ## T4 追加的 4 个（依赖 / 甘特图）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | GET | `/business/task/{id}/dependencies` | `business:task:read` | 前置 + 后继两组（**带可见性**） |
//! | POST | `/business/task/{id}/dependencies` | `business:task:read` | 加前置（自依赖 / 父子 / 跨项目 / 成环 一律 400） |
//! | DELETE | `/business/task/{id}/dependencies/{dep_id}` | `business:task:read` | 解除依赖（`dep_id` = 对端任务 id，双向匹配） |
//! | GET | `/business/task/gantt` | `business:task:read` | 甘特数据（时间窗 ≤ 180 天 + 任务 + 依赖边） |
//!
//! ### 依赖的三个「必须服务端做」的判断
//!
//! 1. **环检测**：加一条依赖后如果成环，必须 `400 + 63101` 并**点名环上每个任务**
//!    （红线：不许只回「有环」）。实现在事务内做（[`add_dependency`]），
//!    检测与插入之间用 `pg_advisory_xact_lock` 串行化 —— 否则两个并发请求各自
//!    「单独看都不成环」也会一起把圈闭上。
//! 2. **进 DOING 的前置校验**：`TODO → DOING` 时列未完成前置（`409 + 63102`），
//!    前置 `CANCELLED` 不算阻塞（[`crate::business::task::dep::is_blocking_status`]）。
//! 3. **前置完成 / 取消的通知**：`DONE` 强制通知后继负责人「可以开始了」，
//!    `CANCELLED` 尊重偏好提醒「前置取消了，你可能要调整计划」。
//!
//! ### 为什么中间件闸是 `business:task:read` 而不是 `business:task:assign`
//!
//! 分派权的真实口径是「**行级关系**」：创建人 / 当前负责人 / 分派人天然可派。
//! 这三类身份不是权限码能表达的——若把 `business:task:assign` 写进
//! `#[nik::permission]`，创建人（他往往没有这个码）会被中间件挡在门外，
//! 「创建人可以把活派出去」这条最基本的产品语义直接失效。
//!
//! 所以：**中间件只做入口闸**（能看任务的人才谈得上分派），真矩阵在 handler 里
//! （[`actor_role`] + [`may_assign`]）；`business:task:assign` / `business:task:member`
//! 两个权限码是**加项**——给管理员 / 「AI 团队主管」这类需要跨任务派活的角色用。
//! V0109 只把这两个码授给超管，其余角色要什么就勾什么（全员授予等于没有隔离：
//! 「无关用户分派 → 403」这条验收就不成立了）。
//!
//! # 三条贯穿全卡的硬约束
//!
//! 1. **读路径一律带可见性**：`/page`、`/{id}`、`/{id}` 的子任务列表都走
//!    [`crate::business::task::model::page_visible`] / `find_detail` / `list_children`。
//!    看不见的任务返回 **404**（不是 403）—— 403 会告诉调用者「这个 ID 存在」，
//!    等于把任务 ID 变成了可枚举的资源清单。
//! 2. **写路径一律先查再判**：编辑/删除先 `model::find_raw`（不做可见性过滤，但要判断
//!    「我是不是负责人/创建人」），再决定放行 —— 用可见性做写权限会让「协作人能改」。
//! 3. **状态与进度不在本卡**：`status` / `progress` / `done_at` / 取消原因 / 打回原因
//!    都是 T2 状态机的交付物，本卡的 PUT 不接受这些字段（见 `dto::TaskUpdateReq` 文档）。
//!
//! # 权限码 vs 数据范围（最容易被搞混的一处）
//!
//! 权限码（`business:task:*`）决定**能不能进这个功能**；数据范围（`sys_role.data_scope`）
//! 决定**能看到哪些任务**。V0103 把 4 个权限码授给了所有角色，所以「谁都进得来」；
//! 真正的隔离在 [`model::page_visible`] 的可见性谓词里（scope ∪ 参与关系）。

//! ## T13 追加的 1 个（父任务状态巡检 / 兜底对账）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | GET | `/business/task/admin/repair-parent-status` | `business:task:update` | 扫「子任务全终态、父任务非 `REVIEW`」并修（`dry_run` 缺省只报不修；仅超管） |
//!
//! 为什么需要它：审批通过的 `REVIEW → DONE` 由 `flow::task_bridge::settle` 写，
//! 而 settle 跑在审批事务提交之后 ⇒ 级联只能开独立事务，极端失败会留下
//! 「子任务全终态、父任务停在 DOING」。本接口是那条链路的巡检兜底。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::flow::task_bridge;
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::storage;
use crate::business::task::dep;
use crate::business::task::dto::{
    self, GanttBarRes, GanttDepRes, GanttRangeRes, GanttTaskRes, MyCounts, MyTasksRes,
    ParentProgressRes, TaskAiProgressReq, TaskAiProgressRes, TaskAssignReq, TaskAssignRes,
    TaskAttachmentRes, TaskCardRes, TaskCommentReq, TaskCommentRes, TaskCreateReq, TaskDeleteRes,
    TaskDependenciesRes, TaskDependencyAddReq, TaskDependencyAddRes, TaskDependencyItemRes,
    TaskDependencyRemoveRes, TaskDetailRes, TaskDispatchReq, TaskDispatchRes, TaskGanttQuery,
    TaskGanttRes, TaskMemberAddReq, TaskMemberAddRes, TaskMemberRemoveRes, TaskMemberRes,
    TaskMembersRes, TaskPageQuery, TaskPageRes, TaskPostCommentReq, TaskProgressReq,
    TaskProgressRes, TaskSaveRes, TaskStatusChangeReq, TaskStatusRes, TaskSubtaskCreateReq,
    TaskSubtaskListRes, TaskUpdateReq, TaskUploadRes,
};
use crate::business::task::hermes_sync;
use crate::business::task::model::{self, MyBucket, TaskCardRow, TaskDetailRow, TaskFilter};
use crate::business::task::state::{self, TaskStatus};
use crate::middleware::task_token::AiTaskActor;
use crate::shared::ai_actor;
use crate::shared::error_code;
use crate::utils;

// ==================== 分页 ====================

/// 任务分页（**可见性 = 数据范围 ∪ 我参与的任务 id 数组**）。
///
/// # 参数
///
/// - `claims`：当前用户（数据范围从库里现解析，不信令牌快照）；
/// - `query`：状态 / 优先级 / 负责人 / 截止范围 / 关键词 + `root_only` + 分页。
///
/// `root_only=true` 时只返回顶层任务（`parent_id IS NULL`），`total` 也按顶层计数；
/// 前端树形列表用它 + 逐层 `GET /business/task/{id}/subtasks` 懒加载（T8-C）。
///
/// # 返回值
///
/// [`TaskPageRes`]（`list` / `total` / `page` / `page_size` + `member_scope_truncated`），
/// 排序：截止日升序（无期限垫底）→ 创建时间倒序。
/// `member_scope_truncated = true` 表示「我参与」id 超限、本次只按范围过滤（D7 降级）。
///
/// # 异常情况
///
/// - 401：令牌缺用户标识；
/// - 400 + `10001`：`status` / `priority` / `owner_id` 取值非法；
/// - 500：数据库异常 / 数据权限片段形态异常。
#[nik::permission(
    path = "/business/task/page",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/page")]
pub async fn page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<TaskPageQuery>,
) -> ResResult<TaskPageRes> {
    let me = principal::resolve_for_claims(&claims).await?;
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = build_filter(&query)?;
    let result = model::page_visible(&me, &filter, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询任务"))?;
    let list = result.list.iter().map(card_res).collect();
    Ok(Res::ok(TaskPageRes {
        list,
        total: result.total,
        page: result.page,
        page_size: result.size,
        member_scope_truncated: result.member_scope_truncated,
    }))
}

/// 把查询参数归一化成 [`TaskFilter`]（非法枚举值 → 400，不静默忽略）。
fn build_filter(query: &TaskPageQuery) -> std::result::Result<TaskFilter, ApiError> {
    let status = match query.status.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(dto::normalize_status(raw).ok_or_else(|| {
            param_invalid(format!(
                "状态取值非法：{raw}（应为 {}）",
                dto::STATUSES.join(" / ")
            ))
        })?),
    };
    // T9-A：状态多选（逗号分隔）。`statuses` 非空时优先于单值 `status`；
    // 空字符串 / 全是空串 → `None`（等价不传）。
    let statuses = match query.statuses.as_deref() {
        None => None,
        Some(raw) => {
            let list = raw
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| {
                    dto::normalize_status(item).ok_or_else(|| {
                        param_invalid(format!(
                            "状态取值非法：{item}（应为 {}）",
                            dto::STATUSES.join(" / ")
                        ))
                    })
                })
                .collect::<std::result::Result<Vec<_>, ApiError>>()?;
            (!list.is_empty()).then_some(list)
        }
    };
    let priority = match query.priority.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(dto::normalize_priority(Some(raw)).ok_or_else(|| {
            param_invalid(format!(
                "优先级取值非法：{raw}（应为 {}）",
                dto::PRIORITIES.join(" / ")
            ))
        })?),
    };
    let owner_id = match query.owner_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "负责人")?),
    };
    let project_id = match query.project_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "项目")?),
    };
    // P-FLOW-1A：来源筛选（`source=FLOW` 只看流程生成的任务）。非法值 400，不静默忽略。
    let source = match query.source.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            let upper = raw.to_ascii_uppercase();
            if !dto::SOURCES.contains(&upper.as_str()) {
                return Err(param_invalid(format!(
                    "来源取值非法：{raw}（应为 {}）",
                    dto::SOURCES.join(" / ")
                )));
            }
            Some(upper)
        }
    };
    if let (Some(from), Some(to)) = (query.due_from, query.due_to)
        && from > to
    {
        return Err(param_invalid(
            "截止日范围非法：下界不能晚于上界".to_string(),
        ));
    }
    Ok(TaskFilter {
        status,
        statuses,
        priority,
        owner_id,
        project_id,
        source,
        due_from: query.due_from,
        due_to: query.due_to,
        keyword: query.normalized_keyword(),
        // T8-C：树形视图只取顶层（缺省 false → 其余视图行为不变）
        root_only: query.root_only.unwrap_or(false),
    })
}

/// 把任务分页查询参数归一化成 [`TaskFilter`]（**供项目模块复用**）。
///
/// 项目下任务列表（`GET /business/project/{id}/tasks`）必须与任务页**同一套过滤口径**，
/// 否则「项目里没有这条任务」与「筛选条件不同」会混在一起，排查成本极高。
/// 项目模块只在本函数的结果上覆盖 `project_id` 一个字段。
pub(crate) fn build_task_filter(
    query: &TaskPageQuery,
) -> std::result::Result<TaskFilter, ApiError> {
    build_filter(query)
}

/// 把可见性分页结果包装成出参（**供项目模块复用同一个出参形状**）。
pub(crate) fn page_res(result: &model::VisiblePage) -> TaskPageRes {
    TaskPageRes {
        list: result.list.iter().map(card_res).collect(),
        total: result.total,
        page: result.page,
        page_size: result.size,
        member_scope_truncated: result.member_scope_truncated,
    }
}

// ==================== 创建 ====================

/// 新建任务（两秒建任务：只传标题也能建）。
///
/// # 默认值（设计文档 §十，逐条实现）
///
/// - 负责人 = 创建人自己（传 `owner_id` 则直接分派，`source = ASSIGNED` 且创建人自动成为协作人）；
/// - 状态 = `TODO`、进度 = 0（`INSERT` 里的字面量，不接受入参覆盖）；
/// - 优先级 = `P2`；
/// - 截止日 = **本周五**（CST；周六/周日建则顺延到下一个周五）；
/// - 需验收 = `false`。
///
/// # 返回值
///
/// `{ id, task_no, status, priority, progress, due_date, owner_id }`。
///
/// # 异常情况
///
/// - 400 + `63003`：标题为空 / 超长；
/// - 400 + `63004`：优先级 / 工时 / 日期非法；
/// - 400 + `63005`：父任务不存在 / 超过 8 层 / 成环；
/// - 400 + `10001`：`owner_id` / `parent_id` 不是合法 UUID；
/// - 400 + `63009`：负责人不存在或已停用；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task",
    method = "POST",
    permission = "business:task:create"
)]
#[nik::post("/business/task")]
pub async fn create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<TaskCreateReq>,
) -> ResResult<TaskSaveRes> {
    let me = current_user(&claims)?;
    let title = normalize_title(&req.title)?;
    let description = normalize_description(req.description.as_deref())?;
    let priority = match req.priority.as_deref() {
        None => dto::DEFAULT_PRIORITY.to_string(),
        Some(raw) => dto::normalize_priority(Some(raw)).ok_or_else(|| {
            param_invalid(format!(
                "优先级取值非法：{raw}（应为 {}）",
                dto::PRIORITIES.join(" / ")
            ))
        })?,
    };
    let estimate_hours = validate_hours(req.estimate_hours)?;
    if let (Some(start), Some(due)) = (req.start_date, req.due_date)
        && start > due
    {
        return Err(param_invalid(
            "日期非法：计划开始日不能晚于截止日".to_string(),
        ));
    }

    // 负责人：缺省 = 自己；传他人 = 分派（§八「分派即自动加协作人」）
    let owner_id = match req.owner_id.as_deref().map(str::trim) {
        None | Some("") => me,
        Some(raw) => parse_uuid_field(raw, "负责人")?,
    };
    if owner_id != me
        && !model::user_exists(owner_id)
            .await
            .map_err(|error| utils::db_error(error, "校验任务负责人"))?
    {
        return Err(ApiError::new(
            400,
            error_code::TASK_OWNER_INVALID,
            "负责人不存在或已停用",
        ));
    }
    let dept_id = model::user_dept(owner_id)
        .await
        .map_err(|error| utils::db_error(error, "查询负责人所属部门"))?;

    // 父任务：存在 + 深度 ≤ MAX_TASK_DEPTH（8 层）+ 不成环
    let parent_id = match req.parent_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "父任务")?),
    };
    if let Some(parent_id) = parent_id {
        model::validate_parent(parent_id, None)
            .await
            .map_err(parent_invalid)?;
    }
    let project_id = match req.project_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "项目")?),
    };

    let due_date = req
        .due_date
        .unwrap_or_else(|| model::default_due_date(model::today_cst()));
    let id = Uuid::now_v7();
    let month = model::month_key(model::today_cst());
    let new = model::NewTask {
        id,
        title,
        description,
        priority: priority.clone(),
        need_review: req.need_review.unwrap_or(false),
        owner_id,
        creator_id: me,
        assigner_id: (owner_id != me).then_some(me),
        dept_id,
        parent_id,
        project_id,
        start_date: req.start_date,
        due_date: Some(due_date),
        estimate_hours,
        source: if owner_id == me {
            dto::SOURCE_SELF.to_string()
        } else {
            dto::SOURCE_ASSIGNED.to_string()
        },
        add_creator_as_collaborator: owner_id != me,
    };
    let task_no = db()
        .transaction(|tx| async move { model::create_task(&tx, &new, me, &month).await })
        .await
        .map_err(|error| utils::db_error(error, "新建任务"))?;

    tracing::info!(
        task_id = %id,
        task_no = %task_no,
        owner_id = %owner_id,
        creator = %me,
        "任务已创建"
    );
    Ok(Res::ok(TaskSaveRes {
        id: id.to_string(),
        task_no,
        status: dto::STATUS_TODO.to_string(),
        priority,
        progress: 0,
        due_date: Some(due_date),
        owner_id: owner_id.to_string(),
    }))
}

// ==================== 编辑 ====================

/// 编辑任务（**仅负责人 / 创建人**）。
///
/// # 只改本次传入的字段
///
/// 缺席字段保持原值（`Option<Option<T>>` 语义，见 `dto::TaskUpdateReq`）。
/// 改 `owner_id` 时同步刷新 `dept_id` 快照（否则老板把任务交给别的部门的人之后，
/// 原部门的看板还会显示它）。
///
/// # 转派不在这里
///
/// 设计文档 §八 的「转派」要额外动作：原负责人变协作人 + 发通知 + `source` 变
/// `TRANSFERRED` —— 那是 T3 的交付物。本卡只做「直接改字段」。
///
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 已软删；
/// - 403 + `63002`：不是负责人也不是创建人；
/// - 400 + `63003` / `63004` / `63005` / `63009`：同创建；
/// - 400 + `10001`：空 PUT（一个字段都没传）。
#[nik::permission(
    path = "/business/task/{id}",
    method = "PUT",
    permission = "business:task:update"
)]
#[nik::put("/business/task/{id}")]
pub async fn update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskUpdateReq>,
) -> ResResult<TaskSaveRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    if req.is_empty() {
        return Err(param_invalid("请求体没有任何可修改字段".to_string()));
    }
    let current = require_task(task_id).await?;
    if current.card.owner_id != me && current.creator_id != me {
        return Err(ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的负责人或创建人可以编辑",
        ));
    }

    let mut patch = model::TaskPatch::default();
    if let Some(title) = req.title.as_ref() {
        patch.title = Some(normalize_title(title)?);
    }
    if let Some(description) = req.description.as_ref() {
        patch.description = Some(match description.as_deref() {
            Some(raw) => normalize_description(Some(raw))?,
            None => None,
        });
    }
    if let Some(priority) = req.priority.as_deref() {
        patch.priority = Some(dto::normalize_priority(Some(priority)).ok_or_else(|| {
            param_invalid(format!(
                "优先级取值非法：{priority}（应为 {}）",
                dto::PRIORITIES.join(" / ")
            ))
        })?);
    }
    patch.need_review = req.need_review;
    if let Some(owner_raw) = req.owner_id.as_deref() {
        let owner_id = parse_uuid_field(owner_raw, "负责人")?;
        if !model::user_exists(owner_id)
            .await
            .map_err(|error| utils::db_error(error, "校验任务负责人"))?
        {
            return Err(ApiError::new(
                400,
                error_code::TASK_OWNER_INVALID,
                "负责人不存在或已停用",
            ));
        }
        let dept_id = model::user_dept(owner_id)
            .await
            .map_err(|error| utils::db_error(error, "查询负责人所属部门"))?;
        patch.owner = Some((owner_id, dept_id));
    }
    if let Some(parent_id) = req.parent_id.as_ref() {
        match parent_id {
            Some(raw) => {
                let parent_id = parse_uuid_field(raw, "父任务")?;
                model::validate_parent(parent_id, Some(task_id))
                    .await
                    .map_err(parent_invalid)?;
                patch.parent_id = Some(Some(parent_id));
            }
            None => patch.parent_id = Some(None),
        }
    }
    if let Some(project_id) = req.project_id.as_ref() {
        patch.project_id = Some(match project_id {
            Some(raw) => Some(parse_uuid_field(raw, "项目")?),
            None => None,
        });
    }
    if let Some(start_date) = req.start_date.as_ref() {
        patch.start_date = Some(*start_date);
    }
    if let Some(due_date) = req.due_date.as_ref() {
        patch.due_date = Some(*due_date);
    }
    if let Some(estimate_hours) = req.estimate_hours.as_ref() {
        patch.estimate_hours = Some(validate_hours(*estimate_hours)?);
    }
    if let (Some(Some(start)), Some(Some(due))) = (patch.start_date, patch.due_date)
        && start > due
    {
        return Err(param_invalid(
            "日期非法：计划开始日不能晚于截止日".to_string(),
        ));
    }

    let affected = db()
        .transaction(|tx| async move { model::update_task(&tx, task_id, &patch, me).await })
        .await
        .map_err(|error| utils::db_error(error, "编辑任务"))?;
    if affected == 0 {
        return Err(not_found());
    }
    let updated = require_task(task_id).await?;
    Ok(Res::ok(save_res(&updated)))
}

// ==================== 删除 ====================

/// 软删除任务（负责人 / 创建人 / 超管）。
///
/// # 为什么不物理删除
///
/// 变更日志、通知、工时、依赖都指向任务 ID；物理删除会让这些引用变成悬空。
/// 软删后 `/page`、`/my`、详情一律看不到它（可见性谓词与列表都带 `deleted_at IS NULL`），
/// 但编号 `task_no` 不再复用（唯一索引建在全表上）。
///
/// # 有未完成子任务时拒绝（409）
///
/// 父任务被删掉、子任务还挂着 `parent_id` 指向它，详情页会展开出一堆孤儿。
/// 先让用户处理子任务（完成或取消），是更安全的默认。
///
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 已软删；
/// - 403 + `63202`：**操作者是 AI 用户（`user_type='ROBOT'`）** —— 硬拒，无角色可解；
/// - 403 + `63002`：既不是负责人 / 创建人，也不是超管；
/// - 409 + `63006`：仍有未完成子任务；
/// - 500：数据库异常。
///
/// # 副作用（T12）
///
/// 软删任务与「终止它的在跑审批单」在**同一事务**里：该任务名下 `PENDING` 的
/// 审批实例 → `CANCELLED`（同写 `ended_at` + `result_comment`），其 `PENDING`
/// 待办 → `CANCELLED`（同写 `completed_at`）。否则审批单成孤儿，一直挂在待办里。
/// `APPROVED` / `REJECTED` / `WITHDRAWN` 的实例**不动**（已完成的事实留作审计）。
#[nik::permission(
    path = "/business/task/{id}",
    method = "DELETE",
    permission = "business:task:delete"
)]
#[nik::delete("/business/task/{id}")]
pub async fn delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskDeleteRes> {
    let me = current_user(&claims)?;
    // 红线（修正 11 + T5）：AI 用户（`user_type=ROBOT`）**永远不能删任务**。
    //
    // 这是**硬拒**，与分派不同：分派「默认不能、可按角色授予」（只查权限码），
    // 删除则连超管角色都救不回来 —— 理由是一条 AI 执行者被注入后
    // 「删掉任务」是不可逆的破坏（评论 / 日志 / 通知全指向一个软删的主键），
    // 而「停止执行」有正当替代路径（转派给人类 / 取消任务）。
    //
    // 错误码 `63202`（`AI_TASK_DELETE_FORBIDDEN`）：T5 卡面把 AI 执行者硬拒
    // 统一到 632xx 子段，T3 临时用的 `63016` 保留但不再返回（见 error_code 注释）。
    if let Some(error) = ai_actor::task_delete_forbidden(user_type_of(me).await?.as_deref()) {
        return Err(error);
    }
    let me_scope = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let current = require_task(task_id).await?;
    let is_owner_or_creator = current.card.owner_id == me || current.creator_id == me;
    if !is_owner_or_creator && !me_scope.is_super() {
        return Err(ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的负责人、创建人或管理员可以删除",
        ));
    }
    let open_children = model::count_open_children(task_id)
        .await
        .map_err(|error| utils::db_error(error, "统计未完成子任务"))?;
    if open_children > 0 {
        return Err(ApiError::new(
            409,
            error_code::TASK_HAS_OPEN_CHILDREN,
            format!("还有 {open_children} 个未完成子任务，请先处理再删除"),
        ));
    }
    // T12：软删任务与「终止它的在跑审批单」必须**同一事务**。
    // 只删任务会留下 `PENDING` 孤儿实例（一直挂在审批待办里，点进去任务已不存在）；
    // 终止失败就整笔回滚 —— 留个孤儿待办比这次删不掉任务更麻烦（卡面决策）。
    let task_no = current.card.task_no.clone();
    let (affected, cancelled) = db()
        .transaction(|tx| async move {
            let affected = model::soft_delete(&tx, task_id, me).await?;
            // 已经删过（重复删除幂等，affected = 0）→ 没有在跑实例可终止，省一次写。
            let cancelled = if affected > 0 {
                task_bridge::cancel_on_task_deleted(&tx, task_id, &task_no).await?
            } else {
                crate::business::flow::model::CancelledOfBiz::default()
            };
            Ok((affected, cancelled))
        })
        .await
        .map_err(|error| utils::db_error(error, "删除任务"))?;
    tracing::info!(
        task_id = %task_id,
        operator = %me,
        affected,
        cancelled_instances = cancelled.instances,
        cancelled_tasks = cancelled.tasks,
        "任务已软删并终止在跑审批单"
    );
    Ok(Res::ok(TaskDeleteRes {
        id: task_id.to_string(),
        deleted: affected > 0,
    }))
}

// ==================== 详情 ====================

/// 任务详情（子任务 / 协作人 / 附件一次给全）。
///
/// # 可见性
///
/// 看不见的任务 → **404**（不是 403）。子任务列表同样按可见性过滤：
/// 一个跨部门协作人能看到父任务，但看不到父任务下属于别的部门、且没把他拉进去的子任务。
///
/// # 双身份（T5b · 令牌白名单的 4 个端点之一）
///
/// 这个端点同时接受两种身份：
///
/// - **人类 JWT**：身份由框架认证层注入 `AuthClaims`；因为路径在
///   `#[nik::no_auth]` 白名单里，权限层会跳过本路径，所以 `business:task:read`
///   的判定**挪到 handler 里**（[`ensure_task_read_permission`]），可见性照旧走
///   [`principal`]；
/// - **AI 任务令牌**（`Authorization: Bearer cpai_…`）：
///   [`crate::middleware::task_token::guard`] 已校验「令牌 ↔ 路径任务一致」，
///   这里以任务的 AI 负责人身份解析数据范围（**不再做 scope 检查**：令牌本身
///   已把范围限定在这一个任务上）。
///
/// 两条身份都不会为 `None`：都没有时返回 `401`。
///
/// # 异常情况
///
/// - 401：既没有有效 JWT 也没有任务令牌；
/// - 403 + `63002`：人类账号没有 `business:task:read`；
/// - 404 + `63001`：任务不存在 / 已软删 / 不在可见范围内；
/// - 400 + `10001`：路径里的 id 不是 UUID；
/// - 500：数据库异常。
#[nik::no_auth("/business/task/{id}")]
#[nik::get("/business/task/{id}")]
pub async fn detail(
    claims: Option<Extension<AuthClaims>>,
    ai_actor: Option<Extension<AiTaskActor>>,
    Path(id): Path<String>,
) -> ResResult<TaskDetailRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let reader = resolve_reader(claims, ai_actor)?;
    let me = reader.principal().await?;
    let row = model::find_detail(&me, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let children = model::list_children(&me, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    let members = model::list_members(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let attachments = model::list_attachments(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务附件"))?;

    Ok(Res::ok(TaskDetailRes {
        card: card_res(&row.card),
        description: row.description.clone(),
        creator_id: row.creator_id.to_string(),
        creator_name: row.creator_name.clone(),
        assigner_id: row.assigner_id.map(|id| id.to_string()),
        assigner_name: row.assigner_name.clone(),
        dept_name: row.dept_name.clone(),
        parent_id: row.parent_id.map(|id| id.to_string()),
        parent_task_no: row.parent_task_no.clone(),
        done_at: row.done_at,
        estimate_hours: row.estimate_hours,
        actual_hours: row.actual_hours,
        cancel_reason: row.cancel_reason.clone(),
        reject_reason: row.reject_reason.clone(),
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
        exec_mode: row.exec_mode.clone(),
        hermes_board: row.hermes_board.clone(),
        hermes_task_id: row.hermes_task_id.clone(),
        hermes_status: row.hermes_status.clone(),
        ai_summary: row.ai_summary.clone(),
        ai_result: row
            .ai_result
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        children: children.iter().map(card_res).collect(),
        members: members
            .iter()
            .map(|member| TaskMemberRes {
                user_id: member.user_id.to_string(),
                user_name: member.user_name.clone(),
                role: member.role.clone(),
                created_at: member.created_at,
            })
            .collect(),
        attachments: attachments
            .iter()
            .map(|item| TaskAttachmentRes {
                id: item.id.to_string(),
                file_id: item.file_id.to_string(),
                file_name: item.file_name.clone(),
                file_size: item.file_size,
                sort_order: item.sort_order,
                source: item.source.clone(),
                ai_profile: item.ai_profile.clone(),
            })
            .collect(),
    }))
}

// ==================== 我的任务 ====================

/// 我的任务（五分组：逾期 / 今日 / 本周 / 以后 / 无期限）。
///
/// # 口径
///
/// - 只算 `owner_id = 我` 的任务（协作人看到 ≠ 我的任务；协作视图走 `/page`）；
/// - `DONE` / `CANCELLED` 不进「我的任务」（它们不再是「要做的事」）；
/// - 日期按 **CST** 算（`model::today_cst`），分组规则见 `model::bucket_of`。
///
/// # 异常情况
///
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/my",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/my")]
pub async fn my(Extension(AuthClaims(claims)): Extension<AuthClaims>) -> ResResult<MyTasksRes> {
    let me = current_user(&claims)?;
    let rows = model::my_tasks(me)
        .await
        .map_err(|error| utils::db_error(error, "查询我的任务"))?;
    let today = model::today_cst();
    let mut overdue = Vec::new();
    let mut today_list = Vec::new();
    let mut this_week = Vec::new();
    let mut later = Vec::new();
    let mut no_due = Vec::new();
    for row in &rows {
        match model::bucket_of(row.due_date, today) {
            MyBucket::Overdue => overdue.push(card_res(row)),
            MyBucket::Today => today_list.push(card_res(row)),
            MyBucket::ThisWeek => this_week.push(card_res(row)),
            MyBucket::Later => later.push(card_res(row)),
            MyBucket::NoDue => no_due.push(card_res(row)),
        }
    }
    let total = rows.len() as i64;
    Ok(Res::ok(MyTasksRes {
        counts: MyCounts {
            overdue: overdue.len() as i64,
            today: today_list.len() as i64,
            this_week: this_week.len() as i64,
            later: later.len() as i64,
            no_due: no_due.len() as i64,
        },
        overdue,
        today: today_list,
        this_week,
        later,
        no_due,
        total,
    }))
}

// ==================== 状态机（T2） ====================

/// 改状态（`PATCH /business/task/{id}/status`）。
///
/// # 四段式判定（顺序不可换，见 [`crate::business::task::state::check_transition`]）
///
/// 0. **审批门闸**：任务挂了审批单且实例 `PENDING` 时的 `REVIEW → DONE / DOING` → `409 + 65030`
///    （**先于**白名单与角色：审批中换谁来点都是同一个答案）；
/// 1. **可见性**：看不见的任务 → `404 + 63001`（不暴露「存在但无权」）；
/// 2. **白名单**：`from → to` 不在 [`state::TRANSITIONS`] 里 → `409 + 63010`；
/// 3. **角色 + 附加条件**：无权 → `403 + 63011`；打回缺原因 → `400 + 63013`；
///    `need_review=true` 却想直接完成 → `409 + 63010`。
///
/// # 副作用（全部在同一个事务里）
///
/// - 改 `status` / `progress`（无子任务才联动）/ `done_at` / 打回或取消原因；
/// - 写一条 `comment_type = STATUS_CHANGE` 的评论（含 from/to/reason）；
/// - 写 `sys_change_log`（`action = STATUS`，白名单字段 diff）；
/// - 若本条是子任务 → 沿 `parent_id` 逐级重算父任务进度（架构 D4/D5）；
/// - 若本条是子任务 → 沿 `parent_id` 逐级**级联父任务状态**（T10：首个活动子任务 →
///   父任务进 `DOING`；子任务全部到终态 → 父任务进 `REVIEW`，见
///   [`model::cascade_parent_status`]）；
/// - **本条推到 `CANCELLED` → 同一事务里级联取消全部非终态子孙**（T11：父任务取消的
///   语义就是「这条线不要了」，见 [`model::cascade_cancel_descendants`]）；
/// - **本条推到 `CANCELLED` → 同一事务里撤掉本人与全部级联子孙的在跑审批单**
///   （P11 铁律①「正向必配反向」：实例 `CANCELLED` + 待办 `CANCELLED` + 节点 `CANCELLED`，
///   见 [`crate::business::flow::task_bridge::cancel_on_task_cancelled`]；撤不掉 → 整笔回滚）；
/// - **`DOING → REVIEW` 且配了 `TASK_ACCEPT` 审批流程 → 自动发起审批单并回写
///   `approval_instance_id`**（没配流程则什么都不做，保持原两级验收，见
///   [`crate::business::flow::task_bridge::prepare_accept`]）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 id 不是 UUID；400 + `63004`：`to` 不是 5 态之一；
/// - 400 + `63013`：打回没填原因 / 不足 5 字；
/// - 403 + `63011`：当前身份无权推这个流转；
/// - 404 + `63001`：任务不存在 / 不可见；
/// - 409 + `63010`：非法流转（跳级 / 终态直取 / 同状态）；
/// - 409 + `65030`：审批单还在跑，`REVIEW` 出边一律不许人工推（等审批结论）；
/// - 409 + `63021`：**父任务已取消**，子任务不能继续推进（子任务自己 → `CANCELLED` 除外）；
/// - 409 + `63006`：还有未完成子任务，不能完成；
/// - 500：数据库异常 / 变更日志未登记 / 审批单发起失败（整笔回滚）。
#[nik::permission(
    path = "/business/task/{id}/status",
    method = "PATCH",
    permission = "business:task:status"
)]
#[nik::patch("/business/task/{id}/status")]
pub async fn change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskStatusChangeReq>,
) -> ResResult<TaskStatusRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;

    // ① 可见性：只有看得见的人才能推（协作人看得见 → 下一步会被角色判定挡在 403）
    let current = model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;

    // ② 目标状态取值
    let to = TaskStatus::parse(&req.to).ok_or_else(|| {
        param_invalid(format!(
            "状态取值非法：{}（应为 {}）",
            req.to,
            TaskStatus::ALL
                .iter()
                .map(|status| status.code())
                .collect::<Vec<_>>()
                .join(" / ")
        ))
    })?;
    let from = TaskStatus::parse(&current.card.status).ok_or_else(|| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("库里的任务状态不是 5 态之一：{}", current.card.status),
        )
    })?;

    // ③ 前守卫（T11 规则②）：父任务已取消 → 子任务一律不许推进。
    //    为什么放在状态机之前：父已取消时前端仍可能把「开始/提交验收」按钮露出来，
    //    此时回 63021（父任务已取消）比回 63010（非法流转）准确 —— 流转本身是合法的，
    //    作废的是整条线。守卫只看父任务，`CANCELLED` 自己幂等放行。
    let parent_status = model::parent_status(db(), task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询父任务状态"))?;
    if model::blocks_child_advance(parent_status, to) {
        return Err(parent_cancelled());
    }

    // ③ 审批门闸 + 白名单 + 角色 + 附加条件（D3b：门闸在状态机之前）
    let people = state::TaskPeople {
        owner_id: current.card.owner_id,
        creator_id: current.creator_id,
        assigner_id: current.assigner_id,
    };
    let role_mask = people.role_mask(&state::Actor::new(me, mine.is_super()));
    let reason = normalize_reason(req.reason.as_deref())?;
    // 门闸输入：任务上挂了审批单就现查实例状态（读不到 → 不拦，见 state::flow_gate_blocked）
    let instance_status = task_bridge::instance_status_of(current.approval_instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单状态"))?;
    let transition = state::check_transition(
        // 红线：HTTP 路径永远是「人类」身份，不从请求读这个值
        state::ActorKind::User,
        from,
        to,
        role_mask,
        current.card.need_review,
        reason.as_deref(),
        current.approval_instance_id,
        instance_status.as_deref(),
    )
    .map_err(transition_check_error)?;

    // ④ 进入 DONE：有未完成子任务 → 409（设计 §三「父任务 DONE 前置检查」）
    if to == TaskStatus::Done {
        let open = model::count_open_children(task_id)
            .await
            .map_err(|error| utils::db_error(error, "统计未完成子任务"))?;
        if open > 0 {
            return Err(ApiError::new(
                409,
                error_code::TASK_HAS_OPEN_CHILDREN,
                format!("还有 {open} 个未完成子任务，不能直接完成父任务"),
            ));
        }
    }

    // ⑤ 前置依赖校验（H-DEP）：T4 落地「所有 FS 前置必须 DONE」。
    //    这里保留调用点与注释，T4 只需把 `check_dependencies_ready` 换成真实实现，
    //    本函数的其余部分不用改。
    if from == TaskStatus::Todo && to == TaskStatus::Doing {
        check_dependencies_ready(task_id).await?;
    }

    // ⑤b 提交验收（DOING → REVIEW）且 `flow_def` 里配了 `TASK_ACCEPT` 流程 → 自动发起审批单。
    //    没配流程 → `prepare_accept` 返回 `None`，照原两级验收走（**不报错**，存量行为不变）。
    //    真正的入库在 ⑧ 的事务里（`start_in_tx`）：审批单建不出来就整笔回滚。
    let accept = if from == TaskStatus::Doing && to == TaskStatus::Review {
        let form_data = serde_json::json!({
            "task_id": task_id.to_string(),
            "task_no": current.card.task_no,
            "title": current.card.title,
            "priority": current.card.priority,
        });
        task_bridge::prepare_accept(
            task_id,
            &current.card.task_no,
            &current.card.title,
            me,
            form_data,
        )
        .await
        .map_err(|error| utils::db_error(error, "准备验收审批单"))?
    } else {
        None
    };

    // ⑥ 进度：有子任务的任务由汇总决定，**任何流转都不许手改**
    let children = model::children_of(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    let locked = state::progress_locked(&model::snapshots_of(&children));
    let new_progress = if locked {
        None
    } else {
        transition.progress.apply(current.card.progress)
    };

    // ⑦ 评论正文（from/to/reason 三件套，前端时间线按它渲染）
    let mut content = format!(
        "状态变更：{}（{}）→ {}（{}）",
        from.label(),
        from.code(),
        to.label(),
        to.code()
    );
    if let Some(reason) = reason.as_deref() {
        content.push_str(&format!("；原因：{reason}"));
    }
    if locked {
        content.push_str("；本条有子任务，进度由子任务汇总");
    } else if let Some(progress) = new_progress
        && progress != current.card.progress
    {
        content.push_str(&format!("；进度 {}% → {progress}%", current.card.progress));
    }

    let update = model::StatusUpdate {
        to: to.code().to_string(),
        progress: new_progress,
        done_at: match transition.done_at {
            state::DoneAtEffect::Keep => None,
            state::DoneAtEffect::Clear => Some(None),
            // 时间点在 Rust 侧算好：变更日志的 diff 与库里的值必须是同一个
            state::DoneAtEffect::SetNow => Some(Some(Utc::now())),
        },
        reject_reason: if from == TaskStatus::Review && to == TaskStatus::Doing {
            reason.clone()
        } else {
            None
        },
        cancel_reason: if to == TaskStatus::Cancelled {
            reason.clone()
        } else {
            None
        },
    };
    let after_progress = i32::from(new_progress.unwrap_or(current.card.progress));
    let after_done_at = match update.done_at.as_ref() {
        Some(value) => value.as_ref().map(|at| at.to_rfc3339()),
        None => current.done_at.map(|at| at.to_rfc3339()),
    };

    // ⑧ 同事务：写状态 → 写评论 → 写变更日志 → 重算父任务
    let before = serde_json::json!({
        "status": from.code(),
        "progress": i32::from(current.card.progress),
        "done_at": current.done_at.map(|at| at.to_rfc3339()),
        "reject_reason": current.reject_reason,
        "cancel_reason": current.cancel_reason,
    });
    let after = serde_json::json!({
        "status": to.code(),
        "progress": after_progress,
        "done_at": after_done_at,
        "reject_reason": if update.reject_reason.is_some() { reason.clone() } else { current.reject_reason.clone() },
        "cancel_reason": if update.cancel_reason.is_some() { reason.clone() } else { current.cancel_reason.clone() },
    });
    let operator_name = display_name(&claims);
    let operator_name_tx = operator_name.clone();
    let action_label = transition.label;
    let task_no = current.card.task_no.clone();
    let task_no_tx = task_no.clone();
    let (comment_id, affected, recalcs, cascades, cancels, approvals) = db()
        .transaction(|tx| async move {
            // T11 守卫（**事务内**再判一次）：③ 前那次读与这次写之间，父任务可能被
            // 并发取消 —— 那种情况下子任务照样不许推进（漏洞 2 的并发变体）。
            // 错误用哨兵回传，闭包外的 `change_status_tx_error` 把它翻成 409 + 63021。
            if model::blocks_child_advance(model::parent_status(&tx, task_id).await?, to) {
                return Err(DbError::InvalidValue(PARENT_CANCELLED_SENTINEL.to_string()));
            }
            let affected = model::apply_status(&tx, task_id, &update, me).await?;
            if affected == 0 {
                // 事务里被并发软删：评论与日志都不该落
                return Ok((
                    Uuid::nil(),
                    0,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    crate::business::flow::model::CancelledOfBiz::default(),
                ));
            }
            // T11 规则①：本条推到 CANCELLED → 同一事务里级联取消全部非终态子孙
            // （一条递归 CTE；DONE 不回退；失败 → 整笔回滚，不接受「父取消、子还在跑」）
            let cancels = if to == TaskStatus::Cancelled {
                model::cascade_cancel_descendants(&tx, task_id, me, model::CASCADE_CANCEL_REASON)
                    .await?
            } else {
                Vec::new()
            };
            // P11 · 铁律①「正向必配反向」：任务取消 → 它的审批实例与待办必须一起撤，
            // 否则审批人的「待我审批」里挂着一条点不动的孤儿待办（老板报的 TSK-149）。
            // 必须与状态写入同一事务：撤不掉就整笔回滚，不接受「任务 CANCELLED、待办还 PENDING」。
            // 级联取消的子孙也是「任务已取消」，各自的审批同样要撤（逐个按自己的 task_no 精确匹配）。
            let mut approvals = if to == TaskStatus::Cancelled {
                task_bridge::cancel_on_task_cancelled(&tx, task_id, &task_no_tx).await?
            } else {
                crate::business::flow::model::CancelledOfBiz::default()
            };
            for cancel in &cancels {
                let sub =
                    task_bridge::cancel_on_task_cancelled(&tx, cancel.task_id, &cancel.task_no)
                        .await?;
                approvals.instances += sub.instances;
                approvals.tasks += sub.tasks;
                approvals.nodes += sub.nodes;
            }
            // D3b：提交验收即发起审批单（与状态写入同事务：建不出审批单 → 整笔回滚）
            if let Some(start) = accept.as_ref() {
                task_bridge::start_in_tx(&tx, start, task_id).await?;
            }
            let comment_id = model::insert_comment(
                &tx,
                task_id,
                me,
                &content,
                model::COMMENT_TYPE_STATUS_CHANGE,
                me,
            )
            .await?;
            changelog::record(
                &tx,
                &ChangeCtx::api(Some(me), Some(&operator_name_tx)),
                ChangeTarget::of(
                    "biz_task",
                    task_id.to_string(),
                    Action::Status,
                    action_label,
                ),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| {
                DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg))
            })?;
            // T11：每个被级联取消的子孙各记一条变更日志（审计口径：不能只留在父任务
            // 那条汇总里 —— 事后要能回答「这条子任务是谁、为什么取消的」），
            // 并在父任务时间线上补一条汇总评论。
            for cancel in &cancels {
                let before = serde_json::json!({ "status": cancel.from.code() });
                let after = serde_json::json!({ "status": TaskStatus::Cancelled.code() });
                changelog::record(
                    &tx,
                    &ChangeCtx::api(Some(me), Some(operator_name_tx.as_str())),
                    ChangeTarget::of(
                        "biz_task",
                        cancel.task_id.to_string(),
                        Action::Status,
                        model::CASCADE_CANCEL_REASON,
                    ),
                    DiffInput::update(&before, &after),
                )
                .await
                .map_err(|error| {
                    DbError::InvalidValue(format!(
                        "变更日志写入失败 [{}] {}",
                        error.code, error.msg
                    ))
                })?;
            }
            if !cancels.is_empty() {
                model::insert_comment(
                    &tx,
                    task_id,
                    me,
                    &format!(
                        "级联取消：本任务取消后，{} 个未完成的子孙任务已随之取消（{}）",
                        cancels.len(),
                        model::CASCADE_CANCEL_REASON
                    ),
                    model::COMMENT_TYPE_STATUS_CHANGE,
                    me,
                )
                .await?;
            }
            let recalcs = model::recalc_ancestors(&tx, task_id, me).await?;
            // T10：父任务状态随子任务级联（**同事务**：级联失败 → 子任务状态一起回滚，
            // 红线是「宁可这次没改成，也不接受父任务状态错着」）
            let cascades = model::cascade_parent_status(&tx, task_id, me).await?;
            // 级联的变更日志也在这个闭包里写：状态改了而日志没写就是审计缺口，
            // 两者必须同生共死（G5 门禁：`record` 的调用点只能在事务闭包内）。
            for cascade in &cascades {
                let before = serde_json::json!({ "status": cascade.from.code() });
                let after = serde_json::json!({ "status": cascade.to.code() });
                changelog::record(
                    &tx,
                    &ChangeCtx::api(Some(me), Some(operator_name_tx.as_str())),
                    ChangeTarget::of(
                        "biz_task",
                        cascade.task_id.to_string(),
                        Action::Status,
                        cascade.reason,
                    ),
                    DiffInput::update(&before, &after),
                )
                .await
                .map_err(|error| {
                    DbError::InvalidValue(format!(
                        "变更日志写入失败 [{}] {}",
                        error.code, error.msg
                    ))
                })?;
            }
            Ok((comment_id, affected, recalcs, cascades, cancels, approvals))
        })
        .await
        .map_err(change_status_tx_error)?;
    if affected == 0 {
        return Err(not_found());
    }

    // ⑨ 通知（事务外：通知失败不该回滚状态）
    notify_status_change(&current, from, to, me, &operator_name, reason.as_deref()).await;
    notify_ready_to_close(&recalcs, me, &operator_name).await;
    // ⑨b 级联结果留痕（改动已在事务里落库，这里只补一条可 grep 的汇总行）
    for cascade in &cascades {
        tracing::info!(
            task_id = %cascade.task_id,
            from = cascade.from.code(),
            to = cascade.to.code(),
            reason = cascade.reason,
            "父任务状态级联"
        );
    }
    // ⑨c T11：级联取消留痕（每条已在事务里各写一条变更日志，这里补一条可 grep 的汇总行）
    if !cancels.is_empty() {
        tracing::info!(
            task_id = %task_id,
            cancelled = cancels.len() as i64,
            reason = model::CASCADE_CANCEL_REASON,
            "父任务取消：子任务已级联取消"
        );
    }
    // ⑨d P11：撤销审批的留痕（孤儿待办是「正向没有配套反向」的典型，日志要能一眼查到）
    if to == TaskStatus::Cancelled
        && approvals != crate::business::flow::model::CancelledOfBiz::default()
    {
        tracing::info!(
            task_id = %task_id,
            task_no = %task_no,
            instances = approvals.instances,
            flow_tasks = approvals.tasks,
            nodes = approvals.nodes,
            comment = task_bridge::TASK_CANCEL_COMMENT,
            "任务取消：在跑审批实例与待办已一并撤销"
        );
    }
    // ⑩ 依赖联动（T4）：前置 DONE → 强制通知后继负责人；前置 CANCELLED → 尊重偏好提醒
    if matches!(to, TaskStatus::Done | TaskStatus::Cancelled) {
        notify_dependents(&current, to, me, &operator_name).await;
    }

    let updated = require_task(task_id).await?;
    tracing::info!(
        task_id = %task_id,
        from = from.code(),
        to = to.code(),
        operator = %me,
        "任务状态已流转"
    );
    Ok(Res::ok(TaskStatusRes {
        id: task_id.to_string(),
        task_no: updated.card.task_no.clone(),
        from: from.code().to_string(),
        status: updated.card.status.clone(),
        progress: i32::from(updated.card.progress),
        done_at: updated.done_at,
        comment_id: comment_id.to_string(),
        parent_progress: recalcs
            .iter()
            .map(|recalc| ParentProgressRes {
                id: recalc.task_id.to_string(),
                progress: i32::from(recalc.new),
                previous: i32::from(recalc.old),
                mode: recalc.mode.to_string(),
                all_done: recalc.all_done,
            })
            .collect(),
    }))
}

/// 改进度（`PATCH /business/task/{id}/progress`）。
///
/// # 两条硬规则
///
/// 1. **有子任务 → `409 + 63012`**（红线：父任务的进度由子任务汇总，手改无效且必须报错，
///    不许「静默改成想要的值然后被下次重算覆盖」）；
/// 2. **状态联动**：`DOING → max(进度, 10)`、`REVIEW` / `DONE → 100`、`CANCELLED` 冻结
///    （取消中的任务改进度没有意义，也不该被状态机之外的操作改）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 id 不是 UUID；400 + `63004`：进度不在 0~100；
/// - 403 + `63002`：既不是负责人 / 创建人，也不是管理员；
/// - 404 + `63001`：任务不存在 / 不可见；
/// - 409 + `63012`：有子任务，进度由子任务汇总；
/// - 409 + `63010`：已取消的任务进度被冻结；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/progress",
    method = "PATCH",
    permission = "business:task:progress"
)]
#[nik::patch("/business/task/{id}/progress")]
pub async fn change_progress(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskProgressReq>,
) -> ResResult<TaskProgressRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let current = model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;

    // 写权限：负责人 / 创建人 / 管理员（协作人看得见但改不了）
    if current.card.owner_id != me && current.creator_id != me && !mine.is_super() {
        return Err(ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的负责人、创建人或管理员可以修改进度",
        ));
    }
    if !(0..=100).contains(&req.progress) {
        return Err(param_invalid(format!(
            "进度必须在 0 ~ 100 之间（收到 {}）",
            req.progress
        )));
    }

    let children = model::children_of(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    if state::progress_locked(&model::snapshots_of(&children)) {
        return Err(ApiError::new(
            409,
            error_code::TASK_PROGRESS_LOCKED,
            format!(
                "进度由子任务汇总，不能手动修改（当前 {} 个子任务，汇总值已写入任务）",
                children.len()
            ),
        ));
    }

    let status = TaskStatus::parse(&current.card.status).ok_or_else(|| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("库里的任务状态不是 5 态之一：{}", current.card.status),
        )
    })?;
    let value = requested_progress(status, req.progress)?;

    let (affected, recalcs) = db()
        .transaction(|tx| async move {
            let affected = model::set_progress(&tx, task_id, value, me).await?;
            let recalcs = if affected == 0 {
                Vec::new()
            } else {
                model::recalc_ancestors(&tx, task_id, me).await?
            };
            Ok((affected, recalcs))
        })
        .await
        .map_err(|error| utils::db_error(error, "修改任务进度"))?;
    if affected == 0 {
        return Err(not_found());
    }
    let operator_name = display_name(&claims);
    notify_ready_to_close(&recalcs, me, &operator_name).await;
    let updated = require_task(task_id).await?;
    tracing::info!(task_id = %task_id, progress = value, operator = %me, "任务进度已修改");
    Ok(Res::ok(TaskProgressRes {
        id: task_id.to_string(),
        progress: i32::from(updated.card.progress),
        requested: req.progress,
    }))
}

/// 建子任务（`POST /business/task/{id}/subtask`，快捷入口）。
///
/// # 继承规则（卡面「inherit parent 的 project / 优先级默认」）
///
/// 未显式传的字段按父任务取：`project_id`、`priority`、`need_review`、`due_date`。
/// 负责人缺省 = **当前操作人**（不是父任务负责人：拆子任务的人自己认领更符合直觉，
/// 要指派给别人就显式传 `owner_id`）。
///
/// # 深度
///
/// 复用 [`model::validate_parent`]：父任务链最多 [`model::MAX_TASK_DEPTH`] 层（8 层），
/// 第 9 层建不出来；不能成环。3~8 级嵌套都是本接口正常可建的形态（原先是「最多 2 层」）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 / 字段 id 不是 UUID；400 + `63003` / `63004`：标题 / 字段非法；
/// - 400 + `63005`：父任务链已到 8 层（第 9 层建不出来）/ 成环；
/// - 404 + `63001`：父任务不存在 / 不可见；
/// - 409 + `63006`：父任务已完成 / 已取消（终态不允许再加子任务）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/subtask",
    method = "POST",
    permission = "business:task:subtask"
)]
#[nik::post("/business/task/{id}/subtask")]
pub async fn create_subtask(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskSubtaskCreateReq>,
) -> ResResult<TaskSaveRes> {
    let me = current_user(&claims)?;
    let mine = principal::resolve_for_claims(&claims).await?;
    let parent_id = parse_uuid_field(&id, "父任务")?;
    let parent = model::find_detail(&mine, parent_id)
        .await
        .map_err(|error| utils::db_error(error, "查询父任务"))?
        .ok_or_else(not_found)?;
    let parent_status = TaskStatus::parse(&parent.card.status).unwrap_or(TaskStatus::Todo);
    if parent_status.is_terminal() {
        return Err(ApiError::new(
            409,
            error_code::TASK_HAS_OPEN_CHILDREN,
            format!(
                "父任务已处于终态（{}），不能再添加子任务；请先重开",
                parent_status.label()
            ),
        ));
    }
    // 深度 / 成环：父任务自身不能已是子任务
    model::validate_parent(parent_id, None)
        .await
        .map_err(parent_invalid)?;

    let title = normalize_title(&req.title)?;
    let description = normalize_description(req.description.as_deref())?;
    let priority = match req.priority.as_deref() {
        None => parent.card.priority.clone(),
        Some(raw) => dto::normalize_priority(Some(raw)).ok_or_else(|| {
            param_invalid(format!(
                "优先级取值非法：{raw}（应为 {}）",
                dto::PRIORITIES.join(" / ")
            ))
        })?,
    };
    let estimate_hours = validate_hours(req.estimate_hours)?;
    let owner_id = match req.owner_id.as_deref().map(str::trim) {
        None | Some("") => me,
        Some(raw) => parse_uuid_field(raw, "负责人")?,
    };
    if owner_id != me
        && !model::user_exists(owner_id)
            .await
            .map_err(|error| utils::db_error(error, "校验任务负责人"))?
    {
        return Err(ApiError::new(
            400,
            error_code::TASK_OWNER_INVALID,
            "负责人不存在或已停用",
        ));
    }
    let dept_id = model::user_dept(owner_id)
        .await
        .map_err(|error| utils::db_error(error, "查询负责人所属部门"))?;
    let due_date = req.due_date.or(parent.card.due_date);

    let id = Uuid::now_v7();
    let month = model::month_key(model::today_cst());
    let new = model::NewTask {
        id,
        title,
        description,
        priority: priority.clone(),
        need_review: req.need_review.unwrap_or(parent.card.need_review),
        owner_id,
        creator_id: me,
        assigner_id: (owner_id != me).then_some(me),
        dept_id,
        parent_id: Some(parent_id),
        project_id: parent.project_id,
        start_date: req.start_date,
        due_date,
        estimate_hours,
        source: if owner_id == me {
            dto::SOURCE_SELF.to_string()
        } else {
            dto::SOURCE_ASSIGNED.to_string()
        },
        add_creator_as_collaborator: owner_id != me,
    };
    let task_no = db()
        .transaction(|tx| async move {
            let task_no = model::create_task(&tx, &new, me, &month).await?;
            // 新增子任务会改变父任务的聚合分母 → 立刻重算（写路径契约）
            model::recalc_ancestors(&tx, id, me).await?;
            Ok(task_no)
        })
        .await
        .map_err(|error| utils::db_error(error, "新建子任务"))?;

    tracing::info!(
        parent_id = %parent_id,
        task_id = %id,
        task_no = %task_no,
        owner_id = %owner_id,
        "子任务已创建"
    );
    Ok(Res::ok(TaskSaveRes {
        id: id.to_string(),
        task_no,
        status: dto::STATUS_TODO.to_string(),
        priority,
        progress: 0,
        due_date,
        owner_id: owner_id.to_string(),
    }))
}

/// 子任务列表（`GET /business/task/{id}/subtasks`）。
///
/// # 两份数据，口径要分清
///
/// - `list`：**按调用者可见性过滤**（与 T1 详情页左栏一致：看不见的子任务不下发）；
/// - `total` / `progress_locked` / `progress`：**按全部子任务算**（聚合口径不能被
///   「你看不见的那条」影响，否则同一个父任务在不同人眼里进度不一样）。
///
/// # 异常情况
///
/// - 404 + `63001`：父任务不存在 / 不可见；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/subtasks",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/subtasks")]
pub async fn list_subtasks(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskSubtaskListRes> {
    let mine = principal::resolve_for_claims(&claims).await?;
    let parent_id = parse_uuid_field(&id, "父任务")?;
    let parent = model::find_detail(&mine, parent_id)
        .await
        .map_err(|error| utils::db_error(error, "查询父任务"))?
        .ok_or_else(not_found)?;
    let visible = model::list_children(&mine, parent_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    let all = model::children_of(parent_id)
        .await
        .map_err(|error| utils::db_error(error, "查询子任务"))?;
    Ok(Res::ok(TaskSubtaskListRes {
        parent_id: parent_id.to_string(),
        parent_status: parent.card.status.clone(),
        progress: i32::from(parent.card.progress),
        progress_locked: state::progress_locked(&model::snapshots_of(&all)),
        total: all.len() as i64,
        list: visible.iter().map(card_res).collect(),
    }))
}

// ==================== 依赖 / 甘特图（T4） ====================

/// 依赖管理权的行级矩阵（与分派同一套 `ActorRole`）。
///
/// 口径（卡面「二、接口」的权限码 + 与 T3 一致的落地方式）：
/// 「负责人 / 创建人 / 分派人」天然可管依赖（他们本来就决定这条任务怎么做），
/// 其他人要 `business:task:dependency`（管理员 / 项目协调角色）。
fn may_manage_dependencies(role: ActorRole, has_permission: bool) -> bool {
    has_permission || !matches!(role, ActorRole::Outsider)
}

/// 依赖无权的 403（`63002`，与编辑 / 分派同一出口）。
fn forbidden_dependency() -> ApiError {
    ApiError::new(
        403,
        error_code::TASK_FORBIDDEN,
        "只有负责人 / 创建人 / 分派人（或持有 business:task:dependency 的角色）可以管理任务依赖",
    )
}

/// 依赖类型不合法 / 自依赖 / 父子互依赖 / 跨项目 的统一 400（`63103`）。
fn dependency_invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::TASK_DEPENDENCY_INVALID, message)
}

/// 事务内向 handler 传「环检测拒绝」的哨兵前缀。
///
/// `DbError` 只能携带字符串（`InvalidValue(String)`），而环拒绝要带**渲染好的路径**。
/// 用固定前缀把「业务拒绝（→ 400 + 63101）」与「真正的数据库异常（→ 500）」分开，
/// 避免为了传一个路径把整套错误类型改一遍。
const DEP_CYCLE_PREFIX: &str = "TASK_DEP_CYCLE:";

/// 依赖写操作的咨询锁 key（`pg_advisory_xact_lock`）。
///
/// 为什么需要：环检测是「读全量边 → 判断 → 插入」三步。两个并发请求各自读到
/// **不含对方**的边集合时都可能判定「不成环」，先后插入后却把圈闭上了。
/// 事务级咨询锁把这一段串行化 —— 依赖的写入频率远低于读取，串行化没有代价。
const DEP_ADVISORY_LOCK: i64 = 7_340_105;

/// 依赖行 → 出参（`show_blocking = true` 只对前置组有意义）。
fn dependency_item_res(row: &model::DependencyRow, show_blocking: bool) -> TaskDependencyItemRes {
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
async fn dependency_groups(
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
        prerequisites
            .iter()
            .map(|row| dependency_item_res(row, true))
            .collect(),
        successors
            .iter()
            .map(|row| dependency_item_res(row, false))
            .collect(),
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
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
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
struct DepWriteOutcome {
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
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
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
#[nik::permission(
    path = "/business/task/gantt",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/gantt")]
pub async fn gantt(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<TaskGanttQuery>,
) -> ResResult<TaskGanttRes> {
    let mine = principal::resolve_for_claims(&claims).await?;
    if query
        .board_id
        .as_deref()
        .is_some_and(|raw| !raw.trim().is_empty())
    {
        return Err(dependency_invalid(
            "board_id 维度属于 P1（自定义看板列），本版请用 project_id / assignee_id 或 scope=my"
                .to_string(),
        ));
    }
    let today = model::today_cst();
    let range = dep::resolve_range(query.start, query.end, today).map_err(|error| {
        ApiError::new(400, error_code::TASK_GANTT_RANGE_INVALID, error.message())
    })?;

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
            return Err(param_invalid(format!(
                "scope 取值非法：{raw}（应为 my / all）"
            )));
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
fn ensure_task_read_permission(claims: &TokenClaims) -> std::result::Result<(), ApiError> {
    if claims.has_permission(dto::PERM_TASK_READ) {
        return Ok(());
    }
    Err(ApiError::new(
        403,
        error_code::TASK_FORBIDDEN,
        "没有任务读取权限（business:task:read）",
    ))
}

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
    let author = utils::current_user_uuid(&claims).ok_or_else(|| {
        ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识")
    })?;
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
fn comment_type_verdict(raw: Option<&str>) -> std::result::Result<(), ApiError> {
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
async fn persist_comment(
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
        let notified = crate::business::task::collab::notify_mentions(
            task,
            &content,
            &targets,
            author,
            author_name,
        )
        .await;
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

/// 上传任务附件（`POST /business/task/{id}/attachment`，`multipart` 字段名 `file`）。
///
/// # 分类（老大 1A 的落点）
///
/// - 人类上传 → `category='task'`（50 MB，允许办公文档 / 压缩包）；
/// - AI 上传 → `category='task_ai'`（100 MB，白名单更严、禁可执行扩展名、禁直传）。
///
/// # 命名（E14）
///
/// AI 上传的文件名若没带本任务号，**自动加** `<任务号>-` 前缀
/// （`report.md` → `TSK-202610-000123-report.md`）；人类上传保持原名。
/// 理由：同一个 AI 处理多个任务时都产出 `report.md`，附件列表里全叫一个名字。
///
/// 两者的策略校验 / 秒传 / 落库都走
/// [`crate::business::storage::handler::store_server_upload`] —— 与存储模块的
/// 通用上传**逐字节同一套**，AI 因此拿不到比人类更宽的入口。
///
/// # 异常情况
///
/// - 401：无身份；403：人类无读权限；404：任务不存在 / 看不见；
/// - 400：没有 `file` 字段 / 扩展名或 MIME 不被策略允许 / **单任务附件已达 50 个**；
/// - 413：超过策略或服务端上限；500/502：写对象或落库失败。
#[nik::no_auth("/business/task/{id}/attachment")]
#[nik::post("/business/task/{id}/attachment")]
pub async fn upload_attachment(
    claims: Option<Extension<AuthClaims>>,
    ai_actor: Option<Extension<AiTaskActor>>,
    Path(id): Path<String>,
    headers: axum::http::HeaderMap,
    mut multipart: axum::extract::Multipart,
) -> ResResult<TaskUploadRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let reader = resolve_reader(claims, ai_actor)?;
    let me = reader.principal().await?;
    // E14：AI 的产出文件要按任务号命名，所以这里要把任务号留下（人类上传不用）。
    let task_no = model::find_detail(&me, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务"))?
        .ok_or_else(not_found)?
        .card
        .task_no;

    // T6b：单任务附件上限 50（卡面 §二 规则 2）。**读流之前**判 ——
    // 否则一个 50 MB 的文件会先被完整读进内存再被拒（白烧一次 IO 与内存）。
    let existing = crate::business::task::collab::count_attachments(task_id)
        .await
        .map_err(|error| utils::db_error(error, "统计任务附件"))?;
    if let Err(limit) = crate::business::task::collab::attachment_quota_verdict(existing) {
        return Err(ApiError::new(
            400,
            error_code::TASK_FIELD_INVALID,
            format!("单任务附件最多 {limit} 个，请先删除不再需要的附件"),
        ));
    }

    // 读流之前先用 Content-Length 判一次（框架 DefaultBodyLimit 会中途掐断 body，
    // 掐断时的错误信息是「读取失败」，很容易被误判成策略问题）
    let server_limit = storage::handler::server_upload_limit_bytes();
    if let Some(declared) = storage::handler::declared_content_length(&headers)
        && declared > server_limit
    {
        return Err(storage::handler::server_size_error(server_limit));
    }

    let (raw_name, mime_type, bytes) = read_single_upload(&mut multipart).await?;
    // E14：AI 产出文件名自动带本任务号前缀（`TSK-202610-000123-report.md`）。
    //
    // 只在 AI 侧做：人类附件叫什么叫人自己定，改名反而会破坏人的直觉。
    // 自动加而不是「只打 WARN」的理由见 [`hermes_sync::name_output_file`]。
    let file_name = if reader.is_ai() {
        let prefixed = hermes_sync::name_output_file(&task_no, &raw_name);
        if prefixed != raw_name {
            tracing::info!(
                task_no = %task_no,
                raw_name = %raw_name,
                file_name = %prefixed,
                "AI 产出文件名未带任务号，已自动加前缀"
            );
        }
        prefixed
    } else {
        raw_name
    };
    let category = if reader.is_ai() {
        hermes_sync::AI_UPLOAD_CATEGORY
    } else {
        HUMAN_UPLOAD_CATEGORY
    };
    let operator = reader.user_id();
    let record = storage::handler::store_server_upload(
        category,
        &file_name,
        &mime_type,
        &bytes,
        Some(operator),
    )
    .await?;

    // E14：秒传按「内容 hash」复用 `sys_file` 记录，同名不同内容不受影响；但同一份字节
    // 被第二个任务再传时，落到附件列表的仍是**首次上传**那个名字（记录是共用的）。
    // 只留痕不阻断 —— 改名意味着复制一条 sys_file（多一份存储），不值当。
    if reader.is_ai() && record.original_name.as_deref() != Some(file_name.as_str()) {
        tracing::warn!(
            task_no = %task_no,
            wanted = %file_name,
            kept = ?record.original_name,
            file_id = %record.id,
            "AI 产出命中内容秒传，沿用首次上传的文件名"
        );
    }

    let attachment_id = if reader.is_ai() {
        let profile = match &reader {
            TaskReader::Ai(actor) => actor.profile.clone(),
            TaskReader::Human(_) => None,
        };
        model::bind_ai_attachment(db(), task_id, record.id, profile.as_deref(), operator)
            .await
            .map_err(|error| utils::db_error(error, "绑定 AI 附件"))?
    } else {
        model::bind_human_attachment(db(), task_id, record.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "绑定任务附件"))?
    };

    tracing::info!(
        task_id = %task_id,
        file_id = %record.id,
        file_size = record.file_size,
        category,
        ai = reader.is_ai(),
        "任务附件已上传"
    );

    Ok(Res::ok(TaskUploadRes {
        attachment_id: attachment_id.to_string(),
        file_id: record.id.to_string(),
        file_name: record.original_name.clone().unwrap_or(file_name),
        file_size: record.file_size,
        source: if reader.is_ai() { "AI" } else { "HUMAN" }.to_string(),
        category: category.to_string(),
    }))
}

/// 人类上传任务附件用的分类（`V0107` 已种策略：50 MB）。
const HUMAN_UPLOAD_CATEGORY: &str = "task";

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
                model::insert_comment(
                    &tx,
                    task_id,
                    author,
                    &content,
                    model::COMMENT_TYPE_AI_PROGRESS,
                    author,
                )
                .await?;
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

    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
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
fn normalize_comment_content(
    raw: &str,
    limit: &CommentLimit,
) -> std::result::Result<String, ApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::TASK_FIELD_INVALID,
            "评论正文不能为空",
        ));
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
fn normalize_mentions(raw: &[String]) -> std::result::Result<Vec<Uuid>, ApiError> {
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

/// 从 multipart 里读**第一个**文件字段（表单字段名 `file`，允许其它字段混排）。
///
/// # 为什么自己读而不用存储模块的 `read_single_file`
///
/// 那个函数绑定在存储模块的策略（大小 / 扩展名）上，而任务附件必须先按
/// 「人类 / AI」选分类（`task` / `task_ai`）才能取策略。这里只负责「把字节读出来」，
/// 策略校验留给 `store_server_upload`（唯一实现）。
///
/// # 异常情况
///
/// - 400 + `10006`：没有文件字段 / 读取失败；
/// - 413：单字段超过服务端上限（框架 `DefaultBodyLimit` 兜底，这里提前给白话文案）。
async fn read_single_upload(
    multipart: &mut axum::extract::Multipart,
) -> std::result::Result<(String, String, Vec<u8>), ApiError> {
    let limit = storage::handler::server_upload_limit_bytes();
    while let Some(field) = multipart.next_field().await.map_err(|error| {
        ApiError::new(
            400,
            error_code::PAYLOAD_INVALID,
            format!("上传表单解析失败：{error}"),
        )
    })? {
        let name = field.name().unwrap_or_default().to_string();
        let file_name = field.file_name().map(|value| value.to_string());
        let mime = field
            .content_type()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());
        if name != "file" && file_name.is_none() {
            // 表单里的普通字段（如 category）直接跳过
            continue;
        }
        let bytes = field.bytes().await.map_err(|error| {
            ApiError::new(
                400,
                error_code::PAYLOAD_INVALID,
                format!("读取上传文件失败：{error}（单文件上限 {limit} 字节）"),
            )
        })?;
        if bytes.is_empty() {
            return Err(ApiError::new(
                400,
                error_code::PAYLOAD_INVALID,
                "上传的文件是空的".to_string(),
            ));
        }
        let file_name = file_name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "upload.bin".to_string());
        return Ok((file_name, mime, bytes.to_vec()));
    }
    Err(ApiError::new(
        400,
        error_code::PAYLOAD_INVALID,
        "上传表单里没有文件字段（字段名应为 file）".to_string(),
    ))
}

// ==================== 内部工具 ====================

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
fn parent_invalid(error: DbError) -> ApiError {
    ApiError::new(400, error_code::TASK_PARENT_INVALID, format!("{error}"))
}

/// 取任务（写路径用；不做可见性过滤）。
async fn require_task(id: Uuid) -> std::result::Result<TaskDetailRow, ApiError> {
    model::find_raw(id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务"))?
        .ok_or_else(not_found)
}

/// 标题归一化：折叠空白 + 非空 + ≤200 字符。
fn normalize_title(raw: &str) -> std::result::Result<String, ApiError> {
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
fn normalize_description(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
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
fn validate_hours(raw: Option<f64>) -> std::result::Result<Option<f64>, ApiError> {
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
fn card_res(row: &TaskCardRow) -> TaskCardRes {
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
fn save_res(row: &TaskDetailRow) -> TaskSaveRes {
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
fn normalize_reason(raw: Option<&str>) -> std::result::Result<Option<String>, ApiError> {
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
fn transition_check_error(error: state::TransitionCheckError) -> ApiError {
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
fn parent_cancelled() -> ApiError {
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
const PARENT_CANCELLED_SENTINEL: &str = "TASK_PARENT_CANCELLED";

/// 事务错误 → HTTP 错误：T11 守卫的哨兵翻成 `409 + 63021`，其余一律 500。
fn change_status_tx_error(error: DbError) -> ApiError {
    let blocked = matches!(&error, DbError::InvalidValue(msg) if msg == PARENT_CANCELLED_SENTINEL);
    if blocked {
        parent_cancelled()
    } else {
        utils::db_error(error, "变更任务状态")
    }
}

/// 状态机判定失败 → HTTP 错误（**三档**，不许统一成 500）。
///
/// - 打回缺原因 → `400 + 63013`（客户端补上就能成功）；
/// - 身份不够 → `403 + 63011`（换个有权限的人来做）；
/// - 其余（跳级 / 终态直取 / 同状态 / 需验收却直接完成）→ `409 + 63010`（状态冲突）。
fn transition_error(error: state::TransitionError) -> ApiError {
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
fn requested_progress(status: TaskStatus, requested: i32) -> std::result::Result<i16, ApiError> {
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
async fn check_dependencies_ready(task_id: Uuid) -> std::result::Result<(), ApiError> {
    let blocking = model::blocking_prerequisites(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询前置依赖"))?;
    if blocking.is_empty() {
        return Ok(());
    }
    let names = blocking
        .iter()
        .map(|row| {
            format!(
                "{}「{}」（{}）",
                row.task_no,
                row.title,
                status_label(&row.status)
            )
        })
        .collect::<Vec<_>>()
        .join("、");
    Err(ApiError::new(
        409,
        error_code::TASK_DEPENDENCY_BLOCKED,
        format!("前置任务未完成：{names}。等它们完成（或取消）后再开始；管理员可强开并留痕（T7）"),
    ))
}

/// 状态字面量 → 中文名（未知状态原样回显，不 panic）。
fn status_label(status: &str) -> String {
    TaskStatus::parse(status)
        .map(|parsed| parsed.label().to_string())
        .unwrap_or_else(|| status.to_string())
}

/// 一条任务通知的内容（把「收件人 / 强度 / 文案」打包，避免 8 个位置参数）。
///
/// 单独抽结构的直接原因：clippy `too_many_arguments`（8/7）在 `-D warnings` 下是硬错误；
/// 顺带的收益是调用点可读 —— `TaskNotice { recipients, force, .. }` 比 8 个裸参数
/// 更难把 `force` 和 `exclude_operator` 两个布尔写反（它们语义完全不同）。
struct TaskNotice<'a> {
    /// 候选收件人（会去掉操作人自己、排序去重）。
    recipients: Vec<Uuid>,
    /// `true` = 强制送达（跳过 `notify.inbox` 偏好）。
    force: bool,
    /// 是否剔除操作人自己（转派给原负责人的回执要传 `false`）。
    exclude_operator: bool,
    /// 标题。
    title: &'a str,
    /// 正文。
    content: &'a str,
}

/// 发一条任务站内信（**唯一的任务通知出口**）。
///
/// # 参数
///
/// - `task`：任务（取标题 / 编号 / 主键拼链接）；
/// - `recipients`：候选接收人（会自动去掉操作人自己、去重）；
/// - `force`：`true` 时**跳过** `notify.inbox` 偏好（修正 8 的强制分级）。
///   `false` 时关掉站内信的人不会落 `sys_notice_receiver`（过滤在
///   `notice::dispatch` 里做，本函数只传开关）；
/// - `exclude_operator`：是否剔除操作人自己（默认 `true` —— 「自己干的事不用通知自己」）。
///   **转派时给原负责人的那条回执必须传 `false`**：原负责人往往就是操作人，
///   而卡面明确要求「原负责人收到『任务已被转派』」（这是「你的活确实交出去了」的凭据，
///   不是噪音）；
///
/// - `title` / `content`：文案；
/// - `operator` / `operator_name`：发送人（通知里显示「谁干的」）。
///
/// # 返回值
///
/// **实际投递的目标**（已剔除操作人自己；通知失败时为空 `Vec`）。
/// 分派接口用它回填 `notified`，便于调用方确认「谁真的被告知了」。
///
/// # 失败口径
///
/// **通知失败不影响业务**：状态已经落库、评论已经写好，这里只记 WARN。
/// 把通知当强一致会让「通知服务抖一下 → 任务改不了」。
async fn dispatch_task_notice(
    task: &TaskDetailRow,
    notice: TaskNotice<'_>,
    operator: Uuid,
    operator_name: &str,
) -> Vec<Uuid> {
    let TaskNotice {
        recipients,
        force,
        exclude_operator,
        title,
        content,
    } = notice;
    let mut targets: Vec<Uuid> = recipients
        .into_iter()
        .filter(|user_id| !(exclude_operator && *user_id == operator))
        .collect();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return Vec::new();
    }
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        title,
        content,
        NoticeTarget::User(targets.clone()),
    );
    draft.link_url = Some(format!("/task/list?id={}", task.card.id));
    draft.biz_type = Some("task".to_string());
    draft.biz_id = Some(task.card.id.to_string());
    draft.sender_id = Some(operator);
    draft.sender_name = Some(operator_name.to_string());
    // 修正 8 的分级：**只管把 force 传下去**，偏好过滤的唯一实现在
    // `notice::dispatch` → `fanout::respecting_preference`（本卡把 T2 里那份
    // Rust 侧逐个 `inbox_enabled` 的判断删掉了：两份实现迟早不一致）。
    draft.force = force;
    match notice::dispatch(draft).await {
        Ok(result) => {
            tracing::info!(
                notice_id = %result.notice_id,
                receivers = result.receiver_count,
                force,
                "任务通知已发送"
            );
            targets
        }
        Err(error) => {
            tracing::warn!(error = %error, force, "任务通知发送失败（不影响业务）");
            Vec::new()
        }
    }
}

/// 状态流转通知（修正 8 分级表，逐行实现）。
///
/// | 事件 | 接收人 | 强制 |
/// |---|---|---|
/// | 被打回（`REVIEW → DOING`） | 负责人 | ✅ |
/// | 待你验收（`→ REVIEW`） | 创建人 + 分派人 | ✅ |
/// | 其它状态变更（他人操作） | 负责人 + 创建人 | ❌ 尊重 `notify.inbox` |
async fn notify_status_change(
    task: &TaskDetailRow,
    from: TaskStatus,
    to: TaskStatus,
    operator: Uuid,
    operator_name: &str,
    reason: Option<&str>,
) {
    let (recipients, force, title, content) = match (from, to) {
        (TaskStatus::Review, TaskStatus::Doing) => (
            vec![task.card.owner_id],
            true,
            "任务被打回".to_string(),
            format!(
                "任务「{}」（{}）被打回：{}；改完请重新提交验收",
                task.card.title,
                task.card.task_no,
                reason.unwrap_or("未填写原因")
            ),
        ),
        (_, TaskStatus::Review) => {
            let mut recipients = vec![task.creator_id];
            if let Some(assigner_id) = task.assigner_id {
                recipients.push(assigner_id);
            }
            (
                recipients,
                true,
                "任务待你验收".to_string(),
                format!(
                    "任务「{}」（{}）已由 {operator_name} 提交验收，请确认",
                    task.card.title, task.card.task_no
                ),
            )
        }
        _ => (
            vec![task.card.owner_id, task.creator_id],
            false,
            "任务状态变更".to_string(),
            format!(
                "任务「{}」（{}）由 {operator_name} 从「{}」改为「{}」",
                task.card.title,
                task.card.task_no,
                from.label(),
                to.label()
            ),
        ),
    };
    dispatch_task_notice(
        task,
        TaskNotice {
            recipients,
            force,
            exclude_operator: true,
            title: &title,
            content: &content,
        },
        operator,
        operator_name,
    )
    .await;
}

/// 父任务「子任务全部完成」通知（尊重偏好，修正 8）。
///
/// 只在**这次重算把父任务推到 100%** 时发一次（`old != 100 && new == 100`），
/// 避免同一次批量流转里对同一个父任务重复通知。
async fn notify_ready_to_close(
    recalcs: &[model::ProgressRecalc],
    operator: Uuid,
    operator_name: &str,
) {
    for recalc in recalcs {
        if !recalc.all_done || recalc.new != 100 || recalc.old == 100 {
            continue;
        }
        let Ok(Some(parent)) = model::find_raw(recalc.task_id).await else {
            continue;
        };
        let content = format!(
            "任务「{}」（{}）的子任务已全部完成，进度汇总为 100%，可以结项了",
            parent.card.title, parent.card.task_no
        );
        dispatch_task_notice(
            &parent,
            TaskNotice {
                recipients: vec![parent.card.owner_id],
                force: false,
                exclude_operator: true,
                title: "子任务全部完成",
                content: &content,
            },
            operator,
            operator_name,
        )
        .await;
    }
}

/// 前置完成 / 取消 → 通知**后继负责人**（T4 联动，核心设计 §四「运行时约束 4」+ §九通知表）。
///
/// # 分级（修正 8）
///
/// | 事件 | 收件人 | `force` |
/// |---|---|---|
/// | 前置 `DONE`（解开阻塞） | 每个后继的负责人 | `true`（**强制**：这是「可以开工了」的信号，不能被偏好静默丢弃） |
/// | 前置 `CANCELLED` | 每个后继的负责人 | `false`（噪音档：前置取消只是提醒你调整计划） |
///
/// # 口径
///
/// - 只通知**还没结束**（非 `DONE` / `CANCELLED`）的后继：给已完成的任务发「可以开始了」是纯噪音；
/// - 每个后继发**一条**站内信（链到后继任务，不是前置任务 —— 收件人要处理的是自己那条）；
/// - 通知失败只记 WARN，不影响业务（与其它任务通知同一口径）。
async fn notify_dependents(
    task: &TaskDetailRow,
    to: TaskStatus,
    operator: Uuid,
    operator_name: &str,
) {
    let successors = match model::list_successors(task.card.id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, task_id = %task.card.id, "查询后继任务失败，跳过依赖联动通知");
            return;
        }
    };
    for successor in successors {
        if !dep::is_blocking_status(&successor.status) {
            continue; // 后继已结束（DONE / CANCELLED），没有必要提醒
        }
        let Ok(Some(target)) = model::find_raw(successor.peer_id).await else {
            continue;
        };
        let (title, content, force) = match to {
            TaskStatus::Done => (
                "前置任务已完成，可以开始了",
                format!(
                    "前置任务「{}」（{}）已完成，任务「{}」（{}）可以开始了",
                    task.card.title, task.card.task_no, target.card.title, target.card.task_no
                ),
                true,
            ),
            _ => (
                "前置任务已取消",
                format!(
                    "前置任务「{}」（{}）已取消 —— 该前置不再阻塞你，但你可能需要调整任务「{}」（{}）的计划",
                    task.card.title, task.card.task_no, target.card.title, target.card.task_no
                ),
                false,
            ),
        };
        dispatch_task_notice(
            &target,
            TaskNotice {
                recipients: vec![target.card.owner_id],
                force,
                exclude_operator: true,
                title,
                content: &content,
            },
            operator,
            operator_name,
        )
        .await;
    }
}

// ==================== 分派 / 转派 / 协作人（T3） ====================

/// 当前账号与任务的关系（分派 / 协作人的权限矩阵只看它 + 权限码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorRole {
    /// 创建人。
    Creator,
    /// 当前负责人。
    Owner,
    /// 分派人。
    Assigner,
    /// 与任务没有行级关系（只能靠权限码 / 超管）。
    Outsider,
}

/// 身份判定（纯函数，单测覆盖全矩阵）。
///
/// 优先级：创建人 > 负责人 > 分派人。一个人同时是「创建人 + 负责人」时按创建人算
/// —— 与 `state::TaskPeople::role_mask` 的位掩码口径一致（那边是全选，这边取最左侧）。
fn actor_role(me: Uuid, creator_id: Uuid, owner_id: Uuid, assigner_id: Option<Uuid>) -> ActorRole {
    if me == creator_id {
        ActorRole::Creator
    } else if me == owner_id {
        ActorRole::Owner
    } else if assigner_id == Some(me) {
        ActorRole::Assigner
    } else {
        ActorRole::Outsider
    }
}

/// 分派 / 转派是否放行：**行级关系（创建人 / 负责人 / 分派人）∪ 权限码**。
///
/// # 为什么不硬拒 `user_type = ROBOT`（修正 11）
///
/// 分派限制**只查权限码**：`ROBOT` 账号没有 `business:task:assign` 时自然被挡
/// （它的行级关系通常也不成立）；一旦管理员把它加进带该权限码的角色（如
/// 「AI 团队主管」），它就**能**分派 —— 「默认不能、可按角色授予」因此自然成立。
/// 在这里按 `user_type` 硬拒，等于把这条路彻底焊死，与修正 11 直接冲突。
fn may_assign(role: ActorRole, has_permission: bool) -> bool {
    has_permission || role != ActorRole::Outsider
}

/// 协作人管理是否放行（矩阵与分派相同，权限码换成 `business:task:member`）。
fn may_manage_members(role: ActorRole, has_permission: bool) -> bool {
    has_permission || role != ActorRole::Outsider
}

/// 转派后落库的 `assigner_id`（单测覆盖）。
///
/// 口径（卡面 §二 规则 1）：**操作人非创建人时记操作人**；创建人自己派活时沿用原值
/// —— 创建人本来就是这条任务的源头，把 `assigner_id` 指向他反而让「谁派的」变成
/// 冗余信息（`creator_id` 已经说明了一切）。
fn next_assigner(operator: Uuid, creator_id: Uuid, current: Option<Uuid>) -> Option<Uuid> {
    if operator == creator_id {
        current
    } else {
        Some(operator)
    }
}

/// 创建人 / 负责人是**受保护的协作人**：不许从协作人列表里移除。
///
/// 他们是可见性谓词的前两个分支，移除协作人行并不能让他们看不见任务，
/// 只会让列表少一个人 —— 要么无意义，要么是误操作（卡面 §三 规则 3）。
fn protected_member(user_id: Uuid, creator_id: Uuid, owner_id: Uuid) -> bool {
    user_id == creator_id || user_id == owner_id
}

/// 当前账号的 `sys_user.user_type`（`HUMAN` / `ROBOT`；读不到就是 `None`）。
///
/// 判定统一交给 [`crate::shared::ai_actor`]：本模块只负责「把类型读出来」，
/// 不自己写 `== "ROBOT"`（四处入口各写一遍，文案与状态码必然漂移）。
async fn user_type_of(user_id: Uuid) -> std::result::Result<Option<String>, ApiError> {
    model::user_type_of(user_id)
        .await
        .map_err(|error| utils::db_error(error, "查询账号类型"))
}

/// 当前账号是不是 AI 用户（`sys_user.user_type = 'ROBOT'`）。
///
/// **只用于文案与「硬拒」**（删任务），不参与分派 / 协作人的准入判定（修正 11）。
async fn is_robot(user_id: Uuid) -> std::result::Result<bool, ApiError> {
    Ok(ai_actor::is_robot(user_type_of(user_id).await?.as_deref()))
}

/// 分派被拒 → 403（AI 账号给「怎么才能有权限」的文案，人给的文案列出四条通路）。
fn forbidden_assign(is_robot: bool) -> ApiError {
    if is_robot {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "AI 用户默认不能分派任务，如需请授予角色权限（business:task:assign）",
        )
    } else {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的创建人、当前负责人、分派人，或持有 business:task:assign 权限的人可以分派 / 转派",
        )
    }
}

/// 协作人管理被拒 → 403（文案同上，权限码换成 `business:task:member`）。
fn forbidden_member(is_robot: bool) -> ApiError {
    if is_robot {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "AI 用户默认不能管理任务协作人，如需请授予角色权限（business:task:member）",
        )
    } else {
        ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "只有任务的创建人、当前负责人、分派人，或持有 business:task:member 权限的人可以管理协作人",
        )
    }
}

/// 加协作人的上限判定（纯函数，单测覆盖边界：50 允许、51 拒绝）。
///
/// # 为什么按「现有 + 真正新增」算
///
/// 幂等重复提交不该占额度：一个 50 人的任务再提交「把这 50 人加一遍」，实际
/// 新增 0 人，不该报上限错误（前端整表提交时会经常发生）。
fn ensure_member_quota(existing: usize, adding: usize) -> std::result::Result<(), ApiError> {
    let total = existing as i64 + adding as i64;
    if total > dto::MAX_MEMBERS_PER_TASK {
        return Err(ApiError::new(
            400,
            error_code::TASK_MEMBER_LIMIT,
            format!(
                "单个任务最多 {} 个协作人（现有 {existing} 人，本次新增 {adding} 人 = {total} 人）",
                dto::MAX_MEMBERS_PER_TASK
            ),
        ));
    }
    Ok(())
}

/// 协作人行 → 出参。
fn member_res(row: &model::TaskMemberRow) -> TaskMemberRes {
    TaskMemberRes {
        user_id: row.user_id.to_string(),
        user_name: row.user_name.clone(),
        role: row.role.clone(),
        created_at: row.created_at,
    }
}

/// 解析并去重协作人 id 列表（空列表 → 400）。
fn parse_user_ids(raw: &[String]) -> std::result::Result<Vec<Uuid>, ApiError> {
    let mut ids: Vec<Uuid> = Vec::with_capacity(raw.len());
    for item in raw {
        let text = item.trim();
        if text.is_empty() {
            continue;
        }
        let id = parse_uuid_field(text, "协作人")?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(param_invalid("user_ids 不能为空".to_string()));
    }
    Ok(ids)
}

/// 分派 / 转派（`PATCH /business/task/{id}/assign`）。
///
/// # 谁能分派（矩阵见 [`may_assign`]）
///
/// | 身份 | 能否 |
/// |------|------|
/// | 创建人 | ✅ |
/// | 当前负责人（转派给别人） | ✅ |
/// | 分派人 | ✅ |
/// | 持有 `business:task:assign`（含超管） | ✅ |
/// | 其他人 | ❌ 403 |
///
/// # 一次分派改五样东西（同一事务）
///
/// ① `owner_id` + `dept_id` 快照（不换 dept 会让「原部门看板」继续显示这条任务）；
/// ② `assigner_id`（操作人非创建人时 = 操作人）；③ `source = TRANSFERRED`；
/// ④ **原负责人自动成为协作人**（否则他一转派就再也看不见这条任务了）；
/// ⑤ 一条 `ASSIGN` 评论 + 一条变更日志（`owner_id / assigner_id / source` 三列）。
///
/// # 通知是**强制**的（修正 8）
///
/// 新负责人「你被分派了任务」、原负责人「任务已被转派」两条都 `force = true`：
/// 收件人把 `notify.inbox` 关掉也照发（漏掉 = 没人干活，是直接损失）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 / 请求里的 UUID 非法；400 + `63017`：新负责人=当前负责人；
/// - 400 + `63009`：新负责人不存在或已停用；
/// - 403 + `63002`：无权分派（AI 账号会拿到「请授予角色权限」的文案）；
/// - 404 + `63001`：任务不存在 / 已软删；
/// - 409 + `63018`：并发转派（负责人已被别人改掉）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/assign",
    method = "PATCH",
    permission = "business:task:read"
)]
#[nik::patch("/business/task/{id}/assign")]
pub async fn assign(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskAssignReq>,
) -> ResResult<TaskAssignRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    // 分派属写操作：沿用删除接口的 403 口径（不用可见性的 404），
    // 否则「看不见任务的人来分派」会得到 404，验收要求的是 403。
    let current = require_task(task_id).await?;

    // ① 权限矩阵：行级关系 ∪ 权限码
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
    if !may_assign(role, claims.has_permission(dto::PERM_ASSIGN)) {
        return Err(forbidden_assign(is_robot(me).await?));
    }

    // ② 新负责人：合法 uuid + 有效用户（ROBOT **可以**被分派：AI 是执行者）
    let new_owner = parse_uuid_field(req.owner_id.trim(), "新负责人")?;
    if new_owner == current.card.owner_id {
        return Err(ApiError::new(
            400,
            error_code::TASK_ASSIGN_SAME_OWNER,
            "新负责人与当前负责人相同，无需分派 / 转派",
        ));
    }
    if !model::user_exists(new_owner)
        .await
        .map_err(|error| utils::db_error(error, "校验新负责人"))?
    {
        return Err(ApiError::new(
            400,
            error_code::TASK_OWNER_INVALID,
            "新负责人不存在或已停用（已删除 / status≠1 的账号不能被分派）",
        ));
    }
    let dept_id = model::user_dept(new_owner)
        .await
        .map_err(|error| utils::db_error(error, "查询新负责人所属部门"))?;
    let new_owner_name = model::user_display_name(new_owner)
        .await
        .map_err(|error| utils::db_error(error, "查询新负责人姓名"))?;
    let reason = normalize_reason(req.reason.as_deref())?;

    let old_owner_id = current.card.owner_id;
    let old_owner_name = current.card.owner_name.clone();
    let assigner_id = next_assigner(me, current.creator_id, current.assigner_id);
    let operator_name = display_name(&claims);

    // ③ 评论正文（详情页时间线：「谁把活从谁手里拿给了谁」）
    let mut content = format!("分派：负责人由 {old_owner_name} 改为 {new_owner_name}");
    if let Some(reason) = reason.as_deref() {
        content.push_str(&format!("；原因：{reason}"));
    }

    // ④ 一个事务：改负责人 → 原负责人入库协作人 → ASSIGN 评论 → 变更日志
    let change = model::OwnerChange {
        task_id,
        owner_id: new_owner,
        dept_id,
        assigner_id,
        source: dto::SOURCE_TRANSFERRED.to_string(),
        operator: me,
        expected_owner: old_owner_id,
    };
    let before = serde_json::json!({
        "owner_id": old_owner_id.to_string(),
        "assigner_id": current.assigner_id.map(|id| id.to_string()),
        "source": current.card.source,
    });
    let after = serde_json::json!({
        "owner_id": new_owner.to_string(),
        "assigner_id": assigner_id.map(|id| id.to_string()),
        "source": dto::SOURCE_TRANSFERRED,
    });
    let operator_name_tx = operator_name.clone();
    let (comment_id, affected, auto_member) = db()
        .transaction(|tx| async move {
            let affected = model::assign_owner(&tx, &change).await?;
            if affected == 0 {
                return Ok((Uuid::nil(), 0_u64, false));
            }
            let auto_member = old_owner_id != new_owner;
            if auto_member {
                model::add_member(
                    &tx,
                    task_id,
                    old_owner_id,
                    dto::MEMBER_ROLE_COLLABORATOR,
                    me,
                )
                .await?;
            }
            let comment_id =
                model::insert_comment(&tx, task_id, me, &content, model::COMMENT_TYPE_ASSIGN, me)
                    .await?;
            changelog::record(
                &tx,
                &ChangeCtx::api(Some(me), Some(&operator_name_tx)),
                ChangeTarget::of(
                    "biz_task",
                    task_id.to_string(),
                    Action::Assign,
                    "分派 / 转派",
                ),
                DiffInput::update(&before, &after),
            )
            .await
            .map_err(|error| {
                DbError::InvalidValue(format!("变更日志写入失败 [{}] {}", error.code, error.msg))
            })?;
            Ok((comment_id, affected, auto_member))
        })
        .await
        .map_err(|error| utils::db_error(error, "分派任务"))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::TASK_ASSIGN_CONFLICT,
            "任务负责人已被他人修改（或任务已删除），请刷新后重试",
        ));
    }

    // ⑤ 通知（**强制**：force = true，偏好在服务端被绕过）。事务外发：通知失败不回滚业务。
    let updated = require_task(task_id).await?;
    let mut notified: Vec<Uuid> = Vec::new();
    notified.extend(
        dispatch_task_notice(
            &updated,
            TaskNotice {
                recipients: vec![new_owner],
                force: true,
                exclude_operator: true,
                title: "你被分派了任务",
                content: &format!(
                    "任务「{}」（{}）已由 {operator_name} 分派给你，请打开任务详情确认",
                    updated.card.title, updated.card.task_no
                ),
            },
            me,
            &operator_name,
        )
        .await,
    );
    if old_owner_id != new_owner {
        notified.extend(
            dispatch_task_notice(
                &updated,
                TaskNotice {
                    recipients: vec![old_owner_id],
                    force: true,
                    // 原负责人常常就是操作人：这条回执**必须**发给他（卡面 §二 规则 4）
                    exclude_operator: false,
                    title: "任务已被转派",
                    content: &format!(
                        "任务「{}」（{}）已由 {operator_name} 转派给 {new_owner_name}；你已自动成为协作人，仍可查看进度",
                        updated.card.title, updated.card.task_no
                    ),
                },
                me,
                &operator_name,
            )
            .await,
        );
    }
    notified.sort();
    notified.dedup();

    tracing::info!(
        task_id = %task_id,
        from = %old_owner_id,
        to = %new_owner,
        operator = %me,
        auto_member,
        "任务已分派 / 转派"
    );
    Ok(Res::ok(TaskAssignRes {
        id: task_id.to_string(),
        task_no: updated.card.task_no.clone(),
        owner_id: new_owner.to_string(),
        previous_owner_id: old_owner_id.to_string(),
        assigner_id: assigner_id.map(|id| id.to_string()),
        source: dto::SOURCE_TRANSFERRED.to_string(),
        auto_member,
        comment_id: comment_id.to_string(),
        notified: notified.iter().map(|id| id.to_string()).collect(),
    }))
}

/// 加协作人（`POST /business/task/{id}/members`）。
///
/// # 规则
///
/// 1. 谁能加：创建人 / 负责人 / 分派人 ∪ `business:task:member`（矩阵同分派）；
/// 2. 协作人**自动获得可见性**（`biz_task_member` 已在可见性 id 数组里，T1）；
/// 3. **创建人 / 负责人不入协作人行**（他们本来就能看，落行只会让列表虚胖）——
///    请求里带了他们不算错：进回执的 `skipped`；
/// 4. **幂等**：已经在里头的进 `existing`，不报错、不重复插；
/// 5. 上限 **50 人**（现有 + 本次新增，超了 400 + `63014`）；
/// 6. 强制通知（`force = true`）：被加的人一定收到。
///
/// # 异常情况
///
/// - 400 + `10001`：uuid 非法 / `user_ids` 为空；400 + `63019`：含无效用户；
/// - 400 + `63014`：超过 50 人上限；
/// - 403 + `63002`：无权管理协作人；404 + `63001`：任务不存在 / 已软删；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/members",
    method = "POST",
    permission = "business:task:read"
)]
#[nik::post("/business/task/{id}/members")]
pub async fn add_members(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<TaskMemberAddReq>,
) -> ResResult<TaskMemberAddRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let current = require_task(task_id).await?;
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
    if !may_manage_members(role, claims.has_permission(dto::PERM_MEMBER)) {
        return Err(forbidden_member(is_robot(me).await?));
    }

    let wanted = parse_user_ids(&req.user_ids)?;

    // ① 一次性校验有效性（一条 SQL 取出有效 id，差集就是不能加的人）
    let valid = model::valid_users(&wanted)
        .await
        .map_err(|error| utils::db_error(error, "校验协作人"))?;
    let invalid: Vec<String> = wanted
        .iter()
        .filter(|id| !valid.contains(id))
        .map(|id| id.to_string())
        .collect();
    if !invalid.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::TASK_MEMBER_INVALID,
            format!(
                "以下用户不存在或已停用，不能加为协作人：{}",
                invalid.join(", ")
            ),
        ));
    }

    // ② 创建人 / 负责人跳过；其余分「已存在」与「待新增」
    let existing_ids = model::member_ids(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let mut skipped: Vec<Uuid> = Vec::new();
    let mut existing: Vec<Uuid> = Vec::new();
    let mut to_add: Vec<Uuid> = Vec::new();
    for id in &wanted {
        if protected_member(*id, current.creator_id, current.card.owner_id) {
            skipped.push(*id);
        } else if existing_ids.contains(id) {
            existing.push(*id);
        } else {
            to_add.push(*id);
        }
    }

    // ③ 上限：现有 + 本次新增 ≤ 50（按「去重后真正会新增的人数」算，幂等重复不加人）
    ensure_member_quota(existing_ids.len(), to_add.len())?;

    // ④ 集合式入库（一条 INSERT ... SELECT，返回真正新增的 id）
    let added = if to_add.is_empty() {
        Vec::new()
    } else {
        let ids = to_add.clone();
        db().transaction(|tx| async move { model::insert_members(&tx, task_id, &ids, me).await })
            .await
            .map_err(|error| utils::db_error(error, "加任务协作人"))?
    };

    // ⑤ 通知（强制：协作关系是「你被拉进来了」，不能因为偏好静默丢失）
    if !added.is_empty() {
        let content = format!(
            "任务「{}」（{}）已把你加为协作人，你可以查看进度、评论并收到后续通知",
            current.card.title, current.card.task_no
        );
        dispatch_task_notice(
            &current,
            TaskNotice {
                recipients: added.clone(),
                force: true,
                exclude_operator: true,
                title: "你被加为任务协作人",
                content: &content,
            },
            me,
            &display_name(&claims),
        )
        .await;
    }

    let members = model::list_members(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let total = members.len() as i64;
    tracing::info!(task_id = %task_id, added = added.len(), existing = existing.len(), skipped = skipped.len(), "任务协作人已添加");
    Ok(Res::ok(TaskMemberAddRes {
        task_id: task_id.to_string(),
        added: added.iter().map(|id| id.to_string()).collect(),
        existing: existing.iter().map(|id| id.to_string()).collect(),
        skipped: skipped.iter().map(|id| id.to_string()).collect(),
        list: members.iter().map(member_res).collect(),
        total,
        limit: dto::MAX_MEMBERS_PER_TASK,
    }))
}

/// 移除协作人（`DELETE /business/task/{id}/members/{user_id}`）。
///
/// # 规则
///
/// - 权限同「加协作人」；
/// - **创建人 / 负责人不许移除**（400 + `63015`）：他们本来就能看，「移除」只会
///   造成「列表里没有他、但他照样能改任务」的错觉；
/// - 不是协作人 → 404 + `63020`（不静默成功：否则前端会以为移除生效了）；
/// - 通知**尊重偏好**（`force = false`，修正 8 的噪音档）。
///
/// # 异常情况
///
/// - 400 + `10001`：uuid 非法；400 + `63015`：目标是创建人 / 负责人；
/// - 403 + `63002`：无权管理协作人；404 + `63001` / `63020`：任务 / 协作关系不存在；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/members/{user_id}",
    method = "DELETE",
    permission = "business:task:read"
)]
#[nik::delete("/business/task/{id}/members/{user_id}")]
pub async fn remove_member(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, user_id)): Path<(String, String)>,
) -> ResResult<TaskMemberRemoveRes> {
    let me = current_user(&claims)?;
    let task_id = parse_uuid_field(&id, "任务")?;
    let target = parse_uuid_field(&user_id, "协作人")?;
    let current = require_task(task_id).await?;
    let role = actor_role(
        me,
        current.creator_id,
        current.card.owner_id,
        current.assigner_id,
    );
    if !may_manage_members(role, claims.has_permission(dto::PERM_MEMBER)) {
        return Err(forbidden_member(is_robot(me).await?));
    }
    if protected_member(target, current.creator_id, current.card.owner_id) {
        return Err(ApiError::new(
            400,
            error_code::TASK_MEMBER_PROTECTED,
            "创建人和负责人本来就是任务的可见方，不能从协作人里移除（要换人请用分派 / 转派）",
        ));
    }

    let removed = db()
        .transaction(|tx| async move { model::remove_member(&tx, task_id, target).await })
        .await
        .map_err(|error| utils::db_error(error, "移除任务协作人"))?;
    if removed == 0 {
        return Err(ApiError::new(
            404,
            error_code::TASK_MEMBER_NOT_FOUND,
            "该用户不是本任务的协作人",
        ));
    }

    // 尊重偏好（force = false）：这是噪音档通知，收件人关了站内信就不发。
    let content = format!(
        "任务「{}」（{}）已把你从协作人中移除，你将不再收到该任务的更新",
        current.card.title, current.card.task_no
    );
    dispatch_task_notice(
        &current,
        TaskNotice {
            recipients: vec![target],
            force: false,
            exclude_operator: true,
            title: "你已从任务协作人中移除",
            content: &content,
        },
        me,
        &display_name(&claims),
    )
    .await;

    let members = model::list_members(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let total = members.len() as i64;
    tracing::info!(task_id = %task_id, user_id = %target, operator = %me, "任务协作人已移除");
    Ok(Res::ok(TaskMemberRemoveRes {
        task_id: task_id.to_string(),
        user_id: target.to_string(),
        removed: true,
        total,
    }))
}

/// 协作人列表（`GET /business/task/{id}/members`）。
///
/// # 可见性
///
/// 走 [`model::find_detail`]（数据范围 ∪ 参与关系）：看不见的任务一律 **404**
/// —— 协作人列表会暴露「这条任务上都有谁」，不能给看不见任务的人看。
///
/// # 异常情况
///
/// - 400 + `10001`：uuid 非法；404 + `63001`：任务不存在 / 不可见；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/members",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/members")]
pub async fn members(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskMembersRes> {
    let mine = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
    model::find_detail(&mine, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let rows = model::list_members(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务协作人"))?;
    let total = rows.len() as i64;
    Ok(Res::ok(TaskMembersRes {
        task_id: task_id.to_string(),
        list: rows.iter().map(member_res).collect(),
        total,
        limit: dto::MAX_MEMBERS_PER_TASK,
    }))
}

// 写路径的权限判断口径（说明）：
//
// 编辑用 `find_raw` + 「我是不是负责人/创建人」，不碰数据范围 —— 协作人虽然
// *能看见* 任务，但 **不能改**（可见性 ≠ 写权限）。只有删除需要判「超管」，
// 那里才解析 `ScopedPrincipal`。

// ==================== 单元测试（纯逻辑） ====================

// ==================== 流程来源（P-FLOW-1A） ====================

/// 任务所属流程（`GET /business/task/{id}/flow`）。
///
/// 任务详情页的「所属审批单」卡片用它：单号 / 流程名 / 发起人 / 当前节点 / 实例状态。
///
/// # 手工任务也 200
///
/// `source != 'FLOW'` 的任务 `flow` 字段为 `null`，**不是 404**：
/// 「这张任务没有流程」是正常事实（两秒建任务的人不会因此看到一个错误弹窗）。
///
/// # 可见性
///
/// 与详情接口**同一套**（`数据范围 ∪ 参与关系`）：看不见的任务一律 404，不暴露「存在但无权」。
///
/// # 异常情况
///
/// - 400 + `10001`：路径里的 id 不是 UUID；
/// - 404 + `63001`：任务不存在 / 已软删 / 不在可见范围内；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/flow",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/{id}/flow")]
pub async fn flow_of_task(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::TaskFlowRes> {
    let task_id = parse_uuid_field(&id, "任务")?;
    let me = principal::resolve_for_claims(&claims).await?;
    // 可见性口径与详情接口一致：看不见的任务一律 404（不暴露「存在但无权」）。
    let row = model::find_detail(&me, task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务详情"))?
        .ok_or_else(not_found)?;
    let flow = model::flow_ref_of_task(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务所属流程"))?;
    Ok(Res::ok(dto::TaskFlowRes {
        task_id: task_id.to_string(),
        task_no: row.card.task_no.clone(),
        flow: flow.map(|flow| dto::TaskFlowDetailRes {
            instance_id: flow.instance_id.to_string(),
            instance_no: flow.instance_no,
            flow_code: flow.flow_code,
            flow_name: flow.flow_name,
            biz_type: flow.biz_type,
            biz_no: flow.biz_no.unwrap_or_default(),
            instance_status: flow.instance_status,
            initiator_id: flow.initiator_id.to_string(),
            initiator_name: flow.initiator_name,
            started_at: flow.started_at,
            ended_at: flow.ended_at,
            node_key: flow.node_key,
            node_name: flow.node_name,
            round: flow.round,
            node_status: flow.node_status,
        }),
    }))
}

// ==================== 统计（T5b · b4：exec_mode 维度） ====================

/// 按执行模式统计（`GET /business/task/stats`，**人类会话调用**）。
///
/// # 为什么要单开一个接口，而不是在 `page` 上加 `group_by`
///
/// 「人类 / AI 分别统计完成率与平均耗时」是**聚合**语义：它要跨全部可见任务做
/// `GROUP BY exec_mode`，而分页接口每页最多 200 行，前端自己算出来的完成率只代表
/// 当前页。聚合必须在服务端做，且必须走同一条可见性谓词（否则「我的完成率」会被
/// 别人的任务稀释）。
///
/// # 出参口径
///
/// - `by_exec_mode` **恒含 `HUMAN` 与 `AI` 两行**，窗口内没有任务就给全 0；
/// - `total` 是合计行：计数直接相加，**平均耗时按已完成数加权、平均进度按有效任务数加权**
///   （见 [`stats_total`]）；
/// - 窗口按**创建时间**的 CST 日区间，默认最近 30 天（含今天），上限 366 天。
///
/// # 权限
///
/// 与列表 / 甘特同一套：`business:task:read` 决定能否进页面，
/// `scope ∪ 参与关系` 决定能统计到哪些任务。
///
/// # 异常情况
///
/// - 400 + `10001`：日期 / uuid / scope 取值非法，或窗口倒置 / 超过 366 天；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/stats",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/stats")]
pub async fn stats(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::TaskStatsQuery>,
) -> ResResult<dto::TaskStatsRes> {
    let principal = principal::resolve_for_claims(&claims).await?;
    let end = query.end.unwrap_or_else(model::today_cst);
    let start = query
        .start
        .unwrap_or_else(|| model::stats_default_window(end).0);
    if let Some(message) = model::stats_window_error(start, end) {
        return Err(param_invalid(message));
    }
    let project_id = match query.project_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(parse_uuid_field(raw, "项目")?),
    };
    let mine = match query.scope.as_deref().map(str::trim) {
        None | Some("") | Some("all") => None,
        Some("my") => Some(principal.user_id()),
        Some(raw) => {
            return Err(param_invalid(format!(
                "scope 取值非法：{raw}（应为 my / all）"
            )));
        }
    };

    let rows = model::task_stats(
        &principal,
        &model::StatsFilter {
            start,
            end,
            project_id,
            mine,
        },
    )
    .await
    .map_err(|error| utils::db_error(error, "统计任务"))?;

    // 固定两行（HUMAN / AI）——前端的对比卡片是固定两列
    let mut by_exec_mode: Vec<dto::ExecModeStatRes> = Vec::with_capacity(2);
    for mode in ["HUMAN", "AI"] {
        let row = rows.iter().find(|item| item.exec_mode == mode);
        by_exec_mode.push(stats_row(mode, row));
    }
    // 兜底：库里出现第三种 exec_mode（历史脏数据）时如实列出，不静默吞掉
    for row in &rows {
        if !matches!(row.exec_mode.as_str(), "HUMAN" | "AI") {
            by_exec_mode.push(stats_row(&row.exec_mode, Some(row)));
        }
    }
    let total = stats_total(&by_exec_mode);

    tracing::info!(
        start = %start,
        end = %end,
        modes = by_exec_mode.len(),
        total_tasks = total.total,
        "任务统计已生成（按执行模式）"
    );
    Ok(Res::ok(dto::TaskStatsRes {
        start,
        end,
        total,
        by_exec_mode,
    }))
}

/// 父任务状态巡检 / 兜底对账（T13）。
///
/// # 为什么需要这条接口
///
/// 审批通过的 `REVIEW → DONE` 走的是 `flow::task_bridge::settle`，它跑在**审批事务
/// 提交之后**，所以它里面的父任务级联只能开独立事务 ⇒ 极端失败（那一刻库故障 /
/// 进程被杀 / 并发冲突）会留下「子任务全终态、父任务停在 `DOING`」的不一致
/// （老六真机验收实测就是这个形态）。本接口把这类不一致扫出来并按**同一条规则**
/// 修回去（修复起点是子任务，跑的是生产同款 `cascade_parent_status`）。
///
/// # 权限
///
/// 只给**超级管理员**：这是跨全库的批量修数接口（会改别人负责的任务状态），
/// 不按单个任务的数据范围做闸。中间件闸用 `business:task:update`（管理员本就持有），
/// handler 里再判一次 `is_super_admin()`，非超管 → `403 + 63002`。
///
/// # 参数
///
/// - `dry_run`：缺省 `true`（只报不修）；`false` 才真改；
/// - `limit`：缺省 / 上限 [`model::REPAIR_SCAN_LIMIT`]（200）。
///
/// # 异常情况
///
/// - 401：令牌缺用户标识；
/// - 403 + `63002`：不是超级管理员；
/// - 500：数据库异常（`dry_run=false` 时单条修复失败**不中断**整批，
///   失败条数进 `failed`，逐条 `warn` 留痕）。
#[nik::permission(
    path = "/business/task/admin/repair-parent-status",
    method = "GET",
    permission = "business:task:update"
)]
#[nik::get("/business/task/admin/repair-parent-status")]
pub async fn repair_parent_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::ParentStatusRepairQuery>,
) -> ResResult<dto::ParentStatusRepairRes> {
    if !claims.is_super_admin() {
        return Err(ApiError::new(
            403,
            error_code::TASK_FORBIDDEN,
            "父任务状态巡检是批量修数接口，仅超级管理员可执行",
        ));
    }
    let me = current_user(&claims)?;
    let dry_run = query.dry_run.unwrap_or(true);
    let limit = model::normalize_repair_limit(query.limit);
    let gaps = model::scan_parent_status_gaps(db(), limit)
        .await
        .map_err(|error| utils::db_error(error, "巡检父任务状态"))?;

    let mut items = Vec::with_capacity(gaps.len());
    let mut fixed = 0_usize;
    let mut failed = 0_usize;
    for gap in &gaps {
        let mut status_after = None;
        if !dry_run {
            // 修复起点用子任务（不是父任务自己）：走的正是「子任务状态变更后」那条
            // 生产路径，判据 / 防环 / 时间线评论全复用，巡检不可能判出第二个口径。
            let start = gap.sample_child;
            let repaired = db()
                .transaction(move |tx| async move {
                    let cascades = model::cascade_parent_status(&tx, start, me).await?;
                    for cascade in &cascades {
                        let before = serde_json::json!({ "status": cascade.from.code() });
                        let after = serde_json::json!({ "status": cascade.to.code() });
                        changelog::record(
                            &tx,
                            &ChangeCtx::api(Some(me), None),
                            ChangeTarget::of(
                                "biz_task",
                                cascade.task_id.to_string(),
                                Action::Status,
                                cascade.reason,
                            ),
                            DiffInput::update(&before, &after),
                        )
                        .await
                        .map_err(|error| {
                            DbError::InvalidValue(format!(
                                "变更日志写入失败 [{}] {}",
                                error.code, error.msg
                            ))
                        })?;
                    }
                    Ok(cascades)
                })
                .await;
            match repaired {
                Ok(cascades) => {
                    if cascades.is_empty() {
                        // 扫描与修复之间被别人改过（并发）→ 不算修好，也不算失败
                        tracing::info!(task_id = %gap.task_id, "巡检：该父任务已无级联动作（并发修正）");
                    } else {
                        fixed += 1;
                    }
                }
                Err(error) => {
                    failed += 1;
                    tracing::warn!(task_id = %gap.task_id, %error, "巡检修复父任务状态失败");
                }
            }
            // 修后状态**回读库里**，不拿返回值猜（对账接口只认落库事实）
            status_after = require_task(gap.task_id)
                .await
                .ok()
                .map(|task| task.card.status);
        }
        items.push(dto::ParentStatusGapRes {
            id: gap.task_id.to_string(),
            task_no: gap.task_no.clone(),
            title: gap.title.clone(),
            status: gap.status.code().to_string(),
            progress: gap.progress,
            children: gap.children,
            target: gap.target.code().to_string(),
            status_after,
        });
    }

    tracing::info!(
        dry_run,
        total = items.len() as i64,
        fixed = fixed as i64,
        failed = failed as i64,
        "父任务状态巡检完成"
    );
    Ok(Res::ok(dto::ParentStatusRepairRes {
        dry_run,
        total: items.len(),
        fixed,
        failed,
        items,
    }))
}

/// 把一行统计转成响应（`None` = 窗口内没有该模式的任务 → 全 0）。
fn stats_row(mode: &str, row: Option<&model::ExecModeStat>) -> dto::ExecModeStatRes {
    let empty = model::ExecModeStat {
        exec_mode: mode.to_string(),
        ..Default::default()
    };
    let stat = row.unwrap_or(&empty);
    dto::ExecModeStatRes {
        exec_mode: mode.to_string(),
        total: stat.total,
        todo: stat.todo,
        doing: stat.doing,
        review: stat.review,
        done: stat.done,
        cancelled: stat.cancelled,
        completion_rate: model::completion_rate(stat.done, stat.total, stat.cancelled),
        avg_done_hours: stat.avg_done_hours.map(round_one_decimal),
        avg_progress: stat.avg_progress.map(round_one_decimal),
    }
}

/// 合计行：计数直接相加；**平均值按权重加权**，不是把两个平均数再平均。
///
/// 反例（为什么必须加权）：人类 100 条已完成（平均 10 h）+ AI 1 条已完成（平均 40 h）。
/// 直接平均 = 25 h（把一条 AI 任务的权重放大到 50%）；加权 = `(10×100 + 40×1)/101 ≈ 10.3 h`，
/// 这才是「全体任务的平均耗时」。
fn stats_total(rows: &[dto::ExecModeStatRes]) -> dto::ExecModeStatRes {
    let sum = |pick: fn(&dto::ExecModeStatRes) -> i64| rows.iter().map(pick).sum::<i64>();
    let total = sum(|row| row.total);
    let done = sum(|row| row.done);
    let cancelled = sum(|row| row.cancelled);
    let hours = weighted_average(
        rows.iter()
            .filter_map(|row| row.avg_done_hours.map(|avg| (avg, row.done))),
    );
    let progress = weighted_average(rows.iter().filter_map(|row| {
        let effective = (row.total - row.cancelled).max(0);
        row.avg_progress.map(|avg| (avg, effective))
    }));
    dto::ExecModeStatRes {
        exec_mode: "TOTAL".to_string(),
        total,
        todo: sum(|row| row.todo),
        doing: sum(|row| row.doing),
        review: sum(|row| row.review),
        done,
        cancelled,
        completion_rate: model::completion_rate(done, total, cancelled),
        avg_done_hours: hours.map(round_one_decimal),
        avg_progress: progress.map(round_one_decimal),
    }
}

/// 加权平均 `Σ(value × weight) / Σweight`；权重和为 0 时返回 `None`。
fn weighted_average<I>(items: I) -> Option<f64>
where
    I: Iterator<Item = (f64, i64)>,
{
    let (weighted, weight) = items.fold((0.0_f64, 0_i64), |(sum, count), (value, item_weight)| {
        let item_weight = item_weight.max(0);
        (sum + value * item_weight as f64, count + item_weight)
    });
    if weight == 0 {
        None
    } else {
        Some(weighted / weight as f64)
    }
}

/// 保留 1 位小数（`10.26 → 10.3`）。前端只显示 1 位，服务端先归一避免「9.9 vs 10.0」两处不一致。
fn round_one_decimal(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[cfg(test)]
mod stats_handler_tests {
    use super::*;

    fn row(mode: &str, done: i64, cancelled: i64, hours: Option<f64>) -> dto::ExecModeStatRes {
        let total = done + cancelled + 1;
        dto::ExecModeStatRes {
            exec_mode: mode.to_string(),
            total,
            todo: 1,
            doing: 0,
            review: 0,
            done,
            cancelled,
            completion_rate: model::completion_rate(done, total, cancelled),
            avg_done_hours: hours,
            avg_progress: Some(50.0),
        }
    }

    #[test]
    fn total_is_weighted_by_done_count_not_a_plain_mean() {
        let rows = vec![
            row("HUMAN", 100, 0, Some(10.0)),
            row("AI", 1, 0, Some(40.0)),
        ];
        let total = stats_total(&rows);
        assert_eq!(total.total, 103, "101 + 2（每行多 1 条待开始）");
        assert_eq!(total.done, 101);
        assert_eq!(
            total.avg_done_hours,
            Some(10.3),
            "加权：(10×100 + 40×1)/101 ≈ 10.297 → 10.3；直接平均会得 25.0"
        );
    }

    #[test]
    fn total_handles_empty_and_cancelled_only() {
        let rows = vec![row("HUMAN", 0, 0, None), row("AI", 0, 3, None)];
        let total = stats_total(&rows);
        assert_eq!(total.avg_done_hours, None, "没有已完成任务 → null 而不是 0");
        assert_eq!(total.completion_rate, 0.0);
        assert_eq!(total.exec_mode, "TOTAL");
    }

    #[test]
    fn weighted_average_ignores_zero_and_negative_weights() {
        assert_eq!(weighted_average(std::iter::empty()), None);
        assert_eq!(weighted_average(vec![(5.0, 0)].into_iter()), None);
        assert_eq!(
            weighted_average(vec![(5.0, 0), (3.0, 2)].into_iter()),
            Some(3.0)
        );
        assert_eq!(
            weighted_average(vec![(99.0, -4), (2.0, 1)].into_iter()),
            Some(2.0),
            "负权重按 0 处理，不能把平均值带偏"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("合法日期")
    }

    #[test]
    fn title_is_trimmed_and_rejected_when_blank() {
        assert_eq!(
            normalize_title("  订单   审批 ").expect("合法标题"),
            "订单 审批"
        );
        let error = normalize_title("   ").expect_err("空标题必须报错");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::TASK_TITLE_INVALID);

        let long = "任".repeat(dto::MAX_TITLE_CHARS + 1);
        assert!(normalize_title(&long).is_err());
    }

    #[test]
    fn hours_are_bounded_and_rounded() {
        assert_eq!(validate_hours(None).expect("空值合法"), None);
        assert_eq!(validate_hours(Some(1.26)).expect("合法"), Some(1.3));
        assert!(validate_hours(Some(-1.0)).is_err());
        assert!(validate_hours(Some(f64::NAN)).is_err());
        assert!(validate_hours(Some(100_000.0)).is_err());
    }

    #[test]
    fn filter_rejects_unknown_status_and_priority() {
        let bad_status = TaskPageQuery {
            status: Some("ARCHIVED".to_string()),
            ..TaskPageQuery::default()
        };
        let error = build_filter(&bad_status).expect_err("非法状态必须 400");
        assert_eq!(error.status, 400);

        let bad_priority = TaskPageQuery {
            priority: Some("P9".to_string()),
            ..TaskPageQuery::default()
        };
        assert!(build_filter(&bad_priority).is_err());

        let bad_range = TaskPageQuery {
            due_from: Some(date(2026, 11, 1)),
            due_to: Some(date(2026, 10, 1)),
            ..TaskPageQuery::default()
        };
        assert!(build_filter(&bad_range).is_err());

        let ok = TaskPageQuery {
            status: Some("doing".to_string()),
            priority: Some("p1".to_string()),
            ..TaskPageQuery::default()
        };
        let filter = build_filter(&ok).expect("合法筛选");
        assert_eq!(filter.status.as_deref(), Some("DOING"));
        assert_eq!(filter.priority.as_deref(), Some("P1"));
    }

    #[test]
    fn filter_normalizes_statuses_list() {
        let query = TaskPageQuery {
            statuses: Some(" TODO , doing ,, DONE ".to_string()),
            ..TaskPageQuery::default()
        };
        let filter = build_filter(&query).expect("合法多选");
        assert_eq!(
            filter.statuses,
            Some(vec![
                "TODO".to_string(),
                "DOING".to_string(),
                "DONE".to_string()
            ]),
            "逗号分隔 + 去空白 + 大写归一 + 丢空段"
        );
    }

    #[test]
    fn filter_rejects_unknown_status_in_statuses() {
        let query = TaskPageQuery {
            statuses: Some("TODO,FOO".to_string()),
            ..TaskPageQuery::default()
        };
        let error = build_filter(&query).expect_err("非法状态必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);
        assert!(error.msg.contains("FOO"), "报错要指出非法值：{}", error.msg);
    }

    #[test]
    fn filter_blank_statuses_means_no_filter() {
        // 空串 / 只有逗号 → None（等价不传），不是「筛空集合」
        for raw in ["", "   ", ",", " , , "] {
            let query = TaskPageQuery {
                statuses: Some(raw.to_string()),
                ..TaskPageQuery::default()
            };
            let filter = build_filter(&query).expect("空串合法");
            assert_eq!(filter.statuses, None, "{raw:?} 应归一成 None");
        }
    }

    #[test]
    fn filter_keeps_legacy_status_alongside_statuses() {
        // 两个都传：两个字段各自保留（优先关系由 build_filters 兑现）
        let query = TaskPageQuery {
            status: Some("todo".to_string()),
            statuses: Some("DONE,DOING".to_string()),
            ..TaskPageQuery::default()
        };
        let filter = build_filter(&query).expect("合法");
        assert_eq!(filter.status.as_deref(), Some("TODO"), "旧字段仍归一化");
        assert_eq!(
            filter.statuses,
            Some(vec!["DONE".to_string(), "DOING".to_string()]),
            "多值字段保留顺序"
        );
    }

    #[test]
    fn filter_rejects_bad_owner_uuid() {
        let query = TaskPageQuery {
            owner_id: Some("not-a-uuid".to_string()),
            ..TaskPageQuery::default()
        };
        assert!(build_filter(&query).is_err());
    }

    #[test]
    fn current_user_rejects_bad_subject() {
        let claims = TokenClaims {
            jti: None,
            sub: "not-a-uuid".to_string(),
            username: None,
            roles: Vec::new(),
            permissions: Vec::new(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        };
        let error = current_user(&claims).expect_err("非法 sub 必须报错");
        assert_eq!(error.status, 401);
    }

    // ==================== T3：分派 / 转派 / 协作人 ====================

    /// 分派 / 协作人的权限矩阵：**行级关系 ∪ 权限码**，其他人一律拒绝。
    ///
    /// 逐格断言（4 身份 × 2 权限码 = 8 格 × 2 个动作），防止「改了一边忘了另一边」：
    /// 这两条矩阵是「谁能把活派出去 / 谁能拉人」的唯一口径，错一格就是越权。
    #[test]
    fn assign_and_member_permission_matrix() {
        let roles = [
            ActorRole::Creator,
            ActorRole::Owner,
            ActorRole::Assigner,
            ActorRole::Outsider,
        ];
        for role in roles {
            let related = role != ActorRole::Outsider;
            assert_eq!(
                may_assign(role, false),
                related,
                "{role:?} 没有权限码时：行级关系才放行"
            );
            assert_eq!(
                may_manage_members(role, false),
                related,
                "{role:?} 协作人矩阵应与分派一致"
            );
            assert!(may_assign(role, true), "{role:?} 有权限码时一律放行");
            assert!(
                may_manage_members(role, true),
                "{role:?} 有 member 码时一律放行"
            );
        }
        // 关键的一格：**无关用户 + 无权限码 = 403**（验收第 3 条）
        assert!(!may_assign(ActorRole::Outsider, false));
        // 关键的另一格：AI 用户被授予权限码后必须放行（修正 11，验收第 6 条）
        assert!(may_assign(ActorRole::Outsider, true));
    }

    /// 身份判定优先级：创建人 > 负责人 > 分派人。
    ///
    /// 「创建人自己就是负责人」是最常见的情形（自建任务），必须稳定判成创建人
    /// —— 它与 `next_assigner` 的「操作人 = 创建人则沿用原 assigner」联动。
    #[test]
    fn actor_role_prefers_creator_then_owner_then_assigner() {
        let creator = Uuid::now_v7();
        let owner = Uuid::now_v7();
        let assigner = Uuid::now_v7();
        let other = Uuid::now_v7();

        assert_eq!(
            actor_role(creator, creator, owner, Some(assigner)),
            ActorRole::Creator
        );
        assert_eq!(
            actor_role(owner, creator, owner, Some(assigner)),
            ActorRole::Owner
        );
        assert_eq!(
            actor_role(assigner, creator, owner, Some(assigner)),
            ActorRole::Assigner
        );
        assert_eq!(actor_role(other, creator, owner, None), ActorRole::Outsider);
        // 分派人缺席（自建任务）时，除了创建人 / 负责人谁都是 outsider
        assert_eq!(
            actor_role(assigner, creator, owner, None),
            ActorRole::Outsider
        );
    }

    /// `assigner_id` 落库口径：操作人非创建人 → 记操作人；创建人自己派 → 沿用原值。
    #[test]
    fn assigner_id_follows_card_rule() {
        let creator = Uuid::now_v7();
        let owner = Uuid::now_v7();
        let third = Uuid::now_v7();

        // 创建人分派：assigner 保持原样（自建任务本来就没有 assigner）
        assert_eq!(next_assigner(creator, creator, None), None);
        // 负责人转派：assigner = 操作人（接手的人要知道是谁派的）
        assert_eq!(next_assigner(owner, creator, None), Some(owner));
        // 已有分派人再被转派：assigner 改成新操作人（最近一次经手人）
        assert_eq!(next_assigner(third, creator, Some(creator)), Some(third));
    }

    /// 创建人 / 负责人是受保护协作人（不许移除）。
    #[test]
    fn creator_and_owner_are_protected_members() {
        let creator = Uuid::now_v7();
        let owner = Uuid::now_v7();
        let member = Uuid::now_v7();

        assert!(protected_member(creator, creator, owner));
        assert!(protected_member(owner, creator, owner));
        assert!(!protected_member(member, creator, owner));
    }

    /// 协作人上限边界：50 允许、51 拒绝，错误是 **400 + 63014**（不是 500、不是 409）。
    #[test]
    fn member_quota_boundary() {
        assert!(ensure_member_quota(0, 0).is_ok());
        assert!(ensure_member_quota(0, 50).is_ok(), "正好 50 人必须允许");
        assert!(ensure_member_quota(47, 3).is_ok(), "47 + 3 = 50 必须允许");
        // 幂等重复提交（新增 0 人）不该占额度
        assert!(
            ensure_member_quota(50, 0).is_ok(),
            "满 50 人时重复加同一批人不报上限"
        );

        let error = ensure_member_quota(48, 3).expect_err("51 人必须被拒绝");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::TASK_MEMBER_LIMIT);
        assert!(error.msg.contains("50"), "文案要带上限数字：{}", error.msg);

        let error = ensure_member_quota(0, 51).expect_err("一次加 51 人必须被拒绝");
        assert_eq!(error.code, error_code::TASK_MEMBER_LIMIT);
    }

    /// 人类入口的正文上限：5000 允许、5001 → **400 + 63004**（不许静默截断）。
    #[test]
    fn user_comment_content_limit_is_strict() {
        let at_limit = "字".repeat(MAX_USER_COMMENT_CHARS);
        let accepted =
            normalize_comment_content(&at_limit, &CommentLimit::Strict(MAX_USER_COMMENT_CHARS))
                .expect("正好 5000 字必须接受");
        assert_eq!(accepted.chars().count(), MAX_USER_COMMENT_CHARS);
        // 首尾空白不计入长度
        let padded = format!("  {}  ", "字".repeat(MAX_USER_COMMENT_CHARS));
        assert!(
            normalize_comment_content(&padded, &CommentLimit::Strict(MAX_USER_COMMENT_CHARS))
                .is_ok()
        );

        let too_long = "字".repeat(MAX_USER_COMMENT_CHARS + 1);
        let error =
            normalize_comment_content(&too_long, &CommentLimit::Strict(MAX_USER_COMMENT_CHARS))
                .expect_err("5001 字必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::TASK_FIELD_INVALID);
        assert!(error.msg.contains("5000"), "文案要带上限：{}", error.msg);

        // 空 / 全空白 → 400
        let error = normalize_comment_content("  \n\t ", &CommentLimit::Strict(5_000))
            .expect_err("空正文必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::TASK_FIELD_INVALID);
    }

    /// AI 入口保持「超长截断」（回传体积不可控，截断也比整条丢掉强）。
    #[test]
    fn ai_comment_content_is_truncated() {
        let long = "字".repeat(MAX_COMMENT_CHARS + 10);
        let kept = normalize_comment_content(&long, &CommentLimit::Truncate(MAX_COMMENT_CHARS))
            .expect("AI 正文超长要截断而不是拒绝");
        assert_eq!(kept.chars().count(), MAX_COMMENT_CHARS);
    }

    /// `comment_type` 只允许 PLAIN：伪造系统类型一律 400（红线）。
    #[test]
    fn comment_type_forgery_is_rejected() {
        assert!(comment_type_verdict(None).is_ok());
        assert!(comment_type_verdict(Some("   ")).is_ok());
        assert!(comment_type_verdict(Some("PLAIN")).is_ok());
        assert!(comment_type_verdict(Some(" plain ")).is_ok());
        for forged in ["AI_REPORT", "STATUS_CHANGE", "AI_PROGRESS", "ASSIGN"] {
            let error = comment_type_verdict(Some(forged)).expect_err("伪造类型必须被拒");
            assert_eq!(error.status, 400);
            assert_eq!(error.code, error_code::TASK_FIELD_INVALID);
            assert!(
                error.msg.contains(forged),
                "文案要点名被拒的类型：{}",
                error.msg
            );
        }
    }
}

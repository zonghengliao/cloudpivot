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
//! ### 为什么中间件闸是 `business:task:read` 而不是 `business:task:assign`
//!
//! 分派权的真实口径是「**行级关系**」：创建人 / 当前负责人 / 分派人天然可派。
//! 这三类身份不是权限码能表达的——若把 `business:task:assign` 写进
//! `#[alun::permission]`，创建人（他往往没有这个码）会被中间件挡在门外，
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

use alun::prelude::chrono::Utc;
use alun::prelude::*;
use alun::uuid::Uuid;
use axum::Json;
use axum::extract::Path;

use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::notice::dto::{NoticeLevel, NoticeType};
use crate::business::notice::{self, NoticeDraft, NoticeTarget};
use crate::business::shared::principal;
use crate::business::task::dto::{
    self, MyCounts, MyTasksRes, ParentProgressRes, TaskAssignReq, TaskAssignRes, TaskAttachmentRes,
    TaskCardRes, TaskCreateReq, TaskDeleteRes, TaskDetailRes, TaskMemberAddReq, TaskMemberAddRes,
    TaskMemberRemoveRes, TaskMemberRes, TaskMembersRes, TaskPageQuery, TaskPageRes,
    TaskProgressReq, TaskProgressRes, TaskSaveRes, TaskStatusChangeReq, TaskStatusRes,
    TaskSubtaskCreateReq, TaskSubtaskListRes, TaskUpdateReq,
};
use crate::business::task::model::{self, MyBucket, TaskCardRow, TaskDetailRow, TaskFilter};
use crate::business::task::state::{self, TaskStatus};
use crate::shared::error_code;
use crate::utils;

// ==================== 分页 ====================

/// 任务分页（**可见性 = 数据范围 ∪ 我参与的任务 id 数组**）。
///
/// # 参数
///
/// - `claims`：当前用户（数据范围从库里现解析，不信令牌快照）；
/// - `query`：状态 / 优先级 / 负责人 / 截止范围 / 关键词 + 分页。
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
#[alun::permission(
    path = "/business/task/page",
    method = "GET",
    permission = "business:task:read"
)]
#[alun::get("/business/task/page")]
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
    if let (Some(from), Some(to)) = (query.due_from, query.due_to)
        && from > to
    {
        return Err(param_invalid(
            "截止日范围非法：下界不能晚于上界".to_string(),
        ));
    }
    Ok(TaskFilter {
        status,
        priority,
        owner_id,
        due_from: query.due_from,
        due_to: query.due_to,
        keyword: query.normalized_keyword(),
    })
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
/// - 400 + `63005`：父任务不存在 / 超过 2 层 / 成环；
/// - 400 + `10001`：`owner_id` / `parent_id` 不是合法 UUID；
/// - 400 + `63009`：负责人不存在或已停用；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task",
    method = "POST",
    permission = "business:task:create"
)]
#[alun::post("/business/task")]
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

    // 父任务：存在 + 深度 ≤ 2 + 不成环
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
#[alun::permission(
    path = "/business/task/{id}",
    method = "PUT",
    permission = "business:task:update"
)]
#[alun::put("/business/task/{id}")]
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
/// - 403 + `63002`：既不是负责人 / 创建人，也不是超管；
/// - 409 + `63006`：仍有未完成子任务；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/{id}",
    method = "DELETE",
    permission = "business:task:delete"
)]
#[alun::delete("/business/task/{id}")]
pub async fn delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskDeleteRes> {
    let me = current_user(&claims)?;
    // 红线（修正 11）：AI 用户（`user_type=ROBOT`）**永远不能删任务**。
    //
    // 这是**硬拒**，与分派不同：分派「默认不能、可按角色授予」（只查权限码），
    // 删除则连超管角色都救不回来 —— 理由是一条 AI 执行者被注入后
    // 「删掉任务」是不可逆的破坏（评论 / 日志 / 通知全指向一个软删的主键），
    // 而「停止执行」有正当替代路径（转派给人类 / 取消任务）。
    if is_robot(me).await? {
        return Err(ApiError::new(
            403,
            error_code::TASK_ROBOT_FORBIDDEN,
            "AI 用户不能删除任务（如需停止执行，请把任务转派给人类用户或改为「已取消」）",
        ));
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
    let affected = db()
        .transaction(|tx| async move { model::soft_delete(&tx, task_id, me).await })
        .await
        .map_err(|error| utils::db_error(error, "删除任务"))?;
    tracing::info!(task_id = %task_id, operator = %me, affected, "任务已软删");
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
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 已软删 / 不在可见范围内；
/// - 400 + `10001`：路径里的 id 不是 UUID；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/{id}",
    method = "GET",
    permission = "business:task:read"
)]
#[alun::get("/business/task/{id}")]
pub async fn detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskDetailRes> {
    let me = principal::resolve_for_claims(&claims).await?;
    let task_id = parse_uuid_field(&id, "任务")?;
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
        project_id: row.project_id.map(|id| id.to_string()),
        done_at: row.done_at,
        estimate_hours: row.estimate_hours,
        actual_hours: row.actual_hours,
        cancel_reason: row.cancel_reason.clone(),
        reject_reason: row.reject_reason.clone(),
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
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
#[alun::permission(
    path = "/business/task/my",
    method = "GET",
    permission = "business:task:read"
)]
#[alun::get("/business/task/my")]
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
/// # 三段式判定（顺序不可换，见 [`crate::business::task::state::authorize`]）
///
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
/// - 若本条是子任务 → 沿 `parent_id` 逐级重算父任务进度（架构 D4/D5）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 id 不是 UUID；400 + `63004`：`to` 不是 5 态之一；
/// - 400 + `63013`：打回没填原因 / 不足 5 字；
/// - 403 + `63011`：当前身份无权推这个流转；
/// - 404 + `63001`：任务不存在 / 不可见；
/// - 409 + `63010`：非法流转（跳级 / 终态直取 / 同状态）；
/// - 409 + `63006`：还有未完成子任务，不能完成；
/// - 500：数据库异常 / 变更日志未登记。
#[alun::permission(
    path = "/business/task/{id}/status",
    method = "PATCH",
    permission = "business:task:status"
)]
#[alun::patch("/business/task/{id}/status")]
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

    // ③ 白名单 + 角色 + 附加条件
    let people = state::TaskPeople {
        owner_id: current.card.owner_id,
        creator_id: current.creator_id,
        assigner_id: current.assigner_id,
    };
    let role_mask = people.role_mask(&state::Actor::new(me, mine.is_super()));
    let reason = normalize_reason(req.reason.as_deref())?;
    let transition = state::authorize(
        from,
        to,
        role_mask,
        current.card.need_review,
        reason.as_deref(),
    )
    .map_err(transition_error)?;

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
    let (comment_id, affected, recalcs) = db()
        .transaction(|tx| async move {
            let affected = model::apply_status(&tx, task_id, &update, me).await?;
            if affected == 0 {
                // 事务里被并发软删：评论与日志都不该落
                return Ok((Uuid::nil(), 0, Vec::new()));
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
            let recalcs = model::recalc_ancestors(&tx, task_id, me).await?;
            Ok((comment_id, affected, recalcs))
        })
        .await
        .map_err(|error| utils::db_error(error, "变更任务状态"))?;
    if affected == 0 {
        return Err(not_found());
    }

    // ⑨ 通知（事务外：通知失败不该回滚状态）
    notify_status_change(&current, from, to, me, &operator_name, reason.as_deref()).await;
    notify_ready_to_close(&recalcs, me, &operator_name).await;

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
#[alun::permission(
    path = "/business/task/{id}/progress",
    method = "PATCH",
    permission = "business:task:progress"
)]
#[alun::patch("/business/task/{id}/progress")]
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
/// 复用 T1 的 [`model::validate_parent`]：父任务自身不能已有 `parent_id`（限 2 层）、
/// 不能成环 —— 三级嵌套只可能由 T1 的历史数据造成（本接口不会造出来）。
///
/// # 异常情况
///
/// - 400 + `10001`：路径 / 字段 id 不是 UUID；400 + `63003` / `63004`：标题 / 字段非法；
/// - 400 + `63005`：父任务已是子任务（超过 2 层）；
/// - 404 + `63001`：父任务不存在 / 不可见；
/// - 409 + `63006`：父任务已完成 / 已取消（终态不允许再加子任务）；
/// - 500：数据库异常。
#[alun::permission(
    path = "/business/task/{id}/subtask",
    method = "POST",
    permission = "business:task:subtask"
)]
#[alun::post("/business/task/{id}/subtask")]
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
#[alun::permission(
    path = "/business/task/{id}/subtasks",
    method = "GET",
    permission = "business:task:read"
)]
#[alun::get("/business/task/{id}/subtasks")]
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

// ==================== 内部工具 ====================

/// 当前用户（令牌 `sub`）。
fn current_user(claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    utils::current_user_uuid(claims)
        .ok_or_else(|| ApiError::new(401, error_code::AUTH_TOKEN_INVALID, "当前令牌缺少用户标识"))
}

/// 解析 UUID 字段（失败 → 400 + `10001`）。
fn parse_uuid_field(raw: &str, label: &str) -> std::result::Result<Uuid, ApiError> {
    utils::parse_uuid(raw).ok_or_else(|| param_invalid(format!("{label} ID 不是合法 UUID：{raw}")))
}

/// 400 参数错误。
fn param_invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

/// 任务不存在 / 看不见 → `404 + 63001`。
fn not_found() -> ApiError {
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
            if !hours.is_finite() || hours < 0.0 || hours > 9_999.9 {
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
        created_at: row.created_at,
        updated_at: row.updated_at,
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
fn display_name(claims: &TokenClaims) -> String {
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

/// 前置依赖校验钩子（**H-DEP**，T4 接线）。
///
/// 核心设计 §二 规则 1：`TODO → DOING` 要求「所有 FS 前置必须 DONE」。
/// T2 的交付范围不含依赖接口（`biz_task_dependency` 表 T1 已建、无写入路径），
/// 所以此刻依赖恒为空、恒放行。**保留这个调用点**是为了让 T4 只改函数体：
/// 换成「查 FS 前置 → 有未完成的就 `409 + 63014`」即可，状态机与 handler 骨架不动。
async fn check_dependencies_ready(task_id: Uuid) -> std::result::Result<(), ApiError> {
    tracing::debug!(task_id = %task_id, "前置依赖校验：T2 阶段无依赖数据，放行（T4 接入）");
    Ok(())
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
    draft.link_url = Some(format!("/task/{}", task.card.id));
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

/// 当前账号是不是 AI 用户（`sys_user.user_type = 'ROBOT'`）。
///
/// **只用于文案与「硬拒」**（删任务），不参与分派 / 协作人的准入判定（修正 11）。
async fn is_robot(user_id: Uuid) -> std::result::Result<bool, ApiError> {
    Ok(model::user_type_of(user_id)
        .await
        .map_err(|error| utils::db_error(error, "查询账号类型"))?
        .as_deref()
        == Some("ROBOT"))
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
#[alun::permission(
    path = "/business/task/{id}/assign",
    method = "PATCH",
    permission = "business:task:read"
)]
#[alun::patch("/business/task/{id}/assign")]
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
#[alun::permission(
    path = "/business/task/{id}/members",
    method = "POST",
    permission = "business:task:read"
)]
#[alun::post("/business/task/{id}/members")]
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
#[alun::permission(
    path = "/business/task/{id}/members/{user_id}",
    method = "DELETE",
    permission = "business:task:read"
)]
#[alun::delete("/business/task/{id}/members/{user_id}")]
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
#[alun::permission(
    path = "/business/task/{id}/members",
    method = "GET",
    permission = "business:task:read"
)]
#[alun::get("/business/task/{id}/members")]
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
}

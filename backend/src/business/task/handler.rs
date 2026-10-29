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
    self, MyCounts, MyTasksRes, ParentProgressRes, TaskAttachmentRes, TaskCardRes, TaskCreateReq,
    TaskDeleteRes, TaskDetailRes, TaskMemberRes, TaskPageQuery, TaskPageRes, TaskProgressReq,
    TaskProgressRes, TaskSaveRes, TaskStatusChangeReq, TaskStatusRes, TaskSubtaskCreateReq,
    TaskSubtaskListRes, TaskUpdateReq,
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

/// 发一条任务站内信（**唯一的任务通知出口**）。
///
/// # 参数
///
/// - `task`：任务（取标题 / 编号 / 主键拼链接）；
/// - `recipients`：候选接收人（会自动去掉操作人自己、去重）；
/// - `force`：`true` 时**跳过** `notify.inbox` 偏好（修正 8 的强制分级）；
/// - `title` / `content`：文案；
/// - `operator` / `operator_name`：发送人（通知里显示「谁干的」）。
///
/// # 失败口径
///
/// **通知失败不影响业务**：状态已经落库、评论已经写好，这里只记 WARN。
/// 把通知当强一致会让「通知服务抖一下 → 任务改不了」。
async fn dispatch_task_notice(
    task: &TaskDetailRow,
    recipients: Vec<Uuid>,
    force: bool,
    title: &str,
    content: &str,
    operator: Uuid,
    operator_name: &str,
) {
    let mut targets: Vec<Uuid> = recipients
        .into_iter()
        .filter(|user_id| *user_id != operator)
        .collect();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return;
    }
    if !force {
        let mut kept = Vec::with_capacity(targets.len());
        for user_id in targets {
            if model::inbox_enabled(user_id).await {
                kept.push(user_id);
            }
        }
        targets = kept;
        if targets.is_empty() {
            tracing::debug!(task_id = %task.card.id, "接收人都关闭了站内信，跳过通知");
            return;
        }
    }
    let mut draft = NoticeDraft::text(
        NoticeType::Todo,
        NoticeLevel::Info,
        title,
        content,
        NoticeTarget::User(targets),
    );
    draft.link_url = Some(format!("/task/{}", task.card.id));
    draft.biz_type = Some("task".to_string());
    draft.biz_id = Some(task.card.id.to_string());
    draft.sender_id = Some(operator);
    draft.sender_name = Some(operator_name.to_string());
    match notice::dispatch(draft).await {
        Ok(result) => tracing::info!(
            notice_id = %result.notice_id,
            receivers = result.receiver_count,
            force,
            "任务通知已发送"
        ),
        Err(error) => tracing::warn!(error = %error, force, "任务通知发送失败（不影响业务）"),
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
        recipients,
        force,
        &title,
        &content,
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
            vec![parent.card.owner_id],
            false,
            "子任务全部完成",
            &content,
            operator,
            operator_name,
        )
        .await;
    }
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
}

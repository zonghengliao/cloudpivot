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

use alun::prelude::*;
use alun::uuid::Uuid;
use axum::Json;
use axum::extract::Path;

use crate::business::shared::principal;
use crate::business::task::dto::{
    self, MyCounts, MyTasksRes, TaskAttachmentRes, TaskCardRes, TaskCreateReq, TaskDeleteRes,
    TaskDetailRes, TaskMemberRes, TaskPageQuery, TaskPageRes, TaskSaveRes, TaskUpdateReq,
};
use crate::business::task::model::{self, MyBucket, TaskCardRow, TaskDetailRow, TaskFilter};
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

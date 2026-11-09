//! 项目分组 · 接口层（批次 C+ · T7c）。
//!
//! # 接口清单（`/api` 前缀由 `[router].prefix` 注入）
//!
//! | 方法 | 路径 | 中间件闸 | 说明 |
//! |------|------|----------|------|
//! | GET | `/business/project/page` | `business:project:read` | 分页（名称/编码 + 状态 + 负责人） |
//! | GET | `/business/project/{id}` | `business:project:read` | 详情（**含统计** + 成员） |
//! | POST | `/business/project` | `business:project:create` | 新建（编码未删唯一） |
//! | PUT | `/business/project/{id}` | `business:project:read` | 编辑（**行级**：负责人/创建人 ∪ `business:project:update`） |
//! | DELETE | `/business/project/{id}` | `business:project:read` | 删除（**任务不删**，`project_id` 置 NULL） |
//! | GET | `/business/project/{id}/tasks` | `business:project:read` | 项目下任务（**走 T1 可见性**） |
//! | GET | `/business/project/{id}/members` | `business:project:read` | 成员列表 |
//! | POST | `/business/project/{id}/members` | `business:project:read` | 加成员（行级 + 幂等） |
//! | DELETE | `/business/project/{id}/members/{user_id}` | `business:project:read` | 移除成员（行级） |
//! | PUT | `/business/task/{id}/project` | `business:task:read` | 任务归入 / 移出项目（**行级**） |
//!
//! # 三条红线（卡面「红线」段逐条落地）
//!
//! 1. **项目不是权限边界**：项目**下任务列表**直接调
//!    [`crate::business::task::model::page_visible`]（T1 的唯一可见性实现），本模块
//!    **没有**任何「按 project_id 列任务」的自写 SQL；统计接口也同样按 T1 裁剪
//!    （见 `model::stats_for_projects` 的文档）。验收 6 证明：项目成员 B 打开项目 → 任务数 0。
//! 2. **删项目不删任务**：`soft_delete_project` + `unbind_tasks` + `delete_members`
//!    在**同一个事务**里；回执里带 `unbound_tasks` 数字（验收 4 贴它 + 贴库）。
//! 3. **改造型接口的中间件闸不用改造权限码**：`PUT` / `DELETE` 的真实口径是
//!    「负责人 / 创建人 / 管理员」，权限码表达不了行级关系（T3 的教训）。
//!    因此中间件统一用 `business:project:read`（人人有），行级判定在
//!    [`ensure_writable`] 里用 `claims.has_permission(...)` 兜底管理员。
//!
//! # 任务侧改造（本卡同时落地）
//!
//! - `POST /business/task` / `PUT /business/task/{id}` 的 `project_id` 字段 T1 就已支持，
//!   但**没有校验目标项目是否存在**；本卡在归入接口里补上（[`assign_task_project`]），
//!   并在任务详情/列表里下发 `project_name` / `project_code` 供前端展示与跳转。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::project::{dto, model};
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::task::{dto as task_dto, handler as task_handler, model as task_model};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 sys_menu.auth_code 逐字一致） ====================
//
// 注意：中间件闸的权限码写在 `#[nik::permission(permission = "...")]` 注解里（字面量，
// 不能引用常量），这里只保留**行级判定要用**的三个常量，避免 `dead_code` 告警
// （clippy `-D warnings` 会把死代码升级成错误）。

/// 编辑项目 / 维护成员（管理员侧；负责人与创建人靠行级关系放行）。
const PERM_UPDATE: &str = "business:project:update";
/// 删除项目（管理员侧；负责人与创建人靠行级关系放行）。
const PERM_DELETE: &str = "business:project:delete";
/// 把任务归入 / 移出项目的管理员侧权限码（卡面规则 5）。
const PERM_TASK_UPDATE: &str = "business:task:update";

// ==================== 分页 ====================

/// 项目分页（`GET /business/project/page`）。
///
/// # 参数
///
/// - `name`：名称 **或** 编码的关键词（ILIKE，通配符已转义）；
/// - `status`：`ACTIVE` / `ARCHIVED`（非法值 → 400，不静默忽略）；
/// - `owner_id`：负责人 UUID；
/// - `page` / `page_size`：框架 `normalize_page` 归一化。
///
/// # 返回值
///
/// [`dto::ProjectPageRes`]：列表项自带 `task_total` / `completion_rate`（**我可见**口径）
/// 与 `member_count`，前端不必再逐行拉统计。
///
/// # 异常情况
///
/// - 400 + `10001`：`status` / `owner_id` 取值非法；
/// - 500：数据库异常 / 数据权限片段形态异常。
#[nik::permission(
    path = "/business/project/page",
    method = "GET",
    permission = "business:project:read"
)]
#[nik::get("/business/project/page")]
pub async fn page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<dto::ProjectPageQuery>,
) -> ResResult<dto::ProjectPageRes> {
    let me = principal::resolve_for_claims(&claims).await?;
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let filter = build_filter(&query)?;
    let result = model::page_visible(&me, &filter, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询项目"))?;

    let ids = result.list.iter().map(|row| row.id).collect::<Vec<_>>();
    let stats = model::stats_for_projects(&me, &ids)
        .await
        .map_err(|error| utils::db_error(error, "统计项目任务"))?;
    let members = model::member_counts(&ids)
        .await
        .map_err(|error| utils::db_error(error, "统计项目成员"))?;

    let list = result
        .list
        .iter()
        .map(|row| project_res(row, lookup_stats(&stats, row.id), lookup_count(&members, row.id)))
        .collect();
    Ok(Res::ok(dto::ProjectPageRes {
        list,
        total: result.total,
        page: result.page,
        page_size: result.size,
    }))
}

/// 把查询参数归一化成 [`model::ProjectFilter`]（非法枚举值 → 400）。
fn build_filter(query: &dto::ProjectPageQuery) -> std::result::Result<model::ProjectFilter, ApiError> {
    let status =
        match query.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(dto::normalize_status(raw).ok_or_else(|| {
                field_invalid(format!("项目状态取值非法：{raw}（应为 {}）", dto::STATUSES.join(" / ")))
            })?),
        };
    let owner_id = match query.owner_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(task_handler::parse_uuid_field(raw, "负责人")?),
    };
    Ok(model::ProjectFilter {
        name: query.normalized_name(),
        status,
        owner_id,
    })
}

// ==================== 详情 ====================

/// 项目详情（`GET /business/project/{id}`）：头部 + **统计** + 成员。
///
/// 统计口径：任务总数 / 各状态 / 完成率（分母剔除 `CANCELLED`）/ 总预估工时 / 总实际工时，
/// **全部按 T1 可见性裁剪**（看不见的任务连数量都不透露）。
///
/// # 异常情况
///
/// - 404 + `63401`：项目不存在 / 已删 / 不在可见范围；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/project/{id}",
    method = "GET",
    permission = "business:project:read"
)]
#[nik::get("/business/project/{id}")]
pub async fn detail(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::ProjectDetailRes> {
    let me = principal::resolve_for_claims(&claims).await?;
    let project_id = task_handler::parse_uuid_field(&id, "项目")?;
    let row = require_visible(&me, project_id).await?;

    let stats = model::stats_for_projects(&me, &[project_id])
        .await
        .map_err(|error| utils::db_error(error, "统计项目任务"))?
        .into_iter()
        .next()
        .unwrap_or_else(|| model::ProjectStats {
            project_id,
            ..model::ProjectStats::default()
        });
    let members = model::list_members(project_id)
        .await
        .map_err(|error| utils::db_error(error, "查询项目成员"))?;

    Ok(Res::ok(dto::ProjectDetailRes {
        project: project_res(&row, stats.clone(), members.len() as i64),
        stats: dto::ProjectStatsRes {
            total: stats.total,
            todo: stats.todo,
            doing: stats.doing,
            review: stats.review,
            done: stats.done,
            cancelled: stats.cancelled,
            completion_rate: stats.completion_rate(),
            estimate_hours: stats.estimate_hours,
            actual_hours: stats.actual_hours,
        },
        members: members.iter().map(member_res).collect(),
    }))
}

// ==================== 新建 ====================

/// 新建项目（`POST /business/project`）。
///
/// # 规则
///
/// - `code` 归一化后**未删唯一**（先查一次给中文 400，唯一索引是最后一道闸）；
/// - `owner_id` 缺省 = 创建人自己；显式指定时必须存在且启用；
/// - 归属部门取「负责人当时的部门」，与 `biz_task.dept_id` 的快照口径一致；
/// - 起止日期倒挂 → 400。
///
/// # 异常情况
///
/// - 400 + `63403`：编码 / 名称 / 日期 / 负责人不合法；
/// - 400 + `63402`：编码已被占用；
/// - 400 + `10001`：`owner_id` 不是合法 UUID；
/// - 500：数据库异常。
#[nik::permission(path = "/business/project", method = "POST", permission = "business:project:create")]
#[nik::post("/business/project")]
pub async fn create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<dto::ProjectCreateReq>,
) -> ResResult<dto::ProjectSaveRes> {
    let me = task_handler::current_user(&claims)?;
    let name = dto::normalize_name(&req.name).map_err(field_invalid)?;
    let code = dto::normalize_code(&req.code).map_err(field_invalid)?;
    let description = dto::normalize_description(req.description.as_deref()).map_err(field_invalid)?;
    ensure_date_order(req.start_date, req.due_date)?;

    let owner_id = match req.owner_id.as_deref().map(str::trim) {
        None | Some("") => Some(me),
        Some(raw) => Some(task_handler::parse_uuid_field(raw, "负责人")?),
    };
    let dept_id = ensure_owner(owner_id).await?;

    if model::code_taken(&code, None)
        .await
        .map_err(|error| utils::db_error(error, "校验项目编码"))?
    {
        return Err(code_duplicate(&code));
    }

    let new = model::NewProject {
        id: Uuid::now_v7(),
        code: code.clone(),
        name: name.clone(),
        description,
        owner_id,
        dept_id,
        start_date: req.start_date,
        end_date: req.due_date,
    };
    let new_id = new.id;
    db().transaction(move |tx| async move { model::create_project(&tx, &new, me).await })
        .await
        .map_err(|error| utils::db_error(error, "新建项目"))?;

    Ok(Res::ok(dto::ProjectSaveRes {
        id: new_id.to_string(),
        code,
        name,
        status: dto::STATUS_ACTIVE.to_string(),
    }))
}

// ==================== 编辑 ====================

/// 编辑项目（`PUT /business/project/{id}`）。
///
/// # 权限
///
/// 行级：**负责人 / 创建人** ∪ `business:project:update`（超管与授权角色）。
///
/// # 异常情况
///
/// - 404 + `63401`：项目不存在 / 已删 / 不可见；
/// - 403 + `63404`：无权修改；
/// - 400 + `63402` / `63403`：编码重复 / 字段非法；
/// - 400 + `10001`：空 PUT（一个字段都没传）。
#[nik::permission(
    path = "/business/project/{id}",
    method = "PUT",
    permission = "business:project:read"
)]
#[nik::put("/business/project/{id}")]
pub async fn update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::ProjectUpdateReq>,
) -> ResResult<dto::ProjectSaveRes> {
    let me = task_handler::current_user(&claims)?;
    let rows = principal::resolve_for_claims(&claims).await?;
    let project_id = task_handler::parse_uuid_field(&id, "项目")?;
    if req.is_empty() {
        return Err(task_handler::param_invalid("请求体没有任何可修改字段".to_string()));
    }
    let current = require_visible(&rows, project_id).await?;
    ensure_writable(
        &claims,
        me,
        &current,
        PERM_UPDATE,
        "只有项目负责人、创建人或管理员可以编辑项目",
    )?;

    let mut patch = model::ProjectPatch::default();
    if let Some(raw) = req.name.as_deref() {
        patch.name = Some(dto::normalize_name(raw).map_err(field_invalid)?);
    }
    if let Some(raw) = req.code.as_deref() {
        let code = dto::normalize_code(raw).map_err(field_invalid)?;
        if code != current.code
            && model::code_taken(&code, Some(project_id))
                .await
                .map_err(|error| utils::db_error(error, "校验项目编码"))?
        {
            return Err(code_duplicate(&code));
        }
        patch.code = Some(code);
    }
    if let Some(description) = req.description.as_ref() {
        patch.description = Some(dto::normalize_description(description.as_deref()).map_err(field_invalid)?);
    }
    if let Some(owner) = req.owner_id.as_ref() {
        match owner {
            Some(raw) => {
                let owner_id = task_handler::parse_uuid_field(raw, "负责人")?;
                let dept_id = ensure_owner(Some(owner_id)).await?;
                patch.owner = Some(Some((owner_id, dept_id)));
            }
            None => patch.owner = Some(None),
        }
    }
    if let Some(raw) = req.status.as_deref() {
        patch.status =
            Some(dto::normalize_status(raw).ok_or_else(|| {
                field_invalid(format!("项目状态取值非法：{raw}（应为 {}）", dto::STATUSES.join(" / ")))
            })?);
    }
    if let Some(start_date) = req.start_date.as_ref() {
        patch.start_date = Some(*start_date);
    }
    if let Some(due_date) = req.due_date.as_ref() {
        patch.end_date = Some(*due_date);
    }
    // 日期校验用「合并后」的值：只改一端时不拿旧值比就会漏掉倒挂。
    let effective_start = patch.start_date.unwrap_or(current.start_date);
    let effective_end = patch.end_date.unwrap_or(current.end_date);
    ensure_date_order(effective_start, effective_end)?;

    db().transaction(|tx| async move { model::update_project(&tx, project_id, &patch, me).await })
        .await
        .map_err(|error| utils::db_error(error, "编辑项目"))?;

    let updated = require_visible(&rows, project_id).await?;
    Ok(Res::ok(dto::ProjectSaveRes {
        id: updated.id.to_string(),
        code: updated.code.clone(),
        name: updated.name.clone(),
        status: updated.status.clone(),
    }))
}

// ==================== 删除 ====================

/// 删除项目（`DELETE /business/project/{id}`）。
///
/// # 红线：**删项目不删任务**
///
/// 同一事务内三步：① 项目软删；② `biz_task.project_id` 置 NULL（任务行一行不动）；
/// ③ 成员行物理删（卡面规则 6）。回执带 `unbound_tasks` / `removed_members` 两个数字，
/// 验收脚本直接比对库里的值。
///
/// # 权限
///
/// 行级：**负责人 / 创建人** ∪ `business:project:delete`。
///
/// # 异常情况
///
/// - 404 + `63401`：项目不存在 / 已删 / 不可见；
/// - 403 + `63404`：无权删除；
/// - 500：数据库异常（整个事务回滚，任务不会被解绑）。
#[nik::permission(
    path = "/business/project/{id}",
    method = "DELETE",
    permission = "business:project:read"
)]
#[nik::delete("/business/project/{id}")]
pub async fn delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<dto::ProjectDeleteRes> {
    let me = task_handler::current_user(&claims)?;
    let rows = principal::resolve_for_claims(&claims).await?;
    let project_id = task_handler::parse_uuid_field(&id, "项目")?;
    let current = require_visible(&rows, project_id).await?;
    ensure_writable(
        &claims,
        me,
        &current,
        PERM_DELETE,
        "只有项目负责人、创建人或管理员可以删除项目",
    )?;

    let (deleted, unbound, removed) = db()
        .transaction(|tx| async move {
            let deleted = model::soft_delete_project(&tx, project_id, me).await?;
            if deleted == 0 {
                return Ok((0_u64, 0_u64, 0_u64));
            }
            let unbound = model::unbind_tasks(&tx, project_id, me).await?;
            let removed = model::delete_members(&tx, project_id).await?;
            Ok((deleted, unbound, removed))
        })
        .await
        .map_err(|error| utils::db_error(error, "删除项目"))?;

    if deleted == 0 {
        return Err(project_not_found());
    }
    Ok(Res::ok(dto::ProjectDeleteRes {
        deleted: true,
        unbound_tasks: unbound,
        removed_members: removed,
    }))
}

// ==================== 项目下任务 ====================

/// 项目下任务分页（`GET /business/project/{id}/tasks`）。
///
/// # 红线：**必须走 T1 的可见性**
///
/// 直接复用 [`task_model::page_visible`]（T1 的唯一实现：数据范围 ∪ 参与关系），
/// 只把 `project_id` 作为**额外过滤条件**塞进 `TaskFilter`。
/// 「按 project_id 直接查 biz_task」在这里是一个**不存在**的写法 —— 卡面红线原话。
///
/// # 返回值
///
/// 与 `GET /business/task/page` **同一个出参形状**（[`task_dto::TaskPageRes`]），
/// 前端可以复用同一个列表组件。
///
/// # 异常情况
///
/// - 404 + `63401`：项目不可见（先判项目，再判任务：项目都看不见就不必查任务）；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/project/{id}/tasks",
    method = "GET",
    permission = "business:project:read"
)]
#[nik::get("/business/project/{id}/tasks")]
pub async fn tasks(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Query(query): Query<task_dto::TaskPageQuery>,
) -> ResResult<task_dto::TaskPageRes> {
    let me = principal::resolve_for_claims(&claims).await?;
    let project_id = task_handler::parse_uuid_field(&id, "项目")?;
    // ① 项目本身必须可见（否则连「这个项目存在」都不该确认）
    require_visible(&me, project_id).await?;
    // ② 任务一律走 T1 的可见性实现
    let (page_no, page_size) = utils::normalize_page(query.page, query.page_size);
    let mut filter = task_handler::build_task_filter(&query)?;
    filter.project_id = Some(project_id);
    let result = task_model::page_visible(&me, &filter, page_no, page_size)
        .await
        .map_err(|error| utils::db_error(error, "分页查询项目下任务"))?;
    Ok(Res::ok(task_handler::page_res(&result)))
}

// ==================== 任务归入 / 移出 ====================

/// 把任务归入 / 移出项目（`PUT /business/task/{id}/project`）。
///
/// # 请求体
///
/// ```json
/// { "project_id": "01920000-0000-7000-8000-000000000001" }   // 归入
/// { "project_id": null }                                    // 移出
/// ```
///
/// 字段**缺席 → 400**（分不清「没传」与「清空」，必须显式表达意图）。
///
/// # 权限（卡面规则 5）
///
/// 行级：任务**负责人 / 创建人** ∪ `business:task:update`。
/// **不认项目负责人** —— 项目不是权限边界，反过来也一样：项目负责人不能凭身份改别人的任务。
///
/// # 目标项目校验
///
/// 目标项目必须**对当前用户可见**（`find_visible`）：看不见的项目一律 404，
/// 既防止「猜 id 把任务塞进别人项目」，也让前端下拉框与实际可选项一致。
///
/// # 异常情况
///
/// - 404 + `63001`：任务不存在 / 已删；
/// - 404 + `63401`：目标项目不存在 / 不可见；
/// - 403 + `63407`：无权归入 / 移出；
/// - 400 + `10001`：`project_id` 不是合法 UUID / 字段缺席；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/{id}/project",
    method = "PUT",
    permission = "business:task:read"
)]
#[nik::put("/business/task/{id}/project")]
pub async fn assign_task_project(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::TaskProjectReq>,
) -> ResResult<dto::TaskProjectRes> {
    let me = task_handler::current_user(&claims)?;
    let rows = principal::resolve_for_claims(&claims).await?;
    let task_id = task_handler::parse_uuid_field(&id, "任务")?;
    let Some(target) = req.project_id.as_ref() else {
        return Err(task_handler::param_invalid(
            "请求体必须显式给出 project_id（归入传项目 id，移出传 null）".to_string(),
        ));
    };

    // ① 任务必须存在（写路径不要求可见，但要能判负责人/创建人）
    let task = task_model::find_raw(task_id)
        .await
        .map_err(|error| utils::db_error(error, "查询任务"))?
        .ok_or_else(|| ApiError::new(404, error_code::TASK_NOT_FOUND, "任务不存在或已删除"))?;
    let is_owner = task.card.owner_id == me;
    let is_creator = task.creator_id == me;
    if !is_owner && !is_creator && !claims.has_permission(PERM_TASK_UPDATE) {
        return Err(ApiError::new(
            403,
            error_code::PROJECT_TASK_FORBIDDEN,
            "只有任务负责人、创建人或管理员可以把任务归入/移出项目",
        ));
    }

    // ② 目标项目必须可见（移出时无需校验）
    let mut target_project = None;
    if let Some(raw) = target {
        let project_id = task_handler::parse_uuid_field(raw, "项目")?;
        target_project = Some(require_visible(&rows, project_id).await?);
    }

    let project_id = target_project.as_ref().map(|row| row.id);
    db().transaction(|tx| async move { model::set_task_project(&tx, task_id, project_id, me).await })
        .await
        .map_err(|error| utils::db_error(error, "把任务归入项目"))?;

    Ok(Res::ok(dto::TaskProjectRes {
        task_id: task_id.to_string(),
        project_id: project_id.map(|id| id.to_string()),
        project_code: target_project.as_ref().map(|row| row.code.clone()),
        project_name: target_project.as_ref().map(|row| row.name.clone()),
    }))
}

// ==================== 成员 ====================

/// 项目成员列表（`GET /business/project/{id}/members`）。
///
/// # 异常情况
///
/// - 404 + `63401`：项目不可见；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/project/{id}/members",
    method = "GET",
    permission = "business:project:read"
)]
#[nik::get("/business/project/{id}/members")]
pub async fn members(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<Vec<dto::ProjectMemberRes>> {
    let me = principal::resolve_for_claims(&claims).await?;
    let project_id = task_handler::parse_uuid_field(&id, "项目")?;
    require_visible(&me, project_id).await?;
    let rows = model::list_members(project_id)
        .await
        .map_err(|error| utils::db_error(error, "查询项目成员"))?;
    Ok(Res::ok(rows.iter().map(member_res).collect()))
}

/// 加项目成员（`POST /business/project/{id}/members`）。
///
/// 幂等：已经是成员时 `changed = false`，不报错（前端重复点不该看到红色提示）。
///
/// # 异常情况
///
/// - 404 + `63401`：项目不可见；
/// - 403 + `63404`：无权维护成员；
/// - 400 + `63405`：成员不存在 / 已停用 / 角色非法；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/project/{id}/members",
    method = "POST",
    permission = "business:project:read"
)]
#[nik::post("/business/project/{id}/members")]
pub async fn add_member(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<dto::ProjectMemberAddReq>,
) -> ResResult<dto::ProjectMemberChangeRes> {
    let me = task_handler::current_user(&claims)?;
    let rows = principal::resolve_for_claims(&claims).await?;
    let project_id = task_handler::parse_uuid_field(&id, "项目")?;
    let current = require_visible(&rows, project_id).await?;
    ensure_writable(
        &claims,
        me,
        &current,
        PERM_UPDATE,
        "只有项目负责人、创建人或管理员可以维护项目成员",
    )?;

    let user_id = task_handler::parse_uuid_field(&req.user_id, "成员")?;
    let role = dto::normalize_member_role(req.role.as_deref())
        .ok_or_else(|| field_invalid(format!("成员角色取值非法（应为 {}）", dto::MEMBER_ROLES.join(" / "))))?;
    if !task_model::user_exists(user_id)
        .await
        .map_err(|error| utils::db_error(error, "校验项目成员"))?
    {
        return Err(ApiError::new(
            400,
            error_code::PROJECT_MEMBER_INVALID,
            "成员不存在或已停用",
        ));
    }

    let changed = db()
        .transaction(|tx| async move { model::add_member(&tx, project_id, user_id, &role, me).await })
        .await
        .map_err(|error| utils::db_error(error, "添加项目成员"))?;
    let list = model::list_members(project_id)
        .await
        .map_err(|error| utils::db_error(error, "查询项目成员"))?;
    Ok(Res::ok(dto::ProjectMemberChangeRes {
        changed: changed > 0,
        members: list.iter().map(member_res).collect(),
    }))
}

/// 移除项目成员（`DELETE /business/project/{id}/members/{user_id}`）。
///
/// # 异常情况
///
/// - 404 + `63401`：项目不可见；
/// - 403 + `63404`：无权维护成员；
/// - 404 + `63406`：本来就不是成员；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/project/{id}/members/{user_id}",
    method = "DELETE",
    permission = "business:project:read"
)]
#[nik::delete("/business/project/{id}/members/{user_id}")]
pub async fn remove_member(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path((id, user_id)): Path<(String, String)>,
) -> ResResult<dto::ProjectMemberChangeRes> {
    let me = task_handler::current_user(&claims)?;
    let rows = principal::resolve_for_claims(&claims).await?;
    let project_id = task_handler::parse_uuid_field(&id, "项目")?;
    let current = require_visible(&rows, project_id).await?;
    ensure_writable(
        &claims,
        me,
        &current,
        PERM_UPDATE,
        "只有项目负责人、创建人或管理员可以维护项目成员",
    )?;
    let target = task_handler::parse_uuid_field(&user_id, "成员")?;

    let removed = db()
        .transaction(|tx| async move { model::remove_member(&tx, project_id, target).await })
        .await
        .map_err(|error| utils::db_error(error, "移除项目成员"))?;
    if removed == 0 {
        return Err(ApiError::new(
            404,
            error_code::PROJECT_MEMBER_NOT_FOUND,
            "该用户不是项目成员",
        ));
    }
    let list = model::list_members(project_id)
        .await
        .map_err(|error| utils::db_error(error, "查询项目成员"))?;
    Ok(Res::ok(dto::ProjectMemberChangeRes {
        changed: true,
        members: list.iter().map(member_res).collect(),
    }))
}

// ==================== 内部工具 ====================

/// 取可见项目（不存在 / 不可见 → 404 `63401`）。
async fn require_visible(principal: &ScopedPrincipal, id: Uuid) -> std::result::Result<model::ProjectRow, ApiError> {
    model::find_visible(principal, id)
        .await
        .map_err(|error| utils::db_error(error, "查询项目"))?
        .ok_or_else(project_not_found)
}

/// 行级写权限：负责人 / 创建人 ∪ 权限码。
fn ensure_writable(
    claims: &TokenClaims,
    me: Uuid,
    row: &model::ProjectRow,
    code: &str,
    message: &str,
) -> std::result::Result<(), ApiError> {
    if row.owner_id == Some(me) || row.create_by == Some(me) || claims.has_permission(code) {
        return Ok(());
    }
    Err(ApiError::new(403, error_code::PROJECT_FORBIDDEN, message))
}

/// 校验负责人存在并返回其部门快照（缺省 / 清空 → `None`）。
async fn ensure_owner(owner_id: Option<Uuid>) -> std::result::Result<Option<Uuid>, ApiError> {
    let Some(owner_id) = owner_id else {
        return Ok(None);
    };
    if !task_model::user_exists(owner_id)
        .await
        .map_err(|error| utils::db_error(error, "校验项目负责人"))?
    {
        return Err(field_invalid("负责人不存在或已停用".to_string()));
    }
    task_model::user_dept(owner_id)
        .await
        .map_err(|error| utils::db_error(error, "查询负责人所属部门"))
}

/// 起止日期倒挂 → 400。
fn ensure_date_order(
    start: Option<chrono::NaiveDate>,
    end: Option<chrono::NaiveDate>,
) -> std::result::Result<(), ApiError> {
    if let (Some(start), Some(end)) = (start, end)
        && start > end
    {
        return Err(field_invalid("日期非法：项目开始日不能晚于结束日".to_string()));
    }
    Ok(())
}

/// 400 + `63403` 项目字段非法。
fn field_invalid(message: String) -> ApiError {
    ApiError::new(400, error_code::PROJECT_FIELD_INVALID, message)
}

/// 400 + `63402` 编码重复。
fn code_duplicate(code: &str) -> ApiError {
    ApiError::new(
        400,
        error_code::PROJECT_CODE_DUPLICATE,
        format!("项目编码已被占用：{code}"),
    )
}

/// 404 + `63401` 项目不存在 / 不可见。
fn project_not_found() -> ApiError {
    ApiError::new(404, error_code::PROJECT_NOT_FOUND, "项目不存在或已删除")
}

/// 从批量统计结果里取一条（没有可见任务 → 全零）。
fn lookup_stats(stats: &[model::ProjectStats], id: Uuid) -> model::ProjectStats {
    stats
        .iter()
        .find(|item| item.project_id == id)
        .cloned()
        .unwrap_or_else(|| model::ProjectStats {
            project_id: id,
            ..model::ProjectStats::default()
        })
}

/// 从成员计数结果里取一条。
fn lookup_count(counts: &[(Uuid, i64)], id: Uuid) -> i64 {
    counts
        .iter()
        .find(|(project_id, _)| *project_id == id)
        .map(|(_, total)| *total)
        .unwrap_or(0)
}

/// 行 → 列表项 / 详情头部 DTO。
fn project_res(row: &model::ProjectRow, stats: model::ProjectStats, member_count: i64) -> dto::ProjectRes {
    dto::ProjectRes {
        id: row.id.to_string(),
        code: row.code.clone(),
        name: row.name.clone(),
        description: row.description.clone().unwrap_or_default(),
        owner_id: row.owner_id.map(|id| id.to_string()),
        owner_name: row.owner_name.clone(),
        dept_id: row.dept_id.map(|id| id.to_string()),
        dept_name: row.dept_name.clone(),
        status: row.status.clone(),
        start_date: row.start_date,
        due_date: row.end_date,
        task_total: stats.total,
        task_done: stats.done,
        completion_rate: stats.completion_rate(),
        member_count,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// 成员行 → DTO。
fn member_res(row: &model::MemberRow) -> dto::ProjectMemberRes {
    dto::ProjectMemberRes {
        user_id: row.user_id.to_string(),
        username: row.username.clone(),
        real_name: row.real_name.clone(),
        role: row.role.clone(),
        created_at: row.created_at,
    }
}

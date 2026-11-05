// ============================================================================
// task_handler_hierarchy.rs —— 父子关系：建子任务 / 子任务列表 / 父任务状态巡检修复
//
// 由 `handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use super::task_handler_common::*;
use crate::business::changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput};
use crate::business::shared::principal::{self};
use crate::business::task::dto::{self, TaskSaveRes, TaskSubtaskCreateReq, TaskSubtaskListRes};
use crate::business::task::model::{self};
use crate::business::task::state::{self, TaskStatus};
use crate::shared::error_code;
use crate::utils;

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
        // P2-9：子任务不继承父任务的单据关联（同理：挂单要显式）
        doc_type: None,
        doc_id: None,
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

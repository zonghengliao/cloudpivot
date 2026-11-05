// ============================================================================
// task_handler_crud.rs —— 任务 CRUD：分页 / 按单据查 / 建 / 复制 / 改 / 删 / 详情 / 我的任务
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
use crate::business::flow::task_bridge;
use crate::business::shared::principal::{self};
use crate::business::task::doc_ref;
use crate::business::task::dto::{
    self, MyCounts, MyTasksRes, TaskAttachmentRes, TaskByDocQuery, TaskByDocRes, TaskCreateReq,
    TaskDeleteRes, TaskDetailRes, TaskMemberRes, TaskPageQuery, TaskPageRes, TaskSaveRes,
    TaskUpdateReq,
};
use crate::business::task::model::{self, MyBucket, TaskFilter};
use crate::middleware::task_token::AiTaskActor;
use crate::shared::ai_actor;
use crate::shared::error_code;
use crate::utils;

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

/// 按单据反查「这张单上挂了哪些任务」（P2-9 / DC-19）。
///
/// # 它解决什么
///
/// 采购单详情底部的「关联任务」区、以及任务详情里的「关联单据」链接都需要
/// 这条反向查询。**不许**让前端拿 `doc_id` 去翻任务列表再自己筛 —— 那要拉全量
/// 任务，而且会把可见性判断放到前端。
///
/// # 参数
///
/// - `doc_type` / `doc_id`：**成对必填**（白名单与存在性校验都走
///   [`doc_ref::resolve`]，与建任务时同一份实现）；
/// - 结果按创建时间倒序，硬上限 [`model::DOC_TASK_LIMIT`] 条。
///
/// # 可见性
///
/// 走 [`model::list_by_doc`]：**与任务列表同一套谓词**（数据范围 ∪ 我参与）。
/// 挂单不是共享权限 —— 别人挂在同一张采购单上的任务，我看采购单也看不到。
///
/// # 异常情况
///
/// - 400 + `63022`：`doc_type` 不在白名单；
/// - 400 + `63023`：单据不存在 / 已软删；
/// - 400 + `63004`：`doc_id` 不是合法 UUID；
/// - 401：令牌缺用户标识；
/// - 500：数据库异常。
#[nik::permission(
    path = "/business/task/by-doc",
    method = "GET",
    permission = "business:task:read"
)]
#[nik::get("/business/task/by-doc")]
pub async fn by_doc(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<TaskByDocQuery>,
) -> ResResult<TaskByDocRes> {
    let me = principal::resolve_for_claims(&claims).await?;
    let (target, doc_id) = doc_ref::resolve(Some(&query.doc_type), Some(&query.doc_id))
        .await?
        .ok_or_else(|| param_invalid("doc_type 与 doc_id 都必须给出".to_string()))?;
    let rows = model::list_by_doc(&me, target.kind.as_str(), doc_id, model::DOC_TASK_LIMIT)
        .await
        .map_err(|error| utils::db_error(error, "按单据查询任务"))?;
    let doc_no = doc_ref::doc_no(target, doc_id).await;
    Ok(Res::ok(TaskByDocRes {
        doc_type: target.kind.as_str().to_string(),
        doc_id: doc_id.to_string(),
        doc_no,
        total: i64::try_from(rows.len()).unwrap_or(i64::MAX),
        list: rows.iter().map(card_res).collect(),
    }))
}

/// 把查询参数归一化成 [`TaskFilter`]（非法枚举值 → 400，不静默忽略）。
pub(super) fn build_filter(query: &TaskPageQuery) -> std::result::Result<TaskFilter, ApiError> {
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
    // 关联单据（P2-9 / DC-19）：白名单 → UUID → 存在且未软删，三件一起校验。
    // 采购单详情底部的「关联任务」区新建任务时带的就是这一对。
    let doc = doc_ref::resolve(req.doc_type.as_deref(), req.doc_id.as_deref()).await?;

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
        doc_type: doc.map(|(target, _)| target.kind.as_str().to_string()),
        doc_id: doc.map(|(_, id)| id),
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

// ==================== 创建后：复制为新任务（D9） ====================

/// 复制接口的权限码 = **建单权限码**（卡面硬红线 5）。
pub const COPY_PERMISSION: &str = "business:task:create";

/// 副本标题的后缀。用全角括号与中文标题同气质；`str::ends_with` 可逐字断言。
pub const COPY_TITLE_SUFFIX: &str = "（副本）";

/// 副本标题：`原标题（副本）`，超长时**先砍原标题**再拼后缀（后缀不许被截断）。
///
/// 标题上限 [`dto::MAX_TITLE_CHARS`]（200 字）；原标题正好 200 字时直接拼会超限，
/// 而「副本」两字被截掉的标题和原任务无法区分，所以砍的是原标题尾部。
pub fn copy_title(origin: &str) -> String {
    let keep = dto::MAX_TITLE_CHARS.saturating_sub(COPY_TITLE_SUFFIX.chars().count());
    format!(
        "{}{}",
        dto::truncate_chars(origin.trim(), keep),
        COPY_TITLE_SUFFIX
    )
}

/// 「复制为新任务」的核心逻辑（endpoint 与单测共用）。
///
/// # 语义（卡面 §一 / §五 ⑤）
///
/// ```text
/// 保留：标题（+「（副本）」）/ 描述 / 负责人 / 优先级 / 需验收 / 预估工时 / 所属项目
/// 不复制：子任务（`parent_id` 指向它的那些）、评论、协作人、附件、工时记录、依赖边
/// 清空：任务编号（重新走 `TSK-YYYYMM-NNNNNN` 取号）、状态（`TODO`）、进度（0）、
///       父任务（`parent_id = NULL`）、审批实例、取消/打回原因、完成时间
/// 改：  计划开始日 = NULL、截止日 = 本周五（同新建默认）
/// ```
///
/// # ★ 决策：`parent_id` 清空
///
/// 卡片只说「不复制子任务」，没说父任务。这里取「复制出来的是一张**独立新任务**」：
/// 原任务是某父任务的第 3 个子任务，复制单再挂同一父任务，会让父任务的进度分母
/// 悄悄 +1（用户以为自己只是「再来一件同样的事」）。要挂回去，编辑一次即可。
///
/// # ★ 决策：日期不复制
///
/// 原任务的截止日往往已过去（复制场景多是「上个月那件事再来一遍」），照抄会得到一张
/// **建出来就逾期**的任务。截止日按新建口径给「本周五」，开始日留空由用户自己定。
///
/// # ★ 决策：负责人保留（不是改成自己）
///
/// 卡面明写「复制负责人」。若负责人不是当前用户，则当前用户是**分派人**
/// （`assigner_id = 我`、`source = ASSIGNED`、我自动成为协作人）——与新建任务里
/// 「替别人建单」同一套口径，不额外造第二条规则。
///
/// # 异常情况
///
/// - 403：无 `business:task:create`（复制 = 建单）；无 `business:task:read` 或任务不在可见范围 → 404；
/// - 404：任务不存在 / 已软删 / 看不见；
/// - 400：标题拼接后仍非法（原标题全空白，理论上被库约束挡在前）；
/// - 500：数据库异常。
pub async fn copy_task(id: Uuid, claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    ensure_task_create_permission(claims)?;
    let me = current_user(claims)?;
    // 可见性照详情页口径：看不见的任务连「复制」都不该知道它存在（404 而非 403）
    ensure_task_read_permission(claims)?;
    let scope = principal::resolve_for_claims(claims).await?;
    let origin = model::find_detail(&scope, id)
        .await
        .map_err(|error| utils::db_error(error, "查询待复制的任务"))?
        .ok_or_else(not_found)?;

    let title = copy_title(&origin.card.title);
    if title.trim() == COPY_TITLE_SUFFIX {
        return Err(param_invalid("任务标题为空，无法生成副本标题".to_string()));
    }
    let owner_id = origin.card.owner_id;
    let today = model::today_cst();
    let new = model::NewTask {
        id: Uuid::now_v7(),
        title,
        description: origin.description.clone(),
        priority: origin.card.priority.clone(),
        need_review: origin.card.need_review,
        owner_id,
        creator_id: me,
        assigner_id: (owner_id != me).then_some(me),
        dept_id: origin.card.dept_id,
        // 副本是独立新任务：不挂父任务（见文档注释）
        parent_id: None,
        project_id: origin.card.project_id,
        // P2-9：副本**不继承**单据关联（挂单是显式业务声明；静默继承会让单据侧
        // 「关联任务」里冒出用户没主动挂的行。要挂就编辑时显式再挂一次）。
        doc_type: None,
        doc_id: None,
        start_date: None,
        due_date: Some(model::default_due_date(today)),
        estimate_hours: origin.estimate_hours,
        source: if owner_id == me {
            dto::SOURCE_SELF.to_string()
        } else {
            dto::SOURCE_ASSIGNED.to_string()
        },
        add_creator_as_collaborator: owner_id != me,
    };
    let month = model::month_key(today);
    let new_id = new.id;
    let task_no = db()
        .transaction(|tx| async move { model::create_task(&tx, &new, me, &month).await })
        .await
        .map_err(|error| utils::db_error(error, "复制任务"))?;
    tracing::info!(
        origin_task = %id,
        new_task_id = %new_id,
        task_no = %task_no,
        operator = %me,
        "任务已复制为新任务"
    );
    Ok(new_id)
}

/// 复制为新任务（`POST /business/task/{id}/copy`，权限 = 建单权限）。
#[nik::permission(
    path = "/business/task/{id}/copy",
    method = "POST",
    permission = "business:task:create"
)]
#[nik::post("/business/task/{id}/copy")]
pub async fn task_copy(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<TaskSaveRes> {
    let origin_id = parse_uuid_field(&id, "任务")?;
    let new_id = copy_task(origin_id, &claims).await?;
    let row = require_task(new_id).await?;
    Ok(Res::ok(save_res(&row)))
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
    // P2-9：关联单据整对改。缺席 = 不改；两个都 `null` = 清单；只给一个 → 400 + 63004。
    if req.doc_type.is_some() || req.doc_id.is_some() {
        if req.doc_type.is_some() != req.doc_id.is_some() {
            return Err(ApiError::new(
                400,
                error_code::TASK_FIELD_INVALID,
                "doc_type 与 doc_id 必须同时给出：要么两个都给（挂/换单），\
                 要么两个都置 null（清单）",
            ));
        }
        let (doc_type, doc_id) = (
            req.doc_type.as_ref().and_then(|inner| inner.as_deref()),
            req.doc_id.as_ref().and_then(|inner| inner.as_deref()),
        );
        patch.doc = Some(match doc_ref::resolve(doc_type, doc_id).await? {
            Some((target, id)) => (Some(target.kind.as_str().to_string()), Some(id)),
            None => (None, None),
        });
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
    // P2-9：关联单据的单号（展示用；查不到 / 已软删 → None，前端显示「单据已删除」）
    let doc_id_text = row.card.doc_id.map(|id| id.to_string());
    let doc_no = doc_ref::doc_no_of(row.card.doc_type.as_deref(), doc_id_text.as_deref()).await;

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
        doc_no,
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

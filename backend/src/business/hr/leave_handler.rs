//! 请假单接口层（`biz_leave_request` · HR-25）：设计 §5.2 的 #26~#31（6 条）。
//!
//! | # | 方法 | 路径（`/api` 前缀由框架注入） | 权限码 | 做什么 |
//! |:--:|---|---|---|---|
//! | 26 | GET | `/business/hr/leave/page` | `business:hr:leave:list` | 列表（按 dept 数据权限；`mine=true` 只看本人） |
//! | 27 | GET | `/business/hr/leave/{id}` | `business:hr:leave:detail` | 详情 + 审批轨迹（`flow` 模块的时间线） |
//! | 28 | POST | `/business/hr/leave` | `business:hr:leave:create` | 建草稿（服务端取号 + 抄员工快照） |
//! | 29 | PUT | `/business/hr/leave/{id}` | `business:hr:leave:update` | 改草稿（部分更新，锁死态拒绝） |
//! | 30 | POST | `/business/hr/leave/{id}/submit` | `business:hr:leave:submit` | 提交审批（发起 `hr_leave` 流程） |
//! | 31 | POST | `/business/hr/leave/{id}/cancel` | `business:hr:leave:cancel` | 作废 = 删除草稿 / 撤回审批 / 销假 |
//!
//! 数据层在 [`crate::business::hr::leave_model`]，入参校验在 [`crate::business::hr::leave_dto`]。
//!
//! # 状态机（设计 §4.2.2，本卡验收项之一）
//!
//! ```text
//! DRAFT ──提交(#30)──▶ SUBMITTED ──审批通过(bridge HR-27)──▶ APPROVED ──销假(#31)──▶ CANCELLED
//!   │                     │
//!   │                     └──驳回(bridge) / 撤回(#31)──▶ DRAFT
//!   └──删除(#31，软删)──▶ deleted_at（状态不变）
//! ```
//!
//! 四条边在 `state_machine::LEAVE_EDGES` 里（HR-08 注册），本文件**不写**
//! `match (from, to)`：边的合法性、`biz_status_log` 的落库方式全在
//! [`state_machine::transition`]；本文件只做「锁行 → 判当前态 → 走状态机 → CAS 回写」。
//!
//! # 一条 `/cancel` 为什么要分三条路（设计 §5.2 #31 + §4.2.2）
//!
//! 设计只给了一条作废接口，但状态机给的是三种后果，**不能合并成一种**：
//!
//! | 当前态 | 后果 | 为什么不能合并 |
//! |---|---|---|
//! | `DRAFT` | 软删（`deleted_at`），状态**不变** | 草稿从未提交过，进 `CANCELLED` 会留下一条「有人批过又被作废」的假轨迹；`SUBMITTED` 才需要撤回 |
//! | `SUBMITTED` | **撤回**：终止在跑实例 + `SUBMITTED → DRAFT` | 直接进 `CANCELLED` 会留下在跑的审批实例顶住 `uk_biz_flow_instance_biz_active`，而且审批人还会在待办里看到一张已作废的单 |
//! | `APPROVED` | 销假：`APPROVED → CANCELLED`（原因必填） | 已批准的假影响考勤与工资扣款基线（`{ATT_LEAVE_UNPAID_DAYS}`），只能作废、不能改 |
//! | `CANCELLED` | 409 | 已经作废过 |
//!
//! 回执里的 `action` 字段（`DELETED` / `WITHDRAWN` / `CANCELLED`）就是这三条路，
//! 前端按钮文案与提示直接用它，不要自己按状态推。
//!
//! # 提交（#30）为什么要开事务、先锁行、再判「有没有流程」
//!
//! 提交一次要同时落下三样东西：审批实例（`flow` 五表）、单据状态（`status` + `submitted_*`）、
//! 状态日志（`biz_status_log`）。三者必须同生同灭 —— 只建实例不改状态 = 审批人在待办里
//! 看到一张仍是「草稿」的单；只改状态不建实例 = 单据永远停在 `SUBMITTED` 且没有待办能推进它
//! （`AGENTS.md` 约定 11 的死单）。
//!
//! **没有已发布流程 = fail-closed**（设计 §4.3 第 3 条）：`approval_gate::authorize_missing_flow`
//! 返回 409 + `66033` 并点名 `hr_leave`，单据留在 `DRAFT` 可改可删。
//! 请假单**刻意不提供** `business:erp:skip-approval` 旁路：状态机里没有 `DRAFT → APPROVED`
//! 这条边（`LEAVE_EDGES` 只 4 条），「跳过审批」只能靠绕过状态机 —— 那等于把边表的守卫
//! 重新打开。要免审就走**发布一条没有审批节点的流程**（引擎在同事务里直接 `APPROVED`，
//! 本文件的 `auto_finished` 分支会把单据一并推到 `APPROVED`，不留中间态）。
//!
//! # 并发（`FOR UPDATE` + 状态 CAS 两层）
//!
//! 每个写操作都是「读当前状态 → 判守卫 → 写」。不锁行的话两个人同时点「提交」各读到
//! `DRAFT`、各建一张实例，第二次会撞 `uk_biz_flow_instance_biz_active` 变成 500。
//! 锁行 + `UPDATE … WHERE status = 旧值` 两层：只有一次能成功，另一次拿到 `rows = 0`
//! → 409「已被他人处理，请刷新」。
//!
//! # 姓名与快照（`AGENTS.md` 约定 13）
//!
//! - `employee_no` / `employee_name` / `dept_id` 是**请假发生时的快照**，建单时由本文件
//!   从 `biz_employee` 抄（**不信前端**，也不让 model 去 join 员工表 —— 建单要的是「当时」的值）；
//! - 出参的 `dept_name` / `submitted_by_name` / `cancelled_by_name` 由 `leave_model::ROW_SQL` join；
//! - 审批轨迹里的人名来自 `biz_flow_task.assignee_name`（流程引擎自己的快照列）。

use axum::Json;
use axum::extract::{Extension, Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::{self, ApprovalGate};
use crate::business::flow::{engine, instance_pdf, model as flow_model};
use crate::business::hr::employee_dto::timestamp_cst;
use crate::business::hr::employee_model;
use crate::business::hr::leave_bridge;
use crate::business::hr::leave_dto::{
    self, LeaveApprovalRes, LeaveApprovalTaskRes, LeaveCancelAction, LeaveCancelReq, LeaveCancelRes, LeaveCreateReq,
    LeaveDetailRes, LeavePageQuery, LeaveRes, LeaveSubmitReq, LeaveUpdateReq,
};
use crate::business::hr::leave_model::{self as model, LeaveInsert};
use crate::business::shared::principal::{self, ScopedPrincipal};
use crate::business::shared::state_machine::{self, Operator};
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0387 的 sys_menu.auth_code 逐字一致） ====================

/// 列表（页面码，设计 §5.2 #26）。
pub const PERM_LIST: &str = "business:hr:leave:list";
/// 详情（设计 §5.2 #27）。
pub const PERM_DETAIL: &str = "business:hr:leave:detail";
/// 建单（设计 §5.2 #28）。
pub const PERM_CREATE: &str = "business:hr:leave:create";
/// 改草稿（设计 §5.2 #29）。
pub const PERM_UPDATE: &str = "business:hr:leave:update";
/// 提交审批（设计 §5.2 #30）。
pub const PERM_SUBMIT: &str = "business:hr:leave:submit";
/// 作废 / 撤回 / 销假（设计 §5.2 #31）。
pub const PERM_CANCEL: &str = "business:hr:leave:cancel";

// ==================== 审批流常量（设计 §4.3 的 biz_type 表） ====================

/// 请假审批的业务类型（`DocKind::LeaveRequest` 对应的 `biz_flow_instance.biz_type`）。
pub const FLOW_BIZ_TYPE: &str = "HR_LEAVE";

/// 请假审批的流程编码（**一个字都不能错**：查不到流程 = 提交入口 409）。
///
/// 流程由 **HR-26** 的迁移种进 `biz_flow_def`（`flow_code = 'hr_leave'`）。
/// 本卡（HR-25）只消费它：没有它，提交入口按 fail-closed 拒绝，单据留在草稿。
pub const FLOW_CODE: &str = "hr_leave";

/// 员工状态「离职」的码值（`ck_biz_employee_status` 四值之一）。
///
/// `employee_dto` 只导出了 `STATUS_VALUES` 数组（`["PROBATION","ACTIVE","SUSPENDED","RESIGNED"]`），
/// 没有单值常量；这里写一个**本地**常量并在下面单测里与数组对拍，
/// 免得「数组改了、这个常量没改」时静默失效。
const EMPLOYEE_STATUS_RESIGNED: &str = "RESIGNED";

// ==================== 错误原语 ====================

/// `404 + 10002`：单不存在 / 已软删。
fn not_found() -> ApiError {
    ApiError::new(404, error_code::RESOURCE_NOT_FOUND, "请假单不存在或已被删除")
}

/// `409 + 10003`：并发保护（状态已被他人改动 / 原地重放）。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// `409 + 68012`：「当前状态下做不了这件事」（设计给请假单的专用码）。
///
/// 与 `10003` 分开：前端对 `68012` 直接出人话（状态不允许），对 `10003` 提示「刷新重试」。
fn rule_blocked(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::HR_LEAVE_RULE_BLOCKED, message.into())
}

/// 路径参数里的 uuid。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("{field} 必须是合法的 uuid（收到 `{text}`）"),
        )
    })
}

/// 待办状态 → 中文名（审批轨迹用）。
///
/// 与 `flow::dto` 的前端口径同源；只覆盖时间线上会出现的值，未知值原样返回
/// （不吞码值 —— 排障时看到 `SOME_NEW_STATUS` 比看到「未知」有用）。
fn task_status_label(status: &str) -> String {
    match status.trim().to_ascii_uppercase().as_str() {
        "PENDING" => "待审批".to_string(),
        "APPROVED" => "已同意".to_string(),
        "REJECTED" => "已驳回".to_string(),
        "SKIPPED" => "已跳过".to_string(),
        "TRANSFERRED" => "已转办".to_string(),
        "CANCELLED" => "已终止".to_string(),
        "WITHDRAWN" => "已撤回".to_string(),
        other => other.to_string(),
    }
}

// ==================== ㉖ 列表 ====================

/// 请假单列表（设计 §5.2 #26）：按 `dept_id` 数据权限裁剪（设计 §4.7.3）。
///
/// `mine=true` 走「我的请假」口径（`biz_employee.user_id = 当前登录用户`），
/// 由 `leave_model::list_query` 的 `EXISTS` 子查询实现 —— 普通员工即使数据范围是
/// `SELF`（本表 `owner_col = NULL`，无法按归属裁）也能看到自己请过的假。
///
/// # 闸只有一道：`business:hr:leave:list`（卡 t_0af7ec46 解耦后）
///
/// HR-47 那轮（t_be54535c）在这里加过第二道闸 `scope_to_self_without_detail`
/// （缺 `leave:detail` 就强制 `mine=true`），根因是「请假单页」**页面行**的 `auth_code`
/// 就是 `business:hr:leave:list` ⇒ 员工为了让页面显形，顺带持有了全量列表码。
///
/// `V0401` 把页面行改成 `business:hr:leave:page` 之后，页面显形与全量接口分开：
/// `EMPLOYEE` 只有页面码 + `leave:create`，走到本接口就被**框架路由闸**判 403。
/// 员工自己的请假走 `/business/hr/leave/my`（登录即可，只出本人），前端「我的请假」
/// tab 已经改调它（`frontend/src/views/business/hr/leave/index.vue`）。
///
/// 所以人肉闸已删除；本接口只剩 `#[nik::permission(permission = "business:hr:leave:list")]`
/// 这一道。判据与回归：`hr_permission_matrix.sh`（员工 403、人事经理 200 全量）。
#[nik::permission(
    path = "/business/hr/leave/page",
    method = "GET",
    permission = "business:hr:leave:list"
)]
#[nik::get("/business/hr/leave/page")]
pub async fn leave_page(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<LeavePageQuery>,
) -> ResResult<PageData<Vec<LeaveRes>>> {
    let (page, page_size) = leave_dto::normalize_page(query.page, query.page_size);
    let filter = leave_dto::filter_of(&query)?;
    let principal = principal::resolve_for_claims(&claims).await?;
    let (list, total) = page_leaves(&principal, &filter, page, page_size).await?;
    Ok(Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size))
}

/// 列表组装（接口与真库用例共用同一条路径）。
pub async fn page_leaves(
    principal: &ScopedPrincipal,
    filter: &leave_dto::LeaveFilter,
    page: u64,
    page_size: u64,
) -> std::result::Result<(Vec<LeaveRes>, i64), ApiError> {
    let (rows, total) = model::page(filter, page, page_size, principal)
        .await
        .map_err(|error| utils::db_error(error, "分页查询请假单"))?;
    Ok((rows.iter().map(|row| row.to_res()).collect(), total))
}

// ==================== ㉗ 详情 ====================

/// 请假单详情（设计 §5.2 #27）：单据本体 + 审批轨迹。
///
/// 回执的形状见 [`LeaveDetailRes`]：`LeaveRes` 的字段**平铺**在顶层，另加
/// `approval`（当前实例 + 时间线）与 `approval_history`（提交过几次）。
#[nik::permission(
    path = "/business/hr/leave/{id}",
    method = "GET",
    permission = "business:hr:leave:detail"
)]
#[nik::get("/business/hr/leave/{id}")]
pub async fn leave_detail(Path(id): Path<String>) -> ResResult<LeaveDetailRes> {
    let leave_id = parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(leave_id).await?))
}

/// 详情组装（接口与真库用例共用同一条路径）。
///
/// 审批轨迹**只读** `flow` 模块的公开读原语（`find_instance` / `nodes_of` / `timeline` /
/// `instances_of_biz`）：审批的真相在 `biz_flow_*` 五表里，`hr` 模块不该有第二份 SQL。
///
/// 实例已不存在（历史脏数据 / 被硬删）时 `approval=None` 而不是 404 —— 详情页的主体是
/// 请假单，审批单缺失只该少一个 tab，不该整页打不开。
pub async fn detail_of(leave_id: Uuid) -> std::result::Result<LeaveDetailRes, ApiError> {
    let row = model::find_in(db(), leave_id)
        .await
        .map_err(|error| utils::db_error(error, "查询请假单"))?
        .ok_or_else(not_found)?;

    let instances = flow_model::instances_of_biz(Some(FLOW_BIZ_TYPE), Some(leave_id), None)
        .await
        .map_err(|error| utils::db_error(error, "查询请假单的审批实例"))?;
    let history = i32::try_from(instances.len()).unwrap_or(i32::MAX);

    // 当前实例优先取单据上挂的 `approval_instance_id`；它没挂但库里有实例（历史脏数据）时
    // 退化成「最新那张」，免得详情页在「单据显示审批中、审批轨迹一片空白」之间自相矛盾。
    let instance = match row.approval_instance_id {
        Some(instance_id) => flow_model::find_instance(instance_id)
            .await
            .map_err(|error| utils::db_error(error, "查询请假审批实例"))?,
        None => instances.first().cloned(),
    };
    let approval = match instance {
        None => None,
        Some(instance) => Some(approval_of(&instance).await?),
    };

    Ok(LeaveDetailRes {
        leave: row.to_res(),
        approval,
        approval_history: history,
    })
}

/// 实例 → 审批轨迹回执（实例行 + 节点名映射 + 时间线）。
async fn approval_of(instance: &flow_model::InstanceRow) -> std::result::Result<LeaveApprovalRes, ApiError> {
    let nodes = flow_model::nodes_of(instance.id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批节点进度"))?;
    // 同一 node_key 的 node_name 是发布快照里的固定值，跨轮次不会变 ⇒ 用一次内存映射
    // 补上时间线（时间线行不带 JOIN，见 `flow_model_progress.rs::timeline`）。
    let node_names: std::collections::HashMap<&str, &str> = nodes
        .iter()
        .map(|node| (node.node_key.as_str(), node.node_name.as_str()))
        .collect();
    let timeline = flow_model::timeline(instance.id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批时间线"))?;
    Ok(LeaveApprovalRes {
        instance_id: instance.id.to_string(),
        instance_no: instance.instance_no.clone(),
        flow_code: instance.flow_code.clone(),
        flow_name: instance.flow_name.clone(),
        status: instance.status.clone(),
        status_label: instance_pdf::instance_status_label(&instance.status).to_string(),
        initiator_name: instance.initiator_name.clone().unwrap_or_default(),
        started_at: timestamp_cst(instance.started_at),
        ended_at: instance.ended_at.map(timestamp_cst),
        result_comment: instance.result_comment.clone(),
        timeline: timeline
            .iter()
            .map(|task| LeaveApprovalTaskRes {
                node_key: task.node_key.clone(),
                node_name: node_names
                    .get(task.node_key.as_str())
                    .map(|name| (*name).to_string())
                    .unwrap_or_else(|| task.node_key.clone()),
                round: task.round,
                assignee_id: task.assignee_id.to_string(),
                assignee_name: task.assignee_name.clone().unwrap_or_default(),
                status: task.status.clone(),
                status_label: task_status_label(&task.status),
                comment: task.comment.clone(),
                is_self_approval: task.is_self_approval,
                is_auto: task.is_auto,
                created_at: timestamp_cst(task.created_at),
                completed_at: task.completed_at.map(timestamp_cst),
            })
            .collect(),
    })
}

// ==================== ㉘ 建单 ====================

/// 建请假单（设计 §5.2 #28）：落库即 `DRAFT`，单号服务端取。
#[nik::permission(
    path = "/business/hr/leave",
    method = "POST",
    permission = "business:hr:leave:create"
)]
#[nik::post("/business/hr/leave")]
pub async fn leave_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<LeaveCreateReq>,
) -> ResResult<LeaveRes> {
    let operator: Operator = (&claims).into();
    Ok(Res::ok(create_leave(operator, &req).await?))
}

/// 建单的业务实现（接口与真库用例共用同一条路径）。
///
/// 事务里的顺序是 `查员工 → 取号 → 插单 → 写建单日志 → 读回`：
/// - **员工快照在同事务里查**：建单与抄快照之间员工被改名/调部门，落库的仍是同一份；
/// - **取号与插入同事务**：一起提交、一起回滚，不留空号（`LV-yyyyMMdd-000001` 出现断号
///   会让审计问「中间那张去哪了」）；
/// - **建单日志与单据同生同灭**：`biz_status_log` 没有建单行，这张单的「谁建的、何时建的」
///   就是空白（设计 §4.2.2 要求状态流转留痕，建单同理）。
pub async fn create_leave(operator: Operator, req: &LeaveCreateReq) -> std::result::Result<LeaveRes, ApiError> {
    let draft = leave_dto::create_of(req)?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建请假单事务"))?;

    // ① 员工必须存在且未删除（FK 是 RESTRICT，但先查一次能报人话）
    let employee = match employee_model::find_in(&tx, draft.employee_id).await {
        Ok(Some(employee)) => employee,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(ApiError::new(
                404,
                error_code::RESOURCE_NOT_FOUND,
                format!(
                    "员工 {} 不存在或已删除：请假单必须挂在一个在职员工上",
                    draft.employee_id
                ),
            ));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询请假员工"));
        }
    };
    // ② 已离职的员工不能再请假（假期是「在职期间的出勤例外」，离职后补单应该走考勤修正）
    if employee.status == EMPLOYEE_STATUS_RESIGNED {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "员工 {}（{}）已离职，不能新建请假单：离职后的考勤差异请走考勤台账修正，\
             不要补一张离职后的请假单",
            employee.name, employee.emp_no
        )));
    }

    // ③ 取号（同事务）→ ④ 插单（员工三列快照取自上面那次读）
    let leave_no = match model::next_leave_no(&tx, Utc::now()).await {
        Ok(no) => no,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "取请假单号"));
        }
    };
    let id = Uuid::now_v7();
    if let Err(error) = model::insert(
        &tx,
        LeaveInsert {
            id,
            leave_no: &leave_no,
            employee_no: &employee.emp_no,
            employee_name: &employee.name,
            dept_id: Some(employee.dept_id),
            draft: &draft,
        },
        operator.id,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(model::write_error(error, "hr_leave_create", &leave_no));
    }

    // ⑤ 建单日志（状态 = DRAFT，与库里的行一致）
    if let Err(error) = state_machine::log_created(
        &tx,
        state_machine::ENTITY_LEAVE_REQUEST,
        id,
        &leave_no,
        leave_dto::STATUS_DRAFT,
        operator,
        Some("新建请假单"),
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }

    // ⑥ 读回（同事务 —— 出参是库里的真实值，不是入参的回显）
    let row = match model::find_in(&tx, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回新建的请假单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交建请假单事务"))?;
    Ok(row.to_res())
}

// ==================== ㉙ 改草稿 ====================

/// 改请假单（设计 §5.2 #29）：**部分更新**，只允许 `DRAFT`。
#[nik::permission(
    path = "/business/hr/leave/{id}",
    method = "PUT",
    permission = "business:hr:leave:update"
)]
#[nik::put("/business/hr/leave/{id}")]
pub async fn leave_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<LeaveUpdateReq>,
) -> ResResult<LeaveRes> {
    let leave_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(update_leave(operator, leave_id, &req).await?))
}

/// 改草稿的业务实现。
///
/// 先锁行再判「能不能改」：否则「读到 DRAFT → 别人提交了 → 本请求照旧写」会把
/// 已经进审批的天数改掉（审批人看的表单快照与库里对不上）。
///
/// **无改动 → 200 + 不写库**（[`leave_dto::LeavePatch::changed`]）：请假表单是整页编辑，
/// 用户点「保存」时可能什么都没改；回 409 会弹一个毫无意义的冲突框，回 200 才是
/// 「保存成功，什么也没变」（与考勤台账的单行修正同口径）。
pub async fn update_leave(
    operator: Operator,
    leave_id: Uuid,
    req: &LeaveUpdateReq,
) -> std::result::Result<LeaveRes, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改请假单事务"))?;
    let row = match model::lock_in(&tx, leave_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定请假单"));
        }
    };
    if !row.is_editable() {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "请假单 {} 当前是「{}」（{}），只有草稿（DRAFT）可以修改：\
             审批中的请先撤回，已批准的只能作废重开",
            row.leave_no,
            leave_dto::status_label(&row.status),
            row.status
        )));
    }
    let patch = match leave_dto::patch_of(req, &row.draft_state()) {
        Ok(patch) => patch,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if !patch.changed {
        // 无改动：不写库、不写状态日志，直接回当前值（事务回滚释放行锁）
        let _ = tx.rollback().await;
        return Ok(row.to_res());
    }
    match model::update(&tx, leave_id, &patch, operator.id).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            return Err(conflict(format!(
                "请假单 {} 的状态在本次修改期间已被改动（期望草稿），请刷新后重试",
                row.leave_no
            )));
        }
        Ok(_) => {}
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(model::write_error(error, "hr_leave_update", &row.leave_no));
        }
    }
    let updated = match model::find_in(&tx, leave_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回修改后的请假单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交改请假单事务"))?;
    Ok(updated.to_res())
}

// ==================== ㉚ 提交审批 ====================

/// 提交请假单审批（设计 §5.2 #30）：`DRAFT → SUBMITTED` + 发起 `hr_leave` 实例。
#[nik::permission(
    path = "/business/hr/leave/{id}/submit",
    method = "POST",
    permission = "business:hr:leave:submit"
)]
#[nik::post("/business/hr/leave/{id}/submit")]
pub async fn leave_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<LeaveSubmitReq>,
) -> ResResult<LeaveRes> {
    let leave_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(submit_leave(operator, leave_id, &req).await?))
}

/// 提交审批的业务实现。
///
/// # 闸门顺序（越早拒绝越好，全部在同一个事务里）
///
/// 1. 只有 `DRAFT` 能提交（审批中的要先撤回；已批准的只能作废重开）；
/// 2. 同一单已有在跑的审批 → 409（幂等保护，避免撞 `uk_biz_flow_instance_biz_active`）；
/// 3. 没有已发布的 `hr_leave` 流程 → `approval_gate::authorize_missing_flow`
///    （409 + `66033`，点名流程编码）。**不做「没流程就直接生效」**（设计 §4.3 第 3 条）；
/// 4. 起实例 → 状态机 `DRAFT → SUBMITTED` → CAS 回写 `status` + `approval_instance_id`；
/// 5. 流程里没有审批节点（全被跳过 / 免审配置）⇒ 引擎在同事务里直接 `APPROVED`，
///    本函数立刻把单据推到 `APPROVED` —— **绝不留「实例已通过、单据还在审批中且无待办」的死单**。
pub async fn submit_leave(
    operator: Operator,
    leave_id: Uuid,
    req: &LeaveSubmitReq,
) -> std::result::Result<LeaveRes, ApiError> {
    let remark = leave_dto::submit_of(req)?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启提交请假单事务"))?;
    let row = match model::lock_in(&tx, leave_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定请假单"));
        }
    };
    // ① 状态闸
    if row.status != leave_dto::STATUS_DRAFT {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "请假单 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交审批{}",
            row.leave_no,
            leave_dto::status_label(&row.status),
            row.status,
            if row.status == leave_dto::STATUS_SUBMITTED {
                "：审批中的单请先撤回"
            } else {
                "：已批准的假只能作废重开"
            }
        )));
    }
    // ② 幂等：已有在跑实例（正常不会发生 —— ① 已经挡住非草稿）
    match flow_model::biz_instance_running_in(&tx, FLOW_BIZ_TYPE, leave_id).await {
        Ok(false) => {}
        Ok(true) => {
            let _ = tx.rollback().await;
            return Err(conflict(format!(
                "请假单 {} 已在审批中：请等审批结束，或先撤回再提交",
                row.leave_no
            )));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询请假单在跑的审批"));
        }
    }
    // ③ 流程必须已发布（fail-closed）。请假单不提供 skip-approval 旁路：
    //    状态机没有 `DRAFT → APPROVED` 边，跳过审批只能绕过边表（见模块文档）。
    let flow = match published_leave_flow().await {
        Ok(flow) => flow,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询请假审批流程"));
        }
    };
    let Some(flow) = flow else {
        let _ = tx.rollback().await;
        return Err(
            approval_gate::authorize_missing_flow(FLOW_BIZ_TYPE, FLOW_CODE, &ApprovalGate::none())
                .expect_err("没有已发布流程时 authorize_missing_flow 必须返回错误（fail-closed）"),
        );
    };
    let Some(operator_id) = operator.id else {
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_INVALID,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let (initiator_name, initiator_dept_id) = match flow_model::user_profile(operator_id).await {
        Ok(profile) => profile.unwrap_or((String::new(), None)),
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "查询提交人"));
        }
    };
    // ④ 起实例
    let instance_id = Uuid::now_v7();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(leave_id),
        biz_no: Some(row.leave_no.clone()),
        title: format!("{} {} 请假 {} 天", row.leave_no, row.employee_name, row.duration_days)
            .chars()
            .take(200)
            .collect::<String>(),
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: leave_form_data(&row),
    };
    let mut out = match engine::start(&tx, &start).await {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(model::write_error(error, "发起请假审批单", &row.leave_no));
        }
    };
    if let Some(rejected) = out.rejected.take() {
        let _ = tx.rollback().await;
        // FIX-1：`appended` 而不是重建 —— 重建会丢 65018 的结构化载荷（前端「指派审批人」引导）
        let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
            "（内控要求：审批人不能是发起人本人。请让上级提交，或给上级配审批权限后再审）"
        } else {
            ""
        };
        return Err(rejected.appended(hint));
    }
    let started = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ⑤ 状态机（唯一入口：边校验 + 写 biz_status_log）
    let log_reason = remark.as_deref().unwrap_or("提交审批");
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_LEAVE_REQUEST,
        leave_id,
        &row.leave_no,
        &state_machine::LEAVE_EDGES,
        leave_dto::STATUS_DRAFT,
        leave_dto::STATUS_SUBMITTED,
        operator.clone(),
        Some(log_reason),
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    // ⑥ CAS 回写状态 + 实例 id（状态机只管日志，不回写业务表）
    if let Err(error) = apply_status(
        &tx,
        leave_id,
        &row.leave_no,
        leave_dto::STATUS_DRAFT,
        leave_dto::STATUS_SUBMITTED,
        None,
        operator.id,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    if let Err(error) = model::set_approval_instance(&tx, leave_id, Some(started), operator.id).await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "回写请假单审批实例"));
    }

    // ⑦ 免审直过：流程里没有审批节点（全跳过 / 免审配置）⇒ 实例已在同事务里 APPROVED。
    //    **必须在同一个事务里把单据推到 APPROVED**：留到提交后再做，中途失败就变成
    //    「实例已通过、单据停在 SUBMITTED、没有任何待办能推进它」的死单（AGENTS.md 约定 11）。
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    if auto_finished {
        if let Err(error) = state_machine::transition(
            &tx,
            state_machine::ENTITY_LEAVE_REQUEST,
            leave_id,
            &row.leave_no,
            &state_machine::LEAVE_EDGES,
            leave_dto::STATUS_SUBMITTED,
            leave_dto::STATUS_APPROVED,
            Operator::named(Some(operator_id), operator.name.clone().unwrap_or_default()),
            Some("审批免点（流程未配置审批节点）：同事务直接通过"),
            None,
        )
        .await
        {
            let _ = tx.rollback().await;
            return Err(error);
        }
        if let Err(error) = apply_status(
            &tx,
            leave_id,
            &row.leave_no,
            leave_dto::STATUS_SUBMITTED,
            leave_dto::STATUS_APPROVED,
            None,
            operator.id,
        )
        .await
        {
            let _ = tx.rollback().await;
            return Err(error);
        }
    }

    let updated = match model::find_in(&tx, leave_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回提交后的请假单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交请假单审批事务"))?;

    // ⑧ 锁外副作用：审批通知 / 客户自配的通知节点。**提交之后**才发，失败只 WARN
    //    （状态与实例已经落库，一封站内信失败不该把审批撤回去）。
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        leave_id = %leave_id,
        leave_no = %row.leave_no,
        instance_id = %started,
        flow_code = %FLOW_CODE,
        auto_finished,
        "请假单已提交审批"
    );
    Ok(updated.to_res())
}

/// 发起请假单时冻结的表单变量（`biz_flow_instance.form_data`）。
///
/// 为什么把**中文名**一起冻进去：审批节点常常派给没有 `business:hr:leave:*` 读权限的岗位，
/// 审批人点开待办要能直接看懂「谁请了什么假」，而不是一串 uuid；
/// `duration_days` 写成 **JSON 字符串**（`Decimal` 序列化口径与出参一致，不做浮点转换）。
///
/// **另给一个数值镜像 `duration_days_num`**（HR-26 加入）：
/// `hr_leave` 流程的「请假 ≥ 3 天 → 人事经理复核」是**条件边**
/// （`biz_flow_edge.condition_expr` = `form.duration_days_num gte 3`），而条件求值器
/// （`flow::graph::compare`）对 `gt/gte/lt/lte` 一律 `Value::as_f64()` —— 左值是字符串
/// 就 `TypeMismatch`，引擎按「条件不成立」**静默走兜底边**（只有 WARN 日志）。
/// 也就是说：只留字符串形态会让长假**绕过人事复核**，且不报任何错。
/// 故这里保留字符串（展示 / 出参口径不变）**并**补一个同源同值的 JSON number 供条件求值。
/// 两边的钉子：本函数单测 + `tests/hr_26_leave_flow_seed.rs`（种子里引用的字段必须是 number）。
fn leave_form_data(row: &model::LeaveRow) -> serde_json::Value {
    let mut form = serde_json::json!({
        "leave_id": row.id.to_string(),
        "leave_no": row.leave_no,
        "employee_id": row.employee_id.to_string(),
        "employee_no": row.employee_no,
        "employee_name": row.employee_name,
        "dept_id": row.dept_id.map(|id| id.to_string()),
        "dept_name": row.dept_name,
        "leave_type": row.leave_type,
        "leave_type_label": leave_dto::leave_type_label(&row.leave_type),
        "start_time": timestamp_cst(row.start_time),
        "end_time": timestamp_cst(row.end_time),
        "duration_days": row.duration_days.to_string(),
        "paid": row.paid,
        "paid_label": leave_dto::paid_label(row.paid),
        "reason": row.reason,
    });
    if let Ok(days) = row.duration_days.to_string().parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(days)
    {
        form["duration_days_num"] = serde_json::Value::Number(number);
    }
    form
}

/// 状态 CAS 回写 + 「影响 0 行」翻译成人话（409）。
#[allow(clippy::too_many_arguments)]
async fn apply_status(
    tx: &Transaction,
    leave_id: Uuid,
    leave_no: &str,
    from: &str,
    to: &str,
    cancel_reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let affected = match model::set_status(tx, leave_id, from, to, cancel_reason, operator).await {
        Ok(affected) => affected,
        Err(error) => return Err(model::write_error(error, "回写请假单状态", leave_no)),
    };
    if affected == 0 {
        return Err(conflict(format!(
            "请假单 {leave_no} 的状态在本次操作期间已被改动（期望 {from}），请刷新后重试"
        )));
    }
    Ok(())
}

/// 查「已发布的请假审批流程」。`None` = **没配流程**（不是错误，由调用方按
/// `approval_gate` 的 fail-closed 口径处理）。
///
/// 排序口径与 `erp::contract_bridge::published_contract_flow` 完全一致
/// （`biz_type` 精确命中优先 → `is_default` → 版本号 → 更新时间），
/// 免得同一个库里「合同走 A 流程、请假走 B 流程」这种两套口径。
async fn published_leave_flow() -> DbResult<Option<LeaveFlow>> {
    // SCOPE-EXEMPT: 流程定义（biz_flow_def）是全局配置，按 biz_type / flow_code 精确取 1 行。
    let rows = db()
        .find_with_params(
            "SELECT d.id AS def_id, d.flow_code, d.flow_name, d.biz_type, \
                    d.current_version_id AS version_id, d.current_version_no AS version_no \
               FROM biz_flow_def d \
              WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED' \
                AND d.current_version_id IS NOT NULL \
                AND (d.biz_type = ? OR lower(d.flow_code) = ?) \
              ORDER BY (d.biz_type = ?) DESC, d.is_default DESC, \
                       d.current_version_no DESC, d.updated_at DESC \
              LIMIT 1",
            vec![
                Value::Text(FLOW_BIZ_TYPE.to_string()),
                Value::Text(FLOW_CODE.to_string()),
                Value::Text(FLOW_BIZ_TYPE.to_string()),
            ],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(LeaveFlow {
            def_id: row.get_typed("def_id")?,
            flow_code: row.get_typed("flow_code")?,
            flow_name: row.get_typed("flow_name")?,
            biz_type: row.get_typed("biz_type")?,
            version_id: row.get_typed("version_id")?,
            version_no: row.get_typed("version_no")?,
        })),
    }
}

/// 已发布的请假审批流程（`hr_leave`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveFlow {
    /// 流程定义 id。
    pub def_id: Uuid,
    /// 流程编码。
    pub flow_code: String,
    /// 流程名称。
    pub flow_name: String,
    /// 业务类型（`HR_LEAVE`）。
    pub biz_type: String,
    /// 当前发布版本 id。
    pub version_id: Uuid,
    /// 当前发布版本号。
    pub version_no: i32,
}

// ==================== ㉛ 作废 / 撤回 / 删除 ====================

/// 作废请假单（设计 §5.2 #31）：一条路径承载「删草稿 / 撤回 / 销假」三件事。
///
/// 走哪条由单据**当前状态**决定（[`LeaveCancelAction::of_status`]），回执的 `action`
/// 会告诉调用方做的是哪一件。`reason` 必填且 ≥ 5 字（设计 §5.2 #31）：
/// 三个动作都会改掉考勤/工资口径或在审批链上留痕，必须能交代为什么。
#[nik::permission(
    path = "/business/hr/leave/{id}/cancel",
    method = "POST",
    permission = "business:hr:leave:cancel"
)]
#[nik::post("/business/hr/leave/{id}/cancel")]
pub async fn leave_cancel(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<LeaveCancelReq>,
) -> ResResult<LeaveCancelRes> {
    let leave_id = parse_uuid(&id, "id")?;
    let operator: Operator = (&claims).into();
    Ok(Res::ok(cancel_leave(operator, leave_id, &req).await?))
}

/// 作废 / 撤回 / 删除的业务实现。
///
/// # 三条路的共同点与不同点
///
/// 三条路都：锁行 → 判状态 → 写库 → 提交；`CANCELLED` / `Withdrawn` 还都要走状态机
/// （`APPROVED → CANCELLED` 是必填原因边；`SUBMITTED → DRAFT` 是撤回边）。
/// 只有 `Deleted` **不碰状态机**：草稿从未提交过，软删不是状态流转。
///
/// # 撤回为什么要先终止实例
///
/// `SUBMITTED → DRAFT` 之后单据回到草稿可改可重提，而那张实例还在 `PENDING`：
/// ① 审批人待办里会出现一张「已经回到草稿」的单，审批人点同意会推一个不存在的状态；
/// ② 重提时要新建实例，`uk_biz_flow_instance_biz_active`（同一 `biz_type + biz_id`
/// 只允许一张在跑）会撞唯一键 → 500。
pub async fn cancel_leave(
    operator: Operator,
    leave_id: Uuid,
    req: &LeaveCancelReq,
) -> std::result::Result<LeaveCancelRes, ApiError> {
    let cancel = leave_dto::cancel_of(req)?;
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废请假单事务"))?;
    let row = match model::lock_in(&tx, leave_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定请假单"));
        }
    };
    let Some(action) = LeaveCancelAction::of_status(&row.status) else {
        let _ = tx.rollback().await;
        return Err(rule_blocked(format!(
            "请假单 {} 当前是「{}」（{}），没有可执行的作废动作{}",
            row.leave_no,
            leave_dto::status_label(&row.status),
            row.status,
            if row.status == leave_dto::STATUS_CANCELLED {
                "：这张单已经作废过了"
            } else {
                ""
            }
        )));
    };

    let instance_status = match action {
        // ---- ① 草稿：软删（状态不变） ----
        LeaveCancelAction::Deleted => {
            match model::soft_delete(&tx, leave_id, operator.id).await {
                Ok(0) => {
                    let _ = tx.rollback().await;
                    return Err(conflict(format!(
                        "请假单 {} 已被他人删除或已不是草稿，请刷新后重试",
                        row.leave_no
                    )));
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(model::write_error(error, "删除请假单草稿", &row.leave_no));
                }
            }
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交删除请假单事务"))?;
            tracing::info!(leave_id = %leave_id, leave_no = %row.leave_no, "请假单草稿已删除");
            return Ok(LeaveCancelRes {
                leave: row.to_res(),
                action: action.as_str().to_string(),
                action_label: action.label().to_string(),
                instance_status: None,
            });
        }
        // ---- ② 审批中：撤回（终止实例 + 回草稿） ----
        LeaveCancelAction::Withdrawn => {
            let terminated = match leave_bridge::cancel_approval_of(&tx, &row).await {
                Ok(status) => status,
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
            };
            if let Err(error) = state_machine::transition(
                &tx,
                state_machine::ENTITY_LEAVE_REQUEST,
                leave_id,
                &row.leave_no,
                &state_machine::LEAVE_EDGES,
                leave_dto::STATUS_SUBMITTED,
                leave_dto::STATUS_DRAFT,
                operator.clone(),
                Some(&cancel.reason),
                None,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(error);
            }
            // 撤回不写 `cancel_reason`（那一列是「作废原因」，`ck_biz_leave_cancel` 只在
            // `CANCELLED` 时要求它）；撤回理由留在 `biz_status_log.reason`。
            if let Err(error) = apply_status(
                &tx,
                leave_id,
                &row.leave_no,
                leave_dto::STATUS_SUBMITTED,
                leave_dto::STATUS_DRAFT,
                None,
                operator.id,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(error);
            }
            // 实例已终止 ⇒ 清掉单据上的实例引用，重提时新建一张（历史轨迹在 flow 五表里）
            if let Err(error) = model::set_approval_instance(&tx, leave_id, None, operator.id).await {
                let _ = tx.rollback().await;
                return Err(utils::db_error(error, "清空请假单审批实例引用"));
            }
            terminated
        }
        // ---- ③ 已批准：销假（进 CANCELLED，原因必填） ----
        LeaveCancelAction::Cancelled => {
            if let Err(error) = state_machine::transition(
                &tx,
                state_machine::ENTITY_LEAVE_REQUEST,
                leave_id,
                &row.leave_no,
                &state_machine::LEAVE_EDGES,
                leave_dto::STATUS_APPROVED,
                leave_dto::STATUS_CANCELLED,
                operator.clone(),
                Some(&cancel.reason),
                None,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(error);
            }
            // 库层 `ck_biz_leave_cancel` 要求 `CANCELLED` 必须带 `cancel_reason` ⇒ 与状态同语句写
            if let Err(error) = apply_status(
                &tx,
                leave_id,
                &row.leave_no,
                leave_dto::STATUS_APPROVED,
                leave_dto::STATUS_CANCELLED,
                Some(&cancel.reason),
                operator.id,
            )
            .await
            {
                let _ = tx.rollback().await;
                return Err(error);
            }
            None
        }
    };

    let updated = match model::find_in(&tx, leave_id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found());
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读回作废后的请假单"));
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交作废请假单事务"))?;
    tracing::info!(
        leave_id = %leave_id,
        leave_no = %row.leave_no,
        action = action.as_str(),
        "请假单状态已变更（撤回 / 销假）"
    );
    Ok(LeaveCancelRes {
        leave: updated.to_res(),
        action: action.as_str().to_string(),
        action_label: action.label().to_string(),
        instance_status,
    })
}

// 实例终止已搬到 [`leave_bridge::cancel_approval_of`]（HR-27）：撤回 / 作废都要终止实例，
// 那段逻辑与审批回调桥属于同一件事（「单据 ↔ 审批实例」的边界），放在桥里只有一处口径。

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;

    /// ⓪ 本地「离职」常量必须仍在 `employee_dto::STATUS_VALUES` 里（数组改了这里要跟着改）。
    #[test]
    fn resigned_constant_matches_employee_domain() {
        assert!(crate::business::hr::employee_dto::STATUS_VALUES.contains(&EMPLOYEE_STATUS_RESIGNED));
    }

    /// ① 作废三岔口的状态映射（本卡验收项「单测覆盖撤回/作废边界」的第一层）。
    ///
    /// 钉死「哪个状态走哪条路」：`SUBMITTED` 必须是**撤回**（终止实例 + 回草稿），
    /// 不能是 `CANCELLED` —— 状态机里没有 `SUBMITTED → CANCELLED` 这条边。
    #[test]
    fn cancel_action_maps_each_status() {
        use LeaveCancelAction::{Cancelled, Deleted, Withdrawn};
        for (status, expected) in [
            ("DRAFT", Deleted),
            (" dRaft ", Deleted),
            ("SUBMITTED", Withdrawn),
            ("APPROVED", Cancelled),
        ] {
            assert_eq!(
                LeaveCancelAction::of_status(status),
                Some(expected),
                "{status} 的作废动作判错"
            );
        }
        // 已作废 / 未知状态一律没有作废路径（409），不许静默重放
        for status in ["CANCELLED", "", "PENDING", "DELETED"] {
            assert_eq!(LeaveCancelAction::of_status(status), None, "{status} 不该有作废路径");
        }
    }

    /// ② 三个动作的码值 / 中文名稳定（前端按 `action` 分支，改字即破坏契约）。
    #[test]
    fn cancel_action_codes_are_stable() {
        assert_eq!(LeaveCancelAction::Deleted.as_str(), "DELETED");
        assert_eq!(LeaveCancelAction::Withdrawn.as_str(), "WITHDRAWN");
        assert_eq!(LeaveCancelAction::Cancelled.as_str(), "CANCELLED");
        assert_eq!(LeaveCancelAction::Deleted.label(), "草稿已删除");
        assert_eq!(LeaveCancelAction::Withdrawn.label(), "已撤回（回到草稿）");
        assert_eq!(LeaveCancelAction::Cancelled.label(), "已作废（销假）");
    }

    /// ③ **撤回边与作废边必须在状态机里存在，且作废边要求原因**（本卡验收项的主判据）。
    ///
    /// 这条用例挡的是「有人在 `shared/state_machine` 里删边 / 把原因边改成可选」：
    /// 删了 `SUBMITTED → DRAFT`，撤回会在 `transition` 里 500；把
    /// `APPROVED → CANCELLED` 从 `LEAVE_REASON_EDGES` 拿掉，销假就能不带原因。
    #[test]
    fn withdraw_and_cancel_edges_are_registered() {
        let entity = state_machine::Entity::LeaveRequest;
        assert!(
            entity.can_transition(leave_dto::STATUS_SUBMITTED, leave_dto::STATUS_DRAFT),
            "撤回边 SUBMITTED → DRAFT 必须在 LEAVE_EDGES 里"
        );
        assert!(
            entity.can_transition(leave_dto::STATUS_APPROVED, leave_dto::STATUS_CANCELLED),
            "销假边 APPROVED → CANCELLED 必须在 LEAVE_EDGES 里"
        );
        assert!(
            !entity.can_transition(leave_dto::STATUS_SUBMITTED, leave_dto::STATUS_CANCELLED),
            "审批中不能直接作废：必须先撤回（否则留下在跑实例）"
        );
        // 只有 APPROVED → CANCELLED 需要原因；撤回不强制（同一条边也承载「驳回」）
        let reason_edges = state_machine::LEAVE_REASON_EDGES;
        assert!(reason_edges.contains(&(leave_dto::STATUS_APPROVED, leave_dto::STATUS_CANCELLED)));
        assert!(!reason_edges.contains(&(leave_dto::STATUS_SUBMITTED, leave_dto::STATUS_DRAFT)));
    }

    /// ④ 锁死态与「可编辑态」互斥：`APPROVED` / `CANCELLED` 既不能改也不能删。
    #[test]
    fn locked_statuses_are_not_editable() {
        for status in state_machine::LEAVE_LOCKED {
            assert!(leave_dto::is_locked(status), "{status} 必须是锁死态");
            assert!(
                !leave_dto::EDITABLE_STATUSES.contains(&status),
                "{status} 不能出现在可编辑态里"
            );
            assert_eq!(
                LeaveCancelAction::of_status(status),
                (status == leave_dto::STATUS_APPROVED).then_some(LeaveCancelAction::Cancelled),
                "{status} 的作废判定与锁死口径不一致"
            );
        }
        assert!(leave_dto::EDITABLE_STATUSES.contains(&leave_dto::STATUS_DRAFT));
    }

    /// ⑤ 流程常量与设计 §4.3 的表逐字一致（写错 = 提交入口永远 409 且看起来像「没配流程」）。
    #[test]
    fn flow_constants_match_design() {
        assert_eq!(FLOW_BIZ_TYPE, "HR_LEAVE");
        assert_eq!(FLOW_CODE, "hr_leave");
        // 与词表（`business/shared/doc_kind.rs`）里的 DocKind 落库值对齐
        assert_eq!(state_machine::ENTITY_LEAVE_REQUEST, "LEAVE_REQUEST");
    }

    /// ⑥ 六个权限码就是 `V0387` 种子里的那 6 个（写错 → 线上只有超管能用）。
    #[test]
    fn permission_codes_match_seed() {
        const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");
        for code in [
            PERM_LIST,
            PERM_DETAIL,
            PERM_CREATE,
            PERM_UPDATE,
            PERM_SUBMIT,
            PERM_CANCEL,
        ] {
            assert!(
                V0387_SEED.contains(code),
                "权限码 {code} 不在 V0387 的菜单种子里 —— 线上会 403"
            );
        }
    }

    /// ⑦ 待办状态的中文名（审批轨迹显示；未知码值原样透出，不吞）。
    #[test]
    fn task_status_labels() {
        assert_eq!(task_status_label("PENDING"), "待审批");
        assert_eq!(task_status_label("approved"), "已同意");
        assert_eq!(task_status_label("SKIPPED"), "已跳过");
        assert_eq!(task_status_label("SOMETHING_NEW"), "SOMETHING_NEW");
    }

    /// ⑧ 表单变量：人看的字段齐全，`duration_days` 是字符串（不做浮点）。
    #[test]
    fn form_data_freezes_human_readable_fields() {
        let row = model::LeaveRow {
            id: Uuid::now_v7(),
            leave_no: "LV-20261112-000001".to_string(),
            employee_id: Uuid::now_v7(),
            employee_no: "EMP-0007".to_string(),
            employee_name: "张三".to_string(),
            dept_id: None,
            dept_name: "生产部".to_string(),
            leave_type: "ANNUAL".to_string(),
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_days: Decimal::new(15, 1),
            paid: 1,
            status: leave_dto::STATUS_DRAFT.to_string(),
            reason: Some("回家".to_string()),
            approval_instance_id: None,
            submitted_at: None,
            submitted_by: None,
            submitted_by_name: None,
            cancelled_at: None,
            cancelled_by: None,
            cancelled_by_name: None,
            cancel_reason: None,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let form = leave_form_data(&row);
        assert_eq!(form["leave_no"], "LV-20261112-000001");
        assert_eq!(form["employee_name"], "张三");
        assert_eq!(form["leave_type_label"], "年假");
        assert_eq!(form["paid_label"], "带薪");
        assert_eq!(form["duration_days"], "1.5", "天数必须是字符串（前端不做浮点）");
        assert_eq!(form["dept_name"], "生产部");
        assert_eq!(form["reason"], "回家");
        // ★ HR-26：数值镜像（`hr_leave` 的「≥3 天 → 人事经理复核」条件边只认它）。
        //   必须是 JSON **number**：条件求值器对 gte 走 `Value::as_f64()`，
        //   字符串会 TypeMismatch → 静默走兜底边 → 长假绕过人事复核。
        assert_eq!(
            form["duration_days_num"],
            serde_json::json!(1.5),
            "duration_days_num 必须是数值（与字符串同源同值）"
        );
        assert!(
            form["duration_days_num"].is_number(),
            "duration_days_num 必须是 JSON number，不能是字符串"
        );
    }

    /// ★ HR-26：3 天边界（条件边是 `gte 3`：恰好 3 天要复核，2.5 天不要）。
    ///
    /// 这里断言的是**镜像值与阈值的相对关系**，阈值本身在迁移
    /// `V0393__hr_leave_flow.up.sql` 里（`tests/hr_26_leave_flow_seed.rs` 读文件对拍）。
    #[test]
    fn duration_days_num_mirrors_the_decimal_at_the_three_day_boundary() {
        for (scaled, expect_days, crosses) in [(25i64, 2.5_f64, false), (30, 3.0, true), (35, 3.5, true)] {
            let mut row = sample_row();
            row.duration_days = Decimal::new(scaled, 1);
            let form = leave_form_data(&row);
            assert_eq!(form["duration_days_num"], serde_json::json!(expect_days));
            let days = form["duration_days_num"].as_f64().expect("必须是数值");
            assert_eq!(
                days >= 3.0,
                crosses,
                "{} 天：>= 3 应为 {crosses}（条件边阈值 3）",
                expect_days
            );
        }
    }

    /// 测试用请假单行（字段齐全，值可预测）。
    fn sample_row() -> model::LeaveRow {
        model::LeaveRow {
            id: Uuid::now_v7(),
            leave_no: "LV-20261112-000001".to_string(),
            employee_id: Uuid::now_v7(),
            employee_no: "EMP-0007".to_string(),
            employee_name: "张三".to_string(),
            dept_id: None,
            dept_name: "生产部".to_string(),
            leave_type: "ANNUAL".to_string(),
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_days: Decimal::new(15, 1),
            paid: 1,
            status: leave_dto::STATUS_DRAFT.to_string(),
            reason: Some("回家".to_string()),
            approval_instance_id: None,
            submitted_at: None,
            submitted_by: None,
            submitted_by_name: None,
            cancelled_at: None,
            cancelled_by: None,
            cancelled_by_name: None,
            cancel_reason: None,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ==================== 列表：闸只有路由注解那一道（t_be54535c → t_0af7ec46） ====================

    /// 解耦后 `/leave/page` 的闸**只有框架路由注解那一道**（卡 t_0af7ec46）。
    ///
    /// 页面码 `leave:page` 与全量列表码 `leave:list` 已经分开授予（迁移 `V0401`）：
    /// 员工不再持有 `leave:list` ⇒ 路由闸直接 403，而他自己的请假走 `/leave/my`。
    /// 若有人把 handler 级的第二道闸加回来，说明「页面码 ⇒ 全量码」的老耦合又回来了
    /// （加回来也不会更安全，只会让员工的自助入口被重复收窄）。
    #[test]
    fn list_gate_is_the_route_annotation_only() {
        let source = include_str!("leave_handler.rs");
        assert!(
            source.contains(&format!("permission = \"{}\"", super::PERM_LIST)),
            "列表路由的 #[nik::permission] 注解里没有 {}",
            super::PERM_LIST
        );
        // 拼出来查：直接写字面量会让本用例自己的源码命中这条断言（永远是 true）。
        // 判的是「函数还在不在」（`fn …`），不是「这个名字有没有被提到」——
        // 文档注释里提到它、解释为什么退休，是**应该**有的。
        let forbidden = concat!("fn scope_to_self", "_without_detail");
        assert!(
            !source.contains(forbidden),
            "handler 里又出现了第二道数据闸「{forbidden}」：页面码已与接口码解耦，闸应退休"
        );
        // 再守一道：调用点也不许有（有人留着函数体只改名不算修好）。
        let called = concat!("let query = scope_to_self", "_without_detail(&claims, query);");
        assert!(
            !source.contains(called),
            "leave_page 里又调用了第二道数据闸：员工自助的收窄应交给路由闸 + /leave/my"
        );
    }

    /// 闸门用的常量必须与 `#[nik::permission]` 注解里写的码**逐字一致**。
    ///
    /// 两处分离的代价：注解写 `leave:detail`、常量写成 `leave:details` 时编译照样过，
    /// 只会在线上把「能看别人」的角色也一起收窄成「只看自己」（静默的功能缩水）。
    #[test]
    fn detail_permission_constant_matches_the_annotation() {
        let source = include_str!("leave_handler.rs");
        assert!(
            source.contains(&format!("permission = \"{}\"", leave_dto::PERM_LEAVE_DETAIL)),
            "leave_handler 的 #[nik::permission] 注解里没有 {} —— 常量与注解已分叉",
            leave_dto::PERM_LEAVE_DETAIL
        );
    }
}

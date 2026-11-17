//! 采购计划接口层（M4-02）：建单 / 改单 / 删单 / 分页 / 详情 / **提交审批**。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/purchase-plan/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST   | `/business/erp/purchase-plan` | `business:erp:purchase-plan:create` | 建计划（`PPL-yyyyMMdd-000001`，`DRAFT`） |
//! | PUT    | `/business/erp/purchase-plan/{id}` | `business:erp:purchase-plan:update` | 整单替换（只有 `DRAFT`） |
//! | DELETE | `/business/erp/purchase-plan/{id}` | `business:erp:purchase-plan:delete` | 软删（只有 `DRAFT`） |
//! | GET    | `/business/erp/purchase-plan/page` | `business:erp:purchase-plan:list` | 分页（关键字 / 状态 / 类型 / 年度 / 部门） |
//! | GET    | `/business/erp/purchase-plan/{id}` | `business:erp:purchase-plan:list` | 详情（单头 + 明细） |
//! | POST   | `/business/erp/purchase-plan/{id}/submit` | `business:erp:purchase-plan:submit` | 提交审批（走 `pp_approve` 流程） |
//!
//! 转采购申请（`POST …/{id}/to-requisition`，M4-03）在
//! [`crate::business::erp::purchase::purchase_plan_convert_handler`] —— 那边要连带
//! 写申请单与已执行量，和本文件的「单据自身状态机」不是一件事，所以另开一份。
//!
//! # 三条硬规则（每条都有落库证据）
//!
//! ```text
//! ① 走审批流，不硬编码：提交时查「已发布的 pp_approve 流程」——
//!    有 → 同一事务里「发起审批 + CAS 状态 DRAFT→SUBMITTED + 写实例 id」；
//!    没有 → **默认 409**，只有持 business:erp:skip-approval + ≥10 字理由才直接 APPROVED
//!    （理由写进备注）。代码里没有任何「金额 > X 就加签」的 if —— 分级在流程定义的条件边里。
//! ② 单头三个 total_* 是派生列：请求体里带了也不读，一律 `recalc_totals` 重算
//!    （M4-01 的交接注释第 1 条）。
//! ③ 月度计划的 parent_plan_id 必须是**年度**计划且同年度（库层 CHECK 只拦自指，
//!    跨行语义在这里判 —— M4-01 交接注释第 2 条）。
//! ```
//!
//! # 幂等
//!
//! * 提交：`SELECT … FOR UPDATE` 锁单头 + 状态机 CAS（`DRAFT → SUBMITTED`）+
//!   `biz_instance_running` 双保险 —— 重复提交 409 且带审批单号；
//! * 改单 / 删单：CAS（`WHERE status='DRAFT'`），并发的第二个请求拿到 0 行 → 409。
//!
//! # 事务提交后失败必须有补偿（AGENTS 约定 11）
//!
//! 免审直过分支（流程没有审批节点 → 实例当场 `APPROVED`）在事务提交后立即回调
//! `purchase_plan_bridge::on_pp_settled`。它失败时单据会停在 `SUBMITTED` 且**没有待办**
//! 能推进它，所以这里在失败路径上**显式打回草稿**并对外报错（用户能重新提交）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_purchase_plan / _item 与采购申请单同口径（计划是公司级滚动文件，
//!        采购要看全公司）；`dept_id` 是「编制部门」快照语义而不是数据范围列。
//!        分页走 model::page_plans（内部 scope::paginate_unscoped，闸①口径）。
//! ```

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::{self, ApprovalGate};
use crate::business::erp::purchase::purchase_model;
use crate::business::erp::purchase::purchase_plan_bridge;
use crate::business::erp::purchase::purchase_plan_dto::{
    self as dto, CreatePlanReq, PLAN_PERMISSIONS, PlanDetailRes, PlanFilter, PlanItemRes, PlanPageQuery, PlanRowRes,
    SubmitPlanReq, check_cells_unique, check_items, check_month_for_type, check_name, check_parent_period, check_price,
    check_qty, check_type, check_year, clean_remark, parse_optional_uuid, parse_uuid_field, status_label,
};
use crate::business::erp::purchase::purchase_plan_model::{
    self as model, NewPlanHead, NewPlanItem, PlanItemRow, PlanRow,
};
use crate::business::erp::unit_guard;
use crate::business::flow::engine;
use crate::business::flow::model as flow_model;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 权限码（与 V0447 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 + 详情。
pub const PERM_LIST: &str = "business:erp:purchase-plan:list";
/// 新建。
pub const PERM_CREATE: &str = "business:erp:purchase-plan:create";
/// 修改。
pub const PERM_UPDATE: &str = "business:erp:purchase-plan:update";
/// 删除。
pub const PERM_DELETE: &str = "business:erp:purchase-plan:delete";
/// 提交审批。
pub const PERM_SUBMIT: &str = "business:erp:purchase-plan:submit";
/// 转采购申请（M4-03）。
pub const PERM_TO_REQUISITION: &str = "business:erp:purchase-plan:to-requisition";

// 代码里断言一份常量与 DTO 的清单一致：`perm_menu_gate` 只认字面量，
// 但两份清单漂移时这里会红，不会等到运行时 403。
const _: () = assert!(PLAN_PERMISSIONS.len() == 6);

/// 单据中文名（409 / 404 报文用）。
const LABEL: &str = "采购计划";

// ==================== 错误出口 ====================

fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

fn conflict(code: i32, message: impl Into<String>) -> ApiError {
    ApiError::new(409, code, message.into())
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("采购计划不存在或已删除（{id}）"),
    )
}

fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// `DbError` → `ApiError`（写库阶段的统一出口）。
fn write_error(error: DbError, context: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{context}失败：{error}"))
}

/// 发起审批撞唯一索引时的 409（同一张单不允许两个在跑的审批）。
fn submit_conflict(error: DbError, context: &str, plan_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&error) {
        tracing::warn!(
            context,
            plan_no,
            error = %error,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress(LABEL, Some(plan_no));
    }
    write_error(error, context)
}

// ==================== 入参 → 单头 + 明细（建单 / 改单一套） ====================

/// 把请求体校验成「可直接落库的单头 + 明细」。
///
/// 校验顺序（越早拒绝越好）：名称 → 类型 → 年度 → 月份自洽 → 备注 → 明细行数 →
/// 部门存在性 → 父单存在性/语义 → 逐行（物料 / 仓库 / 数量 / 单价 / 单位）→
/// 「物料 × 仓库」唯一 → 同「类型+年度+月+部门」唯一（`self_id` 用于改单排除自己）。
async fn build_head_and_items(
    req: &CreatePlanReq,
    operator: Option<Uuid>,
    self_id: Option<Uuid>,
) -> std::result::Result<(NewPlanHead, Vec<NewPlanItem>), ApiError> {
    let plan_name = check_name(req.plan_name.as_deref())?;
    let plan_type = check_type(req.plan_type.as_deref().unwrap_or_default())?;
    let plan_year = check_year(req.plan_year.ok_or_else(|| bad_request("plan_year 不能为空"))?)?;
    let plan_month = check_month_for_type(&plan_type, req.plan_month)?;
    let remark = clean_remark(req.remark.as_deref())?;

    // 部门（可空 = 公司级计划）：给了就必须存在，并落名称快照
    let dept_id = parse_optional_uuid(req.dept_id.as_deref(), "dept_id")?;
    let dept_name = match dept_id {
        None => None,
        Some(id) => Some(
            model::dept_name_of(db(), id)
                .await
                .map_err(|error| utils::db_error(error, "查询编制部门"))?
                .ok_or_else(|| reference_missing("dept_id 部门不存在或已删除"))?,
        ),
    };

    // 父单：只有月度计划能挂，且必须是同年度年度计划（跨行语义，库里不兜）
    let parent_plan_id = parse_optional_uuid(req.parent_plan_id.as_deref(), "parent_plan_id")?;
    if let Some(parent_id) = parent_plan_id {
        if Some(parent_id) == self_id {
            return Err(bad_request("parent_plan_id 不能指向自己"));
        }
        let facts = model::parent_facts(db(), parent_id)
            .await
            .map_err(|error| utils::db_error(error, "查询父年度计划"))?
            .ok_or_else(|| reference_missing("parent_plan_id 计划不存在或已删除"))?;
        check_parent_period(
            &plan_type,
            plan_year,
            plan_month,
            Some((facts.0.as_str(), facts.1, facts.2)),
        )?;
    }

    // 编制人：缺省 = 当前登录人；姓名一律服务端快照（前端传的名字不可信）
    let planner_id = match parse_optional_uuid(req.planner_id.as_deref(), "planner_id")? {
        Some(id) => Some(id),
        None => operator,
    };
    let planner_name = match planner_id {
        Some(id) => user_name_of(id).await?,
        None => None,
    };

    check_items(&req.items)?;
    let mut items = Vec::with_capacity(req.items.len());
    let mut cells = Vec::with_capacity(req.items.len());
    for (index, line) in req.items.iter().enumerate() {
        let material_id = parse_uuid_field(line.material_id.as_deref(), &format!("items[{index}].material_id"))?;
        let warehouse_id = parse_uuid_field(line.warehouse_id.as_deref(), &format!("items[{index}].warehouse_id"))?;
        let planned_qty = check_qty(line.planned_qty, &format!("items[{index}].planned_qty"))?;
        let planned_price = check_price(line.planned_price, &format!("items[{index}].planned_price"))?;
        let material = purchase_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].material_id 物料不存在或已删除（{material_id}）"
                ))
            })?;
        // DC-09：单位一律由物料基础单位带出（折算单位会让「5 箱」记成「5 个」）
        unit_guard::ensure_base_unit(material_id, index, planned_qty).await?;
        purchase_model::warehouse_ref(warehouse_id)
            .await
            .map_err(|error| utils::db_error(error, "查询仓库"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].warehouse_id 仓库不存在或已删除（{warehouse_id}）"
                ))
            })?;
        cells.push(dto::cell_key(material_id, warehouse_id));
        items.push(NewPlanItem {
            material_id,
            warehouse_id,
            unit_id: material.unit_id,
            planned_qty,
            planned_price,
            remark: clean_remark(line.remark.as_deref())?,
        });
    }
    // 「物料 × 仓库」唯一：在请求层先拒，报文点出第几行撞第几行（库层兜底是 500）
    check_cells_unique(&cells)?;

    // 同「类型 + 年度 + 月 + 编制部门」只能一条存活计划（`uk_biz_purchase_plan_period`）
    let existing = model::find_plan_by_period(db(), &plan_type, plan_year, plan_month, dept_id)
        .await
        .map_err(|error| utils::db_error(error, "查询同期计划"))?;
    if let Some(existing) = existing
        && Some(existing.id) != self_id
    {
        return Err(conflict(
            error_code::RESOURCE_CONFLICT,
            format!(
                "同一编制范围已存在{}（{}）：{} 年{}计划已由「{}」占用，\
                 请改计划类型 / 年度 / 月份 / 编制部门，或直接编辑那张单",
                dto::plan_type_label(&plan_type),
                existing.plan_no,
                plan_year,
                plan_month.map(|month| format!(" {month} 月")).unwrap_or_default(),
                existing.dept_name.clone().unwrap_or_else(|| "公司级".to_string())
            ),
        ));
    }

    Ok((
        NewPlanHead {
            plan_name,
            plan_type,
            plan_year,
            plan_month,
            parent_plan_id,
            dept_id,
            dept_name,
            planner_id,
            planner_name,
            remark,
        },
        items,
    ))
}

/// 用户显示名（`real_name` 优先，其次 `username`）。
async fn user_name_of(id: Uuid) -> std::result::Result<Option<String>, ApiError> {
    let rows = db()
        .find_with_params(
            "SELECT COALESCE(NULLIF(real_name, ''), username) AS name FROM sys_user \
              WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询编制人"))?;
    rows.first()
        .map(|row| row.get_typed::<String>("name"))
        .transpose()
        .map_err(|error| utils::db_error(error, "读取编制人姓名"))
}

// ==================== ① 建单 ====================

/// 建单核心逻辑（handler 与单测共用，只跳过认证）。
pub async fn create_plan(req: &CreatePlanReq, operator: Option<Uuid>) -> std::result::Result<Uuid, ApiError> {
    let (head, items) = build_head_and_items(req, operator, None).await?;
    model::create_plan(&head, &items, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "创建采购计划"))
}

/// 新建采购计划。
#[nik::permission(
    path = "/business/erp/purchase-plan",
    method = "POST",
    permission = "business:erp:purchase-plan:create"
)]
#[nik::post("/business/erp/purchase-plan")]
pub async fn purchase_plan_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreatePlanReq>,
) -> ResResult<PlanDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = create_plan(&req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ② 改单 ====================

/// 改单核心逻辑：整单替换，只有 `DRAFT` 能改。
pub async fn update_plan(id: Uuid, req: &CreatePlanReq, operator: Option<Uuid>) -> std::result::Result<(), ApiError> {
    let row = model::find_plan(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划"))?
        .ok_or_else(|| not_found(id))?;
    ensure_draft(&row, "修改")?;

    let (head, items) = build_head_and_items(req, operator, Some(id)).await?;
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    let outcome = async { model::replace_plan(&tx, id, &head, &items, operator, now).await }.await;
    let affected = match outcome {
        Ok(affected) => affected,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "修改采购计划"));
        }
    };
    if affected == 0 {
        let _ = tx.rollback().await;
        return Err(status_drift(&row, "修改"));
    }
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交改单事务"))?;
    Ok(())
}

/// 修改采购计划（整单替换）。
#[nik::permission(
    path = "/business/erp/purchase-plan/{id}",
    method = "PUT",
    permission = "business:erp:purchase-plan:update"
)]
#[nik::put("/business/erp/purchase-plan/{id}")]
pub async fn purchase_plan_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreatePlanReq>,
) -> ResResult<PlanDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let operator = utils::current_user_uuid(&claims);
    update_plan(id, &req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③ 删单（软删） ====================

/// 删单核心逻辑：只有 `DRAFT` 能删（审批中的单删了会让审批详情指向空气）。
pub async fn delete_plan(id: Uuid, operator: Option<Uuid>) -> std::result::Result<(), ApiError> {
    let row = model::find_plan(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划"))?
        .ok_or_else(|| not_found(id))?;
    ensure_draft(&row, "删除")?;
    let affected = model::soft_delete_plan(db(), id, operator, Utc::now())
        .await
        .map_err(|error| write_error(error, "删除采购计划"))?;
    if affected == 0 {
        return Err(status_drift(&row, "删除"));
    }
    Ok(())
}

/// 删除采购计划。
#[nik::permission(
    path = "/business/erp/purchase-plan/{id}",
    method = "DELETE",
    permission = "business:erp:purchase-plan:delete"
)]
#[nik::delete("/business/erp/purchase-plan/{id}")]
pub async fn purchase_plan_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let operator = utils::current_user_uuid(&claims);
    delete_plan(id, operator).await?;
    Ok(Res::ok(()))
}

// ==================== ④ 分页 / 详情 ====================

/// 分页查询。
#[nik::permission(
    path = "/business/erp/purchase-plan/page",
    method = "GET",
    permission = "business:erp:purchase-plan:list"
)]
#[nik::get("/business/erp/purchase-plan/page")]
pub async fn purchase_plan_page(Query(query): Query<PlanPageQuery>) -> ResResult<PageData<Vec<PlanRowRes>>> {
    let filter: PlanFilter = query.filter()?;
    let (rows, total) = model::page_plans(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划"))?;
    Ok(Res::ok(PageData {
        list: rows.iter().map(to_row_res).collect(),
        total,
        page: filter.page,
        page_size: filter.page_size,
    }))
}

/// 详情。
#[nik::permission(
    path = "/business/erp/purchase-plan/{id}",
    method = "GET",
    permission = "business:erp:purchase-plan:list"
)]
#[nik::get("/business/erp/purchase-plan/{id}")]
pub async fn purchase_plan_detail(Path(id): Path<String>) -> ResResult<PlanDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 组装详情。
pub async fn detail_of(id: Uuid) -> std::result::Result<PlanDetailRes, ApiError> {
    let row = model::find_plan(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划"))?
        .ok_or_else(|| not_found(id))?;
    let items = model::items_of_plan(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划明细"))?;
    Ok(PlanDetailRes {
        plan: to_row_res(&row),
        items: items.iter().map(to_item_res).collect(),
    })
}

// ==================== ⑤ 提交审批 ====================

/// 提交审批核心逻辑。
///
/// # 顺序（每一步都有理由）
///
/// 1. 只允许 `DRAFT`（`SUBMITTED` 单独给「已在审批中 + 审批单号」的 409）；
/// 2. 明细非空、计划量合计 > 0（0 量单审批没有意义，也让「执行率」失去分母）；
/// 3. 有已发布流程 → 同一事务里「发起审批 + CAS 状态 + 实例 id」；
///    没流程 → 默认 409，持跳过权限才直接 `APPROVED`（理由写备注）。
pub async fn submit_plan(
    id: Uuid,
    req: &SubmitPlanReq,
    operator: Option<Uuid>,
    gate: &ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let row = model::find_plan(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划"))?
        .ok_or_else(|| not_found(id))?;
    if row.status == model::STATUS_SUBMITTED {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(
            error_code::ERP_REQUISITION_STATUS_GUARD,
            format!(
                "采购计划 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
                row.plan_no, instance_no
            ),
        ));
    }
    ensure_draft(&row, "提交审批")?;
    let items = model::items_of_plan(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "采购计划 {} 没有任何明细，不能提交审批",
            row.plan_no
        )));
    }
    if row.total_planned_qty <= Decimal::ZERO {
        return Err(bad_request(format!(
            "采购计划 {} 的计划量合计为 {}，不能提交审批（计划量必须 > 0）",
            row.plan_no, row.total_planned_qty
        )));
    }
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    let business_reason = clean_remark(req.reason.as_deref())?;

    match purchase_plan_bridge::published_pp_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划审批流程"))?
    {
        Some(flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(purchase_plan_bridge::FLOW_CODE, gate)?;
            submit_with_approval(&row, &items, &flow, operator_id, business_reason).await
        }
        None => {
            // ARCH-3 DC-01：**不静默降级**。默认 409（点名 pp_approve）；
            // 只有「持 business:erp:skip-approval + 理由 ≥10 字」才放行，理由写进备注。
            let skip_reason = approval_gate::authorize_missing_flow(
                purchase_plan_bridge::BIZ_TYPE,
                purchase_plan_bridge::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                plan_id = %row.id,
                plan_no = %row.plan_no,
                %skip_reason,
                "采购计划提交：没有已发布的 pp_approve 流程，按「显式跳过审批」直接放行到 APPROVED"
            );
            let reason = approval_gate::merge_reason(business_reason.as_deref(), &skip_reason);
            let affected = model::mark_skipped_approval(db(), row.id, Some(&reason), Some(operator_id), Utc::now())
                .await
                .map_err(|error| write_error(error, "采购计划进入已批准（跳过审批）"))?;
            if affected == 0 {
                return Err(status_drift(&row, "提交审批"));
            }
            Ok(())
        }
    }
}

/// 走审批的提交：状态推进 + 发起实例**同一事务**。
async fn submit_with_approval(
    row: &PlanRow,
    items: &[PlanItemRow],
    flow: &purchase_plan_bridge::PpFlow,
    operator_id: Uuid,
    business_reason: Option<String>,
) -> std::result::Result<(), ApiError> {
    // 幂等：同一单已有在跑的审批 → 409（唯一索引之前先给可读报文）
    if flow_model::biz_instance_running(purchase_plan_bridge::BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(
            error_code::ERP_REQUISITION_STATUS_GUARD,
            format!(
                "采购计划 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
                row.plan_no, instance_no
            ),
        ));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 采购计划审批", row.plan_no)
        .chars()
        .take(200)
        .collect::<String>();
    let start = engine::StartReq {
        instance_id,
        def_id: flow.def_id,
        version_id: flow.version_id,
        version_no: flow.version_no,
        flow_code: flow.flow_code.clone(),
        flow_name: flow.flow_name.clone(),
        biz_type: flow.biz_type.clone(),
        biz_id: Some(row.id),
        biz_no: Some(row.plan_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: form_data(row, items),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启采购计划审批事务"))?;
    let outcome = async {
        let mut out = engine::start(&tx, &start)
            .await
            .map_err(|error| submit_conflict(error, "发起采购计划审批单", &row.plan_no))?;
        if let Some(rejected) = out.rejected.take() {
            let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
                "（内控要求：审批人不能是发起人本人。请由他人提交，或让另一个账号具备审批权限后再审）"
            } else {
                ""
            };
            return Err(rejected.appended(hint));
        }
        let instance = out
            .instance_id
            .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
        let affected = model::mark_submitted(
            &tx,
            row.id,
            instance,
            business_reason.as_deref(),
            Some(operator_id),
            now,
        )
        .await
        .map_err(|error| write_error(error, "采购计划进入已提交"))?;
        if affected == 0 {
            return Err(status_drift(row, "提交审批"));
        }
        Ok::<engine::ActionOut, ApiError>(out)
    }
    .await;
    let mut out = match outcome {
        Ok(out) => out,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let instance = out.instance_id.unwrap_or_default();
    let auto_finished = out.instance_status.eq_ignore_ascii_case("APPROVED");
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交采购计划审批事务"))?;
    // 锁外副作用（审批通知 / 客户自配的通知节点）必须在提交之后发，失败只 WARN
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
    tracing::info!(
        plan_id = %row.id,
        plan_no = %row.plan_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "采购计划已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有任何审批节点（全被跳过）→ 实例在同一个事务里就走完了。
    // 这里必须立刻回调到 APPROVED，否则单据停在 SUBMITTED 且**没有任何待办**能推进它
    // （AGENTS 约定 11 点名的高危形态）；回调失败就显式打回草稿，让用户能重提。
    if auto_finished
        && purchase_plan_bridge::on_pp_settled(Some(instance), "APPROVED")
            .await
            .is_none()
    {
        let compensation = model::mark_back_to_draft(
            db(),
            row.id,
            Some("免审过账回调失败，系统已自动打回草稿，请重新提交（无需改库）"),
            Some(operator_id),
            Utc::now(),
        )
        .await;
        match compensation {
            Ok(affected) if affected > 0 => {
                tracing::warn!(
                    plan_id = %row.id,
                    plan_no = %row.plan_no,
                    instance_id = %instance,
                    "免审直过回调失败：单据已打回 DRAFT（用户可重新提交）"
                );
                return Err(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!(
                        "采购计划 {} 的审批实例已结束（无审批节点），但结论回写失败；\
                         系统已自动把单据打回草稿，请重新提交（无需改库）",
                        row.plan_no
                    ),
                ));
            }
            other => {
                tracing::error!(
                    plan_id = %row.id,
                    plan_no = %row.plan_no,
                    instance_id = %instance,
                    result = ?other,
                    "免审直过回调失败且打回草稿也失败：单据停在 SUBMITTED（无待办），需人工介入"
                );
                return Err(ApiError::new(
                    500,
                    error_code::INTERNAL_ERROR,
                    format!(
                        "采购计划 {} 的审批实例已结束，但结论回写与自动打回都失败（审批单号可查）；\
                         请联系管理员排查，单据当前停在「审批中」",
                        row.plan_no
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 审批表单变量（条件边的作用域）。
///
/// ⚠️ 金额必须写成 JSON **数字**（不是字符串）：条件边比较走 `as_f64()`，
/// `"500000.0000"` 会 TypeMismatch → 引擎按「条件不成立」静默走兜底边
///（也就是大额计划悄悄跳过财务经理，这是本卡最危险的一类静默失败）。
fn form_data(row: &PlanRow, items: &[PlanItemRow]) -> serde_json::Value {
    let lines: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "line_no": item.line_no,
                "material_id": item.material_id.to_string(),
                "material_code": item.material_code,
                "material_name": item.material_name,
                "warehouse_name": item.warehouse_name,
                "unit_name": item.unit_name,
                "planned_qty": item.planned_qty.to_string(),
                "planned_price": item.planned_price.to_string(),
                "planned_amount": item.planned_amount.to_string(),
            })
        })
        .collect();
    serde_json::json!({
        "plan_no": row.plan_no,
        "plan_name": row.plan_name,
        "plan_type": row.plan_type,
        "plan_year": row.plan_year,
        "plan_month": row.plan_month,
        "dept_name": row.dept_name.clone().unwrap_or_default(),
        "planner_name": row.planner_name.clone().unwrap_or_default(),
        "item_count": row.item_count,
        // 条件边读这两个键（**数字**，不是字符串）
        "total_planned_qty": json_number(&row.total_planned_qty.round_dp(4).to_string()),
        "total_planned_amount": json_number(&row.total_planned_amount.round_dp(4).to_string()),
        "items": lines,
    })
}

/// 十进制字符串 → JSON **数字**（条件边比较走 `as_f64()`，字符串会 TypeMismatch）。
fn json_number(decimal_text: &str) -> serde_json::Value {
    decimal_text
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

/// 在跑实例的单号（拼 409 文案用；查不到就给 `-`）。
async fn pending_instance_no_of(instance_id: Option<Uuid>) -> std::result::Result<String, ApiError> {
    let Some(instance_id) = instance_id else {
        return Ok("-".to_string());
    };
    let no = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询审批单"))?
        .map(|instance| instance.instance_no)
        .unwrap_or_else(|| "-".to_string());
    Ok(no)
}

/// 提交采购计划审批。
#[nik::permission(
    path = "/business/erp/purchase-plan/{id}/submit",
    method = "POST",
    permission = "business:erp:purchase-plan:submit"
)]
#[nik::post("/business/erp/purchase-plan/{id}/submit")]
pub async fn purchase_plan_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<SubmitPlanReq>>,
) -> ResResult<PlanDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let gate = ApprovalGate::new(&req.skip, &claims);
    let operator = utils::current_user_uuid(&claims);
    submit_plan(id, &req, operator, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== 状态守卫（三处共用，报文一致） ====================

/// 只有草稿能做（改 / 删 / 提交）。
fn ensure_draft(row: &PlanRow, action: &str) -> std::result::Result<(), ApiError> {
    if row.status == model::STATUS_DRAFT {
        return Ok(());
    }
    Err(conflict(
        error_code::ERP_REQUISITION_STATUS_GUARD,
        format!(
            "采购计划 {} 当前是「{}」（{}），只有草稿（DRAFT）可以{action}；\
             已批准的计划要调整请新建一张（审批过的计划不接受改写）",
            row.plan_no,
            status_label(&row.status),
            row.status
        ),
    ))
}

/// 读到的状态和刚写完的 CAS 撞车时的 409（并发提交 / 并发删除）。
fn status_drift(row: &PlanRow, action: &str) -> ApiError {
    conflict(
        error_code::ERP_REQUISITION_STATUS_GUARD,
        format!(
            "采购计划 {} 的状态在本次{action}期间已被改动（期望 DRAFT），请刷新后重试",
            row.plan_no
        ),
    )
}

// ==================== 行 → DTO ====================

/// 单头行 → DTO（详情 / 列表 / 转换结果共用一份，避免三条赋值路径）。
pub(crate) fn to_row_res(row: &PlanRow) -> PlanRowRes {
    PlanRowRes {
        id: row.id.to_string(),
        plan_no: row.plan_no.clone(),
        plan_name: row.plan_name.clone(),
        plan_type: row.plan_type.clone(),
        plan_type_label: dto::plan_type_label(&row.plan_type).to_string(),
        plan_year: row.plan_year,
        plan_month: row.plan_month,
        parent_plan_id: row.parent_plan_id.map(|id| id.to_string()),
        parent_plan_no: row.parent_plan_no.clone(),
        dept_id: row.dept_id.map(|id| id.to_string()),
        dept_name: row.dept_name.clone(),
        planner_id: row.planner_id.map(|id| id.to_string()),
        planner_name: row.planner_name.clone(),
        status: row.status.clone(),
        status_label: status_label(&row.status).to_string(),
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
        total_planned_qty: row.total_planned_qty,
        total_executed_qty: row.total_executed_qty,
        total_planned_amount: row.total_planned_amount,
        remark: row.remark.clone(),
        item_count: row.item_count,
        created_at: row.created_at.to_rfc3339(),
        create_by: row.create_by.map(|id| id.to_string()),
        editable: row.status == model::STATUS_DRAFT,
    }
}

fn to_item_res(item: &PlanItemRow) -> PlanItemRes {
    PlanItemRes {
        id: item.id.to_string(),
        line_no: item.line_no,
        material_id: item.material_id.to_string(),
        material_code: item.material_code.clone(),
        material_name: item.material_name.clone(),
        warehouse_id: item.warehouse_id.to_string(),
        warehouse_name: item.warehouse_name.clone(),
        unit_id: item.unit_id.to_string(),
        unit_name: item.unit_name.clone(),
        planned_qty: item.planned_qty,
        executed_qty: item.executed_qty,
        planned_price: item.planned_price,
        planned_amount: item.planned_amount,
        remark: item.remark.clone(),
    }
}

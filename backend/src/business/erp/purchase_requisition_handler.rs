//! 采购申请接口层（P2-02）：建申请 / 分页 / 详情 / 提交审批 / **合并**转采购订单。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/purchase-requisition/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/purchase-requisition` | `business:erp:purchase-requisition:create` | 建申请（`PRQ-yyyyMMdd-000001`，`DRAFT`） |
//! | GET  | `/business/erp/purchase-requisition/page` | `business:erp:purchase-requisition:list` | 分页（`convertible_only=true` 只给可转单的） |
//! | GET  | `/business/erp/purchase-requisition/{id}` | `business:erp:purchase-requisition:list` | 详情（单头 + 明细） |
//! | POST | `/business/erp/purchase-requisition/{id}/submit` | `business:erp:purchase-requisition:submit` | 提交审批（走 `pr_approve` 流程；超预算加签） |
//! | POST | `/business/erp/purchase-requisition/{id}/to-po` | `business:erp:purchase-requisition:to-po` | 转采购订单（`requisition_ids` 可带多张 → **合并**成一张 PO） |
//!
//! # 与卡面路径的差异
//!
//! 卡面写 `POST /erp/purchase-requisition`，真实前缀是 `/api/business/erp/purchase-requisition`
//! （`[router].prefix = /api` + 业务容器 `business`，与采购订单 / 询比价一致）。
//! 前端 API 层必须写全 `/api/...`，否则命中 SPA fallback 返回 HTML。
//!
//! # 三条硬规则（每条都有落库证据）
//!
//! ```text
//! ① 走审批流，不硬编码：提交时查「已发布的 pr_approve 流程」——
//!    有 → 发起审批单（预算超限时流程的条件边自动加一道财务经理特殊审批）；
//!    没有 → **默认 409**，只有持 business:erp:skip-approval + ≥10 字理由才放行
//!    （放行 = 直接 APPROVED，理由写进备注）。代码里没有任何「金额 > X 就加签」的 if。
//! ② 审批通过才能转单：to-po 的每张单都必须是 APPROVED 且没转过（CAS + 行锁）。
//! ③ 合并采购：同物料 qty 相加、单价按数量加权平均、税率必须一致、
//!    需求日期取最早、供应商按「建议一致 or 请求指定」定 —— 一张 PO 承接多张申请单，
//!    两张申请单的 status 都变 CONVERTED 且 po_id 指向同一张单。
//! ```
//!
//! # 幂等
//!
//! * 提交：状态机 CAS（`DRAFT → SUBMITTED`）+ `biz_instance_running` 双保险，
//!   重复提交 409 且带审批单号；
//! * 转单：`SELECT … FOR UPDATE` 按 id 升序锁多张单头 + 逐张 CAS（`APPROVED → CONVERTED`
//!   且 `po_id IS NULL`）⇒ 并发 / 重复转单只有一次成功。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_purchase_requisition / _item 是 ERP 单据表，可见性由
//!         business:erp:purchase-requisition:* 权限码控制；分页走
//!         model::page_requisitions（内部 scope::paginate_unscoped，闸①口径）。
//! ```

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::{self, ApprovalGate};
use crate::business::erp::purchase_dto::{CreatePurchaseOrderReq, PurchaseOrderItemReq};
use crate::business::erp::purchase_handler;
use crate::business::erp::purchase_model::{self, PRICE_MODE_EXCL};
use crate::business::erp::purchase_requisition_dto::{
    CreateRequisitionReq, MAX_LINES, MAX_MERGE, MergeError, MergeInput, MergedLine, MergedLineRes,
    REQUISITION_PERMISSIONS, RequisitionDetailRes, RequisitionItemRes, RequisitionPageQuery, RequisitionRowRes,
    STATUS_APPROVED, STATUS_CONVERTED, STATUS_DRAFT, STATUS_SUBMITTED, SubmitRequisitionReq, ToPoReq, ToPoRes,
    WarehouseError, check_price, check_qty, check_tax, clean_remark, line_amount, merge_by_material,
    parse_optional_uuid, parse_uuid_field, price_incl_of, resolve_merge_supplier, resolve_merge_warehouse,
    status_label, today_cst,
};
use crate::business::erp::purchase_requisition_model::{
    self as model, ItemRow, NewItem, NewRequisition, RequisitionRow,
};
use crate::business::erp::requisition_bridge;
use crate::business::erp::unit_guard;
use crate::business::flow::engine;
use crate::business::flow::model as flow_model;
use crate::shared::error_code;
use crate::utils;

// ==================== 权限码（与 V0331 的 sys_menu.auth_code 逐字一致） ====================

/// 列表 + 详情。
pub const PERM_LIST: &str = "business:erp:purchase-requisition:list";
/// 新建申请。
pub const PERM_CREATE: &str = "business:erp:purchase-requisition:create";
/// 提交审批。
pub const PERM_SUBMIT: &str = "business:erp:purchase-requisition:submit";
/// 转采购订单。
pub const PERM_TO_PO: &str = "business:erp:purchase-requisition:to-po";

// 代码里断言一份常量与 DTO 的清单一致：`perm_menu_gate` 只认字面量，
// 但两份清单漂移时这里会红，不会等到运行时 403。
const _: () = assert!(REQUISITION_PERMISSIONS.len() == 4);

/// 单据中文名（409 / 404 报文用）。
const LABEL: &str = "采购申请单";

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
        format!("采购申请单不存在或已删除（{id}）"),
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
fn submit_conflict(error: DbError, context: &str, req_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&error) {
        tracing::warn!(
            context,
            req_no,
            error = %error,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress(LABEL, Some(req_no));
    }
    write_error(error, context)
}

// ==================== ① 建申请 ====================

/// 建单核心逻辑（handler 与单测共用，只跳过认证）。
pub async fn create_requisition(
    req: &CreateRequisitionReq,
    operator: Option<Uuid>,
) -> std::result::Result<Uuid, ApiError> {
    let dept_id = parse_uuid_field(req.dept_id.as_deref(), "dept_id")?;
    let dept_name = model::dept_name_of(db(), dept_id)
        .await
        .map_err(|error| utils::db_error(error, "查询申请部门"))?
        .ok_or_else(|| reference_missing("dept_id 部门不存在或已删除"))?;

    let warehouse_id = parse_uuid_field(req.warehouse_id.as_deref(), "warehouse_id")?;
    purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    if req.items.is_empty() {
        return Err(bad_request("items 不能为空：至少填 1 个物料"));
    }
    if req.items.len() > MAX_LINES {
        return Err(bad_request(format!(
            "items 最多 {MAX_LINES} 行（收到 {}）",
            req.items.len()
        )));
    }

    // 申请人：缺省 = 当前登录人；姓名一律服务端快照（前端传的名字不可信）
    let applicant_id = match parse_optional_uuid(req.applicant_id.as_deref(), "applicant_id")? {
        Some(id) => Some(id),
        None => operator,
    };
    let applicant_name = match applicant_id {
        Some(id) => user_name_of(id).await?,
        None => None,
    };

    let mut items = Vec::with_capacity(req.items.len());
    let mut amount_lines: Vec<(Decimal, Decimal)> = Vec::with_capacity(req.items.len());
    for (index, line) in req.items.iter().enumerate() {
        let material_id = parse_uuid_field(line.material_id.as_deref(), &format!("items[{index}].material_id"))?;
        let qty = check_qty(line.qty, &format!("items[{index}].qty"))?;
        let est_price = check_price(line.est_price, &format!("items[{index}].est_price"))?;
        let tax_rate = check_tax(line.tax_rate, &format!("items[{index}].tax_rate"))?;
        let material = purchase_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].material_id 物料不存在或已删除（{material_id}）"
                ))
            })?;
        // DC-09：单位一律由物料基础单位带出（折算单位会让「5 箱」记成「5 个」）
        unit_guard::ensure_base_unit(material_id, index, qty).await?;
        let suggest_supplier_id = parse_optional_uuid(
            line.suggest_supplier_id.as_deref(),
            &format!("items[{index}].suggest_supplier_id"),
        )?;
        if let Some(supplier_id) = suggest_supplier_id {
            purchase_model::supplier_ref(supplier_id)
                .await
                .map_err(|error| utils::db_error(error, "查询建议供应商"))?
                .ok_or_else(|| {
                    reference_missing(format!(
                        "items[{index}].suggest_supplier_id 供应商不存在或已删除（{supplier_id}）"
                    ))
                })?;
        }
        amount_lines.push((qty, est_price));
        items.push(NewItem {
            material_id,
            unit_id: material.unit_id,
            qty,
            est_price,
            tax_rate,
            demand_date: line.demand_date,
            suggest_supplier_id,
            remark: clean_remark(line.remark.as_deref())?,
        });
    }

    let head = NewRequisition {
        req_date: req.req_date.unwrap_or_else(today_cst),
        dept_id,
        dept_name: Some(dept_name),
        applicant_id,
        applicant_name,
        demand_date: req.demand_date,
        warehouse_id,
        est_amount: crate::business::erp::purchase_requisition_dto::est_amount_of(&amount_lines),
        remark: clean_remark(req.remark.as_deref())?,
    };
    model::create_requisition(&head, &items, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "创建采购申请单"))
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
        .map_err(|error| utils::db_error(error, "查询申请人"))?;
    rows.first()
        .map(|row| row.get_typed::<String>("name"))
        .transpose()
        .map_err(|error| utils::db_error(error, "读取申请人姓名"))
}

/// 新建采购申请。
#[nik::permission(
    path = "/business/erp/purchase-requisition",
    method = "POST",
    permission = "business:erp:purchase-requisition:create"
)]
#[nik::post("/business/erp/purchase-requisition")]
pub async fn purchase_requisition_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateRequisitionReq>,
) -> ResResult<RequisitionDetailRes> {
    let operator = utils::current_user_uuid(&claims);
    let id = create_requisition(&req, operator).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ② 分页 / 详情 ====================

/// 分页查询。
#[nik::permission(
    path = "/business/erp/purchase-requisition/page",
    method = "GET",
    permission = "business:erp:purchase-requisition:list"
)]
#[nik::get("/business/erp/purchase-requisition/page")]
pub async fn purchase_requisition_page(
    Query(query): Query<RequisitionPageQuery>,
) -> ResResult<PageData<Vec<RequisitionRowRes>>> {
    let filter = query.filter()?;
    let (rows, total) = model::page_requisitions(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询采购申请单"))?;
    Ok(Res::ok(PageData {
        list: rows.iter().map(to_row_res).collect(),
        total,
        page: filter.page,
        page_size: filter.page_size,
    }))
}

/// 详情。
#[nik::permission(
    path = "/business/erp/purchase-requisition/{id}",
    method = "GET",
    permission = "business:erp:purchase-requisition:list"
)]
#[nik::get("/business/erp/purchase-requisition/{id}")]
pub async fn purchase_requisition_detail(Path(id): Path<String>) -> ResResult<RequisitionDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 组装详情。
pub async fn detail_of(id: Uuid) -> std::result::Result<RequisitionDetailRes, ApiError> {
    let row = model::find_requisition(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购申请单"))?
        .ok_or_else(|| not_found(id))?;
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购申请明细"))?;
    Ok(RequisitionDetailRes {
        requisition: to_row_res(&row),
        items: items.iter().map(to_item_res).collect(),
    })
}

// ==================== ③ 提交审批 ====================

/// 提交审批核心逻辑。
///
/// # 顺序（每一步都有理由）
///
/// 1. 只允许 `DRAFT`（`SUBMITTED` 单独给「已在审批中 + 审批单号」的 409）；
/// 2. 明细非空、合计 > 0（0 元单没有提交的意义，也会让预算闸失效）；
/// 3. **预算判定**（只读）：`BLOCK` → 409；`WARN` 超限 → 记快照，由流程条件边加签；
/// 4. 有已发布流程 → 同一事务里「发起审批 + CAS 状态 + 预算快照」；
///    没流程 → 默认 409，持跳过权限才直接 `APPROVED`（理由写备注）。
pub async fn submit_requisition(
    id: Uuid,
    req: &SubmitRequisitionReq,
    operator: Option<Uuid>,
    gate: &ApprovalGate<'_>,
) -> std::result::Result<(), ApiError> {
    let row = model::find_requisition(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购申请单"))?
        .ok_or_else(|| not_found(id))?;
    if row.status == STATUS_SUBMITTED {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(
            error_code::ERP_REQUISITION_STATUS_GUARD,
            format!(
                "采购申请 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
                row.req_no, instance_no
            ),
        ));
    }
    if row.status != STATUS_DRAFT {
        return Err(conflict(
            error_code::ERP_REQUISITION_STATUS_GUARD,
            format!(
                "采购申请 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交审批",
                row.req_no,
                status_label(&row.status),
                row.status
            ),
        ));
    }
    let items = model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购申请明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "采购申请 {} 没有任何明细，不能提交审批",
            row.req_no
        )));
    }
    if row.est_amount <= Decimal::ZERO {
        return Err(bad_request(format!(
            "采购申请 {} 的预估金额为 {}，不能提交审批（金额必须 > 0）",
            row.req_no, row.est_amount
        )));
    }
    let Some(operator_id) = operator else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };

    // ★ 预算：只判不占（理由见 requisition_bridge 模块文档）
    let period_code = row.req_date.format("%Y-%m").to_string();
    let budget = requisition_bridge::check_for_requisition(
        db(),
        row.dept_id,
        row.warehouse_id,
        items.first().map(|item| item.material_id),
        &period_code,
        row.est_amount,
    )
    .await
    .map_err(|error| utils::db_error(error, "预算校验（采购申请提交）"))?;
    if budget.over() {
        tracing::warn!(
            req_id = %row.id,
            req_no = %row.req_no,
            mode = %budget.mode.as_str(),
            reasons = %budget.reason,
            "采购申请提交：超出部门预算（超预算将按流程条件边加一道特殊审批）"
        );
    }
    if budget.blocks() {
        return Err(ApiError::new(
            409,
            error_code::ERP_BUDGET_EXCEEDED,
            format!(
                "采购申请 {} 超出部门预算，且该槽的控制策略是 BLOCK（强管控），不能提交。{}\n\
                 三条出路：① 在「预算管理」里对「部门 × 科目 × 期间」做一次预算调整（走审批，新增版本）；\
                 ② 让管理员把该槽（或全局 sys_config `erp.budget.control_mode`）改成 WARN —— \
                 那样超预算申请会带着「超预算」标记进审批，由财务经理特殊审批复核；\
                 ③ 削减申请数量或单价后重提。",
                row.req_no, budget.reason
            ),
        ));
    }
    let snapshot = budget.snapshot();
    let business_reason = clean_remark(req.reason.as_deref())?;

    match requisition_bridge::published_pr_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询采购申请审批流程"))?
    {
        Some(flow) => {
            // 有流程就必须走流程：请求里带了 skip_approval 也要 400（不许绕过已配置的内控）
            approval_gate::ensure_not_skipped(requisition_bridge::FLOW_CODE, gate)?;
            submit_with_approval(&row, &items, &flow, &snapshot, operator_id, business_reason).await
        }
        None => {
            // ARCH-3 DC-01：**不再静默降级**。默认 409（点名 pr_approve）；
            // 只有「持 business:erp:skip-approval + 理由 ≥10 字」才放行，
            // 理由随状态流转写进备注（审计据此追溯为什么这次没有复核）。
            let skip_reason = approval_gate::authorize_missing_flow(
                requisition_bridge::BIZ_TYPE,
                requisition_bridge::FLOW_CODE,
                gate,
            )?;
            tracing::warn!(
                req_id = %row.id,
                req_no = %row.req_no,
                %skip_reason,
                "采购申请提交：没有已发布的 pr_approve 流程，按「显式跳过审批」直接放行到 APPROVED"
            );
            let reason = approval_gate::merge_reason(business_reason.as_deref(), &skip_reason);
            let affected =
                model::mark_skipped_approval(db(), row.id, &snapshot, &reason, Some(operator_id), Utc::now())
                    .await
                    .map_err(|error| write_error(error, "采购申请进入已审批（跳过审批）"))?;
            if affected == 0 {
                return Err(conflict(
                    error_code::ERP_REQUISITION_STATUS_GUARD,
                    format!(
                        "采购申请 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
                        row.req_no
                    ),
                ));
            }
            Ok(())
        }
    }
}

/// 走审批的提交：状态推进 + 发起实例 + 预算快照**同一事务**。
async fn submit_with_approval(
    row: &RequisitionRow,
    items: &[ItemRow],
    flow: &requisition_bridge::PrFlow,
    budget: &model::BudgetSnapshot,
    operator_id: Uuid,
    business_reason: Option<String>,
) -> std::result::Result<(), ApiError> {
    // 幂等：同一单已有在跑的审批 → 409（唯一索引之前先给可读报文）
    if flow_model::biz_instance_running(requisition_bridge::BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(
            error_code::ERP_REQUISITION_STATUS_GUARD,
            format!(
                "采购申请 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
                row.req_no, instance_no
            ),
        ));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 采购申请审批", row.req_no)
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
        biz_no: Some(row.req_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: form_data(row, items, budget),
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启采购申请审批事务"))?;
    let outcome = async {
        let mut out = engine::start(&tx, &start)
            .await
            .map_err(|error| submit_conflict(error, "发起采购申请审批单", &row.req_no))?;
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
            budget,
            instance,
            business_reason.as_deref(),
            Some(operator_id),
            now,
        )
        .await
        .map_err(|error| write_error(error, "采购申请进入已提交"))?;
        if affected == 0 {
            return Err(conflict(
                error_code::ERP_REQUISITION_STATUS_GUARD,
                format!(
                    "采购申请 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
                    row.req_no
                ),
            ));
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
        .map_err(|error| utils::db_error(error, "提交采购申请审批事务"))?;
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
        req_id = %row.id,
        req_no = %row.req_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "采购申请已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有任何审批节点（全被跳过）→ 实例在同一个事务里就走完了，
    // 提交后立刻执行「审批通过」的回调，否则单据会停在 SUBMITTED 且没有待办能推进它。
    if auto_finished {
        requisition_bridge::on_pr_settled(Some(instance), "APPROVED").await;
    }
    Ok(())
}

/// 审批表单变量（条件边的作用域）。
///
/// ⚠️ 金额必须写成 JSON **数字**（不是字符串）：条件边比较走 `as_f64()`，
/// `"5000.0000"` 会 TypeMismatch → 引擎按「条件不成立」静默走兜底边
///（也就是超预算的单悄悄跳过财务经理，这是本卡最危险的一类静默失败）。
fn form_data(row: &RequisitionRow, items: &[ItemRow], budget: &model::BudgetSnapshot) -> serde_json::Value {
    let lines: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "line_no": item.line_no,
                "material_id": item.material_id.to_string(),
                "material_code": item.material_code,
                "material_name": item.material_name,
                "unit_name": item.unit_name,
                "qty": item.qty.to_string(),
                "est_price": item.est_price.to_string(),
                "amount": line_amount(item.qty, item.est_price).to_string(),
            })
        })
        .collect();
    serde_json::json!({
        "req_no": row.req_no,
        "req_date": row.req_date.to_string(),
        "dept_name": row.dept_name.clone().unwrap_or_default(),
        "applicant_name": row.applicant_name.clone().unwrap_or_default(),
        "warehouse_name": row.warehouse_name,
        "demand_date": row.demand_date.map(|date| date.to_string()).unwrap_or_default(),
        // 条件边读这两个键（**数字**，不是字符串）
        "est_amount": json_number(&row.est_amount.round_dp(4).to_string()),
        "over_budget_amount": json_number(&budget.over_amount.round_dp(4).to_string()),
        "over_budget": budget.over_budget,
        "budget_reason": budget.reason.clone().unwrap_or_default(),
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

/// 提交采购申请审批。
#[nik::permission(
    path = "/business/erp/purchase-requisition/{id}/submit",
    method = "POST",
    permission = "business:erp:purchase-requisition:submit"
)]
#[nik::post("/business/erp/purchase-requisition/{id}/submit")]
pub async fn purchase_requisition_submit(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<SubmitRequisitionReq>>,
) -> ResResult<RequisitionDetailRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let gate = ApprovalGate::new(&req.skip, &claims);
    let operator = utils::current_user_uuid(&claims);
    submit_requisition(id, &req, operator, &gate).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 转采购订单（支持合并） ====================

/// 转单核心逻辑：**多张申请单 → 一张采购订单**。
///
/// # 合并口径（纯函数在 dto 里，单测钉住）
///
/// * 同物料 qty 相加；单价按数量加权平均（6 位舍入）；税率必须一致；
/// * 需求日期取最早；供应商 = 请求指定 or「建议供应商一致」；
/// * 收货仓库 = 请求指定 or「各申请单一致」。
///
/// # 事务边界（**已知限制**，与 P2-01 询比价同款）
///
/// 采购订单由 `purchase_handler::create_purchase_order` 用自己的事务创建（不暴露外部事务入口），
/// 所以「建 PO」与「回写申请单」不是同一个事务：PO 建成功而回写失败时，会留下
/// 「PO 已存在、申请单仍 APPROVED」的中间态（重试会重复建单）。
/// 判定口径：回写用 CAS（`po_id IS NULL AND status='APPROVED'`），并发/重复提交只会成功一次；
/// 中间态由「申请单仍可转 → 人工核对 PO 号」兜底，不产生错账（申请单没有账）。
pub async fn to_purchase_order(
    path_id: Uuid,
    req: &ToPoReq,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<ToPoRes, ApiError> {
    // ① 收集 id（去重保序，路径 id 必须在内）
    let mut ids: Vec<Uuid> = Vec::with_capacity(req.requisition_ids.len() + 1);
    for raw in &req.requisition_ids {
        let id = parse_uuid_field(Some(raw), "requisition_ids[]")?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if !ids.contains(&path_id) {
        if ids.is_empty() {
            ids.push(path_id);
        } else {
            return Err(bad_request(format!(
                "requisition_ids 必须包含路径上的申请单 {path_id}（否则会悄悄少转一张单）"
            )));
        }
    }
    if ids.len() > MAX_MERGE {
        return Err(bad_request(format!(
            "一次最多合并 {MAX_MERGE} 张申请单（收到 {}）",
            ids.len()
        )));
    }

    // ② 定序加锁（id 升序，避免并发转单互相锁死）
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启转单事务"))?;
    let locked = async { model::lock_many(&tx, &ids).await }.await;
    let rows = match locked {
        Ok(rows) => rows,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "锁定采购申请单"));
        }
    };
    if rows.len() != ids.len() {
        let found: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !found.contains(id))
            .map(|id| id.to_string())
            .collect();
        let _ = tx.rollback().await;
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            format!("以下采购申请单不存在或已删除：{}", missing.join("、")),
        ));
    }
    // ③ 每张都必须是「已审批且没转过」
    for row in &rows {
        if row.po_id.is_some() || row.status == STATUS_CONVERTED {
            let _ = tx.rollback().await;
            return Err(conflict(
                error_code::ERP_REQUISITION_STATUS_GUARD,
                format!(
                    "采购申请 {} 已转采购订单（{}），不能重复转单",
                    row.req_no,
                    row.po_id.map(|id| id.to_string()).unwrap_or_else(|| "-".to_string())
                ),
            ));
        }
        if row.status != STATUS_APPROVED {
            let _ = tx.rollback().await;
            return Err(conflict(
                error_code::ERP_REQUISITION_STATUS_GUARD,
                format!(
                    "采购申请 {} 当前是「{}」（{}），必须审批通过（APPROVED）才能转采购订单",
                    row.req_no,
                    status_label(&row.status),
                    row.status
                ),
            ));
        }
    }

    // ④ 合并明细（同物料相加 / 加权单价 / 税率一致 / 需求日期取最早）
    let mut inputs: Vec<MergeInput> = Vec::new();
    for row in &rows {
        let items = model::items_of(row.id)
            .await
            .map_err(|error| utils::db_error(error, "查询采购申请明细"))?;
        for item in &items {
            inputs.push(MergeInput {
                requisition_id: row.id,
                requisition_no: row.req_no.clone(),
                item_id: item.id,
                material_id: item.material_id,
                qty: item.qty,
                est_price: item.est_price,
                tax_rate: item.tax_rate,
                demand_date: item.demand_date.or(row.demand_date),
                suggest_supplier_id: item.suggest_supplier_id,
            });
        }
    }
    let merged = match merge_by_material(&inputs) {
        Ok(lines) => lines,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(merge_error(error));
        }
    };
    let requested_supplier = parse_optional_uuid(req.supplier_id.as_deref(), "supplier_id")?;
    let supplier_id = match resolve_merge_supplier(&inputs, requested_supplier) {
        Ok(id) => id,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(merge_error(error));
        }
    };
    let supplier_name = purchase_model::supplier_ref(supplier_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;
    let warehouses: Vec<Uuid> = rows.iter().map(|row| row.warehouse_id).collect();
    let requested_warehouse = parse_optional_uuid(req.warehouse_id.as_deref(), "warehouse_id")?;
    let warehouse_id = match resolve_merge_warehouse(&warehouses, requested_warehouse) {
        Ok(id) => id,
        Err(WarehouseError::Conflict(candidates)) => {
            let _ = tx.rollback().await;
            return Err(conflict(
                error_code::ERP_REQUISITION_MERGE_MISMATCH,
                format!(
                    "合并转单的多张申请单收货仓库不一致（{}）：请在请求里显式指定 warehouse_id",
                    candidates
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join("、")
                ),
            ));
        }
        Err(WarehouseError::Empty) => {
            let _ = tx.rollback().await;
            return Err(bad_request("合并转单没有任何申请单，无法确定收货仓库"));
        }
    };
    purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    // ⑤ 建采购订单（单价 = 合并后的预估不含税单价，金额由采购模块服务端现算）
    let order_date = req.order_date.unwrap_or_else(today_cst);
    let expect_date = req
        .expect_date
        .or_else(|| merged.iter().filter_map(|line| line.demand_date).min());
    let req_nos: Vec<String> = rows.iter().map(|row| row.req_no.clone()).collect();
    let remark = match clean_remark(req.remark.as_deref())? {
        Some(text) => format!("{text}｜由采购申请 {} 合并转单", req_nos.join("、")),
        None => format!("由采购申请 {} 合并转单（同物料合并）", req_nos.join("、")),
    };
    let po_items: Vec<PurchaseOrderItemReq> = merged
        .iter()
        .map(|line| PurchaseOrderItemReq {
            material_id: Some(line.material_id.to_string()),
            qty: Some(line.qty),
            price_excl: Some(line.est_price),
            price_incl: None,
            tax_rate: Some(line.tax_rate),
            remark: Some(format!("合并自 {}", line.source_req_nos.join("、"))),
        })
        .collect();
    let po_req = CreatePurchaseOrderReq {
        supplier_id: Some(supplier_id.to_string()),
        warehouse_id: Some(warehouse_id.to_string()),
        purchase_type: None, // 缺省 SPOT（零星采购），与「部门提需求」的定位一致
        contract_id: None,
        project_id: None,
        order_date: Some(order_date),
        expect_date,
        price_mode: Some(PRICE_MODE_EXCL.to_string()),
        credit_days: None,
        remark: Some(remark),
        items: po_items,
    };
    let po_id = match purchase_handler::create_purchase_order(&po_req, operator).await {
        Ok(id) => id,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let detail = purchase_handler::detail_of(po_id).await?;
    let po_no = detail.order.order_no.clone();
    let total_with_tax = detail.order.total_with_tax;
    // 物料 → 采购单明细行 id（合并后一物料一行，取第一条命中的）
    let mut po_item_of_material: BTreeMap<Uuid, Uuid> = BTreeMap::new();
    for line in &detail.items {
        if let Ok(material_id) = Uuid::parse_str(&line.material_id)
            && let Ok(po_item_id) = Uuid::parse_str(&line.id)
        {
            po_item_of_material.entry(material_id).or_insert(po_item_id);
        }
    }

    // ⑥ 同一事务：逐张 CAS `APPROVED → CONVERTED` + 明细回写 po_item_id
    let mut mapping: BTreeMap<Uuid, Uuid> = BTreeMap::new();
    let mut written = 0u64;
    for row in &rows {
        let affected = model::mark_converted(&tx, row.id, po_id, &po_no, operator, now)
            .await
            .map_err(|error| write_error(error, "采购申请进入已转单"))?;
        if affected == 0 {
            let _ = tx.rollback().await;
            return Err(conflict(
                error_code::ERP_REQUISITION_STATUS_GUARD,
                format!(
                    "采购申请 {} 的状态在本次转单期间已被改动（期望 APPROVED 且未转单），请刷新后重试",
                    row.req_no
                ),
            ));
        }
        written += affected;
    }
    for input in &inputs {
        if let Some(po_item_id) = po_item_of_material.get(&input.material_id) {
            mapping.insert(input.item_id, *po_item_id);
        }
    }
    model::attach_po_items(&tx, &mapping)
        .await
        .map_err(|error| write_error(error, "回写采购订单明细对应关系"))?;
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交转单事务"))?;
    tracing::info!(
        po_id = %po_id,
        po_no = %po_no,
        requisitions = %req_nos.join(","),
        written,
        lines = merged.len(),
        "采购申请合并转采购订单完成"
    );

    Ok(ToPoRes {
        purchase_order: crate::business::erp::purchase_requisition_dto::CreatedPurchaseOrderRes {
            po_id: po_id.to_string(),
            po_no,
            supplier_id: supplier_id.to_string(),
            supplier_name,
            line_count: merged.len(),
            total_with_tax,
        },
        requisition_nos: req_nos,
        lines: {
            // 合并结果是纯函数产物（只有 material_id），响应契约里的物料编码/名称在这里回填
            let mut materials: BTreeMap<Uuid, (String, String)> = BTreeMap::new();
            for line in &merged {
                if materials.contains_key(&line.material_id) {
                    continue;
                }
                let pair = model::material_ref_of(db(), line.material_id)
                    .await
                    .map_err(|error| utils::db_error(error, "查询合并明细的物料编码"))?
                    .unwrap_or_default();
                materials.insert(line.material_id, pair);
            }
            merged
                .iter()
                .enumerate()
                .map(|(index, line)| to_merged_res(index, line, &po_item_of_material, &materials))
                .collect()
        },
        merged_count: rows.len(),
    })
}

/// 合并失败 → 409（前端按码区分「合并规则拒绝」，不是泛化的状态冲突）。
fn merge_error(error: MergeError) -> ApiError {
    match error {
        MergeError::TaxRateConflict { material_id, rates } => conflict(
            error_code::ERP_REQUISITION_MERGE_MISMATCH,
            format!(
                "物料 {material_id} 在各申请单上的税率不一致（{}）：请先把税率改成一致再合并（不同税率的明细不能并成采购单一行）",
                rates
                    .iter()
                    .map(|rate| format!("{rate}%"))
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        ),
        MergeError::SupplierAmbiguous { suppliers } => conflict(
            error_code::ERP_REQUISITION_MERGE_MISMATCH,
            format!(
                "合并的申请单指定了不同供应商（{}）：合并单只能是一个供应商，请在请求里显式指定 supplier_id",
                suppliers.iter().map(|id| id.to_string()).collect::<Vec<_>>().join("、")
            ),
        ),
        MergeError::SupplierRequired => conflict(
            error_code::ERP_REQUISITION_MERGE_MISMATCH,
            "合并的申请单都没有填建议供应商：请在请求里显式指定 supplier_id（采购单必须有供应商）",
        ),
        MergeError::Empty => bad_request("没有可合并的申请明细（申请单明细为空）"),
    }
}

fn to_merged_res(
    index: usize,
    line: &MergedLine,
    po_item_of_material: &BTreeMap<Uuid, Uuid>,
    materials: &BTreeMap<Uuid, (String, String)>,
) -> MergedLineRes {
    let (material_code, material_name) = materials.get(&line.material_id).cloned().unwrap_or_default();
    MergedLineRes {
        line_no: i32::try_from(index + 1).unwrap_or(i32::MAX),
        material_id: line.material_id.to_string(),
        material_code,
        material_name,
        qty: line.qty,
        est_price: line.est_price,
        tax_rate: line.tax_rate,
        demand_date: line.demand_date,
        source_req_nos: line.source_req_nos.clone(),
        source_item_ids: line.source_item_ids.iter().map(|id| id.to_string()).collect(),
        po_item_id: po_item_of_material.get(&line.material_id).map(|id| id.to_string()),
    }
}

/// 转采购订单（`requisition_ids` 可带多张 → 合并成一张 PO）。
#[nik::permission(
    path = "/business/erp/purchase-requisition/{id}/to-po",
    method = "POST",
    permission = "business:erp:purchase-requisition:to-po"
)]
#[nik::post("/business/erp/purchase-requisition/{id}/to-po")]
pub async fn purchase_requisition_to_po(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<ToPoReq>>,
) -> ResResult<ToPoRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(to_purchase_order(id, &req, operator, Utc::now()).await?))
}

// ==================== 出参转换 ====================

fn to_row_res(row: &RequisitionRow) -> RequisitionRowRes {
    RequisitionRowRes {
        id: row.id.to_string(),
        req_no: row.req_no.clone(),
        req_date: row.req_date,
        dept_id: row.dept_id.to_string(),
        dept_name: row.dept_name.clone().unwrap_or_default(),
        applicant_id: row.applicant_id.map(|id| id.to_string()),
        applicant_name: row.applicant_name.clone(),
        demand_date: row.demand_date,
        warehouse_id: row.warehouse_id.to_string(),
        warehouse_name: row.warehouse_name.clone(),
        status: row.status.clone(),
        status_label: status_label(&row.status).to_string(),
        est_amount: row.est_amount,
        item_count: row.item_count,
        over_budget: row.over_budget,
        over_amount: row.over_amount,
        budget_reason: row.budget_reason.clone(),
        budget_checked: row.budget_checked,
        approval_instance_id: row.approval_instance_id.map(|id| id.to_string()),
        po_id: row.po_id.map(|id| id.to_string()),
        po_no: row.po_no.clone(),
        converted_at: row.converted_at.map(|value| value.to_rfc3339()),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
    }
}

fn to_item_res(item: &ItemRow) -> RequisitionItemRes {
    RequisitionItemRes {
        id: item.id.to_string(),
        line_no: item.line_no,
        material_id: item.material_id.to_string(),
        material_code: item.material_code.clone(),
        material_name: item.material_name.clone(),
        unit_id: item.unit_id.to_string(),
        unit_name: item.unit_name.clone(),
        qty: item.qty,
        est_price: item.est_price,
        est_price_incl: price_incl_of(item.est_price, item.tax_rate),
        tax_rate: item.tax_rate,
        amount: line_amount(item.qty, item.est_price),
        demand_date: item.demand_date,
        suggest_supplier_id: item.suggest_supplier_id.map(|id| id.to_string()),
        suggest_supplier_name: item.suggest_supplier_name.clone().filter(|name| !name.is_empty()),
        po_item_id: item.po_item_id.map(|id| id.to_string()),
        remark: item.remark.clone(),
    }
}

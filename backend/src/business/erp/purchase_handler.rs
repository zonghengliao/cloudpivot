//! 采购订单接口层（批次 D · P2-A2）：建 / 改 / 状态流转 / 删（列表与详情沿用 W1c）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/purchase-order/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/purchase-order` | `business:erp:purchase-order:create` | 建单（`DRAFT` + `PO-yyyyMMdd-000001`，金额服务端算） |
//! | PUT | `/business/erp/purchase-order/{id}` | `business:erp:purchase-order:update` | 改单（**仅 `DRAFT`**，明细致删旧插新） |
//! | PATCH | `/business/erp/purchase-order/{id}/status` | `business:erp:purchase-order:status` | 状态流转（4 条合法边，其余 409） |
//! | DELETE | `/business/erp/purchase-order/{id}` | `business:erp:purchase-order:delete` | 软删（**仅 `DRAFT`**） |
//! | GET | `/business/erp/purchase-order/page` / `{id}` | `business:erp:purchase-order:list` | **沿用 W1c 的只读接口**（P2-A2 只补 `price_mode` 返回） |
//!
//! # 为什么业务逻辑拆成普通 async 函数
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；把「校验 + 取号 + 落库」
//! 收进 [`create_purchase_order`] / [`update_purchase_order`] / [`change_purchase_order_status`] /
//! [`delete_purchase_order`] 之后，单测走**同一条代码路径**（只跳过认证）。
//!
//! # 金额只由服务端算（硬红线 2）
//!
//! 请求契约里没有金额字段（见 [`crate::business::erp::purchase_dto`]），
//! 单头 `total_*` 与行 `amount / tax_amount / with_tax_amount` 全部由
//! [`crate::business::erp::purchase_model::line_amounts`] 现算并逐行舍入后相加。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 采购订单是 ERP 单据表（V0031 建表，无 dept_id / 归属人列），
//! 可见性由 business:erp:purchase-order:* 权限码控制；分页沿用 model::purchase_order_page
//! （内部走 scope::paginate_unscoped，闸①口径）。
//! ```
//!
//! # 不收货（硬红线 7）
//!
//! 本卡**不写** `biz_stock_in` / `biz_inventory` / `biz_inventory_txn`，
//! `received_qty` 建单恒 0，`PARTIAL_RECEIVED` / `RECEIVED` 两个状态也不由本卡写入（P2-B1）。

use axum::Json;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::PurchaseOrderRes;
use crate::business::erp::purchase_dto::{
    CreatePurchaseOrderReq, PendingItemRes, PendingOrderRes, PendingReceiveRes, PendingStockInRes,
    PurchaseOrderDetailRes, PurchaseOrderLineRes, PurchaseOrderStatusReq, ensure_submittable_lines,
};
use crate::business::erp::purchase_model::{
    self, NewItem, NewPurchaseOrder, STATUS_CANCELLED, STATUS_DRAFT, STATUS_SUBMITTED, Totals,
    can_transition, status_label, transition_rejected_message,
};
use crate::business::erp::stockin_model;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 共用辅助 ====================

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 操作人（`create_by` / `update_by`）。
fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

/// 不存在统一 404。
fn not_found(what: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{what}不存在或已删除"),
    )
}

/// 状态冲突统一 409。
fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 弱引用缺失 / 数据不一致统一 400（`66002`）。
fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "采购订单号重复（唯一约束{}）：取号器与库里的号段不一致，请联系运维排查 biz_seq",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足库约束 {}（三金额必须精确恒等 total_with_tax = total_amount + total_tax；\
                 数量 > 0；税率 0–100）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::ForeignKeyViolation { constraint, .. } => ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!(
                "引用的数据不存在（外键 {}）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 详情组装 ====================

/// 详情：单头（含三金额 + 口径）+ 明细（行号升序）。
pub async fn detail_of(id: Uuid) -> std::result::Result<PurchaseOrderDetailRes, ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let items = purchase_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单明细"))?;
    Ok(PurchaseOrderDetailRes {
        order: PurchaseOrderRes::from(&row),
        items: items.iter().map(PurchaseOrderLineRes::from).collect(),
    })
}

// ==================== ① 建单 ====================

/// 建单核心逻辑（handler 与单测共用）。
///
/// 顺序：DTO 校验 + 算金额（纯函数）→ 供应商 / 仓库存在 → 逐行物料存在（带出 `unit_id`）→
/// 一个事务里「取号 + 单头 + 明细」。
pub async fn create_purchase_order(
    req: &CreatePurchaseOrderReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<Uuid, ApiError> {
    let op = operator.into();
    let prepared = req.prepare()?;
    let supplier_id = parse_uuid(&prepared.supplier_id_raw, "supplier_id")?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;

    purchase_model::supplier_ref(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;
    purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    let items = prepare_items(&prepared).await?;
    let totals = Totals::of(&items);
    let new = NewPurchaseOrder {
        supplier_id,
        warehouse_id,
        order_date: prepared.order_date,
        expect_date: prepared.expect_date,
        price_mode: &prepared.price_mode,
        remark: prepared.remark.as_deref(),
        credit_days: prepared.credit_days,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    match purchase_model::create(&tx, &new, &items, &totals, op, now).await {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "创建采购订单"))
        }
    }
}

/// 明细：物料存在性 + 基础单位（`unit_id` 列 `NOT NULL`，由物料带出，不让客户端传）。
async fn prepare_items(
    prepared: &crate::business::erp::purchase_dto::PreparedOrder,
) -> std::result::Result<Vec<NewItem>, ApiError> {
    let mut items = Vec::with_capacity(prepared.lines.len());
    for (index, line) in prepared.lines.iter().enumerate() {
        let material_id = parse_uuid(
            &line.material_id_raw,
            &format!("items[{index}].material_id"),
        )?;
        let material = purchase_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].material_id 物料不存在或已删除（{material_id}）"
                ))
            })?;
        items.push(NewItem {
            material_id,
            unit_id: material.unit_id,
            qty: line.qty,
            price: line.price_excl,
            price_incl: line.price_incl,
            tax_rate: line.tax_rate,
            amounts: line.amounts,
            remark: line.remark.clone(),
        });
    }
    Ok(items)
}

/// 新建采购订单（`status = DRAFT`，单号 `PO-yyyyMMdd-000001`）。
#[nik::permission(
    path = "/business/erp/purchase-order",
    method = "POST",
    permission = "business:erp:purchase-order:create"
)]
#[nik::post("/business/erp/purchase-order")]
pub async fn purchase_order_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreatePurchaseOrderReq>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = create_purchase_order(&req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ⑥ 待收明细（D4：一键生成收货单的数据源） ====================

/// 「查采购单待收明细」的核心逻辑（handler 与单测共用）。
///
/// 口径（卡面 F2 + 硬红线 4 / 5）：
///
/// - `received_qty` **只算已过账（`POSTED`）的收货单**，草稿 / 作废 / 软删一律不算
///   （聚合表达式见 [`crate::business::erp::purchase_model::POSTED_RECEIVED_SQL`]）；
/// - 按**采购明细行**（`si.source_item_id`）匹配，不按 `material_id`；
/// - `pending_qty = ordered_qty - received_qty`（负数按 0）；
/// - `can_receive` 同时要求「状态可收」与「还有待收行」：`RECEIVED` / `CLOSED` /
///   `CANCELLED` / `DRAFT` 都给 `false` + 明确原因，前端不用自己拼文案。
pub async fn pending_items_of(id: Uuid) -> std::result::Result<PendingReceiveRes, ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let lines = purchase_model::pending_receive_lines(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购单待收明细"))?;
    let briefs = purchase_model::stock_ins_of_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购单已生成的收货单"))?;

    // 名称查不到（供应商 / 仓库被软删）一律给空串：前端是把它回填进下拉框的，
    // 给 null 会让「带出默认值」这一步静默失败。
    let supplier_name = purchase_model::supplier_ref(row.supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .unwrap_or_default();
    let warehouse_name = purchase_model::warehouse_ref(row.warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .unwrap_or_default();

    let pending_line_count = lines.iter().filter(|line| line.has_pending()).count();
    let receivable = stockin_model::purchase_receivable(&row.status);
    let can_receive = receivable && pending_line_count > 0;
    let blocked_reason = if can_receive {
        None
    } else if pending_line_count > 0 {
        // 状态不可收（DRAFT / RECEIVED / CLOSED / CANCELLED）且还有待收行：原因在状态上
        Some(format!(
            "采购单 {} 当前是「{}」（{}），不能收货：只有已提交（SUBMITTED）/ 已审批（APPROVED）/\n部分收货（PARTIAL_RECEIVED）的采购单可以收",
            row.order_no,
            status_label(&row.status),
            row.status
        ))
    } else {
        // 一行都不剩：原因在数量上（文案要带「已收满」+ 当前状态，两个问题都答）
        Some(format!(
            "采购单 {} 已收满：{} 行全部收完，没有可收的待收数量（当前状态「{}」/ {}）",
            row.order_no,
            lines.len(),
            status_label(&row.status),
            row.status
        ))
    };

    Ok(PendingReceiveRes {
        order: PendingOrderRes {
            id: row.id.to_string(),
            order_no: row.order_no.clone(),
            status: row.status.clone(),
            status_label: status_label(&row.status).to_string(),
            supplier_id: row.supplier_id.to_string(),
            supplier_name,
            warehouse_id: row.warehouse_id.to_string(),
            warehouse_name,
            total_qty: row.total_qty,
            order_date: row.order_date,
            remark: row.remark.clone(),
        },
        items: lines.iter().map(PendingItemRes::from_line).collect(),
        stock_ins: briefs.iter().map(PendingStockInRes::from_brief).collect(),
        pending_line_count,
        can_receive,
        blocked_reason,
    })
}

/// 查采购单待收明细（收货页「一键带出」的数据源）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}/pending-items",
    method = "GET",
    permission = "business:erp:purchase-order:list"
)]
#[nik::get("/business/erp/purchase-order/{id}/pending-items")]
pub async fn purchase_order_pending_items(Path(id): Path<String>) -> ResResult<PendingReceiveRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(pending_items_of(id).await?))
}

// ==================== ② 改单 ====================

/// 改单核心逻辑：**只有 `DRAFT` 能改**（其它状态 → 409），明细整体重建。
pub async fn update_purchase_order(
    id: Uuid,
    req: &CreatePurchaseOrderReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let prepared = req.prepare()?;
    let current = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    if current.status != STATUS_DRAFT {
        // P10 铁律 ④：入账态（收过货）→ 统一的「已入账」文案
        state_machine::ensure_writable(
            state_machine::ENTITY_PURCHASE_ORDER,
            &current.status,
            &current.order_no,
        )?;
        return Err(conflict(format!(
            "采购订单当前是「{}」（{}），只有草稿（DRAFT）能编辑：\
             已提交的单据请先取消，或由收货流程处理",
            status_label(&current.status),
            current.status
        )));
    }

    let supplier_id = parse_uuid(&prepared.supplier_id_raw, "supplier_id")?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;
    purchase_model::supplier_ref(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;
    purchase_model::warehouse_ref(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    let items = prepare_items(&prepared).await?;
    let totals = Totals::of(&items);
    let new = NewPurchaseOrder {
        supplier_id,
        warehouse_id,
        order_date: prepared.order_date,
        expect_date: prepared.expect_date,
        price_mode: &prepared.price_mode,
        remark: prepared.remark.as_deref(),
        credit_days: prepared.credit_days,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    match purchase_model::update(&tx, id, &new, &items, &totals, operator, now).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            // 读状态与写状态之间被别人推走（并发）：按冲突处理，不静默成功
            Err(conflict(
                "采购订单状态在本次编辑期间已被改动（只有草稿能改），请刷新后重试",
            ))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交改单事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "编辑采购订单"))
        }
    }
}

/// 编辑采购订单（仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}",
    method = "PUT",
    permission = "business:erp:purchase-order:update"
)]
#[nik::put("/business/erp/purchase-order/{id}")]
pub async fn purchase_order_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreatePurchaseOrderReq>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = parse_uuid(&id, "id")?;
    update_purchase_order(id, &req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③ 状态流转 ====================

/// 状态流转核心逻辑（handler 与单测共用）。
///
/// 顺序：读单 → 目标态白名单 → 状态机（非法边 409）→ 提交前的明细数量守卫 →
/// CAS 写状态 → 回读。
pub async fn change_purchase_order_status(
    id: Uuid,
    req: &PurchaseOrderStatusReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let target = req.target_status()?;
    let reason = req.reason_text();
    if let Some(text) = reason
        && text.chars().count() > crate::business::erp::purchase_dto::MAX_REASON_LEN
    {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "reason 超过 {} 字（当前 {} 字）",
                crate::business::erp::purchase_dto::MAX_REASON_LEN,
                text.chars().count()
            ),
        ));
    }

    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    if !can_transition(&row.status, target) {
        return Err(conflict(transition_rejected_message(&row.status, target)));
    }

    // SUBMITTED → CANCELLED 必须给出原因（卡面 §三）；DRAFT → CANCELLED 原因可选
    if target == STATUS_CANCELLED && row.status == STATUS_SUBMITTED && reason.is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "已提交的采购订单作废必须给出原因（reason）：提交后可能已有收货安排，\
             原因要能追溯到为什么不用了",
        ));
    }

    // 提交前的明细守卫（PRD §3.1.6：「提交/过账时拦截 0 数量」）
    if target == STATUS_SUBMITTED {
        let lines = purchase_model::items_of(id)
            .await
            .map_err(|error| utils::db_error(error, "查询采购订单明细"))?;
        ensure_submittable_lines(&lines)?;
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    // P10 铁律 ③：先过唯一入口（边表校验 + reason 校验 + 写 biz_status_log），再 CAS
    if let Err(error) = state_machine::transition(
        &tx,
        state_machine::ENTITY_PURCHASE_ORDER,
        id,
        &row.order_no,
        &purchase_model::LEGAL_EDGES,
        &row.status,
        target,
        op.clone(),
        reason,
        None,
    )
    .await
    {
        let _ = tx.rollback().await;
        return Err(error);
    }
    match purchase_model::apply_status(&tx, id, &row.status, target, reason, op.id, now).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            Err(conflict(format!(
                "采购订单状态在本次流转期间已被改动（期望 {}），请刷新后重试",
                row.status
            )))
        }
        Ok(_) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "采购订单状态流转"))
        }
    }
}

/// 状态流转（`DRAFT→SUBMITTED` / `DRAFT→CANCELLED` / `SUBMITTED→CLOSED` / `SUBMITTED→CANCELLED`）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}/status",
    method = "PATCH",
    permission = "business:erp:purchase-order:status"
)]
#[nik::patch("/business/erp/purchase-order/{id}/status")]
pub async fn purchase_order_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<PurchaseOrderStatusReq>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = parse_uuid(&id, "id")?;
    change_purchase_order_status(id, &req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 删除 ====================

/// 删除核心逻辑：**只有 `DRAFT`** 能删（软删）。
pub async fn delete_purchase_order(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    if row.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "采购订单当前是「{}」（{}），只有草稿（DRAFT）能删：\
             已提交 / 已关闭 / 已取消的单据是审批与收货的事实，只能作废（状态流转）",
            status_label(&row.status),
            row.status
        )));
    }
    let affected = purchase_model::soft_delete(db(), id, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "软删采购订单"))?;
    if affected == 0 {
        return Err(not_found("采购订单"));
    }
    Ok(())
}

/// 删除采购订单（软删；仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}",
    method = "DELETE",
    permission = "business:erp:purchase-order:delete"
)]
#[nik::delete("/business/erp/purchase-order/{id}")]
pub async fn purchase_order_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    delete_purchase_order(id, operator_of(&claims)).await?;
    Ok(Res::ok(()))
}

// ==================== 单测 ====================
//
// 纯逻辑（金额口径 / 状态机 / 校验）在 `purchase_dto` 与 `purchase_model` 的单测里；
// 这里只放**真库用例**（建单落库 / 改单 / 状态机 CAS / 软删）。
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0145 的隔离库」，缺省则全部 `SKIP`：
//
// ```bash
// su - postgres -c 'createdb -O cloudpivot cloudpivot_p2a2'
// export ERP_DB_URL='postgres://cloudpivot:<pw>@127.0.0.1:5432/cloudpivot_p2a2'
// cargo test --lib business::erp::purchase_handler
// ```
//
// 夹具固定 UUID（`d2a2…` 段）+ `P2A2-` 前缀，每次 `setup()` 先按供应商清掉上一轮的单
// （`biz_purchase_order_item` 是 `ON DELETE CASCADE`，明细跟着走），所以重复跑不积累。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::purchase_dto::PurchaseOrderItemReq;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    const SUPPLIER: &str = "d2a20001-0000-4000-8000-000000000001";
    const WAREHOUSE: &str = "d2a20001-0000-4000-8000-000000000002";
    const MATERIAL_A: &str = "d2a20001-0000-4000-8000-000000000011";
    const MATERIAL_B: &str = "d2a20001-0000-4000-8000-000000000012";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('d2a20001-0000-4000-8000-000000000003', 'P2A2-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
         VALUES ('d2a20001-0000-4000-8000-000000000001', 'P2A2-SUP', 'P2A2 测试供应商', 1)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
         VALUES ('d2a20001-0000-4000-8000-000000000002', 'P2A2-WH', 'P2A2 测试仓',
                 '/d2a20001-0000-4000-8000-000000000002/', 1, 1, 1)
         ON CONFLICT (id) DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status)
         VALUES ('d2a20001-0000-4000-8000-000000000011', 'P2A2-M1', '螺丝 M6x20',
                 'd2a20001-0000-4000-8000-000000000003', 1),
                ('d2a20001-0000-4000-8000-000000000012', 'P2A2-M2', '垫片 8mm',
                 'd2a20001-0000-4000-8000-000000000003', 1)
         ON CONFLICT (id) DO NOTHING",
    ];

    /// 真库用例必须串行（共用全局 `db()` 与同一供应商夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 初始化全局数据库（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 每个 `#[tokio::test]` 是独立 runtime，而 `db()` 是进程级全局池：
                // `max_lifetime = 0` 逼池子每次新建连接，避免借到上一个 runtime 的死连接（W1b 实测）。
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。`None` = 没配 `ERP_DB_URL`。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        // 先清上一轮的订单（明细随主单 CASCADE）
        let sql = format!("DELETE FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid");
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("清理上一轮采购订单失败：{err}");
        }
        for sql in FIXTURES {
            if let Err(err) = db().execute_with_params(sql, vec![]).await {
                panic!("夹具写入失败：{err}\nSQL: {sql}");
            }
        }
        Some(guard)
    }

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// 组一个建单请求：`(material, qty, price, tax_rate)`。
    ///
    /// 口径由 `mode` 决定：`EXCL` 只给 `price_excl`，`INCL` 只给 `price_incl`
    /// （与前端两种录入形态一致）。
    fn req(mode: &str, lines: &[(&str, &str, &str, &str)]) -> CreatePurchaseOrderReq {
        CreatePurchaseOrderReq {
            supplier_id: Some(SUPPLIER.to_string()),
            warehouse_id: Some(WAREHOUSE.to_string()),
            price_mode: Some(mode.to_string()),
            items: lines
                .iter()
                .map(|(material, qty, price, rate)| PurchaseOrderItemReq {
                    material_id: Some((*material).to_string()),
                    qty: Some(dec(qty)),
                    price_excl: (mode == "EXCL").then(|| dec(price)),
                    price_incl: (mode == "INCL").then(|| dec(price)),
                    tax_rate: Some(dec(rate)),
                    remark: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn status_req(status: &str, reason: Option<&str>) -> PurchaseOrderStatusReq {
        PurchaseOrderStatusReq {
            status: Some(status.to_string()),
            reason: reason.map(str::to_string),
        }
    }

    /// ① 口径 A 算例落库：100 × 2、13% → 200.00 / 26.00 / 226.00（PRD §3.1.4）。
    #[tokio::test]
    async fn excl_example_persisted_with_server_side_amounts() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req("EXCL", &[(MATERIAL_A, "100", "2", "13")]);
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.status, STATUS_DRAFT);
        assert_eq!(detail.order.price_mode, "EXCL");
        assert!(
            detail.order.order_no.starts_with("PO-"),
            "{}",
            detail.order.order_no
        );
        assert_eq!(detail.order.total_amount, dec("200.00"));
        assert_eq!(detail.order.total_tax, dec("26.00"));
        assert_eq!(detail.order.total_with_tax, dec("226.00"));
        assert_eq!(detail.items.len(), 1);
        let line = &detail.items[0];
        assert_eq!(line.amount, dec("200.00"));
        assert_eq!(line.tax_amount, dec("26.00"));
        assert_eq!(line.with_tax_amount, dec("226.00"));
        // 含税单价是派生快照：2 × 1.13 = 2.26
        assert_eq!(line.price_incl, dec("2.260000"));
        assert_eq!(line.received_qty, dec("0"));
    }

    /// ② 口径 B 算例落库：100 × 2、13% → 176.99 / 23.01 / 200.00（减法倒挤）。
    #[tokio::test]
    async fn incl_example_persisted_with_subtracted_tax() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req("INCL", &[(MATERIAL_A, "100", "2", "13")]);
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.price_mode, "INCL");
        assert_eq!(detail.order.total_with_tax, dec("200.00"));
        assert_eq!(detail.order.total_amount, dec("176.99"));
        assert_eq!(detail.order.total_tax, dec("23.01"));
        let line = &detail.items[0];
        assert_eq!(line.amount, dec("176.99"));
        assert_eq!(line.tax_amount, dec("23.01"));
        // 减法倒挤的恒等式必须精确成立（否则 ck_biz_po_item_tax 会拒写）
        assert_eq!(line.amount + line.tax_amount, line.with_tax_amount);
    }

    /// ③ 尾差：3 行各 1 × 0.005 → 逐行舍入 0.01 × 3 = 0.03（先加总再舍入会得 0.02）。
    #[tokio::test]
    async fn totals_are_rounded_per_line_not_on_sum() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req(
            "EXCL",
            &[
                (MATERIAL_A, "1", "0.005", "0"),
                (MATERIAL_A, "1", "0.005", "0"),
                (MATERIAL_A, "1", "0.005", "0"),
            ],
        );
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items.len(), 3);
        for line in &detail.items {
            assert_eq!(line.amount, dec("0.01"), "逐行必须舍入到 2 位");
        }
        assert_eq!(detail.order.total_amount, dec("0.03"));
        assert_ne!(
            detail.order.total_amount,
            dec("0.02"),
            "先加总再舍入会得 0.02 —— 那是 PRD §3.1.5 明令禁止的口径"
        );
    }

    /// ④ 改单：DRAFT 能改，明细「删旧插新」，汇总跟着重算。
    #[tokio::test]
    async fn update_draft_replaces_items_and_totals() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        let changed = req(
            "EXCL",
            &[(MATERIAL_A, "10", "2", "13"), (MATERIAL_B, "5", "3", "6")],
        );
        update_purchase_order(id, &changed, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items.len(), 2, "旧行必须被替换掉");
        assert_eq!(detail.items[0].line_no, 1);
        assert_eq!(detail.items[1].line_no, 2);
        // 20.00 + 26.00? → 10×2=20, 税 2.60；5×3=15, 税 0.90
        assert_eq!(detail.order.total_amount, dec("35.00"));
        assert_eq!(detail.order.total_tax, dec("3.50"));
        assert_eq!(detail.order.total_with_tax, dec("38.50"));
        assert_eq!(detail.order.total_qty, dec("15"));
    }

    /// ⑤ 改单：SUBMITTED 不能改 → 409（卡面 ⑬）。
    #[tokio::test]
    async fn update_submitted_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), None)
            .await
            .unwrap();
        let err = update_purchase_order(id, &req("EXCL", &[(MATERIAL_A, "1", "2", "13")]), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("只有草稿"), "{}", err.msg);
    }

    /// ⑥ 状态机：DRAFT → SUBMITTED ✅，写 `submitted_at` / `submitted_by`（卡面 ⑭）。
    #[tokio::test]
    async fn submit_moves_draft_to_submitted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let operator = Uuid::now_v7();
        let id = create_purchase_order(
            &req("EXCL", &[(MATERIAL_A, "100", "2", "13")]),
            Some(operator),
        )
        .await
        .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), Some(operator))
            .await
            .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.status, STATUS_SUBMITTED);
        assert!(detail.order.submitted_at.is_some(), "提交必须写时间戳");
        assert_eq!(
            detail.order.submitted_by.as_deref(),
            Some(operator.to_string().as_str())
        );
    }

    /// ⑦ 状态机：DRAFT → CLOSED 跳级 → 409（卡面 ⑮）。
    #[tokio::test]
    async fn draft_cannot_jump_to_closed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        let err = change_purchase_order_status(id, &status_req("CLOSED", None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("SUBMITTED"),
            "文案要点名可选去向：{}",
            err.msg
        );
    }

    /// ⑧ 作废：SUBMITTED → CANCELLED 必须给 reason（400），给了才成功并追加进 remark。
    #[tokio::test]
    async fn cancel_submitted_requires_reason_then_appends_remark() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), None)
            .await
            .unwrap();

        let err = change_purchase_order_status(id, &status_req(STATUS_CANCELLED, None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("reason"), "{}", err.msg);

        change_purchase_order_status(
            id,
            &status_req(STATUS_CANCELLED, Some("供应商断供，改走另一家")),
            None,
        )
        .await
        .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.order.status, STATUS_CANCELLED);
        assert!(
            detail.order.closed_at.is_some(),
            "作废落 closed_at（表里没有 cancelled_at）"
        );
        let remark = detail.order.remark.clone().unwrap_or_default();
        assert!(remark.contains("取消原因：供应商断供"), "{remark}");
    }

    /// ⑨ 删除：DRAFT 能软删（卡面 ⑯），删完详情 404。
    #[tokio::test]
    async fn delete_draft_soft_deletes() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        delete_purchase_order(id, None).await.unwrap();
        let err = detail_of(id).await.unwrap_err();
        assert_eq!(err.status, 404, "{}", err.msg);
        // 软删：行还在库里，只是 deleted_at 非空
        let left: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_purchase_order WHERE id = ?::uuid AND deleted_at IS NOT NULL",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(left, 1);
    }

    /// ⑩ 删除：SUBMITTED 不能删 → 409（卡面 ⑰）。
    #[tokio::test]
    async fn delete_submitted_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        change_purchase_order_status(id, &status_req(STATUS_SUBMITTED, None), None)
            .await
            .unwrap();
        let err = delete_purchase_order(id, None).await.unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("只有草稿"), "{}", err.msg);
    }

    /// ⑪ 多税率单：各行独立算，税额相加正确（13% 与 6% 混排）。
    #[tokio::test]
    async fn multi_tax_rate_lines_are_independent() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let request = req(
            "EXCL",
            &[
                (MATERIAL_A, "10", "2.5", "13"),
                (MATERIAL_B, "3", "10", "6"),
            ],
        );
        let id = create_purchase_order(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        // 行 1：25.00 + 3.25 = 28.25；行 2：30.00 + 1.80 = 31.80
        assert_eq!(detail.items[0].amount, dec("25.00"));
        assert_eq!(detail.items[0].tax_amount, dec("3.25"));
        assert_eq!(detail.items[1].amount, dec("30.00"));
        assert_eq!(detail.items[1].tax_amount, dec("1.80"));
        assert_eq!(detail.order.total_amount, dec("55.00"));
        assert_eq!(detail.order.total_tax, dec("5.05"));
        assert_eq!(detail.order.total_with_tax, dec("60.05"));
    }

    /// ⑫ 引用校验：物料不存在 → 400 + 66002（不落半张单）。
    #[tokio::test]
    async fn unknown_material_is_reference_error() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let missing = Uuid::now_v7().to_string();
        let request = req("EXCL", &[(missing.as_str(), "1", "1", "13")]);
        let err = create_purchase_order(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
    }
}

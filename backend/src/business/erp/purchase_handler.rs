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
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::dto::PurchaseOrderRes;
use crate::business::erp::form_snapshot;
use crate::business::erp::model::PurchaseOrderRow;
use crate::business::erp::purchase_bridge;
use crate::business::erp::purchase_dto::{
    CreatePurchaseOrderReq, PendingItemRes, PendingOrderRes, PendingReceiveRes, PendingStockInRes,
    PurchaseOrderDetailRes, PurchaseOrderLineRes, PurchaseOrderStatusReq, ensure_submittable_lines,
};
use crate::business::erp::purchase_model::{
    self, NewItem, NewPurchaseOrder, STATUS_CANCELLED, STATUS_DRAFT, STATUS_SUBMITTED, Totals,
    can_transition, status_label, transition_rejected_message,
};
use crate::business::erp::stockin_model;
use crate::business::flow::{engine, model as flow_model};
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

/// 单据名词（409 文案里用）。
const PO_LABEL: &str = "采购订单";

/// 提交审批撞 `uk_biz_flow_instance_biz_active` → `409 + 本单号`（不是「单号重复、去查 biz_seq」）。
///
/// 判据与文案收在 [`flow_model::is_biz_active_duplicate`] / [`flow_model::approval_in_progress`]，
/// 与调拨单等兄弟模块同一份实现。只在**拿得到单号的入口**（发起采购审批）用它；
/// 其余出口走 [`write_error`] 的兜底分支（文案退化成「该采购订单」）。
fn submit_conflict(err: DbError, ctx: &str, order_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(
            context = ctx,
            order_no,
            error = %err,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress(PO_LABEL, Some(order_no));
    }
    write_error(err, ctx)
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    // ⚠️ 顺序要紧：审批实例的唯一索引冲突**先**认出来。
    // 落到下面的 `UniqueViolation` 分支会被说成「采购订单号重复（唯一约束
    // uk_biz_flow_instance_biz_active）…联系运维排查 biz_seq」—— 用户按提示去查取号器，
    // 方向完全错（真正原因是这张单已经有一个在跑的审批单）。
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(context = ctx, error = %err, "撞审批实例唯一索引：已有在跑实例，翻 409");
        return flow_model::approval_in_progress(PO_LABEL, None);
    }
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
    let mut order = PurchaseOrderRes::from(&row);
    fill_approval_no(std::slice::from_mut(&mut order)).await;
    Ok(PurchaseOrderDetailRes {
        order,
        items: items.iter().map(PurchaseOrderLineRes::from).collect(),
    })
}

/// 回填审批单号（`approval_instance_no`）：一次查完整页的实例号，逐行按 id 对上。
///
/// 为什么要它：列表 / 详情要显示「审批中（FLW-xxx）」（O5 的可执行报文口径），
/// 而 `PurchaseOrderRes::from(&Row)` 只拿得到实例 **id**。批量查一次而不是逐行查 ——
/// 一页 20 行逐行查就是 20 次往返。
pub async fn fill_approval_no(rows: &mut [PurchaseOrderRes]) {
    let ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|row| row.approval_instance_id.as_deref())
        .filter_map(|raw| Uuid::parse_str(raw).ok())
        .collect();
    if ids.is_empty() {
        return;
    }
    let joined = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let found = db()
        .find_with_params(
            "SELECT id, instance_no FROM biz_flow_instance \
              WHERE id = ANY(?::text::uuid[])",
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await
        .unwrap_or_default();
    let map: std::collections::HashMap<String, String> = found
        .iter()
        .filter_map(|row| {
            let id: Uuid = row.get_typed("id").ok()?;
            let no: String = row.get_typed("instance_no").ok()?;
            Some((id.to_string(), no))
        })
        .collect();
    for row in rows.iter_mut() {
        if let Some(id) = row.approval_instance_id.as_deref()
            && let Some(no) = map.get(id)
        {
            row.approval_instance_no = Some(no.clone());
        }
    }
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
    // P-FLOW-3A 反向兜底：审批中作废 → **同一事务先终止在跑的审批单**（P11：不留孤儿待办，
    // 否则 `uk_biz_flow_instance_biz_active` 会让这张单以后再也发不起审批）
    if row.status == STATUS_SUBMITTED {
        purchase_bridge::cancel_approval_of(&tx, id, &row.order_no, "采购单已作废，审批自动终止")
            .await
            .map_err(|error| write_error(error, "作废：终止在跑的审批"))?;
    }
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
    // P-FLOW-3A：`SUBMITTED` 这个目标态走**审批入口**（有流程就先送审），
    // 其余（CLOSED / CANCELLED）仍是普通状态流转。
    if req.target_status()? == STATUS_SUBMITTED {
        submit_approval(id, &claims).await?;
    } else {
        change_purchase_order_status(id, &req, &claims).await?;
    }
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③b 提交审批（P-FLOW-3A） ====================

/// 提交采购订单审批的**唯一入口**：配了已发布的 `po_approve` 流程就先送审（`DRAFT → SUBMITTED`
/// + 审批单），没配就按 P-FLOW-3A 之前的行为直接提交（并留一条 `warn`）。
///
/// 为什么入口收在这里：`PATCH /status`（老前端）与 `POST /submit-approval`（本卡新按钮）
/// **必须同一条路** —— 否则审批可以被换一个 URL 绕过。
pub async fn submit_approval(
    id: Uuid,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    // 已提交（审批中）→ 409 且带审批单号（O5：报文要能直接照着去找）
    if row.status == STATUS_SUBMITTED {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "采购单 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.order_no, instance_no
        )));
    }
    match purchase_bridge::published_po_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单审批流程"))?
    {
        Some(flow) => submit_po_approval(&row, &op, &flow).await,
        None => {
            tracing::warn!(
                order_id = %row.id,
                order_no = %row.order_no,
                "没有配置已发布的 po_approve 采购审批流程：本次提交**降级**为直接提交（无审批），请尽快补配流程"
            );
            let req = PurchaseOrderStatusReq {
                status: Some(STATUS_SUBMITTED.to_string()),
                reason: None,
            };
            change_purchase_order_status(id, &req, op).await
        }
    }
}

/// **发起采购订单审批**：`DRAFT → SUBMITTED`，同一事务里状态机过闸 + 发起审批 + 回写实例。
async fn submit_po_approval(
    row: &PurchaseOrderRow,
    operator: &state_machine::Operator,
    flow: &purchase_bridge::PoFlow,
) -> std::result::Result<(), ApiError> {
    // ① 只有草稿能提交审批
    if row.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "采购订单 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交审批",
            row.order_no,
            status_label(&row.status),
            row.status
        )));
    }
    // ② 明细 / 金额闸（与直接提交同一套守卫：0 数量、0 金额不许送审）
    let lines = purchase_model::items_of(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单明细"))?;
    if lines.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("采购订单 {} 没有任何明细，不能提交审批", row.order_no),
        ));
    }
    ensure_submittable_lines(&lines)?;
    if row.total_with_tax <= Decimal::ZERO {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "采购订单 {} 的含税金额为 {}，不能提交审批（金额必须 > 0）",
                row.order_no, row.total_with_tax
            ),
        ));
    }
    let Some(operator_id) = operator.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    // ③ 幂等：同一单已有在跑的审批 → 409
    if flow_model::biz_instance_running(purchase_bridge::BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_instance_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "采购单 {} 已在审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.order_no, instance_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 采购订单审批", row.order_no)
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
        biz_no: Some(row.order_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: purchase_form_data(row).await,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启采购审批事务"))?;
    let outcome = submit_po_tx(&tx, row, operator, &start, now).await;
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
        .map_err(|error| utils::db_error(error, "提交采购审批事务"))?;
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
        order_id = %row.id,
        order_no = %row.order_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "采购订单已提交审批（状态 SUBMITTED）"
    );
    // 流程里没有任何审批节点（全被跳过）→ 实例在同一个事务里就走完了，
    // 提交后立刻执行「审批通过」的回调，否则单据会停在 SUBMITTED 且没有待办能推进它。
    if auto_finished {
        purchase_bridge::on_po_settled(Some(instance), "APPROVED").await;
    }
    Ok(())
}

/// [`submit_po_approval`] 的事务体：返回引擎动作结果（调用方提交后负责发锁外副作用）。
async fn submit_po_tx(
    tx: &Transaction,
    row: &PurchaseOrderRow,
    operator: &state_machine::Operator,
    start: &engine::StartReq,
    now: DateTime<Utc>,
) -> std::result::Result<engine::ActionOut, ApiError> {
    // ① 状态机唯一入口（边表 + 写 biz_status_log）
    state_machine::transition(
        tx,
        state_machine::ENTITY_PURCHASE_ORDER,
        row.id,
        &row.order_no,
        &purchase_model::LEGAL_EDGES,
        STATUS_DRAFT,
        STATUS_SUBMITTED,
        operator.clone(),
        Some("提交审批"),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一事务里建实例 + 待办 + 节点）
    let mut out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, "发起采购审批单", &row.order_no))?;
    if let Some(rejected) = out.rejected.take() {
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!(
                "{}（内控要求：审批人不能是发起人本人。请由他人提交，或让另一个账号具备审批权限后再审）",
                rejected.msg
            ),
        ));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 提交痕迹 + 实例 id
    let affected = purchase_model::apply_status_with_instance(
        tx,
        row.id,
        STATUS_DRAFT,
        STATUS_SUBMITTED,
        None,
        operator.id,
        Some(instance_id),
        now,
    )
    .await
    .map_err(|error| write_error(error, "采购单进入已提交"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "采购订单 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
            row.order_no
        )));
    }
    Ok(out)
}

/// 在跑实例的单号（拼 409 文案用；查不到就给 `-`）。
async fn pending_instance_no_of(
    instance_id: Option<Uuid>,
) -> std::result::Result<String, ApiError> {
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

/// 采购审批的**表单变量**。金额必须写成 JSON **数字**（不是字符串）：
/// 条件边比较走 `as_f64()`，`"5000.0000"` 会 TypeMismatch → 引擎按「条件不成立」静默走兜底边。
///
/// ⚠️ 返回类型写全 `serde_json::Value`：`use nik::prelude::*` 里的 `Value` 是数据库绑定值。
///
/// # P1-FIX：冻结显示名 + 明细（`supplier_name` / `warehouse_name` / `items`）
///
/// 审批岗位角色没有 `business:erp:*` 读权限（四个角色实测 0 条），前端「ID→名称」必然降级成
/// `(ID: …)`。所以把**人看的中文名**与**明细**一起冻进 `form_data`：审批人零 ERP 权限也能看懂，
/// 明细也不会因为单据后续变动而和审批时看到的不一致。键名契约见
/// [`crate::business::erp::form_snapshot`]。查询失败**不阻断提交**（降级为不写那几个键）：
/// 显示名缺了顶多退化回 `(ID: …)`，不该让审批发不出去。
async fn purchase_form_data(row: &PurchaseOrderRow) -> serde_json::Value {
    let amount = row.total_with_tax.round_dp(4).to_string();
    let number = amount
        .parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null);
    let mut form = serde_json::json!({
        "order_id": row.id.to_string(),
        "order_no": row.order_no,
        "amount": number,
        "total_with_tax": row.total_with_tax.to_string(),
        "total_amount": row.total_amount.to_string(),
        "total_qty": row.total_qty.to_string(),
        "supplier_id": row.supplier_id.to_string(),
        "warehouse_id": row.warehouse_id.to_string(),
        "price_mode": row.price_mode,
        // 内控留痕：审批人 ≠ 制单人（`purchase_bridge::guard_self_approval` 兜底）
        "create_by": row.create_by.map(|value| value.to_string()),
    });
    // 显示名 / 明细：查不到就**不写这个键**（前端按 `*_id` 兜底），绝不写空串冒充「查到了」。
    let (supplier, warehouse, items) = (
        form_snapshot::supplier_name(db(), row.supplier_id).await,
        form_snapshot::warehouse_name(db(), row.warehouse_id).await,
        form_snapshot::purchase_items(db(), row.id).await,
    );
    if let Some(object) = form.as_object_mut() {
        match supplier {
            Ok(Some(name)) => {
                object.insert("supplier_name".to_string(), serde_json::Value::String(name));
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(order_id = %row.id, %error, "审批快照：供应商名查询失败，降级")
            }
        }
        match warehouse {
            Ok(Some(name)) => {
                object.insert(
                    "warehouse_name".to_string(),
                    serde_json::Value::String(name),
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(order_id = %row.id, %error, "审批快照：仓库名查询失败，降级")
            }
        }
        match items {
            Ok(items) => {
                object.insert("items".to_string(), items);
            }
            Err(error) => {
                tracing::warn!(order_id = %row.id, %error, "审批快照：明细查询失败，降级")
            }
        }
    }
    form
}

/// 提交采购订单审批（`DRAFT → SUBMITTED`，或未配流程时降级直接提交）。
#[nik::permission(
    path = "/business/erp/purchase-order/{id}/submit-approval",
    method = "POST",
    permission = "business:erp:purchase-order:submit-approval"
)]
#[nik::post("/business/erp/purchase-order/{id}/submit-approval")]
pub async fn purchase_order_submit_approval(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<PurchaseOrderDetailRes> {
    let id = parse_uuid(&id, "id")?;
    submit_approval(id, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 删除 ====================

/// 删除核心逻辑：**DRAFT 可删**；**审批中（`SUBMITTED` 且确有在跑审批）也可删**
/// —— 后者会连同审批单一起终止（P11：不许留孤儿待办）。
///
/// 其余状态（已审批 / 部分收货 / 已收货 / 已关闭 / 已取消）是审批与收货的事实，只能作废。
pub async fn delete_purchase_order(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = purchase_model::find_order(id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购订单"))?
        .ok_or_else(|| not_found("采购订单"))?;
    let approving = row.status == STATUS_SUBMITTED
        && flow_model::biz_instance_running(purchase_bridge::BIZ_TYPE, id)
            .await
            .map_err(|error| utils::db_error(error, "查询在跑审批单"))?;
    if row.status != STATUS_DRAFT && !approving {
        return Err(conflict(format!(
            "采购订单当前是「{}」（{}），只有草稿（DRAFT）或审批中（SUBMITTED + 在跑审批）能删：\
             已提交 / 已审批 / 已收货 / 已关闭 / 已取消的单据是审批与收货的事实，只能作废（状态流转）",
            status_label(&row.status),
            row.status
        )));
    }
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启删除事务"))?;
    let outcome = async {
        // 先撤在跑的审批（幂等：没有就什么都不做），再软删
        purchase_bridge::cancel_approval_of(&tx, id, &row.order_no, "采购单已删除，审批自动终止")
            .await
            .map_err(|error| write_error(error, "删除：终止在跑的审批"))?;
        let affected = purchase_model::soft_delete(&tx, id, operator, now)
            .await
            .map_err(|error| utils::db_error(error, "软删采购订单"))?;
        if affected == 0 {
            return Err(not_found("采购订单"));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交删除事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
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

    // ---------- 审批实例唯一索引冲突：409 + 自家单号（不是「单号重复、去查 biz_seq」） ----------

    /// 撞 `uk_biz_flow_instance_biz_active` → **409 + 本单号 + 「已有审批中实例」**。
    ///
    /// 两条分支都钉住：驱动给了约束名（`UniqueViolation`）与退化形态（`DbError::InvalidValue`，
    /// 文案里只剩 PG 原文）—— 后者正是真库里掉到「联系运维排查 biz_seq」的那条路。
    #[test]
    fn biz_active_conflict_maps_to_409_with_order_no() {
        let variant = || DbError::UniqueViolation {
            column: None,
            constraint: Some(flow_model::BIZ_ACTIVE_INDEX.to_string()),
            source: None,
        };
        let by_text = || {
            DbError::InvalidValue(format!(
                "db error: ERROR: duplicate key value violates unique constraint \"{}\"",
                flow_model::BIZ_ACTIVE_INDEX
            ))
        };
        // 别的唯一索引不能被误判成审批冲突
        let other = DbError::UniqueViolation {
            column: None,
            constraint: Some("uk_biz_purchase_order_no".to_string()),
            source: None,
        };
        assert!(!flow_model::is_biz_active_duplicate(&other));

        for err in [variant(), by_text()] {
            let api = submit_conflict(err, "发起采购审批单", "PO-20261102-000123");
            assert_eq!(api.status, 409, "{}", api.msg);
            assert_eq!(api.code, error_code::RESOURCE_CONFLICT);
            assert!(api.msg.contains("PO-20261102-000123"), "{}", api.msg);
            assert!(api.msg.contains("已有审批中实例"), "{}", api.msg);
            assert!(
                !api.msg.contains("biz_seq"),
                "不能把用户指到取号器：{}",
                api.msg
            );
            assert!(!api.msg.contains("单号重复"), "{}", api.msg);
        }

        // 拿不到单号的通用出口（write_error）也必须 409 + 可执行文案
        let generic = write_error(variant(), "采购订单状态流转");
        assert_eq!(generic.status, 409);
        assert_eq!(generic.code, error_code::RESOURCE_CONFLICT);
        assert!(generic.msg.contains("该采购订单"), "{}", generic.msg);
        assert!(!generic.msg.contains("biz_seq"), "{}", generic.msg);

        // 真的号段冲突不能被新分支吞掉：仍然 409 + 取号器提示
        let genuine = write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_purchase_order_no".to_string()),
                source: None,
            },
            "创建采购订单",
        );
        assert_eq!(genuine.status, 409);
        assert!(genuine.msg.contains("biz_seq"), "{}", genuine.msg);
    }

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

    /// 本夹具的 `PO_APPROVE` 实例定位口径（三处复用，必须逐字一致）。
    ///
    /// 三条任一成立即算本夹具的：
    /// 1. `biz_id` 指向本夹具供应商的采购单；
    /// 2. `biz_id` 为空，或指向**已经不存在的采购单** —— 上一轮 `setup()` 删单时留下的孤儿，
    ///    正是「每跑一轮全量就多 7 条 `PO_APPROVE | PENDING`」的真身（实例 `biz_id` 是弱引用，
    ///    删单据带不走它）；
    /// 3. 都没有也算（`biz_id IS NULL`），清掉没有归属的残行。
    ///
    /// 不按 `biz_no LIKE 'PO-%'` 扫：号段前缀是生产口径（技能 §6.40 的教训），
    /// 而「孤儿 + 本夹具供应商的单据」这个外键链已经足够精确。
    fn po_instance_where() -> String {
        format!(
            "biz_type = 'PO_APPROVE' AND ( \
                 biz_id IS NULL \
              OR biz_id NOT IN (SELECT id FROM biz_purchase_order) \
              OR biz_id IN (SELECT id FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid))"
        )
    }

    /// 清掉本夹具的审批痕迹（**删单据之前**跑）：实例 / 待办 / 抄送 / 状态日志。
    ///
    /// # 顺序为什么必须是这样
    ///
    /// 1. `biz_purchase_order.approval_instance_id` 是弱引用 → 先置 NULL（否则单据指着不存在的实例，
    ///    `instance_status_of` 查不到就按「不拦」处理，门闸静默失效）；
    /// 2. `biz_flow_instance.prev_instance_id` 是**自引用且没有 ON DELETE**（V0122）→ 先解开，
    ///    漏了这一步整批 DELETE 会被外键拦下并抛 `query failed: db error`（PG 原文里没有约束名）；
    /// 3. `biz_flow_task` / `biz_flow_cc` 虽然 ON DELETE CASCADE，仍显式先删（把顺序写死）；
    /// 4. `biz_flow_instance` 本体（**含孤儿**，见 [`po_instance_where`]）；
    /// 5. `biz_status_log` **没有外键**，必须在删单据行之前按 `entity_id` 清，否则每轮多攒几十行
    ///    永远对不上的孤儿。
    async fn purge_approval_traces() {
        let inst_where = po_instance_where();
        let statements = [
            format!(
                "UPDATE biz_purchase_order SET approval_instance_id = NULL \
                  WHERE supplier_id = '{SUPPLIER}'::uuid"
            ),
            format!(
                "UPDATE biz_flow_instance SET prev_instance_id = NULL \
                  WHERE prev_instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
            ),
            format!(
                "DELETE FROM biz_flow_task \
                  WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
            ),
            format!(
                "DELETE FROM biz_flow_cc \
                  WHERE instance_id IN (SELECT id FROM biz_flow_instance WHERE {inst_where})"
            ),
            format!("DELETE FROM biz_flow_instance WHERE {inst_where}"),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND ( \
                     entity_id IN (SELECT id FROM biz_purchase_order \
                                    WHERE supplier_id = '{SUPPLIER}'::uuid) \
                  OR entity_id NOT IN (SELECT id FROM biz_purchase_order))"
            ),
        ];
        for sql in statements {
            if let Err(err) = db().execute_with_params(&sql, vec![]).await {
                panic!("清理采购审批痕迹失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 清掉本夹具的**全部**痕迹 + 单据（每个用例开头与模块级收尾共用）。
    ///
    /// 顺序不可交换：先解弱引用 / 自引用外键、删实例与状态日志，最后才删单据本体
    /// （详见 [`Self::purge_approval_traces`]）。
    async fn purge_fixture_docs() {
        purge_approval_traces().await;
        let sql = format!("DELETE FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid");
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("清理上一轮采购订单失败：{err}");
        }
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。`None` = 没配 `ERP_DB_URL`。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        // 先清审批痕迹（实例是弱引用，删单据带不走 → 不先清就会积累孤儿 PENDING），
        // 再清上一轮的订单（明细随主单 CASCADE）。
        purge_fixture_docs().await;
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

    // ==================== P-FLOW-3A：采购订单审批（8 条） ====================
    //
    // 这一批跑的是**真种子**（V0182 的 `po_approve`：7 节点 8 边，三个审批节点全是 ROLE），
    // 所以它同时验证「种子能发起、能派给角色、能按金额分流」——不是另造一张测试图。

    /// 角色夹具（V0182 种子里的固定 UUID）。
    const ROLE_DEPT_LEADER: &str = "20000000-0000-4000-8000-0000000f0001";
    /// 采购经理。
    const ROLE_PM: &str = "20000000-0000-4000-8000-0000000f0002";
    /// 总经理。
    const ROLE_GM: &str = "20000000-0000-4000-8000-0000000f0003";
    /// 制单人（无审批角色）。
    const U_MAKER: &str = "d2a2000a-0000-4000-8000-000000000001";
    /// 部门主管（DEPT_LEADER 成员）。
    const U_DEPT: &str = "d2a2000a-0000-4000-8000-000000000002";
    /// 采购经理（PURCHASE_MANAGER 成员）。
    const U_PM: &str = "d2a2000a-0000-4000-8000-000000000003";
    /// 总经理（GENERAL_MANAGER 成员）。
    const U_GM: &str = "d2a2000a-0000-4000-8000-000000000004";

    fn uid(raw: &str) -> Uuid {
        Uuid::parse_str(raw).expect("夹具 UUID 非法")
    }

    async fn sql(text: &str) {
        if let Err(err) = db().execute_with_params(text, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {text}");
        }
    }

    /// 审批夹具：四个账号 + 角色绑定 + 清掉本卡造过的审批实例。
    ///
    /// 四个种子角色是**共享资源**：别的用例 / 运维给他们绑过的人会让「这条待办派给谁」
    /// 不可判定，所以这里**只留本卡夹具的人**（按 id 白名单删绑定），并在模块末尾说明。
    async fn setup_approval() {
        sql(&format!(
            "DELETE FROM sys_user_role WHERE role_id IN \
                ('{ROLE_DEPT_LEADER}'::uuid, '{ROLE_PM}'::uuid, '{ROLE_GM}'::uuid) \
                AND user_id NOT IN ('{U_DEPT}'::uuid, '{U_PM}'::uuid, '{U_GM}'::uuid)"
        ))
        .await;
        for (id, name) in [
            (U_MAKER, "p3a 制单人"),
            (U_DEPT, "p3a 部门主管"),
            (U_PM, "p3a 采购经理"),
            (U_GM, "p3a 总经理"),
        ] {
            sql(&format!(
                "INSERT INTO sys_user (id, username, password, real_name, status) \
                 VALUES ('{id}'::uuid, 'p3a_{id_tail}', 'x', '{name}', 1) \
                 ON CONFLICT (id) DO NOTHING",
                id_tail = &id[id.len() - 4..]
            ))
            .await;
        }
        for (user, role) in [(U_DEPT, ROLE_DEPT_LEADER), (U_PM, ROLE_PM), (U_GM, ROLE_GM)] {
            sql(&format!(
                "INSERT INTO sys_user_role (user_id, role_id) VALUES ('{user}'::uuid, '{role}'::uuid) \
                 ON CONFLICT DO NOTHING"
            ))
            .await;
        }
        // 本卡夹具的单：先清审批痕迹（**含上一轮删单留下的孤儿实例**，见
        // `po_instance_where` 的第 2 条），再清订单本体 —— 顺序反了就会每轮攒一批孤儿 PENDING。
        purge_fixture_docs().await;
    }

    /// 造一张含税金额 = `amount` 的采购单（单行、税率 0，所以含税 = 不含税）。
    async fn order_with_amount(amount: &str) -> Uuid {
        let request = req("EXCL", &[(MATERIAL_A, "1", amount, "0")]);
        create_purchase_order(&request, Some(uid(U_MAKER)))
            .await
            .unwrap_or_else(|err| panic!("造单失败：{} {}", err.code, err.msg))
    }

    /// 该单在跑的实例 id（没有 → panic）。
    async fn running_instance(order_id: Uuid) -> Uuid {
        db().scalar_with_params::<Option<Uuid>>(
            "SELECT id FROM biz_flow_instance WHERE biz_type = 'PO_APPROVE' \
              AND biz_id = ?::uuid AND status = 'PENDING' LIMIT 1",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap()
        .expect("应当有在跑的审批实例")
    }

    /// 该实例的待办（PENDING），按 assignee 升序。
    async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
        let rows = db()
            .find_with_params(
                "SELECT id, assignee_id FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' \
                  ORDER BY assignee_id, id",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("读待办失败");
        rows.iter()
            .map(|row| {
                (
                    row.get_typed("id").unwrap(),
                    row.get_typed("assignee_id").unwrap(),
                )
            })
            .collect()
    }

    /// 某人在本实例上的待办（**不限状态**）：`(task_id, status, comment)`。
    ///
    /// P0-FIX 之后「制单人 / 发起人」不再拿 `PENDING`（生成时就被排除成 `SKIPPED`），
    /// 所以内控用例必须按「人」取行，而不是按 `status = 'PENDING'` 取。
    async fn task_of(instance_id: Uuid, assignee: Uuid) -> Option<(Uuid, String, String)> {
        let rows = db()
            .find_with_params(
                "SELECT id, status, COALESCE(comment,'') AS comment FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND assignee_id = ?::uuid ORDER BY created_at LIMIT 1",
                vec![Value::from(&instance_id), Value::from(&assignee)],
            )
            .await
            .expect("读待办失败");
        rows.first().map(|row| {
            (
                row.get_typed("id").unwrap(),
                row.get_typed("status").unwrap(),
                row.get_typed("comment").unwrap(),
            )
        })
    }

    /// 以某人的身份同意一条待办，并按 `flow::handler` 的口径推进业务回调。
    async fn approve_as(task_id: Uuid, operator: &str) -> crate::business::flow::engine::ActionOut {
        let op = uid(operator);
        let out = db()
            .transaction(|tx| async move {
                crate::business::flow::engine::approve(&tx, task_id, op, Some("同意")).await
            })
            .await
            .unwrap_or_else(|err| panic!("同意失败：{err}"));
        purchase_bridge::on_po_settled(out.instance_id, &out.instance_status).await;
        out
    }

    /// 以某人的身份驳回（驳回并终止）。
    async fn reject_as(
        task_id: Uuid,
        operator: &str,
        comment: &str,
    ) -> crate::business::flow::engine::ActionOut {
        let op = uid(operator);
        let out = db()
            .transaction(|tx| async move {
                crate::business::flow::engine::reject(
                    &tx,
                    task_id,
                    op,
                    comment,
                    crate::business::flow::engine::ReturnTo::Final,
                )
                .await
            })
            .await
            .unwrap_or_else(|err| panic!("驳回失败：{err}"));
        purchase_bridge::on_po_settled(out.instance_id, &out.instance_status).await;
        out
    }

    /// ① 提交审批 → 生成实例 + 状态 SUBMITTED + 回写 approval_instance_id + 写状态日志。
    #[tokio::test]
    async fn pflow3a_po_submit_creates_instance_and_submits() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)))
            .await
            .unwrap_or_else(|err| panic!("提交审批失败：{} {}", err.code, err.msg));

        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_SUBMITTED);
        let instance = row.approval_instance_id.expect("必须回写审批实例 id");
        let status: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "PENDING", "实例必须还在跑（等待审批）");
        let biz_no: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT biz_no FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(biz_no, row.order_no, "实例的 biz_no 必须是订单号");
        // 状态日志：建单 1 条 + 提交 1 条
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log WHERE entity_id = ?::uuid \
                  AND entity_type = 'PURCHASE_ORDER'",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(logs, 2, "建单 + 提交审批各一条状态日志");
    }

    /// ② 8000 元（< 1 万）→ 派给「部门主管」。
    #[tokio::test]
    async fn pflow3a_po_8000_goes_to_dept_leader() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        assert_eq!(
            tasks.len(),
            1,
            "第一档只有一个审批人，实际待办 {}",
            tasks.len()
        );
        assert_eq!(
            tasks[0].1,
            uid(U_DEPT),
            "8000 元必须派给 DEPT_LEADER（部门主管）"
        );
    }

    /// ③ 50000 元（1–10 万）→ 派给「采购经理」。
    #[tokio::test]
    async fn pflow3a_po_50000_goes_to_purchase_manager() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("50000").await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].1,
            uid(U_PM),
            "50000 元必须派给 PURCHASE_MANAGER（采购经理）"
        );
    }

    /// ④ 500000 元（≥ 10 万）→ 采购经理 + 总经理**会签**，只批一人不通过。
    #[tokio::test]
    async fn pflow3a_po_500000_needs_both_countersigners() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("500000").await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let instance = running_instance(id).await;
        let tasks = pending_tasks(instance).await;
        assert_eq!(tasks.len(), 2, "会签节点必须同时生成 2 条待办");
        let assignees: Vec<Uuid> = tasks.iter().map(|(_, who)| *who).collect();
        assert!(assignees.contains(&uid(U_PM)), "{}", U_PM);
        assert!(assignees.contains(&uid(U_GM)), "{}", U_GM);

        // 只批采购经理那一票 → 单据仍是 SUBMITTED（会签没走完）
        let pm_task = tasks.iter().find(|(_, who)| *who == uid(U_PM)).unwrap().0;
        let out = approve_as(pm_task, U_PM).await;
        assert_eq!(out.instance_status, "PENDING", "会签没走完不能结束");
        assert_eq!(
            purchase_model::find_order(id)
                .await
                .unwrap()
                .unwrap()
                .status,
            STATUS_SUBMITTED,
            "只批一票不能变 APPROVED"
        );

        // 总经理那一票再批 → 实例结束，单据 APPROVED
        let gm_task = tasks.iter().find(|(_, who)| *who == uid(U_GM)).unwrap().0;
        let out = approve_as(gm_task, U_GM).await;
        assert_eq!(out.instance_status, "APPROVED");
        assert_eq!(
            purchase_model::find_order(id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "APPROVED",
            "两人都批才 APPROVED"
        );
    }

    /// ⑤ 审批通过 → 单据 APPROVED（此后可收货）。
    #[tokio::test]
    async fn pflow3a_po_approve_marks_order_approved() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        approve_as(tasks[0].0, U_DEPT).await;

        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(row.status, "APPROVED");
        assert!(row.approved_at.is_some(), "审批通过必须写 approved_at");
        assert_eq!(row.approved_by, Some(uid(U_DEPT)), "审批人要落库");
        assert!(
            row.approval_instance_id.is_some(),
            "审批通过后保留实例 id 作审计（详情显示「审批通过 FLW-xxx」）"
        );
        // 采购买单进入可收货态
        assert!(stockin_model::purchase_receivable(&row.status));
    }

    /// ⑥ 审批驳回 → 回 DRAFT + 审批意见写进 remark。
    #[tokio::test]
    async fn pflow3a_po_reject_back_to_draft_with_comment() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let tasks = pending_tasks(running_instance(id).await).await;
        reject_as(tasks[0].0, U_DEPT, "预算不够，下季度再买").await;

        let row = purchase_model::find_order(id).await.unwrap().unwrap();
        assert_eq!(
            row.status, "DRAFT",
            "驳回必须打回草稿（不是关闭、不是取消）"
        );
        let remark = row.remark.unwrap_or_default();
        assert!(
            remark.contains("预算不够"),
            "审批意见必须写进 remark，实际：{remark}"
        );
        assert!(
            row.approval_instance_id.is_none(),
            "打回草稿必须清空审批实例（否则下次提交的判断被污染）"
        );
        // 打回后还能重新提交（边表 DRAFT → SUBMITTED 仍合法）
        submit_approval(id, Some(uid(U_MAKER)))
            .await
            .expect("打回后必须能重新提交审批");
    }

    /// ⑦ 制单人自批 → 403 + 65019（内控：制单与审批分离）。
    ///
    /// P0-FIX 之后语义是「**生成待办时就排除**」：制单人不再是 `PENDING`，而是留一条
    /// `SKIPPED` 行（带内控原因）—— 他既看不到待办，也批不了；而他人照常能批。
    /// 403 兜底仍在（转办 / 加签 / 老数据绕过时会走到它）。
    #[tokio::test]
    async fn pflow3a_po_maker_cannot_approve_own_order() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        // 让制单人**同时**是部门主管：内控要拦的就是这种「自己提自己批」
        sql(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) \
             VALUES ('{U_MAKER}'::uuid, '{ROLE_DEPT_LEADER}'::uuid) ON CONFLICT DO NOTHING"
        ))
        .await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let instance = running_instance(id).await;
        let (mine, status, comment) = task_of(instance, uid(U_MAKER))
            .await
            .expect("制单人在节点上必须有留痕行（不删待办，只改状态）");
        assert_eq!(
            status, "SKIPPED",
            "P0-FIX：制单人的票在生成时就被内控排除，不进 PENDING"
        );
        assert!(comment.contains("内控"), "留痕要写原因：{comment}");
        assert!(
            pending_tasks(instance)
                .await
                .iter()
                .all(|(_, who)| *who != uid(U_MAKER)),
            "制单人不该出现在「待批」清单里（他点不动）"
        );

        let err = purchase_bridge::guard_self_approval(mine, uid(U_MAKER))
            .await
            .expect_err("自批必须被拦");
        assert_eq!(err.status, 403, "{}", err.msg);
        assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(
            err.msg.contains("不能审批自己提交的采购订单"),
            "{}",
            err.msg
        );
        // 换成别的部门主管就能批（闸只拦本人）
        purchase_bridge::guard_self_approval(mine, uid(U_DEPT))
            .await
            .expect("他人可以正常审批");
        sql(&format!(
            "DELETE FROM sys_user_role WHERE user_id = '{U_MAKER}'::uuid \
              AND role_id = '{ROLE_DEPT_LEADER}'::uuid"
        ))
        .await;
    }

    /// ⑧ 重复提交审批 → 409（报文带审批单号，O5）。
    #[tokio::test]
    async fn pflow3a_po_duplicate_submit_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let instance = running_instance(id).await;
        let instance_no: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT instance_no FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();

        let err = submit_approval(id, Some(uid(U_MAKER)))
            .await
            .expect_err("重复提交必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains(&instance_no),
            "报文要点名审批单号：{}",
            err.msg
        );
        // 不能因此多出一张审批单
        let count: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'PO_APPROVE' AND biz_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 1, "同一单只能有一张审批实例");
    }

    /// ⑨ **P1-FIX 验收标准 1 + 3**：提交审批后 `form_data` 里冻结了显示名 / 明细，
    /// 且「我的待办」列表（`TaskRes`）直接带出同一份 `form_data`（前端不必再逐单查实例详情）。
    ///
    /// 这条用**真库 + 真审批链**跑：`create_purchase_order` → `submit_approval` → 读
    /// `biz_flow_instance.form_data` / `model::page_my_tasks`。卡面第 1 条的验收 SQL
    /// 就是这里的第一步。
    #[tokio::test]
    async fn p1fix_po_form_data_freezes_display_names_items_and_task_carries_it() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        // 8000 元 → 部门主管单节点（U_DEPT），便于查「我的待办」。
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER))).await.unwrap();
        let instance = running_instance(id).await;

        // 验收标准 1 的 SQL：直接读库里冻结的那一份。
        let text: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT form_data::text FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .expect("实例必须存在");
        let form: serde_json::Value =
            serde_json::from_str(&text).expect("form_data 必须是合法 JSON");

        // 显示名：审批岗位角色没有 business:erp:* 权限，这两个键是「看不见 ID 也能看懂」的全部依靠。
        assert_eq!(
            form["supplier_name"], "P2A2 测试供应商",
            "供应商中文名必须冻结进 form_data：{form}"
        );
        assert_eq!(
            form["warehouse_name"], "P2A2 测试仓",
            "收货仓库中文名必须冻结进 form_data：{form}"
        );
        // 明细：八个键齐全（前端按存在的键出列，少一个就是少一列）。
        let items = form["items"].as_array().expect("items 必须是数组");
        assert_eq!(items.len(), 1, "本单只有一行明细：{form}");
        for key in form_snapshot::ITEM_KEYS {
            assert!(
                items[0].get(key).is_some(),
                "明细行缺键 `{key}`（前端会少一列）：{form}"
            );
        }
        assert_eq!(items[0]["material_code"], "P2A2-M1");
        assert_eq!(items[0]["material_name"], "螺丝 M6x20");
        assert_eq!(items[0]["unit_name"], "个");
        assert_eq!(items[0]["qty"], "1.0000");
        assert_eq!(items[0]["amount"], "8000.0000");
        // 条件边要用的 `amount` 仍然是 **JSON 数字**（写成字符串会让条件边静默走兜底边）。
        assert_eq!(form["amount"], 8000.0);
        assert!(form["amount"].is_number(), "amount 必须是数字：{form}");

        // 验收标准 3：待办列表行自带同一份 form_data（`TaskRes.form_data`）。
        let todo = crate::business::flow::model::page_my_tasks(
            uid(U_DEPT),
            crate::business::flow::model::MY_TODO_STATUSES,
            &crate::business::flow::dto::TASK_KINDS,
            crate::business::flow::model::MyTaskOrder::Arrived,
            1,
            20,
            true,
        )
        .await
        .expect("查我的待办");
        let mine = todo
            .list
            .iter()
            .find(|row| row.task.instance_id == instance)
            .expect("部门主管必须有一条待办");
        assert_eq!(
            mine.form_data["supplier_name"], "P2A2 测试供应商",
            "待办列表必须直接带出 form_data（否则前端又得逐单补详情）"
        );
        assert_eq!(
            mine.form_data["total_with_tax"], "8000.0000",
            "卡片摘要要用的价税合计也在里面"
        );
        assert_eq!(mine.form_data["items"][0]["material_name"], "螺丝 M6x20");
    }

    /// ⑩ 显示名查不到（供应商被硬删 / id 写错）→ **不写这个键**，其余字段照旧。
    ///
    /// 契约：`None` 是「没查到」，不能写空串（前端会显示「供应商：」后面空空的假象），
    /// 更不能因此让审批发不出去。
    #[tokio::test]
    async fn p1fix_po_form_data_omits_unknown_display_names() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_purchase_order(&req("EXCL", &[(MATERIAL_A, "1", "100", "0")]), None)
            .await
            .unwrap();
        let mut row = purchase_model::find_order(id).await.unwrap().unwrap();
        // 指向一个不存在的供应商 / 仓库（模拟主数据被删）。
        row.supplier_id = Uuid::nil();
        row.warehouse_id = Uuid::nil();
        let form = purchase_form_data(&row).await;
        assert!(
            form.get("supplier_name").is_none(),
            "查不到供应商名就不能写这个键：{form}"
        );
        assert!(
            form.get("warehouse_name").is_none(),
            "查不到仓库名就不能写这个键：{form}"
        );
        // ids / 金额 / 明细照旧（降级只影响「显示名」）。
        assert_eq!(form["supplier_id"], Uuid::nil().to_string());
        assert_eq!(form["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(form["order_no"], row.order_no.as_str());
    }

    // ---------- ⑬ 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 本模块跑完**不许留**「PO_APPROVE 实例 / 待办 / 状态日志 / 夹具订单」四类残留。
    ///
    /// 为什么必须有这一条：`setup()` / `setup_approval()` 只在**每个用例开头**跑，
    /// 模块最后一个用例的产物没人收；同一库连跑第二遍时，遗留的
    /// `PO_APPROVE | PENDING` 会顶住 `uk_biz_flow_instance_biz_active`（下一次提交永久 409），
    /// `biz_status_log` 则每轮多攒几十行永远对不上的孤儿。libtest 按名字排序，`zz_` 必然最后。
    ///
    /// 顺带钉住「孤儿也算本夹具的」这条口径：本用例造的单被清场删掉后，实例
    /// （`biz_id` 是弱引用）必须**跟着一起**被 `purge_fixture_docs` 收走 ——
    /// 这正是「每跑一轮多 7 条 PENDING」的结构性缺口。
    #[tokio::test]
    async fn zz_teardown_purges_po_approval_residue() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_approval().await;
        let id = order_with_amount("8000").await;
        submit_approval(id, Some(uid(U_MAKER)))
            .await
            .expect("提交审批应成功");
        let instance_id = running_instance(id).await;
        assert!(
            !pending_tasks(instance_id).await.is_empty(),
            "清场前必须有待办，否则这条用例证明不了什么"
        );

        purge_fixture_docs().await;

        // ① 单据没了
        assert!(
            purchase_model::find_order(id)
                .await
                .expect("查单据")
                .is_none(),
            "清场后采购订单必须不存在"
        );
        // ② 实例没了（**孤儿也认**：单据已删，实例仍必须被收走）
        for (label, sql) in [
            (
                "残留实例",
                "SELECT count(*) FROM biz_flow_instance WHERE id = ?::uuid".to_string(),
            ),
            (
                "残留待办",
                "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid".to_string(),
            ),
        ] {
            let n: i64 = db()
                .scalar_with_params::<Option<i64>>(&sql, vec![Value::from(&instance_id)])
                .await
                .expect("查残留")
                .unwrap_or(0);
            assert_eq!(n, 0, "清场后{label}必须为 0");
        }
        // ③ 状态日志没了（无外键 → 必须在删单据之前清）
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'PURCHASE_ORDER' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0);
        assert_eq!(logs, 0, "清场后状态日志必须为 0");
        // ④ 全库不剩「在跑的采购审批实例」—— 顶住唯一索引的就是它
        let running: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_flow_instance \
                  WHERE biz_type = 'PO_APPROVE' AND status = 'PENDING' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("查在跑实例")
            .unwrap_or(0);
        assert_eq!(
            running, 0,
            "模块收尾后不许剩 PENDING 的采购审批实例（同库连跑第二遍的前提）"
        );
    }
}

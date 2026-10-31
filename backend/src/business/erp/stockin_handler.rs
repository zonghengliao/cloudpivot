//! 收货入库单接口层（批次 D · P2-B1）：建 / 改 / 状态流转（过账）/ 删 / 列表 / 详情。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/stock-in/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/stock-in` | `business:erp:stock-in:create` | 建单（`DRAFT` + `IN-yyyyMMdd-000001`，金额服务端算） |
//! | PUT | `/business/erp/stock-in/{id}` | `business:erp:stock-in:update` | 改单（**仅 `DRAFT`**） |
//! | PATCH | `/business/erp/stock-in/{id}/status` | `business:erp:stock-in:status` | `DRAFT→POSTED`（过账，**核心**）/ `→CANCELLED` |
//! | DELETE | `/business/erp/stock-in/{id}` | `business:erp:stock-in:delete` | 软删（**仅 `DRAFT`**） |
//! | GET | `/business/erp/stock-in/page` | `business:erp:stock-in:list` | 分页（关键词 / 状态 / 供应商 / 仓库 / 日期区间） |
//! | GET | `/business/erp/stock-in/{id}` | `business:erp:stock-in:detail` | 详情（单头 + 明细 + 序列号件数） |
//!
//! # 为什么业务逻辑拆成普通 async 函数
//!
//! axum handler 要 `AuthClaims` + `Json` 才能跑，单测没法直接调；把「校验 + 取号 + 落库 +
//! 过账」收进 [`create_stock_in`] / [`update_stock_in`] / [`change_stock_in_status`] /
//! [`delete_stock_in`] 之后，单测走**同一条代码路径**（只跳过认证）。
//!
//! # ★ 过账（硬红线 2 / 3 / 4 / 7）
//!
//! 一个事务里做完六件事（顺序即代码顺序）：
//!
//! ```text
//! ① 锁单头 SELECT … FOR UPDATE → 状态必须 DRAFT，否则 409（重复过账 = 409，幂等）
//! ② CAS 写状态 DRAFT → POSTED（posted_at / posted_by）
//! ③ 明细**按 (material_id, location_id, line_no) 升序**逐行：库存加法 + 库位账加法 + 写 PURCHASE_IN 流水
//!    · 加锁用 SELECT … FOR UPDATE（同一物料同一库位并发串行化，before_* 快照不打架）
//!    · new_amount = old_amount + 不含税金额；new_avg = ROUND(new_amount / new_qty, 6)（SQL 侧算）
//!    · P6 补口：同一行还要把 qty 加到 `biz_inventory_location`（成本账 + 库位账两本账同事务，
//!      见 [`stockin_model::apply_location_qty`]；「有库位」按行 id UPDATE、「无库位」加 NULL 桶）
//! ④ 一物一码物料生成 / 落序列号（数量 = 本行 qty）
//! ⑤ 带来源采购单时回写 received_qty（条件 `received_qty + 本次 ≤ qty`，0 行 = 超收 400）
//! ⑥ 推送采购单状态 PARTIAL_RECEIVED / RECEIVED
//! ```
//!
//! ⚠️ 成本口径：`biz_inventory.amount` 与流水 `amount` 只加**不含税金额**
//! （PRD §3.2：进项税价外可抵扣，不进存货成本）。
//!
//! # 与卡面的一处入参差异（`serial_ids` 在过账时提交）
//!
//! `biz_stock_in_item` **没有存序列号的列**，硬红线 5 又不许改 V0137-V0145。所以：
//!
//! * 建单 / 改单的 `items[].serial_ids` 会被**校验**（条数必须 = qty、单行内不重复、≤64 字），
//!   但**不落库**（卡面 ⑩ 的 400 在建单就生效）；
//! * 真正落库的序列号在 `PATCH …/status` 的 body 里给：
//!   `{"status":"POSTED","serials":[{"line_no":1,"serial_ids":["SN-...","SN-..."]}]}`；
//! * 不给 = 由系统按 `SN-yyyyMMdd-NNNNNN` 取号生成（卡面 ③ 的默认路径）。
//!
//! 这条差异写进证据文档与看板：想「建单时贴码、过账时自动用」需要给明细表加一列（属后续迁移）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 收货入库单是 ERP 单据表（V0145 建表，无 dept_id / 归属人列），
//! 可见性由 business:erp:stock-in:* 权限码控制；分页走 scope::paginate_unscoped（闸①口径）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
//! ```

use std::collections::HashMap;

use axum::{Extension, Json};
use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::model;
use crate::business::erp::payable;
use crate::business::erp::stockin_dto::{
    CreateStockInReq, MAX_REASON_LEN, PreparedStockIn, StockInDetailRes, StockInItemRes,
    StockInPageQuery, StockInRes, StockInStatusReq,
};
use crate::business::erp::stockin_model::{
    self, CostBasis, NewStockIn, NewStockInLine, STATUS_CANCELLED, STATUS_DRAFT, STATUS_POSTED,
    Totals, can_transition, status_label, transition_rejected_message,
};
use crate::business::erp::voucher;
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

/// 可空 UUID 原文 → `Option<Uuid>`（空 = `None`）。
fn parse_optional_uuid(
    raw: Option<&String>,
    field: &str,
) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(|text| text.trim()) {
        None | Some("") => Ok(None),
        Some(text) => parse_uuid(text, field).map(Some),
    }
}

/// 操作人（`create_by` / `posted_by`）。
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

/// 参数类 400（`10001`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 作废路径的「状态不允许」哨兵归类（模型层只回 `DbError`，这里映射成 `409` 文案）。
enum CancelGuard {
    /// 库存 / 库位账不够退（货已部分出库）→ 文案带「请先做退货入库冲销」。
    Shipped,
    /// 其余带哨兵的拒绝（序列号已出库 / 过账流水缺失）→ 文案原样前置「不能作废：」。
    Detail,
}

/// 作废路径的错误收口：库存不足（[`stockin_model::SHORTFALL_PREFIX`]）/ 序列号已出库
/// （[`stockin_model::SERIAL_SHIPPED_PREFIX`]）/ 过账流水缺失（[`stockin_model::MISSING_TXN_PREFIX`]）
/// → `409 + 文案`。
///
/// 「货已部分出库」不是参数错（400）、也不是服务端故障（500），而是**当前状态不允许**
/// —— 必须先做退货入库冲销，所以是 409。
fn cancel_write_error(err: DbError, ctx: &str) -> ApiError {
    let sentinel = match &err {
        DbError::InvalidValue(message) => message
            .strip_prefix(stockin_model::SHORTFALL_PREFIX)
            .map(|detail| (CancelGuard::Shipped, detail.to_string()))
            .or_else(|| {
                message
                    .strip_prefix(stockin_model::SERIAL_SHIPPED_PREFIX)
                    .map(|detail| (CancelGuard::Detail, detail.to_string()))
            })
            .or_else(|| {
                message
                    .strip_prefix(stockin_model::MISSING_TXN_PREFIX)
                    .map(|detail| (CancelGuard::Detail, detail.to_string()))
            }),
        _ => None,
    };
    match sentinel {
        Some((CancelGuard::Shipped, detail)) => ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("该单的货已部分出库（{detail}），不能直接作废；请先做退货入库冲销"),
        ),
        Some((CancelGuard::Detail, detail)) => ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("不能作废：{detail}"),
        ),
        None => write_error(err, ctx),
    }
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`、外键 → `400 + 66002`。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "收货入库单号或序列号重复（唯一约束{}）：取号器与库里的号段不一致，\
                 请联系运维排查 biz_seq",
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
                "数据不满足库约束 {}（三金额必须精确恒等 with_tax_amount = amount + tax_amount；\
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
        // P2-C1：应付 / 凭证生成失败（科目映射缺失、金额为 0、重复红冲…）都是**业务校验**
        // 失败，必须 400 + 原文，不能折成 500「服务器内部错误」——否则财务看到的是
        // 「服务器内部错误」，运维也定位不到是哪个科目没配。
        DbError::InvalidValue(message) => ApiError::new(400, error_code::PARAM_INVALID, message),
        other => utils::db_error(other, ctx),
    }
}

// ==================== 详情组装 ====================

/// 详情：单头 + 明细 + 本单序列号件数。
pub async fn detail_of(id: Uuid) -> std::result::Result<StockInDetailRes, ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    let items = stockin_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单明细"))?;
    let serial_count = stockin_model::serial_count_of(id)
        .await
        .map_err(|error| utils::db_error(error, "统计收货入库单序列号"))?;
    Ok(StockInDetailRes::with_summary(
        StockInRes::from(&row),
        items.iter().map(StockInItemRes::from).collect(),
        serial_count,
    ))
}

// ==================== 明细规划（校验 + 查引用） ====================

/// 一行「已解析好、可落库」的明细。
///
/// `serial_required` 不在这里存：过账时按物料**重新查一次**（建单到过账之间物料的
/// 一物一码开关可能被改），建单期只用来拒绝「非一物一码物料却给了 serial_ids」。
#[derive(Debug, Clone)]
struct LinePlan {
    /// 待落库的明细。
    new: NewStockInLine,
}

/// 把校验后的 DTO 明细变成可落库的明细：查物料（`unit_id` / `serial_required`）、
/// 查库位、核对采购明细行的归属与未收数量。
async fn plan_lines(
    prepared: &PreparedStockIn,
    source_order_id: Option<Uuid>,
    warehouse_id: Uuid,
) -> std::result::Result<Vec<LinePlan>, ApiError> {
    let mut plans = Vec::with_capacity(prepared.lines.len());
    // 同一采购明细行在一张收货单里可能出现多次（同物料不同库位），超收要按「合计」判
    let mut requested_by_source: HashMap<Uuid, Decimal> = HashMap::new();

    for (index, line) in prepared.lines.iter().enumerate() {
        let material_id = parse_uuid(
            &line.material_id_raw,
            &format!("items[{index}].material_id"),
        )?;
        let material = model::material_find_by_id(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| {
                reference_missing(format!(
                    "items[{index}].material_id 物料不存在或已删除（{material_id}）"
                ))
            })?;
        let serial_required = stockin_model::material_serial_required(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料一物一码开关"))?
            .unwrap_or(false);
        if !serial_required && !line.serial_ids.is_empty() {
            return Err(ApiError::new(
                400,
                error_code::ERP_SERIAL_NOT_REQUIRED,
                format!(
                    "items[{index}]（物料 {}）不是一物一码物料（serial_required = false），\
                     不能给 serial_ids：{} 条号码无处可落",
                    material.material_code,
                    line.serial_ids.len()
                ),
            ));
        }

        let location_id = match line.location_id_raw.as_deref() {
            None => None,
            Some(raw) => {
                let location_id = parse_uuid(raw, &format!("items[{index}].location_id"))?;
                let reference =
                    crate::business::erp::serial_model::inventory_location_ref(location_id)
                        .await
                        .map_err(|error| utils::db_error(error, "查询库位库存行"))?
                        .ok_or_else(|| {
                            reference_missing(format!(
                                "items[{index}].location_id 库位库存行不存在（{location_id}；\
                             必须是 biz_inventory_location.id，不是库位字典 id）"
                            ))
                        })?;
                if reference.warehouse_id != warehouse_id {
                    return Err(reference_missing(format!(
                        "items[{index}].location_id 的库位属于仓库 {}，与收货仓库 {warehouse_id} 不一致",
                        reference.warehouse_id
                    )));
                }
                Some(location_id)
            }
        };

        let source_item_id = parse_optional_uuid(
            line.source_item_id_raw.as_ref(),
            &format!("items[{index}].purchase_order_item_id"),
        )?;
        if source_item_id.is_some() && source_order_id.is_none() {
            return Err(bad_request(format!(
                "items[{index}].purchase_order_item_id 给了，但单头没有 source_order_id：\
                 来源行必须挂在具体的采购单上"
            )));
        }
        if let Some(item_id) = source_item_id {
            let (order_of_item, po_item) = po_item_of(item_id).await?;
            if Some(order_of_item) != source_order_id {
                return Err(reference_missing(format!(
                    "items[{index}].purchase_order_item_id（{item_id}）不属于本单的来源采购单"
                )));
            }
            let remaining = stockin_model::remaining_qty(&po_item);
            let requested = requested_by_source.entry(item_id).or_default();
            *requested += line.qty;
            if *requested > remaining {
                return Err(bad_request(format!(
                    "items[{index}] 收货数量超过采购单未收数量：该行采购 {} {}、已收 {} {}，\
                     本单累计要收 {} {}（超出 {}）",
                    po_item.qty,
                    "件",
                    po_item.received_qty,
                    "件",
                    *requested,
                    "件",
                    *requested - remaining
                )));
            }
        }

        plans.push(LinePlan {
            new: NewStockInLine {
                material_id,
                unit_id: material.unit_id,
                source_item_id,
                qty: line.qty,
                price: line.price_excl,
                price_incl: line.price_incl,
                tax_rate: line.tax_rate,
                amount: line.amount,
                tax_amount: line.tax_amount,
                with_tax_amount: line.with_tax_amount,
                location_id,
                serial_ids: line.serial_ids.clone(),
                remark: line.remark.clone(),
            },
        });
    }
    Ok(plans)
}

/// 取采购明细行 + 它所属的采购单 id。
async fn po_item_of(
    item_id: Uuid,
) -> std::result::Result<(Uuid, crate::business::erp::model::PurchaseOrderItemRow), ApiError> {
    let row = model::purchase_order_item_find_by_id(item_id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购明细行"))?
        .ok_or_else(|| reference_missing(format!("采购明细行不存在（{item_id}）")))?;
    Ok((row.order_id, row))
}

/// 校验来源采购单：存在、可收货、仓库一致。返回采购单行。
async fn require_purchase_order(
    order_id: Uuid,
    warehouse_id: Uuid,
) -> std::result::Result<model::PurchaseOrderRow, ApiError> {
    let order = stockin_model::purchase_order_ref(order_id)
        .await
        .map_err(|error| utils::db_error(error, "查询来源采购单"))?
        .ok_or_else(|| {
            reference_missing(format!(
                "source_order_id 采购单不存在或已删除（{order_id}）"
            ))
        })?;
    if !stockin_model::purchase_receivable(&order.status) {
        return Err(bad_request(format!(
            "来源采购单 {} 当前是「{}」（{}），不能收货：只有已提交（SUBMITTED）/ 已审批（APPROVED）/\
             部分收货（PARTIAL_RECEIVED）的采购单可以收",
            order.order_no,
            purchase_status_label(&order.status),
            order.status
        )));
    }
    if order.warehouse_id != warehouse_id {
        return Err(bad_request(format!(
            "收货仓库（{warehouse_id}）与采购单 {} 的收货仓库（{}）不一致",
            order.order_no, order.warehouse_id
        )));
    }
    Ok(order)
}

/// 采购单状态中文标签（只用于文案）。
fn purchase_status_label(status: &str) -> &'static str {
    match status {
        "DRAFT" => "草稿",
        "SUBMITTED" => "已提交",
        "APPROVED" => "已审批",
        "PARTIAL_RECEIVED" => "部分收货",
        "RECEIVED" => "已收货",
        "CLOSED" => "已关闭",
        "CANCELLED" => "已取消",
        _ => "未知状态",
    }
}

/// 带 `source_order_id` 且没给明细时，按采购单**未收数量**自动带出一批明细。
///
/// 口径：只取 `qty - received_qty > 0` 的行，单价 / 税率沿用采购明细（含税单价也照搬快照），
/// `source_item_id` 落该采购行 id —— 这样过账能回写 `received_qty`、后续能判超收。
async fn pull_lines_from_purchase(
    order_id: Uuid,
) -> std::result::Result<Vec<crate::business::erp::stockin_dto::StockInItemReq>, ApiError> {
    let items = model::purchase_order_items(order_id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购单明细"))?;
    let mut lines = Vec::new();
    for item in items {
        let remaining = stockin_model::remaining_qty(&item);
        if remaining <= Decimal::ZERO {
            continue;
        }
        lines.push(crate::business::erp::stockin_dto::StockInItemReq {
            purchase_order_item_id: Some(item.id.to_string()),
            material_id: Some(item.material_id.to_string()),
            qty: Some(remaining),
            price_excl: Some(item.price),
            tax_rate: Some(item.tax_rate),
            location_id: None,
            serial_ids: Vec::new(),
            remark: Some(format!("由采购单自动带出（未收 {remaining}）")),
        });
    }
    if lines.is_empty() {
        return Err(bad_request(
            "来源采购单每一行都已收满，没有可自动带出的未收数量：请手工指定要收的明细",
        ));
    }
    Ok(lines)
}

/// 采购单明细快照（超收文案用）：`source_item_id → (qty, received_qty)`。
async fn purchase_item_snapshot(
    order_id: Option<Uuid>,
) -> std::result::Result<HashMap<Uuid, (Decimal, Decimal)>, ApiError> {
    let mut map = HashMap::new();
    let Some(order_id) = order_id else {
        return Ok(map);
    };
    let items = model::purchase_order_items(order_id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购单明细"))?;
    for item in items {
        map.insert(item.id, (item.qty, item.received_qty));
    }
    Ok(map)
}

// ==================== ① 建单 ====================

/// 建单核心逻辑（handler 与单测共用）。
///
/// 顺序：DTO 校验 + 算金额（纯函数）→ 供应商 / 仓库存在 → 来源采购单可收 →
/// （明细为空时按采购单未收数量带出）→ 逐行物料 / 库位 / 采购行校验 → 一个事务里「取号 + 单头 + 明细」。
pub async fn create_stock_in(
    req: &CreateStockInReq,
    operator: Option<Uuid>,
) -> std::result::Result<Uuid, ApiError> {
    let mut request = req.clone();
    let source_order_id = parse_optional_uuid(request.source_order_id.as_ref(), "source_order_id")?;
    if request.items.is_empty() {
        let Some(order_id) = source_order_id else {
            return Err(bad_request(
                "items 不能为空：没有来源采购单时至少要有一行明细",
            ));
        };
        request.items = pull_lines_from_purchase(order_id).await?;
    }

    let prepared = request.prepare(false)?;
    let supplier_id = parse_uuid(&prepared.supplier_id_raw, "supplier_id")?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;

    model::supplier_find_by_id(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;
    model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;
    if let Some(order_id) = source_order_id {
        require_purchase_order(order_id, warehouse_id).await?;
    }

    let plans = plan_lines(&prepared, source_order_id, warehouse_id).await?;
    let lines: Vec<NewStockInLine> = plans.iter().map(|plan| plan.new.clone()).collect();
    let totals = Totals::of(&lines);
    let new = NewStockIn {
        supplier_id,
        warehouse_id,
        source_order_id,
        in_date: prepared.in_date,
        invoice_received: prepared.invoice_received,
        remark: prepared.remark.as_deref(),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    match stockin_model::create(&tx, &new, &lines, &totals, operator, now).await {
        Ok(id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交建单事务"))?;
            Ok(id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "创建收货入库单"))
        }
    }
}

/// 新建收货入库单（`status = DRAFT`，单号 `IN-yyyyMMdd-000001`）。
#[nik::permission(
    path = "/business/erp/stock-in",
    method = "POST",
    permission = "business:erp:stock-in:create"
)]
#[nik::post("/business/erp/stock-in")]
pub async fn stock_in_create(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<CreateStockInReq>,
) -> ResResult<StockInDetailRes> {
    let id = create_stock_in(&req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ② 改单 ====================

/// 改单核心逻辑：**只有 `DRAFT` 能改**（其它状态 → 409），明细整体重建。
pub async fn update_stock_in(
    id: Uuid,
    req: &CreateStockInReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let current = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if current.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "收货入库单当前是「{}」（{}），只有草稿（DRAFT）能编辑：已过账的单据已经加过库存，\
             要改只能作废后重开",
            status_label(&current.status),
            current.status
        )));
    }

    let mut request = req.clone();
    let source_order_id = parse_optional_uuid(request.source_order_id.as_ref(), "source_order_id")?;
    if request.items.is_empty() {
        let Some(order_id) = source_order_id else {
            return Err(bad_request(
                "items 不能为空：没有来源采购单时至少要有一行明细",
            ));
        };
        request.items = pull_lines_from_purchase(order_id).await?;
    }

    let prepared = request.prepare(false)?;
    let supplier_id = parse_uuid(&prepared.supplier_id_raw, "supplier_id")?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;
    model::supplier_find_by_id(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;
    model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;
    if let Some(order_id) = source_order_id {
        require_purchase_order(order_id, warehouse_id).await?;
    }

    let plans = plan_lines(&prepared, source_order_id, warehouse_id).await?;
    let lines: Vec<NewStockInLine> = plans.iter().map(|plan| plan.new.clone()).collect();
    let totals = Totals::of(&lines);
    let new = NewStockIn {
        supplier_id,
        warehouse_id,
        source_order_id,
        in_date: prepared.in_date,
        invoice_received: prepared.invoice_received,
        remark: prepared.remark.as_deref(),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启改单事务"))?;
    match stockin_model::update(&tx, id, &new, &lines, &totals, operator, now).await {
        Ok(0) => {
            let _ = tx.rollback().await;
            Err(conflict(
                "收货入库单状态在本次编辑期间已被改动（只有草稿能改），请刷新后重试",
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
            Err(write_error(error, "编辑收货入库单"))
        }
    }
}

/// 编辑收货入库单（仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}",
    method = "PUT",
    permission = "business:erp:stock-in:update"
)]
#[nik::put("/business/erp/stock-in/{id}")]
pub async fn stock_in_update(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<CreateStockInReq>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    update_stock_in(id, &req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③ 状态流转（过账 / 作废） ====================

/// 状态流转核心逻辑（handler 与单测共用）。
///
/// `POSTED` 走 [`post_stock_in`]（一个事务里做完六件事），`CANCELLED` 只改状态。
pub async fn change_stock_in_status(
    id: Uuid,
    req: &StockInStatusReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let target = req.target_status()?;
    let reason = req.reason_text();
    if let Some(text) = reason
        && text.chars().count() > MAX_REASON_LEN
    {
        return Err(bad_request(format!(
            "reason 超过 {MAX_REASON_LEN} 字（当前 {} 字）",
            text.chars().count()
        )));
    }

    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if !can_transition(&row.status, target) {
        if row.status == STATUS_POSTED && target == STATUS_POSTED {
            return Err(conflict(format!(
                "收货入库单 {} 已过账（POSTED，时间 {}），不能重复过账：库存只加一次，\
                 若要重收请先作废后重开",
                row.in_no,
                row.posted_at
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string())
            )));
        }
        // 幂等（卡面 §一 ⑦）：已作废再作废 → 409，绝不二次退库存
        if row.status == STATUS_CANCELLED && target == STATUS_CANCELLED {
            return Err(conflict(format!(
                "收货入库单 {} 已经是「已作废」（CANCELLED），不能重复作废：库存只退一次",
                row.in_no
            )));
        }
        return Err(conflict(transition_rejected_message(&row.status, target)));
    }

    if target == STATUS_POSTED {
        return post_stock_in(id, &row, req, operator).await;
    }

    // 作废：DRAFT 从未加过库存；POSTED 的库存/库位账/流水在同一事务里反向冲减（P9）
    if row.status == STATUS_POSTED && reason.is_none() {
        return Err(bad_request(
            "已过账的收货单作废必须给出原因（reason）：库存已增加，红冲要能追溯到为什么",
        ));
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    match cancel_in_tx(&tx, id, &row, reason, operator, now).await {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交状态流转事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 作废事务体（P9 补齐库存回滚 + P2-C1 的凭证红冲 + P10 补序列号退账）：
/// CAS 改状态 → **退序列号 + 库存/库位账反向冲减 + 反向流水** → 红冲凭证 → 应付单作废，
/// **同一个事务**。
///
/// ⚠️ P9 之前这里只做凭证红冲，库存与流水**没退**（不变式 E4 挂在 2 组数据上）。
/// 现在按卡面 §一 的 7 步补齐：锁库存 → 校验可退 → 退成本账 → 退库位账 → 写
/// `OUT / PURCHASE_IN_CANCEL` 流水 → 红字凭证 → 应付作废。任一步失败 → 整单回滚
/// （handler 的 `tx.rollback()`），库存 / 凭证 / 流水三处同时不变。
///
/// ⚠️ P10 补上第 ② 步「退序列号」：一物一码物料的序列号行挂在
/// `source_type='PURCHASE' AND source_id = 本单` 上，作废不退会让
/// `biz_material_serial` 里留下 `IN_STOCK` 的幽灵号，与 `biz_inventory.qty`
/// （作废后已减）对不上 —— 就是 W2c 口径的账实不符。口径见
/// [`stockin_model::void_serials_of`]（未出库 → 软删；已出库/报废 → `409`）。
async fn cancel_in_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    row: &stockin_model::StockInRow,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    let affected =
        stockin_model::apply_status(ex, id, &row.status, STATUS_CANCELLED, reason, operator, now)
            .await
            .map_err(|error| write_error(error, "收货入库单状态流转"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单状态在本次流转期间已被改动（期望 {}），请刷新后重试",
            row.status
        )));
    }

    if row.status == STATUS_POSTED {
        // ① 读作废计划：数量取明细、金额取过账流水（不按现价估算）
        let plan = stockin_model::cancel_plan_of(ex, id, &row.in_no, row.warehouse_id)
            .await
            .map_err(|error| cancel_write_error(error, "作废：读取过账流水"))?;
        let doc = stockin_model::CancelDoc {
            id,
            in_no: &row.in_no,
        };
        // ② 退序列号（一物一码）：本单生成的号必须全部仍在库，否则 409；在库则软删
        //    （`deleted_at` 置位、行保留 —— 审计痕迹不丢，同号可被重新贴用）。
        //    放在库存冲减**之前**：一物一码物料的「货已出库」最硬的证据是序列号状态，
        //    先判它才能给出「哪一件已经出库」这种可执行报文，而不是笼统的「库存不足」。
        let serials_voided = stockin_model::void_serials_of(ex, id, &row.in_no, operator, now)
            .await
            .map_err(|error| cancel_write_error(error, "作废：退回一物一码序列号"))?;
        // ③ 逐行反向冲减（成本账 → 库位账 → 反向流水）；锁顺序 = 计划的排序（material_id 升序）
        let mut reversed_qty = Decimal::ZERO;
        let mut reversed_amount = Decimal::ZERO;
        for line in &plan {
            let applied = stockin_model::reverse_cost(
                ex,
                line.material_id,
                row.warehouse_id,
                line.location_id,
                line,
                now,
            )
            .await
            .map_err(|error| cancel_write_error(error, "作废：库存反向冲减"))?;
            stockin_model::reverse_location_qty(
                ex,
                line.material_id,
                row.warehouse_id,
                line.location_id,
                line.qty,
                now,
            )
            .await
            .map_err(|error| cancel_write_error(error, "作废：库位账反向冲减"))?;
            stockin_model::write_cancel_txn(ex, &doc, line, &applied, operator, now)
                .await
                .map_err(|error| write_error(error, "作废：写反向流水"))?;
            reversed_qty += line.qty;
            reversed_amount += line.amount;
        }

        // ④ 红字凭证（借贷互换，原凭证保持 POSTED 并打 reversed_by）
        let red = voucher::reverse_for_stock_in(ex, row, reason, operator, now)
            .await
            .map_err(|error| write_error(error, "作废：生成红字凭证"))?;
        // ⑤ 应付单 → CANCELLED
        payable::cancel_for_stock_in(ex, id, operator, now)
            .await
            .map_err(|error| write_error(error, "作废：应付单作废"))?;
        tracing::info!(
            stock_in_id = %id,
            in_no = %row.in_no,
            lines = plan.len(),
            reversed_qty = %reversed_qty,
            reversed_amount = %reversed_amount,
            serials_voided = serials_voided,
            red_voucher_id = %red.map(|v| v.to_string()).unwrap_or_else(|| "(无原凭证)".to_string()),
            "收货单作废：库存/库位账已反向冲减 + 反向流水已写 + 序列号已退回 + 红字凭证已生成 + 应付单已作废"
        );
        if red.is_none() {
            tracing::warn!(
                stock_in_id = %id,
                in_no = %row.in_no,
                "收货单作废：没有找到原凭证（历史数据），只作废了应付单"
            );
        }
    }
    Ok(())
}

/// **过账**（本卡核心）：一个事务里改状态 + 加库存 + 写流水 + 生成序列号 + 回写采购单。
async fn post_stock_in(
    id: Uuid,
    header: &stockin_model::StockInRow,
    req: &StockInStatusReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let items = stockin_model::items_of(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单明细"))?;
    if items.is_empty() {
        return Err(bad_request("收货入库单没有任何明细，无法过账"));
    }

    // 一物一码开关：事务外查（少占锁）
    let mut serial_required: HashMap<Uuid, bool> = HashMap::new();
    for item in &items {
        let flag = stockin_model::material_serial_required(item.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料一物一码开关"))?
            .unwrap_or(false);
        serial_required.insert(item.material_id, flag);
    }
    let provided = request_serials(req, &items)?;
    let po_snapshot = purchase_item_snapshot(header.source_order_id).await?;

    // PRD §3.2.6：多行按 material_id 升序加锁（再按库位 / 行号，保证全序），避免两个收货单互相死锁
    let mut ordered: Vec<&stockin_model::StockInItemRow> = items.iter().collect();
    ordered.sort_by(|a, b| {
        (a.material_id, a.location_id, a.line_no).cmp(&(b.material_id, b.location_id, b.line_no))
    });

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启过账事务"))?;
    let outcome = post_in_tx(
        &tx,
        id,
        header,
        &ordered,
        &serial_required,
        &provided,
        &po_snapshot,
        operator,
        now,
    )
    .await;
    match outcome {
        Ok(()) => {
            // 逐行写失败时由 drop 回滚（不 commit 就等于回滚），但显式 rollback 更清楚
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交过账事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 过账事务体（抽出来便于「失败即回滚」的统一收口）。
#[allow(clippy::too_many_arguments)]
async fn post_in_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    header: &stockin_model::StockInRow,
    ordered: &[&stockin_model::StockInItemRow],
    serial_required: &HashMap<Uuid, bool>,
    provided: &HashMap<i32, Vec<String>>,
    po_snapshot: &HashMap<Uuid, (Decimal, Decimal)>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 锁单头（并发重复过账在这里串行化）
    let locked = stockin_model::lock_stock_in(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if locked.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），不能过账：库存只加一次",
            locked.in_no,
            status_label(&locked.status),
            locked.status
        )));
    }

    // ② CAS 写状态（`WHERE status = 'DRAFT'`）
    let affected =
        stockin_model::apply_status(ex, id, STATUS_DRAFT, STATUS_POSTED, None, operator, now)
            .await
            .map_err(|error| write_error(error, "收货入库单过账改状态"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 已被过账（并发），本次未生效",
            header.in_no
        )));
    }

    // ③ 逐行：库存加法 + 流水（成本口径 = 不含税金额）
    for item in ordered {
        let cost = CostBasis::of(item.qty, item.amount);
        let applied = stockin_model::apply_cost(
            ex,
            item.material_id,
            header.warehouse_id,
            item.location_id,
            cost,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：库存加法"))?;
        // ③b 库位账（数量账）：与成本账**同一事务**同步加，否则违反不变式③
        //     （SUM(库位账) = 成本账 qty）。锁顺序：成本账已锁 → 这里只锁库位账一行，
        //     明细在调用方已按 material_id 升序排好，故两个收货单并发不会交叉等待。
        stockin_model::apply_location_qty(
            ex,
            item.material_id,
            header.warehouse_id,
            item.location_id,
            item.qty,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：库位账加法"))?;
        stockin_model::write_txn(
            ex,
            item.material_id,
            header.warehouse_id,
            item.line_no,
            cost,
            &applied,
            id,
            &header.in_no,
            operator,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：写库存流水"))?;

        // ④ 一物一码：落 / 生成序列号
        if serial_required
            .get(&item.material_id)
            .copied()
            .unwrap_or(false)
        {
            let serial_ids = provided.get(&item.line_no).cloned().unwrap_or_default();
            stockin_model::generate_serials(
                ex,
                &stockin_model::SerialBatch {
                    stock_in_id: id,
                    material_id: item.material_id,
                    warehouse_id: header.warehouse_id,
                    location_id: item.location_id,
                    qty: item.qty,
                    serial_ids: &serial_ids,
                    operator,
                    now,
                },
            )
            .await
            .map_err(|error| match error {
                DbError::UniqueViolation { constraint, .. } => ApiError::new(
                    409,
                    error_code::ERP_CODE_CONFLICT,
                    format!(
                        "序列号重复（唯一约束{}）：这个码已经入过库，不能重复入库",
                        constraint
                            .as_deref()
                            .map(|name| format!(" {name}"))
                            .unwrap_or_default()
                    ),
                ),
                other => write_error(other, "过账：生成序列号"),
            })?;
        }

        // ⑤ 回写采购明细已收数量（条件更新，0 行 = 超收）
        if let Some(source_item_id) = item.source_item_id {
            let affected = stockin_model::add_received_qty(ex, source_item_id, item.qty, now)
                .await
                .map_err(|error| write_error(error, "过账：回写采购已收数量"))?;
            if affected == 0 {
                let (po_qty, received) = po_snapshot
                    .get(&source_item_id)
                    .copied()
                    .unwrap_or_default();
                return Err(bad_request(format!(
                    "第 {} 行收货数量超过采购单未收数量：采购 {po_qty}、已收 {received}，\
                     本次再收 {} → 超出 {}（并发下别的收货单可能刚收走了）",
                    item.line_no,
                    item.qty,
                    received + item.qty - po_qty
                )));
            }
        }
    }

    // ⑥ 推进采购单状态（PARTIAL_RECEIVED / RECEIVED）
    if let Some(order_id) = header.source_order_id {
        stockin_model::sync_purchase_status(ex, order_id, now)
            .await
            .map_err(|error| write_error(error, "过账：推进采购单状态"))?;
    }

    // ⑦ 应付单 + 凭证（P2-C1，方案 A：入库即生成；**同一事务**，失败整单回滚）
    //    金额全部取自单头（header.total_*），不重算（硬红线 5）。
    //    顺序：先凭证（应付要引用 voucher_id）→ 再应付 → 最后回写收货单两列。
    let first_material = ordered.first().map(|item| item.material_id);
    let voucher_id = voucher::create_for_stock_in(ex, &locked, first_material, operator, now)
        .await
        .map_err(|error| write_error(error, "过账：生成凭证"))?;
    let payable_id = payable::create_for_stock_in(ex, &locked, Some(voucher_id), operator, now)
        .await
        .map_err(|error| write_error(error, "过账：生成应付单"))?;
    stockin_model::link_voucher_and_payable(ex, id, voucher_id, payable_id, now)
        .await
        .map_err(|error| write_error(error, "过账：回写凭证/应付引用"))?;
    tracing::info!(
        stock_in_id = %id,
        in_no = %locked.in_no,
        voucher_id = %voucher_id,
        payable_id = %payable_id,
        total_with_tax = %locked.total_with_tax,
        "收货过账：已生成应付单与凭证（借存货 + 借进项税 / 贷应付）"
    );
    Ok(())
}

/// 校验 PATCH body 里的序列号（`line_no → 号码`），并把数量与明细 qty 对齐（卡面 ⑩）。
fn request_serials(
    req: &StockInStatusReq,
    items: &[stockin_model::StockInItemRow],
) -> std::result::Result<HashMap<i32, Vec<String>>, ApiError> {
    let mut map: HashMap<i32, Vec<String>> = HashMap::new();
    if req.serials.is_empty() {
        return Ok(map);
    }
    let by_line: HashMap<i32, &stockin_model::StockInItemRow> =
        items.iter().map(|item| (item.line_no, item)).collect();
    for entry in &req.serials {
        let Some(item) = by_line.get(&entry.line_no) else {
            return Err(bad_request(format!(
                "serials 里的 line_no = {} 在本单明细里不存在（本单行号：{}）",
                entry.line_no,
                items
                    .iter()
                    .map(|it| it.line_no.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        let cleaned: Vec<String> = entry
            .serial_ids
            .iter()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .collect();
        let count = cleaned.len();
        let expected: i64 = item.qty.try_into().map_err(|_| {
            bad_request(format!(
                "第 {} 行 qty = {} 不是整数：一物一码物料按件入库",
                item.line_no, item.qty
            ))
        })?;
        if i64::try_from(count).unwrap_or(-1) != expected {
            return Err(bad_request(format!(
                "第 {} 行的序列号条数与数量不符：qty = {}（{expected} 件），serial_ids 给了 {count} 条",
                entry.line_no, item.qty
            )));
        }
        let mut seen = std::collections::HashSet::with_capacity(cleaned.len());
        if let Some(dup) = cleaned.iter().find(|serial_no| !seen.insert(*serial_no)) {
            return Err(bad_request(format!(
                "第 {} 行的序列号重复：`{dup}`",
                entry.line_no
            )));
        }
        if map.insert(entry.line_no, cleaned).is_some() {
            return Err(bad_request(format!(
                "serials 里 line_no = {} 出现了两次",
                entry.line_no
            )));
        }
    }
    Ok(map)
}

/// 收货入库单状态流转（`DRAFT→POSTED` 过账 / `DRAFT→CANCELLED` / `POSTED→CANCELLED`）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/status",
    method = "PATCH",
    permission = "business:erp:stock-in:status"
)]
#[nik::patch("/business/erp/stock-in/{id}/status")]
pub async fn stock_in_change_status(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    Json(req): Json<StockInStatusReq>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    change_stock_in_status(id, &req, operator_of(&claims)).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 删除 ====================

/// 删除核心逻辑：**只有 `DRAFT`** 能删（软删）。
pub async fn delete_stock_in(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if row.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "收货入库单当前是「{}」（{}），只有草稿（DRAFT）能删：\
             已过账的单据是库存与成本链的事实，只能作废",
            status_label(&row.status),
            row.status
        )));
    }
    let affected = stockin_model::soft_delete(db(), id, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "软删收货入库单"))?;
    if affected == 0 {
        return Err(not_found("收货入库单"));
    }
    Ok(())
}

/// 删除收货入库单（软删；仅 DRAFT）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}",
    method = "DELETE",
    permission = "business:erp:stock-in:delete"
)]
#[nik::delete("/business/erp/stock-in/{id}")]
pub async fn stock_in_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    delete_stock_in(id, operator_of(&claims)).await?;
    Ok(Res::ok(()))
}

// ==================== ⑤ 列表 / 详情 ====================

/// 收货入库单分页。
#[nik::permission(
    path = "/business/erp/stock-in/page",
    method = "GET",
    permission = "business:erp:stock-in:list"
)]
#[nik::get("/business/erp/stock-in/page")]
pub async fn stock_in_page(
    Query(query): Query<StockInPageQuery>,
) -> ResResult<PageData<Vec<StockInRes>>> {
    let page = query.page;
    let page_size = query.page_size;
    let (rows, total) = stockin_model::stock_in_page(&query.to_filter())
        .await
        .map_err(|error| utils::db_error(error, "分页查询收货入库单"))?;
    let list = rows.iter().map(StockInRes::from).collect();
    Ok(page_body(list, total, page, page_size))
}

/// 收货入库单详情。
#[nik::permission(
    path = "/business/erp/stock-in/{id}",
    method = "GET",
    permission = "business:erp:stock-in:detail"
)]
#[nik::get("/business/erp/stock-in/{id}")]
pub async fn stock_in_detail(Path(id): Path<String>) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 分页响应包装（与 `handler::page_body` 同口径；那边的函数是私有的）。
fn page_body<T: Serialize>(
    list: Vec<T>,
    total: i64,
    page: u64,
    page_size: u64,
) -> Res<PageData<Vec<T>>> {
    let (page, page_size) = model::normalize_page(page, page_size);
    Res::page(list, u64::try_from(total).unwrap_or(0), page, page_size)
}

// ==================== 单测 ====================
//
// 纯逻辑（金额 / 状态机 / 序列号条数校验）在 `stockin_dto` 与 `stockin_model` 的单测里；
// 这里放**真库用例**（全链路：建单 → 过账 → 查库存 / 流水 / 序列号 / 采购回写）。
//
// 需要真库：`ERP_DB_URL` 指向「已应用 V0145 的隔离库」，缺省则全部 `SKIP`：
//
// ```bash
// su - postgres -c 'createdb -O cloudpivot cloudpivot_p2b1'
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p2b1'
// cargo test --lib business::erp::stockin
// ```
//
// 夹具固定 UUID（`b2b1…` 段）+ `P2B1-` 前缀；每次 `setup()` 先按供应商清掉上一轮的单、
// 流水与库存行，所以重复跑不积累（序列号按 `source_id` 删）。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::stockin_dto::{StockInItemReq, StockInLineSerials};
    use std::str::FromStr;

    const UNIT: &str = "b2b10001-0000-4000-8000-000000000001";
    const SUPPLIER: &str = "b2b10001-0000-4000-8000-000000000002";
    const WAREHOUSE: &str = "b2b10001-0000-4000-8000-000000000003";
    /// 非一物一码物料（螺丝）。
    const MATERIAL_A: &str = "b2b10001-0000-4000-8000-000000000011";
    /// 非一物一码物料（垫片）。
    const MATERIAL_B: &str = "b2b10001-0000-4000-8000-000000000012";
    /// 一物一码物料（电机）。
    const MATERIAL_SN: &str = "b2b10001-0000-4000-8000-000000000013";
    /// 库位字典（`biz_location.id`，P6：库位账行的 `location_id` 列指这个）。
    const LOC_DICT_A: &str = "b2b10003-0000-4000-8000-000000000001";
    /// 库位字典（第二个库位）。
    const LOC_DICT_B: &str = "b2b10003-0000-4000-8000-000000000002";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('b2b10001-0000-4000-8000-000000000001', 'P2B1-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
         VALUES ('b2b10001-0000-4000-8000-000000000002', 'P2B1-SUP', 'P2B1 测试供应商', 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status)
         VALUES ('b2b10001-0000-4000-8000-000000000003', 'P2B1-WH', 'P2B1 收货仓',
                 '/b2b10001-0000-4000-8000-000000000003/', 1, 1, 1)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
         VALUES ('b2b10001-0000-4000-8000-000000000011', 'P2B1-M1', '螺丝 M6x20',
                 'b2b10001-0000-4000-8000-000000000001', 1, false),
                ('b2b10001-0000-4000-8000-000000000012', 'P2B1-M2', '垫片 8mm',
                 'b2b10001-0000-4000-8000-000000000001', 1, false),
                ('b2b10001-0000-4000-8000-000000000013', 'P2B1-M3', '伺服电机',
                 'b2b10001-0000-4000-8000-000000000001', 1, true)
         ON CONFLICT DO NOTHING",
        // P6：两个库位字典（库位账行的 location_id 列指它；无外键，弱引用）
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                                   is_default, capacity, status, sort_order)
         VALUES ('b2b10003-0000-4000-8000-000000000001', 'P2B1-L01', 'P2B1 A 区一号',
                 'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 1),
                ('b2b10003-0000-4000-8000-000000000002', 'P2B1-L02', 'P2B1 A 区二号',
                 'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 2)
         ON CONFLICT DO NOTHING",
    ];

    /// 真库用例必须串行（共用全局 `db()`、同一批夹具与同一 `biz_seq(IN)`）。
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

    /// 清掉本模块造的所有夹具（含 P9 新增的凭证/应付清理）。
    ///
    /// 顺序有讲究：序列号 → 流水 → 库存 → 库位账 → 凭证/应付 → 收货单 → 采购单。
    /// 凭证/应付是**弱引用**（无外键），必须在删收货单之前按 `source_id` 清掉。
    async fn cleanup_fixtures() {
        let cleanups = [
            format!(
                "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_inventory WHERE material_id IN \
             ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid, '{MATERIAL_SN}'::uuid) \
             AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            // P6：库位账必须排在 biz_inventory 之后删（`fk_biz_inv_location_id` 指本表）
            format!(
                "DELETE FROM biz_inventory_location WHERE material_id IN \
             ('{MATERIAL_A}'::uuid, '{MATERIAL_B}'::uuid, '{MATERIAL_SN}'::uuid) \
             AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            // P9：过账即生成凭证/应付，不清理会让「按 period 全局计数」的其它模块用例漂数
            format!(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_id IN \
              (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_voucher WHERE source_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_payable WHERE source_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid)"
            ),
            format!("DELETE FROM biz_stock_in WHERE supplier_id = '{SUPPLIER}'::uuid"),
            format!("DELETE FROM biz_purchase_order WHERE supplier_id = '{SUPPLIER}'::uuid"),
        ];
        for sql in cleanups {
            if let Err(err) = db().execute_with_params(&sql, vec![]).await {
                panic!("清理数据失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 拿锁 + 连库 + 清上一轮 + 建夹具。
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup_fixtures().await;
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

    /// 一行收货明细的请求态。
    fn line(material: &str, qty: &str, price: &str, rate: &str) -> StockInItemReq {
        StockInItemReq {
            purchase_order_item_id: None,
            material_id: Some(material.to_string()),
            qty: Some(dec(qty)),
            price_excl: Some(dec(price)),
            tax_rate: Some(dec(rate)),
            location_id: None,
            serial_ids: Vec::new(),
            remark: None,
        }
    }

    /// 同 [`line`]，但带上库位（`biz_inventory_location.id`，P6）。
    fn line_at(
        material: &str,
        qty: &str,
        price: &str,
        rate: &str,
        location: &str,
    ) -> StockInItemReq {
        StockInItemReq {
            location_id: Some(location.to_string()),
            ..line(material, qty, price, rate)
        }
    }

    fn req(lines: Vec<StockInItemReq>) -> CreateStockInReq {
        CreateStockInReq {
            source_order_id: None,
            supplier_id: Some(SUPPLIER.to_string()),
            warehouse_id: Some(WAREHOUSE.to_string()),
            in_date: None,
            invoice_received: None,
            remark: None,
            items: lines,
        }
    }

    fn post_req() -> StockInStatusReq {
        StockInStatusReq {
            status: Some("POSTED".to_string()),
            reason: None,
            serials: Vec::new(),
        }
    }

    /// 库存三值。
    async fn inventory_of(material: &str) -> (Decimal, Decimal, Decimal) {
        let rows = db()
            .find_with_params(
                "SELECT qty, amount, avg_cost FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
                vec![
                    Value::from(&Uuid::parse_str(material).unwrap()),
                    Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                ],
            )
            .await
            .unwrap_or_else(|err| panic!("查库存失败：{err}"));
        match rows.first() {
            None => (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
            Some(row) => (
                row.get_typed("qty").unwrap(),
                row.get_typed("amount").unwrap(),
                row.get_typed("avg_cost").unwrap(),
            ),
        }
    }

    /// 建 + 过账一步到位，返回收货单 id。
    async fn post_new(lines: Vec<StockInItemReq>) -> Uuid {
        let id = create_stock_in(&req(lines), None).await.unwrap();
        change_stock_in_status(id, &post_req(), None)
            .await
            .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
        id
    }

    /// ① 首次入库：库存从无到有，qty / amount / avg_cost 正确。
    #[tokio::test]
    async fn first_receipt_creates_inventory_row() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
        );
        let id = post_new(vec![line(MATERIAL_A, "100", "2", "13")]).await;

        let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("100"));
        assert_eq!(
            amount,
            dec("200"),
            "入库金额必须是不含税 200，不是价税合计 226"
        );
        assert_eq!(avg, dec("2.000000"));

        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.header.status, STATUS_POSTED);
        assert!(detail.header.posted_at.is_some(), "过账必须写 posted_at");
        assert_eq!(detail.header.total_amount, dec("200"));
        assert_eq!(detail.header.total_tax, dec("26"));
        assert_eq!(detail.header.total_with_tax, dec("226"));
        assert!(detail.summary.contains("不含税"), "{}", detail.summary);
    }

    /// ② 二次入库：移动加权重算（PRD §3.2.5 算例 1：100@2 + 100@3 → 200 / 500 / 2.5）。
    #[tokio::test]
    async fn second_receipt_moves_weighted_average() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
        post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;
        let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("200"));
        assert_eq!(amount, dec("500"));
        assert_eq!(avg, dec("2.500000"), "500 / 200 = 2.5（PRD §3.2.5 算例 1）");
    }

    /// ②b 除不尽的尾差：10@1.11 + 3@3.33 → 13 / 21.09 / 1.622308（PRD §3.2.5 算例 2）。
    #[tokio::test]
    async fn rounding_dust_keeps_amount_authoritative() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        post_new(vec![line(MATERIAL_A, "10", "1.11", "0")]).await;
        post_new(vec![line(MATERIAL_A, "3", "3.33", "0")]).await;
        let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("13"));
        assert_eq!(amount, dec("21.09"), "金额是加法得出的权威值，不修尾差");
        assert_eq!(
            avg,
            dec("1.622308"),
            "21.09 / 13 = 1.62230769… → 6 位四舍五入"
        );
    }

    /// ③ 一物一码物料：生成对应数量的序列号，`warehouse_id` 正确（卡面 ③）。
    #[tokio::test]
    async fn serial_material_generates_serials_in_receipt_warehouse() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_SN, "5", "40", "13")]).await;
        assert_eq!(stockin_model::serial_count_of(id).await.unwrap(), 5);
        let rows = db()
            .find_with_params(
                "SELECT serial_no, warehouse_id, status, source_type, source_id \
                   FROM biz_material_serial WHERE source_id = ?::uuid ORDER BY serial_no",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 5);
        for row in &rows {
            let serial_no: String = row.get_typed("serial_no").unwrap();
            assert!(serial_no.starts_with("SN-"), "{serial_no}");
            let warehouse: Uuid = row.get_typed("warehouse_id").unwrap();
            assert_eq!(warehouse.to_string(), WAREHOUSE, "序列号必须落在收货仓");
            let status: String = row.get_typed("status").unwrap();
            assert_eq!(status, "IN_STOCK");
            let source_type: String = row.get_typed("source_type").unwrap();
            assert_eq!(source_type, "PURCHASE");
        }
        // 账面数量与序列号件数一致（不变式①）
        let (qty, _, _) = inventory_of(MATERIAL_SN).await;
        assert_eq!(qty, dec("5"));
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.serial_count, 5);
    }

    /// ④ 非一物一码：不生成序列号。
    #[tokio::test]
    async fn non_serial_material_generates_no_serials() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "7", "1.5", "0")]).await;
        assert_eq!(stockin_model::serial_count_of(id).await.unwrap(), 0);
        let total: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_material_serial WHERE material_id = ?::uuid",
                vec![Value::from(&Uuid::parse_str(MATERIAL_A).unwrap())],
            )
            .await
            .unwrap();
        assert_eq!(total, 0);
    }

    /// ⑤ 流水：`txn_type=IN`、`biz_type=PURCHASE_IN`、`before_*` 快照正确（第二次入库才有非零快照）。
    #[tokio::test]
    async fn txn_snapshot_is_before_and_after() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let first = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
        let second = post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;

        let rows = db()
            .find_with_params(
                "SELECT txn_type, biz_type, doc_type, doc_no, line_no, qty, unit_cost, amount, \
                        before_qty, before_amount, before_cost, after_qty, after_amount, after_cost, \
                        period_code \
                   FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id = ?::uuid",
                vec![Value::from(&second)],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "一张收货单的一行 = 一条流水");
        let row = &rows[0];
        assert_eq!(row.get_typed::<String>("txn_type").unwrap(), "IN");
        assert_eq!(row.get_typed::<String>("biz_type").unwrap(), "PURCHASE_IN");
        assert_eq!(row.get_typed::<String>("doc_type").unwrap(), "STOCK_IN");
        assert_eq!(row.get_typed::<i32>("line_no").unwrap(), 1);
        assert_eq!(row.get_typed::<Decimal>("qty").unwrap(), dec("100"));
        assert_eq!(
            row.get_typed::<Decimal>("unit_cost").unwrap(),
            dec("3.000000")
        );
        assert_eq!(row.get_typed::<Decimal>("amount").unwrap(), dec("300"));
        assert_eq!(
            row.get_typed::<Decimal>("before_qty").unwrap(),
            dec("100"),
            "第二次入库的 before_qty 必须是第一张单留下的 100"
        );
        assert_eq!(
            row.get_typed::<Decimal>("before_amount").unwrap(),
            dec("200")
        );
        assert_eq!(
            row.get_typed::<Decimal>("before_cost").unwrap(),
            dec("2.000000")
        );
        assert_eq!(row.get_typed::<Decimal>("after_qty").unwrap(), dec("200"));
        assert_eq!(
            row.get_typed::<Decimal>("after_amount").unwrap(),
            dec("500")
        );
        assert_eq!(
            row.get_typed::<Decimal>("after_cost").unwrap(),
            dec("2.500000")
        );
        let period: String = row.get_typed("period_code").unwrap();
        assert_eq!(period.len(), 7, "period_code 是 yyyy-MM：{period}");
        // 第一张单的 before 全是 0
        let first_rows = db()
            .find_with_params(
                "SELECT before_qty, before_amount FROM biz_inventory_txn \
                  WHERE doc_id = ?::uuid",
                vec![Value::from(&first)],
            )
            .await
            .unwrap();
        assert_eq!(
            first_rows[0].get_typed::<Decimal>("before_qty").unwrap(),
            dec("0")
        );
    }

    /// ⑥ 并发：两个收货单同时收同一物料（两行顺序相反）→ 不丢数、不死锁。
    ///
    /// 两行顺序相反是关键：若不按 `material_id` 升序加锁，A 先锁 M1 再锁 M2、B 先锁 M2 再锁 M1
    /// 就是教科书死锁。这里断言（a）两单都过账成功（无死锁）；（b）库存 = 两单之和（不丢数）。
    #[tokio::test]
    async fn concurrent_receipts_do_not_lose_or_deadlock() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let a = create_stock_in(
            &req(vec![
                line(MATERIAL_A, "10", "2", "0"),
                line(MATERIAL_B, "10", "5", "0"),
            ]),
            None,
        )
        .await
        .unwrap();
        let b = create_stock_in(
            &req(vec![
                line(MATERIAL_B, "20", "5", "0"),
                line(MATERIAL_A, "20", "2", "0"),
            ]),
            None,
        )
        .await
        .unwrap();

        let post_request = post_req();
        let (ra, rb) = tokio::join!(
            change_stock_in_status(a, &post_request, None),
            change_stock_in_status(b, &post_request, None)
        );
        ra.unwrap_or_else(|err| panic!("并发过账 A 失败（死锁？）：{} {}", err.code, err.msg));
        rb.unwrap_or_else(|err| panic!("并发过账 B 失败（死锁？）：{} {}", err.code, err.msg));

        let (qty_a, amount_a, avg_a) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty_a, dec("30"), "10 + 20，一笔都不能丢");
        assert_eq!(amount_a, dec("60"));
        assert_eq!(avg_a, dec("2.000000"));
        let (qty_b, amount_b, _) = inventory_of(MATERIAL_B).await;
        assert_eq!(qty_b, dec("30"));
        assert_eq!(amount_b, dec("150"), "10×5 + 20×5");
        // 两单各一行流水，共 4 条
        let txns: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_inventory_txn WHERE doc_id IN (?::uuid, ?::uuid)",
                vec![Value::from(&a), Value::from(&b)],
            )
            .await
            .unwrap();
        assert_eq!(txns, 4);
    }

    /// ⑦ 过账幂等：重复 POSTED → 409，且库存只加一次。
    #[tokio::test]
    async fn repeated_post_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("已过账"), "{}", err.msg);
        let (qty, _, _) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("10"), "库存只加一次");
        let txns: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(txns, 1, "流水也只写一条");
    }

    /// ⑧ DRAFT 改单：能改，明细整单重建、汇总重算。
    #[tokio::test]
    async fn update_draft_replaces_lines() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_stock_in(&req(vec![line(MATERIAL_A, "100", "2", "13")]), None)
            .await
            .unwrap();
        update_stock_in(
            id,
            &req(vec![
                line(MATERIAL_A, "10", "2", "13"),
                line(MATERIAL_B, "5", "3", "6"),
            ]),
            None,
        )
        .await
        .unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items.len(), 2, "旧行必须被替换掉");
        assert_eq!(detail.items[0].line_no, 1);
        assert_eq!(detail.items[1].line_no, 2);
        assert_eq!(detail.header.total_amount, dec("35"), "10×2 + 5×3");
        assert_eq!(detail.header.total_tax, dec("3.50"), "2.60 + 0.90");
        assert_eq!(detail.header.total_qty, dec("15"));
    }

    /// ⑨ POSTED 改单：409。
    #[tokio::test]
    async fn update_posted_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
        let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "1", "2", "0")]), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("只有草稿"), "{}", err.msg);
    }

    /// ⑩ 一物一码数量与 `serial_ids` 条数不符 → 400（建单就拦住）。
    #[tokio::test]
    async fn serial_count_mismatch_is_bad_request() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let mut bad = line(MATERIAL_SN, "3", "40", "13");
        bad.serial_ids = vec!["SN-A".to_string(), "SN-B".to_string()];
        let err = create_stock_in(&req(vec![bad]), None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("序列号条数与数量不符"), "{}", err.msg);

        // 数量对得上就放行；自定义码在**过账**时提交（明细表没有存序列号的列）
        let mut good = line(MATERIAL_SN, "2", "40", "0");
        good.serial_ids = vec!["SN-P2B1-001".to_string(), "SN-P2B1-002".to_string()];
        let id = create_stock_in(&req(vec![good]), None).await.unwrap();
        let mut post = post_req();
        post.serials = vec![StockInLineSerials {
            line_no: 1,
            serial_ids: vec!["SN-P2B1-001".to_string(), "SN-P2B1-002".to_string()],
        }];
        change_stock_in_status(id, &post, None).await.unwrap();
        let rows = db()
            .find_with_params(
                "SELECT serial_no FROM biz_material_serial WHERE source_id = ?::uuid ORDER BY serial_no",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        let numbers: Vec<String> = rows
            .iter()
            .map(|row| row.get_typed("serial_no").unwrap())
            .collect();
        assert_eq!(
            numbers,
            vec!["SN-P2B1-001", "SN-P2B1-002"],
            "过账时提交的码要原样落库（不改写供应商贴的码）"
        );

        // 过账时条数不符也拦住
        let mut bad_post = post_req();
        bad_post.serials = vec![StockInLineSerials {
            line_no: 1,
            serial_ids: vec!["ONLY-ONE".to_string()],
        }];
        let another = create_stock_in(&req(vec![line(MATERIAL_SN, "2", "40", "0")]), None)
            .await
            .unwrap();
        let err = change_stock_in_status(another, &bad_post, None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("序列号条数与数量不符"), "{}", err.msg);
    }

    /// ⑩b 非一物一码物料给了 `serial_ids` → 400（`66007`），不静默丢。
    #[tokio::test]
    async fn serial_ids_on_non_serial_material_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let mut bad = line(MATERIAL_A, "2", "1", "0");
        bad.serial_ids = vec!["SN-X".to_string(), "SN-Y".to_string()];
        let err = create_stock_in(&req(vec![bad]), None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_SERIAL_NOT_REQUIRED);
    }

    /// ⑪ 收货数量超过采购单未收数量 → 400（带 `source_order_id`）。
    #[tokio::test]
    async fn over_receipt_against_purchase_order_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("30", "10").await;
        // 剩余 20，要收 25 → 建单即 400
        let mut over = line(MATERIAL_A, "25", "2", "0");
        over.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![over]);
        request.source_order_id = Some(order_id.to_string());
        let err = create_stock_in(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("超过采购单未收数量"), "{}", err.msg);

        // 收 20（刚好剩余）→ 成功，采购单变 RECEIVED 且 received_qty = 30
        let mut ok = line(MATERIAL_A, "20", "2", "0");
        ok.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![ok]);
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        change_stock_in_status(id, &post_req(), None).await.unwrap();

        let row = db()
            .find_with_params(
                "SELECT received_qty FROM biz_purchase_order_item WHERE id = ?::uuid",
                vec![Value::from(&item_id)],
            )
            .await
            .unwrap();
        assert_eq!(
            row[0].get_typed::<Decimal>("received_qty").unwrap(),
            dec("30"),
            "过账必须回写采购明细的已收数量"
        );
        let status: String = db()
            .scalar_with_params(
                "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
                vec![Value::from(&order_id)],
            )
            .await
            .unwrap();
        assert_eq!(status, "RECEIVED", "收满自动推进采购单状态");
    }

    /// ⑪b 自动带出：只给 `source_order_id`（items 空），按未收数量成单。
    #[tokio::test]
    async fn auto_pull_lines_from_purchase_order() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("30", "10").await;
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items.len(), 1);
        assert_eq!(detail.items[0].qty, dec("20"), "自动带出的应该是未收的 20");
        assert_eq!(
            detail.items[0].source_item_id.as_deref(),
            Some(item_id.to_string().as_str())
        );

        change_stock_in_status(id, &post_req(), None).await.unwrap();
        let status: String = db()
            .scalar_with_params(
                "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
                vec![Value::from(&order_id)],
            )
            .await
            .unwrap();
        assert_eq!(status, "RECEIVED");
    }

    /// ⑫ 成本口径：入库成本用不含税金额，不是价税合计（13% 时差 13%）。
    #[tokio::test]
    async fn cost_uses_net_amount_not_gross() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "50", "10", "13")]).await;
        // 不含税 500、税 65、价税合计 565
        let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("50"));
        assert_eq!(amount, dec("500"), "存货成本 = 不含税 500（不是 565）");
        assert_ne!(amount, dec("565"));
        assert_eq!(avg, dec("10.000000"));
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.items[0].amount, dec("500"));
        assert_eq!(detail.items[0].tax_amount, dec("65"));
        assert_eq!(detail.items[0].with_tax_amount, dec("565"));
        assert_eq!(
            detail.items[0].cost_price,
            dec("10.000000"),
            "cost_price 落不含税单价"
        );
        let txn_amount: Decimal = db()
            .scalar_with_params(
                "SELECT amount FROM biz_inventory_txn WHERE doc_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(txn_amount, dec("500"), "流水金额也只用不含税");
    }

    /// ⑬ 作废：DRAFT → CANCELLED 不写库存；POSTED → CANCELLED 需原因，**库存冲回**（P9 改口径）。
    #[tokio::test]
    async fn cancel_paths() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let draft = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
            .await
            .unwrap();
        change_stock_in_status(
            draft,
            &StockInStatusReq {
                status: Some("CANCELLED".to_string()),
                reason: Some("供应商没货".to_string()),
                serials: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            inventory_of(MATERIAL_A).await.0,
            Decimal::ZERO,
            "草稿作废不该产生库存"
        );
        let detail = detail_of(draft).await.unwrap();
        assert_eq!(detail.header.status, STATUS_CANCELLED);
        assert!(detail.header.cancelled_at.is_some());
        assert_eq!(detail.header.cancel_reason.as_deref(), Some("供应商没货"));

        // POSTED → CANCELLED：必须给原因
        let posted = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        let err = change_stock_in_status(
            posted,
            &StockInStatusReq {
                status: Some("CANCELLED".to_string()),
                reason: None,
                serials: Vec::new(),
            },
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        change_stock_in_status(
            posted,
            &StockInStatusReq {
                status: Some("CANCELLED".to_string()),
                reason: Some("收错仓了".to_string()),
                serials: Vec::new(),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            inventory_of(MATERIAL_A).await.0,
            dec("0"),
            "P9：POSTED→CANCELLED 必须把库存退回去（旧口径「只改状态」是 E4 的根因）"
        );
        assert_eq!(inventory_of(MATERIAL_A).await.1, dec("0"));
    }

    /// ⑭ 删除：仅 DRAFT 能软删；POSTED 删除 → 409。
    #[tokio::test]
    async fn delete_only_draft() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let draft = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
            .await
            .unwrap();
        delete_stock_in(draft, None).await.unwrap();
        assert!(detail_of(draft).await.is_err(), "软删后详情 404");

        let posted = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        let err = delete_stock_in(posted, None).await.unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
    }

    /// ⑮ 列表：按状态 / 供应商筛，分页字段与总数正确。
    #[tokio::test]
    async fn page_filters_by_status() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        create_stock_in(&req(vec![line(MATERIAL_B, "5", "2", "0")]), None)
            .await
            .unwrap();
        let filter = stockin_model::StockInFilter {
            status: Some("POSTED".to_string()),
            supplier_id: Some(SUPPLIER.to_string()),
            page: 1,
            page_size: 50,
            ..Default::default()
        };
        let (rows, total) = stockin_model::stock_in_page(&filter).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, STATUS_POSTED);
        assert!(rows[0].in_no.starts_with("IN-"), "{}", rows[0].in_no);
        assert_eq!(rows[0].currency_code, "CNY");
    }

    /// 造一张采购单（`qty` 件的 1 行，已收 `received`）并置为 `SUBMITTED`。
    async fn seed_purchase_order(qty: &str, received: &str) -> (Uuid, Uuid) {
        let order_id = Uuid::now_v7();
        let item_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_purchase_order \
                 (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty, \
                  total_amount, total_tax, total_with_tax) \
             VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?::uuid, 'SUBMITTED', \
                 NULLIF(?::text,'')::numeric, 0, 0, 0)",
            vec![
                Value::from(&order_id),
                Value::Text(format!("B1-{}", &order_id.to_string()[..8])),
                Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .unwrap();
        db().execute_with_params(
            "INSERT INTO biz_purchase_order_item \
                 (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
                  amount, tax_amount, with_tax_amount, received_qty) \
             VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, 0, 0, NULLIF(?::text,'')::numeric, 0, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric)",
            vec![
                Value::from(&item_id),
                Value::from(&order_id),
                Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
                Value::from(&Uuid::parse_str(UNIT).unwrap()),
                Value::Text(qty.to_string()),
                Value::Text("2".to_string()),
                Value::Text(qty.to_string()),
                Value::Text(qty.to_string()),
                Value::Text(received.to_string()),
            ],
        )
        .await
        .unwrap();
        (order_id, item_id)
    }

    // ==================== P6：库位账（biz_inventory_location）====================

    /// 造 / 取一个库位账行（`(物料, 仓库, 库位字典)` 三元组），返回**真实行 id**。
    ///
    /// 不写死主键：`uk_biz_inv_loc(material, warehouse, location_id)` 决定唯一性，
    /// 上一轮留下的行会保留原主键（`DO NOTHING` 就取不到 id，所以用「自更新」形态回读）。
    async fn seed_location_row(material: &str, location_dict: &str) -> Uuid {
        let rows = db()
            .find_with_params(
                &format!(
                    "INSERT INTO biz_inventory_location AS il \
                         (id, material_id, warehouse_id, location_id, qty, locked_qty) \
                     VALUES (gen_random_uuid(), '{material}'::uuid, '{WAREHOUSE}'::uuid, \
                             '{location_dict}'::uuid, 0, 0) \
                     ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE \
                         SET qty = il.qty \
                     RETURNING il.id"
                ),
                vec![],
            )
            .await
            .unwrap_or_else(|err| panic!("造库位账行失败：{err}"));
        rows.first()
            .unwrap_or_else(|| panic!("造库位账行没有返回行"))
            .get_typed("id")
            .unwrap_or_else(|err| panic!("取库位账行 id 失败：{err}"))
    }

    /// 库位账某一行的 `(qty, locked_qty)`（行不存在 → `None`）。
    async fn location_row_of(id: &Uuid) -> Option<(Decimal, Decimal)> {
        let rows = db()
            .find_with_params(
                "SELECT qty, locked_qty FROM biz_inventory_location WHERE id = ?::uuid",
                vec![Value::from(id)],
            )
            .await
            .unwrap_or_else(|err| panic!("查库位账行失败：{err}"));
        rows.first().map(|row| {
            (
                row.get_typed("qty").unwrap(),
                row.get_typed("locked_qty").unwrap(),
            )
        })
    }

    /// 库位账某一行的数量（行不存在 → `None`）。
    async fn location_qty_of(id: &Uuid) -> Option<Decimal> {
        location_row_of(id).await.map(|(qty, _)| qty)
    }

    /// 库位账「未分配库位桶」（`location_id IS NULL`）的数量（行不存在 → `None`）。
    async fn null_bucket_qty(material: &str) -> Option<Decimal> {
        let rows = db()
            .find_with_params(
                &format!(
                    "SELECT qty FROM biz_inventory_location \
                      WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid \
                        AND location_id IS NULL"
                ),
                vec![],
            )
            .await
            .unwrap_or_else(|err| panic!("查 NULL 桶失败：{err}"));
        rows.first().map(|row| row.get_typed("qty").unwrap())
    }

    /// 库位账：某物料在该仓的**行数**。
    async fn location_row_count(material: &str) -> i64 {
        db().scalar_with_params(
            &format!(
                "SELECT COALESCE(COUNT(*), 0) FROM biz_inventory_location \
                  WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("统计库位账行数失败：{err}"))
    }

    /// 库位账：某物料在该仓的 qty **合计**（`SUM`，含 NULL 桶）。
    async fn location_qty_sum(material: &str) -> Decimal {
        db().scalar_with_params(
            &format!(
                "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location \
                  WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("汇总库位账失败：{err}"))
    }

    /// 成本账：某物料在该仓的 qty **合计**（`SUM`，含 NULL 汇总桶）。
    ///
    /// V0139 之后 `biz_inventory` 也是 `(物料, 仓库, 库位)` 三列唯一，所以「该物料该仓的成本账数量」
    /// 在**同一口径**下就是按库位求和 —— 与库位账的 `SUM(qty)` 可比。
    async fn cost_qty_sum(material: &str) -> Decimal {
        db().scalar_with_params(
            &format!(
                "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory \
                  WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("汇总成本账失败：{err}"))
    }

    /// ⑥（P6）收货**不带** `location_id` → 库位账写 `location_id IS NULL` 那一行。
    #[tokio::test]
    async fn receipt_without_location_writes_null_bucket() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        assert_eq!(
            null_bucket_qty(MATERIAL_A).await,
            None,
            "开局不能有 NULL 桶行"
        );
        post_new(vec![line(MATERIAL_A, "200", "2", "0")]).await;

        assert_eq!(
            null_bucket_qty(MATERIAL_A).await,
            Some(dec("200")),
            "不带库位的收货必须落进 (物料, 仓库, NULL) 桶"
        );
        assert_eq!(location_row_count(MATERIAL_A).await, 1, "只该有一行");
        let rows = db()
            .find_with_params(
                &format!(
                    "SELECT id FROM biz_inventory_location \
                      WHERE material_id = '{MATERIAL_A}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
                ),
                vec![],
            )
            .await
            .unwrap();
        let row_id: Uuid = rows[0].get_typed("id").unwrap();
        let (_, locked) = location_row_of(&row_id).await.expect("库位账行必须存在");
        assert_eq!(locked, Decimal::ZERO, "收货不占锁定量，locked_qty 初始 0");
    }

    /// ⑦（P6）收货**带** `location_id` → 库位账加到那一行（不是新起一行）。
    #[tokio::test]
    async fn receipt_with_location_updates_that_row() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
        assert_eq!(location_qty_of(&row).await, Some(dec("0")));

        post_new(vec![line_at(MATERIAL_A, "120", "2", "0", &row.to_string())]).await;

        assert_eq!(
            location_qty_of(&row).await,
            Some(dec("120")),
            "库位账必须加在指定的那一行上"
        );
        assert_eq!(location_row_count(MATERIAL_A).await, 1, "不能多起一行");
        assert_eq!(
            null_bucket_qty(MATERIAL_A).await,
            None,
            "有库位时不该动 NULL 桶"
        );
        // 成本账同一维度：biz_inventory 的 location_id 也指这一行
        let cost_loc: Option<Uuid> = db()
            .find_with_params(
                &format!(
                    "SELECT location_id FROM biz_inventory \
                      WHERE material_id = '{MATERIAL_A}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
                ),
                vec![],
            )
            .await
            .unwrap()
            .first()
            .and_then(|row| row.get_typed("location_id").unwrap());
        assert_eq!(cost_loc, Some(row), "成本账与库位账必须指向同一行");
    }

    /// ⑧（P6）同一物料同一库位多次收货 → **累加**，不新建行（唯一键生效）。
    #[tokio::test]
    async fn same_location_receipts_accumulate_one_row() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
        post_new(vec![line_at(MATERIAL_A, "70", "2", "0", &row.to_string())]).await;
        post_new(vec![line_at(MATERIAL_A, "30", "2", "0", &row.to_string())]).await;

        assert_eq!(location_qty_of(&row).await, Some(dec("100")));
        assert_eq!(
            location_row_count(MATERIAL_A).await,
            1,
            "uk_biz_inv_loc(material, warehouse, location) 生效：只有一行"
        );
    }

    /// ⑨（P6）不同库位分别收 → 两行独立（互不串数）。
    #[tokio::test]
    async fn different_locations_stay_separate() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let a = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
        let b = seed_location_row(MATERIAL_A, LOC_DICT_B).await;
        assert_ne!(a, b);

        // 一张单里两行分别进两个库位
        post_new(vec![
            line_at(MATERIAL_A, "40", "2", "0", &a.to_string()),
            line_at(MATERIAL_A, "60", "2", "0", &b.to_string()),
        ])
        .await;

        assert_eq!(location_qty_of(&a).await, Some(dec("40")));
        assert_eq!(location_qty_of(&b).await, Some(dec("60")));
        assert_eq!(location_row_count(MATERIAL_A).await, 2);
    }

    /// ⑩（P6）不变式③：`SUM(库位账 qty) = 成本账 qty`（多库位 + 多张单混合）。
    #[tokio::test]
    async fn location_sum_equals_cost_book() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let a = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
        let b = seed_location_row(MATERIAL_A, LOC_DICT_B).await;
        post_new(vec![
            line_at(MATERIAL_A, "120", "2", "0", &a.to_string()),
            line_at(MATERIAL_A, "80", "3", "0", &b.to_string()),
        ])
        .await;
        post_new(vec![line(MATERIAL_A, "200", "4", "0")]).await; // NULL 桶
        post_new(vec![line_at(MATERIAL_A, "50", "5", "0", &a.to_string())]).await;

        let loc = location_qty_sum(MATERIAL_A).await;
        let cost = cost_qty_sum(MATERIAL_A).await;
        assert_eq!(loc, dec("450"), "120 + 80 + 200 + 50");
        assert_eq!(
            loc, cost,
            "不变式③：SUM(库位账) 必须等于成本账 qty（{loc} vs {cost}）"
        );
        // 成本金额仍按移动加权走（120×2 + 80×3 + 200×4 + 50×5 = 1490）
        let (_, amount, _) = inventory_of(MATERIAL_A).await;
        // NULL 桶那一行的金额 = 这一桶自己的加法（200×4 = 800）
        assert_eq!(amount, dec("800"), "NULL 桶的成本账金额");
    }

    /// ⑪（P6）事务原子性：库位账写入失败 → 成本账**一起回滚**。
    ///
    /// 造法：明细的 `location_id` 指向**别的物料**的库位账行。建单只校验「行存在 + 仓库一致」
    /// （P2-B1 的既有口径），过账时 [`stockin_model::apply_location_qty`] 才会发现物料不一致 →
    /// 这一步在 `apply_cost` 之后报错，正是「成本账已写、库位账写不进」的那一瞬。
    #[tokio::test]
    async fn location_failure_rolls_back_cost_account() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 别的物料的库位账行（同一个仓，所以建单校验能过）
        let other = seed_location_row(MATERIAL_B, LOC_DICT_A).await;
        let other_before = location_qty_of(&other).await;

        let id = create_stock_in(
            &req(vec![line_at(
                MATERIAL_A,
                "15",
                "2",
                "0",
                &other.to_string(),
            )]),
            None,
        )
        .await
        .expect("建单应通过（只校验库位行存在且同仓）");
        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .expect_err("过账必须被拒：库位账行属于另一个物料");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("不一致"), "{}", err.msg);

        // 成本账、库位账、流水、单据状态 —— 四样都必须回到过账前
        assert_eq!(
            cost_qty_sum(MATERIAL_A).await,
            Decimal::ZERO,
            "成本账必须一起回滚（不能被先写进去）"
        );
        assert_eq!(location_row_count(MATERIAL_A).await, 0);
        assert_eq!(
            location_qty_of(&other).await,
            other_before,
            "别人的库位账行一个字都不能动"
        );
        let txns: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(txns, 0, "流水也不能留下");
        let status = stockin_model::find_stock_in(id)
            .await
            .unwrap()
            .expect("单据还在")
            .status;
        assert_eq!(status, STATUS_DRAFT, "状态必须还是 DRAFT（可重试）");
    }

    /// ⑫（P6）并发收货同一物料 → 不死锁、不丢数、库位账仍是一行。
    #[tokio::test]
    async fn concurrent_receipts_share_one_location_row() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let a = create_stock_in(&req(vec![line(MATERIAL_A, "10", "2", "0")]), None)
            .await
            .unwrap();
        let b = create_stock_in(&req(vec![line(MATERIAL_A, "20", "2", "0")]), None)
            .await
            .unwrap();
        let post_request = post_req();
        let (ra, rb) = tokio::join!(
            change_stock_in_status(a, &post_request, None),
            change_stock_in_status(b, &post_request, None)
        );
        ra.unwrap_or_else(|err| panic!("并发过账 A 失败（死锁？）：{} {}", err.code, err.msg));
        rb.unwrap_or_else(|err| panic!("并发过账 B 失败（死锁？）：{} {}", err.code, err.msg));

        assert_eq!(
            null_bucket_qty(MATERIAL_A).await,
            Some(dec("30")),
            "10 + 20，库位账一笔都不能丢"
        );
        assert_eq!(
            location_row_count(MATERIAL_A).await,
            1,
            "并发下 ON CONFLICT 必须把它们并到一行"
        );
        assert_eq!(cost_qty_sum(MATERIAL_A).await, dec("30"));
        assert_eq!(location_qty_sum(MATERIAL_A).await, dec("30"));
    }

    /// ⑬（P6）已有库位账行时 → **UPDATE** 而非 INSERT 冲突（行数与主键都不变）。
    #[tokio::test]
    async fn existing_location_row_is_updated_not_reinserted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 先手造一行「已有库存」的库位账（模拟历史库位库存 500）
        let row = seed_location_row(MATERIAL_A, LOC_DICT_B).await;
        db().execute_with_params(
            "UPDATE biz_inventory_location SET qty = 500, locked_qty = 5 WHERE id = ?::uuid",
            vec![Value::from(&row)],
        )
        .await
        .unwrap();

        post_new(vec![line_at(MATERIAL_A, "80", "2", "0", &row.to_string())]).await;

        assert_eq!(
            location_qty_of(&row).await,
            Some(dec("580")),
            "必须在原行上累加（500 + 80）"
        );
        let (_, locked) = location_row_of(&row).await.unwrap();
        assert_eq!(locked, dec("5"), "已有 locked_qty 不能被收货重置");
        assert_eq!(location_row_count(MATERIAL_A).await, 1);
        // 主键没被 upsert 换掉
        let still: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM biz_inventory_location WHERE id = ?::uuid",
                vec![Value::from(&row)],
            )
            .await
            .unwrap();
        assert_eq!(still, 1, "行主键不能变（序列号/调拨都硬引用它）");
    }

    /// ⑭（P6）指针校验：`location_id` 指向**别的物料**的库位账行 → 过账 400 并说明原因。
    ///
    /// 与 ⑪ 同一路径的两侧口径（⑪ 证回滚，本条证「拒绝 + 报文」）：400 + 原文，
    /// 不能折成 500「服务器内部错误」。
    #[tokio::test]
    async fn location_row_of_another_material_is_rejected_with_reason() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let other = seed_location_row(MATERIAL_B, LOC_DICT_A).await;
        let id = create_stock_in(
            &req(vec![line_at(MATERIAL_A, "5", "1", "0", &other.to_string())]),
            None,
        )
        .await
        .unwrap();
        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .expect_err("必须拒绝：库位行属于另一个物料");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(
            err.msg.contains("物料") && err.msg.contains("不一致"),
            "报文要点名物料不一致：{}",
            err.msg
        );
        assert!(
            !err.msg.contains("服务器内部错误"),
            "必须是可读的业务报文：{}",
            err.msg
        );
    }

    // ==================== P9：作废回滚库存（卡面 §五，12 条） ====================

    /// 作废请求体。
    fn cancel_req(reason: Option<&str>) -> StockInStatusReq {
        StockInStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: reason.map(str::to_string),
            serials: Vec::new(),
        }
    }

    /// 作废并断言成功。
    async fn cancel(id: Uuid, reason: &str) {
        change_stock_in_status(id, &cancel_req(Some(reason)), None)
            .await
            .unwrap_or_else(|err| panic!("作废失败：{} {}", err.code, err.msg));
    }

    /// 某张单某类流水的条数。
    async fn txn_count(doc: Uuid, biz_type: &str) -> i64 {
        db().scalar_with_params(
            "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid AND biz_type = ?",
            vec![Value::from(&doc), Value::from(biz_type)],
        )
        .await
        .unwrap_or_else(|err| panic!("查流水失败：{err}"))
    }

    /// 某张单的凭证条数（`is_red` 为空 = 全部）。
    async fn voucher_count(doc: Uuid, red: Option<i16>) -> i64 {
        let sql = if red.is_some() {
            "SELECT COUNT(*) FROM biz_voucher WHERE source_id = ?::uuid AND is_red = 1"
        } else {
            "SELECT COUNT(*) FROM biz_voucher WHERE source_id = ?::uuid"
        };
        db().scalar_with_params(sql, vec![Value::from(&doc)])
            .await
            .unwrap_or_else(|err| panic!("查凭证失败：{err}"))
    }

    /// ① 作废收货单 → 库存回滚到作废前（成本账三值全归零）。
    #[tokio::test]
    async fn cancel_rolls_back_cost_inventory() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (dec("100"), dec("200"), dec("2.000000")),
            "前置：过账后库存 100 / 200 / 2.0"
        );
        cancel(id, "供应商召回").await;
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
            "作废后必须回到作废前（库存行 qty / amount / avg_cost 全归零）"
        );
        assert_eq!(detail_of(id).await.unwrap().header.status, STATUS_CANCELLED);
    }

    /// ② 作废收货单 → 库位账同步回滚（P6 写入的那一本账也要退）。
    #[tokio::test]
    async fn cancel_rolls_back_location_ledger() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let row = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
        let id = post_new(vec![line_at(MATERIAL_A, "40", "2", "0", &row.to_string())]).await;
        assert_eq!(
            location_qty_of(&row).await,
            Some(dec("40")),
            "前置：过账写了库位账 40"
        );
        cancel(id, "库位选错").await;
        assert_eq!(
            location_qty_of(&row).await,
            Some(Decimal::ZERO),
            "库位账必须同步回滚到 0（否则两账不一致 = 不变式 B1）"
        );
    }

    /// ③ 作废 → 生成反向流水 `OUT / PURCHASE_IN_CANCEL`，且 before_/after_ 六字段真填。
    #[tokio::test]
    async fn cancel_writes_out_txn() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "60", "2.5", "0")]).await;
        let in_no = detail_of(id).await.unwrap().header.in_no;
        cancel(id, "重复下单").await;

        let rows = db()
            .find_with_params(
                "SELECT txn_type, biz_type, doc_no, qty, amount, unit_cost, before_qty, after_qty, \
                        before_amount, after_amount, before_cost, after_cost \
                   FROM biz_inventory_txn WHERE doc_id = ?::uuid AND txn_type = 'OUT'",
                vec![Value::from(&id)],
            )
            .await
            .unwrap_or_else(|err| panic!("查反向流水失败：{err}"));
        assert_eq!(rows.len(), 1, "一张单一行明细 → 一条反向流水");
        let row = &rows[0];
        let txn_type: String = row.get_typed("txn_type").unwrap();
        let biz_type: String = row.get_typed("biz_type").unwrap();
        let doc_no: String = row.get_typed("doc_no").unwrap();
        assert_eq!(txn_type, stockin_model::TXN_TYPE_OUT);
        assert_eq!(biz_type, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL);
        assert_eq!(doc_no, in_no, "反向流水必须带原收货单号（可追溯）");
        assert_eq!(row.get_typed::<Decimal>("qty").unwrap(), dec("60"));
        assert_eq!(row.get_typed::<Decimal>("amount").unwrap(), dec("150"));
        assert_eq!(row.get_typed::<Decimal>("unit_cost").unwrap(), dec("2.5"));
        assert_eq!(row.get_typed::<Decimal>("before_qty").unwrap(), dec("60"));
        assert_eq!(
            row.get_typed::<Decimal>("after_qty").unwrap(),
            Decimal::ZERO
        );
        assert_eq!(
            row.get_typed::<Decimal>("before_amount").unwrap(),
            dec("150")
        );
        assert_eq!(
            row.get_typed::<Decimal>("after_amount").unwrap(),
            Decimal::ZERO
        );
        assert_eq!(
            row.get_typed::<Decimal>("before_cost").unwrap(),
            dec("2.500000")
        );
        assert_eq!(
            row.get_typed::<Decimal>("after_cost").unwrap(),
            Decimal::ZERO
        );
    }

    /// ④ 作废 → 红字凭证照旧生成（P2-C1 逻辑不被破坏，双向指针都在）。
    #[tokio::test]
    async fn cancel_still_generates_red_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
        let originals = db()
            .find_with_params(
                "SELECT id, voucher_no, total_debit, total_credit FROM biz_voucher \
                  WHERE source_id = ?::uuid AND is_red = 0",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(originals.len(), 1, "前置：过账即生成 1 张有效凭证");
        let original_id: Uuid = originals[0].get_typed("id").unwrap();

        cancel(id, "退货").await;
        let reds = db()
            .find_with_params(
                "SELECT id, reverse_of, total_debit, total_credit FROM biz_voucher \
                  WHERE source_id = ?::uuid AND is_red = 1",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(reds.len(), 1, "作废必须生成 1 张红字凭证");
        let red_id: Uuid = reds[0].get_typed("id").unwrap();
        assert_eq!(
            reds[0].get_typed::<Option<Uuid>>("reverse_of").unwrap(),
            Some(original_id),
            "红字凭证必须指向原凭证"
        );
        assert_eq!(
            reds[0].get_typed::<Decimal>("total_debit").unwrap(),
            originals[0].get_typed::<Decimal>("total_debit").unwrap(),
            "红冲金额 = 原凭证金额（借贷互换，不用负数）"
        );
        let reversed_by: Option<Uuid> = db()
            .scalar_with_params(
                "SELECT reversed_by FROM biz_voucher WHERE id = ?::uuid",
                vec![Value::from(&original_id)],
            )
            .await
            .unwrap();
        assert_eq!(reversed_by, Some(red_id), "原凭证要打 reversed_by（双向）");
    }

    /// ⑤ 幂等：已作废再作废 → 409，且库存不会被二次扣成负数。
    #[tokio::test]
    async fn cancel_twice_is_409() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        cancel(id, "第一次作废").await;
        let err = change_stock_in_status(id, &cancel_req(Some("第二次作废")), None)
            .await
            .expect_err("重复作废必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("不能重复作废"),
            "报文要点明幂等语义：{}",
            err.msg
        );
        assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO);
        assert_eq!(
            txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
            1,
            "反向流水只能有一条"
        );
    }

    /// ⑥ 库存不足（货已出库）时作废 → 409 + 文案，不许把库存扣成负数。
    #[tokio::test]
    async fn cancel_with_shipped_goods_is_409() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "9", "2", "0")]).await;
        // 模拟「货已出库部分」：库存只剩 3
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 3, amount = 6, avg_cost = 2 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![
                Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            ],
        )
        .await
        .unwrap();

        let err = change_stock_in_status(id, &cancel_req(Some("作废试试")), None)
            .await
            .expect_err("库存不足必须拒绝");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("已部分出库") && err.msg.contains("退货入库冲销"),
            "文案要说清「为什么不能作废」和「该怎么处理」：{}",
            err.msg
        );
        assert!(err.msg.contains('9'), "文案要点名本单数量：{}", err.msg);
        assert_eq!(
            inventory_of(MATERIAL_A).await.0,
            dec("3"),
            "拒绝时库存一个字节都不能动"
        );
        assert_eq!(
            detail_of(id).await.unwrap().header.status,
            STATUS_POSTED,
            "拒绝时必须整单回滚（状态仍是 POSTED）"
        );
    }

    /// ⑦ 冲减金额用过账流水里的 `amount`，不是「当前均价 × 数量」。
    ///
    /// 构造：先 100@2（200），再 100@3（300）→ 200 件 / 500 元 / 均价 2.5。
    /// 作废**第一张**只能退 200（流水原额），按现价算会退 250，直接污染成本。
    #[tokio::test]
    async fn cancel_uses_posted_amount_not_current_avg() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let first = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
        post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (dec("200"), dec("500"), dec("2.500000")),
            "前置：移动加权后 200 / 500 / 2.5"
        );

        cancel(first, "第一张收错了").await;
        let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("100"), "退的是第一张的 100 件");
        assert_eq!(
            amount,
            dec("300"),
            "金额必须减第一张过账流水里的 200（500 - 200 = 300）"
        );
        assert_ne!(
            amount,
            dec("250"),
            "按当前均价退 100×2.5=250 是错的：会把第二层的成本摊到第一层上"
        );
        assert_eq!(
            avg,
            dec("3.000000"),
            "剩余结存 300/100 = 3.0，正好是第二层的原始单价（摘掉一层 = 还原加权平均）"
        );
    }

    /// ⑧ 冲减后数量归零 → 金额也必须归零（不留「0 件还有钱」的幽灵金额）。
    #[tokio::test]
    async fn cancel_zero_qty_zeroes_amount() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "7", "3", "0")]).await;
        assert_eq!(inventory_of(MATERIAL_A).await.1, dec("21"));
        cancel(id, "整单退回").await;
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
            "数量归零时金额与均价必须同时归零"
        );
    }

    /// ⑨ 并发作废两张单（同一物料）→ 不死锁、两单都成功、数量正确。
    #[tokio::test]
    async fn concurrent_cancels_do_not_deadlock() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let a = post_new(vec![line(MATERIAL_A, "30", "2", "0")]).await;
        let b = post_new(vec![line(MATERIAL_A, "20", "3", "0")]).await;
        assert_eq!(inventory_of(MATERIAL_A).await.0, dec("50"));

        let req_a = cancel_req(Some("并发 A"));
        let req_b = cancel_req(Some("并发 B"));
        let (ra, rb) = tokio::join!(
            change_stock_in_status(a, &req_a, None),
            change_stock_in_status(b, &req_b, None)
        );
        ra.unwrap_or_else(|err| panic!("A 作废失败：{} {}", err.code, err.msg));
        rb.unwrap_or_else(|err| panic!("B 作废失败：{} {}", err.code, err.msg));

        assert_eq!(
            inventory_of(MATERIAL_A).await.0,
            Decimal::ZERO,
            "两张单都作废 → 库存归零（没有互相踩掉/多扣）"
        );
        assert_eq!(
            txn_count(a, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
            1
        );
        assert_eq!(
            txn_count(b, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
            1
        );
    }

    /// ⑩ 作废失败 → 事务整体回滚：库存 / 流水 / 凭证 / 状态全都不变。
    #[tokio::test]
    async fn failed_cancel_rolls_back_everything() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "9", "2", "0")]).await;
        let vouchers_before = voucher_count(id, None).await;
        assert_eq!(vouchers_before, 1);
        // 只留 1 件（模拟货已出库 8 件）
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 1, amount = 2 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![
                Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            ],
        )
        .await
        .unwrap();

        let err = change_stock_in_status(id, &cancel_req(Some("作废")), None)
            .await
            .expect_err("必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);

        assert_eq!(inventory_of(MATERIAL_A).await.0, dec("1"), "库存不变");
        assert_eq!(
            txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
            0,
            "不能留下半条反向流水"
        );
        assert_eq!(
            voucher_count(id, None).await,
            vouchers_before,
            "不能留下红字凭证"
        );
        assert_eq!(voucher_count(id, Some(1)).await, 0, "红字凭证一条都不许有");
        assert_eq!(
            detail_of(id).await.unwrap().header.status,
            STATUS_POSTED,
            "状态必须回滚成 POSTED"
        );
    }

    /// ⑪ 作废后不变量 E4 通过：库存 ≤ 有效入库（POSTED 单明细合计），且流水累计 = 库存。
    #[tokio::test]
    async fn cancel_keeps_invariant_e4() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let a = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
        post_new(vec![line(MATERIAL_A, "50", "4", "0")]).await;
        cancel(a, "作废其中一张").await;

        let material = Uuid::parse_str(MATERIAL_A).unwrap();
        let warehouse = Uuid::parse_str(WAREHOUSE).unwrap();
        // E4：库存 > 有效入库 的组合（本夹具范围内）
        let over: i64 = db()
            .scalar_with_params(
                "SELECT COUNT(*) FROM ( \
                     SELECT v.qty AS inv_qty, coalesce(p.q, 0) AS posted_qty \
                       FROM (SELECT sum(qty) AS qty FROM biz_inventory \
                              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid) v \
                       LEFT JOIN (SELECT sum(i.qty) AS q \
                                    FROM biz_stock_in s JOIN biz_stock_in_item i ON i.stock_in_id = s.id \
                                   WHERE s.deleted_at IS NULL AND s.status = 'POSTED' \
                                     AND s.warehouse_id = ?::uuid AND i.material_id = ?::uuid) p ON TRUE \
                 ) t WHERE t.inv_qty > t.posted_qty + 0.0001",
                vec![
                    Value::from(&material),
                    Value::from(&warehouse),
                    Value::from(&warehouse),
                    Value::from(&material),
                ],
            )
            .await
            .unwrap();
        assert_eq!(over, 0, "作废后库存不许大于有效入库量（E4）");
        assert_eq!(inventory_of(MATERIAL_A).await.0, dec("50"));

        // B4：流水累计 = 当前库存
        let signed: Decimal = db()
            .scalar_with_params(
                "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END), 0) \
                   FROM biz_inventory_txn WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
                vec![Value::from(&material), Value::from(&warehouse)],
            )
            .await
            .unwrap();
        assert_eq!(
            signed,
            inventory_of(MATERIAL_A).await.0,
            "反向流水必须让「流水累计 = 当前库存」重新成立（B4）"
        );
    }

    // ==================== P10：作废退序列号（卡面 §一） ====================

    /// 某张单生成的序列号：`(存活条数, 已软删条数)`。
    async fn serial_counts_of(doc: Uuid) -> (i64, i64) {
        let mut out = [0i64; 2];
        for (slot, cond) in out
            .iter_mut()
            .zip(["deleted_at IS NULL", "deleted_at IS NOT NULL"])
        {
            let sql = format!(
                "SELECT count(*) FROM biz_material_serial \
                  WHERE source_id = ?::uuid AND source_type = 'PURCHASE' AND {cond}"
            );
            *slot = db()
                .scalar_with_params(&sql, vec![Value::from(&doc)])
                .await
                .unwrap_or_else(|err| panic!("查序列号失败：{err}"));
        }
        (out[0], out[1])
    }

    /// ① 作废一物一码收货单 → 本单生成的序列号全部退回（软删），库存同步归零。
    #[tokio::test]
    async fn cancel_posted_voids_serials_of_the_receipt() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_SN, "3", "40", "0")]).await;
        assert_eq!(
            stockin_model::serial_count_of(id).await.unwrap(),
            3,
            "前置：过账生成 3 条序列号"
        );
        assert_eq!(inventory_of(MATERIAL_SN).await.0, dec("3"));

        cancel(id, "供应商整批退回").await;

        assert_eq!(
            stockin_model::serial_count_of(id).await.unwrap(),
            0,
            "P10：作废后本单的序列号必须全部出账（之前一条都不退，8 条 IN_STOCK 挂在作废单上）"
        );
        assert_eq!(
            serial_counts_of(id).await,
            (0, 3),
            "退回 = 软删（行留着做审计），不是物理删"
        );
        assert_eq!(
            inventory_of(MATERIAL_SN).await,
            (dec("0"), dec("0"), dec("0")),
            "序列号退账与库存退账是同一步，作废后两账都归零"
        );

        // 软删不是状态变更：`status` 仍是 IN_STOCK（作废 ≠ 出库/报废，三态约束不含 VOID）
        let rows = db()
            .find_with_params(
                "SELECT status FROM biz_material_serial \
                  WHERE source_id = ?::uuid AND deleted_at IS NOT NULL",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        for row in &rows {
            assert_eq!(row.get_typed::<String>("status").unwrap(), "IN_STOCK");
        }

        // 不变式①（W2c 口径）：在库序列号件数 == 库存数量
        let in_stock: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_material_serial \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                    AND status = 'IN_STOCK' AND deleted_at IS NULL",
                vec![
                    Value::from(&Uuid::parse_str(MATERIAL_SN).unwrap()),
                    Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(in_stock, 0, "作废后在库序列号件数必须归零");
    }

    /// ② 本单有序列号已出库 → 拒绝作废（409），库存 / 序列号 / 单据状态一个字节都不动。
    #[tokio::test]
    async fn cancel_refuses_when_serial_already_shipped() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_SN, "3", "40", "0")]).await;
        // 模拟「已出库 1 件」：最小号转 OUTBOUND，库存同步 3 → 2（真实出库就是这么走的）
        db().execute_with_params(
            "UPDATE biz_material_serial SET status = 'OUTBOUND', outbound_at = now() \
              WHERE id = (SELECT id FROM biz_material_serial \
                           WHERE source_id = ?::uuid AND deleted_at IS NULL \
                           ORDER BY serial_no LIMIT 1)",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
        db().execute_with_params(
            "UPDATE biz_inventory SET qty = 2, amount = 80, avg_cost = 40 \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![
                Value::from(&Uuid::parse_str(MATERIAL_SN).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            ],
        )
        .await
        .unwrap();

        let err = change_stock_in_status(id, &cancel_req(Some("作废试试")), None)
            .await
            .expect_err("本单序列号已出库必须拒绝");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("序列号") && err.msg.contains("已出库"),
            "文案要点名「是哪一类货走了」：{}",
            err.msg
        );
        assert!(
            err.msg.contains("SN-"),
            "文案要带出具体号码（运维才能去查那一件）：{}",
            err.msg
        );
        assert!(
            err.msg.contains("退货入库冲销"),
            "文案要给可执行出路：{}",
            err.msg
        );

        // 整单回滚：序列号一条都没被软删、库存没有二次扣减、状态仍 POSTED
        assert_eq!(
            serial_counts_of(id).await,
            (3, 0),
            "拒绝时序列号一条都不许动"
        );
        assert_eq!(
            inventory_of(MATERIAL_SN).await.0,
            dec("2"),
            "拒绝时库存不许二次扣减"
        );
        assert_eq!(
            detail_of(id).await.unwrap().header.status,
            STATUS_POSTED,
            "拒绝时必须整单回滚"
        );
    }

    /// ③ 只退本单的号：同物料另一张收货单的序列号不能被连带作废。
    #[tokio::test]
    async fn cancel_voids_only_serials_of_that_receipt() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let first = post_new(vec![line(MATERIAL_SN, "2", "40", "0")]).await;
        let second = post_new(vec![line(MATERIAL_SN, "1", "40", "0")]).await;
        assert_eq!(serial_counts_of(first).await, (2, 0));
        assert_eq!(serial_counts_of(second).await, (1, 0));

        cancel(first, "少收两件").await;

        assert_eq!(
            serial_counts_of(first).await,
            (0, 2),
            "本单的号必须全部退回"
        );
        assert_eq!(
            serial_counts_of(second).await,
            (1, 0),
            "别的单的号不能被连带作废（`source_id` 精确到单）"
        );
        assert_eq!(
            inventory_of(MATERIAL_SN).await.0,
            dec("1"),
            "库存只退本单那 2 件"
        );
    }

    /// ⑫ 流水缺失的历史单：拒绝作废并说明原因（不许按现价估算，不许静默）。
    #[tokio::test]
    async fn cancel_refuses_when_posted_txn_missing() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "12", "2", "0")]).await;
        // 造「单据 POSTED 但过账流水丢了」的历史形态（P9 之前的数据缺口长这样）
        db().execute_with_params(
            "DELETE FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();

        let err = change_stock_in_status(id, &cancel_req(Some("作废")), None)
            .await
            .expect_err("流水缺失必须拒绝");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("找不到过账流水"),
            "报文要说清缺口在哪：{}",
            err.msg
        );
        assert_eq!(
            inventory_of(MATERIAL_A).await.0,
            dec("12"),
            "拒绝时库存不动（不能凭现价估算退多少）"
        );
        assert_eq!(detail_of(id).await.unwrap().header.status, STATUS_POSTED);

        // 收尾清理（本模块最后一个用例）：过账即生成凭证，凭证若留在库里，
        // 按「period 全局计数」断言的模块（voucher_query）会漂数。
        // 既有用例只在 setup 里清上一轮，最后一条跑完夹具会留在库 —— 这里补一刀。
        cleanup_fixtures().await;
    }
}

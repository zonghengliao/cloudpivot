//! 收货入库单接口层（批次 D · P2-B1）：建 / 改 / 状态流转（过账）/ 删 / 列表 / 详情。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/stock-in/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/stock-in` | `business:erp:stock-in:create` | 建单（`DRAFT` + `IN-yyyyMMdd-000001`，金额服务端算） |
//! | POST | `/business/erp/stock-in/{id}/copy` | `business:erp:stock-in:create` | **复制为新草稿**（D9：原单只读，新号 / `DRAFT` / 采购关联清空） |
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
//! ①b DC-03：带来源采购单时**再过一遍**来源单闸 —— 锁采购单头（FOR UPDATE）→ 状态复核
//!    （同一 `purchase_order_fault` 判据，出口 409 + 66031）→ 本次过账数量 ≤ 可用待收量
//!    （口径 = `POSTED_RECEIVED_SQL`，出口 409 + 66032）。在扣减库存之前，失败整笔回滚。
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

use crate::business::erp::form_snapshot;
use crate::business::erp::model;
use crate::business::erp::payable;
use crate::business::erp::separation;
use crate::business::erp::stockin_bridge;
use crate::business::erp::stockin_dto::{
    CreateStockInReq, MAX_REASON_LEN, PreparedStockIn, StockInDetailRes, StockInItemReq,
    StockInItemRes, StockInPageQuery, StockInRes, StockInStatusReq,
};
use crate::business::erp::stockin_model::{
    self, CostBasis, NewStockIn, NewStockInLine, STATUS_CANCELLED, STATUS_DRAFT,
    STATUS_PENDING_CANCEL, STATUS_PENDING_POST, STATUS_POSTED, Totals, can_transition,
    status_label, transition_rejected_message,
};
use crate::business::erp::unit_guard;
use crate::business::erp::voucher;
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

/// 数量文案：去掉尾部的 0（`70.0000` → `70`，`1.5000` → `1.5`）。
///
/// 库列是 `NUMERIC(18,4)`，直接 `{}` 打印会变成「70.0000 件」——
/// 给老板看的错误文案不该带这种噪声。
fn trim_qty(value: Decimal) -> String {
    value.round_dp(4).normalize().to_string()
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

/// 单据名词（409 文案里用）。
const STOCK_IN_LABEL: &str = "收货入库单";

/// 提交审批撞 `uk_biz_flow_instance_biz_active` → `409 + 本单号`（不是「单号重复、去查 biz_seq」）。
///
/// 判据与文案收在 [`flow_model::is_biz_active_duplicate`] / [`flow_model::approval_in_progress`]，
/// 与调拨单等兄弟模块同一份实现。只在**拿得到单号的入口**（发起过账 / 发起作废审批）用它；
/// 其余出口走 [`write_error`] 的兜底分支（文案退化成「该收货入库单」）。
fn submit_conflict(err: DbError, ctx: &str, in_no: &str) -> ApiError {
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(
            context = ctx,
            in_no,
            error = %err,
            "提交审批撞审批实例唯一索引：已有在跑实例，翻 409 + 单号"
        );
        return flow_model::approval_in_progress(STOCK_IN_LABEL, Some(in_no));
    }
    write_error(err, ctx)
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`、外键 → `400 + 66002`。
fn write_error(err: DbError, ctx: &str) -> ApiError {
    // ⚠️ 顺序要紧：审批实例的唯一索引冲突**先**认出来。
    // 落到下面的 `UniqueViolation` 分支会被说成「收货入库单号或序列号重复（唯一约束
    // uk_biz_flow_instance_biz_active）…联系运维排查 biz_seq」—— 用户按提示去查取号器，
    // 方向完全错（真正原因是这张单已经有一个在跑的过账/作废审批单）。
    if flow_model::is_biz_active_duplicate(&err) {
        tracing::warn!(context = ctx, error = %err, "撞审批实例唯一索引：已有在跑实例，翻 409");
        return flow_model::approval_in_progress(STOCK_IN_LABEL, None);
    }
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
        //
        // P10：采购单被并发推走（`PO_STATUS_RACE|`）属于「当前状态不允许这次操作」→ 409，
        // 不是参数错（400）；整笔过账已回滚，客户端刷新重试即可。
        DbError::InvalidValue(message) => {
            if message.starts_with(stockin_model::PO_RACE_PREFIX) {
                ApiError::new(409, error_code::RESOURCE_CONFLICT, message)
            } else {
                ApiError::new(400, error_code::PARAM_INVALID, message)
            }
        }
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
    // 来源采购单号（F3）：详情页要显示「来源采购单：PO-xxx」并可点回采购单。
    let mut header = StockInRes::from(&row);
    let mut contract_no = None;
    if let Some(order_id) = row.source_order_id {
        header.source_order_no = stockin_model::order_no_of(order_id)
            .await
            .map_err(|error| utils::db_error(error, "查询来源采购单号"))?;
        // A4：合同号**一跳带出**（读采购单上的 `contract_no_snapshot`），
        // 不 JOIN `biz_contract`（合同在两环之上，设计 §6.4 / AP-6）。
        contract_no = stockin_model::contract_no_of(order_id)
            .await
            .map_err(|error| utils::db_error(error, "查询来源采购单的合同号"))?;
    }
    // 供应商名：读 `supplier_id` 快照关联主数据取名（**不 JOIN 采购单**，R3-a 列表口径冻结）
    if let Some(supplier_id) = row.supplier_id {
        let names = crate::business::erp::purchase_model::supplier_names_of(&[supplier_id])
            .await
            .map_err(|error| utils::db_error(error, "查询供应商名称"))?;
        header.supplier_name = names.get(&supplier_id).cloned();
    }
    let mut detail = StockInDetailRes::with_summary(
        header,
        items.iter().map(StockInItemRes::from).collect(),
        serial_count,
    );
    detail.contract_no = contract_no;
    Ok(detail)
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
        // ★ DC-09 单位止血（方案 B）：明细 `unit_id` 由物料带出；物料挂折算单位（「箱」）时
        //   过账会把「5 箱」按 5 个加库存 —— 本版本不乘 rate，直接拒单。
        unit_guard::ensure_base_unit(material_id, index, line.qty).await?;
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
                // 文案口径（卡面 O5 / 硬红线 6）：一句话说清「本次多少 / 待收多少 / 已收多少 / 订单多少」，
                // 让用户改数量时不用再翻采购单。
                return Err(bad_request(format!(
                    "物料 {} 本次收 {} 件，但采购单只剩 {} 件未收（已收 {} / 订单 {}）：\
                     请把这一行改到 {} 件以内，或先核对已过账的收货单",
                    material.material_code,
                    trim_qty(*requested),
                    trim_qty(remaining),
                    trim_qty(po_item.received_qty),
                    trim_qty(po_item.qty),
                    trim_qty(remaining)
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

/// 来源采购单**不可收**的两种情形（[`purchase_order_fault`] 的唯一产出）。
///
/// DC-03：建单期与过账期必须同一判据，只是**出口码**不同（400 / 409），
/// 所以判据抽成纯函数，两个入口各自翻译文案，不去复制判断逻辑。
enum PurchaseOrderFault {
    /// 状态不在可收白名单（`purchase_receivable`）：`DRAFT` / `RECEIVED` / `CLOSED` / `CANCELLED`。
    NotReceivable,
    /// 收货仓库与采购单的收货仓库不一致。
    WarehouseMismatch,
}

/// 来源采购单的可收判据（**唯一实现**）：`None` = 可收。
fn purchase_order_fault(
    order: &model::PurchaseOrderRow,
    warehouse_id: Uuid,
) -> Option<PurchaseOrderFault> {
    if !stockin_model::purchase_receivable(&order.status) {
        return Some(PurchaseOrderFault::NotReceivable);
    }
    if order.warehouse_id != warehouse_id {
        return Some(PurchaseOrderFault::WarehouseMismatch);
    }
    None
}

/// 取来源采购单（不存在 / 已删除 → `400 + 66002`）。
async fn fetch_purchase_order(
    order_id: Uuid,
) -> std::result::Result<model::PurchaseOrderRow, ApiError> {
    stockin_model::purchase_order_ref(order_id)
        .await
        .map_err(|error| utils::db_error(error, "查询来源采购单"))?
        .ok_or_else(|| {
            reference_missing(format!(
                "source_order_id 采购单不存在或已删除（{order_id}）"
            ))
        })
}

/// 校验来源采购单：存在、可收货、仓库一致。返回采购单行（**建单 / 改单**入口，出口 400）。
async fn require_purchase_order(
    order_id: Uuid,
    warehouse_id: Uuid,
) -> std::result::Result<model::PurchaseOrderRow, ApiError> {
    let order = fetch_purchase_order(order_id).await?;
    match purchase_order_fault(&order, warehouse_id) {
        None => Ok(order),
        Some(PurchaseOrderFault::NotReceivable) => Err(bad_request(format!(
            "来源采购单 {} 当前是「{}」（{}），不能收货：只有已提交（SUBMITTED）/ 已审批（APPROVED）/\
             部分收货（PARTIAL_RECEIVED）的采购单可以收",
            order.order_no,
            purchase_status_label(&order.status),
            order.status
        ))),
        Some(PurchaseOrderFault::WarehouseMismatch) => Err(bad_request(format!(
            "收货仓库（{warehouse_id}）与采购单 {} 的收货仓库（{}）不一致",
            order.order_no, order.warehouse_id
        ))),
    }
}

/// 过账期复核来源采购单（**DC-03 ①**）：判据与 [`require_purchase_order`] **同一份**
/// （都走 [`purchase_order_fault`]），但出口是 `409`。
///
/// 为什么不是 400：建单那一刻采购单是可收的，过账时不可收了 ⇒ 事实在「建单 → 过账」
/// 期间被别人改过（采购单被取消 / 关闭 / 已收满）。这是**现状与单据前提冲突**
/// （409），不是入参格式错（400）。报文点名采购单号 + 当前状态 + 状态码，
/// 并说清「本单未过账、三本账与应付凭证都没动」。
async fn require_purchase_order_at_post(
    order_id: Uuid,
    warehouse_id: Uuid,
) -> std::result::Result<model::PurchaseOrderRow, ApiError> {
    let order = fetch_purchase_order(order_id).await?;
    match purchase_order_fault(&order, warehouse_id) {
        None => Ok(order),
        Some(PurchaseOrderFault::NotReceivable) => Err(ApiError::new(
            409,
            error_code::ERP_SI_PO_NOT_RECEIVABLE,
            format!(
                "来源采购单 {} 当前是「{}」（{}），不能过账入库：它在「建单」与「过账」之间被取消 / 关闭了。\
                 只有已提交（SUBMITTED）/ 已审批（APPROVED）/ 部分收货（PARTIAL_RECEIVED）的采购单能收。\
                 本单未过账：库存三本账、应付单、进项凭证一条都没有生成。\
                 请核对采购单现状 —— 本单仍是草稿（DRAFT）的改单或作废后重开，\
                 处于过账审批中（PENDING_POST）的先撤回再处理",
                order.order_no,
                purchase_status_label(&order.status),
                order.status
            ),
        )),
        Some(PurchaseOrderFault::WarehouseMismatch) => Err(conflict(format!(
            "采购单 {} 的收货仓库（{}）与本次收货仓库（{warehouse_id}）不一致，不能过账：\
             本单未过账（库存三本账 / 应付 / 凭证都没动），请核对后重开",
            order.order_no, order.warehouse_id
        ))),
    }
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

// ==================== 供应商来源：唯一取值点（A4） ====================

/// 「供应商 + 无源标记」的唯一取值结果。
///
/// 设计 §6.2 ② 的 `guard_parent_supplier`，扩成**同时决定供应商来源与无源标记**：
/// 建单 / 改单 / 过账复校三处调用同一个函数，口径不会分叉。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParentPointer {
    /// 落库的供应商（有源 = 采购单的；无源 = 用户显式填的）。
    supplier_id: Uuid,
    /// 是否无源单据（AP-5：期初 / 复制 / 其他，必须显式）。
    is_sourceless: bool,
}

/// 解析并钉死「供应商 + 无源标记」。
///
/// # 规则（卡面 A4 §1 / 设计 §4.3 §4.5 §6.2）
///
/// | 情形 | 结果 |
/// |---|---|
/// | 有 `source_order_id` + 采购单存在 / 可收 / 仓库一致 | 供应商 = 采购单供应商（派生），`is_sourceless = false` |
/// | 有 `source_order_id` + 客户端传了不同供应商 | 400 + [`error_code::ERP_SI_SUPPLIER_MISMATCH`]（**不静默覆盖**） |
/// | 有 `source_order_id` + `is_sourceless = true`（自相矛盾） | 以来源单为准**归一化**：落库 `is_sourceless = false`，供应商取自采购单 |
/// | 无 `source_order_id` + 没声明无源 | 400 + [`error_code::ERP_SI_SOURCE_REQUIRED`]（AP-4：入库绑采购单，不绑供应商） |
/// | 无 `source_order_id` + `is_sourceless = true` + 有供应商 + 有备注 | 通过，`is_sourceless = true`（E4：期初 / 复制 / 其他） |
/// | 无 `source_order_id` + 无供应商 / 无备注 | 400（无源也要说清「哪来的」） |
///
/// 为什么「无源」要求备注：AP-5 要求无源可枚举且能进巡检，光一个布尔位只能数出「有几张」，
/// 数不出「为什么」——运维看到 12 张无源单时总得知道该不该清。
async fn resolve_parent_pointer(
    source_order_id: Option<Uuid>,
    declared_sourceless: Option<bool>,
    given_supplier: Option<&str>,
    remark: Option<&str>,
    warehouse_id: Uuid,
) -> std::result::Result<ParentPointer, ApiError> {
    match source_order_id {
        Some(order_id) => {
            // 有源单恒为「有源」：客户端多带一个 `is_sourceless=true` 属于自相矛盾的冗余
            // （标记本来就等价于「没有来源单」），这里按**来源单为准**归一化，不静默改供应商、
            // 也不因此拒单 —— 真正要拦的是「有源单把供应商填成别人」，见下面的 66028。
            // 反过来「无源却没打标记」才是必须拒的（AP-5：无源要显式），见 None 分支。
            // 正常/未删/可收/仓库一致（D4 的既有校验，原样复用）
            let order = require_purchase_order(order_id, warehouse_id).await?;
            let truth = order.supplier_id;
            if let Some(raw) = given_supplier {
                let got = parse_uuid(raw, "supplier_id")?;
                if got != truth {
                    return Err(ApiError::new(
                        400,
                        error_code::ERP_SI_SUPPLIER_MISMATCH,
                        format!(
                            "供应商与来源采购单 {} 不一致：本单填的是 {}，采购单是 {}。\
                             入库单的供应商由采购单带出、不允许手工填写；\
                             请留空由服务端自动带出（相等则幂等放行）",
                            order.order_no, got, truth
                        ),
                    ));
                }
            }
            Ok(ParentPointer {
                supplier_id: truth,
                is_sourceless: false,
            })
        }
        None => {
            if declared_sourceless != Some(true) {
                return Err(ApiError::new(
                    400,
                    error_code::ERP_SI_SOURCE_REQUIRED,
                    "采购入库必须绑定来源采购单（source_order_id）：供应商由采购单带出，\
                     不再由用户手工填写。确属没有采购单的入库（期初 / 复制 / 其他）\
                     请显式传 is_sourceless=true 并在 remark 写明来源"
                        .to_string(),
                ));
            }
            let Some(raw) = given_supplier.map(str::trim).filter(|t| !t.is_empty()) else {
                return Err(ApiError::new(
                    400,
                    error_code::ERP_SI_SOURCELESS_CONFLICT,
                    "无源入库单必须自己指定供应商（supplier_id）：没有采购单可带出".to_string(),
                ));
            };
            let reason = remark.map(str::trim).unwrap_or_default();
            if reason.is_empty() {
                return Err(ApiError::new(
                    400,
                    error_code::ERP_SI_SOURCELESS_CONFLICT,
                    "无源入库单必须填写 remark 说明来源（期初 / 复制自 IN-xxx / 其他）：\
                     无源标记要能进巡检、能被人看懂"
                        .to_string(),
                ));
            }
            Ok(ParentPointer {
                supplier_id: parse_uuid(raw, "supplier_id")?,
                is_sourceless: true,
            })
        }
    }
}

/// 过账前复校：单据头的供应商必须仍等于来源采购单的供应商（兜「建单后被直接改库」）。
///
/// 设计 §7.2「过账前复校」：建单期的校验挡不住绕过接口的 UPDATE，
/// 而应付单直接取单据头 `supplier_id`（`payable_bridge`），错了就是记错账。
async fn guard_posting_supplier(
    header: &stockin_model::StockInRow,
) -> std::result::Result<(), ApiError> {
    let Some(order_id) = header.source_order_id else {
        return Ok(());
    };
    let order = stockin_model::purchase_order_ref(order_id)
        .await
        .map_err(|error| utils::db_error(error, "查询来源采购单"))?
        .ok_or_else(|| {
            reference_missing(format!(
                "来源采购单不存在或已删除（{order_id}）：已绑定采购单的入库单不允许带着悬空来源过账"
            ))
        })?;
    if header.supplier_id != Some(order.supplier_id) {
        return Err(ApiError::new(
            400,
            error_code::ERP_SI_SUPPLIER_MISMATCH,
            format!(
                "收货入库单 {} 的供应商（{}）与来源采购单 {} 的供应商（{}）不一致，不能过账：\
                 供应商是采购单带出的快照，被直接改库了。请核对后重开单据",
                header.in_no,
                header
                    .supplier_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "空".to_string()),
                order.order_no,
                order.supplier_id
            ),
        ));
    }
    Ok(())
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

/// 把 `purchase_order_id`（卡面 D4 的字段名）归一成 `source_order_id`（库里列名）。
///
/// 两个都给且**不相等** → 400：静默挑一个会让「带错单」这类事故无声无息。
/// 归一后写回 `request.source_order_id`，后面的代码只认一个字段（不留双真相）。
fn resolve_purchase_order(
    request: &mut CreateStockInReq,
) -> std::result::Result<Option<String>, ApiError> {
    let pick = |raw: Option<&str>| {
        raw.map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    let column = pick(request.source_order_id.as_deref());
    let alias = pick(request.purchase_order_id.as_deref());
    match (column, alias) {
        (Some(left), Some(right)) if left != right => Err(bad_request(format!(
            "source_order_id（{left}）与 purchase_order_id（{right}）不一致：\
             两者是同一个字段（来源采购单），只能给一个或给相同的值"
        ))),
        (Some(left), _) => Ok(Some(left)),
        (None, Some(right)) => {
            request.source_order_id = Some(right.clone());
            Ok(Some(right))
        }
        (None, None) => Ok(None),
    }
}

/// 建单核心逻辑（handler 与单测共用）。
///
/// 顺序：DTO 校验 + 算金额（纯函数）→ 供应商 / 仓库存在 → 来源采购单可收 →
/// （明细为空时按采购单未收数量带出）→ 逐行物料 / 库位 / 采购行校验 → 一个事务里「取号 + 单头 + 明细」。
pub async fn create_stock_in(
    req: &CreateStockInReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<Uuid, ApiError> {
    let op = operator.into();
    let mut request = req.clone();
    resolve_purchase_order(&mut request)?;
    let source_order_id =
        parse_optional_uuid(request.source_order_id.as_ref(), "purchase_order_id")?;
    if request.items.is_empty() {
        let Some(order_id) = source_order_id else {
            return Err(bad_request(
                "items 不能为空：没有来源采购单时至少要有一行明细",
            ));
        };
        request.items = pull_lines_from_purchase(order_id).await?;
    }

    let prepared = request.prepare(false)?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;

    model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    // ★ A4：供应商的唯一取值点。有源 → 从采购单派生（客户端填错即 400）；
    //   无源 → 必须显式 is_sourceless=true + 供应商 + 备注（AP-5）。
    let pointer = resolve_parent_pointer(
        source_order_id,
        request.is_sourceless,
        prepared.supplier_id_raw.as_deref(),
        prepared.remark.as_deref(),
        warehouse_id,
    )
    .await?;
    let supplier_id = pointer.supplier_id;
    model::supplier_find_by_id(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;

    let plans = plan_lines(&prepared, source_order_id, warehouse_id).await?;
    let lines: Vec<NewStockInLine> = plans.iter().map(|plan| plan.new.clone()).collect();
    let totals = Totals::of(&lines);
    let new = NewStockIn {
        supplier_id,
        warehouse_id,
        source_order_id,
        is_sourceless: pointer.is_sourceless,
        in_date: prepared.in_date,
        invoice_received: prepared.invoice_received,
        remark: prepared.remark.as_deref(),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启建单事务"))?;
    match stockin_model::create(&tx, &new, &lines, &totals, op, now).await {
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
    let id = create_stock_in(&req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ①b 复制为新单（D9） ====================

/// 复制接口的权限码 = **建单权限码**（卡面硬红线 5：复制 = 建单，不另种码）。
///
/// 单独提成常量是为了让单测能逐字断言「复制守的就是 create 那一个码」，
/// 也防止将来有人给它顺手换成一个新码（那会让「能建单不能复制」这种裂缝出现）。
pub const COPY_PERMISSION: &str = "business:erp:stock-in:create";

/// 「复制为新单」的核心逻辑（endpoint 与单测共用，只跳过认证）。
///
/// # 语义（卡面 §二）
///
/// ```text
/// 保留：供应商 / 仓库 / 库位 / 明细（物料·数量·单价·税率）/ 发票标记 / 备注
/// 清空：单号（重新走 biz_seq 取号）、id、created_* / updated_* / *_by、
///       状态 → DRAFT、审批实例、凭证、应付、过账时间/人、作废时间/人/原因、
///       来源采购单（见下）与明细行的 source_item_id、明细行的序列号
/// 改：  入库日期 = 今天（CST）
/// ```
///
/// # ★ 决策：来源采购单**清空**（卡面 §二 的关键决策点，选 B）
///
/// 保留 `source_order_id` 看起来更「省事」，但原单**已经占用了那张采购单的待收额度**
/// （过账时回写了 `received_qty`），复制单再挂同一个采购单，会二次占用同一批待收数量：
/// 轻则过账时超收被拒（400），重则两张单各收一次、`received_qty` 加两遍，
/// 待收数量算错且难查。所以新单一律 `source_order_id = NULL`，
/// 由用户在明细里重新关联（前端复制成功后会提示「请重新关联采购单」）。
///
/// # ★ 决策：入库日期取「今天」
///
/// 复制是**一次新的入库动作**，不是「补录原单那天」。沿用旧日期有两个坑：
/// ① 老单日期落在已结账期间 → 过账被期间闸拒（用户看到的是「复制出来的单不能过账」）；
/// ② 复制单与原单同日期同供应商同金额，月末对账时极易被当成重复入库。
///
/// # ★ 决策：明细行 `serial_ids` 不复制
///
/// 序列号是**实物一物一码**，原单那批号码已经跟着原单过账落库（`IN_STOCK`）。
/// 复制单再带同一批号码，要么过账时报「号码已被占用」，要么把同一件货记成两件。
/// 复制单的序列号按原路径处理：过大账时由系统取号或现场扫码提交。
///
/// # 异常情况
///
/// - 403：无 `business:erp:stock-in:create`；
/// - 404：原单不存在 / 已软删；
/// - 400：原单没有明细（`items 不能为空`）/ 原单缺供应商（库约束要求 `PURCHASE` 必填）；
/// - 409：取号器与库里的号段不一致（`write_error` 统一口径）；
/// - 500：数据库异常。
pub async fn copy_stock_in(id: Uuid, claims: &TokenClaims) -> std::result::Result<Uuid, ApiError> {
    // 行级权限闸：路由注解已拦一次，这里再判一次（单测能直接调业务函数证明 403）
    crate::business::erp::period::ensure_permission(claims, COPY_PERMISSION, "复制收货入库单")?;
    let op = state_machine::Operator::from(claims);
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启复制事务"))?;

    // ① 读原单 + 明细（FOR SHARE，只读；原单一个字节都不改）
    let origin = match stockin_model::copy_source(&tx, id).await {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            let _ = tx.rollback().await;
            return Err(not_found("收货入库单"));
        }
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "读取待复制的收货入库单"));
        }
    };
    let (origin, items) = origin;
    if items.is_empty() {
        let _ = tx.rollback().await;
        return Err(bad_request(format!(
            "收货入库单 {} 没有任何明细，复制出来会是一张空单：请先给原单补明细，\
             或直接新建一张并填写物料",
            origin.in_no
        )));
    }
    let Some(supplier_id) = origin.supplier_id else {
        let _ = tx.rollback().await;
        return Err(bad_request(format!(
            "收货入库单 {} 没有供应商（supplier_id 为空），复制出来的采购入库单不合法：\
             请先给原单补供应商",
            origin.in_no
        )));
    };

    // ② 构造新单：新号 / DRAFT / 关联全清（见文档注释的三条决策）
    let lines: Vec<NewStockInLine> = items
        .iter()
        .map(|item| NewStockInLine {
            material_id: item.material_id,
            unit_id: item.unit_id,
            // 来源采购明细行 = 采购关联的一部分，一起清（否则待收校验会拿旧行去比）
            source_item_id: None,
            qty: item.qty,
            price: item.price,
            price_incl: item.price_incl,
            tax_rate: item.tax_rate,
            amount: item.amount,
            tax_amount: item.tax_amount,
            with_tax_amount: item.with_tax_amount,
            location_id: item.location_id,
            // 实物序列号不复制（原单那批号码已经过账占用）
            serial_ids: Vec::new(),
            remark: item.remark.clone(),
        })
        .collect();
    let totals = Totals::of(&lines);
    // ★ A4 / 设计 §4.5 档 B：复制单是 E4 的「无源」情形 —— 来源单已清空，
    //   所以必须显式打无源标记，并在备注里写明「复制自 IN-xxx」（无源要可枚举、可追溯）。
    //   备注长度按 `MAX_REMARK_LEN` 兜底：追加后超长就只保留原备注（不因为一条标记把复制搞失败）。
    let copy_mark = format!("| 复制自 {}", origin.in_no);
    let remark = match origin.remark.as_deref().map(str::trim) {
        None | Some("") => copy_mark,
        Some(text) => {
            let merged = format!("{text} {copy_mark}");
            if merged.chars().count() > crate::business::erp::stockin_dto::MAX_REMARK_LEN {
                text.to_string()
            } else {
                merged
            }
        }
    };
    let new = NewStockIn {
        supplier_id,
        warehouse_id: origin.warehouse_id,
        source_order_id: None,
        is_sourceless: true,
        in_date: crate::business::erp::stockin_dto::today_cst(),
        invoice_received: origin.invoice_received,
        remark: Some(remark.as_str()),
    };

    // ③ 插入新单 + 明细（`create` 内部取号 + 写建单日志，与正常建单同一条路径）
    match stockin_model::create(&tx, &new, &lines, &totals, op, now).await {
        Ok(new_id) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交复制事务"))?;
            Ok(new_id)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(write_error(error, "复制收货入库单"))
        }
    }
}

/// 复制为新单（`POST /business/erp/stock-in/{id}/copy`，权限 = 建单权限）。
///
/// 响应与建单完全一致（新单的详情），前端拿到就能直接打开编辑弹窗。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/copy",
    method = "POST",
    permission = "business:erp:stock-in:create"
)]
#[nik::post("/business/erp/stock-in/{id}/copy")]
pub async fn stock_in_copy(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<StockInDetailRes> {
    let origin_id = parse_uuid(&id, "收货入库单 id")?;
    let new_id = copy_stock_in(origin_id, &claims).await?;
    Ok(Res::ok(detail_of(new_id).await?))
}

// ==================== ② 改单 ====================

/// 入账态改单的 409（统一文案 + 点名本次想改的字段）。
fn locked_change(current: &stockin_model::StockInRow, field: &str) -> ApiError {
    let base = state_machine::locked_message(
        state_machine::ENTITY_STOCK_IN,
        &current.status,
        &current.in_no,
    );
    ApiError::new(
        409,
        error_code::RESOURCE_CONFLICT,
        format!("{base}\n本次改动涉及：{field}"),
    )
}

/// 入账后（`POSTED`）的改单守卫：**只允许 `remark` 变化**，其余业务字段一律 409。
///
/// 判据是「请求里显式给出的字段」vs「库里现值」逐项比对：
/// 没给的字段视为「不打算改」，给了且不同就是一次账务字段变更 → 拒绝。
/// 明细按 `(物料, 库位, 数量, 不含税单价, 税率)` 多重集合比对，顺序不影响判定。
async fn ensure_remark_only(
    current: &stockin_model::StockInRow,
    req: &CreateStockInReq,
) -> std::result::Result<(), ApiError> {
    if let Some(flag) = req.is_sourceless
        && flag != current.is_sourceless
    {
        return Err(locked_change(current, "无源标记 is_sourceless"));
    }
    if let Some(raw) = req
        .supplier_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        && parse_uuid(raw, "supplier_id")? != current.supplier_id.unwrap_or_else(Uuid::nil)
    {
        return Err(locked_change(current, "供应商 supplier_id"));
    }
    if let Some(raw) = req
        .warehouse_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        && parse_uuid(raw, "warehouse_id")? != current.warehouse_id
    {
        return Err(locked_change(current, "仓库 warehouse_id"));
    }
    if let Some(raw) = req
        .source_order_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        && Some(parse_uuid(raw, "source_order_id")?) != current.source_order_id
    {
        return Err(locked_change(current, "来源采购单 source_order_id"));
    }
    if let Some(in_date) = req.in_date
        && in_date != current.in_date
    {
        return Err(locked_change(current, "入库日期 in_date"));
    }
    if let Some(flag) = req.invoice_received
        && flag != current.invoice_received
    {
        return Err(locked_change(current, "发票标志 invoice_received"));
    }

    // 明细：给了就必须与库里完全一致（金额由明细派生，明细一致则金额必然一致）
    if !req.items.is_empty() {
        let stored = stockin_model::items_of(current.id)
            .await
            .map_err(|error| utils::db_error(error, "查询收货入库单明细"))?;
        if !items_match(&stored, &req.items)? {
            return Err(locked_change(current, "明细 / 数量 / 单价 / 税率"));
        }
        // 表头三金额由明细派生，明细一致时只可能被「自己算一套」绕过 —— 单测 ⑧ 用
        // `total_amount` 直改的场景由明细比对拦住；这里再兜一层总量校验。
    }
    Ok(())
}

/// 只读派生字段（数量合计 / 不含税金额 / 税额）的守卫。
///
/// 这三个数是**服务端由明细派生**的列，请求体里出现它们只有两种合法可能：
/// 客户端原样回显（值相等）—— 放行；想绕过明细直接改合计 —— 必须拒。
///
/// * 入账态（`POSTED`）：值不等 → **409**「单据已入账…」（与改明细同一条文案）；
/// * 草稿态：值不等 → **400**「由明细派生」—— 草稿本来就该改明细，传合计没有语义。
///
/// 为什么必须显式收下这几个字段而不是让 serde 当成未知字段丢掉：
/// 卡面 §五-3 的验收请求就是 `PUT {"total_amount": 999}`。丢掉未知字段会让它
/// 变成「什么都没改」→ `update_remark` → **200**，一个「改金额」的请求假绿。
fn ensure_derived_totals(
    current: &stockin_model::StockInRow,
    req: &CreateStockInReq,
    locked: bool,
) -> std::result::Result<(), ApiError> {
    let given = [
        ("数量合计 total_qty", req.total_qty, current.total_qty),
        (
            "不含税金额合计 total_amount",
            req.total_amount,
            current.total_amount,
        ),
        ("税额合计 total_tax", req.total_tax, current.total_tax),
    ];
    for (field, provided, stored) in given {
        let Some(provided) = provided else { continue };
        if provided.round_dp(4) == stored.round_dp(4) {
            continue;
        }
        if locked {
            return Err(locked_change(current, field));
        }
        return Err(bad_request(format!(
            "{field} 由明细派生，不能在改单时直接传：请改明细（数量 / 单价 / 税率），\
             合计由服务端重算"
        )));
    }
    Ok(())
}

/// 明细多重集合比对（顺序无关；请求里没给的字段不参与比较）。
fn items_match(
    stored: &[stockin_model::StockInItemRow],
    requested: &[StockInItemReq],
) -> std::result::Result<bool, ApiError> {
    if stored.len() != requested.len() {
        return Ok(false);
    }
    // 逐行按物料分组比较：同一物料可能多行，按 (物料, 库位) 归并后比较数量与单价
    let mut left: Vec<(Uuid, Option<Uuid>, Decimal, Decimal, Decimal)> = stored
        .iter()
        .map(|row| {
            (
                row.material_id,
                row.location_id,
                row.qty.round_dp(4),
                row.price.round_dp(6),
                row.tax_rate.round_dp(4),
            )
        })
        .collect();
    let mut right: Vec<(Uuid, Option<Uuid>, Decimal, Decimal, Decimal)> = Vec::new();
    for item in requested {
        let material = parse_uuid(
            item.material_id
                .as_deref()
                .map(str::trim)
                .filter(|text: &&str| !text.is_empty())
                .unwrap_or_default(),
            "items[].material_id",
        )?;
        let location = match item
            .location_id
            .as_deref()
            .map(str::trim)
            .filter(|text: &&str| !text.is_empty())
        {
            Some(raw) => Some(parse_uuid(raw, "items[].location_id")?),
            None => None,
        };
        let qty = item.qty.unwrap_or(Decimal::ZERO).round_dp(4);
        let price = item.price_excl.unwrap_or(Decimal::ZERO).round_dp(6);
        let tax_rate = item.tax_rate.unwrap_or(Decimal::ZERO).round_dp(4);
        right.push((material, location, qty, price, tax_rate));
    }
    left.sort();
    right.sort();
    Ok(left == right)
}

/// 改单核心逻辑：**只有 `DRAFT` 能改**（其它状态 → 409），明细整体重建。
///
/// # P10 铁律 ④：入账后只读
///
/// 命中入账态（`STOCK_IN` = `POSTED`）时：
///
/// * 请求里**任何**金额 / 数量 / 明细 / 供应商 / 仓库 / 来源单 / 入库日期 / 发票标志的改动
///   → **409**「单据已入账…不可修改」（[`state_machine::ensure_writable`] 的统一文案）；
/// * 只改 `remark`（白名单）→ 放行，走 [`stockin_model::update_remark`] 单列更新。
///
/// 判据是**服务端逐字段比对库里现值**，不是看前端有没有藏按钮。
pub async fn update_stock_in(
    id: Uuid,
    req: &CreateStockInReq,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let current = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;

    if state_machine::Entity::StockIn.is_locked(&current.status) {
        ensure_derived_totals(&current, req, true)?;
        ensure_remark_only(&current, req).await?;
        stockin_model::update_remark(db(), id, req.remark.as_deref(), operator, Utc::now())
            .await
            .map_err(|error| write_error(error, "编辑收货入库单备注"))?;
        return Ok(());
    }

    if current.status != STATUS_DRAFT {
        return Err(conflict(format!(
            "收货入库单当前是「{}」（{}），只有草稿（DRAFT）能编辑：已过账的单据已经加过库存，\
             要改只能作废后重开",
            status_label(&current.status),
            current.status
        )));
    }

    // 草稿态：合计同样不许由客户端指定（明细才是唯一真相源）
    ensure_derived_totals(&current, req, false)?;

    let mut request = req.clone();
    resolve_purchase_order(&mut request)?;
    let source_order_id =
        parse_optional_uuid(request.source_order_id.as_ref(), "purchase_order_id")?;
    if request.items.is_empty() {
        let Some(order_id) = source_order_id else {
            return Err(bad_request(
                "items 不能为空：没有来源采购单时至少要有一行明细",
            ));
        };
        request.items = pull_lines_from_purchase(order_id).await?;
    }

    let prepared = request.prepare(false)?;
    let warehouse_id = parse_uuid(&prepared.warehouse_id_raw, "warehouse_id")?;
    model::warehouse_find_by_id(warehouse_id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货仓库"))?
        .ok_or_else(|| reference_missing("warehouse_id 仓库不存在或已删除"))?;

    // ★ A4：草稿改单也走同一个取值点。
    //   ① 请求没带 `is_sourceless` 且这张单本来就是无源单（`current.is_sourceless`）→ 沿用它，
    //      免得「改个备注 / 改个数量」还要每次重申无源身份；
    //   ② 请求带了 ≠ 库里的值 → 按请求走：有源单解绑必须显式声明无源，无源单挂上采购单
    //      则供应商随之重新派生（不允许单独把供应商从 A 换成 B）。
    let declared_sourceless = request
        .is_sourceless
        .or_else(|| (source_order_id.is_none() && current.is_sourceless).then_some(true));
    let pointer = resolve_parent_pointer(
        source_order_id,
        declared_sourceless,
        prepared.supplier_id_raw.as_deref(),
        prepared.remark.as_deref(),
        warehouse_id,
    )
    .await?;
    let supplier_id = pointer.supplier_id;
    model::supplier_find_by_id(supplier_id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商"))?
        .ok_or_else(|| reference_missing("supplier_id 供应商不存在或已删除"))?;

    let plans = plan_lines(&prepared, source_order_id, warehouse_id).await?;
    let lines: Vec<NewStockInLine> = plans.iter().map(|plan| plan.new.clone()).collect();
    let totals = Totals::of(&lines);
    let new = NewStockIn {
        supplier_id,
        warehouse_id,
        source_order_id,
        is_sourceless: pointer.is_sourceless,
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
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
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
        // ★ ERP-B2：制单 ≠ 过账（可配开关，默认关闭）。
        //   这里是「没有配过账审批流程」时的降级直过路径；配了流程的另一条路
        //   （免审直过 / 审批通过回调）在 [`post_after_approval`] 里再闸一次，
        //   两条路都不会漏 —— 真正动账的是那一个函数。
        separation::guard(
            separation::Duty::Post,
            row.create_by,
            op.id,
            &format!("收货入库单 {}", row.in_no),
        )
        .await?;
        return post_stock_in(id, &row, req, &op).await;
    }

    // 作废：DRAFT 从未加过库存；POSTED 的库存/库位账/流水在同一事务里反向冲减（P9）
    if row.status == STATUS_POSTED && reason.is_none() {
        return Err(bad_request(
            "已过账的收货单作废必须给出原因（reason）：库存已增加，红冲要能追溯到为什么",
        ));
    }

    // ★ P-FLOW-3A：过账审批**中**的单据不能被「状态流转」直接过账
    //   （那等于绕过审批）。合法边 `PENDING_POST → POSTED` 只给审批回调用。
    if row.status == STATUS_PENDING_POST && target == STATUS_POSTED {
        let instance_no = pending_post_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "收货入库单 {} 已在过账审批中（{}）：审批通过会自动过账（库存/凭证/应付一起动）；\
             想取消本次提交请用「撤回」",
            row.in_no, instance_no
        )));
    }

    // ★ P16：作废审批**中**的单据不能被「状态流转」直接作废（那等于绕过复核）。
    //   合法边 `PENDING_CANCEL → CANCELLED` 只给**审批回调**用，外部 API 想走必须等结论。
    if row.status == STATUS_PENDING_CANCEL && target == STATUS_CANCELLED {
        return Err(conflict(format!(
            "收货入库单 {} 的作废申请正在审批中（{}）：审批通过会自动作废并回滚六本账；\
             想取消本次申请请用「撤回」",
            row.in_no, STATUS_PENDING_CANCEL
        )));
    }

    // ★ P16：已过账单据的作废先走**复核**（一次作废同时改六本账，不许一个人说了算）。
    //   配了已发布的 `stock_in_cancel` 流程 → 只发起审批（POSTED → PENDING_CANCEL，账不动）；
    //   没配 → 降级为旧行为（直接作废），但要留 warn（卡面 §二 ①「降级」）。
    if row.status == STATUS_POSTED {
        match stockin_model::published_cancel_flow()
            .await
            .map_err(|error| utils::db_error(error, "查询收货单作废审批流程"))?
        {
            Some(flow) => {
                return submit_cancel_approval(&row, reason.unwrap_or_default(), &op, &flow).await;
            }
            None => tracing::warn!(
                stock_in_id = %row.id,
                in_no = %row.in_no,
                "没有配置已发布的 stock_in_cancel 作废审批流程：本次作废**降级**为直接执行（六本账立即回滚），请尽快补配流程"
            ),
        }
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启状态流转事务"))?;
    match cancel_in_tx(&tx, id, &row, reason, &op, now).await {
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

/// **发起作废审批**（P16）：`POSTED → PENDING_CANCEL`，只挂审批单，六本账一动没动。
///
/// 一个事务里做完三件事：状态机过闸（边表 + reason + `biz_status_log`）→
/// `engine::start` 发起审批单 → CAS 回写单据状态与 `approval_instance_id`。
/// 任一步失败整笔回滚：**不会出现「单据在审批中但审批单没了」**。
///
/// # 错误
///
/// * `409`：已有在跑的审批单（撞 `uk_biz_flow_instance_biz_active` 之前先拦，报文可执行）；
/// * `409` + `65019`：内控拦下自审（流程节点 `self_approval=FORBID` 且发起人就是审批人）；
/// * `409`：CAS 落空（并发重复发起 / 单据已被别处改状态）。
async fn submit_cancel_approval(
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: &state_machine::Operator,
    flow: &stockin_model::PublishedFlow,
) -> std::result::Result<(), ApiError> {
    // ④ 可行性预检：货已部分出库 / 序列号已出仓的单**不许送审**
    //    （否则审批人会「批成功但账退不掉」，单据永远卡在作废审批中）
    stockin_model::ensure_cancellable(db(), row.id, &row.in_no, row.warehouse_id)
        .await
        .map_err(|error| cancel_write_error(error, "发起作废审批：可行性预检"))?;
    let Some(operator_id) = operator.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "发起作废审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    // ③ 重复发起 → 409（先查，别等库层的 uk_biz_flow_instance_biz_active 抛 23505）
    if flow_model::biz_instance_running(&flow.biz_type, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        return Err(conflict(format!(
            "收货入库单 {} 已有在跑的作废审批：请等审批结束，或先「撤回」再重新发起",
            row.in_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 收货单作废", row.in_no)
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
        biz_no: Some(row.in_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: serde_json::json!({
            "stock_in_id": row.id.to_string(),
            "in_no": row.in_no,
            "cancel_reason": reason,
            "total_qty": row.total_qty.to_string(),
            "total_amount": row.total_amount.to_string(),
            // 内控留痕：审批人 ≠ 制单人（流程节点的 self_approval=FORBID 兜底）
            "create_by": row.create_by.map(|v| v.to_string()),
        }),
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废审批事务"))?;
    let outcome = submit_cancel_tx(&tx, row, reason, operator, &start, now).await;
    match outcome {
        Ok(instance) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废审批事务"))?;
            tracing::info!(
                stock_in_id = %row.id,
                in_no = %row.in_no,
                instance_id = %instance,
                flow_code = %start.flow_code,
                "收货单已发起作废审批（状态 PENDING_CANCEL，六本账未动）"
            );
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// [`submit_cancel_approval`] 的事务体（抽出来便于统一收口回滚）。
///
/// 收 `&Transaction` 而不是 `&impl SqlExecutor`：`engine::start` 的签名要求真事务
/// （引擎要靠它在同一个事务里建实例 + 待办 + 节点）。
async fn submit_cancel_tx(
    tx: &Transaction,
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: &state_machine::Operator,
    start: &engine::StartReq,
    now: DateTime<Utc>,
) -> std::result::Result<Uuid, ApiError> {
    // ① 状态机唯一入口：边表校验 + reason 校验 + 写 biz_status_log
    state_machine::transition(
        tx,
        state_machine::ENTITY_STOCK_IN,
        row.id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        STATUS_POSTED,
        STATUS_PENDING_CANCEL,
        operator.clone(),
        Some(reason),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一个事务里建实例 + 待办 + 节点）
    let out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, "发起作废审批单", &row.in_no))?;
    if let Some(rejected) = out.rejected {
        // 例如 self_approval=FORBID 命中（65019）：内控要求复核人 ≠ 发起人。
        // 原样把引擎的码与文案交给前端，再补一句「怎么解决」。
        let hint = if rejected.code == error_code::FLOW_SELF_APPROVAL_FORBIDDEN {
            "（内控要求：作废复核人不能是发起人本人。请由他人发起，或让另一个账号具备复核权限后再审）"
        } else {
            ""
        };
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!("{}{hint}", rejected.msg),
        ));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 申请理由 + 实例 id（三者同一笔 UPDATE，否则撞 ck_biz_si_pending_cancel）
    let affected = stockin_model::apply_status_with_instance(
        tx,
        row.id,
        STATUS_POSTED,
        STATUS_PENDING_CANCEL,
        Some(reason),
        operator.id,
        Some(instance_id),
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货单进入作废审批中"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 的状态在本次发起期间已被改动（期望 POSTED），请刷新后重试",
            row.in_no
        )));
    }
    Ok(instance_id)
}

/// **申请人撤回**在跑的作废审批：`PENDING_CANCEL → POSTED`，审批单落 `WITHDRAWN`。
///
/// 与「驳回」的区别只在审批单元数据的来源（谁撤的 / 为什么），单据侧动作完全一样：
/// 回到已过账，六本账一动没动，且**必须把实例终止**（否则 `uk_biz_flow_instance_biz_active`
/// 会让下一次发起永久 409 —— 这正是 P11 踩过的坑）。
pub async fn withdraw_cancel_approval(
    id: Uuid,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if row.status != STATUS_PENDING_CANCEL {
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），只有「作废审批中」的单子可以撤回作废申请",
            row.in_no,
            status_label(&row.status),
            row.status
        )));
    }
    let Some(operator_id) = op.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "撤回作废申请需要登录用户",
        ));
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启撤回事务"))?;
    let outcome = withdraw_cancel_tx(&tx, id, &row, operator_id, now).await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交撤回事务"))?;
            tracing::info!(
                stock_in_id = %id,
                in_no = %row.in_no,
                operator = %operator_id,
                "收货单作废申请已撤回（单据回到已过账，账未动）"
            );
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// [`withdraw_cancel_approval`] 的事务体。
///
/// 顺序：**先撤审批单**（`engine::withdraw` → 实例 `WITHDRAWN`，会校验「只有发起人能撤」），
/// 再改单据状态。反过来的话，撤审批失败时单据已经回到 POSTED —— 那就是
/// 「单据说没事、审批中心还挂着待办」的孤儿（卡面 ⑭ 的整笔回滚要求）。
async fn withdraw_cancel_tx(
    tx: &Transaction,
    id: Uuid,
    row: &stockin_model::StockInRow,
    operator: Uuid,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 撤审批单：有在跑的实例走引擎（留 WITHDRAWN 轨迹 + 校验发起人身份）
    if let Some(instance_id) = row.approval_instance_id {
        let status = flow_model::find_instance(instance_id)
            .await
            .map_err(|error| utils::db_error(error, "查询作废审批单状态"))?
            .map(|instance| instance.status);
        if status.as_deref().map(str::to_ascii_uppercase).as_deref() == Some("PENDING") {
            let out = engine::withdraw(tx, instance_id, operator)
                .await
                .map_err(|error| write_error(error, "撤回作废审批单"))?;
            if let Some(rejected) = out.rejected {
                return Err(rejected);
            }
        } else {
            // 实例已不在跑（可能刚被审批推进）→ 用「按业务终止」兜底，保证不留孤儿
            stockin_bridge::cancel_approval_of(tx, id, &row.in_no, "申请人撤回作废申请")
                .await
                .map_err(|error| write_error(error, "终止作废审批单"))?;
        }
    }
    // ② 单据回到已过账（CAS，清空实例与申请理由）
    state_machine::transition(
        tx,
        state_machine::ENTITY_STOCK_IN,
        id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        STATUS_PENDING_CANCEL,
        STATUS_POSTED,
        operator,
        Some("申请人撤回作废申请"),
        None,
    )
    .await?;
    let affected = stockin_model::apply_status(
        tx,
        id,
        STATUS_PENDING_CANCEL,
        STATUS_POSTED,
        None,
        Some(operator),
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货单撤回作废"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 的状态在本次撤回期间已被改动（期望 {STATUS_PENDING_CANCEL}），请刷新后重试",
            row.in_no
        )));
    }
    Ok(())
}

/// 审批**通过**之后执行真作废（由 [`stockin_bridge::on_cancel_settled`] 调用）。
///
/// 复用 [`cancel_in_tx`]（P9 的六本账回滚），只把「起始状态」换成 `PENDING_CANCEL`：
/// 库存 / 库位账 / 流水（取流水里的 amount，不按现价）/ 红字凭证 / 应付作废 / 序列号软删，
/// 全在调用方开的一个事务里，任一步失败整笔回滚（单据仍是作废审批中）。
pub async fn cancel_after_approval(
    id: Uuid,
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let op = state_machine::Operator::of(operator);
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启审批作废事务"))?;
    match cancel_in_tx(&tx, id, row, Some(reason), &op, now).await {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交审批作废事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 审批**未通过**之后把单据打回已过账（驳回 / 撤回 / 终止）。
///
/// 账一动没动 —— 这里只有状态 + 清理 `approval_instance_id`。
pub async fn restore_after_cancel_approval(
    id: Uuid,
    row: &stockin_model::StockInRow,
    verdict: stockin_bridge::Verdict,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let reason = match verdict {
        stockin_bridge::Verdict::Cancel => "作废审批通过（不应走到打回）",
        stockin_bridge::Verdict::Restore => "作废审批未通过，单据回到已过账",
    };
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启作废打回事务"))?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_IN,
            id,
            &row.in_no,
            &stockin_model::LEGAL_EDGES,
            STATUS_PENDING_CANCEL,
            STATUS_POSTED,
            operator,
            Some(reason),
            None,
        )
        .await?;
        let affected = stockin_model::apply_status(
            &tx,
            id,
            STATUS_PENDING_CANCEL,
            STATUS_POSTED,
            None,
            operator,
            now,
        )
        .await
        .map_err(|error| write_error(error, "收货单作废打回"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "收货入库单 {} 的状态在本次打回期间已被改动（期望 {STATUS_PENDING_CANCEL}），请刷新后重试",
                row.in_no
            )));
        }
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交作废打回事务"))?;
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
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ⓪b P-FLOW-3A：过账审批中作废 → **先终止在跑的审批单**（同一事务）。
    //     不终止就是 P11 的老坑：单据作废了，审批中心还挂着一条待办，点进去 404；
    //     更糟的是 `uk_biz_flow_instance_biz_active` 会让这张单以后再也发不起审批。
    if row.status == STATUS_PENDING_POST {
        stockin_bridge::cancel_approval_of(ex, id, &row.in_no, "收货单在过账审批中作废")
            .await
            .map_err(|error| write_error(error, "作废：终止在跑的过账审批"))?;
    }
    // ⓪ P10 铁律 ③：状态变更先过唯一入口（边表 + reason 校验 + 写 biz_status_log）。
    //    `POSTED → CANCELLED` 的 reason 必填由状态机注册表统一把关（400），
    //    handler 那层的人性化文案保留，两处不会漂移（同一份 reason_edges）。
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_IN,
        id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        &row.status,
        STATUS_CANCELLED,
        operator.clone(),
        reason,
        None,
    )
    .await?;

    let affected = stockin_model::apply_status(
        ex,
        id,
        &row.status,
        STATUS_CANCELLED,
        reason,
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货入库单状态流转"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单状态在本次流转期间已被改动（期望 {}），请刷新后重试",
            row.status
        )));
    }

    // 作废的六本账回滚：`POSTED`（无流程降级 / DRAFT 之外的直接作废）与
    // `PENDING_CANCEL`（审批通过）走**同一段代码** —— 硬红线 5：不许为审批另写一套回滚。
    if row.status == STATUS_POSTED || row.status == STATUS_PENDING_CANCEL {
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
        let serials_voided = stockin_model::void_serials_of(ex, id, &row.in_no, operator.id, now)
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
            stockin_model::write_cancel_txn(ex, &doc, line, &applied, operator.id, now)
                .await
                .map_err(|error| write_error(error, "作废：写反向流水"))?;
            reversed_qty += line.qty;
            reversed_amount += line.amount;
        }

        // ④ 红字凭证（借贷互换，原凭证保持 POSTED 并打 reversed_by）
        let red = voucher::reverse_for_stock_in(ex, row, reason, operator.id, now)
            .await
            .map_err(|error| write_error(error, "作废：生成红字凭证"))?;
        // ⑤ 应付单 → CANCELLED
        payable::cancel_for_stock_in(ex, id, operator.id, now)
            .await
            .map_err(|error| write_error(error, "作废：应付单作废"))?;
        // ⑥ 回退采购单的已收数量 + 重算采购单状态（D4）
        //
        // 没有这一步：作废只回滚了库存六本账，`biz_purchase_order_item.received_qty`
        // 还是满的 —— 采购单显示"已收完"、`RECEIVED` 又不在收货白名单里，
        // 老板再也收不进这批货（正是 D4 要解决的场景）。
        // 顺序：先退数量再重算状态（重算读的是退完之后的数）。
        if let Some(order_id) = row.source_order_id {
            let source_lines = stockin_model::source_lines_of(ex, id)
                .await
                .map_err(|error| utils::db_error(error, "作废：查询来源采购明细"))?;
            for (source_item_id, qty) in source_lines {
                stockin_model::revert_received_qty(ex, source_item_id, qty, now)
                    .await
                    .map_err(|error| write_error(error, "作废：回退采购单已收数量"))?;
            }
            stockin_model::sync_purchase_status(
                ex,
                order_id,
                &row.in_no,
                "收货作废",
                operator.id,
                now,
            )
            .await
            .map_err(|error| write_error(error, "作废：回退采购单状态"))?;
        }
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

/// 过账所需的**输入快照**（明细 / 一物一码开关 / 序列号请求 / 采购未收快照）。
///
/// `req = None` 表示**审批回调路径**（[`post_after_approval`]）：审批单不携带序列号，
/// 一物一码走 [`stockin_model::generate_serials`] 的「入参为空 → 自动取号」分支
/// （与直接过账「操作员不填序列号」走的是同一条路，不另写一套）。
async fn posting_inputs(
    id: Uuid,
    source_order_id: Option<Uuid>,
    req: Option<&StockInStatusReq>,
) -> std::result::Result<
    (
        Vec<stockin_model::StockInItemRow>,
        HashMap<Uuid, bool>,
        HashMap<i32, Vec<String>>,
        HashMap<Uuid, (Decimal, Decimal)>,
    ),
    ApiError,
> {
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
    let provided = match req {
        Some(req) => request_serials(req, &items)?,
        None => HashMap::new(),
    };
    let po_snapshot = purchase_item_snapshot(source_order_id).await?;
    Ok((items, serial_required, provided, po_snapshot))
}

/// 走「过账」的**唯一对外入口**（P-FLOW-3A）：配了已发布的 `stock_in_post` 流程就先送审，
/// 没配就按 P-FLOW-3A 之前的行为直接过账（并留一条 `warn`，绝不把过账锁死）。
///
/// 为什么入口收在这里、而不是散在路由里：`PATCH /status`（下游老前端）与
/// `POST /submit-post`（本卡新按钮）**必须同一条路** —— 否则审批可以被换一个 URL 绕过。
pub async fn submit_post(
    id: Uuid,
    req: &StockInStatusReq,
    operator: impl Into<state_machine::Operator>,
) -> std::result::Result<(), ApiError> {
    let op = operator.into();
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    let target = req.target_status()?;
    if target != STATUS_POSTED {
        return change_stock_in_status(id, req, op).await;
    }
    match stockin_model::published_post_flow()
        .await
        .map_err(|error| utils::db_error(error, "查询收货单过账审批流程"))?
    {
        Some(flow) => submit_post_approval(&row, &op, &flow, req).await,
        None => {
            tracing::warn!(
                stock_in_id = %row.id,
                in_no = %row.in_no,
                "没有配置已发布的 stock_in_post 过账审批流程：本次过账**降级**为直接执行，请尽快补配流程"
            );
            change_stock_in_status(id, req, op).await
        }
    }
}

/// **发起过账审批**（P-FLOW-3A）：`DRAFT → PENDING_POST`，只挂审批单，六本账一动没动。
///
/// 一个事务里做完三件事：状态机过闸（边表 + `biz_status_log`）→ `engine::start` 发起审批单 →
/// CAS 回写单据状态与 `approval_instance_id`。任一步失败整笔回滚：
/// **不会出现「单据在审批中但审批单没了」**。
async fn submit_post_approval(
    row: &stockin_model::StockInRow,
    operator: &state_machine::Operator,
    flow: &stockin_model::PublishedFlow,
    req: &StockInStatusReq,
) -> std::result::Result<(), ApiError> {
    // ① 单据必须还是草稿（审批中 / 已过账 / 已作废都不能再提交）
    if row.status != STATUS_DRAFT {
        if row.status == STATUS_PENDING_POST {
            let instance_no = pending_post_no_of(row.approval_instance_id).await?;
            return Err(conflict(format!(
                "收货入库单 {} 已在过账审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
                row.in_no, instance_no
            )));
        }
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），只有草稿（DRAFT）可以提交过账审批",
            row.in_no,
            status_label(&row.status),
            row.status
        )));
    }
    // ② 明细 / 金额闸：空单、零金额不许送审（审批人批了也过不了账）
    let items = stockin_model::items_of(row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单明细"))?;
    if items.is_empty() {
        return Err(bad_request(format!(
            "收货入库单 {} 没有任何明细，不能提交过账审批",
            row.in_no
        )));
    }
    if row.total_with_tax <= Decimal::ZERO {
        return Err(bad_request(format!(
            "收货入库单 {} 的含税金额为 {}，不能提交过账审批（金额必须 > 0）",
            row.in_no, row.total_with_tax
        )));
    }
    // 序列号在提交时就校验一遍（数量与号码必须对齐），失败别送去给审批人批
    let _ = request_serials(req, &items)?;
    let Some(operator_id) = operator.id else {
        return Err(ApiError::new(
            401,
            error_code::AUTH_TOKEN_MISSING,
            "提交过账审批需要登录用户（令牌里没有用户 id）",
        ));
    };
    // ③ 幂等：同一单已有在跑的过账审批 → 409（先查，别等库层 uk_biz_flow_instance_biz_active）
    if flow_model::biz_instance_running(stockin_model::POST_BIZ_TYPE, row.id)
        .await
        .map_err(|error| utils::db_error(error, "查询在跑审批单"))?
    {
        let instance_no = pending_post_no_of(row.approval_instance_id).await?;
        return Err(conflict(format!(
            "收货入库单 {} 已在过账审批中（{}）：请等审批结束，或从审批中心撤回后再提交",
            row.in_no, instance_no
        )));
    }
    let (initiator_name, initiator_dept_id) = flow_model::user_profile(operator_id)
        .await
        .map_err(|error| utils::db_error(error, "查询发起人"))?
        .unwrap_or((String::new(), None));
    let instance_id = Uuid::now_v7();
    let title = format!("{} 收货过账", row.in_no)
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
        biz_no: Some(row.in_no.clone()),
        title,
        initiator_id: operator_id,
        initiator_name: Some(initiator_name),
        initiator_dept_id,
        form_data: stock_in_form_data(row).await,
    };

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启过账审批事务"))?;
    let outcome = submit_post_tx(&tx, row, operator, &start, now).await;
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
        .map_err(|error| utils::db_error(error, "提交过账审批事务"))?;
    // 锁外副作用：小额免审分支的 NOTIFY_NODE 知会消息就在这批草稿里，必须在提交之后发
    flush_effects(&mut out).await;
    tracing::info!(
        stock_in_id = %row.id,
        in_no = %row.in_no,
        instance_id = %instance,
        flow_code = %start.flow_code,
        "收货单已提交过账审批（状态 PENDING_POST，六本账未动）"
    );
    // 「小额免审」分支会在同一个事务里走完（NOTIFY 节点不阻塞）→ 提交后**立即执行过账**，
    // 否则单据会停在 PENDING_POST 且没有任何人会来推进它（没有待办）。
    if auto_finished {
        match post_after_approval(row.id, Some(operator_id)).await {
            Ok(()) => return Ok(()),
            // ★ ERP-B2：免审直过被「制单 ≠ 过账」拦下 —— 单据已经在 PENDING_POST，
            //   而流程没有留下任何待办（免审），必须当场打回 DRAFT，否则就是一张死单。
            //   打回后由**别人**来提交即可过账（换人这条路正是内控想要的）。
            Err(error) if error.code == error_code::ERP_SEPARATION_OF_DUTIES => {
                if let Err(restore_error) = restore_after_post_rejection(
                    row.id,
                    row,
                    "制单与过账分离：免审过账被内控拦截，已打回草稿等待他人过账",
                    Some(operator_id),
                )
                .await
                {
                    tracing::error!(
                        stock_in_id = %row.id,
                        in_no = %row.in_no,
                        error = %restore_error.msg,
                        "免审过账被内控拦截后打回草稿失败：单据可能停在 PENDING_POST"
                    );
                    return Err(restore_error);
                }
                tracing::warn!(
                    stock_in_id = %row.id,
                    in_no = %row.in_no,
                    "免审过账被「制单与过账分离」拦截，单据已打回 DRAFT（需他人提交过账）"
                );
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// [`submit_post_approval`] 的事务体：返回引擎动作结果（调用方提交后负责发锁外副作用）。
async fn submit_post_tx(
    tx: &Transaction,
    row: &stockin_model::StockInRow,
    operator: &state_machine::Operator,
    start: &engine::StartReq,
    now: DateTime<Utc>,
) -> std::result::Result<engine::ActionOut, ApiError> {
    // ① 状态机唯一入口：边表校验 + 写 biz_status_log
    state_machine::transition(
        tx,
        state_machine::ENTITY_STOCK_IN,
        row.id,
        &row.in_no,
        &stockin_model::LEGAL_EDGES,
        STATUS_DRAFT,
        STATUS_PENDING_POST,
        operator.clone(),
        Some("提交过账审批"),
        None,
    )
    .await?;
    // ② 发起审批单（引擎在同一个事务里建实例 + 待办 + 节点）
    let mut out = engine::start(tx, start)
        .await
        .map_err(|error| submit_conflict(error, "发起过账审批单", &row.in_no))?;
    if let Some(rejected) = out.rejected.take() {
        return Err(ApiError::new(
            rejected.status,
            rejected.code,
            format!(
                "{}（内控要求：过账复核人不能是发起人本人。请由他人提交，或让另一个账号具备复核权限后再审）",
                rejected.msg
            ),
        ));
    }
    let instance_id = out
        .instance_id
        .ok_or_else(|| ApiError::new(500, error_code::INTERNAL_ERROR, "审批单发起后没有实例 id"))?;
    // ③ CAS 回写单据：状态 + 实例 id（两者同一笔 UPDATE，否则撞 ck_biz_si_pending_approval）
    let affected = stockin_model::apply_status_with_instance(
        tx,
        row.id,
        STATUS_DRAFT,
        STATUS_PENDING_POST,
        None,
        operator.id,
        Some(instance_id),
        now,
    )
    .await
    .map_err(|error| write_error(error, "收货单进入过账审批中"))?;
    if affected == 0 {
        return Err(conflict(format!(
            "收货入库单 {} 的状态在本次提交期间已被改动（期望 DRAFT），请刷新后重试",
            row.in_no
        )));
    }
    Ok(out)
}

/// 事务提交后发送锁外副作用（审批通知 / 通知节点的知会消息）。
///
/// 与 `flow::handler::drain` 同口径：投递失败只 WARN，**不影响已经落库的业务**
/// （流程已推进是事实，邮件/站内信没发出去是运维问题，不该让单据回滚）。
async fn flush_effects(out: &mut engine::ActionOut) {
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
}

/// 审批**通过**之后执行过账（由 [`stockin_bridge::on_post_settled`] / 提交后的「免审」分支调用）。
///
/// 复用 [`post_in_tx`]（六本账唯一实现），只把起始状态换成 `PENDING_POST`：
/// 库存 / 库位账 / 流水 / 序列号 / 采购已收数量 / 凭证 / 应付全在同一个事务里，
/// 任一步失败整笔回滚（单据仍是过账审批中，等人工或下一轮重试）。
pub async fn post_after_approval(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if row.status != STATUS_PENDING_POST {
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），不是过账审批中：不能执行过账",
            row.in_no,
            status_label(&row.status),
            row.status
        )));
    }
    // ★ ERP-B2：制单 ≠ 过账（可配开关，默认关闭）。
    //
    // 这里是**真正动六本账**的地方，两条路都汇到它：
    // ① 免审直过（[`submit_post_approval`] 的 `auto_finished` 分支，`operator` = 提交人本人）
    //    —— 种子流程 `stock_in_post` 对「< 1000 元」的单据只发知会不排审批，
    //    过账人就是制单人本人，这是「收货单制单人 = 过账人 8 条」的真正来源；
    // ② 审批通过回调（`stockin_bridge`，`operator` = 审批人）—— 通常天然分流。
    //
    // 放在 `post_in_tx` 之前：六本账一动没动就被拦下，调用方（bridge / 免审分支）
    // 负责把单据打回草稿，不会留下「停在审批中却没有任何待办」的死单。
    // ★ A4：审批通过的过账路径同样复校一次（两条入口同一把尺子）
    guard_posting_supplier(&row).await?;
    let op = state_machine::Operator::of(operator.or(row.create_by));
    separation::guard(
        separation::Duty::Post,
        row.create_by,
        op.id,
        &format!("收货入库单 {}", row.in_no),
    )
    .await?;
    let (items, serial_required, provided, po_snapshot) =
        posting_inputs(id, row.source_order_id, None).await?;
    let mut ordered: Vec<&stockin_model::StockInItemRow> = items.iter().collect();
    ordered.sort_by(|a, b| {
        (a.material_id, a.location_id, a.line_no).cmp(&(b.material_id, b.location_id, b.line_no))
    });
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启审批过账事务"))?;
    let outcome = post_in_tx(
        &tx,
        id,
        &row,
        &ordered,
        &serial_required,
        &provided,
        &po_snapshot,
        &op,
        STATUS_PENDING_POST,
        now,
    )
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交审批过账事务"))?;
            tracing::info!(
                stock_in_id = %id,
                in_no = %row.in_no,
                "过账审批通过：库存/库位账/流水/序列号/凭证/应付已在一个事务里落库"
            );
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 审批**未通过**之后把单据打回草稿（驳回 / 撤回 / 终止）。
///
/// 账一动不动 —— 这里只有状态 + 清理 `approval_instance_id`。
pub async fn restore_after_post_rejection(
    id: Uuid,
    row: &stockin_model::StockInRow,
    reason: &str,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启过账打回事务"))?;
    let outcome = async {
        state_machine::transition(
            &tx,
            state_machine::ENTITY_STOCK_IN,
            id,
            &row.in_no,
            &stockin_model::LEGAL_EDGES,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            operator,
            Some(reason),
            None,
        )
        .await?;
        let affected = stockin_model::apply_status(
            &tx,
            id,
            STATUS_PENDING_POST,
            STATUS_DRAFT,
            None,
            operator,
            now,
        )
        .await
        .map_err(|error| write_error(error, "收货单过账审批打回草稿"))?;
        if affected == 0 {
            return Err(conflict(format!(
                "收货入库单 {} 的状态在本次打回期间已被改动（期望 {STATUS_PENDING_POST}），请刷新后重试",
                row.in_no
            )));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交过账打回事务"))?;
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// 在跑实例的单号（拼 409 文案用；查不到就给 `-`）。
async fn pending_post_no_of(instance_id: Option<Uuid>) -> std::result::Result<String, ApiError> {
    let Some(instance_id) = instance_id else {
        return Ok("-".to_string());
    };
    let no = flow_model::find_instance(instance_id)
        .await
        .map_err(|error| utils::db_error(error, "查询过账审批单"))?
        .map(|instance| instance.instance_no)
        .unwrap_or_else(|| "-".to_string());
    Ok(no)
}

/// 过账审批的**表单变量**。金额必须写成 JSON **数字**（不是字符串）：条件边比较走 `as_f64()`，
/// `"5000.0000"` 会 TypeMismatch → 引擎按「条件不成立」静默走兜底边（P17 实测）。
///
/// ⚠️ 返回类型必须写全 `serde_json::Value`：本文件 `use nik::prelude::*` 带进来的
/// `Value` 是 **数据库绑定值**（`nik_db_core::Value`），两者同名不同物。
///
/// # P1-FIX：冻结显示名 + 明细（`supplier_name` / `warehouse_name` / `items`）
///
/// 审批岗位角色没有 `business:erp:*` 读权限（生产实测四个角色 0 条），前端译名必然降级成
/// `(ID: …)`。显示名与明细在**发起过账审批时**冻结，审批人（仓管 / 财务）零 ERP 权限也能看懂。
/// 键名契约见 [`crate::business::erp::form_snapshot`]；查询失败降级为不写该键，不阻断提交。
async fn stock_in_form_data(row: &stockin_model::StockInRow) -> serde_json::Value {
    let mut form = serde_json::json!({
        "stock_in_id": row.id.to_string(),
        "in_no": row.in_no,
        "amount": decimal_as_number(row.total_with_tax),
        "total_with_tax": row.total_with_tax.to_string(),
        "total_amount": row.total_amount.to_string(),
        "total_qty": row.total_qty.to_string(),
        "warehouse_id": row.warehouse_id.to_string(),
        "supplier_id": row.supplier_id.map(|value| value.to_string()),
        // 内控留痕：审批人 ≠ 制单人（`stockin_bridge::guard_self_approval` 兜底）
        "create_by": row.create_by.map(|value| value.to_string()),
    });
    // 供应商可空（无来源采购单的收货）：可空时**不查也不写**，别写出 `"null"` 这种值。
    let supplier = match row.supplier_id {
        Some(id) => form_snapshot::supplier_name(db(), id).await,
        None => Ok(None),
    };
    let warehouse = form_snapshot::warehouse_name(db(), row.warehouse_id).await;
    let items = form_snapshot::stock_in_items(db(), row.id).await;
    if let Some(object) = form.as_object_mut() {
        match supplier {
            Ok(Some(name)) => {
                object.insert("supplier_name".to_string(), serde_json::Value::String(name));
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(stock_in_id = %row.id, %error, "审批快照：供应商名查询失败，降级");
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
                tracing::warn!(stock_in_id = %row.id, %error, "审批快照：仓库名查询失败，降级");
            }
        }
        match items {
            Ok(items) => {
                object.insert("items".to_string(), items);
            }
            Err(error) => {
                tracing::warn!(stock_in_id = %row.id, %error, "审批快照：明细查询失败，降级");
            }
        }
    }
    form
}

/// `Decimal` → `serde_json::Number`（`rust_decimal` 未开 `maths`，没有 `to_f64`）。
fn decimal_as_number(value: Decimal) -> serde_json::Value {
    let text = value.round_dp(4).to_string();
    match text.parse::<f64>() {
        Ok(float) => serde_json::Number::from_f64(float)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

/// 过账前复核来源采购单（**DC-03**）：状态 + 待收额度，在**扣减库存之前**、同一事务内。
///
/// # 为什么必须在这里再来一次
///
/// 建单期的 [`require_purchase_order`] 只保证「建单那一刻可收」。审计 DC-03 实测：
/// 采购单在「建单 → 过账」之间被取消 / 关闭，或另一张收货单把它收满，
/// 过账照样跑完 —— 三本账 + 应付 + 进项凭证全部生成，采购单状态还被 `sync_purchase_status`
/// 静默跳过（不可收的状态不在它的白名单里）。
///
/// # 三条闸（顺序即代码顺序）
///
/// 1. [`stockin_model::lock_purchase_order`]：`FOR UPDATE` 锁采购单头，
///    并发「取消采购单」「另一张收货单过账」都要等本事务结束 ⇒ 下面两条判据在提交前有效；
/// 2. [`require_purchase_order_at_post`]：状态复核（与建单同一判据，出口 409）；
/// 3. 本次过账数量 ≤ 可用待收量：待收口径复用
///    [`stockin_model::order_item_balances`]（= `POSTED_RECEIVED_SQL`），
///    挡住「两张收货单双开、各按未收额度建单、先后都过账」这种重复入库。
async fn guard_purchase_before_post(
    ex: &impl SqlExecutor,
    header: &stockin_model::StockInRow,
    ordered: &[&stockin_model::StockInItemRow],
) -> std::result::Result<(), ApiError> {
    let Some(order_id) = header.source_order_id else {
        // 无源单据（期初 / 复制 / 其他）：没有采购单可复核
        return Ok(());
    };
    // ① 锁采购单头（同时确认它还在）
    let locked = stockin_model::lock_purchase_order(ex, order_id)
        .await
        .map_err(|error| utils::db_error(error, "过账：锁定来源采购单"))?;
    let Some((order_no, _)) = locked else {
        return Err(conflict(format!(
            "来源采购单不存在或已删除（{order_id}）：已绑定采购单的入库单不允许带着悬空来源过账，\
             本单未过账"
        )));
    };
    // ② 状态复核（同一判据，出口 409）
    require_purchase_order_at_post(order_id, header.warehouse_id).await?;
    // ③ 待收额度复核：本单**按采购明细行聚合**后的数量 ≤ 可用待收量
    //    （同一采购行可能被本单拆成多行收，聚合后再比才对得上口径）
    let mut wanted: Vec<(Uuid, Decimal, i32)> = Vec::new();
    for item in ordered {
        let Some(source_item_id) = item.source_item_id else {
            continue;
        };
        match wanted
            .iter_mut()
            .find(|(item_id, _, _)| *item_id == source_item_id)
        {
            Some(slot) => slot.1 += item.qty,
            None => wanted.push((source_item_id, item.qty, item.line_no)),
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let balances = stockin_model::order_item_balances(ex, order_id)
        .await
        .map_err(|error| utils::db_error(error, "过账：查询采购单待收数量"))?;
    for (source_item_id, qty, line_no) in wanted {
        let Some(balance) = balances.iter().find(|row| row.item_id == source_item_id) else {
            return Err(conflict(format!(
                "第 {line_no} 行引用的采购明细（{source_item_id}）已不属于采购单 {order_no}：\
                 该采购行被删除或挪到了别的采购单。本单未过账，请重新从采购单带出明细"
            )));
        };
        let available = balance.available();
        if qty > available {
            return Err(ApiError::new(
                409,
                error_code::ERP_SI_OVER_RECEIVE,
                format!(
                    "第 {line_no} 行本次过账 {} 件，但来源采购单 {order_no} 第 {} 行只剩 {} 件待收\
                     （订单 {}、已过账收货 {}）：另一张收货单可能刚过账收走了。\
                     本单未过账（库存三本账 / 应付 / 凭证都没动），请把这一行改到 {} 件以内，\
                     或先核对已过账的收货单",
                    trim_qty(qty),
                    balance.line_no,
                    trim_qty(available),
                    trim_qty(balance.order_qty),
                    trim_qty(balance.posted_received),
                    trim_qty(available)
                ),
            ));
        }
    }
    Ok(())
}

/// **过账**（本卡核心）：一个事务里改状态 + 加库存 + 写流水 + 生成序列号 + 回写采购单。
async fn post_stock_in(
    id: Uuid,
    header: &stockin_model::StockInRow,
    req: &StockInStatusReq,
    operator: &state_machine::Operator,
) -> std::result::Result<(), ApiError> {
    // ★ A4：过账前复校「单据头供应商 = 来源采购单供应商」。
    //   建单期的校验挡不住绕过接口的 UPDATE，而应付/凭证直接取单据头 `supplier_id`。
    guard_posting_supplier(header).await?;
    let (items, serial_required, provided, po_snapshot) =
        posting_inputs(id, header.source_order_id, Some(req)).await?;

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
        STATUS_DRAFT,
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
///
/// `from_status` 是**唯一**与入口相关的差别（P-FLOW-3A 加的）：
/// * `DRAFT` —— 直接过账（未配过账流程 / 内部调用 / 老用例）；
/// * `PENDING_POST` —— 过账审批通过后执行（[`post_after_approval`]）。
///
/// 其余六本账动作（库存 / 库位账 / 流水 / 序列号 / 采购已收 / 凭证 + 应付）
/// **一行都没重写**：审批通过只换了入口状态，"审 A 记 B" 的风险由
/// `STOCK_IN_LOCKED`（PENDING_POST 锁单据）兜住。
#[allow(clippy::too_many_arguments)]
async fn post_in_tx(
    ex: &impl SqlExecutor,
    id: Uuid,
    header: &stockin_model::StockInRow,
    ordered: &[&stockin_model::StockInItemRow],
    serial_required: &HashMap<Uuid, bool>,
    provided: &HashMap<i32, Vec<String>>,
    po_snapshot: &HashMap<Uuid, (Decimal, Decimal)>,
    operator: &state_machine::Operator,
    from_status: &str,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    // ① 锁单头（并发重复过账在这里串行化）
    let locked = stockin_model::lock_stock_in(ex, id)
        .await
        .map_err(|error| utils::db_error(error, "锁定收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    if locked.status != from_status {
        return Err(conflict(format!(
            "收货入库单 {} 当前是「{}」（{}），不能过账：库存只加一次",
            locked.in_no,
            status_label(&locked.status),
            locked.status
        )));
    }

    // ①b DC-03：过账复核来源采购单（状态 + 待收额度）。
    //    必须在 ② 改状态与 ③ 扣减库存**之前**：否则「采购单已取消」也会把三本账、
    //    应付与进项凭证写出去。锁在事务内，失败整笔回滚（不留半条状态日志）。
    guard_purchase_before_post(ex, &locked, ordered).await?;

    // ② CAS 写状态（`WHERE status = from_status`）
    //    P10 铁律 ③：先过唯一入口（边表 + reason 校验 + 写 biz_status_log），
    //    再 CAS 改状态。两步同一事务：CAS 落空 → 整笔回滚，日志不会半条落地。
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_IN,
        id,
        &header.in_no,
        &stockin_model::LEGAL_EDGES,
        from_status,
        STATUS_POSTED,
        operator.clone(),
        None,
        None,
    )
    .await?;
    let affected =
        stockin_model::apply_status(ex, id, from_status, STATUS_POSTED, None, operator.id, now)
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
            operator.id,
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
                    operator: operator.id,
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

    // ⑥ 推进采购单状态（PARTIAL_RECEIVED / RECEIVED）—— P10：走状态机唯一入口
    if let Some(order_id) = header.source_order_id {
        stockin_model::sync_purchase_status(
            ex,
            order_id,
            &header.in_no,
            "收货过账",
            operator.id,
            now,
        )
        .await
        .map_err(|error| write_error(error, "过账：推进采购单状态"))?;
    }

    // ⑦ 应付单 + 凭证（P2-C1，方案 A：入库即生成；**同一事务**，失败整单回滚）
    //    金额全部取自单头（header.total_*），不重算（硬红线 5）。
    //    顺序：先凭证（应付要引用 voucher_id）→ 再应付 → 最后回写收货单两列。
    let first_material = ordered.first().map(|item| item.material_id);
    let voucher_id = voucher::create_for_stock_in(ex, &locked, first_material, operator.id, now)
        .await
        .map_err(|error| write_error(error, "过账：生成凭证"))?;
    let payable_id = payable::create_for_stock_in(ex, &locked, Some(voucher_id), operator.id, now)
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
    submit_post(id, &req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③c 提交过账审批（P-FLOW-3A） ====================

/// 提交收货单过账（`DRAFT → PENDING_POST`，或未配流程时降级直接过账）。
///
/// 与 `PATCH /status` 走**同一个函数**（[`submit_post`]）：
/// 只要配了已发布的 `stock_in_post` 流程，两条路都必须先送审 —— 不存在换 URL 绕过审批。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/submit-post",
    method = "POST",
    permission = "business:erp:stock-in:submit-post-approval"
)]
#[nik::post("/business/erp/stock-in/{id}/submit-post")]
pub async fn stock_in_submit_post(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<StockInStatusReq>>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    let mut req = body.map(|Json(item)| item).unwrap_or_default();
    // `/submit-post` 的语义就是「提交过账」：body 可省，缺省目标态即 POSTED
    if req.status.is_none() {
        req.status = Some(STATUS_POSTED.to_string());
    }
    submit_post(id, &req, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ③b 撤回作废申请（P16） ====================

/// 撤回收货单的作废申请（`PENDING_CANCEL → POSTED`）。
///
/// 权限码沿用 `business:erp:stock-in:status`：撤回是「状态流转」的一种，
/// 且引擎侧还会校验「只有发起人能撤」（其余人 403 + 65004）。
#[nik::permission(
    path = "/business/erp/stock-in/{id}/cancel-withdraw",
    method = "POST",
    permission = "business:erp:stock-in:status"
)]
#[nik::post("/business/erp/stock-in/{id}/cancel-withdraw")]
pub async fn stock_in_cancel_withdraw(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<StockInDetailRes> {
    let id = parse_uuid(&id, "id")?;
    withdraw_cancel_approval(id, &claims).await?;
    Ok(Res::ok(detail_of(id).await?))
}

// ==================== ④ 删除 ====================

/// 删除核心逻辑：**只有 `DRAFT`** 能删（软删）。
///
/// ★ P16 反向兜底（P11 的教训）：删除**同一事务**里还要把这张单在跑的作废审批
/// **一并终止**（`stockin_bridge::cancel_approval_of`）。DRAFT 单据按当前状态机不可能
/// 挂着审批单，但这条兜底是仓库「删单 ≠ 留孤儿待办」的机器化承诺 —— 哪天放开
/// 「审批中的单据也能删」，这里不会漏。
pub async fn delete_stock_in(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = stockin_model::find_stock_in(id)
        .await
        .map_err(|error| utils::db_error(error, "查询收货入库单"))?
        .ok_or_else(|| not_found("收货入库单"))?;
    // P-FLOW-3A：`PENDING_POST`（过账审批中）也允许删 —— 审批单会被同一事务终止，
    // 不留孤儿待办（P11 教训）。除这两种状态外，单据是库存/成本链的事实，只能作废。
    if row.status != STATUS_DRAFT && row.status != STATUS_PENDING_POST {
        return Err(conflict(format!(
            "收货入库单当前是「{}」（{}），只有草稿（DRAFT）或过账审批中（{STATUS_PENDING_POST}）能删：\
             已过账的单据是库存与成本链的事实，只能作废（{STATUS_PENDING_CANCEL} 状态下请先撤回作废申请）",
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
        // ① 先撤在跑的审批（作废审批 / 过账审批都按 biz_id 找；幂等：没有就什么都不做）
        stockin_bridge::cancel_approval_of(&tx, id, &row.in_no, "收货单已删除，审批自动终止")
            .await
            .map_err(|error| write_error(error, "删除：终止在跑的审批"))?;
        // ② 再软删单据
        let affected = stockin_model::soft_delete(&tx, id, operator, now)
            .await
            .map_err(|error| utils::db_error(error, "软删收货入库单"))?;
        if affected == 0 {
            return Err(not_found("收货入库单"));
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
    let mut list: Vec<StockInRes> = rows.iter().map(StockInRes::from).collect();
    // 来源采购单号：一次 `IN (...)` 批量补齐（列表要显示「来源采购单 PO-xxx」，
    // 逐条查 = 一页 20 次往返）。
    let order_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.source_order_id).collect();
    if !order_ids.is_empty() {
        let nos = stockin_model::order_nos_of(&order_ids)
            .await
            .map_err(|error| utils::db_error(error, "查询来源采购单号"))?;
        for (header, row) in list.iter_mut().zip(rows.iter()) {
            if let Some(order_id) = row.source_order_id {
                header.source_order_no = nos.get(&order_id).cloned();
            }
        }
    }
    // 供应商名称：同样一次 `IN (...)` 批量补齐，读的是**本单的 `supplier_id` 快照**去关联主数据
    // （不是实时 JOIN 采购单拿供应商 —— 列表口径必须冻结在「收货那一刻」，R3-a）。
    let supplier_ids: Vec<Uuid> = rows.iter().filter_map(|row| row.supplier_id).collect();
    if !supplier_ids.is_empty() {
        let names = crate::business::erp::purchase_model::supplier_names_of(&supplier_ids)
            .await
            .map_err(|error| utils::db_error(error, "查询供应商名称"))?;
        for (header, row) in list.iter_mut().zip(rows.iter()) {
            if let Some(supplier_id) = row.supplier_id {
                header.supplier_name = names.get(&supplier_id).cloned();
            }
        }
    }
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

    // ---------- 审批实例唯一索引冲突：409 + 自家单号（不是「单号重复、去查 biz_seq」） ----------

    /// 撞 `uk_biz_flow_instance_biz_active` → **409 + 本单号 + 「已有审批中实例」**。
    ///
    /// 两条分支都钉住：驱动给了约束名（`UniqueViolation`）与退化形态（`DbError::InvalidValue`，
    /// 文案里只剩 PG 原文）—— 后者正是真库里掉到「联系运维排查 biz_seq」的那条路。
    #[test]
    fn biz_active_conflict_maps_to_409_with_stock_in_no() {
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
            constraint: Some("uk_biz_stock_in_no".to_string()),
            source: None,
        };
        assert!(!flow_model::is_biz_active_duplicate(&other));

        for err in [variant(), by_text()] {
            let api = submit_conflict(err, "发起过账审批单", "IN-20261102-000123");
            assert_eq!(api.status, 409, "{}", api.msg);
            assert_eq!(api.code, error_code::RESOURCE_CONFLICT);
            assert!(api.msg.contains("IN-20261102-000123"), "{}", api.msg);
            assert!(api.msg.contains("已有审批中实例"), "{}", api.msg);
            assert!(
                !api.msg.contains("biz_seq"),
                "不能把用户指到取号器：{}",
                api.msg
            );
            assert!(!api.msg.contains("单号重复"), "{}", api.msg);
        }

        // 拿不到单号的通用出口（write_error）也必须 409 + 可执行文案
        let generic = write_error(variant(), "收货入库单状态流转");
        assert_eq!(generic.status, 409);
        assert_eq!(generic.code, error_code::RESOURCE_CONFLICT);
        assert!(generic.msg.contains("该收货入库单"), "{}", generic.msg);
        assert!(!generic.msg.contains("biz_seq"), "{}", generic.msg);

        // 真的号段冲突不能被新分支吞掉：仍然 409 + 取号器提示
        let genuine = write_error(
            DbError::UniqueViolation {
                column: None,
                constraint: Some("uk_biz_stock_in_no".to_string()),
                source: None,
            },
            "创建收货入库单",
        );
        assert_eq!(genuine.status, 409);
        assert!(genuine.msg.contains("biz_seq"), "{}", genuine.msg);
    }

    const UNIT: &str = "b2b10001-0000-4000-8000-000000000001";
    const SUPPLIER: &str = "b2b10001-0000-4000-8000-000000000002";
    const WAREHOUSE: &str = "b2b10001-0000-4000-8000-000000000003";
    /// 非一物一码物料（螺丝）。
    const MATERIAL_A: &str = "b2b10001-0000-4000-8000-000000000011";
    /// 非一物一码物料（垫片）。
    const MATERIAL_B: &str = "b2b10001-0000-4000-8000-000000000012";
    /// 一物一码物料（电机）。
    const MATERIAL_SN: &str = "b2b10001-0000-4000-8000-000000000013";
    /// DC-09 夹具：挂在折算单位上的物料 —— 本版本**不能**在收货单里使用。
    const MATERIAL_BOX: &str = "b2b10001-0000-4000-8000-000000000015";
    /// 库位字典（`biz_location.id`，P6：库位账行的 `location_id` 列指这个）。
    const LOC_DICT_A: &str = "b2b10003-0000-4000-8000-000000000001";
    /// 库位字典（第二个库位）。
    const LOC_DICT_B: &str = "b2b10003-0000-4000-8000-000000000002";
    /// A4 夹具：**第二个**供应商。
    ///
    /// 与 [`SUPPLIER`] 分开，才能证明「入库单的供应商确实从采购单派生」——
    /// 用同一个供应商的话，派生与手填的结果一样，断言再严也测不出东西。
    const A4_SUPPLIER_B: &str = "b2b10001-0000-4000-8000-000000000014";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('b2b10001-0000-4000-8000-000000000001', 'P2B1-PCS', '个', 1, NULL, 1.000000, 1, 1)
         ON CONFLICT DO NOTHING",
        // DC-09 夹具：折算单位「箱」（is_base = 0，1 箱 = 12 个）。收货单里用它会 400。
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
         VALUES ('b2b10001-0000-4000-8000-000000000004', 'P2B1-BOX', '箱', 0,
                 'b2b10001-0000-4000-8000-000000000001', 12.000000, 1, 2)
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
         VALUES ('b2b10001-0000-4000-8000-000000000002', 'P2B1-SUP', 'P2B1 测试供应商', 1)
         ON CONFLICT DO NOTHING",
        // A4 夹具：第二个供应商（证明入库单供应商是从采购单派生的）
        "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status)
         VALUES ('b2b10001-0000-4000-8000-000000000014', 'A4-SUP-B', 'A4 二号供应商', 1)
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
        // DC-09 夹具：物料挂在折算单位「箱」上 —— 收货单必须被拒（手工插入，模拟主数据里
        // 已经配成折算单位的存量物料；主数据接口本卡不动）。
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
         VALUES ('b2b10001-0000-4000-8000-000000000015', 'P2B1-M4-BOX', '整箱垫片（折算单位）',
                 'b2b10001-0000-4000-8000-000000000004', 1, false)
         ON CONFLICT DO NOTHING",
        // P6：两个库位字典（库位账行的 location_id 列指它；无外键，弱引用）
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                                   is_default, capacity, status, sort_order)
         VALUES ('b2b10003-0000-4000-8000-000000000001', 'P2B1-L01', 'P2B1 A 区一号',
                 'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 1),
                ('b2b10003-0000-4000-8000-000000000002', 'P2B1-L02', 'P2B1 A 区二号',
                 'b2b10001-0000-4000-8000-000000000003', 'STORAGE', 0, NULL, 1, 2)
         ON CONFLICT DO NOTHING",
        // P16：作废审批的**发起人**（仓管员，故意不是超管）。
        // 流程节点 `self_approval=FORBID`（内控：审批人 ≠ 发起人），若发起人本身就是超管，
        // 引擎会因为他出现在 ADMIN 候选里直接 422 —— 那不是 bug，是「不许自审自批」的硬拦。
        "INSERT INTO sys_user (id, username, password, real_name)
         VALUES ('b2b10009-0000-4000-8000-000000000001', 'p16_keeper', 'x', 'P16 仓管员')
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

    /// 本夹具的「过账 / 作废」审批实例定位口径（多处复用，必须逐字一致）。
    ///
    /// 三条任一成立即算本夹具的：
    /// 1. `biz_id` 指向本夹具供应商的收货单；
    /// 2. `biz_id` 为空，或指向**已经不存在的收货单** —— 上一轮 `cleanup_fixtures()` 删单时留下的孤儿，
    ///    这正是「每跑一轮全量就多 10 条 `STOCK_IN_POST | PENDING`」的真身
    ///    （实例 `biz_id` 是弱引用，删单据带不走它）；
    /// 3. 按本模块的两条审批流类型（`STOCK_IN_POST` / `STOCK_IN_CANCEL`）。
    fn post_instance_where() -> String {
        format!(
            "biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') AND ( \
                 biz_id IS NULL \
              OR biz_id NOT IN (SELECT id FROM biz_stock_in) \
              OR biz_id IN (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)))"
        )
    }

    /// 清掉本夹具的审批痕迹（**删收货单之前**跑）：实例 / 待办 / 抄送。
    ///
    /// 顺序不可交换（与 transfer / purchase 同一范式）：
    /// 1. `biz_stock_in.approval_instance_id` 是弱引用 → 先置 NULL；
    /// 2. `biz_flow_instance.prev_instance_id` 是**自引用且没有 ON DELETE**（V0122）→ 先解开，
    ///    否则整批 DELETE 被外键拦下、只抛 `query failed: db error`（PG 原文里没有约束名）；
    /// 3. `biz_flow_task` / `biz_flow_cc` 虽 ON DELETE CASCADE，仍显式先删（顺序写死）；
    /// 4. 实例本体（**含孤儿**，见 [`post_instance_where`]）。
    async fn purge_post_approval_traces() {
        let inst_where = post_instance_where();
        let statements = [
            // `ck_biz_si_pending_cancel` 把「PENDING_CANCEL / PENDING_POST ⇒ approval_instance_id 非空」
            // 钉成库级约束 ⇒ 只能给「非待审状态」的单解开弱引用（待审的两类单据在
            // `cleanup_fixtures` 里会被整行删掉，不需要先置 NULL）。
            format!(
                "UPDATE biz_stock_in SET approval_instance_id = NULL \
                  WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid) AND deleted_at IS NULL \
                    AND status NOT IN ('PENDING_CANCEL', 'PENDING_POST')"
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
        ];
        for sql in statements {
            if let Err(err) = db().execute_with_params(&sql, vec![]).await {
                panic!("清理收货审批痕迹失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 清掉本模块造的所有夹具（含 P9 新增的凭证/应付清理）。
    ///
    /// 顺序有讲究：序列号 → 流水 → 库存 → 库位账 → 凭证/应付 → 收货单 → 采购单。
    /// 凭证/应付是**弱引用**（无外键），必须在删收货单之前按 `source_id` 清掉。
    async fn cleanup_fixtures() {
        // 审批实例先清（`biz_id` 是弱引用：删收货单带不走实例，会留下孤儿 PENDING →
        // 每轮多 10~15 条、顶住 `uk_biz_flow_instance_biz_active`）。
        purge_post_approval_traces().await;
        let cleanups = [
            // P16：某条用例会把种子流程临时下线（降级路径），这里无条件恢复，
            // 免得一条用例 panic 之后拖垮后面所有用例
            "UPDATE biz_flow_def SET deleted_at = NULL WHERE flow_code = 'stock_in_cancel'"
                .to_string(),
            // 作废审批实例已由上面的 `purge_post_approval_traces` 一并清掉（含孤儿）。
            // P10：状态日志没有外键，必须在删单据之前按「本夹具的单据 id / 单号」清掉；
            // 同时收掉上一轮删单留下的孤儿（`entity_id` 已指向不存在的收货单 —— 每轮几十行）。
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND ( \
                     entity_id IN (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)) \
                  OR entity_id NOT IN (SELECT id FROM biz_stock_in))"
            ),
            "DELETE FROM biz_status_log WHERE entity_no LIKE 'B1-%'".to_string(),
            format!(
                "DELETE FROM biz_material_serial WHERE source_type = 'PURCHASE' AND source_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
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
              (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)))"
            ),
            format!(
                "DELETE FROM biz_voucher WHERE source_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
            ),
            format!(
                "DELETE FROM biz_payable WHERE source_id IN \
             (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
            ),
            // 作废路径会为凭证 / 应付写状态日志（P10 起所有实体都写），而日志**没有外键**：
            // 单据删了日志还在，每轮多攒几百行永远对不上的孤儿。这里收掉「entity_id 已指向
            // 不存在的凭证 / 应付」的行（活行的日志一条不动）。
            "DELETE FROM biz_status_log WHERE entity_type IN ('PAYABLE', 'VOUCHER') \
                AND entity_id NOT IN (SELECT id FROM biz_payable) \
                AND entity_id NOT IN (SELECT id FROM biz_voucher)"
                .to_string(),
            format!(
                "DELETE FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)"
            ),
            format!(
                "DELETE FROM biz_purchase_order WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)"
            ),
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
            purchase_order_id: None,
            supplier_id: Some(SUPPLIER.to_string()),
            // A4：没有来源采购单 ⇒ 必须显式声明「无源单据」+ 写明来源（AP-5）
            is_sourceless: Some(true),
            warehouse_id: Some(WAREHOUSE.to_string()),
            in_date: None,
            invoice_received: None,
            remark: Some("A4 夹具：无源入库（手工单）".to_string()),
            items: lines,
            total_qty: None,
            total_amount: None,
            total_tax: None,
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

    /// DC-09（P0 单位止血 · 收货单侧）：物料挂折算单位「箱」（1 箱 = 12 个）→ 400 + 折算建议，
    /// 单头与明细**一行都不落库**（过账更是无从发生）。
    ///
    /// 收货单是库存三本账的唯一入口：这里漏掉一行，账上就是「箱数当个数」。
    #[tokio::test]
    async fn non_base_unit_line_is_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // 2 箱 = 24 个
        let error = create_stock_in(&req(vec![line(MATERIAL_BOX, "2", "2", "13")]), None)
            .await
            .unwrap_err();
        assert_eq!(error.status, 400, "{}", error.msg);
        assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
        assert!(
            error.msg.contains(
                "单位「箱」不是基本单位：本版本不支持多单位换算，\
                 请按基本单位录入（1 箱 = 12 个 → 录 24）"
            ),
            "{}",
            error.msg
        );
        assert!(error.msg.contains("items[0]"), "{}", error.msg);
        assert!(error.msg.contains("P2B1-M4-BOX"), "{}", error.msg);

        let rows = db()
            .find_with_params(
                "SELECT COUNT(*) AS cnt FROM biz_stock_in_item WHERE material_id = ?::uuid",
                vec![Value::from(
                    &Uuid::parse_str(MATERIAL_BOX).expect("夹具 UUID"),
                )],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.first().map(|row| row.get_typed::<i64>("cnt").unwrap()),
            Some(0),
            "被拒的收货单不许留下明细行"
        );

        // 混填（基本单位 + 折算单位）整单拒：前一行也不能先落库
        let mixed = create_stock_in(
            &req(vec![
                line(MATERIAL_A, "1", "2", "13"),
                line(MATERIAL_BOX, "1", "2", "13"),
            ]),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(mixed.code, error_code::ERP_UNIT_NOT_BASE, "{}", mixed.msg);
        assert!(mixed.msg.contains("items[1]"), "{}", mixed.msg);

        // 基本单位路径不受影响（同一夹具、同一请求形状）
        create_stock_in(&req(vec![line(MATERIAL_A, "1", "2", "13")]), None)
            .await
            .unwrap();
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

    /// ⑨ POSTED 改单：409。P10 起文案换成统一的「单据已入账」口径（铁律 ④），
    /// 语义不变：已过账的单据不能改任何账务字段。
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
        assert!(err.msg.contains("单据已入账"), "{}", err.msg);
        assert!(err.msg.contains("作废或红冲"), "{}", err.msg);
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
        // D4 文案口径：一句话说清「本次 / 待收 / 已收 / 订单」四个数
        assert!(err.msg.contains("本次收 25 件"), "{}", err.msg);
        assert!(err.msg.contains("只剩 20 件未收"), "{}", err.msg);
        assert!(err.msg.contains("已收 10"), "{}", err.msg);
        assert!(err.msg.contains("订单 30"), "{}", err.msg);

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

    // ==================== DC-03：过账复核来源采购单（ARCH-3 R-P0-3 / §5） ====================

    /// 建一张**带来源采购单**的收货单草稿（一行，收 `qty`，单价 2 / 税率 13%）。
    async fn sourced_doc(order_id: Uuid, item_id: Uuid, qty: &str) -> Uuid {
        let mut only = line(MATERIAL_A, qty, "2", "13");
        only.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![only]);
        request.source_order_id = Some(order_id.to_string());
        create_stock_in(&request, None)
            .await
            .unwrap_or_else(|err| panic!("建单失败：{} {}", err.code, err.msg))
    }

    /// 采购单号（报文断言用）。
    async fn po_no_of(order_id: Uuid) -> String {
        db().scalar_with_params(
            "SELECT order_no FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap()
    }

    /// 过账会动的五处痕迹：库存成本账 / 库位账 / 流水 / 凭证 / 应付。
    ///
    /// 顺序固定为 `[inv, loc, txn, voucher, payable]`；被拒的单**每一处都必须不变**。
    async fn posting_footprint(stock_in_id: Uuid) -> Vec<i64> {
        let material = Uuid::parse_str(MATERIAL_A).unwrap();
        let warehouse = Uuid::parse_str(WAREHOUSE).unwrap();
        let inv: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND qty <> 0",
                vec![Value::from(&material), Value::from(&warehouse)],
            )
            .await
            .unwrap();
        let loc: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_inventory_location \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND qty <> 0",
                vec![Value::from(&material), Value::from(&warehouse)],
            )
            .await
            .unwrap();
        let txn: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id = ?::uuid",
                vec![Value::from(&stock_in_id)],
            )
            .await
            .unwrap();
        let voucher: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
                vec![Value::from(&stock_in_id)],
            )
            .await
            .unwrap();
        let payable: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_payable WHERE source_id = ?::uuid",
                vec![Value::from(&stock_in_id)],
            )
            .await
            .unwrap();
        vec![inv, loc, txn, voucher, payable]
    }

    /// DC-03 单测 A：采购单在「建单」与「过账」之间被取消 → 过账 **409 + 66031**，
    /// 报文点名采购单号与当前状态，且三本账 / 凭证 / 应付 **0 变化**。
    #[tokio::test]
    async fn dc03_posting_rejected_when_source_order_cancelled() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("100", "0").await;
        let order_no = po_no_of(order_id).await;
        let id = sourced_doc(order_id, item_id, "100").await;
        assert_eq!(
            posting_footprint(id).await,
            vec![0, 0, 0, 0, 0],
            "前提：只建单不动任何账"
        );

        // 采购单被取消（模拟另一个人在采购页点了「取消」，与建单不在同一时刻）
        d4_set_po_status(order_id, "CANCELLED").await;

        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .expect_err("采购单已取消，过账必须被拦");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert_eq!(
            err.code,
            error_code::ERP_SI_PO_NOT_RECEIVABLE,
            "{}",
            err.msg
        );
        assert!(
            err.msg.contains(&order_no),
            "报文必须点名采购单号：{}",
            err.msg
        );
        assert!(
            err.msg.contains("已取消") && err.msg.contains("CANCELLED"),
            "报文必须带当前状态：{}",
            err.msg
        );
        assert!(err.msg.contains("本单未过账"), "{}", err.msg);

        assert_eq!(
            posting_footprint(id).await,
            vec![0, 0, 0, 0, 0],
            "被拒后三本账 / 凭证 / 应付都不许留痕迹"
        );
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
        );
        assert_eq!(stock_in_status(id).await, STATUS_DRAFT, "单据必须留在草稿");
        assert_eq!(
            d4_ledger_received(item_id).await,
            Decimal::ZERO,
            "采购明细的已收数量不许动"
        );
        assert_eq!(
            d4_po_status(order_id).await,
            "CANCELLED",
            "采购单状态不许被悄悄推成 PARTIAL_RECEIVED"
        );
    }

    /// DC-03 单测 B：两张收货单**双开**（采购 100，两张各收 60）→ 先过账的成功，
    /// 后过账的 **409 + 66032**，报文给出「只剩 40 件待收」。
    ///
    /// ⚠️ 卡面写的是「建 A（60）→ 建 B（60，采购只剩 40）→ 过账 B」：A 过账后
    /// 采购端只剩 40，B 按 60 建单会被 D4 的建单闸当场 400，**B 根本建不出来**。
    /// 真实可达的双开是「先建两张、各自都按未收 100 通过、再先后过账」——本用例就钉这条。
    #[tokio::test]
    async fn dc03_second_open_receipt_is_rejected_with_available_qty() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("100", "0").await;
        let order_no = po_no_of(order_id).await;
        let a = sourced_doc(order_id, item_id, "60").await;
        let b = sourced_doc(order_id, item_id, "60").await;

        change_stock_in_status(a, &post_req(), None)
            .await
            .unwrap_or_else(|err| panic!("A 过账失败：{} {}", err.code, err.msg));
        assert_eq!(d4_ledger_received(item_id).await, dec("60"));
        let before_b = posting_footprint(b).await;
        assert_eq!(
            before_b,
            vec![1, 1, 0, 0, 0],
            "前提：A 已过账（成本账 / 库位账各 1 行），B 还没有任何痕迹"
        );

        let err = change_stock_in_status(b, &post_req(), None)
            .await
            .expect_err("采购单只剩 40，B 收 60 必须被拦");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_SI_OVER_RECEIVE, "{}", err.msg);
        assert!(err.msg.contains(&order_no), "{}", err.msg);
        assert!(
            err.msg.contains("只剩 40 件待收"),
            "报文必须给出可用待收量：{}",
            err.msg
        );
        assert!(err.msg.contains("订单 100"), "{}", err.msg);
        assert!(err.msg.contains("已过账收货 60"), "{}", err.msg);

        assert_eq!(
            posting_footprint(b).await,
            before_b,
            "被拒的 B 不许留下任何账"
        );
        assert_eq!(stock_in_status(b).await, STATUS_DRAFT);
        assert_eq!(
            d4_ledger_received(item_id).await,
            dec("60"),
            "采购已收只算 A 的 60"
        );
        assert_eq!(inventory_of(MATERIAL_A).await.0, dec("60"));
    }

    /// DC-03 单测 C：正常路径不受影响 —— 采购 100 / 收 100/100 → 过账成功，采购单 RECEIVED。
    ///
    /// 同时钉住「两道闸不打架」：收满之后再建一张就会被**建单闸**（400，不可收）挡住，
    /// 而不是落到过账闸上。
    #[tokio::test]
    async fn dc03_full_receipt_still_posts() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("100", "0").await;
        let id = sourced_doc(order_id, item_id, "100").await;
        change_stock_in_status(id, &post_req(), None)
            .await
            .expect("正常收满 100/100 必须放行");
        assert_eq!(stock_in_status(id).await, STATUS_POSTED);
        assert_eq!(d4_ledger_received(item_id).await, dec("100"));
        assert_eq!(d4_po_status(order_id).await, "RECEIVED");
        assert_eq!(inventory_of(MATERIAL_A).await.0, dec("100"));

        // 采购单已 RECEIVED ⇒ 建单闸（400）先拦，不靠过账闸兜
        let mut another = line(MATERIAL_A, "1", "2", "13");
        another.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![another]);
        request.source_order_id = Some(order_id.to_string());
        let err = create_stock_in(&request, None)
            .await
            .expect_err("已收满的采购单不许再建收货单");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("不能收货"), "{}", err.msg);
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
        // P16：给了原因 → 发起作废审批 → 复核通过 → 才回滚六本账
        cancel(posted, "收错仓了").await;
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
                // 单号尾段必须取 uuid 的**随机段**：`Uuid::now_v7` 字符串前 8 位是毫秒时间戳的
                // 高 32 位，同一个 ~65 秒窗口内所有用例会撞成同一个单号（`uk_biz_po_no` 直接炸）。
                Value::Text(format!("B1-{}", &order_id.simple().to_string()[24..32])),
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
    //
    // ⚠️ P16 起「作废」是**两步**：`POSTED → PENDING_CANCEL`（发起审批）→ 审批通过 →
    // `CANCELLED`（执行六本账回滚）。P9 的这 12 条断言（库存 / 库位账 / 流水 / 凭证 /
    // 序列号全部回滚）**一条都没改**，只是把它们共同的前置「作废」从句柄换成了
    // [`cancel`]（提交 + 复核通过 + 回调），走的是与生产完全相同的路径。
    // 这样新增的「复核」既被覆盖，P9 的回归也仍然钉得住。

    /// 作废请求体。
    fn cancel_req(reason: Option<&str>) -> StockInStatusReq {
        StockInStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: reason.map(str::to_string),
            serials: Vec::new(),
        }
    }

    /// 作废并断言成功 = **发起作废审批 + 复核通过 + 六本账回滚**（P16 生产路径）。
    async fn cancel(id: Uuid, reason: &str) {
        cancel_request(id, reason).await;
        approve_cancel(id).await;
        assert_eq!(
            detail_of(id).await.unwrap().header.status,
            STATUS_CANCELLED,
            "复核通过后单据必须真的作废"
        );
    }

    /// 只**发起**作废审批（不进审批），返回审批单落库后的库存三值快照。
    async fn cancel_request(id: Uuid, reason: &str) {
        change_stock_in_status(id, &cancel_req(Some(reason)), Some(uuid_keeper()))
            .await
            .unwrap_or_else(|err| panic!("发起作废审批失败：{} {}", err.code, err.msg));
    }

    /// 作废审批的发起人（仓管员；非超管 —— 见 `FIXTURES` 的说明）。
    fn uuid_keeper() -> Uuid {
        Uuid::parse_str("b2b10009-0000-4000-8000-000000000001").expect("夹具用户 UUID")
    }

    /// 取某单审批单里第一条 `PENDING` 待办（含审批人）。
    async fn pending_task_of(stock_in_id: Uuid) -> (Uuid, Uuid) {
        let instance_id: Option<Uuid> = db()
            .scalar_with_params(
                "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&stock_in_id)],
            )
            .await
            .expect("查 approval_instance_id");
        let instance_id = instance_id.expect("作废审批中必须有实例 id（ck_biz_si_pending_cancel）");
        let rows = db()
            .find_with_params(
                "SELECT id, assignee_id FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' \
                  ORDER BY created_at ASC",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查作废审批待办");
        let row = rows.first().expect("作废审批必须有 PENDING 待办");
        (
            row.get_typed("id").expect("待办 id"),
            row.get_typed("assignee_id").expect("审批人"),
        )
    }

    /// 复核**通过**：引擎投票 → 提交事务 → 回调收货单（生产同款顺序）。
    async fn approve_cancel(stock_in_id: Uuid) {
        let (task_id, approver) = pending_task_of(stock_in_id).await;
        let tx = db().begin().await.expect("开审批事务");
        let out = engine::approve(&tx, task_id, approver, Some("单测：同意作废"))
            .await
            .expect("引擎同意失败");
        tx.commit().await.expect("提交审批事务");
        crate::business::erp::stockin_bridge::on_cancel_settled(
            out.instance_id,
            &out.instance_status,
        )
        .await;
    }

    /// 复核**驳回**（并终止）：实例落 `REJECTED` → 回执打回已过账。
    async fn reject_cancel(stock_in_id: Uuid, comment: &str) {
        let (task_id, approver) = pending_task_of(stock_in_id).await;
        let tx = db().begin().await.expect("开审批事务");
        let out = engine::reject(&tx, task_id, approver, comment, engine::ReturnTo::Final)
            .await
            .expect("引擎驳回失败");
        tx.commit().await.expect("提交审批事务");
        crate::business::erp::stockin_bridge::on_cancel_settled(
            out.instance_id,
            &out.instance_status,
        )
        .await;
    }

    /// 某单的实例状态（没有实例 → `None`）。
    async fn instance_status_of(stock_in_id: Uuid) -> Option<String> {
        let instance_id: Option<Uuid> = db()
            .scalar_with_params(
                "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&stock_in_id)],
            )
            .await
            .expect("查 approval_instance_id");
        let instance_id = instance_id?;
        flow_model::find_instance(instance_id)
            .await
            .expect("查审批实例")
            .map(|row| row.status)
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
            change_stock_in_status(a, &req_a, Some(uuid_keeper())),
            change_stock_in_status(b, &req_b, Some(uuid_keeper()))
        );
        ra.unwrap_or_else(|err| panic!("A 作废失败：{} {}", err.code, err.msg));
        rb.unwrap_or_else(|err| panic!("B 作废失败：{} {}", err.code, err.msg));
        // P16：六本账回滚发生在**复核通过**那一刻 —— 并发通过两张单，验证的是同一件事
        // （反向冲减按 material_id 升序加锁，两张单不会交叉等待）。
        tokio::join!(approve_cancel(a), approve_cancel(b));

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

    // ==================== P10（铁律 ③④）：入账后只读 + 采购单状态追溯 ====================

    /// ⑧ POSTED 收货单改金额 → 409，文案点「已入账」。
    #[tokio::test]
    async fn p10_posted_amount_change_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "5", "3", "0")]), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("单据已入账"), "{}", err.msg);
        assert!(err.msg.contains("POSTED"), "文案要点名状态：{}", err.msg);
    }

    /// ⑨ POSTED 收货单改数量 → 409。
    #[tokio::test]
    async fn p10_posted_qty_change_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "6", "2", "0")]), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("单据已入账"), "{}", err.msg);
    }

    /// ⑩ POSTED 收货单改明细（换物料）→ 409。
    #[tokio::test]
    async fn p10_posted_items_change_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        let err = update_stock_in(id, &req(vec![line(MATERIAL_B, "5", "2", "0")]), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("明细"), "{}", err.msg);
    }

    /// ⑪ POSTED 收货单只改 remark → 放行（白名单），且真的落库、金额不动。
    #[tokio::test]
    async fn p10_posted_remark_change_is_allowed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        let mut request = req(vec![line(MATERIAL_A, "5", "2", "0")]);
        request.remark = Some("客户急件，先入先出".to_string());
        update_stock_in(id, &request, None)
            .await
            .unwrap_or_else(|err| panic!("只改备注应当放行：{} {}", err.code, err.msg));

        let remark: Option<String> = db()
            .scalar_with_params(
                "SELECT remark FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(remark.as_deref(), Some("客户急件，先入先出"));
        let amount: Decimal = db()
            .scalar_with_params(
                "SELECT total_amount FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(amount, dec("10"), "白名单只放 remark，金额一分不能动");
    }

    /// ⑫ DRAFT 收货单改金额 → 放行（未入账可以改）。
    #[tokio::test]
    async fn p10_draft_amount_change_is_allowed() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
            .await
            .unwrap();
        update_stock_in(id, &req(vec![line(MATERIAL_A, "5", "3", "0")]), None)
            .await
            .unwrap_or_else(|err| panic!("草稿应当能改：{} {}", err.code, err.msg));
        let amount: Decimal = db()
            .scalar_with_params(
                "SELECT total_amount FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(amount, dec("15"));
    }

    /// ⑰ 卡面 §五-3 的验收请求原样跑一遍：`PUT {"total_amount": 999}` → 409。
    ///
    /// 这条专门钉住「未知字段被 serde 静默丢掉 → 200」的假绿：
    /// `CreateStockInReq` 必须显式收下 `total_*`，否则改金额的请求会变成「什么都没改」。
    #[tokio::test]
    async fn p10_posted_total_amount_field_alone_is_conflict() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        // 整个 body 只有 total_amount：明细、表头字段一个都不给
        let payload = CreateStockInReq {
            total_amount: Some(dec("999")),
            ..Default::default()
        };
        let err = update_stock_in(id, &payload, None).await.unwrap_err();
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("单据已入账"), "{}", err.msg);
        assert!(
            err.msg.contains("total_amount"),
            "文案要点名被改的字段：{}",
            err.msg
        );
        // 库里金额一个字节都不能动
        let stored: Decimal = db()
            .scalar_with_params(
                "SELECT total_amount FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(stored, dec("10"));
    }

    /// ⑱ 草稿态直接传合计 → 400（合计只能由明细派生；不是「已入账」问题）。
    #[tokio::test]
    async fn p10_draft_total_amount_direct_is_bad_request() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
            .await
            .unwrap();
        let payload = CreateStockInReq {
            total_amount: Some(dec("999")),
            ..Default::default()
        };
        let err = update_stock_in(id, &payload, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("由明细派生"), "{}", err.msg);
    }

    /// ⑬ 收货过账后采购单变 RECEIVED，`biz_status_log` 一行且 `biz_ref_no` = 收货单号。
    #[tokio::test]
    async fn p10_receipt_advances_order_and_logs_ref() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("30", "10").await;
        let mut line_a = line(MATERIAL_A, "20", "2", "0");
        line_a.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![line_a]);
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        change_stock_in_status(id, &post_req(), None)
            .await
            .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));

        let status: String = db()
            .scalar_with_params(
                "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
                vec![Value::from(&order_id)],
            )
            .await
            .unwrap();
        assert_eq!(status, "RECEIVED");

        let in_no: String = db()
            .scalar_with_params(
                "SELECT in_no FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        let rows = db()
            .find_with_params(
                "SELECT entity_type, from_status, to_status, biz_ref_type, biz_ref_no \
                   FROM biz_status_log WHERE entity_id = ?::uuid ORDER BY created_at",
                vec![Value::from(&order_id)],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "推进一次 = 一行日志（不许自环重复写）");
        let row = &rows[0];
        assert_eq!(
            row.get_typed::<String>("entity_type").unwrap(),
            "PURCHASE_ORDER"
        );
        assert_eq!(row.get_typed::<String>("from_status").unwrap(), "SUBMITTED");
        assert_eq!(row.get_typed::<String>("to_status").unwrap(), "RECEIVED");
        assert_eq!(
            row.get_typed::<Option<String>>("biz_ref_no")
                .unwrap()
                .as_deref(),
            Some(in_no.as_str()),
            "日志必须能追到是哪张收货单推的"
        );
        assert_eq!(
            row.get_typed::<Option<String>>("biz_ref_type")
                .unwrap()
                .as_deref(),
            Some("STOCK_IN")
        );
    }

    /// ⑭ 部分收货 → 采购单变 PARTIAL_RECEIVED。
    #[tokio::test]
    async fn p10_partial_receipt_advances_to_partial_received() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("30", "10").await;
        let mut line_a = line(MATERIAL_A, "10", "2", "0");
        line_a.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![line_a]);
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        change_stock_in_status(id, &post_req(), None).await.unwrap();

        let status: String = db()
            .scalar_with_params(
                "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
                vec![Value::from(&order_id)],
            )
            .await
            .unwrap();
        assert_eq!(status, "PARTIAL_RECEIVED");

        let order_no: String = db()
            .scalar_with_params(
                "SELECT order_no FROM biz_purchase_order WHERE id = ?::uuid",
                vec![Value::from(&order_id)],
            )
            .await
            .unwrap();
        let trail = crate::business::shared::state_machine::trail_of_no(db(), &order_no)
            .await
            .unwrap();
        assert_eq!(trail.len(), 1);
        assert_eq!(trail[0].from_status.as_deref(), Some("SUBMITTED"));
        assert_eq!(trail[0].to_status, "PARTIAL_RECEIVED");
    }

    // ==================== P16：收货单作废审批（卡面 §二，14 条 + 内控 2 条） ====================
    //
    // 全部走**生产同款路径**：`change_stock_in_status`（发起）→ `engine::approve/reject/withdraw`
    // （复核）→ `stockin_bridge::on_cancel_settled`（回调）。夹具里的「发起人」是仓管员
    // （非超管），审批人是库里唯一的 SUPER_ADMIN —— 这正是 `self_approval=FORBID`
    // （内控：审批人 ≠ 发起人）能在真实库里跑起来的前提。

    /// 建 + 过账（`create_by` = 操作人，供内控判据用）。
    #[allow(dead_code)]
    async fn post_new_by(lines: Vec<StockInItemReq>, operator: Option<Uuid>) -> Uuid {
        let id = create_stock_in(&req(lines), operator).await.unwrap();
        change_stock_in_status(id, &post_req(), operator)
            .await
            .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
        id
    }

    /// ① 发起：`POSTED → PENDING_CANCEL` + 生成审批实例 + 回写 `approval_instance_id`，
    /// 而**六本账一动没动**。
    #[tokio::test]
    async fn p16_submit_moves_to_pending_cancel_with_instance() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
        let before = inventory_of(MATERIAL_A).await;
        assert_eq!(before.0, dec("10"), "前置：过账后库存 10");

        cancel_request(id, "供应商召回").await;

        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.header.status, STATUS_PENDING_CANCEL,
            "发起作废只挂审批单，单据进「作废审批中」"
        );
        let instance_id = detail
            .header
            .approval_instance_id
            .clone()
            .expect("必须回写 approval_instance_id");
        assert_eq!(
            detail.header.cancel_reason.as_deref(),
            Some("供应商召回"),
            "申请理由要落库（审批人据此判断）"
        );
        // 状态日志：POSTED → PENDING_CANCEL
        let trail = crate::business::shared::state_machine::trail_of_no(db(), &detail.header.in_no)
            .await
            .unwrap();
        assert_eq!(trail.last().unwrap().to_status, STATUS_PENDING_CANCEL);
        // 六本账一个字节都没动
        assert_eq!(inventory_of(MATERIAL_A).await, before, "发起审批不许动库存");
        assert_eq!(
            txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
            0,
            "发起审批不许写反向流水"
        );
        assert_eq!(voucher_count(id, Some(1)).await, 0, "发起审批不许红冲");
        // 审批中心真的有待办（否则等于没人能复核）
        let todos: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
                vec![Value::from(&Uuid::parse_str(&instance_id).unwrap())],
            )
            .await
            .unwrap();
        assert_eq!(todos, 1, "发起后审批中心必须有 1 条待办");
    }

    /// ② 未配流程 → 降级：直接作废（保持 P16 之前的旧行为），并WARN。
    #[tokio::test]
    async fn p16_no_flow_degrades_to_direct_cancel() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_B, "6", "5", "0")]).await;
        // 把种子流程临时「下线」（干净恢复由 cleanup_fixtures 保证）
        db().execute_with_params(
            "UPDATE biz_flow_def SET deleted_at = now() WHERE flow_code = 'stock_in_cancel'",
            vec![],
        )
        .await
        .unwrap();
        assert!(
            stockin_model::published_cancel_flow()
                .await
                .unwrap()
                .is_none(),
            "流程下线后必须查不到"
        );

        change_stock_in_status(id, &cancel_req(Some("降级直接作废")), Some(uuid_keeper()))
            .await
            .expect("没配流程时作废必须放行（降级），不能把功能锁死");

        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.header.status, STATUS_CANCELLED,
            "降级路径一次到位（旧行为）"
        );
        assert!(
            detail.header.approval_instance_id.is_none(),
            "降级路径不挂审批单"
        );
        assert_eq!(
            inventory_of(MATERIAL_B).await,
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
            "降级路径照样要回滚六本账"
        );
    }

    /// ③ 重复发起 → 409（已有在跑的实例），且不再生成第二条实例。
    #[tokio::test]
    async fn p16_duplicate_submit_is_409() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "4", "2", "0")]).await;
        cancel_request(id, "第一次发起").await;
        let err = change_stock_in_status(id, &cancel_req(Some("第二次发起")), Some(uuid_keeper()))
            .await
            .expect_err("已有在跑的审批单必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("审批中") || err.msg.contains("已有在跑的作废审批"),
            "报文要点明原因与出路：{}",
            err.msg
        );
        let instances: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_CANCEL' AND biz_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        assert_eq!(instances, 1, "不能生成第二条审批实例");
        assert_eq!(
            detail_of(id).await.unwrap().header.status,
            STATUS_PENDING_CANCEL
        );
    }

    /// ④ 作废审批中单据**锁定**：改金额 / 改明细 → 409，数据一个字节都不动。
    #[tokio::test]
    async fn p16_pending_cancel_is_locked() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "8", "2", "0")]).await;
        cancel_request(id, "审批中不许改").await;

        // 改明细（数量从 8 改成 99）
        let mut changed = req(vec![line(MATERIAL_A, "99", "2", "0")]);
        changed.total_amount = Some(dec("198"));
        let err = update_stock_in(id, &changed, Some(uuid_keeper()))
            .await
            .expect_err("作废审批中改明细必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("不可修改") || err.msg.contains("已入账"),
            "文案要说清锁定语义：{}",
            err.msg
        );
        // 只改金额合计
        let mut amount_only = req(vec![line(MATERIAL_A, "8", "2", "0")]);
        amount_only.total_amount = Some(dec("999"));
        let err = update_stock_in(id, &amount_only, Some(uuid_keeper()))
            .await
            .expect_err("作废审批中改金额必须 409");
        assert_eq!(err.status, 409, "{}", err.msg);
        // 库里数据没被改（明细 qty 仍是 8、合计仍是 16）
        let items = stockin_model::items_of(id).await.unwrap();
        assert_eq!(items[0].qty, dec("8"), "锁定期间明细必须原样");
        assert_eq!(detail_of(id).await.unwrap().header.total_amount, dec("16"));
        // 审批中的单子也删不掉（只能先撤回）
        let err = delete_stock_in(id, Some(uuid_keeper()))
            .await
            .expect_err("审批中的单子不能删");
        assert_eq!(err.status, 409, "{}", err.msg);
    }

    /// ⑤ 审批通过 → 单据 `CANCELLED`，**六本账全部回滚**（库存 / 库位账 / 流水 / 凭证 / 应付 / 序列号）。
    #[tokio::test]
    async fn p16_approval_rolls_back_six_ledgers() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let loc = seed_location_row(MATERIAL_SN, LOC_DICT_A).await;
        let id = post_new(vec![line_at(
            MATERIAL_SN,
            "3",
            "100",
            "0",
            &loc.to_string(),
        )])
        .await;
        assert_eq!(
            location_qty_of(&loc).await,
            Some(dec("3")),
            "前置：库位账 3 件"
        );
        assert_eq!(serial_counts_of(id).await.0, 3, "一物一码 3 件在库");
        assert_eq!(voucher_count(id, None).await, 1, "过账生成 1 张凭证");
        let payable_status = |sql: String| async move {
            db().scalar_with_params(&sql, vec![Value::from(&id)])
                .await
                .unwrap()
        };
        let payable_before: String =
            payable_status("SELECT status::text FROM biz_payable WHERE source_id = ?::uuid".into())
                .await;
        assert_eq!(payable_before, "OPEN");

        cancel_request(id, "整批退回").await;
        approve_cancel(id).await;

        // ① 单据
        assert_eq!(detail_of(id).await.unwrap().header.status, STATUS_CANCELLED);
        // ② 库存（成本账）—— 本单带库位，成本账与库位账两本都要归零
        let cost_rows = db()
            .find_with_params(
                "SELECT qty, amount FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id = ?::uuid",
                vec![
                    Value::from(&Uuid::parse_str(MATERIAL_SN).unwrap()),
                    Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                    Value::from(&loc),
                ],
            )
            .await
            .unwrap();
        if let Some(row) = cost_rows.first() {
            assert_eq!(
                row.get_typed::<Decimal>("qty").unwrap(),
                Decimal::ZERO,
                "成本账数量必须归零"
            );
            assert_eq!(
                row.get_typed::<Decimal>("amount").unwrap(),
                Decimal::ZERO,
                "成本账金额必须归零"
            );
        }
        // ③ 库位账
        assert_eq!(location_qty_of(&loc).await, Some(Decimal::ZERO));
        // ④ 流水：一条 OUT / PURCHASE_IN_CANCEL
        assert_eq!(
            txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
            1
        );
        // ⑤ 凭证：多出一张红字
        assert_eq!(voucher_count(id, Some(1)).await, 1, "必须生成红字凭证");
        // ⑥ 应付：作废
        let payable_after: String =
            payable_status("SELECT status::text FROM biz_payable WHERE source_id = ?::uuid".into())
                .await;
        assert_eq!(payable_after, "CANCELLED", "应付单必须联动作废");
        // ⑦ 序列号：3 件全部软删（行保留、审计不丢）
        let (alive, deleted) = serial_counts_of(id).await;
        assert_eq!((alive, deleted), (0, 3), "序列号必须软删（不是物理删）");
    }

    /// ⑥ 审批通过 → 库存回滚金额用**过账流水里的 amount**，不是现价（P9 口径）。
    #[tokio::test]
    async fn p16_approval_uses_posted_amount_not_current_avg() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let first = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
        post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (dec("200"), dec("500"), dec("2.500000"))
        );

        cancel_request(first, "第一张收错了").await;
        // 审批中：账没动
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (dec("200"), dec("500"), dec("2.500000")),
            "审批未通过前库存必须原样"
        );
        approve_cancel(first).await;

        let (qty, amount, _avg) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("100"));
        assert_eq!(amount, dec("300"), "退的是流水原额 200（500-200）");
        assert_ne!(amount, dec("250"), "按现价 2.5×100 是错的");
    }

    /// ⑦ 审批通过 → 生成红字凭证（借贷互换、原凭证打 reversed_by）。
    #[tokio::test]
    async fn p16_approval_generates_red_voucher() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "10", "3", "0")]).await;
        assert_eq!(voucher_count(id, Some(1)).await, 0);
        cancel(id, "退货").await;
        assert_eq!(voucher_count(id, Some(1)).await, 1, "必须有 1 张红字凭证");
        // 红字凭证借贷平衡：Σ借 = Σ贷
        let (debit, credit): (Decimal, Decimal) = db()
            .find_with_params(
                "SELECT COALESCE(SUM(e.debit_amount),0) AS d, COALESCE(SUM(e.credit_amount),0) AS c \
                   FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
                  WHERE v.source_id = ?::uuid AND v.is_red = 1",
                vec![Value::from(&id)],
            )
            .await
            .map(|rows| {
                let row = rows.first().expect("红字凭证分录");
                (
                    row.get_typed::<Decimal>("d").unwrap(),
                    row.get_typed::<Decimal>("c").unwrap(),
                )
            })
            .unwrap();
        assert_eq!(debit, credit, "红字凭证必须借贷平衡");
    }

    /// ⑧ 审批通过 → 序列号软删（行保留，`deleted_at` 置位）。
    #[tokio::test]
    async fn p16_approval_voids_serials() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_SN, "2", "50", "0")]).await;
        assert_eq!(serial_counts_of(id).await, (2, 0));
        cancel_request(id, "整批退回").await;
        assert_eq!(serial_counts_of(id).await, (2, 0), "审批中不许提前动序列号");
        approve_cancel(id).await;
        assert_eq!(serial_counts_of(id).await, (0, 2), "审批通过才软删");
    }

    /// ⑨ 审批驳回 → 单据回到 `POSTED`，**账一点没动**，实例落 `REJECTED`。
    #[tokio::test]
    async fn p16_reject_restores_posted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "12", "2", "0")]).await;
        let before = inventory_of(MATERIAL_A).await;
        cancel_request(id, "想作废").await;
        reject_cancel(id, "不同意，数量再核一遍").await;

        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.header.status, STATUS_POSTED, "驳回必须打回已过账");
        assert!(
            detail.header.approval_instance_id.is_none(),
            "打回后实例引用必须清空（否则下次发起撞唯一索引）"
        );
        assert_eq!(inventory_of(MATERIAL_A).await, before, "驳回不许动账");
        assert_eq!(
            txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
            0
        );
        assert_eq!(voucher_count(id, Some(1)).await, 0);
        let instance_id: Option<Uuid> = db()
            .scalar_with_params(
                "SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_CANCEL' AND biz_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        let instance = flow_model::find_instance(instance_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance.status, "REJECTED");
        assert_eq!(
            instance_status_of(id).await,
            None,
            "打回后单据不该再挂着实例（ck_biz_si_pending_cancel）"
        );
    }

    /// ⑩ 申请人撤回 → 实例 `WITHDRAWN`，单据回到 `POSTED`，账不动。
    #[tokio::test]
    async fn p16_withdraw_restores_posted() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_B, "5", "4", "0")]).await;
        let before = inventory_of(MATERIAL_B).await;
        cancel_request(id, "反悔了").await;
        let instance_id: Option<Uuid> = db()
            .scalar_with_params(
                "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap();
        let instance_id = instance_id.expect("审批中必须有实例");

        withdraw_cancel_approval(id, uuid_keeper())
            .await
            .expect("发起人撤回必须成功");

        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.header.status, STATUS_POSTED);
        assert!(detail.header.approval_instance_id.is_none());
        let instance = flow_model::find_instance(instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance.status, "WITHDRAWN", "撤回要留 WITHDRAWN 轨迹");
        assert_eq!(inventory_of(MATERIAL_B).await, before, "撤回不许动账");
        // 待办不能留成 PENDING（否则就是孤儿待办）
        let pending: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
                vec![Value::from(&instance_id)],
            )
            .await
            .unwrap();
        assert_eq!(pending, 0, "撤回后不许留下 PENDING 待办");
    }

    /// ⑪ 驳回后**可以再次发起**作废（实例已落终态，不撞 uk_ 唯一索引）。
    #[tokio::test]
    async fn p16_resubmit_after_reject() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "6", "2", "0")]).await;
        cancel_request(id, "第一次申请").await;
        reject_cancel(id, "不同意：先跟供应商确认").await;
        cancel_request(id, "第二次申请：已确认").await;
        assert_eq!(
            detail_of(id).await.unwrap().header.status,
            STATUS_PENDING_CANCEL,
            "驳回后必须能再次发起"
        );
        approve_cancel(id).await;
        assert_eq!(
            detail_of(id).await.unwrap().header.status,
            STATUS_CANCELLED,
            "第二次复核通过后必须真作废"
        );
    }

    /// ⑫ 反向：收货单被**软删**（数据修复 / 其它路径）→ 审批实例被回收，不留孤儿待办。
    #[tokio::test]
    async fn p16_soft_deleted_doc_reconciles_instance() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_B, "3", "2", "0")]).await;
        cancel_request(id, "要作废").await;
        let instance_id: Uuid = db()
            .scalar_with_params::<Option<Uuid>>(
                "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .expect("审批中必须有实例");
        // 模拟「单据被别的路径软删」：删单 API 只放行 DRAFT，所以这里直接改库（数据修复口径），
        // 而 `ck_biz_si_pending_cancel` 要求非审批中状态不得挂实例 → 一起清掉引用
        db().execute_with_params(
            "UPDATE biz_stock_in SET deleted_at = now(), status = 'CANCELLED', \
                approval_instance_id = NULL WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();

        let cancelled = crate::business::erp::stockin_bridge::reconcile_orphan_cancels()
            .await
            .expect("孤儿回收必须成功");
        assert!(cancelled >= 1, "必须回收至少 1 条（实际 {cancelled}）");

        let instance = flow_model::find_instance(instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance.status, "CANCELLED", "单据没了，审批单必须终止");
        let pending: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
                vec![Value::from(&instance_id)],
            )
            .await
            .unwrap();
        assert_eq!(pending, 0, "P11 教训：不许留孤儿待办");
    }

    /// ⑬ 反向：审批中单据被**其它路径**改成 `CANCELLED` → 审批实例被回收。
    #[tokio::test]
    async fn p16_other_path_cancel_reconciles_instance() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
        cancel_request(id, "要作废").await;
        let instance_id: Uuid = db()
            .scalar_with_params::<Option<Uuid>>(
                "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .expect("审批中必须有实例");
        // 模拟「别处把单据改成了已作废」：审批单还挂着 PENDING
        db().execute_with_params(
            "UPDATE biz_stock_in SET status = 'CANCELLED', approval_instance_id = NULL, \
                cancelled_at = now() WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();

        crate::business::erp::stockin_bridge::reconcile_orphan_cancels()
            .await
            .expect("孤儿回收必须成功");
        let instance = flow_model::find_instance(instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance.status, "CANCELLED");
        // 回收不许「顺手」回滚六本账（那是审批通过才有的动作）
        assert_eq!(
            inventory_of(MATERIAL_A).await.0,
            dec("5"),
            "回收只终止审批单，正本库存不动"
        );
    }

    /// ⑭ 撤审批失败（非发起人撤回 → 403）→ **整笔回滚**：单据状态与实例都不变。
    #[tokio::test]
    async fn p16_withdraw_failure_rolls_back() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "7", "2", "0")]).await;
        cancel_request(id, "要作废").await;
        let instance_id: Uuid = db()
            .scalar_with_params::<Option<Uuid>>(
                "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .expect("审批中必须有实例");
        // 别人（不是发起人）来撤 → 引擎 403，整笔回滚
        let outsider = Uuid::parse_str("b2b10009-0000-4000-8000-0000000000ff").unwrap();
        let err = withdraw_cancel_approval(id, outsider)
            .await
            .expect_err("非发起人不能撤回");
        assert_eq!(err.status, 403, "{}", err.msg);

        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.header.status, STATUS_PENDING_CANCEL,
            "撤回失败时单据状态必须原样（整笔回滚）"
        );
        assert_eq!(
            detail.header.approval_instance_id.as_deref(),
            Some(instance_id.to_string().as_str()),
            "实例引用也不能被清空"
        );
        let instance = flow_model::find_instance(instance_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(instance.status, "PENDING", "审批单还在跑");
    }

    /// ⑮ 内控：作废复核人**不能是制单人**（`self_approval=FORBID` 硬拦在派单）。
    ///
    /// 构造：制单人是超管（= 唯一的审批候选人），由仓管员代发起 → 引擎派单时命中
    /// 「候选人是发起人？不是；但流程节点 FORBID…」—— 这里断言的是**发起人=超管**的场景：
    /// 超管自己发起作废，候选里就有他自己 → 422 + 65019，单据保持 POSTED，账不动。
    #[tokio::test]
    async fn p16_self_approval_is_forbidden() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let id = post_new(vec![line(MATERIAL_A, "3", "2", "0")]).await;
        // 库里唯一的超管（种子流程的审批候选人）
        let admin: Uuid = db()
            .scalar_with_params::<Option<Uuid>>(
                "SELECT u.id FROM sys_user u \
                   JOIN sys_user_role ur ON ur.user_id = u.id \
                   JOIN sys_role r ON r.id = ur.role_id \
                  WHERE r.role_code = 'SUPER_ADMIN' LIMIT 1",
                vec![],
            )
            .await
            .unwrap()
            .expect("库里必须有超管");

        let err = change_stock_in_status(id, &cancel_req(Some("超管自批试试")), Some(admin))
            .await
            .expect_err("审批人 = 发起人 必须被内控拦下");
        assert_eq!(err.status, 422, "{}", err.msg);
        assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(
            err.msg.contains("内控") && err.msg.contains("复核人"),
            "报文要说清内控口径与出路：{}",
            err.msg
        );
        // 整笔回滚：单据仍是已过账、没有审批单、账没动
        let detail = detail_of(id).await.unwrap();
        assert_eq!(detail.header.status, STATUS_POSTED);
        assert!(detail.header.approval_instance_id.is_none());
        assert_eq!(inventory_of(MATERIAL_A).await.0, dec("3"));
    }

    /// ⑯ 种子流程自洽：`graph_hash = sha256(canonical_json(快照))`（改了快照不改 hash 就该红），
    /// 且审批节点确实是「超管 + 禁自审」。
    #[tokio::test]
    async fn p16_seed_flow_is_self_consistent() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        use crate::business::flow::graph::{canonical_json, parse_snapshot};
        let row = db()
            .find_with_params(
                "SELECT d.flow_code, d.biz_type, d.status, d.is_default, v.graph_hash, \
                        v.graph_snapshot, v.node_count, v.edge_count \
                   FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
                  WHERE d.flow_code = 'stock_in_cancel' AND d.deleted_at IS NULL",
                vec![],
            )
            .await
            .unwrap();
        let row = row.first().expect("V0174 必须种好 stock_in_cancel");
        assert_eq!(
            row.get_typed::<String>("biz_type").unwrap(),
            "STOCK_IN_CANCEL"
        );
        assert_eq!(row.get_typed::<String>("status").unwrap(), "PUBLISHED");
        assert!(row.get_typed::<bool>("is_default").unwrap());
        let stored: String = row.get_typed("graph_hash").unwrap();
        let snapshot: serde_json::Value = row.get_typed("graph_snapshot").unwrap();
        let recomputed = {
            let json = canonical_json(&snapshot);
            // 与 graph::FlowGraph::graph_hash 同一算法（sha256 → 小写 hex）
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(json.as_bytes());
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        assert_eq!(
            stored, recomputed,
            "种子快照与 graph_hash 必须自洽（改了快照不改 hash = 版本漂移）"
        );
        assert_eq!(row.get_typed::<i32>("node_count").unwrap(), 3);
        assert_eq!(row.get_typed::<i32>("edge_count").unwrap(), 2);
        // 快照能被引擎解析（否则发起就 500）
        let snapshot_text = canonical_json(&snapshot);
        let graph = parse_snapshot(&snapshot_text).expect("快照必须可解析");
        let review = graph
            .nodes
            .iter()
            .find(|node| node.node_type == "APPROVAL")
            .expect("必须有审批节点");
        assert_eq!(review.assignee_type.as_deref(), Some("ADMIN"));
        assert_eq!(review.self_approval, "FORBID", "内控：不许自审自批");
    }

    // ==================== D4：采购订单 → 一键生成收货单 ====================

    /// 造一张采购单（1 行，物料 A），返回 `(order_id, item_id)`。
    ///
    /// `received` **只写明细的冗余列**；要造「真的收过货」请用
    /// [`d4_seed_po_with_posted`]（D4 起已收数量有两个独立来源，测试必须让它们一致）。
    async fn d4_seed_po(qty: &str, received: &str) -> (Uuid, Uuid) {
        seed_purchase_order(qty, received).await
    }

    /// 插一张「已过账」的收货单（只写单头 + 一行明细，用于喂 pending 聚合）。
    ///
    /// 不生成凭证 / 库存流水 —— 这些用例只验「待收数量」口径，**不能**跑不变量套件
    /// （D1 要求已过账单必有凭证），所以单测库与 e2e 库分开。
    async fn d4_insert_posted(order_id: Uuid, item_id: Uuid, qty: &str) {
        let id = Uuid::now_v7();
        // 同上：单号取 uuid 随机段，别用时间戳段（同窗口多张单会撞 `uk_biz_si_no`）
        let in_no = format!("B1-IN-{}", &id.simple().to_string()[24..32]);
        db().execute_with_params(
            "INSERT INTO biz_stock_in \
                 (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, total_qty) \
             VALUES (?::uuid, ?, 'PURCHASE', ?::uuid, ?::uuid, ?::uuid, CURRENT_DATE, 'POSTED', \
                 NULLIF(?::text,'')::numeric)",
            vec![
                Value::from(&id),
                Value::Text(in_no),
                Value::from(&order_id),
                Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .unwrap();
        db().execute_with_params(
            "INSERT INTO biz_stock_in_item \
                 (id, stock_in_id, line_no, source_item_id, material_id, unit_id, qty) \
             VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric)",
            vec![
                Value::from(&Uuid::now_v7()),
                Value::from(&id),
                Value::from(&item_id),
                Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
                Value::from(&Uuid::parse_str(UNIT).unwrap()),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .unwrap();
    }

    /// 造采购单 + 真账：`received > 0` 时补一张 POSTED 收货单（明细列与聚合口径一致）。
    async fn d4_seed_po_with_posted(qty: &str, received: &str) -> (Uuid, Uuid) {
        let (order_id, item_id) = d4_seed_po(qty, received).await;
        if dec(received) > Decimal::ZERO {
            d4_insert_posted(order_id, item_id, received).await;
        }
        (order_id, item_id)
    }

    /// 改采购单状态（造 DRAFT / RECEIVED / CANCELLED 这些非默认态）。
    async fn d4_set_po_status(order_id: Uuid, status: &str) {
        db().execute_with_params(
            "UPDATE biz_purchase_order SET status = ? WHERE id = ?::uuid",
            vec![Value::Text(status.to_string()), Value::from(&order_id)],
        )
        .await
        .unwrap();
    }

    /// 读采购单状态。
    async fn d4_po_status(order_id: Uuid) -> String {
        db().scalar_with_params(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap()
    }

    /// 读采购明细的 `received_qty`（冗余列）。
    async fn d4_ledger_received(item_id: Uuid) -> Decimal {
        db().scalar_with_params(
            "SELECT received_qty FROM biz_purchase_order_item WHERE id = ?::uuid",
            vec![Value::from(&item_id)],
        )
        .await
        .unwrap()
    }

    /// ① 全新采购单（没收过）→ `pending_qty = ordered_qty`，且可以收货。
    #[tokio::test]
    async fn d4_pending_fresh_order_equals_ordered() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, _) = d4_seed_po("100", "0").await;
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items.len(), 1);
        assert_eq!(pending.items[0].ordered_qty, dec("100"));
        assert_eq!(pending.items[0].received_qty, dec("0"));
        assert_eq!(
            pending.items[0].pending_qty,
            dec("100"),
            "没收过 → 待收 = 订单"
        );
        assert_eq!(pending.items[0].material_code, "P2B1-M1", "带出物料编码");
        assert_eq!(pending.order.supplier_id, SUPPLIER, "带出供应商（O1）");
        assert_eq!(pending.order.warehouse_id, WAREHOUSE, "带出仓库（O1）");
        assert_eq!(pending.order.supplier_name, "P2B1 测试供应商");
        assert!(pending.can_receive);
        assert_eq!(pending.pending_line_count, 1);
        assert!(pending.blocked_reason.is_none());
    }

    /// ② 部分收货后 → `pending_qty = ordered − received`（只算已过账）。
    #[tokio::test]
    async fn d4_pending_after_partial_receipt() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, _) = d4_seed_po_with_posted("100", "30").await;
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items[0].received_qty, dec("30"));
        assert_eq!(pending.items[0].pending_qty, dec("70"), "待收 = 100 − 30");
        assert!(pending.can_receive);
    }

    /// ③ 全部收完 → 所有行 `pending_qty = 0`，`can_receive = false` + 明确原因。
    #[tokio::test]
    async fn d4_pending_all_received_is_zero() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, _) = d4_seed_po_with_posted("100", "100").await;
        d4_set_po_status(order_id, "RECEIVED").await;
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items[0].pending_qty, Decimal::ZERO);
        assert_eq!(pending.pending_line_count, 0);
        assert!(!pending.can_receive, "收满后前端不该显示「生成收货单」按钮");
        let reason = pending.blocked_reason.expect("必须给出不能收的原因");
        assert!(reason.contains("已收满"), "{reason}");
    }

    /// ④ 多张收货单累加 → `received_qty` 是总和。
    #[tokio::test]
    async fn d4_pending_sums_multiple_receipts() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po("100", "50").await;
        // 明细列写 50，真账拆成两张 POSTED（30 + 20）
        d4_insert_posted(order_id, item_id, "30").await;
        d4_insert_posted(order_id, item_id, "20").await;
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items[0].received_qty, dec("50"), "两张单要累加");
        assert_eq!(pending.items[0].pending_qty, dec("50"));
        assert_eq!(pending.stock_ins.len(), 2, "已生成收货单要能反查");
        assert!(pending.stock_ins.iter().all(|item| item.posted));
    }

    /// ⑤ 草稿收货单**不算**已收（硬红线 4：只有 POSTED 计数）。
    #[tokio::test]
    async fn d4_pending_ignores_draft_stock_in() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, _) = d4_seed_po("100", "0").await;
        // 只给 source_order_id → 自动带出 100 行；**不过账**，停在 DRAFT
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_id.to_string());
        let draft = create_stock_in(&request, None).await.unwrap();
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items[0].received_qty, dec("0"), "草稿不算已收");
        assert_eq!(pending.items[0].pending_qty, dec("100"));
        assert_eq!(pending.stock_ins.len(), 1);
        assert!(!pending.stock_ins[0].posted, "草稿要标成未过账");
        // 过账后才算
        change_stock_in_status(draft, &post_req(), None)
            .await
            .unwrap();
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items[0].received_qty, dec("100"), "过账后才计入");
        assert_eq!(pending.items[0].pending_qty, Decimal::ZERO);
    }

    /// ⑥ 超收 → 400，文案点明「本次 80 > 待收 70（已收 30 / 订单 100）」（O5 / 硬红线 6）。
    #[tokio::test]
    async fn d4_over_receipt_message_is_exact() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po_with_posted("100", "30").await;
        let mut over = line(MATERIAL_A, "80", "2", "0");
        over.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![over]);
        request.source_order_id = Some(order_id.to_string());
        let err = create_stock_in(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("物料 P2B1-M1"), "要点名物料：{}", err.msg);
        assert!(err.msg.contains("本次收 80 件"), "{}", err.msg);
        assert!(err.msg.contains("只剩 70 件未收"), "{}", err.msg);
        assert!(err.msg.contains("已收 30"), "{}", err.msg);
        assert!(err.msg.contains("订单 100"), "{}", err.msg);
    }

    /// ⑦ 采购单是 DRAFT → 400（草稿不能收货）。
    #[tokio::test]
    async fn d4_draft_order_cannot_receive() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, _) = d4_seed_po("10", "0").await;
        d4_set_po_status(order_id, "DRAFT").await;
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_id.to_string());
        let err = create_stock_in(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("DRAFT"), "{}", err.msg);
        assert!(err.msg.contains("不能收货"), "{}", err.msg);
        // 待收接口同样给出 false + 原因
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert!(!pending.can_receive);
        assert!(
            pending
                .blocked_reason
                .unwrap_or_default()
                .contains("不能收货"),
            "待收接口也要说清为什么不能收"
        );
    }

    /// ⑧ 采购单已 RECEIVED → 400（已收完，D4 把 RECEIVED 移出收货白名单）。
    #[tokio::test]
    async fn d4_received_order_cannot_receive() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po_with_posted("100", "100").await;
        d4_set_po_status(order_id, "RECEIVED").await;
        let mut again = line(MATERIAL_A, "1", "2", "0");
        again.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![again]);
        request.source_order_id = Some(order_id.to_string());
        let err = create_stock_in(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("RECEIVED"), "{}", err.msg);
        assert!(err.msg.contains("不能收货"), "{}", err.msg);
    }

    /// ⑨ 采购单已 CANCELLED → 400。
    #[tokio::test]
    async fn d4_cancelled_order_cannot_receive() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, _) = d4_seed_po("10", "0").await;
        d4_set_po_status(order_id, "CANCELLED").await;
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_id.to_string());
        let err = create_stock_in(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("CANCELLED"), "{}", err.msg);
    }

    /// ⑩ 明细的 `source_item_id` 不属于该采购单 → 400（不许跨单收货）。
    #[tokio::test]
    async fn d4_foreign_source_item_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_a, _) = d4_seed_po("10", "0").await;
        let (_order_b, item_b) = d4_seed_po("10", "0").await;
        let mut foreign = line(MATERIAL_A, "5", "2", "0");
        foreign.purchase_order_item_id = Some(item_b.to_string());
        let mut request = req(vec![foreign]);
        request.source_order_id = Some(order_a.to_string());
        let err = create_stock_in(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(
            err.msg.contains("不属于本单的来源采购单"),
            "跨单明细必须点名拒绝：{}",
            err.msg
        );
    }

    /// ⑪ 生成收货单并过账（部分）→ 采购单变 PARTIAL_RECEIVED。
    #[tokio::test]
    async fn d4_post_advances_to_partial_received() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po("100", "0").await;
        let mut partial = line(MATERIAL_A, "30", "2", "0");
        partial.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![partial]);
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        // 生成的是草稿：过账前采购单状态不许变（硬红线 7「不许自动过账」）
        assert_eq!(d4_po_status(order_id).await, "SUBMITTED", "建单不许改状态");
        assert_eq!(d4_ledger_received(item_id).await, dec("0"), "草稿不动已收");
        change_stock_in_status(id, &post_req(), None).await.unwrap();
        assert_eq!(d4_po_status(order_id).await, "PARTIAL_RECEIVED");
        assert_eq!(d4_ledger_received(item_id).await, dec("30"));
    }

    /// ⑫ 收完全部 → 采购单变 RECEIVED。
    #[tokio::test]
    async fn d4_post_all_advances_to_received() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, _) = d4_seed_po("40", "0").await;
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        change_stock_in_status(id, &post_req(), None).await.unwrap();
        assert_eq!(d4_po_status(order_id).await, "RECEIVED");
    }

    /// ⑬ 删除草稿收货单 → 采购单状态与已收数量都不变（草稿从不计入）。
    #[tokio::test]
    async fn d4_delete_draft_does_not_change_order() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po("100", "0").await;
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        delete_stock_in(id, None).await.unwrap();
        assert_eq!(
            d4_po_status(order_id).await,
            "SUBMITTED",
            "删草稿不该动采购单"
        );
        assert_eq!(d4_ledger_received(item_id).await, dec("0"));
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(
            pending.items[0].pending_qty,
            dec("100"),
            "删草稿后待收回到满额"
        );
        assert!(pending.stock_ins.is_empty(), "软删的收货单不该出现在反查里");
    }

    /// ⑭ 作废已过账收货单 → 已收数量回退、采购单从 RECEIVED 退回 PARTIAL_RECEIVED，
    /// 并且**能再收一次**（D4 的核心：收错了还能重来）。
    #[tokio::test]
    async fn d4_cancel_posted_restores_pending() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po("100", "0").await;
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        change_stock_in_status(id, &post_req(), None).await.unwrap();
        assert_eq!(d4_po_status(order_id).await, "RECEIVED");
        assert_eq!(d4_ledger_received(item_id).await, dec("100"));

        cancel(id, "收错了要退").await;
        assert_eq!(
            d4_ledger_received(item_id).await,
            dec("0"),
            "作废必须把已收数量退回去"
        );
        assert_eq!(
            d4_po_status(order_id).await,
            "PARTIAL_RECEIVED",
            "已收货单作废后采购单退回部分收货（否则再也收不进来）"
        );
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items[0].received_qty, dec("0"), "作废的单不算已收");
        assert_eq!(pending.items[0].pending_qty, dec("100"));
        assert!(pending.can_receive, "作废后必须能重新收货");

        // 真的能再收：再建一张并过账
        let mut again = req(Vec::new());
        again.source_order_id = Some(order_id.to_string());
        let second = create_stock_in(&again, None).await.unwrap();
        change_stock_in_status(second, &post_req(), None)
            .await
            .unwrap();
        assert_eq!(d4_po_status(order_id).await, "RECEIVED");
    }

    /// ⑮ `purchase_order_id`（卡面字段名）与 `source_order_id`（库列名）等价。
    #[tokio::test]
    async fn d4_purchase_order_id_alias_links_source() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po("20", "0").await;
        let mut request = req(Vec::new());
        request.purchase_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        let detail = detail_of(id).await.unwrap();
        assert_eq!(
            detail.header.source_order_id.as_deref(),
            Some(order_id.to_string().as_str()),
            "别名要落到 source_order_id 列"
        );
        assert_eq!(
            detail.purchase_order_id.as_deref(),
            Some(order_id.to_string().as_str()),
            "详情响应要给 purchase_order_id（卡面 ①）"
        );
        assert!(
            detail.purchase_order_no.is_some(),
            "详情响应要给 purchase_order_no（卡面 F3）"
        );
        assert_eq!(
            detail.items[0].source_item_id.as_deref(),
            Some(item_id.to_string().as_str())
        );
    }

    /// ⑯ 两个字段都给且不一致 → 400（不静默挑一个）。
    #[tokio::test]
    async fn d4_conflicting_alias_rejected() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_a, _) = d4_seed_po("20", "0").await;
        let (order_b, _) = d4_seed_po("20", "0").await;
        let mut request = req(Vec::new());
        request.source_order_id = Some(order_a.to_string());
        request.purchase_order_id = Some(order_b.to_string());
        let err = create_stock_in(&request, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains("不一致"), "{}", err.msg);
    }

    /// ⑰ 两条「已收」口径必须永远相等（冗余列 vs POSTED 聚合）。
    ///
    /// 这是 D4 的口径锁：以后谁改一侧忘了另一侧，这条先红。
    #[tokio::test]
    async fn d4_pending_matches_ledger_column() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = d4_seed_po("100", "0").await;
        // 两张**部分**收货单：30 → 过账，20 → 过账
        for qty in ["30", "20"] {
            let mut request = req(vec![{
                let mut line = line(MATERIAL_A, qty, "2", "0");
                line.purchase_order_item_id = Some(item_id.to_string());
                line
            }]);
            request.source_order_id = Some(order_id.to_string());
            let id = create_stock_in(&request, None).await.unwrap();
            change_stock_in_status(id, &post_req(), None).await.unwrap();
        }
        let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
            .await
            .unwrap();
        assert_eq!(pending.items.len(), 1);
        let row = &pending.items[0];
        assert_eq!(row.received_qty, dec("50"), "两张单要累加");
        assert_eq!(row.pending_qty, dec("50"));
        let ledger: Decimal = d4_ledger_received(item_id).await;
        assert_eq!(
            row.received_qty, ledger,
            "冗余列（写路径闸）与已过账聚合（读路径）必须一致"
        );
    }

    // ==================== P-FLOW-3A：收货过账审批（8 条） ====================
    //
    // 跑的是**真种子**（V0182 的 `stock_in_post`：5 节点 5 边，含 NOTIFY_NODE 小额免审分支），
    // 不是另造测试图 —— 所以这些用例同时验证「种子能发起 / 免审能直接过 / 阈值落在条件边上」。

    /// 仓管主管角色（V0182 种子里的固定 UUID）。
    const ROLE_WH_MGR: &str = "20000000-0000-4000-8000-0000000f0004";
    /// 仓管主管账号。
    const U_WH_MGR: &str = "b2b1000a-0000-4000-8000-000000000002";
    /// 制单人（P16 夹具里的仓管员，本卡用来验证「自己提自己批」被拦）。
    const U_KEEPER: &str = "b2b10009-0000-4000-8000-000000000001";

    fn uid_of(raw: &str) -> Uuid {
        Uuid::parse_str(raw).expect("夹具 UUID 非法")
    }

    async fn sql(text: &str) {
        if let Err(err) = db().execute_with_params(text, vec![]).await {
            panic!("SQL 失败：{err}\nSQL: {text}");
        }
    }

    /// 过账审批夹具：仓管主管账号 + 角色绑定 + 清掉本卡夹具的审批实例 / 通知。
    async fn setup_post_approval() {
        // 仓管主管角色是共享资源：只留本卡夹具的人（否则「派给谁」不可判定）
        sql(&format!(
            "DELETE FROM sys_user_role WHERE role_id = '{ROLE_WH_MGR}'::uuid \
                AND user_id <> '{U_WH_MGR}'::uuid"
        ))
        .await;
        sql(&format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES ('{U_WH_MGR}'::uuid, 'p3a_wh_mgr', 'x', 'p3a 仓管主管', 1) \
             ON CONFLICT (id) DO NOTHING"
        ))
        .await;
        sql(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) \
             VALUES ('{U_WH_MGR}'::uuid, '{ROLE_WH_MGR}'::uuid) ON CONFLICT DO NOTHING"
        ))
        .await;
        for sql_text in [
            // 通知接收人是硬外键 → 必须先删接收人再删通知（顺序反了会 FK 违约）。
            // ⚠️ `sys_notice.biz_id` 是 **varchar**（V0043），必须 `id::text` 才能和它比。
            format!(
                "DELETE FROM sys_notice_receiver WHERE notice_id IN \
                   (SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
                     (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
                        AND biz_id IN \
                       (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))))"
            ),
            format!(
                "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
                   (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
                      AND biz_id IN \
                     (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)))"
            ),
            format!(
                "DELETE FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' AND biz_id IN \
                   (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
            ),
        ] {
            sql(&sql_text).await;
        }
    }

    /// 造一张含税金额 = `amount` 的收货单（单行、税率 0）。
    async fn stock_in_with_amount(amount: &str, maker: Option<&str>) -> Uuid {
        create_stock_in(
            &req(vec![line(MATERIAL_A, "1", amount, "0")]),
            maker.map(uid_of),
        )
        .await
        .unwrap_or_else(|err| panic!("造收货单失败：{} {}", err.code, err.msg))
    }

    async fn running_post_instance(stock_in_id: Uuid) -> Uuid {
        db().scalar_with_params::<Option<Uuid>>(
            "SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
              AND biz_id = ?::uuid AND status = 'PENDING' LIMIT 1",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .unwrap()
        .expect("应当有在跑的过账审批实例")
    }

    async fn post_pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
        let rows = db()
            .find_with_params(
                "SELECT id, assignee_id FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY assignee_id, id",
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
    /// P0-FIX 之后「制单人 / 发起人」不再拿 `PENDING`（生成时就被排除成 `SKIPPED`）。
    async fn post_task_of(instance_id: Uuid, assignee: Uuid) -> Option<(Uuid, String, String)> {
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

    /// 以某人身份同意一条待办，并按 `flow::handler` 的口径推进业务回调。
    async fn post_approve_as(
        task_id: Uuid,
        operator: &str,
    ) -> crate::business::flow::engine::ActionOut {
        let op = uid_of(operator);
        let out = db()
            .transaction(|tx| async move {
                crate::business::flow::engine::approve(&tx, task_id, op, Some("同意")).await
            })
            .await
            .unwrap_or_else(|err| panic!("同意失败：{err}"));
        stockin_bridge::on_post_settled(out.instance_id, &out.instance_status).await;
        out
    }

    async fn stock_in_status(id: Uuid) -> String {
        stockin_model::find_stock_in(id)
            .await
            .unwrap()
            .expect("收货单不存在")
            .status
    }

    /// ⑨ 提交过账 → PENDING_POST + 实例 + 六本账一分没动。
    #[tokio::test]
    async fn pflow3a_si_submit_post_pending_without_ledger() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("5000", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap_or_else(|err| panic!("提交过账失败：{} {}", err.code, err.msg));

        assert_eq!(stock_in_status(id).await, "PENDING_POST");
        let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert!(row.approval_instance_id.is_some(), "必须回写审批实例");
        let instance = running_post_instance(id).await;
        let tasks = post_pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1, "5000 元走仓管主管审批");
        assert_eq!(tasks[0].1, uid_of(U_WH_MGR));
        // 六本账一分没动
        assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO, "库存未加");
        let txns: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(txns, 0, "未过账不许写流水");
        let vouchers: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(vouchers, 0, "未过账不许出凭证");
    }

    /// ⑩ 500 元（< 1000）→ 免审直接过账 + 通知仓管主管（NOTIFY_NODE）。
    #[tokio::test]
    async fn pflow3a_si_small_amount_auto_posts_and_notifies() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("500", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap_or_else(|err| panic!("提交过账失败：{} {}", err.code, err.msg));

        // 免审：同一请求里就走完了过账
        assert_eq!(stock_in_status(id).await, "POSTED", "小额免审必须直接过账");
        assert_eq!(inventory_of(MATERIAL_A).await.0, dec("1"), "库存已加");
        // 实例已结束（NOTIFY 节点不阻塞）
        let instance: Uuid = db()
            .scalar_with_params::<Option<Uuid>>(
                "SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
                  AND biz_id = ?::uuid ORDER BY started_at DESC LIMIT 1",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .expect("实例应当存在");
        let status: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "APPROVED", "免审分支实例应自动走完");
        // 知会通知（sys_notice.biz_type = FLOW_NOTIFY，收件人 = 仓管主管）
        let notices: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM sys_notice WHERE biz_id = ? AND biz_type = 'FLOW_NOTIFY'",
                vec![Value::Text(instance.to_string())],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notices, 1, "免审必须发一条知会通知");
        let receivers: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM sys_notice_receiver r JOIN sys_notice n ON n.id = r.notice_id \
                  WHERE n.biz_id = ? AND n.biz_type = 'FLOW_NOTIFY'",
                vec![Value::Text(instance.to_string())],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receivers, 1, "通知要扇出给仓管主管（1 人）");
    }

    /// ⑪ 5000 元 → 走仓管主管审批（待办派给角色成员）。
    #[tokio::test]
    async fn pflow3a_si_5000_needs_manager_approval() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("5000", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap();
        let instance = running_post_instance(id).await;
        let tasks = post_pending_tasks(instance).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].1, uid_of(U_WH_MGR), "5000 元派给仓管主管");
        assert_eq!(stock_in_status(id).await, "PENDING_POST");
    }

    /// ⑫ 审批通过 → POSTED + 六本账全动（库存 / 流水 / 凭证 / 应付 / 库位账）。
    #[tokio::test]
    async fn pflow3a_si_approve_posts_all_books() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("5000", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap();
        let tasks = post_pending_tasks(running_post_instance(id).await).await;
        post_approve_as(tasks[0].0, U_WH_MGR).await;

        assert_eq!(stock_in_status(id).await, "POSTED");
        let (qty, amount, _) = inventory_of(MATERIAL_A).await;
        assert_eq!(qty, dec("1"), "库存数量");
        assert_eq!(amount, dec("5000"), "库存成本（不含税）");
        let txns: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(txns, 1, "库存流水 1 条");
        let vouchers: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(vouchers, 1, "过账即出凭证");
        let payables: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_payable WHERE source_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(payables, 1, "过账即生成应付单");
        let loc_qty: Decimal = db()
            .scalar_with_params::<Option<Decimal>>(
                "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location WHERE material_id = ?::uuid",
                vec![Value::from(&uid_of(MATERIAL_A))],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loc_qty, dec("1"), "库位账与成本账相等");
    }

    /// ⑬ 审批驳回 → 回 DRAFT + 账一分没动；打回后还能重新提交。
    #[tokio::test]
    async fn pflow3a_si_reject_back_to_draft_without_ledger() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("5000", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap();
        let tasks = post_pending_tasks(running_post_instance(id).await).await;
        let op = uid_of(U_WH_MGR);
        let out = db()
            .transaction(|tx| async move {
                crate::business::flow::engine::reject(
                    &tx,
                    tasks[0].0,
                    op,
                    "数量与送货单不符，退回核对",
                    crate::business::flow::engine::ReturnTo::Final,
                )
                .await
            })
            .await
            .unwrap();
        stockin_bridge::on_post_settled(out.instance_id, &out.instance_status).await;

        assert_eq!(stock_in_status(id).await, "DRAFT", "驳回必须打回草稿");
        assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO, "账没动");
        let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert!(row.approval_instance_id.is_none(), "打回要清空审批实例");
        // 打回后能重新提交
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .expect("打回后必须能重新提交过账审批");
        assert_eq!(stock_in_status(id).await, "PENDING_POST");
    }

    /// ⑭ PENDING_POST 状态改金额 / 数量 → 409（锁单据）。
    #[tokio::test]
    async fn pflow3a_si_pending_post_is_locked() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("5000", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap();
        assert_eq!(stock_in_status(id).await, "PENDING_POST");

        let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "9", "5000", "0")]), None)
            .await
            .expect_err("审批中的单据不许改金额 / 明细");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(
            err.msg.contains("PENDING_POST") || err.msg.contains("不可修改"),
            "{}",
            err.msg
        );
        // 状态流转也不能「直接过账」绕过审批
        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .expect_err("审批中不许直接过账");
        assert_eq!(err.status, 409, "{}", err.msg);
        assert!(err.msg.contains("过账审批中"), "{}", err.msg);
        // 单据没被改动
        let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(row.total_with_tax, dec("5000"));
        assert_eq!(row.status, "PENDING_POST");
    }

    /// ⑮ 审批中删除收货单 → 同一事务撤审批（不留孤儿待办，P11）。
    #[tokio::test]
    async fn pflow3a_si_delete_cancels_pending_approval() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("5000", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap();
        let instance = running_post_instance(id).await;

        delete_stock_in(id, None)
            .await
            .unwrap_or_else(|err| panic!("删除失败：{} {}", err.code, err.msg));

        let status: String = db()
            .scalar_with_params::<Option<String>>(
                "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, "CANCELLED", "删单必须终止审批实例");
        let pending: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
                vec![Value::from(&instance)],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending, 0, "不许留孤儿待办");
        assert!(
            stockin_model::find_stock_in(id).await.unwrap().is_none(),
            "单据已软删"
        );
    }

    /// ⑯ 制单人自批 → 403 + 65019。
    ///
    /// P0-FIX 之后语义是「**生成待办时就排除**」：制单人（也是仓管主管）拿到的是
    /// `SKIPPED` 留痕行，不是 `PENDING`；403 兜底仍在（防绕过）。
    #[tokio::test]
    async fn pflow3a_si_maker_cannot_approve_own_post() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        // 制单人 = 仓管主管本人（自己提自己批）
        sql(&format!(
            "INSERT INTO sys_user_role (user_id, role_id) \
             VALUES ('{U_KEEPER}'::uuid, '{ROLE_WH_MGR}'::uuid) ON CONFLICT DO NOTHING"
        ))
        .await;
        let id = stock_in_with_amount("5000", Some(U_KEEPER)).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .unwrap();
        let instance = running_post_instance(id).await;
        let (mine, status, comment) = post_task_of(instance, uid_of(U_KEEPER))
            .await
            .expect("制单人必须留痕（不删待办，只改状态）");
        assert_eq!(
            status, "SKIPPED",
            "P0-FIX：制单人的票在生成时就被内控排除，不进 PENDING"
        );
        assert!(comment.contains("内控"), "留痕要写原因：{comment}");
        assert!(
            post_pending_tasks(instance)
                .await
                .iter()
                .all(|(_, who)| *who != uid_of(U_KEEPER)),
            "制单人不该出现在「待批」清单里（他点不动）"
        );

        let err = stockin_bridge::guard_self_approval(mine, uid_of(U_KEEPER))
            .await
            .expect_err("自批必须被拦");
        assert_eq!(err.status, 403, "{}", err.msg);
        assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
        assert!(
            err.msg.contains("不能审批自己提交的过账申请"),
            "{}",
            err.msg
        );
        // 他人可正常审批
        stockin_bridge::guard_self_approval(mine, uid_of(U_WH_MGR))
            .await
            .expect("他人可以正常审批");
        sql(&format!(
            "DELETE FROM sys_user_role WHERE user_id = '{U_KEEPER}'::uuid \
              AND role_id = '{ROLE_WH_MGR}'::uuid"
        ))
        .await;
    }

    // ==================== D9：复制为新单（收货单） ====================
    //
    // 每个用例都断言「原单一个字节都不改」的至少一个切面；`mutation` 这条用例做整行对照。

    /// 复制用的假令牌：`permissions` 决定 `has_permission`（与框架判定同口径）。
    fn copy_claims(perms: &[&str]) -> TokenClaims {
        TokenClaims {
            jti: None,
            sub: "018f4b2c-d9d9-7d9d-8d9d-000000000001".to_string(),
            username: Some("d9-copier".to_string()),
            roles: Vec::new(),
            permissions: perms.iter().map(|p| (*p).to_string()).collect(),
            token_type: None,
            exp: 0,
            iat: 0,
            extra: std::collections::HashMap::new(),
        }
    }

    /// 只给「建单」一个码的令牌：证明「复制权限 = create 权限」在业务分支里也成立。
    fn create_only() -> TokenClaims {
        copy_claims(&[COPY_PERMISSION])
    }

    /// 原单整行快照（用于「复制不改原单」的整行对照）。
    ///
    /// 用 `to_jsonb(t)` 把整行拍成文本：比逐列断言更狠 —— 以后加了新列也自动纳入对照。
    /// 取出来的是**同一行前后两次**的文本，键序恒定，所以可以逐字比。
    async fn header_snapshot(id: Uuid) -> String {
        db().scalar_with_params(
            "SELECT (to_jsonb(t))::text FROM biz_stock_in t WHERE t.id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .expect("查原单快照失败")
    }

    /// ① 复制一张已过账收货单：新单号 + `DRAFT` + 明细/金额/入库日期口径一致。
    #[tokio::test]
    async fn d9_stockin_copy_creates_draft_with_new_no_and_same_items() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let origin_id = post_new(vec![
            line(MATERIAL_A, "10", "2", "13"),
            line(MATERIAL_B, "5", "1.5", "0"),
        ])
        .await;
        let origin = detail_of(origin_id).await.unwrap();
        assert_eq!(origin.header.status, STATUS_POSTED);

        let new_id = copy_stock_in(origin_id, &create_only())
            .await
            .expect("复制应成功");
        let copied = detail_of(new_id).await.unwrap();

        assert_ne!(copied.header.id, origin.header.id);
        assert_ne!(copied.header.in_no, origin.header.in_no, "单号必须是新的");
        assert!(
            copied.header.in_no.starts_with("IN-"),
            "{}",
            copied.header.in_no
        );
        assert_eq!(copied.header.status, STATUS_DRAFT, "复制出来的必须是草稿");
        assert_eq!(copied.header.status_label, "草稿");
        assert_eq!(copied.header.supplier_id, origin.header.supplier_id);
        assert_eq!(copied.header.warehouse_id, origin.header.warehouse_id);
        assert_eq!(copied.header.total_qty, origin.header.total_qty);
        assert_eq!(copied.header.total_amount, origin.header.total_amount);
        assert_eq!(copied.header.total_tax, origin.header.total_tax);
        assert_eq!(copied.header.total_with_tax, origin.header.total_with_tax);
        assert_eq!(
            copied.header.invoice_received,
            origin.header.invoice_received
        );
        assert_eq!(copied.items.len(), origin.items.len());
        for (new_line, old_line) in copied.items.iter().zip(origin.items.iter()) {
            assert_eq!(new_line.material_id, old_line.material_id);
            assert_eq!(new_line.qty, old_line.qty);
            assert_eq!(new_line.price, old_line.price);
            assert_eq!(new_line.tax_rate, old_line.tax_rate);
            assert_eq!(new_line.amount, old_line.amount);
            assert_eq!(new_line.location_id, old_line.location_id);
        }
        // 入库日期口径：复制是新的入库动作 → 今天（不沿用原单日期）
        assert_eq!(
            copied.header.in_date,
            crate::business::erp::stockin_dto::today_cst()
        );
        // 原单没被动：仍是已过账
        let origin_after = detail_of(origin_id).await.unwrap();
        assert_eq!(origin_after.header.status, STATUS_POSTED);
        assert_eq!(origin_after.header.in_no, origin.header.in_no);
    }

    /// ② 复制带来源采购单的收货单 → 新单 `purchase_order_id`（`source_order_id`）清空。
    #[tokio::test]
    async fn d9_stockin_copy_clears_purchase_order_id() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("30", "0").await;
        let mut only = line(MATERIAL_A, "10", "2", "0");
        only.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![only]);
        request.source_order_id = Some(order_id.to_string());
        let origin_id = create_stock_in(&request, None).await.unwrap();
        let origin = detail_of(origin_id).await.unwrap();
        assert_eq!(
            origin.header.source_order_id.as_deref(),
            Some(order_id.to_string().as_str()),
            "前提：原单确实引用了采购单"
        );

        let new_id = copy_stock_in(origin_id, &create_only()).await.unwrap();
        let copied = detail_of(new_id).await.unwrap();
        assert!(
            copied.header.source_order_id.is_none(),
            "卡面决策 B：新单必须清空采购关联（否则待收额度被二次占用），实际 {:?}",
            copied.header.source_order_id
        );
        assert!(copied.header.source_order_no.is_none());
        assert!(
            copied
                .items
                .iter()
                .all(|line| line.source_item_id.is_none()),
            "明细行的 source_item_id 也必须清空"
        );
        // 原单的关联原样保留（复制不改原单）
        let origin_after = detail_of(origin_id).await.unwrap();
        assert_eq!(
            origin_after.header.source_order_id,
            origin.header.source_order_id
        );
        assert_eq!(
            origin_after.items[0].source_item_id,
            origin.items[0].source_item_id
        );
    }

    /// ③ 已过账原单（有凭证 + 应付）复制后：凭证 / 应付 / 审批实例 / 过账时间全部为空。
    #[tokio::test]
    async fn d9_stockin_copy_clears_voucher_payable_and_approval_links() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let origin_id = post_new(vec![line(MATERIAL_A, "10", "2", "13")]).await;
        let origin = detail_of(origin_id).await.unwrap();
        assert!(
            origin.header.voucher_id.is_some(),
            "前提：过账要生成进项凭证"
        );
        assert!(origin.header.payable_id.is_some(), "前提：过账要生成应付单");

        let new_id = copy_stock_in(origin_id, &create_only()).await.unwrap();
        let copied = detail_of(new_id).await.unwrap();
        assert!(copied.header.voucher_id.is_none(), "凭证关联必须清空");
        assert!(copied.header.payable_id.is_none(), "应付关联必须清空");
        assert!(
            copied.header.approval_instance_id.is_none(),
            "审批实例必须清空"
        );
        assert!(copied.header.posted_at.is_none(), "过账时间必须清空");
        assert!(copied.header.posted_by.is_none());
        assert!(copied.header.cancelled_at.is_none());
        assert!(copied.header.cancel_reason.is_none());
        assert!(copied.header.created_at >= origin.header.created_at);
        // 库里再核一遍（详情 DTO 万一漏字段，这条兜住）：用 `IS NULL` 判据，
        // 免得依赖驱动的时间类型映射
        let rows = db()
            .find_with_params(
                "SELECT status, (voucher_id IS NULL) AS v_null, (payable_id IS NULL) AS p_null, \
                        (approval_instance_id IS NULL) AS a_null, (posted_at IS NULL) AS posted_null, \
                        (posted_by IS NULL) AS poster_null, (cancelled_at IS NULL) AS cancel_null \
                   FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&new_id)],
            )
            .await
            .unwrap();
        let row = rows.first().expect("新单必须存在");
        assert_eq!(row.get_typed::<String>("status").unwrap(), STATUS_DRAFT);
        assert!(
            row.get_typed::<bool>("v_null").unwrap(),
            "voucher_id 必须 NULL"
        );
        assert!(
            row.get_typed::<bool>("p_null").unwrap(),
            "payable_id 必须 NULL"
        );
        assert!(
            row.get_typed::<bool>("a_null").unwrap(),
            "approval_instance_id 必须 NULL"
        );
        assert!(
            row.get_typed::<bool>("posted_null").unwrap(),
            "posted_at 必须 NULL"
        );
        assert!(row.get_typed::<bool>("poster_null").unwrap());
        assert!(row.get_typed::<bool>("cancel_null").unwrap());
    }

    /// ⑥ 原单引用的采购单已被物理删除 → 复制照常成功（采购关联本来就清空）。
    #[tokio::test]
    async fn d9_stockin_copy_works_when_source_purchase_order_is_gone() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = seed_purchase_order("10", "0").await;
        let mut only = line(MATERIAL_A, "10", "2", "0");
        only.purchase_order_item_id = Some(item_id.to_string());
        let mut request = req(vec![only]);
        request.source_order_id = Some(order_id.to_string());
        let origin_id = create_stock_in(&request, None).await.unwrap();

        // 采购单物理删除（`source_order_id` 是弱引用，没有外键挡）
        sql(&format!(
            "DELETE FROM biz_purchase_order_item WHERE order_id = '{order_id}'::uuid"
        ))
        .await;
        sql(&format!(
            "DELETE FROM biz_purchase_order WHERE id = '{order_id}'::uuid"
        ))
        .await;

        let new_id = copy_stock_in(origin_id, &create_only())
            .await
            .expect("采购单没了也要能复制（复制根本不读它）");
        let copied = detail_of(new_id).await.unwrap();
        assert!(copied.header.source_order_id.is_none());
        assert!(copied.header.source_order_no.is_none());
        assert_eq!(copied.items.len(), 1);
        assert_eq!(copied.items[0].qty, dec("10"));
    }

    /// ⑦ 原单没有明细 → 400，且文案点明「没有明细」。
    #[tokio::test]
    async fn d9_stockin_copy_without_items_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let origin_id = create_stock_in(&req(vec![line(MATERIAL_A, "3", "2", "0")]), None)
            .await
            .unwrap();
        // 造「空壳单」：把明细删掉（库层没有「至少一行」的约束）
        sql(&format!(
            "DELETE FROM biz_stock_in_item WHERE stock_in_id = '{origin_id}'::uuid"
        ))
        .await;

        let err = copy_stock_in(origin_id, &create_only())
            .await
            .expect_err("没有明细必须 400");
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("没有任何明细"), "文案要点名：{}", err.msg);
        // 一张新单都不许建出来
        let count: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_stock_in WHERE supplier_id = ?::uuid",
                vec![Value::from(&Uuid::parse_str(SUPPLIER).unwrap())],
            )
            .await
            .unwrap();
        assert_eq!(count, 1, "失败时不能留下半张单");
    }

    /// ⑧ 连点三下 → 三张不同单号的草稿（取号器每次都取新号）。
    #[tokio::test]
    async fn d9_stockin_copy_three_times_yields_three_distinct_nos() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let origin_id = post_new(vec![line(MATERIAL_A, "2", "2", "0")]).await;
        let mut nos = Vec::new();
        for _ in 0..3 {
            let new_id = copy_stock_in(origin_id, &create_only()).await.unwrap();
            let detail = detail_of(new_id).await.unwrap();
            assert_eq!(detail.header.status, STATUS_DRAFT);
            nos.push(detail.header.in_no);
        }
        nos.sort();
        nos.dedup();
        assert_eq!(nos.len(), 3, "三次复制必须三个不同单号：{nos:?}");
    }

    /// ⑨ 复制不改原单：整行快照 + 明细行 id / 数量前后完全一致。
    #[tokio::test]
    async fn d9_stockin_copy_does_not_touch_origin() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let origin_id = post_new(vec![line(MATERIAL_A, "4", "2.5", "13")]).await;
        let before_header = header_snapshot(origin_id).await;
        let before_items: Vec<(Uuid, i32, Decimal)> = db()
            .find_with_params(
                "SELECT id, line_no, qty FROM biz_stock_in_item WHERE stock_in_id = ?::uuid \
                  ORDER BY line_no",
                vec![Value::from(&origin_id)],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.get_typed::<Uuid>("id").unwrap(),
                    row.get_typed::<i32>("line_no").unwrap(),
                    row.get_typed::<Decimal>("qty").unwrap(),
                )
            })
            .collect();
        let before_updated: String = db()
            .scalar_with_params(
                "SELECT updated_at::text FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&origin_id)],
            )
            .await
            .unwrap();

        copy_stock_in(origin_id, &create_only()).await.unwrap();
        copy_stock_in(origin_id, &create_only()).await.unwrap();

        assert_eq!(
            header_snapshot(origin_id).await,
            before_header,
            "原单整行必须一个字节都不变（含 updated_at）"
        );
        let after_updated: String = db()
            .scalar_with_params(
                "SELECT updated_at::text FROM biz_stock_in WHERE id = ?::uuid",
                vec![Value::from(&origin_id)],
            )
            .await
            .unwrap();
        assert_eq!(after_updated, before_updated, "原单 updated_at 不许变");
        let after_items: Vec<(Uuid, i32, Decimal)> = db()
            .find_with_params(
                "SELECT id, line_no, qty FROM biz_stock_in_item WHERE stock_in_id = ?::uuid \
                  ORDER BY line_no",
                vec![Value::from(&origin_id)],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.get_typed::<Uuid>("id").unwrap(),
                    row.get_typed::<i32>("line_no").unwrap(),
                    row.get_typed::<Decimal>("qty").unwrap(),
                )
            })
            .collect();
        assert_eq!(
            after_items, before_items,
            "原单明细行必须原样（id 都不许变）"
        );
    }

    /// ⑩ 权限：只有 `:list` 码 → 403 且文案点名 create；复制守的码与建单逐字相同。
    #[tokio::test]
    async fn d9_stockin_copy_requires_create_permission() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        assert_eq!(
            COPY_PERMISSION, "business:erp:stock-in:create",
            "硬红线 5：复制权限必须 = 建单权限码"
        );
        let origin_id = create_stock_in(&req(vec![line(MATERIAL_A, "1", "2", "0")]), None)
            .await
            .unwrap();

        let no_create = copy_claims(&["business:erp:stock-in:list"]);
        let err = copy_stock_in(origin_id, &no_create)
            .await
            .expect_err("没有建单权限必须 403");
        assert_eq!(err.status, 403, "{}", err.msg);
        assert!(
            err.msg.contains("business:erp:stock-in:create"),
            "403 文案要点名权限码：{}",
            err.msg
        );
        // 放宽成「有 create」→ 放行（证明差异只在那一个码）
        let with_create = copy_claims(&["business:erp:stock-in:list", COPY_PERMISSION]);
        copy_stock_in(origin_id, &with_create)
            .await
            .expect("有 create 码就应放行");
    }

    // ==================== A4：入库单改绑采购单（供应商从采购单派生 + 无源标记） ====================
    //
    // 卡面 t_a8aa8c71 验收 A 的 11 条断言在这里逐条落地。全部走**生产同一条代码路径**
    // （`create_stock_in` / `update_stock_in` / `change_stock_in_status` / `detail_of` /
    // `stock_in_page`），只跳过认证 —— 单测能调的路径就是 HTTP 走的那条。
    //
    // 夹具设计的关键：A4 用**第二个供应商** [`A4_SUPPLIER_B`] 建采购单，
    // 而请求里不带 `supplier_id`。如果供应商不是真从采购单派生，落库值会是空的或另一个值，
    // 断言立刻红 —— 用同一个供应商就测不出「派生」。

    /// 造一张**指定供应商 / 指定状态**的采购单（1 行物料 A），返回 `(order_id, item_id)`。
    async fn a4_seed_po(supplier: &str, qty: &str, status: &str) -> (Uuid, Uuid) {
        let order_id = Uuid::now_v7();
        let item_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_purchase_order \
                 (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty, \
                  total_amount, total_tax, total_with_tax) \
             VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?::uuid, ?, \
                 NULLIF(?::text,'')::numeric, 0, 0, 0)",
            vec![
                Value::from(&order_id),
                // 单号取 uuid 的随机段（同毫秒多张单不会撞 uk_biz_po_no）
                Value::Text(format!("B1-A4-{}", &order_id.simple().to_string()[24..32])),
                Value::from(&Uuid::parse_str(supplier).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                Value::Text(status.to_string()),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .expect("A4 夹具：采购单");
        db().execute_with_params(
            "INSERT INTO biz_purchase_order_item \
                 (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
                  amount, tax_amount, with_tax_amount, received_qty) \
             VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, \
                 2, 2, 0, NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric, 0)",
            vec![
                Value::from(&item_id),
                Value::from(&order_id),
                Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
                Value::from(&Uuid::parse_str(UNIT).unwrap()),
                Value::Text(qty.to_string()),
                Value::Text(qty.to_string()),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .expect("A4 夹具：采购明细");
        (order_id, item_id)
    }

    /// 从采购单生成收货单的请求（`supplier` = `None` 时证明服务端派生）。
    fn a4_sourced_req(
        order_id: Uuid,
        item_id: Uuid,
        qty: &str,
        supplier: Option<&str>,
    ) -> CreateStockInReq {
        let mut one = line(MATERIAL_A, qty, "2", "0");
        one.purchase_order_item_id = Some(item_id.to_string());
        CreateStockInReq {
            source_order_id: Some(order_id.to_string()),
            supplier_id: supplier.map(str::to_string),
            warehouse_id: Some(WAREHOUSE.to_string()),
            items: vec![one],
            ..Default::default()
        }
    }

    /// 采购单当前状态（A4 用例读回写结果用）。
    async fn a4_po_status(order_id: Uuid) -> String {
        db().scalar_with_params(
            "SELECT status::text FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .expect("查采购单状态")
    }

    /// ① + ⑥ + ⑨：有源单 → 创建成功，供应商**从采购单派生**（请求里根本没给），
    /// 且明细 `source_item_id` 是从采购单带出的。
    #[tokio::test]
    async fn a4_01_purchase_with_source_derives_supplier() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;

        let id = create_stock_in(&a4_sourced_req(order_id, item_id, "4", None), None)
            .await
            .unwrap_or_else(|err| panic!("有源单应当建得出来：{} {}", err.code, err.msg));

        let row = stockin_model::find_stock_in(id)
            .await
            .unwrap()
            .expect("单头");
        assert_eq!(
            row.supplier_id,
            Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap()),
            "供应商必须从采购单派生（请求里没传 supplier_id）"
        );
        assert_eq!(row.source_order_id, Some(order_id), "主关联 = 采购单");
        assert!(!row.is_sourceless, "有源单不能被打成无源");

        let items = stockin_model::items_of(id).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].source_item_id,
            Some(item_id),
            "明细的来源行必须是从采购单带出的 source_item_id（只绑上一环）"
        );
        assert_eq!(items[0].qty, dec("4"));
    }

    /// ②：PURCHASE 型却没绑采购单、也没声明无源 → 400 + 66027。
    ///
    /// 这条是 AP-4 的机械保障：入库绑的是**采购单**，不是供应商 ——
    /// 客户端再也不能「填个供应商就建单」。
    #[tokio::test]
    async fn a4_02_purchase_without_source_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let payload = CreateStockInReq {
            supplier_id: Some(SUPPLIER.to_string()),
            is_sourceless: None,
            warehouse_id: Some(WAREHOUSE.to_string()),
            items: vec![line(MATERIAL_A, "1", "2", "0")],
            ..Default::default()
        };
        let err = create_stock_in(&payload, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_SI_SOURCE_REQUIRED);
        assert!(
            err.msg.contains("来源采购单") && err.msg.contains("is_sourceless"),
            "文案要说清「要么绑采购单、要么显式声明无源」：{}",
            err.msg
        );
    }

    /// ③：来源采购单不存在 / 已软删 → 400（66002 引用缺失）。
    #[tokio::test]
    async fn a4_03_source_order_missing_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
        db().execute_with_params(
            "UPDATE biz_purchase_order SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .expect("软删采购单");

        let err = create_stock_in(&a4_sourced_req(order_id, item_id, "1", None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
        assert!(err.msg.contains("采购单不存在或已删除"), "{}", err.msg);
    }

    /// ④：采购单状态不允许收货（DRAFT / CANCELLED / CLOSED）→ 400。
    ///
    /// ⚠️ 与卡面的**一处口径差异**（已写进完成说明）：卡面写「状态允许收货
    /// （APPROVED / PARTIAL_RECEIVED）」，本仓 `purchase_receivable` 的白名单是
    /// `SUBMITTED | APPROVED | PARTIAL_RECEIVED`（D4 定稿；ERP-B2 的内控开关默认关，
    /// SUBMITTED 是「还没走审批」的常态）。把 SUBMITTED 判死会让「建单 → 收货」这条主链路
    /// 在默认配置下直接断掉，所以本卡**保留** SUBMITTED 可收，只把终态 / 草稿态判死。
    #[tokio::test]
    async fn a4_04_source_order_not_receivable_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        for status in ["DRAFT", "CANCELLED", "CLOSED"] {
            let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", status).await;
            let err = create_stock_in(&a4_sourced_req(order_id, item_id, "1", None), None)
                .await
                .err()
                .unwrap_or_else(|| panic!("采购单状态 {status} 不允许收货，却建单成功了"));
            assert_eq!(err.status, 400, "{}", err.msg);
            assert!(
                err.msg.contains("不能收货"),
                "文案要说清「这张采购单现在不能收货」：{}",
                err.msg
            );
        }
    }

    /// ⑤：客户端传的供应商 ≠ 采购单供应商 → 400 + 66028（**不静默覆盖**）。
    /// 传相同的值 → 幂等放行（不是错误）。
    #[tokio::test]
    async fn a4_05_client_supplier_mismatch_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;

        let err = create_stock_in(
            &a4_sourced_req(order_id, item_id, "1", Some(SUPPLIER)),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_SI_SUPPLIER_MISMATCH);
        assert!(
            err.msg.contains("由采购单带出") && err.msg.contains("不允许手工填写"),
            "文案要说清「从采购单带出、别手填」：{}",
            err.msg
        );

        // 传对（与采购单一致）→ 幂等放行，落库仍是采购单那边的值
        let id = create_stock_in(
            &a4_sourced_req(order_id, item_id, "1", Some(A4_SUPPLIER_B)),
            None,
        )
        .await
        .unwrap_or_else(|err| panic!("传对供应商不该报错：{} {}", err.code, err.msg));
        let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(
            row.supplier_id,
            Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap())
        );
    }

    /// ⑦ + ⑧：无源单据必须**显式**打标记 + 写原因；有源单不许标无源。
    #[tokio::test]
    async fn a4_06_sourceless_requires_flag_and_reason() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        // ① 无源 + 没有标记 → 400（走的是「必须绑采购单」那条）
        let no_flag = CreateStockInReq {
            supplier_id: Some(SUPPLIER.to_string()),
            warehouse_id: Some(WAREHOUSE.to_string()),
            items: vec![line(MATERIAL_A, "1", "2", "0")],
            ..Default::default()
        };
        let err = create_stock_in(&no_flag, None).await.unwrap_err();
        assert_eq!(err.code, error_code::ERP_SI_SOURCE_REQUIRED, "{}", err.msg);

        // ② 无源 + 有标记但没写原因 → 400（AP-5：无源要说清「哪来的」）
        let no_reason = CreateStockInReq {
            supplier_id: Some(SUPPLIER.to_string()),
            is_sourceless: Some(true),
            warehouse_id: Some(WAREHOUSE.to_string()),
            items: vec![line(MATERIAL_A, "1", "2", "0")],
            ..Default::default()
        };
        let err = create_stock_in(&no_reason, None).await.unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_SI_SOURCELESS_CONFLICT);
        assert!(err.msg.contains("remark"), "{}", err.msg);

        // ③ 无源 + 标记 + 原因 → 成功，落库 is_sourceless = true
        let ok = CreateStockInReq {
            remark: Some("期初补录：2026-10 前的手工库存".to_string()),
            ..no_reason
        };
        let id = create_stock_in(&ok, None)
            .await
            .unwrap_or_else(|err| panic!("无源单应当建得出来：{} {}", err.code, err.msg));
        let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert!(row.is_sourceless, "无源标记必须落库");
        assert_eq!(row.source_order_id, None);
        assert_eq!(row.supplier_id, Some(Uuid::parse_str(SUPPLIER).unwrap()));

        // ④ 有源单却声明无源 → 以**来源单为准**归一化（不拒单、也不静默改供应商）：
        //    标记本来就等价于「没有来源单」，多带的 true 是冗余而非谎言，落库仍是
        //    is_sourceless = false + 供应商来自采购单。这条把「归一化」钉成显式行为，
        //    免得将来有人把它改成「静默信任客户端的标记」。
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
        let mut mixed = a4_sourced_req(order_id, item_id, "1", None);
        mixed.is_sourceless = Some(true);
        mixed.remark = Some("自相矛盾的请求".to_string());
        let id = create_stock_in(&mixed, None)
            .await
            .unwrap_or_else(|err| panic!("有源单多带无源标记不该拒单：{} {}", err.code, err.msg));
        let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert!(!row.is_sourceless, "有源单落库必须是无源 = false");
        assert_eq!(
            row.supplier_id,
            Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap()),
            "供应商仍取自采购单"
        );
    }

    /// ⑦ + ⑧（DB 层）：非 PURCHASE 型入库单带着供应商 → 撞 `ck_biz_si_supplier_scope`；
    /// 无源却不打标记 → 撞 `ck_biz_si_sourceless_pair`。
    ///
    /// 为什么用裸 SQL 而不是 DTO：`create` 路径把 `in_type` 硬编码成 `'PURCHASE'`，
    /// 接口层根本造不出 `RETURN` / `OTHER`（A4 不扩枚举，设计 §4.4 要求「未来新增枚举值」
    /// 而不是放宽现有约束）。这两条约束必须由 DB 兜住，才挡得住绕过接口的写入。
    #[tokio::test]
    async fn a4_07_non_purchase_with_supplier_db_rejects() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let insert = |in_type: &'static str, sourceless: bool, with_supplier: bool| {
            let supplier = if with_supplier {
                format!("'{SUPPLIER}'::uuid")
            } else {
                "NULL".to_string()
            };
            format!(
                "INSERT INTO biz_stock_in (id, in_no, in_type, supplier_id, warehouse_id, in_date, \
                     status, total_qty, total_amount, total_tax, total_with_tax, is_sourceless) \
                 VALUES (gen_random_uuid(), 'B1-A4-NEG-{in_type}-{sourceless}-{with_supplier}', \
                     '{in_type}', {supplier}, '{WAREHOUSE}'::uuid, CURRENT_DATE, 'DRAFT', \
                     0, 0, 0, 0, {sourceless})"
            )
        };

        // ① 非 PURCHASE 带供应商 → 拒（A3 的 ck_biz_si_supplier_scope）
        let err = db()
            .execute_with_params(&insert("RETURN", true, true), vec![])
            .await
            .expect_err("非采购入库带供应商必须被 DB 拒");
        assert!(
            format!("{err}").contains("ck_biz_si_supplier_scope"),
            "应当撞 ck_biz_si_supplier_scope：{err}"
        );

        // ② 无源却不打标记（任何 in_type）→ 拒（本卡的 ck_biz_si_sourceless_pair）
        let err = db()
            .execute_with_params(&insert("OTHER", false, false), vec![])
            .await
            .expect_err("无源单据不打标记必须被 DB 拒");
        assert!(
            format!("{err}").contains("ck_biz_si_sourceless_pair"),
            "应当撞 ck_biz_si_sourceless_pair：{err}"
        );

        // ③ 一行都不许落库
        let left: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'B1-A4-NEG-%'",
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(left, 0, "反例一行都不该落库");
    }

    /// ⑩：超收（本次数量 > 采购单待收）→ 400（D4 的老校验还在）。
    #[tokio::test]
    async fn a4_08_over_receipt_is_400() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "5", "SUBMITTED").await;

        // ① 一次就要 6 件（采购单只有 5）→ 400
        let err = create_stock_in(&a4_sourced_req(order_id, item_id, "6", None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(
            err.msg.contains("未收") && err.msg.contains("件"),
            "文案要说清本次 / 待收 / 已收 / 订单：{}",
            err.msg
        );

        // ② 先收 3 并**过账**（待收 5 → 2），再要 3 → 400（待收是按已过账数量算的）
        let first = create_stock_in(&a4_sourced_req(order_id, item_id, "3", None), None)
            .await
            .expect("收 3 件合法");
        change_stock_in_status(first, &post_req(), None)
            .await
            .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
        let err = create_stock_in(&a4_sourced_req(order_id, item_id, "3", None), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(
            err.msg.contains("未收"),
            "过账后待收只剩 2 件，再收 3 件必须被拒：{}",
            err.msg
        );
    }

    /// ⑪：列表下发 `source_order_no` 快照 + `supplier_name`（读主数据）+ `is_sourceless`；
    /// 详情另外下发**一跳带出**的合同号（不 JOIN 合同表）。
    #[tokio::test]
    async fn a4_09_list_and_detail_expose_snapshot_columns() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
        let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
            .await
            .expect("建单");
        let po_no = stockin_model::order_no_of(order_id)
            .await
            .unwrap()
            .expect("采购单号");

        let page = stock_in_page(Query(StockInPageQuery {
            supplier_id: Some(A4_SUPPLIER_B.to_string()),
            page: 1,
            page_size: 20,
            ..Default::default()
        }))
        .await
        .expect("列表应能查");
        let list = page.data.expect("有数据").list;
        assert_eq!(list.len(), 1, "本供应商下就这一张单");
        assert_eq!(
            list[0].source_order_no.as_deref(),
            Some(po_no.as_str()),
            "列表必须下发来源采购单号快照"
        );
        assert_eq!(
            list[0].supplier_name.as_deref(),
            Some("A4 二号供应商"),
            "供应商名读 supplier_id 快照关联主数据（不是实时 JOIN 采购单）"
        );
        assert!(!list[0].is_sourceless);

        let detail = detail_of(id).await.expect("详情");
        assert_eq!(
            detail.header.supplier_name.as_deref(),
            Some("A4 二号供应商")
        );
        assert_eq!(detail.purchase_order_no.as_deref(), Some(po_no.as_str()));
        // 这张采购单是 SPOT（没挂合同）→ 一跳带出为空，且**绝不**去 JOIN biz_contract
        assert_eq!(detail.contract_no, None);
    }

    /// 过账前复校（设计 §7.2）：建单后有人直接改库把供应商换掉 → 过账 400，
    /// 库存一分没动（应付单取的就是单据头供应商，放过就是记错账）。
    #[tokio::test]
    async fn a4_10_posting_rechecks_supplier() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
        let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
            .await
            .expect("建单");

        // 模拟「绕过接口直接改库」
        db().execute_with_params(
            "UPDATE biz_stock_in SET supplier_id = ?::uuid WHERE id = ?::uuid",
            vec![
                Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
                Value::from(&id),
            ],
        )
        .await
        .expect("模拟改库");

        let err = change_stock_in_status(id, &post_req(), None)
            .await
            .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_SI_SUPPLIER_MISMATCH);
        assert!(err.msg.contains("直接改库"), "{}", err.msg);
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (dec("0"), dec("0"), dec("0")),
            "复校拦下后库存必须一分没动"
        );
    }

    /// 全链路（卡面 §5）：有源建单 → 过账（库存 + 流水 + 凭证 + 采购回写 + 状态推进）→
    /// 作废（账与采购数量全部回基线）。这条就是 A4 最怕被改坏的链。
    #[tokio::test]
    async fn a4_11_post_and_cancel_keep_purchase_chain() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
        let id = create_stock_in(&a4_sourced_req(order_id, item_id, "6", None), None)
            .await
            .expect("建单");

        change_stock_in_status(id, &post_req(), None)
            .await
            .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));

        // ① 库存增加
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (dec("6"), dec("12"), dec("2.000000")),
            "过账后库存 6 件 / 12 元 / 单价 2"
        );
        // ② 流水 1 条（PURCHASE_IN）
        assert_eq!(txn_count(id, "PURCHASE_IN").await, 1, "过账写一条入库流水");
        // ③ 凭证生成（1 张，非红冲）
        assert_eq!(voucher_count(id, None).await, 1, "过账生成 1 张凭证");
        assert_eq!(voucher_count(id, Some(1)).await, 0, "此刻不该有红冲凭证");
        // ④ 采购明细已收数量 + 采购单状态推进（部分收货）
        assert_eq!(
            db().scalar_with_params::<Decimal>(
                "SELECT received_qty FROM biz_purchase_order_item WHERE id = ?::uuid",
                vec![Value::from(&item_id)],
            )
            .await
            .unwrap(),
            dec("6"),
            "过账必须回写采购明细 received_qty"
        );
        assert_eq!(a4_po_status(order_id).await, "PARTIAL_RECEIVED");

        // 作废 → 账与采购数量全部回基线（ERP 铁律）
        cancel(id, "A4 全链路：作废回基线").await;
        assert_eq!(
            inventory_of(MATERIAL_A).await,
            (dec("0"), dec("0"), dec("0")),
            "作废后库存必须回基线"
        );
        assert_eq!(
            db().scalar_with_params::<Decimal>(
                "SELECT received_qty FROM biz_purchase_order_item WHERE id = ?::uuid",
                vec![Value::from(&item_id)],
            )
            .await
            .unwrap(),
            dec("0"),
            "作废必须把采购明细的已收数量退回去"
        );
        assert_eq!(
            a4_po_status(order_id).await,
            "PARTIAL_RECEIVED",
            "退回后采购单回到「部分收货」（RECEIVED → PARTIAL_RECEIVED 是 D4 的合法边）"
        );
    }

    /// 复制为新单：`source_order_id` 被清空 ⇒ 必须打成无源，并在备注里留「复制自 IN-xxx」。
    #[tokio::test]
    async fn a4_12_copy_marks_sourceless() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
        let origin = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
            .await
            .expect("建单");
        let origin_no = stockin_model::find_stock_in(origin)
            .await
            .unwrap()
            .unwrap()
            .in_no;

        let copy_id = copy_stock_in(origin, &create_only()).await.expect("复制");
        let copy = stockin_model::find_stock_in(copy_id)
            .await
            .unwrap()
            .unwrap();
        assert!(copy.is_sourceless, "复制单没有来源单 ⇒ 必须是无源单");
        assert_eq!(copy.source_order_id, None);
        assert_eq!(
            copy.supplier_id,
            Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap())
        );
        assert!(
            copy.remark
                .as_deref()
                .unwrap_or_default()
                .contains(&origin_no),
            "备注要留「复制自 {origin_no}」：{:?}",
            copy.remark
        );
    }

    /// 草稿改单也不能把供应商单独换成别的（有源单只能跟着采购单走）。
    #[tokio::test]
    async fn a4_13_draft_update_cannot_hand_pick_supplier() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
        let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
            .await
            .expect("建单");

        let err = update_stock_in(
            id,
            &a4_sourced_req(order_id, item_id, "2", Some(SUPPLIER)),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400, "{}", err.msg);
        assert_eq!(err.code, error_code::ERP_SI_SUPPLIER_MISMATCH);
        let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
        assert_eq!(
            row.supplier_id,
            Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap()),
            "被拒之后库里的供应商一个字节都不许动"
        );
    }

    // ---------- ⑬ 收尾清场：模块级 teardown（`zz_` 排在本模块最后） ----------

    /// 本模块跑完**不许留**「STOCK_IN_POST 实例 / 待办 / 状态日志 / 夹具收货单」四类残留。
    ///
    /// 为什么必须有这一条：`setup()` 只在**每个用例开头**跑，模块最后一个用例的产物没人收；
    /// 同一库连跑第二遍时，遗留的 `STOCK_IN_POST | PENDING` 会顶住
    /// `uk_biz_flow_instance_biz_active`（下一次提交永久 409），`biz_status_log` 则每轮多攒几十行。
    /// libtest 按名字排序，`zz_` 必然最后。
    ///
    /// 顺带钉住「孤儿也算本夹具的」这条口径：本用例造的单被清场删掉后，实例
    /// （`biz_id` 是弱引用）必须**跟着一起**被 `purge_post_approval_traces` 收走 ——
    /// 这正是「每跑一轮多 10 条 PENDING」的结构性缺口。
    #[tokio::test]
    async fn zz_teardown_purges_post_approval_residue() {
        let _db = match setup().await {
            Some(guard) => guard,
            None => return,
        };
        setup_post_approval().await;
        let id = stock_in_with_amount("5000", None).await;
        submit_post(id, &post_req(), Some(uid_of(U_KEEPER)))
            .await
            .expect("提交过账审批应成功");
        let instance_id = running_post_instance(id).await;
        assert!(
            !post_pending_tasks(instance_id).await.is_empty(),
            "清场前必须有待办，否则这条用例证明不了什么"
        );

        // 走模块**真实**的收尾路径（`cleanup_fixtures`：先审批痕迹 → 状态日志 → 单据本体），
        // 而不是在用例里另写一套 —— 这样这条用例证明的就是「模块收尾真的不留残留」。
        cleanup_fixtures().await;
        // ① 实例没了（**孤儿也认**：单据已删，实例仍必须被收走）
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
        // ② 状态日志没了（无外键 → 必须在删单据之前清）
        let logs: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'STOCK_IN' AND entity_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志")
            .unwrap_or(0);
        assert_eq!(logs, 0, "清场后状态日志必须为 0");
        // ③ 全库不剩「在跑的过账实例」—— 顶住唯一索引的就是它
        let running: i64 = db()
            .scalar_with_params::<Option<i64>>(
                "SELECT count(*) FROM biz_flow_instance \
                  WHERE biz_type = 'STOCK_IN_POST' AND status = 'PENDING' AND deleted_at IS NULL",
                vec![],
            )
            .await
            .expect("查在跑实例")
            .unwrap_or(0);
        assert_eq!(
            running, 0,
            "模块收尾后不许剩 PENDING 的过账审批实例（同库连跑第二遍的前提）"
        );
    }
}

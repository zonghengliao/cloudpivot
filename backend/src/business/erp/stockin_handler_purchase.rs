// ============================================================================
// stockin_handler_purchase.rs —— 采购联动：来源单解析 / 供应商守卫 / 行回填 / 过账前复查
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_common::*;
use crate::business::erp::model;
use crate::business::erp::stockin_dto::CreateStockInReq;
use crate::business::erp::stockin_model::{self};
use crate::business::erp::unit_guard;
use crate::shared::error_code;
use crate::utils;

/// 取采购明细行 + 它所属的采购单 id。
pub(super) async fn po_item_of(
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
pub(super) enum PurchaseOrderFault {
    /// 状态不在可收白名单（`purchase_receivable`）：`DRAFT` / `RECEIVED` / `CLOSED` / `CANCELLED`。
    NotReceivable,
    /// 收货仓库与采购单的收货仓库不一致。
    WarehouseMismatch,
}

/// 来源采购单的可收判据（**唯一实现**）：`None` = 可收。
pub(super) fn purchase_order_fault(
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
pub(super) async fn fetch_purchase_order(
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
pub(super) async fn require_purchase_order(
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
pub(super) async fn require_purchase_order_at_post(
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
pub(super) fn purchase_status_label(status: &str) -> &'static str {
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
pub(super) struct ParentPointer {
    /// 落库的供应商（有源 = 采购单的；无源 = 用户显式填的）。
    pub(super) supplier_id: Uuid,
    /// 是否无源单据（AP-5：期初 / 复制 / 其他，必须显式）。
    pub(super) is_sourceless: bool,
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
pub(super) async fn resolve_parent_pointer(
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
pub(super) async fn guard_posting_supplier(
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
/// 口径：只取 `qty - received_qty > 0` 的行（`received_qty` 是派生值，P2-3），
/// 单价 / 税率沿用采购明细（含税单价也照搬快照），
/// `source_item_id` 落该采购行 id —— 过账时按这一列判超收（`order_item_balances`）。
pub(super) async fn pull_lines_from_purchase(
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

// P2-3：原来这里还有 `purchase_item_snapshot(order_id)`，给过账 ⑤ 步拼
// 「采购 X、已收 Y、超出 Z」的超收文案（`source_item_id → (qty, received_qty)`）。
// 列删除后 ⑤ 步一并消失，超收拦截只剩
// `stockin_handler_purchase::guard_purchase_before_post` 一处，
// 文案直接带 `order_item_balances` 的订单量 / 已过账收货量 —— 少一份快照，
// 也少一处「快照与实时值不一致」的可能。

// ==================== ① 建单 ====================

/// 把 `purchase_order_id`（卡面 D4 的字段名）归一成 `source_order_id`（库里列名）。
///
/// 两个都给且**不相等** → 400：静默挑一个会让「带错单」这类事故无声无息。
/// 归一后写回 `request.source_order_id`，后面的代码只认一个字段（不留双真相）。
pub(super) fn resolve_purchase_order(
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
pub(super) async fn guard_purchase_before_post(
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

/// ★ DC-09 补口（缺口 1）：过账**前**按**明细快照的 `unit_id`** 逐行复检单位。
///
/// 建单 / 改单期的 `unit_guard::ensure_base_unit` 判的是「物料现在的单位」，
/// 挡不住两类单：**升级前建的折算单位草稿**、**绕过接口 `UPDATE biz_stock_in_item.unit_id`**。
/// 而 `post_in_tx` 只认明细快照的 `unit_id`，`qty` 直接进三本账 —— 漏一次就是「5 箱记成 5 个」。
///
/// 位置：`posting_inputs` 之后、`db().begin()` **之前**（六本账一动没动就被拦下），
/// 与 A4 的 [`guard_posting_supplier`] 同一范式；两条过账入口（直接过账 / 审批通过）
/// 都调它，不会只堵一条。
///
/// # 异常情况
///
/// 数据库异常 → [`ApiError`]（`500`）；非基本单位 → `400 + ERP_UNIT_NOT_BASE`。
pub(super) async fn guard_posting_units(
    items: &[stockin_model::StockInItemRow],
) -> std::result::Result<(), ApiError> {
    for (index, item) in items.iter().enumerate() {
        unit_guard::ensure_unit_row_is_base(
            db(),
            item.unit_id,
            Some(item.material_id),
            index,
            item.qty,
        )
        .await?;
    }
    Ok(())
}

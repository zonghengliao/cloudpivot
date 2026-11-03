// ============================================================================
// stockin_handler_common.rs —— 底座：错误构造 / 行计划 / 快照与派生总额 / 锁与权限判定
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_purchase::*;
use crate::business::erp::form_snapshot;
use crate::business::erp::model;
use crate::business::erp::stockin_dto::{
    CreateStockInReq, PreparedStockIn, StockInDetailRes, StockInItemReq, StockInItemRes, StockInRes,
};
use crate::business::erp::stockin_model::{self, NewStockInLine};
use crate::business::erp::unit_guard;
use crate::business::flow::{engine, model as flow_model};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 共用辅助 ====================

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
pub(super) fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
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
pub(super) fn parse_optional_uuid(
    raw: Option<&String>,
    field: &str,
) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(|text| text.trim()) {
        None | Some("") => Ok(None),
        Some(text) => parse_uuid(text, field).map(Some),
    }
}

/// 操作人（`create_by` / `posted_by`）。
pub(super) fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

/// 不存在统一 404。
pub(super) fn not_found(what: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{what}不存在或已删除"),
    )
}

/// 状态冲突统一 409。
pub(super) fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 弱引用缺失 / 数据不一致统一 400（`66002`）。
pub(super) fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// 参数类 400（`10001`）。
pub(super) fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 数量文案：去掉尾部的 0（`70.0000` → `70`，`1.5000` → `1.5`）。
///
/// 库列是 `NUMERIC(18,4)`，直接 `{}` 打印会变成「70.0000 件」——
/// 给老板看的错误文案不该带这种噪声。
pub(super) fn trim_qty(value: Decimal) -> String {
    value.round_dp(4).normalize().to_string()
}

/// 作废路径的「状态不允许」哨兵归类（模型层只回 `DbError`，这里映射成 `409` 文案）。
pub(super) enum CancelGuard {
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
pub(super) fn cancel_write_error(err: DbError, ctx: &str) -> ApiError {
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
pub(super) const STOCK_IN_LABEL: &str = "收货入库单";

/// 提交审批撞 `uk_biz_flow_instance_biz_active` → `409 + 本单号`（不是「单号重复、去查 biz_seq」）。
///
/// 判据与文案收在 [`flow_model::is_biz_active_duplicate`] / [`flow_model::approval_in_progress`]，
/// 与调拨单等兄弟模块同一份实现。只在**拿得到单号的入口**（发起过账 / 发起作废审批）用它；
/// 其余出口走 [`write_error`] 的兜底分支（文案退化成「该收货入库单」）。
pub(super) fn submit_conflict(err: DbError, ctx: &str, in_no: &str) -> ApiError {
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
pub(super) fn write_error(err: DbError, ctx: &str) -> ApiError {
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
pub(super) struct LinePlan {
    /// 待落库的明细。
    pub(super) new: NewStockInLine,
}

/// 把校验后的 DTO 明细变成可落库的明细：查物料（`unit_id` / `serial_required`）、
/// 查库位、核对采购明细行的归属与未收数量。
pub(super) async fn plan_lines(
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

/// 复制接口的权限码 = **建单权限码**（卡面硬红线 5：复制 = 建单，不另种码）。
///
/// 单独提成常量是为了让单测能逐字断言「复制守的就是 create 那一个码」，
/// 也防止将来有人给它顺手换成一个新码（那会让「能建单不能复制」这种裂缝出现）。
pub const COPY_PERMISSION: &str = "business:erp:stock-in:create";

/// 入账态改单的 409（统一文案 + 点名本次想改的字段）。
pub(super) fn locked_change(current: &stockin_model::StockInRow, field: &str) -> ApiError {
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
pub(super) async fn ensure_remark_only(
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
pub(super) fn ensure_derived_totals(
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
pub(super) fn items_match(
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

/// 事务提交后发送锁外副作用（审批通知 / 通知节点的知会消息）。
///
/// 与 `flow::handler::drain` 同口径：投递失败只 WARN，**不影响已经落库的业务**
/// （流程已推进是事实，邮件/站内信没发出去是运维问题，不该让单据回滚）。
pub(super) async fn flush_effects(out: &mut engine::ActionOut) {
    let effects = out.effects.take();
    if !effects.is_empty() {
        engine::flush_effects(effects).await;
    }
    let drafts = out.effects.take_drafts();
    if !drafts.is_empty() {
        engine::flush_drafts(drafts).await;
    }
}

/// 在跑实例的单号（拼 409 文案用；查不到就给 `-`）。
pub(super) async fn pending_post_no_of(
    instance_id: Option<Uuid>,
) -> std::result::Result<String, ApiError> {
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
pub(super) async fn stock_in_form_data(row: &stockin_model::StockInRow) -> serde_json::Value {
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
pub(super) fn decimal_as_number(value: Decimal) -> serde_json::Value {
    let text = value.round_dp(4).to_string();
    match text.parse::<f64>() {
        Ok(float) => serde_json::Number::from_f64(float)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

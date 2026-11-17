//! 框架协议执行量统计接口层（M4-09）：**已用 / 剩余**。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/framework-agreement/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |:--|:--|:--|:--|
//! | GET | `/business/erp/framework-agreement/execution/page` | `business:erp:framework-agreement:execution` | 跨协议执行量总览（关键字 / 供应商 / 状态 / 有效期状态 / 只看超限） |
//! | GET | `/business/erp/framework-agreement/{id}/execution` | 同上 | 单协议执行量：单头汇总 + 逐物料行（已用 / 剩余 / 执行率 / 行状态）+ 下钻来源 |
//!
//! # 四条硬口径
//!
//! 1. **只读**：两个接口都不写库（已用量的真相是单据链，见 V0452 头注释）；
//! 2. **回写列只作对账**：`booked_*` 与 `executed_*` 并排返回，差非 0 就是「M4-08 的回写
//!    与单据链分叉」，报表直接显形（`items[].booked_drift`）；
//! 3. **剩余是 `Option`**：`null` = 不限（**不是** 0），负数 = 超上限 —— 前端不许把 null 显示成 0；
//! 4. **下钻不过滤已释放单**：作废 / 软删的采购单要出现在 `sources` 里（`counted = false` +
//!    原因），否则用户会以为「这批量凭空少了」（见 `model::sources_of_agreement`）。
//!
//! # 空物料范围的协议为什么是 200 而不是 404
//!
//! 采购计划执行率（M4-04）把「一行明细都没有的计划」按 404 处理 —— 那是计划口径（空计划
//! 要么没编完、要么该删）。框架协议不同：**协议单头本身是合法实体**，物料范围 0 行是
//! 「拟稿还没编物料」的正常中间态，返 404 会让详情页把「协议存在但还没编物料」误报成
//! 「协议不存在」（前端只能弹「已被删除」）。所以这里只在**协议不存在 / 已软删**时 404，
//! 空范围返 200 + `items = []` + `item_count = 0`，`exec_state = NOT_STARTED`。
//!
//! # 为什么业务逻辑拆成普通 async 函数
//!
//! axum handler 要 `Query` / `Path` 才能跑，真库用例没法直接调；把「取数 + 组装」
//! 收进 [`page_of`] / [`execution_of`] 之后，用例走**同一条代码路径**（只跳过认证与路由）。

use axum::extract::{Path, Query};
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

use crate::business::erp::purchase::framework_agreement_execution_dto::{
    AgreementExecItemRes, AgreementExecRowRes, AgreementExecutionRes, ExecSourceRes, ExecutionFilter,
    ExecutionPageQuery, agreement_status_label, exec_state_label, pct_of, price_mode_label, validity_state_label,
};
use crate::business::erp::purchase::framework_agreement_execution_model::{self as model, HeadRow, ItemRow, SourceRow};
use crate::business::erp::purchase::purchase_model;

/// 解析路径里的 UUID（非法 → 400，绝不静默当作「没传」）。
fn parse_agreement_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(crate::business::erp::purchase::framework_agreement_execution_dto::param_error("id 不能为空"));
    }
    Uuid::parse_str(text).map_err(|_| {
        crate::business::erp::purchase::framework_agreement_execution_dto::param_error(format!(
            "id={text} 不是合法 uuid"
        ))
    })
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("框架协议不存在或已删除（{id}）"),
    )
}

// ==================== ① 总览（跨协议执行量） ====================

/// 跨协议执行量总览。
#[nik::permission(
    path = "/business/erp/framework-agreement/execution/page",
    method = "GET",
    permission = "business:erp:framework-agreement:execution"
)]
#[nik::get("/business/erp/framework-agreement/execution/page")]
pub async fn framework_agreement_execution_page(
    Query(query): Query<ExecutionPageQuery>,
) -> ResResult<PageData<Vec<AgreementExecRowRes>>> {
    let filter = query.filter()?;
    let (list, total) = page_of(&filter).await?;
    Ok(Res::ok(PageData {
        list,
        total,
        page: filter.page,
        page_size: filter.page_size,
    }))
}

/// 总览核心逻辑（真库用例与 handler 共用同一条路径）。
pub async fn page_of(filter: &ExecutionFilter) -> std::result::Result<(Vec<AgreementExecRowRes>, u64), ApiError> {
    let (rows, total) = model::page_execution(filter)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议执行量总览"))?;
    Ok((rows.iter().map(to_row_res).collect(), total))
}

// ==================== ② 详情（单协议执行量 + 下钻） ====================

/// 单协议执行量详情。
#[nik::permission(
    path = "/business/erp/framework-agreement/{id}/execution",
    method = "GET",
    permission = "business:erp:framework-agreement:execution"
)]
#[nik::get("/business/erp/framework-agreement/{id}/execution")]
pub async fn framework_agreement_execution_detail(Path(id): Path<String>) -> ResResult<AgreementExecutionRes> {
    let agreement_id = parse_agreement_id(&id)?;
    Ok(Res::ok(execution_of(agreement_id).await?))
}

/// 详情核心逻辑：单头 + 逐物料行 + 每行来源。
///
/// 404 语义：**只有**协议不存在 / 已软删才 404（空物料范围是合法中间态，见模块头）。
pub async fn execution_of(agreement_id: Uuid) -> std::result::Result<AgreementExecutionRes, ApiError> {
    let head = model::head_of(db(), agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议执行量"))?
        .ok_or_else(|| not_found(agreement_id))?;
    let items = model::items_of_agreement(db(), agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议执行量明细"))?;
    let sources = model::sources_of_agreement(db(), agreement_id)
        .await
        .map_err(|error| utils::db_error(error, "查询框架协议执行量来源单据"))?;
    Ok(AgreementExecutionRes {
        agreement: to_row_res(&head),
        items: items.iter().map(to_item_res).collect(),
        sources: sources.iter().map(to_source_res).collect(),
    })
}

// ==================== 行 → 出参 ====================

fn to_row_res(row: &HeadRow) -> AgreementExecRowRes {
    AgreementExecRowRes {
        agreement_id: row.agreement_id.to_string(),
        agreement_no: row.agreement_no.clone(),
        agreement_name: row.agreement_name.clone(),
        supplier_id: row.supplier_id.to_string(),
        supplier_name: row.supplier_name.clone(),
        supplier_display_name: row.supplier_display_name.clone(),
        dept_id: row.dept_id.map(|id| id.to_string()),
        dept_name: row.dept_name.clone(),
        owner_id: row.owner_id.map(|id| id.to_string()),
        owner_name: row.owner_name.clone(),
        start_date: row.start_date.to_string(),
        end_date: row.end_date.to_string(),
        sign_date: row.sign_date.map(|date| date.to_string()),
        status: row.status.clone(),
        status_label: agreement_status_label(&row.status).to_string(),
        validity_state: row.validity_state.clone(),
        validity_state_label: validity_state_label(&row.validity_state).to_string(),
        is_effective: row.is_effective,
        days_left: i64::from(row.days_left),
        price_mode: row.price_mode.clone(),
        price_mode_label: price_mode_label(&row.price_mode).to_string(),
        price_includes_tax: row.price_includes_tax,
        price_currency: row.price_currency.clone(),
        price_tolerance: row.price_tolerance,
        total_max_qty: row.total_max_qty,
        total_max_amount: row.total_max_amount,
        cap_qty: row.cap_qty,
        cap_amount: row.cap_amount,
        item_max_qty_sum: row.item_max_qty_sum,
        item_count: row.item_count,
        unlimited_items: row.unlimited_items,
        over_items: row.over_items,
        done_items: row.done_items,
        partial_items: row.partial_items,
        not_started_items: row.not_started_items,
        executed_qty: row.executed_qty,
        executed_amount: row.executed_amount,
        executed_amount_with_tax: row.executed_amount_with_tax,
        executed_amount_capped: row.executed_amount_capped,
        booked_total_qty: row.booked_total_qty,
        booked_total_amount: row.booked_total_amount,
        remaining_qty: row.remaining_qty,
        remaining_amount: row.remaining_amount,
        exec_qty_rate: row.exec_qty_rate,
        exec_qty_rate_pct: pct_of(row.exec_qty_rate),
        exec_amount_rate: row.exec_amount_rate,
        exec_amount_rate_pct: pct_of(row.exec_amount_rate),
        exec_state: row.exec_state.clone(),
        exec_state_label: exec_state_label(&row.exec_state).to_string(),
        order_doc_count: row.order_doc_count,
        released_doc_count: row.released_doc_count,
        last_order_on: row.last_order_on.map(|date| date.to_string()),
        created_at: row.created_at.to_rfc3339(),
    }
}

fn to_item_res(row: &ItemRow) -> AgreementExecItemRes {
    AgreementExecItemRes {
        agreement_item_id: row.agreement_item_id.to_string(),
        line_no: row.line_no,
        material_id: row.material_id.to_string(),
        material_code: row.material_code.clone(),
        material_name: row.material_name.clone(),
        unit_id: row.unit_id.to_string(),
        unit_name: row.unit_name.clone(),
        agreed_price: row.agreed_price,
        tax_rate: row.tax_rate,
        max_qty: row.max_qty,
        executed_qty: row.executed_qty,
        booked_qty: row.booked_qty,
        booked_drift: row.booked_qty - row.executed_qty,
        executed_amount: row.executed_amount,
        executed_amount_with_tax: row.executed_amount_with_tax,
        executed_amount_capped: row.executed_amount_capped,
        cap_amount: row.cap_amount,
        remaining_qty: row.remaining_qty,
        remaining_amount: row.remaining_amount,
        exec_qty_rate: row.exec_qty_rate,
        exec_qty_rate_pct: pct_of(row.exec_qty_rate),
        exec_amount_rate: row.exec_amount_rate,
        exec_state: row.exec_state.clone(),
        exec_state_label: exec_state_label(&row.exec_state).to_string(),
        order_doc_count: row.order_doc_count,
        order_line_count: row.order_line_count,
        last_order_on: row.last_order_on.map(|date| date.to_string()),
    }
}

fn to_source_res(row: &SourceRow) -> ExecSourceRes {
    let (counted, verdict) = row.verdict();
    ExecSourceRes {
        agreement_item_id: row.agreement_item_id.to_string(),
        order_id: row.order_id.to_string(),
        order_no: row.order_no.clone(),
        order_status: row.order_status.clone(),
        order_status_label: purchase_model::status_label(&row.order_status).to_string(),
        order_date: row.order_date.to_string(),
        order_deleted_at: row.order_deleted_at.map(|at| at.to_rfc3339()),
        line_no: row.line_no,
        qty: row.qty,
        amount: row.amount,
        with_tax_amount: row.with_tax_amount,
        price: row.price,
        counted,
        verdict: verdict.to_string(),
    }
}

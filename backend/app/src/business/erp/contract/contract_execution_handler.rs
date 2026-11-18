//! 合同执行量统计接口层（M4-19）：**已执行 / 合同额**。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/contract/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |:--|:--|:--|:--|
//! | GET | `/business/erp/contract/execution/page` | `business:erp:contract:execution` | 跨合同执行量总览（关键字 / 类型 / 状态 / 执行状态 / 只看超限 / 只看过期 / 只看币种串） |
//! | GET | `/business/erp/contract/{id}/execution` | 同上 | 单合同执行量：单头汇总 + 下钻单据（含作废 / 软删） |
//!
//! # 四条硬口径
//!
//! 1. **只读**：两个接口都不写库（已执行额的真相是单据链，见 V0458 头注释）；
//! 2. **口径与额度守卫同源**：已执行额 = Σ 计入单据 `total_with_tax`，判据
//!    `deleted_at IS NULL AND status <> 'CANCELLED'` —— 与
//!    `purchase_handler::guard_contract_amount` / `sales_order_handler::guard_contract_amount`
//!    的 `committed_amount` 逐字一致。换口径的后果是「报表说还剩 10、下单却被拒」；
//! 3. **剩余额是 `Decimal` 且**可为负**：越界（守卫只拦新单）必须显形，绝不截成 0；
//! 4. **下钻不过滤已释放单**：作废 / 软删的单据要出现在 `sources` 里（`counted = false` +
//!    原因），否则用户会以为「这批量凭空少了」。
//!
//! # 404 语义
//!
//! 只有**合同不存在 / 已软删**才 404（视图里没有这一行）。一张单据都没挂是正常中间态，
//! 返 200 + `sources = []` + `exec_state = NOT_STARTED`（服务 / 租赁合同是 `NO_DOWNSTREAM`）。
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

use crate::business::erp::contract::contract_execution_dto::{
    ContractExecRowRes, ContractExecutionRes, ExecSourceRes, ExecutionFilter, ExecutionPageQuery, contract_type_label,
    downstream_label, exec_state_label, order_status_label, param_error, pct_of, source_type_label,
};
use crate::business::erp::contract::contract_execution_model::{self as model, HeadRow, SourceRow};
use crate::business::erp::contract::contract_model;

/// 解析路径里的 UUID（非法 → 400，绝不静默当作「没传」）。
fn parse_contract_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(param_error("id 不能为空"));
    }
    Uuid::parse_str(text).map_err(|_| param_error(format!("id={text} 不是合法 uuid")))
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::ERP_CONTRACT_NOT_USABLE,
        format!("合同不存在或已删除（{id}）"),
    )
}

// ==================== ① 总览（跨合同执行量） ====================

/// 跨合同执行量总览。
#[nik::permission(
    path = "/business/erp/contract/execution/page",
    method = "GET",
    permission = "business:erp:contract:execution"
)]
#[nik::get("/business/erp/contract/execution/page")]
pub async fn contract_execution_page(
    Query(query): Query<ExecutionPageQuery>,
) -> ResResult<PageData<Vec<ContractExecRowRes>>> {
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
pub async fn page_of(filter: &ExecutionFilter) -> std::result::Result<(Vec<ContractExecRowRes>, u64), ApiError> {
    let (rows, total) = model::page_execution(filter)
        .await
        .map_err(|error| utils::db_error(error, "查询合同执行量总览"))?;
    Ok((rows.iter().map(to_row_res).collect(), total))
}

// ==================== ② 详情（单合同执行量 + 下钻） ====================

/// 单合同执行量详情。
#[nik::permission(
    path = "/business/erp/contract/{id}/execution",
    method = "GET",
    permission = "business:erp:contract:execution"
)]
#[nik::get("/business/erp/contract/{id}/execution")]
pub async fn contract_execution_detail(Path(id): Path<String>) -> ResResult<ContractExecutionRes> {
    let contract_id = parse_contract_id(&id)?;
    Ok(Res::ok(execution_of(contract_id).await?))
}

/// 详情核心逻辑：单头 + 下钻单据。
///
/// 404 语义：**只有**合同不存在 / 已软删才 404（见模块头）。
pub async fn execution_of(contract_id: Uuid) -> std::result::Result<ContractExecutionRes, ApiError> {
    let head = model::head_of(db(), contract_id)
        .await
        .map_err(|error| utils::db_error(error, "查询合同执行量"))?
        .ok_or_else(|| not_found(contract_id))?;
    let sources = model::sources_of_contract(db(), contract_id)
        .await
        .map_err(|error| utils::db_error(error, "查询合同执行来源单据"))?;
    let source_count = sources.len();
    let counted_count = sources.iter().filter(|row| row.counted).count();
    Ok(ContractExecutionRes {
        contract: to_row_res(&head),
        sources: sources.iter().map(to_source_res).collect(),
        source_count,
        counted_count,
        released_count: source_count - counted_count,
    })
}

// ==================== 行 → 出参 ====================

fn to_row_res(row: &HeadRow) -> ContractExecRowRes {
    // 展示用乙方名：快照优先（审计「签约时是谁」），快照空时用现名（存量合同不回填）
    let party_b_display_name = if row.party_b_name.trim().is_empty() {
        row.party_b_current_name.clone().unwrap_or_default()
    } else {
        row.party_b_name.clone()
    };
    ContractExecRowRes {
        contract_id: row.contract_id.to_string(),
        contract_no: row.contract_no.clone(),
        contract_name: row.contract_name.clone(),
        contract_type: row.contract_type.clone(),
        contract_type_label: contract_type_label(&row.contract_type).to_string(),
        status: row.status.clone(),
        status_label: contract_model::status_label(&row.status).to_string(),
        usable_for_order: row.usable_for_order,
        expired: row.expired,
        days_left: row.days_left.map(i64::from),
        party_a_id: row.party_a_id.map(|id| id.to_string()),
        party_a_name: row.party_a_name.clone(),
        party_b_type: row.party_b_type.clone(),
        party_b_id: row.party_b_id.to_string(),
        party_b_name: row.party_b_name.clone(),
        party_b_current_name: row.party_b_current_name.clone(),
        party_b_display_name,
        owner_id: row.owner_id.map(|id| id.to_string()),
        owner_name: row.owner_name.clone(),
        dept_id: row.dept_id.map(|id| id.to_string()),
        dept_name: row.dept_name.clone(),
        amount: row.amount,
        currency_code: row.currency_code.clone(),
        executed_amount: row.executed_amount,
        executed_amount_net: row.executed_amount_net,
        remaining_amount: row.remaining_amount,
        exec_amount_rate: row.exec_amount_rate,
        exec_amount_rate_pct: pct_of(row.exec_amount_rate),
        exec_state: row.exec_state.clone(),
        exec_state_label: exec_state_label(&row.exec_state).to_string(),
        downstream_type: row.downstream_type.clone(),
        downstream_type_label: downstream_label(row.downstream_type.as_deref()).map(str::to_string),
        order_doc_count: row.order_doc_count,
        released_doc_count: row.released_doc_count,
        currency_mismatch_doc_count: row.currency_mismatch_doc_count,
        purchase_doc_count: row.purchase_doc_count,
        sales_doc_count: row.sales_doc_count,
        last_order_on: row.last_order_on.map(|date| date.to_string()),
        sign_date: row.sign_date.map(|date| date.to_string()),
        start_date: row.start_date.map(|date| date.to_string()),
        end_date: row.end_date.map(|date| date.to_string()),
        created_at: row.created_at.to_rfc3339(),
    }
}

fn to_source_res(row: &SourceRow) -> ExecSourceRes {
    let (counted, verdict) = row.verdict();
    ExecSourceRes {
        order_id: row.order_id.to_string(),
        order_no: row.order_no.clone(),
        source_type: row.source_type.clone(),
        source_type_label: source_type_label(&row.source_type).to_string(),
        order_status: row.order_status.clone(),
        order_status_label: order_status_label(&row.order_status).to_string(),
        order_date: row.order_date.to_string(),
        currency_code: row.currency_code.clone(),
        currency_matched: row.currency_matched,
        total_amount: row.total_amount,
        total_with_tax: row.total_with_tax,
        order_deleted_at: row.order_deleted_at.map(|at| at.to_rfc3339()),
        counted,
        verdict: verdict.to_string(),
    }
}

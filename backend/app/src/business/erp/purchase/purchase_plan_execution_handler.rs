//! 采购计划执行率跟踪接口层（M4-04）：**已下单 / 计划量**。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/purchase-plan/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |:--|:--|:--|:--|
//! | GET | `/business/erp/purchase-plan/execution/page` | `business:erp:purchase-plan:execution` | 跨计划执行率总览（年度 / 月份 / 类型 / 状态 / 部门 / 关键字 / 执行率下限） |
//! | GET | `/business/erp/purchase-plan/{id}/execution` | 同上 | 单计划执行率：单头汇总 + 逐行（计划 / 已申请 / 已下单 / 执行率 / 行状态）+ 每行来源单据 |
//!
//! # 三条硬口径
//!
//! 1. **只读**：两个接口都不写库（执行率的真相是单据链，见 V0449 头注释）；
//! 2. **整单状态由汇总算**：不取「明细行状态的多数」—— 一张 9 行下满 + 1 行未开工的计划，
//!    整单率是 90%，不是「下满」（`exec_state_of` 同一套判据）；
//! 3. **下钻不过滤已删单**：作废 / 软删的采购单要出现在 `sources` 里（`counted = false` +
//!    原因），否则用户会以为「这批量凭空少了」（见 model::sources_of_plan）。
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

use crate::business::erp::purchase::purchase_plan_execution_dto::{
    ExecSourceRes, ExecutionFilter, ExecutionPageQuery, PlanExecItemRes, PlanExecRowRes, PlanExecutionRes,
    convertible_of, exec_state_label, exec_state_of, param_error, pct_of, plan_status_label, plan_type_label_of,
    requisition_status_label,
};
use crate::business::erp::purchase::purchase_plan_execution_model::{self as model, HeadRow, ItemRow, SourceRow};

/// 解析路径里的 UUID（非法 → 400，绝不静默当作「没传」）。
fn parse_plan_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(param_error("id 不能为空"));
    }
    Uuid::parse_str(text).map_err(|_| param_error(format!("id={text} 不是合法 uuid")))
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("采购计划不存在或已删除（{id}）"),
    )
}

// ==================== ① 总览（跨计划执行率） ====================

/// 跨计划执行率总览。
#[nik::permission(
    path = "/business/erp/purchase-plan/execution/page",
    method = "GET",
    permission = "business:erp:purchase-plan:execution"
)]
#[nik::get("/business/erp/purchase-plan/execution/page")]
pub async fn purchase_plan_execution_page(
    Query(query): Query<ExecutionPageQuery>,
) -> ResResult<PageData<Vec<PlanExecRowRes>>> {
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
pub async fn page_of(filter: &ExecutionFilter) -> std::result::Result<(Vec<PlanExecRowRes>, u64), ApiError> {
    let (rows, total) = model::page_execution(filter)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划执行率总览"))?;
    Ok((rows.iter().map(to_row_res).collect(), total))
}

// ==================== ② 详情（单计划执行率 + 下钻） ====================

/// 单计划执行率详情。
#[nik::permission(
    path = "/business/erp/purchase-plan/{id}/execution",
    method = "GET",
    permission = "business:erp:purchase-plan:execution"
)]
#[nik::get("/business/erp/purchase-plan/{id}/execution")]
pub async fn purchase_plan_execution_detail(Path(id): Path<String>) -> ResResult<PlanExecutionRes> {
    let plan_id = parse_plan_id(&id)?;
    Ok(Res::ok(execution_of(plan_id).await?))
}

/// 详情核心逻辑：单头 + 逐行 + 每行来源。
///
/// 404 语义：计划不存在、已软删、或一张明细都没有 —— 第三种情况视图能算出单头
/// （`line_count = 0`、率 `null`），但「执行率报表里出现空计划」只会让人以为数据丢了，
/// 所以和不存在同口径报 404（空计划要么还没编完，要么本来就该删）。
pub async fn execution_of(plan_id: Uuid) -> std::result::Result<PlanExecutionRes, ApiError> {
    let head = model::head_of(db(), plan_id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划执行率"))?
        .ok_or_else(|| not_found(plan_id))?;
    if head.line_count == 0 {
        return Err(not_found(plan_id));
    }
    let items = model::items_of_plan(db(), plan_id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划执行率明细"))?;
    let sources = model::sources_of_plan(db(), plan_id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划执行率来源单据"))?;
    Ok(PlanExecutionRes {
        plan: to_row_res(&head),
        items: items
            .iter()
            .map(|item| to_item_res(item, sources.iter().filter(|row| row.plan_item_id == item.plan_item_id)))
            .collect(),
    })
}

// ==================== ③ 行 → 报文 ====================

fn to_row_res(row: &HeadRow) -> PlanExecRowRes {
    let exec_state = exec_state_of(row.ordered_qty, row.planned_qty);
    PlanExecRowRes {
        plan_id: row.plan_id.to_string(),
        plan_no: row.plan_no.clone(),
        plan_name: row.plan_name.clone(),
        plan_type: row.plan_type.clone(),
        plan_type_label: plan_type_label_of(&row.plan_type).to_string(),
        plan_year: row.plan_year,
        plan_month: row.plan_month,
        dept_id: row.dept_id.map(|id| id.to_string()),
        dept_name: row.dept_name.clone(),
        planner_name: row.planner_name.clone(),
        status: row.status.clone(),
        status_label: plan_status_label(&row.status).to_string(),
        total_planned_amount: row.total_planned_amount,
        line_count: row.line_count,
        planned_qty: row.planned_qty,
        booked_qty: row.booked_qty,
        req_qty: row.req_qty,
        ordered_qty: row.ordered_qty,
        remaining_qty: row.remaining_qty,
        convertible_qty: convertible_of(row.planned_qty, row.booked_qty),
        exec_rate: row.exec_rate,
        exec_rate_pct: pct_of(row.exec_rate),
        req_rate: row.req_rate,
        req_rate_pct: pct_of(row.req_rate),
        exec_state: exec_state.to_string(),
        exec_state_label: exec_state_label(exec_state).to_string(),
        not_started_lines: row.not_started_lines,
        partial_lines: row.partial_lines,
        done_lines: row.done_lines,
        over_lines: row.over_lines,
        created_at: row.created_at.to_rfc3339(),
    }
}

fn to_item_res<'a>(item: &ItemRow, sources: impl Iterator<Item = &'a SourceRow>) -> PlanExecItemRes {
    PlanExecItemRes {
        plan_item_id: item.plan_item_id.to_string(),
        line_no: item.line_no,
        material_id: item.material_id.to_string(),
        material_code: item.material_code.clone(),
        material_name: item.material_name.clone(),
        warehouse_id: item.warehouse_id.to_string(),
        warehouse_name: item.warehouse_name.clone(),
        unit_id: item.unit_id.to_string(),
        unit_name: item.unit_name.clone(),
        planned_qty: item.planned_qty,
        planned_price: item.planned_price,
        planned_amount: item.planned_amount,
        booked_qty: item.booked_qty,
        req_qty: item.req_qty,
        ordered_qty: item.ordered_qty,
        remaining_qty: item.remaining_qty,
        convertible_qty: convertible_of(item.planned_qty, item.booked_qty),
        exec_rate: item.exec_rate,
        exec_rate_pct: pct_of(item.exec_rate),
        req_rate: item.req_rate,
        req_rate_pct: pct_of(item.req_rate),
        exec_state: item.exec_state.clone(),
        exec_state_label: exec_state_label(&item.exec_state).to_string(),
        over_plan: item.exec_state == crate::business::erp::purchase::purchase_plan_execution_dto::STATE_OVER,
        req_count: item.req_count,
        req_doc_count: item.req_doc_count,
        po_doc_count: item.po_doc_count,
        sources: sources.map(to_source_res).collect(),
    }
}

fn to_source_res(row: &SourceRow) -> ExecSourceRes {
    let (counted, reason) = row.verdict();
    ExecSourceRes {
        req_item_id: row.req_item_id.to_string(),
        req_id: row.req_id.to_string(),
        req_no: row.req_no.clone(),
        req_status: row.req_status.clone(),
        req_status_label: requisition_status_label(&row.req_status).to_string(),
        qty: row.qty,
        po_id: row.po_id.map(|id| id.to_string()),
        po_no: row.po_no.clone(),
        po_status: row.po_status.clone(),
        po_status_label: row
            .po_status
            .as_deref()
            .map(|status| crate::business::erp::purchase::purchase_model::status_label(status).to_string()),
        counted,
        reason: reason.to_string(),
    }
}

// ==================== 单测（纯逻辑：常量 / 组装不连库的部分） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::purchase::purchase_plan_execution_dto::{
        EXEC_PERMISSION, STATE_DONE, STATE_OVER, STATE_PARTIAL,
    };
    use crate::business::erp::purchase::purchase_plan_execution_dto::{exec_rate_of, remaining_of};
    use nik::prelude::rust_decimal::Decimal;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    /// ① 权限码与 V0449 的按钮行逐字一致（页面显形码不许出现在后端，约定 14）。
    #[test]
    fn permission_constant_matches_menu() {
        assert_eq!(EXEC_PERMISSION, "business:erp:purchase-plan:execution");
        assert!(!EXEC_PERMISSION.ends_with(":page"));
    }

    /// ② 路径参数：非法 uuid → 400（不静默当成「没传」）。
    #[test]
    fn path_id_is_validated() {
        let ok = parse_plan_id("d4040001-0000-4000-8000-000000000001").expect("合法 uuid");
        assert_eq!(ok.to_string(), "d4040001-0000-4000-8000-000000000001");
        for bad in ["", "   ", "not-a-uuid"] {
            let error = parse_plan_id(bad).expect_err("必须 400");
            assert_eq!(error.code, error_code::PARAM_INVALID);
        }
    }

    /// ③ 组装：单头报文里的率 / 状态 / 剩余都来自同一套纯函数（不另写一遍算）。
    #[test]
    fn head_res_uses_shared_pure_functions() {
        let row = HeadRow {
            plan_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000001").expect("uuid"),
            plan_no: "PPL-20261118-000001".to_string(),
            plan_name: "2026 年度采购计划".to_string(),
            plan_type: "ANNUAL".to_string(),
            plan_year: 2026,
            plan_month: None,
            dept_id: None,
            dept_name: None,
            planner_name: None,
            status: "APPROVED".to_string(),
            total_planned_amount: dec("1000"),
            line_count: 2,
            planned_qty: dec("100"),
            booked_qty: dec("112"),
            req_qty: dec("112"),
            ordered_qty: dec("40"),
            remaining_qty: remaining_of(dec("100"), dec("40")),
            exec_rate: exec_rate_of(dec("40"), dec("100")),
            req_rate: exec_rate_of(dec("112"), dec("100")),
            not_started_lines: 1,
            partial_lines: 1,
            done_lines: 0,
            over_lines: 0,
            created_at: nik::prelude::chrono::Utc::now(),
        };
        let res = to_row_res(&row);
        assert_eq!(res.exec_state, STATE_PARTIAL);
        assert_eq!(res.exec_state_label, "部分下单");
        assert_eq!(res.exec_rate_pct, Some(dec("40.00")));
        assert_eq!(res.req_rate_pct, Some(dec("112.00")));
        assert_eq!(res.remaining_qty, dec("60"));
        assert_eq!(res.status_label, "已批准");
        assert_eq!(res.plan_type_label, "年度计划");
    }

    /// ④ 组装：明细行的 `over_plan` 直接由状态给出（前端标红不用自己比大小）。
    #[test]
    fn item_res_marks_over_plan() {
        let item = ItemRow {
            plan_item_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000011").expect("uuid"),
            line_no: 1,
            material_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000021").expect("uuid"),
            material_code: "M-1".to_string(),
            material_name: "物料 1".to_string(),
            warehouse_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000031").expect("uuid"),
            warehouse_name: "主仓".to_string(),
            unit_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000041").expect("uuid"),
            unit_name: "个".to_string(),
            planned_qty: dec("10"),
            planned_price: dec("1"),
            planned_amount: dec("10"),
            booked_qty: dec("15"),
            req_qty: dec("15"),
            ordered_qty: dec("15"),
            remaining_qty: dec("-5"),
            exec_rate: exec_rate_of(dec("15"), dec("10")),
            req_rate: exec_rate_of(dec("15"), dec("10")),
            exec_state: STATE_OVER.to_string(),
            req_count: 2,
            req_doc_count: 2,
            po_doc_count: 1,
        };
        let res = to_item_res(&item, std::iter::empty());
        assert!(res.over_plan, "超计划必须标出来");
        assert_eq!(res.exec_state_label, "超计划");
        assert_eq!(res.exec_rate_pct, Some(dec("150.00")));
        assert_eq!(res.remaining_qty, dec("-5"), "剩余保留负数");
        assert!(res.sources.is_empty());
    }

    /// ⑤ 组装：已下满的一行 `over_plan = false`（DONE ≠ OVER）。
    #[test]
    fn done_line_is_not_over() {
        let mut item = ItemRow {
            plan_item_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000012").expect("uuid"),
            line_no: 2,
            material_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000022").expect("uuid"),
            material_code: "M-2".to_string(),
            material_name: "物料 2".to_string(),
            warehouse_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000032").expect("uuid"),
            warehouse_name: "主仓".to_string(),
            unit_id: Uuid::parse_str("d4040001-0000-4000-8000-000000000042").expect("uuid"),
            unit_name: "个".to_string(),
            planned_qty: dec("5"),
            planned_price: dec("1"),
            planned_amount: dec("5"),
            booked_qty: dec("5"),
            req_qty: dec("5"),
            ordered_qty: dec("5"),
            remaining_qty: dec("0"),
            exec_rate: exec_rate_of(dec("5"), dec("5")),
            req_rate: exec_rate_of(dec("5"), dec("5")),
            exec_state: STATE_DONE.to_string(),
            req_count: 1,
            req_doc_count: 1,
            po_doc_count: 1,
        };
        let res = to_item_res(&item, std::iter::empty());
        assert!(!res.over_plan);
        assert_eq!(res.exec_state_label, "已下满");
        assert_eq!(res.exec_rate_pct, Some(dec("100.00")));
        item.exec_state = STATE_PARTIAL.to_string();
        assert_eq!(to_item_res(&item, std::iter::empty()).exec_state_label, "部分下单");
    }
}

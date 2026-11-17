//! 采购计划 → 采购申请转换（M4-03）：按计划生成申请。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/purchase-plan/{id}/to-requisition`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | POST | `/business/erp/purchase-plan/{id}/to-requisition` | `business:erp:purchase-plan:to-requisition` | 按计划生成采购申请（**一仓一单**），回写已执行量 |
//!
//! # 一次调用做四件事（同一事务）
//!
//! ```text
//! ① 校验：计划必须 APPROVED；申请部门（计划部门 or 请求覆盖）必须存在；
//! ② 拆单：按明细的 `warehouse_id` 分组 —— 申请单单头只有一个收货仓库（NOT NULL），
//!    而计划粒度是「物料 × 仓库」，所以 N 个仓库生成 N 张申请单（`PRQ-yyyyMMdd-NNNNNN`）；
//! ③ 派生：申请明细逐行带 `plan_item_id`（溯源），申请单头带 `source_plan_id`；
//! ④ 回写：`executed_qty += 本次转出量`，然后 `recalc_totals` 重算单头三个合计。
//! ```
//!
//! # 为什么必须一个事务
//!
//! 申请单造出去、已执行量没记 → 同一段计划量会被转两遍（重复采购）；
//! 已执行量记了、申请单没造 → 计划显示「已执行」却查不到任何单据（假执行）。
//! 两种半成品都比失败更糟，所以 `begin → 建单 → 回写 → recalc → commit` 一把锁在事务里，
//! 任一步失败整批回滚 —— 用户看到 500/409，但数据是干净的，重试即可（AGENTS 约定 11：
//! 不留死单，也不需要改库）。
//!
//! # 并发口径
//!
//! * `lock_plan`：`SELECT … FOR UPDATE` 锁单头，并在锁内**重判**状态（读到的 APPROVED
//!   可能在等锁期间被别人改掉 —— 计划没提供改状态的接口，但审批回调会）；
//! * `lock_items`：`SELECT id … ORDER BY id FOR UPDATE` 锁全部明细行，
//!   **先锁后读** —— 否则两次并发转换会读到同一份 `executed_qty`，各转一遍剩余量。
//! * 回写用 `WHERE id = ?` 并检查影响行数；0 行 = 行被并发删掉 → 409（事务回滚）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_purchase_plan / _item 与采购申请单同口径（计划是公司级滚动文件，
//!        采购要看全公司）。可见性由 business:erp:purchase-plan:* 权限码控制，
//!        本路由按 `to-requisition` 判权；生成的申请单归采购申请域（`purchase-requisition:*`）。
//! ```

use axum::Json;
use axum::extract::Path;
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase::purchase_plan_convert_dto::{
    ConvertedLineRes, ConvertedRequisitionRes, ConvertibleLine, ToRequisitionReq, ToRequisitionRes, check_tax,
    clean_remark, compose_head_remark, group_by_warehouse, parse_optional_uuid, plan_conversion, today_cst,
};
use crate::business::erp::purchase::purchase_plan_dto::{parse_uuid_field, status_label};
use crate::business::erp::purchase::purchase_plan_handler::to_row_res;
use crate::business::erp::purchase::purchase_plan_model::{self as model, PlanRow, STATUS_APPROVED};
use crate::business::erp::purchase::purchase_requisition_model::{self as requisition_model, NewItem, NewRequisition};
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

/// 单据中文名（报文用）。
const LABEL: &str = "采购计划";

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
        format!("采购计划不存在或已删除（{id}）"),
    )
}

fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

fn write_error(error: DbError, context: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, format!("{context}失败：{error}"))
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

/// 状态闸：只有 APPROVED 能转（草稿没批、审批中还在批）。
fn ensure_approved(row: &PlanRow) -> std::result::Result<(), ApiError> {
    if row.status == STATUS_APPROVED {
        return Ok(());
    }
    Err(status_guard_error(&row.plan_no, &row.status))
}

/// 状态不对时的 409（`ensure_approved` 与锁内重判共用一份报文）。
fn status_guard_error(plan_no: &str, status: &str) -> ApiError {
    conflict(
        error_code::ERP_REQUISITION_STATUS_GUARD,
        format!(
            "{LABEL} {plan_no} 当前是「{}」（{status}）：必须先提交审批并批准（APPROVED）才能转采购申请；\
             未批准的计划改了还会变，转出去的申请单就跟着错",
            status_label(status)
        ),
    )
}

// ==================== 核心逻辑（handler 与真库用例共用） ====================

/// 按计划生成采购申请（一仓一单），并在同一事务里回写已执行量。
pub async fn to_requisition(
    plan_id: Uuid,
    req: &ToRequisitionReq,
    operator: Option<Uuid>,
) -> std::result::Result<ToRequisitionRes, ApiError> {
    // ---- 事务外的前置校验（尽量把 400 挡在开事务之前，省一次回滚）----
    let plan = model::find_plan(db(), plan_id)
        .await
        .map_err(|error| utils::db_error(error, "查询采购计划"))?
        .ok_or_else(|| not_found(plan_id))?;
    ensure_approved(&plan)?;

    // 申请部门：请求覆盖 > 计划编制部门。公司级计划（编制部门为空）必须显式给 ——
    // `biz_purchase_requisition.dept_id` 是 NOT NULL，这里不给就会撞库层 500。
    let dept_id = match parse_optional_uuid(req.dept_id.as_deref(), "dept_id")? {
        Some(id) => id,
        None => plan.dept_id.ok_or_else(|| {
            bad_request(
                "这张计划没有编制部门（公司级计划）：转采购申请必须给 dept_id\
                 （申请单的申请部门是必填；请在请求里指定这次申请归哪个部门）",
            )
        })?,
    };
    let dept_name = model::dept_name_of(db(), dept_id)
        .await
        .map_err(|error| utils::db_error(error, "查询申请部门"))?
        .ok_or_else(|| reference_missing("dept_id 部门不存在或已删除"))?;

    let applicant_id = match parse_optional_uuid(req.applicant_id.as_deref(), "applicant_id")? {
        Some(id) => Some(id),
        None => operator,
    };
    let applicant_name = match applicant_id {
        Some(id) => user_name_of(id).await?,
        None => None,
    };

    let tax_rate = check_tax(req.tax_rate)?;
    let remark = clean_remark(req.remark.as_deref())?;
    let req_date = req.req_date.unwrap_or_else(today_cst);
    let demand_date = req.demand_date;
    let now = Utc::now();

    // ---- 事务：锁单头 → 锁明细 → 读明细 → 建单 → 回写 → recalc ----
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启转采购申请事务"))?;

    let outcome = async {
        // ① 锁单头并在锁内重判状态（等锁期间审批回调可能把状态改掉）
        let locked = model::lock_plan(&tx, plan_id)
            .await
            .map_err(|error| utils::db_error(error, "锁定采购计划"))?
            .ok_or_else(|| not_found(plan_id))?;
        if locked.0 != STATUS_APPROVED {
            return Err(status_guard_error(&plan.plan_no, &locked.0));
        }

        // ② 先锁明细行再读（并发口径见模块文档）
        model::lock_items(&tx, plan_id)
            .await
            .map_err(|error| utils::db_error(error, "锁定采购计划明细"))?;
        let items = model::items_of_plan(&tx, plan_id)
            .await
            .map_err(|error| utils::db_error(error, "查询采购计划明细"))?;
        let convertible: Vec<ConvertibleLine> = items.iter().map(ConvertibleLine::from).collect();

        // ③ 纯逻辑：选行 / 校验数量单价 / 按仓分组
        let lines = plan_conversion(&convertible, &req.items)?;
        let groups = group_by_warehouse(&lines);

        // ④ 一仓一张申请单
        let mut created: Vec<ConvertedRequisitionRes> = Vec::with_capacity(groups.len());
        for group in &groups {
            let head_remark = compose_head_remark(&plan.plan_no, &group.warehouse_name, remark.as_deref())?;
            let head = NewRequisition {
                req_date,
                dept_id,
                dept_name: Some(dept_name.clone()),
                applicant_id,
                applicant_name: applicant_name.clone(),
                demand_date,
                warehouse_id: group.warehouse_id,
                est_amount: group.est_amount(),
                remark: Some(head_remark),
                source_plan_id: Some(plan_id),
            };
            let new_items: Vec<NewItem> = group
                .lines
                .iter()
                .map(|line| NewItem {
                    material_id: line.material_id,
                    unit_id: line.unit_id,
                    qty: line.qty,
                    est_price: line.est_price,
                    tax_rate,
                    demand_date,
                    suggest_supplier_id: None,
                    remark: line.remark.clone(),
                    plan_item_id: Some(line.plan_item_id),
                })
                .collect();
            let (requisition_id, req_no) =
                requisition_model::create_requisition_in(&tx, &head, &new_items, operator, now)
                    .await
                    .map_err(|error| utils::db_error(error, "生成采购申请单"))?;
            created.push(ConvertedRequisitionRes {
                requisition_id: requisition_id.to_string(),
                req_no,
                warehouse_id: group.warehouse_id.to_string(),
                warehouse_name: group.warehouse_name.clone(),
                dept_id: dept_id.to_string(),
                dept_name: Some(dept_name.clone()),
                req_date: req_date.to_string(),
                est_amount: group.est_amount(),
                line_count: group.lines.len(),
                lines: group
                    .lines
                    .iter()
                    .map(|line| ConvertedLineRes {
                        plan_item_id: line.plan_item_id.to_string(),
                        plan_line_no: line.line_no,
                        material_id: line.material_id.to_string(),
                        material_code: line.material_code.clone(),
                        material_name: line.material_name.clone(),
                        qty: line.qty,
                        est_price: line.est_price,
                        amount: line.amount,
                        remaining_after: line.remaining_after,
                    })
                    .collect(),
            });
        }

        // ⑤ 回写已执行量（每行一条 UPDATE；0 行 = 行被并发删了 → 409 并回滚）
        for line in &lines {
            let affected = model::add_executed_qty(&tx, line.plan_item_id, line.qty, now)
                .await
                .map_err(|error| write_error(error, "回写计划已执行量"))?;
            if affected == 0 {
                return Err(conflict(
                    error_code::ERP_REQUISITION_STATUS_GUARD,
                    format!(
                        "计划第 {} 行在本次转换期间被改动或删除，请刷新后重试（本次转换已全部回滚）",
                        line.line_no
                    ),
                ));
            }
        }
        // ⑥ 单头合计按明细重算（派生列，永不接受客户端值）
        if !model::recalc_totals(&tx, plan_id)
            .await
            .map_err(|error| write_error(error, "重算计划合计"))?
        {
            return Err(not_found(plan_id));
        }
        // ⑦ 在事务内取最新单头（提交后再读会让「读失败」变成「写成功却报错」）
        let updated = model::find_plan(&tx, plan_id)
            .await
            .map_err(|error| utils::db_error(error, "读取采购计划"))?
            .ok_or_else(|| not_found(plan_id))?;

        Ok::<(Vec<ConvertedRequisitionRes>, PlanRow), ApiError>((created, updated))
    }
    .await;

    let (requisitions, updated) = match outcome {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交转采购申请事务"))?;

    let converted_lines: usize = requisitions.iter().map(|req| req.line_count).sum();
    let converted_qty = requisitions
        .iter()
        .flat_map(|req| req.lines.iter())
        .fold(Decimal::ZERO, |sum, line| sum + line.qty);
    let converted_amount = requisitions
        .iter()
        .flat_map(|req| req.lines.iter())
        .fold(Decimal::ZERO, |sum, line| sum + line.amount);

    tracing::info!(
        plan_id = %plan_id,
        plan_no = %plan.plan_no,
        requisitions = requisitions.len(),
        lines = converted_lines,
        qty = %converted_qty,
        "采购计划已转采购申请（一仓一单，已执行量已回写）"
    );

    Ok(ToRequisitionRes {
        plan: to_row_res(&updated),
        requisitions,
        converted_lines,
        converted_qty,
        converted_amount,
    })
}

// ==================== 路由 ====================

/// 采购计划 → 采购申请。
#[nik::permission(
    path = "/business/erp/purchase-plan/{id}/to-requisition",
    method = "POST",
    permission = "business:erp:purchase-plan:to-requisition"
)]
#[nik::post("/business/erp/purchase-plan/{id}/to-requisition")]
pub async fn purchase_plan_to_requisition(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
    body: Option<Json<ToRequisitionReq>>,
) -> ResResult<ToRequisitionRes> {
    let id = parse_uuid_field(Some(&id), "id")?;
    let req = body.map(|Json(item)| item).unwrap_or_default();
    let operator = utils::current_user_uuid(&claims);
    let result = to_requisition(id, &req, operator).await?;
    Ok(Res::ok(result))
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::purchase::purchase_plan_handler::PERM_TO_REQUISITION;

    /// 权限码常量必须与路由注解里的字面量逐字一致（`perm_menu_gate` 只认注解里的字符串）。
    #[test]
    fn permission_code_matches_route_annotation() {
        assert_eq!(PERM_TO_REQUISITION, "business:erp:purchase-plan:to-requisition");
    }

    /// 状态闸报文：非 APPROVED 一律 409，且点出当前状态与前置条件（不能只有错误码）。
    #[test]
    fn status_guard_message_is_actionable() {
        let error = status_guard_error("PPL-20261117-000001", "DRAFT");
        assert_eq!(error.code, error_code::ERP_REQUISITION_STATUS_GUARD);
        assert!(error.msg.contains("PPL-20261117-000001"), "要点出单号：{}", error.msg);
        assert!(error.msg.contains("草稿"), "要点出状态中文：{}", error.msg);
        assert!(error.msg.contains("APPROVED"), "要点出前置条件：{}", error.msg);
    }
}

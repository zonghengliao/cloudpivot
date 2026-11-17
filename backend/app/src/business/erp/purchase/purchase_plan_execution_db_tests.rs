// ============================================================================
// purchase_plan_execution_db_tests.rs —— 采购计划执行率跟踪（M4-04）的真库用例
//
// 卡：t_901d117b（M4-04【M4·采购】采购计划：执行率跟踪 —— 已下单 / 计划量）
//
// # 覆盖什么（每条都对应 V0449 头注释里的一句口径）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `fresh_plan_has_zero_execution` | 新计划：率 0、两行都 NOT_STARTED、剩余 = 计划量、`sources` 为空 |
// | `requisition_without_po_counts_as_requested_only` | 转了申请没下单：`req_qty` = 计划量、`ordered_qty` = 0、`booked_qty == req_qty`（回写列与单据链不分叉）、下钻原因「未转采购单」 |
// | `converted_po_counts_as_ordered_and_rate_is_full` | 转采购单后：`ordered_qty` = 计划量、率 = 1、DONE、剩余 0、下钻带采购单号且 `counted = true` |
// | `cancelled_po_drops_out_of_ordered` | 采购单作废：`ordered_qty` 回落 0、**`req_qty` 不变**（申请还在）、原因「采购单已作废」 |
// | `partial_conversion_keeps_remaining_and_convertible` | 分批：转 4/10 → 已申请 4、已下单 0、`remaining_qty` 10、`convertible_qty` 6（两个口径不许混） |
// | `merged_po_is_not_double_counted` | 合并转单：同物料两仓并进一张采购单的一行，两个计划行各算自己那段量，合计等于采购单量而不是 2 倍 |
// | `over_plan_reports_over_state` | 超计划（库层刻意不拦）：率 1.5、OVER、剩余 −5、`min_rate=100` 能筛出来 |
// | `head_view_matches_item_summary` | 单头视图的合计 / 行计数与 `summary_of(明细)` 逐项相等（视图聚合与纯函数同一套算法） |
// | `overview_filters_and_hides_soft_deleted` | 总览筛选（年度 / 状态 / 执行率下限 / 关键字）+ 软删计划不进总览、详情 404 |
// | `zz_teardown_purges_fixtures` | 清场：`M404-` 夹具 0 残留 |
//
// # ⚠ 真库用例必须串行
//
// ```bash
// ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m404_drill" \
//   cargo test --lib business::erp::purchase::purchase_plan_execution_db_tests -- --test-threads=1
// ```
//
// 计划必须先**真走一遍审批**（`pp_approve`）才变 APPROVED；申请单也必须**真提交真审批**
// （`pr_approve`）才能转采购单 —— 本文件不直插 `UPDATE … SET status='APPROVED'`，
// 否则测的就不是「只有批准过的计划能转 / 只有批准过的申请能下单」。
// 唯一直插的是「超计划」那条：转申请入口会拦超出剩余量（M4-03 的口径），
// 现实里只能由库层被直接改过造成，所以用 SQL 造夹具（V0446 头注释 4 刻意不拦）。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::purchase_dto::PurchaseOrderStatusReq;
use crate::business::erp::purchase::purchase_handler;
use crate::business::erp::purchase::purchase_plan_bridge;
use crate::business::erp::purchase::purchase_plan_convert_dto::{ConvertLineReq, ToRequisitionReq};
use crate::business::erp::purchase::purchase_plan_convert_handler as convert;
use crate::business::erp::purchase::purchase_plan_dto::{CreatePlanReq, PlanItemReq, STATUS_SUBMITTED, SubmitPlanReq};
use crate::business::erp::purchase::purchase_plan_execution_dto::{
    ExecFacts, ExecutionPageQuery, PlanExecItemRes, PlanExecutionRes, STATE_DONE, STATE_NOT_STARTED, STATE_OVER,
    summary_of,
};
use crate::business::erp::purchase::purchase_plan_execution_handler as exec_handler;
use crate::business::erp::purchase::purchase_plan_handler as plan_handler;
use crate::business::erp::purchase::purchase_requisition_dto::{SubmitRequisitionReq, ToPoReq};
use crate::business::erp::purchase::purchase_requisition_handler as requisition_handler;
use crate::business::erp::requisition_bridge;
use crate::business::flow::engine;
use crate::test_seed;

/// 夹具名前缀（清场按它反查，绝不按生产号段删）。
const NAME_PREFIX: &str = "M404-";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 操作人（真实存在的 `sys_user`）：`biz_flow_instance.initiator_id` 有外键。
async fn operator() -> Uuid {
    static CACHED: std::sync::OnceLock<Uuid> = std::sync::OnceLock::new();
    if let Some(id) = CACHED.get() {
        return *id;
    }
    let rows = db()
        .find_with_params(
            "SELECT id FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at, username LIMIT 1",
            vec![],
        )
        .await
        .expect("查用户");
    let id = rows
        .first()
        .map(|row| row.get_typed::<Uuid>("id").expect("user id"))
        .expect("模板库里至少要有一个用户（发起审批要外键到 sys_user）");
    let _ = CACHED.set(id);
    id
}

/// 取串行锁 + 探库 + 清场 + 就绪「非制单人」的审批岗位成员。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    // 空库里往往只有一个 admin，而 `pp_approve` / `pr_approve` 都带职责分离内控
    // （65018：「审批人全部是制单人本人」直接拒派）—— 审批岗位必须有**非制单人**的成员，
    // 用例才走得完真审批。用统一的底座夹具（`test_seed` re-export，P2-62 口径），
    // 不在这里手写用户 / 角色成员夹具（test_seed_gate 的预算只许降）。
    test_seed::ensure_approver_roles().await;
    Some(guard)
}

/// 清场：采购单（含超计划用例直插的那张）→ 申请单（明细靠 FK CASCADE）→
/// 审批实例 / 待办 → 计划（明细 CASCADE）→ 夹具主数据。
async fn cleanup() {
    // 采购单：先按「来源计划」反查（真实转单生成的 PO-yyyyMMdd 号段），再按夹具前缀兜底
    exec(
        "DELETE FROM biz_purchase_order \
          WHERE id IN (SELECT po_id FROM biz_purchase_requisition \
                        WHERE source_plan_id IN (SELECT id FROM biz_purchase_plan \
                                                  WHERE plan_name LIKE 'M404-%')) \
             OR order_no LIKE 'M404-%'",
        vec![],
    )
    .await;
    exec(
        "DELETE FROM biz_purchase_requisition \
          WHERE source_plan_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M404-%') \
             OR req_no LIKE 'M404-%'",
        vec![],
    )
    .await;
    exec(
        "DELETE FROM biz_flow_task WHERE instance_id IN ( \
             SELECT i.id FROM biz_flow_instance i \
              JOIN biz_purchase_plan p ON p.id = i.biz_id \
             WHERE i.biz_type = 'PP_APPROVE' AND p.plan_name LIKE 'M404-%')",
        vec![],
    )
    .await;
    exec(
        "DELETE FROM biz_flow_instance i USING biz_purchase_plan p \
          WHERE i.biz_type = 'PP_APPROVE' AND i.biz_id = p.id AND p.plan_name LIKE 'M404-%'",
        vec![],
    )
    .await;
    exec("DELETE FROM biz_purchase_plan WHERE plan_name LIKE 'M404-%'", vec![]).await;
    exec("DELETE FROM biz_material WHERE material_code LIKE 'M404-MAT-%'", vec![]).await;
    exec(
        "DELETE FROM biz_warehouse WHERE warehouse_code LIKE 'M404-WH-%'",
        vec![],
    )
    .await;
    exec("DELETE FROM biz_supplier WHERE supplier_code LIKE 'M404-SUP-%'", vec![]).await;
}

/// 主数据：两个物料 × 两个仓库 + 一个部门 + 一个供应商。
struct Fixtures {
    material_a: Uuid,
    material_b: Uuid,
    warehouse_a: Uuid,
    warehouse_b: Uuid,
    dept: Uuid,
    supplier: Uuid,
}

async fn fixtures() -> Fixtures {
    Fixtures {
        material_a: test_seed::ensure_material_base_unit("M404-MAT-A", "M404 物料 A").await,
        material_b: test_seed::ensure_material_base_unit("M404-MAT-B", "M404 物料 B").await,
        warehouse_a: test_seed::ensure_warehouse("M404-WH-A", "M404 仓库 A").await,
        warehouse_b: test_seed::ensure_warehouse("M404-WH-B", "M404 仓库 B").await,
        dept: cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await,
        supplier: test_seed::ensure_supplier("M404-SUP-A", "M404 供应商 A").await,
    }
}

fn item(material: Uuid, warehouse: Uuid, qty: &str, price: &str) -> PlanItemReq {
    PlanItemReq {
        material_id: Some(material.to_string()),
        warehouse_id: Some(warehouse.to_string()),
        planned_qty: Some(dec(qty)),
        planned_price: Some(dec(price)),
        remark: None,
    }
}

fn plan_req(name: &str, year: i32, dept: Option<Uuid>, items: Vec<PlanItemReq>) -> CreatePlanReq {
    CreatePlanReq {
        plan_name: Some(format!("{NAME_PREFIX}{name}")),
        plan_type: Some("ANNUAL".to_string()),
        plan_year: Some(year),
        plan_month: None,
        parent_plan_id: None,
        dept_id: dept.map(|id| id.to_string()),
        planner_id: None,
        remark: Some("M404 执行率用例".to_string()),
        items,
    }
}

async fn status_and_instance(plan_id: Uuid) -> (String, Option<Uuid>) {
    let rows = db()
        .find_with_params(
            "SELECT status, approval_instance_id FROM biz_purchase_plan WHERE id = ?::uuid",
            vec![Value::from(&plan_id)],
        )
        .await
        .expect("查采购计划");
    let row = rows.first().expect("计划存在");
    (
        row.get_typed("status").expect("status"),
        row.get_typed("approval_instance_id").expect("instance"),
    )
}

async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, String, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id AS task_id, node_key, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY created_at",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查待办");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("task_id").expect("task id"),
                row.get_typed("node_key").expect("node key"),
                row.get_typed("assignee_id").expect("assignee"),
            )
        })
        .collect()
}

/// 审批同意一票（与 `flow::handler::approve_task` 同一条引擎路径 + 提交后回调）。
async fn approve_task(task_id: Uuid, operator: Uuid) -> String {
    let out = db()
        .transaction(|tx| async move { engine::approve(&tx, task_id, operator, Some("M404 用例同意")).await })
        .await
        .unwrap_or_else(|error| panic!("审批同意失败：{error}"));
    purchase_plan_bridge::on_pp_settled(out.instance_id, &out.instance_status).await;
    out.instance_status
}

/// 走完所有审批（逐节点真派真批）。
async fn approve_until_final(instance_id: Uuid) -> String {
    let mut last = String::from("PENDING");
    for _ in 0..5 {
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        for (task_id, _, assignee) in tasks {
            last = approve_task(task_id, assignee).await;
        }
        if last == "APPROVED" || last == "REJECTED" {
            break;
        }
    }
    last
}

/// 建一张计划（可选：真走完 `pp_approve` 到 APPROVED）。
async fn plan_of(name: &str, year: i32, dept: Option<Uuid>, items: Vec<PlanItemReq>, approve: bool) -> Uuid {
    let id = plan_handler::create_plan(&plan_req(name, year, dept, items), Some(operator().await))
        .await
        .unwrap_or_else(|error| panic!("建单失败：{} {}", error.code, error.msg));
    if !approve {
        return id;
    }
    plan_handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .unwrap_or_else(|error| panic!("提交审批失败：{} {}", error.code, error.msg));
    let (status, instance) = status_and_instance(id).await;
    assert_eq!(status, STATUS_SUBMITTED, "提交后必须 SUBMITTED");
    let final_status = approve_until_final(instance.expect("SUBMITTED 必有实例")).await;
    assert_eq!(final_status, "APPROVED", "审批应通过");
    id
}

/// 申请单真走一遍 `pr_approve` 到 APPROVED。
async fn approve_requisition(req_id: Uuid) {
    requisition_handler::submit_requisition(
        req_id,
        &SubmitRequisitionReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .unwrap_or_else(|error| panic!("申请提交审批失败：{} {}", error.code, error.msg));
    let rows = db()
        .find_with_params(
            "SELECT approval_instance_id FROM biz_purchase_requisition WHERE id = ?::uuid",
            vec![Value::from(&req_id)],
        )
        .await
        .expect("查申请单");
    let instance = rows
        .first()
        .and_then(|row| row.get_typed::<Option<Uuid>>("approval_instance_id").expect("instance"))
        .expect("提交后必有审批实例");
    for _ in 0..5 {
        let tasks = pending_tasks(instance).await;
        if tasks.is_empty() {
            break;
        }
        for (task_id, _, assignee) in tasks {
            let out = db()
                .transaction(|tx| async move {
                    engine::approve(&tx, task_id, assignee, Some("M404 用例同意申请")).await
                })
                .await
                .unwrap_or_else(|error| panic!("申请审批失败：{error}"));
            requisition_bridge::on_pr_settled(out.instance_id, &out.instance_status).await;
        }
    }
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_purchase_requisition WHERE id = ?::uuid",
            vec![Value::from(&req_id)],
        )
        .await
        .expect("查申请单");
    let status: String = rows.first().expect("申请单存在").get_typed("status").expect("status");
    assert_eq!(status, "APPROVED", "申请审批应通过");
}

/// 转采购单（多张 → 一张，同物料合并成一行），返回 `(po_id, po_no)`。
async fn to_po(req_ids: &[Uuid], supplier: Uuid, warehouse: Option<Uuid>) -> (Uuid, String) {
    let body = ToPoReq {
        requisition_ids: req_ids.iter().map(|id| id.to_string()).collect(),
        supplier_id: Some(supplier.to_string()),
        // 多张申请单的收货仓库不一致时必须显式指定（采购申请域的既有口径，66050）
        warehouse_id: warehouse.map(|id| id.to_string()),
        ..ToPoReq::default()
    };
    assert!(!body.requisition_ids.is_empty(), "用例必须至少给一张申请单");
    let out = requisition_handler::to_purchase_order(req_ids[0], &body, Some(operator().await), Utc::now())
        .await
        .unwrap_or_else(|error| panic!("转采购单失败：{} {}", error.code, error.msg));
    let po_id = Uuid::parse_str(&out.header.po_id).expect("po id");
    (po_id, out.header.po_no.clone())
}

/// 采购单作废（DRAFT → CANCELLED，真走状态机）。
async fn cancel_po(po_id: Uuid) {
    let req = PurchaseOrderStatusReq {
        status: Some("CANCELLED".to_string()),
        reason: Some("M404 用例作废采购单".to_string()),
        ..PurchaseOrderStatusReq::default()
    };
    purchase_handler::change_purchase_order_status(po_id, &req, operator().await)
        .await
        .unwrap_or_else(|error| panic!("作废采购单失败：{} {}", error.code, error.msg));
}

/// 执行率详情（与 HTTP handler 同一条路径）。
async fn execution_of(plan_id: Uuid) -> PlanExecutionRes {
    exec_handler::execution_of(plan_id)
        .await
        .unwrap_or_else(|error| panic!("查执行率失败：{} {}", error.code, error.msg))
}

/// 按行号取明细（找不到直接 panic：行号对不上说明视图排序错了）。
fn line(res: &PlanExecutionRes, line_no: i32) -> &PlanExecItemRes {
    res.items
        .iter()
        .find(|item| item.line_no == line_no)
        .unwrap_or_else(|| panic!("执行率明细里没有第 {line_no} 行"))
}

/// 总览筛选（与 HTTP handler 同一条路径）。
async fn page_of(
    query: ExecutionPageQuery,
) -> Vec<crate::business::erp::purchase::purchase_plan_execution_dto::PlanExecRowRes> {
    let filter = query
        .filter()
        .unwrap_or_else(|error| panic!("筛选参数非法：{}", error.msg));
    let (rows, _total) = exec_handler::page_of(&filter)
        .await
        .unwrap_or_else(|error| panic!("查总览失败：{} {}", error.code, error.msg));
    rows
}

// ==================== 用例 ====================

/// ① 新计划：率 0、两行都是未开工、剩余 = 计划量、没有来源单据。
#[tokio::test]
async fn fresh_plan_has_zero_execution() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = plan_of(
        "新计划",
        2050,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "100", "2.5"),
            item(fx.material_b, fx.warehouse_b, "10", "100"),
        ],
        false,
    )
    .await;

    let res = execution_of(plan_id).await;
    assert_eq!(res.plan.line_count, 2);
    assert_eq!(res.plan.planned_qty, dec("110"));
    assert_eq!(res.plan.req_qty, Decimal::ZERO, "没转申请：已申请 0");
    assert_eq!(res.plan.ordered_qty, Decimal::ZERO, "没下单：已下单 0");
    assert_eq!(res.plan.remaining_qty, dec("110"), "剩余 = 计划 − 已下单");
    assert_eq!(res.plan.convertible_qty, dec("110"), "可转 = 计划 − 已申请");
    assert_eq!(res.plan.exec_rate, Some(Decimal::ZERO));
    assert_eq!(res.plan.exec_rate_pct, Some(dec("0.00")));
    assert_eq!(res.plan.exec_state, STATE_NOT_STARTED);
    assert_eq!(res.plan.not_started_lines, 2);
    assert_eq!(res.plan.done_lines, 0);

    for item in &res.items {
        assert_eq!(item.ordered_qty, Decimal::ZERO);
        assert_eq!(item.exec_state, STATE_NOT_STARTED);
        assert!(item.sources.is_empty(), "第 {} 行不该有来源单据", item.line_no);
        assert!(!item.over_plan);
    }

    cleanup().await;
}

/// ② 转了申请没下单：`req_qty` = 计划量、`ordered_qty` = 0、回写列与单据链不分叉。
#[tokio::test]
async fn requisition_without_po_counts_as_requested_only() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = plan_of(
        "转申请未下单",
        2051,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "100", "2.5"),
            item(fx.material_b, fx.warehouse_b, "10", "100"),
        ],
        true,
    )
    .await;

    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转申请应成功");
    assert_eq!(out.requisitions.len(), 2, "一仓一单");

    let res = execution_of(plan_id).await;
    assert_eq!(res.plan.req_qty, dec("110"), "已申请 = 转出量");
    assert_eq!(res.plan.booked_qty, dec("110"), "回写列 executed_qty 也应是 110");
    assert_eq!(
        res.plan.booked_qty, res.plan.req_qty,
        "回写列与单据链不许分叉（booked == req）"
    );
    assert_eq!(res.plan.ordered_qty, Decimal::ZERO, "没有采购单：已下单 0");
    assert_eq!(res.plan.exec_rate, Some(Decimal::ZERO), "执行率对已下单算，不看已申请");
    assert_eq!(res.plan.req_rate, Some(dec("1")), "已申请率 100%");
    assert_eq!(res.plan.req_rate_pct, Some(dec("100.00")));
    assert_eq!(res.plan.exec_state, STATE_NOT_STARTED);
    assert_eq!(res.plan.convertible_qty, Decimal::ZERO, "全转完，可转为 0");
    assert_eq!(res.plan.remaining_qty, dec("110"), "但还没下单，剩余仍是 110");

    let first = line(&res, 1);
    assert_eq!(first.req_qty, dec("100"));
    assert_eq!(first.ordered_qty, Decimal::ZERO);
    assert_eq!(first.req_doc_count, 1);
    assert_eq!(first.po_doc_count, 0);
    assert_eq!(first.sources.len(), 1, "第 1 行只有一张申请明细");
    let source = &first.sources[0];
    assert!(!source.counted, "没转采购单 → 不计入已下单");
    assert_eq!(source.reason, "未转采购单");
    assert!(source.req_no.starts_with("PRQ-"), "来源单号要带出来：{}", source.req_no);
    assert!(source.po_id.is_none() && source.po_no.is_none());
    assert_eq!(source.qty, dec("100"));

    cleanup().await;
}

/// ③ 转采购单后：已下单 = 计划量、执行率 100%、DONE、剩余 0、下钻带采购单号。
#[tokio::test]
async fn converted_po_counts_as_ordered_and_rate_is_full() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = plan_of(
        "转采购单",
        2052,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "10", "2.5")],
        true,
    )
    .await;

    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转申请应成功");
    let req_id = Uuid::parse_str(&out.requisitions[0].requisition_id).expect("req id");
    approve_requisition(req_id).await;
    let (_po_id, po_no) = to_po(&[req_id], fx.supplier, None).await;

    let res = execution_of(plan_id).await;
    assert_eq!(res.plan.ordered_qty, dec("10"), "整行已下单");
    assert_eq!(res.plan.req_qty, dec("10"));
    assert_eq!(res.plan.remaining_qty, Decimal::ZERO);
    assert_eq!(res.plan.exec_rate, Some(dec("1")));
    assert_eq!(res.plan.exec_rate_pct, Some(dec("100.00")));
    assert_eq!(res.plan.exec_state, STATE_DONE, "下满不是 OVER");
    assert_eq!(res.plan.done_lines, 1);
    assert_eq!(res.plan.over_lines, 0);

    let first = line(&res, 1);
    assert_eq!(first.ordered_qty, dec("10"));
    assert_eq!(first.po_doc_count, 1);
    assert!(!first.over_plan, "恰好下满不算超计划");
    let source = &first.sources[0];
    assert!(source.counted, "已转采购单 → 计入已下单");
    assert_eq!(source.reason, "已下单");
    assert_eq!(source.po_no.as_deref(), Some(po_no.as_str()));
    assert_eq!(
        source.po_status.as_deref(),
        Some("DRAFT"),
        "转出来的采购单是草稿，但仍算已下单"
    );
    assert_eq!(source.po_status_label.as_deref(), Some("草稿"));

    cleanup().await;
}

/// ④ 采购单作废：已下单回落 0，但已申请不变（申请还在），原因点出「已作废」。
#[tokio::test]
async fn cancelled_po_drops_out_of_ordered() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = plan_of(
        "采购单作废",
        2053,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "8", "1")],
        true,
    )
    .await;

    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转申请应成功");
    let req_id = Uuid::parse_str(&out.requisitions[0].requisition_id).expect("req id");
    approve_requisition(req_id).await;
    let (po_id, _po_no) = to_po(&[req_id], fx.supplier, None).await;

    assert_eq!(execution_of(plan_id).await.plan.ordered_qty, dec("8"));
    cancel_po(po_id).await;

    let res = execution_of(plan_id).await;
    assert_eq!(res.plan.ordered_qty, Decimal::ZERO, "作废后不许再算已下单");
    assert_eq!(res.plan.req_qty, dec("8"), "申请单还在：已申请不变");
    assert_eq!(res.plan.remaining_qty, dec("8"), "剩余回到 8");
    assert_eq!(res.plan.exec_rate, Some(Decimal::ZERO));
    assert_eq!(res.plan.exec_state, STATE_NOT_STARTED);

    let source = &line(&res, 1).sources[0];
    assert!(!source.counted);
    assert_eq!(source.reason, "采购单已作废");
    assert_eq!(source.po_status.as_deref(), Some("CANCELLED"));
    assert_eq!(source.po_status_label.as_deref(), Some("已取消"));

    cleanup().await;
}

/// ⑤ 分批：转 4/10 → 已申请 4、已下单 0、剩余 10、可转 6（两个口径不许混）。
#[tokio::test]
async fn partial_conversion_keeps_remaining_and_convertible() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = plan_of(
        "分批转申请",
        2054,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "10", "1")],
        true,
    )
    .await;
    let plan_item_id = plan_item_ids(plan_id).await[0];

    let req = ToRequisitionReq {
        items: vec![ConvertLineReq {
            plan_item_id: Some(plan_item_id.to_string()),
            qty: Some(dec("4")),
            est_price: None,
        }],
        ..ToRequisitionReq::default()
    };
    convert::to_requisition(plan_id, &req, Some(operator().await))
        .await
        .expect("部分转申请应成功");

    let res = execution_of(plan_id).await;
    assert_eq!(res.plan.planned_qty, dec("10"));
    assert_eq!(res.plan.req_qty, dec("4"), "只转了 4");
    assert_eq!(res.plan.booked_qty, dec("4"), "回写列也应是 4");
    assert_eq!(res.plan.ordered_qty, Decimal::ZERO);
    assert_eq!(res.plan.remaining_qty, dec("10"), "还没下单：剩余仍是 10");
    assert_eq!(res.plan.convertible_qty, dec("6"), "还能再转 6");
    assert_eq!(res.plan.req_rate, Some(dec("0.4")));

    cleanup().await;
}

/// ⑥ 合并转单：同物料两仓并进一张采购单的一行 —— 两个计划行各算自己那段量。
#[tokio::test]
async fn merged_po_is_not_double_counted() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    // 同一物料挂在两个仓库 → 两个计划行 → 转出两张申请单（一仓一单）→ 合并成一张采购单的一行
    let plan_id = plan_of(
        "合并转单",
        2055,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "10", "2"),
            item(fx.material_a, fx.warehouse_b, "4", "2"),
        ],
        true,
    )
    .await;

    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转申请应成功");
    assert_eq!(out.requisitions.len(), 2, "两个仓库 → 两张申请单");
    let mut req_ids: Vec<Uuid> = out
        .requisitions
        .iter()
        .map(|row| Uuid::parse_str(&row.requisition_id).expect("req id"))
        .collect();
    for req_id in &req_ids {
        approve_requisition(*req_id).await;
    }
    req_ids.sort();
    let (po_id, _po_no) = to_po(&req_ids, fx.supplier, Some(fx.warehouse_a)).await;

    // 采购单那边只有一行（同物料合并），量 = 10 + 4
    let po_qty: Decimal = db()
        .scalar_with_params(
            "SELECT COALESCE(SUM(qty), 0) FROM biz_purchase_order_item WHERE order_id = ?::uuid",
            vec![Value::from(&po_id)],
        )
        .await
        .expect("查采购明细合计");
    assert_eq!(po_qty, dec("14"), "采购单一行合并成 14");

    let res = execution_of(plan_id).await;
    assert_eq!(line(&res, 1).ordered_qty, dec("10"), "仓库 A 那行只算自己 10");
    assert_eq!(line(&res, 2).ordered_qty, dec("4"), "仓库 B 那行只算自己 4");
    assert_eq!(
        res.plan.ordered_qty,
        dec("14"),
        "合计 = 采购单量，**不是** 28（合并行不许给每个计划行各记一遍）"
    );
    assert_eq!(res.plan.exec_rate, Some(dec("1")));
    assert_eq!(res.plan.exec_state, STATE_DONE);

    cleanup().await;
}

/// ⑦ 超计划：率 1.5、OVER、剩余 −5、`min_rate=100` 能筛出来。
#[tokio::test]
async fn over_plan_reports_over_state() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = plan_of(
        "超计划",
        2056,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "10", "1")],
        true,
    )
    .await;
    let plan_item_id = plan_item_ids(plan_id).await[0];

    // 直插夹具：现实里只有「库层被直接改过」才可能超计划（转申请入口会拦剩余量，M4-03 口径）
    // 三条语句分别执行（驱动的 execute 是 prepared statement，一次只许一条语句）
    let over_req = Uuid::new_v4();
    let over_po = Uuid::new_v4();
    exec(
        &format!(
            "INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status) \
             VALUES ('{over_po}', 'M404-PO-OVER-0001', '{supplier}', DATE '2026-11-18', '{warehouse}', 'APPROVED')",
            supplier = fx.supplier,
            warehouse = fx.warehouse_a,
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "INSERT INTO biz_purchase_requisition \
                 (id, req_no, dept_id, warehouse_id, status, po_id, po_no, converted_at, source_plan_id) \
             VALUES ('{over_req}', 'M404-PRQ-OVER-0001', '{dept}', '{warehouse}', 'CONVERTED', \
                     '{over_po}', 'M404-PO-OVER-0001', now(), '{plan}')",
            dept = fx.dept,
            warehouse = fx.warehouse_a,
            plan = plan_id,
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "INSERT INTO biz_purchase_requisition_item \
                 (requisition_id, line_no, material_id, unit_id, qty, plan_item_id) \
             SELECT '{over_req}', 1, material_id, unit_id, 15, id \
               FROM biz_purchase_plan_item WHERE id = '{item}'",
            item = plan_item_id,
        ),
        vec![],
    )
    .await;

    let res = execution_of(plan_id).await;
    assert_eq!(res.plan.planned_qty, dec("10"));
    assert_eq!(res.plan.ordered_qty, dec("15"), "10 计划 + 5 超量");
    assert_eq!(res.plan.remaining_qty, dec("-5"), "剩余必须保留负数");
    assert_eq!(res.plan.exec_rate, Some(dec("1.5")));
    assert_eq!(res.plan.exec_rate_pct, Some(dec("150.00")));
    assert_eq!(res.plan.exec_state, STATE_OVER);
    assert_eq!(res.plan.over_lines, 1);
    assert_eq!(res.plan.done_lines, 0);

    let first = line(&res, 1);
    assert!(first.over_plan, "超计划必须标出来");
    assert_eq!(first.exec_state, STATE_OVER);
    assert_eq!(first.exec_state_label, "超计划");
    assert_eq!(first.remaining_qty, dec("-5"));

    // min_rate = 100%：这张计划必须被筛出来；关键字也认（本夹具独一份）
    let rows = page_of(ExecutionPageQuery {
        keyword: Some("M404-超计划".to_string()),
        min_rate: Some(100),
        ..ExecutionPageQuery::default()
    })
    .await;
    assert_eq!(rows.len(), 1, "min_rate=100 必须命中这张超计划单");
    assert_eq!(rows[0].plan_id, plan_id.to_string());
    assert_eq!(rows[0].exec_state, STATE_OVER);

    // min_rate = 200%：它已经不达标了（1.5 < 2）
    let rows = page_of(ExecutionPageQuery {
        keyword: Some("M404-超计划".to_string()),
        min_rate: Some(200),
        ..ExecutionPageQuery::default()
    })
    .await;
    assert!(rows.is_empty(), "min_rate=200 不该命中 150% 的单");

    cleanup().await;
}

/// ⑧ 单头视图与 `summary_of(明细)` 逐项相等（视图聚合与纯函数同一套算法）。
#[tokio::test]
async fn head_view_matches_item_summary() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    // 三行：一行下满、一行转申请未下单、一行没动
    let plan_id = plan_of(
        "混合状态",
        2057,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "6", "1"),
            item(fx.material_b, fx.warehouse_a, "9", "1"),
            item(fx.material_a, fx.warehouse_b, "3", "1"),
        ],
        true,
    )
    .await;
    let items = plan_item_ids(plan_id).await;

    // 只把第 1 行转申请并下单
    let req = ToRequisitionReq {
        items: vec![ConvertLineReq {
            plan_item_id: Some(items[0].to_string()),
            qty: None,
            est_price: None,
        }],
        ..ToRequisitionReq::default()
    };
    let out = convert::to_requisition(plan_id, &req, Some(operator().await))
        .await
        .expect("转申请应成功");
    let req_id = Uuid::parse_str(&out.requisitions[0].requisition_id).expect("req id");
    approve_requisition(req_id).await;
    let (_, _po) = to_po(&[req_id], fx.supplier, None).await;

    // 第 2 行也转申请（不下单）
    let req = ToRequisitionReq {
        items: vec![ConvertLineReq {
            plan_item_id: Some(items[1].to_string()),
            qty: Some(dec("5")),
            est_price: None,
        }],
        ..ToRequisitionReq::default()
    };
    convert::to_requisition(plan_id, &req, Some(operator().await))
        .await
        .expect("第 2 行部分转申请应成功");

    let res = execution_of(plan_id).await;
    let facts: Vec<ExecFacts> = res
        .items
        .iter()
        .map(|item| ExecFacts {
            planned_qty: item.planned_qty,
            booked_qty: item.booked_qty,
            req_qty: item.req_qty,
            ordered_qty: item.ordered_qty,
        })
        .collect();
    let summary = summary_of(&facts);

    assert_eq!(res.plan.line_count, summary.line_count);
    assert_eq!(res.plan.planned_qty, summary.planned_qty);
    assert_eq!(res.plan.booked_qty, summary.booked_qty);
    assert_eq!(res.plan.req_qty, summary.req_qty);
    assert_eq!(res.plan.ordered_qty, summary.ordered_qty);
    assert_eq!(res.plan.remaining_qty, summary.remaining_qty);
    assert_eq!(res.plan.exec_rate, summary.exec_rate);
    assert_eq!(res.plan.req_rate, summary.req_rate);
    assert_eq!(res.plan.not_started_lines, summary.not_started_lines);
    assert_eq!(res.plan.partial_lines, summary.partial_lines);
    assert_eq!(res.plan.done_lines, summary.done_lines);
    assert_eq!(res.plan.over_lines, summary.over_lines);

    // 具体形态也钉一下：1 行下满 + 1 行部分申请未下单 + 1 行未开工
    assert_eq!(res.plan.done_lines, 1);
    assert_eq!(res.plan.not_started_lines, 2, "第 2、3 行都没下单");
    assert_eq!(res.plan.planned_qty, dec("18"));
    assert_eq!(res.plan.ordered_qty, dec("6"));
    assert_eq!(res.plan.exec_rate, Some(dec("0.333333")));

    cleanup().await;
}

/// ⑨ 总览筛选 + 软删计划隐藏（详情 404）。
#[tokio::test]
async fn overview_filters_and_hides_soft_deleted() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = plan_of(
        "总览筛选",
        2058,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "5", "1")],
        false,
    )
    .await;
    let other_id = plan_of(
        "总览筛选另一张",
        2058,
        None,
        vec![item(fx.material_b, fx.warehouse_b, "7", "1")],
        false,
    )
    .await;

    // 关键字命中两张（同一前缀）
    let rows = page_of(ExecutionPageQuery {
        keyword: Some("M404-总览筛选".to_string()),
        plan_year: Some(2058),
        ..ExecutionPageQuery::default()
    })
    .await;
    assert_eq!(rows.len(), 2, "两张计划都该命中，实际 {} 张", rows.len());
    assert!(rows.iter().all(|row| row.plan_year == 2058));
    assert!(rows.iter().all(|row| row.exec_state == STATE_NOT_STARTED));

    // 状态筛选：两张都是 DRAFT
    let rows = page_of(ExecutionPageQuery {
        keyword: Some("M404-总览筛选".to_string()),
        status: Some("DRAFT".to_string()),
        ..ExecutionPageQuery::default()
    })
    .await;
    assert_eq!(rows.len(), 2, "DRAFT 筛选应命中");

    // 年度筛选：换一年就没
    let rows = page_of(ExecutionPageQuery {
        keyword: Some("M404-总览筛选".to_string()),
        plan_year: Some(2059),
        ..ExecutionPageQuery::default()
    })
    .await;
    assert!(rows.is_empty(), "2059 年不该有计划");

    // 软删一张 → 总览少一张、详情 404
    plan_handler::delete_plan(plan_id, Some(operator().await))
        .await
        .expect("草稿计划应能删");
    let rows = page_of(ExecutionPageQuery {
        keyword: Some("M404-总览筛选".to_string()),
        plan_year: Some(2058),
        ..ExecutionPageQuery::default()
    })
    .await;
    assert_eq!(rows.len(), 1, "软删的计划不进总览");
    assert_eq!(rows[0].plan_id, other_id.to_string());

    let error = exec_handler::execution_of(plan_id)
        .await
        .expect_err("已删计划的执行率必须 404");
    assert_eq!(error.code, cloudpivot_kernel::shared::error_code::RESOURCE_NOT_FOUND);

    cleanup().await;
}

/// ⑩ 清场：`M404-` 夹具 0 残留。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else {
        return;
    };
    fixtures().await;
    // 造一点残留再清（证明 cleanup 真的按前缀删）
    plan_of(
        "清场探针",
        2060,
        None,
        vec![item(
            test_seed::ensure_material_base_unit("M404-MAT-T", "M404 清场物料").await,
            test_seed::ensure_warehouse("M404-WH-T", "M404 清场仓库").await,
            "1",
            "1",
        )],
        false,
    )
    .await;
    cleanup().await;

    for (table, column, pattern) in [
        ("biz_purchase_plan", "plan_name", "M404-%"),
        ("biz_purchase_requisition", "req_no", "M404-%"),
        ("biz_purchase_order", "order_no", "M404-%"),
        ("biz_material", "material_code", "M404-MAT-%"),
        ("biz_warehouse", "warehouse_code", "M404-WH-%"),
    ] {
        let left: i64 = db()
            .scalar_with_params(
                &format!("SELECT count(*) FROM {table} WHERE {column} LIKE ?"),
                vec![Value::Text(pattern.to_string())],
            )
            .await
            .unwrap_or_else(|error| panic!("查 {table} 残留失败：{error}"));
        assert_eq!(left, 0, "{table} 还残留 {left} 行（前缀 {pattern}）");
    }
}

/// 计划明细 id（按行号升序）。
async fn plan_item_ids(plan_id: Uuid) -> Vec<Uuid> {
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_purchase_plan_item WHERE plan_id = ?::uuid ORDER BY line_no",
            vec![Value::from(&plan_id)],
        )
        .await
        .expect("查明细");
    rows.iter().map(|row| row.get_typed("id").expect("item id")).collect()
}

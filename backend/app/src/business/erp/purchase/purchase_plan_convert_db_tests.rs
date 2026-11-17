// ============================================================================
// purchase_plan_convert_db_tests.rs —— 采购计划 → 采购申请转换（M4-03）的真库用例
//
// 卡：t_03a60b7a（M4-03【M4·采购】采购计划：→ 采购申请转换）
//
// # 覆盖什么（每条都对应卡面 / V0446 / V0448 里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `approved_plan_converts_one_requisition_per_warehouse` | 一仓一单：3 行 / 2 仓 → 2 张申请单；单号 `PRQ-yyyyMMdd-NNNNNN`；明细按计划行号；`est_amount` 服务端算 |
// | `provenance_columns_are_written` | 溯源：申请单头 `source_plan_id`、申请明细 `plan_item_id` 都指向计划（V0448 两列） |
// | `executed_qty_and_totals_are_written_back` | 回写：明细 `executed_qty += 转出量`，单头 `total_executed_qty` 跟着重算 |
// | `draft_and_submitted_plans_cannot_convert` | 状态闸：DRAFT / SUBMITTED 都 409，报文点出当前状态 |
// | `partial_then_full_conversion_then_refused` | 分批：先转一部分，再转剩余，第三次 409（剩余可转量都是 0） |
// | `over_remaining_is_rejected_with_numbers` | 超出剩余量 400，报文点出剩余量与计划行号（不静默截断、不撞库 500） |
// | `zero_price_line_needs_est_price_override` | 计划没单价 → 400；请求覆盖单价后放行 |
// | `company_level_plan_needs_dept` | 公司级计划（无编制部门）必须给 `dept_id`（申请单 dept NOT NULL） |
// | `foreign_plan_item_is_rejected` | `plan_items` 必须属于这张计划 → 400 |
// | `converted_requisition_is_a_real_requisition` | 生成的单能被采购申请域读到（detail_of），仓库 / 部门 / 单位 / 税率都对 |
// | `zz_teardown_purges_fixtures` | 清场：`M403-` 夹具 0 残留 |
//
// # ⚠ 真库用例必须串行
//
// ```bash
// ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m403_drill" \
//   cargo test --lib business::erp::purchase::purchase_plan_convert_db_tests -- --test-threads=1
// ```
//
// 计划必须先**真走一遍审批**（`pp_approve`，V0447 种下）才变 APPROVED —— 本文件不直插
// `UPDATE … SET status='APPROVED'`：那样测的就不是「只有批准过的计划能转」。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::purchase_plan_bridge;
use crate::business::erp::purchase::purchase_plan_convert_dto::{ConvertLineReq, ToRequisitionReq};
use crate::business::erp::purchase::purchase_plan_convert_handler as convert;
use crate::business::erp::purchase::purchase_plan_dto::{
    CreatePlanReq, PlanItemReq, STATUS_DRAFT, STATUS_SUBMITTED, SubmitPlanReq,
};
use crate::business::erp::purchase::purchase_plan_handler as plan_handler;
use crate::business::erp::purchase::purchase_requisition_handler as requisition_handler;
use crate::business::flow::engine;
use crate::test_seed;

/// 夹具名前缀（清场按它反查，绝不按生产号段删）。
const NAME_PREFIX: &str = "M403-";

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
    // 用例里那张 `approved_plan` 要真走完 `pp_approve`，而该流程带职责分离内控：
    // 空库只剩 admin 时 65018（「审批人全部是制单人本人」）会把提交直接拒掉。
    // 审批岗位成员走底座夹具（`test_seed` re-export，P2-62：测试代码不手写用户 / 角色成员）。
    test_seed::ensure_approver_roles().await;
    Some(guard)
}

/// 清场：申请单（明细靠 FK CASCADE）→ 审批实例 / 待办 → 计划（明细 CASCADE）→ 夹具主数据。
async fn cleanup() {
    exec(
        "DELETE FROM biz_purchase_requisition \
          WHERE source_plan_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M403-%')",
        vec![],
    )
    .await;
    exec(
        "DELETE FROM biz_flow_task WHERE instance_id IN ( \
             SELECT i.id FROM biz_flow_instance i \
              JOIN biz_purchase_plan p ON p.id = i.biz_id \
             WHERE i.biz_type = 'PP_APPROVE' AND p.plan_name LIKE 'M403-%')",
        vec![],
    )
    .await;
    exec(
        "DELETE FROM biz_flow_instance i USING biz_purchase_plan p \
          WHERE i.biz_type = 'PP_APPROVE' AND i.biz_id = p.id AND p.plan_name LIKE 'M403-%'",
        vec![],
    )
    .await;
    exec("DELETE FROM biz_purchase_plan WHERE plan_name LIKE 'M403-%'", vec![]).await;
    exec("DELETE FROM biz_material WHERE material_code LIKE 'M403-MAT-%'", vec![]).await;
}

/// 主数据：两个物料 × 两个仓库 + 一个部门（部门走 `test_helpers::ensure_any_dept`，
/// 不自己造 —— 数据范围 / 预算等下游逻辑都假定部门来自种子里那套）。
struct Fixtures {
    material_a: Uuid,
    material_b: Uuid,
    warehouse_a: Uuid,
    warehouse_b: Uuid,
    dept: Uuid,
}

async fn fixtures() -> Fixtures {
    Fixtures {
        material_a: test_seed::ensure_material_base_unit("M403-MAT-A", "M403 物料 A").await,
        material_b: test_seed::ensure_material_base_unit("M403-MAT-B", "M403 物料 B").await,
        warehouse_a: test_seed::ensure_warehouse("M403-WH-A", "M403 仓库 A").await,
        warehouse_b: test_seed::ensure_warehouse("M403-WH-B", "M403 仓库 B").await,
        dept: cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await,
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
        remark: None,
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
        .transaction(|tx| async move { engine::approve(&tx, task_id, operator, Some("M403 用例同意")).await })
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

/// 建一张计划并**真走完审批**到 APPROVED。
async fn approved_plan(name: &str, year: i32, dept: Option<Uuid>, items: Vec<PlanItemReq>) -> Uuid {
    let id = plan_handler::create_plan(&plan_req(name, year, dept, items), Some(operator().await))
        .await
        .unwrap_or_else(|error| panic!("建单失败：{} {}", error.code, error.msg));
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

async fn plan_item_ids(plan_id: Uuid) -> Vec<(Uuid, i32, Decimal)> {
    let rows = db()
        .find_with_params(
            "SELECT id, line_no, executed_qty FROM biz_purchase_plan_item \
              WHERE plan_id = ?::uuid ORDER BY line_no",
            vec![Value::from(&plan_id)],
        )
        .await
        .expect("查明细");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("id").expect("item id"),
                row.get_typed("line_no").expect("line_no"),
                row.get_typed("executed_qty").expect("executed_qty"),
            )
        })
        .collect()
}

// ==================== 用例 ====================

/// ① 一仓一单：3 行 / 2 仓 → 2 张申请单，单号形状、明细、金额都对。
#[tokio::test]
async fn approved_plan_converts_one_requisition_per_warehouse() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "一仓一单",
        2040,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "10", "2.5"),
            item(fx.material_b, fx.warehouse_a, "2", "100"),
            item(fx.material_a, fx.warehouse_b, "4", "3"),
        ],
    )
    .await;

    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转换应成功");

    assert_eq!(out.requisitions.len(), 2, "两个仓库 → 两张申请单");
    assert_eq!(out.converted_lines, 3, "三个计划行全部转出");
    assert_eq!(out.converted_qty, dec("16"), "10 + 2 + 4");
    assert_eq!(out.converted_amount, dec("237"), "25 + 200 + 12");

    let first = &out.requisitions[0];
    let second = &out.requisitions[1];
    assert_eq!(first.warehouse_id, fx.warehouse_a.to_string(), "仓库 A 单在前");
    assert_eq!(first.lines.len(), 2);
    assert_eq!(first.est_amount, dec("225"), "25 + 200");
    assert_eq!(first.line_count, 2);
    assert_eq!(second.warehouse_id, fx.warehouse_b.to_string());
    assert_eq!(second.est_amount, dec("12"));

    for requisition in &out.requisitions {
        let no = &requisition.req_no;
        assert_eq!(no.len(), 19, "单号必须是 PRQ-yyyyMMdd-NNNNNN，实际 {no}");
        assert!(no.starts_with("PRQ-"), "单号必须以 PRQ- 开头，实际 {no}");
        assert_eq!(requisition.dept_id, fx.dept.to_string());
        assert!(!requisition.warehouse_name.is_empty(), "仓库名要带出来");
    }
    // 明细按计划行号升序（仓库 A 的两行是计划第 1、2 行）
    let lines: Vec<i32> = first.lines.iter().map(|line| line.plan_line_no).collect();
    assert_eq!(lines, vec![1, 2], "申请明细按计划行号排序");
    assert_eq!(first.lines[0].qty, dec("10"));
    assert_eq!(first.lines[0].est_price, dec("2.5"));
    assert_eq!(first.lines[0].amount, dec("25"));
    assert_eq!(first.lines[0].remaining_after, Decimal::ZERO, "全转后剩余 0");

    cleanup().await;
}

/// ② 溯源：申请单头 `source_plan_id` + 明细 `plan_item_id` 都指回计划（V0448 的两列）。
#[tokio::test]
async fn provenance_columns_are_written() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "溯源",
        2041,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "3", "1"),
            item(fx.material_b, fx.warehouse_b, "5", "2"),
        ],
    )
    .await;
    let plan_items = plan_item_ids(plan_id).await;

    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转换");

    for requisition in &out.requisitions {
        let requisition_id = Uuid::parse_str(&requisition.requisition_id).expect("uuid");
        let rows = db()
            .find_with_params(
                "SELECT source_plan_id FROM biz_purchase_requisition WHERE id = ?::uuid",
                vec![Value::from(&requisition_id)],
            )
            .await
            .expect("查申请单");
        let head = rows.first().expect("申请单存在");
        assert_eq!(
            head.get_typed::<Uuid>("source_plan_id").expect("source_plan_id"),
            plan_id,
            "申请单头必须记来源计划"
        );
    }

    let rows = db()
        .find_with_params(
            "SELECT plan_item_id FROM biz_purchase_requisition_item it \
               JOIN biz_purchase_requisition r ON r.id = it.requisition_id \
              WHERE r.source_plan_id = ?::uuid ORDER BY it.line_no",
            vec![Value::from(&plan_id)],
        )
        .await
        .expect("查申请明细");
    let got: Vec<Uuid> = rows
        .iter()
        .map(|row| row.get_typed::<Uuid>("plan_item_id").expect("plan_item_id"))
        .collect();
    // 两张申请单各 1 行；集合必须等于计划的两行
    let mut expected: Vec<Uuid> = plan_items.iter().map(|(id, _, _)| *id).collect();
    let mut got_sorted = got.clone();
    got_sorted.sort();
    expected.sort();
    assert_eq!(got_sorted, expected, "申请明细的 plan_item_id 必须逐行指回计划");

    cleanup().await;
}

/// ③ 回写：明细 `executed_qty` 与单头 `total_executed_qty` 都跟着走。
#[tokio::test]
async fn executed_qty_and_totals_are_written_back() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "回写",
        2042,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "10", "2"),
            item(fx.material_b, fx.warehouse_b, "6", "1.5"),
        ],
    )
    .await;

    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转换");

    let items = plan_item_ids(plan_id).await;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].2, dec("10"), "第 1 行已执行量 = 计划量");
    assert_eq!(items[1].2, dec("6"), "第 2 行已执行量 = 计划量");
    assert_eq!(out.plan.total_executed_qty, dec("16"), "单头合计已重算");
    assert_eq!(out.plan.total_planned_qty, dec("16"), "计划量不变");
    assert_eq!(out.plan.total_planned_amount, dec("29"), "计划金额不变（20 + 9）");

    // 库里也必须是这个值（不是只改了内存里的 DTO）
    let rows = db()
        .find_with_params(
            "SELECT total_executed_qty FROM biz_purchase_plan WHERE id = ?::uuid",
            vec![Value::from(&plan_id)],
        )
        .await
        .expect("查单头");
    let total: Decimal = rows
        .first()
        .expect("计划存在")
        .get_typed("total_executed_qty")
        .expect("total_executed_qty");
    assert_eq!(total, dec("16"), "落库的单头合计必须是 16");

    cleanup().await;
}

/// ④ 状态闸：DRAFT / SUBMITTED 都 409（报文点出当前状态）。
#[tokio::test]
async fn draft_and_submitted_plans_cannot_convert() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let draft_id = plan_handler::create_plan(
        &plan_req(
            "草稿不能转",
            2043,
            Some(fx.dept),
            vec![item(fx.material_a, fx.warehouse_a, "1", "1")],
        ),
        Some(operator().await),
    )
    .await
    .expect("建单");

    let error = convert::to_requisition(draft_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect_err("草稿不能转");
    assert_eq!(
        error.code,
        cloudpivot_kernel::shared::error_code::ERP_REQUISITION_STATUS_GUARD
    );
    assert!(error.msg.contains("DRAFT"), "报文要点出当前状态：{}", error.msg);
    assert!(error.msg.contains("APPROVED"), "报文要说清前置条件：{}", error.msg);
    assert_eq!(
        status_and_instance(draft_id).await.0,
        STATUS_DRAFT,
        "失败的转换不能改状态"
    );

    // 提交后（SUBMITTED）同样不能转
    plan_handler::submit_plan(
        draft_id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交");
    let error = convert::to_requisition(draft_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect_err("审批中不能转");
    assert!(error.msg.contains("SUBMITTED"), "报文要点出审批中：{}", error.msg);
    assert_eq!(
        plan_item_ids(draft_id).await[0].2,
        Decimal::ZERO,
        "失败的转换不能写已执行量"
    );

    cleanup().await;
}

/// ⑤ 分批：先转一部分，再转剩余，第三次 409。
#[tokio::test]
async fn partial_then_full_conversion_then_refused() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "分批",
        2044,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "10", "2")],
    )
    .await;
    let items = plan_item_ids(plan_id).await;

    let partial = ToRequisitionReq {
        items: vec![ConvertLineReq {
            plan_item_id: Some(items[0].0.to_string()),
            qty: Some(dec("4")),
            est_price: None,
        }],
        ..ToRequisitionReq::default()
    };
    let out = convert::to_requisition(plan_id, &partial, Some(operator().await))
        .await
        .expect("部分转换");
    assert_eq!(out.converted_qty, dec("4"));
    assert_eq!(out.requisitions[0].lines[0].remaining_after, dec("6"), "剩余 6");
    assert_eq!(plan_item_ids(plan_id).await[0].2, dec("4"), "已执行量 4");

    // 再转剩余 6
    let out = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect("转剩余");
    assert_eq!(out.converted_qty, dec("6"), "第二次只转剩余的 6");

    // 第三次：全部转完 → 409
    let error = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect_err("全转完必须拒");
    assert_eq!(error.code, cloudpivot_kernel::shared::error_code::RESOURCE_CONFLICT);
    assert!(error.msg.contains("剩余可转量都是 0"), "{}", error.msg);
    assert_eq!(
        plan_item_ids(plan_id).await[0].2,
        dec("10"),
        "已执行量仍是 10（没被多写）"
    );

    // 申请单只有两张（第三次没造空单）
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS n FROM biz_purchase_requisition WHERE source_plan_id = ?::uuid",
            vec![Value::from(&plan_id)],
        )
        .await
        .expect("数申请单");
    assert_eq!(
        rows.first().expect("count").get_typed::<i64>("n").expect("n"),
        2,
        "两次成功转换 = 两张申请单"
    );

    cleanup().await;
}

/// ⑥ 超出剩余量 → 400，报文点出剩余量与计划行号。
#[tokio::test]
async fn over_remaining_is_rejected_with_numbers() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "超量",
        2045,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "5", "1")],
    )
    .await;
    let items = plan_item_ids(plan_id).await;

    let req = ToRequisitionReq {
        items: vec![ConvertLineReq {
            plan_item_id: Some(items[0].0.to_string()),
            qty: Some(dec("6")),
            est_price: None,
        }],
        ..ToRequisitionReq::default()
    };
    let error = convert::to_requisition(plan_id, &req, Some(operator().await))
        .await
        .expect_err("超出剩余量必须 400");
    assert!(error.msg.contains("超过剩余可转量 5"), "要有剩余量：{}", error.msg);
    assert!(error.msg.contains("计划第 1 行"), "要有行号：{}", error.msg);
    assert_eq!(
        plan_item_ids(plan_id).await[0].2,
        Decimal::ZERO,
        "失败的转换不能写已执行量"
    );

    cleanup().await;
}

/// ⑦ 计划行没单价 → 400；请求覆盖单价（`items[].est_price`）后放行。
#[tokio::test]
async fn zero_price_line_needs_est_price_override() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let mut req = plan_req(
        "零单价",
        2046,
        Some(fx.dept),
        vec![PlanItemReq {
            material_id: Some(fx.material_a.to_string()),
            warehouse_id: Some(fx.warehouse_a.to_string()),
            planned_qty: Some(dec("2")),
            // 计划允许「单价待定」= 0（V0446 ck_biz_ppi_price 只兜 ≥ 0）
            planned_price: None,
            remark: None,
        }],
    );
    req.plan_name = Some(format!("{NAME_PREFIX}零单价"));
    let plan_id = plan_handler::create_plan(&req, Some(operator().await))
        .await
        .expect("建单（0 单价合法）");
    plan_handler::submit_plan(
        plan_id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交");
    let (_, instance) = status_and_instance(plan_id).await;
    approve_until_final(instance.expect("实例")).await;

    let error = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect_err("0 单价必须拒");
    assert!(error.msg.contains("没有预估单价"), "报文要说清原因：{}", error.msg);
    assert!(error.msg.contains("est_price"), "报文要给解法：{}", error.msg);

    let items = plan_item_ids(plan_id).await;
    let override_req = ToRequisitionReq {
        items: vec![ConvertLineReq {
            plan_item_id: Some(items[0].0.to_string()),
            qty: None,
            est_price: Some(dec("7.5")),
        }],
        ..ToRequisitionReq::default()
    };
    let out = convert::to_requisition(plan_id, &override_req, Some(operator().await))
        .await
        .expect("覆盖单价后应成功");
    assert_eq!(out.requisitions[0].lines[0].est_price, dec("7.5"));
    assert_eq!(out.requisitions[0].est_amount, dec("15"), "2 × 7.5");

    cleanup().await;
}

/// ⑧ 公司级计划（没有编制部门）必须给 `dept_id`：不给 400，给了能转。
#[tokio::test]
async fn company_level_plan_needs_dept() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "公司级",
        2047,
        None,
        vec![item(fx.material_a, fx.warehouse_a, "1", "2")],
    )
    .await;

    let error = convert::to_requisition(plan_id, &ToRequisitionReq::default(), Some(operator().await))
        .await
        .expect_err("没部门必须 400");
    assert!(error.msg.contains("dept_id"), "报文要点出缺哪个字段：{}", error.msg);

    let req = ToRequisitionReq {
        dept_id: Some(fx.dept.to_string()),
        ..ToRequisitionReq::default()
    };
    let out = convert::to_requisition(plan_id, &req, Some(operator().await))
        .await
        .expect("给了部门应成功");
    assert_eq!(out.requisitions[0].dept_id, fx.dept.to_string());
    assert!(out.requisitions[0].dept_name.is_some(), "部门名要带出来（快照）");

    cleanup().await;
}

/// ⑨ 请求里的计划行必须属于这张计划 → 400。
#[tokio::test]
async fn foreign_plan_item_is_rejected() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "外来行",
        2048,
        Some(fx.dept),
        vec![item(fx.material_a, fx.warehouse_a, "1", "2")],
    )
    .await;
    let other_id = approved_plan(
        "另一张",
        2049,
        Some(fx.dept),
        vec![item(fx.material_b, fx.warehouse_b, "1", "2")],
    )
    .await;
    let other_items = plan_item_ids(other_id).await;

    let req = ToRequisitionReq {
        items: vec![ConvertLineReq {
            plan_item_id: Some(other_items[0].0.to_string()),
            qty: None,
            est_price: None,
        }],
        ..ToRequisitionReq::default()
    };
    let error = convert::to_requisition(plan_id, &req, Some(operator().await))
        .await
        .expect_err("别的计划的行不能转");
    assert!(error.msg.contains("不属于这张采购计划"), "{}", error.msg);
    assert_eq!(
        plan_item_ids(plan_id).await[0].2,
        Decimal::ZERO,
        "失败的转换不能写已执行量"
    );

    cleanup().await;
}

/// ⑩ 生成的单是真申请单：采购申请域读得到，仓库 / 部门 / 单位 / 税率 / 金额都对。
#[tokio::test]
async fn converted_requisition_is_a_real_requisition() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let plan_id = approved_plan(
        "真申请单",
        2050,
        Some(fx.dept),
        vec![
            item(fx.material_a, fx.warehouse_a, "3", "10"),
            item(fx.material_b, fx.warehouse_a, "2", "5"),
        ],
    )
    .await;

    let req = ToRequisitionReq {
        tax_rate: Some(dec("13")),
        remark: Some("M403 用例备注".to_string()),
        ..ToRequisitionReq::default()
    };
    let out = convert::to_requisition(plan_id, &req, Some(operator().await))
        .await
        .expect("转换");
    assert_eq!(out.requisitions.len(), 1, "两行同仓 → 一张单");

    let created = &out.requisitions[0];
    let detail = requisition_handler::detail_of(Uuid::parse_str(&created.requisition_id).expect("uuid"))
        .await
        .expect("采购申请域必须读得到这张单");
    assert_eq!(detail.requisition.req_no, created.req_no);
    assert_eq!(detail.requisition.status, "DRAFT", "转出来的申请单是草稿（要再走审批）");
    assert_eq!(detail.requisition.warehouse_id, fx.warehouse_a.to_string());
    assert_eq!(detail.requisition.dept_id, fx.dept.to_string());
    assert_eq!(detail.items.len(), 2);
    assert_eq!(detail.requisition.est_amount, dec("40"), "30 + 10");
    for line in &detail.items {
        assert_eq!(line.tax_rate, dec("13"), "整单税率取请求值");
        assert!(!line.unit_name.is_empty(), "单位名要 join 出来");
        assert_eq!(line.demand_date, None, "没给需求日期就是空");
    }
    let remark = detail.requisition.remark.clone().unwrap_or_default();
    assert!(remark.contains("由采购计划"), "备注要写清来源：{remark}");
    assert!(remark.contains(&out.plan.plan_no), "备注要带计划单号：{remark}");
    assert!(remark.contains("M403 用例备注"), "用户备注要带上：{remark}");

    cleanup().await;
}

/// ⑪ 清场：`M403-` 夹具 0 残留（申请单 / 计划 / 明细 / 部门）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else {
        return;
    };
    cleanup().await;
    let rows = db()
        .find_with_params(
            "SELECT (SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE 'M403-%') AS plans, \
                    (SELECT count(*) FROM biz_purchase_requisition WHERE remark LIKE '%M403%') AS reqs",
            vec![],
        )
        .await
        .expect("清场点数");
    let row = rows.first().expect("count");
    assert_eq!(row.get_typed::<i64>("plans").expect("plans"), 0, "计划残留");
    assert_eq!(row.get_typed::<i64>("reqs").expect("reqs"), 0, "申请单残留");
}

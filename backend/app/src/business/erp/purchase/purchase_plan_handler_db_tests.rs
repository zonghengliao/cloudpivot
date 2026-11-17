// ============================================================================
// purchase_plan_handler_db_tests.rs —— 采购计划 CRUD + 审批（M4-02）的真库用例
//
// 卡：t_c6473745（M4-02【M4·采购】采购计划：CRUD + 审批）
//
// # 覆盖什么（每条都对应卡面 / V0446 / V0447 里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `create_plan_computes_totals_and_joins_names` | 建单：单号形状、状态 DRAFT、**三个 total_* 由服务端按明细算**、单位由物料带出、明细带物料/仓库/单位名且按行号 |
// | `duplicate_period_is_rejected` | 同「类型+年度+月+部门」只能一条存活计划（uk_biz_purchase_plan_period 的友好侧） |
// | `duplicate_material_warehouse_cell_is_rejected` | 明细粒度 = 物料 × 仓库；重复时报文点出第几行撞第几行 |
// | `month_must_match_type` | 年度不带月 / 月度必须 1–12（ck_biz_pp_period 的接口侧） |
// | `parent_plan_must_be_annual_of_same_year` | 月度挂年度、同年度、父单本身不是月度 |
// | `update_replaces_items_and_recalcs` | 改单 = 整单替换；明细换掉后合计跟着变 |
// | `only_draft_can_be_updated_or_deleted` | 提交后改 / 删都是 409（不是 500、不是静默成功） |
// | `delete_soft_deletes_and_detail_404` | 删草稿 = 软删；详情随即 404 |
// | `submit_goes_through_flow_and_approve_lands_approved` | **走 flow**：提交发起 pp_approve 实例，真引擎审批一票 → 回调落 APPROVED |
// | `large_plan_needs_finance_second_sign` | 条件边：计划金额 ≥ 50 万时第一票之后还有财务经理那一票 |
// | `reject_returns_to_draft_with_comment` | 驳回 → 打回 DRAFT，意见写进备注 |
// | `monthly_plan_submits_and_approves_too` | 月度计划同样能走完（plan_month 非空不影响引擎） |
// | `page_filters_by_status_type_year_and_keyword` | 分页筛选：状态 / 类型 / 年度 / 关键字 |
// | `zz_teardown_purges_fixtures` | 清场：`M402-` 夹具 0 残留（含流程实例与待办） |
//
// # ⚠ 真库用例必须串行
//
// 共用全局 `db()` 连接池与 `aging_db_tests::DB_LOCK`，跑法：
//
// ```bash
// ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m402_drill" \
//   cargo test --lib business::erp::purchase::purchase_plan_handler_db_tests -- --test-threads=1
// ```
//
// 流程种子（`pp_approve`）由 V0447 种下：**没有它时提交会被 409 拦下**（默认拒绝 + 显式跳过），
// 这本身是正确行为，本文件的用例要求「有流程」的那几条会红 —— 说明演练库没跑全迁移。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::purchase_plan_bridge;
use crate::business::erp::purchase::purchase_plan_dto::{
    CreatePlanReq, PlanItemReq, PlanPageQuery, STATUS_APPROVED, STATUS_DRAFT, STATUS_SUBMITTED, SubmitPlanReq,
};
use crate::business::erp::purchase::purchase_plan_handler as handler;
use crate::business::erp::purchase::purchase_plan_model as model;
use crate::business::flow::engine;
use crate::test_seed;

/// 夹具名前缀（清场按它反查，绝不按生产号段删）。
const NAME_PREFIX: &str = "M402-";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 操作人（真实存在的 `sys_user` 行）：`biz_flow_instance.initiator_id` 有外键，
/// 假 uuid 会让「发起审批」直接 500，所以从模板库里挑一个启用用户。
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

/// 取串行锁 + 探库 + 清场。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场：先删本卡造的计划（明细靠 `ON DELETE CASCADE`），再删它们引用的审批实例与待办。
async fn cleanup() {
    exec(
        "DELETE FROM biz_flow_task WHERE instance_id IN ( \
             SELECT i.id FROM biz_flow_instance i \
              JOIN biz_purchase_plan p ON p.id = i.biz_id \
             WHERE i.biz_type = 'PP_APPROVE' AND p.plan_name LIKE 'M402-%')",
        vec![],
    )
    .await;
    exec(
        "DELETE FROM biz_flow_instance i USING biz_purchase_plan p \
          WHERE i.biz_type = 'PP_APPROVE' AND i.biz_id = p.id AND p.plan_name LIKE 'M402-%'",
        vec![],
    )
    .await;
    exec("DELETE FROM biz_purchase_plan WHERE plan_name LIKE 'M402-%'", vec![]).await;
    // 夹具主数据也清掉：`test_seed` 的 `ensure_material` 是「缺才补」，上一轮留下的行
    // 可能带着**非基本单位**（本卡要求按基本单位录入），不清就会一直撞 `unit_guard`。
    exec("DELETE FROM biz_material WHERE material_code LIKE 'M402-MAT-%'", vec![]).await;
}

/// 主数据：两个物料 × 两个仓库（都走 `test_seed`，禁止手写主数据 INSERT）。
struct Fixtures {
    material_a: Uuid,
    material_b: Uuid,
    warehouse_a: Uuid,
    warehouse_b: Uuid,
}

async fn fixtures() -> Fixtures {
    Fixtures {
        material_a: test_seed::ensure_material_base_unit("M402-MAT-A", "M402 物料 A").await,
        material_b: test_seed::ensure_material_base_unit("M402-MAT-B", "M402 物料 B").await,
        warehouse_a: test_seed::ensure_warehouse("M402-WH-A", "M402 仓库 A").await,
        warehouse_b: test_seed::ensure_warehouse("M402-WH-B", "M402 仓库 B").await,
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

/// 一张年度计划的请求（`name` 决定夹具身份）。
fn annual_req(name: &str, year: i32, items: Vec<PlanItemReq>) -> CreatePlanReq {
    CreatePlanReq {
        plan_name: Some(format!("{NAME_PREFIX}{name}")),
        plan_type: Some("ANNUAL".to_string()),
        plan_year: Some(year),
        plan_month: None,
        parent_plan_id: None,
        dept_id: None,
        planner_id: None,
        remark: Some(format!("{NAME_PREFIX}用例")),
        items,
    }
}

/// 一张月度计划的请求。
fn monthly_req(name: &str, year: i32, month: i32, items: Vec<PlanItemReq>) -> CreatePlanReq {
    CreatePlanReq {
        plan_type: Some("MONTHLY".to_string()),
        plan_month: Some(month),
        ..annual_req(name, year, items)
    }
}

async fn status_of(plan_id: Uuid) -> (String, Option<Uuid>, Option<String>) {
    let rows = db()
        .find_with_params(
            "SELECT status, approval_instance_id, remark FROM biz_purchase_plan WHERE id = ?::uuid",
            vec![Value::from(&plan_id)],
        )
        .await
        .expect("查采购计划");
    let row = rows.first().expect("计划存在");
    (
        row.get_typed("status").expect("status"),
        row.get_typed("approval_instance_id").expect("instance"),
        row.get_typed("remark").expect("remark"),
    )
}

/// 实例上的待办（`task_id / node_key / assignee_id`），按创建顺序。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, String, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id AS task_id, node_key, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' \
              ORDER BY created_at",
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

/// 审批同意一票（与 `flow::handler::approve_task` 同一条引擎路径 + 同款「提交后再回调」）。
async fn approve_task(task_id: Uuid, operator: Uuid) -> String {
    let out = db()
        .transaction(|tx| async move { engine::approve(&tx, task_id, operator, Some("M402 用例同意")).await })
        .await
        .unwrap_or_else(|error| panic!("审批同意失败：{error}"));
    purchase_plan_bridge::on_pp_settled(out.instance_id, &out.instance_status).await;
    out.instance_status
}

/// 走完所有审批（逐节点真派真批），返回实例最终状态。
async fn approve_until_final(instance_id: Uuid) -> String {
    let mut last = String::from("PENDING");
    for _ in 0..5 {
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        for (task_id, node_key, assignee) in tasks {
            tracing::info!(%node_key, "M402 用例：审批一票");
            last = approve_task(task_id, assignee).await;
        }
        if last == "APPROVED" || last == "REJECTED" {
            break;
        }
    }
    last
}

// ==================== 用例 ====================

/// ① 建单：单号 / 状态 / 合计由服务端算 / 单位由物料带出 / 明细带名称并按行号。
#[tokio::test]
async fn create_plan_computes_totals_and_joins_names() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let req = annual_req(
        "建单",
        2026,
        vec![
            item(fx.material_a, fx.warehouse_a, "3", "2.5"),
            item(fx.material_a, fx.warehouse_b, "1", "10"),
            item(fx.material_b, fx.warehouse_a, "2", "0.3333"),
        ],
    );
    let id = handler::create_plan(&req, Some(operator().await))
        .await
        .expect("建单应成功");

    let detail = handler::detail_of(id).await.expect("详情应成功");
    // 单号形状 `PPL-yyyyMMdd-000001`：4 + 8 + 1 + 6 = 19 个字符，且日期段必须是 8 位数字
    let no = &detail.plan.plan_no;
    assert_eq!(no.len(), 19, "单号长度必须是 PPL-yyyyMMdd-NNNNNN，实际 {no}");
    assert!(no.starts_with("PPL-"), "单号必须以 PPL- 开头，实际 {no}");
    assert!(
        no[4..12].chars().all(|c| c.is_ascii_digit()) && no[13..].chars().all(|c| c.is_ascii_digit()),
        "单号日期段与流水段必须是数字，实际 {no}"
    );
    assert_eq!(detail.plan.status, STATUS_DRAFT);
    assert!(detail.plan.editable, "草稿应 editable");
    // 计划量 = 3 + 1 + 2 = 6；金额 = 7.5 + 10 + 0.6666 = 18.1666（4 位舍入）
    assert_eq!(detail.plan.total_planned_qty, dec("6"), "单头合计必须按明细重算");
    assert_eq!(detail.plan.total_planned_amount, dec("18.1666"));
    assert_eq!(detail.plan.total_executed_qty, Decimal::ZERO);
    assert_eq!(detail.plan.item_count, 3);

    assert_eq!(detail.items.len(), 3);
    let line_nos: Vec<i32> = detail.items.iter().map(|line| line.line_no).collect();
    assert_eq!(line_nos, vec![1, 2, 3], "明细按行号升序");
    for line in &detail.items {
        assert!(!line.material_code.is_empty(), "物料编码必须 join 出来");
        assert!(!line.material_name.is_empty(), "物料名必须 join 出来");
        assert!(!line.warehouse_name.is_empty(), "仓库名必须 join 出来");
        assert!(!line.unit_name.is_empty(), "单位名必须由物料带出");
        assert_eq!(line.executed_qty, Decimal::ZERO);
    }
    // 单位是物料的基础单位（客户端不许传）
    assert_eq!(detail.items[0].unit_id, detail.items[1].unit_id, "同一物料单位一致");
    assert_eq!(detail.items[0].planned_amount, dec("7.5000"));

    // 客户端传的 total_* 一律被忽略：请求里根本没有这两个字段（DTO 不接收）
    let row = model::find_plan(db(), id).await.expect("查单").expect("存在");
    assert_eq!(row.status, STATUS_DRAFT);
    assert_eq!(row.planner_id, Some(operator().await), "编制人缺省 = 当前登录人");

    cleanup().await;
}

/// ② 同「类型 + 年度 + 月 + 编制部门」只能一条存活计划（第二次 409，报文带对方单号）。
#[tokio::test]
async fn duplicate_period_is_rejected() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let req = annual_req("重复期", 2026, vec![item(fx.material_a, fx.warehouse_a, "1", "1")]);
    let first = handler::create_plan(&req, Some(operator().await))
        .await
        .expect("第一张应成功");

    let error = handler::create_plan(&req, Some(operator().await))
        .await
        .expect_err("同期间第二张必须 409");
    assert_eq!(error.status, 409, "重复期间是业务冲突，不是 500");
    assert!(error.msg.contains("PPL-"), "报文要点出占用方的单号：{}", error.msg);

    // 换月份（月度）不算重复
    let monthly = monthly_req("重复期", 2026, 11, vec![item(fx.material_a, fx.warehouse_a, "1", "1")]);
    handler::create_plan(&monthly, Some(operator().await))
        .await
        .expect("月度计划与年度计划不冲突");

    // 同期间改单（不含自己）不算重复
    let mut update = req.clone();
    update.plan_name = Some(format!("{NAME_PREFIX}重复期-改名"));
    handler::update_plan(first, &update, Some(operator().await))
        .await
        .expect("改自己是允许的（探针要排除自己）");

    cleanup().await;
}

/// ③ 明细粒度 = 物料 × 仓库：同一张单里重复在请求层就拒，报文点出两行行号。
#[tokio::test]
async fn duplicate_material_warehouse_cell_is_rejected() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let req = annual_req(
        "重复明细",
        2026,
        vec![
            item(fx.material_a, fx.warehouse_a, "1", "1"),
            item(fx.material_b, fx.warehouse_b, "2", "2"),
            item(fx.material_a, fx.warehouse_a, "3", "3"),
        ],
    );
    let error = handler::create_plan(&req, Some(operator().await))
        .await
        .expect_err("同物料同仓重复必须拒");
    assert_eq!(error.status, 400);
    assert!(error.msg.contains("第 3 行"), "报文要点出重复行：{}", error.msg);
    assert!(error.msg.contains("第 1 行"), "报文要点出首行：{}", error.msg);

    // 同物料换仓 = 合法两行
    let ok = annual_req(
        "重复明细",
        2026,
        vec![
            item(fx.material_a, fx.warehouse_a, "1", "1"),
            item(fx.material_a, fx.warehouse_b, "2", "2"),
        ],
    );
    handler::create_plan(&ok, Some(operator().await))
        .await
        .expect("同物料换仓是两行，必须允许");

    cleanup().await;
}

/// ④ 类型 × 月份自洽（年度不带月 / 月度必须 1–12）。
#[tokio::test]
async fn month_must_match_type() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let lines = vec![item(fx.material_a, fx.warehouse_a, "1", "1")];

    let annual_with_month = CreatePlanReq {
        plan_month: Some(11),
        ..annual_req("月错配-年度带月", 2026, lines.clone())
    };
    let error = handler::create_plan(&annual_with_month, Some(operator().await))
        .await
        .expect_err("年度计划带月必须 400");
    assert_eq!(error.status, 400);

    let monthly_without_month = CreatePlanReq {
        plan_type: Some("MONTHLY".to_string()),
        plan_month: None,
        ..annual_req("月错配-月度无月", 2026, lines.clone())
    };
    let error = handler::create_plan(&monthly_without_month, Some(operator().await))
        .await
        .expect_err("月度计划没月份必须 400");
    assert_eq!(error.status, 400);

    let monthly_bad_month = monthly_req("月错配-13月", 2026, 13, lines.clone());
    let error = handler::create_plan(&monthly_bad_month, Some(operator().await))
        .await
        .expect_err("13 月必须 400");
    assert_eq!(error.status, 400);

    // 合法：月度带月
    handler::create_plan(&monthly_req("月错配-合法", 2026, 12, lines), Some(operator().await))
        .await
        .expect("月度带月合法");

    cleanup().await;
}

/// ⑤ 月度计划的父单必须是同年度年度计划（跨行语义，库层只拦自指）。
#[tokio::test]
async fn parent_plan_must_be_annual_of_same_year() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let lines = vec![item(fx.material_a, fx.warehouse_a, "1", "1")];
    let annual = handler::create_plan(&annual_req("父单", 2026, lines.clone()), Some(operator().await))
        .await
        .expect("年度计划");

    // 合法：月度挂同年度年度计划
    let good = CreatePlanReq {
        parent_plan_id: Some(annual.to_string()),
        ..monthly_req("父单-合法", 2026, 11, lines.clone())
    };
    let id = handler::create_plan(&good, Some(operator().await))
        .await
        .expect("月度挂年度合法");
    let detail = handler::detail_of(id).await.expect("详情");
    assert_eq!(detail.plan.parent_plan_id.as_deref(), Some(annual.to_string().as_str()));
    assert_eq!(
        detail.plan.parent_plan_no.as_deref(),
        Some(detail.plan.parent_plan_no.clone().unwrap_or_default().as_str()),
        "父单号必须 join 出来"
    );

    // 非法一：年度计划挂父单
    let annual_parent = CreatePlanReq {
        parent_plan_id: Some(annual.to_string()),
        ..annual_req("父单-年度挂年度", 2026, lines.clone())
    };
    assert_eq!(
        handler::create_plan(&annual_parent, Some(operator().await))
            .await
            .expect_err("只有月度能挂父单")
            .status,
        400
    );

    // 非法二：父亲是月度计划
    let monthly_parent = CreatePlanReq {
        parent_plan_id: Some(id.to_string()),
        ..monthly_req("父单-月度挂月度", 2026, 12, lines.clone())
    };
    assert_eq!(
        handler::create_plan(&monthly_parent, Some(operator().await))
            .await
            .expect_err("父亲必须年度")
            .status,
        400
    );

    // 非法三：年度不一致
    let mismatched = CreatePlanReq {
        parent_plan_id: Some(annual.to_string()),
        ..monthly_req("父单-年度不符", 2025, 11, lines)
    };
    assert_eq!(
        handler::create_plan(&mismatched, Some(operator().await))
            .await
            .expect_err("年度不符必须拒")
            .status,
        400
    );

    cleanup().await;
}

/// ⑥ 改单 = 整单替换（明细换掉后合计跟着变）。
#[tokio::test]
async fn update_replaces_items_and_recalcs() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let id = handler::create_plan(
        &annual_req("改单", 2026, vec![item(fx.material_a, fx.warehouse_a, "1", "1")]),
        Some(operator().await),
    )
    .await
    .expect("建单");

    let mut update = annual_req(
        "改单",
        2026,
        vec![
            item(fx.material_b, fx.warehouse_b, "4", "2.5"),
            item(fx.material_a, fx.warehouse_a, "2", "1"),
        ],
    );
    update.plan_name = Some(format!("{NAME_PREFIX}改单-已改"));
    handler::update_plan(id, &update, Some(operator().await))
        .await
        .expect("改草稿应成功");

    let detail = handler::detail_of(id).await.expect("详情");
    assert_eq!(detail.plan.plan_name, format!("{NAME_PREFIX}改单-已改"));
    assert_eq!(detail.items.len(), 2, "明细是整单替换，旧行不残留");
    assert_eq!(detail.plan.total_planned_qty, dec("6"), "4 + 2");
    assert_eq!(detail.plan.total_planned_amount, dec("12"), "10 + 2");
    assert_eq!(detail.items[0].line_no, 1);
    assert_eq!(detail.items[1].line_no, 2);

    cleanup().await;
}

/// ⑦ 只有草稿能改 / 删（提交后 409，报文说清当前状态）。
#[tokio::test]
async fn only_draft_can_be_updated_or_deleted() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let id = handler::create_plan(
        &annual_req("非草稿", 2026, vec![item(fx.material_a, fx.warehouse_a, "2", "1")]),
        Some(operator().await),
    )
    .await
    .expect("建单");
    handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交审批");

    let req = annual_req("非草稿", 2026, vec![item(fx.material_a, fx.warehouse_a, "3", "1")]);
    let error = handler::update_plan(id, &req, Some(operator().await))
        .await
        .expect_err("审批中不许改");
    assert_eq!(error.status, 409);
    assert!(
        error.msg.contains("SUBMITTED") || error.msg.contains("审批中"),
        "报文要说当前状态：{}",
        error.msg
    );

    let error = handler::delete_plan(id, Some(operator().await))
        .await
        .expect_err("审批中不许删");
    assert_eq!(error.status, 409);

    // 重复提交：409 且报文带审批单号
    let error = handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect_err("重复提交必须 409");
    assert_eq!(error.status, 409);
    assert!(error.msg.contains("已在审批中"), "报文要说清已在审批中：{}", error.msg);

    // 清掉在跑实例再清场（否则实例引用计划 id 会留在库里）
    cleanup().await;
}

/// ⑧ 删草稿 = 软删；详情随即 404。
#[tokio::test]
async fn delete_soft_deletes_and_detail_404() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let id = handler::create_plan(
        &annual_req("删单", 2026, vec![item(fx.material_a, fx.warehouse_a, "1", "1")]),
        Some(operator().await),
    )
    .await
    .expect("建单");

    handler::delete_plan(id, Some(operator().await))
        .await
        .expect("删草稿应成功");
    assert!(
        model::find_plan(db(), id).await.expect("查单").is_none(),
        "软删后 find_plan 必须查不到"
    );
    assert_eq!(handler::detail_of(id).await.expect_err("详情应 404").status, 404);
    // 明细跟着不可见（通过单头取，单头没了自然没有明细入口）
    assert!(
        !model::items_of_plan(db(), id).await.expect("查明细").is_empty(),
        "明细行仍在（软删只动单头，审计可回溯）"
    );
    // 删第二次 → 404
    assert_eq!(
        handler::delete_plan(id, Some(operator().await))
            .await
            .expect_err("已删")
            .status,
        404
    );

    // 软删后同期间可以重建（uk_biz_purchase_plan_period 是部分唯一索引）
    handler::create_plan(
        &annual_req("删单-重建", 2026, vec![item(fx.material_a, fx.warehouse_a, "1", "1")]),
        Some(operator().await),
    )
    .await
    .expect("软删后同期间重建应成功");

    cleanup().await;
}

/// ⑨ **走 flow**：提交发起 pp_approve 实例 → 真引擎审批一票 → 回调落 APPROVED。
#[tokio::test]
async fn submit_goes_through_flow_and_approve_lands_approved() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let id = handler::create_plan(
        &annual_req("走流程", 2026, vec![item(fx.material_a, fx.warehouse_a, "10", "2")]),
        Some(operator().await),
    )
    .await
    .expect("建单");

    handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交审批应成功");

    let (status, instance, _) = status_of(id).await;
    assert_eq!(status, STATUS_SUBMITTED, "提交后必须 SUBMITTED");
    let instance_id = instance.expect("SUBMITTED 必须带审批实例（ck_biz_pp_instance）");

    // 实例必须真的是 PP_APPROVE 且挂在计划上
    let rows = db()
        .find_with_params(
            "SELECT biz_type, biz_id, status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查实例");
    let row = rows.first().expect("实例存在");
    assert_eq!(row.get_typed::<String>("biz_type").expect("biz_type"), "PP_APPROVE");
    assert_eq!(row.get_typed::<Uuid>("biz_id").expect("biz_id"), id);

    // 第一票必须是采购经理节点
    let tasks = pending_tasks(instance_id).await;
    assert!(!tasks.is_empty(), "发起后应有待办");
    assert_eq!(tasks[0].1, "n_pm", "第一站必须是采购经理审批");

    let final_status = approve_until_final(instance_id).await;
    assert_eq!(final_status, "APPROVED", "小额计划一票即通过");

    let (status, _, remark) = status_of(id).await;
    assert_eq!(status, STATUS_APPROVED, "审批通过必须回调成 APPROVED");
    assert!(
        remark.unwrap_or_default().contains("采购计划审批通过"),
        "回调要把结论写进备注"
    );

    cleanup().await;
}

/// ⑩ 条件边：计划金额 ≥ 50 万 → 采购经理批完还有财务经理那一票。
#[tokio::test]
async fn large_plan_needs_finance_second_sign() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let id = handler::create_plan(
        &annual_req("大额", 2026, vec![item(fx.material_a, fx.warehouse_a, "1000", "600")]),
        Some(operator().await),
    )
    .await
    .expect("建单");
    let detail = handler::detail_of(id).await.expect("详情");
    assert_eq!(detail.plan.total_planned_amount, dec("600000.0000"), "60 万 > 50 万档");

    handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交审批");
    let (_, instance, _) = status_of(id).await;
    let instance_id = instance.expect("实例");

    let tasks = pending_tasks(instance_id).await;
    assert_eq!(tasks[0].1, "n_pm");
    let after_first = approve_task(tasks[0].0, tasks[0].2).await;
    assert_ne!(after_first, "APPROVED", "60 万不能在采购经理那一票就结束");

    let tasks = pending_tasks(instance_id).await;
    assert!(!tasks.is_empty(), "第二站应有待办");
    assert!(
        tasks.iter().all(|(_, node_key, _)| node_key == "n_fin"),
        "第二站必须是财务经理复核（会签模式 ANY 会按候选人生成多条待办）：{tasks:?}"
    );
    let after_second = approve_task(tasks[0].0, tasks[0].2).await;
    assert_eq!(after_second, "APPROVED", "两票后实例通过（ANY 会签一票即过）");
    assert_eq!(status_of(id).await.0, STATUS_APPROVED);

    cleanup().await;
}

/// ⑪ 驳回 → 打回 DRAFT，意见写进备注。
#[tokio::test]
async fn reject_returns_to_draft_with_comment() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let id = handler::create_plan(
        &annual_req("驳回", 2026, vec![item(fx.material_a, fx.warehouse_a, "5", "1")]),
        Some(operator().await),
    )
    .await
    .expect("建单");
    handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交审批");
    let (_, instance, _) = status_of(id).await;
    let instance_id = instance.expect("实例");
    let tasks = pending_tasks(instance_id).await;
    let (task_id, _, assignee) = tasks[0];

    let out = db()
        .transaction(|tx| async move {
            engine::reject(
                &tx,
                task_id,
                assignee,
                "M402 用例驳回：计划量偏高",
                engine::ReturnTo::Final,
            )
            .await
        })
        .await
        .expect("驳回");
    purchase_plan_bridge::on_pp_settled(out.instance_id, &out.instance_status).await;

    let (status, instance, remark) = status_of(id).await;
    assert_eq!(status, STATUS_DRAFT, "驳回必须打回草稿（不是关闭）");
    assert!(instance.is_none(), "打回后必须清掉审批实例（下次提交重新发起）");
    assert!(
        remark.unwrap_or_default().contains("计划量偏高"),
        "驳回意见必须写进备注（人要看到被打回的理由）"
    );

    // 打回后可以改、可以再提交
    handler::update_plan(
        id,
        &annual_req("驳回", 2026, vec![item(fx.material_a, fx.warehouse_a, "3", "1")]),
        Some(operator().await),
    )
    .await
    .expect("打回后能改");
    handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("打回后能再提交");

    cleanup().await;
}

/// ⑫ 月度计划同样能走完流程（plan_month 非空不影响引擎与条件边）。
#[tokio::test]
async fn monthly_plan_submits_and_approves_too() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let id = handler::create_plan(
        &monthly_req("月度", 2026, 11, vec![item(fx.material_b, fx.warehouse_b, "7", "1.5")]),
        Some(operator().await),
    )
    .await
    .expect("建单");
    handler::submit_plan(
        id,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("月度计划提交审批");
    let (_, instance, _) = status_of(id).await;
    let final_status = approve_until_final(instance.expect("实例")).await;
    assert_eq!(final_status, "APPROVED");
    assert_eq!(status_of(id).await.0, STATUS_APPROVED);

    cleanup().await;
}

/// ⑬ 分页筛选：状态 / 类型 / 年度 / 关键字。
#[tokio::test]
async fn page_filters_by_status_type_year_and_keyword() {
    let Some(_guard) = setup().await else {
        return;
    };
    let fx = fixtures().await;
    let annual = handler::create_plan(
        &annual_req("分页-年度", 2031, vec![item(fx.material_a, fx.warehouse_a, "1", "1")]),
        Some(operator().await),
    )
    .await
    .expect("年度计划");
    let _monthly = handler::create_plan(
        &monthly_req(
            "分页-月度",
            2031,
            3,
            vec![item(fx.material_a, fx.warehouse_a, "2", "2")],
        ),
        Some(operator().await),
    )
    .await
    .expect("月度计划");
    handler::submit_plan(
        annual,
        &SubmitPlanReq::default(),
        Some(operator().await),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交");

    let query = PlanPageQuery {
        page: Some(1),
        page_size: Some(50),
        keyword: Some("M402-分页".to_string()),
        plan_year: Some(2031),
        ..PlanPageQuery::default()
    };
    let filter = query.filter().expect("筛选参数合法");
    let (rows, total) = model::page_plans(&filter).await.expect("分页");
    assert_eq!(total, 2, "关键字 + 年度应命中两张");
    assert_eq!(rows.len(), 2);

    // 状态筛选：只有一张提交了
    let filter = PlanPageQuery {
        status: Some(STATUS_SUBMITTED.to_string()),
        plan_year: Some(2031),
        keyword: Some("M402-分页".to_string()),
        ..PlanPageQuery::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, total) = model::page_plans(&filter).await.expect("分页");
    assert_eq!(total, 1);
    assert_eq!(
        rows[0].plan_no,
        model::find_plan(db(), annual).await.expect("查").expect("存在").plan_no
    );

    // 类型筛选：MONTHLY 只有一张
    let filter = PlanPageQuery {
        plan_type: Some("MONTHLY".to_string()),
        plan_year: Some(2031),
        keyword: Some("M402-分页".to_string()),
        ..PlanPageQuery::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (_, total) = model::page_plans(&filter).await.expect("分页");
    assert_eq!(total, 1);

    cleanup().await;
}

/// ⑭ 清场：夹具 0 残留（计划 / 流程实例 / 待办）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else {
        return;
    };
    cleanup().await;
    let plans = db()
        .find_with_params(
            "SELECT count(*) AS n FROM biz_purchase_plan WHERE plan_name LIKE 'M402-%'",
            vec![],
        )
        .await
        .expect("数夹具");
    assert_eq!(plans[0].get_typed::<i64>("n").expect("n"), 0, "计划夹具必须 0 残留");
    let instances = db()
        .find_with_params(
            "SELECT count(*) AS n FROM biz_flow_instance i \
              WHERE i.biz_type = 'PP_APPROVE' \
                AND NOT EXISTS (SELECT 1 FROM biz_purchase_plan p WHERE p.id = i.biz_id \
                                 AND p.plan_name NOT LIKE 'M402-%')",
            vec![],
        )
        .await
        .expect("数实例");
    assert_eq!(
        instances[0].get_typed::<i64>("n").expect("n"),
        0,
        "本卡造的 PP_APPROVE 实例必须 0 残留"
    );
    let _ = Utc::now();
}

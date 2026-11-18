// ============================================================================
// purchase_chain_e2e_tests.rs —— 采购缺口 E2E：计划 → 申请 → 订单 → 入库（M4-20）
//
// 卡：t_6a4a60ba（M4-20【M4·采购】采购缺口 E2E：计划 → 申请 → 订单 → 入库）
//
// # 这一份钉的是什么（一句话）
//
// **一张年度采购计划怎么一路变成仓里的库存 + 应付账款**：四个单据节点一次跑完、
// 每一步都从**库里**取事实（不看回执自说自话）、每个「交接处」都有断言，
// 中间夹三处**顺序反证**（未批准的不能转、未审批的订单不能收、已收满不能再收）。
//
// # 为什么需要这份用例（前面几张卡各自钉死了自己那一段，但没有一条串起来）
//
// ```text
// purchase_plan_db_tests            ← 计划表结构 / 取号 / 合计（单段，单据是 SQL 夹具）
// purchase_plan_handler_db_tests    ← 计划 CRUD + 审批（单段，到 APPROVED 为止）
// purchase_plan_convert_db_tests    ← 计划 → 申请：一仓一单 / 行级溯源 / 原子性（申请单到 DRAFT/APPROVED）
// purchase_plan_execution_db_tests  ← 计划执行率：已申请 / 已下单 / 剩余（单段，收货从未发生过）
// stockin_handler_tests_*           ← 收货建单 / 过账 / 三本账 / 超收 / 作废（来源采购单是 SQL 夹具）
// 本文件（M4-20）                   ← **一张计划走到入库**：上游夹具全部走真实入口，
//                                     每段的「交接处」是本文件唯一关心的东西
// ```
//
// 于是「计划转出来的申请、合并成的订单，能不能真的被批、真的收、真的落账」这件事，
// 在库里从来没有被验证过 —— 这就是本卡要补的缺口。
//
// # 链路（10 步 · 断言点即卡面「全链」的落点）
//
// ```text
//  ① 建采购计划（DRAFT，PPL-yyyyMMdd-nnnnnn，2 行 / 同一仓）        金额台恒等自检
//  ② 提交审批 → pp_approve → 逐节点审批 → APPROVED
//     └ 反证：DRAFT 计划转申请 409（未批准的计划改了会变）
//  ③ 计划转采购申请（一仓一单 → 1 张 PRQ）                         明细带 plan_item_id 溯源
//     └ 反证：DRAFT 申请转采购单 409（审批是转单前置）
//  ④ 申请提交审批 → pr_approve → 逐节点审批 → APPROVED
//  ⑤ 申请合并转采购订单（2 行 → 1 张 PO）                           申请单转 CONVERTED + 回链 po_id/po_no
//     └ 反证：DRAFT 采购单收货 400（只有 SUBMITTED/APPROVED/PARTIAL_RECEIVED 能收）
//  ⑥ 采购单提交审批 → po_approve → 逐节点审批 → APPROVED            审批通过才进入「在途/待收」
//  ⑦ 建收货单（items 留空 = 从采购单带出未收完的行）→ DRAFT          此刻库存**一分未加**
//  ⑧ 过账（POSTED）→ 三本账 + 成本 + 流水 + 凭证 + 应付 + 采购单 RECEIVED
//  ⑨ 收口：重复过账 409 / 已收满再收 400 / 已转单的申请再转 409 /
//           计划可转量 0 / 无 PENDING 审批实例 / 状态日志可回溯
//  ⑩ 第二条用例：分批收货（40 → 60+10）—— PARTIAL_RECEIVED → RECEIVED，
//           两张收货单两套凭证/应付、库存合计仍等于订单量、计划执行率不受收货影响
// ```
//
// # 本卡实测到的三处口径（写断言前必须先看，别照直觉写）
//
// 1. **过账才动账**：建单（⑦）只落 `DRAFT` + 明细，库存/凭证/应付**一条都没有**；
//    断言「建单后库存为 0」是本卡防「草稿也入账」的硬闸。
// 2. **采购单「已收数量」是派生值，不回写**（P2-3）：`biz_purchase_order_item` 没有
//    `received_qty` 列，待收量由「已过账收货单聚合」现算 —— 所以断言要读
//    `purchase_handler::pending_items_of`（同一条聚合 SQL），别去找那一列。
// 3. **成本口径只取不含税金额**（PRD §3.2）：`biz_inventory.amount` = 1250（不是 1412.5），
//    进项税 162.5 走 `2221.01`；应付贷方 2202 = 价税合计 1412.5。三个数混着写必假红。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `M420-` 夹具、同一份 `biz_seq` 号段），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m420_e2e" cargo test -p cloudpivot --lib \
//   business::erp::purchase::purchase_chain_e2e_tests -- --test-threads=1
// ```
//
// 也可直接跑 `bash scripts/verify/m420_purchase_chain_e2e_drill.sh`
// （克隆演练库 → 补 V0405…最新 → 前置断言 → 跑本文件 + 采购域回归 → 夹具零残留）。
// ============================================================================

use nik::prelude::chrono::{Datelike, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db as shared_ensure_db};
use crate::business::erp::purchase::purchase_handler;
use crate::business::erp::purchase::purchase_plan_convert_dto::ToRequisitionReq;
use crate::business::erp::purchase::purchase_plan_convert_handler as convert;
use crate::business::erp::purchase::purchase_plan_dto::{CreatePlanReq, PlanItemReq, SubmitPlanReq};
use crate::business::erp::purchase::purchase_plan_execution_dto::{STATE_DONE, STATE_NOT_STARTED, STATE_PARTIAL};
use crate::business::erp::purchase::purchase_plan_execution_handler as exec_handler;
use crate::business::erp::purchase::purchase_plan_handler as plan_handler;
use crate::business::erp::purchase::purchase_requisition_dto::{SubmitRequisitionReq, ToPoReq};
use crate::business::erp::purchase::purchase_requisition_handler as requisition_handler;
use crate::business::erp::stockin::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};
use crate::business::erp::stockin::stockin_handler;
use crate::business::flow::{doc_reader, engine};
use crate::test_seed;

// ============================================================================
// 夹具命名空间（清场按它反查，绝不按生产号段删）
// ============================================================================

/// 夹具名前缀（计划名 / 单号兜底）。
const NAME_PREFIX: &str = "M420-";
/// 夹具仓库编码。
const WH_CODE: &str = "M420-WH";
/// 夹具物料编码（甲：单价 2.5 / 乙：单价 100）。
const MAT_A_CODE: &str = "M420-M1";
const MAT_B_CODE: &str = "M420-M2";
/// 夹具供应商编码。
const SUP_CODE: &str = "M420-SUP";
/// 采购制单人（提交审批的人）—— **不绑任何审批角色**，否则会撞制单/审批分离闸。
const MAKER: &str = "m420_maker";
/// 收货制单人（仓管）。
const KEEPER: &str = "m420_keeper";
/// 过账人 —— 与收货制单人分开，ERP-B2「制单 ≠ 过账」开关打开时也成立。
const POSTER: &str = "m420_poster";

// ============================================================================
// 库连接 / 读数小工具
// ============================================================================

fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>()
        .unwrap_or_else(|error| panic!("定点字面量非法（{text}）：{error}"))
}

/// 真库就绪判定：**设了 `ERP_DB_URL` 却连不上 ⇒ 直接 panic**。
///
/// 仓里通用的 `ensure_db()` 连不上只打印一行 `SKIP:` 然后返回 `false`，用例随即
/// `return` —— 在 cargo 眼里那是 **ok**。对真库用例这就是「假绿」：连接串写错 /
/// 库没起来 / 库名不存在全都静默变成「全绿」。这里堵死：**没配**才允许跳过。
async fn ensure_db_or_panic() -> bool {
    let configured = std::env::var("ERP_DB_URL").is_ok();
    let ready = shared_ensure_db().await;
    if configured && !ready {
        panic!("ERP_DB_URL 已设置但连不上库：拒绝静默 SKIP 假绿（连接串错 / 库没起来 / 库名不存在都要红）");
    }
    ready
}

async fn sql(statement: &str) {
    db().exec_sql(statement.to_string(), vec![])
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
}

async fn exec_params(statement: &str, params: Vec<Value>) {
    db().execute_with_params(statement, params)
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
}

async fn scalar(query: &str, params: Vec<Value>) -> Option<String> {
    match db().scalar_with_params::<String>(query, params).await {
        Ok(text) => Some(text),
        Err(DbError::NotFound) => None,
        Err(error) => panic!("标量查询失败：{error}\n{query}"),
    }
}

async fn count(query: &str, params: Vec<Value>) -> i64 {
    db().scalar_with_params::<i64>(query, params)
        .await
        .unwrap_or_else(|error| panic!("计数查询失败：{error}\n{query}"))
}

async fn dec_of(query: &str, params: Vec<Value>) -> Decimal {
    scalar(query, params)
        .await
        .and_then(|text| text.parse::<Decimal>().ok())
        .unwrap_or_else(|| panic!("数量/金额读取失败：{query}"))
}

// ============================================================================
// 夹具
// ============================================================================

/// 本文件用到的主数据与账号。
struct Fx {
    maker: Uuid,
    keeper: Uuid,
    poster: Uuid,
    dept: Uuid,
    supplier: Uuid,
    warehouse: Uuid,
    mat_a: Uuid,
    mat_b: Uuid,
}

async fn fixture() -> Fx {
    // 物料必须挂**基本单位**：`unit_guard` 只认 `is_base = 1`，而 `ensure_material`
    // 挑的是「第一个启用单位」（模板库里可能是「箱」）→ 采购/收货建单直接 66030。
    // `upsert_material` 是 `ON CONFLICT DO NOTHING`，早先失败留下的脏夹具要显式校正。
    let mat_a = test_seed::ensure_material_base_unit(MAT_A_CODE, "M4-20 全链夹具物料甲").await;
    let mat_b = test_seed::ensure_material_base_unit(MAT_B_CODE, "M4-20 全链夹具物料乙").await;
    let unit = test_seed::ensure_base_unit().await;
    for material in [mat_a, mat_b] {
        exec_params(
            "UPDATE biz_material SET unit_id = ?::uuid, deleted_at = NULL WHERE id = ?::uuid",
            vec![Value::from(&unit), Value::from(&material)],
        )
        .await;
    }
    Fx {
        maker: test_seed::ensure_user(MAKER, "M4-20 采购制单人", None).await,
        keeper: test_seed::ensure_user(KEEPER, "M4-20 收货制单人", None).await,
        poster: test_seed::ensure_user(POSTER, "M4-20 过账人", None).await,
        dept: cloudpivot_kernel::shared::test_helpers::ensure_any_dept().await,
        supplier: test_seed::ensure_supplier(SUP_CODE, "M4-20 全链夹具供应商").await,
        warehouse: test_seed::ensure_warehouse(WH_CODE, "M4-20 全链夹具仓").await,
        mat_a,
        mat_b,
    }
}

/// 清场：只清本模块**业务单据**及其全部痕迹（按夹具主数据反查，不扫生产号段）。
///
/// 顺序不可交换：
/// 1. `biz_status_log` 是弱引用（无外键）⇒ **先于**单据按 id 清（单据一删就查不到 id）；
/// 2. 凭证 / 应付是弱引用（`source_type` + `source_id`）⇒ 在删收货单**之前**清；
/// 3. 审批实例 `biz_id` 也是弱引用，且 `prev_instance_id` 自引用无 ON DELETE ⇒ 先解开再删；
/// 4. 外键链：`biz_purchase_order_item` / `biz_purchase_requisition_item` /
///    `biz_stock_in_item` / `biz_voucher_entry` 都是 CASCADE，删单头即可；
///    `biz_inventory`（FK 指向库位账）→ `biz_inventory_location` 要**倒序**删。
async fn cleanup() {
    let plans = format!("SELECT id FROM biz_purchase_plan WHERE plan_name LIKE '{NAME_PREFIX}%'");
    let reqs = format!(
        "SELECT id FROM biz_purchase_requisition WHERE source_plan_id IN ({plans}) \
            OR req_no LIKE '{NAME_PREFIX}%'"
    );
    let pos = format!(
        "SELECT id FROM biz_purchase_order WHERE id IN \
            (SELECT po_id FROM biz_purchase_requisition WHERE source_plan_id IN ({plans}) AND po_id IS NOT NULL) \
            OR order_no LIKE '{NAME_PREFIX}%'"
    );
    let ins = format!(
        "SELECT id FROM biz_stock_in WHERE source_order_id IN ({pos}) \
            OR in_no LIKE '{NAME_PREFIX}%' \
            OR supplier_id IN (SELECT id FROM biz_supplier WHERE supplier_code = '{SUP_CODE}')"
    );
    let instances = format!(
        "SELECT id FROM biz_flow_instance \
          WHERE (biz_type = 'PP_APPROVE' AND biz_id IN ({plans})) \
             OR (biz_type = 'PR_APPROVE' AND biz_id IN ({reqs})) \
             OR (biz_type = 'PO_APPROVE' AND biz_id IN ({pos}))"
    );
    let mats = format!("(SELECT id FROM biz_material WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}'))");
    let statements: Vec<String> = vec![
        // ① 状态日志（弱引用，先清）
        format!(
            "DELETE FROM biz_status_log WHERE \
                (entity_type = 'PURCHASE_ORDER' AND entity_id IN ({pos})) \
             OR (entity_type = 'STOCK_IN' AND entity_id IN ({ins})) \
             OR (entity_type = 'PAYABLE' AND entity_id IN \
                    (SELECT id FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id IN ({ins}))) \
             OR (entity_type = 'VOUCHER' AND entity_id IN \
                    (SELECT id FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id IN ({ins})))"
        ),
        // ② 凭证分录 → 凭证 → 应付（都按 source 反查）
        format!(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
                (SELECT id FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id IN ({ins}))"
        ),
        format!("DELETE FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id IN ({ins})"),
        format!("DELETE FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id IN ({ins})"),
        // ③ 审批痕迹：解自引用 → 待办 / 节点 / 抄送 → 实例
        format!("UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE prev_instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_task WHERE instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_instance_node WHERE instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_cc WHERE instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_instance WHERE id IN ({instances})"),
        // ④ 收货单（明细 CASCADE）
        format!("DELETE FROM biz_stock_in WHERE id IN ({ins})"),
        // ⑤ 订单 / 申请（明细 CASCADE）
        format!("DELETE FROM biz_purchase_order WHERE id IN ({pos})"),
        format!("DELETE FROM biz_purchase_requisition WHERE id IN ({reqs})"),
        // ⑥ 计划（明细 CASCADE）
        format!("DELETE FROM biz_purchase_plan WHERE id IN ({plans})"),
        // ⑦ 三本账：流水 → 成本账（FK 指向库位账）→ 库位账
        format!("DELETE FROM biz_inventory_txn WHERE material_id IN {mats}"),
        format!("DELETE FROM biz_inventory WHERE material_id IN {mats}"),
        format!("DELETE FROM biz_inventory_location WHERE material_id IN {mats}"),
        // ⑧ 序列号（一物一码物料才会用到；本夹具不启用，防上轮残留）
        format!("DELETE FROM biz_material_serial WHERE material_id IN {mats}"),
        // ⑨ 主数据（自己命名的夹具行）
        format!("DELETE FROM biz_material WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}')"),
        format!("DELETE FROM biz_warehouse WHERE warehouse_code = '{WH_CODE}'"),
        format!("DELETE FROM biz_supplier WHERE supplier_code = '{SUP_CODE}'"),
    ];
    for statement in statements {
        // 序列号表可能在某些部署里尚未建（V0138 之前）—— 只有这一条允许失败
        if statement.contains("biz_material_serial") {
            let _ = db().exec_sql(statement, vec![]).await;
            continue;
        }
        sql(&statement).await;
    }
    test_seed::purge_user(MAKER).await;
    test_seed::purge_user(KEEPER).await;
    test_seed::purge_user(POSTER).await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db_or_panic().await {
        return None;
    }
    // 主数据先就位（`test_seed` 幂等），再清业务单据。
    fixture().await;
    cleanup().await;
    // 空库里往往只有一个 admin，而 pp/pr/po 三条流程都带职责分离内控
    // （65018「审批人全部是制单人本人」直接拒派）—— 审批岗位必须有**非制单人**的成员。
    // 判据是「有就不动」，种子库上零新增。
    test_seed::ensure_approver_roles().await;
    // 凭证 / 应付的期间闸：按今天（CST，与单据入口同一口径）的开账月
    let today = (Utc::now() + nik::prelude::chrono::Duration::hours(8)).date_naive();
    test_seed::ensure_accounting_period(today.year(), today.month()).await;
    Some(guard)
}

// ============================================================================
// 审批：与 `flow::handler::approve_task` 逐行同款（不直插 APPROVED）
// ============================================================================

/// 把 ERP 的审批回调实现注册进**进程级**注册表。
///
/// 生产是 `bootstrap` 启动期调 `erp::doc_reader_impl::register_all()`；`cargo test --lib`
/// 不起 bootstrap，进程级注册表是**空的** —— 于是 `dispatch_settled` 一个实现都找不到，
/// 审批通过后业务桥不执行，链路会以「审批 APPROVED 但单据仍是 SUBMITTED」这种
/// **看似业务 bug、实则环境缺件**的形态红掉。
async fn ensure_doc_registry() {
    match crate::business::erp::doc_reader_impl::register_all() {
        Ok(count) => println!("[M4-20] ERP 审批回调实现已注册 {count} 个"),
        Err(reason) => println!("[M4-20] ERP 审批回调注册表已就绪（{reason}）"),
    }
    let len = doc_reader::len();
    assert!(
        len >= 18,
        "进程级审批回调注册表必须有 18 个 ERP 实现（否则审批通过不会回调业务单据），实际 {len}"
    );
}

/// 当前在跑的待办（按创建顺序）→ `(task_id, node_key, assignee_id)`。
///
/// **必须带上 `assignee_id`**：节点是 `assignee_type='ROLE'`，受派人由「角色在职成员」
/// 展开，库里有几个该角色的人就有几条待办；拿别人去点会被引擎按「非受派人」拒掉。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, String, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT t.id AS task_id, t.node_key, t.assignee_id \
               FROM biz_flow_task t \
              WHERE t.instance_id = ?::uuid AND t.status = 'PENDING' \
              ORDER BY t.created_at, t.node_key",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查待办");
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("task_id").expect("task id"),
                row.get_typed::<String>("node_key").expect("node key"),
                row.get_typed::<Uuid>("assignee_id").expect("assignee id"),
            )
        })
        .collect()
}

async fn instance_status(instance_id: Uuid) -> Option<String> {
    scalar(
        "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
        vec![Value::from(&instance_id)],
    )
    .await
}

/// 逐节点把实例批到终态，返回最终实例状态（不假设节点数 / 会签人数）。
async fn approve_until_settled(instance_id: Uuid) -> String {
    let mut hops = 0_usize;
    loop {
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        hops += 1;
        assert!(hops <= 10, "审批跳数异常（>10 跳）：实例 {instance_id} 可能成环");
        for (task_id, node_key, assignee) in tasks {
            println!("[M4-20]   审批待办：节点 {node_key} / 受派人 {assignee}");
            doc_reader::guard_self_approval_for_task(task_id, assignee)
                .await
                .unwrap_or_else(|error| panic!("自审闸拒绝（节点 {node_key}）：{} {}", error.code, error.msg));
            doc_reader::guard_approve_for_task(task_id)
                .await
                .unwrap_or_else(|error| panic!("审批前业务闸拒绝（节点 {node_key}）：{} {}", error.code, error.msg));
            let out = db()
                .transaction(|tx| async move { engine::approve(&tx, task_id, assignee, Some("同意")).await })
                .await
                .unwrap_or_else(|error| panic!("审批同意失败（节点 {node_key}）：{error}"));
            doc_reader::dispatch_settled(out.instance_id, &out.instance_status).await;
        }
    }
    instance_status(instance_id)
        .await
        .unwrap_or_else(|| panic!("实例 {instance_id} 不存在"))
}

/// 单据当前挂的审批实例 id（提交审批后必有一个）。
async fn instance_of(table: &str, id: Uuid) -> Uuid {
    scalar(
        &format!("SELECT approval_instance_id::text FROM {table} WHERE id = ?::uuid"),
        vec![Value::from(&id)],
    )
    .await
    .and_then(|text| Uuid::parse_str(&text).ok())
    .unwrap_or_else(|| panic!("{table} 提交审批后必须挂审批实例（id={id}）"))
}

// ============================================================================
// 单据动作（全部走真实入口）
// ============================================================================

fn plan_item(material: Uuid, warehouse: Uuid, qty: &str, price: &str) -> PlanItemReq {
    PlanItemReq {
        material_id: Some(material.to_string()),
        warehouse_id: Some(warehouse.to_string()),
        planned_qty: Some(dec(qty)),
        planned_price: Some(dec(price)),
        remark: None,
    }
}

fn plan_req(name: &str, dept: Uuid, items: Vec<PlanItemReq>) -> CreatePlanReq {
    CreatePlanReq {
        plan_name: Some(format!("{NAME_PREFIX}{name}")),
        plan_type: Some("ANNUAL".to_string()),
        plan_year: Some(2051),
        plan_month: None,
        parent_plan_id: None,
        dept_id: Some(dept.to_string()),
        planner_id: None,
        remark: Some("M4-20 全链夹具".to_string()),
        items,
    }
}

/// 建一张年度计划（两行、同一仓）→ `plan_id`。
async fn create_plan(fx: &Fx, name: &str) -> Uuid {
    plan_handler::create_plan(
        &plan_req(
            name,
            fx.dept,
            vec![
                plan_item(fx.mat_a, fx.warehouse, "100", "2.5"),
                plan_item(fx.mat_b, fx.warehouse, "10", "100"),
            ],
        ),
        Some(fx.maker),
    )
    .await
    .unwrap_or_else(|error| panic!("建采购计划应成功：{} {}", error.code, error.msg))
}

/// 计划提交审批并批到 APPROVED（真发起 `pp_approve`、真派待办、真点同意）。
async fn approve_plan(fx: &Fx, plan_id: Uuid) {
    plan_handler::submit_plan(
        plan_id,
        &SubmitPlanReq::default(),
        Some(fx.maker),
        &ApprovalGate::none(),
    )
    .await
    .unwrap_or_else(|error| panic!("计划提交审批应成功：{} {}", error.code, error.msg));
    let instance = instance_of("biz_purchase_plan", plan_id).await;
    let final_status = approve_until_settled(instance).await;
    assert_eq!(final_status, "APPROVED", "计划审批实例必须走到 APPROVED");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_plan WHERE id = ?::uuid",
            vec![Value::from(&plan_id)]
        )
        .await
        .as_deref(),
        Some("APPROVED"),
        "审批通过回调必须把计划推到 APPROVED"
    );
}

/// 计划转申请（返回 `(申请单 id, 申请单号)`；一仓一单 ⇒ 本夹具恒为 1 张）。
async fn plan_to_requisition(fx: &Fx, plan_id: Uuid) -> (Uuid, String) {
    let out = convert::to_requisition(
        plan_id,
        &ToRequisitionReq {
            dept_id: None,
            applicant_id: None,
            req_date: None,
            demand_date: None,
            tax_rate: Some(dec("13")),
            remark: Some("M4-20 全链：计划转申请".to_string()),
            items: vec![],
        },
        Some(fx.maker),
    )
    .await
    .unwrap_or_else(|error| panic!("计划转申请应成功：{} {}", error.code, error.msg));
    assert_eq!(
        out.requisitions.len(),
        1,
        "同一仓的计划行必须合成 1 张申请单（一仓一单）"
    );
    let req = &out.requisitions[0];
    let id = Uuid::parse_str(&req.requisition_id).expect("申请单 id 是 UUID");
    (id, req.req_no.clone())
}

/// 申请提交审批并批到 APPROVED。
async fn approve_requisition(fx: &Fx, req_id: Uuid) {
    requisition_handler::submit_requisition(
        req_id,
        &SubmitRequisitionReq::default(),
        Some(fx.maker),
        &ApprovalGate::none(),
    )
    .await
    .unwrap_or_else(|error| panic!("申请提交审批应成功：{} {}", error.code, error.msg));
    let instance = instance_of("biz_purchase_requisition", req_id).await;
    let final_status = approve_until_settled(instance).await;
    assert_eq!(final_status, "APPROVED", "申请审批实例必须走到 APPROVED");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_requisition WHERE id = ?::uuid",
            vec![Value::from(&req_id)]
        )
        .await
        .as_deref(),
        Some("APPROVED"),
        "审批通过回调必须把申请单推到 APPROVED"
    );
}

/// 申请转采购订单 → `(po_id, po_no)`。
async fn requisition_to_po(fx: &Fx, req_id: Uuid) -> (Uuid, String) {
    let out = requisition_handler::to_purchase_order(
        req_id,
        &ToPoReq {
            requisition_ids: vec![req_id.to_string()],
            supplier_id: Some(fx.supplier.to_string()),
            warehouse_id: Some(fx.warehouse.to_string()),
            order_date: None,
            expect_date: None,
            remark: Some("M4-20 全链：申请转订单".to_string()),
        },
        Some(fx.maker),
        Utc::now(),
    )
    .await
    .unwrap_or_else(|error| panic!("申请转采购单应成功：{} {}", error.code, error.msg));
    let po_id = Uuid::parse_str(&out.header.po_id).expect("采购单 id 是 UUID");
    (po_id, out.header.po_no.clone())
}

/// 采购单提交审批并批到 APPROVED（`po_approve`：<1 万元走部门主管一跳）。
async fn approve_po(fx: &Fx, po_id: Uuid) {
    purchase_handler::submit_approval(po_id, Some(fx.maker), &ApprovalGate::none())
        .await
        .unwrap_or_else(|error| panic!("采购单提交审批应成功：{} {}", error.code, error.msg));
    let instance = instance_of("biz_purchase_order", po_id).await;
    let final_status = approve_until_settled(instance).await;
    assert_eq!(final_status, "APPROVED", "采购单审批实例必须走到 APPROVED");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po_id)]
        )
        .await
        .as_deref(),
        Some("APPROVED"),
        "审批通过回调必须把采购单推到 APPROVED"
    );
}

/// 从采购单建收货单（`items` 留空 = 服务端带出全部未收完的行）。
async fn create_stock_in_from_po(fx: &Fx, po_id: Uuid, remark: &str) -> Uuid {
    stockin_handler::create_stock_in(
        &CreateStockInReq {
            source_order_id: Some(po_id.to_string()),
            purchase_order_id: None,
            supplier_id: None,
            is_sourceless: None,
            warehouse_id: Some(fx.warehouse.to_string()),
            in_date: None,
            invoice_received: None,
            remark: Some(remark.to_string()),
            items: vec![],
            total_qty: None,
            total_amount: None,
            total_tax: None,
        },
        Some(fx.keeper),
    )
    .await
    .unwrap_or_else(|error| panic!("建收货单应成功：{} {}", error.code, error.msg))
}

/// 收货单过账（系统路径：`DRAFT → POSTED`，六本账一把事务）。
async fn post_stock_in(fx: &Fx, in_id: Uuid) {
    stockin_handler::change_stock_in_status(
        in_id,
        &StockInStatusReq {
            status: Some("POSTED".to_string()),
            ..StockInStatusReq::default()
        },
        Some(fx.poster),
    )
    .await
    .unwrap_or_else(|error| panic!("收货过账应成功：{} {}", error.code, error.msg));
}

// ============================================================================
// 读数（断言用，一律走 SQL —— 回执只是「我以为」，库才是事实）
// ============================================================================

async fn stock_qty(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory WHERE material_id = ?::uuid",
        vec![Value::from(&material_id)],
    )
    .await
}

async fn stock_amount(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(amount), 0)::text FROM biz_inventory WHERE material_id = ?::uuid",
        vec![Value::from(&material_id)],
    )
    .await
}

async fn location_qty(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_location WHERE material_id = ?::uuid",
        vec![Value::from(&material_id)],
    )
    .await
}

async fn txn_in_qty(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_txn \
          WHERE material_id = ?::uuid AND txn_type = 'IN' AND biz_type = 'PURCHASE_IN'",
        vec![Value::from(&material_id)],
    )
    .await
}

/// 某张收货单生成的凭证张数。
async fn voucher_count_of(in_id: Uuid) -> i64 {
    count(
        "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await
}

/// 某张收货单生成的应付张数。
async fn payable_count_of(in_id: Uuid) -> i64 {
    count(
        "SELECT count(*) FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await
}

/// 凭证里某科目的借方合计。
async fn voucher_debit_of(in_id: Uuid, account_code: &str) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(e.debit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid AND e.account_code = ?",
        vec![Value::from(&in_id), Value::Text(account_code.to_string())],
    )
    .await
}

/// 凭证里某科目的贷方合计。
async fn voucher_credit_of(in_id: Uuid, account_code: &str) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(e.credit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid AND e.account_code = ?",
        vec![Value::from(&in_id), Value::Text(account_code.to_string())],
    )
    .await
}

/// 计划执行率详情（与 HTTP handler 同一条路径）。
async fn execution_of(plan_id: Uuid) -> crate::business::erp::purchase::purchase_plan_execution_dto::PlanExecutionRes {
    exec_handler::execution_of(plan_id)
        .await
        .unwrap_or_else(|error| panic!("查执行率失败：{} {}", error.code, error.msg))
}

/// 按**物料**从待收清单取行。
///
/// 不按数组下标：合并成采购单后行序由 `merge_by_material` 决定（不是计划行序），
/// 按下标断言会在「物料顺换了个位置」时红成假故障。
fn pending_line(
    res: &crate::business::erp::purchase::purchase_dto::PendingReceiveRes,
    material: Uuid,
) -> &crate::business::erp::purchase::purchase_dto::PendingItemRes {
    res.items
        .iter()
        .find(|item| item.material_id == material.to_string())
        .unwrap_or_else(|| panic!("待收清单里没有物料 {material}"))
}

// ============================================================================
// 用例 ①：全链（计划 → 申请 → 订单 → 入库）
// ============================================================================

/// **全链一次跑完**：计划 → 计划审批 → 转申请 → 申请审批 → 转订单 → 订单审批 →
/// 收货建单 → 过账 → 三本账 / 凭证 / 应付 / 采购单状态 / 计划执行率，九处交接逐一断言。
///
/// 三处「顺序反证」夹在链中间（未批准计划不能转 / 未审批订单不能收 / 已收满不能再收），
/// 因为它们是同一条链上的闸，脱离链单独跑就证明不了「这条链真的挡得住」。
#[tokio::test]
async fn plan_to_requisition_to_po_to_stockin_full_chain() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let fx = fixture().await;

    // ---------- ① 建采购计划（DRAFT，2 行 / 同一仓） ----------
    println!("[M4-20] 步 1/10 建采购计划（物料甲 100 @2.5 + 物料乙 10 @100 = 1250）");
    let plan_id = create_plan(&fx, "全链计划").await;
    let plan_shape = scalar(
        "SELECT plan_no || '|' || status || '|' || plan_type || '|' || plan_year::text || '|' || total_planned_amount::text \
           FROM biz_purchase_plan WHERE id = ?::uuid",
        vec![Value::from(&plan_id)],
    )
    .await
    .expect("计划存在");
    let parts: Vec<&str> = plan_shape.split('|').collect();
    let plan_no = parts[0].to_string();
    assert!(plan_no.starts_with("PPL-"), "计划单号必须是 PPL- 前缀，实际 {plan_no}");
    assert_eq!(parts[1], "DRAFT", "新建计划必须是草稿，实际 {plan_shape}");
    assert_eq!(parts[2], "ANNUAL", "计划类型沿用请求");
    assert_eq!(parts[3], "2051", "计划年度沿用请求");
    assert_eq!(dec(parts[4]), dec("1250"), "计划金额 = 100×2.5 + 10×100");
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_purchase_plan_item WHERE plan_id = ?::uuid",
            vec![Value::from(&plan_id)]
        )
        .await,
        2,
        "两行明细都要落库"
    );
    let fresh = execution_of(plan_id).await;
    assert_eq!(fresh.plan.exec_state, STATE_NOT_STARTED, "新计划执行率状态必须是未开工");
    assert_eq!(fresh.plan.ordered_qty, Decimal::ZERO);

    // ---------- ②a 反证：DRAFT 计划不能转申请 ----------
    println!("[M4-20] 步 2/10 反证：DRAFT 计划转申请必须 409");
    let error = convert::to_requisition(
        plan_id,
        &ToRequisitionReq {
            tax_rate: Some(dec("13")),
            ..ToRequisitionReq::default()
        },
        Some(fx.maker),
    )
    .await
    .expect_err("未批准的计划不许转采购申请");
    assert_eq!(
        error.status, 409,
        "状态闸是 409，实际 {}（{}）",
        error.status, error.msg
    );
    assert!(
        error.msg.contains("APPROVED"),
        "文案要指名叫人先批准（APPROVED），实际：{}",
        error.msg
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_purchase_requisition WHERE source_plan_id = ?::uuid",
            vec![Value::from(&plan_id)]
        )
        .await,
        0,
        "被拒的转换不许留下任何申请单"
    );

    // ---------- ② 计划提交审批 → APPROVED ----------
    println!("[M4-20] 步 3/10 计划提交审批（pp_approve）→ 逐节点审批");
    let before = scalar(
        "SELECT status FROM biz_purchase_plan WHERE id = ?::uuid",
        vec![Value::from(&plan_id)],
    )
    .await
    .expect("计划存在");
    assert_eq!(before, "DRAFT");
    approve_plan(&fx, plan_id).await;

    // ---------- ③ 计划转申请（一仓一单） ----------
    println!("[M4-20] 步 4/10 计划转采购申请（一仓一单 → 1 张 PRQ，税率 13%）");
    let (req_id, req_no) = plan_to_requisition(&fx, plan_id).await;
    assert!(req_no.starts_with("PRQ-"), "申请单号必须是 PRQ- 前缀，实际 {req_no}");
    let req_shape = scalar(
        "SELECT status || '|' || COALESCE((SELECT SUM(qty) FROM biz_purchase_requisition_item \
                WHERE requisition_id = biz_purchase_requisition.id), 0)::text \
                || '|' || est_amount::text || '|' || source_plan_id::text \
           FROM biz_purchase_requisition WHERE id = ?::uuid",
        vec![Value::from(&req_id)],
    )
    .await
    .expect("申请单存在");
    let req_parts: Vec<&str> = req_shape.split('|').collect();
    assert_eq!(
        req_parts[0], "DRAFT",
        "转出来的申请单是草稿（审批是转单前置），实际 {req_shape}"
    );
    assert_eq!(dec(req_parts[1]), dec("110"), "申请数量 = 100 + 10");
    assert_eq!(dec(req_parts[2]), dec("1250"), "申请预估金额 = 计划金额（不重新定价）");
    assert_eq!(
        req_parts[3],
        plan_id.to_string(),
        "申请单头必须回链来源计划（source_plan_id）"
    );
    let traced = count(
        "SELECT count(*) FROM biz_purchase_requisition_item \
          WHERE requisition_id = ?::uuid AND plan_item_id IS NOT NULL",
        vec![Value::from(&req_id)],
    )
    .await;
    assert_eq!(traced, 2, "申请明细必须逐行带 plan_item_id（全链可追溯）");

    // ---------- ③b 反证：DRAFT 申请不能转订单 ----------
    println!("[M4-20] 步 5/10 反证：DRAFT 申请转采购单必须 409");
    let error = requisition_handler::to_purchase_order(
        req_id,
        &ToPoReq {
            requisition_ids: vec![req_id.to_string()],
            supplier_id: Some(fx.supplier.to_string()),
            warehouse_id: Some(fx.warehouse.to_string()),
            ..ToPoReq::default()
        },
        Some(fx.maker),
        Utc::now(),
    )
    .await
    .expect_err("未审批的申请单不许转采购单");
    assert_eq!(
        error.status, 409,
        "状态闸是 409，实际 {}（{}）",
        error.status, error.msg
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_purchase_order WHERE id IN \
                (SELECT po_id FROM biz_purchase_requisition WHERE id = ?::uuid AND po_id IS NOT NULL)",
            vec![Value::from(&req_id)]
        )
        .await,
        0,
        "被拒的转单不许留下采购单"
    );

    // ---------- ④ 申请提交审批 → APPROVED ----------
    println!("[M4-20] 步 6/10 申请提交审批（pr_approve）→ 逐节点审批");
    approve_requisition(&fx, req_id).await;

    // ---------- ⑤ 申请转采购订单 ----------
    println!("[M4-20] 步 7/10 申请转采购订单（PO）");
    let (po_id, po_no) = requisition_to_po(&fx, req_id).await;
    assert!(po_no.starts_with("PO-"), "采购单号必须是 PO- 前缀，实际 {po_no}");
    let po_shape = scalar(
        "SELECT status || '|' || total_qty::text || '|' || total_amount::text || '|' || total_tax::text \
                || '|' || total_with_tax::text || '|' || supplier_id::text || '|' || warehouse_id::text \
           FROM biz_purchase_order WHERE id = ?::uuid",
        vec![Value::from(&po_id)],
    )
    .await
    .expect("采购单存在");
    let po_parts: Vec<&str> = po_shape.split('|').collect();
    assert_eq!(po_parts[0], "DRAFT", "转单只建草稿订单，实际 {po_shape}");
    assert_eq!(dec(po_parts[1]), dec("110"), "订单数量 = 申请数量");
    assert_eq!(dec(po_parts[2]), dec("1250"), "订单不含税金额 = 250 + 1000");
    assert_eq!(dec(po_parts[3]), dec("162.5"), "税额 = 1250 × 13%");
    assert_eq!(dec(po_parts[4]), dec("1412.5"), "价税合计 = 1250 + 162.5");
    assert_eq!(po_parts[5], fx.supplier.to_string(), "供应商沿用请求");
    assert_eq!(po_parts[6], fx.warehouse.to_string(), "收货仓沿用请求");
    let converted = scalar(
        "SELECT status || '|' || po_id::text || '|' || po_no || '|' || (converted_at IS NOT NULL)::text \
           FROM biz_purchase_requisition WHERE id = ?::uuid",
        vec![Value::from(&req_id)],
    )
    .await
    .expect("申请单存在");
    assert_eq!(
        converted,
        format!("CONVERTED|{po_id}|{po_no}|true"),
        "申请单必须转 CONVERTED 并回填 po_id / po_no / converted_at（全链可追溯）"
    );

    // ---------- ⑤b 反证：DRAFT 采购单不能收货 ----------
    println!("[M4-20] 步 8/10 反证：DRAFT 采购单收货必须 400");
    let error = stockin_handler::create_stock_in(
        &CreateStockInReq {
            source_order_id: Some(po_id.to_string()),
            warehouse_id: Some(fx.warehouse.to_string()),
            remark: Some("M4-20 反证：未审批的采购单".to_string()),
            items: vec![],
            ..CreateStockInReq::default()
        },
        Some(fx.keeper),
    )
    .await
    .expect_err("DRAFT 采购单不许收货");
    assert_eq!(
        error.status, 400,
        "建单期状态闸是 400，实际 {}（{}）",
        error.status, error.msg
    );
    assert!(
        error.msg.contains("不能收货"),
        "文案要说明「这状态不能收货」，实际：{}",
        error.msg
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_stock_in WHERE source_order_id = ?::uuid",
            vec![Value::from(&po_id)]
        )
        .await,
        0,
        "被拒的收货不许留下收货单"
    );

    // ---------- ⑥ 采购单提交审批 → APPROVED ----------
    println!("[M4-20] 步 9/10 采购单提交审批（po_approve：<1 万走部门主管）→ APPROVED");
    approve_po(&fx, po_id).await;
    let pending = purchase_handler::pending_items_of(po_id)
        .await
        .unwrap_or_else(|error| panic!("查待收失败：{} {}", error.code, error.msg));
    assert_eq!(pending.items.len(), 2, "两行都在待收清单里");
    assert_eq!(
        pending_line(&pending, fx.mat_a).received_qty,
        Decimal::ZERO,
        "还没收：已收 0"
    );
    assert_eq!(
        pending_line(&pending, fx.mat_a).pending_qty,
        dec("100"),
        "待收 = 订单量"
    );
    assert!(pending.can_receive, "APPROVED 的订单可以收货");

    // ---------- ⑦ 建收货单（带出未收完的行） ----------
    println!("[M4-20] 步 10/10 建收货单（items 留空 = 从采购单带出）→ POSTED");
    let in_id = create_stock_in_from_po(&fx, po_id, "M4-20 全链：整单收货").await;
    let in_shape = scalar(
        "SELECT in_no || '|' || status || '|' || total_qty::text || '|' || total_amount::text \
                || '|' || total_tax::text || '|' || total_with_tax::text || '|' || in_type \
           FROM biz_stock_in WHERE id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await
    .expect("收货单存在");
    let in_parts: Vec<&str> = in_shape.split('|').collect();
    let in_no = in_parts[0].to_string();
    assert!(in_no.starts_with("IN-"), "收货单号必须是 IN- 前缀，实际 {in_no}");
    assert_eq!(in_parts[1], "DRAFT", "建单只到草稿，实际 {in_shape}");
    assert_eq!(dec(in_parts[2]), dec("110"), "收货数量 = 订单待收量");
    assert_eq!(dec(in_parts[3]), dec("1250"), "收货不含税金额 = 订单不含税金额");
    assert_eq!(dec(in_parts[4]), dec("162.5"), "收货税额");
    assert_eq!(dec(in_parts[5]), dec("1412.5"), "收货价税合计");
    assert_eq!(in_parts[6], "PURCHASE", "有来源采购单的收货单 in_type 必须是 PURCHASE");
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_stock_in_item WHERE stock_in_id = ?::uuid AND source_item_id IS NOT NULL",
            vec![Value::from(&in_id)]
        )
        .await,
        2,
        "两行明细都要带来源采购明细行（source_item_id）"
    );
    // 建单**不动账**：库存 0、无凭证、无应付
    assert_eq!(stock_qty(fx.mat_a).await, Decimal::ZERO, "建单不过账：库存必须还是 0");
    assert_eq!(voucher_count_of(in_id).await, 0, "建单不生成凭证");
    assert_eq!(payable_count_of(in_id).await, 0, "建单不生成应付");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po_id)]
        )
        .await
        .as_deref(),
        Some("APPROVED"),
        "收货单没过账：采购单状态不动（已收数量是派生值，草稿不算）"
    );

    // ---------- ⑧ 过账：三本账 + 凭证 + 应付 + 采购单 RECEIVED ----------
    post_stock_in(&fx, in_id).await;
    assert_eq!(
        scalar(
            "SELECT status FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&in_id)]
        )
        .await
        .as_deref(),
        Some("POSTED"),
        "过账后收货单必须是 POSTED"
    );
    // 三本账：成本账 / 库位账 / 流水
    assert_eq!(stock_qty(fx.mat_a).await, dec("100"), "成本账 0 + 100");
    assert_eq!(stock_qty(fx.mat_b).await, dec("10"), "成本账 0 + 10");
    assert_eq!(location_qty(fx.mat_a).await, dec("100"), "库位账与成本账同事务同量");
    assert_eq!(location_qty(fx.mat_b).await, dec("10"));
    assert_eq!(
        txn_in_qty(fx.mat_a).await,
        dec("100"),
        "物料甲必须留下 100 的 PURCHASE_IN 流水"
    );
    assert_eq!(
        txn_in_qty(fx.mat_b).await,
        dec("10"),
        "物料乙必须留下 10 的 PURCHASE_IN 流水"
    );
    // 成本口径 = 不含税金额（进项税价外，不进存货成本）
    assert_eq!(
        stock_amount(fx.mat_a).await,
        dec("250"),
        "成本金额 = 100 × 2.5（不含税）"
    );
    assert_eq!(
        stock_amount(fx.mat_b).await,
        dec("1000"),
        "成本金额 = 10 × 100（不含税）"
    );
    // 凭证：借 1405（按物料两行）+ 借 2221.01 进项税 / 贷 2202 应付
    assert_eq!(voucher_count_of(in_id).await, 1, "过账必须生成 1 张进项凭证");
    let debit_inventory = voucher_debit_of(in_id, "1405").await;
    let debit_tax = voucher_debit_of(in_id, "2221.01").await;
    let credit_payable = voucher_credit_of(in_id, "2202").await;
    let total_debit = dec_of(
        "SELECT COALESCE(SUM(e.debit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await;
    let total_credit = dec_of(
        "SELECT COALESCE(SUM(e.credit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await;
    assert_eq!(debit_inventory, dec("1250"), "存货借方 = 不含税金额 1250");
    assert_eq!(debit_tax, dec("162.5"), "进项税借方 = 税额 162.5");
    assert_eq!(credit_payable, dec("1412.5"), "应付贷方 = 价税合计 1412.5");
    assert_eq!(
        total_debit, total_credit,
        "凭证必须借贷平衡（借 {total_debit} / 贷 {total_credit}）"
    );
    // 应付单：金额 = 价税合计、状态未结清、来源指回收货单
    assert_eq!(payable_count_of(in_id).await, 1, "过账必须挂 1 张应付");
    let payable_shape = scalar(
        "SELECT payable_no || '|' || amount::text || '|' || status || '|' || source_no \
           FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await
    .expect("应付单存在");
    let payable_parts: Vec<&str> = payable_shape.split('|').collect();
    assert!(
        payable_parts[0].starts_with("AP-"),
        "应付单号必须是 AP- 前缀，实际 {payable_shape}"
    );
    assert_eq!(
        dec(payable_parts[1]),
        dec("1412.5"),
        "应付金额 = 价税合计，实际 {payable_shape}"
    );
    assert_eq!(payable_parts[2], "OPEN", "新挂的应付是未结清，实际 {payable_shape}");
    assert_eq!(payable_parts[3], in_no, "应付来源单号必须指回收货单号");
    assert_eq!(
        scalar(
            "SELECT (voucher_id IS NOT NULL)::text || '|' || (payable_id IS NOT NULL)::text \
               FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&in_id)]
        )
        .await
        .as_deref(),
        Some("true|true"),
        "收货单必须回写 voucher_id / payable_id（详情页要能跳凭证与应付）"
    );
    // 采购单：收满 → RECEIVED；已收数量是派生值
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po_id)]
        )
        .await
        .as_deref(),
        Some("RECEIVED"),
        "整单收完采购单必须转 RECEIVED"
    );
    let pending = purchase_handler::pending_items_of(po_id)
        .await
        .unwrap_or_else(|error| panic!("查待收失败：{} {}", error.code, error.msg));
    assert_eq!(
        pending_line(&pending, fx.mat_a).received_qty,
        dec("100"),
        "已收 = 已过账收货量（派生）"
    );
    assert_eq!(pending_line(&pending, fx.mat_a).pending_qty, Decimal::ZERO, "待收归零");
    assert_eq!(
        pending_line(&pending, fx.mat_b).received_qty,
        dec("10"),
        "物料乙同样收满"
    );
    assert!(!pending.can_receive, "收满后前端不该再显示「生成收货单」");
    // 计划执行率：已下单 = 计划量 → 已下满、剩余 0
    let exec = execution_of(plan_id).await;
    assert_eq!(exec.plan.req_qty, dec("110"), "已申请 = 计划量（转申请时回写）");
    assert_eq!(exec.plan.ordered_qty, dec("110"), "已下单 = 计划量（采购单未作废）");
    assert_eq!(exec.plan.remaining_qty, Decimal::ZERO, "剩余 = 计划 − 已下单 = 0");
    assert_eq!(exec.plan.exec_state, STATE_DONE, "执行率状态必须是已下满");
    assert_eq!(exec.plan.exec_rate, Some(dec("1")), "执行率 = 1.00");

    // ---------- ⑨ 收口 ----------
    println!("[M4-20] 收口：重复过账 / 已收满再收 / 二次转单 / 计划可转量 / 无 PENDING 实例");
    let error = stockin_handler::change_stock_in_status(
        in_id,
        &StockInStatusReq {
            status: Some("POSTED".to_string()),
            ..StockInStatusReq::default()
        },
        Some(fx.poster),
    )
    .await
    .expect_err("已过账的收货单不许重复过账");
    assert_eq!(
        error.status, 409,
        "重复过账是 409，实际 {}（{}）",
        error.status, error.msg
    );

    let error = stockin_handler::create_stock_in(
        &CreateStockInReq {
            source_order_id: Some(po_id.to_string()),
            warehouse_id: Some(fx.warehouse.to_string()),
            remark: Some("M4-20 反证：已收满".to_string()),
            items: vec![],
            ..CreateStockInReq::default()
        },
        Some(fx.keeper),
    )
    .await
    .expect_err("已收满的采购单不许再建收货单");
    assert_eq!(
        error.status, 400,
        "已收满再收是 400，实际 {}（{}）",
        error.status, error.msg
    );
    assert!(
        error.msg.contains("收满"),
        "文案要说明「每行都已收满」，实际：{}",
        error.msg
    );

    let error = requisition_handler::to_purchase_order(
        req_id,
        &ToPoReq {
            requisition_ids: vec![req_id.to_string()],
            supplier_id: Some(fx.supplier.to_string()),
            warehouse_id: Some(fx.warehouse.to_string()),
            ..ToPoReq::default()
        },
        Some(fx.maker),
        Utc::now(),
    )
    .await
    .expect_err("已转单的申请单不许再转第二次");
    assert_eq!(
        error.status, 409,
        "二次转单是 409，实际 {}（{}）",
        error.status, error.msg
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_purchase_order WHERE id IN \
                (SELECT po_id FROM biz_purchase_requisition WHERE source_plan_id = ?::uuid AND po_id IS NOT NULL)",
            vec![Value::from(&plan_id)]
        )
        .await,
        1,
        "全链只允许产出 1 张采购单（重复转单不许造第二张）"
    );

    let exec = execution_of(plan_id).await;
    assert_eq!(
        exec.plan.convertible_qty,
        Decimal::ZERO,
        "计划可转量必须为 0（量已全部转出）"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_flow_instance \
              WHERE status = 'PENDING' \
                AND ((biz_type = 'PP_APPROVE' AND biz_id = ?::uuid) \
                  OR (biz_type = 'PR_APPROVE' AND biz_id = ?::uuid) \
                  OR (biz_type = 'PO_APPROVE' AND biz_id = ?::uuid))",
            vec![Value::from(&plan_id), Value::from(&req_id), Value::from(&po_id)]
        )
        .await,
        0,
        "全链跑完不许留下任何 PENDING 审批实例（死单）"
    );
    // 状态日志必须能回答「这张单一路怎么走过来的」（plan / req 不是 state_machine 实体，无日志）
    let po_log = count(
        "SELECT count(*) FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id = ?::uuid",
        vec![Value::from(&po_id)],
    )
    .await;
    assert!(po_log >= 2, "采购单至少 2 条轨迹（提交 / 审批 / 收满），实际 {po_log}");
    let in_log = count(
        "SELECT count(*) FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await;
    assert!(in_log >= 1, "收货单至少 1 条轨迹（过账），实际 {in_log}");

    println!(
        "[M4-20] 全链通过：{} → {} → {} → {}（库存 110 / 应付 1412.50）",
        plan_no, req_no, po_no, in_no
    );

    // 证据模式：保留夹具供库侧取证（`M420_KEEP_FIXTURES=1`；业务断言已全部跑完）
    if std::env::var("M420_KEEP_FIXTURES").as_deref() == Ok("1") {
        println!("[M4-20] M420_KEEP_FIXTURES=1：保留夹具不清场（供 SQL 取证）");
        return;
    }
    cleanup().await;
}

// ============================================================================
// 用例 ②：分批收货（PARTIAL_RECEIVED → RECEIVED）
// ============================================================================

/// 走到「已批准的采购单」为止（计划 → 申请 → 订单，三步审批全真跑）。
async fn chain_to_approved_po(fx: &Fx) -> (Uuid, Uuid, Uuid) {
    let plan_id = create_plan(fx, "分批计划").await;
    approve_plan(fx, plan_id).await;
    let (req_id, _) = plan_to_requisition(fx, plan_id).await;
    approve_requisition(fx, req_id).await;
    let (po_id, _) = requisition_to_po(fx, req_id).await;
    approve_po(fx, po_id).await;
    (plan_id, req_id, po_id)
}

/// 分批收货：先收 40（物料甲）→ `PARTIAL_RECEIVED`；再收剩余 60 + 10 → `RECEIVED`。
///
/// 钉住三件事：
/// 1. 部分收货**只按已过账量**推进状态（草稿/未收完都不许提前 RECEIVED）；
/// 2. 两张收货单各自生成一套凭证 / 应付，金额相加等于整单（不许重复记账、不许漏记）；
/// 3. 计划执行率**看的是已下单，不是已入库** —— 收了一部分不该让执行率回到「部分下单」。
#[tokio::test]
async fn partial_receipt_advances_po_and_keeps_plan_execution() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let fx = fixture().await;
    let (plan_id, _req_id, po_id) = chain_to_approved_po(&fx).await;

    // ---------- 第一批：只收物料甲的 40（明细显式给） ----------
    let lines = purchase_handler::pending_items_of(po_id)
        .await
        .unwrap_or_else(|error| panic!("查待收失败：{} {}", error.code, error.msg));
    let line_a = lines
        .items
        .iter()
        .find(|item| item.material_id == fx.mat_a.to_string())
        .unwrap_or_else(|| panic!("待收清单里必须有物料甲"));
    let line_a_item_id = line_a.item_id.clone();

    let first = stockin_handler::create_stock_in(
        &CreateStockInReq {
            source_order_id: Some(po_id.to_string()),
            warehouse_id: Some(fx.warehouse.to_string()),
            remark: Some("M4-20 分批：第一批 40".to_string()),
            items: vec![StockInItemReq {
                purchase_order_item_id: Some(line_a_item_id),
                material_id: Some(fx.mat_a.to_string()),
                qty: Some(dec("40")),
                price_excl: Some(dec("2.5")),
                tax_rate: Some(dec("13")),
                location_id: None,
                location_dict_id: None,
                serial_ids: vec![],
                remark: Some("M4-20 分批：第一批".to_string()),
            }],
            ..CreateStockInReq::default()
        },
        Some(fx.keeper),
    )
    .await
    .unwrap_or_else(|error| panic!("建第一批收货单应成功：{} {}", error.code, error.msg));
    post_stock_in(&fx, first).await;

    assert_eq!(stock_qty(fx.mat_a).await, dec("40"), "第一批只加 40");
    assert_eq!(stock_qty(fx.mat_b).await, Decimal::ZERO, "物料乙还没收");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po_id)]
        )
        .await
        .as_deref(),
        Some("PARTIAL_RECEIVED"),
        "部分收货后采购单必须转 PARTIAL_RECEIVED"
    );
    let pending = purchase_handler::pending_items_of(po_id)
        .await
        .unwrap_or_else(|error| panic!("查待收失败：{} {}", error.code, error.msg));
    assert_eq!(
        pending_line(&pending, fx.mat_a).received_qty,
        dec("40"),
        "已收 = 40（派生值）"
    );
    assert_eq!(
        pending_line(&pending, fx.mat_a).pending_qty,
        dec("60"),
        "待收 = 100 − 40"
    );
    assert_eq!(
        pending_line(&pending, fx.mat_b).pending_qty,
        dec("10"),
        "物料乙仍待收 10"
    );
    assert!(pending.can_receive, "还没收满，可以继续收");
    // 第一批自己的一套凭证 / 应付
    assert_eq!(voucher_count_of(first).await, 1, "第一批 1 张凭证");
    assert_eq!(payable_count_of(first).await, 1, "第一批 1 张应付");
    assert_eq!(
        dec_of(
            "SELECT amount::text FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
            vec![Value::from(&first)]
        )
        .await,
        dec("113"),
        "第一批应付 = 40 × 2.5 × 1.13 = 113"
    );
    // 计划执行率不受收货影响（口径是「已下单」）
    let exec = execution_of(plan_id).await;
    assert_eq!(exec.plan.ordered_qty, dec("110"), "已下单与收货无关");
    assert_eq!(exec.plan.exec_state, STATE_DONE, "计划仍是已下满");
    assert_ne!(exec.plan.exec_state, STATE_PARTIAL);

    // ---------- 第二批：带出剩余（60 + 10） ----------
    let second = create_stock_in_from_po(&fx, po_id, "M4-20 分批：第二批余量").await;
    let second_qty = scalar(
        "SELECT total_qty::text FROM biz_stock_in WHERE id = ?::uuid",
        vec![Value::from(&second)],
    )
    .await
    .expect("第二批收货单存在");
    assert_eq!(dec(&second_qty), dec("70"), "第二批带出剩余 60 + 10");
    post_stock_in(&fx, second).await;

    assert_eq!(stock_qty(fx.mat_a).await, dec("100"), "两批合计 = 订单量 100");
    assert_eq!(stock_qty(fx.mat_b).await, dec("10"), "物料乙 10");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po_id)]
        )
        .await
        .as_deref(),
        Some("RECEIVED"),
        "收满后采购单必须转 RECEIVED"
    );
    let pending = purchase_handler::pending_items_of(po_id)
        .await
        .unwrap_or_else(|error| panic!("查待收失败：{} {}", error.code, error.msg));
    assert_eq!(pending_line(&pending, fx.mat_a).pending_qty, Decimal::ZERO);
    assert_eq!(pending_line(&pending, fx.mat_b).pending_qty, Decimal::ZERO);
    // 两批应付合计 = 整单价税合计（不许重复记账）
    let payable_total = dec_of(
        "SELECT COALESCE(SUM(amount), 0)::text FROM biz_payable \
          WHERE source_type = 'STOCK_IN' AND source_id IN (SELECT id FROM biz_stock_in WHERE source_order_id = ?::uuid)",
        vec![Value::from(&po_id)],
    )
    .await;
    assert_eq!(payable_total, dec("1412.5"), "两批应付合计 = 1412.5，不许重复也不许漏");
    // 两批凭证借贷合计也等于整单
    let debit_total = dec_of(
        "SELECT COALESCE(SUM(e.debit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id IN \
                (SELECT id FROM biz_stock_in WHERE source_order_id = ?::uuid)",
        vec![Value::from(&po_id)],
    )
    .await;
    assert_eq!(debit_total, dec("1412.5"), "两批凭证借方合计 = 整单");

    cleanup().await;
}

// ============================================================================
// 用例 ③：清场零残留（与 drill 脚本的库侧断言互为双保险）
// ============================================================================

/// 全链跑完后夹具零残留。
///
/// 放在同一个文件里而不是只在脚本里：脚本只能看「表里有没有前缀行」，
/// 而这条用例看的是「业务单据 + 审批实例 + 凭证 / 应付 + 三本账 + 状态日志」一起归零。
#[tokio::test]
async fn zz_teardown_leaves_no_purchase_chain_residue() {
    let Some(_guard) = setup().await else { return };
    let fx = fixture().await;
    let mats = format!("(SELECT id FROM biz_material WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}'))");
    let plans = format!("(SELECT id FROM biz_purchase_plan WHERE plan_name LIKE '{NAME_PREFIX}%')");
    let checks: Vec<(&str, String, Vec<Value>)> = vec![
        (
            "采购计划",
            format!("SELECT count(*) FROM biz_purchase_plan WHERE id IN {plans}"),
            vec![],
        ),
        (
            "采购申请",
            "SELECT count(*) FROM biz_purchase_requisition WHERE source_plan_id IN \
                (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%')"
                .to_string(),
            vec![],
        ),
        (
            "采购订单",
            "SELECT count(*) FROM biz_purchase_order WHERE id IN \
                (SELECT po_id FROM biz_purchase_requisition WHERE po_id IS NOT NULL AND source_plan_id IN \
                    (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%'))"
                .to_string(),
            vec![],
        ),
        (
            "收货单",
            "SELECT count(*) FROM biz_stock_in WHERE supplier_id = ?::uuid".to_string(),
            vec![Value::from(&fx.supplier)],
        ),
        (
            "应付",
            "SELECT count(*) FROM biz_payable WHERE supplier_id = ?::uuid".to_string(),
            vec![Value::from(&fx.supplier)],
        ),
        (
            "审批实例",
            "SELECT count(*) FROM biz_flow_instance WHERE biz_type IN ('PP_APPROVE','PR_APPROVE','PO_APPROVE') \
                  AND biz_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%')"
                .to_string(),
            vec![],
        ),
        (
            "成本账",
            format!("SELECT count(*) FROM biz_inventory WHERE material_id IN {mats}"),
            vec![],
        ),
        (
            "库位账",
            format!("SELECT count(*) FROM biz_inventory_location WHERE material_id IN {mats}"),
            vec![],
        ),
        (
            "库存流水",
            format!("SELECT count(*) FROM biz_inventory_txn WHERE material_id IN {mats}"),
            vec![],
        ),
        (
            "全部状态的待办（PENDING 不许留死单）",
            "SELECT count(*) FROM biz_flow_task t JOIN biz_flow_instance i ON i.id = t.instance_id \
              WHERE t.status = 'PENDING' AND i.biz_type IN ('PP_APPROVE','PR_APPROVE','PO_APPROVE') \
                AND i.biz_id IN (SELECT id FROM biz_purchase_plan WHERE plan_name LIKE 'M420-%')"
                .to_string(),
            vec![],
        ),
    ];
    // 先真跑一遍全链的一半（到已批准采购单），再清场，断言归零 —— 只断言「本来就空」等于没测。
    let (_plan_id, _req_id, po_id) = chain_to_approved_po(&fx).await;
    let in_id = create_stock_in_from_po(&fx, po_id, "M4-20 清场用例：整单收货").await;
    post_stock_in(&fx, in_id).await;
    assert!(
        count(
            "SELECT count(*) FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
            vec![Value::from(&in_id)]
        )
        .await
            > 0,
        "清场前必须先有痕迹（否则这条用例什么都没证明）"
    );
    cleanup().await;
    for (label, query, params) in checks {
        assert_eq!(count(&query, params).await, 0, "{label} 必须零残留");
    }
}

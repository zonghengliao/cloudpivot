// ============================================================================
// framework_contract_e2e_tests.rs —— 采购缺口 E2E：框架协议 + 合同关联（M4-21）
//
// 卡：t_b62c50ae（M4-21【M4·采购】采购缺口 E2E：框架协议 + 合同关联 · 含超限场景）
//
// # 这一份钉的是什么（一句话）
//
// **采购单的两条「带定价约束」的下单路线各走一遍完整链路，并且把超限钉在链中间**：
//
// ```text
//   路线 A（框架协议）：建协议 → 提交 → FWA_APPROVE 两节点真批 → 零星采购单引用协议
//                       → 执行量涨 → 行上限 / 单价容差 / 协议外物料 / 有效期 / 供应商
//                       → 单头总量与总额 → 作废释放额度 → 收货过账（成本 = 协议价）
//   路线 B（合同采购）：合同 67.80 → CONTRACT 单（供应商从乙方派生）→ 累计到边界
//                       → 超合同额 66019 → 作废释放 → 收货过账 → 合同执行量不因收货翻倍
//   互斥闸：合同采购不能同时挂框架协议；零星采购不能挂合同（两个价格上限说不清以谁为准）
// ```
//
// # 为什么需要这份用例（前面几张卡各自钉死了自己那一段，但没有一条串起来）
//
// ```text
// framework_agreement_db_tests         ← 协议两表结构 / 有效期 / 上限语义（SQL 夹具）
// framework_agreement_ref_db_tests     ← 下单引用校验（单段：协议是 SQL 夹具，采购单一条一条建）
// framework_agreement_execution_db_tests ← 执行量报表（单段：不检查协议怎么来的）
// framework_agreement_over_limit_tests ← 超限纯逻辑 + 真库反例（单段）
// framework_agreement_crud_db_tests    ← 建/改/删/提交/审批 + **一张**单涨执行量（到下单为止，从不收货）
// contract_execution_db_tests (M4-19)  ← 合同执行量（单段：合同是 SQL 夹具）
// purchase_chain_e2e_tests (M4-20)     ← 计划 → 申请 → 订单 → 入库（**不挂协议、不挂合同**）
// 本文件（M4-21）                      ← 协议/合同**从建单到入库**的完整路线，超限夹在链中间
// ```
//
// 于是「审批通过的协议真能一路收到账吗」「协议价是不是真的落成了库存成本、含税额是不是真的
// 落成了应付」「收货会不会把协议/合同的已用量再算一遍」这三件事，在库里从来没有被验证过。
//
// # 五条实测口径（写断言前必须先看，别照直觉写）
//
// 1. **协议金额上限按「协议价是否含税」的口径比**：本文件协议 `price_includes_tax = false`，
//    所以 `total_max_amount` 比的是 **Σ 不含税金额**（`biz_purchase_order_item.amount`）。
//    拿含税额去比会白白多算一个税率，边界永远撞不上。
// 2. **合同额度与合同执行量都按 `total_with_tax` 现算**（M4-18/M4-19 同一口径）。
//    两条路线的分母口径**不同**，混着写必假红。
// 3. **收货过账不占协议 / 合同额度**：已用量来自「未作废未删除的采购单」，收货单不在其中。
//    过账后执行量必须**一动不动** —— 这条是本文件防「重复计额」的硬闸。
// 4. **草稿单也占额度**（`usage_of_agreement` 只排除作废 / 软删，不看状态）：
//    所以超限反证之前必须先记基线，事后断言「一行没多」。
// 5. **作废才释放额度**（`DRAFT → CANCELLED` 也释放，M4-08 `release_order`）。
//    释放后能重新下满，是本文件证明「额度不是只减不加」的活证据。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `M421-` 夹具、同一份 `biz_seq` 号段），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m421_e2e" cargo test -p cloudpivot --lib \
//   business::erp::purchase::framework_contract_e2e_tests -- --test-threads=1
// ```
//
// 也可直接跑 `bash scripts/verify/m421_framework_contract_e2e_drill.sh`
// （克隆演练库 → 补迁移 → 前置断言 → 跑本文件 + 采购/收货域回归 → 夹具零残留）。
// ============================================================================

use nik::prelude::chrono::{Datelike, Duration, NaiveDate};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::contract::contract_execution_handler as contract_exec;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db as shared_ensure_db};
use crate::business::erp::purchase::framework_agreement_dto::{
    AgreementItemReq, CreateAgreementReq, SubmitAgreementReq,
};
use crate::business::erp::purchase::framework_agreement_execution_dto::{
    AgreementExecItemRes, STATE_DONE, STATE_NOT_STARTED, STATE_PARTIAL, STATE_UNLIMITED,
};
use crate::business::erp::purchase::framework_agreement_execution_handler as fwa_exec;
use crate::business::erp::purchase::framework_agreement_handler as fwa_handler;
use crate::business::erp::purchase::framework_agreement_model as fwa_model;
use crate::business::erp::purchase::purchase_dto::{
    CreatePurchaseOrderReq, PurchaseOrderItemReq, PurchaseOrderStatusReq, today_cst,
};
use crate::business::erp::purchase::purchase_handler;
use crate::business::erp::stockin::stockin_dto::{CreateStockInReq, StockInStatusReq};
use crate::business::erp::stockin::stockin_handler;
use crate::business::flow::{doc_reader, engine};
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

// ============================================================================
// 夹具命名空间（清场按它反查，绝不按生产号段删）
// ============================================================================

/// 夹具名前缀（协议名）。
const NAME_PREFIX: &str = "M421-";
/// 夹具仓库编码。
const WH_CODE: &str = "M421-WH";
/// 夹具物料甲（有行上限：100）。
const MAT_A_CODE: &str = "M421-MAT-A";
/// 夹具物料乙（另一行，行上限给得宽：1000）。
const MAT_B_CODE: &str = "M421-MAT-B";
/// 夹具物料丙（**不在**协议物料范围里，验「协议外物料」反证）。
const MAT_C_CODE: &str = "M421-MAT-C";
/// 夹具供应商（协议签约方 / 合同乙方）。
const SUP_CODE: &str = "M421-SUP";
/// 第二个供应商（验「协议供应商 ≠ 订单供应商」）。
const SUP2_CODE: &str = "M421-SUP2";
/// 夹具客户（销售合同夹具的乙方 —— `ck_biz_contract_type_party_pair` 要求 SALES ⇔ CUSTOMER）。
const CUS_CODE: &str = "M421-CUS";
/// 合同号前缀（合同夹具按它清）。
const CONTRACT_PREFIX: &str = "M421-HT-";

/// 采购制单人（提交审批的人）—— **不绑任何审批角色**，否则会撞制单/审批分离闸。
const MAKER: &str = "m421_maker";
/// 收货制单人（仓管）。
const KEEPER: &str = "m421_keeper";
/// 过账人 —— 与收货制单人分开，ERP-B2「制单 ≠ 过账」开关打开时也成立。
const POSTER: &str = "m421_poster";

/// 合同夹具固定 id：采购合同（额度 67.80 = 三张 22.60 的单，边界好撞）。
const C_PUR: &str = "00000000-0000-4000-8000-000004210001";
/// 合同夹具固定 id：草稿状态采购合同（不可用于下单）。
const C_DRAFT: &str = "00000000-0000-4000-8000-000004210002";
/// 合同夹具固定 id：销售合同（类型不对）。
const C_SALES: &str = "00000000-0000-4000-8000-000004210003";
/// 合同夹具固定 id：已软删采购合同。
const C_GONE: &str = "00000000-0000-4000-8000-000004210004";

/// 标准合同采购单（10 × 2.00 + 13% ⇒ 含税 22.60）。
const ORDER_WITH_TAX: &str = "22.60";

// ============================================================================
// 底座：库连接 / 读数
// ============================================================================

fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>()
        .unwrap_or_else(|error| panic!("定点字面量非法（{text}）：{error}"))
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap_or_else(|error| panic!("日期字面量非法（{text}）：{error}"))
}

/// 真库就绪判定：**设了 `ERP_DB_URL` 却连不上 ⇒ 直接 panic**。
///
/// 仓里通用的 `ensure_db()` 连不上只打印一行 `SKIP:` 然后返回 `false`，用例随即 `return` ——
/// 在 cargo 眼里那是 **ok**。对真库用例这就是「假绿」：连接串写错 / 库没起来 / 库名不存在
/// 全都静默变成「全绿」。这里堵死：**没配**才允许跳过。
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

struct Fx {
    maker: Uuid,
    keeper: Uuid,
    poster: Uuid,
    supplier: Uuid,
    supplier2: Uuid,
    customer: Uuid,
    warehouse: Uuid,
    mat_a: Uuid,
    mat_b: Uuid,
    mat_c: Uuid,
}

async fn fixture() -> Fx {
    // 物料必须挂**基本单位**：`unit_guard` 只认 `is_base = 1`，而 `ensure_material`
    // 挑的是「第一个启用单位」（模板库里可能是「箱」）→ 采购/收货建单直接 66030。
    // `upsert_material` 是 `ON CONFLICT DO NOTHING`，早先失败留下的脏夹具要显式校正。
    let mat_a = test_seed::ensure_material_base_unit(MAT_A_CODE, "M4-21 框架协议夹具物料甲").await;
    let mat_b = test_seed::ensure_material_base_unit(MAT_B_CODE, "M4-21 框架协议夹具物料乙").await;
    let mat_c = test_seed::ensure_material_base_unit(MAT_C_CODE, "M4-21 协议外夹具物料丙").await;
    let unit = test_seed::ensure_base_unit().await;
    for material in [mat_a, mat_b, mat_c] {
        exec_params(
            "UPDATE biz_material SET unit_id = ?::uuid, deleted_at = NULL WHERE id = ?::uuid",
            vec![Value::from(&unit), Value::from(&material)],
        )
        .await;
    }
    // 客户（只给销售合同夹具当乙方；`SALES ⇔ CUSTOMER` 是库层 CHECK）。
    // 主数据一律走 `test_seed::ensure_*`（scripts/verify 的 test_seed_gate 只许降不许升）。
    let customer = test_seed::ensure_customer(CUS_CODE, "M4-21 对照客户").await;
    Fx {
        maker: test_seed::ensure_user(MAKER, "M4-21 采购制单人", None).await,
        keeper: test_seed::ensure_user(KEEPER, "M4-21 收货制单人", None).await,
        poster: test_seed::ensure_user(POSTER, "M4-21 过账人", None).await,
        supplier: test_seed::ensure_supplier(SUP_CODE, "M4-21 框架协议供应商").await,
        supplier2: test_seed::ensure_supplier(SUP2_CODE, "M4-21 对照供应商").await,
        customer,
        warehouse: test_seed::ensure_warehouse(WH_CODE, "M4-21 收货仓").await,
        mat_a,
        mat_b,
        mat_c,
    }
}

/// 清场：只清本文件**业务单据**及其全部痕迹（按夹具主数据反查，不扫生产号段）。
///
/// 顺序不可交换：
/// 1. `biz_status_log` 是弱引用（无外键）⇒ **先于**单据按 id 清（单据一删就查不到 id）；
/// 2. 凭证 / 应付是弱引用（`source_type` + `source_id`）⇒ 在删收货单**之前**清；
/// 3. 审批实例 `biz_id` 也是弱引用，且 `prev_instance_id` 自引用无 ON DELETE ⇒ 先解开再删；
/// 4. 外键链：订单 / 协议明细 CASCADE，删单头即可；
///    `biz_inventory`（FK 指向库位账）→ `biz_inventory_location` 要**倒序**删。
async fn cleanup() {
    let agreements = format!("SELECT id FROM biz_framework_agreement WHERE agreement_name LIKE '{NAME_PREFIX}%'");
    let orders = format!(
        "SELECT id FROM biz_purchase_order WHERE supplier_id IN \
            (SELECT id FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}'))"
    );
    let ins = format!(
        "SELECT id FROM biz_stock_in WHERE source_order_id IN ({orders}) \
            OR supplier_id IN (SELECT id FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}'))"
    );
    let instances = format!(
        "SELECT id FROM biz_flow_instance \
          WHERE (biz_type = 'FWA_APPROVE' AND biz_id IN ({agreements})) \
             OR (biz_type = 'PO_APPROVE' AND biz_id IN ({orders}))"
    );
    let mats = format!(
        "(SELECT id FROM biz_material WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}', '{MAT_C_CODE}'))"
    );
    let statements: Vec<String> = vec![
        // ① 状态日志（弱引用，先清）
        format!(
            "DELETE FROM biz_status_log WHERE \
                (entity_type = 'PURCHASE_ORDER' AND entity_id IN ({orders})) \
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
        // ⑤ 采购单（明细 CASCADE）
        format!("DELETE FROM biz_purchase_order WHERE id IN ({orders})"),
        // ⑥ 框架协议（明细 CASCADE）
        format!("DELETE FROM biz_framework_agreement WHERE id IN ({agreements})"),
        // ⑦ 合同夹具（按号段）
        format!("DELETE FROM biz_contract WHERE contract_no LIKE '{CONTRACT_PREFIX}%'"),
        // ⑧ 三本账：流水 → 成本账（FK 指向库位账）→ 库位账
        format!("DELETE FROM biz_inventory_txn WHERE material_id IN {mats}"),
        format!("DELETE FROM biz_inventory WHERE material_id IN {mats}"),
        format!("DELETE FROM biz_inventory_location WHERE material_id IN {mats}"),
        // ⑨ 序列号（一物一码物料才会用到；本夹具不启用，防上轮残留）
        format!("DELETE FROM biz_material_serial WHERE material_id IN {mats}"),
        // ⑩ 主数据（自己命名的夹具行）
        format!("DELETE FROM biz_material WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}', '{MAT_C_CODE}')"),
        format!("DELETE FROM biz_warehouse WHERE warehouse_code = '{WH_CODE}'"),
        format!("DELETE FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}')"),
        format!("DELETE FROM biz_customer WHERE code = '{CUS_CODE}'"),
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
    // 空库里往往只有一个 admin，而 FWA / PO 两条流程都带职责分离内控
    // （65018「审批人全部是制单人本人」直接拒派）—— 审批岗位必须有**非制单人**的成员。
    test_seed::ensure_approver_roles().await;
    // 凭证 / 应付的期间闸：按今天（CST，与单据入口同一口径）的开账月
    let today = today_cst();
    test_seed::ensure_accounting_period(today.year(), today.month()).await;
    // 真库自证标记：演练脚本靠它区分「用例真跑了」与「没连上库、提前 return 也算 passed」
    println!("[M4-21] setup 走通（连上库 + 清场 + 审批岗位就绪）");
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
        Ok(count) => println!("[M4-21] ERP 审批回调实现已注册 {count} 个"),
        Err(reason) => println!("[M4-21] ERP 审批回调注册表已就绪（{reason}）"),
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

/// 逐节点把实例批到终态，返回 `(走过的节点数, 最终实例状态)`（不假设节点数 / 会签人数）。
async fn approve_until_settled(instance_id: Uuid) -> (usize, String) {
    let mut hops = 0_usize;
    loop {
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        hops += 1;
        assert!(hops <= 10, "审批跳数异常（>10 跳）：实例 {instance_id} 可能成环");
        for (task_id, node_key, assignee) in tasks {
            println!("[M4-21]   审批待办：节点 {node_key} / 受派人 {assignee}");
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
    let status = instance_status(instance_id)
        .await
        .unwrap_or_else(|| panic!("实例 {instance_id} 不存在"));
    (hops, status)
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

/// 协议物料范围一行（税率 13% 分数制 `0.13`）。
fn agreement_item(material: Uuid, price: &str, max_qty: Option<&str>) -> AgreementItemReq {
    AgreementItemReq {
        material_id: Some(material.to_string()),
        agreed_price: Some(dec(price)),
        tax_rate: Some(dec("0.13")),
        max_qty: max_qty.map(dec),
        remark: Some("M4-21 夹具".to_string()),
    }
}

/// 建协议请求（各用例只改自己关心的字段）。
fn agreement_req(
    name: &str,
    supplier: Uuid,
    items: Vec<AgreementItemReq>,
    total_max_qty: Option<&str>,
    total_max_amount: Option<&str>,
) -> CreateAgreementReq {
    CreateAgreementReq {
        agreement_name: Some(format!("{NAME_PREFIX}{name}")),
        supplier_id: Some(supplier.to_string()),
        dept_id: None,
        owner_id: None,
        start_date: Some("2026-01-01".to_string()),
        end_date: Some("2099-12-31".to_string()),
        sign_date: Some("2025-12-20".to_string()),
        price_mode: Some("FIXED".to_string()),
        // 协议价**不含税**：`total_max_amount` 因此比 Σ 不含税金额（口径 ①）
        price_includes_tax: Some(false),
        price_currency: Some("CNY".to_string()),
        price_tolerance: Some(dec("0.05")),
        price_adjust_rule: None,
        total_max_qty: total_max_qty.map(dec),
        total_max_amount: total_max_amount.map(dec),
        remark: Some(format!("{NAME_PREFIX}E2E 夹具")),
        items,
    }
}

async fn create_agreement(fx: &Fx, req: &CreateAgreementReq) -> Uuid {
    fwa_handler::create_agreement(req, Some(fx.maker))
        .await
        .unwrap_or_else(|error| panic!("建框架协议应成功：{} {}", error.code, error.msg))
}

async fn agreement_status(id: Uuid) -> Option<String> {
    // 与 `fwa_model::find_agreement` 同口径：软删的行视为「不存在」（审计仍留明细）
    scalar(
        "SELECT status FROM biz_framework_agreement WHERE id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&id)],
    )
    .await
}

/// 提交 + 逐节点真批到 APPROVED，返回走过的节点数。
async fn approve_agreement(fx: &Fx, id: Uuid) -> usize {
    fwa_handler::submit_agreement(
        id,
        &SubmitAgreementReq::default(),
        Some(fx.maker),
        &ApprovalGate::none(),
    )
    .await
    .unwrap_or_else(|error| panic!("协议提交审批应成功：{} {}", error.code, error.msg));
    let instance = instance_of("biz_framework_agreement", id).await;
    let (hops, status) = approve_until_settled(instance).await;
    assert_eq!(status, "APPROVED", "协议审批实例必须走到 APPROVED");
    assert_eq!(
        agreement_status(id).await.as_deref(),
        Some("APPROVED"),
        "审批通过回调必须把协议推到 APPROVED"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_flow_instance WHERE id = ?::uuid AND status = 'APPROVED'",
            vec![Value::from(&instance)]
        )
        .await,
        1,
        "审批实例本身也必须落成 APPROVED"
    );
    hops
}

/// 采购单明细一行（不含税单价口径 `EXCL`，税率百分数 13）。
fn po_line(material: Uuid, qty: &str, price_excl: &str) -> PurchaseOrderItemReq {
    PurchaseOrderItemReq {
        material_id: Some(material.to_string()),
        qty: Some(dec(qty)),
        price_excl: Some(dec(price_excl)),
        price_incl: None,
        tax_rate: Some(dec("13")),
        remark: None,
    }
}

/// 零星采购单（挂框架协议）请求。
fn spot_po(fx: &Fx, agreement: Option<Uuid>, items: Vec<PurchaseOrderItemReq>) -> CreatePurchaseOrderReq {
    CreatePurchaseOrderReq {
        supplier_id: Some(fx.supplier.to_string()),
        warehouse_id: Some(fx.warehouse.to_string()),
        purchase_type: Some("SPOT".to_string()),
        contract_id: None,
        framework_agreement_id: agreement.map(|id| id.to_string()),
        project_id: None,
        order_date: None,
        expect_date: None,
        price_mode: Some("EXCL".to_string()),
        credit_days: None,
        remark: Some(format!("{NAME_PREFIX}E2E 零星采购")),
        items,
    }
}

/// 合同采购单请求（供应商从合同乙方派生，**刻意不填**）。
fn contract_po(fx: &Fx, contract: Uuid, qty: &str, price_excl: &str) -> CreatePurchaseOrderReq {
    CreatePurchaseOrderReq {
        supplier_id: None,
        warehouse_id: Some(fx.warehouse.to_string()),
        purchase_type: Some("CONTRACT".to_string()),
        contract_id: Some(contract.to_string()),
        framework_agreement_id: None,
        project_id: None,
        order_date: None,
        expect_date: None,
        price_mode: Some("EXCL".to_string()),
        credit_days: None,
        remark: Some(format!("{NAME_PREFIX}E2E 合同采购")),
        items: vec![po_line(fx.mat_a, qty, price_excl)],
    }
}

async fn create_po(fx: &Fx, req: &CreatePurchaseOrderReq) -> std::result::Result<Uuid, ApiError> {
    purchase_handler::create_purchase_order(req, Some(fx.maker)).await
}

/// 采购单提交审批并批到 APPROVED（`po_approve`）。
async fn approve_po(fx: &Fx, po_id: Uuid) {
    purchase_handler::submit_approval(po_id, Some(fx.maker), &ApprovalGate::none())
        .await
        .unwrap_or_else(|error| panic!("采购单提交审批应成功：{} {}", error.code, error.msg));
    let instance = instance_of("biz_purchase_order", po_id).await;
    let (_, status) = approve_until_settled(instance).await;
    assert_eq!(status, "APPROVED", "采购单审批实例必须走到 APPROVED");
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
async fn create_stock_in_from_po(fx: &Fx, po_id: Uuid) -> Uuid {
    stockin_handler::create_stock_in(
        &CreateStockInReq {
            source_order_id: Some(po_id.to_string()),
            purchase_order_id: None,
            supplier_id: None,
            is_sourceless: None,
            warehouse_id: Some(fx.warehouse.to_string()),
            in_date: None,
            invoice_received: None,
            remark: Some(format!("{NAME_PREFIX}E2E 收货")),
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

/// 收货单过账（`DRAFT → POSTED`，六本账一把事务）。
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

/// 作废采购单（`DRAFT / SUBMITTED → CANCELLED`；作废即释放协议 / 合同额度）。
async fn cancel_po(fx: &Fx, po_id: Uuid, reason: Option<&str>) {
    purchase_handler::change_purchase_order_status(
        po_id,
        &PurchaseOrderStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: reason.map(str::to_string),
            ..PurchaseOrderStatusReq::default()
        },
        Some(fx.maker),
    )
    .await
    .unwrap_or_else(|error| panic!("作废采购单应成功：{} {}", error.code, error.msg));
}

// ============================================================================
// 读数（断言用，一律走 SQL / 只读接口 —— 回执只是「我以为」，库才是事实）
// ============================================================================

/// 某供应商名下**未软删**的采购单张数（超限反证用：被拒的单不许落库）。
async fn order_count(fx: &Fx) -> i64 {
    count(
        "SELECT count(*) FROM biz_purchase_order \
          WHERE supplier_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&fx.supplier)],
    )
    .await
}

/// 协议执行量详情（M4-09 的只读接口，与 HTTP 同一条路径）。
async fn agreement_exec(
    agreement_id: Uuid,
) -> crate::business::erp::purchase::framework_agreement_execution_dto::AgreementExecutionRes {
    fwa_exec::execution_of(agreement_id)
        .await
        .unwrap_or_else(|error| panic!("查协议执行量失败：{} {}", error.code, error.msg))
}

/// 按**物料**取协议执行量行（不按下标：行序由协议行号决定，与断言顺序无关）。
fn exec_line(
    res: &crate::business::erp::purchase::framework_agreement_execution_dto::AgreementExecutionRes,
    material: Uuid,
) -> &AgreementExecItemRes {
    res.items
        .iter()
        .find(|item| item.material_id == material.to_string())
        .unwrap_or_else(|| panic!("协议执行量里没有物料 {material}"))
}

/// 合同执行量详情（M4-19 的只读接口）。
async fn contract_exec_detail(
    contract_id: Uuid,
) -> crate::business::erp::contract::contract_execution_dto::ContractExecutionRes {
    contract_exec::execution_of(contract_id)
        .await
        .unwrap_or_else(|error| panic!("查合同执行量失败：{} {}", error.code, error.msg))
}

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

async fn voucher_debit_of(in_id: Uuid, account_code: &str) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(e.debit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid AND e.account_code = ?",
        vec![Value::from(&in_id), Value::Text(account_code.to_string())],
    )
    .await
}

async fn voucher_credit_of(in_id: Uuid, account_code: &str) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(e.credit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid AND e.account_code = ?",
        vec![Value::from(&in_id), Value::Text(account_code.to_string())],
    )
    .await
}

// ============================================================================
// 合同夹具
//
// 合同有 CRUD 接口，但 handler 要 `Extension<AuthClaims>`，真库用例拿不到登录态；
// 本文件只让合同行「合法存在」—— `ck_biz_contract_*` 九条 CHECK 在库层兜着。
// ============================================================================

/// 一份合同夹具（字段多，用一个结构体而不是 9 个位置参数）。
struct ContractSpec<'a> {
    id: &'a str,
    contract_no: String,
    name: &'a str,
    contract_type: &'a str,
    party_b_type: &'a str,
    party_b: Uuid,
    amount: &'a str,
    status: &'a str,
    deleted: bool,
}

async fn seed_contract(spec: &ContractSpec<'_>) {
    let today = today_cst();
    let start = today - Duration::days(30);
    let end = today + Duration::days(365);
    let id = Uuid::parse_str(spec.id).expect("合同夹具 id 必须是 UUID");
    let deleted_at = if spec.deleted { "now()" } else { "NULL" };
    // 日期内联（都是本函数算出来的 `NaiveDate`，不含外部输入）；金额走 `?::text::numeric`
    // 让服务端把参数推断成 text —— 直接 `?::numeric` 会推成 numeric 而客户端送的是字符串。
    exec_params(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status, deleted_at)
             VALUES (?::uuid, ?, ?, ?, ?, ?::uuid, ?, ?::text::numeric, 'CNY',
                     '{start}'::date, '{start}'::date, '{end}'::date, ?, {deleted_at})"
        ),
        vec![
            Value::from(&id),
            Value::Text(spec.contract_no.clone()),
            Value::Text(spec.name.to_string()),
            Value::Text(spec.contract_type.to_string()),
            Value::Text(spec.party_b_type.to_string()),
            Value::from(&spec.party_b),
            Value::Text("M4-21 合同乙方".to_string()),
            Value::Text(spec.amount.to_string()),
            Value::Text(spec.status.to_string()),
        ],
    )
    .await;
}

/// 本文件四个合同夹具（每次重建，内容一致）。
async fn seed_contracts(fx: &Fx) {
    seed_contract(&ContractSpec {
        id: C_PUR,
        contract_no: format!("{CONTRACT_PREFIX}PUR"),
        name: "M4-21 采购合同（额度 67.80）",
        contract_type: "PURCHASE",
        party_b_type: "SUPPLIER",
        party_b: fx.supplier,
        amount: "67.80",
        status: "ACTIVE",
        deleted: false,
    })
    .await;
    seed_contract(&ContractSpec {
        id: C_DRAFT,
        contract_no: format!("{CONTRACT_PREFIX}DRAFT"),
        name: "M4-21 采购合同（草稿）",
        contract_type: "PURCHASE",
        party_b_type: "SUPPLIER",
        party_b: fx.supplier,
        amount: "100.00",
        status: "DRAFT",
        deleted: false,
    })
    .await;
    seed_contract(&ContractSpec {
        id: C_SALES,
        contract_no: format!("{CONTRACT_PREFIX}SAL"),
        name: "M4-21 销售合同",
        contract_type: "SALES",
        party_b_type: "CUSTOMER",
        party_b: fx.customer,
        amount: "100.00",
        status: "ACTIVE",
        deleted: false,
    })
    .await;
    seed_contract(&ContractSpec {
        id: C_GONE,
        contract_no: format!("{CONTRACT_PREFIX}GONE"),
        name: "M4-21 采购合同（已删）",
        contract_type: "PURCHASE",
        party_b_type: "SUPPLIER",
        party_b: fx.supplier,
        amount: "100.00",
        status: "ACTIVE",
        deleted: true,
    })
    .await;
}

// ============================================================================
// 断言小工具
// ============================================================================

/// 建单类出口的统一断言：期望 400 / 409 + 指定错误码 + 报文含某段话。
fn expect_reject<T: std::fmt::Debug>(
    result: std::result::Result<T, ApiError>,
    status: u16,
    code: i32,
    contains: &str,
) -> ApiError {
    match result {
        Ok(value) => panic!("期望被拒（{status}/{code}，含「{contains}」），实际成功：{value:?}"),
        Err(error) => {
            assert_eq!(
                error.status, status,
                "状态码不对：{} / {} —— {}",
                error.status, error.code, error.msg
            );
            assert_eq!(error.code, code, "错误码不对：{} —— {}", error.code, error.msg);
            assert!(
                error.msg.contains(contains),
                "报文没点出关键原因（期望含「{contains}」）：{}",
                error.msg
            );
            error
        }
    }
}

/// 只校验状态码 + 报文（用于 `param_invalid` 这类不关心具体码的组合闸）。
fn expect_reject_msg<T: std::fmt::Debug>(
    result: std::result::Result<T, ApiError>,
    status: u16,
    contains: &str,
) -> ApiError {
    match result {
        Ok(value) => panic!("期望被拒（{status}，含「{contains}」），实际成功：{value:?}"),
        Err(error) => {
            assert_eq!(
                error.status, status,
                "状态码不对：{} / {} —— {}",
                error.status, error.code, error.msg
            );
            assert!(
                error.msg.contains(contains),
                "报文没点出关键原因（期望含「{contains}」）：{}",
                error.msg
            );
            error
        }
    }
}

// ============================================================================
// 用例 ①：框架协议全链 —— 建 → 审 → 下单 → 各类超限 → 释放 → 头级上限
// ============================================================================

/// 协议从草稿到「下满为止」的完整路线，**把六种拒绝夹在链中间**：
///
/// ```text
/// ① 建协议（A 行上限 100 / B 行上限 1000 / 单头总量 130 / 单头总额 1980 不含税）
/// ② 反证：DRAFT 协议下单 → 400 / 66096（草稿能下单 = 没有审批）—— 且不许落单
/// ③ 提交 → FWA_APPROVE → 采购经理 → 财务经理 两节点真批 → 协议 APPROVED
/// ④ PO1（A 60@10.5 容差内 + B 2@50）→ 协议引用落库 + 行 id 服务端现查 + 执行量涨
/// ⑤ PO2（A 40@10 = 把 A 行下满）→ 行状态 DONE / 剩余 0
/// ⑥ 反证：A 再下 1 → 409 / 66098（行数量上限）—— 一行不落
/// ⑦ 反证：A 单价 10.6 > 10×1.05 → 409 / 66097（容差外）—— 一行不落
/// ⑧ 反证：协议外物料 → 400 / 66096 —— 一行不落
/// ⑨ 反证：下单日 2025-12-31（协议未生效）→ 400 / 66096 —— 一行不落
/// ⑩ 反证：换一个供应商挂这份协议 → 400 / 66096 —— 一行不落
/// ⑪ PO8（B 17@50）→ 单头总额**正好 = 1980** 放行（边界不是拒绝）
/// ⑫ 反证：B 1@50 → 409 / 66098（总额超限）—— 一行不落
/// ⑬ 作废 PO2 → A 行已用量回落 60 / 单头回落；再下 A 40 → 又能下满（额度会还回来）
/// ⑭ 第二份协议（只有单头总量 100、无行上限、无总额）→ 恰好下 100 放行、再下 1 拒绝
/// ```
#[tokio::test]
async fn agreement_chain_with_caps_and_release() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let fx = fixture().await;

    // ---------- ① 建协议（DRAFT） ----------
    println!("[M4-21] 步 1 建框架协议（A 10 元/上限 100；B 50 元/上限 1000；头 130 量 / 1980 额）");
    let request = agreement_req(
        "CHAIN",
        fx.supplier,
        vec![
            agreement_item(fx.mat_a, "10", Some("100")),
            agreement_item(fx.mat_b, "50", Some("1000")),
        ],
        Some("130"),
        Some("1980"),
    );
    let agreement = create_agreement(&fx, &request).await;
    let row = fwa_model::find_agreement(db(), agreement)
        .await
        .expect("查协议")
        .expect("协议必须存在");
    assert!(
        row.agreement_no.starts_with("FWA-") && row.agreement_no.len() == 19,
        "单号必须是 FWA-yyyyMMdd-NNNNNN（19 位），实际 {}",
        row.agreement_no
    );
    assert_eq!(row.status, fwa_model::STATUS_DRAFT, "新建必须是草稿");
    assert_eq!(row.total_executed_qty, Decimal::ZERO, "派生合计初始为 0");
    assert_eq!(row.item_count, 2, "两行物料范围都要落库");
    let detail = fwa_handler::detail_of(agreement).await.expect("协议详情");
    assert!(detail.agreement.editable, "草稿必须 editable（前端据此放按钮）");
    assert_eq!(detail.items.len(), 2);
    let exec0 = agreement_exec(agreement).await;
    assert_eq!(
        exec_line(&exec0, fx.mat_a).exec_state,
        STATE_NOT_STARTED,
        "刚建的协议行必须是「未开工」"
    );
    let baseline_orders = order_count(&fx).await;

    // ---------- ② 反证：草稿协议不能下单 ----------
    println!("[M4-21] 步 2 反证：DRAFT 协议下单必须 400 / 66096");
    expect_reject(
        create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "1", "10")])).await,
        400,
        error_code::ERP_FWA_NOT_APPLICABLE,
        "只有审批通过",
    );
    assert_eq!(
        order_count(&fx).await,
        baseline_orders,
        "被拒的单不许落库（草稿协议下单不能留下半张单）"
    );

    // ---------- ③ 提交 → 两节点真批 ----------
    println!("[M4-21] 步 3 协议提交审批 → 采购经理 → 财务经理 两节点真批");
    let hops = approve_agreement(&fx, agreement).await;
    assert_eq!(
        hops, 2,
        "fw_agreement 流程是「采购经理审批 → 财务经理复核」两跳，实际 {hops}"
    );

    // ---------- ④ 第一张单：容差内单价 + 两行 ----------
    println!("[M4-21] 步 4 下单 PO1：A 60@10.5（= 10×1.05 容差上限）+ B 2@50");
    let po1 = create_po(
        &fx,
        &spot_po(
            &fx,
            Some(agreement),
            vec![po_line(fx.mat_a, "60", "10.5"), po_line(fx.mat_b, "2", "50")],
        ),
    )
    .await
    .expect("审批通过的协议必须能被下单引用");
    let po1_shape = scalar(
        "SELECT order_no || '|' || status || '|' || purchase_type || '|' || \
                COALESCE(framework_agreement_id::text, '') || '|' || \
                COALESCE(framework_agreement_no_snapshot, '') || '|' || \
                total_amount::text || '|' || total_tax::text || '|' || total_with_tax::text \
           FROM biz_purchase_order WHERE id = ?::uuid",
        vec![Value::from(&po1)],
    )
    .await
    .expect("采购单存在");
    let parts: Vec<&str> = po1_shape.split('|').collect();
    assert!(parts[0].starts_with("PO-"), "采购单号必须是 PO- 前缀，实际 {po1_shape}");
    assert_eq!(parts[1], "DRAFT", "新建采购单必须是草稿");
    assert_eq!(parts[2], "SPOT", "挂框架协议的必须是零星采购");
    assert_eq!(parts[3], agreement.to_string(), "协议 id 必须落库（弱引用）");
    assert_eq!(
        parts[4], row.agreement_no,
        "协议号必须落单头快照（协议被软删后还能回溯）"
    );
    assert_eq!(dec(parts[5]), dec("730"), "不含税 = 60×10.5 + 2×50");
    assert_eq!(dec(parts[6]), dec("94.9"), "进项税 = 730×13%");
    assert_eq!(dec(parts[7]), dec("824.9"), "价税合计 = 730 + 94.9");
    // 行上的协议行 id 是**服务端按（协议, 物料）现查**的，客户端不许传
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_purchase_order_item WHERE order_id = ?::uuid \
                AND framework_agreement_item_id IS NOT NULL",
            vec![Value::from(&po1)]
        )
        .await,
        2,
        "两行明细都必须带上协议行 id（执行量按它聚合）"
    );
    assert_eq!(
        count(
            "SELECT count(DISTINCT framework_agreement_item_id) FROM biz_purchase_order_item \
              WHERE order_id = ?::uuid",
            vec![Value::from(&po1)]
        )
        .await,
        2,
        "两行必须指向两个不同的协议行（不是都指第一行）"
    );

    let exec1 = agreement_exec(agreement).await;
    let line_a = exec_line(&exec1, fx.mat_a);
    let line_b = exec_line(&exec1, fx.mat_b);
    assert_eq!(line_a.executed_qty, dec("60"), "A 行已用量必须涨到 60");
    assert_eq!(line_a.booked_qty, dec("60"), "回写快照必须与链现算一致");
    assert_eq!(line_a.booked_drift, Decimal::ZERO, "回写与单据链不许分叉（drift = 0）");
    assert_eq!(line_a.remaining_qty, Some(dec("40")), "A 行剩余 = 100 − 60");
    assert_eq!(line_a.exec_state, STATE_PARTIAL);
    assert_eq!(line_a.order_doc_count, 1, "A 行计入 1 张单");
    assert_eq!(line_b.executed_qty, dec("2"), "B 行已用量 = 2");
    assert_eq!(exec1.agreement.executed_qty, dec("62"), "单头已用量 = 60 + 2");
    assert_eq!(exec1.agreement.executed_amount, dec("730"), "单头已用额 = Σ 不含税");
    assert_eq!(exec1.sources.len(), 2, "下钻来源 = 两张单 × 行（PO1 的 A、B 各一行）");
    assert!(exec1.sources.iter().all(|source| source.counted), "未作废的单必须计入");

    // ---------- ⑤ 第二张单：把 A 行下满 ----------
    println!("[M4-21] 步 5 下单 PO2：A 40@10 → A 行下满（100/100）");
    let po2 = create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "40", "10")]))
        .await
        .expect("行上限内必须放行");
    let exec2 = agreement_exec(agreement).await;
    let line_a = exec_line(&exec2, fx.mat_a);
    assert_eq!(line_a.executed_qty, dec("100"), "A 行已用量 = 100（正好上限）");
    assert_eq!(line_a.remaining_qty, Some(Decimal::ZERO), "剩余 0");
    assert_eq!(line_a.exec_state, STATE_DONE, "== 上限是 DONE，不是 OVER");
    assert_eq!(exec2.agreement.executed_qty, dec("102"));

    // ---------- ⑥~⑩ 五种拒绝（每一种都断言「一行不落」） ----------
    let baseline_orders = order_count(&fx).await;
    println!("[M4-21] 步 6 反证：A 行再下 1 → 409 / 66098（行数量上限）");
    expect_reject(
        create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "1", "10")])).await,
        409,
        error_code::ERP_FWA_QTY_EXCEEDED,
        "数量上限",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "超行上限的单不许落库");

    println!("[M4-21] 步 7 反证：A 单价 10.6（> 10×1.05 容差）→ 409 / 66097");
    expect_reject(
        create_po(
            &fx,
            &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "1", "10.6")]),
        )
        .await,
        409,
        error_code::ERP_FWA_PRICE_EXCEEDED,
        "高于框架协议",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "超协议价的单不许落库");

    println!("[M4-21] 步 8 反证：协议外物料 → 400 / 66096");
    expect_reject(
        create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_c, "1", "10")])).await,
        400,
        error_code::ERP_FWA_NOT_APPLICABLE,
        "不在框架协议",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "协议外物料的单不许落库");

    println!("[M4-21] 步 9 反证：下单日 2025-12-31（协议期外）→ 400 / 66096");
    let mut expired = spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "1", "10")]);
    expired.order_date = Some(date("2025-12-31"));
    expect_reject(
        create_po(&fx, &expired).await,
        400,
        error_code::ERP_FWA_NOT_APPLICABLE,
        "有效期",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "协议期外的单不许落库");

    println!("[M4-21] 步 10 反证：换供应商（{SUP2_CODE}）挂这份协议 → 400 / 66096");
    let mut other_supplier = spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "1", "10")]);
    other_supplier.supplier_id = Some(fx.supplier2.to_string());
    expect_reject(
        create_po(&fx, &other_supplier).await,
        400,
        error_code::ERP_FWA_NOT_APPLICABLE,
        "供应商",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "供应商不符的单不许落库");

    // ---------- ⑪ 单头总额边界（正好等于上限 → 放行） ----------
    println!("[M4-21] 步 11 下单 PO8：B 17@50 → 单头总额正好 = 1980（边界放行）");
    let po8 = create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_b, "17", "50")]))
        .await
        .expect("单头总额正好等于上限必须放行（边界不是拒绝）");
    let exec3 = agreement_exec(agreement).await;
    assert_eq!(
        exec3.agreement.executed_amount,
        dec("1980"),
        "单头已用额 = 730 + 400 + 850"
    );
    assert_eq!(exec3.agreement.executed_qty, dec("119"), "单头已用量 = 62 + 40 + 17");
    let mut counted_orders: Vec<String> = exec3
        .sources
        .iter()
        .filter(|source| source.counted)
        .map(|source| source.order_id.clone())
        .collect();
    counted_orders.sort();
    counted_orders.dedup();
    assert_eq!(counted_orders.len(), 3, "三张未作废的单都要计入（PO1 / PO2 / PO8）");
    assert!(counted_orders.contains(&po1.to_string()));
    assert!(counted_orders.contains(&po2.to_string()));
    assert!(counted_orders.contains(&po8.to_string()));

    // ---------- ⑫ 单头总额超限 ----------
    println!("[M4-21] 步 12 反证：B 1@50 → 409 / 66098（单头总额超限）");
    let baseline_orders = order_count(&fx).await;
    expect_reject(
        create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_b, "1", "50")])).await,
        409,
        error_code::ERP_FWA_QTY_EXCEEDED,
        "总额",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "超总额的单不许落库");

    // ---------- ⑬ 作废释放额度 → 又能下满 ----------
    println!("[M4-21] 步 13 作废 PO2（A 40）→ 额度回落 60/1580 → 再下 A 40 又放行");
    cancel_po(&fx, po2, None).await;
    let exec4 = agreement_exec(agreement).await;
    let line_a = exec_line(&exec4, fx.mat_a);
    assert_eq!(line_a.executed_qty, dec("60"), "作废后 A 行已用量回落 60");
    assert_eq!(line_a.remaining_qty, Some(dec("40")), "剩余回到 40");
    assert_eq!(
        exec4.agreement.executed_amount,
        dec("1580"),
        "作废释放金额：1980 − 400 = 1580"
    );
    let cancelled = exec4
        .sources
        .iter()
        .find(|source| source.order_id == po2.to_string())
        .expect("作废单必须留在下钻里（审计要能看见）");
    assert!(!cancelled.counted, "作废单不许再计入已用量");
    assert!(
        cancelled.verdict.contains("作废"),
        "下钻要说明为什么不计（实际：{}）",
        cancelled.verdict
    );
    let po_after_release = create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "40", "10")]))
        .await
        .expect("释放出来的额度必须能重新下满（额度不是只减不加）");
    let exec5 = agreement_exec(agreement).await;
    assert_eq!(
        exec_line(&exec5, fx.mat_a).executed_qty,
        dec("100"),
        "重新下满后 A 行 = 100"
    );
    assert_eq!(exec5.agreement.executed_amount, dec("1980"), "重新下满后总额回到 1980");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po_after_release)]
        )
        .await
        .as_deref(),
        Some("DRAFT"),
        "释放后重下的单是草稿（额度按「未作废未删除」算，不看审批状态 —— 口径 ④）"
    );

    // ---------- ⑭ 第二份协议：只考「单头总量」 ----------
    println!("[M4-21] 步 14 第二份协议（只有单头总量 100）→ 正 100 放行 / 再 1 拒绝");
    let qty_req = agreement_req(
        "QTYCAP",
        fx.supplier,
        vec![agreement_item(fx.mat_a, "10", None)],
        Some("100"),
        None,
    );
    let qty_agreement = create_agreement(&fx, &qty_req).await;
    approve_agreement(&fx, qty_agreement).await;
    let qty_po = create_po(
        &fx,
        &spot_po(&fx, Some(qty_agreement), vec![po_line(fx.mat_a, "100", "10")]),
    )
    .await
    .expect("单头总量正好等于 100 必须放行（边界不是拒绝）");
    let qty_exec = agreement_exec(qty_agreement).await;
    assert_eq!(qty_exec.agreement.executed_qty, dec("100"), "单头已用量 = 100");
    assert_eq!(qty_exec.agreement.total_max_qty, Some(dec("100")));
    let line = exec_line(&qty_exec, fx.mat_a);
    // 行本身没有 max_qty ⇒ 行状态是「不限」；单头总量是另一层判据（下面这一票拒绝证明它真的生效）
    assert_eq!(line.max_qty, None, "这一行刻意不设行上限：单头总量才是本用例的判据");
    assert_eq!(
        line.exec_state, STATE_UNLIMITED,
        "行无上限 ⇒ 行状态是「不限」（不是 DONE —— 行级判据根本没有分母）"
    );
    let baseline_orders = order_count(&fx).await;
    expect_reject(
        create_po(
            &fx,
            &spot_po(&fx, Some(qty_agreement), vec![po_line(fx.mat_a, "1", "10")]),
        )
        .await,
        409,
        error_code::ERP_FWA_QTY_EXCEEDED,
        "总量",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "超单头总量的单不许落库");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&qty_po)]
        )
        .await
        .as_deref(),
        Some("DRAFT")
    );

    // ---------- 收口 ----------
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_flow_instance WHERE status = 'PENDING' AND deleted_at IS NULL \
                AND biz_type = 'FWA_APPROVE' AND biz_id IN \
                    (SELECT id FROM biz_framework_agreement WHERE agreement_name LIKE 'M421-%')",
            vec![]
        )
        .await,
        0,
        "本文件的协议不许留下 PENDING 审批实例（死单）"
    );
    println!("[M4-21] 用例① 通过：协议链 14 步（六种拒绝 + 两次边界 + 一次释放）");
}

// ============================================================================
// 用例 ②：协议单 → 收货过账（成本 = 协议价，收货不占额度）
// ============================================================================

/// **协议价是不是真的落成了库存成本、含税额是不是真的落成了应付**：
/// 这是 M4-11b 的 `full_chain_order_raises_execution` 从没走到的地方（它到下单为止），
/// 也是 M4-20 收货链从没走过的地方（它的来源采购单不挂协议）。
///
/// 断言口径：
/// - 库存成本 = **不含税**协议价（60 × 10 = 600）；进项税 78 走 `2221.01`；应付 678；
/// - 采购单 → RECEIVED（已收数量是派生值，没有回写列）；
/// - **协议已用量过账前后一动不动**（收货不是「又下了一次单」，重复计额就是超卖的另一种形态）。
#[tokio::test]
async fn agreement_order_posts_to_stock_and_payable() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let fx = fixture().await;

    println!("[M4-21] 步 1 建协议（A 10 元 / 不限量）→ 提交 → 两节点真批");
    let request = agreement_req(
        "POST",
        fx.supplier,
        vec![agreement_item(fx.mat_a, "10", None)],
        None,
        None,
    );
    let agreement = create_agreement(&fx, &request).await;
    approve_agreement(&fx, agreement).await;

    println!("[M4-21] 步 2 下单 PO（A 60@10 = 600）→ 提交审批 → po_approve 真批");
    let po = create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "60", "10")]))
        .await
        .expect("审批通过的协议必须能被下单引用");
    approve_po(&fx, po).await;
    let exec_before = agreement_exec(agreement).await;
    assert_eq!(
        exec_before.agreement.executed_qty,
        dec("60"),
        "下单后协议已用量 = 60（拒收前）"
    );

    println!("[M4-21] 步 3 建收货单（items 留空 = 服务端带出未收完的行）→ DRAFT，此刻不动账");
    let in_id = create_stock_in_from_po(&fx, po).await;
    assert_eq!(
        stock_qty(fx.mat_a).await,
        Decimal::ZERO,
        "收货单是草稿时库存必须一分未加（草稿不算已收）"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_payable WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
            vec![Value::from(&in_id)]
        )
        .await,
        0,
        "建单不生成应付"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
            vec![Value::from(&in_id)]
        )
        .await,
        0,
        "建单不生成凭证"
    );

    println!("[M4-21] 步 4 过账 → 三本账 / 凭证 / 应付 / 采购单状态");
    post_stock_in(&fx, in_id).await;
    assert_eq!(stock_qty(fx.mat_a).await, dec("60"), "成本账数量 = 收货量");
    assert_eq!(
        stock_amount(fx.mat_a).await,
        dec("600"),
        "成本账金额 = **不含税**协议价（60 × 10；不是价税合计 678）"
    );
    assert_eq!(
        voucher_debit_of(in_id, "1405").await,
        dec("600"),
        "借 1405 存货 = 不含税金额"
    );
    assert_eq!(
        voucher_debit_of(in_id, "2221.01").await,
        dec("78"),
        "借 2221.01 进项税 = 600 × 13%"
    );
    assert_eq!(
        voucher_credit_of(in_id, "2202").await,
        dec("678"),
        "贷 2202 应付 = 价税合计"
    );
    let debit = dec_of(
        "SELECT COALESCE(SUM(e.debit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await;
    let credit = dec_of(
        "SELECT COALESCE(SUM(e.credit_amount), 0)::text FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_IN' AND v.source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await;
    assert_eq!(debit, credit, "借贷必须平衡（借 {debit} / 贷 {credit}）");
    assert_eq!(debit, dec("678"), "凭证借方合计 = 价税合计");
    let payable = scalar(
        "SELECT amount::text || '|' || status FROM biz_payable \
          WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
        vec![Value::from(&in_id)],
    )
    .await
    .expect("过账必须生成应付");
    let payable_parts: Vec<&str> = payable.split('|').collect();
    assert_eq!(dec(payable_parts[0]), dec("678"), "应付金额 = 价税合计");
    assert_eq!(payable_parts[1], "OPEN", "应付初始状态 OPEN");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po)]
        )
        .await
        .as_deref(),
        Some("RECEIVED"),
        "全额收完的采购单必须推到 RECEIVED"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_inventory_txn WHERE material_id = ?::uuid \
                AND biz_type = 'PURCHASE_IN' AND txn_type = 'IN'",
            vec![Value::from(&fx.mat_a)]
        )
        .await,
        1,
        "PURCHASE_IN 流水必须有一条"
    );

    println!("[M4-21] 步 5 收口：协议已用量过账前后一动不动（收货不占额度）");
    let exec_after = agreement_exec(agreement).await;
    assert_eq!(
        exec_after.agreement.executed_qty,
        dec("60"),
        "收货过账**不许**改变协议已用量（口径 ③：已用量只来自采购单）"
    );
    assert_eq!(
        exec_after.agreement.executed_amount,
        dec("600"),
        "已用额也不许因为收货而翻倍"
    );
    assert_eq!(
        exec_line(&exec_after, fx.mat_a).booked_drift,
        Decimal::ZERO,
        "收货后回写快照与链现算仍须一致"
    );
    assert_eq!(
        exec_after.sources.len(),
        1,
        "下钻来源只有那张采购单（收货单不算协议用量来源）"
    );
    println!("[M4-21] 用例② 通过：协议价落成成本 600 / 进项税 78 / 应付 678，额度未重复计");
}

// ============================================================================
// 用例 ③：合同采购全链 —— 合同额度边界 + 超限 + 作废释放 + 收货不改执行量
// ============================================================================

/// 合同采购路线：**金额不超合同**（M4-18 守卫）+ **执行量只算一次**（M4-19 报表）。
///
/// 合同夹具额度刻意取 67.80（= 三张 22.60 的单），让「正好等于合同额」这个边界可以真的撞上。
#[tokio::test]
async fn contract_linked_chain_with_amount_cap() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let fx = fixture().await;
    seed_contracts(&fx).await;
    let contract = Uuid::parse_str(C_PUR).expect("合同夹具 id");

    println!("[M4-21] 步 1 合同采购单（10 × 2.00 + 13% = 22.60，供应商从乙方派生）");
    let po1 = create_po(&fx, &contract_po(&fx, contract, "10", "2"))
        .await
        .expect("合同额度内的单必须能建");
    let shape = scalar(
        "SELECT purchase_type || '|' || COALESCE(contract_id::text, '') || '|' || \
                COALESCE(contract_no_snapshot, '') || '|' || COALESCE(framework_agreement_id::text, '') || '|' || \
                supplier_id::text || '|' || total_with_tax::text || '|' || status \
           FROM biz_purchase_order WHERE id = ?::uuid",
        vec![Value::from(&po1)],
    )
    .await
    .expect("采购单存在");
    let parts: Vec<&str> = shape.split('|').collect();
    assert_eq!(parts[0], "CONTRACT", "采购类型必须落库为 CONTRACT");
    assert_eq!(parts[1], contract.to_string(), "contract_id 必须落库");
    assert_eq!(
        parts[2],
        format!("{CONTRACT_PREFIX}PUR"),
        "合同号必须落单头快照（合同改名后历史单仍显示当时那份）"
    );
    assert_eq!(parts[3], "", "合同采购**不许**同时挂框架协议（prepare 已拦）");
    assert_eq!(parts[4], fx.supplier.to_string(), "供应商必须从合同乙方派生");
    assert_eq!(dec(parts[5]), dec(ORDER_WITH_TAX), "价税合计 = 22.60");
    assert_eq!(parts[6], "DRAFT", "新建必须草稿");

    let exec1 = contract_exec_detail(contract).await;
    assert_eq!(exec1.contract.amount, dec("67.80"), "合同额来自夹具");
    assert_eq!(exec1.contract.executed_amount, dec(ORDER_WITH_TAX), "已执行 = 22.60");
    assert_eq!(exec1.contract.remaining_amount, dec("45.20"), "剩余 = 67.80 − 22.60");
    assert_eq!(exec1.contract.order_doc_count, 1);
    assert_eq!(exec1.contract.exec_state, "PARTIAL");
    assert!(exec1.sources.iter().all(|source| source.counted));

    println!("[M4-21] 步 2 第二张单（20 × 2.00 = 45.20）→ 累计正好 = 合同额 67.80（边界放行）");
    let po2 = create_po(&fx, &contract_po(&fx, contract, "20", "2"))
        .await
        .expect("累计正好等于合同额必须放行（边界不是拒绝）");
    let exec2 = contract_exec_detail(contract).await;
    assert_eq!(exec2.contract.executed_amount, dec("67.80"), "已执行 = 合同额");
    assert_eq!(exec2.contract.remaining_amount, Decimal::ZERO, "剩余 0");
    assert_eq!(exec2.contract.exec_state, "DONE", "== 合同额是 DONE，不是 OVER");
    assert_eq!(exec2.contract.order_doc_count, 2);

    println!("[M4-21] 步 3 反证：再下 1 件（+2.26）→ 409 / 66019，且不落库");
    let baseline_orders = order_count(&fx).await;
    expect_reject(
        create_po(&fx, &contract_po(&fx, contract, "1", "2")).await,
        409,
        error_code::ERP_CONTRACT_AMOUNT_EXCEEDED,
        "额度不足",
    );
    assert_eq!(order_count(&fx).await, baseline_orders, "超合同额的单不许落库");
    assert_eq!(
        contract_exec_detail(contract).await.contract.executed_amount,
        dec("67.80"),
        "被拒的单不许影响已执行额"
    );

    println!("[M4-21] 步 4 作废 PO2 → 额度释放（已执行 22.60 / 剩余 45.20），下钻标「不计入」");
    cancel_po(&fx, po2, None).await;
    let exec3 = contract_exec_detail(contract).await;
    assert_eq!(exec3.contract.executed_amount, dec(ORDER_WITH_TAX), "作废后已执行回落");
    assert_eq!(exec3.contract.remaining_amount, dec("45.20"), "剩余回到 45.20");
    assert_eq!(exec3.contract.order_doc_count, 1);
    assert_eq!(exec3.contract.released_doc_count, 1, "释放（作废）单据数 = 1");
    let cancelled = exec3
        .sources
        .iter()
        .find(|source| source.order_id == po2.to_string())
        .expect("作废单必须留在下钻里");
    assert!(!cancelled.counted, "作废单不许计入已执行额");
    assert!(
        cancelled.verdict.contains("作废"),
        "下钻要说明为什么不计（实际：{}）",
        cancelled.verdict
    );

    println!("[M4-21] 步 5 释放后重下 1 件 → 放行（额度会还回来）");
    let po4 = create_po(&fx, &contract_po(&fx, contract, "1", "2"))
        .await
        .expect("释放出来的额度必须能再用");
    let exec4 = contract_exec_detail(contract).await;
    assert_eq!(exec4.contract.executed_amount, dec("24.86"), "已执行 = 22.60 + 2.26");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&po4)]
        )
        .await
        .as_deref(),
        Some("DRAFT")
    );

    println!("[M4-21] 步 6 收货过账 PO1（10 × 2.00）→ 库存 / 凭证 / 应付");
    approve_po(&fx, po1).await;
    let in_id = create_stock_in_from_po(&fx, po1).await;
    post_stock_in(&fx, in_id).await;
    assert_eq!(stock_qty(fx.mat_a).await, dec("10"), "成本账数量 = 10");
    assert_eq!(stock_amount(fx.mat_a).await, dec("20"), "成本账金额 = 不含税 20");
    assert_eq!(voucher_debit_of(in_id, "1405").await, dec("20"));
    assert_eq!(voucher_debit_of(in_id, "2221.01").await, dec("2.6"));
    assert_eq!(voucher_credit_of(in_id, "2202").await, dec("22.6"));

    println!("[M4-21] 步 7 收口：收货不改合同执行量（合同看的是下单额，不是入库额）");
    let exec5 = contract_exec_detail(contract).await;
    assert_eq!(
        exec5.contract.executed_amount,
        dec("24.86"),
        "收货过账**不许**改变合同已执行额（口径 ③）"
    );
    assert_eq!(exec5.contract.order_doc_count, 2, "计入的单仍是 PO1 + PO4");

    println!("[M4-21] 步 8 反证：已收货的订单不许从状态入口直接作废（要联动作废）→ 409");
    let error = purchase_handler::change_purchase_order_status(
        po1,
        &PurchaseOrderStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: Some("M4-21 反证".to_string()),
            ..PurchaseOrderStatusReq::default()
        },
        Some(fx.maker),
    )
    .await
    .expect_err("已收货的订单直接作废必须被拒（R-P1-5）");
    assert_eq!(error.status, 409, "状态码：{} —— {}", error.status, error.msg);
    assert!(
        error.msg.contains("收过货") || error.msg.contains("影响面"),
        "报文必须把用户导向联动作废入口：{}",
        error.msg
    );
    println!("[M4-21] 用例③ 通过：合同链 8 步（边界 / 超限 / 释放 / 收货不改执行量 / 联动作废闸）");
}

// ============================================================================
// 用例 ④：两条路线的互斥 + 各种「引用不可用」
// ============================================================================

/// 采购单只有两条带定价约束的路线，**互斥**；引用不可用时必须在建单口就拒（不许落半张单）。
///
/// 这一条钉的是「组合矩阵」：4 种类型/外键组合 + 4 种不可用引用，一共 10 个 400。
#[tokio::test]
async fn route_exclusivity_and_unusable_references() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let fx = fixture().await;
    seed_contracts(&fx).await;

    println!("[M4-21] 步 0 造三份协议：草稿 / 审批中 / 已软删");
    let draft = create_agreement(
        &fx,
        &agreement_req(
            "GUARD-DRAFT",
            fx.supplier,
            vec![agreement_item(fx.mat_a, "10", None)],
            None,
            None,
        ),
    )
    .await;
    let submitted = create_agreement(
        &fx,
        &agreement_req(
            "GUARD-SUBMITTED",
            fx.supplier,
            vec![agreement_item(fx.mat_a, "10", None)],
            None,
            None,
        ),
    )
    .await;
    fwa_handler::submit_agreement(
        submitted,
        &SubmitAgreementReq::default(),
        Some(fx.maker),
        &ApprovalGate::none(),
    )
    .await
    .expect("提交审批（只为造出 SUBMITTED 态）");
    assert_eq!(agreement_status(submitted).await.as_deref(), Some("SUBMITTED"));
    let gone = create_agreement(
        &fx,
        &agreement_req(
            "GUARD-GONE",
            fx.supplier,
            vec![agreement_item(fx.mat_a, "10", None)],
            None,
            None,
        ),
    )
    .await;
    fwa_handler::delete_agreement(gone, Some(fx.maker))
        .await
        .expect("草稿协议可以软删");
    assert_eq!(agreement_status(gone).await, None, "软删后查不到");

    let baseline_orders = order_count(&fx).await;
    let contract = Uuid::parse_str(C_PUR).expect("合同夹具 id");
    let draft_contract = Uuid::parse_str(C_DRAFT).expect("草稿合同 id");
    let sales_contract = Uuid::parse_str(C_SALES).expect("销售合同 id");
    let gone_contract = Uuid::parse_str(C_GONE).expect("已删合同 id");

    // ① CONTRACT + 框架协议 → 400（两个价格上限说不清以谁为准）
    println!("[M4-21] 步 1 CONTRACT + 框架协议 → 400");
    let mut both = contract_po(&fx, contract, "1", "2");
    both.framework_agreement_id = Some(draft.to_string());
    expect_reject_msg(create_po(&fx, &both).await, 400, "不能同时挂框架协议");

    // ② SPOT + contract_id → 400
    println!("[M4-21] 步 2 SPOT + contract_id → 400");
    let mut spot_with_contract = spot_po(&fx, None, vec![po_line(fx.mat_a, "1", "10")]);
    spot_with_contract.contract_id = Some(contract.to_string());
    expect_reject_msg(create_po(&fx, &spot_with_contract).await, 400, "不能给 contract_id");

    // ③ CONTRACT 不给 contract_id → 400
    println!("[M4-21] 步 3 CONTRACT 不给 contract_id → 400");
    let mut no_contract = contract_po(&fx, contract, "1", "2");
    no_contract.contract_id = None;
    expect_reject_msg(create_po(&fx, &no_contract).await, 400, "必须给 contract_id");

    // ④ CONTRACT + 手填一个不是乙方的供应商 → 400
    println!("[M4-21] 步 4 CONTRACT + 供应商与合同乙方不一致 → 400");
    let mut mismatch = contract_po(&fx, contract, "1", "2");
    mismatch.supplier_id = Some(fx.supplier2.to_string());
    expect_reject_msg(create_po(&fx, &mismatch).await, 400, "从合同乙方带出");

    // ⑤ 草稿状态的合同 → 400 / 66014
    println!("[M4-21] 步 5 草稿合同下单 → 400 / 66014");
    expect_reject(
        create_po(&fx, &contract_po(&fx, draft_contract, "1", "2")).await,
        400,
        error_code::ERP_CONTRACT_NOT_USABLE,
        "只有已签署",
    );

    // ⑥ 销售合同拿来采购 → 400 / 66014
    println!("[M4-21] 步 6 销售合同用于采购下单 → 400 / 66014");
    expect_reject(
        create_po(&fx, &contract_po(&fx, sales_contract, "1", "2")).await,
        400,
        error_code::ERP_CONTRACT_NOT_USABLE,
        "只接受 PURCHASE",
    );

    // ⑦ 已软删的合同 → 400 / 66002
    println!("[M4-21] 步 7 已软删合同下单 → 400 / 66002");
    expect_reject(
        create_po(&fx, &contract_po(&fx, gone_contract, "1", "2")).await,
        400,
        error_code::ERP_REFERENCE_MISSING,
        "不存在或已删除",
    );

    // ⑧ 草稿协议 → 400 / 66096
    println!("[M4-21] 步 8 草稿协议下单 → 400 / 66096");
    expect_reject(
        create_po(&fx, &spot_po(&fx, Some(draft), vec![po_line(fx.mat_a, "1", "10")])).await,
        400,
        error_code::ERP_FWA_NOT_APPLICABLE,
        "只有审批通过",
    );

    // ⑨ 审批中的协议 → 400 / 66096
    println!("[M4-21] 步 9 审批中（SUBMITTED）的协议下单 → 400 / 66096");
    expect_reject(
        create_po(&fx, &spot_po(&fx, Some(submitted), vec![po_line(fx.mat_a, "1", "10")])).await,
        400,
        error_code::ERP_FWA_NOT_APPLICABLE,
        "只有审批通过",
    );

    // ⑩ 已软删的协议 → 400 / 66096
    println!("[M4-21] 步 10 已软删协议下单 → 400 / 66096");
    expect_reject(
        create_po(&fx, &spot_po(&fx, Some(gone), vec![po_line(fx.mat_a, "1", "10")])).await,
        400,
        error_code::ERP_FWA_NOT_APPLICABLE,
        "不存在或已删除",
    );

    // 收口：十次拒绝一行都不许落库
    assert_eq!(
        order_count(&fx).await,
        baseline_orders,
        "十次被拒的建单尝试都不许留下采购单（每次拒绝都要回滚干净）"
    );
    println!("[M4-21] 用例④ 通过：10 条组合闸全部 400 且零残留");
}

// ============================================================================
// 用例 ⑤：清场自检
// ============================================================================

/// 本文件的夹具 0 残留（含审批实例 / 待办 / 凭证 / 应付 / 三本账 / 合同）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let fx = fixture().await;
    seed_contracts(&fx).await;
    // 先真跑半条链造痕迹（否则「清场为 0」是空表上的一句话）
    let agreement = create_agreement(
        &fx,
        &agreement_req(
            "TEARDOWN",
            fx.supplier,
            vec![agreement_item(fx.mat_a, "10", None)],
            None,
            None,
        ),
    )
    .await;
    approve_agreement(&fx, agreement).await;
    let po = create_po(&fx, &spot_po(&fx, Some(agreement), vec![po_line(fx.mat_a, "5", "10")]))
        .await
        .expect("下单");
    approve_po(&fx, po).await;
    let in_id = create_stock_in_from_po(&fx, po).await;
    post_stock_in(&fx, in_id).await;
    assert!(
        count(
            "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid",
            vec![Value::from(&in_id)]
        )
        .await
            > 0,
        "清场前必须真的造出痕迹（凭证）"
    );

    cleanup().await;

    let leftover = |label: &str, query: &str| (label.to_string(), query.to_string());
    let checks = vec![
        leftover(
            "协议",
            &format!("SELECT count(*) FROM biz_framework_agreement WHERE agreement_name LIKE '{NAME_PREFIX}%'"),
        ),
        leftover(
            "协议明细",
            "SELECT count(*) FROM biz_framework_agreement_item i \
              WHERE NOT EXISTS (SELECT 1 FROM biz_framework_agreement a WHERE a.id = i.agreement_id)",
        ),
        leftover(
            "采购单",
            &format!(
                "SELECT count(*) FROM biz_purchase_order WHERE supplier_id IN \
                    (SELECT id FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}'))"
            ),
        ),
        leftover(
            "收货单",
            &format!(
                "SELECT count(*) FROM biz_stock_in WHERE supplier_id IN \
                    (SELECT id FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}'))"
            ),
        ),
        leftover(
            "审批实例",
            &format!(
                "SELECT count(*) FROM biz_flow_instance WHERE biz_type IN ('FWA_APPROVE', 'PO_APPROVE') \
                    AND biz_id IN (SELECT id FROM biz_purchase_order WHERE order_no LIKE '%' AND remark LIKE '{NAME_PREFIX}%')"
            ),
        ),
        leftover(
            "凭证",
            &format!(
                "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_IN' AND source_id IN \
                    (SELECT id FROM biz_stock_in WHERE supplier_id IN \
                        (SELECT id FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}')))"
            ),
        ),
        leftover(
            "应付",
            &format!(
                "SELECT count(*) FROM biz_payable WHERE source_id IN \
                    (SELECT id FROM biz_stock_in WHERE supplier_id IN \
                        (SELECT id FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}')))"
            ),
        ),
        leftover(
            "合同",
            &format!("SELECT count(*) FROM biz_contract WHERE contract_no LIKE '{CONTRACT_PREFIX}%'"),
        ),
        leftover(
            "三本账",
            &format!(
                "SELECT (SELECT count(*) FROM biz_inventory WHERE material_id IN \
                     (SELECT id FROM biz_material WHERE material_code LIKE '{NAME_PREFIX}%')) + \
                        (SELECT count(*) FROM biz_inventory_location WHERE material_id IN \
                     (SELECT id FROM biz_material WHERE material_code LIKE '{NAME_PREFIX}%')) + \
                        (SELECT count(*) FROM biz_inventory_txn WHERE material_id IN \
                     (SELECT id FROM biz_material WHERE material_code LIKE '{NAME_PREFIX}%'))"
            ),
        ),
        leftover(
            "主数据",
            &format!(
                "SELECT (SELECT count(*) FROM biz_material WHERE material_code LIKE '{NAME_PREFIX}%') + \
                        (SELECT count(*) FROM biz_warehouse WHERE warehouse_code = '{WH_CODE}') + \
                        (SELECT count(*) FROM biz_supplier WHERE supplier_code IN ('{SUP_CODE}', '{SUP2_CODE}')) + \
                        (SELECT count(*) FROM biz_customer WHERE code = '{CUS_CODE}')"
            ),
        ),
        leftover(
            "夹具账号",
            "SELECT count(*) FROM sys_user WHERE username IN ('m421_maker', 'm421_keeper', 'm421_poster')",
        ),
    ];
    for (label, query) in checks {
        assert_eq!(count(&query, vec![]).await, 0, "清场后 {label} 必须 0 残留");
    }
    println!("[M4-21] 用例⑤ 通过：11 类痕迹零残留");
}

// ============================================================================
// sales_chain_e2e_tests.rs —— 销售缺口 E2E：报价单 → 销售订单 → 出库（M3-24）
//
// 卡：t_367777a2（M3-24【M3·销售】销售缺口 E2E：报价单 → 订单 → 出库）
//
// # 这一份钉的是什么（一句话）
//
// **一张报价单怎么一路变成客户签收的销售出库单** —— 全链一次跑完、每步都有断言、
// 每步都从库/回执取事实。前面几张卡各自钉死了自己那一段（M3-05 状态机 / M3-08 转单 /
// M2-02 发货过账 / M2-03 签收确认），但**没有一条用例把它们串起来**：
// 上游卡里「订单」要么是直插 SQL 的 `APPROVED` 夹具（`sales_approval_bridge` /
// `shipment_receipt_tests`），要么停在 `DRAFT`（`quotation_convert_tests`）。
// 于是「报价单转出来的那张订单，能不能真的走完审批、发货、出库、挂账」这件事，
// 在库里从来没有被验证过。这就是本卡要补的缺口。
//
// # 链路（9 步 · 断言点即卡面「全链」的落点）
//
// ```text
//  ① 建报价单（DRAFT，QT-yyyyMMdd-nnnnnn，2 行 / 13% 税）       金额恒等自检
//  ② 提交审批 → 流程 quotation_approve → 逐节点审批              审批通过**自动发出**
//     └ 反证：SENT 直接转单 409（只有 ACCEPTED 能转）
//  ③ 客户接受（SENT → ACCEPTED）
//  ④ 转销售订单（带行 / 回链 / 备注来源 / 轨迹 CONVERT）          新订单 = DRAFT
//     └ 反证：DRAFT 订单不能开发货单（409，审批前不许发货）
//  ⑤ 订单提交审批 → so_approve → 逐节点审批 → APPROVED            审批是发货前置
//  ⑥ 建发货单（items 留空 = 从订单带出未发完的行）→ DRAFT
//  ⑦ 提交 → PENDING_POST；过账 → POSTED                          在途账 + 出库待确认
//     └ 反证：此刻库存**一分未扣**、无凭证、无应收（收入成本不早确认）
//  ⑧ 签收确认（confirm_receipt）→ 出库单 POSTED                    三本账负向 + 成本凭证 + 挂应收
//  ⑨ 收口：重复转单幂等回原单、已 OUTBOUND 不能再发货、无 PENDING 实例、无孤儿
// ```
//
// # 与其他用例的分工（不重测）
//
// ```text
// quotation_state_tests        ← 报价单状态机 / 审批闸 / 已转单不许作废（单段）
// quotation_convert_tests      ← 转单入参闸 / 行级复制 / 原子性 / 幂等（单段）
// sales_approval_bridge::tests ← 订单三档金额审批（单段，订单是 SQL 夹具）
// shipment_receipt_tests       ← 发货过账 / 签收确认（单段，订单是 SQL 夹具）
// 本文件（M3-24）               ← **一张单从报价走到签收**：上游夹具全部走真实入口，
//                                每段的「交接处」是本文件唯一关心的东西
// ```
//
// # 为什么审批不走 SQL 夹具（这是本卡的关键决定）
//
// 上游单测用 SQL 造 `APPROVED` 实例是**对的**（它们验的是「闸对实例状态怎么反应」）。
// 但 E2E 要回答的是「这张报价单转出来的订单**能不能**被批」—— 那必须真发起流程、
// 真派待办、真逐条点同意、真走 `doc_reader::dispatch_settled` 的业务桥回调，
// 与 HTTP `POST /business/flow/task/{id}/approve` 的执行链**逐行同款**
// （见 `business/flow/handler.rs::approve_task`）：
//
// ```text
// guard_self_approval_for_task → guard_approve_for_task → engine::approve(tx)
//   → dispatch_settled（ERP 16 条审批线按 biz_type 扇出）
// ```
//
// 审批人**不由用例指定**：节点是 `assignee_type='ROLE'`，待办由「角色在职成员」展开，
// 所以用例逐个取库给的 `assignee_id` 去点（与 `sales_approval_bridge` 同口径），
// 既能在空库（只有夹具审批人）也能在种子库（多个角色成员）上成立。
//
// # 本卡实测到的两处口径（写断言前必须先看，别照直觉写）
//
// 1. **发货过账（⑥⑦）不扣库存、不出凭证、不挂应收** —— 这是 M2-02 的既定设计
//    （货出了仓、客户还没签收：既不是「仓里的库存」也不是「已出库」）。
//    所以 ⑦ 的断言是「库存不变 + 在途增加 + 出库单 PENDING_POST + 凭证/应收 0 张」，
//    ⑧ 签收确认之后才是「三本账负向 + 成本结转凭证 + 应收挂账」。
//    把 ⑦ 当 ⑧ 写（在过账后就断言库存减少）是本卡最容易踩的假红。
// 2. **应收金额取「发货明细的含税金额」，不是订单头的 `total_with_tax`** ——
//    `receivable_bridge::taxed_amount_of(&lines)`。两者在「整单一次发完」时相等，
//    但部分发货时不等；本用例断言的是「两者相等」（整单发完的事实），
//    而不是「实现读的是订单头」—— 别把等价当因果。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `M324-` 夹具、同一份 `biz_seq` 号段），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m324_e2e" cargo test --lib \
//   business::erp::sales::sales_chain_e2e_tests -- --test-threads=1
// ```
//
// 也可直接跑 `bash scripts/verify/m324_sales_chain_e2e_drill.sh`
// （克隆演练库 → 补 V0405…V0445 → 跑本文件 + 销售域回归 → 断言 SKIP=0 与夹具零残留）。
// ============================================================================

use nik::prelude::chrono::{Datelike, Duration, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::fixture_hygiene;
use crate::business::erp::sales::quotation_handler::{
    self as quote, ConvertQuotationReq, CreateQuotationReq, QuotationLineReq, QuotationStatusReq,
};
use crate::business::erp::sales::sales_order_handler;
use crate::business::erp::shipment::shipment_handler::{self, CreateShipmentReq, ShipmentStatusReq};
use crate::business::erp::stockin::stockin_model;
use crate::business::flow::{doc_reader, engine};
use crate::test_seed;

/// 真库用例必须串行（共用全局 `db()` 与同一批 `M324-` 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const CUSTOMER_CODE: &str = "M324-CUST";
const WAREHOUSE_CODE: &str = "M324-WH";
const MAT_A_CODE: &str = "M324-M1";
const MAT_B_CODE: &str = "M324-M2";
/// 制单人（提交审批的人）—— 必须是**不属于**审批角色的人，否则会撞制单/审批分离闸。
const MAKER: &str = "m324_maker";
/// 夹具审批人（绑 `SALES_SUPERVISOR`；两条流程的第一跳都是这个角色）。
const APPROVER: &str = "m324_appr";
/// 本模块两张夹具物料（清场 / 三本账 / 在途账共用，按编码取，不写死 UUID）。
const MATS_SQL: &str = "(SELECT id FROM biz_material WHERE material_code IN ('M324-M1', 'M324-M2'))";

// ============================================================================
// 夹具
// ============================================================================

fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>()
        .unwrap_or_else(|error| panic!("定点字面量非法（{text}）：{error}"))
}

async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL") else {
                println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                },
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn sql(statement: &str) {
    db().exec_sql(statement.to_string(), vec![])
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
        .unwrap_or_else(|| panic!("金额/数量读取失败：{query}"))
}

async fn customer() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_CODE, "M3-24 销售全链夹具客户").await
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_CODE, "M3-24 全链夹具仓").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "全链夹具物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "全链夹具物料乙").await
}

/// 制单人 / 审批人：主数据走 `test_seed`（P2-62 门禁：测试代码不许手写主数据 INSERT）。
async fn people() -> (Uuid, Uuid) {
    let maker = test_seed::ensure_user(MAKER, "M3-24 销售制单人", None).await;
    let approver = test_seed::ensure_user(APPROVER, "M3-24 销售审批人", None).await;
    // 两条流程（`quotation_approve` / `so_approve`）第一跳都是 SALES_SUPERVISOR；
    // `bind_role` 幂等（`ON CONFLICT (user_id, role_id) DO NOTHING`），种子里该角色
    // 已有别人时也**只是多一个成员**，待办条数由库决定（用例不假设审批人是谁）。
    test_seed::bind_role(approver, "SALES_SUPERVISOR").await;
    (maker, approver)
}

/// 清场：只清本模块**业务单据**及其痕迹（`M324-CUST` 客户名下的报价 / 订单 / 发货 / 出库）。
///
/// 顺序不可交换：
/// 1. `biz_status_log` 没有外键 ⇒ **先于**单据按 id 清（单据一删就查不到 id，改收不走）；
/// 2. 凭证 / 应收是弱引用（`source_id`）⇒ 在删出库单**之前**清；
/// 3. 审批实例 `biz_id` 也是弱引用，且 `prev_instance_id` 是自引用无 ON DELETE ⇒ 先解开再删；
/// 4. 外键链：`biz_inventory`（FK 指向库位账）→ `biz_inventory_location`；
///    `biz_stock_out` → `biz_shipment` → `biz_sales_order` → `biz_quotation`。
async fn cleanup() {
    let cust = format!("SELECT id FROM biz_customer WHERE code = '{CUSTOMER_CODE}'");
    let quotes = format!("SELECT id FROM biz_quotation WHERE customer_id IN ({cust})");
    let orders = format!("SELECT id FROM biz_sales_order WHERE customer_id IN ({cust})");
    let shipments = format!("(SELECT id FROM biz_shipment WHERE so_id IN ({orders}))");
    let stock_outs = format!("(SELECT id FROM biz_stock_out WHERE source_id IN {shipments})");
    let receivables =
        format!("SELECT id FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id IN {stock_outs}");
    let vouchers = format!("SELECT id FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN {stock_outs}");
    let instances = format!(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type IN ('QUOTATION_APPROVE', 'SO_APPROVE') \
            AND (biz_id IN ({quotes}) OR biz_id IN ({orders}))"
    );
    let statements: Vec<String> = vec![
        format!(
            "DELETE FROM biz_status_log WHERE \
                (entity_type = 'QUOTATION' AND entity_id IN ({quotes})) \
             OR (entity_type = 'SALE_ORDER' AND entity_id IN ({orders})) \
             OR (entity_type = 'SHIPMENT' AND entity_id IN {shipments}) \
             OR (entity_type = 'STOCK_OUT' AND entity_id IN {stock_outs}) \
             OR (entity_type = 'RECEIVABLE' AND entity_id IN ({receivables})) \
             OR (entity_type = 'VOUCHER' AND entity_id IN ({vouchers}))"
        ),
        // 凭证分录 → 凭证（`source_type='STOCK_OUT'`）→ 应收（`SALES_OUT`）
        format!(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
               (SELECT id FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN {stock_outs})"
        ),
        format!("DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN {stock_outs}"),
        format!("DELETE FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id IN {stock_outs}"),
        // 审批痕迹：解自引用 → 待办 / 节点进度 / 抄送 → 实例
        format!("UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE prev_instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_task WHERE instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_instance_node WHERE instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_cc WHERE instance_id IN ({instances})"),
        format!("DELETE FROM biz_flow_instance WHERE id IN ({instances})"),
        // 在途账是硬外键（V0420）但显式删一遍更稳：单一失败点比「靠级联」好定位
        format!(
            "DELETE FROM biz_inventory_in_transit \
              WHERE shipment_id IN {shipments} OR material_id IN {MATS_SQL}"
        ),
        format!("DELETE FROM biz_stock_out WHERE source_id IN {shipments}"),
        format!("DELETE FROM biz_shipment WHERE so_id IN ({orders})"),
        format!("DELETE FROM biz_sales_order WHERE customer_id IN ({cust})"),
        format!("DELETE FROM biz_quotation WHERE customer_id IN ({cust})"),
        format!("DELETE FROM biz_customer WHERE code = '{CUSTOMER_CODE}'"),
        // 三本账：流水 → 成本账（FK 指向库位账）→ 库位账
        format!("DELETE FROM biz_inventory_txn WHERE material_id IN {MATS_SQL}"),
        format!("DELETE FROM biz_inventory WHERE material_id IN {MATS_SQL}"),
        format!("DELETE FROM biz_inventory_location WHERE material_id IN {MATS_SQL}"),
    ];
    for statement in statements {
        sql(&statement).await;
    }
    test_seed::purge_user(MAKER).await;
    test_seed::purge_user(APPROVER).await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    // 主数据先就位（`test_seed` 幂等），再清业务单据。
    customer().await;
    warehouse().await;
    mat_a().await;
    mat_b().await;
    people().await;
    cleanup().await;
    Some(guard)
}

/// 造库存（成本账 + 库位账，走入库侧写入口 `stockin_model`，与收货/签收用例同口径）。
async fn seed_stock(material_id: Uuid, qty: &str, unit_cost: &str) {
    let now = Utc::now();
    let warehouse_id = warehouse().await;
    let quantity = dec(qty);
    stockin_model::apply_location_qty(db(), material_id, warehouse_id, None, quantity, now)
        .await
        .expect("库位账加法（未分配桶）");
    stockin_model::apply_cost(
        db(),
        material_id,
        warehouse_id,
        None,
        stockin_model::CostBasis::of(quantity, quantity * dec(unit_cost)),
        now,
    )
    .await
    .expect("成本账加法");
}

/// 一条报价明细（金额由 `model::line_amounts` 同口径算，和 handler 里的入参闸一致）。
fn quote_line(material_id: Uuid, qty: &str, price: &str, tax_rate: &str) -> QuotationLineReq {
    QuotationLineReq {
        material_id: material_id.to_string(),
        qty: Some(dec(qty)),
        price: Some(dec(price)),
        tax_rate: Some(dec(tax_rate)),
        remark: None,
    }
}

// ============================================================================
// 审批：与 `flow::handler::approve_task` 逐行同款（不直插 APPROVED）
// ============================================================================

/// 当前在跑的待办（按创建顺序）→ `(task_id, node_key, assignee_id)`。
///
/// **必须带上 `assignee_id`**：节点是 `assignee_type='ROLE'`，受派人由「角色在职成员」展开，
/// 库里有几个该角色的人就有几条待办；拿别人去点会被引擎按「非受派人」拒掉。
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

/// 把 ERP 的 18 个审批回调实现注册进**进程级**注册表。
///
/// # 为什么 E2E 必须自己注册（真机不会遇到，`cargo test --lib` 一定会）
///
/// 生产是 `bootstrap` 启动期调 `erp::doc_reader_impl::register_all()`；`cargo test --lib`
/// 不起 bootstrap，进程级注册表是**空的** —— 于是 `dispatch_settled` 一个实现都找不到，
/// 审批通过后业务桥不执行，链路会以「审批 APPROVED 但报价单仍是 DRAFT」这种
/// **看似业务 bug、实则环境缺件**的形态红掉。注册完再断言注册表长度 ≥ 18，
/// 把「注册失败」变成一句明确的断言而不是下游的莫名其妙。
async fn ensure_doc_registry() {
    match crate::business::erp::doc_reader_impl::register_all() {
        Ok(count) => println!("[M3-24] ERP 审批回调实现已注册 {count} 个"),
        // 重复注册 = 本进程里别的用例已经注册过（幂等语义），继续用现成的
        Err(reason) => println!("[M3-24] ERP 审批回调注册表已就绪（{reason}）"),
    }
    let len = doc_reader::len();
    assert!(
        len >= 18,
        "进程级审批回调注册表必须有 18 个 ERP 实现（否则审批通过不会回调业务单据），实际 {len}"
    );
}

/// 逐节点把实例批到终态，返回最终实例状态。
///
/// 循环到没有 `PENDING` 待办为止（不假设节点数 / 会签人数）；每一跳都走与 HTTP 入口
/// 同款的三步：自审闸 → `engine::approve`（事务）→ `dispatch_settled`（业务桥扇出）。
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
            println!("[M3-24]   审批待办：节点 {node_key} / 受派人 {assignee}");
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

// ============================================================================
// 用例：报价单 → 销售订单 → 出库（全链）
// ============================================================================

/// **全链一次跑完**：报价单 → 审批发出 → 客户接受 → 转订单 → 订单审批 → 发货 →
/// 过账（在途）→ 签收确认（出库）→ 挂应收 + 成本结转凭证。
///
/// 9 步、逐步断言；两个「顺序反证」（未接受不能转单 / 未审批不能发货）夹在链中间，
/// 因为它们是同一条链上的闸，脱离链单独跑就证明不了「这条链真的挡得住」。
#[tokio::test]
async fn quotation_to_order_to_outbound_full_chain() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let (maker, _approver) = people().await;
    let customer_id = customer().await;
    let warehouse_id = warehouse().await;
    let (mat_1, mat_2) = (mat_a().await, mat_b().await);
    // 库存：各 100 件 @ 成本 10（出库 10 / 5 件，结转成本 100 / 50）
    seed_stock(mat_1, "100", "10").await;
    seed_stock(mat_2, "100", "10").await;
    // 会计期间（凭证期间闸）：按今天（CST，与单据入口同一口径）的开账月
    let today = shipment_handler::today_cst(Utc::now());
    test_seed::ensure_accounting_period(today.year(), today.month()).await;

    // ---------- ① 建报价单（DRAFT） ----------
    println!("[M3-24] 步 1/9 建报价单（2 行 / 13% 税）");
    let quote = quote::create_quotation(
        Some(maker),
        CreateQuotationReq {
            customer_id: customer_id.to_string(),
            quote_date: None,
            valid_until: (today + Duration::days(14)).to_string(),
            remark: Some("M3-24 全链夹具：报价".to_string()),
            items: vec![
                quote_line(mat_1, "10", "100", "13"),
                quote_line(mat_2, "5", "200", "13"),
            ],
        },
    )
    .await
    .unwrap_or_else(|error| panic!("建报价单应成功：{} {}", error.code, error.msg));
    let quote_id = Uuid::parse_str(&quote.id).expect("报价单 id 是 UUID");
    assert_eq!(quote.status, "DRAFT", "新建报价单必须是草稿");
    assert!(
        quote.quote_no.starts_with("QT-"),
        "报价单号必须是 QT- 前缀，实际 {}",
        quote.quote_no
    );
    assert_eq!(quote.total_amount, dec("2000"), "不含税 = 10×100 + 5×200");
    assert_eq!(quote.total_tax, dec("260"), "税额 = 2000 × 13%");
    assert_eq!(quote.total_with_tax, dec("2260"), "价税合计 = 2000 + 260");
    assert_eq!(quote.items.len(), 2, "两张明细行都落库");

    // ---------- ② 提交审批 → 审批通过 = 自动发出 ----------
    println!("[M3-24] 步 2/9 报价单提交审批（quotation_approve）");
    let submitted = quote::submit_approval(Some(maker), &quote_id.to_string(), &ApprovalGate::none())
        .await
        .unwrap_or_else(|error| {
            panic!(
                "提交审批应成功（库里有已发布的 quotation_approve）：{} {}",
                error.code, error.msg
            )
        });
    assert_eq!(
        submitted.status, "DRAFT",
        "审批在跑时报价单仍是草稿（审批不是状态，见 V0435 口径）"
    );
    let instance_id = scalar(
        "SELECT approval_instance_id::text FROM biz_quotation WHERE id = ?::uuid",
        vec![Value::from(&quote_id)],
    )
    .await
    .and_then(|text| Uuid::parse_str(&text).ok())
    .expect("提交审批后必须挂审批实例");
    let final_status = approve_until_settled(instance_id).await;
    assert_eq!(final_status, "APPROVED", "审批实例必须走到 APPROVED");
    let status = scalar(
        "SELECT status FROM biz_quotation WHERE id = ?::uuid",
        vec![Value::from(&quote_id)],
    )
    .await
    .expect("报价单存在");
    assert_eq!(
        status, "SENT",
        "审批通过回调必须**自动发出**（DRAFT → SENT，不用人再点一次）"
    );

    // ---------- ②b 反证：SENT 不能直接转单 ----------
    println!("[M3-24] 步 3/9 反证：SENT 直接转单必须 409（只有 ACCEPTED 能转）");
    let error = quote::convert_quotation(None, &quote_id.to_string(), ConvertQuotationReq::default())
        .await
        .expect_err("SENT 状态转单必须被拒（客户还没接受）");
    assert_eq!(error.status, 409, "非法状态转单是 409，实际 {}", error.status);
    assert!(
        error.msg.contains("ACCEPTED"),
        "文案要指名叫客户先接受（ACCEPTED），实际：{}",
        error.msg
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_sales_order WHERE customer_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&customer_id)]
        )
        .await,
        0,
        "被拒的转单不许留下任何订单"
    );

    // ---------- ③ 客户接受 ----------
    println!("[M3-24] 步 4/9 客户接受（SENT → ACCEPTED）");
    let accepted = quote::change_quotation_status(
        Some(maker),
        &quote_id.to_string(),
        QuotationStatusReq {
            target_status: "ACCEPTED".to_string(),
            reason: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("客户接受应成功：{} {}", error.code, error.msg));
    assert_eq!(accepted.status, "ACCEPTED");

    // ---------- ④ 转销售订单 ----------
    println!("[M3-24] 步 5/9 转销售订单（带行 / 回链 / 备注来源 / 轨迹 CONVERT）");
    let converted = quote::convert_quotation(
        Some(maker),
        &quote_id.to_string(),
        ConvertQuotationReq {
            remark: Some("M3-24 全链：客户已接受".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|error| panic!("转单应成功：{} {}", error.code, error.msg));
    let order_id = Uuid::parse_str(&converted.header.so_id).expect("订单 id 是 UUID");
    assert_eq!(
        converted.header.status, "DRAFT",
        "转单只建草稿订单（提交/信用/出库走订单自己的流程）"
    );
    assert!(converted.header.so_no.starts_with("SO-"), "订单号必须是 SO- 前缀");
    assert_eq!(converted.header.customer_id, customer_id.to_string(), "客户沿用报价单");
    assert_eq!(converted.header.total_amount, dec("2000"), "金额整列搬，不重新报价");
    assert_eq!(converted.header.total_with_tax, dec("2260"));
    assert_eq!(converted.items.len(), 2, "明细逐行搬");
    assert_eq!(
        converted.meta.extra.get("already_converted"),
        Some(&serde_json::json!(false)),
        "首次转单不是幂等命中"
    );
    // 回链 + 备注 + 轨迹（三处都要，少一处就断链）
    let back_link = scalar(
        "SELECT converted_so_id::text || '/' || COALESCE(converted_at IS NOT NULL, false)::text \
           FROM biz_quotation WHERE id = ?::uuid",
        vec![Value::from(&quote_id)],
    )
    .await
    .expect("报价单存在");
    assert_eq!(
        back_link,
        format!("{order_id}/true"),
        "报价单必须回填 converted_so_id + converted_at"
    );
    let order_remark = scalar(
        "SELECT COALESCE(remark, '') FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("订单存在");
    assert!(
        order_remark.contains(&quote.quote_no) && order_remark.contains("M3-24 全链"),
        "订单备注必须同时带来源单号与人工备注，实际：{order_remark}"
    );
    let convert_log = count(
        "SELECT count(*) FROM biz_status_log \
          WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid AND action = 'CONVERT'",
        vec![Value::from(&quote_id)],
    )
    .await;
    assert_eq!(convert_log, 1, "转单必须留一条 action=CONVERT 的轨迹");

    // ---------- ④b 反证：草稿订单不能发货 ----------
    println!("[M3-24] 步 6/9 反证：DRAFT 订单开发货单必须 409（审批是发货前置）");
    let error = shipment_handler::create_shipment(
        Some(maker),
        CreateShipmentReq {
            so_id: order_id.to_string(),
            warehouse_id: warehouse_id.to_string(),
            ship_date: None,
            remark: None,
            items: vec![],
        },
    )
    .await
    .expect_err("未审批的订单不许发货");
    assert_eq!(error.status, 409, "订单状态闸是 409，实际 {}", error.status);
    assert!(
        error.msg.contains("能发货") || error.msg.contains("DRAFT"),
        "文案要说明「未审批的订单不能发货」，实际：{}",
        error.msg
    );

    // ---------- ⑤ 订单审批（so_approve） ----------
    println!("[M3-24] 步 7/9 订单提交审批 → APPROVED");
    let submitted = sales_order_handler::submit_approval(Some(maker), &order_id.to_string(), &ApprovalGate::none())
        .await
        .unwrap_or_else(|error| panic!("订单提交审批应成功：{} {}", error.code, error.msg));
    assert_eq!(submitted.status, "SUBMITTED");
    let order_instance = scalar(
        "SELECT approval_instance_id::text FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .and_then(|text| Uuid::parse_str(&text).ok())
    .expect("订单提交后必须挂审批实例");
    let final_status = approve_until_settled(order_instance).await;
    assert_eq!(final_status, "APPROVED");
    let order_status = scalar(
        "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("订单存在");
    assert_eq!(order_status, "APPROVED", "审批通过后订单必须是 APPROVED");
    assert!(
        scalar(
            "SELECT approval_instance_id::text FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)]
        )
        .await
        .is_some(),
        "APPROVED 后实例 id 保留作审计（详情页要能显示审批单号）"
    );

    // ---------- ⑥ 建发货单（带出未发完成行） ----------
    println!("[M3-24] 步 8/9 建发货单 → 提交 → 过账（在途，不扣库存）");
    let shipment = shipment_handler::create_shipment(
        Some(maker),
        CreateShipmentReq {
            so_id: order_id.to_string(),
            warehouse_id: warehouse_id.to_string(),
            ship_date: None,
            remark: Some("M3-24 全链：整单发货".to_string()),
            // 留空 = 从订单带出全部未发完的行（这是前端「从订单带出」的唯一读数口径）
            items: vec![],
        },
    )
    .await
    .unwrap_or_else(|error| panic!("建发货单应成功：{} {}", error.code, error.msg));
    let ship_id = Uuid::parse_str(&shipment.id).expect("发货单 id 是 UUID");
    assert_eq!(shipment.status, "DRAFT");
    assert!(shipment.ship_no.starts_with("SH-"), "发货单号必须是 SH- 前缀");
    assert_eq!(shipment.items.len(), 2, "带出订单的两行未发量");
    assert_eq!(shipment.total_qty, dec("15"), "合计 10 + 5 件");
    assert_eq!(
        shipment.so_no.as_deref(),
        Some(converted.header.so_no.as_str()),
        "发货单必须回显来源订单号（全链可追溯）"
    );

    // ---------- ⑦ 提交 → 过账（在途 + 出库待确认） ----------
    let submitted_ship = shipment_handler::change_shipment_status(
        Some(maker),
        &ship_id.to_string(),
        ShipmentStatusReq {
            target_status: "PENDING_POST".to_string(),
            reason: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("发货单提交应成功：{} {}", error.code, error.msg));
    assert_eq!(submitted_ship.status, "PENDING_POST");
    let posted = shipment_handler::change_shipment_status(
        Some(maker),
        &ship_id.to_string(),
        ShipmentStatusReq {
            target_status: "POSTED".to_string(),
            reason: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("发货过账应成功：{} {}", error.code, error.msg));
    assert_eq!(posted.status, "POSTED");
    let stock_out_id = posted
        .stock_out_id
        .as_deref()
        .and_then(|text| Uuid::parse_str(text).ok())
        .expect("过账必须生成销售出库单");
    // 出库待确认：单头 + 来源链 + 状态
    let out_shape = scalar(
        "SELECT out_no || '/' || status || '/' || COALESCE(source_type, '') || '/' || out_type \
           FROM biz_stock_out WHERE id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
    .expect("出库单存在");
    let out_no = out_shape.split('/').next().unwrap_or_default().to_string();
    assert!(out_no.starts_with("OUT-"), "出库单号必须是 OUT- 前缀，实际 {out_no}");
    assert!(
        out_shape.ends_with("/PENDING_POST/SALES/OTHER"),
        "销售出库单必须是 source_type=SALES + out_type=OTHER + PENDING_POST，实际 {out_shape}"
    );
    assert_eq!(
        scalar(
            "SELECT source_id::text FROM biz_stock_out WHERE id = ?::uuid",
            vec![Value::from(&stock_out_id)]
        )
        .await
        .as_deref(),
        Some(ship_id.to_string().as_str()),
        "出库单必须指回发货单（source_id）"
    );
    // 过账**不扣库存**（M2-02：货在途，既不是仓里也不是已出库）
    assert_eq!(stock_qty(mat_1).await, dec("100"), "发货过账不扣库存（物料甲）");
    assert_eq!(stock_qty(mat_2).await, dec("100"), "发货过账不扣库存（物料乙）");
    assert_eq!(open_transit_qty(mat_1).await, dec("10"), "在途 10（物料甲）");
    assert_eq!(open_transit_qty(mat_2).await, dec("5"), "在途 5（物料乙）");
    assert_eq!(voucher_count_of(stock_out_id).await, 0, "过账后签收前没有成本结转凭证");
    assert_eq!(receivable_count_of(stock_out_id).await, 0, "过账后签收前不挂应收");
    // 订单：全发完 → OUTBOUND；行上 delivered_qty 回写
    assert_eq!(
        scalar(
            "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)]
        )
        .await
        .as_deref(),
        Some("OUTBOUND"),
        "整单发完订单必须转 OUTBOUND"
    );
    let delivered = scalar(
        "SELECT string_agg(delivered_qty::text, ',' ORDER BY line_no) \
           FROM biz_sales_order_item WHERE so_id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("订单明细存在");
    assert_eq!(delivered, "10.0000,5.0000", "delivered_qty 必须按行回写");

    // ---------- ⑧ 签收确认 = 出库过账 ----------
    println!("[M3-24] 步 9/9 签收确认（出库过账：三本账 + 成本结转凭证 + 挂应收）");
    let received = shipment_handler::confirm_receipt(Some(maker), &ship_id.to_string(), Some("M3-24 全链：客户签收"))
        .await
        .unwrap_or_else(|error| panic!("签收确认应成功：{} {}", error.code, error.msg));
    assert_eq!(received.status, "POSTED", "签收不改发货单状态（已发货）");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_stock_out WHERE id = ?::uuid",
            vec![Value::from(&stock_out_id)]
        )
        .await
        .as_deref(),
        Some("POSTED"),
        "签收确认必须把销售出库单过账"
    );
    // ① 三本账负向（成本账 / 库位账 / 流水）
    assert_eq!(stock_qty(mat_1).await, dec("90"), "成本账 100 − 10 = 90");
    assert_eq!(stock_qty(mat_2).await, dec("95"), "成本账 100 − 5 = 95");
    assert_eq!(location_qty(mat_1).await, dec("90"), "库位账同样 90");
    assert_eq!(location_qty(mat_2).await, dec("95"), "库位账同样 95");
    assert_eq!(txn_out(mat_1).await, dec("10"), "物料甲必须留下 10 的 OUT 流水");
    assert_eq!(txn_out(mat_2).await, dec("5"), "物料乙必须留下 5 的 OUT 流水");
    let txn_biz_type = scalar(
        "SELECT string_agg(DISTINCT biz_type, ',') FROM biz_inventory_txn \
          WHERE material_id IN (SELECT id FROM biz_material WHERE material_code IN ('M324-M1', 'M324-M2')) \
            AND txn_type = 'OUT'",
        vec![],
    )
    .await
    .expect("出库流水存在");
    assert_eq!(
        txn_biz_type, "SALE_OUT",
        "销售出库流水必须是 SALE_OUT（只看 out_type 会误记成 OTHER_OUT）"
    );
    // ② 在途清零
    assert_eq!(open_transit_qty(mat_1).await, Decimal::ZERO, "签收后在途归零（物料甲）");
    assert_eq!(open_transit_qty(mat_2).await, Decimal::ZERO, "签收后在途归零（物料乙）");
    let transit_shape = scalar(
        "SELECT string_agg(status || ':' || shipped_qty::text || '/' || received_qty::text, ',' \
                            ORDER BY status) \
           FROM biz_inventory_in_transit WHERE shipment_id = ?::uuid",
        vec![Value::from(&ship_id)],
    )
    .await
    .expect("在途行存在");
    assert_eq!(
        transit_shape, "RECEIVED:10.0000/10.0000,RECEIVED:5.0000/5.0000",
        "两条在途行都必须「签满 + RECEIVED」"
    );
    // ③ 成本结转凭证：1 张，借=贷=结转成本（10×10 + 5×10 = 150）
    assert_eq!(voucher_count_of(stock_out_id).await, 1, "签收确认才生成成本结转凭证");
    let voucher_shape = scalar(
        "SELECT v.voucher_no || '/' || v.total_debit::text || '/' || v.total_credit::text || '/' || \
                (SELECT count(*)::text FROM biz_voucher_entry e WHERE e.voucher_id = v.id) \
           FROM biz_voucher v WHERE v.source_type = 'STOCK_OUT' AND v.source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
    .expect("成本结转凭证存在");
    let total_debit = voucher_shape.split('/').nth(1).unwrap_or_default().to_string();
    assert_eq!(
        total_debit, "150.0000",
        "成本结转额 = 100 + 50，实际凭证 {voucher_shape}"
    );
    let parts: Vec<&str> = voucher_shape.split('/').collect();
    assert_eq!(parts[1], parts[2], "凭证必须借贷平衡，实际 {voucher_shape}");
    // 分录条数与科目：两张物料各一条借（结转成本）+ 一条贷（库存商品）
    let entries = scalar(
        "SELECT string_agg(e.account_code || ':' || e.debit_amount::text || '/' || e.credit_amount::text, ',' \
                            ORDER BY e.account_code) \
           FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.source_type = 'STOCK_OUT' AND v.source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
    .expect("凭证分录存在");
    println!("[M3-24] 成本结转凭证分录：{entries}");
    assert!(
        entries.contains("6401:100.0000/0.0000") && entries.contains("6401:50.0000/0.0000"),
        "借方必须是 6401 主营业务成本（100 + 50），实际 {entries}"
    );
    assert!(
        entries.contains("1405:0.0000/100.0000") && entries.contains("1405:0.0000/50.0000"),
        "贷方必须是 1405 库存商品（100 + 50），实际 {entries}"
    );
    assert_eq!(parts[3], "4", "两张物料各借/贷一条 = 4 条分录，实际 {voucher_shape}");
    assert_eq!(
        dec_of(
            "SELECT total_amount::text FROM biz_stock_out WHERE id = ?::uuid",
            vec![Value::from(&stock_out_id)]
        )
        .await,
        dec("150"),
        "出库单成本金额 = 结转成本 150"
    );
    // ④ 应收挂账：金额 = 发货明细含税 = 订单价税合计（整单发完，两者相等）
    assert_eq!(receivable_count_of(stock_out_id).await, 1, "签收确认必须挂应收");
    let ar_shape = scalar(
        "SELECT ar_no || '/' || amount::text || '/' || status || '/' || source_no \
           FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
    .expect("应收存在");
    let ar_parts: Vec<&str> = ar_shape.split('/').collect();
    assert!(
        ar_parts[0].starts_with("AR-"),
        "应收单号必须是 AR- 前缀，实际 {ar_shape}"
    );
    assert_eq!(ar_parts[1], "2260.0000", "应收金额 = 含税 2260，实际 {ar_shape}");
    assert_eq!(ar_parts[2], "OPEN", "新挂的应收是未结清（OPEN），实际 {ar_shape}");
    assert_eq!(ar_parts[3], out_no, "应收来源单号必须指出库单号（全链可追溯）");
    assert_eq!(
        scalar(
            "SELECT customer_id::text FROM biz_receivable \
              WHERE source_type = 'SALES_OUT' AND source_id = ?::uuid",
            vec![Value::from(&stock_out_id)]
        )
        .await
        .as_deref(),
        Some(customer_id.to_string().as_str()),
        "应收必须挂到报价单的客户身上"
    );

    // ---------- ⑨ 收口 ----------
    println!("[M3-24] 收口：幂等重放 / 终态不可重发 / 无 PENDING 实例 / 无孤儿日志");
    let replay = quote::convert_quotation(None, &quote_id.to_string(), ConvertQuotationReq::default())
        .await
        .unwrap_or_else(|error| panic!("已转单的报价单再点转单必须幂等回原单：{} {}", error.code, error.msg));
    assert_eq!(replay.header.so_id, order_id.to_string(), "幂等重放必须回原订单");
    assert_eq!(
        replay.meta.extra.get("already_converted"),
        Some(&serde_json::json!(true)),
        "幂等重放必须标记 already_converted = true"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_sales_order WHERE customer_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&customer_id)]
        )
        .await,
        1,
        "全链只允许产出 1 张订单（幂等不许造第二张）"
    );
    let error = shipment_handler::create_shipment(
        Some(maker),
        CreateShipmentReq {
            so_id: order_id.to_string(),
            warehouse_id: warehouse_id.to_string(),
            ship_date: None,
            remark: None,
            items: vec![],
        },
    )
    .await
    .expect_err("已 OUTBOUND 的订单不能继续发货");
    assert_eq!(error.status, 409, "终态订单发货是 409，实际 {}", error.status);
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_flow_instance \
              WHERE status = 'PENDING' \
                AND (biz_id IN (SELECT id FROM biz_quotation WHERE customer_id = ?::uuid) \
                  OR biz_id IN (SELECT id FROM biz_sales_order WHERE customer_id = ?::uuid))",
            vec![Value::from(&customer_id), Value::from(&customer_id)]
        )
        .await,
        0,
        "全链跑完不许留下任何 PENDING 审批实例（死单）"
    );
    // 状态日志必须能回答「这张单一路怎么走过来的」
    let quote_log = count(
        "SELECT count(*) FROM biz_status_log WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid",
        vec![Value::from(&quote_id)],
    )
    .await;
    assert!(
        quote_log >= 3,
        "报价单至少 3 条轨迹（发出 / 接受 / 转单），实际 {quote_log}"
    );
    let order_log = count(
        "SELECT count(*) FROM biz_status_log WHERE entity_type = 'SALE_ORDER' AND entity_id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await;
    assert!(
        order_log >= 3,
        "订单至少 3 条轨迹（提交 / 审批 / 出库），实际 {order_log}"
    );
    let ship_log = count(
        "SELECT count(*) FROM biz_status_log WHERE entity_type = 'SHIPMENT' AND entity_id = ?::uuid",
        vec![Value::from(&ship_id)],
    )
    .await;
    assert!(ship_log >= 2, "发货单至少 2 条轨迹（提交 / 过账），实际 {ship_log}");

    println!(
        "[M3-24] 全链通过：{} → {} → {} → {} → 应收 {:.2} / 成本凭证 {:.2}",
        quote.quote_no,
        converted.header.so_no,
        shipment.ship_no,
        out_no,
        dec("2260"),
        dec("150")
    );

    // 证据模式：保留夹具供库侧取证（`M324_KEEP_FIXTURES=1`；业务断言已全部跑完）
    if std::env::var("M324_KEEP_FIXTURES").as_deref() == Ok("1") {
        println!("[M3-24] M324_KEEP_FIXTURES=1：保留夹具不清场（供 SQL 取证）");
        return;
    }

    // 收尾：清掉本模块痕迹，并把销售链的历史孤儿日志一并收干（模板库里可能自带，
    // 所以这里**只看打印不断言 0**：断言 0 会把「上一轮别的模块留下的」算到本卡头上）。
    cleanup().await;
    let orphans = fixture_hygiene::purge_orphans(fixture_hygiene::SALES).await;
    println!("[M3-24] 收尾：清理销售链孤儿状态日志 {orphans} 条（历史遗留，非本组产生）");
    // 本组自己的日志一条都不许剩下（按 id 精确判，不受历史孤儿影响）
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_status_log WHERE \
                (entity_type = 'QUOTATION' AND entity_id = ?::uuid) \
             OR (entity_type = 'SALE_ORDER' AND entity_id = ?::uuid) \
             OR (entity_type = 'SHIPMENT' AND entity_id = ?::uuid) \
             OR (entity_type = 'STOCK_OUT' AND entity_id = ?::uuid)",
            vec![
                Value::from(&quote_id),
                Value::from(&order_id),
                Value::from(&ship_id),
                Value::from(&stock_out_id),
            ]
        )
        .await,
        0,
        "本组四类单据的状态日志必须随单据一起清干净"
    );
}

/// 全链跑完后夹具零残留（与 drill 脚本的库侧断言互为双保险）。
///
/// 放在同一个文件里而不是脚本里：脚本只能看「表里有没有前缀行」，
/// 而这条用例看的是「业务单据 + 审批实例 + 状态日志 + 凭证/应收」四类一起归零。
#[tokio::test]
async fn zz_teardown_leaves_no_sales_chain_residue() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer().await;
    let params = || vec![Value::from(&customer_id)];
    // 六类业务痕迹一起归零（单据 / 审批实例 / 凭证 / 应收 / 在途 / 三本账）
    let residue: Vec<(&str, &str)> = vec![
        (
            "报价单",
            "SELECT count(*) FROM biz_quotation WHERE customer_id = ?::uuid",
        ),
        (
            "销售订单",
            "SELECT count(*) FROM biz_sales_order WHERE customer_id = ?::uuid",
        ),
        (
            "审批实例",
            "SELECT count(*) FROM biz_flow_instance \
              WHERE biz_id IN (SELECT id FROM biz_quotation WHERE customer_id = ?::uuid) \
                 OR biz_id IN (SELECT id FROM biz_sales_order WHERE customer_id = ?::uuid)",
        ),
        (
            "夹具仓库库存",
            "SELECT count(*) FROM biz_inventory WHERE warehouse_id IN \
                (SELECT id FROM biz_warehouse WHERE warehouse_code = 'M324-WH')",
        ),
        (
            "在途账",
            "SELECT count(*) FROM biz_inventory_in_transit WHERE material_id IN (SELECT id FROM biz_material \
                WHERE material_code IN ('M324-M1', 'M324-M2'))",
        ),
        (
            "出库流水",
            "SELECT count(*) FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material \
                WHERE material_code IN ('M324-M1', 'M324-M2'))",
        ),
    ];
    for (label, query) in residue {
        // 占位符个数决定给几个参数（审批实例那条 2 个，其余 1 个，库存/在途/流水 0 个）
        let args = match query.matches("?::uuid").count() {
            2 => vec![Value::from(&customer_id), Value::from(&customer_id)],
            1 => params(),
            _ => vec![],
        };
        assert_eq!(count(query, args).await, 0, "{label} 必须零残留");
    }
    // 断言完再清场：`setup()` 为跑断言而重建了夹具客户，末尾把它一起收走，
    // 否则 drill 脚本的「M324-CUST 零残留」会红在这条**故意留下的**夹具上。
    cleanup().await;
}

// ============================================================================
// 读了算数的小工具（断言用的读数，一律走 SQL —— 回执只是「我以为」，库才是事实）
// ============================================================================

/// 成本账数量（`biz_inventory` 按物料汇总）。
async fn stock_qty(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory WHERE material_id = ?::uuid",
        vec![Value::from(&material_id)],
    )
    .await
}

/// 库位账数量（`biz_inventory_location` 按物料汇总）。
async fn location_qty(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_location WHERE material_id = ?::uuid",
        vec![Value::from(&material_id)],
    )
    .await
}

/// 未签收在途量（`biz_inventory_in_transit.status = 'IN_TRANSIT'`）。
async fn open_transit_qty(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(qty_in_transit), 0)::text FROM biz_inventory_in_transit \
          WHERE material_id = ?::uuid AND status = 'IN_TRANSIT'",
        vec![Value::from(&material_id)],
    )
    .await
}

/// OUT 流水数量合计。
async fn txn_out(material_id: Uuid) -> Decimal {
    dec_of(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_txn \
          WHERE material_id = ?::uuid AND txn_type = 'OUT'",
        vec![Value::from(&material_id)],
    )
    .await
}

async fn voucher_count_of(stock_out_id: Uuid) -> i64 {
    count(
        "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
}

async fn receivable_count_of(stock_out_id: Uuid) -> i64 {
    count(
        "SELECT count(*) FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
}

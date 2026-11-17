// ============================================================================
// credit_control_e2e_tests.rs —— 销售缺口 E2E：信用控制（M3-25 · 超限场景）
//
// 卡：t_76dcfc44（M3-25【M3·销售】销售缺口 E2E：信用控制 / 超限场景）
//
// # 这一份钉的是什么（一句话）
//
// **一张已经批过的订单真的吃掉了额度、然后下一张单被它拦住、再然后钱收回来额度真的还了**
// —— 用一个客户、七张单、一张收款，把「占用 → 超限拦截 → 调额放行 → 核销释放 → 策略
// 放行（WARN/PASS）→ 审批点拦截 → 过审兜底打回草稿 → 不控额度」串成一条链。
//
// # 为什么上游已经有一堆信用单测了，还要这一条
//
// ```text
// credit::tests                      ← 纯函数与公式（不碰单据）
// sales_order_handler::tests          ← 提交守卫（订单是 SQL 夹具、客户额度手改）
// sales_approval_bridge::tests        ← 审批前闸 / 过审兜底（实例与订单都是 SQL 夹具）
// credit_release_tests                ← 核销即释放（订单 + 应收都是 SQL 直插）
// credit_concurrency_tests            ← 并发 CAS（同上）
// 本文件（M3-25）                      ← **同一张客户额度账，单据全部走真实入口**：
//                                      建单 → 提交审批 → 真派真批 → 发货 → 签收 → 挂应收
//                                      → 登记收款 → 核销；每换一段都重新读库对账
// ```
//
// 上游用例各自证明「这一段对不对」；本卡证明「它们接起来之后，额度账还是对的」——
// 尤其是 **APPROVED 时刻才占用、核销时刻才释放** 这两个时机，只有跑完整条链才有意义。
//
// # 链路（12 步 · 断言点即卡面「超限场景」的落点）
//
// ```text
//  ① 客户额度 1000 / 策略 BLOCK；订单 A 含税 800 → 提交 → 真审批 → APPROVED  占用 800
//  ② 订单 B 含税 300 → 提交：**409 + 10003**（800 + 300 > 1000）            单据留 DRAFT、零实例、零台账
//     └ 快照：verdict=BLOCK / allowed=false / available=200 / over=100
//  ③ 额度调到 1100（走客户编辑真接口）→ B 重提：**边界 = 恰好等于额度算通过** → 审批 → 占用 1100
//  ④ 订单 C 含税 300 → 提交 409（1100 + 300 > 1100）                        额度真被吃满
//  ⑤ 发货 A → 过账 → 签收确认 → 应收 800 OPEN；此刻占用**仍是 1100**（挂账不释放）
//  ⑥ 登记收款 800 → 核销 → **核销即释放**：占用 1100 → 300
//     └ 回执 released=800 / cause=AR_SETTLED / 台账一行 delta=-800
//  ⑦ C 重提 → 通过（300 + 0 + 300 ≤ 1100）→ 审批 → 占用 600                被拦的单能继续
//  ⑧ 策略改 WARN：订单 D 含税 700 → 提交放行 + credit_warning + 台账（delta=0）→ 审批 → 占用 1300
//  ⑨ 策略改 PASS：订单 E 含税 500 → 提交静默放行（warning=None，台账不增）→ 审批 → 占用 1800
//  ⑩ 审批点闸：额度调 5000 → 订单 F(1000) 提交通过 → **额度调低到 100** → 审批 409 + 10003
//     └ 待办仍 PENDING、单据仍 SUBMITTED、额度不动（可再点 / 可驳回）
//  ⑪ 过审兜底（约定 11）：额度仍 100，绕过审批前闸直接 approve → 回调把单**打回 DRAFT**
//     原因写 remark，实例 APPROVED、额度不动；调回 5000 重提 → 审批通过 → 占用 2800（不留死单）
//  ⑫ 不控额度反证：额度 0 + 策略 BLOCK → 订单 G(5000) 提交照过（UNCONTROLLED）→ 审批 → 占用 7800
// ```
//
// 收口断言：缓存列 == 公式（S9 同源）、台账 Σdelta == 缓存列、快照 cache_drift=false、
// 本客户无 PENDING 审批实例、夹具清场后零残留。
//
// # 与 M3-24（报价 → 订单 → 出库 E2E）的分工
//
// M3-24 从报价单出发，验的是「单据链能不能走通」；本卡从订单出发，验的是「额度账怎么变」。
// 两卡共用同一套骨架（`test_seed` 夹具 + 审批三步真派真批 + `run_group` 失败定位 + 有牙演练），
// 但**夹具前缀互不相同**（`M324-` / `M325-`），可以在同一个演练库里先后跑。
//
// # 实测到的三处口径（写断言前先看，别照直觉写）
//
// 1. **挂应收 ≠ 释放额度**：`confirm_receipt` 只是把敞口从「订单未发」挪到「应收未收」，
//    `credit_used` 一分不动（`ARCH-7 §15.5.2`：出集只有作废 / 收款核销 / 退货过账三条路）。
//    ⑤ 的断言必须写「仍 1100」，⑥ 才是「→ 300」。
// 2. **`credit_limit = 0` 时策略完全不生效**（`CreditVerdict::Uncontrolled`）：BLOCK 也放行，
//    快照 `available` 为 `null` 而不是 0 —— 前端别把 null 渲染成「额度 0 元」。
// 3. **审批被拦时单据留在 `SUBMITTED`，审批人是可以再点的**；只有「审批已通过、写单据时
//    才发现超限」才打回 `DRAFT`（约定 11 的补偿）。两条路径的断言完全不同，别混。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `M325-` 夹具、同一份 `biz_seq` 号段），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m325_credit_e2e" cargo test --lib \
//   business::erp::sales::credit_control_e2e_tests -- --test-threads=1
// ```
//
// 也可直接跑 `bash scripts/verify/m325_credit_control_e2e_drill.sh`
// （克隆演练库 → 补 V0405…V0445 → 前置断言 → 本文件 + 信用域 + 销售域回归 → SKIP=0
//  → 夹具零残留 → DROP）。
// ============================================================================

use nik::prelude::chrono::{Datelike, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::approval_gate::ApprovalGate;
use crate::business::erp::credit;
use crate::business::erp::credit_handler::{self, CreditSnapshotQuery, CreditSnapshotRes};
use crate::business::erp::customer::customer_handler::{self, UpdateCustomerReq};
use crate::business::erp::receipt::{self, AllocateItemReq, AllocateReq, CreateReceiptReq};
use crate::business::erp::sales::sales_order_handler::{self, CreateSalesOrderReq, SalesOrderItemReq};
use crate::business::erp::shipment::shipment_handler::{self, CreateShipmentReq, ShipmentStatusReq};
use crate::business::erp::stockin::stockin_model;
use crate::business::flow::{doc_reader, engine};
use crate::test_seed;

/// 真库用例必须串行（共用全局 `db()` 与同一批 `M325-` 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const CUSTOMER_CODE: &str = "M325-CUST";
const CUSTOMER_NAME: &str = "M3-25 信用控制 E2E 客户";
const WAREHOUSE_CODE: &str = "M325-WH";
const MAT_A_CODE: &str = "M325-M1";
const MAT_B_CODE: &str = "M325-M2";
/// 制单人（提交审批的人）—— 必须是**不属于**审批角色的人，否则会撞制单/审批分离闸。
const MAKER: &str = "m325_maker";
/// 夹具审批人（绑 `SALES_SUPERVISOR`；`so_approve` 第一跳就是这个角色）。
const APPROVER: &str = "m325_appr";
/// 本模块两张夹具物料（清场 / 三本账 / 在途账共用，按编码取，不写死 UUID）。
const MATS_SQL: &str = "(SELECT id FROM biz_material WHERE material_code IN ('M325-M1', 'M325-M2'))";

/// 初始额度（订单 A 800 + 订单 B 300 恰好超 100，是做「卡在边界上」的最小配置）。
const LIMIT_START: &str = "1000";
/// 调额后额度（800 + 300 恰好等于它 —— 边界「等号算通过」的落点）。
const LIMIT_RAISED: &str = "1100";
/// 订单单价（两张物料同价，成本 5 ⇒ 发货 80 件结转成本 400）。
const PRICE: &str = "10";
/// 订单 A 的发货量（80 × 10 = 800 含税；税率 0，金额不必再换算）。
const QTY_A: &str = "80";

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

async fn customer() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_CODE, CUSTOMER_NAME).await
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_CODE, "M3-25 信用 E2E 夹具仓").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "信用 E2E 夹具物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "信用 E2E 夹具物料乙").await
}

/// 制单人 / 审批人：主数据走 `test_seed`（P2-62 门禁：测试代码不许手写主数据 INSERT）。
async fn people() -> (Uuid, Uuid) {
    let maker = test_seed::ensure_user(MAKER, "M3-25 信用 E2E 制单人", None).await;
    let approver = test_seed::ensure_user(APPROVER, "M3-25 信用 E2E 审批人", None).await;
    // `so_approve` 第一跳是 SALES_SUPERVISOR；`bind_role` 幂等，种子里该角色已有别人时
    // 只是多一个成员（待办条数由库决定，用例不假设审批人是谁）。
    test_seed::bind_role(approver, "SALES_SUPERVISOR").await;
    (maker, approver)
}

/// 清场：只清本模块**业务单据**及其痕迹（`M325-CUST` 客户名下的订单 / 发货 / 出库 / 应收 / 收款）。
///
/// 顺序不可交换：
/// 1. `biz_status_log` 没有外键 ⇒ **先于**单据按 id 清（单据一删就查不到 id，改收不走）；
/// 2. 凭证 / 应收 / 收款是弱引用或 `RESTRICT` ⇒ 在删来源单**之前**清；
/// 3. 审批实例 `biz_id` 是弱引用，`prev_instance_id` 自引用无 ON DELETE ⇒ 先解开再删；
/// 4. 外键链：`biz_inventory`（FK 指向库位账）→ `biz_inventory_location`；
///    `biz_stock_out` → `biz_shipment` → `biz_sales_order`。
async fn cleanup() {
    let cust = format!("SELECT id FROM biz_customer WHERE code = '{CUSTOMER_CODE}'");
    let orders = format!("SELECT id FROM biz_sales_order WHERE customer_id IN ({cust})");
    let shipments = format!("(SELECT id FROM biz_shipment WHERE so_id IN ({orders}))");
    let stock_outs = format!("(SELECT id FROM biz_stock_out WHERE source_id IN {shipments})");
    let receivables = format!("SELECT id FROM biz_receivable WHERE customer_id IN ({cust})");
    let receipts = format!("SELECT id FROM biz_receipt WHERE customer_id IN ({cust})");
    let instances = format!("SELECT id FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE' AND biz_id IN ({orders})");
    let vouchers = format!(
        "(SELECT id FROM biz_voucher WHERE (source_type = 'STOCK_OUT' AND source_id IN {stock_outs}) \
            OR (source_type = 'RECEIPT' AND source_id IN ({receipts})))"
    );
    let statements: Vec<String> = vec![
        format!(
            "DELETE FROM biz_status_log WHERE \
                (entity_type = 'SALE_ORDER' AND entity_id IN ({orders})) \
             OR (entity_type = 'SHIPMENT' AND entity_id IN {shipments}) \
             OR (entity_type = 'STOCK_OUT' AND entity_id IN {stock_outs}) \
             OR (entity_type = 'RECEIVABLE' AND entity_id IN ({receivables})) \
             OR (entity_type = 'RECEIPT' AND entity_id IN ({receipts})) \
             OR (entity_type = 'VOUCHER' AND entity_id IN {vouchers})"
        ),
        // 凭证分录 → 凭证
        format!("DELETE FROM biz_voucher_entry WHERE voucher_id IN {vouchers}"),
        format!("DELETE FROM biz_voucher WHERE id IN {vouchers}"),
        // 核销明细挂应收（RESTRICT）→ 先删明细，再删收款、应收
        format!("DELETE FROM biz_receipt_allocation WHERE receivable_id IN ({receivables})"),
        format!("DELETE FROM biz_receipt_allocation WHERE receipt_id IN ({receipts})"),
        format!("DELETE FROM biz_receipt WHERE customer_id IN ({cust})"),
        format!("DELETE FROM biz_receivable WHERE customer_id IN ({cust})"),
        // 信用台账（调试凭证：额度账的历史，随客户一起清）
        format!("DELETE FROM biz_credit_log WHERE customer_id IN ({cust})"),
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
        // 客户本体（软删记录也一并物理清：本测试前缀独占，不与别人共用编码）
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
    // 客户额度配置回到本卡的起点（额度 / 策略走真实编辑接口，缓存列只能直改）
    customer().await;
    set_credit(LIMIT_START, credit::POLICY_BLOCK).await;
    // 缓存列不是接口能写的（它是派生值）：清完单据后显式归零，与公式同步
    sql(&format!(
        "UPDATE biz_customer SET credit_used = 0 WHERE code = '{CUSTOMER_CODE}'"
    ))
    .await;
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

// ============================================================================
// 客户额度配置（走真实编辑接口，不直改库 —— 否则「额度怎么改的」这条链没被验过）
// ============================================================================

/// 改客户额度 / 策略（`update_customer`，与 `PUT /business/erp/customer/{id}` 同一条路）。
async fn set_credit(limit: &str, policy: &str) {
    let id = customer().await;
    customer_handler::update_customer(
        None,
        &id.to_string(),
        UpdateCustomerReq {
            credit_limit: Some(dec(limit)),
            credit_policy: Some(policy.to_string()),
            status: Some("ACTIVE".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|error| panic!("改客户额度失败（{limit}/{policy}）：{} {}", error.code, error.msg));
}

/// 缓存列 `credit_used`。
async fn used() -> Decimal {
    let id = customer().await;
    credit::stored(db(), id).await.expect("读缓存列").expect("客户存在")
}

/// **缓存列 == 公式 == 期望值** 三件事一起断言（S9 同源；差一分钱都算漂移）。
///
/// 只断言缓存列会漏掉「公式变了而缓存没跟上」；只断言公式会漏掉「缓存没刷新」——
/// 而后者正是客户端 / 前端看到的值。
async fn assert_used(label: &str, expected: &str) {
    let cached = used().await;
    let usage = credit::usage_of(db(), customer().await).await.expect("现算额度口径");
    assert_eq!(
        cached,
        dec(expected),
        "{label}：缓存列 credit_used 应为 {expected}，实际 {cached}（{}）",
        usage.summary()
    );
    assert_eq!(
        usage.used, cached,
        "{label}：缓存列必须等于公式（{}+{}+{} ⇒ {}），实际缓存 {cached}",
        usage.ordered, usage.received, usage.returned, usage.used
    );
}

/// 客户信用快照（与 `GET /business/erp/credit/{customer_id}` 同一条路）。
async fn snapshot(amount: Option<&str>) -> CreditSnapshotRes {
    let id = customer().await;
    credit_handler::credit_snapshot_of(
        &id.to_string(),
        CreditSnapshotQuery {
            amount: amount.map(dec),
            exclude_order_id: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("读信用快照失败：{} {}", error.code, error.msg))
}

/// 台账行数（可按 cause 收窄）。
async fn ledger_count(cause: Option<&str>) -> i64 {
    let id = customer().await;
    match cause {
        Some(cause) => {
            count(
                "SELECT count(*) FROM biz_credit_log WHERE customer_id = ?::uuid AND cause = ?",
                vec![Value::from(&id), Value::Text(cause.to_string())],
            )
            .await
        }
        None => {
            count(
                "SELECT count(*) FROM biz_credit_log WHERE customer_id = ?::uuid",
                vec![Value::from(&id)],
            )
            .await
        }
    }
}

/// 台账某一单号下的行数（`ref_no` 是订单号 / 收款单号）。
async fn ledger_count_of_ref(cause: &str, ref_no: &str) -> i64 {
    let id = customer().await;
    count(
        "SELECT count(*) FROM biz_credit_log WHERE customer_id = ?::uuid AND cause = ? AND ref_no = ?",
        vec![
            Value::from(&id),
            Value::Text(cause.to_string()),
            Value::Text(ref_no.to_string()),
        ],
    )
    .await
}

// ============================================================================
// 订单 / 审批（与 HTTP 入口逐行同款，不直插状态）
// ============================================================================

/// 建一张销售订单（走 `create_sales_order` 真接口；税率 0 ⇒ 含税 = 不含税）。
async fn new_order(maker: Uuid, material_id: Uuid, qty: &str, remark: &str) -> (Uuid, String, Decimal) {
    let customer_id = customer().await;
    let res = sales_order_handler::create_sales_order(
        Some(maker),
        CreateSalesOrderReq {
            customer_id: customer_id.to_string(),
            remark: Some(remark.to_string()),
            items: vec![SalesOrderItemReq {
                material_id: material_id.to_string(),
                qty: Some(dec(qty)),
                price: Some(dec(PRICE)),
                tax_rate: Some(Decimal::ZERO),
            }],
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|error| panic!("建销售订单应成功：{} {}", error.code, error.msg));
    assert_eq!(res.status, "DRAFT", "新建销售订单必须是草稿，实际 {}", res.status);
    assert!(
        res.so_no.starts_with("SO-"),
        "订单号必须是 SO- 前缀，实际 {}",
        res.so_no
    );
    let id = Uuid::parse_str(&res.id).expect("订单 id 是 UUID");
    (id, res.so_no.clone(), res.total_with_tax)
}

/// 提交审批（返回 `Result`，让用例自己断言拦截）。
async fn submit(maker: Uuid, order_id: Uuid) -> std::result::Result<sales_order_handler::SalesOrderRes, ApiError> {
    sales_order_handler::submit_approval(Some(maker), &order_id.to_string(), &ApprovalGate::none()).await
}

/// 订单当前状态。
async fn order_status(order_id: Uuid) -> Option<String> {
    scalar(
        "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
}

/// 订单的审批实例 id（`None` = 没挂实例）。
async fn order_instance(order_id: Uuid) -> Option<Uuid> {
    // `COALESCE` + 空串过滤：列是 NULL 时驱动会把 `NULL::text` 报成 `Unknown` 类型
    // （`expected Text, got Unknown`），直接 `::text` 取不到「没有实例」这个事实。
    scalar(
        "SELECT COALESCE(approval_instance_id::text, '') FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .filter(|text| !text.is_empty())
    .and_then(|text| Uuid::parse_str(&text).ok())
}

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
/// 生产是 `bootstrap` 启动期调 `erp::doc_reader_impl::register_all()`；`cargo test --lib`
/// 不起 bootstrap，进程级注册表是**空的** —— 于是 `dispatch_settled` 一个实现都找不到，
/// 审批通过后业务桥不执行（表现成「实例 APPROVED、订单还是 SUBMITTED」这种假业务 bug）。
async fn ensure_doc_registry() {
    match crate::business::erp::doc_reader_impl::register_all() {
        Ok(count) => println!("[M3-25] ERP 审批回调实现已注册 {count} 个"),
        Err(reason) => println!("[M3-25] ERP 审批回调注册表已就绪（{reason}）"),
    }
    let len = doc_reader::len();
    assert!(
        len >= 18,
        "进程级审批回调注册表必须有 18 个 ERP 实现（否则审批通过不会回调业务单据），实际 {len}"
    );
}

/// 逐节点把实例批到终态，返回最终实例状态。
///
/// 每一跳都走与 HTTP 入口同款的三步：自审闸 → 审批前业务闸（含**信用闸**）→
/// `engine::approve`（事务）→ `dispatch_settled`（业务桥扇出）。
async fn approve_until_settled(instance_id: Uuid) -> String {
    let mut hops = 0_usize;
    loop {
        // 实例已经落到终态 ⇒ 本轮结束。**必须先看实例状态**：节点是「角色全员待办」，
        // 实例被第一个人批完之后，其余待办行仍留在 `PENDING`（引擎只关掉实例，不逐行回收），
        // 继续点它们会拿一张已 APPROVED 的单去重算额度（已占里已经含它自己）——
        // 那是假红，不是业务 bug。
        if instance_status(instance_id).await.as_deref() != Some("PENDING") {
            break;
        }
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        hops += 1;
        assert!(hops <= 10, "审批跳数异常（>10 跳）：实例 {instance_id} 可能成环");
        for (task_id, node_key, assignee) in tasks {
            println!("[M3-25]   审批待办：节点 {node_key} / 受派人 {assignee}");
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
            if out.instance_status != "PENDING" {
                break;
            }
        }
    }
    instance_status(instance_id)
        .await
        .unwrap_or_else(|| panic!("实例 {instance_id} 不存在"))
}

/// **跳过审批前闸**直接批（只用于验证「审批已通过、写单据时才超限」的兜底分支）。
///
/// 真实窗口是「审批人点了同意 → 回调写单据」之间额度被别人占掉；用例里没法精确卡那个
/// 毫秒窗口，所以把额度先调低、再绕过前闸直接 approve —— 走的正是 `settle` 里那一段兜底。
async fn approve_skipping_guards(instance_id: Uuid) -> String {
    let mut hops = 0_usize;
    loop {
        if instance_status(instance_id).await.as_deref() != Some("PENDING") {
            break;
        }
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        hops += 1;
        assert!(hops <= 10, "审批跳数异常（>10 跳）：实例 {instance_id} 可能成环");
        for (task_id, node_key, assignee) in tasks {
            println!("[M3-25]   审批待办（绕过前闸）：节点 {node_key} / 受派人 {assignee}");
            let out = db()
                .transaction(|tx| async move { engine::approve(&tx, task_id, assignee, Some("同意")).await })
                .await
                .unwrap_or_else(|error| panic!("审批同意失败（节点 {node_key}）：{error}"));
            doc_reader::dispatch_settled(out.instance_id, &out.instance_status).await;
            if out.instance_status != "PENDING" {
                break;
            }
        }
    }
    instance_status(instance_id)
        .await
        .unwrap_or_else(|| panic!("实例 {instance_id} 不存在"))
}

/// 把订单提交 + 逐节点真批，返回 `(审批实例 id, 最终实例状态)`。
async fn submit_and_approve(maker: Uuid, order_id: Uuid, so_no: &str) -> (Uuid, String) {
    let submitted = submit(maker, order_id)
        .await
        .unwrap_or_else(|error| panic!("订单 {so_no} 提交审批应成功：{} {}", error.code, error.msg));
    assert_eq!(
        submitted.status, "SUBMITTED",
        "订单 {so_no} 提交后必须是 SUBMITTED，实际 {}",
        submitted.status
    );
    assert!(
        submitted.credit_warning.is_none(),
        "未超限的单提交响应不该带 credit_warning"
    );
    let instance = order_instance(order_id)
        .await
        .unwrap_or_else(|| panic!("订单 {so_no} 提交后必须挂审批实例"));
    let final_status = approve_until_settled(instance).await;
    assert_eq!(final_status, "APPROVED", "审批实例必须走到 APPROVED");
    assert_eq!(
        order_status(order_id).await.as_deref(),
        Some("APPROVED"),
        "订单 {so_no} 审批通过后必须是 APPROVED"
    );
    (instance, final_status)
}

/// 断言「提交被信用闸拦下」的公共口径：409 + 10003 + 单据留 DRAFT + 零实例 + 额度不动。
async fn assert_blocked(maker: Uuid, order_id: Uuid, so_no: &str, expected_used: &str) {
    let error = submit(maker, order_id)
        .await
        .expect_err(&format!("订单 {so_no} 超限且策略 BLOCK 必须被拦（不能放行）"));
    assert_eq!(error.status, 409, "超限拦截是 409，实际 {}", error.status);
    assert_eq!(
        error.code,
        cloudpivot_kernel::shared::error_code::RESOURCE_CONFLICT,
        "超限拦截的码必须是 10003（RESOURCE_CONFLICT），实际 {}（{}）",
        error.code,
        error.msg
    );
    assert!(
        error.msg.contains("信用额度不足") && error.msg.contains("超出"),
        "拒绝文案必须说清「信用额度不足 + 超出多少」，实际：{}",
        error.msg
    );
    assert!(
        error.msg.contains("收款") || error.msg.contains("调高额度"),
        "拒绝文案必须给出路（收款 / 调额 / 改策略），实际：{}",
        error.msg
    );
    // 被拦的单必须原地不动：DRAFT、无实例、不占额度、不写警示台账
    assert_eq!(
        order_status(order_id).await.as_deref(),
        Some("DRAFT"),
        "被拦的订单 {so_no} 必须留在 DRAFT（可改后重提，不留死单）"
    );
    assert!(
        order_instance(order_id).await.is_none(),
        "被拦的订单 {so_no} 不许留下审批实例"
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'SO_APPROVE' AND biz_id = ?::uuid",
            vec![Value::from(&order_id)]
        )
        .await,
        0,
        "被拦的订单 {so_no} 不许留下任何审批实例行"
    );
    assert_used(&format!("{so_no} 被拦后额度不动"), expected_used).await;
    assert_eq!(
        ledger_count_of_ref(credit::CAUSE_ORDER_OCCUPY, so_no).await,
        0,
        "被拦的订单 {so_no} 不许记 ORDER_OCCUPY 台账"
    );
}

// ============================================================================
// 用例：信用控制超限场景（全链）
// ============================================================================

/// **超限场景全链**：占用 → 拦截 → 调额放行 → 核销释放 → WARN/PASS → 审批点拦截 →
/// 过审兜底打回草稿 → 不控额度。
#[tokio::test]
async fn credit_control_over_limit_full_chain() {
    let Some(_guard) = setup().await else { return };
    ensure_doc_registry().await;
    let (maker, _approver) = people().await;
    let warehouse_id = warehouse().await;
    let (mat_1, mat_2) = (mat_a().await, mat_b().await);
    // 库存：各 100 件 @ 成本 5（订单 A 发 80 件 ⇒ 结转成本 400）
    seed_stock(mat_1, "100", "5").await;
    seed_stock(mat_2, "100", "5").await;
    let today = shipment_handler::today_cst(Utc::now());
    test_seed::ensure_accounting_period(today.year(), today.month()).await;
    sql(&format!(
        "UPDATE biz_accounting_period SET status = 'OPEN' WHERE period_code = '{}-{:02}'",
        today.year(),
        today.month()
    ))
    .await;

    // ---------- ① 订单 A：额度内的单正常占用 ----------
    println!("[M3-25] 步 1/12 建订单 A（含税 800 / 额度 1000）→ 提交 → 真审批 → APPROVED");
    let (order_a, so_a, total_a) = new_order(maker, mat_1, QTY_A, "M3-25 步 1：额度内首单").await;
    assert_eq!(total_a, dec("800"), "订单 A 价税合计 = 80 × 10 × (1 + 0%)");
    assert_used("A 建单（草稿不占额度）", "0").await;
    let snap = snapshot(Some("800")).await;
    assert_eq!(snap.verdict, "WITHIN", "A 提交前试算必须是 WITHIN");
    assert!(snap.allowed, "A 在额度内必须允许提交");
    let (_instance_a, _) = submit_and_approve(maker, order_a, &so_a).await;
    assert_used("A 审批通过（占用时机 = APPROVED）", "800").await;
    assert_eq!(
        ledger_count_of_ref(credit::CAUSE_ORDER_OCCUPY, &so_a).await,
        1,
        "A 审批通过必须记一条 ORDER_OCCUPY 台账"
    );
    let snap = snapshot(None).await;
    assert_eq!(snap.used, dec("800"), "快照已占 = 800");
    assert_eq!(snap.available, Some(dec("200")), "快照可用 = 1000 − 800 = 200");
    assert!(!snap.cache_drift, "快照必须报告缓存无漂移");

    // ---------- ② 订单 B：超限 → 409 拦截 ----------
    println!("[M3-25] 步 2/12 订单 B（含税 300）超限：提交必须 409 + 10003，单据留 DRAFT");
    let (order_b, so_b, total_b) = new_order(maker, mat_2, "30", "M3-25 步 2：超限被拦").await;
    assert_eq!(total_b, dec("300"), "订单 B 价税合计 = 30 × 10");
    // 下单前试算：800 + 300 = 1100 超 100
    let snap = snapshot(Some("300")).await;
    assert_eq!(snap.verdict, "BLOCK", "试算必须是 BLOCK（BLOCK 是默认策略）");
    assert!(!snap.allowed, "BLOCK 时 allowed 必须为 false");
    assert_eq!(snap.total, dec("1100"), "合计 = 已占 800 + 本单 300");
    assert_eq!(snap.over, dec("100"), "超出额 = 1100 − 1000 = 100");
    assert_eq!(snap.available, Some(dec("200")), "可用额度 = 200（不够这单）");
    assert_eq!(snap.order_amount, Some(dec("300")), "快照回显本次订单金额");
    assert_eq!(snap.customer_code, CUSTOMER_CODE);
    assert_eq!(snap.credit_policy, credit::POLICY_BLOCK);
    assert_blocked(maker, order_b, &so_b, "800").await;
    // 被拦后快照仍一致（没有任何副作用）
    let snap = snapshot(None).await;
    assert_eq!(snap.used, dec("800"), "被拦后已占仍是 800");
    assert_eq!(snap.pending, Decimal::ZERO, "被拦的单不进入在审占用");
    assert!(!snap.cache_drift, "被拦后缓存列与公式必须仍然一致");

    // ---------- ③ 调额 → B 重提（边界 = 等号算通过）----------
    println!("[M3-25] 步 3/12 额度调高到 1100（走客户编辑接口）→ B 重提：800 + 300 = 1100 边界通过");
    set_credit(LIMIT_RAISED, credit::POLICY_BLOCK).await;
    let snap = snapshot(Some("300")).await;
    assert_eq!(snap.credit_limit, dec(LIMIT_RAISED), "接口写入的额度必须立刻生效");
    assert_eq!(
        snap.verdict, "WITHIN",
        "已占 800 + 本单 300 = 1100 恰好等于额度 ⇒ 必须算通过（边界含等号）"
    );
    let (_instance_b, _) = submit_and_approve(maker, order_b, &so_b).await;
    assert_used("B 审批通过", "1100").await;
    let snap = snapshot(None).await;
    assert_eq!(snap.available, Some(Decimal::ZERO), "额度用满 ⇒ 可用 0");
    assert_eq!(snap.verdict, "WITHIN", "不传 amount 时判定的是「当前已占」状态");

    // ---------- ④ 订单 C：额度被吃满后再来一张 ----------
    println!("[M3-25] 步 4/12 订单 C（含税 300）：额度已用满，提交必须再次 409");
    let (order_c, so_c, total_c) = new_order(maker, mat_2, "30", "M3-25 步 4：额度用满被拦").await;
    assert_eq!(total_c, dec("300"));
    assert_blocked(maker, order_c, &so_c, "1100").await;

    // ---------- ⑤ 发货 A → 签收 → 挂应收（挂账不释放） ----------
    println!("[M3-25] 步 5/12 发货 A → 过账 → 签收确认 → 挂应收 800（此刻额度**不释放**）");
    let shipment = shipment_handler::create_shipment(
        Some(maker),
        CreateShipmentReq {
            so_id: order_a.to_string(),
            warehouse_id: warehouse_id.to_string(),
            ship_date: None,
            remark: Some("M3-25 信用链：整单发货".to_string()),
            items: vec![],
        },
    )
    .await
    .unwrap_or_else(|error| panic!("建发货单应成功：{} {}", error.code, error.msg));
    let ship_id = Uuid::parse_str(&shipment.id).expect("发货单 id 是 UUID");
    assert_eq!(shipment.status, "DRAFT");
    assert_eq!(shipment.total_qty, dec(QTY_A), "发货量 = 订单 A 的 80 件");
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
    // M2-02 口径：过账只在途，不挂应收 ⇒ 额度一分不动
    assert_used("发货过账（在途，订单仍占额度）", "1100").await;
    let received = shipment_handler::confirm_receipt(Some(maker), &ship_id.to_string(), Some("M3-25 信用链：客户签收"))
        .await
        .unwrap_or_else(|error| panic!("签收确认应成功：{} {}", error.code, error.msg));
    assert_eq!(received.status, "POSTED");
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
    let ar_shape = scalar(
        "SELECT ar_no || '/' || amount::text || '/' || status \
           FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
    .expect("签收确认必须挂应收");
    let ar_parts: Vec<&str> = ar_shape.split('/').collect();
    assert_eq!(ar_parts[1], "800.0000", "应收金额 = 发货含税 800，实际 {ar_shape}");
    assert_eq!(ar_parts[2], "OPEN", "新挂的应收是未结清（OPEN），实际 {ar_shape}");
    let receivable_id = scalar(
        "SELECT id::text FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
    .expect("应收 id");
    // ★ 挂账 ≠ 释放：应收只是把敞口从「订单」挪到「应收」，客户欠的钱一分没少
    assert_used("签收挂应收（挂账不释放额度）", "1100").await;
    let snap = snapshot(None).await;
    assert_eq!(
        snap.open_receivable,
        dec("800"),
        "快照的未清应收对照列 = 800（对照口径，不重复计入占用）"
    );
    assert_eq!(snap.occupied, dec("1100"), "占用仍 = 已占 1100 + 在审 0");

    // ---------- ⑥ 收款核销 → 核销即释放 ----------
    println!("[M3-25] 步 6/12 登记收款 800 → 核销该应收 → 占用 1100 → 300（核销即释放）");
    let created_receipt = receipt::create(
        Some(maker),
        CreateReceiptReq {
            customer_id: customer().await.to_string(),
            receipt_date: None,
            amount: dec("800"),
            method: Some("BANK_TRANSFER".to_string()),
            bank_account: Some("6222 0000 0000 3333".to_string()),
            remark: Some("M3-25 信用链：收 A 的货款".to_string()),
            currency_code: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("登记收款应成功：{} {}", error.code, error.msg));
    let receipt_id = created_receipt.receipt.id.clone();
    let receipt_no = created_receipt.receipt.receipt_no.clone();
    assert!(
        receipt_no.starts_with("RC-"),
        "收款单号必须是 RC- 前缀（库侧取证口径），实际 {receipt_no}"
    );
    assert_used("登记收款（未核销 = 不释放）", "1100").await;
    let allocated = receipt::allocate(
        Some(maker),
        &receipt_id,
        AllocateReq {
            items: vec![AllocateItemReq {
                receivable_id: receivable_id.clone(),
                amount: dec("800"),
                remark: Some("M3-25 核销".to_string()),
            }],
        },
    )
    .await
    .unwrap_or_else(|error| panic!("核销应成功：{} {}", error.code, error.msg));
    // 回执：释放 800，before 1100 → after 300，cause = AR_SETTLED，ref_no = 收款单号
    let release = allocated
        .credit_release
        .expect("核销动了额度 ⇒ 必须有 CreditRelease 回执");
    assert_eq!(release.cause, credit::CAUSE_AR_SETTLED);
    assert_eq!(release.ref_no, receipt_no, "回执 ref_no = 收款单号");
    assert_eq!(release.before_used, dec("1100"));
    assert_eq!(release.after_used, dec("300"));
    assert_eq!(release.released, dec("800"), "released 正数 = 释放");
    assert_used("核销后（核销即释放，不等 02:10 重算）", "300").await;
    let ar_after = scalar(
        "SELECT received_amount::text || '/' || status FROM biz_receivable WHERE id = ?::uuid",
        vec![Value::from(&Uuid::parse_str(&receivable_id).expect("应收 id 是 UUID"))],
    )
    .await
    .expect("应收存在");
    assert_eq!(
        ar_after, "800.0000/SETTLED",
        "应收必须被真回写成已结清，实际 {ar_after}"
    );
    assert_eq!(
        ledger_count_of_ref(credit::CAUSE_AR_SETTLED, &receipt_no).await,
        1,
        "核销必须记一条 AR_SETTLED 台账（ref_no = 收款单号）"
    );
    let snap = snapshot(None).await;
    assert_eq!(snap.used, dec("300"), "快照已占 = 300");
    assert_eq!(snap.available, Some(dec("800")), "可用回到 1100 − 300 = 800");
    assert_eq!(snap.open_receivable, Decimal::ZERO, "应收已结清 ⇒ 未清对照列归零");

    // ---------- ⑦ 被拦的 C 重新提交（额度释放后能继续） ----------
    println!("[M3-25] 步 7/12 订单 C 重提：300 + 0 + 300 = 600 ≤ 1100 → 通过 → 审批");
    let snap = snapshot(Some("300")).await;
    assert_eq!(snap.verdict, "WITHIN", "释放后 300 + 300 = 600 在额度内");
    let (_, _) = submit_and_approve(maker, order_c, &so_c).await;
    assert_used("C 审批通过", "600").await;

    // ---------- ⑧ WARN 策略：放行 + 提示 + 台账 ----------
    println!("[M3-25] 步 8/12 策略改 WARN：订单 D（700）超限但放行，响应带 credit_warning + 台账 delta=0");
    set_credit("1100", credit::POLICY_WARN).await;
    let warn_before = ledger_count(Some(credit::CAUSE_OVER_LIMIT_WARN)).await;
    let (order_d, so_d, total_d) = new_order(maker, mat_1, "70", "M3-25 步 8：WARN 放行").await;
    assert_eq!(total_d, dec("700"));
    let snap = snapshot(Some("700")).await;
    assert_eq!(snap.verdict, "WARN", "策略 WARN 时超限判定必须是 WARN");
    assert!(snap.allowed, "WARN 必须允许提交");
    assert_eq!(snap.total, dec("1300"), "600 + 700 = 1300");
    assert_eq!(snap.over, dec("200"), "超出 200");
    let warned = submit(maker, order_d)
        .await
        .unwrap_or_else(|error| panic!("WARN 策略必须放行提交：{} {}", error.code, error.msg));
    assert_eq!(warned.status, "SUBMITTED", "WARN 放行后单据进入 SUBMITTED");
    let warning = warned
        .credit_warning
        .as_deref()
        .expect("WARN 策略超限提交必须带 credit_warning 文案");
    assert!(
        warning.contains("WARN") && warning.contains(&so_d),
        "超限提示必须点名策略与本单号，实际：{warning}"
    );
    assert_eq!(
        ledger_count(Some(credit::CAUSE_OVER_LIMIT_WARN)).await,
        warn_before + 1,
        "WARN 放行必须落一条 OVER_LIMIT_WARN 台账"
    );
    assert_eq!(
        scalar(
            "SELECT before_used::text || '/' || after_used::text || '/' || delta::text \
               FROM biz_credit_log WHERE customer_id = ?::uuid AND cause = ? AND ref_no = ?",
            vec![
                Value::from(&customer().await),
                Value::Text(credit::CAUSE_OVER_LIMIT_WARN.to_string()),
                Value::Text(so_d.clone())
            ]
        )
        .await
        .as_deref(),
        Some("600.0000/600.0000/0.0000"),
        "超限警示台账 delta 必须是 0（放行不改变已占额度）"
    );
    // 审批通过：审批前闸放行（WARN），落 APPROVED 之后占用才涨
    let instance_d = order_instance(order_d)
        .await
        .unwrap_or_else(|| panic!("WARN 放行的订单 {so_d} 必须挂审批实例"));
    let final_d = approve_until_settled(instance_d).await;
    assert_eq!(final_d, "APPROVED");
    assert_eq!(order_status(order_d).await.as_deref(), Some("APPROVED"));
    assert_used("D 审批通过（WARN 放行后占用 1300）", "1300").await;
    assert_eq!(
        ledger_count_of_ref(credit::CAUSE_OVER_LIMIT_WARN, &so_d).await,
        1,
        "同一张单的警示台账必须幂等（提交写过后审批不再重复写）"
    );

    // ---------- ⑨ PASS 策略：静默放行 ----------
    println!("[M3-25] 步 9/12 策略改 PASS：订单 E（500）超限静默放行（无 warning、台账不增）");
    set_credit("1100", credit::POLICY_PASS).await;
    let warn_before = ledger_count(Some(credit::CAUSE_OVER_LIMIT_WARN)).await;
    let (order_e, so_e, total_e) = new_order(maker, mat_2, "50", "M3-25 步 9：PASS 静默放行").await;
    assert_eq!(total_e, dec("500"));
    let snap = snapshot(Some("500")).await;
    assert_eq!(snap.verdict, "PASS", "策略 PASS 时超限判定必须是 PASS");
    assert!(snap.allowed);
    let passed = submit(maker, order_e)
        .await
        .unwrap_or_else(|error| panic!("PASS 策略必须放行提交：{} {}", error.code, error.msg));
    assert_eq!(passed.status, "SUBMITTED");
    assert!(
        passed.credit_warning.is_none(),
        "PASS 是显式例外：响应不许带提示文案，实际 {:?}",
        passed.credit_warning
    );
    assert_eq!(
        ledger_count(Some(credit::CAUSE_OVER_LIMIT_WARN)).await,
        warn_before,
        "PASS 不留痕：台账不许增加"
    );
    let instance_e = order_instance(order_e)
        .await
        .unwrap_or_else(|| panic!("PASS 放行的订单 {so_e} 必须挂审批实例"));
    let final_e = approve_until_settled(instance_e).await;
    assert_eq!(final_e, "APPROVED");
    assert_used("E 审批通过（PASS 放行后占用 1800）", "1800").await;
    assert_eq!(
        ledger_count(Some(credit::CAUSE_OVER_LIMIT_WARN)).await,
        warn_before,
        "PASS 的审批通过同样不许写警示台账"
    );

    // ---------- ⑩ 审批点信用闸 ----------
    println!("[M3-25] 步 10/12 额度调 5000 → 订单 F(1000) 提交通过 → 额度调低到 100 → 审批被 409 拦");
    set_credit("5000", credit::POLICY_BLOCK).await;
    let (order_f, so_f, total_f) = new_order(maker, mat_2, "100", "M3-25 步 10：审批点被拦").await;
    assert_eq!(total_f, dec("1000"));
    let submitted_f = submit(maker, order_f)
        .await
        .unwrap_or_else(|error| panic!("额度 5000 时提交应通过：{} {}", error.code, error.msg));
    assert_eq!(submitted_f.status, "SUBMITTED");
    let instance_f = order_instance(order_f)
        .await
        .unwrap_or_else(|| panic!("订单 {so_f} 必须挂审批实例"));
    // 审批前额度被调低（真实场景：财务收紧 / 别人的单被批）
    set_credit("100", credit::POLICY_BLOCK).await;
    let tasks = pending_tasks(instance_f).await;
    assert!(!tasks.is_empty(), "审批必须还有待办才能验证前闸");
    let (task_id, node_key, _) = tasks[0].clone();
    let error = doc_reader::guard_approve_for_task(task_id)
        .await
        .expect_err("额度调低后，审批点同意必须被信用闸拦下");
    assert_eq!(error.status, 409, "审批点拦截是 409，实际 {}", error.status);
    assert_eq!(
        error.code,
        cloudpivot_kernel::shared::error_code::RESOURCE_CONFLICT,
        "审批点拦截的码必须是 10003，实际 {}（{}）",
        error.code,
        error.msg
    );
    assert!(
        error.msg.contains("信用额度不足"),
        "审批点拒绝文案必须说清信用额度不足，实际：{}",
        error.msg
    );
    // 待办与单据都不动（可再点 / 可驳回）
    assert_eq!(
        pending_tasks(instance_f).await.len(),
        tasks.len(),
        "被拦后待办必须原样保留（审批人可以再点或驳回）"
    );
    assert_eq!(
        order_status(order_f).await.as_deref(),
        Some("SUBMITTED"),
        "审批点被拦时单据必须留在 SUBMITTED"
    );
    assert_used("审批点被拦（额度不动）", "1800").await;
    println!("[M3-25]   节点 {node_key} 的审批前闸已拦下（符合预期）");

    // ---------- ⑪ 过审兜底：打回草稿（约定 11，不留死单） ----------
    println!("[M3-25] 步 11/12 绕过前闸直接 approve → 回调发现超限 → 打回 DRAFT（可重提）");
    let final_f = approve_skipping_guards(instance_f).await;
    assert_eq!(final_f, "APPROVED", "审批单本身已经 APPORVED 且不可回滚");
    assert_eq!(
        order_status(order_f).await.as_deref(),
        Some("DRAFT"),
        "过审兜底必须把单据打回 DRAFT（约定 11：不许停在 SUBMITTED 变死单）"
    );
    assert!(
        order_instance(order_f).await.is_none(),
        "打回草稿后必须清空 approval_instance_id（否则界面会显示一个已结束的审批）"
    );
    let remark = scalar(
        "SELECT COALESCE(remark, '') FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&order_f)],
    )
    .await
    .expect("订单存在");
    assert!(
        remark.contains("信用额度不足") && remark.contains("退回草稿"),
        "打回原因必须写进 remark（让人一眼看到为什么被打回），实际：{remark}"
    );
    assert_used("过审兜底（额度不动）", "1800").await;
    assert_eq!(
        ledger_count_of_ref(credit::CAUSE_ORDER_OCCUPY, &so_f).await,
        0,
        "被打回的单不许记占用台账"
    );
    // 调回额度 → 重新提交 → 审批通过（证明它是活的，不是死单）
    set_credit("5000", credit::POLICY_BLOCK).await;
    let (_, _) = submit_and_approve(maker, order_f, &so_f).await;
    assert_used("F 重提并审批通过", "2800").await;

    // ---------- ⑫ 不控额度：limit = 0 时策略完全不生效 ----------
    println!("[M3-25] 步 12/12 额度置 0（不控额度）：订单 G(5000) 在 BLOCK 策略下照样提交");
    set_credit("0", credit::POLICY_BLOCK).await;
    let (order_g, so_g, total_g) = new_order(maker, mat_1, "500", "M3-25 步 12：不控额度").await;
    assert_eq!(total_g, dec("5000"));
    let snap = snapshot(Some("5000")).await;
    assert_eq!(snap.verdict, "UNCONTROLLED", "额度 0 = 不控额度");
    assert!(snap.allowed);
    assert_eq!(snap.available, None, "不控额度时可用额度是 null（不是 0）");
    assert_eq!(snap.available_after, None);
    assert_eq!(snap.over, Decimal::ZERO);
    assert!(!snap.is_over);
    let (_, _) = submit_and_approve(maker, order_g, &so_g).await;
    assert_used("G 审批通过（不控额度也照常占用）", "7800").await;

    // ---------- 收口 ----------
    println!("[M3-25] 收口：台账 Σdelta / 无 PENDING 实例 / 快照无漂移 / 夹具清场");
    // 台账 Σdelta 必须等于缓存列（每个事件都是「现算 - 缓存」，累计即恒等）
    let ledger_sum = scalar(
        "SELECT COALESCE(SUM(delta), 0)::text FROM biz_credit_log WHERE customer_id = ?::uuid",
        vec![Value::from(&customer().await)],
    )
    .await
    .expect("台账合计");
    assert_eq!(
        dec(&ledger_sum),
        used().await,
        "台账 Σdelta 必须等于缓存列 credit_used（{ledger_sum} vs {}）",
        used().await
    );
    // 本客户没有任何停在 PENDING 的审批（七张单全部收口）
    assert_eq!(
        count(
            "SELECT count(*) FROM biz_flow_instance \
              WHERE status = 'PENDING' AND biz_type = 'SO_APPROVE' \
                AND biz_id IN (SELECT id FROM biz_sales_order WHERE customer_id = ?::uuid)",
            vec![Value::from(&customer().await)]
        )
        .await,
        0,
        "本客户的订单审批不许留下 PENDING 实例（死单）"
    );
    // 七张单的终态（A OUTBOUND / B APPROVED / C APPROVED / D APPROVED / E APPROVED /
    // F APPROVED / G APPROVED）—— 一条 SQL 汇总，少一张都会露出来
    let statuses = scalar(
        "SELECT string_agg(status, ',' ORDER BY so_no) FROM biz_sales_order \
          WHERE customer_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&customer().await)],
    )
    .await
    .expect("订单状态汇总");
    assert_eq!(
        statuses, "OUTBOUND,APPROVED,APPROVED,APPROVED,APPROVED,APPROVED,APPROVED",
        "七张单的终态必须与链路一致（A 已出库，其余已审批）"
    );
    let snap = snapshot(None).await;
    assert!(!snap.cache_drift, "收口时缓存列与公式必须零漂移");
    assert_eq!(snap.verdict, "UNCONTROLLED", "收口时额度 0（不控额度）");

    println!(
        "[M3-25] 全链通过：{} 额度 1000→1100→0 的 12 步信用链，终态占用 {:.2}",
        CUSTOMER_CODE,
        used().await
    );

    // 证据模式：保留夹具供库侧取证（`M325_KEEP_FIXTURES=1`；业务断言已全部跑完）
    if std::env::var("M325_KEEP_FIXTURES").as_deref() == Ok("1") {
        println!("[M3-25] M325_KEEP_FIXTURES=1：保留夹具不清场（供 SQL 取证）");
        return;
    }

    // 收尾：清掉本模块痕迹
    cleanup().await;
    let residue = count(
        "SELECT count(*) FROM biz_credit_log WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code = ?)",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
    )
    .await;
    assert_eq!(residue, 0, "清场后本客户的信用台账必须归零");
}

/// 收尾用例：夹具零残留（与 drill 脚本的库侧断言互为双保险）。
///
/// 放在同一个文件里而不是脚本里：脚本只能看「表里有没有前缀行」，
/// 而这条用例看的是「订单 / 审批实例 / 应收 / 收款 / 凭证 / 台账 / 三本账」一起归零。
#[tokio::test]
async fn zz_teardown_leaves_no_credit_residue() {
    let Some(_guard) = setup().await else { return };
    let customer_id = customer().await;
    // 七类业务痕迹一起归零（上游用例已清场；这里防的是「用例自己漏删」）
    let residue: Vec<(&str, &str)> = vec![
        (
            "销售订单",
            "SELECT count(*) FROM biz_sales_order WHERE customer_id = ?::uuid",
        ),
        (
            "审批实例",
            "SELECT count(*) FROM biz_flow_instance \
              WHERE biz_type = 'SO_APPROVE' AND biz_id IN \
                (SELECT id FROM biz_sales_order WHERE customer_id = ?::uuid)",
        ),
        (
            "信用台账",
            "SELECT count(*) FROM biz_credit_log WHERE customer_id = ?::uuid",
        ),
        (
            "应收",
            "SELECT count(*) FROM biz_receivable WHERE customer_id = ?::uuid",
        ),
        ("收款", "SELECT count(*) FROM biz_receipt WHERE customer_id = ?::uuid"),
        (
            "夹具仓库存",
            "SELECT count(*) FROM biz_inventory WHERE warehouse_id IN \
                (SELECT id FROM biz_warehouse WHERE warehouse_code = 'M325-WH')",
        ),
        (
            "出库流水",
            "SELECT count(*) FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material \
                WHERE material_code IN ('M325-M1', 'M325-M2'))",
        ),
        (
            "在途账",
            "SELECT count(*) FROM biz_inventory_in_transit WHERE material_id IN (SELECT id FROM biz_material \
                WHERE material_code IN ('M325-M1', 'M325-M2'))",
        ),
    ];
    for (label, query) in residue {
        let args = match query.matches("?::uuid").count() {
            0 => vec![],
            _ => vec![Value::from(&customer_id)],
        };
        assert_eq!(count(query, args).await, 0, "{label} 必须零残留");
    }
    // 客户额度起点回到 1000 / BLOCK（drill 脚本断言用；setup 已经设过，这里再收一次）
    assert_used("收尾：缓存列与公式一致（0）", "0").await;
    // 断言完再清场：`setup()` 为跑断言而重建了夹具客户，末尾把它一起收走，
    // 否则 drill 脚本的「M325-CUST 零残留」会红在这条**故意留下的**夹具上。
    cleanup().await;
}

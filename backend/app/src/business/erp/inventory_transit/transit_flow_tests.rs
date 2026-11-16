// ============================================================================
// transit_flow_tests.rs —— 在途库存「发货 → 签收」全流程单测（M2-06）
//
// 卡：t_b3bcd66b（M2-06【M2·仓库】在途库存：单测 / 发货→签收 全流程）
//
// # 这一份和另外三份用例的分工
//
// ```text
// shipment_posting  / shipment_receipt_tests   ← 单个写路径的**动作**（M2-02 / M2-03）
//                                                过账干了什么、签收干了什么、失败能不能重试
// transit_query_tests  / transit_reconcile_tests ← 只读接口的**形状**（M2-04 / M2-05）
//                                                直插夹具造「各种状态 × 各种日期 × 各种不平」
// 本文件（M2-06）                              ← 四张卡拼起来的**链路**
//                                                一个动作都要经 handler 走真实状态机，
//                                                每一步都用「读接口」反过来验收账本
// ```
//
// 也就是说：本文件**不重测**「过账扣不扣库存」「对账能不能报 QTY_MISMATCH」这类单点口径
// （那些归各自的卡），它钉的是**跨卡的一致性** ——
//
// * 同一笔货在三个视角（明细 `/page`、汇总 `/summary`、对账 `/reconcile`）看到的数**必须相等**；
// * 在途账与三本账（成本账 / 库位账 / 流水）在过账 / 签收两个时刻的**联动方向**不能错；
// * 在途量必须**真实占用可用量**（否则同一批货能被反复发出去）；
// * 未过账的发货单（DRAFT / PENDING_POST）**不进**在途账、也不进对账（否则满屏假红）。
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `full_flow_three_stages_stay_balanced` | 发货→签收 主链路：DRAFT→PENDING_POST→POSTED→签收，四段各查 `/page` `/summary` `/reconcile` + 三本账 |
// | `multi_material_shipment_receives_every_line` | 一张单多物料：每个物料各一行在途，签收后**逐行**清零（不能漏行） |
// | `split_shipments_accumulate_and_drain_transit` | 同一订单行分两次发货：在途累计 7（2 张单）→ 签一张剩 4 → 全签 0 |
// | `transit_holds_available_qty_so_the_same_goods_cannot_ship_twice` | 在途占用可用量：账面 10 / 在途 8 时再发 5 被拒（409 写清「在途未签收」）；补货后可重试，不留死单 |
// | `transit_and_availability_are_per_warehouse` | 可用量按仓算：A 仓有货不能发 B 仓的单；在途行落在发货仓上 |
// | `read_apis_agree_with_each_other` | 三方读数一致：明细 Σ在途 == 汇总合计 == 对账 `in_transit_qty`（签收前后各断言一次） |
// | `unposted_shipments_are_out_of_the_transit_books` | DRAFT / PENDING_POST 单：在途 0 行、`/page` 查不到、对账既不 `MISSING_TRANSIT` 也不 `untracked` |
//
// # 为什么这里全部走 handler（而不是直插在途行）
//
// M2-04 / M2-05 的用例直插是为了造「故意不平」的数据；本卡要的恰恰是**「真实的单走得通」**，
// 所以订单 / 发货单从 DRAFT 起步、每一步都用 `change_shipment_status` / `confirm_receipt`
// 推进，只有**夹具单据的建单**（订单头 + 明细 + 发货单头）用 SQL —— 那是别的卡的建单入口，
// 不是本卡要验的东西。
//
// 主数据一律走 `crate::test_seed::ensure_*`（P2-62 门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与同一批 `M206-` 夹具），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_xxx_flow" cargo test --lib \
//   business::erp::inventory_transit::transit_flow_tests -- --test-threads=1
// ```
//
// 也可直接跑 `bash scripts/verify/m206_transit_flow_drill.sh`（克隆演练库 → 补迁移 → 跑本组）。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_transit::dto::{
    TransitFilter, TransitReconcileFilter, TransitReconcileRes, TransitSummaryFilter, TransitSummaryRes,
};
use crate::business::erp::inventory_transit::handler;
use crate::business::erp::shipment::shipment_handler;
use crate::business::erp::stockin::stockin_model;
use crate::test_seed;

/// 真库用例必须串行（共用全局 `db()` 与同一批 `M206-` 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAREHOUSE_A_CODE: &str = "M206-WHA";
const WAREHOUSE_B_CODE: &str = "M206-WHB";
const MAT_A_CODE: &str = "M206-M1";
const MAT_B_CODE: &str = "M206-M2";
const CUSTOMER_CODE: &str = "M206-CUST";

/// 本模块两张夹具物料的 id 集合（清场 / 清三本账共用，按编码取，不写死 UUID）。
const MATS_SQL: &str = "(SELECT id FROM biz_material WHERE material_code IN ('M206-M1', 'M206-M2'))";

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "全流程仓甲").await
}

async fn warehouse_b() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_B_CODE, "全流程仓乙").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "全流程物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "全流程物料乙").await
}

async fn customer() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_CODE, "全流程客户").await
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

async fn exec(statement: &str, params: Vec<Value>) {
    db().execute_with_params(statement, params)
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
}

async fn sql(statement: &str) {
    db().exec_sql(statement.to_string(), vec![])
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量非法")
}

/// 可选标量（`NotFound` → `None`）。
async fn text_of(query: &str, params: Vec<Value>) -> Option<String> {
    match db().scalar_with_params::<String>(query, params).await {
        Ok(text) => Some(text),
        Err(DbError::NotFound) => None,
        Err(error) => panic!("标量查询失败：{error}\n{query}"),
    }
}

/// 标量数值（缺行 / 解析失败 → 0；用于 `COALESCE(SUM(...), 0)` 这类聚合）。
async fn num_of(query: &str, params: Vec<Value>) -> Decimal {
    text_of(query, params)
        .await
        .and_then(|text| text.trim().parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO)
}

async fn count_of(query: &str, params: Vec<Value>) -> i64 {
    db().scalar_with_params::<i64>(query, params)
        .await
        .unwrap_or_else(|error| panic!("计数查询失败：{error}\n{query}"))
}

/// 清场：只删本模块**业务单据**（`M206-` 客户下的订单 / 发货单 / 出库单 / 凭证 / 应收 / 三本账）。
///
/// 主数据（客户 / 仓库 / 物料）由 `test_seed` 负责，**不删** —— 它们是跨模块复用的夹具。
/// ⚠ 每个用例的 setup 与末尾都要调用：别的模块有断言「全表行数」的用例，夹具不能被带着跑。
async fn cleanup() {
    let orders = format!(
        "(SELECT id FROM biz_sales_order WHERE customer_id IN \
          (SELECT id FROM biz_customer WHERE code = '{CUSTOMER_CODE}'))"
    );
    let shipments = format!("(SELECT id FROM biz_shipment WHERE so_id IN {orders})");
    let stock_outs = format!("(SELECT id FROM biz_stock_out WHERE source_id IN {shipments})");
    sql(&format!(
        "DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN {stock_outs}"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id IN {stock_outs}"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_status_log WHERE \
            (entity_type = 'STOCK_OUT' AND entity_id IN {stock_outs}) \
         OR (entity_type = 'SHIPMENT' AND entity_id IN {shipments}) \
         OR (entity_type = 'SALE_ORDER' AND entity_id IN {orders})"
    ))
    .await;
    // 在途行随 biz_shipment 的 ON DELETE CASCADE 一起消失（V0420 硬外键），显式删一遍更稳
    sql(&format!(
        "DELETE FROM biz_inventory_in_transit WHERE shipment_id IN {shipments} \
            OR material_id IN {MATS_SQL}"
    ))
    .await;
    sql(&format!("DELETE FROM biz_stock_out WHERE source_id IN {shipments}")).await;
    sql(&format!("DELETE FROM biz_shipment WHERE so_id IN {orders}")).await;
    sql(&format!(
        "DELETE FROM biz_sales_order WHERE customer_id IN \
                  (SELECT id FROM biz_customer WHERE code = '{CUSTOMER_CODE}')"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_inventory_txn WHERE material_id IN {MATS_SQL}"
    ))
    .await;
    sql(&format!("DELETE FROM biz_inventory WHERE material_id IN {MATS_SQL}")).await;
    sql(&format!(
        "DELETE FROM biz_inventory_location WHERE material_id IN {MATS_SQL}"
    ))
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    // 主数据先就位（`test_seed` 幂等），再清业务单据。
    warehouse_a().await;
    warehouse_b().await;
    mat_a().await;
    mat_b().await;
    customer().await;
    cleanup().await;
    Some(guard)
}

// ==================== 夹具：单据 ====================

/// 建夹具订单：`lines` = `(物料, 数量)`，`status` 一般是 `APPROVED`（可发货）。
async fn fixture_order(lines: &[(Uuid, &str)], status: &str) -> (Uuid, Vec<Uuid>) {
    let so_id = Uuid::now_v7();
    let so_no = format!("M206-SO-{}", &so_id.to_string()[24..]);
    exec(
        "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status) \
         VALUES (?, ?, ?, CURRENT_DATE, ?)",
        vec![
            Value::from(so_id),
            Value::from(so_no),
            Value::from(customer().await),
            Value::from(status),
        ],
    )
    .await;
    let mut item_ids = Vec::with_capacity(lines.len());
    for (index, (material_id, qty)) in lines.iter().enumerate() {
        let item_id = Uuid::now_v7();
        exec(
            &format!(
                "INSERT INTO biz_sales_order_item \
                    (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
                 VALUES (?, ?, {}, ?, ?::text::numeric, 100, 0, 13, 0, 0)",
                index + 1
            ),
            vec![
                Value::from(item_id),
                Value::from(so_id),
                Value::from(*material_id),
                Value::Text(qty.to_string()),
            ],
        )
        .await;
        item_ids.push(item_id);
    }
    (so_id, item_ids)
}

/// 建夹具发货单（`status = DRAFT`，让用例自己走状态机）：`items` = `(订单行, 物料, 数量)`。
async fn fixture_shipment(so_id: Uuid, warehouse_id: Uuid, items: &[(Uuid, Uuid, &str)]) -> Uuid {
    let id = Uuid::now_v7();
    let total: Decimal = items
        .iter()
        .map(|(_, _, qty)| dec(qty))
        .fold(Decimal::ZERO, |acc, qty| acc + qty);
    exec(
        "INSERT INTO biz_shipment \
            (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty) \
         VALUES (?, ?, ?, ?, ?, CURRENT_DATE, 'DRAFT', ?::text::numeric)",
        vec![
            Value::from(id),
            Value::from(format!("M206-SH-{}", &id.to_string()[24..])),
            Value::from(so_id),
            Value::from(customer().await),
            Value::from(warehouse_id),
            Value::Text(total.to_string()),
        ],
    )
    .await;
    for (so_item_id, material_id, qty) in items {
        exec(
            "INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty) \
             VALUES (?, ?, ?, ?::text::numeric)",
            vec![
                Value::from(id),
                Value::from(*so_item_id),
                Value::from(*material_id),
                Value::Text(qty.to_string()),
            ],
        )
        .await;
    }
    id
}

// ==================== 夹具：库存 ====================

/// 造库存（成本账 + 库位账；与 `shipment_posting` 的夹具同口径，走入库侧写入口）。
async fn seed_stock(material_id: Uuid, warehouse_id: Uuid, qty: &str) {
    let now = Utc::now();
    let quantity = dec(qty);
    stockin_model::apply_location_qty(db(), material_id, warehouse_id, None, quantity, now)
        .await
        .expect("库位账加法（未分配桶）");
    stockin_model::apply_cost(
        db(),
        material_id,
        warehouse_id,
        None,
        stockin_model::CostBasis::of(quantity, quantity * dec("10")),
        now,
    )
    .await
    .expect("成本账加法");
}

// ==================== 夹具：走 handler 的动作 ====================

/// 状态流转（`PENDING_POST` / `POSTED` / `CANCELLED` 都走这一条）。
async fn set_status(ship_id: Uuid, target: &str) -> std::result::Result<shipment_handler::ShipmentRes, ApiError> {
    shipment_handler::change_shipment_status(
        None,
        &ship_id.to_string(),
        shipment_handler::ShipmentStatusReq {
            target_status: target.to_string(),
            reason: None,
        },
    )
    .await
}

/// 发货过账（`PENDING_POST → POSTED`）：在途记账 + 出库单待确认。
async fn post(ship_id: Uuid) -> std::result::Result<shipment_handler::ShipmentRes, ApiError> {
    set_status(ship_id, "POSTED").await
}

/// 签收确认（在途转已出库）。
async fn receive(ship_id: Uuid) -> std::result::Result<shipment_handler::ShipmentRes, ApiError> {
    shipment_handler::confirm_receipt(None, &ship_id.to_string(), Some("M2-06 全流程：整单签收")).await
}

// ==================== 夹具：读账本（不经过接口，直接看库） ====================

/// 一行在途账的快照（按物料升序）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct TransitSnap {
    material_id: Uuid,
    warehouse_id: Uuid,
    shipped: Decimal,
    received: Decimal,
    open: Decimal,
    status: String,
    has_received_at: bool,
}

async fn transit_rows(ship_id: Uuid) -> Vec<TransitSnap> {
    let rows = db()
        .query_sql(
            "SELECT material_id::text AS mat, warehouse_id::text AS wh, \
                    shipped_qty::text AS shipped, received_qty::text AS received, \
                    qty_in_transit::text AS open, status, \
                    (received_at IS NOT NULL)::text AS at \
               FROM biz_inventory_in_transit WHERE shipment_id = ?::uuid ORDER BY material_id"
                .to_string(),
            vec![Value::from(&ship_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查询在途行失败：{error}"));
    rows.iter()
        .map(|row| TransitSnap {
            material_id: row.get_typed::<Uuid>("mat").expect("在途行 material_id"),
            warehouse_id: row.get_typed::<Uuid>("wh").expect("在途行 warehouse_id"),
            shipped: row
                .get_typed::<String>("shipped")
                .expect("在途行 shipped")
                .parse()
                .expect("shipped 数值"),
            received: row
                .get_typed::<String>("received")
                .expect("在途行 received")
                .parse()
                .expect("received 数值"),
            open: row
                .get_typed::<String>("open")
                .expect("在途行 open")
                .parse()
                .expect("open 数值"),
            status: row.get_typed::<String>("status").expect("在途行 status"),
            has_received_at: row.get_typed::<String>("at").expect("在途行 received_at") == "true",
        })
        .collect()
}

/// 该发货单的未签收在途合计（`status = 'IN_TRANSIT'`）。
async fn open_of(ship_id: Uuid) -> Decimal {
    num_of(
        "SELECT COALESCE(SUM(qty_in_transit), 0)::text FROM biz_inventory_in_transit \
          WHERE shipment_id = ?::uuid AND status = 'IN_TRANSIT'",
        vec![Value::from(&ship_id)],
    )
    .await
}

/// 成本账结存（物料 × 仓库）。
async fn cost_on_hand(material_id: Uuid, warehouse_id: Uuid) -> Decimal {
    num_of(
        "SELECT COALESCE(qty, 0)::text FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await
}

/// 库位账结存（物料 × 仓库，所有库位合计）。
async fn location_on_hand(material_id: Uuid, warehouse_id: Uuid) -> Decimal {
    num_of(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await
}

/// 出库流水净额（OUT 记负）。
async fn out_txn_net(material_id: Uuid, warehouse_id: Uuid) -> Decimal {
    num_of(
        "SELECT COALESCE(SUM(CASE WHEN txn_type = 'OUT' THEN -qty ELSE qty END), 0)::text \
           FROM biz_inventory_txn WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await
}

/// 该发货单的关联出库单：`(id, 状态, total_qty)`。
async fn stock_out_of(ship_id: Uuid) -> Option<(Uuid, String, Decimal)> {
    let rows = db()
        .query_sql(
            "SELECT id::text AS id, status, total_qty::text AS qty FROM biz_stock_out \
              WHERE source_id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&ship_id)],
        )
        .await
        .expect("查询关联出库单");
    rows.first().map(|row| {
        (
            row.get_typed::<String>("id")
                .expect("出库单 id")
                .parse()
                .expect("出库单 id 解析"),
            row.get_typed::<String>("status").expect("出库单状态"),
            row.get_typed::<String>("qty")
                .expect("出库单数量")
                .parse()
                .expect("出库单数量解析"),
        )
    })
}

/// 该发货单的关联出库单数（断言「没生成第二张」用）。
async fn stock_out_count(ship_id: Uuid) -> i64 {
    count_of(
        "SELECT count(*) FROM biz_stock_out WHERE source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&ship_id)],
    )
    .await
}

/// 出库单的成本结转凭证张数 / 应收条数。
async fn voucher_count(stock_out_id: Uuid) -> i64 {
    count_of(
        "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
}

async fn receivable_count(stock_out_id: Uuid) -> i64 {
    count_of(
        "SELECT count(*) FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
}

/// 发货单当前状态（库里读，不信任上一次调用的返回值）。
async fn shipment_status(ship_id: Uuid) -> String {
    text_of(
        "SELECT status FROM biz_shipment WHERE id = ?::uuid",
        vec![Value::from(&ship_id)],
    )
    .await
    .unwrap_or_default()
}

async fn order_status(so_id: Uuid) -> String {
    text_of(
        "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&so_id)],
    )
    .await
    .unwrap_or_default()
}

async fn delivered_of(so_item_id: Uuid) -> Decimal {
    num_of(
        "SELECT COALESCE(delivered_qty, 0)::text FROM biz_sales_order_item WHERE id = ?::uuid",
        vec![Value::from(&so_item_id)],
    )
    .await
}

/// 在途账恒等式巡检（V0420 的生成列 + CHECK 的语义自检）：
/// `shipped = received + in_transit`；`RECEIVED` 必余量 0 且有签收时间；`IN_TRANSIT` 必有余量。
/// 返回**违反条数**（0 = 干净）。
async fn identity_violations(ship_id: Uuid) -> i64 {
    count_of(
        "SELECT count(*) FROM biz_inventory_in_transit \
          WHERE shipment_id = ?::uuid \
            AND (shipped_qty <> received_qty + qty_in_transit \
                 OR (status = 'RECEIVED' AND (qty_in_transit <> 0 OR received_at IS NULL)) \
                 OR (status = 'IN_TRANSIT' AND qty_in_transit <= 0))",
        vec![Value::from(&ship_id)],
    )
    .await
}

// ==================== 夹具：读接口（M2-04 / M2-05） ====================

async fn page_of_shipment(
    ship_id: Uuid,
    status: Option<&str>,
) -> PageData<Vec<crate::business::erp::inventory_transit::dto::TransitRes>> {
    handler::page_of(TransitFilter {
        shipment_id: Some(ship_id.to_string()),
        status: status.map(str::to_string),
        ..Default::default()
    })
    .await
    .unwrap_or_else(|error| panic!("在途明细分页失败：{} {}", error.code, error.msg))
}

async fn summary_of_material(material_id: Uuid) -> TransitSummaryRes {
    handler::summary_of(TransitSummaryFilter {
        material_id: Some(material_id.to_string()),
        ..Default::default()
    })
    .await
    .unwrap_or_else(|error| panic!("在途汇总失败：{} {}", error.code, error.msg))
}

async fn reconcile_of_shipment(ship_id: Uuid) -> TransitReconcileRes {
    handler::reconcile_of(TransitReconcileFilter {
        shipment_id: Some(ship_id.to_string()),
        include_matched: Some("1".to_string()),
        ..Default::default()
    })
    .await
    .unwrap_or_else(|error| panic!("在途对账失败：{} {}", error.code, error.msg))
}

async fn reconcile_of_material(material_id: Uuid) -> TransitReconcileRes {
    handler::reconcile_of(TransitReconcileFilter {
        material_id: Some(material_id.to_string()),
        include_matched: Some("1".to_string()),
        ..Default::default()
    })
    .await
    .unwrap_or_else(|error| panic!("在途对账失败：{} {}", error.code, error.msg))
}

/// 对账必须「对平」：零差异行、零硬问题、未截断。
fn assert_balanced(res: &TransitReconcileRes, label: &str) {
    assert!(
        res.balanced,
        "{label}：对账必须 balanced（checked={} matched={} mismatched={} diff={} truncated={} rows={:?}）",
        res.checked,
        res.matched,
        res.mismatched,
        res.totals.diff,
        res.truncated,
        res.rows.iter().map(|row| (&row.issues, row.diff)).collect::<Vec<_>>()
    );
    assert_eq!(res.mismatched, 0, "{label}：不应有硬问题组");
    assert_eq!(res.totals.diff, Decimal::ZERO, "{label}：三侧差额必须为 0");
    assert!(res.rows.iter().all(|row| row.matched), "{label}：每一组都应 matched");
}

// ==================== 用例 ====================

/// **主链路：发货 → 签收，四段各有账**。
///
/// ```text
/// 段① DRAFT          在途 0 行 / 账面 20 / 无出库单
/// 段② PENDING_POST   在途 0 行 / 账面 20 / 无出库单（还没发货，只占着单）
/// 段③ POSTED         在途 6   / 账面 20（一分没扣）/ 出库单 PENDING_POST / 无凭证无应收
/// 段④ 签收           在途 0   / 账面 14 / 出库单 POSTED / 凭证 1 张 + 应收 1 条
/// ```
///
/// 每段都用 `/page`、`/summary`、`/reconcile` 三个只读面反查一遍 —— 写路径说「记了」不算，
/// 读接口能看见才算账。
#[tokio::test]
async fn full_flow_three_stages_stay_balanced() {
    let Some(_guard) = setup().await else { return };
    let (wh, mat) = (warehouse_a().await, mat_a().await);
    seed_stock(mat, wh, "20").await;
    let (so_id, items) = fixture_order(&[(mat, "6")], "APPROVED").await;
    let ship_id = fixture_shipment(so_id, wh, &[(items[0], mat, "6")]).await;

    // ---------- 段① DRAFT：还没提交，账上什么都不该有 ----------
    assert!(transit_rows(ship_id).await.is_empty(), "DRAFT 不该有在途行");
    assert_eq!(page_of_shipment(ship_id, None).await.total, 0, "DRAFT 单查不到在途明细");
    let rec = reconcile_of_shipment(ship_id).await;
    assert_eq!(rec.checked, 0, "DRAFT 单不进对账");
    assert_balanced(&rec, "DRAFT");

    // ---------- 段② PENDING_POST：提交待过账，仍然只是「单」不是「货」 ----------
    set_status(ship_id, "PENDING_POST")
        .await
        .unwrap_or_else(|error| panic!("提交待过账应成功：{} {}", error.code, error.msg));
    assert_eq!(shipment_status(ship_id).await, "PENDING_POST");
    assert!(transit_rows(ship_id).await.is_empty(), "PENDING_POST 不该有在途行");
    assert_eq!(stock_out_count(ship_id).await, 0, "没过账就没有出库单");
    assert_eq!(cost_on_hand(mat, wh).await, dec("20"), "账面不动");
    let rec = reconcile_of_shipment(ship_id).await;
    assert_eq!(rec.checked, 0, "PENDING_POST 单不进对账（否则会满屏 MISSING_TRANSIT）");
    assert_balanced(&rec, "PENDING_POST");

    // ---------- 段③ POSTED：货在路上，库存一分没扣 ----------
    post(ship_id)
        .await
        .unwrap_or_else(|error| panic!("发货过账应成功：{} {}", error.code, error.msg));
    assert_eq!(shipment_status(ship_id).await, "POSTED");

    let rows = transit_rows(ship_id).await;
    assert_eq!(rows.len(), 1, "单物料一张单 = 一行在途");
    assert_eq!(
        rows[0],
        TransitSnap {
            material_id: mat,
            warehouse_id: wh,
            shipped: dec("6"),
            received: Decimal::ZERO,
            open: dec("6"),
            status: "IN_TRANSIT".to_string(),
            has_received_at: false,
        },
        "过账后：已发 6 / 已签 0 / 在途 6 / IN_TRANSIT / 无签收时间"
    );
    assert_eq!(cost_on_hand(mat, wh).await, dec("20"), "过账不扣库存（M2-02 两段式）");
    assert_eq!(location_on_hand(mat, wh).await, dec("20"), "库位账同样不动");
    assert_eq!(out_txn_net(mat, wh).await, Decimal::ZERO, "还没出库，不该有 OUT 流水");
    let (stock_out_id, stock_out_status, stock_out_qty) = stock_out_of(ship_id).await.expect("过账必须生成销售出库单");
    assert_eq!(stock_out_status, "PENDING_POST", "出库单停在「出库待确认」");
    assert_eq!(stock_out_qty, dec("6"));
    assert_eq!(voucher_count(stock_out_id).await, 0, "签收前没有成本结转凭证");
    assert_eq!(receivable_count(stock_out_id).await, 0, "签收前没有应收");
    assert_eq!(delivered_of(items[0]).await, dec("6"), "过账即回写订单行 delivered_qty");
    assert_eq!(order_status(so_id).await, "OUTBOUND", "发满 6/6 → 订单已出库");

    // 读面：明细 1 行（名字批量补齐），汇总 6，对账对平
    let page = page_of_shipment(ship_id, None).await;
    assert_eq!(page.total, 1);
    let row = &page.list[0];
    assert_eq!(row.qty_in_transit, dec("6"), "在途量取 PG 生成列");
    assert_eq!(row.received_qty, Decimal::ZERO);
    assert_eq!(row.status, "IN_TRANSIT");
    assert_eq!(row.material_code.as_deref(), Some(MAT_A_CODE), "物料编码必须下发");
    assert_eq!(
        row.warehouse_code.as_deref(),
        Some(WAREHOUSE_A_CODE),
        "仓库编码必须下发"
    );
    assert_eq!(row.customer_code.as_deref(), Some(CUSTOMER_CODE), "客户编码必须下发");
    assert!(row.ship_no.is_some(), "发货单号必须下发");

    let summary = summary_of_material(mat).await;
    assert_eq!(summary.row_count, 1, "物料甲 × 仓甲 一组");
    assert_eq!(summary.total_qty_in_transit, dec("6"));
    assert_eq!(summary.total_shipments, 1);
    assert_eq!(summary.total_lines, 1);

    let rec = reconcile_of_shipment(ship_id).await;
    assert_eq!(rec.checked, 1, "已过账单进入对账");
    assert_eq!(rec.totals.in_transit_qty, dec("6"));
    assert_eq!(rec.totals.received_qty, Decimal::ZERO);
    assert_eq!(rec.totals.item_qty, dec("6"), "发货总量 = Σ 明细 qty（SSOT）");
    assert_balanced(&rec, "过账后");

    // ---------- 段④ 签收：在途清零、库存扣减、凭证与应收产生 ----------
    let res = receive(ship_id)
        .await
        .unwrap_or_else(|error| panic!("签收确认应成功：{} {}", error.code, error.msg));
    assert_eq!(res.status, "POSTED", "签收不改发货单状态（已发货就是 POSTED）");
    assert_eq!(shipment_status(ship_id).await, "POSTED");

    let rows = transit_rows(ship_id).await;
    assert_eq!(rows.len(), 1, "签收不新增在途行，只把原行签满");
    assert_eq!(
        rows[0],
        TransitSnap {
            material_id: mat,
            warehouse_id: wh,
            shipped: dec("6"),
            received: dec("6"),
            open: Decimal::ZERO,
            status: "RECEIVED".to_string(),
            has_received_at: true,
        },
        "签收后：已发 6 / 已签 6 / 在途 0 / RECEIVED / 有签收时间"
    );
    assert_eq!(cost_on_hand(mat, wh).await, dec("14"), "成本账 20 − 6");
    assert_eq!(location_on_hand(mat, wh).await, dec("14"), "库位账 20 − 6");
    assert_eq!(out_txn_net(mat, wh).await, dec("-6"), "留一条 −6 的 OUT 流水");
    let (_, stock_out_status, _) = stock_out_of(ship_id).await.expect("出库单还在");
    assert_eq!(stock_out_status, "POSTED", "签收把出库单过账");
    assert_eq!(voucher_count(stock_out_id).await, 1, "签收才生成成本结转凭证");
    assert_eq!(receivable_count(stock_out_id).await, 1, "签收才挂应收");

    // 读面：默认口径（只看 IN_TRANSIT）为空；显式 RECEIVED 能看到那一条历史
    assert_eq!(open_of(ship_id).await, Decimal::ZERO, "在途归零");
    assert_eq!(
        page_of_shipment(ship_id, None).await.total,
        0,
        "默认「看在途」查不到已签收的行"
    );
    let received = page_of_shipment(ship_id, Some("RECEIVED")).await;
    assert_eq!(received.total, 1, "显式点名 RECEIVED 才看得到");
    assert_eq!(received.list[0].qty_in_transit, Decimal::ZERO);
    assert_eq!(received.list[0].status_label, "已签收");

    let summary = summary_of_material(mat).await;
    assert_eq!(summary.row_count, 0, "签收后汇总无在途组");
    assert_eq!(summary.total_qty_in_transit, Decimal::ZERO);

    let rec = reconcile_of_shipment(ship_id).await;
    assert_eq!(rec.checked, 1);
    assert_eq!(rec.totals.in_transit_qty, Decimal::ZERO, "在途 0");
    assert_eq!(rec.totals.received_qty, dec("6"), "已出 = 发货总量");
    assert_balanced(&rec, "签收后");

    // ---------- 全程恒等式（V0420 生成列 + CHECK 的语义自检） ----------
    assert_eq!(
        identity_violations(ship_id).await,
        0,
        "全程 shipped = received + in_transit"
    );

    cleanup().await;
    println!("[M2-06] 主链路：在途 0→6→0，库存 20→20→14，对账两段 balanced");
}

/// **多物料一张单：每个物料各一行在途，签收逐行清零。**
///
/// 一张发货单两行物料（甲 5 / 乙 3）：过账后在途 2 行、汇总 2 组；
/// 签收后两行都 `RECEIVED`、余量 0，库存分别 10→5 / 10→7。
#[tokio::test]
async fn multi_material_shipment_receives_every_line() {
    let Some(_guard) = setup().await else { return };
    let (wh, ma, mb) = (warehouse_a().await, mat_a().await, mat_b().await);
    seed_stock(ma, wh, "10").await;
    seed_stock(mb, wh, "10").await;
    let (so_id, items) = fixture_order(&[(ma, "5"), (mb, "3")], "APPROVED").await;
    let ship_id = fixture_shipment(so_id, wh, &[(items[0], ma, "5"), (items[1], mb, "3")]).await;

    set_status(ship_id, "PENDING_POST").await.expect("提交待过账");
    post(ship_id).await.expect("发货过账");

    let rows = transit_rows(ship_id).await;
    assert_eq!(rows.len(), 2, "两个物料 = 两行在途（不能合并成一行）");
    let open_total: Decimal = rows.iter().map(|row| row.open).sum();
    assert_eq!(open_total, dec("8"), "在途合计 5 + 3");
    assert!(
        rows.iter()
            .all(|row| row.status == "IN_TRANSIT" && row.open > Decimal::ZERO),
        "两行都必须 IN_TRANSIT 且有余量"
    );
    assert_eq!(cost_on_hand(ma, wh).await, dec("10"), "甲库存不动");
    assert_eq!(cost_on_hand(mb, wh).await, dec("10"), "乙库存不动");
    let (_, _, stock_out_qty) = stock_out_of(ship_id).await.expect("出库单");
    assert_eq!(stock_out_qty, dec("8"), "出库单按整单合计 8");

    let page = page_of_shipment(ship_id, None).await;
    assert_eq!(page.total, 2, "明细两行");
    let summary = summary_of_material(ma).await;
    assert_eq!(summary.row_count, 1, "物料甲 × 仓甲 一组");
    assert_eq!(summary.total_qty_in_transit, dec("5"));
    let summary = summary_of_material(mb).await;
    assert_eq!(summary.total_qty_in_transit, dec("3"));

    let rec = reconcile_of_shipment(ship_id).await;
    assert_eq!(rec.checked, 2, "对账粒度 = 发货单 × 物料，两组");
    assert_eq!(rec.totals.item_qty, dec("8"));
    assert_balanced(&rec, "多物料过账后");

    receive(ship_id).await.expect("签收确认");

    let rows = transit_rows(ship_id).await;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|row| row.status == "RECEIVED" && row.open == Decimal::ZERO && row.has_received_at),
        "两行都要签满 + RECEIVED + 有签收时间，实际 {rows:?}"
    );
    assert_eq!(cost_on_hand(ma, wh).await, dec("5"), "甲 10 − 5");
    assert_eq!(cost_on_hand(mb, wh).await, dec("7"), "乙 10 − 3");
    assert_eq!(location_on_hand(ma, wh).await, dec("5"));
    assert_eq!(location_on_hand(mb, wh).await, dec("7"));
    assert_eq!(out_txn_net(ma, wh).await, dec("-5"));
    assert_eq!(out_txn_net(mb, wh).await, dec("-3"));
    assert_eq!(page_of_shipment(ship_id, None).await.total, 0, "签收后无在途");
    assert_eq!(
        reconcile_of_shipment(ship_id).await.totals.in_transit_qty,
        Decimal::ZERO
    );

    let rec = reconcile_of_shipment(ship_id).await;
    assert_eq!(rec.totals.received_qty, dec("8"), "已出 = 8");
    assert_balanced(&rec, "多物料签收后");
    assert_eq!(identity_violations(ship_id).await, 0);

    cleanup().await;
    println!("[M2-06] 多物料：在途 8（2 行）→ 0，库存甲 −5 / 乙 −3");
}

/// **同一订单行分两次发货：在途累计、逐张签收递减。**
///
/// 订单行 7，两张发货单各 3 / 4：过账后 2 行在途合计 7（`shipments = 2`）；
/// 签第一张 → 剩 4；签第二张 → 0；全程对账 `balanced`。
#[tokio::test]
async fn split_shipments_accumulate_and_drain_transit() {
    let Some(_guard) = setup().await else { return };
    let (wh, mat) = (warehouse_a().await, mat_a().await);
    seed_stock(mat, wh, "20").await;
    let (so_id, items) = fixture_order(&[(mat, "7")], "APPROVED").await;
    let ship1 = fixture_shipment(so_id, wh, &[(items[0], mat, "3")]).await;
    let ship2 = fixture_shipment(so_id, wh, &[(items[0], mat, "4")]).await;

    for (ship, qty) in [(ship1, "3"), (ship2, "4")] {
        set_status(ship, "PENDING_POST").await.expect("提交待过账");
        post(ship)
            .await
            .unwrap_or_else(|error| panic!("过账 {qty} 应成功：{} {}", error.code, error.msg));
    }

    assert_eq!(open_of(ship1).await, dec("3"));
    assert_eq!(open_of(ship2).await, dec("4"));
    assert_eq!(
        open_of(ship1).await + open_of(ship2).await,
        dec("7"),
        "在途累计 = 两次发货之和"
    );
    assert_eq!(cost_on_hand(mat, wh).await, dec("20"), "两张单都过账也一分不扣");
    assert_eq!(delivered_of(items[0]).await, dec("7"), "delivered_qty 累计 7");
    assert_eq!(order_status(so_id).await, "OUTBOUND", "发满 7/7");

    let summary = summary_of_material(mat).await;
    assert_eq!(summary.row_count, 1, "物料甲 × 仓甲 合并成一组");
    assert_eq!(summary.total_qty_in_transit, dec("7"));
    assert_eq!(summary.total_shipments, 2, "组内两家发货单");
    assert_eq!(summary.total_lines, 2);

    let rec = reconcile_of_material(mat).await;
    assert_eq!(rec.checked, 2, "对账按 发货单 × 物料 分组，两组");
    assert_eq!(rec.totals.in_transit_qty, dec("7"));
    assert_balanced(&rec, "两张单都在途");

    // 签第一张：剩 4
    receive(ship1).await.expect("签收第一张");
    assert_eq!(open_of(ship1).await, Decimal::ZERO);
    assert_eq!(open_of(ship2).await, dec("4"), "另一张不受影响");
    assert_eq!(cost_on_hand(mat, wh).await, dec("17"), "扣掉先签的那批 3");
    let summary = summary_of_material(mat).await;
    assert_eq!(summary.total_qty_in_transit, dec("4"));
    assert_eq!(summary.total_shipments, 1, "只剩一张还在途");
    let rec = reconcile_of_material(mat).await;
    assert_eq!(rec.totals.received_qty, dec("3"));
    assert_eq!(rec.totals.in_transit_qty, dec("4"));
    assert_balanced(&rec, "签第一张后");

    // 签第二张：清零
    receive(ship2).await.expect("签收第二张");
    assert_eq!(open_of(ship2).await, Decimal::ZERO);
    assert_eq!(cost_on_hand(mat, wh).await, dec("13"), "20 − 3 − 4");
    assert_eq!(out_txn_net(mat, wh).await, dec("-7"), "两条 OUT 流水合计 −7");
    assert_eq!(summary_of_material(mat).await.total_qty_in_transit, Decimal::ZERO);
    let rec = reconcile_of_material(mat).await;
    assert_eq!(rec.totals.in_transit_qty, Decimal::ZERO);
    assert_eq!(rec.totals.received_qty, dec("7"), "已出 = 发货总量 7");
    assert_balanced(&rec, "全部签收后");
    assert_eq!(identity_violations(ship1).await + identity_violations(ship2).await, 0);

    cleanup().await;
    println!("[M2-06] 分两次发货：在途 3+4=7 → 签一张剩 4 → 全签 0，库存 20→13");
}

/// **在途占用可用量：同一批货不能被反复发出去。**
///
/// 账面 10、先发 8 过账（在途 8）→ 再发 5 时可用量只有 2，过账必须被拒（409，文案写明
/// 「在途未签收」），且**不留中间态**（发货单仍 PENDING_POST、在途仍 8、库存仍 10、无第二张出库单）。
/// 补货 3 后可用 5 → 同一张单直接重试成功（AGENTS.md 约定 11：不需要改库）。
#[tokio::test]
async fn transit_holds_available_qty_so_the_same_goods_cannot_ship_twice() {
    let Some(_guard) = setup().await else { return };
    let (wh, mat) = (warehouse_a().await, mat_a().await);
    seed_stock(mat, wh, "10").await;
    let (so_id, items) = fixture_order(&[(mat, "15")], "APPROVED").await;
    let ship1 = fixture_shipment(so_id, wh, &[(items[0], mat, "8")]).await;
    let ship2 = fixture_shipment(so_id, wh, &[(items[0], mat, "5")]).await;

    set_status(ship1, "PENDING_POST").await.expect("提交待过账");
    post(ship1).await.expect("第一批 8 应能过账");
    assert_eq!(open_of(ship1).await, dec("8"));

    set_status(ship2, "PENDING_POST").await.expect("提交待过账");
    let error = post(ship2).await.expect_err("可用量只剩 2，发 5 必须被拒");
    assert_eq!(error.status, 409, "库存不足应 409，实际 {} {}", error.status, error.msg);
    assert!(
        error.msg.contains("在途未签收") || error.msg.contains("可用"),
        "文案必须解释「可用量 = 账面 − 在途未签收」，实际：{}",
        error.msg
    );

    // 不留死单：被拒的单原样停在 PENDING_POST，账上一分没动
    assert_eq!(
        shipment_status(ship2).await,
        "PENDING_POST",
        "被拒后仍是待过账，可直接重试"
    );
    assert_eq!(open_of(ship2).await, Decimal::ZERO, "被拒的单不该留下半条在途");
    assert_eq!(open_of(ship1).await, dec("8"), "已过账的在途不受影响");
    assert_eq!(cost_on_hand(mat, wh).await, dec("10"), "库存仍未扣");
    assert_eq!(stock_out_count(ship2).await, 0, "被拒的过账不生成出库单");

    // 补货 3 → 可用 13 − 8 = 5，重试成功
    seed_stock(mat, wh, "3").await;
    post(ship2)
        .await
        .unwrap_or_else(|error| panic!("补货后重试应成功：{} {}", error.code, error.msg));
    assert_eq!(open_of(ship2).await, dec("5"));
    assert_eq!(
        open_of(ship1).await + open_of(ship2).await,
        dec("13"),
        "在途合计 8 + 5，与账面 13 相等（没有超发）"
    );

    let rec = reconcile_of_material(mat).await;
    assert_eq!(rec.checked, 2);
    assert_eq!(rec.totals.in_transit_qty, dec("13"));
    assert_balanced(&rec, "补货后两张单都在途");

    receive(ship1).await.expect("签收第一批");
    assert_eq!(cost_on_hand(mat, wh).await, dec("5"), "13 − 8");
    assert_eq!(open_of(ship2).await, dec("5"), "第二批仍在途");
    assert_eq!(identity_violations(ship1).await + identity_violations(ship2).await, 0);

    cleanup().await;
    println!("[M2-06] 在途占可用量：账面 10 / 在途 8 时发 5 被拒 409；补货 3 后重试成功");
}

/// **可用量按仓算：A 仓有货不能顶 B 仓的单。**
///
/// 甲在仓甲有 10、仓乙 0。从仓乙发 5 → 过账 409（可用 0），不留单据中间态；
/// 从仓甲发 5 → 成功，在途行落在**发货仓甲**；签收后仓甲剩 5、仓乙仍是 0。
#[tokio::test]
async fn transit_and_availability_are_per_warehouse() {
    let Some(_guard) = setup().await else { return };
    let (wa, wb, mat) = (warehouse_a().await, warehouse_b().await, mat_a().await);
    seed_stock(mat, wa, "10").await;
    let (so_id, items) = fixture_order(&[(mat, "10")], "APPROVED").await;
    let ship_b = fixture_shipment(so_id, wb, &[(items[0], mat, "5")]).await;
    let ship_a = fixture_shipment(so_id, wa, &[(items[0], mat, "5")]).await;

    set_status(ship_b, "PENDING_POST").await.expect("提交待过账");
    let error = post(ship_b).await.expect_err("仓乙没货，不能过账");
    assert_eq!(error.status, 409, "实际 {} {}", error.status, error.msg);
    assert_eq!(shipment_status(ship_b).await, "PENDING_POST", "被拒的单仍可重试");
    assert!(transit_rows(ship_b).await.is_empty(), "被拒不过账 → 无在途行");
    assert_eq!(stock_out_count(ship_b).await, 0);
    let rec = reconcile_of_shipment(ship_b).await;
    assert_eq!(rec.checked, 0, "没过账的单不进对账（也不会报 MISSING_TRANSIT 假红）");
    assert_balanced(&rec, "仓乙被拒后");

    set_status(ship_a, "PENDING_POST").await.expect("提交待过账");
    post(ship_a).await.expect("仓甲有货，应能过账");
    let rows = transit_rows(ship_a).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].warehouse_id, wa, "在途行记在**发货仓**（不是收货方所在地）");
    assert_eq!(rows[0].open, dec("5"));

    let page = page_of_shipment(ship_a, None).await;
    assert_eq!(page.list[0].warehouse_code.as_deref(), Some(WAREHOUSE_A_CODE));

    receive(ship_a).await.expect("签收");
    assert_eq!(cost_on_hand(mat, wa).await, dec("5"), "仓甲 10 − 5");
    assert_eq!(cost_on_hand(mat, wb).await, Decimal::ZERO, "仓乙始终为 0");
    assert_eq!(location_on_hand(mat, wa).await, dec("5"));
    assert_eq!(summary_of_material(mat).await.total_qty_in_transit, Decimal::ZERO);
    assert_eq!(identity_violations(ship_a).await, 0);

    cleanup().await;
    println!("[M2-06] 按仓算可用量：仓乙 0 库存过账被拒，仓甲 10 → 5");
}

/// **三方读数一致：明细 Σ在途 == 汇总合计 == 对账 `in_transit_qty`。**
///
/// 两个物料 × 两张发货单 = 4 行在途（14 件）。三个接口是三条 SQL，口径跑偏时
/// 「明细 14 / 汇总 12 / 对账 14」这种不一致只能靠这条断言抓出来。
/// 签收其中一张后（剩 6 件）再比对一次 —— 单次相等可能只是巧合。
#[tokio::test]
async fn read_apis_agree_with_each_other() {
    let Some(_guard) = setup().await else { return };
    let (wh, ma, mb) = (warehouse_a().await, mat_a().await, mat_b().await);
    seed_stock(ma, wh, "10").await;
    seed_stock(mb, wh, "10").await;
    let (so1, items1) = fixture_order(&[(ma, "5"), (mb, "3")], "APPROVED").await;
    let (so2, items2) = fixture_order(&[(ma, "2"), (mb, "4")], "APPROVED").await;
    let ship1 = fixture_shipment(so1, wh, &[(items1[0], ma, "5"), (items1[1], mb, "3")]).await;
    let ship2 = fixture_shipment(so2, wh, &[(items2[0], ma, "2"), (items2[1], mb, "4")]).await;
    for ship in [ship1, ship2] {
        set_status(ship, "PENDING_POST").await.expect("提交待过账");
        post(ship).await.expect("发货过账");
    }

    // 阶段一：4 行在途 / 14 件
    let page_total = page_of_shipment(ship1, None).await.total + page_of_shipment(ship2, None).await.total;
    assert_eq!(page_total, 4, "两张单 × 两个物料 = 4 行");

    let page_sum: Decimal = {
        let mut sum = Decimal::ZERO;
        for ship in [ship1, ship2] {
            for row in page_of_shipment(ship, None).await.list {
                sum += row.qty_in_transit;
            }
        }
        sum
    };
    let summary_sum =
        summary_of_material(ma).await.total_qty_in_transit + summary_of_material(mb).await.total_qty_in_transit;
    let rec_sum =
        reconcile_of_material(ma).await.totals.in_transit_qty + reconcile_of_material(mb).await.totals.in_transit_qty;

    assert_eq!(page_sum, dec("14"), "明细逐行相加 = 5+3+2+4");
    assert_eq!(summary_sum, page_sum, "汇总合计必须等于明细之和");
    assert_eq!(rec_sum, page_sum, "对账在途合计必须等于明细之和");
    assert_eq!(summary_of_material(ma).await.total_shipments, 2, "物料甲横跨两张发货单");

    // 阶段二：签掉 ship1（8 件），三方读数一起降到 6
    receive(ship1).await.expect("签收第一张");
    let page_sum: Decimal = {
        let mut sum = Decimal::ZERO;
        for ship in [ship1, ship2] {
            for row in page_of_shipment(ship, None).await.list {
                sum += row.qty_in_transit;
            }
        }
        sum
    };
    let summary_sum =
        summary_of_material(ma).await.total_qty_in_transit + summary_of_material(mb).await.total_qty_in_transit;
    let rec_sum =
        reconcile_of_material(ma).await.totals.in_transit_qty + reconcile_of_material(mb).await.totals.in_transit_qty;
    assert_eq!(page_sum, dec("6"), "签掉 8 件，剩 2+4");
    assert_eq!(summary_sum, page_sum, "签收后汇总仍要等于明细之和");
    assert_eq!(rec_sum, page_sum, "签收后对账仍要等于明细之和");

    let rec = reconcile_of_material(ma).await;
    assert_eq!(rec.checked, 2, "物料甲两张单两组");
    assert_balanced(&rec, "读数一致用例");

    cleanup().await;
    println!("[M2-06] 三方读数一致：明细=汇总=对账 14 → 签一张后同为 6");
}

/// **未过账的发货单不进在途账，也不进对账。**
///
/// 同一物料同时挂三张单：一张已过账（4）、一张 PENDING_POST（2）、一张 DRAFT（3）。
/// 在途账只有 4；`/page` 只有 1 行；对账 `checked = 1`、`untracked = 0`、`mismatched = 0` ——
/// 未过账的单**不能**以 `MISSING_TRANSIT` 出现（否则每张草稿单都是一条假红）。
#[tokio::test]
async fn unposted_shipments_are_out_of_the_transit_books() {
    let Some(_guard) = setup().await else { return };
    let (wh, mat) = (warehouse_a().await, mat_a().await);
    seed_stock(mat, wh, "10").await;
    let (so_id, items) = fixture_order(&[(mat, "9")], "APPROVED").await;
    let ship_posted = fixture_shipment(so_id, wh, &[(items[0], mat, "4")]).await;
    let ship_pending = fixture_shipment(so_id, wh, &[(items[0], mat, "2")]).await;
    let ship_draft = fixture_shipment(so_id, wh, &[(items[0], mat, "3")]).await;

    set_status(ship_posted, "PENDING_POST").await.expect("提交待过账");
    post(ship_posted).await.expect("只有这一张过账");
    set_status(ship_pending, "PENDING_POST")
        .await
        .expect("第二张停在待过账");

    assert_eq!(transit_rows(ship_posted).await.len(), 1, "过账单有 1 行在途");
    assert!(transit_rows(ship_pending).await.is_empty(), "PENDING_POST 单无在途行");
    assert!(transit_rows(ship_draft).await.is_empty(), "DRAFT 单无在途行");
    assert_eq!(open_of(ship_posted).await, dec("4"));
    assert_eq!(
        open_of(ship_posted).await + open_of(ship_pending).await + open_of(ship_draft).await,
        dec("4"),
        "在途只认已过账的那些货"
    );

    assert_eq!(page_of_shipment(ship_posted, None).await.total, 1);
    assert_eq!(page_of_shipment(ship_pending, None).await.total, 0);
    assert_eq!(page_of_shipment(ship_draft, None).await.total, 0);

    let rec = reconcile_of_material(mat).await;
    assert_eq!(rec.checked, 1, "对账只比对已过账单");
    assert_eq!(rec.mismatched, 0, "未过账单不许报差异");
    assert_eq!(rec.totals.untracked, 0, "未过账单不该进 untracked（明细侧就排除了）");
    assert_eq!(rec.totals.item_qty, dec("4"), "对账的 SSOT 只含已过账单的明细");
    assert!(
        rec.rows
            .iter()
            .all(|row| !row.issues.contains(&"MISSING_TRANSIT".to_string())),
        "不能有 MISSING_TRANSIT 假红"
    );
    assert_balanced(&rec, "有无过账单混在一起");

    // 那两张单过账后各自进账，在途应该变成 4 + 2 + 3
    post(ship_pending).await.expect("第二张过账");
    set_status(ship_draft, "PENDING_POST").await.expect("第三张提交");
    post(ship_draft).await.expect("第三张过账");
    assert_eq!(summary_of_material(mat).await.total_qty_in_transit, dec("9"));
    let rec = reconcile_of_material(mat).await;
    assert_eq!(rec.checked, 3, "三张单都进了对账");
    assert_balanced(&rec, "三张单全部过账后");
    assert_eq!(
        identity_violations(ship_posted).await
            + identity_violations(ship_pending).await
            + identity_violations(ship_draft).await,
        0
    );

    cleanup().await;
    println!("[M2-06] 未过账单不入账：在途 4（过账）→ 9（三张全部过账），全程无假红");
}

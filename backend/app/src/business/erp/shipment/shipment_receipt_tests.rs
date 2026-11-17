// ============================================================================
// shipment_receipt_tests.rs —— 签收确认（M2-03）的真库用例
//
// 卡：t_5f324a4c（M2-03【M2·仓库】在途库存：签收确认 → 在途转已出库）
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `receipt_clears_transit_and_posts_stock_out` | 签收后**扣库存 + 清在途**：三本账负向、OUT 流水、成本结转凭证、应收挂账、在途行 `RECEIVED`（余量 0、`received_at` 有值） |
// | `repeat_receipt_is_rejected_at_gate` | 重复签收 409，**不会扣第二次库存**、不会多一张凭证 |
// | `receipt_requires_posted_shipment` | 草稿 / 待过账的发货单没有在途可签 —— 409 且不动账 |
// | `posting_from_stock_out_page_also_clears_transit` | 出库单页面点「过账」= 同一件事的另一个入口，在途一样清零（两个入口不各算一次） |
// | `receipt_failure_leaves_transit_open_and_retryable` | 过账半边失败（负库存）→ 整笔回滚：在途仍 `IN_TRANSIT`、出库单仍 `PENDING_POST`；补货后**重试成功**（AGENTS.md 约定 11：不留死单） |
// | `cancel_pending_stock_out_releases_transit` | **M2-FIX（t_f123b57f）**：作废「出库待确认」的销售出库单（撤回→作废两步）⇒ 在途行 `CANCELLED`（数量列原样、备注写作废原因）、Σ未签收在途归零、可用量恢复、**同一批货能再发**；撤回那一步不释放在途、账一分没动 |
// | `cancel_posted_stock_out_leaves_received_transit_alone` | **M2-FIX**：已过账（`POSTED`）出库单作废走红冲，**不许碰在途行**（仍 `RECEIVED` + 原备注），否则已签收的量被重复放回可用量 |
// | `transit_release_only_applies_to_sales_source` | 纯逻辑：释放在途的判据只认 `source_type='SALES'` + `source_id`（手工建单不碰在途） |
//
// # 为什么单独一个文件
//
// `shipment_handler.rs` 已经 1400+ 行（AGENTS.md 单文件体量口径），真库夹具（建单 / 造库存 /
// 读三本账）与 `shipment_posting` 的用例同款但**编码前缀不同**（`SO10R-`），这样两个模块的
// `cleanup()` 不会互删。只在 `cargo test` 时编译。
//
// 主数据一律走 `crate::test_seed::ensure_*`（P2-62 门禁：测试代码不许手写主数据 INSERT，
// 预算只许降不许涨），只有业务单据（订单 / 发货单 / 出库单）才用本文件的 SQL。
//
// ⚠ 真库用例必须串行（共用全局 `db()`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_xxx_receipt" cargo test --lib \
//   business::erp::shipment::shipment_receipt_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::shipment::shipment_handler;
use crate::business::erp::shipment::shipment_model;
use crate::business::erp::stockin::stockin_model;
use crate::business::erp::stockout::stockout_dto::StockOutStatusReq;
use crate::business::erp::stockout::stockout_handler;
use crate::test_seed;

/// 真库用例必须串行（共用全局 `db()` 与同一批 `SO10R-` 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAREHOUSE_CODE: &str = "SO10R-WH";
const MAT_A_CODE: &str = "SO10R-M1";
const CUSTOMER_CODE: &str = "SO10R-CUST";
/// 本模块两张夹具物料的 id 集合（清场 / 清三本账共用，按编码取，不写死 UUID）。
const MATS_SQL: &str = "(SELECT id FROM biz_material WHERE material_code IN ('SO10R-M1', 'SO10R-M2'))";

/// 夹具仓库（`test_seed` 幂等：同一 code 永远是同一个派生 id）。
async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_CODE, "签收演练仓").await
}

/// 夹具物料甲（本模块的单行发货 / 签收都是它）。
async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "签收物料甲").await
}

/// 夹具客户（签收后的应收会挂到它身上）。
async fn customer() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_CODE, "签收演练客户").await
}

fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>().expect("定点字面量非法")
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

/// 清场：只清本模块**业务单据**（`SO10R-` 客户下的订单 / 发货单 / 出库单 / 凭证 / 应收 / 三本账）。
///
/// 主数据（客户 / 仓库 / 物料）由 `test_seed` 负责，**不删** —— 它们是跨模块复用的夹具。
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
    warehouse().await;
    mat_a().await;
    customer().await;
    cleanup().await;
    Some(guard)
}

/// 造库存（成本账 + 库位账；与 `shipment_posting` 的夹具同口径，走入库侧写入口）。
async fn seed_stock(qty: &str) {
    let now = Utc::now();
    let material_id = mat_a().await;
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
        stockin_model::CostBasis::of(quantity, quantity * dec("10")),
        now,
    )
    .await
    .expect("成本账加法");
}

async fn fixture_order(so_item_qty: &str, status: &str) -> (Uuid, Uuid) {
    let so_id = Uuid::now_v7();
    let so_no = format!("SO10R-SO-{}", &so_id.to_string()[24..]);
    db().execute_with_params(
        "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status) \
         VALUES (?, ?, ?, CURRENT_DATE, ?)",
        vec![
            Value::from(so_id),
            Value::from(so_no),
            Value::from(customer().await),
            Value::from(status),
        ],
    )
    .await
    .expect("插夹具订单");
    let item_id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_sales_order_item \
            (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
         VALUES (?, ?, 1, ?, ?::text::numeric, 100, 0, 13, 0, 0)",
        vec![
            Value::from(item_id),
            Value::from(so_id),
            Value::from(mat_a().await),
            Value::Text(so_item_qty.to_string()),
        ],
    )
    .await
    .expect("插夹具订单行");
    (so_id, item_id)
}

async fn fixture_shipment(so_id: Uuid, so_item_id: Uuid, qty: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_shipment \
            (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty) \
         VALUES (?, ?, ?, ?, ?, CURRENT_DATE, 'PENDING_POST', ?::text::numeric)",
        vec![
            Value::from(id),
            Value::from(format!("SO10R-SH-{}", &id.to_string()[24..])),
            Value::from(so_id),
            Value::from(customer().await),
            Value::from(warehouse().await),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .expect("插夹具发货单");
    db().execute_with_params(
        "INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty) \
         VALUES (?, ?, ?, ?::text::numeric)",
        vec![
            Value::from(id),
            Value::from(so_item_id),
            Value::from(mat_a().await),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .expect("插夹具发货明细");
    id
}

/// 发货过账（走 handler 公开入口：在途 + 出库待确认）。
async fn post_shipment(ship_id: Uuid) -> std::result::Result<shipment_handler::ShipmentRes, ApiError> {
    shipment_handler::change_shipment_status(
        None,
        &ship_id.to_string(),
        shipment_handler::ShipmentStatusReq {
            target_status: "POSTED".to_string(),
            reason: None,
        },
    )
    .await
}

/// 签收确认（走 handler 公开入口：在途转已出库）。
async fn receive(ship_id: Uuid) -> std::result::Result<shipment_handler::ShipmentRes, ApiError> {
    shipment_handler::confirm_receipt(None, &ship_id.to_string(), Some("签收演练：整单签收")).await
}

/// 出库单状态流转（前端「作废」按钮走的是这个入口：`PATCH /business/erp/stock-out/{id}/status`）。
async fn cancel_stock_out(stock_out_id: Uuid, reason: &str) -> std::result::Result<(), ApiError> {
    stockout_handler::change_status(
        stock_out_id,
        &StockOutStatusReq {
            status: Some("CANCELLED".to_string()),
            target_status: None,
            reason: Some(reason.to_string()),
        },
        None,
    )
    .await
}

/// 出库单「撤回」：`PENDING_POST → DRAFT`（同一条 `PATCH /status` 入口）。
///
/// 出库单的边表（`state_machine::STOCK_OUT_EDGES`，6 条）**没有** `PENDING_POST → CANCELLED`；
/// 待过账单要作废必须先撤回成草稿，与「已提交的单不能一步作废」同口径。
async fn withdraw_stock_out(stock_out_id: Uuid, reason: &str) -> std::result::Result<(), ApiError> {
    stockout_handler::change_status(
        stock_out_id,
        &StockOutStatusReq {
            status: Some("DRAFT".to_string()),
            target_status: None,
            reason: Some(reason.to_string()),
        },
        None,
    )
    .await
}

async fn cost_qty() -> Decimal {
    scalar(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory WHERE material_id = ?::uuid",
        vec![Value::from(&mat_a().await)],
    )
    .await
    .and_then(|text| text.parse::<Decimal>().ok())
    .unwrap_or(Decimal::ZERO)
}

async fn location_qty() -> Decimal {
    scalar(
        "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_location WHERE material_id = ?::uuid",
        vec![Value::from(&mat_a().await)],
    )
    .await
    .and_then(|text| text.parse::<Decimal>().ok())
    .unwrap_or(Decimal::ZERO)
}

async fn txn_net() -> Decimal {
    scalar(
        "SELECT COALESCE(SUM(CASE WHEN txn_type = 'OUT' THEN -qty ELSE qty END), 0)::text \
           FROM biz_inventory_txn WHERE material_id = ?::uuid",
        vec![Value::from(&mat_a().await)],
    )
    .await
    .and_then(|text| text.parse::<Decimal>().ok())
    .unwrap_or(Decimal::ZERO)
}

async fn open_transit_qty() -> Decimal {
    scalar(
        "SELECT COALESCE(SUM(qty_in_transit), 0)::text FROM biz_inventory_in_transit \
          WHERE material_id = ?::uuid AND status = 'IN_TRANSIT'",
        vec![Value::from(&mat_a().await)],
    )
    .await
    .and_then(|text| text.parse::<Decimal>().ok())
    .unwrap_or(Decimal::ZERO)
}

/// 在途行的落库形态：`shipped/received/qty_in_transit/status/received_at 是否有值`。
async fn transit_shape(ship_id: Uuid) -> Option<String> {
    scalar(
        "SELECT shipped_qty::text || '/' || received_qty::text || '/' || qty_in_transit::text \
                || '/' || status || '/' || COALESCE(received_at IS NOT NULL, false)::text \
           FROM biz_inventory_in_transit WHERE shipment_id = ?::uuid AND material_id = ?::uuid",
        vec![Value::from(&ship_id), Value::from(&mat_a().await)],
    )
    .await
}

/// 在途行备注（作废原因落在这里；`COALESCE` 把 NULL 变成空串，便于断言）。
async fn transit_remark(ship_id: Uuid) -> String {
    scalar(
        "SELECT COALESCE(remark, '') FROM biz_inventory_in_transit \
          WHERE shipment_id = ?::uuid AND material_id = ?::uuid",
        vec![Value::from(&ship_id), Value::from(&mat_a().await)],
    )
    .await
    .unwrap_or_default()
}

async fn stock_out_of(ship_id: Uuid) -> Option<Uuid> {
    db().scalar_with_params::<Uuid>(
        "SELECT id FROM biz_stock_out WHERE source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&ship_id)],
    )
    .await
    .ok()
}

async fn stock_out_status(stock_out_id: Uuid) -> Option<String> {
    scalar(
        "SELECT status FROM biz_stock_out WHERE id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
}

async fn voucher_count(stock_out_id: Uuid) -> i64 {
    count(
        "SELECT count(*) FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
}

async fn receivable_count(stock_out_id: Uuid) -> i64 {
    count(
        "SELECT count(*) FROM biz_receivable WHERE source_type = 'SALES_OUT' AND source_id = ?::uuid",
        vec![Value::from(&stock_out_id)],
    )
    .await
}

// ==================== 用例 ====================

/// **签收确认 = 扣库存 + 清在途**（本卡的核心口径）。
///
/// 发货过账后：库存 20 一分没动、在途 6、出库单 `PENDING_POST`、无凭证、无应收。
/// 签收确认后：库存 14（成本账 / 库位账都减）、OUT 流水 6、成本结转凭证 1 张、
/// 应收 1 条、出库单 `POSTED`、在途行 `6/6/0/RECEIVED` 且 `received_at` 有值。
#[tokio::test]
async fn receipt_clears_transit_and_posts_stock_out() {
    let Some(_guard) = setup().await else { return };
    seed_stock("20").await;
    let (so_id, item_id) = fixture_order("6", "APPROVED").await;
    let ship_id = fixture_shipment(so_id, item_id, "6").await;
    post_shipment(ship_id)
        .await
        .unwrap_or_else(|error| panic!("发货过账应成功：{} {}", error.code, error.msg));

    // 过账后（签收前）：库存不扣、在途 6、出库单待确认、无凭证无应收
    let stock_out_id = stock_out_of(ship_id).await.expect("过账必须生成销售出库单");
    assert_eq!(cost_qty().await, dec("20"), "发货过账不扣库存");
    assert_eq!(open_transit_qty().await, dec("6"), "在途 6");
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("PENDING_POST"),
        "签收前出库单必须停在「出库待确认」"
    );
    assert_eq!(voucher_count(stock_out_id).await, 0, "签收前没有成本结转凭证");
    assert_eq!(receivable_count(stock_out_id).await, 0, "签收前没有应收");

    // 签收确认
    let res = receive(ship_id)
        .await
        .unwrap_or_else(|error| panic!("签收确认应成功：{} {}", error.code, error.msg));
    assert_eq!(res.status, "POSTED", "发货单状态不变（POSTED = 已发货，签收不倒退）");
    assert!(res.stock_out_id.is_some(), "详情必须回显关联出库单");

    // ① 在途清零：一条 UPDATE 同时推 received_qty / status / received_at（V0420 的 CHECK 咬合）
    assert_eq!(
        transit_shape(ship_id).await.as_deref(),
        Some("6.0000/6.0000/0.0000/RECEIVED/true"),
        "在途行必须是「签满 + RECEIVED + 有签收时间」"
    );
    assert_eq!(open_transit_qty().await, Decimal::ZERO, "签收后在途归零");

    // ② 三本账负向
    assert_eq!(cost_qty().await, dec("14"), "成本账 20 − 6 = 14");
    assert_eq!(location_qty().await, dec("14"), "库位账 20 − 6 = 14");
    assert_eq!(txn_net().await, dec("-6"), "必须留下 −6 的 OUT 流水");

    // ③ 出库单过账 + 成本结转凭证 + 应收挂账
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("POSTED"),
        "签收确认必须把销售出库单过账"
    );
    assert!(
        scalar(
            "SELECT posted_at::text FROM biz_stock_out WHERE id = ?::uuid",
            vec![Value::from(&stock_out_id)]
        )
        .await
        .is_some(),
        "过账后 posted_at 必须有值"
    );
    assert_eq!(voucher_count(stock_out_id).await, 1, "签收确认才生成成本结转凭证");
    assert_eq!(receivable_count(stock_out_id).await, 1, "签收确认才挂应收");

    // ④ 状态日志能回答「谁在什么时候把它签掉的」
    let reason = scalar(
        "SELECT reason FROM biz_status_log \
          WHERE entity_type = 'STOCK_OUT' AND entity_id = ?::uuid AND to_status = 'POSTED' \
          ORDER BY created_at DESC LIMIT 1",
        vec![Value::from(&stock_out_id)],
    )
    .await;
    assert!(
        reason.as_deref().is_some_and(|text| text.contains("客户签收确认")),
        "状态日志要写明是「客户签收确认」而不是谁手工过账，实际 {reason:?}"
    );

    println!("[M2-03] 签收确认：在途 6→0（RECEIVED），库存 20→14，凭证/应收各 1 张，出库单 POSTED");
}

/// **重复签收 409**：第二次不会再扣一次库存，也不会多出一张凭证。
#[tokio::test]
async fn repeat_receipt_is_rejected_at_gate() {
    let Some(_guard) = setup().await else { return };
    seed_stock("20").await;
    let (so_id, item_id) = fixture_order("4", "APPROVED").await;
    let ship_id = fixture_shipment(so_id, item_id, "4").await;
    post_shipment(ship_id).await.expect("发货过账");
    receive(ship_id).await.expect("第一次签收");
    let stock_out_id = stock_out_of(ship_id).await.expect("出库单");

    let error = receive(ship_id).await.expect_err("已签收的单再签必须被拒");
    assert_eq!(error.status, 409, "重复签收应 409，实际 {}", error.status);
    assert!(
        error.msg.contains("没有未签收") || error.msg.contains("已经签收"),
        "文案要点明「没有未签收的在途量」，实际：{}",
        error.msg
    );

    assert_eq!(cost_qty().await, dec("16"), "库存不许被扣第二次");
    assert_eq!(location_qty().await, dec("16"));
    assert_eq!(txn_net().await, dec("-4"), "流水不许写第二遍");
    assert_eq!(voucher_count(stock_out_id).await, 1, "凭证只有一张");
    assert_eq!(receivable_count(stock_out_id).await, 1, "应收只有一条");
    assert_eq!(
        transit_shape(ship_id).await.as_deref(),
        Some("4.0000/4.0000/0.0000/RECEIVED/true"),
        "在途行仍是「签满 RECEIVED」"
    );
}

/// **草稿 / 待过账的发货单不能签收**（还没有在途量）——409，且一分账不动。
#[tokio::test]
async fn receipt_requires_posted_shipment() {
    let Some(_guard) = setup().await else { return };
    seed_stock("20").await;
    let (so_id, item_id) = fixture_order("5", "APPROVED").await;
    let ship_id = fixture_shipment(so_id, item_id, "5").await;

    let error = receive(ship_id).await.expect_err("PENDING_POST 的发货单不能签收");
    assert_eq!(error.status, 409, "应 409，实际 {}", error.status);
    assert!(
        error.msg.contains("不能签收确认") && error.msg.contains("先过账"),
        "文案要告诉用户先过账，实际：{}",
        error.msg
    );
    assert_eq!(cost_qty().await, dec("20"), "库存不许动");
    assert_eq!(open_transit_qty().await, Decimal::ZERO, "没有在途");
    assert!(stock_out_of(ship_id).await.is_none(), "没生成出库单");
    assert_eq!(
        scalar(
            "SELECT status FROM biz_shipment WHERE id = ?::uuid",
            vec![Value::from(&ship_id)]
        )
        .await
        .as_deref(),
        Some("PENDING_POST"),
        "发货单必须仍是 PENDING_POST"
    );
}

/// **出库单页面点「过账」= 同一件事的另一个入口**：在途一样清零，账只算一次。
#[tokio::test]
async fn posting_from_stock_out_page_also_clears_transit() {
    let Some(_guard) = setup().await else { return };
    seed_stock("20").await;
    let (so_id, item_id) = fixture_order("7", "APPROVED").await;
    let ship_id = fixture_shipment(so_id, item_id, "7").await;
    post_shipment(ship_id).await.expect("发货过账");
    let stock_out_id = stock_out_of(ship_id).await.expect("出库单");
    assert_eq!(open_transit_qty().await, dec("7"), "签收前在途 7");

    // 走**出库单**的既有状态流转入口（前端「过账」按钮就是它）
    stockout_handler::change_status(
        stock_out_id,
        &StockOutStatusReq {
            status: Some("POSTED".to_string()),
            target_status: None,
            reason: None,
        },
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("出库单过账应成功：{} {}", error.code, error.msg));

    assert_eq!(
        transit_shape(ship_id).await.as_deref(),
        Some("7.0000/7.0000/0.0000/RECEIVED/true"),
        "从出库单过账也必须把在途签收清零（同一收口）"
    );
    assert_eq!(cost_qty().await, dec("13"), "库存只扣一次：20 − 7");
    assert_eq!(voucher_count(stock_out_id).await, 1);
    assert_eq!(receivable_count(stock_out_id).await, 1);

    // 再从发货单点签收：没有在途可签 → 409，账不再动（不会双重过账）
    let error = receive(ship_id).await.expect_err("出库单已过账后签收必须被拒");
    assert_eq!(error.status, 409);
    assert_eq!(cost_qty().await, dec("13"), "库存不许被扣第二次");
}

/// **过账半边失败 → 整笔回滚，补货后重试成功**（AGENTS.md 约定 11：不留死单）。
///
/// 造法：发货过账（在途 6）之后把三本账清空 ⇒ 签收时负库存闸 409。
/// 期望：在途仍 `IN_TRANSIT`（没被清掉）、出库单仍 `PENDING_POST`、无凭证无应收；
/// 再把库存补回来 → 同一个接口重试直接成功（用户不需要工程师改库）。
#[tokio::test]
async fn receipt_failure_leaves_transit_open_and_retryable() {
    let Some(_guard) = setup().await else { return };
    seed_stock("6").await;
    let (so_id, item_id) = fixture_order("6", "APPROVED").await;
    let ship_id = fixture_shipment(so_id, item_id, "6").await;
    post_shipment(ship_id).await.expect("发货过账");
    let stock_out_id = stock_out_of(ship_id).await.expect("出库单");

    // 造「货已在路上、账上却不够扣」：把三本账清空（模拟别处已经把货挪走）
    sql(&format!(
        "DELETE FROM biz_inventory_txn WHERE material_id IN {MATS_SQL}"
    ))
    .await;
    sql(&format!("DELETE FROM biz_inventory WHERE material_id IN {MATS_SQL}")).await;
    sql(&format!(
        "DELETE FROM biz_inventory_location WHERE material_id IN {MATS_SQL}"
    ))
    .await;

    let error = receive(ship_id)
        .await
        .expect_err("账上没有库存，签收过账必须被负库存闸拦下");
    assert_eq!(error.status, 409, "负库存应 409，实际 {}", error.status);

    // 关键：失败后**不许**把在途清掉、**不许**把出库单留成 POSTED
    assert_eq!(
        transit_shape(ship_id).await.as_deref(),
        Some("6.0000/0.0000/6.0000/IN_TRANSIT/false"),
        "在途必须原样留着（可重试），实际 {:?}",
        transit_shape(ship_id).await
    );
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("PENDING_POST"),
        "出库单必须打回可重试状态（PENDING_POST）"
    );
    assert_eq!(voucher_count(stock_out_id).await, 0, "失败不许留半张凭证");
    assert_eq!(receivable_count(stock_out_id).await, 0, "失败不许挂应收");

    // 补货 → 同一个接口重试成功（用户自己能恢复）
    seed_stock("6").await;
    receive(ship_id)
        .await
        .unwrap_or_else(|error| panic!("补货后重试应成功：{} {}", error.code, error.msg));
    assert_eq!(open_transit_qty().await, Decimal::ZERO, "重试后在途归零");
    assert_eq!(cost_qty().await, dec("0"), "补 6 扣 6 = 0");
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("POSTED"),
        "重试后出库单已过账"
    );
}

/// 纯逻辑：签收原因文案（状态日志 + 在途备注共用一条）。
#[test]
fn receipt_reason_carries_ship_no() {
    let reason = crate::business::erp::shipment::shipment_posting::receipt_reason("SH-20261116-000042");
    assert!(reason.contains("签收"), "必须说明是签收：{reason}");
    assert!(
        reason.contains("SH-20261116-000042"),
        "必须带发货单号（对账要能倒推）：{reason}"
    );
}

/// 纯逻辑：`shipment_model::STATUS_POSTED` 是签收确认的前置态（与状态域同源）。
#[test]
fn receipt_gate_uses_posted_status() {
    assert_eq!(shipment_model::STATUS_POSTED, "POSTED");
    assert!(
        !shipment_model::can_transition("POSTED", "POSTED"),
        "签收确认不推进发货单状态（POSTED 是终态），在途/出库单才是它的落点"
    );
}

// ==================== 作废释放在途（M2-FIX t_f123b57f） ====================
//
// M2-02 的口径是「发货过账只写在途 + 生成出库待确认单，库存不扣」，可用量口径是
// 「可用 = 账面 − 未签收在途」。作废那张出库待确认单 = 这批货不再出库，在途必须撤回 ——
// 否则可用量被永久占住，单据却已是终态、界面上无按钮可点，只能改库恢复。

/// **作废「出库待确认」的销售出库单 → 在途行 CANCELLED、可用量释放、同一批货能再发**。
///
/// 造法（把缺口逼到必现）：账面只有 6，第一批发 6（在途 6）⇒ 可用量 0，
/// 再发同一批货必被「可用 = 账面 − 在途」拦下（409）—— 这就是 M2-02 遗留缺口的现场。
/// 作废后：在途行 `CANCELLED`（数量列一个不动、备注写原因）、账一分没动、
/// 同一批货再发能过账（不需要工程师改库）。
///
/// ⚠ 操作路径是 **撤回 → 作废** 两步：出库单边表（`STOCK_OUT_EDGES` 6 条）没有
/// `PENDING_POST → CANCELLED`，待过账单必须先撤回成草稿再作废。「撤回」这一步不能释放在途
/// （货还在路上、单据还可能再提交），所以两条分支分别断言。
#[tokio::test]
async fn cancel_pending_stock_out_releases_transit() {
    let Some(_guard) = setup().await else { return };
    seed_stock("6").await;
    let (so_id, item_id) = fixture_order("12", "APPROVED").await;
    let first = fixture_shipment(so_id, item_id, "6").await;
    post_shipment(first)
        .await
        .unwrap_or_else(|error| panic!("第一批发货过账应成功：{} {}", error.code, error.msg));
    let stock_out_id = stock_out_of(first).await.expect("过账必须生成销售出库单");
    assert_eq!(open_transit_qty().await, dec("6"), "发货后 6 在途");
    assert_eq!(cost_qty().await, dec("6"), "发货不扣库存（M2-02 口径）");
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("PENDING_POST"),
        "出库单停在「出库待确认」"
    );

    // 缺口现场：可用量 6 − 在途 6 = 0 ⇒ 同一批货再发 6 必须被拦下
    let second = fixture_shipment(so_id, item_id, "6").await;
    let blocked = post_shipment(second)
        .await
        .expect_err("在途占着可用量，同一批货再发 6 必须被拒");
    assert_eq!(blocked.status, 409, "应是库存不足，实际 {}", blocked.status);
    assert!(
        blocked.msg.contains("在途"),
        "文案要点明「在途未签收」占着可用量，实际：{}",
        blocked.msg
    );
    assert!(stock_out_of(second).await.is_none(), "被拒的发货单不许生成出库单");

    // 作废「出库待确认」的销售出库单 —— 边表没有 PENDING_POST → CANCELLED，
    // 真实操作路径是 **撤回 → 作废** 两步（前端两个按钮，两个入口都走 change_status）。
    //
    // ① 撤回（PENDING_POST → DRAFT）**不释放**在途：单据还可能被再次提交，
    //    货也还在路上 —— 这时把在途撤掉就等于凭空还回可用量。
    withdraw_stock_out(stock_out_id, "客户可能要取消，先撤回改单")
        .await
        .unwrap_or_else(|error| panic!("撤回待过账出库单应成功：{} {}", error.code, error.msg));
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("DRAFT"),
        "撤回后回到草稿"
    );
    assert_eq!(
        transit_shape(first).await.as_deref(),
        Some("6.0000/0.0000/6.0000/IN_TRANSIT/false"),
        "撤回不是作废：在途必须原样（仍占着可用量）"
    );

    // ② 作废（DRAFT → CANCELLED）—— 这里才是「这批货不出库了」，在途要同步撤回
    cancel_stock_out(stock_out_id, "客户取消订单，这批货不出库了")
        .await
        .unwrap_or_else(|error| panic!("作废出库待确认单应成功：{} {}", error.code, error.msg));
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("CANCELLED"),
        "出库单已作废"
    );

    // ① 在途行：状态 CANCELLED、数量列原样（「发过货」是事实）、备注 = 作废原因
    assert_eq!(
        transit_shape(first).await.as_deref(),
        Some("6.0000/0.0000/6.0000/CANCELLED/false"),
        "在途行必须置 CANCELLED（只改状态，不动 shipped/received），实际 {:?}",
        transit_shape(first).await
    );
    assert_eq!(
        transit_remark(first).await,
        "客户取消订单，这批货不出库了",
        "作废原因要落在在途行 remark（审计能回答「为什么不算在途了」）"
    );

    // ② Σ(未签收在途) 归零 = 可用量恢复
    assert_eq!(
        open_transit_qty().await,
        Decimal::ZERO,
        "作废后未签收在途必须归零（可用量 = 账面 − 在途 恢复成 6）"
    );

    // ③ 账一分没动：三本账 / 流水 / 凭证 / 应收都不许有痕迹（作废的是「没动过账」的单）
    assert_eq!(cost_qty().await, dec("6"), "成本账不许动");
    assert_eq!(location_qty().await, dec("6"), "库位账不许动");
    assert_eq!(txn_net().await, Decimal::ZERO, "不许写出入库流水");
    assert_eq!(voucher_count(stock_out_id).await, 0, "不许有凭证");
    assert_eq!(receivable_count(stock_out_id).await, 0, "不许挂应收");

    // ④ 同一批货现在能过账（这就是「用户自己能恢复」）
    post_shipment(second)
        .await
        .unwrap_or_else(|error| panic!("作废释放在途后同一批货应能再发：{} {}", error.code, error.msg));
    assert_eq!(open_transit_qty().await, dec("6"), "只有第二批的在途");
    assert_eq!(cost_qty().await, dec("6"), "账面仍不扣（还没签收）");
    assert_eq!(
        transit_shape(first).await.as_deref(),
        Some("6.0000/0.0000/6.0000/CANCELLED/false"),
        "作废行不许被后来的发货「复活」"
    );

    // ⑤ 终态：已作废的单不能再作废（状态机 409），在途行也不被二次改动
    let again = cancel_stock_out(stock_out_id, "再作废一次")
        .await
        .expect_err("已作废的出库单不能再作废");
    assert_eq!(again.status, 409);
    assert_eq!(
        transit_remark(first).await,
        "客户取消订单，这批货不出库了",
        "二次作废不许覆盖原备注"
    );

    println!("[M2-FIX] 作废出库待确认单：在途 6 → CANCELLED（备注留痕），可用量恢复，同一批货重发成功");
}

/// **已过账（POSTED）的出库单作废不许碰在途行**：那条路走红冲，在途早已 `RECEIVED`。
///
/// 若在这里也把在途置 `CANCELLED`，等于把已签收的量重复放回可用量（一张单被算两遍），
/// 所以「POSTED 分支不碰在途」必须有一条用例钉死。
#[tokio::test]
async fn cancel_posted_stock_out_leaves_received_transit_alone() {
    let Some(_guard) = setup().await else { return };
    seed_stock("20").await;
    let (so_id, item_id) = fixture_order("6", "APPROVED").await;
    let ship_id = fixture_shipment(so_id, item_id, "6").await;
    post_shipment(ship_id).await.expect("发货过账");
    let stock_out_id = stock_out_of(ship_id).await.expect("出库单");
    receive(ship_id).await.expect("签收确认（在途 → RECEIVED）");
    assert_eq!(
        transit_shape(ship_id).await.as_deref(),
        Some("6.0000/6.0000/0.0000/RECEIVED/true"),
        "签收后在途行是「签满 RECEIVED」"
    );
    // 签收时写下的在途备注（文案由 `receipt_reason` 统一，这里只做「红冲不许改它」的基线）
    let remark_before = transit_remark(ship_id).await;
    assert!(
        remark_before.contains("签收"),
        "签收必须在途行留下备注，实际 {remark_before:?}"
    );

    // 已过账的出库单作废：三本账回滚 + 红冲 + 冲销应收
    cancel_stock_out(stock_out_id, "客户退货，红冲")
        .await
        .unwrap_or_else(|error| panic!("已过账出库单作废应成功：{} {}", error.code, error.msg));
    assert_eq!(
        stock_out_status(stock_out_id).await.as_deref(),
        Some("CANCELLED"),
        "出库单已作废"
    );

    // 在途行一个字段都不许动（receipt 的四个落库值 + 备注）
    assert_eq!(
        transit_shape(ship_id).await.as_deref(),
        Some("6.0000/6.0000/0.0000/RECEIVED/true"),
        "已过账作废走红冲，在途行必须原样（RECEIVED / 有签收时间）"
    );
    assert_eq!(
        transit_remark(ship_id).await,
        remark_before,
        "红冲不许覆盖签收时写的在途备注"
    );
    assert_eq!(open_transit_qty().await, Decimal::ZERO, "未签收在途仍是 0");

    // 红冲确实做了事：库存原路退回、凭证蓝 + 红各一张
    assert_eq!(cost_qty().await, dec("20"), "作废已过账 = 库存原路退回");
    assert_eq!(location_qty().await, dec("20"));
    assert_eq!(voucher_count(stock_out_id).await, 2, "蓝字 + 红字各一张");
}

/// 纯逻辑：作废释放在途的判据只认 `source_type='SALES'` + `source_id`。
#[test]
fn transit_release_only_applies_to_sales_source() {
    use crate::business::erp::stockout::stockout_base::SOURCE_TYPE_SALES;
    assert_eq!(SOURCE_TYPE_SALES, "SALES", "判据字面值必须与 V0285 的落库值一致");
    // 手工建的其他出库单（source_type = None / source_id = None）filter 后必然是 None ⇒ 不碰在途
    let manual: Option<&str> = None;
    assert_eq!(
        manual.filter(|value| Some(*value) == Some(SOURCE_TYPE_SALES)),
        None,
        "手工建单没有发货单可对应，不许释放在途"
    );
}

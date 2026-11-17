// ============================================================================
// transit_query_tests.rs —— 在途库存查询（M2-04）的真库用例
//
// 卡：t_f1bce846（M2-04【M2·仓库】在途库存：查询接口）
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `page_filters_by_material_warehouse_customer` | 卡面「按物料 / 仓库 / 客户查在途」：三个筛各自命中、且**只**命中该命中的行 |
// | `page_fills_names_without_n_plus_one` | 约定 13：`material_id` / `warehouse_id` / `customer_id` / `shipment_id` 都成对返回名字 / 单号 |
// | `default_status_is_open_transit` | 契约「在途取数一律带状态过滤」：留空只看 `IN_TRANSIT`；`RECEIVED` / `CANCELLED` 必须显式点名 |
// | `qty_in_transit_comes_from_generated_column` | M2-01：在途量取库里的生成列（`shipped - received`），不在 Rust 里算 |
// | `summary_groups_by_material_and_warehouse` | 汇总按 物料 × 仓库 合并多张发货单；合计 = 各行之和 |
// | `summary_ignores_cancelled_and_received` | 作废 / 已签收行不进汇总（它们是「不算在途」的状态） |
// | `age_days_and_ship_date_range` | 账龄由 PG 算（`CURRENT_DATE - ship_date`），区间筛选含边界 |
// | `pagination_is_stable_and_counts_all` | 分页 `total` 是筛选后的总量、行数受 `page_size` 限制、排序最老在前 |
//
// # 为什么夹具直接插在途行（不走过账 / 签收）
//
// 本卡是**只读**接口，写路径的口径已由 M2-02（`shipment_posting`）与 M2-03
// （`shipment_receipt_tests`）的用例钉住；这里要造的是「各种状态 × 各种日期」的组合，
// 走真实过账既造不出 `CANCELLED`、也造不出回溯 10 天的 `ship_date`。
// 直插的唯一风险是绕过 CHECK —— 而 V0420 的 4 条 CHECK 在库层就把非法组合挡住了。
//
// 主数据一律走 `crate::test_seed::ensure_*`（P2-62 门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_xxx_transit" cargo test --lib \
//   business::erp::inventory_transit::transit_query_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_transit::dto::{TransitFilter, TransitSummaryFilter};
use crate::business::erp::inventory_transit::handler;
use crate::test_seed;

/// 真库用例必须串行（共用全局 `db()` 与同一批 `M204-` 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAREHOUSE_A_CODE: &str = "M204-WHA";
const WAREHOUSE_B_CODE: &str = "M204-WHB";
const MAT_A_CODE: &str = "M204-M1";
const MAT_B_CODE: &str = "M204-M2";
const CUSTOMER_A_CODE: &str = "M204-CA";
const CUSTOMER_B_CODE: &str = "M204-CB";

/// 本模块的四张业务单据前缀（清场按客户反查，因为夹具客户只归本模块用）。
const SHIP_PREFIX: &str = "M204-SH-";
const SO_PREFIX: &str = "M204-SO-";

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "在途查询仓甲").await
}

async fn warehouse_b() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_B_CODE, "在途查询仓乙").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "在途查询物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "在途查询物料乙").await
}

async fn customer_a() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_A_CODE, "在途查询客户甲").await
}

async fn customer_b() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_B_CODE, "在途查询客户乙").await
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

/// 清场：删本模块**业务单据**（两个夹具客户下的订单 / 发货单 / 在途行）+ 别的套件留在
/// 在途账上的残留行（理由见 [`purge_foreign_transit_rows`]）。
/// 主数据（客户 / 仓库 / 物料）由 `test_seed` 负责，**不删** —— 它们是跨模块复用的夹具。
async fn cleanup() {
    purge_foreign_transit_rows().await;
    let orders = format!(
        "(SELECT id FROM biz_sales_order WHERE customer_id IN \
          (SELECT id FROM biz_customer WHERE code IN ('{CUSTOMER_A_CODE}', '{CUSTOMER_B_CODE}')))"
    );
    let shipments = format!("(SELECT id FROM biz_shipment WHERE so_id IN {orders})");
    // 在途行随 biz_shipment 的 ON DELETE CASCADE 一起消失（V0420 硬外键），显式删一遍更稳
    exec(
        &format!("DELETE FROM biz_inventory_in_transit WHERE shipment_id IN {shipments}"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_status_log WHERE entity_type = 'SHIPMENT' AND entity_id IN {shipments}"),
        vec![],
    )
    .await;
    exec(&format!("DELETE FROM biz_shipment WHERE so_id IN {orders}"), vec![]).await;
    exec(&format!("DELETE FROM biz_sales_order WHERE id IN {orders}"), vec![]).await;
}

/// 把「不属于本模块夹具」的在途行挡在门外，并断言真的清干了。
///
/// # 为什么本模块要替别人收拾
///
/// `biz_inventory_in_transit` 是**派生投影**（发货过账写入、签收改状态），本模块的
/// 8 条用例里有 6 条按契约要走「默认筛选」（`page_of(filter())` / `summary_of(default)`），
/// 也就是**全表口径** —— 这是契约要求的（「留空 = 只看未签收在途」不能被测试夹具篡改成
/// 「只看我的」）。而别的套件（`receivable_bridge` 的 `SO14-`、`shipment_receipt_tests` 的
/// `SO10R-`）造完发货单不收回在途行：它们留在库里，本模块第二轮就恒红。
///
/// 实测（卡 t_9c3551b0，同一库连跑第二轮）：`left: 2 / right: 1`、`left: 4 / right: 3`……
/// 8 条全红，原因都是「别人的一行」。
///
/// # 为什么不改成「只查自己的」
///
/// 那会把「默认筛选」这条契约的判据从「全表」缩成「我的两行」，属于**放宽判据**。
/// 这里反过来：造数前把别人的残留清掉 + **断言清干**（清不干净直接红），判据仍是全表，
/// 但不再受别的套件漏清的影响。清理范围只碰在途账（派生投影，可随时按发货单重算），
/// 不动任何人的单据；被删的是「发货单还在、过账/签收用例早已跑完」的残留行。
async fn purge_foreign_transit_rows() {
    let removed = db()
        .execute_with_params(
            "DELETE FROM biz_inventory_in_transit WHERE shipment_id NOT IN \
             (SELECT id FROM biz_shipment WHERE ship_no LIKE 'M204-SH-%')",
            vec![],
        )
        .await
        .expect("清理别的套件留下的在途行");
    let left: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM biz_inventory_in_transit WHERE shipment_id NOT IN \
             (SELECT id FROM biz_shipment WHERE ship_no LIKE 'M204-SH-%')"
                .to_string(),
            vec![],
        )
        .await
        .expect("核对外来在途行");
    assert_eq!(
        left, 0,
        "外来在途行必须清干（本轮清了 {removed} 行）；清不干净说明 DELETE 条件与本模块的\
         发货单前缀 'M204-SH-' 对不上"
    );
    if removed > 0 {
        println!("[M2-04] 清场：清掉别的套件留下的在途行 {removed} 行（派生投影，按发货单可重算）");
    }
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
    customer_a().await;
    customer_b().await;
    cleanup().await;
    Some(guard)
}

/// 造一张夹具发货单（订单 + 单头；在途行由 [`insert_transit`] 单独插）。
async fn fixture_shipment(customer_id: Uuid, warehouse_id: Uuid, ship_date: &str) -> Uuid {
    let so_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status) \
         VALUES (?, ?, ?, ?::text::date, 'APPROVED')",
        vec![
            Value::from(so_id),
            Value::from(format!("{SO_PREFIX}{}", &so_id.to_string()[24..])),
            Value::from(&customer_id),
            Value::Text(ship_date.to_string()),
        ],
    )
    .await;
    let ship_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_shipment \
            (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty) \
         VALUES (?, ?, ?, ?, ?, ?::text::date, 'POSTED', 0)",
        vec![
            Value::from(ship_id),
            Value::from(format!("{SHIP_PREFIX}{}", &ship_id.to_string()[24..])),
            Value::from(&so_id),
            Value::from(&customer_id),
            Value::from(&warehouse_id),
            Value::Text(ship_date.to_string()),
        ],
    )
    .await;
    ship_id
}

/// 造一张夹具发货单，发货日期由 `days_ago` 决定（0 = 今天；账龄用例用）。
async fn fixture_shipment_days_ago(customer_id: Uuid, warehouse_id: Uuid, days_ago: i32) -> Uuid {
    let (so_id, ship_id) = (Uuid::now_v7(), Uuid::now_v7());
    exec(
        "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status) \
         VALUES (?, ?, ?, CURRENT_DATE, 'APPROVED')",
        vec![
            Value::from(so_id),
            Value::from(format!("{SO_PREFIX}{}", &so_id.to_string()[24..])),
            Value::from(&customer_id),
        ],
    )
    .await;
    exec(
        "INSERT INTO biz_shipment \
            (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty) \
         VALUES (?, ?, ?, ?, ?, CURRENT_DATE - ?::int, 'POSTED', 0)",
        vec![
            Value::from(ship_id),
            Value::from(format!("{SHIP_PREFIX}{}", &ship_id.to_string()[24..])),
            Value::from(&so_id),
            Value::from(&customer_id),
            Value::from(&warehouse_id),
            Value::I32(days_ago),
        ],
    )
    .await;
    ship_id
}

/// 插一行在途账（`status` 必须是三值之一；`RECEIVED` 会自动补 `received_at`）。
#[allow(clippy::too_many_arguments)]
async fn insert_transit(
    shipment_id: Uuid,
    material_id: Uuid,
    warehouse_id: Uuid,
    ship_date: &str,
    shipped: &str,
    received: &str,
    status: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_inventory_in_transit \
            (id, shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, \
             status, received_at) \
         VALUES (?, ?, ?, ?, ?::text::date, ?::text::numeric, ?::text::numeric, ?, \
                 CASE WHEN ? = 'RECEIVED' THEN now() ELSE NULL END)",
        vec![
            Value::from(id),
            Value::from(&shipment_id),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(ship_date.to_string()),
            Value::Text(shipped.to_string()),
            Value::Text(received.to_string()),
            Value::from(status),
            Value::from(status),
        ],
    )
    .await;
    id
}

/// 插一行在途账，发货日期 = `CURRENT_DATE - days_ago`（账龄用例用；日期由库侧算）。
#[allow(clippy::too_many_arguments)]
async fn insert_transit_days_ago(
    shipment_id: Uuid,
    material_id: Uuid,
    warehouse_id: Uuid,
    days_ago: i32,
    shipped: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_inventory_in_transit \
            (id, shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, \
             status) \
         VALUES (?, ?, ?, ?, CURRENT_DATE - ?::int, ?::text::numeric, 0, 'IN_TRANSIT')",
        vec![
            Value::from(id),
            Value::from(&shipment_id),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::I32(days_ago),
            Value::Text(shipped.to_string()),
        ],
    )
    .await;
    id
}

fn dec(text: &str) -> nik::prelude::rust_decimal::Decimal {
    text.parse().expect("定点字面量非法")
}

fn filter() -> TransitFilter {
    TransitFilter::default()
}

// ==================== 用例 ====================

/// 卡面「按物料 / 仓库 / 客户查在途」：三个筛各自只命中该命中的行。
#[tokio::test]
async fn page_filters_by_material_warehouse_customer() {
    let Some(_guard) = setup().await else { return };
    let (wh_a, wh_b) = (warehouse_a().await, warehouse_b().await);
    let (mat_1, mat_2) = (mat_a().await, mat_b().await);
    let (cust_a, cust_b) = (customer_a().await, customer_b().await);

    // ① 客户甲 · 仓甲 · 物料甲；② 客户乙 · 仓乙 · 物料乙
    let ship_a = fixture_shipment(cust_a, wh_a, "2026-11-10").await;
    let ship_b = fixture_shipment(cust_b, wh_b, "2026-11-11").await;
    insert_transit(ship_a, mat_1, wh_a, "2026-11-10", "10", "0", "IN_TRANSIT").await;
    insert_transit(ship_b, mat_2, wh_b, "2026-11-11", "7", "0", "IN_TRANSIT").await;

    let all = handler::page_of(filter()).await.expect("默认分页应成功");
    assert_eq!(all.total, 2, "两张在途行都应查到（total={}）", all.total);

    // 按物料
    let by_material = handler::page_of(TransitFilter {
        material_id: Some(mat_1.to_string()),
        ..filter()
    })
    .await
    .expect("按物料查");
    assert_eq!(by_material.total, 1, "物料甲只有一行在途");
    assert_eq!(by_material.list[0].material_id, mat_1.to_string());
    assert_eq!(by_material.list[0].warehouse_id, wh_a.to_string());

    // 按仓库
    let by_warehouse = handler::page_of(TransitFilter {
        warehouse_id: Some(wh_b.to_string()),
        ..filter()
    })
    .await
    .expect("按仓库查");
    assert_eq!(by_warehouse.total, 1, "仓乙只有一行在途");
    assert_eq!(by_warehouse.list[0].shipment_id, ship_b.to_string());

    // 按客户（经发货单头关联）
    let by_customer = handler::page_of(TransitFilter {
        customer_id: Some(cust_a.to_string()),
        ..filter()
    })
    .await
    .expect("按客户查");
    assert_eq!(by_customer.total, 1, "客户甲只有一行在途");
    assert_eq!(by_customer.list[0].shipment_id, ship_a.to_string());
    assert_eq!(by_customer.list[0].customer_id, Some(cust_a.to_string()));

    // 按发货单（发货单详情「查看在途」的入口）
    let by_shipment = handler::page_of(TransitFilter {
        shipment_id: Some(ship_b.to_string()),
        ..filter()
    })
    .await
    .expect("按发货单查");
    assert_eq!(by_shipment.total, 1);
    assert_eq!(by_shipment.list[0].material_id, mat_2.to_string());

    // 三个条件同时给（AND 语义）：物料甲 + 仓乙 = 0 行
    let none = handler::page_of(TransitFilter {
        material_id: Some(mat_1.to_string()),
        warehouse_id: Some(wh_b.to_string()),
        ..filter()
    })
    .await
    .expect("组合查");
    assert_eq!(none.total, 0, "条件之间是 AND，交叉组合应为空");
    println!("[M2-04] 物料 / 仓库 / 客户 / 发货单四个筛各命中 1 行，交叉组合 0 行 ✓");
}

/// 约定 13：`*_id` 必须成对返回名字 / 单号（一页固定 3 条批量 SQL，不做 N+1）。
#[tokio::test]
async fn page_fills_names_without_n_plus_one() {
    let Some(_guard) = setup().await else { return };
    let ship_id = fixture_shipment(customer_a().await, warehouse_a().await, "2026-11-12").await;
    insert_transit(
        ship_id,
        mat_a().await,
        warehouse_a().await,
        "2026-11-12",
        "5",
        "0",
        "IN_TRANSIT",
    )
    .await;

    let page = handler::page_of(filter()).await.expect("分页应成功");
    assert_eq!(page.list.len(), 1);
    let row = &page.list[0];
    assert_eq!(row.material_code.as_deref(), Some(MAT_A_CODE), "物料编码必须下发");
    assert_eq!(row.material_name.as_deref(), Some("在途查询物料甲"));
    assert_eq!(
        row.warehouse_code.as_deref(),
        Some(WAREHOUSE_A_CODE),
        "仓库编码必须下发"
    );
    assert_eq!(row.warehouse_name.as_deref(), Some("在途查询仓甲"));
    assert_eq!(row.customer_code.as_deref(), Some(CUSTOMER_A_CODE), "客户编码必须下发");
    assert_eq!(row.customer_name.as_deref(), Some("在途查询客户甲"));
    assert!(
        row.ship_no.as_deref().is_some_and(|no| no.starts_with(SHIP_PREFIX)),
        "发货单号必须下发"
    );
    assert!(
        row.unit_name.is_some(),
        "单位名由物料主数据带出（测试库夹具物料有单位）"
    );
    assert_eq!(row.status_label, "在途");
    println!("[M2-04] 一页行补齐：物料码/名、仓库码/名、客户码/名、发货单号、单位名 ✓");
}

/// 契约「在途取数一律带状态过滤」：留空 = 只看未签收；已签收 / 已作废必须显式点名。
#[tokio::test]
async fn default_status_is_open_transit() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let cust = customer_a().await;
    let ship_open = fixture_shipment(cust, wh, "2026-11-10").await;
    let ship_done = fixture_shipment(cust, wh, "2026-11-10").await;
    let ship_void = fixture_shipment(cust, wh, "2026-11-10").await;
    insert_transit(ship_open, mat, wh, "2026-11-10", "10", "0", "IN_TRANSIT").await;
    insert_transit(ship_done, mat, wh, "2026-11-10", "10", "10", "RECEIVED").await;
    insert_transit(ship_void, mat, wh, "2026-11-10", "10", "0", "CANCELLED").await;

    let default_page = handler::page_of(filter()).await.expect("默认分页");
    assert_eq!(default_page.total, 1, "默认只返回未签收在途（不是全部三行）");
    assert_eq!(default_page.list[0].status, "IN_TRANSIT");
    assert_eq!(default_page.list[0].shipment_id, ship_open.to_string());

    let received = handler::page_of(TransitFilter {
        status: Some("RECEIVED".to_string()),
        ..filter()
    })
    .await
    .expect("显式查已签收");
    assert_eq!(received.total, 1);
    assert_eq!(received.list[0].status_label, "已签收");
    assert!(received.list[0].received_at.is_some(), "RECEIVED 必有签收时间");

    let cancelled = handler::page_of(TransitFilter {
        status: Some("CANCELLED".to_string()),
        ..filter()
    })
    .await
    .expect("显式查已作废（审计视角）");
    assert_eq!(cancelled.total, 1);
    assert_eq!(cancelled.list[0].status_label, "已作废");

    // 非法状态值 → 400 + 66006（不静默当成默认值）
    let error = handler::page_of(TransitFilter {
        status: Some("RECIEVED".to_string()),
        ..filter()
    })
    .await
    .expect_err("拼错的状态必须报错");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, cloudpivot_kernel::shared::error_code::ERP_FILTER_INVALID);
    println!("[M2-04] 默认只含 IN_TRANSIT；RECEIVED / CANCELLED 需显式点名；非法值 400+66006 ✓");
}

/// M2-01：在途量取库里的生成列（`shipped - received`），本模块不自己减。
#[tokio::test]
async fn qty_in_transit_comes_from_generated_column() {
    let Some(_guard) = setup().await else { return };
    let ship_id = fixture_shipment(customer_a().await, warehouse_a().await, "2026-11-09").await;
    insert_transit(
        ship_id,
        mat_a().await,
        warehouse_a().await,
        "2026-11-09",
        "10",
        "4",
        "IN_TRANSIT",
    )
    .await;

    let page = handler::page_of(filter()).await.expect("分页");
    assert_eq!(page.list.len(), 1);
    let row = &page.list[0];
    assert_eq!(row.shipped_qty, dec("10.0000"));
    assert_eq!(row.received_qty, dec("4.0000"));
    assert_eq!(
        row.qty_in_transit,
        dec("6.0000"),
        "在途量必须 = 发货 − 签收（库层生成列）"
    );

    // 直接对库：读到的就是生成列的值（口径只有一份）
    let from_db: String = db()
        .scalar_with_params::<String>(
            "SELECT qty_in_transit::text FROM biz_inventory_in_transit WHERE shipment_id = ?::uuid",
            vec![Value::from(&ship_id)],
        )
        .await
        .expect("库侧读在途量");
    assert_eq!(from_db, "6.0000");
    println!("[M2-04] 发货 10 / 签收 4 → 在途 6（与库里的生成列逐字一致）✓");
}

/// 汇总按 物料 × 仓库 合并多张发货单，合计 = 各行之和。
#[tokio::test]
async fn summary_groups_by_material_and_warehouse() {
    let Some(_guard) = setup().await else { return };
    let (wh_a, wh_b) = (warehouse_a().await, warehouse_b().await);
    let (mat_1, mat_2) = (mat_a().await, mat_b().await);
    let cust = customer_a().await;
    // 物料甲 + 仓甲：两张发货单（3 + 5 = 8）；物料乙 + 仓乙：一张（2）
    let s1 = fixture_shipment(cust, wh_a, "2026-11-08").await;
    let s2 = fixture_shipment(cust, wh_a, "2026-11-10").await;
    let s3 = fixture_shipment(cust, wh_b, "2026-11-11").await;
    insert_transit(s1, mat_1, wh_a, "2026-11-08", "3", "0", "IN_TRANSIT").await;
    insert_transit(s2, mat_1, wh_a, "2026-11-10", "5", "1", "IN_TRANSIT").await;
    insert_transit(s3, mat_2, wh_b, "2026-11-11", "2", "0", "IN_TRANSIT").await;

    let summary = handler::summary_of(TransitSummaryFilter::default())
        .await
        .expect("汇总应成功");
    assert_eq!(summary.row_count, 2, "两个 物料×仓库 组");
    assert!(!summary.truncated);
    assert_eq!(summary.total_lines, 3, "三行在途");
    assert_eq!(summary.total_shipments, 3, "三张发货单");
    // 在途：3 + (5-1) + 2 = 9
    assert_eq!(summary.total_qty_in_transit, dec("9.0000"));

    // 物料甲 + 仓甲那一组：在途 3 + 4 = 7、两张单、最早 2026-11-08
    let group = summary
        .rows
        .iter()
        .find(|row| row.material_id == mat_1.to_string() && row.warehouse_id == wh_a.to_string())
        .expect("物料甲 × 仓甲 组必须存在");
    assert_eq!(group.qty_in_transit, dec("7.0000"));
    assert_eq!(group.shipped_qty, dec("8.0000"));
    assert_eq!(group.received_qty, dec("1.0000"));
    assert_eq!(group.shipments, 2, "两张发货单合成一组");
    assert_eq!(group.first_ship_date, "2026-11-08", "最早发货日期");
    assert_eq!(group.material_code.as_deref(), Some(MAT_A_CODE));
    assert_eq!(group.warehouse_name.as_deref(), Some("在途查询仓甲"));
    // 各行之和 == 合计（截断时才会有差，本用例没截断）
    let sum_of_rows: nik::prelude::rust_decimal::Decimal = summary.rows.iter().map(|row| row.qty_in_transit).sum();
    assert_eq!(sum_of_rows, summary.total_qty_in_transit, "行之和必须等于合计");
    println!("[M2-04] 汇总 2 组 / 行之和 9 = 合计 9；组内两张单合并、最早日期与在途天数有值 ✓");
}

/// 作废 / 已签收行**不进**汇总（它们是「不算在途」的状态）。
#[tokio::test]
async fn summary_ignores_cancelled_and_received() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let cust = customer_a().await;
    let open = fixture_shipment(cust, wh, "2026-11-10").await;
    let done = fixture_shipment(cust, wh, "2026-11-10").await;
    let void = fixture_shipment(cust, wh, "2026-11-10").await;
    insert_transit(open, mat, wh, "2026-11-10", "4", "0", "IN_TRANSIT").await;
    insert_transit(done, mat, wh, "2026-11-10", "9", "9", "RECEIVED").await;
    insert_transit(void, mat, wh, "2026-11-10", "6", "0", "CANCELLED").await;

    let summary = handler::summary_of(TransitSummaryFilter::default())
        .await
        .expect("汇总应成功");
    assert_eq!(summary.row_count, 1, "三行只归成一组");
    assert_eq!(summary.total_qty_in_transit, dec("4.0000"), "作废 / 已签收都不算在途");
    assert_eq!(summary.total_lines, 1, "汇总只统计未签收行");
    assert_eq!(summary.total_shipments, 1, "只统计那张未签收的发货单");
    assert_eq!(summary.rows[0].shipments, 1);
    println!("[M2-04] 汇总排除 RECEIVED / CANCELLED：在途 4（不是 4+0+6）✓");
}

/// 账龄由 PG 算，区间筛选含边界。
#[tokio::test]
async fn age_days_and_ship_date_range() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let cust = customer_a().await;
    // 老单（10 天前）+ 边界单（正好 5 天前）+ 新单（1 天前）；日期一律由库侧
    // `CURRENT_DATE - n` 算，测试跨零点 / 跨时区都不会漂。
    let old_ship = fixture_shipment_days_ago(cust, wh, 10).await;
    let edge_ship = fixture_shipment_days_ago(cust, wh, 5).await;
    let new_ship = fixture_shipment_days_ago(cust, wh, 1).await;
    insert_transit_days_ago(old_ship, mat, wh, 10, "5").await;
    insert_transit_days_ago(edge_ship, mat, wh, 5, "5").await;
    insert_transit_days_ago(new_ship, mat, wh, 1, "5").await;

    let page = handler::page_of(filter()).await.expect("分页");
    assert_eq!(page.total, 3);
    // 排序：最老在前（账龄视角）
    assert_eq!(page.list[0].shipment_id, old_ship.to_string(), "最老的排在前面");
    assert_eq!(
        page.list[0].age_days, 10,
        "在途天数 = CURRENT_DATE - ship_date（库侧算）"
    );
    assert_eq!(page.list[1].age_days, 5);
    assert_eq!(page.list[2].age_days, 1);

    // 区间筛选：`ship_date_to` = 5 天前 → 10 天前 + **边界当天**那两行（闭区间），1 天前的不算
    let boundary: String = db()
        .scalar_with_params::<String>("SELECT to_char(CURRENT_DATE - 5, 'YYYY-MM-DD')", vec![])
        .await
        .expect("边界日期");
    let ranged = handler::page_of(TransitFilter {
        ship_date_to: Some(boundary.clone()),
        ..filter()
    })
    .await
    .expect("区间筛选");
    assert_eq!(ranged.total, 2, "边界当天那行必须在结果里（闭区间）");
    assert_eq!(ranged.list[0].shipment_id, old_ship.to_string());
    assert_eq!(ranged.list[1].shipment_id, edge_ship.to_string());
    assert_eq!(ranged.list[1].ship_date, boundary, "ship_date_to 含当天（闭区间）");

    // 区间反了 → 400（静默 0 行是假数据）
    let error = handler::page_of(TransitFilter {
        ship_date_from: Some("2026-11-30".to_string()),
        ship_date_to: Some("2026-11-01".to_string()),
        ..filter()
    })
    .await
    .expect_err("区间反了必须报错");
    assert_eq!(error.status, 400);
    println!("[M2-04] age_days 由库算（10 / 1）；ship_date_to 边界含当天；区间反了 400 ✓");
}

/// 分页：`total` 是筛选后的总量、行数受 `page_size` 限制、排序稳定。
#[tokio::test]
async fn pagination_is_stable_and_counts_all() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let cust = customer_a().await;
    let mut ships = Vec::new();
    for _ in 0..3 {
        let ship = fixture_shipment(cust, wh, "2026-11-10").await;
        insert_transit(ship, mat, wh, "2026-11-10", "1", "0", "IN_TRANSIT").await;
        ships.push(ship);
    }

    let first = handler::page_of(TransitFilter {
        page: 1,
        page_size: 2,
        ..filter()
    })
    .await
    .expect("第 1 页");
    assert_eq!(first.total, 3, "total 是筛选后的总量，不是本页行数");
    assert_eq!(first.list.len(), 2, "page_size = 2");
    assert_eq!(first.page, 1);
    assert_eq!(first.page_size, 2);

    let second = handler::page_of(TransitFilter {
        page: 2,
        page_size: 2,
        ..filter()
    })
    .await
    .expect("第 2 页");
    assert_eq!(second.total, 3);
    assert_eq!(second.list.len(), 1, "第 2 页只剩 1 行");
    // 两页不重叠（排序稳定，不会同一行出现在两页里）
    let first_ids: Vec<&String> = first.list.iter().map(|row| &row.id).collect();
    let second_ids: Vec<&String> = second.list.iter().map(|row| &row.id).collect();
    assert!(
        first_ids.iter().all(|id| !second_ids.contains(id)),
        "两页必须没有重复行（排序带 id 兜底）"
    );
    // 两页并起来必须正好是那三张发货单（不多不少）
    let mut covered: Vec<String> = first.list.iter().map(|row| row.shipment_id.clone()).collect();
    covered.extend(second.list.iter().map(|row| row.shipment_id.clone()));
    for ship in &ships {
        assert!(covered.contains(&ship.to_string()), "发货单 {ship} 必须出现在两页之一");
    }
    println!("[M2-04] page_size=2：total=3 / 第1页2行 / 第2页1行 / 两页无重复且覆盖全部三行 ✓");
}

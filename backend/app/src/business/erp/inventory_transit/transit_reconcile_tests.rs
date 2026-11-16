// ============================================================================
// transit_reconcile_tests.rs —— 在途对账（M2-05）的真库用例
//
// 卡：t_b545b37a（M2-05【M2·仓库】在途库存：对账（在途 + 已出 = 发货总量））
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `three_stages_are_all_balanced` | 卡面恒等式：未签收 / 部分签收 / 全签收三种形态都满足「发货总量 = 已出 + 在途」 |
// | `quantity_mismatch_reports_diff` | 两侧都有行但发货量不等 → `QTY_MISMATCH` + 差额逐分给出来 |
// | `missing_transit_is_reported` | 有明细、没有在途行 → `MISSING_TRANSIT`（未纳入在途账；发货量单列） |
// | `orphan_transit_is_reported` | 有在途行、明细里没有该物料 → `ORPHAN_TRANSIT`（另一个方向也要查） |
// | `cancelled_only_is_flagged_not_mismatched` | 整批作废 → `CANCELLED_ONLY`，**不计入不平**、不进对账式 |
// | `multi_warehouse_rows_are_flagged` | 同一 (发货单, 物料) 多仓在途 → `MULTI_WAREHOUSE` |
// | `draft_shipments_are_out_of_scope` | 草稿 / 待过账单有明细但**不该**有在途行 ⇒ 不报差异（否则满屏假红） |
// | `filters_narrow_the_scope` | 物料 / 发货单 / 日期区间三个筛各自只命中该命中的组 |
// | `include_matched_controls_row_detail` | 缺省只给差异行；`include_matched=true` 连对平的组一起给 |
// | `identity_never_breaks_on_generated_column` | 生成列恒等（`shipped = received + in_transit`）在全表成立 |
//
// # 为什么夹具直接插单据 + 在途行（不走过账 / 签收）
//
// 过账与签收会把库存、凭证、应收、出库单一起牵动（M2-02/M2-03 已各自有用例钉住），
// 而本卡要造的是「**故意不平**的数据」—— 过账路径根本造不出 `QTY_MISMATCH` /
// `ORPHAN_TRANSIT`。直插的唯一风险是绕过 CHECK，而 V0420 的 4 条 CHECK 在库层兜住非法组合。
//
// 主数据一律走 `crate::test_seed::ensure_*`（P2-62 门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_xxx_reconcile" cargo test --lib \
//   business::erp::inventory_transit::transit_reconcile_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_transit::dto::{TransitReconcileFilter, TransitReconcileRes};
use crate::business::erp::inventory_transit::handler;
use crate::business::erp::inventory_transit::reconcile::{self, ReconcileQuery};
use crate::test_seed;

/// 真库用例必须串行（共用全局 `db()` 与同一批 `M205-` 夹具）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAREHOUSE_A_CODE: &str = "M205-WHA";
const WAREHOUSE_B_CODE: &str = "M205-WHB";
const MAT_A_CODE: &str = "M205-M1";
const MAT_B_CODE: &str = "M205-M2";
const CUSTOMER_CODE: &str = "M205-CA";

const SHIP_PREFIX: &str = "M205-SH-";
const SO_PREFIX: &str = "M205-SO-";

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "对账仓甲").await
}

async fn warehouse_b() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_B_CODE, "对账仓乙").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "对账物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "对账物料乙").await
}

async fn customer() -> Uuid {
    test_seed::ensure_customer(CUSTOMER_CODE, "对账客户甲").await
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

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量非法")
}

/// 清场：只删本模块**业务单据**（夹具客户下的订单 / 发货单 / 在途行）。
/// 主数据（客户 / 仓库 / 物料）由 `test_seed` 负责，**不删** —— 它们是跨模块复用的夹具。
async fn cleanup() {
    let orders = format!(
        "(SELECT id FROM biz_sales_order WHERE customer_id IN \
          (SELECT id FROM biz_customer WHERE code = '{CUSTOMER_CODE}'))"
    );
    let shipments = format!("(SELECT id FROM biz_shipment WHERE so_id IN {orders})");
    exec(
        &format!("DELETE FROM biz_inventory_in_transit WHERE shipment_id IN {shipments}"),
        vec![],
    )
    .await;
    exec(&format!("DELETE FROM biz_shipment WHERE so_id IN {orders}"), vec![]).await;
    exec(&format!("DELETE FROM biz_sales_order WHERE id IN {orders}"), vec![]).await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    warehouse_a().await;
    warehouse_b().await;
    mat_a().await;
    mat_b().await;
    customer().await;
    cleanup().await;
    Some(guard)
}

/// 造一张夹具发货单（订单 + 每物料一行订单行 + 每物料一行发货行）。
///
/// `status` 决定它参不参与对账（只有 `POSTED` 参与）；`items` 是 `(物料, 数量)`。
async fn fixture_shipment(warehouse_id: Uuid, ship_date: &str, status: &str, items: &[(Uuid, &str)]) -> Uuid {
    let customer_id = customer().await;
    let so_id = Uuid::now_v7();
    let ship_id = Uuid::now_v7();
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
    let total: Decimal = items.iter().map(|(_, qty)| dec(qty)).sum();
    exec(
        "INSERT INTO biz_shipment \
            (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty) \
         VALUES (?, ?, ?, ?, ?, ?::text::date, ?, ?::text::numeric)",
        vec![
            Value::from(ship_id),
            Value::from(format!("{SHIP_PREFIX}{}", &ship_id.to_string()[24..])),
            Value::from(&so_id),
            Value::from(&customer_id),
            Value::from(&warehouse_id),
            Value::Text(ship_date.to_string()),
            Value::from(status),
            Value::Text(total.to_string()),
        ],
    )
    .await;
    for (index, (material_id, qty)) in items.iter().enumerate() {
        let so_item_id = Uuid::now_v7();
        let line_no = i32::try_from(index + 1).unwrap_or(1);
        exec(
            "INSERT INTO biz_sales_order_item \
                (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
             VALUES (?, ?, ?, ?, ?::text::numeric, 10, 0, 0, 0, 0)",
            vec![
                Value::from(so_item_id),
                Value::from(&so_id),
                Value::I32(line_no),
                Value::from(material_id),
                Value::Text((*qty).to_string()),
            ],
        )
        .await;
        exec(
            "INSERT INTO biz_shipment_item (id, shipment_id, so_item_id, material_id, qty) \
             VALUES (?, ?, ?, ?, ?::text::numeric)",
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&ship_id),
                Value::from(&so_item_id),
                Value::from(material_id),
                Value::Text((*qty).to_string()),
            ],
        )
        .await;
    }
    ship_id
}

/// 插一行在途账（`RECEIVED` 自动补 `received_at`；`CANCELLED` 不受数量一致性约束）。
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

/// 只按**发货单**对账（把本用例的夹具与库里别的数据隔开）。
async fn reconcile_shipment(shipment_id: Uuid) -> TransitReconcileRes {
    handler::reconcile_of(TransitReconcileFilter {
        shipment_id: Some(shipment_id.to_string()),
        include_matched: Some("true".to_string()),
        ..TransitReconcileFilter::default()
    })
    .await
    .expect("按发货单对账应成功")
}

// ==================== 用例 ====================

/// 卡面恒等式：未签收 / 部分签收 / 全签收三种形态都「发货总量 = 已出 + 在途」。
#[tokio::test]
async fn three_stages_are_all_balanced() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;

    // ① 未签收：发货 100，在途 100
    let open = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat, "100")]).await;
    insert_transit(open, mat, wh, "2026-11-10", "100", "0", "IN_TRANSIT").await;
    // ② 部分签收：发货 100，已出 40，在途 60
    let partial = fixture_shipment(wh, "2026-11-11", "POSTED", &[(mat, "100")]).await;
    insert_transit(partial, mat, wh, "2026-11-11", "100", "40", "IN_TRANSIT").await;
    // ③ 全签收：发货 100，已出 100，在途 0
    let done = fixture_shipment(wh, "2026-11-12", "POSTED", &[(mat, "100")]).await;
    insert_transit(done, mat, wh, "2026-11-12", "100", "100", "RECEIVED").await;

    for (label, ship, received, open_qty) in [
        ("未签收", open, "0", "100"),
        ("部分签收", partial, "40", "60"),
        ("全签收", done, "100", "0"),
    ] {
        let report = reconcile_shipment(ship).await;
        assert!(report.balanced, "{label} 应对平：{report:?}");
        assert_eq!(report.mismatched, 0, "{label} 不该有硬问题");
        assert_eq!(report.rows.len(), 1, "{label} include_matched 应该给 1 行");
        let row = &report.rows[0];
        assert_eq!(row.item_qty, dec("100"), "{label} 发货总量取明细");
        assert_eq!(row.received_qty, dec(received), "{label} 已出");
        assert_eq!(row.qty_in_transit, dec(open_qty), "{label} 在途");
        assert_eq!(row.diff, Decimal::ZERO, "{label} 差额必须 0");
        assert!(row.issues.is_empty(), "{label} issues={:?}", row.issues);
        assert!(row.matched);
        assert_eq!(row.ship_no.as_deref().map(|no| no.starts_with(SHIP_PREFIX)), Some(true));
        assert_eq!(row.material_code.as_deref(), Some(MAT_A_CODE), "物料编码必须下发");
        assert_eq!(
            row.warehouse_code.as_deref(),
            Some(WAREHOUSE_A_CODE),
            "仓库编码必须下发"
        );
    }

    // 三张单合起来：发货 300 = 已出 140 + 在途 160
    let all = handler::reconcile_of(TransitReconcileFilter {
        material_id: Some(mat.to_string()),
        ..TransitReconcileFilter::default()
    })
    .await
    .expect("按物料对账");
    assert_eq!(all.totals.item_qty, dec("300"));
    assert_eq!(all.totals.received_qty, dec("140"));
    assert_eq!(all.totals.in_transit_qty, dec("160"));
    assert_eq!(all.totals.diff, Decimal::ZERO, "整体恒等");
    assert!(all.balanced);
    cleanup().await;
    println!("[M2-05] 未签收 / 部分签收 / 全签收三形态都对平；合计 300 = 140 + 160 ✓");
}

/// 两侧都有行但发货量不等 → `QTY_MISMATCH`，差额逐分给出来。
#[tokio::test]
async fn quantity_mismatch_reports_diff() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    // 明细发了 100，在途账只记了 90（模拟过账漏写 / 被手工改小）
    let ship = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat, "100")]).await;
    insert_transit(ship, mat, wh, "2026-11-10", "90", "30", "IN_TRANSIT").await;

    let report = reconcile_shipment(ship).await;
    assert!(!report.balanced, "有硬问题不能声称对平");
    assert_eq!(report.mismatched, 1);
    let row = &report.rows[0];
    assert_eq!(row.issues, vec![reconcile::ISSUE_QTY_MISMATCH.to_string()]);
    assert_eq!(row.issue_labels.len(), 1, "每个码都要有中文说明");
    assert!(row.issue_labels[0].contains("发货量"), "文案：{}", row.issue_labels[0]);
    assert_eq!(row.item_qty, dec("100"));
    assert_eq!(row.shipped_qty, dec("90"));
    assert_eq!(row.diff, dec("10"), "差额 = 100 − (30 + 60)");
    assert_eq!(report.totals.diff, dec("10"), "整体差额跟着这组走");
    cleanup().await;
    println!("[M2-05] 明细 100 / 在途账 90 → QTY_MISMATCH，差额 10 逐分给出 ✓");
}

/// 有明细、没有在途行 → `MISSING_TRANSIT`；发货量进 `untracked_*` 不进对账式。
#[tokio::test]
async fn missing_transit_is_reported() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    // 已过账、但没有在途账记录（M2-02 之前的过账形态 / 写入被绕过）
    let ship = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat, "50")]).await;

    let report = reconcile_shipment(ship).await;
    assert_eq!(report.mismatched, 1, "缺在途行是硬问题");
    assert!(!report.balanced);
    let row = &report.rows[0];
    assert_eq!(row.issues, vec![reconcile::ISSUE_MISSING_TRANSIT.to_string()]);
    assert_eq!(row.item_qty, dec("50"));
    assert!(row.warehouse_id.is_none(), "没有在途行时仓库为空（来源是单头）");
    assert_eq!(report.totals.untracked, 1);
    assert_eq!(report.totals.untracked_item_qty, dec("50"), "未纳入的发货量单列");
    assert_eq!(report.totals.item_qty, Decimal::ZERO, "对账式不含未纳入的组");
    cleanup().await;
    println!("[M2-05] 有明细无在途行 → MISSING_TRANSIT；50 进 untracked_item_qty ✓");
}

/// 有在途行、明细里没有该物料 → `ORPHAN_TRANSIT`（另一个方向也要查）。
#[tokio::test]
async fn orphan_transit_is_reported() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let (mat_1, mat_2) = (mat_a().await, mat_b().await);
    // 明细只有物料甲；在途账里却多出一行物料乙（明细被删 / 挂错物料）
    let ship = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat_1, "10")]).await;
    insert_transit(ship, mat_1, wh, "2026-11-10", "10", "0", "IN_TRANSIT").await;
    insert_transit(ship, mat_2, wh, "2026-11-10", "4", "0", "IN_TRANSIT").await;

    let report = reconcile_shipment(ship).await;
    assert_eq!(report.checked, 2, "两个物料各一组");
    assert_eq!(report.mismatched, 1);
    let orphan = report
        .rows
        .iter()
        .find(|row| row.material_id == mat_2.to_string())
        .expect("物料乙必须被报出来（LEFT JOIN 写法会漏掉这一侧）");
    assert_eq!(orphan.issues, vec![reconcile::ISSUE_ORPHAN_TRANSIT.to_string()]);
    assert_eq!(orphan.item_qty, Decimal::ZERO);
    assert_eq!(orphan.qty_in_transit, dec("4"));
    assert_eq!(orphan.diff, dec("-4"), "多记的在途量差额为负");
    cleanup().await;
    println!("[M2-05] 在途行多出物料乙 → ORPHAN_TRANSIT（两个方向都查）✓");
}

/// 整批作废：只有在途作废行 → `CANCELLED_ONLY`，不算不平、不进对账式。
#[tokio::test]
async fn cancelled_only_is_flagged_not_mismatched() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let ship = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat, "10")]).await;
    insert_transit(ship, mat, wh, "2026-11-10", "10", "0", "CANCELLED").await;

    let report = reconcile_shipment(ship).await;
    assert_eq!(report.mismatched, 0, "作废不是算错");
    assert!(report.balanced, "作废组不进对账式 ⇒ 不该把 balanced 拖红");
    let row = &report.rows[0];
    assert_eq!(row.issues, vec![reconcile::ISSUE_CANCELLED_ONLY.to_string()]);
    assert!(!row.matched, "但要标注出来（不是「对平」）");
    assert_eq!(row.cancelled_qty, dec("10"));
    assert_eq!(row.qty_in_transit, Decimal::ZERO, "作废行不算在途");
    assert_eq!(row.received_qty, Decimal::ZERO, "作废行不算已出");
    assert_eq!(report.totals.cancelled, 1);
    assert_eq!(report.totals.cancelled_qty, dec("10"));
    cleanup().await;
    println!("[M2-05] 整批作废 → CANCELLED_ONLY（标注，不计不平、不进对账式）✓");
}

/// 同一 (发货单, 物料) 在多仓都有在途行 → `MULTI_WAREHOUSE`（同一批货记两份）。
#[tokio::test]
async fn multi_warehouse_rows_are_flagged() {
    let Some(_guard) = setup().await else { return };
    let (wh_a, wh_b) = (warehouse_a().await, warehouse_b().await);
    let mat = mat_a().await;
    let ship = fixture_shipment(wh_a, "2026-11-10", "POSTED", &[(mat, "10")]).await;
    insert_transit(ship, mat, wh_a, "2026-11-10", "10", "0", "IN_TRANSIT").await;
    insert_transit(ship, mat, wh_b, "2026-11-10", "10", "0", "IN_TRANSIT").await;

    let report = reconcile_shipment(ship).await;
    assert_eq!(report.checked, 1, "同一物料只算一组");
    assert_eq!(report.mismatched, 1);
    let row = &report.rows[0];
    assert!(
        row.issues.contains(&reconcile::ISSUE_MULTI_WAREHOUSE.to_string()),
        "issues={:?}",
        row.issues
    );
    assert_eq!(row.qty_in_transit, dec("20"), "两行都在途 ⇒ 在途 20 而明细只有 10");
    cleanup().await;
    println!("[M2-05] 同单同物料两仓在途 → MULTI_WAREHOUSE（同一批货被记两份）✓");
}

/// 草稿 / 待过账的发货单有明细但**不该**有在途行 ⇒ 不参与对账（不报假差异）。
#[tokio::test]
async fn draft_shipments_are_out_of_scope() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let draft = fixture_shipment(wh, "2026-11-10", "DRAFT", &[(mat, "30")]).await;
    let pending = fixture_shipment(wh, "2026-11-10", "PENDING_POST", &[(mat, "30")]).await;

    for ship in [draft, pending] {
        let report = reconcile_shipment(ship).await;
        assert_eq!(report.checked, 0, "未过账的单不进对账（否则满屏 MISSING_TRANSIT）");
        assert!(report.balanced, "空范围也算对平");
        assert!(report.rows.is_empty());
    }
    cleanup().await;
    println!("[M2-05] DRAFT / PENDING_POST 单不参与对账（明细侧恒带 s.status='POSTED'）✓");
}

/// 物料 / 发货单 / 日期区间三个筛各自只命中该命中的组。
#[tokio::test]
async fn filters_narrow_the_scope() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let (mat_1, mat_2) = (mat_a().await, mat_b().await);
    let ship_a = fixture_shipment(wh, "2026-11-05", "POSTED", &[(mat_1, "5")]).await;
    let ship_b = fixture_shipment(wh, "2026-11-20", "POSTED", &[(mat_2, "7")]).await;
    insert_transit(ship_a, mat_1, wh, "2026-11-05", "5", "0", "IN_TRANSIT").await;
    insert_transit(ship_b, mat_2, wh, "2026-11-20", "7", "0", "IN_TRANSIT").await;

    let by_material = handler::reconcile_of(TransitReconcileFilter {
        material_id: Some(mat_2.to_string()),
        include_matched: Some("true".to_string()),
        ..TransitReconcileFilter::default()
    })
    .await
    .expect("按物料");
    assert_eq!(by_material.checked, 1, "物料乙只有一组");
    assert_eq!(by_material.rows[0].shipment_id, ship_b.to_string());

    let by_date = handler::reconcile_of(TransitReconcileFilter {
        material_id: Some(mat_1.to_string()),
        ship_date_from: Some("2026-11-10".to_string()),
        ship_date_to: Some("2026-11-30".to_string()),
        include_matched: Some("true".to_string()),
        ..TransitReconcileFilter::default()
    })
    .await
    .expect("按日期区间");
    assert_eq!(by_date.checked, 0, "物料甲的 11-05 单不在区间内");

    // 区间边界闭区间：正好 = 发货日
    let boundary = handler::reconcile_of(TransitReconcileFilter {
        material_id: Some(mat_1.to_string()),
        ship_date_from: Some("2026-11-05".to_string()),
        ship_date_to: Some("2026-11-05".to_string()),
        include_matched: Some("true".to_string()),
        ..TransitReconcileFilter::default()
    })
    .await
    .expect("边界当天");
    assert_eq!(boundary.checked, 1, "闭区间含当天");
    assert_eq!(boundary.rows[0].shipment_id, ship_a.to_string());
    assert_eq!(boundary.rows[0].ship_date.as_deref(), Some("2026-11-05"));
    cleanup().await;
    println!("[M2-05] 物料 / 日期区间（闭边界）各自收窄；区间外的单不混入 ✓");
}

/// `include_matched`：缺省只给差异行，`true` 连对平的组一起给。
#[tokio::test]
async fn include_matched_controls_row_detail() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let good = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat, "5")]).await;
    insert_transit(good, mat, wh, "2026-11-10", "5", "0", "IN_TRANSIT").await;
    let bad = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat, "5")]).await;
    insert_transit(bad, mat, wh, "2026-11-10", "4", "0", "IN_TRANSIT").await;

    // 按物料把两张单一起捞（同一物料），缺省只给差异那张
    let only_diff = handler::reconcile_of(TransitReconcileFilter {
        material_id: Some(mat.to_string()),
        ..TransitReconcileFilter::default()
    })
    .await
    .expect("缺省");
    assert_eq!(only_diff.checked, 2, "两组都核了");
    assert_eq!(only_diff.matched, 1);
    assert_eq!(only_diff.mismatched, 1);
    assert_eq!(only_diff.rows.len(), 1, "缺省只返回有差异的组");
    assert_eq!(only_diff.rows[0].shipment_id, bad.to_string());

    let both = handler::reconcile_of(TransitReconcileFilter {
        material_id: Some(mat.to_string()),
        include_matched: Some("1".to_string()),
        ..TransitReconcileFilter::default()
    })
    .await
    .expect("include_matched=true");
    assert_eq!(both.rows.len(), 2, "include_matched=1 连对平的组一起给");
    assert_eq!(both.rows.iter().filter(|row| row.matched).count(), 1);
    cleanup().await;
    println!("[M2-05] 缺省 1 行（差异）/ include_matched=1 → 2 行（含对平）✓");
}

/// 生成列恒等：全表 `shipped_qty = received_qty + qty_in_transit`（绕过应用也写不出别的）。
#[tokio::test]
async fn identity_never_breaks_on_generated_column() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse_a().await;
    let mat = mat_a().await;
    let ship = fixture_shipment(wh, "2026-11-10", "POSTED", &[(mat, "10")]).await;
    insert_transit(ship, mat, wh, "2026-11-10", "10", "3", "IN_TRANSIT").await;

    let broken: i64 = db()
        .scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_inventory_in_transit \
              WHERE shipped_qty <> received_qty + qty_in_transit",
            vec![],
        )
        .await
        .expect("恒等巡检");
    assert_eq!(broken, 0, "生成列口径被绕过 ⇒ V0420 的 GENERATED 定义出问题了");

    // 顺带钉住：扫描函数读出来的组合与直接 SQL 的聚合一致（口径只有一份）
    let (combos, truncated) = reconcile::scan(&ReconcileQuery {
        shipment_id: Some(ship),
        ..ReconcileQuery::default()
    })
    .await
    .expect("扫描");
    assert!(!truncated);
    assert_eq!(combos.len(), 1);
    assert_eq!(combos[0].shipped_qty, dec("10"));
    assert_eq!(combos[0].received_qty, dec("3"));
    assert_eq!(combos[0].qty_in_transit, dec("7"));
    assert!(!combos[0].identity_broken);
    cleanup().await;
    println!("[M2-05] 全表恒等 0 行破例；scan 读出的三侧数量与库一致 ✓");
}

// ============================================================================
// replenish_db_tests.rs —— 补货建议（M2-15）的真库用例
//
// 卡：t_527ceaa3（M2-15【M2·仓库】补货建议：安全库存 + 在途 + 未清订单）
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `suggestion_combines_stock_transit_and_open_po` | 卡面三项都参与算术：结存 30 − 在途 5 + 未清订单 10 → 预计 35；min 50 / max 100 → 建议 65 |
// | `outbound_transit_matches_transit_summary` | 「在途」取的就是 M2-04 的口径：本表 `outbound_transit_qty` 与在途汇总接口的 `qty_in_transit` 逐格相等 |
// | `open_po_counts_only_undelivered_of_receivable_status` | 未清订单 = SUBMITTED/APPROVED/PARTIAL_RECEIVED 且未收完；DRAFT / RECEIVED / CLOSED / CANCELLED / 软删 一律不计 |
// | `water_falls_back_to_material_safety_stock` | 阈值走 M2-13 的三级回退：无策略行 → 物料级 safety_stock |
// | `policy_zero_blocks_fallback` | 策略行 `min = 0` 是**明示不预警**，物料级 50 也不管用 → 无建议 |
// | `expected_equal_to_min_does_not_trigger` | 边界：预计可用 == 最低水位不触发（与 M2-14 的 `qty < min` 同一刀） |
// | `covered_rows_appear_only_with_include_covered` | 账面跌破水位但未清订单盖住 → 默认不出现；`include_covered=true` 才出现且结论是 COVERED、建议 0 |
// | `in_transit_warehouse_and_disabled_material_are_excluded` | 虚拟在途仓不可补货；停用物料不出现（与 M2-14 / reminder_stock 同口径） |
// | `pagination_and_total_are_consistent` | `total` 是全量、翻页不重不漏、按建议量降序 |
// | `keyword_filter_narrows_without_touching_other_materials` | 关键字只命中该物料，别的格子数量不受影响 |
// | `zz_teardown_purges_fixtures` | 清场：`M215-` 的库存行 / 水位行 / 在途 / 未清订单 / 单据 0 条 |
//
// # 为什么夹具直接插表（不走过账）
//
// 本卡是**只读**现算：要造的是「结存 × 发货在途 × 未清订单 × 水位来源」的组合，其中
// 「在途 5 而账面还是 30」这种状态本身就要走两步过账（发货过账 → 未签收）才自然出现，
// 而组合有十几种。直插的风险由库层约束兜着：`biz_inventory_in_transit` 的
// `ck_biz_inv_transit_qty` / `ck_biz_inv_transit_state`（状态与数量必须自洽）、
// `biz_purchase_order_item.ck_biz_po_item_recv`（`received_qty <= qty`）、
// `biz_material_stock_policy` 的三条 CHECK。
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m215_drill" cargo test --lib \
//   business::erp::replenish -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::inventory_transit::dto::TransitSummaryFilter;
use crate::business::erp::inventory_transit::handler as transit_handler;
use crate::business::erp::replenish::dto::{ReplenishFilter, VERDICT_COVERED, VERDICT_PURCHASE};
use crate::business::erp::replenish::handler;
use crate::business::erp::stock_policy::dto::{STATUS_DISABLED, STATUS_ENABLED};
use crate::test_seed;

const WAREHOUSE_CODE: &str = "M215-WHA";
const TRANSIT_WAREHOUSE_CODE: &str = "M215-WHT";
const MAT_A_CODE: &str = "M215-M1";
const MAT_B_CODE: &str = "M215-M2";
const MAT_OFF_CODE: &str = "M215-M9";
const SUPPLIER_CODE: &str = "M215-SUP";
const CUSTOMER_CODE: &str = "M215-CUS";

/// 夹具单号前缀（清场按它反查，绝不按「生产号段」删）。
const DOC_PREFIX: &str = "M215-";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_CODE, "补货建议仓").await
}

/// 在途仓（虚拟记账仓，`warehouse_type = 'IN_TRANSIT'`）：本表必须排除它。
async fn transit_warehouse() -> Uuid {
    let id = test_seed::ensure_warehouse(TRANSIT_WAREHOUSE_CODE, "补货建议在途仓").await;
    exec(
        "UPDATE biz_warehouse SET warehouse_type = 'IN_TRANSIT' WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await;
    id
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "补货建议物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "补货建议物料乙").await
}

async fn mat_off() -> Uuid {
    let id = test_seed::ensure_material(MAT_OFF_CODE, "补货建议停用物料").await;
    exec(
        "UPDATE biz_material SET status = 0 WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await;
    id
}

/// 结存（`biz_inventory` 先 UPDATE、没命中再 INSERT —— 不删不重建：行 id 是建议行的身份）。
async fn set_qty(material_id: Uuid, warehouse_id: Uuid, qty: &str) {
    let affected = db()
        .execute_with_params(
            "UPDATE biz_inventory SET qty = ?::text::numeric, updated_at = now() \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![
                Value::Text(qty.to_string()),
                Value::from(&material_id),
                Value::from(&warehouse_id),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("更新结存失败：{error}"));
    if affected == 0 {
        exec(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, qty, created_at, updated_at) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::text::numeric, now(), now())",
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::Text(qty.to_string()),
            ],
        )
        .await;
    }
}

/// 多加一个**库位成本桶**（`biz_inventory` 的第二行，`location_id` 指向一条库位账行）。
///
/// 用来钉住「一格 = 物料 × 仓库，成本桶必须合计」：真实仓库按库位分成本时会自然出现这种行。
async fn add_cost_bucket(material_id: Uuid, warehouse_id: Uuid, qty: &str) {
    let bucket_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, 0)",
        vec![
            Value::from(&bucket_id),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::from(&Uuid::now_v7()), // 库位 id 是弱引用（无外键），造一个即可
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, ?::text::numeric, 0, 0, 0)",
        vec![
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::from(&bucket_id),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
}

/// 物料级安全库存（策略行缺席时的回退值）。
async fn set_material_safety(material_id: Uuid, value: &str) {
    exec(
        "UPDATE biz_material SET safety_stock = ?::text::numeric WHERE id = ?::uuid",
        vec![Value::Text(value.to_string()), Value::from(&material_id)],
    )
    .await;
}

/// 一格的策略行（`uk_biz_msp_mat_wh` 只看未删行，先删后插即「重置」）。
async fn set_policy(material_id: Uuid, warehouse_id: Uuid, min: &str, max: Option<&str>, status: i16) {
    clear_policy(material_id, warehouse_id).await;
    exec(
        "INSERT INTO biz_material_stock_policy (material_id, warehouse_id, min_qty, max_qty, status) \
         VALUES (?::uuid, ?::uuid, ?::text::numeric, NULLIF(?::text, '')::numeric, ?::text::smallint)",
        vec![
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(min.to_string()),
            Value::Text(max.unwrap_or_default().to_string()),
            Value::Text(status.to_string()),
        ],
    )
    .await;
}

async fn clear_policy(material_id: Uuid, warehouse_id: Uuid) {
    exec(
        "DELETE FROM biz_material_stock_policy WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await;
}

/// 造一条发货在途（客户 → 销售订单 → 发货单 → 在途行，链条与真实过账同形）。
///
/// 返回在途行 id。`status = 'RECEIVED'` 时按 `ck_biz_inv_transit_state` 补齐
/// `received_qty = shipped_qty` 与 `received_at`。
async fn add_transit(material_id: Uuid, warehouse_id: Uuid, shipped: &str, received: &str, status: &str) -> Uuid {
    let customer = test_seed::ensure_customer(CUSTOMER_CODE, "补货建议客户").await;
    let suffix = test_seed::uniq_suffix();
    let so_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, 'APPROVED')",
        vec![
            Value::from(&so_id),
            Value::Text(format!("{DOC_PREFIX}SO-{suffix}")),
            Value::from(&customer),
        ],
    )
    .await;
    let shipment_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status) \
         VALUES (?::uuid, ?, ?::uuid, ?::uuid, ?::uuid, CURRENT_DATE, 'POSTED')",
        vec![
            Value::from(&shipment_id),
            Value::Text(format!("{DOC_PREFIX}SH-{suffix}")),
            Value::from(&so_id),
            Value::from(&customer),
            Value::from(&warehouse_id),
        ],
    )
    .await;
    let received_at = if status == "RECEIVED" {
        "now()"
    } else {
        "NULL::timestamptz"
    };
    exec(
        &format!(
            "INSERT INTO biz_inventory_in_transit \
                 (id, shipment_id, material_id, warehouse_id, ship_date, shipped_qty, received_qty, status, received_at) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, CURRENT_DATE, ?::text::numeric, \
                     ?::text::numeric, ?, {received_at})"
        ),
        vec![
            Value::from(&shipment_id),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(shipped.to_string()),
            Value::Text(received.to_string()),
            Value::Text(status.to_string()),
        ],
    )
    .await;
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_inventory_in_transit \
              WHERE shipment_id = ?::uuid AND material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![
                Value::from(&shipment_id),
                Value::from(&material_id),
                Value::from(&warehouse_id),
            ],
        )
        .await
        .expect("查在途行");
    rows.first()
        .expect("在途行必须存在")
        .get_typed("id")
        .expect("在途行 id")
}

/// 造一张采购订单 + 一行明细（订购量 `qty`），返回订单 id。
///
/// **已收数量不在这里写**：`biz_purchase_order_item.received_qty` 由 V0217 删除，
/// 已收数量只从「已过账收货单」现算（`purchase_model::POSTED_RECEIVED_SQL`）。
/// 要造「部分到货」请配合 [`post_receipt`]。
///
/// `deleted = true` → 软删整单（软删的单不算在途：`po.deleted_at IS NULL` 是扫描条件）。
async fn add_po(material_id: Uuid, warehouse_id: Uuid, qty: &str, status: &str, deleted: bool) -> Uuid {
    let supplier = test_seed::ensure_supplier(SUPPLIER_CODE, "补货建议供应商").await;
    let unit = test_seed::ensure_unit().await;
    let order_id = Uuid::now_v7();
    let deleted_at = if deleted { "now()" } else { "NULL::timestamptz" };
    exec(
        &format!(
            "INSERT INTO biz_purchase_order \
                 (id, order_no, supplier_id, order_date, warehouse_id, status, deleted_at) \
             VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?::uuid, ?, {deleted_at})"
        ),
        vec![
            Value::from(&order_id),
            Value::Text(format!("{DOC_PREFIX}PO-{}", test_seed::uniq_suffix())),
            Value::from(&supplier),
            Value::from(&warehouse_id),
            Value::Text(status.to_string()),
        ],
    )
    .await;
    exec(
        "INSERT INTO biz_purchase_order_item \
             (order_id, line_no, material_id, unit_id, qty, price) \
         VALUES (?::uuid, 1, ?::uuid, ?::uuid, ?::text::numeric, 1)",
        vec![
            Value::from(&order_id),
            Value::from(&material_id),
            Value::from(&unit),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    order_id
}

/// 给采购订单的第一行「过账一张收货单」（`status = POSTED`），= 已收 `qty`。
///
/// 只走**唯一真相**那条路（V0217）：已收数量由已过账收货单现算，所以夹具也必须造收货单，
/// 不许往采购明细上写一个数（那一列已经没有）。
async fn post_receipt(order_id: Uuid, material_id: Uuid, warehouse_id: Uuid, qty: &str) {
    let unit = test_seed::ensure_unit().await;
    let supplier = test_seed::ensure_supplier(SUPPLIER_CODE, "补货建议供应商").await;
    let rows = db()
        .find_with_params(
            "SELECT id FROM biz_purchase_order_item WHERE order_id = ?::uuid ORDER BY line_no ASC LIMIT 1",
            vec![Value::from(&order_id)],
        )
        .await
        .expect("查采购明细行");
    let item_id: Uuid = rows
        .first()
        .expect("采购明细行必须存在")
        .get_typed("id")
        .expect("明细行 id");
    let stock_in_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_stock_in \
             (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, total_qty) \
         VALUES (?::uuid, ?, 'PURCHASE', ?::uuid, ?::uuid, ?::uuid, CURRENT_DATE, 'POSTED', ?::text::numeric)",
        vec![
            Value::from(&stock_in_id),
            Value::Text(format!("{DOC_PREFIX}IN-{}", test_seed::uniq_suffix())),
            Value::from(&order_id),
            Value::from(&supplier),
            Value::from(&warehouse_id),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    exec(
        "INSERT INTO biz_stock_in_item \
             (stock_in_id, line_no, source_item_id, material_id, unit_id, qty) \
         VALUES (?::uuid, 1, ?::uuid, ?::uuid, ?::uuid, ?::text::numeric)",
        vec![
            Value::from(&stock_in_id),
            Value::from(&item_id),
            Value::from(&material_id),
            Value::from(&unit),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
}

/// 清场：只删本模块造的库存行 / 水位行 / 在途 / 未清订单 / 单据（主数据由 `ensure_*` 复用）。
async fn cleanup() {
    let mats = format!("'{MAT_A_CODE}', '{MAT_B_CODE}', '{MAT_OFF_CODE}'");
    // 在途 → 发货单 → 销售订单（发货单对销售订单是 RESTRICT 外键，顺序不能反）
    exec(
        &format!(
            "DELETE FROM biz_inventory_in_transit WHERE material_id IN \
              (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_shipment WHERE ship_no LIKE '{DOC_PREFIX}%'"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_sales_order WHERE so_no LIKE '{DOC_PREFIX}%'"),
        vec![],
    )
    .await;
    // 已过账收货单（「已收数量」的唯一真相）→ 明细随单头 CASCADE
    exec(
        &format!("DELETE FROM biz_stock_in WHERE in_no LIKE '{DOC_PREFIX}%'"),
        vec![],
    )
    .await;
    // 未清订单：明细 → 主单
    exec(
        &format!(
            "DELETE FROM biz_purchase_order_item WHERE order_id IN \
              (SELECT id FROM biz_purchase_order WHERE order_no LIKE '{DOC_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_purchase_order WHERE order_no LIKE '{DOC_PREFIX}%'"),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_inventory WHERE material_id IN \
              (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
        ),
        vec![],
    )
    .await;
    // 库位账行（成本桶测试造的）必须在本仓库存行删掉之后再删（FK: biz_inventory.location_id）
    exec(
        &format!(
            "DELETE FROM biz_inventory_location WHERE material_id IN \
              (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_material_stock_policy WHERE material_id IN \
              (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
        ),
        vec![],
    )
    .await;
    exec(
        &format!("UPDATE biz_material SET safety_stock = 0, status = 1 WHERE material_code IN ({mats})"),
        vec![],
    )
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    warehouse().await;
    transit_warehouse().await;
    mat_a().await;
    mat_b().await;
    mat_off().await;
    cleanup().await;
    Some(guard)
}

/// 只查这一格（物料 + 仓库限定），默认开关。
async fn one_cell(material_id: Uuid, warehouse_id: Uuid) -> Vec<crate::business::erp::replenish::dto::ReplenishRowRes> {
    handler::replenish_page_of(ReplenishFilter {
        material_id: Some(material_id.to_string()),
        warehouse_id: Some(warehouse_id.to_string()),
        ..Default::default()
    })
    .await
    .expect("跑补货建议")
    .rows
}

// ============================================================================
// 用例
// ============================================================================

/// ① 卡面的主场景：安全库存 + 在途 + 未清订单 → 建议采购量，四舍五入一律不碰。
#[tokio::test]
async fn suggestion_combines_stock_transit_and_open_po() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", Some("100"), STATUS_ENABLED).await;
    set_qty(material, wh, "30").await;
    add_transit(material, wh, "5", "0", "IN_TRANSIT").await;
    add_po(material, wh, "10", "APPROVED", false).await;

    let rows = one_cell(material, wh).await;
    assert_eq!(rows.len(), 1, "一格一条建议");
    let row = &rows[0];
    assert_eq!(row.on_hand_qty, dec("30"), "账面结存");
    assert_eq!(row.outbound_transit_qty, dec("5"), "发货在途 5（账面还没扣）");
    assert_eq!(row.open_po_qty, dec("10"), "未清订单 10");
    assert_eq!(row.available_qty, dec("25"), "可用 = 30 − 5");
    assert_eq!(row.expected_qty, dec("35"), "预计 = 25 + 10");
    assert_eq!(row.min_qty, Some(dec("50")));
    assert_eq!(row.min_source, "POLICY");
    assert_eq!(row.max_qty, Some(dec("100")));
    assert_eq!(row.target_qty, Some(dec("100")), "补到最高水位");
    assert_eq!(row.target_source, "MAX");
    assert_eq!(row.shortage_qty, dec("65"));
    assert_eq!(row.suggested_qty, dec("65"), "建议 = 100 − 35");
    assert_eq!(row.verdict, VERDICT_PURCHASE);
    assert!(row.triggered && row.below_min);
    assert_eq!(row.verdict_label, "建议采购");
    assert_eq!(row.material_code.as_deref(), Some(MAT_A_CODE), "名字走实时 join");
    assert_eq!(row.warehouse_name.as_deref(), Some("补货建议仓"));
    assert_eq!(row.inventory_row_count, 1, "一格一个成本桶");
    assert!(!row.material_id.is_empty() && !row.warehouse_id.is_empty());
    println!("[M2-15] 结存 30 − 在途 5 + 未清订单 10 → 预计 35；min 50 / max 100 → 建议 65 ✓");
}

/// ② 「一格 = 物料 × 仓库」：同一个仓里两个成本桶必须合计成一行（否则账面 35 被看成 30 和 5 两格）。
#[tokio::test]
async fn cost_buckets_in_one_warehouse_are_summed() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", Some("100"), STATUS_ENABLED).await;
    set_qty(material, wh, "30").await;
    add_cost_bucket(material, wh, "5").await;

    let rows = one_cell(material, wh).await;
    assert_eq!(rows.len(), 1, "两个成本桶只出一行：{rows:?}");
    assert_eq!(rows[0].on_hand_qty, dec("35"), "30 + 5 按仓合计");
    assert_eq!(rows[0].inventory_row_count, 2, "桶数可复核");
    assert_eq!(rows[0].suggested_qty, dec("65"), "100 − 35");
    println!("[M2-15] 同仓两个成本桶合计成一行（30 + 5 = 35，建议 65）✓");
}

/// ③ 「在途」与 M2-04 的口径逐格相等（同一批数据、两个接口，不许各算一遍）。
#[tokio::test]
async fn outbound_transit_matches_transit_summary() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    set_policy(material, wh, "100", None, STATUS_ENABLED).await;
    set_qty(material, wh, "40").await;
    // 两条在途：3 + 4.5 = 7.5；再放一条已签收的（不许算进来）
    add_transit(material, wh, "3", "0", "IN_TRANSIT").await;
    add_transit(material, wh, "4.5", "0", "IN_TRANSIT").await;
    add_transit(material, wh, "9", "9", "RECEIVED").await;

    let summary = transit_handler::summary_of(TransitSummaryFilter {
        material_id: Some(material.to_string()),
        warehouse_id: Some(wh.to_string()),
        customer_id: None,
    })
    .await
    .expect("在途汇总");
    let transit_qty: Decimal = summary
        .rows
        .iter()
        .filter(|row| row.material_id == material.to_string() && row.warehouse_id == wh.to_string())
        .map(|row| row.qty_in_transit)
        .sum();
    assert_eq!(transit_qty, dec("7.5"), "在途汇总：3 + 4.5（已签收 9 不算）");

    let rows = one_cell(material, wh).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].outbound_transit_qty, transit_qty,
        "补货建议的在途必须与在途汇总接口逐格相等（同一个口径，不许各算一遍）"
    );
    assert_eq!(rows[0].open_po_qty, Decimal::ZERO, "没有采购单 → 未清 0");
    assert_eq!(rows[0].suggested_qty, dec("67.5"), "100 − (40 − 7.5)");
    println!("[M2-15] outbound_transit_qty 与在途汇总接口逐格相等（7.5）✓");
}

/// ③ 未清订单的状态域：只有「已提交 / 已审批 / 部分到货 且未收完」才算供给。
#[tokio::test]
async fn open_po_counts_only_undelivered_of_receivable_status() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    set_policy(material, wh, "100", None, STATUS_ENABLED).await;
    set_qty(material, wh, "0").await;

    // 计入：4（部分到货，收 6 剩 4）+ 5（已审批未收）+ 6（已提交未收）= 15
    let partial = add_po(material, wh, "10", "PARTIAL_RECEIVED", false).await;
    post_receipt(partial, material, wh, "6").await;
    add_po(material, wh, "5", "APPROVED", false).await;
    add_po(material, wh, "6", "SUBMITTED", false).await;
    // 不计：草稿 / 已收满 / 已关闭 / 已作废 / 软删
    add_po(material, wh, "100", "DRAFT", false).await;
    let received = add_po(material, wh, "100", "RECEIVED", false).await;
    post_receipt(received, material, wh, "100").await; // 收满 → 未收 0
    add_po(material, wh, "100", "CLOSED", false).await;
    add_po(material, wh, "100", "CANCELLED", false).await;
    add_po(material, wh, "100", "APPROVED", true).await;

    let rows = one_cell(material, wh).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].open_po_qty, dec("15"), "10-6=4 + 5 + 6 = 15");
    assert_eq!(rows[0].expected_qty, dec("15"));
    assert_eq!(rows[0].suggested_qty, dec("85"), "100 − 15");
    println!("[M2-15] 未清订单只算 SUBMITTED/APPROVED/PARTIAL_RECEIVED 未收完；DRAFT/已收满/已关/作废/软删不计 ✓");
}

/// ④ 没有策略行 → 回退物料级 `safety_stock`（M2-13 的三级回退，本卡不重写）。
#[tokio::test]
async fn water_falls_back_to_material_safety_stock() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    clear_policy(material, wh).await;
    set_material_safety(material, "40").await;
    set_qty(material, wh, "20").await;

    let rows = one_cell(material, wh).await;
    assert_eq!(rows.len(), 1, "物料级水位也要给建议（回退生效）");
    assert_eq!(rows[0].min_qty, Some(dec("40")));
    assert_eq!(rows[0].min_source, "MATERIAL");
    assert_eq!(rows[0].max_qty, None, "物料级没有最高水位");
    assert_eq!(rows[0].target_source, "MIN", "未设 max → 补到 min");
    assert_eq!(rows[0].suggested_qty, dec("20"));
    println!("[M2-15] 无策略行 → 回退物料级 safety_stock 40，补到 40，建议 20 ✓");
}

/// ⑤ 策略行 `min = 0` 是**明示不预警**：阻断回退，物料级 50 也不管用。
#[tokio::test]
async fn policy_zero_blocks_fallback() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    set_material_safety(material, "50").await;
    set_policy(material, wh, "0", None, STATUS_ENABLED).await;
    set_qty(material, wh, "0").await;

    let rows = one_cell(material, wh).await;
    assert!(rows.is_empty(), "min = 0 → 没有目标水位、没有建议：{rows:?}");

    // 停用（0 号状态）则回退物料级 50 → 建议 50（与「从未设置过」相同）
    set_policy(material, wh, "0", None, STATUS_DISABLED).await;
    let rows = one_cell(material, wh).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].min_source, "MATERIAL");
    assert_eq!(rows[0].suggested_qty, dec("50"));
    println!("[M2-15] 策略 min=0 阻断回退（无建议）；策略停用则回退物料级 50 ✓");
}

/// ⑥ 边界：预计可用正好等于最低水位 → 不触发（与 M2-14 的 `qty < min` 同一刀）。
#[tokio::test]
async fn expected_equal_to_min_does_not_trigger() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", Some("80"), STATUS_ENABLED).await;
    // 结存 30 + 未清订单 20 = 50
    set_qty(material, wh, "30").await;
    add_po(material, wh, "20", "APPROVED", false).await;

    let rows = one_cell(material, wh).await;
    assert!(rows.is_empty(), "等于再订货点不补：{rows:?}");

    // 差 0.0001 就触发，且补到最高水位 80（80 − 49.9999）
    set_qty(material, wh, "29.9999").await;
    let rows = one_cell(material, wh).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].suggested_qty, dec("30.0001"));
    println!("[M2-15] 预计可用 == 最低水位不触发；差 0.0001 即补到最高水位（30.0001）✓");
}

/// ⑦ 账面跌破水位但未清订单盖住 → 默认不出现；`include_covered` 才出现（COVERED / 建议 0）。
#[tokio::test]
async fn covered_rows_appear_only_with_include_covered() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let material = mat_a().await;
    set_policy(material, wh, "50", None, STATUS_ENABLED).await;
    set_qty(material, wh, "30").await;
    add_po(material, wh, "40", "APPROVED", false).await;

    let default_rows = one_cell(material, wh).await;
    assert!(default_rows.is_empty(), "默认只返回「要下单」的行：{default_rows:?}");

    let report = handler::replenish_page_of(ReplenishFilter {
        material_id: Some(material.to_string()),
        warehouse_id: Some(wh.to_string()),
        include_covered: Some("true".to_string()),
        ..Default::default()
    })
    .await
    .expect("带覆盖行查询");
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.total, 1);
    assert_eq!(report.totals.purchase_row_count, 0);
    assert_eq!(report.totals.covered_row_count, 1);
    let row = &report.rows[0];
    assert_eq!(row.verdict, VERDICT_COVERED);
    assert_eq!(row.verdict_label, "在途覆盖");
    assert_eq!(row.suggested_qty, Decimal::ZERO, "被未清订单覆盖 → 不补");
    assert!(row.below_min, "账面 30 < 50");
    assert!(!row.triggered, "预计可用 70 >= 50");
    assert!(
        report.hints.iter().any(|hint| hint.contains("在途覆盖")),
        "要有一句解释这些行为什么不用下单：{:?}",
        report.hints
    );
    println!("[M2-15] COVERED：默认不出现；include_covered=true 才出现（建议 0 + 解释）✓");
}

/// ⑧ 在途仓（虚拟记账仓）与停用物料都不参与补货。
#[tokio::test]
async fn in_transit_warehouse_and_disabled_material_are_excluded() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let transit_wh = transit_warehouse().await;
    let material = mat_a().await;
    let off = mat_off().await;

    // 在途仓里的同一格：水位 100、结存 0 → 若参与就会给建议
    set_policy(material, transit_wh, "100", None, STATUS_ENABLED).await;
    set_qty(material, transit_wh, "0").await;
    // 停用物料：水位 100、结存 0 → 若参与就会给建议
    set_policy(off, wh, "100", None, STATUS_ENABLED).await;
    set_qty(off, wh, "0").await;
    // 对照组：正常物料正常仓，一样是 0 → 必须有建议（证明上面不是「全都不返回」）
    set_policy(material, wh, "100", None, STATUS_ENABLED).await;
    set_qty(material, wh, "0").await;

    let report = handler::replenish_page_of(ReplenishFilter::default())
        .await
        .expect("全量查询");
    let cells: Vec<(String, String)> = report
        .rows
        .iter()
        .map(|row| (row.material_id.clone(), row.warehouse_id.clone()))
        .collect();
    assert!(
        cells.contains(&(material.to_string(), wh.to_string())),
        "正常格子必须在结果里：{cells:?}"
    );
    assert!(
        !cells.contains(&(material.to_string(), transit_wh.to_string())),
        "在途仓不是可补货的实体仓"
    );
    assert!(!cells.iter().any(|(mat, _)| mat == &off.to_string()), "停用物料不出现");
    println!("[M2-15] 在途仓 / 停用物料被排除，正常格子照常给建议 ✓");
}

/// ⑨ 分页：`total` 是全量、翻页不重不漏、最急的在第一页。
#[tokio::test]
async fn pagination_and_total_are_consistent() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let a = mat_a().await;
    let b = mat_b().await;
    set_policy(a, wh, "100", None, STATUS_ENABLED).await;
    set_qty(a, wh, "10").await; // 建议 90
    set_policy(b, wh, "100", None, STATUS_ENABLED).await;
    set_qty(b, wh, "70").await; // 建议 30

    let first = handler::replenish_page_of(ReplenishFilter {
        warehouse_id: Some(wh.to_string()),
        page: 1,
        page_size: 1,
        ..Default::default()
    })
    .await
    .expect("第 1 页");
    assert_eq!(first.total, 2, "total 是分页前全量");
    assert_eq!(first.rows.len(), 1, "每页 1 条");
    assert_eq!(first.totals.suggested_qty_total, dec("120"), "合计按全量算（90 + 30）");
    assert_eq!(first.totals.material_count, 2);
    let first_mat = first.rows[0].material_id.clone();
    assert_eq!(first.rows[0].suggested_qty, dec("90"), "最急的在第一页");

    let second = handler::replenish_page_of(ReplenishFilter {
        warehouse_id: Some(wh.to_string()),
        page: 2,
        page_size: 1,
        ..Default::default()
    })
    .await
    .expect("第 2 页");
    assert_eq!(second.page, 2);
    assert_eq!(second.rows.len(), 1);
    assert_ne!(second.rows[0].material_id, first_mat, "两页不许重复同一行");

    let third = handler::replenish_page_of(ReplenishFilter {
        warehouse_id: Some(wh.to_string()),
        page: 3,
        page_size: 1,
        ..Default::default()
    })
    .await
    .expect("第 3 页");
    assert!(third.rows.is_empty(), "越界返回空页（不报错）");
    println!("[M2-15] 分页：total 全量、合计全量、翻页不重不漏、越界空页 ✓");
}

/// ⑩ 关键字只命中该物料，不影响别的格子（筛选条件写错最容易「看起来对」）。
#[tokio::test]
async fn keyword_filter_narrows_without_touching_other_materials() {
    let Some(_guard) = setup().await else { return };
    let wh = warehouse().await;
    let a = mat_a().await;
    let b = mat_b().await;
    set_policy(a, wh, "100", None, STATUS_ENABLED).await;
    set_qty(a, wh, "0").await;
    set_policy(b, wh, "100", None, STATUS_ENABLED).await;
    set_qty(b, wh, "0").await;

    let by_code = handler::replenish_page_of(ReplenishFilter {
        keyword: Some(MAT_A_CODE.to_string()),
        ..Default::default()
    })
    .await
    .expect("按编码筛");
    assert_eq!(by_code.total, 1, "只命中甲物料：{:?}", by_code.rows);
    assert_eq!(by_code.rows[0].material_code.as_deref(), Some(MAT_A_CODE));
    assert_eq!(by_code.rows[0].suggested_qty, dec("100"), "数量不受筛选影响");

    let by_name = handler::replenish_page_of(ReplenishFilter {
        keyword: Some("物料乙".to_string()),
        ..Default::default()
    })
    .await
    .expect("按名称筛");
    assert_eq!(by_name.total, 1);
    assert_eq!(by_name.rows[0].material_code.as_deref(), Some(MAT_B_CODE));

    let nothing = handler::replenish_page_of(ReplenishFilter {
        keyword: Some("不存在的物料名".to_string()),
        ..Default::default()
    })
    .await
    .expect("筛不到也不报错");
    assert_eq!(nothing.total, 0);
    assert!(
        nothing.hints.iter().any(|hint| hint.contains("没有扫到")),
        "空结果的解释要说清是「没有库存行 / 被筛空」：{:?}",
        nothing.hints
    );
    println!("[M2-15] 关键字（编码 / 名称）只收窄范围，不改数量；空结果有解释 ✓");
}

/// ⑪ 清场：本卡造的库存行 / 水位行 / 在途 / 未清订单 / 单据 0 条。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;

    let mats = format!("'{MAT_A_CODE}', '{MAT_B_CODE}', '{MAT_OFF_CODE}'");
    for (label, sql) in [
        (
            "在途行",
            format!(
                "SELECT count(*) AS n FROM biz_inventory_in_transit WHERE material_id IN \
                  (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
            ),
        ),
        (
            "采购订单",
            format!("SELECT count(*) AS n FROM biz_purchase_order WHERE order_no LIKE '{DOC_PREFIX}%'"),
        ),
        (
            "发货单",
            format!("SELECT count(*) AS n FROM biz_shipment WHERE ship_no LIKE '{DOC_PREFIX}%'"),
        ),
        (
            "库存行",
            format!(
                "SELECT count(*) AS n FROM biz_inventory WHERE material_id IN \
                  (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
            ),
        ),
        (
            "库位账行",
            format!(
                "SELECT count(*) AS n FROM biz_inventory_location WHERE material_id IN \
                  (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
            ),
        ),
        (
            "水位行",
            format!(
                "SELECT count(*) AS n FROM biz_material_stock_policy WHERE material_id IN \
                  (SELECT id FROM biz_material WHERE material_code IN ({mats}))"
            ),
        ),
    ] {
        let rows = db().find_with_params(&sql, vec![]).await.expect("清场核对");
        let n: i64 = rows.first().expect("count 行").get_typed("n").expect("count");
        assert_eq!(n, 0, "清场后 {label} 应为 0 条");
    }
    println!("[M2-15] 清场：在途 / 采购订单 / 发货单 / 库存行 / 水位行 全部 0 条 ✓");
}

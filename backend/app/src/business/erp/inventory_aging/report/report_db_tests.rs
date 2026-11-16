// ============================================================================
// report_db_tests.rs —— 库龄分段报表（M2-09）的真库用例
//
// 卡：t_4e2cb6f6（M2-09【M2·仓库】库龄分析：报表（0-30/31-60/61-90/90+））
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `report_buckets_follow_fifo_layer_ages` | 卡面「0-30/31-60/61-90/90+」：四档按**层入库日期**分，出库先吃老层（FIFO 由 M2-08 的库函数负责） |
// | `report_groups_by_material_and_warehouse` | 卡面「按物料/仓库汇总」：物料 / 仓库任一不同就是不同的行，不跨格合并 |
// | `bucket_boundaries_at_90_and_91_days` | 边界口径：第 90 天还在 61-90，第 91 天进「90 天以上」 |
// | `as_of_snapshot_cuts_by_business_date` | 基准日按**业务日期**切（补录单据：业务日期在基准日之后的不许出现） |
// | `total_matches_stock_qty` | 账实勾稽：Σ档位数量 = Σ行数量 = `biz_inventory.qty` |
// | `filters_narrow_to_one_partition` | 物料 / 仓库筛选（推进入库函数）只留这一格 |
// | `names_are_filled_and_summary_equals_rows` | 主数据名字批量补齐（无 N+1）；合计恒等式 Σ档 = Σ行 |
// | `empty_result_is_not_an_error` | 没有结存 → 0 行 + 0 合计 + 提示，不报错 |
// | `zz_teardown_purges_fixtures` | 清场：`M209-` 残留 0 |
//
// # 为什么夹具直接插流水（不走过账）
//
// 与 M2-08 同因：本卡要造的是「各种入库日期 × 各种出库量」的组合，走真实过账造不出
// 「业务日期 06-01、过账时间 11-12」这种补录单据（过账一律 `Utc::now()`），而那正是
// 库龄档位的判据。直插的风险由 V0137 的四条 CHECK 在库层兜着；
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// # 复用 M2-08 的夹具原语
//
// 入库单 / 明细 / 出库单 / 流水四条 INSERT 与「连库 + 串行锁」直接复用
// `inventory_aging::aging_db_tests`（已 `pub(crate)`）—— 两批用例共用同一个 `db()` 连接池，
// 必须用**同一个** `DB_LOCK` 串行，各锁各的等于没锁。本文件只造自己的 `M209-` 夹具。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m209_report_test" cargo test --lib \
//   business::erp::inventory_aging::report -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{
    DB_LOCK, ensure_db, exec, insert_stock_in, insert_stock_in_item, insert_stock_out, insert_txn,
};
use crate::business::erp::inventory_aging::report::dto::{InventoryAgingReportQuery, InventoryAgingReportRes};
use crate::business::erp::inventory_aging::report::handler;
use crate::test_seed;

const WAREHOUSE_A_CODE: &str = "M209-WHA";
const WAREHOUSE_B_CODE: &str = "M209-WHB";
const MAT_A_CODE: &str = "M209-M1";
const MAT_B_CODE: &str = "M209-M2";

/// 夹具单号前缀（清场按它反查，绝不按「生产号段」删）。
const DOC_PREFIX: &str = "M209-";

/// 基准日（与 M2-08 用例同一口径，便于两张卡的数字对着看）。
const AS_OF: &str = "2026-11-16";

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "库龄报表仓甲").await
}

async fn warehouse_b() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_B_CODE, "库龄报表仓乙").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "库龄报表物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "库龄报表物料乙").await
}

/// 清场：只删本模块造的流水 / 单据 / 库存行（主数据由 `test_seed` 复用，不删）。
async fn cleanup() {
    exec(
        &format!("DELETE FROM biz_inventory_txn WHERE doc_no LIKE '{DOC_PREFIX}%'"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_stock_in WHERE in_no LIKE '{DOC_PREFIX}%'"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_stock_out WHERE out_no LIKE '{DOC_PREFIX}%'"),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_inventory WHERE material_id IN \
              (SELECT id FROM biz_material WHERE material_code IN ('{MAT_A_CODE}', '{MAT_B_CODE}'))"
        ),
        vec![],
    )
    .await;
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
    cleanup().await;
    Some(guard)
}

// ==================== 夹具 ====================

/// 造一层入库（单头 + 明细 + 流水），业务日期 = 过账日期（这里不考补录，
/// 补录口径由 `as_of_snapshot_cuts_by_business_date` 专测）。
async fn seed_layer(material: Uuid, warehouse: Uuid, in_no: &str, in_date: &str, qty: &str, unit_cost: &str) {
    let doc_id = insert_stock_in(in_no, warehouse, in_date, qty).await;
    insert_stock_in_item(doc_id, material, qty, &format!("LOT-{in_no}")).await;
    insert_txn(
        material,
        warehouse,
        "IN",
        "PURCHASE_IN",
        "STOCK_IN",
        doc_id,
        in_no,
        qty,
        unit_cost,
        in_date,
    )
    .await;
}

/// 造一笔出库（单头 + 流水）。
async fn seed_out(material: Uuid, warehouse: Uuid, out_no: &str, out_date: &str, qty: &str, unit_cost: &str) {
    let doc_id = insert_stock_out(out_no, warehouse, out_date, qty).await;
    insert_txn(
        material,
        warehouse,
        "OUT",
        "SALE_OUT",
        "STOCK_OUT",
        doc_id,
        out_no,
        qty,
        unit_cost,
        out_date,
    )
    .await;
}

/// 场景 A（物料甲 × 仓甲）：三层入库 + 一笔 FIFO 出库。
///
/// ```text
/// 层① 100  2026-06-01（基准日库龄 168 天）┐
/// 层② 200  2026-09-20（57 天）           ├ 出库 120（2026-11-10）→ 层① 吃满、层② 吃 20
/// 层③  50  2026-11-05（11 天）           ┘
///   ⇒ 剩：层② 180（31-60）、层③ 50（0-30）；合计 230，层数 2，最老 2026-09-20，最大库龄 57
///   ⇒ 加权平均库龄 = (180×57 + 50×11) / 230 = 47.00
/// ```
async fn seed_scenario_a() -> (Uuid, Uuid) {
    let material = mat_a().await;
    let warehouse = warehouse_a().await;
    seed_layer(material, warehouse, "M209-IN-A1", "2026-06-01", "100", "10").await;
    seed_layer(material, warehouse, "M209-IN-A2", "2026-09-20", "200", "10").await;
    seed_layer(material, warehouse, "M209-IN-A3", "2026-11-05", "50", "10").await;
    seed_out(material, warehouse, "M209-OUT-A", "2026-11-10", "120", "10").await;
    (material, warehouse)
}

/// 场景 B（物料乙 × 仓乙）：三层入库、无出库 —— 四档里 90+ / 61-90 / 0-30 各占一层。
///
/// ```text
/// 层① 70  2026-07-01（138 天 → 90 天以上）
/// 层② 30  2026-09-01（76 天  → 61-90 天）
/// 层③ 20  2026-10-20（27 天  → 0-30 天）
///   ⇒ 合计 120，层数 3，最老 2026-07-01，最大库龄 138
///   ⇒ 加权平均 = (70×138 + 30×76 + 20×27) / 120 = 104.00
/// ```
async fn seed_scenario_b() -> (Uuid, Uuid) {
    let material = mat_b().await;
    let warehouse = warehouse_b().await;
    seed_layer(material, warehouse, "M209-IN-B1", "2026-07-01", "70", "2").await;
    seed_layer(material, warehouse, "M209-IN-B2", "2026-09-01", "30", "2").await;
    seed_layer(material, warehouse, "M209-IN-B3", "2026-10-20", "20", "2").await;
    (material, warehouse)
}

/// 走 handler 出报表（真接口路径：解析 → 取数 → 透视 → 合计 → 截断 → 补名）。
async fn report(as_of: &str, material: Option<Uuid>, warehouse: Option<Uuid>) -> InventoryAgingReportRes {
    handler::report_of(InventoryAgingReportQuery {
        as_of: Some(as_of.to_string()),
        material_id: material.map(|id| id.to_string()),
        warehouse_id: warehouse.map(|id| id.to_string()),
    })
    .await
    .unwrap_or_else(|error| panic!("库龄报表失败：{} {}", error.code, error.msg))
}

fn bucket_qty(report: &InventoryAgingReportRes, row_index: usize, key: &str) -> Decimal {
    report.rows[row_index]
        .buckets
        .iter()
        .find(|cell| cell.key == key)
        .unwrap_or_else(|| panic!("行 {row_index} 没有档位 {key}"))
        .qty
}

// ==================== 用例 ====================

/// 四档按**层入库日期**分；出库先吃老层（FIFO 由 M2-08 的库函数负责，报表只分组）。
#[tokio::test]
async fn report_buckets_follow_fifo_layer_ages() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_scenario_a().await;

    let report = report(AS_OF, Some(material), Some(warehouse)).await;
    assert_eq!(report.row_count, 1, "一个 物料 × 仓库 一行");
    let row = &report.rows[0];
    assert_eq!(row.qty_total, dec("230"), "100+200+50 − 120 = 230");
    assert_eq!(row.layer_count, 2, "吃满的层①不算层数");
    assert_eq!(bucket_qty(&report, 0, "D0_30"), dec("50"), "层③ 11 天");
    assert_eq!(bucket_qty(&report, 0, "D31_60"), dec("180"), "层② 57 天（被吃掉 20）");
    assert_eq!(
        bucket_qty(&report, 0, "D61_90"),
        Decimal::ZERO,
        "这一格没有 61-90 天的货"
    );
    assert_eq!(bucket_qty(&report, 0, "OVER_90"), Decimal::ZERO, "层① 已被 FIFO 吃满");
    assert_eq!(row.oldest_inbound_date, date("2026-09-20"), "最老的是还剩货的层②");
    assert_eq!(row.max_age_days, 57);
    assert_eq!(row.avg_age_days, dec("47"), "(180×57 + 50×11) / 230 = 47.00");
    // 合计 = Σ各行
    let summary_qty: Decimal = report.summary.iter().map(|cell| cell.qty).sum();
    assert_eq!(summary_qty, dec("230"));
    assert_eq!(report.qty_total, dec("230"));
    println!("[M2-09] 四档：0-30=50、31-60=180、其他 0；合计 230 / 2 层 / 加权平均 47 天 ✓");
}

/// 按 物料 × 仓库 汇总：两格不合并；行序「最老货在前」。
///
/// ⚠ 演练库是 `cloudpivot_test` 的克隆（**带演示数据**），所以这里只断言**本卡夹具那两格**
/// （按 物料 × 仓库 认领自己的行），不假设库里只有这两行 —— 否则用例会随库里的存量数据漂。
/// 「不跨格合并」的判据就落在这两格各自的数量上：若被误合并成一行，两格的数量会加在一起。
#[tokio::test]
async fn report_groups_by_material_and_warehouse() {
    let Some(_guard) = setup().await else { return };
    let (mat_a_id, wh_a_id) = seed_scenario_a().await;
    let (mat_b_id, wh_b_id) = seed_scenario_b().await;

    let report = report(AS_OF, None, None).await;
    let mine: Vec<&crate::business::erp::inventory_aging::report::dto::MaterialWarehouseAgingRes> = report
        .rows
        .iter()
        .filter(|row| {
            (row.material_id == mat_a_id.to_string() && row.warehouse_id == wh_a_id.to_string())
                || (row.material_id == mat_b_id.to_string() && row.warehouse_id == wh_b_id.to_string())
        })
        .collect();
    assert_eq!(mine.len(), 2, "两个 物料 × 仓库 组合 → 两行（不合并成一行）");
    assert_ne!(mine[0].material_id, mine[1].material_id);
    assert_ne!(mine[0].warehouse_id, mine[1].warehouse_id);

    // 各自的数量 / 层数 / 最老日期（合并了就会变成 350 / 5 层）
    let row_a = mine
        .iter()
        .find(|row| row.material_id == mat_a_id.to_string())
        .expect("物料甲的行");
    assert_eq!(row_a.qty_total, dec("230"), "场景 A 合计（没被别格的数据污染）");
    assert_eq!(row_a.layer_count, 2);
    assert_eq!(row_a.oldest_inbound_date, date("2026-09-20"));
    let row_b = mine
        .iter()
        .find(|row| row.material_id == mat_b_id.to_string())
        .expect("物料乙的行");
    assert_eq!(row_b.qty_total, dec("120"), "场景 B 合计");
    assert_eq!(row_b.layer_count, 3);
    assert_eq!(row_b.oldest_inbound_date, date("2026-07-01"));

    // 场景 B 三层的库龄落在三个不同档：0-30 / 61-90 / 90 天以上
    let buckets: std::collections::HashMap<&str, Decimal> =
        row_b.buckets.iter().map(|cell| (cell.key.as_str(), cell.qty)).collect();
    assert_eq!(buckets["D0_30"], dec("20"));
    assert_eq!(buckets["D61_90"], dec("30"));
    assert_eq!(buckets["OVER_90"], dec("70"));
    assert_eq!(buckets["D31_60"], Decimal::ZERO);
    // 场景 A：0-30=50、31-60=180
    let buckets_a: std::collections::HashMap<&str, Decimal> =
        row_a.buckets.iter().map(|cell| (cell.key.as_str(), cell.qty)).collect();
    assert_eq!(buckets_a["D0_30"], dec("50"));
    assert_eq!(buckets_a["D31_60"], dec("180"));

    // 行序「最老货在前」：场景 B（2026-07-01）必须排在场景 A（2026-09-20）之前
    let index_b = report
        .rows
        .iter()
        .position(|row| row.material_id == mat_b_id.to_string() && row.warehouse_id == wh_b_id.to_string())
        .expect("场景 B 行");
    let index_a = report
        .rows
        .iter()
        .position(|row| row.material_id == mat_a_id.to_string() && row.warehouse_id == wh_a_id.to_string())
        .expect("场景 A 行");
    assert!(index_b < index_a, "最老货在前：场景 B 应排在场景 A 之前");

    // 合计恒等式（对全库都成立，与存量数据无关）
    let summary_qty: Decimal = report.summary.iter().map(|cell| cell.qty).sum();
    let row_qty: Decimal = report.rows.iter().map(|row| row.qty_total).sum();
    assert_eq!(summary_qty, row_qty, "Σ档位 必须等于 Σ行");
    assert_eq!(report.qty_total, summary_qty);
    assert_eq!(
        report.layer_count,
        report.summary.iter().map(|cell| cell.layer_count).sum::<i64>()
    );
    println!("[M2-09] 汇总：两格各自独立（230 / 120，不跨物料/仓库合并）、最老在前、Σ档=Σ行 ✓");
}

/// 边界：第 90 天仍算 61-90，第 91 天进「90 天以上」。
#[tokio::test]
async fn bucket_boundaries_at_90_and_91_days() {
    let Some(_guard) = setup().await else { return };
    let material = mat_b().await;
    let warehouse = warehouse_a().await;
    // 2026-11-16 − 90 天 = 2026-08-18（61-90 的最后一刻）
    seed_layer(material, warehouse, "M209-IN-D90", "2026-08-18", "10", "1").await;
    // 2026-11-16 − 91 天 = 2026-08-17（进 90 天以上）
    seed_layer(material, warehouse, "M209-IN-D91", "2026-08-17", "20", "1").await;
    // 2026-11-16 当天入库（库龄 0 → 0-30 的第一刻）
    seed_layer(material, warehouse, "M209-IN-D0", AS_OF, "5", "1").await;

    let report = report(AS_OF, Some(material), Some(warehouse)).await;
    assert_eq!(bucket_qty(&report, 0, "D61_90"), dec("10"), "90 天整 → 61-90");
    assert_eq!(bucket_qty(&report, 0, "OVER_90"), dec("20"), "91 天 → 90 天以上");
    assert_eq!(bucket_qty(&report, 0, "D0_30"), dec("5"), "当天入库 → 0-30");
    assert_eq!(report.rows[0].max_age_days, 91);
    println!("[M2-09] 边界：90 天 → 61-90、91 天 → 90 天以上、0 天 → 0-30 ✓");
}

/// 基准日按**业务日期**切：业务日期晚于基准日的入库 / 出库都不许出现。
#[tokio::test]
async fn as_of_snapshot_cuts_by_business_date() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_scenario_a().await;

    // 2026-10-01：层③（11-05）与出库（11-10）都还没发生 → 三层全满；
    // 层① 06-01 的库龄 122 天 → 90 天以上；层② 09-20 的库龄 11 天 → 0-30。
    let report = report("2026-10-01", Some(material), Some(warehouse)).await;
    assert_eq!(report.rows[0].qty_total, dec("300"), "出库还没发生，300 全在");
    assert_eq!(report.rows[0].layer_count, 2, "层③ 业务日期 11-05 晚于基准日");
    assert_eq!(bucket_qty(&report, 0, "OVER_90"), dec("100"), "层① 122 天");
    assert_eq!(bucket_qty(&report, 0, "D0_30"), dec("200"), "层② 11 天");
    assert_eq!(report.rows[0].oldest_inbound_date, date("2026-06-01"));
    println!("[M2-09] 基准日快照：2026-10-01 → 300（层③与出库都不在）✓");
}

/// 账实勾稽：Σ档位数量 = Σ行数量 = `biz_inventory.qty`。
#[tokio::test]
async fn total_matches_stock_qty() {
    let Some(_guard) = setup().await else { return };
    let (mat_a_id, wh_a_id) = seed_scenario_a().await;
    let (mat_b_id, wh_b_id) = seed_scenario_b().await;
    // 合规路径下库存结存由过账写；这里直插流水，所以库存行也直插（与 M2-08 用例同款）
    exec(
        "INSERT INTO biz_inventory (material_id, warehouse_id, qty, locked_qty, avg_cost, amount) \
         VALUES (?, ?, 230, 0, 10, 2300), (?, ?, 120, 0, 2, 240)",
        vec![
            Value::from(&mat_a_id),
            Value::from(&wh_a_id),
            Value::from(&mat_b_id),
            Value::from(&wh_b_id),
        ],
    )
    .await;
    let stock_qty: String = db()
        .scalar_with_params(
            "SELECT SUM(qty)::text FROM biz_inventory \
              WHERE material_id IN (?, ?)",
            vec![Value::from(&mat_a_id), Value::from(&mat_b_id)],
        )
        .await
        .expect("读库存合计");

    // 只取本卡的两格：演练库带演示数据，整体加总得不到 350
    // （两个仓库都是 `test_seed` 新建的，里面只可能躺着本卡的流水）
    let report_a = report(AS_OF, None, Some(wh_a_id)).await;
    let report_b = report(AS_OF, None, Some(wh_b_id)).await;
    let row_qty = report_a.qty_total + report_b.qty_total;
    let bucket_qty: Decimal = report_a
        .summary
        .iter()
        .chain(report_b.summary.iter())
        .map(|cell| cell.qty)
        .sum();
    assert_eq!(row_qty, dec(&stock_qty), "Σ行数量 必须等于 biz_inventory.qty");
    assert_eq!(bucket_qty, row_qty, "Σ档位 必须等于 Σ行");
    assert_eq!(row_qty, dec("350"), "场景 A 230 + 场景 B 120");
    assert_eq!(dec(&stock_qty), dec("350"));
    println!("[M2-09] 勾稽：Σ档位 350 == Σ行 350 == biz_inventory.qty 350 ✓");
}

/// 物料 / 仓库筛选（推进入库函数）只留这一格。
#[tokio::test]
async fn filters_narrow_to_one_partition() {
    let Some(_guard) = setup().await else { return };
    let (mat_a_id, wh_a_id) = seed_scenario_a().await;
    seed_scenario_b().await;

    let only_material = report(AS_OF, Some(mat_a_id), None).await;
    assert_eq!(only_material.row_count, 1);
    assert_eq!(only_material.qty_total, dec("230"));
    assert_eq!(only_material.material_count, 1);

    let only_warehouse = report(AS_OF, None, Some(wh_a_id)).await;
    assert_eq!(only_warehouse.row_count, 1);
    assert_eq!(only_warehouse.qty_total, dec("230"));

    let both = report(AS_OF, Some(mat_a_id), Some(wh_a_id)).await;
    assert_eq!(both.row_count, 1);
    assert_eq!(both.qty_total, dec("230"));

    // 不存在的组合 → 0 行（不是「忽略筛选返回全量」）
    let none = report(
        AS_OF,
        Some(mat_a_id),
        Some(test_seed::ensure_warehouse("M209-WHZ", "库龄报表仓丙").await),
    )
    .await;
    assert_eq!(none.row_count, 0);
    assert_eq!(none.qty_total, Decimal::ZERO);
    println!("[M2-09] 筛选：物料 / 仓库 / 两者 → 各 1 行；不存在的组合 → 0 行 ✓");
}

/// 主数据名字批量补齐（无 N+1）；合计恒等式 Σ档 = Σ行。
///
/// 只看本卡的两个仓（演练库带演示数据；软删的主数据会让「所有行都有名字」这条断言假红）。
#[tokio::test]
async fn names_are_filled_and_summary_equals_rows() {
    let Some(_guard) = setup().await else { return };
    let (_, wh_a_id) = seed_scenario_a().await;
    let (_, wh_b_id) = seed_scenario_b().await;

    for warehouse in [wh_a_id, wh_b_id] {
        let report = report(AS_OF, None, Some(warehouse)).await;
        assert!(!report.rows.is_empty(), "本卡的仓里必须有行");
        for row in &report.rows {
            assert!(row.material_code.is_some(), "物料编码要补上：{row:?}");
            assert!(row.material_name.is_some(), "物料名称要补上：{row:?}");
            assert!(row.warehouse_code.is_some(), "仓库编码要补上：{row:?}");
            assert!(row.warehouse_name.is_some(), "仓库名称要补上：{row:?}");
            // 行合计 = Σ本行各档（同一个数算一遍）
            let sum: Decimal = row.buckets.iter().map(|cell| cell.qty).sum();
            assert_eq!(sum, row.qty_total, "行 {} 的 Σ档 ≠ 行合计", row.material_id);
            let layers: i64 = row.buckets.iter().map(|cell| cell.layer_count).sum();
            assert_eq!(layers, row.layer_count, "行 {} 的 Σ档层数 ≠ 行层数", row.material_id);
            assert_eq!(row.buckets.len(), 4, "四档固定（缺档给 0，不给 NULL）");
        }
        // 合计恒等式
        let summary_qty: Decimal = report.summary.iter().map(|cell| cell.qty).sum();
        let row_qty: Decimal = report.rows.iter().map(|row| row.qty_total).sum();
        assert_eq!(summary_qty, row_qty);
        assert_eq!(summary_qty, report.qty_total);
        assert_eq!(report.row_count, i64::try_from(report.rows.len()).unwrap());
        assert!(!report.rows_truncated, "两行远未到上限");
        assert_eq!(report.row_limit, 2000);
        assert_eq!(report.buckets.len(), 4, "表头四档");
        assert_eq!(report.as_of, date(AS_OF));
        assert_eq!(report.as_of_source, "QUERY", "传了 as_of → 来源 QUERY");
        assert_eq!(report.report_code, "INVENTORY_AGING");
    }
    println!("[M2-09] 名字补齐 + 合计恒等式（Σ档 = Σ行 = 总量）✓");
}

/// 没有结存 → 0 行 + 0 合计 + 提示，**不报错**（空数据不是出错）。
#[tokio::test]
async fn empty_result_is_not_an_error() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_scenario_a().await;

    // 基准日早于所有入库业务日期（只看本卡这一格：演练库的演示数据不该影响「空结果」用例）
    let report = report("2025-01-01", Some(material), Some(warehouse)).await;
    assert_eq!(report.row_count, 0);
    assert_eq!(report.qty_total, Decimal::ZERO);
    assert_eq!(report.amount_total, Decimal::ZERO);
    assert_eq!(report.layer_count, 0);
    assert!(report.rows.is_empty());
    assert!(
        report.hints.iter().any(|hint| hint.contains("没有还剩结存的库龄层")),
        "空结果要给口径提示：{:?}",
        report.hints
    );
    assert!(!report.rows_truncated, "空结果不是截断（两者都表现为「少行」，别混）");
    println!("[M2-09] 空结果：0 行 / 0 合计 / 有提示，不报错 ✓");
}

/// 基准日留空 → 今天（CST）且来源回显 `TODAY`（用户不传也得知道这是哪天的表）。
#[tokio::test]
async fn default_as_of_is_today_cst_and_echoed() {
    let Some(_guard) = setup().await else { return };
    let report = handler::report_of(InventoryAgingReportQuery::default())
        .await
        .expect("默认基准日不该报错");
    let cst_today = (Utc::now() + Duration::hours(8)).date_naive();
    assert_eq!(report.as_of, cst_today, "留空 = CST 今天（不是 UTC 今天）");
    assert_eq!(report.as_of_source, "TODAY");
    // 只断言形状，不断言内容：演练库带演示数据，内容随库漂
    assert_eq!(report.buckets.len(), 4);
    assert!(!report.report_name.is_empty());
    println!("[M2-09] 基准日留空 → {}（as_of_source=TODAY）✓", report.as_of);
}

/// 清场用例（放最后跑：其余用例共用 `M209-` 夹具，谁先谁后都靠 `setup` 重置）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT (SELECT count(*) FROM biz_inventory_txn WHERE doc_no LIKE 'M209-%') \
                  + (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'M209-%') \
                  + (SELECT count(*) FROM biz_stock_out WHERE out_no LIKE 'M209-%')",
            vec![],
        )
        .await
        .expect("清场核对");
    assert_eq!(left, 0, "清场后 M209- 残留必须为 0");
    println!("[M2-09] 清场：M209- 残留 0 ✓");
}

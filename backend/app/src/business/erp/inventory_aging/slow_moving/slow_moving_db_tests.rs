// ============================================================================
// slow_moving_db_tests.rs —— 呆滞料标记（M2-10）的真库用例
//
// 卡：t_bb0fe3c6（M2-10【M2·仓库】库龄：呆滞料标记（超 N 天无出库））
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `slow_rows_mark_layers_older_than_threshold` | 卡面「超 N 天无出库」：**批次库龄 > N 且仍有剩余**的层才算呆滞；不呆滞的格不进行（但进 summary） |
// | `threshold_boundary_exactly_n_days_is_not_slow` | 边界：90 天整**不算**、91 天才算（严格大于，与报表「90 天以上 = 91 起」同一把尺） |
// | `default_threshold_matches_report_over_90` | 口径互证：N=90 时每格 `qty_slow` == 库龄报表「90 天以上」档数量（同一库函数、同一基准日） |
// | `threshold_from_config_and_query_override` | 卡面「可配置阈值」：参数 > `sys_config` > 缺省；坏配置回缺省**并告警** |
// | `filters_narrow_to_one_partition` | 物料 / 仓库筛选（推进入库函数）只留这一格；名字批量补齐（无 N+1） |
// | `empty_result_is_not_an_error` | 没有结存 → 0 行 / 0 汇总 / 有提示，不报错 |
// | `default_as_of_is_today_cst_and_echoed` | 基准日留空 → 今天 CST 且来源回显 `TODAY` |
// | `zz_teardown_purges_fixtures` | 清场：`M210-` 残留 0、阈值参数回到 V0426 种子行（用例不删它） |
//
// # 为什么夹具直接插流水（不走过账）
//
// 与 M2-08 / M2-09 同因：本卡要造的是「各种入库日期 × 各种出库量」的组合，走真实过账
// 造不出「业务日期 06-01、过账时间 11-12」这种补录单据（过账一律 `Utc::now()`），
// 而那正是库龄与呆滞判据。直插的风险由 V0137 的 CHECK 在库层兜着；
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// # 复用 M2-08 / M2-09 的夹具原语
//
// 入库单 / 明细 / 出库单 / 流水四条 INSERT 与「连库 + 串行锁」直接复用
// `inventory_aging::aging_db_tests`（已 `pub(crate)`）—— 三批用例共用同一个 `db()`
// 连接池，必须用**同一个** `DB_LOCK` 串行，各锁各的等于没锁。本文件只造自己的 `M210-` 夹具。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m210_slow_test" cargo test --lib \
//   business::erp::inventory_aging::slow_moving -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::{Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{
    DB_LOCK, ensure_db, exec, insert_stock_in, insert_stock_in_item, insert_stock_out, insert_txn,
};
use crate::business::erp::inventory_aging::report::dto::InventoryAgingReportQuery;
use crate::business::erp::inventory_aging::report::handler as report_handler;
use crate::business::erp::inventory_aging::slow_moving::dto::{SlowMovingQuery, SlowMovingRes, THRESHOLD_CONFIG_KEY};
use crate::business::erp::inventory_aging::slow_moving::handler;
use crate::support::config::model::clear_runtime_snapshot_for_tests;
use crate::test_seed;

const WAREHOUSE_A_CODE: &str = "M210-WHA";
const WAREHOUSE_B_CODE: &str = "M210-WHB";
const MAT_A_CODE: &str = "M210-M1";
const MAT_B_CODE: &str = "M210-M2";

/// 夹具单号前缀（清场按它反查，绝不按「生产号段」删）。
const DOC_PREFIX: &str = "M210-";

/// 基准日（与 M2-08 / M2-09 用例同一口径，便于三张卡的数字对着看）。
const AS_OF: &str = "2026-11-16";

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "呆滞料仓甲").await
}

async fn warehouse_b() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_B_CODE, "呆滞料仓乙").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "呆滞料物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "呆滞料物料乙").await
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
    // 阈值参数：**不动迁移种下的那一行**（那是 V0426 的产物，删了演练库就缺参）。
    // 只把「上一轮中途 panic 留下的临时行」按种子值归一（90 + 种子描述），见文件尾
    // `SEED_DESC` / `TEMP_DESC` 的说明。
    exec(
        &format!(
            "UPDATE sys_config SET config_value = '90', config_type = 'NUMBER', description = '{SEED_DESC}' \
              WHERE config_key = '{THRESHOLD_CONFIG_KEY}' AND description = '{TEMP_DESC}'"
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

/// 造一层入库（单头 + 明细 + 流水），业务日期 = 过账日期。
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

/// 场景 A（物料甲 × 仓甲）：三层入库 + 一笔 FIFO 出库 —— **没有呆滞层**（N=90）。
///
/// ```text
/// 层① 100  2026-06-01（168 天）┐
/// 层② 200  2026-09-20（57 天） ├ 出库 120（11-10）→ 层① 吃满、层② 吃 20
/// 层③  50  2026-11-05（11 天） ┘
///   ⇒ 剩：层② 180（57 天）、层③ 50（11 天）；总 230，假 90 天阈值下呆滞 0
///   ⇒ 阈值降到 50 天时：层② 180 变呆滞（180/230 = 78.26%）
/// ```
async fn seed_scenario_a() -> (Uuid, Uuid) {
    let material = mat_a().await;
    let warehouse = warehouse_a().await;
    seed_layer(material, warehouse, "M210-IN-A1", "2026-06-01", "100", "10").await;
    seed_layer(material, warehouse, "M210-IN-A2", "2026-09-20", "200", "10").await;
    seed_layer(material, warehouse, "M210-IN-A3", "2026-11-05", "50", "10").await;
    seed_out(material, warehouse, "M210-OUT-A", "2026-11-10", "120", "10").await;
    (material, warehouse)
}

/// 场景 B（物料乙 × 仓乙）：三层入库、无出库 —— 90 天阈值下只有最老那层呆滞。
///
/// ```text
/// 层① 70  2026-07-01（138 天）→ 呆滞
/// 层② 30  2026-09-01（76 天） → 不呆滞
/// 层③ 20  2026-10-20（27 天） → 不呆滞
///   ⇒ 总 120，呆滞 70（58.33%），呆滞参考金额 70 × 2 = 140，最大库龄 138
/// ```
async fn seed_scenario_b() -> (Uuid, Uuid) {
    let material = mat_b().await;
    let warehouse = warehouse_b().await;
    seed_layer(material, warehouse, "M210-IN-B1", "2026-07-01", "70", "2").await;
    seed_layer(material, warehouse, "M210-IN-B2", "2026-09-01", "30", "2").await;
    seed_layer(material, warehouse, "M210-IN-B3", "2026-10-20", "20", "2").await;
    (material, warehouse)
}

/// 走 handler 出呆滞料清单（真接口路径：解析 → 解阈值 → 取数 → 汇总 → 挑行 → 补名）。
async fn slow_moving(as_of: &str, material: Option<Uuid>, warehouse: Option<Uuid>, days: Option<i64>) -> SlowMovingRes {
    handler::slow_moving_of(SlowMovingQuery {
        as_of: Some(as_of.to_string()),
        material_id: material.map(|id| id.to_string()),
        warehouse_id: warehouse.map(|id| id.to_string()),
        days: days.map(|value| value.to_string()),
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "呆滞料标记失败：{} {}（内部原因：{:?}）",
            error.code, error.msg, error.internal_detail
        )
    })
}

/// 在本表的行里认领「这一格」（演练库带演示数据，不能假设只有自己那几行）。
fn find_row(
    report: &SlowMovingRes,
    material: Uuid,
    warehouse: Uuid,
) -> Option<&crate::business::erp::inventory_aging::slow_moving::dto::SlowMovingRowRes> {
    report
        .rows
        .iter()
        .find(|row| row.material_id == material.to_string() && row.warehouse_id == warehouse.to_string())
}

// ==================== 用例 ====================

/// 卡面口径：**批次库龄 > N 且仍有剩余**才算呆滞；不呆滞的格不进行（但进汇总）。
#[tokio::test]
async fn slow_rows_mark_layers_older_than_threshold() {
    let Some(_guard) = setup().await else { return };
    let (mat_a_id, wh_a_id) = seed_scenario_a().await;
    let (mat_b_id, wh_b_id) = seed_scenario_b().await;

    // ① 缺省档（N=90）：只有场景 B 的最老层呆滞
    let report = slow_moving(AS_OF, None, None, Some(90)).await;
    assert_eq!(report.threshold_days, 90);
    assert_eq!(report.threshold_source, "QUERY");
    let row_b = find_row(&report, mat_b_id, wh_b_id).expect("场景 B 应有呆滞行");
    assert_eq!(row_b.qty_on_hand, dec("120"), "总结存 70+30+20");
    assert_eq!(row_b.qty_slow, dec("70"), "只有 138 天那层呆滞");
    assert_eq!(row_b.slow_layer_count, 1);
    assert_eq!(row_b.layer_count, 3, "层数是全部有结存的层");
    assert_eq!(row_b.amount_slow, dec("140"), "70 × 2 = 140（参考金额）");
    assert_eq!(row_b.slow_ratio, dec("0.5833"), "70 / 120 = 0.5833");
    assert_eq!(row_b.max_age_days, 138);
    assert_eq!(row_b.oldest_inbound_date, date("2026-07-01"));
    assert_eq!(row_b.material_code.as_deref(), Some(MAT_B_CODE), "物料名字要补上");
    assert_eq!(
        row_b.warehouse_code.as_deref(),
        Some(WAREHOUSE_B_CODE),
        "仓库名字要补上"
    );

    // 场景 A 三层都不满 90 天（最老那一层已被 FIFO 吃满）→ 不进行
    assert!(
        find_row(&report, mat_a_id, wh_a_id).is_none(),
        "没有超过 90 天的层就不该进行：{:?}",
        report.rows
    );

    // ② 阈值降到 50 天：场景 A 的 57 天那层（剩 180）进呆滞
    let report = slow_moving(AS_OF, Some(mat_a_id), Some(wh_a_id), Some(50)).await;
    assert_eq!(report.row_count, 1);
    let row_a = &report.rows[0];
    assert_eq!(row_a.qty_on_hand, dec("230"));
    assert_eq!(row_a.qty_slow, dec("180"), "层② 剩 180（57 天）");
    assert_eq!(row_a.slow_ratio, dec("0.7826"), "180 / 230 = 0.7826");
    assert_eq!(report.summary.qty_slow, dec("180"), "汇总 = Σ行（只这一格）");

    // ③ 不变量：呆滞数量永远不超过结存数量（逐格）
    for row in &report.rows {
        assert!(row.qty_slow <= row.qty_on_hand, "呆滞数 > 结存数：{row:?}");
        assert!(row.slow_layer_count <= row.layer_count, "呆滞层数 > 层数：{row:?}");
        assert!(row.qty_slow > Decimal::ZERO, "本表只返回有呆滞的行：{row:?}");
    }
    println!("[M2-10] 判定：N=90 → 只有 138 天那层呆滞（70/120）；N=50 → 57 天那层也进（180/230）✓");
}

/// 边界：`> N`（严格大于）—— 90 天整**不算**呆滞，91 天才算（与报表档位同一把尺）。
#[tokio::test]
async fn threshold_boundary_exactly_n_days_is_not_slow() {
    let Some(_guard) = setup().await else { return };
    let material = mat_a().await;
    let warehouse = warehouse_a().await;
    // 2026-11-16 − 90 天 = 2026-08-18（正好 90 天：不算呆滞）
    seed_layer(material, warehouse, "M210-IN-D90", "2026-08-18", "10", "1").await;
    // 2026-11-16 − 91 天 = 2026-08-17（91 天：算呆滞）
    seed_layer(material, warehouse, "M210-IN-D91", "2026-08-17", "20", "1").await;
    // 当天入库（0 天：不算）
    seed_layer(material, warehouse, "M210-IN-D0", AS_OF, "5", "1").await;

    let report = slow_moving(AS_OF, Some(material), Some(warehouse), Some(90)).await;
    assert_eq!(report.row_count, 1);
    let row = &report.rows[0];
    assert_eq!(row.qty_slow, dec("20"), "90 天整不算、91 天算");
    assert_eq!(row.qty_on_hand, dec("35"), "10 + 20 + 5");
    assert_eq!(row.slow_layer_count, 1);
    assert_eq!(row.max_age_days, 91);

    // 阈值降到 89：90 天那层也进（严格大于 89）
    let report = slow_moving(AS_OF, Some(material), Some(warehouse), Some(89)).await;
    assert_eq!(report.rows[0].qty_slow, dec("30"), "90 天 > 89 天 ⇒ 也算呆滞");

    // 阈值提到 91：一层都不呆滞（91 天 > 91 不成立）
    let report = slow_moving(AS_OF, Some(material), Some(warehouse), Some(91)).await;
    assert_eq!(report.row_count, 0, "91 > 91 不成立 ⇒ 没有呆滞行");
    assert_eq!(report.summary.qty_slow, Decimal::ZERO);
    assert_eq!(report.summary.scanned_row_count, 1, "格还是被扫过的（分母在）");
    println!("[M2-10] 边界：90 天不算 / 91 天算；N=91 时同一格 0 呆滞但被扫过 ✓");
}

/// 口径互证：N=90 时每格 `qty_slow` == 库龄报表「90 天以上」档数量。
#[tokio::test]
async fn default_threshold_matches_report_over_90() {
    let Some(_guard) = setup().await else { return };
    let (mat_a_id, wh_a_id) = seed_scenario_a().await;
    let (mat_b_id, wh_b_id) = seed_scenario_b().await;

    let slow = slow_moving(AS_OF, None, None, Some(90)).await;
    let aging = report_handler::report_of(InventoryAgingReportQuery {
        as_of: Some(AS_OF.to_string()),
        material_id: None,
        warehouse_id: None,
    })
    .await
    .expect("库龄报表失败");

    for (material, warehouse) in [(mat_a_id, wh_a_id), (mat_b_id, wh_b_id)] {
        let over_90 = aging
            .rows
            .iter()
            .find(|row| row.material_id == material.to_string() && row.warehouse_id == warehouse.to_string())
            .and_then(|row| row.buckets.iter().find(|cell| cell.key == "OVER_90"))
            .map(|cell| cell.qty)
            .unwrap_or(Decimal::ZERO);
        let qty_slow = find_row(&slow, material, warehouse)
            .map(|row| row.qty_slow)
            .unwrap_or(Decimal::ZERO);
        assert_eq!(
            qty_slow, over_90,
            "物料 {material} / 仓库 {warehouse}：呆滞数量 {qty_slow} ≠ 报表 90 天以上 {over_90}"
        );
    }
    println!("[M2-10] 互证：N=90 的 qty_slow 逐格等于报表「90 天以上」档 ✓");
}

/// 卡面「可配置阈值」：参数 > `sys_config` > 缺省；坏配置回缺省**并告警**。
///
/// ⚠ 本用例会临时改 `sys_config` 的阈值行，结束时按原值 / 原状态还原
/// （`restore_threshold_row`：UPDATE 不 DELETE，迁移种下的那一行不许被用例删掉）。
///
/// ⚠ **每一步都先钉「写入生效」再钉「取值生效」**：报表读参数走
/// `SysConfigModel::read_text`，它优先命中**进程内快照**（同一进程里跑过
/// `fx_settings` / `opening` 之类写过参数的用例就会装载快照），命中即不再回源。
/// 于是「库里有 30、报表读回 90」会伪装成「配置值不生效」。三个写入口
/// （`write_threshold_row` / `disable_threshold_row` / `restore_threshold_row`）
/// 都在写后调 `clear_runtime_snapshot_for_tests()` 显式作废快照；本用例再用直查
/// `sys_config` 把「写入生效」这一步单独断言出来，失败时能一眼分辨是**写没落库**
/// 还是**读了旧快照**（卡 t_9c3551b0）。
#[tokio::test]
async fn threshold_from_config_and_query_override() {
    let Some(_guard) = setup().await else { return };
    let (_, wh_b_id) = seed_scenario_b().await;
    let material = mat_b().await;

    let original = threshold_row().await;

    // ① 系统参数 = 30 → 没传 days 时生效，来源 CONFIG
    write_threshold_row("30", TEMP_DESC, 1).await;
    assert_eq!(
        threshold_row().await,
        Some(("30".to_string(), TEMP_DESC.to_string(), 1)),
        "写入生效：`sys_config` 里必须已经是 30（直查，不经过参数读取路径）"
    );
    let report = slow_moving(AS_OF, Some(material), Some(wh_b_id), None).await;
    assert_eq!(report.threshold_days, 30, "配置值生效");
    assert_eq!(report.threshold_source, "CONFIG");
    assert_eq!(report.threshold_config_key, THRESHOLD_CONFIG_KEY);
    assert_eq!(report.rows[0].qty_slow, dec("100"), "138 天 + 76 天的两层都 > 30");
    assert_eq!(report.rows[0].slow_layer_count, 2);

    // ② 请求参数覆盖配置（哪怕配置合法）
    let report = slow_moving(AS_OF, Some(material), Some(wh_b_id), Some(200)).await;
    assert_eq!(report.threshold_days, 200);
    assert_eq!(report.threshold_source, "QUERY");
    assert_eq!(report.row_count, 0, "200 天：没有一层够老");

    // ③ 参数行停用（= 没配置；`read_text` 对停用行回 None）→ 回缺省 90
    disable_threshold_row().await;
    assert_eq!(
        threshold_row().await.map(|(_, _, status)| status),
        Some(0),
        "写入生效：阈值行必须已停用（直查）"
    );
    let report = slow_moving(AS_OF, Some(material), Some(wh_b_id), None).await;
    assert_eq!(report.threshold_days, 90);
    assert_eq!(report.threshold_source, "DEFAULT");
    assert_eq!(report.rows[0].qty_slow, dec("70"));

    // ④ 配了坏值 → 回缺省 + hints 点名参数键（不静默）
    write_threshold_row("abc", TEMP_DESC, 1).await;
    let report = slow_moving(AS_OF, Some(material), Some(wh_b_id), None).await;
    assert_eq!(report.threshold_days, 90, "坏值必须回缺省");
    assert_eq!(report.threshold_source, "DEFAULT");
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains(THRESHOLD_CONFIG_KEY) && hint.contains("不可用")),
        "坏配置必须在 hints 里点名：{:?}",
        report.hints
    );

    restore_threshold_row(original).await;
    println!("[M2-10] 阈值三级：配置 30 生效 / 参数 200 覆盖 / 停用回缺省 90 / 坏值 abc 回缺省并告警 ✓");
}

/// 物料 / 仓库筛选（推进入库函数）只留这一格；汇总也按同一筛选收窄。
#[tokio::test]
async fn filters_narrow_to_one_partition() {
    let Some(_guard) = setup().await else { return };
    let (mat_a_id, _) = seed_scenario_a().await;
    let (mat_b_id, wh_b_id) = seed_scenario_b().await;

    let only_material = slow_moving(AS_OF, Some(mat_b_id), None, Some(90)).await;
    assert_eq!(only_material.row_count, 1);
    assert_eq!(only_material.summary.scanned_row_count, 1, "只扫这一格");

    let only_warehouse = slow_moving(AS_OF, None, Some(wh_b_id), Some(90)).await;
    assert_eq!(only_warehouse.row_count, 1);

    let both = slow_moving(AS_OF, Some(mat_b_id), Some(wh_b_id), Some(90)).await;
    assert_eq!(both.row_count, 1);
    assert_eq!(both.summary.slow_row_count, 1);
    assert_eq!(both.summary.qty_on_hand, dec("120"));
    assert_eq!(both.summary.qty_slow, dec("70"));
    assert_eq!(both.summary.amount_slow, dec("140"));
    assert_eq!(both.summary.slow_ratio, dec("0.5833"));
    assert_eq!(both.summary.layer_count, 3);
    assert_eq!(both.summary.slow_layer_count, 1);

    // 不存在的组合 → 0 行（不是「忽略筛选返回全量」）
    let none = slow_moving(
        AS_OF,
        Some(mat_a_id),
        Some(test_seed::ensure_warehouse("M210-WHZ", "呆滞料仓丙").await),
        Some(90),
    )
    .await;
    assert_eq!(none.row_count, 0);
    assert_eq!(none.summary.scanned_row_count, 0);
    assert_eq!(none.summary.qty_on_hand, Decimal::ZERO);
    println!("[M2-10] 筛选：物料 / 仓库 / 两者各 1 行；不存在的组合 → 0 行 0 扫过 ✓");
}

/// 没有结存 → 0 行 / 0 汇总 / 有提示，**不报错**（空数据不是出错）。
#[tokio::test]
async fn empty_result_is_not_an_error() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_scenario_a().await;

    // 基准日早于所有入库业务日期
    let report = slow_moving("2025-01-01", Some(material), Some(warehouse), Some(90)).await;
    assert_eq!(report.row_count, 0);
    assert_eq!(report.summary.scanned_row_count, 0);
    assert_eq!(report.summary.qty_slow, Decimal::ZERO);
    assert_eq!(report.summary.amount_on_hand, Decimal::ZERO);
    assert!(report.rows.is_empty());
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("没有任何还剩结存的库龄层")),
        "空结果要给口径提示：{:?}",
        report.hints
    );
    assert!(!report.rows_truncated, "空结果不是截断（两者都表现为「少行」，别混）");
    println!("[M2-10] 空结果：0 行 / 0 汇总 / 有提示，不报错 ✓");
}

/// 基准日留空 → 今天（CST）且来源回显 `TODAY`。
#[tokio::test]
async fn default_as_of_is_today_cst_and_echoed() {
    let Some(_guard) = setup().await else { return };
    let report = handler::slow_moving_of(SlowMovingQuery::default())
        .await
        .expect("默认基准日不该报错");
    let cst_today = (Utc::now() + Duration::hours(8)).date_naive();
    assert_eq!(report.as_of, cst_today, "留空 = CST 今天（不是 UTC 今天）");
    assert_eq!(report.as_of_source, "TODAY");
    assert!(
        ["QUERY", "CONFIG", "DEFAULT"].contains(&report.threshold_source.as_str()),
        "阈值来源只能是三者之一：{}",
        report.threshold_source
    );
    assert_eq!(report.report_code, "INVENTORY_SLOW_MOVING");
    assert_eq!(report.report_name, "呆滞料清单");
    assert_eq!(report.row_limit, 2000);
    println!("[M2-10] 基准日留空 → {}（as_of_source=TODAY）✓", report.as_of);
}

/// 清场用例（放最后跑：其余用例共用 `M210-` 夹具，谁先谁后都靠 `setup` 重置）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT (SELECT count(*) FROM biz_inventory_txn WHERE doc_no LIKE 'M210-%') \
                  + (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'M210-%') \
                  + (SELECT count(*) FROM biz_stock_out WHERE out_no LIKE 'M210-%')",
            vec![],
        )
        .await
        .expect("清场核对");
    assert_eq!(left, 0, "清场后 M210- 残留必须为 0");

    // 阈值参数必须回到「迁移种子那一行」：行在、启用、描述不是测试临时标记
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT config_value::text AS config_value, COALESCE(description, '')::text AS description, status::int AS status \
                   FROM sys_config WHERE config_key = '{THRESHOLD_CONFIG_KEY}' AND deleted_at IS NULL"
            ),
            vec![],
        )
        .await
        .expect("清场核对参数行");
    assert_eq!(rows.len(), 1, "阈值参数行必须还在（V0426 的产物，用例不许删掉）");
    let description: String = rows[0].get_typed("description").expect("描述");
    let status: i32 = rows[0].get_typed("status").expect("状态");
    assert_ne!(description, TEMP_DESC, "临时标记必须已被归一成种子描述");
    assert_eq!(status, 1, "阈值参数必须保持启用");
    println!("[M2-10] 清场：M210- 残留 0、阈值参数回到 V0426 种子行 ✓");
}

// ==================== 阈值参数行的读 / 写 / 还原 ====================
//
// ⚠ 这一行是 **V0426 种下的**（不是测试造的），所以用例**不做 DELETE**：
//   · 改值用 UPDATE（行不存在才 INSERT）；
//   · 「模拟没配置」用 `status = 0`（停用）—— `SysConfigModel::read_text` 对停用行回 `None`，
//     与「没这行」等价（见 `support::config::model::read_text_from_db`）；
//   · 结束按原值 / 原状态还原；万一中途 panic，`cleanup` 会把带着 [`TEMP_DESC`] 标记的
//     残行按种子值归一（90 + 种子描述），不把迁移的产物删掉。

/// V0426 种下的描述（识别「这一行是种子」用；与迁移逐字一致）。
const SEED_DESC: &str = "呆滞料判定阈值（天）：库龄严格大于该值且仍有剩余的入库批次视为呆滞\
（缺省 90，与库龄报表「90 天以上」档对齐；范围 1-3650）";

/// 测试临时写入时的描述（`cleanup` 按它认领「上一轮 panic 留下的残行」）。
const TEMP_DESC: &str = "M2-10 测试临时阈值（跑完还原）";

/// 库里当前的阈值行（值 + 描述 + 状态）。
async fn threshold_row() -> Option<(String, String, i32)> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT config_value::text AS config_value, COALESCE(description, '')::text AS description, status::int AS status \
                   FROM sys_config WHERE config_key = '{THRESHOLD_CONFIG_KEY}' AND deleted_at IS NULL LIMIT 1"
            ),
            vec![],
        )
        .await
        .ok()?;
    let row = rows.first()?;
    Some((
        row.get_typed("config_value").ok()?,
        row.get_typed("description").ok()?,
        row.get_typed("status").ok()?,
    ))
}

/// 写阈值（行在 → UPDATE，不在 → INSERT；**不删行**）。
async fn write_threshold_row(value: &str, description: &str, status: i32) {
    let affected = db()
        .execute_with_params(
            &format!(
                "UPDATE sys_config SET config_value = ?::text, description = ?::text, status = ?::text::int \
                  WHERE config_key = '{THRESHOLD_CONFIG_KEY}' AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(value.to_string()),
                Value::Text(description.to_string()),
                Value::Text(status.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("更新阈值参数失败：{error}"));
    if affected > 0 {
        // 直改库必须显式作废进程内参数快照：否则 `read_text` 会命中快照里的旧值
        // （实测 t_9c3551b0：跑过 `fx_settings` 之后本用例恒读回种子值 90）。
        clear_runtime_snapshot_for_tests();
        return;
    }
    exec(
        &format!(
            "INSERT INTO sys_config (config_key, config_value, config_type, description, category, \
                                     sort_order, config_level, is_modifiable, is_encrypted, is_restart, status) \
             VALUES ('{THRESHOLD_CONFIG_KEY}', ?::text, 'NUMBER', ?::text, 'erp_inventory', 10, 2, TRUE, FALSE, FALSE, ?::text::int)"
        ),
        vec![
            Value::Text(value.to_string()),
            Value::Text(description.to_string()),
            Value::Text(status.to_string()),
        ],
    )
    .await;
    // 直改库后必须显式作废快照（理由见 `write_threshold_row`）。
    clear_runtime_snapshot_for_tests();
}

/// 模拟「没配置」：把参数行**停用**（`read_text` 对停用行回 `None`，与没这行等价）。
async fn disable_threshold_row() {
    exec(
        &format!(
            "UPDATE sys_config SET status = 0, description = '{TEMP_DESC}' \
              WHERE config_key = '{THRESHOLD_CONFIG_KEY}'"
        ),
        vec![],
    )
    .await;
    clear_runtime_snapshot_for_tests();
}

/// 还原阈值行（`None` = 本来就没有 → 删掉；正常情形是还原成 V0426 种子行）。
async fn restore_threshold_row(original: Option<(String, String, i32)>) {
    match original {
        Some((value, description, status)) => write_threshold_row(&value, &description, status).await,
        None => {
            exec(
                &format!("DELETE FROM sys_config WHERE config_key = '{THRESHOLD_CONFIG_KEY}'"),
                vec![],
            )
            .await;
            clear_runtime_snapshot_for_tests();
        }
    }
}

// ============================================================================
// aging_db_tests.rs —— 库龄计算（M2-08）的真库用例
//
// 卡：t_397dc77c（M2-08【M2·仓库】库龄：计算（按批次入库时间））
//
// # 覆盖什么（每条都对应卡面 / 上游契约的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `fifo_layers_split_oldest_first` | 卡面「FIFO 批次库龄」：老层先吃满，剩余 = 较新的层；库龄 = 基准日 − 层日期 |
// | `include_consumed_shows_fully_eaten_layer` | 默认只看有结存的层；`include_consumed=true` 才把吃满的层也翻出来 |
// | `as_of_cuts_by_business_date_not_posting_time` | 期末快照按**业务日期**切（补录单据：过账晚于业务日期照样算进那一期） |
// | `lot_no_filter_narrows_to_one_layer` | 批号筛选（来自入库单明细 `lot_no`）只命中那一层 |
// | `age_range_maps_to_inbound_date_bounds` | `min/max_age_days` 折成入库日期区间（与 `age_days` 逐天等价） |
// | `sql_matches_reference_allocator` | SQL 的 FIFO 与 Rust 参考实现 `fifo::allocate_fifo` **逐层相同**（两份实现互证） |
// | `remaining_total_matches_stock_qty` | `Σ层剩余 = biz_inventory.qty`（正常数据下的账实勾稽） |
// | `over_consumed_partition_yields_no_layers` | 出库 > 入库（脏数据）时不给负数层，剩余全 0 |
// | `pagination_is_stable_and_counts_all` | `total` 是筛选后总量、行数受 `page_size` 限制、最老在前 |
// | `warehouse_filter_isolates_partitions` | 库龄层按 `物料 × 仓库` 分格，不跨仓合并 |
//
// # 为什么夹具直接插流水（不走过账）
//
// 本卡是**只读**计算：要造的是「各种入库日期 × 各种出库量 × 补录时间差」的组合。
// 走真实过账造不出「业务日期 10-20、过账时间 11-12」的补录单据（过账一律 `Utc::now()`），
// 而这正是本卡最关键的日期口径。直插的风险是绕过 CHECK —— V0137 的四条 CHECK 在库层兜着。
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m208_aging_test" cargo test --lib \
//   business::erp::inventory_aging::aging_db_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::dto::AgingFilter;
use crate::business::erp::inventory_aging::fifo::{allocate_fifo, sum_of};
use crate::business::erp::inventory_aging::handler;
use crate::test_seed;

/// 真库用例必须串行（共用全局 `db()` 与同一批 `M208-` 夹具）。
///
/// `pub(crate)`：M2-09 的库龄分段报表用例（`report::report_db_tests`）用**同一个**锁串行
/// —— 两批用例共用一个 `db()` 连接池，各锁各的等于没锁。
pub(crate) static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const WAREHOUSE_A_CODE: &str = "M208-WHA";
const WAREHOUSE_B_CODE: &str = "M208-WHB";
const MAT_A_CODE: &str = "M208-M1";
const MAT_B_CODE: &str = "M208-M2";

/// 夹具单号前缀（清场按它反查，绝不按「生产号段」删）。
const DOC_PREFIX: &str = "M208-";

// ---- 三个入库层的业务日期 / 过账时刻（见模块头注释：IN-C 与 OUT-X 都是「补录」）----
const IN_A_DATE: &str = "2026-09-01";
const IN_B_DATE: &str = "2026-10-15";
const IN_C_DATE: &str = "2026-11-10";
const OUT_X_DATE: &str = "2026-10-20";
/// 入库 C 的过账时刻**晚于**它的业务日期（补录）。
const IN_C_POSTED: &str = "2026-11-14T02:00:00Z";
/// 出库 X 的过账时刻**晚于**它的业务日期（补录）。
const OUT_X_POSTED: &str = "2026-11-12T02:00:00Z";

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn warehouse_a() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_A_CODE, "库龄仓甲").await
}

async fn warehouse_b() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_B_CODE, "库龄仓乙").await
}

async fn mat_a() -> Uuid {
    test_seed::ensure_material(MAT_A_CODE, "库龄物料甲").await
}

async fn mat_b() -> Uuid {
    test_seed::ensure_material(MAT_B_CODE, "库龄物料乙").await
}

pub(crate) async fn ensure_db() -> bool {
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

pub(crate) async fn exec(statement: &str, params: Vec<Value>) {
    db().execute_with_params(statement, params)
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
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

/// 一张入库单头（`in_date` = 业务日期；`in_type='OTHER'` 免得撞「采购必须带供应商」的 CHECK，
/// `is_sourceless = true` 是 `ck_biz_si_sourceless_pair` 对「无源单」的硬要求）。
pub(crate) async fn insert_stock_in(in_no: &str, warehouse_id: Uuid, in_date: &str, qty: &str) -> Uuid {
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status, total_qty, is_sourceless) \
         VALUES (?, ?, 'OTHER', ?, ?::text::date, 'POSTED', ?::text::numeric, TRUE)",
        vec![
            Value::from(id),
            Value::from(in_no),
            Value::from(&warehouse_id),
            Value::Text(in_date.to_string()),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    id
}

/// 入库单明细（批号挂在这一行上；`(stock_in_id, line_no)` 唯一）。
pub(crate) async fn insert_stock_in_item(stock_in_id: Uuid, material_id: Uuid, qty: &str, lot_no: &str) {
    let unit_id = test_seed::ensure_unit().await;
    exec(
        "INSERT INTO biz_stock_in_item \
            (stock_in_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
             amount, tax_amount, with_tax_amount, cost_price, lot_no) \
         VALUES (?, 1, ?, ?, ?::text::numeric, 0, 0, 0, 0, 0, 0, 0, ?)",
        vec![
            Value::from(&stock_in_id),
            Value::from(&material_id),
            Value::from(&unit_id),
            Value::Text(qty.to_string()),
            Value::from(lot_no),
        ],
    )
    .await;
}

/// 一张出库单头（`out_date` = 业务日期）。
pub(crate) async fn insert_stock_out(out_no: &str, warehouse_id: Uuid, out_date: &str, qty: &str) -> Uuid {
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, total_qty) \
         VALUES (?, ?, 'OTHER', ?, ?::text::date, 'POSTED', ?::text::numeric)",
        vec![
            Value::from(id),
            Value::from(out_no),
            Value::from(&warehouse_id),
            Value::Text(out_date.to_string()),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    id
}

/// 一笔库存流水（**直接插**，见模块头注释）。
///
/// `posted_at` 单独传：本卡最关键的口径就是「过账时间 ≠ 业务日期」（补录单据）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_txn(
    material_id: Uuid,
    warehouse_id: Uuid,
    txn_type: &str,
    biz_type: &str,
    doc_type: &str,
    doc_id: Uuid,
    doc_no: &str,
    qty: &str,
    unit_cost: &str,
    posted_at: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    let posted = if posted_at.contains('T') {
        posted_at.to_string()
    } else {
        format!("{posted_at}T02:00:00Z")
    };
    exec(
        "INSERT INTO biz_inventory_txn \
            (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, \
             qty, unit_cost, amount, before_qty, before_amount, before_cost, \
             after_qty, after_amount, after_cost, period_code, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?::text::numeric, ?::text::numeric, 0, \
                 0, 0, 0, 0, 0, 0, '2026-11', ?::text::timestamptz)",
        vec![
            Value::from(id),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::from(txn_type),
            Value::from(biz_type),
            Value::from(doc_type),
            Value::from(&doc_id),
            Value::from(doc_no),
            Value::Text(qty.to_string()),
            Value::Text(unit_cost.to_string()),
            Value::Text(posted),
        ],
    )
    .await;
    id
}

/// 造标准三层 + 一笔补录出库（物料甲 / 仓甲）：100(09-01) / 200(10-15) / 50(11-10) − 180(10-20)。
async fn seed_three_layers() -> (Uuid, Uuid) {
    let material = mat_a().await;
    let warehouse = warehouse_a().await;

    for (in_no, in_date, qty, lot, posted) in [
        ("M208-IN-A", IN_A_DATE, "100", "LOT-A", "2026-09-02"),
        ("M208-IN-B", IN_B_DATE, "200", "LOT-B", "2026-10-16"),
        ("M208-IN-C", IN_C_DATE, "50", "LOT-C", IN_C_POSTED),
    ] {
        let doc_id = insert_stock_in(in_no, warehouse, in_date, qty).await;
        insert_stock_in_item(doc_id, material, qty, lot).await;
        insert_txn(
            material,
            warehouse,
            "IN",
            "PURCHASE_IN",
            "STOCK_IN",
            doc_id,
            in_no,
            qty,
            "10",
            posted,
        )
        .await;
    }

    let out_id = insert_stock_out("M208-OUT-X", warehouse, OUT_X_DATE, "180").await;
    insert_txn(
        material,
        warehouse,
        "OUT",
        "SALE_OUT",
        "STOCK_OUT",
        out_id,
        "M208-OUT-X",
        "180",
        "10",
        OUT_X_POSTED,
    )
    .await;

    (material, warehouse)
}

/// 走 handler 取一页（真接口路径：解析 → 校验 → 取数 → 补名）。
async fn page(filter: AgingFilter) -> Vec<crate::business::erp::inventory_aging::dto::AgingLayerRes> {
    handler::layer_page_of(filter)
        .await
        .unwrap_or_else(|error| panic!("库龄分页失败：{} {}", error.code, error.msg))
        .list
}

fn filter_of(as_of: &str, material: Uuid, warehouse: Uuid) -> AgingFilter {
    AgingFilter {
        material_id: Some(material.to_string()),
        warehouse_id: Some(warehouse.to_string()),
        as_of: Some(as_of.to_string()),
        page: 1,
        page_size: 50,
        ..AgingFilter::default()
    }
}

// ==================== 用例 ====================

/// FIFO：老层先被吃满，剩余落在较新的层上；库龄 = 基准日 − 层日期。
#[tokio::test]
async fn fifo_layers_split_oldest_first() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    let rows = page(filter_of("2026-11-16", material, warehouse)).await;
    assert_eq!(rows.len(), 2, "默认只看有结存的层（A 层已被吃满）");
    assert_eq!(rows[0].doc_no, "M208-IN-B", "最老在前：B 层（10-15）");
    assert_eq!(rows[0].qty_in, dec("200"));
    assert_eq!(rows[0].qty_consumed, dec("80"));
    assert_eq!(rows[0].qty_remaining, dec("120"));
    assert_eq!(rows[0].age_days, 32, "11-16 − 10-15 = 32 天");
    assert_eq!(rows[0].lot_no.as_deref(), Some("LOT-B"));
    assert_eq!(rows[1].doc_no, "M208-IN-C");
    assert_eq!(rows[1].qty_remaining, dec("50"));
    assert_eq!(rows[1].age_days, 6);
    assert_eq!(rows[0].material_name.as_deref(), Some("库龄物料甲"), "名字要批量补齐");
    assert_eq!(rows[0].warehouse_name.as_deref(), Some("库龄仓甲"));
    assert_eq!(rows[0].as_of, date("2026-11-16"));
    println!("[M2-08] FIFO 分层：100/200/50 出 180 → 剩 120(B,32天) + 50(C,6天) ✓");
}

/// 吃满的层默认不返回；显式 `include_consumed=true` 才翻出来（且剩余为 0）。
#[tokio::test]
async fn include_consumed_shows_fully_eaten_layer() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    let rows = page(AgingFilter {
        include_consumed: Some("true".to_string()),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].doc_no, "M208-IN-A");
    assert_eq!(rows[0].qty_consumed, dec("100"));
    assert_eq!(rows[0].qty_remaining, Decimal::ZERO);
    assert_eq!(rows[0].age_days, 76, "11-16 − 09-01 = 76 天");
    println!("[M2-08] include_consumed=true：三行全出，A 层剩 0 / 已消耗 100 ✓");
}

/// 期末快照按**业务日期**切：补录单据（过账晚于业务日期）照样算进它所属的那一期。
#[tokio::test]
async fn as_of_cuts_by_business_date_not_posting_time() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    // ① 10-31：出库 X（业务日期 10-20，过账 11-12）必须**算进来** → B 层被吃 80；
    //    C 层（业务日期 11-10）不许出现，尽管它的过账时刻 11-14 也在基准日之后无关。
    let rows = page(AgingFilter {
        include_consumed: Some("true".to_string()),
        ..filter_of("2026-10-31", material, warehouse)
    })
    .await;
    let docs: Vec<&str> = rows.iter().map(|row| row.doc_no.as_str()).collect();
    assert_eq!(docs, ["M208-IN-A", "M208-IN-B"], "C 层业务日期晚于基准日，不许出现");
    assert_eq!(rows[0].qty_remaining, Decimal::ZERO, "A 被吃满");
    assert_eq!(
        rows[1].qty_remaining,
        dec("120"),
        "出库按业务日期算进来了（过账时间更晚也得算）"
    );
    assert_eq!(rows[1].age_days, 16);

    // ② 10-18：出库 X（业务日期 10-20）还没发生 → 两层都满
    let rows = page(filter_of("2026-10-18", material, warehouse)).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].qty_remaining, dec("100"));
    assert_eq!(rows[1].qty_remaining, dec("200"));

    // ③ 11-12：C 层（业务日期 11-10，过账 11-14）必须出现 —— 过账时间不是快照判据
    let rows = page(filter_of("2026-11-12", material, warehouse)).await;
    let docs: Vec<&str> = rows.iter().map(|row| row.doc_no.as_str()).collect();
    assert_eq!(docs, ["M208-IN-B", "M208-IN-C"]);
    assert_eq!(rows[1].age_days, 2);
    println!("[M2-08] 期末快照按业务日期切（补录单据算进所属期间，过账时间不参与）✓");
}

/// 批号筛选只命中那一层（批号来自入库单明细 `lot_no`）。
#[tokio::test]
async fn lot_no_filter_narrows_to_one_layer() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    let rows = page(AgingFilter {
        lot_no: Some(" LOT-B ".to_string()),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    assert_eq!(rows.len(), 1, "批号筛选只留一层");
    assert_eq!(rows[0].doc_no, "M208-IN-B");
    assert_eq!(rows[0].qty_remaining, dec("120"));

    let rows = page(AgingFilter {
        lot_no: Some("LOT-NOT-EXIST".to_string()),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    assert!(rows.is_empty(), "不存在的批号 → 空页（不是全量）");
    println!("[M2-08] 批号筛选（含空白归一化）只命中 LOT-B ✓");
}

/// 库龄区间折成入库日期区间：`min_age_days` / `max_age_days` 含边界。
#[tokio::test]
async fn age_range_maps_to_inbound_date_bounds() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    let rows = page(AgingFilter {
        min_age_days: Some(10),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    assert_eq!(rows.len(), 1, "只要库龄 ≥ 10 天的：B(32)");
    assert_eq!(rows[0].doc_no, "M208-IN-B");

    let rows = page(AgingFilter {
        max_age_days: Some(10),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    assert_eq!(rows.len(), 1, "只要库龄 ≤ 10 天的：C(6)");
    assert_eq!(rows[0].doc_no, "M208-IN-C");

    let rows = page(AgingFilter {
        min_age_days: Some(6),
        max_age_days: Some(32),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    assert_eq!(rows.len(), 2, "区间含两个边界（6 与 32 都算）");
    println!("[M2-08] 库龄区间 → 入库日期区间，边界含当天 ✓");
}

/// SQL 的 FIFO 与 Rust 参考实现逐层相同（两份实现互证）。
#[tokio::test]
async fn sql_matches_reference_allocator() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    let rows = page(AgingFilter {
        include_consumed: Some("true".to_string()),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    let sql_remaining: Vec<Decimal> = rows.iter().map(|row| row.qty_remaining).collect();
    let inflows: Vec<Decimal> = rows.iter().map(|row| row.qty_in).collect();
    assert_eq!(
        sql_remaining,
        allocate_fifo(&inflows, dec("180")),
        "SQL 与参考实现的层剩余不一致"
    );
    assert_eq!(sum_of(&sql_remaining), dec("170"));

    // 出库量 0（基准日早于出库业务日期）同样对拍
    let rows = page(filter_of("2026-10-18", material, warehouse)).await;
    let sql_remaining: Vec<Decimal> = rows.iter().map(|row| row.qty_remaining).collect();
    let inflows: Vec<Decimal> = rows.iter().map(|row| row.qty_in).collect();
    assert_eq!(sql_remaining, allocate_fifo(&inflows, Decimal::ZERO));
    println!("[M2-08] SQL FIFO == Rust 参考实现（出库 180 / 0 两组）✓");
}

/// 正常数据下 `Σ层剩余 = biz_inventory.qty`（账实勾稽）。
#[tokio::test]
async fn remaining_total_matches_stock_qty() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    exec(
        "INSERT INTO biz_inventory (material_id, warehouse_id, qty, locked_qty, avg_cost, amount) \
         VALUES (?, ?, 170, 0, 10, 1700)",
        vec![Value::from(&material), Value::from(&warehouse)],
    )
    .await;

    let rows = page(AgingFilter {
        include_consumed: Some("true".to_string()),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    let remaining: Vec<Decimal> = rows.iter().map(|row| row.qty_remaining).collect();
    let stock_qty: String = db()
        .scalar_with_params(
            "SELECT qty::text FROM biz_inventory WHERE material_id = ? AND warehouse_id = ?",
            vec![Value::from(&material), Value::from(&warehouse)],
        )
        .await
        .expect("读库存行");
    assert_eq!(
        sum_of(&remaining),
        dec(&stock_qty),
        "Σ层剩余 必须等于 biz_inventory.qty"
    );
    assert_eq!(sum_of(&remaining), dec("170"));
    println!("[M2-08] Σ层剩余(170) == biz_inventory.qty(170) ✓");
}

/// 出库 > 入库（脏数据 / 允许负库存的仓）：不给负数层，剩余全 0。
#[tokio::test]
async fn over_consumed_partition_yields_no_layers() {
    let Some(_guard) = setup().await else { return };
    let material = mat_b().await;
    let warehouse = warehouse_b().await;

    let in_id = insert_stock_in("M208-IN-OVER", warehouse, "2026-10-01", "10").await;
    insert_stock_in_item(in_id, material, "10", "LOT-OVER").await;
    insert_txn(
        material,
        warehouse,
        "IN",
        "PURCHASE_IN",
        "STOCK_IN",
        in_id,
        "M208-IN-OVER",
        "10",
        "10",
        "2026-10-01",
    )
    .await;
    let out_id = insert_stock_out("M208-OUT-OVER", warehouse, "2026-10-05", "20").await;
    insert_txn(
        material,
        warehouse,
        "OUT",
        "SALE_OUT",
        "STOCK_OUT",
        out_id,
        "M208-OUT-OVER",
        "20",
        "10",
        "2026-10-05",
    )
    .await;

    let rows = page(filter_of("2026-11-16", material, warehouse)).await;
    assert!(rows.is_empty(), "默认只看有结存的层：吃空的层不返回");

    let rows = page(AgingFilter {
        include_consumed: Some("true".to_string()),
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].qty_remaining, Decimal::ZERO, "不许编造负数层");
    assert_eq!(rows[0].qty_consumed, dec("10"), "消耗量不超过本层原量");
    println!("[M2-08] 出库 20 > 入库 10 → 剩余 0（不编负数层）✓");
}

/// 分页：`total` 是筛选后总量，行数受 `page_size` 限制，最老在前。
#[tokio::test]
async fn pagination_is_stable_and_counts_all() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;

    let first = handler::layer_page_of(AgingFilter {
        page: 1,
        page_size: 1,
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await
    .expect("第 1 页");
    assert_eq!(first.total, 2, "总量是筛选后的真实总数，不是本页行数");
    assert_eq!(first.list.len(), 1);
    assert_eq!(first.page, 1);
    assert_eq!(first.page_size, 1);
    assert_eq!(first.list[0].doc_no, "M208-IN-B", "最老在前");

    let second = handler::layer_page_of(AgingFilter {
        page: 2,
        page_size: 1,
        ..filter_of("2026-11-16", material, warehouse)
    })
    .await
    .expect("第 2 页");
    assert_eq!(second.total, 2);
    assert_eq!(second.list.len(), 1);
    assert_eq!(second.list[0].doc_no, "M208-IN-C", "第 2 页接上，不重不漏");
    println!("[M2-08] 分页：total=2、page_size=1 逐页取齐（老→新）✓");
}

/// 库龄层按 `物料 × 仓库` 分格：另一个仓的层不混进来，也不影响这一格的 FIFO。
#[tokio::test]
async fn warehouse_filter_isolates_partitions() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_three_layers().await;
    let other_warehouse = warehouse_b().await;

    let doc_id = insert_stock_in("M208-IN-WB", other_warehouse, "2026-10-20", "70").await;
    insert_stock_in_item(doc_id, material, "70", "LOT-WB").await;
    insert_txn(
        material,
        other_warehouse,
        "IN",
        "PURCHASE_IN",
        "STOCK_IN",
        doc_id,
        "M208-IN-WB",
        "70",
        "10",
        "2026-10-20",
    )
    .await;

    let rows = page(filter_of("2026-11-16", material, warehouse)).await;
    assert_eq!(rows.len(), 2, "仓甲只有两层");
    assert!(rows.iter().all(|row| row.warehouse_id == warehouse.to_string()));

    let rows = page(filter_of("2026-11-16", material, other_warehouse)).await;
    assert_eq!(rows.len(), 1, "仓乙只有一层，且不受仓甲出库影响");
    assert_eq!(rows[0].qty_remaining, dec("70"));
    assert_eq!(rows[0].lot_no.as_deref(), Some("LOT-WB"));
    println!("[M2-08] 层按 物料 × 仓库 分格（仓乙 70 不被仓甲的出库吃掉）✓");
}

/// 清场用例（放最后跑：其余用例共用 `M208-` 夹具，谁先谁后都靠 `setup` 重置）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let left: i64 = db()
        .scalar_with_params(
            "SELECT (SELECT count(*) FROM biz_inventory_txn WHERE doc_no LIKE 'M208-%') \
                  + (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'M208-%') \
                  + (SELECT count(*) FROM biz_stock_out WHERE out_no LIKE 'M208-%')",
            vec![],
        )
        .await
        .expect("清场核对");
    assert_eq!(left, 0, "清场后 M208- 残留必须为 0");
    println!("[M2-08] 清场：M208- 残留 0 ✓");
}

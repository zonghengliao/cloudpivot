// ============================================================================
// aging_period_tests.rs —— 库龄的**跨期**与**呆滞判定**单测（M2-11）
//
// 卡：t_855bc226（M2-11【M2·仓库】库龄：单测 · 含跨期与呆滞判定）
//
// # 为什么单开一张卡（M2-08/09/10 已经有 86 条用例了）
//
// 那三张卡的用例各自只钉**自己那个视图**在**一个基准日**上的口径：
//
//   M2-08（计算） `as_of = 2026-11-16` 一天 → 分层 / 分页 / 批号 / 区间 …
//   M2-09（报表） `as_of = 2026-11-16` 一天 → 四档 / 边界 90|91 / 账实勾稽 …
//   M2-10（呆滞） `as_of = 2026-11-16` 一天 → 阈值边界 / 三级取值 / 与报表互证 …
//
// 三张卡都没回答的是「**基准日往前走，数字怎么变**」和「**同一批货在跨期之后会不会被判成呆滞**」。
// 而库龄报表最常被问的两个问题恰好是这两个：「上个月末这货多老？」和「这批货什么时候算呆滞？」。
// 跨期口径一旦漂（比如某处用了过账时间、某处用了期间内天数），单日用例全都照样绿。
//
// 所以本文件只做两件事（卡面「含跨期与呆滞判定」逐字落成用例）：
//
//   ① **跨期**：基准日沿月末推进（08-31 → 09-30 → 10-31 → 11-30 → 12-31），
//      层剩余 / 结存 / 档位 / 呆滞量逐期对拍；补录单据（业务日期在本期、过账在下期）
//      必须落在**业务日期所属的那一期**；期初层（`doc_type = OPENING`）按 `period_code` 月初；
//      业务日期 == 基准日当天**含**（期初含当天、期末含当天）。
//   ② **呆滞判定**：同一层货随着基准日推进，只会在 `库龄 > N` 那**一天**翻成呆滞，
//      且必须与报表「90 天以上」档**同一天翻转**（同一把尺，不能一个 90 算、一个 91 算）。
//
// # 用例清单
//
// | 用例 | 钉住的口径 | 类型 |
// |---|---|---|
// | `bucket_and_slow_flip_on_the_same_day` | 档位与呆滞**同一把尺**：`bucket_of(age) == "OVER_90"` ⇔ `is_slow(age, 90)`，逐日 0..=400 | 纯逻辑 |
// | `age_days_across_month_year_and_leap_boundaries` | 跨月 / 跨年 / 闰年 2 月：库龄是**日历天数**，不是「本期第几天」 | 纯逻辑 |
// | `period_end_identity_holds_for_slow_quantity` | 每一期：`Σ OVER_90 档数量 == Σ 呆滞层数量`（同一批层的两种判法必须给同一个数） | 纯逻辑 |
// | `inbound_bounds_hold_at_period_ends` | 期初 / 期末 / 闰日基准日上，`inbound_bounds` 与 `age_days` 逐天等价（负库龄也算） | 纯逻辑 |
// | `cross_period_snapshots_evolve_per_period_end` | 跨期快照：同一批流水在四个月末的层剩余 / 结存 / 库龄逐步演进（FIFO 吃老层） | 真库 |
// | `backdated_document_lands_in_its_business_period` | 补录单据按**业务日期**归期（过账时间晚于基准日也照样计入） | 真库 |
// | `slow_flag_flips_when_period_advances_past_threshold` | 呆滞判定跨期翻转：29 → 60 → 90 天都不呆滞，**91 天才呆滞**；档位同期迁移 0-30 → 31-60 → 61-90 → 90+ | 真库 |
// | `three_views_agree_at_every_period_end` | 明细 / 报表 / 呆滞三个视图在**每一个**期末基准日互相自洽（Σ层剩余 == Σ档 == 呆滞 + 不呆滞；N=90 时呆滞 == OVER_90） | 真库 |
// | `period_boundary_day_is_inclusive` | 业务日期 == 基准日**含**：当天入库当天出库在当天期末就看得见（期初 / 期末都不许差一天） | 真库 |
// | `opening_layer_dates_from_period_start` | 期初层（`doc_type = OPENING`）的层日期 = `period_code` 月初，**不取过账时间**（否则 9 月期初会被算成 11 月进的货） | 真库 |
// | `zz_teardown_purges_fixtures` | 清场：`M211-` 残留 0 | 真库 |
//
// # 为什么夹具还是直接插流水
//
// 与 M2-08/09/10 同因：本卡要造的是「业务日期 × 过账时间 × 基准日」三者错位的组合，
// 走真实过账造不出「业务日期 09-28、过账 11-25」的补录单据（过账一律 `Utc::now()`），
// 而那正是跨期归属的判据。直插的风险由 V0137 的 CHECK 在库层兜着（`txn_type` / `qty > 0`）；
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// # 复用既有夹具原语与串行锁
//
// 入库单 / 明细 / 出库单 / 流水四条 INSERT 与「连库 + 串行锁」直接复用
// `inventory_aging::aging_db_tests`（已 `pub(crate)`）—— 四批用例共用同一个 `db()`
// 连接池，必须用**同一个** `DB_LOCK` 串行，各锁各的等于没锁。本文件只造自己的 `M211-` 夹具。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m211_period_test" cargo test --lib \
//   business::erp::inventory_aging::aging_period_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::{Duration, NaiveDate};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{
    DB_LOCK, ensure_db, exec, insert_stock_in, insert_stock_in_item, insert_stock_out, insert_txn,
};
use crate::business::erp::inventory_aging::dto::{AgingFilter, AgingLayerRes, age_days_of, inbound_bounds};
use crate::business::erp::inventory_aging::handler;
use crate::business::erp::inventory_aging::report::dto::{
    InventoryAgingReportQuery, InventoryAgingReportRes, MaterialWarehouseAgingRes, bucket_of,
};
use crate::business::erp::inventory_aging::report::handler as report_handler;
use crate::business::erp::inventory_aging::slow_moving::dto::{
    DEFAULT_THRESHOLD_DAYS, SlowMovingQuery, SlowMovingRes, is_slow,
};
use crate::business::erp::inventory_aging::slow_moving::handler as slow_handler;
use crate::test_seed;

/// 夹具单号前缀（清场按它反查，绝不按「生产号段」删）。
const DOC_PREFIX: &str = "M211-";

// ---- 五个夹具格（每个用例用自己的 物料 × 仓库，互不干扰）----
/// ① 跨期生命周期（四个月末逐期演进）。
const MAT_LIFE: &str = "M211-M1";
/// ② 呆滞翻转（一层货，基准日推着走）。
const MAT_SLOW: &str = "M211-M2";
/// ③ 补录单据（业务日期本期、过账下期）。
const MAT_BACKDATED: &str = "M211-M3";
/// ④ 期末当天边界（当天进当天出）。
const MAT_BOUNDARY: &str = "M211-M4";
/// ⑤ 库存期初（`doc_type = OPENING`）。
const MAT_OPENING: &str = "M211-M5";

const WAREHOUSE_A_CODE: &str = "M211-WHA";
const WAREHOUSE_B_CODE: &str = "M211-WHB";

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 跨期推进用的五个基准日（四个月末 + 翻月后的第一天：`11-30` 满 90 天、`12-01` 满 91 天）。
const PERIOD_ENDS: [&str; 6] = [
    "2026-08-31",
    "2026-09-30",
    "2026-10-31",
    "2026-11-30",
    "2026-12-01",
    "2026-12-31",
];

// ==================== 纯逻辑用例（不连库）====================

/// 档位与呆滞判定必须是**同一把尺**：0-400 天逐日比，`OVER_90` ⇔ `库龄 > 90`。
///
/// 这条为什么重要：报表说「90 天以上 = 91 天起」、呆滞料说「超过 N 天」，
/// 用户会拿两张表对账；只要有一边把 90 天整算进「以上/呆滞」，同一天同一批货
/// 在两张表上就会打架，而且**没有任何一处报错**。M2-10 的决策是「`age > N` 严格大于」，
/// 本用例把它和报表档位表绑在同一个循环里 —— 以后谁改档位表或改比较符号，
/// 只要两边不再同步，这里立刻红。
#[test]
fn bucket_and_slow_flip_on_the_same_day() {
    // 档位迁移的三个翻转日（跨期视角：同一层货，基准日每推一天，档位只在这几天变）
    let flips = [
        (30_i64, "D0_30", "D31_60"),
        (60, "D31_60", "D61_90"),
        (90, "D61_90", "OVER_90"),
    ];
    for (boundary, before, after) in flips {
        assert_eq!(bucket_of(boundary), before, "{boundary} 天还在「{before}」档");
        assert_eq!(bucket_of(boundary + 1), after, "{} 天进「{after}」档", boundary + 1);
    }

    // 逐日：档位是否「90 天以上」必须与呆滞判定（阈值 90）**逐日同真同假**
    for age in 0..=400_i64 {
        assert_eq!(
            bucket_of(age) == "OVER_90",
            is_slow(age, DEFAULT_THRESHOLD_DAYS),
            "库龄 {age} 天：档位「{:?}」与呆滞判定（> {} 天）不一致",
            bucket_of(age),
            DEFAULT_THRESHOLD_DAYS
        );
    }
    assert!(!is_slow(90, 90), "90 天整不算呆滞（严格大于）");
    assert!(is_slow(91, 90), "91 天才算呆滞");
    println!("[M2-11] 档位与呆滞同一把尺：0..=400 天逐日同真同假，翻转日 = 30|31 / 60|61 / 90|91 ✓");
}

/// 库龄是**日历天数**，跨月 / 跨年 / 闰年 2 月都不许差一天（不是「本期第几天」）。
///
/// 跨期报表最容易出的一类偏差：把库龄当成「期间内天数」（期末 − 期初），
/// 于是同一层货在 9 月末显示 29 天、10 月末显示 30 天。库龄不是这样算的。
#[test]
fn age_days_across_month_year_and_leap_boundaries() {
    // 月末 → 次月月初：跨月不重置
    assert_eq!(age_days_of(date("2026-09-30"), date("2026-09-01")), 29);
    assert_eq!(age_days_of(date("2026-10-31"), date("2026-09-01")), 60);
    assert_eq!(age_days_of(date("2026-11-30"), date("2026-09-01")), 90);
    assert_eq!(
        age_days_of(date("2026-12-01"), date("2026-09-01")),
        91,
        "翻月第一天继续累加，不重置"
    );

    // 跨年：12-31 → 次年 01-01 只差一天（不是「新期间第 1 天」）
    assert_eq!(age_days_of(date("2027-01-01"), date("2026-12-31")), 1);
    assert_eq!(age_days_of(date("2027-01-01"), date("2026-12-01")), 31);

    // 闰年 2 月：2028 是闰年（02-29 存在），2027 不是
    assert_eq!(age_days_of(date("2028-03-01"), date("2028-02-29")), 1);
    assert_eq!(
        age_days_of(date("2028-03-01"), date("2028-02-28")),
        2,
        "闰年 02-28 → 03-01 = 2 天"
    );
    assert_eq!(
        age_days_of(date("2027-03-01"), date("2027-02-28")),
        1,
        "平年 02-28 → 03-01 = 1 天"
    );
    assert_eq!(
        age_days_of(date("2028-02-29"), date("2027-02-28")),
        366,
        "闰年整年 366 天"
    );

    // 每月末的库龄必须与「月初入库」的天数逐月对上（跨期报表的口径基数）
    assert_eq!(age_days_of(date("2026-02-28"), date("2026-02-01")), 27);
    assert_eq!(age_days_of(date("2026-01-31"), date("2026-01-01")), 30);
    println!("[M2-11] 库龄是日历天数：跨月 29/60/90/91、跨年 1/31、闰年 2/366 天 ✓");
}

/// 每一期上，`Σ「90 天以上」档数量` 必须等于 `Σ 呆滞层数量`（同一批层的两种判法）。
///
/// 纯逻辑版：不查库，拿一组「层库龄 × 层数量」，用报表的档位函数和呆滞的判定函数
/// 各算一遍，逐期（四个月末 + 翻月首日）都不许不等。真库版见
/// `three_views_agree_at_every_period_end`。
#[test]
fn period_end_identity_holds_for_slow_quantity() {
    // (入库日期, 数量) —— 五个层横跨四个月，含 0 天、30/31、60/61、90/91 这些边界
    let layers: [(NaiveDate, Decimal); 8] = [
        (date("2026-09-01"), dec("60")),
        (date("2026-01-31"), dec("10")),
        (date("2026-06-30"), dec("20")),
        (date("2026-11-30"), dec("5")),
        (date("2026-12-01"), dec("7.5")),
        (date("2026-08-31"), dec("3")),
        (date("2026-10-01"), dec("11")),
        (date("2027-02-28"), dec("4")),
    ];

    for as_of in PERIOD_ENDS {
        let as_of = date(as_of);
        let mut over_90_qty = Decimal::ZERO;
        let mut slow_qty = Decimal::ZERO;
        let mut total_qty = Decimal::ZERO;
        for (inbound, qty) in layers {
            // 基准日早于入库日的层在那一期还不存在（库龄为负是真数据，但这一期不该参与）
            if inbound > as_of {
                continue;
            }
            let age = age_days_of(as_of, inbound);
            total_qty += qty;
            if bucket_of(age) == "OVER_90" {
                over_90_qty += qty;
            }
            if is_slow(age, DEFAULT_THRESHOLD_DAYS) {
                slow_qty += qty;
            }
        }
        assert_eq!(
            over_90_qty, slow_qty,
            "{as_of}：报表「90 天以上」档 {over_90_qty} ≠ 呆滞数量 {slow_qty}"
        );
        assert!(
            slow_qty <= total_qty,
            "{as_of}：呆滞量 {slow_qty} 超过总结存 {total_qty}"
        );
    }
    println!(
        "[M2-11] 逐期恒等式：Σ OVER_90 档 == Σ 呆滞层数量（{} 个期末基准日）✓",
        PERIOD_ENDS.len()
    );
}

/// 期初 / 期末 / 闰日基准日上，`inbound_bounds`（SQL 侧唯一的日期算术）与 `age_days` 逐天等价。
///
/// M2-08 只在一个基准日（2026-11-16）对拍过一次；跨期报表的基准日会落在月末、
/// 月初、闰日上，日期算术一旦在月末错一天（月份天数），报表就会漏层 / 多层。
#[test]
fn inbound_bounds_hold_at_period_ends() {
    let as_ofs = [
        date("2026-08-31"),
        date("2026-09-30"),
        date("2026-10-01"),
        date("2026-11-30"),
        date("2028-02-29"),
        date("2027-01-01"),
    ];
    for as_of in as_ofs {
        for (min, max) in [
            (None, None),
            (Some(0), None),
            (None, Some(30)),
            (Some(31), Some(90)),
            (Some(91), None),
            (Some(60), Some(60)),
        ] {
            let (from, to) = inbound_bounds(as_of, min, max);
            for age in -3..=200_i64 {
                let inbound = as_of - Duration::days(age);
                let want = min.is_none_or(|value| age >= value) && max.is_none_or(|value| age <= value);
                let got = from.is_none_or(|bound| inbound >= bound) && to.is_none_or(|bound| inbound <= bound);
                assert_eq!(
                    got, want,
                    "as_of={as_of} min={min:?} max={max:?} age={age}：折算区间与逐天判档不一致"
                );
            }
        }
    }
    println!("[M2-11] inbound_bounds 在月末 / 月初 / 闰日 / 跨年基准日上逐天等价（6 个基准日 × 6 组区间 × -3..=200）✓");
}

// ==================== 真库用例的连库 / 夹具 ====================

async fn material(code: &str, name: &str) -> Uuid {
    test_seed::ensure_material(code, name).await
}

async fn warehouse(code: &str, name: &str) -> Uuid {
    test_seed::ensure_warehouse(code, name).await
}

/// 清场：只删本模块造的流水 / 单据 / 库存行（主数据由 `test_seed` 复用，不删）。
///
/// 期初流水（`doc_type = OPENING`）的 `doc_no` 形如 `OPENING-2026-09`（生产真实写法，
/// 见 `erp::opening`），不匹配 `M211-` 前缀 —— 所以另按「本卡物料」精确认领，
/// 绝不按 `OPENING-%` 全删（演练库里有别人的期初数据）。
async fn cleanup() {
    for mat in [MAT_LIFE, MAT_SLOW, MAT_BACKDATED, MAT_BOUNDARY, MAT_OPENING] {
        exec(
            &format!(
                "DELETE FROM biz_inventory_txn WHERE doc_no LIKE '{DOC_PREFIX}%' \
                    OR (doc_type = 'OPENING' AND material_id IN (SELECT id FROM biz_material WHERE material_code = '{mat}'))"
            ),
            vec![],
        )
        .await;
        exec(
            &format!(
                "DELETE FROM biz_inventory WHERE material_id IN \
                  (SELECT id FROM biz_material WHERE material_code = '{mat}')"
            ),
            vec![],
        )
        .await;
    }
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
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    warehouse(WAREHOUSE_A_CODE, "跨期仓甲").await;
    warehouse(WAREHOUSE_B_CODE, "跨期仓乙").await;
    material(MAT_LIFE, "跨期物料生命周期").await;
    material(MAT_SLOW, "跨期物料呆滞").await;
    material(MAT_BACKDATED, "跨期物料补录").await;
    material(MAT_BOUNDARY, "跨期物料边界").await;
    material(MAT_OPENING, "跨期物料期初").await;
    cleanup().await;
    Some(guard)
}

/// 造一层入库（单头 + 明细 + 流水）。`posted` 单独传：本卡的关键就是「业务日期 ≠ 过账时间」。
async fn seed_layer(
    material: Uuid,
    warehouse: Uuid,
    in_no: &str,
    in_date: &str,
    qty: &str,
    unit_cost: &str,
    posted: &str,
) {
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
        posted,
    )
    .await;
}

/// 造一笔出库（单头 + 流水）。
async fn seed_out(
    material: Uuid,
    warehouse: Uuid,
    out_no: &str,
    out_date: &str,
    qty: &str,
    unit_cost: &str,
    posted: &str,
) {
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
        posted,
    )
    .await;
}

/// 造一笔**库存期初**流水（`doc_type = OPENING`，生产里由 `erp::opening` 写）。
///
/// 它没有来源单据（`doc_id` 是随机 UUID），层日期只能从 `period_code` 推 —— 见 V0424 的
/// `DATED_CTE` 兜底链。`posted` 故意给得很晚（过账当月），用来证明「层日期不取过账时间」。
async fn seed_opening(material: Uuid, warehouse: Uuid, period_code: &str, qty: &str, unit_cost: &str, posted: &str) {
    exec(
        "INSERT INTO biz_inventory_txn \
            (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, \
             qty, unit_cost, amount, before_qty, before_amount, before_cost, \
             after_qty, after_amount, after_cost, period_code, created_at) \
         VALUES (?, ?, ?, 'IN', 'OPENING', 'OPENING', ?, ?, 1, ?::text::numeric, ?::text::numeric, \
                 0, 0, 0, 0, 0, 0, 0, ?, ?::text::timestamptz)",
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(&material),
            Value::from(&warehouse),
            Value::from(Uuid::now_v7()),
            Value::from(format!("OPENING-{period_code}")),
            Value::Text(qty.to_string()),
            Value::Text(unit_cost.to_string()),
            Value::from(period_code),
            Value::Text(posted.to_string()),
        ],
    )
    .await;
}

// ---- 夹具：五种形态 ----

/// ① 跨期生命周期（物料「生命周期」× 仓甲）：四个月末逐期演进。
///
/// ```text
/// IN-A 100  2026-08-10  ┐
/// IN-B 200  2026-09-15  ├ OUT  120  2026-10-05（FIFO 吃老层）
/// IN-C  50  2026-11-20  ┘
///   ⇒ 08-31: A 100（21 天）                          结存 100
///   ⇒ 09-30: A 100（51）+ B 200（15）                结存 300
///   ⇒ 10-31: B 180（46）（A 被吃满）                 结存 180
///   ⇒ 11-30: B 180（76）+ C 50（10）                 结存 230
///   ⇒ 12-31: B 180（107，呆滞）+ C 50（41）           结存 230，呆滞 180
/// ```
async fn seed_lifecycle() -> (Uuid, Uuid) {
    let material = material(MAT_LIFE, "跨期物料生命周期").await;
    let warehouse = warehouse(WAREHOUSE_A_CODE, "跨期仓甲").await;
    seed_layer(
        material,
        warehouse,
        "M211-IN-A",
        "2026-08-10",
        "100",
        "10",
        "2026-08-11",
    )
    .await;
    seed_layer(
        material,
        warehouse,
        "M211-IN-B",
        "2026-09-15",
        "200",
        "12",
        "2026-09-16",
    )
    .await;
    seed_out(
        material,
        warehouse,
        "M211-OUT-A",
        "2026-10-05",
        "120",
        "10",
        "2026-10-06",
    )
    .await;
    seed_layer(material, warehouse, "M211-IN-C", "2026-11-20", "50", "15", "2026-11-21").await;
    (material, warehouse)
}

/// ② 呆滞翻转（物料「呆滞」× 仓乙）：一层货，不出库，基准日推着走。
///
/// ```text
/// IN-D 60  2026-09-01  ⇒ 09-30: 29 天(D0_30) / 10-31: 60 天(D31_60)
///                     ⇒ 11-30: 90 天(D61_90，仍不呆滞) / 12-01: 91 天(OVER_90，呆滞)
/// ```
async fn seed_slow_flip() -> (Uuid, Uuid) {
    let material = material(MAT_SLOW, "跨期物料呆滞").await;
    let warehouse = warehouse(WAREHOUSE_B_CODE, "跨期仓乙").await;
    seed_layer(material, warehouse, "M211-IN-D", "2026-09-01", "60", "5", "2026-09-02").await;
    (material, warehouse)
}

// ---- 取数（全部走 handler 真路径）----

fn aging_filter(as_of: &str, material: Uuid, warehouse: Uuid, include_consumed: bool) -> AgingFilter {
    AgingFilter {
        material_id: Some(material.to_string()),
        warehouse_id: Some(warehouse.to_string()),
        as_of: Some(as_of.to_string()),
        include_consumed: Some(include_consumed.to_string()),
        page: 1,
        page_size: 100,
        ..AgingFilter::default()
    }
}

async fn layers(as_of: &str, material: Uuid, warehouse: Uuid) -> Vec<AgingLayerRes> {
    handler::layer_page_of(aging_filter(as_of, material, warehouse, false))
        .await
        .unwrap_or_else(|error| panic!("库龄分页失败（{as_of}）：{} {}", error.code, error.msg))
        .list
}

async fn report(as_of: &str, material: Uuid, warehouse: Uuid) -> InventoryAgingReportRes {
    report_handler::report_of(InventoryAgingReportQuery {
        as_of: Some(as_of.to_string()),
        material_id: Some(material.to_string()),
        warehouse_id: Some(warehouse.to_string()),
    })
    .await
    .unwrap_or_else(|error| panic!("库龄报表失败（{as_of}）：{} {}", error.code, error.msg))
}

async fn slow(as_of: &str, material: Uuid, warehouse: Uuid) -> SlowMovingRes {
    slow_handler::slow_moving_of(SlowMovingQuery {
        as_of: Some(as_of.to_string()),
        material_id: Some(material.to_string()),
        warehouse_id: Some(warehouse.to_string()),
        days: Some(DEFAULT_THRESHOLD_DAYS.to_string()),
    })
    .await
    .unwrap_or_else(|error| panic!("呆滞料清单失败（{as_of}）：{} {}", error.code, error.msg))
}

/// 本卡只按 `物料 × 仓库` 过滤取数，所以报表 / 呆滞料的结果里最多一行（有结存时恰好一行）。
fn only_row<'a>(report: &'a InventoryAgingReportRes, as_of: &str) -> &'a MaterialWarehouseAgingRes {
    assert_eq!(
        report.rows.len(),
        1,
        "{as_of}：按 物料 × 仓库 过滤后应恰好 1 行，实际 {} 行",
        report.rows.len()
    );
    &report.rows[0]
}

/// 取报表里某一档的数量（缺档 = 0，与 SQL「空档真的没有货」同义）。
fn bucket_qty(row: &MaterialWarehouseAgingRes, key: &str) -> Decimal {
    row.buckets
        .iter()
        .find(|bucket| bucket.key == key)
        .map(|bucket| bucket.qty)
        .unwrap_or(Decimal::ZERO)
}

fn sum_remaining(rows: &[AgingLayerRes]) -> Decimal {
    rows.iter().map(|row| row.qty_remaining).sum()
}

/// 一个期末基准日上的期望快照（`cross_period_snapshots_evolve_per_period_end` 的用例表）。
///
/// 用结构体而不是四元组：字段一多，`(&str, Vec<(&str, &str, i64)>, &str, Vec<&str>)`
/// 这种类型既过不了 `clippy::type_complexity`，也没人看得懂第 3 位是结存还是层数。
struct PeriodSnapshot {
    /// 基准日（`yyyy-MM-dd`）。
    as_of: &'static str,
    /// 该日**还有结存**的层：`(单号, 剩余数量, 库龄)`，按「最老在前」。
    live: Vec<(&'static str, &'static str, i64)>,
    /// 该日结存合计。
    total: &'static str,
    /// 该日**全部层**（含被 FIFO 吃满、剩余 0 的）单号，按「最老在前」。
    all: Vec<&'static str>,
}

// ==================== 真库用例（跨期）====================

/// 跨期快照：同一批流水，基准日沿月末推进，层剩余 / 结存 / 库龄逐期演进（FIFO 吃老层）。
///
/// 钉住的是「期末库龄报表问的是**那一天账上是什么样**」：每推一个月，只有两件事会变 ——
/// 新入库的层出现、老层按 FIFO 被出库吃掉；已经存在的老层**只会变老，不会消失或改日期**。
#[tokio::test]
async fn cross_period_snapshots_evolve_per_period_end() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_lifecycle().await;

    let expected = [
        PeriodSnapshot {
            as_of: "2026-08-31",
            live: vec![("M211-IN-A", "100", 21)],
            total: "100",
            all: vec!["M211-IN-A"],
        },
        PeriodSnapshot {
            as_of: "2026-09-30",
            live: vec![("M211-IN-A", "100", 51), ("M211-IN-B", "200", 15)],
            total: "300",
            all: vec!["M211-IN-A", "M211-IN-B"],
        },
        PeriodSnapshot {
            as_of: "2026-10-31",
            live: vec![("M211-IN-B", "180", 46)],
            total: "180",
            all: vec!["M211-IN-A", "M211-IN-B"],
        },
        PeriodSnapshot {
            as_of: "2026-11-30",
            live: vec![("M211-IN-B", "180", 76), ("M211-IN-C", "50", 10)],
            total: "230",
            all: vec!["M211-IN-A", "M211-IN-B", "M211-IN-C"],
        },
    ];

    for PeriodSnapshot {
        as_of,
        live: want_layers,
        total: want_total,
        all: want_all,
    } in expected
    {
        let rows = layers(as_of, material, warehouse).await;
        let docs: Vec<&str> = rows.iter().map(|row| row.doc_no.as_str()).collect();
        let want_docs: Vec<&str> = want_layers.iter().map(|(doc, _, _)| *doc).collect();
        assert_eq!(docs, want_docs, "{as_of}：有结存的层不对（FIFO 吃老层）");

        for (row, want) in rows.iter().zip(want_layers.iter()) {
            let (doc, qty, age) = *want;
            assert_eq!(row.doc_no, doc, "{as_of}");
            assert_eq!(row.qty_remaining, dec(qty), "{as_of} {doc}：层剩余不对");
            assert_eq!(row.age_days, age, "{as_of} {doc}：库龄应为 as_of − 入库业务日期");
            assert_eq!(row.as_of, date(as_of), "基准日必须回显");
            assert_eq!(
                row.inbound_date,
                row.as_of - Duration::days(row.age_days),
                "库龄 = 基准日 − 层日期"
            );
        }
        assert_eq!(sum_remaining(&rows), dec(want_total), "{as_of}：结存合计不对");

        // 老层只会变老，**不会消失**：把吃满的层也翻出来，累计的层集合逐期只增不减，
        // 且每层的层日期（入库业务日期）跨期不变 —— 吃满只让剩余归 0，不改层的身份。
        let all = handler::layer_page_of(aging_filter(as_of, material, warehouse, true))
            .await
            .unwrap_or_else(|error| panic!("库龄分页（含吃满层）失败（{as_of}）：{} {}", error.code, error.msg))
            .list;
        let all_docs: Vec<&str> = all.iter().map(|row| row.doc_no.as_str()).collect();
        assert_eq!(all_docs, want_all, "{as_of}：含吃满层的层集合不对（老层被跨期弄丢了）");
        for row in &all {
            let (want_inbound, want_qty_in) = match row.doc_no.as_str() {
                "M211-IN-A" => (date("2026-08-10"), "100"),
                "M211-IN-B" => (date("2026-09-15"), "200"),
                "M211-IN-C" => (date("2026-11-20"), "50"),
                other => panic!("{as_of}：出现了不该有的层 {other}"),
            };
            assert_eq!(
                row.inbound_date, want_inbound,
                "{as_of} {}：层日期跨期不许变",
                row.doc_no
            );
            assert_eq!(
                row.qty_in,
                dec(want_qty_in),
                "{as_of} {}：层入库量跨期不许变",
                row.doc_no
            );
            assert_eq!(
                row.qty_consumed + row.qty_remaining,
                row.qty_in,
                "{as_of} {}：已吃 + 剩余必须等于入库量",
                row.doc_no
            );
        }

        // 报表在同一基准日必须给同一个结存（跨期也不许两个视图各算一套）
        let report = report(as_of, material, warehouse).await;
        let report_row = only_row(&report, as_of);
        assert_eq!(
            report_row.qty_total,
            sum_remaining(&rows),
            "{as_of}：报表结存 ≠ 明细 Σ层剩余"
        );
    }
    println!("[M2-11] 跨期快照：08-31/09-30/10-31/11-30 逐期演进（100 → 300 → 180 → 230）✓");
}

/// 补录单据按**业务日期**归期：业务日期 09-28/09-29、过账 11-25/11-26 的单据，
/// 在 09-30 的期末报表里必须已经算进去（尽管那一刻它还没过账）。
///
/// 这是跨期口径最容易错的一处：用 `created_at`（过账墙钟）当判据，9 月期末就会漏掉
/// 一批「9 月发生的业务、10 月才补录」的单据，而用户拿 9 月表对账时只会看到「少了」。
#[tokio::test]
async fn backdated_document_lands_in_its_business_period() {
    let Some(_guard) = setup().await else { return };
    let material = material(MAT_BACKDATED, "跨期物料补录").await;
    let warehouse = warehouse(WAREHOUSE_B_CODE, "跨期仓乙").await;
    // 业务日期在 9 月，过账在 11 月（补录）
    seed_layer(
        material,
        warehouse,
        "M211-IN-E",
        "2026-09-28",
        "40",
        "3",
        "2026-11-25T02:00:00Z",
    )
    .await;
    seed_out(
        material,
        warehouse,
        "M211-OUT-E",
        "2026-09-29",
        "10",
        "3",
        "2026-11-26T02:00:00Z",
    )
    .await;

    // ① 业务日期之前（09-27）：两层都还没发生
    let rows = layers("2026-09-27", material, warehouse).await;
    assert!(rows.is_empty(), "业务日期晚于基准日 → 不许出现：{rows:?}");

    // ② 9 月期末（09-30）：补录单据必须已经算进来（过账时间 11-25 晚于基准日也不行）
    let rows = layers("2026-09-30", material, warehouse).await;
    assert_eq!(rows.len(), 1, "入库层必须出现");
    let row = &rows[0];
    assert_eq!(row.doc_no, "M211-IN-E");
    assert_eq!(row.inbound_date, date("2026-09-28"), "层日期取业务日期，不取过账时间");
    assert_eq!(row.age_days, 2, "09-30 − 09-28 = 2 天");
    assert_eq!(row.qty_in, dec("40"));
    assert_eq!(row.qty_consumed, dec("10"), "9 月那笔出库（业务日期 09-29）也要算进来");
    assert_eq!(row.qty_remaining, dec("30"));
    assert!(
        row.posted_at > date("2026-09-30").and_hms_opt(0, 0, 0).unwrap().and_utc(),
        "夹具前提：过账时间确实晚于基准日（否则这条用例证明不了「按业务日期归期」）"
    );

    // ③ 10 月期末：同一层的库龄继续累加（不因「跨期」重置）
    let rows = layers("2026-10-31", material, warehouse).await;
    assert_eq!(rows[0].age_days, 33, "10-31 − 09-28 = 33 天");
    assert_eq!(rows[0].qty_remaining, dec("30"), "10 月没有再动，剩余不变");

    // ④ 报表同样按业务日期归期：9 月期末这一格已经有 30 件（落 0-30 档）
    let report = report("2026-09-30", material, warehouse).await;
    let report_row = only_row(&report, "2026-09-30");
    assert_eq!(report_row.qty_total, dec("30"));
    assert_eq!(bucket_qty(report_row, "D0_30"), dec("30"));
    println!("[M2-11] 补录按业务日期归期：09-28/09-29 的补录单进 9 月期末（过账 11 月不影响）✓");
}

/// 呆滞判定跨期翻转：一层货 29 → 60 → 90 天都不呆滞，**91 天**才呆滞；档位同期迁移。
///
/// 这是「跨期 × 呆滞判定」的交点：M2-10 钉的是**同一天换阈值**（N=89/90/91），
/// 本用例钉的是**同一层货换基准日** —— 业务上更常见的问法是「这批货几月份算呆滞」。
/// 两边必须给同一个答案（同一把尺：`库龄 > N`），所以本用例同时读明细、报表、呆滞三个视图。
#[tokio::test]
async fn slow_flag_flips_when_period_advances_past_threshold() {
    let Some(_guard) = setup().await else { return };
    let (material, warehouse) = seed_slow_flip().await;

    // (基准日, 库龄, 报表档位, 是否呆滞)
    let timeline = [
        ("2026-09-30", 29, "D0_30", false),
        ("2026-10-31", 60, "D31_60", false),
        ("2026-11-30", 90, "D61_90", false),
        ("2026-12-01", 91, "OVER_90", true),
        ("2026-12-31", 121, "OVER_90", true),
    ];

    for (as_of, age, bucket, slow_expected) in timeline {
        // ① 明细：库龄就是 as_of − 入库日期
        let rows = layers(as_of, material, warehouse).await;
        assert_eq!(rows.len(), 1, "{as_of}：只有一层货");
        assert_eq!(rows[0].age_days, age, "{as_of}：库龄不对");
        assert_eq!(rows[0].inbound_date, date("2026-09-01"), "层日期跨期不变");
        assert_eq!(rows[0].qty_remaining, dec("60"), "{as_of}：没有出库，剩余恒为 60");

        // ② 报表：整层落进哪一档（跨期逐档迁移）
        let report = report(as_of, material, warehouse).await;
        let report_row = only_row(&report, as_of);
        assert_eq!(
            bucket_qty(report_row, bucket),
            dec("60"),
            "{as_of}：报表档位应为 {bucket}"
        );
        let non_zero: Vec<&str> = report_row
            .buckets
            .iter()
            .filter(|cell| cell.qty > Decimal::ZERO)
            .map(|cell| cell.key.as_str())
            .collect();
        assert_eq!(non_zero, vec![bucket], "{as_of}：只应有一个非空档，实际 {non_zero:?}");

        // ③ 呆滞：只有 91 天那次才进行（呆滞表只返回有呆滞的格）
        let slow_res = slow(as_of, material, warehouse).await;
        assert_eq!(
            slow_res.threshold_days, DEFAULT_THRESHOLD_DAYS,
            "{as_of}：阈值口径固定 90"
        );
        assert_eq!(slow_res.summary.scanned_row_count, 1, "{as_of}：这一格被扫过（分母在）");
        if slow_expected {
            assert_eq!(slow_res.row_count, 1, "{as_of}：{age} 天应判呆滞");
            let row = &slow_res.rows[0];
            assert_eq!(row.qty_slow, dec("60"), "{as_of}：整层 60 件呆滞");
            assert_eq!(row.qty_on_hand, dec("60"));
            assert_eq!(row.slow_ratio, dec("1"));
            assert_eq!(row.slow_layer_count, 1);
            assert_eq!(row.max_age_days, age);
            assert_eq!(
                bucket_qty(report_row, "OVER_90"),
                row.qty_slow,
                "呆滞量必须等于报表 90+ 档"
            );
        } else {
            assert_eq!(slow_res.row_count, 0, "{as_of}：{age} 天还不算呆滞（严格大于 90）");
            assert!(slow_res.rows.is_empty());
            assert_eq!(slow_res.summary.qty_slow, Decimal::ZERO);
            assert_eq!(
                slow_res.summary.qty_on_hand,
                dec("60"),
                "{as_of}：不呆滞的货也算进结存分母"
            );
        }
    }
    println!("[M2-11] 呆滞跨期翻转：29/60/90 天不呆滞，91 天转呆滞；档位 0-30→31-60→61-90→90+ 同步 ✓");
}

/// 三个视图在**每一个**期末基准日互相自洽（这是本卡的核心不变量）。
///
/// 逐期逐格断言四条恒等式：
///   ① Σ层剩余（明细）= 报表 `qty_total` = Σ报表各档；
///   ② 报表层数 = 明细层数（有结存的层）；
///   ③ N=90 时呆滞 `qty_slow` == 报表「90 天以上」档数量（呆滞是 90+ 档的子集，数量相等）；
///   ④ 呆滞 `qty_on_hand` == 结存合计（不呆滞的货也算进分母，否则比例会虚高）。
#[tokio::test]
async fn three_views_agree_at_every_period_end() {
    let Some(_guard) = setup().await else { return };
    let life = seed_lifecycle().await;
    let slow_cell = seed_slow_flip().await;

    for as_of in PERIOD_ENDS {
        for (material, warehouse) in [life, slow_cell] {
            let rows = layers(as_of, material, warehouse).await;
            let report = report(as_of, material, warehouse).await;
            let slow_res = slow(as_of, material, warehouse).await;
            let detail_total = sum_remaining(&rows);

            if rows.is_empty() {
                // 基准日早于该格所有入库业务日期：三个视图都必须是「空」，且都不报错
                assert!(report.rows.is_empty(), "{as_of}：明细空但报表有行");
                assert_eq!(report.qty_total, Decimal::ZERO);
                assert_eq!(slow_res.row_count, 0);
                assert_eq!(slow_res.summary.scanned_row_count, 0, "{as_of}：没结存的格不算被扫过");
                assert_eq!(slow_res.summary.qty_on_hand, Decimal::ZERO);
                continue;
            }

            let report_row = only_row(&report, as_of);
            let bucket_total: Decimal = report_row.buckets.iter().map(|bucket| bucket.qty).sum();
            let over_90 = bucket_qty(report_row, "OVER_90");

            assert_eq!(report_row.qty_total, detail_total, "{as_of}：报表结存 ≠ 明细 Σ层剩余");
            assert_eq!(bucket_total, detail_total, "{as_of}：Σ档 ≠ 结存（档位表把货弄丢了）");
            assert_eq!(
                report_row.layer_count,
                rows.len() as i64,
                "{as_of}：报表层数 ≠ 明细层数"
            );
            assert_eq!(report.qty_total, detail_total, "{as_of}：报表合计 ≠ 明细");
            assert_eq!(report.row_count, 1);

            assert_eq!(slow_res.summary.scanned_row_count, 1, "{as_of}");
            assert_eq!(
                slow_res.summary.qty_on_hand, detail_total,
                "{as_of}：呆滞表结存分母 ≠ 明细"
            );
            if over_90 > Decimal::ZERO {
                assert_eq!(slow_res.row_count, 1, "{as_of}：有 90+ 档就必须有呆滞行");
                let row = &slow_res.rows[0];
                assert_eq!(row.qty_slow, over_90, "{as_of}：呆滞量 ≠ 报表 90+ 档");
                assert_eq!(row.qty_on_hand, detail_total);
                assert_eq!(slow_res.summary.qty_slow, over_90);
            } else {
                assert_eq!(slow_res.row_count, 0, "{as_of}：没有 90+ 档就不该有呆滞行");
                assert_eq!(slow_res.summary.qty_slow, Decimal::ZERO);
            }

            // 呆滞一定落在 OVER_90 之内：呆滞层数 ≤ 该档层数（同一个判据的两个投影）
            let over_90_layers = report_row
                .buckets
                .iter()
                .find(|bucket| bucket.key == "OVER_90")
                .map(|bucket| bucket.layer_count)
                .unwrap_or(0);
            let slow_layers = slow_res.rows.first().map(|row| row.slow_layer_count).unwrap_or(0);
            assert_eq!(slow_layers, over_90_layers, "{as_of}：呆滞层数 ≠ 90+ 档层数");
        }
    }
    println!(
        "[M2-11] 三视图逐期自洽：{} 个基准日 × 2 格（明细 Σ层剩余 = Σ档 = 呆滞 + 不呆滞；N=90 时呆滞 == OVER_90）✓",
        PERIOD_ENDS.len()
    );
}

/// 期末 / 期初当天**含**当天：业务日期 == 基准日的单据计入；差一天就不计入。
///
/// 差一天是跨期报表最典型的错法（`<` 与 `<=` 之争）：9 月 30 日的出库如果被算进 10 月，
/// 9 月期末就会虚高一层货、10 月则少一笔出库 —— 而且逐月看都不报错。
#[tokio::test]
async fn period_boundary_day_is_inclusive() {
    let Some(_guard) = setup().await else { return };
    let material = material(MAT_BOUNDARY, "跨期物料边界").await;
    let warehouse = warehouse(WAREHOUSE_A_CODE, "跨期仓甲").await;
    // 10-01 当天入库 10 件、当天又出库 4 件（业务日期都是 10-01）
    seed_layer(
        material,
        warehouse,
        "M211-IN-F",
        "2026-10-01",
        "10",
        "1",
        "2026-10-01T02:00:00Z",
    )
    .await;
    seed_out(
        material,
        warehouse,
        "M211-OUT-F",
        "2026-10-01",
        "4",
        "1",
        "2026-10-01T03:00:00Z",
    )
    .await;

    // ① 前一天（09-30）：两层都没发生
    let rows = layers("2026-09-30", material, warehouse).await;
    assert!(rows.is_empty(), "业务日期 10-01 的单据不许出现在 09-30 的期末");

    // ② 当天（10-01）：入库与出库都算进来（含当天），当天出库后结存 6
    let rows = layers("2026-10-01", material, warehouse).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].qty_in, dec("10"));
    assert_eq!(rows[0].qty_consumed, dec("4"), "当天入库、当天出库：出库也得算进来");
    assert_eq!(rows[0].qty_remaining, dec("6"));
    assert_eq!(rows[0].age_days, 0, "当天入库，库龄 0（不是 1）");

    // ③ 次日（10-02）：结存不变，库龄 +1
    let rows = layers("2026-10-02", material, warehouse).await;
    assert_eq!(rows[0].qty_remaining, dec("6"));
    assert_eq!(rows[0].age_days, 1);

    // ④ 报表与呆滞在当天也自洽（新货不许因为「入库日期 = 基准日」被判成呆滞或漏档）
    let report = report("2026-10-01", material, warehouse).await;
    let report_row = only_row(&report, "2026-10-01");
    assert_eq!(bucket_qty(report_row, "D0_30"), dec("6"));
    assert_eq!(slow("2026-10-01", material, warehouse).await.row_count, 0);
    println!("[M2-11] 边界当天含：10-01 进 + 出都在当天期末可见（结存 6、库龄 0）✓");
}

/// 期初层（`doc_type = OPENING`）的层日期 = `period_code` 月初，**不取过账时间**。
///
/// 生产里库存期初是「开账时一次性录入」的（过账墙钟 = 开账当天），而它的业务含义是
/// 「**那一期开始时**账上就有这些货」。如果层日期取 `created_at`，9 月期初会被算成
/// 11 月进的货，档位从「90 天以上」直接掉到「0-30 天」—— 呆滞料清单会凭空少一批老货。
#[tokio::test]
async fn opening_layer_dates_from_period_start() {
    let Some(_guard) = setup().await else { return };
    let material = material(MAT_OPENING, "跨期物料期初").await;
    let warehouse = warehouse(WAREHOUSE_A_CODE, "跨期仓甲").await;
    // 期间 2026-09 的期初，过账时间故意给 11-28（开账晚于期间起点是常态）
    seed_opening(material, warehouse, "2026-09", "30", "8", "2026-11-28T02:00:00Z").await;

    // ① 8 月末：期初属于 9 月，还不该出现
    let rows = layers("2026-08-31", material, warehouse).await;
    assert!(rows.is_empty(), "2026-09 的期初不许出现在 8 月末：{rows:?}");

    // ② 9 月 1 日（期间起点）：出现，层日期 = 09-01，库龄 0
    let rows = layers("2026-09-01", material, warehouse).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].doc_type, "OPENING");
    assert_eq!(rows[0].biz_type, "OPENING");
    assert_eq!(rows[0].inbound_date, date("2026-09-01"), "层日期 = period_code 月初");
    assert_eq!(rows[0].age_days, 0);
    assert_eq!(rows[0].qty_remaining, dec("30"));

    // ③ 11 月末（过账之后）：层日期仍是 09-01，库龄 90，不是「11-28 入库的 2 天新货」
    let rows = layers("2026-11-30", material, warehouse).await;
    assert_eq!(rows[0].inbound_date, date("2026-09-01"), "过账时间不参与层日期");
    assert_eq!(rows[0].age_days, 90, "11-30 − 09-01 = 90 天");
    let report = report("2026-11-30", material, warehouse).await;
    let report_row = only_row(&report, "2026-11-30");
    assert_eq!(bucket_qty(report_row, "D61_90"), dec("30"), "90 天还在 61-90 档");
    assert_eq!(
        slow("2026-11-30", material, warehouse).await.row_count,
        0,
        "90 天整还不算呆滞"
    );

    // ④ 12-01：91 天，转呆滞（期初也会老，也要进呆滞清单）
    let slow_res = slow("2026-12-01", material, warehouse).await;
    assert_eq!(slow_res.row_count, 1);
    assert_eq!(slow_res.rows[0].qty_slow, dec("30"));
    assert_eq!(slow_res.rows[0].oldest_inbound_date, date("2026-09-01"));
    println!("[M2-11] 期初层：层日期 = 期间月初（09-01），11-30 满 90 天、12-01 转呆滞 ✓");
}

/// 清场用例（放最后跑：其余用例共用 `M211-` 夹具，谁先谁后都靠 `setup` 重置）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    // 先造一批（证明清理确实作用在数据上，而不是「本来就没有」）
    seed_lifecycle().await;
    seed_slow_flip().await;
    seed_opening(
        material(MAT_OPENING, "跨期物料期初").await,
        warehouse(WAREHOUSE_A_CODE, "跨期仓甲").await,
        "2026-09",
        "30",
        "8",
        "2026-11-28T02:00:00Z",
    )
    .await;

    cleanup().await;
    let statement = format!(
        "SELECT (SELECT count(*) FROM biz_inventory_txn WHERE doc_no LIKE '{DOC_PREFIX}%' \
                     OR (doc_type = 'OPENING' AND material_id IN \
                         (SELECT id FROM biz_material WHERE material_code = '{MAT_OPENING}'))) \
              + (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE '{DOC_PREFIX}%') \
              + (SELECT count(*) FROM biz_stock_out WHERE out_no LIKE '{DOC_PREFIX}%')"
    );
    let left: i64 = db().scalar_with_params(&statement, vec![]).await.expect("清场核对");
    assert_eq!(left, 0, "清场后 M211- 残留必须为 0");
    println!("[M2-11] 清场：M211- 残留 0（含期初流水）✓");
}

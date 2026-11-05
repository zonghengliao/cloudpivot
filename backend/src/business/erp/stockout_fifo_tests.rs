// ============================================================================
// stockout_fifo_tests.rs —— P1-FIX-2「出库 FIFO 自动分配」用例
//
// 覆盖卡面验收项：
//   ① FIFO 贪心分配（纯函数）：最早建账的库位先取，跨桶凑满，缺口如实报出；
//   ② 建单就把库位**预填**进明细（不再等到过账才挑）；
//   ③ 跨库位够量 → 建单**自动拆成多行**，过账一次走通（不再要求用户手工拆行）；
//   ④ 手改覆盖：显式传 `location_id` 就只扣那一桶，FIFO 不参与；
//   ⑤ 部分库存 / 完全没货 → 建单可存但详情给出「差多少 + 现在货在哪些库位」的提示，
//      过账 409 的文案同样点明缺口与分布，且整笔回滚（不落半张单）；
//   ⑥ 同一张单多行同一物料不会重复分配同一个桶；
//   ⑦ 「未分配」桶照样参与 FIFO（它是真实存在的一个桶，不是特例）。
//
// ⚠ `ERP_DB_URL` 未设置时真库用例全部 SKIP（假绿判据：跑完总耗时一秒 vs 几分钟）。
// 纯函数用例（`fifo_*` 前 4 条）不依赖数据库，任何环境都会跑。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::*;
use crate::business::erp::stockin_model;
use crate::business::erp::stockout_dto::{CreateStockOutReq, StockOutItemReq};
use crate::business::erp::stockout_handler;

// ---------- 夹具（与 stockout_tests.rs 的 d2030100-* 不重叠） ----------

const WAREHOUSE: &str = "d2f1f200-0000-4000-8000-000000000002";
const WAREHOUSE_NEG: &str = "d2f1f200-0000-4000-8000-000000000003";
const MATERIAL: &str = "d2f1f200-0000-4000-8000-000000000011";
const LOC_A: &str = "d2f1f200-0000-4000-8000-0000000000a1";
const LOC_B: &str = "d2f1f200-0000-4000-8000-0000000000a2";
const LOC_CODE_A: &str = "P1F2-L01";
const LOC_CODE_B: &str = "P1F2-L02";

const PINNED_MATERIALS: &str =
    "('d2f1f200-0000-4000-8000-000000000011'::uuid, 'd2f1f200-0000-4000-8000-000000000012'::uuid)";

const FIXTURES: &[&str] = &[
    "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
     VALUES ('d2f1f200-0000-4000-8000-000000000001', 'P1F2-PCS', '个', 1, NULL, 1.000000, 1, 1) \
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative) \
     VALUES ('d2f1f200-0000-4000-8000-000000000002', 'P1F2-WH', 'P1F2 出库仓', \
             '/d2f1f200-0000-4000-8000-000000000002/', 1, 1, 1, 0), \
            ('d2f1f200-0000-4000-8000-000000000003', 'P1F2-WHN', 'P1F2 负库存仓', \
             '/d2f1f200-0000-4000-8000-000000000003/', 1, 1, 1, 1) \
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
     VALUES ('d2f1f200-0000-4000-8000-000000000011', 'P1F2-M1', 'P1F2 螺丝', \
             'd2f1f200-0000-4000-8000-000000000001', 1, false), \
            ('d2f1f200-0000-4000-8000-000000000012', 'P1F2-M2', 'P1F2 垫片', \
             'd2f1f200-0000-4000-8000-000000000001', 1, false) \
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_location (id, warehouse_id, location_code, location_name, is_default, status, sort_order) \
     VALUES ('d2f1f200-0000-4000-8000-0000000000a1', 'd2f1f200-0000-4000-8000-000000000002', \
             'P1F2-L01', 'P1F2 A 库位', 0, 1, 1), \
            ('d2f1f200-0000-4000-8000-0000000000a2', 'd2f1f200-0000-4000-8000-000000000002', \
             'P1F2-L02', 'P1F2 B 库位', 0, 1, 2) \
     ON CONFLICT DO NOTHING",
];

/// 真库用例必须串行（共用全局 `db()`、同一批夹具与同一批 `biz_seq`）。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => set_db(connection).is_ok(),
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

fn uuid(raw: &str) -> Uuid {
    Uuid::parse_str(raw).expect("夹具 UUID 非法")
}

fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>().expect("数量字面量非法")
}

async fn sql(statement: &str) {
    db().exec_sql(statement.to_string(), vec![])
        .await
        .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
}

/// 清场：只清本模块夹具物料 / 夹具仓库的账，不碰别的用例（stockout_tests 用 d2030100-*）。
async fn cleanup_fixtures() {
    // ① 凭证（先红字后蓝字；分录随凭证级联删）
    sql(&format!(
        "DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN \
            (SELECT stock_out_id FROM biz_stock_out_item WHERE material_id IN {PINNED_MATERIALS})"
    ))
    .await;
    // ② 状态日志（无外键，按本模块单据 id 收窄）
    sql(&format!(
        "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_OUT' AND entity_id IN \
            (SELECT stock_out_id FROM biz_stock_out_item WHERE material_id IN {PINNED_MATERIALS})"
    ))
    .await;
    // ③ 单据（明细 ON DELETE CASCADE）
    sql(&format!(
        "DELETE FROM biz_stock_out WHERE id IN \
            (SELECT stock_out_id FROM biz_stock_out_item WHERE material_id IN {PINNED_MATERIALS})"
    ))
    .await;
    // ④ 流水
    sql(&format!(
        "DELETE FROM biz_inventory_txn WHERE material_id IN {PINNED_MATERIALS}"
    ))
    .await;
    // ⑤ 两本账
    sql(&format!(
        "DELETE FROM biz_inventory WHERE material_id IN {PINNED_MATERIALS}"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_inventory_location WHERE material_id IN {PINNED_MATERIALS}"
    ))
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup_fixtures().await;
    for statement in FIXTURES {
        sql(statement).await;
    }
    Some(guard)
}

// ---------- 造库存（可控 FIFO 次序） ----------

/// 往某个库位桶里放一笔货（三本账里只做「成本账 + 库位账」，不建凭证 —— 本模块只验出库侧）。
///
/// `dict = None` → 「未分配」库位桶；`age_minutes` 越大 = 建账越早 = FIFO 越先被扣。
async fn seed_bucket(dict: Option<&str>, qty: &str, age_minutes: i64) -> Uuid {
    seed_bucket_in(WAREHOUSE, MATERIAL, dict, qty, age_minutes).await
}

async fn seed_bucket_in(
    warehouse: &str,
    material: &str,
    dict: Option<&str>,
    qty: &str,
    age_minutes: i64,
) -> Uuid {
    let now = Utc::now();
    let warehouse_id = uuid(warehouse);
    let material_id = uuid(material);
    let quantity = dec(qty);
    let amount = (quantity * dec("10")).round_dp(4);
    let ledger_row = match dict {
        Some(dict_id) => {
            let row = stockin_model::ensure_location_row(
                db(),
                material_id,
                warehouse_id,
                uuid(dict_id),
                now,
            )
            .await
            .expect("库位账行 find-or-create");
            stockin_model::apply_location_qty(
                db(),
                material_id,
                warehouse_id,
                Some(row),
                quantity,
                now,
            )
            .await
            .expect("库位账加法");
            row
        }
        None => {
            stockin_model::apply_location_qty(db(), material_id, warehouse_id, None, quantity, now)
                .await
                .expect("未分配桶加法");
            null_bucket_row(warehouse, material)
                .await
                .expect("未分配桶行")
        }
    };
    let cost_bucket = if dict.is_some() {
        Some(ledger_row)
    } else {
        None
    };
    stockin_model::apply_cost(
        db(),
        material_id,
        warehouse_id,
        cost_bucket,
        stockin_model::CostBasis::of(quantity, amount),
        now,
    )
    .await
    .expect("成本账加法");
    // 把 FIFO 次序钉死（不靠两个事务的时间戳碰运气）
    sql(&format!(
        "UPDATE biz_inventory_location SET created_at = now() - interval '{age_minutes} minutes' \
          WHERE id = '{ledger_row}'::uuid"
    ))
    .await;
    ledger_row
}

async fn null_bucket_row(warehouse: &str, material: &str) -> Option<Uuid> {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT id FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await
    .expect("查未分配桶行")
}

/// 某库位字典当前的库位账数量（行不存在 → 0）。
async fn qty_at(dict: Option<&str>) -> Decimal {
    db().scalar_with_params::<Option<Decimal>>(
        "SELECT qty FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
            AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
              = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid)",
        vec![
            Value::from(&uuid(MATERIAL)),
            Value::from(&uuid(WAREHOUSE)),
            Value::Text(dict.unwrap_or_default().to_string()),
        ],
    )
    .await
    .expect("查库位账数量")
    .unwrap_or(Decimal::ZERO)
}

/// 建一张出库单（`location_row = None` → 走 FIFO 自动分配）。
async fn out_doc(qty: &str, location_row: Option<Uuid>) -> Uuid {
    out_doc_lines(vec![StockOutItemReq {
        material_id: MATERIAL.to_string(),
        qty: dec(qty),
        location_id: location_row.map(|id| id.to_string()),
        lot_no: None,
        remark: None,
    }])
    .await
}

async fn out_doc_lines(items: Vec<StockOutItemReq>) -> Uuid {
    let req = CreateStockOutReq {
        out_type: Some("ISSUE".to_string()),
        warehouse_id: Some(WAREHOUSE.to_string()),
        out_date: None,
        reason: Some("P1-FIX-2 FIFO 用例".to_string()),
        remark: None,
        items,
    };
    stockout_handler::create_stock_out(&req, None)
        .await
        .unwrap_or_else(|error| panic!("建出库单失败：{} {}", error.code, error.msg))
}

/// 明细按**行号**排好（`items_of` 是按锁顺序排的，断言 FIFO 顺序必须看 `line_no`）。
fn detail_rows(
    detail: &crate::business::erp::stockout_dto::StockOutDetailRes,
) -> Vec<(i32, Option<Uuid>, Decimal)> {
    let mut rows: Vec<(i32, Option<Uuid>, Decimal)> = detail
        .items
        .iter()
        .map(|item| {
            (
                item.line_no,
                item.location_id
                    .as_deref()
                    .map(|raw| Uuid::parse_str(raw).expect("明细 location_id 非法")),
                dec(&item.qty),
            )
        })
        .collect();
    rows.sort_by_key(|(line_no, _, _)| *line_no);
    rows
}

// ============================================================================
// ①~④ 纯函数：FIFO 贪心分配（不连库，任何环境都跑）
// ============================================================================

fn buckets(list: &[(&str, &str)]) -> Vec<FifoBucket> {
    list.iter()
        .enumerate()
        .map(|(index, (code, qty))| FifoBucket {
            location_row_id: Some(Uuid::from_u128(index as u128 + 1)),
            location_code: (*code).to_string(),
            available: dec(qty),
        })
        .collect()
}

/// FIFO 顺序：先取最早建账的桶，取满即停。
#[test]
fn fifo_allocate_takes_earliest_bucket_first() {
    let list = buckets(&[("L01", "30"), ("L02", "100")]);
    let (picks, shortfall) = allocate_fifo(&list, dec("50"));
    assert_eq!(shortfall, Decimal::ZERO, "总量 130 ≥ 50，不该有缺口");
    assert_eq!(picks.len(), 2, "30 不够 50，必须跨到第二个桶");
    assert_eq!(picks[0].location_code, "L01");
    assert_eq!(picks[0].take, dec("30"), "先取最早建账的桶");
    assert_eq!(picks[1].location_code, "L02");
    assert_eq!(picks[1].take, dec("20"), "只从第二个桶补差 20");
}

/// 刚好凑满：整桶取完不留缺口（P1-FIX-2 的「不再手工拆行」最小复现）。
#[test]
fn fifo_allocate_exact_sum_has_no_shortfall() {
    let list = buckets(&[("L01", "30"), ("L02", "30")]);
    let (picks, shortfall) = allocate_fifo(&list, dec("60"));
    assert_eq!(shortfall, Decimal::ZERO);
    assert_eq!(picks.len(), 2);
    assert_eq!(picks[0].take + picks[1].take, dec("60"));
}

/// 部分库存：缺口如实报出，且**不**把不够的量硬塞进桶里。
#[test]
fn fifo_allocate_reports_shortfall() {
    let list = buckets(&[("L01", "30"), ("L02", "20")]);
    let (picks, shortfall) = allocate_fifo(&list, dec("60"));
    assert_eq!(shortfall, dec("10"), "60 − (30+20) = 10");
    assert_eq!(picks.len(), 2);
    assert_eq!(picks[1].take, dec("20"), "每个桶最多取到自己的可用量");
}

/// 完全没货：拣货为空、缺口 = 需求，摘要说「无库存」。
#[test]
fn fifo_allocate_without_stock_is_all_shortfall() {
    let (picks, shortfall) = allocate_fifo(&[], dec("5"));
    assert!(picks.is_empty());
    assert_eq!(shortfall, dec("5"));
    assert_eq!(describe_buckets(&[]), "无库存");
}

/// 小数数量也按 4 位口径对齐（`numeric(18,4)`，别在尾数上差一分）。
#[test]
fn fifo_allocate_keeps_four_decimals() {
    let list = buckets(&[("L01", "1.2345"), ("L02", "10")]);
    let (picks, shortfall) = allocate_fifo(&list, dec("2.5"));
    assert_eq!(shortfall, Decimal::ZERO);
    assert_eq!(picks[0].take, dec("1.2345"));
    assert_eq!(picks[1].take, dec("1.2655"));
}

// ============================================================================
// ⑤ 真库：建单即预填（单库位够量）
// ============================================================================

/// 单库位够量 → 建单就定死库位（不需要等到过账），且**不**多拆行。
#[tokio::test]
async fn fifo_create_prefills_single_location() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row_a = seed_bucket(Some(LOC_A), "50", 60).await;

    let doc = out_doc("20", None).await;
    let detail = stockout_handler::detail_of(doc).await.expect("详情");

    let rows = detail_rows(&detail);
    assert_eq!(rows.len(), 1, "单库位够量不该拆行");
    assert_eq!(
        rows[0].1,
        Some(row_a),
        "建单必须把 FIFO 选中的库位预填进明细"
    );
    assert_eq!(rows[0].2, dec("20"));
    assert_eq!(
        dec(&detail.header.total_qty),
        dec("20"),
        "拆行不能改变单头合计"
    );
    assert!(
        detail.stock_warnings.is_empty(),
        "库存够量不该有提示：{:?}",
        detail.stock_warnings
    );
}

// ============================================================================
// ⑥ 真库：跨库位够量 → 自动拆行 + 过账一次走通
// ============================================================================

/// A 30（早）+ B 30（晚），领用 60 → 建单拆成 2 行，过账一次走通、两桶清零。
///
/// 这就是评审实测 ③ 段的反例：同样是「单个库位都不够 60」，现在系统自己拆，不再 409
/// 让用户手工拆行。
#[tokio::test]
async fn fifo_create_auto_splits_across_locations_and_posts() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row_a = seed_bucket(Some(LOC_A), "30", 120).await;
    let row_b = seed_bucket(Some(LOC_B), "30", 60).await;

    let doc = out_doc("60", None).await;
    let detail = stockout_handler::detail_of(doc).await.expect("详情");
    let rows = detail_rows(&detail);
    assert_eq!(rows.len(), 2, "跨库位够量必须自动拆成 2 行");
    assert_eq!(
        rows[0],
        (1, Some(row_a), dec("30")),
        "第 1 行 = 最早建账的 A，整桶"
    );
    assert_eq!(
        rows[1],
        (2, Some(row_b), dec("30")),
        "第 2 行 = 后建的 B，补差 30"
    );
    assert_eq!(dec(&detail.header.total_qty), dec("60"), "拆行后合计不变");
    assert!(detail.stock_warnings.is_empty(), "够量不该提示");

    stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("跨库位出库应一次过账成功（不再要求手工拆行）");
    assert_eq!(qty_at(Some(LOC_A)).await, Decimal::ZERO, "A 被扣光");
    assert_eq!(qty_at(Some(LOC_B)).await, Decimal::ZERO, "B 被扣光");
}

/// FIFO 顺序：A 20（早）+ B 50（晚）领 30 → A 先扣光（20），再从 B 取 10。
#[tokio::test]
async fn fifo_split_follows_creation_order() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row_a = seed_bucket(Some(LOC_A), "20", 120).await;
    let row_b = seed_bucket(Some(LOC_B), "50", 60).await;

    let doc = out_doc("30", None).await;
    let rows = detail_rows(&stockout_handler::detail_of(doc).await.expect("详情"));
    assert_eq!(
        rows,
        vec![(1, Some(row_a), dec("20")), (2, Some(row_b), dec("10"))]
    );

    stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("过账");
    assert_eq!(qty_at(Some(LOC_A)).await, Decimal::ZERO);
    assert_eq!(qty_at(Some(LOC_B)).await, dec("40"), "B 只剩 40");
}

// ============================================================================
// ⑦ 真库：手改覆盖自动值
// ============================================================================

/// 显式指定后建的 B（FIFO 本该先扣 A）→ 只扣 B，A 一克不动。
#[tokio::test]
async fn fifo_manual_location_overrides_auto() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    seed_bucket(Some(LOC_A), "50", 120).await;
    let row_b = seed_bucket(Some(LOC_B), "50", 60).await;

    let doc = out_doc("30", Some(row_b)).await;
    let rows = detail_rows(&stockout_handler::detail_of(doc).await.expect("详情"));
    assert_eq!(
        rows,
        vec![(1, Some(row_b), dec("30"))],
        "手填库位覆盖 FIFO，且不拆行"
    );

    stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("指定库位出库");
    assert_eq!(qty_at(Some(LOC_B)).await, dec("20"), "只扣 B");
    assert_eq!(qty_at(Some(LOC_A)).await, dec("50"), "A 不动");
}

// ============================================================================
// ⑧ 真库：部分库存 / 完全没货 → 提示清晰、过账 409 且整笔回滚
// ============================================================================

/// A 30 + B 20，领 60（差 10）→ 建单可存但详情点名「差 10 + 分布」，过账 409 同样说清，
/// 且账一分没动（整笔回滚）。
#[tokio::test]
async fn fifo_insufficient_stock_warns_then_rejects_post() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    seed_bucket(Some(LOC_A), "30", 120).await;
    seed_bucket(Some(LOC_B), "20", 60).await;

    let doc = out_doc("60", None).await;
    let detail = stockout_handler::detail_of(doc).await.expect("详情");
    let rows = detail_rows(&detail);
    assert_eq!(rows.len(), 1, "不够量时保持单行（不硬拆）");
    assert_eq!(rows[0].1, None, "不够量时不预填库位（交给过账拦）");
    assert_eq!(detail.stock_warnings.len(), 1, "应给出一条库存提示");
    let warning = &detail.stock_warnings[0];
    for expected in ["差 10.0000", LOC_CODE_A, LOC_CODE_B, "可用合计 50.0000"] {
        assert!(
            warning.contains(expected),
            "提示要包含「{expected}」：{warning}"
        );
    }

    let error = stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect_err("库存不足必须 409");
    assert_eq!(error.status, 409);
    for expected in ["差 10.0000", LOC_CODE_A, LOC_CODE_B, "可用合计 50.0000"] {
        assert!(
            error.msg.contains(expected),
            "409 文案要包含「{expected}」：{}",
            error.msg
        );
    }
    assert!(
        error.msg.contains("自动"),
        "文案要指出「重新保存草稿会自动拆行」：{}",
        error.msg
    );
    assert!(
        !error.msg.contains("请把这一行拆成多行"),
        "旧口径「请手工拆行」必须消失：{}",
        error.msg
    );
    // 整笔回滚：状态没变、两桶原样
    let after = stockout_handler::detail_of(doc).await.expect("详情");
    assert_eq!(
        after.header.status, "DRAFT",
        "过账失败必须整笔回滚（状态不变）"
    );
    assert_eq!(qty_at(Some(LOC_A)).await, dec("30"));
    assert_eq!(qty_at(Some(LOC_B)).await, dec("20"));
}

/// 完全没货 → 提示「可用合计 0」，过账 409 说清「无货可出」。
#[tokio::test]
async fn fifo_no_stock_at_all_is_explained() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let doc = out_doc("5", None).await;
    let detail = stockout_handler::detail_of(doc).await.expect("详情");
    assert_eq!(detail.stock_warnings.len(), 1);
    assert!(
        detail.stock_warnings[0].contains("无库存"),
        "提示要说清现在一点货都没有：{:?}",
        detail.stock_warnings
    );

    let error = stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect_err("没货必须 409");
    assert_eq!(error.status, 409);
    assert!(
        error.msg.contains("可用合计 0") && error.msg.contains("差 5"),
        "409 要点名缺口与零库存：{}",
        error.msg
    );
}

// ============================================================================
// ⑨ 真库：未分配桶参与 FIFO + 多行同物料不重复分配
// ============================================================================

/// 「未分配」桶 10（早）+ A 20（晚），领 25 → 先扣未分配 10，再从 A 取 15。
#[tokio::test]
async fn fifo_null_bucket_participates() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    seed_bucket(None, "10", 120).await;
    let row_a = seed_bucket(Some(LOC_A), "20", 60).await;

    let doc = out_doc("25", None).await;
    let rows = detail_rows(&stockout_handler::detail_of(doc).await.expect("详情"));
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (1, None, dec("10")),
        "未分配桶也是桶，按建账时间参与 FIFO"
    );
    assert_eq!(rows[1], (2, Some(row_a), dec("15")));

    stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("过账");
    assert_eq!(qty_at(None).await, Decimal::ZERO);
    assert_eq!(qty_at(Some(LOC_A)).await, dec("5"));
}

/// 同一物料两行、合计超过单桶 —— 第 2 行不能把第 1 行已经预占的货重复分配。
#[tokio::test]
async fn fifo_multiple_lines_do_not_double_book() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row_a = seed_bucket(Some(LOC_A), "30", 60).await;

    let doc = out_doc_lines(vec![
        StockOutItemReq {
            material_id: MATERIAL.to_string(),
            qty: dec("20"),
            location_id: None,
            lot_no: None,
            remark: None,
        },
        StockOutItemReq {
            material_id: MATERIAL.to_string(),
            qty: dec("20"),
            location_id: None,
            lot_no: None,
            remark: None,
        },
    ])
    .await;
    let detail = stockout_handler::detail_of(doc).await.expect("详情");
    let rows = detail_rows(&detail);
    assert_eq!(rows.len(), 2, "第 1 行够量拆 1 行；第 2 行不够量保持单行");
    assert_eq!(rows[0], (1, Some(row_a), dec("20")), "第 1 行预占 20");
    assert_eq!(
        rows[1],
        (2, None, dec("20")),
        "第 2 行只剩 10，不预填（不能重复分配同一桶）"
    );
    assert_eq!(
        dec(&detail.header.total_qty),
        dec("40"),
        "用户要的合计不能被预分配改掉"
    );
}

/// 负库存仓：不够量也不预拆，过账按「允许负库存」放行（口径与改前一致）。
#[tokio::test]
async fn fifo_negative_warehouse_still_posts_short() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 负库存仓先放 5（早），领用 20：预填只取够的部分 → 不够量 → 保持单行 → 过账走负库存
    seed_bucket_in(WAREHOUSE_NEG, MATERIAL, Some(LOC_A), "5", 60).await;
    let req = CreateStockOutReq {
        out_type: Some("ISSUE".to_string()),
        warehouse_id: Some(WAREHOUSE_NEG.to_string()),
        out_date: None,
        reason: Some("P1-FIX-2 负库存仓".to_string()),
        remark: None,
        items: vec![StockOutItemReq {
            material_id: MATERIAL.to_string(),
            qty: dec("20"),
            location_id: None,
            lot_no: None,
            remark: None,
        }],
    };
    let doc = stockout_handler::create_stock_out(&req, None)
        .await
        .expect("负库存仓建单");
    stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("负库存仓按既定策略放行");
}

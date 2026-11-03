// ============================================================================
// stockout_tests.rs —— 出库单真库用例（`ERP_DB_URL` 开关，与其它 ERP 用例同口径）
//
// 覆盖 ARCH-3 DC-13 的验收四项：
//   ① 过账后 B1 / B2 / B4 / C1 全绿（三本账 + 财务账 + 流水）；
//   ② 负库存被拦（409 + 66034，报文点名物料与可用量），且整笔回滚；
//   ③ 作废后三本账回滚 + 凭证红冲（原凭证 reversed_by → 红字凭证）；
//   ④ 状态机（提交待过账 / 撤回 / 非法边 / 一物一码物料被拒）。
//
// ⚠ `ERP_DB_URL` 未设置时全部 SKIP（假绿）。交付前必须真跑：
//
// ```bash
// ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/<干净库>" \
//   cargo test --lib business::erp::stockout_model::tests
// ```
//
// # 为什么自己造入库而不是调 `create_stock_in`
//
// 收货过账在 DC-01 之后要过审批闸（`stock_in_post` 流程 / 显式跳过），
// 单测里没有 claims 可判权限。本模块的用例只验证**出库侧**的账务正确性，
// 所以用 [`seed_inbound`] 直接落「三本账 + 一张期初凭证」：
// 这是真实的期初入库形态（借 1405 / 贷 1901），B1/B2/B4/C1 的基线因此是真的平。
// 端到端（建收货单 → 过账 → 出库 → 巡检脚本）走隔离库 + HTTP，见
// `scripts/verify/dc13_stock_out_e2e.py`。
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::*;
use crate::business::erp::voucher;
use crate::business::shared::state_machine;

// ---------- 夹具 ----------

const WAREHOUSE: &str = "d2030100-0000-4000-8000-000000000002";
const WAREHOUSE_NEG: &str = "d2030100-0000-4000-8000-000000000003";
const MATERIAL_A: &str = "d2030100-0000-4000-8000-000000000011";
const MATERIAL_B: &str = "d2030100-0000-4000-8000-000000000012";
/// 一物一码物料（出库单必须拒绝它）。
const MATERIAL_SERIAL: &str = "d2030100-0000-4000-8000-000000000013";

const FIXTURES: &[&str] = &[
    "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order)
     VALUES ('d2030100-0000-4000-8000-000000000001', 'DC13-PCS', '个', 1, NULL, 1.000000, 1, 1)
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative)
     VALUES ('d2030100-0000-4000-8000-000000000002', 'DC13-WH', 'DC13 出库仓',
             '/d2030100-0000-4000-8000-000000000002/', 1, 1, 1, 0),
            ('d2030100-0000-4000-8000-000000000003', 'DC13-WHN', 'DC13 负库存仓',
             '/d2030100-0000-4000-8000-000000000003/', 1, 1, 1, 1)
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required)
     VALUES ('d2030100-0000-4000-8000-000000000011', 'DC13-M1', 'DC13 螺丝',
             'd2030100-0000-4000-8000-000000000001', 1, false),
            ('d2030100-0000-4000-8000-000000000012', 'DC13-M2', 'DC13 垫片',
             'd2030100-0000-4000-8000-000000000001', 1, false),
            ('d2030100-0000-4000-8000-000000000013', 'DC13-M3', 'DC13 一物一码件',
             'd2030100-0000-4000-8000-000000000001', 1, true)
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

fn uuid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID")
}

/// 清场（模块内所有用例前后都跑一遍，避免跨用例污染）。
async fn cleanup_fixtures() {
    let doc_ids = "SELECT id FROM biz_stock_out WHERE out_no LIKE 'OUT-%' AND create_by IS NULL OR out_no LIKE 'OUT-%'";
    // ① 状态日志（无外键，必须显式删；口径按 entity_type 收窄）
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_OUT' \
              AND entity_id NOT IN (SELECT id FROM biz_stock_out)"
                .to_string(),
            vec![],
        )
        .await;
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_OUT'".to_string(),
            vec![],
        )
        .await;
    // ② 凭证（先删红字，再删蓝字；分录由外键级联）
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND is_red = 1".to_string(),
            vec![],
        )
        .await;
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT'".to_string(),
            vec![],
        )
        .await;
    // 期初夹具凭证（`seed_inbound` 落的「借 1405 / 贷 1901」）：不删会让 1405 余额
    // 跨用例累积（实测：5 条残留 → B2 报 1030 的差，看起来像出库把账做坏了）
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_voucher WHERE source_type = 'SERIAL_BATCH' AND source_no = 'SEED-IN'"
                .to_string(),
            vec![],
        )
        .await;
    // ③ 库存流水（本模块的夹具单据 + 夹具物料）
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_inventory_txn WHERE material_id IN \
                ('d2030100-0000-4000-8000-000000000011'::uuid, \
                 'd2030100-0000-4000-8000-000000000012'::uuid, \
                 'd2030100-0000-4000-8000-000000000013'::uuid)"
                .to_string(),
            vec![],
        )
        .await;
    // ④ 单据（明细 ON DELETE CASCADE）
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_stock_out WHERE out_no LIKE 'OUT-%'".to_string(),
            vec![],
        )
        .await;
    // ⑤ 两本账
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_inventory WHERE material_id IN \
                ('d2030100-0000-4000-8000-000000000011'::uuid, \
                 'd2030100-0000-4000-8000-000000000012'::uuid, \
                 'd2030100-0000-4000-8000-000000000013'::uuid)"
                .to_string(),
            vec![],
        )
        .await;
    let _ = db()
        .exec_sql(
            "DELETE FROM biz_inventory_location WHERE material_id IN \
                ('d2030100-0000-4000-8000-000000000011'::uuid, \
                 'd2030100-0000-4000-8000-000000000012'::uuid, \
                 'd2030100-0000-4000-8000-000000000013'::uuid)"
                .to_string(),
            vec![],
        )
        .await;
    let _ = doc_ids;
}

/// 夹具准备：清场 + 落夹具主数据。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup_fixtures().await;
    for sql in FIXTURES {
        db().exec_sql(sql.to_string(), vec![])
            .await
            .unwrap_or_else(|error| panic!("夹具写入失败：{error}\n{sql}"));
    }
    Some(guard)
}

// ---------- 造入库（三本账 + 期初凭证） ----------

/// 直接落一笔「期初入库」：三本账 + 借 1405 / 贷 1901 的凭证。
///
/// 这样 B1（库位账 = 成本账）/ B2（库存金额 = 1405 余额）/ B4（流水 = 库存）
/// 的**基线**是真的平 —— 后续出库把这四条压红就一定是出库侧的问题。
async fn seed_inbound(material_id: Uuid, warehouse_id: Uuid, qty: Decimal, unit_cost: Decimal) {
    let now = Utc::now();
    let amount = (qty * unit_cost).round_dp(4);
    let applied = crate::business::erp::stockin_model::apply_cost(
        db(),
        material_id,
        warehouse_id,
        None,
        crate::business::erp::stockin_model::CostBasis::of(qty, amount),
        now,
    )
    .await
    .expect("期初入库：成本账");
    crate::business::erp::stockin_model::apply_location_qty(
        db(),
        material_id,
        warehouse_id,
        None,
        qty,
        now,
    )
    .await
    .expect("期初入库：库位账");
    crate::business::erp::stockin_model::write_txn(
        db(),
        material_id,
        warehouse_id,
        1,
        crate::business::erp::stockin_model::CostBasis::of(qty, amount),
        &applied,
        Uuid::now_v7(),
        "SEED-IN",
        None,
        now,
    )
    .await
    .expect("期初入库：流水");

    // 期初凭证：借 1405 / 贷 1901（与 serial 批量生成的期初口径一致）
    let inventory = voucher::account_required(db(), voucher::DEFAULT_INVENTORY_CODE, "库存商品")
        .await
        .expect("科目 1405 必须存在（V0137 种子）");
    let counterpart =
        voucher::account_required(db(), voucher::ACCOUNT_PENDING_GAIN_LOSS, "待处理财产损溢")
            .await
            .expect("科目 1901 必须存在（V0160 种子）");
    voucher::post_voucher(
        db(),
        &voucher::PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: voucher::SOURCE_SERIAL_BATCH,
            source_id: Some(Uuid::now_v7()),
            source_no: Some("SEED-IN"),
            voucher_date: voucher::today_cst(now),
            summary: "DC13 期初入库".to_string(),
            lines: vec![
                voucher::EntryLine {
                    line_no: 1,
                    summary: "DC13 期初入库".to_string(),
                    account: inventory,
                    debit: amount,
                    credit: Decimal::ZERO,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: Some(warehouse_id),
                },
                voucher::EntryLine {
                    line_no: 2,
                    summary: "DC13 期初入库".to_string(),
                    account: counterpart,
                    debit: Decimal::ZERO,
                    credit: amount,
                    aux_supplier_id: None,
                    aux_material_id: Some(material_id),
                    aux_warehouse_id: None,
                },
            ],
            remark: None,
            red_of: None,
        },
        None,
        now,
    )
    .await
    .expect("期初入库：凭证");
}

// ---------- 断言助手 ----------

/// `(qty, amount)` 按 (物料, 仓库) 汇总成本账。
async fn cost_ledger(material_id: Uuid, warehouse_id: Uuid) -> (Decimal, Decimal) {
    let row = db()
        .query_first_sql(
            "SELECT coalesce(sum(qty),0) AS qty, coalesce(sum(amount),0) AS amount \
               FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid"
                .to_string(),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await
        .expect("读成本账")
        .expect("聚合必有行");
    (
        row.get_typed("qty").expect("qty"),
        row.get_typed("amount").expect("amount"),
    )
}

/// 库位账合计。
async fn location_ledger(material_id: Uuid, warehouse_id: Uuid) -> Decimal {
    db().scalar_with_params(
        "SELECT coalesce(sum(qty),0) FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await
    .expect("读库位账")
}

/// 流水累计（IN 加、OUT 减）。
async fn txn_total(material_id: Uuid, warehouse_id: Uuid) -> Decimal {
    db().scalar_with_params(
        "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END),0) \
           FROM biz_inventory_txn WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await
    .expect("读流水")
}

/// 1405 科目余额（Σ借 − Σ贷）。仅用于人工排查，保留作诊断工具。
#[allow(dead_code)]
async fn inventory_account_balance() -> Decimal {
    db().scalar_sql(
        "SELECT coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)),0) \
           FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL \
           JOIN biz_account a ON a.id = e.account_id \
          WHERE a.account_code LIKE '1405%'"
            .to_string(),
        vec![],
    )
    .await
    .expect("读 1405 余额")
}

/// 库存总金额（全部物料）。仅用于人工排查，保留作诊断工具。
#[allow(dead_code)]
async fn inventory_total_amount() -> Decimal {
    db().scalar_sql(
        "SELECT coalesce(sum(amount),0) FROM biz_inventory".to_string(),
        vec![],
    )
    .await
    .expect("读库存金额")
}

/// B1 / B4 / C1 三条不变式（与 `erp_invariants.py` 同口径；B2 单独判，因为它受全局库存影响）。
async fn assert_ledgers_green(material_id: Uuid, warehouse_id: Uuid, ctx: &str) {
    let (cost_qty, _cost_amount) = cost_ledger(material_id, warehouse_id).await;
    let loc = location_ledger(material_id, warehouse_id).await;
    let txn = txn_total(material_id, warehouse_id).await;
    assert_eq!(
        cost_qty.round_dp(4),
        loc.round_dp(4),
        "[{ctx}] B1 违约：成本账 {cost_qty} ≠ 库位账 {loc}"
    );
    assert_eq!(
        cost_qty.round_dp(4),
        txn.round_dp(4),
        "[{ctx}] B4 违约：成本账 {cost_qty} ≠ 流水累计 {txn}"
    );
    assert!(
        cost_qty >= Decimal::ZERO,
        "[{ctx}] C1 违约：库存为负 {cost_qty}"
    );
}

/// B2：Σ 库存金额 = 1405 余额（容差 0.0001，与巡检 TOL 一致）。
///
/// ⚠️ **按物料收窄**（`aux_material_id`）：巡检口径是「全库 Σ库存金额 = 1405 余额」，
/// 但真库用例与其它模块共用同一个库，别的模块留下的库存 / 期初凭证会让**全局** B2
/// 永远对不上（实测全量套件里报「库存 1440 ≠ 1405 余额 1000，差 440」，而那 440 正是本用例自己的）。
/// 收窄到「本夹具物料的库存金额 vs aux_material 指向它的 1405 分录」后，
/// 判据仍然咬住「出库结转金额是否等于账面金额减少量」这件事本身。
async fn assert_b2_green(material_id: Uuid, ctx: &str) {
    let inv: Decimal = db()
        .scalar_with_params(
            "SELECT coalesce(sum(amount),0) FROM biz_inventory WHERE material_id = ?::uuid",
            vec![Value::from(&material_id)],
        )
        .await
        .expect("读该物料的库存金额");
    let gl: Decimal = db()
        .scalar_with_params(
            "SELECT coalesce(sum(coalesce(e.debit_amount,0) - coalesce(e.credit_amount,0)),0) \
               FROM biz_voucher_entry e \
               JOIN biz_voucher v ON v.id = e.voucher_id AND v.deleted_at IS NULL \
               JOIN biz_account a ON a.id = e.account_id \
              WHERE a.account_code LIKE '1405%' AND e.aux_material_id = ?::uuid",
            vec![Value::from(&material_id)],
        )
        .await
        .expect("读该物料的 1405 余额");
    let diff = (inv - gl).abs();
    assert!(
        diff <= Decimal::new(1, 4),
        "[{ctx}] B2 违约：该物料库存金额 {inv} ≠ 1405 余额 {gl}（差 {diff}）"
    );
}

/// 造一张出库单（不走 HTTP）。
async fn make_doc(
    out_type: &str,
    warehouse_id: Uuid,
    material_id: Uuid,
    qty: Decimal,
    reason: &str,
) -> Uuid {
    let req = super::super::stockout_dto::CreateStockOutReq {
        out_type: Some(out_type.to_string()),
        warehouse_id: Some(warehouse_id.to_string()),
        out_date: None,
        reason: Some(reason.to_string()),
        remark: None,
        items: vec![super::super::stockout_dto::StockOutItemReq {
            material_id: material_id.to_string(),
            qty,
            location_id: None,
            lot_no: None,
            remark: None,
        }],
    };
    super::super::stockout_handler::create_stock_out(&req, None)
        .await
        .unwrap_or_else(|error| panic!("建出库单失败：{} {}", error.code, error.msg))
}

// ==================== ① 全链：入库 100 → 领用 30 → 报废 10 → 盘亏 5 ====================

/// 过账三张单后：库存 55 / 金额 550，B1 · B2 · B4 · C1 全绿，
/// 每张单各一张成本结转凭证（借 6401 / 1901，贷 1405）。
#[tokio::test]
async fn dc13_full_chain_keeps_six_ledgers_green() {
    let Some(_guard) = setup().await else { return };
    let material = uuid(MATERIAL_A);
    let warehouse = uuid(WAREHOUSE);
    seed_inbound(material, warehouse, Decimal::from(100), Decimal::from(10)).await;
    assert_b2_green(material, "期初基线").await;

    // 领用 30 → 借 6401
    let issue = make_doc("ISSUE", warehouse, material, Decimal::from(30), "车间领用").await;
    super::super::stockout_handler::post_stock_out(issue, None, None)
        .await
        .expect("领用过账");
    // 报废 10 → 借 1901
    let scrap = make_doc("SCRAP", warehouse, material, Decimal::from(10), "锈蚀报废").await;
    super::super::stockout_handler::post_stock_out(scrap, None, None)
        .await
        .expect("报废过账");
    // 盘亏 5 → 借 1901
    let loss = make_doc("LOSS", warehouse, material, Decimal::from(5), "盘点短少").await;
    super::super::stockout_handler::post_stock_out(loss, None, None)
        .await
        .expect("盘亏过账");

    let (qty, amount) = cost_ledger(material, warehouse).await;
    assert_eq!(
        qty.round_dp(4),
        Decimal::from(55),
        "账面数量应为 100-30-10-5=55"
    );
    assert_eq!(
        amount.round_dp(4),
        Decimal::from(550),
        "账面金额应为 1000-300-100-50=550（移动加权单价 10）"
    );
    assert_ledgers_green(material, warehouse, "全链").await;
    assert_b2_green(material, "全链").await;

    // 三张单各一张凭证，且状态都是 POSTED、金额与明细一致
    for (id, expect_debit_code) in [(issue, "6401"), (scrap, "1901"), (loss, "1901")] {
        let row = super::find(id).await.expect("查单").expect("单据存在");
        assert_eq!(row.status, STATUS_POSTED);
        let voucher_id = row.voucher_id.expect("已过账必须有凭证（差额 > 0）");
        let entries = voucher::entries_of(db(), voucher_id).await.expect("读分录");
        assert_eq!(entries.len(), 2, "成本结转凭证两条分录");
        let debit = entries
            .iter()
            .find(|entry| entry.debit > Decimal::ZERO)
            .expect("必须有借方行");
        assert_eq!(
            debit.account.code, expect_debit_code,
            "借方科目应为 {expect_debit_code}"
        );
        assert_eq!(
            debit.debit.round_dp(4),
            row.total_amount.round_dp(4),
            "凭证金额必须等于单头金额（账上真实差额）"
        );
        let credit = entries
            .iter()
            .find(|entry| entry.credit > Decimal::ZERO)
            .expect("必须有贷方行");
        assert_eq!(credit.account.code, "1405", "贷方必须是 1405 库存商品");
    }
    println!("[DC-13] 全链通过：库存 55 / 金额 550 / B1·B2·B4·C1 全绿");
}

// ==================== ② 负库存被拦 ====================

/// 出库量 > 账面可用量且仓库不允许负库存 → 409 + 66034，报文点物料与可用量，
/// **且整笔回滚**（单据仍是草稿、账一分没动）。
#[tokio::test]
async fn dc13_negative_stock_is_blocked_and_rolled_back() {
    let Some(_guard) = setup().await else { return };
    let material = uuid(MATERIAL_B);
    let warehouse = uuid(WAREHOUSE);
    seed_inbound(material, warehouse, Decimal::from(20), Decimal::from(5)).await;
    let (before_qty, before_amount) = cost_ledger(material, warehouse).await;

    let doc = make_doc("ISSUE", warehouse, material, Decimal::from(50), "超额领用").await;
    let error = super::super::stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect_err("超量出库必须被拦");
    assert_eq!(error.status, 409, "负库存拦截必须是 409");
    assert_eq!(
        error.code,
        crate::shared::error_code::ERP_STOCK_OUT_NEGATIVE,
        "必须用 66034 而不是通用冲突码"
    );
    assert!(
        error.msg.contains("DC13-M2"),
        "报文必须点名物料编码：{}",
        error.msg
    );
    assert!(
        error.msg.contains("20"),
        "报文必须给出可用量（20）：{}",
        error.msg
    );

    // 整笔回滚：单据仍是草稿，账没动，也没有流水
    let row = super::find(doc).await.expect("查单").expect("单据存在");
    assert_eq!(row.status, STATUS_DRAFT, "过账失败后单据必须还是草稿");
    let (after_qty, after_amount) = cost_ledger(material, warehouse).await;
    assert_eq!(after_qty, before_qty, "失败不得改动账面数量");
    assert_eq!(after_amount, before_amount, "失败不得改动账面金额");
    assert_eq!(
        txn_total(material, warehouse).await,
        before_qty,
        "失败不得写流水"
    );
    assert!(row.voucher_id.is_none(), "失败不得留半张凭证");
    println!("[DC-13] 负库存拦截通过：409/66034 + 整笔回滚");
}

// ==================== ②b 允许负库存的仓库：放行，金额只减到 0 ====================

/// `biz_warehouse.allow_negative = 1` 时超量出库放行：数量转负、金额夹到 0
/// （不能把 amount 减成负数：`ck_biz_inv_txn_amt` 与巡检 C1 都会红）。
#[tokio::test]
async fn dc13_allow_negative_warehouse_lets_qty_go_negative_but_amount_clamps() {
    let Some(_guard) = setup().await else { return };
    let material = uuid(MATERIAL_B);
    let warehouse = uuid(WAREHOUSE_NEG);
    seed_inbound(material, warehouse, Decimal::from(10), Decimal::from(3)).await;

    let doc = make_doc(
        "OTHER",
        warehouse,
        material,
        Decimal::from(15),
        "负库存仓放行",
    )
    .await;
    super::super::stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("允许负库存的仓库必须放行");

    let (qty, amount) = cost_ledger(material, warehouse).await;
    assert_eq!(qty.round_dp(4), Decimal::from(-5), "允许负库存 → 数量 -5");
    assert_eq!(
        amount.round_dp(4),
        Decimal::ZERO,
        "金额夹到 0（超出的部分没有成本可结转）"
    );
    // 三本账仍同步（B1/B4 不因负库存而失衡）
    assert_eq!(
        location_ledger(material, warehouse).await.round_dp(4),
        qty.round_dp(4),
        "B1：库位账同步转负"
    );
    assert_eq!(
        txn_total(material, warehouse).await.round_dp(4),
        qty.round_dp(4),
        "B4：流水累计同步转负"
    );
    assert_b2_green(material, "负库存仓").await;
    println!("[DC-13] 负库存仓放行：qty -5 / amount 0 / B1·B2·B4 仍平");
}

// ==================== ③ 作废：三本账回滚 + 凭证红冲 ====================
/// 过账后作废：数量与金额原路退回、写一条 IN 冲销流水、原凭证被红冲（`reversed_by` → 红字）。
#[tokio::test]
async fn dc13_cancel_restores_ledgers_and_reverses_voucher() {
    let Some(_guard) = setup().await else { return };
    let material = uuid(MATERIAL_A);
    let warehouse = uuid(WAREHOUSE);
    seed_inbound(material, warehouse, Decimal::from(100), Decimal::from(10)).await;

    let doc = make_doc(
        "ISSUE",
        warehouse,
        material,
        Decimal::from(40),
        "领用后撤回",
    )
    .await;
    super::super::stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("过账");
    let posted = super::find(doc).await.expect("查单").expect("单据存在");
    let blue_voucher = posted.voucher_id.expect("过账必有蓝字凭证");
    assert_eq!(
        cost_ledger(material, warehouse).await.0,
        Decimal::from(60),
        "过账后 60"
    );

    // 作废必须写原因（空原因 → 400，不落库）
    let empty = super::super::stockout_handler::cancel_stock_out(doc, "   ", None).await;
    let error = empty.expect_err("空原因必须 400");
    assert_eq!(error.status, 400);

    super::super::stockout_handler::cancel_stock_out(doc, "领错料，退回", None)
        .await
        .expect("作废");

    let cancelled = super::find(doc).await.expect("查单").expect("单据存在");
    assert_eq!(cancelled.status, STATUS_CANCELLED);
    assert_eq!(cancelled.cancel_reason.as_deref(), Some("领错料，退回"));
    assert_eq!(
        cost_ledger(material, warehouse).await,
        (Decimal::from(100), Decimal::from(1000)),
        "作废必须把数量与金额原路退回"
    );
    assert_ledgers_green(material, warehouse, "作废后").await;
    assert_b2_green(material, "作废后").await;

    // 红字凭证：原凭证 reversed_by 指向红字，红字凭证 is_red = 1 且分录借贷互换
    let original = voucher::find_voucher(db(), blue_voucher)
        .await
        .expect("读原凭证")
        .expect("原凭证存在");
    let red_id = original.reversed_by.expect("原凭证必须被标记已红冲");
    let red = voucher::find_voucher(db(), red_id)
        .await
        .expect("读红字凭证")
        .expect("红字凭证存在");
    assert_eq!(red.is_red, 1, "红字凭证 is_red = 1");
    let red_entries = voucher::entries_of(db(), red_id).await.expect("读红字分录");
    let red_debit: Decimal = red_entries
        .iter()
        .find(|entry| entry.debit > Decimal::ZERO)
        .map(|entry| entry.debit)
        .expect("红字有借方行");
    assert_eq!(
        red_debit.round_dp(4),
        posted.total_amount.round_dp(4),
        "红冲金额等于原凭证金额"
    );
    // 单据 voucher_id 指向红字凭证（详情页能一跳看到冲销）
    assert_eq!(cancelled.voucher_id, Some(red_id));
    println!("[DC-13] 作废通过：三本账回滚 + 红字凭证 {red_id}");
}

// ==================== ④ 状态机 / 一物一码 / 权限码 ====================

/// 提交待过账 → 撤回；非法边（已过账再过账）409；一物一码物料建单 400。
#[tokio::test]
async fn dc13_state_machine_and_guards() {
    let Some(_guard) = setup().await else { return };
    let material = uuid(MATERIAL_A);
    let warehouse = uuid(WAREHOUSE);
    seed_inbound(material, warehouse, Decimal::from(50), Decimal::from(2)).await;

    let doc = make_doc("ISSUE", warehouse, material, Decimal::from(10), "交接测试").await;

    // ① 提交待过账（账没动）
    let req = super::super::stockout_dto::StockOutStatusReq {
        status: Some("PENDING_POST".to_string()),
        target_status: None,
        reason: Some("提交给仓管过账".to_string()),
    };
    super::super::stockout_handler::change_status(doc, &req, None)
        .await
        .expect("提交待过账");
    let (qty, _) = cost_ledger(material, warehouse).await;
    assert_eq!(qty, Decimal::from(50), "提交待过账不得动账");

    // ② 撤回（PENDING_POST → DRAFT）
    let back = super::super::stockout_dto::StockOutStatusReq {
        status: Some("DRAFT".to_string()),
        target_status: None,
        reason: None,
    };
    super::super::stockout_handler::change_status(doc, &back, None)
        .await
        .expect("撤回");
    assert_eq!(
        super::find(doc).await.expect("查单").expect("单据").status,
        STATUS_DRAFT
    );

    // ③ 非法边：已作废的单不能再过账（409）
    super::super::stockout_handler::cancel_stock_out(doc, "不领了", None)
        .await
        .expect("草稿作废");
    let again = super::super::stockout_handler::post_stock_out(doc, None, None).await;
    let error = again.expect_err("已作废不能再过账");
    assert_eq!(error.status, 409);

    // ④ 状态机纯逻辑：边表与文案
    assert!(super::super::stockout_base::can_transition(
        "DRAFT", "POSTED"
    ));
    assert!(!super::super::stockout_base::can_transition(
        "CANCELLED",
        "POSTED"
    ));
    let message = super::super::stockout_base::transition_rejected_message("CANCELLED", "POSTED");
    assert!(message.contains("终态"), "终态文案：{message}");

    // ⑤ 一物一码物料：建单 400（账面件数只由序列号维护）
    let req = super::super::stockout_dto::CreateStockOutReq {
        out_type: Some("ISSUE".to_string()),
        warehouse_id: Some(warehouse.to_string()),
        out_date: None,
        reason: Some("一物一码".to_string()),
        remark: None,
        items: vec![super::super::stockout_dto::StockOutItemReq {
            material_id: MATERIAL_SERIAL.to_string(),
            qty: Decimal::from(1),
            location_id: None,
            lot_no: None,
            remark: None,
        }],
    };
    let error = super::super::stockout_handler::create_stock_out(&req, None)
        .await
        .expect_err("一物一码物料必须被拒");
    assert_eq!(error.status, 400);
    assert!(
        error.msg.contains("一物一码") && error.msg.contains("序列号"),
        "报文要指出改走序列号出库：{}",
        error.msg
    );

    // ⑥ 报废未写原因：400
    let req = super::super::stockout_dto::CreateStockOutReq {
        out_type: Some("SCRAP".to_string()),
        warehouse_id: Some(warehouse.to_string()),
        out_date: None,
        reason: None,
        remark: None,
        items: vec![super::super::stockout_dto::StockOutItemReq {
            material_id: MATERIAL_A.to_string(),
            qty: Decimal::from(1),
            location_id: None,
            lot_no: None,
            remark: None,
        }],
    };
    let error = super::super::stockout_handler::create_stock_out(&req, None)
        .await
        .expect_err("报废必须写原因");
    assert_eq!(error.status, 400);
    assert!(
        error.msg.contains("1901"),
        "文案要点出挂 1901：{}",
        error.msg
    );
}

/// 权限码与迁移逐字一致（注解写错一个字母就是 403，且只有非超管会撞上）。
#[test]
fn dc13_permission_codes_match_migration() {
    let sql = include_str!("../../../migrations/business/V0203__erp_stock_out.up.sql");
    for (code, label) in super::super::stockout_dto::STOCK_OUT_PERMISSIONS {
        assert!(
            sql.contains(code),
            "权限码 `{code}`（{label}）必须出现在 V0203 迁移里，否则非超管调用会 403"
        );
        assert!(
            code.starts_with("business:erp:stock-out:"),
            "权限码前缀必须是 business:erp:stock-out:*：{code}"
        );
    }
    assert_eq!(
        super::super::stockout_dto::STOCK_OUT_PERMISSIONS.len(),
        6,
        "6 个码：list / detail / create / update / delete / status"
    );
    // 表名与 DDL 一致
    assert_eq!(super::TABLE, "biz_stock_out");
    assert_eq!(super::ITEM_TABLE, "biz_stock_out_item");
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS biz_stock_out ("));
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS biz_stock_out_item ("));
    // 出库类型四值
    for value in super::OUT_TYPES {
        assert!(
            sql.contains(&format!("'{value}'")),
            "out_type `{value}` 必须在 DDL 的 CHECK 里"
        );
    }
}

/// 模块收尾：清场后必须零残留（`zz_` 前缀保证排在本模块最后）。
#[tokio::test]
async fn zz_dc13_fixtures_are_cleaned() {
    let Some(_guard) = setup().await else { return };
    cleanup_fixtures().await;
    let docs: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM biz_stock_out WHERE out_no LIKE 'OUT-%'".to_string(),
            vec![],
        )
        .await
        .expect("数单据");
    let logs: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM biz_status_log WHERE entity_type = 'STOCK_OUT'".to_string(),
            vec![],
        )
        .await
        .expect("数日志");
    let txns: i64 = db()
        .scalar_sql(
            "SELECT count(*) FROM biz_inventory_txn WHERE material_id IN \
                ('d2030100-0000-4000-8000-000000000011'::uuid, \
                 'd2030100-0000-4000-8000-000000000012'::uuid, \
                 'd2030100-0000-4000-8000-000000000013'::uuid)"
                .to_string(),
            vec![],
        )
        .await
        .expect("数流水");
    assert_eq!(docs, 0, "出库单残留 {docs} 张");
    assert_eq!(logs, 0, "状态日志残留 {logs} 条");
    assert_eq!(txns, 0, "库存流水残留 {txns} 条");
    let _ = state_machine::ENTITY_STOCK_OUT;
}

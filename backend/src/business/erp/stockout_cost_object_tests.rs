//! 领料单挂成本对象（ARCH-9 B1-10）的真库用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 **SKIP**（与本仓其它 ERP 真库用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/<已迁移到 V0381 的库> \
//!   cargo test --lib cost_object_tests -- --test-threads=1
//! ```
//!
//! # 覆盖的验收项（卡面三条逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | [`create_with_open_cost_object_reports_production_debit`] | 挂 OPEN 对象 ⇒ 落库 + 详情回显对象号 + **借方科目 4001.01** |
//! | 2 | [`create_with_closed_or_cancelled_object_is_rejected`] | 挂 CLOSED / CANCELLED ⇒ 409 + 66057，且**一行都没落库** |
//! | 3 | [`non_issue_doc_cannot_attach_object`] | 报废单挂对象 ⇒ 400（资产损失不进 4001.01） |
//! | 4 | [`update_sets_and_clears_cost_object`] | 改单三态：设值 / 清空 |
//! | 5 | [`posting_splits_debit_by_cost_object`] | 过账实证：挂对象借 4001.01，不挂仍借 6401，贷都是 1405 |
//! | 6 | [`posting_refuses_when_object_closed_after_draft`] | 过账兜底：草稿期间对象被关批 ⇒ 409 + 66057，单仍是 DRAFT、账一分没动 |
//!
//! # B4-3 追加：一键领料预填（`cost_object_issue_prefill`）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 9 | [`issue_prefill_without_history_returns_object_and_workshop_only`] | 无历史：对象号 / 车间 / 事由齐，物料范围为空（不凭空编料） |
//! | 10 | [`issue_prefill_reuses_last_issue_materials_and_warehouse`] | 有历史：物料 + 上次数量 + 上次领料仓库 |
//!
//! # 夹具纪律
//!
//! 全部用 `d2b10c00-*` 固定 UUID（与 `d2030100-*` / `d2f1f200-*` 两套既有夹具不重叠），
//! 清场按**本模块的物料 / 仓库 / 成本对象**收窄，绝不按前缀扫全库。
//!
//! # 依赖的库对象
//!
//! `biz_stock_out.cost_object_id`（V0375）、`biz_cost_object`（V0370）、
//! 末级科目 `4001.01`（V0377）。**库必须迁移到 V0377 之后**，否则用例会在
//! 「科目 4001.01 不存在」处失败 —— 那是环境问题，不是代码问题（报错信息里会带这句话）。

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::*;
use crate::business::erp::stockout_dto::{CreateStockOutReq, IssuePrefillQuery, StockOutItemReq};
use crate::business::erp::stockout_handler;
use crate::business::erp::stockout_model;
use crate::business::erp::voucher;
use crate::shared::error_code;

// ---------- 夹具（固定 UUID，每轮 setup 重建） ----------

const WAREHOUSE: &str = "d2b10c00-0000-4000-8000-000000000002";
const MATERIAL: &str = "d2b10c00-0000-4000-8000-000000000011";
const DEPT: &str = "d2b10c00-0000-4000-8000-0000000000d1";
const PRODUCT: &str = "d2b10c00-0000-4000-8000-000000000021";
const OBJECT_OPEN: &str = "d2b10c00-0000-4000-8000-0000000000a1";
const OBJECT_CLOSED: &str = "d2b10c00-0000-4000-8000-0000000000a2";
const OBJECT_CANCELLED: &str = "d2b10c00-0000-4000-8000-0000000000a3";

/// 本模块物料在流水 / 库位账里的收窄条件（清场用）。
const PINNED_MATERIALS: &str = "('d2b10c00-0000-4000-8000-000000000011'::uuid, \
                                'd2b10c00-0000-4000-8000-000000000021'::uuid)";

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

/// 成本对象夹具的 SQL（`status` 由用例给；CLOSED 必须带 `closed_at`，
/// 否则撞 `ck_biz_cost_object_closed`）。
fn object_insert(id: &str, no: &str, status: &str) -> String {
    let closed_at = if status == "CLOSED" { "now()" } else { "NULL" };
    format!(
        "INSERT INTO biz_cost_object \
            (id, object_no, object_type, product_id, lot_no, target_warehouse_id, plan_qty, \
             workshop_dept_id, workshop_dept_name, start_date, status, closed_at, created_at, updated_at) \
         VALUES ('{id}'::uuid, '{no}', 'BATCH', '{PRODUCT}'::uuid, '{no}', '{WAREHOUSE}'::uuid, 100, \
                 '{DEPT}'::uuid, 'B1-10 车间', '2026-11-01', '{status}', {closed_at}, now(), now()) \
         ON CONFLICT (id) DO NOTHING"
    )
}

/// 清场：只清本模块夹具的账与单据（按物料 / 固定 id 收窄）。
async fn cleanup_fixtures() {
    sql(&format!(
        "DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN \
            (SELECT stock_out_id FROM biz_stock_out_item WHERE material_id IN {PINNED_MATERIALS})"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_status_log WHERE entity_type = 'STOCK_OUT' AND entity_id IN \
            (SELECT stock_out_id FROM biz_stock_out_item WHERE material_id IN {PINNED_MATERIALS})"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_stock_out WHERE id IN \
            (SELECT stock_out_id FROM biz_stock_out_item WHERE material_id IN {PINNED_MATERIALS})"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_inventory_txn WHERE material_id IN {PINNED_MATERIALS}"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_inventory WHERE material_id IN {PINNED_MATERIALS}"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_inventory_location WHERE material_id IN {PINNED_MATERIALS}"
    ))
    .await;
    for object in [OBJECT_OPEN, OBJECT_CLOSED, OBJECT_CANCELLED] {
        sql(&format!("DELETE FROM biz_cost_object WHERE id = '{object}'::uuid")).await;
    }
    sql(&format!("DELETE FROM biz_material WHERE id = '{PRODUCT}'::uuid")).await;
}

const FIXTURES: &[&str] = &[
    "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
     VALUES ('d2b10c00-0000-4000-8000-000000000001', 'B10-PCS', '个', 1, NULL, 1.000000, 1, 1) \
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative) \
     VALUES ('d2b10c00-0000-4000-8000-000000000002', 'B10-WH', 'B1-10 出库仓', \
             '/d2b10c00-0000-4000-8000-000000000002/', 1, 1, 1, 0) \
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
     VALUES ('d2b10c00-0000-4000-8000-000000000011', 'B10-M1', 'B1-10 钢板', \
             'd2b10c00-0000-4000-8000-000000000001', 1, false) \
     ON CONFLICT DO NOTHING",
    "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status, serial_required) \
     VALUES ('d2b10c00-0000-4000-8000-000000000021', 'B10-P1', 'B1-10 产成品', \
             'd2b10c00-0000-4000-8000-000000000001', 'FINISHED', 1, false) \
     ON CONFLICT DO NOTHING",
];

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup_fixtures().await;
    let dept = crate::shared::test_helpers::ensure_any_dept().await;
    // 夹具声明是常量（`&'static str`），车间 id 是运行时取的，所以这里补一条 UPDATE 把
    // 成本对象的车间指到真实存在的部门上（`workshop_dept_id` 没有物理外键，但与
    // `close_facts` 的车间口径一致更贴近真实数据）。
    for statement in FIXTURES {
        sql(statement).await;
    }
    sql(&object_insert(OBJECT_OPEN, "CB-B110-000001", "OPEN")).await;
    sql(&object_insert(OBJECT_CLOSED, "CB-B110-000002", "CLOSED")).await;
    sql(&object_insert(OBJECT_CANCELLED, "CB-B110-000003", "CANCELLED")).await;
    sql(&format!(
        "UPDATE biz_cost_object SET workshop_dept_id = '{dept}'::uuid \
          WHERE id IN ('{OBJECT_OPEN}'::uuid, '{OBJECT_CLOSED}'::uuid, '{OBJECT_CANCELLED}'::uuid)"
    ))
    .await;
    // 末级科目 4001.01 是 V0377 种的；没有它在这里就说清楚是环境问题
    let leaf: Option<String> = db()
        .scalar_with_params(
            "SELECT account_code FROM biz_account WHERE account_code = '4001.01' \
              AND is_leaf = 1 AND deleted_at IS NULL",
            vec![],
        )
        .await
        .expect("查 4001.01");
    assert!(
        leaf.is_some(),
        "库必须迁移到 V0377 之后（缺末级科目 4001.01）—— 这是环境问题，不是用例失败"
    );
    Some(guard)
}

// ---------- 造库存 / 取数 ----------

/// 往「未分配」库位桶里放一笔货（只做成本账 + 库位账，不出期初凭证 —— 本模块只验出库侧）。
async fn seed_inbound(qty: &str, unit_cost: &str) {
    let now = Utc::now();
    let material = uuid(MATERIAL);
    let warehouse = uuid(WAREHOUSE);
    let quantity = dec(qty);
    let amount = (quantity * dec(unit_cost)).round_dp(4);
    crate::business::erp::stockin_model::apply_cost(
        db(),
        material,
        warehouse,
        None,
        crate::business::erp::stockin_model::CostBasis::of(quantity, amount),
        now,
    )
    .await
    .expect("期初：成本账");
    crate::business::erp::stockin_model::apply_location_qty(db(), material, warehouse, None, quantity, now)
        .await
        .expect("期初：库位账");
}

/// 某物料在一张单上的库存账面数量（清场后为 0 / 不存在）。
async fn stock_qty() -> Decimal {
    db().scalar_with_params::<Option<Decimal>>(
        "SELECT qty FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&uuid(MATERIAL)), Value::from(&uuid(WAREHOUSE))],
    )
    .await
    .expect("查成本账")
    .unwrap_or(Decimal::ZERO)
}

/// 建一张出库单的请求（`out_type` / 成本对象由用例给）。
fn req(out_type: &str, cost_object_id: Option<&str>) -> CreateStockOutReq {
    CreateStockOutReq {
        out_type: Some(out_type.to_string()),
        warehouse_id: Some(WAREHOUSE.to_string()),
        out_date: None,
        reason: Some("B1-10 真库用例".to_string()),
        remark: None,
        cost_object_id: cost_object_id.map(str::to_string),
        items: vec![StockOutItemReq {
            material_id: MATERIAL.to_string(),
            qty: dec("10"),
            location_id: None,
            lot_no: None,
            remark: None,
        }],
    }
}

/// 库里这张单的成本对象列（直接读库，不信接口）。
async fn stored_cost_object(id: Uuid) -> Option<Uuid> {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT cost_object_id FROM biz_stock_out WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .expect("查单据成本对象")
}

async fn stock_out_count() -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_stock_out WHERE id IN \
            (SELECT stock_out_id FROM biz_stock_out_item WHERE material_id IN ('d2b10c00-0000-4000-8000-000000000011'::uuid)) \
           AND deleted_at IS NULL",
        vec![],
    )
    .await
    .expect("数单据")
}

// ==================== 用例 ====================

/// ① 挂 **OPEN** 对象建单：落库 + 详情回显对象号 + **借方科目 4001.01**。
#[tokio::test]
async fn create_with_open_cost_object_reports_production_debit() {
    let Some(_guard) = setup().await else { return };
    let id = stockout_handler::create_stock_out(&req("ISSUE", Some(OBJECT_OPEN)), None)
        .await
        .unwrap_or_else(|error| panic!("建单失败：{} {}", error.code, error.msg));

    assert_eq!(
        stored_cost_object(id).await,
        Some(uuid(OBJECT_OPEN)),
        "cost_object_id 必须落库"
    );

    let detail = stockout_handler::detail_of(id).await.expect("详情");
    assert_eq!(detail.header.cost_object_id.as_deref(), Some(OBJECT_OPEN));
    assert_eq!(
        detail.header.cost_object_no.as_deref(),
        Some("CB-B110-000001"),
        "详情必须回显成本对象号（约定 13：主数据关联实时取名）"
    );
    assert_eq!(
        detail.header.debit_account_code, "4001.01",
        "挂对象 ⇒ 借方 生产成本-直接材料"
    );
    assert_eq!(detail.header.debit_account_name, "生产成本-直接材料");
    println!("[B1-10] 挂 OPEN 对象建单 OK：借方 {}", detail.header.debit_account_code);
}

/// ② 挂 **CLOSED / CANCELLED** 对象：409 + 66057，且**一行都没落库**。
#[tokio::test]
async fn create_with_closed_or_cancelled_object_is_rejected() {
    let Some(_guard) = setup().await else { return };
    for (object, label) in [(OBJECT_CLOSED, "CLOSED"), (OBJECT_CANCELLED, "CANCELLED")] {
        let error = stockout_handler::create_stock_out(&req("ISSUE", Some(object)), None)
            .await
            .expect_err("非 OPEN 对象必须被拒");
        assert_eq!(error.status, 409, "{label}");
        assert_eq!(error.code, error_code::ERP_COST_OBJECT_NOT_OPEN, "{label}");
        assert!(error.msg.contains(label), "报文要点名状态：{}", error.msg);
        println!("[B1-10] {label} 对象被拦：{} {}", error.code, error.msg);
    }
    assert_eq!(stock_out_count().await, 0, "被拒的单据一行都不许落库");
}

/// ③ 报废单挂对象：400（资产损失走 1901，且料归集只扫 ISSUE）。
#[tokio::test]
async fn non_issue_doc_cannot_attach_object() {
    let Some(_guard) = setup().await else { return };
    let error = stockout_handler::create_stock_out(&req("SCRAP", Some(OBJECT_OPEN)), None)
        .await
        .expect_err("报废单挂对象必须被拒");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::PARAM_INVALID);
    assert_eq!(stock_out_count().await, 0);
    println!("[B1-10] 报废单挂对象被拦：{} {}", error.code, error.msg);
}

/// ④ 改单三态：不挂 → 挂上 → 清空（整单覆盖口径）。
#[tokio::test]
async fn update_sets_and_clears_cost_object() {
    let Some(_guard) = setup().await else { return };
    let id = stockout_handler::create_stock_out(&req("ISSUE", None), None)
        .await
        .expect("建单");
    assert_eq!(stored_cost_object(id).await, None, "默认不挂");

    stockout_handler::update_stock_out(id, &req("ISSUE", Some(OBJECT_OPEN)), None)
        .await
        .expect("改单挂对象");
    assert_eq!(stored_cost_object(id).await, Some(uuid(OBJECT_OPEN)));
    let detail = stockout_handler::detail_of(id).await.expect("详情");
    assert_eq!(detail.header.debit_account_code, "4001.01", "挂上之后借方跟着变");

    // 清空：不传 = 整单覆盖为 NULL（与 reason / remark 同口径）
    stockout_handler::update_stock_out(id, &req("ISSUE", None), None)
        .await
        .expect("改单清空");
    assert_eq!(stored_cost_object(id).await, None, "清空后必须回到 NULL");
    let detail = stockout_handler::detail_of(id).await.expect("详情");
    assert_eq!(detail.header.debit_account_code, "6401", "清空后借方回到原逻辑");
    println!("[B1-10] 改单三态 OK（不挂 → 挂 → 清空）");
}

/// ⑤ 过账实证：挂对象借 `4001.01`，不挂仍借 `6401`，贷方都是 `1405`。
///
/// 这是「复算」不止发生在 DTO 上的证据 —— 真实凭证分录就是这么落的。
#[tokio::test]
async fn posting_splits_debit_by_cost_object() {
    let Some(_guard) = setup().await else { return };
    seed_inbound("100", "10").await;

    let attached = stockout_handler::create_stock_out(&req("ISSUE", Some(OBJECT_OPEN)), None)
        .await
        .expect("建单（挂对象）");
    stockout_handler::post_stock_out(attached, None, None)
        .await
        .expect("过账（挂对象）");

    let plain = stockout_handler::create_stock_out(&req("ISSUE", None), None)
        .await
        .expect("建单（不挂）");
    stockout_handler::post_stock_out(plain, None, None)
        .await
        .expect("过账（不挂）");

    for (id, expect_debit, label) in [(attached, "4001.01", "挂对象"), (plain, "6401", "不挂")] {
        let row = stockout_model::find(id).await.expect("查单").expect("单据存在");
        assert_eq!(row.status, STATUS_POSTED, "{label}");
        let voucher_id = row.voucher_id.unwrap_or_else(|| panic!("{label}：过账必须出凭证"));
        let entries = voucher::entries_of(db(), voucher_id).await.expect("读分录");
        let debit = entries
            .iter()
            .find(|entry| entry.debit > Decimal::ZERO)
            .unwrap_or_else(|| panic!("{label}：必须有借方行"));
        assert_eq!(debit.account.code, expect_debit, "{label}：借方科目");
        assert_eq!(
            debit.debit.round_dp(4),
            row.total_amount.round_dp(4),
            "{label}：凭证金额 = 单头金额（账上真实差额）"
        );
        let credit = entries
            .iter()
            .find(|entry| entry.credit > Decimal::ZERO)
            .expect("必须有贷方行");
        assert_eq!(credit.account.code, "1405", "{label}：贷方必须是 1405 库存商品");
        println!("[B1-10] {label} 过账凭证：借 {expect_debit} / 贷 1405 {}", debit.debit);
    }
    // 两张单各出 10，共减 20
    assert_eq!(stock_qty().await.round_dp(4), dec("80"), "库存 100 − 10 − 10 = 80");
}

/// ⑥ 过账兜底：草稿期间对象被关批 ⇒ 409 + 66057，单仍是 DRAFT、账一分没动。
///
/// 为什么要这条：建单到过账之间可以隔几天，关批侧的前置 ④ 只在**关批时**检查，
/// 并发下过账必须先自己再看一眼，否则料会记到已结转的对象上。
#[tokio::test]
async fn posting_refuses_when_object_closed_after_draft() {
    let Some(_guard) = setup().await else { return };
    seed_inbound("50", "10").await;
    let id = stockout_handler::create_stock_out(&req("ISSUE", Some(OBJECT_OPEN)), None)
        .await
        .expect("建单");

    // 模拟「草稿放着的时候对象被关批」（真实路径：人工关批前置 ④ 已拦，这里是并发兜底）
    sql(&format!(
        "UPDATE biz_cost_object SET status = 'CLOSED', closed_at = now() \
          WHERE id = '{OBJECT_OPEN}'::uuid"
    ))
    .await;

    let error = stockout_handler::post_stock_out(id, None, None)
        .await
        .expect_err("对象已关批必须拒绝过账");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_COST_OBJECT_NOT_OPEN);

    let row = stockout_model::find(id).await.expect("查单").expect("单据存在");
    assert_eq!(row.status, STATUS_DRAFT, "被拒的过账必须整笔回滚（单仍是草稿）");
    assert_eq!(stock_qty().await.round_dp(4), dec("50"), "账一分没动");
    println!("[B1-10] 关批后过账被拦且回滚：{} {}", error.code, error.msg);
}

// ==================== B1-12：成本对象下拉（出库表单的选择器数据源） ====================

/// ⑦ 下拉只列 `OPEN`：CLOSED / CANCELLED 一律不出现，产品名 join 出来。
///
/// 为什么要有这条：前端把「只列 OPEN」当验收项，但那句话成不成立只看**这个接口**。
/// 如果在 SQL 里漏了 `status = 'OPEN'`，用户会在下拉里选中一个已关批的批，
/// 提交时才吃 409 + 66057 —— 白填一整张单。
#[tokio::test]
async fn options_list_only_open_objects_with_product_name() {
    let Some(_guard) = setup().await else { return };
    let options = stockout_handler::cost_object_options(&Default::default())
        .await
        .expect("下拉");

    let ids: Vec<&str> = options.iter().map(|item| item.id.as_str()).collect();
    assert!(ids.contains(&OBJECT_OPEN), "OPEN 对象必须在下拉里：{ids:?}");
    for closed in [OBJECT_CLOSED, OBJECT_CANCELLED] {
        assert!(!ids.contains(&closed), "非 OPEN 对象不许出现在下拉里：{ids:?}");
    }

    let hit = options.iter().find(|item| item.id == OBJECT_OPEN).expect("OPEN 对象");
    assert_eq!(hit.object_no, "CB-B110-000001");
    assert_eq!(hit.product_name.as_deref(), Some("B1-10 产成品"), "产品名 join");
    assert_eq!(hit.product_code.as_deref(), Some("B10-P1"));
    assert_eq!(hit.plan_qty.parse::<Decimal>().expect("计划数量"), dec("100"));
    assert_eq!(hit.start_date.as_deref(), Some("2026-11-01"));
    println!("[B1-12] 下拉 {} 条，只含 OPEN（{}）", options.len(), hit.object_no);
}

/// ⑧ 关键字命中对象号 / 产品编码 / 产品名称（三列任一即可），且仍只含 OPEN。
#[tokio::test]
async fn options_filter_by_keyword_on_object_no_and_product() {
    let Some(_guard) = setup().await else { return };

    for (keyword, label) in [
        ("CB-B110-000001", "对象号"),
        ("B10-P1", "产品编码"),
        ("产成品", "产品名称"),
    ] {
        let query = crate::business::erp::stockout_dto::CostObjectOptionQuery {
            keyword: Some(keyword.to_string()),
        };
        let options = stockout_handler::cost_object_options(&query)
            .await
            .unwrap_or_else(|error| panic!("下拉（{label}）失败：{} {}", error.code, error.msg));
        let ids: Vec<&str> = options.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![OBJECT_OPEN],
            "{label}「{keyword}」应只命中 OPEN 那条：{ids:?}"
        );
    }

    // 命中 CLOSED 对象号 → 空列表（关批的批不因为能被搜到就出现）
    let query = crate::business::erp::stockout_dto::CostObjectOptionQuery {
        keyword: Some("CB-B110-000002".to_string()),
    };
    let options = stockout_handler::cost_object_options(&query).await.expect("下拉");
    assert!(options.is_empty(), "已关批对象的号搜出来也必须是空：{options:?}");
    println!("[B1-12] 关键字三列命中 OK，关批对象搜不到");
}

// ==================== 一键领料预填（B4-3） ====================

fn prefill_query(object: &str) -> IssuePrefillQuery {
    IssuePrefillQuery {
        cost_object_id: Some(object.to_string()),
    }
}

/// ⑨ 没有历史领料：预填仍要给出成本对象 / 车间 / 事由，但物料范围为空。
///
/// 这条钉住的是「不要凭空编物料」——一期没有 BOM（§8.3 R-12），
/// 服务端只能把事实上领过的料摆出来。
#[tokio::test]
async fn issue_prefill_without_history_returns_object_and_workshop_only() {
    let Some(_guard) = setup().await else { return };
    let prefill = stockout_handler::cost_object_issue_prefill(&prefill_query(OBJECT_OPEN))
        .await
        .unwrap_or_else(|error| panic!("预填失败：{} {}", error.code, error.msg));

    assert_eq!(
        prefill.cost_object_id, OBJECT_OPEN,
        "成本对象必须回传（保存时靠它回挂）"
    );
    assert_eq!(prefill.object_no, "CB-B110-000001");
    assert_eq!(prefill.out_type, "ISSUE", "一键领料恒为领用单");
    assert_eq!(prefill.product_code.as_deref(), Some("B10-P1"));
    assert!(
        !prefill.workshop_dept_name.trim().is_empty(),
        "车间必须预填（出库单没有车间列）"
    );
    assert_eq!(prefill.suggested_warehouse_id, None, "没领过料就别猜仓库");
    assert!(
        prefill.materials.is_empty(),
        "没有历史 → 物料范围为空：{:?}",
        prefill.materials
    );
    assert!(!prefill.has_history);
    assert!(
        prefill.reason.contains("CB-B110-000001") && prefill.reason.contains("生产领料"),
        "事由要带上对象号：{}",
        prefill.reason
    );
    println!("[B4-3] 无历史预填 OK：{}", prefill.reason);
}

/// ⑩ 有过一次领料：物料范围 = 上次领过的物料 + 上次数量；仓库 = 上次领料的仓库。
#[tokio::test]
async fn issue_prefill_reuses_last_issue_materials_and_warehouse() {
    let Some(_guard) = setup().await else { return };
    seed_inbound("100", "10").await;
    // 先按「一键领料」的方式建一张挂对象的领用单（数量由 req() 给了 10）
    let first = stockout_handler::create_stock_out(&req("ISSUE", Some(OBJECT_OPEN)), None)
        .await
        .expect("建单");
    assert_eq!(stored_cost_object(first).await, Some(uuid(OBJECT_OPEN)));

    let prefill = stockout_handler::cost_object_issue_prefill(&prefill_query(OBJECT_OPEN))
        .await
        .expect("预填");

    assert!(prefill.has_history, "刚领过料，必须有历史");
    assert_eq!(prefill.materials.len(), 1, "只有一种物料：{:?}", prefill.materials);
    let line = &prefill.materials[0];
    assert_eq!(line.material_id, MATERIAL, "物料范围 = 上次领过的物料");
    assert_eq!(line.material_code.as_deref(), Some("B10-M1"), "物料编码要 join 出来");
    assert_eq!(line.qty, "10.0000", "数量照抄上一次（四位小数字符串）");
    assert_eq!(
        prefill.suggested_warehouse_id.as_deref(),
        Some(WAREHOUSE),
        "仓库 = 上次领料的仓库（不是成本对象的完工入库仓）"
    );
    assert_eq!(prefill.suggested_warehouse_name.as_deref(), Some("B1-10 出库仓"));
    println!(
        "[B4-3] 有历史预填 OK：{} 种物料，仓库 {:?}",
        prefill.materials.len(),
        prefill.suggested_warehouse_name
    );
}

/// ⑪ 已关批 / 已作废的对象点「领料」：当场 409 + 66057（不等填完单再拦）。
#[tokio::test]
async fn issue_prefill_of_non_open_object_is_rejected_with_409() {
    let Some(_guard) = setup().await else { return };
    for (object, label) in [(OBJECT_CLOSED, "CLOSED"), (OBJECT_CANCELLED, "CANCELLED")] {
        let error = stockout_handler::cost_object_issue_prefill(&prefill_query(object))
            .await
            .expect_err("非 OPEN 对象必须被拒");
        assert_eq!(error.status, 409, "{label}");
        assert_eq!(error.code, error_code::ERP_COST_OBJECT_NOT_OPEN, "{label}");
        assert!(error.msg.contains("CB-B110-00000"), "报文要点名对象：{}", error.msg);
        println!("[B4-3] {label} 对象领料被拦：{} {}", error.code, error.msg);
    }
    // 不存在的对象 → 400 + 66002（引用缺失，与「状态不对」分开）
    let error = stockout_handler::cost_object_issue_prefill(&prefill_query("d2b10c00-0000-4000-8000-0000000000ff"))
        .await
        .expect_err("不存在的对象必须被拒");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::ERP_REFERENCE_MISSING);
}

/// ⑫ 作废的领用单不进物料范围（那笔料没进车间），且缺参 400。
#[tokio::test]
async fn issue_prefill_ignores_cancelled_docs_and_requires_id() {
    let Some(_guard) = setup().await else { return };
    seed_inbound("100", "10").await;
    let id = stockout_handler::create_stock_out(&req("ISSUE", Some(OBJECT_OPEN)), None)
        .await
        .expect("建单");
    stockout_handler::change_status(
        id,
        &crate::business::erp::stockout_dto::StockOutStatusReq {
            status: Some("CANCELLED".to_string()),
            target_status: None,
            reason: Some("B4-3 用例：作废后不该再被预填".to_string()),
        },
        None,
    )
    .await
    .expect("作废");

    let prefill = stockout_handler::cost_object_issue_prefill(&prefill_query(OBJECT_OPEN))
        .await
        .expect("预填");
    assert!(
        prefill.materials.is_empty(),
        "作废单的物料不进物料范围：{:?}",
        prefill.materials
    );
    assert!(!prefill.has_history);

    let error = stockout_handler::cost_object_issue_prefill(&IssuePrefillQuery::default())
        .await
        .expect_err("缺参必须 400");
    assert_eq!(error.status, 400);
    println!("[B4-3] 作废单不参与物料范围 + 缺参 400 OK");
}

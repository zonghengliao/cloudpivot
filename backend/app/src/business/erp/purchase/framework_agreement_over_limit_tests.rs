// ============================================================================
// framework_agreement_over_limit_tests.rs —— 框架协议「超限反例」单测（M4-10）
//
// 卡：t_4da4c498（M4-10【M4·采购】框架协议：单测 · 含超限反例）
//
// # 不重复劳动：本卡为什么只写这 10 条
//
// M4-07（表结构）/ M4-08（订单引用）/ M4-09（执行量统计）三卡已各自带真库用例
// （`framework_agreement_db_tests` 14 条、`framework_agreement_ref_db_tests` 13 条、
// `framework_agreement_execution_db_tests` 12 条）与纯逻辑用例（model 7 条、ref 5 条、
// execution_dto 8 条）。本卡**不抄第二遍**，只补它们没覆盖到的「拒绝分支」——
// 卡面点名要求的「超限反例」，以及超限判定里最容易写错的三件事：
//
//   ① **绕限**：同一物料拆成多行，单行不超、合计超；
//   ② **判据来源**：准入必须看「现算 Σ」，不能看回写列（回写列清零仍要拦、虚高不许误拒）；
//   ③ **边界**：恰等于上限必须放行（明细 / 单头总量 / 单头总额三处），多 0.0001 也要拒。
//
// # 用例表（每条都对应 `framework_agreement_ref::resolve` 里的一段原话）
//
// | 用例 | 钉住的判据 / 反例 |
// |---|---|
// | `split_lines_on_same_material_cannot_bypass_cap` | 同一物料两行各 60（单行都 ≤ 100）合计 120 → 409/66098；改成 60+40=100 放行 |
// | `per_material_cap_rejects_fractional_overrun` | 已满 100 再下 0.0001 也要拒（`>` 不是 `>=`；报文「剩余 0」） |
// | `unlimited_material_is_not_capped` | `max_qty IS NULL` = 不限：123456 照下，回写如实 |
// | `stale_booked_column_does_not_open_the_gate` | 回写列漏刷清零仍拦（准入看现算 100，报文「已用 100」） |
// | `inflated_booked_column_does_not_false_reject` | 回写列虚高 999999 不误拒，且下单后重算自愈回 100 |
// | `soft_deleted_agreement_is_rejected` | 协议软删 / 协议不存在 → 400/66096（报文给「重新选择」） |
// | `currency_mismatch_is_rejected` | 协议价币种 USD ↔ 本单 CNY → 400/66096（跨币种不比对协议价） |
// | `tax_inclusive_total_amount_cap_counts_with_tax` | 含税协议的总额上限按**价税合计**比：10×10（不含税 100 / 价税合计 113）超 100 → 409；8 件 90.4 放行 |
// | `update_does_not_double_count_own_usage` | 改单必须排除本单旧明细（100 原样改回不许算成 200 误拒）；改到 120 照拒且额度不动 |
// | `head_total_qty_cap_allows_exact_boundary` | 单头总量：30+20=50 恰等于上限放行，+1 拒绝 |
// | `head_total_amount_cap_allows_exact_boundary` | 单头总额：10×10=100 恰等于上限放行，再下 20 拒绝 |
// | `zz_teardown_purges_fixtures` | 清场：`M410-` 夹具 0 残留 |
//
// # 夹具
//
// 协议两表仍**没有 CRUD 接口**（计划里没有那张卡），所以协议夹具走裸 SQL；
// 采购单走**真实入口** `purchase_handler::create_purchase_order` /
// `update_purchase_order`（超限判据就在这条路径上）。主数据一律 `crate::test_seed::ensure_*`。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m410_ut" cargo test --lib \
//   business::erp::purchase::framework_agreement_over_limit -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::framework_agreement_model as fwa;
use crate::business::erp::purchase::framework_agreement_ref as fwa_ref;
use crate::business::erp::purchase::purchase_dto::{CreatePurchaseOrderReq, PurchaseOrderItemReq};
use crate::business::erp::purchase::purchase_handler;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具名前缀（清场按它反查，绝不按生产号段删）。
const PREFIX: &str = "M410-";
/// 夹具供应商编码。
const SUP_CODE: &str = "M410-SUP";
/// 夹具物料编码（协议内，明细上限 100）。
const MAT_A_CODE: &str = "M410-MATA";
/// 夹具物料编码（协议内，**不限量**）。
const MAT_B_CODE: &str = "M410-MATB";

// ---------- 协议夹具（固定 UUID；setup() 每次重建）----------
/// 主协议：10.00 不含税 / 5% 容差；物料 A 上限 100、物料 B 不限量；单头无封顶。
const AG_MAIN: &str = "00000000-0000-4000-8000-000004100001";
/// 含税协议 + 单头总额封顶 100（**含税口径**：比价税合计）。
const AG_TAX_AMOUNT: &str = "00000000-0000-4000-8000-000004100002";
/// 外币协议（USD）—— 本单恒按 CNY 结算，用来验币种不符被拒。
const AG_USD: &str = "00000000-0000-4000-8000-000004100003";
/// 已软删协议（`deleted_at` 非空 = 不能再被引用）。
const AG_DELETED: &str = "00000000-0000-4000-8000-000004100004";
/// 单头总量封顶 50。
const AG_TOTAL_QTY: &str = "00000000-0000-4000-8000-000004100005";
/// 单头总额封顶 100（不含税口径）。
const AG_TOTAL_AMOUNT: &str = "00000000-0000-4000-8000-000004100006";
/// 库中**不存在**的协议主键。
const AG_MISSING: &str = "00000000-0000-4000-8000-0000041000ff";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 夹具 UUID 字面量 → `Uuid`（`?::uuid` 位置必须绑 uuid 类型，绑 Text 会
/// `error serializing parameter` —— 与 V0450 / M4-08 用例踩过的坑同源）。
fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 字面量")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    seed_agreements().await;
    Some(guard)
}

/// 清场：状态日志（无外键）→ 采购单（明细 CASCADE）→ 协议（物料范围 CASCADE）。
async fn cleanup() {
    let sup = supplier().await;
    exec(
        "DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id IN \
            (SELECT id FROM biz_purchase_order WHERE supplier_id = ?::uuid)"
            .to_string()
            .as_str(),
        vec![Value::from(&sup)],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_purchase_order WHERE supplier_id = ?::uuid \
                OR framework_agreement_id IN \
                (SELECT id FROM {0} WHERE agreement_name LIKE '{PREFIX}%')",
            fwa::AGREEMENT_TABLE
        ),
        vec![Value::from(&sup)],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM {} WHERE agreement_name LIKE '{PREFIX}%'",
            fwa::AGREEMENT_TABLE
        ),
        vec![],
    )
    .await;
    // 物料夹具：`test_seed::upsert_material` 是 `ON CONFLICT DO NOTHING`，上一轮若留下
    // 「单位不是基本单位」的行会一直被 66030 拦 → 每轮强制改回基本单位。
    let base_unit = test_seed::ensure_base_unit().await;
    exec(
        "UPDATE biz_material SET unit_id = ?::uuid WHERE material_code LIKE 'M410-%'",
        vec![Value::from(&base_unit)],
    )
    .await;
}

async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUP_CODE, "M410 超限用例供应商").await
}

async fn material_a() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_A_CODE, "M410 物料甲（上限 100）").await
}

async fn material_b() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_B_CODE, "M410 物料乙（不限量）").await
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse("M410-WH", "M410 收货仓").await
}

/// 一份协议的夹具描述。
struct AgreementSeed {
    id: &'static str,
    no: &'static str,
    name: &'static str,
    status: &'static str,
    start: &'static str,
    end: &'static str,
    includes_tax: bool,
    currency: &'static str,
    /// 物料 A 的协议价（物料 B 恒 20.00 不含税，只做「换一个物料」的用例）。
    agreed_price: &'static str,
    tolerance: &'static str,
    total_max_qty: Option<&'static str>,
    total_max_amount: Option<&'static str>,
    /// 写入后立刻软删（`deleted_at = now()`）。
    soft_deleted: bool,
}

async fn insert_agreement(seed: &AgreementSeed, supplier_id: Uuid, mat_a: Uuid, mat_b: Uuid) {
    exec(
        &format!(
            "INSERT INTO {0} (id, agreement_no, agreement_name, supplier_id, supplier_name, \
                              start_date, end_date, sign_date, price_mode, price_includes_tax, \
                              price_currency, price_tolerance, total_max_qty, total_max_amount, status) \
             VALUES (?::uuid, ?::text, ?::text, ?::uuid, ?::text, ?::text::date, ?::text::date, \
                     ?::text::date, 'FIXED', ?::text::boolean, ?::text, ?::text::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?::text)",
            fwa::AGREEMENT_TABLE
        ),
        vec![
            Value::from(&uid(seed.id)),
            Value::Text(seed.no.to_string()),
            Value::Text(seed.name.to_string()),
            Value::from(&supplier_id),
            Value::Text(format!("{PREFIX}供应商快照")),
            Value::Text(seed.start.to_string()),
            Value::Text(seed.end.to_string()),
            Value::Text(seed.start.to_string()),
            Value::Text(seed.includes_tax.to_string()),
            Value::Text(seed.currency.to_string()),
            Value::Text(seed.tolerance.to_string()),
            Value::Text(seed.total_max_qty.unwrap_or_default().to_string()),
            Value::Text(seed.total_max_amount.unwrap_or_default().to_string()),
            Value::Text(seed.status.to_string()),
        ],
    )
    .await;
    for (line_no, material_id, price, max_qty) in [
        (1_i32, mat_a, seed.agreed_price, Some("100")),
        (2_i32, mat_b, "20.000000", None),
    ] {
        exec(
            &format!(
                "INSERT INTO {0} (agreement_id, line_no, material_id, unit_id, agreed_price, \
                                  tax_rate, max_qty) \
                 VALUES (?::uuid, ?, ?::uuid, \
                         (SELECT unit_id FROM biz_material WHERE id = ?::uuid), \
                         ?::text::numeric, 0.13, NULLIF(?::text,'')::numeric)",
                fwa::ITEM_TABLE
            ),
            vec![
                Value::from(&uid(seed.id)),
                Value::from(line_no),
                Value::from(&material_id),
                Value::from(&material_id),
                Value::Text(price.to_string()),
                Value::Text(max_qty.unwrap_or_default().to_string()),
            ],
        )
        .await;
    }
    if seed.soft_deleted {
        exec(
            &format!(
                "UPDATE {} SET deleted_at = now() WHERE id = ?::uuid",
                fwa::AGREEMENT_TABLE
            ),
            vec![Value::from(&uid(seed.id))],
        )
        .await;
    }
}

async fn seed_agreements() {
    let sup = supplier().await;
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    warehouse().await;

    let seeds = [
        AgreementSeed {
            id: AG_MAIN,
            no: "FWA-19990101-941001",
            name: "M410-主协议",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            currency: "CNY",
            agreed_price: "10.000000",
            tolerance: "0.05",
            total_max_qty: None,
            total_max_amount: None,
            soft_deleted: false,
        },
        AgreementSeed {
            id: AG_TAX_AMOUNT,
            no: "FWA-19990101-941002",
            name: "M410-含税总额封顶",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: true,
            currency: "CNY",
            // 11.30 含税 ÷ 1.13 = 10.00 不含税，容差 0
            agreed_price: "11.300000",
            tolerance: "0",
            total_max_qty: None,
            total_max_amount: Some("100"),
            soft_deleted: false,
        },
        AgreementSeed {
            id: AG_USD,
            no: "FWA-19990101-941003",
            name: "M410-外币协议",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            currency: "USD",
            agreed_price: "10.000000",
            tolerance: "0",
            total_max_qty: None,
            total_max_amount: None,
            soft_deleted: false,
        },
        AgreementSeed {
            id: AG_DELETED,
            no: "FWA-19990101-941004",
            name: "M410-已软删协议",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            currency: "CNY",
            agreed_price: "10.000000",
            tolerance: "0",
            total_max_qty: None,
            total_max_amount: None,
            soft_deleted: true,
        },
        AgreementSeed {
            id: AG_TOTAL_QTY,
            no: "FWA-19990101-941005",
            name: "M410-总量封顶",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            currency: "CNY",
            agreed_price: "10.000000",
            tolerance: "0",
            total_max_qty: Some("50"),
            total_max_amount: None,
            soft_deleted: false,
        },
        AgreementSeed {
            id: AG_TOTAL_AMOUNT,
            no: "FWA-19990101-941006",
            name: "M410-总额封顶",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            currency: "CNY",
            agreed_price: "10.000000",
            tolerance: "0",
            total_max_qty: None,
            total_max_amount: Some("100"),
            soft_deleted: false,
        },
    ];
    for seed in &seeds {
        insert_agreement(seed, sup, mat_a, mat_b).await;
    }
}

/// 建单请求（`EXCL` 口径 + 指定协议；行 = (物料, 数量, 不含税单价)）。
fn req(
    agreement: Option<&str>,
    supplier_id: Uuid,
    warehouse_id: Uuid,
    lines: &[(Uuid, &str, &str)],
) -> CreatePurchaseOrderReq {
    CreatePurchaseOrderReq {
        supplier_id: Some(supplier_id.to_string()),
        warehouse_id: Some(warehouse_id.to_string()),
        purchase_type: Some("SPOT".to_string()),
        contract_id: None,
        framework_agreement_id: agreement.map(str::to_string),
        project_id: None,
        order_date: Some(date("2026-11-18")),
        expect_date: None,
        price_mode: Some("EXCL".to_string()),
        credit_days: None,
        remark: Some(format!("{PREFIX}用例")),
        items: lines
            .iter()
            .map(|(material_id, qty, price)| PurchaseOrderItemReq {
                material_id: Some(material_id.to_string()),
                qty: Some(dec(qty)),
                price_excl: Some(dec(price)),
                price_incl: None,
                tax_rate: Some(dec("13")),
                remark: None,
            })
            .collect(),
    }
}

/// 按协议下多行单（真实入口）。
async fn create_multi(agreement: &str, lines: &[(Uuid, &str, &str)]) -> std::result::Result<Uuid, ApiError> {
    let sup = supplier().await;
    let wh = warehouse().await;
    let request = req(Some(agreement), sup, wh, lines);
    purchase_handler::create_purchase_order(&request, None).await
}

/// 按协议下**单行**单（真实入口）。
async fn create_with(agreement: &str, material: Uuid, qty: &str, price: &str) -> std::result::Result<Uuid, ApiError> {
    create_multi(agreement, &[(material, qty, price)]).await
}

/// 读一个标量（数值），没有行则 0。
async fn decimal_of(sql: &str, params: Vec<Value>) -> Decimal {
    let rows = db()
        .find_with_params(sql, params)
        .await
        .unwrap_or_else(|error| panic!("查询失败：{error}\n{sql}"));
    match rows.first() {
        None => Decimal::ZERO,
        Some(row) => row.get_typed("v").expect("数值列"),
    }
}

/// 某个协议行当前的已执行量（回写列）。
async fn executed_of(agreement_id: &str, material: Uuid) -> Decimal {
    decimal_of(
        &format!(
            "SELECT COALESCE(executed_qty, 0) AS v FROM {} \
              WHERE agreement_id = ?::uuid AND material_id = ?::uuid",
            fwa::ITEM_TABLE
        ),
        vec![Value::from(&uid(agreement_id)), Value::from(&material)],
    )
    .await
}

/// 直接改回写列（模拟「漏刷 / 脏数据」—— 只用来越权验证「准入不看它」）。
async fn set_booked(agreement_id: &str, material: Uuid, qty: &str) {
    exec(
        &format!(
            "UPDATE {} SET executed_qty = ?::text::numeric \
              WHERE agreement_id = ?::uuid AND material_id = ?::uuid",
            fwa::ITEM_TABLE
        ),
        vec![
            Value::Text(qty.to_string()),
            Value::from(&uid(agreement_id)),
            Value::from(&material),
        ],
    )
    .await;
}

/// 该供应商下的采购单张数（「被拒后一张都不许落库」用）。
async fn order_count() -> i64 {
    let rows = db()
        .find_with_params(
            "SELECT count(*) AS v FROM biz_purchase_order WHERE supplier_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&supplier().await)],
        )
        .await
        .expect("统计采购单");
    rows.first().and_then(|row| row.get_typed::<i64>("v").ok()).unwrap_or(0)
}

// ==================== 用例 ====================

/// 绕限反例：同一物料拆成两行 —— 单行各 60 都不超明细上限，合计 120 必须被拦。
///
/// 判据在 `resolve` ⑥：本单的用量按**协议行**聚合（`this_qty`）后再比上限，
/// 不是逐行比 —— 逐行比的话「两行各 60」就能把 100 的上限买成 120。
#[tokio::test]
async fn split_lines_on_same_material_cannot_bypass_cap() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let before = order_count().await;

    let error = create_multi(AG_MAIN, &[(mat, "60", "10"), (mat, "60", "10")])
        .await
        .expect_err("同一物料两行各 60（单行都不超 100）合计 120 必须拒绝");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(
        error.msg.contains("本单该物料合计"),
        "报文必须说明是「本单该物料合计」超限，而不是单行超限：{}",
        error.msg
    );
    assert_eq!(order_count().await, before, "被拒的建单不许留下任何单据");
    assert_eq!(executed_of(AG_MAIN, mat).await, Decimal::ZERO, "被拒的建单不许占额度");

    // 反证：同样两行、合计正好 100 时放行 —— 证明拒绝来自「合计超限」，不是「两行」本身
    create_multi(AG_MAIN, &[(mat, "60", "10"), (mat, "40", "10")])
        .await
        .expect("合计恰等于上限 100 必须放行");
    assert_eq!(executed_of(AG_MAIN, mat).await, dec("100"));
    println!("[M4-10] 绕限 同一物料拆两行合计超限 409/66098，合计恰好 100 放行 ✓");
}

/// 边界反例：明细上限已满 100 时，再下 0.0001（最小标度）也必须拒。
///
/// `>` 语义：`used + added > max_qty` 才拒，`==` 放行。报文里的「剩余」是
/// `max_qty - used`（此处 0），不是被截断的负数或空串。
#[tokio::test]
async fn per_material_cap_rejects_fractional_overrun() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    create_with(AG_MAIN, mat, "100", "10").await.expect("先占满 100");

    let error = create_with(AG_MAIN, mat, "0.0001", "10")
        .await
        .expect_err("超出 0.0001 也是超限（不许因为「量很小」放过）");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(error.msg.contains("剩余 0"), "报文必须给出剩余量 0：{}", error.msg);
    assert_eq!(executed_of(AG_MAIN, mat).await, dec("100"), "被拒的单不许污染已执行量");
    println!("[M4-10] 边界 超 0.0001 拒绝 + 报文剩余 0 ✓");
}

/// 不限量（`max_qty IS NULL`）就是**不设上限**，不是「上限 0」。
///
/// 用 123456 这种远超任何常规量的数字验证：`None` 分支必须直接 `continue`，
/// 任何 `unwrap_or(0)` 都会把「不限」读成「一点都不剩」。
#[tokio::test]
async fn unlimited_material_is_not_capped() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_b().await;
    let id = create_with(AG_MAIN, mat, "123456", "20")
        .await
        .expect("不限量物料不受数量上限约束");
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        dec("123456"),
        "回写列必须如实记录（不限量不等于不统计）"
    );
    let items = fwa_ref::referenced_items_of_order(db(), id).await.expect("明细引用");
    assert_eq!(items.len(), 1, "明细必须带协议行引用");
    println!("[M4-10] 不限量 123456 照下且回写如实 ✓");
}

/// 判据来源（方向一）：回写列**漏刷清零**时，准入仍按现算 Σ 拦住。
///
/// 若准入读回写列，协议明明已被 100 占满，却会因为「回写列 0」再放行一单 ——
/// 漏刷一次就白放一次量。`resolve` ⑥ 的 `used` 来自 `usage_of_agreement`（现算），
/// 报文里的「已用」因此必须是 100 而不是 0。
#[tokio::test]
async fn stale_booked_column_does_not_open_the_gate() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    create_with(AG_MAIN, mat, "100", "10").await.expect("先占满 100");

    // 模拟漏刷：把回写列清零（真实库里可能就是某次重算没跑到）
    set_booked(AG_MAIN, mat, "0").await;
    let error = create_with(AG_MAIN, mat, "1", "10")
        .await
        .expect_err("回写列清零也照样拦住（准入必须看现算 Σ）");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(
        error.msg.contains("已用 100"),
        "报文里的「已用」必须是现算的 100，不是回写列的 0：{}",
        error.msg
    );
    println!("[M4-10] 判据来源 回写列漏刷清零仍拦住（现算 100）✓");
}

/// 判据来源（方向二）：回写列**虚高**时不许误拒合法单，且下单后重算自愈。
///
/// 只用了 60，回写列却被写成 999999：若准入读回写列，剩 40 的合法单会被拒；
/// 现算 Σ 下 60 + 40 = 100 恰等于上限 → 放行，并顺手把回写列重算回 100。
#[tokio::test]
async fn inflated_booked_column_does_not_false_reject() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    create_with(AG_MAIN, mat, "60", "10").await.expect("先下 60");
    set_booked(AG_MAIN, mat, "999999").await;

    create_with(AG_MAIN, mat, "40", "10")
        .await
        .expect("回写列虚高不许误拒（现算 60 + 40 = 100 恰好到顶）");
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        dec("100"),
        "下单后回写列必须被重算成真实值 100（自愈，而不是 999999 + 40）"
    );
    println!("[M4-10] 判据来源 回写列虚高不误拒 + 重算自愈 ✓");
}

/// 协议不存在 / 已软删 → 400 / 66096（「协议不可用」而不是 404「单据不存在」）。
#[tokio::test]
async fn soft_deleted_agreement_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;

    let deleted = create_with(AG_DELETED, mat, "1", "10")
        .await
        .expect_err("已软删的协议不许再被引用");
    assert_eq!(deleted.status, 400, "{}", deleted.msg);
    assert_eq!(deleted.code, error_code::ERP_FWA_NOT_APPLICABLE, "{}", deleted.msg);
    assert!(
        deleted.msg.contains("不存在或已删除"),
        "报文必须说明协议不存在/已删除：{}",
        deleted.msg
    );

    let missing = create_with(AG_MISSING, mat, "1", "10")
        .await
        .expect_err("不存在的协议必须拒绝");
    assert_eq!(missing.status, 400, "{}", missing.msg);
    assert_eq!(missing.code, error_code::ERP_FWA_NOT_APPLICABLE, "{}", missing.msg);
    println!("[M4-10] 可用性 软删/不存在协议 400/66096 ✓");
}

/// 币种不一致 → 400 / 66096：跨币种比价没有意义（汇率谁定、按哪天都不在协议里）。
///
/// 采购单建单请求体**没有**币种字段，服务端恒按 CNY（`ORDER_CURRENCY`）——
/// 所以外币协议一律不能引用，而不是「按协议币种换算」。
#[tokio::test]
async fn currency_mismatch_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let error = create_with(AG_USD, mat, "1", "10")
        .await
        .expect_err("USD 协议不能被人民币单引用");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_NOT_APPLICABLE, "{}", error.msg);
    assert!(
        error.msg.contains("币种") && error.msg.contains("USD"),
        "报文必须点出币种不一致（USD ↔ CNY）：{}",
        error.msg
    );
    println!("[M4-10] 币种 USD ↔ CNY 400/66096 ✓");
}

/// 含税协议的**总额上限**按价税合计比（`amount_for_cap`）。
///
/// 10 × 10（不含税金额 100 / 价税合计 113）在总额上限 100 下：
/// * 用价税合计比 → 113 > 100 → 拒（正确）；
/// * 用不含税金额比 → 100 ≤ 100 → 放（等于白送 13% 的额度）。
/// 反证：8 件（80 / 90.4）放行，说明限额确实在 100 附近生效。
#[tokio::test]
async fn tax_inclusive_total_amount_cap_counts_with_tax() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let error = create_with(AG_TAX_AMOUNT, mat, "10", "10")
        .await
        .expect_err("含税协议总额 100，价税合计 113 必须超限");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(
        error.msg.contains("总额") && error.msg.contains("含税"),
        "报文必须点名「总额」且标明含税口径：{}",
        error.msg
    );
    create_with(AG_TAX_AMOUNT, mat, "8", "10")
        .await
        .expect("价税合计 90.4 < 100 必须放行（说明上限在 100 附近，不是永不生效）");
    println!("[M4-10] 金额 含税协议总额走价税合计口径 ✓");
}

/// 改单必须排除**本单自己的旧明细**（`exclude_order`），否则原样改回就变 200 超限。
#[tokio::test]
async fn update_does_not_double_count_own_usage() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let id = create_with(AG_MAIN, mat, "100", "10").await.expect("先占满 100");
    let sup = supplier().await;
    let wh = warehouse().await;

    // 原样改单（100 → 100）：若不排除自己，现算 100 + 本单 100 = 200 > 100 会误拒
    let same = req(Some(AG_MAIN), sup, wh, &[(mat, "100", "10")]);
    purchase_handler::update_purchase_order(id, &same, None)
        .await
        .expect("原样改单必须放行（不许把自己的旧明细算两遍）");
    assert_eq!(executed_of(AG_MAIN, mat).await, dec("100"));

    // 改成 120：确实超限 → 409，且额度保持 100（失败的改单整单回滚）
    let over = req(Some(AG_MAIN), sup, wh, &[(mat, "120", "10")]);
    let error = purchase_handler::update_purchase_order(id, &over, None)
        .await
        .expect_err("改到 120 超上限必须拒绝");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        dec("100"),
        "被拒的改单不许改动额度（旧明细仍占 100）"
    );
    println!("[M4-10] 改单 排除自身旧明细 + 超限整单回滚 ✓");
}

/// 单头总量上限的边界：30 + 20 = 50 恰等于上限放行；再多 1 拒。
#[tokio::test]
async fn head_total_qty_cap_allows_exact_boundary() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    create_multi(AG_TOTAL_QTY, &[(mat_a, "30", "10"), (mat_b, "20", "20")])
        .await
        .expect("总量恰等于 50 必须放行");

    let error = create_with(AG_TOTAL_QTY, mat_b, "1", "20")
        .await
        .expect_err("51 > 总量 50 必须拒绝");
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(error.msg.contains("总量"), "报文必须点名「总量」上限：{}", error.msg);
    println!("[M4-10] 边界 单头总量恰好 50 放行 / +1 拒绝 ✓");
}

/// 单头总额上限的边界：10 × 10 = 100 恰等于上限放行；再下 20 拒。
#[tokio::test]
async fn head_total_amount_cap_allows_exact_boundary() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    create_with(AG_TOTAL_AMOUNT, mat_a, "10", "10")
        .await
        .expect("金额恰等于 100 必须放行");

    let error = create_with(AG_TOTAL_AMOUNT, mat_b, "1", "20")
        .await
        .expect_err("120 > 总额 100 必须拒绝");
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(error.msg.contains("总额"), "报文必须点名「总额」上限：{}", error.msg);
    assert_eq!(
        executed_of(AG_TOTAL_AMOUNT, mat_b).await,
        Decimal::ZERO,
        "被拒的单不占另一物料的额度"
    );
    println!("[M4-10] 边界 单头总额恰好 100 放行 / 再下 20 拒绝 ✓");
}

/// 清场：`M410-` 夹具 0 残留（libtest 按名字排序，`zz_` 必然最后跑）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    cleanup().await;
    let agreements = decimal_of(
        &format!(
            "SELECT count(*)::numeric AS v FROM {} WHERE agreement_name LIKE '{PREFIX}%'",
            fwa::AGREEMENT_TABLE
        ),
        vec![],
    )
    .await;
    assert_eq!(agreements, Decimal::ZERO, "M410- 协议 0 残留");
    let orders = decimal_of(
        "SELECT count(*)::numeric AS v FROM biz_purchase_order \
          WHERE supplier_id IN (SELECT id FROM biz_supplier WHERE supplier_code LIKE 'M410-%')",
        vec![],
    )
    .await;
    assert_eq!(orders, Decimal::ZERO, "M410- 采购单 0 残留");
    println!("[M4-10] 清场 夹具 0 残留 ✓");
}

// ============================================================================
// framework_agreement_ref_db_tests.rs —— 框架协议「订单引用」的真库用例（M4-08）
//
// 卡：t_06b8c65b（M4-08【M4·采购】框架协议：订单引用（校验价格/数量）· 超限拒绝）
//
// # 覆盖什么（每条都对应卡面 / V0451 / `framework_agreement_ref` 里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `order_without_agreement_is_allowed` | 引用是可选的：不挂协议照常下单，价格不校验、额度不动 |
// | `price_at_cap_is_accepted` | 卡面「校验价格」放行侧：`单价 = 协议价 × (1+容差)` 必须放得过去（`<=` 语义） |
// | `price_above_cap_is_rejected` | 卡面「超限拒绝」：超容差 → **409 / 66097**，且**一张单都不许落库** |
// | `tax_inclusive_agreement_compares_ex_tax` | 协议价含税时先按协议税率折成不含税再比（拿含税价直接比会白送 13%） |
// | `per_material_cap_blocks_over_placement` | 卡面「校验数量」：同协议行累计量超 `max_qty` → **409 / 66098**，报文给剩余量 |
// | `agreement_total_qty_cap` | 单头 `total_max_qty` 封顶（一揽子协议只给总量） |
// | `agreement_total_amount_cap` | 单头 `total_max_amount` 封顶（按协议价口径比金额） |
// | `draft_update_recomputes_executed` | 改单后已执行量**重算**（不是 +=）：100 → 30 |
// | `cancelled_order_releases_quota` | 作废释放额度（作废单不该继续吃上限） |
// | `deleted_order_releases_quota` | 软删释放额度 |
// | `non_effective_agreement_is_rejected` | 草稿 / 过期的协议不能下单（**400 / 66096**，报文给两条出路） |
// | `supplier_and_material_scope_are_enforced` | 供应商不符 / 物料不在协议范围 → 400（报文点出行号） |
// | `detail_exposes_agreement_reference` | 详情出参带协议引用（协议号 / 状态 / 容差） |
// | `zz_teardown_purges_fixtures` | 清场：`M408-` 夹具 0 残留 |
//
// # 夹具
//
// 协议两表 M4-07 只交表结构（没有 CRUD 接口），所以协议夹具走裸 SQL；
// 采购单走**真实入口** `purchase_handler::create_purchase_order`（本卡改的就是这条路径）。
// 主数据（供应商 / 物料 / 仓库）一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m408_drill" cargo test --lib \
//   business::erp::purchase::framework_agreement_ref -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::framework_agreement_model as fwa;
use crate::business::erp::purchase::framework_agreement_ref as fwa_ref;
use crate::business::erp::purchase::purchase_dto::{
    CreatePurchaseOrderReq, PurchaseOrderItemReq, PurchaseOrderStatusReq,
};
use crate::business::erp::purchase::purchase_handler;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具名前缀（清场按它反查，绝不按「生产号段」删）。
const PREFIX: &str = "M408-";
/// 夹具供应商编码（正常供应商）。
const SUP_CODE: &str = "M408-SUP";
/// 夹具供应商编码（「别人家的」供应商：用来验供应商不符）。
const SUP_OTHER_CODE: &str = "M408-SUP2";
/// 夹具物料编码（在协议范围内，有数量上限）。
const MAT_A_CODE: &str = "M408-MATA";
/// 夹具物料编码（在协议范围内，无数量上限）。
const MAT_B_CODE: &str = "M408-MATB";
/// 夹具物料编码（**不在**协议范围内）。
const MAT_OUT_CODE: &str = "M408-MATOUT";

// ---------- 协议夹具（固定 UUID；setup() 每次重建）----------
/// 主夹具：10.00 不含税 / 5% 容差 / 物料 A 上限 100、物料 B 不限。
const AG_MAIN: &str = "00000000-0000-4000-8000-000004080001";
/// 含税协议：11.30 含税（税率 13% ⇒ 折不含税 10.00）/ 容差 0。
const AG_TAX_INCL: &str = "00000000-0000-4000-8000-000004080002";
/// 总量封顶：`total_max_qty = 50`。
const AG_TOTAL_QTY: &str = "00000000-0000-4000-8000-000004080003";
/// 总额封顶：`total_max_amount = 100`（不含税口径）。
const AG_TOTAL_AMOUNT: &str = "00000000-0000-4000-8000-000004080004";
/// 草稿协议（不能下单）。
const AG_DRAFT: &str = "00000000-0000-4000-8000-000004080005";
/// 已过期协议（APPROVED 但效期在 2020 年）。
const AG_EXPIRED: &str = "00000000-0000-4000-8000-000004080006";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 夹具 UUID 字面量 → `Uuid`（`?::uuid` 位置必须绑 uuid 类型，绑 Text 会
/// `error serializing parameter` —— 与 V0450 用例踩过的坑同源）。
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
    let sup = test_seed::ensure_supplier(SUP_CODE, "M408 配件供应商").await;
    let sup_other = test_seed::ensure_supplier(SUP_OTHER_CODE, "M408 别人家的供应商").await;
    exec(
        "DELETE FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER' AND entity_id IN \
            (SELECT id FROM biz_purchase_order WHERE supplier_id IN (?::uuid, ?::uuid))"
            .to_string()
            .as_str(),
        vec![Value::from(&sup), Value::from(&sup_other)],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_purchase_order WHERE supplier_id IN (?::uuid, ?::uuid) \
                OR framework_agreement_id IN \
                (SELECT id FROM {0} WHERE agreement_name LIKE '{PREFIX}%')",
            fwa::AGREEMENT_TABLE
        ),
        vec![Value::from(&sup), Value::from(&sup_other)],
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
    // 物料夹具：上一轮可能留下「单位不是基本单位」的行程（`test_seed::upsert_material` 是
    // ON CONFLICT DO NOTHING）→ 每轮强制把单位改回基本单位，免得用例被 66030 误拦。
    let base_unit = test_seed::ensure_base_unit().await;
    exec(
        "UPDATE biz_material SET unit_id = ?::uuid WHERE material_code LIKE 'M408-%'",
        vec![Value::from(&base_unit)],
    )
    .await;
}

async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUP_CODE, "M408 配件供应商").await
}

async fn supplier_other() -> Uuid {
    test_seed::ensure_supplier(SUP_OTHER_CODE, "M408 别人家的供应商").await
}

async fn material_a() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_A_CODE, "M408 物料甲（限量）").await
}

async fn material_b() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_B_CODE, "M408 物料乙（不限量）").await
}

async fn material_out() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_OUT_CODE, "M408 物料丙（协议外）").await
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse("M408-WH", "M408 收货仓").await
}

/// 一张协议 + 两行物料范围（A：上限 100；B：不限）。
struct AgreementSeed {
    id: &'static str,
    no: &'static str,
    name: &'static str,
    status: &'static str,
    start: &'static str,
    end: &'static str,
    includes_tax: bool,
    agreed_price: &'static str,
    tolerance: &'static str,
    total_max_qty: Option<&'static str>,
    total_max_amount: Option<&'static str>,
}

async fn insert_agreement(seed: &AgreementSeed, supplier_id: Uuid, material_a: Uuid, material_b: Uuid) {
    exec(
        &format!(
            "INSERT INTO {0} (id, agreement_no, agreement_name, supplier_id, supplier_name, \
                              start_date, end_date, sign_date, price_mode, price_includes_tax, \
                              price_currency, price_tolerance, total_max_qty, total_max_amount, status) \
             VALUES (?::uuid, ?::text, ?::text, ?::uuid, ?::text, ?::text::date, ?::text::date, \
                     ?::text::date, 'FIXED', ?::text::boolean, 'CNY', ?::text::numeric, \
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
            Value::Text(seed.tolerance.to_string()),
            Value::Text(seed.total_max_qty.unwrap_or_default().to_string()),
            Value::Text(seed.total_max_amount.unwrap_or_default().to_string()),
            Value::Text(seed.status.to_string()),
        ],
    )
    .await;
    for (line_no, material_id, agreed_price, max_qty) in [
        (1_i32, material_a, seed.agreed_price, Some("100")),
        (2_i32, material_b, "20.000000", None),
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
                Value::Text(agreed_price.to_string()),
                Value::Text(max_qty.unwrap_or_default().to_string()),
            ],
        )
        .await;
    }
}

async fn seed_agreements() {
    let sup = supplier().await;
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    // 物料主数据先准备好（`unit_id` 从物料带出，见上面 SQL）
    material_out().await;
    warehouse().await;
    supplier_other().await;

    let seeds = [
        AgreementSeed {
            id: AG_MAIN,
            no: "FWA-19990101-940101",
            name: "M408-主协议",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            agreed_price: "10.000000",
            tolerance: "0.05",
            total_max_qty: None,
            total_max_amount: None,
        },
        AgreementSeed {
            id: AG_TAX_INCL,
            no: "FWA-19990101-940102",
            name: "M408-含税协议",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: true,
            agreed_price: "11.300000",
            tolerance: "0",
            total_max_qty: None,
            total_max_amount: None,
        },
        AgreementSeed {
            id: AG_TOTAL_QTY,
            no: "FWA-19990101-940103",
            name: "M408-总量封顶",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            agreed_price: "10.000000",
            tolerance: "0.05",
            total_max_qty: Some("50"),
            total_max_amount: None,
        },
        AgreementSeed {
            id: AG_TOTAL_AMOUNT,
            no: "FWA-19990101-940104",
            name: "M408-总额封顶",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            agreed_price: "10.000000",
            tolerance: "0.05",
            total_max_qty: None,
            total_max_amount: Some("100"),
        },
        AgreementSeed {
            id: AG_DRAFT,
            no: "FWA-19990101-940105",
            name: "M408-草稿协议",
            status: fwa::STATUS_DRAFT,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            agreed_price: "10.000000",
            tolerance: "0.05",
            total_max_qty: None,
            total_max_amount: None,
        },
        AgreementSeed {
            id: AG_EXPIRED,
            no: "FWA-19990101-940106",
            name: "M408-过期协议",
            status: fwa::STATUS_APPROVED,
            start: "2020-01-01",
            end: "2020-12-31",
            includes_tax: false,
            agreed_price: "10.000000",
            tolerance: "0.05",
            total_max_qty: None,
            total_max_amount: None,
        },
    ];
    for seed in &seeds {
        insert_agreement(seed, sup, mat_a, mat_b).await;
    }
}

/// 建一张采购单的请求（`EXCL` 口径 + 指定协议；行 = (物料, 数量, 不含税单价)）。
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

/// 方便：主夹具协议 + 单行物料 A。
async fn create_with(agreement: &str, material: Uuid, qty: &str, price: &str) -> std::result::Result<Uuid, ApiError> {
    let sup = supplier().await;
    let wh = warehouse().await;
    let request = req(Some(agreement), sup, wh, &[(material, qty, price)]);
    purchase_handler::create_purchase_order(&request, None).await
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

/// 引用是可选的：不挂协议照常下单，价格再高也不校验、协议额度不动。
#[tokio::test]
async fn order_without_agreement_is_allowed() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let sup = supplier().await;
    let wh = warehouse().await;
    let mat = material_a().await;
    // 单价 999 远超任何协议价：不挂协议就不该被拦（零星采购的价格由采购员负责）
    let request = req(None, sup, wh, &[(mat, "5", "999")]);
    let id = purchase_handler::create_purchase_order(&request, None)
        .await
        .expect("不挂协议必须能下单");
    let detail = purchase_handler::detail_of(id).await.expect("详情");
    assert!(detail.framework_agreement.is_none(), "不挂协议的采购单不许有协议引用");
    assert!(
        fwa_ref::referenced_items_of_order(db(), id).await.unwrap().is_empty(),
        "明细不许带协议行 id"
    );
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        Decimal::ZERO,
        "不挂协议不许动协议已执行量"
    );
    println!("[M4-08] 可选引用 不挂协议可下单，额度不占 ✓");
}

/// 卡面「校验价格」的放行侧：`单价 = 协议价 × (1 + 容差)` 必须放得过去（`<=` 语义），
/// 并在同一事务里把已执行量回写上去。
#[tokio::test]
async fn price_at_cap_is_accepted() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    // 主协议：10.00 不含税 + 5% 容差 ⇒ 上限 10.50
    let id = create_with(AG_MAIN, mat, "30", "10.5").await.expect("等于上限必须放行");
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        dec("30"),
        "下单后已执行量必须回写（30）"
    );
    // 已执行金额 = 30 × 10.5 = 315
    let amount = decimal_of(
        &format!(
            "SELECT executed_amount AS v FROM {} WHERE agreement_id = ?::uuid AND material_id = ?::uuid",
            fwa::ITEM_TABLE
        ),
        vec![Value::from(&uid(AG_MAIN)), Value::from(&mat)],
    )
    .await;
    assert_eq!(amount, dec("315"), "已执行金额按不含税口径回写");
    // 明细真的落了协议行 id
    let items = fwa_ref::referenced_items_of_order(db(), id).await.unwrap();
    assert_eq!(items.len(), 1, "明细必须带协议行引用");

    let detail = purchase_handler::detail_of(id).await.expect("详情");
    let reference = detail.framework_agreement.expect("详情必须带协议引用");
    assert_eq!(reference.agreement_no, "FWA-19990101-940101");
    assert_eq!(reference.status.as_deref(), Some(fwa::STATUS_APPROVED));
    assert_eq!(reference.price_tolerance, Some(dec("0.05")));
    assert_eq!(reference.price_includes_tax, Some(false));
    println!("[M4-08] 价格 等额放行 + 执行量回写 ✓");
}

/// 卡面「超限拒绝」的价格侧：超容差 → 409 / 66097，报文给出上限与算法，
/// 且**一张单都不许落库**（事务回滚，不留半张单）。
#[tokio::test]
async fn price_above_cap_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let before = order_count().await;
    let mat = material_a().await;
    let error = create_with(AG_MAIN, mat, "10", "10.51")
        .await
        .expect_err("超容差必须拒绝");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_PRICE_EXCEEDED, "{}", error.msg);
    assert!(
        error.msg.contains("10.500000") && error.msg.contains("容差"),
        "报文必须给出上限与容差：{}",
        error.msg
    );
    assert_eq!(order_count().await, before, "被拒的建单不许留下任何单据");
    assert_eq!(executed_of(AG_MAIN, mat).await, Decimal::ZERO, "被拒的建单不许占额度");
    println!("[M4-08] 价格 超容差 409/66097，零落库 ✓");
}

/// 协议价含税：先按协议税率（0.13 分数）折成不含税 10.00 再比价。
/// 拿 11.30 直接跟采购单的不含税价比，等于白送 13% 的涨价空间。
#[tokio::test]
async fn tax_inclusive_agreement_compares_ex_tax() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    // 含税协议 11.30 / 税率 13% ⇒ 不含税 10.00（容差 0）
    create_with(AG_TAX_INCL, mat, "1", "10")
        .await
        .expect("等于折算后的协议价必须放行");
    let error = create_with(AG_TAX_INCL, mat, "1", "10.01")
        .await
        .expect_err("高于折算后的协议价必须拒绝");
    assert_eq!(error.code, error_code::ERP_FWA_PRICE_EXCEEDED, "{}", error.msg);
    assert!(
        error.msg.contains("含税") && error.msg.contains("已按税率折算"),
        "报文必须说明「协议价含税，已按税率折算」：{}",
        error.msg
    );
    // 上限必须是**折算后**的 10（不是 11.30 —— 那等于白送 13%）
    assert!(
        error.msg.contains("上限 10"),
        "报文的上限必须是折算后的 10（含税 11.30 按 13% 倒推）：{}",
        error.msg
    );
    println!("[M4-08] 价格 含税协议按税率折算后比价 ✓");
}

/// 卡面「校验数量」：同一协议行的累计量超 `max_qty` → 409 / 66098，
/// 报文给出「已用 / 本单 / 上限 / 剩余」，且额度按「现算」而不是「回写列」。
#[tokio::test]
async fn per_material_cap_blocks_over_placement() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    create_with(AG_MAIN, mat, "60", "10").await.expect("首单 60 在上限内");
    let error = create_with(AG_MAIN, mat, "50", "10")
        .await
        .expect_err("累计 110 > 上限 100 必须拒绝");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(
        error.msg.contains("100") && error.msg.contains("60") && error.msg.contains("剩余"),
        "报文必须给出上限 / 已用 / 剩余：{}",
        error.msg
    );
    // 额度没被那笔失败的单污染
    assert_eq!(executed_of(AG_MAIN, mat).await, dec("60"), "失败单不许占额度");

    create_with(AG_MAIN, mat, "40", "10")
        .await
        .expect("补到 100 恰好等于上限，必须放行");
    assert_eq!(executed_of(AG_MAIN, mat).await, dec("100"));
    println!("[M4-08] 数量 明细上限 409/66098 + 恰等于上限放行 ✓");
}

/// 单头总量封顶：一揽子协议只给总量、不给逐物料量时也要拦得住。
#[tokio::test]
async fn agreement_total_qty_cap() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    // 总量上限 50：物料 A 40 + 物料 B 20 = 60 > 50（单物料都没超明细上限）
    create_with(AG_TOTAL_QTY, mat_a, "40", "10")
        .await
        .expect("首单 40 在总量内");
    let error = create_with(AG_TOTAL_QTY, mat_b, "20", "20")
        .await
        .expect_err("总量 60 > 50 必须拒绝");
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(error.msg.contains("总量"), "报文必须点名「总量」上限：{}", error.msg);
    println!("[M4-08] 数量 单头总量封顶 ✓");
}

/// 单头总额封顶：按协议价口径比金额（不含税协议比不含税合计）。
#[tokio::test]
async fn agreement_total_amount_cap() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    // 总额上限 100：A 6×10 = 60；B 3×20 = 60 → 120 > 100
    create_with(AG_TOTAL_AMOUNT, mat_a, "6", "10")
        .await
        .expect("首单 60 在总额内");
    let error = create_with(AG_TOTAL_AMOUNT, mat_b, "3", "20")
        .await
        .expect_err("总额 120 > 100 必须拒绝");
    assert_eq!(error.code, error_code::ERP_FWA_QTY_EXCEEDED, "{}", error.msg);
    assert!(error.msg.contains("总额"), "报文必须点名「总额」上限：{}", error.msg);
    println!("[M4-08] 金额 单头总额封顶 ✓");
}

/// 改单：已执行量**重算**成新值（不是「+=」），旧明细的量必须释放。
#[tokio::test]
async fn draft_update_recomputes_executed() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let id = create_with(AG_MAIN, mat, "100", "10").await.expect("建单 100");
    assert_eq!(executed_of(AG_MAIN, mat).await, dec("100"));

    let sup = supplier().await;
    let wh = warehouse().await;
    let request = req(Some(AG_MAIN), sup, wh, &[(mat, "30", "10")]);
    purchase_handler::update_purchase_order(id, &request, None)
        .await
        .expect("草稿改单");
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        dec("30"),
        "改单后必须重算成 30（残留 100 说明没释放旧明细）"
    );
    println!("[M4-08] 改单 已执行量重算而不是累加 ✓");
}

/// 作废：额度必须还回去（作废单继续吃上限，等于把协议量白占着）。
#[tokio::test]
async fn cancelled_order_releases_quota() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let id = create_with(AG_MAIN, mat, "100", "10").await.expect("建单 100");
    assert_eq!(executed_of(AG_MAIN, mat).await, dec("100"));
    // 额度满了：再下 1 个都该被拒
    assert!(create_with(AG_MAIN, mat, "1", "10").await.is_err());

    let status = PurchaseOrderStatusReq {
        status: Some("CANCELLED".to_string()),
        reason: Some("M408 用例：作废释放额度".to_string()),
        ..Default::default()
    };
    purchase_handler::change_purchase_order_status(id, &status, None)
        .await
        .expect("草稿作废");
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        Decimal::ZERO,
        "作废后已执行量必须回落到 0"
    );
    create_with(AG_MAIN, mat, "100", "10")
        .await
        .expect("释放后同样 100 必须能再下");
    println!("[M4-08] 作废 释放额度 ✓");
}

/// 软删：与作废同口径 —— 删掉的单不该继续占额度。
#[tokio::test]
async fn deleted_order_releases_quota() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let id = create_with(AG_MAIN, mat, "80", "10").await.expect("建单 80");
    purchase_handler::delete_purchase_order(id, None)
        .await
        .expect("草稿软删");
    assert_eq!(
        executed_of(AG_MAIN, mat).await,
        Decimal::ZERO,
        "软删后已执行量必须回落到 0"
    );
    create_with(AG_MAIN, mat, "100", "10")
        .await
        .expect("释放后 100 必须能下");
    println!("[M4-08] 软删 释放额度 ✓");
}

/// 草稿 / 过期的协议不能下单：400 / 66096，报文给出起止日与「改用零星采购」的出路。
#[tokio::test]
async fn non_effective_agreement_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_a().await;
    let draft = create_with(AG_DRAFT, mat, "1", "10")
        .await
        .expect_err("草稿协议不许下单");
    assert_eq!(draft.status, 400, "{}", draft.msg);
    assert_eq!(draft.code, error_code::ERP_FWA_NOT_APPLICABLE, "{}", draft.msg);
    assert!(
        draft.msg.contains("APPROVED") || draft.msg.contains("审批"),
        "报文必须说明只有审批通过的协议能下单：{}",
        draft.msg
    );

    let expired = create_with(AG_EXPIRED, mat, "1", "10")
        .await
        .expect_err("过期协议不许下单");
    assert_eq!(expired.code, error_code::ERP_FWA_NOT_APPLICABLE, "{}", expired.msg);
    assert!(
        expired.msg.contains("2020-12-31") && expired.msg.contains("零星采购"),
        "报文必须给出到期日与出路：{}",
        expired.msg
    );
    println!("[M4-08] 可用性 草稿/过期协议 400/66096 ✓");
}

/// 供应商不符 / 物料不在协议范围 → 400，报文点出行号与物料。
#[tokio::test]
async fn supplier_and_material_scope_are_enforced() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let wh = warehouse().await;
    let mat = material_a().await;
    // ① 供应商不符：协议对的是 M408-SUP，本单填「别人家的」
    let other = supplier_other().await;
    let request = req(Some(AG_MAIN), other, wh, &[(mat, "1", "10")]);
    let error = purchase_handler::create_purchase_order(&request, None)
        .await
        .expect_err("供应商不符必须拒绝");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_FWA_NOT_APPLICABLE, "{}", error.msg);
    assert!(error.msg.contains("不一致"), "{}", error.msg);

    // ② 物料不在协议范围
    let out = material_out().await;
    let error = create_with(AG_MAIN, out, "1", "10")
        .await
        .expect_err("协议外物料必须拒绝");
    assert_eq!(error.code, error_code::ERP_FWA_NOT_APPLICABLE, "{}", error.msg);
    assert!(
        error.msg.contains("第 1 行") && error.msg.contains("物料范围"),
        "报文必须点出行号与「不在物料范围」：{}",
        error.msg
    );
    println!("[M4-08] 范围 供应商 / 物料范围校验 ✓");
}

/// 详情出参带协议引用（前端 M4-11 要据此显示「按哪份协议下单」）。
#[tokio::test]
async fn detail_exposes_agreement_reference() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mat = material_b().await;
    let id = create_with(AG_MAIN, mat, "2", "20").await.expect("无上限物料可下单");
    let detail = purchase_handler::detail_of(id).await.expect("详情");
    let reference = detail.framework_agreement.expect("详情必须带协议引用");
    assert_eq!(reference.agreement_id, AG_MAIN);
    assert_eq!(reference.agreement_name.as_deref(), Some("M408-主协议"));
    assert_eq!(reference.start_date, Some(date("2020-01-01")));
    assert_eq!(reference.end_date, Some(date("2099-12-31")));
    assert_eq!(reference.price_currency.as_deref(), Some("CNY"));
    println!("[M4-08] 详情 协议引用出参 ✓");
}

/// 清场：`M408-` 夹具 0 残留（libtest 按名字排序，`zz_` 必然最后跑）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    cleanup().await;
    let left = decimal_of(
        &format!(
            "SELECT count(*)::numeric AS v FROM {} WHERE agreement_name LIKE '{PREFIX}%'",
            fwa::AGREEMENT_TABLE
        ),
        vec![],
    )
    .await;
    assert_eq!(left, Decimal::ZERO, "M408- 协议 0 残留");
    let orders = decimal_of(
        "SELECT count(*)::numeric AS v FROM biz_purchase_order \
          WHERE supplier_id IN (SELECT id FROM biz_supplier WHERE supplier_code LIKE 'M408-%')",
        vec![],
    )
    .await;
    assert_eq!(orders, Decimal::ZERO, "M408- 采购单 0 残留");
    println!("[M4-08] 清场 夹具 0 残留 ✓");
}

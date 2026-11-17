// ============================================================================
// framework_agreement_execution_db_tests.rs —— 框架协议「执行量统计」的真库用例（M4-09）
//
// 卡：t_645358e9（M4-09【M4·采购】框架协议：执行量统计 —— 已用 / 剩余）
//
// # 覆盖什么（每条都对应 V0452 头注释里的一句口径）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `fresh_agreement_reports_full_remaining` | 新协议：已用 0、剩余 = 封顶、率 0、NOT_STARTED、sources 空 |
// | `orders_accumulate_usage_and_rate` | 两张单累加：已用 50、剩余 50、率 0.5、PARTIAL、单据数 2、最近下单日 |
// | `cancelled_order_drops_out_of_usage` | 作废：已用回落、下钻带 `counted=false` + 「采购单已作废」 |
// | `deleted_order_drops_out_of_usage` | 软删：同上，原因「采购单已删除」 |
// | `over_limit_shows_negative_remaining` | 超上限（只有直接改库才造得出）：OVER、剩余为负、不许截成 0 |
// | `unlimited_item_reports_null_remaining` | 不限量：剩余 / 率是 **null**（不是 0），状态 UNLIMITED |
// | `head_cap_overrides_item_caps` | 单头 total_max_qty 封顶优先于「明细合计」 |
// | `amount_basis_follows_price_includes_tax` | 含税协议按 Σwith_tax 比、不含税按 Σamount 比 |
// | `head_totals_match_item_summary` | 单头视图合计与 `summary_of(明细)` 逐项相等 |
// | `validity_states_follow_status_and_dates` | DRAFT / SUBMITTED / NOT_STARTED / ACTIVE / EXPIRED + is_effective |
// | `overview_filters_and_hides_soft_deleted` | 总览筛选（关键字 / 供应商 / 状态 / 有效期 / 只看超限）+ 软删协议不进总览、详情 404 |
// | `zz_teardown_purges_fixtures` | 清场：`M409-` 夹具 0 残留 |
//
// # 夹具
//
// 协议两表没有 CRUD 接口（M4-07 只交表结构），所以协议夹具走裸 SQL；
// 采购单走**真实入口** `purchase_handler::create_purchase_order`（已用量的现算路径就是它写的行）——
// 只有「超上限」那条用 SQL 把 `max_qty` 改小（M4-08 会拦住超上限下单，现实里只可能是库被直接改过）。
// 主数据（供应商 / 物料 / 仓库）一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m409_exec" cargo test --lib \
//   business::erp::purchase::framework_agreement_execution -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::framework_agreement_execution_dto::{
    AgreementExecItemRes, AgreementExecRowRes, ExecutionFilter, STATE_NOT_STARTED, STATE_OVER, STATE_PARTIAL,
    STATE_UNLIMITED, UsageFacts, VALIDITY_ACTIVE, VALIDITY_DRAFT, VALIDITY_EXPIRED, VALIDITY_NOT_STARTED,
    VALIDITY_SUBMITTED, summary_of,
};
use crate::business::erp::purchase::framework_agreement_execution_handler as exec_handler;
use crate::business::erp::purchase::framework_agreement_model as fwa;
use crate::business::erp::purchase::purchase_dto::{
    CreatePurchaseOrderReq, PurchaseOrderItemReq, PurchaseOrderStatusReq,
};
use crate::business::erp::purchase::purchase_handler;
use crate::test_seed;

/// 夹具名前缀（清场按它反查，绝不按「生产号段」删）。
const PREFIX: &str = "M409-";
/// 夹具供应商编码。
const SUP_CODE: &str = "M409-SUP";
/// 夹具物料编码（有上限的那格）。
const MAT_A_CODE: &str = "M409-MATA";
/// 夹具物料编码（不限量的那格）。
const MAT_B_CODE: &str = "M409-MATB";

// ---------- 协议夹具（固定 UUID；setup() 每次重建）----------
/// 主夹具：不限税、无单头封顶；物料 A 上限 100 @10、物料 B 不限 @5。
const AG_MAIN: &str = "00000000-0000-4000-8000-000004090001";
/// 含税协议：11.30 含税（税率 13% ⇒ 不含税 10）/ 物料 A 上限 50。
const AG_TAX: &str = "00000000-0000-4000-8000-000004090002";
/// 单头数量封顶 50（明细不限量）：验「单头封顶优先」。
const AG_TOTAL: &str = "00000000-0000-4000-8000-000004090003";
/// 无单头封顶、明细全有上限（40 + 60 = 100）：验「明细合计兜底」。
const AG_ITEMSUM: &str = "00000000-0000-4000-8000-000004090004";
/// 草稿协议（有效期状态 DRAFT，不生效）。
const AG_DRAFT: &str = "00000000-0000-4000-8000-000004090005";
/// 已过期协议（APPROVED 但效期在 2020 年）。
const AG_EXPIRED: &str = "00000000-0000-4000-8000-000004090006";
/// 未生效协议（APPROVED 但 2099 年才生效）。
const AG_FUTURE: &str = "00000000-0000-4000-8000-000004090007";
/// 已软删协议（总览与详情都不许出现）。
const AG_DELETED: &str = "00000000-0000-4000-8000-000004090008";
/// 单头金额封顶 100（明细不限量）：验「金额封顶」与不含税口径。
const AG_AMOUNT: &str = "00000000-0000-4000-8000-000004090009";
/// 审批中协议（有效期状态 SUBMITTED）。
const AG_SUBMITTED: &str = "00000000-0000-4000-8000-00000409000a";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 夹具 UUID 字面量 → `Uuid`（`?::uuid` 位置必须绑 uuid 类型，绑 Text 会
/// `error serializing parameter` —— 与 M4-07/M4-08 用例踩过的坑同源）。
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

/// 清场：采购单（明细 CASCADE）→ 协议（物料行 CASCADE）→ 物料单位复位。
async fn cleanup() {
    let sup = test_seed::ensure_supplier(SUP_CODE, "M409 配件供应商").await;
    exec(
        "DELETE FROM biz_purchase_order WHERE supplier_id = ?::uuid \
            OR framework_agreement_no_snapshot LIKE 'FWA-19990101-9409%'"
            .to_string()
            .as_str(),
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
    // 物料夹具：上一轮可能留下「单位不是基本单位」的行（`test_seed::upsert_material` 是
    // ON CONFLICT DO NOTHING）→ 每轮强制把单位改回基本单位，免得建单被 66030 误拦。
    let base_unit = test_seed::ensure_base_unit().await;
    exec(
        "UPDATE biz_material SET unit_id = ?::uuid WHERE material_code LIKE 'M409-%'",
        vec![Value::from(&base_unit)],
    )
    .await;
}

async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUP_CODE, "M409 配件供应商").await
}

async fn material_a() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_A_CODE, "M409 物料甲（限量）").await
}

async fn material_b() -> Uuid {
    test_seed::ensure_material_base_unit(MAT_B_CODE, "M409 物料乙（不限量）").await
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse("M409-WH", "M409 收货仓").await
}

/// 一张协议 + 若干物料行（`(行号, 物料, 协议价, 税率, 数量上限)`）。
struct AgreementSeed {
    id: &'static str,
    no: &'static str,
    name: &'static str,
    status: &'static str,
    start: &'static str,
    end: &'static str,
    includes_tax: bool,
    total_max_qty: Option<&'static str>,
    total_max_amount: Option<&'static str>,
    deleted: bool,
    with_instance: bool,
}

impl AgreementSeed {
    fn new(id: &'static str, no: &'static str, name: &'static str, status: &'static str) -> Self {
        Self {
            id,
            no,
            name,
            status,
            start: "2020-01-01",
            end: "2099-12-31",
            includes_tax: false,
            total_max_qty: None,
            total_max_amount: None,
            deleted: false,
            with_instance: false,
        }
    }
}

async fn insert_agreement(seed: &AgreementSeed, supplier_id: Uuid, lines: &[(i32, Uuid, &str, &str, Option<&str>)]) {
    exec(
        &format!(
            "INSERT INTO {0} (id, agreement_no, agreement_name, supplier_id, supplier_name, \
                              start_date, end_date, sign_date, price_mode, price_includes_tax, \
                              price_currency, price_tolerance, total_max_qty, total_max_amount, \
                              status, approval_instance_id, deleted_at) \
             VALUES (?::uuid, ?::text, ?::text, ?::uuid, ?::text, ?::text::date, ?::text::date, \
                     ?::text::date, 'FIXED', ?::text::boolean, 'CNY', 0.05, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?::text, ?::uuid, \
                     CASE WHEN ?::text::boolean THEN now() ELSE NULL END)",
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
            Value::Text(seed.total_max_qty.unwrap_or_default().to_string()),
            Value::Text(seed.total_max_amount.unwrap_or_default().to_string()),
            Value::Text(seed.status.to_string()),
            match seed.with_instance {
                true => Value::from(&Uuid::now_v7()),
                false => Value::Null,
            },
            Value::Text(seed.deleted.to_string()),
        ],
    )
    .await;
    for (line_no, material_id, agreed_price, tax_rate, max_qty) in lines {
        exec(
            &format!(
                "INSERT INTO {0} (agreement_id, line_no, material_id, unit_id, agreed_price, \
                                  tax_rate, max_qty) \
                 VALUES (?::uuid, ?, ?::uuid, (SELECT unit_id FROM biz_material WHERE id = ?::uuid), \
                         ?::text::numeric, ?::text::numeric, NULLIF(?::text,'')::numeric)",
                fwa::ITEM_TABLE
            ),
            vec![
                Value::from(&uid(seed.id)),
                Value::from(*line_no),
                Value::from(material_id),
                Value::from(material_id),
                Value::Text(agreed_price.to_string()),
                Value::Text(tax_rate.to_string()),
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
    warehouse().await;

    // 主夹具：A 上限 100 @10，B 不限 @5
    let main_lines = [
        (1_i32, mat_a, "10.000000", "0.13", Some("100")),
        (2, mat_b, "5.000000", "0.13", None),
    ];
    insert_agreement(
        &AgreementSeed::new(AG_MAIN, "FWA-19990101-940901", "M409-主协议", fwa::STATUS_APPROVED),
        sup,
        &main_lines,
    )
    .await;

    // 含税协议：A 上限 50 @11.30 含税
    let tax_lines = [(1_i32, mat_a, "11.300000", "0.13", Some("50"))];
    let mut tax = AgreementSeed::new(AG_TAX, "FWA-19990101-940902", "M409-含税协议", fwa::STATUS_APPROVED);
    tax.includes_tax = true;
    insert_agreement(&tax, sup, &tax_lines).await;

    // 单头数量封顶 50（明细不限量）
    let total_lines = [(1_i32, mat_a, "10.000000", "0.13", None)];
    let mut total = AgreementSeed::new(AG_TOTAL, "FWA-19990101-940903", "M409-单头封顶", fwa::STATUS_APPROVED);
    total.total_max_qty = Some("50");
    insert_agreement(&total, sup, &total_lines).await;

    // 无单头封顶、明细全有上限（40 + 60 = 100）
    let itemsum_lines = [
        (1_i32, mat_a, "10.000000", "0.13", Some("40")),
        (2, mat_b, "5.000000", "0.13", Some("60")),
    ];
    insert_agreement(
        &AgreementSeed::new(
            AG_ITEMSUM,
            "FWA-19990101-940904",
            "M409-明细合计封顶",
            fwa::STATUS_APPROVED,
        ),
        sup,
        &itemsum_lines,
    )
    .await;

    // 草稿 / 过期 / 未生效 / 已软删 / 审批中
    insert_agreement(
        &AgreementSeed::new(AG_DRAFT, "FWA-19990101-940905", "M409-草稿协议", fwa::STATUS_DRAFT),
        sup,
        &main_lines,
    )
    .await;
    let mut expired = AgreementSeed::new(AG_EXPIRED, "FWA-19990101-940906", "M409-过期协议", fwa::STATUS_APPROVED);
    expired.start = "2020-01-01";
    expired.end = "2020-12-31";
    insert_agreement(&expired, sup, &main_lines).await;
    let mut future = AgreementSeed::new(
        AG_FUTURE,
        "FWA-19990101-940907",
        "M409-未生效协议",
        fwa::STATUS_APPROVED,
    );
    future.start = "2099-01-01";
    future.end = "2099-12-31";
    insert_agreement(&future, sup, &main_lines).await;
    let mut deleted = AgreementSeed::new(AG_DELETED, "FWA-19990101-940908", "M409-已删协议", fwa::STATUS_APPROVED);
    deleted.deleted = true;
    insert_agreement(&deleted, sup, &main_lines).await;
    let mut submitted = AgreementSeed::new(
        AG_SUBMITTED,
        "FWA-19990101-940909",
        "M409-审批中协议",
        fwa::STATUS_SUBMITTED,
    );
    submitted.with_instance = true;
    insert_agreement(&submitted, sup, &main_lines).await;

    // 单头金额封顶 100（明细不限量，不含税口径）
    let amount_lines = [(1_i32, mat_a, "10.000000", "0.13", None)];
    let mut amount = AgreementSeed::new(AG_AMOUNT, "FWA-19990101-940910", "M409-金额封顶", fwa::STATUS_APPROVED);
    amount.total_max_amount = Some("100");
    insert_agreement(&amount, sup, &amount_lines).await;
}

/// 建单请求（`EXCL` 口径 + 指定协议）。
fn order_req(
    agreement: &str,
    supplier_id: Uuid,
    warehouse_id: Uuid,
    lines: &[(Uuid, &str, &str)],
) -> CreatePurchaseOrderReq {
    CreatePurchaseOrderReq {
        supplier_id: Some(supplier_id.to_string()),
        warehouse_id: Some(warehouse_id.to_string()),
        purchase_type: Some("SPOT".to_string()),
        contract_id: None,
        framework_agreement_id: Some(agreement.to_string()),
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

/// 走真实建单入口下一张挂协议的采购单（返回采购单 id）。
async fn place(agreement: &str, material: Uuid, qty: &str, price: &str) -> Uuid {
    let req = order_req(
        agreement,
        supplier().await,
        warehouse().await,
        &[(material, qty, price)],
    );
    purchase_handler::create_purchase_order(&req, None)
        .await
        .unwrap_or_else(|error| panic!("建单失败（{agreement} / {qty} @ {price}）：{error:?}"))
}

/// 读单头执行量（走 handler 的 `page_of` 之外的 model 路径：`execution_of` 需要协议存在）。
async fn head(agreement: &str) -> AgreementExecRowRes {
    exec_handler::execution_of(uid(agreement))
        .await
        .unwrap_or_else(|error| panic!("取执行量详情失败（{agreement}）：{error:?}"))
        .agreement
}

/// 读逐行执行量。
async fn items(agreement: &str) -> Vec<AgreementExecItemRes> {
    exec_handler::execution_of(uid(agreement))
        .await
        .unwrap_or_else(|error| panic!("取执行量详情失败（{agreement}）：{error:?}"))
        .items
}

fn filter(keyword: Option<&str>) -> ExecutionFilter {
    ExecutionFilter {
        page: 1,
        page_size: 50,
        keyword: keyword.map(str::to_string),
        supplier_id: None,
        status: None,
        validity_state: None,
        over_only: false,
    }
}

/// 总览里是否出现某个协议号。
async fn in_overview(filter: &ExecutionFilter, agreement_no: &str) -> bool {
    let (rows, _total) = exec_handler::page_of(filter).await.expect("总览查询");
    rows.iter().any(|row| row.agreement_no == agreement_no)
}

// ==================== 用例 ====================

/// 新协议：已用 0、剩余 = 封顶（明细合计）、率 0、NOT_STARTED、下钻为空。
#[tokio::test]
async fn fresh_agreement_reports_full_remaining() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 新协议");
    let head = head(AG_ITEMSUM).await;
    assert_eq!(head.executed_qty, Decimal::ZERO, "新协议已用量必须是 0");
    assert_eq!(head.cap_qty, Some(dec("100")), "无单头封顶时按明细合计（40+60）");
    assert_eq!(head.remaining_qty, Some(dec("100")), "剩余 = 封顶 − 已用");
    assert_eq!(head.exec_qty_rate, Some(Decimal::ZERO), "执行率 0");
    assert_eq!(head.exec_qty_rate_pct, Some(Decimal::ZERO));
    assert_eq!(head.exec_state, STATE_NOT_STARTED);
    assert_eq!(head.item_count, 2);
    assert_eq!(head.not_started_items, 2);
    assert_eq!(head.order_doc_count, 0);
    assert_eq!(head.booked_total_qty, Decimal::ZERO, "回写列也应是 0（没下过单）");
    // 行级：两行都 NOT_STARTED，剩余 = 各自上限
    let rows = items(AG_ITEMSUM).await;
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.exec_state, STATE_NOT_STARTED);
        assert_eq!(row.executed_qty, Decimal::ZERO);
        assert_eq!(row.remaining_qty, row.max_qty, "剩余 = max_qty − 0");
    }
    // 详情：没有来源单据
    let detail = exec_handler::execution_of(uid(AG_ITEMSUM)).await.expect("详情");
    assert!(detail.sources.is_empty(), "没下过单 → sources 空");
}

/// 两张单累加：已用 50、剩余 50、率 0.5、PARTIAL、单据数 2、最近下单日。
#[tokio::test]
async fn orders_accumulate_usage_and_rate() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 累加");
    let mat = material_a().await;
    place(AG_ITEMSUM, mat, "20", "10").await;
    place(AG_ITEMSUM, mat, "10", "10").await;

    let head = head(AG_ITEMSUM).await;
    assert_eq!(head.executed_qty, dec("30"), "20 + 10");
    assert_eq!(head.remaining_qty, Some(dec("70")), "100 − 30");
    assert_eq!(head.exec_qty_rate, Some(dec("0.3")));
    assert_eq!(head.exec_qty_rate_pct, Some(dec("30.00")));
    assert_eq!(head.exec_state, STATE_PARTIAL);
    assert_eq!(head.order_doc_count, 2, "两张单都吃额度");
    assert_eq!(head.executed_amount, dec("300"), "30 × 10");
    assert_eq!(head.last_order_on.as_deref(), Some("2026-11-18"));
    // 回写列（M4-08 重算）与单据链一致 —— 不一致就是分叉
    assert_eq!(head.booked_total_qty, head.executed_qty);

    let detail = exec_handler::execution_of(uid(AG_ITEMSUM)).await.expect("详情");
    assert_eq!(detail.sources.len(), 2);
    assert!(detail.sources.iter().all(|s| s.counted), "两张单都计入");
    assert_eq!(detail.items[0].booked_drift, Decimal::ZERO, "回写与单据链不分叉");
    assert_eq!(detail.items[0].order_doc_count, 2);
}

/// 作废：已用回落，下钻保留这张单并标 `counted=false` + 「采购单已作废」。
#[tokio::test]
async fn cancelled_order_drops_out_of_usage() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 作废");
    let mat = material_a().await;
    place(AG_ITEMSUM, mat, "20", "10").await;
    let second = place(AG_ITEMSUM, mat, "10", "10").await;
    assert_eq!(head(AG_ITEMSUM).await.executed_qty, dec("30"));

    let status = PurchaseOrderStatusReq {
        status: Some("CANCELLED".to_string()),
        reason: Some("M409 用例：作废释放已用量".to_string()),
        ..Default::default()
    };
    purchase_handler::change_purchase_order_status(second, &status, None)
        .await
        .expect("作废采购单");

    let head = head(AG_ITEMSUM).await;
    assert_eq!(head.executed_qty, dec("20"), "作废单不该继续吃额度");
    assert_eq!(head.remaining_qty, Some(dec("80")));
    assert_eq!(head.order_doc_count, 1, "计入的只剩 1 张");
    assert_eq!(head.released_doc_count, 1, "作废的进「已释放」计数");
    let detail = exec_handler::execution_of(uid(AG_ITEMSUM)).await.expect("详情");
    assert_eq!(detail.sources.len(), 2, "下钻不过滤已作废单（否则用户以为量凭空少了）");
    let cancelled = detail
        .sources
        .iter()
        .find(|s| s.order_id == second.to_string())
        .expect("作废单必须出现在下钻里");
    assert!(!cancelled.counted);
    assert_eq!(cancelled.verdict, "采购单已作废");
    assert_eq!(cancelled.order_status, "CANCELLED");
}

/// 软删：已用回落，下钻保留这张单并标 `counted=false` + 「采购单已删除」。
#[tokio::test]
async fn deleted_order_drops_out_of_usage() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 软删");
    let mat = material_a().await;
    place(AG_ITEMSUM, mat, "20", "10").await;
    let second = place(AG_ITEMSUM, mat, "10", "10").await;
    purchase_handler::delete_purchase_order(second, None)
        .await
        .expect("软删采购单");

    let head = head(AG_ITEMSUM).await;
    assert_eq!(head.executed_qty, dec("20"), "软删单不该继续吃额度");
    assert_eq!(head.released_doc_count, 1);
    let detail = exec_handler::execution_of(uid(AG_ITEMSUM)).await.expect("详情");
    let deleted = detail
        .sources
        .iter()
        .find(|s| s.order_id == second.to_string())
        .expect("软删单必须出现在下钻里");
    assert!(!deleted.counted);
    assert_eq!(deleted.verdict, "采购单已删除");
    assert!(deleted.order_deleted_at.is_some());
}

/// 超上限：只有「库被直接改过」才造得出（M4-08 会拒新单）→ OVER + 负剩余。
#[tokio::test]
async fn over_limit_shows_negative_remaining() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 超限");
    let mat = material_a().await;
    place(AG_ITEMSUM, mat, "30", "10").await;
    // 把该行上限从 40 改成 20（直接改库 = 现实里「手工修数据 / 导入」造成的超限）
    exec(
        &format!(
            "UPDATE {} SET max_qty = 20 WHERE agreement_id = ?::uuid AND material_id = ?::uuid",
            fwa::ITEM_TABLE
        ),
        vec![Value::from(&uid(AG_ITEMSUM)), Value::from(&mat)],
    )
    .await;

    let rows = items(AG_ITEMSUM).await;
    let row = rows
        .iter()
        .find(|row| row.material_id == mat.to_string())
        .expect("限量行");
    assert_eq!(row.exec_state, STATE_OVER, "超上限必须显形，不许截成 DONE");
    assert_eq!(row.remaining_qty, Some(dec("-10")), "剩余是负数，不是 0");
    assert_eq!(row.exec_qty_rate, Some(dec("1.5")));
    let head = head(AG_ITEMSUM).await;
    assert_eq!(head.over_items, 1);
}

/// 不限量：剩余与执行率是 **null**（不是 0），状态 UNLIMITED；单头也不封顶。
#[tokio::test]
async fn unlimited_item_reports_null_remaining() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 不限量");
    let mat_b = material_b().await;
    place(AG_MAIN, mat_b, "10", "5").await;

    let rows = items(AG_MAIN).await;
    let row = rows
        .iter()
        .find(|row| row.material_id == mat_b.to_string())
        .expect("不限量行");
    assert_eq!(row.exec_state, STATE_UNLIMITED);
    assert_eq!(row.max_qty, None);
    assert_eq!(row.remaining_qty, None, "不限量的剩余是 null，不是 0");
    assert_eq!(row.exec_qty_rate, None);
    assert_eq!(row.exec_qty_rate_pct, None);
    assert_eq!(row.cap_amount, None);
    assert_eq!(row.executed_qty, dec("10"));

    // 单头：AG_MAIN 无单头封顶 + 有一行不限量 ⇒ 整体不封顶
    let head = head(AG_MAIN).await;
    assert_eq!(head.unlimited_items, 1);
    assert_eq!(head.cap_qty, None);
    assert_eq!(head.remaining_qty, None);
    assert_eq!(head.exec_state, STATE_UNLIMITED);
    assert_eq!(head.executed_amount, dec("50"), "10 × 5");
}

/// 单头封顶优先：明细不限量、单头 50 → 封顶取 50。
#[tokio::test]
async fn head_cap_overrides_item_caps() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 单头封顶");
    let mat = material_a().await;
    place(AG_TOTAL, mat, "20", "10").await;
    let head = head(AG_TOTAL).await;
    assert_eq!(head.cap_qty, Some(dec("50")), "单头 total_max_qty 优先");
    assert_eq!(head.remaining_qty, Some(dec("30")));
    assert_eq!(head.exec_qty_rate, Some(dec("0.4")));
    assert_eq!(head.exec_state, STATE_PARTIAL);
    assert_eq!(head.unlimited_items, 1, "明细行本身不限量");
}

/// 金额口径跟协议价走：含税协议按 Σwith_tax 比；不含税按 Σamount 比。
#[tokio::test]
async fn amount_basis_follows_price_includes_tax() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 金额口径");
    let mat = material_a().await;

    // 含税协议：协议价 11.30 含税，采购单行价 10 不含税 + 13% 税 ⇒ with_tax 113
    place(AG_TAX, mat, "10", "10").await;
    let rows = items(AG_TAX).await;
    let row = rows.first().expect("含税协议应有一行");
    assert_eq!(row.executed_amount, dec("100"), "不含税金额 10 × 10");
    assert_eq!(row.executed_amount_with_tax, dec("113"), "含税金额 100 × 1.13");
    assert_eq!(row.executed_amount_capped, dec("113"), "含税协议比含税口径");
    assert_eq!(row.cap_amount, Some(dec("565")), "50 × 11.30");

    // 金额封顶（不含税口径）：单头 100，已用 40 → 剩 60
    place(AG_AMOUNT, mat, "4", "10").await;
    let head = head(AG_AMOUNT).await;
    assert_eq!(head.cap_amount, Some(dec("100")));
    assert_eq!(head.executed_amount_capped, dec("40"));
    assert_eq!(head.remaining_amount, Some(dec("60")));
    assert_eq!(head.exec_amount_rate, Some(dec("0.4")));
    assert_eq!(head.exec_state, STATE_PARTIAL, "数量不封顶时按金额判状态");
}

/// 单头视图合计与 `summary_of(明细)` 逐项相等（两套聚合口径不许分叉）。
#[tokio::test]
async fn head_totals_match_item_summary() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 合计对拍");
    let mat_a = material_a().await;
    let mat_b = material_b().await;
    place(AG_ITEMSUM, mat_a, "25", "10").await;
    place(AG_ITEMSUM, mat_b, "60", "5").await;

    let head = head(AG_ITEMSUM).await;
    let rows = items(AG_ITEMSUM).await;
    let facts: Vec<UsageFacts> = rows
        .iter()
        .map(|row| UsageFacts {
            max_qty: row.max_qty,
            executed_qty: row.executed_qty,
            executed_amount_capped: row.executed_amount_capped,
        })
        .collect();
    let summary = summary_of(&facts);
    assert_eq!(head.item_count, summary.item_count);
    assert_eq!(head.unlimited_items, summary.unlimited_items);
    assert_eq!(head.done_items, summary.done_items);
    assert_eq!(head.partial_items, summary.partial_items);
    assert_eq!(head.over_items, summary.over_items);
    assert_eq!(head.not_started_items, summary.not_started_items);
    assert_eq!(head.executed_qty, summary.executed_qty);
    assert_eq!(head.executed_amount_capped, summary.executed_amount_capped);
    assert_eq!(head.cap_qty, summary.cap_qty);
    assert_eq!(head.remaining_qty, summary.remaining_qty);
    assert_eq!(head.exec_qty_rate, summary.exec_qty_rate);
    assert_eq!(head.exec_state, summary.exec_state);
    assert_eq!(summary.done_items, 1, "物料 B 用量 60 = 上限 60 ⇒ DONE");
}

/// 有效期状态：草稿 / 审批中 / 未生效 / 生效中 / 已过期 + is_effective。
#[tokio::test]
async fn validity_states_follow_status_and_dates() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 有效期");

    let draft = head(AG_DRAFT).await;
    assert_eq!(draft.validity_state, VALIDITY_DRAFT);
    assert!(!draft.is_effective, "草稿不许生效（有日期也不许）");

    let submitted = head(AG_SUBMITTED).await;
    assert_eq!(submitted.validity_state, VALIDITY_SUBMITTED);
    assert!(!submitted.is_effective);

    let future = head(AG_FUTURE).await;
    assert_eq!(future.validity_state, VALIDITY_NOT_STARTED);
    assert!(!future.is_effective);
    assert!(future.days_left > 0);

    let expired = head(AG_EXPIRED).await;
    assert_eq!(expired.validity_state, VALIDITY_EXPIRED);
    assert!(!expired.is_effective);
    assert!(expired.days_left < 0, "已过期 ⇒ 距截止日为负");

    let active = head(AG_MAIN).await;
    assert_eq!(active.validity_state, VALIDITY_ACTIVE);
    assert!(active.is_effective, "APPROVED + 在效期内 ⇒ 今天生效");
    assert_eq!(active.validity_state_label, "生效中");
}

/// 总览筛选（关键字 / 供应商 / 状态 / 有效期 / 只看超限）+ 软删协议不进总览、详情 404。
#[tokio::test]
async fn overview_filters_and_hides_soft_deleted() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 总览筛选");

    // 关键字：夹具前缀只命中本卡的 10 份
    let all = filter(Some(PREFIX));
    let (rows, total) = exec_handler::page_of(&all).await.expect("总览");
    assert_eq!(total, 9, "存活协议 9 份（AG_DELETED 已软删）");
    assert!(rows.iter().all(|row| row.agreement_name.starts_with(PREFIX)));
    assert!(
        !rows.iter().any(|row| row.agreement_no == "FWA-19990101-940908"),
        "软删协议不许进总览"
    );

    // 状态筛选
    let mut draft_only = filter(Some(PREFIX));
    draft_only.status = Some(fwa::STATUS_DRAFT.to_string());
    assert!(in_overview(&draft_only, "FWA-19990101-940905").await);
    assert!(!in_overview(&draft_only, "FWA-19990101-940901").await);

    // 有效期筛选
    let mut expired_only = filter(Some(PREFIX));
    expired_only.validity_state = Some(VALIDITY_EXPIRED.to_string());
    assert!(in_overview(&expired_only, "FWA-19990101-940906").await);
    assert!(!in_overview(&expired_only, "FWA-19990101-940907").await);

    // 供应商筛选
    let sup = supplier().await;
    let mut by_supplier = filter(None);
    by_supplier.supplier_id = Some(sup);
    let (rows, total) = exec_handler::page_of(&by_supplier).await.expect("按供应商");
    assert!(total >= 9, "本卡夹具协议全挂在同一供应商下（实际 {total}）");
    assert!(rows.iter().all(|row| row.supplier_id == sup.to_string()));

    // 只看超限：先把 AG_ITEMSUM 的行上限改小造一个 OVER
    let mat = material_a().await;
    place(AG_ITEMSUM, mat, "30", "10").await;
    exec(
        &format!(
            "UPDATE {} SET max_qty = 20 WHERE agreement_id = ?::uuid AND material_id = ?::uuid",
            fwa::ITEM_TABLE
        ),
        vec![Value::from(&uid(AG_ITEMSUM)), Value::from(&mat)],
    )
    .await;
    let mut over_only = filter(Some(PREFIX));
    over_only.over_only = true;
    let (rows, _total) = exec_handler::page_of(&over_only).await.expect("只看超限");
    assert_eq!(rows.len(), 1, "只有 AG_ITEMSUM 有超限行");
    assert_eq!(rows[0].agreement_id, AG_ITEMSUM);
    assert_eq!(rows[0].over_items, 1);

    // 详情 404：软删协议
    let error = exec_handler::execution_of(uid(AG_DELETED))
        .await
        .expect_err("软删协议的详情必须 404");
    assert_eq!(error.status, 404);
}

/// 清场：`M409-` 夹具 0 残留。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    println!("[M4-09] 清场");
    let mat = material_a().await;
    place(AG_ITEMSUM, mat, "10", "10").await;
    cleanup().await;

    let rows = db()
        .find_with_params(
            &format!(
                "SELECT count(*) AS v FROM {} WHERE agreement_name LIKE '{PREFIX}%'",
                fwa::AGREEMENT_TABLE
            ),
            vec![],
        )
        .await
        .expect("统计协议残留");
    let left = rows
        .first()
        .and_then(|row| row.get_typed::<i64>("v").ok())
        .unwrap_or(-1);
    assert_eq!(left, 0, "M409- 协议必须 0 残留");

    let rows = db()
        .find_with_params(
            "SELECT count(*) AS v FROM biz_purchase_order WHERE remark LIKE 'M409-%'",
            vec![],
        )
        .await
        .expect("统计采购单残留");
    let left = rows
        .first()
        .and_then(|row| row.get_typed::<i64>("v").ok())
        .unwrap_or(-1);
    assert_eq!(left, 0, "M409- 采购单必须 0 残留");
}

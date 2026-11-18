// ============================================================================
// contract_execution_db_tests.rs —— 合同「执行量统计」的真库用例（M4-19）
//
// 卡：t_08ae874c（M4-19【M4·采购】合同执行量统计 —— 已执行 / 合同额）
//
// # 覆盖什么（每条都对应 V0458 头注释里的一句口径）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `fresh_contract_reports_full_remaining` | 新合同：已执行 0、剩余 = 合同额、率 0、NOT_STARTED、sources 空 |
// | `purchase_orders_accumulate_executed_amount` | 两张单累加：已执行 = Σ 含税额、剩余、率、PARTIAL、单据数、最近执行日 |
// | `cancelled_order_releases_amount` | 作废：已执行回落、下钻带 `counted=false` + 「单据已作废」 |
// | `deleted_order_releases_amount` | 软删（走真实删除入口）：同上，原因「单据已删除」 |
// | `boundary_exact_amount_is_done` | 边界：已执行 = 合同额 ⇒ DONE / 率 1 / 剩余 0（不是 OVER） |
// | `over_contract_shows_negative_remaining` | 超合同额（只有直接改库才造得出）：OVER、剩余为负、率 > 1，不许截成 0 |
// | `sales_contract_counts_sales_orders_only` | 销售合同看销售单；采购单再多也不串到销售合同上 |
// | `service_and_lease_have_no_downstream` | SERVICE / LEASE 是 NO_DOWNSTREAM，不是「未开工」 |
// | `soft_deleted_contract_hidden_and_detail_404` | 软删合同不进总览、详情 404 |
// | `currency_mismatch_is_visible_but_still_counted` | 币种串了：照计入（与守卫同口径）+ `currency_mismatch_doc_count` 显形 |
// | `overview_filters_and_ordering` | 总览筛选（类型 / 状态 / 执行状态 / 只看超限 / 只看过期 / 只看币种串 / 关键字）+ 排序 |
// | `zz_teardown_purges_fixtures` | 清场：`M419-` 夹具 0 残留 |
//
// # 夹具
//
// 「合同」与「主数据」走 `crate::test_seed::ensure_*` / 裸 SQL（合同表有 CRUD 接口，但
// handler 要 `Extension<AuthClaims>`，真库用例拿不到登录态；本卡是**只读报表**，
// 合同行只需要「合法存在」—— `ck_biz_contract_*` 九条 CHECK 在库层兜着）；
// 「单据」走**真实入口**：采购单 `purchase_handler::create_purchase_order` /
// 作废 `change_purchase_order_status` / 删除 `delete_purchase_order`，
// 销售单 `sales_order_handler::create_sales_order` —— 已执行额的现算路径就是它们写的行。
// 只有两处用 SQL 改库，且都在用例注释里写明「现实里只能靠改库造出来」：
// ① 把合同额改小造超限；② 插一张 USD 采购单造币种串了（建单入口不收币种）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m419_exec" cargo test --lib \
//   business::erp::contract::contract_execution -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::contract::contract_execution_dto::{
    DOWNSTREAM_PURCHASE, DOWNSTREAM_SALES, ExecutionPageQuery, STATE_DONE, STATE_NO_DOWNSTREAM, STATE_NOT_STARTED,
    STATE_OVER, STATE_PARTIAL,
};
use crate::business::erp::contract::contract_execution_handler as exec_handler;
use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db as shared_ensure_db, exec};
use crate::business::erp::purchase::purchase_dto::{
    CreatePurchaseOrderReq, PurchaseOrderItemReq, PurchaseOrderStatusReq,
};
use crate::business::erp::purchase::purchase_handler;
use crate::business::erp::sales::sales_order_handler::{self, CreateSalesOrderReq, SalesOrderItemReq};
use crate::test_seed;

/// 夹具前缀（清场按它反查，绝不按「生产号段」删）。
const PREFIX: &str = "M419-";

/// 夹具供应商 / 客户 / 物料 / 仓库编码。
const SUP_CODE: &str = "M419-SUP";
const CUS_CODE: &str = "M419-CUS";
const MAT_CODE: &str = "M419-MAT";
const WH_CODE: &str = "M419-WH";

// ---------- 合同夹具（固定 UUID；setup() 每次重建内容一致的夹具） ----------
/// 采购合同 1000：累加 / 作废 / 软删 / 超限四条路径都用它。
const C_PUR: &str = "00000000-0000-4000-8000-000004190001";
/// 采购合同 22.60：与一张标准采购单（10 × 2.00 + 13%）**正好相等**，验 DONE 边界。
const C_PUR_FULL: &str = "00000000-0000-4000-8000-000004190002";
/// 销售合同 500：验「销售合同只看销售单」。
const C_SALES: &str = "00000000-0000-4000-8000-000004190003";
/// 服务合同 100：无下游单据。
const C_SERVICE: &str = "00000000-0000-4000-8000-000004190004";
/// 租赁合同 100：同上。
const C_LEASE: &str = "00000000-0000-4000-8000-000004190005";
/// 已软删采购合同 700：不许进总览 / 详情 404。
const C_DELETED: &str = "00000000-0000-4000-8000-000004190006";
/// 已过期采购合同 100（`end_date` = 昨天）。
const C_EXPIRED: &str = "00000000-0000-4000-8000-000004190007";
/// 币种串了的采购合同 1000（CNY 合同挂一张 USD 单）。
const C_CURRENCY: &str = "00000000-0000-4000-8000-000004190008";

/// 标准采购单（10 × 2.00 + 13%）的含税金额 = 22.60（与 M4-18 夹具同口径）。
const ORDER_WITH_TAX: &str = "22.60";
/// 第二张采购单（5 × 2.00 + 13%）= 11.30。
const ORDER2_WITH_TAX: &str = "11.30";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

/// 夹具 UUID 字面量 → `Uuid`（`?::uuid` 位置必须绑 uuid 类型，绑 Text 会
/// `error serializing parameter` —— M4-07/08/09 都踩过）。
fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 字面量")
}

fn today() -> NaiveDate {
    nik::prelude::chrono::Utc::now().date_naive()
}

/// `today() ± days`（`NaiveDate` 没有 `Add<i32>`，统一走 `TimeDelta`）。
fn days_from_today(days: i64) -> NaiveDate {
    today() + nik::prelude::chrono::TimeDelta::days(days)
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db_or_panic().await {
        return None;
    }
    cleanup().await;
    seed_contracts().await;
    Some(guard)
}

/// 真库就绪判定：**设了 `ERP_DB_URL` 却连不上 ⇒ 直接 panic**。
///
/// 仓里通用的 `ensure_db()` 在连不上时只打印一行 `SKIP:` 然后返回 `false`，
/// 用例随即 `return` —— 在 cargo 眼里那是 **ok**。对真库用例来说这是"假绿"：
/// 连接串写错 / 库没起来 / 连到别人的库，全都会静默变成「12 条用例都通过」。
/// 本文件把这条堵死：**没配** `ERP_DB_URL` 才允许跳过（本机不想连库的场景）；
/// **配了**却连不上必须红，让人看见。
async fn ensure_db_or_panic() -> bool {
    let configured = std::env::var("ERP_DB_URL").is_ok();
    let ready = shared_ensure_db().await;
    if configured && !ready {
        panic!("ERP_DB_URL 已设置但连不上库：拒绝静默 SKIP 假绿（连接串错 / 库没起来 / 库名不存在都要红）");
    }
    ready
}

/// 本夹具的供应商 id。
async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUP_CODE, "M419 执行量供应商").await
}

/// 本夹具的客户 id。
async fn customer() -> Uuid {
    test_seed::ensure_customer(CUS_CODE, "M419 执行量客户").await
}

/// 本夹具的物料 id。
///
/// ⚠ 必须挂**基本单位**：`unit_guard` 只认 `is_base = 1`（本版本不支持多单位换算），
/// 而 `ensure_unit` 挑的是「第一个启用单位」，模板库里那可能是「箱」。另外
/// `upsert_material` 是 `ON CONFLICT DO NOTHING`，所以这里显式把单位**校正**回基本单位
/// （否则一条早先失败的用例留下的非基本单位会让本文件所有用例连环红）。
async fn material() -> Uuid {
    let id = test_seed::ensure_material_base_unit(MAT_CODE, "M419 执行量物料").await;
    let unit = test_seed::ensure_base_unit().await;
    exec(
        "UPDATE biz_material SET unit_id = ?::uuid, deleted_at = NULL WHERE id = ?::uuid",
        vec![Value::from(&unit), Value::from(&id)],
    )
    .await;
    id
}

/// 本夹具的仓库 id。
async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse(WH_CODE, "M419 执行量仓").await
}

/// 清场：单据（按**夹具合同 id** 反查）→ 合同（按合同号前缀）。主数据由 `test_seed` 复用，不删。
///
/// ⚠ 不能按单号前缀删：单据走真实入口（`create_purchase_order` / `create_sales_order`），
/// 单号是**生产的 `PO-…` / `SO-…` 号段**，不会带夹具前缀。夹具合同 id 是唯一可靠判据
/// （合同是固定 UUID，单据的 `contract_id` 指向它）—— 这也保证本文件不会误删别人的行。
async fn cleanup() {
    let ids = fixture_contract_ids();
    exec(
        &format!("UPDATE biz_purchase_order SET approval_instance_id = NULL WHERE contract_id IN ({ids})"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_purchase_order WHERE contract_id IN ({ids})"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_sales_order WHERE contract_id IN ({ids})"),
        vec![],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_contract WHERE contract_no LIKE '{PREFIX}%'"),
        vec![],
    )
    .await;
}

/// 夹具合同 id 的 SQL 字面量清单（`'…'::uuid, '…'::uuid, …`）。
fn fixture_contract_ids() -> String {
    [
        C_PUR, C_PUR_FULL, C_SALES, C_SERVICE, C_LEASE, C_DELETED, C_EXPIRED, C_CURRENCY,
    ]
    .iter()
    .map(|id| format!("'{id}'::uuid"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// 重建合同夹具（内容每次一致；已存在的先删掉再插，避免上一轮残留状态影响断言）。
async fn seed_contracts() {
    let sup = supplier().await;
    let cus = customer().await;
    let far = days_from_today(365);
    let near = days_from_today(-30);
    let yesterday = days_from_today(-1);

    // 采购合同（1000）：ACTIVE，效期覆盖今天
    exec(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status)
             VALUES ('{C_PUR}'::uuid, '{PREFIX}HT-PUR', 'M419 采购合同 1000', 'PURCHASE', 'SUPPLIER',
                     '{sup}'::uuid, 'M419 执行量供应商', 1000.0000, 'CNY',
                     '{near}', '{near}', '{far}', 'ACTIVE')"
        ),
        vec![],
    )
    .await;
    // 采购合同（22.60）：与标准采购单含税额正好相等
    exec(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status)
             VALUES ('{C_PUR_FULL}'::uuid, '{PREFIX}HT-FULL', 'M419 采购合同 22.60', 'PURCHASE', 'SUPPLIER',
                     '{sup}'::uuid, 'M419 执行量供应商', {ORDER_WITH_TAX}, 'CNY',
                     '{near}', '{near}', '{far}', 'ACTIVE')"
        ),
        vec![],
    )
    .await;
    // 销售合同（500）
    exec(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status)
             VALUES ('{C_SALES}'::uuid, '{PREFIX}HT-SAL', 'M419 销售合同 500', 'SALES', 'CUSTOMER',
                     '{cus}'::uuid, 'M419 执行量客户', 500.0000, 'CNY',
                     '{near}', '{near}', '{far}', 'ACTIVE')"
        ),
        vec![],
    )
    .await;
    // 服务合同 / 租赁合同（无下游单据）
    exec(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status)
             VALUES ('{C_SERVICE}'::uuid, '{PREFIX}HT-SRV', 'M419 服务合同 100', 'SERVICE', 'SUPPLIER',
                     '{sup}'::uuid, 'M419 执行量供应商', 100.0000, 'CNY',
                     '{near}', '{near}', '{far}', 'ACTIVE'),
                    ('{C_LEASE}'::uuid, '{PREFIX}HT-LEA', 'M419 租赁合同 100', 'LEASE', 'SUPPLIER',
                     '{sup}'::uuid, 'M419 执行量供应商', 100.0000, 'CNY',
                     '{near}', '{near}', '{far}', 'ACTIVE')"
        ),
        vec![],
    )
    .await;
    // 已软删采购合同（700）
    exec(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status, deleted_at)
             VALUES ('{C_DELETED}'::uuid, '{PREFIX}HT-DEL', 'M419 采购合同（已删）', 'PURCHASE', 'SUPPLIER',
                     '{sup}'::uuid, 'M419 执行量供应商', 700.0000, 'CNY',
                     '{near}', '{near}', '{far}', 'ACTIVE', now())"
        ),
        vec![],
    )
    .await;
    // 已过期采购合同（100，end_date = 昨天）
    exec(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status)
             VALUES ('{C_EXPIRED}'::uuid, '{PREFIX}HT-EXP', 'M419 采购合同（已过期）', 'PURCHASE', 'SUPPLIER',
                     '{sup}'::uuid, 'M419 执行量供应商', 100.0000, 'CNY',
                     '{near}', '{near}', '{yesterday}', 'ACTIVE')"
        ),
        vec![],
    )
    .await;
    // 币种串了的采购合同（CNY 合同，稍后挂一张 USD 单）
    exec(
        &format!(
            "INSERT INTO biz_contract (id, contract_no, contract_name, contract_type, party_b_type,
                                       party_b_id, party_b_name, amount, currency_code,
                                       sign_date, start_date, end_date, status)
             VALUES ('{C_CURRENCY}'::uuid, '{PREFIX}HT-CUR', 'M419 采购合同（币种对照）', 'PURCHASE', 'SUPPLIER',
                     '{sup}'::uuid, 'M419 执行量供应商', 1000.0000, 'CNY',
                     '{near}', '{near}', '{far}', 'ACTIVE')"
        ),
        vec![],
    )
    .await;
}

// ==================== 造单（全部走真实入口） ====================

/// 在指定采购合同上建一张采购单（10 × 2.00 + 13% ⇒ 含税 22.60），返回单 id。
async fn create_purchase_on(contract: &str) -> Uuid {
    create_purchase_with(contract, "10").await
}

/// 在指定采购合同上建一张采购单（`qty` × 2.00 + 13%），返回单 id。
async fn create_purchase_with(contract: &str, qty: &str) -> Uuid {
    let request = CreatePurchaseOrderReq {
        supplier_id: None, // CONTRACT：供应商由合同乙方派生
        warehouse_id: Some(warehouse().await.to_string()),
        purchase_type: Some("CONTRACT".to_string()),
        contract_id: Some(contract.to_string()),
        order_date: Some(today()),
        items: vec![PurchaseOrderItemReq {
            material_id: Some(material().await.to_string()),
            qty: Some(dec(qty)),
            price_excl: Some(dec("2")),
            tax_rate: Some(dec("13")),
            ..Default::default()
        }],
        ..Default::default()
    };
    purchase_handler::create_purchase_order(&request, None)
        .await
        .expect("合同采购单应建成功")
}

/// 采购单的价税合计（从真实详情读回来，不自己算）。
async fn purchase_with_tax(id: Uuid) -> Decimal {
    purchase_handler::detail_of(id)
        .await
        .expect("查采购单详情")
        .order
        .total_with_tax
}

/// 作废一张采购单（真实状态流转：DRAFT → CANCELLED）。
async fn cancel_purchase(id: Uuid) {
    let req = PurchaseOrderStatusReq {
        status: Some("CANCELLED".to_string()),
        reason: Some("M419 用例：作废后应释放合同额度".to_string()),
        ..Default::default()
    };
    purchase_handler::change_purchase_order_status(id, &req, None)
        .await
        .expect("草稿采购单应能作废");
}

/// 在指定销售合同上建一张销售单（`qty` × 5.00，税率 0），返回 (id, 含税合计)。
async fn create_sales_on(contract: &str, qty: &str) -> (Uuid, Decimal) {
    let request = CreateSalesOrderReq {
        customer_id: customer().await.to_string(),
        contract_id: Some(contract.to_string()),
        order_date: Some(today().to_string()),
        items: vec![SalesOrderItemReq {
            material_id: material().await.to_string(),
            qty: Some(dec(qty)),
            price: Some(dec("5")),
            tax_rate: Some(Decimal::ZERO),
        }],
        ..Default::default()
    };
    let created = sales_order_handler::create_sales_order(None, request)
        .await
        .expect("销售单应建成功");
    (uid(&created.id), created.total_with_tax)
}

/// 取某合同的执行量单头（走 handler 的真实代码路径）。
async fn head(contract: &str) -> crate::business::erp::contract::contract_execution_dto::ContractExecRowRes {
    exec_handler::execution_of(uid(contract))
        .await
        .expect("合同执行量详情")
        .contract
}

/// 总览里按合同 id 找一行（找不到 = None）。
async fn row_in_page(
    query: ExecutionPageQuery,
    contract: &str,
) -> Option<crate::business::erp::contract::contract_execution_dto::ContractExecRowRes> {
    let filter = query.filter().expect("筛选参数合法");
    let (rows, _total) = exec_handler::page_of(&filter).await.expect("总览查询");
    let target = uid(contract);
    rows.into_iter().find(|row| row.contract_id == target.to_string())
}

// ==================== 用例 ====================

/// ① 新合同：已执行 0、剩余 = 合同额、率 0、NOT_STARTED、sources 空。
#[tokio::test]
async fn fresh_contract_reports_full_remaining() {
    let Some(_guard) = setup().await else { return };
    let detail = exec_handler::execution_of(uid(C_PUR)).await.expect("详情");
    assert_eq!(detail.contract.executed_amount, Decimal::ZERO);
    assert_eq!(detail.contract.executed_amount_net, Decimal::ZERO);
    assert_eq!(detail.contract.remaining_amount, dec("1000"));
    assert_eq!(detail.contract.exec_amount_rate, Some(Decimal::ZERO));
    assert_eq!(detail.contract.exec_amount_rate_pct, Some(Decimal::ZERO));
    assert_eq!(detail.contract.exec_state, STATE_NOT_STARTED);
    assert_eq!(detail.contract.exec_state_label, "未开工");
    assert_eq!(detail.contract.order_doc_count, 0);
    assert_eq!(detail.contract.downstream_type.as_deref(), Some(DOWNSTREAM_PURCHASE));
    assert!(detail.sources.is_empty(), "一张单都没挂时 sources 必须是空数组");
    assert_eq!(
        (detail.source_count, detail.counted_count, detail.released_count),
        (0, 0, 0)
    );
    println!("[M4-19] 新合同：剩余 = 合同额 1000、NOT_STARTED ✓");
}

/// ② 两张单累加：已执行 = Σ 含税额，剩余 / 率 / PARTIAL / 单据数 / 最近执行日逐项对。
#[tokio::test]
async fn purchase_orders_accumulate_executed_amount() {
    let Some(_guard) = setup().await else { return };
    let first = create_purchase_on(C_PUR).await;
    let second = create_purchase_with(C_PUR, "5").await;
    let expect = purchase_with_tax(first).await + purchase_with_tax(second).await;
    assert_eq!(
        expect,
        dec(ORDER_WITH_TAX) + dec(ORDER2_WITH_TAX),
        "夹具金额口径：22.60 + 11.30"
    );

    let detail = exec_handler::execution_of(uid(C_PUR)).await.expect("详情");
    let head = &detail.contract;
    assert_eq!(head.executed_amount, expect, "已执行额 = Σ 计入单据 total_with_tax");
    assert_eq!(head.remaining_amount, dec("1000") - expect);
    assert_eq!(head.exec_amount_rate, Some(dec("0.0339")), "33.90 / 1000，6 位小数");
    assert_eq!(head.exec_amount_rate_pct, Some(dec("3.39")));
    assert_eq!(head.exec_state, STATE_PARTIAL);
    assert_eq!(head.order_doc_count, 2);
    assert_eq!(head.released_doc_count, 0);
    assert_eq!(head.purchase_doc_count, 2);
    assert_eq!(head.sales_doc_count, 0);
    assert_eq!(head.last_order_on.as_deref(), Some(today().to_string().as_str()));
    assert_eq!(detail.counted_count, 2);
    assert!(detail.sources.iter().all(|row| row.counted));
    println!("[M4-19] 累加：33.90 / 1000 = 3.39%、PARTIAL ✓");
}

/// ③ 作废释放额度：已执行回落，下钻带 `counted=false` + 「已作废」原因。
#[tokio::test]
async fn cancelled_order_releases_amount() {
    let Some(_guard) = setup().await else { return };
    let keep = create_purchase_on(C_PUR).await;
    let doomed = create_purchase_with(C_PUR, "5").await;
    let before = head(C_PUR).await;
    assert_eq!(before.order_doc_count, 2);

    cancel_purchase(doomed).await;

    let detail = exec_handler::execution_of(uid(C_PUR)).await.expect("详情");
    let head = &detail.contract;
    assert_eq!(head.executed_amount, purchase_with_tax(keep).await, "作废单不再计入");
    assert_eq!(head.order_doc_count, 1);
    assert_eq!(head.released_doc_count, 1);
    let released = detail
        .sources
        .iter()
        .find(|row| row.order_id == doomed.to_string())
        .expect("作废单仍必须出现在下钻里（不许凭空消失）");
    assert!(!released.counted);
    assert!(released.verdict.contains("作废"), "原因要说清：{}", released.verdict);
    assert_eq!(released.order_status, "CANCELLED");
    assert_eq!(released.order_status_label, "已作废");
    println!("[M4-19] 作废：已执行回落到 22.60、下钻带原因 ✓");
}

/// ④ 软删释放额度（走真实删除入口）：已执行回落 + 原因「已删除」。
#[tokio::test]
async fn deleted_order_releases_amount() {
    let Some(_guard) = setup().await else { return };
    let keep = create_purchase_on(C_PUR).await;
    let doomed = create_purchase_with(C_PUR, "5").await;
    purchase_handler::delete_purchase_order(doomed, None)
        .await
        .expect("草稿采购单应能删");

    let detail = exec_handler::execution_of(uid(C_PUR)).await.expect("详情");
    assert_eq!(detail.contract.executed_amount, purchase_with_tax(keep).await);
    assert_eq!(detail.contract.released_doc_count, 1);
    let released = detail
        .sources
        .iter()
        .find(|row| row.order_id == doomed.to_string())
        .expect("软删单仍必须出现在下钻里");
    assert!(!released.counted);
    assert!(released.verdict.contains("删除"), "{}", released.verdict);
    assert!(released.order_deleted_at.is_some());
    println!("[M4-19] 软删：已执行回落、下钻带删除原因 ✓");
}

/// ⑤ 边界：已执行 = 合同额 ⇒ DONE（不是 OVER），剩余 0、率 1、百分比 100。
#[tokio::test]
async fn boundary_exact_amount_is_done() {
    let Some(_guard) = setup().await else { return };
    let order = create_purchase_on(C_PUR_FULL).await;
    assert_eq!(
        purchase_with_tax(order).await,
        dec("22.60"),
        "夹具合同额与单据额必须正好相等"
    );
    let head = head(C_PUR_FULL).await;
    assert_eq!(head.executed_amount, dec("22.60"));
    assert_eq!(head.remaining_amount, Decimal::ZERO);
    assert_eq!(head.exec_amount_rate, Some(Decimal::ONE));
    assert_eq!(head.exec_amount_rate_pct, Some(dec("100.00")));
    assert_eq!(head.exec_state, STATE_DONE, "用满是用满，不是超限");
    println!("[M4-19] 边界：22.60/22.60 ⇒ DONE、剩余 0 ✓");
}

/// ⑥ 超合同额：剩余为负、率 > 1、状态 OVER（只有直接改库才造得出，守卫会拦新单）。
#[tokio::test]
async fn over_contract_shows_negative_remaining() {
    let Some(_guard) = setup().await else { return };
    let order = create_purchase_on(C_PUR).await;
    let with_tax = purchase_with_tax(order).await;
    // 现实路径造不出超限（守卫会 409），只有「合同额被改小 / 单据被恢复」才会出现 ——
    // 报表必须让这种行显形，而不是把它藏起来。
    exec(
        &format!("UPDATE biz_contract SET amount = 20.0000 WHERE id = '{C_PUR}'::uuid"),
        vec![],
    )
    .await;

    let head = head(C_PUR).await;
    assert_eq!(head.executed_amount, with_tax);
    assert_eq!(head.remaining_amount, dec("20") - with_tax);
    assert!(head.remaining_amount < Decimal::ZERO, "剩余必须为负，不许截成 0");
    assert_eq!(head.exec_amount_rate, Some(dec("1.13")));
    assert_eq!(head.exec_amount_rate_pct, Some(dec("113.00")));
    assert_eq!(head.exec_state, STATE_OVER);
    assert_eq!(head.exec_state_label, "已超限");
    println!("[M4-19] 超限：剩余 {}（负数）、率 113%、OVER ✓", head.remaining_amount);
}

/// ⑦ 销售合同看销售单：销售单计入、采购单不串过来；下游类型 = SALES_ORDER。
#[tokio::test]
async fn sales_contract_counts_sales_orders_only() {
    let Some(_guard) = setup().await else { return };
    // 采购侧先造一张单（挂在采购合同上），它不该对销售合同有任何影响
    create_purchase_on(C_PUR).await;
    let (sales_id, sales_total) = create_sales_on(C_SALES, "40").await;
    assert_eq!(sales_total, dec("200"), "40 × 5.00、税率 0 ⇒ 含税 200");

    let detail = exec_handler::execution_of(uid(C_SALES)).await.expect("详情");
    assert_eq!(detail.contract.executed_amount, dec("200"));
    assert_eq!(detail.contract.remaining_amount, dec("300"));
    assert_eq!(detail.contract.exec_amount_rate, Some(dec("0.4")));
    assert_eq!(detail.contract.exec_state, STATE_PARTIAL);
    assert_eq!(detail.contract.downstream_type.as_deref(), Some(DOWNSTREAM_SALES));
    assert_eq!(detail.contract.downstream_type_label.as_deref(), Some("销售单"));
    assert_eq!(detail.contract.sales_doc_count, 1);
    assert_eq!(detail.contract.purchase_doc_count, 0);
    assert_eq!(detail.sources.len(), 1);
    assert_eq!(detail.sources[0].order_id, sales_id.to_string());
    assert_eq!(detail.sources[0].source_type, DOWNSTREAM_SALES);
    assert_eq!(detail.sources[0].source_type_label, "销售单");
    assert!(detail.sources[0].counted);
    println!("[M4-19] 销售合同：200/500 = 40%、只看销售单 ✓");
}

/// ⑧ 服务 / 租赁合同没有下游单据 ⇒ NO_DOWNSTREAM（不是 NOT_STARTED）。
#[tokio::test]
async fn service_and_lease_have_no_downstream() {
    let Some(_guard) = setup().await else { return };
    for contract in [C_SERVICE, C_LEASE] {
        let head = head(contract).await;
        assert_eq!(head.exec_state, STATE_NO_DOWNSTREAM, "{contract}");
        assert_eq!(head.exec_state_label, "无下游单据");
        assert_eq!(head.downstream_type, None);
        assert_eq!(head.downstream_type_label, None);
        assert_eq!(head.executed_amount, Decimal::ZERO);
        assert_eq!(head.remaining_amount, head.amount, "没有下游 ⇒ 剩余 = 合同额原样");
        assert_eq!(head.exec_amount_rate, Some(Decimal::ZERO));
    }
    println!("[M4-19] 服务 / 租赁：NO_DOWNSTREAM、剩余 = 合同额 ✓");
}

/// ⑨ 已软删合同：不进总览、详情 404。
#[tokio::test]
async fn soft_deleted_contract_hidden_and_detail_404() {
    let Some(_guard) = setup().await else { return };
    let err = exec_handler::execution_of(uid(C_DELETED))
        .await
        .expect_err("已软删合同必须 404");
    assert_eq!(err.status, 404);
    assert!(err.msg.contains("不存在或已删除"), "{}", err.msg);

    let filter = ExecutionPageQuery {
        keyword: Some(format!("{PREFIX}HT-DEL")),
        page_size: Some(200),
        ..Default::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, total) = exec_handler::page_of(&filter).await.expect("总览查询");
    assert_eq!(total, 0, "已软删合同不许出现在总览里");
    assert!(rows.is_empty());
    println!("[M4-19] 软删合同：总览不可见、详情 404 ✓");
}

/// ⑩ 币种串了：照计入（与守卫同口径），但 `currency_mismatch_doc_count` 与下钻必须显形。
#[tokio::test]
async fn currency_mismatch_is_visible_but_still_counted() {
    let Some(_guard) = setup().await else { return };
    // 采购建单入口不收币种（单据币种跟账套本位币），跨币种只能靠改库 / 导入造出来 ——
    // 这正是它必须显形的原因：没人会在界面上看到它，只能从报表发现。
    let sup = supplier().await;
    exec(
        &format!(
            "INSERT INTO biz_purchase_order
                (id, order_no, supplier_id, order_date, warehouse_id, status, currency_code,
                 total_amount, total_tax, total_with_tax, purchase_type, contract_id, contract_no_snapshot)
             VALUES ('00000000-0000-4000-8000-0000041900b1'::uuid, '{PREFIX}PO-USD',
                     '{sup}'::uuid, current_date, gen_random_uuid(), 'APPROVED', 'USD',
                     88.4956, 11.5044, 100.0000, 'CONTRACT', '{C_CURRENCY}'::uuid, '{PREFIX}HT-CUR')"
        ),
        vec![],
    )
    .await;

    let detail = exec_handler::execution_of(uid(C_CURRENCY)).await.expect("详情");
    assert_eq!(
        detail.contract.executed_amount,
        dec("100"),
        "口径不改：USD 单照加（守卫也不看币种）"
    );
    assert_eq!(detail.contract.currency_mismatch_doc_count, 1);
    assert_eq!(detail.contract.currency_code, "CNY");
    let row = detail.sources.first().expect("应有下钻行");
    assert!(!row.currency_matched, "币种串了必须显形");
    assert_eq!(row.currency_code, "USD");
    assert!(row.counted, "显形 ≠ 剔除：still counted");

    // 只看币种串了的总览筛选能命中它
    let filter = ExecutionPageQuery {
        currency_mismatch_only: Some(true),
        page_size: Some(200),
        ..Default::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, _total) = exec_handler::page_of(&filter).await.expect("总览查询");
    assert!(
        rows.iter()
            .any(|candidate| candidate.contract_id == uid(C_CURRENCY).to_string()),
        "currency_mismatch_only 必须命中这张合同"
    );
    println!("[M4-19] 币种串了：照计 100 + mismatch=1 + 筛选命中 ✓");
}

/// ⑪ 总览筛选 + 排序：类型 / 状态 / 执行状态 / 只看超限 / 只看过期 / 关键字；OVER 排前面。
#[tokio::test]
async fn overview_filters_and_ordering() {
    let Some(_guard) = setup().await else { return };
    // 一张用满的（DONE）+ 一张超限的（OVER）+ 一张过期的（NOT_STARTED）
    create_purchase_on(C_PUR_FULL).await;
    let order = create_purchase_on(C_PUR).await;
    let _ = purchase_with_tax(order).await;
    exec(
        &format!("UPDATE biz_contract SET amount = 20.0000 WHERE id = '{C_PUR}'::uuid"),
        vec![],
    )
    .await;

    // 只看超限
    let over = ExecutionPageQuery {
        over_only: Some(true),
        page_size: Some(200),
        ..Default::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, _total) = exec_handler::page_of(&over).await.expect("总览查询");
    assert!(rows.iter().any(|row| row.contract_id == uid(C_PUR).to_string()));
    assert!(
        rows.iter().all(|row| row.exec_state == STATE_OVER),
        "over_only 只许返回 OVER 行"
    );

    // 只看过期
    let expired = ExecutionPageQuery {
        expired_only: Some(true),
        page_size: Some(200),
        ..Default::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, _total) = exec_handler::page_of(&expired).await.expect("总览查询");
    assert!(rows.iter().any(|row| row.contract_id == uid(C_EXPIRED).to_string()));
    assert!(rows.iter().all(|row| row.expired));

    // 按合同类型筛
    let sales = ExecutionPageQuery {
        contract_type: Some("SALES".to_string()),
        page_size: Some(200),
        ..Default::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, _total) = exec_handler::page_of(&sales).await.expect("总览查询");
    assert!(rows.iter().all(|row| row.contract_type == "SALES"));
    assert!(rows.iter().any(|row| row.contract_id == uid(C_SALES).to_string()));

    // 按执行状态筛
    let done = ExecutionPageQuery {
        exec_state: Some(STATE_DONE.to_string()),
        page_size: Some(200),
        ..Default::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, _total) = exec_handler::page_of(&done).await.expect("总览查询");
    assert!(rows.iter().any(|row| row.contract_id == uid(C_PUR_FULL).to_string()));

    // 关键字命中合同号
    let by_no = row_in_page(
        ExecutionPageQuery {
            keyword: Some(format!("{PREFIX}HT-FULL")),
            page_size: Some(200),
            ..Default::default()
        },
        C_PUR_FULL,
    )
    .await;
    assert!(by_no.is_some(), "关键字按合同号必须能查到");

    // 排序：OVER 排最前（控盘先看越界）
    let all = ExecutionPageQuery {
        page_size: Some(200),
        ..Default::default()
    }
    .filter()
    .expect("筛选参数合法");
    let (rows, total) = exec_handler::page_of(&all).await.expect("总览查询");
    assert!(total >= 7, "夹具合同至少 7 张存活（实际 {total}）");
    assert_eq!(
        rows.first().map(|row| row.exec_state.as_str()),
        Some(STATE_OVER),
        "OVER 必须排第一"
    );
    println!("[M4-19] 总览筛选 / 排序：over_only / expired_only / 类型 / 执行状态 / 关键字 / OVER 优先 ✓");
}

/// ⑫ 清场：`M419-` 夹具 0 残留（单据按夹具合同 id 反查 / 合同按号前缀，都不许留在库里）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    create_purchase_on(C_PUR).await;
    create_sales_on(C_SALES, "1").await;
    let ids = fixture_contract_ids();
    let before: i64 = db()
        .scalar_with_params(
            &format!(
                "SELECT (SELECT count(*) FROM biz_purchase_order WHERE contract_id IN ({ids})) \
                      + (SELECT count(*) FROM biz_sales_order WHERE contract_id IN ({ids}))"
            ),
            vec![],
        )
        .await
        .expect("统计夹具单据");
    assert_eq!(before, 2, "清场前应有一张夹具采购单 + 一张夹具销售单（实际 {before}）");

    cleanup().await;

    for (sql, label) in [
        (
            format!("SELECT count(*) FROM biz_purchase_order WHERE contract_id IN ({ids})"),
            "采购单",
        ),
        (
            format!("SELECT count(*) FROM biz_sales_order WHERE contract_id IN ({ids})"),
            "销售单",
        ),
        (
            format!("SELECT count(*) FROM biz_contract WHERE contract_no LIKE '{PREFIX}%'"),
            "合同",
        ),
        (
            format!("SELECT count(*) FROM v_contract_execution WHERE contract_no LIKE '{PREFIX}%'"),
            "视图里的合同",
        ),
        (
            format!("SELECT count(*) FROM v_contract_execution_order WHERE contract_id IN ({ids})"),
            "视图里的单据",
        ),
    ] {
        let left: i64 = db().scalar_with_params(&sql, vec![]).await.expect("统计残留");
        assert_eq!(left, 0, "{label} 清场后必须 0 残留（实际 {left}）");
    }
    println!("[M4-19] 清场：M419- 夹具 0 残留 ✓");
}

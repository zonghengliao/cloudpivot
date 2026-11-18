// ============================================================================
// sales_variance_db_tests.rs —— 预测 vs 实际（偏差分析，M3-21）的真库用例
//
// 卡：t_31257c83（M3-21【M3·销售】销售预测：预测 vs 实际对比 + 偏差分析）
// 口径依据：`sales_target_dto` 的「预测 vs 实际（偏差分析）」一节
//          + `sales_target_model` 的「预测 vs 实际：取数」一节
//
// # 覆盖什么（每条对应卡面 / 口径文档的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `customer_grain_compares_latest_version_against_orders` | 预测侧取**当前版本**（v2 覆盖 v1）；实际侧 = 期间内有效订单本位币含税金额 + 明细数量；没预测却有实际的客户补成漏预测行；汇总与明细同源 |
// | `material_grain_uses_line_amount_and_qty` | 物料粒度按**明细**汇总（金额 + 数量），不漏掉没预测的物料 |
// | `base_currency_is_used_for_foreign_order` | 外币订单按 `base_total_with_tax` 算实际（不把 1000 USD 当 1000 CNY） |
// | `all_grain_uses_company_total` | 全局粒度只有一行「全公司」，实际 = 全公司订单合计 |
// | `dimension_counts_report_every_grain` | 各粒度条数四种都给（0 也给），前端下拉不用猜 |
// | `invalid_inputs_are_400_without_touching_db` | 缺期间 / 粒度越界 / 容差非法 / 期间越界 → 400 |
// | `zz_teardown_purges_fixtures` | 清场：本卡夹具预测与订单 0 残留 |
//
// # 为什么直调 handler 而不是拼 SQL
//
// 被测的是「取数 + 组装 + 判档」整条链路：`handler::forecast_variance` 与 HTTP 路由走**同一条**
// 代码（只跳过认证）。夹具的订单/明细直插（绕过状态机造「已批准订单」），预测行走
// `handler::create_forecast`（版本链、取号、唯一索引都照真实路径）。主数据走 `crate::test_seed::ensure_*`
// （门禁：测试不许手写主数据 INSERT）。
//
// # 与其它 ERP 用例共用串行锁
//
// `db()` 是进程级全局池；复用 `aging_db_tests::DB_LOCK`（各锁各的等于没锁）。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m321_variance" cargo test --lib \
//   business::erp::sales::sales_variance -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::sales::sales_target_dto::{
    FORECAST_DIM_ALL, FORECAST_DIM_CUSTOMER, FORECAST_DIM_CUSTOMER_MATERIAL, FORECAST_DIM_MATERIAL, ForecastCreateReq,
    ForecastVarianceQuery,
};
use crate::business::erp::sales::sales_target_handler as handler;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具订单号前缀（清场按它删，绝不碰生产号段）。
pub(crate) const SO_PREFIX: &str = "M321-SO-";
/// 夹具期间（2032 年的预测真实业务不会有，天然隔离）。
pub(crate) const PERIOD: &str = "2032-07";
/// 夹具预测备注（清场判据：只删本卡造的预测行）。
pub(crate) const MARK: &str = "M3-21 对比夹具";
/// 夹具客户命名空间（清场判据）：M3-21 用 `M321-`，M3-22 的汇总口径用例复用
/// `M322-`（两批用例共用本文件的 `setup` / `cleanup`，所以两个前缀都要清）。
pub(crate) const CUS_PREFIXES: &[&str] = &["M321-", "M322-"];

pub(crate) fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

pub(crate) async fn customer(code: &str, name: &str) -> Uuid {
    test_seed::ensure_customer(code, name).await
}

pub(crate) async fn material(code: &str, name: &str) -> Uuid {
    test_seed::ensure_material(code, name).await
}

/// 清场：先删本卡造的预测行与订单（明细随订单 CASCADE），再把夹具命名空间的
/// **客户行**删掉 —— `cleanup()` 只在用例开头跑，不删客户就是「最后一条用例的客户
/// 永久残留」，演练脚本的净增量因此非 0（见 `test_seed::purge_customers`）。
/// 物料等其它主数据仍由 `test_seed` 复用（确定性 id，不会增长）。
pub(crate) async fn cleanup() {
    exec(
        "DELETE FROM biz_sales_forecast WHERE remark = ?",
        vec![Value::Text(MARK.to_string())],
    )
    .await;
    exec(
        &format!("DELETE FROM biz_sales_order WHERE so_no LIKE '{SO_PREFIX}%'"),
        vec![],
    )
    .await;
    for prefix in CUS_PREFIXES {
        test_seed::purge_customers(prefix).await;
    }
}

pub(crate) async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 一张「已批准」订单（金额 = 含税 = 不含税，税率 0 ⇒ `ck_sales_order_amount` 恒等成立）。
///
/// `total` 是原币含税额，`base_total` 是**本位币**含税额（本位币单两者相等）。
pub(crate) async fn sell(customer_id: Uuid, date: &str, total: &str, base_total: &str) -> Uuid {
    let so_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_sales_order \
                (id, so_no, customer_id, order_date, status, total_amount, total_tax, \
                 total_with_tax, base_total_amount, base_total_tax, base_total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, ?::text::date, 'APPROVED', ?::text::numeric, 0, ?::text::numeric, \
                 ?::text::numeric, 0, ?::text::numeric)",
        vec![
            Value::from(&so_id),
            Value::from(format!("{SO_PREFIX}{}", &so_id.to_string()[24..])),
            Value::from(&customer_id),
            Value::Text(date.to_string()),
            Value::Text(total.to_string()),
            Value::Text(total.to_string()),
            Value::Text(base_total.to_string()),
            Value::Text(base_total.to_string()),
        ],
    )
    .await;
    so_id
}

/// 订单明细一行（`amount` 不含税、税额 0 ⇒ 含税 = amount）。
pub(crate) async fn sell_line(so_id: Uuid, line_no: i32, material_id: Uuid, qty: &str, amount: &str) {
    exec(
        "INSERT INTO biz_sales_order_item \
                (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
         VALUES (?::uuid, ?, ?::uuid, ?::text::numeric, 1, ?::text::numeric, 0, 0, 0)",
        vec![
            Value::from(&so_id),
            Value::I32(line_no),
            Value::from(&material_id),
            Value::Text(qty.to_string()),
            Value::Text(amount.to_string()),
        ],
    )
    .await;
}

/// 建一条预测（走 handler：版本链 / 取号 / 唯一索引都照真实路径）。
pub(crate) async fn forecast(
    customer_id: Option<Uuid>,
    material_id: Option<Uuid>,
    amount: &str,
    qty: &str,
    version_no: i32,
    revised_from_id: Option<&str>,
) -> String {
    handler::create_forecast(
        None,
        ForecastCreateReq {
            customer_id: customer_id.map(|id| id.to_string()),
            material_id: material_id.map(|id| id.to_string()),
            period_code: PERIOD.to_string(),
            forecast_amount: dec(amount),
            forecast_qty: Some(dec(qty)),
            confidence: None,
            version_no: Some(version_no),
            revised_from_id: revised_from_id.map(str::to_string),
            remark: Some(MARK.to_string()),
        },
    )
    .await
    .expect("建预测")
    .id
}

pub(crate) fn query(dimension: &str, tolerance: Option<&str>) -> ForecastVarianceQuery {
    ForecastVarianceQuery {
        period_code: Some(PERIOD.to_string()),
        dimension: Some(dimension.to_string()),
        tolerance_pct: tolerance.map(str::to_string),
    }
}

/// 某粒度的行（按显示名前缀找，避免依赖排序位置）。
pub(crate) fn row_named<'a>(
    response: &'a crate::business::erp::sales::sales_target_dto::ForecastVarianceRes,
    prefix: &str,
) -> &'a crate::business::erp::sales::sales_target_dto::ForecastVarianceRow {
    response
        .rows
        .iter()
        .find(|row| row.display_name.starts_with(prefix))
        .unwrap_or_else(|| {
            panic!(
                "没找到显示名以「{prefix}」开头的行；实际行：{:?}",
                response.rows.iter().map(|row| &row.display_name).collect::<Vec<_>>()
            )
        })
}

// ==================== 用例 ====================

/// 客户粒度：预测取当前版本（v2 覆盖 v1），实际 = 订单本位币含税金额 + 明细数量，
/// 没预测的客户补漏预测行，汇总与明细同源。
#[tokio::test]
async fn customer_grain_compares_latest_version_against_orders() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M321-CA", "对比客户甲").await;
    let customer_b = customer("M321-CB", "对比客户乙").await;
    let material_1 = material("M321-M1", "对比物料甲").await;
    let material_2 = material("M321-M2", "对比物料乙").await;

    // 预测 v1 → v2（当前版本必须是 v2；拿 v1 去比就是拿旧版本算偏差）
    let v1 = forecast(Some(customer_a), None, "100000", "1000", 1, None).await;
    let v2 = forecast(Some(customer_a), None, "120000", "1100", 2, Some(&v1)).await;
    assert_ne!(v1, v2);

    // 客户甲：两单合计 90000 / 900 件；客户乙：20000 / 200 件（完全没预测）
    let order_1 = sell(customer_a, "2032-07-05", "60000", "60000").await;
    sell_line(order_1, 1, material_1, "600", "60000").await;
    let order_2 = sell(customer_a, "2032-07-20", "30000", "30000").await;
    sell_line(order_2, 1, material_2, "300", "30000").await;
    let order_3 = sell(customer_b, "2032-07-08", "20000", "20000").await;
    sell_line(order_3, 1, material_1, "200", "20000").await;

    let response = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, None))
        .await
        .expect("对比查询");

    assert_eq!(response.period_code, PERIOD);
    assert_eq!(response.dimension, FORECAST_DIM_CUSTOMER);
    assert_eq!(response.basis, "ORDER", "本接口只算订单口径");
    assert_eq!(response.tolerance_pct, dec("5"), "容差缺省 ±5%");
    assert_eq!(response.rows.len(), 2, "一行预测 + 一行漏预测");

    let row_a = row_named(&response, "对比客户甲");
    assert!(row_a.has_forecast);
    assert_eq!(row_a.version_no, Some(2), "必须取当前版本 v2");
    assert_eq!(row_a.forecast_amount, dec("120000.00"));
    assert_eq!(row_a.forecast_qty, dec("1100"));
    assert_eq!(row_a.actual_amount, dec("90000.00"), "两单合计（本位币含税）");
    assert_eq!(row_a.actual_qty, dec("900"));
    assert_eq!(row_a.variance_amount, dec("-30000.00"));
    assert_eq!(row_a.variance_rate, Some(dec("-25.00")));
    assert_eq!(row_a.variance_qty, dec("-200"));
    assert_eq!(row_a.deviation_level, "UNDER");

    let row_b = row_named(&response, "对比客户乙");
    assert!(!row_b.has_forecast, "没预测但有实际 ⇒ 漏预测行");
    assert!(row_b.forecast_id.is_none() && row_b.forecast_no.is_none());
    assert_eq!(row_b.actual_amount, dec("20000.00"));
    assert_eq!(row_b.variance_amount, dec("20000.00"));
    assert_eq!(row_b.variance_rate, None, "预测 0 ⇒ 比率无意义");
    assert_eq!(row_b.deviation_level, "NO_FORECAST");

    // 汇总 = 同一批行相加（不是另跑一遍全库 SQL）
    assert_eq!(response.summary.row_count, 2);
    assert_eq!(response.summary.forecast_count, 1);
    assert_eq!(response.summary.unforecast_count, 1);
    assert_eq!(response.summary.forecast_amount, dec("120000"));
    assert_eq!(response.summary.actual_amount, dec("110000"));
    assert_eq!(response.summary.variance_amount, dec("-10000.00"));
    assert_eq!(response.summary.variance_rate, Some(dec("-8.33")));
    assert_eq!(response.summary.under_count, 1);
    assert_eq!(response.summary.mape_pct, Some(dec("25.00")), "只算有预测金额的行");
    let worst = response.summary.worst.expect("有行就有最贵偏差");
    assert_eq!(worst.display_name, "对比客户甲");
    assert_eq!(worst.variance_amount, dec("-30000.00"));

    println!("[M3-21] 客户粒度：v2 覆盖 v1、漏预测行与汇总同源 ✓");
}

/// 物料粒度：按明细汇总金额与数量，没预测的物料也要出现。
#[tokio::test]
async fn material_grain_uses_line_amount_and_qty() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M321-CA", "对比客户甲").await;
    let material_1 = material("M321-M1", "对比物料甲").await;
    let material_2 = material("M321-M2", "对比物料乙").await;

    forecast(None, Some(material_1), "50000", "500", 1, None).await;

    // 一张单两行：M1 600 件 60000、M2 300 件 30000
    let order = sell(customer_a, "2032-07-11", "90000", "90000").await;
    sell_line(order, 1, material_1, "600", "60000").await;
    sell_line(order, 2, material_2, "300", "30000").await;

    let response = handler::forecast_variance(query(FORECAST_DIM_MATERIAL, Some("10")))
        .await
        .expect("对比查询");

    assert_eq!(response.rows.len(), 2, "M1 有预测、M2 漏预测");
    let row_1 = row_named(&response, "对比物料甲");
    assert_eq!(row_1.actual_amount, dec("60000.00"));
    assert_eq!(row_1.actual_qty, dec("600"));
    assert_eq!(row_1.variance_amount, dec("10000.00"));
    assert_eq!(row_1.variance_rate, Some(dec("20.00")));
    assert_eq!(row_1.deviation_level, "OVER");
    assert_eq!(row_1.variance_qty, dec("100"));

    let row_2 = row_named(&response, "对比物料乙");
    assert!(!row_2.has_forecast);
    assert_eq!(row_2.material_id.as_deref(), Some(material_2.to_string().as_str()));
    assert_eq!(row_2.customer_id, None, "物料粒度的行没有客户归属");
    assert_eq!(row_2.actual_amount, dec("30000.00"));
    assert_eq!(row_2.actual_qty, dec("300"));

    println!("[M3-21] 物料粒度：明细金额 + 数量、漏预测物料浮出 ✓");
}

/// 外币订单按本位币折算（1000 USD / 汇率 7 ⇒ 实际 7000 CNY）。
#[tokio::test]
async fn base_currency_is_used_for_foreign_order() {
    let Some(_guard) = setup().await else { return };
    let customer_c = customer("M321-CC", "对比客户丙").await;
    let material_1 = material("M321-M1", "对比物料甲").await;

    let order = sell(customer_c, "2032-07-15", "1000", "7000").await;
    sell_line(order, 1, material_1, "10", "1000").await;

    let response = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, None))
        .await
        .expect("对比查询");
    let row = row_named(&response, "对比客户丙");
    assert_eq!(row.actual_amount, dec("7000.00"), "本位币含税，不是原币");

    // 物料粒度同样折本位币（明细按订单「本位币 / 原币」比例摊）
    let response = handler::forecast_variance(query(FORECAST_DIM_MATERIAL, None))
        .await
        .expect("对比查询");
    let row = row_named(&response, "对比物料甲");
    assert_eq!(row.actual_amount, dec("7000.00"));

    println!("[M3-21] 外币订单按本位币折算 ✓");
}

/// 全局粒度：只有一行「全公司」，实际 = 全公司订单合计。
#[tokio::test]
async fn all_grain_uses_company_total() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M321-CA", "对比客户甲").await;
    let material_1 = material("M321-M1", "对比物料甲").await;

    forecast(None, None, "100000", "1000", 1, None).await;
    let order = sell(customer_a, "2032-07-02", "88000", "88000").await;
    sell_line(order, 1, material_1, "800", "88000").await;

    let response = handler::forecast_variance(query(FORECAST_DIM_ALL, Some("20")))
        .await
        .expect("对比查询");
    assert_eq!(response.rows.len(), 1);
    let row = &response.rows[0];
    assert_eq!(row.display_name, "全公司");
    assert_eq!(row.actual_amount, dec("88000.00"));
    assert_eq!(row.variance_amount, dec("-12000.00"));
    assert_eq!(row.variance_rate, Some(dec("-12.00")));
    assert_eq!(row.deviation_level, "ON_TARGET", "12% 在 ±20% 容差内");
    assert_eq!(row.customer_id, None);

    println!("[M3-21] 全局粒度：一行全公司 + 容差判档 ✓");
}

/// 各粒度条数：四种都给（0 也给），前端下拉不必猜。
#[tokio::test]
async fn dimension_counts_report_every_grain() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M321-CA", "对比客户甲").await;
    let material_1 = material("M321-M1", "对比物料甲").await;

    forecast(Some(customer_a), None, "1000", "1", 1, None).await;
    forecast(None, Some(material_1), "2000", "2", 1, None).await;
    forecast(Some(customer_a), Some(material_1), "3000", "3", 1, None).await;
    forecast(None, None, "4000", "4", 1, None).await;

    let response = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, None))
        .await
        .expect("对比查询");
    assert_eq!(response.dimension_counts.len(), 4);
    let count_of = |dimension: &str| {
        response
            .dimension_counts
            .iter()
            .find(|item| item.dimension == dimension)
            .map(|item| item.row_count)
            .expect("四种粒度都要回")
    };
    assert_eq!(count_of(FORECAST_DIM_CUSTOMER), 1);
    assert_eq!(count_of(FORECAST_DIM_MATERIAL), 1);
    assert_eq!(count_of(FORECAST_DIM_CUSTOMER_MATERIAL), 1);
    assert_eq!(count_of(FORECAST_DIM_ALL), 1);
    // 客户粒度的 rows 只含客户级预测（另三种粒度不混进来）
    assert!(response.rows.iter().all(|row| row.material_id.is_none()));

    println!("[M3-21] 四种粒度条数全回、明细不混粒度 ✓");
}

/// 入参错误一律 400（在取数之前拦下，不碰库）。
#[tokio::test]
async fn invalid_inputs_are_400_without_touching_db() {
    let Some(_guard) = setup().await else { return };

    let cases = [
        (
            ForecastVarianceQuery {
                period_code: None,
                dimension: None,
                tolerance_pct: None,
            },
            "period_code 必填",
        ),
        (
            ForecastVarianceQuery {
                period_code: Some("  ".to_string()),
                dimension: None,
                tolerance_pct: None,
            },
            "period_code 必填",
        ),
        (
            ForecastVarianceQuery {
                period_code: Some("2032-13".to_string()),
                dimension: None,
                tolerance_pct: None,
            },
            "period_code",
        ),
        (query("REGION", None), "dimension 只认"),
        (query(FORECAST_DIM_CUSTOMER, Some("abc")), "tolerance_pct"),
        (query(FORECAST_DIM_CUSTOMER, Some("-1")), "tolerance_pct"),
        (query(FORECAST_DIM_CUSTOMER, Some("101")), "tolerance_pct"),
    ];
    for (bad, hint) in cases {
        let error = handler::forecast_variance(bad).await.expect_err("非法入参必须被拒");
        assert_eq!(error.status, 400, "应是 400：{hint}");
        assert_eq!(error.code, error_code::PARAM_INVALID, "{hint}");
        assert!(error.msg.contains(hint), "文案要含「{hint}」，实际：{}", error.msg);
    }

    println!("[M3-21] 七种非法入参全部 400 ✓");
}

/// 清场断言：本卡夹具预测与订单 0 残留，夹具客户行也 0 残留
/// （客户行留在库里会把演练脚本的「整个演练净增量」顶成非 0）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;

    let forecast_rows: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_sales_forecast WHERE remark = ?",
            vec![Value::Text(MARK.to_string())],
        )
        .await
        .expect("统计预测残留");
    assert_eq!(forecast_rows, 0, "预测夹具必须清干");

    let order_rows: i64 = db()
        .scalar_with_params(
            &format!("SELECT count(*) FROM biz_sales_order WHERE so_no LIKE '{SO_PREFIX}%'"),
            vec![],
        )
        .await
        .expect("统计订单残留");
    assert_eq!(order_rows, 0, "订单夹具必须清干");

    for prefix in CUS_PREFIXES {
        let customers = test_seed::customer_count(prefix).await;
        assert_eq!(customers, 0, "{prefix} 夹具客户必须清干（残留会让演练净增量非 0）");
    }

    println!("[M3-21] 清场：预测 0 行、订单 0 行、夹具客户 0 行 ✓");
}

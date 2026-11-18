// ============================================================================
// sales_variance_summary_db_tests.rs —— 销售预测偏差分析的**汇总口径**真库用例（M3-22）
//
// 卡：t_e33d2100（M3-22【M3·销售】销售预测：单测，含汇总口径）
// 口径依据：`sales_target_dto` 的「预测 vs 实际（偏差分析）」一节
//          + `sales_target_handler::summarize_variance` 的函数文档
//
// # 为什么单独一个文件（而不是塞进 `sales_variance_db_tests`）
//
// M3-21 的用例钉的是「单条链路能跑通」（v2 覆盖 v1 / 本位币折算 / 漏预测行 / 粒度计数 /
// 入参 400），本卡钉的是**汇总数字本身**：合计怎么加、档位怎么数、比率用什么当分母、
// 容差动的是哪一层。夹具**完全复用** M3-21 的那批（`sell` / `sell_line` / `forecast` /
// `setup` / `cleanup` / `row_named`，都已 `pub(crate)`），不另造一套 —— 两批用例跑在
// 同一个串行锁、同一个期间（`2032-07`）、同一份清场判据上，谁先跑都把对方的夹具清干净。
//
// # 覆盖什么（每条对应口径文档的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `summary_adds_rows_and_excludes_history_versions` | 汇总 = 本粒度这批行相加；**历史版本不进预测合计**（v1 不被 v2 之外再加一遍）；合计偏差率 = Σ实际 ÷ Σ预测；MAPE 行等权；worst 取 \|偏差\| 最大 |
// | `soft_deleted_current_version_falls_back_in_variance` | 软删当前版本后，对比接口回落到上一版（V0444 视图的语义要**穿过** handler 真的生效，不是只有视图 SQL 对） |
// | `pair_grain_reports_unforecast_pairs` | 客户 × 物料粒度的漏预测行带**两维** id/名字，并进合计的实际额 |
// | `grains_share_actual_total_but_isolate_forecast` | 同一批订单的四种切法：实际合计**相同**（不是四份不同的钱），预测合计**各不相同且绝不相加**（相加 = 双计） |
// | `tolerance_only_moves_the_level_not_the_totals` | 容差只影响判档，不碰任何合计金额/偏差率（否则同一个月的账换个容差就变样） |
// | `zz_teardown_purges_fixtures` | 清场：夹具预测与订单 0 残留，夹具客户行（`M321-`/`M322-`）也 0 残留 |
//
// # 跑法（真库用例必须串行，共用进程级 `db()` 池）
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m322_summary" cargo test --lib \
//   business::erp::sales::sales_variance -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::erp::sales::sales_target_dto::{
    FORECAST_DIM_ALL, FORECAST_DIM_CUSTOMER, FORECAST_DIM_CUSTOMER_MATERIAL, FORECAST_DIM_MATERIAL, ForecastVarianceRes,
};
use crate::business::erp::sales::sales_target_handler as handler;
use crate::business::erp::sales::sales_variance_db_tests::{
    CUS_PREFIXES, MARK, PERIOD, SO_PREFIX, cleanup, customer, dec, forecast, material, query, row_named, sell,
    sell_line, setup,
};
use crate::test_seed;

/// 某粒度的条数（`dimension_counts` 四种粒度都会回，缺了就是契约破了）。
fn count_of(response: &ForecastVarianceRes, dimension: &str) -> u32 {
    response
        .dimension_counts
        .iter()
        .find(|item| item.dimension == dimension)
        .map(|item| item.row_count)
        .unwrap_or_else(|| panic!("dimension_counts 缺了 {dimension}"))
}

/// 汇总与明细同源的自洽断言：把返回的行自己再加一遍，必须与 `summary` 一分不差。
///
/// 这是「汇总没有另跑一遍全库 SQL」的唯一可执行判据 —— 只比几个硬编码数字，
/// 取数口径漂了（比如汇总走了另一种过滤）照样能过。
fn assert_summary_is_sum_of_rows(response: &ForecastVarianceRes) {
    let row_forecast: Decimal = response.rows.iter().map(|row| row.forecast_amount).sum();
    let row_actual: Decimal = response.rows.iter().map(|row| row.actual_amount).sum();
    let row_forecast_qty: Decimal = response.rows.iter().map(|row| row.forecast_qty).sum();
    let row_actual_qty: Decimal = response.rows.iter().map(|row| row.actual_qty).sum();
    assert_eq!(row_forecast, response.summary.forecast_amount, "预测合计 ≠ 行相加");
    assert_eq!(row_actual, response.summary.actual_amount, "实际合计 ≠ 行相加");
    assert_eq!(row_forecast_qty, response.summary.forecast_qty, "预测数量合计 ≠ 行相加");
    assert_eq!(row_actual_qty, response.summary.actual_qty, "实际数量合计 ≠ 行相加");
    assert_eq!(
        u32::try_from(response.rows.len()).expect("行数"),
        response.summary.row_count,
        "row_count 必须等于返回的行数"
    );
    assert_eq!(
        response.summary.forecast_count + response.summary.unforecast_count,
        response.summary.row_count,
        "forecast_count + unforecast_count 必须等于 row_count"
    );
    assert_eq!(
        response.summary.over_count + response.summary.under_count + response.summary.on_target_count,
        response.summary.forecast_count,
        "三个档位之和必须等于有预测的行数（漏预测行不占档位）"
    );
    let count_by_hand = |has_forecast: bool| {
        u32::try_from(
            response
                .rows
                .iter()
                .filter(|row| row.has_forecast == has_forecast)
                .count(),
        )
        .expect("行数")
    };
    assert_eq!(response.summary.forecast_count, count_by_hand(true));
    assert_eq!(response.summary.unforecast_count, count_by_hand(false));
}

// ==================== 用例 ====================

/// 汇总 = 本粒度这批行相加；历史版本不进预测合计；合计率 / MAPE / worst 三个口径各归各。
#[tokio::test]
async fn summary_adds_rows_and_excludes_history_versions() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M322-CA", "汇总客户甲").await;
    let customer_b = customer("M322-CB", "汇总客户乙").await;
    let customer_c = customer("M322-CC", "汇总客户丙").await;
    let material_1 = material("M322-M1", "汇总物料甲").await;
    let material_2 = material("M322-M2", "汇总物料乙").await;

    // 客户甲：v1 10 万 → v2 12 万（当前版本 = v2，预测合计里**只能出现一次**）
    let v1 = forecast(Some(customer_a), None, "100000", "1000", 1, None).await;
    forecast(Some(customer_a), None, "120000", "1100", 2, Some(&v1)).await;
    // 客户乙：5 万；客户丙：4 万（一单没开）
    forecast(Some(customer_b), None, "50000", "500", 1, None).await;
    forecast(Some(customer_c), None, "40000", "400", 1, None).await;

    // 客户甲两张单合计 9 万 / 900 件；客户乙一张单 6 万 / 600 件
    let order_1 = sell(customer_a, "2032-07-03", "60000", "60000").await;
    sell_line(order_1, 1, material_1, "600", "60000").await;
    let order_2 = sell(customer_a, "2032-07-18", "30000", "30000").await;
    sell_line(order_2, 1, material_2, "300", "30000").await;
    let order_3 = sell(customer_b, "2032-07-09", "60000", "60000").await;
    sell_line(order_3, 1, material_1, "600", "60000").await;

    let response = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, None))
        .await
        .expect("对比查询");

    assert_eq!(response.rows.len(), 3, "三条客户级预测，没有漏预测行");
    // 当前版本口径：客户甲那行是 v2，不是 v1
    let row_a = row_named(&response, "汇总客户甲");
    assert_eq!(row_a.version_no, Some(2));
    assert_eq!(row_a.forecast_amount, dec("120000.00"));
    assert_eq!(
        row_a.actual_amount,
        dec("90000.00"),
        "两张单相加（按单汇总，不按明细行数翻倍）"
    );
    assert_eq!(row_a.variance_amount, dec("-30000.00"));
    assert_eq!(row_a.variance_rate, Some(dec("-25.00")));
    assert_eq!(row_a.deviation_level, "UNDER");

    // ---- 汇总口径（本卡的主角）----
    // 预测合计 = 12 万 + 5 万 + 4 万 = 21 万；v1 的 10 万**不进**（进了就是 31 万）
    assert_eq!(response.summary.forecast_amount, dec("210000"));
    assert_eq!(response.summary.actual_amount, dec("150000"));
    assert_eq!(response.summary.variance_amount, dec("-60000.00"));
    // 合计偏差率按钱算：-60000 / 210000 = -28.57%（不是 (-25 + 20 - 100) / 3 = -35.00%）
    assert_eq!(response.summary.variance_rate, Some(dec("-28.57")));
    // 数量侧同口径：预测 2000（1100 + 500 + 400）/ 实际 1500
    assert_eq!(response.summary.forecast_qty, dec("2000"));
    assert_eq!(response.summary.actual_qty, dec("1500"));
    assert_eq!(response.summary.variance_qty, dec("-500"));
    assert_eq!(response.summary.variance_qty_rate, Some(dec("-25.00")));
    // 档位计数：乙 +20% OVER；甲 -25%、丙 -100% UNDER
    assert_eq!(
        (
            response.summary.over_count,
            response.summary.under_count,
            response.summary.on_target_count,
        ),
        (1, 2, 0)
    );
    // MAPE 行等权：(25 + 20 + 100) / 3 = 48.33%
    assert_eq!(response.summary.mape_pct, Some(dec("48.33")));

    assert_summary_is_sum_of_rows(&response);

    // worst = |偏差| 最大的一行：客户丙 -4 万（比客户甲 -3 万还大，因为它一单没开）
    let worst = response.summary.worst.clone().expect("有行就有最贵偏差");
    assert_eq!(worst.display_name, "汇总客户丙");
    assert_eq!(worst.variance_amount, dec("-40000.00"));
    assert_eq!(worst.deviation_level, "UNDER");

    println!("[M3-22] 汇总 = 行相加、历史版本不双计、合计率 ≠ 行率平均 ✓");
}

/// 软删当前版本后，对比接口回落上一版（不是「没预测」，也不是拿被删的那版去比）。
#[tokio::test]
async fn soft_deleted_current_version_falls_back_in_variance() {
    let Some(_guard) = setup().await else { return };
    let customer_d = customer("M322-CD", "汇总客户丁").await;
    let material_1 = material("M322-M1", "汇总物料甲").await;

    let v1 = forecast(Some(customer_d), None, "100000", "1000", 1, None).await;
    let v2 = forecast(Some(customer_d), None, "120000", "1100", 2, Some(&v1)).await;
    // 删掉最新的 v2：版本链语义是「删掉这一版」，不是「把历史也删了」
    handler::delete_forecast(None, &v2).await.expect("软删当前版本");

    let order = sell(customer_d, "2032-07-12", "100000", "100000").await;
    sell_line(order, 1, material_1, "1000", "100000").await;

    let response = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, None))
        .await
        .expect("对比查询");

    assert_eq!(response.rows.len(), 1, "回落后的 v1 仍然是一行预测（不是漏预测行）");
    let row = row_named(&response, "汇总客户丁");
    assert!(row.has_forecast, "删了 v2 不等于这个客户没预测");
    assert_eq!(row.version_no, Some(1), "必须回落到 v1");
    assert_eq!(
        row.forecast_amount,
        dec("100000.00"),
        "拿到 v2 的 12 万就是拿了一条已删的版本"
    );
    assert_eq!(row.actual_amount, dec("100000.00"));
    assert_eq!(row.variance_amount, Decimal::ZERO);
    assert_eq!(row.deviation_level, "ON_TARGET");

    assert_eq!(
        response.summary.forecast_amount,
        dec("100000"),
        "汇总也不能算进去被删的版本"
    );
    assert_eq!(response.summary.forecast_qty, dec("1000"));
    assert_eq!(
        count_of(&response, FORECAST_DIM_CUSTOMER),
        1,
        "各粒度条数只数存活行的当前版本"
    );
    assert_summary_is_sum_of_rows(&response);

    println!("[M3-22] 软删当前版本 ⇒ 对比接口回落上一版 ✓");
}

/// 客户 × 物料粒度：漏预测行两维都带，并进合计的实际额。
#[tokio::test]
async fn pair_grain_reports_unforecast_pairs() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M322-CA", "汇总客户甲").await;
    let customer_b = customer("M322-CB", "汇总客户乙").await;
    let material_1 = material("M322-M1", "汇总物料甲").await;
    let material_2 = material("M322-M2", "汇总物料乙").await;

    forecast(Some(customer_a), Some(material_1), "10000", "100", 1, None).await;

    // 三条不同的（客户, 物料）实际：只有甲×物料甲有预测
    let order_1 = sell(customer_a, "2032-07-05", "8000", "8000").await;
    sell_line(order_1, 1, material_1, "80", "8000").await;
    let order_2 = sell(customer_a, "2032-07-06", "3000", "3000").await;
    sell_line(order_2, 1, material_2, "30", "3000").await;
    let order_3 = sell(customer_b, "2032-07-07", "5000", "5000").await;
    sell_line(order_3, 1, material_1, "50", "5000").await;

    let response = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER_MATERIAL, None))
        .await
        .expect("对比查询");
    assert_eq!(response.rows.len(), 3, "一行预测 + 两行漏预测");

    let covered = row_named(&response, "汇总客户甲 × 汇总物料甲");
    assert!(covered.has_forecast);
    assert_eq!(covered.actual_amount, dec("8000.00"));
    assert_eq!(covered.variance_amount, dec("-2000.00"));
    assert_eq!(covered.variance_rate, Some(dec("-20.00")));

    let missing = row_named(&response, "汇总客户乙 × 汇总物料甲");
    assert!(!missing.has_forecast, "没预测却卖掉了 ⇒ 漏预测行");
    assert_eq!(missing.deviation_level, "NO_FORECAST");
    assert!(
        missing.customer_id.is_some() && missing.material_id.is_some(),
        "两维 id 都要给"
    );
    assert_eq!(missing.customer_name.as_deref(), Some("汇总客户乙"));
    assert_eq!(missing.material_name.as_deref(), Some("汇总物料甲"));
    assert!(missing.forecast_id.is_none() && missing.forecast_no.is_none());
    assert_eq!(missing.variance_amount, dec("5000.00"));
    assert_eq!(missing.variance_rate, None, "没有预测 ⇒ 比率无意义");

    let missing_2 = row_named(&response, "汇总客户甲 × 汇总物料乙");
    assert!(!missing_2.has_forecast);
    assert_eq!(missing_2.actual_amount, dec("3000.00"));

    // 汇总：漏预测行的实际额进合计（分子分母都进），MAPE 只算有预测那一行
    assert_eq!(response.summary.forecast_amount, dec("10000"));
    assert_eq!(response.summary.actual_amount, dec("16000"));
    assert_eq!(response.summary.variance_amount, dec("6000.00"));
    assert_eq!(response.summary.variance_rate, Some(dec("60.00")));
    assert_eq!(response.summary.forecast_count, 1);
    assert_eq!(response.summary.unforecast_count, 2);
    assert_eq!(response.summary.mape_pct, Some(dec("20.00")));
    assert_summary_is_sum_of_rows(&response);

    println!("[M3-22] 客户 × 物料粒度：漏预测行两维齐全、进合计 ✓");
}

/// 同一批订单的四种切法：实际合计相同，预测合计各归各的粒度（绝不相加）。
#[tokio::test]
async fn grains_share_actual_total_but_isolate_forecast() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M322-CA", "汇总客户甲").await;
    let material_1 = material("M322-M1", "汇总物料甲").await;

    // 四种粒度各一条预测（同一期间），实际订单只有一张：客户甲 × 物料甲 9 万 / 900 件
    forecast(Some(customer_a), None, "100000", "1000", 1, None).await;
    forecast(None, Some(material_1), "50000", "500", 1, None).await;
    forecast(Some(customer_a), Some(material_1), "30000", "300", 1, None).await;
    forecast(None, None, "200000", "2000", 1, None).await;
    let order = sell(customer_a, "2032-07-21", "90000", "90000").await;
    sell_line(order, 1, material_1, "900", "90000").await;

    let customer_view = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, None))
        .await
        .expect("客户粒度");
    let material_view = handler::forecast_variance(query(FORECAST_DIM_MATERIAL, None))
        .await
        .expect("物料粒度");
    let pair_view = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER_MATERIAL, None))
        .await
        .expect("客户 × 物料粒度");
    let all_view = handler::forecast_variance(query(FORECAST_DIM_ALL, None))
        .await
        .expect("全公司粒度");

    // ① 实际侧：同一批订单的三种切法 + 全公司，实际合计必须一致（9 万 / 900 件）
    for (name, view) in [
        ("CUSTOMER", &customer_view),
        ("MATERIAL", &material_view),
        ("CUSTOMER_MATERIAL", &pair_view),
        ("ALL", &all_view),
    ] {
        assert_eq!(
            view.summary.actual_amount,
            dec("90000"),
            "{name} 粒度的实际合计与其它切法不一致（同一批订单不该算出两份钱）"
        );
        assert_eq!(view.summary.actual_qty, dec("900"), "{name} 粒度的实际数量不一致");
        assert_summary_is_sum_of_rows(view);
    }

    // ② 预测侧：只汇总本次粒度（10 万 / 5 万 / 3 万 / 20 万），四份互不相干
    let expectations = [
        ("CUSTOMER", &customer_view, "100000", "-10.00"),
        ("MATERIAL", &material_view, "50000", "80.00"),
        ("CUSTOMER_MATERIAL", &pair_view, "30000", "200.00"),
        ("ALL", &all_view, "200000", "-55.00"),
    ];
    let mut forecast_total = Decimal::ZERO;
    for (name, view, forecast, rate) in expectations {
        assert_eq!(view.summary.forecast_amount, dec(forecast), "{name} 的预测合计串粒度了");
        assert_eq!(view.summary.variance_rate, Some(dec(rate)), "{name} 的合计偏差率");
        forecast_total += view.summary.forecast_amount;
    }
    // 四份预测相加 = 38 万，任何一份 summary 都不该等于这个数（等于就是跨粒度双计）
    assert_eq!(forecast_total, dec("380000"));
    for view in [&customer_view, &material_view, &pair_view, &all_view] {
        assert_ne!(
            view.summary.forecast_amount,
            dec("380000"),
            "预测合计不该是四种粒度相加"
        );
    }

    // ③ 各粒度条数各一条，明细不混粒度
    assert_eq!(count_of(&customer_view, FORECAST_DIM_CUSTOMER), 1);
    assert_eq!(count_of(&customer_view, FORECAST_DIM_MATERIAL), 1);
    assert_eq!(count_of(&customer_view, FORECAST_DIM_CUSTOMER_MATERIAL), 1);
    assert_eq!(count_of(&customer_view, FORECAST_DIM_ALL), 1);
    assert!(customer_view.rows.iter().all(|row| row.material_id.is_none()));
    assert!(material_view.rows.iter().all(|row| row.customer_id.is_none()));
    assert!(
        pair_view
            .rows
            .iter()
            .all(|row| row.customer_id.is_some() && row.material_id.is_some())
    );

    println!("[M3-22] 四粒度：实际合计同源、预测合计隔离（不相加）✓");
}

/// 容差只动判档，不动任何合计（同一个月的账换个容差不能变样）。
#[tokio::test]
async fn tolerance_only_moves_the_level_not_the_totals() {
    let Some(_guard) = setup().await else { return };
    let customer_a = customer("M322-CA", "汇总客户甲").await;
    let material_1 = material("M322-M1", "汇总物料甲").await;

    forecast(Some(customer_a), None, "100000", "1000", 1, None).await;
    // 实际正好比预测高 5.00%：容差边界
    let order = sell(customer_a, "2032-07-30", "105000", "105000").await;
    sell_line(order, 1, material_1, "1050", "105000").await;

    let default_view = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, None))
        .await
        .expect("缺省容差");
    let strict_view = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, Some("0")))
        .await
        .expect("容差 0");
    let wide_view = handler::forecast_variance(query(FORECAST_DIM_CUSTOMER, Some("10")))
        .await
        .expect("容差 10");

    // 判档：缺省 ±5% ⇒ 正好 5.00% 算命中；容差 0 ⇒ 出界；容差 10% ⇒ 命中
    assert_eq!(default_view.tolerance_pct, dec("5"));
    assert_eq!(row_named(&default_view, "汇总客户甲").deviation_level, "ON_TARGET");
    assert_eq!(strict_view.tolerance_pct, Decimal::ZERO);
    assert_eq!(row_named(&strict_view, "汇总客户甲").deviation_level, "OVER");
    assert_eq!(wide_view.tolerance_pct, dec("10"));
    assert_eq!(row_named(&wide_view, "汇总客户甲").deviation_level, "ON_TARGET");

    // 合计口径：三次调用的金额/偏差/率/数量一字不差
    for (name, view) in [
        ("缺省 5", &default_view),
        ("容差 0", &strict_view),
        ("容差 10", &wide_view),
    ] {
        assert_eq!(view.summary.forecast_amount, dec("100000"), "{name}");
        assert_eq!(view.summary.actual_amount, dec("105000"), "{name}");
        assert_eq!(view.summary.variance_amount, dec("5000.00"), "{name}");
        assert_eq!(view.summary.variance_rate, Some(dec("5.00")), "{name}");
        assert_eq!(view.summary.forecast_qty, dec("1000"), "{name}");
        assert_eq!(view.summary.actual_qty, dec("1050"), "{name}");
        assert_eq!(view.summary.variance_qty, dec("50"), "{name}");
        assert_eq!(view.summary.variance_qty_rate, Some(dec("5.00")), "{name}");
        assert_summary_is_sum_of_rows(view);
    }
    // 容差变了，档位计数随之变（这是唯一允许变的东西）
    assert_eq!(default_view.summary.on_target_count, 1);
    assert_eq!(strict_view.summary.over_count, 1);

    println!("[M3-22] 容差只动判档、不动合计 ✓");
}

/// 清场：本卡与 M3-21 共用的夹具（预测行 / 订单行）必须 0 残留。
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
    assert_eq!(forecast_rows, 0, "预测夹具必须清干（期间 {PERIOD}）");

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

    println!("[M3-22] 清场：预测 0 行、订单 0 行、夹具客户 0 行 ✓");
}

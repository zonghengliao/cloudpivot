// ============================================================================
// sales_forecast_db_tests.rs —— 销售预测表结构（M3-20）的真库用例
//
// 卡：t_0be1f0b6（M3-20【M3·销售】销售预测：表结构（月度 / 客户 / 物料维度）+ 预测值 + 版本）
// 结构依据：V0318（建表）+ V0444（预测数量 / 版本链 / 物料索引 / 当前版本视图）
//
// # 覆盖什么（每条对应迁移或卡面的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `create_reads_back_qty_and_dimension_names` | 月度 × 客户 × 物料建行：预测金额 + **预测数量**原样存取；客户名/物料名 JOIN 回来；`version_no` 默认 1、`revised_from_id` 默认空 |
// | `qty_zero_when_only_amount_is_given` | 不传数量 ⇒ 库里 0（「只预测金额不预测数量」），不是 NULL、不是报错 |
// | `bad_qty_is_400_and_writes_nothing` | 负数量 / 5 位小数 → 400，且库里一行都不留（入口校验先于写） |
// | `duplicate_dimension_period_version_is_409` | 同（客户, 物料, 期间, 版本）第二条 → 409（V0318 的 `uk_biz_sales_forecast_dim`） |
// | `version_chain_links_previous_version` | v2 带 `revised_from_id = v1`、`version_no = 2` 落库；读回链正确；「当前版本」视图给 v2 |
// | `version_chain_rejects_bad_links` | 自指 / 指向不存在 / 跨维度 / 版本号跳号 四种坏链 → 400（不是 500） |
// | `soft_deleted_latest_falls_back_to_previous` | 软删 v2 ⇒ 当前版本视图回落 v1（历史没有被一起删掉） |
// | `current_view_stays_out_of_list_ordering` | 分页（查表）能看到两个版本，视图只给一条 —— 下游 SUM 不会把历史版本重复累加 |
// | `material_dimension_filter_is_exact` | 物料维度筛选只命中该物料的预测（月度窗口不变） |
// | `zz_teardown_purges_fixtures` | 清场：`M320-` 客户名下 / 探针期间的预测 0 行 |
//
// # 为什么直调 handler 而不是拼 SQL
//
// 写入路径就是三条接口（建 / 改 / 删）的编排函数，没有「过账」类动作可绕 —— 用例直接调
// `handler::create_forecast` / `update_forecast` / `delete_forecast`，走的是**和 HTTP 同一条**
// 代码路径（只跳过认证）。唯一直接查库的地方是「当前版本视图」（它是给下游读的，不经过 handler）。
// 主数据一律走 `crate::test_seed::ensure_*`（门禁：测试不许手写主数据 INSERT）。
//
// # 与其它 ERP 用例共用串行锁
//
// `db()` 是进程级全局池；本文件复用 `aging_db_tests::DB_LOCK`（各锁各的等于没锁）。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m320_forecast" cargo test --lib \
//   business::erp::sales::sales_forecast -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::sales::sales_target_dto::{
    CONFIDENCE_HIGH, ForecastCreateReq, ForecastPageQuery, ForecastUpdateReq,
};
use crate::business::erp::sales::sales_target_handler as handler;
use crate::business::erp::sales::sales_target_model as model;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具客户编码前缀（清场判据）。
const CUS_PREFIX: &str = "M320-";
/// 夹具期间（真实业务不会用 2031 年的预测，天然隔离）。
const PERIOD: &str = "2031-03";
/// 探针期间（全局口径行，双空维度）。
const PERIOD_GLOBAL: &str = "2031-04";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn customer(code: &str) -> Uuid {
    test_seed::ensure_customer(code, &format!("预测夹具客户 {code}")).await
}

async fn material(code: &str) -> Uuid {
    test_seed::ensure_material(code, &format!("预测夹具物料 {code}")).await
}

/// 一份预测入参（默认月度 + 无版本链）。
fn req(customer_id: Option<Uuid>, material_id: Option<Uuid>, amount: &str, qty: Option<&str>) -> ForecastCreateReq {
    ForecastCreateReq {
        customer_id: customer_id.map(|id| id.to_string()),
        material_id: material_id.map(|id| id.to_string()),
        period_code: PERIOD.to_string(),
        forecast_amount: dec(amount),
        forecast_qty: qty.map(dec),
        confidence: None,
        version_no: None,
        revised_from_id: None,
        remark: Some("M3-20 预测夹具".to_string()),
    }
}

/// 清场：先删本模块造的行（按 `M320-` 客户 / 夹具期间），再把命名空间 `M320-` 的
/// **客户行**删掉 —— `cleanup()` 只在用例开头跑，不删客户就是「最后一条用例的客户
/// 永久残留」，演练脚本的净增量因此非 0（见 `test_seed::purge_customers`）。
/// 物料等其它主数据仍由 `test_seed` 复用（确定性 id，不会增长）。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM biz_sales_forecast WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%') \
                 OR period_code IN ('{PERIOD}', '{PERIOD_GLOBAL}')"
        ),
        vec![],
    )
    .await;
    test_seed::purge_customers(CUS_PREFIX).await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 某维度某期间在库里的物理行数（含软删 —— 断言「一行都不留」用）。
async fn row_count(customer_id: Uuid, period: &str) -> i64 {
    let rows = db()
        .query_sql(
            "SELECT count(*) AS n FROM biz_sales_forecast WHERE customer_id = ?::uuid AND period_code = ?".to_string(),
            vec![Value::from(&customer_id), Value::Text(period.to_string())],
        )
        .await
        .expect("计数");
    rows.first()
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()
        .expect("读计数")
        .unwrap_or(0)
}

// ==================== 用例 ====================

/// 月度 × 客户 × 物料建行：预测值（金额 + 数量）原样存取，维度名 JOIN 回来，版本默认值对。
#[tokio::test]
async fn create_reads_back_qty_and_dimension_names() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C1").await;
    let mat = material("M320-M1").await;

    let created = handler::create_forecast(None, req(Some(cus), Some(mat), "120000.00", Some("5000.1250")))
        .await
        .expect("建预测（客户 × 物料 × 月度）");

    // 编号形状：SFC-yyyyMMdd-NNNNNN（biz_seq，V0318 种的规则）
    assert!(
        created.forecast_no.starts_with("SFC-"),
        "预测编号：{}",
        created.forecast_no
    );
    assert_eq!(created.period_code, PERIOD, "月度维度原样落库");
    assert_eq!(created.forecast_amount, dec("120000.00"));
    assert_eq!(created.forecast_qty, dec("5000.1250"), "预测数量 4 位小数原样存取");
    assert_eq!(created.confidence, "MEDIUM", "信心度默认 MEDIUM");
    assert_eq!(created.version_no, 1, "版本号默认 1");
    assert!(created.revised_from_id.is_none(), "首版没有版本链");
    assert_eq!(created.customer_id.as_deref(), Some(cus.to_string().as_str()));
    assert_eq!(created.material_id.as_deref(), Some(mat.to_string().as_str()));
    assert!(created.customer_name.is_some(), "客户名要 JOIN 回来（不许显示 UUID）");
    assert!(created.material_name.is_some(), "物料名要 JOIN 回来");

    println!(
        "[M3-20] 建行：{} / qty {} / v1 ✓",
        created.forecast_no, created.forecast_qty
    );
}

/// 不传数量 ⇒ 库里 0（「只预测金额不预测数量」），不是 NULL、不报错。
#[tokio::test]
async fn qty_zero_when_only_amount_is_given() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C2").await;

    let created = handler::create_forecast(None, req(Some(cus), None, "88000.00", None))
        .await
        .expect("只给金额建预测");
    assert_eq!(created.forecast_qty, Decimal::ZERO, "缺省数量必须是 0");

    // 库里也是 0（不是 NULL）：从 model 读回同一条
    let id = Uuid::parse_str(&created.id).expect("id");
    let row = model::find_forecast(id).await.expect("读回").expect("存在");
    assert_eq!(row.0.forecast_qty, Decimal::ZERO);

    println!("[M3-20] 缺省数量 = 0 ✓（金额口径预测）");
}

/// 负数量 / 5 位小数 → 400，且库里一行都不留。
#[tokio::test]
async fn bad_qty_is_400_and_writes_nothing() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C3").await;

    let negative = handler::create_forecast(None, req(Some(cus), None, "1000.00", Some("-1")))
        .await
        .expect_err("负数量应被拒");
    assert_eq!(negative.status, 400);
    assert_eq!(negative.code, error_code::PARAM_INVALID);

    let over_precision = handler::create_forecast(None, req(Some(cus), None, "1000.00", Some("1.00001")))
        .await
        .expect_err("5 位小数应被拒");
    assert_eq!(over_precision.status, 400);

    assert_eq!(row_count(cus, PERIOD).await, 0, "入口校验失败不许落库");

    println!("[M3-20] 负数量 / 超精度 → 400 且零落库 ✓");
}

/// 同（客户, 物料, 期间, 版本）第二条 → 409（V0318 的唯一索引仍在）。
#[tokio::test]
async fn duplicate_dimension_period_version_is_409() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C4").await;
    let mat = material("M320-M4").await;

    handler::create_forecast(None, req(Some(cus), Some(mat), "1000.00", Some("10")))
        .await
        .expect("第一条 v1");

    let dupe = handler::create_forecast(None, req(Some(cus), Some(mat), "2000.00", Some("20")))
        .await
        .expect_err("同维度同期间同版本第二条必须被拒");
    assert_eq!(dupe.status, 409, "唯一索引冲突要翻成 409，不是 500");
    assert_eq!(dupe.code, error_code::RESOURCE_CONFLICT);

    // 换版本号就能进（滚动预测 = 新行）
    let mut v2 = req(Some(cus), Some(mat), "2000.00", Some("20"));
    v2.version_no = Some(2);
    handler::create_forecast(None, v2).await.expect("换版本号可建");

    assert_eq!(row_count(cus, PERIOD).await, 2, "v1 + v2 两行");
    println!("[M3-20] 同维度同版本 → 409；换版本号可建 ✓");
}

/// 版本链：v2 指向 v1，`version_no` 必须 2；当前版本视图给 v2。
#[tokio::test]
async fn version_chain_links_previous_version() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C5").await;
    let mat = material("M320-M5").await;

    let v1 = handler::create_forecast(None, req(Some(cus), Some(mat), "100000.00", Some("4000")))
        .await
        .expect("v1");

    let mut v2_req = req(Some(cus), Some(mat), "150000.00", Some("6000"));
    v2_req.version_no = Some(2);
    v2_req.revised_from_id = Some(v1.id.clone());
    v2_req.confidence = Some(CONFIDENCE_HIGH.to_string());
    let v2 = handler::create_forecast(None, v2_req).await.expect("v2 带版本链");

    assert_eq!(v2.version_no, 2);
    assert_eq!(v2.revised_from_id.as_deref(), Some(v1.id.as_str()), "版本链必须指回 v1");
    assert_eq!(v2.forecast_qty, dec("6000"));

    // 当前版本视图（下游 M3-21 的口径）：同维度同期间只出 v2
    let current = model::find_current_forecast(Some(cus), Some(mat), PERIOD)
        .await
        .expect("查当前版本")
        .expect("有当前版本");
    assert_eq!(current.id.to_string(), v2.id, "当前版本 = 最大 version_no");
    assert_eq!(current.version_no, 2);
    assert_eq!(current.revised_from_id.map(|id| id.to_string()), Some(v1.id.clone()));

    println!("[M3-20] 版本链 v1 → v2 ✓，当前版本视图给 v2 ✓");
}

/// 四种坏链都必须 400（不是 500、不是静默写进库）。
#[tokio::test]
async fn version_chain_rejects_bad_links() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C6").await;
    let other_cus = customer("M320-C6B").await;
    let mat = material("M320-M6").await;

    let v1 = handler::create_forecast(None, req(Some(cus), Some(mat), "100000.00", Some("4000")))
        .await
        .expect("v1");

    // ① 自指：改自己指向自己
    let self_ref = handler::update_forecast(
        None,
        &v1.id,
        ForecastUpdateReq {
            customer_id: None,
            material_id: None,
            period_code: None,
            forecast_amount: None,
            forecast_qty: None,
            confidence: None,
            version_no: None,
            revised_from_id: Some(Some(v1.id.clone())),
            remark: None,
        },
    )
    .await
    .expect_err("自指必须被拒");
    assert_eq!(self_ref.status, 400);
    assert_eq!(self_ref.code, error_code::PARAM_INVALID);

    // ② 指向不存在的预测
    let mut ghost = req(Some(cus), Some(mat), "1.00", Some("1"));
    ghost.version_no = Some(2);
    ghost.revised_from_id = Some(Uuid::now_v7().to_string());
    assert_eq!(
        handler::create_forecast(None, ghost).await.expect_err("幽灵链").status,
        400
    );

    // ③ 跨维度（客户不同）
    let mut cross = req(Some(other_cus), Some(mat), "1.00", Some("1"));
    cross.version_no = Some(2);
    cross.revised_from_id = Some(v1.id.clone());
    let cross_error = handler::create_forecast(None, cross).await.expect_err("跨维度必须被拒");
    assert_eq!(cross_error.status, 400);
    assert_eq!(cross_error.code, error_code::PARAM_INVALID);

    // ④ 版本号跳号（上一版是 v1，却建 v3）
    let mut skipped = req(Some(cus), Some(mat), "1.00", Some("1"));
    skipped.version_no = Some(3);
    skipped.revised_from_id = Some(v1.id.clone());
    assert_eq!(
        handler::create_forecast(None, skipped).await.expect_err("跳号").status,
        400
    );

    // 坏链一行都没落库：同维度同期间只有 v1
    assert_eq!(row_count(cus, PERIOD).await, 1);
    assert_eq!(row_count(other_cus, PERIOD).await, 0);

    // 正对照：v2 版本号对得上就能建
    let mut good = req(Some(cus), Some(mat), "1.00", Some("1"));
    good.version_no = Some(2);
    good.revised_from_id = Some(v1.id.clone());
    handler::create_forecast(None, good).await.expect("v2 合法链");

    println!("[M3-20] 自指 / 幽灵 / 跨维度 / 跳号 四种坏链 → 400，零落库 ✓");
}

/// 软删当前版本 ⇒ 当前版本视图回落上一版（历史不会被一起删掉）。
#[tokio::test]
async fn soft_deleted_latest_falls_back_to_previous() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C7").await;
    let mat = material("M320-M7").await;

    let v1 = handler::create_forecast(None, req(Some(cus), Some(mat), "100000.00", Some("4000")))
        .await
        .expect("v1");
    let mut v2_req = req(Some(cus), Some(mat), "120000.00", Some("5000"));
    v2_req.version_no = Some(2);
    v2_req.revised_from_id = Some(v1.id.clone());
    let v2 = handler::create_forecast(None, v2_req).await.expect("v2");

    handler::delete_forecast(None, &v2.id).await.expect("软删 v2");

    let current = model::find_current_forecast(Some(cus), Some(mat), PERIOD)
        .await
        .expect("查当前版本")
        .expect("软删最新版后应回落到 v1");
    assert_eq!(current.id.to_string(), v1.id, "当前版本回落到 v1");
    assert_eq!(current.version_no, 1);

    // 物理行还在（软删不是删行，审计要能查）
    assert_eq!(row_count(cus, PERIOD).await, 2);
    println!("[M3-20] 软删 v2 → 当前版本回落 v1 ✓（物理两行都还在）");
}

/// 查表能列出全部版本；当前版本视图只有一条 —— 下游 SUM 不会被历史版本重复累加。
#[tokio::test]
async fn current_view_stays_out_of_list_ordering() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C8").await;
    let mat = material("M320-M8").await;

    let v1 = handler::create_forecast(None, req(Some(cus), Some(mat), "100000.00", Some("4000")))
        .await
        .expect("v1");
    let mut v2_req = req(Some(cus), Some(mat), "110000.00", Some("4400"));
    v2_req.version_no = Some(2);
    v2_req.revised_from_id = Some(v1.id.clone());
    let v2 = handler::create_forecast(None, v2_req).await.expect("v2");

    let mut v3_req = req(Some(cus), Some(mat), "120000.00", Some("4800"));
    v3_req.version_no = Some(3);
    v3_req.revised_from_id = Some(v2.id.clone());
    handler::create_forecast(None, v3_req).await.expect("v3");

    // 查表（列表口径）：三个版本都在
    let page = handler::page_forecasts(ForecastPageQuery {
        period_code: Some(PERIOD.to_string()),
        customer_id: Some(cus.to_string()),
        page: Some(1),
        page_size: Some(50),
        ..Default::default()
    })
    .await
    .expect("列表");
    assert_eq!(page.total, 3, "列表要能看到三个历史版本（版本历史是可见的）");

    // 当前版本视图：只有一条，且是 v3
    let current = model::find_current_forecast(Some(cus), Some(mat), PERIOD)
        .await
        .expect("查当前版本")
        .expect("有当前版本");
    assert_eq!(current.version_no, 3, "当前版本 = v3");

    println!("[M3-20] 列表 3 条历史版本 vs 当前版本视图 1 条 ✓");
}

/// 物料维度筛选精确命中（月度窗口不变）；全局口径（双空维度）也能取当前版本。
#[tokio::test]
async fn material_dimension_filter_is_exact() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-C9").await;
    let mat_a = material("M320-M9A").await;
    let mat_b = material("M320-M9B").await;

    handler::create_forecast(None, req(Some(cus), Some(mat_a), "30000.00", Some("100")))
        .await
        .expect("物料 A 预测");
    handler::create_forecast(None, req(Some(cus), Some(mat_b), "40000.00", Some("200")))
        .await
        .expect("物料 B 预测");

    let only_a = handler::page_forecasts(ForecastPageQuery {
        period_code: Some(PERIOD.to_string()),
        material_id: Some(mat_a.to_string()),
        page: Some(1),
        page_size: Some(50),
        ..Default::default()
    })
    .await
    .expect("按物料筛选");
    assert_eq!(only_a.total, 1, "物料维度筛选只命中该物料");
    assert_eq!(only_a.list[0].material_id.as_deref(), Some(mat_a.to_string().as_str()));
    assert_eq!(only_a.list[0].forecast_qty, dec("100"));

    // 全局口径（客户 / 物料都空 = 全局滚动预测）：走当前版本视图也能取到
    let mut global = req(None, None, "900000.00", Some("1"));
    global.period_code = PERIOD_GLOBAL.to_string();
    handler::create_forecast(None, global).await.expect("全局预测");

    let global_current = model::find_current_forecast(None, None, PERIOD_GLOBAL)
        .await
        .expect("查全局当前版本")
        .expect("全局行必须能取到（双空维度的 IS NOT DISTINCT FROM 匹配）");
    assert_eq!(global_current.forecast_amount, dec("900000.00"));

    println!("[M3-20] 物料维度筛选精确 + 全局口径可取 ✓");
}

/// 更新路径：只改数量、断链（`revised_from_id = null`）都要能落地。
#[tokio::test]
async fn update_qty_and_break_chain() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M320-CA").await;
    let mat = material("M320-MA").await;

    let v1 = handler::create_forecast(None, req(Some(cus), Some(mat), "10000.00", Some("100")))
        .await
        .expect("v1");
    let mut v2_req = req(Some(cus), Some(mat), "12000.00", Some("120"));
    v2_req.version_no = Some(2);
    v2_req.revised_from_id = Some(v1.id.clone());
    let v2 = handler::create_forecast(None, v2_req).await.expect("v2");

    // 只改数量：版本链 / 金额 / 版本号都不动
    let updated = handler::update_forecast(
        None,
        &v2.id,
        ForecastUpdateReq {
            customer_id: None,
            material_id: None,
            period_code: None,
            forecast_amount: None,
            forecast_qty: Some(dec("125")),
            confidence: None,
            version_no: None,
            revised_from_id: None,
            remark: None,
        },
    )
    .await
    .expect("改数量");
    assert_eq!(updated.forecast_qty, dec("125"));
    assert_eq!(updated.forecast_amount, dec("12000.00"), "没传的金额不许被清");
    assert_eq!(
        updated.revised_from_id.as_deref(),
        Some(v1.id.as_str()),
        "没传的链不许断"
    );
    assert_eq!(updated.version_no, 2);

    // 断链：显式 null
    let broken = handler::update_forecast(
        None,
        &v2.id,
        ForecastUpdateReq {
            customer_id: None,
            material_id: None,
            period_code: None,
            forecast_amount: None,
            forecast_qty: None,
            confidence: None,
            version_no: None,
            revised_from_id: Some(None), // 显式 null = 清空（断链）
            remark: None,
        },
    )
    .await
    .expect("断链");
    assert!(broken.revised_from_id.is_none());

    println!("[M3-20] 改数量 / 断链 ✓");
}

/// 清场：夹具行必须 0 残留。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;

    let rows = db()
        .query_sql(
            format!(
                "SELECT count(*) AS n FROM biz_sales_forecast WHERE \
                   customer_id IN (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%') \
                      OR period_code IN ('{PERIOD}', '{PERIOD_GLOBAL}')"
            ),
            vec![],
        )
        .await
        .expect("清场计数");
    let left: i64 = rows
        .first()
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()
        .expect("读计数")
        .unwrap_or(0);
    assert_eq!(left, 0, "清场残留");
    let customers = test_seed::customer_count(CUS_PREFIX).await;
    assert_eq!(customers, 0, "M320- 夹具客户应清零（残留会让演练净增量非 0）");
    println!("[M3-20] 清场完成：预测 0 行、夹具客户 0 行 ✓");
}

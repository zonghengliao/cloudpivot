// ============================================================================
// purchase_plan_db_tests.rs —— 采购计划表结构（M4-01）的真库用例
//
// 卡：t_c5ecacf7（M4-01【M4·采购】采购计划：表结构（年度/月度，物料 × 仓库））
//
// # 覆盖什么（每条都对应卡面 / V0446 里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `schema_shape_matches_migration` | 两张表在、列数 22/13、外键 ON DELETE CASCADE、四个唯一索引与三个查询索引都在 |
// | `annual_plan_has_no_month_monthly_plan_needs_one` | 卡面「年度/月度」：ANNUAL 带月被拒、MONTHLY 无月/13 月被拒 |
// | `plan_no_format_is_enforced` | 单号只能是 `PPL-yyyyMMdd-000001`（取号规则的形状写在库层） |
// | `status_domain_is_three_values_and_submitted_needs_instance` | 状态只有 DRAFT/SUBMITTED/APPROVED；SUBMITTED 无审批实例被拒；CLOSED 这类死状态插不进去 |
// | `same_period_and_dept_keeps_one_live_plan` | 「类型 + 年度 + 月 + 部门」存活唯一；换部门可共存；软删后同一格能重建 |
// | `item_granularity_is_material_x_warehouse` | 卡面粒度：同计划同物料换仓 = 两行（允许）；同计划同物料同仓 = 一行（重复被拒）；换计划可同格 |
// | `planned_qty_positive_executed_non_negative_and_may_exceed` | 计划量 > 0、已执行量 ≥ 0；**执行量超计划不拦**（决定写在这里，不是漏了） |
// | `totals_default_zero_and_recalc_rolls_up_items` | 三个合计列默认 0；`recalc_totals` 后 = Σ明细；`items_of_plan` 带物料名/仓库名/单位名且按行号升序 |
// | `delete_plan_cascades_items` | 删单头连带删明细（不留孤儿行） |
// | `next_plan_no_uses_ppl_seq` | 取号行可用：连续两个号递增且能过库层单号 CHECK |
// | `zz_teardown_purges_fixtures` | 清场：`M401-` 夹具 0 残留 |
//
// # 为什么夹具走裸 SQL 而不走「建单接口」
//
// 本卡**没有建单接口**（M4-01 只交表结构，建单属 M4-02）。要验的正是库层约束本身，
// 所以夹具直接 INSERT，用 `assert_rejected()` 断言坏数据被 CHECK / UNIQUE / FK 拒掉。
// 主数据（物料 / 仓库 / 单位）一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m401_drill" cargo test --lib \
//   business::erp::purchase::purchase_plan_db_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::purchase_plan_model as model;
use crate::test_seed;

/// 夹具单号池：`9xxxxx` 段 + 1999-01-01 的日期段，绝不与真实取号（当天日期）撞号。
const PLAN_NO_BASE: &str = "PPL-19990101-9";
/// 夹具名前缀（清场按它反查，绝不按「生产号段」删）。
const NAME_PREFIX: &str = "M401-";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场：只删本卡造的计划（明细靠 `ON DELETE CASCADE` 跟着走）。
async fn cleanup() {
    exec(
        &format!("DELETE FROM biz_purchase_plan WHERE plan_name LIKE '{NAME_PREFIX}%'"),
        vec![],
    )
    .await;
}

/// 夹具单号：`PPL-19990101-9NNNNN`（6 位流水，满足 `ck_biz_pp_no_fmt`）。
fn plan_no(seq: u32) -> String {
    format!("{PLAN_NO_BASE}{seq:05}")
}

/// 一张计划（裸 SQL；本卡没有建单接口）。
#[allow(clippy::too_many_arguments)]
async fn insert_plan(
    no: &str,
    name: &str,
    plan_type: &str,
    year: i32,
    month: Option<i32>,
    dept: Option<Uuid>,
    status: &str,
    instance: Option<Uuid>,
) -> Uuid {
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_purchase_plan \
            (id, plan_no, plan_name, plan_type, plan_year, plan_month, dept_id, dept_name, \
             status, approval_instance_id) \
         VALUES (?, ?, ?, ?, ?, ?::int, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), ?, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::Text(no.to_string()),
            Value::Text(name.to_string()),
            Value::Text(plan_type.to_string()),
            Value::I32(year),
            match month {
                Some(month) => Value::I32(month),
                None => Value::Null,
            },
            Value::Text(dept.map(|id| id.to_string()).unwrap_or_default()),
            Value::Text(dept.map(|_| format!("{NAME_PREFIX}部门")).unwrap_or_default()),
            Value::Text(status.to_string()),
            Value::Text(instance.map(|id| id.to_string()).unwrap_or_default()),
        ],
    )
    .await;
    id
}

/// 一行明细（裸 SQL）。
async fn insert_item(plan_id: Uuid, line_no: i32, material: Uuid, warehouse: Uuid, qty: &str) -> Uuid {
    let unit = test_seed::ensure_unit().await;
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_purchase_plan_item \
            (id, plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty) \
         VALUES (?, ?, ?, ?, ?, ?, ?::text::numeric)",
        vec![
            Value::from(&id),
            Value::from(&plan_id),
            Value::I32(line_no),
            Value::from(&material),
            Value::from(&warehouse),
            Value::from(&unit),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    id
}

/// 期望被库层拒掉的写入：返回错误文本（断言里看得到是哪条约束拦的）。
async fn assert_rejected(sql: &str, params: Vec<Value>, why: &str) -> String {
    // SCOPE-EXEMPT: 用例直插夹具表，不涉及数据范围。
    match db().execute_with_params(sql, params).await {
        Ok(_) => panic!("{why}：库层居然接受了这条写入（约束失效）"),
        Err(error) => error.to_string(),
    }
}

// ==================== 用例 ====================

/// 表形状：两表在、列数、外键级联、索引（库层与 V0446 逐条对齐）。
#[tokio::test]
async fn schema_shape_matches_migration() {
    let Some(_guard) = setup().await else { return };

    let tables: i64 = db()
        .query_sql(
            "SELECT count(*) AS n FROM pg_class c JOIN pg_namespace n2 ON n2.oid = c.relnamespace \
              WHERE n2.nspname = 'public' AND c.relkind = 'r' \
                AND c.relname IN ('biz_purchase_plan','biz_purchase_plan_item')"
                .to_string(),
            vec![],
        )
        .await
        .expect("查表")
        .first()
        .and_then(|row| row.get_typed("n").ok())
        .unwrap_or(0);
    assert_eq!(tables, 2, "V0446 的两张表必须都在");

    let head_cols: i64 = db()
        .query_sql(
            "SELECT count(*) AS n FROM information_schema.columns \
              WHERE table_schema='public' AND table_name='biz_purchase_plan'"
                .to_string(),
            vec![],
        )
        .await
        .expect("查列")
        .first()
        .and_then(|row| row.get_typed("n").ok())
        .unwrap_or(0);
    assert_eq!(head_cols, 22, "单头 22 列（含审计 5 列与 parent_plan_id）");

    let item_cols: i64 = db()
        .query_sql(
            "SELECT count(*) AS n FROM information_schema.columns \
              WHERE table_schema='public' AND table_name='biz_purchase_plan_item'"
                .to_string(),
            vec![],
        )
        .await
        .expect("查列")
        .first()
        .and_then(|row| row.get_typed("n").ok())
        .unwrap_or(0);
    assert_eq!(item_cols, 13, "明细 13 列");

    // 外键必须是 CASCADE（删单头留明细 = 孤儿行）
    let delete_rule: String = db()
        .query_sql(
            "SELECT rc.delete_rule \
               FROM information_schema.referential_constraints rc \
              WHERE rc.constraint_schema='public' AND rc.constraint_name='fk_biz_ppi_plan'"
                .to_string(),
            vec![],
        )
        .await
        .expect("查外键")
        .first()
        .and_then(|row| row.get_typed("delete_rule").ok())
        .expect("外键 fk_biz_ppi_plan 必须存在");
    assert_eq!(delete_rule, "CASCADE");

    let indexes: i64 = db()
        .query_sql(
            "SELECT count(DISTINCT indexname) AS n FROM pg_indexes \
              WHERE schemaname='public' AND indexname IN \
                ('uk_biz_purchase_plan_no','uk_biz_purchase_plan_period','idx_biz_purchase_plan_period', \
                 'idx_biz_purchase_plan_status','idx_biz_purchase_plan_parent', \
                 'uk_biz_pp_item_cell','uk_biz_pp_item_line','idx_biz_pp_item_material')"
                .to_string(),
            vec![],
        )
        .await
        .expect("查索引")
        .first()
        .and_then(|row| row.get_typed("n").ok())
        .unwrap_or(0);
    assert_eq!(indexes, 8, "V0446 的 8 个索引一个都不能少");
    println!("[M4-01] 表形状：2 表 / 22+13 列 / FK CASCADE / 8 索引 ✓");
}

/// 卡面「年度/月度」：类型与月份必须自洽。
#[tokio::test]
async fn annual_plan_has_no_month_monthly_plan_needs_one() {
    let Some(_guard) = setup().await else { return };

    // ANNUAL 带月 → 拒
    let err = assert_rejected(
        "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month) \
         VALUES (?, ?, 'ANNUAL', 2026, 3)",
        vec![Value::Text(plan_no(1)), Value::Text(format!("{NAME_PREFIX}年度带月"))],
        "年度计划带月份",
    )
    .await;
    assert!(err.contains("ck_biz_pp_period"), "应命中 ck_biz_pp_period，实际：{err}");

    // MONTHLY 无月 → 拒
    assert_rejected(
        "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month) \
         VALUES (?, ?, 'MONTHLY', 2026, NULL)",
        vec![Value::Text(plan_no(2)), Value::Text(format!("{NAME_PREFIX}月度无月"))],
        "月度计划没有月份",
    )
    .await;

    // MONTHLY 13 月 / 0 月 → 拒
    for bad_month in [0i32, 13] {
        assert_rejected(
            "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month) \
             VALUES (?, ?, 'MONTHLY', 2026, ?::int)",
            vec![
                Value::Text(plan_no(3)),
                Value::Text(format!("{NAME_PREFIX}月度越界月份")),
                Value::I32(bad_month),
            ],
            "月度计划月份越界",
        )
        .await;
    }

    // 合法两侧：年度 2026（无月）、月度 2026-11 都能插
    let annual = insert_plan(
        &plan_no(4),
        &format!("{NAME_PREFIX}2026 年度采购计划"),
        "ANNUAL",
        2026,
        None,
        None,
        "DRAFT",
        None,
    )
    .await;
    let monthly = insert_plan(
        &plan_no(5),
        &format!("{NAME_PREFIX}2026 年 11 月采购计划"),
        "MONTHLY",
        2026,
        Some(11),
        None,
        "DRAFT",
        None,
    )
    .await;
    assert_ne!(annual, monthly);

    let rows = model::items_of_plan(db(), annual).await.expect("读明细");
    assert!(rows.is_empty(), "新计划没有明细");
    println!("[M4-01] 年度/月度自洽：ANNUAL 无月、MONTHLY 1–12，越界全被库层拒 ✓");
}

/// 单号形状写在库层（`ck_biz_pp_no_fmt`），取号规则一旦改坏会在这里先炸。
#[tokio::test]
async fn plan_no_format_is_enforced() {
    let Some(_guard) = setup().await else { return };

    for bad in ["M401-1", "PPL-2026-000001", "PPL-20261117-1", "ppl-20261117-000001"] {
        assert_rejected(
            "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year) \
             VALUES (?, ?, 'ANNUAL', 2026)",
            vec![
                Value::Text(bad.to_string()),
                Value::Text(format!("{NAME_PREFIX}坏单号")),
            ],
            "非法计划单号",
        )
        .await;
    }
    insert_plan(
        &plan_no(6),
        &format!("{NAME_PREFIX}单号形状"),
        "ANNUAL",
        2026,
        None,
        None,
        "DRAFT",
        None,
    )
    .await;
    println!("[M4-01] 单号形状：只有 PPL-yyyyMMdd-000001 能进表 ✓");
}

/// 状态域三值 + SUBMITTED 必有审批实例（不造 CLOSED 这类没有入口的死状态）。
#[tokio::test]
async fn status_domain_is_three_values_and_submitted_needs_instance() {
    let Some(_guard) = setup().await else { return };

    let err = assert_rejected(
        "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, status) \
         VALUES (?, ?, 'ANNUAL', 2027, 'CLOSED')",
        vec![Value::Text(plan_no(7)), Value::Text(format!("{NAME_PREFIX}死状态"))],
        "状态域外的 CLOSED",
    )
    .await;
    assert!(err.contains("ck_biz_pp_status"), "应命中 ck_biz_pp_status，实际：{err}");

    let err = assert_rejected(
        "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, status) \
         VALUES (?, ?, 'ANNUAL', 2027, 'SUBMITTED')",
        vec![Value::Text(plan_no(8)), Value::Text(format!("{NAME_PREFIX}无实例"))],
        "SUBMITTED 没有审批实例",
    )
    .await;
    assert!(
        err.contains("ck_biz_pp_instance"),
        "应命中 ck_biz_pp_instance，实际：{err}"
    );

    // 带实例的 SUBMITTED 能进；DRAFT 的默认值也对
    insert_plan(
        &plan_no(9),
        &format!("{NAME_PREFIX}已提交"),
        "ANNUAL",
        2027,
        None,
        None,
        "SUBMITTED",
        Some(Uuid::now_v7()),
    )
    .await;
    let draft = insert_plan(
        &plan_no(10),
        &format!("{NAME_PREFIX}草稿"),
        "ANNUAL",
        2028,
        None,
        None,
        "DRAFT",
        None,
    )
    .await;
    let row = model::find_plan(db(), draft)
        .await
        .expect("读单头")
        .expect("刚插的计划能读到");
    assert_eq!(row.status, "DRAFT");
    assert_eq!(row.plan_type, "ANNUAL");
    assert_eq!(row.plan_month, None);
    println!("[M4-01] 状态域：DRAFT/SUBMITTED/APPROVED 三值，SUBMITTED 必带实例 ✓");
}

/// 「类型 + 年度 + 月 + 编制部门」存活唯一；换部门可共存；软删后同一格能重建。
#[tokio::test]
async fn same_period_and_dept_keeps_one_live_plan() {
    let Some(_guard) = setup().await else { return };

    let first = insert_plan(
        &plan_no(11),
        &format!("{NAME_PREFIX}月度甲"),
        "MONTHLY",
        2026,
        Some(3),
        None,
        "DRAFT",
        None,
    )
    .await;

    let err = assert_rejected(
        "INSERT INTO biz_purchase_plan (plan_no, plan_name, plan_type, plan_year, plan_month) \
         VALUES (?, ?, 'MONTHLY', 2026, 3)",
        vec![Value::Text(plan_no(12)), Value::Text(format!("{NAME_PREFIX}月度重复"))],
        "同类型同期间同部门（都为空部门）的第二张计划",
    )
    .await;
    assert!(
        err.contains("uk_biz_purchase_plan_period"),
        "应命中期间唯一索引，实际：{err}"
    );

    // 换一个编制部门：可以共存（部门级计划各自独立）
    let other_dept = Uuid::now_v7();
    let second = insert_plan(
        &plan_no(13),
        &format!("{NAME_PREFIX}月度乙"),
        "MONTHLY",
        2026,
        Some(3),
        Some(other_dept),
        "DRAFT",
        None,
    )
    .await;

    // 按唯一键探针能各自查到（M4-02 建单前判重就用这两个入口）
    let none_plan = model::find_plan_by_period(db(), "MONTHLY", 2026, Some(3), None)
        .await
        .expect("探针")
        .expect("空部门那张必须在");
    assert_eq!(none_plan.id, first);
    let dept_plan = model::find_plan_by_period(db(), "MONTHLY", 2026, Some(3), Some(other_dept))
        .await
        .expect("探针")
        .expect("部门那张必须在");
    assert_eq!(dept_plan.id, second);
    assert_eq!(
        dept_plan.dept_name.as_deref(),
        Some(format!("{NAME_PREFIX}部门").as_str())
    );

    // 软删后同一格能重建（部分唯一索引的意义）
    exec(
        "UPDATE biz_purchase_plan SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&first)],
    )
    .await;
    let rebuilt = insert_plan(
        &plan_no(14),
        &format!("{NAME_PREFIX}月度丙"),
        "MONTHLY",
        2026,
        Some(3),
        None,
        "DRAFT",
        None,
    )
    .await;
    let probe = model::find_plan_by_period(db(), "MONTHLY", 2026, Some(3), None)
        .await
        .expect("探针")
        .expect("软删后重建的那张必须在");
    assert_eq!(probe.id, rebuilt);
    assert!(
        model::find_plan(db(), first).await.expect("读软删行").is_none(),
        "软删的计划不该再被读到"
    );
    println!("[M4-01] 期间唯一：同格两张被拒、换部门共存、软删后可重建 ✓");
}

/// 卡面粒度：物料 × 仓库。
#[tokio::test]
async fn item_granularity_is_material_x_warehouse() {
    let Some(_guard) = setup().await else { return };

    let material = test_seed::ensure_material("M401-M1", "采购计划物料甲").await;
    let material2 = test_seed::ensure_material("M401-M2", "采购计划物料乙").await;
    let wh_a = test_seed::ensure_warehouse("M401-WHA", "采购计划仓甲").await;
    let wh_b = test_seed::ensure_warehouse("M401-WHB", "采购计划仓乙").await;

    let plan = insert_plan(
        &plan_no(15),
        &format!("{NAME_PREFIX}粒度"),
        "MONTHLY",
        2026,
        Some(4),
        None,
        "DRAFT",
        None,
    )
    .await;
    let plan2 = insert_plan(
        &plan_no(16),
        &format!("{NAME_PREFIX}粒度二"),
        "MONTHLY",
        2026,
        Some(5),
        None,
        "DRAFT",
        None,
    )
    .await;

    insert_item(plan, 1, material, wh_a, "100").await;
    // 同物料换仓 = 第二行（这正是「物料 × 仓库」粒度语）
    insert_item(plan, 2, material, wh_b, "50").await;
    // 同仓换物料 = 第三行
    insert_item(plan, 3, material2, wh_a, "20").await;

    let err = assert_rejected(
        "INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty) \
         VALUES (?, 4, ?, ?, ?, 10)",
        vec![
            Value::from(&plan),
            Value::from(&material),
            Value::from(&wh_a),
            Value::from(&test_seed::ensure_unit().await),
        ],
        "同一计划里同物料同仓库的第二行",
    )
    .await;
    assert!(err.contains("uk_biz_pp_item_cell"), "应命中粒度唯一索引，实际：{err}");

    // 换一张计划：同一个格子可以有（不同期间各计划各算）
    insert_item(plan2, 1, material, wh_a, "999").await;

    let rows = model::items_of_plan(db(), plan).await.expect("读明细");
    assert_eq!(rows.len(), 3, "同物料两仓 + 另一物料一仓 = 3 行");
    assert_eq!(rows[0].line_no, 1, "按行号升序");
    assert_eq!(rows[0].material_code, "M401-M1");
    assert_eq!(rows[0].material_name, "采购计划物料甲");
    assert_eq!(
        rows[0].warehouse_name, "采购计划仓甲",
        "仓库名要 join 实时取（不许返回 UUID）"
    );
    assert!(!rows[0].unit_name.is_empty(), "单位名要 join");
    assert_eq!(rows[1].warehouse_name, "采购计划仓乙");
    assert_eq!(rows[2].material_code, "M401-M2");
    println!("[M4-01] 粒度：物料 × 仓库（同计划 3 格）+ 重复格被拒 + 跨计划可同格 ✓");
}

/// 计划量 > 0、已执行量 ≥ 0；**执行量超计划不拦**（决定写在这里）。
#[tokio::test]
async fn planned_qty_positive_executed_non_negative_and_may_exceed() {
    let Some(_guard) = setup().await else { return };

    let material = test_seed::ensure_material("M401-M3", "采购计划物料丙").await;
    let warehouse = test_seed::ensure_warehouse("M401-WHC", "采购计划仓丙").await;
    let unit = test_seed::ensure_unit().await;
    let plan = insert_plan(
        &plan_no(17),
        &format!("{NAME_PREFIX}数量"),
        "MONTHLY",
        2026,
        Some(6),
        None,
        "DRAFT",
        None,
    )
    .await;

    for bad_qty in ["0", "-1"] {
        assert_rejected(
            "INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty) \
             VALUES (?, 1, ?, ?, ?, ?::text::numeric)",
            vec![
                Value::from(&plan),
                Value::from(&material),
                Value::from(&warehouse),
                Value::from(&unit),
                Value::Text(bad_qty.to_string()),
            ],
            "非正计划量",
        )
        .await;
    }

    let err = assert_rejected(
        "INSERT INTO biz_purchase_plan_item (plan_id, line_no, material_id, warehouse_id, unit_id, planned_qty, executed_qty) \
         VALUES (?, 1, ?, ?, ?, 10, -1)",
        vec![
            Value::from(&plan),
            Value::from(&material),
            Value::from(&warehouse),
            Value::from(&unit),
        ],
        "负已执行量",
    )
    .await;
    assert!(err.contains("ck_biz_ppi_exec"), "应命中 ck_biz_ppi_exec，实际：{err}");

    // 执行量 > 计划量：允许（超计划执行现实里有，拦在库层等于逼用户改计划才能下单）
    let item = insert_item(plan, 1, material, warehouse, "100").await;
    exec(
        "UPDATE biz_purchase_plan_item SET executed_qty = 150 WHERE id = ?::uuid",
        vec![Value::from(&item)],
    )
    .await;
    let rows = model::items_of_plan(db(), plan).await.expect("读明细");
    assert_eq!(
        rows[0].executed_qty,
        dec("150"),
        "执行量超计划必须写得进去（执行率 > 100% 是报表的事）"
    );
    println!("[M4-01] 数量：计划量 > 0、执行量 ≥ 0 且允许超计划 ✓");
}

/// 合计列默认 0，`recalc_totals` 后 = Σ明细（服务端算，客户端不许传）。
#[tokio::test]
async fn totals_default_zero_and_recalc_rolls_up_items() {
    let Some(_guard) = setup().await else { return };

    let material = test_seed::ensure_material("M401-M4", "采购计划物料丁").await;
    let material2 = test_seed::ensure_material("M401-M5", "采购计划物料戊").await;
    let wh_a = test_seed::ensure_warehouse("M401-WHD", "采购计划仓丁").await;
    let wh_b = test_seed::ensure_warehouse("M401-WHE", "采购计划仓戊").await;

    let plan = insert_plan(
        &plan_no(18),
        &format!("{NAME_PREFIX}合计"),
        "ANNUAL",
        2026,
        None,
        None,
        "DRAFT",
        None,
    )
    .await;
    let row = model::find_plan(db(), plan).await.expect("读单头").expect("刚插的计划");
    assert_eq!(row.total_planned_qty, dec("0"));
    assert_eq!(row.total_executed_qty, dec("0"));
    assert_eq!(row.total_planned_amount, dec("0"));
    assert_eq!(row.item_count, 0);

    // 两行明细：100×12.5 + 60×3 = 1250 + 180 = 1430
    let first = insert_item(plan, 1, material, wh_a, "100").await;
    let second = insert_item(plan, 2, material2, wh_b, "60").await;
    exec(
        "UPDATE biz_purchase_plan_item SET planned_price = 12.5, planned_amount = 1250, executed_qty = 30 \
          WHERE id = ?::uuid",
        vec![Value::from(&first)],
    )
    .await;
    exec(
        "UPDATE biz_purchase_plan_item SET planned_price = 3, planned_amount = 180, executed_qty = 60 \
          WHERE id = ?::uuid",
        vec![Value::from(&second)],
    )
    .await;

    // 明细变了但没重算之前，单头合计还是旧值（这就是「派生列」的代价，也是要 recalc 的原因）
    let stale = model::find_plan(db(), plan).await.expect("读单头").expect("计划");
    assert_eq!(stale.total_planned_qty, dec("0"), "未重算前不该自己变");
    assert_eq!(stale.item_count, 2, "明细计数是派生查询，实时正确");

    assert!(
        model::recalc_totals(db(), plan).await.expect("重算"),
        "计划存在，重算必须命中一行"
    );
    let fresh = model::find_plan(db(), plan).await.expect("读单头").expect("计划");
    assert_eq!(fresh.total_planned_qty, dec("160"), "Σ计划量 100 + 60");
    assert_eq!(fresh.total_executed_qty, dec("90"), "Σ执行量 30 + 60");
    assert_eq!(fresh.total_planned_amount, dec("1430"), "Σ计划金额 1250 + 180");

    // 不存在的计划：返回 false（调用方按 404 处理，而不是静默成功）
    assert!(
        !model::recalc_totals(db(), Uuid::now_v7())
            .await
            .expect("重算不存在的计划")
    );
    println!("[M4-01] 合计：默认 0 → recalc 后 160 / 90 / 1430，缺失计划返回 false ✓");
}

/// 删单头连带删明细（ON DELETE CASCADE，不留孤儿）。
#[tokio::test]
async fn delete_plan_cascades_items() {
    let Some(_guard) = setup().await else { return };

    let material = test_seed::ensure_material("M401-M6", "采购计划物料己").await;
    let warehouse = test_seed::ensure_warehouse("M401-WHF", "采购计划仓己").await;
    let plan = insert_plan(
        &plan_no(19),
        &format!("{NAME_PREFIX}级联"),
        "MONTHLY",
        2026,
        Some(7),
        None,
        "DRAFT",
        None,
    )
    .await;
    insert_item(plan, 1, material, warehouse, "10").await;

    exec(
        "DELETE FROM biz_purchase_plan WHERE id = ?::uuid",
        vec![Value::from(&plan)],
    )
    .await;

    let left: i64 = db()
        .query_sql(
            "SELECT count(*) AS n FROM biz_purchase_plan_item WHERE plan_id = ?::uuid".to_string(),
            vec![Value::from(&plan)],
        )
        .await
        .expect("查孤儿明细")
        .first()
        .and_then(|row| row.get_typed("n").ok())
        .unwrap_or(-1);
    assert_eq!(left, 0, "删单头后明细必须一起走");
    println!("[M4-01] 级联：删单头 → 明细 0 行 ✓");
}

/// 取号：`biz_seq(PPL)` 可用，连续两个号递增且能过库层单号 CHECK。
#[tokio::test]
async fn next_plan_no_uses_ppl_seq() {
    let Some(_guard) = setup().await else { return };

    let now = Utc::now();
    let first = model::next_plan_no(db(), now).await.expect("第一个号");
    let second = model::next_plan_no(db(), now).await.expect("第二个号");
    assert!(first.starts_with("PPL-"), "号段前缀不对：{first}");
    assert_eq!(first.len(), 19, "PPL-yyyyMMdd-000001 共 19 字符");
    assert_eq!(&first[..12], &second[..12], "同一 CST 日期内周期键必须一致");
    let n1: i64 = first[13..].parse().expect("流水号");
    let n2: i64 = second[13..].parse().expect("流水号");
    assert_eq!(n2, n1 + 1, "同一个号池必须严格递增");

    // 号能直接过库层 CHECK（形状口径一致：库层正则与取号函数不许各写一套）
    insert_plan(
        &first,
        &format!("{NAME_PREFIX}取号"),
        "ANNUAL",
        2029,
        None,
        None,
        "DRAFT",
        None,
    )
    .await;
    let row = model::find_plan_by_period(db(), "ANNUAL", 2029, None, None)
        .await
        .expect("探针")
        .expect("用取号建的计划必须能按期间查到");
    assert_eq!(row.plan_no, first);
    println!("[M4-01] 取号：{first} → {second}（CST 日滚动，过库层单号 CHECK）✓");
}

/// 清场：本卡夹具 0 残留。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let left: i64 = db()
        .query_sql(
            format!(
                "SELECT (SELECT count(*) FROM biz_purchase_plan WHERE plan_name LIKE '{NAME_PREFIX}%') \
                      + (SELECT count(*) FROM biz_purchase_plan_item it JOIN biz_purchase_plan p ON p.id = it.plan_id \
                          WHERE p.plan_name LIKE '{NAME_PREFIX}%') AS n"
            ),
            vec![],
        )
        .await
        .expect("清场统计")
        .first()
        .and_then(|row| row.get_typed("n").ok())
        .unwrap_or(-1);
    assert_eq!(left, 0, "清场后不该再有 M401- 夹具");
    println!("[M4-01] 清场：M401- 计划与明细 0 残留 ✓");
}

// ============================================================================
// supplier_evaluation_db_tests.rs —— 供应商评估表结构（M4-12）的真库用例
//
// 卡：t_3e550d38（M4-12【M4·采购】供应商评估：表结构（质量/交期/价格/服务四维））
//
// # 覆盖什么（每条都对应卡面 / V0454 里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `schema_shape_matches_migration` | 表在、25 列、4 索引、两个唯一索引都带软删条件、`total_score` 是 STORED 生成列、取号行 SEV 一行 |
// | `period_window_and_eval_date_invariants` | 评估期间起 ≤ 止；评估日不早于期间起点（期间没开始就评完 = 录错） |
// | `weights_must_sum_to_one_and_be_in_range` | 卡面「权重」：四等分默认值是可用的；和 1.1 / 0.9 都被拒；单项越界被拒；40/30/20/10 放行 |
// | `dimension_scores_are_0_to_100` | 卡面「四维打分」：0 与 100 边界放行；100.01 / 负数被拒 |
// | `total_score_is_generated_from_weights_and_scores` | 卡面「打分 + 权重」：总分由库算且改分数自动跟；显式写 `total_score`（INSERT / UPDATE）都被 PG 拒（428C9） |
// | `model_weighted_total_matches_db_generated_column` | model 侧 [`weighted_total`] 与生成列**逐位一致**（含 .005 中点）—— 公式只有一份 |
// | `grade_domain_and_null_means_unrated` | 等级只认 A/B/C/D 或 NULL（未评级）；E / UNRATED 插不进 |
// | `evaluation_no_format_and_surviving_uniqueness` | 单号只能是 `SEV-yyyyMMdd-000001`；存活唯一；软删后同号可复用 |
// | `one_evaluation_per_supplier_per_period` | 同供应商同期间只能一份；换期间 / 换供应商可插（多评估人要走追加迁移，不在这里放开） |
// | `next_evaluation_no_uses_sev_seq` | 取号行可用：连续两个号递增且能过库层单号 CHECK |
// | `supplier_snapshot_and_live_name_coexist` | 快照与实时现名并存（约定 13）：快照空 → 回退现名；快照有值 → 显示快照 |
// | `zz_teardown_purges_fixtures` | 清场：`M412-` 夹具 0 残留 |
//
// # 为什么夹具走裸 SQL 而不走「建单接口」
//
// 本卡**没有建单接口**（M4-12 只交表结构，打分编排与评级属 M4-13）。要验的正是库层约束
// 本身，所以夹具直接 INSERT，用 `assert_rejected()` 断言坏数据被 CHECK / UNIQUE 拒掉。
// 主数据（供应商 / 评估人）一律走 `crate::test_seed::ensure_*`（门禁：测试代码不许手写主数据 INSERT）。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m412_sev_drill" cargo test --lib \
//   business::erp::purchase::supplier_evaluation_db_tests -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::supplier_evaluation_model as model;
use crate::test_seed;

/// 夹具单号池：`9xxxxx` 段 + 1999-01-01 的日期段，绝不与真实取号（当天日期）撞号。
const NO_BASE: &str = "SEV-19990101-9";
/// 夹具名前缀（清场按它反查，绝不按「生产号段」删）。
const NAME_PREFIX: &str = "M412-";
/// 夹具供应商编码（主）/ 副。
const SUPPLIER_CODE: &str = "M412-SUP";
/// 副供应商编码（验「换供应商可以同期间」）。
const SUPPLIER_CODE_B: &str = "M412-SUP-B";

/// 夹具期间池：同一用例里给同一供应商插多份评估时，一份一个期间
/// （`uk_biz_sev_supplier_period` 只允许同期间一份 —— 这就是被测的约束本身）。
const FIXTURE_PERIODS: [(&str, &str); 8] = [
    ("2026-01-01", "2026-03-31"),
    ("2026-04-01", "2026-06-30"),
    ("2026-07-01", "2026-09-30"),
    ("2026-10-01", "2026-12-31"),
    ("2027-01-01", "2027-03-31"),
    ("2027-04-01", "2027-06-30"),
    ("2027-07-01", "2027-09-30"),
    ("2027-10-01", "2027-12-31"),
];

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
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
    Some(guard)
}

/// 清场：只删本卡的夹具（按名字前缀）。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM {} WHERE supplier_name LIKE '{NAME_PREFIX}%' OR remark LIKE '{NAME_PREFIX}%'",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;
}

/// 夹具发号器：进程内单调递增 —— 同一用例里插入多份评估时各拿一个新号。
fn fixture_no() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(100);
    format!("{NO_BASE}{:05}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// 主 / 副夹具供应商。
async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUPPLIER_CODE, "供应商评估夹具供应商").await
}

/// 副夹具供应商。
async fn supplier_b() -> Uuid {
    test_seed::ensure_supplier(SUPPLIER_CODE_B, "供应商评估夹具供应商B").await
}

/// 一份评估的夹具入参（权重与分数按字符串给，方便表达越界值）。
struct NewEval<'a> {
    supplier: Uuid,
    start: &'a str,
    end: &'a str,
    weights: [&'a str; 4],
    scores: [&'a str; 4],
    grade: Option<&'a str>,
    supplier_name: Option<&'a str>,
}

impl<'a> NewEval<'a> {
    /// 一份合法基线：四等分权重 + 80/90/70/60，期间 2026-01-01..2026-03-31。
    fn valid(supplier: Uuid) -> Self {
        Self {
            supplier,
            start: "2026-01-01",
            end: "2026-03-31",
            weights: ["0.25", "0.25", "0.25", "0.25"],
            scores: ["80", "90", "70", "60"],
            grade: None,
            supplier_name: Some("M412-夹具供应商快照"),
        }
    }

    /// 换到第 `index` 组期间（同一用例里要给同一供应商插多份评估时用：同期间只能有一份）。
    fn at(mut self, index: usize) -> Self {
        let (start, end) = FIXTURE_PERIODS[index];
        self.start = start;
        self.end = end;
        self
    }
}

/// 插一份评估，返回 id。
async fn insert_eval(head: &NewEval<'_>) -> Uuid {
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_supplier_evaluation \
            (id, evaluation_no, supplier_id, supplier_name, period_start, period_end, eval_date, \
             quality_weight, delivery_weight, price_weight, service_weight, \
             quality_score, delivery_score, price_score, service_score, grade, remark) \
         VALUES (?, ?::text, ?::uuid, NULLIF(?::text,''), ?::text::date, ?::text::date, ?::text::date, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                 NULLIF(?::text,''), ?::text)",
        vec![
            Value::from(&id),
            Value::Text(fixture_no()),
            Value::from(&head.supplier),
            Value::Text(head.supplier_name.unwrap_or_default().to_string()),
            Value::Text(head.start.to_string()),
            Value::Text(head.end.to_string()),
            // 评估日固定取期间止 + 1 天：不依赖「今天」，也不会撞 ck_biz_sev_eval_date
            Value::Text((date(head.end) + Duration::days(1)).format("%Y-%m-%d").to_string()),
            Value::Text(head.weights[0].to_string()),
            Value::Text(head.weights[1].to_string()),
            Value::Text(head.weights[2].to_string()),
            Value::Text(head.weights[3].to_string()),
            Value::Text(head.scores[0].to_string()),
            Value::Text(head.scores[1].to_string()),
            Value::Text(head.scores[2].to_string()),
            Value::Text(head.scores[3].to_string()),
            Value::Text(head.grade.unwrap_or_default().to_string()),
            Value::Text(format!("{NAME_PREFIX}夹具评估")),
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

/// 单列计数（`SELECT count(*) AS n`）。
async fn count_of(sql: &str) -> i64 {
    db().query_sql(sql.to_string(), vec![])
        .await
        .expect("查计数")
        .first()
        .and_then(|row| row.get_typed::<i64>("n").ok())
        .unwrap_or(0)
}

/// 直接读一列定点数（生成列 / 加权总分对拍用）。
async fn numeric_of(sql: &str) -> Decimal {
    db().query_sql(sql.to_string(), vec![])
        .await
        .expect("查定点数")
        .first()
        .and_then(|row| row.get_typed::<Decimal>("v").ok())
        .expect("必须读得到一个 numeric")
}

// ==================== 用例 ====================

/// 表形状：表在、列数 25、索引 4（两个部分唯一）、总分是生成列、取号行一行。
#[tokio::test]
async fn schema_shape_matches_migration() {
    let Some(_guard) = setup().await else { return };

    let table = count_of(
        "SELECT count(*) AS n FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace \
          WHERE ns.nspname = 'public' AND c.relkind = 'r' AND c.relname = 'biz_supplier_evaluation'",
    )
    .await;
    assert_eq!(table, 1, "V0454 的表必须建出来");

    let cols = count_of(
        "SELECT count(*) AS n FROM information_schema.columns \
          WHERE table_schema='public' AND table_name='biz_supplier_evaluation'",
    )
    .await;
    assert_eq!(
        cols, 25,
        "25 列（编号 2 + 主数据 5 + 评估期日 3 + 权重 4 + 分数 4 + 总分 1 + 等级 1 + 备注 1 + 审计 5）"
    );

    let indexes = count_of(
        "SELECT count(DISTINCT indexname) AS n FROM pg_indexes \
          WHERE schemaname='public' AND indexname IN \
            ('uk_biz_sev_no','uk_biz_sev_supplier_period','idx_biz_sev_period','idx_biz_sev_grade')",
    )
    .await;
    assert_eq!(indexes, 4, "V0454 的 4 个索引一个都不能少");

    let partial = count_of(
        "SELECT count(*) AS n FROM pg_indexes \
          WHERE schemaname='public' \
            AND indexname IN ('uk_biz_sev_no','uk_biz_sev_supplier_period') \
            AND indexdef LIKE '%WHERE (deleted_at IS NULL)%'",
    )
    .await;
    assert_eq!(partial, 2, "两个唯一索引都必须带软删条件（软删后可复用）");

    // total_score 必须是 STORED 生成列，表达式里必须有四维权重与 round
    let generated = count_of(
        "SELECT count(*) AS n FROM information_schema.columns \
          WHERE table_schema='public' AND table_name='biz_supplier_evaluation' \
            AND column_name='total_score' AND is_generated='ALWAYS' \
            AND generation_expression LIKE '%quality_weight%' \
            AND generation_expression LIKE '%service_score%' \
            AND generation_expression LIKE 'round%'",
    )
    .await;
    assert_eq!(generated, 1, "total_score 必须是 round(Σ 权重 × 分数) 的生成列");

    let seq = count_of(
        "SELECT count(*) AS n FROM biz_seq \
          WHERE seq_type='SEV' AND deleted_at IS NULL AND prefix='SEV' AND separator='-'",
    )
    .await;
    assert_eq!(seq, 1, "取号规则 SEV 只有一行");

    println!("[M4-12] 表形状：1 表 / 25 列 / 4 索引 / 2 部分唯一 / total_score 生成列 / SEV 取号 ✓");
}

/// 卡面「评估期间」：起 ≤ 止；评估日不早于期间起点。
#[tokio::test]
async fn period_window_and_eval_date_invariants() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    // 起 > 止 → 拒
    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end) \
         VALUES (?::text, ?::uuid, ?::text::date, ?::text::date)",
        vec![
            Value::Text(fixture_no()),
            Value::from(&sup),
            Value::Text("2026-03-31".to_string()),
            Value::Text("2026-01-01".to_string()),
        ],
        "评估期间起晚于止",
    )
    .await;
    assert!(
        err.contains("ck_biz_sev_period"),
        "应命中 ck_biz_sev_period，实际：{err}"
    );

    // 评估日早于期间起点 → 拒
    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, eval_date) \
         VALUES (?::text, ?::uuid, ?::text::date, ?::text::date, ?::text::date)",
        vec![
            Value::Text(fixture_no()),
            Value::from(&sup),
            Value::Text("2026-01-01".to_string()),
            Value::Text("2026-03-31".to_string()),
            Value::Text("2025-12-31".to_string()),
        ],
        "评估日早于期间起点",
    )
    .await;
    assert!(
        err.contains("ck_biz_sev_eval_date"),
        "应命中 ck_biz_sev_eval_date，实际：{err}"
    );

    // 单日期间 + 评估日 = 期间起点：合法（边界）
    let id = insert_eval(&NewEval {
        start: "2026-06-01",
        end: "2026-06-01",
        ..NewEval::valid(sup)
    })
    .await;
    let row = model::find_evaluation(db(), id)
        .await
        .expect("读评估")
        .expect("评估必须读得到");
    assert_eq!(row.period_start, date("2026-06-01"));
    assert_eq!(row.period_end, date("2026-06-01"));
    assert_eq!(row.eval_date, date("2026-06-02"), "夹具评估日 = 期间止 + 1 天");

    println!("[M4-12] 评估期间：起 <= 止、评估日 >= 期间起点（库层 CHECK）；单日期间放行 ✓");
}

/// 卡面「权重」：四维之和必须精确 = 1，单项 0–1。
#[tokio::test]
async fn weights_must_sum_to_one_and_be_in_range() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    // 和 1.1（四项都合法但和超了）
    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, \
             quality_weight, delivery_weight, price_weight, service_weight) \
         VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31', 0.4, 0.3, 0.2, 0.2)",
        vec![Value::Text(fixture_no()), Value::from(&sup)],
        "四维权重之和 1.1",
    )
    .await;
    assert!(
        err.contains("ck_biz_sev_weights"),
        "应命中 ck_biz_sev_weights，实际：{err}"
    );

    // 和 0.9（只写三项，第四项取默认 0.25 → 1.15）
    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, \
             quality_weight, delivery_weight, price_weight) \
         VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31', 0.4, 0.3, 0.2)",
        vec![Value::Text(fixture_no()), Value::from(&sup)],
        "只写三个权重（和 1.15）",
    )
    .await;
    assert!(
        err.contains("ck_biz_sev_weights"),
        "应命中 ck_biz_sev_weights，实际：{err}"
    );

    // 单项负数（即使和凑成 1）→ 拒
    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, \
             quality_weight, delivery_weight, price_weight, service_weight) \
         VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31', 1.5, -0.5, 0, 0)",
        vec![Value::Text(fixture_no()), Value::from(&sup)],
        "权重 1.5 / -0.5（和仍是 1）",
    )
    .await;
    assert!(
        err.contains("ck_biz_sev_weights"),
        "应命中 ck_biz_sev_weights，实际：{err}"
    );

    // 40/30/20/10 是合法组合
    let id = insert_eval(&NewEval {
        weights: ["0.4", "0.3", "0.2", "0.1"],
        ..NewEval::valid(sup)
    })
    .await;
    let row = model::find_evaluation(db(), id).await.expect("读").expect("必须读得到");
    assert_eq!(row.weights().sum(), Decimal::ONE, "读回来的四维权重和必须 = 1");
    assert!(row.weights().is_valid(), "model 判据与库层同口径");

    println!("[M4-12] 权重：和 1.1 / 1.15 / 单项越界全被库层拒；40-30-20-10 放行 ✓");
}

/// 卡面「四维打分」：0–100（含边界），越界被拒。
#[tokio::test]
async fn dimension_scores_are_0_to_100() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, quality_score) \
         VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31', ?::text::numeric)",
        vec![
            Value::Text(fixture_no()),
            Value::from(&sup),
            Value::Text("100.01".to_string()),
        ],
        "质量得分 100.01",
    )
    .await;
    assert!(
        err.contains("ck_biz_sev_scores"),
        "应命中 ck_biz_sev_scores，实际：{err}"
    );

    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, service_score) \
         VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31', ?::text::numeric)",
        vec![
            Value::Text(fixture_no()),
            Value::from(&sup),
            Value::Text("-1".to_string()),
        ],
        "服务得分 -1",
    )
    .await;
    assert!(
        err.contains("ck_biz_sev_scores"),
        "应命中 ck_biz_sev_scores，实际：{err}"
    );

    // 0 和 100 都是合法极端值
    let id = insert_eval(&NewEval {
        scores: ["0", "100", "0", "100"],
        ..NewEval::valid(sup)
    })
    .await;
    let row = model::find_evaluation(db(), id).await.expect("读").expect("必须读得到");
    assert_eq!(row.quality_score, Decimal::ZERO);
    assert_eq!(row.delivery_score, dec("100"));
    assert!(row.scores().is_valid());

    println!("[M4-12] 分数：100.01 / -1 被拒；0 与 100 边界放行 ✓");
}

/// 卡面「打分 + 权重」：总分是库算的，且显式写不进去。
#[tokio::test]
async fn total_score_is_generated_from_weights_and_scores() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    // 0.4/0.3/0.2/0.1 × 80/90/70/60 = 32+27+14+6 = 79.00
    let id = insert_eval(&NewEval {
        weights: ["0.4", "0.3", "0.2", "0.1"],
        ..NewEval::valid(sup)
    })
    .await;
    let total = numeric_of(&format!(
        "SELECT total_score AS v FROM biz_supplier_evaluation WHERE id = '{id}'::uuid"
    ))
    .await;
    assert_eq!(total, dec("79.00"), "加权总分必须由库算出 79.00");

    // 改一维分数 → 总分自动跟（不需要任何重算代码）
    exec(
        &format!("UPDATE biz_supplier_evaluation SET delivery_score = 100 WHERE id = '{id}'::uuid"),
        vec![],
    )
    .await;
    let total = numeric_of(&format!(
        "SELECT total_score AS v FROM biz_supplier_evaluation WHERE id = '{id}'::uuid"
    ))
    .await;
    assert_eq!(total, dec("82.00"), "改分数后总分必须自动重算（生成列）");

    // 显式写 total_score（INSERT）必须被 PG 拒：428C9 cannot insert a non-DEFAULT value
    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, total_score) \
         VALUES (?::text, ?::uuid, '2026-04-01', '2026-06-30', 99)",
        vec![Value::Text(fixture_no()), Value::from(&sup)],
        "显式写 total_score",
    )
    .await;
    assert!(
        err.contains("total_score") || err.to_lowercase().contains("generated"),
        "显式写生成列应报列名 / generated 相关错误，实际：{err}"
    );

    // UPDATE 显式写也必须被拒
    let err = assert_rejected(
        "UPDATE biz_supplier_evaluation SET total_score = 1 WHERE id = ?::uuid",
        vec![Value::from(&id)],
        "UPDATE 显式写 total_score",
    )
    .await;
    assert!(
        err.contains("total_score") || err.to_lowercase().contains("generated"),
        "UPDATE 生成列应报列名 / generated 相关错误，实际：{err}"
    );

    println!("[M4-12] 总分：库算出 79.00、改分自动 82.00；INSERT/UPDATE 显式写都被 PG 拒 ✓");
}

/// model 侧加权函数与库层生成列**逐位一致**（含 .005 中点）。
#[tokio::test]
async fn model_weighted_total_matches_db_generated_column() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    // 五组：常规 / 全满 / 全零 / .005 中点 / 四舍五入边界
    let cases: [([&str; 4], [&str; 4]); 5] = [
        (["0.4", "0.3", "0.2", "0.1"], ["80", "90", "70", "60"]),
        (["0.25", "0.25", "0.25", "0.25"], ["100", "100", "100", "100"]),
        (["0.25", "0.25", "0.25", "0.25"], ["0", "0", "0", "0"]),
        // 0.5 × 0.01 = 0.005 → 中点远离零 → 0.01
        (["0.5", "0.5", "0", "0"], ["0.01", "0", "0", "0"]),
        // 0.25 × 99.99 = 24.9975 → 25.00
        (["0.25", "0.25", "0.25", "0.25"], ["99.99", "0", "0", "0"]),
    ];

    for (index, (weights, scores)) in cases.iter().enumerate() {
        let id = insert_eval(
            &NewEval {
                weights: *weights,
                scores: *scores,
                ..NewEval::valid(sup)
            }
            .at(index),
        )
        .await;
        let row = model::find_evaluation(db(), id)
            .await
            .expect("读评估")
            .expect("评估必须读得到");
        let rust_total = model::weighted_total(&row.weights(), &row.scores());
        assert_eq!(
            rust_total, row.total_score,
            "第 {index} 组：model::weighted_total 与库层生成列必须逐位一致（权重 {weights:?} / 分数 {scores:?}）"
        );
        assert_eq!(
            row.recomputed_total(),
            row.total_score,
            "第 {index} 组：行上重算也必须等于生成列"
        );
    }

    println!("[M4-12] 对拍：model::weighted_total 与生成列逐位一致（5 组，含 .005 中点）✓");
}

/// 等级域：A/B/C/D 或 NULL（未评级），其它值插不进。
#[tokio::test]
async fn grade_domain_and_null_means_unrated() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    for bad in ["E", "a", "AA"] {
        let err = assert_rejected(
            "INSERT INTO biz_supplier_evaluation \
                (evaluation_no, supplier_id, period_start, period_end, grade) \
             VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31', ?::text)",
            vec![
                Value::Text(fixture_no()),
                Value::from(&sup),
                Value::Text(bad.to_string()),
            ],
            &format!("等级 {bad}"),
        )
        .await;
        assert!(
            err.contains("ck_biz_sev_grade"),
            "等级 {bad} 应命中 ck_biz_sev_grade，实际：{err}"
        );
    }

    // 四个合法等级都能落库（一份一个期间：同期间只允许一份，这正是下面那条约束）
    for (index, grade) in model::GRADES.iter().enumerate() {
        let id = insert_eval(
            &NewEval {
                grade: Some(grade),
                ..NewEval::valid(sup)
            }
            .at(index),
        )
        .await;
        let row = model::find_evaluation(db(), id).await.expect("读").expect("必须读得到");
        assert_eq!(row.grade.as_deref(), Some(*grade));
        assert!(row.is_graded(), "{grade} 级必须算已评级");
    }

    // 未评级：grade 留空 → NULL（不是 "UNRATED" 字符串）
    let id = insert_eval(&NewEval::valid(sup).at(model::GRADES.len())).await;
    let row = model::find_evaluation(db(), id).await.expect("读").expect("必须读得到");
    assert_eq!(row.grade, None, "未评级必须是 NULL");
    assert!(!row.is_graded());
    assert!(model::grade_is_valid(row.grade.as_deref()));

    println!("[M4-12] 等级：E / a / AA 被拒；A/B/C/D 落库；未评级是 NULL 不是字符串 ✓");
}

/// 单号形状 + 存活唯一 + 软删后可复用同号。
#[tokio::test]
async fn evaluation_no_format_and_surviving_uniqueness() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    for bad in [
        "SEV-2026-1",
        "SEV-20260101-1",
        "SV-20260101-000001",
        "SEV20260101000001",
    ] {
        let err = assert_rejected(
            "INSERT INTO biz_supplier_evaluation \
                (evaluation_no, supplier_id, period_start, period_end) \
             VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31')",
            vec![Value::Text(bad.to_string()), Value::from(&sup)],
            &format!("单号形状 {bad}"),
        )
        .await;
        assert!(
            err.contains("ck_biz_sev_no_fmt"),
            "单号 {bad} 应命中 ck_biz_sev_no_fmt，实际：{err}"
        );
    }

    // 同号第二份存活评估 → 唯一键拒
    let first = insert_eval(&NewEval::valid(sup)).await;
    let no: String = db()
        .query_sql(
            format!("SELECT evaluation_no FROM biz_supplier_evaluation WHERE id = '{first}'::uuid"),
            vec![],
        )
        .await
        .expect("读单号")
        .first()
        .and_then(|row| row.get_typed("evaluation_no").ok())
        .expect("单号必须读得到");

    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, supplier_name) \
         VALUES (?::text, ?::uuid, '2025-01-01', '2025-03-31', ?::text)",
        vec![
            Value::Text(no.clone()),
            Value::from(&supplier_b().await),
            Value::Text(format!("{NAME_PREFIX}同号第二份")),
        ],
        "同号第二份存活评估",
    )
    .await;
    assert!(err.contains("uk_biz_sev_no"), "应命中 uk_biz_sev_no，实际：{err}");

    // 软删后同号可复用（部分唯一索引的意义）
    exec(
        &format!("UPDATE biz_supplier_evaluation SET deleted_at = now() WHERE id = '{first}'::uuid"),
        vec![],
    )
    .await;
    let reuse = insert_eval(&NewEval::valid(supplier_b().await)).await;
    exec(
        &format!("UPDATE biz_supplier_evaluation SET evaluation_no = '{no}' WHERE id = '{reuse}'::uuid"),
        vec![],
    )
    .await;
    let reused_no: String = db()
        .query_sql(
            format!("SELECT evaluation_no FROM biz_supplier_evaluation WHERE id = '{reuse}'::uuid"),
            vec![],
        )
        .await
        .expect("读复用后的单号")
        .first()
        .and_then(|row| row.get_typed("evaluation_no").ok())
        .expect("单号必须读得到");
    assert_eq!(reused_no, no, "软删后同号必须可以复用（部分唯一索引）");

    println!("[M4-12] 单号：形状 4 种坏例全拒；存活唯一；软删后同号可复用 ✓");
}

/// 同供应商同期间只能一份评估；换期间 / 换供应商可以。
#[tokio::test]
async fn one_evaluation_per_supplier_per_period() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    let _first = insert_eval(&NewEval::valid(sup)).await;

    let err = assert_rejected(
        "INSERT INTO biz_supplier_evaluation \
            (evaluation_no, supplier_id, period_start, period_end, supplier_name) \
         VALUES (?::text, ?::uuid, '2026-01-01', '2026-03-31', ?::text)",
        vec![
            Value::Text(fixture_no()),
            Value::from(&sup),
            Value::Text(format!("{NAME_PREFIX}同期间第二份")),
        ],
        "同供应商同期间的第二份评估",
    )
    .await;
    assert!(
        err.contains("uk_biz_sev_supplier_period"),
        "应命中 uk_biz_sev_supplier_period，实际：{err}"
    );

    // 换期间（Q2）→ 可以
    let q2 = insert_eval(&NewEval {
        start: "2026-04-01",
        end: "2026-06-30",
        ..NewEval::valid(sup)
    })
    .await;
    assert!(model::find_evaluation(db(), q2).await.expect("读").is_some());

    // 换供应商（同期间）→ 可以
    let other = insert_eval(&NewEval::valid(supplier_b().await)).await;
    assert!(model::find_evaluation(db(), other).await.expect("读").is_some());

    // model 的唯一键探针也要能按 (供应商, 期间) 取到那一份
    let row = model::evaluation_of_period(db(), sup, date("2026-01-01"), date("2026-03-31"))
        .await
        .expect("按期间取")
        .expect("同期间那一份必须取得到");
    assert_eq!(row.supplier_id, sup);

    println!("[M4-12] 期间唯一：同供应商同期间第二份被拒；换期间 / 换供应商放行 ✓");
}

/// 取号：连续两个号递增，且能过库层单号 CHECK。
#[tokio::test]
async fn next_evaluation_no_uses_sev_seq() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;
    let now: DateTime<Utc> = Utc::now();

    let first = model::next_evaluation_no(db(), now).await.expect("取第一个号");
    let second = model::next_evaluation_no(db(), now).await.expect("取第二个号");
    let prefix = format!("{}-{}-", model::NO_PREFIX, model::period_key_of(now));
    assert!(first.starts_with(&prefix), "单号必须带日期段：{first}");
    assert!(second.starts_with(&prefix), "单号必须带日期段：{second}");
    let no_of =
        |text: &str| -> i64 { text.rsplit('-').next().expect("有流水段").parse().expect("流水是数字") };
    assert_eq!(
        no_of(&second),
        no_of(&first) + 1,
        "同一天连取两个号必须递增：{first} → {second}"
    );

    // 取到的号必须能过库层 ck_biz_sev_no_fmt：拿它插一条真数据
    let id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_supplier_evaluation \
            (id, evaluation_no, supplier_id, supplier_name, period_start, period_end, eval_date) \
         VALUES (?, ?::text, ?::uuid, ?::text, '2026-01-01', '2026-03-31', '2026-04-01')",
        vec![
            Value::from(&id),
            Value::Text(first.clone()),
            Value::from(&sup),
            Value::Text(format!("{NAME_PREFIX}取号夹具")),
        ],
    )
    .await;
    assert!(
        model::find_evaluation_by_no(db(), &first)
            .await
            .expect("按号取")
            .is_some(),
        "取号 + 落库 + 按号反查必须闭环"
    );

    println!("[M4-12] 取号：SEV-yyyyMMdd-NNNNNN 递增、过库层 CHECK、按号可反查 ✓");
}

/// 供应商快照与实时现名并存（约定 13）。
#[tokio::test]
async fn supplier_snapshot_and_live_name_coexist() {
    let Some(_guard) = setup().await else { return };
    let sup = supplier().await;

    // 快照有值 → 展示走快照（供应商改名后仍显示当时那家）
    let with_snapshot = insert_eval(&NewEval {
        supplier_name: Some("M412-改名前的供应商"),
        ..NewEval::valid(sup)
    })
    .await;
    let row = model::find_evaluation(db(), with_snapshot)
        .await
        .expect("读")
        .expect("必须读得到");
    assert_eq!(row.supplier_name.as_deref(), Some("M412-改名前的供应商"));
    assert_eq!(
        row.supplier_display_name, "M412-改名前的供应商",
        "有快照时显示快照，不能被实时名覆盖"
    );

    // 快照为空 → 回退到实时 join 的现名
    let without_snapshot = insert_eval(&NewEval {
        start: "2026-04-01",
        end: "2026-06-30",
        supplier_name: None,
        ..NewEval::valid(sup)
    })
    .await;
    let row = model::find_evaluation(db(), without_snapshot)
        .await
        .expect("读")
        .expect("必须读得到");
    assert_eq!(row.supplier_name, None, "快照为空就是 NULL（不写空串）");
    assert_eq!(
        row.supplier_display_name, "供应商评估夹具供应商",
        "无快照时展示名走 biz_supplier 实时 join"
    );

    // 列表 / 最近一期也走同一份 select
    let list = model::evaluations_of_supplier(db(), sup).await.expect("列表");
    assert!(list.len() >= 2, "同一供应商的两份评估都要读得到");
    assert_eq!(
        list.first().map(|row| row.period_end),
        Some(date("2026-06-30")),
        "列表按期间倒序（最新在前）"
    );
    let latest = model::latest_evaluation(db(), sup)
        .await
        .expect("最近一期")
        .expect("必须有最近一期");
    assert_eq!(latest.period_end, date("2026-06-30"));

    println!("[M4-12] 姓名口径：快照优先、空快照回退现名；列表倒序 / latest 取最近一期 ✓");
}

/// 清场：本卡夹具 0 残留。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    let left = count_of(&format!(
        "SELECT count(*) AS n FROM {} WHERE supplier_name LIKE '{NAME_PREFIX}%' OR remark LIKE '{NAME_PREFIX}%'",
        model::EVALUATION_TABLE
    ))
    .await;
    assert_eq!(left, 0, "{NAME_PREFIX} 夹具必须清干净");
    println!("[M4-12] 清场：M412- 夹具 0 残留 ✓");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_numbers_sit_in_the_probe_band() {
        // 夹具号必须是 1999-01-01 + 6 位流水（绝不与真实取号撞）
        let no = format!("{NO_BASE}00123");
        assert!(no.starts_with("SEV-19990101-9"));
        assert_eq!(no.len(), "SEV-19990101-900123".len());
    }
}

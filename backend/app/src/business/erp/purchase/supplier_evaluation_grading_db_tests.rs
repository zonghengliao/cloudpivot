// ============================================================================
// supplier_evaluation_grading_db_tests.rs —— 供应商评估打分 + 评级的真库用例（M4-13）
//
// 卡：t_252f2eaf（M4-13【M4·采购】供应商评估：打分 + 评级（A/B/C/D））
//
// # 覆盖什么（每条都对应卡面 / 评级阈值的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `save_creates_graded_evaluation` | 「打分」：四维分数 + 权重落库 → 总分由库层生成列算 → 等级按阈值写上；单号形如 SEV-yyyyMMdd-000001；供应商名 / 评估人存快照 |
// | `save_twice_overwrites_same_document` | 「同供应商同期间只有一份评估」：重打分**覆盖**同一张单（单号与 id 都不变），等级跟着新总分走 |
// | `grade_boundaries_in_database` | 「按总分分级」：89.99/90/80/79.99/70/69.99/0/100 八个点上库算总分与等级**逐点对拍**（含下界语义） |
// | `rust_total_matches_generated_column_on_grid` | 三套权重 × 三组分数的 9 个组合：库层生成列总分 == `weighted_total` 试算（公式只有一份） |
// | `invalid_inputs_are_rejected_before_write` | 权重和 ≠ 1 / 分数越界 / 期间颠倒 / 评估日早于期间起 / 供应商不存在 → `InvalidValue`，且**一行都没写** |
// | `regrade_repairs_wrong_and_null_grades` | 「阈值改了要能回算」：错等级与被清空的等级都被按总分纠正；再跑一次 0 改动（幂等） |
// | `regrade_scope_leaves_other_periods_alone` | 限定期间的重评级只碰落在该区间内的评估单 |
// | `delivery_stat_counts_only_posted_purchase_stock_ins` | 交期参考分的数据口径：只算 `POSTED` + `PURCHASE` + 挂采购单 + 有交期；DRAFT / 他供应商 / 无源 / 期间外一概不计 |
// | `delivery_stat_empty_without_receipts` | 没有到货数据 → 统计全 0 且建议分为 `None`（不是 0 分） |
// | `zz_teardown_purges_fixtures` | 清场：`M413-` 夹具 0 残留（评估单 / 入库单 / 明细 / 采购单） |
//
// # 为什么夹具一律 `M413-` 前缀
//
// 真库用例共用同一个库（模板克隆），清场只能按**自己的前缀**删；按「生产号段」或
// 「全表清空」删会误伤别人的夹具（AGENTS 的测试纪律）。
// 主数据（供应商 / 仓库 / 物料 / 单位）一律走 `crate::test_seed::ensure_*`，
// 测试代码**不许手写主数据 INSERT**。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m413_sev_drill" cargo test --lib \
//   business::erp::purchase::supplier_evaluation_grading -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::{DateTime, Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::supplier_evaluation_grading as grading;
use crate::business::erp::purchase::supplier_evaluation_model as model;
use crate::business::erp::purchase::supplier_evaluation_model::{DimensionScores, Weights};
use crate::test_seed;

/// 夹具名前缀（清场按它反查）。
const TAG: &str = "M413-";
/// 夹具供应商编码（主）。
const SUPPLIER_CODE: &str = "M413-SUP";
/// 夹具仓库编码。
const WAREHOUSE_CODE: &str = "M413-WH";
/// 夹具物料编码。
const MATERIAL_CODE: &str = "M413-MAT";

/// 夹具期间池：同一供应商要落多份评估时必须一份一个期间
/// （`uk_biz_sev_supplier_period` 就是被测的「一份期间一个结论」）。
const FIXTURE_PERIODS: [(&str, &str); 12] = [
    ("2028-01-01", "2028-03-31"),
    ("2028-04-01", "2028-06-30"),
    ("2028-07-01", "2028-09-30"),
    ("2028-10-01", "2028-12-31"),
    ("2029-01-01", "2029-03-31"),
    ("2029-04-01", "2029-06-30"),
    ("2029-07-01", "2029-09-30"),
    ("2029-10-01", "2029-12-31"),
    ("2030-01-01", "2030-03-31"),
    ("2030-04-01", "2030-06-30"),
    ("2030-07-01", "2030-09-30"),
    ("2030-10-01", "2030-12-31"),
];

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
}

fn period(index: usize) -> (NaiveDate, NaiveDate) {
    let (start, end) = FIXTURE_PERIODS.get(index).copied().unwrap_or_else(|| {
        panic!(
            "FIXTURE_PERIODS 只有 {} 个期间，装不下 index={index}（一个用例里同一供应商多份评估必须一份一个期间）",
            FIXTURE_PERIODS.len()
        )
    });
    (date(start), date(end))
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场：只删本卡的夹具（明细先删，再删单头）。
async fn cleanup() {
    exec(
        "DELETE FROM biz_stock_in_item WHERE stock_in_id IN \
             (SELECT id FROM biz_stock_in WHERE in_no LIKE 'M413-%')",
        vec![],
    )
    .await;
    exec("DELETE FROM biz_stock_in WHERE in_no LIKE 'M413-%'", vec![]).await;
    exec(
        "DELETE FROM biz_purchase_order_item WHERE order_id IN \
             (SELECT id FROM biz_purchase_order WHERE order_no LIKE 'M413-%')",
        vec![],
    )
    .await;
    exec("DELETE FROM biz_purchase_order WHERE order_no LIKE 'M413-%'", vec![]).await;
    exec(
        &format!(
            "DELETE FROM {} WHERE supplier_name LIKE '{TAG}%' OR remark LIKE '{TAG}%'",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;
}

async fn supplier() -> Uuid {
    test_seed::ensure_supplier(SUPPLIER_CODE, "M413 供应商评估夹具供应商").await
}

async fn warehouse() -> Uuid {
    test_seed::ensure_warehouse(WAREHOUSE_CODE, "M413 收货仓").await
}

async fn material() -> Uuid {
    test_seed::ensure_material_base_unit(MATERIAL_CODE, "M413 评估物料").await
}

/// 一份打分入参（权重四等分，四维同分 —— 总分就等于那个分数，边界值好写）。
fn input(supplier_id: Uuid, index: usize, score: &str) -> grading::EvaluationInput {
    let (start, end) = period(index);
    let value = dec(score);
    grading::EvaluationInput {
        supplier_id,
        period_start: start,
        period_end: end,
        eval_date: end,
        weights: Weights::equal(),
        scores: DimensionScores {
            quality: value,
            delivery: value,
            price: value,
            service: value,
        },
        remark: Some(format!("{TAG}夹具")),
        evaluator_id: None,
        evaluator_name: Some("M413 评估人".to_string()),
        operator_id: None,
    }
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

/// 统计一条单值。
async fn count_of(sql: &str, params: Vec<Value>) -> i64 {
    db().scalar_with_params(sql, params)
        .await
        .unwrap_or_else(|error| panic!("查计数失败：{error}\n{sql}"))
}

/// 造一张采购单（可带交期 `expect_date`）。
async fn seed_po(order_no: &str, supplier_id: Uuid, expect_date: Option<NaiveDate>) -> Uuid {
    let id = Uuid::now_v7();
    let expect = match expect_date {
        Some(value) => Value::Text(model::date_text(value)),
        None => Value::Null,
    };
    exec(
        "INSERT INTO biz_purchase_order \
             (id, order_no, supplier_id, order_date, expect_date, warehouse_id, status, \
              total_qty, total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, ?::text::date, ?::text::date, ?::uuid, 'APPROVED', 0, 0, 0, 0)",
        vec![
            Value::from(&id),
            Value::Text(order_no.to_string()),
            Value::from(&supplier_id),
            Value::Text("2028-01-05".to_string()),
            expect,
            Value::from(&warehouse().await),
        ],
    )
    .await;
    id
}

/// 造一张入库单 + 一行明细（`qty` 由调用方给）。
///
/// 三条互相咬合的库层约束在这里一次满足：
/// `ck_biz_si_sourceless_pair`（`is_sourceless = (source_order_id IS NULL)`）、
/// `ck_biz_si_supplier_scope`（`PURCHASE/SOURCELESS` 必须有供应商，其它类型必须没有）、
/// `ck_biz_si_tax`（`total_with_tax = total_amount + total_tax`，夹具全 0）。
async fn seed_stock_in(
    in_no: &str,
    supplier_id: Option<Uuid>,
    po: Option<Uuid>,
    in_date: NaiveDate,
    qty: &str,
    status: &str,
    in_type: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    let supplier_value = match supplier_id {
        Some(value) => Value::from(&value),
        None => Value::Null,
    };
    let po_value = match po {
        Some(value) => Value::from(&value),
        None => Value::Null,
    };
    exec(
        "INSERT INTO biz_stock_in \
             (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, \
              is_sourceless, total_qty, total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?, ?, ?, ?::uuid, ?::text::date, ?, ?, ?::text::numeric, 0, 0, 0)",
        vec![
            Value::from(&id),
            Value::Text(in_no.to_string()),
            Value::Text(in_type.to_string()),
            po_value,
            supplier_value,
            Value::from(&warehouse().await),
            Value::Text(model::date_text(in_date)),
            Value::Text(status.to_string()),
            Value::Bool(po.is_none()),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    exec(
        "INSERT INTO biz_stock_in_item \
             (id, stock_in_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
              amount, tax_amount, with_tax_amount) \
         VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, ?::text::numeric, 0, 0, 0, 0, 0, 0)",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&id),
            Value::from(&material().await),
            Value::from(&test_seed::ensure_base_unit().await),
            Value::Text(qty.to_string()),
        ],
    )
    .await;
    id
}

// ==================== 打分落库 ====================

#[tokio::test]
async fn save_creates_graded_evaluation() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    let (start, end) = period(0);
    let value = dec("80");
    let mut draft = input(supplier_id, 0, "80");
    // 40/30/20/10 × 80/90/70/60 = 79.00 → C
    draft.weights = Weights {
        quality: dec("0.4"),
        delivery: dec("0.3"),
        price: dec("0.2"),
        service: dec("0.1"),
    };
    draft.scores = DimensionScores {
        quality: dec("80"),
        delivery: dec("90"),
        price: dec("70"),
        service: dec("60"),
    };
    let outcome = grading::save_evaluation(db(), &draft, now()).await.expect("打分落库");
    assert!(outcome.created, "第一次打分必须是新建");
    let row = &outcome.row;
    assert_eq!(row.total_score, dec("79.00"), "总分由库层生成列算");
    assert_eq!(row.grade.as_deref(), Some(model::GRADE_C), "79.00 落在 [70,80) = C");
    assert_eq!(row.period_start, start);
    assert_eq!(row.period_end, end);
    assert_eq!(row.scores().quality, value);
    assert_eq!(row.weights(), draft.weights);
    assert!(
        row.evaluation_no.starts_with("SEV-") && row.evaluation_no.len() == 19,
        "单号形如 SEV-yyyyMMdd-000001，实际 {}",
        row.evaluation_no
    );
    assert_eq!(
        row.supplier_name.as_deref(),
        Some("M413 供应商评估夹具供应商"),
        "供应商名快照（约定 13）"
    );
    assert_eq!(row.evaluator_name.as_deref(), Some("M413 评估人"));
    // 落库的等级必须能由落库的总分复算出来（等级与阈值同源）
    assert_eq!(
        grading::grade_for_total(row.total_score).expect("总分合法"),
        row.grade.as_deref().expect("已评级")
    );
}

#[tokio::test]
async fn save_twice_overwrites_same_document() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    let first = grading::save_evaluation(db(), &input(supplier_id, 0, "79.99"), now())
        .await
        .expect("首轮打分");
    assert_eq!(first.row.grade.as_deref(), Some(model::GRADE_C));

    // 同一个供应商同一个期间重打分：覆盖，不新建单号
    let second = grading::save_evaluation(db(), &input(supplier_id, 0, "90"), now())
        .await
        .expect("重打分");
    assert!(!second.created, "重打分必须是覆盖（同期间只有一份评估）");
    assert_eq!(second.row.id, first.row.id, "覆盖同一行");
    assert_eq!(
        second.row.evaluation_no, first.row.evaluation_no,
        "重打分不改单号（历史期间结论靠单号回溯）"
    );
    assert_eq!(second.row.total_score, dec("90.00"));
    assert_eq!(second.row.grade.as_deref(), Some(model::GRADE_A), "90.00 含下界 = A");
    assert_eq!(
        count_of(
            &format!(
                "SELECT count(*) FROM {} WHERE supplier_id = ?::uuid AND deleted_at IS NULL",
                model::EVALUATION_TABLE
            ),
            vec![Value::from(&supplier_id)],
        )
        .await,
        1,
        "同供应商同期间只有一份存活评估"
    );
}

#[tokio::test]
async fn grade_boundaries_in_database() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    // 八个边界点：分界含下界（90 是 A、89.99 是 B；70 是 C、69.99 是 D）
    let cases: [(&str, &str); 8] = [
        ("89.99", model::GRADE_B),
        ("90", model::GRADE_A),
        ("80", model::GRADE_B),
        ("79.99", model::GRADE_C),
        ("70", model::GRADE_C),
        ("69.99", model::GRADE_D),
        ("0", model::GRADE_D),
        ("100", model::GRADE_A),
    ];
    for (index, (score, expected)) in cases.iter().enumerate() {
        let outcome = grading::save_evaluation(db(), &input(supplier_id, index, score), now())
            .await
            .unwrap_or_else(|error| panic!("总分 {score} 打分失败：{error}"));
        assert_eq!(
            outcome.row.total_score,
            dec(score),
            "四等分 + 四维同分 {score} → 总分就是 {score}"
        );
        assert_eq!(
            outcome.row.grade.as_deref(),
            Some(*expected),
            "总分 {} 的等级应为 {expected}",
            outcome.row.total_score
        );
    }
}

#[tokio::test]
async fn rust_total_matches_generated_column_on_grid() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    let weight_sets = [
        Weights::equal(),
        Weights {
            quality: dec("0.4"),
            delivery: dec("0.3"),
            price: dec("0.2"),
            service: dec("0.1"),
        },
        Weights {
            quality: dec("0.5"),
            delivery: dec("0.2"),
            price: dec("0.2"),
            service: dec("0.1"),
        },
    ];
    let score_sets = [
        DimensionScores {
            quality: dec("100"),
            delivery: dec("0"),
            price: dec("55.5"),
            service: dec("66.65"),
        },
        DimensionScores {
            quality: dec("30.05"),
            delivery: dec("70.15"),
            price: dec("90.45"),
            service: dec("10.35"),
        },
        DimensionScores {
            quality: dec("99.99"),
            delivery: dec("0.01"),
            price: dec("50"),
            service: dec("50"),
        },
    ];
    let mut index = 0usize;
    for weights in weight_sets {
        for scores in score_sets {
            let mut draft = input(supplier_id, index, "0");
            draft.weights = weights;
            draft.scores = scores;
            let outcome = grading::save_evaluation(db(), &draft, now())
                .await
                .unwrap_or_else(|error| panic!("第 {index} 组打分失败：{error}"));
            let expected_total = model::weighted_total(&weights, &scores);
            assert_eq!(
                outcome.row.total_score, expected_total,
                "第 {index} 组：库层生成列总分与 Rust 试算不一致"
            );
            assert_eq!(
                grading::grade_for_total(expected_total).expect("总分合法"),
                outcome.row.grade.as_deref().expect("已评级"),
                "第 {index} 组的等级与阈值不同源"
            );
            index += 1;
        }
    }
}

#[tokio::test]
async fn invalid_inputs_are_rejected_before_write() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    let (start, end) = period(0);

    // ① 权重之和 ≠ 1
    let mut bad = input(supplier_id, 0, "80");
    bad.weights.service = dec("0.3");
    let error = grading::save_evaluation(db(), &bad, now())
        .await
        .expect_err("权重和 1.1 必须被拒");
    assert!(matches!(error, DbError::InvalidValue(_)), "应是 400 语义：{error}");

    // ② 分数越界
    let mut bad = input(supplier_id, 0, "80");
    bad.scores.price = dec("100.01");
    let error = grading::save_evaluation(db(), &bad, now())
        .await
        .expect_err("分数 100.01 必须被拒");
    assert!(matches!(error, DbError::InvalidValue(_)));

    // ③ 期间起晚于止
    let mut bad = input(supplier_id, 0, "80");
    bad.period_start = end;
    bad.period_end = start;
    bad.eval_date = end;
    let error = grading::save_evaluation(db(), &bad, now())
        .await
        .expect_err("期间颠倒必须被拒");
    assert!(matches!(error, DbError::InvalidValue(_)));

    // ④ 评估日早于期间起点
    let mut bad = input(supplier_id, 0, "80");
    bad.eval_date = start - Duration::days(1);
    let error = grading::save_evaluation(db(), &bad, now())
        .await
        .expect_err("评估日早于期间起点必须被拒");
    assert!(matches!(error, DbError::InvalidValue(_)));

    // ⑤ 供应商不存在（弱引用表没有外键，必须由入口判）
    let bad = input(Uuid::now_v7(), 0, "80");
    let error = grading::save_evaluation(db(), &bad, now())
        .await
        .expect_err("供应商不存在必须被拒");
    assert!(matches!(error, DbError::InvalidValue(_)));

    // 以上五条一条都不许落库
    assert_eq!(
        count_of(
            &format!(
                "SELECT count(*) FROM {} WHERE supplier_id = ?::uuid AND period_start = ?::text::date",
                model::EVALUATION_TABLE
            ),
            vec![Value::from(&supplier_id), Value::Text(model::date_text(start))],
        )
        .await,
        0,
        "校验失败不许留下半张评估单"
    );
}

// ==================== 重评级 ====================

/// 直接插一份「等级写错 / 没写」的评估（分数是真的，等级是拿来被测的）。
async fn insert_with_wrong_grade(supplier_id: Uuid, index: usize, score: &str, grade: Option<&str>) {
    let (start, end) = period(index);
    let grade_value = match grade {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    };
    exec(
        &format!(
            "INSERT INTO {} \
                 (evaluation_no, supplier_id, supplier_name, period_start, period_end, eval_date, \
                  quality_score, delivery_score, price_score, service_score, grade, remark) \
             VALUES (?, ?::uuid, ?, ?::text::date, ?::text::date, ?::text::date, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?, ?)",
            model::EVALUATION_TABLE
        ),
        vec![
            Value::Text(format!("SEV-19990101-{:06}", 413000 + index as i64)),
            Value::from(&supplier_id),
            Value::Text(format!("{TAG}错等级夹具")),
            Value::Text(model::date_text(start)),
            Value::Text(model::date_text(end)),
            Value::Text(model::date_text(end)),
            Value::Text(score.to_string()),
            Value::Text(score.to_string()),
            Value::Text(score.to_string()),
            Value::Text(score.to_string()),
            grade_value,
            Value::Text(format!("{TAG}夹具")),
        ],
    )
    .await;
}

#[tokio::test]
async fn regrade_repairs_wrong_and_null_grades() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    // 三份：60 分却写 A（该 D）、85 分写 A（该 B）、95 分等级被清空（该 A）
    insert_with_wrong_grade(supplier_id, 0, "60", Some(model::GRADE_A)).await;
    insert_with_wrong_grade(supplier_id, 1, "85", Some(model::GRADE_A)).await;
    insert_with_wrong_grade(supplier_id, 2, "95", None).await;

    let (start, _end) = period(0);
    let scope = Some((start, period(2).1));
    let changed = grading::regrade_evaluations(db(), scope).await.expect("重评级");
    assert_eq!(changed, 3, "三份都要被纠正（两份错等级 + 一份未评级）");

    let rows = model::evaluations_of_supplier(db(), supplier_id)
        .await
        .expect("读回评估");
    let mut got: Vec<(String, String)> = rows
        .iter()
        .map(|row| (row.total_score.to_string(), row.grade.clone().unwrap_or_default()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("60.00".to_string(), model::GRADE_D.to_string()),
            ("85.00".to_string(), model::GRADE_B.to_string()),
            ("95.00".to_string(), model::GRADE_A.to_string()),
        ],
        "等级必须按总分重算"
    );

    let again = grading::regrade_evaluations(db(), scope).await.expect("再跑一次");
    assert_eq!(again, 0, "重评级幂等：等级已经对了就不改");
}

#[tokio::test]
async fn regrade_scope_leaves_other_periods_alone() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    insert_with_wrong_grade(supplier_id, 0, "60", Some(model::GRADE_A)).await;
    insert_with_wrong_grade(supplier_id, 1, "95", Some(model::GRADE_D)).await;

    // 只重算第 1 个期间所在的区间
    let (start, end) = period(0);
    let changed = grading::regrade_evaluations(db(), Some((start, end)))
        .await
        .expect("按期间重评级");
    assert_eq!(changed, 1, "只有落在区间内的那一份被改");

    let first = model::evaluation_of_period(db(), supplier_id, start, end)
        .await
        .expect("查期间一的评估")
        .expect("存在");
    assert_eq!(first.grade.as_deref(), Some(model::GRADE_D), "60 分纠成 D");

    let (start2, end2) = period(1);
    let second = model::evaluation_of_period(db(), supplier_id, start2, end2)
        .await
        .expect("查期间二的评估")
        .expect("存在");
    assert_eq!(
        second.grade.as_deref(),
        Some(model::GRADE_D),
        "区间外的评估保持原样（这里故意写错的 D 不被修）"
    );
}

// ==================== 交期参考分（真实入库单） ====================

#[tokio::test]
async fn delivery_stat_counts_only_posted_purchase_stock_ins() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    let other_supplier = test_seed::ensure_supplier("M413-SUP-B", "M413 另一家供应商").await;
    let expect_date = date("2028-02-10");
    let po = seed_po("M413-PO-001", supplier_id, Some(expect_date)).await;
    let po_no_expect = seed_po("M413-PO-002", supplier_id, None).await;
    let po_other = seed_po("M413-PO-003", other_supplier, Some(expect_date)).await;

    // 准时 60（in_date <= expect_date）
    seed_stock_in(
        "M413-IN-001",
        Some(supplier_id),
        Some(po),
        date("2028-02-05"),
        "60",
        "POSTED",
        "PURCHASE",
    )
    .await;
    // 逾期 40
    seed_stock_in(
        "M413-IN-002",
        Some(supplier_id),
        Some(po),
        date("2028-02-20"),
        "40",
        "POSTED",
        "PURCHASE",
    )
    .await;
    // 草稿：没入账，不计
    seed_stock_in(
        "M413-IN-003",
        Some(supplier_id),
        Some(po),
        date("2028-02-06"),
        "999",
        "DRAFT",
        "PURCHASE",
    )
    .await;
    // 已取消：不计
    seed_stock_in(
        "M413-IN-004",
        Some(supplier_id),
        Some(po),
        date("2028-02-07"),
        "999",
        "CANCELLED",
        "PURCHASE",
    )
    .await;
    // 期间外：不计
    seed_stock_in(
        "M413-IN-005",
        Some(supplier_id),
        Some(po),
        date("2028-05-01"),
        "999",
        "POSTED",
        "PURCHASE",
    )
    .await;
    // 采购单没有交期：无法判准时，不计
    seed_stock_in(
        "M413-IN-006",
        Some(supplier_id),
        Some(po_no_expect),
        date("2028-02-08"),
        "999",
        "POSTED",
        "PURCHASE",
    )
    .await;
    // 无源入库（OTHER，无供应商）：不计
    seed_stock_in("M413-IN-007", None, None, date("2028-02-09"), "999", "POSTED", "OTHER").await;
    // 别家供应商的到货：不计
    seed_stock_in(
        "M413-IN-008",
        Some(other_supplier),
        Some(po_other),
        date("2028-02-04"),
        "999",
        "POSTED",
        "PURCHASE",
    )
    .await;

    let stat = grading::delivery_stat(db(), supplier_id, date("2028-01-01"), date("2028-03-31"))
        .await
        .expect("算交期统计");
    assert_eq!(stat.docs, 2, "只有两张已过账的采购入库");
    assert_eq!(stat.late_docs, 1, "其中一张含逾期行");
    assert_eq!(stat.total_qty, dec("100"));
    assert_eq!(stat.on_time_qty, dec("60"));
    assert_eq!(stat.late_qty, dec("40"));
    assert_eq!(stat.suggested_score(), Some(dec("60.00")), "准时率 = 60/100");
}

#[tokio::test]
async fn delivery_stat_empty_without_receipts() {
    let Some(_guard) = setup().await else { return };
    let supplier_id = supplier().await;
    let stat = grading::delivery_stat(db(), supplier_id, date("2028-01-01"), date("2028-03-31"))
        .await
        .expect("算交期统计");
    assert_eq!(stat, grading::DeliveryStat::empty());
    assert!(!stat.is_usable());
    assert_eq!(stat.suggested_score(), None, "没有到货数据时不许给 0 分");
}

// ==================== 清场 ====================

#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    let left = count_of(
        "SELECT (SELECT count(*) FROM biz_supplier_evaluation WHERE supplier_name LIKE 'M413-%' \
                                                                    OR remark LIKE 'M413-%') \
              + (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'M413-%') \
              + (SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'M413-%')",
        vec![],
    )
    .await;
    assert_eq!(left, 0, "M413- 夹具必须 0 残留");
}

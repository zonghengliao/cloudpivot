// ============================================================================
// supplier_evaluation_suite_tests.rs —— 供应商评估「单测」收口（M4-15）
//
// 卡：t_73b6adf8（M4-15【M4·采购】供应商评估：单测 · 含评级边界）
//
// # 不重复劳动：本卡为什么只写这些
//
// M4-12（表结构）/ M4-13（打分 + 评级）两卡已各带用例：model 纯逻辑 12 条、
// grading 纯逻辑 6 条、schema 真库 11 条、grading 真库 10 条。M4-13 的 README
// §四**逐条点名**本卡还欠什么：「并发打分同一期间」「跨期重打分」「超大方量性能」。
// 本卡不抄第二遍，只补这三处 + 卡面点名的「评级边界」做穷举（原来只有 8 个点）。
//
// | 用例 | 钉住的口径（对应卡面 / 上游契约的一句原话） |
// |---|---|
// | `grade_policy_holds_on_every_hundredth_point` | 「评级边界」：总分 0.00–100.00 **逐 0.01 穷举** 10001 个点，等级必须与政策（≥90 A / ≥80 B / ≥70 C / 其余 D）逐点一致；全区间只有 3 次跳变且落在 70.00 / 80.00 / 90.00 |
// | `rounding_happens_before_grading` | 政策比的是**舍入后**的总分：69.995 / 79.995 / 89.995 这三点抬进上一档（C / B / A），69.994 落回本档；若先评级再舍入，这三点会判低一档 |
// | `single_dimension_weights_reach_every_band` | 权重允许 0（四维只留一维仍合法）：单独一维 70 分就是 C、90 分就是 A —— 边界不因权重结构而漂 |
// | `case_sql_and_rust_share_one_threshold_table` | 阈值**只有一份**：`grade_case_sql` 里的每个 `>=` 都逐字来自 `GRADE_THRESHOLDS`，顺序也一致（regrade 与打分不会各用一套阈值） |
// | `weight_and_score_domains_are_validated_at_the_edges` | 权重各 0–1、和精确 = 1（0 与 1 都是合法端点，0.9999 不合法）；分数 0 / 100 合法，100.01 / -0.01 不合法 |
// | `rating_boundaries_hold_in_the_database` | 同样 12 个边界点（含三个 .995 中点）走 `save_evaluation` 落库：库层生成列总分、写入的等级、`grade_for_total(总分)` 三者逐点相同 |
// | `sql_case_agrees_with_rust_on_every_hundredth_point` | 库里执行 `grade_case_sql` 生成的 `CASE`，对 10001 个总分点与 Rust `grade_for_total` **逐点相同** —— 打分路径与 regrade 路径不许分叉 |
// | `concurrent_scoring_of_one_period_keeps_one_document` | 「并发打分同一期间」（M4-13 README 点名的缺口）：5 轮 × 4 个并发写同一供应商同一期间，全部调用成功、只留 1 行、四个返回值同 id；**不出现「加权总分漂移」这种并发误报** |
// | `cross_period_scoring_makes_independent_documents` | 「跨期重打分」：三个期间三份单据；重打其中一期只改它（id / 单号不变，另两期一字不动）；期间边界差一天 = 另一份单据；最近一期 = 期末最新那份 |
// | `rejected_inputs_never_touch_an_existing_evaluation` | 「拒绝分支不许留半行」：权重和 ≠ 1 / 权重为负 / 分数越界 / 期间颠倒 / 评估日早于期间 / 供应商不存在 / 供应商已软删 —— 七类全部 400 语义，且已有那份评估的总分 / 等级 / 备注**一个字节都不动** |
// | `bulk_regrade_is_one_pass_idempotent_and_leaves_scores_alone` | 「超大方量性能」：1200 行错等级一次 regrade 全改对（第二遍 0 改动），分数与单号不动、只动 `grade`/`updated_at`，单次全表回算远低于 60s 的粗上限 |
// | `zz_teardown_purges_fixtures` | 清场：`M415` 夹具 0 残留 |
//
// # 夹具
//
// 主数据（供应商）一律 `crate::test_seed::ensure_supplier`，本文件**零**主数据手写 INSERT
// （P2-62 门禁）。评估单本身不是主数据，批量造数走 `generate_series`（表没有 CRUD 接口）。
// 清场只按本卡前缀 `M415` 反查，绝不按生产号段删。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m415_ut_drill" cargo test --lib \
//   business::erp::purchase::supplier_evaluation_suite -- --test-threads=1
// ```
// ============================================================================

use std::time::Instant;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::supplier_evaluation_grading as grading;
use crate::business::erp::purchase::supplier_evaluation_model as model;
use crate::business::erp::purchase::supplier_evaluation_model::{
    DimensionScores, TOTAL_ROUNDING, TOTAL_SCALE, Weights,
};
use crate::test_seed;

/// 夹具名前缀（清场按它反查）。
const TAG: &str = "M415";
/// 夹具供应商编码（主力）。
const SUPPLIER_CODE: &str = "M415-SUP";
/// 夹具供应商编码（用来验「已软删的供应商不许评估」，用例结束会还原）。
const SUPPLIER_DELETED_CODE: &str = "M415-SUP-DEL";

/// 夹具期间池：同一供应商要多份评估时必须一份一个期间
/// （`uk_biz_sev_supplier_period` 就是被测的「一份期间一个结论」）。
const FIXTURE_PERIODS: [(&str, &str); 16] = [
    ("2032-01-01", "2032-03-31"),
    ("2032-04-01", "2032-06-30"),
    ("2032-07-01", "2032-09-30"),
    ("2032-10-01", "2032-12-31"),
    ("2033-01-01", "2033-03-31"),
    ("2033-04-01", "2033-06-30"),
    ("2033-07-01", "2033-09-30"),
    ("2033-10-01", "2033-12-31"),
    ("2034-01-01", "2034-03-31"),
    ("2034-04-01", "2034-06-30"),
    ("2034-07-01", "2034-09-30"),
    ("2034-10-01", "2034-12-31"),
    ("2035-01-01", "2035-03-31"),
    ("2035-04-01", "2035-06-30"),
    ("2035-07-01", "2035-09-30"),
    ("2035-10-01", "2035-12-31"),
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
            "FIXTURE_PERIODS 只有 {} 个期间，装不下 index={index}（同一供应商多份评估必须一份一个期间）",
            FIXTURE_PERIODS.len()
        )
    });
    (date(start), date(end))
}

// ============================================================================
// 纯逻辑（不连库）
// ============================================================================

/// 政策本身（**独立复述**一遍，不调用被测实现）：这是「边界」用例的对照答案，
/// 写在这里而不是从 `GRADE_THRESHOLDS` 推——否则实现错了用例也会跟着错。
fn policy_grade(total: Decimal) -> &'static str {
    if total >= dec("90") {
        model::GRADE_A
    } else if total >= dec("80") {
        model::GRADE_B
    } else if total >= dec("70") {
        model::GRADE_C
    } else {
        model::GRADE_D
    }
}

#[test]
fn grade_policy_holds_on_every_hundredth_point() {
    println!("[M4-15] grade_policy_holds_on_every_hundredth_point");
    let mut changes: Vec<String> = Vec::new();
    let mut previous: Option<&'static str> = None;
    for hundredths in 0..=10_000i64 {
        let total = Decimal::new(hundredths, 2);
        let expected = policy_grade(total);
        let actual =
            grading::grade_for_total(total).unwrap_or_else(|error| panic!("总分 {total} 应可评级，实际报错：{error}"));
        assert_eq!(actual, expected, "总分 {total}：政策 {expected} vs 实现 {actual}");
        if let Some(prev) = previous
            && prev != actual
        {
            changes.push(total.to_string());
            assert_ne!(prev, actual, "等级跳变记录出错");
        }
        previous = Some(actual);
    }
    // 全区间只允许在三个阈值点上跳变，且含下界（跳变点本身就是上一档）
    assert_eq!(
        changes,
        vec!["70.00".to_string(), "80.00".to_string(), "90.00".to_string()],
        "整个 0.00–100.00 区间只有三次等级跳变，且必须落在 70/80/90"
    );
    // 越界不是「D」，是算错了
    assert!(
        grading::grade_for_total(dec("-0.01")).is_err(),
        "负总分必须报错，不能默默给 D"
    );
    assert!(grading::grade_for_total(dec("100.01")).is_err(), "超过 100 分必须报错");
}

#[test]
fn rounding_happens_before_grading() {
    println!("[M4-15] rounding_happens_before_grading");
    // 政策比的是**舍入后**（2 位、中点远离零）的总分 —— 与库层生成列同一套舍入。
    assert_eq!(
        dec("69.995").round_dp_with_strategy(TOTAL_SCALE, TOTAL_ROUNDING),
        dec("70.00"),
        "69.995 的中点舍入必须抬到 70.00（不是截断成 69.99）"
    );
    assert_eq!(
        dec("69.994").round_dp_with_strategy(TOTAL_SCALE, TOTAL_ROUNDING),
        dec("69.99"),
        "69.994 不中点，舍入后仍是 69.99"
    );

    // 四等分权重下 (90+90+90+9.98)/4 = 69.995 → 库层与 Rust 都是 70.00 → C
    let equal = Weights::equal();
    let case_c = DimensionScores {
        quality: dec("90"),
        delivery: dec("90"),
        price: dec("90"),
        service: dec("9.98"),
    };
    let (grade, total) = grading::grade_and_total(&equal, &case_c).expect("合法入参");
    assert_eq!(total, dec("70.00"), "69.995 必须先进位成 70.00");
    assert_eq!(grade, model::GRADE_C, "舍入后 70.00 = C（含下界）；先评级会误判 D");

    // (90+90+90+49.98)/4 = 79.995 → 80.00 → B（同样是 .995 中点抬起）
    let case_b = DimensionScores {
        quality: dec("90"),
        delivery: dec("90"),
        price: dec("90"),
        service: dec("49.98"),
    };
    let (grade, total) = grading::grade_and_total(&equal, &case_b).expect("合法入参");
    assert_eq!(total, dec("80.00"));
    assert_eq!(grade, model::GRADE_B, "舍入后 80.00 = B");

    // (90+90+90+89.98)/4 = 89.995 → 90.00 → A
    let case_a = DimensionScores {
        quality: dec("90"),
        delivery: dec("90"),
        price: dec("90"),
        service: dec("89.98"),
    };
    let (grade, total) = grading::grade_and_total(&equal, &case_a).expect("合法入参");
    assert_eq!(total, dec("90.00"));
    assert_eq!(grade, model::GRADE_A, "舍入后 90.00 = A（含下界）");
}

#[test]
fn single_dimension_weights_reach_every_band() {
    println!("[M4-15] single_dimension_weights_reach_every_band");
    let only_quality = Weights {
        quality: Decimal::ONE,
        delivery: Decimal::ZERO,
        price: Decimal::ZERO,
        service: Decimal::ZERO,
    };
    assert!(only_quality.is_valid(), "四维只留一维（和 = 1）是合法权重");
    let at = |value: &str| DimensionScores {
        quality: dec(value),
        delivery: Decimal::ZERO,
        price: Decimal::ZERO,
        service: Decimal::ZERO,
    };
    for (score, expected) in [
        ("100", model::GRADE_A),
        ("90", model::GRADE_A),
        ("89.99", model::GRADE_B),
        ("80", model::GRADE_B),
        ("79.99", model::GRADE_C),
        ("70", model::GRADE_C),
        ("69.99", model::GRADE_D),
        ("0", model::GRADE_D),
    ] {
        let (grade, total) = grading::grade_and_total(&only_quality, &at(score)).expect("合法入参");
        assert_eq!(total, dec(score), "单一维度权重 1 时总分就等于那一维分数");
        assert_eq!(grade, expected, "单维 {score} 分应为 {expected}");
    }
    // 两维半分、另两维 0 也要合法（现实里最常见的 50/50）
    let half = Weights {
        quality: dec("0.5"),
        delivery: dec("0.5"),
        price: Decimal::ZERO,
        service: Decimal::ZERO,
    };
    assert!(half.is_valid());
    let mixed = DimensionScores {
        quality: dec("90"),
        delivery: dec("70"),
        price: dec("100"),
        service: dec("100"),
    };
    let (grade, total) = grading::grade_and_total(&half, &mixed).expect("合法入参");
    assert_eq!(total, dec("80.00"), "0.5×90 + 0.5×70 = 80，后两维权重 0 不参与");
    assert_eq!(grade, model::GRADE_B);
}

#[test]
fn case_sql_and_rust_share_one_threshold_table() {
    println!("[M4-15] case_sql_and_rust_share_one_threshold_table");
    let sql = grading::grade_case_sql("total_score");
    // ① 每一条阈值都逐字出现在 CASE 里，顺序与阈值表一致
    for (grade, min_total) in grading::GRADE_THRESHOLDS {
        let needle = format!("WHEN total_score >= {min_total} THEN '{grade}'");
        assert!(sql.contains(&needle), "CASE 里缺少阈值「{needle}」：{sql}");
    }
    // ② CASE 里**所有** `>=` 右侧的数字都必须来自阈值表（不许有第二份阈值）
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    let mut thresholds_in_sql: Vec<String> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if *token == ">=" {
            let next = tokens
                .get(index + 1)
                .unwrap_or_else(|| panic!("CASE 里 >= 后面缺数字：{sql}"));
            thresholds_in_sql.push((*next).to_string());
        }
    }
    let expected: Vec<String> = grading::GRADE_THRESHOLDS
        .iter()
        .map(|(_, min_total)| (*min_total).to_string())
        .collect();
    assert_eq!(
        thresholds_in_sql, expected,
        "CASE 的阈值序列必须**逐字等于** GRADE_THRESHOLDS（阈值只有一份）"
    );
    // ③ WHEN 子句 = 1 个 NULL 判空 + 4 个阈值 + 1 个 END 兜底
    assert_eq!(sql.matches("WHEN").count(), 5, "CASE 的 WHEN 个数必须是 5：{sql}");
    assert!(
        sql.starts_with("CASE WHEN total_score IS NULL THEN NULL"),
        "NULL 必须给 NULL（未评级）"
    );
}

#[test]
fn weight_and_score_domains_are_validated_at_the_edges() {
    println!("[M4-15] weight_and_score_domains_are_validated_at_the_edges");
    // 权重端点：0 与 1 都合法；和必须精确 = 1
    assert!(Weights::equal().is_valid());
    assert!(
        Weights {
            quality: Decimal::ONE,
            delivery: Decimal::ZERO,
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        }
        .is_valid(),
        "1/0/0/0 合法（端点）"
    );
    assert!(
        !Weights {
            quality: dec("0.5000"),
            delivery: dec("0.4999"),
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        }
        .is_valid(),
        "和 0.9999 ≠ 1，不合法（「差不多是 1」不算）"
    );
    assert!(
        !Weights {
            quality: dec("1.5"),
            delivery: dec("-0.5"),
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        }
        .is_valid(),
        "和确实是 1，但单项越界（负 / >1）不合法"
    );
    assert!(
        !Weights {
            quality: dec("1.0001"),
            delivery: Decimal::ZERO,
            price: Decimal::ZERO,
            service: dec("-0.0001"),
        }
        .is_valid(),
        "超过 1 的单项即使和对也不合法"
    );

    // 分数端点：0 与 100 合法，出界一点点都不行
    let at = |quality: &str, service: &str| DimensionScores {
        quality: dec(quality),
        delivery: dec("0"),
        price: dec("100"),
        service: dec(service),
    };
    assert!(at("0", "100").is_valid(), "0 与 100 都是合法极端值");
    assert!(!at("100.01", "0").is_valid(), "100.01 越界");
    assert!(!at("0", "-0.01").is_valid(), "-0.01 越界");
    // 非法入参在 `grade_and_total` 里就该被拦（400 语义，不等到写库）
    assert!(grading::grade_and_total(&Weights::equal(), &at("100.01", "0")).is_err());
    assert!(grading::grade_and_total(&Weights::equal(), &at("0", "-0.01")).is_err());
}

// ============================================================================
// 真库夹具
// ============================================================================

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

/// 清场：只删本卡的夹具。
async fn cleanup() {
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
    test_seed::ensure_supplier(SUPPLIER_CODE, "M415 供应商评估单测夹具").await
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

/// 一份打分入参（四等分权重）。
fn input_with(supplier_id: Uuid, period_index: usize, scores: DimensionScores) -> grading::EvaluationInput {
    let (start, end) = period(period_index);
    grading::EvaluationInput {
        supplier_id,
        period_start: start,
        period_end: end,
        eval_date: end,
        weights: Weights::equal(),
        scores,
        remark: Some(format!("{TAG}夹具")),
        evaluator_id: None,
        evaluator_name: Some("M415 评估人".to_string()),
        operator_id: None,
    }
}

/// 四维同分的入参（总分就等于那个分数，边界值好写）。
fn input(supplier_id: Uuid, period_index: usize, score: &str) -> grading::EvaluationInput {
    let value = dec(score);
    input_with(
        supplier_id,
        period_index,
        DimensionScores {
            quality: value,
            delivery: value,
            price: value,
            service: value,
        },
    )
}

async fn count_of(sql: &str, params: Vec<Value>) -> i64 {
    db().scalar_with_params(sql, params)
        .await
        .unwrap_or_else(|error| panic!("查计数失败：{error}\n{sql}"))
}

/// 某供应商某期间的评估行数（应当恒为 0 或 1）。
async fn documents_of(supplier_id: Uuid, period_index: usize) -> i64 {
    let (start, end) = period(period_index);
    count_of(
        &format!(
            "SELECT count(*) FROM {} WHERE supplier_id = ?::uuid \
                AND period_start = ?::text::date AND period_end = ?::text::date AND deleted_at IS NULL",
            model::EVALUATION_TABLE
        ),
        vec![
            Value::from(&supplier_id),
            Value::Text(model::date_text(start)),
            Value::Text(model::date_text(end)),
        ],
    )
    .await
}

// ============================================================================
// 真库：评级边界（落库后三处对拍）
// ============================================================================

#[tokio::test]
async fn rating_boundaries_hold_in_the_database() {
    let Some(_guard) = setup().await else { return };
    println!("[M4-15] rating_boundaries_hold_in_the_database");
    let supplier_id = supplier().await;

    // (质量, 交期, 价格, 服务, 期望总分, 期望等级)
    // 四等分权重；带 .995 的三组是「中点舍入抬起一档」，其余是阈值本身。
    let cases: [(&str, &str, &str, &str, &str, &str); 12] = [
        ("0", "0", "0", "0", "0.00", model::GRADE_D),
        ("69.99", "69.99", "69.99", "69.99", "69.99", model::GRADE_D),
        // (90+90+90+9.98)/4 = 69.995 → 70.00 → C（若先评级再舍入会判 D）
        ("90", "90", "90", "9.98", "70.00", model::GRADE_C),
        ("70", "70", "70", "70", "70.00", model::GRADE_C),
        ("79.99", "79.99", "79.99", "79.99", "79.99", model::GRADE_C),
        // (90+90+90+49.98)/4 = 79.995 → 80.00 → B
        ("90", "90", "90", "49.98", "80.00", model::GRADE_B),
        ("80", "80", "80", "80", "80.00", model::GRADE_B),
        // (90+90+90+89.98)/4 = 89.995 → 90.00 → A
        ("90", "90", "90", "89.98", "90.00", model::GRADE_A),
        ("89.99", "89.99", "89.99", "89.99", "89.99", model::GRADE_B),
        ("90", "90", "90", "90", "90.00", model::GRADE_A),
        ("100", "100", "100", "100", "100.00", model::GRADE_A),
        // 两端极值混合：(0+100+0+100)/4 = 50.00
        ("0", "100", "0", "100", "50.00", model::GRADE_D),
    ];

    for (index, (quality, delivery, price, service, expected_total, expected_grade)) in cases.iter().enumerate() {
        let draft = input_with(
            supplier_id,
            index,
            DimensionScores {
                quality: dec(quality),
                delivery: dec(delivery),
                price: dec(price),
                service: dec(service),
            },
        );
        let outcome = grading::save_evaluation(db(), &draft, now())
            .await
            .unwrap_or_else(|error| panic!("第 {index} 组（{expected_total}）打分失败：{error}"));
        let row = &outcome.row;
        assert!(outcome.created, "第 {index} 组是本期第一份，必须新建");
        assert_eq!(
            row.total_score,
            dec(expected_total),
            "第 {index} 组：库层生成列总分应为 {expected_total}"
        );
        assert_eq!(
            row.grade.as_deref(),
            Some(*expected_grade),
            "第 {index} 组：总分 {} 的等级应为 {expected_grade}",
            row.total_score
        );
        // 三处对拍：库层总分 == Rust 试算；等级 == 按库层总分复算的等级
        assert_eq!(
            row.total_score,
            row.recomputed_total(),
            "第 {index} 组：生成列与 weighted_total 漂移"
        );
        assert_eq!(
            grading::grade_for_total(row.total_score).expect("总分合法"),
            *expected_grade,
            "第 {index} 组：写入的等级与阈值表不同源"
        );
        assert_eq!(
            documents_of(supplier_id, index).await,
            1,
            "第 {index} 组：一份期间只能有一份评估"
        );
    }
}

#[tokio::test]
async fn sql_case_agrees_with_rust_on_every_hundredth_point() {
    let Some(_guard) = setup().await else { return };
    println!("[M4-15] sql_case_agrees_with_rust_on_every_hundredth_point");
    // 10001 个点（0.00–100.00，步长 0.01）一次性交给 PG，用**生产路径同一份**
    // grade_case_sql 生成的 CASE 求值，再与 Rust grade_for_total 逐点对拍。
    // 这条用例是「打分（Rust）与 regrade（SQL）不许分叉」的兜底：只改一边必红。
    let mut values = String::with_capacity(80_000);
    let mut expected_points: Vec<Decimal> = Vec::with_capacity(10_001);
    for hundredths in 0..=10_000i64 {
        let point = Decimal::new(hundredths, 2);
        if !expected_points.is_empty() {
            values.push(',');
        }
        values.push_str(&format!("({point})"));
        expected_points.push(point);
    }
    let sql = format!(
        "SELECT x::text AS total, {} AS grade FROM (VALUES {values}) AS t(x) ORDER BY x",
        grading::grade_case_sql("x")
    );
    let rows = db()
        .query_sql(sql, vec![])
        .await
        .unwrap_or_else(|error| panic!("库层执行 grade_case_sql 失败：{error}"));
    assert_eq!(rows.len(), expected_points.len(), "SQL 必须逐点返回 10001 行");
    let mut mismatches: Vec<String> = Vec::new();
    for row in &rows {
        let text: String = row
            .get_typed("total")
            .unwrap_or_else(|error| panic!("读 total 失败：{error}"));
        let sql_grade: Option<String> = row
            .get_typed("grade")
            .unwrap_or_else(|error| panic!("读 grade 失败：{error}"));
        let total = dec(&text);
        let rust_grade = grading::grade_for_total(total).expect("总分合法");
        if sql_grade.as_deref() != Some(rust_grade) {
            mismatches.push(format!("总分 {text}：SQL {:?} vs Rust {rust_grade}", sql_grade));
        }
    }
    assert!(
        mismatches.is_empty(),
        "SQL CASE 与 Rust 阈值在 {} 个点上分叉（前 5 个）：{:?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(5)]
    );
}

// ============================================================================
// 真库：并发打分同一期间（M4-13 README 点名的缺口）
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_scoring_of_one_period_keeps_one_document() {
    let Some(_guard) = setup().await else { return };
    println!("[M4-15] concurrent_scoring_of_one_period_keeps_one_document");
    let supplier_id = supplier().await;
    // 四路并发：四个评估人同时给同一供应商同一期间打分，分数各不相同（真实场景：
    // 两人同时点保存）。唯一键只允许一行，后到者必须**退化成覆盖**而不是抛错。
    let variants = ["61", "74", "88", "96"];
    for round in 0..5usize {
        let mut drafts: Vec<grading::EvaluationInput> = Vec::new();
        for (slot, score) in variants.iter().enumerate() {
            let mut draft = input(supplier_id, round, score);
            draft.remark = Some(format!("{TAG}并发-r{round}-s{slot}"));
            drafts.push(draft);
        }
        let (r0, r1, r2, r3) = tokio::join!(
            grading::save_evaluation(db(), &drafts[0], now()),
            grading::save_evaluation(db(), &drafts[1], now()),
            grading::save_evaluation(db(), &drafts[2], now()),
            grading::save_evaluation(db(), &drafts[3], now()),
        );
        let outcomes = [r0, r1, r2, r3];
        let mut ids: Vec<Uuid> = Vec::new();
        for (slot, outcome) in outcomes.into_iter().enumerate() {
            let outcome = outcome
                .unwrap_or_else(|error| panic!("第 {round} 轮第 {slot} 路并发打分失败（并发不许给用户报错）：{error}"));
            ids.push(outcome.row.id);
        }
        // 四个返回值必须是同一份单据
        assert!(
            ids.iter().all(|id| *id == ids[0]),
            "第 {round} 轮：并发写入出现多份单据 {ids:?}"
        );
        // 库里同一供应商同一期间只能有一行
        assert_eq!(
            documents_of(supplier_id, round).await,
            1,
            "第 {round} 轮：一份期间只能有一份评估"
        );
        // 落库的那一行必须自洽（不能出现「分数是 A 的、等级是 B 的」撕裂行）
        let row = model::evaluation_of_period(db(), supplier_id, period(round).0, period(round).1)
            .await
            .expect("查该期间评估")
            .expect("并发写入后必须查得到");
        assert_eq!(
            row.total_score,
            row.recomputed_total(),
            "第 {round} 轮：总分与四维分数不自洽（撕裂写入）"
        );
        assert_eq!(
            row.grade.as_deref(),
            Some(grading::grade_for_total(row.total_score).expect("总分合法")),
            "第 {round} 轮：等级与总分不自洽（撕裂写入）"
        );
        let expected: Vec<(Decimal, &'static str)> = variants
            .iter()
            .map(|score| {
                let value = dec(score);
                (value, grading::grade_for_total(value).expect("总分合法"))
            })
            .collect();
        assert!(
            expected
                .iter()
                .any(|(total, grade)| *total == row.total_score && *grade == row.grade.as_deref().unwrap_or("")),
            "第 {round} 轮：落库的（总分 {}, 等级 {:?}）不是任何一路的完整写入",
            row.total_score,
            row.grade
        );
        let remark = row.remark.unwrap_or_default();
        assert!(
            (0..variants.len()).any(|slot| remark == format!("{TAG}并发-r{round}-s{slot}")),
            "第 {round} 轮：备注 {remark} 不是任何一路的写入（说明行被拼装过）"
        );
        // 单号也只有一张（取号没被并发写乱）
        let numbers = count_of(
            &format!(
                "SELECT count(DISTINCT evaluation_no) FROM {} WHERE supplier_id = ?::uuid \
                    AND period_start = ?::text::date AND deleted_at IS NULL",
                model::EVALUATION_TABLE
            ),
            vec![
                Value::from(&supplier_id),
                Value::Text(model::date_text(period(round).0)),
            ],
        )
        .await;
        assert_eq!(numbers, 1, "第 {round} 轮：同一期间只该有一个单号");
    }
}

// ============================================================================
// 真库：跨期重打分（M4-13 README 点名的缺口）
// ============================================================================

#[tokio::test]
async fn cross_period_scoring_makes_independent_documents() {
    let Some(_guard) = setup().await else { return };
    println!("[M4-15] cross_period_scoring_makes_independent_documents");
    let supplier_id = supplier().await;

    let first = grading::save_evaluation(db(), &input(supplier_id, 0, "60"), now())
        .await
        .expect("期间一打分");
    let second = grading::save_evaluation(db(), &input(supplier_id, 1, "80"), now())
        .await
        .expect("期间二打分");
    let third = grading::save_evaluation(db(), &input(supplier_id, 2, "95"), now())
        .await
        .expect("期间三打分");
    assert!(first.created && second.created && third.created, "三个期间各是新单据");
    let nos = [
        first.row.evaluation_no.clone(),
        second.row.evaluation_no.clone(),
        third.row.evaluation_no.clone(),
    ];
    assert_eq!(
        nos.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "三个期间三张单号，不能复用：{nos:?}"
    );
    assert_eq!(first.row.grade.as_deref(), Some(model::GRADE_D), "60 = D");
    assert_eq!(second.row.grade.as_deref(), Some(model::GRADE_B), "80 = B");
    assert_eq!(third.row.grade.as_deref(), Some(model::GRADE_A), "95 = A");

    // 重打「期间二」：只改它，另两期一个字节都不动
    let rescored = grading::save_evaluation(db(), &input(supplier_id, 1, "40"), now())
        .await
        .expect("期间二重打分");
    assert!(!rescored.created, "同一期间重打分是覆盖，不是新单据");
    assert_eq!(rescored.row.id, second.row.id, "覆盖同一行");
    assert_eq!(rescored.row.evaluation_no, second.row.evaluation_no, "重打分不改单号");
    assert_eq!(rescored.row.total_score, dec("40.00"), "分数按新入参");
    assert_eq!(rescored.row.grade.as_deref(), Some(model::GRADE_D), "40 = D");

    let again_first = model::evaluation_of_period(db(), supplier_id, period(0).0, period(0).1)
        .await
        .expect("查期间一")
        .expect("存在");
    let again_third = model::evaluation_of_period(db(), supplier_id, period(2).0, period(2).1)
        .await
        .expect("查期间三")
        .expect("存在");
    assert_eq!(again_first.total_score, dec("60.00"), "期间一不受期间二重打分影响");
    assert_eq!(again_first.grade.as_deref(), Some(model::GRADE_D));
    assert_eq!(again_third.total_score, dec("95.00"), "期间三不受影响");
    assert_eq!(again_third.grade.as_deref(), Some(model::GRADE_A));

    // 期间边界差一天 = 另一份单据（唯一键含起止两端）
    let mut narrow = input(supplier_id, 3, "70");
    narrow.period_end -= nik::prelude::chrono::Duration::days(1);
    narrow.eval_date = narrow.period_end;
    let narrow_row = grading::save_evaluation(db(), &narrow, now())
        .await
        .expect("期间截止差一天的打分");
    assert!(narrow_row.created, "期间边界不同就是另一份单据");

    // 列表按期末倒序（最近在前）；最近一期 = 期末最新的那份
    let all = model::evaluations_of_supplier(db(), supplier_id)
        .await
        .expect("查该供应商全部评估");
    assert_eq!(all.len(), 4, "该供应商共 4 份评估（3 个整季 + 1 个窄期间）");
    let ends: Vec<NaiveDate> = all.iter().map(|row| row.period_end).collect();
    let mut sorted = ends.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(ends, sorted, "列表必须按期末倒序：{ends:?}");
    let latest = model::latest_evaluation(db(), supplier_id)
        .await
        .expect("查最近一期")
        .expect("存在");
    assert_eq!(latest.period_end, ends[0], "最近一期 = 期末最晚的那份");
    assert_eq!(latest.total_score, dec("70.00"));
}

// ============================================================================
// 真库：拒绝分支不许写坏已有评估
// ============================================================================

#[tokio::test]
async fn rejected_inputs_never_touch_an_existing_evaluation() {
    let Some(_guard) = setup().await else { return };
    println!("[M4-15] rejected_inputs_never_touch_an_existing_evaluation");
    let supplier_id = supplier().await;
    // 基线：期间一已有一份 80 分（B）
    let baseline = grading::save_evaluation(db(), &input(supplier_id, 0, "80"), now())
        .await
        .expect("基线打分");
    assert_eq!(baseline.row.total_score, dec("80.00"));
    let baseline_no = baseline.row.evaluation_no.clone();
    let total_rows = count_of(
        &format!(
            "SELECT count(*) FROM {} WHERE deleted_at IS NULL",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;

    // 七类非法入参（前五类都指向**期间一**：若校验漏了，就会把基线写坏）
    let mut extra_weight = input(supplier_id, 0, "90");
    extra_weight.weights.service = dec("0.30"); // 和 1.1
    let mut negative_weight = input(supplier_id, 0, "90");
    negative_weight.weights.quality = dec("1.5");
    negative_weight.weights.delivery = dec("-0.5");
    let mut over_score = input(supplier_id, 0, "90");
    over_score.scores.price = dec("100.01");
    let mut negative_score = input(supplier_id, 0, "90");
    negative_score.scores.service = dec("-0.01");
    let mut reversed = input(supplier_id, 4, "90");
    reversed.period_start = date("2033-09-30");
    reversed.period_end = date("2033-07-01");
    reversed.eval_date = date("2033-09-30");
    let mut early_eval = input(supplier_id, 5, "90");
    early_eval.eval_date = early_eval.period_start - nik::prelude::chrono::Duration::days(1);
    let mut ghost = input(supplier_id, 6, "90");
    ghost.supplier_id = Uuid::now_v7(); // 库里没有这家供应商

    let cases: [(&str, grading::EvaluationInput); 7] = [
        ("权重和 = 1.1", extra_weight),
        ("权重单项越界（负 / >1，和仍为 1）", negative_weight),
        ("分数 100.01 越界", over_score),
        ("分数 -0.01 越界", negative_score),
        ("期间起晚于止", reversed),
        ("评估日早于期间起点", early_eval),
        ("供应商不存在", ghost),
    ];
    for (why, draft) in cases {
        let error = grading::save_evaluation(db(), &draft, now())
            .await
            .expect_err(&format!("{why} 必须被拒"));
        assert!(
            matches!(error, DbError::InvalidValue(_)),
            "{why} 应是 400 语义（InvalidValue），实际：{error}"
        );
    }

    // ① 基线一个字节没动 ② 表里没多一行 ③ 单号没换
    let after = model::evaluation_of_period(db(), supplier_id, period(0).0, period(0).1)
        .await
        .expect("查基线")
        .expect("基线必须仍在");
    assert_eq!(after.evaluation_no, baseline_no, "非法入参不许换单号");
    assert_eq!(after.total_score, dec("80.00"), "非法入参不许改基线总分");
    assert_eq!(after.scores().quality, dec("80"), "非法入参不许改基线分数");
    assert_eq!(after.grade.as_deref(), Some(model::GRADE_B), "非法入参不许改基等级");
    assert_eq!(
        after.remark.as_deref(),
        Some(format!("{TAG}夹具").as_str()),
        "非法入参不许改基线备注"
    );
    assert_eq!(
        count_of(
            &format!(
                "SELECT count(*) FROM {} WHERE deleted_at IS NULL",
                model::EVALUATION_TABLE
            ),
            vec![]
        )
        .await,
        total_rows,
        "被拒的七类入参一行都不许写"
    );

    // 已软删的供应商：名字快照查不到 → 400（弱引用表没有外键兜底，这一条必须挡）
    let deleted_id = test_seed::ensure_supplier(SUPPLIER_DELETED_CODE, "M415 已删供应商").await;
    exec(
        "UPDATE biz_supplier SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&deleted_id)],
    )
    .await;
    let error = grading::save_evaluation(db(), &input(deleted_id, 7, "90"), now())
        .await
        .expect_err("已软删的供应商不许再评估");
    assert!(matches!(error, DbError::InvalidValue(_)), "应是 400 语义：{error}");
    assert_eq!(documents_of(deleted_id, 7).await, 0, "已软删供应商不许留下评估行");
    exec(
        "UPDATE biz_supplier SET deleted_at = NULL WHERE id = ?::uuid",
        vec![Value::from(&deleted_id)],
    )
    .await;
}

// ============================================================================
// 真库：超大方量 regrade（M4-13 README 点名的缺口）
// ============================================================================

#[tokio::test]
async fn bulk_regrade_is_one_pass_idempotent_and_leaves_scores_alone() {
    let Some(_guard) = setup().await else { return };
    println!("[M4-15] bulk_regrade_is_one_pass_idempotent_and_leaves_scores_alone");
    const ROWS: i64 = 1_200;
    let window_start = date("2031-01-01");
    let window_end = date("2031-03-31");
    // 一次 INSERT … SELECT 造 1200 份同期间评估：四维全 50（真值 50.00 = D），
    // 但等级故意写成 A —— regrade 必须一次全改对。供应商用随机 UUID（弱引用，
    // 不为性能用例去碰主数据 —— P2-62 门禁也不许测试手写 biz_supplier INSERT）。
    db().exec_sql(
        format!(
            "INSERT INTO {} \
                 (id, evaluation_no, supplier_id, supplier_name, period_start, period_end, eval_date, \
                  quality_score, delivery_score, price_score, service_score, grade, remark) \
             SELECT gen_random_uuid(), \
                    'SEV-20310101-' || lpad(g::text, 6, '0'), \
                    gen_random_uuid(), \
                    '{TAG}-BULK-' || g, \
                    DATE '{window_start}', DATE '{window_end}', DATE '{window_end}', \
                    50, 50, 50, 50, 'A', '{TAG}bulk' \
               FROM generate_series(1, {ROWS}) AS g",
            model::EVALUATION_TABLE,
            window_start = model::date_text(window_start),
            window_end = model::date_text(window_end),
        ),
        vec![],
    )
    .await
    .expect("批量造数");

    let scope = Some((window_start, window_end));
    let before_sum = count_of(
        &format!(
            "SELECT COALESCE(sum(total_score), 0)::bigint FROM {} WHERE remark = '{TAG}bulk'",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;
    assert_eq!(before_sum, ROWS * 50, "批量行的总分合计 = 1200 × 50（生成列）");

    let started = Instant::now();
    let changed = grading::regrade_evaluations(db(), scope).await.expect("全量 regrade");
    let elapsed = started.elapsed();
    assert_eq!(changed, ROWS as u64, "1200 行错等级必须一次全改");
    assert!(
        elapsed.as_secs() < 60,
        "1200 行的全表回算耗时 {elapsed:?} 已超过 60s 粗上限（sql_case_agrees_with_rust_on_every_hundredth_point 之外的性能红线）"
    );
    println!("[M4-15-bulk] bulk regrade 1200 行耗时 {elapsed:?}");

    // ① 等级全改成 D（50 分）② 分数与单号一个没动 ③ 第二遍 0 改动
    let wrong = count_of(
        &format!(
            "SELECT count(*) FROM {} WHERE remark = '{TAG}bulk' AND grade <> 'D'",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;
    assert_eq!(wrong, 0, "regrade 后不许还有错等级的行");
    let after_sum = count_of(
        &format!(
            "SELECT COALESCE(sum(total_score), 0)::bigint FROM {} WHERE remark = '{TAG}bulk'",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;
    assert_eq!(after_sum, before_sum, "regrade 只改等级，分数（生成列总分）必须不动");
    let sample = db()
        .query_sql(
            format!(
                "SELECT evaluation_no, quality_score::text AS q, quality_weight::text AS w, grade \
                   FROM {} WHERE remark = '{TAG}bulk' ORDER BY evaluation_no LIMIT 1",
                model::EVALUATION_TABLE
            ),
            vec![],
        )
        .await
        .expect("取样例行");
    let row = sample.first().expect("至少一行");
    assert_eq!(
        row.get_typed::<String>("evaluation_no").expect("单号"),
        "SEV-20310101-000001"
    );
    assert_eq!(row.get_typed::<String>("q").expect("质量分"), "50.00");
    assert_eq!(row.get_typed::<String>("w").expect("质量权重"), "0.2500");
    assert_eq!(row.get_typed::<String>("grade").expect("等级"), "D");

    let again = grading::regrade_evaluations(db(), scope).await.expect("再跑一次");
    assert_eq!(again, 0, "regrade 幂等：等级都对了就不改（1200 行也一样）");
}

// ============================================================================
// 清场
// ============================================================================

#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    println!("[M4-15] zz_teardown_purges_fixtures");
    let left = count_of(
        &format!(
            "SELECT count(*) FROM {} WHERE supplier_name LIKE '{TAG}%' OR remark LIKE '{TAG}%'",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;
    assert_eq!(left, 0, "{TAG} 夹具必须 0 残留");
}

// ============================================================================
// supplier_evaluation_handler_db_tests.rs —— 供应商评估接口层的真库用例（M4-16）
//
// 卡：t_005d6b65（M4-16【M4·采购】供应商评估：前端页面 = 评估录入 + 排名）
//
// # 为什么这一层也要真库用例
//
// M4-13 已经测过「打分 + 评级」的数据层（`save_evaluation`），但本卡新写了两段
// **没有任何用例走过** 的 SQL：`model::page_evaluations`（台账分页 + 关键字 + 等级筛选）
// 与 `model::evaluations_in_period`（排名排序 + LIMIT）。这两段一旦写错（`?::text::date`
// 绑定类型、`evaluations_in_period` 排序键），页面就是「打开一片空白」或「名次乱序」，
// 而纯逻辑单测抓不到 —— 必须连真库跑。
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `save_creates_then_overwrites_same_document` | 走 handler 的入口（不是直接调 model）：同供应商同期间第二次 = 覆盖，单号与 id 不变，总分与等级来自库 |
// | `page_filters_by_grade_keyword_and_period` | 台账分页：等级筛选 / 关键字（单号 / 供应商名 / 备注）/ 期间筛选各自命中且不分页错位 |
// | `rank_orders_by_total_desc_and_numbers_from_one` | 排名：总分降序、名次从 1 起不并列、期间标签（`2028-Q1`）、`limit` 生效 |
// | `zz_teardown_purges_fixtures` | 清场：`M416-` 夹具 0 残留（评估单 / 供应商） |
//
// # 夹具纪律
//
// 夹具一律 `M416-` 前缀（供应商编码 / 备注），清场只按自己的前缀删。
// 供应商走 `test_seed::ensure_supplier`（确定性 id + 幂等 upsert），测试代码不手写主数据。
//
// ⚠ 真库用例必须串行（共用全局 `db()` 与 `aging_db_tests::DB_LOCK`），跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m416_sev_page" cargo test --lib \
//   business::erp::purchase::supplier_evaluation_handler -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::purchase::supplier_evaluation_dto::{EvaluationPageQuery, RankQuery, SaveEvaluationReq};
use crate::business::erp::purchase::supplier_evaluation_handler as handler;
use crate::business::erp::purchase::supplier_evaluation_model as model;
use crate::test_seed;

/// 夹具名前缀（清场按它反查）。
const TAG: &str = "M416-";
/// 夹具供应商编码（三家，排名用例要三家同一期间）。
const SUPPLIER_CODES: [&str; 3] = ["M416-SUP-A", "M416-SUP-B", "M416-SUP-C"];

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

/// 清场：只删本卡的夹具（评估单先删，再删夹具供应商）。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM {} WHERE supplier_name LIKE '{TAG}%' OR remark LIKE '{TAG}%'",
            model::EVALUATION_TABLE
        ),
        vec![],
    )
    .await;
    let params: Vec<Value> = SUPPLIER_CODES
        .iter()
        .map(|code| Value::Text((*code).to_string()))
        .collect();
    exec("DELETE FROM biz_supplier WHERE supplier_code IN (?, ?, ?)", params).await;
}

async fn supplier(code: &str) -> Uuid {
    test_seed::ensure_supplier(code, &format!("M416 供应商评估页夹具 {code}")).await
}

/// 一份打分入参（四等分权重 + 四维同分 ⇒ 总分 = 该分数，等级好算）。
fn save_req(supplier_id: Uuid, start: &str, end: &str, score: &str) -> SaveEvaluationReq {
    SaveEvaluationReq {
        supplier_id: Some(supplier_id.to_string()),
        period_start: Some(start.to_string()),
        period_end: Some(end.to_string()),
        eval_date: Some(end.to_string()),
        quality_weight: Some("0.25".to_string()),
        delivery_weight: Some("0.25".to_string()),
        price_weight: Some("0.25".to_string()),
        service_weight: Some("0.25".to_string()),
        quality_score: Some(score.to_string()),
        delivery_score: Some(score.to_string()),
        price_score: Some(score.to_string()),
        service_score: Some(score.to_string()),
        remark: Some(format!("{TAG}夹具")),
    }
}

fn page_query(grade: Option<&str>, keyword: Option<&str>) -> EvaluationPageQuery {
    EvaluationPageQuery {
        page: Some(1),
        page_size: Some(50),
        keyword: keyword.map(str::to_string),
        grade: grade.map(str::to_string),
        supplier_id: None,
        period_start: None,
        period_end: None,
    }
}

// ==================== ① 打分入口：新建 → 覆盖 ====================

#[tokio::test]
async fn save_creates_then_overwrites_same_document() {
    let Some(_guard) = setup().await else {
        return;
    };
    let supplier_id = supplier(SUPPLIER_CODES[0]).await;

    // 第一次：取号新建，总分 = 四维同分 = 95.00（A）
    let first = handler::save_evaluation_core(&save_req(supplier_id, "2028-01-01", "2028-03-31", "95"), None)
        .await
        .expect("第一次打分应成功");
    assert!(first.created, "第一次必须是新建");
    assert_eq!(first.row.total_score.to_string(), "95.00", "总分来自库层生成列");
    assert_eq!(first.row.grade.as_deref(), Some("A"));
    assert_eq!(first.row.period_label, "2028-Q1", "期间标签由后端给（季度）");
    assert!(
        first.row.evaluation_no.starts_with("SEV-"),
        "单号形如 SEV-yyyyMMdd-000001：{}",
        first.row.evaluation_no
    );

    // 第二次：同供应商同期间再打一次 = 覆盖（单号与 id 都不变，等级跟着新总分走）
    let second = handler::save_evaluation_core(&save_req(supplier_id, "2028-01-01", "2028-03-31", "75"), None)
        .await
        .expect("重新打分应成功（同期间覆盖）");
    assert!(!second.created, "同期间第二次必须是覆盖，不是新建");
    assert_eq!(second.row.id, first.row.id, "覆盖不许换 id");
    assert_eq!(second.row.evaluation_no, first.row.evaluation_no, "覆盖不许换单号");
    assert_eq!(second.row.total_score.to_string(), "75.00");
    assert_eq!(second.row.grade.as_deref(), Some("C"), "75 分落在 [70,80) ⇒ C");

    // 库里只有一张：一份期间一个结论
    let rows: i64 = db()
        .scalar_with_params(
            &format!(
                "SELECT count(*) FROM {} WHERE supplier_id = ?::uuid AND deleted_at IS NULL",
                model::EVALUATION_TABLE
            ),
            vec![Value::from(&supplier_id)],
        )
        .await
        .expect("查评估单张数");
    assert_eq!(rows, 1, "同一供应商同一期间只许有一份评估");
}

// ==================== ② 台账分页：等级 / 关键字 / 期间 ====================

#[tokio::test]
async fn page_filters_by_grade_keyword_and_period() {
    let Some(_guard) = setup().await else {
        return;
    };
    let supplier_id = supplier(SUPPLIER_CODES[0]).await;

    // 三个期间三种等级（四等分权重 ⇒ 总分 = 分数）
    for (start, end, score, grade) in [
        ("2028-01-01", "2028-03-31", "95", "A"),
        ("2028-04-01", "2028-06-30", "85", "B"),
        ("2028-07-01", "2028-09-30", "65", "D"),
    ] {
        let res = handler::save_evaluation_core(&save_req(supplier_id, start, end, score), None)
            .await
            .expect("打分应成功");
        assert_eq!(res.row.grade.as_deref(), Some(grade), "{score} 分应评 {grade}");
    }

    // 等级筛选：只命中 A 那一份
    let filter = page_query(Some("A"), None).filter().expect("等级闭集内");
    let (rows, total) = model::page_evaluations(&filter).await.expect("台账分页");
    assert_eq!(total, 1, "库里只有一份 A 级（另两份是 B / D）");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].grade.as_deref(), Some("A"));

    // 关键字：单号前缀 / 供应商名快照 / 备注，三种都能查到同一家
    for keyword in ["SEV-", TAG] {
        let filter = page_query(None, Some(keyword)).filter().expect("关键字合法");
        let (rows, total) = model::page_evaluations(&filter).await.expect("台账分页");
        assert!(total >= 3, "关键字 {keyword} 应命中三份，实际 {total}");
        assert!(rows.iter().all(|row| row.supplier_id == supplier_id));
    }

    // 期间筛选：整段相等（不是区间相交）
    let mut query = page_query(None, None);
    query.period_start = Some("2028-04-01".to_string());
    query.period_end = Some("2028-06-30".to_string());
    let filter = query.filter().expect("期间合法");
    let (rows, total) = model::page_evaluations(&filter).await.expect("台账分页");
    assert_eq!(total, 1, "只有 Q2 那一份落在该期间");
    assert_eq!(rows[0].grade.as_deref(), Some("B"));
}

// ==================== ③ 排名：降序 + 名次从 1 起 ====================

#[tokio::test]
async fn rank_orders_by_total_desc_and_numbers_from_one() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 三家同一期间，分数故意乱序录（B 最高先录、C 最低、A 居中）
    let scores = [(0usize, "85"), (1usize, "92"), (2usize, "70")];
    for (index, score) in scores {
        let supplier_id = supplier(SUPPLIER_CODES[index]).await;
        handler::save_evaluation_core(&save_req(supplier_id, "2028-01-01", "2028-03-31", score), None)
            .await
            .expect("打分应成功");
    }

    let query = RankQuery {
        period_start: Some("2028-01-01".to_string()),
        period_end: Some("2028-03-31".to_string()),
        limit: Some(100),
    };
    let (start, end, limit) = query.range().expect("期间合法");
    let rows = model::evaluations_in_period(db(), start, end, limit)
        .await
        .expect("查排名");
    assert_eq!(rows.len(), 3, "该期间三家都参与排名");

    let rank = handler::build_rank(start, end, &rows);
    assert_eq!(rank.period_label, "2028-Q1");
    assert_eq!(rank.total, 3);
    let order: Vec<String> = rank.rows.iter().map(|item| item.row.total_score.to_string()).collect();
    assert_eq!(order, vec!["92.00", "85.00", "70.00"], "必须按总分降序");
    let numbers: Vec<u32> = rank.rows.iter().map(|item| item.rank).collect();
    assert_eq!(numbers, vec![1, 2, 3], "名次从 1 起、不并列");
    // 第 1 名的等级也跟着总分走（92 ⇒ A）
    assert_eq!(rank.rows[0].row.grade.as_deref(), Some("A"));

    // limit 生效：只要前 2 名
    let top = model::evaluations_in_period(db(), start, end, 2)
        .await
        .expect("查前 2 名");
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].total_score.to_string(), "92.00");
    assert_eq!(top[1].total_score.to_string(), "85.00");

    // 别的期间查不到这三家（期间是全等，不是区间相交）
    let other = model::evaluations_in_period(db(), date("2028-04-01"), date("2028-06-30"), 100)
        .await
        .expect("查别的期间");
    assert!(other.is_empty(), "Q2 没有评估 ⇒ 排名为空（页面显示空态）");
}

// ==================== ④ 清场 ====================

#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 造一批（走真实入口，保证清场要处理的是真实形态的单）
    let supplier_id = supplier(SUPPLIER_CODES[0]).await;
    handler::save_evaluation_core(&save_req(supplier_id, "2028-01-01", "2028-03-31", "90"), None)
        .await
        .expect("打分应成功");
    let before = count_fixtures().await;
    assert!(before > 0, "清场前必须有夹具（否则本用例是假绿）");

    cleanup().await;
    assert_eq!(count_fixtures().await, 0, "清场后 M416- 夹具必须 0 残留");
}

/// `M416-` 夹具残留（评估单 + 供应商）。
async fn count_fixtures() -> i64 {
    let evaluations: i64 = db()
        .scalar_with_params(
            &format!(
                "SELECT count(*) FROM {} WHERE supplier_name LIKE '{TAG}%' OR remark LIKE '{TAG}%'",
                model::EVALUATION_TABLE
            ),
            vec![],
        )
        .await
        .expect("查评估夹具残留");
    let suppliers: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_supplier WHERE supplier_code IN (?, ?, ?)",
            SUPPLIER_CODES
                .iter()
                .map(|code| Value::Text((*code).to_string()))
                .collect(),
        )
        .await
        .expect("查供应商夹具残留");
    evaluations + suppliers
}

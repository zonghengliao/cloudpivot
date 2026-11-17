//! 供应商评估打分 + 评级（M4-13）。
//!
//! 表结构见 `V0454__supplier_evaluation.up.sql`（M4-12），本文件**只做两件事**：
//!
//! ```text
//! ① 打分：把四维分数 + 四维权重算成加权总分，并落库（同一供应商同一期间只有一份评估 —— 重打分 = 改这一份）
//! ② 评级：按总分给出 A/B/C/D（阈值 = 本文件的 GRADE_THRESHOLDS，唯一一处）；阈值可回算历史单（regrade）
//! ```
//!
//! | 入口 | 作用 |
//! |------|------|
//! | [`grade_for_total`] | 总分 → 等级（纯函数，阈值唯一出处） |
//! | [`grade_and_total`] | 四维权重 + 四维分数 → (等级, 总分)（权重的合法性与 [`weighted_total`] 同源） |
//! | [`grade_case_sql`] | 用**同一份阈值**生成 SQL `CASE`（regrade 用；阈值不许出现第二份） |
//! | [`save_evaluation`] | 打分落库：无则取号新建、有则重打分覆盖，写完读回对拍生成列总分 |
//! | [`regrade_evaluations`] | 按当前阈值重写等级（可限定期间），返回改动行数 |
//! | [`delivery_stat`] / [`DeliveryStat::suggested_score`] | 交期维度的**客观参考分**：按真实入库单 vs 采购单交期算准时率 |
//!
//! # 评级阈值（政策 · 决定，不是待办）
//!
//! ```text
//! A 优秀 : 总分 >= 90      含下界（90.00 就是 A）
//! B 良好 : 80 <= 总分 < 90
//! C 合格 : 70 <= 总分 < 80
//! D 不合格: 总分 < 70      含 0 分
//! ```
//!
//! 为什么阈值写在代码里、而不是写进 DDL 或配置表：
//!
//! * 写进 `CHECK` 生成列 → 调阈值必须发迁移，而阈值是**年年会调的商务政策**（M4-12 头注释 3 同一条理由）；
//! * 写进配置表 → 要再造一张「评估配置」表 + 一套编辑界面，本仓目前没有任何卡定义它，造了就是死结构；
//! * 因此：**阈值只有一个出处 = [`GRADE_THRESHOLDS`]**，`save_evaluation` 与 `regrade_evaluations`
//!   都从它派生（后者由 [`grade_case_sql`] 拼 SQL），改阈值只改一处 + 跑一次 regrade。
//!
//! # 打分口径（哪些维度能自动算，哪些不能 —— 诚实写清）
//!
//! * **交期 DELIVERY 有客观数据**：`biz_stock_in`（`in_type='PURCHASE'`、`status='POSTED'`、
//!   未删）挂 `source_order_id` → `biz_purchase_order.expect_date`；比较 `in_date` 与 `expect_date`
//!   得到准时数量占比。见 [`delivery_stat`] / [`DeliveryStat::suggested_score`]。
//! * **质量 QUALITY / 价格 PRICE / 服务 SERVICE 没有数据源**：本仓没有来料质检表（`biz_stock_in_item`
//!   没有合格/拒收数量）、也没有供应商比价基准表（`biz_price` 是物料价格策略，不是同料跨供应商报价）。
//!   这三维**由评估人打分**，本文件不编公式 —— 编出来的「质量分」是假数据，不如明确写「人工评」。
//!
//! # 重打分语义
//!
//! `uk_biz_sev_supplier_period` 决定「一份供应商一个期间只有一个结论」。所以 [`save_evaluation`]：
//!
//! * 该供应商该期间**没有**评估 → 取号（`SEV-yyyyMMdd-000001`）+ 快照供应商名 + 写等级；
//! * **已有** → 覆盖四维分数 / 权重 / 评估日 / 评估人 / 备注 / 等级，**单号不变**
//!   （重打分不是新单据；历史期间结论要靠单号回溯）。
//! * 并发建同一份：先撞 `unique_violation` 的那一方**退化成覆盖**（重查一遍再 UPDATE），
//!   不把 500 抛给用户（同供应商同期间本来就只有一份，用户意图是相同的）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_supplier_evaluation 是公司级采购文件且无 dept 归属列（M4-12 头注释 7），
//!         不登记 shared::scope::SCOPED_TABLES；本文件只提供数据层入口，可见性由 M4-16 的权限码控制。
//! ```
//!
//! # 单测
//!
//! 纯逻辑在本文件 `mod tests`；真库用例在 `supplier_evaluation_grading_db_tests.rs`
//! （`ERP_DB_URL` 开关，与其它 ERP 用例共用 `DB_LOCK` 串行）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase::supplier_evaluation_model as model;
use crate::business::erp::purchase::supplier_evaluation_model::{
    DimensionScores, EvaluationRow, TOTAL_ROUNDING, TOTAL_SCALE, Weights, weighted_total,
};

// ==================== 评级阈值（政策唯一出处） ====================

/// 等级阈值表：`(等级, 总分下界)`，**按下界降序**，比较一律「含下界」（`>=`）。
///
/// 最后一项必须是 `0`（覆盖 `[0, 70)` 的剩余区间）——[`grade_for_total`] 依赖它兜底，
/// 越界总分（<0 / >100）反而报错而不是猜等级。
pub const GRADE_THRESHOLDS: [(&str, &str); 4] = [
    (model::GRADE_A, "90"),
    (model::GRADE_B, "80"),
    (model::GRADE_C, "70"),
    (model::GRADE_D, "0"),
];

/// 阈值字面量 → `Decimal`（常量表用字符串存，避免依赖 `Decimal` 的 const 构造能力）。
fn threshold_dec(text: &str) -> Decimal {
    text.parse().expect("阈值字面量必须是定点数")
}

/// 总分 → 等级。
///
/// * `total` 必须已按 [`TOTAL_SCALE`] 舍入（[`weighted_total`] / 库层生成列都是 2 位）；
/// * 越界（`< 0` 或 `> 100`）返回 `InvalidValue` —— 那是算错/写错，不能默默给个 D。
pub fn grade_for_total(total: Decimal) -> DbResult<&'static str> {
    let max = Decimal::from(model::SCORE_MAX);
    if total < Decimal::ZERO || total > max {
        return Err(DbError::InvalidValue(format!(
            "加权总分 {total} 不在 0–{max} 之间，无法评级"
        )));
    }
    for (grade, min_total) in GRADE_THRESHOLDS {
        if total >= threshold_dec(min_total) {
            return Ok(grade);
        }
    }
    Err(DbError::InvalidValue(format!(
        "加权总分 {total} 没有命中任何等级阈值（阈值表必须由 0 兜底）"
    )))
}

/// 四维权重 + 四维分数 → (等级, 加权总分)。
///
/// 权重 / 分数先按**与库层 CHECK 同一份判据**校验（[`Weights::is_valid`] / [`DimensionScores::is_valid`]），
/// 不合法在这里就 `InvalidValue`（调用方拿到 400，而不是等库层抛 500）。
pub fn grade_and_total(weights: &Weights, scores: &DimensionScores) -> DbResult<(&'static str, Decimal)> {
    if !weights.is_valid() {
        return Err(DbError::InvalidValue(
            "四维权重必须各在 0–1 且之和精确等于 1（ck_biz_sev_weights）".to_string(),
        ));
    }
    if !scores.is_valid() {
        return Err(DbError::InvalidValue(format!(
            "四维分数必须各在 0–{}（ck_biz_sev_scores）",
            model::SCORE_MAX
        )));
    }
    let total = weighted_total(weights, scores);
    Ok((grade_for_total(total)?, total))
}

/// 用 [`GRADE_THRESHOLDS`] 生成 SQL `CASE`（regrade 用）。
///
/// 生成形态（阈值表原样映射，不手写第二份）：
///
/// ```text
/// CASE WHEN total_score IS NULL THEN NULL
///      WHEN total_score >= 90 THEN 'A'
///      WHEN total_score >= 80 THEN 'B'
///      WHEN total_score >= 70 THEN 'C'
///      WHEN total_score >= 0  THEN 'D'
///      ELSE 'D' END
/// ```
///
/// `total_score` 为 NULL 时给 NULL（= 未评级），**不许**把「没有总分」算成 D。
pub fn grade_case_sql(column: &str) -> String {
    let fallback = GRADE_THRESHOLDS.last().map(|(grade, _)| *grade).expect("阈值表非空");
    let mut sql = format!("CASE WHEN {column} IS NULL THEN NULL");
    for (grade, min_total) in GRADE_THRESHOLDS {
        sql.push_str(&format!(" WHEN {column} >= {min_total} THEN '{grade}'"));
    }
    sql.push_str(&format!(" ELSE '{fallback}' END"));
    sql
}

// ==================== 交期维度的客观参考分 ====================

/// 一段时间内某供应商的到货准时情况（按**入库行数量**统计，不看金额）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeliveryStat {
    /// 参与统计的入库单张数（`POSTED` + 挂采购单 + 有交期）。
    pub docs: i64,
    /// 其中含逾期行的入库单张数。
    pub late_docs: i64,
    /// 到货总数量。
    pub total_qty: Decimal,
    /// 准时（`in_date <= expect_date`）行数量合计。
    pub on_time_qty: Decimal,
    /// 逾期（`in_date > expect_date`）行数量合计。
    pub late_qty: Decimal,
}

impl DeliveryStat {
    /// 空统计（该供应商该期间没有任何已过账的采购入库）。
    pub fn empty() -> Self {
        Self {
            docs: 0,
            late_docs: 0,
            total_qty: Decimal::ZERO,
            on_time_qty: Decimal::ZERO,
            late_qty: Decimal::ZERO,
        }
    }

    /// 是否有可用于打分的到货数据（没有就只能人工打分）。
    pub fn is_usable(&self) -> bool {
        self.total_qty > Decimal::ZERO
    }

    /// 准时率（0–100，2 位小数）。**没有到货数量时返回 `None`** —— 不把「无数据」写成 0 分
    /// （0 分是「全部逾期」的意思，两者不能混）。
    pub fn on_time_rate(&self) -> Option<Decimal> {
        if !self.is_usable() {
            return None;
        }
        let hundred = Decimal::from(model::SCORE_MAX);
        Some((self.on_time_qty * hundred / self.total_qty).round_dp_with_strategy(TOTAL_SCALE, TOTAL_ROUNDING))
    }

    /// 交期维度的**建议分**（= 准时率）。前端可一键填入，评估人仍可改。
    pub fn suggested_score(&self) -> Option<Decimal> {
        self.on_time_rate()
    }
}

/// 算某供应商在 `[period_start, period_end]`（按入库业务日期 `in_date`）内的交期统计。
///
/// 口径（逐条对应 SQL）：
///
/// * 单据：`biz_stock_in` 未删 + `status='POSTED'` + `in_type='PURCHASE'` + `supplier_id = 入参`；
/// * 必须挂采购单（`source_order_id`）且采购单未删、`expect_date` 非空 —— 没有交期就无法判准时；
/// * 行级判定：`si.in_date <= po.expect_date` 为准时（部分到货也按行算）；
/// * `DRAFT` / `PENDING_*` / `CANCELLED` 一概不计（没入账的到货不是事实）。
pub async fn delivery_stat(
    ex: &impl SqlExecutor,
    supplier_id: Uuid,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> DbResult<DeliveryStat> {
    // SCOPE-EXEMPT: 统计口径随评估表的可见性走（见模块头「数据权限」）。
    let rows = ex
        .query_sql(
            "SELECT count(DISTINCT si.id) AS docs, \
                    count(DISTINCT si.id) FILTER (WHERE si.in_date > po.expect_date) AS late_docs, \
                    COALESCE(sum(i.qty), 0) AS total_qty, \
                    COALESCE(sum(i.qty) FILTER (WHERE si.in_date <= po.expect_date), 0) AS on_time_qty \
               FROM biz_stock_in si \
               JOIN biz_purchase_order po \
                 ON po.id = si.source_order_id AND po.deleted_at IS NULL \
               JOIN biz_stock_in_item i ON i.stock_in_id = si.id \
              WHERE si.deleted_at IS NULL \
                AND si.status = 'POSTED' \
                AND si.in_type = 'PURCHASE' \
                AND si.supplier_id = ?::uuid \
                AND si.in_date >= ?::text::date \
                AND si.in_date <= ?::text::date \
                AND po.expect_date IS NOT NULL"
                .to_string(),
            vec![
                Value::from(&supplier_id),
                Value::Text(model::date_text(period_start)),
                Value::Text(model::date_text(period_end)),
            ],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(DeliveryStat::empty());
    };
    let total_qty: Decimal = row.get_typed("total_qty")?;
    let on_time_qty: Decimal = row.get_typed("on_time_qty")?;
    let late_qty = total_qty - on_time_qty;
    Ok(DeliveryStat {
        docs: row.get_typed("docs")?,
        late_docs: row.get_typed("late_docs")?,
        total_qty,
        on_time_qty,
        late_qty,
    })
}

// ==================== 打分落库（新建 / 重打分） ====================

/// 一次打分的全部入参（四维分数 + 权重 + 期间/评估人）。
///
/// 供应商名**不**从入参来：落库时按 `supplier_id` 查现名写快照（约定 13：评估结论针对当时那家供应商）。
#[derive(Debug, Clone)]
pub struct EvaluationInput {
    /// 供应商（必须存在且未删）。
    pub supplier_id: Uuid,
    /// 评估期间起（含）。
    pub period_start: NaiveDate,
    /// 评估期间止（含）。
    pub period_end: NaiveDate,
    /// 评估日（不许早于期间起点）。
    pub eval_date: NaiveDate,
    /// 四维权重（各 0–1、之和精确 = 1）。
    pub weights: Weights,
    /// 四维分数（各 0–100）。
    pub scores: DimensionScores,
    /// 备注（扣分事由 / 改进要求）。
    pub remark: Option<String>,
    /// 评估人。
    pub evaluator_id: Option<Uuid>,
    /// 评估人姓名快照（审计/历史类，约定 13）。
    pub evaluator_name: Option<String>,
    /// 操作人（写 `create_by` / `update_by`）。
    pub operator_id: Option<Uuid>,
}

/// 落库结果。
#[derive(Debug, Clone)]
pub struct SaveOutcome {
    /// 落库后的整行（总分取自库层生成列）。
    pub row: EvaluationRow,
    /// `true` = 本次取号新建；`false` = 覆盖已有评估（重打分）。
    pub created: bool,
}

/// 入参校验（先于任何一条 SQL）：期间 / 评估日 / 权重 / 分数。
///
/// 这些判据与库层 CHECK 一一对应，在代码里先判一遍是为了把「400 语义」的错误挡在写库之前。
fn validate_input(input: &EvaluationInput) -> DbResult<()> {
    if !model::period_is_valid(input.period_start, input.period_end) {
        return Err(DbError::InvalidValue(format!(
            "评估期间起 {} 晚于止 {}（ck_biz_sev_period）",
            model::date_text(input.period_start),
            model::date_text(input.period_end)
        )));
    }
    if !model::eval_date_is_valid(input.period_start, input.eval_date) {
        return Err(DbError::InvalidValue(format!(
            "评估日 {} 早于评估期间起点 {}（ck_biz_sev_eval_date）",
            model::date_text(input.eval_date),
            model::date_text(input.period_start)
        )));
    }
    grade_and_total(&input.weights, &input.scores).map(|_| ())
}

/// 供应商现名（姓名快照来源）。供应商不存在 → `InvalidValue`（弱引用表没有外键兜底）。
async fn supplier_snapshot(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<String> {
    // SCOPE-EXEMPT: 供应商主数据无归属列（与 biz_supplier_evaluation 同口径）。
    let rows = ex
        .query_sql(
            "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    match rows.first() {
        Some(row) => row.get_typed("supplier_name"),
        None => Err(DbError::InvalidValue(format!(
            "供应商 {supplier_id} 不存在或已删，无法评估"
        ))),
    }
}

fn optional_uuid(value: Option<Uuid>) -> Value {
    match value {
        Some(id) => Value::from(&id),
        None => Value::Null,
    }
}

fn optional_str(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_string()),
        None => Value::Null,
    }
}

/// 取号新建一份评估。
#[allow(clippy::too_many_arguments)]
async fn insert_evaluation(
    ex: &impl SqlExecutor,
    input: &EvaluationInput,
    grade: &str,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let evaluation_no = model::next_evaluation_no(ex, now).await?;
    let supplier_name = supplier_snapshot(ex, input.supplier_id).await?;
    let id = Uuid::now_v7();
    ex.exec_sql(
        format!(
            "INSERT INTO {} \
                 (id, evaluation_no, supplier_id, supplier_name, period_start, period_end, eval_date, \
                  evaluator_id, evaluator_name, \
                  quality_weight, delivery_weight, price_weight, service_weight, \
                  quality_score, delivery_score, price_score, service_score, \
                  grade, remark, create_by, update_by) \
             VALUES (?::uuid, ?, ?::uuid, ?, ?::text::date, ?::text::date, ?::text::date, \
                     ?, ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                     ?::text::numeric, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                     ?, ?, ?::uuid, ?::uuid)",
            model::EVALUATION_TABLE
        ),
        vec![
            Value::from(&id),
            Value::Text(evaluation_no),
            Value::from(&input.supplier_id),
            Value::Text(supplier_name),
            Value::Text(model::date_text(input.period_start)),
            Value::Text(model::date_text(input.period_end)),
            Value::Text(model::date_text(input.eval_date)),
            optional_uuid(input.evaluator_id),
            optional_str(input.evaluator_name.as_deref()),
            Value::Text(model::decimal_text(input.weights.quality)),
            Value::Text(model::decimal_text(input.weights.delivery)),
            Value::Text(model::decimal_text(input.weights.price)),
            Value::Text(model::decimal_text(input.weights.service)),
            Value::Text(model::decimal_text(input.scores.quality)),
            Value::Text(model::decimal_text(input.scores.delivery)),
            Value::Text(model::decimal_text(input.scores.price)),
            Value::Text(model::decimal_text(input.scores.service)),
            Value::Text(grade.to_string()),
            optional_str(input.remark.as_deref()),
            optional_uuid(input.operator_id),
            optional_uuid(input.operator_id),
        ],
    )
    .await?;
    Ok(id)
}

/// 覆盖已有评估（重打分：单号不变，分数 / 权重 / 评估日 / 评估人 / 备注 / 等级全部按入参）。
async fn update_evaluation(
    ex: &impl SqlExecutor,
    row: &EvaluationRow,
    input: &EvaluationInput,
    grade: &str,
) -> DbResult<()> {
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {} SET \
                     eval_date = ?::text::date, \
                     evaluator_id = ?, evaluator_name = ?, \
                     quality_weight = ?::text::numeric, delivery_weight = ?::text::numeric, \
                     price_weight = ?::text::numeric, service_weight = ?::text::numeric, \
                     quality_score = ?::text::numeric, delivery_score = ?::text::numeric, \
                     price_score = ?::text::numeric, service_score = ?::text::numeric, \
                     grade = ?, remark = ?, update_by = ?::uuid, updated_at = now() \
                   WHERE id = ?::uuid AND deleted_at IS NULL",
                model::EVALUATION_TABLE
            ),
            vec![
                Value::Text(model::date_text(input.eval_date)),
                optional_uuid(input.evaluator_id),
                optional_str(input.evaluator_name.as_deref()),
                Value::Text(model::decimal_text(input.weights.quality)),
                Value::Text(model::decimal_text(input.weights.delivery)),
                Value::Text(model::decimal_text(input.weights.price)),
                Value::Text(model::decimal_text(input.weights.service)),
                Value::Text(model::decimal_text(input.scores.quality)),
                Value::Text(model::decimal_text(input.scores.delivery)),
                Value::Text(model::decimal_text(input.scores.price)),
                Value::Text(model::decimal_text(input.scores.service)),
                Value::Text(grade.to_string()),
                optional_str(input.remark.as_deref()),
                optional_uuid(input.operator_id),
                Value::from(&row.id),
            ],
        )
        .await?;
    if affected == 0 {
        return Err(DbError::InvalidValue(format!(
            "评估 {} 在更新期间被删除（影响 0 行），请重试",
            row.evaluation_no
        )));
    }
    Ok(())
}

/// 打分并落库：**同一供应商同一期间只有一份评估**，没有就新建、有就重打分覆盖。
///
/// 落库后立即读回并**对拍**两件事：
///
/// 1. 读回的 `total_score`（库层生成列）必须等于 Rust 侧 [`weighted_total`] 的试算值 ——
///    两条公式一旦漂移，这里立刻报错，而不是让「总分」和「等级」长期不一致；
/// 2. 读回的 `grade` 必须等于 [`grade_for_total`]（读回总分）—— 等级与阈值同源。
///
/// 不给 `total_score` 一个写入口（它本来就是生成列，写不进）；调用方只需要传四维分数 + 权重。
pub async fn save_evaluation(
    ex: &impl SqlExecutor,
    input: &EvaluationInput,
    now: DateTime<Utc>,
) -> DbResult<SaveOutcome> {
    validate_input(input)?;
    let (grade, rust_total) = grade_and_total(&input.weights, &input.scores)?;

    let existing = model::evaluation_of_period(ex, input.supplier_id, input.period_start, input.period_end).await?;
    let (id, created) = match existing {
        Some(row) => {
            update_evaluation(ex, &row, input, grade).await?;
            (row.id, false)
        }
        None => match insert_evaluation(ex, input, grade, now).await {
            Ok(id) => (id, true),
            // 并发建同一份：另一边先插进去了（唯一键 uk_biz_sev_supplier_period 拦下）。
            // 重查一遍退化成覆盖 —— 用户的意图（这份期间的结果是我这些分数）是相同的。
            Err(DbError::UniqueViolation { .. }) => {
                match model::evaluation_of_period(ex, input.supplier_id, input.period_start, input.period_end).await? {
                    Some(row) => {
                        update_evaluation(ex, &row, input, grade).await?;
                        (row.id, false)
                    }
                    None => {
                        return Err(DbError::InvalidValue(
                            "并发写入同一供应商同一期间的评估失败（唯一键冲突且重查不到那一行），请重试".to_string(),
                        ));
                    }
                }
            }
            Err(other) => return Err(other),
        },
    };

    let row = model::find_evaluation(ex, id)
        .await?
        .ok_or_else(|| DbError::InvalidValue(format!("评估 {id} 落库后读不回来（同事务内被删？）")))?;
    // ① 两条公式对拍（库层生成列 vs Rust 试算）
    if row.total_score != rust_total {
        return Err(DbError::InvalidValue(format!(
            "加权总分漂移：库层生成列 {} vs Rust 试算 {rust_total}（生成列表达式与 weighted_total 不一致）",
            row.total_score
        )));
    }
    // ② 等级与阈值同源
    let expected_grade = grade_for_total(row.total_score)?;
    if row.grade.as_deref() != Some(expected_grade) {
        return Err(DbError::InvalidValue(format!(
            "等级漂移：落库 {} vs 按总分 {} 应为 {expected_grade}",
            row.grade.unwrap_or_else(|| "NULL".to_string()),
            row.total_score
        )));
    }
    Ok(SaveOutcome { row, created })
}

// ==================== 按总分重评级（阈值变更后回算） ====================

/// 按当前阈值重算并回写等级，返回**实际改动**的行数（幂等：再跑一次应为 0）。
///
/// * `scope = None` → 全表存活评估；
/// * `scope = Some((start, end))` → 期间**完全落在** `[start, end]` 内的评估
///   （`period_start >= start AND period_end <= end`，即「本季度/本年度那些评估单」）。
///
/// 只改 `grade` 与 `updated_at`，不碰分数、不动单号 —— 分数是评估人的事实，等级是政策的函数。
pub async fn regrade_evaluations(ex: &impl SqlExecutor, scope: Option<(NaiveDate, NaiveDate)>) -> DbResult<u64> {
    let case = grade_case_sql("total_score");
    // 参数顺序必须与 SQL 里 `?` 出现的顺序一致：过滤条件在前（`CASE` 里没有占位符）。
    let (filter, params) = match scope {
        None => (String::new(), Vec::new()),
        Some((start, end)) => (
            " AND period_start >= ?::text::date AND period_end <= ?::text::date".to_string(),
            vec![Value::Text(model::date_text(start)), Value::Text(model::date_text(end))],
        ),
    };
    let sql = format!(
        "UPDATE {} SET grade = {case}, updated_at = now() \
          WHERE deleted_at IS NULL{filter} AND grade IS DISTINCT FROM ({case})",
        model::EVALUATION_TABLE
    );
    ex.exec_sql(sql, params).await
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn scores(all: &str) -> DimensionScores {
        DimensionScores {
            quality: dec(all),
            delivery: dec(all),
            price: dec(all),
            service: dec(all),
        }
    }

    #[test]
    fn grade_thresholds_are_descending_and_end_with_zero() {
        let mut previous: Option<Decimal> = None;
        for (grade, min_total) in GRADE_THRESHOLDS {
            assert!(model::GRADES.contains(&grade), "等级 {grade} 必须在值域内");
            let min = threshold_dec(min_total);
            if let Some(prev) = previous {
                assert!(min < prev, "阈值必须严格递减（{grade} = {min}）");
            }
            previous = Some(min);
        }
        assert_eq!(
            threshold_dec(GRADE_THRESHOLDS.last().expect("非空").1),
            Decimal::ZERO,
            "最后一项必须由 0 兜底"
        );
    }

    #[test]
    fn grade_boundaries_are_inclusive_on_the_lower_bound() {
        // 分界线就是分界线：90.00 是 A，89.99 是 B；70.00 是 C，69.99 是 D
        assert_eq!(grade_for_total(dec("100")).unwrap(), model::GRADE_A);
        assert_eq!(grade_for_total(dec("90.00")).unwrap(), model::GRADE_A);
        assert_eq!(grade_for_total(dec("89.99")).unwrap(), model::GRADE_B);
        assert_eq!(grade_for_total(dec("80.00")).unwrap(), model::GRADE_B);
        assert_eq!(grade_for_total(dec("79.99")).unwrap(), model::GRADE_C);
        assert_eq!(grade_for_total(dec("70.00")).unwrap(), model::GRADE_C);
        assert_eq!(grade_for_total(dec("69.99")).unwrap(), model::GRADE_D);
        assert_eq!(grade_for_total(dec("0.00")).unwrap(), model::GRADE_D);
    }

    #[test]
    fn grade_rejects_out_of_range_totals() {
        assert!(grade_for_total(dec("-0.01")).is_err(), "负总分是算错了，不是 D");
        assert!(grade_for_total(dec("100.01")).is_err(), "超过 100 分是算错了");
    }

    #[test]
    fn grade_and_total_uses_weighted_total_and_validates_inputs() {
        let weights = Weights {
            quality: dec("0.4"),
            delivery: dec("0.3"),
            price: dec("0.2"),
            service: dec("0.1"),
        };
        // 32 + 27 + 14 + 6 = 79.00 → C
        let four = DimensionScores {
            quality: dec("80"),
            delivery: dec("90"),
            price: dec("70"),
            service: dec("60"),
        };
        let (grade, total) = grade_and_total(&weights, &four).expect("合法入参");
        assert_eq!(total, dec("79.00"));
        assert_eq!(grade, model::GRADE_C);
        // 四等分 + 全 90 → 90.00 → A（正好压线）
        let (grade, total) = grade_and_total(&Weights::equal(), &scores("90")).expect("合法入参");
        assert_eq!(total, dec("90.00"));
        assert_eq!(grade, model::GRADE_A);
        // 权重和 ≠ 1 → 拒
        let bad_weights = Weights {
            quality: dec("0.4"),
            delivery: dec("0.3"),
            price: dec("0.2"),
            service: dec("0.2"),
        };
        assert!(grade_and_total(&bad_weights, &scores("90")).is_err());
        // 分数越界 → 拒
        assert!(grade_and_total(&Weights::equal(), &scores("100.01")).is_err());
        assert!(grade_and_total(&Weights::equal(), &scores("-1")).is_err());
    }

    #[test]
    fn grade_case_sql_mirrors_the_threshold_table() {
        let sql = grade_case_sql("total_score");
        assert_eq!(
            sql,
            "CASE WHEN total_score IS NULL THEN NULL \
             WHEN total_score >= 90 THEN 'A' \
             WHEN total_score >= 80 THEN 'B' \
             WHEN total_score >= 70 THEN 'C' \
             WHEN total_score >= 0 THEN 'D' \
             ELSE 'D' END",
            "SQL CASE 必须逐字来自 GRADE_THRESHOLDS（阈值不许有第二份）"
        );
        // 换列名也照样成立（regrade 之外复用时不出错）
        assert!(grade_case_sql("e.total_score").contains("WHEN e.total_score >= 90 THEN 'A'"));
    }

    #[test]
    fn delivery_stat_scores_only_when_there_is_data() {
        let empty = DeliveryStat::empty();
        assert!(!empty.is_usable());
        assert_eq!(empty.on_time_rate(), None, "没有到货数据 = 不能打分，不是 0 分");
        assert_eq!(empty.suggested_score(), None);

        let full = DeliveryStat {
            docs: 2,
            late_docs: 0,
            total_qty: dec("100"),
            on_time_qty: dec("100"),
            late_qty: Decimal::ZERO,
        };
        assert_eq!(full.suggested_score(), Some(dec("100.00")));

        let partial = DeliveryStat {
            docs: 3,
            late_docs: 1,
            total_qty: dec("120"),
            on_time_qty: dec("80"),
            late_qty: dec("40"),
        };
        // 80 / 120 = 66.666… → 66.67（与总分同一套 2 位舍入）
        assert_eq!(partial.suggested_score(), Some(dec("66.67")));

        let all_late = DeliveryStat {
            docs: 1,
            late_docs: 1,
            total_qty: dec("10"),
            on_time_qty: Decimal::ZERO,
            late_qty: dec("10"),
        };
        assert_eq!(all_late.suggested_score(), Some(dec("0.00")), "全部逾期才是 0 分");
    }
}

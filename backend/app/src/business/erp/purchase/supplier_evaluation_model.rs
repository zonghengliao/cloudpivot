//! 供应商评估数据层（M4-12 表结构）。
//!
//! 表结构唯一依据：`backend/app/migrations/business/V0454__supplier_evaluation.up.sql`。
//! 评估打分编排（读订单/收货算分）、评级阈值（A/B/C/D 分界）、准入与黑名单流程、CRUD 接口
//! 分别属 M4-13 / M4-14 —— 本文件**只有表结构配套的常量 / 行结构 / 取号 / 读入口 / 权重算术**，
//! 不含校验编排与路由。
//!
//! | 表 | 行数级 | 关键不变量 |
//! |----|--------|------------|
//! | `biz_supplier_evaluation` | 每家供应商每期间一行 | 单号存活唯一（`SEV-yyyyMMdd-000001`）；`period_start <= period_end`；`eval_date >= period_start`；四维权重各 0–1 且**之和精确 = 1**；四维分数各 0–100；`total_score` 是**生成列**（写不进）；`grade` 为 NULL(未评级) 或 A/B/C/D；`(supplier_id, period_start, period_end)` 存活唯一 |
//!
//! # 三条口径（写在这里，免得下游各写一套）
//!
//! ① **总分是库算的**：`total_score = round(Σ 权重 × 分数, 2)`，在 DDL 里是
//!    `GENERATED ALWAYS AS … STORED`。任何写入路径显式写它都会被 PG 拒（428C9）。
//!    [`weighted_total`] 是**同一公式的 Rust 版**（建单前试算 / 前端预览用），
//!    真库用例逐位对拍生成列 —— 两边不许出现第二种舍入口径。
//! ② **权重之和恒为 1**：[`Weights::is_valid`] 与库层 `ck_biz_sev_weights` 同一份判据。
//!    四等分 [`Weights::equal`] 是库层默认值。想改「相对权重按比例归一」是政策变更，
//!    要**追加**迁移放宽 CHECK，不在代码里偷偷归一。
//! ③ **等级是政策，不是算术**：[`GRADES`] 只钉值域（A/B/C/D，`None` = 未评级）。
//!    分数 → 等级的分界属 M4-13，本文件**故意不写**阈值（写在这里就成了第二份真相）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_supplier_evaluation 与采购计划 / 框架协议同口径 —— 供应商评估是
//!         公司级采购文件（采购要看全公司、财务要复核），本表也没有 dept 归属列，
//!         所以不登记 shared::scope::SCOPED_TABLES。可见性由 M4-16 的权限码控制。
//! ```
//!
//! # 为什么不建明细表
//!
//! 卡面的「四维」就是四列分数 + 四列权重。细分指标（来料合格率 / 准时交付率）的
//! 分子分母与抽样口径属于 M4-13 的业务设计，现在建一张没有写入方的空表就是死结构；
//! 真要拆时**追加**迁移建 `biz_supplier_evaluation_item`，本表四列降级为汇总缓存。

use nik::prelude::chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;
use nik::uuid::Uuid;

// ==================== 表名 / 取号 / 枚举值 ====================

/// 供应商评估表。
pub const EVALUATION_TABLE: &str = "biz_supplier_evaluation";
/// 取号键（`biz_seq.seq_type`）。
pub const SEQ_TYPE: &str = "SEV";
/// 评估单号前缀。
pub const NO_PREFIX: &str = "SEV";
/// 流水号宽度。
pub const NO_SEQ_WIDTH: usize = 6;

/// 四维（与库层四列一一对应；M4-13 的算分循环 / 前端列序都按这个顺序）。
pub const DIMENSIONS: &[&str] = &["QUALITY", "DELIVERY", "PRICE", "SERVICE"];
/// 维：质量。
pub const DIM_QUALITY: &str = "QUALITY";
/// 维：交期。
pub const DIM_DELIVERY: &str = "DELIVERY";
/// 维：价格。
pub const DIM_PRICE: &str = "PRICE";
/// 维：服务。
pub const DIM_SERVICE: &str = "SERVICE";

/// 等级：A。
pub const GRADE_A: &str = "A";
/// 等级：B。
pub const GRADE_B: &str = "B";
/// 等级：C。
pub const GRADE_C: &str = "C";
/// 等级：D。
pub const GRADE_D: &str = "D";
/// 等级值域（与库层 `ck_biz_sev_grade` 同一份；`None` = 未评级，**不是**第 5 个等级）。
/// 分界阈值属 M4-13，这里只钉值域。
pub const GRADES: &[&str] = &[GRADE_A, GRADE_B, GRADE_C, GRADE_D];

/// 分数上限（百分制）。
pub const SCORE_MAX: i32 = 100;

/// 总分舍入位数（与库层生成列的 `round(…, 2)` 同一个 2）。
pub const TOTAL_SCALE: u32 = 2;

/// 与库层 `round(numeric, 2)` 一致的舍入策略：四舍五入（中点远离零）。
pub const TOTAL_ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 权重 / 分数（四维算术） ====================

/// 四维权重（0–1，之和必须 = 1）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    /// 质量权重。
    pub quality: Decimal,
    /// 交期权重。
    pub delivery: Decimal,
    /// 价格权重。
    pub price: Decimal,
    /// 服务权重。
    pub service: Decimal,
}

impl Weights {
    /// 四等分（0.25 × 4）—— 与库层列默认值逐位相同。
    pub fn equal() -> Self {
        let quarter = Decimal::new(25, 2);
        Self {
            quality: quarter,
            delivery: quarter,
            price: quarter,
            service: quarter,
        }
    }

    /// 之和（精确 `Decimal` 加法，不是浮点）。
    pub fn sum(&self) -> Decimal {
        self.quality + self.delivery + self.price + self.service
    }

    /// 是否合法：四项各在 0–1 且**之和精确 = 1**（库层 `ck_biz_sev_weights` 的同一份判据）。
    ///
    /// 「差不多等于 1」不算合法 —— 权重口径只允许一种：精确 1。
    pub fn is_valid(&self) -> bool {
        let in_range = |value: Decimal| value >= Decimal::ZERO && value <= Decimal::ONE;
        in_range(self.quality)
            && in_range(self.delivery)
            && in_range(self.price)
            && in_range(self.service)
            && self.sum() == Decimal::ONE
    }
}

/// 四维得分（0–100，百分制）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DimensionScores {
    /// 质量得分。
    pub quality: Decimal,
    /// 交期得分。
    pub delivery: Decimal,
    /// 价格得分。
    pub price: Decimal,
    /// 服务得分。
    pub service: Decimal,
}

impl DimensionScores {
    /// 四项是否都在 0–100（库层 `ck_biz_sev_scores` 的同一份判据）。
    pub fn is_valid(&self) -> bool {
        let max = Decimal::from(SCORE_MAX);
        let in_range = |value: Decimal| value >= Decimal::ZERO && value <= max;
        in_range(self.quality) && in_range(self.delivery) && in_range(self.price) && in_range(self.service)
    }
}

/// 加权总分：`round(Σ 权重 × 分数, 2)`。
///
/// **与库层生成列逐位对齐**（真库用例 `model_weighted_total_matches_db_generated_column` 对拍，
/// 含 `.005` 中点）：两边任一处改了公式或舍入，用例立刻红。
/// 建单前试算、M4-13 的算分编排、前端预览都用它，不要再写第二份。
pub fn weighted_total(weights: &Weights, scores: &DimensionScores) -> Decimal {
    let raw = weights.quality * scores.quality
        + weights.delivery * scores.delivery
        + weights.price * scores.price
        + weights.service * scores.service;
    raw.round_dp_with_strategy(TOTAL_SCALE, TOTAL_ROUNDING)
}

// ==================== 行结构 ====================

/// 评估单行（列表 / 详情用；供应商现名是实时 join 的派生列）。
#[derive(Debug, Clone)]
pub struct EvaluationRow {
    /// 主键。
    pub id: Uuid,
    /// 评估单号（`SEV-yyyyMMdd-000001`）。
    pub evaluation_no: String,
    /// 供应商。
    pub supplier_id: Uuid,
    /// 供应商名**快照**（评估结论的历史事实，约定 13）。
    pub supplier_name: Option<String>,
    /// 供应商**现名**（实时 join；快照为空时也回退到它）。
    pub supplier_display_name: String,
    /// 评估期间起（含）。
    pub period_start: NaiveDate,
    /// 评估期间止（含）。
    pub period_end: NaiveDate,
    /// 评估日。
    pub eval_date: NaiveDate,
    /// 评估人。
    pub evaluator_id: Option<Uuid>,
    /// 评估人姓名快照。
    pub evaluator_name: Option<String>,
    /// 质量权重。
    pub quality_weight: Decimal,
    /// 交期权重。
    pub delivery_weight: Decimal,
    /// 价格权重。
    pub price_weight: Decimal,
    /// 服务权重。
    pub service_weight: Decimal,
    /// 质量得分。
    pub quality_score: Decimal,
    /// 交期得分。
    pub delivery_score: Decimal,
    /// 价格得分。
    pub price_score: Decimal,
    /// 服务得分。
    pub service_score: Decimal,
    /// 加权总分（生成列；只读）。
    pub total_score: Decimal,
    /// 等级（`None` = 未评级）。
    pub grade: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
}

impl EvaluationRow {
    /// 四维权重（便于复用 [`Weights`] 的判据 / 算分）。
    pub fn weights(&self) -> Weights {
        Weights {
            quality: self.quality_weight,
            delivery: self.delivery_weight,
            price: self.price_weight,
            service: self.service_weight,
        }
    }

    /// 四维得分。
    pub fn scores(&self) -> DimensionScores {
        DimensionScores {
            quality: self.quality_score,
            delivery: self.delivery_score,
            price: self.price_score,
            service: self.service_score,
        }
    }

    /// 是否已评级（`grade IS NULL` = 未评级，不是「E 级」）。
    pub fn is_graded(&self) -> bool {
        self.grade.is_some()
    }

    /// 用本行的权重与分数重算加权总分 —— 只用于对账（库里那一列是生成列，写不进）。
    pub fn recomputed_total(&self) -> Decimal {
        weighted_total(&self.weights(), &self.scores())
    }
}

// ==================== 纯逻辑 ====================

/// 评估单号拼装：`SEV-{周期键}-{流水 6 位}`。
pub fn format_evaluation_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0width$}", width = NO_SEQ_WIDTH)
}

/// 取号周期键（北京时间 `yyyyMMdd`，与其它 ERP 单据同口径：按 CST 天滚动，不是 UTC 天）。
pub fn period_key_of(now: DateTime<Utc>) -> String {
    (now + Duration::hours(8)).format("%Y%m%d").to_string()
}

/// 评估日的北京时间（`eval_date` 默认值在库里是 `CURRENT_DATE`，服务端写入口要用同一口径）。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    (now + Duration::hours(8)).date_naive()
}

/// 评估期间是否合法：起 ≤ 止（库层 `ck_biz_sev_period` 的同一份判据）。
pub fn period_is_valid(start: NaiveDate, end: NaiveDate) -> bool {
    start <= end
}

/// 评估日是否合法：不早于期间起点（库层 `ck_biz_sev_eval_date` 的同一份判据）。
pub fn eval_date_is_valid(period_start: NaiveDate, eval_date: NaiveDate) -> bool {
    eval_date >= period_start
}

/// 等级是否在值域内（`None` = 未评级，合法）。
pub fn grade_is_valid(grade: Option<&str>) -> bool {
    match grade {
        None => true,
        Some(value) => GRADES.contains(&value),
    }
}

/// 期间的自然语言标签（`2026-01-01..2026-03-31` → `2026-Q1`），只用于展示。
///
/// 认三种口径：整月（1 日 → 月末且同月）、整季（1/4/7/10 月 1 日 → 该季末月月末）、
/// 整年（1 月 1 日 → 12 月 31 日）；其它起止日原样 `YYYY-MM-DD~YYYY-MM-DD`。
/// 为什么认整季：卡面没限定评估周期，季度是最常用的一档（见 V0454 头注释 4）。
pub fn period_label(start: NaiveDate, end: NaiveDate) -> String {
    let year = start.year();
    let month = start.month();
    let end_month_last = days_in_month(end.year(), end.month());
    let is_month_end = end.day() == end_month_last;
    let full_start = start.day() == 1;
    if full_start && is_month_end {
        if month == 1 && end.month() == 12 {
            return format!("{year}-FY");
        }
        if end.month() == month {
            return format!("{year}-{month:02}");
        }
        if end.month() == month + 2 && matches!(month, 1 | 4 | 7 | 10) {
            return format!("{year}-Q{}", (month - 1) / 3 + 1);
        }
    }
    format!("{}~{}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
}

/// 某年某月的天数（闰年 2 月由 `NaiveDate` 自己算，不手写 28/29）。
fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let first_of_next = NaiveDate::from_ymd_opt(next_year, next_month, 1).expect("下月 1 日合法");
    first_of_next.pred_opt().expect("上月最后一天").day()
}

// ==================== 取号 ====================

/// `biz_seq(seq_type='SEV')` 缺失时幂等补一行（迁移已种，这里是自愈兜底）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'SEV', '供应商评估单号', 'SEV', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '供应商评估单号：SEV-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SEV' AND deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await
    .or_else(|error| match error {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='SEV')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'SEV' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个评估单号（周期键按北京时间）。返回形如 `SEV-20261118-000001`。
pub async fn next_evaluation_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_evaluation_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='SEV') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 读 ====================

/// 单头 select（带供应商现名）。
fn evaluation_select_sql() -> String {
    format!(
        "SELECT e.id, e.evaluation_no, e.supplier_id, e.supplier_name, \
                COALESCE(e.supplier_name, s.supplier_name, '') AS supplier_display_name, \
                e.period_start, e.period_end, e.eval_date, e.evaluator_id, e.evaluator_name, \
                e.quality_weight, e.delivery_weight, e.price_weight, e.service_weight, \
                e.quality_score, e.delivery_score, e.price_score, e.service_score, \
                e.total_score, e.grade, e.remark, e.created_at, e.create_by \
           FROM {EVALUATION_TABLE} e \
           LEFT JOIN biz_supplier s ON s.id = e.supplier_id AND s.deleted_at IS NULL \
          WHERE e.deleted_at IS NULL"
    )
}

fn read_evaluation_row(row: &Row) -> DbResult<EvaluationRow> {
    Ok(EvaluationRow {
        id: row.get_typed("id")?,
        evaluation_no: row.get_typed("evaluation_no")?,
        supplier_id: row.get_typed("supplier_id")?,
        supplier_name: row.get_typed("supplier_name")?,
        supplier_display_name: row.get_typed("supplier_display_name")?,
        period_start: row.get_typed("period_start")?,
        period_end: row.get_typed("period_end")?,
        eval_date: row.get_typed("eval_date")?,
        evaluator_id: row.get_typed("evaluator_id")?,
        evaluator_name: row.get_typed("evaluator_name")?,
        quality_weight: row.get_typed("quality_weight")?,
        delivery_weight: row.get_typed("delivery_weight")?,
        price_weight: row.get_typed("price_weight")?,
        service_weight: row.get_typed("service_weight")?,
        quality_score: row.get_typed("quality_score")?,
        delivery_score: row.get_typed("delivery_score")?,
        price_score: row.get_typed("price_score")?,
        service_score: row.get_typed("service_score")?,
        total_score: row.get_typed("total_score")?,
        grade: row.get_typed("grade")?,
        remark: row.get_typed("remark")?,
        created_at: row.get_typed("created_at")?,
        create_by: row.get_typed("create_by")?,
    })
}

/// 按主键取一份存活评估（不存在 / 已软删 → `None`）。
pub async fn find_evaluation(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<EvaluationRow>> {
    // SCOPE-EXEMPT: 按主键取单行，不涉及数据范围（见模块头「数据权限」）。
    let rows = ex
        .query_sql(
            format!("{} AND e.id = ?::uuid", evaluation_select_sql()),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(read_evaluation_row).transpose()
}

/// 按单号取一份存活评估（`uk_biz_sev_no` 的唯一键探针 —— 建单前判重比撞库报 500 友好）。
pub async fn find_evaluation_by_no(ex: &impl SqlExecutor, evaluation_no: &str) -> DbResult<Option<EvaluationRow>> {
    // SCOPE-EXEMPT: 唯一键探针，不涉及数据范围。
    let rows = ex
        .query_sql(
            format!("{} AND e.evaluation_no = ?", evaluation_select_sql()),
            vec![Value::Text(evaluation_no.to_string())],
        )
        .await?;
    rows.first().map(read_evaluation_row).transpose()
}

/// 取「某供应商某期间」的那一份评估（`uk_biz_sev_supplier_period` 的唯一键探针）。
///
/// M4-13 算分前先调它：已有就改（重算），没有就新建 —— 一份期间只留一个结论。
pub async fn evaluation_of_period(
    ex: &impl SqlExecutor,
    supplier_id: Uuid,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> DbResult<Option<EvaluationRow>> {
    // SCOPE-EXEMPT: 唯一键探针，不涉及数据范围。
    let rows = ex
        .query_sql(
            format!(
                "{} AND e.supplier_id = ?::uuid \
                    AND e.period_start = ?::text::date AND e.period_end = ?::text::date",
                evaluation_select_sql()
            ),
            vec![
                Value::from(&supplier_id),
                Value::Text(date_text(period_start)),
                Value::Text(date_text(period_end)),
            ],
        )
        .await?;
    rows.first().map(read_evaluation_row).transpose()
}

/// 一家供应商的全部评估（期间倒序，即最新在前）。
pub async fn evaluations_of_supplier(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<Vec<EvaluationRow>> {
    // SCOPE-EXEMPT: 明细随单头可见性走（见模块头「数据权限」）。
    let rows = ex
        .query_sql(
            format!(
                "{} AND e.supplier_id = ?::uuid ORDER BY e.period_end DESC, e.eval_date DESC",
                evaluation_select_sql()
            ),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    rows.iter().map(read_evaluation_row).collect()
}

/// 某供应商**最近一期**评估（M4-14 准入 / 黑名单、下单优先级都按它判）。
pub async fn latest_evaluation(ex: &impl SqlExecutor, supplier_id: Uuid) -> DbResult<Option<EvaluationRow>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            format!(
                "{} AND e.supplier_id = ?::uuid ORDER BY e.period_end DESC, e.eval_date DESC LIMIT 1",
                evaluation_select_sql()
            ),
            vec![Value::from(&supplier_id)],
        )
        .await?;
    rows.first().map(read_evaluation_row).transpose()
}

/// 某期间内已评级的评估（`grade IS NOT NULL`，按总分降序）—— 「本期 A 级供应商」清单。
pub async fn graded_in_period(
    ex: &impl SqlExecutor,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> DbResult<Vec<EvaluationRow>> {
    // SCOPE-EXEMPT: 同上。
    let rows = ex
        .query_sql(
            format!(
                "{} AND e.period_start = ?::text::date AND e.period_end = ?::text::date \
                    AND e.grade IS NOT NULL ORDER BY e.total_score DESC, e.supplier_id",
                evaluation_select_sql()
            ),
            vec![Value::Text(date_text(period_start)), Value::Text(date_text(period_end))],
        )
        .await?;
    rows.iter().map(read_evaluation_row).collect()
}

/// 可选文本（`None` / 空串都折成 NULL）。
pub fn optional_text(value: Option<&str>) -> Value {
    Value::Text(value.unwrap_or_default().to_string())
}

/// 日期字面量 `YYYY-MM-DD`（配 `?::text::date` 用：裸 `?::date` 会让 PG 把参数推断成 date，
/// `Value::Text` 编码不进去 —— 与 V0450 / commission_rule_model 同一条教训）。
pub fn date_text(value: NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

/// 定点数字字面量（配 `?::text::numeric` 用，避免浮点编码）。
pub fn decimal_text(value: Decimal) -> String {
    value.to_string()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("定点字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn cst(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("rfc3339 时刻字面量")
            .with_timezone(&Utc)
    }

    #[test]
    fn no_format_is_padded_to_six_digits() {
        assert_eq!(format_evaluation_no("20261118", 1), "SEV-20261118-000001");
        assert_eq!(format_evaluation_no("20261118", 123456), "SEV-20261118-123456");
        // 号池用尽会给出 7 位（库层 ck_biz_sev_no_fmt 会拦，这里只验证拼装不截断）
        assert_eq!(format_evaluation_no("20261118", 1000000), "SEV-20261118-1000000");
    }

    #[test]
    fn period_key_rolls_on_beijing_midnight() {
        // UTC 15:59:59 = 北京 23:59:59 → 还是 18 号
        assert_eq!(period_key_of(cst("2026-11-18T15:59:59Z")), "20261118");
        // UTC 16:00:00 = 北京次日 00:00 → 19 号（UTC 天口径会停在前一天，这就是为什么按 CST）
        assert_eq!(period_key_of(cst("2026-11-18T16:00:00Z")), "20261119");
        assert_eq!(today_cst(cst("2026-11-18T16:00:00Z")), date("2026-11-19"));
    }

    #[test]
    fn equal_weights_are_valid_and_sum_to_one() {
        let weights = Weights::equal();
        assert_eq!(weights.quality, dec("0.25"));
        assert_eq!(weights.sum(), Decimal::ONE);
        assert!(weights.is_valid());
    }

    #[test]
    fn weight_sum_must_be_exactly_one() {
        let mut weights = Weights {
            quality: dec("0.4"),
            delivery: dec("0.3"),
            price: dec("0.2"),
            service: dec("0.1"),
        };
        assert!(weights.is_valid());
        // 和 1.1 → 不合法（库层 ck_biz_sev_weights 同样拒）
        weights.service = dec("0.2");
        assert!(!weights.is_valid());
        // 和 0.9 → 同样不合法（「差不多是 1」不算合法）
        weights.service = Decimal::ZERO;
        assert!(!weights.is_valid());
        // 单项越界（负数）即使和凑成 1 也不合法
        let skewed = Weights {
            quality: dec("1.5"),
            delivery: dec("-0.5"),
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        };
        assert_eq!(skewed.sum(), Decimal::ONE, "和确实是 1，但单项越界");
        assert!(!skewed.is_valid());
    }

    #[test]
    fn scores_are_0_to_100_inclusive() {
        let ok = DimensionScores {
            quality: dec("0"),
            delivery: dec("100"),
            price: dec("59.99"),
            service: dec("60.00"),
        };
        assert!(ok.is_valid());
        let over = DimensionScores {
            quality: dec("100.01"),
            ..ok
        };
        assert!(!over.is_valid());
        let negative = DimensionScores {
            service: dec("-0.01"),
            ..ok
        };
        assert!(!negative.is_valid());
    }

    #[test]
    fn weighted_total_matches_hand_arithmetic() {
        let weights = Weights {
            quality: dec("0.4"),
            delivery: dec("0.3"),
            price: dec("0.2"),
            service: dec("0.1"),
        };
        let scores = DimensionScores {
            quality: dec("80"),
            delivery: dec("90"),
            price: dec("70"),
            service: dec("60"),
        };
        // 32 + 27 + 14 + 6 = 79
        assert_eq!(weighted_total(&weights, &scores), dec("79.00"));
        // 四等分 + 全 100 分 → 100.00
        let all_max = DimensionScores {
            quality: dec("100"),
            delivery: dec("100"),
            price: dec("100"),
            service: dec("100"),
        };
        assert_eq!(weighted_total(&Weights::equal(), &all_max), dec("100.00"));
        // 全 0 分 → 0.00
        let all_zero = DimensionScores {
            quality: Decimal::ZERO,
            delivery: Decimal::ZERO,
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        };
        assert_eq!(weighted_total(&Weights::equal(), &all_zero), dec("0.00"));
    }

    #[test]
    fn weighted_total_rounds_midpoint_away_from_zero_like_pg() {
        // 0.5 × 0.01 = 0.005 → PG round(numeric,2) = 0.01（中点远离零）
        let weights = Weights {
            quality: dec("0.5"),
            delivery: dec("0.5"),
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        };
        let scores = DimensionScores {
            quality: dec("0.01"),
            delivery: Decimal::ZERO,
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        };
        assert_eq!(weighted_total(&weights, &scores), dec("0.01"));
        // 0.25 × 99.99 = 24.9975 → 25.00
        let quarter = Weights::equal();
        let scores = DimensionScores {
            quality: dec("99.99"),
            delivery: Decimal::ZERO,
            price: Decimal::ZERO,
            service: Decimal::ZERO,
        };
        assert_eq!(weighted_total(&quarter, &scores), dec("25.00"));
    }

    #[test]
    fn grade_domain_keeps_unrated_as_none() {
        assert!(grade_is_valid(None), "NULL = 未评级，必须合法");
        for grade in GRADES {
            assert!(grade_is_valid(Some(*grade)));
        }
        assert!(!grade_is_valid(Some("E")));
        assert!(!grade_is_valid(Some("UNRATED")), "未评级只许 NULL，不造字符串");
        assert_eq!(GRADES.len(), 4);
    }

    #[test]
    fn period_and_eval_date_invariants() {
        assert!(period_is_valid(date("2026-01-01"), date("2026-03-31")));
        assert!(period_is_valid(date("2026-01-01"), date("2026-01-01")), "单日期间合法");
        assert!(!period_is_valid(date("2026-03-31"), date("2026-01-01")));
        // 评估日：期间内 / 期间后都合法，早于期间起点不合法
        assert!(eval_date_is_valid(date("2026-01-01"), date("2026-02-15")));
        assert!(eval_date_is_valid(date("2026-01-01"), date("2026-04-02")));
        assert!(!eval_date_is_valid(date("2026-01-01"), date("2025-12-31")));
    }

    #[test]
    fn period_label_recognizes_month_quarter_and_year() {
        assert_eq!(period_label(date("2026-01-01"), date("2026-01-31")), "2026-01");
        assert_eq!(period_label(date("2026-02-01"), date("2026-02-28")), "2026-02");
        assert_eq!(
            period_label(date("2024-02-01"), date("2024-02-29")),
            "2024-02",
            "闰年 2 月 29 天"
        );
        assert_eq!(period_label(date("2026-01-01"), date("2026-03-31")), "2026-Q1");
        assert_eq!(period_label(date("2026-04-01"), date("2026-06-30")), "2026-Q2");
        assert_eq!(period_label(date("2026-07-01"), date("2026-09-30")), "2026-Q3");
        assert_eq!(period_label(date("2026-10-01"), date("2026-12-31")), "2026-Q4");
        assert_eq!(period_label(date("2026-01-01"), date("2026-12-31")), "2026-FY");
        // 不是整月/整季/整年 → 原样起止（不猜）
        assert_eq!(
            period_label(date("2026-01-05"), date("2026-03-31")),
            "2026-01-05~2026-03-31"
        );
        assert_eq!(
            period_label(date("2026-01-01"), date("2026-03-30")),
            "2026-01-01~2026-03-30"
        );
        assert_eq!(
            period_label(date("2026-02-01"), date("2026-04-30")),
            "2026-02-01~2026-04-30"
        );
    }

    #[test]
    fn dimension_consts_are_unique_and_ordered() {
        assert_eq!(DIMENSIONS.len(), 4);
        let mut sorted = DIMENSIONS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "四维常量不许重名");
        assert_eq!(DIMENSIONS[0], DIM_QUALITY);
        assert_eq!(DIMENSIONS[1], DIM_DELIVERY);
        assert_eq!(DIMENSIONS[2], DIM_PRICE);
        assert_eq!(DIMENSIONS[3], DIM_SERVICE);
    }

    #[test]
    fn decimal_and_date_text_are_plain_literals() {
        assert_eq!(date_text(date("2026-03-31")), "2026-03-31");
        assert_eq!(decimal_text(dec("0.2500")), "0.2500");
        assert_eq!(decimal_text(dec("79.00")), "79.00");
    }
}

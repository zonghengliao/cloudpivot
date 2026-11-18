//! 供应商评估接口 DTO（M4-16 页面取数 / 录入 / 排名）。
//!
//! 数据层在 [`crate::business::erp::purchase::supplier_evaluation_model`]（表结构，M4-12）与
//! [`crate::business::erp::purchase::supplier_evaluation_grading`]（打分 + 评级，M4-13）；
//! 本文件只做**入参校验 + 出参整形**，不重写任何业务判据。
//!
//! # 与 `V0457` 的权限码一一对应
//!
//! | 码 | 承载接口 |
//! |----|----------|
//! | `business:erp:supplier-evaluation:list` | `GET /page`、`GET /rank`、`GET /{id}` |
//! | `business:erp:supplier-evaluation:save` | `POST /"`（打分录入 / 重打分覆盖）、`GET /delivery-suggestion` |
//!
//! 页面显形码 `business:erp:supplier-evaluation:page` **只给前端**（侧栏显隐 + 路由
//! `meta.permission`），本文件与全部后端源码零引用（AGENTS 约定 14；已在
//! `permission_audit_tests::ROUTE_LEVEL_ONLY` 登记）。
//!
//! # 校验口径（与库层 CHECK / M4-13 同一份判据，不在页面里再写一套）
//!
//! * 四维**权重**各 0–1 且之和精确 = 1（`ck_biz_sev_weights`，判据 [`model::Weights::is_valid`]）；
//! * 四维**分数**各 0–100（`ck_biz_sev_scores`）；
//! * `period_start <= period_end`（`ck_biz_sev_period`）、`eval_date >= period_start`（`ck_biz_sev_eval_date`）；
//! * 等级闭集 A/B/C/D（`None` = 未评级，**不是**第五个等级）；
//! * `total_score` 是库层**生成列**，任何入参都不接（写了会被 PG 428C9 拒）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::purchase::supplier_evaluation_model::{self as model, EvaluationRow, GRADES, Weights};
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（与 V0457 的 sys_menu.auth_code 逐字一致） ====================

/// 本模块全部接口码（`perm_menu_gate.py` 要求每个都在 `sys_menu` 有承载行）。
pub const EVALUATION_PERMISSIONS: [&str; 2] = [PERM_LIST, PERM_SAVE];
/// 列表 / 排名 / 详情。
pub const PERM_LIST: &str = "business:erp:supplier-evaluation:list";
/// 打分录入（含交期参考分）。
pub const PERM_SAVE: &str = "business:erp:supplier-evaluation:save";

/// 关键字长度上限（超了 400，页面先提示不截断）。
pub const MAX_KEYWORD: usize = 64;

// ==================== 400 / 404 小工具 ====================

/// 400 + 参数错。
pub fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 404 + 评估单不存在。
pub fn not_found(id: Uuid) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("供应商评估单 {id} 不存在或已删除"),
    )
}

// ==================== 解析小工具 ====================

/// 解析 uuid（非法值 → 400，点名是哪个字段）。
pub fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| bad_request(format!("{field}={raw} 不是合法的 uuid")))
}

/// 解析 `YYYY-MM-DD` 日期（非法值 → 400）。
pub fn parse_date(raw: &str, field: &str) -> std::result::Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| bad_request(format!("{field}={raw} 不是合法的日期（要 YYYY-MM-DD）")))
}

/// 解析定点数（非法值 / NaN / 空 → 400）。
pub fn parse_decimal(raw: &str, field: &str) -> std::result::Result<Decimal, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(bad_request(format!("{field} 不能为空")));
    }
    text.parse::<Decimal>()
        .map_err(|_| bad_request(format!("{field}={raw} 不是合法的数值")))
}

/// 选项文本归一（`None` / 空串 → `None`）。
fn clean(raw: Option<&str>) -> Option<String> {
    let text = raw.unwrap_or_default().trim();
    if text.is_empty() { None } else { Some(text.to_string()) }
}

// ==================== 分页查询 ====================

/// 分页查询参数（`GET /business/erp/supplier-evaluation/page`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EvaluationPageQuery {
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页条数（1–200）。
    pub page_size: Option<u64>,
    /// 关键字（单号 / 供应商名 / 备注）。
    pub keyword: Option<String>,
    /// 等级（A/B/C/D；空 = 全部，**没有**「未评级」选项 —— 未评级只出现在历史数据里）。
    pub grade: Option<String>,
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 评估期间起（含；与 `period_end` 成对使用）。
    pub period_start: Option<String>,
    /// 评估期间止（含）。
    pub period_end: Option<String>,
}

/// 已校验的分页筛选（model 只认这个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationFilter {
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 关键字（已 trim）。
    pub keyword: Option<String>,
    /// 等级。
    pub grade: Option<String>,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
    /// 期间起。
    pub period_start: Option<NaiveDate>,
    /// 期间止。
    pub period_end: Option<NaiveDate>,
}

impl EvaluationPageQuery {
    /// 校验并归一化（非法值 → 400，不静默忽略）。
    pub fn filter(&self) -> std::result::Result<EvaluationFilter, ApiError> {
        let page = self.page.unwrap_or(1).max(1);
        let page_size = self.page_size.unwrap_or(20).clamp(1, 200);
        let keyword = clean(self.keyword.as_deref());
        if let Some(word) = keyword.as_deref()
            && word.chars().count() > MAX_KEYWORD
        {
            return Err(bad_request(format!(
                "keyword 超过 {MAX_KEYWORD} 字符（当前 {}），请缩短后再查",
                word.chars().count()
            )));
        }
        let grade = match clean(self.grade.as_deref()) {
            None => None,
            Some(raw) => {
                let grade = raw.to_ascii_uppercase();
                if !GRADES.contains(&grade.as_str()) {
                    return Err(bad_request(format!(
                        "grade={grade} 不在等级闭集内（{}）",
                        GRADES.join(" / ")
                    )));
                }
                Some(grade)
            }
        };
        let supplier_id = match clean(self.supplier_id.as_deref()) {
            None => None,
            Some(raw) => Some(parse_uuid(&raw, "supplier_id")?),
        };
        let period_start = match clean(self.period_start.as_deref()) {
            None => None,
            Some(raw) => Some(parse_date(&raw, "period_start")?),
        };
        let period_end = match clean(self.period_end.as_deref()) {
            None => None,
            Some(raw) => Some(parse_date(&raw, "period_end")?),
        };
        if let (Some(start), Some(end)) = (period_start, period_end)
            && !model::period_is_valid(start, end)
        {
            return Err(bad_request(format!(
                "period_start {} 晚于 period_end {}",
                model::date_text(start),
                model::date_text(end)
            )));
        }
        Ok(EvaluationFilter {
            page,
            page_size,
            keyword,
            grade,
            supplier_id,
            period_start,
            period_end,
        })
    }
}

// ==================== 排名 ====================

/// 排名查询参数（`GET /business/erp/supplier-evaluation/rank`）。
///
/// 期间**必填**：排名是「同一期间同一口径下比分数」，不给默认期间是为了避免
/// 「忘了选期间却看到一屏看似合理的名次」。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RankQuery {
    /// 评估期间起（含）。
    pub period_start: Option<String>,
    /// 评估期间止（含）。
    pub period_end: Option<String>,
    /// 取前 N 名（默认 100，上限 500）。
    pub limit: Option<u64>,
}

impl RankQuery {
    /// 校验并归一化。
    pub fn range(&self) -> std::result::Result<(NaiveDate, NaiveDate, u64), ApiError> {
        let start = clean(self.period_start.as_deref())
            .ok_or_else(|| bad_request("排名必须指定 period_start（评估期间起）"))?;
        let end =
            clean(self.period_end.as_deref()).ok_or_else(|| bad_request("排名必须指定 period_end（评估期间止）"))?;
        let start = parse_date(&start, "period_start")?;
        let end = parse_date(&end, "period_end")?;
        if !model::period_is_valid(start, end) {
            return Err(bad_request(format!(
                "period_start {} 晚于 period_end {}",
                model::date_text(start),
                model::date_text(end)
            )));
        }
        Ok((start, end, self.limit.unwrap_or(100).clamp(1, 500)))
    }
}

// ==================== 交期参考分 ====================

/// 交期参考分查询参数（`GET /business/erp/supplier-evaluation/delivery-suggestion`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SuggestionQuery {
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 评估期间起（含）。
    pub period_start: Option<String>,
    /// 评估期间止（含）。
    pub period_end: Option<String>,
}

impl SuggestionQuery {
    /// 校验并归一化。
    pub fn parse(&self) -> std::result::Result<(Uuid, NaiveDate, NaiveDate), ApiError> {
        let supplier = clean(self.supplier_id.as_deref()).ok_or_else(|| bad_request("supplier_id 不能为空"))?;
        let supplier_id = parse_uuid(&supplier, "supplier_id")?;
        let start = clean(self.period_start.as_deref()).ok_or_else(|| bad_request("period_start 不能为空"))?;
        let end = clean(self.period_end.as_deref()).ok_or_else(|| bad_request("period_end 不能为空"))?;
        let start = parse_date(&start, "period_start")?;
        let end = parse_date(&end, "period_end")?;
        if !model::period_is_valid(start, end) {
            return Err(bad_request(format!(
                "period_start {} 晚于 period_end {}",
                model::date_text(start),
                model::date_text(end)
            )));
        }
        Ok((supplier_id, start, end))
    }
}

// ==================== 打分录入 ====================

/// 打分入参（`POST /business/erp/supplier-evaluation`）。
///
/// 语义 = **该供应商该期间的结论就是我给的这些分数**：没有就取号新建、有就覆盖
/// （`uk_biz_sev_supplier_period` 一份期间只有一个结论，见 M4-13 头注释）。
/// 四维权重不传 → 四等分 0.25（与库层默认值同口径）；传了就必须四个都给且之和 = 1。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SaveEvaluationReq {
    /// 供应商 id（必填 uuid）。
    pub supplier_id: Option<String>,
    /// 评估期间起（必填 `YYYY-MM-DD`）。
    pub period_start: Option<String>,
    /// 评估期间止（必填 `YYYY-MM-DD`）。
    pub period_end: Option<String>,
    /// 评估日（可空 → 北京时间当天；不许早于期间起点）。
    pub eval_date: Option<String>,
    /// 质量权重（0–1；四个权重要么全给要么全不给）。
    pub quality_weight: Option<String>,
    /// 交期权重。
    pub delivery_weight: Option<String>,
    /// 价格权重。
    pub price_weight: Option<String>,
    /// 服务权重。
    pub service_weight: Option<String>,
    /// 质量得分（0–100，必填）。
    pub quality_score: Option<String>,
    /// 交期得分（0–100，必填）。
    pub delivery_score: Option<String>,
    /// 价格得分（0–100，必填）。
    pub price_score: Option<String>,
    /// 服务得分（0–100，必填）。
    pub service_score: Option<String>,
    /// 备注（扣分事由 / 改进要求 / 复审约定）。
    pub remark: Option<String>,
}

/// 已解析的打分入参（handler 侧再补评估人 / 评估日）。
#[derive(Debug, Clone)]
pub struct ParsedEvaluation {
    /// 供应商。
    pub supplier_id: Uuid,
    /// 期间起。
    pub period_start: NaiveDate,
    /// 期间止。
    pub period_end: NaiveDate,
    /// 评估日（显式给的就是它，没给置 `None` 由 handler 填当天）。
    pub eval_date: Option<NaiveDate>,
    /// 四维权重。
    pub weights: Weights,
    /// 四维分数。
    pub scores: model::DimensionScores,
    /// 备注。
    pub remark: Option<String>,
}

impl SaveEvaluationReq {
    /// 校验并归一化（权重 / 分数 / 期间 / 评估日，全部在写库之前判完）。
    pub fn parse(&self) -> std::result::Result<ParsedEvaluation, ApiError> {
        let supplier = clean(self.supplier_id.as_deref()).ok_or_else(|| bad_request("supplier_id 不能为空"))?;
        let supplier_id = parse_uuid(&supplier, "supplier_id")?;
        let start = clean(self.period_start.as_deref()).ok_or_else(|| bad_request("period_start 不能为空"))?;
        let end = clean(self.period_end.as_deref()).ok_or_else(|| bad_request("period_end 不能为空"))?;
        let period_start = parse_date(&start, "period_start")?;
        let period_end = parse_date(&end, "period_end")?;
        if !model::period_is_valid(period_start, period_end) {
            return Err(bad_request(format!(
                "评估期间起 {} 晚于止 {}（ck_biz_sev_period）",
                model::date_text(period_start),
                model::date_text(period_end)
            )));
        }
        let eval_date = match clean(self.eval_date.as_deref()) {
            None => None,
            Some(raw) => {
                let date = parse_date(&raw, "eval_date")?;
                if !model::eval_date_is_valid(period_start, date) {
                    return Err(bad_request(format!(
                        "评估日 {} 早于评估期间起点 {}（ck_biz_sev_eval_date）",
                        model::date_text(date),
                        model::date_text(period_start)
                    )));
                }
                Some(date)
            }
        };
        let weights = self.parse_weights()?;
        if !weights.is_valid() {
            return Err(bad_request(
                "四维权重必须各在 0–1 且之和精确等于 1（ck_biz_sev_weights）".to_string(),
            ));
        }
        let scores = model::DimensionScores {
            quality: self.require_score(self.quality_score.as_deref(), "quality_score")?,
            delivery: self.require_score(self.delivery_score.as_deref(), "delivery_score")?,
            price: self.require_score(self.price_score.as_deref(), "price_score")?,
            service: self.require_score(self.service_score.as_deref(), "service_score")?,
        };
        if !scores.is_valid() {
            return Err(bad_request(format!(
                "四维分数必须各在 0–{}（ck_biz_sev_scores）",
                model::SCORE_MAX
            )));
        }
        Ok(ParsedEvaluation {
            supplier_id,
            period_start,
            period_end,
            eval_date,
            weights,
            scores,
            remark: clean(self.remark.as_deref()),
        })
    }

    /// 权重解析：四个都不给 → 四等分；给了任意一个就必须四个都给。
    fn parse_weights(&self) -> std::result::Result<Weights, ApiError> {
        let raw = [
            self.quality_weight.as_deref(),
            self.delivery_weight.as_deref(),
            self.price_weight.as_deref(),
            self.service_weight.as_deref(),
        ];
        let given: Vec<&str> = raw
            .iter()
            .filter_map(|item| item.map(str::trim))
            .filter(|text| !text.is_empty())
            .collect();
        if given.is_empty() {
            return Ok(Weights::equal());
        }
        if given.len() != 4 {
            return Err(bad_request(
                "四维权重要么四个都给（之和 = 1），要么都不给（默认四等分 0.25）",
            ));
        }
        Ok(Weights {
            quality: parse_decimal(given[0], "quality_weight")?,
            delivery: parse_decimal(given[1], "delivery_weight")?,
            price: parse_decimal(given[2], "price_weight")?,
            service: parse_decimal(given[3], "service_weight")?,
        })
    }

    /// 必填分数（空 → 400，点名哪一维）。
    fn require_score(&self, raw: Option<&str>, field: &str) -> std::result::Result<Decimal, ApiError> {
        let text = clean(raw).ok_or_else(|| bad_request(format!("{field} 不能为空（四维都要打分）")))?;
        parse_decimal(&text, field)
    }
}

// ==================== 出参 ====================

/// 评估单一行（列表 / 详情 / 排名**同一个结构**，页面不为详情另建窄类型）。
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationRowRes {
    /// 主键。
    pub id: String,
    /// 单号 `SEV-yyyyMMdd-NNNNNN`。
    pub evaluation_no: String,
    /// 供应商 id。
    pub supplier_id: String,
    /// 供应商名**快照**（评估结论的历史事实，约定 13）。
    pub supplier_name: Option<String>,
    /// 供应商**现名**（快照为空时回退到它）。
    pub supplier_display_name: String,
    /// 期间起 `YYYY-MM-DD`。
    pub period_start: String,
    /// 期间止 `YYYY-MM-DD`。
    pub period_end: String,
    /// 期间展示标签（`2026-Q1` / `2026-03` / `2026-FY` / `起~止`）。
    pub period_label: String,
    /// 评估日。
    pub eval_date: String,
    /// 评估人 id。
    pub evaluator_id: Option<String>,
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
    /// 加权总分（库层生成列，只读）。
    pub total_score: Decimal,
    /// 等级（`None` = 未评级）。
    pub grade: Option<String>,
    /// 等级中文（未评级时是「未评级」）。
    pub grade_label: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间（RFC3339）。
    pub created_at: String,
}

/// 等级中文名（`None` = 未评级，不造「E 级」）。
pub fn grade_label(grade: Option<&str>) -> &'static str {
    match grade {
        Some(model::GRADE_A) => "A（优秀）",
        Some(model::GRADE_B) => "B（良好）",
        Some(model::GRADE_C) => "C（合格需改进）",
        Some(model::GRADE_D) => "D（不合格）",
        _ => "未评级",
    }
}

/// 行 → 出参。
pub fn to_row_res(row: &EvaluationRow) -> EvaluationRowRes {
    EvaluationRowRes {
        id: row.id.to_string(),
        evaluation_no: row.evaluation_no.clone(),
        supplier_id: row.supplier_id.to_string(),
        supplier_name: row.supplier_name.clone(),
        supplier_display_name: row.supplier_display_name.clone(),
        period_start: model::date_text(row.period_start),
        period_end: model::date_text(row.period_end),
        period_label: model::period_label(row.period_start, row.period_end),
        eval_date: model::date_text(row.eval_date),
        evaluator_id: row.evaluator_id.map(|id| id.to_string()),
        evaluator_name: row.evaluator_name.clone(),
        quality_weight: row.quality_weight,
        delivery_weight: row.delivery_weight,
        price_weight: row.price_weight,
        service_weight: row.service_weight,
        quality_score: row.quality_score,
        delivery_score: row.delivery_score,
        price_score: row.price_score,
        service_score: row.service_score,
        total_score: row.total_score,
        grade: row.grade.clone(),
        grade_label: grade_label(row.grade.as_deref()).to_string(),
        remark: row.remark.clone(),
        created_at: row.created_at.to_rfc3339(),
    }
}

/// 打分结果（新建 / 覆盖同一结构，`created` 区分）。
#[derive(Debug, Clone, Serialize)]
pub struct SaveEvaluationRes {
    /// 落库后的整行（总分取自库层生成列）。
    pub row: EvaluationRowRes,
    /// `true` = 本次取号新建；`false` = 覆盖已有评估（重打分）。
    pub created: bool,
}

/// 排名一行（`rank` 从 1 起；行本体与列表同构）。
#[derive(Debug, Clone, Serialize)]
pub struct RankRowRes {
    /// 名次（1 起，按总分降序；并列按单号升序，名次不并列 —— 与 SQL 的 ORDER BY 一致）。
    pub rank: u32,
    /// 评估单本体（`#[serde(flatten)]`：前端拿到的是「行 + rank」的一层结构）。
    #[serde(flatten)]
    pub row: EvaluationRowRes,
}

/// 排名出参。
#[derive(Debug, Clone, Serialize)]
pub struct RankRes {
    /// 期间起。
    pub period_start: String,
    /// 期间止。
    pub period_end: String,
    /// 期间展示标签。
    pub period_label: String,
    /// 该期间参与排名的评估数（= `rows.len()`，给「共 N 家」用）。
    pub total: u32,
    /// 名次列表（已按总分降序）。
    pub rows: Vec<RankRowRes>,
}

/// 交期参考分出参（`DeliveryStat` + 说明）。
#[derive(Debug, Clone, Serialize)]
pub struct DeliverySuggestionRes {
    /// 供应商 id。
    pub supplier_id: String,
    /// 期间起。
    pub period_start: String,
    /// 期间止。
    pub period_end: String,
    /// 参与统计的入库单张数（`POSTED` + 挂采购单 + 有交期）。
    pub docs: i64,
    /// 其中含逾期行的入库单张数。
    pub late_docs: i64,
    /// 到货总数量。
    pub total_qty: Decimal,
    /// 准时数量。
    pub on_time_qty: Decimal,
    /// 逾期数量。
    pub late_qty: Decimal,
    /// 准时率（0–100，2 位）；**没有到货数据时是 `null`**（不是 0 分）。
    pub on_time_rate: Option<Decimal>,
    /// 建议的交期维度得分（= 准时率；`null` = 无数据，只能人工打分）。
    pub suggested_score: Option<Decimal>,
    /// 一句话口径说明（页面原样展示，免得用户以为三维也有自动分）。
    pub note: String,
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份合法的打分入参（四等分权重 + 四维 80 分）。
    fn valid_req() -> SaveEvaluationReq {
        SaveEvaluationReq {
            supplier_id: Some("0193f0e1-0000-7000-8000-000000000001".to_string()),
            period_start: Some("2028-01-01".to_string()),
            period_end: Some("2028-03-31".to_string()),
            eval_date: Some("2028-03-31".to_string()),
            quality_weight: None,
            delivery_weight: None,
            price_weight: None,
            service_weight: None,
            quality_score: Some("80".to_string()),
            delivery_score: Some("80".to_string()),
            price_score: Some("80".to_string()),
            service_score: Some("80".to_string()),
            remark: Some("  备注两边空格要 trim  ".to_string()),
        }
    }

    fn status_of(error: &ApiError) -> u16 {
        error.status
    }

    #[test]
    fn save_accepts_valid_input_and_defaults_equal_weights() {
        let parsed = valid_req().parse().expect("合法入参");
        assert_eq!(
            parsed.weights,
            Weights::equal(),
            "不传权重 = 四等分（与库层默认值一致）"
        );
        assert_eq!(parsed.remark.as_deref(), Some("备注两边空格要 trim"));
        assert_eq!(model::date_text(parsed.period_start), "2028-01-01");
    }

    #[test]
    fn save_requires_all_four_scores() {
        let mut req = valid_req();
        req.price_score = Some("   ".to_string());
        let error = req.parse().expect_err("缺价格得分必须拒");
        assert_eq!(status_of(&error), 400);
    }

    #[test]
    fn save_rejects_partial_weights() {
        let mut req = valid_req();
        req.quality_weight = Some("0.4".to_string());
        let error = req.parse().expect_err("只给一个权重必须拒（要么四个要么不给）");
        assert_eq!(status_of(&error), 400);
        assert!(error.msg.as_str().contains("四个都给"));
    }

    #[test]
    fn save_rejects_weight_sum_not_exactly_one() {
        let mut req = valid_req();
        req.quality_weight = Some("0.4".to_string());
        req.delivery_weight = Some("0.3".to_string());
        req.price_weight = Some("0.2".to_string());
        req.service_weight = Some("0.2".to_string());
        let error = req.parse().expect_err("和 1.1 必须拒");
        assert_eq!(status_of(&error), 400);
        assert!(error.msg.as_str().contains("之和精确等于 1"));
    }

    #[test]
    fn save_rejects_score_out_of_range() {
        let mut req = valid_req();
        req.quality_score = Some("100.01".to_string());
        let error = req.parse().expect_err("分数越界必须拒");
        assert_eq!(status_of(&error), 400);
        assert!(error.msg.as_str().contains("0–100"));
    }

    #[test]
    fn save_rejects_inverted_period_and_early_eval_date() {
        let mut req = valid_req();
        req.period_start = Some("2028-03-31".to_string());
        req.period_end = Some("2028-01-01".to_string());
        let inverted = req.parse().expect_err("期间颠倒");
        assert_eq!(status_of(&inverted), 400);

        let mut req = valid_req();
        req.eval_date = Some("2027-12-31".to_string());
        let error = req.parse().expect_err("评估日早于期间起");
        assert_eq!(status_of(&error), 400);
        assert!(error.msg.as_str().contains("早于评估期间起点"));
    }

    #[test]
    fn save_rejects_bad_uuid_and_bad_date() {
        let mut req = valid_req();
        req.supplier_id = Some("not-a-uuid".to_string());
        let bad_uuid = req.parse().expect_err("非法 uuid");
        assert_eq!(status_of(&bad_uuid), 400);

        let mut req = valid_req();
        req.period_start = Some("2028-13-01".to_string());
        let bad_date = req.parse().expect_err("非法日期");
        assert_eq!(status_of(&bad_date), 400);
    }

    #[test]
    fn page_filter_rejects_grade_outside_closed_set() {
        let query = EvaluationPageQuery {
            grade: Some("E".to_string()),
            ..EvaluationPageQuery::default()
        };
        let error = query.filter().expect_err("E 不在等级闭集");
        assert_eq!(status_of(&error), 400);
        assert!(error.msg.as_str().contains("A / B / C / D"));
    }

    #[test]
    fn page_filter_clamps_paging_and_trims_keyword() {
        let query = EvaluationPageQuery {
            page: Some(0),
            page_size: Some(9999),
            keyword: Some("  SEV-2028  ".to_string()),
            ..EvaluationPageQuery::default()
        };
        let filter = query.filter().expect("合法");
        assert_eq!(filter.page, 1, "页码最小 1");
        assert_eq!(filter.page_size, 200, "每页上限 200");
        assert_eq!(filter.keyword.as_deref(), Some("SEV-2028"));
        assert!(filter.grade.is_none(), "空等级 = 不筛");
    }

    #[test]
    fn page_filter_rejects_over_long_keyword() {
        let query = EvaluationPageQuery {
            keyword: Some("x".repeat(MAX_KEYWORD + 1)),
            ..EvaluationPageQuery::default()
        };
        let error = query.filter().expect_err("超长关键字必须 400（不截断）");
        assert_eq!(status_of(&error), 400);
    }

    #[test]
    fn rank_requires_period() {
        let error = RankQuery::default().range().expect_err("排名期间必填");
        assert_eq!(status_of(&error), 400);

        let (start, end, limit) = RankQuery {
            period_start: Some("2028-01-01".to_string()),
            period_end: Some("2028-03-31".to_string()),
            limit: None,
        }
        .range()
        .expect("合法");
        assert_eq!(model::date_text(start), "2028-01-01");
        assert_eq!(model::date_text(end), "2028-03-31");
        assert_eq!(limit, 100, "默认取前 100 名");
    }

    #[test]
    fn suggestion_requires_supplier_and_period() {
        let error = SuggestionQuery::default().parse().expect_err("三件都必填");
        assert_eq!(status_of(&error), 400);
    }

    #[test]
    fn row_res_carries_labels_and_stringified_ids() {
        let row = EvaluationRow {
            id: Uuid::now_v7(),
            evaluation_no: "SEV-20280101-000001".to_string(),
            supplier_id: Uuid::now_v7(),
            supplier_name: Some("M416 快照名".to_string()),
            supplier_display_name: "M416 现名".to_string(),
            period_start: NaiveDate::parse_from_str("2028-01-01", "%Y-%m-%d").expect("日期"),
            period_end: NaiveDate::parse_from_str("2028-03-31", "%Y-%m-%d").expect("日期"),
            eval_date: NaiveDate::parse_from_str("2028-03-31", "%Y-%m-%d").expect("日期"),
            evaluator_id: None,
            evaluator_name: Some("M416 评估人".to_string()),
            quality_weight: "0.25".parse().expect("定点"),
            delivery_weight: "0.25".parse().expect("定点"),
            price_weight: "0.25".parse().expect("定点"),
            service_weight: "0.25".parse().expect("定点"),
            quality_score: "90".parse().expect("定点"),
            delivery_score: "90".parse().expect("定点"),
            price_score: "90".parse().expect("定点"),
            service_score: "90".parse().expect("定点"),
            total_score: "90.00".parse().expect("定点"),
            grade: Some(model::GRADE_A.to_string()),
            remark: None,
            created_at: chromo_now(),
            create_by: None,
        };
        let res = to_row_res(&row);
        assert_eq!(res.period_label, "2028-Q1", "period_label 由 model 派生");
        assert_eq!(res.grade_label, "A（优秀）");
        assert_eq!(res.supplier_display_name, "M416 现名", "展示用现名，快照另存");
        assert_eq!(res.supplier_name.as_deref(), Some("M416 快照名"));
        assert_eq!(res.id, row.id.to_string(), "uuid 出参一律字符串");
        assert!(res.evaluator_id.is_none());
    }

    #[test]
    fn grade_label_covers_unrated_as_null_not_e() {
        assert_eq!(grade_label(None), "未评级");
        assert_eq!(grade_label(Some("D")), "D（不合格）");
        assert_eq!(grade_label(Some("E")), "未评级", "值域外的值不造第五个等级");
    }

    fn chromo_now() -> nik::prelude::chrono::DateTime<nik::prelude::chrono::Utc> {
        nik::prelude::chrono::Utc::now()
    }
}

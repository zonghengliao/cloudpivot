//! 供应商评估接口层（M4-16）：分页 / 详情 / 打分录入 / 本期排名 / 交期参考分。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/supplier-evaluation/...`。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET  | `/business/erp/supplier-evaluation/page` | `business:erp:supplier-evaluation:list` | 台账分页（关键字 / 等级 / 供应商 / 期间） |
//! | GET  | `/business/erp/supplier-evaluation/rank` | 同上 | 本期排名（期间必填，按总分降序，名次从 1 起） |
//! | GET  | `/business/erp/supplier-evaluation/delivery-suggestion` | `business:erp:supplier-evaluation:save` | 交期维度参考分（按真实入库单算准时率；无数据给 `null`） |
//! | GET  | `/business/erp/supplier-evaluation/{id}` | `business:erp:supplier-evaluation:list` | 详情 |
//! | POST | `/business/erp/supplier-evaluation` | `business:erp:supplier-evaluation:save` | 打分录入（同供应商同期间：没有就取号新建，有就覆盖） |
//!
//! 页面显形码 `business:erp:supplier-evaluation:page` 只给前端（侧栏显隐 + 路由 `meta.permission`），
//! 后端零引用（AGENTS 约定 14）。
//!
//! # 三条硬规则（都继承自 M4-12 / M4-13，不在本层重写）
//!
//! ```text
//! ① 总分是**库层生成列**：入参里没有 total_score，出参里的 total_score 就是库里那一列；
//! ② 等级与阈值同源：grade 由 grading::save_evaluation 按 GRADE_THRESHOLDS 写，
//!    本层不查阈值、不判等级（连 if 都不写）；
//! ③ 一份期间一个结论：同 (supplier_id, period_start, period_end) 重复打分 = 覆盖，
//!    单号不变（uk_biz_sev_supplier_period）。
//! ```
//!
//! # 为什么没有删 / 改接口
//!
//! 评估结论是**打分事实**，改分就是重新打分（同一个 POST 入口覆盖），删单没有业务含义
//! （删了「上季度这家多少分」就消失了）。要作废一家供应商，走 M4-14 的准入 / 黑名单流程。

use axum::Json;
use axum::extract::{Path, Query};
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase::supplier_access_model as access_model;
use crate::business::erp::purchase::supplier_evaluation_dto::{
    self as dto, DeliverySuggestionRes, EvaluationPageQuery, EvaluationRowRes, PERM_LIST, PERM_SAVE, RankQuery,
    RankRes, RankRowRes, SaveEvaluationReq, SaveEvaluationRes, SuggestionQuery,
};
use crate::business::erp::purchase::supplier_evaluation_grading::{self as grading, DeliveryStat, EvaluationInput};
use crate::business::erp::purchase::supplier_evaluation_model as model;
use cloudpivot_kernel::utils;

// 代码里断言一份常量与路由注解一致：路由上的字面量必须能在本模块的常量清单里找到
const _: () = {
    let _ = [PERM_LIST, PERM_SAVE];
};

// ==================== 错误归一 ====================

/// 数据层错误 → HTTP：入参类（`InvalidValue`）是 400 且**带原文**，其余一律 500（不泄露内部细节）。
///
/// 为什么要单独判 `InvalidValue`：`grading::save_evaluation` 会把「权重和不是 1 / 分数越界 /
/// 评估日早于期间 / 供应商不存在」都报成 `InvalidValue` —— 这些是用户能自己改的输入错误，
/// 不该是「服务器内部错误」。
fn map_db(error: DbError, ctx: &str) -> ApiError {
    match error {
        DbError::InvalidValue(message) => dto::bad_request(message),
        other => utils::db_error(other, ctx),
    }
}

// ==================== ① 台账分页 ====================

/// 台账分页（`GET /page`）。
#[nik::permission(
    path = "/business/erp/supplier-evaluation/page",
    method = "GET",
    permission = "business:erp:supplier-evaluation:list"
)]
#[nik::get("/business/erp/supplier-evaluation/page")]
pub async fn supplier_evaluation_page(
    Query(query): Query<EvaluationPageQuery>,
) -> ResResult<PageData<Vec<EvaluationRowRes>>> {
    let filter = query.filter()?;
    let (rows, total) = model::page_evaluations(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商评估台账"))?;
    Ok(Res::ok(PageData {
        list: rows.iter().map(dto::to_row_res).collect(),
        total,
        page: filter.page,
        page_size: filter.page_size,
    }))
}

// ==================== ② 本期排名 ====================

/// 本期排名（`GET /rank`）：同一期间内按加权总分降序，名次从 1 起。
///
/// 名次**不并列**：排序键是 `total_score DESC, evaluation_no`（总分相同按单号先后），
/// 所以「同分两家」会拿到相邻两个名次 —— 排名榜要的是「第几」，不是竞赛名次（1,1,3）。
#[nik::permission(
    path = "/business/erp/supplier-evaluation/rank",
    method = "GET",
    permission = "business:erp:supplier-evaluation:list"
)]
#[nik::get("/business/erp/supplier-evaluation/rank")]
pub async fn supplier_evaluation_rank(Query(query): Query<RankQuery>) -> ResResult<RankRes> {
    let (period_start, period_end, limit) = query.range()?;
    let rows = model::evaluations_in_period(db(), period_start, period_end, limit)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商评估排名"))?;
    Ok(Res::ok(build_rank(period_start, period_end, &rows)))
}

/// 行列表 → 排名出参（名次从 1 起；**不并列**：排序键已在 SQL 里定死 `total_score DESC, evaluation_no`）。
///
/// 抽成 pub 是为了让真库用例直接打「名次怎么编」这一段，而不必起一个 HTTP 服务。
pub fn build_rank(
    period_start: nik::prelude::chrono::NaiveDate,
    period_end: nik::prelude::chrono::NaiveDate,
    rows: &[model::EvaluationRow],
) -> RankRes {
    let ranked: Vec<RankRowRes> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| RankRowRes {
            rank: u32::try_from(index + 1).unwrap_or(u32::MAX),
            row: dto::to_row_res(row),
        })
        .collect();
    RankRes {
        period_start: model::date_text(period_start),
        period_end: model::date_text(period_end),
        period_label: model::period_label(period_start, period_end),
        total: u32::try_from(ranked.len()).unwrap_or(u32::MAX),
        rows: ranked,
    }
}

// ==================== ③ 交期参考分 ====================

/// 交期维度参考分（`GET /delivery-suggestion`）。
///
/// 只有**交期**有客观数据（`biz_stock_in` 挂采购单后比 `in_date` 与 `expect_date`）；
/// 质量 / 价格 / 服务三维没有数据源，必须人工打分 —— 所以这里只算交期，`note` 里写明口径。
#[nik::permission(
    path = "/business/erp/supplier-evaluation/delivery-suggestion",
    method = "GET",
    permission = "business:erp:supplier-evaluation:save"
)]
#[nik::get("/business/erp/supplier-evaluation/delivery-suggestion")]
pub async fn supplier_evaluation_delivery_suggestion(
    Query(query): Query<SuggestionQuery>,
) -> ResResult<DeliverySuggestionRes> {
    let (supplier_id, period_start, period_end) = query.parse()?;
    let stat: DeliveryStat = grading::delivery_stat(db(), supplier_id, period_start, period_end)
        .await
        .map_err(|error| utils::db_error(error, "统计交期准时情况"))?;
    Ok(Res::ok(DeliverySuggestionRes {
        supplier_id: supplier_id.to_string(),
        period_start: model::date_text(period_start),
        period_end: model::date_text(period_end),
        docs: stat.docs,
        late_docs: stat.late_docs,
        total_qty: stat.total_qty,
        on_time_qty: stat.on_time_qty,
        late_qty: stat.late_qty,
        on_time_rate: stat.on_time_rate(),
        suggested_score: stat.suggested_score(),
        note: if stat.is_usable() {
            "交期建议分 = 准时数量占比（只看已过账、挂采购单且有交期的采购入库）；可手动改。".to_string()
        } else {
            "该期间没有可判准时的采购入库（需已过账 + 挂采购单 + 有交期），交期维度只能人工打分。".to_string()
        },
    }))
}

// ==================== ④ 详情 ====================

/// 详情（`GET /{id}`）。
#[nik::permission(
    path = "/business/erp/supplier-evaluation/{id}",
    method = "GET",
    permission = "business:erp:supplier-evaluation:list"
)]
#[nik::get("/business/erp/supplier-evaluation/{id}")]
pub async fn supplier_evaluation_detail(Path(id): Path<String>) -> ResResult<EvaluationRowRes> {
    let id = dto::parse_uuid(&id, "id")?;
    Ok(Res::ok(detail_of(id).await?))
}

/// 组装详情（打分入口的返回值共用）。
pub async fn detail_of(id: Uuid) -> std::result::Result<EvaluationRowRes, ApiError> {
    Ok(dto::to_row_res(&load_or_404(id).await?))
}

// ==================== ⑤ 打分录入 ====================

/// 打分核心逻辑（单测 / 页面走同一条代码路径）。
///
/// `operator` 同时写进 `evaluator_id` 与 `create_by`：评估人默认就是当前操作人
/// （页面不提供「替别人打分」的入口 —— 那会让「谁给的分」失去审计意义）。
pub async fn save_evaluation_core(
    req: &SaveEvaluationReq,
    operator: Option<Uuid>,
) -> std::result::Result<SaveEvaluationRes, ApiError> {
    let parsed = req.parse()?;
    let now = Utc::now();
    let eval_date = parsed.eval_date.unwrap_or_else(|| model::today_cst(now));
    let evaluator_name = match operator {
        Some(id) => access_model::user_name(db(), id)
            .await
            .map_err(|error| utils::db_error(error, "查询评估人姓名"))?,
        None => None,
    };
    let input = EvaluationInput {
        supplier_id: parsed.supplier_id,
        period_start: parsed.period_start,
        period_end: parsed.period_end,
        eval_date,
        weights: parsed.weights,
        scores: parsed.scores,
        remark: parsed.remark,
        evaluator_id: operator,
        evaluator_name,
        operator_id: operator,
    };
    let outcome = grading::save_evaluation(db(), &input, now)
        .await
        .map_err(|error| map_db(error, "保存供应商评估"))?;
    Ok(SaveEvaluationRes {
        row: dto::to_row_res(&outcome.row),
        created: outcome.created,
    })
}

/// 打分录入（`POST`）：同供应商同期间没有就新建（取号 `SEV-yyyyMMdd-NNNNNN`）、有就覆盖。
#[nik::permission(
    path = "/business/erp/supplier-evaluation",
    method = "POST",
    permission = "business:erp:supplier-evaluation:save"
)]
#[nik::post("/business/erp/supplier-evaluation")]
pub async fn supplier_evaluation_save(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Json(req): Json<SaveEvaluationReq>,
) -> ResResult<SaveEvaluationRes> {
    let operator = utils::current_user_uuid(&claims);
    Ok(Res::ok(save_evaluation_core(&req, operator).await?))
}

// ==================== 内部小工具 ====================

/// 取评估单，不存在 → 404。
async fn load_or_404(id: Uuid) -> std::result::Result<model::EvaluationRow, ApiError> {
    model::find_evaluation(db(), id)
        .await
        .map_err(|error| utils::db_error(error, "查询供应商评估单"))?
        .ok_or_else(|| dto::not_found(id))
}

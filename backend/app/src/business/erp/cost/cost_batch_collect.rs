//! 分批法「料工费归集到批次」· 编排层（成本核算 · M5-03）。
//!
//! 两件事：
//!
//! | 动作 | 写库 | 说明 |
//! |---|---|---|
//! | [`preview`] 三路来源预览 | ❌ | 把「本批本期的料/工/费各是哪几笔、哪些已归集、哪些待归集、哪些归不了及原因」一次算清 |
//! | [`collect`] 执行归集 | ✅ 一个事务 | 把本批本期该归集的行写进 `biz_cost_collect`（先软删过期行、再插入、再回查） |
//!
//! # 为什么这两个动作共用同一套「扫描 + 取差」
//!
//! 预览与执行如果各写一套判据，就会出现「预览说会写 3 笔、点了却写 2 笔」。
//! 所以两者都走 [`gather`]（同一个扫描 + 同一个 `dto::plan`），预览只是**不落库**：
//! 唯一的差别是 `collect` 把 `plan.to_insert` / `plan.stale` 落到库里。
//!
//! # 事务边界（一次调用一个事务）
//!
//! `collect` 在**一个事务**里做完：再确认批次状态 → 扫三路 → 校验 → 清过期 → 写新行 →
//! 回查合计。中途任一步失败 ⇒ 一行都不写（不会出现「料归集了、工没归集」的中间态）。
//!
//! # 先软删、再插入（顺序不能反）
//!
//! `uk_biz_cost_collect_src` 是 `WHERE deleted_at IS NULL` 的**部分**唯一索引：
//! 同一来源行「改挂批次 / 金额被修正」时，旧行还活着就占着键，先 INSERT 会被
//! `ON CONFLICT DO NOTHING` 静默吞掉，随后再删旧行 ⇒ 这个键上一行不剩。
//! 与 calc 各段同一处纪律（那边是 B2-6/7/8，这里只是复述，别改顺序）。
//!
//! # 不做的事（明确边界）
//!
//! * **不碰期间批次**：不建、不改、不重算 `biz_cost_period` / `biz_cost_period_item`
//!   —— 那是 calc 与结算的活。本期已有 `DRAFT` 批次时出参给 `recalc_required = true`，
//!   由用户决定何时重跑 calc；
//! * **不碰别的批次**：`biz_cost_collect` 里别的对象的行一行不动（见
//!   `dto::subset_for_object` 的三条取舍）；
//! * **不碰池**：折旧 / 报销 / 未挂对象的费用进车间费用池，属分摊段；
//! * **不接金额入参**：所有金额来自来源单据，绕过单据就只能走领料/工时/费用三个入口。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_batch_collect_dto::{
    self as dto, BatchCollectReq, BatchCollectRes, BatchSourceQuery, BatchSourcesRes, ElementCollectRes,
    ElementSourcesRes, PeriodBatchRes, SourceLineRes,
};
use crate::business::erp::cost::cost_batch_collect_model::{self as model, ElementTotals};
use crate::business::erp::cost::cost_calc_material;
use crate::business::erp::cost::cost_collect_dto::{self as collect_dto, CollectPlan, MaterialLine};
use crate::business::erp::cost::cost_collect_model;
use crate::business::erp::cost::cost_collect_sheet_dto::{SheetAmounts, SheetBatchRes};
use crate::business::erp::cost::cost_labor_collect_dto::{self as labor_dto, LaborLine, LaborPlan};
use crate::business::erp::cost::cost_overhead_collect_dto::{self as oh_dto, OverheadLine, OverheadPlan};
use crate::business::erp::cost::cost_overhead_collect_model;
use crate::business::erp::cost::cost_period_dto;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 错误出口 ====================

/// 批次不存在 / 已删 / 不在你的数据范围 —— 三种都是 404（不区分，避免探测）。
fn batch_not_found(raw: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!(
            "批次不存在、已删除，或不在你的数据范围（{raw}）。\
             归集表请用 GET /business/erp/cost-collect-sheet?period_code=yyyy-MM 先找到批次"
        ),
    )
}

/// 路径参数 → UUID（非法形状也是 404：这个位置本来就该是 id）。
fn parse_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| batch_not_found(raw))
}

// ==================== 三路「扫描 + 取差」 ====================

/// 料：本批本期已过账领料 + 该看的已有归集行。
struct MaterialGather {
    candidates: Vec<MaterialLine>,
    plan: CollectPlan,
}

/// 工：本批本期已确认工时 + 该看的已有归集行。
struct LaborGather {
    candidates: Vec<LaborLine>,
    plan: LaborPlan,
}

/// 费：本批本期直接挂本批的手工费用 + 该看的已有归集行。
struct OverheadGather {
    candidates: Vec<OverheadLine>,
    plan: OverheadPlan,
}

/// 三路取差结果。
struct Gathered {
    material: MaterialGather,
    labor: LaborGather,
    overhead: OverheadGather,
}

/// 扫描 + 校验 + 取差（**不写库**，预览与执行共用）。
///
/// 校验顺序：先校验全部来源行（异常行当场 400 并点名单号，一行都不写），再取差。
/// 与 calc 各段同一条纪律：**绝不静默跳过**异常来源行 —— 跳过的后果是归集金额与
/// 领料/工时台账差一笔、且没人知道为什么。
async fn gather(
    ex: &impl SqlExecutor,
    period_code: &str,
    start: NaiveDate,
    end: NaiveDate,
    object_id: Uuid,
) -> std::result::Result<Gathered, ApiError> {
    // ---------- 料 ----------
    let mut candidates = cost_collect_model::material_source_lines(ex, start, end)
        .await
        .map_err(|error| utils::db_error(error, "扫描本期已过账领料"))?;
    candidates.retain(|line| line.cost_object_id == object_id);
    for line in &candidates {
        if let Err(reason) = line.validate() {
            return Err(collect_dto::source_invalid(&line.out_no, &reason));
        }
    }
    let all_existing = cost_collect_model::collect_rows_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取本期已归集的料行"))?;
    let keys: Vec<_> = candidates.iter().map(MaterialLine::key).collect();
    let existing = dto::subset_for_object(&all_existing, object_id, &keys);
    let material = MaterialGather {
        plan: collect_dto::plan(&candidates, &existing),
        candidates,
    };

    // ---------- 工 ----------
    let mut labor_candidates = cost_collect_model::labor_source_lines(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "扫描本期已确认工时"))?;
    labor_candidates.retain(|line| line.cost_object_id == object_id);
    for line in &labor_candidates {
        if let Err(reason) = line.validate() {
            return Err(labor_dto::labor_invalid(line, &reason));
        }
    }
    let all_labor = cost_collect_model::labor_collect_rows_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取本期已归集的工行"))?;
    let labor_keys: Vec<_> = labor_candidates.iter().map(LaborLine::key).collect();
    let labor_existing = dto::subset_for_object(&all_labor, object_id, &labor_keys);
    let labor = LaborGather {
        plan: labor_dto::plan_of(&labor_candidates, &labor_existing),
        candidates: labor_candidates,
    };

    // ---------- 费 ----------
    let mut oh_candidates = cost_overhead_collect_model::overhead_source_lines(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "扫描本期手工费用台账"))?;
    // 只收「直接挂本批」的：折旧/报销恒为 `None`，未挂对象的手工费用也进池
    oh_candidates.retain(|line| line.cost_object_id == Some(object_id));
    for line in &oh_candidates {
        if let Err(reason) = line.validate() {
            return Err(oh_dto::overhead_invalid(line, &reason));
        }
    }
    let all_oh = cost_overhead_collect_model::overhead_collect_rows_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取本期已归集的费行"))?;
    let oh_keys: Vec<_> = oh_candidates.iter().map(OverheadLine::key).collect();
    let oh_existing = dto::subset_for_object(&all_oh, object_id, &oh_keys);
    let overhead = OverheadGather {
        plan: oh_dto::plan_of(&oh_candidates, &oh_existing),
        candidates: oh_candidates,
    };

    Ok(Gathered {
        material,
        labor,
        overhead,
    })
}

// ==================== 预览 ====================

/// 三路来源预览（**只读**）。
///
/// 被状态闸拦下时**不报错**：照常返回来源清单 + `collectable = false` + 原因与修法，
/// 前端可以「按钮置灰 + tooltip 说明」。这与 `collect` 的选择不同（那个必须 409）：
/// 看数永远是允许的，改数才要拦。
pub async fn preview(
    principal: &ScopedPrincipal,
    id_raw: &str,
    query: BatchSourceQuery,
) -> std::result::Result<BatchSourcesRes, ApiError> {
    let object_id = parse_id(id_raw)?;
    let period_code = dto::period_of(query.period_code.as_deref())?;

    let batch = model::find_visible_batch(principal, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询批次抬头"))?
        .ok_or_else(|| batch_not_found(id_raw))?;

    let period_batch = model::active_period_batch(db(), &period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询本期成本批次"))?;
    let block = dto::block_of(&batch.object_no, &batch.status, period_batch.as_ref());

    let (start, end) = cost_calc_material::period_bounds(&period_code).await?;
    let gathered = gather(db(), &period_code, start, end, object_id).await?;

    let totals = model::element_totals(db(), object_id, &period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询本批已归集额"))?;

    // 物料名批量补（一次 IN 查询，不做 N+1）
    let material_ids: Vec<Uuid> = gathered
        .material
        .candidates
        .iter()
        .map(|line| line.material_id)
        .collect();
    let labels = model::material_labels(db(), &material_ids)
        .await
        .map_err(|error| utils::db_error(error, "补物料名"))?;

    let material = material_sources_res(&gathered.material, &totals, &labels);
    let labor = labor_sources_res(&gathered.labor, &totals);
    let overhead = overhead_sources_res(&gathered.overhead, &totals);

    let (mat, lab, oh) = (totals.material_amount, totals.labor_amount, totals.overhead_amount);
    // 归集后合计 = 已归集 − 本次软删 + 本次新写（与各要素的 `after_amount` 同一份算式）
    let mat_after = dto::after_amount(
        mat,
        gathered.material.plan.stale_amount(),
        gathered.material.plan.inserted_amount(),
    );
    let lab_after = dto::after_amount(
        lab,
        gathered.labor.plan.stale_amount(),
        gathered.labor.plan.inserted_amount(),
    );
    let oh_after = dto::after_amount(
        oh,
        gathered.overhead.plan.stale_amount(),
        gathered.overhead.plan.inserted_amount(),
    );
    let recalc_required = period_batch
        .as_ref()
        .is_some_and(|period| period.status == cost_period_dto::STATUS_DRAFT);

    Ok(BatchSourcesRes {
        period_code,
        batch,
        period_batch: period_batch.as_ref().map(PeriodBatchRes::of),
        collectable: block.is_none(),
        block_reason: block.as_ref().map(|item| item.reason.clone()),
        block_fix: block.as_ref().map(|item| item.fix.clone()),
        recalc_required,
        material,
        labor,
        overhead,
        totals: SheetAmounts::of(mat, lab, oh),
        after_totals: SheetAmounts::of(mat_after, lab_after, oh_after),
    })
}

/// 料路出参。
fn material_sources_res(
    gather: &MaterialGather,
    totals: &ElementTotals,
    labels: &std::collections::HashMap<Uuid, (Option<String>, Option<String>)>,
) -> ElementSourcesRes {
    let already: std::collections::HashSet<_> = gather.plan.already.iter().cloned().collect();
    let pending: std::collections::HashSet<_> = gather.plan.to_insert.iter().map(MaterialLine::key).collect();
    let rows = gather
        .candidates
        .iter()
        .map(|line| {
            let (state, state_label) = dto::line_state(&line.key(), &already, &pending);
            let (code, name) = labels.get(&line.material_id).cloned().unwrap_or((None, None));
            let label = match (code.as_deref(), name.as_deref()) {
                (_, Some(name)) => format!("领料单 {} {}", line.out_no, name),
                (Some(code), None) => format!("领料单 {} 物料 {}", line.out_no, code),
                _ => format!("领料单 {} 物料 {}", line.out_no, line.material_id),
            };
            SourceLineRes {
                element: dto::ELEMENT_MATERIAL,
                source_type: collect_dto::SOURCE_STOCK_OUT.to_string(),
                source_id: line.item_id.to_string(),
                source_no: Some(line.out_no.clone()),
                occur_date: line.out_date.to_string(),
                label,
                qty: Some(dto::amount_text(line.qty)),
                unit_price: Some(dto::amount_text(line.unit_price)),
                hours: None,
                rate: None,
                material_id: Some(line.material_id.to_string()),
                material_code: code,
                material_name: name,
                account_code: None,
                dept_name: line.dept_name.clone(),
                state,
                state_label,
                reason: None,
                amount: dto::amount_text(line.amount),
            }
        })
        .collect::<Vec<_>>();
    let rows_total = rows.len();
    let amount_total = gather.candidates.iter().map(|line| line.amount).sum();
    let pending_amount = gather.plan.inserted_amount();
    let purged_amount = gather.plan.stale_amount();
    ElementSourcesRes {
        element: dto::ELEMENT_MATERIAL,
        element_label: dto::element_label(dto::ELEMENT_MATERIAL),
        rows,
        rows_total,
        amount_total: dto::amount_text(amount_total),
        collected_rows: totals.material_rows,
        collected_amount: dto::amount_text(totals.material_amount),
        pending_rows: gather.plan.to_insert.len(),
        pending_amount: dto::amount_text(pending_amount),
        purged_rows: gather.plan.stale.len(),
        purged_amount: dto::amount_text(purged_amount),
        uncollected_rows: 0,
        uncollected_amount: dto::amount_text(Decimal::ZERO),
        after_amount: dto::amount_text(dto::after_amount(totals.material_amount, purged_amount, pending_amount)),
    }
}

/// 工路出参（缺费率的行 `UNCOLLECTABLE` + 原因）。
fn labor_sources_res(gather: &LaborGather, totals: &ElementTotals) -> ElementSourcesRes {
    let already: std::collections::HashSet<_> = gather.plan.already.iter().cloned().collect();
    let pending: std::collections::HashSet<_> = gather.plan.to_insert.iter().map(LaborLine::key).collect();
    let blocked: std::collections::HashMap<_, _> =
        gather.plan.uncollected.iter().map(|row| (row.labor_id, row)).collect();
    let rows = gather
        .candidates
        .iter()
        .map(|line| {
            let (state, state_label) = dto::line_state(&line.key(), &already, &pending);
            let reason = blocked.get(&line.labor_id).map(|row| match &row.hint {
                Some(hint) => format!("{}；{}", row.reason, hint),
                None => row.reason.clone(),
            });
            SourceLineRes {
                element: dto::ELEMENT_LABOR,
                source_type: labor_dto::SOURCE_LABOR_TICKET.to_string(),
                source_id: line.labor_id.to_string(),
                source_no: None,
                occur_date: line.work_date.to_string(),
                label: line.label(),
                qty: None,
                unit_price: None,
                hours: Some(dto::amount_text(line.hours)),
                rate: Some(dto::amount_text(line.rate)),
                material_id: None,
                material_code: None,
                material_name: None,
                account_code: None,
                dept_name: line.dept_name.clone(),
                state,
                state_label,
                reason,
                amount: dto::amount_text(line.amount),
            }
        })
        .collect::<Vec<_>>();
    let rows_total = rows.len();
    let amount_total = gather.candidates.iter().map(|line| line.amount).sum();
    let pending_amount = gather.plan.inserted_amount();
    let purged_amount = gather.plan.stale_amount();
    let uncollected_amount: Decimal = gather.plan.uncollected.iter().map(|row| row.hours * row.rate).sum();
    ElementSourcesRes {
        element: dto::ELEMENT_LABOR,
        element_label: dto::element_label(dto::ELEMENT_LABOR),
        rows,
        rows_total,
        amount_total: dto::amount_text(amount_total),
        collected_rows: totals.labor_rows,
        collected_amount: dto::amount_text(totals.labor_amount),
        pending_rows: gather.plan.to_insert.len(),
        pending_amount: dto::amount_text(pending_amount),
        purged_rows: gather.plan.stale.len(),
        purged_amount: dto::amount_text(purged_amount),
        uncollected_rows: gather.plan.uncollected.len(),
        uncollected_amount: dto::amount_text(uncollected_amount),
        after_amount: dto::amount_text(dto::after_amount(totals.labor_amount, purged_amount, pending_amount)),
    }
}

/// 费路出参（金额为 0 的行 `UNCOLLECTABLE`）。
fn overhead_sources_res(gather: &OverheadGather, totals: &ElementTotals) -> ElementSourcesRes {
    let already: std::collections::HashSet<_> = gather.plan.already.iter().cloned().collect();
    let pending: std::collections::HashSet<_> = gather.plan.to_insert.iter().map(OverheadLine::key).collect();
    let blocked: std::collections::HashMap<_, _> = gather
        .plan
        .uncollected
        .iter()
        .map(|row| ((row.source_type.clone(), row.source_id), row))
        .collect();
    let rows = gather
        .candidates
        .iter()
        .map(|line| {
            let (state, state_label) = dto::line_state(&line.key(), &already, &pending);
            let reason = blocked
                .get(&(line.source_type.clone(), line.source_id))
                .map(|row| match &row.hint {
                    Some(hint) => format!("{}；{}", row.reason, hint),
                    None => row.reason.clone(),
                });
            SourceLineRes {
                element: dto::ELEMENT_OVERHEAD,
                source_type: line.source_type.clone(),
                source_id: line.source_id.to_string(),
                source_no: line.source_no.clone(),
                occur_date: line.occur_date.to_string(),
                label: line.subject.clone(),
                qty: None,
                unit_price: None,
                hours: None,
                rate: None,
                material_id: None,
                material_code: None,
                material_name: None,
                account_code: Some(line.account_code.clone()),
                dept_name: line.dept_name.clone(),
                state,
                state_label,
                reason,
                amount: dto::amount_text(line.amount),
            }
        })
        .collect::<Vec<_>>();
    let rows_total = rows.len();
    let amount_total = gather.candidates.iter().map(|line| line.amount).sum();
    let pending_amount = gather.plan.inserted_amount();
    let purged_amount = gather.plan.stale_amount();
    let uncollected_amount: Decimal = gather.plan.uncollected.iter().map(|row| row.amount).sum();
    ElementSourcesRes {
        element: dto::ELEMENT_OVERHEAD,
        element_label: dto::element_label(dto::ELEMENT_OVERHEAD),
        rows,
        rows_total,
        amount_total: dto::amount_text(amount_total),
        collected_rows: totals.overhead_rows,
        collected_amount: dto::amount_text(totals.overhead_amount),
        pending_rows: gather.plan.to_insert.len(),
        pending_amount: dto::amount_text(pending_amount),
        purged_rows: gather.plan.stale.len(),
        purged_amount: dto::amount_text(purged_amount),
        uncollected_rows: gather.plan.uncollected.len(),
        uncollected_amount: dto::amount_text(uncollected_amount),
        after_amount: dto::amount_text(dto::after_amount(totals.overhead_amount, purged_amount, pending_amount)),
    }
}

// ==================== 执行归集 ====================

/// 把本批本期的料 / 工 / 费归集进 `biz_cost_collect`（一个事务，幂等）。
///
/// # 幂等
///
/// 同一批同一期间重复调用：第二次 `inserted = 0`、`skipped_existing = 候补数`，
/// 归集行的金额与 `updated_at` 一律不动（与 calc 各段同一个 `plan` 纯函数保证）。
///
/// # 异常情况
///
/// | 情况 | 出口 |
/// |---|---|
/// | 批次不存在 / 已删 / 不可见 | 404 |
/// | 期间缺失或非法 | 400 |
/// | 批次已关批 / 已作废 / 本期批次已冻结 | 409 + `66106`（点名批次号与状态，给修法） |
/// | 来源行账面数据非法 | 400 + `66064`（点名来源单号，一行都不写） |
/// | 查库 / 写库失败 | 500（事务回滚，无中间态） |
pub async fn collect(
    principal: &ScopedPrincipal,
    operator: Option<Uuid>,
    id_raw: &str,
    req: BatchCollectReq,
) -> std::result::Result<BatchCollectRes, ApiError> {
    let object_id = parse_id(id_raw)?;
    let period_code = dto::period_of(req.period_code.as_deref())?;

    let batch = model::find_visible_batch(principal, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询批次抬头"))?
        .ok_or_else(|| batch_not_found(id_raw))?;

    let period_batch = model::active_period_batch(db(), &period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询本期成本批次"))?;
    if let Some(block) = dto::block_of(&batch.object_no, &batch.status, period_batch.as_ref()) {
        return Err(dto::not_collectable(&block));
    }

    let (start, end) = cost_calc_material::period_bounds(&period_code).await?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启单批次归集事务"))?;

    let outcome = collect_in_tx(&tx, &batch, &period_code, start, end, object_id, operator).await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            // 事务随 tx drop 回滚；显式 rollback 只为把失败原因留在日志里
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "单批次归集回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交单批次归集事务"))?;
    Ok(outcome)
}

/// 事务内的归集主体（**不提交**，调用方负责 `commit` / `rollback`）。
async fn collect_in_tx(
    ex: &impl SqlExecutor,
    batch: &SheetBatchRes,
    period_code: &str,
    start: NaiveDate,
    end: NaiveDate,
    object_id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<BatchCollectRes, ApiError> {
    // 再确认一次批次状态：校验与写库之间可能有人关批 / 作废
    let status = model::object_status_in_tx(ex, object_id)
        .await
        .map_err(|error| utils::db_error(error, "事务内确认批次状态"))?
        .ok_or_else(|| batch_not_found(&object_id.to_string()))?;
    let period_batch = model::active_period_batch(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "事务内确认本期成本批次"))?;
    if let Some(block) = dto::block_of(&batch.object_no, &status, period_batch.as_ref()) {
        return Err(dto::not_collectable(&block));
    }

    let gathered = gather(ex, period_code, start, end, object_id).await?;

    // ---------- 料：先清过期、再写新行 ----------
    for row in &gathered.material.plan.stale {
        cost_collect_model::soft_delete_row(ex, row.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "清理过期的料归集行"))?;
    }
    let material_inserted =
        cost_collect_model::insert_material_lines(ex, period_code, &gathered.material.plan.to_insert, operator)
            .await
            .map_err(|error| utils::db_error(error, "写入料归集行"))?;

    // ---------- 工 ----------
    for row in &gathered.labor.plan.stale {
        cost_collect_model::soft_delete_row(ex, row.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "清理过期的工归集行"))?;
    }
    let labor_inserted =
        cost_collect_model::insert_labor_lines(ex, period_code, &gathered.labor.plan.to_insert, operator)
            .await
            .map_err(|error| utils::db_error(error, "写入工归集行"))?;

    // ---------- 费 ----------
    for row in &gathered.overhead.plan.stale {
        cost_collect_model::soft_delete_row(ex, row.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "清理过期的费归集行"))?;
    }
    let overhead_inserted = cost_overhead_collect_model::insert_overhead_lines(
        ex,
        period_code,
        &gathered.overhead.plan.to_insert,
        operator,
    )
    .await
    .map_err(|error| utils::db_error(error, "写入费归集行"))?;

    // ---------- 回查（写后真实值：汇报与前端合计都取这个数） ----------
    let totals = model::element_totals(ex, object_id, period_code)
        .await
        .map_err(|error| utils::db_error(error, "回查本批归集额"))?;

    let material = element_collect_res(
        dto::ELEMENT_MATERIAL,
        gathered.material.candidates.len(),
        &gathered.material.plan.already,
        usize::try_from(material_inserted).unwrap_or(gathered.material.plan.to_insert.len()),
        gathered.material.plan.stale.len(),
        gathered.material.plan.inserted_amount(),
        gathered.material.plan.stale_amount(),
        totals.material_rows,
        totals.material_amount,
        0,
        Decimal::ZERO,
    );
    let labor_uncollected_amount: Decimal = gathered
        .labor
        .plan
        .uncollected
        .iter()
        .map(|row| row.hours * row.rate)
        .sum();
    let labor = element_collect_res(
        dto::ELEMENT_LABOR,
        gathered.labor.candidates.len(),
        &gathered.labor.plan.already,
        usize::try_from(labor_inserted).unwrap_or(gathered.labor.plan.to_insert.len()),
        gathered.labor.plan.stale.len(),
        gathered.labor.plan.inserted_amount(),
        gathered.labor.plan.stale_amount(),
        totals.labor_rows,
        totals.labor_amount,
        gathered.labor.plan.uncollected.len(),
        labor_uncollected_amount,
    );
    let oh_uncollected_amount: Decimal = gathered.overhead.plan.uncollected.iter().map(|row| row.amount).sum();
    let overhead = element_collect_res(
        dto::ELEMENT_OVERHEAD,
        gathered.overhead.candidates.len(),
        &gathered.overhead.plan.already,
        usize::try_from(overhead_inserted).unwrap_or(gathered.overhead.plan.to_insert.len()),
        gathered.overhead.plan.stale.len(),
        gathered.overhead.plan.inserted_amount(),
        gathered.overhead.plan.stale_amount(),
        totals.overhead_rows,
        totals.overhead_amount,
        gathered.overhead.plan.uncollected.len(),
        oh_uncollected_amount,
    );

    let recalc_required = period_batch
        .as_ref()
        .is_some_and(|period| period.status == crate::business::erp::cost::cost_period_dto::STATUS_DRAFT);
    let message = collect_message(
        &material,
        &labor,
        &overhead,
        totals.total(),
        recalc_required,
        &period_batch,
    );

    Ok(BatchCollectRes {
        period_code: period_code.to_string(),
        batch: batch.clone(),
        period_batch: period_batch.as_ref().map(PeriodBatchRes::of),
        recalc_required,
        material,
        labor,
        overhead,
        totals: SheetAmounts::of(totals.material_amount, totals.labor_amount, totals.overhead_amount),
        message,
    })
}

/// 一路要素的执行结果装配（`inserted` 用写入原语返回的真实行数）。
#[allow(clippy::too_many_arguments)]
fn element_collect_res(
    element: &'static str,
    scanned: usize,
    already: &[collect_dto::CollectKey],
    inserted: usize,
    purged: usize,
    inserted_amount: Decimal,
    purged_amount: Decimal,
    collected_rows: i64,
    collected_amount: Decimal,
    uncollected_rows: usize,
    uncollected_amount: Decimal,
) -> ElementCollectRes {
    ElementCollectRes {
        element,
        element_label: dto::element_label(element),
        scanned,
        inserted,
        skipped_existing: already.len(),
        purged,
        inserted_amount: dto::amount_text(inserted_amount),
        purged_amount: dto::amount_text(purged_amount),
        collected_rows,
        collected_amount: dto::amount_text(collected_amount),
        uncollected_rows,
        uncollected_amount: dto::amount_text(uncollected_amount),
    }
}

/// 人话结论（前端 Toast 直出）。
fn collect_message(
    material: &ElementCollectRes,
    labor: &ElementCollectRes,
    overhead: &ElementCollectRes,
    total: Decimal,
    recalc_required: bool,
    period_batch: &Option<dto::PeriodBatchRef>,
) -> String {
    let inserted = material.inserted + labor.inserted + overhead.inserted;
    let purged = material.purged + labor.purged + overhead.purged;
    let uncollected = material.uncollected_rows + labor.uncollected_rows + overhead.uncollected_rows;
    let mut text = if inserted == 0 && purged == 0 {
        format!(
            "本批本期没有新的可归集行（料 {} / 工 {} / 费 {} 已全部归集过），合计 {} 元",
            material.skipped_existing,
            labor.skipped_existing,
            overhead.skipped_existing,
            dto::amount_text(total)
        )
    } else {
        format!(
            "已归集 {inserted} 笔（料 {}+工 {}+费 {}），清理过期 {purged} 笔；本批本期合计 {} 元",
            material.inserted,
            labor.inserted,
            overhead.inserted,
            dto::amount_text(total)
        )
    };
    if uncollected > 0 {
        text.push_str(&format!(
            "；另有 {uncollected} 笔来源不合归集口径（缺工资率 / 金额为 0），未写库，见明细里的原因"
        ));
    }
    if recalc_required && let Some(period) = period_batch {
        text.push_str(&format!(
            "；本期成本批次 {} 仍是草稿，成本计算单需重跑 calc 才跟上",
            period.batch_no
        ));
    }
    text
}

// ==================== 单测（不需要数据库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_param_garbage_is_404_not_400() {
        assert_eq!(parse_id("not-a-uuid").expect_err("非法 id").status, 404);
        assert_eq!(parse_id(&Uuid::nil().to_string()).expect("合法 id"), Uuid::nil());
    }

    #[test]
    fn collect_message_reports_noop_and_recalc_hint() {
        let material = element_collect_res(
            dto::ELEMENT_MATERIAL,
            2,
            &[],
            0,
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            2,
            Decimal::new(1500000, 4),
            0,
            Decimal::ZERO,
        );
        let labor = element_collect_res(
            dto::ELEMENT_LABOR,
            1,
            &[],
            0,
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            1,
            Decimal::new(300000, 4),
            1,
            Decimal::new(100000, 4),
        );
        let overhead = element_collect_res(
            dto::ELEMENT_OVERHEAD,
            0,
            &[],
            0,
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            0,
            Decimal::ZERO,
            0,
            Decimal::ZERO,
        );
        let period = Some(dto::PeriodBatchRef {
            batch_no: "CS-203107-0001".to_string(),
            status: "DRAFT".to_string(),
        });
        let message = collect_message(&material, &labor, &overhead, Decimal::new(1800000, 4), true, &period);
        assert!(message.contains("没有新的可归集行"), "{message}");
        assert!(message.contains("1 笔来源不合归集口径"), "{message}");
        assert!(message.contains("CS-203107-0001"), "{message}");
        assert!(message.contains("重跑 calc"), "{message}");
    }

    #[test]
    fn collect_message_counts_inserts_and_purges() {
        let material = element_collect_res(
            dto::ELEMENT_MATERIAL,
            3,
            &[],
            2,
            1,
            Decimal::new(1500000, 4),
            Decimal::new(100000, 4),
            3,
            Decimal::new(1400000, 4),
            0,
            Decimal::ZERO,
        );
        let labor = element_collect_res(
            dto::ELEMENT_LABOR,
            0,
            &[],
            0,
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            0,
            Decimal::ZERO,
            0,
            Decimal::ZERO,
        );
        let overhead = element_collect_res(
            dto::ELEMENT_OVERHEAD,
            1,
            &[],
            1,
            0,
            Decimal::new(200000, 4),
            Decimal::ZERO,
            1,
            Decimal::new(200000, 4),
            0,
            Decimal::ZERO,
        );
        let message = collect_message(&material, &labor, &overhead, Decimal::new(1600000, 4), false, &None);
        assert!(message.contains("已归集 3 笔"), "{message}");
        assert!(message.contains("清理过期 1 笔"), "{message}");
        assert!(!message.contains("重跑 calc"), "没有草稿批次时不该提重算：{message}");
    }
}

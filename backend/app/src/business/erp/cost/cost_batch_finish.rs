//! 分批法「完工批次结转（完工转出）」· 编排层（成本核算 · M5-04）。
//!
//! 两件事：
//!
//! | 动作 | 写库 | 说明 |
//! |---|---|---|
//! | [`preview`] 结转试算 | ❌ | 累计归集 / 已结转 / 本次可转多少 / 单位成本 / 为什么不能转 |
//! | [`finish`] 执行结转 | ✅ 一个事务 | 锁批次行 → 重算差额 → 写结转单 → 回查 |
//!
//! # 事务边界（一次调用一个事务）
//!
//! ```text
//! BEGIN
//!  ① SELECT biz_cost_object … FOR UPDATE          ← 串行点：同一批的结转不该并行
//!  ② 重算 累计归集 − 已结转                       ← 锁到手之后才算，锁前算的作废
//!  ③ 判定（闸门）→ 有拦截就整笔回滚并 409
//!  ④ 差额为 0 ⇒ 幂等空转（不写空单）
//!  ⑤ INSERT biz_cost_batch_finish
//! COMMIT
//! ⑥ 回查（用 preview 的同一套代码重新算一遍，出参不回显请求里的数字）
//! ```
//!
//! # 提交后没有会失败的账务动作
//!
//! 本卡**不出凭证、不建入库单**（理由见 `cost_batch_finish_dto` 模块头：ARCH-9 D-1 的
//! 「唯一出口」是期间结算，绕开会让 C-COST-2 / C-COST-3 恒红）。事务提交后只剩一次
//! 只读回查 —— 它失败也只是「写成功了但回查没读到」，重试即幂等（AGENTS 约定 11 的
//! 「提交后失败留死单」在这里没有发生面：没有后续状态推进、没有外部调用）。
//!
//! # 前置卫生四条为什么在事务外查
//!
//! 草稿工时 / 未归集领料 / 未过账领料 / 期间锁定这四条来自
//! `cost_object_model::close_facts`（与**关批**同一套判据，见 model 层模块头），它内部走
//! `db()` 而不是本事务的 `tx`：我们这时持有 `biz_cost_object` 的行锁，如果让它在**另一条
//! 连接**上再去读一次同一个对象行，就会自己等自己。所以这四条在开事务前取一次；
//! 期间内真的又插了草稿工时，下一次结转会算进差额、preview 也会照实报出来。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_batch_collect_dto::{self as collect_dto, PeriodBatchRes};
use crate::business::erp::cost::cost_batch_collect_model;
use crate::business::erp::cost::cost_batch_finish_dto::{
    self as dto, AmountsRes, BatchFinishPreviewRes, BatchFinishQuery, BatchFinishReq, BatchFinishRes, FinishFacts,
};
use crate::business::erp::cost::cost_batch_finish_model::{self as model, FinishInsert, ObjectSnapshot};
use crate::business::erp::cost::cost_object_dto;
use crate::business::erp::cost::cost_object_model;
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
             请先用 GET /business/erp/cost-collect-sheet?period_code=yyyy-MM 找到批次"
        ),
    )
}

/// 路径参数 → UUID（非法形状也是 404：这个位置本来就该是 id）。
fn parse_id(raw: &str) -> std::result::Result<Uuid, ApiError> {
    Uuid::parse_str(raw.trim()).map_err(|_| batch_not_found(raw))
}

// ==================== 事实采集 ====================

/// 组装判定事实（试算与执行共用**同一份**装配，避免「预览说能转、点了不能转」）。
///
/// `hygiene` 是事务外取到的四条卫生事实（草稿工时 / 未归集 / 未过账 / 期间锁定）。
async fn gather(
    ex: &impl SqlExecutor,
    object_id: Uuid,
    snapshot: &ObjectSnapshot,
    period_batch: Option<&collect_dto::PeriodBatchRef>,
    hygiene: &cost_object_dto::CloseFacts,
) -> std::result::Result<(FinishFacts, model::Transferred), ApiError> {
    let (collected, _rows) = model::accumulated_totals(ex, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询本批累计归集"))?;
    let transferred = model::transferred_totals(ex, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询本批已结转额"))?;
    let settled_items = model::settled_items(ex, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询本批已结算的计算单"))?;

    let facts = FinishFacts {
        object_status: snapshot.status.clone(),
        wip_qty: snapshot.wip_qty,
        completed_qty: snapshot.completed_qty,
        collected,
        transferred: transferred.amounts,
        settled_items,
        draft_labor: hygiene.draft_labor,
        uncollected_stock_out: hygiene.uncollected_stock_out,
        unposted_stock_out: hygiene.unposted_stock_out,
        locked_periods: hygiene.locked_periods.clone(),
        period_batch: period_batch.cloned(),
    };
    Ok((facts, transferred))
}

/// 出参组装（**只读**；`finish` 写完之后也走这里回查）。
async fn build_preview(
    principal: &ScopedPrincipal,
    object_id: Uuid,
    id_raw: &str,
    period_code: &str,
) -> std::result::Result<BatchFinishPreviewRes, ApiError> {
    let batch = model::find_visible_batch(principal, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询批次抬头"))?
        .ok_or_else(|| batch_not_found(id_raw))?;
    let snapshot = model::read_object(db(), object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象"))?
        .ok_or_else(|| batch_not_found(id_raw))?;
    let period_batch = cost_batch_collect_model::active_period_batch(db(), period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询结转期间的成本批次"))?;
    // 关批同一套卫生判据（`force_wip = true` 只为**压掉它的在制拦截**：本卡的在制判定
    // 是「分批法不按约当分摊」，报文与修法都不同，由 `evaluate_finish` 自己出）
    let hygiene = cost_object_model::close_facts(object_id, snapshot.wip_qty, true)
        .await
        .map_err(|error| utils::db_error(error, "采集结转前置事实"))?;

    let (facts, transferred) = gather(db(), object_id, &snapshot, period_batch.as_ref(), &hygiene).await?;
    let blockers = dto::evaluate_finish(&facts);
    let plan = dto::plan_transfer(
        &facts.collected,
        &transferred.amounts,
        facts.completed_qty,
        transferred.times,
    );
    let history = model::history(db(), object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询历史结转单"))?;

    Ok(BatchFinishPreviewRes {
        batch,
        period_code: period_code.to_string(),
        period_batch: period_batch.as_ref().map(PeriodBatchRes::of),
        collected: AmountsRes::of(&facts.collected),
        transferred: AmountsRes::of(&transferred.amounts),
        pending: AmountsRes::of(&plan.transfer),
        finished_qty: dto::quantity_text(facts.completed_qty),
        unit_cost: dto::unit_cost_text(plan.unit_cost),
        cumulative_unit_cost: dto::unit_cost_text(plan.cumulative_unit_cost),
        transferable: blockers.is_empty() && plan.transferable,
        transfer_no: plan.transfer_no,
        settled_in_books: facts.settled_items > 0,
        history,
        warnings: warnings_of(&facts, &plan, period_batch.as_ref()),
        blockers,
    })
}

/// 提醒（不拦，但用户必须知道的口径边界）。
fn warnings_of(
    facts: &FinishFacts,
    plan: &dto::TransferPlan,
    period_batch: Option<&collect_dto::PeriodBatchRef>,
) -> Vec<String> {
    let mut warnings = vec![
        "本接口只出成本工作底稿（biz_cost_batch_finish）：不出凭证、不建完工入库单。\
         账务出口是期间结算（ARCH-9 D-1），工作底稿不参与 4101 / 4001 余额与 C-COST-2 勾稽"
            .to_string(),
    ];
    if plan.transferable && facts.object_status == cost_object_dto::STATUS_OPEN {
        warnings.push(
            "结转后请关批（POST /business/erp/cost-object/{id}/close）：\
             批还开着就还能领料报工，新增成本会变成新的未结转差额"
                .to_string(),
        );
    }
    if facts.object_status == cost_object_dto::STATUS_CLOSED {
        warnings.push(
            "本批已关批：本次结转只补「关批后迟到的真成本」（迟过账的领料 / 迟确认的工时），\
             不会再变动批次状态"
                .to_string(),
        );
    }
    if facts.collected.is_zero() && plan.transferable {
        warnings.push("累计归集为 0 但仍有可转差额：请核对归集行是否被手工改过".to_string());
    }
    if let Some(period) = period_batch
        && period.status == cost_period_dto::STATUS_DRAFT
    {
        warnings.push(
            "结转期间已有 DRAFT 成本批次：若之后走期间结算，本批会被再结一次\
             （账务口径以结算为准，工作底稿届时只是历史留痕）"
                .to_string(),
        );
    }
    warnings
}

// ==================== 试算 ====================

/// 结转试算（**只读**；被拦也照常返回数，`transferable = false` + 拦截清单）。
///
/// 与 `finish` 的选择一致：`finish` 遇到拦截必须 409（写库才拦），
/// 试算永远给数（看数不拦）。
pub async fn preview(
    principal: &ScopedPrincipal,
    id_raw: &str,
    query: BatchFinishQuery,
) -> std::result::Result<BatchFinishPreviewRes, ApiError> {
    let object_id = parse_id(id_raw)?;
    let period_code = dto::period_of(query.period_code.as_deref())?;
    build_preview(principal, object_id, id_raw, &period_code).await
}

// ==================== 执行结转 ====================

/// 执行完工批次结转（一个事务，幂等）。
///
/// # 出口
///
/// | 情况 | 出口 |
/// |---|---|
/// | 批次不存在 / 已删 / 不可见 | 404 |
/// | 期间缺失或非法 | 400 |
/// | 有拦截（作废 / 已结算 / 还有在制 / 没产出 / 没成本 / 卫生四条 / 期间冻结） | 409 + `66107` |
/// | 差额为 0（重放 / 没有新成本） | **200 幂等空转**（`inserted = false`，不写空单） |
/// | 正常 | 200（写一张结转单，`inserted = true`） |
pub async fn finish(
    principal: &ScopedPrincipal,
    operator: Option<Uuid>,
    id_raw: &str,
    req: BatchFinishReq,
) -> std::result::Result<BatchFinishRes, ApiError> {
    let object_id = parse_id(id_raw)?;
    let period_code = dto::period_of(req.period_code.as_deref())?;
    let remark = dto::clean_remark(req.remark.as_deref())?;

    // 可见性探针（不可见直接 404，不做任何写）
    let _batch = model::find_visible_batch(principal, object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询批次抬头"))?
        .ok_or_else(|| batch_not_found(id_raw))?;

    // 事务外取卫生四条（理由见模块头：close_facts 走 db()，事务内再调会自己等自己的行锁）
    let pre_snapshot = model::read_object(db(), object_id)
        .await
        .map_err(|error| utils::db_error(error, "查询成本对象"))?
        .ok_or_else(|| batch_not_found(id_raw))?;
    let hygiene = cost_object_model::close_facts(object_id, pre_snapshot.wip_qty, true)
        .await
        .map_err(|error| utils::db_error(error, "采集结转前置事实"))?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启完工结转事务"))?;

    let outcome = finish_in_tx(
        &tx,
        operator,
        id_raw,
        object_id,
        &period_code,
        remark.as_deref(),
        &hygiene,
    )
    .await;
    let inserted = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "完工结转回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交完工结转事务"))?;

    // 回查：写完之后的口径（出参不回显请求里的数字）
    let preview = build_preview(principal, object_id, id_raw, &period_code).await?;
    let (inserted_flag, finish_id, message, transfer) = match inserted {
        Some((finish_id, transfer, unit_cost, transfer_no)) => (
            true,
            Some(finish_id.to_string()),
            format!(
                "已结转第 {} 次：料 {} / 工 {} / 费 {}，合计 {}，本次单位成本 {}",
                transfer_no,
                dto::amount_text(transfer.material),
                dto::amount_text(transfer.labor),
                dto::amount_text(transfer.overhead),
                dto::amount_text(transfer.material + transfer.labor + transfer.overhead),
                dto::unit_cost_text(unit_cost),
            ),
            transfer,
        ),
        None => (
            false,
            None,
            "无可结转差额（累计归集已被历史结转单覆盖）：本次没有写新结转单".to_string(),
            dto::Amounts::default(),
        ),
    };

    Ok(BatchFinishRes {
        inserted: inserted_flag,
        message,
        finish_id,
        transfer: AmountsRes::of(&transfer),
        remaining: preview.pending.clone(),
        preview,
    })
}

/// 事务体：锁行 → 重算 → 判定 → 写单。
///
/// 返回 `Some((结转单 id, 本次结转金额))` = 真的写了；`None` = 差额为 0 的幂等空转。
#[allow(clippy::too_many_arguments)]
async fn finish_in_tx(
    tx: &impl SqlExecutor,
    operator: Option<Uuid>,
    id_raw: &str,
    object_id: Uuid,
    period_code: &str,
    remark: Option<&str>,
    hygiene: &cost_object_dto::CloseFacts,
) -> std::result::Result<Option<(Uuid, dto::Amounts, Decimal, i32)>, ApiError> {
    let snapshot = model::lock_object(tx, object_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定成本对象"))?
        .ok_or_else(|| batch_not_found(id_raw))?;

    let period_batch = cost_batch_collect_model::active_period_batch(tx, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询结转期间的成本批次"))?;
    let (facts, transferred) = gather(tx, object_id, &snapshot, period_batch.as_ref(), hygiene).await?;

    let blockers = dto::evaluate_finish(&facts);
    if !blockers.is_empty() {
        return Err(dto::not_transferable(&snapshot.object_no, &blockers));
    }

    let plan = dto::plan_transfer(
        &facts.collected,
        &transferred.amounts,
        facts.completed_qty,
        transferred.times,
    );
    if !plan.transferable {
        return Ok(None);
    }

    let remark = remark.map(str::to_string).or_else(|| {
        Some(format!(
            "分批法完工结转：完工 {} 件，结算差额（累计归集 {} − 已结转 {}）",
            dto::quantity_text(plan.finished_qty),
            dto::amount_text(facts.collected.total()),
            dto::amount_text(transferred.amounts.total()),
        ))
    });
    let id = model::insert_finish(
        tx,
        &FinishInsert {
            object_id,
            object_no: &snapshot.object_no,
            period_code,
            transfer_seq: plan.transfer_no,
            product_id: snapshot.product_id,
            lot_no: snapshot.lot_no.as_deref(),
            finished_qty: plan.finished_qty,
            unit_cost: plan.unit_cost,
            collected: facts.collected,
            transfer: plan.transfer,
            remark: remark.as_deref(),
            operator,
        },
    )
    .await
    .map_err(|error| utils::db_error(error, "写完工结转单"))?;

    Ok(Some((id, plan.transfer, plan.unit_cost, plan.transfer_no)))
}

/// 进度自检用：本次差额是否为 0 的判定（供真库用例与前端提示共用同一份口径）。
pub fn is_noop(plan: &dto::TransferPlan) -> bool {
    !plan.transferable && plan.transfer.total() == Decimal::ZERO
}

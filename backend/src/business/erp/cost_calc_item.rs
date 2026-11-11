//! calc 第 6 段：**约当产量与成本计算单**（ARCH-9 §4.6 · B2-11）。
//!
//! ```text
//! ① 闸：该期间活跃批次必须可重算（DRAFT / CANCELLED / REVERSED），且没有已结算的计算单
//! ② 清当期：删本批次的 CALC 计算单（整删重灌，SETTLED 行不动）
//! ③ 取对象：当期有归集行 ∪ 上期已结算还有期末在制（`OPEN` / `CLOSED`）
//! ④ 每对象取期初：上期已结算计算单 → 期初在制数量 / 成本 + 三要素还原；
//!    上期被红冲 ⇒ 409（设计 §4.6.3）
//! ⑤ 算：约当产量 → 单位成本 → 完工转出 → 期末在制（差额倒挤）
//! ⑥ 落库 + 回查：从库里读回来逐行复算守恒式，行数与合计都要对上
//! ```
//!
//! # 没有 HTTP 入口（与第 1–5 段同构）
//!
//! 设计 §5.2 的接口清单里只有 `POST /business/erp/cost-period/calc` 一个入口（#27，属 calc 编排卡）。
//! 本段交付 [`calc_items`] / [`calc_items_in_tx`] 两个函数，由编排方在同一次「逐段清 + 重灌」里调用。
//! 单独暴露一个「只算计算单」的接口会让期间停在半算状态（计算单有了、结算没有）。
//!
//! # 批次必须已存在（`period_id` 是外键）
//!
//! `biz_cost_period_item.period_id NOT NULL REFERENCES biz_cost_period(id)`（V0373）⇒
//! 没有批次行就无处安放计算单，报 409 + `66071` 并指向「先建批次」。批次号的号段
//! （`CS-yyyyMM-0001`）与建批次动作属于编排卡 / B3-1，本段不越界造批次。
//!
//! # 事务
//!
//! ①–⑥ 全在一个事务里：中途任何一条 409 都回滚，旧计算单原样保留（不会出现
//! 「旧的删了、新的只写了一半」＝计算单缺对象的半算状态）。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_calc_overhead;
use crate::business::erp::cost_period_item_dto::{
    self as dto, BegCost, ItemCalc, ItemInput, ItemRunOutcome, OutputInput, PriorItem, SkippedObject,
};
use crate::business::erp::cost_period_item_model::{self as model, ItemRow};
use crate::utils;

/// 算当期全部成本对象的计算单（calc 第 6 段，自开事务）。
///
/// 返回 [`ItemRunOutcome`]：逐对象的计算单 + 合计 + 跳过的对象 + 清掉的旧行数。
/// 重复调用结果收敛（第二次先删 CALC 行再重灌，行数不变、金额不变）。
///
/// # 异常情况
///
/// - 期间编码非法 ⇒ 400（与料 / 工 / 费段同一文案）
/// - 该期间没有活跃批次 ⇒ 409 + `66071`
/// - 批次已提交 / 审批 / 结算，或已有 `SETTLED` 计算单 ⇒ 409 + `66070`
/// - 上期批次被红冲 ⇒ 409 + `66069`（设计 §4.6.3）
/// - 产量 / 进度 / 投料方式不合法 ⇒ 409 + `66067`；有完工无材料 ⇒ 409 + `66068`
/// - 守恒断言 / 落库后回查不一致 ⇒ 500（`INTERNAL_ERROR`）
/// - 查库 / 写库失败 ⇒ 500
pub async fn calc_items(period_code: &str, operator: Option<Uuid>) -> std::result::Result<ItemRunOutcome, ApiError> {
    cost_calc_overhead::ensure_period_code(period_code)?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启成本计算单事务"))?;

    let outcome = match calc_items_in_tx(&tx, period_code, operator).await {
        Ok(outcome) => outcome,
        Err(error) => {
            // 事务随 tx drop 回滚；显式 rollback 只是把原因留在日志里
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "成本计算单回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交成本计算单事务"))?;
    tracing::info!(target: "cost", "{}", outcome.summary_line());
    Ok(outcome)
}

/// 事务内的计算（供 calc 编排方在自己已有的事务里复用）。
///
/// **不提交、不回滚**：调用方负责。期间编码校验也不在这里做（调用方已做）。
///
/// # 调用顺序契约（编排方必须遵守）
///
/// 第 ⑤ 步（分摊）必须先跑完：制造费用是通过 `ALLOC` 归集行落到对象上的，
/// 本段直接把 `element='OVERHEAD'` 的归集行求和，分摊没跑就等于「制造费用没进对象」。
pub async fn calc_items_in_tx(
    ex: &impl SqlExecutor,
    period_code: &str,
    _operator: Option<Uuid>,
) -> std::result::Result<ItemRunOutcome, ApiError> {
    // ---------- ① 闸 ----------
    let batch = model::active_batch(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询成本批次"))?
        .ok_or_else(|| dto::period_batch_missing(period_code))?;
    if !batch.redoable() {
        return Err(dto::period_locked(period_code, &batch.batch_no, &batch.status));
    }
    let settled = model::settled_count(ex, batch.id)
        .await
        .map_err(|error| utils::db_error(error, "查询已结算计算单"))?;
    if settled > 0 {
        // 行状态是 SETTLED 而批次状态还停在 DRAFT/REVERSED：宁可拦住也不改写已入账金额
        return Err(dto::period_locked(period_code, &batch.batch_no, "计算单已结算"));
    }

    // ---------- ② 清当期（只清 CALC） ----------
    let purged = model::purge_calc_rows(ex, batch.id)
        .await
        .map_err(|error| utils::db_error(error, "清理当期旧计算单"))?;
    if purged > 0 {
        tracing::info!(target: "cost", period_code, purged, "重算计算单：已清掉当期旧行");
    }

    // ---------- ③ 取对象 ----------
    let objects = model::objects_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询本期成本对象"))?;

    let mut items: Vec<ItemCalc> = Vec::with_capacity(objects.len());
    let mut skipped: Vec<SkippedObject> = Vec::new();

    for object in &objects {
        // ---------- ④ 期初在制（上期已结算计算单结转） ----------
        let reversed = model::reversed_prior_periods(ex, object.cost_object_id, period_code)
            .await
            .map_err(|error| utils::db_error(error, "查询上期红冲批次"))?;
        if !reversed.is_empty() {
            return Err(dto::prior_period_reversed(&object.object_no, &reversed));
        }

        let prior_row = model::prior_settled_item(ex, object.cost_object_id, period_code)
            .await
            .map_err(|error| utils::db_error(error, "查询上期计算单"))?;
        let prior_completed = model::prior_completed_qty(ex, object.cost_object_id, period_code)
            .await
            .map_err(|error| utils::db_error(error, "汇总前期完工数量"))?;

        let (beg, prior_wip_qty) = match &prior_row {
            None => (BegCost::default(), rust_decimal_zero()),
            Some(prior) => (
                dto::beg_cost_of(&PriorItem {
                    period_code: prior.period_code.clone(),
                    wip_end_qty: prior.wip_end_qty,
                    wip_end_cost: prior.wip_end_cost,
                    completion_pct: prior.completion_pct,
                    unit_material: prior.unit_material,
                    unit_labor: prior.unit_labor,
                    unit_overhead: prior.unit_overhead,
                })?,
                prior.wip_end_qty,
            ),
        };

        // ---------- ⑤ 算 ----------
        let calc = dto::compose(&ItemInput {
            period_code: period_code.to_string(),
            cost_object_id: object.cost_object_id,
            object_no: object.object_no.clone(),
            product_id: object.product_id,
            output: OutputInput {
                cumulative_completed_qty: object.cumulative_completed_qty,
                prior_completed_qty: prior_completed,
                wip_end_qty: object.wip_qty,
                completion_pct: object.completion_pct,
                prior_wip_qty,
                material_input_mode: object.material_input_mode.clone(),
            },
            beg,
            mat_cur: object.mat_cur,
            labor_cur: object.labor_cur,
            oh_cur: object.oh_cur,
        })?;

        if calc.is_empty() {
            skipped.push(SkippedObject {
                object_no: object.object_no.clone(),
                reason: format!("期间 {period_code} 既无投入、也无完工与期末在制（期初在制也为 0），不写 0 行"),
            });
            continue;
        }

        // ---------- ⑥ 落库 ----------
        let row = ItemRow {
            id: Uuid::now_v7(),
            period_id: batch.id,
            cost_object_id: calc.cost_object_id,
            product_id: calc.product_id,
            wip_beg_qty: calc.wip_beg_qty,
            wip_beg_cost: calc.wip_beg_cost,
            completed_qty: calc.completed_qty,
            wip_end_qty: calc.wip_end_qty,
            completion_pct: calc.completion_pct,
            equiv_material: calc.equiv_material,
            equiv_conversion: calc.equiv_conversion,
            mat_cur: calc.mat_cur,
            labor_cur: calc.labor_cur,
            oh_cur: calc.oh_cur,
            unit_material: calc.unit_material,
            unit_labor: calc.unit_labor,
            unit_overhead: calc.unit_overhead,
            finished_cost: calc.finished_cost,
            wip_end_cost: calc.wip_end_cost,
            unit_finished_cost: calc.unit_finished_cost,
            status: "CALC".to_string(),
        };
        model::insert_item(ex, &row)
            .await
            .map_err(|error| utils::db_error(error, "写入成本计算单"))?;
        tracing::debug!(target: "cost", "{}", calc.summary_line());
        items.push(calc);
    }

    // ---------- ⑥' 回查（库是唯一真相） ----------
    let stored = model::items_of_period(ex, batch.id)
        .await
        .map_err(|error| utils::db_error(error, "回查当期计算单"))?;
    verify_readback(period_code, &items, &stored)?;

    let outcome = ItemRunOutcome {
        period_code: period_code.to_string(),
        batch_no: batch.batch_no,
        total_material: items.iter().map(|item| item.mat_cur).sum(),
        total_labor: items.iter().map(|item| item.labor_cur).sum(),
        total_overhead: items.iter().map(|item| item.oh_cur).sum(),
        total_cost: items.iter().map(ItemCalc::input_total).sum(),
        total_finished: items.iter().map(|item| item.finished_cost).sum(),
        total_wip: items.iter().map(|item| item.wip_end_cost).sum(),
        items,
        skipped,
        purged: i64::try_from(purged).unwrap_or(i64::MAX),
    };
    Ok(outcome)
}

/// 落库后回查：行数、恒等式、合计三项都要对上（写库丢行 / 标度被截断都会在这里现形）。
fn verify_readback(period_code: &str, expected: &[ItemCalc], stored: &[ItemRow]) -> std::result::Result<(), ApiError> {
    if stored.len() != expected.len() {
        return Err(dto::readback_broken(
            period_code,
            &format!("内存里算出 {} 行，库里只有 {} 行", expected.len(), stored.len()),
        ));
    }
    for row in stored {
        // 库层 `ck_biz_cost_item_cons` 已经拦了一次；这里读回来的值必须自己也能立住
        if row.outflow_total() != row.inflow_total() {
            return Err(dto::readback_broken(
                period_code,
                &format!(
                    "对象 {} 的行不满足守恒式：完工 {} + 在制 {} ≠ 期初 {} + 投入 {}（料 {} + 工 {} + 费 {}）",
                    row.cost_object_id,
                    dto::fmt(row.finished_cost, 4),
                    dto::fmt(row.wip_end_cost, 4),
                    dto::fmt(row.wip_beg_cost, 4),
                    dto::fmt(row.mat_cur + row.labor_cur + row.oh_cur, 4),
                    dto::fmt(row.mat_cur, 4),
                    dto::fmt(row.labor_cur, 4),
                    dto::fmt(row.oh_cur, 4)
                ),
            ));
        }
    }
    let stored_finished: nik::prelude::rust_decimal::Decimal = stored.iter().map(|row| row.finished_cost).sum();
    let expected_finished: nik::prelude::rust_decimal::Decimal = expected.iter().map(|item| item.finished_cost).sum();
    if stored_finished != expected_finished {
        return Err(dto::readback_broken(
            period_code,
            &format!(
                "完工转出合计不一致：内存 {}，库里 {}",
                dto::fmt(expected_finished, 4),
                dto::fmt(stored_finished, 4)
            ),
        ));
    }
    Ok(())
}

/// `Decimal::ZERO`（少一行 import）。
fn rust_decimal_zero() -> nik::prelude::rust_decimal::Decimal {
    nik::prelude::rust_decimal::Decimal::ZERO
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    /// 非法期间在**开事务之前**就被拦下（不占连接、不打库、不留半个事务）。
    #[tokio::test]
    async fn bad_period_is_rejected_before_any_query() {
        for bad in ["2026-13", "2026-00", "202611", "", "2026-1", "2026-11-01"] {
            let error = calc_items(bad, None).await.expect_err("非法期间必须 400");
            assert_eq!(
                error.status, 400,
                "「{bad}」应 400，实际 {} {}",
                error.status, error.msg
            );
        }
    }

    /// 回查：行数不等 ⇒ 500（写库丢行是内部错误，不是用户数据问题）。
    #[test]
    fn readback_rejects_row_count_mismatch() {
        let items = vec![sample_calc()];
        let error = verify_readback("2026-11", &items, &[]).expect_err("库里 0 行必须报错");
        assert_eq!(error.status, 500);
        assert!(error.msg.contains("1 行"), "{}", error.msg);
    }

    /// 回查：库里的行不满足守恒式 ⇒ 500（标度截断 / 写库被改写都会这样）。
    #[test]
    fn readback_rejects_broken_identity() {
        let item = sample_calc();
        let mut row = ItemRow {
            id: Uuid::nil(),
            period_id: Uuid::nil(),
            cost_object_id: item.cost_object_id,
            product_id: item.product_id,
            wip_beg_qty: item.wip_beg_qty,
            wip_beg_cost: item.wip_beg_cost,
            completed_qty: item.completed_qty,
            wip_end_qty: item.wip_end_qty,
            completion_pct: item.completion_pct,
            equiv_material: item.equiv_material,
            equiv_conversion: item.equiv_conversion,
            mat_cur: item.mat_cur,
            labor_cur: item.labor_cur,
            oh_cur: item.oh_cur,
            unit_material: item.unit_material,
            unit_labor: item.unit_labor,
            unit_overhead: item.unit_overhead,
            finished_cost: item.finished_cost,
            wip_end_cost: item.wip_end_cost + dec("0.0001"),
            unit_finished_cost: item.unit_finished_cost,
            status: "CALC".to_string(),
        };
        let error = verify_readback("2026-11", std::slice::from_ref(&item), std::slice::from_ref(&row))
            .expect_err("多 0.0001 的在制必须被回查拦住");
        assert_eq!(error.status, 500);
        assert!(error.msg.contains("不满足守恒式"), "{}", error.msg);

        // 修回一致后应当通过
        row.wip_end_cost = item.wip_end_cost;
        verify_readback("2026-11", std::slice::from_ref(&item), std::slice::from_ref(&row))
            .expect("一致的数据必须通过回查");
    }

    /// 一份自洽的计算单（算例 A 的数字，纯内存）。
    fn sample_calc() -> ItemCalc {
        dto::compose(&ItemInput {
            period_code: "2026-11".to_string(),
            cost_object_id: Uuid::from_u128(7),
            object_no: "CB-20261110-000001".to_string(),
            product_id: Uuid::from_u128(8),
            output: OutputInput {
                cumulative_completed_qty: dec("800"),
                prior_completed_qty: Decimal::ZERO,
                wip_end_qty: dec("200"),
                completion_pct: dec("50"),
                prior_wip_qty: Decimal::ZERO,
                material_input_mode: dto::INPUT_MODE_ONE_TIME.to_string(),
            },
            beg: BegCost::default(),
            mat_cur: dec("19000"),
            labor_cur: dec("4000"),
            oh_cur: dec("4000"),
        })
        .expect("算例 A")
    }
}

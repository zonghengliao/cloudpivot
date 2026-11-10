//! calc 第 5 段：**分摊计算**（ARCH-9 §4.4.3 · B2-10）。
//!
//! ```text
//! ① 闸：该期间批次状态必须可重算（DRAFT / CANCELLED / REVERSED）     → 否则 409 + 66066
//! ② 清当期：先删 ALLOC 归集行，再删分摊结果（§4.4.5 整删重灌）
//! ③ 选规则版本：每 (车间, 科目) 族取覆盖该期间且生效最晚的一条
//! ④ 池：Σ 当期池行（element=OVERHEAD、不挂对象、三类来源）按 (车间, 科目)
//! ⑤ 基数：四种标准，只统计「参与分摊」的对象（§4.4.3 判据）
//! ⑥ 分摊：最大余数法（dto::allocate）+ **逐规则守恒断言**（Σ alloc = pool）
//! ⑦ 落库：biz_cost_alloc_result 一行 + biz_cost_collect(ALLOC) 一行（同一事务）
//! ⑧ 回查：从库里按 (车间, 科目) 汇总分摊额，必须逐组等于本次要摊的池
//! ```
//!
//! # 没有 HTTP 入口（与第 1–4 段同构）
//!
//! 设计 §5.2 的接口清单里只有 `POST /business/erp/cost-period/calc` 一个入口
//! （#27，属 calc 编排卡）。本函数是**第 5 段**，由编排方在同一次「清当期 + 重灌」里调用。
//! 单独暴露一个「只摊不结算」的接口会让期间停在半算状态（分摊有了、计算单没有）。
//!
//! # 调用顺序契约（编排方必须遵守）
//!
//! 第 5 段**必须**在第 4 段（池与 4101 对账，
//! [`cost_calc_reconcile::reconcile_in_tx`](crate::business::erp::cost_calc_reconcile::reconcile_in_tx)）
//! **之后**调用：账上多出来的费用（旁路记账）如果先摊下去，错的不是成本而是全部存货。
//! 本段不重复跑对账（编排方一次事务里只该跑一次），但把池合计放进
//! [`AllocRunOutcome::total_pool`] 供编排方与对账结果互校。
//!
//! # 边界行为（卡面验收项，三条都写死）
//!
//! | 场景 | 行为 | 为什么 |
//! |---|---|---|
//! | 池 = 0（或当期没这个科目的池行） | **跳过**该规则，进 [`AllocRunOutcome::zero_pool`]，不写任何行 | 没有钱可摊，写一堆 0 元结果只会让计算单变脏 |
//! | 池 > 0 且当期无在产对象 | **409 + `66062`** | 设计 §4.4.3：不许「挂池里留到下期」（那正是上期漏摊的来源） |
//! | 池 > 0 且该标准基数合计 = 0 | **409 + `66063`** | 除零不可能守恒；按 0 摊会让 4101 清不掉 |
//!
//! 池 > 0 但没有规则覆盖 → 不报错，进 [`AllocRunOutcome::uncovered`]（人话原因 + 怎么修）。
//! 它和上面两条不同：那是**配置缺失**（没配规则），用户补一条规则重算即可，
//! 不需要先修单据；由未归集清单（B2-12）在界面上列出来。
//!
//! # 事务
//!
//! ①–⑧ 全在一个事务里：中途失败一行都不写，期间不会停在「删了旧结果、新结果没进」的空档
//! （那是标准的死单形态 —— 计算单会读到 0 分摊）。

use std::collections::HashMap;

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_alloc_calc_dto::{
    self as dto, AllocRunOutcome, RuleOutcome, UncoveredPool, ZeroPoolRule,
};
use crate::business::erp::cost_alloc_result_model::{self as result_model, AllocCollectRow, AllocResultRow};
use crate::business::erp::cost_alloc_rule_dto::{self as rule_dto, AllocObject, ObjectBasis};
use crate::business::erp::cost_alloc_rule_model::{
    self as rule_model, CostAllocRuleRow, LaborBasisRow, ObjectAmountRow, ParticipantRow,
};
use crate::business::erp::cost_calc_overhead;
use crate::business::erp::cost_overhead_collect_dto;
use crate::business::erp::cost_overhead_collect_model;
use crate::business::erp::ledger;
use crate::utils;

/// 分摊当期全部制造费用（calc 第 5 段，自开事务）。
///
/// 返回 [`AllocRunOutcome`]：逐规则逐对象的分摊额 + 池为 0 的跳过清单 + 无规则覆盖的池 +
/// 落库行数 + 清掉的旧行数。重复调用结果收敛（第二次先整删再重灌，行数不变、金额不变）。
///
/// # 异常情况
///
/// - 期间编码非法 ⇒ 400（与料 / 工 / 费段同一文案）
/// - 期间批次已提交 / 审批 / 结算 ⇒ 409 + `66066`（报文带批次号，指向「撤回 / 红冲」）
/// - 池 > 0 但当期无在产对象 ⇒ 409 + `66062`（点名车间、科目、金额）
/// - 池 > 0 但该标准基数合计 0 ⇒ 409 + `66063`（点名标准、缺什么数据）
/// - 守恒断言 / 落库后回查不一致 ⇒ 500（`INTERNAL_ERROR`，见 `cost_alloc_calc_dto` 模块头）
/// - 查库 / 写库失败 ⇒ 500
pub async fn allocate_overhead(
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<AllocRunOutcome, ApiError> {
    cost_calc_overhead::ensure_period_code(period_code)?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启分摊计算事务"))?;

    let outcome = match allocate_overhead_in_tx(&tx, period_code, operator).await {
        Ok(outcome) => outcome,
        Err(error) => {
            // 事务随 tx drop 回滚；显式 rollback 只是把原因留在日志里
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "分摊计算回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交分摊计算事务"))?;
    tracing::info!(target: "cost", "{}", outcome.summary_line());
    Ok(outcome)
}

/// 事务内的分摊计算（供 calc 编排方在自己已有的事务里复用）。
///
/// **不提交、不回滚**：调用方负责。期间编码校验也不在这里做（调用方已做，见
/// [`cost_calc_overhead::ensure_period_code`]）。
pub async fn allocate_overhead_in_tx(
    ex: &impl SqlExecutor,
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<AllocRunOutcome, ApiError> {
    let (period_from, period_to) =
        ledger::month_range(period_code).ok_or_else(|| cost_overhead_collect_dto::period_invalid(period_code))?;

    // ---------- ① 闸：已提交/结算的批次不许重算（§4.4.5） ----------
    if let Some(state) = result_model::period_state(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询成本批次状态"))?
        && !state.redoable()
    {
        return Err(dto::period_locked(period_code, &state.batch_no, &state.status));
    }

    // ---------- ② 清当期（先删 ALLOC 归集行，再删结果行） ----------
    let (purged_collect, purged_results) = result_model::purge_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "清理当期旧分摊结果"))?;
    if purged_results > 0 || purged_collect > 0 {
        tracing::info!(
            target: "cost",
            period_code,
            purged_results,
            purged_collect,
            "重算分摊：已清掉当期旧结果"
        );
    }

    // ---------- ③ 选规则版本 ----------
    let candidates = rule_model::active_rules_in(ex)
        .await
        .map_err(|error| utils::db_error(error, "查询启用的分摊规则"))?;
    let selected = rule_model::select_versions(&candidates, period_from, period_to);

    // ---------- ④ 池 ----------
    let pools = cost_overhead_collect_model::pool_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "汇总当期车间费用池"))?;
    let pool_map: HashMap<(Uuid, String), nik::prelude::rust_decimal::Decimal> = pools
        .iter()
        .map(|row| ((row.dept_id, row.account_code.clone()), row.amount))
        .collect();

    // ---------- ⑤ 基数数据源（每期间一次 / 每车间一次，避免 N+1） ----------
    let material_map = amount_map(
        rule_model::material_basis(ex, period_code)
            .await
            .map_err(|error| utils::db_error(error, "汇总材料成本基数"))?,
    );
    let qty_map = amount_map(
        rule_model::completed_qty(ex, period_code)
            .await
            .map_err(|error| utils::db_error(error, "汇总完工数量基数"))?,
    );
    let mut dept_cache: HashMap<Uuid, DeptBasis> = HashMap::new();

    let covered: std::collections::HashSet<(Uuid, String)> = selected
        .iter()
        .map(|rule| (rule.source_dept_id, rule.account_code.clone()))
        .collect();
    let uncovered: Vec<UncoveredPool> = dto::uncovered_pools(&pools, &covered);

    // ---------- ⑥⑦ 逐规则：算 → 断言 → 落库 ----------
    let mut rules: Vec<RuleOutcome> = Vec::with_capacity(selected.len());
    let mut zero_pool: Vec<ZeroPoolRule> = Vec::new();
    let mut total_pool = nik::prelude::rust_decimal::Decimal::ZERO;
    let mut total_alloc = nik::prelude::rust_decimal::Decimal::ZERO;
    let mut result_rows = 0i64;
    let mut collect_rows = 0i64;
    let occur_date = period_to;

    for rule in &selected {
        let key = (rule.source_dept_id, rule.account_code.clone());
        let pool = pool_map.get(&key).copied().unwrap_or_default();
        if pool == nik::prelude::rust_decimal::Decimal::ZERO {
            // 边界行为①：池为 0 → 跳过（不写 0 元行），但**记进产出**，不是静默跳过
            zero_pool.push(ZeroPoolRule {
                rule_id: rule.id,
                rule_code: rule.rule_code.clone(),
                dept_name: rule.display_dept_name(),
                account_code: rule.account_code.clone(),
                reason: dto::zero_pool_reason(period_code),
            });
            continue;
        }

        let basis = dept_cache.entry(rule.source_dept_id).or_insert_with(DeptBasis::new);
        basis.ensure_loaded(ex, period_code, rule.source_dept_id).await?;

        // 边界行为②：池 > 0 且无在产对象 → 409（报文带车间名与科目，用户才知道去补哪批）
        if basis.participants.is_empty() {
            return Err(rule_dto::no_cost_object(
                &rule.display_dept_name(),
                &rule.account_code,
                pool,
            ));
        }

        let objects = basis.objects(rule, &material_map, &qty_map);
        // 边界行为③：池 > 0 且该标准基数合计 0 → 409（`dto::allocate` 的兜底文案没有车间/科目，
        // 先在这里用带名字的版本拦一次）
        let basis_total = objects
            .iter()
            .map(|object| object.basis_value)
            .fold(nik::prelude::rust_decimal::Decimal::ZERO, |acc, value| acc + value);
        if basis_total == nik::prelude::rust_decimal::Decimal::ZERO {
            return Err(rule_dto::basis_total_zero(
                &rule.basis,
                &rule.display_dept_name(),
                &rule.account_code,
                pool,
            ));
        }

        let outcome = rule_dto::allocate(pool, &objects)?;
        let group = dto::rule_outcome(rule, &outcome);
        // 守恒断言（逐规则 Σ alloc == pool，尾差最多一行）
        dto::ensure_conservation(&group)?;

        for line in &group.lines {
            let result_id = Uuid::now_v7();
            let row = AllocResultRow {
                id: result_id,
                period_code: period_code.to_string(),
                rule_id: rule.id,
                cost_object_id: line.cost_object_id,
                dept_id: group.dept_id,
                dept_name: group.dept_name.clone(),
                account_code: group.account_code.clone(),
                basis_value: line.basis_value,
                basis_total: group.basis_total,
                pool_amount: group.pool_amount,
                alloc_amount: line.alloc_amount,
                rounding_adj: line.rounding_adj,
            };
            result_model::insert_result(ex, &row, operator)
                .await
                .map_err(|error| utils::db_error(error, "写入分摊结果"))?;
            result_rows += 1;

            let collect = AllocCollectRow {
                id: Uuid::now_v7(),
                period_code: period_code.to_string(),
                result_id,
                cost_object_id: line.cost_object_id,
                dept_id: group.dept_id,
                dept_name: group.dept_name.clone(),
                amount: line.alloc_amount,
                occur_date,
                remark: format!(
                    "分摊 {} {}（对象 {}）",
                    rule.rule_code, group.account_code, line.cost_object_no
                ),
            };
            result_model::insert_alloc_collect(ex, &collect)
                .await
                .map_err(|error| utils::db_error(error, "写入分摊转入的归集行"))?;
            collect_rows += 1;
        }

        tracing::debug!(target: "cost", "{}", group.summary_line());
        total_pool += group.pool_amount;
        total_alloc += group.alloc_total;
        rules.push(group);
    }

    // ---------- ⑧ 回查（写后真实值；库是唯一真相） ----------
    let expected: Vec<(Uuid, String, nik::prelude::rust_decimal::Decimal)> = rules
        .iter()
        .map(|group| (group.dept_id, group.account_code.clone(), group.pool_amount))
        .collect();
    let stored = result_model::results_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "回查当期分摊结果"))?;
    dto::verify_readback(period_code, &expected, &stored)?;

    // 结果表与归集表必须逐行同额：一行结果 = 一行 ALLOC 归集（设计 §4.3 幂等键）
    let stored_collect = result_model::alloc_collect_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "回查当期分摊归集行"))?;
    if stored_collect.len() != stored.len() {
        return Err(dto::readback_rows_broken(
            period_code,
            i64::try_from(stored.len()).unwrap_or(i64::MAX),
            i64::try_from(stored_collect.len()).unwrap_or(i64::MAX),
        ));
    }
    let collect_total = stored_collect
        .iter()
        .map(|(_, amount)| *amount)
        .fold(nik::prelude::rust_decimal::Decimal::ZERO, |acc, value| acc + value);
    if collect_total != total_alloc {
        return Err(dto::readback_broken(
            period_code,
            "(ALLOC 合计)",
            total_alloc,
            collect_total,
        ));
    }

    Ok(AllocRunOutcome {
        period_code: period_code.to_string(),
        rules,
        zero_pool,
        uncovered,
        total_pool,
        total_alloc,
        result_rows,
        collect_rows,
        purged_rows: i64::try_from(purged_results).unwrap_or(i64::MAX),
    })
}

/// 一个车间当期的基数原料（参与对象 + 工时），**每车间只查一次**。
struct DeptBasis {
    /// 是否已取过数（即使该车间当期一个参与对象都没有，也不要重复查）。
    loaded: bool,
    /// 参与分摊的对象（`OPEN`/`CLOSED` + 当期有料或工投入）。
    participants: Vec<ParticipantRow>,
    /// `成本对象 → (人工工时, 机器工时)`。
    labor: HashMap<Uuid, LaborBasisRow>,
}

impl DeptBasis {
    fn new() -> Self {
        Self {
            loaded: false,
            participants: Vec::new(),
            labor: HashMap::new(),
        }
    }

    /// 首次用到该车间时取一次数（同一车间多条规则时复用）。
    async fn ensure_loaded(
        &mut self,
        ex: &impl SqlExecutor,
        period_code: &str,
        dept_id: Uuid,
    ) -> std::result::Result<(), ApiError> {
        if self.loaded {
            return Ok(());
        }
        self.participants = rule_model::participants(ex, period_code, dept_id)
            .await
            .map_err(|error| utils::db_error(error, "查询参与分摊的成本对象"))?;
        self.labor = rule_model::labor_basis(ex, period_code, dept_id)
            .await
            .map_err(|error| utils::db_error(error, "汇总工时基数"))?
            .into_iter()
            .map(|row| (row.cost_object_id, row))
            .collect();
        self.loaded = true;
        Ok(())
    }

    /// 按规则的标准把四个候选基数拼成「参与分摊对象」列表（顺序稳定：对象号升序）。
    fn objects(
        &self,
        rule: &CostAllocRuleRow,
        material_map: &HashMap<Uuid, nik::prelude::rust_decimal::Decimal>,
        qty_map: &HashMap<Uuid, nik::prelude::rust_decimal::Decimal>,
    ) -> Vec<AllocObject> {
        self.participants
            .iter()
            .map(|participant| {
                let labor = self.labor.get(&participant.cost_object_id);
                let basis = ObjectBasis {
                    labor_hours: labor.map(|row| row.labor_hours).unwrap_or_default(),
                    machine_hours: labor.map(|row| row.machine_hours).unwrap_or_default(),
                    completed_qty: qty_map.get(&participant.cost_object_id).copied().unwrap_or_default(),
                    material_cost: material_map
                        .get(&participant.cost_object_id)
                        .copied()
                        .unwrap_or_default(),
                };
                AllocObject {
                    cost_object_id: participant.cost_object_id,
                    cost_object_no: participant.object_no.clone(),
                    cost_object_status: participant.status.clone(),
                    basis_value: rule_dto::basis_value(&rule.basis, &basis),
                }
            })
            .collect()
    }
}

/// `Vec<(对象, 金额)>` → `HashMap`（材料成本 / 完工数量共用）。
fn amount_map(rows: Vec<ObjectAmountRow>) -> HashMap<Uuid, nik::prelude::rust_decimal::Decimal> {
    rows.into_iter().map(|row| (row.cost_object_id, row.amount)).collect()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 非法期间在**开事务之前**就被拦下（不占连接、不打库、不留半个事务）。
    #[tokio::test]
    async fn bad_period_is_rejected_before_any_query() {
        for bad in ["2026-13", "2026-00", "202611", "", "2026-1", "2026-11-01"] {
            let error = allocate_overhead(bad, None).await.expect_err("非法期间必须 400");
            assert_eq!(
                error.status, 400,
                "「{bad}」应 400，实际 {} {}",
                error.status, error.msg
            );
        }
    }

    /// 期间闸的三种状态放行 / 四种状态拒绝（纯逻辑，真库用例再验一次读写行为）。
    #[test]
    fn period_state_gate_matches_the_redoable_list() {
        use crate::business::erp::cost_alloc_result_model::PeriodState;
        let state = |status: &str| PeriodState {
            period_code: "2026-11".to_string(),
            batch_no: "CS-202611-0001".to_string(),
            status: status.to_string(),
        };
        assert!(state("DRAFT").redoable(), "DRAFT 必须能重算");
        for blocked in ["SUBMITTED", "APPROVED", "SETTLED", "CLOSED"] {
            assert!(!state(blocked).redoable(), "{blocked} 必须拦住重算");
        }
    }

    /// 材料 / 完工数量映射：同一对象多行聚合后的值原样带过去（0 值也要在表里，
    /// 否则「该对象材料成本 0」会退化成「查不到」）。
    #[test]
    fn amount_map_keeps_zero_amounts() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let map = amount_map(vec![
            ObjectAmountRow {
                cost_object_id: a,
                amount: nik::prelude::rust_decimal::Decimal::ZERO,
            },
            ObjectAmountRow {
                cost_object_id: b,
                amount: nik::prelude::rust_decimal::Decimal::new(125, 1),
            },
        ]);
        assert_eq!(map.len(), 2);
        assert_eq!(map[&a], nik::prelude::rust_decimal::Decimal::ZERO);
        assert_eq!(map[&b].to_string(), "12.5");
    }
}

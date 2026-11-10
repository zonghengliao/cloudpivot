//! calc 第 3 段：**费归集**（ARCH-9 §4.3 费三路 · B2-8）。
//!
//! ```text
//! 扫当期三路费用来源（折旧 POSTED / 报销 APPROVED,PAID / 手工台账，科目 4101.*）
//!   → 与当期已归集的费行取差（幂等键 = (来源类型, 来源行 id, 0)）
//!   → 金额为 0 的行进「未归集清单」，不写 collect
//!   → 新行 INSERT … ON CONFLICT DO NOTHING（池行 cost_object_id 为空，直接行非空）
//!   → 过期行（单据作废 / 台账删除 / 改挂对象 / 金额修正）软删
//!   → 回查当期费归集总额 + 按 (车间, 科目) 的池汇总一并返回
//! ```
//!
//! # 没有 HTTP 入口（这是有意的）
//!
//! 设计 §5.2 的接口清单里只有 `POST /business/erp/cost-period/calc` 一个入口
//! （#27，属成本核算批次卡）。本函数是**第 3 段**，由编排方在同一次「清当期 + 重灌」里调用；
//! 单独暴露一个「只归集费」的接口会让期间数据停在半算状态（料有了、分摊没有）。
//! 「未归集清单」的对外出口是 `GET /cost-period/{id}/uncollected`（#33，属 B2-12）。
//!
//! # 一次调用是一个事务
//!
//! 扫描 → 校验 → 清过期 → 写新行 → 池汇总 → 回查，全在一个事务里：中途失败
//! 不回写任何归集行，避免「扫进去一半」的中间态。口径校验在读库后立刻做，
//! 异常行当场 400 + `66064` 并点名单行 —— 绝不静默跳过。
//!
//! # 池与直接行（验收项「按 (车间, 科目) 汇总成池」）
//!
//! - **池行**：折旧 / 报销两路，以及没挂成本对象的手工台账 ⇒ `cost_object_id IS NULL`，
//!   按 (车间, 科目) 汇总后等第 ⑤⑥ 步分摊（[`OverheadCollectOutcome::pool`]）；
//! - **直接行**：挂了成本对象的手工台账 ⇒ `cost_object_id` 非空，直接算到该对象，
//!   **不进池**（进了就会既直接计入又参与分摊，重复计成本）。
//!
//! 科目不在 `4101.*` 的费用（例如折旧科目配成 5602.01）**不会进本段**，也不会报错 ——
//! 那是「这笔费用压根不进成本」的业务配置（§4.5.1，零代码可切），不是数据错误。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_collect_dto::PURGED_DETAIL_LIMIT;
use crate::business::erp::cost_collect_model;
use crate::business::erp::cost_overhead_collect_dto::{self as dto, OverheadCollectOutcome, PoolAmount};
use crate::business::erp::cost_overhead_collect_model as model;
use crate::business::erp::ledger;
use crate::utils;

/// 期间编码形态校验（`yyyy-MM`）：在**开事务之前**判，非法直接 400。
pub fn ensure_period_code(period_code: &str) -> std::result::Result<(), ApiError> {
    if ledger::month_range(period_code).is_none() {
        return Err(dto::period_invalid(period_code));
    }
    Ok(())
}

/// 归集当期**全部**三路费用（calc 第 3 段）。
///
/// 返回 [`OverheadCollectOutcome`]：扫到几行 / 新写几行 / 池与直接各多少 / 池按科目怎么分 /
/// 未归集清单 / 写后当期费归集总额。重复调用结果收敛（第二次是无操作）。
///
/// # 异常情况
///
/// - 期间编码非法 ⇒ 400（[`dto::period_invalid`]）
/// - 来源行账面数据非法（金额为负 / 超列标度 / 科目不在 4101.* / 没车间）⇒ 400 + `66064`，
///   点名单行与原因，整笔回滚（一行都不写）
/// - 查库 / 写库失败 ⇒ 500
pub async fn collect_overhead(
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<OverheadCollectOutcome, ApiError> {
    ensure_period_code(period_code)?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启费归集事务"))?;

    let outcome = collect_overhead_in_tx(&tx, period_code, operator).await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            // 事务随 tx drop 回滚；这里显式 rollback 只是把失败原因留在日志里
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "费归集回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交费归集事务"))?;
    Ok(outcome)
}

/// 事务内的费归集（供 calc 编排方在自己已有的事务里复用同一段逻辑）。
///
/// **不提交**：调用方负责 `commit` / `rollback`。
pub async fn collect_overhead_in_tx(
    ex: &impl SqlExecutor,
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<OverheadCollectOutcome, ApiError> {
    // ① 扫候补行 + 取当期已归集费行（两个查询都不带副作用）
    let candidates = model::all_source_lines(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "扫描当期费用来源（折旧/报销/手工台账）"))?;
    let existing = model::overhead_collect_rows_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取当期已归集的费用行"))?;

    // ② 口径校验：异常行当场报错（不静默跳过），放在写库之前 ⇒ 一行都不写
    for line in &candidates {
        if let Err(reason) = line.validate() {
            return Err(dto::overhead_invalid(line, &reason));
        }
    }

    // ③ 纯函数取差：该写哪些 / 哪些已归集 / 哪些过期 / 哪些 0 元未归集
    let plan = dto::plan_of(&candidates, &existing);

    // ④ 先清过期行，再写新行 —— 顺序不能反！
    //    `uk_biz_cost_collect_src` 是 `WHERE deleted_at IS NULL` 的**部分**唯一索引：
    //    同一来源行「改挂对象 / 金额被修正」时，旧行还活着就占着键，
    //    先 INSERT 会被 `ON CONFLICT DO NOTHING` 静默吞掉，随后再删旧行 ⇒ 这一键上一行不剩。
    for row in &plan.stale {
        cost_collect_model::soft_delete_row(ex, row.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "清理过期的费归集行"))?;
    }

    // ⑤ 写新行（幂等：键还在（并发写入了）就跳过，返回行数才是真实新增）
    let inserted = model::insert_overhead_lines(ex, period_code, &plan.to_insert, operator)
        .await
        .map_err(|error| utils::db_error(error, "写入费归集行"))?;
    let inserted = usize::try_from(inserted).unwrap_or(plan.to_insert.len());

    // ⑥ 池汇总（从**已落库**的归集行反查科目，而不是内存里的计划 —— 并发/幂等场景下库是唯一真相）
    let pool = model::pool_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "汇总当期车间费用池"))?;

    // ⑦ 回查（写后真实值：汇报与对账都用这个数，不用内存里的累加）
    let (collected_rows, collected_amount) = model::collected_overhead_total(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "回查当期费归集总额"))?;

    // ⑧ 计划侧统计与未归集清单（先把需要的数取完，再把 `uncollected` 移出去）
    let inserted_amount = plan.inserted_amount();
    let inserted_pool_amount = plan.inserted_pool_amount();
    let inserted_direct_amount = plan.inserted_direct_amount();
    let purged_amount = plan.stale_amount();
    let mut uncollected = plan.uncollected;
    fill_repair_hints(&mut uncollected);

    Ok(OverheadCollectOutcome {
        period_code: period_code.to_string(),
        scanned: candidates.len(),
        inserted,
        skipped_existing: plan.already.len(),
        purged: plan.stale.len(),
        duplicated: plan.duplicated,
        inserted_amount,
        // 池 / 直接金额按**计划**统计（回查的 pool 是库里的全量，无法区分「本次新增」）
        inserted_pool_amount,
        inserted_direct_amount,
        purged_amount,
        collected_rows,
        collected_amount,
        pool,
        uncollected,
        purged_detail: plan
            .stale
            .iter()
            .take(PURGED_DETAIL_LIMIT)
            .map(|row| crate::business::erp::cost_collect_dto::PurgedRow {
                id: row.id,
                source_no: row.source_no.clone(),
                amount: row.amount,
            })
            .collect(),
    })
}

/// 给未归集清单补「怎么修」：本段只有一种缺口（来源金额为 0），
/// 修法固定指向来源单据本身 —— 不需要查费率那种两级诊断，所以是纯文案。
fn fill_repair_hints(rows: &mut [dto::UncollectedOverhead]) {
    for row in rows.iter_mut() {
        row.hint = Some(format!(
            "请核对来源单据：{} 的金额为 0（折旧是否该计提 / 报销明细是否录错科目或金额），\
             修正后重新计算本期成本；若这笔费用本来就不属于本期，请作废该单据",
            row.subject
        ));
    }
}

/// 池按科目合计（编排卡展示「本期共多少制造费用待分摊」）。
pub fn pool_total(pool: &[PoolAmount]) -> nik::prelude::rust_decimal::Decimal {
    pool.iter().map(|row| row.amount).sum()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 期间编码非法在**开事务之前**就被拦下（不占连接、不打库）。
    #[tokio::test]
    async fn bad_period_is_rejected_before_any_query() {
        for bad in ["2026-13", "2026-00", "202611", "", "2026-1"] {
            let err = ensure_period_code(bad).expect_err("非法期间必须 400");
            assert_eq!(err.status, 400, "期间「{bad}」应 400");
            assert!(err.msg.contains(bad) || err.msg.contains("yyyy-MM"));
        }
        assert!(ensure_period_code("2026-11").is_ok());
    }

    /// 编排层只写 OVERHEAD 要素 + 三种费用来源（别把料 / 工段的常量搬过来）。
    #[test]
    fn segment_only_writes_overhead_from_the_three_sources() {
        assert_eq!(dto::ELEMENT_OVERHEAD, "OVERHEAD");
        assert_eq!(dto::OVERHEAD_SOURCES.len(), 3);
        assert!(dto::OVERHEAD_SOURCES.contains(&"DEPRECIATION"));
        assert!(dto::OVERHEAD_SOURCES.contains(&"EXPENSE"));
        assert!(dto::OVERHEAD_SOURCES.contains(&"OVERHEAD_TICKET"));
        // 取数 SQL 的条件就是本段的口径（改 SQL 忘改文档 / 反之都在这条断上）
        assert!(model::DEPRECIATION_SOURCE_SQL.contains("dp.status = 'POSTED'"));
        assert!(model::EXPENSE_SOURCE_SQL.contains("r.status IN ('APPROVED','PAID')"));
        assert!(model::OVERHEAD_SOURCE_SQL.contains("t.deleted_at IS NULL"));
        assert!(model::INSERT_OVERHEAD_SQL.contains("ON CONFLICT (source_type, source_id, line_no)"));
    }

    /// 段 3 的入口签名只依赖期间 + 操作人（编排方不必知道三张来源表长什么样）。
    #[tokio::test]
    async fn entry_point_rejects_bad_period_without_touching_db() {
        let err = collect_overhead("2026-99", None).await.expect_err("非法期间必须 400");
        assert_eq!(err.status, 400);
    }

    /// 池合计 = 各行相加（编排卡展示用，不许各自四舍五入）。
    #[test]
    fn pool_total_sums_rows() {
        use nik::prelude::rust_decimal::Decimal;
        use std::str::FromStr;
        let dec = |text: &str| Decimal::from_str(text).expect("金额");
        let pool = vec![
            PoolAmount {
                dept_id: Uuid::now_v7(),
                dept_name: Some("机加工车间".to_string()),
                account_code: "4101.01".to_string(),
                amount: dec("1000.0001"),
                rows: 2,
            },
            PoolAmount {
                dept_id: Uuid::now_v7(),
                dept_name: None,
                account_code: "4101.02".to_string(),
                amount: dec("0.0001"),
                rows: 1,
            },
        ];
        assert_eq!(pool_total(&pool), dec("1000.0002"));
        assert_eq!(pool_total(&[]), Decimal::ZERO);
    }
}

//! calc 第 4 段：**费用池与 `4101.*` 对账**（ARCH-9 §4.11 C-COST-1 · B2-9）。
//!
//! ```text
//! 读池侧（biz_cost_collect 池行，按 (车间, 科目, 来源) 汇总）  ┐ 同一事务 = 同一快照
//! 读账面侧（biz_voucher_entry 的 4101.* 借方，带凭证号）        ┘
//!   → 逐 (车间, 科目) 分组求差（纯函数，见 cost_reconcile_dto）
//!   → 有差 ⇒ 409 + 66065，报文列出「差在哪几张凭证」+ 怎么修
//! ```
//!
//! # 这是 C-COST-1 的实现
//!
//! 「当期 `Σ collect(OVERHEAD, 池行)` = 当期 `4101.*` 借方发生额（按 (车间, 科目) 分组）」，
//! 级别 BLOCKER：差一分就说明有人绕过成本模块记了 4101 —— 那笔费用成本核算看不见，
//! 结算后 4101 清不掉。
//!
//! # 为什么分 `Calc` / `Settle` 两个阶段（口径说明，不是设计走样）
//!
//! 折旧与报销的凭证随来源单据过账生成；**手工费用台账的「借 4101 / 贷 对方科目」是结算时
//! 才生成的**（设计 §4.8 ④，与分摊凭证同一事务）。所以 calc 时刻三路池里必然有一段账上
//! 还没有的手工台账金额：
//!
//! - 把它当差错 ⇒ 每张有手工台账的期间都红，财务只能去删台账；
//! - 把它当不存在 ⇒ 结算时 C-COST-1 形同虚设。
//!
//! 处理办法是显式命名为「待结算过账」（[`ReconcileOutcome::pending_amount`]）：
//! `Calc` 阶段不要求它有账面背书，`Settle` 阶段（B3-6 事务内自检，第四张凭证已过账）要求。
//! 两个阶段共用同一份代码与同一条判据（逐组差额为 0），不存在「两套算法」。
//!
//! # 没有 HTTP 入口（与前三段同构）
//!
//! 设计 §5.2 只有 `POST /business/erp/cost-period/calc` 一个入口（#27，属 calc 编排卡）。
//! 本函数由编排方在 calc 的第 ④ 步调用：**不平就不许继续分摊**（继续下去会把「账上多出来的
//! 费用」摊到成本对象上，错的不是成本而是全部存货）。

use nik::prelude::*;

use crate::business::erp::cost_calc_overhead;
use crate::business::erp::cost_reconcile_dto::{self as dto, ReconcileOutcome, Stage};
use crate::business::erp::cost_reconcile_model as model;
use crate::utils;

/// 对账（自开事务）：返回结论文本，**不平直接 409**。
///
/// # 异常情况
///
/// - 期间编码非法 ⇒ 400（与料 / 工 / 费段同一文案）
/// - 池与账面不平 ⇒ 409 + `66065`，报文列出差额凭证号与修法
/// - 查库失败 ⇒ 500
pub async fn reconcile_pool_with_ledger(
    period_code: &str,
    stage: Stage,
) -> std::result::Result<ReconcileOutcome, ApiError> {
    cost_calc_overhead::ensure_period_code(period_code)?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启费用池对账事务"))?;

    let outcome = reconcile_in_tx(&tx, period_code, stage).await?;

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交费用池对账事务"))?;

    dto::ensure_balanced(&outcome)?;
    Ok(outcome)
}

/// 事务内的对账（供 calc / settle 编排方在自己已有的事务里复用）。
///
/// **不提交、不报错**：返回结论，由调用方决定是 `?`（[`dto::ensure_balanced`]）还是先记录再继续。
/// 结算事务（B3-6）必须在**自己的事务内**调用它 —— 用自己的写读做断言，不平就整个回滚。
pub async fn reconcile_in_tx(
    ex: &impl SqlExecutor,
    period_code: &str,
    stage: Stage,
) -> std::result::Result<ReconcileOutcome, ApiError> {
    // ① 池侧：当期池行按 (车间, 科目, 来源) 汇总
    let pool = model::pool_by_source(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取当期车间费用池"))?;

    // ② 账面侧：当期 4101.* 借方发生额（逐条分录，带凭证号）
    let ledger = model::ledger_debits(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取当期 4101.* 借方发生额"))?;

    // ③ 逐 (车间, 科目) 分组求差（纯函数）
    let outcome = dto::reconcile(period_code, stage, &pool, &ledger);

    if outcome.balanced() {
        tracing::info!(target: "cost", "{}", outcome.summary_line());
    } else {
        tracing::warn!(target: "cost", "{}", outcome.summary_line());
    }
    Ok(outcome)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost_overhead_collect_dto::{SOURCE_DEPRECIATION, SOURCE_OVERHEAD_TICKET};
    use nik::prelude::rust_decimal::Decimal;
    use nik::uuid::Uuid;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("金额字面量")
    }

    /// 非法期间在**开事务之前**就被拦下（不占连接、不打库）。
    #[tokio::test]
    async fn bad_period_is_rejected_before_any_query() {
        for bad in ["2026-13", "2026-00", "202611", "", "2026-1"] {
            let err = reconcile_pool_with_ledger(bad, Stage::Calc)
                .await
                .expect_err("非法期间必须 400");
            assert_eq!(err.status, 400, "期间「{bad}」应 400");
        }
    }

    /// 平 ⇒ `Ok(())`；不平 ⇒ 409 + 66065（结算事务里也用它做事务内断言）。
    #[test]
    fn ensure_balanced_turns_a_difference_into_a_409() {
        let dept = Uuid::now_v7();
        let balanced = dto::reconcile("2026-11", Stage::Calc, &[], &[]);
        assert!(dto::ensure_balanced(&balanced).is_ok());

        let pool = vec![dto::PoolSideRow {
            dept_id: dept,
            dept_name: Some("机加工车间".to_string()),
            account_code: "4101.01".to_string(),
            source_type: SOURCE_DEPRECIATION.to_string(),
            amount: dec("100"),
            rows: 1,
        }];
        let unbalanced = dto::reconcile("2026-11", Stage::Calc, &pool, &[]);
        let err = dto::ensure_balanced(&unbalanced).expect_err("不平必须报错");
        assert_eq!(err.status, 409);
        assert_eq!(err.code, crate::shared::error_code::ERP_COST_POOL_LEDGER_MISMATCH);
        assert!(err.msg.contains("4101.01"), "报文要点名科目与车间：{}", err.msg);
    }

    /// 池侧的「待结算过账」不参与 Calc 判平（否则每张有手工台账的期间都红）。
    #[test]
    fn manual_ticket_does_not_block_the_calc_stage() {
        let dept = Uuid::now_v7();
        let pool = vec![dto::PoolSideRow {
            dept_id: dept,
            dept_name: None,
            account_code: "4101.03".to_string(),
            source_type: SOURCE_OVERHEAD_TICKET.to_string(),
            amount: dec("150"),
            rows: 1,
        }];
        let outcome = dto::reconcile("2026-11", Stage::Calc, &pool, &[]);
        assert_eq!(outcome.pending_amount, dec("150"));
        assert!(dto::ensure_balanced(&outcome).is_ok());
    }
}

//! calc 第 1 段：**料归集**（ARCH-9 §4.3 / §3.1 ③ · B2-6）。
//!
//! ```text
//! 扫当期「已过账 + 挂成本对象」的领料明细（biz_stock_out_item）
//!   → 与当期已归集的料行取差（幂等键 = 来源明细 id）
//!   → 新行 INSERT … ON CONFLICT DO NOTHING
//!   → 过期行（来源已作废 / 改挂对象）软删
//!   → 回查当期料归集总额一并返回
//! ```
//!
//! # 没有 HTTP 入口（这是有意的）
//!
//! 设计 §5.2 的接口清单里只有 `POST /business/erp/cost-period/calc` 一个入口
//! （#27，属成本核算批次卡）。calc 是一串六段流水线，本函数是**第 1 段**，
//! 由编排方在同一次「清当期 + 重灌」里调用；单独暴露一个「只归集料」的接口
//! 会让期间数据停在半算状态（料有了、工费没有），比不给接口更危险。
//!
//! # 一次调用是一个事务
//!
//! 扫描 → 清过期行 → 插入新行 → 回查，全在一个事务里：中途失败（例如行数据非法）时
//! 不回写任何归集行，避免「扫进去一半」的中间态。**口径校验在读库后立刻做**
//! （3000 行期间也只是一次线性校验），发现异常行当场 400 + `66064` 并点名单号 ——
//! 绝不静默跳过（跳过的后果是归集金额与领料金额差一笔、且没人知道为什么）。
//!
//! # 金额 / 期间 / 幂等的三条口径（设计 §4.3）
//!
//! 1. 金额取 `biz_stock_out_item.amount`（过账时写入的**账上真实扣减**），不按
//!    `qty × 单价` 重算 —— 多桶 + 4 位舍入下两者必然有分毫差，重算就对不上 1405；
//! 2. 期间按 `biz_stock_out.out_date` 归属（期间区间优先取 `biz_accounting_period`
//!    的 `start_date/end_date`，月结把期间拉长/缩短时也跟得上）；
//! 3. 重复跑不重复写：来源明细 id 做幂等键，第二次跑 `inserted = 0`、
//!    `skipped_existing = scanned`，归集行的金额与 `updated_at` 一律不动。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_collect_dto::{self as dto, MaterialCollectOutcome, PURGED_DETAIL_LIMIT, PurgedRow};
use crate::business::erp::cost_collect_model as model;
use crate::business::erp::ledger;
use crate::utils;

/// 期间区间：先校验编码形态（400），再取库里的真实区间（`biz_accounting_period` 优先）。
pub async fn period_bounds(period_code: &str) -> std::result::Result<(NaiveDate, NaiveDate), ApiError> {
    if ledger::month_range(period_code).is_none() {
        return Err(dto::period_invalid(period_code));
    }
    ledger::period_range(period_code)
        .await
        .map_err(|error| utils::db_error(error, "取会计期间的起止日"))
}

/// 归集当期**全部**已过账领料（calc 第 1 段）。
///
/// 返回 [`MaterialCollectOutcome`]：扫到几行 / 新写几行 / 跳过几行 / 清掉几行 /
/// 写后当期料归集总额。重复调用结果收敛（第二次是无操作）。
///
/// # 异常情况
///
/// - 期间编码非法 ⇒ 400（[`dto::period_invalid`]）
/// - 来源行账面数据非法（数量 ≤ 0 / 金额 < 0 / 物料为空 / 超列标度）⇒ 400 + `66064`，
///   点名单号与原因，整笔回滚（一行都不写）
/// - 查库 / 写库失败 ⇒ 500
pub async fn collect_material(
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<MaterialCollectOutcome, ApiError> {
    let (start, end) = period_bounds(period_code).await?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启料归集事务"))?;

    let outcome = collect_material_in_tx(&tx, period_code, start, end, operator).await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            // 事务随 tx drop 回滚；这里显式 rollback 只是把失败原因留在日志里
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "料归集回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交料归集事务"))?;
    Ok(outcome)
}

/// 事务内的料归集（供 calc 编排方在自己已有的事务里复用同一段逻辑）。
///
/// **不提交**：调用方负责 `commit` / `rollback`。
pub async fn collect_material_in_tx(
    ex: &impl SqlExecutor,
    period_code: &str,
    start: NaiveDate,
    end: NaiveDate,
    operator: Option<Uuid>,
) -> std::result::Result<MaterialCollectOutcome, ApiError> {
    // ① 扫候补行 + 取当期已归集行（两个查询都不带副作用）
    let candidates = model::material_source_lines(ex, start, end)
        .await
        .map_err(|error| utils::db_error(error, "扫描当期已过账领料"))?;
    let existing = model::collect_rows_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取当期已归集的领料行"))?;

    // ② 口径校验：异常行当场报错（不静默跳过），放在写库之前 ⇒ 一行都不写
    for line in &candidates {
        if let Err(reason) = line.validate() {
            return Err(dto::source_invalid(&line.out_no, &reason));
        }
    }

    // ③ 纯函数取差：该写哪些、哪些已归集、哪些已过期
    let plan = dto::plan(&candidates, &existing);

    // ④ 先清过期行，再写新行 —— 顺序不能反！
    //    `uk_biz_cost_collect_src` 是 `WHERE deleted_at IS NULL` 的**部分**唯一索引：
    //    同一来源行「改挂对象 / 金额被修正」时，旧行还活着就占着键，
    //    先 INSERT 会被 `ON CONFLICT DO NOTHING` 静默吞掉，随后再删旧行 ⇒ 这一键上一行不剩。
    for row in &plan.stale {
        model::soft_delete_row(ex, row.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "清理过期的料归集行"))?;
    }

    // ⑤ 写新行（幂等：键还在（并发写入了）就跳过，返回行数才是真实新增）
    let inserted = model::insert_material_lines(ex, period_code, &plan.to_insert, operator)
        .await
        .map_err(|error| utils::db_error(error, "写入料归集行"))?;
    let inserted = usize::try_from(inserted).unwrap_or(plan.to_insert.len());

    // ⑥ 回查（写后真实值：汇报与 C-COST-5 都用这个数，不用内存里的累加）
    let (collected_rows, collected_amount) = model::collected_total(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "回查当期料归集总额"))?;

    Ok(MaterialCollectOutcome {
        period_code: period_code.to_string(),
        scanned: candidates.len(),
        inserted,
        skipped_existing: plan.already.len(),
        purged: plan.stale.len(),
        duplicated: plan.duplicated,
        inserted_amount: plan.inserted_amount(),
        purged_amount: plan.stale_amount(),
        zero_amount: plan.zero_amount(),
        collected_rows,
        collected_amount,
        purged_detail: plan
            .stale
            .iter()
            .take(PURGED_DETAIL_LIMIT)
            .map(|row| PurgedRow {
                id: row.id,
                source_no: row.source_no.clone(),
                amount: row.amount,
            })
            .collect(),
    })
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 期间编码非法在**开事务之前**就被拦下（不占连接、不打库）。
    #[tokio::test]
    async fn bad_period_is_rejected_before_any_query() {
        for bad in ["2026-13", "2026-00", "202611", "", "2026-1"] {
            let err = period_bounds(bad).await.expect_err("非法期间必须 400");
            assert_eq!(err.status, 400, "期间「{bad}」应 400");
            assert!(err.msg.contains(bad) || err.msg.contains("yyyy-MM"));
        }
    }

    /// 编排层用的口径常量没有走样（段 1 只写 MATERIAL + STOCK_OUT）。
    #[test]
    fn segment_only_writes_material_from_stock_out() {
        assert_eq!(dto::ELEMENT_MATERIAL, "MATERIAL");
        assert_eq!(dto::SOURCE_STOCK_OUT, "STOCK_OUT");
        assert_eq!(dto::OUT_TYPE_ISSUE, "ISSUE");
        assert_eq!(dto::OUT_STATUS_POSTED, "POSTED");
        assert_eq!(dto::MATERIAL_LINE_NO, 1);
        // 扫描 SQL 的条件就是段 1 的口径（改 SQL 忘改文档 / 反之都在这条断上）
        assert!(model::MATERIAL_SOURCE_SQL.contains("h.status = 'POSTED'"));
    }
}

//! calc 第 2 段：**工归集**（ARCH-9 §4.3 / §3.1 ④ · B2-7）。
//!
//! ```text
//! 扫当期「已确认（CONFIRMED）」的工时台账行（biz_cost_labor）
//!   → 与当期已归集的工行取差（幂等键 = 工时行 id，line_no = 0）
//!   → 缺费率 / 金额舍入为 0 的行进「未归集清单」，不写 collect、不按 0 计
//!   → 新行 INSERT … ON CONFLICT DO NOTHING
//!   → 过期行（工时行被删 / 改挂对象）软删
//!   → 回查当期工归集总额一并返回
//! ```
//!
//! # 没有 HTTP 入口（这是有意的）
//!
//! 设计 §5.2 的接口清单里只有 `POST /business/erp/cost-period/calc` 一个入口
//! （#27，属成本核算批次卡）。本函数是**第 2 段**，由编排方在同一次「清当期 + 重灌」里调用；
//! 单独暴露一个「只归集工」的接口会让期间数据停在半算状态（料有了、费没有），比不给更危险。
//! 「未归集清单」的对外出口是 `GET /cost-period/{id}/uncollected`（#33，属 B2-12），
//! 本段只负责把**工侧的缺口**算出来交给它。
//!
//! # 一次调用是一个事务
//!
//! 扫描 → 校验 → 清过期 → 写新行 → 回查，全在一个事务里：中途失败（例如台账行账面非法）
//! 不回写任何归集行，避免「扫进去一半」的中间态。口径校验在读库后立刻做，
//! 异常行当场 400 + `66064` 并点名单行 —— 绝不静默跳过。
//!
//! # 三条口径（设计 §4.3）
//!
//! 1. 只取 `status='CONFIRMED'` 的工时（DRAFT 还能改，归集它等于归集一个会变的数）；
//! 2. 金额 = 工时 × 工资率，**照抄台账的 `amount`**（库层 CHECK 已钉死 `round(hours × rate, 4)`，
//!    `rate` 是录入时由 B2-3 按「对象 > 车间 > 公司」取数后落的快照）；
//! 3. 缺费率（`rate = 0`）的行**不写 0 金额的归集行**，进未归集清单并给出「怎么修」的提示
//!    （设计 D-7 不猜：0 与「没配费率」在账上分不清）。

use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_collect_dto::PURGED_DETAIL_LIMIT;
use crate::business::erp::cost_collect_model as model;
use crate::business::erp::cost_labor_collect_dto::{self as dto, LaborCollectOutcome, UncollectedLabor};
use crate::business::erp::cost_rate_dto::{self as rate_dto, RATE_TYPE_LABOR, RateRequest, RateSubject};
use crate::business::erp::cost_rate_handler;
use crate::business::erp::ledger;
use crate::utils;

/// 期间编码形态校验（`yyyy-MM`）：在**开事务之前**判，非法直接 400。
pub fn ensure_period_code(period_code: &str) -> std::result::Result<(), ApiError> {
    if ledger::month_range(period_code).is_none() {
        return Err(dto::period_invalid(period_code));
    }
    Ok(())
}

/// 归集当期**全部**已确认工时（calc 第 2 段）。
///
/// 返回 [`LaborCollectOutcome`]：扫到几行 / 新写几行 / 跳过几行 / 清掉几行 / 未归集清单 /
/// 写后当期工归集总额。重复调用结果收敛（第二次是无操作）。
///
/// # 异常情况
///
/// - 期间编码非法 ⇒ 400（[`dto::period_invalid`]）
/// - 台账行账面数据非法（工时 ≤ 0 / 金额 ≠ 工时 × 费率 / 字段超列标度）⇒ 400 + `66064`，
///   点名单行与原因，整笔回滚（一行都不写）
/// - 查库 / 写库失败 ⇒ 500
pub async fn collect_labor(
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<LaborCollectOutcome, ApiError> {
    ensure_period_code(period_code)?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启工归集事务"))?;

    let outcome = collect_labor_in_tx(&tx, period_code, operator).await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            // 事务随 tx drop 回滚；这里显式 rollback 只是把失败原因留在日志里
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "工归集回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交工归集事务"))?;
    Ok(outcome)
}

/// 事务内的工归集（供 calc 编排方在自己已有的事务里复用同一段逻辑）。
///
/// **不提交**：调用方负责 `commit` / `rollback`。
pub async fn collect_labor_in_tx(
    ex: &impl SqlExecutor,
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<LaborCollectOutcome, ApiError> {
    // ① 扫候补行 + 取当期已归集行（两个查询都不带副作用）
    let candidates = model::labor_source_lines(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "扫描当期已确认工时"))?;
    let existing = model::labor_collect_rows_of_period(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "读取当期已归集的工时行"))?;

    // ② 口径校验：异常行当场报错（不静默跳过），放在写库之前 ⇒ 一行都不写
    for line in &candidates {
        if let Err(reason) = line.validate() {
            return Err(dto::labor_invalid(line, &reason));
        }
    }

    // ③ 纯函数取差：该写哪些 / 哪些已归集 / 哪些过期 / 哪些缺费率
    let mut plan = dto::plan_of(&candidates, &existing);

    // ④ 先清过期行，再写新行 —— 顺序不能反！
    //    `uk_biz_cost_collect_src` 是 `WHERE deleted_at IS NULL` 的**部分**唯一索引：
    //    同一工时行「改挂对象 / 金额被修正」时，旧行还活着就占着键，
    //    先 INSERT 会被 `ON CONFLICT DO NOTHING` 静默吞掉，随后再删旧行 ⇒ 这一键上一行不剩。
    for row in &plan.stale {
        model::soft_delete_row(ex, row.id, operator)
            .await
            .map_err(|error| utils::db_error(error, "清理过期的工归集行"))?;
    }

    // ⑤ 写新行（幂等：键还在（并发写入了）就跳过，返回行数才是真实新增）
    let inserted = model::insert_labor_lines(ex, period_code, &plan.to_insert, operator)
        .await
        .map_err(|error| utils::db_error(error, "写入工归集行"))?;
    let inserted = usize::try_from(inserted).unwrap_or(plan.to_insert.len());

    // ⑥ 未归集清单补「怎么修」（查一次 B2-3 的候选费率，纯函数逐行判）
    if !plan.uncollected.is_empty() {
        fill_repair_hints(&mut plan.uncollected).await;
    }

    // ⑦ 回查（写后真实值：汇报与对账都用这个数，不用内存里的累加）
    let (collected_rows, collected_amount) = model::collected_labor_total(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "回查当期工归集总额"))?;

    Ok(LaborCollectOutcome {
        period_code: period_code.to_string(),
        scanned: candidates.len(),
        inserted,
        skipped_existing: plan.already.len(),
        purged: plan.stale.len(),
        duplicated: plan.duplicated,
        inserted_amount: plan.inserted_amount(),
        purged_amount: plan.stale_amount(),
        collected_rows,
        collected_amount,
        uncollected_hours: plan.uncollected_hours(),
        uncollected: plan.uncollected,
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

/// 给未归集清单补「怎么修」：现在能不能取到费率 / 该去配哪一级。
///
/// 只在**真有未归集行**时查一次库（`rates_of_type` 一次取回同类型全部未删除行，
/// 逐行用纯函数 [`rate_dto::resolve`] 判），不在每行上各打一次库。
async fn fill_repair_hints(rows: &mut [UncollectedLabor]) {
    let candidates = match cost_rate_handler::candidates(RATE_TYPE_LABOR).await {
        Ok(rows) => rows,
        Err(error) => {
            // 查费率失败不该让整段归集失败：缺费率本身就是「可以先算着、结算前补」的缺口
            for row in rows.iter_mut() {
                row.hint = Some(format!(
                    "（查成本费率失败，暂时无法给出修复建议：{}）请检查 biz_cost_rate 后重跑计算",
                    error.msg
                ));
            }
            return;
        }
    };

    for row in rows.iter_mut() {
        let subject = RateSubject {
            cost_object_no: row.cost_object_no.as_deref().unwrap_or("-"),
            dept_name: row.dept_name.as_deref().unwrap_or("-"),
        };
        let request = RateRequest::labor(row.cost_object_id, dept_id_of(row), row.work_date);
        row.hint = Some(match rate_dto::resolve(&candidates, &request, &subject) {
            Ok(resolved) => format!(
                "现在已能取到 {} 元/小时（来源：{}），请在工时台账把这一行的工资率补上（或作废重录）后重新确认，再重跑计算",
                resolved.rate.normalize(),
                resolved.scope_detail(&subject)
            ),
            Err(error) => format!(
                "费率取数三级都没命中：{} 请先维护 biz_cost_rate（生效期要覆盖 {}），再重跑计算",
                error.msg, row.work_date
            ),
        });
    }
}

/// 未归集行的车间 id（`UncollectedLabor` 只留了车间名给人看，费率取数需要 id）。
///
/// 缺费率行**必然**来自工时台账，而台账行的 `dept_id` 是非空的；本结构为了出参瘦身
/// 没有重复携带它，这里用一个不可能命中的占位（nil）也能让 `resolve` 走「车间级不匹配
/// → 公司级」的降级路径 —— 但那样会给出**错的**建议（其实车间级能命中）。
/// 所以 [`UncollectedLabor`] 里保留 `dept_id`，见结构定义。
fn dept_id_of(row: &UncollectedLabor) -> Uuid {
    row.dept_id
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

    /// 编排层只写 LABOR + LABOR_TICKET（别把料段的常量搬过来）。
    #[test]
    fn segment_only_writes_labor_from_labor_ticket() {
        assert_eq!(dto::ELEMENT_LABOR, "LABOR");
        assert_eq!(dto::SOURCE_LABOR_TICKET, "LABOR_TICKET");
        assert_eq!(dto::LABOR_LINE_NO, 0);
        // 取数 SQL 的条件就是本段的口径（改 SQL 忘改文档 / 反之都在这条断上）
        assert!(model::LABOR_SOURCE_SQL.contains("l.status = 'CONFIRMED'"));
        assert!(model::INSERT_LABOR_SQL.contains("ON CONFLICT (source_type, source_id, line_no)"));
    }

    /// 未归集行必须带 `dept_id`（否则费率建议会走错降级路径）。
    #[test]
    fn uncollected_row_carries_dept_id_for_rate_lookup() {
        let dept = Uuid::now_v7();
        let row = UncollectedLabor {
            labor_id: Uuid::now_v7(),
            cost_object_id: Uuid::now_v7(),
            cost_object_no: None,
            employee_name: "张伟".to_string(),
            dept_name: None,
            dept_id: dept,
            work_date: nik::prelude::chrono::NaiveDate::from_ymd_opt(2026, 11, 5).expect("日期"),
            hours: nik::prelude::rust_decimal::Decimal::new(4, 0),
            rate: nik::prelude::rust_decimal::Decimal::ZERO,
            reason: "工资率为 0".to_string(),
            hint: None,
        };
        assert_eq!(dept_id_of(&row), dept);
    }

    /// 段 2 的入口签名只依赖期间 + 操作人（编排方不必知道工时台账长什么样）。
    #[tokio::test]
    async fn entry_point_rejects_bad_period_without_touching_db() {
        let err = collect_labor("2026-99", None).await.expect_err("非法期间必须 400");
        assert_eq!(err.status, 400);
    }
}

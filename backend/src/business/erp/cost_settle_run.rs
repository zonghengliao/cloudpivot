//! 结算编排：状态回写 + 事务内 6 条断言（ARCH-9 §4.8 ①–⑥ · COST-B3-6）。
//!
//! 设计：`docs/architecture/ARCH-9-成本核算升级设计.md` §4.8（结算事务与失败补偿）、
//! §4.11（巡检不变量 C-COST-1~4）、§4.7.5（算例 A 的四张凭证与 4001 余额）。
//!
//! # 一次结算 = 一个事务
//!
//! ```
//! BEGIN
//!  ① biz_cost_period FOR UPDATE + 状态校验（APPROVED）+ 期间未结账 + 无红冲在跑
//!  ② 完工入库      cost_settle_finish::generate  （B3-4：入库单 + 三本账 + 计算单回写）
//!  ③ 三/四张凭证   cost_settle_voucher::post_all  （B3-5：全走 voucher::post_voucher）
//!  ④ 批次状态回写  状态机 APPROVED→SETTLED + CAS 更新单头（凭证 id / 合计 / 对象数）
//!  ⑤ 事务内 6 条断言（下面「6 条断言」）—— 任一不平 ⇒ 整个事务回滚
//! COMMIT
//! ⑥ 提交后：本卡**没有任何**会失败的账务动作（通知不在 B3 范围，见「已知偏差」）
//! ```
//!
//! 为什么把入库 / 凭证 / 状态全放进一个事务：AGENTS 约定 11 针对的是「`commit()` 之后
//! 还要做可能失败的后续动作」。这里所有账务动作都在事务内，提交后只剩「发通知」这一个
//! 只打 `warn` 的动作 ⇒ **不存在「提交后失败留死单」的场景**（设计 §4.8 末段）。
//!
//! # 6 条断言（事务内版 C-COST-1~4）
//!
//! 断言读的是**本事务刚写入的数据**（自己的写读），不平就整笔回滚 —— 这正是
//! 「结算过的账当天就红」的唯一有效防线。
//!
//! | # | 编号 | 判据 | 设计出处 |
//! |---|---|---|---|
//! | 1 | `C-COST-1` | 当期 `Σ biz_cost_collect(OVERHEAD, 池行)` = 当期 `4101.*` 借方发生额（按 (车间, 科目) 分组，复用 `cost_calc_reconcile::reconcile_in_tx(Stage::Settle)`） | §4.11 C-COST-1 |
//! | 2 | `C-COST-2` | `Σ biz_stock_in(PRODUCTION, 本批次).total_amount` = 完工结转凭证**分录**里 1405 的借方合计 = `Σ item.finished_cost` | §4.11 C-COST-2 |
//! | 3 | `C-COST-2·行` | 逐对象：`completed_qty > 0` ⇒ 计算单 `SETTLED` + 绑单 + 入库单数量/金额与计算单**逐行相等**；且入库单张数 = 有完工的对象数 | §4.7.6 勾稽 |
//! | 4 | `C-COST-3` | `4001.01/.02/.03` 本期净发生（借 − 贷）= `Σ(wip_end_cost − wip_beg_cost)` | §4.11 C-COST-3 |
//! | 5 | `C-COST-4·清零` | 每个 (车间, 科目) 的 `4101.*` 本期借方 = 贷方（池在结算后余额为 0） | §4.11 C-COST-4 后半 |
//! | 6 | `C-COST-4·守恒` | `Σ biz_cost_alloc_result.alloc_amount` = `Σ biz_cost_collect(OVERHEAD, 池行)`（分摊守恒 / 无漏摊） | §4.11 C-COST-4 前半 |
//!
//! ## 为什么把「分摊守恒」放在第 6 条（而不是第 5 条）
//!
//! 卡面验收要求「故意让第 6 条失败，验证前面全部回滚」。分摊守恒是**唯一可以只被它一条
//! 抓到**的缺口：删掉 / 少写一行分摊结果时，池与账面（断言 1）、入库与凭证（断言 2/3）、
//! 4001 余额（断言 4）、4101 清零（断言 5）**全都仍然对平**（分摊结果不参与任何凭证），
//! 只有第 6 条会红。真库用例 `sixth_assertion_rolls_back_everything` 就是这么做的。
//! 若把「4101 清零」放第 6，要制造「只它一条红」的缺口得往 4101 记一笔旁路账 —— 那会
//! 同时打红断言 1（池少、账面多），验不出「第 6 条失败 ⇒ 全回滚」。
//!
//! ## 断言 4 为什么是「本期净发生」而不是「期末余额」
//!
//! 设计 C-COST-3 的表述是「最新已结算批次的 `Σ wip_end_cost` = `4001.*` 期末余额」。事务内
//! 版取**等价的本期形态**：`期末余额 − 期初余额 = Σ(wip_end − wip_beg)`，而「期初余额 =
//! 上期已结算批次的 `Σ wip_end`」由**上一期同一条断言**保证（归纳）。这样做有两个实在理由：
//!
//! ① **免疫无关期间**：全库 `SUM` 会把别的年度 / 别的期间（含夹具、期初凭证）一起算进来，
//!    一个期间的对平却因为隔壁期间的历史余额而红，是假警报；
//! ② **红冲重结可判**：红冲后同期间重结时，旧批次的蓝字 + 红字凭证都落在**同一期间**，
//!    净发生自然归零，新批次的凭证再进来 —— 本期形态不需要额外排除逻辑。
//!
//! 全局余额版（含跨期结转校验）留给 B3-10 巡检（`erp_invariants.py` 跑整库、不赶时间）。
//!
//! # 失败口径
//!
//! 断言失败一律 409 + `66077`（除断言 1 复用 `66065`「池与 4101 借方不平」，与 calc 段 4
//! 同一个码、同一套报文）。**不给「跳过断言」的开关**：断言是结算的一部分，不是可选体检。
//!
//! # 已知偏差（卡内报备）
//!
//! * §4.8 ⑦ 的「提交后发通知（站内信 + 待办完成）」不在本卡：B3 批次里没有通知卡，
//!   且它失败只打 `warn`、不影响单据状态，不构成死单风险。
//! * 计算单 `completed_qty = 0` 的行**不置 `SETTLED`**（`ck_biz_cost_item_settled` 要求
//!   `SETTLED` 必须有 `stock_in_id`，而 0 件不建入库单）—— 这是 B3-4 的既定口径，
//!   本卡沿用：断言 3 的判据是「`completed_qty > 0` ⇒ 必须已绑单」，不反过来要求。
// ============================================================================

use nik::prelude::chrono::{DateTime, Datelike, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_calc_reconcile;
use crate::business::erp::cost_period_dto::{self, STATUS_APPROVED, STATUS_SETTLED};
use crate::business::erp::cost_period_model as model;
use crate::business::erp::cost_reconcile_dto::{self as reconcile_dto, Stage};
use crate::business::erp::cost_settle_finish::{self, FinishStockIn, SettleContext};
use crate::business::erp::cost_settle_voucher::{self as cv, ItemCostRow, SettledVouchers, VoucherContext};
use crate::business::erp::voucher;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（与设计 / 迁移逐字对齐，单测钉死） ====================

/// 生产成本的三个末级科目（§4.7：`4001` 已翻非末级，记账必须到末级）。
pub const ACCOUNTS_WIP: [&str; 3] = [cv::ACCOUNT_MATERIAL, cv::ACCOUNT_LABOR, cv::ACCOUNT_OVERHEAD];

/// 制造费用的末级前缀（`4101.01`–`4101.09`）。
pub const PREFIX_OVERHEAD: &str = "4101.";

/// 完工入库科目（完工结转凭证的借方）。
pub const ACCOUNT_FINISHED_GOODS: &str = cv::ACCOUNT_FINISHED_GOODS;

/// 金额容差：`NUMERIC(18,4)` 下的「一分钱以内」按设计 §4.11 的 0.0001 判。
pub const TOLERANCE: &str = "0.0001";

/// 6 条断言的编号（顺序即执行顺序，**不要重排**，见模块头「为什么第 6 条」）。
pub const ASSERT_IDS: [&str; 6] = [
    "C-COST-1",
    "C-COST-2",
    "C-COST-2·行",
    "C-COST-3",
    "C-COST-4·清零",
    "C-COST-4·守恒",
];

/// 6 条断言的判据名（报文 / 单测共用）。
pub const ASSERT_NAMES: [&str; 6] = [
    "当期池 = 4101.* 借方发生额（逐 (车间,科目) 组）",
    "生产入库金额 = 完工结转凭证借方 1405 = Σ 完工成本",
    "入库单 ↔ 计算单逐行同源（数量 / 金额 / 状态）",
    "4001 本期净发生 = Σ(期末在制 − 期初在制)",
    "每个 (车间,科目) 的 4101 本期借贷相等（池清零）",
    "Σ 分摊结果 = Σ 池行（分摊守恒 / 无漏摊）",
];

/// 入库单已作废（红冲置的；断言统计时排除，否则红冲重结会把旧单算成金额）。
pub const STOCK_IN_CANCELLED: &str = cost_settle_finish::STOCK_IN_CANCELLED;

/// 计算单已结算。
pub const ITEM_SETTLED: &str = cost_settle_finish::ITEM_SETTLED;

// ==================== 结构 ====================

/// 一次结算的输入（批次快照 + 会计日期 + 操作人）。
#[derive(Debug, Clone)]
pub struct SettleInput<'a> {
    /// 结算批次 id。
    pub period_id: Uuid,
    /// 期间码 `yyyy-MM`。
    pub period_code: &'a str,
    /// 批次号 `CS-yyyyMM-NNNN`。
    pub batch_no: &'a str,
    /// 人工来源（`MANUAL` / `PAYROLL`）：决定要不要出人工计提凭证。
    pub labor_source: &'a str,
    /// 完工入库日期（= 期末日）。
    pub in_date: NaiveDate,
    /// 凭证日期（必须落在 `period_code` 内；= 期末日）。
    pub voucher_date: NaiveDate,
    /// 操作人（`settled_by` / 凭证 `posted_by` / 状态日志）。
    pub operator: Option<Uuid>,
    /// 业务时间。
    pub now: DateTime<Utc>,
}

impl SettleInput<'_> {
    /// 入库上下文（B3-4 的入参）。
    fn finish_context(&self) -> SettleContext<'_> {
        SettleContext {
            period_id: self.period_id,
            period_code: self.period_code,
            batch_no: self.batch_no,
            in_date: self.in_date,
            operator: self.operator,
            now: self.now,
        }
    }

    /// 凭证上下文（B3-5 的入参）。
    fn voucher_context(&self) -> VoucherContext<'_> {
        VoucherContext {
            period_id: self.period_id,
            period_code: self.period_code,
            batch_no: self.batch_no,
            voucher_date: self.voucher_date,
            labor_source: self.labor_source,
            operator: self.operator,
            now: self.now,
        }
    }
}

/// 一条断言的通过记录（运维 / 卡片报备用；失败就不返回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertionReport {
    /// 编号（`C-COST-1` …）。
    pub id: &'static str,
    /// 判据名。
    pub name: &'static str,
    /// 实测细节（金额 / 组数 / 行数）。
    pub detail: String,
}

/// 一次结算的全部产物（调用方只用来报数 / 打日志）。
#[derive(Debug, Clone)]
pub struct SettleRun {
    /// 批次 id。
    pub period_id: Uuid,
    /// 批次号。
    pub batch_no: String,
    /// 完工入库单（每对象一张）。
    pub stock_ins: Vec<FinishStockIn>,
    /// 三 / 四张凭证的 id 与合计。
    pub vouchers: SettledVouchers,
    /// 回写到单头的合计。
    pub totals: model::PeriodTotals,
    /// 计算单行数（= `object_count`）。
    pub item_count: i32,
    /// 6 条断言的通过记录。
    pub assertions: Vec<AssertionReport>,
}

// ==================== 对外入口 ====================

/// **结算（设计 §5.2 #29）**：一个事务内完成入库 + 凭证 + 状态回写 + 6 条断言。
///
/// 失败语义：任何一步失败（含 6 条断言里任一条）⇒ 整笔回滚，批次仍是 `APPROVED`，
/// 一张入库单 / 一张凭证 / 一条库存流水都不留 —— 用户可以修数据后再点一次结算。
pub async fn settle_period(id: Uuid, operator: Option<Uuid>) -> std::result::Result<SettleRun, ApiError> {
    // 预检（无锁）：把「状态不对」这类人话错误提前给出来，不开事务。
    let snapshot = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待结算的成本核算批次"))?
        .ok_or_else(|| cost_period_dto::not_found(id))?;
    if snapshot.status != STATUS_APPROVED {
        return Err(cost_period_dto::status_guard(
            &snapshot.batch_no,
            "结算",
            &snapshot.status,
            &[STATUS_APPROVED],
        ));
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启结算事务"))?;
    let outcome = async {
        let locked = lock_period(&tx, id).await?;
        ensure_no_pending_reverse(&tx, &locked.period_code).await?;
        let day = period_end_of(&tx, &locked.period_code, now).await?;
        let input = SettleInput {
            period_id: locked.id,
            period_code: &locked.period_code,
            batch_no: &locked.batch_no,
            labor_source: &locked.labor_source,
            in_date: day,
            voucher_date: day,
            operator,
            now,
        };
        settle_in_tx(&tx, &input).await
    }
    .await;

    match outcome {
        Ok(run) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交结算事务"))?;
            Ok(run)
        }
        Err(error) => {
            // 回滚不是「尽力而为」：拿不到 rollback 也要把原错误抛出去（连接断开时 PG 自己回滚）。
            if let Err(rollback) = tx.rollback().await {
                println!("[cost_settle] 结算回滚失败（{error:?}）：{rollback}");
            }
            Err(error)
        }
    }
}

/// 事务内的结算编排（②–⑤，**不含断言**）。**不提交**：调用方负责事务边界。
///
/// 与 [`settle_in_tx`] 拆开是为了让真库用例能在「写完但还没断言」的位置制造一个缺口，
/// 再单独跑断言 —— 那是「第 6 条失败 ⇒ 前面全回滚」唯一能真验的姿势（见
/// `cost_settle_run_db_tests`）。生产路径只走 [`settle_in_tx`]。
pub async fn apply_in_tx(ex: &impl SqlExecutor, input: &SettleInput<'_>) -> std::result::Result<SettleRun, ApiError> {
    // ② 计算单（先读：入库会改 `stock_in_id` / `status`，金额列不变）
    let items = cv::batch_items(ex, input.period_id)
        .await
        .map_err(|error| utils::db_error(error, "结算：读计算单"))?;
    if items.is_empty() {
        return Err(cost_period_dto::conflict(format!(
            "成本核算批次 {} 没有任何计算单：请先跑 calc（#27）再结算",
            input.batch_no
        )));
    }

    // ③ 完工入库（B3-4）
    let stock_ins = cost_settle_finish::generate(ex, &input.finish_context()).await?;

    // ④ 三 / 四张凭证（B3-5）
    let vouchers = cv::post_all(ex, &input.voucher_context()).await?;

    // ⑤ 状态回写
    let totals = totals_of(&items);
    let item_count = i32::try_from(items.len()).unwrap_or(i32::MAX);
    write_back(ex, input, &totals, item_count, &vouchers).await?;

    Ok(SettleRun {
        period_id: input.period_id,
        batch_no: input.batch_no.to_string(),
        stock_ins,
        vouchers,
        totals,
        item_count,
        assertions: Vec::new(),
    })
}

/// 事务内的结算 = ②–⑤ 写入 + ⑥ 6 条断言。**不提交**：调用方负责事务边界。
pub async fn settle_in_tx(ex: &impl SqlExecutor, input: &SettleInput<'_>) -> std::result::Result<SettleRun, ApiError> {
    let mut run = apply_in_tx(ex, input).await?;
    // ⑥ 事务内 6 条断言（读的是本事务刚写入的数据）
    let items = cv::batch_items(ex, input.period_id)
        .await
        .map_err(|error| utils::db_error(error, "结算：断言前重读计算单"))?;
    run.assertions = run_asserts(ex, input, &run.vouchers, &items).await?;
    Ok(run)
}

// ==================== ① 锁 + 前置校验 ====================

/// 锁住的批次行（`FOR UPDATE`）。
#[derive(Debug, Clone)]
pub struct LockedPeriod {
    /// 主键。
    pub id: Uuid,
    /// 期间码。
    pub period_code: String,
    /// 批次号。
    pub batch_no: String,
    /// 状态。
    pub status: String,
    /// 人工来源。
    pub labor_source: String,
}

/// `SELECT … FOR UPDATE` 锁批次行，并**在锁内复核状态**（并发双结算的正面闸）。
async fn lock_period(ex: &impl SqlExecutor, id: Uuid) -> std::result::Result<LockedPeriod, ApiError> {
    let row = ex
        .query_first_sql(
            "SELECT id, period_code, batch_no, status, labor_source \
               FROM biz_cost_period \
              WHERE id = ?::uuid AND deleted_at IS NULL \
              FOR UPDATE"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定待结算批次"))?
        .ok_or_else(|| cost_period_dto::not_found(id))?;
    let locked = LockedPeriod {
        id: row.get_typed("id")?,
        period_code: row.get_typed("period_code")?,
        batch_no: row.get_typed("batch_no")?,
        status: row.get_typed("status")?,
        labor_source: row.get_typed("labor_source")?,
    };
    if locked.status != STATUS_APPROVED {
        return Err(cost_period_dto::status_guard(
            &locked.batch_no,
            "结算",
            &locked.status,
            &[STATUS_APPROVED],
        ));
    }
    Ok(locked)
}

/// 期间已结账 / 有红冲在跑 ⇒ 一律拒（设计 §4.8 ②）。
///
/// 期间闸 `post_voucher` 里也有一道（真闸）；这里提前拦是为了给「请先反结账」这句人话。
async fn ensure_no_pending_reverse(ex: &impl SqlExecutor, period_code: &str) -> std::result::Result<(), ApiError> {
    if voucher::is_period_closed(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询期间结账状态"))?
    {
        return Err(cost_period_dto::conflict(format!(
            "会计期间 {period_code} 已结账，不能结算成本批次：要结算请先反结账（财务总监权限），\
             结账后的凭证写不进去"
        )));
    }
    let pending: i64 = ex
        .query_first_sql(
            "SELECT count(*)::bigint AS n \
               FROM biz_voucher v \
              WHERE v.period = ? AND v.deleted_at IS NULL \
                AND v.pending_reverse_instance_id IS NOT NULL \
                AND v.source_type = ?"
                .to_string(),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(cv::SOURCE_TYPE_COST_PERIOD.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询期间红冲状态"))?
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()?
        .unwrap_or(0);
    if pending > 0 {
        return Err(cost_period_dto::conflict(format!(
            "会计期间 {period_code} 有 {pending} 张成本结算凭证正在走红冲审批：\
             等红冲结束（或撤回）后再结算，否则新旧账会交叉"
        )));
    }
    Ok(())
}

/// 期间末日：先查 `biz_accounting_period` 的 `end_date`（期间可能不按自然月切），
/// 查不到就退化成日期所在月的最后一天（与 `voucher::period_of` 的 fail-open 同口径）。
pub async fn period_end_of(
    ex: &impl SqlExecutor,
    period_code: &str,
    now: DateTime<Utc>,
) -> std::result::Result<NaiveDate, ApiError> {
    let row = ex
        .query_first_sql(
            "SELECT end_date FROM biz_accounting_period WHERE period_code = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询会计期间末日"))?;
    if let Some(row) = row {
        let end: NaiveDate = row.get_typed("end_date")?;
        return Ok(end);
    }
    month_end(period_code).ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "期间码 {period_code} 不是 yyyy-MM 形状，算不出期间末日（现在 {:?}）",
                now.date_naive()
            ),
        )
    })
}

/// `yyyy-MM` → 该月最后一天（纯函数，单测覆盖闰年）。
pub fn month_end(period_code: &str) -> Option<NaiveDate> {
    let (year, month) = period_code.split_once('-')?;
    let year: i32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let (next_year, next_month) = if month == 12 {
        (year.checked_add(1)?, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)?.pred_opt()
}

// ==================== ⑤ 状态回写 ====================

/// 计算单合计（与 `period_model::totals_of` 同口径：金额 4 位、逐列求和）。
pub fn totals_of(items: &[ItemCostRow]) -> model::PeriodTotals {
    let mut totals = model::PeriodTotals {
        object_count: i64::try_from(items.len()).unwrap_or(i64::MAX),
        ..model::PeriodTotals::default()
    };
    for item in items {
        totals.total_material += item.mat_cur;
        totals.total_labor += item.labor_cur;
        totals.total_overhead += item.oh_cur;
        totals.total_finished += item.finished_cost;
        totals.total_wip += item.wip_end_cost;
    }
    totals.total_cost = totals.total_material + totals.total_labor + totals.total_overhead;
    totals
}

/// 状态机 `APPROVED → SETTLED` + CAS 回写单头（凭证 id / 合计 / 对象数 / 结算人）。
async fn write_back(
    ex: &impl SqlExecutor,
    input: &SettleInput<'_>,
    totals: &model::PeriodTotals,
    item_count: i32,
    vouchers: &SettledVouchers,
) -> std::result::Result<(), ApiError> {
    state_machine::transition(
        ex,
        state_machine::ENTITY_COST_PERIOD,
        input.period_id,
        input.batch_no,
        &model::LEGAL_EDGES,
        STATUS_APPROVED,
        STATUS_SETTLED,
        input.operator,
        None,
        None,
    )
    .await?;

    let affected = ex
        .exec_sql(
            "UPDATE biz_cost_period \
                SET status = ?, settled_at = ?, settled_by = NULLIF(?::text, '')::uuid, \
                    labor_voucher_id = NULLIF(?::text, '')::uuid, \
                    overhead_voucher_id = NULLIF(?::text, '')::uuid, \
                    finish_voucher_id = ?::uuid, \
                    object_count = ?, total_material = ?::text::numeric, \
                    total_labor = ?::text::numeric, total_overhead = ?::text::numeric, \
                    total_cost = ?::text::numeric, total_finished = ?::text::numeric, \
                    total_wip = ?::text::numeric, updated_at = ? \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(STATUS_SETTLED.to_string()),
                Value::from(input.now),
                Value::Text(input.operator.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(vouchers.labor.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(vouchers.overhead.map(|id| id.to_string()).unwrap_or_default()),
                Value::from(&vouchers.finish),
                Value::from(item_count),
                Value::Text(totals.total_material.to_string()),
                Value::Text(totals.total_labor.to_string()),
                Value::Text(totals.total_overhead.to_string()),
                Value::Text(totals.total_cost.to_string()),
                Value::Text(totals.total_finished.to_string()),
                Value::Text(totals.total_wip.to_string()),
                Value::from(input.now),
                Value::from(&input.period_id),
                Value::Text(STATUS_APPROVED.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "结算：回写批次状态与合计"))?;
    if affected == 0 {
        return Err(cost_period_dto::conflict(format!(
            "成本核算批次 {} 已不是「审批通过」（并发结算 / 已被改状态），本次结算作废",
            input.batch_no
        )));
    }
    Ok(())
}

// ==================== ⑥ 6 条断言 ====================

/// 断言失败的统一错误出口（409 + `66077`）。
fn assert_failed(index: usize, detail: String) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_SETTLE_ASSERT,
        format!(
            "结算事务断言 {}「{}」不平，整个结算已回滚：{detail}",
            ASSERT_IDS[index], ASSERT_NAMES[index]
        ),
    )
}

/// 容差比较（`NUMERIC(18,4)` 下的「一分钱以内」）。
pub fn within_tolerance(left: Decimal, right: Decimal) -> bool {
    let tolerance = TOLERANCE.parse::<Decimal>().unwrap_or(Decimal::ZERO);
    (left - right).abs() <= tolerance
}

/// 逐条跑 6 条断言：任何一条失败 ⇒ 返回 `Err`，调用方（结算事务）随即回滚。
pub async fn run_asserts(
    ex: &impl SqlExecutor,
    input: &SettleInput<'_>,
    vouchers: &SettledVouchers,
    items: &[ItemCostRow],
) -> std::result::Result<Vec<AssertionReport>, ApiError> {
    let mut reports = Vec::with_capacity(ASSERT_IDS.len());
    reports.push(assert_pool_matches_ledger(ex, input.period_code).await?);
    reports.push(assert_finished_matches_voucher(ex, input.period_id, items, vouchers).await?);
    reports.push(assert_stock_in_matches_items(ex, input.period_id).await?);
    reports.push(assert_wip_matches_production_balance(ex, input.period_id, input.period_code, items).await?);
    reports.push(assert_overhead_pools_cleared(ex, input.period_code).await?);
    reports.push(assert_alloc_conserves_pool(ex, input.period_code).await?);
    Ok(reports)
}

/// 断言 1（`C-COST-1`）：当期池 = 当期 `4101.*` 借方发生额，按 (车间, 科目) 逐组对平。
///
/// 复用 calc 第 4 段的同一份实现（`Stage::Settle` 认三路来源全部已入账），失败码 `66065`。
pub async fn assert_pool_matches_ledger(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> std::result::Result<AssertionReport, ApiError> {
    let outcome = cost_calc_reconcile::reconcile_in_tx(ex, period_code, Stage::Settle).await?;
    if !outcome.balanced() {
        // 复用 `66065` 的报文（差额分组 + 凭证号 + 修法都在里面）
        reconcile_dto::ensure_balanced(&outcome)?;
    }
    Ok(AssertionReport {
        id: ASSERT_IDS[0],
        name: ASSERT_NAMES[0],
        detail: outcome.summary_line(),
    })
}

/// 断言 2（`C-COST-2`）：`Σ 生产入库单金额` = 完工结转凭证 **1405 借方** = `Σ finished_cost`。
///
/// 三处必须同源：入库单是库存侧，凭证是账面侧，计算单是成本侧。任何一处单飞都是 bug。
pub async fn assert_finished_matches_voucher(
    ex: &impl SqlExecutor,
    period_id: Uuid,
    items: &[ItemCostRow],
    vouchers: &SettledVouchers,
) -> std::result::Result<AssertionReport, ApiError> {
    let stock_total = scalar_money(
        ex,
        "SELECT COALESCE(sum(total_amount), 0)::text AS amount FROM biz_stock_in \
          WHERE cost_period_id = ?::uuid AND deleted_at IS NULL AND in_type = ? AND status <> ?",
        vec![
            Value::from(&period_id),
            Value::Text(cost_settle_finish::IN_TYPE_PRODUCTION.to_string()),
            Value::Text(STOCK_IN_CANCELLED.to_string()),
        ],
        "结算断言：读本批次入库金额合计",
    )
    .await?;
    let voucher_debit = scalar_money(
        ex,
        "SELECT COALESCE(sum(e.debit_amount), 0)::text AS amount \
           FROM biz_voucher_entry e \
          WHERE e.voucher_id = ?::uuid AND e.account_code = ?",
        vec![
            Value::from(&vouchers.finish),
            Value::Text(ACCOUNT_FINISHED_GOODS.to_string()),
        ],
        "结算断言：读完工结转凭证 1405 借方",
    )
    .await?;
    let item_total = items.iter().fold(Decimal::ZERO, |sum, item| sum + item.finished_cost);

    let detail = format!(
        "入库单合计 {stock_total} / 完工凭证 1405 借方 {voucher_debit} / Σ完工成本 {item_total} \
         （凭证 {finish}）",
        finish = vouchers.finish
    );
    if !within_tolerance(stock_total, voucher_debit)
        || !within_tolerance(voucher_debit, item_total)
        || !within_tolerance(vouchers.finish_total, item_total)
    {
        return Err(assert_failed(
            1,
            format!(
                "{detail}；差异 = 入库 {}、凭证 {}。修法：核对 biz_stock_in / 完工凭证 / 计算单 \
                 哪一侧被改过，重算该期间后再结算",
                stock_total - item_total,
                voucher_debit - item_total
            ),
        ));
    }
    Ok(AssertionReport {
        id: ASSERT_IDS[1],
        name: ASSERT_NAMES[1],
        detail,
    })
}

/// 断言 3（`C-COST-2·行`）：`completed_qty > 0` 的计算单必须已绑单且被入库单**逐行**背书。
///
/// 判据（一条 SQL 数「不符行数」，必须为 0）：
/// * 计算单 `status = 'SETTLED'` 且 `stock_in_id` 非空；
/// * 对应的入库单存在、未作废、`total_qty = completed_qty`、`total_amount = finished_cost`；
/// * 入库单张数 = 有完工的计算单行数（不多不少）。
pub async fn assert_stock_in_matches_items(
    ex: &impl SqlExecutor,
    period_id: Uuid,
) -> std::result::Result<AssertionReport, ApiError> {
    let broken = scalar_i64(
        ex,
        "SELECT count(*)::bigint AS n \
           FROM biz_cost_period_item i \
           LEFT JOIN biz_stock_in s \
                  ON s.id = i.stock_in_id AND s.deleted_at IS NULL AND s.status <> ? \
          WHERE i.period_id = ?::uuid AND i.completed_qty > 0 \
            AND (i.stock_in_id IS NULL OR i.status <> ? OR s.id IS NULL \
                 OR s.total_qty <> i.completed_qty OR s.total_amount <> i.finished_cost)",
        vec![
            Value::Text(STOCK_IN_CANCELLED.to_string()),
            Value::from(&period_id),
            Value::Text(ITEM_SETTLED.to_string()),
        ],
        "结算断言：读入库单 ↔ 计算单逐行差异",
    )
    .await?;
    let items_with_output = scalar_i64(
        ex,
        "SELECT count(*)::bigint AS n FROM biz_cost_period_item \
          WHERE period_id = ?::uuid AND completed_qty > 0",
        vec![Value::from(&period_id)],
        "结算断言：读有完工的计算单行数",
    )
    .await?;
    let stock_ins = scalar_i64(
        ex,
        "SELECT count(*)::bigint AS n FROM biz_stock_in \
          WHERE cost_period_id = ?::uuid AND deleted_at IS NULL AND status <> ?",
        vec![Value::from(&period_id), Value::Text(STOCK_IN_CANCELLED.to_string())],
        "结算断言：读本批次入库单张数",
    )
    .await?;

    let detail = format!("有完工的计算单 {items_with_output} 行 / 本批次入库单 {stock_ins} 张，逐行不符 {broken} 行");
    if broken > 0 || items_with_output != stock_ins {
        return Err(assert_failed(
            2,
            format!(
                "{detail}；逐行背书要求「每个有完工的对象恰有一张入库单，数量金额相等」。\
                 修法：先红冲该批次（把入库单置 CANCELLED）再重算重结"
            ),
        ));
    }
    Ok(AssertionReport {
        id: ASSERT_IDS[2],
        name: ASSERT_NAMES[2],
        detail,
    })
}

/// 断言 4（`C-COST-3`）：`4001.01/.02/.03` 本期净发生 = `Σ(wip_end_cost − wip_beg_cost)`。
///
/// 「本期净发生」含**红字凭证**（`is_red = 1`）：红冲后同期间重结时，旧批次的蓝字 + 红字
/// 必须互抵为 0 才对（见模块头「断言 4 为什么是本期净发生」）。
pub async fn assert_wip_matches_production_balance(
    ex: &impl SqlExecutor,
    period_id: Uuid,
    period_code: &str,
    items: &[ItemCostRow],
) -> std::result::Result<AssertionReport, ApiError> {
    let net = scalar_money(
        ex,
        "SELECT COALESCE(sum(e.debit_amount - e.credit_amount), 0)::text AS amount \
           FROM biz_voucher_entry e \
           JOIN biz_voucher v ON v.id = e.voucher_id \
          WHERE v.period = ? AND v.status = 'POSTED' AND v.deleted_at IS NULL \
            AND e.account_code IN (?, ?, ?)",
        vec![
            Value::Text(period_code.to_string()),
            Value::Text(ACCOUNTS_WIP[0].to_string()),
            Value::Text(ACCOUNTS_WIP[1].to_string()),
            Value::Text(ACCOUNTS_WIP[2].to_string()),
        ],
        "结算断言：读 4001.* 本期净发生",
    )
    .await?;
    let wip_beg = scalar_money(
        ex,
        "SELECT COALESCE(sum(wip_beg_cost), 0)::text AS amount \
           FROM biz_cost_period_item WHERE period_id = ?::uuid",
        vec![Value::from(&period_id)],
        "结算断言：读计算单期初在制",
    )
    .await?;
    let wip_end = items.iter().fold(Decimal::ZERO, |sum, item| sum + item.wip_end_cost);
    let expected = wip_end - wip_beg;

    let detail = format!("4001 本期净发生 {net} / Σ(期末在制 {wip_end} − 期初在制 {wip_beg}) = {expected}");
    if !within_tolerance(net, expected) {
        return Err(assert_failed(
            3,
            format!(
                "{detail}；差异 {}。含义：料 / 工 / 费的借方没有全落在 4001.*，或完工结转贷方\
                 与计算单的完工成本不是同一次 calc 的结果。修法：核对 4101 结转与领料凭证\
                 （借方是否 4001.01）后重算该期间",
                net - expected
            ),
        ));
    }
    Ok(AssertionReport {
        id: ASSERT_IDS[3],
        name: ASSERT_NAMES[3],
        detail,
    })
}

/// 断言 5（`C-COST-4·清零`）：每个 (车间, 科目) 的 `4101.*` 本期借方 = 贷方（池余额 0）。
pub async fn assert_overhead_pools_cleared(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> std::result::Result<AssertionReport, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT COALESCE(e.aux_dept_id::text, '') AS dept_id, e.account_code, \
                    sum(e.debit_amount)::text AS debit, sum(e.credit_amount)::text AS credit \
               FROM biz_voucher_entry e \
               JOIN biz_voucher v ON v.id = e.voucher_id \
              WHERE v.period = ? AND v.status = 'POSTED' AND v.deleted_at IS NULL \
                AND e.account_code LIKE ? \
              GROUP BY e.aux_dept_id, e.account_code \
              ORDER BY e.account_code ASC, dept_id ASC"
                .to_string(),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(format!("{PREFIX_OVERHEAD}%")),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "结算断言：读 4101.* 本期借贷"))?;
    let mut groups = 0_usize;
    let mut leaks: Vec<String> = Vec::new();
    for row in &rows {
        groups += 1;
        let debit = parse_money(&row.get_typed::<String>("debit")?)?;
        let credit = parse_money(&row.get_typed::<String>("credit")?)?;
        if !within_tolerance(debit, credit) {
            let dept_id: String = row.get_typed("dept_id")?;
            let account: String = row.get_typed("account_code")?;
            leaks.push(format!(
                "车间 {} / 科目 {account}：借 {debit} − 贷 {credit} = {}",
                if dept_id.is_empty() {
                    "（未指定）".to_string()
                } else {
                    dept_id
                },
                debit - credit
            ));
        }
    }
    let detail = format!("4101.* 本期 {groups} 个 (车间,科目) 组，未清零 {} 组", leaks.len());
    if !leaks.is_empty() {
        return Err(assert_failed(
            4,
            format!(
                "{detail}：{}。含义：有人绕过成本模块直接记了 4101（旁路记账），或分摊 / \
                 手工台账凭证没把这些池子摊完。修法：按上面的差额找出那张凭证改走成本模块后重算再结",
                leaks.join("；")
            ),
        ));
    }
    Ok(AssertionReport {
        id: ASSERT_IDS[4],
        name: ASSERT_NAMES[4],
        detail,
    })
}

/// 断言 6（`C-COST-4·守恒`）：`Σ biz_cost_alloc_result.alloc_amount` = `Σ biz_cost_collect(池行)`。
///
/// 分摊结果不参与任何凭证，所以它是**唯一只被本条抓到**的缺口（少摊 / 多摊 / 漏摊期外对象）。
pub async fn assert_alloc_conserves_pool(
    ex: &impl SqlExecutor,
    period_code: &str,
) -> std::result::Result<AssertionReport, ApiError> {
    let pool = scalar_money(
        ex,
        "SELECT COALESCE(sum(amount), 0)::text AS amount FROM biz_cost_collect \
          WHERE period_code = ? AND element = 'OVERHEAD' AND cost_object_id IS NULL \
            AND deleted_at IS NULL",
        vec![Value::Text(period_code.to_string())],
        "结算断言：读当期费用池合计",
    )
    .await?;
    let allocated = scalar_money(
        ex,
        "SELECT COALESCE(sum(alloc_amount), 0)::text AS amount FROM biz_cost_alloc_result \
          WHERE period_code = ?",
        vec![Value::Text(period_code.to_string())],
        "结算断言：读当期分摊结果合计",
    )
    .await?;
    let detail = format!("池 {pool} / 已分摊 {allocated}（差额 {}）", pool - allocated);
    if !within_tolerance(pool, allocated) {
        return Err(assert_failed(
            5,
            format!(
                "{detail}。含义：分摊没守恒（少数了分摊结果行 / 基数合计 0 的池被静默丢掉），\
                 池里的钱没全摊到成本对象身上 —— 摊不掉的会在 4101 留余额。\
                 修法：重算该期间（calc 第 5 段会按最大余数法重建分摊结果）"
            ),
        ));
    }
    Ok(AssertionReport {
        id: ASSERT_IDS[5],
        name: ASSERT_NAMES[5],
        detail,
    })
}

// ==================== 小工具 ====================

/// 读一个金额标量（`::text` 出去再解析，避免 `NUMERIC` 的类型映射差异）。
async fn scalar_money(
    ex: &impl SqlExecutor,
    sql: &str,
    params: Vec<Value>,
    context: &str,
) -> std::result::Result<Decimal, ApiError> {
    let row = ex
        .query_first_sql(sql.to_string(), params)
        .await
        .map_err(|error| utils::db_error(error, context))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("{context}：聚合查询没有返回行（不该发生）"),
            )
        })?;
    parse_money(&row.get_typed::<String>("amount")?)
}

/// 读一个计数标量。
async fn scalar_i64(
    ex: &impl SqlExecutor,
    sql: &str,
    params: Vec<Value>,
    context: &str,
) -> std::result::Result<i64, ApiError> {
    let row = ex
        .query_first_sql(sql.to_string(), params)
        .await
        .map_err(|error| utils::db_error(error, context))?
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                format!("{context}：聚合查询没有返回行（不该发生）"),
            )
        })?;
    let count: i64 = row.get_typed("n")?;
    Ok(count)
}

/// `::text` → `Decimal`。
fn parse_money(text: &str) -> std::result::Result<Decimal, ApiError> {
    text.trim().parse::<Decimal>().map_err(|_| {
        ApiError::new(
            500,
            error_code::INTERNAL_ERROR,
            format!("结算断言：金额列返回了非数字 {text:?}"),
        )
    })
}

/// 当期码 → 该月第一天（筛选 / 报文用；纯函数）。
pub fn month_start(period_code: &str) -> Option<NaiveDate> {
    let (year, month) = period_code.split_once('-')?;
    NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, 1)
}

/// 期间码是否 `yyyy-MM`。
pub fn valid_period_code(period_code: &str) -> bool {
    matches!(month_start(period_code), Some(date) if date.month() >= 1 && date.month() <= 12)
}

// ==================== 单测（纯逻辑 / 常量对齐；真库用例见 cost_settle_run_db_tests） ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertion_ids_and_names_are_paired() {
        assert_eq!(ASSERT_IDS.len(), 6, "卡面要求 6 条断言");
        assert_eq!(ASSERT_NAMES.len(), ASSERT_IDS.len());
        // 顺序是契约（第 6 条必须是「分摊守恒」，可独立制造失败，见模块头）
        assert_eq!(ASSERT_IDS[0], "C-COST-1");
        assert_eq!(ASSERT_IDS[5], "C-COST-4·守恒");
        assert!(ASSERT_NAMES[5].contains("分摊守恒"));
    }

    #[test]
    fn accounts_match_the_voucher_module() {
        // 4001 的三末级与 4101 前缀必须与 B3-5 的凭证模块同一份真相
        assert_eq!(ACCOUNTS_WIP, ["4001.01", "4001.02", "4001.03"]);
        assert_eq!(ACCOUNTS_WIP[0], cv::ACCOUNT_MATERIAL);
        assert_eq!(ACCOUNTS_WIP[1], cv::ACCOUNT_LABOR);
        assert_eq!(ACCOUNTS_WIP[2], cv::ACCOUNT_OVERHEAD);
        assert_eq!(ACCOUNT_FINISHED_GOODS, "1405");
        assert_eq!(PREFIX_OVERHEAD, "4101.");
    }

    #[test]
    fn month_end_handles_leap_years_and_year_end() {
        assert_eq!(month_end("2026-11"), NaiveDate::from_ymd_opt(2026, 11, 30));
        assert_eq!(month_end("2026-02"), NaiveDate::from_ymd_opt(2026, 2, 28));
        assert_eq!(month_end("2024-02"), NaiveDate::from_ymd_opt(2024, 2, 29));
        assert_eq!(month_end("2026-12"), NaiveDate::from_ymd_opt(2026, 12, 31));
        assert_eq!(month_end("2026-13"), None);
        assert_eq!(month_end("2026-00"), None);
        assert_eq!(month_end("bad"), None);
    }

    #[test]
    fn period_code_validity() {
        assert!(valid_period_code("2026-11"));
        assert!(valid_period_code("2600-01"));
        assert!(!valid_period_code("2026-13"));
        assert!(!valid_period_code("202611"));
        assert_eq!(month_start("2026-11"), NaiveDate::from_ymd_opt(2026, 11, 1));
    }

    #[test]
    fn tolerance_is_one_hundredth_of_a_cent() {
        let cent = "0.0001".parse::<Decimal>().unwrap();
        assert!(within_tolerance(Decimal::ZERO, cent));
        assert!(within_tolerance(cent, Decimal::ZERO));
        assert!(!within_tolerance(Decimal::ZERO, cent + cent));
        assert_eq!(TOLERANCE, "0.0001");
    }

    #[test]
    fn totals_of_sums_every_column_and_counts_objects() {
        let item = |mat: &str, labor: &str, oh: &str, finished: &str, wip: &str| ItemCostRow {
            item_id: Uuid::now_v7(),
            cost_object_id: Uuid::now_v7(),
            object_no: "CB-1".to_string(),
            product_id: Uuid::now_v7(),
            warehouse_id: Uuid::now_v7(),
            completed_qty: Decimal::from(800),
            mat_cur: mat.parse().unwrap(),
            labor_cur: labor.parse().unwrap(),
            oh_cur: oh.parse().unwrap(),
            unit_material: Decimal::ZERO,
            unit_labor: Decimal::ZERO,
            unit_overhead: Decimal::ZERO,
            finished_cost: finished.parse().unwrap(),
            wip_end_cost: wip.parse().unwrap(),
        };
        // 算例 A 的两行：合计仍是 19000 / 4000 / 4000 / 22311.1104 / 4688.8896
        let items = vec![
            item("10000", "2000", "2000", "12000", "2000"),
            item("9000", "2000", "2000", "10311.1104", "2688.8896"),
        ];
        let totals = totals_of(&items);
        assert_eq!(totals.object_count, 2);
        assert_eq!(totals.total_material, "19000".parse::<Decimal>().unwrap());
        assert_eq!(totals.total_labor, "4000".parse::<Decimal>().unwrap());
        assert_eq!(totals.total_overhead, "4000".parse::<Decimal>().unwrap());
        assert_eq!(totals.total_cost, "27000".parse::<Decimal>().unwrap());
        assert_eq!(totals.total_finished, "22311.1104".parse::<Decimal>().unwrap());
        assert_eq!(totals.total_wip, "4688.8896".parse::<Decimal>().unwrap());
        // 守恒：投入 = 完工 + 在制
        assert_eq!(totals.total_cost, totals.total_finished + totals.total_wip);
    }

    #[test]
    fn assert_error_payload_names_the_assertion() {
        let error = assert_failed(5, "池 4000 / 已分摊 3900".to_string());
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_SETTLE_ASSERT);
        assert!(error.msg.contains("C-COST-4·守恒"), "报文：{}", error.msg);
        assert!(error.msg.contains("整个结算已回滚"), "报文：{}", error.msg);
    }

    #[test]
    fn assert_failed_code_is_the_registered_one() {
        assert_eq!(error_code::ERP_COST_SETTLE_ASSERT, 66077);
    }

    #[test]
    fn month_end_wont_overflow_on_extreme_years() {
        // i32 上限年份：next_year 会溢出 ⇒ 返回 None 而不是 panic
        assert_eq!(month_end("2147483647-12"), None);
    }
}

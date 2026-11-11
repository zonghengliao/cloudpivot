//! 红冲：逐凭证 `reverse_posted` + 库存回冲 + 生产入库单作废 + 状态 → `REVERSED`
//! （ARCH-9 §4.8 反向路径 / §5.2 #30 · COST-B3-7）。
//!
//! 设计：`docs/architecture/ARCH-9-成本核算升级设计.md` §4.8（反向场景）、
//! §4.1 状态机（`uk_biz_cost_period` 排除 `REVERSED` ⇒ 红冲后同期间可重建批次）。
//!
//! # 一次红冲 = 一个事务（设计 §4.8 反向路径逐行对应）
//!
//! ```text
//! BEGIN
//!  ① biz_cost_period FOR UPDATE；要求 status = 'SETTLED'；期间未结账
//!  ② 逐凭证 voucher::reverse_posted   （红字凭证，原凭证保持 POSTED 并打 reversed_by）
//!  ③ 逐对象回冲库存                    （inventory_writer::subtract_cost /
//!     subtract_location_qty / insert_txn(PRODUCE_CANCEL)）
//!  ④ 生产入库单作废                    （status='CANCELLED' + cancelled_at/by/reason + 状态日志）
//!  ⑤ 计算单回到未结算                  （status='CALC'、stock_in_id=NULL；§4.8「红冲后回到未结算」）
//!  ⑥ 批次状态 → REVERSED               （状态机 SETTLED→REVERSED + CAS 回写 reversed_*）
//! COMMIT
//! ```
//!
//! 提交后**没有任何会失败的账务动作**（无通知 / 无外部调用）⇒ 不构成 AGENTS 约定 11 的
//! 「提交后失败留死单」。
//!
//! # 为什么四段都在一个事务里（而不是先改状态再补账）
//!
//! 红冲是「撤销一次已经入账的结算」。任何一段单独提交都会留下**账实不符**：
//! 凭证红了但库存没退 ⇒ 存货虚高；库存退了但入库单还是 `POSTED` ⇒ 同一批货能再收一次。
//! 所以四段共用一个事务，任一段失败（**尤其是库存退不回来**）整笔回滚，批次仍是 `SETTLED`，
//! 用户可以走「销售退货 / 盘亏」把货补回来再红冲 —— 这是设计 §4.8 明确写的业务边界。
//!
//! # 红字凭证的日期为什么落在**被红冲的期间**
//!
//! `reverse_posted` 的凭证日期决定新凭证属于哪个会计期间。红冲的是「该期间的结算」，
//! 红字必须和蓝字落在**同一期间**才对平：`4001.* / 4101.* / 1405` 的「本期净发生」在
//! 红冲后立刻归零，随后同一期间的新批次重结时 6 条断言（B3-6）依然成立。
//! 若按「今天」生成，旧蓝字留在原期间、红字跑到新期间 ⇒ 两个期间的余额都不对，重结必红。
//! 期间已结账时 `post_voucher` 本身也会拒，所以本模块在开事务前先给「请先反结账」的人话。
//!
//! # 与设计的已知偏差（卡内报备）
//!
//! * 设计 §4.8 ① 允许 `status ∈ ('SETTLED','CLOSED')`，但状态机 `COST_PERIOD_EDGES` 只有
//!   `SETTLED → REVERSED` **一条**红冲边（`CLOSED → REVERSED` 不存在），且 B3 批次内
//!   没有任何代码把批次置 `CLOSED`（关期卡未实施）。所以本卡只接受 `SETTLED`；
//!   `CLOSED` 批次的红冲要等「关期 / 反关期」卡落地后补边（本卡不动边表，边表是 B3-1 定稿）。
//! * 单头 `labor/overhead/finish_voucher_id` **保留不置空**：`ck_biz_cost_period_settle` 要求
//!   `REVERSED` 仍有 `settled_at` 与 `finish_voucher_id`，且审计要能顺藤摸到「红了哪几张蓝字」。
//! * 红冲不审批（设计 §4.8 反向路径没有审批环节，只要求 `cost-period:reverse` 权限 + 原因 ≥10 字）。
//! ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_period_dto::{self, STATUS_REVERSED, STATUS_SETTLED};
use crate::business::erp::cost_period_model as model;
use crate::business::erp::cost_settle_finish::{
    BIZ_TYPE_PRODUCTION_IN, ITEM_CALC, ITEM_SETTLED, STOCK_IN_CANCELLED, STOCK_IN_POSTED,
};
use crate::business::erp::cost_settle_run;
use crate::business::erp::cost_settle_voucher as cv;
use crate::business::erp::inventory_writer::{self, TxnRecord};
use crate::business::erp::stockin_base::DOC_TYPE_STOCK_IN;
use crate::business::erp::stockin_model::{
    self, AppliedInventory, CancelDoc, CancelLine, MISSING_TXN_PREFIX, SHORTFALL_PREFIX,
};
use crate::business::erp::voucher::{self, ReversalInput};
use crate::business::shared::doc_kind::TxnBizType;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（与设计 / 词表逐字对齐，单测钉死） ====================

/// 库存流水方向：出（`ck_biz_inv_txn_type` 只认 `IN` / `OUT`）。
pub const TXN_TYPE_OUT: &str = "OUT";

/// 库存流水的业务类型：生产入库红冲（设计 §4.8 ③ 点名的 `PRODUCE_CANCEL`）。
///
/// 直接取词表枚举而不是手写字符串：`TxnBizType::ProduceCancel` 早就在词表里
/// （`doc_kind.rs`），写串字会有「改词表漏改这里」的漂移风险；`as_str()` 是 `const fn`。
pub const BIZ_TYPE_PRODUCE_CANCEL: &str = TxnBizType::ProduceCancel.as_str();

/// 数量容差（`NUMERIC(18,4)` 的最小单位）。
pub const QTY_TOLERANCE_SCALE: u32 = 4;

// ==================== 结构 ====================

/// 一次红冲的输入（批次快照 + 原因 + 操作人）。
#[derive(Debug, Clone)]
pub struct ReverseInput<'a> {
    /// 结算批次 id。
    pub period_id: Uuid,
    /// 期间码 `yyyy-MM`（写进反向流水的 `period_code`）。
    pub period_code: &'a str,
    /// 批次号 `CS-yyyyMM-NNNN`（状态日志 / 报文用）。
    pub batch_no: &'a str,
    /// 红冲原因（≥10 字；进红字凭证摘要、入库单 `cancel_reason`、批次 `reverse_reason`）。
    pub reason: &'a str,
    /// 红字凭证日期（= 被红冲期间的末日，见模块头）。
    pub voucher_date: NaiveDate,
    /// 操作人（`reversed_by` / 凭证 `posted_by` / 流水 `operator`）。
    pub operator: Option<Uuid>,
    /// 业务时间。
    pub now: DateTime<Utc>,
}

/// 一张已红冲的蓝字凭证 → 它的红字凭证。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReversedVoucher {
    /// 原（蓝字）凭证 id。
    pub original_id: Uuid,
    /// 原凭证号。
    pub original_no: String,
    /// 红字凭证 id。
    pub red_id: Uuid,
    /// 红字凭证号。
    pub red_no: String,
}

/// 一张已作废的完工入库单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReversedStockIn {
    /// 入库单 id。
    pub stock_in_id: Uuid,
    /// 入库单号。
    pub in_no: String,
    /// 回冲数量（= 原入库数量）。
    pub qty: Decimal,
    /// 回冲金额（= 原过账流水金额）。
    pub amount: Decimal,
}

/// 一次红冲的全部产物（调用方只用来报数 / 打日志）。
#[derive(Debug, Clone)]
pub struct ReverseRun {
    /// 批次 id。
    pub period_id: Uuid,
    /// 批次号。
    pub batch_no: String,
    /// 红冲的凭证（按凭证号顺序）。
    pub vouchers: Vec<ReversedVoucher>,
    /// 作废的入库单（按单号顺序）。
    pub stock_ins: Vec<ReversedStockIn>,
    /// 回到 `CALC` 的计算单行数。
    pub reset_items: u64,
}

impl ReverseRun {
    /// 红冲合计（数量, 金额）—— 与「库存回冲总额」对平用。
    pub fn totals(&self) -> (Decimal, Decimal) {
        self.stock_ins
            .iter()
            .fold((Decimal::ZERO, Decimal::ZERO), |(qty, amount), row| {
                (qty + row.qty, amount + row.amount)
            })
    }

    /// 一行摘要（日志 / 卡面报备用）。
    pub fn summary_line(&self) -> String {
        let (qty, amount) = self.totals();
        format!(
            "红冲 {}：红字凭证 {} 张 / 作废入库单 {} 张（回冲 {} 件 {} 元）/ 计算单回 CALC {} 行",
            self.batch_no,
            self.vouchers.len(),
            self.stock_ins.len(),
            qty.round_dp(4),
            amount.round_dp(4),
            self.reset_items
        )
    }
}

/// 一张待作废的完工入库单（单头三列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenStockIn {
    /// `biz_stock_in.id`。
    pub id: Uuid,
    /// 单号。
    pub in_no: String,
    /// 入库仓库（回冲流水要用）。
    pub warehouse_id: Uuid,
}

// ==================== 对外入口 ====================

/// **红冲（设计 §5.2 #30）**：一个事务内 红字凭证 + 库存回冲 + 入库单作废 + 状态回退。
///
/// 失败语义：任何一步失败（含库存退不回来）⇒ 整笔回滚，批次仍是 `SETTLED`，
/// 库存 / 凭证 / 入库单三处一条都不动 —— 用户把货补回（退货入库 / 盘盈）后可再点红冲。
pub async fn reverse_period(
    id: Uuid,
    reason: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<ReverseRun, ApiError> {
    // 原因校验（≥10 字）：在开事务前给 400，不浪费一次连接。
    let reason = cost_period_dto::require_reason(reason, cost_period_dto::REVERSE_REASON_MIN, "红冲")?;

    // 预检（无锁）：状态不对这类人话错误提前说，不开事务。
    let snapshot = model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询待红冲的成本核算批次"))?
        .ok_or_else(|| cost_period_dto::not_found(id))?;
    if snapshot.status != STATUS_SETTLED {
        return Err(cost_period_dto::status_guard(
            &snapshot.batch_no,
            "红冲",
            &snapshot.status,
            &[STATUS_SETTLED],
        ));
    }

    let now = Utc::now();
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启红冲事务"))?;
    let outcome = async {
        // ① 锁批次行 + 锁内复核状态
        let locked = lock_period(&tx, id).await?;
        ensure_period_open(&tx, &locked.period_code).await?;
        let voucher_date = cost_settle_run::period_end_of(&tx, &locked.period_code, now).await?;
        let input = ReverseInput {
            period_id: locked.id,
            period_code: &locked.period_code,
            batch_no: &locked.batch_no,
            reason: &reason,
            voucher_date,
            operator,
            now,
        };
        reverse_in_tx(&tx, &input).await
    }
    .await;

    match outcome {
        Ok(run) => {
            tx.commit()
                .await
                .map_err(|error| utils::db_error(error, "提交红冲事务"))?;
            tracing::info!(target: "cost", "{}", run.summary_line());
            Ok(run)
        }
        Err(error) => {
            // 回滚不是「尽力而为」：拿不到 rollback 也要把原错误抛出去（连接断开时 PG 自己回滚）。
            if let Err(rollback) = tx.rollback().await {
                println!("[cost_reverse] 红冲回滚失败（{error:?}）：{rollback}");
            }
            Err(error)
        }
    }
}

/// 事务内的红冲编排（②–⑥）。**不提交**：调用方负责事务边界。
///
/// 拆成独立函数是为了让真库用例能在「打到某一段失败」处注入缺口（如先出库把库存扣掉），
/// 再单独验证「整笔零写入」；生产路径只走 [`reverse_period`]。
pub async fn reverse_in_tx(
    ex: &impl SqlExecutor,
    input: &ReverseInput<'_>,
) -> std::result::Result<ReverseRun, ApiError> {
    // ② 逐凭证红冲
    let vouchers = reverse_vouchers(ex, input).await?;
    // ③④ 逐入库单：库存回冲 + 单头作废
    let stock_ins = reverse_stock_ins(ex, input).await?;
    // ⑤ 计算单回到未结算（可改数重算）
    let reset_items = reset_items(ex, input).await?;
    // ⑥ 批次 → REVERSED
    write_back(ex, input).await?;
    Ok(ReverseRun {
        period_id: input.period_id,
        batch_no: input.batch_no.to_string(),
        vouchers,
        stock_ins,
        reset_items,
    })
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
}

/// `SELECT … FOR UPDATE` 锁批次行，并**在锁内复核状态**（并发双红冲 / 红冲与结算互斥的正门）。
async fn lock_period(ex: &impl SqlExecutor, id: Uuid) -> std::result::Result<LockedPeriod, ApiError> {
    let row = ex
        .query_first_sql(
            "SELECT id, period_code, batch_no, status \
               FROM biz_cost_period \
              WHERE id = ?::uuid AND deleted_at IS NULL \
              FOR UPDATE"
                .to_string(),
            vec![Value::from(&id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定待红冲批次"))?
        .ok_or_else(|| cost_period_dto::not_found(id))?;
    let locked = LockedPeriod {
        id: row.get_typed("id")?,
        period_code: row.get_typed("period_code")?,
        batch_no: row.get_typed("batch_no")?,
        status: row.get_typed("status")?,
    };
    if locked.status != STATUS_SETTLED {
        return Err(cost_period_dto::status_guard(
            &locked.batch_no,
            "红冲",
            &locked.status,
            &[STATUS_SETTLED],
        ));
    }
    Ok(locked)
}

/// 期间已结账 ⇒ 一律拒（设计 §4.10「期间已结账：calc/settle 一律拒；要改就反结账」）。
///
/// `post_voucher` 里也有一道真闸，这里提前拦是为了给「请先反结账」这句人话 ——
/// 否则用户看到的是一句 `500`（`reverse_posted` 把 `DbError` 冒泡成内部错误）。
async fn ensure_period_open(ex: &impl SqlExecutor, period_code: &str) -> std::result::Result<(), ApiError> {
    if voucher::is_period_closed(ex, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询期间结账状态"))?
    {
        return Err(cost_period_dto::conflict(format!(
            "会计期间 {period_code} 已结账，红冲凭证写不进去：请先反结账（财务总监权限）再红冲。\
             红字凭证必须落在被红冲的同一期间，结账期间不许再记账"
        )));
    }
    Ok(())
}

// ==================== ② 凭证红冲 ====================

/// 该批次的全部**未红冲蓝字**结算凭证 → 逐张 `reverse_posted`。
///
/// 顺序 = `voucher_no` 升序（由 `cv::voucher_ids_of` 保证）：并发红冲两张批次时按同一全序
/// 拿凭证行锁，不会交叉等待。
///
/// 没有凭证 ⇒ 409 + `66078`：`SETTLED` 却一张凭证都没有，是账面数据坏了
/// （库层 `ck_biz_cost_period_settle` 要求 `finish_voucher_id` 非空），不能「静默当红冲完成」。
async fn reverse_vouchers(
    ex: &impl SqlExecutor,
    input: &ReverseInput<'_>,
) -> std::result::Result<Vec<ReversedVoucher>, ApiError> {
    let ids = cv::voucher_ids_of(ex, input.period_id)
        .await
        .map_err(|error| utils::db_error(error, "红冲：查该批次的结算凭证"))?;
    if ids.is_empty() {
        return Err(ApiError::new(
            409,
            error_code::ERP_COST_REVERSE_BLOCKED,
            format!(
                "成本核算批次 {} 是「已结算」状态，但查不到任何未红冲的结算凭证：账面数据不完整，\
                 不能红冲（红冲必须有蓝字凭证可红）。请先用巡检脚本核对 biz_voucher 与批次的关系",
                input.batch_no
            ),
        ));
    }
    let mut done = Vec::with_capacity(ids.len());
    for id in ids {
        let original = voucher::find_voucher_locked(ex, id)
            .await
            .map_err(|error| utils::db_error(error, "红冲：锁待红冲凭证"))?
            .ok_or_else(|| {
                ApiError::new(
                    409,
                    error_code::ERP_COST_REVERSE_BLOCKED,
                    format!(
                        "成本核算批次 {} 的结算凭证 {id} 在锁定时已不存在（被删 / 被并发红冲）：\
                         请刷新后重试，若反复出现请查 biz_voucher 的 deleted_at",
                        input.batch_no
                    ),
                )
            })?;
        let red_id = voucher::reverse_posted(
            ex,
            &original,
            ReversalInput {
                reason: Some(input.reason),
                voucher_date: input.voucher_date,
            },
            input.operator,
            input.now,
        )
        .await
        .map_err(|error| reversal_error(error, "红冲：生成红字凭证"))?;
        let red_no: Option<String> = ex
            .scalar_sql(
                "SELECT voucher_no FROM biz_voucher WHERE id = ?::uuid".to_string(),
                vec![Value::from(&red_id)],
            )
            .await
            .map_err(|error| utils::db_error(error, "红冲：读红字凭证号"))?;
        let red_no = red_no.unwrap_or_else(|| red_id.to_string());
        done.push(ReversedVoucher {
            original_id: original.id,
            original_no: original.voucher_no.clone(),
            red_id,
            red_no,
        });
    }
    Ok(done)
}

// ==================== ③④ 库存回冲 + 入库单作废 ====================

/// 该批次**未作废**的完工入库单（按单号排序 = 回冲的锁顺序）。
pub async fn open_stock_ins_of(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<Vec<OpenStockIn>> {
    let rows = ex
        .query_sql(
            "SELECT id, in_no, warehouse_id FROM biz_stock_in \
              WHERE cost_period_id = ?::uuid AND deleted_at IS NULL AND status = ? \
              ORDER BY in_no ASC, id ASC"
                .to_string(),
            vec![Value::from(&period_id), Value::Text(STOCK_IN_POSTED.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(OpenStockIn {
                id: row.get_typed("id")?,
                in_no: row.get_typed("in_no")?,
                warehouse_id: row.get_typed("warehouse_id")?,
            })
        })
        .collect()
}

/// 逐入库单：库存三本账回冲（成本账 → 库位账 → 反向流水）+ 单头作废。
async fn reverse_stock_ins(
    ex: &impl SqlExecutor,
    input: &ReverseInput<'_>,
) -> std::result::Result<Vec<ReversedStockIn>, ApiError> {
    let stock_ins = open_stock_ins_of(ex, input.period_id)
        .await
        .map_err(|error| utils::db_error(error, "红冲：查本批次的完工入库单"))?;
    let mut done = Vec::with_capacity(stock_ins.len());
    for stock_in in &stock_ins {
        let plan = reverse_plan_of(ex, stock_in.id, &stock_in.in_no, stock_in.warehouse_id)
            .await
            .map_err(|error| reversal_error(error, "红冲：读完工入库的过账流水"))?;
        let doc = CancelDoc {
            id: stock_in.id,
            in_no: &stock_in.in_no,
        };
        let (mut qty, mut amount) = (Decimal::ZERO, Decimal::ZERO);
        // 锁顺序 = 计划排序（material_id 升序，与结算写入同序）⇒ 不产生交叉等待
        for line in &plan {
            let applied = stockin_model::reverse_cost(
                ex,
                line.material_id,
                stock_in.warehouse_id,
                line.location_id,
                line,
                input.now,
            )
            .await
            .map_err(|error| reversal_error(error, "红冲：成本账回冲（货是否已出库）"))?;
            stockin_model::reverse_location_qty(
                ex,
                line.material_id,
                stock_in.warehouse_id,
                line.location_id,
                line.qty,
                input.now,
            )
            .await
            .map_err(|error| reversal_error(error, "红冲：库位账回冲"))?;
            write_reverse_txn(ex, &doc, line, &applied, input)
                .await
                .map_err(|error| utils::db_error(error, "红冲：写反向库存流水"))?;
            qty += line.qty;
            amount += line.amount;
        }
        // ④ 单头作废（状态机过闸 + CAS 落 status/cancelled_*；同事务）
        state_machine::transition(
            ex,
            state_machine::ENTITY_STOCK_IN,
            stock_in.id,
            &stock_in.in_no,
            &stockin_model::LEGAL_EDGES,
            STOCK_IN_POSTED,
            STOCK_IN_CANCELLED,
            input.operator,
            Some(input.reason),
            None,
        )
        .await?;
        let affected = ex
            .exec_sql(
                "UPDATE biz_stock_in \
                    SET status = ?, cancelled_at = ?, cancelled_by = NULLIF(?::text,'')::uuid, \
                        cancel_reason = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                    .to_string(),
                vec![
                    Value::Text(STOCK_IN_CANCELLED.to_string()),
                    Value::from(input.now),
                    Value::Text(input.operator.map(|id| id.to_string()).unwrap_or_default()),
                    Value::Text(input.reason.to_string()),
                    Value::from(input.now),
                    Value::Text(input.operator.map(|id| id.to_string()).unwrap_or_default()),
                    Value::from(&stock_in.id),
                    Value::Text(STOCK_IN_POSTED.to_string()),
                ],
            )
            .await
            .map_err(|error| utils::db_error(error, "红冲：作废完工入库单"))?;
        if affected == 0 {
            return Err(cost_period_dto::conflict(format!(
                "完工入库单 {} 的状态在本次红冲期间被改动（期望 {STOCK_IN_POSTED}），本次红冲已回滚",
                stock_in.in_no
            )));
        }
        done.push(ReversedStockIn {
            stock_in_id: stock_in.id,
            in_no: stock_in.in_no.clone(),
            qty: qty.round_dp(4),
            amount: amount.round_dp(4),
        });
    }
    Ok(done)
}

/// 一条明细的回冲计划（数量取明细、金额取**过账流水实际写入的**金额）。
///
/// 为什么不信现价：期间内又进过货，`avg_cost` 已经变了；用现价冲减会把成本污染到别的批次。
/// 与收货作废（`stockin_cancel::cancel_plan_of`）同一口径，差别只有 `biz_type`：
/// 完工入库的流水是 `PRODUCTION_IN`，红冲必须按它反查。
pub async fn reverse_plan_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    in_no: &str,
    warehouse_id: Uuid,
) -> DbResult<Vec<CancelLine>> {
    let rows = ex
        .query_sql(
            "SELECT i.line_no, i.material_id, i.location_id, i.qty, \
                    coalesce(t.qty, 0) AS txn_qty, coalesce(t.amount, 0) AS txn_amount, \
                    coalesce(t.cnt, 0) AS txn_cnt \
               FROM biz_stock_in_item i \
               LEFT JOIN (SELECT line_no, sum(qty) AS qty, sum(amount) AS amount, count(*) AS cnt \
                            FROM biz_inventory_txn \
                           WHERE doc_type = ? AND doc_id = ?::uuid \
                             AND txn_type = ? AND biz_type = ? \
                           GROUP BY line_no) t ON t.line_no = i.line_no \
              WHERE i.stock_in_id = ?::uuid \
              ORDER BY i.material_id, i.location_id NULLS FIRST, i.line_no"
                .to_string(),
            vec![
                Value::from(DOC_TYPE_STOCK_IN),
                Value::from(&stock_in_id),
                Value::from("IN"),
                Value::from(BIZ_TYPE_PRODUCTION_IN),
                Value::from(&stock_in_id),
            ],
        )
        .await?;

    let mut plan = Vec::with_capacity(rows.len());
    for row in rows {
        let line_no: i32 = row.get_typed("line_no")?;
        let material_id: Uuid = row.get_typed("material_id")?;
        let location_id: Option<Uuid> = row.get_typed("location_id")?;
        let qty: Decimal = row.get_typed("qty")?;
        let txn_qty: Decimal = row.get_typed("txn_qty")?;
        let txn_amount: Decimal = row.get_typed("txn_amount")?;
        let txn_cnt: i64 = row.get_typed("txn_cnt")?;
        if let Err(message) = check_txn_line(line_no, in_no, qty, txn_cnt, txn_qty) {
            return Err(DbError::InvalidValue(message));
        }
        let unit_cost = if txn_qty.is_zero() {
            Decimal::ZERO
        } else {
            (txn_amount / txn_qty).round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero)
        };
        plan.push(CancelLine {
            line_no,
            material_id,
            warehouse_id,
            location_id,
            qty,
            amount: txn_amount,
            unit_cost,
        });
    }
    Ok(plan)
}

/// 明细 ↔ 过账流水的对平判据（**纯函数**，单测覆盖两条失败分支）。
///
/// * 流水缺失 ⇒ 拒绝（没法知道该退多少金额，绝不按现价估算）；
/// * 数量不一致 ⇒ 拒绝（`POSTED` 的单据不应可改，出现就是数据被绕过接口改过）。
pub fn check_txn_line(
    line_no: i32,
    in_no: &str,
    qty: Decimal,
    txn_cnt: i64,
    txn_qty: Decimal,
) -> std::result::Result<(), String> {
    if txn_cnt == 0 {
        return Err(format!(
            "{MISSING_TXN_PREFIX}完工入库单 {in_no} 第 {line_no} 行在 biz_inventory_txn 里找不到\
             过账流水（`biz_type='{BIZ_TYPE_PRODUCTION_IN}'`）：无法确定要退回多少金额，\
             拒绝按现价估算。请先用数据修复脚本补流水，或走人工红字调整单"
        ));
    }
    let tolerance = Decimal::new(1, QTY_TOLERANCE_SCALE);
    if (txn_qty - qty).abs() > tolerance {
        return Err(format!(
            "完工入库单 {in_no} 第 {line_no} 行：明细数量 {qty} 与过账流水数量 {txn_qty} 不一致，\
             单据已被改过（POSTED 不应可改），拒绝红冲"
        ));
    }
    Ok(())
}

/// 写一条**反向流水**：`txn_type='OUT'` + `biz_type='PRODUCE_CANCEL'`。
///
/// `before_* / after_*` 由 `inventory_writer` 真填（审计要能从流水复原每一次回冲）。
async fn write_reverse_txn(
    ex: &impl SqlExecutor,
    doc: &CancelDoc<'_>,
    line: &CancelLine,
    applied: &AppliedInventory,
    input: &ReverseInput<'_>,
) -> DbResult<()> {
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id: line.material_id,
            warehouse_id: line.warehouse_id,
            txn_type: TXN_TYPE_OUT,
            biz_type: BIZ_TYPE_PRODUCE_CANCEL,
            doc_type: DOC_TYPE_STOCK_IN,
            doc_id: doc.id,
            doc_no: doc.in_no,
            line_no: line.line_no,
            qty: line.qty.round_dp(4),
            unit_cost: line.unit_cost,
            amount: line.amount.round_dp(4),
            before: applied.before,
            after: applied.after,
            operator: input.operator,
            period_code: Some(input.period_code),
            remark: Some("红冲完工入库"),
            now: input.now,
        },
    )
    .await
    .map(|_| ())
}

// ==================== ⑤ 计算单回退 ====================

/// 计算单回到「未结算」（`SETTLED → CALC` + `stock_in_id = NULL`）。
///
/// 设计 §4.8：红冲后该对象回到未结算状态 → 可以改数重算、重建新批次。
/// `ck_biz_cost_item_settled` 只禁止「`SETTLED` 而没有入库单」，所以这两列必须**同时**清。
///
/// 不要求影响行数 > 0：本期 0 完工的批次本来就允许结算（§4.10「结算时发现某对象 0 完工」），
/// 那种批次没有一行是 `SETTLED`，红冲照样合法。
async fn reset_items(ex: &impl SqlExecutor, input: &ReverseInput<'_>) -> std::result::Result<u64, ApiError> {
    ex.exec_sql(
        "UPDATE biz_cost_period_item \
            SET status = ?, stock_in_id = NULL, updated_at = ? \
          WHERE period_id = ?::uuid AND status = ?"
            .to_string(),
        vec![
            Value::Text(ITEM_CALC.to_string()),
            Value::from(input.now),
            Value::from(&input.period_id),
            Value::Text(ITEM_SETTLED.to_string()),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "红冲：计算单回退为未结算"))
}

// ==================== ⑥ 批次状态 → REVERSED ====================

/// 状态机 `SETTLED → REVERSED`（必填原因）+ CAS 回写 `reversed_at/by/reason`。
///
/// **保留** `settled_at` 与三个 `*_voucher_id`：库层 `ck_biz_cost_period_settle` 要求
/// `REVERSED` 仍有这两样，审计也要能顺着指针找到「红了哪几张蓝字凭证」。
async fn write_back(ex: &impl SqlExecutor, input: &ReverseInput<'_>) -> std::result::Result<(), ApiError> {
    state_machine::transition(
        ex,
        state_machine::ENTITY_COST_PERIOD,
        input.period_id,
        input.batch_no,
        &model::LEGAL_EDGES,
        STATUS_SETTLED,
        STATUS_REVERSED,
        input.operator,
        Some(input.reason),
        None,
    )
    .await?;

    let affected = ex
        .exec_sql(
            "UPDATE biz_cost_period \
                SET status = ?, reversed_at = ?, reversed_by = NULLIF(?::text,'')::uuid, \
                    reverse_reason = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(STATUS_REVERSED.to_string()),
                Value::from(input.now),
                Value::Text(input.operator.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(input.reason.to_string()),
                Value::from(input.now),
                Value::Text(input.operator.map(|id| id.to_string()).unwrap_or_default()),
                Value::from(&input.period_id),
                Value::Text(STATUS_SETTLED.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "红冲：回写批次状态"))?;
    if affected == 0 {
        return Err(cost_period_dto::conflict(format!(
            "成本核算批次 {} 的状态在本次红冲期间被改动（期望 {STATUS_SETTLED}），本次红冲已回滚",
            input.batch_no
        )));
    }
    Ok(())
}

// ==================== 错误出口 ====================

/// 库存退不回来 / 流水缺失 ⇒ **409 + `66078`**（业务边界，不是 500）。
///
/// 设计 §4.8：「库存回冲失败（如货已发出、库存不足）→ 整个事务回滚，批次仍是 `SETTLED`，
/// 报错提示『该批货已出库，不能红冲，请走销售退货/盘亏流程』。这是明确的业务边界，不是死单。」
fn reversal_error(error: DbError, ctx: &str) -> ApiError {
    let detail = match &error {
        DbError::InvalidValue(message) => message
            .strip_prefix(SHORTFALL_PREFIX)
            .map(|text| {
                format!(
                    "该批货已出库（{text}）：完工入库的库存已经不够回冲，红冲会做出负库存。\
                     请先走销售退货 / 盘点盘盈把货补回，再红冲；或者不红冲，直接在本期新批次里做调整"
                )
            })
            .or_else(|| {
                message
                    .strip_prefix(MISSING_TXN_PREFIX)
                    .map(|text| format!("过账流水缺失，无法确定回冲金额：{text}"))
            }),
        _ => None,
    };
    match detail {
        Some(detail) => ApiError::new(409, error_code::ERP_COST_REVERSE_BLOCKED, detail),
        None => utils::db_error(error, ctx),
    }
}

// ==================== 单测（纯逻辑 / 常量对齐；真库用例见 cost_reverse_run_db_tests） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn dec(text: &str) -> Decimal {
        text.parse().expect("金额字面量")
    }

    /// 迁移原文 —— 常量必须与库层定义逐字对齐（改迁移漏改常量 ⇒ 这里先红）。
    const V0373_UP: &str = include_str!("../../../migrations/business/V0373__cost_period_tables.up.sql");

    #[test]
    fn produce_cancel_is_a_real_vocabulary_value() {
        assert_eq!(BIZ_TYPE_PRODUCE_CANCEL, "PRODUCE_CANCEL");
        assert_eq!(BIZ_TYPE_PRODUCE_CANCEL, TxnBizType::ProduceCancel.as_str());
        // 词表里能反查回来（库里有历史流水时，巡检脚本按词表解析 biz_type）
        assert_eq!(TxnBizType::parse("PRODUCE_CANCEL"), Some(TxnBizType::ProduceCancel));
        // 这条流水造成的单据是入库单 —— E4 净额判据按单据轴解释
        assert_eq!(
            TxnBizType::ProduceCancel.doc_kind(),
            crate::business::shared::doc_kind::DocKind::StockIn
        );
        // 与结算写入的 PRODUCTION_IN 是**一对**（一进一出，净额归零）
        assert_eq!(BIZ_TYPE_PRODUCTION_IN, "PRODUCTION_IN");
        assert_eq!(TXN_TYPE_OUT, "OUT");
    }

    #[test]
    fn reversed_status_and_reason_edge_are_registered() {
        assert_eq!(STATUS_REVERSED, "REVERSED");
        // 库层 ck：REVERSED 必须有 reversed_at + reverse_reason
        assert!(
            V0373_UP.contains("status <> 'REVERSED'"),
            "ck_biz_cost_period_rev 必须要求红冲时间与原因"
        );
        // 库层 ck：REVERSED 仍要 settled_at + finish_voucher_id（所以本模块不置空它们）
        assert!(
            V0373_UP.contains("status NOT IN ('SETTLED','CLOSED','REVERSED')"),
            "ck_biz_cost_period_settle 对 REVERSED 同样生效"
        );
        // 唯一键排除 REVERSED：红冲后同期间可重建批次（B3-7 卡面「可重算」的库层前提）
        assert!(
            V0373_UP.contains("status NOT IN ('REVERSED','CANCELLED')"),
            "uk_biz_cost_period 必须排除 REVERSED，否则红冲后永远不能重算"
        );
        // 状态机边表：SETTLED → REVERSED 在，且**红冲边必须要求原因**
        assert!(model::LEGAL_EDGES.contains(&(STATUS_SETTLED, STATUS_REVERSED)));
        assert!(
            state_machine::COST_PERIOD_REASON_EDGES.contains(&(STATUS_SETTLED, STATUS_REVERSED)),
            "红冲影响已入账数据，必须留原因"
        );
        // 设计 §4.8 说 SETTLED / CLOSED 都能红冲，但边表只有一条；本卡只做 SETTLED（见模块头偏差）
        assert!(
            !model::LEGAL_EDGES.contains(&("CLOSED", STATUS_REVERSED)),
            "CLOSED → REVERSED 不在边表：关期批次的红冲要等关期卡补边"
        );
    }

    #[test]
    fn item_statuses_round_trip_to_calc() {
        assert_eq!(ITEM_CALC, "CALC");
        assert!(
            V0373_UP.contains("status IN ('CALC','SETTLED')"),
            "计算单只有两个状态，红冲回退只能回 CALC"
        );
        // 回退时必须**同时**清 stock_in_id：ck 要求 SETTLED 必有入库单，CALC 可以没有
        assert!(V0373_UP.contains("status <> 'SETTLED' OR stock_in_id IS NOT NULL"));
        assert_eq!(STOCK_IN_POSTED, "POSTED");
        assert_eq!(STOCK_IN_CANCELLED, "CANCELLED");
    }

    #[test]
    fn stock_in_cancel_edge_exists_with_reason() {
        // 红冲要作废生产入库单：POSTED → CANCELLED 必须在入库单边表里、且要原因
        assert!(stockin_model::LEGAL_EDGES.contains(&(STOCK_IN_POSTED, STOCK_IN_CANCELLED)));
        assert!(
            state_machine::STOCK_IN_REASON_EDGES.contains(&(STOCK_IN_POSTED, STOCK_IN_CANCELLED)),
            "作废已过账的入库单必须留原因（红字凭证摘要用它）"
        );
        // 作废只改状态与三列 cancel_*，不动数量金额 —— CHECK 里没有「CANCELLED 必须数量为 0」这种约束
        assert!(V0373_UP.contains("REVERSED"));
    }

    #[test]
    fn check_txn_line_rejects_missing_and_mismatched_flow() {
        // 正常：数量一致
        assert!(check_txn_line(1, "IN-1", dec("800"), 1, dec("800")).is_ok());
        // 容差以内（4 位小数的最后一分）
        assert!(check_txn_line(1, "IN-1", dec("800.0000"), 1, dec("800.0001")).is_ok());

        // 流水缺失：点名单号与行号，并要求补流水而不是按现价估
        let missing = check_txn_line(3, "IN-7", dec("800"), 0, Decimal::ZERO).expect_err("流水缺失");
        assert!(missing.starts_with(MISSING_TXN_PREFIX), "{missing}");
        assert!(missing.contains("IN-7") && missing.contains("第 3 行"), "{missing}");
        assert!(missing.contains("拒绝按现价估算"), "{missing}");

        // 数量不一致：单据被绕过接口改过
        let mismatch = check_txn_line(1, "IN-1", dec("800"), 1, dec("700")).expect_err("数量不符");
        assert!(mismatch.contains("800") && mismatch.contains("700"), "{mismatch}");
        assert!(mismatch.contains("拒绝红冲"), "{mismatch}");
    }

    #[test]
    fn run_totals_and_summary_line() {
        let run = ReverseRun {
            period_id: Uuid::now_v7(),
            batch_no: "CS-202611-0001".into(),
            vouchers: vec![
                ReversedVoucher {
                    original_id: Uuid::now_v7(),
                    original_no: "PZ-202611-0001".into(),
                    red_id: Uuid::now_v7(),
                    red_no: "PZR-202611-0001".into(),
                },
                ReversedVoucher {
                    original_id: Uuid::now_v7(),
                    original_no: "PZ-202611-0002".into(),
                    red_id: Uuid::now_v7(),
                    red_no: "PZR-202611-0002".into(),
                },
            ],
            stock_ins: vec![
                ReversedStockIn {
                    stock_in_id: Uuid::now_v7(),
                    in_no: "IN-1".into(),
                    qty: dec("800"),
                    amount: dec("22311.1104"),
                },
                ReversedStockIn {
                    stock_in_id: Uuid::now_v7(),
                    in_no: "IN-2".into(),
                    qty: dec("150"),
                    amount: dec("300"),
                },
            ],
            reset_items: 2,
        };
        assert_eq!(run.totals(), (dec("950"), dec("22611.1104")));
        let line = run.summary_line();
        assert!(line.contains("红字凭证 2 张"), "{line}");
        assert!(line.contains("作废入库单 2 张"), "{line}");
        assert!(line.contains("回冲 950"), "{line}");
        // 空库存（0 完工批次）也不 panic
        let empty = ReverseRun {
            period_id: Uuid::now_v7(),
            batch_no: "CS-1".into(),
            vouchers: Vec::new(),
            stock_ins: Vec::new(),
            reset_items: 0,
        };
        assert_eq!(empty.totals(), (Decimal::ZERO, Decimal::ZERO));
        assert!(empty.summary_line().contains("红字凭证 0 张"));
    }

    #[test]
    fn reversal_error_maps_shortfall_to_409() {
        let error = DbError::InvalidValue(format!("{}库存 3 < 本单 800", SHORTFALL_PREFIX));
        let mapped = reversal_error(error, "红冲：成本账回冲");
        assert_eq!(mapped.status, 409);
        assert_eq!(mapped.code, error_code::ERP_COST_REVERSE_BLOCKED);
        // 设计 §4.8 的原文口径：明确告诉用户下一步走哪
        assert!(mapped.msg.contains("销售退货"), "{}", mapped.msg);
        assert!(mapped.msg.contains("已出库"), "{}", mapped.msg);

        let error = DbError::InvalidValue(format!("{MISSING_TXN_PREFIX}缺流水"));
        let mapped = reversal_error(error, "红冲：读流水");
        assert_eq!(mapped.status, 409);
        assert!(mapped.msg.contains("流水缺失"), "{}", mapped.msg);

        // 其它错误维持原出口（500 内部错误；`utils::db_error` 对内部错误不回显明细，这是有意的）
        let mapped = reversal_error(DbError::InvalidValue("别的错".into()), "红冲：别的");
        assert_eq!(mapped.status, 500);
        assert_eq!(mapped.code, error_code::INTERNAL_ERROR);
    }

    #[test]
    fn reverse_input_keeps_voucher_date_in_the_reversed_period() {
        // 红字凭证日期 = 被红冲期间的末日：与蓝字同期间才能让「本期净发生」归零
        let day = NaiveDate::from_ymd_opt(2026, 11, 30).expect("日期");
        let input = ReverseInput {
            period_id: Uuid::now_v7(),
            period_code: "2026-11",
            batch_no: "CS-202611-0001",
            reason: "期间产量录错，整期重算",
            voucher_date: day,
            operator: None,
            now: Utc::now(),
        };
        assert_eq!(input.voucher_date.format("%Y-%m").to_string(), input.period_code);
        assert!(reverse_reason_min_ok(input.reason), "原因 ≥10 字才放行");
    }

    /// 原因长度闸的口径（与 handler/DTO 共用同一个常量）。
    fn reverse_reason_min_ok(reason: &str) -> bool {
        reason.chars().count() >= cost_period_dto::REVERSE_REASON_MIN
    }
}

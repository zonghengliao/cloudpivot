//! calc **编排**：把六段计算串成一次 HTTP 调用（ARCH-9 §5.2 #27 · COST-B2-15）。
//!
//! ```text
//! ① 定批次：期间有活跃批次？
//!      无       → 建 DRAFT（`CS-yyyyMM-NNNN`）→ 往下算
//!      DRAFT    → 原地重算（幂等）
//!      其它     → 409「已提交 / 已结算，请先撤回或红冲」
//! ② 清计算单：`purge_calc_rows(period_id)`（只删本批次的 `CALC` 行）
//! ③ 料 → 工 → 费 → 池对账 → 分摊 → 计算单（顺序不可交换）
//! ④ 一次事务：②③ 与「建批次」同生共死；任一段 409 全回滚，期间保持原状态
//! ⑤ 出参 = `GET /cost-period/{id}` 的批次详情（同一份 `detail_of`）+ `calc` 分段摘要
//! ```
//!
//! # 为什么单独一个模块
//!
//! 六段各自有 `*_in_tx`（`cost_calc_material` / `cost_calc_labor` / `cost_calc_overhead` /
//! `cost_calc_reconcile` / `cost_calc_alloc` / `cost_calc_item`），每段都写着「没有 HTTP 入口，
//! 由编排方在同一次『清当期 + 重灌』里调用」。本模块就是那个编排方：**不重写任何算账逻辑**，
//! 只负责定批次、排顺序、收口事务、把失败翻成人话。
//!
//! # 顺序契约（各段模块头写死的）
//!
//! 分摊 → 计算单不可交换：制造费用是通过 `ALLOC` 归集行落到对象上的，分摊没跑就等于
//! 「制造费用没进对象」；池对账又必须早于分摊（账上旁路记的费用先摊下去，错的不是成本
//! 而是全部存货）。所以顺序固定为 料 → 工 → 费 → 对账 → 分摊 → 计算单。
//!
//! # 「重算不重复累加」怎么保证
//!
//! 每一段都自带收敛口径，编排方**不**做「`DELETE … WHERE period_code = ?`」这种一刀切
//! （设计 §4.3 的写法会把同期间**已结算批次**的归集行一起删掉，那是审计凭据）：
//!
//! | 段 | 收敛方式 |
//! |---|---|
//! | 料 / 工 / 费归集 | 以来源行 id 为幂等键取差：`ON CONFLICT DO NOTHING` + 软删过期行 |
//! | 池对账 | 只读（不写账） |
//! | 分摊 | `purge_period`：只删自家 `ALLOC` 归集行 + `biz_cost_alloc_result` |
//! | 计算单 | `purge_calc_rows`：只删本批次 `status = 'CALC'` 的行 |
//!
//! 编排方额外再显式清一次计算单（卡面要点 ②）：段 6 内部还会清一遍，这里是**编排契约**，
//! 让「重算前必 purge」这件事在编排层可见，而不是依赖下游函数不被人改。
//!
//! # 失败语义（约定 11：不许留死单）
//!
//! 全部在一个事务里，任一段失败 ⇒ 整笔回滚 ⇒ 期间停在**调用前**的状态：
//!
//! - 期间本来没有批次 ⇒ 批次也不存在（不会留一张「建了但没算」的空 DRAFT 占着唯一键）
//! - 期间本来有 DRAFT ⇒ 旧计算单原样保留（不会出现「旧的删了、新的写了一半」）
//! - 报错带段名：`… （成本计算第 4 段「费用池对账」失败；整笔已回滚）`
//!
//! 每一段的 409 本身都点名对象 / 单据 / 凭证号（料 `66064`、工 `66064`、对账 `66065`、
//! 分摊 `66062` / `66063`、计算单 `66067` ~ `66069`），编排层只负责标出**是哪一段**。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::cost_calc_alloc;
use crate::business::erp::cost_calc_item;
use crate::business::erp::cost_calc_labor;
use crate::business::erp::cost_calc_material;
use crate::business::erp::cost_calc_overhead;
use crate::business::erp::cost_calc_reconcile;
use crate::business::erp::cost_labor_dto;
use crate::business::erp::cost_period_dto::{self as period_dto, CostPeriodDetailRes, STATUS_DRAFT};
use crate::business::erp::cost_period_handler;
use crate::business::erp::cost_period_item_dto;
use crate::business::erp::cost_period_item_model as item_model;
use crate::business::erp::cost_period_model as model;
use crate::business::erp::cost_reconcile_dto;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 出参 ====================

/// 一次 `calc` 的分段摘要（随批次详情一起返回，给前端 toast / 联调排查用）。
///
/// **不参与算账**：所有金额都是各段回查后的实测值，明细以 `detail.items` 为准。
#[derive(Debug, Clone, Serialize)]
pub struct CalcRunSummary {
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 本次算的是哪张批次。
    pub batch_id: String,
    /// 批次号 `CS-yyyyMM-NNNN`。
    pub batch_no: String,
    /// `true` = 本次**新建**了 `DRAFT` 批次；`false` = 在已有 `DRAFT` 上重算。
    pub batch_created: bool,
    /// 料归集段。
    pub material: MaterialStage,
    /// 工归集段。
    pub labor: LaborStage,
    /// 费归集段。
    pub overhead: OverheadStage,
    /// 费用池 ↔ `4101.*` 对账段。
    pub reconcile: ReconcileStage,
    /// 分摊段。
    pub alloc: AllocStage,
    /// 计算单段。
    pub items: ItemStage,
}

/// 料归集段摘要。
#[derive(Debug, Clone, Serialize)]
pub struct MaterialStage {
    /// 扫到的领料明细行数。
    pub scanned: usize,
    /// 本次新写行数（重算时通常是 0）。
    pub inserted: usize,
    /// 本次软删的过期行数。
    pub purged: usize,
    /// 写后回查：当期料归集金额。
    pub amount: String,
}

/// 工归集段摘要。
#[derive(Debug, Clone, Serialize)]
pub struct LaborStage {
    /// 扫到的已确认工时行数。
    pub scanned: usize,
    /// 本次新写行数。
    pub inserted: usize,
    /// 本次软删的过期行数。
    pub purged: usize,
    /// 写后回查：当期工归集金额。
    pub amount: String,
    /// 缺费率 / 金额舍入为 0 而**没进成本**的行数（缺口清单的工侧口径）。
    pub uncollected: usize,
}

/// 费归集段摘要。
#[derive(Debug, Clone, Serialize)]
pub struct OverheadStage {
    /// 扫到的三路来源行数（折旧 / 报销 / 手工台账）。
    pub scanned: usize,
    /// 本次新写行数。
    pub inserted: usize,
    /// 本次软删的过期行数。
    pub purged: usize,
    /// 写后回查：当期费归集金额。
    pub amount: String,
    /// 归集后车间费用池金额（待分摊）。
    pub pool: String,
    /// 金额为 0 而没进成本的行数。
    pub uncollected: usize,
}

/// 对账段摘要。
#[derive(Debug, Clone, Serialize)]
pub struct ReconcileStage {
    /// 池金额（含待结算过账）。
    pub pool: String,
    /// 本阶段应已入账的池金额（`Calc` 阶段不含手工台账的待结算过账）。
    pub expected: String,
    /// 待结算过账金额（结算事务才要求账面背书）。
    pub pending: String,
    /// 账面 `4101.*` 借方发生额。
    pub ledger: String,
    /// 逐组差额合计（平的时候是 0）。
    pub diff: String,
}

/// 分摊段摘要。
#[derive(Debug, Clone, Serialize)]
pub struct AllocStage {
    /// 真摊出去的规则条数。
    pub rules: usize,
    /// 落库的分摊结果行数。
    pub result_rows: i64,
    /// 分摊额合计（== 池合计）。
    pub amount: String,
    /// 池为 0 被跳过的规则数。
    pub zero_pool: usize,
    /// 有池但没有规则覆盖的组数（配置缺失，不阻断 calc；结算前要解释）。
    pub uncovered: usize,
}

/// 计算单段摘要。
#[derive(Debug, Clone, Serialize)]
pub struct ItemStage {
    /// 写出的计算单行数（= 算出成本的成本对象数）。
    pub rows: usize,
    /// 「无投入无产量」被跳过的对象数。
    pub skipped: usize,
    /// 清掉的旧计算单行数（编排层第 ② 步那一次；段 6 内部还会清一次，届时为 0）。
    pub purged: i64,
    /// Σ 本期材料投入。
    pub total_material: String,
    /// Σ 本期人工投入。
    pub total_labor: String,
    /// Σ 本期制造费用投入。
    pub total_overhead: String,
    /// Σ 本期投入（= 料 + 工 + 费）。
    pub total_cost: String,
    /// Σ 完工转出。
    pub total_finished: String,
    /// Σ 期末在制。
    pub total_wip: String,
}

/// `POST /cost-period/calc` 的响应：批次详情（与 `GET /cost-period/{id}` **同结构**）+ 分段摘要。
///
/// `#[serde(flatten)]` 保证前端按 `CostPeriodDetail` 解析时不缺任何一个字段；
/// `calc` 是**增量**字段，老调用方忽略即可。
#[derive(Debug, Clone, Serialize)]
pub struct CostCalcRes {
    /// 批次详情（单头 + items + items_summary + next_statuses + legal_edges）。
    #[serde(flatten)]
    pub detail: CostPeriodDetailRes,
    /// 本次六段的分段摘要。
    pub calc: CalcRunSummary,
}

// ==================== 错误出口 ====================

/// 409：批次状态不允许重算（`66073`，与 B3-1 的「非 DRAFT 写操作」同一个码）。
///
/// 文案必须告诉用户**下一步点哪**：审批中 → 撤回；已结算 → 红冲；已关期 → 反结账。
pub fn period_locked(period_code: &str, batch_no: &str, status: &str) -> ApiError {
    let next = match status {
        period_dto::STATUS_SUBMITTED => "请先「撤回」审批，回到草稿后再算",
        period_dto::STATUS_APPROVED => "已审批通过、数据已冻结：只能结算，或让审批人驳回后重算",
        period_dto::STATUS_SETTLED => "已结算入账：请先「红冲结算」，之后同期间可重建批次重算",
        period_dto::STATUS_CLOSED => "期间已关账：请先反结账（财务总监），再红冲重算",
        _ => "请先把它撤回 / 红冲 / 作废后再算",
    };
    ApiError::new(
        409,
        error_code::ERP_COST_PERIOD_STATUS_GUARD,
        format!("期间 {period_code} 的批次 {batch_no} 当前是「{status}」，不能重算（结果已冻结、可能已入账）：{next}"),
    )
}

/// 给某一段的失败加上「第几段 + 段名」，**保留**原状态码 / 业务码 / 结构化载荷。
///
/// 用户看到的报文形如：
/// `期间 2026-11 池 3000 ≠ 账面 2900 …（成本计算第 4 段「费用池对账」失败；整笔已回滚，批次状态未变）`
fn stage_failed(error: ApiError, ordinal: u8, name: &str) -> ApiError {
    error.appended(format!(
        "（成本计算第 {ordinal} 段「{name}」失败；整笔已回滚，期间保持调用前的状态，修好后可直接重算）"
    ))
}

// ==================== 编排 ====================

/// 一次六段计算（自开事务并提交）。
///
/// # 异常情况
///
/// - 期间编码非法 / 缺失 ⇒ 400
/// - 期间有非 `DRAFT` 活跃批次 ⇒ 409 + `66073`（文案点名下一步）
/// - 任一段的业务拒绝 ⇒ 原码返回（`66062` ~ `66069`、`66064`、`66065`）+ 段名后缀，整笔回滚
/// - 并发下两张 DRAFT 抢同一个期间 ⇒ 409 + `66072`（唯一键的人话版）
/// - 查库 / 写库失败 ⇒ 500
pub async fn calc_period(
    period_code_raw: Option<&str>,
    operator: Option<Uuid>,
) -> std::result::Result<CostCalcRes, ApiError> {
    // 期间形态先判（不占连接、不打库）：`cost_labor_dto` 与料 / 工 / 费段同一文案
    let period_code = cost_labor_dto::normalize_period(period_code_raw)?;
    // 期间区间取库里的真实起止（月结把期间拉长 / 缩短时跟得上）；只读、在事务外
    let (start, end) = cost_calc_material::period_bounds(&period_code).await?;

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启成本计算事务"))?;

    let outcome = run_in_tx(&tx, &period_code, start, end, operator).await;
    let (batch_id, summary) = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            // 事务随 tx drop 回滚；显式 rollback 只是把原因留在日志里
            if let Err(rollback) = tx.rollback().await {
                tracing::error!(error = %rollback, "成本计算回滚失败");
            }
            return Err(error);
        }
    };

    tx.commit()
        .await
        .map_err(|error| utils::db_error(error, "提交成本计算事务"))?;
    tracing::info!(
        target: "cost",
        period_code = %summary.period_code,
        batch_no = %summary.batch_no,
        created = summary.batch_created,
        rows = summary.items.rows,
        finished = %summary.items.total_finished,
        wip = %summary.items.total_wip,
        "成本计算完成（料→工→费→对账→分摊→计算单）"
    );

    // 出参用**同一个** `detail_of`：与 `GET /cost-period/{id}` 逐字段一致（含 items 的 join）
    let detail = cost_period_handler::detail_of(batch_id).await?;
    Ok(CostCalcRes { detail, calc: summary })
}

/// 事务内的六段编排（不提交、不回滚；返回批次 id 与分段摘要）。
async fn run_in_tx(
    tx: &impl SqlExecutor,
    period_code: &str,
    start: nik::prelude::chrono::NaiveDate,
    end: nik::prelude::chrono::NaiveDate,
    operator: Option<Uuid>,
) -> std::result::Result<(Uuid, CalcRunSummary), ApiError> {
    // ---------- ① 定批次 ----------
    let batch = ensure_batch(tx, period_code, operator).await?;

    // ---------- ② 清本批次旧计算单（卡面要点 ②：重算前必 purge） ----------
    let purged_items = item_model::purge_calc_rows(tx, batch.id)
        .await
        .map_err(|error| utils::db_error(error, "清理本次要重算的成本计算单"))?;
    if purged_items > 0 {
        tracing::info!(target: "cost", period_code, batch_no = %batch.batch_no, purged_items, "重算：已清掉本批次旧计算单");
    }

    // ---------- ③ 第 1 段：料归集 ----------
    let material = cost_calc_material::collect_material_in_tx(tx, period_code, start, end, operator)
        .await
        .map_err(|error| stage_failed(error, 1, "物料归集"))?;

    // ---------- ③ 第 2 段：工归集 ----------
    let labor = cost_calc_labor::collect_labor_in_tx(tx, period_code, operator)
        .await
        .map_err(|error| stage_failed(error, 2, "人工归集"))?;

    // ---------- ③ 第 3 段：费归集（池行 + 直接行） ----------
    let overhead = cost_calc_overhead::collect_overhead_in_tx(tx, period_code, operator)
        .await
        .map_err(|error| stage_failed(error, 3, "制造费用归集"))?;

    // ---------- ③ 第 4 段：池 ↔ 4101 借方对账（不平不许继续分摊） ----------
    let reconcile = cost_calc_reconcile::reconcile_in_tx(tx, period_code, cost_reconcile_dto::Stage::Calc)
        .await
        .map_err(|error| stage_failed(error, 4, "费用池对账"))?;
    cost_reconcile_dto::ensure_balanced(&reconcile).map_err(|error| stage_failed(error, 4, "费用池对账"))?;

    // ---------- ③ 第 5 段：分摊（必须早于计算单） ----------
    let alloc = cost_calc_alloc::allocate_overhead_in_tx(tx, period_code, operator)
        .await
        .map_err(|error| stage_failed(error, 5, "制造费用分摊"))?;

    // ---------- ③ 第 6 段：约当产量 + 成本计算单 ----------
    let items = cost_calc_item::calc_items_in_tx(tx, period_code, operator)
        .await
        .map_err(|error| stage_failed(error, 6, "成本计算单"))?;

    // ---------- ④ 分段摘要 ----------
    let summary = CalcRunSummary {
        period_code: period_code.to_string(),
        batch_id: batch.id.to_string(),
        batch_no: batch.batch_no.clone(),
        batch_created: batch.created,
        material: MaterialStage {
            scanned: material.scanned,
            inserted: material.inserted,
            purged: material.purged,
            amount: money(material.collected_amount),
        },
        labor: LaborStage {
            scanned: labor.scanned,
            inserted: labor.inserted,
            purged: labor.purged,
            amount: money(labor.collected_amount),
            uncollected: labor.uncollected.len(),
        },
        overhead: OverheadStage {
            scanned: overhead.scanned,
            inserted: overhead.inserted,
            purged: overhead.purged,
            amount: money(overhead.collected_amount),
            pool: money(overhead.pool_amount()),
            uncollected: overhead.uncollected.len(),
        },
        reconcile: ReconcileStage {
            pool: money(reconcile.pool_amount),
            expected: money(reconcile.expected_amount),
            pending: money(reconcile.pending_amount),
            ledger: money(reconcile.ledger_amount),
            diff: money(reconcile.diff_amount),
        },
        alloc: AllocStage {
            rules: alloc.rules.len(),
            result_rows: alloc.result_rows,
            amount: money(alloc.total_alloc),
            zero_pool: alloc.zero_pool.len(),
            uncovered: alloc.uncovered.len(),
        },
        items: ItemStage {
            rows: items.items.len(),
            skipped: items.skipped.len(),
            // 编排层在第 ② 步已经清过一遍（`purged_items`），段 6 内部的 purge 那时是 0；
            // 回报「实际清掉了多少旧行」要用编排层这个数，否则重算永远显示 0。
            purged: i64::try_from(purged_items).unwrap_or(i64::MAX),
            total_material: money(items.total_material),
            total_labor: money(items.total_labor),
            total_overhead: money(items.total_overhead),
            total_cost: money(items.total_cost),
            total_finished: money(items.total_finished),
            total_wip: money(items.total_wip),
        },
    };
    Ok((batch.id, summary))
}

/// 本次要算的批次。
struct BatchTarget {
    id: Uuid,
    batch_no: String,
    /// `true` = 本次新建（首次调用）。
    created: bool,
}

/// 活跃批次里**明确禁止重算**的四个状态。
///
/// 「活跃」的唯一定义来自 `uk_biz_cost_period`（V0373）：`deleted_at IS NULL AND
/// status NOT IN ('REVERSED','CANCELLED')` —— 即 `DRAFT` / `SUBMITTED` / `APPROVED` /
/// `SETTLED` / `CLOSED` 都算活跃。活跃里**只有 `DRAFT` 可以原地重算**，剩下这四个必须 409；
/// `REVERSED` / `CANCELLED` 不活跃 ⇒ 走「建新批次」。
///
/// 这份清单与 [`period_dto::STATUSES`] 的偏差由单测
/// `locked_statuses_match_the_live_set` 兜住：将来加状态忘了决定「能不能重算」会直接红。
const LOCKED_STATUSES: [&str; 4] = [
    period_dto::STATUS_SUBMITTED,
    period_dto::STATUS_APPROVED,
    period_dto::STATUS_SETTLED,
    period_dto::STATUS_CLOSED,
];

/// 定批次（无则建 `DRAFT`）；非 `DRAFT` 的活跃批次直接 409。
///
/// # 为什么「红冲后重算」是**建新批次**而不是原地重算 `REVERSED` 批次
///
/// `uk_biz_cost_period` 的部分唯一索引把 `REVERSED` / `CANCELLED` 排除在外，设计 §4.1 /
/// §4.8 的口径就是「红冲后同期间可重建批次再算」。`REVERSED` 那一批的红字凭证与红冲原因
/// 是审计凭据，原地改它的账等于把红冲记录也一起改了。所以这里走「无活跃批次 ⇒ 建 DRAFT」，
/// 用户点一次重算就得到一张干净的新批次，金额与红冲前一致（同一批来源行、同一套口径）。
///
/// # 状态判定为什么**不能**写成「查 DRAFT，查不到就新建」
///
/// 那样在 `SUBMITTED`（审批中）的期间上会再塞一张 `DRAFT`：轻则被唯一键挡住、用户看到一句
/// `duplicate key` 不知道是状态问题，重则（唯一索引被人改宽）同期间出现两张活跃批次、
/// 两张都能往下走结算。所以这里对**活跃批次的每一个状态都显式表态**：
/// `DRAFT` → 原地重算（幂等）；[`LOCKED_STATUSES`] → 409 + 点名下一步；
/// 其余（`REVERSED` / `CANCELLED`，正常已被 `active_by_period` 过滤掉）→ 也按锁定处理
/// （fail closed：宁可报错让用户看一眼，也不在他以为是草稿的时候动账）。
async fn ensure_batch(
    tx: &impl SqlExecutor,
    period_code: &str,
    operator: Option<Uuid>,
) -> std::result::Result<BatchTarget, ApiError> {
    if let Some(batch) = model::active_by_period(tx, period_code)
        .await
        .map_err(|error| utils::db_error(error, "查询期间的活跃批次"))?
    {
        if batch.status == STATUS_DRAFT {
            return Ok(BatchTarget {
                id: batch.id,
                batch_no: batch.batch_no,
                created: false,
            });
        }
        if LOCKED_STATUSES.contains(&batch.status.as_str()) {
            return Err(period_locked(period_code, &batch.batch_no, &batch.status));
        }
        // 走到这里说明库里有一张「活跃但不在值域内」的批次（人工改库 / 将来新增状态没登记）。
        // 同样拒绝：未知状态**绝不能**被当成草稿重算。
        tracing::warn!(
            target: "cost",
            period_code,
            batch_no = %batch.batch_no,
            status = %batch.status,
            "活跃批次状态不在值域内，按锁定处理（fail closed）"
        );
        return Err(period_locked(period_code, &batch.batch_no, &batch.status));
    }

    // 没有活跃批次（从未建过 / 上次已红冲或作废）⇒ 建 DRAFT
    let batch_no = model::next_batch_no(tx, period_code)
        .await
        .map_err(|error| utils::db_error(error, "生成成本核算批次号"))?;
    let id = Uuid::now_v7();
    model::insert_draft(
        tx,
        id,
        period_code,
        &batch_no,
        period_dto::LABOR_SOURCE_MANUAL,
        None,
        operator,
    )
    .await
    .map_err(|error| match error {
        // 并发两个 calc：都判定「没有活跃批次」→ 一个先插成功，另一个撞唯一键。
        // 翻译成人话（用户再点一次就是重算，不会重复建单）
        DbError::UniqueViolation { .. } => period_dto::duplicate_period(period_code, "（并发创建）"),
        other => utils::db_error(other, "新建成本核算批次"),
    })?;
    // 建单留痕（from_status = NULL），与手工建单同一条轨迹口径
    state_machine::log_created(
        tx,
        state_machine::ENTITY_COST_PERIOD,
        id,
        &batch_no,
        STATUS_DRAFT,
        operator,
        Some("成本计算（#27）自动创建 DRAFT 批次"),
    )
    .await?;
    Ok(BatchTarget {
        id,
        batch_no,
        created: true,
    })
}

/// 金额出参一律字符串（前端 JS number 会丢精度）。
fn money(value: Decimal) -> String {
    cost_period_item_dto::fmt(value, 4)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 期间缺失 / 形态非法 ⇒ 400，且**在开事务之前**（不占连接、不打库）。
    #[tokio::test]
    async fn bad_period_is_rejected_before_any_query() {
        for bad in [None, Some(""), Some("2026-13"), Some("202611"), Some("2026-1")] {
            let error = calc_period(bad, None).await.expect_err("非法期间必须 400");
            assert_eq!(
                error.status, 400,
                "「{bad:?}」应 400，实际 {} {}",
                error.status, error.msg
            );
        }
    }

    /// 非 `DRAFT` 的活跃批次 ⇒ 409 + `66073`，文案必须点名「下一步点哪」。
    #[test]
    fn locked_statuses_tell_the_user_the_next_step() {
        for (status, hint) in [
            (period_dto::STATUS_SUBMITTED, "撤回"),
            (period_dto::STATUS_APPROVED, "审批"),
            (period_dto::STATUS_SETTLED, "红冲"),
            (period_dto::STATUS_CLOSED, "反结账"),
        ] {
            let error = period_locked("2026-11", "CS-202611-0001", status);
            assert_eq!(error.status, 409, "{status}");
            assert_eq!(error.code, error_code::ERP_COST_PERIOD_STATUS_GUARD, "{status}");
            assert!(
                error.msg.contains(hint),
                "{status} 的文案要指向「{hint}」：{}",
                error.msg
            );
            assert!(error.msg.contains("CS-202611-0001"), "报文要点名批次号：{}", error.msg);
            assert!(error.msg.contains("2026-11"), "报文要点名期间：{}", error.msg);
        }
    }

    /// 锁定的状态清单必须**恰好**等于「活跃批次里除 `DRAFT` 之外的那些」。
    ///
    /// 活跃 = `uk_biz_cost_period` 未排除的全部状态（`STATUSES` 去掉 `REVERSED` / `CANCELLED`）。
    /// 将来往 `STATUSES` 里加一个状态（例如审批引擎改口径引入 `PENDING`），这条会红，
    /// 逼着人回答「这个状态能不能重算」——而不是让 `ensure_batch` 悄悄把它当草稿。
    #[test]
    fn locked_statuses_match_the_live_set() {
        let live_non_draft: Vec<&str> = period_dto::STATUSES
            .iter()
            .copied()
            .filter(|status| *status != STATUS_DRAFT)
            .filter(|status| !matches!(*status, period_dto::STATUS_REVERSED | period_dto::STATUS_CANCELLED))
            .collect();
        assert_eq!(
            live_non_draft,
            LOCKED_STATUSES.to_vec(),
            "活跃但非 DRAFT 的状态必须逐个登记在 LOCKED_STATUSES 里"
        );
        // 红冲 / 作废**不**在锁定清单里：它们不占唯一键，calc 要能建新批次
        for status in [period_dto::STATUS_REVERSED, period_dto::STATUS_CANCELLED] {
            assert!(!LOCKED_STATUSES.contains(&status), "{status} 不活跃，不该被锁");
        }
    }

    /// 值域外的状态（这次误报里的 `PENDING`）也一律 409 —— fail closed，不给「当草稿」的机会。
    #[test]
    fn unknown_status_fails_closed() {
        let error = period_locked("2026-11", "CS-202611-0001", "PENDING");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::ERP_COST_PERIOD_STATUS_GUARD);
        assert!(error.msg.contains("PENDING"), "报文要点名当前状态：{}", error.msg);
        assert!(
            error.msg.contains("撤回") || error.msg.contains("红冲") || error.msg.contains("作废"),
            "未知状态的兜底文案要给出可执行动作：{}",
            error.msg
        );
    }

    /// 段名后缀**不改变**原状态码 / 业务码（前端按码分支的逻辑不能被编排层破坏）。
    #[test]
    fn stage_suffix_keeps_code_and_status() {
        let original = ApiError::new(409, error_code::ERP_COST_OUTPUT_QTY_INVALID, "产量不能为负")
            .with_data(serde_json::json!({ "object_no": "CB-1" }));
        let wrapped = stage_failed(original, 6, "成本计算单");
        assert_eq!(wrapped.status, 409);
        assert_eq!(wrapped.code, error_code::ERP_COST_OUTPUT_QTY_INVALID);
        assert!(wrapped.msg.starts_with("产量不能为负"), "原文必须在前：{}", wrapped.msg);
        assert!(wrapped.msg.contains("第 6 段「成本计算单」"), "{}", wrapped.msg);
        assert!(wrapped.msg.contains("整笔已回滚"), "{}", wrapped.msg);
        assert!(wrapped.data.is_some(), "结构化载荷（前端据此引导）不能在编排层丢掉");
    }

    /// 出参契约：详情字段**平铺**在顶层（前端按 `CostPeriodDetail` 解析不能缺字段），
    /// `calc` 是增量字段。
    #[test]
    fn response_shape_flattens_detail_and_adds_calc() {
        let detail = crate::business::erp::cost_period_dto::CostPeriodDetailRes {
            id: "b".to_string(),
            org_id: Uuid::nil().to_string(),
            period_code: "2026-11".to_string(),
            batch_no: "CS-202611-0001".to_string(),
            status: STATUS_DRAFT.to_string(),
            labor_source: period_dto::LABOR_SOURCE_MANUAL.to_string(),
            object_count: 1,
            total_material: "19000.0000".to_string(),
            total_labor: "4000.0000".to_string(),
            total_overhead: "4000.0000".to_string(),
            total_cost: "27000.0000".to_string(),
            total_finished: "22311.1104".to_string(),
            total_wip: "4688.8896".to_string(),
            approval_instance_id: None,
            labor_voucher_id: None,
            overhead_voucher_id: None,
            finish_voucher_id: None,
            settled_at: None,
            settled_by: None,
            settled_by_name: None,
            reversed_at: None,
            reverse_reason: None,
            cancel_reason: None,
            remark: None,
            created_by_name: Some("张三".to_string()),
            created_at: "2026-11-11T00:00:00+08:00".to_string(),
            updated_at: "2026-11-11T00:00:00+08:00".to_string(),
            items: Vec::new(),
            items_summary: crate::business::erp::cost_period_model::PeriodTotals::default().to_summary(),
            next_statuses: vec!["SUBMITTED".to_string()],
            legal_edges: Vec::new(),
        };
        let summary = CalcRunSummary {
            period_code: "2026-11".to_string(),
            batch_id: "b".to_string(),
            batch_no: "CS-202611-0001".to_string(),
            batch_created: true,
            material: MaterialStage {
                scanned: 2,
                inserted: 2,
                purged: 0,
                amount: "19000.0000".to_string(),
            },
            labor: LaborStage {
                scanned: 1,
                inserted: 1,
                purged: 0,
                amount: "4000.0000".to_string(),
                uncollected: 0,
            },
            overhead: OverheadStage {
                scanned: 2,
                inserted: 2,
                purged: 0,
                amount: "4000.0000".to_string(),
                pool: "4000.0000".to_string(),
                uncollected: 0,
            },
            reconcile: ReconcileStage {
                pool: "4000.0000".to_string(),
                expected: "0".to_string(),
                pending: "4000.0000".to_string(),
                ledger: "0".to_string(),
                diff: "0".to_string(),
            },
            alloc: AllocStage {
                rules: 2,
                result_rows: 2,
                amount: "4000.0000".to_string(),
                zero_pool: 0,
                uncovered: 0,
            },
            items: ItemStage {
                rows: 1,
                skipped: 0,
                purged: 0,
                total_material: "19000.0000".to_string(),
                total_labor: "4000.0000".to_string(),
                total_overhead: "4000.0000".to_string(),
                total_cost: "27000.0000".to_string(),
                total_finished: "22311.1104".to_string(),
                total_wip: "4688.8896".to_string(),
            },
        };
        let res = CostCalcRes { detail, calc: summary };
        let json = serde_json::to_value(&res).expect("序列化");
        // 详情字段在顶层（平铺）
        for field in [
            "id",
            "period_code",
            "batch_no",
            "status",
            "items",
            "items_summary",
            "next_statuses",
            "legal_edges",
            "total_cost",
        ] {
            assert!(json.get(field).is_some(), "顶层缺详情字段 {field}（前端会 undefined）");
        }
        // 分段摘要在 calc 下
        assert_eq!(json["calc"]["batch_created"], serde_json::json!(true));
        assert_eq!(json["calc"]["items"]["rows"], serde_json::json!(1));
        assert_eq!(
            json["calc"]["items"]["total_finished"],
            serde_json::json!("22311.1104"),
            "金额是字符串"
        );
        assert_eq!(json["calc"]["material"]["amount"], serde_json::json!("19000.0000"));
    }

    /// 金额一律 4 位小数字符串（与计算单落库标度一致，前端直接展示不用再格式化）。
    #[test]
    fn money_is_fixed_scale_string() {
        use std::str::FromStr;
        assert_eq!(money(Decimal::ZERO), "0.0000");
        assert_eq!(money(Decimal::from_str("22311.1104").expect("金额")), "22311.1104");
        assert_eq!(money(Decimal::from_str("4000").expect("金额")), "4000.0000");
    }
}

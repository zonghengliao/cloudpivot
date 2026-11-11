//! calc 第 6 段 · **约当产量与成本计算单**（ARCH-9 §4.6 · B2-11）——纯逻辑部分。
//!
//! ```text
//! ① 约当产量（加权平均法，设计 §4.6.1）
//!    材料（一次投入）：EQ_mat = FQ + WQ
//!    加工（人工/费用）：EQ_cnv = FQ + WQ × P / 100
//! ② 单位成本（6 位）
//!    u_mat = round((BC_mat + M) / EQ_mat, 6)
//!    u_lab = round((BC_lab + L) / EQ_cnv, 6)
//!    u_oh  = round((BC_oh  + O) / EQ_cnv, 6)
//! ③ 完工转出（4 位，逐要素算出后相加）
//!    finished = round(FQ × u_mat, 4) + round(FQ × u_lab, 4) + round(FQ × u_oh, 4)
//! ④ 期末在制 = 投入合计 − 完工转出（**差额倒挤**，保证库层 `ck_biz_cost_item_cons` 成立）
//! ⑤ 完工单位成本 = round(finished / FQ, 6) → 结算时写 `biz_stock_in_item.cost_price`
//! ```
//!
//! # 为什么 ④ 必须倒挤（设计 §4.6.1 原话）
//!
//! `round(FQ×u,4) + round(WQ×P×u,4)` 与「投入合计」之间必然有 0.0001–0.0008 的舍入差
//! （算例 A：按公式反算在制 4688.8888，倒挤得 4688.8896，差 0.0008）。两处各算一遍就会出现
//! 「完工 + 在制 ≠ 投入」，而 `ck_biz_cost_item_cons` 是**库层约束**，会直接把结算打回。
//!
//! # ★ 尾差的吸收方：由「有数量的一侧」吸收（设计 §4.6.1 ④ 的一处收敛）
//!
//! 设计只写「期末在制 = 投入 − 完工」，默认在制侧吸收。但 `WQ = 0`（本期全部完工）时，
//! 在制侧吸收会留下「期末在制数量 0、期末在制成本 0.0006」这种行 —— 对象随后关批，
//! 那 0.0006 就永久挂在 4001 上没人认领（C-COST-3 的余额里多出一笔无主的尾差）。
//! 落地口径：**WQ = 0 ⇒ 完工转出 = 投入合计、期末在制 = 0**（尾差进完工）；
//! **WQ > 0 ⇒ 设计原文**（尾差进在制，在制本来就是「剩下的」）。
//! 两条分支都让 `完工 + 在制 = 期初 + 本期投入` 分毫不差。已在卡内报为偏差。
//!
//! # 期初在制怎么来（设计 §4.6.3）
//!
//! 上期**已结算**（`SETTLED`/`CLOSED`）计算单的 `wip_end_qty` / `wip_end_cost` →
//! 本期 `wip_beg_qty` / `wip_beg_cost`；并按上期单位成本把 `wip_end_cost` **还原到三要素**
//! （材料的期初只进材料单位成本，不额外存 3 列，见 [`beg_cost_of`]）：
//!
//! ```text
//! BC_mat = round(u_mat(上期) × WQ(上期), 4)
//! BC_lab = round(u_lab(上期) × (WQ(上期) × P(上期)/100), 4)
//! BC_oh  = wip_end_cost(上期) − BC_mat − BC_lab     ← 三要素合计 = 上期期末在制成本，一分不差
//! ```

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::shared::error_code;

/// 金额 / 数量的小数位（库列 `NUMERIC(18,4)`）。
pub const MONEY_SCALE: u32 = 4;

/// 单位成本的小数位（库列 `NUMERIC(18,6)`）。
pub const UNIT_SCALE: u32 = 6;

/// 百分比的小数位（库列 `NUMERIC(5,2)`）。
pub const PCT_SCALE: u32 = 2;

/// 一期唯一支持的投料方式（设计 §3.1 ②：`AS_PROGRESS` 属三期）。
pub const INPUT_MODE_ONE_TIME: &str = "ONE_TIME";

// ==================== 入参结构 ====================

/// 上期已结算计算单的结转源（设计 §4.6.3）。
#[derive(Debug, Clone, PartialEq)]
pub struct PriorItem {
    /// 上期期间（报文里点名「期初来自哪一期」）。
    pub period_code: String,
    /// 上期期末在制数量。
    pub wip_end_qty: Decimal,
    /// 上期期末在制成本。
    pub wip_end_cost: Decimal,
    /// 上期期末在制完工进度（还原转换成本要用）。
    pub completion_pct: Decimal,
    /// 上期单位材料成本（18,6）。
    pub unit_material: Decimal,
    /// 上期单位人工成本（18,6）。
    pub unit_labor: Decimal,
    /// 上期单位制造费用（18,6）。
    pub unit_overhead: Decimal,
}

/// 期初在制成本按要素的拆分（不落库，只进单位成本的分子）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BegCost {
    /// 期初在制的材料部分。
    pub material: Decimal,
    /// 期初在制的转换成本部分 —— 人工。
    pub labor: Decimal,
    /// 期初在制的转换成本部分 —— 制造费用（含还原时的 4 位尾差）。
    pub overhead: Decimal,
    /// 合计（= 上期期末在制成本，一分不差）。
    pub total: Decimal,
}

/// 本期产量 / 进度输入（设计 §4.5.1：来源是成本对象页，车间主任每月填报）。
#[derive(Debug, Clone, PartialEq)]
pub struct OutputInput {
    /// 累计完工数量（`biz_cost_object.completed_qty`，库注释「结算回写」）。
    pub cumulative_completed_qty: Decimal,
    /// **前期已结算**计算单的完工数量合计（本卡用「累计 − 前期」推出本期完工，见模块头）。
    pub prior_completed_qty: Decimal,
    /// 本期期末在制数量（`biz_cost_object.wip_qty`）。
    pub wip_end_qty: Decimal,
    /// 本期期末在制完工进度 %（`biz_cost_object.completion_pct`）。
    pub completion_pct: Decimal,
    /// 期初在制数量（上期期末在制数量结转；首期为 0）。
    pub prior_wip_qty: Decimal,
    /// 投料方式（一期只允许 `ONE_TIME`）。
    pub material_input_mode: String,
}

impl OutputInput {
    /// 本期完工数量 = 累计完工 − 前期已结算完工合计。
    ///
    /// 为什么不是直接用 `completed_qty`：那一列是**累计**（库注释与前端详情「累计完工」都这么标），
    /// 跨月对象第二个月直接取累计会把前几个月的完工再转出一次（完工翻倍、在制被抓成负数）。
    /// 算例 A 是单期对象（前期 0）⇒ 与设计 §4.6.2 的 800 完全一致。
    pub fn completed_qty_this_period(&self, object_no: &str) -> std::result::Result<Decimal, ApiError> {
        let delta = self.cumulative_completed_qty - self.prior_completed_qty;
        if delta < Decimal::ZERO {
            return Err(output_qty_invalid(
                object_no,
                &format!(
                    "累计完工数量 {} 小于前期已结算计算单的完工合计 {}（成本对象页的累计完工被改小了？）",
                    fmt(self.cumulative_completed_qty, 4),
                    fmt(self.prior_completed_qty, 4)
                ),
            ));
        }
        Ok(delta)
    }
}

/// 一个成本对象本期算出计算单所需的全部输入。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemInput {
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号（报文点名用）。
    pub object_no: String,
    /// 产出品（`biz_cost_period_item.product_id` 冗余快照）。
    pub product_id: Uuid,
    /// 产量 / 进度。
    pub output: OutputInput,
    /// 期初在制（上期结转，首期全 0）。
    pub beg: BegCost,
    /// 本期材料投入（Σ collect element='MATERIAL'）。
    pub mat_cur: Decimal,
    /// 本期人工投入（Σ collect element='LABOR'）。
    pub labor_cur: Decimal,
    /// 本期制造费用投入（Σ collect element='OVERHEAD'，含分摊转入的 ALLOC 行）。
    pub oh_cur: Decimal,
}

/// 一张算完的计算单（= `biz_cost_period_item` 的一行）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemCalc {
    /// 成本对象号。
    pub object_no: String,
    /// 成本对象 id。
    pub cost_object_id: Uuid,
    /// 产出品 id。
    pub product_id: Uuid,
    /// 期初在制数量。
    pub wip_beg_qty: Decimal,
    /// 期初在制成本（= 上期期末在制成本）。
    pub wip_beg_cost: Decimal,
    /// 本期完工数量。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_end_qty: Decimal,
    /// 期末在制完工进度 %。
    pub completion_pct: Decimal,
    /// 约当产量（材料）。
    pub equiv_material: Decimal,
    /// 约当产量（转换成本）。
    pub equiv_conversion: Decimal,
    /// 本期材料投入。
    pub mat_cur: Decimal,
    /// 本期人工投入。
    pub labor_cur: Decimal,
    /// 本期制造费用投入。
    pub oh_cur: Decimal,
    /// 单位材料成本（6 位）。
    pub unit_material: Decimal,
    /// 单位人工成本（6 位）。
    pub unit_labor: Decimal,
    /// 单位制造费用（6 位）。
    pub unit_overhead: Decimal,
    /// 完工转出成本（4 位）。
    pub finished_cost: Decimal,
    /// 期末在制成本（4 位，差额倒挤）。
    pub wip_end_cost: Decimal,
    /// 完工单位成本（6 位）→ 结算写 `biz_stock_in_item.cost_price`。
    pub unit_finished_cost: Decimal,
}

impl ItemCalc {
    /// 本期投入合计（料 + 工 + 费）。
    pub fn input_total(&self) -> Decimal {
        self.mat_cur + self.labor_cur + self.oh_cur
    }

    /// 守恒式左边：完工转出 + 期末在制。
    pub fn outflow_total(&self) -> Decimal {
        self.finished_cost + self.wip_end_cost
    }

    /// 守恒式右边：期初在制 + 本期投入。
    pub fn inflow_total(&self) -> Decimal {
        self.wip_beg_cost + self.input_total()
    }

    /// 恒等式是否成立（`finished + wip_end = wip_beg + mat + labor + oh`）。
    pub fn identity_holds(&self) -> bool {
        self.outflow_total() == self.inflow_total()
    }

    /// 什么都没发生（无投入、无完工、无期末在制）—— 段内跳过、不写 0 行。
    pub fn is_empty(&self) -> bool {
        self.input_total().is_zero()
            && self.completed_qty.is_zero()
            && self.wip_end_qty.is_zero()
            && self.wip_beg_cost.is_zero()
    }

    /// 日志/证据用的一行摘要。
    pub fn summary_line(&self) -> String {
        format!(
            "{} 完工 {} @{} + 在制 {} @{}（投入 {} / 期初 {}）",
            self.object_no,
            fmt(self.completed_qty, 4),
            fmt(self.unit_finished_cost, 6),
            fmt(self.wip_end_qty, 4),
            fmt(self.wip_end_cost, 4),
            fmt(self.input_total(), 4),
            fmt(self.wip_beg_cost, 4)
        )
    }
}

/// 段产出。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRunOutcome {
    /// 期间。
    pub period_code: String,
    /// 命中的批次号。
    pub batch_no: String,
    /// 算出的计算单（顺序稳定：对象号升序）。
    pub items: Vec<ItemCalc>,
    /// 什么成本都没有、不写行的对象。
    pub skipped: Vec<SkippedObject>,
    /// Σ 本期材料投入。
    pub total_material: Decimal,
    /// Σ 本期人工投入。
    pub total_labor: Decimal,
    /// Σ 本期制造费用投入。
    pub total_overhead: Decimal,
    /// Σ 本期投入（= 料 + 工 + 费）。
    pub total_cost: Decimal,
    /// Σ 完工转出。
    pub total_finished: Decimal,
    /// Σ 期末在制。
    pub total_wip: Decimal,
    /// 清掉的旧计算单行数。
    pub purged: i64,
}

impl ItemRunOutcome {
    /// 算出计算单的对象数。
    pub fn object_count(&self) -> usize {
        self.items.len()
    }

    /// 日志/证据用的一行摘要。
    pub fn summary_line(&self) -> String {
        format!(
            "期间 {} 批次 {} 计算单 {} 行（投入 {} = 完工 {} + 在制 {}），清旧 {} 行，跳过 {} 个对象",
            self.period_code,
            self.batch_no,
            self.items.len(),
            fmt(self.total_cost, 4),
            fmt(self.total_finished, 4),
            fmt(self.total_wip, 4),
            self.purged,
            self.skipped.len()
        )
    }
}

/// 没写行的对象（无投入 / 无数量 —— 写 0 行只会让计算单变脏）。
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedObject {
    /// 对象号。
    pub object_no: String,
    /// 人话原因。
    pub reason: String,
}

// ==================== 计算 ====================

/// 4 位金额。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp(MONEY_SCALE)
}

/// 6 位单价 / 单位成本。
pub fn unit_cost(value: Decimal) -> Decimal {
    value.round_dp(UNIT_SCALE)
}

/// 期初在制按要素还原（设计 §4.6.3，口径见模块头）。
///
/// 三要素合计**恒等于** `prior.wip_end_cost`：材料、人工各自按上期单位成本 × 数量算，
/// 差额落在制造费用上（它本来就是「加工成本里说不清的那部分」，且不额外加列）。
pub fn beg_cost_of(prior: &PriorItem) -> std::result::Result<BegCost, ApiError> {
    let conversion_qty = prior.wip_end_qty * prior.completion_pct / Decimal::ONE_HUNDRED;
    let material = money(prior.unit_material * prior.wip_end_qty);
    let labor = money(prior.unit_labor * conversion_qty);
    let overhead = prior.wip_end_cost - material - labor;
    if material < Decimal::ZERO || labor < Decimal::ZERO {
        return Err(beg_cost_invalid(
            &prior.period_code,
            material,
            labor,
            prior.wip_end_cost,
        ));
    }
    Ok(BegCost {
        material,
        labor,
        overhead,
        total: prior.wip_end_cost,
    })
}

/// 约当产量（材料 / 转换成本），**未舍入**（除数要精确，舍入只用在落库值上）。
pub fn equivalents(completed_qty: Decimal, wip_end_qty: Decimal, completion_pct: Decimal) -> (Decimal, Decimal) {
    let material = completed_qty + wip_end_qty;
    let conversion = completed_qty + wip_end_qty * completion_pct / Decimal::ONE_HUNDRED;
    (material, conversion)
}

/// 算一张计算单：校验 → 约当产量 → 单位成本 → 完工转出 → 期末在制（差额倒挤）。
pub fn compose(input: &ItemInput) -> std::result::Result<ItemCalc, ApiError> {
    let out = &input.output;

    // ---- 校验（每条都给「怎么修」，设计 §6.2.2 文案口径）----
    if out.material_input_mode != INPUT_MODE_ONE_TIME {
        return Err(output_qty_invalid(
            &input.object_no,
            &format!(
                "投料方式是 {}，一期只实现「开工一次投入」（ONE_TIME）—— 按进度投料属三期",
                out.material_input_mode
            ),
        ));
    }
    let completed_qty = out.completed_qty_this_period(&input.object_no)?;
    if out.wip_end_qty > Decimal::ZERO && out.completion_pct <= Decimal::ZERO {
        return Err(output_qty_invalid(
            &input.object_no,
            &format!(
                "期末在制 {} 但有完工进度未填（0%），约当产量算不出来；请在成本对象页填写本期完工数量与期末在制后重算",
                fmt(out.wip_end_qty, 4)
            ),
        ));
    }
    if out.completion_pct < Decimal::ZERO || out.completion_pct > Decimal::ONE_HUNDRED {
        return Err(output_qty_invalid(
            &input.object_no,
            &format!("完工进度 {}% 不在 0–100 之间", fmt(out.completion_pct, PCT_SCALE)),
        ));
    }
    let input_total = input.mat_cur + input.labor_cur + input.oh_cur;
    if completed_qty.is_zero() && out.wip_end_qty.is_zero() {
        if input_total.is_zero() && input.beg.total.is_zero() {
            // 什么都没发生：写全 0 行只会让计算单变脏，由调用方跳过（`ItemCalc::is_empty`）。
            return Ok(empty_calc(input, completed_qty));
        }
        return Err(output_qty_invalid(
            &input.object_no,
            &format!(
                "本期既无完工也无期末在制，但有 {} 元成本（期初在制 {}）—— 请在成本对象页填写本期完工数量与期末在制后重算",
                fmt(input_total, 4),
                fmt(input.beg.total, 4)
            ),
        ));
    }
    // 有完工、无任何材料（材料一次投入的语义下才成立）
    if completed_qty > Decimal::ZERO
        && input.mat_cur.is_zero()
        && input.beg.material.is_zero()
        && out.wip_end_qty.is_zero()
    {
        return Err(finished_without_material(
            &input.object_no,
            completed_qty,
            input.beg.total,
        ));
    }

    // ---- 约当产量 ----
    let (eq_material, eq_conversion) = equivalents(completed_qty, out.wip_end_qty, out.completion_pct);

    // ---- 单位成本（6 位）----
    let unit_material = unit_cost(div_or_zero(input.beg.material + input.mat_cur, eq_material));
    let unit_labor = unit_cost(div_or_zero(input.beg.labor + input.labor_cur, eq_conversion));
    let unit_overhead = unit_cost(div_or_zero(input.beg.overhead + input.oh_cur, eq_conversion));
    // 期初要素还原把尾差压给了制造费用，极端情况下可能出现 −0.0001 量级的负数 ⇒ 不外泄负数单价
    if unit_material < Decimal::ZERO || unit_labor < Decimal::ZERO || unit_overhead < Decimal::ZERO {
        return Err(beg_cost_invalid(
            &input.object_no,
            input.beg.material,
            input.beg.labor,
            input.beg.overhead,
        ));
    }

    // ---- 完工转出（4 位，逐要素相加）----
    let finished_by_formula =
        money(completed_qty * unit_material) + money(completed_qty * unit_labor) + money(completed_qty * unit_overhead);

    // ---- 期末在制（差额倒挤；尾差由「有数量的一侧」吸收，见模块头）----
    let inflow = input.beg.total + input_total;
    let (finished_cost, wip_end_cost) = if out.wip_end_qty.is_zero() {
        // 本期全部完工：尾差进完工，在制留 0（否则关批后 4001 永久挂一笔无主尾差）
        (inflow, Decimal::ZERO)
    } else {
        (finished_by_formula, inflow - finished_by_formula)
    };

    if wip_end_cost < Decimal::ZERO || finished_cost < Decimal::ZERO {
        return Err(output_qty_invalid(
            &input.object_no,
            &format!(
                "完工转出 {} 与期末在制 {} 出现了负数（投入合计 {}，期初在制 {}）—— 通常是期末在制数量填得比「期初 + 本期完工」还多",
                fmt(finished_cost, 4),
                fmt(wip_end_cost, 4),
                fmt(inflow, 4),
                fmt(input.beg.total, 4)
            ),
        ));
    }

    let calc = ItemCalc {
        object_no: input.object_no.clone(),
        cost_object_id: input.cost_object_id,
        product_id: input.product_id,
        wip_beg_qty: input.output.prior_wip_qty,
        wip_beg_cost: input.beg.total,
        completed_qty,
        wip_end_qty: out.wip_end_qty,
        completion_pct: out.completion_pct,
        equiv_material: money(eq_material),
        equiv_conversion: money(eq_conversion),
        mat_cur: input.mat_cur,
        labor_cur: input.labor_cur,
        oh_cur: input.oh_cur,
        unit_material,
        unit_labor,
        unit_overhead,
        finished_cost,
        wip_end_cost,
        unit_finished_cost: unit_cost(div_or_zero(finished_cost, completed_qty)),
    };
    ensure_identity(&calc)?;
    Ok(calc)
}

/// 恒等式断言（`完工转出 + 期末在制 = 期初在制 + 本期投入`）。
///
/// 库层 `ck_biz_cost_item_cons` 是同一口径；这里先拦一次，好让报文说清是哪个对象的哪个数不对
/// （写到库里才炸的话，用户只看到「数据不满足约束」）。落库后回查也复用它。
pub fn ensure_identity(calc: &ItemCalc) -> std::result::Result<(), ApiError> {
    if calc.identity_holds() {
        return Ok(());
    }
    Err(identity_broken(
        &calc.object_no,
        &format!(
            "完工转出 {} + 期末在制 {} = {}，但期初在制 {} + 本期投入 {} = {}",
            fmt(calc.finished_cost, 4),
            fmt(calc.wip_end_cost, 4),
            fmt(calc.outflow_total(), 4),
            fmt(calc.wip_beg_cost, 4),
            fmt(calc.input_total(), 4),
            fmt(calc.inflow_total(), 4)
        ),
    ))
}

/// 空计算单（无投入 / 无数量）—— 只用于「跳过」判定，不落库。
fn empty_calc(input: &ItemInput, completed_qty: Decimal) -> ItemCalc {
    ItemCalc {
        object_no: input.object_no.clone(),
        cost_object_id: input.cost_object_id,
        product_id: input.product_id,
        wip_beg_qty: input.output.prior_wip_qty,
        wip_beg_cost: input.beg.total,
        completed_qty,
        wip_end_qty: input.output.wip_end_qty,
        completion_pct: input.output.completion_pct,
        equiv_material: Decimal::ZERO,
        equiv_conversion: Decimal::ZERO,
        mat_cur: input.mat_cur,
        labor_cur: input.labor_cur,
        oh_cur: input.oh_cur,
        unit_material: Decimal::ZERO,
        unit_labor: Decimal::ZERO,
        unit_overhead: Decimal::ZERO,
        finished_cost: Decimal::ZERO,
        wip_end_cost: Decimal::ZERO,
        unit_finished_cost: Decimal::ZERO,
    }
}

/// 除零安全：分母为 0 时返回 0（调用前已用校验挡住「该有分母却没有」的形态）。
fn div_or_zero(numerator: Decimal, denominator: Decimal) -> Decimal {
    if denominator.is_zero() {
        Decimal::ZERO
    } else {
        numerator / denominator
    }
}

/// 小数位格式（报文 / 证据）。
pub fn fmt(value: Decimal, scale: u32) -> String {
    format!("{:.*}", scale as usize, value)
}

// ==================== 错误构造 ====================

/// 409 + `66067`：成本对象的本期产量 / 进度 / 投料方式数据不合法。
///
/// 为什么一个码覆盖 6 种原因：用户的**动作完全相同** —— 回成本对象页改「本期完工数量 /
/// 期末在制 / 完工进度」，然后重算。原因写在报文里，前端不需要给 6 条不同提示。
pub fn output_qty_invalid(object_no: &str, reason: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_OUTPUT_QTY_INVALID,
        format!(
            "成本对象 {object_no} 的本期产量 / 在制数据不合法：{reason}。\n\
             修法：到「成本对象」页面填写本期完工数量、期末在制数量与完工进度后重算；\n\
             完工进度只在「期末在制 > 0」时必填（材料按开工一次投入，在制按 100% 计材料）"
        ),
    )
}

/// 409 + `66068`：有完工数量但一笔材料都没归集（设计 §4.10「零成本完工是数据错误」）。
pub fn finished_without_material(object_no: &str, completed_qty: Decimal, beg_cost: Decimal) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_FINISHED_WITHOUT_MATERIAL,
        format!(
            "成本对象 {object_no} 本期完工 {} 件，但没有任何材料归集（期初在制成本 {}）。\n\
             零成本完工是数据错误，不是业务：材料按「开工一次投入」计入成本，完工了必然投过料。\n\
             修法：检查领料单是否已过账且挂了该成本对象，或到「未归集清单」看漏了哪几张单",
            fmt(completed_qty, 4),
            fmt(beg_cost, 4)
        ),
    )
}

/// 409 + `66069`：上期批次已红冲，本期不得自动结转期初在制（设计 §4.6.3）。
pub fn prior_period_reversed(object_no: &str, periods: &[String]) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_PRIOR_PERIOD_REVERSED,
        format!(
            "成本对象 {object_no} 的上期批次已红冲（期间 {}），期初在制不能自动结转。\n\
             红冲后的上期期末在制成本是「不可信的历史」，直接取数会把错数带进本期。\n\
             修法：先重算上期（红冲批次不占位，同期间可以重建批次），再算本期",
            periods.join("、")
        ),
    )
}

/// 409 + `66070`：该期间成本批次已提交 / 审批 / 结算，禁止重算计算单。
pub fn period_locked(period_code: &str, batch_no: &str, status: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_ITEM_PERIOD_LOCKED,
        format!(
            "期间 {period_code} 的成本批次 {batch_no} 当前状态 {status}，不允许重算成本计算单。\n\
             计算单是结算入库与结转凭证的数字来源，重算会改写已入账的成本。\n\
             修法：提交中的批次先「撤回」、已结算的批次先「红冲」，退回 DRAFT 后再重算"
        ),
    )
}

/// 409 + `66071`：该期间还没有成本核算批次（计算单要挂批次，`period_id` 是外键）。
pub fn period_batch_missing(period_code: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_PERIOD_BATCH_MISSING,
        format!(
            "期间 {period_code} 还没有成本核算批次，成本计算单无处安放（`biz_cost_period_item.period_id` 是外键）。\n\
             修法：先建该期间的成本批次（成本核算页「选期间」会自动建），再重跑计算"
        ),
    )
}

/// 409 + `66067`：期初在制成本的三要素还原出现负数（上期单位成本与期末成本不自洽）。
pub fn beg_cost_invalid(period_code: &str, material: Decimal, labor: Decimal, wip_end_cost: Decimal) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_OUTPUT_QTY_INVALID,
        format!(
            "上期批次 {period_code} 的期末在制成本还原到要素后出现负数：\
             材料 {}、人工 {}、期末在制 {}。\n\
             这说明上期计算单的「单位成本 × 期末在制数量」与期末在制成本自相矛盾（历史数据被改过？）。\n\
             修法：重算上期批次，让单位成本与期末在制成本出自同一次计算，再算本期",
            fmt(material, 4),
            fmt(labor, 4),
            fmt(wip_end_cost, 4)
        ),
    )
}

/// 500：成本守恒断言被打破（算法/写库出了问题，不是用户数据问题）。
pub fn identity_broken(object_no: &str, detail: &str) -> ApiError {
    tracing::error!(object_no, detail, "成本计算单守恒断言失败");
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("成本对象 {object_no} 的计算单不满足守恒式（完工 + 在制 = 期初 + 投入）：{detail}"),
    )
}

/// 500：落库后回查与内存里的结果不一致（丢行 / 标度被截断 / 旧行没清干净）。
pub fn readback_broken(period_code: &str, detail: &str) -> ApiError {
    tracing::error!(period_code, detail, "成本计算单落库后回查不一致");
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("期间 {period_code} 的成本计算单落库后回查不一致：{detail}"),
    )
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn object_input(completed: &str, wip: &str, pct: &str, mat: &str, labor: &str, oh: &str) -> ItemInput {
        ItemInput {
            period_code: "2026-11".to_string(),
            cost_object_id: Uuid::from_u128(1),
            object_no: "CB-20261110-000001".to_string(),
            product_id: Uuid::from_u128(2),
            output: OutputInput {
                cumulative_completed_qty: dec(completed),
                prior_completed_qty: Decimal::ZERO,
                wip_end_qty: dec(wip),
                completion_pct: dec(pct),
                material_input_mode: INPUT_MODE_ONE_TIME.to_string(),
                prior_wip_qty: Decimal::ZERO,
            },
            beg: BegCost::default(),
            mat_cur: dec(mat),
            labor_cur: dec(labor),
            oh_cur: dec(oh),
        }
    }

    /// ★ 卡面验收：**设计 §4.6.2 算例 A 的死数字**逐字段核。
    ///
    /// 输入：料 1200×12.50 + 500×8.00 = 19000；工 80h×50 = 4000；分摊制造费用 4000；
    /// 完工 800；期末在制 200、进度 50%；材料一次投入。
    #[test]
    fn example_a_matches_the_design_numbers() {
        let calc = compose(&object_input("800", "200", "50", "19000", "4000", "4000")).expect("算例 A 必须算得出来");

        // 设计 §4.6.2 的表格逐行
        assert_eq!(fmt(calc.equiv_material, 4), "1000.0000", "EQ_mat = 800 + 200");
        assert_eq!(fmt(calc.equiv_conversion, 4), "900.0000", "EQ_cnv = 800 + 200×50%");
        assert_eq!(fmt(calc.unit_material, 6), "19.000000");
        assert_eq!(fmt(calc.unit_labor, 6), "4.444444");
        assert_eq!(fmt(calc.unit_overhead, 6), "4.444444");
        assert_eq!(fmt(calc.finished_cost, 4), "22311.1104");
        assert_eq!(fmt(calc.wip_end_cost, 4), "4688.8896");
        assert_eq!(fmt(calc.unit_finished_cost, 6), "27.888888", "= 22311.1104 / 800");
        assert_eq!(fmt(calc.input_total(), 4), "27000.0000", "19000 + 4000 + 4000");
        assert_eq!(calc.completed_qty, dec("800"));
        assert_eq!(calc.wip_end_qty, dec("200"));
    }

    /// ★ 卡面验收：**恒等式**（完工 + 期末在制 = 期初在制 + 本期归集）。
    #[test]
    fn identity_holds_on_example_a() {
        let calc = compose(&object_input("800", "200", "50", "19000", "4000", "4000")).expect("算例 A");
        assert!(calc.identity_holds(), "守恒式必须成立");
        assert_eq!(
            fmt(calc.outflow_total(), 4),
            fmt(calc.inflow_total(), 4),
            "22311.1104 + 4688.8896 = 27000.0000 = 期初 0 + 投入 27000"
        );
        ensure_identity(&calc).expect("ensure_identity 也通过");
    }

    /// ★ 卡面验收：**4 位小数尾差倒挤到在制**（设计 §4.6.1 ④ 的 0.0008）。
    ///
    /// 按公式反算期末在制 = 200×19 + 100×4.444444 ×2 = 4688.8888，倒挤得 4688.8896，
    /// 差的 0.0008 就是两个转换要素各 800×0.0000004 的舍入尾差。
    #[test]
    fn rounding_gap_is_absorbed_by_wip() {
        let calc = compose(&object_input("800", "200", "50", "19000", "4000", "4000")).expect("算例 A");
        let formula_wip =
            dec("200") * calc.unit_material + dec("100") * calc.unit_labor + dec("100") * calc.unit_overhead;
        assert_eq!(fmt(formula_wip, 4), "4688.8888", "按公式反算的在制");
        assert_eq!(
            fmt(calc.wip_end_cost - formula_wip, 4),
            "0.0008",
            "尾差 0.0008 必须落在在制（倒挤），否则库层守恒 CHECK 会把结算打回"
        );
    }

    /// 尾差吸收方向：`WQ = 0`（本期全部完工）时尾差进**完工**，在制留 0。
    #[test]
    fn wip_zero_puts_the_gap_into_finished() {
        let calc = compose(&object_input("1000", "0", "0", "9000", "4000", "4000")).expect("全完工");
        assert_eq!(fmt(calc.wip_end_cost, 4), "0.0000", "在制数量 0 ⇒ 在制成本 0");
        assert_eq!(
            fmt(calc.finished_cost, 4),
            "17000.0000",
            "完工 = 投入合计（尾差进完工）"
        );
        assert!(calc.identity_holds());
        assert_eq!(fmt(calc.unit_finished_cost, 6), "17.000000");
    }

    /// 全在制（`FQ = 0`）：完工 0、期末在制 = 投入合计（设计 §4.10「允许全在制」）。
    #[test]
    fn zero_finished_keeps_everything_in_wip() {
        let input = object_input("0", "300", "60", "6000", "1800", "1200");
        let calc = compose(&input).expect("全在制");
        assert_eq!(fmt(calc.completed_qty, 4), "0.0000");
        assert_eq!(fmt(calc.finished_cost, 4), "0.0000");
        assert_eq!(fmt(calc.wip_end_cost, 4), "9000.0000");
        assert_eq!(fmt(calc.equiv_material, 4), "300.0000");
        assert_eq!(fmt(calc.equiv_conversion, 4), "180.0000", "300 × 60%");
        assert_eq!(
            fmt(calc.unit_finished_cost, 6),
            "0.000000",
            "没完工，单位完工成本 0（不除零）"
        );
        assert!(calc.identity_holds());
    }

    /// 一期只支持「开工一次投入」：`AS_PROGRESS` 直接 409 + 66067（不猜三期口径）。
    #[test]
    fn as_progress_input_mode_is_rejected() {
        let mut input = object_input("10", "0", "0", "100", "50", "50");
        input.output.material_input_mode = "AS_PROGRESS".to_string();
        let error = compose(&input).expect_err("三期才支持按进度投料");
        assert_eq!(error.status, 409);
        assert!(
            error.msg.contains("ONE_TIME"),
            "报文要点名一期唯一的投料方式：{}",
            error.msg
        );
    }

    /// 期末在制 > 0 但进度未填 ⇒ 409（报错文案指向「成本对象页」）。
    #[test]
    fn wip_without_progress_is_rejected() {
        let error = compose(&object_input("800", "200", "0", "19000", "4000", "4000")).expect_err("缺进度");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("完工进度未填"), "{}", error.msg);
    }

    /// 有完工、无材料、无期初 ⇒ 409 + 66068（零成本完工是数据错误）。
    #[test]
    fn finished_without_material_is_rejected() {
        let error = compose(&object_input("100", "0", "0", "0", "500", "500")).expect_err("零成本完工");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("没有任何材料归集"), "{}", error.msg);
    }

    /// 有完工、无本期材料，但**期初在制已有材料**（跨月完工既有在制）⇒ 不算错。
    #[test]
    fn finished_without_material_is_fine_when_beg_wip_has_material() {
        let mut input = object_input("200", "0", "0", "0", "400", "400");
        input.beg = BegCost {
            material: dec("3800"),
            labor: dec("444.4444"),
            overhead: dec("444.4452"),
            total: dec("4688.8896"),
        };
        input.output.prior_wip_qty = dec("200");
        input.output.cumulative_completed_qty = dec("200");
        let calc = compose(&input).expect("期初在制里已经有料，本期完工不是零成本完工");
        assert_eq!(fmt(calc.wip_beg_qty, 4), "200.0000");
        assert!(calc.identity_holds());
        assert_eq!(
            fmt(calc.finished_cost, 4),
            "5488.8896",
            "期初 4688.8896 + 本期 800（全完工 ⇒ 尾差进完工）"
        );
    }

    /// 既无完工也无在制却有成本 ⇒ 409（报文给修法）。
    #[test]
    fn cost_without_any_quantity_is_rejected() {
        let error = compose(&object_input("0", "0", "0", "100", "0", "0")).expect_err("有成本没产量");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("本期完工数量与期末在制"), "{}", error.msg);
    }

    /// 累计完工被改小（本期完工为负）⇒ 409，报文点名两个数。
    #[test]
    fn cumulative_qty_rollback_is_rejected() {
        let mut input = object_input("300", "0", "0", "1000", "0", "0");
        input.output.prior_completed_qty = dec("800");
        let error = compose(&input).expect_err("累计完工不能小于前期合计");
        assert_eq!(error.status, 409);
        assert!(error.msg.contains("小于前期已结算计算单的完工合计"), "{}", error.msg);
    }

    /// ★ 本期完工 = 累计完工 − 前期已结算完工（跨月对象不会把前几个月的完工再转出一次）。
    #[test]
    fn completed_qty_is_the_delta_of_cumulative() {
        let mut input = object_input("1300", "0", "0", "5000", "2000", "2000");
        input.output.prior_completed_qty = dec("800");
        let calc = compose(&input).expect("跨月对象");
        assert_eq!(fmt(calc.completed_qty, 4), "500.0000", "1300 − 800");
        assert_eq!(fmt(calc.equiv_material, 4), "500.0000");
    }

    /// 期初在制按上期单位成本还原到三要素，**三要素合计 = 上期期末在制成本**（一分不差）。
    #[test]
    fn beg_cost_split_sums_back_to_prior_wip() {
        let prior = PriorItem {
            period_code: "2026-10".to_string(),
            wip_end_qty: dec("200"),
            wip_end_cost: dec("4688.8896"),
            completion_pct: dec("50"),
            unit_material: dec("19.000000"),
            unit_labor: dec("4.444444"),
            unit_overhead: dec("4.444444"),
        };
        let beg = beg_cost_of(&prior).expect("还原");
        assert_eq!(fmt(beg.material, 4), "3800.0000", "19 × 200");
        assert_eq!(fmt(beg.labor, 4), "444.4444", "4.444444 × 100");
        assert_eq!(
            fmt(beg.overhead, 4),
            "444.4452",
            "差额落在制造费用（444.4444 + 444.4444 = 4688.8888 ≠ 4688.8896）"
        );
        assert_eq!(fmt(beg.total, 4), "4688.8896");
        assert_eq!(
            beg.material + beg.labor + beg.overhead,
            prior.wip_end_cost,
            "三要素合计 = 上期期末在制"
        );
    }

    /// 期初在制数量进 `wip_beg_qty`、成本进 `wip_beg_cost`，且材料的期初只进材料单位成本。
    #[test]
    fn beg_material_only_feeds_the_material_unit_cost() {
        let mut input = object_input("500", "0", "0", "5000", "2000", "2000");
        input.beg = BegCost {
            material: dec("3800"),
            labor: dec("444.4444"),
            overhead: dec("444.4452"),
            total: dec("4688.8896"),
        };
        input.output.prior_wip_qty = dec("200");
        let calc = compose(&input).expect("跨月");
        assert_eq!(fmt(calc.unit_material, 6), "17.600000", "(3800 + 5000) / 500");
        assert_eq!(fmt(calc.unit_labor, 6), "4.888889", "(444.4444 + 2000) / 500");
        assert_eq!(fmt(calc.unit_overhead, 6), "4.888890", "(444.4452 + 2000) / 500");
        assert_eq!(fmt(calc.finished_cost, 4), "13688.8896", "全完工 ⇒ 尾差进完工");
        assert_eq!(fmt(calc.unit_finished_cost, 6), "27.377779");
        assert!(calc.identity_holds());
    }

    /// 空白对象（无投入、无数量、无期初）⇒ `is_empty()`，段内跳过而不写 0 行。
    #[test]
    fn empty_object_is_flagged_not_written() {
        let calc = compose(&object_input("0", "0", "0", "0", "0", "0")).expect("空对象不是错误");
        assert!(calc.is_empty(), "全 0 要能被识别成「跳过」");
    }

    /// 一堆组合下恒等式都成立、且没有任何负数（倒挤不会跑偏）。
    #[test]
    fn identity_holds_across_a_grid() {
        for (done, wip, pct) in [
            ("0", "100", "100"),
            ("1", "1", "1"),
            ("7", "3", "33.33"),
            ("333.3333", "66.6667", "70"),
            ("1000", "0", "0"),
        ] {
            for (mat, labor, oh) in [
                ("0.0001", "0.0001", "0.0001"),
                ("12345.6789", "9876.5432", "4321.0987"),
                ("1", "100", "0"),
            ] {
                let calc = compose(&object_input(done, wip, pct, mat, labor, oh))
                    .unwrap_or_else(|error| panic!("({done},{wip},{pct},{mat}) 应算得出来：{}", error.msg));
                assert!(calc.identity_holds(), "({done},{wip},{pct}) 守恒");
                assert!(calc.finished_cost >= Decimal::ZERO, "完工不能负");
                assert!(calc.wip_end_cost >= Decimal::ZERO, "在制不能负");
                assert!(calc.unit_finished_cost >= Decimal::ZERO, "单位成本不能负");
            }
        }
    }

    /// 完工数量 0 时不做除法（`unit_finished_cost = 0`，不是 NaN / panic）。
    #[test]
    fn zero_completed_qty_never_divides_by_zero() {
        let calc = compose(&object_input("0", "10", "50", "100", "100", "100")).expect("全在制");
        assert_eq!(calc.unit_finished_cost, Decimal::ZERO);
        assert_eq!(fmt(calc.wip_end_cost, 4), "300.0000");
    }
}

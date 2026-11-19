//! 差异分解 · **人工效率差异（工时）** 的契约层与纯逻辑（成本核算 · M5-14）。
//!
//! 本文件只做一件事：把 ARCH-9 §4.9.1 的**第四条**公式做成纯函数 ——
//! `人工效率差异 =（实际工时 − 标准工时）× 标准工资率`，外加两件它必须要有的东西：
//! ① 标准工时的**唯一**算法（`标准单耗工时 × 完工数`，§4.9.1 脚注）；
//! ② 人工两因素（工资率差 + 效率差）的**同参配对**，让 §4.9.2 的
//! `率差 + 效差 = 实际人工 − 标准人工` 在代码里成为构造保证而不是口头约定。
//!
//! 零 DB、零路由、零迁移、零新增权限码。落表（M5-15）/ 凭证（M5-16）/ 单测对拍（M5-17）
//! 直接调 [`labor_eff_variance`]、[`std_hours_of`]、[`labor_two_factor`]。
//!
//! # 一堆话不必再说：算数只有一份
//!
//! 效率差与材料量差**同形**（都是「用量的差 × 标准价」），所以本文件**不重写**算数 ——
//! [`labor_eff_variance`] 校验完工时口径后直接交给 M5-13 的
//! [`qty_variance`](crate::business::erp::cost::cost_qty_variance_dto::qty_variance)。
//! 单测 [`tests::delegates_to_the_qty_family_arithmetic`] 拿两条路径的返回值逐字段比相等：
//! 哪天有人为了「效率差看起来不一样」手抄一份公式，这条测试立刻红。
//!
//! ```text
//! pivot = std_amount_at_actual_qty = money(标准工资率 × 实际工时)   ← M5-12 的右端也是这个数
//! 率差  = money(实际工资率 × 实际工时) − pivot                     ← M5-12（LABOR_RATE）
//! 效差  = pivot − money(标准工资率 × 标准工时)                     ← 本卡（LABOR_EFF）
//! ────────────────────────────────────────────────────────────
//! 率差 + 效差 = money(实际工资率 × 实际工时) − money(标准工资率 × 标准工时)
//! ```
//!
//! 两个半区共用同一个 `pivot`，所以恒等式在 **4 位金额**上精确闭合（不是「差不多相等」）。
//! 单测 [`tests::pivot_is_the_same_number_as_the_wage_rate_line`] 直接比两条线的 pivot 相等。
//!
//! # 本卡的口径（逐条写死，改之前先看 ARCH-9 §4.9）
//!
//! | 问题 | 本卡的口径 | 为什么 |
//! |---|---|---|
//! | 用哪个费率乘 | **标准工资率**，不是实际工资率 | 效率差回答「多干了多少小时 × 这些小时本该多少钱」。用实际费率乘会把工资率差重复计入，两因素恒等式立刻破（正是 §4.9.2 点名的两种常见错法之一） |
//! | 金额取整 | **按项取整**：`amount = money(标准费率 × 实际工时) − money(标准费率 × 标准工时)`，左端与 M5-12 的 pivot 字面同源 | 先差后乘会让恒等式差一厘；左端必须与工资率差那一行是同一个数 |
//! | 标准工时怎么来 | **只走 [`std_hours_of`]**（`单位标准工时 × 完工数`），并**先 round 到列口径 6 位再算金额** | `biz_cost_variance.std_qty` 是 `NUMERIC(18,6)`。让库在 insert 时截断，就会出现「算金额用的工时 ≠ 落表的工时」，报表上恒等式差一厘而且查不出来 |
//! | 符号 | 正数 = 不利（干慢了/多耗了工时）、负数 = 有利（少用了工时） | 与 §4.9.4、`ck_biz_cost_var_sign` 同号，前端不用翻符号 |
//! | 0 差异的方向 | 库层字面值 `UNFAVORABLE`，展示文案「持平」 | `ck_biz_cost_var_dir` 只有两值；文案逻辑复用 M5-12 的 `direction_label_of` |
//! | 输入域 | 实际工时 / 标准工时 / 标准工资率一律 ≥ 0；负数 400 | 负工时不是「负生产」，是上游算错了。静默取绝对值 = 把错账藏起来 |
//! | 输入精度 | 三个入参**最多 6 位小数**（= 落库列标度 `NUMERIC(18,6)`），多了 400 | 台账工时是 `10,2`、标准卡单位工时是 `18,4`，**加权平均出来的实际费率**动辄十几位小数 —— 让库静默截断就会「同一条工时在两条路径上算出不同金额」（`cost_labor_dto` 模块头同一条纪律） |
//! | 差异率 / 工时效率达成率 | **本卡不算** | 分母口径（标准额 / 标准工时）由报表层（M5-19）统一处理，避免两处各写一遍除法 |
//! | 入账吗 | **不入账**（不落凭证、不动存货） | ARCH-9 §4.9.3 / D-15：本期只让差异「被看见」，差异凭证是 M5-16 的独立决策 |
//!
//! # 入参的现实来源（取数在 M5-15，这里只写清口径，避免取错列）
//!
//! | 入参 | 现实来源 | 列精度 |
//! |---|---|---|
//! | `actual_hours` | `biz_cost_labor.hours`（工时台账）按期间 × 成本对象汇总 | `NUMERIC(10,2)`，且 `ck_biz_cost_labor_hours` 要求 `hours > 0` |
//! | `actual_rate` | `biz_cost_labor.rate`（台账费率快照；**单人单行**时直接取，多行见下） | `NUMERIC(18,6)` |
//! | `std_hours` | [`std_hours_of`]`(biz_cost_standard.std_labor_hours, 完工数)` | 单位工时 `18,4` × 完工数 ⇒ 本函数回 6 位 |
//! | `std_rate` | `biz_cost_standard.std_labor_rate` | `NUMERIC(18,6)` |
//!
//! # 两处会静默算错的地方（都已在测试里钉住）
//!
//! **① 多行台账不许先平均再算。** `biz_cost_labor.amount` 由库层
//! `ck_biz_cost_labor_amount CHECK (amount = round(hours * rate, 4))` 逐行锁死 ——
//! 「实际人工成本」在库里是 **Σ 逐行 round**，不是 `round(Σ工时 × 平均费率)`。
//! 因而多行时必须**逐行算差再相加**（跨行汇总属 M5-15 的取数/分摊），
//! 本文件的 [`labor_two_factor`] 只对「一组费率小时口径一致」的行成交。
//!
//! **② `money()` 是银行家舍入，PG `round()` 是半数远离零。** 两者在「恰好中点」上差 0.0001：
//! `money(1.00005) = 1.0000`（进位到偶），而 `round(1.00005::numeric, 4) = 1.0001`
//! （已在 145 真库用 `SELECT` 验过，见 `docs/evidence/2026-11-19-t_3bca9f14-cost-labor-eff-variance/`）。
//! 差异表的 `amount` 沿用 `money()`（M5-11/M5-12/M5-13 全仓同一处舍入，恒等式才闭合）；
//! 与台账 `Σ amount` 对平时若差 0.0001 的整数倍，**概率就是这两个策略的分歧**，不是公式错 ——
//! 单测 [`tests::ledger_rounds_half_up_while_money_rounds_to_even`] 把这一分钱钉成了可复现的断言。

use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::cost::cost_qty_variance_dto::{QtyVarianceLine, qty_variance};
use crate::business::erp::cost::cost_standard_compare_dto::ELEMENT_LABOR;
use crate::business::erp::cost::cost_variance_dto::{
    VARIANCE_LABOR_RATE, VarianceLine, db_direction_of, direction_label_of, price_variance,
};

/// 效率差的差异类型字面值（`biz_cost_variance.variance_type` 的取值之一）。
///
/// **直接复用 M5-13 的常量而不是再写一遍字面量**：两个模块各写一份 `"LABOR_EFF"`，
/// 将来改字面值就会一半改一半没改。这里只做转口。
pub use crate::business::erp::cost::cost_qty_variance_dto::VARIANCE_LABOR_EFF;

/// 工时入参与落库列（`biz_cost_variance.actual_qty` / `std_qty`）的标度：`NUMERIC(18,6)`。
///
/// **别和 `cost_standard_dto::HOURS_SCALE`（= 4）混**：那个是**标准卡上的单位标准工时列**
/// （`biz_cost_standard.std_labor_hours NUMERIC(18,4)`）。这里是差异表存「实际/标准工时」的列，
/// 两者差 2 位，取数时拿错列就会把单位工时当总工时用。
pub const HOURS_SCALE: u32 = 6;

/// 差异表工时列的上限（`NUMERIC(18,6)` = 12 位整数 + 6 位小数）。
///
/// 只用于把「库层会报 500」的输入前移成 400 人话；真实工时比这个数小十几个数量级。
fn hours_max() -> Decimal {
    Decimal::from_i128_with_scale(999_999_999_999_999_999, HOURS_SCALE)
}

/// 工时 / 费率取整策略：与 PG `round(numeric, int)` 同口径（半数远离零）。
///
/// 只用于**把输入对齐到列标度**（[`std_hours_of`] 的乘积、[`labor_eff_variance`] 的精度护栏），
/// **不用于算金额** —— 金额一律走 `money()`（见模块头「两处会静默算错的地方」②）。
const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 标准工时 ====================

/// **标准工时** = `单位标准工时 × 完工数`（ARCH-9 §4.9.1 脚注），结果按落库列标度 round 到 6 位。
///
/// 为什么把这一步做成函数而不是让调用方自己乘：`std_labor_hours` 是 `18,4`，
/// 完工数带小数时乘积会有 7~10 位小数，而差异表 `std_qty` 只有 6 位。
/// 若让库在 insert 时截断，`std_amount = money(费率 × 标准工时)` 用的是一个数、
/// 报表回头读 `std_qty` 拿到的是另一个数 —— 恒等式在报表上差一厘且无从追查。
/// 所以**先 round 到列口径，再用它算钱**（单测
/// [`tests::stored_std_hours_are_the_same_number_used_in_the_amount`] 盯这一条）。
///
/// 两个合法边界（不当错误拦）：
/// - **完工数 = 0**（全在制 / 尚未产出）⇒ 标准工时 0 ⇒ 实际工时全算超用。诚实结果，别跳过；
/// - **单位标准工时 = 0**（新品没定工时 / 标准卡没填）⇒ 标准工时 0，同上。
pub fn std_hours_of(unit_std_hours: Decimal, completed_qty: Decimal) -> std::result::Result<Decimal, ApiError> {
    if unit_std_hours < Decimal::ZERO {
        return Err(invalid(format!(
            "标准工时：单位标准工时不能为负（收到 {unit_std_hours}）。标准卡 std_labor_hours 库层也是 >= 0"
        )));
    }
    if completed_qty < Decimal::ZERO {
        return Err(invalid(format!(
            "标准工时：完工数不能为负（收到 {completed_qty}）。完工数来自生产入库/计算单，负数是上游算错了"
        )));
    }
    let hours = (unit_std_hours * completed_qty).round_dp_with_strategy(HOURS_SCALE, ROUNDING);
    if hours.abs() > hours_max() {
        return Err(invalid(format!(
            "标准工时 {hours} 超出差异表 std_qty 列 NUMERIC(18,6) 的范围。\
             请先核查单位标准工时与完工数是否填错了量级（是不是把「分钟」当「小时」了）"
        )));
    }
    Ok(hours)
}

// ==================== 效率差 ====================

/// **人工效率差异**（ARCH-9 §4.9.1 第 4 行 / §4.9.2 人工算例）：
/// `效率差 =（实际工时 − 标准工时）× 标准工资率`。
///
/// 入参读法与 M5-13 的 `qty_variance(实际量, 标准量, 标准价)` 完全一致，
/// 只是量换成了工时、价换成了工资率 —— 算数**直接委托**给它，不另写一份。
///
/// ```text
/// labor_eff_variance(90, 80, 50.00) ⇒ amount 500.0000 / 不利 / pivot 4500.0000
///                   实际 标准 标准费率   （= (90 − 80) × 50，ARCH-9 §4.9.2 人工算例）
/// ```
///
/// 返回 [`QtyVarianceLine`]（**与量差同一结构**，落表列映射见该结构的文档）：
///
/// | 库列 | 取自 |
/// |---|---|
/// | `variance_type` / `element` | `LABOR_EFF` / `LABOR`（库层 `ck_biz_cost_var_type` / `ck_biz_cost_var_elem` 校验） |
/// | `actual_qty` | 实际**工时**（列名是 qty，语义是「用量」，工时就是人工的用量） |
/// | `std_qty` | 标准工时（**必须**由 [`std_hours_of`] 得出） |
/// | `std_price` | 标准工资率 |
/// | `actual_price` | **写 NULL** —— 效率差公式不吃实际费率（实际费率在 `LABOR_RATE` 那一行） |
/// | `amount` / `direction` | 本结构 `amount` + `direction()` |
pub fn labor_eff_variance(
    actual_hours: Decimal,
    std_hours: Decimal,
    std_rate: Decimal,
) -> std::result::Result<QtyVarianceLine, ApiError> {
    // 守卫按**工时口径**重写（不直接吃 qty_variance 的消息）：
    // 同一个负数，材料侧该说「实际用量」、人工侧该说「实际工时」，否则用户拿着报文找不到是哪一列。
    if actual_hours < Decimal::ZERO {
        return Err(invalid(format!(
            "人工效率差异：实际工时不能为负（收到 {actual_hours}）。负工时说明工时台账/汇总算错了，请先修台账再算差异"
        )));
    }
    if std_hours < Decimal::ZERO {
        return Err(invalid(format!(
            "人工效率差异：标准工时不能为负（收到 {std_hours}）。\
             标准工时 = 单位标准工时 × 完工数，两项都不该是负的（请用 std_hours_of 算）"
        )));
    }
    if std_rate < Decimal::ZERO {
        return Err(invalid(format!(
            "人工效率差异：标准工资率不能为负（收到 {std_rate}）。标准卡 std_labor_rate 库层也是 >= 0；\
             费率 0 是合法输入（未定薪 / 内部支援），负费率不是"
        )));
    }
    ensure_column_scale("实际工时", actual_hours)?;
    ensure_column_scale("标准工时", std_hours)?;
    ensure_column_scale("标准工资率", std_rate)?;

    qty_variance(VARIANCE_LABOR_EFF, ELEMENT_LABOR, actual_hours, std_hours, std_rate)
}

/// 入参精度护栏：超过落库列标度（6 位）就 400，指名是哪一列、该先怎么处理。
///
/// 典型的 7+ 位小数来源是「多行台账的加权平均费率」`Σamount / Σhours`：
/// 它本身不是错，但**落库列只有 6 位**，直接传下去会被 PG 静默截断，
/// 于是算金额的费率与落表的费率不是同一个数。要求调用方显式 round，就是把这件事摆在明面上。
fn ensure_column_scale(field: &str, value: Decimal) -> std::result::Result<(), ApiError> {
    if value.scale() > HOURS_SCALE {
        return Err(invalid(format!(
            "{field} {value} 有 {} 位小数，超过差异表列口径 {HOURS_SCALE} 位（NUMERIC(18,6)）。\
             多出的位数会被库静默截断 ⇒ 算金额用的值与落表的值不是同一个数。\
             请先 round 到 {HOURS_SCALE} 位（与 PG round 同口径：半数远离零）再调用",
            value.scale()
        )));
    }
    Ok(())
}

// ==================== 人工两因素配对 ====================

/// 人工两因素的**同参结果**：工资率差（M5-12）+ 效率差（本卡）。
///
/// 存在的理由只有一条：§4.9.2 要求 `率差 + 效差 = 实际人工 − 标准人工` 必须是**断言**。
/// 两条线若各自从不同入口拿「实际工时」，一处用 90、一处用 88，恒等式当场破掉，
/// 而每一行看起来都「算对了」。所以这里把四个入参只收一次，两条线从同一组数出发。
#[derive(Debug, Clone, PartialEq)]
pub struct LaborTwoFactor {
    /// 工资率差异（`LABOR_RATE`，M5-12 的 `price_variance` 产出）。
    pub rate: VarianceLine,
    /// 效率差异（`LABOR_EFF`，本卡产出）。
    pub eff: QtyVarianceLine,
}

impl LaborTwoFactor {
    /// 实际人工成本（4 位）= `money(实际工资率 × 实际工时)`。
    ///
    /// ⚠️ 与工时台账的 `Σ biz_cost_labor.amount` **在一个条件下相等**：本对象覆盖的所有台账行
    /// 费率与工时口径一致（或只有一行）。多行不同费率见模块头「两处会静默算错的地方」①。
    pub fn actual_amount(&self) -> Decimal {
        self.rate.actual_amount
    }

    /// 标准人工成本（4 位）= `money(标准工资率 × 标准工时)`，恒等式的右端。
    pub fn std_amount(&self) -> Decimal {
        self.eff.std_amount
    }

    /// 两因素合计（4 位，正 = 不利）= 率差 + 效差。
    pub fn total(&self) -> Decimal {
        self.rate.amount + self.eff.amount
    }

    /// 恒等式的残差（4 位）= `(率差 + 效差) − (实际 − 标准)`。**必须恒为 0**。
    ///
    /// 不为 0 只可能是「两条线的 pivot 被各自 round 过一次」或「入参不是同一组」——
    /// 无论哪种，写进差异表的金额都已经错了，不能再往汇总里加。
    pub fn residual(&self) -> Decimal {
        self.total() - (self.actual_amount() - self.std_amount())
    }

    /// 恒等式是否精确闭合（4 位口径）。
    pub fn closes_exactly(&self) -> bool {
        self.residual().is_zero()
    }

    /// 合计口径的库层方向（`FAVORABLE` / `UNFAVORABLE`）。
    pub fn direction(&self) -> &'static str {
        db_direction_of(self.total())
    }

    /// 合计口径的展示文案（不利 / 有利 / 持平）。
    pub fn direction_label(&self) -> &'static str {
        direction_label_of(self.total())
    }

    /// 两因素合计文本（固定 4 位）。
    pub fn total_text(&self) -> String {
        money_text(self.total())
    }
}

/// **人工两因素**：一次拿到工资率差与效率差，且共用同一组入参（实际费率 / 标准费率 / 实际工时 / 标准工时）。
///
/// 率差的算数仍然只有 M5-12 的 `price_variance` 一份、效率差只有 M5-13 的 `qty_variance` 一份 ——
/// 本函数只做「配对 + 守卫顺序」，不新增任何公式。
///
/// ```text
/// labor_two_factor(48, 50, 90, 80)
///   rate = (48 − 50) × 90 = −180.0000（有利）
///   eff  = (90 − 80) × 50 = +500.0000（不利）
///   合计 = 320.0000 = 实际 4320 − 标准 4000   ← §4.9.2 人工算例，精确闭合
/// ```
pub fn labor_two_factor(
    actual_rate: Decimal,
    std_rate: Decimal,
    actual_hours: Decimal,
    std_hours: Decimal,
) -> std::result::Result<LaborTwoFactor, ApiError> {
    // 效率差先算：它的守卫把「工时/费率」两列的精度问题在入口就指出来（消息比率差侧更具体）。
    let eff = labor_eff_variance(actual_hours, std_hours, std_rate)?;
    let rate = price_variance(VARIANCE_LABOR_RATE, ELEMENT_LABOR, actual_rate, std_rate, actual_hours)?;
    Ok(LaborTwoFactor { rate, eff })
}

// ==================== 展示 ====================

/// 工时文本：按列标度 6 位 round 后去掉尾随 0（`80.00` ⇒ `80`，`0.333333` 原样）。
///
/// 工时不是金额：财务看工时习惯「90」「1.5」，而不是「90.0000」；
/// 但也不能用浮点格式化（会丢精度），所以先按列标度 round，再在字符串上去尾零。
/// **别用 `money_text` 显示工时** —— 那个固定 4 位，会把 `0.999999 h` 显示成 `1.0000`。
pub fn hours_text(value: Decimal) -> String {
    // `Decimal` 的 Display 会**带着标度**打印（80.00 ⇒ "80.00"），所以先去小数位再手工去尾零。
    let mut text = value.round_dp_with_strategy(HOURS_SCALE, ROUNDING).to_string();
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    text
}

/// 金额文本（固定 4 位）—— 转口 M5-12 的实现，避免本卡另写一份格式化。
fn money_text(value: Decimal) -> String {
    crate::business::erp::cost::cost_variance_dto::money_text(value)
}

// ==================== 错误构造 ====================

/// 400：入参不合法（不新增业务码：纯入参校验走 `10001`，同 M5-12 / M5-13）。
fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_qty_variance_dto::{mat_qty_variance, variance_label};
    use crate::business::erp::cost::cost_standard_compare_dto::{DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE};
    use crate::business::erp::cost::cost_variance_dto::money_text as mt;
    use std::str::FromStr as _;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    /// 4 位金额文本。
    fn text(value: Decimal) -> String {
        mt(value)
    }

    // ---------- 算例（ARCH-9 §4.9.2 人工算例） ----------

    #[test]
    fn drill_example_labor_efficiency_variance() {
        // 标准：800 件 × 0.10 h × 50.00；实际：90 h × 48.00
        let line = labor_eff_variance(dec("90"), dec("80"), dec("50.00")).expect("人工算例");
        assert_eq!(line.variance_type, VARIANCE_LABOR_EFF);
        assert_eq!(line.element, ELEMENT_LABOR);
        assert_eq!(line.actual_qty, dec("90"), "实际工时");
        assert_eq!(line.std_qty, dec("80"), "标准工时");
        assert_eq!(line.std_price, dec("50.00"), "标准工资率");
        assert_eq!(text(line.std_amount_at_actual_qty), "4500.0000", "分水岭 = 90 × 50.00");
        assert_eq!(text(line.std_amount), "4000.0000", "标准 80 × 50.00");
        assert_eq!(text(line.amount), "500.0000", "(90 − 80) × 50 = +500（不利）");
        assert_eq!(text(line.raw_amount), "500.0000");
        assert_eq!(line.rounding_diff(), Decimal::ZERO);
        assert_eq!(line.qty_delta(), dec("10"), "多用了 10 小时");
        assert_eq!(line.direction(), DIRECTION_UNFAVORABLE);
        assert_eq!(line.direction_label(), "不利");
        assert_eq!(line.amount_text(), "500.0000");
        assert!(!line.is_zero());
    }

    #[test]
    fn formula_matches_the_card_title() {
        // 卡面「效率差 =（实际工时 − 标准工时）× 标准工资率」：raw_amount 就是这个乘式，未取整
        let line = labor_eff_variance(dec("90"), dec("80"), dec("50.00")).expect("人工算例");
        assert_eq!(line.raw_amount, (line.actual_qty - line.std_qty) * line.std_price);
    }

    // ---------- 与 M5-12 / M5-13 的口径同源 ----------

    #[test]
    fn delegates_to_the_qty_family_arithmetic() {
        // 效率差与材料量差同形 ⇒ 算数只有一份。有人手抄公式就会在这条上红
        let eff = labor_eff_variance(dec("90"), dec("80"), dec("50")).expect("效率差");
        let via_family =
            qty_variance(VARIANCE_LABOR_EFF, ELEMENT_LABOR, dec("90"), dec("80"), dec("50")).expect("用量差一族");
        assert_eq!(
            eff, via_family,
            "两条路径必须逐字段相同（同乘式、同 money()、同守卫顺序）"
        );
    }

    #[test]
    fn pivot_is_the_same_number_as_the_wage_rate_line() {
        // 分水岭只有一个：率差行的右端与本卡的左端必须字面相等
        let pair = labor_two_factor(dec("48"), dec("50"), dec("90"), dec("80")).expect("两因素");
        assert_eq!(
            pair.rate.std_amount_at_actual_qty, pair.eff.std_amount_at_actual_qty,
            "pivot 被各自 round 一遍就会在这里分叉，恒等式随之破掉"
        );
        assert_eq!(text(pair.eff.std_amount_at_actual_qty), "4500.0000");
    }

    #[test]
    fn two_factor_uses_one_set_of_inputs_for_both_lines() {
        // 同一实际工时 / 同一标准费率：两条线不可能各拿一个数（这正是本节存在的理由）
        let pair = labor_two_factor(dec("48"), dec("50"), dec("90"), dec("80")).expect("两因素");
        assert_eq!(pair.rate.actual_qty, pair.eff.actual_qty, "同一实际工时");
        assert_eq!(pair.rate.std_price, pair.eff.std_price, "同一标准工资率");
        assert_eq!(pair.rate.actual_price, dec("48"), "实际费率只出现在率差行");
        assert_eq!(pair.eff.std_qty, dec("80"));
        assert_eq!(pair.rate.std_qty, None, "率差公式不吃标准工时（列留空，写库时按 NULL）");
    }

    // ---------- §4.9.2 人工恒等式 ----------

    #[test]
    fn two_factor_identity_closes_on_the_labor_drill() {
        let pair = labor_two_factor(dec("48"), dec("50"), dec("90"), dec("80")).expect("两因素");
        assert_eq!(text(pair.actual_amount()), "4320.0000", "实际 90 × 48");
        assert_eq!(text(pair.std_amount()), "4000.0000", "标准 80 × 50");
        assert_eq!(text(pair.rate.amount), "-180.0000", "率差 (48 − 50) × 90");
        assert_eq!(text(pair.eff.amount), "500.0000", "效差 (90 − 80) × 50");
        assert_eq!(text(pair.total()), "320.0000", "−180 + 500");
        assert_eq!(
            text(pair.actual_amount() - pair.std_amount()),
            "320.0000",
            "4320 − 4000：与两因素合计同一个数"
        );
        assert_eq!(pair.residual(), Decimal::ZERO, "恒等式精确闭合，残差 0");
        assert!(pair.closes_exactly());
        assert_eq!(pair.direction(), DIRECTION_UNFAVORABLE, "净超支 ⇒ 不利");
        assert_eq!(pair.direction_label(), "不利");
        assert_eq!(pair.total_text(), "320.0000");
    }

    #[test]
    fn two_factor_identity_closes_with_six_decimal_rate() {
        // 6 位费率（标准卡 std_labor_rate 的真实精度）：两端各自 4 位取整后仍须闭合
        let pair = labor_two_factor(dec("48.123456"), dec("50.000001"), dec("90.5"), dec("80.25")).expect("两因素");
        assert_eq!(
            text(pair.actual_amount()),
            "4355.1728",
            "48.123456 × 90.5 = 4355.172768"
        );
        assert_eq!(
            text(pair.std_amount()),
            "4012.5001",
            "50.000001 × 80.25 = 4012.50008025"
        );
        assert_eq!(text(pair.total()), "342.6727");
        assert_eq!(pair.residual(), Decimal::ZERO, "6 位费率下残差仍为 0");
    }

    #[test]
    fn identity_residual_is_zero_across_a_grid() {
        // 「恒等式必须做成断言」—— 用一组覆盖正/负/零/6 位小数的输入当断言跑一遍
        let rates = ["0", "48", "48.123456", "50.000001", "120"];
        let hours = ["0", "1", "80", "90.5", "0.000001"];
        for ar in rates {
            for sr in rates {
                for ah in hours {
                    for sh in hours {
                        let pair = labor_two_factor(dec(ar), dec(sr), dec(ah), dec(sh))
                            .unwrap_or_else(|err| panic!("合法输入被拒 {ar}/{sr}/{ah}/{sh}: {}", err.msg));
                        assert_eq!(
                            pair.residual(),
                            Decimal::ZERO,
                            "恒等式破在 {ar}/{sr}/{ah}/{sh}：率差 {} + 效差 {} ≠ 实际 {} − 标准 {}",
                            pair.rate.amount,
                            pair.eff.amount,
                            pair.actual_amount(),
                            pair.std_amount()
                        );
                    }
                }
            }
        }
    }

    // ---------- 标准工时（单位工时 × 完工数） ----------

    #[test]
    fn std_hours_of_multiplies_unit_hours_by_completed_qty() {
        assert_eq!(
            text(std_hours_of(dec("0.10"), dec("800")).expect("算例")),
            "80.0000",
            "0.10 × 800"
        );
        assert!(
            std_hours_of(dec("0.10"), dec("800")).expect("算例").scale() <= HOURS_SCALE,
            "小数位不超过落库列标度 6（Decimal 不去尾零，80.00 的 scale 是 2 —— 库列会存成 80.000000）"
        );
        assert_eq!(text(std_hours_of(dec("0.3333"), dec("3")).expect("算例")), "0.9999");
        assert_eq!(text(std_hours_of(dec("2.5"), dec("33")).expect("算例")), "82.5000");
    }

    #[test]
    fn std_hours_of_rounds_half_away_from_zero_like_postgres() {
        // 0.001 × 0.0005 = 0.0000005 ⇒ 半远离零进到 0.000001（银行家舍入会给 0.000000）
        let hours = std_hours_of(dec("0.001"), dec("0.0005")).expect("临界值");
        assert_eq!(hours, dec("0.000001"), "与 PG round(numeric,6) 同口径（145 真库已验）");
        assert_eq!(hours.scale(), HOURS_SCALE);
        assert_eq!(hours_text(hours), "0.000001");
        // 反向临界：0.0000004 ⇒ 舍到 0
        assert_eq!(
            std_hours_of(dec("0.0004"), dec("0.001")).expect("临界值"),
            Decimal::ZERO
        );
    }

    #[test]
    fn stored_std_hours_are_the_same_number_used_in_the_amount() {
        // 先 round 到列口径再用它算钱：落表的 std_qty 与算 std_amount 的工时必须是同一个数
        let std_hours = std_hours_of(dec("0.333333"), dec("3")).expect("标准工时");
        assert_eq!(
            hours_text(std_hours),
            "0.999999",
            "工时文本不用 4 位金额格式（否则显示成 1.0000）"
        );
        let line = labor_eff_variance(dec("1.5"), std_hours, dec("50")).expect("效率差");
        assert_eq!(line.std_qty, std_hours, "落表的 std_qty 就是算钱用的那个值");
        assert_eq!(
            text(line.std_amount),
            "50.0000",
            "money(50 × 0.999999) = 49.99995 ⇒ 50.0000"
        );
        assert_eq!(text(line.amount), "25.0000", "money(75) − 50 = 25");
    }

    #[test]
    fn std_hours_of_zero_completions_is_zero_not_an_error() {
        // 全在制 / 尚未产出：标准工时 0 是合法结果，实际工时全算超用
        let std_hours = std_hours_of(dec("0.10"), Decimal::ZERO).expect("0 完工");
        assert_eq!(std_hours, Decimal::ZERO);
        let line = labor_eff_variance(dec("12"), std_hours, dec("50")).expect("效率差");
        assert_eq!(
            text(line.amount),
            "600.0000",
            "没完工却出了 12 小时 ⇒ 600 不利，这是诚实结果"
        );
        assert_eq!(line.direction(), DIRECTION_UNFAVORABLE);
    }

    fn assert_invalid<T: std::fmt::Debug>(result: std::result::Result<T, ApiError>, needle: &str) -> ApiError {
        let err = result.expect_err("非法输入必须被拒，不静默纠正");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert_eq!(err.status, 400);
        assert!(
            err.msg.contains(needle),
            "错误消息要指到具体字段（含「{needle}」），实际：{}",
            err.msg
        );
        err
    }

    #[test]
    fn std_hours_of_rejects_negative_inputs() {
        assert_invalid(std_hours_of(dec("-0.1"), dec("800")), "单位标准工时");
        assert_invalid(std_hours_of(dec("0.1"), dec("-1")), "完工数");
    }

    #[test]
    fn std_hours_of_rejects_overflow_beyond_the_column() {
        // 18,4 的单位工时 × 六位数量级 ⇒ 远超 NUMERIC(18,6) 的 12 位整数
        let err = assert_invalid(std_hours_of(dec("99999999.9999"), dec("999999")), "NUMERIC(18,6)");
        assert!(err.msg.contains("分钟"), "要提示最常见的量级错：{}", err.msg);
    }

    // ---------- 边界：0 值 / 0 费率 / 0 工时 ----------

    #[test]
    fn zero_std_hours_puts_all_actual_hours_into_the_variance() {
        // 标准工时 0（新品没定工时 / 标准卡没填工时）：实际工时全算超用
        let line = labor_eff_variance(dec("90"), Decimal::ZERO, dec("50")).expect("标准工时 0");
        assert_eq!(text(line.std_amount_at_actual_qty), "4500.0000");
        assert_eq!(text(line.std_amount), "0.0000");
        assert_eq!(text(line.amount), "4500.0000");
        assert_eq!(line.direction(), DIRECTION_UNFAVORABLE);
    }

    #[test]
    fn zero_actual_hours_is_favorable_and_means_missing_ledger() {
        // 0 实际工时在数学上是「全额有利」，但库里 hours > 0 是硬约束 ⇒ 实际是「一条台账都没有」。
        // 本卡不拦（数学上合法），由 M5-15 把它归进「未归集」而不是当成绩效优良入表。
        let line = labor_eff_variance(Decimal::ZERO, dec("80"), dec("50")).expect("0 工时");
        assert_eq!(text(line.amount), "-4000.0000");
        assert_eq!(line.direction(), DIRECTION_FAVORABLE);
        assert_eq!(line.direction_label(), "有利");
    }

    #[test]
    fn zero_std_rate_makes_the_efficiency_variance_vanish() {
        // 标准费率 0（未定薪）：效差恒 0，差异全部落在率差那一半
        let line = labor_eff_variance(dec("90"), dec("80"), Decimal::ZERO).expect("费率 0");
        assert!(line.is_zero());
        assert_eq!(text(line.std_amount_at_actual_qty), "0.0000");
        assert_eq!(line.direction_label(), "持平");
        let pair = labor_two_factor(dec("48"), Decimal::ZERO, dec("90"), dec("80")).expect("两因素");
        assert_eq!(text(pair.rate.amount), "4320.0000", "费率 0 ⇒ 实际人工全额归率差侧");
        assert_eq!(text(pair.total()), "4320.0000");
        assert!(pair.closes_exactly());
    }

    #[test]
    fn no_hours_difference_is_zero_with_writable_direction() {
        let line = labor_eff_variance(dec("90"), dec("90"), dec("50")).expect("零工时差");
        assert!(line.is_zero());
        assert_eq!(line.direction_label(), "持平", "人看的是持平");
        assert_eq!(
            line.direction(),
            DIRECTION_UNFAVORABLE,
            "库层只认两值，0 走 amount >= 0 那支（ck_biz_cost_var_sign 不拦）"
        );
        assert_eq!(line.qty_delta(), Decimal::ZERO);
    }

    #[test]
    fn shorter_hours_than_standard_is_favorable() {
        let line = labor_eff_variance(dec("70"), dec("80"), dec("50")).expect("省工时");
        assert_eq!(text(line.amount), "-500.0000");
        assert_eq!(line.direction(), DIRECTION_FAVORABLE);
        assert_eq!(line.qty_delta(), dec("-10"));
    }

    // ---------- 取整与尾差 ----------

    #[test]
    fn amount_is_rounded_to_four_decimals() {
        // 6 位费率：3.5 × 1.234567 = 4.3209845 ⇒ 4.3210；3.25 × 1.234567 = 4.01234275 ⇒ 4.0123
        let line = labor_eff_variance(dec("3.5"), dec("3.25"), dec("1.234567")).expect("6 位费率");
        assert_eq!(text(line.std_amount_at_actual_qty), "4.3210");
        assert_eq!(text(line.std_amount), "4.0123");
        assert_eq!(text(line.amount), "0.3087");
        assert!(line.amount.scale() <= 4, "入库列是 NUMERIC(18,4)");
        assert_eq!(line.raw_amount, dec("0.30864175"), "精确值 8 位，raw 不取整");
        assert_eq!(text(line.rounding_diff()), "0.0001", "口径尾差 0.3087 − 0.30864175");
    }

    #[test]
    fn rounding_diff_surfaces_when_the_two_legs_round_apart() {
        // 两端 4 位取舍方向相反：raw = 2 × 1.00005 = 2.00010，口径金额 = 3.0002 − 1.0000 = 2.0002
        let line = labor_eff_variance(dec("3"), dec("1"), dec("1.00005")).expect("临界值");
        assert_eq!(text(line.std_amount_at_actual_qty), "3.0002");
        assert_eq!(text(line.std_amount), "1.0000");
        assert_eq!(text(line.amount), "2.0002");
        assert_eq!(line.raw_amount, dec("2.00010"));
        assert_eq!(
            text(line.rounding_diff()),
            "0.0001",
            "口径尾差必须算得出来（对平用，不是差异）"
        );
    }

    #[test]
    fn ledger_rounds_half_up_while_money_rounds_to_even() {
        // 台账 ck_biz_cost_labor_amount 用的是 PG round(…,4)（半数远离零）；差异表金额走 money()（银行家舍入）。
        // 恰好中点上两者差 0.0001 —— 与台账对平时看到这一分钱，先看这里，别改公式。
        let hours = dec("2.00");
        let rate = dec("1.000025"); // 2.00 × 1.000025 = 2.00005，正落在中点上
        let line = labor_eff_variance(hours, hours, rate).expect("临界值");
        assert_eq!(
            text(line.std_amount_at_actual_qty),
            "2.0000",
            "money(2.00005) ⇒ 进位到偶 ⇒ 2.0000"
        );
        assert_eq!(
            dec("2.0001"),
            dec("2.00005").round_dp_with_strategy(4, ROUNDING),
            "PG round(2.00005::numeric,4) = 2.0001（145 真库 SELECT 已验证）"
        );
        assert_ne!(
            line.std_amount_at_actual_qty,
            dec("2.0001"),
            "这一分钱就是两种舍入策略的分歧；差异表口径统一用 money()，对平差 0.0001 不算恒等式破"
        );
    }

    // ---------- 输入域（纯入参校验 ⇒ 400 + 10001，不新增业务码） ----------

    #[test]
    fn negative_actual_hours_is_rejected_in_hours_wording() {
        let err = assert_invalid(labor_eff_variance(dec("-1"), dec("80"), dec("50")), "实际工时");
        assert!(!err.msg.contains("实际用量"), "人工侧不许冒材料侧的词：{}", err.msg);
    }

    #[test]
    fn negative_std_hours_is_rejected() {
        assert_invalid(labor_eff_variance(dec("90"), dec("-1"), dec("50")), "标准工时");
    }

    #[test]
    fn negative_std_rate_is_rejected() {
        assert_invalid(labor_eff_variance(dec("90"), dec("80"), dec("-0.01")), "标准工资率");
    }

    #[test]
    fn hours_beyond_column_scale_are_rejected() {
        // 加权平均费率动辄十几位小数：列只有 6 位，多出的会被库静默截断 ⇒ 入口拒
        let err = assert_invalid(labor_eff_variance(dec("90.1234567"), dec("80"), dec("50")), "实际工时");
        assert!(err.msg.contains("6 位"), "要说清列口径：{}", err.msg);
        assert_invalid(labor_eff_variance(dec("90"), dec("80.1234567"), dec("50")), "标准工时");
        assert_invalid(
            labor_eff_variance(dec("90"), dec("80"), dec("48.1234567")),
            "标准工资率",
        );
    }

    #[test]
    fn amount_beyond_money_column_is_rejected() {
        // 12 位整工时 × 12 位整费率 ⇒ 千万亿级，超出金额列 NUMERIC(18,4)
        let err = assert_invalid(
            labor_eff_variance(dec("999999999999"), Decimal::ZERO, dec("999999999999")),
            "NUMERIC(18,4)",
        );
        assert!(err.msg.contains("量级"), "消息要能让人自己核查量级：{}", err.msg);
    }

    #[test]
    fn two_factor_rejects_bad_input_through_the_same_guards() {
        assert_invalid(
            labor_two_factor(dec("-48"), dec("50"), dec("90"), dec("80")),
            "实际单价",
        );
        assert_invalid(
            labor_two_factor(dec("48"), dec("50"), dec("-90"), dec("80")),
            "实际工时",
        );
    }

    // ---------- 与库层的口径对拍 ----------

    #[test]
    fn direction_always_satisfies_db_sign_check() {
        // ck_biz_cost_var_sign：UNFAVORABLE ⇒ amount >= 0；FAVORABLE ⇒ amount <= 0
        let cases = [
            ("90", "80", "50"), // 正
            ("70", "80", "50"), // 负
            ("90", "90", "50"), // 0
            ("90", "80", "0"),  // 费率 0
            ("90", "0", "50"),  // 标准工时 0
        ];
        for (ah, sh, rate) in cases {
            let line = labor_eff_variance(dec(ah), dec(sh), dec(rate)).expect("合法输入");
            match line.direction() {
                DIRECTION_UNFAVORABLE => assert!(line.amount >= Decimal::ZERO),
                DIRECTION_FAVORABLE => assert!(line.amount <= Decimal::ZERO),
                other => panic!("库层方向只有两个值，得到 {other}"),
            }
        }
    }

    #[test]
    fn db_enum_mirror_matches_v0374_check_constraints() {
        let ddl = include_str!("../../../../migrations/business/V0374__cost_standard_tables.up.sql");
        assert!(
            ddl.contains(&format!("'{VARIANCE_LABOR_EFF}'")),
            "库层 ck_biz_cost_var_type 里没有 {VARIANCE_LABOR_EFF}，常量已与迁移分叉"
        );
        assert!(
            ddl.contains(&format!("'{ELEMENT_LABOR}'")),
            "库层 ck_biz_cost_var_elem 里没有 LABOR"
        );
        for direction in [DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE] {
            assert!(
                ddl.contains(&format!("'{direction}'")),
                "库层 ck_biz_cost_var_dir 里没有 {direction}"
            );
        }
        // 落库列标度是本文件 HOURS_SCALE 的依据：actual_qty / std_qty 必须是 NUMERIC(18,6)
        assert!(
            ddl.contains("actual_qty     NUMERIC(18,6), std_qty    NUMERIC(18,6)"),
            "biz_cost_variance 的工时列标度改了 ⇒ HOURS_SCALE 与 ensure_column_scale 一起改"
        );
    }

    #[test]
    fn entry_point_is_pinned_to_labor_eff() {
        let line = labor_eff_variance(dec("90"), dec("80"), dec("50")).expect("人工算例");
        assert_eq!(line.variance_type, "LABOR_EFF");
        assert_eq!(line.element, "LABOR");
        assert_eq!(
            variance_label(VARIANCE_LABOR_EFF),
            "人工效率差异",
            "文案复用 M5-13 的那一份"
        );
    }

    // ---------- 展示 ----------

    #[test]
    fn hours_text_trims_trailing_zeros_but_keeps_precision() {
        assert_eq!(hours_text(dec("90.000000")), "90");
        assert_eq!(hours_text(dec("90")), "90");
        assert_eq!(hours_text(dec("1.5")), "1.5");
        assert_eq!(hours_text(dec("0.333333")), "0.333333");
        // 超过 6 位的值按 PG 口径 round 后再显示（不出现 0.3333333）
        assert_eq!(hours_text(dec("0.3333333")), "0.333333");
        assert_eq!(hours_text(dec("-10.25")), "-10.25");
    }

    #[test]
    fn efficiency_is_independent_of_the_actual_rate() {
        // 效率差的入参里没有实际费率 —— 结构上就不可能被「用工贵了」污染
        let a = labor_eff_variance(dec("90"), dec("80"), dec("50")).expect("a");
        let pair_low = labor_two_factor(dec("20"), dec("50"), dec("90"), dec("80")).expect("低费率");
        let pair_high = labor_two_factor(dec("200"), dec("50"), dec("90"), dec("80")).expect("高费率");
        assert_eq!(a, pair_low.eff, "实际费率 20 或 200 都不影响效差");
        assert_eq!(pair_low.eff, pair_high.eff);
        assert_ne!(pair_low.rate.amount, pair_high.rate.amount, "变的只有率差");
        // 材料量差走同一算数但类型不同：标签互不串
        assert_eq!(
            mat_qty_variance(dec("10"), dec("8"), dec("5"))
                .expect("量差")
                .variance_type,
            "MAT_QTY"
        );
    }
}

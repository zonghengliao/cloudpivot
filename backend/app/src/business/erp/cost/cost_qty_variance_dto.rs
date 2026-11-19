//! 差异分解 · **材料量差** 的契约层与纯逻辑（成本核算 · M5-13）。
//!
//! 本文件只做一件事：把 ARCH-9 §4.9.1 的**第二条**公式做成纯函数 ——
//! `量差 =（实际用量 − 标准用量）× 标准单价`，外加它的一行结果结构（对应
//! `biz_cost_variance` 一行里**算数相关**的那几列）。
//! 零 DB、零路由、零迁移、零新增权限码 —— 用它的卡（M5-14 效率差 / M5-15 分摊 /
//! M5-16 凭证 / M5-17 三差异单测）直接调 [`mat_qty_variance`] 或通用入口 [`qty_variance`]。
//!
//! # 与 M5-12（价差）的关系：一条分水岭，两个半区
//!
//! `M5-12` 把「按实际量算的标准额」定成**唯一分水岭**：
//!
//! ```text
//! pivot = std_amount_at_actual_qty = money(标准价 × 实际量)
//! 价差  = money(实际价 × 实际量) − pivot        // M5-12，pivot 是它的右端
//! 量差  = pivot − money(标准价 × 标准量)        // 本卡，pivot 是它的左端
//! ────────────────────────────────────────────
//! 价差 + 量差 = money(实际价 × 实际量) − money(标准价 × 标准量)
//! ```
//!
//! 两卡共用同一个 `pivot`，所以 §4.9.2 的两因素恒等式在 **4 位金额**上精确闭合，
//! 不是「差不多相等」。**本卡绝不自己再 round 一次 pivot** —— 那是把闭合点劈成两个的唯一方式，
//! 也是恒等式破掉 0.0001 的唯一来源。单测 [`tests`] 里 `shared_pivot_is_literally_the_same_number`
//! 直接拿价差行的 `std_amount_at_actual_qty` 与本卡的 `std_amount_at_actual_qty` 比相等。
//!
//! # 本卡的口径（逐条写死，改之前先看 ARCH-9 §4.9）
//!
//! | 问题 | 本卡的口径 | 为什么 |
//! |---|---|---|
//! | 用哪个价乘 | **标准价**，不是实际价 | 量差要回答「多用了多少 × 本该多少钱」。用实际价乘会把价差重复计入，两因素恒等式立刻破掉（§4.9.2 明列的两大常见错法之一） |
//! | 金额取整 | **按项取整**：`amount = money(标准价 × 实际量) − money(标准价 × 标准量)`，而不是 `money((实际量−标准量) × 标准价)` | 左端必须与 M5-12 的 pivot 字面相同（同一函数、同一入参、同一取整），右端是标准成本本身。先差后乘会让恒等式差一厘 |
//! | 符号 | 正数 = 不利（多用了）、负数 = 有利（少用了） | 与 §4.9.4、`ck_biz_cost_var_sign` 同号，前端不用翻符号 |
//! | 0 差异的方向 | 库层字面值 `UNFAVORABLE`，展示文案「持平」 | `ck_biz_cost_var_dir` 只有两值；文案逻辑复用 [`direction_label_of`]，不另写一份 |
//! | 输入域 | 实际量 / 标准量 / 标准价一律 ≥ 0；负数直接 400 | 负用量不是「倒退生产」，是上游算错了。静默取绝对值 = 把错账藏起来 |
//! | 要素与类型配对 | `MAT_QTY` ⇔ `MATERIAL`、`LABOR_EFF` ⇔ `LABOR`，**配错直接 400** | 两者公式同形（都是「用量的差 × 标准价」），但写进 `biz_cost_variance` 时 `element` 决定了它汇入哪个成本要素。放任配错 = 造一行永远对不上三要素合计的差异 |
//! | 标准量从哪来 | **本卡不管**（由调用方传） | 标准量 = 标准单耗 × 完工数（§4.9.1 脚注），取数属报表/落表层（M5-15），纯函数不摸 BOM |
//! | 差异率 | **本卡不算** | 分母是标准成本额（右端），为 0 时按 `M5-11` 口径给 `null` 而非 0%，由报表层统一处理 |
//! | 入账吗 | **不入账**（不落凭证、不动存货） | ARCH-9 §4.9.3 / D-15：本期只让差异「被看见」，差异凭证是 M5-16 的独立决策 |
//!
//! # 两个「不知道标准量」的合法场景（别当错误拦）
//!
//! - **标准量 = 0**（BOM 缺项 / 新产品还没定单耗）：量差 = 全部按实际量算的标准额。数值上就是「实际用量全算超用」，
//!   这是诚实的结果 —— 把 0 当「无标准」跳过，报表里就会少一块差异。
//! - **标准价 = 0**（免费料 / 标准卡没填价）：量差恒 0，差异全归价差（M5-12 那侧会给全额）。
//!   两个半区各自为 0 而不是互相串，正是「分水岭」的语义。
//!
//! # 六类差异里本卡交付第二类
//!
//! `biz_cost_variance.variance_type` 有六个值。本文件实现**用量差那一族**
//! （`MAT_QTY` 与 `LABOR_EFF` 同形：都是「用量的差 × 标准价」，一个乘标准单价、一个乘标准工资率）。
//! 价差一族在 M5-12（[`crate::business::erp::cost::cost_variance_dto`]），
//! 制造费用两差（`OH_SPEND` / `OH_VOLUME`）是**另一种形态**（不比两种价，是做预算相减），不归这两族 —— 别混用。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::cost::cost_period_item_dto::money;
use crate::business::erp::cost::cost_standard_compare_dto::{ELEMENT_LABOR, ELEMENT_MATERIAL, ELEMENTS};
use crate::business::erp::cost::cost_variance_dto::{db_direction_of, direction_label_of, max_money, money_text};

// ==================== 值域（库层枚举镜像） ====================

/// 材料量差（ARCH-9 §4.9.1 第 2 行；`biz_cost_variance.variance_type` 的取值之一）。
pub const VARIANCE_MAT_QTY: &str = "MAT_QTY";

/// 人工效率差异：与材料量差**同形**（标准工资率 × 用量的差），M5-14 直接用本文件的算数。
pub const VARIANCE_LABOR_EFF: &str = "LABOR_EFF";

/// 本文件这套算数覆盖的差异类型（只有用量差一族）。
pub const QTY_VARIANCE_TYPES: [&str; 2] = [VARIANCE_MAT_QTY, VARIANCE_LABOR_EFF];

/// 差异类型的人话名（错误消息与报表标题用）。
pub fn variance_label(variance_type: &str) -> &'static str {
    match variance_type {
        VARIANCE_MAT_QTY => "材料量差",
        VARIANCE_LABOR_EFF => "人工效率差异",
        _ => "用量差",
    }
}

/// 用量差一族「该挂哪个成本要素」—— 库里 `element` 列决定这行汇入哪一项三要素合计。
///
/// `MAT_QTY` 只有挂 `MATERIAL`、`LABOR_EFF` 只有挂 `LABOR` 才是对的；
/// 别的组合算数上能算，写进差异表就是一行永远对不上合计口径的脏数据。
pub fn canonical_element(variance_type: &str) -> Option<&'static str> {
    match variance_type {
        VARIANCE_MAT_QTY => Some(ELEMENT_MATERIAL),
        VARIANCE_LABOR_EFF => Some(ELEMENT_LABOR),
        _ => None,
    }
}

// ==================== 结果 ====================

/// 一条用量差的算数结果（对应 `biz_cost_variance` 一行里**算数相关**的列）。
///
/// 维度列（`period_code` / `cost_object_id` / `standard_id` / `material_id` / `create_by`）
/// **刻意不在这里**：本文件是纯函数，不感知期间与主数据，由写库方在 M5-15 / M5-16 补。
///
/// 落库映射（M5-15 照此填）：
///
/// | 库列 | 取自 |
/// |---|---|
/// | `variance_type` / `element` | 本结构同名字段（库层 `ck_biz_cost_var_type` / `ck_biz_cost_var_elem` 校验） |
/// | `actual_qty` / `std_qty` / `std_price` | 本结构同名字段 |
/// | `actual_price` | **写 NULL** —— 量差公式不吃实际单价（写标准价会让人以为实际价=标准价） |
/// | `amount` / `direction` | 本结构 `amount` + [`QtyVarianceLine::direction`]（已是库层列口径） |
///
/// 与 M5-12 的 `VarianceLine` **故意不同构**：那边两端口径是
/// `actual_amount − std_amount_at_actual_qty`（在实际量上比两种价），
/// 这边是 `std_amount_at_actual_qty − std_amount`（在同一价格下比两种量）。
/// 硬套一个结构会让 `actual_amount` 这个名字在本卡里变成谎话，所以各用各的。
#[derive(Debug, Clone, PartialEq)]
pub struct QtyVarianceLine {
    /// 差异类型字面值（见 [`QTY_VARIANCE_TYPES`]）。
    pub variance_type: &'static str,
    /// 要素字面值（与类型配对，见 [`canonical_element`]）。
    pub element: &'static str,
    /// 实际用量（或实际工时）。
    pub actual_qty: Decimal,
    /// 标准用量（或标准工时）= 标准单耗 × 完工数（本卡只管收，不管取）。
    pub std_qty: Decimal,
    /// 标准单价（或标准工资率）—— 量差**只**用这一个价。
    pub std_price: Decimal,
    /// 按实际量算的标准额（4 位）= `money(标准价 × 实际量)`。
    ///
    /// **与 M5-12 价差行的 `std_amount_at_actual_qty` 是同一个数**（同一分水岭，见模块头）。
    pub std_amount_at_actual_qty: Decimal,
    /// 标准成本额（4 位）= `money(标准价 × 标准量)`，即恒等式的右端。
    pub std_amount: Decimal,
    /// **量差**（4 位，正 = 不利）= `std_amount_at_actual_qty − std_amount`。
    pub amount: Decimal,
    /// 未取整的精确乘积差 `(实际量 − 标准量) × 标准价`（6 位量 × 6 位价 ⇒ 最多 12 位），用于解释尾差与对拍。
    pub raw_amount: Decimal,
}

impl QtyVarianceLine {
    /// 库层 `direction` 字面值（只有两个值，可直接写库）。
    pub fn direction(&self) -> &'static str {
        db_direction_of(self.amount)
    }

    /// 展示文案：不利 / 有利 / 持平（与 M5-12 共用 [`direction_label_of`]）。
    pub fn direction_label(&self) -> &'static str {
        direction_label_of(self.amount)
    }

    /// 差异额是否为 0（写库方可用它跳过「零差异行」，别往差异表灌噪声）。
    pub fn is_zero(&self) -> bool {
        self.amount.is_zero()
    }

    /// 用量差本身（实际量 − 标准量，未取整）：报表上「多用了 320 件」显示这个数，不是金额。
    pub fn qty_delta(&self) -> Decimal {
        self.actual_qty - self.std_qty
    }

    /// 口径尾差 = 取整口径金额 − 未取整精确值（4 位）。**不是差异**，只用于对平。
    pub fn rounding_diff(&self) -> Decimal {
        money(self.amount - self.raw_amount)
    }

    /// 差异额文本（固定 4 位，前端与 remark 共用）。
    pub fn amount_text(&self) -> String {
        money_text(self.amount)
    }
}

// ==================== 算数 ====================

/// 用量差一族（材料量差 / 人工效率差异）的公共算数：**在同一标准价下比两种量**。
///
/// 入参顺序统一成「实际X、标准X、乘数」—— 与 M5-12 的
/// `price_variance(实际价, 标准价, 实际量)` 同一读法，本卡是
/// `qty_variance(实际量, 标准量, 标准价)`。
///
/// - `variance_type` 必须是 [`QTY_VARIANCE_TYPES`] 之一（`MAT_PRICE` / `OH_SPEND` 这类不是
///   「比两种量」的公式走这里只会算错，所以直接拒）；
/// - `element` 必须与类型配对（见 [`canonical_element`]），否则写进差异表会挂错要素；
/// - 用量与标准价一律 ≥ 0。
///
/// 结果里的 `amount` 是 4 位金额（口径见模块头），`raw_amount` 是未取整的精确值。
pub fn qty_variance(
    variance_type: &str,
    element: &str,
    actual_qty: Decimal,
    std_qty: Decimal,
    std_price: Decimal,
) -> std::result::Result<QtyVarianceLine, ApiError> {
    let Some(expected_element) = canonical_element(variance_type) else {
        return Err(invalid(format!(
            "差异类型 {variance_type} 不能走用量差公式：本函数只算 {}（用量的差 × 标准价）；\
             价差一族在 M5-12，制造费用两差是「预算相减」的另一种形态，别混用",
            QTY_VARIANCE_TYPES.join(" / ")
        )));
    };
    if !ELEMENTS.contains(&element) {
        return Err(invalid(format!(
            "要素 {element} 不合法：只能是 {} 之一（库层 ck_biz_cost_var_elem 同口径）",
            ELEMENTS.join(" / ")
        )));
    }
    if element != expected_element {
        return Err(invalid(format!(
            "{} 的要素只能是 {expected_element}（收到 {element}）。\
             配错会让这行差异汇入错的成本要素合计，库里 element 列就是干这个的",
            variance_label(variance_type)
        )));
    }
    let label = variance_label(variance_type);
    if actual_qty < Decimal::ZERO {
        return Err(invalid(format!(
            "{label}：实际用量不能为负（收到 {actual_qty}）。负用量说明上游算错了，请先修数量再算差异"
        )));
    }
    if std_qty < Decimal::ZERO {
        return Err(invalid(format!(
            "{label}：标准用量不能为负（收到 {std_qty}）。标准量 = 标准单耗 × 完工数，两项都不该是负的"
        )));
    }
    if std_price < Decimal::ZERO {
        return Err(invalid(format!(
            "{label}：标准单价不能为负（收到 {std_price}）。标准价 0 是合法输入（免费料 / 未定价），负价不是"
        )));
    }

    // 左端必须与 M5-12 的 pivot 字面相同：同一乘式、同一 money()。
    let std_amount_at_actual_qty = money(std_price * actual_qty);
    let std_amount = money(std_price * std_qty);
    let amount = std_amount_at_actual_qty - std_amount; // 两端同为 4 位 ⇒ 结果精确 4 位
    let raw_amount = (actual_qty - std_qty) * std_price;
    if amount.abs() > max_money() {
        return Err(invalid(format!(
            "{label}：差异额 {amount} 超出金额列 NUMERIC(18,4) 的范围。\
             请先核查用量与标准价是否填错了量级"
        )));
    }
    Ok(QtyVarianceLine {
        variance_type: QTY_VARIANCE_TYPES
            .iter()
            .find(|known| **known == variance_type)
            .copied()
            .unwrap_or(VARIANCE_MAT_QTY),
        element: expected_element,
        actual_qty,
        std_qty,
        std_price,
        std_amount_at_actual_qty,
        std_amount,
        amount,
        raw_amount,
    })
}

/// **材料量差**（ARCH-9 §4.9.1 第 2 行 / §4.9.2 算例 C）：
/// `量差 =（实际用量 − 标准用量）× 标准单价`。
///
/// 取整口径、分水岭、符号、输入域见模块头。
///
/// ```text
/// mat_qty_variance(1200, 880, 12.00) ⇒ amount 3840.0000 / 不利
///                 实际量 标准量 标准价   （= (1200 − 880) × 12.00，§4.9.2 算例 C）
/// ```
pub fn mat_qty_variance(
    actual_qty: Decimal,
    std_qty: Decimal,
    std_price: Decimal,
) -> std::result::Result<QtyVarianceLine, ApiError> {
    qty_variance(VARIANCE_MAT_QTY, ELEMENT_MATERIAL, actual_qty, std_qty, std_price)
}

// ==================== 错误构造 ====================

/// 400：入参不合法（不新增业务码，见 `shared/error_code.rs` 段说明：纯入参校验走 10001）。
fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_standard_compare_dto::{DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE};
    use crate::business::erp::cost::cost_variance_dto::{VARIANCE_LABOR_RATE, mat_price_variance, price_variance};
    use std::str::FromStr as _;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn text(value: Decimal) -> String {
        money_text(value)
    }

    // ---------- 算例（ARCH-9 §4.9.2 真库算例 C） ----------

    #[test]
    fn drill_example_c_material_qty_variance() {
        // 标准：800 件 × 1.1 件/件 = 880 件 × 12.00；实际：1200 件 × 12.50
        let line = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("算例 C");
        assert_eq!(line.variance_type, VARIANCE_MAT_QTY);
        assert_eq!(line.element, ELEMENT_MATERIAL);
        assert_eq!(line.actual_qty, dec("1200"));
        assert_eq!(line.std_qty, dec("880"));
        assert_eq!(line.std_price, dec("12.00"));
        assert_eq!(
            text(line.std_amount_at_actual_qty),
            "14400.0000",
            "分水岭 = 1200 × 12.00（与价差行右端同一个数）"
        );
        assert_eq!(text(line.std_amount), "10560.0000", "标准 880 × 12.00");
        assert_eq!(text(line.amount), "3840.0000", "(1200 − 880) × 12.00 = +3840（不利）");
        assert_eq!(text(line.raw_amount), "3840.0000");
        assert_eq!(line.rounding_diff(), Decimal::ZERO, "本例 4 位取舍方向一致，尾差 0");
        assert_eq!(line.qty_delta(), dec("320"), "多用了 320 件");
        assert_eq!(line.direction(), DIRECTION_UNFAVORABLE);
        assert_eq!(line.amount_text(), "3840.0000");
        assert_eq!(line.direction_label(), "不利");
        assert!(!line.is_zero());
    }

    #[test]
    fn formula_matches_the_card_title() {
        // 卡面「量差 =（实际量 − 标准量）× 标准价」：raw_amount 就是这个乘式，未取整
        let line = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("算例 C");
        assert_eq!(line.raw_amount, (line.actual_qty - line.std_qty) * line.std_price);
    }

    // ---------- 与 M5-12 的联合口径（分水岭 + 两因素恒等式） ----------

    #[test]
    fn shared_pivot_is_literally_the_same_number() {
        // 同一入参下，价差行的 std_amount_at_actual_qty 与本卡的分水岭必须字面相等
        let price = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("价差");
        let qty = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("量差");
        assert_eq!(
            price.std_amount_at_actual_qty, qty.std_amount_at_actual_qty,
            "分水岭只有一个：两卡各自 round 一遍就会在这里分叉"
        );
        assert_eq!(text(qty.std_amount_at_actual_qty), "14400.0000");
    }

    #[test]
    fn two_factor_identity_closes_on_the_shared_pivot() {
        // §4.9.2：价差 + 量差 = 实际材料成本 − 标准材料成本
        let price = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("价差");
        let qty = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("量差");
        assert_eq!(text(price.amount), "600.0000");
        assert_eq!(text(qty.amount), "3840.0000");
        assert_eq!(text(price.amount + qty.amount), "4440.0000");
        assert_eq!(
            price.actual_amount - qty.std_amount,
            price.amount + qty.amount,
            "价差 + 量差 = 实际 − 标准"
        );
        assert_eq!(text(price.actual_amount - qty.std_amount), "4440.0000");
    }

    #[test]
    fn identity_holds_with_six_decimal_standard_price() {
        // 6 位标准价（实测下来最容易露尾差的输入）：两端各自取整也必须在 4 位上闭合
        let std_price = dec("0.333333");
        let price = mat_price_variance(dec("0.4"), std_price, dec("1000")).expect("价差");
        let qty = mat_qty_variance(dec("1000"), dec("900"), std_price).expect("量差");
        assert_eq!(text(qty.std_amount_at_actual_qty), "333.3330");
        assert_eq!(text(qty.std_amount), "299.9997");
        assert_eq!(text(qty.amount), "33.3333");
        assert_eq!(text(price.amount), "66.6670");
        assert_eq!(
            price.actual_amount - qty.std_amount,
            price.amount + qty.amount,
            "6 位价下恒等式仍精确闭合"
        );
        assert_eq!(text(price.amount + qty.amount), "100.0003");
    }

    #[test]
    fn labor_efficiency_variance_reuses_the_same_arithmetic() {
        // §4.9.2 人工算例：效率差 = (90 − 80) × 50 = +500（不利）
        let eff = qty_variance(VARIANCE_LABOR_EFF, ELEMENT_LABOR, dec("90"), dec("80"), dec("50")).expect("效率差");
        assert_eq!(text(eff.std_amount_at_actual_qty), "4500.0000");
        assert_eq!(text(eff.std_amount), "4000.0000");
        assert_eq!(text(eff.amount), "500.0000");
        assert_eq!(eff.direction(), DIRECTION_UNFAVORABLE);

        // 与工资率差（M5-12）合起来 = 实际人工 − 标准人工
        let rate =
            price_variance(VARIANCE_LABOR_RATE, ELEMENT_LABOR, dec("48"), dec("50"), dec("90")).expect("工资率差");
        assert_eq!(text(rate.amount), "-180.0000");
        assert_eq!(text(rate.amount + eff.amount), "320.0000");
        assert_eq!(rate.actual_amount - eff.std_amount, rate.amount + eff.amount);
    }

    // ---------- 边界：0 值 / 0 价 / 0 标准量 ----------

    #[test]
    fn no_usage_difference_gives_zero_variance_but_still_a_writable_direction() {
        let line = mat_qty_variance(dec("1200"), dec("1200"), dec("12")).expect("零量差");
        assert!(line.is_zero());
        assert_eq!(text(line.std_amount_at_actual_qty), "14400.0000");
        assert_eq!(text(line.std_amount), "14400.0000");
        assert_eq!(line.direction_label(), "持平", "人看的是持平");
        assert_eq!(
            line.direction(),
            DIRECTION_UNFAVORABLE,
            "库层只认两值，0 走 amount >= 0 那支（ck_biz_cost_var_sign 不拦）"
        );
        assert_eq!(line.qty_delta(), Decimal::ZERO);
    }

    #[test]
    fn less_usage_than_standard_is_favorable() {
        let line = mat_qty_variance(dec("880"), dec("1000"), dec("12")).expect("少用");
        assert_eq!(text(line.amount), "-1440.0000");
        assert_eq!(line.direction(), DIRECTION_FAVORABLE);
        assert_eq!(line.direction_label(), "有利");
        assert_eq!(line.qty_delta(), dec("-120"));
    }

    #[test]
    fn zero_std_qty_puts_the_whole_actual_usage_into_the_variance() {
        // 标准量 0（BOM 缺项 / 新品没定单耗）：实际用量全算超用，这是诚实结果，不是错误
        let line = mat_qty_variance(dec("30"), Decimal::ZERO, dec("10")).expect("标准量 0");
        assert_eq!(text(line.std_amount_at_actual_qty), "300.0000");
        assert_eq!(text(line.std_amount), "0.0000");
        assert_eq!(text(line.amount), "300.0000");
        assert_eq!(line.direction(), DIRECTION_UNFAVORABLE);
    }

    #[test]
    fn zero_std_price_makes_the_qty_variance_vanish() {
        // 标准价 0（免费料 / 未定价）：量差恒 0，差异全部归到价差那一半（M5-12）
        let line = mat_qty_variance(dec("1200"), dec("880"), Decimal::ZERO).expect("标准价 0");
        assert!(line.is_zero());
        assert_eq!(text(line.std_amount_at_actual_qty), "0.0000");
        assert_eq!(text(line.std_amount), "0.0000");
        assert_eq!(line.direction_label(), "持平");
        let price = mat_price_variance(dec("12.50"), Decimal::ZERO, dec("1200")).expect("价差");
        assert_eq!(
            text(price.amount),
            "15000.0000",
            "标准价 0 时全额定在价差侧，量差侧为 0（两个半区各归各位）"
        );
    }

    #[test]
    fn zero_usage_on_both_sides_is_zero() {
        let line = mat_qty_variance(Decimal::ZERO, Decimal::ZERO, dec("12")).expect("双 0");
        assert!(line.is_zero());
        assert_eq!(line.qty_delta(), Decimal::ZERO);
    }

    // ---------- 取整 ----------

    #[test]
    fn amount_is_rounded_to_four_decimals() {
        // 0.9 × 3.333333 = 2.9999997 ⇒ 4 位 3.0000；标准 0.9 × 3 = 2.7000 ⇒ 量差 0.3000
        let line = mat_qty_variance(dec("3.333333"), dec("3"), dec("0.9")).expect("6 位用量");
        assert_eq!(text(line.amount), "0.3000");
        assert!(line.amount.scale() <= 4, "入库列是 NUMERIC(18,4)，不得带 5 位以上小数");
        assert_eq!(line.raw_amount, dec("0.2999997"), "精确值保留 7 位（12 位上限内）");
        assert!(line.raw_amount.scale() > 4, "raw 不取整，才看得见尾差");
        assert_eq!(line.rounding_diff(), Decimal::ZERO, "0.3000 与精确值对齐");
    }

    #[test]
    fn rounding_diff_surfaces_when_the_two_legs_round_apart() {
        // 两端 4 位取舍方向相反：raw = 2 × 1.00005 = 2.00010，口径金额 = 3.0002 − 1.0000 = 2.0002
        // （rust_decimal 默认 banker's rounding：…15 进位到偶、…05 不进位到偶）
        let line = mat_qty_variance(dec("3"), dec("1"), dec("1.00005")).expect("临界值");
        assert_eq!(text(line.std_amount_at_actual_qty), "3.0002");
        assert_eq!(text(line.std_amount), "1.0000");
        assert_eq!(text(line.amount), "2.0002");
        assert_eq!(line.raw_amount, dec("2.00010"), "精确值 2.00010 = 2.0001");
        assert_eq!(
            text(line.rounding_diff()),
            "0.0001",
            "口径尾差必须能算出来（对平用，不是差异）"
        );
    }

    #[test]
    fn rounding_diff_is_zero_in_the_drill_example() {
        let line = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("算例 C");
        assert_eq!(line.rounding_diff(), Decimal::ZERO);
    }

    // ---------- 输入域（纯入参校验 ⇒ 400 + 10001，不新增业务码） ----------

    fn assert_invalid(result: std::result::Result<QtyVarianceLine, ApiError>, needle: &str) -> ApiError {
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
    fn negative_actual_qty_is_rejected() {
        assert_invalid(mat_qty_variance(dec("-1"), dec("10"), dec("12")), "实际用量");
    }

    #[test]
    fn negative_std_qty_is_rejected() {
        assert_invalid(mat_qty_variance(dec("10"), dec("-1"), dec("12")), "标准用量");
    }

    #[test]
    fn negative_std_price_is_rejected() {
        assert_invalid(mat_qty_variance(dec("10"), dec("8"), dec("-0.01")), "标准单价");
    }

    #[test]
    fn price_side_variance_type_is_rejected() {
        // MAT_PRICE / LABOR_RATE 是库层合法值，但「比两种价」的公式走这里必然算错
        for wrong in [
            crate::business::erp::cost::cost_variance_dto::VARIANCE_MAT_PRICE,
            VARIANCE_LABOR_RATE,
        ] {
            let err = qty_variance(wrong, ELEMENT_MATERIAL, dec("10"), dec("8"), dec("12"))
                .expect_err("率差不能套用量差公式");
            assert_eq!(err.code, error_code::PARAM_INVALID);
            assert!(err.msg.contains(VARIANCE_MAT_QTY), "要说清只算哪几类：{}", err.msg);
        }
    }

    #[test]
    fn overhead_variance_type_is_rejected() {
        // 制造费用两差是「预算相减」，不是「比两种量」
        let err = qty_variance("OH_SPEND", ELEMENT_MATERIAL, dec("10"), dec("8"), dec("12"))
            .expect_err("耗费差不能套用量差公式");
        assert_eq!(err.code, error_code::PARAM_INVALID);
    }

    #[test]
    fn unknown_variance_type_is_rejected() {
        let err = qty_variance("MAT_QT", ELEMENT_MATERIAL, dec("10"), dec("8"), dec("12"))
            .expect_err("拼错的类型不许静默通过");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("MAT_QT"));
    }

    #[test]
    fn unknown_element_is_rejected() {
        let err = qty_variance(VARIANCE_MAT_QTY, "TAX", dec("10"), dec("8"), dec("12")).expect_err("要素只能是三要素");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("TAX"));
    }

    #[test]
    fn mismatched_element_is_rejected() {
        // 算数上能算，但写进差异表会挂错要素 ⇒ 这类组合在入口就拒
        let err = qty_variance(VARIANCE_MAT_QTY, ELEMENT_LABOR, dec("10"), dec("8"), dec("12"))
            .expect_err("材料量差不能挂人工要素");
        assert!(err.msg.contains(ELEMENT_MATERIAL), "要说清该挂哪个要素：{}", err.msg);
        let err = qty_variance(VARIANCE_LABOR_EFF, ELEMENT_MATERIAL, dec("90"), dec("80"), dec("50"))
            .expect_err("效率差不能挂材料要素");
        assert!(err.msg.contains(ELEMENT_LABOR), "{}", err.msg);
    }

    #[test]
    fn amount_beyond_money_column_is_rejected() {
        // 12 位整价 × 6 位整量 ⇒ 千万亿级，超出 NUMERIC(18,4)（14 位整数）
        let err = mat_qty_variance(dec("999999.999999"), Decimal::ZERO, dec("999999999999.999999"))
            .expect_err("超金额列上限要在算数层拒掉，别等库层报 500");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(
            err.msg.contains("NUMERIC(18,4)"),
            "消息要能让人自己去核查量级：{}",
            err.msg
        );
    }

    // ---------- 与库层的口径对拍 ----------

    #[test]
    fn direction_always_satisfies_db_sign_check() {
        // ck_biz_cost_var_sign：UNFAVORABLE ⇒ amount >= 0；FAVORABLE ⇒ amount <= 0
        let cases = [
            ("1200", "880", "12.00"), // 正
            ("880", "1000", "12.00"), // 负
            ("1200", "1200", "12"),   // 0
            ("1200", "880", "0"),     // 标准价 0
        ];
        for (aq, sq, sp) in cases {
            let line = mat_qty_variance(dec(aq), dec(sq), dec(sp)).expect("合法输入");
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
        for literal in QTY_VARIANCE_TYPES {
            assert!(
                ddl.contains(&format!("'{literal}'")),
                "库层 ck_biz_cost_var_type 里没有 {literal}，本文件的常量已与迁移分叉"
            );
        }
        for element in ELEMENTS {
            assert!(
                ddl.contains(&format!("'{element}'")),
                "库层 ck_biz_cost_var_elem 里没有 {element}"
            );
        }
        for direction in [DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE] {
            assert!(
                ddl.contains(&format!("'{direction}'")),
                "库层 ck_biz_cost_var_dir 里没有 {direction}"
            );
        }
    }

    #[test]
    fn entry_point_is_pinned_to_mat_qty() {
        // 入口函数不许因为「泛型算数」把类型标签让调用方随便传
        let line = mat_qty_variance(dec("1200"), dec("880"), dec("12")).expect("算例 C");
        assert_eq!(line.variance_type, "MAT_QTY");
        assert_eq!(line.element, "MATERIAL");
        assert_eq!(canonical_element(VARIANCE_MAT_QTY), Some(ELEMENT_MATERIAL));
        assert_eq!(canonical_element(VARIANCE_LABOR_EFF), Some(ELEMENT_LABOR));
        assert_eq!(canonical_element("MAT_PRICE"), None, "率差一族不归本文件");
    }

    #[test]
    fn labels_cover_the_qty_family() {
        assert_eq!(variance_label(VARIANCE_MAT_QTY), "材料量差");
        assert_eq!(variance_label(VARIANCE_LABOR_EFF), "人工效率差异");
    }
}

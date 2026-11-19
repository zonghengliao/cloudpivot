//! 差异分解 · **材料价差** 的契约层与纯逻辑（成本核算 · M5-12）。
//!
//! 本文件只做一件事：把 ARCH-9 §4.9.1 的**第一条**公式做成纯函数 ——
//! `价差 =（实际单价 − 标准单价）× 实际用量`，外加它的一行结果结构（对应
//! `biz_cost_variance` 一行里**算数相关**的那几列）。
//! 零 DB、零路由、零迁移、零新增权限码 —— 用它的卡（M5-13 量差 / M5-14 效率差 /
//! M5-15 分摊 / M5-16 凭证 / M5-17 三差异单测）直接调 [`mat_price_variance`]。
//!
//! # 本卡的口径（逐条写死，改之前先看 ARCH-9 §4.9）
//!
//! | 问题 | 本卡的口径 | 为什么 |
//! |---|---|---|
//! | 金额取整 | **按项取整**（每端各 `round_dp(4)`）而不是「先算差再取整」：`amount = money(实际价 × 实际量) − money(标准价 × 实际量)` | 两因素恒等式 `价差 + 量差 = 实际 − 标准` 要在 **4 位金额**上精确闭合。两端都从同一个 4 位金额出发，相等是构造保证；先差后乘会各自留 0.0001 的尾数，恒等式就差一厘 |
//! | 分水岭（pivot） | `std_amount_at_actual_qty = money(标准价 × 实际量)` —— 既是价差的右端，也是**量差的左端** | 恒等式的闭合点只能有一个。M5-13 的 `量差 = pivot − money(标准价 × 标准量)`，两卡共用这一列，谁都不许自己再 round 一遍 |
//! | 符号 | 正数 = 不利（超支）、负数 = 有利（节约） | 与 ARCH-9 §4.9.4 和 `ck_biz_cost_var_sign` 同号；前端不用再翻符号 |
//! | 0 差异的方向 | 库层字面值给 `UNFAVORABLE`（`amount >= 0` 那支），展示文案给「持平」 | `ck_biz_cost_var_dir` 只有两个值，写 `EVEN` 会被库层打回；但「持平」是人看的，不该显示成「不利」 |
//! | 输入域 | 单价 / 用量一律 ≥ 0；负数直接 400，不静默取绝对值 | 负用量不是「倒退生产」，是上游算错了。静默纠正 = 把错账藏起来 |
//! | 差异率 | **本卡不算** | 分母是「按实际量算的标准额」= `pivot`；基数为 0 时给 `null` 而不是 0% 的口径与 M5-11 报表一致，由报表层统一处理（避免两处各写一遍除法） |
//! | 入账吗 | **不入账**（不落凭证、不动存货） | ARCH-9 §4.9.3 / D-15：本期只让差异「被看见」，差异凭证是 M5-16 的独立决策 |
//!
//! # 与「先算差再乘」的差别（尾差，不许藏）
//!
//! `raw_amount = (实际价 − 标准价) × 实际量` 是**未取整**的精确值（价 6 位 × 量 6 位 ⇒ 最多 12 位）；
//! 入库的 `amount` 是 4 位。两者之差记为 [`VarianceLine::rounding_diff`]，语义是「口径尾差，不是差异」——
//! 与 M5-11 报表的 `rounding_diff` 同一个用法：财务拿报表对计算单时，差的那 0.0001 得有个名字。
//! 只有「两端口径金额的 4 位取舍方向相反」时才会出现（`4.9.2` 的真库算例 C 尾差恰为 0）。
//!
//! # 六类差异里本卡只交付一类
//!
//! `biz_cost_variance.variance_type` 的库层枚举有六个值（`MAT_PRICE` / `MAT_QTY` /
//! `LABOR_RATE` / `LABOR_EFF` / `OH_SPEND` / `OH_VOLUME`）。本文件只实现**率差那一族**的算数
//! （`MAT_PRICE` 与 `LABOR_RATE` **同形**：在实际量上比两种价），量差 / 效率差 / 制造费用两差
//! 分别在 M5-13 / M5-14 / 后续卡落地 —— 别看着 `RATE_VARIANCE_TYPES` 有两个值就以为量差也算好了。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::cost::cost_period_item_dto::money;
use crate::business::erp::cost::cost_standard_compare_dto::{
    DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE, ELEMENT_MATERIAL, ELEMENTS,
};

// ==================== 值域（库层枚举镜像） ====================

/// 材料价差（ARCH-9 §4.9.1 第 1 行；`biz_cost_variance.variance_type` 的取值之一）。
pub const VARIANCE_MAT_PRICE: &str = "MAT_PRICE";

/// 人工工资率差异：与材料价差**同形**（在实际工时上比两种费率），M5-14 直接用本文件的算数。
pub const VARIANCE_LABOR_RATE: &str = "LABOR_RATE";

/// 本文件这套算数覆盖的差异类型（只有率差一族）。
pub const RATE_VARIANCE_TYPES: [&str; 2] = [VARIANCE_MAT_PRICE, VARIANCE_LABOR_RATE];

/// 金额列 `NUMERIC(18,4)` 的上限（14 位整数 + 4 位小数）。
///
/// 只用于把「库层会报 500」的输入前移成 400 人话；真实成本离这个数有七八个数量级。
/// 量差（M5-13）与后续差异卡共用这一条边界，别各自写一份。
pub(crate) fn max_money() -> Decimal {
    Decimal::from_i128_with_scale(999_999_999_999_999_999, 4)
}

/// 差异类型的人话名（错误消息用）。
pub fn variance_label(variance_type: &str) -> &'static str {
    match variance_type {
        VARIANCE_MAT_PRICE => "材料价差",
        VARIANCE_LABOR_RATE => "工资率差异",
        _ => "率差",
    }
}

// ==================== 结果 ====================

/// 一条差异的算数结果（对应 `biz_cost_variance` 一行里**算数相关**的列）。
///
/// 维度列（`period_code` / `cost_object_id` / `standard_id` / `material_id` / `create_by`）
/// **刻意不在这里**：本文件是纯函数，不感知期间与主数据，由写库方在 M5-15 / M5-16 补。
#[derive(Debug, Clone, PartialEq)]
pub struct VarianceLine {
    /// 差异类型字面值（见 [`RATE_VARIANCE_TYPES`]；库层 `ck_biz_cost_var_type` 校验）。
    pub variance_type: &'static str,
    /// 要素字面值（`MATERIAL` / `LABOR` / `OVERHEAD`；库层 `ck_biz_cost_var_elem` 校验）。
    pub element: &'static str,
    /// 实际用量（或实际工时）。
    pub actual_qty: Decimal,
    /// 实际单价（或实际工资率）。
    pub actual_price: Decimal,
    /// 标准单价（或标准工资率）。
    pub std_price: Decimal,
    /// 标准用量（或标准工时）。
    ///
    /// 价差公式**不吃**这一列 ⇒ 本卡恒为 `None`（库层该列可空）。量差（M5-13）才需要它。
    pub std_qty: Option<Decimal>,
    /// 按实际量算的实际金额（4 位）= `money(实际价 × 实际量)`。
    pub actual_amount: Decimal,
    /// 按实际量算的标准金额（4 位）= `money(标准价 × 实际量)`。
    ///
    /// 两因素的分水岭：价差走到这里为止，量差从这里出发（见模块头「分水岭」）。
    pub std_amount_at_actual_qty: Decimal,
    /// **价差**（4 位，正 = 不利）= `actual_amount − std_amount_at_actual_qty`。
    pub amount: Decimal,
    /// 未取整的精确乘积差 `(实际价 − 标准价) × 实际量`（最多 12 位），用于解释尾差与对拍。
    pub raw_amount: Decimal,
}

impl VarianceLine {
    /// 库层 `direction` 字面值（**只有两个值**，可直接写库）。
    pub fn direction(&self) -> &'static str {
        db_direction_of(self.amount)
    }

    /// 展示文案：不利 / 有利 / 持平（0 差异不显示成「不利」，见模块头口径表）。
    pub fn direction_label(&self) -> &'static str {
        direction_label_of(self.amount)
    }

    /// 差异额是否为 0（写库方可用它跳过「零差异行」，别往差异表灌噪声）。
    pub fn is_zero(&self) -> bool {
        self.amount.is_zero()
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

/// 差异方向 → 库层字面值（`amount >= 0` 记不利，含恰好为 0 的情形）。
///
/// 与报表层的 `EVEN`（「持平」）分开：`ck_biz_cost_var_dir` 只认两个值，写第三个会被库层打回。
pub fn db_direction_of(amount: Decimal) -> &'static str {
    if amount < Decimal::ZERO {
        DIRECTION_FAVORABLE
    } else {
        DIRECTION_UNFAVORABLE
    }
}

/// 差异方向 → 展示文案（不利 / 有利 / 持平）。
///
/// 与 [`db_direction_of`]（写库用，只有两值）**分开**：0 差异给人看是「持平」，
/// 但库里必须落 `UNFAVORABLE`。量差（M5-13）与后续差异卡共用这一份文案逻辑。
pub fn direction_label_of(amount: Decimal) -> &'static str {
    if amount.is_zero() {
        "持平"
    } else if amount > Decimal::ZERO {
        "不利"
    } else {
        "有利"
    }
}

/// 金额文本（固定 4 位，避免科学计数法）。
pub fn money_text(value: Decimal) -> String {
    format!("{:.*}", 4, money(value))
}

// ==================== 算数 ====================

/// 率差一族（材料价差 / 工资率差异）的公共算数：**在「实际量」上比两种价**。
///
/// - `variance_type` 必须是 [`RATE_VARIANCE_TYPES`] 之一（`MAT_QTY` / `OH_SPEND` 这类
///   不是「比价」的公式走这里只会算错，所以直接拒）；
/// - `element` 必须是库层三要素之一（`MATERIAL` / `LABOR` / `OVERHEAD`）；
/// - 单价与用量一律 ≥ 0。
///
/// 结果里的 `amount` 是 4 位金额（口径见模块头），`raw_amount` 是未取整的精确值。
pub fn price_variance(
    variance_type: &str,
    element: &str,
    actual_price: Decimal,
    std_price: Decimal,
    actual_qty: Decimal,
) -> std::result::Result<VarianceLine, ApiError> {
    if !RATE_VARIANCE_TYPES.contains(&variance_type) {
        return Err(invalid(format!(
            "差异类型 {variance_type} 不能走率差公式：本函数只算 {}（实际量 × 价差）；\
             量差 / 效率差 / 制造费用两差各有自己的公式，别混用",
            RATE_VARIANCE_TYPES.join(" / ")
        )));
    }
    if !ELEMENTS.contains(&element) {
        return Err(invalid(format!(
            "要素 {element} 不合法：只能是 {} 之一（库层 ck_biz_cost_var_elem 同口径）",
            ELEMENTS.join(" / ")
        )));
    }
    let label = variance_label(variance_type);
    if actual_qty < Decimal::ZERO {
        return Err(invalid(format!(
            "{label}：实际用量不能为负（收到 {actual_qty}）。负用量说明上游算错了，请先修数量再算差异"
        )));
    }
    if actual_price < Decimal::ZERO {
        return Err(invalid(format!(
            "{label}：实际单价不能为负（收到 {actual_price}）。单价为 0 是合法输入，负单价不是"
        )));
    }
    if std_price < Decimal::ZERO {
        return Err(invalid(format!(
            "{label}：标准单价不能为负（收到 {std_price}）。标准卡库层 CHECK 也是 std_price >= 0"
        )));
    }

    let actual_amount = money(actual_price * actual_qty);
    let std_amount_at_actual_qty = money(std_price * actual_qty);
    let amount = actual_amount - std_amount_at_actual_qty; // 两端同为 4 位 ⇒ 结果精确 4 位
    let raw_amount = (actual_price - std_price) * actual_qty;
    if amount.abs() > max_money() {
        return Err(invalid(format!(
            "{label}：差异额 {amount} 超出金额列 NUMERIC(18,4) 的范围。请先核查单价与用量是否填错了量级"
        )));
    }
    Ok(VarianceLine {
        variance_type: RATE_VARIANCE_TYPES
            .iter()
            .find(|known| **known == variance_type)
            .copied()
            .unwrap_or(VARIANCE_MAT_PRICE),
        element: ELEMENTS
            .iter()
            .find(|known| **known == element)
            .copied()
            .unwrap_or(ELEMENT_MATERIAL),
        actual_qty,
        actual_price,
        std_price,
        std_qty: None,
        actual_amount,
        std_amount_at_actual_qty,
        amount,
        raw_amount,
    })
}

/// **材料价差**（ARCH-9 §4.9.1 第 1 行 / §4.9.2 算例 C）：
/// `价差 =（实际单价 − 标准单价）× 实际用量`。
///
/// 取整口径、分水岭、符号、输入域见模块头。返回一行的算数结果；
/// `std_qty` 为 `None`（价差公式不用标准用量）。
///
/// ```text
/// mat_price_variance(12.50, 12.00, 1200) ⇒ amount 600.0000 / 不利 / pivot 14400.0000
///                       实际价  标准价  实际量      （= (12.50 − 12.00) × 1200，ARCH-9 §4.9.2 算例 C）
/// ```
pub fn mat_price_variance(
    actual_price: Decimal,
    std_price: Decimal,
    actual_qty: Decimal,
) -> std::result::Result<VarianceLine, ApiError> {
    price_variance(
        VARIANCE_MAT_PRICE,
        ELEMENT_MATERIAL,
        actual_price,
        std_price,
        actual_qty,
    )
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
    use std::str::FromStr as _;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn text(value: Decimal) -> String {
        money_text(value)
    }

    // ---------- 算例（ARCH-9 §4.9.2 真库算例 C） ----------

    #[test]
    fn drill_example_c_material_price_variance() {
        // 标准：800 件 × 1.1 件/件 × 12.00；实际：1200 × 12.50
        let line = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("算例 C");
        assert_eq!(line.variance_type, VARIANCE_MAT_PRICE);
        assert_eq!(line.element, ELEMENT_MATERIAL);
        assert_eq!(text(line.actual_amount), "15000.0000", "实际 1200 × 12.50");
        assert_eq!(
            text(line.std_amount_at_actual_qty),
            "14400.0000",
            "分水岭 = 1200 × 12.00（按实际量算的标准额）"
        );
        assert_eq!(text(line.amount), "600.0000", "(12.50 − 12.00) × 1200 = +600（不利）");
        assert_eq!(text(line.raw_amount), "600.0000");
        assert_eq!(line.rounding_diff(), Decimal::ZERO, "本例 4 位取舍方向一致，尾差 0");
        assert_eq!(line.direction(), DIRECTION_UNFAVORABLE);
        assert_eq!(line.amount_text(), "600.0000");
        assert_eq!(line.direction_label(), "不利");
        assert!(!line.is_zero());
        assert_eq!(line.std_qty, None, "价差公式不用标准用量，列留空（量差才填）");
    }

    #[test]
    fn two_factor_identity_closes_on_the_shared_pivot() {
        // §4.9.2 恒等式：价差 + 量差 = 实际材料成本 − 标准材料成本
        let price = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("价差");
        let std_qty = dec("880"); // 800 件 × 1.1
        let std_amount = money(dec("12.00") * std_qty); // 10560.0000
        let act_amount = price.actual_amount; // 15000.0000
        // 量差（M5-13 交付）从同一个分水岭出发 ⇒ 恒等式在 4 位金额上精确闭合
        let qty_amount = price.std_amount_at_actual_qty - std_amount;
        assert_eq!(text(std_amount), "10560.0000");
        assert_eq!(text(qty_amount), "3840.0000", "§4.9.2 算例 C 的量差");
        assert_eq!(
            price.amount + qty_amount,
            act_amount - std_amount,
            "价差 + 量差 = 实际 − 标准"
        );
        assert_eq!(text(act_amount - std_amount), "4440.0000");
    }

    #[test]
    fn labor_rate_variance_reuses_the_same_arithmetic() {
        // §4.9.2 人工算例：工资率差 = (48 − 50) × 90 = −180（有利）
        let line = price_variance(VARIANCE_LABOR_RATE, "LABOR", dec("48"), dec("50"), dec("90")).expect("工资率差");
        assert_eq!(text(line.actual_amount), "4320.0000");
        assert_eq!(text(line.std_amount_at_actual_qty), "4500.0000");
        assert_eq!(text(line.amount), "-180.0000");
        assert_eq!(line.direction(), DIRECTION_FAVORABLE);
        assert_eq!(line.direction_label(), "有利");
        // 效率差那一半（M5-14）：从同一个分水岭出发，(90 − 80) × 50 = 500
        let std_hours = dec("80");
        let efficiency = line.std_amount_at_actual_qty - money(dec("50") * std_hours);
        assert_eq!(text(efficiency), "500.0000");
        assert_eq!(
            line.amount + efficiency,
            line.actual_amount - money(dec("50") * std_hours),
            "率差 + 效差 = 实际 − 标准"
        );
    }

    // ---------- 边界：0 值 / 0 价 / 0 量 ----------

    #[test]
    fn zero_usage_gives_zero_variance_but_still_a_writable_direction() {
        let line = mat_price_variance(dec("12.50"), dec("12.00"), Decimal::ZERO).expect("零用量");
        assert_eq!(line.actual_amount, Decimal::ZERO);
        assert_eq!(line.std_amount_at_actual_qty, Decimal::ZERO);
        assert!(line.is_zero());
        assert_eq!(line.direction_label(), "持平", "人看的是持平");
        assert_eq!(
            line.direction(),
            DIRECTION_UNFAVORABLE,
            "库层只认两值，0 走 amount >= 0 那支（ck_biz_cost_var_sign 不拦）"
        );
    }

    #[test]
    fn equal_prices_give_zero_variance() {
        let line = mat_price_variance(dec("12"), dec("12"), dec("500")).expect("同价");
        assert!(line.is_zero());
        assert_eq!(text(line.actual_amount), "6000.0000");
        assert_eq!(text(line.std_amount_at_actual_qty), "6000.0000");
        assert_eq!(line.direction_label(), "持平");
    }

    #[test]
    fn zero_std_price_makes_the_whole_actual_amount_a_variance() {
        // 标准价 0 是合法输入（免费料 / 标准卡没填价）：价差 = 全部实际材料成本
        let line = mat_price_variance(dec("12.50"), Decimal::ZERO, dec("100")).expect("标准价 0");
        assert_eq!(text(line.std_amount_at_actual_qty), "0.0000");
        assert_eq!(text(line.amount), "1250.0000");
        assert_eq!(line.direction(), DIRECTION_UNFAVORABLE);
    }

    #[test]
    fn whole_actual_amount_is_favorable_when_price_drops_to_zero() {
        let line = mat_price_variance(Decimal::ZERO, dec("8.00"), dec("100")).expect("实际价 0");
        assert_eq!(text(line.amount), "-800.0000");
        assert_eq!(line.direction(), DIRECTION_FAVORABLE);
        assert_eq!(line.amount_text(), "-800.0000");
    }

    // ---------- 取整 ----------

    #[test]
    fn amount_is_rounded_to_four_decimals() {
        // 12.123456 × 7 = 84.864192 ⇒ 4 位 84.8642；标准 84.0000 ⇒ 价差 0.8642
        let line = mat_price_variance(dec("12.123456"), dec("12"), dec("7")).expect("6 位单价");
        assert_eq!(text(line.amount), "0.8642");
        assert!(line.amount.scale() <= 4, "入库列是 NUMERIC(18,4)，不得带 5 位以上小数");
        assert_eq!(line.raw_amount, dec("0.864192"), "精确值保留 6 位（12 位上限内）");
        assert!(line.raw_amount.scale() > 4, "raw 不取整，才看得见尾差");
        assert_eq!(
            line.rounding_diff(),
            Decimal::ZERO,
            "84.8642 − 84.0000 = 0.8642，与精确值对齐"
        );
    }

    #[test]
    fn rounding_diff_surfaces_when_the_two_legs_round_apart() {
        // 两端 4 位取舍方向相反：raw = 1.00010，口径金额 = 2.0002 − 1.0000 = 1.0002
        // （rust_decimal 默认 banker's rounding：…15 进位到偶、…05 不进位到偶）
        let line = mat_price_variance(dec("2.00015"), dec("1.00005"), Decimal::ONE).expect("临界值");
        assert_eq!(text(line.actual_amount), "2.0002");
        assert_eq!(text(line.std_amount_at_actual_qty), "1.0000");
        assert_eq!(text(line.amount), "1.0002");
        assert_eq!(line.raw_amount, dec("1.0001"), "精确值 1.00010 = 1.0001");
        assert_eq!(
            text(line.rounding_diff()),
            "0.0001",
            "口径尾差必须能算出来（对平用，不是差异）"
        );
    }

    #[test]
    fn rounding_diff_is_zero_in_the_drill_example() {
        let line = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("算例 C");
        assert_eq!(line.rounding_diff(), Decimal::ZERO);
    }

    // ---------- 输入域（纯入参校验 ⇒ 400 + 10001，不新增业务码） ----------

    fn assert_invalid(result: std::result::Result<VarianceLine, ApiError>, needle: &str) {
        let err = result.expect_err("负数必须被拒，不静默取绝对值");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert_eq!(err.status, 400);
        assert!(
            err.msg.contains(needle),
            "错误消息要指到具体字段（含「{needle}」），实际：{}",
            err.msg
        );
    }

    #[test]
    fn negative_actual_price_is_rejected() {
        assert_invalid(mat_price_variance(dec("-0.01"), dec("12"), dec("10")), "实际单价");
    }

    #[test]
    fn negative_std_price_is_rejected() {
        assert_invalid(mat_price_variance(dec("12"), dec("-0.01"), dec("10")), "标准单价");
    }

    #[test]
    fn negative_usage_is_rejected() {
        assert_invalid(mat_price_variance(dec("12"), dec("11"), dec("-1")), "实际用量");
    }

    #[test]
    fn non_rate_variance_type_is_rejected() {
        // MAT_QTY 是库层合法值，但不是「在实际量上比两种价」的公式 ⇒ 走这里必然算错
        let err =
            price_variance("MAT_QTY", "MATERIAL", dec("12"), dec("11"), dec("10")).expect_err("量差不能套率差公式");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("MAT_PRICE"), "要说清只算哪几类：{}", err.msg);
    }

    #[test]
    fn unknown_variance_type_is_rejected() {
        let err =
            price_variance("MAT_PRC", "MATERIAL", dec("12"), dec("11"), dec("10")).expect_err("拼错的类型不许静默通过");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("MAT_PRC"));
    }

    #[test]
    fn unknown_element_is_rejected() {
        let err =
            price_variance(VARIANCE_MAT_PRICE, "TAX", dec("12"), dec("11"), dec("10")).expect_err("要素只能是三要素");
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("TAX"));
    }

    #[test]
    fn amount_beyond_money_column_is_rejected() {
        // 12 位整数价 × 6 位整数量 ⇒ 千万亿级，超出 NUMERIC(18,4)（14 位整数）
        let err = mat_price_variance(dec("999999999999.999999"), dec("0"), dec("999999.999999"))
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
            ("12.50", "12.00", "1200"), // 正
            ("11.50", "12.00", "200"),  // 负
            ("12.00", "12.00", "5"),    // 0
            ("12.50", "12.00", "0"),    // 量 0
        ];
        for (ap, sp, aq) in cases {
            let line = mat_price_variance(dec(ap), dec(sp), dec(aq)).expect("合法输入");
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
        for literal in RATE_VARIANCE_TYPES {
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
    fn result_type_is_pinned_to_mat_price_for_the_card_entry_point() {
        // 入口函数不许因为「泛型算数」把类型标签让调用方随便传
        let line = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("算例 C");
        assert_eq!(line.variance_type, "MAT_PRICE");
        assert_eq!(line.element, "MATERIAL");
    }

    #[test]
    fn labels_cover_the_rate_family() {
        assert_eq!(variance_label(VARIANCE_MAT_PRICE), "材料价差");
        assert_eq!(variance_label(VARIANCE_LABOR_RATE), "工资率差异");
    }
}

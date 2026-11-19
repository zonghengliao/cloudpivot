//! 标准成本三差异单测（成本核算 · M5-17）—— **含正负差异**。
//!
//! 本文件不新增任何业务能力：它把 M5-12（材料价差）/ M5-13（材料量差）/
//! M5-14（人工效率差异）三个**已经在生产的纯函数入口**放进同一张标准成本卡里跑，
//! 钉住「三个差异到底怎么算」在跨卡组装时不被哪一张卡悄悄改掉。
//!
//! # 为什么要有这张卡（三张卡各自都有单测了）
//!
//! M5-12/13/14 各自的用例都是**单入口**验证：给两个价、两个量，断言一个金额。
//! 那种用例证明「这个函数按契约工作」，证明不了：
//!
//! * 标准成本卡上的「单位单耗 / 单位工时」被**正确放大**成期间标准量；
//! * 三个差异放在一张表里时，**正负号不会互相串**（有利与不利混在一起时最容易错）；
//! * §4.9.2 的两条恒等式在**整卡**（多物料 + 人工）上仍然精确闭合到分；
//! * M5-11 报表的「要素差异」与 M5-12~14 算出来的差异是**同一个数**。
//!
//! # 三种差异（本卡口径）
//!
//! | 差异 | 入口 | 公式（ARCH-9 §4.9.1） | 标准量从哪来 |
//! |---|---|---|---|
//! | 材料价差 `MAT_PRICE` | [`mat_price_variance`] | `(实际单价 − 标准单价) × 实际用量` | 不吃标准量 |
//! | 材料量差 `MAT_QTY` | [`mat_qty_variance`] | `(实际用量 − 标准用量) × 标准单价` | 单位标准单耗 × 完工数 |
//! | 人工效率差异 `LABOR_EFF` | [`labor_eff_variance`] | `(实际工时 − 标准工时) × 标准工资率` | [`std_hours_of`]（单位标准工时 × 完工数） |
//!
//! 工资率差异 `LABOR_RATE` 与效率差异**同一条恒等式**（`率差 + 效差 = 实际人工 − 标准人工`），
//! 所以本卡通过 [`labor_two_factor`] 一并断言 —— **不是**第四种差异，是效率差必须配对的另一半。
//! 制造费用两差（`OH_SPEND` / `OH_VOLUME`）是「预算相减」的另一种形态，不在这三张卡的算数里，
//! 本卡只在 [`compare_total_beyond_the_three_variances_is_the_overhead_variance`] 里
//! 把它当作「报表总差异 − 三差异」的差额显式指出来。
//!
//! # 符号口径（全卡统一，改之前先看 ARCH-9 §4.9.4）
//!
//! ```text
//! amount > 0  ⇒ 不利（UNFAVORABLE）：超支 / 多用了 / 干慢了
//! amount < 0  ⇒ 有利（FAVORABLE）  ：节约 / 少用了 / 干快了
//! amount = 0  ⇒ 展示「持平」，写库仍落 UNFAVORABLE（ck_biz_cost_var_dir 只有两个值）
//! ```
//!
//! 本卡的每个「正负对照」用例都同时断言**金额符号**与**方向字面值**：
//! 只断言金额会漏掉「金额对了但方向填反」这种会让报表把有利显示成不利的错。
//!
//! # 覆盖矩阵
//!
//! | 组 | 覆盖 | 用例 |
//! |---|---|---|
//! | A 标准卡派生 | 单位单耗/单位工时 → 期间标准量；卡上三要素 → 差异基准 | 1–3 |
//! | B 设计算例 C | 三个差异 + 两条恒等式 + 正负混合 | 4–6 |
//! | C 三态（正/负/零） | 三种差异各自的 不利 / 有利 / 持平 | 7–9 |
//! | D 符号对称 | 实际与标准对调 ⇒ 金额精确反号 | 10 |
//! | E 库层符号闸 | `ck_biz_cost_var_sign` 的方向 ⇔ 符号 | 11 |
//! | F 恒等式网格 | 材料 / 人工各一组网格（含 6 位小数、尾差、有利为主）+ 错公式反证 | 12–15 |
//! | G 多物料正负混排 | 一张卡里三条相反符号的材料行 + 人工 | 16–17 |
//! | H 与 M5-11 对拍 | 报表要素差异 == 三差异之和（正 / 负 / 费用差） | 18–20 |
//! | I 边界 | 完工 0 / 标准量 0 / 标准价 0 / 零差异 / 费率无关 | 21–25 |
//! | J 落表与守卫 | 类型三件套、`biz_cost_variance` 列口径、负入参 400 | 26–29 |
//!
//! 全部用例**不连库**（纯逻辑）：三张卡的算数入口都是纯函数，恒等式不依赖任何取数。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::uuid::Uuid;
use std::str::FromStr as _;

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::cost::cost_labor_eff_variance_dto::{
    LaborTwoFactor, labor_eff_variance, labor_two_factor, std_hours_of,
};
use crate::business::erp::cost::cost_period_item_dto::money;
use crate::business::erp::cost::cost_qty_variance_dto::{
    QtyVarianceLine, VARIANCE_MAT_QTY, mat_qty_variance, variance_label as qty_label,
};
use crate::business::erp::cost::cost_standard_compare_dto::{
    ActualSide, CompareCore, DIRECTION_FAVORABLE, DIRECTION_UNFAVORABLE, ELEMENT_LABOR, ELEMENT_MATERIAL, ELEMENTS,
    STATE_BOTH, StandardSide, calc_of,
};
use crate::business::erp::cost::cost_standard_dto::{STATUS_ACTIVE, StandardItemSpec, StandardSpec};
use crate::business::erp::cost::cost_variance_alloc_dto::VARIANCE_TYPES_ALL;
use crate::business::erp::cost::cost_variance_dto::{
    VARIANCE_LABOR_RATE, VARIANCE_MAT_PRICE, VarianceLine, mat_price_variance, money_text, variance_label as rate_label,
};

// ==================== 夹具 ====================

/// 差异表「用量」列的标度（`biz_cost_variance.actual_qty / std_qty` = `NUMERIC(18,6)`）。
const QTY_SCALE: u32 = 6;

/// 与 PG `round(numeric, int)` 同口径（半数远离零）。
///
/// 与 `cost_labor_eff_variance_dto` 的取整策略一致 —— 测试若用 banker's rounding，
/// 会在 `.00005` 这类边界上算出生产算不出来的数，那是假红。
const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).unwrap_or_else(|_| panic!("数值字面量无法解析：{text}"))
}

fn uid(n: u128) -> Uuid {
    Uuid::from_u128(0x1700_0000_0000_0000_0000_0000_0000_0000 + n)
}

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap_or_else(|| panic!("非法日期 {year}-{month}-{day}"))
}

/// 4 位金额文本（`money_text` 是生产实现，测试不另写格式化）。
fn text(value: Decimal) -> String {
    money_text(value)
}

fn assert_money(value: Decimal, expected: &str, what: &str) {
    assert_eq!(text(value), expected, "{what}（实得 {value}）");
}

/// 标准成本卡上的一行材料（**单位口径**：每件成品耗多少、单价多少）。
#[derive(Debug, Clone)]
struct Component {
    material_id: Uuid,
    unit_std_qty: Decimal,
    std_price: Decimal,
}

/// 实际发生的一行材料（**期间口径**：实际单价与实际用量）。
#[derive(Debug, Clone)]
struct ActualComponent {
    material_id: Uuid,
    actual_price: Decimal,
    actual_qty: Decimal,
}

/// 一张标准成本卡 + 一个完工数量。
///
/// 卡上的数是「每件」的，差异要的是「本期」的 —— 中间那一步（乘完工数、按列口径取整）
/// 正是本卡 A 组要钉的东西。
struct StandardCard {
    finished_id: Uuid,
    completed_qty: Decimal,
    components: Vec<Component>,
    unit_std_hours: Decimal,
    std_rate: Decimal,
    std_overhead_per_unit: Decimal,
}

impl StandardCard {
    /// 材料的**标准用量** = 单位标准单耗 × 完工数，按差异表列口径取 6 位。
    ///
    /// 生产侧没有材料标准量的公共算法（M5-13 明确「标准量由调用方传」），所以取整策略
    /// 落在调用方；这里与 [`std_hours_of`] 用同一口径（18,6、半数远离零），
    /// 免得「材料量一套取整、人工量另一套」在恒等式上分叉。
    fn std_qty(&self, component: &Component) -> Decimal {
        (component.unit_std_qty * self.completed_qty).round_dp_with_strategy(QTY_SCALE, ROUNDING)
    }

    /// 标准工时（唯一算法在 M5-14，本文件不重写）。
    fn std_hours(&self) -> Decimal {
        std_hours_of(self.unit_std_hours, self.completed_qty).expect("标准工时")
    }

    /// 归一化标准卡（M5-08 的 `StandardSpec`）—— 三要素金额由它算，测试不另抄一份。
    fn spec(&self) -> StandardSpec {
        StandardSpec {
            material_id: self.finished_id,
            effective_from: day(2026, 1, 1),
            effective_to: None,
            std_labor_hours: self.unit_std_hours,
            std_labor_rate: self.std_rate,
            std_overhead_cost: self.std_overhead_per_unit,
            status: STATUS_ACTIVE,
            remark: None,
            items: self
                .components
                .iter()
                .map(|component| StandardItemSpec {
                    component_material_id: component.material_id,
                    std_qty: component.unit_std_qty,
                    std_price: component.std_price,
                })
                .collect(),
        }
    }

    /// 三个差异 + 人工率差：**全部走生产入口**，测试不实现任何公式。
    fn variances(&self, actual: &Actuals) -> ThreeVariances {
        assert_eq!(
            self.components.len(),
            actual.components.len(),
            "夹具不匹配：标准卡与实际的物料行数不同"
        );
        let price = self
            .components
            .iter()
            .zip(&actual.components)
            .map(|(component, actual)| {
                assert_eq!(component.material_id, actual.material_id, "夹具不匹配：物料对不上");
                mat_price_variance(actual.actual_price, component.std_price, actual.actual_qty).expect("价差")
            })
            .collect();
        let qty = self
            .components
            .iter()
            .zip(&actual.components)
            .map(|(component, actual)| {
                mat_qty_variance(actual.actual_qty, self.std_qty(component), component.std_price).expect("量差")
            })
            .collect();
        let labor = labor_two_factor(actual.actual_rate, self.std_rate, actual.actual_hours, self.std_hours())
            .expect("人工两因素");
        ThreeVariances { price, qty, labor }
    }

    fn standard_side(&self) -> StandardSide {
        let unit = self.spec().amounts();
        StandardSide {
            standard_id: uid(900),
            version_no: 1,
            effective_from: day(2026, 1, 1),
            effective_to: None,
            std_material_cost: unit.material,
            std_labor_cost: unit.labor,
            std_overhead_cost: unit.overhead,
            std_total_cost: unit.total(),
            std_labor_hours: self.unit_std_hours,
            std_labor_rate: self.std_rate,
            published_at: None,
            switch_in_period: false,
        }
    }

    fn actual_side(&self, actual: &Actuals, variances: &ThreeVariances) -> ActualSide {
        let material = variances.actual_material();
        let labor = variances.actual_labor();
        let book_amount = material + labor + actual.overhead_amount;
        ActualSide {
            object_count: 1,
            completed_qty: self.completed_qty,
            book_amount,
            amount_material: material,
            amount_labor: labor,
            amount_overhead: actual.overhead_amount,
            period_input: book_amount,
            wip_end_cost: Decimal::ZERO,
        }
    }

    /// M5-11 报表的一行取数（与 `cost_standard_compare_model` 同形状）。
    fn compare_core(&self, actual: &Actuals) -> CompareCore {
        let variances = self.variances(actual);
        CompareCore {
            material_id: self.finished_id,
            material_code: Some("FG-1700".to_string()),
            material_name: Some("测试成品".to_string()),
            material_spec: None,
            unpublished_versions: 0,
            standard: Some(self.standard_side()),
            actual: Some(self.actual_side(actual, &variances)),
        }
    }
}

/// 一个期间的实际发生额。
struct Actuals {
    components: Vec<ActualComponent>,
    actual_hours: Decimal,
    actual_rate: Decimal,
    /// 实际制造费用 —— 三差异**不吃**它，只在 M5-11 对拍时用来显出差额。
    overhead_amount: Decimal,
}

impl Actuals {
    fn of(components: Vec<ActualComponent>, hours: &str, rate: &str) -> Self {
        Self {
            components,
            actual_hours: dec(hours),
            actual_rate: dec(rate),
            overhead_amount: Decimal::ZERO,
        }
    }

    fn with_overhead(mut self, overhead: &str) -> Self {
        self.overhead_amount = dec(overhead);
        self
    }
}

/// 三个差异 + 人工率差（人工那一半由 [`labor_two_factor`] 保证同参）。
struct ThreeVariances {
    price: Vec<VarianceLine>,
    qty: Vec<QtyVarianceLine>,
    labor: LaborTwoFactor,
}

impl ThreeVariances {
    /// 材料差异 = Σ 价差 + Σ 量差（4 位，正 = 不利）。
    fn material_amount(&self) -> Decimal {
        self.price.iter().map(|line| line.amount).sum::<Decimal>()
            + self.qty.iter().map(|line| line.amount).sum::<Decimal>()
    }

    /// 人工差异 = 率差 + 效差（4 位，正 = 不利）。
    fn labor_amount(&self) -> Decimal {
        self.labor.total()
    }

    /// 三差异合计。
    fn total(&self) -> Decimal {
        self.material_amount() + self.labor_amount()
    }

    fn actual_material(&self) -> Decimal {
        self.price.iter().map(|line| line.actual_amount).sum()
    }

    fn std_material(&self) -> Decimal {
        self.qty.iter().map(|line| line.std_amount).sum()
    }

    fn actual_labor(&self) -> Decimal {
        self.labor.actual_amount()
    }

    fn std_labor(&self) -> Decimal {
        self.labor.std_amount()
    }

    /// 材料恒等式残差 = (价差 + 量差) − (实际材料 − 标准材料)，必须为 0。
    fn material_residual(&self) -> Decimal {
        self.material_amount() - (self.actual_material() - self.std_material())
    }

    /// 人工恒等式残差（M5-14 已实现，这里只取用）。
    fn labor_residual(&self) -> Decimal {
        self.labor.residual()
    }

    fn closes_exactly(&self) -> bool {
        self.material_residual().is_zero() && self.labor_residual().is_zero()
    }

    /// 四个差异行（价差*、量差*、率差、效差）的**落表视角**。
    fn rows(&self) -> Vec<VarianceRow<'_>> {
        let mut rows: Vec<VarianceRow<'_>> = self.price.iter().map(VarianceRow::of_price).collect();
        rows.extend(self.qty.iter().map(VarianceRow::of_qty));
        rows.push(VarianceRow::of_price(&self.labor.rate));
        rows.push(VarianceRow::of_qty(&self.labor.eff));
        rows
    }
}

/// `biz_cost_variance` 一行里**算数相关**的列（写库方按它填）。
struct VarianceRow<'a> {
    variance_type: &'a str,
    element: &'a str,
    amount: Decimal,
    direction: &'a str,
    label: &'a str,
    std_qty: Option<Decimal>,
}

impl<'a> VarianceRow<'a> {
    fn of_price(line: &'a VarianceLine) -> Self {
        Self {
            variance_type: line.variance_type,
            element: line.element,
            amount: line.amount,
            direction: line.direction(),
            label: line.direction_label(),
            std_qty: line.std_qty,
        }
    }

    fn of_qty(line: &'a QtyVarianceLine) -> Self {
        Self {
            variance_type: line.variance_type,
            element: line.element,
            amount: line.amount,
            direction: line.direction(),
            label: line.direction_label(),
            std_qty: Some(line.std_qty),
        }
    }
}

// ---------- 设计算例 C 的夹具（ARCH-9 §4.9.2） ----------

/// 算例 C 的标准卡：800 件 × 1.1 件/件 × 12.00；800 件 × 0.10 h × 50.00。
fn drill_card() -> StandardCard {
    StandardCard {
        finished_id: uid(1),
        completed_qty: dec("800"),
        components: vec![Component {
            material_id: uid(2),
            unit_std_qty: dec("1.1"),
            std_price: dec("12.00"),
        }],
        unit_std_hours: dec("0.10"),
        std_rate: dec("50.00"),
        std_overhead_per_unit: dec("2.00"),
    }
}

/// 算例 C 的实际：1200 × 12.50；90 h × 48.00。
fn drill_actuals() -> Actuals {
    Actuals::of(
        vec![ActualComponent {
            material_id: uid(2),
            actual_price: dec("12.50"),
            actual_qty: dec("1200"),
        }],
        "90",
        "48",
    )
    .with_overhead("1600.00")
}

/// 单物料单卡（量、价、工时、费率、完工数全部字面给），
/// 用于边界与符号用例 —— 避免每处都手抄一个 `StandardCard { .. }`。
fn one_component_card(completed: &str, unit_qty: &str, std_price: &str, unit_hours: &str, rate: &str) -> StandardCard {
    StandardCard {
        finished_id: uid(11),
        completed_qty: dec(completed),
        components: vec![Component {
            material_id: uid(12),
            unit_std_qty: dec(unit_qty),
            std_price: dec(std_price),
        }],
        unit_std_hours: dec(unit_hours),
        std_rate: dec(rate),
        std_overhead_per_unit: Decimal::ZERO,
    }
}

// ==================== A. 标准卡派生 ====================

#[test]
fn standard_card_derives_the_quantities_the_variances_consume() {
    let card = drill_card();
    assert_eq!(card.std_qty(&card.components[0]), dec("880"), "1.1 件/件 × 800 件");
    assert_eq!(card.std_hours(), dec("80"), "0.10 h/件 × 800 件");

    let variances = card.variances(&drill_actuals());
    assert_eq!(
        variances.qty[0].std_qty,
        card.std_qty(&card.components[0]),
        "量差吃到的标准量必须就是卡上算出来的那个数"
    );
    assert_eq!(
        variances.labor.eff.std_qty,
        card.std_hours(),
        "效率差吃到的标准工时必须走 std_hours_of（唯一算法）"
    );
}

#[test]
fn standard_card_unit_cost_times_completed_equals_the_variance_baseline() {
    let card = drill_card();
    let unit = card.spec().amounts();
    assert_eq!(unit.material, dec("13.2"), "单位标准材料 = 1.1 × 12");
    assert_eq!(unit.labor, dec("5"), "单位标准人工 = 0.10 × 50");

    let variances = card.variances(&drill_actuals());
    let card_material = money(unit.material * card.completed_qty);
    let card_labor = money(unit.labor * card.completed_qty);
    assert_money(card_material, "10560.0000", "标准卡材料 × 完工数");
    assert_money(card_labor, "4000.0000", "标准卡人工 × 完工数");
    assert_eq!(
        card_material,
        variances.std_material(),
        "差异的「标准材料成本」必须与标准卡算出来的同数（两条路径不许各算一份）"
    );
    assert_eq!(
        card_labor,
        variances.std_labor(),
        "差异的「标准人工成本」必须与标准卡算出来的同数"
    );
}

#[test]
fn std_hours_are_rounded_to_the_column_scale_before_they_are_priced() {
    // 单位工时 0.333333 h/件 × 3 件 = 0.999999 h（正好 6 位，落表不会截断）
    let card = one_component_card("3", "1", "10", "0.333333", "60");
    let hours = card.std_hours();
    assert_eq!(hours, dec("0.999999"));

    let variances = card.variances(&Actuals::of(
        vec![ActualComponent {
            material_id: uid(12),
            actual_price: dec("10"),
            actual_qty: dec("1"),
        }],
        "1.2",
        "60",
    ));
    let eff = &variances.labor.eff;
    assert_eq!(eff.std_qty, hours, "算金额用的工时 == 落表的工时");
    assert_eq!(
        eff.std_amount,
        money(dec("60") * hours),
        "标准额必须用落表那个工时算，不许用未取整的 0.9999990…"
    );
    assert_money(eff.std_amount, "59.9999", "0.999999 × 60 = 59.99994 ⇒ 4 位 59.9999");
}

// ==================== B. 设计算例 C（ARCH-9 §4.9.2） ====================

#[test]
fn drill_example_c_reproduces_the_three_variances() {
    let variances = drill_card().variances(&drill_actuals());

    assert_money(variances.price[0].amount, "600.0000", "价差 (12.50 − 12.00) × 1200");
    assert_money(variances.qty[0].amount, "3840.0000", "量差 (1200 − 880) × 12.00");
    assert_money(variances.labor.eff.amount, "500.0000", "效率差 (90 − 80) × 50.00");

    assert_eq!(variances.price[0].variance_type, VARIANCE_MAT_PRICE);
    assert_eq!(variances.price[0].element, ELEMENT_MATERIAL);
    assert_eq!(variances.qty[0].variance_type, VARIANCE_MAT_QTY);
    assert_eq!(variances.qty[0].element, ELEMENT_MATERIAL);
    assert_eq!(variances.labor.eff.variance_type, "LABOR_EFF");
    assert_eq!(variances.labor.eff.element, ELEMENT_LABOR);
    assert_eq!(variances.labor.rate.variance_type, VARIANCE_LABOR_RATE);
    assert_eq!(variances.labor.rate.element, ELEMENT_LABOR);
}

#[test]
fn drill_example_c_closes_both_two_factor_identities() {
    let variances = drill_card().variances(&drill_actuals());

    assert_money(variances.actual_material(), "15000.0000", "实际 1200 × 12.50");
    assert_money(variances.std_material(), "10560.0000", "标准 880 × 12.00");
    assert_money(variances.material_amount(), "4440.0000", "价差 600 + 量差 3840");
    assert_eq!(
        variances.material_amount(),
        variances.actual_material() - variances.std_material(),
        "§4.9.2 材料恒等式"
    );

    assert_money(variances.actual_labor(), "4320.0000", "实际 90 h × 48.00");
    assert_money(variances.std_labor(), "4000.0000", "标准 80 h × 50.00");
    assert_money(variances.labor_amount(), "320.0000", "率差 −180 + 效差 500");
    assert_money(variances.labor.rate.amount, "-180.0000", "工资率差 (48 − 50) × 90");
    assert_eq!(
        variances.labor_amount(),
        variances.actual_labor() - variances.std_labor(),
        "§4.9.2 人工恒等式"
    );

    assert_money(variances.total(), "4760.0000", "三差异合计");
    assert!(variances.closes_exactly(), "两条恒等式的残差都必须为 0");
    assert_eq!(variances.material_residual(), Decimal::ZERO);
    assert_eq!(variances.labor_residual(), Decimal::ZERO);
}

#[test]
fn drill_example_c_mixes_unfavorable_and_favorable_lines() {
    let variances = drill_card().variances(&drill_actuals());

    assert_eq!(variances.price[0].direction(), DIRECTION_UNFAVORABLE, "涨价 = 不利");
    assert_eq!(variances.price[0].direction_label(), "不利");
    assert_eq!(variances.qty[0].direction(), DIRECTION_UNFAVORABLE, "超用 = 不利");
    assert_eq!(variances.labor.eff.direction(), DIRECTION_UNFAVORABLE, "干慢 = 不利");
    assert_eq!(variances.labor.rate.direction(), DIRECTION_FAVORABLE, "用工便宜 = 有利");
    assert_eq!(variances.labor.rate.direction_label(), "有利");
    assert_eq!(
        variances.labor.direction_label(),
        "不利",
        "率差 −180 与效差 +500 相抵后 +320，合计是不利"
    );
}

// ==================== C. 三态（正 / 负 / 零） ====================

#[test]
fn material_price_variance_covers_unfavorable_favorable_and_even() {
    let up = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("涨价");
    let down = mat_price_variance(dec("11.50"), dec("12.00"), dec("1200")).expect("降价");
    let even = mat_price_variance(dec("12.00"), dec("12.00"), dec("1200")).expect("同价");

    assert_money(up.amount, "600.0000", "(12.50 − 12.00) × 1200");
    assert_money(down.amount, "-600.0000", "(11.50 − 12.00) × 1200");
    assert!(even.is_zero(), "同价 ⇒ 价差 0");

    assert_eq!(up.direction(), DIRECTION_UNFAVORABLE);
    assert_eq!(down.direction(), DIRECTION_FAVORABLE);
    assert_eq!(up.direction_label(), "不利");
    assert_eq!(down.direction_label(), "有利");
    assert_eq!(even.direction_label(), "持平");
    assert_eq!(even.direction(), DIRECTION_UNFAVORABLE, "0 也要能写库（库层只有两值）");
}

#[test]
fn material_qty_variance_covers_unfavorable_favorable_and_even() {
    let over = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("超用");
    let under = mat_qty_variance(dec("700"), dec("880"), dec("12.00")).expect("少用");
    let even = mat_qty_variance(dec("880"), dec("880"), dec("12.00")).expect("刚好");

    assert_money(over.amount, "3840.0000", "(1200 − 880) × 12");
    assert_money(under.amount, "-2160.0000", "(700 − 880) × 12");
    assert!(even.is_zero());

    assert_eq!(over.direction(), DIRECTION_UNFAVORABLE);
    assert_eq!(under.direction(), DIRECTION_FAVORABLE);
    assert_eq!(over.qty_delta(), dec("320"), "报表上「多用了 320 件」显示这个");
    assert_eq!(under.qty_delta(), dec("-180"), "少用 180 件");
    assert_eq!(even.qty_delta(), Decimal::ZERO);
}

#[test]
fn labor_efficiency_variance_covers_unfavorable_favorable_and_even() {
    let slow = labor_eff_variance(dec("90"), dec("80"), dec("50")).expect("干慢");
    let fast = labor_eff_variance(dec("70"), dec("80"), dec("50")).expect("干快");
    let even = labor_eff_variance(dec("80"), dec("80"), dec("50")).expect("刚好");

    assert_money(slow.amount, "500.0000", "(90 − 80) × 50");
    assert_money(fast.amount, "-500.0000", "(70 − 80) × 50");
    assert!(even.is_zero());

    assert_eq!(slow.direction(), DIRECTION_UNFAVORABLE);
    assert_eq!(fast.direction(), DIRECTION_FAVORABLE);
    assert_eq!(slow.qty_delta(), dec("10"), "多干 10 小时");
    assert_eq!(fast.direction_label(), "有利");
}

// ==================== D. 符号对称 ====================

#[test]
fn swapping_actual_and_standard_flips_each_sign_exactly() {
    // 三个入口都必须满足：把实际与标准对调，金额精确反号（不能有一厘漂移）
    let price = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("价差");
    let price_swapped = mat_price_variance(dec("12.00"), dec("12.50"), dec("1200")).expect("价差（对调）");
    assert_eq!(price_swapped.amount, -price.amount);
    assert_money(price.amount + price_swapped.amount, "0.0000", "对调后两行相消");

    let qty = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("量差");
    let qty_swapped = mat_qty_variance(dec("880"), dec("1200"), dec("12.00")).expect("量差（对调）");
    assert_eq!(qty_swapped.amount, -qty.amount);

    let eff = labor_eff_variance(dec("90"), dec("80"), dec("50")).expect("效率差");
    let eff_swapped = labor_eff_variance(dec("80"), dec("90"), dec("50")).expect("效率差（对调）");
    assert_eq!(eff_swapped.amount, -eff.amount);

    for (original, swapped) in [
        (price.amount, price_swapped.amount),
        (qty.amount, qty_swapped.amount),
        (eff.amount, eff_swapped.amount),
    ] {
        assert!(
            (original.is_sign_negative() && swapped.is_sign_positive())
                || (original.is_sign_positive() && swapped.is_sign_negative()),
            "对调后符号必须相反：{original} vs {swapped}"
        );
    }
}

// ==================== E. 库层符号闸 ====================

#[test]
fn direction_mapping_always_satisfies_the_db_sign_check() {
    // ck_biz_cost_var_sign：UNFAVORABLE ⇒ amount >= 0；FAVORABLE ⇒ amount <= 0。
    // 拿一整卡（含一条有利的率差）逐行验，比只挑三个金额更难被「方向填反」骗过。
    let variances = drill_card().variances(&drill_actuals());
    let rows = variances.rows();
    assert_eq!(rows.len(), 4, "一物料 + 人工两因素 = 4 行");

    for row in &rows {
        match row.direction {
            DIRECTION_UNFAVORABLE => assert!(
                row.amount >= Decimal::ZERO,
                "{} 落 UNFAVORABLE 但金额是负的（{}）",
                row.variance_type,
                row.amount
            ),
            DIRECTION_FAVORABLE => assert!(
                row.amount <= Decimal::ZERO,
                "{} 落 FAVORABLE 但金额是正的（{}）",
                row.variance_type,
                row.amount
            ),
            other => panic!("库层方向只有两个值，得到 {other}"),
        }
    }
    assert!(
        rows.iter().any(|row| row.direction == DIRECTION_FAVORABLE),
        "这批数据里必须真的有一条有利行，否则本用例没测到负号那一侧"
    );
}

// ==================== F. 恒等式网格 ====================

#[derive(Debug, Clone, Copy)]
struct MaterialCase {
    std_price: &'static str,
    actual_price: &'static str,
    actual_qty: &'static str,
    std_qty: &'static str,
}

/// 材料网格：涨价 / 降价 / 只动量 / 只动价 / 6 位价 / 四位金额取舍不一致 / 极小量 / 极大量。
const MATERIAL_CASES: [MaterialCase; 9] = [
    MaterialCase {
        std_price: "12.00",
        actual_price: "12.50",
        actual_qty: "1200",
        std_qty: "880",
    },
    MaterialCase {
        std_price: "12.00",
        actual_price: "11.50",
        actual_qty: "700",
        std_qty: "880",
    },
    MaterialCase {
        std_price: "12.00",
        actual_price: "12.00",
        actual_qty: "1200",
        std_qty: "880",
    },
    MaterialCase {
        std_price: "12.00",
        actual_price: "12.50",
        actual_qty: "880",
        std_qty: "880",
    },
    MaterialCase {
        std_price: "12.123456",
        actual_price: "12.123456",
        actual_qty: "7.5",
        std_qty: "5",
    },
    MaterialCase {
        std_price: "1.00005",
        actual_price: "2.00015",
        actual_qty: "1",
        std_qty: "1",
    },
    MaterialCase {
        std_price: "0.000001",
        actual_price: "0.000002",
        actual_qty: "3",
        std_qty: "2",
    },
    MaterialCase {
        std_price: "99.999999",
        actual_price: "0.000001",
        actual_qty: "1.5",
        std_qty: "2.5",
    },
    MaterialCase {
        std_price: "12",
        actual_price: "12.000001",
        actual_qty: "0.000001",
        std_qty: "0",
    },
];

#[test]
fn material_identity_holds_across_a_sign_grid() {
    for case in MATERIAL_CASES {
        let price = mat_price_variance(dec(case.actual_price), dec(case.std_price), dec(case.actual_qty))
            .unwrap_or_else(|err| panic!("价差算不出来：{case:?} / {}", err.msg));
        let qty = mat_qty_variance(dec(case.actual_qty), dec(case.std_qty), dec(case.std_price))
            .unwrap_or_else(|err| panic!("量差算不出来：{case:?} / {}", err.msg));

        assert_eq!(
            price.std_amount_at_actual_qty, qty.std_amount_at_actual_qty,
            "{case:?}：价差右端与量差左端必须是同一个分水岭，否则恒等式必破"
        );
        assert_eq!(
            price.amount + qty.amount,
            price.actual_amount - qty.std_amount,
            "{case:?}：价差 + 量差 ≠ 实际 − 标准"
        );
        assert_eq!(
            price.actual_amount,
            money(dec(case.actual_price) * dec(case.actual_qty)),
            "{case:?}"
        );
        assert_eq!(
            qty.std_amount,
            money(dec(case.std_price) * dec(case.std_qty)),
            "{case:?}"
        );
        assert!(
            price.amount.scale() <= 4,
            "{case:?}：价差带 {} 位小数",
            price.amount.scale()
        );
        assert!(
            qty.amount.scale() <= 4,
            "{case:?}：量差带 {} 位小数",
            qty.amount.scale()
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct LaborCase {
    std_rate: &'static str,
    actual_rate: &'static str,
    actual_hours: &'static str,
    std_hours: &'static str,
}

/// 人工网格：用工贵 / 用工便宜 / 6 位费率 / 只动工时 / 零工时 / 微量差。
const LABOR_CASES: [LaborCase; 8] = [
    LaborCase {
        std_rate: "50.00",
        actual_rate: "48.00",
        actual_hours: "90",
        std_hours: "80",
    },
    LaborCase {
        std_rate: "50.00",
        actual_rate: "52.00",
        actual_hours: "72",
        std_hours: "80",
    },
    LaborCase {
        std_rate: "60.000001",
        actual_rate: "60.000002",
        actual_hours: "3.5",
        std_hours: "3",
    },
    LaborCase {
        std_rate: "0.50",
        actual_rate: "0.25",
        actual_hours: "8",
        std_hours: "10",
    },
    LaborCase {
        std_rate: "50.00",
        actual_rate: "50.00",
        actual_hours: "80",
        std_hours: "80",
    },
    LaborCase {
        std_rate: "50.00",
        actual_rate: "49.999999",
        actual_hours: "1",
        std_hours: "1",
    },
    LaborCase {
        std_rate: "100.00",
        actual_rate: "100.00",
        actual_hours: "0",
        std_hours: "5",
    },
    LaborCase {
        std_rate: "33.333333",
        actual_rate: "33.333334",
        actual_hours: "6",
        std_hours: "4",
    },
];

#[test]
fn labor_identity_holds_across_a_sign_grid() {
    for case in LABOR_CASES {
        let pair = labor_two_factor(
            dec(case.actual_rate),
            dec(case.std_rate),
            dec(case.actual_hours),
            dec(case.std_hours),
        )
        .unwrap_or_else(|err| panic!("人工两因素算不出来：{case:?} / {}", err.msg));

        assert!(
            pair.closes_exactly(),
            "{case:?}：率差 + 效差 ≠ 实际人工 − 标准人工（残差 {}）",
            pair.residual()
        );
        assert_eq!(
            pair.rate.std_amount_at_actual_qty, pair.eff.std_amount_at_actual_qty,
            "{case:?}：率差右端与效差左端必须是同一个分水岭"
        );
        assert_eq!(
            pair.rate.actual_amount,
            money(dec(case.actual_rate) * dec(case.actual_hours))
        );
        assert_eq!(pair.eff.std_amount, money(dec(case.std_rate) * dec(case.std_hours)));
        assert!(pair.rate.amount.scale() <= 4, "{case:?}");
        assert!(pair.eff.amount.scale() <= 4, "{case:?}");
        assert_eq!(pair.total(), pair.actual_amount() - pair.std_amount(), "{case:?}");
    }
}

#[test]
fn identity_closes_when_only_one_factor_moves() {
    // 只动价：实际量 == 标准量 ⇒ 量差 0，差额全在价差
    let only_price = mat_price_variance(dec("12.50"), dec("12.00"), dec("880")).expect("只动价");
    let no_qty_move = mat_qty_variance(dec("880"), dec("880"), dec("12.00")).expect("量差 0");
    assert!(no_qty_move.is_zero(), "量没动，量差必须是 0");
    assert_money(only_price.amount, "440.0000", "(12.50 − 12.00) × 880");
    assert_eq!(
        only_price.amount + no_qty_move.amount,
        only_price.actual_amount - no_qty_move.std_amount
    );

    // 只动量：两种价相同 ⇒ 价差 0，差额全在量差
    let only_qty = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("只动量");
    let no_price_move = mat_price_variance(dec("12.00"), dec("12.00"), dec("1200")).expect("价差 0");
    assert!(no_price_move.is_zero());
    assert_money(only_qty.amount, "3840.0000", "(1200 − 880) × 12");
    assert_eq!(
        no_price_move.amount + only_qty.amount,
        no_price_move.actual_amount - only_qty.std_amount
    );

    // 只动工时：费率相同 ⇒ 率差 0，差额全在效差
    let only_hours = labor_two_factor(dec("50"), dec("50"), dec("90"), dec("80")).expect("只动工时");
    assert!(only_hours.rate.amount.is_zero(), "费率没动，率差必须是 0");
    assert_money(only_hours.eff.amount, "500.0000", "(90 − 80) × 50");
    assert_money(only_hours.total(), "500.0000", "差额全在效差");
    assert!(only_hours.closes_exactly());
}

#[test]
fn the_identity_rejects_the_classic_wrong_formulas() {
    // §4.9.2 点名的两种常见错法。本用例证明「恒等式断言」是有牙的：
    // 真有人把公式改成这两支，上面那些恒等式用例会立刻红，而不是只差一厘地绿着。
    let price = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("价差");
    let right_qty = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("量差（对）");
    // 错法①：量差用实际单价乘
    let wrong_qty = money(dec("12.50") * dec("1200")) - money(dec("12.50") * dec("880"));
    assert_eq!(
        price.amount + right_qty.amount,
        price.actual_amount - right_qty.std_amount,
        "正确公式闭合"
    );
    assert_ne!(
        price.amount + wrong_qty,
        price.actual_amount - right_qty.std_amount,
        "错法①（量差用实际价）必须让恒等式破 —— 否则这条断言抓不到公式错"
    );

    // 错法②：效率差用实际工资率乘
    let pair = labor_two_factor(dec("48"), dec("50"), dec("90"), dec("80")).expect("人工两因素");
    let wrong_eff = money(dec("48") * dec("90")) - money(dec("48") * dec("80"));
    assert!(pair.closes_exactly(), "正确公式闭合");
    assert_ne!(
        pair.rate.amount + wrong_eff,
        pair.actual_amount() - pair.std_amount(),
        "错法②（效差用实际费率）必须让恒等式破"
    );
}

// ==================== G. 多物料正负混排 ====================
#[test]
fn mixed_material_lines_keep_their_own_signs_when_summed() {
    // 三条材料行：一路涨 + 超用（不利），二路跌 + 少用（有利），三路涨 + 少用（净有利）
    let card = StandardCard {
        finished_id: uid(20),
        completed_qty: dec("100"),
        components: vec![
            Component {
                material_id: uid(21),
                unit_std_qty: dec("2"),
                std_price: dec("10.00"),
            },
            Component {
                material_id: uid(22),
                unit_std_qty: dec("1.5"),
                std_price: dec("20.00"),
            },
            Component {
                material_id: uid(23),
                unit_std_qty: dec("0.5"),
                std_price: dec("8.00"),
            },
        ],
        unit_std_hours: dec("0.5"),
        std_rate: dec("40.00"),
        std_overhead_per_unit: Decimal::ZERO,
    };
    let actual = Actuals::of(
        vec![
            ActualComponent {
                material_id: uid(21),
                actual_price: dec("11.00"),
                actual_qty: dec("220"),
            },
            ActualComponent {
                material_id: uid(22),
                actual_price: dec("18.00"),
                actual_qty: dec("140"),
            },
            ActualComponent {
                material_id: uid(23),
                actual_price: dec("9.00"),
                actual_qty: dec("40"),
            },
        ],
        "55",
        "42.00",
    );

    let variances = card.variances(&actual);

    // 每行的符号必须各归各的：混合符号最容易在汇总时被「取绝对值」吃掉
    assert_money(variances.price[0].amount, "220.0000", "一路：(11 − 10) × 220 不利");
    assert_money(variances.qty[0].amount, "200.0000", "一路：(220 − 200) × 10 超用");
    assert_money(variances.price[1].amount, "-280.0000", "二路：(18 − 20) × 140 有利");
    assert_money(variances.qty[1].amount, "-200.0000", "二路：(140 − 150) × 20 少用");
    assert_money(variances.price[2].amount, "40.0000", "三路：(9 − 8) × 40");
    assert_money(variances.qty[2].amount, "-80.0000", "三路：(40 − 50) × 8 少用");

    assert_eq!(variances.price[0].direction(), DIRECTION_UNFAVORABLE);
    assert_eq!(variances.price[1].direction(), DIRECTION_FAVORABLE);
    assert_eq!(variances.qty[1].direction(), DIRECTION_FAVORABLE);
    assert_eq!(variances.qty[2].direction(), DIRECTION_FAVORABLE);

    // 汇总：正负相抵后材料净有利 100
    assert_money(variances.actual_material(), "5300.0000", "2420 + 2520 + 360");
    assert_money(variances.std_material(), "5400.0000", "2000 + 3000 + 400");
    assert_money(variances.material_amount(), "-100.0000", "三行差异正负相抵");
    assert!(variances.closes_exactly(), "多物料逐行取整后恒等式仍须精确闭合");
}

#[test]
fn realistic_month_with_mixed_signs_keeps_every_line_straight() {
    // 一期真实形态：材料一路跌价+少用（有利），二路涨价+超用（不利），人工用工便宜但干慢
    let card = StandardCard {
        finished_id: uid(30),
        completed_qty: dec("500"),
        components: vec![
            Component {
                material_id: uid(31),
                unit_std_qty: dec("1.2"),
                std_price: dec("15.00"),
            },
            Component {
                material_id: uid(32),
                unit_std_qty: dec("0.8"),
                std_price: dec("25.00"),
            },
        ],
        unit_std_hours: dec("0.2"),
        std_rate: dec("60.00"),
        std_overhead_per_unit: Decimal::ZERO,
    };
    let actual = Actuals::of(
        vec![
            ActualComponent {
                material_id: uid(31),
                actual_price: dec("14.00"),
                actual_qty: dec("580"),
            },
            ActualComponent {
                material_id: uid(32),
                actual_price: dec("26.00"),
                actual_qty: dec("420"),
            },
        ],
        "105",
        "58.00",
    );

    let variances = card.variances(&actual);
    assert_money(variances.price[0].amount, "-580.0000", "一路跌价有利");
    assert_money(variances.qty[0].amount, "-300.0000", "一路少用有利");
    assert_money(variances.price[1].amount, "420.0000", "二路涨价不利");
    assert_money(variances.qty[1].amount, "500.0000", "二路超用不利");
    assert_money(variances.material_amount(), "40.0000", "−580 − 300 + 420 + 500");
    assert_money(variances.labor.rate.amount, "-210.0000", "用工便宜：(58 − 60) × 105");
    assert_money(variances.labor.eff.amount, "300.0000", "干慢：(105 − 100) × 60");
    assert_money(variances.total(), "130.0000", "40 + 90");
    assert_eq!(
        variances.total(),
        (variances.actual_material() + variances.actual_labor()) - (variances.std_material() + variances.std_labor()),
        "整卡恒等式：三差异合计 = 实际（料+工）− 标准（料+工）"
    );
    assert!(variances.closes_exactly());
}

// ==================== H. 与 M5-11 报表对拍 ====================

#[test]
fn compare_report_row_matches_the_material_and_labor_variances() {
    let card = drill_card();
    let actual = drill_actuals();
    let variances = card.variances(&actual);
    let core = card.compare_core(&actual);
    assert_eq!(core.state(), STATE_BOTH);

    let row = calc_of(&core);
    assert_eq!(
        row.std_amount[0],
        variances.std_material(),
        "报表标准材料额 = 量差用的标准额"
    );
    assert_eq!(
        row.act_amount[0],
        variances.actual_material(),
        "报表实际材料额 = 价差用的实际额"
    );
    assert_eq!(
        row.std_amount[1],
        variances.std_labor(),
        "报表标准人工额 = 效差用的标准额"
    );
    assert_eq!(
        row.act_amount[1],
        variances.actual_labor(),
        "报表实际人工额 = 率差用的实际额"
    );

    assert_eq!(
        row.act_amount[0] - row.std_amount[0],
        variances.material_amount(),
        "M5-11 的材料要素差异必须等于 价差 + 量差"
    );
    assert_eq!(
        row.act_amount[1] - row.std_amount[1],
        variances.labor_amount(),
        "M5-11 的人工要素差异必须等于 率差 + 效差"
    );
    assert_money(
        row.amount(),
        "4760.0000",
        "本例制造费用无差异 ⇒ 报表总差异 = 三差异之和",
    );
    assert_eq!(row.amount(), variances.total());
    assert_eq!(row.direction(), DIRECTION_UNFAVORABLE);
}

#[test]
fn compare_report_row_matches_a_favorable_variance_sum() {
    let card = drill_card();
    let actual = Actuals::of(
        vec![ActualComponent {
            material_id: uid(2),
            actual_price: dec("11.50"),
            actual_qty: dec("700"),
        }],
        "72",
        "52.00",
    )
    .with_overhead("1520.00");

    let variances = card.variances(&actual);
    assert_money(variances.material_amount(), "-2510.0000", "价差 −350 + 量差 −2160");
    assert_money(variances.labor_amount(), "-256.0000", "率差 144 + 效差 −400");
    assert_money(variances.total(), "-2766.0000", "三差异整体有利");

    let row = calc_of(&card.compare_core(&actual));
    assert_eq!(
        row.amount(),
        variances.total() + (actual.overhead_amount - money(card.std_overhead_per_unit * card.completed_qty)),
        "报表总差异 = 三差异 + 制造费用差异（本例 −80）"
    );
    assert_money(row.amount(), "-2846.0000", "全部有利的一期");
    assert_eq!(row.direction(), DIRECTION_FAVORABLE, "负总额必须显示为有利");
    assert!(variances.closes_exactly());
}

#[test]
fn compare_total_beyond_the_three_variances_is_the_overhead_variance() {
    let card = drill_card();
    let actual = drill_actuals().with_overhead("1500.00"); // 标准 2.00 × 800 = 1600，少花 100
    let variances = card.variances(&actual);
    let row = calc_of(&card.compare_core(&actual));

    let overhead_variance = actual.overhead_amount - money(card.std_overhead_per_unit * card.completed_qty);
    assert_money(overhead_variance, "-100.0000", "制造费用少花 100");
    assert_eq!(
        row.amount() - variances.total(),
        overhead_variance,
        "报表总差异 − 三差异必须**恰好**是制造费用差异（三差异不含费用两差，这一条把口径写死）"
    );
    assert_money(row.amount(), "4660.0000", "4760 − 100");
}

// ==================== I. 边界 ====================

#[test]
fn no_completion_makes_all_actual_usage_and_hours_unfavorable() {
    let card = one_component_card("0", "1.1", "12.00", "0.10", "50");
    assert_eq!(card.std_qty(&card.components[0]), Decimal::ZERO, "没完工 ⇒ 标准量 0");
    assert_eq!(card.std_hours(), Decimal::ZERO, "没完工 ⇒ 标准工时 0");

    let actual = Actuals::of(
        vec![ActualComponent {
            material_id: uid(12),
            actual_price: dec("12.50"),
            actual_qty: dec("300"),
        }],
        "25",
        "48.00",
    );
    let variances = card.variances(&actual);

    assert_money(
        variances.qty[0].amount,
        "3600.0000",
        "标准量 0 ⇒ 实际 300 全算超用（× 12）",
    );
    assert_money(
        variances.price[0].amount,
        "150.0000",
        "价差只在实际量上比价，与完工数无关",
    );
    assert_money(
        variances.labor.eff.amount,
        "1250.0000",
        "标准工时 0 ⇒ 25 h 全算超工时（× 50）",
    );
    assert_eq!(variances.qty[0].direction(), DIRECTION_UNFAVORABLE);
    assert_eq!(variances.labor.eff.direction(), DIRECTION_UNFAVORABLE);
    assert_money(
        variances.material_amount(),
        "3750.0000",
        "150 + 3600 = 实际材料 3750 − 0",
    );
    assert!(variances.closes_exactly());
}

#[test]
fn zero_standard_usage_puts_the_whole_pivot_into_the_qty_side() {
    // 标准单耗 0（新品没定 BOM）：标准量 0，实际用量全算超用；价差不受影响
    let card = one_component_card("100", "0", "12.00", "0.10", "50");
    let actual = Actuals::of(
        vec![ActualComponent {
            material_id: uid(12),
            actual_price: dec("12.50"),
            actual_qty: dec("100"),
        }],
        "10",
        "50",
    );
    let variances = card.variances(&actual);

    assert_eq!(variances.qty[0].std_qty, Decimal::ZERO);
    assert_money(variances.qty[0].amount, "1200.0000", "100 × 12 全算超用");
    assert_money(variances.price[0].amount, "50.0000", "(12.50 − 12.00) × 100");
    assert_money(variances.material_amount(), "1250.0000", "实际 1250 − 标准 0");
    assert!(variances.closes_exactly());
}

#[test]
fn zero_standard_price_collapses_the_qty_side_to_zero() {
    // 标准价 0（免费料 / 标准卡没填价）：量差恒 0，差异全归价差 —— 两个半区各自为 0，不互相串
    let card = one_component_card("100", "0.8", "0", "0.10", "50");
    let actual = Actuals::of(
        vec![ActualComponent {
            material_id: uid(12),
            actual_price: dec("12.50"),
            actual_qty: dec("100"),
        }],
        "10",
        "50",
    );
    let variances = card.variances(&actual);

    assert!(variances.qty[0].is_zero(), "标准价为 0 ⇒ 量差 0");
    assert_money(variances.price[0].amount, "1250.0000", "价差吃掉全部实际成本");
    assert_money(variances.material_amount(), "1250.0000", "实际 1250 − 标准 0");
    assert!(variances.closes_exactly());
}

#[test]
fn zero_variance_lines_are_flagged_and_still_carry_a_writable_direction() {
    // 实际与标准完全一致：三行都是 0，但每行仍必须给出可写库的方向
    let card = drill_card();
    let actual = Actuals::of(
        vec![ActualComponent {
            material_id: uid(2),
            actual_price: dec("12.00"),
            actual_qty: dec("880"),
        }],
        "80",
        "50",
    );
    let variances = card.variances(&actual);

    assert!(variances.price[0].is_zero());
    assert!(variances.qty[0].is_zero());
    assert!(variances.labor.eff.is_zero());
    assert!(variances.labor.rate.is_zero());
    assert_money(variances.total(), "0.0000", "完全达标");

    for row in variances.rows() {
        assert_eq!(row.label, "持平", "{} 应显示持平", row.variance_type);
        assert_eq!(
            row.direction, DIRECTION_UNFAVORABLE,
            "库层只有两值，0 走 amount >= 0 那支"
        );
    }
    assert!(variances.closes_exactly());
}

#[test]
fn labor_efficiency_is_independent_of_the_actual_rate() {
    // 效率差只吃「标准费率」，实际费率变了不能动它（否则率差被重复计入）
    let cheap = labor_two_factor(dec("48"), dec("50"), dec("90"), dec("80")).expect("便宜");
    let dear = labor_two_factor(dec("55"), dec("50"), dec("90"), dec("80")).expect("贵");

    assert_eq!(cheap.eff, dear.eff, "实际费率不同，效率差必须逐字段相同");
    assert_eq!(cheap.eff.amount, dec("500"));
    assert_ne!(cheap.rate.amount, dear.rate.amount);
    assert_eq!(
        dear.rate.amount - cheap.rate.amount,
        money(dec("55") * dec("90")) - money(dec("48") * dec("90")),
        "费率的差只反映在率差上"
    );
    assert!(cheap.closes_exactly() && dear.closes_exactly());
}

// ==================== J. 落表口径与守卫 ====================

#[test]
fn the_three_entry_points_cover_exactly_the_three_types() {
    let price = mat_price_variance(dec("12.50"), dec("12.00"), dec("1200")).expect("价差");
    let qty = mat_qty_variance(dec("1200"), dec("880"), dec("12.00")).expect("量差");
    let eff = labor_eff_variance(dec("90"), dec("80"), dec("50")).expect("效率差");

    assert_eq!(
        (price.variance_type, price.element),
        (VARIANCE_MAT_PRICE, ELEMENT_MATERIAL)
    );
    assert_eq!((qty.variance_type, qty.element), (VARIANCE_MAT_QTY, ELEMENT_MATERIAL));
    assert_eq!((eff.variance_type, eff.element), ("LABOR_EFF", ELEMENT_LABOR));

    assert_eq!(rate_label(VARIANCE_MAT_PRICE), "材料价差");
    assert_eq!(qty_label(VARIANCE_MAT_QTY), "材料量差");
    assert_eq!(qty_label("LABOR_EFF"), "人工效率差异");
    assert_eq!(qty_label(VARIANCE_MAT_QTY), "材料量差");
    assert_ne!(rate_label(VARIANCE_MAT_PRICE), qty_label(VARIANCE_MAT_QTY));
    assert_ne!(qty_label(VARIANCE_MAT_QTY), qty_label("LABOR_EFF"));

    for variance_type in [price.variance_type, qty.variance_type, eff.variance_type] {
        assert!(
            VARIANCE_TYPES_ALL.contains(&variance_type),
            "{variance_type} 不在 biz_cost_variance 的六值枚举里"
        );
    }
}

#[test]
fn every_line_carries_what_biz_cost_variance_needs() {
    let variances = drill_card().variances(&drill_actuals());
    let rows = variances.rows();
    assert_eq!(rows.len(), 4);

    for row in &rows {
        assert!(
            row.amount.scale() <= 4,
            "{} 的金额带 {} 位小数，入库列是 NUMERIC(18,4)",
            row.variance_type,
            row.amount.scale()
        );
        assert!(
            VARIANCE_TYPES_ALL.contains(&row.variance_type),
            "{} 不是库层 ck_biz_cost_var_type 的取值",
            row.variance_type
        );
        assert!(
            ELEMENTS.contains(&row.element),
            "{} 不是库层 ck_biz_cost_var_elem 的取值",
            row.element
        );
        assert!(
            matches!(row.direction, DIRECTION_UNFAVORABLE | DIRECTION_FAVORABLE),
            "{} 的方向 {} 不是库层两值之一",
            row.variance_type,
            row.direction
        );
        // 率差族不吃标准用量（写 NULL）；用量差族必须带标准用量，否则量差无法复核
        match row.variance_type {
            VARIANCE_MAT_PRICE | VARIANCE_LABOR_RATE => {
                assert!(
                    row.std_qty.is_none(),
                    "{} 的 std_qty 必须留空（公式不吃它）",
                    row.variance_type
                );
            }
            _ => {
                assert!(
                    row.std_qty.is_some(),
                    "{} 必须带 std_qty（量差要能复核）",
                    row.variance_type
                );
            }
        }
    }

    // 与库层 DDL 对拍：类型字面值与符号 CHECK 必须是本文件假设的那条
    let ddl = include_str!("../../../../migrations/business/V0374__cost_standard_tables.up.sql");
    for literal in [VARIANCE_MAT_PRICE, VARIANCE_MAT_QTY, "LABOR_EFF", VARIANCE_LABOR_RATE] {
        assert!(
            ddl.contains(&format!("'{literal}'")),
            "库层 ck_biz_cost_var_type 里没有 {literal}"
        );
    }
    assert!(
        ddl.contains("ck_biz_cost_var_sign"),
        "符号 CHECK 不见了：方向与金额可以互相矛盾"
    );
    assert!(
        ddl.contains("direction = 'UNFAVORABLE' AND amount >= 0"),
        "不利侧的符号闸变了"
    );
    assert!(
        ddl.contains("direction = 'FAVORABLE' AND amount <= 0"),
        "有利侧的符号闸变了"
    );
}

#[test]
fn six_decimal_inputs_never_leak_more_than_four_decimals() {
    let price = mat_price_variance(dec("12.123456"), dec("12.000001"), dec("7.123456")).expect("价差");
    let qty = mat_qty_variance(dec("7.123456"), dec("6.5"), dec("12.000001")).expect("量差");
    let eff = labor_eff_variance(dec("3.123456"), dec("2.5"), dec("33.333333")).expect("效率差");

    for (name, amount) in [("价差", price.amount), ("量差", qty.amount), ("效率差", eff.amount)] {
        assert!(
            amount.scale() <= 4,
            "{name} 带 {} 位小数：入库会被静默截断",
            amount.scale()
        );
    }
    assert_eq!(price.actual_amount, money(dec("12.123456") * dec("7.123456")));
    assert_eq!(qty.actual_qty, dec("7.123456"), "6 位用量原样保留（差异表是 18,6）");
    assert_eq!(eff.std_price, dec("33.333333"));
    assert_eq!(
        price.std_amount_at_actual_qty, qty.std_amount_at_actual_qty,
        "6 位输入下两个半区仍要共用一个分水岭"
    );
    assert_eq!(
        price.amount + qty.amount,
        price.actual_amount - qty.std_amount,
        "6 位输入的恒等式也必须精确闭合"
    );
}

#[test]
fn all_three_entry_points_reject_negative_inputs_with_field_wording() {
    // 负数不是「倒退生产」，是上游算错了：三个入口都必须 400，且消息指到具体字段
    let price = mat_price_variance(dec("-0.01"), dec("12"), dec("10")).expect_err("负实际价必须被拒");
    assert_eq!(price.code, error_code::PARAM_INVALID);
    assert_eq!(price.status, 400);
    assert!(
        price.msg.contains("实际单价"),
        "价差消息要指到「实际单价」：{}",
        price.msg
    );

    let qty = mat_qty_variance(dec("10"), dec("-1"), dec("12")).expect_err("负标准量必须被拒");
    assert_eq!(qty.code, error_code::PARAM_INVALID);
    assert!(qty.msg.contains("标准用量"), "量差消息要指到「标准用量」：{}", qty.msg);

    let eff = labor_eff_variance(dec("90"), dec("-1"), dec("50")).expect_err("负标准工时必须被拒");
    assert_eq!(eff.code, error_code::PARAM_INVALID);
    assert!(
        eff.msg.contains("标准工时"),
        "效率差消息要指到「标准工时」：{}",
        eff.msg
    );

    let hours = std_hours_of(dec("-0.1"), dec("10")).expect_err("负单位工时必须被拒");
    assert_eq!(hours.code, error_code::PARAM_INVALID);
    assert!(
        hours.msg.contains("单位标准工时"),
        "要指到「单位标准工时」：{}",
        hours.msg
    );
}

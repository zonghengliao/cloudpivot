//! 标准成本 × 实际成本 **逐物料对比** 的契约层与纯逻辑（成本核算 · M5-11）。
//!
//! 本文件只做三件事：**值域常量**、**算数**（一行一个 [`RowCalc`]，纯函数、零 DB）、**出参结构**。
//! 取数在 [`cost_standard_compare_model`](crate::business::erp::cost::cost_standard_compare_model)，
//! 编排与 HTTP 在 [`cost_standard_compare_handler`](crate::business::erp::cost::cost_standard_compare_handler)。
//!
//! # 本卡的口径（逐条写死，改之前先看 ARCH-9 §4.9）
//!
//! | 问题 | 本卡的口径 | 为什么 |
//! |---|---|---|
//! | 标准一侧取哪一版 | **期间末生效** + `status=1` + `publish_state='PUBLISHED'` 的那一版 | 与 M5-08 `period-sheet` 同口径（差异分析按「期末产量 × 标准单位成本」结账）；期间中途换版不静默：`switch_in_period=true` |
//! | 实际一侧取什么 | 当期**活跃批次**（`biz_cost_period`）的**成本计算单** `biz_cost_period_item` | 计算单是唯一同时有「完工数量 + 三要素单位成本 + 完工成本」的地方；`biz_cost_collect` 只有本期投入、没有完工口径 |
//! | 比较「单位」还是「总额」 | **两个都给**：单位差异是核心指标，金额差异 = 单位成本 × 完工数量 | 只看总额会把「产量少」错读成「成本低」，只看单价又看不到真金白银 |
//! | 差异符号 | `amount = 实际 − 标准`，**正数 = 不利（超支）/ 负数 = 有利（节约）** | 与 ARCH-9 §4.9 / `biz_cost_variance.amount` 同号；前端不用再翻符号 |
//! | 差异率 | `amount / 标准应发生额`，标准额 = 0 时给 `None` | 除以 0 不是「0%」而是「无从比较」，必须区分 |
//! | 差异入账吗 | **不入账**（不动存货、不出凭证） | ARCH-9 D-15：标准成本计价属独立 ADR；本卡只让差异「被看见」 |
//!
//! # 三种状态（决定什么算「可比」）
//!
//! ```text
//! BOTH        有已发布标准 + 本期有完工 ⇒ 可比（差异额 / 差异率才有意义）
//! NO_STANDARD 本期有实际完工，但没有已发布的标准 ⇒ 不可比（hint 指向「建卡 / 发布」）
//! NO_ACTUAL   有已发布标准，但本期没有计算单 / 完工数量为 0 ⇒ 不可比（hint 指向「先做成本计算」）
//! ```
//!
//! 状态只由**结构**决定（`standard` 是否存在、`completed_qty` 是否 > 0），不含任何金额判据 ——
//! 这样 SQL 侧的筛选 / 排序与这里的判定能逐条对上，模型层不必重复实现算数。
//!
//! # 尾差（不许藏）
//!
//! `act_amount` 是「完工数量 × 单位成本」逐要素相加的口径（与计算单 `finished_by_formula` 同源）；
//! 计算单账上的 `finished_cost` 在有期末在制时会吸收四位小数尾差。两者之差单列为
//! `rounding_diff`（`book_amount − act_amount`），**它不是差异**，只用于对平。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::cost::cost_period_item_dto::{money, unit_cost};

// ==================== 值域 ====================

/// 可比：两侧都在。
pub const STATE_BOTH: &str = "BOTH";
/// 有实际、没标准。
pub const STATE_NO_STANDARD: &str = "NO_STANDARD";
/// 有标准、没实际（无计算单或完工数量为 0）。
pub const STATE_NO_ACTUAL: &str = "NO_ACTUAL";

/// 状态文本。
pub fn state_label(state: &str) -> &'static str {
    match state {
        STATE_BOTH => "可比",
        STATE_NO_STANDARD => "缺标准",
        STATE_NO_ACTUAL => "缺实际",
        _ => "未知",
    }
}

/// 材料要素（与 `biz_cost_period_item.unit_material` / `biz_cost_variance.element` 同字面值）。
pub const ELEMENT_MATERIAL: &str = "MATERIAL";
/// 人工要素。
pub const ELEMENT_LABOR: &str = "LABOR";
/// 制造费用要素。
pub const ELEMENT_OVERHEAD: &str = "OVERHEAD";

/// 三要素顺序（出参顺序 = 计算单列顺序 = 凭证顺序；改这里就是改全表列序）。
pub const ELEMENTS: [&str; 3] = [ELEMENT_MATERIAL, ELEMENT_LABOR, ELEMENT_OVERHEAD];

/// 要素文本。
pub fn element_label(element: &str) -> &'static str {
    match element {
        ELEMENT_MATERIAL => "材料",
        ELEMENT_LABOR => "人工",
        ELEMENT_OVERHEAD => "制造费用",
        _ => "其它",
    }
}

/// 不利（实际 > 标准，超支）。
pub const DIRECTION_UNFAVORABLE: &str = "UNFAVORABLE";
/// 有利（实际 < 标准，节约）。
pub const DIRECTION_FAVORABLE: &str = "FAVORABLE";
/// 持平（差异额恰为 0）。
pub const DIRECTION_EVEN: &str = "EVEN";

/// 方向文本。
pub fn direction_label(direction: &str) -> &'static str {
    match direction {
        DIRECTION_UNFAVORABLE => "不利",
        DIRECTION_FAVORABLE => "有利",
        DIRECTION_EVEN => "持平",
        _ => "未知",
    }
}

/// 差异方向：`正 = 不利 / 负 = 有利 / 0 = 持平`（与 `ck_biz_cost_var_sign` 同号）。
pub fn direction_of(amount: Decimal) -> &'static str {
    if amount.is_zero() {
        DIRECTION_EVEN
    } else if amount > Decimal::ZERO {
        DIRECTION_UNFAVORABLE
    } else {
        DIRECTION_FAVORABLE
    }
}

/// 差异率（6 位小数，`0.012345` = 1.2345%）。
///
/// 分母为 0 时返回 `None`：标准应发生额为 0 意味着「没有可比基数」，
/// 不是「差异率 0%」（后者会让「标准压根没配」看起来像「完全达标」）。
pub fn rate_of(amount: Decimal, base: Decimal) -> Option<Decimal> {
    if base.is_zero() {
        None
    } else {
        Some(unit_cost(amount / base))
    }
}

/// 列表筛选（对外取值 `all` / `comparable` / `no_standard` / `no_actual`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StateFilter {
    /// 全部（默认）：两侧任一存在即列。
    #[default]
    All,
    /// 只看可比且有差异的（`|差异额| > 0`）：报表默认该看的视图。
    Comparable,
    /// 只看缺标准的。
    NoStandard,
    /// 只看缺实际的。
    NoActual,
}

impl StateFilter {
    /// 解析 `?state=`（缺省 = `all`；值域外的取值 400，不静默回落）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
            return Ok(Self::All);
        };
        match text.to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "comparable" => Ok(Self::Comparable),
            "no_standard" => Ok(Self::NoStandard),
            "no_actual" => Ok(Self::NoActual),
            other => Err(ApiError::new(
                400,
                cloudpivot_kernel::shared::error_code::PARAM_INVALID,
                format!(
                    "state 只能是 all / comparable / no_standard / no_actual（收到「{other}」）：\
                     comparable = 只看可比且有差异的物料，no_standard = 只看本期有完工但没已发布标准的，\
                     no_actual = 只看有标准但本期没完工 / 没算的"
                ),
            )),
        }
    }

    /// 对外字面值（回显用，与 [`StateFilter::parse`] 一一对应）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Comparable => "comparable",
            Self::NoStandard => "no_standard",
            Self::NoActual => "no_actual",
        }
    }
}

// ==================== 取数行（model → dto） ====================

/// 标准成本卡一侧（已发布 + 期间末生效）。
#[derive(Debug, Clone, PartialEq)]
pub struct StandardSide {
    /// 卡头 id。
    pub standard_id: Uuid,
    /// 版本号。
    pub version_no: i32,
    /// 生效起始日。
    pub effective_from: NaiveDate,
    /// 停止日。
    pub effective_to: Option<NaiveDate>,
    /// 单位标准材料成本。
    pub std_material_cost: Decimal,
    /// 单位标准人工成本。
    pub std_labor_cost: Decimal,
    /// 单位标准制造费用。
    pub std_overhead_cost: Decimal,
    /// 单位标准总成本（库层 CHECK 保证 = 三要素之和）。
    pub std_total_cost: Decimal,
    /// 单位标准工时。
    pub std_labor_hours: Decimal,
    /// 标准工资率。
    pub std_labor_rate: Decimal,
    /// 最近一次发布时刻。
    pub published_at: Option<DateTime<Utc>>,
    /// 期间内是否还生效过别的已发布版本（换版提示）。
    pub switch_in_period: bool,
}

/// 实际成本一侧（当期活跃批次的成本计算单，按产出品聚合）。
#[derive(Debug, Clone, PartialEq)]
pub struct ActualSide {
    /// 本期涉及的计算单行数（批次数）。
    pub object_count: i64,
    /// 本期完工数量（Σ `completed_qty`）。
    pub completed_qty: Decimal,
    /// 账面完工成本（Σ `finished_cost`）—— 对平用，不参与差异。
    pub book_amount: Decimal,
    /// 完工口径的实际材料成本（Σ round(完工数量 × 单位材料成本, 4)）。
    pub amount_material: Decimal,
    /// 完工口径的实际人工成本。
    pub amount_labor: Decimal,
    /// 完工口径的实际制造费用。
    pub amount_overhead: Decimal,
    /// 本期投入合计（Σ mat_cur + labor_cur + oh_cur）—— 只作参考：含留在在制里的部分。
    pub period_input: Decimal,
    /// 期末在制成本（Σ `wip_end_cost`）—— 只作参考。
    pub wip_end_cost: Decimal,
}

impl ActualSide {
    /// 三要素完工成本（顺序见 [`ELEMENTS`]）。
    pub fn amounts(&self) -> [Decimal; 3] {
        [self.amount_material, self.amount_labor, self.amount_overhead]
    }

    /// 完工成本合计（三要素相加，**不用** `book_amount`：后者含尾差）。
    pub fn total(&self) -> Decimal {
        self.amounts().iter().copied().sum()
    }
}

/// 一行物料的两侧取数（`model` 直接产出它，`dto` 只管算数与出参）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompareCore {
    /// 物料（成品 / 半成品）。
    pub material_id: Uuid,
    /// 物料编码（实时 join `biz_material`；物料被删后为空）。
    pub material_code: Option<String>,
    /// 物料名称（实时 join）。
    pub material_name: Option<String>,
    /// 物料规格（实时 join）。
    pub material_spec: Option<String>,
    /// 该物料的**未发布**标准版本数（草稿 / 审批中）—— 缺标准时用来分辨「没建卡」还是「没发布」。
    pub unpublished_versions: i64,
    /// 标准一侧（无已发布标准 ⇒ `None`）。
    pub standard: Option<StandardSide>,
    /// 实际一侧（本期无计算单 ⇒ `None`）。
    pub actual: Option<ActualSide>,
}

impl CompareCore {
    /// 对比状态（只由结构决定，见模块头）。
    pub fn state(&self) -> &'static str {
        match (&self.standard, &self.actual) {
            (Some(_), Some(act)) if !act.completed_qty.is_zero() => STATE_BOTH,
            (None, _) => STATE_NO_STANDARD,
            _ => STATE_NO_ACTUAL,
        }
    }

    /// 展示 / 排序用的物料标签（编码 + 名称；物料已删则只有 id）。
    pub fn material_label(&self) -> String {
        match (&self.material_code, &self.material_name) {
            (Some(code), Some(name)) => format!("{code} {name}"),
            (Some(code), None) => code.clone(),
            (None, Some(name)) => name.clone(),
            (None, None) => self.material_id.to_string(),
        }
    }
}

// ==================== 算数（唯一入口） ====================

/// 一行对比的**算数结果**（纯 Decimal；出参与合计共用同一份，不重复实现口径）。
#[derive(Debug, Clone, PartialEq)]
pub struct RowCalc {
    /// 状态（见模块头）。
    pub state: &'static str,
    /// 完工数量（缺实际时为 0）。
    pub completed_qty: Decimal,
    /// 单位标准成本（三要素，顺序见 [`ELEMENTS`]）。
    pub std_unit: [Decimal; 3],
    /// 单位实际成本（三要素；`completed_qty = 0` 时全 0 —— 除数为 0 不做除法）。
    pub act_unit: [Decimal; 3],
    /// 标准应发生额（= 单位标准 × 完工数量，逐要素 4 位）。
    pub std_amount: [Decimal; 3],
    /// 实际完工成本（三要素 4 位）。
    pub act_amount: [Decimal; 3],
    /// 账面完工成本（Σ `finished_cost`）。
    pub book_amount: Decimal,
}

impl RowCalc {
    /// 标准应发生额合计。
    pub fn std_total(&self) -> Decimal {
        self.std_amount.iter().copied().sum()
    }

    /// 实际完工成本合计（三要素相加）。
    pub fn act_total(&self) -> Decimal {
        self.act_amount.iter().copied().sum()
    }

    /// 差异额 = 实际 − 标准（正 = 不利）。
    pub fn amount(&self) -> Decimal {
        self.act_total() - self.std_total()
    }

    /// 单位标准成本合计。
    pub fn std_unit_total(&self) -> Decimal {
        self.std_unit.iter().copied().sum()
    }

    /// 单位实际成本合计。
    pub fn act_unit_total(&self) -> Decimal {
        self.act_unit.iter().copied().sum()
    }

    /// 单位差异（单位实际 − 单位标准，6 位）。
    pub fn unit_amount(&self) -> Decimal {
        unit_cost(self.act_unit_total() - self.std_unit_total())
    }

    /// 差异率（`None` = 标准应发生额为 0）。
    pub fn rate(&self) -> Option<Decimal> {
        rate_of(self.amount(), self.std_total())
    }

    /// 差异方向。
    pub fn direction(&self) -> &'static str {
        direction_of(self.amount())
    }

    /// 尾差 = 账面完工成本 − 三要素相加的实际完工成本（**不是差异**，对平用）。
    pub fn rounding_diff(&self) -> Decimal {
        money(self.book_amount - self.act_total())
    }

    /// 可比（只有可比行才出差异块、才进合计的差异口径）。
    pub fn comparable(&self) -> bool {
        self.state == STATE_BOTH
    }
}

/// 一行取数 → 一行算数（**全模块唯一的算数入口**，单测直接喂 [`CompareCore`]）。
pub fn calc_of(core: &CompareCore) -> RowCalc {
    let state = core.state();
    let std_unit = core
        .standard
        .as_ref()
        .map(|std| [std.std_material_cost, std.std_labor_cost, std.std_overhead_cost])
        .unwrap_or([Decimal::ZERO; 3]);
    let (qty, act_amount, book_amount) = match core.actual.as_ref() {
        Some(act) => (act.completed_qty, act.amounts(), act.book_amount),
        None => (Decimal::ZERO, [Decimal::ZERO; 3], Decimal::ZERO),
    };
    // 单位实际成本 = 完工成本 / 完工数量：数量为 0 时不做除法（0 而不是 NaN / panic）
    let act_unit: [Decimal; 3] = std::array::from_fn(|i| {
        if qty.is_zero() {
            Decimal::ZERO
        } else {
            unit_cost(act_amount[i] / qty)
        }
    });
    // 标准应发生额 = 单位标准 × 完工数量（缺实际 ⇒ 数量 0 ⇒ 金额 0，不必再分支）
    let std_amount: [Decimal; 3] = std::array::from_fn(|i| money(std_unit[i] * qty));
    RowCalc {
        state,
        completed_qty: qty,
        std_unit,
        act_unit,
        std_amount,
        act_amount,
        book_amount,
    }
}

// ==================== 合计 ====================

/// 整表合计（可比行才进差异口径；缺标准的实际成本单列，不混进差异）。
///
/// 两个来源共用它：真库 SQL 聚合（[`cost_standard_compare_model`]）与纯函数
/// [`totals_of`]（单测 / 对拍）。字段语义必须逐条一致，真库用例会对拍。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TotalsCalc {
    /// 行数（物料数）。
    pub rows: i64,
    /// 可比行数。
    pub comparable_rows: i64,
    /// 缺标准的行数。
    pub no_standard_rows: i64,
    /// 缺实际的行数。
    pub no_actual_rows: i64,
    /// 可比行的完工数量合计。
    pub completed_qty: Decimal,
    /// 可比行的标准应发生额（三要素）。
    pub std_amount: [Decimal; 3],
    /// 可比行的实际完工成本（三要素）。
    pub act_amount: [Decimal; 3],
    /// 可比行的账面完工成本合计。
    pub book_amount: Decimal,
    /// 缺标准行的实际完工成本合计（**不可比**，只作「没标准的有多少钱」参考）。
    pub no_standard_amount: Decimal,
}

impl TotalsCalc {
    /// 标准应发生额合计。
    pub fn std_total(&self) -> Decimal {
        self.std_amount.iter().copied().sum()
    }

    /// 实际完工成本合计。
    pub fn act_total(&self) -> Decimal {
        self.act_amount.iter().copied().sum()
    }

    /// 差异额合计。
    pub fn amount(&self) -> Decimal {
        self.act_total() - self.std_total()
    }

    /// 差异率合计（口径 = 总差异 / 总标准额，不是逐行差异率的平均）。
    pub fn rate(&self) -> Option<Decimal> {
        rate_of(self.amount(), self.std_total())
    }

    /// 方向。
    pub fn direction(&self) -> &'static str {
        direction_of(self.amount())
    }

    /// 尾差合计。
    pub fn rounding_diff(&self) -> Decimal {
        money(self.book_amount - self.act_total())
    }
}

/// 逐行算数 → 整表合计（**只吃 [`RowCalc`]**，不重算任何公式）。
pub fn totals_of(calcs: &[RowCalc]) -> TotalsCalc {
    let mut totals = TotalsCalc::default();
    for calc in calcs {
        totals.rows += 1;
        match calc.state {
            STATE_BOTH => {
                totals.comparable_rows += 1;
                totals.completed_qty += calc.completed_qty;
                totals.book_amount += calc.book_amount;
                for i in 0..ELEMENTS.len() {
                    totals.std_amount[i] += calc.std_amount[i];
                    totals.act_amount[i] += calc.act_amount[i];
                }
            }
            STATE_NO_STANDARD => {
                totals.no_standard_rows += 1;
                totals.no_standard_amount += calc.act_total();
            }
            _ => totals.no_actual_rows += 1,
        }
    }
    totals
}

// ==================== 出参 ====================

/// 数值 → 文本（固定小数位，避免前端拿到科学计数法）。
pub fn fmt(value: Decimal, scale: u32) -> String {
    format!("{:.*}", scale as usize, value)
}

/// 金额文本（4 位）。
pub fn money_text(value: Decimal) -> String {
    fmt(money(value), 4)
}

/// 单价 / 单位成本文本（6 位）。
pub fn unit_text(value: Decimal) -> String {
    fmt(unit_cost(value), 6)
}

/// 标准一侧出参。
#[derive(Debug, Clone, Serialize)]
pub struct StandardView {
    /// 卡头 id。
    pub id: String,
    /// 版本号。
    pub version_no: i32,
    /// 生效起始日。
    pub effective_from: String,
    /// 停止日（空 = 至今有效）。
    pub effective_to: Option<String>,
    /// 最近一次发布时刻。
    pub published_at: Option<String>,
    /// 期间内是否换过版（本月还有别的已发布版本生效过）。
    pub switch_in_period: bool,
    /// 单位标准材料成本。
    pub std_material_cost: String,
    /// 单位标准人工成本。
    pub std_labor_cost: String,
    /// 单位标准制造费用。
    pub std_overhead_cost: String,
    /// 单位标准总成本。
    pub std_total_cost: String,
    /// 单位标准工时。
    pub std_labor_hours: String,
    /// 标准工资率。
    pub std_labor_rate: String,
}

/// 实际一侧出参。
#[derive(Debug, Clone, Serialize)]
pub struct ActualView {
    /// 本期涉及的计算单行数（批次数）。
    pub object_count: i64,
    /// 本期完工数量。
    pub completed_qty: String,
    /// 账面完工成本（Σ `finished_cost`）。
    pub book_amount: String,
    /// 完工口径实际材料成本。
    pub amount_material: String,
    /// 完工口径实际人工成本。
    pub amount_labor: String,
    /// 完工口径实际制造费用。
    pub amount_overhead: String,
    /// 完工口径实际成本合计。
    pub amount_total: String,
    /// 单位实际材料成本。
    pub unit_material: String,
    /// 单位实际人工成本。
    pub unit_labor: String,
    /// 单位实际制造费用。
    pub unit_overhead: String,
    /// 单位实际成本合计。
    pub unit_total: String,
    /// 本期投入合计（含留在在制的部分，仅参考）。
    pub period_input: String,
    /// 期末在制成本（仅参考）。
    pub wip_end_cost: String,
}

/// 要素级差异出参（行内 / 合计共用；合计块的三个 `unit_*` 为空串）。
#[derive(Debug, Clone, Serialize)]
pub struct ElementView {
    /// 要素字面值（`MATERIAL` / `LABOR` / `OVERHEAD`）。
    pub element: String,
    /// 要素文本。
    pub element_label: String,
    /// 单位标准成本（合计块为空串）。
    pub std_unit: String,
    /// 单位实际成本（合计块为空串）。
    pub act_unit: String,
    /// 单位差异（合计块为空串）。
    pub unit_amount: String,
    /// 标准应发生额。
    pub std_amount: String,
    /// 实际完工成本。
    pub act_amount: String,
    /// 差异额（正 = 不利）。
    pub amount: String,
    /// 差异率（`null` = 标准应发生额为 0）。
    pub rate: Option<String>,
    /// 方向（`FAVORABLE` / `UNFAVORABLE` / `EVEN`）。
    pub direction: String,
    /// 方向文本。
    pub direction_label: String,
}

/// 差异块（只有可比行才有）。
#[derive(Debug, Clone, Serialize)]
pub struct VarianceView {
    /// 完工数量（比较基数）。
    pub completed_qty: String,
    /// 标准应发生额合计。
    pub std_amount: String,
    /// 实际完工成本合计。
    pub act_amount: String,
    /// 差异额（正 = 不利）。
    pub amount: String,
    /// 差异率（`null` = 标准应发生额为 0）。
    pub rate: Option<String>,
    /// 方向。
    pub direction: String,
    /// 方向文本。
    pub direction_label: String,
    /// 单位标准成本合计。
    pub unit_std_amount: String,
    /// 单位实际成本合计。
    pub unit_act_amount: String,
    /// 单位差异。
    pub unit_amount: String,
    /// 三要素差异（顺序见 [`ELEMENTS`]）。
    pub elements: Vec<ElementView>,
    /// 尾差（账面完工成本 − 三要素实际完工成本；对平用，不是差异）。
    pub rounding_diff: String,
}

/// 逐物料对比一行。
#[derive(Debug, Clone, Serialize)]
pub struct CompareRow {
    /// 物料。
    pub material_id: String,
    /// 物料编码（实时 join；物料被删后为空）。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 物料规格。
    pub material_spec: Option<String>,
    /// 对比状态（`BOTH` / `NO_STANDARD` / `NO_ACTUAL`）。
    pub state: String,
    /// 状态文本。
    pub state_label: String,
    /// 标准一侧（无已发布版本 ⇒ `null`）。
    pub standard: Option<StandardView>,
    /// 实际一侧（本期无计算单 ⇒ `null`）。
    pub actual: Option<ActualView>,
    /// 差异块（不可比 ⇒ `null`）。
    pub variance: Option<VarianceView>,
    /// 本行该怎么看 / 怎么修（空串 = 无需说明）。
    pub hint: String,
}

/// 整表合计出参。
#[derive(Debug, Clone, Serialize)]
pub struct TotalsView {
    /// 行数（物料数）。
    pub rows: i64,
    /// 可比行数。
    pub comparable_rows: i64,
    /// 缺标准的行数。
    pub no_standard_rows: i64,
    /// 缺实际的行数。
    pub no_actual_rows: i64,
    /// 可比行完工数量合计。
    pub completed_qty: String,
    /// 标准应发生额合计。
    pub std_amount: String,
    /// 实际完工成本合计。
    pub act_amount: String,
    /// 差异额合计（正 = 不利）。
    pub amount: String,
    /// 差异率合计（`null` = 标准应发生额合计为 0）。
    pub rate: Option<String>,
    /// 方向。
    pub direction: String,
    /// 方向文本。
    pub direction_label: String,
    /// 缺标准行的实际完工成本合计（不可比，参考）。
    pub no_standard_amount: String,
    /// 可比行账面完工成本合计。
    pub book_amount: String,
    /// 尾差合计（对平用）。
    pub rounding_diff: String,
    /// 三要素合计（顺序见 [`ELEMENTS`]）。
    pub elements: Vec<ElementView>,
}

/// 当期活跃批次（实际成本的出处）。
#[derive(Debug, Clone, Serialize)]
pub struct BatchView {
    /// 批次 id。
    pub id: String,
    /// 批次号（`CS-yyyyMM-0001`）。
    pub batch_no: String,
    /// 状态（`DRAFT` / `SUBMITTED` / `APPROVED` / `SETTLED` / `CLOSED`）。
    pub status: String,
}

/// 逐物料对比报表出参（`GET /business/erp/report/cost-variance`）。
#[derive(Debug, Clone, Serialize)]
pub struct CompareRes {
    /// 期间（`yyyy-MM`）。
    pub period_code: String,
    /// 期间首日。
    pub period_start: String,
    /// 期间末日 —— 标准成本按这一天取版本。
    pub period_end: String,
    /// 标准一侧的取数口径（固定文案，前端直接展示）。
    pub standard_basis: String,
    /// 当期活跃批次（没有 ⇒ `null`：本期还没做成本计算）。
    pub batch: Option<BatchView>,
    /// 实际一侧是否就绪（有活跃批次即有计算单可读）。
    pub cost_ready: bool,
    /// 本次筛选（`all` / `comparable` / `no_standard` / `no_actual`）。
    pub state_filter: String,
    /// 物料名关键字（回显）。
    pub keyword: Option<String>,
    /// 当前页行。
    pub list: Vec<CompareRow>,
    /// 命中总数（按筛选后）。
    pub total: u64,
    /// 当前页。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 整表合计（**按筛选后的全集**，不是当前页）。
    pub totals: TotalsView,
    /// 口径说明（差异入账与否 / 正负号 / 尾差）。
    pub note: String,
}

/// 标准一侧取数口径文案（出参、文档、前端提示共用一句话）。
pub const STANDARD_BASIS: &str = "期间末生效 + 已发布 + 启用的那一版标准成本";

/// 报表口径说明。
pub fn note_of(period_end: &str) -> String {
    format!(
        "标准成本取「{STANDARD_BASIS}」（本期基准日 {period_end}）；实际成本取当期成本核算批次的成本计算单\
         （完工数量 × 三要素单位成本）。差异 = 实际 − 标准，正数 = 不利（超支）、负数 = 有利（节约）；\
         差异率 = 差异额 / 标准应发生额，标准额为 0 时不计算。差异**不入账**：本报表只让差异被看见，\
         不动存货计价、不出凭证（ARCH-9 D-15）。"
    )
}

// ==================== 装配 ====================

/// 取数行 → 出参行（含 `hint`）。编排层唯一调用的装配函数。
pub fn row_view(core: &CompareCore, period_code: &str) -> CompareRow {
    let calc = calc_of(core);
    let standard = core.standard.as_ref().map(|std| StandardView {
        id: std.standard_id.to_string(),
        version_no: std.version_no,
        effective_from: std.effective_from.to_string(),
        effective_to: std.effective_to.map(|date| date.to_string()),
        published_at: std.published_at.map(|at| at.to_rfc3339()),
        switch_in_period: std.switch_in_period,
        std_material_cost: unit_text(std.std_material_cost),
        std_labor_cost: unit_text(std.std_labor_cost),
        std_overhead_cost: unit_text(std.std_overhead_cost),
        std_total_cost: unit_text(std.std_total_cost),
        std_labor_hours: fmt(std.std_labor_hours, 4),
        std_labor_rate: unit_text(std.std_labor_rate),
    });
    let actual = core.actual.as_ref().map(|act| ActualView {
        object_count: act.object_count,
        completed_qty: fmt(act.completed_qty, 4),
        book_amount: money_text(act.book_amount),
        amount_material: money_text(act.amount_material),
        amount_labor: money_text(act.amount_labor),
        amount_overhead: money_text(act.amount_overhead),
        amount_total: money_text(act.total()),
        unit_material: unit_text(calc.act_unit[0]),
        unit_labor: unit_text(calc.act_unit[1]),
        unit_overhead: unit_text(calc.act_unit[2]),
        unit_total: unit_text(calc.act_unit_total()),
        period_input: money_text(act.period_input),
        wip_end_cost: money_text(act.wip_end_cost),
    });
    let variance = calc.comparable().then(|| VarianceView {
        completed_qty: fmt(calc.completed_qty, 4),
        std_amount: money_text(calc.std_total()),
        act_amount: money_text(calc.act_total()),
        amount: money_text(calc.amount()),
        rate: calc.rate().map(|rate| fmt(rate, 6)),
        direction: calc.direction().to_string(),
        direction_label: direction_label(calc.direction()).to_string(),
        unit_std_amount: unit_text(calc.std_unit_total()),
        unit_act_amount: unit_text(calc.act_unit_total()),
        unit_amount: unit_text(calc.unit_amount()),
        elements: ELEMENTS
            .iter()
            .enumerate()
            .map(|(index, element)| element_view(element, &calc, index))
            .collect(),
        rounding_diff: money_text(calc.rounding_diff()),
    });
    CompareRow {
        material_id: core.material_id.to_string(),
        material_code: core.material_code.clone(),
        material_name: core.material_name.clone(),
        material_spec: core.material_spec.clone(),
        state: calc.state.to_string(),
        state_label: state_label(calc.state).to_string(),
        standard,
        actual,
        variance,
        hint: hint_of(core, &calc, period_code),
    }
}

/// 要素差异出参（行内用；`index` 取 [`ELEMENTS`] 下标）。
fn element_view(element: &str, calc: &RowCalc, index: usize) -> ElementView {
    let std_amount = calc.std_amount[index];
    let act_amount = calc.act_amount[index];
    let amount = act_amount - std_amount;
    ElementView {
        element: element.to_string(),
        element_label: element_label(element).to_string(),
        std_unit: unit_text(calc.std_unit[index]),
        act_unit: unit_text(calc.act_unit[index]),
        unit_amount: unit_text(calc.act_unit[index] - calc.std_unit[index]),
        std_amount: money_text(std_amount),
        act_amount: money_text(act_amount),
        amount: money_text(amount),
        rate: rate_of(amount, std_amount).map(|rate| fmt(rate, 6)),
        direction: direction_of(amount).to_string(),
        direction_label: direction_label(direction_of(amount)).to_string(),
    }
}

/// 合计 → 出参（三要素块的三个 `unit_*` 留空串：合计的「单位」没有业务含义）。
pub fn totals_view(totals: &TotalsCalc) -> TotalsView {
    TotalsView {
        rows: totals.rows,
        comparable_rows: totals.comparable_rows,
        no_standard_rows: totals.no_standard_rows,
        no_actual_rows: totals.no_actual_rows,
        completed_qty: fmt(totals.completed_qty, 4),
        std_amount: money_text(totals.std_total()),
        act_amount: money_text(totals.act_total()),
        amount: money_text(totals.amount()),
        rate: totals.rate().map(|rate| fmt(rate, 6)),
        direction: totals.direction().to_string(),
        direction_label: direction_label(totals.direction()).to_string(),
        no_standard_amount: money_text(totals.no_standard_amount),
        book_amount: money_text(totals.book_amount),
        rounding_diff: money_text(totals.rounding_diff()),
        elements: ELEMENTS
            .iter()
            .enumerate()
            .map(|(index, element)| {
                let std_amount = totals.std_amount[index];
                let act_amount = totals.act_amount[index];
                let amount = act_amount - std_amount;
                ElementView {
                    element: element.to_string(),
                    element_label: element_label(element).to_string(),
                    std_unit: String::new(),
                    act_unit: String::new(),
                    unit_amount: String::new(),
                    std_amount: money_text(std_amount),
                    act_amount: money_text(act_amount),
                    amount: money_text(amount),
                    rate: rate_of(amount, std_amount).map(|rate| fmt(rate, 6)),
                    direction: direction_of(amount).to_string(),
                    direction_label: direction_label(direction_of(amount)).to_string(),
                }
            })
            .collect(),
    }
}

/// 本行该怎么看 / 怎么修（**具体动作**，不是「请检查」）。
pub fn hint_of(core: &CompareCore, calc: &RowCalc, period_code: &str) -> String {
    match calc.state {
        STATE_NO_STANDARD => {
            let label = core.material_label();
            let actual = money_text(calc.act_total());
            if core.unpublished_versions > 0 {
                format!(
                    "{label}：本期有完工（实际 {actual}），但这一版标准成本还没发布\
                     （未发布版本 {} 个：草稿 / 审批中）。去「标准成本」页提交发布审批，通过后本行即可对比",
                    core.unpublished_versions
                )
            } else {
                format!(
                    "{label}：本期有完工（实际 {actual}），但没有标准成本卡。\
                     去「标准成本」页按期间建一版并提交发布，才谈得上对比"
                )
            }
        }
        STATE_NO_ACTUAL => {
            let label = core.material_label();
            match core.actual.as_ref() {
                None => format!(
                    "{label}：有标准（单位 {}），但 {period_code} 没有成本计算单。\
                     先做成本核算（该期间的成本核算批次 → 计算），或本期确实没投产",
                    unit_text(calc.std_unit_total())
                ),
                Some(act) => format!(
                    "{label}：有标准（单位 {}），但本期完工数量为 0（全在制：本期投入 {}、\
                     期末在制 {}）—— 单位成本无从比较，等完工后再看",
                    unit_text(calc.std_unit_total()),
                    money_text(act.period_input),
                    money_text(act.wip_end_cost)
                ),
            }
        }
        _ => {
            let mut notes: Vec<String> = Vec::new();
            if core.standard.as_ref().map(|std| std.switch_in_period).unwrap_or(false) {
                notes.push("本期换过标准成本版本：本行按期末生效的那一版对比".to_string());
            }
            if core.unpublished_versions > 0 {
                notes.push(format!(
                    "另有 {} 个未发布版本（草稿 / 审批中），不影响本期对比",
                    core.unpublished_versions
                ));
            }
            let rounding = calc.rounding_diff();
            if !rounding.is_zero() {
                notes.push(format!(
                    "账面完工成本与「单位成本 × 完工数量」相差 {}（四位小数取舍，对平用，不是差异）",
                    money_text(rounding)
                ));
            }
            notes.join("；")
        }
    }
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn standard(material: Option<Decimal>, labor: Option<Decimal>, overhead: Option<Decimal>) -> StandardSide {
        let material = material.unwrap_or(dec("10"));
        let labor = labor.unwrap_or(dec("4"));
        let overhead = overhead.unwrap_or(dec("6"));
        StandardSide {
            standard_id: Uuid::from_u128(0x5100_0000_0000_0000_0000_0000_0000_0001),
            version_no: 2,
            effective_from: date("2026-11-01"),
            effective_to: None,
            std_material_cost: material,
            std_labor_cost: labor,
            std_overhead_cost: overhead,
            std_total_cost: material + labor + overhead,
            std_labor_hours: dec("0.5"),
            std_labor_rate: dec("50"),
            published_at: None,
            switch_in_period: false,
        }
    }

    fn actual(qty: Decimal, material: Decimal, labor: Decimal, overhead: Decimal, book: Decimal) -> ActualSide {
        ActualSide {
            object_count: 1,
            completed_qty: qty,
            book_amount: book,
            amount_material: material,
            amount_labor: labor,
            amount_overhead: overhead,
            period_input: material + labor + overhead,
            wip_end_cost: Decimal::ZERO,
        }
    }

    fn core(standard: Option<StandardSide>, actual: Option<ActualSide>) -> CompareCore {
        CompareCore {
            material_id: Uuid::from_u128(0x5100_0000_0000_0000_0000_0000_0000_0002),
            material_code: Some("FG-100".to_string()),
            material_name: Some("成品 A".to_string()),
            material_spec: Some("标准规格".to_string()),
            unpublished_versions: 0,
            standard,
            actual,
        }
    }

    // ---------- 三态 ----------

    #[test]
    fn state_needs_both_sides_and_finished_quantity() {
        let both = core(
            Some(standard(None, None, None)),
            Some(actual(dec("100"), dec("1000"), dec("400"), dec("600"), dec("2000"))),
        );
        assert_eq!(both.state(), STATE_BOTH);

        let no_std = core(
            None,
            Some(actual(dec("100"), dec("1000"), dec("400"), dec("600"), dec("2000"))),
        );
        assert_eq!(no_std.state(), STATE_NO_STANDARD);

        let no_act = core(Some(standard(None, None, None)), None);
        assert_eq!(no_act.state(), STATE_NO_ACTUAL);

        // 有计算单但本期完工为 0（全在制）：单位成本无从比较 ⇒ 缺实际，不是可比
        let wip_only = core(
            Some(standard(None, None, None)),
            Some(actual(Decimal::ZERO, dec("500"), dec("200"), dec("300"), Decimal::ZERO)),
        );
        assert_eq!(wip_only.state(), STATE_NO_ACTUAL);
    }

    // ---------- 算数（ARCH-9 §4.9 算例 C 的形状） ----------

    #[test]
    fn both_sides_compare_units_and_amounts() {
        // 标准单位 20（10/4/6），实际 100 件：材料 1200×12.5 → 但这里用的是计算单口径
        // 实际完工 2300（1100/400/800）⇒ 单位 23（11/4/8）
        let row = core(
            Some(standard(None, None, None)),
            Some(actual(dec("100"), dec("1100"), dec("400"), dec("800"), dec("2300"))),
        );
        let calc = calc_of(&row);
        assert_eq!(calc.completed_qty, dec("100"));
        assert_eq!(calc.std_unit_total(), dec("20"));
        assert_eq!(calc.act_unit_total(), dec("23"));
        assert_eq!(calc.std_total(), dec("2000"), "标准应发生 = 20 × 100");
        assert_eq!(calc.act_total(), dec("2300"));
        assert_eq!(calc.amount(), dec("300"), "差异 = 实际 − 标准");
        assert_eq!(calc.unit_amount(), dec("3"));
        assert_eq!(calc.rate(), Some(dec("0.150000")), "300 / 2000 = 15%");
        assert_eq!(calc.direction(), DIRECTION_UNFAVORABLE);
        assert_eq!(calc.rounding_diff(), Decimal::ZERO, "账面 = 三要素相加 ⇒ 无尾差");
    }

    #[test]
    fn favorable_variance_is_negative() {
        let row = core(
            Some(standard(None, None, None)),
            Some(actual(dec("100"), dec("900"), dec("400"), dec("600"), dec("1900"))),
        );
        let calc = calc_of(&row);
        assert_eq!(calc.amount(), dec("-100"), "节约必须是负差异");
        assert_eq!(calc.direction(), DIRECTION_FAVORABLE);
        assert_eq!(calc.rate(), Some(dec("-0.050000")));
    }

    #[test]
    fn even_variance_is_neither_favorable_nor_unfavorable() {
        let row = core(
            Some(standard(None, None, None)),
            Some(actual(dec("100"), dec("1000"), dec("400"), dec("600"), dec("2000"))),
        );
        let calc = calc_of(&row);
        assert_eq!(calc.amount(), Decimal::ZERO);
        assert_eq!(calc.direction(), DIRECTION_EVEN);
        assert_eq!(
            calc.rate(),
            Some(Decimal::ZERO),
            "有基数时 0 差异 ⇒ 0%（与「无基数」区分）"
        );
    }

    #[test]
    fn zero_standard_base_has_no_rate() {
        // 标准卡三要素全 0（服务类 / 只登记工时的料），实际有成本 ⇒ 差异率无从算
        let row = core(
            Some(standard(Some(Decimal::ZERO), Some(Decimal::ZERO), Some(Decimal::ZERO))),
            Some(actual(dec("10"), dec("100"), Decimal::ZERO, Decimal::ZERO, dec("100"))),
        );
        let calc = calc_of(&row);
        assert_eq!(calc.rate(), None, "标准额为 0 不能当成 0%");
        assert_eq!(calc.direction(), DIRECTION_UNFAVORABLE);
    }

    #[test]
    fn elements_split_adds_up_to_the_total() {
        let row = core(
            Some(standard(Some(dec("10")), Some(dec("4")), Some(dec("6")))),
            Some(actual(dec("50"), dec("600"), dec("150"), dec("300"), dec("1050"))),
        );
        let calc = calc_of(&row);
        assert_eq!(calc.std_amount, [dec("500"), dec("200"), dec("300")]);
        assert_eq!(calc.act_amount, [dec("600"), dec("150"), dec("300")]);
        assert_eq!(calc.std_total(), calc.std_amount.iter().copied().sum::<Decimal>());
        assert_eq!(calc.act_total(), calc.act_amount.iter().copied().sum::<Decimal>());
        // 逐要素差异：材料 +100（不利）、人工 −50（有利）、费用 0（持平）
        let material = calc.act_amount[0] - calc.std_amount[0];
        let labor = calc.act_amount[1] - calc.std_amount[1];
        let overhead = calc.act_amount[2] - calc.std_amount[2];
        assert_eq!((material, labor, overhead), (dec("100"), dec("-50"), Decimal::ZERO));
        assert_eq!(direction_of(material), DIRECTION_UNFAVORABLE);
        assert_eq!(direction_of(labor), DIRECTION_FAVORABLE);
        assert_eq!(direction_of(overhead), DIRECTION_EVEN);
        assert_eq!(material + labor + overhead, calc.amount(), "要素相加必须等于总差异");
    }

    // ---------- 边界 ----------

    #[test]
    fn zero_quantity_never_divides_by_zero() {
        let row = core(
            Some(standard(None, None, None)),
            Some(actual(Decimal::ZERO, dec("500"), dec("200"), dec("300"), Decimal::ZERO)),
        );
        let calc = calc_of(&row);
        assert_eq!(
            calc.act_unit,
            [Decimal::ZERO; 3],
            "完工 0 ⇒ 单位成本 0（不是 panic / NaN）"
        );
        assert_eq!(calc.std_total(), Decimal::ZERO, "标准应发生 = 单位 × 0");
        assert_eq!(calc.state, STATE_NO_ACTUAL);
        assert!(!calc.comparable());
    }

    #[test]
    fn missing_actual_side_is_all_zero_and_not_comparable() {
        let row = core(Some(standard(None, None, None)), None);
        let calc = calc_of(&row);
        assert_eq!(calc.completed_qty, Decimal::ZERO);
        assert_eq!(calc.act_total(), Decimal::ZERO);
        assert_eq!(calc.std_total(), Decimal::ZERO);
        assert!(!calc.comparable());
    }

    #[test]
    fn missing_standard_side_keeps_actual_amount_but_stays_incomparable() {
        let row = core(
            None,
            Some(actual(dec("10"), dec("100"), dec("40"), dec("60"), dec("200"))),
        );
        let calc = calc_of(&row);
        assert_eq!(calc.act_total(), dec("200"));
        assert_eq!(calc.act_unit_total(), dec("20"));
        assert_eq!(calc.std_total(), Decimal::ZERO, "缺标准 ⇒ 标准应发生 0（不猜）");
        assert!(!calc.comparable());
    }

    #[test]
    fn rounding_diff_is_book_minus_element_sum() {
        // 计算单账上 22311.1104，三要素相加 22311.1103 ⇒ 尾差 0.0001（对平用，不是差异）
        let row = core(
            Some(standard(None, None, None)),
            Some(actual(
                dec("800"),
                dec("10000.0000"),
                dec("4000.0000"),
                dec("8311.1103"),
                dec("22311.1104"),
            )),
        );
        let calc = calc_of(&row);
        assert_eq!(calc.act_total(), dec("22311.1103"));
        assert_eq!(calc.rounding_diff(), dec("0.0001"));
    }

    // ---------- 合计 ----------

    #[test]
    fn totals_only_fold_comparable_rows_into_variance() {
        let both = calc_of(&core(
            Some(standard(None, None, None)),
            Some(actual(dec("100"), dec("1100"), dec("400"), dec("600"), dec("2100"))),
        ));
        let favorable = calc_of(&core(
            Some(standard(None, None, None)),
            Some(actual(dec("50"), dec("400"), dec("200"), dec("300"), dec("900"))),
        ));
        let no_std = calc_of(&core(
            None,
            Some(actual(dec("10"), dec("100"), dec("40"), dec("60"), dec("200"))),
        ));
        let no_act = calc_of(&core(Some(standard(None, None, None)), None));
        let totals = totals_of(&[both, favorable, no_std, no_act]);

        assert_eq!(totals.rows, 4);
        assert_eq!(totals.comparable_rows, 2);
        assert_eq!(totals.no_standard_rows, 1);
        assert_eq!(totals.no_actual_rows, 1);
        assert_eq!(totals.completed_qty, dec("150"));
        assert_eq!(totals.std_total(), dec("3000"), "2000 + 1000");
        assert_eq!(totals.act_total(), dec("3000"), "2100 + 900");
        assert_eq!(totals.amount(), Decimal::ZERO);
        assert_eq!(totals.rate(), Some(Decimal::ZERO));
        assert_eq!(
            totals.no_standard_amount,
            dec("200"),
            "缺标准的实际成本单列，不混进差异"
        );
    }

    #[test]
    fn empty_totals_have_no_rate() {
        let totals = totals_of(&[]);
        assert_eq!(totals.rows, 0);
        assert_eq!(totals.rate(), None, "没有可比行时差异率是无从比较");
        assert_eq!(totals.direction(), DIRECTION_EVEN);
    }

    #[test]
    fn totals_rate_is_total_over_total_not_average_of_rates() {
        // 两行差异率 +50%（100/200）与 −10%（−100/1000）⇒ 合计 0 / 1200 = 0%
        let big = calc_of(&core(
            Some(standard(Some(dec("2")), Some(Decimal::ZERO), Some(Decimal::ZERO))),
            Some(actual(dec("100"), dec("300"), Decimal::ZERO, Decimal::ZERO, dec("300"))),
        ));
        let small = calc_of(&core(
            Some(standard(Some(dec("10")), Some(Decimal::ZERO), Some(Decimal::ZERO))),
            Some(actual(dec("100"), dec("900"), Decimal::ZERO, Decimal::ZERO, dec("900"))),
        ));
        let totals = totals_of(&[big, small]);
        assert_eq!(totals.amount(), Decimal::ZERO);
        assert_eq!(totals.rate(), Some(Decimal::ZERO), "合计口径 = 总差额 / 总标准额");
    }

    // ---------- 出参装配 ----------

    #[test]
    fn row_view_exposes_both_sides_and_null_variance_when_incomparable() {
        let comparable = core(
            Some(standard(None, None, None)),
            Some(actual(dec("100"), dec("1100"), dec("400"), dec("800"), dec("2300"))),
        );
        let view = row_view(&comparable, "2026-11");
        assert_eq!(view.state, STATE_BOTH);
        assert_eq!(view.state_label, "可比");
        assert!(view.standard.is_some());
        assert!(view.actual.is_some());
        let variance = view.variance.as_ref().expect("可比行必须有差异块");
        assert_eq!(variance.amount, "300.0000");
        assert_eq!(variance.rate.as_deref(), Some("0.150000"));
        assert_eq!(variance.elements.len(), 3, "三要素都要出");
        assert_eq!(variance.elements[0].element, ELEMENT_MATERIAL);
        assert_eq!(variance.elements[0].element_label, "材料");
        assert!(view.hint.is_empty(), "无可挑剔的可比行不该有提示：{}", view.hint);

        let no_standard = core(
            None,
            Some(actual(dec("10"), dec("100"), dec("40"), dec("60"), dec("200"))),
        );
        let view = row_view(&no_standard, "2026-11");
        assert_eq!(view.state, STATE_NO_STANDARD);
        assert!(view.standard.is_none());
        assert!(view.variance.is_none(), "不可比不给差异块（避免 0 被当成「完全达标」）");
        assert!(view.hint.contains("没有标准成本卡"), "{}", view.hint);
    }

    #[test]
    fn numbers_are_rendered_with_fixed_scale() {
        let row = core(
            Some(standard(None, None, None)),
            Some(actual(dec("3"), dec("10"), dec("1"), dec("1"), dec("12"))),
        );
        let view = row_view(&row, "2026-11");
        let actual = view.actual.expect("有实际侧");
        assert_eq!(actual.completed_qty, "3.0000");
        assert_eq!(actual.unit_material, "3.333333", "单位成本 6 位");
        assert_eq!(actual.amount_total, "12.0000", "金额 4 位");
        let standard_view = view.standard.expect("有标准侧");
        assert_eq!(standard_view.std_total_cost, "20.000000");
        assert_eq!(standard_view.std_labor_hours, "0.5000", "工时 4 位");
    }

    // ---------- 提示（必须给动作） ----------

    #[test]
    fn hint_for_missing_standard_distinguishes_unpublished_from_absent() {
        let mut row = core(
            None,
            Some(actual(dec("10"), dec("100"), dec("40"), dec("60"), dec("200"))),
        );
        row.unpublished_versions = 2;
        let hint = hint_of(&row, &calc_of(&row), "2026-11");
        assert!(hint.contains("还没发布"), "{hint}");
        assert!(hint.contains("2 个"), "要说出还有几个未发布版本：{hint}");
        assert!(hint.contains("提交发布审批"), "要给出具体动作：{hint}");

        row.unpublished_versions = 0;
        let hint = hint_of(&row, &calc_of(&row), "2026-11");
        assert!(hint.contains("没有标准成本卡"), "{hint}");
        assert!(hint.contains("建一版"), "要给出具体动作：{hint}");
    }

    #[test]
    fn hint_for_missing_actual_points_at_cost_calc_or_wip() {
        let row = core(Some(standard(None, None, None)), None);
        let hint = hint_of(&row, &calc_of(&row), "2026-11");
        assert!(hint.contains("2026-11"), "要点名期间：{hint}");
        assert!(hint.contains("成本核算"), "要给出具体动作：{hint}");

        let wip = core(
            Some(standard(None, None, None)),
            Some(actual(Decimal::ZERO, dec("500"), dec("200"), dec("300"), Decimal::ZERO)),
        );
        let hint = hint_of(&wip, &calc_of(&wip), "2026-11");
        assert!(hint.contains("完工数量为 0"), "{hint}");
        assert!(hint.contains("1000.0000"), "要说清本期投入多少：{hint}");
    }

    #[test]
    fn hint_for_comparable_row_flags_version_switch_and_rounding() {
        let mut side = standard(None, None, None);
        side.switch_in_period = true;
        let row = core(
            Some(side),
            Some(actual(
                dec("100"),
                dec("1000"),
                dec("400"),
                dec("600.0001"),
                dec("2000.0003"),
            )),
        );
        let hint = hint_of(&row, &calc_of(&row), "2026-11");
        assert!(hint.contains("换过标准成本版本"), "{hint}");
        assert!(hint.contains("四位小数取舍"), "尾差必须解释：{hint}");
        assert!(hint.contains("不是差异"), "{hint}");
    }

    // ---------- 值域文本 ----------

    #[test]
    fn labels_cover_the_documented_value_domains() {
        for (state, label) in [
            (STATE_BOTH, "可比"),
            (STATE_NO_STANDARD, "缺标准"),
            (STATE_NO_ACTUAL, "缺实际"),
        ] {
            assert_eq!(state_label(state), label);
        }
        for (element, label) in [
            (ELEMENT_MATERIAL, "材料"),
            (ELEMENT_LABOR, "人工"),
            (ELEMENT_OVERHEAD, "制造费用"),
        ] {
            assert_eq!(element_label(element), label);
        }
        for (direction, label) in [
            (DIRECTION_FAVORABLE, "有利"),
            (DIRECTION_UNFAVORABLE, "不利"),
            (DIRECTION_EVEN, "持平"),
        ] {
            assert_eq!(direction_label(direction), label);
        }
        // 与 `biz_cost_variance.element` / `variance_type` 的值域同字面值（库层 CHECK 依赖它）
        assert_eq!(ELEMENTS, ["MATERIAL", "LABOR", "OVERHEAD"]);
    }
}

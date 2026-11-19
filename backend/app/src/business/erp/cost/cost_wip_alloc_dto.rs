//! 在制品「料工费在完工与在制之间分配」的**契约层与纯逻辑**（成本核算 · M5-22）。
//!
//! # 这张表算什么（约当产量法）
//!
//! ```text
//! 每个成本对象、每个要素（料 / 工 / 费）三列钱：
//!   池    = 期初在制（按上期单位成本还原到要素）+ 本期投入（biz_cost_collect.element）
//!   分母  = 材料 → 材料约当（EQ_mat）；人工 / 制造费用 → 加工约当（EQ_cnv）
//!   分配率 = 池 / 分母                       （6 位，与计算单 unit_* 同标度）
//!   完工   = 本期完工数量 × 分配率            （4 位；本期在制为 0 时 = 整池，尾差进完工）
//!   在制   = 池 − 完工                        （4 位，差额倒挤 —— 三要素合计守恒）
//! ```
//!
//! 约当产量**不在这里另写一份**：直接调 [`equiv::equiv_of`]（M5-20 的同一段代码，
//! 它自己又复用 calc 第 6 段的 `equivalents`）。否则「表里的约当」与「计算单的约当」
//! 迟早差一位，而这张表的价值就是**把分母和分子的钱对上**。
//!
//! 分配率的分母刻意用**未舍入**的约当（`equivalents` 的原值，与 calc 的
//! `unit_* = 池 / eq` 完全同源），表里显示的约当按 4 位（与 `equiv_*` 落库列同标度）。
//! 所以拿「显示值」回算分配率可能在**第 6 位**差 1 —— 这是 calc 本身的口径，
//! 不是本表多出来的误差；页脚把这句话写出来。
//!
//! # 不画假 0
//!
//! 算不出来（缺完工率 / 无产量 / 投料方式不支持 / 期初还原失败 / 出现负数分配）
//! 的行，金额列一律 `null`（**不是 0**）：写 0 会被读成「这行没有成本」，
//! 而真相是「少一个输入」。问题码与「怎么修」逐条随行返回。
//!
//! # 与本期计算单对拍
//!
//! 计算单（`biz_cost_period_item`）是执行 `POST /cost-period/calc` 那一刻的快照。
//! 本表按**当前**的成本对象数量与本期归集重算一遍，逐字段比：完工数量 / 在制数量 /
//! 完工率 / 两个约当 / 三个单位成本 / 期初在制成本 / 完工转出 / 期末在制。
//! 任一不等 ⇒ `recalc_required = true` + `drift[]` 给「表里 X / 计算单 Y」，
//! `state=drift` 可一屏筛出。
//!
//! # 与其它卡的分工（本卡不越界）
//!
//! * **M5-20 约当产量表**：只出分母（本表吃它，不重复算）；
//! * **M5-21 完工率录入（按工序）**：对象的完工率从哪来（本表只读汇总后的对象级值）；
//! * **M5-23 期末在制余额**：本表已经把「在制」那一列算出来了，M5-23 若要做余额台账
//!   应当消费同一口径（不另起公式）；
//! * **M5-24 在制品单测 / M5-25 前端页**：本卡只出 API 与单测。
//!
//! # 只读
//!
//! 零写库、零迁移、零新增权限码（复用 `business:erp:cost-object:list`，与
//! `cost-wip-equiv` / `cost-collect-sheet` / `wip-balance` 同码：看得到成本对象的人
//! 就是该看这张表的人）。因此也没有「事务提交后失败留死单」的面（AGENTS 约定 11）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::erp::cost::cost_period_item_dto;
use crate::business::erp::cost::cost_wip_equiv_dto::{
    self as equiv, DriftField, EquivFacts, EquivIssue, EquivState, IssueView, RowHead,
};
// 出参形状**复用** M5-20 的两个小结构（下拉项 / 批次快照）：前端一页两表不用两套组件。
pub use crate::business::erp::cost::cost_wip_equiv_dto::{BatchView, OptionView};
use cloudpivot_kernel::shared::error_code;

// ==================== 权限码（复用 cost-object:list） ====================

/// 表读取（**复用**成本对象列表的码：零新增码，菜单里已有承载行）。
pub const PERM_LIST: &str = "business:erp:cost-object:list";

/// 本模块全部权限码（与其它成本模块 `all_permissions()` 同形）。
pub const PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 口径文案（前端页脚直接用，别在前端手抄） ====================

/// 池的构成。
pub const FORMULA_POOL: &str = "成本池 = 期初在制（上期单位成本还原到料/工/费）+ 本期投入（归集明细按 element 分类）";

/// 分配率的分母。
pub const FORMULA_RATE: &str =
    "分配率 = 成本池 ÷ 约当产量（材料用材料约当，人工与制造费用用加工约当；6 位，与计算单 unit_* 同源）";

/// 完工 / 在制的拆分。
pub const FORMULA_SPLIT: &str =
    "完工 = 本期完工数量 × 分配率；在制 = 池 − 完工（差额倒挤，三要素合计守恒）；本期在制为 0 时尾差全部进完工";

/// 分配率小数位（与计算单 `unit_*` 的 `NUMERIC(18,6)` 一致）。
pub const RATE_SCALE: u32 = cost_period_item_dto::UNIT_SCALE;

/// 金额小数位（与计算单金额列 `NUMERIC(18,4)` 一致）。
pub const MONEY_SCALE: u32 = cost_period_item_dto::MONEY_SCALE;

// ==================== 要素 ====================

/// 三个成本要素（表的列分组、分配率的分母选择都由它决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element {
    /// 材料（分母 = 材料约当：开工一次投入 ⇒ 在制按 100%）。
    Material,
    /// 人工（分母 = 加工约当）。
    Labor,
    /// 制造费用（分母 = 加工约当；含分摊转入的 ALLOC 行）。
    Overhead,
}

impl Element {
    /// 固定顺序（出参列顺序、合计顺序）。材料在前：材料占比最大且口径最简单。
    pub const ALL: [Element; 3] = [Element::Material, Element::Labor, Element::Overhead];

    /// 归集明细里 `element` 列的取值（`biz_cost_collect.element`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Element::Material => "MATERIAL",
            Element::Labor => "LABOR",
            Element::Overhead => "OVERHEAD",
        }
    }

    /// 中文名（表头直接用）。
    pub fn label(&self) -> &'static str {
        match self {
            Element::Material => "材料",
            Element::Labor => "人工",
            Element::Overhead => "制造费用",
        }
    }

    /// 这一要素用哪个分母（`material` / `conversion`，与 M5-20 的约当口径对齐）。
    pub fn equiv_kind(&self) -> &'static str {
        match self {
            Element::Material => "material",
            Element::Labor | Element::Overhead => "conversion",
        }
    }

    /// 在三个要素里的下标（0 / 1 / 2）。
    pub fn index(&self) -> usize {
        match self {
            Element::Material => 0,
            Element::Labor => 1,
            Element::Overhead => 2,
        }
    }

    /// 这一要素的分母是否按材料口径（在制 100%）。
    pub fn uses_material_equiv(&self) -> bool {
        matches!(self, Element::Material)
    }
}

// ==================== 输入事实 ====================

/// 上期**已结算**计算单里当「期初」用的那几列（设计 §4.6.3）。
#[derive(Debug, Clone, PartialEq)]
pub struct PriorFacts {
    /// 上期期间（报文里点名「期初来自哪一期」）。
    pub period_code: String,
    /// 上期期末在制数量。
    pub wip_end_qty: Decimal,
    /// 上期期末在制成本。
    pub wip_end_cost: Decimal,
    /// 上期期末在制完工进度 %（还原人工要用）。
    pub completion_pct: Decimal,
    /// 上期单位材料成本（18,6）。
    pub unit_material: Decimal,
    /// 上期单位人工成本（18,6）。
    pub unit_labor: Decimal,
    /// 上期单位制造费用（18,6）。
    pub unit_overhead: Decimal,
}

impl PriorFacts {
    /// → calc 的期初还原入参（**不另写一套还原算式**）。
    pub fn prior_item(&self) -> cost_period_item_dto::PriorItem {
        cost_period_item_dto::PriorItem {
            period_code: self.period_code.clone(),
            wip_end_qty: self.wip_end_qty,
            wip_end_cost: self.wip_end_cost,
            completion_pct: self.completion_pct,
            unit_material: self.unit_material,
            unit_labor: self.unit_labor,
            unit_overhead: self.unit_overhead,
        }
    }
}

/// 本期计算单快照（没有批次 / 没算过 / 该对象没算出计算单 ⇒ `None`）。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredItem {
    /// 计算单行状态（`CALC` / `SETTLED`）。
    pub status: String,
    /// 计算单里的期初在制成本。
    pub wip_beg_cost: Decimal,
    /// 计算单里的本期完工数量。
    pub completed_qty: Decimal,
    /// 计算单里的期末在制数量。
    pub wip_end_qty: Decimal,
    /// 计算单里的完工率。
    pub completion_pct: Decimal,
    /// 计算单里的材料约当。
    pub equiv_material: Decimal,
    /// 计算单里的加工约当。
    pub equiv_conversion: Decimal,
    /// 计算单里的单位材料成本。
    pub unit_material: Decimal,
    /// 计算单里的单位人工成本。
    pub unit_labor: Decimal,
    /// 计算单里的单位制造费用。
    pub unit_overhead: Decimal,
    /// 计算单里的完工转出成本。
    pub finished_cost: Decimal,
    /// 计算单里的期末在制成本。
    pub wip_end_cost: Decimal,
}

impl StoredItem {
    /// 这一行是否已结算（已结算的漂移只能红冲重算，不能直接重算 CALC）。
    pub fn settled(&self) -> bool {
        self.status == "SETTLED"
    }
}

/// 分一行钱所需的**全部**事实（纯数据，零 DB）。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocFacts {
    /// 成本对象号（报文点名用）。
    pub object_no: String,
    /// 投料方式（一期只有 `ONE_TIME` 参与材料约当=100%）。
    pub material_input_mode: String,
    /// 累计完工数量（`biz_cost_object.completed_qty`）。
    pub cumulative_completed_qty: Decimal,
    /// 前期**已结算**计算单的完工合计（本期完工 = 累计 − 它）。
    pub prior_completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 期末在制完工率 %（0–100）。
    pub completion_pct: Decimal,
    /// 本期材料投入（Σ collect element='MATERIAL'）。
    pub mat_cur: Decimal,
    /// 本期人工投入（Σ collect element='LABOR'）。
    pub labor_cur: Decimal,
    /// 本期制造费用投入（Σ collect element='OVERHEAD'）。
    pub oh_cur: Decimal,
    /// 上期已结算计算单（期初来源；首期 ⇒ `None`）。
    pub prior: Option<PriorFacts>,
    /// 本期计算单快照（无批次 / 没算过 ⇒ `None`）。
    pub stored: Option<StoredItem>,
}

impl AllocFacts {
    /// 本期投入合计（料 + 工 + 费）。
    pub fn cur_total(&self) -> Decimal {
        self.mat_cur + self.labor_cur + self.oh_cur
    }

    /// 本期投入（按下标取，避免三处写 `match`）。
    pub fn cur_of(&self, element: Element) -> Decimal {
        match element {
            Element::Material => self.mat_cur,
            Element::Labor => self.labor_cur,
            Element::Overhead => self.oh_cur,
        }
    }

    /// → M5-20 的约当事实（同一段代码算分母）。
    pub fn equiv_facts(&self) -> EquivFacts {
        EquivFacts {
            object_no: self.object_no.clone(),
            material_input_mode: self.material_input_mode.clone(),
            cumulative_completed_qty: self.cumulative_completed_qty,
            prior_completed_qty: self.prior_completed_qty,
            wip_qty: self.wip_qty,
            completion_pct: self.completion_pct,
            collected_total: self.cur_total(),
            prior_wip_end_qty: self.prior.as_ref().map(|prior| prior.wip_end_qty).unwrap_or_default(),
            stored: None,
        }
    }
}

/// 期初在制按要素的拆分（复用 calc 的还原口径）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BegElements {
    /// 材料部分。
    pub material: Decimal,
    /// 人工部分。
    pub labor: Decimal,
    /// 制造费用部分（含还原尾差）。
    pub overhead: Decimal,
    /// 合计（= 上期期末在制成本，一分不差）。
    pub total: Decimal,
}

impl BegElements {
    /// 按要素取。
    pub fn get(&self, element: Element) -> Decimal {
        match element {
            Element::Material => self.material,
            Element::Labor => self.labor,
            Element::Overhead => self.overhead,
        }
    }
}

/// 期初还原：直接调 calc 的 [`cost_period_item_dto::beg_cost_of`]。
///
/// 还原不出来（上期单位成本为负 ⇒ 材料 / 人工算出负数）时给一条问题码，
/// 而不是把负数带进后面的分配（带进去会让「完工 + 在制 = 池」出现负金额）。
pub fn beg_elements(prior: Option<&PriorFacts>) -> std::result::Result<BegElements, EquivIssue> {
    let Some(prior) = prior else {
        return Ok(BegElements::default());
    };
    match cost_period_item_dto::beg_cost_of(&prior.prior_item()) {
        Ok(beg) => Ok(BegElements {
            material: beg.material,
            labor: beg.labor,
            overhead: beg.overhead,
            total: beg.total,
        }),
        Err(error) => Err(EquivIssue {
            code: "BEG_COST_INVALID",
            message: format!("期初在制还原失败（来源 {}）：{}", prior.period_code, error.msg),
            fix: "核对上期已结算计算单的单位成本（材料 / 人工不应为负）；上期确实是错的，先红冲上期批次再重算本期"
                .to_string(),
        }),
    }
}

// ==================== 一行的分配结果 ====================

/// 一个要素的分配（六个金额 + 一个分母）。
#[derive(Debug, Clone, PartialEq)]
pub struct ElementAlloc {
    /// 要素。
    pub element: Element,
    /// 期初在制（还原后）。
    pub beg: Option<Decimal>,
    /// 本期投入。
    pub cur: Decimal,
    /// 成本池（= 期初 + 投入）。
    pub pool: Option<Decimal>,
    /// 分母（约当产量，4 位显示；算不出来 ⇒ `None`）。
    pub equiv: Option<Decimal>,
    /// 分配率（6 位；池为空 / 分母不可用 ⇒ `None`）。
    pub rate: Option<Decimal>,
    /// 完工分配（4 位）。
    pub finished: Option<Decimal>,
    /// 期末在制分配（4 位；= 池 − 完工）。
    pub wip: Option<Decimal>,
}

impl ElementAlloc {
    /// 本要素守恒（`完工 + 在制 = 池`）；算不出来的要素不参与。
    pub fn balanced(&self) -> bool {
        match (self.pool, self.finished, self.wip) {
            (Some(pool), Some(finished), Some(wip)) => finished + wip == pool,
            _ => true,
        }
    }
}

/// 一行的分配结果（含状态 / 问题 / 警告 / 与计算单的差异）。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocCalc {
    /// 本期完工数量（= 累计 − 前期已结算）。
    pub completed_qty: Decimal,
    /// 期末在制数量。
    pub wip_qty: Decimal,
    /// 期末在制完工率 %。
    pub completion_pct: Decimal,
    /// 材料约当（4 位）。
    pub equiv_material: Option<Decimal>,
    /// 加工约当（4 位）。
    pub equiv_conversion: Option<Decimal>,
    /// 期初在制成本合计。
    pub beg_total: Option<Decimal>,
    /// 本期投入合计。
    pub cur_total: Decimal,
    /// 成本池合计。
    pub pool_total: Option<Decimal>,
    /// 完工分配合计。
    pub finished_total: Option<Decimal>,
    /// 期末在制分配合计。
    pub wip_total: Option<Decimal>,
    /// 完工单位成本（= 完工合计 ÷ 完工数量，6 位）。
    pub unit_finished_cost: Option<Decimal>,
    /// 三个要素（固定顺序）。
    pub elements: Vec<ElementAlloc>,
    /// 行形状。
    pub state: EquivState,
    /// 算不出来的原因（`state = blocked` 时非空）。
    pub issues: Vec<EquivIssue>,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
    /// 表值与本期计算单不一致 ⇒ 需要重算。
    pub recalc_required: bool,
    /// 具体差在哪些字段。
    pub drift: Vec<DriftField>,
}

impl AllocCalc {
    /// 取某要素的分配。
    pub fn element_of(&self, element: Element) -> &ElementAlloc {
        &self.elements[element.index()]
    }

    /// 是否分得干净（每个要素都守恒）。
    pub fn balanced(&self) -> bool {
        self.elements.iter().all(ElementAlloc::balanced)
    }

    /// 第一类问题码（报文 / 汇总用）。
    pub fn first_issue(&self) -> Option<&'static str> {
        self.issues.first().map(|issue| issue.code)
    }
}

// ==================== 算数 ====================

/// 除法（分母 0 ⇒ 0，与 calc 的 `div_or_zero` 同口径）。
fn div_or_zero(numerator: Decimal, denominator: Decimal) -> Decimal {
    if denominator.is_zero() {
        Decimal::ZERO
    } else {
        numerator / denominator
    }
}

/// 分一行钱（**纯函数**：不连库、不读时钟、不写日志）。
pub fn alloc_of(facts: &AllocFacts) -> AllocCalc {
    // ---- ① 分母：M5-20 的同一段代码（它内部又复用 calc 第 6 段）----
    let eq = equiv::equiv_of(&facts.equiv_facts());
    let mut issues = eq.issues.clone();
    let mut warnings = eq.warnings.clone();

    // ---- ② 期初（按要素）----
    let beg = match beg_elements(facts.prior.as_ref()) {
        Ok(beg) => Some(beg),
        Err(issue) => {
            issues.push(issue);
            None
        }
    };

    // ---- ③ 未舍入分母（与 calc 的 `unit_*` 同一分母；显示值另按 4 位）----
    let (eq_material_raw, eq_conversion_raw) =
        cost_period_item_dto::equivalents(eq.completed_qty, facts.wip_qty, facts.completion_pct);

    // ---- ④ 逐要素组装（先把「事实」摆齐，再决定分不分得出来）----
    let mut elements: Vec<ElementAlloc> = Vec::with_capacity(3);
    let mut blocked = !eq.complete() || beg.is_none();
    for element in Element::ALL {
        let eq_shown = if element.uses_material_equiv() {
            eq.equiv_material
        } else {
            eq.equiv_conversion
        };
        let cur = facts.cur_of(element);
        let beg_el = beg.map(|beg| beg.get(element));
        let pool = beg_el.map(|beg| beg + cur);
        elements.push(ElementAlloc {
            element,
            beg: beg_el,
            cur,
            pool,
            equiv: eq_shown,
            rate: None,
            finished: None,
            wip: None,
        });
    }

    // ---- ⑤ 能算则算：分配率 → 完工 → 在制 ----
    if !blocked {
        for item in &mut elements {
            let eq_raw = if item.element.uses_material_equiv() {
                eq_material_raw
            } else {
                eq_conversion_raw
            };
            let Some(pool) = item.pool else {
                blocked = true;
                continue;
            };
            if eq_raw <= Decimal::ZERO {
                // 理论上到不了（eq.complete() 为真 + 有池 ⇒ 分母 > 0），防御性拦下：
                // 分母 0 时给 0 分配率会把整池悄悄算成在制，读的人看不出是错的。
                issues.push(EquivIssue {
                    code: "ZERO_EQUIV",
                    message: format!(
                        "{} 的约当产量是 0，但成本池是 {}",
                        item.element.label(),
                        money_text(pool)
                    ),
                    fix: "核对本期完工数量与期末在制数量（两者都为 0 时不该有归集成本）".to_string(),
                });
                blocked = true;
                continue;
            }
            let rate = cost_period_item_dto::unit_cost(div_or_zero(pool, eq_raw));
            // 本期无在制 ⇒ 整池进完工（尾差进完工，与 calc 第 6 段同一分支）
            let finished = if facts.wip_qty.is_zero() {
                cost_period_item_dto::money(pool)
            } else {
                cost_period_item_dto::money(eq.completed_qty * rate)
            };
            let wip = cost_period_item_dto::money(pool - finished);
            if rate < Decimal::ZERO || finished < Decimal::ZERO || wip < Decimal::ZERO {
                issues.push(EquivIssue {
                    code: "NEGATIVE_ALLOC",
                    message: format!(
                        "{} 分出了负数（池 {} / 分配率 {} → 完工 {}、在制 {}）：本期投入或期初可能是红字 / 负数",
                        item.element.label(),
                        money_text(pool),
                        rate_text(rate),
                        money_text(finished),
                        money_text(wip)
                    ),
                    fix: "核对本期该要素的归集明细（红字冲销是否冲过头）与上期单位成本；修好后重算".to_string(),
                });
                blocked = true;
                continue;
            }
            item.rate = Some(rate);
            item.finished = Some(finished);
            item.wip = Some(wip);
        }
    }

    // ---- ⑥ 汇总 ----
    let beg_total = beg.map(|beg| cost_period_item_dto::money(beg.total));
    let cur_total = facts.cur_total();
    let pool_total = beg_total.map(|beg| beg + cur_total);
    let (finished_total, wip_total) = if blocked {
        (None, None)
    } else {
        let finished: Decimal = elements.iter().filter_map(|item| item.finished).sum();
        let wip: Decimal = elements.iter().filter_map(|item| item.wip).sum();
        (Some(finished), Some(wip))
    };
    let unit_finished_cost = match finished_total {
        Some(finished) if eq.completed_qty > Decimal::ZERO => {
            Some(cost_period_item_dto::unit_cost(div_or_zero(finished, eq.completed_qty)))
        }
        _ => None,
    };

    // ---- ⑦ 只提示不拦 ----
    if !blocked {
        if eq.completed_qty.is_zero() && facts.wip_qty > Decimal::ZERO {
            warnings.push(format!(
                "本期没有完工：整池 {} 全部留在期末在制（在制约当 {}）",
                pool_total.map(money_text).unwrap_or_else(|| "—".to_string()),
                if facts.completion_pct.is_zero() {
                    "—".to_string()
                } else {
                    qty_text(facts.wip_qty)
                }
            ));
        }
        if facts.wip_qty.is_zero() {
            warnings.push("本期全部完工（期末在制为 0）：所有成本含尾差全部进完工，在制分配为 0".to_string());
        }
        if pool_total == Some(Decimal::ZERO) && (!eq.completed_qty.is_zero() || facts.wip_qty > Decimal::ZERO) {
            warnings.push("本期没有可分配的成本（期初在制 0 + 本期投入 0）：分配额是 0，不是算不出来".to_string());
        }
    }

    // ---- ⑧ 与本期计算单对拍 ----
    let drift = match (&facts.stored, blocked) {
        (Some(stored), false) => drift_of(facts, &eq, beg_total, finished_total, wip_total, &elements, stored),
        _ => Vec::new(),
    };
    let recalc_required = !drift.is_empty();

    // ---- ⑨ 状态 ----
    let state = if blocked {
        EquivState::Blocked
    } else if facts.wip_qty > Decimal::ZERO {
        EquivState::Wip
    } else {
        EquivState::Finished
    };

    // 分不出来时：金额列一律清空（不画假 0），分母与池照留（它们是事实）
    if blocked {
        for item in &mut elements {
            item.rate = None;
            item.finished = None;
            item.wip = None;
        }
    }

    AllocCalc {
        completed_qty: eq.completed_qty,
        wip_qty: facts.wip_qty,
        completion_pct: facts.completion_pct,
        equiv_material: eq.equiv_material,
        equiv_conversion: eq.equiv_conversion,
        beg_total,
        cur_total,
        pool_total,
        finished_total,
        wip_total,
        unit_finished_cost,
        elements,
        state,
        issues,
        warnings,
        recalc_required,
        drift,
    }
}

/// 表值 vs 计算单：逐字段对比（数量 / 约当 4 位，单位成本 6 位，金额 4 位）。
fn drift_of(
    facts: &AllocFacts,
    eq: &equiv::EquivCalc,
    beg_total: Option<Decimal>,
    finished_total: Option<Decimal>,
    wip_total: Option<Decimal>,
    elements: &[ElementAlloc],
    stored: &StoredItem,
) -> Vec<DriftField> {
    let mut drift = Vec::new();
    let mut push_if_diff = |field: &'static str, table_value: Decimal, calc_value: Decimal| {
        if table_value != calc_value {
            drift.push(DriftField {
                field,
                table_value,
                calc_value,
            });
        }
    };
    push_if_diff("本期完工数量", eq.completed_qty, stored.completed_qty);
    push_if_diff("期末在制数量", facts.wip_qty, stored.wip_end_qty);
    push_if_diff("完工率", facts.completion_pct, stored.completion_pct);
    if let Some(value) = eq.equiv_material {
        push_if_diff("材料约当", value, stored.equiv_material);
    }
    if let Some(value) = eq.equiv_conversion {
        push_if_diff("加工约当", value, stored.equiv_conversion);
    }
    if let Some(value) = beg_total {
        push_if_diff("期初在制成本", value, stored.wip_beg_cost);
    }
    let unit_of = |element: Element| elements[element.index()].rate;
    if let Some(value) = unit_of(Element::Material) {
        push_if_diff("单位材料成本", value, stored.unit_material);
    }
    if let Some(value) = unit_of(Element::Labor) {
        push_if_diff("单位人工成本", value, stored.unit_labor);
    }
    if let Some(value) = unit_of(Element::Overhead) {
        push_if_diff("单位制造费用", value, stored.unit_overhead);
    }
    if let Some(value) = finished_total {
        push_if_diff("完工转出成本", value, stored.finished_cost);
    }
    if let Some(value) = wip_total {
        push_if_diff("期末在制成本", value, stored.wip_end_cost);
    }
    drift
}

/// 漂移的一句话说明（前端 tooltip / 列表展开用）。
pub fn drift_hint(calc: &AllocCalc, stored: &StoredItem) -> String {
    if calc.drift.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = calc
        .drift
        .iter()
        .map(|item| {
            format!(
                "{}：表里 {} / 计算单 {}",
                item.field,
                field_text(item.field, item.table_value),
                field_text(item.field, item.calc_value)
            )
        })
        .collect();
    let tail = if stored.settled() {
        "该计算单已结算：改数量 / 完工率 / 归集后不能直接重算，需红冲本期批次再重算"
    } else {
        "重算本期（POST /business/erp/cost-period/calc）后两侧就会一致"
    };
    format!("与本期计算单不一致（{}）。{tail}", pairs.join("；"))
}

// ==================== 合计 ====================

/// 整张表的合计（**不是当前页**：翻页时这个数不能变）。
///
/// 金额合计**只加能算的行**：算不出来的行如果按 0 加进去，合计会悄悄变小 ——
/// 「池 > 完工 + 在制」看起来像丢了钱；只加一半（料加了、工费没加）又会给出
/// 一个读不出含义的数。整行跳过最诚实：合计旁边就写着「有 N 行算不出来」。
/// 数量合计（完工 / 在制）算所有行 —— 它们是事实，不因缺一个输入而消失。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllocTotals {
    /// 行数。
    pub row_count: u64,
    /// 有在制的行数。
    pub wip_row_count: u64,
    /// 全部完工的行数。
    pub finished_row_count: u64,
    /// 算不出来的行数。
    pub blocked_row_count: u64,
    /// 与计算单不一致的行数。
    pub drift_row_count: u64,
    /// 本期完工数量合计。
    pub completed_qty: Decimal,
    /// 期末在制数量合计。
    pub wip_qty: Decimal,
    /// 材料成本池合计。
    pub pool_material: Decimal,
    /// 人工成本池合计。
    pub pool_labor: Decimal,
    /// 制造费用成本池合计。
    pub pool_overhead: Decimal,
    /// 成本池合计。
    pub pool_total: Decimal,
    /// 完工分配合计（料 + 工 + 费）。
    pub finished_total: Decimal,
    /// 期末在制分配合计（料 + 工 + 费）。
    pub wip_total: Decimal,
}

impl AllocTotals {
    /// 累加一行。
    pub fn add(&mut self, calc: &AllocCalc) {
        self.row_count += 1;
        match calc.state {
            EquivState::Wip => self.wip_row_count += 1,
            EquivState::Finished => self.finished_row_count += 1,
            EquivState::Blocked => self.blocked_row_count += 1,
        }
        if calc.recalc_required {
            self.drift_row_count += 1;
        }
        self.completed_qty += calc.completed_qty;
        self.wip_qty += calc.wip_qty;
        if calc.state == EquivState::Blocked {
            return;
        }
        for item in &calc.elements {
            let pool = item.pool.unwrap_or_default();
            match item.element {
                Element::Material => self.pool_material += pool,
                Element::Labor => self.pool_labor += pool,
                Element::Overhead => self.pool_overhead += pool,
            }
        }
        self.pool_total += calc.pool_total.unwrap_or_default();
        self.finished_total += calc.finished_total.unwrap_or_default();
        self.wip_total += calc.wip_total.unwrap_or_default();
    }
}

// ==================== 入参 ====================

/// `GET /business/erp/cost-wip-alloc` 的入参。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct AllocQuery {
    /// 期间（必填，`yyyy-MM`）。
    pub period_code: Option<String>,
    /// 关键字（对象号 / 生产订单号 / 批号 / 产出品编码 / 产出品名称）。
    pub keyword: Option<String>,
    /// 状态筛选（`all` / `wip` / `finished` / `blocked` / `drift`；缺省 `all`）。
    pub state: Option<String>,
    /// 页码（缺省 1）。
    pub page: Option<u64>,
    /// 每页条数（缺省 20，上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的筛选（纯逻辑可单测）。
#[derive(Debug, Clone, PartialEq)]
pub struct AllocFilter {
    /// 期间编码。
    pub period_code: String,
    /// 关键字（已 trim，空串 ⇒ `None`）。
    pub keyword: Option<String>,
    /// 状态筛选。
    pub state: AllocStateFilter,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 状态筛选档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocStateFilter {
    /// 全部。
    All,
    /// 有在制（料工费分别在完工 / 在制两处）。
    Wip,
    /// 全部完工（在制 0，整池进完工）。
    Finished,
    /// 算不出来（金额为空）。
    Blocked,
    /// 与本期计算单不一致。
    Drift,
}

impl AllocStateFilter {
    /// 解析（拼错 ⇒ 400，不静默回落 `all`）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        match raw.map(str::trim).filter(|text| !text.is_empty()) {
            None => Ok(AllocStateFilter::All),
            Some("all") => Ok(AllocStateFilter::All),
            Some("wip") => Ok(AllocStateFilter::Wip),
            Some("finished") => Ok(AllocStateFilter::Finished),
            Some("blocked") => Ok(AllocStateFilter::Blocked),
            Some("drift") => Ok(AllocStateFilter::Drift),
            Some(other) => Err(bad_request(&format!(
                "state 只能是 all / wip / finished / blocked / drift，收到 {other}"
            ))),
        }
    }

    /// 回显值。
    pub fn as_str(&self) -> &'static str {
        match self {
            AllocStateFilter::All => "all",
            AllocStateFilter::Wip => "wip",
            AllocStateFilter::Finished => "finished",
            AllocStateFilter::Blocked => "blocked",
            AllocStateFilter::Drift => "drift",
        }
    }

    /// 中文名（下拉直接用）。
    pub fn label(&self) -> &'static str {
        match self {
            AllocStateFilter::All => "全部",
            AllocStateFilter::Wip => "有在制",
            AllocStateFilter::Finished => "全部完工",
            AllocStateFilter::Blocked => "算不出来",
            AllocStateFilter::Drift => "与计算单不一致",
        }
    }

    /// 是否命中一行。
    pub fn matches(&self, calc: &AllocCalc) -> bool {
        match self {
            AllocStateFilter::All => true,
            AllocStateFilter::Wip => calc.state == EquivState::Wip,
            AllocStateFilter::Finished => calc.state == EquivState::Finished,
            AllocStateFilter::Blocked => calc.state == EquivState::Blocked,
            AllocStateFilter::Drift => calc.recalc_required,
        }
    }
}

/// 全部筛选档（给前端下拉用；顺序即界面顺序）。
pub const STATE_FILTERS: [AllocStateFilter; 5] = [
    AllocStateFilter::All,
    AllocStateFilter::Wip,
    AllocStateFilter::Finished,
    AllocStateFilter::Blocked,
    AllocStateFilter::Drift,
];

// ==================== 出参 ====================

/// 一个要素的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ElementView {
    /// 要素码（`MATERIAL` / `LABOR` / `OVERHEAD`）。
    pub element: String,
    /// 中文名。
    pub label: String,
    /// 分母口径（`material` / `conversion`）。
    pub equiv_kind: String,
    /// 期初在制（`null` = 期初还原失败）。
    pub beg: Option<String>,
    /// 本期投入。
    pub cur: String,
    /// 成本池。
    pub pool: Option<String>,
    /// 约当产量（分母，4 位）。
    pub equiv: Option<String>,
    /// 分配率（6 位）。
    pub rate: Option<String>,
    /// 完工分配（4 位）。
    pub finished: Option<String>,
    /// 期末在制分配（4 位）。
    pub wip: Option<String>,
    /// 本要素守恒（`完工 + 在制 = 池`）。
    pub balanced: bool,
}

/// 一行（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AllocRow {
    /// 成本对象 id。
    pub object_id: String,
    /// 对象号。
    pub object_no: String,
    /// 对象类型（`BATCH` / …）。
    pub object_type: String,
    /// 生产订单号。
    pub source_doc_no: Option<String>,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: String,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 生产车间 id。
    pub workshop_dept_id: String,
    /// 生产车间名称。
    pub workshop_dept_name: Option<String>,
    /// 计划数量。
    pub plan_qty: String,
    /// 累计完工数量。
    pub cumulative_completed_qty: String,
    /// 前期已结算完工合计。
    pub prior_completed_qty: String,
    /// 本期完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 完工率 %。
    pub completion_pct: String,
    /// 材料约当。
    pub equiv_material: Option<String>,
    /// 加工约当。
    pub equiv_conversion: Option<String>,
    /// 期初在制成本。
    pub beg_total: Option<String>,
    /// 本期投入。
    pub cur_total: String,
    /// 成本池。
    pub pool_total: Option<String>,
    /// 完工分配合计。
    pub finished_total: Option<String>,
    /// 期末在制分配合计。
    pub wip_total: Option<String>,
    /// 完工单位成本（6 位）。
    pub unit_finished_cost: Option<String>,
    /// 三个要素（固定顺序：料 / 工 / 费）。
    pub elements: Vec<ElementView>,
    /// 行状态（`wip` / `finished` / `blocked`）。
    pub state: String,
    /// 行状态中文名。
    pub state_label: String,
    /// 需要重算（表值与本期计算单不一致）。
    pub recalc_required: bool,
    /// 期初来源（上期已结算计算单；首期 ⇒ `null`）。
    pub prior: Option<PriorView>,
    /// 本期计算单快照（没批次 / 没算过 ⇒ `null`）。
    pub calc: Option<AllocCalcView>,
    /// 算不出来的原因。
    pub issues: Vec<IssueView>,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
}

/// 期初来源（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PriorView {
    /// 上期期间。
    pub period_code: String,
    /// 上期期末在制数量。
    pub wip_end_qty: String,
    /// 上期期末在制成本。
    pub wip_end_cost: String,
    /// 上期单位材料成本。
    pub unit_material: String,
    /// 上期单位人工成本。
    pub unit_labor: String,
    /// 上期单位制造费用。
    pub unit_overhead: String,
}

/// 计算单快照（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AllocCalcView {
    /// 计算单状态（`CALC` / `SETTLED`）。
    pub status: String,
    /// 期初在制成本。
    pub wip_beg_cost: String,
    /// 完工数量。
    pub completed_qty: String,
    /// 期末在制数量。
    pub wip_end_qty: String,
    /// 完工率。
    pub completion_pct: String,
    /// 材料约当。
    pub equiv_material: String,
    /// 加工约当。
    pub equiv_conversion: String,
    /// 单位材料成本。
    pub unit_material: String,
    /// 单位人工成本。
    pub unit_labor: String,
    /// 单位制造费用。
    pub unit_overhead: String,
    /// 完工转出成本。
    pub finished_cost: String,
    /// 期末在制成本。
    pub wip_end_cost: String,
    /// 两侧一致（含单位成本与两个金额）。
    pub agrees: bool,
    /// 差异字段（`agrees = false` 时非空）。
    pub drift: Vec<equiv::DriftView>,
    /// 一句话说明（一致 / 怎么修）。
    pub note: String,
}

/// 合计（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AllocTotalsView {
    /// 行数。
    pub row_count: u64,
    /// 有在制的行数。
    pub wip_row_count: u64,
    /// 全部完工的行数。
    pub finished_row_count: u64,
    /// 算不出来的行数。
    pub blocked_row_count: u64,
    /// 与计算单不一致的行数。
    pub drift_row_count: u64,
    /// 本期完工数量合计。
    pub completed_qty: String,
    /// 期末在制数量合计。
    pub wip_qty: String,
    /// 材料成本池合计。
    pub pool_material: String,
    /// 人工成本池合计。
    pub pool_labor: String,
    /// 制造费用成本池合计。
    pub pool_overhead: String,
    /// 成本池合计。
    pub pool_total: String,
    /// 完工分配合计。
    pub finished_total: String,
    /// 期末在制分配合计。
    pub wip_total: String,
    /// 合计口径的一句话（**算不出来的行不参与金额合计**）。
    pub note: String,
}

/// 整张表的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AllocRes {
    /// 期间。
    pub period_code: String,
    /// 当期成本核算批次（没建 ⇒ `null`，表是试算视图）。
    pub batch: Option<BatchView>,
    /// 是否已建批次（决定 `calc` 一列有没有）。
    pub cost_ready: bool,
    /// 状态筛选回显。
    pub state: String,
    /// 关键字回显。
    pub keyword: Option<String>,
    /// 池的公式。
    pub formula_pool: String,
    /// 分配率的公式。
    pub formula_rate: String,
    /// 完工 / 在制拆分的公式。
    pub formula_split: String,
    /// 合计（整张表，不受分页影响）。
    pub totals: AllocTotalsView,
    /// 数据页。
    pub list: Vec<AllocRow>,
    /// 命中总行数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 可选筛档（前端下拉）。
    pub state_options: Vec<OptionView>,
    /// 页脚说明（含「没批次」「有算不出来的行」等提示）。
    pub note: String,
}

// ==================== 装配 ====================

/// 行 → 出参（纯函数：所有数字都已由 [`alloc_of`] 算好）。
pub fn row_view(head: &RowHead, facts: &AllocFacts, calc: &AllocCalc) -> AllocRow {
    AllocRow {
        object_id: head.object_id.clone(),
        object_no: head.object_no.clone(),
        object_type: head.object_type.clone(),
        source_doc_no: head.source_doc_no.clone(),
        lot_no: head.lot_no.clone(),
        product_id: head.product_id.clone(),
        product_code: head.product_code.clone(),
        product_name: head.product_name.clone(),
        workshop_dept_id: head.workshop_dept_id.clone(),
        workshop_dept_name: head.workshop_dept_name.clone(),
        plan_qty: head.plan_qty.clone(),
        cumulative_completed_qty: qty_text(facts.cumulative_completed_qty),
        prior_completed_qty: qty_text(facts.prior_completed_qty),
        completed_qty: qty_text(calc.completed_qty),
        wip_qty: qty_text(calc.wip_qty),
        completion_pct: pct_text(calc.completion_pct),
        equiv_material: calc.equiv_material.map(qty_text),
        equiv_conversion: calc.equiv_conversion.map(qty_text),
        beg_total: calc.beg_total.map(qty_text),
        cur_total: qty_text(calc.cur_total),
        pool_total: calc.pool_total.map(qty_text),
        finished_total: calc.finished_total.map(qty_text),
        wip_total: calc.wip_total.map(qty_text),
        unit_finished_cost: calc.unit_finished_cost.map(rate_text),
        elements: calc
            .elements
            .iter()
            .map(|item| ElementView {
                element: item.element.as_str().to_string(),
                label: item.element.label().to_string(),
                equiv_kind: item.element.equiv_kind().to_string(),
                beg: item.beg.map(qty_text),
                cur: qty_text(item.cur),
                pool: item.pool.map(qty_text),
                equiv: item.equiv.map(qty_text),
                rate: item.rate.map(rate_text),
                finished: item.finished.map(qty_text),
                wip: item.wip.map(qty_text),
                balanced: item.balanced(),
            })
            .collect(),
        state: calc.state.as_str().to_string(),
        state_label: calc.state.label().to_string(),
        recalc_required: calc.recalc_required,
        prior: facts.prior.as_ref().map(|prior| PriorView {
            period_code: prior.period_code.clone(),
            wip_end_qty: qty_text(prior.wip_end_qty),
            wip_end_cost: qty_text(prior.wip_end_cost),
            unit_material: rate_text(prior.unit_material),
            unit_labor: rate_text(prior.unit_labor),
            unit_overhead: rate_text(prior.unit_overhead),
        }),
        calc: facts.stored.as_ref().map(|stored| AllocCalcView {
            status: stored.status.clone(),
            wip_beg_cost: qty_text(stored.wip_beg_cost),
            completed_qty: qty_text(stored.completed_qty),
            wip_end_qty: qty_text(stored.wip_end_qty),
            completion_pct: pct_text(stored.completion_pct),
            equiv_material: qty_text(stored.equiv_material),
            equiv_conversion: qty_text(stored.equiv_conversion),
            unit_material: rate_text(stored.unit_material),
            unit_labor: rate_text(stored.unit_labor),
            unit_overhead: rate_text(stored.unit_overhead),
            finished_cost: qty_text(stored.finished_cost),
            wip_end_cost: qty_text(stored.wip_end_cost),
            agrees: !calc.recalc_required,
            drift: calc
                .drift
                .iter()
                .map(|item| equiv::DriftView {
                    field: item.field.to_string(),
                    table_value: field_text(item.field, item.table_value),
                    calc_value: field_text(item.field, item.calc_value),
                })
                .collect(),
            note: if calc.recalc_required {
                drift_hint(calc, stored)
            } else {
                "表值与本期计算单一致（同一组数量 / 完工率 / 投入算出的同一组分配）".to_string()
            },
        }),
        issues: calc
            .issues
            .iter()
            .map(|issue| IssueView {
                code: issue.code.to_string(),
                message: issue.message.clone(),
                fix: issue.fix.clone(),
            })
            .collect(),
        warnings: calc.warnings.clone(),
    }
}

/// 合计 → 出参（合计口径写在 `note` 里，读的人不用猜）。
pub fn totals_view(totals: &AllocTotals) -> AllocTotalsView {
    let note = if totals.blocked_row_count > 0 {
        format!(
            "金额合计不含 {} 行算不出来的行（它们的池与分配给空值，加 0 会让合计看起来像丢了钱）；\
             数量合计含全部行（完工 / 在制是事实）。材料池 + 人工池 + 制造费用池 = 成本池 = 完工 + 在制。",
            totals.blocked_row_count
        )
    } else {
        "金额合计含全部行；材料池 + 人工池 + 制造费用池 = 成本池 = 完工 + 在制。".to_string()
    };
    AllocTotalsView {
        row_count: totals.row_count,
        wip_row_count: totals.wip_row_count,
        finished_row_count: totals.finished_row_count,
        blocked_row_count: totals.blocked_row_count,
        drift_row_count: totals.drift_row_count,
        completed_qty: qty_text(totals.completed_qty),
        wip_qty: qty_text(totals.wip_qty),
        pool_material: qty_text(totals.pool_material),
        pool_labor: qty_text(totals.pool_labor),
        pool_overhead: qty_text(totals.pool_overhead),
        pool_total: qty_text(totals.pool_total),
        finished_total: qty_text(totals.finished_total),
        wip_total: qty_text(totals.wip_total),
        note,
    }
}

/// 筛档 → 出参。
pub fn state_options() -> Vec<OptionView> {
    STATE_FILTERS
        .iter()
        .map(|item| OptionView {
            value: item.as_str().to_string(),
            label: item.label().to_string(),
        })
        .collect()
}

// ==================== 文本 ====================

/// 数量 / 金额 → 文本（**4 位**，与库列 `NUMERIC(18,4)` / `equiv_*` 标度一致）。
///
/// 固定标度是为了让「表值 vs 计算单」在前端逐字可比：`1000` 与 `1000.0000`
/// 在字符串层面不相等，会让人误以为有差异。前端只做展示，不重新算。
pub fn qty_text(value: Decimal) -> String {
    format!("{:.4}", value.round_dp(MONEY_SCALE))
}

/// 完工率 → 文本（2 位，与 `NUMERIC(5,2)` 一致）。
pub fn pct_text(value: Decimal) -> String {
    format!("{:.2}", value.round_dp(2))
}

/// 分配率 / 单位成本 → 文本（**6 位**，与 `NUMERIC(18,6)` 一致）。
pub fn rate_text(value: Decimal) -> String {
    format!("{:.6}", value.round_dp(RATE_SCALE))
}

/// 金额 → 报文里的短写法（去掉尾随 0：`1000.0000` ⇒ `1000`）。
pub fn money_text(value: Decimal) -> String {
    value.round_dp(MONEY_SCALE).normalize().to_string()
}

/// 差异字段 → 文本：完工率 2 位、单位成本类 6 位、其余（数量 / 约当 / 金额）4 位。
fn field_text(field: &'static str, value: Decimal) -> String {
    match field {
        "完工率" => pct_text(value),
        "单位材料成本" | "单位人工成本" | "单位制造费用" => rate_text(value),
        _ => qty_text(value),
    }
}

// ==================== 错误 ====================

/// 400 + `PARAM_INVALID`（与成本域其它模块同形）。
pub fn bad_request(message: &str) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.to_string())
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数量 / 金额字面量")
    }

    /// 设计算例：完工 800 / 在制 200 / 完工率 50% / 料 10000 / 工 4500 / 费 2700。
    fn facts() -> AllocFacts {
        AllocFacts {
            object_no: "CB-T-001".to_string(),
            material_input_mode: cost_period_item_dto::INPUT_MODE_ONE_TIME.to_string(),
            cumulative_completed_qty: dec("800"),
            prior_completed_qty: Decimal::ZERO,
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            mat_cur: dec("10000"),
            labor_cur: dec("4500"),
            oh_cur: dec("2700"),
            prior: None,
            stored: None,
        }
    }

    fn element(calc: &AllocCalc, element: Element) -> &ElementAlloc {
        calc.element_of(element)
    }

    // ---------- 权限与要素 ----------

    #[test]
    fn permission_reuses_cost_object_list() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(PERMISSIONS, [PERM_LIST]);
    }

    #[test]
    fn elements_have_stable_labels_and_denominators() {
        assert_eq!(Element::Material.as_str(), "MATERIAL");
        assert_eq!(Element::Labor.as_str(), "LABOR");
        assert_eq!(Element::Overhead.as_str(), "OVERHEAD");
        assert_eq!(Element::Material.label(), "材料");
        assert_eq!(Element::Labor.label(), "人工");
        assert_eq!(Element::Overhead.label(), "制造费用");
        assert_eq!(Element::Material.equiv_kind(), "material");
        assert_eq!(Element::Labor.equiv_kind(), "conversion");
        assert_eq!(Element::Overhead.equiv_kind(), "conversion");
        assert_eq!(Element::ALL.map(|item| item.index()), [0, 1, 2]);
    }

    // ---------- 正常分配（ARCH-9 §4.6.2 算例口径） ----------

    #[test]
    fn splits_material_labor_overhead_between_finished_and_wip() {
        let calc = alloc_of(&facts());
        assert_eq!(calc.state, EquivState::Wip);
        assert_eq!(calc.completed_qty, dec("800"));
        // 分母：材料 1000、加工 900
        assert_eq!(calc.equiv_material, Some(dec("1000")));
        assert_eq!(calc.equiv_conversion, Some(dec("900")));
        // 料：10000 / 1000 = 10 ⇒ 完工 8000、在制 2000
        let material = element(&calc, Element::Material);
        assert_eq!(material.rate, Some(dec("10")));
        assert_eq!(material.finished, Some(dec("8000")));
        assert_eq!(material.wip, Some(dec("2000")));
        assert_eq!(material.equiv, Some(dec("1000")));
        // 工：4500 / 900 = 5 ⇒ 完工 4000、在制 500
        let labor = element(&calc, Element::Labor);
        assert_eq!(labor.rate, Some(dec("5")));
        assert_eq!(labor.finished, Some(dec("4000")));
        assert_eq!(labor.wip, Some(dec("500")));
        // 费：2700 / 900 = 3 ⇒ 完工 2400、在制 300
        let overhead = element(&calc, Element::Overhead);
        assert_eq!(overhead.rate, Some(dec("3")));
        assert_eq!(overhead.finished, Some(dec("2400")));
        assert_eq!(overhead.wip, Some(dec("300")));
        // 合计守恒
        assert_eq!(calc.pool_total, Some(dec("17200")));
        assert_eq!(calc.finished_total, Some(dec("14400")));
        assert_eq!(calc.wip_total, Some(dec("2800")));
        assert_eq!(
            calc.finished_total.unwrap() + calc.wip_total.unwrap(),
            calc.pool_total.unwrap()
        );
        assert!(calc.balanced());
        assert!(calc.issues.is_empty(), "正常行不该有问题：{:?}", calc.issues);
        assert!(!calc.recalc_required, "没有计算单就没有对拍");
        assert_eq!(calc.unit_finished_cost, Some(dec("18")));
    }

    #[test]
    fn completion_pct_only_moves_the_conversion_pools() {
        let mut high = facts();
        high.completion_pct = dec("100");
        let calc = alloc_of(&high);
        // 材料分母不变 ⇒ 材料分配不变
        assert_eq!(element(&calc, Element::Material).finished, Some(dec("8000")));
        assert_eq!(element(&calc, Element::Material).wip, Some(dec("2000")));
        // 加工分母 800 + 200 = 1000 ⇒ 工 4.5、费 2.7
        assert_eq!(element(&calc, Element::Labor).rate, Some(dec("4.5")));
        assert_eq!(element(&calc, Element::Labor).finished, Some(dec("3600")));
        assert_eq!(element(&calc, Element::Labor).wip, Some(dec("900")));
        assert_eq!(element(&calc, Element::Overhead).finished, Some(dec("2160")));
        assert_eq!(element(&calc, Element::Overhead).wip, Some(dec("540")));
        assert_eq!(calc.wip_total, Some(dec("3440")));
        assert!(calc.balanced());
    }

    #[test]
    fn all_finished_puts_the_whole_pool_into_finished_goods() {
        let mut all_done = facts();
        all_done.wip_qty = Decimal::ZERO;
        all_done.completion_pct = Decimal::ZERO;
        let calc = alloc_of(&all_done);
        assert_eq!(calc.state, EquivState::Finished);
        // 分母 = 800（两列同值）
        assert_eq!(calc.equiv_material, Some(dec("800")));
        assert_eq!(calc.equiv_conversion, Some(dec("800")));
        // 尾差全部进完工：三个要素的完工额之和 = 池
        assert_eq!(calc.finished_total, Some(dec("17200")));
        assert_eq!(calc.wip_total, Some(Decimal::ZERO));
        assert_eq!(element(&calc, Element::Material).finished, Some(dec("10000")));
        assert_eq!(element(&calc, Element::Labor).finished, Some(dec("4500")));
        assert_eq!(element(&calc, Element::Overhead).finished, Some(dec("2700")));
        assert!(
            calc.warnings.iter().any(|text| text.contains("全部完工")),
            "要提示尾差进完工"
        );
    }

    #[test]
    fn fractional_rate_puts_the_tail_into_wip_not_into_thin_air() {
        // 完工 1 / 在制 2 / 完工率 50% ⇒ 材料分母 3、加工分母 2
        let mut odd = facts();
        odd.cumulative_completed_qty = dec("1");
        odd.wip_qty = dec("2");
        odd.mat_cur = dec("10");
        odd.labor_cur = dec("10");
        odd.oh_cur = Decimal::ZERO;
        let calc = alloc_of(&odd);
        // 材料率 3.333333 ⇒ 完工 3.3333、在制 6.6667（合计 10，一分不差）
        assert_eq!(element(&calc, Element::Material).rate, Some(dec("3.333333")));
        assert_eq!(element(&calc, Element::Material).finished, Some(dec("3.3333")));
        assert_eq!(element(&calc, Element::Material).wip, Some(dec("6.6667")));
        assert_eq!(
            element(&calc, Element::Material).finished.unwrap() + element(&calc, Element::Material).wip.unwrap(),
            dec("10")
        );
        assert!(calc.balanced());
    }

    #[test]
    fn no_completion_leaves_everything_in_wip() {
        let mut no_finish = facts();
        no_finish.cumulative_completed_qty = Decimal::ZERO;
        let calc = alloc_of(&no_finish);
        assert_eq!(calc.completed_qty, Decimal::ZERO);
        assert_eq!(element(&calc, Element::Material).finished, Some(Decimal::ZERO));
        assert_eq!(element(&calc, Element::Material).wip, Some(dec("10000")));
        assert_eq!(calc.finished_total, Some(Decimal::ZERO));
        assert_eq!(calc.wip_total, Some(dec("17200")));
        assert!(
            calc.warnings.iter().any(|text| text.contains("本期没有完工")),
            "要说明全部留在在制"
        );
    }

    #[test]
    fn rate_uses_the_unrounded_equivalence_like_calc_does() {
        // 在制 100.0001 × 33.33% = 33.3300333：显示 4 位是 33.3300，分母必须是原值
        let mut tricky = facts();
        tricky.cumulative_completed_qty = dec("1");
        tricky.wip_qty = dec("100.0001");
        tricky.completion_pct = dec("33.33");
        tricky.mat_cur = dec("100");
        tricky.labor_cur = dec("100");
        tricky.oh_cur = Decimal::ZERO;
        let calc = alloc_of(&tricky);
        let conversion = element(&calc, Element::Labor);
        let raw = dec("1") + dec("100.0001") * dec("33.33") / dec("100");
        let expected = cost_period_item_dto::unit_cost(dec("100") / raw);
        assert_eq!(conversion.rate, Some(expected));
        // 显示值仍是 4 位
        assert_eq!(calc.equiv_conversion, Some(equiv::eq_qty(raw)));
        assert_eq!(calc.equiv_conversion, Some(dec("34.3300")));
    }

    // ---------- 期初（上期已结算） ----------

    #[test]
    fn beginning_elements_come_from_the_prior_unit_costs() {
        let mut with_beg = facts();
        with_beg.prior = Some(PriorFacts {
            period_code: "2026-10".to_string(),
            wip_end_qty: dec("100"),
            wip_end_cost: dec("3000"),
            completion_pct: dec("50"),
            unit_material: dec("20"),
            unit_labor: dec("6"),
            unit_overhead: dec("1.4"),
        });
        let beg = beg_elements(with_beg.prior.as_ref()).expect("期初可还原");
        // 材料 20 × 100 = 2000；人工 6 × 50 = 300；制造费用取尾差 3000 − 2300 = 700
        assert_eq!(beg.material, dec("2000"));
        assert_eq!(beg.labor, dec("300"));
        assert_eq!(beg.overhead, dec("700"));
        assert_eq!(beg.total, dec("3000"), "三要素合计 = 上期期末在制成本");

        let calc = alloc_of(&with_beg);
        assert_eq!(calc.beg_total, Some(dec("3000")));
        assert_eq!(calc.pool_total, Some(dec("20200")));
        assert_eq!(element(&calc, Element::Material).pool, Some(dec("12000")));
        // 材料率 12000 / 1000 = 12 ⇒ 完工 9600
        assert_eq!(element(&calc, Element::Material).finished, Some(dec("9600")));
        assert!(calc.balanced());
    }

    #[test]
    fn no_prior_means_zero_beginning() {
        let beg = beg_elements(None).expect("首期期初为 0");
        assert_eq!(beg, BegElements::default());
        assert_eq!(beg.total, Decimal::ZERO);
    }

    #[test]
    fn negative_prior_unit_cost_blocks_with_beg_code() {
        let mut broken = facts();
        broken.prior = Some(PriorFacts {
            period_code: "2026-10".to_string(),
            wip_end_qty: dec("100"),
            wip_end_cost: dec("100"),
            completion_pct: dec("50"),
            unit_material: dec("-1"),
            unit_labor: dec("0"),
            unit_overhead: dec("0"),
        });
        let calc = alloc_of(&broken);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("BEG_COST_INVALID"));
        assert_eq!(calc.beg_total, None);
        assert_eq!(calc.pool_total, None);
        assert_eq!(calc.finished_total, None);
        let material = element(&calc, Element::Material);
        assert_eq!(material.beg, None, "期初还原失败不给半个期初");
        assert_eq!(material.pool, None);
        assert_eq!(material.rate, None);
        assert_eq!(material.finished, None);
        assert_eq!(material.cur, dec("10000"), "本期投入是事实，照给");
        let issue = &calc.issues[0];
        assert!(!issue.fix.is_empty(), "每条问题都要有怎么修");
    }

    // ---------- 算不出来（不画假 0） ----------

    #[test]
    fn wip_without_pct_is_blocked_not_zero() {
        let mut no_pct = facts();
        no_pct.completion_pct = Decimal::ZERO;
        let calc = alloc_of(&no_pct);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("WIP_PCT_MISSING"));
        assert_eq!(calc.equiv_conversion, None);
        assert_eq!(calc.equiv_material, Some(dec("1000")), "材料侧不受完工率影响");
        // 整行不给金额：只给材料侧会给一个「半个分配」
        assert_eq!(calc.finished_total, None);
        assert_eq!(calc.wip_total, None);
        for item in &calc.elements {
            assert_eq!(item.rate, None, "{} 不该给分配率", item.element.label());
            assert_eq!(item.finished, None);
            assert_eq!(item.wip, None);
            assert_eq!(item.pool, Some(item.beg.unwrap_or_default() + item.cur));
        }
        assert!(calc.balanced(), "算不出来时守恒断言不适用（全部 None）");
    }

    #[test]
    fn no_output_is_blocked_even_with_cost() {
        let mut no_output = facts();
        no_output.cumulative_completed_qty = Decimal::ZERO;
        no_output.wip_qty = Decimal::ZERO;
        no_output.completion_pct = Decimal::ZERO;
        let calc = alloc_of(&no_output);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("NO_OUTPUT"));
        assert!(calc.issues[0].message.contains("17200"), "报文要点出有多少钱");
    }

    #[test]
    fn unsupported_material_mode_blocks_material_side() {
        let mut as_progress = facts();
        as_progress.material_input_mode = "AS_PROGRESS".to_string();
        let calc = alloc_of(&as_progress);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("MATERIAL_MODE_UNSUPPORTED"));
    }

    #[test]
    fn negative_pool_blocks_with_a_negative_alloc_code() {
        let mut negative = facts();
        negative.mat_cur = dec("-100");
        let calc = alloc_of(&negative);
        assert_eq!(calc.state, EquivState::Blocked);
        assert_eq!(calc.first_issue(), Some("NEGATIVE_ALLOC"));
        assert_eq!(calc.finished_total, None);
        assert!(calc.issues[0].message.contains("材料"));
        assert!(!calc.issues[0].fix.is_empty());
    }

    // ---------- 对拍计算单 ----------

    fn stored_of(calc: &AllocCalc) -> StoredItem {
        StoredItem {
            status: "CALC".to_string(),
            wip_beg_cost: calc.beg_total.unwrap_or_default(),
            completed_qty: calc.completed_qty,
            wip_end_qty: calc.wip_qty,
            completion_pct: calc.completion_pct,
            equiv_material: calc.equiv_material.unwrap_or_default(),
            equiv_conversion: calc.equiv_conversion.unwrap_or_default(),
            unit_material: calc.element_of(Element::Material).rate.unwrap_or_default(),
            unit_labor: calc.element_of(Element::Labor).rate.unwrap_or_default(),
            unit_overhead: calc.element_of(Element::Overhead).rate.unwrap_or_default(),
            finished_cost: calc.finished_total.unwrap_or_default(),
            wip_end_cost: calc.wip_total.unwrap_or_default(),
        }
    }

    #[test]
    fn no_drift_when_the_stored_item_is_the_same_calculation() {
        let mut same = facts();
        let computed = alloc_of(&same);
        same.stored = Some(stored_of(&computed));
        let calc = alloc_of(&same);
        assert!(
            calc.drift.is_empty(),
            "同一组输入的同一组分配不该报漂移：{:?}",
            calc.drift
        );
        assert!(!calc.recalc_required);
    }

    #[test]
    fn drift_flags_unit_cost_and_amounts() {
        let mut changed = facts();
        let computed = alloc_of(&changed);
        let mut stored = stored_of(&computed);
        stored.unit_material = dec("9.5");
        stored.finished_cost = dec("14000");
        stored.wip_end_cost = dec("3000");
        changed.stored = Some(stored);
        let calc = alloc_of(&changed);
        assert!(calc.recalc_required);
        let fields: Vec<&str> = calc.drift.iter().map(|item| item.field).collect();
        assert!(fields.contains(&"单位材料成本"), "字段：{fields:?}");
        assert!(fields.contains(&"完工转出成本"));
        assert!(fields.contains(&"期末在制成本"));
        let note = drift_hint(&calc, changed.stored.as_ref().unwrap());
        assert!(note.contains("重算"), "说明要指向重算：{note}");
    }

    #[test]
    fn drift_flags_input_change_through_quantities() {
        let mut changed = facts();
        let stored = stored_of(&alloc_of(&changed));
        changed.completion_pct = dec("60");
        changed.stored = Some(stored);
        let calc = alloc_of(&changed);
        let fields: Vec<&str> = calc.drift.iter().map(|item| item.field).collect();
        assert!(fields.contains(&"完工率"), "字段：{fields:?}");
        assert!(fields.contains(&"加工约当"), "约当变了也要报：{fields:?}");
        assert!(calc.recalc_required);
    }

    #[test]
    fn settled_item_points_to_reversal_instead_of_recalc() {
        let mut changed = facts();
        let mut stored = stored_of(&alloc_of(&changed));
        stored.status = "SETTLED".to_string();
        stored.unit_labor = dec("4.9");
        changed.stored = Some(stored);
        let calc = alloc_of(&changed);
        let note = drift_hint(&calc, changed.stored.as_ref().unwrap());
        assert!(note.contains("红冲"), "已结算只能红冲：{note}");
    }

    #[test]
    fn blocked_rows_do_not_produce_drift_noise() {
        let mut broken = facts();
        broken.completion_pct = Decimal::ZERO;
        broken.stored = Some(StoredItem {
            status: "CALC".to_string(),
            wip_beg_cost: Decimal::ZERO,
            completed_qty: dec("800"),
            wip_end_qty: dec("200"),
            completion_pct: dec("50"),
            equiv_material: dec("1000"),
            equiv_conversion: dec("900"),
            unit_material: dec("10"),
            unit_labor: dec("5"),
            unit_overhead: dec("3"),
            finished_cost: dec("14400"),
            wip_end_cost: dec("2800"),
        });
        let calc = alloc_of(&broken);
        assert_eq!(calc.state, EquivState::Blocked);
        assert!(calc.drift.is_empty(), "算不出来的行不该再叠一层漂移");
        assert!(!calc.recalc_required);
    }

    // ---------- 合计 ----------

    #[test]
    fn totals_skip_blocked_money_but_count_its_quantities() {
        let good = alloc_of(&facts());
        let mut broken = facts();
        broken.wip_qty = Decimal::ZERO;
        broken.completion_pct = Decimal::ZERO;
        broken.cumulative_completed_qty = Decimal::ZERO;
        let broken = alloc_of(&broken);

        let mut totals = AllocTotals::default();
        totals.add(&good);
        totals.add(&broken);
        assert_eq!(totals.row_count, 2);
        assert_eq!(totals.wip_row_count, 1);
        assert_eq!(totals.blocked_row_count, 1);
        assert_eq!(totals.completed_qty, dec("800"), "数量合计含 blocked 行（它是 0）");
        assert_eq!(totals.pool_total, dec("17200"), "金额合计不含 blocked 行");
        assert_eq!(totals.pool_material, dec("10000"));
        assert_eq!(totals.pool_labor, dec("4500"));
        assert_eq!(totals.pool_overhead, dec("2700"));
        assert_eq!(totals.finished_total, dec("14400"));
        assert_eq!(totals.wip_total, dec("2800"));
        assert_eq!(
            totals.finished_total + totals.wip_total,
            totals.pool_total,
            "整表守恒：完工 + 在制 = 池"
        );
        let view = totals_view(&totals);
        assert!(view.note.contains("1 行"), "合计口径要写出来：{}", view.note);
    }

    #[test]
    fn totals_count_drift_rows() {
        let mut changed = facts();
        let mut stored = stored_of(&alloc_of(&changed));
        stored.unit_material = dec("11");
        changed.stored = Some(stored);
        let calc = alloc_of(&changed);
        let mut totals = AllocTotals::default();
        totals.add(&calc);
        assert_eq!(totals.drift_row_count, 1);
    }

    // ---------- 筛档 ----------

    #[test]
    fn state_filter_parses_and_rejects_typos() {
        assert_eq!(AllocStateFilter::parse(None).unwrap(), AllocStateFilter::All);
        assert_eq!(AllocStateFilter::parse(Some("  ")).unwrap(), AllocStateFilter::All);
        assert_eq!(AllocStateFilter::parse(Some("wip")).unwrap(), AllocStateFilter::Wip);
        assert_eq!(AllocStateFilter::parse(Some("drift")).unwrap(), AllocStateFilter::Drift);
        let error = AllocStateFilter::parse(Some("wipp")).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("wipp"));
    }

    #[test]
    fn state_filter_matches_the_row_shape() {
        let wip_row = alloc_of(&facts());
        assert!(AllocStateFilter::All.matches(&wip_row));
        assert!(AllocStateFilter::Wip.matches(&wip_row));
        assert!(!AllocStateFilter::Finished.matches(&wip_row));
        assert!(!AllocStateFilter::Blocked.matches(&wip_row));

        let mut all_done = facts();
        all_done.wip_qty = Decimal::ZERO;
        let finished_row = alloc_of(&all_done);
        assert!(AllocStateFilter::Finished.matches(&finished_row));

        let mut broken = facts();
        broken.completion_pct = Decimal::ZERO;
        let blocked_row = alloc_of(&broken);
        assert!(AllocStateFilter::Blocked.matches(&blocked_row));

        let mut changed = facts();
        let mut stored = stored_of(&alloc_of(&changed));
        stored.unit_labor = dec("1");
        changed.stored = Some(stored);
        let drift_row = alloc_of(&changed);
        assert!(AllocStateFilter::Drift.matches(&drift_row));
        assert!(AllocStateFilter::Wip.matches(&drift_row), "漂移行同时有在制");
    }

    #[test]
    fn state_options_are_in_interface_order() {
        let options = state_options();
        assert_eq!(options.len(), 5);
        assert_eq!(options[0].value, "all");
        assert_eq!(options[1].value, "wip");
        assert_eq!(options[2].value, "finished");
        assert_eq!(options[3].value, "blocked");
        assert_eq!(options[4].value, "drift");
        assert_eq!(options[4].label, "与计算单不一致");
    }

    // ---------- 出参装配 ----------

    #[test]
    fn row_view_formats_every_money_column_at_four_places() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let head = RowHead {
            object_id: "11111111-1111-1111-1111-111111111111".to_string(),
            object_no: facts.object_no.clone(),
            object_type: "BATCH".to_string(),
            source_doc_no: Some("PO-1".to_string()),
            lot_no: Some("LOT-1".to_string()),
            product_id: "22222222-2222-2222-2222-222222222222".to_string(),
            product_code: Some("FG-1".to_string()),
            product_name: Some("成品一".to_string()),
            workshop_dept_id: "33333333-3333-3333-3333-333333333333".to_string(),
            workshop_dept_name: Some("一车间".to_string()),
            plan_qty: "1000.0000".to_string(),
        };
        let row = row_view(&head, &facts, &calc);
        assert_eq!(row.pool_total.as_deref(), Some("17200.0000"));
        assert_eq!(row.finished_total.as_deref(), Some("14400.0000"));
        assert_eq!(row.wip_total.as_deref(), Some("2800.0000"));
        assert_eq!(row.unit_finished_cost.as_deref(), Some("18.000000"));
        assert_eq!(row.state, "wip");
        assert_eq!(row.state_label, "有在制");
        assert_eq!(row.elements.len(), 3);
        assert_eq!(row.elements[0].label, "材料");
        assert_eq!(row.elements[0].rate.as_deref(), Some("10.000000"));
        assert_eq!(row.elements[1].equiv_kind, "conversion");
        assert!(row.calc.is_none(), "没计算单 ⇒ null，不是空对象");
        assert!(row.prior.is_none());
    }

    #[test]
    fn blocked_row_exposes_null_money_and_the_fix() {
        let mut broken = facts();
        broken.completion_pct = Decimal::ZERO;
        let calc = alloc_of(&broken);
        let head = RowHead {
            object_id: "11111111-1111-1111-1111-111111111111".to_string(),
            object_no: broken.object_no.clone(),
            object_type: "BATCH".to_string(),
            source_doc_no: None,
            lot_no: None,
            product_id: "22222222-2222-2222-2222-222222222222".to_string(),
            product_code: None,
            product_name: None,
            workshop_dept_id: "33333333-3333-3333-3333-333333333333".to_string(),
            workshop_dept_name: None,
            plan_qty: "0.0000".to_string(),
        };
        let row = row_view(&head, &broken, &calc);
        assert_eq!(row.state, "blocked");
        assert_eq!(row.finished_total, None);
        assert_eq!(row.wip_total, None);
        // 池是事实（期初 0 + 本期投入），照给；分不出来的只是「完工 / 在制」两列
        assert_eq!(row.pool_total.as_deref(), Some("17200.0000"));
        assert_eq!(row.elements[0].finished, None);
        assert!(!row.issues.is_empty());
        assert!(!row.issues[0].fix.is_empty());
    }

    #[test]
    fn text_helpers_use_fixed_scales() {
        assert_eq!(qty_text(dec("1000")), "1000.0000");
        assert_eq!(pct_text(dec("50")), "50.00");
        assert_eq!(rate_text(dec("3.3")), "3.300000");
        assert_eq!(money_text(dec("1000.0000")), "1000");
        assert_eq!(field_text("完工率", dec("60")), "60.00");
        assert_eq!(field_text("单位材料成本", dec("9.5")), "9.500000");
        assert_eq!(field_text("完工转出成本", dec("14400")), "14400.0000");
    }

    #[test]
    fn zero_pool_warns_but_is_not_blocked() {
        let mut no_cost = facts();
        no_cost.mat_cur = Decimal::ZERO;
        no_cost.labor_cur = Decimal::ZERO;
        no_cost.oh_cur = Decimal::ZERO;
        let calc = alloc_of(&no_cost);
        assert_ne!(calc.state, EquivState::Blocked, "没有钱不是算不出来");
        assert_eq!(calc.finished_total, Some(Decimal::ZERO));
        assert_eq!(calc.wip_total, Some(Decimal::ZERO));
        assert!(calc.warnings.iter().any(|text| text.contains("没有可分配的成本")));
    }
}

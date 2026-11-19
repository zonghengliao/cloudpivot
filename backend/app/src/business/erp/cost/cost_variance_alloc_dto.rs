//! 差异分解 · **差异分摊到成本对象** 的契约层与纯逻辑（成本核算 · M5-15）。
//!
//! 本文件只做一件事：把「已经算出来、但还没归属到具体成本对象」的**差异池**
//! （`MAT_PRICE` / `MAT_QTY` / `LABOR_RATE` / `LABOR_EFF` / `OH_SPEND` / `OH_VOLUME`），
//! 按**规则指定的基数**摊到参与分配的成本对象上，尾差用**最大余数法单行收口**，
//! `Σ分摊 = 待分摊额` 是硬断言（含负池 / 有利差异）。
//!
//! 零 DB、零路由、零迁移、零新增权限码 —— 与 M5-12 / M5-13 / M5-14 同一交付形态。
//! 落表 / 出凭证 / 出报表的卡（M5-16 凭证、M5-19 差异分析页）直接调
//! [`allocate_variance`]（单池）或 [`allocate_period`]（按期批量）。
//!
//! # 为什么差异要「摊」到成本对象
//!
//! 差异池回答的是「这个期间、这个料（或这条人工口径）一共多花了多少钱」，
//! 而成本对象回答的是「哪一批货多花了多少钱」。管理上要追的是后者（哪批亏、哪个料贵），
//! 所以必须有一个**明确的、可复算的**归属规则 —— 这一份文件就是那个规则。
//!
//! # 本卡的口径（逐条写死，改之前先看 ARCH-9 §4.9 与本表）
//!
//! | 问题 | 本卡的口径 | 为什么 |
//! |---|---|---|
//! | 摊什么 | 只摊**待分摊额** `allocatable = 差异合计 − 已归属`（已挂 `cost_object_id` 的差异行不再摊） | 已经归到对象的差异再摊一遍就是双计；`biz_cost_variance.cost_object_id` 可空正是为此（空 = 池） |
//! | 按什么摊 | 规则给 `basis` 就用规则的；不给就按**差异类型的默认基数**（见 [`default_basis_of`]） | 「按工时摊材料价差」不是错，是另一种管理口径，必须允许；但拼错字必须 400 而不是静默退化成默认 |
//! | 默认基数怎么定 | 价差 / 量差 → `ACTUAL_QTY`（实际用量）；率差 / 效差 → `LABOR_HOURS`（实际工时）；费用两差 → `STD_AMOUNT`（标准分配额） | 权重必须与差异**同一个量纲**：差异来自「这个对象用了多少」，摊回去就按「用了多少」摊 |
//! | 负数池（有利差异） | 按 `\|池\|` 分，结果整体翻符号 | 最大余数法要求非负池；直接拒会让「整体节约」的期间根本摊不出结果。翻符号后 `Σ分摊 = 池` 仍精确成立（单测钉住） |
//! | 尾差 | **最大余数法单行收口**，`rounding_adj ≠ 0` 的行至多一行，且落在余数最大者 | ARCH-9 §4.4.3 ③ / D-9。多行收尾差 = 各行口径不一致，报表会差一厘但没人说得清差在哪 |
//! | 尾差落在谁头上 | 余数最大 → 基数大 → `cost_object_id` 字典序小（三级并列规则由 [`rule_dto::allocate`] 定，本文件不重写） | 同一次分摊跑两遍必须给出同一个答案（幂等），否则重算一次金额就变 |
//! | 基数 0 的对象 | **留在结果里**（`participates = false`、分摊 0） | 从结果里删掉会让「这批这个月一件没做」查不到账；报表要能显式看到「没参与」 |
//! | 基数全 0 / 一个对象都没有 | 409（`66062` / `66063`），人话 + 怎么修 | 池里的钱摊不出去＝账不平。静默摊成 0 等于把差额藏起来（与 D-7「不猜数据」同一取向） |
//! | 金额标度 | 4 位（`biz_cost_variance.amount NUMERIC(18,4)`），先**截断**到分再收尾差 | 与差异行同标度；四舍五入到分再收尾差会让「截断 + 尾差」的算路和 ARCH §4.4.3 ③ 不一致 |
//! | 分摊比例 `share_rate` | 出参给、**只用于展示**（8 位小数），**不许拿它算钱** | 用它乘回去必然差一分（单测 `share_rate_must_not_be_used_as_money` 把这个坑钉成可复现的断言） |
//! | 入账吗 | **不入账**（不算凭证、不动存货） | ARCH-9 §4.9.3 / D-15：差异是「被看见」的报表结论；差异凭证是 M5-16 的独立决策 |
//!
//! # 算数只有一份（本文件不重写最大余数法）
//!
//! 分摊算数直接委托 B2-10 的 [`rule_dto::allocate`]（制造费用分摊用的同一个函数）：
//! 那也是「最大余数法」在本仓的**唯一实现**。本文件只做三件它不管的事 ——
//! ① 有符号池的符号处理；② 已归属额的扣除；③ 期间 × 差异类型 × 物料的批次守恒。
//! 单测 [`tests::delegates_to_the_overhead_largest_remainder_arithmetic`] 拿两条路径的
//! 逐行结果比相等：哪天有人为了「差异分摊看起来不一样」手抄一份算法，这条测试立刻红。
//!
//! # 取数口径（不在本文件，但写清楚免得取错列）
//!
//! | 基数 | 现实来源 | 列精度 |
//! |---|---|---|
//! | `ACTUAL_QTY` | `Σ biz_stock_out_item.qty`（该期间、该物料、该成本对象、已过账、未作废） | `NUMERIC(18,6)` |
//! | `LABOR_HOURS` | `Σ biz_cost_labor.hours`（只算 `CONFIRMED`） | `NUMERIC(10,2)` |
//! | `OUTPUT_QTY` | `biz_cost_period_item.completed_qty`（本期完工数量） | `NUMERIC(18,6)` |
//! | `STD_AMOUNT` | `Σ money(标准价 × 标准量)`（材料）/ `money(标准费率 × 实际产量)`（费用） | 4 位 |
//!
//! 池的粒度**必须**是「期间 × 差异类型 × 物料（材料类）」：材料类差异不带物料就没法知道
//! 「摊给哪些用过这个料的对象」，摊出来的权重是别的料的用量 —— 所以 [`validate_pool`]
//! 直接拒掉材料类不带物料的池，并在 [`allocate_period`] 里拒掉重复的池键（防双计）。

use std::collections::HashSet;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot_kernel::shared::error_code;

use crate::business::erp::cost::cost_alloc_rule_dto::{self as rule_dto, AllocObject};
use crate::business::erp::cost::cost_period_item_dto::{self as period_item_dto, money};
use crate::business::erp::cost::cost_qty_variance_dto;
use crate::business::erp::cost::cost_standard_compare_dto::{
    ELEMENT_LABOR, ELEMENT_MATERIAL, ELEMENT_OVERHEAD, ELEMENTS,
};
use crate::business::erp::cost::cost_variance_dto::{self as rate_dto, max_money};

/// 材料类差异的默认基数：该成本对象在本期**实际用量**（`Σ 领料出库数量`）。
pub const BASIS_ACTUAL_QTY: &str = "ACTUAL_QTY";

/// 人工类差异的默认基数：该成本对象在本期**实际工时**。
///
/// **字面值与 B2-10 的制造费用分摊标准同值**（`cost_alloc_rule_dto::BASIS_LABOR_HOURS`），
/// 所以这里直接转口而不是再写一遍 `"LABOR_HOURS"` —— 两份字面值将来会一半改一半没改。
pub use crate::business::erp::cost::cost_alloc_rule_dto::{BASIS_LABOR_HOURS, BASIS_OUTPUT_QTY};

/// 制造费用两差的默认基数：该成本对象在本期**标准分配额**（标准费率 × 实际产量）。
///
/// 为什么不是 `OUTPUT_QTY`：费用两差本身就是「预算 − 标准分配额」，钱是按标准费率吸收到
/// 对象上的，摊回去也应当按「吸收了多少」摊；按完工数量摊会让产品结构不同的批次间串账。
pub const BASIS_STD_AMOUNT: &str = "STD_AMOUNT";

/// 差异分摊可用的四种基数（顺序即前端下拉顺序）。
pub const VARIANCE_ALLOC_BASES: [&str; 4] = [BASIS_ACTUAL_QTY, BASIS_LABOR_HOURS, BASIS_OUTPUT_QTY, BASIS_STD_AMOUNT];

/// `basis` 的来源：规则显式指定。
pub const BASIS_SOURCE_RULE: &str = "RULE";

/// `basis` 的来源：按差异类型的默认口径（规则没指定）。
pub const BASIS_SOURCE_DEFAULT: &str = "DEFAULT";

/// 制造费用**耗费**差异（`ARCH-9 §4.9.1` 第 5 行；`ck_biz_cost_var_type` 的取值之一）。
///
/// 公式卡（`实际制造费用 − 预算制造费用`）尚未落地，本模块只用它做「池的差异类型」，
/// 因此先把字面值定在这里并在 [`VARIANCE_TYPES_ALL`] 里登记。公式卡落地时
/// **应把这两个常量搬过去、在本文件 `pub use` 转口** —— 否则两处字面值会分叉。
pub const VARIANCE_OH_SPEND: &str = "OH_SPEND";

/// 制造费用**产量**差异（`预算制造费用 − 标准分配额`），同 [`VARIANCE_OH_SPEND`]。
pub const VARIANCE_OH_VOLUME: &str = "OH_VOLUME";

/// `biz_cost_variance.variance_type` 的全部六个取值（库层 `ck_biz_cost_var_type` 镜像）。
pub const VARIANCE_TYPES_ALL: [&str; 6] = [
    rate_dto::VARIANCE_MAT_PRICE,
    cost_qty_variance_dto::VARIANCE_MAT_QTY,
    rate_dto::VARIANCE_LABOR_RATE,
    cost_qty_variance_dto::VARIANCE_LABOR_EFF,
    VARIANCE_OH_SPEND,
    VARIANCE_OH_VOLUME,
];

/// 分摊比例的标度（**只用于展示**，见模块头）。
pub const SHARE_SCALE: u32 = 8;

/// 对象状态快照里允许的两态（`biz_cost_object.status`：在产 / 关批）。
///
/// 本函数不按状态筛对象（筛谁参与是取数方的事），这里只做值域校验，
/// 防止取数层把 `CANCELLED` 这类已作废对象灌进来摊差异。
pub const OBJECT_STATUSES: [&str; 2] = ["OPEN", "CLOSED"];

// ==================== 文案与值域 ====================

/// 四种基数的中文说明（前端下拉 + 报文用）。
pub fn basis_label(code: &str) -> &'static str {
    match code {
        BASIS_ACTUAL_QTY => "按实际用量分摊",
        BASIS_LABOR_HOURS => "按实际工时分摊",
        BASIS_OUTPUT_QTY => "按本期完工数量分摊",
        BASIS_STD_AMOUNT => "按标准分配额分摊",
        _ => "按未知标准分摊",
    }
}

/// 基数要去哪张表取数（报文里告诉用户「去哪儿补数」）。
pub fn basis_requirement(code: &str) -> &'static str {
    match code {
        BASIS_ACTUAL_QTY => "该物料在该成本对象上的领料出库数量（已过账、未作废）",
        BASIS_LABOR_HOURS => "该成本对象当期已确认的工时台账小时数",
        BASIS_OUTPUT_QTY => "该成本对象本期的完工数量（成本计算单）",
        BASIS_STD_AMOUNT => "该成本对象按标准费率吸收的标准分配额（标准费率 × 实际产量）",
        _ => "该成本对象的基数数据",
    }
}

/// 六类差异的中文名（报文与报表标题用）。
///
/// 四类已由公式卡（M5-12 / M5-13）定义名字，这里**必须与它们逐字一致**（单测
/// [`tests::variance_labels_agree_with_the_formula_modules`] 逐值比对），否则同一张差异
/// 在「差异明细」和「差异分摊」两个界面上叫两个名字。
pub fn variance_type_label(variance_type: &str) -> &'static str {
    match variance_type {
        rate_dto::VARIANCE_MAT_PRICE => rate_dto::variance_label(rate_dto::VARIANCE_MAT_PRICE),
        cost_qty_variance_dto::VARIANCE_MAT_QTY => {
            cost_qty_variance_dto::variance_label(cost_qty_variance_dto::VARIANCE_MAT_QTY)
        }
        rate_dto::VARIANCE_LABOR_RATE => rate_dto::variance_label(rate_dto::VARIANCE_LABOR_RATE),
        cost_qty_variance_dto::VARIANCE_LABOR_EFF => {
            cost_qty_variance_dto::variance_label(cost_qty_variance_dto::VARIANCE_LABOR_EFF)
        }
        VARIANCE_OH_SPEND => "制造费用耗费差异",
        VARIANCE_OH_VOLUME => "制造费用产量差异",
        _ => "差异",
    }
}

/// 差异类型 → 成本要素（`biz_cost_variance.element`）的唯一对照表。
///
/// `MAT_QTY` / `LABOR_EFF` 两行必须与 M5-13 的
/// [`cost_qty_variance_dto::canonical_element`] 一致（单测逐值比对）：
/// 同一行差异在两个模块里挂不同要素，汇总到三要素口径就会一边多一边少。
pub fn canonical_element_of(variance_type: &str) -> Option<&'static str> {
    match variance_type {
        rate_dto::VARIANCE_MAT_PRICE | cost_qty_variance_dto::VARIANCE_MAT_QTY => Some(ELEMENT_MATERIAL),
        rate_dto::VARIANCE_LABOR_RATE | cost_qty_variance_dto::VARIANCE_LABOR_EFF => Some(ELEMENT_LABOR),
        VARIANCE_OH_SPEND | VARIANCE_OH_VOLUME => Some(ELEMENT_OVERHEAD),
        _ => None,
    }
}

/// 差异类型的默认分摊基数（规则没显式指定时用）。
///
/// 未知类型 400：**不回落**到某个「看起来很通用」的基数 —— 摊错基数算出来的金额
/// 看起来完全正常，只有对不上账时才有人发现。
pub fn default_basis_of(variance_type: &str) -> std::result::Result<&'static str, ApiError> {
    match canonical_element_of(variance_type) {
        Some(ELEMENT_MATERIAL) => Ok(BASIS_ACTUAL_QTY),
        Some(ELEMENT_LABOR) => Ok(BASIS_LABOR_HOURS),
        Some(ELEMENT_OVERHEAD) => Ok(BASIS_STD_AMOUNT),
        _ => Err(invalid(format!(
            "差异类型 {variance_type} 不认识：只能是 {} 之一（库层 ck_biz_cost_var_type 同口径）",
            VARIANCE_TYPES_ALL.join(" / ")
        ))),
    }
}

/// 归一化规则给的基数：`None`/空 → 该差异类型的默认基数；给了就必须是四值之一。
///
/// 返回 `(基数, 来源)`，来源进 [`VarianceAllocOutcome::basis_source`]，报表上要能看出
/// 「这一池是按规则摊的还是按默认口径摊的」。
pub fn normalize_basis(
    raw: Option<&str>,
    variance_type: &str,
) -> std::result::Result<(String, &'static str), ApiError> {
    let default = default_basis_of(variance_type)?;
    let text = raw.map(str::trim).filter(|text| !text.is_empty());
    let Some(text) = text else {
        return Ok((default.to_string(), BASIS_SOURCE_DEFAULT));
    };
    if let Some(known) = VARIANCE_ALLOC_BASES.iter().find(|known| **known == text) {
        return Ok(((*known).to_string(), BASIS_SOURCE_RULE));
    }
    Err(invalid(format!(
        "分摊基数 {text} 不合法：差异分摊只认 {} 之一。\n\
         想按默认口径摊就不填基数（材料类默认实际用量、人工类默认实际工时、费用类默认标准分配额）",
        VARIANCE_ALLOC_BASES.join(" / ")
    )))
}

// ==================== 个位数出入参 ====================

/// 一个差异池：同期间 × 同差异类型 × 同物料（材料类）的差异合计。
///
/// `total_amount` 与 `attributed_amount` **都是有符号的**（正 = 不利、负 = 有利）。
/// 待分摊额 = `total_amount − attributed_amount`；两者符号可以不同（某些对象已挂不利、
/// 其余未挂的却有节约），不是错误。
#[derive(Debug, Clone, PartialEq)]
pub struct VariancePool {
    /// 会计期间 `yyyy-MM`。
    pub period_code: String,
    /// 差异类型（`VARIANCE_TYPES_ALL` 之一）。
    pub variance_type: String,
    /// 成本要素（必须与该差异类型配对，见 [`canonical_element_of`]）。
    pub element: String,
    /// 物料（材料类差异必填；人工 / 费用类必须为空）。
    pub material_id: Option<Uuid>,
    /// 物料编码（回显，可空）。
    pub material_code: Option<String>,
    /// 差异合计（有符号，4 位）。
    pub total_amount: Decimal,
    /// 已经直接挂到成本对象的差异合计（有符号，4 位）。
    pub attributed_amount: Decimal,
}

impl VariancePool {
    /// 待分摊额 = 合计 − 已归属。符号可以与本池的「不利/有利」不同（见结构体注释）。
    pub fn allocatable(&self) -> Decimal {
        self.total_amount - self.attributed_amount
    }

    /// 报文里的一行标识（期间 + 类型 + 物料）。
    pub fn label(&self) -> String {
        match (&self.material_id, &self.material_code) {
            (Some(_), Some(code)) => format!(
                "期间 {} 的{}（物料 {}）",
                self.period_code,
                variance_type_label(&self.variance_type),
                code
            ),
            (Some(_), None) => format!(
                "期间 {} 的{}（物料 {}）",
                self.period_code,
                variance_type_label(&self.variance_type),
                self.material_id.map(|id| id.to_string()).unwrap_or_default()
            ),
            _ => format!(
                "期间 {} 的{}",
                self.period_code,
                variance_type_label(&self.variance_type)
            ),
        }
    }

    /// 池键（期间 × 类型 × 物料）：同一批池里不允许两行同键（那是「该聚合没聚合」= 双计）。
    fn key(&self) -> (String, Option<Uuid>) {
        (self.variance_type.clone(), self.material_id)
    }
}

/// 分摊规则（本卡的「按规则分」）。
///
/// 规则只是一个**口径载体**：`basis` 为空就按差异类型的默认基数摊，给了就用它。
/// 规则本身的主数据 / 生效期 / 版本管理属于 B2-5 的分摊规则表（`biz_cost_alloc_rule`），
/// 那是**费用**分摊的规则；差异分摊的规则卡不在本卡范围内，所以这里只留下
/// 「规则编码 / 名称 / 基数」三个字段供落表方快照。
#[derive(Debug, Clone, PartialEq)]
pub struct VarianceAllocRule {
    /// 规则主键（规则来自主数据时回填；走默认口径时为 `None`）。
    pub rule_id: Option<Uuid>,
    /// 规则编码（必填，落 remark / 日志用）。
    pub rule_code: String,
    /// 规则名称（展示用）。
    pub rule_name: String,
    /// 分配基数（`None` = 按差异类型的默认基数）。
    pub basis: Option<String>,
}

impl VarianceAllocRule {
    /// 显式口径的规则。
    pub fn with_basis(rule_code: &str, rule_name: &str, basis: &str) -> Self {
        Self {
            rule_id: None,
            rule_code: rule_code.to_string(),
            rule_name: rule_name.to_string(),
            basis: Some(basis.to_string()),
        }
    }

    /// 「按默认口径摊」的规则（材料类按实际用量、人工类按实际工时、费用类按标准分配额）。
    pub fn by_default() -> Self {
        Self {
            rule_id: None,
            rule_code: "VARIANCE_DEFAULT".to_string(),
            rule_name: "差异分摊默认口径".to_string(),
            basis: None,
        }
    }
}

/// 参与分摊的一个成本对象（基数已由取数方按 [`VarianceAllocOutcome::basis`] 聚合好）。
#[derive(Debug, Clone, PartialEq)]
pub struct VarianceAllocObject {
    /// 成本对象主键。
    pub cost_object_id: Uuid,
    /// 对象号（回显）。
    pub cost_object_no: String,
    /// 对象状态（`OPEN` / `CLOSED`）。
    pub cost_object_status: String,
    /// 该对象在本池基数下的权重（≥ 0）。
    pub basis_value: Decimal,
}

/// 摊到一个成本对象上的差异。
#[derive(Debug, Clone, PartialEq)]
pub struct VarianceAllocLine {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号。
    pub cost_object_no: String,
    /// 对象状态。
    pub cost_object_status: String,
    /// 基数。
    pub basis_value: Decimal,
    /// 分摊比例 = 基数 / 基数合计（8 位，**只用于展示**；基数合计 0 时为 `None`）。
    pub share_rate: Option<Decimal>,
    /// 摊到本对象的差异额（含尾差，符号与待分摊额一致）。
    pub alloc_amount: Decimal,
    /// 尾差调整（只有一行非 0）。
    pub rounding_adj: Decimal,
    /// 是否真的参与了分摊（基数 > 0）。`false` 的行分摊额恒 0，但**仍然出现在结果里**。
    pub participates: bool,
}

/// 一个差异池的分摊结果。
#[derive(Debug, Clone, PartialEq)]
pub struct VarianceAllocOutcome {
    /// 期间。
    pub period_code: String,
    /// 差异类型。
    pub variance_type: String,
    /// 成本要素。
    pub element: String,
    /// 物料（材料类）。
    pub material_id: Option<Uuid>,
    /// 物料编码（回显）。
    pub material_code: Option<String>,
    /// 规则主键（走默认口径时为 `None`）。
    pub rule_id: Option<Uuid>,
    /// 规则编码。
    pub rule_code: String,
    /// 规则名称。
    pub rule_name: String,
    /// 实际使用的分摊基数。
    pub basis: String,
    /// 基数来源（`RULE` / `DEFAULT`）。
    pub basis_source: &'static str,
    /// 差异合计（有符号）。
    pub total_amount: Decimal,
    /// 已归属合计（有符号）。
    pub attributed_amount: Decimal,
    /// 待分摊额 = 合计 − 已归属（有符号）。
    pub allocatable_amount: Decimal,
    /// 基数合计。
    pub basis_total: Decimal,
    /// 逐对象结果（按入参顺序，含基数 0 的对象）。
    pub lines: Vec<VarianceAllocLine>,
    /// 尾差合计（= 待分摊额 − 截断合计）。
    pub rounding_total: Decimal,
}

impl VarianceAllocOutcome {
    /// 本池摊出去的合计（= 待分摊额，断言）。
    pub fn allocated_total(&self) -> Decimal {
        self.lines
            .iter()
            .map(|line| line.alloc_amount)
            .fold(Decimal::ZERO, |acc, value| acc + value)
    }

    /// 恒等式残差：`已归属 + 已摊 − 差异合计`。**必须为 0**。
    pub fn residual(&self) -> Decimal {
        self.attributed_amount + self.allocated_total() - self.total_amount
    }

    /// 本池是否精确闭合（残差为 0 —— 不是「差不多相等」）。
    pub fn closes_exactly(&self) -> bool {
        self.residual().is_zero()
    }

    /// 待分摊额的库层方向字面值（`FAVORABLE` / `UNFAVORABLE`，0 记 `UNFAVORABLE`）。
    pub fn direction(&self) -> &'static str {
        rate_dto::db_direction_of(self.allocatable_amount)
    }

    /// 待分摊额的展示文案（不利 / 有利 / 持平）。
    pub fn direction_label(&self) -> &'static str {
        rate_dto::direction_label_of(self.allocatable_amount)
    }

    /// 参与分摊的对象数（基数 > 0）。
    pub fn participating_count(&self) -> usize {
        self.lines.iter().filter(|line| line.participates).count()
    }

    /// 一句话摘要（日志 / 汇报用）。
    pub fn summary_line(&self) -> String {
        format!(
            "{} {} 按{}：池 {}（已归属 {}）→ 摊 {}（{} 个对象，{} 个零基数，尾差 {}）",
            self.label(),
            self.rule_code,
            basis_label(&self.basis),
            self.total_amount,
            self.attributed_amount,
            self.allocated_total(),
            self.participating_count(),
            self.lines.len() - self.participating_count(),
            self.rounding_total
        )
    }

    /// 报文标识（期间 + 类型 + 物料）。
    fn label(&self) -> String {
        match (&self.material_id, &self.material_code) {
            (Some(_), Some(code)) => format!(
                "期间 {} 的{}（物料 {}）",
                self.period_code,
                variance_type_label(&self.variance_type),
                code
            ),
            (Some(id), None) => format!(
                "期间 {} 的{}（物料 {}）",
                self.period_code,
                variance_type_label(&self.variance_type),
                id
            ),
            _ => format!(
                "期间 {} 的{}",
                self.period_code,
                variance_type_label(&self.variance_type)
            ),
        }
    }
}

/// 一个池的分摊请求：池 + 参与该池分配的对象（**每个池自己的对象集**）。
#[derive(Debug, Clone, PartialEq)]
pub struct VariancePoolRequest {
    /// 差异池。
    pub pool: VariancePool,
    /// 参与本池分配的成本对象。
    pub objects: Vec<VarianceAllocObject>,
}

/// 一个期间的批量分摊请求。
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodVarianceAllocRequest {
    /// 期间（每个池的 `period_code` 必须与它相同）。
    pub period_code: String,
    /// 逐池请求。
    pub pools: Vec<VariancePoolRequest>,
}

/// 一个期间的批量分摊结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodVarianceAllocOutcome {
    /// 期间。
    pub period_code: String,
    /// 逐池结果（按入参顺序）。
    pub outcomes: Vec<VarianceAllocOutcome>,
}

impl PeriodVarianceAllocOutcome {
    /// 差异合计（有符号）。
    pub fn total_amount(&self) -> Decimal {
        self.outcomes
            .iter()
            .map(|outcome| outcome.total_amount)
            .fold(Decimal::ZERO, |acc, value| acc + value)
    }

    /// 已归属合计（有符号）。
    pub fn total_attributed(&self) -> Decimal {
        self.outcomes
            .iter()
            .map(|outcome| outcome.attributed_amount)
            .fold(Decimal::ZERO, |acc, value| acc + value)
    }

    /// 待分摊合计（有符号）。
    pub fn total_allocatable(&self) -> Decimal {
        self.outcomes
            .iter()
            .map(|outcome| outcome.allocatable_amount)
            .fold(Decimal::ZERO, |acc, value| acc + value)
    }

    /// 本次摊出去的合计（有符号）。
    pub fn total_allocated(&self) -> Decimal {
        self.outcomes
            .iter()
            .map(|outcome| outcome.allocated_total())
            .fold(Decimal::ZERO, |acc, value| acc + value)
    }

    /// 结果行数（含零基数行）。
    pub fn line_count(&self) -> usize {
        self.outcomes.iter().map(|outcome| outcome.lines.len()).sum()
    }

    /// 期间级恒等式残差：`Σ已归属 + Σ已摊 − Σ差异合计`。**必须为 0**。
    pub fn residual(&self) -> Decimal {
        self.total_attributed() + self.total_allocated() - self.total_amount()
    }

    /// 期间是否精确闭合。
    pub fn closes_exactly(&self) -> bool {
        self.residual().is_zero()
    }

    /// 一句话摘要。
    pub fn summary_line(&self) -> String {
        format!(
            "{} 差异分摊完成：{} 个池 / {} 行结果 / 合计 {}（已归属 {}）= 摊 {}，残差 {}",
            self.period_code,
            self.outcomes.len(),
            self.line_count(),
            self.total_amount(),
            self.total_attributed(),
            self.total_allocated(),
            self.residual()
        )
    }
}

// ==================== 分摊（纯函数） ====================

/// **单池分摊**：把 `pool.allocatable()` 按 `rule` 指定的基数摊到 `objects` 上。
///
/// 硬断言：`Σ lines.alloc = 待分摊额` 且 `已归属 + 已摊 = 差异合计`（残差 0），
/// 尾差至多落在一行。任一不成立即 500（那是代码问题，不是用户数据问题）。
///
/// 合法边界（**不报错**）：
/// - 待分摊额 = 0（池已全部归属 / 期间没有差异）⇒ 全 0 行，`share_rate` 照给；
/// - 待分摊额 ≠ 0 但某些对象基数为 0 ⇒ 这些行 `participates = false`、分摊 0；
/// - 待分摊额为负（有利差异）⇒ 按 `|池|` 分再整体翻符号。
pub fn allocate_variance(
    rule: &VarianceAllocRule,
    pool: &VariancePool,
    objects: &[VarianceAllocObject],
) -> std::result::Result<VarianceAllocOutcome, ApiError> {
    validate_rule(rule)?;
    validate_pool(pool)?;
    validate_objects(objects)?;
    let (basis, basis_source) = normalize_basis(rule.basis.as_deref(), &pool.variance_type)?;

    let allocatable = money(pool.allocatable());
    let mut basis_total = Decimal::ZERO;
    for object in objects {
        basis_total += object.basis_value;
    }

    if !allocatable.is_zero() && objects.is_empty() {
        return Err(no_cost_object(pool, allocatable, &basis));
    }
    if !allocatable.is_zero() && basis_total.is_zero() {
        return Err(basis_total_zero(pool, &basis, allocatable));
    }
    if allocatable.abs() > max_money() {
        return Err(invalid(format!(
            "{} 的待分摊额 {} 超出金额列 NUMERIC(18,4) 的范围；请先核查差异合计与已归属额是否填错了量级",
            pool.label(),
            allocatable
        )));
    }

    // 最大余数法要求非负池：有利差异（负池）按绝对值分，结果整体翻符号。
    let negative = allocatable < Decimal::ZERO;
    let magnitude = if negative { -allocatable } else { allocatable };
    let alloc_objects = objects
        .iter()
        .map(|object| AllocObject {
            cost_object_id: object.cost_object_id,
            cost_object_no: object.cost_object_no.clone(),
            cost_object_status: object.cost_object_status.clone(),
            basis_value: object.basis_value,
        })
        .collect::<Vec<_>>();
    let raw = rule_dto::allocate(magnitude, &alloc_objects)?;
    debug_assert_eq!(raw.lines.len(), objects.len(), "分摊结果行数必须与入参对象数一致");

    let mut lines = Vec::with_capacity(objects.len());
    let mut rounding_total = Decimal::ZERO;
    for (object, line) in objects.iter().zip(raw.lines.iter()) {
        let alloc_amount = if negative {
            -line.alloc_amount
        } else {
            line.alloc_amount
        };
        let rounding_adj = if negative {
            -line.rounding_adj
        } else {
            line.rounding_adj
        };
        rounding_total += rounding_adj;
        let share_rate = if basis_total.is_zero() {
            None
        } else {
            Some((line.basis_value / basis_total).round_dp(SHARE_SCALE))
        };
        lines.push(VarianceAllocLine {
            cost_object_id: object.cost_object_id,
            cost_object_no: object.cost_object_no.clone(),
            cost_object_status: object.cost_object_status.clone(),
            basis_value: line.basis_value,
            share_rate,
            alloc_amount,
            rounding_adj,
            participates: !line.basis_value.is_zero(),
        });
    }

    let outcome = VarianceAllocOutcome {
        period_code: pool.period_code.clone(),
        variance_type: pool.variance_type.clone(),
        element: pool.element.clone(),
        material_id: pool.material_id,
        material_code: pool.material_code.clone(),
        rule_id: rule.rule_id,
        rule_code: rule.rule_code.clone(),
        rule_name: rule.rule_name.clone(),
        basis,
        basis_source,
        total_amount: pool.total_amount,
        attributed_amount: pool.attributed_amount,
        allocatable_amount: allocatable,
        basis_total,
        lines,
        rounding_total,
    };
    ensure_conservation(&outcome)?;
    Ok(outcome)
}

/// **按期批量分摊**：逐池分摊 + 期间级守恒断言。
///
/// 三个入参护栏（都 400，防「同一笔差异摊两遍」）：
/// ① 每个池的 `period_code` 必须等于请求的期间；
/// ② 池键 `(差异类型, 物料)` **不许重复** —— 重复说明该聚合的没有聚合，两行会各摊一遍；
/// ③ 同池内成本对象也不许重复（在 [`allocate_variance`] 里拦）。
///
/// 空池列表是合法输入（该期间没有差异 ⇒ 没有池），返回 0 个结果、闭合。
pub fn allocate_period(
    rule: &VarianceAllocRule,
    request: &PeriodVarianceAllocRequest,
) -> std::result::Result<PeriodVarianceAllocOutcome, ApiError> {
    rule_dto::period_bounds(&request.period_code)?;
    let mut keys: HashSet<(String, Option<Uuid>)> = HashSet::new();
    let mut outcomes = Vec::with_capacity(request.pools.len());
    for entry in &request.pools {
        if entry.pool.period_code != request.period_code {
            return Err(invalid(format!(
                "池的期间 {} 与本次分摊的期间 {} 不一致：一次只能摊一个期间\
                 （跨期摊差异会让两期的差异报表都对不上）",
                entry.pool.period_code, request.period_code
            )));
        }
        if !keys.insert(entry.pool.key()) {
            return Err(invalid(format!(
                "{} 出现了两个池：同一 (差异类型, 物料) 只能有一个池，\
                 请先把差异行按「期间 × 类型 × 物料」聚合好再摊（两个池会把同一笔差异摊两遍）",
                entry.pool.label()
            )));
        }
        outcomes.push(allocate_variance(rule, &entry.pool, &entry.objects)?);
    }
    let outcome = PeriodVarianceAllocOutcome {
        period_code: request.period_code.clone(),
        outcomes,
    };
    ensure_period_conservation(&outcome)?;
    Ok(outcome)
}

// ==================== 守恒断言（代码问题 ⇒ 500） ====================

/// 单池守恒：`Σ分摊 = 待分摊额`、`已归属 + 已摊 = 合计`、尾差至多一行、每行符号与池一致。
///
/// 失败是 `INTERNAL_ERROR`：`rule_dto::allocate` 已经保证过一遍，这里验的是
/// **转换与翻符号之后**的值 —— 「算法对、映射错」同样会让账永远不平。
fn ensure_conservation(outcome: &VarianceAllocOutcome) -> std::result::Result<(), ApiError> {
    let allocated = outcome.allocated_total();
    if allocated != outcome.allocatable_amount {
        return Err(conservation_broken(
            &outcome.label(),
            format!(
                "各对象分摊额之和 {} ≠ 待分摊额 {}（合计 {} − 已归属 {}）；尾差合计 {}",
                allocated,
                outcome.allocatable_amount,
                outcome.total_amount,
                outcome.attributed_amount,
                outcome.rounding_total
            ),
        ));
    }
    if !outcome.closes_exactly() {
        return Err(conservation_broken(
            &outcome.label(),
            format!(
                "恒等式不闭合：已归属 {} + 已摊 {} ≠ 差异合计 {}（残差 {}）",
                outcome.attributed_amount,
                allocated,
                outcome.total_amount,
                outcome.residual()
            ),
        ));
    }
    let adjusted = outcome.lines.iter().filter(|line| !line.rounding_adj.is_zero()).count();
    if adjusted > 1 {
        return Err(conservation_broken(
            &outcome.label(),
            format!("尾差落在 {adjusted} 行上；最大余数法只允许一行收尾差（§4.4.3 ③）"),
        ));
    }
    let pool_negative = outcome.allocatable_amount < Decimal::ZERO;
    if let Some(line) = outcome.lines.iter().find(|line| {
        if pool_negative {
            line.alloc_amount > Decimal::ZERO
        } else {
            line.alloc_amount < Decimal::ZERO
        }
    }) {
        return Err(conservation_broken(
            &outcome.label(),
            format!(
                "对象「{}」的分摊额 {} 与待分摊额 {} 符号相反（翻符号逻辑出错）",
                line.cost_object_no, line.alloc_amount, outcome.allocatable_amount
            ),
        ));
    }
    let base = outcome
        .lines
        .iter()
        .map(|line| line.alloc_amount - line.rounding_adj)
        .fold(Decimal::ZERO, |acc, value| acc + value);
    if (outcome.allocatable_amount - base) != outcome.rounding_total {
        return Err(conservation_broken(
            &outcome.label(),
            format!(
                "尾差合计 {} ≠ 待分摊额 − 截断合计 {}（截断合计 {}）",
                outcome.rounding_total,
                outcome.allocatable_amount - base,
                base
            ),
        ));
    }
    Ok(())
}

/// 期间级守恒：`Σ已归属 + Σ已摊 = Σ差异合计`。
fn ensure_period_conservation(outcome: &PeriodVarianceAllocOutcome) -> std::result::Result<(), ApiError> {
    if !outcome.closes_exactly() {
        return Err(conservation_broken(
            &outcome.period_code,
            format!(
                "期间级恒等式不闭合：Σ已归属 {} + Σ已摊 {} ≠ Σ差异合计 {}（残差 {}）",
                outcome.total_attributed(),
                outcome.total_allocated(),
                outcome.total_amount(),
                outcome.residual()
            ),
        ));
    }
    Ok(())
}

// ==================== 入参护栏（用户数据 ⇒ 400 / 409） ====================

/// 规则护栏：规则编码必填（日志与 remark 要能追到「按哪条规则摊的」）。
fn validate_rule(rule: &VarianceAllocRule) -> std::result::Result<(), ApiError> {
    if rule.rule_code.trim().is_empty() {
        return Err(invalid(
            "分摊规则编码不能为空：差异分摊结果要能追到「按哪条规则摊的」，\
             走默认口径请用 VarianceAllocRule::by_default()",
        ));
    }
    Ok(())
}

/// 池护栏：期间格式 / 差异类型值域 / 要素与类型配对 / 材料类必须带物料 / 金额标度与量级。
fn validate_pool(pool: &VariancePool) -> std::result::Result<(), ApiError> {
    rule_dto::period_bounds(&pool.period_code)?;
    let canonical = canonical_element_of(&pool.variance_type).ok_or_else(|| {
        invalid(format!(
            "差异类型 {} 不认识：只能是 {} 之一（库层 ck_biz_cost_var_type 同口径）",
            pool.variance_type,
            VARIANCE_TYPES_ALL.join(" / ")
        ))
    })?;
    if !ELEMENTS.contains(&pool.element.as_str()) {
        return Err(invalid(format!(
            "成本要素 {} 不合法：只能是 {} 之一（库层 ck_biz_cost_var_elem 同口径）",
            pool.element,
            ELEMENTS.join(" / ")
        )));
    }
    if pool.element != canonical {
        return Err(invalid(format!(
            "{}只能挂在要素 {} 上（收到 {}）：要素决定这行差异汇入哪一项三要素合计，\
             配错就造出一行永远对不上合计口径的脏数据",
            variance_type_label(&pool.variance_type),
            canonical,
            pool.element
        )));
    }
    if canonical == ELEMENT_MATERIAL && pool.material_id.is_none() {
        return Err(invalid(format!(
            "{}池必须指定物料：材料类差异的权重是「该对象用了多少这个料」，\
             不带物料就分不清摊给谁（摊出来的权重会是别的料的用量）",
            variance_type_label(&pool.variance_type)
        )));
    }
    if canonical != ELEMENT_MATERIAL && pool.material_id.is_some() {
        return Err(invalid(format!(
            "{}不是材料类差异，池上不能挂物料（收到 {}）：\
             挂了物料会让同一笔差异既属于「物料池」又属于「要素池」，报表上会被数两遍",
            variance_type_label(&pool.variance_type),
            pool.material_id.map(|id| id.to_string()).unwrap_or_default()
        )));
    }
    check_amount("差异合计", &pool.label(), pool.total_amount)?;
    check_amount("已归属差异合计", &pool.label(), pool.attributed_amount)?;
    Ok(())
}

/// 金额护栏：4 位标度（= 落库列 `NUMERIC(18,4)`）且在量级范围内。
fn check_amount(what: &str, label: &str, value: Decimal) -> std::result::Result<(), ApiError> {
    if value.normalize().scale() > cost_variance_scale() {
        return Err(invalid(format!(
            "{label} 的{what} {value} 超过 4 位小数：差异表金额列是 NUMERIC(18,4)，\
             多出来的位会被库静默截断，导致「分摊合计 ≠ 池」（口径尾差请保留在 4 位内部）"
        )));
    }
    if value.abs() > max_money() {
        return Err(invalid(format!(
            "{label} 的{what} {value} 超出金额列 NUMERIC(18,4) 的范围，请先核查量级"
        )));
    }
    Ok(())
}

/// 差异表金额列的标度（`biz_cost_variance.amount NUMERIC(18,4)`）。
fn cost_variance_scale() -> u32 {
    period_item_dto::MONEY_SCALE
}

/// 对象护栏：不重复、基数 ≥ 0、状态在值域内。
fn validate_objects(objects: &[VarianceAllocObject]) -> std::result::Result<(), ApiError> {
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(objects.len());
    for object in objects {
        if object.cost_object_no.trim().is_empty() {
            return Err(invalid(format!(
                "成本对象 {} 缺对象号：报文与报表都要靠对象号定位，不能为空",
                object.cost_object_id
            )));
        }
        if !seen.insert(object.cost_object_id) {
            return Err(invalid(format!(
                "成本对象「{}」在同一次分摊里出现了两次：\
                 重复行会把同一批货的差异摊两遍，请按对象聚合后再摊",
                object.cost_object_no
            )));
        }
        if object.basis_value < Decimal::ZERO {
            return Err(invalid(format!(
                "成本对象「{}」的基数不能为负（收到 {}）：负用量 / 负工时不是「负生产」，\
                 是上游取数算错了；静默取绝对值等于把错账藏起来",
                object.cost_object_no, object.basis_value
            )));
        }
        if !OBJECT_STATUSES.contains(&object.cost_object_status.as_str()) {
            return Err(invalid(format!(
                "成本对象「{}」的状态 {} 不在这条分摊的范围内（只收 {}）：\
                 已作废 / 已取消的批次不该再承担本期差异",
                object.cost_object_no,
                object.cost_object_status,
                OBJECT_STATUSES.join(" / ")
            )));
        }
    }
    Ok(())
}

// ==================== 错误构造 ====================

/// 400：纯入参不合法（不新增业务码，见 `shared/error_code.rs`：纯入参校验走 `10001`）。
fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

/// 409 + `66062`：池里有待分摊额，但一个参与分摊的成本对象都没有。
///
/// **复用费用分摊的码**（`ERP_COST_ALLOC_NO_OBJECT`）而不是新开一个：失败模式与修法
/// 完全相同（「当期没有在产对象」），新码只会让前端多写一个分支、客服多背一条话术。
fn no_cost_object(pool: &VariancePool, allocatable: Decimal, basis: &str) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_ALLOC_NO_OBJECT,
        format!(
            "{} 有 {allocatable} 待分摊，但本期一个参与分摊的成本对象都没有。\n\
             参与分摊的条件是：对象属于本期间、状态在制或已关批、且当期有基数数据（{}）。\n\
             系统不做「留在池里下期再摊」的静默处理 —— 那正是漏摊、差异永远摊不完的来源。\n\
             请先补当期该物料的领料 / 工时 / 完工数，或确认该池确实不该摊（那就先把它挂到对象上再摊）",
            pool.label(),
            basis_requirement(basis)
        ),
    )
}

/// 409 + `66063`：池里有待分摊额，但该基数下所有对象的基数合计为 0。
fn basis_total_zero(pool: &VariancePool, basis: &str, allocatable: Decimal) -> ApiError {
    ApiError::new(
        409,
        error_code::ERP_COST_ALLOC_BASIS_ZERO,
        format!(
            "{} 有 {allocatable} 待分摊，但分配标准「{}」的基数合计为 0，无法按比例摊\
             （分摊额全 0 会让池里永远剩 {allocatable}）。\n\
             这个基数要的是：{}。\n\
             请检查当期是否真的没有这份基数；如果这个标准本就不适用，\
             请改用 {} 中的其它基数重摊",
            pool.label(),
            basis_label(basis),
            basis_requirement(basis),
            VARIANCE_ALLOC_BASES.join(" / ")
        ),
    )
}

/// 500：守恒断言被打破（算法 / 翻符号 / 映射出错，不是用户数据问题）。
fn conservation_broken(label: &str, detail: String) -> ApiError {
    tracing::error!(label, detail = %detail, "差异分摊守恒断言失败");
    ApiError::new(
        500,
        error_code::INTERNAL_ERROR,
        format!("差异分摊守恒断言失败（{label}）：{detail}"),
    )
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn text(value: Decimal) -> String {
        format!("{:.4}", value)
    }

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn object_with_id(id: u128, no: &str, basis: &str) -> VarianceAllocObject {
        VarianceAllocObject {
            cost_object_id: uid(id),
            cost_object_no: no.to_string(),
            cost_object_status: "OPEN".to_string(),
            basis_value: dec(basis),
        }
    }

    fn pool(variance_type: &str, material: bool, total: &str, attributed: &str) -> VariancePool {
        let element = canonical_element_of(variance_type).expect("已知类型").to_string();
        VariancePool {
            period_code: "2026-11".to_string(),
            variance_type: variance_type.to_string(),
            element,
            material_id: if material { Some(uid(900)) } else { None },
            material_code: if material { Some("M-001".to_string()) } else { None },
            total_amount: dec(total),
            attributed_amount: dec(attributed),
        }
    }

    fn mat_pool(total: &str, attributed: &str) -> VariancePool {
        pool(rate_dto::VARIANCE_MAT_PRICE, true, total, attributed)
    }

    /// 三个等基数对象（1:1:1）。
    fn three_equal() -> Vec<VarianceAllocObject> {
        vec![
            object_with_id(1, "CB-1", "100"),
            object_with_id(2, "CB-2", "100"),
            object_with_id(3, "CB-3", "100"),
        ]
    }

    // ---------- 默认口径 ----------

    #[test]
    fn default_basis_maps_all_six_types() {
        let table = [
            (rate_dto::VARIANCE_MAT_PRICE, BASIS_ACTUAL_QTY),
            (cost_qty_variance_dto::VARIANCE_MAT_QTY, BASIS_ACTUAL_QTY),
            (rate_dto::VARIANCE_LABOR_RATE, BASIS_LABOR_HOURS),
            (cost_qty_variance_dto::VARIANCE_LABOR_EFF, BASIS_LABOR_HOURS),
            (VARIANCE_OH_SPEND, BASIS_STD_AMOUNT),
            (VARIANCE_OH_VOLUME, BASIS_STD_AMOUNT),
        ];
        for (variance_type, expected) in table {
            assert_eq!(
                default_basis_of(variance_type).expect("已知类型"),
                expected,
                "{variance_type} 的默认基数"
            );
            assert!(VARIANCE_ALLOC_BASES.contains(&expected), "{expected} 必须在四值域内");
        }
        assert_eq!(default_basis_of("FOO").unwrap_err().code, error_code::PARAM_INVALID);
    }

    #[test]
    fn variance_labels_agree_with_the_formula_modules() {
        // 四类已由公式卡命名的差异，名字必须逐字相同（两张界面不能叫两个名字）
        assert_eq!(
            variance_type_label(rate_dto::VARIANCE_MAT_PRICE),
            rate_dto::variance_label(rate_dto::VARIANCE_MAT_PRICE)
        );
        assert_eq!(
            variance_type_label(rate_dto::VARIANCE_LABOR_RATE),
            rate_dto::variance_label(rate_dto::VARIANCE_LABOR_RATE)
        );
        assert_eq!(
            variance_type_label(cost_qty_variance_dto::VARIANCE_MAT_QTY),
            cost_qty_variance_dto::variance_label(cost_qty_variance_dto::VARIANCE_MAT_QTY)
        );
        assert_eq!(
            variance_type_label(cost_qty_variance_dto::VARIANCE_LABOR_EFF),
            cost_qty_variance_dto::variance_label(cost_qty_variance_dto::VARIANCE_LABOR_EFF)
        );
        assert_eq!(variance_type_label(VARIANCE_OH_SPEND), "制造费用耗费差异");
        assert_eq!(variance_type_label(VARIANCE_OH_VOLUME), "制造费用产量差异");
    }

    #[test]
    fn canonical_element_agrees_with_the_qty_module() {
        for variance_type in cost_qty_variance_dto::QTY_VARIANCE_TYPES {
            assert_eq!(
                canonical_element_of(variance_type),
                cost_qty_variance_dto::canonical_element(variance_type),
                "{variance_type} 的要素配对必须与 M5-13 同口径"
            );
        }
        assert_eq!(
            canonical_element_of(rate_dto::VARIANCE_MAT_PRICE),
            Some(ELEMENT_MATERIAL)
        );
        assert_eq!(canonical_element_of(rate_dto::VARIANCE_LABOR_RATE), Some(ELEMENT_LABOR));
        assert_eq!(canonical_element_of(VARIANCE_OH_SPEND), Some(ELEMENT_OVERHEAD));
        assert_eq!(canonical_element_of("FOO"), None);
    }

    #[test]
    fn normalize_basis_prefers_the_rule_and_marks_its_source() {
        let (basis, source) = normalize_basis(None, rate_dto::VARIANCE_MAT_PRICE).expect("默认");
        assert_eq!(basis, BASIS_ACTUAL_QTY);
        assert_eq!(source, BASIS_SOURCE_DEFAULT);

        let (basis, source) = normalize_basis(Some("  "), rate_dto::VARIANCE_MAT_PRICE).expect("空串");
        assert_eq!(basis, BASIS_ACTUAL_QTY);
        assert_eq!(source, BASIS_SOURCE_DEFAULT);

        let (basis, source) = normalize_basis(Some(BASIS_LABOR_HOURS), rate_dto::VARIANCE_MAT_PRICE).expect("规则覆盖");
        assert_eq!(basis, BASIS_LABOR_HOURS);
        assert_eq!(source, BASIS_SOURCE_RULE);
    }

    #[test]
    fn normalize_basis_rejects_typos_instead_of_silently_defaulting() {
        let err = normalize_basis(Some("HOURS"), rate_dto::VARIANCE_MAT_PRICE).unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("HOURS"), "报文要点名拼错的基数：{}", err.msg);
        assert!(err.msg.contains("ACTUAL_QTY"), "报文要给出合法值域：{}", err.msg);
    }

    #[test]
    fn rule_by_default_has_no_basis_and_a_traceable_code() {
        let rule = VarianceAllocRule::by_default();
        assert!(rule.basis.is_none());
        assert_eq!(rule.rule_code, "VARIANCE_DEFAULT");
        assert!(rule.rule_id.is_none());
        let rule = VarianceAllocRule::with_basis("VAR-01", "按工时摊全部差异", BASIS_LABOR_HOURS);
        assert_eq!(rule.basis.as_deref(), Some(BASIS_LABOR_HOURS));
    }

    // ---------- 分摊算数 ----------

    #[test]
    fn allocates_material_price_variance_by_actual_qty() {
        // 价差 600（不利）按实际用量 500 / 400 / 300 摊 → 250 / 200 / 150
        let objects = vec![
            object_with_id(11, "CB-1", "500"),
            object_with_id(12, "CB-2", "400"),
            object_with_id(13, "CB-3", "300"),
        ];
        let outcome =
            allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("600", "0"), &objects).expect("分摊");
        assert_eq!(outcome.basis, BASIS_ACTUAL_QTY);
        assert_eq!(outcome.basis_source, BASIS_SOURCE_DEFAULT);
        assert_eq!(outcome.basis_total, dec("1200"));
        let amounts: Vec<String> = outcome.lines.iter().map(|l| text(l.alloc_amount)).collect();
        assert_eq!(amounts, vec!["250.0000", "200.0000", "150.0000"]);
        assert_eq!(text(outcome.allocated_total()), "600.0000");
        assert_eq!(outcome.rounding_total, Decimal::ZERO);
        assert!(outcome.closes_exactly());
        assert!(outcome.lines.iter().all(|line| line.participates));
        assert_eq!(outcome.participating_count(), 3);
        assert_eq!(outcome.direction(), "UNFAVORABLE");
        assert_eq!(outcome.direction_label(), "不利");
        // 比例只用于展示（8 位）
        assert_eq!(outcome.lines[0].share_rate, Some(dec("0.41666667")));
        assert_eq!(outcome.lines[2].share_rate, Some(dec("0.25")));
    }

    #[test]
    fn largest_remainder_closes_the_pool_exactly() {
        // 1000 按三个等基数摊：333.3333 × 2 + 333.3334（ARCH-9 D-9 / 算例 B2）。
        // 三个余数完全相同 ⇒ 并列规则生效：基数大 → 对象 id 字典序小 ⇒ 尾差落 CB-1。
        let outcome =
            allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("1000", "0"), &three_equal()).expect("分摊");
        let amounts: Vec<String> = outcome.lines.iter().map(|l| text(l.alloc_amount)).collect();
        assert_eq!(amounts, vec!["333.3334", "333.3333", "333.3333"]);
        assert_eq!(text(outcome.allocated_total()), "1000.0000", "守恒：Σ = 池");
        assert_eq!(text(outcome.rounding_total), "0.0001");
        let adjusted: Vec<&str> = outcome
            .lines
            .iter()
            .filter(|line| !line.rounding_adj.is_zero())
            .map(|line| line.cost_object_no.as_str())
            .collect();
        assert_eq!(
            adjusted,
            vec!["CB-1"],
            "尾差只落一行（余数最大；并列时按共享算法的三级规则：基数大 → id 小）"
        );
        assert!(outcome.closes_exactly());
    }

    #[test]
    fn rounding_lands_on_one_line_even_with_many_objects() {
        let objects = (1..=7)
            .map(|index| object_with_id(index, &format!("CB-{index}"), "1"))
            .collect::<Vec<_>>();
        let outcome =
            allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("100", "0"), &objects).expect("分摊");
        let adjusted = outcome.lines.iter().filter(|line| !line.rounding_adj.is_zero()).count();
        assert_eq!(adjusted, 1, "最大余数法只允许一行收尾差");
        assert_eq!(text(outcome.allocated_total()), "100.0000");
        // 100 / 7 = 14.285714…；截断 14.2857 × 7 = 99.9999，尾差 0.0001
        assert_eq!(text(outcome.rounding_total), "0.0001");
    }

    #[test]
    fn favorable_pool_keeps_the_sign_and_still_closes() {
        // 有利差异 −1000 按三等分 → 每行 ≤ 0，Σ = −1000（翻符号后仍精确闭合）
        let outcome = allocate_variance(
            &VarianceAllocRule::by_default(),
            &mat_pool("-1000", "0"),
            &three_equal(),
        )
        .expect("有利差异也要能摊");
        let amounts: Vec<String> = outcome.lines.iter().map(|l| text(l.alloc_amount)).collect();
        assert_eq!(amounts, vec!["-333.3334", "-333.3333", "-333.3333"]);
        assert_eq!(text(outcome.allocated_total()), "-1000.0000");
        assert_eq!(text(outcome.rounding_total), "-0.0001");
        assert!(outcome.lines.iter().all(|line| line.alloc_amount <= Decimal::ZERO));
        assert_eq!(outcome.direction(), "FAVORABLE");
        assert_eq!(outcome.direction_label(), "有利");
        assert!(outcome.closes_exactly());
    }

    #[test]
    fn already_attributed_part_is_not_allocated_twice() {
        // 合计 1000，其中 400 已挂对象 → 只摊 600；已归属 + 已摊 = 1000
        let outcome = allocate_variance(
            &VarianceAllocRule::by_default(),
            &mat_pool("1000", "400"),
            &three_equal(),
        )
        .expect("分摊");
        assert_eq!(text(outcome.allocatable_amount), "600.0000");
        assert_eq!(text(outcome.allocated_total()), "600.0000");
        assert_eq!(text(outcome.attributed_amount), "400.0000");
        assert_eq!(outcome.residual(), Decimal::ZERO);
        assert!(outcome.closes_exactly());
        let amounts: Vec<String> = outcome.lines.iter().map(|l| text(l.alloc_amount)).collect();
        assert_eq!(amounts, vec!["200.0000", "200.0000", "200.0000"]);
    }

    #[test]
    fn attributed_can_carry_the_opposite_sign() {
        // 合计 +100（不利），已挂 −500（有利）→ 待摊 +600
        let outcome = allocate_variance(
            &VarianceAllocRule::by_default(),
            &mat_pool("100", "-500"),
            &three_equal(),
        )
        .expect("分摊");
        assert_eq!(text(outcome.allocatable_amount), "600.0000");
        assert_eq!(text(outcome.allocated_total()), "600.0000");
        assert!(outcome.closes_exactly(), "已归属符号相反也要能闭合");

        // 合计 +100，已挂 +500 → 待摊 −400（其余未挂的行是节约）
        let outcome = allocate_variance(
            &VarianceAllocRule::by_default(),
            &mat_pool("100", "500"),
            &three_equal(),
        )
        .expect("分摊");
        assert_eq!(text(outcome.allocatable_amount), "-400.0000");
        assert!(outcome.lines.iter().all(|line| line.alloc_amount <= Decimal::ZERO));
        assert!(outcome.closes_exactly());
    }

    #[test]
    fn fully_attributed_pool_allocates_nothing_and_is_not_an_error() {
        let outcome = allocate_variance(
            &VarianceAllocRule::by_default(),
            &mat_pool("800", "800"),
            &three_equal(),
        )
        .expect("池已全部归属是正常形态");
        assert_eq!(outcome.allocatable_amount, Decimal::ZERO);
        assert!(outcome.lines.iter().all(|line| line.alloc_amount.is_zero()));
        assert!(outcome.closes_exactly());
        // 比例照给（基数仍然有意义，前端要显示「各对象占多少权重」）
        assert_eq!(outcome.lines[0].share_rate, Some(dec("0.33333333")));
        assert_eq!(outcome.rounding_total, Decimal::ZERO);
    }

    #[test]
    fn zero_pool_without_objects_is_fine() {
        let outcome = allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("0", "0"), &[])
            .expect("期间没有差异是正常形态");
        assert!(outcome.lines.is_empty());
        assert_eq!(outcome.basis_total, Decimal::ZERO);
        assert!(outcome.closes_exactly());
        assert_eq!(outcome.participating_count(), 0);
    }

    #[test]
    fn zero_basis_object_stays_in_the_result() {
        // 三个对象里 CB-2 本期一件没做（基数 0）：留在结果里、分摊 0、标「未参与」
        let objects = vec![
            object_with_id(21, "CB-1", "600"),
            object_with_id(22, "CB-2", "0"),
            object_with_id(23, "CB-3", "400"),
        ];
        let outcome =
            allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("1000", "0"), &objects).expect("分摊");
        assert_eq!(outcome.lines.len(), 3, "零基数对象不许从结果里消失");
        assert!(!outcome.lines[1].participates);
        assert!(outcome.lines[1].alloc_amount.is_zero());
        assert_eq!(outcome.lines[1].share_rate, Some(Decimal::ZERO));
        assert_eq!(outcome.participating_count(), 2);
        let amounts: Vec<String> = outcome.lines.iter().map(|l| text(l.alloc_amount)).collect();
        assert_eq!(amounts, vec!["600.0000", "0.0000", "400.0000"]);
        assert_eq!(text(outcome.allocated_total()), "1000.0000");
    }

    #[test]
    fn rule_basis_overrides_the_type_default() {
        // 材料价差按实际工时摊（客户的另一种管理口径）：显式规则必须被尊重
        let rule = VarianceAllocRule::with_basis("VAR-01", "价差按工时摊", BASIS_LABOR_HOURS);
        let objects = vec![object_with_id(31, "CB-1", "80"), object_with_id(32, "CB-2", "20")];
        let outcome = allocate_variance(&rule, &mat_pool("500", "0"), &objects).expect("分摊");
        assert_eq!(outcome.basis, BASIS_LABOR_HOURS);
        assert_eq!(outcome.basis_source, BASIS_SOURCE_RULE);
        assert_eq!(outcome.rule_code, "VAR-01");
        let amounts: Vec<String> = outcome.lines.iter().map(|l| text(l.alloc_amount)).collect();
        assert_eq!(amounts, vec!["400.0000", "100.0000"]);
        assert_eq!(text(outcome.allocated_total()), "500.0000");
    }

    #[test]
    fn six_types_each_allocate_end_to_end() {
        let table = [
            (rate_dto::VARIANCE_MAT_PRICE, true, "600"),
            (cost_qty_variance_dto::VARIANCE_MAT_QTY, true, "3840"),
            (rate_dto::VARIANCE_LABOR_RATE, false, "-180"),
            (cost_qty_variance_dto::VARIANCE_LABOR_EFF, false, "500"),
            (VARIANCE_OH_SPEND, false, "1200"),
            (VARIANCE_OH_VOLUME, false, "-400"),
        ];
        for (variance_type, material, total) in table {
            let pool = pool(variance_type, material, total, "0");
            let objects = three_equal();
            let outcome = allocate_variance(&VarianceAllocRule::by_default(), &pool, &objects)
                .unwrap_or_else(|err| panic!("{variance_type} 分摊失败：{}", err.msg));
            assert_eq!(
                text(outcome.allocated_total()),
                text(dec(total)),
                "{variance_type} 守恒"
            );
            assert!(outcome.closes_exactly(), "{variance_type} 恒等式");
            assert_eq!(
                outcome.basis,
                default_basis_of(variance_type).expect("已知类型"),
                "{variance_type} 用的是默认基数"
            );
        }
    }

    #[test]
    fn delegates_to_the_overhead_largest_remainder_arithmetic() {
        // 算数只有一份：与费用分摊的 allocate 逐行比相等（防手抄实现）
        let objects = vec![
            object_with_id(41, "CB-1", "1"),
            object_with_id(42, "CB-2", "1"),
            object_with_id(43, "CB-3", "1"),
            object_with_id(44, "CB-4", "2"),
        ];
        let outcome =
            allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("1234.5678", "0"), &objects).expect("分摊");
        let alloc_objects = objects
            .iter()
            .map(|object| AllocObject {
                cost_object_id: object.cost_object_id,
                cost_object_no: object.cost_object_no.clone(),
                cost_object_status: object.cost_object_status.clone(),
                basis_value: object.basis_value,
            })
            .collect::<Vec<_>>();
        let reference = rule_dto::allocate(dec("1234.5678"), &alloc_objects).expect("费用分摊同一算数");
        assert_eq!(outcome.lines.len(), reference.lines.len());
        for (mine, theirs) in outcome.lines.iter().zip(reference.lines.iter()) {
            assert_eq!(mine.alloc_amount, theirs.alloc_amount, "逐行金额必须同源");
            assert_eq!(mine.rounding_adj, theirs.rounding_adj, "逐行尾差必须同源");
            assert_eq!(mine.basis_value, theirs.basis_value);
        }
        assert_eq!(outcome.basis_total, reference.basis_total);
    }

    #[test]
    fn share_rate_must_not_be_used_as_money() {
        // 用 share_rate 乘回去会差一分：这正是「比例只展示」的原因
        let outcome =
            allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("1000", "0"), &three_equal()).expect("分摊");
        let from_share = outcome.lines.iter().fold(Decimal::ZERO, |acc, line| {
            acc + money(line.share_rate.expect("有基数就有比例") * outcome.allocatable_amount)
        });
        assert_eq!(text(from_share), "999.9999", "用比例算钱会少 0.0001");
        assert_ne!(
            from_share,
            outcome.allocated_total(),
            "比例乘回去 ≠ 分摊额 —— 落表只能用分摊额"
        );
        assert_eq!(text(outcome.allocated_total()), "1000.0000", "真正落表的是分摊额");
        assert!(outcome.closes_exactly(), "闭合靠最大余数法，不靠比例");
    }

    #[test]
    fn summary_line_names_the_rule_pool_and_totals() {
        let outcome = allocate_variance(
            &VarianceAllocRule::with_basis("VAR-09", "按完工数摊", BASIS_OUTPUT_QTY),
            &mat_pool("900", "300"),
            &three_equal(),
        )
        .expect("分摊");
        let summary = outcome.summary_line();
        assert!(summary.contains("2026-11"), "{summary}");
        assert!(summary.contains("材料价差"), "{summary}");
        assert!(summary.contains("M-001"), "{summary}");
        assert!(summary.contains("VAR-09"), "{summary}");
        assert!(summary.contains("按本期完工数量分摊"), "{summary}");
        assert!(summary.contains("900"), "{summary}");
        assert!(summary.contains("300"), "{summary}");
    }

    // ---------- 批量（期间级）----------

    #[test]
    fn period_allocates_several_pools_and_closes() {
        let request = PeriodVarianceAllocRequest {
            period_code: "2026-11".to_string(),
            pools: vec![
                VariancePoolRequest {
                    pool: mat_pool("600", "0"),
                    objects: three_equal(),
                },
                VariancePoolRequest {
                    pool: VariancePool {
                        material_id: Some(uid(901)),
                        material_code: Some("M-002".to_string()),
                        ..mat_pool("400", "100")
                    },
                    objects: vec![object_with_id(51, "CB-1", "1"), object_with_id(52, "CB-2", "3")],
                },
            ],
        };
        let outcome = allocate_period(&VarianceAllocRule::by_default(), &request).expect("批量分摊");
        assert_eq!(outcome.outcomes.len(), 2);
        assert_eq!(outcome.line_count(), 5);
        assert_eq!(text(outcome.total_amount()), "1000.0000");
        assert_eq!(text(outcome.total_attributed()), "100.0000");
        assert_eq!(text(outcome.total_allocatable()), "900.0000");
        assert_eq!(text(outcome.total_allocated()), "900.0000");
        assert_eq!(outcome.residual(), Decimal::ZERO);
        assert!(outcome.closes_exactly());
        assert!(outcome.summary_line().contains("2 个池"), "{}", outcome.summary_line());
        assert!(
            outcome.summary_line().contains("5 行结果"),
            "{}",
            outcome.summary_line()
        );
    }

    #[test]
    fn period_rejects_a_pool_from_another_period() {
        let request = PeriodVarianceAllocRequest {
            period_code: "2026-11".to_string(),
            pools: vec![VariancePoolRequest {
                pool: VariancePool {
                    period_code: "2026-10".to_string(),
                    ..mat_pool("600", "0")
                },
                objects: three_equal(),
            }],
        };
        let err = allocate_period(&VarianceAllocRule::by_default(), &request).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("2026-10"), "{}", err.msg);
        assert!(err.msg.contains("2026-11"), "{}", err.msg);
    }

    #[test]
    fn period_rejects_duplicate_pool_keys() {
        let request = PeriodVarianceAllocRequest {
            period_code: "2026-11".to_string(),
            pools: vec![
                VariancePoolRequest {
                    pool: mat_pool("600", "0"),
                    objects: three_equal(),
                },
                VariancePoolRequest {
                    pool: mat_pool("500", "0"),
                    objects: three_equal(),
                },
            ],
        };
        let err = allocate_period(&VarianceAllocRule::by_default(), &request).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("两个池"), "{}", err.msg);
        assert!(err.msg.contains("摊两遍"), "{}", err.msg);
    }

    #[test]
    fn period_with_no_pools_is_a_legal_empty_result() {
        let request = PeriodVarianceAllocRequest {
            period_code: "2026-11".to_string(),
            pools: Vec::new(),
        };
        let outcome = allocate_period(&VarianceAllocRule::by_default(), &request).expect("空期间");
        assert_eq!(outcome.outcomes.len(), 0);
        assert_eq!(outcome.line_count(), 0);
        assert!(outcome.closes_exactly());
        assert_eq!(outcome.total_amount(), Decimal::ZERO);
    }

    // ---------- 护栏 ----------

    #[test]
    fn no_objects_with_money_is_409_with_a_fix_hint() {
        let err = allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("600", "0"), &[]).unwrap_err();
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_COST_ALLOC_NO_OBJECT);
        assert!(err.msg.contains("600"), "{}", err.msg);
        assert!(err.msg.contains("2026-11"), "{}", err.msg);
        assert!(err.msg.contains("材料价差"), "{}", err.msg);
        assert!(err.msg.contains("领料出库数量"), "报文要说清去哪儿补数：{}", err.msg);
    }

    #[test]
    fn all_zero_basis_is_409_not_a_silent_zero_allocation() {
        let objects = vec![object_with_id(61, "CB-1", "0"), object_with_id(62, "CB-2", "0")];
        let err = allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("600", "0"), &objects).unwrap_err();
        assert_eq!(err.status, 409);
        assert_eq!(err.code, error_code::ERP_COST_ALLOC_BASIS_ZERO);
        assert!(err.msg.contains("基数合计为 0"), "{}", err.msg);
        assert!(err.msg.contains("600"), "{}", err.msg);
    }

    #[test]
    fn negative_basis_is_rejected() {
        let objects = vec![object_with_id(71, "CB-1", "-5")];
        let err = allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("600", "0"), &objects).unwrap_err();
        assert_eq!(err.status, 400);
        assert_eq!(err.code, error_code::PARAM_INVALID);
        assert!(err.msg.contains("CB-1"), "{}", err.msg);
    }

    #[test]
    fn duplicate_objects_are_rejected() {
        let objects = vec![object_with_id(81, "CB-1", "100"), object_with_id(81, "CB-1", "100")];
        let err = allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("600", "0"), &objects).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("两次"), "{}", err.msg);
    }

    #[test]
    fn unknown_object_status_is_rejected() {
        let mut objects = three_equal();
        objects[1].cost_object_status = "CANCELLED".to_string();
        let err = allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("600", "0"), &objects).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("CANCELLED"), "{}", err.msg);
    }

    #[test]
    fn rule_code_is_required() {
        let rule = VarianceAllocRule {
            rule_id: None,
            rule_code: "   ".to_string(),
            rule_name: "无编码".to_string(),
            basis: None,
        };
        let err = allocate_variance(&rule, &mat_pool("600", "0"), &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("规则编码"), "{}", err.msg);
    }

    #[test]
    fn unknown_rule_basis_is_rejected() {
        let rule = VarianceAllocRule::with_basis("VAR-02", "拼错基数", "MATERIAL_COST");
        let err = allocate_variance(&rule, &mat_pool("600", "0"), &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("MATERIAL_COST"), "{}", err.msg);
        assert!(err.msg.contains(BASIS_ACTUAL_QTY), "{}", err.msg);
    }

    #[test]
    fn element_must_match_the_variance_type() {
        let mut pool = mat_pool("600", "0");
        pool.element = ELEMENT_LABOR.to_string();
        let err = allocate_variance(&VarianceAllocRule::by_default(), &pool, &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("材料价差"), "{}", err.msg);
        assert!(err.msg.contains(ELEMENT_MATERIAL), "{}", err.msg);
    }

    #[test]
    fn material_pool_without_material_is_rejected() {
        let mut pool = mat_pool("600", "0");
        pool.material_id = None;
        pool.material_code = None;
        let err = allocate_variance(&VarianceAllocRule::by_default(), &pool, &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("必须指定物料"), "{}", err.msg);
    }

    #[test]
    fn labor_pool_with_material_is_rejected() {
        let mut pool = pool(rate_dto::VARIANCE_LABOR_RATE, false, "600", "0");
        pool.material_id = Some(uid(999));
        let err = allocate_variance(&VarianceAllocRule::by_default(), &pool, &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("不能挂物料"), "{}", err.msg);
    }

    #[test]
    fn unknown_variance_type_is_rejected() {
        let mut pool = mat_pool("600", "0");
        pool.variance_type = "MAT_FOO".to_string();
        let err = allocate_variance(&VarianceAllocRule::by_default(), &pool, &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("MAT_FOO"), "{}", err.msg);
        assert!(err.msg.contains("MAT_PRICE"), "{}", err.msg);
    }

    #[test]
    fn period_code_format_is_validated() {
        let mut pool = mat_pool("600", "0");
        pool.period_code = "2026-13".to_string();
        let err = allocate_variance(&VarianceAllocRule::by_default(), &pool, &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("2026-13"), "{}", err.msg);
    }

    #[test]
    fn amount_scale_above_four_is_rejected() {
        let mut pool = mat_pool("600", "0");
        pool.total_amount = dec("600.00005");
        let err = allocate_variance(&VarianceAllocRule::by_default(), &pool, &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("4 位小数"), "{}", err.msg);
    }

    #[test]
    fn amount_beyond_the_numeric_range_is_rejected() {
        let mut pool = mat_pool("600", "0");
        pool.total_amount = dec("10000000000000000");
        let err = allocate_variance(&VarianceAllocRule::by_default(), &pool, &three_equal()).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.msg.contains("NUMERIC(18,4)"), "{}", err.msg);
    }

    #[test]
    fn conservation_assert_fires_on_a_broken_outcome() {
        // 人为造一个丢了一行的结果：断言必须报 500 而不是放过去
        let outcome =
            allocate_variance(&VarianceAllocRule::by_default(), &mat_pool("1000", "0"), &three_equal()).expect("分摊");
        let mut broken = outcome.clone();
        broken.lines.pop();
        let err = ensure_conservation(&broken).unwrap_err();
        assert_eq!(err.status, 500);
        assert_eq!(err.code, error_code::INTERNAL_ERROR);
        assert!(err.msg.contains("守恒断言失败"), "{}", err.msg);

        // 尾差落在两行上同样是 500
        let mut two_adjustments = outcome.clone();
        two_adjustments.lines[0].rounding_adj = dec("0.0001");
        two_adjustments.lines[1].rounding_adj = dec("0.0001");
        let err = ensure_conservation(&two_adjustments).unwrap_err();
        assert_eq!(err.status, 500);
        assert!(err.msg.contains("一行收尾差"), "{}", err.msg);
    }

    #[test]
    fn basis_labels_and_requirements_cover_the_whole_value_range() {
        for basis in VARIANCE_ALLOC_BASES {
            assert_ne!(basis_label(basis), "按未知标准分摊", "{basis} 要有中文名");
            assert!(!basis_requirement(basis).is_empty(), "{basis} 要说清取哪张表");
            assert!(
                basis_requirement(basis) != "该成本对象的基数数据",
                "{basis} 的取数说明不能是兜底文案"
            );
        }
        assert_eq!(basis_label("FOO"), "按未知标准分摊");
    }
}

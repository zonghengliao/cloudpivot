//! 成本还原「半成品成本结构表」的**契约层与纯逻辑**（成本核算 · M5-26）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-restore-structure`（AGENTS 约定 8）。
//!
//! # 这张表回答什么
//!
//! **一个成本对象本期投进去的材料，逐层拆开是什么结构**：哪些是外购材料（到这一层就到底），
//! 哪些是自制半成品（自己也是某个成本对象产出来的，所以还能再往下拆一层）。
//!
//! ```text
//! 本期材料投入  M  = Σ 领料行金额（biz_cost_collect.element = 'MATERIAL'）
//!   ├─ 外购材料 P  = 没有任何成本对象产出过的物料 → 叶子，金额全额进「材料」
//!   └─ 自制半成品 S = 有成本对象产出过的物料（o.product_id = 该物料）
//!        └─ 展开：S 的金额 ÷ 所产批次的完工成本结构 → 本层 料 / 工 / 费
//!             └─ S 自己的材料投入（所产批次的归集）→ 再下一层…
//! ```
//!
//! # 「所产批次的完工成本结构」怎么来的（口径，别自己另算）
//!
//! 取 `biz_cost_period_item`（计算单）里那一行：
//!
//! ```text
//! 完工材料 = round4(完工数量 × unit_material)
//! 完工人工 = round4(完工数量 × unit_labor)
//! 完工制造费用 = 完工转出 − 完工材料 − 完工人工     ← 4 位尾差倒挤进制造费用
//! ```
//!
//! 与 calc 第 6 段（[`cost_period_item_dto::compose`]）的公式逐字一致：那里的
//! `finished_by_formula = completed×unit_material + completed×unit_labor + completed×unit_overhead`，
//! 三项都是 4 位取整；在制为 0 时 `finished_cost` 直接等于流入合计、可能比公式多/少一个尾差，
//! 这个尾差在本表**进制造费用**（与 `beg_cost_of` 把期初还原尾差压给费用的做法同源）。
//! 所以「三要素合计 = 完工转出」在本表是恒等式，不是约等。
//!
//! # 批号对得上 vs 对不上（唯一会影响精度的分叉）
//!
//! 领料明细 `biz_stock_out_item.lot_no` 就是那批半成品的生产批号，`biz_cost_object.lot_no`
//! 是产出批号 —— 两者相等时，本表能精确指到「这批料是哪个成本对象产出来的」，
//! 于是它的结构 = **那一批**的完工成本结构（`source = batch`），并且可以继续往下钻。
//!
//! 对不上（手工归集行没有来源单据、或批号写法不一致）时，退到
//! **同月所产平均结构**：取该物料「本期及以前最近一个有计算单的期间」，
//! 把那个期间所有产出该物料的批次汇总（Σ 完工成本 / Σ 完工数量 及其三要素），
//! 用总结构比例拆本节点金额（`source = period_avg`）。这时**不再下钻**：
//! 拿多批混在一起的子层金额去对一个人的金额，父子之间会加不起来（那是第二本账）。
//!
//! 只认「本期及以前**已产出**」的批次（`period_code <= 本期`）—— 未来期间的计算单
//! 不该解释本期成本；尚未计算的对象不出现在生产者集合里。
//!
//! # 行状态（**不画假 0**）
//!
//! | 状态 | 判据 | 显示 |
//! |---|---|---|
//! | `semi` | 本期材料投入里有自制半成品，且每层结构都取到了 | 逐层结构完整 |
//! | `no_semi` | 本期材料投入全是外购材料 | 一层到底（叶子全进「材料」） |
//! | `no_structure` | 有自制半成品但取不到其完工成本结构（未计算 / 已红冲 / 完工成本为 0） | 该节点三要素给 `null` + `issues` |
//! | `cycle` | 结构出现闭环（A 用 B、B 又用 A） | 截断处 `truncated = true` |
//! | `truncated` | 到 `max_depth` 还没钻完（且下钻有意义） | 截断处 `truncated = true`，调大 `max_depth` 可继续 |
//!
//! 一行同时命中多档时按 `cycle > no_structure > truncated > semi > no_semi` 取一档
//! （最需要人先看的那档），其余事实都在 `issues` / `warnings` 里，不丢。
//!
//! # 逐层视图不是「加总关系」—— 别把各层金额加起来当总额
//!
//! `layers[]` 是**每一层的结构快照**：第 1 层合计 = 本期材料投入（这一层是加总关系），
//! 第 2 层合计 = 各半成品**自身**的材料投入合计（不是父层金额的一部分）。
//! 真正的「还原到料工费」是把各层按比例卷积下去（**M5-27** 的事），本卡只出结构。
//!
//! # 只读
//!
//! 零写库、零迁移、零新增权限码（复用 `business:erp:cost-object:list`，与
//! `cost-wip-equiv` / `cost-wip-alloc` / `cost-wip-balance` 同码：看得到成本对象的人
//! 就是该看这张表的人），也没有「事务提交后失败留死单」的面（AGENTS 约定 11）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use cloudpivot_kernel::shared::error_code;

pub use crate::business::erp::cost::cost_wip_equiv_dto::{BatchView, IssueView, OptionView};

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_LIST: &str = "business:erp:cost-object:list";

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 默认值 / 上限 ====================

/// 默认下钻层数（成品 → 半成品 → 二级半成品 …；6 层覆盖绝大多数离散制造）。
pub const MAX_DEPTH_DEFAULT: u32 = 6;

/// 层数上限（再深就别用递归展开了：要么主数据错了，要么该用标准成本）。
pub const MAX_DEPTH_LIMIT: u32 = 12;

/// 单次请求的节点数上限（超了报 400 让人收窄条件，而不是把内存吃光）。
pub const NODE_GUARD: usize = 20000;

/// 金额精度（与库列 `NUMERIC(18,4)` 一致）。
pub const MONEY_SCALE: u32 = 4;

/// 分配率 / 单位成本精度（与 `NUMERIC(18,6)` 一致）。
pub const RATE_SCALE: u32 = 6;

/// 百分比精度（2 位，与 `NUMERIC(5,2)` 一致）。
pub const PCT_SCALE: u32 = 2;

// ==================== 公式文案 ====================

/// 逐层结构的口径。
pub const FORMULA_LAYER: &str = "逐层结构 = 本期材料投入按「外购材料 / 自制半成品」分；\
                                 自制半成品按其产出批次的完工成本结构展开下一层，直到外购材料为止";

/// 三要素拆分的口径。
pub const FORMULA_ELEMENT: &str = "节点三要素 = 节点金额 × 所产批次完工成本的料/工/费结构比例\
                                   （制造费用吸收 4 位尾差，三要素合计恒等于节点金额）；外购材料全额进材料";

// ==================== 三要素 ====================

/// 料 / 工 / 费三要素金额（4 位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ElementSplit {
    /// 材料。
    pub material: Decimal,
    /// 人工。
    pub labor: Decimal,
    /// 制造费用（**尾差吸收方**）。
    pub overhead: Decimal,
}

impl ElementSplit {
    /// 全 0。
    pub const ZERO: ElementSplit = ElementSplit {
        material: Decimal::ZERO,
        labor: Decimal::ZERO,
        overhead: Decimal::ZERO,
    };

    /// 只有材料（外购材料叶子用）。
    pub fn material_only(amount: Decimal) -> Self {
        Self {
            material: money(amount),
            labor: Decimal::ZERO,
            overhead: Decimal::ZERO,
        }
    }

    /// 合计。
    pub fn total(&self) -> Decimal {
        self.material + self.labor + self.overhead
    }

    /// 累加（合计用）。
    pub fn add(&mut self, other: &ElementSplit) {
        self.material += other.material;
        self.labor += other.labor;
        self.overhead += other.overhead;
    }

    /// 是否全 0。
    pub fn is_zero(&self) -> bool {
        self.material.is_zero() && self.labor.is_zero() && self.overhead.is_zero()
    }

    /// 出参。
    pub fn view(&self) -> ElementView {
        ElementView {
            material: money_text(self.material),
            labor: money_text(self.labor),
            overhead: money_text(self.overhead),
            total: money_text(self.total()),
        }
    }
}

/// 三要素（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ElementView {
    /// 材料。
    pub material: String,
    /// 人工。
    pub labor: String,
    /// 制造费用。
    pub overhead: String,
    /// 合计（恒等于节点金额 / 本层金额）。
    pub total: String,
}

// ==================== 计算单事实 ====================

/// 一个成本对象某一期的计算单事实（产成品与半成品共用同一形状）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemFacts {
    /// 计算单状态（`CALC` / `SETTLED`）。
    pub status: String,
    /// 本期完工数量。
    pub completed_qty: Decimal,
    /// 完工转出成本（4 位，账上的数）。
    pub finished_cost: Decimal,
    /// 完工单位成本（6 位）。
    pub unit_finished_cost: Decimal,
    /// 单位材料成本（6 位）。
    pub unit_material: Decimal,
    /// 单位人工成本（6 位）。
    pub unit_labor: Decimal,
    /// 单位制造费用（6 位）。
    pub unit_overhead: Decimal,
}

/// 是否「有可用的成本结构」（完工数量与完工成本都 > 0）。
pub fn structure_available(item: &ItemFacts) -> bool {
    item.completed_qty > Decimal::ZERO && item.finished_cost > Decimal::ZERO
}

/// 一批产品的完工成本结构（**纯函数**：口径见模块头）。
///
/// 完工数量或完工成本 ≤ 0 ⇒ `None`（结构无从谈起：不拿 0 冒充「全是材料」）。
pub fn structure_of(item: &ItemFacts) -> Option<ElementSplit> {
    if !structure_available(item) {
        return None;
    }
    let material = money(item.completed_qty * item.unit_material);
    let labor = money(item.completed_qty * item.unit_labor);
    clamp_tail(item.finished_cost, material, labor)
}

/// 把「三要素 + 尾差」收成合法结构：制造费用吸收尾差，扣穿了就依次从人工、材料里拨。
///
/// 返回 `None` 只出现在「完工成本比材料 + 人工还小且拨不回来」的病态数据上
/// （正常数据里三项都是同一个公式算出来的，最多差几个 0.0001）。
fn clamp_tail(total: Decimal, material: Decimal, labor: Decimal) -> Option<ElementSplit> {
    if total <= Decimal::ZERO {
        return None;
    }
    let mut material = money(material.max(Decimal::ZERO));
    let mut labor = money(labor.max(Decimal::ZERO));
    let mut overhead = money(total - material - labor);
    if overhead < Decimal::ZERO {
        let mut excess = -overhead;
        overhead = Decimal::ZERO;
        let from_labor = labor.min(excess);
        labor -= from_labor;
        excess -= from_labor;
        if excess > Decimal::ZERO {
            if material < excess {
                return None;
            }
            material -= excess;
        }
    }
    Some(ElementSplit {
        material,
        labor,
        overhead,
    })
}

/// 按结构比例把一个金额拆成三要素（**纯函数**：尾差进制造费用）。
///
/// - 结构合计 ≤ 0 ⇒ `None`（没有比例可依，不给假的「全额材料」）；
/// - 返回值的三要素合计 **恒等于** `amount`（4 位）。
pub fn scale_split(amount: Decimal, structure: &ElementSplit) -> Option<ElementSplit> {
    if amount < Decimal::ZERO {
        return None;
    }
    let total = structure.total();
    if total <= Decimal::ZERO {
        return None;
    }
    let material = money(amount * structure.material / total);
    let labor = money(amount * structure.labor / total);
    clamp_tail(amount, material, labor)
}

/// 占比（`part / whole × 100`，2 位；`whole = 0` ⇒ `None`，不画 0%）。
pub fn share_pct(part: Decimal, whole: Decimal) -> Option<Decimal> {
    if whole <= Decimal::ZERO {
        return None;
    }
    Some((part * Decimal::ONE_HUNDRED / whole).round_dp(PCT_SCALE))
}

// ==================== 生产者（半成品的来源批次） ====================

/// 一个「产出某物料」的成本对象某期计算单（生产者候选）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerFacts {
    /// 产出品物料 id。
    pub product_id: String,
    /// 产出批号（`biz_cost_object.lot_no`，可能为空）。
    pub lot_no: Option<String>,
    /// 成本对象 id。
    pub object_id: String,
    /// 成本对象号。
    pub object_no: String,
    /// 批次期间（`yyyy-MM`）。
    pub period_code: String,
    /// 计算单事实。
    pub item: ItemFacts,
}

/// 结构来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProduceSource {
    /// 批号对得上：用**这一批**的完工成本结构，可继续下钻。
    Batch,
    /// 批号对不上：用同月所产**平均**结构，只拆一层不下钻。
    PeriodAvg,
}

impl ProduceSource {
    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::PeriodAvg => "period_avg",
        }
    }

    /// 中文说明。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Batch => "按批",
            Self::PeriodAvg => "同月平均",
        }
    }
}

/// 解析结果（一个节点从哪批/哪几批拿结构）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProducerChoice {
    /// 来源（`batch` / `period_avg`）。
    pub source: ProduceSource,
    /// 结构所属期间（`batch` = 该批期间；`period_avg` = 同月最近一个有计算单的期间）。
    pub period_code: String,
    /// 参与的对象 id（`batch` 一个；`period_avg` 多个）。
    pub object_ids: Vec<String>,
    /// 参与的对象号（同上）。
    pub object_nos: Vec<String>,
    /// 批号（`period_avg` 且多批不一致 ⇒ `None`）。
    pub lot_no: Option<String>,
    /// 参与批次数。
    pub batch_count: u64,
    /// 参与批次的完工数量合计。
    pub completed_qty: Decimal,
    /// 参与批次的完工成本合计。
    pub finished_cost: Decimal,
    /// 结构（料 / 工 / 费绝对额，合计 = `finished_cost`）。
    pub structure: ElementSplit,
    /// 计算单状态（`batch` = 该批；`period_avg` = 最新那批的状态）。
    pub item_status: String,
}

/// 从候选里解析「这个物料这批料的成本结构该用谁」（**纯函数**）。
///
/// * 批号给了且对得上 ⇒ [`ProduceSource::Batch`]：取该批**最近期间**的那张计算单
///   （同批号跨期重算过的情况：以最近一期为准），同期间多张时 `SETTLED` 优先；
/// * 批号没给 / 对不上 ⇒ [`ProduceSource::PeriodAvg`]：取候选里**最近期间**的全部批次汇总，
///   结构用合计比例（Σ 完工成本 / Σ 完工数量），`finished_cost` 合计为 0 ⇒ `None`；
/// * 候选为空 ⇒ `None`（该物料没有任何成本对象产出过 ⇒ 当作外购材料，叶子）。
pub fn resolve_producer(producers: &[ProducerFacts], lot_no: Option<&str>) -> Option<ProducerChoice> {
    let lot = lot_no.map(str::trim).filter(|text| !text.is_empty());

    // tip: lot 存在且能匹配到批次 → 用批次口径；否则落到"最新重算期间"。
    // 用 and_then 把两层 if let 压成一层（clippy::collapsible_if），
    // 不依赖 let-chains（edition 2021 不支持）。
    if let Some(hit) = lot.and_then(|l| pick_batch(producers, l)) {
        let structure = structure_of(&hit.item)?;
        return Some(ProducerChoice {
            source: ProduceSource::Batch,
            period_code: hit.period_code.clone(),
            object_ids: vec![hit.object_id.clone()],
            object_nos: vec![hit.object_no.clone()],
            lot_no: hit.lot_no.clone(),
            batch_count: 1,
            completed_qty: hit.item.completed_qty,
            finished_cost: hit.item.finished_cost,
            structure,
            item_status: hit.item.status.clone(),
        });
    }

    let latest = producers
        .iter()
        .map(|item| item.period_code.as_str())
        .max()?
        .to_string();
    let group: Vec<&ProducerFacts> = producers.iter().filter(|item| item.period_code == latest).collect();
    let mut completed_qty = Decimal::ZERO;
    let mut finished_cost = Decimal::ZERO;
    let mut structure = ElementSplit::ZERO;
    let mut object_ids = Vec::with_capacity(group.len());
    let mut object_nos = Vec::with_capacity(group.len());
    let mut lots: Vec<Option<String>> = Vec::with_capacity(group.len());
    for item in &group {
        // 没有结构的批次（完工成本 0）不参与平均：掺进去只会把比例拉偏
        let split = structure_of(&item.item)?;
        completed_qty += item.item.completed_qty;
        finished_cost += item.item.finished_cost;
        structure.add(&split);
        object_ids.push(item.object_id.clone());
        object_nos.push(item.object_no.clone());
        lots.push(item.lot_no.clone());
    }
    if finished_cost <= Decimal::ZERO {
        return None;
    }
    let mut unique_lots = lots.into_iter().flatten().collect::<Vec<_>>();
    unique_lots.sort();
    unique_lots.dedup();
    let lot_no = if unique_lots.len() == 1 {
        unique_lots.pop()
    } else {
        None
    };

    Some(ProducerChoice {
        source: ProduceSource::PeriodAvg,
        period_code: latest,
        object_ids,
        object_nos,
        lot_no,
        batch_count: u64::try_from(group.len()).unwrap_or(0),
        completed_qty,
        finished_cost,
        structure,
        item_status: group
            .iter()
            .find(|item| item.item.status == "SETTLED")
            .map(|item| item.item.status.clone())
            .unwrap_or_else(|| group[0].item.status.clone()),
    })
}

/// 批号精确匹配：期间最近者优先，同期多批 `SETTLED` 优先，再按对象号定序（结果可复现）。
fn pick_batch<'a>(producers: &'a [ProducerFacts], lot: &str) -> Option<&'a ProducerFacts> {
    let mut hits: Vec<&ProducerFacts> = producers
        .iter()
        .filter(|item| item.lot_no.as_deref().map(str::trim) == Some(lot))
        .collect();
    hits.sort_by(|left, right| {
        right
            .period_code
            .cmp(&left.period_code)
            .then_with(|| (right.item.status == "SETTLED").cmp(&(left.item.status == "SETTLED")))
            .then_with(|| left.object_no.cmp(&right.object_no))
    });
    hits.into_iter().next()
}

// ==================== 问题 ====================

/// 一条结构取不到 / 被截断的原因（`code` 稳定，前端可判；`message` 说事实；`fix` 说怎么修）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreIssue {
    /// 稳定码。
    pub code: &'static str,
    /// 事实。
    pub message: String,
    /// 怎么修。
    pub fix: String,
}

/// 半成品取不到成本结构。
pub const CODE_NO_STRUCTURE: &str = "NO_STRUCTURE";
/// 结构出现闭环。
pub const CODE_CYCLE: &str = "CYCLE";
/// 到层数上限。
pub const CODE_DEPTH_CUT: &str = "DEPTH_CUT";
/// 结构比例病态（加不齐 / 负数）。
pub const CODE_STRUCTURE_INVALID: &str = "STRUCTURE_INVALID";
/// 节点数到上限。
pub const CODE_NODE_GUARD: &str = "NODE_GUARD";
/// 料行没有物料（库层已拦，这里是兜底）。
pub const CODE_MATERIAL_MISSING: &str = "MATERIAL_MISSING";

impl RestoreIssue {
    /// 出参。
    pub fn view(&self) -> IssueView {
        IssueView {
            code: self.code.to_string(),
            message: self.message.clone(),
            fix: self.fix.clone(),
        }
    }
}

// ==================== 行状态 ====================

/// 一行的形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// 有自制半成品，结构完整。
    Semi,
    /// 全是外购材料（一层到底）。
    NoSemi,
    /// 有半成品但取不到结构。
    NoStructure,
    /// 出现闭环。
    Cycle,
    /// 到层数上限还没钻完。
    Truncated,
}

impl RowState {
    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Semi => "semi",
            Self::NoSemi => "no_semi",
            Self::NoStructure => "no_structure",
            Self::Cycle => "cycle",
            Self::Truncated => "truncated",
        }
    }

    /// 中文名（2–4 字，列标直接用）。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Semi => "逐层可还原",
            Self::NoSemi => "全外购料",
            Self::NoStructure => "缺结构",
            Self::Cycle => "结构闭环",
            Self::Truncated => "未钻到底",
        }
    }

    /// 优先级（一行多档命中时取最高的一档：最需要人先看的那档在前面）。
    pub fn rank(&self) -> u8 {
        match self {
            Self::Cycle => 4,
            Self::NoStructure => 3,
            Self::Truncated => 2,
            Self::Semi => 1,
            Self::NoSemi => 0,
        }
    }
}

/// 按状态筛行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    /// 全部（默认）。
    All,
    /// 有自制半成品且结构完整的行。
    Semi,
    /// 全是外购材料。
    NoSemi,
    /// 需要人处理的（缺结构 / 闭环 / 未钻到底）。
    Blocked,
}

/// 状态筛选项（顺序即下拉顺序）。
pub const STATE_FILTERS: [StateFilter; 4] = [
    StateFilter::All,
    StateFilter::Semi,
    StateFilter::NoSemi,
    StateFilter::Blocked,
];

impl StateFilter {
    /// 解析（拼错 400，不静默回落到 `all`：静默回落会让人以为「筛过了」）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        let text = raw.map(str::trim).filter(|text| !text.is_empty()).unwrap_or("all");
        match text {
            "all" => Ok(Self::All),
            "semi" => Ok(Self::Semi),
            "no_semi" => Ok(Self::NoSemi),
            "blocked" => Ok(Self::Blocked),
            other => Err(bad_request(&format!(
                "state 只支持 all / semi / no_semi / blocked（收到 `{other}`）：\
                 拼错时静默回落到 all 会让人以为筛过了"
            ))),
        }
    }

    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Semi => "semi",
            Self::NoSemi => "no_semi",
            Self::Blocked => "blocked",
        }
    }

    /// 中文名。
    pub fn label(&self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Semi => "可还原",
            Self::NoSemi => "全外购料",
            Self::Blocked => "要处理",
        }
    }

    /// 命中判据。
    pub fn matches(&self, state: RowState) -> bool {
        match self {
            Self::All => true,
            Self::Semi => state == RowState::Semi,
            Self::NoSemi => state == RowState::NoSemi,
            Self::Blocked => matches!(state, RowState::NoStructure | RowState::Cycle | RowState::Truncated),
        }
    }
}

/// 一行多档命中时的取档（**纯函数**）。
pub fn row_state(semi_cost: Decimal, has_no_structure: bool, has_cycle: bool, has_truncated: bool) -> RowState {
    if has_cycle {
        RowState::Cycle
    } else if has_no_structure {
        RowState::NoStructure
    } else if has_truncated {
        RowState::Truncated
    } else if semi_cost > Decimal::ZERO {
        RowState::Semi
    } else {
        RowState::NoSemi
    }
}

// ==================== 出参 ====================

/// 一个结构节点（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeView {
    /// 层级（1 起）。
    pub level: u32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码（主数据已删 ⇒ `null`）。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 领料批号（同批号汇总成一行；没有批号 ⇒ `null`）。
    pub lot_no: Option<String>,
    /// 数量（4 位）。
    pub qty: String,
    /// 领料单价（6 位；金额 / 数量算出来的参考值，`qty = 0` ⇒ `null`）。
    pub unit_cost: Option<String>,
    /// 金额（4 位，领料行金额合计）。
    pub amount: String,
    /// 占**同级**金额的百分比（2 位；同级合计为 0 ⇒ `null`）。
    pub share_pct: Option<String>,
    /// 是否自制半成品（= 有成本对象产出过该物料）。
    pub is_semi: bool,
    /// 结构来源（`batch` / `period_avg`；外购材料 ⇒ `null`）。
    pub source: Option<String>,
    /// 来源中文名。
    pub source_label: Option<String>,
    /// 生产批次信息（外购材料 ⇒ `null`）。
    pub producer: Option<ProducerView>,
    /// 本节点金额的三要素（自制半成品取不到结构 ⇒ `null`，**不画 0**）。
    pub elements: Option<ElementView>,
    /// 子层（外购材料 / `period_avg` 节点为空）。
    pub children: Vec<NodeView>,
    /// 是否被截断（闭环 / 层数上限 / 节点数上限）。
    pub truncated: bool,
    /// 取不到 / 被截断的原因。
    pub issues: Vec<IssueView>,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
}

/// 半成品的产出批次（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProducerView {
    /// 成本对象 id（`period_avg` 且多批 ⇒ `null`，看 `object_nos`）。
    pub object_id: Option<String>,
    /// 成本对象号（多批 ⇒ 用 `/` 连起来，前端直接显示）。
    pub object_nos: String,
    /// 产出批号（多批不一致 ⇒ `null`）。
    pub lot_no: Option<String>,
    /// 结构所属期间。
    pub period_code: String,
    /// 计算单状态（`CALC` / `SETTLED`）。
    pub item_status: String,
    /// 参与批次数。
    pub batch_count: u64,
    /// 完工数量合计（4 位）。
    pub completed_qty: String,
    /// 完工成本合计（4 位）。
    pub finished_cost: String,
    /// 完工单位成本（6 位；完工数量 0 ⇒ `null`）。
    pub unit_finished_cost: Option<String>,
    /// 完工成本的三要素（绝对额）。
    pub elements: ElementView,
}

/// 一层的汇总（出参）。**各层不是加总关系**，见模块头。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LayerView {
    /// 层级（1 起）。
    pub level: u32,
    /// 本层节点数。
    pub node_count: u64,
    /// 本层里自制半成品的节点数。
    pub semi_node_count: u64,
    /// 本层金额合计。
    pub amount: String,
    /// 本层三要素合计（**只累加取得到结构的节点**）。
    pub elements: ElementView,
    /// 三要素取不到的节点数（它们的金额仍进 `amount`，但不进 `elements`）。
    pub incomplete_count: u64,
    /// 本层口径说明。
    pub note: String,
}

/// 一行（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Row {
    /// 成本对象 id。
    pub object_id: String,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 产出批号。
    pub lot_no: Option<String>,
    /// 产出品 id。
    pub product_id: String,
    /// 产出品编码。
    pub product_code: Option<String>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 车间 id。
    pub workshop_dept_id: String,
    /// 车间名称。
    pub workshop_dept_name: Option<String>,
    /// 本期计算单（没批次 / 没算过 ⇒ `null`）。
    pub item: Option<ItemView>,
    /// 本期完工成本（`item` 为空 ⇒ `null`）。
    pub finished_cost: Option<String>,
    /// 本期材料投入（4 位）。
    pub material_cost: String,
    /// 其中自制半成品金额。
    pub semi_cost: String,
    /// 其中外购材料金额。
    pub purchased_cost: String,
    /// 半成品占材料投入的百分比（材料投入 0 ⇒ `null`）。
    pub semi_share: Option<String>,
    /// 行状态（`semi` / `no_semi` / `no_structure` / `cycle` / `truncated`）。
    pub state: String,
    /// 行状态中文名。
    pub state_label: String,
    /// 实际展开层数（最深一层）。
    pub depth: u32,
    /// 节点数。
    pub node_count: u64,
    /// 逐层汇总。
    pub layers: Vec<LayerView>,
    /// 逐层结构树。
    pub tree: Vec<NodeView>,
    /// 取不到 / 被截断的原因（行级，去重后）。
    pub issues: Vec<IssueView>,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
}

/// 本期计算单（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ItemView {
    /// 状态（`CALC` / `SETTLED`）。
    pub status: String,
    /// 完工数量。
    pub completed_qty: String,
    /// 完工转出成本。
    pub finished_cost: String,
    /// 完工单位成本（6 位）。
    pub unit_finished_cost: String,
    /// 本期材料投入（计算单口径）。
    pub mat_cur: String,
    /// 本期人工投入。
    pub labor_cur: String,
    /// 本期制造费用投入。
    pub oh_cur: String,
}

/// 整表合计（内部累加）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    /// 行数。
    pub row_count: u64,
    /// 有半成品且结构完整的行数。
    pub semi_row_count: u64,
    /// 全外购料的行数。
    pub no_semi_row_count: u64,
    /// 缺结构的行数。
    pub no_structure_row_count: u64,
    /// 闭环的行数。
    pub cycle_row_count: u64,
    /// 未钻到底的行数。
    pub truncated_row_count: u64,
    /// 本期材料投入合计。
    pub material_cost: Decimal,
    /// 其中自制半成品合计。
    pub semi_cost: Decimal,
    /// 其中外购材料合计。
    pub purchased_cost: Decimal,
    /// 本期完工成本合计（只累加有计算单的行）。
    pub finished_cost: Decimal,
    /// 有计算单的行数。
    pub finished_row_count: u64,
    /// 最深层数。
    pub max_depth: u32,
    /// 节点数合计。
    pub node_count: u64,
}

impl Totals {
    /// 累加一行。
    pub fn add(&mut self, row: &Row) {
        self.row_count += 1;
        match row.state.as_str() {
            "semi" => self.semi_row_count += 1,
            "no_semi" => self.no_semi_row_count += 1,
            "no_structure" => self.no_structure_row_count += 1,
            "cycle" => self.cycle_row_count += 1,
            "truncated" => self.truncated_row_count += 1,
            _ => {}
        }
        self.material_cost += dec(&row.material_cost);
        self.semi_cost += dec(&row.semi_cost);
        self.purchased_cost += dec(&row.purchased_cost);
        if let Some(finished) = row.finished_cost.as_deref() {
            self.finished_cost += dec(finished);
            self.finished_row_count += 1;
        }
        self.max_depth = self.max_depth.max(row.depth);
        self.node_count += row.node_count;
    }
}

/// 合计（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TotalsView {
    /// 行数。
    pub row_count: u64,
    /// 可还原行数。
    pub semi_row_count: u64,
    /// 全外购料行数。
    pub no_semi_row_count: u64,
    /// 缺结构行数。
    pub no_structure_row_count: u64,
    /// 闭环行数。
    pub cycle_row_count: u64,
    /// 未钻到底行数。
    pub truncated_row_count: u64,
    /// 需要人处理的行数（缺结构 + 闭环 + 未钻到底）。
    pub blocked_row_count: u64,
    /// 本期材料投入合计。
    pub material_cost: String,
    /// 其中自制半成品合计。
    pub semi_cost: String,
    /// 其中外购材料合计。
    pub purchased_cost: String,
    /// 半成品占材料投入的百分比（2 位；材料投入 0 ⇒ `null`）。
    pub semi_share: Option<String>,
    /// 本期完工成本合计。
    pub finished_cost: String,
    /// 有计算单的行数。
    pub finished_row_count: u64,
    /// 最深层数。
    pub max_depth: u32,
    /// 节点数合计。
    pub node_count: u64,
    /// 合计口径说明。
    pub note: String,
}

/// 合计 → 出参。
pub fn totals_view(totals: &Totals) -> TotalsView {
    let blocked_row_count = totals.no_structure_row_count + totals.cycle_row_count + totals.truncated_row_count;
    TotalsView {
        row_count: totals.row_count,
        semi_row_count: totals.semi_row_count,
        no_semi_row_count: totals.no_semi_row_count,
        no_structure_row_count: totals.no_structure_row_count,
        cycle_row_count: totals.cycle_row_count,
        truncated_row_count: totals.truncated_row_count,
        blocked_row_count,
        material_cost: money_text(totals.material_cost),
        semi_cost: money_text(totals.semi_cost),
        purchased_cost: money_text(totals.purchased_cost),
        semi_share: share_pct(totals.semi_cost, totals.material_cost).map(pct_text),
        finished_cost: money_text(totals.finished_cost),
        finished_row_count: totals.finished_row_count,
        max_depth: totals.max_depth,
        node_count: totals.node_count,
        note:
            "合计是整张表的（不受分页影响）；材料投入 = 本期领料归集合计，其中「自制半成品」= 有成本对象产出过的物料。\
               半成品金额只累加第 1 层（它才等于材料投入）；各层之间不是加总关系 —— 还原到料工费是 M5-27 的事"
                .to_string(),
    }
}

/// 入参。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RestoreQuery {
    /// 期间（`yyyy-MM`，必填）。
    pub period_code: Option<String>,
    /// 关键字（对象号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称）。
    pub keyword: Option<String>,
    /// 状态筛选（`all` / `semi` / `no_semi` / `blocked`）。
    pub state: Option<String>,
    /// 页码（1 起）。
    pub page: Option<u64>,
    /// 每页行数（默认 20，上限 200）。
    pub page_size: Option<u64>,
    /// 下钻层数（默认 6，上限 12）。
    pub max_depth: Option<u32>,
}

/// 归一化后的入参。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreFilter {
    /// 期间。
    pub period_code: String,
    /// 关键字（已 trim）。
    pub keyword: Option<String>,
    /// 状态筛选。
    pub state: StateFilter,
    /// 页码。
    pub page: u64,
    /// 每页行数。
    pub page_size: u64,
    /// 下钻层数。
    pub max_depth: u32,
}

/// 整张表的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoreRes {
    /// 期间。
    pub period_code: String,
    /// 当期成本核算批次（没建 ⇒ `null`，行里的计算单可能还没算）。
    pub batch: Option<BatchView>,
    /// 是否已建批次。
    pub cost_ready: bool,
    /// 状态筛选回显。
    pub state: String,
    /// 关键字回显。
    pub keyword: Option<String>,
    /// 下钻层数回显。
    pub max_depth: u32,
    /// 逐层结构口径。
    pub formula_layer: String,
    /// 三要素口径。
    pub formula_element: String,
    /// 合计。
    pub totals: TotalsView,
    /// 行。
    pub list: Vec<Row>,
    /// 筛选后的行数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页行数。
    pub page_size: u64,
    /// 状态选项。
    pub state_options: Vec<OptionView>,
    /// 页脚说明。
    pub note: String,
}

/// 筛选档 → 出参。
pub fn state_options() -> Vec<OptionView> {
    STATE_FILTERS
        .iter()
        .map(|item| OptionView {
            value: item.as_str().to_string(),
            label: item.label().to_string(),
        })
        .collect()
}

// ==================== 逐层汇总（纯函数） ====================

/// 从结构树算逐层汇总（**纯函数**，不连库）。
///
/// 第 1 层的 `amount` 合计 = 本行材料投入（这一层是加总关系）；
/// 第 2 层起是「各节点自身的材料投入」，**不是**父层金额的一部分（见模块头）。
pub fn layer_views(tree: &[NodeView]) -> Vec<LayerView> {
    let mut buckets: Vec<LayerDraft> = Vec::new();
    collect_layers(tree, &mut buckets);
    buckets
        .into_iter()
        .map(|bucket| LayerView {
            level: bucket.level,
            node_count: bucket.node_count,
            semi_node_count: bucket.semi_node_count,
            amount: money_text(bucket.amount),
            elements: bucket.elements.view(),
            incomplete_count: bucket.incomplete_count,
            note: if bucket.level == 1 {
                "第 1 层 = 本期材料投入的全部去向（外购材料 + 自制半成品），本层金额合计等于材料投入".to_string()
            } else {
                format!(
                    "第 {} 层 = 各半成品自身消耗的材料（是「它们」的投入，不是父层金额的一部分）；\
                     三要素只累加取得到结构的 {} 个节点",
                    bucket.level,
                    bucket.node_count.saturating_sub(bucket.incomplete_count)
                )
            },
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
struct LayerDraft {
    level: u32,
    node_count: u64,
    semi_node_count: u64,
    amount: Decimal,
    elements: ElementSplit,
    incomplete_count: u64,
}

fn collect_layers(nodes: &[NodeView], buckets: &mut Vec<LayerDraft>) {
    for node in nodes {
        let index = usize::try_from(node.level.saturating_sub(1)).unwrap_or(0);
        while buckets.len() <= index {
            buckets.push(LayerDraft {
                level: u32::try_from(buckets.len()).unwrap_or(0) + 1,
                ..LayerDraft::default()
            });
        }
        let bucket = &mut buckets[index];
        bucket.node_count += 1;
        if node.is_semi {
            bucket.semi_node_count += 1;
        }
        bucket.amount += dec(&node.amount);
        match node.elements.as_ref() {
            Some(elements) => {
                bucket.elements.material += dec(&elements.material);
                bucket.elements.labor += dec(&elements.labor);
                bucket.elements.overhead += dec(&elements.overhead);
            }
            None => bucket.incomplete_count += 1,
        }
        collect_layers(&node.children, buckets);
    }
}

/// 结构树节点数（纯函数）。
pub fn count_nodes(nodes: &[NodeView]) -> u64 {
    nodes.iter().map(|node| 1 + count_nodes(&node.children)).sum::<u64>()
}

/// 结构树最深一层（纯函数；空树 ⇒ 0）。
pub fn max_level(nodes: &[NodeView]) -> u32 {
    nodes
        .iter()
        .map(|node| node.level.max(max_level(&node.children)))
        .max()
        .unwrap_or(0)
}

// ==================== 数值 / 文本 ====================

/// 金额取整到 4 位（与库列 `NUMERIC(18,4)` 一致；本模块所有三要素运算都过它）。
fn money(value: Decimal) -> Decimal {
    value.round_dp(MONEY_SCALE)
}

/// 金额 → 文本（4 位，去掉尾随 0：与成本域其它报表同口径）。
pub fn money_text(value: Decimal) -> String {
    value.round_dp(MONEY_SCALE).normalize().to_string()
}

/// 数量 → 文本（固定 4 位：与计算单 `NUMERIC(18,4)` 逐字可比）。
pub fn qty_text(value: Decimal) -> String {
    format!("{:.4}", value.round_dp(MONEY_SCALE))
}

/// 百分比 → 文本（2 位）。
pub fn pct_text(value: Decimal) -> String {
    format!("{:.2}", value.round_dp(PCT_SCALE))
}

/// 单价 → 文本（6 位）。
pub fn rate_text(value: Decimal) -> String {
    format!("{:.6}", value.round_dp(RATE_SCALE))
}

/// 金额 → Decimal（内部合计用）。
fn dec(text: &str) -> Decimal {
    text.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

// ==================== 错误 ====================

/// 400 + `PARAM_INVALID`。
pub fn bad_request(message: &str) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.to_string())
}

/// 500 + `INTERNAL_ERROR`：库里的 id / 数值解析不出来（不是用户输入问题）。
pub fn internal_error(message: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, message.to_string())
}

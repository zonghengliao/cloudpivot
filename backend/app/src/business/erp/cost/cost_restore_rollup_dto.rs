//! 成本还原「逐层还原到料工费」的**契约层与纯逻辑**（成本核算 · M5-27）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-restore-rollup`（AGENTS 约定 8）。
//!
//! # 这张表回答什么
//!
//! **本期投进去的钱，还原成「外购材料 / 人工 / 制造费用」各多少**：
//!
//! ```text
//! 还原前（归集口径）          还原（递归卷积）                还原后（原始成本项目）
//!   材料（含半成品）   ──┐
//!   人工              ──┤  材料里凡是自制半成品的金额，就按   ──→  外购材料
//!   制造费用          ──┘  它产出批次的完工成本结构 料/工/费     ──→ 人工
//!                          拆开；拆出来的「料」如果还是半成品     ──→ 制造费用
//!                          就继续往下拆，直到全是外购料为止       （+ 还原不彻底的部分单列）
//! ```
//!
//! 一句话：**材料里的半成品被逐层「化成」料工费**，总额一分不动
//! （`还原后合计 + 还原不彻底金额 = 还原前合计`，是恒等式，真库用例逐行断言）。
//!
//! # 与 M5-26 的分工（别重算结构）
//!
//! M5-26 出的是「**每一层的结构**」（`GET /business/erp/cost-restore-structure` 的 `tree[]`）：
//! 每个节点带 `amount` 与 `elements`（该节点金额按所产批次完工成本结构拆出的料/工/费，
//! `elements.total` 恒等于 `amount`）。本卡**不改一行取数**，直接消费那棵树做卷积：
//!
//! | 层级 | 流入本层的金额（`imputed`） |
//! |---|---|
//! | 第 1 层 | 节点自己的 `amount`（加总 = 本期材料投入） |
//! | 第 N 层 | 上一层节点的「材料部分」按兄弟 `amount` 比例分下来的份额 |
//!
//! 所以第 2 层起的金额**不是**新的投入，而是上一层那笔钱里「还是半成品形态」的部分 ——
//! 两层都加进总额就是算两遍。
//!
//! # 三条硬口径
//!
//! 1. **比例只从 `elements` 来**（M5-26 已经从 `biz_cost_period_item` 取好），本卡不另算比例；
//! 2. **尾差进制造费用**：每次拆分 `材料 + 人工 + 制造费用 = 流入金额`，制造费用吸收 4 位尾差
//!    （与 `cost_period_item_dto` / `beg_cost_of` / M5-26 的 `clamp_tail` 同源）；
//! 3. **`source = period_avg` / 截断 / 闭环的节点不再下钻**：它的「材料部分」原样停在半成品形态，
//!    计入 [`Restored::unresolved`]（**不画假 0**，也不假装已经还原成外购料）。
//!
//! # 三档行状态（比 M5-26 的五档粗，因为这张表只关心「能不能还原」）
//!
//! | 状态 | 判据 | 含义 |
//! |---|---|---|
//! | `no_semi` | 本期材料里没有自制半成品 | 本来就全是外购料，还原前后一致 |
//! | `restored` | 有半成品且 `unresolved = 0` | 逐层还原干净 |
//! | `partial` | 有半成品且 `unresolved > 0` | 有金额停在半成品形态（缺结构 / 截断 / 闭环 / 没有下一层） |
//!
//! `partial` 的原因不丢：M5-26 行上的 `issues` / `warnings` 原样带过来（`NO_STRUCTURE` / `CYCLE` /
//! `DEPTH_CUT`），再加上本表自己的逐层台账（`flows[]`）指出卡在第几层。
//!
//! # 只读
//!
//! 零写库、零迁移、零新增权限码（复用 `business:erp:cost-object:list`，与结构表 / 约当表 /
//! 分配表 / 余额表同码），也没有「事务提交后失败留死单」的面（AGENTS 约定 11）。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use cloudpivot_kernel::shared::error_code;

pub use crate::business::erp::cost::cost_restore_structure_dto::{
    BatchView, ElementSplit, ElementView, IssueView, NodeView, OptionView,
};

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_LIST: &str = "business:erp:cost-object:list";

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 默认值 / 上限 ====================

/// 默认下钻层数（与结构表一致：6 层覆盖绝大多数离散制造）。
pub const MAX_DEPTH_DEFAULT: u32 = crate::business::erp::cost::cost_restore_structure_dto::MAX_DEPTH_DEFAULT;

/// 层数上限。
pub const MAX_DEPTH_LIMIT: u32 = crate::business::erp::cost::cost_restore_structure_dto::MAX_DEPTH_LIMIT;

/// 金额精度（与库列 `NUMERIC(18,4)` 一致）。
pub const MONEY_SCALE: u32 = 4;

/// 百分比精度（2 位）。
pub const PCT_SCALE: u32 = 2;

// ==================== 公式文案 ====================

/// 卷积口径。
pub const FORMULA_RESTORE: &str = "还原 = 材料里的自制半成品按其所产批次的完工成本结构（料/工/费）递归拆开，\
                                   拆出来的「料」若仍是半成品就继续拆，直到全是外购材料；\
                                   每一步 材料 + 人工 + 制造费用 = 流入金额（尾差进制造费用），总额不变";

/// 还原不彻底的口径。
pub const FORMULA_UNRESOLVED: &str = "还原不彻底 = 停在半成品形态、无法继续拆的金额\
                                      （产出批次没计算 / 已红冲 / 结构闭环 / 到层数上限 / 没有下一层投入）；\
                                      它既不算进外购材料，也不丢掉：还原后合计 + 还原不彻底 = 还原前合计";

// ==================== 数值解析 ====================

/// 文本金额 → `Decimal`（出参是 4 位定标字符串，解析失败按 0：不 panic）。
pub fn parse_money(text: &str) -> Decimal {
    text.trim().parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

/// 节点三要素 → `ElementSplit`（`elements` 为空 ⇒ `None`，**不拿 0 冒充**）。
pub fn parse_elements(view: &ElementView) -> Option<ElementSplit> {
    let split = ElementSplit {
        material: parse_money(&view.material),
        labor: parse_money(&view.labor),
        overhead: parse_money(&view.overhead),
    };
    if split.total() <= Decimal::ZERO {
        return None;
    }
    Some(split)
}

/// 金额取整到 4 位。
fn money(value: Decimal) -> Decimal {
    value.round_dp(MONEY_SCALE)
}

/// 金额 → 文本（4 位，去掉尾随 0：与成本域其它报表同口径）。
pub fn money_text(value: Decimal) -> String {
    crate::business::erp::cost::cost_restore_structure_dto::money_text(value)
}

/// 百分比 → 文本（2 位）。
pub fn pct_text(value: Decimal) -> String {
    crate::business::erp::cost::cost_restore_structure_dto::pct_text(value)
}

/// 占比（`part / whole × 100`，2 位；`whole = 0` ⇒ `None`）。
pub fn share_pct(part: Decimal, whole: Decimal) -> Option<Decimal> {
    crate::business::erp::cost::cost_restore_structure_dto::share_pct(part, whole)
}

// ==================== 卷积结果 ====================

/// 一笔钱还原后的去向（4 位；`material + labor + overhead + unresolved == 流入金额`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Restored {
    /// 确认为外购材料。
    pub material: Decimal,
    /// 确认为人工。
    pub labor: Decimal,
    /// 确认为制造费用（**尾差吸收方**）。
    pub overhead: Decimal,
    /// 还原不彻底（仍是半成品形态；不假装成外购料，也不丢掉）。
    pub unresolved: Decimal,
}

impl Restored {
    /// 四段合计。
    pub fn total(&self) -> Decimal {
        self.material + self.labor + self.overhead + self.unresolved
    }

    /// 累加。
    pub fn add(&mut self, other: &Restored) {
        self.material += other.material;
        self.labor += other.labor;
        self.overhead += other.overhead;
        self.unresolved += other.unresolved;
    }

    /// 已还原成外购料 / 人工 / 制造费用三段（不含 `unresolved`）。
    pub fn confirmed(&self) -> ElementSplit {
        ElementSplit {
            material: self.material,
            labor: self.labor,
            overhead: self.overhead,
        }
    }
}

/// 一层卷积的台账（**每一层**流入多少、各去哪了；层与层之间是「往下拨」的关系，不能相加）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// 层级（1 起）。
    pub level: u32,
    /// 本层被卷积的节点数。
    pub node_count: u64,
    /// 流入本层的金额。
    pub imputed: Decimal,
    /// 本层确认为外购材料。
    pub purchased: Decimal,
    /// 本层确认为人工。
    pub labor: Decimal,
    /// 本层确认为制造费用。
    pub overhead: Decimal,
    /// 本层继续往下卷积的金额（= 本层流入的「材料部分」）。
    pub carried_down: Decimal,
    /// 本层无法继续还原。
    pub unresolved: Decimal,
}

impl Default for Flow {
    fn default() -> Self {
        Self {
            level: 0,
            node_count: 0,
            imputed: Decimal::ZERO,
            purchased: Decimal::ZERO,
            labor: Decimal::ZERO,
            overhead: Decimal::ZERO,
            carried_down: Decimal::ZERO,
            unresolved: Decimal::ZERO,
        }
    }
}

impl Flow {
    /// 守恒自检：流入 = 外购 + 人工 + 制造费用 + 继续下钻 + 不彻底。
    pub fn balanced(&self) -> bool {
        self.imputed == self.purchased + self.labor + self.overhead + self.carried_down + self.unresolved
    }
}

/// 一个被还原掉的半成品节点（递归链上的每一环）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemiDetail {
    /// 层级。
    pub level: u32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 领料批号。
    pub lot_no: Option<String>,
    /// 本节点被卷积的金额（父层拨下来的份额）。
    pub imputed: Decimal,
    /// 结构来源（`batch` / `period_avg`）。
    pub source: Option<String>,
    /// 来源中文名。
    pub source_label: Option<String>,
    /// 产出它的成本对象号（多批用 `/` 连）。
    pub producer_nos: Option<String>,
    /// 还原结果（`material` 只含外购料；`total` = 流入 − 不彻底）。
    pub restored: Restored,
    /// 能不能继续下钻（`false` = 停在这一层）。
    pub stopped: bool,
}

/// 一次还原的完整结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rollup {
    /// 还原前（材料含半成品）。
    pub before: ElementSplit,
    /// 还原后（材料只含外购料）。
    pub after: ElementSplit,
    /// 还原不彻底的金额。
    pub unresolved: Decimal,
    /// 逐层台账。
    pub flows: Vec<Flow>,
    /// 半成品还原明细。
    pub semis: Vec<SemiDetail>,
    /// 有节点被层数上限 / 闭环截断。
    pub truncated: bool,
    /// 有节点取不到结构。
    pub missing: bool,
    /// 本行第 1 层节点金额合计（正常等于 `before.material`；不等说明表被改过）。
    pub covered: Decimal,
}

impl Rollup {
    /// 守恒：还原后合计 + 不彻底 = 还原前合计。
    pub fn balanced(&self) -> bool {
        self.after.total() + self.unresolved == self.before.total()
    }
}

// ==================== 递归卷积（纯函数） ====================

#[derive(Debug, Default)]
struct Ctx {
    flows: Vec<Flow>,
    semis: Vec<SemiDetail>,
    truncated: bool,
    missing: bool,
}

fn slot(ctx: &mut Ctx, level: u32) -> &mut Flow {
    let index = usize::try_from(level.saturating_sub(1)).unwrap_or(0);
    while ctx.flows.len() <= index {
        let next = u32::try_from(ctx.flows.len()).unwrap_or(0) + 1;
        ctx.flows.push(Flow {
            level: next,
            ..Flow::default()
        });
    }
    &mut ctx.flows[index]
}

/// 把一个节点「流入的金额」按结构递归还原（**纯函数**：不连库、不看 DB）。
///
/// * `imputed` 是这一次拨到这个节点上的钱（第 1 层 = 节点金额；更深层 = 父层材料部分分下来的份额）；
/// * 三要素比例来自 `node.elements`（M5-26 已算好，`total` 恒等于 `node.amount`）；
/// * 自制半成品且「有子层、没被截断」⇒ 材料部分继续往下卷积；否则材料部分计入 `unresolved`；
/// * 返回值满足 `material + labor + overhead + unresolved = imputed`（尾差进制造费用）。
fn convolve(node: &NodeView, imputed: Decimal, ctx: &mut Ctx) -> Restored {
    let imputed = money(imputed.max(Decimal::ZERO));
    {
        let flow = slot(ctx, node.level);
        flow.node_count += 1;
        flow.imputed += imputed;
    }
    if imputed.is_zero() {
        return Restored::default();
    }

    // ① 结构取不到：整笔钱停在半成品形态
    let Some(base) = node.elements.as_ref().and_then(parse_elements) else {
        ctx.missing = true;
        slot(ctx, node.level).unresolved += imputed;
        let stopped = true;
        record_semi(
            ctx,
            node,
            imputed,
            &Restored {
                unresolved: imputed,
                ..Restored::default()
            },
            stopped,
        );
        return Restored {
            unresolved: imputed,
            ..Restored::default()
        };
    };

    // ② 按比例拆（尾差进制造费用）
    let (material_part, labor_part, overhead_part) = split_by(&base, imputed);
    let mut out = Restored {
        material: Decimal::ZERO,
        labor: labor_part,
        overhead: overhead_part,
        unresolved: Decimal::ZERO,
    };
    {
        let flow = slot(ctx, node.level);
        flow.labor += labor_part;
        flow.overhead += overhead_part;
    }

    // ③ 外购材料：材料部分全额确认（到这一层就到底）
    if !node.is_semi {
        out.material = material_part;
        slot(ctx, node.level).purchased += material_part;
        return out;
    }

    // ④ 自制半成品：能下钻就继续，否则材料部分停在半成品形态
    let child_total: Decimal = node.children.iter().map(|child| parse_money(&child.amount)).sum();
    let drillable = !node.truncated && !node.children.is_empty() && child_total > Decimal::ZERO;
    if !drillable {
        if node.truncated {
            ctx.truncated = true;
        }
        out.unresolved = material_part;
        slot(ctx, node.level).unresolved += material_part;
        record_semi(ctx, node, imputed, &out, true);
        return out;
    }

    slot(ctx, node.level).carried_down += material_part;
    let mut allocated = Decimal::ZERO;
    let last = node.children.len().saturating_sub(1);
    for (index, child) in node.children.iter().enumerate() {
        let share = if index == last {
            (material_part - allocated).max(Decimal::ZERO)
        } else {
            money(material_part * parse_money(&child.amount) / child_total)
        };
        allocated += share;
        if share.is_zero() {
            continue;
        }
        let sub = convolve(child, share, ctx);
        out.add(&sub);
    }
    // 尾差（兄弟比例除不尽 / 舍入）回收到制造费用，保证本节点与台账都守恒
    let drift = imputed - out.total();
    if !drift.is_zero() {
        out.overhead += drift;
        slot(ctx, node.level).overhead += drift;
    }
    record_semi(ctx, node, imputed, &out, false);
    out
}

/// 按结构比例把金额拆成 (材料, 人工, 制造费用)，三项之和**恒等于** `amount`（4 位）。
///
/// 材料 / 人工各自 4 位取整，制造费用吃下差额（与 `cost_period_item_dto` /
/// M5-26 `clamp_tail` 同源）；病态数据（材料 + 人工 > 金额）依次从人工、材料里拨回来。
fn split_by(base: &ElementSplit, amount: Decimal) -> (Decimal, Decimal, Decimal) {
    let total = base.total();
    if total <= Decimal::ZERO || amount <= Decimal::ZERO {
        return (Decimal::ZERO, Decimal::ZERO, money(amount.max(Decimal::ZERO)));
    }
    let mut material = money((amount * base.material / total).max(Decimal::ZERO));
    let mut labor = money((amount * base.labor / total).max(Decimal::ZERO));
    let mut overhead = money(amount - material - labor);
    if overhead < Decimal::ZERO {
        let mut excess = -overhead;
        overhead = Decimal::ZERO;
        let from_labor = labor.min(excess);
        labor -= from_labor;
        excess -= from_labor;
        if excess > Decimal::ZERO {
            material = (material - excess).max(Decimal::ZERO);
        }
    }
    (material, labor, overhead)
}

/// 记一条半成品还原明细（同层同物料同批号合并，避免同一批在半成品清单里刷屏）。
fn record_semi(ctx: &mut Ctx, node: &NodeView, imputed: Decimal, restored: &Restored, stopped: bool) {
    if !node.is_semi {
        return;
    }
    let producer_nos = node.producer.as_ref().map(|item| item.object_nos.clone());
    let detail = SemiDetail {
        level: node.level,
        material_id: node.material_id.clone(),
        material_code: node.material_code.clone(),
        material_name: node.material_name.clone(),
        lot_no: node.lot_no.clone(),
        imputed,
        source: node.source.clone(),
        source_label: node.source_label.clone(),
        producer_nos,
        restored: *restored,
        stopped,
    };
    if let Some(seen) = ctx.semis.iter_mut().find(|seen| {
        seen.level == detail.level
            && seen.material_id == detail.material_id
            && seen.lot_no == detail.lot_no
            && seen.source == detail.source
    }) {
        seen.imputed += detail.imputed;
        seen.restored.add(&detail.restored);
        seen.stopped = seen.stopped || detail.stopped;
        return;
    }
    ctx.semis.push(detail);
}

/// 整行还原：把第 1 层每个节点的金额各自递归卷积，汇总成还原后的三要素。
///
/// 还原后的**人工 / 制造费用**含两部分：本期归集里本来就有的（`before.labor` /
/// `before.overhead`，它们不是材料，不需要还原）+ 从材料里的半成品拆出来的。
/// `before.material` 与「第 1 层节点金额合计」正常相等（同一批归集行）；
/// 万一不等（表被改过 / 精度问题），差额进 `unresolved`，**不让守恒等式失真**。
pub fn restore_of(tree: &[NodeView], before: ElementSplit) -> Rollup {
    let mut ctx = Ctx::default();
    // 本期自有的人工 / 制造费用直接进还原后（材料才需要还原）
    let mut out = Restored {
        material: Decimal::ZERO,
        labor: before.labor,
        overhead: before.overhead,
        unresolved: Decimal::ZERO,
    };
    let mut covered = Decimal::ZERO;
    for node in tree {
        let imputed = parse_money(&node.amount);
        covered += imputed;
        let sub = convolve(node, imputed, &mut ctx);
        out.add(&sub);
    }
    let gap = money(before.material) - money(covered);
    if !gap.is_zero() {
        // 差额只可能来自表被改过（正常是同一批归集行的合计），按「不彻底」记账并保留事实
        out.unresolved += gap;
        ctx.missing = true;
    }
    // 明细按层排（深层的记录先落到 ctx：卷积是「先子后父」）
    ctx.semis.sort_by_key(|item| item.level);
    Rollup {
        before,
        after: out.confirmed(),
        unresolved: out.unresolved,
        flows: ctx.flows,
        semis: ctx.semis,
        truncated: ctx.truncated,
        missing: ctx.missing,
        covered: money(covered),
    }
}

// ==================== 行状态 ====================

/// 一行的还原形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollupState {
    /// 材料里没有自制半成品。
    NoSemi,
    /// 全部还原干净。
    Restored,
    /// 有金额停在半成品形态。
    Partial,
}

impl RollupState {
    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoSemi => "no_semi",
            Self::Restored => "restored",
            Self::Partial => "partial",
        }
    }

    /// 中文名（2–4 字，列标直接用）。
    pub fn label(&self) -> &'static str {
        match self {
            Self::NoSemi => "无需还原",
            Self::Restored => "已还原",
            Self::Partial => "还原不彻底",
        }
    }
}

/// 取行状态（**纯函数**）。
///
/// `semi_count = 0` ⇒ `no_semi`（材料全是外购料）；否则看有没有停在半成品形态的金额。
pub fn row_state(semi_count: u64, unresolved: Decimal) -> RollupState {
    if semi_count == 0 {
        RollupState::NoSemi
    } else if unresolved > Decimal::ZERO {
        RollupState::Partial
    } else {
        RollupState::Restored
    }
}

/// 状态筛选。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    /// 全部。
    All,
    /// 还原干净的行。
    Restored,
    /// 还原不彻底的行。
    Partial,
    /// 材料里没有半成品。
    NoSemi,
}

/// 状态筛选项（顺序即下拉顺序）。
pub const STATE_FILTERS: [StateFilter; 4] = [
    StateFilter::All,
    StateFilter::Restored,
    StateFilter::Partial,
    StateFilter::NoSemi,
];

impl StateFilter {
    /// 解析（拼错 400，不静默回落到 `all`）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        let text = raw.map(str::trim).filter(|text| !text.is_empty()).unwrap_or("all");
        match text {
            "all" => Ok(Self::All),
            "restored" => Ok(Self::Restored),
            "partial" => Ok(Self::Partial),
            "no_semi" => Ok(Self::NoSemi),
            other => Err(bad_request(&format!(
                "state 只支持 all / restored / partial / no_semi（收到 `{other}`）：\
                 拼错时静默回落到 all 会让人以为筛过了"
            ))),
        }
    }

    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Restored => "restored",
            Self::Partial => "partial",
            Self::NoSemi => "no_semi",
        }
    }

    /// 中文名。
    pub fn label(&self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Restored => "已还原",
            Self::Partial => "不彻底",
            Self::NoSemi => "无需还原",
        }
    }

    /// 命中判据。
    pub fn matches(&self, state: RollupState) -> bool {
        match self {
            Self::All => true,
            Self::Restored => state == RollupState::Restored,
            Self::Partial => state == RollupState::Partial,
            Self::NoSemi => state == RollupState::NoSemi,
        }
    }
}

/// 状态选项（出参）。
pub fn state_options() -> Vec<OptionView> {
    STATE_FILTERS
        .iter()
        .map(|item| OptionView {
            value: item.as_str().to_string(),
            label: item.label().to_string(),
        })
        .collect()
}

// ==================== 出参 ====================

/// 逐层台账（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlowView {
    /// 层级（1 起）。
    pub level: u32,
    /// 本层被卷积的节点数。
    pub node_count: u64,
    /// 流入本层的金额。
    pub imputed: String,
    /// 本层确认为外购材料。
    pub purchased: String,
    /// 本层确认为人工。
    pub labor: String,
    /// 本层确认为制造费用。
    pub overhead: String,
    /// 本层继续往下卷积的金额。
    pub carried_down: String,
    /// 本层无法继续还原。
    pub unresolved: String,
    /// 本层口径说明。
    pub note: String,
}

/// 半成品还原明细（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SemiView {
    /// 层级。
    pub level: u32,
    /// 物料 id。
    pub material_id: String,
    /// 物料编码。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 领料批号。
    pub lot_no: Option<String>,
    /// 被卷积的金额。
    pub imputed: String,
    /// 结构来源（`batch` / `period_avg`）。
    pub source: Option<String>,
    /// 来源中文名。
    pub source_label: Option<String>,
    /// 产出它的成本对象号。
    pub producer_nos: Option<String>,
    /// 还原结果（材料只含外购料；`total` = 流入 − 不彻底）。
    pub elements: ElementView,
    /// 还原不彻底。
    pub unresolved: String,
    /// 是否停在这一层（不再往下钻）。
    pub stopped: bool,
}

/// 还原差额（出参；`total` 恒为 0）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeltaView {
    /// 材料变化（还原后 − 还原前，通常为负）。
    pub material: String,
    /// 人工变化（通常为正）。
    pub labor: String,
    /// 制造费用变化（通常为正）。
    pub overhead: String,
    /// 合计变化（**恒为 0**：还原不动总额）。
    pub total: String,
    /// 口径说明。
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
    /// 本期计算单（没算过 ⇒ `null`）。
    pub item: Option<ItemBrief>,
    /// 本期完工成本（没计算单 ⇒ `null`）。
    pub finished_cost: Option<String>,
    /// 还原前（材料**含**半成品）。
    pub before: ElementView,
    /// 材料里的自制半成品金额。
    pub semi_in_material: String,
    /// 材料里的外购料金额（还原前口径）。
    pub purchased_in_material: String,
    /// 半成品占材料投入的百分比。
    pub semi_share: Option<String>,
    /// 还原后（材料只含外购料）。
    pub after: ElementView,
    /// 还原后材料占还原后合计的百分比。
    pub after_material_share: Option<String>,
    /// 还原后人工占比。
    pub after_labor_share: Option<String>,
    /// 还原后制造费用占比。
    pub after_overhead_share: Option<String>,
    /// 还原差额。
    pub delta: DeltaView,
    /// 还原出来的人工（= 还原后人工 − 还原前人工）。
    pub restored_labor: String,
    /// 还原出来的制造费用。
    pub restored_overhead: String,
    /// 还原不彻底的金额。
    pub unresolved: String,
    /// 行状态。
    pub state: String,
    /// 行状态中文名。
    pub state_label: String,
    /// 还原层数（= 台账层数）。
    pub depth: u32,
    /// 节点数。
    pub node_count: u64,
    /// 逐层台账。
    pub flows: Vec<FlowView>,
    /// 半成品还原明细。
    pub semis: Vec<SemiView>,
    /// 取不到 / 被截断的原因（沿用结构表的事实，不重算）。
    pub issues: Vec<IssueView>,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
}

/// 本期计算单要点（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ItemBrief {
    /// 状态（`CALC` / `SETTLED`）。
    pub status: String,
    /// 完工数量。
    pub completed_qty: String,
    /// 完工转出成本。
    pub finished_cost: String,
    /// 完工单位成本（6 位）。
    pub unit_finished_cost: String,
}

/// 整表合计（内部累加）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    /// 行数。
    pub row_count: u64,
    /// 已还原行数。
    pub restored_row_count: u64,
    /// 不彻底行数。
    pub partial_row_count: u64,
    /// 无需还原行数。
    pub no_semi_row_count: u64,
    /// 还原前材料。
    pub before_material: Decimal,
    /// 还原前人工。
    pub before_labor: Decimal,
    /// 还原前制造费用。
    pub before_overhead: Decimal,
    /// 还原前合计。
    pub before_total: Decimal,
    /// 还原后外购材料。
    pub after_material: Decimal,
    /// 还原后人工。
    pub after_labor: Decimal,
    /// 还原后制造费用。
    pub after_overhead: Decimal,
    /// 还原后合计。
    pub after_total: Decimal,
    /// 还原不彻底合计。
    pub unresolved: Decimal,
    /// 材料里的半成品合计。
    pub semi_in_material: Decimal,
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
            "restored" => self.restored_row_count += 1,
            "partial" => self.partial_row_count += 1,
            "no_semi" => self.no_semi_row_count += 1,
            _ => {}
        }
        self.before_material += parse_money(&row.before.material);
        self.before_labor += parse_money(&row.before.labor);
        self.before_overhead += parse_money(&row.before.overhead);
        self.before_total += parse_money(&row.before.total);
        self.after_material += parse_money(&row.after.material);
        self.after_labor += parse_money(&row.after.labor);
        self.after_overhead += parse_money(&row.after.overhead);
        self.after_total += parse_money(&row.after.total);
        self.unresolved += parse_money(&row.unresolved);
        self.semi_in_material += parse_money(&row.semi_in_material);
        self.max_depth = self.max_depth.max(row.depth);
        self.node_count += row.node_count;
    }
}

/// 合计（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TotalsView {
    /// 行数。
    pub row_count: u64,
    /// 已还原行数。
    pub restored_row_count: u64,
    /// 不彻底行数。
    pub partial_row_count: u64,
    /// 无需还原行数。
    pub no_semi_row_count: u64,
    /// 还原前材料（含半成品）。
    pub before_material: String,
    /// 还原前人工。
    pub before_labor: String,
    /// 还原前制造费用。
    pub before_overhead: String,
    /// 还原前合计。
    pub before_total: String,
    /// 还原后外购材料。
    pub after_material: String,
    /// 还原后人工。
    pub after_labor: String,
    /// 还原后制造费用。
    pub after_overhead: String,
    /// 还原后合计。
    pub after_total: String,
    /// 还原不彻底合计。
    pub unresolved: String,
    /// 材料里的半成品合计。
    pub semi_in_material: String,
    /// 还原后合计 + 不彻底 − 还原前合计（应为 0）。
    pub check: String,
    /// 最深层数。
    pub max_depth: u32,
    /// 节点数合计。
    pub node_count: u64,
    /// 材料占还原后合计的百分比。
    pub after_material_share: Option<String>,
    /// 人工占还原后合计的百分比。
    pub after_labor_share: Option<String>,
    /// 制造费用占还原后合计的百分比。
    pub after_overhead_share: Option<String>,
    /// 合计口径说明。
    pub note: String,
}

/// 合计 → 出参。
pub fn totals_view(totals: &Totals) -> TotalsView {
    TotalsView {
        row_count: totals.row_count,
        restored_row_count: totals.restored_row_count,
        partial_row_count: totals.partial_row_count,
        no_semi_row_count: totals.no_semi_row_count,
        before_material: money_text(totals.before_material),
        before_labor: money_text(totals.before_labor),
        before_overhead: money_text(totals.before_overhead),
        before_total: money_text(totals.before_total),
        after_material: money_text(totals.after_material),
        after_labor: money_text(totals.after_labor),
        after_overhead: money_text(totals.after_overhead),
        after_total: money_text(totals.after_total),
        unresolved: money_text(totals.unresolved),
        semi_in_material: money_text(totals.semi_in_material),
        check: money_text(totals.after_total + totals.unresolved - totals.before_total),
        max_depth: totals.max_depth,
        node_count: totals.node_count,
        after_material_share: share_pct(totals.after_material, totals.after_total).map(pct_text),
        after_labor_share: share_pct(totals.after_labor, totals.after_total).map(pct_text),
        after_overhead_share: share_pct(totals.after_overhead, totals.after_total).map(pct_text),
        note: "合计是整张表的（不受分页影响）；「还原后合计 + 还原不彻底 = 还原前合计」恒成立（check 应为 0）。\
               逐层台账各层不是加总关系：第 2 层起是上一层「材料部分」往下拨的钱，不是新增投入"
            .to_string(),
    }
}

/// 入参。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RollupQuery {
    /// 期间（`yyyy-MM`，必填）。
    pub period_code: Option<String>,
    /// 关键字（对象号 / 生产订单号 / 产出批号 / 产出品编码 / 产出品名称）。
    pub keyword: Option<String>,
    /// 状态筛选（`all` / `restored` / `partial` / `no_semi`）。
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
pub struct RollupFilter {
    /// 期间。
    pub period_code: String,
    /// 关键字。
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
pub struct RollupRes {
    /// 期间。
    pub period_code: String,
    /// 当期成本核算批次。
    pub batch: Option<BatchView>,
    /// 是否已建批次。
    pub cost_ready: bool,
    /// 状态筛选回显。
    pub state: String,
    /// 关键字回显。
    pub keyword: Option<String>,
    /// 下钻层数回显。
    pub max_depth: u32,
    /// 卷积口径。
    pub formula_restore: String,
    /// 不彻底口径。
    pub formula_unresolved: String,
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

// ==================== 台账 → 出参（纯函数） ====================

/// 逐层台账 → 出参（附每层口径说明）。
pub fn flow_views(flows: &[Flow]) -> Vec<FlowView> {
    flows
        .iter()
        .map(|flow| FlowView {
            level: flow.level,
            node_count: flow.node_count,
            imputed: money_text(flow.imputed),
            purchased: money_text(flow.purchased),
            labor: money_text(flow.labor),
            overhead: money_text(flow.overhead),
            carried_down: money_text(flow.carried_down),
            unresolved: money_text(flow.unresolved),
            note: if flow.level <= 1 {
                "第 1 层 = 本期材料投入的去向：外购料确认、半成品按结构拆出工/费，其余继续往下".to_string()
            } else {
                format!(
                    "第 {} 层 = 上一层「材料部分」往下拨的 {}（不是新增投入，不能与上层相加）",
                    flow.level,
                    money_text(flow.imputed)
                )
            },
        })
        .collect()
}

/// 半成品明细 → 出参。
pub fn semi_views(semis: &[SemiDetail]) -> Vec<SemiView> {
    semis
        .iter()
        .map(|item| SemiView {
            level: item.level,
            material_id: item.material_id.clone(),
            material_code: item.material_code.clone(),
            material_name: item.material_name.clone(),
            lot_no: item.lot_no.clone(),
            imputed: money_text(item.imputed),
            source: item.source.clone(),
            source_label: item.source_label.clone(),
            producer_nos: item.producer_nos.clone(),
            elements: item.restored.confirmed().view(),
            unresolved: money_text(item.restored.unresolved),
            stopped: item.stopped,
        })
        .collect()
}

/// 还原差额 → 出参。
pub fn delta_view(before: ElementSplit, after: ElementSplit) -> DeltaView {
    DeltaView {
        material: money_text(after.material - before.material),
        labor: money_text(after.labor - before.labor),
        overhead: money_text(after.overhead - before.overhead),
        total: money_text(after.total() - before.total()),
        note: "还原只是把材料里的半成品换成它自己的料/工/费，总额一分不动（total 恒为 0）；\
               材料减少 = 人工与制造费用增加之和"
            .to_string(),
    }
}

// ==================== 错误 ====================

/// 400 + `PARAM_INVALID`。
pub fn bad_request(message: &str) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.to_string())
}

/// 500 + `INTERNAL_ERROR`。
pub fn internal_error(message: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, message.to_string())
}

/// 文本金额 → `Decimal`（出参字符串回读的短名字，单测与装配都用它）。
pub fn dec(text: &str) -> Decimal {
    parse_money(text)
}

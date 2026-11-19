//! 在制品「期末在制余额 + 结转核对」的**契约层与纯逻辑**（成本核算 · M5-23）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-wip-balance`（AGENTS 约定 8）。
//!
//! # 这张表回答什么
//!
//! 截至 `period_code` 的**期末在制余额台账**：一行 = 一个成本对象，把余额的来龙去脉摆平，
//! 再把「这笔余额有没有交给下一期」核出来。
//!
//! ```text
//! 期初在制（上期已结算计算单的期末，按上期单位成本还原到料 / 工 / 费）
//!   + 本期投入（biz_cost_collect 三要素）
//!   − 完工转出
//! = 期末在制余额（差额倒挤，三要素合计守恒）        ← 与 M5-22 的 elements[].wip 同一口径
//!
//! 结转 = 本期**账面**期末在制 → 下一期计算单的期初在制（同一成本对象）
//! ```
//!
//! # 为什么「结转」不是一张新表、不是一次写库
//!
//! 设计 §4.6.3 定了口径：期初在制**自动**从上期**已结算**计算单的期末在制取数
//! （`cost_period_item_model::prior_settled_item`）。也就是说结转在库里本来就有唯一真相 ——
//! `biz_cost_period_item.wip_end_cost`，下一期算的时候自然读走。
//!
//! 再建一张「结转表」会是**第二本成本账**（ARCH-4 D9 明令禁止的反例：两处同步必漂移），
//! 所以本卡不写库、不迁移，而是把「结转到底成没成」**逐行核出来**：
//!
//! | 结转状态 | 判据（全部取自库里已有的数） |
//! |---|---|
//! | `carried` 已结转 | 本期已结算，且下一期计算单的 `wip_beg_cost` / `wip_beg_qty` 与本期账面期末**一分不差** |
//! | `pending` 待结转 | 本期已结算，下一期还没有计算单（下期算的时候自动取走，不用手工搬） |
//! | `mismatch` 结转不齐 | 本期已结算，但下一期期初 ≠ 本期账面期末（差额逐行给数） |
//! | `unsettled` 未入账 | 本期**没有**已结算计算单，试算余额还没入账 ⇒ 下一期取不到（**最危险的一类**） |
//! | `reversed` 已红冲 | 本期批次被红冲：账面余额作废，重算后再看 |
//! | `closed_wip` 关批挂账 | 成本对象已 `CLOSED` 却仍挂着期末在制 ⇒ 不会再有完工，这笔余额永远结不掉 |
//! | `cleared` 无余额 | 期末在制为 0（全部完工），没有东西要结转 |
//! | `blocked` 算不出 | 连余额都算不出来（缺完工率 / 无产量 / 期初还原失败 …，见 `issues`） |
//!
//! `unsettled` 之所以最危险：它**看着有余额**（表里给的是试算数），但库里没有已结算快照，
//! 下一期算的时候期初取 0 —— 这笔钱会凭空变成下期的完工成本。所以它单列一档、单独计数，
//! 并且在合计里单给一个「未入账余额」。
//!
//! # 账面的优先级（已结算的行以快照为准）
//!
//! 已结算的行，**三个金额列（期初 / 完工转出 / 期末余额）取计算单快照**（`SETTLED` 行的
//! `wip_beg_cost` / `finished_cost` / `wip_end_cost`）—— 那才是账上的数。分要素的拆分必须能与
//! 总额加得起来，所以：
//!
//! * 试算与账面一致（M5-22 的 `recalc_required = false`）⇒ 分要素照给（三项加起来 = 余额）；
//! * 试算与账面不一致 ⇒ **分要素一律给 `null`** + 一条 `LEDGER_DRIFT`：算了之后又改过数量 /
//!   完工率 / 归集，此时按试算拆出来的三要素与账面余额**加不齐**，展示出来就是一笔读不懂的账。
//!   修法是红冲本期重算（已结算的单不能直接重算）。
//!
//! 未结算的行：余额给试算值，`source = live`，并在行提示「试算未入账」。
//!
//! # 与其它卡的分工（本卡不越界）
//!
//! * **M5-20 约当产量表**：只出分母；**M5-22 分配表**：一期之内「钱怎么在完工与在制之间分」；
//! * **M5-23（本卡）**：跨期视角 —— 余额怎么滚过来的 + 有没有滚出去（结转）；
//!   分要素的钱**直接吃 M5-22 的 `elements[].wip`**，不另起公式（父卡交接的硬要求）；
//! * **M5-24 在制品单测 / M5-25 前端页**：本卡只出 API 与单测。
//!
//! # 只读
//!
//! 零写库、零迁移、零新增权限码（复用 `business:erp:cost-object:list`，与
//! `cost-wip-equiv` / `cost-wip-alloc` / `cost-collect-sheet` / `wip-balance` 同码：
//! 看得到成本对象的人就是该看这张表的人）。因此也没有「事务提交后失败留死单」的面
//! （AGENTS 约定 11）。4001 科目余额的勾稽由巡检 C-COST-3 兜底（`scripts/verify/erp_invariants.py`），
//! 本表不重复算凭证账。

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use crate::business::erp::cost::cost_period_item_dto;
use crate::business::erp::cost::cost_wip_alloc_dto::{
    self as alloc, AllocCalc, AllocFacts, Element, money_text, pct_text, qty_text,
};
use crate::business::erp::cost::cost_wip_equiv_dto::{EquivState, IssueView, RowHead};
use cloudpivot_kernel::shared::error_code;

pub use crate::business::erp::cost::cost_wip_equiv_dto::{BatchView, OptionView};

// ==================== 权限码 ====================

/// 余额表读取（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_LIST: &str = "business:erp:cost-object:list";

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const PERMISSIONS: [&str; 1] = [PERM_LIST];

// ==================== 公式文案 ====================

/// 余额的滚动公式（页脚 / 前端 tooltip 直接用）。
pub const FORMULA_ROLL: &str = "期末在制余额 = 期初在制 + 本期投入 − 完工转出（差额倒挤；期初 = 上期已结算计算单的期末在制，\
                                按上期单位成本还原到料 / 工 / 费，设计 §4.6.3）";

/// 结转的口径。
pub const FORMULA_CARRY: &str = "结转 = 本期账面的期末在制 → 下一期计算单的期初在制（同一成本对象，一分不差）；\
                                 本期没有已结算的计算单 ⇒ 账面没有这笔余额，下一期取不到";

/// 金额精度（与库列 `NUMERIC(18,4)` 一致）。
pub const MONEY_SCALE: u32 = cost_period_item_dto::MONEY_SCALE;

// ==================== 入参事实 ====================

/// 账面余额（本期已结算计算单的期末在制；没有已结算行 ⇒ `None`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BookBalance {
    /// 账面期末在制数量。
    pub wip_end_qty: Decimal,
    /// 账面期末在制成本。
    pub wip_end_cost: Decimal,
}

/// 下一期（本期之后**最早**的一张计算单）里当「期初」用的那几列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextFacts {
    /// 下一期期间（`yyyy-MM`）。
    pub period_code: String,
    /// 下一期的期初在制数量。
    pub wip_beg_qty: Decimal,
    /// 下一期的期初在制成本。
    pub wip_beg_cost: Decimal,
    /// 下一期计算单行状态（`CALC` / `SETTLED`）。
    pub item_status: String,
    /// 下一期批次状态。
    pub period_status: String,
}

/// 本期批次（活跃的，或最近一次红冲的那条）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRef {
    /// 批次 id。
    pub id: String,
    /// 批次号。
    pub batch_no: String,
    /// 批次状态。
    pub status: String,
    /// 红冲原因（`REVERSED` 才有）。
    pub reverse_reason: Option<String>,
}

impl BatchRef {
    /// 这条批次是不是红冲留痕行。
    pub fn reversed(&self) -> bool {
        self.status == "REVERSED"
    }
}

// ==================== 结转状态 ====================

/// 一行的结转状态（见模块头的判据表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryState {
    /// 已结转：下期期初与本期账面期末一分不差。
    Carried,
    /// 待结转：本期已结算，下期还没算。
    Pending,
    /// 结转不齐：下期期初 ≠ 本期账面期末。
    Mismatch,
    /// 未入账：本期没有已结算计算单，下期取不到。
    Unsettled,
    /// 本期批次已红冲。
    Reversed,
    /// 关批仍挂期末在制。
    ClosedWip,
    /// 期末在制为 0，无余额。
    Cleared,
    /// 余额算不出来。
    Blocked,
}

impl CarryState {
    /// 出参值（前端判色 / 判图标用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Carried => "carried",
            Self::Pending => "pending",
            Self::Mismatch => "mismatch",
            Self::Unsettled => "unsettled",
            Self::Reversed => "reversed",
            Self::ClosedWip => "closed_wip",
            Self::Cleared => "cleared",
            Self::Blocked => "blocked",
        }
    }

    /// 中文名（2–4 字，筛选项与列标用）。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Carried => "已结转",
            Self::Pending => "待结转",
            Self::Mismatch => "结转不齐",
            Self::Unsettled => "未入账",
            Self::Reversed => "已红冲",
            Self::ClosedWip => "关批挂账",
            Self::Cleared => "无余额",
            Self::Blocked => "算不出",
        }
    }

    /// 需要人去处理（前端标红的那几档）。
    pub fn needs_attention(&self) -> bool {
        matches!(
            self,
            Self::Mismatch | Self::Unsettled | Self::Reversed | Self::ClosedWip | Self::Blocked
        )
    }
}

/// 一行余额的结转核对结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryOutcome {
    /// 状态。
    pub state: CarryState,
    /// 下期期间（有计算单时给实际期间，否则给建议的下一期）。
    pub to_period: Option<String>,
    /// 下期实际取走的期初数量。
    pub to_beg_qty: Option<Decimal>,
    /// 下期实际取走的期初成本。
    pub to_beg_cost: Option<Decimal>,
    /// 数量差（下期期初 − 本期账面期末）。
    pub diff_qty: Option<Decimal>,
    /// 成本差（下期期初 − 本期账面期末）。
    pub diff_cost: Option<Decimal>,
    /// 一句话说明（含「怎么修」）。
    pub hint: String,
}

// ==================== 结转核对（纯函数） ====================

/// 核一行余额的结转状态（**纯函数**：不连库、不读时钟、不写日志）。
///
/// # 判定顺序（先到先得，顺序本身是语义）
///
/// 1. 余额算不出来 ⇒ `blocked`（连数都没有，谈不上结转）；
/// 2. 本期批次已红冲 ⇒ `reversed`（账面余额作废）；
/// 3. 期末在制 0 且账面无余额 ⇒ `cleared`；
/// 4. 关批仍挂余额 ⇒ `closed_wip`（比「待结转」更急：永远结不掉）；
/// 5. 本期没有已结算行 ⇒ `unsettled`；
/// 6. 已结算：下期没算 ⇒ `pending`；下期期初对得上 ⇒ `carried`，对不上 ⇒ `mismatch`。
pub fn carry_of(
    period_code: &str,
    calc: &AllocCalc,
    book: Option<BookBalance>,
    object_status: &str,
    batch: Option<&BatchRef>,
    next: Option<&NextFacts>,
) -> CarryOutcome {
    let suggest = crate::business::erp::cost::cost_batch_wip_dto::next_period(period_code);
    let empty = |state: CarryState, hint: String| CarryOutcome {
        state,
        to_period: None,
        to_beg_qty: None,
        to_beg_cost: None,
        diff_qty: None,
        diff_cost: None,
        hint,
    };

    // ① 算不出来：连余额都没有
    if calc.state == EquivState::Blocked {
        let fix = calc
            .issues
            .first()
            .map(|issue| format!("（{}：{}）", issue.code, issue.fix))
            .unwrap_or_default();
        return empty(
            CarryState::Blocked,
            format!(
                "本期余额算不出来，谈不上结转：先按下面 issues 修数据{fix}，再重算本期；\
                 本条余额不在合计里（不按 0 加进去）"
            ),
        );
    }

    // ② 本期批次已红冲：账面余额作废
    if let Some(batch) = batch.filter(|batch| batch.reversed()) {
        let reason = batch
            .reverse_reason
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or("未填原因");
        return empty(
            CarryState::Reversed,
            format!(
                "本期批次 {} 已红冲（{reason}）：账面期末在制已作废，下一期不会取这笔。\
                 重新计算本期（POST /business/erp/cost-period/calc）后再结算，余额才会重新成立",
                batch.batch_no
            ),
        );
    }

    let wip_qty = calc.wip_qty;
    let book_cost = book.map(|book| cost_period_item_dto::money(book.wip_end_cost));
    // 试算余额（没结算时用它判断「有没有余额」；blocked 已在上面拦掉，这里是 Some）
    let live_cost = calc.wip_total.map(cost_period_item_dto::money);
    let balance = book_cost.or(live_cost).unwrap_or(Decimal::ZERO);

    // ③ 没有余额
    if wip_qty.is_zero() && balance.is_zero() {
        return empty(
            CarryState::Cleared,
            "本期期末在制为 0（全部完工）：没有余额要结转".to_string(),
        );
    }

    // ④ 关批还挂着余额：永远结不掉
    if object_status == "CLOSED" {
        return CarryOutcome {
            state: CarryState::ClosedWip,
            to_period: next.map(|next| next.period_code.clone()).or(suggest),
            to_beg_qty: next.map(|next| next.wip_beg_qty),
            to_beg_cost: next.map(|next| cost_period_item_dto::money(next.wip_beg_cost)),
            diff_qty: None,
            diff_cost: None,
            hint: format!(
                "成本对象已关批（CLOSED）却仍挂着期末在制 {} / {} 件：关批之后不会再有完工，\
                 这笔余额会一期一期滚下去永远结不掉。要么改回 OPEN 继续完工（POST /business/erp/cost-object/{{id}} 改状态），\
                 要么按呆滞 / 报废处理（成本调整），不要让它一直挂着",
                cost_period_item_dto::fmt(balance, MONEY_SCALE),
                qty_text(wip_qty)
            ),
        };
    }

    // ⑤ 本期没有已结算行：账面没这笔余额
    let Some(book) = book else {
        let taken = match next {
            Some(next) => format!(
                "下一期 {} 的期初取的是 {}（{}），不是本期的试算余额",
                next.period_code,
                cost_period_item_dto::fmt(cost_period_item_dto::money(next.wip_beg_cost), MONEY_SCALE),
                next.period_status
            ),
            None => format!(
                "下一期 {} 还没有计算单，现在算也只会取到更早的已结算期间（或 0）",
                suggest.clone().unwrap_or_else(|| "（下一期）".to_string())
            ),
        };
        return CarryOutcome {
            state: CarryState::Unsettled,
            to_period: next.map(|next| next.period_code.clone()).or(suggest),
            to_beg_qty: next.map(|next| next.wip_beg_qty),
            to_beg_cost: next.map(|next| cost_period_item_dto::money(next.wip_beg_cost)),
            diff_qty: None,
            diff_cost: None,
            hint: format!(
                "本期还没结算：试算余额 {} / {} 件**还没入账**，下一期取不到它 —— {taken}。\
                 结算本期（POST /business/erp/cost-period/{{id}}/settle）后下期再算才会带上这笔",
                cost_period_item_dto::fmt(balance, MONEY_SCALE),
                qty_text(wip_qty)
            ),
        };
    };

    // ⑥ 已结算：核对下一期
    match next {
        None => CarryOutcome {
            state: CarryState::Pending,
            to_period: suggest,
            to_beg_qty: None,
            to_beg_cost: None,
            diff_qty: None,
            diff_cost: None,
            hint: format!(
                "本期已结算（账面期末在制 {} / {} 件），下一期还没有计算单：下期执行成本计算时\
                 会自动把它取成期初（设计 §4.6.3），不用手工搬数",
                cost_period_item_dto::fmt(cost_period_item_dto::money(book.wip_end_cost), MONEY_SCALE),
                qty_text(book.wip_end_qty)
            ),
        },
        Some(next) => {
            let diff_cost =
                cost_period_item_dto::money(next.wip_beg_cost) - cost_period_item_dto::money(book.wip_end_cost);
            let diff_qty = next.wip_beg_qty - book.wip_end_qty;
            if diff_cost.is_zero() && diff_qty.is_zero() {
                CarryOutcome {
                    state: CarryState::Carried,
                    to_period: Some(next.period_code.clone()),
                    to_beg_qty: Some(next.wip_beg_qty),
                    to_beg_cost: Some(cost_period_item_dto::money(next.wip_beg_cost)),
                    diff_qty: Some(Decimal::ZERO),
                    diff_cost: Some(Decimal::ZERO),
                    hint: format!(
                        "已结转：下一期 {} 的期初 {} / {} 件与本期账面期末一分不差",
                        next.period_code,
                        cost_period_item_dto::fmt(cost_period_item_dto::money(next.wip_beg_cost), MONEY_SCALE),
                        qty_text(book.wip_end_qty)
                    ),
                }
            } else {
                CarryOutcome {
                    state: CarryState::Mismatch,
                    to_period: Some(next.period_code.clone()),
                    to_beg_qty: Some(next.wip_beg_qty),
                    to_beg_cost: Some(cost_period_item_dto::money(next.wip_beg_cost)),
                    diff_qty: Some(diff_qty),
                    diff_cost: Some(diff_cost),
                    hint: format!(
                        "结转不齐：下一期 {} 的期初 {} / {} 件，本期账面期末 {} / {} 件，\
                         差 {} / {} 件。常见成因：下期算完之后本期被红冲重算，或有人手工改过计算单的期初。\
                         按差额核对下期期初来源；确认要改就红冲下期批次再重算",
                        next.period_code,
                        cost_period_item_dto::fmt(cost_period_item_dto::money(next.wip_beg_cost), MONEY_SCALE),
                        qty_text(next.wip_beg_qty),
                        cost_period_item_dto::fmt(cost_period_item_dto::money(book.wip_end_cost), MONEY_SCALE),
                        qty_text(book.wip_end_qty),
                        cost_period_item_dto::fmt(diff_cost, MONEY_SCALE),
                        qty_text(diff_qty)
                    ),
                }
            }
        }
    }
}

// ==================== 一行的余额滚动 ====================

/// 余额的来源口径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceSource {
    /// 本期已结算：三个金额列取计算单快照（账上的数）。
    Settled,
    /// 本期未结算：余额是试算值（未入账）。
    Live,
}

impl BalanceSource {
    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Settled => "settled",
            Self::Live => "live",
        }
    }

    /// 中文名。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Settled => "已入账",
            Self::Live => "试算未入账",
        }
    }
}

/// 一个要素的余额滚动（期初 → 投入 → 转出 → 期末）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ElementRoll {
    /// 要素。
    pub element: Option<Element>,
    /// 期初在制。
    pub beg: Option<Decimal>,
    /// 本期投入。
    pub cur: Option<Decimal>,
    /// 完工转出。
    pub finished: Option<Decimal>,
    /// 期末在制余额。
    pub wip: Option<Decimal>,
}

/// 一行的余额滚动结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceCalc {
    /// 行头数量：期初在制数量（上期期末）。
    pub beg_qty: Decimal,
    /// 期初在制金额。
    pub beg_total: Option<Decimal>,
    /// 本期投入金额。
    pub cur_total: Option<Decimal>,
    /// 完工转出金额。
    pub finished_total: Option<Decimal>,
    /// 期末在制余额（**账面**：已结算取快照，未结算取试算）。
    pub wip_total: Option<Decimal>,
    /// 三个要素（固定顺序：料 / 工 / 费；算不出 / 账面漂移时各金额为 `None`）。
    pub elements: Vec<ElementRoll>,
    /// 口径来源。
    pub source: BalanceSource,
    /// 账面余额（`source = settled` 才有）。
    pub book: Option<BookBalance>,
    /// 已结算但试算与账面不一致（分要素一律不给，避免三项加不出余额）。
    pub ledger_drift: bool,
}

impl BalanceCalc {
    /// 取某要素的滚动。
    pub fn element_of(&self, element: Element) -> &ElementRoll {
        &self.elements[element.index()]
    }

    /// 分要素是否可用（三项加起来 = 余额）。
    pub fn elements_available(&self) -> bool {
        !self.ledger_drift && self.wip_total.is_some()
    }
}

/// 把 M5-22 的分配结果装成「余额滚动」。
///
/// **不另起公式**：期初 / 投入 / 完工 / 期末逐要素直接取 [`AllocCalc::elements`]
/// （父卡交接要求「消费同一口径」），只在此之上加「账面优先」与「分要素能不能展示」两条规则。
pub fn balance_of(facts: &AllocFacts, calc: &AllocCalc) -> BalanceCalc {
    let beg_qty = facts
        .prior
        .as_ref()
        .map(|prior| prior.wip_end_qty)
        .unwrap_or(Decimal::ZERO);
    let settled = facts.stored.as_ref().filter(|stored| stored.settled());
    let blocked = calc.state == EquivState::Blocked;

    let elements: Vec<ElementRoll> = Element::ALL
        .iter()
        .map(|element| {
            let item = calc.element_of(*element);
            if blocked {
                return ElementRoll {
                    element: Some(*element),
                    ..Default::default()
                };
            }
            ElementRoll {
                element: Some(*element),
                beg: item.beg.map(cost_period_item_dto::money),
                cur: Some(cost_period_item_dto::money(item.cur)),
                finished: item.finished.map(cost_period_item_dto::money),
                wip: item.wip.map(cost_period_item_dto::money),
            }
        })
        .collect();

    let mut out = BalanceCalc {
        beg_qty,
        beg_total: calc.beg_total.map(cost_period_item_dto::money),
        cur_total: if blocked {
            None
        } else {
            Some(cost_period_item_dto::money(calc.cur_total))
        },
        finished_total: calc.finished_total.map(cost_period_item_dto::money),
        wip_total: calc.wip_total.map(cost_period_item_dto::money),
        elements,
        source: if settled.is_some() {
            BalanceSource::Settled
        } else {
            BalanceSource::Live
        },
        book: None,
        ledger_drift: false,
    };

    // 已结算：三个金额列以快照为准（账上的数）
    if let Some(stored) = settled {
        out.book = Some(BookBalance {
            wip_end_qty: stored.wip_end_qty,
            wip_end_cost: cost_period_item_dto::money(stored.wip_end_cost),
        });
        out.beg_total = Some(cost_period_item_dto::money(stored.wip_beg_cost));
        out.finished_total = Some(cost_period_item_dto::money(stored.finished_cost));
        out.wip_total = Some(cost_period_item_dto::money(stored.wip_end_cost));
        if calc.recalc_required {
            // 试算与账面不一致：按试算拆出来的三要素加不出余额 ⇒ 一律不给（不画假明细）
            out.ledger_drift = true;
            for item in &mut out.elements {
                item.beg = None;
                item.cur = None;
                item.finished = None;
                item.wip = None;
            }
        }
    }
    out
}

// ==================== 合计 ====================

/// 整张表的合计（**不是当前页**：翻页时这个数不能变）。
///
/// 金额合计跳过 `blocked` 行（它们的余额为空，按 0 加进去会让合计看起来像丢了钱）；
/// 数量合计算全部行。分状态计数让人一眼看出「多少钱还没入账 / 结转不齐」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BalanceTotals {
    /// 行数。
    pub row_count: u64,
    /// 有余额的行数（`carry != cleared`）。
    pub wip_row_count: u64,
    /// 已结转的行数。
    pub carried_count: u64,
    /// 待结转的行数。
    pub pending_count: u64,
    /// 结转不齐的行数。
    pub mismatch_count: u64,
    /// 未入账的行数。
    pub unsettled_count: u64,
    /// 已红冲的行数。
    pub reversed_count: u64,
    /// 关批挂账的行数。
    pub closed_wip_count: u64,
    /// 无余额的行数。
    pub cleared_count: u64,
    /// 算不出来的行数。
    pub blocked_count: u64,
    /// 期初在制数量合计。
    pub beg_qty: Decimal,
    /// 期末在制数量合计。
    pub wip_qty: Decimal,
    /// 期初在制金额合计。
    pub beg_total: Decimal,
    /// 本期投入合计。
    pub cur_total: Decimal,
    /// 完工转出合计。
    pub finished_total: Decimal,
    /// 期末在制余额合计。
    pub wip_total: Decimal,
    /// 期末在制按要素（料）。
    pub wip_material: Decimal,
    /// 期末在制按要素（人工）。
    pub wip_labor: Decimal,
    /// 期末在制按要素（制造费用）。
    pub wip_overhead: Decimal,
    /// 分要素可加的行数（其余行的分要素为空，不进三要素合计）。
    pub element_row_count: u64,
}

impl BalanceTotals {
    /// 累加一行。
    pub fn add(&mut self, calc: &AllocCalc, roll: &BalanceCalc, carry: &CarryOutcome) {
        self.row_count += 1;
        self.wip_qty += calc.wip_qty;
        self.beg_qty += roll.beg_qty;
        match carry.state {
            CarryState::Carried => self.carried_count += 1,
            CarryState::Pending => self.pending_count += 1,
            CarryState::Mismatch => self.mismatch_count += 1,
            CarryState::Unsettled => self.unsettled_count += 1,
            CarryState::Reversed => self.reversed_count += 1,
            CarryState::ClosedWip => self.closed_wip_count += 1,
            CarryState::Cleared => self.cleared_count += 1,
            CarryState::Blocked => self.blocked_count += 1,
        }
        if carry.state != CarryState::Cleared {
            self.wip_row_count += 1;
        }
        let Some(wip_total) = roll.wip_total else {
            // 算不出来的行：金额一律不进合计（数量照进）
            return;
        };
        self.wip_total += wip_total;
        if let Some(value) = roll.beg_total {
            self.beg_total += value;
        }
        if let Some(value) = roll.cur_total {
            self.cur_total += value;
        }
        if let Some(value) = roll.finished_total {
            self.finished_total += value;
        }
        if roll.elements_available() {
            self.element_row_count += 1;
            self.wip_material += roll.element_of(Element::Material).wip.unwrap_or(Decimal::ZERO);
            self.wip_labor += roll.element_of(Element::Labor).wip.unwrap_or(Decimal::ZERO);
            self.wip_overhead += roll.element_of(Element::Overhead).wip.unwrap_or(Decimal::ZERO);
        }
    }
}

// ==================== 入参 ====================

/// 查询参数（HTTP 层直接 `Query`）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct BalanceQuery {
    /// 期间 `yyyy-MM`（**必填**）。
    pub period_code: Option<String>,
    /// 关键字（对象号 / 生产订单号 / 批号 / 产出品编码 / 名称）。
    pub keyword: Option<String>,
    /// 状态筛选（缺省 `wip`：只显示有余额的行）。
    pub state: Option<String>,
    /// 页码（从 1 起）。
    pub page: Option<u64>,
    /// 每页条数（上限 200）。
    pub page_size: Option<u64>,
}

/// 归一化后的筛选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceFilter {
    /// 期间。
    pub period_code: String,
    /// 关键字。
    pub keyword: Option<String>,
    /// 状态筛选。
    pub state: BalanceStateFilter,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 列表的状态筛选（**只读筛选用**，不影响算数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceStateFilter {
    /// 全部行（含无余额）。
    All,
    /// 有余额（默认：期末在制 > 0 或账面有余额）。
    Wip,
    /// 已结转。
    Carried,
    /// 待结转。
    Pending,
    /// 结转不齐。
    Mismatch,
    /// 未入账。
    Unsettled,
    /// 已红冲。
    Reversed,
    /// 关批挂账。
    ClosedWip,
    /// 算不出来。
    Blocked,
    /// 无余额。
    Cleared,
}

impl BalanceStateFilter {
    /// 入参值 → 枚举（缺省 / 空串 = `wip`；拼错 400，不静默回落）。
    pub fn parse(raw: Option<&str>) -> std::result::Result<Self, ApiError> {
        let text = raw.map(str::trim).unwrap_or("").to_ascii_lowercase();
        match text.as_str() {
            "" | "wip" => Ok(Self::Wip),
            "all" => Ok(Self::All),
            "carried" => Ok(Self::Carried),
            "pending" => Ok(Self::Pending),
            "mismatch" => Ok(Self::Mismatch),
            "unsettled" => Ok(Self::Unsettled),
            "reversed" => Ok(Self::Reversed),
            "closed_wip" => Ok(Self::ClosedWip),
            "blocked" => Ok(Self::Blocked),
            "cleared" => Ok(Self::Cleared),
            other => Err(alloc::bad_request(&format!(
                "state（筛选）只能是 wip / all / carried / pending / mismatch / unsettled / \
                 reversed / closed_wip / blocked / cleared，收到 {other}"
            ))),
        }
    }

    /// 出参值。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Wip => "wip",
            Self::Carried => "carried",
            Self::Pending => "pending",
            Self::Mismatch => "mismatch",
            Self::Unsettled => "unsettled",
            Self::Reversed => "reversed",
            Self::ClosedWip => "closed_wip",
            Self::Blocked => "blocked",
            Self::Cleared => "cleared",
        }
    }

    /// 中文名。
    pub fn label(&self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::Wip => "有余额",
            Self::Carried => CarryState::Carried.label(),
            Self::Pending => CarryState::Pending.label(),
            Self::Mismatch => CarryState::Mismatch.label(),
            Self::Unsettled => CarryState::Unsettled.label(),
            Self::Reversed => CarryState::Reversed.label(),
            Self::ClosedWip => CarryState::ClosedWip.label(),
            Self::Blocked => CarryState::Blocked.label(),
            Self::Cleared => CarryState::Cleared.label(),
        }
    }

    /// 这一行是否命中筛选。
    pub fn matches(&self, carry: &CarryOutcome) -> bool {
        match self {
            Self::All => true,
            Self::Wip => carry.state != CarryState::Cleared,
            Self::Carried => carry.state == CarryState::Carried,
            Self::Pending => carry.state == CarryState::Pending,
            Self::Mismatch => carry.state == CarryState::Mismatch,
            Self::Unsettled => carry.state == CarryState::Unsettled,
            Self::Reversed => carry.state == CarryState::Reversed,
            Self::ClosedWip => carry.state == CarryState::ClosedWip,
            Self::Blocked => carry.state == CarryState::Blocked,
            Self::Cleared => carry.state == CarryState::Cleared,
        }
    }
}

/// 下拉选项（顺序即前端展示顺序）。
pub const STATE_FILTERS: [BalanceStateFilter; 10] = [
    BalanceStateFilter::Wip,
    BalanceStateFilter::All,
    BalanceStateFilter::Unsettled,
    BalanceStateFilter::Pending,
    BalanceStateFilter::Carried,
    BalanceStateFilter::Mismatch,
    BalanceStateFilter::ClosedWip,
    BalanceStateFilter::Reversed,
    BalanceStateFilter::Blocked,
    BalanceStateFilter::Cleared,
];

// ==================== 出参 ====================

/// 一个要素的余额滚动（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ElementRollView {
    /// 要素码。
    pub element: String,
    /// 中文名。
    pub label: String,
    /// 期初在制。
    pub beg: Option<String>,
    /// 本期投入。
    pub cur: Option<String>,
    /// 完工转出。
    pub finished: Option<String>,
    /// 期末在制余额。
    pub wip: Option<String>,
}

/// 账面快照（已结算那几列；没结算 ⇒ `null`）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct LedgerView {
    /// 计算单状态。
    pub status: String,
    /// 账面期末在制数量。
    pub wip_end_qty: String,
    /// 账面期末在制成本。
    pub wip_end_cost: String,
    /// 试算与账面一致（`false` ⇒ 分要素不给，见 `note`）。
    pub agrees: bool,
    /// 一句话说明。
    pub note: String,
}

/// 结转核对（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CarryView {
    /// 状态值。
    pub state: String,
    /// 状态中文名。
    pub label: String,
    /// 需要处理（前端标红）。
    pub needs_attention: bool,
    /// 下期期间（没算时给建议的下一期）。
    pub to_period: Option<String>,
    /// 下期实际取走的期初数量。
    pub to_beg_qty: Option<String>,
    /// 下期实际取走的期初成本。
    pub to_beg_cost: Option<String>,
    /// 数量差。
    pub diff_qty: Option<String>,
    /// 成本差。
    pub diff_cost: Option<String>,
    /// 一句话说明（含怎么修）。
    pub hint: String,
}

/// 一行（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BalanceRow {
    /// 成本对象 id。
    pub object_id: String,
    /// 对象号。
    pub object_no: String,
    /// 对象类型。
    pub object_type: String,
    /// 对象状态（`OPEN` / `CLOSED`）。
    pub object_status: String,
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
    /// 本期完工数量。
    pub completed_qty: String,
    /// 期初在制数量（上期期末）。
    pub beg_qty: String,
    /// 期初在制期间（上期；首期 ⇒ `null`）。
    pub beg_period: Option<String>,
    /// 期末在制数量。
    pub wip_qty: String,
    /// 完工率 %。
    pub completion_pct: String,
    /// 期初在制金额。
    pub beg_total: Option<String>,
    /// 本期投入。
    pub cur_total: Option<String>,
    /// 完工转出。
    pub finished_total: Option<String>,
    /// **期末在制余额**（本表的主角）。
    pub wip_total: Option<String>,
    /// 三个要素的滚动（固定顺序：料 / 工 / 费）。
    pub elements: Vec<ElementRollView>,
    /// 分要素能不能用（三项加起来 = 余额）。
    pub elements_available: bool,
    /// 口径来源值（`settled` / `live`）。
    pub source: String,
    /// 口径来源中文名。
    pub source_label: String,
    /// 账面快照。
    pub ledger: Option<LedgerView>,
    /// 结转核对。
    pub carry: CarryView,
    /// 算不出来的原因。
    pub issues: Vec<IssueView>,
    /// 只提示不拦的提醒。
    pub warnings: Vec<String>,
}

/// 合计（出参）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BalanceTotalsView {
    /// 行数。
    pub row_count: u64,
    /// 有余额的行数。
    pub wip_row_count: u64,
    /// 已结转行数。
    pub carried_count: u64,
    /// 待结转行数。
    pub pending_count: u64,
    /// 结转不齐行数。
    pub mismatch_count: u64,
    /// 未入账行数。
    pub unsettled_count: u64,
    /// 已红冲行数。
    pub reversed_count: u64,
    /// 关批挂账行数。
    pub closed_wip_count: u64,
    /// 无余额行数。
    pub cleared_count: u64,
    /// 算不出来行数。
    pub blocked_count: u64,
    /// 期初在制数量合计。
    pub beg_qty: String,
    /// 期末在制数量合计。
    pub wip_qty: String,
    /// 期初在制金额合计。
    pub beg_total: String,
    /// 本期投入合计。
    pub cur_total: String,
    /// 完工转出合计。
    pub finished_total: String,
    /// 期末在制余额合计。
    pub wip_total: String,
    /// 期末在制按要素（料）。
    pub wip_material: String,
    /// 期末在制按要素（人工）。
    pub wip_labor: String,
    /// 期末在制按要素（制造费用）。
    pub wip_overhead: String,
    /// 未入账的余额合计（`unsettled` 行：试算有、账面没有）。
    pub unbooked_total: String,
    /// 关批挂账的余额合计。
    pub closed_wip_total: String,
    /// 结转不齐的余额合计（金额差取绝对值加总，正负不互相抵消）。
    pub mismatch_abs_total: String,
    /// 分要素可加的行数。
    pub element_row_count: u64,
    /// 合计口径的一句话。
    pub note: String,
}

/// 整张表的出参。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BalanceRes {
    /// 期间。
    pub period_code: String,
    /// 当期成本核算批次（没建 / 已红冲 ⇒ `null`）。
    pub batch: Option<BatchView>,
    /// 最近一次红冲的批次（有 ⇒ 本表所有行都按「已红冲」处理）。
    pub reversed_batch: Option<BatchView>,
    /// 是否已建活跃批次。
    pub cost_ready: bool,
    /// 状态筛选回显。
    pub state: String,
    /// 关键字回显。
    pub keyword: Option<String>,
    /// 余额公式。
    pub formula_roll: String,
    /// 结转口径。
    pub formula_carry: String,
    /// 合计（整张表，不受分页影响）。
    pub totals: BalanceTotalsView,
    /// 数据页。
    pub list: Vec<BalanceRow>,
    /// 命中总行数。
    pub total: u64,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 可选筛档（前端下拉）。
    pub state_options: Vec<OptionView>,
    /// 页脚说明。
    pub note: String,
}

// ==================== 装配 ====================

/// 行 → 出参（纯函数：所有数字都已由 [`balance_of`] / [`carry_of`] 算好）。
pub fn row_view(
    head: &RowHead,
    object_status: &str,
    facts: &AllocFacts,
    calc: &AllocCalc,
    roll: &BalanceCalc,
    carry: &CarryOutcome,
) -> BalanceRow {
    BalanceRow {
        object_id: head.object_id.clone(),
        object_no: head.object_no.clone(),
        object_type: head.object_type.clone(),
        object_status: object_status.to_string(),
        source_doc_no: head.source_doc_no.clone(),
        lot_no: head.lot_no.clone(),
        product_id: head.product_id.clone(),
        product_code: head.product_code.clone(),
        product_name: head.product_name.clone(),
        workshop_dept_id: head.workshop_dept_id.clone(),
        workshop_dept_name: head.workshop_dept_name.clone(),
        plan_qty: head.plan_qty.clone(),
        cumulative_completed_qty: qty_text(facts.cumulative_completed_qty),
        completed_qty: qty_text(calc.completed_qty),
        beg_qty: qty_text(roll.beg_qty),
        beg_period: facts.prior.as_ref().map(|prior| prior.period_code.clone()),
        wip_qty: qty_text(calc.wip_qty),
        completion_pct: pct_text(calc.completion_pct),
        beg_total: roll.beg_total.map(qty_text),
        cur_total: roll.cur_total.map(qty_text),
        finished_total: roll.finished_total.map(qty_text),
        wip_total: roll.wip_total.map(qty_text),
        elements: roll
            .elements
            .iter()
            .map(|item| ElementRollView {
                element: item.element.map(|value| value.as_str().to_string()).unwrap_or_default(),
                label: item.element.map(|value| value.label().to_string()).unwrap_or_default(),
                beg: item.beg.map(qty_text),
                cur: item.cur.map(qty_text),
                finished: item.finished.map(qty_text),
                wip: item.wip.map(qty_text),
            })
            .collect(),
        elements_available: roll.elements_available(),
        source: roll.source.as_str().to_string(),
        source_label: roll.source.label().to_string(),
        ledger: facts.stored.as_ref().map(|stored| LedgerView {
            status: stored.status.clone(),
            wip_end_qty: qty_text(stored.wip_end_qty),
            wip_end_cost: qty_text(stored.wip_end_cost),
            agrees: !calc.recalc_required,
            note: if !stored.settled() {
                "本期计算单还没结算（CALC）：账面还没有这笔余额，表里的余额是试算值".to_string()
            } else if calc.recalc_required {
                format!(
                    "试算与账面（已结算）不一致：{}。分要素一律不给 —— 按试算拆出来加不出账面余额；\
                     修法是红冲本期后重算（已结算的单不能直接重算）",
                    alloc::drift_hint(calc, stored)
                )
            } else {
                "试算与账面（已结算）一致：分要素三项加起来 = 账面期末在制".to_string()
            },
        }),
        carry: CarryView {
            state: carry.state.as_str().to_string(),
            label: carry.state.label().to_string(),
            needs_attention: carry.state.needs_attention(),
            to_period: carry.to_period.clone(),
            to_beg_qty: carry.to_beg_qty.map(qty_text),
            to_beg_cost: carry.to_beg_cost.map(qty_text),
            diff_qty: carry.diff_qty.map(qty_text),
            diff_cost: carry.diff_cost.map(qty_text),
            hint: carry.hint.clone(),
        },
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

/// 合计 → 出参。
///
/// `unbooked` / `closed_wip` / `mismatch_abs` 三个「要人看」的数由调用方按整表算好传进来
/// （它们需要逐行的结转结果，[`BalanceTotals`] 只留计数与金额）。
pub fn totals_view(totals: &BalanceTotals, extra: &TotalsExtra) -> BalanceTotalsView {
    let mut note = format!(
        "合计是整张表的（不受分页影响）；金额合计跳过 {} 行算不出来的行（按 0 加进去会让合计看起来像丢了钱），\
         数量合计含全部行。",
        totals.blocked_count
    );
    if totals.unsettled_count > 0 {
        note.push_str(&format!(
            "其中 {} 行**还没入账**（余额 {} ）：本期没结算，下一期取不到这笔钱 —— 优先处理。",
            totals.unsettled_count,
            cost_period_item_dto::fmt(extra.unbooked_total, MONEY_SCALE)
        ));
    }
    if totals.closed_wip_count > 0 {
        note.push_str(&format!(
            "另有 {} 行关批仍挂余额（{}）：不会再有完工，必须改回 OPEN 或按呆滞处理。",
            totals.closed_wip_count,
            cost_period_item_dto::fmt(extra.closed_wip_total, MONEY_SCALE)
        ));
    }
    if totals.mismatch_count > 0 {
        note.push_str(&format!(
            "还有 {} 行结转不齐（差额绝对值合计 {}）：下期期初与本期账面期末对不上。",
            totals.mismatch_count,
            cost_period_item_dto::fmt(extra.mismatch_abs_total, MONEY_SCALE)
        ));
    }
    if totals.element_row_count < totals.wip_row_count {
        note.push_str(&format!(
            "分要素合计只含 {} 行（其余行分要素为空：算不出来或已结算账面与试算漂移），\
             所以「料 + 工 + 费」可能小于余额合计。",
            totals.element_row_count
        ));
    }

    BalanceTotalsView {
        row_count: totals.row_count,
        wip_row_count: totals.wip_row_count,
        carried_count: totals.carried_count,
        pending_count: totals.pending_count,
        mismatch_count: totals.mismatch_count,
        unsettled_count: totals.unsettled_count,
        reversed_count: totals.reversed_count,
        closed_wip_count: totals.closed_wip_count,
        cleared_count: totals.cleared_count,
        blocked_count: totals.blocked_count,
        beg_qty: qty_text(totals.beg_qty),
        wip_qty: qty_text(totals.wip_qty),
        beg_total: qty_text(totals.beg_total),
        cur_total: qty_text(totals.cur_total),
        finished_total: qty_text(totals.finished_total),
        wip_total: qty_text(totals.wip_total),
        wip_material: qty_text(totals.wip_material),
        wip_labor: qty_text(totals.wip_labor),
        wip_overhead: qty_text(totals.wip_overhead),
        unbooked_total: qty_text(extra.unbooked_total),
        closed_wip_total: qty_text(extra.closed_wip_total),
        mismatch_abs_total: qty_text(extra.mismatch_abs_total),
        element_row_count: totals.element_row_count,
        note,
    }
}

/// 合计里需要逐行结转结果才能算的数（`BalanceTotals` 只留计数）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TotalsExtra {
    /// 未入账余额合计。
    pub unbooked_total: Decimal,
    /// 关批挂账余额合计。
    pub closed_wip_total: Decimal,
    /// 结转不齐的金额差绝对值合计。
    pub mismatch_abs_total: Decimal,
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

/// 金额 → 报文里的短写法（去掉尾随 0）。
pub fn amount_text(value: Decimal) -> String {
    money_text(value)
}

/// 500 + `INTERNAL_ERROR`：库里的 id / 数值解析不出来，说明代码与库的数据契约出了问题
/// （不是用户输入问题，不能报 400 让人去改参数）。
pub fn internal_error(message: &str) -> ApiError {
    ApiError::new(500, error_code::INTERNAL_ERROR, message.to_string())
}

/// 400 + `PARAM_INVALID`（与 M5-22 同一个出口，文案口径一致）。
pub fn bad_request(message: &str) -> ApiError {
    alloc::bad_request(message)
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::cost::cost_wip_alloc_dto::{AllocFacts, PriorFacts, StoredItem, alloc_of};
    use crate::business::erp::cost::cost_wip_equiv_dto::EquivIssue;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数量 / 金额字面量")
    }

    /// 设计算例 A：完工 800 / 在制 200 / 完工率 50% / 料 19000 / 工 4000 / 费 4000。
    /// calc 给出：完工 22311.1104 / 在制 4688.8896（见 ARCH-9 §4.6.2）。
    fn facts() -> AllocFacts {
        AllocFacts {
            object_no: "CB-T-001".to_string(),
            material_input_mode: cost_period_item_dto::INPUT_MODE_ONE_TIME.to_string(),
            cumulative_completed_qty: dec("800"),
            prior_completed_qty: Decimal::ZERO,
            wip_qty: dec("200"),
            completion_pct: dec("50"),
            mat_cur: dec("19000"),
            labor_cur: dec("4000"),
            oh_cur: dec("4000"),
            prior: None,
            stored: None,
        }
    }

    fn with_stored(mut facts: AllocFacts, status: &str, calc: &AllocCalc) -> AllocFacts {
        facts.stored = Some(StoredItem {
            status: status.to_string(),
            wip_beg_cost: calc.beg_total.unwrap_or(Decimal::ZERO),
            completed_qty: calc.completed_qty,
            wip_end_qty: calc.wip_qty,
            completion_pct: calc.completion_pct,
            equiv_material: calc.equiv_material.unwrap_or(Decimal::ZERO),
            equiv_conversion: calc.equiv_conversion.unwrap_or(Decimal::ZERO),
            unit_material: calc.element_of(Element::Material).rate.unwrap_or(Decimal::ZERO),
            unit_labor: calc.element_of(Element::Labor).rate.unwrap_or(Decimal::ZERO),
            unit_overhead: calc.element_of(Element::Overhead).rate.unwrap_or(Decimal::ZERO),
            finished_cost: calc.finished_total.unwrap_or(Decimal::ZERO),
            wip_end_cost: calc.wip_total.unwrap_or(Decimal::ZERO),
        });
        facts
    }

    fn next(period: &str, qty: &str, cost: &str) -> NextFacts {
        NextFacts {
            period_code: period.to_string(),
            wip_beg_qty: dec(qty),
            wip_beg_cost: dec(cost),
            item_status: "CALC".to_string(),
            period_status: "DRAFT".to_string(),
        }
    }

    fn batch(status: &str) -> BatchRef {
        BatchRef {
            id: "batch-1".to_string(),
            batch_no: "CS-202611-0001".to_string(),
            status: status.to_string(),
            reverse_reason: Some("红冲测试".to_string()),
        }
    }

    // ---------- 权限 / 状态 ----------

    #[test]
    fn permission_reuses_cost_object_list() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(PERMISSIONS, [PERM_LIST]);
    }

    #[test]
    fn carry_states_have_stable_codes_and_labels() {
        for (state, code, label) in [
            (CarryState::Carried, "carried", "已结转"),
            (CarryState::Pending, "pending", "待结转"),
            (CarryState::Mismatch, "mismatch", "结转不齐"),
            (CarryState::Unsettled, "unsettled", "未入账"),
            (CarryState::Reversed, "reversed", "已红冲"),
            (CarryState::ClosedWip, "closed_wip", "关批挂账"),
            (CarryState::Cleared, "cleared", "无余额"),
            (CarryState::Blocked, "blocked", "算不出"),
        ] {
            assert_eq!(state.as_str(), code);
            assert_eq!(state.label(), label);
            assert!(label.chars().count() <= 4, "中文名要短：{label}");
        }
    }

    #[test]
    fn attention_flags_only_the_actionable_states() {
        assert!(!CarryState::Carried.needs_attention());
        assert!(!CarryState::Pending.needs_attention());
        assert!(!CarryState::Cleared.needs_attention());
        for state in [
            CarryState::Mismatch,
            CarryState::Unsettled,
            CarryState::Reversed,
            CarryState::ClosedWip,
            CarryState::Blocked,
        ] {
            assert!(state.needs_attention(), "{:?} 必须标红", state);
        }
    }

    // ---------- 余额滚动 ----------

    #[test]
    fn roll_forward_carries_the_three_elements() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        // 算例 A：成品 22311.1104 / 在制 4688.8896
        assert_eq!(roll.finished_total, Some(dec("22311.1104")));
        assert_eq!(roll.wip_total, Some(dec("4688.8896")));
        assert_eq!(roll.source, BalanceSource::Live, "没结算就是试算");
        assert!(roll.elements_available());
        // 三要素的期末在制加起来 = 余额（一分不差）
        let sum: Decimal = roll.elements.iter().filter_map(|item| item.wip).sum();
        assert_eq!(sum, roll.wip_total.expect("余额"));
        // 期初 0 + 投入 27000 = 完工 + 在制
        assert_eq!(
            roll.beg_total.expect("期初") + roll.cur_total.expect("投入"),
            roll.finished_total.expect("完工") + roll.wip_total.expect("在制")
        );
    }

    #[test]
    fn settled_rows_take_the_book_values() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        assert_eq!(roll.source, BalanceSource::Settled);
        assert_eq!(roll.book.expect("账面").wip_end_cost, dec("4688.8896"));
        assert!(!roll.ledger_drift, "一致时不该漂移");
        assert!(roll.elements_available());
    }

    #[test]
    fn settled_but_drifted_hides_the_elements() {
        let facts = facts();
        let calc = alloc_of(&facts);
        // 模拟「算完之后又改了归集」：账面上多 100
        let mut facts = with_stored(facts, "SETTLED", &calc);
        if let Some(stored) = facts.stored.as_mut() {
            stored.wip_end_cost = dec("4788.8896");
        }
        let live = alloc_of(&facts);
        let roll = balance_of(&facts, &live);
        assert!(live.recalc_required, "改过数必须判漂移");
        assert!(roll.ledger_drift);
        assert_eq!(roll.wip_total, Some(dec("4788.8896")), "余额取账面");
        assert!(!roll.elements_available(), "漂移时分要素不给");
        assert!(roll.elements.iter().all(|item| item.wip.is_none()));
    }

    #[test]
    fn blocked_rows_have_no_money_at_all() {
        let mut facts = facts();
        facts.completion_pct = Decimal::ZERO; // 在制 > 0 但完工率 0 ⇒ 算不出来
        let calc = alloc_of(&facts);
        assert_eq!(calc.state, EquivState::Blocked);
        let roll = balance_of(&facts, &calc);
        assert_eq!(roll.wip_total, None);
        assert_eq!(roll.cur_total, None);
        assert!(roll.elements.iter().all(|item| item.cur.is_none()));
        assert!(!roll.elements_available());
    }

    #[test]
    fn prior_wip_becomes_the_beginning_quantity() {
        let mut facts = facts();
        facts.prior = Some(PriorFacts {
            period_code: "2026-10".to_string(),
            wip_end_qty: dec("120"),
            wip_end_cost: dec("3000"),
            completion_pct: dec("50"),
            unit_material: dec("10"),
            unit_labor: dec("5"),
            unit_overhead: dec("5"),
        });
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        assert_eq!(roll.beg_qty, dec("120"));
        // 期初按上期单位成本还原：材料 120×10 = 1200，人工 120×50%×5 = 300，费 300，合计 1800?
        // 还原口径由 calc 的 beg_cost_of 定（材料 1200 + 人工 300 + 费 = 总额 3000 − 1200 − 300 = 1500）
        assert_eq!(roll.beg_total, calc.beg_total.map(cost_period_item_dto::money));
    }

    // ---------- 结转核对 ----------

    #[test]
    fn unsettled_balance_is_flagged_as_not_booked() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "OPEN", Some(&batch("DRAFT")), None);
        assert_eq!(carry.state, CarryState::Unsettled);
        assert!(carry.hint.contains("还没入账"));
        assert!(carry.to_period.is_some(), "要给出下一期是哪个期间");
    }

    #[test]
    fn settled_without_next_period_is_pending() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "OPEN", Some(&batch("SETTLED")), None);
        assert_eq!(carry.state, CarryState::Pending);
        assert_eq!(carry.to_period.as_deref(), Some("2026-12"), "建议下一期");
        assert!(carry.hint.contains("自动"));
    }

    #[test]
    fn settled_and_taken_forward_is_carried() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        let next = next("2026-12", "200", "4688.8896");
        let carry = carry_of(
            "2026-11",
            &calc,
            roll.book,
            "OPEN",
            Some(&batch("SETTLED")),
            Some(&next),
        );
        assert_eq!(carry.state, CarryState::Carried);
        assert_eq!(carry.diff_cost, Some(Decimal::ZERO));
        assert_eq!(carry.diff_qty, Some(Decimal::ZERO));
        assert!(carry.hint.contains("一分不差"));
    }

    #[test]
    fn one_cent_difference_is_a_mismatch() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        let next = next("2026-12", "200", "4688.8897");
        let carry = carry_of(
            "2026-11",
            &calc,
            roll.book,
            "OPEN",
            Some(&batch("SETTLED")),
            Some(&next),
        );
        assert_eq!(carry.state, CarryState::Mismatch);
        assert_eq!(carry.diff_cost, Some(dec("0.0001")));
        assert!(carry.hint.contains("差"));
        assert!(carry.hint.contains("红冲"));
    }

    #[test]
    fn quantity_difference_alone_is_a_mismatch() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        let next = next("2026-12", "201", "4688.8896");
        let carry = carry_of(
            "2026-11",
            &calc,
            roll.book,
            "OPEN",
            Some(&batch("SETTLED")),
            Some(&next),
        );
        assert_eq!(carry.state, CarryState::Mismatch, "数量对不上也算结转不齐");
        assert_eq!(carry.diff_qty, Some(dec("1")));
    }

    #[test]
    fn reversed_batch_wins_over_everything_but_blocked() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "OPEN", Some(&batch("REVERSED")), None);
        assert_eq!(carry.state, CarryState::Reversed);
        assert!(carry.hint.contains("红冲测试"), "要把红冲原因写出来");
    }

    #[test]
    fn closed_object_with_wip_is_flagged() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "CLOSED", Some(&batch("SETTLED")), None);
        assert_eq!(carry.state, CarryState::ClosedWip);
        assert!(carry.hint.contains("关批"));
        assert!(carry.hint.contains("OPEN"), "要给怎么修");
    }

    #[test]
    fn zero_balance_is_cleared() {
        let mut facts = facts();
        facts.wip_qty = Decimal::ZERO; // 全部完工
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        assert_eq!(roll.wip_total, Some(Decimal::ZERO));
        let facts = with_stored(facts, "SETTLED", &calc);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "CLOSED", Some(&batch("SETTLED")), None);
        assert_eq!(carry.state, CarryState::Cleared, "关批但无余额不该报挂账");
    }

    #[test]
    fn blocked_row_never_claims_to_be_carried() {
        let mut facts = facts();
        facts.completion_pct = Decimal::ZERO;
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of(
            "2026-11",
            &calc,
            roll.book,
            "OPEN",
            Some(&batch("SETTLED")),
            Some(&next("2026-12", "200", "4688.8896")),
        );
        assert_eq!(carry.state, CarryState::Blocked);
        assert!(carry.hint.contains("怎么修") || carry.hint.contains("修"));
    }

    // ---------- 筛选 ----------

    #[test]
    fn state_filter_parses_and_rejects_typos() {
        assert_eq!(BalanceStateFilter::parse(None).expect("缺省"), BalanceStateFilter::Wip);
        assert_eq!(
            BalanceStateFilter::parse(Some("  ")).expect("空白"),
            BalanceStateFilter::Wip
        );
        assert_eq!(
            BalanceStateFilter::parse(Some("ALL")).expect("大写"),
            BalanceStateFilter::All
        );
        assert_eq!(
            BalanceStateFilter::parse(Some("closed_wip")).expect("下划线"),
            BalanceStateFilter::ClosedWip
        );
        let error = BalanceStateFilter::parse(Some("carry")).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("carry"), "{}", error.msg);
    }

    #[test]
    fn wip_filter_drops_only_cleared_rows() {
        let cleared = CarryOutcome {
            state: CarryState::Cleared,
            to_period: None,
            to_beg_qty: None,
            to_beg_cost: None,
            diff_qty: None,
            diff_cost: None,
            hint: String::new(),
        };
        let unsettled = CarryOutcome {
            state: CarryState::Unsettled,
            ..cleared.clone()
        };
        assert!(!BalanceStateFilter::Wip.matches(&cleared));
        assert!(BalanceStateFilter::Wip.matches(&unsettled));
        assert!(BalanceStateFilter::All.matches(&cleared));
        assert!(!BalanceStateFilter::Unsettled.matches(&cleared));
        assert!(BalanceStateFilter::Unsettled.matches(&unsettled));
    }

    #[test]
    fn options_cover_every_filter_once() {
        let options = state_options();
        assert_eq!(options.len(), STATE_FILTERS.len());
        let mut seen: Vec<String> = options.iter().map(|item| item.value.clone()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), options.len(), "选项值不许重复");
        assert_eq!(options[0].value, "wip", "默认档排最前");
    }

    // ---------- 合计 ----------

    #[test]
    fn totals_skip_blocked_money_but_count_them() {
        let mut totals = BalanceTotals::default();
        let facts = facts();
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "OPEN", None, None);
        totals.add(&calc, &roll, &carry);
        assert_eq!(totals.wip_total, dec("4688.8896"));
        assert_eq!(totals.unsettled_count, 1);
        assert_eq!(totals.wip_row_count, 1);

        let mut broken = facts.clone();
        broken.completion_pct = Decimal::ZERO;
        let blocked_calc = alloc_of(&broken);
        let blocked_roll = balance_of(&broken, &blocked_calc);
        let blocked_carry = carry_of("2026-11", &blocked_calc, blocked_roll.book, "OPEN", None, None);
        totals.add(&blocked_calc, &blocked_roll, &blocked_carry);
        assert_eq!(totals.blocked_count, 1);
        assert_eq!(totals.row_count, 2);
        assert_eq!(totals.wip_total, dec("4688.8896"), "算不出来的行不进金额合计");
        assert_eq!(totals.wip_qty, dec("400"), "数量照进（200 + 200）");
        assert_eq!(totals.element_row_count, 1);
    }

    #[test]
    fn totals_keep_the_identity_between_elements_and_balance() {
        let mut totals = BalanceTotals::default();
        let facts = facts();
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "OPEN", None, None);
        totals.add(&calc, &roll, &carry);
        assert_eq!(
            totals.wip_material + totals.wip_labor + totals.wip_overhead,
            totals.wip_total,
            "料 + 工 + 费 = 余额合计"
        );
        assert_eq!(
            totals.beg_total + totals.cur_total,
            totals.finished_total + totals.wip_total,
            "期初 + 投入 = 完工 + 期末（整表守恒）"
        );
    }

    #[test]
    fn totals_extra_collects_attention_amounts() {
        let extra = TotalsExtra {
            unbooked_total: dec("100.5"),
            closed_wip_total: dec("20"),
            mismatch_abs_total: dec("0.01"),
        };
        let totals = BalanceTotals {
            row_count: 1,
            unsettled_count: 1,
            ..Default::default()
        };
        let view = totals_view(&totals, &extra);
        assert_eq!(view.unbooked_total, "100.5000");
        assert!(view.note.contains("还没入账"));
        assert!(view.note.contains("100.5"));
    }

    // ---------- 出参 ----------

    #[test]
    fn row_view_carries_the_numbers_without_recomputing() {
        let facts = facts();
        let calc = alloc_of(&facts);
        let roll = balance_of(&facts, &calc);
        let carry = carry_of("2026-11", &calc, roll.book, "OPEN", None, None);
        let head = RowHead {
            object_id: "obj".to_string(),
            object_no: "CB-T-001".to_string(),
            object_type: "BATCH".to_string(),
            source_doc_no: None,
            lot_no: None,
            product_id: "prod".to_string(),
            product_code: None,
            product_name: None,
            workshop_dept_id: "dept".to_string(),
            workshop_dept_name: None,
            plan_qty: "1000.0000".to_string(),
        };
        let view = row_view(&head, "OPEN", &facts, &calc, &roll, &carry);
        assert_eq!(view.wip_total.as_deref(), Some("4688.8896"));
        assert_eq!(view.beg_total.as_deref(), Some("0.0000"));
        assert_eq!(view.cur_total.as_deref(), Some("27000.0000"));
        assert_eq!(view.finished_total.as_deref(), Some("22311.1104"));
        assert_eq!(view.source, "live");
        assert_eq!(view.carry.state, "unsettled");
        assert!(view.carry.needs_attention);
        assert_eq!(view.elements.len(), 3);
        assert!(view.elements_available);
        assert!(view.ledger.is_none(), "没结算就没有账面快照");
        let sum: Decimal = view
            .elements
            .iter()
            .filter_map(|item| item.wip.as_deref())
            .map(dec)
            .sum();
        assert_eq!(sum, dec("4688.8896"));
    }

    #[test]
    fn amount_text_drops_trailing_zeros() {
        assert_eq!(amount_text(dec("1000.0000")), "1000");
        assert_eq!(amount_text(dec("4688.8896")), "4688.8896");
    }

    #[test]
    fn issue_helper_is_reusable() {
        // `EquivIssue` 由 M5-20 定义，本卡只透传（不另造问题码）
        let issue = EquivIssue {
            code: "WIP_PCT_MISSING",
            message: "缺完工率".to_string(),
            fix: "填完工率".to_string(),
        };
        assert_eq!(issue.code, "WIP_PCT_MISSING");
    }
}

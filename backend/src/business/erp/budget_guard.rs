//! 预算守卫（ARCH-6 第三章）：**两道闸的纯函数判定** + 占用事实的写入 / 释放。
//!
//! # 口径（逐条对应 ARCH-6 §3.1）
//!
//! ```text
//! 年可用 = 年度额度(槽) − Σ有效 usage(槽，本年度)        -- OCCUPIED + CONSUMED
//! 月可用 = 月度额度(槽，期间 P) − Σ有效 usage(槽，期间 P)
//! 超支额 = max(0, M − 年可用, M − 月可用)
//! over   = (M > 年可用) OR (M > 月可用)
//! ```
//!
//! * **占用中 / 已执行** 都算「已用」：钱已经承诺出去了（`usage.status IN ('OCCUPIED','CONSUMED')`）；
//!   `RELEASED` 不算（单据被驳回 / 撤回 / 取消，额度还回来了）。
//! * **可用余额绝不缓存**（本章红线 2）：所有额度都是现算，库里没有 `used_amount` 这类列，
//!   `biz_budget_usage.budget_id` 只是「当时命中的版本」审计快照，**不参与计算**（D5）。
//! * **`M = 可用` 不算超**（边界用例，见单测 `boundary_equal_is_not_over`）。
//!
//! # `effective_from` 参与判定（FIX-2 定稿：选项 A）
//!
//! 预算审批通过时写 `effective_from = 审批通过次月 1 日`（[`crate::business::erp::budget_bridge::effective_from_of`]）。
//! 控制 / 占用的判定条件是 **`effective_from IS NULL OR effective_from <= 期间月末`**
//! （[`period_end_of`]）：
//!
//! ```text
//! 期间 2026-11（月末 11-30），预算 effective_from = 2026-12-01
//!   → 12-01 > 11-30 → 该版本对 11 月「不生效」：额度 0、槽级策略继承全局
//! 期间 2026-12（月末 12-31）
//!   → 12-01 <= 12-31 → 生效
//! ```
//!
//! 为什么按「期间月末」而不是「单据日期」：月度闸本身就是月粒度（月额度整月有效），
//! 月中生效会出现「半个月的额度按整月校验」；按月末判等于「这一整个月是否已经生效」。
//! `NULL` 放行是为了兼容选项 A 之前落库的 ACTIVE 行（那些行没有生效日，本来就该算生效）。
//!
//! 同口径必须一起改，否则报表和控制分叉（ARCH-6 §3.8.1）：[`active_control_mode`]、
//! [`slot_snapshot`]、`budget_model::report`（报表标注 `effective` / `effective_from`）。
//!
//! **报表时点不同（E2E06-FIX-2）**：报表是「按年度看」的视图 —— 问的是「这份预算在这一年
//! 算不算数」，所以 `effective` 用该年度**期末**判定（[`effective_in_year`] /
//! [`fiscal_year_end`]），不是「今天」。控制侧仍按单据期间月末逐月判。
//! 月度口径（报表主列的「本月」）由 [`MonthGate`] 统一提供，**与提交侧同一个公式**。
//!
//! # 「预算没生效」只提示、不拦（[`pending_budgets`] / [`pending_warnings`]）
//!
//! DRAFT / SUBMITTED / ACTIVE-未到生效日 的预算**不参与控制**（口径不变），
//! 但提交与详情会带回 `budget_warnings[]`（`BUDGET_NOT_EFFECTIVE`），前端标黄提示 ——
//! 否则「预算编了但没送审 → 超支直接放行」是个静默黑洞。
//!
//! # 超预算怎么办：策略 B（标记 + 强制审批），409 只在显式 BLOCK
//!
//! | mode | 行为 |
//! |---|---|
//! | `OFF` | 不校验、不占用（只记 INFO 日志） |
//! | `WARN`（**出厂默认**） | 照常提交，`usage.over_budget=1` + `over_amount` 冻结当时的超支额，审批单 `form_data.over_budget=true` |
//! | `BLOCK` | 提交直接 409（`ERP_BUDGET_EXCEEDED`），调用方回滚事务 —— 连 usage 都不留 |
//!
//! 为什么默认 WARN：忘编预算 / 编漏一个科目不该让业务停摆；标记 + 强制审批让人来兜
//! （ARCH-6 D3）。BLOCK 必须由客户显式打开（槽级 `biz_budget.control_mode` 或全局
//! `sys_config erp.budget.control_mode`）。
//!
//! # 「未编制预算」≠「超预算」（FIX-5）
//!
//! 该槽根本没有本期生效的 ACTIVE 预算时（`missing_budget = true`）：
//!
//! ```text
//! over        = false        -- 没编预算不代表花超了
//! over_amount = None         -- 落库 NULL，不是「全额超支」
//! ```
//!
//! 为什么改：旧实现里 `missing_budget=true` 与 `over=true` 同时成立，于是「没编预算」的单据
//! 全部 `over_budget=1 / over_amount=全额`，列表页一片「超预算」，业务分不清「真超支」和
//! 「压根没编预算」。现在两者是两个互斥的事实：**真超支**（有预算、花过了）才置 `over`，
//! **未编制**只置 `missing_budget`（列表页标黄）。
//!
//! 拦截口径（`blocks()`）**不变**：`BLOCK` 策略下「超支」与「未编制预算」都会被拦，
//! 只是两种理由分开说（`over` / `missing_budget`）—— 强管控的语义是「没有额度就不许花」。
//! `WARN`（出厂默认）/ `OFF` 下未编预算仍然照常提交，只打标记。
//!
//! # 409 报文里说哪条策略（FIX-5）
//!
//! 阻断可能来自**槽级** `control_mode`（`biz_budget.control_mode`）也可能来自**全局**
//! `sys_config`，两者可以不同（典型：全局 WARN + 槽级 BLOCK）。报文必须用
//! **触发阻断那条 check 自己的** `verdict.mode` + [`ModeSource`]（「槽级策略 BLOCK」
//! / 「全局策略 BLOCK」），而不是 `global.label()` —— 否则会出现「策略 标记并强制审批」
//! 却把单子拦下来的自相矛盾。
//!
//! # 并发（不锁，靠唯一键 + 快照，ARCH-6 §3.6）
//!
//! 两张单同时提交都超预算 → 两张都带标记进审批（可接受：人都能看见）；
//! 同一张单双击 → 第二次撞 `uk_biz_budget_usage_source`，调用方把 23505 当**幂等成功**；
//! 撤回 / 驳回后重新提交 → 同键已有 `RELEASED` 行，[`occupy`] 先把它**拉回** `OCCUPIED`
//! 而不是再插一条（唯一键的谓词只有 `deleted_at IS NULL`，`RELEASED` 行也在里面，
//! 纯 INSERT 会撞键 → 整个提交事务回滚）；
//! `BLOCK` 模式若真要做到「一分不超」，才需要 `pg_advisory_xact_lock(hashtext(dept||account||year))`
//! —— 本设计**预留但不实现**（§3.6.2 已论证代价）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 全局控制策略的 `sys_config` 键。
pub const CONFIG_CONTROL_MODE: &str = "erp.budget.control_mode";
/// 采购提交是否做预算校验的 `sys_config` 键。
pub const CONFIG_ENFORCE_ON_PO: &str = "erp.budget.enforce_on_po";

/// 占用来源：费用报销单（FIN-3 接线）。
pub const SOURCE_EXPENSE: &str = "EXPENSE_REPORT";
/// 占用来源：采购订单（本卡接线）。
pub const SOURCE_PURCHASE: &str = "PURCHASE_ORDER";

/// 提示码：该槽存在预算但**尚未生效**（DRAFT / SUBMITTED / 未到 `effective_from`）。
///
/// 这是**提示**不是错误：控制口径不变（DRAFT 预算不参与判定，见 [`active_control_mode`]），
/// 只是让用户知道「钱没被预算管着」。
pub const WARN_BUDGET_NOT_EFFECTIVE: &str = "BUDGET_NOT_EFFECTIVE";

/// usage 状态：占用中。
pub const USAGE_OCCUPIED: &str = "OCCUPIED";
/// usage 状态：已执行（已入账 / 已收货）。
pub const USAGE_CONSUMED: &str = "CONSUMED";
/// usage 状态：已释放（驳回 / 撤回 / 取消 / 作废）。
pub const USAGE_RELEASED: &str = "RELEASED";

/// 预算控制策略（`biz_budget.control_mode` / `sys_config` 的取值域）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlMode {
    /// 不校验、不占用。
    Off,
    /// 标记超预算 + 强制审批（出厂默认）。
    Warn,
    /// 直接阻断（409）。
    Block,
}

impl ControlMode {
    /// 全部取值（顺序 = 严格程度递增）。
    pub const ALL: [ControlMode; 3] = [ControlMode::Off, ControlMode::Warn, ControlMode::Block];

    /// 落库值。
    pub const fn as_str(self) -> &'static str {
        match self {
            ControlMode::Off => "OFF",
            ControlMode::Warn => "WARN",
            ControlMode::Block => "BLOCK",
        }
    }

    /// 中文名。
    pub const fn label(self) -> &'static str {
        match self {
            ControlMode::Off => "不校验",
            ControlMode::Warn => "标记并强制审批",
            ControlMode::Block => "阻断提交",
        }
    }

    /// 字符串 → 策略。**未知一律 `None`**（调用方报错或按默认处理，不许静默猜测）。
    pub fn parse(raw: &str) -> Option<ControlMode> {
        ControlMode::ALL
            .into_iter()
            .find(|mode| mode.as_str() == raw.trim().to_ascii_uppercase())
    }

    /// 槽级 `control_mode` 为空时继承全局（ARCH-6 §3.2.1 的 `NULL` 语义）。
    pub fn resolve(slot_mode: Option<&str>, global_mode: ControlMode) -> ControlMode {
        slot_mode.and_then(ControlMode::parse).unwrap_or(global_mode)
    }

    /// 是否「真的校验」（`OFF` 不校验也不占用）。
    pub const fn enforces(self) -> bool {
        !matches!(self, ControlMode::Off)
    }

    /// 严格程度（`OFF` < `WARN` < `BLOCK`）：多槽汇总时取最严的那条记账。
    pub const fn strictness(self) -> u8 {
        match self {
            ControlMode::Off => 0,
            ControlMode::Warn => 1,
            ControlMode::Block => 2,
        }
    }
}

/// 一条预算判定的策略**来源**（FIX-5：报文里必须说清是「槽级」还是「全局」）。
///
/// 槽级 `biz_budget.control_mode` 会覆盖全局 `sys_config`，所以同一个槽的 `mode` 可能
/// 与全局不同（典型：全局 WARN、槽级 BLOCK）。用全局标签解释槽级阻断 = 报文自相矛盾。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeSource {
    /// 槽级没有配置，继承 `sys_config erp.budget.control_mode`。
    Global,
    /// 槽级 `biz_budget.control_mode` 显式指定。
    Slot,
}

impl ModeSource {
    /// 报文前缀（「全局策略」/「槽级策略」）。
    pub const fn label(self) -> &'static str {
        match self {
            ModeSource::Global => "全局策略",
            ModeSource::Slot => "槽级策略",
        }
    }
}

/// 允许的会计年度窗口（与库内 `ck_biz_budget_year` CHECK 逐字一致：2000..=2100）。
pub const FISCAL_YEAR_MIN: i16 = 2000;
/// 允许的会计年度窗口上界（见 [`FISCAL_YEAR_MIN`]）。
pub const FISCAL_YEAR_MAX: i16 = 2100;

/// 一个预算「槽」= 部门 × 预算科目 × 年度（D5：usage 绑槽不绑版本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// 部门 id（采购侧取提交人部门快照）。
    pub dept_id: Uuid,
    /// 预算科目 id（末级）。
    pub account_id: Uuid,
    /// 会计年度。
    pub fiscal_year: i16,
    /// 发生期间 `yyyy-MM`（含月份，月度闸用）。
    pub period_code: String,
}

impl Slot {
    /// 构造（`fiscal_year` 从期间码推导，避免两处手填不一致）。
    pub fn new(dept_id: Uuid, account_id: Uuid, period_code: &str) -> Option<Self> {
        let year: i16 = period_code.get(0..4)?.parse().ok()?;
        let month: u32 = period_code.get(5..7)?.parse().ok()?;
        if !(FISCAL_YEAR_MIN..=FISCAL_YEAR_MAX).contains(&year) || !(1..=12).contains(&month) {
            return None;
        }
        Some(Self {
            dept_id,
            account_id,
            fiscal_year: year,
            period_code: period_code.to_string(),
        })
    }

    /// 槽的年度键（日志 / 文案用）。
    pub fn year_text(&self) -> String {
        format!("{} 年", self.fiscal_year)
    }

    /// 期间月末日期（`effective_from` 判定用；期间码非法 → `None`）。
    pub fn period_end(&self) -> Option<NaiveDate> {
        period_end_of(&self.period_code)
    }

    /// 期间末月的 `yyyy-MM-dd`（拼 SQL 参数用；非法期间退化成 `None`）。
    pub fn period_end_text(&self) -> Option<String> {
        self.period_end().map(|date| date.to_string())
    }
}

/// 期间码（`yyyy-MM`）的**月末日期**。
///
/// 口径（ARCH-6 §3.2.2，FIX-2 定稿的选项 A）：`effective_from <= 期间月末` 才参与该期间的
/// 控制与占用判定 —— 也就是「按月末那一刻是否已生效」判，与「审批通过次月 1 日生效」
/// 的业务语义一致：11 月的单据不会受 12-01 才生效的额度约束。
pub fn period_end_of(period_code: &str) -> Option<NaiveDate> {
    let year: i32 = period_code.get(0..4)?.parse().ok()?;
    let month: u32 = period_code.get(5..7)?.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)?.pred_opt()
}

/// 会计年度的**期末日期**（`fiscal_year` 的 12-31）。
///
/// 预算按自然年编制（`validate_periods`：12 行分解、期间码 `{fiscal_year}-{月}`），
/// 所以「这一年的期末」= 该年 12-31。
///
/// 年份窗口与库内 CHECK（`ck_biz_budget_year: fiscal_year BETWEEN 2000 AND 2100`）和
/// [`Slot::new`] 保持一致：越界一律 `None`（调用方按「不参与判定」处理，绝不 panic）。
///
/// 用途：**执行报表的 `effective` 时点**。报表是按年度看的视图，问的是「这份预算在
/// 这一年算不算数」，所以拿该年度期末做时点，而不是「今天」——
/// 否则 2026-11 查 FY2027 的报表会出现「2027 年的预算还没生效，但 2027-02 的单据
/// 早被它管着」这种自相矛盾（E2E-06 发现的口径 bug）。
pub fn fiscal_year_end(fiscal_year: i16) -> Option<NaiveDate> {
    if !(FISCAL_YEAR_MIN..=FISCAL_YEAR_MAX).contains(&fiscal_year) {
        return None;
    }
    NaiveDate::from_ymd_opt(i32::from(fiscal_year), 12, 31)
}

/// 报表口径的生效判定：`effective_from <= 该会计年度期末`。
///
/// * `effective_from = None` → `true`（选项 A 之前落库的历史 ACTIVE 行没有生效日，
///   本来就该算生效，与 [`slot_snapshot`] 的 `effective_from IS NULL` 放行同口径）；
/// * `Some(from)` → `from <= fiscal_year_end(fiscal_year)`；
/// * 年份非法（构造不出期末）→ `true`（宁可显示「已生效」，也不因为一个坏年份把整行
///   标成未生效 —— 报表是展示，不是控制）。
///
/// 与提交侧的差别只在**时点**：控制按「单据期间月末」逐月判（[`period_end_of`]），
/// 报表按「该年度期末」整年判。两者判同一件事（`effective_from` 是否已到），
/// 只是粒不同 —— 月度粒度由 [`MonthGate`] 的月度闸体现。
pub fn effective_in_year(effective_from: Option<NaiveDate>, fiscal_year: i16) -> bool {
    match (effective_from, fiscal_year_end(fiscal_year)) {
        (None, _) | (_, None) => true,
        (Some(from), Some(end)) => from <= end,
    }
}

/// 月度闸的现状（**纯数据**）：月额度 / 月已用 → 月可用 / 是否已超 / 超了多少。
///
/// # 为什么单独抽出来（E2E06-FIX-2 的核心）
///
/// 这是全系统**唯一的月度闸公式**：提交侧（[`BudgetVerdict::evaluate`]）与执行报表
/// （`budget_model::report` → `month_summary`）都调它。此前报表只有年度口径，
/// 「本月已超 500」只藏在 `months[]` 明细里，主列一片「正常」，
/// 与提交侧月度闸的分叉必然被用户撞见（ARCH-6 §3.8.1）。
/// 抽成一个函数之后，改口径只用改这里。
///
/// 口径与年度闸同构：
/// ```text
/// 月可用 = 月额度 − 月已用            -- 带符号，报表直接展示（负数 = 超支）
/// 已超   = 月已用 > 月额度            -- 相等不算超（边界同年度闸）
/// 超支额 = 月已用 − 月额度
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthGate {
    /// 月度额度（无分解行 = 0）。
    pub limit: Decimal,
    /// 月度已用（`OCCUPIED + CONSUMED`，同期间）。
    pub used: Decimal,
}

impl MonthGate {
    /// 构造。
    pub fn of(limit: Decimal, used: Decimal) -> Self {
        Self { limit, used }
    }

    /// 月可用（**带符号**：负数 = 本月已超支多少）。
    pub fn available(&self) -> Decimal {
        self.limit - self.used
    }

    /// 月可用（**截零**）：提交侧算缺口用它。
    ///
    /// 为什么不直接用带符号的那个：缺口 = `本次 − 可用`，月已超时带符号的「可用」是负数，
    /// 减一个负数会把**历史超支**再算进本次缺口一次（`1000 − 1500 = −500`，
    /// 本次 100 → 缺口 600，其实本次只超 100）。截零后缺口 = `本次 − 0`，与旧实现逐位一致。
    pub fn available_clamped(&self) -> Decimal {
        self.available().max(Decimal::ZERO)
    }

    /// 本月**是否已超**（`已用 > 额度`；相等不算超 —— 与年度闸 `M = 可用` 的边界一致）。
    pub fn over(&self) -> bool {
        self.used > self.limit
    }

    /// 本月超支额（`None` = 没超，不是「超了 0」）。
    pub fn over_amount(&self) -> Option<Decimal> {
        self.over().then(|| self.used - self.limit)
    }
}

/// 判定结果（**纯数据，不碰库**，六个数全给，便于日志与单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetVerdict {
    /// 生效的控制策略。
    pub mode: ControlMode,
    /// 年度额度（无 ACTIVE 预算 = 0）。
    pub year_limit: Decimal,
    /// 年度已用（OCCUPIED + CONSUMED）。
    pub year_used: Decimal,
    /// 月度额度（无分解行 = 0）。
    pub month_limit: Decimal,
    /// 月度已用（同期间）。
    pub month_used: Decimal,
    /// 本次提交金额。
    pub this_amount: Decimal,
    /// 年度缺口 = max(0, 本次 − 年可用)。
    pub year_gap: Decimal,
    /// 月度缺口 = max(0, 本次 − 月可用)。
    pub month_gap: Decimal,
    /// 是否超预算（任一缺口 > 0）。
    pub over: bool,
    /// 该槽根本没有 ACTIVE 预算（额度按 0 处理）。
    pub missing_budget: bool,
}

impl BudgetVerdict {
    /// 判定（纯函数）。**`this == 可用` 不算超**（边界不能靠 `>=` 的直觉）。
    ///
    /// `budget_id` = 该槽**本期生效的 ACTIVE 预算版本**（[`slot_snapshot`] 的 `budget_id`）：
    /// * `None`（没有预算）→ `missing_budget = true`、`over = false`（FIX-5：没编预算 ≠ 超预算）；
    /// * `Some(..)`（有预算）→ 按两道闸算缺口，缺口 > 0 即 `over`。
    ///
    /// 为什么用「有没有预算行」而不是「额度是否为 0」：没编预算的槽被报过一次之后
    /// 已用就 > 0，用额度推断会把「未编制」误判成「超支」（列表页又变回一片红）。
    pub fn evaluate(
        mode: ControlMode,
        budget_id: Option<Uuid>,
        year_limit: Decimal,
        year_used: Decimal,
        month_limit: Decimal,
        month_used: Decimal,
        this_amount: Decimal,
    ) -> Self {
        let year_avail = (year_limit - year_used).max(Decimal::ZERO);
        // 月度闸走**公共公式** [`MonthGate`]：报表的 `month_over / month_available`
        // 调的是同一个方法，口径不可能分叉（E2E06-FIX-2）。
        let month_gate = MonthGate::of(month_limit, month_used);
        let month_avail = month_gate.available_clamped();
        let year_gap = (this_amount - year_avail).max(Decimal::ZERO);
        let month_gap = (this_amount - month_avail).max(Decimal::ZERO);
        // 没有本期生效的 ACTIVE 预算 = 未编制（与额度 / 已用无关）
        let missing_budget = budget_id.is_none();
        Self {
            mode,
            year_limit,
            year_used,
            month_limit,
            month_used,
            this_amount,
            year_gap,
            month_gap,
            // 没编预算 ≠ 超预算（FIX-5）：两者互斥，别让「没额度」冒充「花超了」
            over: !missing_budget && (year_gap > Decimal::ZERO || month_gap > Decimal::ZERO),
            missing_budget,
        }
    }

    /// 超支额 = 两个缺口里更大的那个（报文与快照都用它）。
    ///
    /// **不超支时返回 0**（保留旧签名，`Decimal` 方便算术）；要区分「没超」与「超了 0」
    /// 请用 [`Self::over_amount_opt`]。
    pub fn over_amount(&self) -> Decimal {
        self.over_amount_opt().unwrap_or(Decimal::ZERO)
    }

    /// 超支额的**可空**形式：`None` = 没超支（含「未编制预算」）。
    ///
    /// 落库用它：`over_amount` 列超支才有值，否则写 `NULL`（FIX-5 验收：未编预算
    /// → `over_budget=0 / missing_budget=1 / over_amount=null`）。
    pub fn over_amount_opt(&self) -> Option<Decimal> {
        let amount = self.year_gap.max(self.month_gap);
        (self.over && amount > Decimal::ZERO).then_some(amount)
    }

    /// 年可用（额度 − 已用，负数截零）。
    pub fn year_available(&self) -> Decimal {
        (self.year_limit - self.year_used).max(Decimal::ZERO)
    }

    /// 月可用（额度 − 已用，负数截零）。走公共公式 [`MonthGate`]。
    pub fn month_available(&self) -> Decimal {
        MonthGate::of(self.month_limit, self.month_used).available_clamped()
    }

    /// 是否应该阻断（`BLOCK` 且「超支」或「未编制预算」）。
    ///
    /// 「未编制预算」也算阻断**只在 BLOCK 下成立**：强管控的语义是「没有额度就不许花」，
    /// 与 ARCH-6 §3.2.5「配置缺失在 BLOCK 下是硬错误」一致。`WARN` / `OFF` 下
    /// 未编预算仍然照常提交（只标记 `missing_budget`），所以默认配置的业务不受影响。
    /// 0 元单永远不拦（没有钱发生，拦它只会挡住冲销/补录类草稿）。
    pub fn blocks(&self) -> bool {
        self.mode == ControlMode::Block && self.this_amount > Decimal::ZERO && (self.over || self.missing_budget)
    }

    /// 一句话结论（日志 / 409 报文 / 审批备注共用，避免三处各写一套文案）。
    pub fn describe(&self) -> String {
        let head = if self.missing_budget {
            "未编制预算".to_string()
        } else if self.over {
            format!("超出预算（超支 {}）", self.over_amount())
        } else {
            format!("预算内（剩余 {}）", self.year_available().min(self.month_available()))
        };
        format!(
            "{head}：本次 {} ｜ 年度 额度 {} / 已用 {} / 可用 {} ｜ 月度 额度 {} / 已用 {} / 可用 {} ｜ 策略 {}（{}）",
            self.this_amount,
            self.year_limit,
            self.year_used,
            self.year_available(),
            self.month_limit,
            self.month_used,
            self.month_available(),
            self.mode.as_str(),
            self.mode.label()
        )
    }
}

/// 一个槽的现状（查库结果，喂给 [`BudgetVerdict::evaluate`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSnapshot {
    /// 最新 ACTIVE 版本的 id（`None` = 该槽没有生效预算）。
    pub budget_id: Option<Uuid>,
    /// 年度额度。
    pub year_limit: Decimal,
    /// 年度已用。
    pub year_used: Decimal,
    /// 月度额度。
    pub month_limit: Decimal,
    /// 月度已用。
    pub month_used: Decimal,
}

/// 全局控制策略（`sys_config`，缺省 = `WARN`）。
pub async fn global_mode(ex: &impl SqlExecutor) -> DbResult<ControlMode> {
    // SCOPE-EXEMPT: 全局配置读 1 行，无归属列。
    let rows = ex
        .query_sql(
            "SELECT config_value FROM sys_config WHERE config_key = ? AND deleted_at IS NULL".to_string(),
            vec![Value::Text(CONFIG_CONTROL_MODE.to_string())],
        )
        .await?;
    let raw: Option<String> = match rows.first() {
        None => None,
        Some(row) => Some(row.get_typed("config_value")?),
    };
    Ok(raw.as_deref().and_then(ControlMode::parse).unwrap_or(ControlMode::Warn))
}

/// 采购提交是否做预算校验（`erp.budget.enforce_on_po`，缺省 true）。
pub async fn enforce_on_po(ex: &impl SqlExecutor) -> DbResult<bool> {
    // SCOPE-EXEMPT: 同上，全局开关 1 行。
    let rows = ex
        .query_sql(
            "SELECT config_value FROM sys_config WHERE config_key = ? AND deleted_at IS NULL".to_string(),
            vec![Value::Text(CONFIG_ENFORCE_ON_PO.to_string())],
        )
        .await?;
    let raw: Option<String> = match rows.first() {
        None => None,
        Some(row) => Some(row.get_typed("config_value")?),
    };
    Ok(!matches!(
        raw.as_deref().map(str::trim),
        Some("false") | Some("0") | Some("FALSE")
    ))
}

/// 读一个槽的现状（现算，不缓存）。
///
/// 三条聚合都走 `idx_biz_budget_usage_slot*`（ARCH-6 红线 2 的实测依据）。
///
/// **`effective_from` 参与判定**（FIX-2 / 选项 A）：只有 `effective_from IS NULL`（历史数据）
/// 或 `effective_from <= 期间月末` 的 ACTIVE 版本才算「本期生效」，未到生效日的版本
/// 额度按 0 处理 —— 否则「审批通过次月 1 日生效」形同虚设（11 月单据被 12 月额度管）。
pub async fn slot_snapshot(ex: &impl SqlExecutor, slot: &Slot) -> DbResult<SlotSnapshot> {
    // ① 最新 ACTIVE 版本（槽级唯一索引保证最多 1 行；`effective_from` 未到 → 视为没有预算）
    let period_end = slot.period_end_text().unwrap_or_else(|| "9999-12-31".to_string());
    let active = ex
        .query_sql(
            "SELECT id, total_amount::text AS total_amount \
               FROM biz_budget \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status = 'ACTIVE' AND deleted_at IS NULL \
                AND (effective_from IS NULL OR effective_from <= ?::text::date) \
              LIMIT 1"
                .to_string(),
            vec![
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
                Value::Text(period_end),
            ],
        )
        .await?;
    let (budget_id, year_limit) = match active.first() {
        None => (None, Decimal::ZERO),
        Some(row) => {
            let id: Uuid = row.get_typed("id")?;
            let text: String = row.get_typed("total_amount")?;
            (Some(id), parse_amount(&text))
        }
    };
    // ② 年度已用 / 月度已用（同槽、有效状态）
    let used = ex
        .query_sql(
            "SELECT COALESCE(SUM(amount), 0)::text AS year_used, \
                    COALESCE(SUM(amount) FILTER (WHERE period_code = ?), 0)::text AS month_used \
               FROM biz_budget_usage \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status IN ('OCCUPIED','CONSUMED') AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::Text(slot.period_code.clone()),
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
            ],
        )
        .await?;
    let (year_used, month_used) = match used.first() {
        None => (Decimal::ZERO, Decimal::ZERO),
        Some(row) => {
            let y: String = row.get_typed("year_used")?;
            let m: String = row.get_typed("month_used")?;
            (parse_amount(&y), parse_amount(&m))
        }
    };
    // ③ 月度额度（分解行；没有 ACTIVE 版本时必然 0）
    let month_limit = match budget_id {
        None => Decimal::ZERO,
        Some(id) => {
            let rows = ex
                .query_sql(
                    "SELECT amount::text AS amount FROM biz_budget_period \
                      WHERE budget_id = ?::uuid AND period_code = ?"
                        .to_string(),
                    vec![Value::from(&id), Value::Text(slot.period_code.clone())],
                )
                .await?;
            match rows.first() {
                None => Decimal::ZERO,
                Some(row) => {
                    let text: String = row.get_typed("amount")?;
                    parse_amount(&text)
                }
            }
        }
    };
    Ok(SlotSnapshot {
        budget_id,
        year_limit,
        year_used,
        month_limit,
        month_used,
    })
}

/// 读槽 + 判定（一次查库 + 纯函数）。
pub async fn check_slot(
    ex: &impl SqlExecutor,
    slot: &Slot,
    amount: Decimal,
    slot_mode: Option<&str>,
) -> DbResult<BudgetVerdict> {
    let global = global_mode(ex).await?;
    let mode = ControlMode::resolve(slot_mode, global);
    let snapshot = slot_snapshot(ex, slot).await?;
    Ok(BudgetVerdict::evaluate(
        mode,
        snapshot.budget_id,
        snapshot.year_limit,
        snapshot.year_used,
        snapshot.month_limit,
        snapshot.month_used,
        amount,
    ))
}

/// [`occupy`] 的入参（`ex` 之外的 9 项）。
#[derive(Debug, Clone, Copy)]
pub struct OccupyInput<'a> {
    /// 预算槽（部门 × 科目 × 期间口径），占用键就是它。
    pub slot: &'a Slot,
    /// 占用金额（含税）。
    pub amount: Decimal,
    /// 占用金额（不含税，超支判定用）。
    pub amount_net: Decimal,
    /// 占用来源类型（`SOURCE_PURCHASE` / `SOURCE_EXPENSE` / …，与 `source_id` 组成唯一键）。
    pub source_type: &'a str,
    /// 来源单据主键（唯一键的另一半）。
    pub source_id: Uuid,
    /// 来源单据号（可读，落 `source_no`）。
    pub source_no: &'a str,
    /// 判定结果（`over_budget` / `over_amount` 按提交时点快照落库）。
    pub verdict: &'a BudgetVerdict,
    /// 预算行快照（只取 `budget_id` 写审计列）。
    pub snapshot: &'a SlotSnapshot,
    /// 操作人（落 `create_by`；`None` = 系统 / 测试）。
    pub operator: Option<Uuid>,
}

/// 写一条占用（`OCCUPIED`）。**与来源单据的状态流转同事务**（ARCH-6 §3.5.1 事务边界）。
///
/// * `verdict` 的 `over_budget / over_amount` 作为**提交时点快照**落库（额度后来被调高，
///   历史单据的「当时超没超」不许跟着变）；
/// * `budget_id` 只写审计列（D5：不参与计算）；
/// * 撞 `uk_biz_budget_usage_source`（重复提交 / 重试）→ 返回 `None`，调用方当**幂等成功**。
///
/// # 先复用、后插入（撤回 / 驳回后重新提交）
///
/// 释放（[`release`]）只把行改成 `RELEASED`，**不删行**（留痕，`deleted_at` 仍为 `NULL`），
/// 而 `uk_biz_budget_usage_source` 的唯一性覆盖 `RELEASED` 行 —— 所以「提交 → 撤回 → 再提交」
/// 时直接 `INSERT` 必然撞 23505，整个提交事务被回滚（真机缺陷：重提 500，单据卡在 DRAFT）。
///
/// 因此这里的第一条语句是**条件 UPDATE**：同键若已有 `RELEASED` 行就把它拉回 `OCCUPIED`
/// （重置 `amount / amount_net / period_code / over_budget / over_amount / budget_id`，
/// 清空 `released_at / release_reason / consumed_at / voucher_id`），只有没命中才 `INSERT`。
///
/// * 唯一键语义不变：同一张单 × 同一槽天生只有一行，状态在 `OCCUPIED ↔ RELEASED` 之间往返；
/// * 历史可追溯：`id` / `created_at` 不动，`release_reason` 留在状态日志与审批轨迹里；
/// * `UPDATE … WHERE status = 'RELEASED' … RETURNING id` 是**原子**的（`UPDATE` 重新求值谓词
///   并持行锁），并发两个提交只有一个能把行拉回来。
///
/// # 为什么不能靠「撞 23505 当幂等」（本次 500 的真实链路）
///
/// 提交是**一个事务**（改状态 + 写占用 + 发审批单）。PostgreSQL 里任何一条语句报错都会把
/// 事务打成 aborted，之后每条语句只返回 `25P02`（`current transaction is aborted`）。
/// 于是旧实现的两行占用是这样炸的：第 1 行 23505 被 `occupy` 当幂等吞掉 → 第 2 行再执行时
/// 撞 `25P02`（这个码没有被映射成任何具体变体，落成 `DbError::Query`，文案就是
/// `query failed: : db error`）→ 整个提交 500。**吞掉 23505 并不能救事务**，
/// 所以正确做法是压根不产生冲突：有释放行就复用。
pub async fn occupy(ex: &impl SqlExecutor, input: OccupyInput<'_>) -> DbResult<Option<Uuid>> {
    let OccupyInput {
        slot,
        amount,
        amount_net,
        source_type,
        source_id,
        source_no,
        verdict,
        snapshot,
        operator,
    } = input;
    let budget_id = snapshot.budget_id.map(|id| id.to_string()).unwrap_or_default();
    // ① 复用：同键的 RELEASED 行拉回 OCCUPIED（撤回 / 驳回后重新提交）。
    let revived = ex
        .query_first_sql(
            "UPDATE biz_budget_usage \
                SET status = 'OCCUPIED', \
                    period_code = ?, \
                    amount = ?::text::numeric, \
                    amount_net = ?::text::numeric, \
                    source_no = ?, \
                    budget_id = NULLIF(?::text,'')::uuid, \
                    over_budget = ?, \
                    over_amount = NULLIF(?::text,'')::numeric, \
                    released_at = NULL, \
                    release_reason = NULL, \
                    consumed_at = NULL, \
                    voucher_id = NULL, \
                    updated_at = now() \
              WHERE source_type = ? AND source_id = ?::uuid \
                AND dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status = 'RELEASED' AND deleted_at IS NULL \
             RETURNING id"
                .to_string(),
            vec![
                Value::Text(slot.period_code.clone()),
                Value::Text(amount.to_string()),
                Value::Text(amount_net.to_string()),
                Value::Text(source_no.to_string()),
                Value::Text(budget_id.clone()),
                Value::from(i32::from(verdict.over)),
                Value::Text(over_amount_text(verdict)),
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
            ],
        )
        .await?;
    if let Some(row) = revived {
        return Ok(Some(row.get_typed("id")?));
    }
    // ② 没命中释放行 → 正常插入（首次提交）。
    let id = Uuid::now_v7();
    let result = ex
        .exec_sql(
            "INSERT INTO biz_budget_usage (
                 id, dept_id, account_id, fiscal_year, period_code,
                 amount, amount_net, source_type, source_id, source_no,
                 status, budget_id, over_budget, over_amount, create_by)
             VALUES (?, ?::uuid, ?::uuid, ?::int, ?, ?::text::numeric, ?::text::numeric,
                     ?, ?::uuid, ?, 'OCCUPIED', NULLIF(?::text,'')::uuid, ?::int,
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::uuid)"
                .to_string(),
            vec![
                Value::from(&id),
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
                Value::Text(slot.period_code.clone()),
                Value::Text(amount.to_string()),
                Value::Text(amount_net.to_string()),
                Value::Text(source_type.to_string()),
                Value::from(&source_id),
                Value::Text(source_no.to_string()),
                Value::Text(budget_id),
                Value::from(i32::from(verdict.over)),
                Value::Text(over_amount_text(verdict)),
                Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            ],
        )
        .await;
    match result {
        Ok(_) => Ok(Some(id)),
        Err(DbError::UniqueViolation { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

/// 释放某张单据的全部占用（驳回 / 撤回 / 取消 / 作废）。**只改状态，不删行**（留痕）。
pub async fn release(ex: &impl SqlExecutor, source_type: &str, source_id: Uuid, reason: &str) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget_usage \
            SET status = 'RELEASED', released_at = now(), release_reason = ?, updated_at = now() \
          WHERE source_type = ? AND source_id = ?::uuid \
            AND status IN ('OCCUPIED','CONSUMED') AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(truncate(reason, 255)),
            Value::Text(source_type.to_string()),
            Value::from(&source_id),
        ],
    )
    .await
}

/// 把某张单据的占用转成「已执行」（报销入账 / 采购订单关闭）。
pub async fn consume(
    ex: &impl SqlExecutor,
    source_type: &str,
    source_id: Uuid,
    voucher_id: Option<Uuid>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget_usage \
            SET status = 'CONSUMED', consumed_at = now(), voucher_id = NULLIF(?::text,'')::uuid, updated_at = now() \
          WHERE source_type = ? AND source_id = ?::uuid \
            AND status = 'OCCUPIED' AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(voucher_id.map(|id| id.to_string()).unwrap_or_default()),
            Value::Text(source_type.to_string()),
            Value::from(&source_id),
        ],
    )
    .await
}

/// 某张单据当前的占用快照（详情页 / 测试用）。
pub async fn usages_of(ex: &impl SqlExecutor, source_type: &str, source_id: Uuid) -> DbResult<Vec<UsageRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, dept_id, account_id, fiscal_year, period_code, amount::text AS amount, \
                    status, (over_budget <> 0) AS over_budget, over_amount::text AS over_amount, \
                    source_type, source_id, source_no, released_at, release_reason \
               FROM biz_budget_usage \
              WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY created_at"
                .to_string(),
            vec![Value::Text(source_type.to_string()), Value::from(&source_id)],
        )
        .await?;
    rows.iter().map(UsageRow::from_row).collect()
}

/// 某个**槽**（部门 × 科目 × 年度）下的全部占用（预算详情页用，FIX-2）。
///
/// 详情页拿到的是「预算单 id」，而 `biz_budget_usage.budget_id` 只是写入时命中的版本快照
/// （D5：不参与计算，历史 usage 可能指向旧版本甚至为空）—— 所以按**槽**查而不是按
/// `budget_id` 查，否则「报销产生的占用一条都看不到」（year_used 却是对的）。
pub async fn usages_of_slot(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    account_id: Uuid,
    fiscal_year: i16,
) -> DbResult<Vec<UsageRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, dept_id, account_id, fiscal_year, period_code, amount::text AS amount, \
                    status, (over_budget <> 0) AS over_budget, over_amount::text AS over_amount, \
                    source_type, source_id, source_no, released_at, release_reason \
               FROM biz_budget_usage \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND deleted_at IS NULL \
              ORDER BY period_code, created_at"
                .to_string(),
            vec![
                Value::from(&dept_id),
                Value::from(&account_id),
                Value::from(i32::from(fiscal_year)),
            ],
        )
        .await?;
    rows.iter().map(UsageRow::from_row).collect()
}

/// 一条占用记录（读模型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    /// 主键。
    pub id: Uuid,
    /// 槽：部门。
    pub dept_id: Uuid,
    /// 槽：科目。
    pub account_id: Uuid,
    /// 槽：年度。
    pub fiscal_year: i16,
    /// 发生期间。
    pub period_code: String,
    /// 占用金额（含税）。
    pub amount: Decimal,
    /// OCCUPIED / CONSUMED / RELEASED。
    pub status: String,
    /// 提交时点是否超预算。
    pub over_budget: bool,
    /// 提交时点超支额（`None` = 当时没超支；FIX-5 起未编预算的单据也是 `None`）。
    pub over_amount: Option<Decimal>,
    /// 来源单据类型（`EXPENSE_REPORT` / `PURCHASE_ORDER`）。
    pub source_type: String,
    /// 来源单据 id。
    pub source_id: Uuid,
    /// 来源单号。
    pub source_no: String,
    /// 释放时间。
    pub released_at: Option<DateTime<Utc>>,
    /// 释放原因。
    pub release_reason: Option<String>,
}

impl UsageRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            dept_id: row.get_typed("dept_id")?,
            account_id: row.get_typed("account_id")?,
            fiscal_year: row.get_typed("fiscal_year")?,
            period_code: row.get_typed("period_code")?,
            amount: parse_amount(&row.get_typed::<String>("amount")?),
            status: row.get_typed("status")?,
            over_budget: row.get_typed("over_budget")?,
            over_amount: row
                .get_typed::<Option<String>>("over_amount")?
                .map(|text| parse_amount(&text)),
            source_type: row.get_typed("source_type")?,
            source_id: row.get_typed("source_id")?,
            source_no: row.get_typed("source_no")?,
            released_at: row.get_typed("released_at")?,
            release_reason: row.get_typed("release_reason")?,
        })
    }
}

fn parse_amount(text: &str) -> Decimal {
    text.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

/// 金额的**人话**文本：去掉无意义的尾随 0（`3500.0000` → `3500`，`833.3300` → `833.33`）。
///
/// 只用于报文 / 日志（给业务人员看）；落库与 JSON 出参仍然用 `Decimal` 原样。
pub fn amount_text(amount: Decimal) -> String {
    amount.round_dp(4).normalize().to_string()
}

/// 期间码 `yyyy-MM` → `yyyy年M月`（报文用；非法输入原样回显，不吞）。
pub fn period_label(period_code: &str) -> String {
    match (period_code.get(0..4), period_code.get(5..7)) {
        (Some(year), Some(month)) => match month.trim_start_matches('0').parse::<u32>() {
            Ok(month) => format!("{year}年{month}月"),
            Err(_) => period_code.to_string(),
        },
        _ => period_code.to_string(),
    }
}

/// 落库用的超支额文本：**没超支 → 空串**（SQL 侧 `NULLIF(?,'')` 转 `NULL`）。
fn over_amount_text(verdict: &BudgetVerdict) -> String {
    verdict.over_amount_opt().map(amount_text).unwrap_or_default()
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ==================== 采购侧接线（控制点 ②，ARCH-6 §3.5.2） ====================

/// 采购提交时的预算判定结果（含「解析不到科目」这个分支）。
///
/// `verdict: None` 有两种含义，靠 [`PurchaseBudget::reason`] 区分：
/// * 该槽没有映射到预算科目（`mapped = false`）；
/// * 控制策略是 `OFF`（不校验、不占用）。
#[derive(Debug, Clone)]
pub struct PurchaseBudget {
    /// 生效策略。
    pub mode: ControlMode,
    /// 是否解析到了预算科目。
    pub mapped: bool,
    /// 命中的映射类型（MATERIAL / MATERIAL_TYPE / WAREHOUSE / DEFAULT）。
    pub map_type: Option<String>,
    /// 预算科目 id。
    pub account_id: Option<Uuid>,
    /// 预算槽（期间取单据日期）。
    pub slot: Option<Slot>,
    /// 判定结果（`None` = 没校验）。
    pub verdict: Option<BudgetVerdict>,
    /// 一句话说明（日志 / 报文用）。
    pub reason: String,
}

impl PurchaseBudget {
    /// 是否超预算。
    pub fn over(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.over)
    }

    /// 是否应该 409 阻断（`BLOCK` + 超预算）。
    pub fn blocks(&self) -> bool {
        self.verdict.as_ref().is_some_and(|v| v.blocks())
    }

    /// 是否需要写占用（解析到科目 + 策略不是 OFF）。
    pub fn should_occupy(&self) -> bool {
        self.mapped && self.mode.enforces()
    }
}

/// 采购单提交时的预算判定（**只读**，不写 usage）。
///
/// 科目解析顺序见 [`crate::business::erp::budget_model::resolve_for_po`]；
/// 部门取「提交人部门」快照（`voucher::user_dept_of`），结算/期间取单据日期。
/// 解析不到科目时按 ARCH-6 §3.2.5 的口径处置：
/// `OFF` / `WARN` → **不占用 + 告警**（映射是运维配置，配不全不该让采购停摆）；
/// `BLOCK` → 调用方 409（强管控下「配置缺失」是硬错误）。
pub async fn check_for_purchase(
    ex: &impl SqlExecutor,
    order: &crate::business::erp::model::PurchaseOrderRow,
    operator_dept: Option<Uuid>,
) -> DbResult<PurchaseBudget> {
    let global = global_mode(ex).await?;
    // 采购侧的全局开关：`erp.budget.enforce_on_po = false` → 整条链路退化成 OFF
    if !enforce_on_po(ex).await? {
        return Ok(PurchaseBudget {
            mode: ControlMode::Off,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: "erp.budget.enforce_on_po = false：采购提交不做预算校验、不占用预算".to_string(),
        });
    }
    // 部门：提交人部门优先，缺失时退回制单人部门
    let dept_id = match operator_dept {
        Some(dept) => Some(dept),
        None => match order.create_by {
            Some(creator) => crate::business::erp::voucher::user_dept_of(ex, creator).await?,
            None => None,
        },
    };
    let Some(dept_id) = dept_id else {
        return Ok(PurchaseBudget {
            mode: global,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: "提交人 / 制单人都没有部门：无法定位预算槽，本单不占用预算（请在用户资料里补部门）".to_string(),
        });
    };
    // 物料：取明细第一行（多物料按金额占比拆是 v2 的事，ARCH-6 §3.2.5 的 ① 命中即可）
    let items = crate::business::erp::purchase_model::items_of(order.id).await?;
    let material_id = items.first().map(|item| item.material_id);
    let material_type = match material_id {
        Some(material) => material_type_code_of(ex, material).await?,
        None => None,
    };
    let resolved = crate::business::erp::budget_model::resolve_for_po(
        ex,
        Some(dept_id),
        Some(order.warehouse_id),
        material_id,
        material_type.as_deref(),
    )
    .await?;
    let Some((account_id, map_type)) = resolved else {
        return Ok(PurchaseBudget {
            mode: global,
            mapped: false,
            map_type: None,
            account_id: None,
            slot: None,
            verdict: None,
            reason: format!(
                "采购单未匹配到预算科目（部门 {} / 仓库 {}）：本单不计入预算；\
                 请管理员在「预算科目映射」里配置（物料 / 物料类型 / 仓库 / 默认兜底）",
                dept_id, order.warehouse_id
            ),
        });
    };
    let period_code = order.order_date.format("%Y-%m").to_string();
    let Some(slot) = Slot::new(dept_id, account_id, &period_code) else {
        return Ok(PurchaseBudget {
            mode: global,
            mapped: false,
            map_type: Some(map_type),
            account_id: Some(account_id),
            slot: None,
            verdict: None,
            reason: format!("单据日期 {period_code} 不是合法期间，本单不占用预算"),
        });
    };
    // 槽级策略来自该槽的 ACTIVE 预算（没编预算 → 继承全局）
    let slot_mode = active_control_mode(ex, &slot).await?;
    let mode = ControlMode::resolve(slot_mode.as_deref(), global);
    let snapshot = slot_snapshot(ex, &slot).await?;
    let verdict = BudgetVerdict::evaluate(
        mode,
        snapshot.budget_id,
        snapshot.year_limit,
        snapshot.year_used,
        snapshot.month_limit,
        snapshot.month_used,
        order.total_with_tax,
    );
    Ok(PurchaseBudget {
        mode,
        mapped: true,
        map_type: Some(map_type),
        account_id: Some(account_id),
        slot: Some(slot),
        reason: verdict.describe(),
        verdict: Some(verdict),
    })
}

/// 该槽本期**生效**预算的控制策略（`None` = 没预算 / 没配 / 尚未到生效日 → 继承全局）。
///
/// `effective_from` 口径与 [`slot_snapshot`] 完全一致（FIX-2 选项 A）：未到生效日的版本
/// 既不提供额度、也不提供槽级策略 —— 否则「12 月才生效的 BLOCK 预算」会拦 11 月的单据。
pub async fn active_control_mode(ex: &impl SqlExecutor, slot: &Slot) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 槽级精确查询，返回 0..1 行的单列。
    let period_end = slot.period_end_text().unwrap_or_else(|| "9999-12-31".to_string());
    let rows = ex
        .query_sql(
            "SELECT control_mode FROM biz_budget \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND status = 'ACTIVE' AND deleted_at IS NULL \
                AND (effective_from IS NULL OR effective_from <= ?::text::date) \
              LIMIT 1"
                .to_string(),
            vec![
                Value::from(&slot.dept_id),
                Value::from(&slot.account_id),
                Value::from(i32::from(slot.fiscal_year)),
                Value::Text(period_end),
            ],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("control_mode"),
    }
}

/// 一条「已有预算但本期不生效」的事实（喂给 [`pending_warnings`] 拼文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBudget {
    /// 预算科目。
    pub account_id: Uuid,
    /// 科目编码（文案用）。
    pub account_code: String,
    /// 预算单号。
    pub budget_no: String,
    /// `DRAFT` / `SUBMITTED` / `ACTIVE`（后者 = 已生效但未到 `effective_from`）。
    pub status: String,
    /// 生效起始日（仅 ACTIVE 有值）。
    pub effective_from: Option<NaiveDate>,
    /// 年度额度（文案里带上，让用户知道「本来能管多少」）。
    pub total_amount: Decimal,
}

/// 一条**提示**（不是错误）：该槽的预算还没生效，本单的超支不会被它控制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetWarning {
    /// 提示码（[`WARN_BUDGET_NOT_EFFECTIVE`]）。
    pub code: String,
    /// 预算科目。
    pub account_id: Uuid,
    /// 科目编码。
    pub account_code: String,
    /// 预算单号。
    pub budget_no: String,
    /// 预算状态（`DRAFT` / `SUBMITTED` / `ACTIVE`）。
    pub status: String,
    /// 状态中文。
    pub status_label: String,
    /// 一句话说明（前端直接展示）。
    pub message: String,
}

/// 查「(部门, 年度) 下这些科目存在但本期不生效的预算」。
///
/// 三种情况都算「不生效」：
/// * `DRAFT` —— 编完没送审；
/// * `SUBMITTED` —— 送审了还没批下来；
/// * `ACTIVE` 且 `effective_from > 期间月末` —— 批了但还没到生效日。
///
/// **只读、不参与控制**：控制口径见 [`active_control_mode`] / [`slot_snapshot`]
/// （依然是「只认本期生效的 ACTIVE」）。本函数只解决「用户不知道预算没生效」。
pub async fn pending_budgets(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    fiscal_year: i16,
    account_ids: &[Uuid],
    period_code: &str,
) -> DbResult<Vec<PendingBudget>> {
    if account_ids.is_empty() {
        return Ok(Vec::new());
    }
    let period_end = period_end_of(period_code)
        .map(|date| date.to_string())
        .unwrap_or_else(|| "9999-12-31".to_string());
    // SCOPE-EXEMPT: 槽级精确查询（部门 + 年度 + 状态收敛），行数 = 该部门该年度的预算条数。
    let rows = ex
        .query_sql(
            "SELECT account_id, account_code, budget_no, status, effective_from, \
                    total_amount::text AS total_amount \
               FROM biz_budget \
              WHERE dept_id = ?::uuid AND fiscal_year = ?::int AND deleted_at IS NULL \
                AND (status IN ('DRAFT','SUBMITTED') \
                     OR (status = 'ACTIVE' AND effective_from IS NOT NULL \
                         AND effective_from > ?::text::date)) \
              ORDER BY account_code, version_no DESC"
                .to_string(),
            vec![
                Value::from(&dept_id),
                Value::from(i32::from(fiscal_year)),
                Value::Text(period_end),
            ],
        )
        .await?;
    let mut out = Vec::new();
    for row in rows.iter() {
        let account_id: Uuid = row.get_typed("account_id")?;
        if !account_ids.contains(&account_id) {
            continue;
        }
        // 同一科目只提示最相关的一条（同科目多版本时取版本号最大那行 —— SQL 已按其倒序）。
        if out.iter().any(|item: &PendingBudget| item.account_id == account_id) {
            continue;
        }
        out.push(PendingBudget {
            account_id,
            account_code: row.get_typed("account_code")?,
            budget_no: row.get_typed("budget_no")?,
            status: row.get_typed("status")?,
            effective_from: row.get_typed("effective_from")?,
            total_amount: parse_amount(&row.get_typed::<String>("total_amount")?),
        });
    }
    Ok(out)
}

/// 把「未生效预算」拼成人话提示（纯函数，单测钉文案口径）。
pub fn pending_warnings(pending: &[PendingBudget], period_code: &str) -> Vec<BudgetWarning> {
    pending
        .iter()
        .map(|item| {
            let (status_label, hint) = match item.status.as_str() {
                "DRAFT" => ("草稿", "请提交审批使其生效".to_string()),
                "SUBMITTED" => ("审批中", "审批通过后次月 1 日生效".to_string()),
                "ACTIVE" => (
                    "已生效（未到生效日）",
                    match item.effective_from {
                        Some(date) => format!("{date} 起生效"),
                        None => "生效日待定".to_string(),
                    },
                ),
                other => (other, String::new()),
            };
            BudgetWarning {
                code: WARN_BUDGET_NOT_EFFECTIVE.to_string(),
                account_id: item.account_id,
                account_code: item.account_code.clone(),
                budget_no: item.budget_no.clone(),
                status: item.status.clone(),
                status_label: status_label.to_string(),
                message: format!(
                    "科目 {} 的预算尚未生效（当前状态：{}，预算单 {}，额度 {}）：\
                     {period_code} 的超支将不受该预算控制。{hint}",
                    item.account_code, status_label, item.budget_no, item.total_amount
                ),
            }
        })
        .collect()
}

/// 物料的类型编码（`biz_material.material_type`）—— 供 MATERIAL_TYPE 映射命中。
///
/// `pub(crate)`：采购申请（P2-02）的预算判定要不重写一遍同样的取数口径 ——
/// 两个入口对「物料类型」的理解必须同源，否则同一个物料在申请与下单时命中不同科目。
pub(crate) async fn material_type_code_of(ex: &impl SqlExecutor, material_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 按主键取 1 行的单列。
    let rows = ex
        .query_sql(
            "SELECT material_type FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&material_id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => row.get_typed("material_type"),
    }
}

/// 写采购单的占用（**与单据状态流转同事务**，ARCH-6 §3.5.1 事务边界）。
///
/// 返回 `Some(usage_id)` = 新写入；`None` = 未占用（没解析到科目 / 策略 OFF /
/// 撞唯一键的幂等重放）。
pub async fn occupy_for_purchase(
    ex: &impl SqlExecutor,
    order: &crate::business::erp::model::PurchaseOrderRow,
    check: &PurchaseBudget,
    operator: Option<Uuid>,
) -> DbResult<Option<Uuid>> {
    if !check.should_occupy() {
        return Ok(None);
    }
    let (Some(slot), Some(verdict)) = (&check.slot, &check.verdict) else {
        return Ok(None);
    };
    let snapshot = slot_snapshot(ex, slot).await?;
    occupy(
        ex,
        OccupyInput {
            slot,
            amount: order.total_with_tax,
            amount_net: order.total_amount,
            source_type: SOURCE_PURCHASE,
            source_id: order.id,
            source_no: &order.order_no,
            verdict,
            snapshot: &snapshot,
            operator,
        },
    )
    .await
}

// ==================== 报销侧接线（控制点 ①，ARCH-6 §3.5.1） ====================
//
// 与采购侧（控制点 ②）的差别：
// * 采购是「一个物料 → 一个预算科目」（`resolve_for_po` 按物料 / 类型 / 仓库 / 默认兜底解析）；
// * 报销是「**明细自带科目**」—— 每条明细在提交时就把 `account_id` 快照下来了
//   （`biz_expense_item.account_id`），所以只要按 `(部门, 科目)` 聚合就得到槽金额，
//   不需要再解析映射表（ARCH-6 §3.5.1 的「按明细科目聚合写 N 行（同科目合并）」）。
// * `budget_controlled = 0` 的费用类型（罚款 / 滞纳金）**不产生 usage、不参与超支判定**
//   （ARCH-6 §4.6 末段）—— 由调用方在传参前过滤掉。

/// 一条待判定的「槽金额」：同一科目的明细已由调用方聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpenseSlotAmount {
    /// 预算科目（明细快照的借方科目）。
    pub account_id: Uuid,
    /// 含税金额（预算口径：报销按价税合计占额度）。
    pub amount: Decimal,
    /// 不含税金额（`amount_net` 列，只做分析，不参与判定）。
    pub amount_net: Decimal,
}

/// 一个槽的判定结果（含快照，供 `occupy_for_expense` 写 `budget_id`）。
#[derive(Debug, Clone)]
pub struct ExpenseSlotCheck {
    /// 槽。
    pub slot: Slot,
    /// 含税金额。
    pub amount: Decimal,
    /// 不含税金额。
    pub amount_net: Decimal,
    /// 判定结果。
    pub verdict: BudgetVerdict,
    /// 快照（`budget_id` 写进 usage 作审计）。
    pub snapshot: SlotSnapshot,
    /// 科目编码（报文用；科目表查不到就 `None`）。
    pub account_code: Option<String>,
    /// 科目名称（报文用）。
    pub account_name: Option<String>,
    /// 这个槽的策略来自槽级还是全局（报文必须说清楚，FIX-5）。
    pub mode_source: ModeSource,
}

impl ExpenseSlotCheck {
    /// 科目显示名：`5602.02 差旅费`（编码 / 名称缺就退化成另一项，都没有才用 id）。
    pub fn account_label(&self) -> String {
        match (&self.account_code, &self.account_name) {
            (Some(code), Some(name)) if !name.trim().is_empty() => format!("{code} {name}"),
            (Some(code), _) => code.clone(),
            (None, Some(name)) if !name.trim().is_empty() => name.clone(),
            _ => self.slot.account_id.to_string(),
        }
    }

    /// 这个槽的一句话现状（409 报文 / 日志 / 审批备注共用，避免三处各写一套文案）。
    ///
    /// 三种形态：**未编制预算** / **额度内** / **超支**。超支时按「咬得最狠」的那道闸
    /// （月度闸优先）报数，并点明策略是 `槽级策略` 还是 `全局策略`。
    pub fn describe(&self) -> String {
        let period = period_label(&self.slot.period_code);
        let verdict = &self.verdict;
        if verdict.missing_budget {
            let base = format!(
                "科目「{}」{period}：未编制预算（该期间没有生效的预算额度），本次 {}",
                self.account_label(),
                amount_text(self.amount)
            );
            // 被 BLOCK 拦下时要说明是**哪一级**策略拦的（WARN 下未编预算只是标记，不写策略）
            if verdict.blocks() {
                return format!(
                    "{base}，{} {}（{}）",
                    self.mode_source.label(),
                    verdict.mode.as_str(),
                    verdict.mode.label()
                );
            }
            return base;
        }
        if !verdict.over {
            return format!(
                "科目「{}」{period}：额度内（剩余 {}），本次 {}",
                self.account_label(),
                amount_text(verdict.year_available().min(verdict.month_available())),
                amount_text(self.amount)
            );
        }
        let (gate, used, limit, gap) = if verdict.month_gap >= verdict.year_gap && verdict.month_gap > Decimal::ZERO {
            ("月度", verdict.month_used, verdict.month_limit, verdict.month_gap)
        } else {
            ("年度", verdict.year_used, verdict.year_limit, verdict.year_gap)
        };
        format!(
            "科目「{}」{period}：{} {}（{}），{gate}已用 {} / {gate}预算 {}，本次 {}，超支 {}",
            self.account_label(),
            self.mode_source.label(),
            verdict.mode.as_str(),
            verdict.mode.label(),
            amount_text(used),
            amount_text(limit),
            amount_text(self.amount),
            amount_text(gap)
        )
    }
}

/// 报销提交时的预算判定汇总。
#[derive(Debug, Clone)]
pub struct ExpenseBudget {
    /// 生效的全局策略（每个槽可能被槽级 `control_mode` 覆盖）。
    pub mode: ControlMode,
    /// 部门。
    pub dept_id: Uuid,
    /// 期间（提交时按费用发生日所属期间取；入账时再按期重算不影响 usage，
    /// 因为 usage 的槽是按提交时期间冻结的审计事实）。
    pub period_code: String,
    /// 逐槽结果（同科目已合并）。
    pub checks: Vec<ExpenseSlotCheck>,
    /// 一句话说明（日志 / 报文用）。
    pub reason: String,
}

impl ExpenseBudget {
    /// 是否有任一槽超预算（真超支；「未编制预算」不算，见模块文档 FIX-5）。
    pub fn over(&self) -> bool {
        self.checks.iter().any(|check| check.verdict.over)
    }

    /// 是否有任一槽「未编制预算」（该槽没有本期生效的 ACTIVE 预算）。
    pub fn missing_budget(&self) -> bool {
        self.checks.iter().any(|check| check.verdict.missing_budget)
    }

    /// 是否应该 409 阻断（`BLOCK` + 超预算）。
    pub fn blocks(&self) -> bool {
        self.checks.iter().any(|check| check.verdict.blocks())
    }

    /// 触发阻断的槽（`BLOCK` + 真超支），按超支额降序（报文最严重的排第一）。
    pub fn blocking_checks(&self) -> Vec<&ExpenseSlotCheck> {
        let mut blocking: Vec<&ExpenseSlotCheck> = self.checks.iter().filter(|check| check.verdict.blocks()).collect();
        blocking.sort_by(|a, b| {
            b.verdict
                .over_amount()
                .cmp(&a.verdict.over_amount())
                .then_with(|| a.slot.account_id.cmp(&b.slot.account_id))
        });
        blocking
    }

    /// 409 报文的**按槽正文**（`None` = 没有槽需要阻断）。
    ///
    /// FIX-5 问题 1：每行用**触发阻断那条 check 自己**的策略（`槽级策略 BLOCK` /
    /// `全局策略 BLOCK`），不再拿全局 `global.label()` 解释槽级阻断。
    pub fn block_reason(&self) -> Option<String> {
        let blocking = self.blocking_checks();
        if blocking.is_empty() {
            return None;
        }
        Some(
            blocking
                .iter()
                .map(|check| format!("· {}", check.describe()))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// 409 报文的第一句：**超支**和**未编制预算**是两种不同的拒绝理由，别混着说。
    pub fn block_headline(&self) -> &'static str {
        if self.blocking_checks().iter().any(|check| check.verdict.over) {
            "超出预算额度"
        } else {
            "涉及的预算科目未编制（当前策略 BLOCK，不予放行）"
        }
    }

    /// 超支额（多槽取最大；与 `form_data.over_amount` 同口径；没超支 = 0）。
    pub fn over_amount(&self) -> Decimal {
        self.checks
            .iter()
            .filter(|check| check.verdict.over)
            .map(|check| check.verdict.over_amount())
            .max()
            .unwrap_or(Decimal::ZERO)
    }

    /// 超支额的**可空**形式（没超支 → `None`，落库写 `NULL`；FIX-5）。
    pub fn over_amount_opt(&self) -> Option<Decimal> {
        self.checks
            .iter()
            .filter_map(|check| check.verdict.over_amount_opt())
            .max()
    }

    /// 是否需要写占用（`OFF` 策略不写）。
    pub fn should_occupy(&self) -> bool {
        self.mode.enforces() && !self.checks.is_empty()
    }

    /// **记账口径**的策略：实际参与判定的槽级策略里最严的那条（没有槽 → 全局）。
    ///
    /// 为什么不是 `self.mode`：`mode` 是全局策略，槽级 `control_mode` 会覆盖它。
    /// 单据上的 `budget_mode` 是**事后审计**用的，必须记「这单真的被哪条策略管过」——
    /// 槽级 BLOCK、全局 WARN 时记 WARN，等于审计看不出这单被拦过（FIX-2 问题 4）。
    pub fn recorded_mode(&self) -> ControlMode {
        self.checks
            .iter()
            .map(|check| check.verdict.mode)
            .max_by_key(|mode| mode.strictness())
            .unwrap_or(self.mode)
    }
}

/// 报销提交时的预算判定（**只读**，不写 usage）。
///
/// `dept_id` 取报销单上的部门快照（不是「提交人当前部门」—— 单据是部门维度的凭证，
/// 报销人换了部门不该让历史单据的预算归属漂移）。
pub async fn check_for_expense(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    period_code: &str,
    items: &[ExpenseSlotAmount],
) -> DbResult<ExpenseBudget> {
    let global = global_mode(ex).await?;
    if !global.enforces() || items.is_empty() {
        return Ok(ExpenseBudget {
            mode: global,
            dept_id,
            period_code: period_code.to_string(),
            checks: Vec::new(),
            reason: if items.is_empty() {
                "本单没有受预算控制的明细（费用类型 budget_controlled = 0）：不占用预算".to_string()
            } else {
                "erp.budget.control_mode = OFF：报销提交不做预算校验、不占用预算".to_string()
            },
        });
    }
    let mut checks = Vec::with_capacity(items.len());
    for item in items {
        let Some(slot) = Slot::new(dept_id, item.account_id, period_code) else {
            return Ok(ExpenseBudget {
                mode: global,
                dept_id,
                period_code: period_code.to_string(),
                checks: Vec::new(),
                reason: format!("期间 {period_code} 不是合法会计期间：本单不占用预算"),
            });
        };
        let slot_mode = active_control_mode(ex, &slot).await?;
        // 策略来源：槽级显式配了就是槽级，否则继承全局（报文要说清楚，FIX-5）
        let mode_source = if slot_mode.as_deref().and_then(ControlMode::parse).is_some() {
            ModeSource::Slot
        } else {
            ModeSource::Global
        };
        let mode = ControlMode::resolve(slot_mode.as_deref(), global);
        let snapshot = slot_snapshot(ex, &slot).await?;
        let verdict = BudgetVerdict::evaluate(
            mode,
            snapshot.budget_id,
            snapshot.year_limit,
            snapshot.year_used,
            snapshot.month_limit,
            snapshot.month_used,
            item.amount,
        );
        // 科目编码 / 名称：只给报文用，查不到不报错（报文退化成 id）
        let (account_code, account_name) = account_label_of(ex, item.account_id).await?;
        checks.push(ExpenseSlotCheck {
            slot,
            amount: item.amount,
            amount_net: item.amount_net,
            verdict,
            snapshot,
            account_code,
            account_name,
            mode_source,
        });
    }
    // 文案只列**真正超支**的槽（逐槽说出「槽级策略 BLOCK」这类事实），
    // 「未编制预算」单独计数 —— 它是标记，不是超支（FIX-5）。
    let over_checks: Vec<&ExpenseSlotCheck> = checks.iter().filter(|check| check.verdict.over).collect();
    let missing_slots = checks.iter().filter(|check| check.verdict.missing_budget).count();
    let reason = if over_checks.is_empty() {
        if missing_slots > 0 {
            format!(
                "{missing_slots}/{} 个预算槽未编制预算（只标记、不按超支处理）",
                checks.len()
            )
        } else {
            format!("{} 个预算槽均在额度内", checks.len())
        }
    } else {
        let mut text = over_checks
            .iter()
            .map(|check| check.describe())
            .collect::<Vec<_>>()
            .join("；");
        if missing_slots > 0 {
            text.push_str(&format!("；另有 {missing_slots} 个预算槽未编制预算（不按超支处理）"));
        }
        text
    };
    Ok(ExpenseBudget {
        mode: global,
        dept_id,
        period_code: period_code.to_string(),
        checks,
        reason,
    })
}

/// 预算科目的编码 / 名称（409 报文用；科目表查不到就 `None`，不报错 ——
/// 报文是附加信息，不能因为缺科目把提交拦掉）。
async fn account_label_of(ex: &impl SqlExecutor, account_id: Uuid) -> DbResult<(Option<String>, Option<String>)> {
    // SCOPE-EXEMPT: 科目表按主键取 1 行。
    let rows = ex
        .query_sql(
            "SELECT account_code, account_name FROM biz_account WHERE id = ?::uuid".to_string(),
            vec![Value::from(&account_id)],
        )
        .await?;
    match rows.first() {
        None => Ok((None, None)),
        Some(row) => Ok((
            row.get_typed::<Option<String>>("account_code")?,
            row.get_typed::<Option<String>>("account_name")?,
        )),
    }
}

/// 写报销单的占用行（`OCCUPIED`）。**与「状态 → SUBMITTED」同事务**。
///
/// 返回实际写入的行数；重复提交（撞 `uk_biz_budget_usage_source`）按幂等成功处理
/// （`occupy` 返回 `None`），不算错。
pub async fn occupy_for_expense(
    ex: &impl SqlExecutor,
    budget: &ExpenseBudget,
    source_id: Uuid,
    source_no: &str,
    operator: Option<Uuid>,
) -> DbResult<usize> {
    if !budget.should_occupy() {
        return Ok(0);
    }
    let mut written = 0;
    for check in &budget.checks {
        if !check.verdict.mode.enforces() {
            continue;
        }
        let id = occupy(
            ex,
            OccupyInput {
                slot: &check.slot,
                amount: check.amount,
                amount_net: check.amount_net,
                source_type: SOURCE_EXPENSE,
                source_id,
                source_no,
                verdict: &check.verdict,
                snapshot: &check.snapshot,
                operator,
            },
        )
        .await?;
        if id.is_some() {
            written += 1;
        }
    }
    Ok(written)
}

// ==================== 编译期自检（纯函数，不连库） ====================

#[cfg(test)]
#[path = "budget_guard_tests.rs"]
mod budget_guard_tests;

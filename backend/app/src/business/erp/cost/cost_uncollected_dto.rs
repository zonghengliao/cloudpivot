//! 「未归集清单」的纯逻辑与出参（ARCH-9 §5.2 #33 / §6.2.2 ② · B2-12）。
//!
//! # 这个接口回答什么问题
//!
//! 成本核算向导第 ② 步「自查缺口」：**带着缺口也能算**（设计 §6.2.2「不阻断」），
//! 但结算前每一处缺口都得有解释。所以本接口一次给齐四块，每块都带
//! 「条数 + 金额 + 怎么修（hint）+ 点得动的跳转（jump）」：
//!
//! | 块 | 机器码 [`KIND_*`] | 语义 | 来源 |
//! |---|---|---|---|
//! | ① 未挂成本对象的领料 | [`KIND_STOCK_OUT`] | 已过账 `ISSUE` 出库单 `cost_object_id IS NULL` | `biz_stock_out` |
//! | ② 缺工时的在产对象 | [`KIND_LABOR_MISSING`] | 本期要点计算单、却一条 `CONFIRMED` 工时都没有的对象 | `biz_cost_object` + `biz_cost_labor` |
//! | ③ 缺费率的工时行 | [`KIND_LABOR_RATE`] | 已确认但 `rate = 0`（或金额舍入成 0）的行，B2-7 起就不写归集 | `biz_cost_labor` |
//! | ④ 未摊销的费用池 | [`KIND_POOL`] | 按 (车间, 科目) 汇总的池 − 已分摊结果 > 0 | `biz_cost_collect` + `biz_cost_alloc_result` |
//!
//! # 与卡面「三类」的关系（**取并集，不改设计**）
//!
//! 卡面验收写的是「缺工时 / 缺费率 / 缺成本对象的领料」，设计 §6.2.2 写的是
//! 「未挂成本对象的领料单 / 无工时的在产对象 / 未分摊的池」。两处**都只是三块**，
//! 但并起来是四块 —— 缺费率在设计里是「未归集工时」的子集（B2-7 的口径），
//! 在设计 §6.2.2 里没有单独成块。本接口把四块**全部**返回：
//! 卡面点名的三类 + 设计点名的池，一个都不少（前端按块渲染，多的那块不碍事）。
//!
//! # 为什么金额一律字符串
//!
//! 与 §5.1 / 全仓 ERP 出参同口径：`NUMERIC(18,4)` 走 JSON number 会在前端 JS 里
//! 丢精度（> 2^53 分），一律 `to_string()`。
//!
//! # 为什么每块都有 `row_limit`
//!
//! 「未归集」这类清单量级不可控（客户第一次上线可能几千张历史领料单没挂对象）。
//! 每块最多回 [`ROW_LIMIT`] 行，`count` 仍是**真实条数**（单独的 `count(*)`），
//! `truncated = true` 时前端提示「只显示前 N 条，请按跳转去原页处理」。

use nik::prelude::rust_decimal::Decimal;
use nik::uuid::Uuid;
use serde::Serialize;

/// 每块最多回多少行（超出只报 `count`）。
pub const ROW_LIMIT: usize = 200;

// ==================== 块的机器码（前端按它做图标 / 标题 / 路由，不按中文猜） ====================

/// ① 未挂成本对象的领料。
pub const KIND_STOCK_OUT: &str = "STOCK_OUT_WITHOUT_OBJECT";
/// ② 缺工时的在产对象。
pub const KIND_LABOR_MISSING: &str = "LABOR_MISSING";
/// ③ 缺费率的工时行。
pub const KIND_LABOR_RATE: &str = "LABOR_RATE_MISSING";
/// ④ 未摊销的费用池。
pub const KIND_POOL: &str = "POOL_UNALLOCATED";

// ==================== 池缺口的三种原因（设计 §6.2.2 的三条错误文案一一对应） ====================

/// 「池无规则」：该 (车间, 科目) 没有生效的分摊规则。
pub const REASON_NO_RULE: &str = "NO_RULE";
/// 「池无对象」：有规则，但该车间本期没有在产成本对象。
pub const REASON_NO_OBJECT: &str = "NO_OBJECT";
/// 有规则也有对象，池还没摊完（本期没算 / 算了一半 / 基数全 0）。
pub const REASON_NOT_ALLOCATED: &str = "NOT_ALLOCATED";

/// 原因的中文短标签（前端标签用）。
pub fn reason_label(reason: &str) -> &'static str {
    match reason {
        REASON_NO_RULE => "无生效规则",
        REASON_NO_OBJECT => "无在产对象",
        _ => "未摊完",
    }
}

// ==================== 跳转 ====================

/// 前端路由：领料（出库单）列表页。
pub const PATH_STOCK_OUT: &str = "/business/erp/stock-out";
/// 前端路由：工时台账。
pub const PATH_COST_LABOR: &str = "/business/erp/cost-labor";
/// 前端路由：分摊规则。
pub const PATH_ALLOC_RULE: &str = "/business/erp/cost-alloc-rule";
/// 前端路由：成本核算（向导所在页）。
pub const PATH_COST_PERIOD: &str = "/business/erp/cost-period";

/// 最小百分号编码（只对 ASCII 里非「URL 安全」的字符编码；CJK 原样透传）。
///
/// 本模块编码的都是 `out_no`（`OUT-yyyyMMdd-NNNNNN`）、`period_code`（`yyyy-MM`）、
/// UUID —— 全是 ASCII，走不到 CJK 分支；留着分支只是为了「别把中文编成乱码」。
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        // CJK 原样透传（绝不把中文编成乱码）；ASCII 里只放行 URL 安全字符
        if !ch.is_ascii() || ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            out.push_str(&format!("%{:02X}", ch as u8));
        }
    }
    out
}

/// 领料单深链：`/business/erp/stock-out?out_no=OUT-…`（本卡给列表页加了 `out_no` 深链）。
pub fn jump_stock_out(out_no: &str) -> String {
    format!("{PATH_STOCK_OUT}?out_no={}", encode(out_no))
}

/// 领料单列表（整块跳转）：按出库类型 + 出库日期区间筛出「本期的领料单」。
pub fn jump_stock_out_list(start: &str, end: &str) -> String {
    format!(
        "{PATH_STOCK_OUT}?out_type=ISSUE&date_from={}&date_to={}",
        encode(start),
        encode(end)
    )
}

/// 工时台账深链（对象 + 期间）：本卡给页面加了 `period_code` / `cost_object_id` 深链。
pub fn jump_labor(period_code: &str, cost_object_id: Uuid) -> String {
    format!(
        "{PATH_COST_LABOR}?period_code={}&cost_object_id={}",
        encode(period_code),
        cost_object_id
    )
}

/// 分摊规则页（池缺口：去建 / 去看规则）。
pub fn jump_alloc_rule() -> String {
    PATH_ALLOC_RULE.to_string()
}

/// 工时台账首页（整块跳转：没有具体对象时用）。
pub fn jump_labor_root() -> String {
    PATH_COST_LABOR.to_string()
}

/// 成本核算页（池缺口「未摊完」的真正动作是回本页重算；行级跳转用不到它时置空）。
pub fn jump_cost_period(period_code: &str) -> String {
    format!("{PATH_COST_PERIOD}?period_code={}", encode(period_code))
}

/// 「4101.01 制造费用-折旧费」：账套里有名字就带上，没有就只回编码。
pub fn account_label(code: &str, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|text| !text.is_empty()) {
        Some(name) => format!("{code} {name}"),
        None => code.to_string(),
    }
}

// ==================== 出参 ====================

/// 一块缺口（四块同构，前端一个组件渲染完）。
#[derive(Debug, Clone, Serialize)]
pub struct GapBlock<T> {
    /// 机器码（[`KIND_STOCK_OUT`] 等）。
    pub kind: String,
    /// 中文块名（侧边栏 / 折叠面板标题直接用）。
    pub title: String,
    /// **真实**条数（不受 `rows` 截断影响）。
    pub count: i64,
    /// 金额合计（字符串，`NUMERIC(18,4)`）。
    pub amount_total: String,
    /// 缺费率块的**受影响工时**合计（其它块恒 `"0"`：它们统计的是钱不是工时）。
    pub hours_total: String,
    /// 这块怎么修（整块级人话）。
    pub hint: String,
    /// 整块跳转（`None` = 动作就在本页，例如「重算」）。
    pub jump: Option<String>,
    /// `rows` 是否被截断（`count > rows.len()`）。
    pub truncated: bool,
    /// 行（最多 [`ROW_LIMIT`] 条）。
    pub rows: Vec<T>,
}

/// 组装一块（统一算 `truncated`，避免每块各写一遍）。
pub fn block_of<T>(
    kind: &str,
    title: &str,
    hint: String,
    jump: Option<String>,
    count: i64,
    amount_total: Decimal,
    rows: Vec<T>,
) -> GapBlock<T> {
    let truncated = count > i64::try_from(rows.len()).unwrap_or(0);
    GapBlock {
        kind: kind.to_string(),
        title: title.to_string(),
        count,
        amount_total: amount_total.to_string(),
        hours_total: "0".to_string(),
        hint,
        jump,
        truncated,
        rows,
    }
}

/// 组装缺费率块（唯一带「工时合计」的一块；钱算不出来，钱列恒 0）。
pub fn labor_rate_block_of(
    title: &str,
    hint: String,
    count: i64,
    hours_total: Decimal,
    rows: Vec<LaborRateGap>,
) -> GapBlock<LaborRateGap> {
    let mut block = block_of(
        KIND_LABOR_RATE,
        title,
        hint,
        Some(jump_labor_root()),
        count,
        Decimal::ZERO,
        rows,
    );
    block.hours_total = hours_total.to_string();
    block
}

/// 一块空块（接口在缺口为 0 时也必须给结构，前端不判 undefined）。
pub fn empty_block<T>(kind: &str, title: &str, hint: String, jump: Option<String>) -> GapBlock<T> {
    block_of(kind, title, hint, jump, 0, Decimal::ZERO, Vec::new())
}

/// ① 一张未挂成本对象的已过账领料单。
#[derive(Debug, Clone, Serialize)]
pub struct StockOutGap {
    /// 来源单据 id（**验收项**：可点击跳转的来源单据 id）。
    pub stock_out_id: String,
    /// 出库单号（给人看 + 深链筛选）。
    pub doc_no: String,
    /// 出库日期（`yyyy-MM-dd`）。
    pub doc_date: String,
    /// 出库类型（恒 `ISSUE`）。
    pub out_type: String,
    /// 出库仓库名。
    pub warehouse_name: Option<String>,
    /// 明细行数。
    pub item_count: i64,
    /// 数量合计。
    pub qty_total: String,
    /// 金额合计（过账时的账上真实成本）。
    pub amount_total: String,
    /// 怎么修（单行级）。
    pub hint: String,
    /// 单行跳转（领料单列表按单号定位）。
    pub jump: String,
}

/// ② 一个「本期要点计算单、却没有一条确认工时」的在产对象。
#[derive(Debug, Clone, Serialize)]
pub struct LaborMissingGap {
    /// 来源单据 id：成本对象。
    pub cost_object_id: String,
    /// 对象号 `CB-yyyyMMdd-NNNNNN`。
    pub object_no: String,
    /// 本期草稿工时行数（> 0 ⇒ 不是「没录」而是「没确认」，文案不同）。
    pub draft_labor_rows: i64,
    /// 本期材料投入（说明「这对象确实在产」，不是空批）。
    pub material_amount: String,
    /// 怎么修。
    pub hint: String,
    /// 跳转（工时台账，按期间 + 对象筛）。
    pub jump: String,
}

/// ③ 一条缺费率的已确认工时行。
#[derive(Debug, Clone, Serialize)]
pub struct LaborRateGap {
    /// 来源单据 id：工时台账行。
    pub labor_id: String,
    /// 成本对象。
    pub cost_object_id: String,
    /// 对象号。
    pub cost_object_no: Option<String>,
    /// 员工姓名快照。
    pub employee_name: String,
    /// 工作中心 id。
    pub dept_id: String,
    /// 工作中心名。
    pub dept_name: Option<String>,
    /// 工作日期（`yyyy-MM-dd`）。
    pub work_date: String,
    /// 未归集工时。
    pub hours: String,
    /// 台账上的工资率（`0` = 未维护）。
    pub rate: String,
    /// 为什么没归集（沿用 B2-7 的口径文案）。
    pub reason: String,
    /// 怎么修（查 B2-3 费率三级后给的具体建议）。
    pub hint: String,
    /// 跳转（工时台账，按期间 + 对象筛）。
    pub jump: String,
}

/// ④ 一个「有未摊完余额」的 (车间, 科目) 费用池。
#[derive(Debug, Clone, Serialize)]
pub struct PoolGap {
    /// 车间 id。
    pub dept_id: String,
    /// 车间名。
    pub dept_name: String,
    /// 费用科目（`4101.xx`）。
    pub account_code: String,
    /// 科目名（账套里没有该科目时为 `None`）。
    pub account_name: Option<String>,
    /// 「4101.01 制造费用-折旧费」。
    pub account_label: String,
    /// 池总额。
    pub pool_amount: String,
    /// 已分摊合计。
    pub allocated_amount: String,
    /// 未摊余额（> 0 才出现在清单里）。
    pub remaining_amount: String,
    /// 池里的来源行数（几张折旧 / 报销 / 手工费用）。
    pub source_rows: i64,
    /// 缺口原因（[`REASON_NO_RULE`] / [`REASON_NO_OBJECT`] / [`REASON_NOT_ALLOCATED`]）。
    pub reason: String,
    /// 原因中文标签。
    pub reason_label: String,
    /// 怎么修（设计 §6.2.2 的三条文案）。
    pub hint: String,
    /// 跳转（`None` = 回本页重算）。
    pub jump: Option<String>,
}

/// 批次摘要（期间已有 / 没有成本核算批次时都能回）。
#[derive(Debug, Clone, Serialize)]
pub struct BatchBrief {
    /// 批次 id（前端拿它调 `/cost-period/{id}/uncollected`）。
    pub id: String,
    /// 批次号 `CS-yyyyMM-0001`。
    pub batch_no: String,
    /// 批次状态。
    pub status: String,
}

/// 「未归集清单」出参（设计 §5.2 #33）。
#[derive(Debug, Clone, Serialize)]
pub struct UncollectedRes {
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 期间首日（`yyyy-MM-dd`）。
    pub period_start: String,
    /// 期间末日（`yyyy-MM-dd`）。
    pub period_end: String,
    /// 当期活跃批次（没有则 `None`：向导第 ② 步可能还没建批次）。
    pub batch: Option<BatchBrief>,
    /// 期间是否已结算 / 已结账（`true` 时缺口只作历史解释，不再是待办）。
    pub settled: bool,
    /// 四块条数合计（前端「有 N 处缺口」徽标用）。
    pub gap_count: i64,
    /// ① 未挂成本对象的领料。
    pub stock_out_without_object: GapBlock<StockOutGap>,
    /// ② 缺工时的在产对象。
    pub labor_missing: GapBlock<LaborMissingGap>,
    /// ③ 缺费率的工时行。
    pub labor_rate_missing: GapBlock<LaborRateGap>,
    /// ④ 未摊销的费用池。
    pub pool_unallocated: GapBlock<PoolGap>,
}

impl UncollectedRes {
    /// 四块条数合计（`gap_count` 的唯一计算口径）。
    pub fn total_gaps(&self) -> i64 {
        self.stock_out_without_object.count
            + self.labor_missing.count
            + self.labor_rate_missing.count
            + self.pool_unallocated.count
    }
}

// ==================== 纯逻辑 ====================

/// 池缺口判定（**纯函数**：库里三件事进来，原因或 `None` 出去）。
///
/// 顺序就是「先补什么」：没规则先建规则；有规则没对象先查建批；都有就是还没算。
/// `remaining <= 0` ⇒ 不是缺口（已摊完 / 池为空）。
pub fn pool_reason(has_rule: bool, has_open_object: bool, remaining: Decimal) -> Option<&'static str> {
    if remaining <= Decimal::ZERO {
        return None;
    }
    if !has_rule {
        return Some(REASON_NO_RULE);
    }
    if !has_open_object {
        return Some(REASON_NO_OBJECT);
    }
    Some(REASON_NOT_ALLOCATED)
}

/// 池缺口的「怎么修」文案（设计 §6.2.2「错误提示必须带怎么修」逐条落地）。
pub fn pool_hint(reason: &str, dept_name: &str, account_label: &str, remaining: Decimal) -> String {
    let amount = remaining.to_string();
    match reason {
        REASON_NO_RULE => format!(
            "{dept_name} 科目 {account_label} 有 {amount} 待分摊，但没有生效的分摊规则。\
             请到「分摊规则」新建规则（或确认该科目不进成本）后重算"
        ),
        REASON_NO_OBJECT => format!(
            "{dept_name} 科目 {account_label} 有 {amount} 待分摊，但本期没有在产成本对象。\
             请检查是否漏建成本对象（或把该车间本期确实无生产解释清楚）后重算"
        ),
        _ => format!(
            "{dept_name} 科目 {account_label} 有 {amount} 待分摊，本期分摊还没摊完。\
             请回到本页点「计算」重算后重看"
        ),
    }
}

/// 缺工时的「怎么修」文案。
///
/// `draft_rows > 0` 时**不能**说「没有工时记录」（用户明明录了，只是没确认），
/// 否则用户会去重复补录 —— 设计 §6.2.2 的原文是「没有工时记录」那一种形态。
pub fn labor_missing_hint(object_no: &str, draft_rows: i64) -> String {
    if draft_rows > 0 {
        format!(
            "成本对象 {object_no} 本期有 {draft_rows} 行草稿工时还没确认，人工成本算不进去。\
             请到「工时台账」批量确认后重算"
        )
    } else {
        format!(
            "成本对象 {object_no} 本期没有工时记录，无法计算人工成本。\
             请录入工时台账（每天或每周）后重算"
        )
    }
}

/// 缺费率行的金额口径：台账行 `amount` 恒等于 `round(hours × rate, 4)`（库层 CHECK 钉死），
/// 缺费率时它是 0。
pub fn money(value: Decimal) -> String {
    value.to_string()
}

/// 领料单「怎么修」文案（单行级）。
pub fn stock_out_hint(doc_no: &str) -> String {
    format!(
        "{doc_no} 已过账但没有挂成本对象：它的领料成本不进成本核算（按存量口径直接进 6401）。\
         若这笔料属于某个生产批，请作废后带上成本对象重开"
    )
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    #[test]
    fn block_codes_are_stable() {
        // 机器码是前后端契约，改名等于破坏前端（改这里必须同步 cost-uncollected-kinds.ts）
        assert_eq!(KIND_STOCK_OUT, "STOCK_OUT_WITHOUT_OBJECT");
        assert_eq!(KIND_LABOR_MISSING, "LABOR_MISSING");
        assert_eq!(KIND_LABOR_RATE, "LABOR_RATE_MISSING");
        assert_eq!(KIND_POOL, "POOL_UNALLOCATED");
        assert_eq!(REASON_NO_RULE, "NO_RULE");
        assert_eq!(REASON_NO_OBJECT, "NO_OBJECT");
        assert_eq!(REASON_NOT_ALLOCATED, "NOT_ALLOCATED");
    }

    #[test]
    fn pool_reason_prefers_rule_then_object_then_recalc() {
        // 未摊完 + 没规则 ⇒ 先建规则
        assert_eq!(
            pool_reason(false, true, dec("3000")),
            Some(REASON_NO_RULE),
            "没规则时先报「无生效规则」，不能报「未摊完」"
        );
        // 有规则但车间没在产对象
        assert_eq!(pool_reason(true, false, dec("3000")), Some(REASON_NO_OBJECT));
        // 都有 ⇒ 还没算 / 没算净
        assert_eq!(
            pool_reason(true, true, dec("0.0001")),
            Some(REASON_NOT_ALLOCATED),
            "哪怕是 1 分钱尾差也必须出现在清单里（不许静默吞）"
        );
        // 摊完了 / 池为空
        assert_eq!(pool_reason(true, true, Decimal::ZERO), None);
        assert_eq!(pool_reason(false, false, dec("-1")), None, "负余额不是缺口");
    }

    #[test]
    fn pool_hint_carries_amount_dept_account_and_next_action() {
        let text = pool_hint(REASON_NO_RULE, "一车间", "4101.01 制造费用-折旧费", dec("3000"));
        assert!(text.contains("一车间"), "{text}");
        assert!(text.contains("4101.01"), "{text}");
        assert!(text.contains("3000"), "{text}");
        assert!(text.contains("分摊规则"), "缺规则要指向「分摊规则」页：{text}");

        let text = pool_hint(REASON_NO_OBJECT, "一车间", "4101.01", dec("50"));
        assert!(text.contains("没有在产成本对象"), "{text}");

        let text = pool_hint(REASON_NOT_ALLOCATED, "一车间", "4101.01", dec("50"));
        assert!(text.contains("计算"), "未摊完的动作是「计算」：{text}");
    }

    #[test]
    fn labor_hint_distinguishes_not_recorded_from_not_confirmed() {
        let none = labor_missing_hint("CB-20261101-000003", 0);
        // 设计与卡面点名的原文（逐字保留，别改成「无工时」这种缩写）
        assert!(none.contains("本期没有工时记录"), "{none}");
        assert!(none.contains("CB-20261101-000003"), "{none}");
        assert!(none.contains("工时台账"), "{none}");

        let draft = labor_missing_hint("CB-20261101-000004", 3);
        assert!(draft.contains("3 行草稿工时"), "{draft}");
        assert!(!draft.contains("没有工时记录"), "已录未确认不能说「没有记录」：{draft}");
    }

    #[test]
    fn block_of_reports_true_count_and_truncation() {
        let block = block_of(
            KIND_STOCK_OUT,
            "未挂成本对象的领料",
            "hint".to_string(),
            Some(jump_stock_out_list("2026-11-01", "2026-11-30")),
            5,
            dec("1234.5600"),
            vec![1, 2],
        );
        assert_eq!(block.count, 5, "count 是真实条数");
        assert_eq!(block.rows.len(), 2);
        assert!(block.truncated, "5 > 2 必须标截断");
        assert_eq!(block.amount_total, "1234.5600");

        let full = block_of(
            KIND_POOL,
            "未摊完的池",
            String::new(),
            None,
            2,
            Decimal::ZERO,
            vec![1, 2],
        );
        assert!(!full.truncated);
        assert!(full.jump.is_none());
    }

    #[test]
    fn empty_block_is_zero_not_undefined() {
        let block: GapBlock<StockOutGap> =
            empty_block(KIND_STOCK_OUT, "未挂成本对象的领料", "无缺口".to_string(), None);
        assert_eq!(block.count, 0);
        assert!(block.rows.is_empty());
        assert!(!block.truncated);
        assert_eq!(block.amount_total, "0");
    }

    #[test]
    fn jumps_carry_the_source_doc_for_click_through() {
        // 验收：每类都要能点回来源单据
        let stock = jump_stock_out("OUT-20261101-000007");
        assert_eq!(stock, "/business/erp/stock-out?out_no=OUT-20261101-000007");

        let object = Uuid::from_u128(7);
        let labor = jump_labor("2026-11", object);
        assert!(labor.starts_with("/business/erp/cost-labor?period_code=2026-11&cost_object_id="));
        assert!(labor.ends_with(&object.to_string()));

        assert_eq!(jump_alloc_rule(), "/business/erp/cost-alloc-rule");
        assert_eq!(
            jump_cost_period("2026-11"),
            "/business/erp/cost-period?period_code=2026-11"
        );
    }

    #[test]
    fn jump_encodes_unsafe_ascii_only() {
        // 出库单号里不会有空格，但编码函数不该把中文编成乱码
        assert_eq!(encode("OUT-1"), "OUT-1");
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("车间"), "车间");
        // 期间里的 `-` 是 URL 安全字符，不能被编成 %2D（否则后端解析不出期间）
        assert_eq!(encode("2026-11"), "2026-11");
    }

    #[test]
    fn labor_rate_block_carries_hours_instead_of_money() {
        let block = labor_rate_block_of("缺费率的工时", "hint".to_string(), 3, dec("12.5"), Vec::new());
        assert_eq!(block.kind, KIND_LABOR_RATE);
        assert_eq!(block.count, 3);
        assert!(block.truncated, "3 条只回了 0 行 ⇒ 必须标截断");
        assert_eq!(block.amount_total, "0", "费率为 0 时钱算不出来");
        assert_eq!(block.hours_total, "12.5", "受影响工时是这块的核心数字");
        assert_eq!(block.jump.as_deref(), Some("/business/erp/cost-labor"));
    }

    #[test]
    fn account_label_falls_back_to_code() {
        assert_eq!(
            account_label("4101.01", Some("制造费用-折旧费")),
            "4101.01 制造费用-折旧费"
        );
        assert_eq!(account_label("4101.01", None), "4101.01");
        assert_eq!(account_label("4101.01", Some("  ")), "4101.01");
    }

    #[test]
    fn total_gaps_sums_the_four_blocks() {
        let res = UncollectedRes {
            period_code: "2026-11".to_string(),
            period_start: "2026-11-01".to_string(),
            period_end: "2026-11-30".to_string(),
            batch: None,
            settled: false,
            gap_count: 0,
            stock_out_without_object: block_of(KIND_STOCK_OUT, "a", String::new(), None, 2, Decimal::ZERO, Vec::new()),
            labor_missing: block_of(
                KIND_LABOR_MISSING,
                "b",
                String::new(),
                None,
                1,
                Decimal::ZERO,
                Vec::new(),
            ),
            labor_rate_missing: block_of(KIND_LABOR_RATE, "c", String::new(), None, 0, Decimal::ZERO, vec![]),
            pool_unallocated: block_of(KIND_POOL, "d", String::new(), None, 3, Decimal::ZERO, Vec::new()),
        };
        assert_eq!(res.total_gaps(), 6);
    }
}

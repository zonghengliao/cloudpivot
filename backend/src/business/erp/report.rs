//! 资产负债表（ERP-R1）：把「科目余额」**组装成报表**，并当场做勾稽校验。**只读**。
//!
//! 表结构依据 `backend/migrations/business/V0183__erp_balance_sheet.up.sql`
//! （`biz_report_item` 新建 + `biz_account` 补到 56 个标准科目），
//! 取数来源 `V0149` 的 `biz_voucher` / `biz_voucher_entry`。
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/balance-sheet?period=2026-10` | `business:erp:report:balance-sheet` | 资产负债表（含勾稽校验） |
//! | GET | `/business/erp/report/balance-sheet/export?period=2026-10` | `business:erp:report:export` | 同一份报表导出 xlsx |
//!
//! # 核心不变量（本模块存在的理由）
//!
//! ```text
//! 资产总计 ≡ 负债合计 + 所有者权益合计
//! ```
//!
//! **差一分钱都要报出来**：`check.balanced == false`、`check.diff` 给出精确差额、
//! `check.message` 给出「资产 X ≠ 负债+权益 Y，差 Z」的整句、`check.hints` 指出最可能的成因
//! （分录被删 / 科目类别配错 / 科目未归类）。绝不四舍五入抹平。
//!
//! ## 为什么这个恒等式在数学上必然成立（而不是"大概齐"）
//!
//! 复式记账下每张 POSTED 凭证的 `Σ借 = Σ贷`（库里有 `CHECK (total_debit = total_credit)`），
//! 所以对任意「整张凭证」的时间窗口都有 `Σ全部科目 (借−贷) = 0`。把科目按类别归到三侧：
//!
//! ```text
//! 资产侧 = ASSET + COST 类科目的净额（借−贷）
//! 负债侧 = LIABILITY 类净额的相反数
//! 权益侧 = EQUITY + PROFIT_LOSS 类净额的相反数   ← 未结账时利润还留在 5xxx/6xxx
//!
//! ⇒ 资产侧 − 负债侧 − 权益侧 = Σ全部科目 (借−贷) = 0
//! ```
//!
//! 所以 `diff != 0` **只可能**是这几种情况，`check.hints` 就是按这个清单给的：
//! ① 某张凭证的分录被删/改（分录合计 ≠ 单头合计，库里的 CHECK 拦不住）→ `unbalanced_vouchers`；
//! ② 科目的 `account_category` 配错（把资产配成负债）；
//! ③ 某科目的分录行 `account_id` 指向了已软删科目（取数取不到）。
//!
//! # 取数口径（与总账 / 余额表同一把尺子）
//!
//! ```text
//! opening_net = Σ(debit − credit)   where voucher_date <  period_start   （历史累计，含往年）
//! period_net  = Σ(debit − credit)   where voucher_date ∈ [period_start, period_end]
//! net         = opening_net + period_net                    （借为正）
//! 报表金额    = balance_direction = DEBIT ? net : −net      （按科目的正常余额方向取正）
//! ```
//!
//! * **只算 `status='POSTED'` 且未删除的凭证**（与 [`crate::business::erp::ledger`] 一致；
//!   草稿不进报表）；
//! * 红字凭证是**借贷互换**（`is_red=1`，金额恒为正），净额天然为负，直接累加即可，
//!   与总账同一口径，不需要特判；
//! * 备抵科目（`1602` 累计折旧 / `1702` 累计摊销）在 `biz_account` 里是
//!   **ASSET 类别 + CREDIT 方向**，报表金额 = 贷方余额（正数），再由报表项的
//!   `is_deduction = true` 在上级小计里**减掉**（硬红线 5）。
//!
//! # 报表项怎么算（`biz_report_item`）
//!
//! 一个报表项由三种方式之一得到金额，**配置在库里，不写死在代码里**：
//!
//! 1. `account_pattern`：科目匹配，四种写法
//!    * `1001,1002` 编码清单；
//!    * `2221` 编码 + 其全部下级（`2221` / `2221.01` / `2221.02`，靠 `2221.` 前缀前缀判定）；
//!    * `1*` / `1%` 编码前缀；
//!    * `@CATEGORY:PROFIT_LOSS` 按科目类别整类匹配（**新增损益科目自动进利润**，
//!      不需要改报表配置 —— 这是"新会计科目不会让报表变不平"的兜底）。
//! 2. `formula`：`+` / `-` 引用其它 `item_code`（只允许引用 `sort_order` 排在**前面**的项，
//!    引用未知编码 → 500，绝不静默算 0）。
//! 3. 两者都空 + `item_level = 1`：纯标题行（流动资产 / 负债 …），金额 `null`。
//!
//! # 未归类差额（`UNMAPPED_*` 三行）
//!
//! 科目表可以被运维随时新增。若某个科目有余额却没有任何报表项匹配它，钱不会凭空消失：
//! 每个 `side` 的差额 = 该侧类别净额合计 − 该侧已映射金额合计，作为
//! `UNMAPPED_ASSETS` / `UNMAPPED_LIABILITIES` / `UNMAPPED_EQUITY` 三行**显式出现在报表上**，
//! 同时进 `check.unmapped`（带科目编码与金额）。正常情况下这三行恒为 0.00。
//!
//! 这样做的收益是**可证**的：`TOTAL_ASSETS` 恒等于 ASSET+COST 类别净额，
//! `TOTAL_LIABILITIES_AND_EQUITY` 恒等于 LIABILITY+EQUITY+PROFIT_LOSS 净额，
//! 于是 `diff` 只剩「借贷不等」这一个成因 —— 报表不平就是真账不平，不是映射漏了。
//!
//! # 快照隔离（硬红线 3）
//!
//! 整个报表在**一个 REPEATABLE READ 事务**里算完（科目余额 + 报表项配置 + 上期对比 +
//! 借贷合计 + 异常凭证清单，5 条 SQL 同一快照）：
//! `SET TRANSACTION ISOLATION LEVEL REPEATABLE READ` 必须是 `BEGIN` 后的**第一条**语句
//! （PostgreSQL 规定），否则报 25001。
//!
//! # 期间口径（与余额表的差异，都是刻意的）
//!
//! * **优先取 `biz_accounting_period`**：`OPEN` / `CLOSED` 都能出表，起止日按库里的
//!   （月结把期间拉长/缩短时，报表跟着走，不按自然月）。
//! * **库里没有期间行，但该月有已记账凭证** → 按自然月推算，`period_status = "DERIVED"`，
//!   `check.hints` 里带一条「请尽快开账」。理由：生产库实测有账无期间的月份（2026-11 两张凭证），
//!   严格 400 会让"有账的月份出不了表"，用户会以为账丢了。
//! * **库里没有期间行，且该月一张凭证都没有** → `400 + 66010`（消息里写清怎么开账）。
//!   空月份绝不编造报告期：那才是真正的静默错误（用户会以为"那个月没发生业务"）。
//! * 状态与期间天数原样回显（`period_status` / `period_start` / `period_end`）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_report_item 是报表配置、biz_account / biz_voucher / biz_voucher_entry 是
//! 全局会计台账（V0137/V0149/V0183 建表，无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:report:* 权限码控制，本模块无分页查询。
//! ```

use std::collections::{BTreeSet, HashMap};

use axum::response::Response;
use nik::prelude::chrono::{DateTime, Local, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::voucher_query::is_month_key;
use crate::shared::error_code;
use crate::utils;

// ==================== 表名 / 常量 ====================

/// 报表编码（`biz_report_item.report_code`）。
pub const REPORT_CODE: &str = "BALANCE_SHEET";

/// 报表中文名（导出文件名 / 页头）。
pub const REPORT_NAME: &str = "资产负债表";

/// 报表项配置表。
pub const ITEM_TABLE: &str = "biz_report_item";

/// 科目表。
pub const ACCOUNT_TABLE: &str = "biz_account";

/// 会计期间表。
pub const PERIOD_TABLE: &str = "biz_accounting_period";

/// 凭证主表。
pub const VOUCHER_TABLE: &str = "biz_voucher";

/// 凭证分录表。
pub const ENTRY_TABLE: &str = "biz_voucher_entry";

/// 只算已记账。
pub const POSTED: &str = "POSTED";

/// 三侧（与 `ck_biz_report_side` 逐字一致）。
pub const SIDES: [&str; 3] = ["ASSET", "LIABILITY", "EQUITY"];

/// 未归类差额行的编码（不落库，运行时补出来的）。
pub const UNMAPPED_ASSETS: &str = "UNMAPPED_ASSETS";
/// 未归类负债。
pub const UNMAPPED_LIABILITIES: &str = "UNMAPPED_LIABILITIES";
/// 未归类权益。
pub const UNMAPPED_EQUITY: &str = "UNMAPPED_EQUITY";

/// 资产总计。
pub const TOTAL_ASSETS: &str = "TOTAL_ASSETS";
/// 负债和所有者权益总计。
pub const TOTAL_LIABILITIES_AND_EQUITY: &str = "TOTAL_LIABILITIES_AND_EQUITY";

/// 类别 → 报表侧（成本类并入资产侧：在产品 / 制造费用是存货的组成部分）。
pub fn category_side(category: &str) -> Option<&'static str> {
    match category {
        "ASSET" | "COST" => Some("ASSET"),
        "LIABILITY" => Some("LIABILITY"),
        "EQUITY" | "PROFIT_LOSS" => Some("EQUITY"),
        _ => None,
    }
}

// ==================== 入参 ====================

/// 资产负债表查询参数（`period` 必填，`yyyy-MM`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BalanceSheetQuery {
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
}

// ==================== 科目余额 ====================

/// 单个科目的报表取数（一次查询拿全，纯数据）。
#[derive(Debug, Clone)]
pub struct AccountBalance {
    /// 科目主键。
    pub account_id: Uuid,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别（ASSET / LIABILITY / EQUITY / COST / PROFIT_LOSS）。
    pub account_category: String,
    /// 余额方向（DEBIT / CREDIT）。
    pub balance_direction: String,
    /// 是否末级。
    pub is_leaf: bool,
    /// 级次。
    pub level: i16,
    /// 期初净额（借 − 贷，期间开始日之前的历史累计）。
    pub net_before: Decimal,
    /// 本期净额（借 − 贷）。
    pub net_period: Decimal,
}

impl AccountBalance {
    /// 期末净额（借为正）。
    pub fn net(&self) -> Decimal {
        self.net_before + self.net_period
    }

    /// 报表口径金额：该科目**对所在报表侧的贡献**（正数 = 增加本侧金额）。
    ///
    /// * 资产负债类：按科目的正常余额方向取正 —— 借方科目 = `net`，
    ///   贷方科目 = `−net`（备抵科目 1602 / 1702 是 ASSET+CREDIT，于是"累计折旧"
    ///   取出来是正数，再由报表项的 `is_deduction` 决定在小计里减掉）；
    /// * **损益类一律 `−net`**（这是「利润对权益的贡献」）：
    ///   收入贷方余额 → `net` 为负 → 贡献为正；费用（借方）→ `net` 为正 → 贡献为负。
    ///   不能按余额方向取正：那样费用会**增加**权益（R1-⑪ 单测就是这么抓到的）。
    pub fn display(&self) -> Decimal {
        if self.account_category == "PROFIT_LOSS" || self.balance_direction == "CREDIT" {
            -self.net()
        } else {
            self.net()
        }
    }

    /// 所属报表侧。
    pub fn side(&self) -> Option<&'static str> {
        category_side(&self.account_category)
    }

    /// 该科目对**本侧净额**的贡献（借为正、按侧折算）。
    ///
    /// 资产侧 = `net`（备抵科目的负 net 天然是减项）；
    /// 负债 / 权益侧 = `−net`（贷方余额为正）。
    pub fn side_econ(&self) -> Decimal {
        zero_norm(match category_side(&self.account_category) {
            Some("ASSET") => self.net(),
            Some(_) => -self.net(),
            None => Decimal::ZERO,
        })
    }
}

// ==================== 报表项配置 ====================

/// `biz_report_item` 的一行（纯数据，供 [`compute`] 消费）。
#[derive(Debug, Clone)]
pub struct ReportItemDef {
    /// 报表项编码。
    pub item_code: String,
    /// 报表项名称。
    pub item_name: String,
    /// 层级。
    pub item_level: i16,
    /// 上级编码。
    pub parent_code: Option<String>,
    /// 所属侧。
    pub side: String,
    /// 科目匹配模式。
    pub account_pattern: Option<String>,
    /// 计算式。
    pub formula: Option<String>,
    /// 是否减项。
    pub is_deduction: bool,
    /// 显示顺序（同时决定公式求值顺序）。
    pub sort_order: i32,
}

/// 报表行（出参，同时是前端渲染的唯一依据）。
#[derive(Debug, Clone, Serialize)]
pub struct ReportLineRes {
    /// 报表项编码。
    pub item_code: String,
    /// 报表项名称。
    pub item_name: String,
    /// 层级（1 大类 / 2 小类 / 3 明细）。
    pub level: i16,
    /// 上级编码。
    pub parent_code: Option<String>,
    /// 金额（标题行为 `null`）。
    pub amount: Option<Decimal>,
    /// 上期金额（无上期期间时为 `null`）。
    pub previous_amount: Option<Decimal>,
    /// 是否减项（累计折旧 / 累计摊销）。
    pub is_deduction: bool,
    /// 是否小计 / 合计行（公式项）。
    pub is_total: bool,
    /// 取数说明（`'1001+1002'` 模式原文或公式原文），前端显示在「数据来源」列。
    pub detail: Option<String>,
    /// 实际命中的科目编码（可审计：报表项到底取了哪些科目的钱）。
    pub accounts: Vec<String>,
}

/// 未归类科目（有余额但没有任何报表项匹配）。
#[derive(Debug, Clone, Serialize)]
pub struct UnmappedAccountRes {
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别。
    pub account_category: String,
    /// 该侧金额。
    pub amount: Decimal,
}

/// 分录合计与单头不符的凭证（不平的直接证据）。
#[derive(Debug, Clone, Serialize)]
pub struct UnbalancedVoucherRes {
    /// 凭证号。
    pub voucher_no: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 分场合计（借）。
    pub entry_debit: Decimal,
    /// 分场合计（贷）。
    pub entry_credit: Decimal,
    /// 单头合计（借）。
    pub header_debit: Decimal,
    /// 单头合计（贷）。
    pub header_credit: Decimal,
}

/// 勾稽校验结果。
#[derive(Debug, Clone, Serialize)]
pub struct BalanceCheckRes {
    /// 资产总计 ≡ 负债+权益总计。
    pub balanced: bool,
    /// 资产总计（`TOTAL_ASSETS`）。
    pub assets_total: Decimal,
    /// 负债和所有者权益总计（`TOTAL_LIABILITIES_AND_EQUITY`）。
    pub liability_equity_total: Decimal,
    /// 差额 = 资产 − (负债+权益)，**不做任何舍入**。
    pub diff: Decimal,
    /// 差额人话（`'0.00'` / `'差 0.0100 元'`）。
    pub diff_text: String,
    /// 整句结论（前端直接显示）。
    pub message: String,
    /// 窗口内分录借方合计（诊断用）。
    pub ledger_debit_total: Decimal,
    /// 窗口内分录贷方合计（诊断用）。
    pub ledger_credit_total: Decimal,
    /// 未归类科目。
    pub unmapped: Vec<UnmappedAccountRes>,
    /// 分录不平的凭证。
    pub unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
    /// 成因提示（人话，最多 5 条）。
    pub hints: Vec<String>,
}

/// 资产负债表出参。
#[derive(Debug, Clone, Serialize)]
pub struct BalanceSheetRes {
    /// 报表编码。
    pub report_code: String,
    /// 报表名称。
    pub report_name: String,
    /// 编制单位。
    pub company_name: String,
    /// 金额单位。
    pub unit: String,
    /// 会计期间（`yyyy-MM`）。
    pub period: String,
    /// 期间中文（`2026年10月`）。
    pub period_name: String,
    /// 期间状态（`OPEN` / `CLOSED`）。
    pub period_status: String,
    /// 期间开始日。
    pub period_start: NaiveDate,
    /// 期间结束日。
    pub period_end: NaiveDate,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
    /// 上期期间（`yyyy-MM`，无上期期间时为 `null`）。
    pub previous_period: Option<String>,
    /// 资产侧。
    pub assets: Vec<ReportLineRes>,
    /// 负债侧。
    pub liabilities: Vec<ReportLineRes>,
    /// 所有者权益侧。
    pub equity: Vec<ReportLineRes>,
    /// 勾稽校验。
    pub check: BalanceCheckRes,
}

// ==================== 期间解析 ====================

/// 期间 → `(年, 月)`：**纯逻辑**，`yyyy-MM` 且月份 1..=12。
pub fn parse_period(period: &str) -> Option<(i32, u32)> {
    if !is_month_key(period) {
        return None;
    }
    let (year, month) = period.split_once('-')?;
    let year: i32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

/// 期间编码 → 自然月起止日（`2026-10` → `2026-10-01` / `2026-10-31`）。
pub fn month_range(period: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (year, month) = parse_period(period)?;
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let end = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    }
    .pred_opt()?;
    Some((start, end))
}

/// 上一期间编码（`2026-01` → `2025-12`；`2026-10` → `2026-09`）。
pub fn previous_period_of(period: &str) -> Option<String> {
    let (year, month) = parse_period(period)?;
    let (year, month) = if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    };
    Some(format!("{year:04}-{month:02}"))
}

/// 期间中文名（`2026-10` → `2026年10月`）。
pub fn period_name_of(period: &str) -> String {
    match parse_period(period) {
        Some((year, month)) => format!("{year}年{month}月"),
        None => period.to_string(),
    }
}

// ==================== 科目匹配（纯逻辑，可单测） ====================

/// 判断科目是否命中匹配模式。
///
/// 语法见模块文档：编码清单 / 编码+下级 / 前缀（`*` 或 `%`）/ `@CATEGORY:类别`，逗号分隔取并集。
pub fn match_pattern(pattern: &str, account: &AccountBalance) -> bool {
    match_account(pattern, &account.account_code, &account.account_category)
}

/// 同上，但只吃「科目编码 + 类别」——供不带 [`AccountBalance`] 的报表复用
/// （利润表按发生额取数，行结构不同，但**匹配语法必须只有一份**）。
pub fn match_account(pattern: &str, account_code: &str, account_category: &str) -> bool {
    pattern.split(',').any(|raw| {
        let token = raw.trim();
        if token.is_empty() {
            return false;
        }
        if let Some(category) = token.strip_prefix("@CATEGORY:") {
            return account_category.eq_ignore_ascii_case(category.trim());
        }
        if let Some(prefix) = token.strip_suffix('*').or_else(|| token.strip_suffix('%')) {
            return !prefix.is_empty() && account_code.starts_with(prefix);
        }
        // 编码本身 或 其下级（`2221` 命中 `2221` / `2221.01`）
        account_code == token || account_code.starts_with(&format!("{token}."))
    })
}

// ==================== 公式求值（纯逻辑，可单测） ====================

/// 求值 `A + B - C` 形式（只支持 `+` / `-`，无括号）。
///
/// 返回 `Err` 的两种情况都**必须**让接口 500，而不是静默算 0：
/// ① 引用了尚未计算 / 不存在的 `item_code`；② 算式本身不合法。
pub fn eval_formula(
    formula: &str,
    amounts: &HashMap<String, Decimal>,
) -> std::result::Result<Decimal, String> {
    let mut total = Decimal::ZERO;
    let mut sign = Decimal::ONE;
    let mut expect_operand = true;
    for raw in formula.split_whitespace() {
        match raw {
            "+" => {
                if expect_operand {
                    return Err(format!("算式「{formula}」出现连续运算符"));
                }
                sign = Decimal::ONE;
                expect_operand = true;
            }
            "-" => {
                if expect_operand {
                    return Err(format!("算式「{formula}」出现连续运算符"));
                }
                sign = -Decimal::ONE;
                expect_operand = true;
            }
            operand => {
                if !expect_operand {
                    return Err(format!(
                        "算式「{formula}」缺少运算符（`{operand}` 前）—— 只支持空格分隔的 `+` / `-`"
                    ));
                }
                let value = amounts.get(operand).ok_or_else(|| {
                    format!("算式「{formula}」引用了未计算或不存在的报表项 `{operand}`（检查 biz_report_item.sort_order 顺序）")
                })?;
                total += sign * *value;
                expect_operand = false;
            }
        }
    }
    if expect_operand {
        return Err(format!("算式「{formula}」以运算符结尾"));
    }
    Ok(total)
}

// ==================== 报表计算（纯逻辑，可单测） ====================

/// 把 `-0` 归一成 `0`。
///
/// `Decimal` 有符号位：`-Decimal::ZERO` 序列化成 `-0` / `-0.0000`，报表上出现「-0」很难看，
/// 而且会让人以为有负数余额。所有参与出参的金额都过一遍这个函数。
pub fn zero_norm(value: Decimal) -> Decimal {
    if value.is_zero() {
        Decimal::ZERO
    } else {
        value
    }
}

/// 一侧的未归类差额（运行时补出来的行）。
fn unmapped_line(side: &str, amount: Decimal) -> ReportLineRes {
    let (code, name) = match side {
        "LIABILITY" => (UNMAPPED_LIABILITIES, "未归类负债（映射缺失）"),
        "EQUITY" => (UNMAPPED_EQUITY, "未归类权益（映射缺失）"),
        _ => (UNMAPPED_ASSETS, "未归类资产（映射缺失）"),
    };
    ReportLineRes {
        item_code: code.to_string(),
        item_name: name.to_string(),
        level: 2,
        parent_code: None,
        amount: Some(amount),
        previous_amount: None,
        is_deduction: false,
        is_total: false,
        detail: Some("按科目类别差额计算：该侧类别净额 − 已映射报表项合计".to_string()),
        accounts: Vec::new(),
    }
}

/// 报表计算结果。
#[derive(Debug, Clone, Default)]
pub struct ComputedSheet {
    /// 资产侧行。
    pub assets: Vec<ReportLineRes>,
    /// 负债侧行。
    pub liabilities: Vec<ReportLineRes>,
    /// 权益侧行。
    pub equity: Vec<ReportLineRes>,
    /// 各项金额（含小计，供上期对比查表）。
    pub amounts: HashMap<String, Decimal>,
    /// 三侧类别净额合计。
    pub side_totals: HashMap<String, Decimal>,
    /// 未归类科目。
    pub unmapped: Vec<UnmappedAccountRes>,
}

impl ComputedSheet {
    /// 资产总计。
    pub fn assets_total(&self) -> Decimal {
        self.amounts.get(TOTAL_ASSETS).copied().unwrap_or_default()
    }

    /// 负债和所有者权益总计。
    pub fn liability_equity_total(&self) -> Decimal {
        self.amounts
            .get(TOTAL_LIABILITIES_AND_EQUITY)
            .copied()
            .unwrap_or_default()
    }

    /// 差额（不做舍入）。
    pub fn diff(&self) -> Decimal {
        self.assets_total() - self.liability_equity_total()
    }
}

/// 把「科目余额 + 报表项配置」算成一张报表。**纯函数**，无 IO。
///
/// # 异常情况
///
/// 返回 `Err` 的只有配置错误（公式引用未知项 / 算式不合法），接口按 500 处理 ——
/// 报表配置错了绝不能静默出 0，那会被人当成"这个月没发生"。
pub fn compute(
    items: &[ReportItemDef],
    accounts: &[AccountBalance],
) -> std::result::Result<ComputedSheet, String> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.item_code.cmp(&b.item_code))
    });

    let mut amounts: HashMap<String, Decimal> = HashMap::new();
    let mut matched: BTreeSet<usize> = BTreeSet::new();
    let mut contributions: HashMap<String, Decimal> = HashMap::new();
    let mut owned: HashMap<String, Vec<String>> = HashMap::new();

    // ---------- 第 1 遍：科目匹配项（不含公式） ----------
    for item in &sorted {
        let Some(pattern) = item.account_pattern.as_deref() else {
            continue;
        };
        let mut sum = Decimal::ZERO;
        let mut codes = Vec::new();
        for (index, account) in accounts.iter().enumerate() {
            if !match_pattern(pattern, account) {
                continue;
            }
            matched.insert(index);
            sum += account.display();
            codes.push(account.account_code.clone());
        }
        codes.sort();
        codes.dedup();
        let sum = zero_norm(sum);
        amounts.insert(item.item_code.clone(), sum);
        owned.insert(item.item_code.clone(), codes);
        let entry = contributions
            .entry(item.side.clone())
            .or_insert(Decimal::ZERO);
        *entry = zero_norm(*entry + if item.is_deduction { -sum } else { sum });
    }

    // ---------- 未归类差额：每侧类别净额 − 已映射金额 ----------
    let mut side_totals: HashMap<String, Decimal> = HashMap::new();
    for side in SIDES {
        side_totals.insert(side.to_string(), Decimal::ZERO);
    }
    for account in accounts {
        if let Some(side) = account.side() {
            *side_totals.entry(side.to_string()).or_insert(Decimal::ZERO) += account.side_econ();
        }
    }
    let mut unmapped: Vec<UnmappedAccountRes> = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        if matched.contains(&index) || account.net().is_zero() {
            continue;
        }
        unmapped.push(UnmappedAccountRes {
            account_code: account.account_code.clone(),
            account_name: account.account_name.clone(),
            account_category: account.account_category.clone(),
            amount: zero_norm(account.display()),
        });
    }
    unmapped.sort_by(|a, b| a.account_code.cmp(&b.account_code));

    for side in SIDES {
        let side_total = side_totals.get(side).copied().unwrap_or_default();
        let mapped = contributions.get(side).copied().unwrap_or_default();
        let residual = zero_norm(side_total - mapped);
        let code = match side {
            "LIABILITY" => UNMAPPED_LIABILITIES,
            "EQUITY" => UNMAPPED_EQUITY,
            _ => UNMAPPED_ASSETS,
        };
        amounts.insert(code.to_string(), residual);
    }

    // ---------- 第 2 遍：公式项（按 sort_order，依赖项必须在前） ----------
    for item in &sorted {
        let Some(formula) = item.formula.as_deref() else {
            continue;
        };
        let value = zero_norm(eval_formula(formula, &amounts)?);
        amounts.insert(item.item_code.clone(), value);
    }

    // ---------- 组装三侧行（含运行时补出来的未归类行） ----------
    let mut assets = Vec::new();
    let mut liabilities = Vec::new();
    let mut equity = Vec::new();
    for item in &sorted {
        if item.item_level == 1 && item.formula.is_none() && item.account_pattern.is_none() {
            // 纯标题行
            let line = ReportLineRes {
                item_code: item.item_code.clone(),
                item_name: item.item_name.clone(),
                level: item.item_level,
                parent_code: item.parent_code.clone(),
                amount: None,
                previous_amount: None,
                is_deduction: item.is_deduction,
                is_total: false,
                detail: None,
                accounts: Vec::new(),
            };
            push_side(&item.side, &mut assets, &mut liabilities, &mut equity, line);
            continue;
        }
        let line = ReportLineRes {
            item_code: item.item_code.clone(),
            item_name: item.item_name.clone(),
            level: item.item_level,
            parent_code: item.parent_code.clone(),
            amount: Some(amounts.get(&item.item_code).copied().unwrap_or_default()),
            previous_amount: None,
            is_deduction: item.is_deduction,
            is_total: item.formula.is_some(),
            detail: item
                .account_pattern
                .clone()
                .or_else(|| item.formula.clone()),
            accounts: owned.get(&item.item_code).cloned().unwrap_or_default(),
        };
        push_side(&item.side, &mut assets, &mut liabilities, &mut equity, line);
    }

    // 未归类行插在**本侧合计行之前**（人看报表时紧挨小计，不会被忽略）
    insert_unmapped(
        &mut assets,
        &amounts,
        UNMAPPED_ASSETS,
        &format!("{REPORT_NAME}·资产"),
    );
    insert_unmapped(
        &mut liabilities,
        &amounts,
        UNMAPPED_LIABILITIES,
        &format!("{REPORT_NAME}·负债"),
    );
    insert_unmapped(
        &mut equity,
        &amounts,
        UNMAPPED_EQUITY,
        &format!("{REPORT_NAME}·权益"),
    );

    Ok(ComputedSheet {
        assets,
        liabilities,
        equity,
        amounts,
        side_totals,
        unmapped,
    })
}

/// 按侧把行塞进对应列表。
fn push_side(
    side: &str,
    assets: &mut Vec<ReportLineRes>,
    liabilities: &mut Vec<ReportLineRes>,
    equity: &mut Vec<ReportLineRes>,
    line: ReportLineRes,
) {
    match side {
        "LIABILITY" => liabilities.push(line),
        "EQUITY" => equity.push(line),
        _ => assets.push(line),
    }
}

/// 把未归类行插到本侧**最后一个合计行之前**（资产侧就是「资产总计」前），找不到就追加。
fn insert_unmapped(
    lines: &mut Vec<ReportLineRes>,
    amounts: &HashMap<String, Decimal>,
    code: &str,
    side_label: &str,
) {
    let amount = amounts.get(code).copied().unwrap_or_default();
    let mut line = unmapped_line(
        match code {
            UNMAPPED_LIABILITIES => "LIABILITY",
            UNMAPPED_EQUITY => "EQUITY",
            _ => "ASSET",
        },
        amount,
    );
    if !amount.is_zero() {
        line.detail = Some(format!(
            "{side_label}：该侧有 {amount} 未归入任何报表项（{}）",
            line.detail.unwrap_or_default()
        ));
    }
    let position = lines
        .iter()
        .rposition(|row| row.is_total || row.item_code.starts_with("TOTAL_"))
        .unwrap_or(lines.len());
    lines.insert(position, line);
}

// ==================== 数据访问 ====================

/// 期间信息。
#[derive(Debug, Clone)]
pub struct PeriodInfo {
    /// 期间编码。
    pub period_code: String,
    /// 状态（`OPEN` / `CLOSED` / 运行时推算的 `DERIVED`）。
    pub status: String,
    /// 开始日。
    pub start_date: NaiveDate,
    /// 结束日。
    pub end_date: NaiveDate,
}

/// 期间状态：库里没有期间行、但该月有已记账凭证时，报表按自然月推算并打这个状态。
///
/// 见 [`resolve_period`] 的说明：报表宁可标「未开账·按自然月推算」也要把数据出出来，
/// 但绝不替一个**空月份**编造报告期。
pub const PERIOD_DERIVED: &str = "DERIVED";

/// 取期间（`biz_accounting_period` 优先；缺失时见 [`resolve_period`]）。
pub async fn load_period(tx: &Transaction, period: &str) -> DbResult<Option<PeriodInfo>> {
    let sql = format!(
        "SELECT period_code, status, start_date, end_date FROM {PERIOD_TABLE} WHERE period_code = ?"
    );
    let row = tx
        .find_with_params(&sql, vec![Value::from(period)])
        .await?
        .into_iter()
        .next();
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(PeriodInfo {
        period_code: row.get_typed("period_code")?,
        status: row.get_typed("status")?,
        start_date: row.get_typed("start_date")?,
        end_date: row.get_typed("end_date")?,
    }))
}

/// 该月窗口内已记账凭证的数量（判断"这个月到底有没有账"）。
pub async fn count_posted_vouchers(
    tx: &Transaction,
    start: NaiveDate,
    end: NaiveDate,
) -> DbResult<i64> {
    let sql = format!(
        "SELECT COUNT(*) AS cnt FROM {VOUCHER_TABLE} \
          WHERE status = '{POSTED}' AND deleted_at IS NULL \
            AND voucher_date >= ? AND voucher_date <= ?"
    );
    let row = tx
        .find_with_params(&sql, vec![Value::from(start), Value::from(end)])
        .await?
        .into_iter()
        .next();
    match row {
        Some(row) => row.get_typed("cnt"),
        None => Ok(0),
    }
}

/// 解析期间：`biz_accounting_period` 优先；**缺失时按"这个月有没有账"分流**。
///
/// ```text
/// 有期间行                    → 用它（OPEN / CLOSED 都行，月结拉长/缩短期间也照样按库里的起止日）
/// 无期间行 且 该月有已记账凭证 → 按自然月推算，status = DERIVED，并给出「请尽快开账」的提示
/// 无期间行 且 该月没有任何凭证 → Err（400 + 66010）
/// ```
///
/// # 为什么不是"一律 400"（与卡面 ⑧ 的差异，务必读完）
///
/// 生产库实测（2026-11-01）：`biz_voucher` 里有 2 张 **2026-11** 的凭证，而
/// `biz_accounting_period` 只有 2026-10 —— 严格 400 会让"有账的月份出不了表"，
/// 这比"没开账就出表"危害大得多（用户会以为账丢了）。
/// 反过来，给一个**空月份**（没有任何凭证）编造报告期才是真正的静默错误：
/// 用户会以为"那个月真的没发生业务"。所以按"有没有账"分流，
/// 空月份仍然 400 + 66010（卡面 ⑧ 的场景被完整保留）。
pub async fn resolve_period(tx: &Transaction, period: &str) -> DbResult<PeriodInfo> {
    if let Some(info) = load_period(tx, period).await? {
        return Ok(info);
    }
    let Some((start, end)) = month_range(period) else {
        return Err(DbError::InvalidValue(format!(
            "period 必须是 yyyy-MM，收到「{period}」"
        )));
    };
    if count_posted_vouchers(tx, start, end).await? == 0 {
        return Err(DbError::InvalidValue(format!(
            "会计期间 {period} 不存在（biz_accounting_period 无记录），且该月没有任何已记账凭证：\
             报表不替空月份编造报告期，请先在「会计期间」里开启该期间"
        )));
    }
    Ok(PeriodInfo {
        period_code: period.to_string(),
        status: PERIOD_DERIVED.to_string(),
        start_date: start,
        end_date: end,
    })
}

/// 科目余额快照（期初 + 本期，全科目一次查完）。
///
/// 两条子查询都用 `status = 'POSTED' AND deleted_at IS NULL`，与总账 / 余额表同一口径。
///
/// # 期初凭证（`voucher_type = 'OPENING'`）怎么算（ERP-R5）
///
/// ```text
/// 「期初」列（net_before）= 期间开始日之前的历史累计
///                        + 本期间自己的期初凭证（它的凭证日期就是期间第一天）
/// 「本期」列（net_period）= [期间开始日, 期间结束日] 内的凭证，**排除期初凭证**
/// 期末 = net_before + net_period  ← 于是「期末 = 期初 + 本期」恒成立
/// ```
///
/// 为什么不能只把期初凭证当普通凭证算进本期：硬红线「期初凭证不计入本期发生额」，
/// 利润表直接复用本函数取「本期 / 本年累计」，一旦期初混进本期，损益发生额就被污染。
///
/// `opening_period` = **报表所属期间**（`yyyy-MM`）。报表与期间不是一一对应时
/// （利润表的「本年累计」窗口从 1 月 1 日起算）传同一期间，保证期初在两边口径一致：
/// 利润表的「期末累计 − 年初之前」正好把期初抵消掉，勾稽不会因为期初而假红。
pub async fn load_accounts(
    tx: &Transaction,
    start: NaiveDate,
    end: NaiveDate,
    opening_period: Option<&str>,
) -> DbResult<Vec<AccountBalance>> {
    let opening_period = opening_period.unwrap_or_default();
    let sql = format!(
        "SELECT a.id, a.account_code, a.account_name, a.account_category, a.balance_direction, \
                a.is_leaf, a.level, \
                COALESCE(o.amount, 0) AS net_before, \
                COALESCE(p.amount, 0) AS net_period \
           FROM {ACCOUNT_TABLE} a \
           LEFT JOIN ( \
                SELECT e.account_id, SUM(e.debit_amount - e.credit_amount) AS amount \
                  FROM {ENTRY_TABLE} e JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
                 WHERE v.status = '{POSTED}' AND v.deleted_at IS NULL \
                   AND (v.voucher_date < ? \
                        OR (v.voucher_type = 'OPENING' AND v.period = NULLIF(?::text, ''))) \
                 GROUP BY e.account_id \
           ) o ON o.account_id = a.id \
           LEFT JOIN ( \
                SELECT e.account_id, SUM(e.debit_amount - e.credit_amount) AS amount \
                  FROM {ENTRY_TABLE} e JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
                 WHERE v.status = '{POSTED}' AND v.deleted_at IS NULL \
                   AND v.voucher_type <> 'OPENING' \
                   AND v.voucher_date >= ? AND v.voucher_date <= ? \
                 GROUP BY e.account_id \
           ) p ON p.account_id = a.id \
          WHERE a.deleted_at IS NULL \
          ORDER BY a.account_code"
    );
    let rows = tx
        .find_with_params(
            &sql,
            vec![
                Value::from(start),
                Value::Text(opening_period.to_string()),
                Value::from(start),
                Value::from(end),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(AccountBalance {
            account_id: row.get_typed("id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_category: row.get_typed("account_category")?,
            balance_direction: row.get_typed("balance_direction")?,
            is_leaf: row.get_typed::<i16>("is_leaf")? == 1,
            level: row.get_typed("level")?,
            net_before: row.get_typed("net_before")?,
            net_period: row.get_typed("net_period")?,
        });
    }
    Ok(out)
}

/// 报表项配置。
pub async fn load_items(tx: &Transaction) -> DbResult<Vec<ReportItemDef>> {
    load_items_of(tx, REPORT_CODE).await
}

/// 任意报表的报表项配置（`report_code` 由调用方给）。
///
/// 利润表（ERP-R2）复用同一张 `biz_report_item` 与同一套匹配 / 求值逻辑，
/// 只有 `report_code` 不同 —— 所以这里按编码取，而不是各写一条 SQL。
pub async fn load_items_of(tx: &Transaction, report_code: &str) -> DbResult<Vec<ReportItemDef>> {
    let sql = format!(
        "SELECT item_code, item_name, item_level, parent_code, side, account_pattern, formula, \
                is_deduction, sort_order \
           FROM {ITEM_TABLE} WHERE report_code = ? AND is_active = 1 \
          ORDER BY sort_order, item_code"
    );
    let rows = tx
        .find_with_params(&sql, vec![Value::from(report_code)])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ReportItemDef {
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            item_level: row.get_typed("item_level")?,
            parent_code: row.get_typed("parent_code").ok(),
            side: row.get_typed("side")?,
            account_pattern: row.get_typed("account_pattern").ok(),
            formula: row.get_typed("formula").ok(),
            is_deduction: row.get_typed::<bool>("is_deduction")?,
            sort_order: row.get_typed("sort_order")?,
        });
    }
    Ok(out)
}

/// 窗口内分录的借贷合计（诊断用）。
pub async fn load_ledger_totals(
    tx: &Transaction,
    start: NaiveDate,
    end: NaiveDate,
) -> DbResult<(Decimal, Decimal)> {
    let sql = format!(
        "SELECT COALESCE(SUM(e.debit_amount), 0) AS debit, COALESCE(SUM(e.credit_amount), 0) AS credit \
           FROM {ENTRY_TABLE} e JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
          WHERE v.status = '{POSTED}' AND v.deleted_at IS NULL \
            AND v.voucher_date >= ? AND v.voucher_date <= ?"
    );
    let row = tx
        .find_with_params(&sql, vec![Value::from(start), Value::from(end)])
        .await?
        .into_iter()
        .next();
    match row {
        Some(row) => Ok((row.get_typed("debit")?, row.get_typed("credit")?)),
        None => Ok((Decimal::ZERO, Decimal::ZERO)),
    }
}

/// 分录合计 ≠ 单头合计的凭证（不平的直接证据，最多 20 条）。
pub async fn load_unbalanced_vouchers(
    tx: &Transaction,
    start: NaiveDate,
    end: NaiveDate,
) -> DbResult<Vec<UnbalancedVoucherRes>> {
    let sql = format!(
        "SELECT v.voucher_no, v.voucher_date, v.total_debit AS header_debit, \
                v.total_credit AS header_credit, \
                COALESCE(SUM(e.debit_amount), 0) AS entry_debit, \
                COALESCE(SUM(e.credit_amount), 0) AS entry_credit \
           FROM {VOUCHER_TABLE} v LEFT JOIN {ENTRY_TABLE} e ON e.voucher_id = v.id \
          WHERE v.status = '{POSTED}' AND v.deleted_at IS NULL \
            AND v.voucher_date >= ? AND v.voucher_date <= ? \
          GROUP BY v.id, v.voucher_no, v.voucher_date, v.total_debit, v.total_credit \
         HAVING COALESCE(SUM(e.debit_amount), 0) <> v.total_debit \
             OR COALESCE(SUM(e.credit_amount), 0) <> v.total_credit \
             OR COALESCE(SUM(e.debit_amount), 0) <> COALESCE(SUM(e.credit_amount), 0) \
          ORDER BY v.voucher_date, v.voucher_no \
          LIMIT 20"
    );
    let rows = tx
        .find_with_params(&sql, vec![Value::from(start), Value::from(end)])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(UnbalancedVoucherRes {
            voucher_no: row.get_typed("voucher_no")?,
            voucher_date: row.get_typed("voucher_date")?,
            entry_debit: row.get_typed("entry_debit")?,
            entry_credit: row.get_typed("entry_credit")?,
            header_debit: row.get_typed("header_debit")?,
            header_credit: row.get_typed("header_credit")?,
        });
    }
    Ok(out)
}

// ==================== 组装（IO + 计算） ====================

/// 一次报表计算的完整产物（接口层用它拼返回体）。
#[derive(Debug)]
pub struct SheetSnapshot {
    /// 计算结果。
    pub sheet: ComputedSheet,
    /// 期间。
    pub period: PeriodInfo,
    /// 上期期间（有期间行才算）。
    pub previous_period: Option<String>,
    /// 上期计算结果（用于逐行对比）。
    pub previous_sheet: Option<ComputedSheet>,
    /// 窗口内分录借方合计。
    pub ledger_debit_total: Decimal,
    /// 窗口内分录贷方合计。
    pub ledger_credit_total: Decimal,
    /// 分录不平的凭证。
    pub unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
}

/// 期初余额的借贷合计（ERP-R5）。不平 → 报表不许出（卡面 ⑫）。
pub async fn load_opening_totals(tx: &Transaction, period: &str) -> DbResult<(Decimal, Decimal)> {
    // SCOPE-EXEMPT: 期初台账无归属列（见 opening 模块文档）。
    let sql = format!(
        "SELECT COALESCE(SUM(debit_opening), 0)::numeric(20,4) AS debit, \
                COALESCE(SUM(credit_opening), 0)::numeric(20,4) AS credit \
           FROM {} WHERE period_code = ?",
        crate::business::erp::opening::TABLE_BALANCE
    );
    let row = tx
        .find_with_params(&sql, vec![Value::from(period)])
        .await?
        .into_iter()
        .next();
    match row {
        Some(row) => Ok((row.get_typed("debit")?, row.get_typed("credit")?)),
        None => Ok((Decimal::ZERO, Decimal::ZERO)),
    }
}

/// 在给定事务（**必须是 REPEATABLE READ 快照**）里算出一整张资产负债表。
///
/// # 异常情况
///
/// * 期间不存在 → `Err(DbError::InvalidValue)`，接口层翻成 400 + 66010；
/// * 报表项配置错（公式引用未知项）→ `Err(DbError::InvalidValue)`，接口层翻成 500。
pub async fn build_snapshot(tx: &Transaction, period: &str) -> DbResult<SheetSnapshot> {
    let info = resolve_period(tx, period).await?;
    let items = load_items(tx).await?;
    if items.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "报表项配置为空（{ITEM_TABLE}.report_code = '{REPORT_CODE}' 一行都没有），无法编制报表"
        )));
    }

    // ERP-R5 卡面 ⑫：期初不平衡 → **不许出表**。
    // 期初不平衡意味着「这套账从第一天就是歪的」，出表只会把错误包装成结论；
    // 唯一的例外路径是「保存时被 400 拦下」，所以真到这里说明数据是绕过接口写进来的
    // （手工改库 / 未完成的迁移），必须让人看见而不是静默出一张差表。
    let (opening_debit, opening_credit) = load_opening_totals(tx, period).await?;
    if opening_debit != opening_credit {
        return Err(DbError::InvalidValue(format!(
            "期初余额不平衡，不允许出表：借方合计 {opening_debit} / 贷方合计 {opening_credit}，\
             差额 {}。请先到「期初余额」页把期初调平（借 = 贷）再编制报表",
            opening_debit - opening_credit
        )));
    }

    let accounts = load_accounts(tx, info.start_date, info.end_date, Some(period)).await?;
    let sheet = compute(&items, &accounts).map_err(DbError::InvalidValue)?;
    let (ledger_debit_total, ledger_credit_total) =
        load_ledger_totals(tx, info.start_date, info.end_date).await?;
    let unbalanced_vouchers = load_unbalanced_vouchers(tx, info.start_date, info.end_date).await?;

    // 上期对比：只有**建过期间行**的上期才出对比（报表不替用户"推导"一个不存在的报告期）
    let previous_code = previous_period_of(period);
    let mut previous_sheet = None;
    let mut previous_period = None;
    if let Some(code) = previous_code.as_deref()
        && let Some(previous_info) = load_period(tx, code).await?
    {
        let previous_accounts = load_accounts(
            tx,
            previous_info.start_date,
            previous_info.end_date,
            Some(&previous_info.period_code),
        )
        .await?;
        previous_sheet = Some(compute(&items, &previous_accounts).map_err(DbError::InvalidValue)?);
        previous_period = Some(previous_info.period_code);
    }

    Ok(SheetSnapshot {
        sheet,
        period: info,
        previous_period,
        previous_sheet,
        ledger_debit_total,
        ledger_credit_total,
        unbalanced_vouchers,
    })
}

/// 差额的人话文本。
///
/// 正常按 2 位小数；但**差额不为 0 而 2 位小数会被舍成 0.00** 时（4 位标度的数值账），
/// 保留 4 位 —— 硬红线 4 不允许"四舍五入抹平"。
pub fn diff_text(diff: Decimal) -> String {
    if diff.is_zero() {
        return "0.00".to_string();
    }
    let rounded = diff.round_dp(2);
    if rounded.is_zero() {
        // 显式 `round_dp` 后再定点格式化：`{:.2}` 直接打 Decimal 是**截断**，
        // 12.3456 会打成 12.34（单测 R1-⑦ 抓到的坑）
        format!("{:.4}", diff.round_dp(4))
    } else {
        format!("{rounded:.2}")
    }
}

/// 勾稽校验（纯逻辑：只吃计算结果与诊断数据）。
pub fn build_check(
    sheet: &ComputedSheet,
    ledger_debit_total: Decimal,
    ledger_credit_total: Decimal,
    unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
) -> BalanceCheckRes {
    let assets_total = sheet.assets_total();
    let liability_equity_total = sheet.liability_equity_total();
    let diff = zero_norm(assets_total - liability_equity_total);
    let balanced = diff.is_zero();
    let mut hints: Vec<String> = Vec::new();

    // 「未归类」三行不为 0 本身就是配置缺陷（哪怕报表是平的）：钱被兜底行吃掉了，
    // 报表上的分类是错的。三行都被算进本侧合计，所以不平衡问题不会因此被掩盖。
    for (code, label) in [
        (UNMAPPED_ASSETS, "资产侧"),
        (UNMAPPED_LIABILITIES, "负债侧"),
        (UNMAPPED_EQUITY, "权益侧"),
    ] {
        let amount = sheet.amounts.get(code).copied().unwrap_or_default();
        if !amount.is_zero() {
            hints.push(format!(
                "{label}有 {amount} 落在「未归类」行：该侧类别净额与已映射报表项合计不等 —— \
                 多半是新增科目没有匹配任何 account_pattern，或该侧小计的加减关系漏了某项"
            ));
        }
    }

    if !balanced {
        let ledger_gap = ledger_debit_total - ledger_credit_total;
        if !ledger_gap.is_zero() {
            hints.push(format!(
                "本期分录借贷不等：Σ借 {ledger_debit_total} ≠ Σ贷 {ledger_credit_total}（差 {ledger_gap}）—— \
                 最可能是某张凭证的分录行被删/被改（单头的 total_debit/total_credit 不会被连带修改）"
            ));
        }
        if !unbalanced_vouchers.is_empty() {
            let nos: Vec<&str> = unbalanced_vouchers
                .iter()
                .take(5)
                .map(|row| row.voucher_no.as_str())
                .collect();
            hints.push(format!(
                "分录合计与单头不符的凭证：{}（共 {} 张，逐张核对分录行）",
                nos.join("、"),
                unbalanced_vouchers.len()
            ));
        }
        if !sheet.unmapped.is_empty() {
            let codes: Vec<&str> = sheet
                .unmapped
                .iter()
                .take(8)
                .map(|row| row.account_code.as_str())
                .collect();
            hints.push(format!(
                "有 {} 个科目没有匹配任何报表项：{}（检查 biz_report_item.account_pattern）",
                sheet.unmapped.len(),
                codes.join("、")
            ));
        }
        hints.push(
            "检查科目类别（biz_account.account_category）：把资产类配成负债类、或把损益类配成资产类，\
             都会让一侧多出同额金额"
                .to_string(),
        );
    }

    let message = if balanced {
        format!("平衡：资产 {assets_total} = 负债和所有者权益 {liability_equity_total}")
    } else {
        format!(
            "不平：资产 {assets_total} ≠ 负债和所有者权益 {liability_equity_total}，差 {} 元（差额不做舍入）",
            diff_text(diff)
        )
    };

    BalanceCheckRes {
        balanced,
        assets_total,
        liability_equity_total,
        diff,
        diff_text: diff_text(diff),
        message,
        ledger_debit_total,
        ledger_credit_total,
        unmapped: sheet.unmapped.clone(),
        unbalanced_vouchers,
        hints,
    }
}

/// 给每一行补上上期金额。
pub fn attach_previous(sheet: &mut ComputedSheet, previous: Option<&ComputedSheet>) {
    let Some(previous) = previous else {
        return;
    };
    for lines in [&mut sheet.assets, &mut sheet.liabilities, &mut sheet.equity] {
        for line in lines.iter_mut() {
            line.previous_amount = previous.amounts.get(&line.item_code).copied();
        }
    }
}

/// 组装接口返回体（纯逻辑：IO 结果进来，JSON 出去）。
pub fn into_response(
    report_code: &str,
    report_name: &str,
    company_name: &str,
    unit: &str,
    snapshot: SheetSnapshot,
) -> BalanceSheetRes {
    let SheetSnapshot {
        mut sheet,
        period,
        previous_period,
        previous_sheet,
        ledger_debit_total,
        ledger_credit_total,
        unbalanced_vouchers,
    } = snapshot;
    attach_previous(&mut sheet, previous_sheet.as_ref());
    let mut check = build_check(
        &sheet,
        ledger_debit_total,
        ledger_credit_total,
        unbalanced_vouchers,
    );
    // 期间是按自然月推算出来的（库里没有期间行，但该月有账）：必须让看报表的人知道
    if period.status == PERIOD_DERIVED {
        check.hints.push(format!(
            "会计期间 {period_code} 未在「会计期间」里建立，本表按自然月（{start} ~ {end}）推算编制；\
             请尽快开账，否则月结 / 期间锁定 / 审计口径都对不上",
            period_code = period.period_code,
            start = period.start_date,
            end = period.end_date
        ));
    }
    BalanceSheetRes {
        report_code: report_code.to_string(),
        report_name: report_name.to_string(),
        company_name: company_name.to_string(),
        unit: unit.to_string(),
        period: period.period_code.clone(),
        period_name: period_name_of(&period.period_code),
        period_status: period.status,
        period_start: period.start_date,
        period_end: period.end_date,
        generated_at: Utc::now(),
        previous_period,
        assets: sheet.assets,
        liabilities: sheet.liabilities,
        equity: sheet.equity,
        check,
    }
}

// ==================== 接口层 ====================

/// 参数校验（纯逻辑）：返回规整后的期间编码。
fn validate_period(raw: Option<&str>) -> std::result::Result<String, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "period 必填，格式 yyyy-MM（例：2026-10）".to_string(),
        ));
    };
    if parse_period(text).is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("period 必须是 yyyy-MM（月份 01–12），收到「{text}」"),
        ));
    }
    Ok(text.to_string())
}

/// 在 REPEATABLE READ 快照里算报表，并把数据层错误翻成 HTTP 错误。
async fn snapshot_or_error(period: &str) -> std::result::Result<SheetSnapshot, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启资产负债表快照事务"))?;
    // ★ 硬红线 3：整个报表在一个 REPEATABLE READ 快照里算完。
    //   `SET TRANSACTION` 必须是 BEGIN 之后的第一条语句（PG 规定，否则 25001）。
    if let Err(error) = tx
        .execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .await
    {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "设置报表事务隔离级别"));
    }
    match build_snapshot(&tx, period).await {
        Ok(snapshot) => {
            if let Err(error) = tx.commit().await {
                return Err(utils::db_error(error, "提交资产负债表快照事务"));
            }
            Ok(snapshot)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            match error {
                DbError::InvalidValue(message) => Err(period_error(period, message)),
                other => Err(utils::db_error(other, "编制资产负债表")),
            }
        }
    }
}

/// 单测用：把「数据层错误消息」翻成 HTTP 错误（与 [`snapshot_or_error`] 同一口径）。
///
/// 生产路径上这段映射发生在 `snapshot_or_error` 里；单测要证明「期初不平衡 = 400 + 66012」
/// 而不是 500，就得能单独调到这条映射，所以把它开成 `cfg(test)` 的入口。
#[cfg(test)]
pub fn period_error_for_test(period: &str, message: &str) -> ApiError {
    period_error(period, message.to_string())
}

/// 数据层「期间不存在 / 期初不平衡 / 配置错」→ 400 / 400+66012 / 500。
fn period_error(period: &str, message: String) -> ApiError {
    if message.contains("期初") && message.contains("不平衡") {
        return ApiError::new(400, error_code::ERP_OPENING_UNBALANCED, message);
    }
    if message.contains("不存在") {
        ApiError::new(400, error_code::ERP_PERIOD_NOT_FOUND, message)
    } else {
        ApiError::new(
            500,
            error_code::ERP_REPORT_CONFIG_INVALID,
            format!("{message}（period={period}）"),
        )
    }
}

/// 编制单位（取系统配置里的系统名，取不到就用产品名）。
async fn company_name() -> String {
    let sql = "SELECT COALESCE(NULLIF(config_value, ''), '云枢') AS name \
                 FROM sys_config WHERE config_key = 'system.name' AND deleted_at IS NULL \
                LIMIT 1";
    match db().find(sql).await {
        Ok(rows) => rows
            .first()
            .and_then(|row| row.get_typed::<String>("name").ok())
            .unwrap_or_else(|| "云枢".to_string()),
        Err(_) => "云枢".to_string(),
    }
}

/// 资产负债表。
#[nik::permission(
    path = "/business/erp/report/balance-sheet",
    method = "GET",
    permission = "business:erp:report:balance-sheet"
)]
#[nik::get("/business/erp/report/balance-sheet")]
pub async fn balance_sheet_handler(
    Query(query): Query<BalanceSheetQuery>,
) -> ResResult<BalanceSheetRes> {
    let period = validate_period(query.period.as_deref())?;
    let snapshot = snapshot_or_error(&period).await?;
    let company = company_name().await;
    Ok(Res::ok(into_response(
        REPORT_CODE,
        REPORT_NAME,
        &company,
        "元",
        snapshot,
    )))
}

/// 资产负债表导出（xlsx，复用 impexp 的表格写出能力）。
///
/// 为什么不走 `business/impexp` 的注册表：那条路的筛选条件只有 `keyword` / `status`
/// （`definition::ExportFilter`），**没有期间维度**；把期间塞进 keyword 属于滥用过滤器，
/// 下一个人接手必然踩坑。报表也不是"表扫描 + keyset 分页"，行数固定几十行 →
/// 直接复用 `impexp::engine::write_sheet`（同一个 xlsx 写出器、同一套错误码）。
#[nik::permission(
    path = "/business/erp/report/balance-sheet/export",
    method = "GET",
    permission = "business:erp:report:export"
)]
#[nik::get("/business/erp/report/balance-sheet/export")]
pub async fn balance_sheet_export_handler(
    Query(query): Query<BalanceSheetQuery>,
) -> std::result::Result<Response, ApiError> {
    let period = validate_period(query.period.as_deref())?;
    let snapshot = snapshot_or_error(&period).await?;
    let company = company_name().await;
    let report = into_response(REPORT_CODE, REPORT_NAME, &company, "元", snapshot);
    let (headers, rows) = export_table(&report);
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows)?;
    Ok(crate::business::impexp::handler::xlsx_response(
        bytes,
        &format!(
            "{REPORT_NAME}_{}_{}.xlsx",
            report.period,
            Local::now().format("%Y%m%d%H%M%S")
        ),
    ))
}

/// 报表项在纸面上的显示名：减项要带「减：」前缀，但**只加一次**。
///
/// V0183 种的 `ACCUM_DEPRECIATION` / `ACCUM_AMORTIZATION` 名字里已经有「减：」，
/// 再拼一次就成「减：减：累计折旧」（前端首测时真出现了）。所以按名字判断，
/// 名字自己带了就不再补。
pub fn display_name(line: &ReportLineRes) -> String {
    if line.is_deduction && !line.item_name.starts_with("减：") {
        format!("减：{}", line.item_name)
    } else {
        line.item_name.clone()
    }
}

/// 报表金额的人话格式：千分位 + 2 位小数；`None`（标题行 / 无数据）给空串。
///
/// 先 `round_dp(2)` 再 `{:.2}` 定点打印：`{:.2}` 直接格式化 `Decimal` 是**截断**
/// （12.3456 → 12.34），导出件少一分钱是事故。利润表导出复用同一个函数，
/// 两张报表的金额写法必须一模一样。
pub fn money_text(value: Option<Decimal>) -> String {
    match value {
        Some(value) => {
            let text = format!("{:.2}", value.round_dp(2));
            let (integer, fraction) = text.split_once('.').unwrap_or((text.as_str(), "00"));
            let negative = integer.starts_with('-');
            let digits: String = integer.chars().filter(|c| c.is_ascii_digit()).collect();
            let grouped = digits
                .as_bytes()
                .rchunks(3)
                .rev()
                .map(|chunk| String::from_utf8_lossy(chunk).to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("{}{grouped}.{fraction}", if negative { "-" } else { "" })
        }
        None => String::new(),
    }
}

/// 把报表拍成「左资产 / 右负债权益」两栏的表格行（与纸面报表同构）。
///
/// 列：`报表项(资产) | 期末余额 | 上期余额 | 报表项(负债和权益) | 期末余额 | 上期余额`。
pub fn export_table(report: &BalanceSheetRes) -> (Vec<&'static str>, Vec<HashMap<String, String>>) {
    const HEADERS: [&str; 6] = [
        "资产",
        "期末余额",
        "上期余额",
        "负债和所有者权益",
        "期末余额",
        "上期余额",
    ];
    let right: Vec<&ReportLineRes> = report
        .liabilities
        .iter()
        .chain(report.equity.iter())
        .collect();
    let mut rows: Vec<HashMap<String, String>> = Vec::new();
    // 表头信息（编制单位 / 期间 / 单位）也进表格，导出的文件脱离页面也能看懂。
    // 注意 HashMap 一个 key 只能有一个值：三行信息各占一列，不重复用列名。
    let mut info = HashMap::new();
    info.insert(
        "资产".to_string(),
        format!("编制单位：{}", report.company_name),
    );
    info.insert(
        "期末余额".to_string(),
        format!("期间：{}", report.period_name),
    );
    info.insert(
        "负债和所有者权益".to_string(),
        format!("单位：{}", report.unit),
    );
    rows.push(info);

    let indent = |level: i16| "    ".repeat(usize::try_from(level.saturating_sub(1)).unwrap_or(0));

    for index in 0..report.assets.len().max(right.len()) {
        let mut row: HashMap<String, String> = HashMap::new();
        if let Some(left) = report.assets.get(index) {
            row.insert(
                "资产".to_string(),
                format!("{}{}", indent(left.level), display_name(left)),
            );
            row.insert("期末余额".to_string(), money_text(left.amount));
            row.insert("上期余额".to_string(), money_text(left.previous_amount));
        }
        if let Some(right_line) = right.get(index) {
            row.insert(
                "负债和所有者权益".to_string(),
                format!("{}{}", indent(right_line.level), display_name(right_line)),
            );
            row.insert("期末余额".to_string(), money_text(right_line.amount));
            row.insert(
                "上期余额".to_string(),
                money_text(right_line.previous_amount),
            );
        }
        rows.push(row);
    }

    // 结论行：平衡 / 不平（不平的把差额写进文件，导出件也瞒不住）
    let mut tail = HashMap::new();
    tail.insert("资产".to_string(), report.check.message.clone());
    rows.push(tail);
    // 上表的 key 顺序由 headers 决定，这里补齐缺的列不会串列（write_sheet 按 headers 取值）。
    (HEADERS.to_vec(), rows)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher_query::tests::{DB_LOCK, ensure_db};
    use std::str::FromStr;

    /// 造一个科目（报表计算用，不进库）。
    fn account(
        code: &str,
        category: &str,
        direction: &str,
        net_before: i64,
        net_period: i64,
    ) -> AccountBalance {
        AccountBalance {
            account_id: Uuid::nil(),
            account_code: code.to_string(),
            account_name: format!("科目{code}"),
            account_category: category.to_string(),
            balance_direction: direction.to_string(),
            is_leaf: true,
            level: 1,
            net_before: Decimal::from(net_before),
            net_period: Decimal::from(net_period),
        }
    }

    /// 造一个报表项（报表计算用，不进库）。
    fn item(
        code: &str,
        side: &str,
        level: i16,
        pattern: Option<&str>,
        formula: Option<&str>,
        deduction: bool,
        order: i32,
    ) -> ReportItemDef {
        ReportItemDef {
            item_code: code.to_string(),
            item_name: code.to_string(),
            item_level: level,
            parent_code: None,
            side: side.to_string(),
            account_pattern: pattern.map(str::to_string),
            formula: formula.map(str::to_string),
            is_deduction: deduction,
            sort_order: order,
        }
    }

    /// 最小可用的资产负债表结构（资产 / 负债各一项 + 总计），用于纯计算用例。
    fn mini_items() -> Vec<ReportItemDef> {
        vec![
            item("CASH", "ASSET", 2, Some("1001,1002"), None, false, 10),
            item("INVENTORY", "ASSET", 2, Some("1405"), None, false, 11),
            item("FIXED_ORIGINAL", "ASSET", 2, Some("1601"), None, false, 12),
            item("ACCUM_DEP", "ASSET", 2, Some("1602"), None, true, 13),
            item(
                "ASSETS_SUB",
                "ASSET",
                2,
                None,
                Some("CASH + INVENTORY + FIXED_ORIGINAL - ACCUM_DEP"),
                false,
                14,
            ),
            item(
                "TOTAL_ASSETS",
                "ASSET",
                1,
                None,
                Some("ASSETS_SUB + UNMAPPED_ASSETS"),
                false,
                15,
            ),
            item("AP", "LIABILITY", 2, Some("2202"), None, false, 20),
            item(
                "TOTAL_LIABILITIES",
                "LIABILITY",
                1,
                None,
                Some("AP + UNMAPPED_LIABILITIES"),
                false,
                21,
            ),
            item("CAPITAL", "EQUITY", 2, Some("3001"), None, false, 30),
            item(
                "PROFIT",
                "EQUITY",
                2,
                Some("@CATEGORY:PROFIT_LOSS,3104"),
                None,
                false,
                31,
            ),
            item(
                "TOTAL_EQUITY",
                "EQUITY",
                2,
                None,
                Some("CAPITAL + PROFIT + UNMAPPED_EQUITY"),
                false,
                32,
            ),
            item(
                "TOTAL_LIABILITIES_AND_EQUITY",
                "EQUITY",
                1,
                None,
                Some("TOTAL_LIABILITIES + TOTAL_EQUITY"),
                false,
                33,
            ),
        ]
    }

    // ---------- ① 纯逻辑：匹配模式 ----------

    fn assert_match(pattern: &str, account: &AccountBalance, expected: bool) {
        assert_eq!(
            match_pattern(pattern, account),
            expected,
            "pattern={pattern} code={} category={}",
            account.account_code,
            account.account_category
        );
    }

    /// ① 匹配模式四种写法：编码清单 / 编码+下级 / 前缀 / 按类别。
    #[test]
    fn r1_pattern_syntax_covers_four_forms() {
        let cash = account("1001", "ASSET", "DEBIT", 0, 0);
        let tax = account("2221", "LIABILITY", "CREDIT", 0, 0);
        let tax_in = account("2221.01", "LIABILITY", "CREDIT", 0, 0);
        let revenue = account("5001", "PROFIT_LOSS", "CREDIT", 0, 0);

        assert_match("1001,1002", &cash, true);
        assert_match("1001,1002", &tax, false);
        // 编码 + 全部下级
        assert_match("2221", &tax, true);
        assert_match("2221", &tax_in, true);
        assert_match("2221.01", &tax, false);
        // 前缀
        assert_match("22*", &tax, true);
        assert_match("1%", &cash, true);
        assert_match("1%", &revenue, false);
        // 类别
        assert_match("@CATEGORY:PROFIT_LOSS", &revenue, true);
        assert_match("@CATEGORY:PROFIT_LOSS", &cash, false);
        println!("[R1-①] 匹配模式 4 种写法 ✓");
    }

    // ---------- ② 纯逻辑：只有货币资金 → 资产 = 负债+权益 ----------

    /// ② 只有货币资金（实收资本投入）：资产 = 负债+权益，diff = 0。
    #[test]
    fn r1_only_cash_balances_with_capital() {
        let accounts = vec![
            // 借 1002 银行存款 1,000 / 贷 3001 实收资本 1,000
            account("1002", "ASSET", "DEBIT", 0, 1_000),
            account("3001", "EQUITY", "CREDIT", 0, -1_000),
        ];
        let sheet = compute(&mini_items(), &accounts).expect("计算");
        assert_eq!(sheet.assets_total(), Decimal::from(1_000));
        assert_eq!(sheet.liability_equity_total(), Decimal::from(1_000));
        assert!(sheet.diff().is_zero(), "应当平衡");
        assert_eq!(
            line(&sheet.assets, "CASH").amount,
            Some(Decimal::from(1_000)),
            "货币资金 1,000"
        );
        println!("[R1-②] 只有货币资金 + 实收资本 → 平衡 ✓");
    }

    // ---------- ③ 纯逻辑：存货 + 应付 ----------

    /// ③ 存货 + 应付：资产 = 存货，负债 = 应付，权益 = 差额（这里是 0）。
    #[test]
    fn r1_inventory_and_payable_split_into_two_sides() {
        let accounts = vec![
            account("1405", "ASSET", "DEBIT", 0, 800),
            account("2202", "LIABILITY", "CREDIT", 0, -800),
        ];
        let sheet = compute(&mini_items(), &accounts).expect("计算");
        assert_eq!(sheet.assets_total(), Decimal::from(800));
        assert_eq!(sheet.liability_equity_total(), Decimal::from(800));
        let inventory = sheet
            .assets
            .iter()
            .find(|row| row.item_code == "INVENTORY")
            .expect("存货行");
        assert_eq!(inventory.amount, Some(Decimal::from(800)));
        let ap = sheet
            .liabilities
            .iter()
            .find(|row| row.item_code == "AP")
            .expect("应付行");
        assert_eq!(ap.amount, Some(Decimal::from(800)));
        println!("[R1-③] 存货 800 + 应付 800 → 两侧各 800 ✓");
    }

    // ---------- ④ 纯逻辑：累计折旧作减项 ----------

    /// ④ 备抵科目：固定资产净值 = 原价 − 累计折旧，且累计折旧行金额为正数（贷方余额）。
    #[test]
    fn r1_accumulated_depreciation_is_a_deduction() {
        let accounts = vec![
            // 借 1601 10,000 / 贷 1602 3,000；贷 3001 7,000
            account("1601", "ASSET", "DEBIT", 0, 10_000),
            account("1602", "ASSET", "CREDIT", 0, -3_000),
            account("3001", "EQUITY", "CREDIT", 0, -7_000),
        ];
        let sheet = compute(&mini_items(), &accounts).expect("计算");
        let original = sheet
            .assets
            .iter()
            .find(|row| row.item_code == "FIXED_ORIGINAL")
            .expect("原价行");
        let depreciation = sheet
            .assets
            .iter()
            .find(|row| row.item_code == "ACCUM_DEP")
            .expect("折旧行");
        assert_eq!(original.amount, Some(Decimal::from(10_000)));
        assert_eq!(
            depreciation.amount,
            Some(Decimal::from(3_000)),
            "累计折旧行取贷方余额，显示为正数"
        );
        assert!(depreciation.is_deduction, "必须标记为减项");
        // 净值在 ASSETS_SUB 里已经体现：10,000 − 3,000
        assert_eq!(sheet.assets_total(), Decimal::from(7_000), "净额 7,000");
        assert_eq!(sheet.liability_equity_total(), Decimal::from(7_000));
        assert!(sheet.diff().is_zero());
        println!("[R1-④] 累计折旧 3,000 作减项 → 资产净额 7,000 ✓");
    }

    // ---------- ⑤ 纯逻辑：多个应交税费明细汇总 ----------

    /// ⑤ 2221.01 / 2221.02 两个明细汇总到 2221 这一项。
    ///
    /// 口径：`net` 是「借 − 贷」，负债类 `display = −net`。
    /// 进项税（借方 100）⇒ display −100；销项税（贷方 400）⇒ display +400 ⇒ 应交税费 = 300。
    #[test]
    fn r1_tax_details_roll_up_into_tax_item() {
        // 把 mini 结构里的「负债合计」换成含应交税费的版本（避免两个同名项）
        let mut items: Vec<ReportItemDef> = mini_items()
            .into_iter()
            .filter(|row| row.item_code != "TOTAL_LIABILITIES")
            .collect();
        items.push(item("TAX", "LIABILITY", 2, Some("2221"), None, false, 22));
        items.push(item(
            "TOTAL_LIABILITIES",
            "LIABILITY",
            1,
            None,
            Some("AP + TAX + UNMAPPED_LIABILITIES"),
            false,
            23,
        ));
        let accounts = vec![
            account("1405", "ASSET", "DEBIT", 0, 300),
            account("2221.01", "LIABILITY", "CREDIT", 0, 100),
            account("2221.02", "LIABILITY", "CREDIT", 0, -400),
        ];
        let sheet = compute(&items, &accounts).expect("计算");
        let tax = line(&sheet.liabilities, "TAX");
        assert_eq!(
            tax.amount,
            Some(Decimal::from(300)),
            "进项税(借 100) −100 + 销项税(贷 400) +400 = 300"
        );
        assert_eq!(
            tax.accounts,
            vec!["2221.01".to_string(), "2221.02".to_string()],
            "命中科目要能审计：两个明细都在"
        );
        assert!(sheet.diff().is_zero(), "300 资产 = 300 应交税费");
        println!("[R1-⑤] 应交税费明细汇总 = 300 ✓");
    }

    // ---------- ⑥ 纯逻辑：不平要报出来 ----------

    /// ⑥ 人为制造不平：资产 100 而权益只有 60 → balanced=false，diff=40。
    #[test]
    fn r1_imbalance_is_reported_with_exact_diff() {
        let accounts = vec![
            account("1002", "ASSET", "DEBIT", 0, 100),
            account("3001", "EQUITY", "CREDIT", 0, -60),
        ];
        let sheet = compute(&mini_items(), &accounts).expect("计算");
        let check = build_check(
            &sheet,
            Decimal::from(100),
            Decimal::from(60),
            vec![UnbalancedVoucherRes {
                voucher_no: "R1-BAD-0001".to_string(),
                voucher_date: NaiveDate::from_ymd_opt(2026, 10, 31).expect("日期"),
                entry_debit: Decimal::from(100),
                entry_credit: Decimal::from(60),
                header_debit: Decimal::from(100),
                header_credit: Decimal::from(100),
            }],
        );
        assert!(!check.balanced, "必须报出不平行");
        assert_eq!(check.diff, Decimal::from(40));
        assert_eq!(check.diff_text, "40.00");
        assert!(
            check.message.contains("不平"),
            "结论必须是人话：{}",
            check.message
        );
        assert!(
            check.hints.iter().any(|hint| hint.contains("分录借贷不等")),
            "必须指出最可能成因：{:?}",
            check.hints
        );
        assert!(
            check.hints.iter().any(|hint| hint.contains("R1-BAD-0001")),
            "必须点名具体凭证：{:?}",
            check.hints
        );
        println!("[R1-⑥] 不平 40.00 被报出，且点名凭证 ✓");
    }

    /// ⑦ 差额 0.0001 不许被舍成 0.00（硬红线 4）。
    #[test]
    fn r1_tiny_diff_is_not_rounded_away() {
        assert_eq!(diff_text(Decimal::ZERO), "0.00");
        assert_eq!(diff_text(Decimal::from_str("12.3456").unwrap()), "12.35");
        assert_eq!(
            diff_text(Decimal::from_str("0.0001").unwrap()),
            "0.0001",
            "2 位小数会抹成 0.00 → 必须保留 4 位"
        );
        // 校验层面：0.0001 也是「不平」
        let accounts = vec![
            account("1002", "ASSET", "DEBIT", 0, 100),
            account("3001", "EQUITY", "CREDIT", 0, -100),
        ];
        let mut sheet = compute(&mini_items(), &accounts).expect("计算");
        sheet.amounts.insert(
            TOTAL_ASSETS.to_string(),
            Decimal::from_str("100.0001").unwrap(),
        );
        let check = build_check(&sheet, Decimal::ZERO, Decimal::ZERO, Vec::new());
        assert!(!check.balanced, "0.0001 的不平也必须报");
        assert_eq!(check.diff_text, "0.0001");
        println!("[R1-⑦] 0.0001 的差额不被抹平 ✓");
    }

    /// ⑧ 公式引用未知项必须报错（不许静默 0）。
    #[test]
    fn r1_formula_with_unknown_item_fails_loudly() {
        let items = vec![
            item("CASH", "ASSET", 2, Some("1001"), None, false, 10),
            item(
                "TOTAL_ASSETS",
                "ASSET",
                1,
                None,
                Some("CASH + NOPE + UNMAPPED_ASSETS"),
                false,
                11,
            ),
        ];
        let accounts = vec![account("1001", "ASSET", "DEBIT", 0, 1)];
        let error = compute(&items, &accounts).expect_err("必须报错");
        assert!(error.contains("NOPE"), "错误里要点名缺的项：{error}");
        assert!(
            eval_formula("A +", &HashMap::new()).is_err(),
            "算式结尾是运算符要报错"
        );
        println!("[R1-⑧] 公式引用未知项 → 显式报错 ✓");
    }

    /// ⑨ 期间解析：yyyy-MM 校验、上月、期间名。
    #[test]
    fn r1_period_parsing_and_previous_month() {
        assert_eq!(parse_period("2026-10"), Some((2026, 10)));
        assert_eq!(parse_period("2026-13"), None, "13 月不存在");
        assert_eq!(parse_period("2026-1"), None, "必须两位月份");
        assert_eq!(parse_period("abc"), None);
        assert_eq!(previous_period_of("2026-10").as_deref(), Some("2026-09"));
        assert_eq!(previous_period_of("2026-01").as_deref(), Some("2025-12"));
        assert_eq!(period_name_of("2026-10"), "2026年10月");
        let (start, end) = month_range("2026-02").expect("闰年还是平年都行");
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 2, 1).expect("日期"));
        assert_eq!(
            end,
            NaiveDate::from_ymd_opt(2026, 2, 28).expect("2026 不是闰年")
        );
        assert!(validate_period(Some("")).is_err(), "空 period 必须 400");
        assert!(
            validate_period(Some("2026-10-01")).is_err(),
            "带日期的 period 必须 400"
        );
        println!("[R1-⑨] 期间解析 / 上期 / 非法格式 ✓");
    }

    // ---------- 真库用例（ERP_DB_URL） ----------

    const FIXTURE_WORD: &str = "ERPR1";

    /// 清夹具 + 建 2026-10 / 2026-11 两个期间（**幂等**）。
    async fn setup_fixture() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        let statements = [
            format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'"),
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) VALUES (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN') \
             ON CONFLICT (period_code) DO NOTHING"
                .to_string(),
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) VALUES (2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'NOT_OPENED') \
             ON CONFLICT (period_code) DO NOTHING"
                .to_string(),
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) VALUES (2026, 9, '2026-09', '2026-09-01', '2026-09-30', 'CLOSED') \
             ON CONFLICT (period_code) DO NOTHING"
                .to_string(),
            // 2026-08 故意**不建**：用来验证「上期没有期间行就不出对比」。
            // 2027-01 建上：上期 2026-12 同样不存在 → 验证缺失分支。
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) VALUES (2027, 1, '2027-01', '2027-01-01', '2027-01-31', 'NOT_OPENED') \
             ON CONFLICT (period_code) DO NOTHING"
                .to_string(),
            // 2000-12：早于库里任何凭证 → 期初必然为 0（"空期间全 0"用例的唯一确定性口径）
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) VALUES (2000, 12, '2000-12', '2000-12-01', '2000-12-31', 'OPEN') \
             ON CONFLICT (period_code) DO NOTHING"
                .to_string(),
        ];
        for sql in &statements {
            db().execute_with_params(sql, vec![])
                .await
                .unwrap_or_else(|error| panic!("夹具 SQL 失败：{error}\n{sql}"));
        }
        Some(guard)
    }

    /// 建一张 POSTED 凭证（两行分录：借 / 贷）。
    async fn post_voucher(
        no: &str,
        date: &str,
        period: &str,
        debit_code: &str,
        credit_code: &str,
        amount: i64,
    ) {
        let sql = format!(
            "WITH v AS ( \
                INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, summary, \
                                         total_debit, total_credit, entry_count, status) \
                VALUES ('{FIXTURE_WORD}', '{no}', '{period}', DATE '{date}', 'ERPR1 夹具', \
                        {amount}, {amount}, 2, 'POSTED') RETURNING id \
             ), d AS ( \
                SELECT id, account_code, account_name FROM biz_account \
                 WHERE account_code = '{debit_code}' AND deleted_at IS NULL \
             ), c AS ( \
                SELECT id, account_code, account_name FROM biz_account \
                 WHERE account_code = '{credit_code}' AND deleted_at IS NULL \
             ) \
             INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, \
                                            account_name, debit_amount, credit_amount) \
             SELECT v.id, 1, 'ERPR1 借方', d.id, d.account_code, d.account_name, {amount}, 0 FROM v, d \
             UNION ALL \
             SELECT v.id, 2, 'ERPR1 贷方', c.id, c.account_code, c.account_name, 0, {amount} FROM v, c"
        );
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("建凭证失败：{error}\n{sql}"));
    }

    /// 在 REPEATABLE READ 事务里跑一次（与接口层同一条路径）。
    async fn run(period: &str) -> SheetSnapshot {
        let tx = db().begin().await.expect("开启事务");
        tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .await
            .expect("设置隔离级别");
        let snapshot = build_snapshot(&tx, period).await.expect("编制报表");
        tx.commit().await.expect("提交");
        snapshot
    }

    fn line<'a>(lines: &'a [ReportLineRes], code: &str) -> &'a ReportLineRes {
        lines
            .iter()
            .find(|row| row.item_code == code)
            .unwrap_or_else(|| panic!("报表里没有 {code} 项"))
    }

    /// 报表快照 → 勾稽结果（复用接口层的同一条路径）。
    fn check_of(snapshot: &SheetSnapshot) -> BalanceCheckRes {
        build_check(
            &snapshot.sheet,
            snapshot.ledger_debit_total,
            snapshot.ledger_credit_total,
            snapshot.unbalanced_vouchers.clone(),
        )
    }

    /// 某行金额（标题行 `null` 按 0 处理，便于做差）。
    fn amount_of(lines: &[ReportLineRes], code: &str) -> Decimal {
        line(lines, code).amount.unwrap_or_default()
    }

    /// ⑩ 真库：空期间（2000-12，早于库里任何凭证）→ 全 0，仍平衡。
    #[tokio::test]
    async fn r1_real_empty_period_is_all_zero_and_balanced() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let snapshot = run("2000-12").await;
        let check = check_of(&snapshot);
        assert!(check.balanced, "空期间必须平衡：{}", check.message);
        assert_eq!(check.assets_total, Decimal::ZERO);
        assert_eq!(check.liability_equity_total, Decimal::ZERO);
        assert_eq!(check.diff_text, "0.00");
        assert!(check.unmapped.is_empty(), "空期间没有未归类科目");
        assert!(
            snapshot
                .sheet
                .assets
                .iter()
                .all(|row| row.amount.unwrap_or_default().is_zero()),
            "空期间每一行都必须是 0"
        );
        println!("[R1-⑩] 空期间 2000-12 → 全 0 且平衡 ✓");
    }

    /// ⑪ 真库：完整一套业务（货币资金 / 存货 / 应付 / 实收资本 / 损益）→ 平衡且逐项对得上。
    ///
    /// **用基线做差**而不是断言绝对值：测试库可能被别的用例塞过数据（凭证会跨用例残留），
    /// 绝对值断言会假红。基线与本期之间只差我这 3 张凭证，差值就是我要证明的东西。
    #[tokio::test]
    async fn r1_real_full_cycle_is_balanced() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let baseline = run("2026-10").await;
        let base_check = check_of(&baseline);
        assert!(
            base_check.balanced,
            "用例前提：基线本身必须平（红了说明库里有别处留下的不平数据）：{}",
            base_check.message
        );

        // 期初投入：借 1002 银行存款 10,000 / 贷 3001 实收资本 10,000（2026-09 已结账期间）
        post_voucher("0001", "2026-09-15", "2026-09", "1002", "3001", 10_000).await;
        // 本期采购：借 1405 库存商品 3,000 / 贷 2202 应付账款 3,000
        post_voucher("0002", "2026-10-08", "2026-10", "1405", "2202", 3_000).await;
        // 本期费用：借 5602 管理费用 500 / 贷 1002 银行存款 500
        post_voucher("0003", "2026-10-20", "2026-10", "5602", "1002", 500).await;

        let after = run("2026-10").await;
        let check = check_of(&after);
        // 资产增 9,500（银行存款）+ 3,000（存货）= 12,500；负债增 3,000；权益增 10,000 − 500 = 9,500
        let d_assets = check.assets_total - base_check.assets_total;
        let d_liab_equity = check.liability_equity_total - base_check.liability_equity_total;
        assert_eq!(d_assets, Decimal::from(12_500), "资产侧增 12,500");
        assert_eq!(d_liab_equity, Decimal::from(12_500), "负债+权益侧增 12,500");
        assert_eq!(check.diff, base_check.diff, "我的 3 张凭证必须不改变差额");
        assert!(
            check.balanced,
            "必须平衡：{} / hints={:?}",
            check.message, check.hints
        );
        // 测试库可能残留别的用例的科目（如 P2D1A/P2D1B），它们落到「未归类」是**正确行为**；
        // 这里只要求"我这张报表用到的标准科目一个都不能未归类"。
        for code in ["1002", "1405", "1601", "1602", "2202", "3001", "5602"] {
            assert!(
                !after
                    .sheet
                    .unmapped
                    .iter()
                    .any(|row| row.account_code == code),
                "{code} 有余额却没匹配任何报表项（映射漏了）"
            );
        }

        assert_eq!(
            amount_of(&after.sheet.assets, "MONETARY_FUNDS")
                - amount_of(&baseline.sheet.assets, "MONETARY_FUNDS"),
            Decimal::from(9_500),
            "银行存款：期初 10,000 − 本期 500"
        );
        assert_eq!(
            amount_of(&after.sheet.assets, "INVENTORY")
                - amount_of(&baseline.sheet.assets, "INVENTORY"),
            Decimal::from(3_000),
            "存货 +3,000"
        );
        assert_eq!(
            amount_of(&after.sheet.liabilities, "ACCOUNTS_PAYABLE")
                - amount_of(&baseline.sheet.liabilities, "ACCOUNTS_PAYABLE"),
            Decimal::from(3_000),
            "应付 +3,000"
        );
        assert_eq!(
            amount_of(&after.sheet.equity, "UNDISTRIBUTED_PROFIT")
                - amount_of(&baseline.sheet.equity, "UNDISTRIBUTED_PROFIT"),
            Decimal::from(-500),
            "本期费用 500 冲减未分配利润"
        );
        assert_eq!(
            amount_of(&after.sheet.equity, "PAID_IN_CAPITAL")
                - amount_of(&baseline.sheet.equity, "PAID_IN_CAPITAL"),
            Decimal::from(10_000),
            "上期（2026-09，状态 CLOSED）的实收资本必须计入期初"
        );
        println!("[R1-⑪] 完整业务：资产 +12,500 = 负债 +3,000 + 权益 +9,500 ✓（含期初余额）");
    }

    /// ⑫ 真库：删掉一条分录制造不平 → balanced=false、差额正确、且点名该凭证。
    ///
    /// 断言放在**清理之后**：哪怕用例红，也不能把「分录不平的凭证」留给后面的用例。
    #[tokio::test]
    async fn r1_real_deleted_entry_breaks_the_balance() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let baseline = run("2026-10").await;
        let base_check = check_of(&baseline);

        post_voucher("0101", "2026-10-05", "2026-10", "1002", "3001", 1_000).await;
        post_voucher("0102", "2026-10-06", "2026-10", "1405", "2202", 400).await;
        let posted = check_of(&run("2026-10").await);

        // 制造不平：把 0102 的贷方分录（应付 400）删掉（单头的 total_credit 不会跟着变）
        let sql = format!(
            "DELETE FROM biz_voucher_entry WHERE voucher_id = ( \
                SELECT id FROM biz_voucher WHERE voucher_no = '0102' AND voucher_word = '{FIXTURE_WORD}') \
               AND credit_amount > 0"
        );
        db().execute_with_params(&sql, vec![])
            .await
            .expect("删分录");
        let broken = check_of(&run("2026-10").await);

        // 先清理（含那张已经被弄坏的凭证），再做断言
        db().execute_with_params(
            "DELETE FROM biz_voucher WHERE voucher_word = 'ERPR1'",
            vec![],
        )
        .await
        .expect("清理夹具凭证");

        assert!(
            base_check.balanced && posted.balanced,
            "用例前提：删分录前必须平（{} / {}）",
            base_check.message,
            posted.message
        );
        assert!(!broken.balanced, "删分录后必须报不平");
        assert_eq!(
            broken.diff - base_check.diff,
            Decimal::from(400),
            "差额必须精确是 400（资产比负债+权益多 400）"
        );
        assert_eq!(broken.diff_text, "400.00");
        assert_eq!(
            broken.ledger_debit_total - broken.ledger_credit_total,
            Decimal::from(400),
            "本期分录借贷差 400"
        );
        assert!(
            broken
                .unbalanced_vouchers
                .iter()
                .any(|row| row.voucher_no == "0102"),
            "必须点名被删分录的凭证：{:?}",
            broken
                .unbalanced_vouchers
                .iter()
                .map(|row| row.voucher_no.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            broken.hints.iter().any(|hint| hint.contains("0102")),
            "提示里要点名凭证：{:?}",
            broken.hints
        );
        assert!(
            broken.message.contains("不平") && broken.message.contains("400.00"),
            "结论必须是「不平 + 差额」：{}",
            broken.message
        );
        println!("[R1-⑫] 删分录 → 不平 400.00、点名 0102 ✓");
    }

    /// ⑬ 真库：期间未建 → 空月份必须 400；**有账的月份**按自然月推算（DERIVED）也要出表。
    #[tokio::test]
    async fn r1_real_missing_period_is_rejected_but_derivable() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        // ① 1999-01：没有期间行、也没有任何凭证 → 必须报错
        let tx = db().begin().await.expect("开启事务");
        let error = build_snapshot(&tx, "1999-01")
            .await
            .expect_err("空月份必须报错");
        let _ = tx.rollback().await;
        let message = format!("{error}");
        assert!(message.contains("1999-01"), "错误信息要带期间：{message}");
        assert!(
            message.contains("不存在"),
            "错误信息要说清是不存在：{message}"
        );

        // ② 2026-12：没有期间行，但**有**一张 12 月的凭证 → 按自然月推算，状态 DERIVED
        post_voucher("0301", "2026-12-05", "2026-12", "1002", "3001", 321).await;
        let snapshot = run("2026-12").await;
        assert_eq!(
            snapshot.period.status, PERIOD_DERIVED,
            "缺期间行但有账 → DERIVED"
        );
        assert_eq!(
            snapshot.period.start_date,
            NaiveDate::from_ymd_opt(2026, 12, 1).expect("日期")
        );
        assert_eq!(
            snapshot.period.end_date,
            NaiveDate::from_ymd_opt(2026, 12, 31).expect("日期")
        );
        let report = into_response("BALANCE_SHEET", REPORT_NAME, "云枢", "元", snapshot);
        assert!(
            report
                .check
                .hints
                .iter()
                .any(|hint| hint.contains("请尽快开账")),
            "推算出来的期间必须在 hints 里提示开账：{:?}",
            report.check.hints
        );
        println!("[R1-⑬] 空月份 400；有账无期间 → DERIVED + 提示开账 ✓");
        // 收尾：清掉这张"无期间行月份"的凭证，避免给后续用例/不变量套件留噪声
        db().execute_with_params(
            "DELETE FROM biz_voucher WHERE voucher_word = 'ERPR1'",
            vec![],
        )
        .await
        .expect("清理夹具凭证");
    }

    /// ⑭ 真库：上期对比能取到（2026-09 建过期间 → 出对比；1999 没有 → 不出）。
    #[tokio::test]
    async fn r1_real_previous_period_comparison() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        // 基线（不写死绝对值：库里可能有别处留下的 3001 分录）
        let baseline_09 = run("2026-09").await;
        let base_capital = amount_of(&baseline_09.sheet.equity, "PAID_IN_CAPITAL");

        // 2026-08 没有期间行，但凭证日期仍决定它落在 2026-09 的**期初**（取数按日期，不看期间表）
        post_voucher("0201", "2026-08-20", "2026-08", "1002", "3001", 700).await;

        // ① 上期存在（2026-09，状态 CLOSED）→ 2026-10 必须出对比，且 CLOSED 也能出表
        let snapshot = run("2026-10").await;
        assert_eq!(snapshot.previous_period.as_deref(), Some("2026-09"));
        let mut sheet = snapshot.sheet.clone();
        attach_previous(&mut sheet, snapshot.previous_sheet.as_ref());
        let capital = sheet
            .equity
            .iter()
            .find(|row| row.item_code == "PAID_IN_CAPITAL")
            .expect("实收资本行");
        assert_eq!(
            capital.previous_amount,
            Some(base_capital + Decimal::from(700)),
            "上期（2026-09 末）实收资本 = 基线 + 700"
        );
        assert_eq!(
            capital.amount,
            Some(base_capital + Decimal::from(700)),
            "本期不再发生 → 期末与上期相同"
        );

        // ② 上期不存在（2027-01 的上期 2026-12 没建期间行）→ previous_period = None
        let snapshot = run("2027-01").await;
        assert_eq!(
            snapshot.previous_period, None,
            "2026-12 没建期间 → 不出上期对比（不替用户推导不存在的报告期）"
        );
        let mut sheet = snapshot.sheet.clone();
        attach_previous(&mut sheet, snapshot.previous_sheet.as_ref());
        assert!(
            sheet
                .assets
                .iter()
                .chain(sheet.liabilities.iter())
                .chain(sheet.equity.iter())
                .all(|row| row.previous_amount.is_none()),
            "没有上期时，逐行 previous_amount 必须全是 null"
        );
        println!("[R1-⑭] 上期对比：2026-10 → 2026-09（CLOSED 也能出表）；2027-01 无上期 → null ✓");
    }

    /// ⑮ 真库：报表配置（41 项）与实际库里的行完全一致，且必含勾稽两项。
    #[tokio::test]
    async fn r1_real_config_is_consistent() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let tx = db().begin().await.expect("开启事务");
        let items = load_items(&tx).await.expect("读报表项");
        let accounts = load_accounts(
            &tx,
            NaiveDate::from_ymd_opt(2026, 10, 1).expect("日期"),
            NaiveDate::from_ymd_opt(2026, 10, 31).expect("日期"),
            None,
        )
        .await
        .expect("读科目");
        let _ = tx.rollback().await;

        assert_eq!(items.len(), 41, "BALANCE_SHEET 应有 41 个启用报表项");
        assert!(
            accounts.len() >= 56,
            "V0183 之后科目应有 ≥ 56 个，实际 {}",
            accounts.len()
        );
        assert!(items.iter().any(|row| row.item_code == TOTAL_ASSETS));
        assert!(
            items
                .iter()
                .any(|row| row.item_code == TOTAL_LIABILITIES_AND_EQUITY)
        );
        assert!(
            items
                .iter()
                .any(|row| row.item_code == "ACCUM_DEPRECIATION" && row.is_deduction),
            "累计折旧必须是减项"
        );
        // 每个**标准科目**（纯数字编码）都必须被某个报表项吃掉（否则报表会漏钱）。
        // 测试库里别的用例自造的科目（如 `P2D1A`）落到「未归类」是预期行为，不算漏。
        let sheet = compute(&items, &accounts).expect("计算");
        let standard_unmapped: Vec<&str> = sheet
            .unmapped
            .iter()
            .map(|row| row.account_code.as_str())
            .filter(|code| code.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .collect();
        assert!(
            standard_unmapped.is_empty(),
            "标准科目不该有未归类：{standard_unmapped:?}"
        );
        // 一个科目只能被**一个**报表项匹配：匹配到两个 = 同一笔钱在报表里被算两遍（静默多计）
        let mut double_matched: Vec<(String, Vec<String>)> = Vec::new();
        for account in &accounts {
            let hits: Vec<String> = items
                .iter()
                .filter(|item| {
                    item.account_pattern
                        .as_deref()
                        .is_some_and(|pattern| match_pattern(pattern, account))
                })
                .map(|item| item.item_code.clone())
                .collect();
            if hits.len() > 1 {
                double_matched.push((account.account_code.clone(), hits));
            }
        }
        assert!(
            double_matched.is_empty(),
            "同一科目被多个报表项匹配（会重复计数）：{double_matched:?}"
        );
        println!(
            "[R1-⑮] 报表配置 {} 项 / 科目 {} 个 / 标准科目全部有映射 ✓（另有 {} 个测试夹具科目未归类，属预期）",
            items.len(),
            accounts.len(),
            sheet.unmapped.len()
        );
    }
}

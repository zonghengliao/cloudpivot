//! 利润表（ERP-R2）：把**损益类科目的发生额**组装成利润表，并与资产负债表勾稽。**只读**。
//!
//! 承接 [`crate::business::erp::report`]（ERP-R1）的框架：同一张 `biz_report_item` 配置表、
//! 同一套科目匹配语法（[`report::match_account`]）、同一个公式求值器（[`report::eval_formula`]）、
//! 同一套期间口径（[`report::resolve_period`]）与同一个 REPEATABLE READ 快照模式。
//! **本模块不重写框架，也不碰凭证 / 科目余额逻辑**（硬红线 1 / 4）。
//!
//! 表结构依据 `backend/migrations/business/V0184__erp_income_statement.up.sql`
//! （`biz_report_item` 里 `report_code = 'INCOME_STATEMENT'` 的 12 行），
//! 取数来源 `V0149` 的 `biz_voucher` / `biz_voucher_entry`。
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/income-statement?period=2026-10` | `business:erp:report:income-statement` | 利润表（本期 + 本年累计 + 两表勾稽） |
//! | GET | `/business/erp/report/income-statement/export?period=2026-10` | `business:erp:report:export` | 同一份报表导出 xlsx |
//!
//! # 核心不变量（本模块存在的理由）
//!
//! ```text
//! 净利润 ≡ 营业收入 − 营业成本 − 税金及附加 − 销售费用 − 管理费用 − 财务费用
//!          + 营业外收入 − 营业外支出 − 所得税费用
//! 净利润（本年累计） ≡ 资产负债表「未分配利润」本年变动
//! ```
//!
//! 第一条是**报表自身的加减关系**（由 `biz_report_item.formula` 保证，单测 R2-①…④ 钉住）。
//! 第二条是**两张报表的勾稽**：净利润是流量，未分配利润是存量，流量必须等于存量的变动。
//! 差一分钱都要报出来：`check.balanced == false`、`check.diff` 精确差额、`check.message`
//! 整句人话、`check.hints` 指出最可能的成因（未归类损益 / 本年利润直接结转 / 上年未结转 /
//! 分录不平）。**绝不四舍五入抹平**（复用 [`report::diff_text`]）。
//!
//! ## 为什么第二条恒等式在数学上必然成立
//!
//! 复式记账下每张 POSTED 凭证 `Σ借 = Σ贷`，所以 [年初, 期末] 窗口内：
//!
//! ```text
//! Σ全部科目 (借−贷) = 0
//! ⇒ Σ损益类 (借−贷) + Σ非损益类 (借−贷) = 0
//! ⇒ 利润 = −Σ损益类 (借−贷) = Σ非损益类 (借−贷)
//! ```
//!
//! 资产负债表「未分配利润」= `3103 本年利润 + 3104 利润分配 + 全部损益类科目的利润贡献`
//! （V0183 的 `UNDISTRIBUTED_PROFIT`），因此它的本年变动 = 损益类贡献变动 + 3103/3104 变动。
//! 只要 ①利润表把每个损益类科目都归了行、②没有手工往 3103/3104 直接记账，两条就相等。
//! 不相等时，`check.hints` 按这个成因清单逐条核对，差额精确到分。
//!
//! # 取数口径（与资产负债表的最大差异：这里是**发生额**，不是余额）
//!
//! ```text
//! 本期     = Σ(借−贷)  where voucher_date ∈ [期间开始日, 期间结束日]
//! 本年累计 = Σ(借−贷)  where voucher_date ∈ [本年 1 月 1 日, 期间结束日]
//! ```
//!
//! 损益类科目**没有期初余额**（年终结转后为 0），所以只取发生额：
//!
//! * 收入类（`balance_direction = 'CREDIT'`，如 5001）：报表金额 = 贷 − 借 = `−Σ(借−贷)`；
//! * 成本费用类（`DEBIT`，如 5401 / 5602）：报表金额 = 借 − 贷 = `Σ(借−贷)`。
//!
//! 两种都是**正数表示该行的正常方向发生额**，加减关系交给 `formula`（`减：` 的行参与减法）。
//! 红字凭证是借贷互换（`is_red = 1`，金额恒为正），净额天然为负 → 红冲自动抵减，
//! 与总账 / 余额表同一把尺子，不需要特判（单测 R2-⑤）。
//!
//! **只算 `status = 'POSTED'` 且未删除的凭证**（草稿不进报表）。
//!
//! # 与资产负债表的口径对齐（`UNDISTRIBUTED_PROFIT` 只认一处配置）
//!
//! 勾稽右式不写死科目清单：运行时从 `biz_report_item` 读
//! `report_code = 'BALANCE_SHEET' AND item_code = 'UNDISTRIBUTED_PROFIT'` 的
//! `account_pattern`（取不到时回退 [`UNDISTRIBUTED_PATTERN_FALLBACK`]）。
//! 于是「谁算未分配利润」永远只有一份定义 —— 改了资产负债表的映射，利润表的勾稽跟着变，
//! 不会出现两张报表各算各的、都自称平。
//!
//! # 未归类损益（`UNMAPPED_PROFIT_LOSS` 行）
//!
//! 卡面 §一 的结构没有「投资收益」等行。若某个损益类科目有发生额却没有被任何报表项匹配，
//! 钱不会凭空消失：报表上多出一行 `UNMAPPED_PROFIT_LOSS`（**运行时补出来，不落库**），
//! 同时 `check.unmapped` 带科目编码与金额、`check.diff` 精确报出差额。
//! 该行**不计入净利润**（净利润严格等于卡面的加减关系），所以只要它不为 0，
//! `check.balanced` 必定为 false —— 这是刻意的：报表漏了钱就不能显示「平」。
//! 要显示这类科目，往 `biz_report_item` 加一行映射即可（配置驱动，不用发版）。
//!
//! # 快照隔离（硬红线 2 / L11）
//!
//! 整张报表在一个 **REPEATABLE READ** 事务里算完（期间 + 报表项配置 + 两段发生额 +
//! 借贷合计 + 异常凭证清单，同一个快照）：`SET TRANSACTION ISOLATION LEVEL REPEATABLE READ`
//! 必须是 `BEGIN` 后的**第一条**语句（PG 规定），否则 25001。
//!
//! # 期间口径
//!
//! 与资产负债表**完全一致**（复用 [`report::resolve_period`]）：有期间行用它（`OPEN`/`CLOSED`
//! 都出表）；有账无期间 → 按自然月推算并标 `DERIVED` + 提示开账；无期间行且无凭证 → `400 + 66010`。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_report_item 是报表配置、biz_account / biz_voucher / biz_voucher_entry 是
//! 全局会计台账（V0137/V0149/V0183 建表，无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:report:income-statement / :export 权限码控制，本模块无分页查询。
//! ```

use std::collections::HashMap;

use axum::response::Response;
use nik::prelude::chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::report::{
    self, AccountBalance, PERIOD_DERIVED, PeriodInfo, ReportItemDef, UnbalancedVoucherRes,
};
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 报表编码（`biz_report_item.report_code`）。
pub const REPORT_CODE: &str = "INCOME_STATEMENT";

/// 报表中文名（导出文件名 / 页头）。
pub const REPORT_NAME: &str = "利润表";

/// 资产负债表编码（勾稽右式从这张表里读「未分配利润」的映射）。
pub const BALANCE_REPORT_CODE: &str = "BALANCE_SHEET";

/// 资产负债表里「未分配利润」的报表项编码。
pub const UNDISTRIBUTED_ITEM: &str = "UNDISTRIBUTED_PROFIT";

/// 「未分配利润」的兜底匹配模式（与 V0183 种进 `BALANCE_SHEET` 的逐字一致）。
pub const UNDISTRIBUTED_PATTERN_FALLBACK: &str = "@CATEGORY:PROFIT_LOSS,3103,3104";

/// 未归类损益行（不落库，运行时补出来的）。
pub const UNMAPPED_PROFIT_LOSS: &str = "UNMAPPED_PROFIT_LOSS";

/// 净利润报表项编码。
pub const NET_PROFIT: &str = "NET_PROFIT";

/// 营业收入报表项编码。
pub const REVENUE: &str = "REVENUE";

/// 营业利润报表项编码。
pub const OPERATING_PROFIT: &str = "OPERATING_PROFIT";

/// 利润总额报表项编码。
pub const TOTAL_PROFIT: &str = "TOTAL_PROFIT";

/// 本期列名。
pub const CURRENT_LABEL: &str = "本期金额";

/// 本年累计列名。
pub const YTD_LABEL: &str = "本年累计";

// ==================== 入参 ====================

/// 利润表查询参数（`period` 必填，`yyyy-MM`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IncomeStatementQuery {
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
}

// ==================== 科目发生额 ====================

/// 单个科目的发生额（四段窗口一次取全，纯数据）。
///
/// 四个净额都是「借 − 贷」的**原始符号**，报表口径由 [`IncomeAccount::current`] /
/// [`IncomeAccount::ytd`] / [`equity_amount`] 各自折算 —— 同一笔数据、两种问法。
#[derive(Debug, Clone)]
pub struct IncomeAccount {
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
    /// 本期发生额（借 − 贷，期间窗口内）。
    pub net_period: Decimal,
    /// 本年累计发生额（借 − 贷，1 月 1 日 → 期间末）。
    pub net_ytd: Decimal,
    /// 期末累计（借 − 贷，含往年）。
    pub net_cum: Decimal,
    /// 年初之前累计（借 − 贷，1 月 1 日之前）。
    pub net_before_year: Decimal,
}

impl IncomeAccount {
    /// 本期报表金额（正数 = 该行正常方向的发生额）。
    pub fn current(&self) -> Decimal {
        income_amount(self.net_period, &self.balance_direction)
    }

    /// 本年累计报表金额。
    pub fn ytd(&self) -> Decimal {
        income_amount(self.net_ytd, &self.balance_direction)
    }

    /// 该科目是否损益类（利润表的取数对象）。
    pub fn is_profit_loss(&self) -> bool {
        self.account_category.eq_ignore_ascii_case("PROFIT_LOSS")
    }
}

/// 损益类科目 → **利润表金额**：收入类取 `−net`，成本费用类取 `net`。
///
/// 与 [`equity_amount`] 的区别是**符号约定**，不是数据：
/// 利润表上「管理费用 500.00」要显示正数（再看表头「减：」），
/// 而权益侧要的是它对利润的**贡献**（−500）。两者混用会把费用算成利润
/// （ERP-R1 的单测 R1-⑪ 就是这么抓到同类翻车的）。
pub fn income_amount(net: Decimal, balance_direction: &str) -> Decimal {
    report::zero_norm(if balance_direction == "CREDIT" {
        -net
    } else {
        net
    })
}

/// 科目 → **权益贡献**（与 `report::AccountBalance::display` 逐字同口径）。
///
/// 损益类一律 `−net`：收入（贷方余额，net 为负）贡献为正，费用（借方）贡献为负。
pub fn equity_amount(account: &IncomeAccount, net: Decimal) -> Decimal {
    report::zero_norm(
        if account.account_category.eq_ignore_ascii_case("PROFIT_LOSS")
            || account.balance_direction == "CREDIT"
        {
            -net
        } else {
            net
        },
    )
}

// ==================== 出参 ====================

/// 利润表行。
#[derive(Debug, Clone, Serialize)]
pub struct IncomeLineRes {
    /// 报表项编码。
    pub item_code: String,
    /// 报表项名称（`减：` / `加：` 前缀已经在名字里，前端不再拼）。
    pub item_name: String,
    /// 层级（1 大类 / 2 明细）。
    pub level: i16,
    /// 上级编码。
    pub parent_code: Option<String>,
    /// 本期金额。
    pub current: Decimal,
    /// 本年累计金额。
    pub ytd: Decimal,
    /// 是否小计 / 合计行（公式项）。
    pub is_total: bool,
    /// 取数说明（`'5001,5051'` 或公式原文）。
    pub detail: Option<String>,
    /// 实际命中的科目编码（可审计）。
    pub accounts: Vec<String>,
}

/// 未归类损益科目（有发生额但没有任何利润表行匹配）。
#[derive(Debug, Clone, Serialize)]
pub struct UnmappedProfitLossRes {
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 本期发生额（正常方向）。
    pub current: Decimal,
    /// 本年累计发生额（正常方向）。
    pub ytd: Decimal,
    /// 对本期利润的影响（收入为正、费用为负）。
    pub effect_current: Decimal,
    /// 对本年累计利润的影响（收入为正、费用为负）。
    pub effect_ytd: Decimal,
}

/// 勾稽校验结果（净利润 本年累计 ≡ 未分配利润 本年变动）。
#[derive(Debug, Clone, Serialize)]
pub struct IncomeCheckRes {
    /// 两侧是否相等（精确比较，不做舍入）。
    pub balanced: bool,
    /// 净利润（本年累计）。
    pub net_profit_ytd: Decimal,
    /// 资产负债表「未分配利润」本年变动。
    pub equity_change: Decimal,
    /// 差额 = 净利润 − 权益变动，**不做任何舍入**。
    pub diff: Decimal,
    /// 差额人话（`'0.00'` / `'差 0.0100 元'` 的数值部分）。
    pub diff_text: String,
    /// 整句结论（前端直接显示）。
    pub message: String,
    /// 期末「未分配利润」（累计）。
    pub undistributed_profit_end: Decimal,
    /// 年初「未分配利润」（累计，本年 1 月 1 日之前）。
    pub undistributed_profit_year_start: Decimal,
    /// 未归类损益合计（对利润的影响）。
    pub unmapped_profit_loss_ytd: Decimal,
    /// 未归类损益科目明细。
    pub unmapped: Vec<UnmappedProfitLossRes>,
    /// 窗口内分录借方合计（本年累计窗口，诊断用）。
    pub ledger_debit_total: Decimal,
    /// 窗口内分录贷方合计。
    pub ledger_credit_total: Decimal,
    /// 分录合计与单头不符的凭证（本年累计窗口）。
    pub unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
    /// 成因提示（人话）。
    pub hints: Vec<String>,
}

/// 利润表出参。
#[derive(Debug, Clone, Serialize)]
pub struct IncomeStatementRes {
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
    /// 期间状态（`OPEN` / `CLOSED` / `DERIVED`）。
    pub period_status: String,
    /// 期间开始日。
    pub period_start: NaiveDate,
    /// 期间结束日。
    pub period_end: NaiveDate,
    /// 本年累计起算日（1 月 1 日）。
    pub ytd_start: NaiveDate,
    /// 本期列名。
    pub current_label: String,
    /// 本年累计列名。
    pub ytd_label: String,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
    /// 报表行（按 `sort_order`，含运行时补出来的未归类行）。
    pub items: Vec<IncomeLineRes>,
    /// 勾稽校验。
    pub check: IncomeCheckRes,
}

// ==================== 纯计算 ====================

/// 计算结果（两列金额 + 未归类明细）。
#[derive(Debug, Clone, Default)]
pub struct ComputedIncome {
    /// 报表行。
    pub items: Vec<IncomeLineRes>,
    /// 本期金额（按 `item_code`）。
    pub current: HashMap<String, Decimal>,
    /// 本年累计金额（按 `item_code`）。
    pub ytd: HashMap<String, Decimal>,
    /// 未归类损益科目。
    pub unmapped: Vec<UnmappedProfitLossRes>,
}

impl ComputedIncome {
    /// 某行的本期金额（找不到按 0）。
    pub fn current_of(&self, item_code: &str) -> Decimal {
        self.current.get(item_code).copied().unwrap_or_default()
    }

    /// 某行的本年累计金额（找不到按 0）。
    pub fn ytd_of(&self, item_code: &str) -> Decimal {
        self.ytd.get(item_code).copied().unwrap_or_default()
    }

    /// 净利润（本期）。
    pub fn net_profit_current(&self) -> Decimal {
        self.current_of(NET_PROFIT)
    }

    /// 净利润（本年累计）。
    pub fn net_profit_ytd(&self) -> Decimal {
        self.ytd_of(NET_PROFIT)
    }

    /// 未归类损益合计（对利润的影响，本年累计）。
    pub fn unmapped_effect_ytd(&self) -> Decimal {
        report::zero_norm(self.unmapped.iter().map(|row| row.effect_ytd).sum())
    }
}

/// 把「科目发生额 + 报表项配置」算成一张利润表。**纯函数**，无 IO。
///
/// # 异常情况
///
/// 返回 `Err` 的只有配置错误（公式引用未知项 / 算式不合法），接口按 500 处理 ——
/// 配置错了绝不能静默出 0，那会被人当成「这个月没发生」。
pub fn compute(
    items: &[ReportItemDef],
    accounts: &[IncomeAccount],
) -> std::result::Result<ComputedIncome, String> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then(a.item_code.cmp(&b.item_code))
    });

    let mut current: HashMap<String, Decimal> = HashMap::new();
    let mut ytd: HashMap<String, Decimal> = HashMap::new();
    let mut matched: Vec<bool> = vec![false; accounts.len()];
    let mut owned: HashMap<String, Vec<String>> = HashMap::new();

    // ---------- 第 1 遍：科目取数项（不含公式） ----------
    for item in &sorted {
        let Some(pattern) = item.account_pattern.as_deref() else {
            continue;
        };
        let mut sum_current = Decimal::ZERO;
        let mut sum_ytd = Decimal::ZERO;
        let mut codes = Vec::new();
        for (index, account) in accounts.iter().enumerate() {
            if !report::match_account(pattern, &account.account_code, &account.account_category) {
                continue;
            }
            matched[index] = true;
            sum_current += account.current();
            sum_ytd += account.ytd();
            codes.push(account.account_code.clone());
        }
        codes.sort();
        codes.dedup();
        current.insert(item.item_code.clone(), report::zero_norm(sum_current));
        ytd.insert(item.item_code.clone(), report::zero_norm(sum_ytd));
        owned.insert(item.item_code.clone(), codes);
    }

    // ---------- 未归类损益：有发生额、却没被任何行匹配的损益类科目 ----------
    let mut unmapped: Vec<UnmappedProfitLossRes> = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        if matched[index] || !account.is_profit_loss() {
            continue;
        }
        let row_current = account.current();
        let row_ytd = account.ytd();
        if row_current.is_zero() && row_ytd.is_zero() {
            continue;
        }
        // 对利润的影响：收入（贷方）为正、费用（借方）为负
        let (effect_current, effect_ytd) = if account.balance_direction == "CREDIT" {
            (row_current, row_ytd)
        } else {
            (-row_current, -row_ytd)
        };
        unmapped.push(UnmappedProfitLossRes {
            account_code: account.account_code.clone(),
            account_name: account.account_name.clone(),
            current: row_current,
            ytd: row_ytd,
            effect_current: report::zero_norm(effect_current),
            effect_ytd: report::zero_norm(effect_ytd),
        });
    }
    unmapped.sort_by(|a, b| a.account_code.cmp(&b.account_code));
    let unmapped_effect_current =
        report::zero_norm(unmapped.iter().map(|row| row.effect_current).sum());
    let unmapped_effect_ytd = report::zero_norm(unmapped.iter().map(|row| row.effect_ytd).sum());
    // 未归类行的金额先塞进两张金额表：这样 `formula` 也可以引用它
    // （比如 `formula = 'TOTAL_PROFIT - INCOME_TAX + UNMAPPED_PROFIT_LOSS'`），
    // 与资产负债表 `UNMAPPED_*` 三行的做法一致。
    current.insert(UNMAPPED_PROFIT_LOSS.to_string(), unmapped_effect_current);
    ytd.insert(UNMAPPED_PROFIT_LOSS.to_string(), unmapped_effect_ytd);

    // ---------- 第 2 遍：公式项（按 sort_order，依赖项必须在前） ----------
    for item in &sorted {
        let Some(formula) = item.formula.as_deref() else {
            continue;
        };
        let value_current = report::zero_norm(report::eval_formula(formula, &current)?);
        let value_ytd = report::zero_norm(report::eval_formula(formula, &ytd)?);
        current.insert(item.item_code.clone(), value_current);
        ytd.insert(item.item_code.clone(), value_ytd);
    }

    // ---------- 组装报表行 ----------
    let mut lines: Vec<IncomeLineRes> = Vec::new();
    for item in &sorted {
        let is_total = item.formula.is_some();
        lines.push(IncomeLineRes {
            item_code: item.item_code.clone(),
            item_name: item.item_name.clone(),
            level: item.item_level,
            parent_code: item.parent_code.clone(),
            current: current.get(&item.item_code).copied().unwrap_or_default(),
            ytd: ytd.get(&item.item_code).copied().unwrap_or_default(),
            is_total,
            detail: item
                .account_pattern
                .clone()
                .or_else(|| item.formula.clone()),
            accounts: owned.get(&item.item_code).cloned().unwrap_or_default(),
        });
    }

    // 未归类行插在**净利润之前**（人看报表时紧挨结论行，不会被忽略）。
    // 金额 = 对利润的影响；它**不参与**净利润公式（净利润严格等于卡面的加减关系），
    // 所以只要它不为 0，勾稽必然不平 —— 这正是我们要报警的场景。
    let unmapped_line = IncomeLineRes {
        item_code: UNMAPPED_PROFIT_LOSS.to_string(),
        item_name: "未归类损益（未计入净利润，映射缺失）".to_string(),
        level: 2,
        parent_code: Some(TOTAL_PROFIT.to_string()),
        current: unmapped_effect_current,
        ytd: unmapped_effect_ytd,
        is_total: false,
        detail: Some(
            "按损益类科目差额计算：所有未被利润表任何行匹配的损益类科目（收入为正、费用为负）。\
             这些钱进了「未分配利润」但没进净利润 —— 到 biz_report_item 给 report_code='INCOME_STATEMENT' 补一行映射"
                .to_string(),
        ),
        accounts: unmapped.iter().map(|row| row.account_code.clone()).collect(),
    };
    let position = lines
        .iter()
        .position(|row| row.item_code == NET_PROFIT)
        .unwrap_or(lines.len());
    lines.insert(position, unmapped_line);

    Ok(ComputedIncome {
        items: lines,
        current,
        ytd,
        unmapped,
    })
}

/// 「未分配利润」的两个时点金额：`(期末累计, 年初之前累计)`。
///
/// 匹配口径来自资产负债表的 `UNDISTRIBUTED_PROFIT` 报表项（见模块文档）。
pub fn undistributed_profit(accounts: &[IncomeAccount], pattern: &str) -> (Decimal, Decimal) {
    let mut end = Decimal::ZERO;
    let mut start = Decimal::ZERO;
    for account in accounts {
        if !report::match_account(pattern, &account.account_code, &account.account_category) {
            continue;
        }
        end += equity_amount(account, account.net_cum);
        start += equity_amount(account, account.net_before_year);
    }
    (report::zero_norm(end), report::zero_norm(start))
}

/// 本年「本年利润 / 利润分配」（3103 / 3104）的直接发生额（对权益的贡献）。
///
/// 这两个科目本年有直接发生额（结转损益、利润分配、以前年度损益调整）时，
/// 未分配利润的变动会**不等于**净利润 —— 这是勾稽不平最常见的正当成因，必须点名。
pub fn capital_profit_movement(accounts: &[IncomeAccount]) -> Decimal {
    report::zero_norm(
        accounts
            .iter()
            .filter(|account| {
                account.account_code == "3103"
                    || account.account_code.starts_with("3103.")
                    || account.account_code == "3104"
                    || account.account_code.starts_with("3104.")
            })
            .map(|account| equity_amount(account, account.net_ytd))
            .sum(),
    )
}

/// 年初「损益类科目未结转」的余额合计（对权益的贡献）。
///
/// 上年没做年终结转时，损益类科目带着余额进入本年；本年累计只取 1 月 1 日以后的发生额，
/// 于是「净利润（本年累计）」天然不等于「未分配利润本年变动」的差额里含这一块。
pub fn profit_loss_carry_in(accounts: &[IncomeAccount]) -> Decimal {
    report::zero_norm(
        accounts
            .iter()
            .filter(|account| account.is_profit_loss())
            .map(|account| equity_amount(account, account.net_before_year))
            .sum(),
    )
}

/// 勾稽校验（纯逻辑：只吃计算结果与诊断数据）。
pub fn build_check(
    computed: &ComputedIncome,
    accounts: &[IncomeAccount],
    undistributed_pattern: &str,
    ledger_debit_total: Decimal,
    ledger_credit_total: Decimal,
    unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
) -> IncomeCheckRes {
    let net_profit_ytd = computed.net_profit_ytd();
    let (undistributed_end, undistributed_start) =
        undistributed_profit(accounts, undistributed_pattern);
    let equity_change = report::zero_norm(undistributed_end - undistributed_start);
    let diff = report::zero_norm(net_profit_ytd - equity_change);
    let balanced = diff.is_zero();
    let unmapped_ytd = computed.unmapped_effect_ytd();

    let mut hints: Vec<String> = Vec::new();
    if !balanced {
        if !computed.unmapped.is_empty() {
            let items: Vec<String> = computed
                .unmapped
                .iter()
                .take(8)
                .map(|row| format!("{}({})", row.account_code, row.ytd))
                .collect();
            hints.push(format!(
                "有 {} 个损益类科目没有归入利润表任何一行：{}（合计影响利润 {}）—— \
                 这些钱进了「未分配利润」但没进净利润，到 biz_report_item 给 \
                 report_code='INCOME_STATEMENT' 补一行映射即可",
                computed.unmapped.len(),
                items.join("、"),
                unmapped_ytd
            ));
        }
        let movement = capital_profit_movement(accounts);
        if !movement.is_zero() {
            hints.push(format!(
                "本年利润 / 利润分配（3103 / 3104）本年直接发生额 {movement}：\
                 结转损益、利润分配、以前年度损益调整都会让「未分配利润变动」≠「净利润」，\
                 这类凭证的对方科目不在损益类里"
            ));
        }
        let carry_in = profit_loss_carry_in(accounts);
        if !carry_in.is_zero() {
            hints.push(format!(
                "损益类科目在年初仍有余额（对权益影响 {carry_in}）：上年没有做年终结转。\
                 本年累计只取 1 月 1 日以后的发生额，差额里含这块上年利润"
            ));
        }
        let ledger_gap = report::zero_norm(ledger_debit_total - ledger_credit_total);
        if !ledger_gap.is_zero() {
            hints.push(format!(
                "本年累计窗口分录借贷不等：Σ借 {ledger_debit_total} ≠ Σ贷 {ledger_credit_total}\
                 （差 {ledger_gap}）—— 最可能是某张凭证的分录行被删/被改"
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
        hints.push(
            "检查利润表映射（biz_report_item）：一行漏映射、或两行重复映射同一科目，都会让净利润与权益变动对不上"
                .to_string(),
        );
    }

    let message = if balanced {
        format!(
            "勾稽一致：净利润（本年累计）{net_profit_ytd} = 资产负债表「未分配利润」本年变动 {equity_change}"
        )
    } else {
        format!(
            "不平：净利润（本年累计）{net_profit_ytd} ≠ 资产负债表「未分配利润」本年变动 {equity_change}，\
             差 {} 元（差额不做舍入）",
            report::diff_text(diff)
        )
    };

    IncomeCheckRes {
        balanced,
        net_profit_ytd,
        equity_change,
        diff,
        diff_text: report::diff_text(diff),
        message,
        undistributed_profit_end: undistributed_end,
        undistributed_profit_year_start: undistributed_start,
        unmapped_profit_loss_ytd: unmapped_ytd,
        unmapped: computed.unmapped.clone(),
        ledger_debit_total,
        ledger_credit_total,
        unbalanced_vouchers,
        hints,
    }
}

// ==================== 数据访问 ====================

/// 期间 → 本年累计起算日（该年 1 月 1 日）。
pub fn ytd_start_of(period: &str) -> Option<NaiveDate> {
    let (year, _month) = report::parse_period(period)?;
    NaiveDate::from_ymd_opt(year, 1, 1)
}

/// 本年累计起算日（拿不到期间时按 `period_end` 的年兜底 —— 调用方已经校验过格式）。
fn ytd_start_for(period: &str, period_end: NaiveDate) -> NaiveDate {
    ytd_start_of(period)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(period_end.year(), 1, 1).unwrap_or(period_end))
}

/// 科目发生额（本期 + 本年累计 + 期末累计 + 年初之前）。
///
/// 复用 [`report::load_accounts`] 查**两段**窗口，不另写 SQL：
///
/// | 窗口 | `net_before` | `net_period` |
/// |------|--------------|--------------|
/// | `[期间开始日, 期间结束日]` | 期间之前累计 | 本期发生额 |
/// | `[本年 1 月 1 日, 期间结束日]` | **年初之前累计** | **本年累计发生额** |
pub async fn load_income_accounts(
    tx: &Transaction,
    period_start: NaiveDate,
    period_end: NaiveDate,
    ytd_start: NaiveDate,
) -> DbResult<Vec<IncomeAccount>> {
    let period_rows = report::load_accounts(tx, period_start, period_end).await?;
    let ytd_rows = report::load_accounts(tx, ytd_start, period_end).await?;
    let ytd_by_id: HashMap<Uuid, AccountBalance> = ytd_rows
        .into_iter()
        .map(|row| (row.account_id, row))
        .collect();

    let mut out = Vec::with_capacity(period_rows.len());
    for row in period_rows {
        let ytd_row = ytd_by_id.get(&row.account_id);
        out.push(IncomeAccount {
            account_id: row.account_id,
            account_code: row.account_code,
            account_name: row.account_name,
            account_category: row.account_category,
            balance_direction: row.balance_direction,
            net_period: row.net_period,
            net_ytd: ytd_row.map(|row| row.net_period).unwrap_or_default(),
            net_cum: row.net_before + row.net_period,
            net_before_year: ytd_row.map(|row| row.net_before).unwrap_or_default(),
        });
    }
    Ok(out)
}

/// 「未分配利润」的匹配模式（取资产负债表的配置；取不到时回退常量）。
pub async fn load_undistributed_pattern(tx: &Transaction) -> DbResult<String> {
    let sql = format!(
        "SELECT account_pattern FROM {} \
          WHERE report_code = ? AND item_code = ? AND is_active = 1 LIMIT 1",
        report::ITEM_TABLE
    );
    let row = tx
        .find_with_params(
            &sql,
            vec![
                Value::from(BALANCE_REPORT_CODE),
                Value::from(UNDISTRIBUTED_ITEM),
            ],
        )
        .await?
        .into_iter()
        .next();
    let pattern = row
        .and_then(|row| row.get_typed::<String>("account_pattern").ok())
        .filter(|text| !text.trim().is_empty());
    Ok(pattern.unwrap_or_else(|| UNDISTRIBUTED_PATTERN_FALLBACK.to_string()))
}

/// 一次报表计算的完整产物（接口层用它拼返回体）。
#[derive(Debug)]
pub struct IncomeSnapshot {
    /// 计算结果。
    pub computed: ComputedIncome,
    /// 期间。
    pub period: PeriodInfo,
    /// 本年累计起算日。
    pub ytd_start: NaiveDate,
    /// 未分配利润的匹配模式。
    pub undistributed_pattern: String,
    /// 科目发生额（勾稽右式要再算一遍）。
    pub accounts: Vec<IncomeAccount>,
    /// 窗口内分录借方合计（本年累计窗口）。
    pub ledger_debit_total: Decimal,
    /// 窗口内分录贷方合计。
    pub ledger_credit_total: Decimal,
    /// 分录不平的凭证（本年累计窗口）。
    pub unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
}

impl IncomeSnapshot {
    /// 勾稽结果。
    pub fn check(&self) -> IncomeCheckRes {
        build_check(
            &self.computed,
            &self.accounts,
            &self.undistributed_pattern,
            self.ledger_debit_total,
            self.ledger_credit_total,
            self.unbalanced_vouchers.clone(),
        )
    }
}

/// 在给定事务（**必须是 REPEATABLE READ 快照**）里算出一整张利润表。
///
/// # 异常情况
///
/// * 期间不存在 → `Err(DbError::InvalidValue)`，接口层翻成 400 + 66010；
/// * 报表项配置错（公式引用未知项）→ `Err(DbError::InvalidValue)`，接口层翻成 500 + 66011。
pub async fn build_snapshot(tx: &Transaction, period: &str) -> DbResult<IncomeSnapshot> {
    let info = report::resolve_period(tx, period).await?;
    let items = report::load_items_of(tx, REPORT_CODE).await?;
    if items.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "报表项配置为空（{} .report_code = '{REPORT_CODE}' 一行都没有），无法编制利润表",
            report::ITEM_TABLE
        )));
    }

    let ytd_start = ytd_start_for(period, info.end_date);
    let accounts = load_income_accounts(tx, info.start_date, info.end_date, ytd_start).await?;
    let computed = compute(&items, &accounts).map_err(DbError::InvalidValue)?;
    let undistributed_pattern = load_undistributed_pattern(tx).await?;

    let (ledger_debit_total, ledger_credit_total) =
        report::load_ledger_totals(tx, ytd_start, info.end_date).await?;
    let unbalanced_vouchers =
        report::load_unbalanced_vouchers(tx, ytd_start, info.end_date).await?;

    Ok(IncomeSnapshot {
        computed,
        period: info,
        ytd_start,
        undistributed_pattern,
        accounts,
        ledger_debit_total,
        ledger_credit_total,
        unbalanced_vouchers,
    })
}

/// 组装接口返回体（纯逻辑：IO 结果进来，JSON 出去）。
pub fn into_response(
    company_name: &str,
    unit: &str,
    snapshot: IncomeSnapshot,
) -> IncomeStatementRes {
    let IncomeSnapshot {
        computed,
        period,
        ytd_start,
        undistributed_pattern,
        accounts,
        ledger_debit_total,
        ledger_credit_total,
        unbalanced_vouchers,
    } = snapshot;
    let mut check = build_check(
        &computed,
        &accounts,
        &undistributed_pattern,
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
    IncomeStatementRes {
        report_code: REPORT_CODE.to_string(),
        report_name: REPORT_NAME.to_string(),
        company_name: company_name.to_string(),
        unit: unit.to_string(),
        period: period.period_code.clone(),
        period_name: report::period_name_of(&period.period_code),
        period_status: period.status,
        period_start: period.start_date,
        period_end: period.end_date,
        ytd_start,
        current_label: CURRENT_LABEL.to_string(),
        ytd_label: YTD_LABEL.to_string(),
        generated_at: Utc::now(),
        items: computed.items,
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
    if report::parse_period(text).is_none() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("period 必须是 yyyy-MM（月份 01–12），收到「{text}」"),
        ));
    }
    Ok(text.to_string())
}

/// 数据层「期间不存在 / 配置错」→ 400 / 500。
fn period_error(period: &str, message: String) -> ApiError {
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

/// 在 REPEATABLE READ 快照里算报表，并把数据层错误翻成 HTTP 错误。
async fn snapshot_or_error(period: &str) -> std::result::Result<IncomeSnapshot, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启利润表快照事务"))?;
    // ★ L11：整张报表在一个 REPEATABLE READ 快照里算完。
    //   `SET TRANSACTION` 必须是 BEGIN 之后的第一条语句（PG 规定，否则 25001）。
    if let Err(error) = tx
        .execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .await
    {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "设置利润表事务隔离级别"));
    }
    match build_snapshot(&tx, period).await {
        Ok(snapshot) => {
            if let Err(error) = tx.commit().await {
                return Err(utils::db_error(error, "提交利润表快照事务"));
            }
            Ok(snapshot)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            match error {
                DbError::InvalidValue(message) => Err(period_error(period, message)),
                other => Err(utils::db_error(other, "编制利润表")),
            }
        }
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

/// 利润表。
#[nik::permission(
    path = "/business/erp/report/income-statement",
    method = "GET",
    permission = "business:erp:report:income-statement"
)]
#[nik::get("/business/erp/report/income-statement")]
pub async fn income_statement_handler(
    Query(query): Query<IncomeStatementQuery>,
) -> ResResult<IncomeStatementRes> {
    let period = validate_period(query.period.as_deref())?;
    let snapshot = snapshot_or_error(&period).await?;
    let company = company_name().await;
    Ok(Res::ok(into_response(&company, "元", snapshot)))
}

/// 利润表导出（xlsx，复用 impexp 的表格写出能力，与资产负债表同一套写法）。
#[nik::permission(
    path = "/business/erp/report/income-statement/export",
    method = "GET",
    permission = "business:erp:report:export"
)]
#[nik::get("/business/erp/report/income-statement/export")]
pub async fn income_statement_export_handler(
    Query(query): Query<IncomeStatementQuery>,
) -> std::result::Result<Response, ApiError> {
    let period = validate_period(query.period.as_deref())?;
    let snapshot = snapshot_or_error(&period).await?;
    let company = company_name().await;
    let report = into_response(&company, "元", snapshot);
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

/// 把报表拍成「项目 / 本期金额 / 本年累计」三列的表格行。
pub fn export_table(
    report: &IncomeStatementRes,
) -> (Vec<&'static str>, Vec<HashMap<String, String>>) {
    const HEADERS: [&str; 3] = ["项目", "本期金额", "本年累计"];
    let mut rows: Vec<HashMap<String, String>> = Vec::new();

    // 表头信息（编制单位 / 期间 / 单位 / 累计起点）也进表格，导出的文件脱离页面也能看懂。
    let mut info = HashMap::new();
    info.insert(
        "项目".to_string(),
        format!("编制单位：{}", report.company_name),
    );
    info.insert(
        "本期金额".to_string(),
        format!("期间：{}（{}）", report.period_name, report.period_status),
    );
    info.insert(
        "本年累计".to_string(),
        format!(
            "单位：{}；本年累计从 {} 起算",
            report.unit, report.ytd_start
        ),
    );
    rows.push(info);

    let indent = |level: i16| "    ".repeat(usize::try_from(level.saturating_sub(1)).unwrap_or(0));
    for line in &report.items {
        let mut row: HashMap<String, String> = HashMap::new();
        row.insert(
            "项目".to_string(),
            format!("{}{}", indent(line.level), line.item_name),
        );
        row.insert(
            "本期金额".to_string(),
            report::money_text(Some(line.current)),
        );
        row.insert("本年累计".to_string(), report::money_text(Some(line.ytd)));
        rows.push(row);
    }

    // 结论行：勾稽一致 / 不平（不平的把差额写进文件，导出件也瞒不住）
    let mut tail = HashMap::new();
    tail.insert("项目".to_string(), report.check.message.clone());
    tail.insert(
        "本年累计".to_string(),
        format!(
            "未分配利润：期末 {} / 年初 {} / 变动 {}",
            report::money_text(Some(report.check.undistributed_profit_end)),
            report::money_text(Some(report.check.undistributed_profit_year_start)),
            report::money_text(Some(report.check.equity_change)),
        ),
    );
    rows.push(tail);
    (HEADERS.to_vec(), rows)
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher_query::tests::{DB_LOCK, ensure_db};
    use std::str::FromStr;

    const FIXTURE_WORD: &str = "ERPR2";

    /// 造一个科目发生额（不进库）。
    fn acct(
        code: &str,
        category: &str,
        direction: &str,
        period: i64,
        ytd: i64,
        cum: i64,
        before_year: i64,
    ) -> IncomeAccount {
        IncomeAccount {
            account_id: Uuid::nil(),
            account_code: code.to_string(),
            account_name: format!("科目{code}"),
            account_category: category.to_string(),
            balance_direction: direction.to_string(),
            net_period: Decimal::from(period),
            net_ytd: Decimal::from(ytd),
            net_cum: Decimal::from(cum),
            net_before_year: Decimal::from(before_year),
        }
    }

    /// 损益类科目：收入（贷方）用负数 net 表达「贷方发生额」。
    fn revenue_account(code: &str, amount: i64) -> IncomeAccount {
        acct(code, "PROFIT_LOSS", "CREDIT", -amount, -amount, -amount, 0)
    }

    /// 损益类科目：成本 / 费用（借方）用正数 net。
    fn expense_account(code: &str, amount: i64) -> IncomeAccount {
        acct(code, "PROFIT_LOSS", "DEBIT", amount, amount, amount, 0)
    }

    /// 精确到分的科目（`0.0001` 这类用例要用十进制字符串造）。
    fn acct_dec(
        code: &str,
        category: &str,
        direction: &str,
        period: &str,
        ytd: &str,
        cum: &str,
        before_year: &str,
    ) -> IncomeAccount {
        IncomeAccount {
            account_id: Uuid::nil(),
            account_code: code.to_string(),
            account_name: format!("科目{code}"),
            account_category: category.to_string(),
            balance_direction: direction.to_string(),
            net_period: Decimal::from_str(period).expect("十进制"),
            net_ytd: Decimal::from_str(ytd).expect("十进制"),
            net_cum: Decimal::from_str(cum).expect("十进制"),
            net_before_year: Decimal::from_str(before_year).expect("十进制"),
        }
    }

    /// 造一个报表项（与 V0184 的写法一致）。
    fn item(
        code: &str,
        name: &str,
        level: i16,
        pattern: Option<&str>,
        formula: Option<&str>,
        order: i32,
    ) -> ReportItemDef {
        ReportItemDef {
            item_code: code.to_string(),
            item_name: name.to_string(),
            item_level: level,
            parent_code: None,
            side: "EQUITY".to_string(),
            account_pattern: pattern.map(str::to_string),
            formula: formula.map(str::to_string),
            is_deduction: false,
            sort_order: order,
        }
    }

    /// 与 V0184 种子逐字一致的最小利润表结构（单测不依赖库，配置改了这里要跟着改）。
    fn income_items() -> Vec<ReportItemDef> {
        vec![
            item("REVENUE", "一、营业收入", 1, Some("5001,5051"), None, 10),
            item(
                "COST_OF_SALES",
                "减：营业成本",
                2,
                Some("5401,5402,6401"),
                None,
                11,
            ),
            item("TAX_SURCHARGE", "减：税金及附加", 2, Some("5403"), None, 12),
            item("SELLING_EXPENSE", "减：销售费用", 2, Some("5601"), None, 13),
            item("ADMIN_EXPENSE", "减：管理费用", 2, Some("5602"), None, 14),
            item("FINANCE_EXPENSE", "减：财务费用", 2, Some("5603"), None, 15),
            item(
                "OPERATING_PROFIT",
                "二、营业利润",
                1,
                None,
                Some(
                    "REVENUE - COST_OF_SALES - TAX_SURCHARGE - SELLING_EXPENSE - ADMIN_EXPENSE - FINANCE_EXPENSE",
                ),
                20,
            ),
            item(
                "NON_OPERATING_INCOME",
                "加：营业外收入",
                2,
                Some("5301"),
                None,
                21,
            ),
            item(
                "NON_OPERATING_EXPENSE",
                "减：营业外支出",
                2,
                Some("5711"),
                None,
                22,
            ),
            item(
                "TOTAL_PROFIT",
                "三、利润总额",
                1,
                None,
                Some("OPERATING_PROFIT + NON_OPERATING_INCOME - NON_OPERATING_EXPENSE"),
                30,
            ),
            item("INCOME_TAX", "减：所得税费用", 2, Some("5801"), None, 31),
            item(
                "NET_PROFIT",
                "四、净利润",
                1,
                None,
                Some("TOTAL_PROFIT - INCOME_TAX"),
                40,
            ),
        ]
    }

    fn line_of<'a>(computed: &'a ComputedIncome, code: &str) -> &'a IncomeLineRes {
        computed
            .items
            .iter()
            .find(|row| row.item_code == code)
            .unwrap_or_else(|| panic!("利润表里没有 {code} 行"))
    }

    // ---------- ① 只有一笔收入 ----------

    /// ① 只有一笔收入（贷 5001）→ 营业收入 = X，净利润 = X。
    #[test]
    fn r2_only_revenue_makes_net_profit_equal_revenue() {
        let accounts = vec![revenue_account("5001", 1_000)];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(computed.current_of("REVENUE"), Decimal::from(1_000));
        assert_eq!(computed.ytd_of("REVENUE"), Decimal::from(1_000));
        assert_eq!(
            computed.current_of("OPERATING_PROFIT"),
            Decimal::from(1_000)
        );
        assert_eq!(computed.net_profit_current(), Decimal::from(1_000));
        assert_eq!(computed.net_profit_ytd(), Decimal::from(1_000));
        assert!(computed.unmapped.is_empty(), "5001 已被映射，不该未归类");
        println!("[R2-①] 一笔收入 1000 → 营业收入 1000 / 净利润 1000 ✓");
    }

    // ---------- ② 收入 + 成本 ----------

    /// ② 收入 + 成本 → 营业利润 = 收入 − 成本。
    #[test]
    fn r2_cost_reduces_operating_profit() {
        let accounts = vec![revenue_account("5001", 1_000), expense_account("5401", 600)];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(computed.current_of("COST_OF_SALES"), Decimal::from(600));
        assert_eq!(
            computed.current_of("OPERATING_PROFIT"),
            Decimal::from(400),
            "营业利润 = 1000 − 600"
        );
        assert_eq!(computed.net_profit_current(), Decimal::from(400));
        // 6401 是存量成本科目，必须与 5401 一起进「营业成本」
        let accounts = vec![revenue_account("5001", 1_000), expense_account("6401", 250)];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(
            computed.current_of("COST_OF_SALES"),
            Decimal::from(250),
            "6401 出库成本必须进营业成本（否则净利润与权益变动对不上）"
        );
        assert_eq!(computed.net_profit_current(), Decimal::from(750));
        println!("[R2-②] 收入 1000 − 成本 600 = 400；6401 也算成本 ✓");
    }

    // ---------- ③ 期间费用 ----------

    /// ③ 期间费用（管理费用）→ 营业利润再减。
    #[test]
    fn r2_period_expense_reduces_operating_profit() {
        let accounts = vec![
            revenue_account("5001", 1_000),
            expense_account("5401", 600),
            expense_account("5602", 150),
            expense_account("5603", 50),
        ];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(computed.current_of("ADMIN_EXPENSE"), Decimal::from(150));
        assert_eq!(
            computed.current_of("OPERATING_PROFIT"),
            Decimal::from(200),
            "1000 − 600 − 150 − 50"
        );
        assert_eq!(computed.net_profit_current(), Decimal::from(200));
        println!("[R2-③] 再减管理费用 150 / 财务费用 50 → 营业利润 200 ✓");
    }

    // ---------- ④ 所得税 ----------

    /// ④ 所得税 → 净利润 = 利润总额 − 所得税。
    #[test]
    fn r2_income_tax_is_deducted_after_total_profit() {
        let accounts = vec![
            revenue_account("5001", 1_000),
            expense_account("5401", 600),
            expense_account("5602", 100),
            revenue_account("5301", 40), // 营业外收入（贷方）
            expense_account("5711", 10), // 营业外支出（借方）
            expense_account("5801", 82), // 所得税费用（借方）
        ];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(computed.current_of("OPERATING_PROFIT"), Decimal::from(300));
        assert_eq!(
            computed.current_of("NON_OPERATING_INCOME"),
            Decimal::from(40)
        );
        assert_eq!(
            computed.current_of("TOTAL_PROFIT"),
            Decimal::from(330),
            "营业利润 300 + 营业外收入 40 − 营业外支出 10"
        );
        assert_eq!(
            computed.net_profit_current(),
            Decimal::from(248),
            "利润总额 330 − 所得税 82"
        );
        println!("[R2-④] 利润总额 330 − 所得税 82 = 净利润 248 ✓");
    }

    // ---------- ⑤ 红冲收入 ----------

    /// ⑤ 红冲收入（借 5001）→ 收入减少。
    ///
    /// 红字凭证在库里是**借贷互换**（`is_red=1`，金额恒正），落到分录上就是「借 5001」，
    /// 净额方向与正数收入相反 —— 报表金额必须跟着减少。
    #[test]
    fn r2_red_reversal_reduces_revenue() {
        let accounts = vec![
            // 正常收入 1000（贷 5001）+ 红冲 300（借 5001）→ 净 700
            acct("5001", "PROFIT_LOSS", "CREDIT", -700, -700, -700, 0),
        ];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(computed.current_of("REVENUE"), Decimal::from(700));
        assert_eq!(computed.net_profit_current(), Decimal::from(700));

        // 红冲把收入冲成负数时必须如实显示负数（不许夹成 0）
        let accounts = vec![acct("5001", "PROFIT_LOSS", "CREDIT", 300, 300, 300, 0)];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(
            computed.current_of("REVENUE"),
            Decimal::from(-300),
            "冲多了就是负数收入，不许夹 0"
        );
        println!("[R2-⑤] 红冲收入 300 → 收入 700；冲成负数也如实显示 ✓");
    }

    // ---------- ⑥ 本年累计（纯逻辑部分：两段窗口各自独立） ----------

    /// ⑥ 本年累计与本期是**两段不同的窗口**：本期只含当月，累计含 1 月以来。
    #[test]
    fn r2_current_and_ytd_are_independent_windows() {
        // 9 月发生 300，10 月发生 200 → 本期 200 / 累计 500
        let accounts = vec![acct("5001", "PROFIT_LOSS", "CREDIT", -200, -500, -500, 0)];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(line_of(&computed, "REVENUE").current, Decimal::from(200));
        assert_eq!(line_of(&computed, "REVENUE").ytd, Decimal::from(500));
        assert_eq!(computed.net_profit_current(), Decimal::from(200));
        assert_eq!(computed.net_profit_ytd(), Decimal::from(500));
        println!("[R2-⑥] 本期 200 / 本年累计 500（两段窗口独立）✓");
    }

    // ---------- ⑦ 空期间 ----------

    /// ⑦ 空期间（一个科目都没有）→ 所有行 0（含合计行与未归类行），不是 `null`。
    #[test]
    fn r2_empty_period_is_all_zero() {
        let computed = compute(&income_items(), &[]).expect("计算");
        assert_eq!(
            computed.items.len(),
            income_items().len() + 1,
            "12 行 + 未归类行"
        );
        for row in &computed.items {
            assert!(
                row.current.is_zero() && row.ytd.is_zero(),
                "{} 应为 0，实际 {} / {}",
                row.item_code,
                row.current,
                row.ytd
            );
        }
        assert!(computed.unmapped.is_empty());
        println!("[R2-⑦] 空期间 → 全 0（含未归类行）✓");
    }

    // ---------- ⑧ 勾稽：净利润累计 = 权益变动（纯逻辑） ----------

    /// ⑧ 勾稽：收入 1000 / 成本 600 / 费用 100 → 净利润 300，未分配利润变动 300 → 平。
    #[test]
    fn r2_check_ties_net_profit_to_equity_movement() {
        let accounts = vec![
            revenue_account("5001", 1_000),
            expense_account("5401", 600),
            expense_account("5602", 100),
        ];
        let computed = compute(&income_items(), &accounts).expect("计算");
        let check = build_check(
            &computed,
            &accounts,
            UNDISTRIBUTED_PATTERN_FALLBACK,
            Decimal::ZERO,
            Decimal::ZERO,
            Vec::new(),
        );
        assert_eq!(check.net_profit_ytd, Decimal::from(300));
        assert_eq!(
            check.equity_change,
            Decimal::from(300),
            "未分配利润变动 = −Σ损益类净额 = 300"
        );
        assert_eq!(check.diff, Decimal::ZERO);
        assert!(check.balanced, "必须平：{}", check.message);
        assert!(check.message.contains("勾稽一致"), "{}", check.message);
        println!("[R2-⑧] 净利润 300 = 权益变动 300 → 勾稽一致 ✓");
    }

    /// ⑧b 未归类的损益科目（如 5111 投资收益）必须让勾稽**不平**并点名科目。
    #[test]
    fn r2_unmapped_profit_loss_breaks_the_check_and_names_account() {
        let accounts = vec![
            revenue_account("5001", 1_000),
            revenue_account("5111", 50), // 卡面结构没有这一行
        ];
        let computed = compute(&income_items(), &accounts).expect("计算");
        assert_eq!(
            computed.net_profit_ytd(),
            Decimal::from(1_000),
            "净利润只含已映射的行"
        );
        assert_eq!(computed.unmapped_effect_ytd(), Decimal::from(50));
        assert_eq!(computed.unmapped.len(), 1);
        let check = build_check(
            &computed,
            &accounts,
            UNDISTRIBUTED_PATTERN_FALLBACK,
            Decimal::ZERO,
            Decimal::ZERO,
            Vec::new(),
        );
        assert!(!check.balanced, "漏一行映射就必须报不平");
        assert_eq!(
            check.diff,
            Decimal::from(-50),
            "净利润 1000 − 权益变动 1050"
        );
        assert_eq!(check.diff_text, "-50.00");
        assert!(
            check.hints.iter().any(|hint| hint.contains("5111")),
            "提示必须点名科目：{:?}",
            check.hints
        );
        let row = line_of(&computed, UNMAPPED_PROFIT_LOSS);
        assert_eq!(row.ytd, Decimal::from(50));
        assert_eq!(row.accounts, vec!["5111".to_string()]);
        println!("[R2-⑧b] 5111 未映射 → 不平 −50.00 且点名科目 ✓");
    }

    // ---------- ⑨ 期间不存在 ----------

    /// ⑨ 期间格式非法 → 400；期间不存在（真库）→ 400 + 66010。
    #[test]
    fn r2_invalid_period_is_rejected() {
        assert!(validate_period(None).is_err(), "period 必填");
        assert!(validate_period(Some("   ")).is_err());
        assert!(validate_period(Some("2026-13")).is_err(), "13 月不存在");
        assert!(validate_period(Some("2026-1")).is_err(), "必须两位月份");
        assert_eq!(
            validate_period(Some(" 2026-10 ")).ok().as_deref(),
            Some("2026-10")
        );
        assert_eq!(
            ytd_start_of("2026-10"),
            NaiveDate::from_ymd_opt(2026, 1, 1),
            "本年累计从 1 月 1 日起算"
        );
        println!("[R2-⑨] period 校验 / 本年累计起点 ✓");
    }

    // ---------- ⑩ 不平场景（人为改一个数） ----------

    /// ⑩ 人为把「本年利润」多记 400 → `balanced=false`、`diff` 精确是 −400.00。
    #[test]
    fn r2_imbalance_is_reported_with_exact_diff() {
        // 利润表侧：收入 1000 / 成本 600 → 净利润 400
        // 权益侧：未分配利润 = 400（损益贡献）+ 400（3104 直接发生额）→ 变动 800
        let accounts = vec![
            revenue_account("5001", 1_000),
            expense_account("5401", 600),
            acct("3104", "EQUITY", "CREDIT", -400, -400, -400, 0),
        ];
        let computed = compute(&income_items(), &accounts).expect("计算");
        let check = build_check(
            &computed,
            &accounts,
            UNDISTRIBUTED_PATTERN_FALLBACK,
            Decimal::ZERO,
            Decimal::ZERO,
            Vec::new(),
        );
        assert_eq!(check.net_profit_ytd, Decimal::from(400));
        assert_eq!(
            check.equity_change,
            Decimal::from(800),
            "权益变动 400 + 400"
        );
        assert!(!check.balanced);
        assert_eq!(check.diff, Decimal::from(-400));
        assert_eq!(check.diff_text, "-400.00");
        assert!(check.message.contains("不平"), "{}", check.message);
        assert!(
            check
                .hints
                .iter()
                .any(|hint| hint.contains("本年利润 / 利润分配")),
            "必须指出 3103/3104 直接发生额：{:?}",
            check.hints
        );
        println!("[R2-⑩] 人为改数 → 不平 −400.00 且指出成因 ✓");
    }

    /// ⑪ 差额 0.0001 不许被舍成 0.00（与资产负债表同一条硬红线）。
    #[test]
    fn r2_tiny_diff_is_not_rounded_away() {
        let accounts = vec![
            revenue_account("5001", 100),
            // 3104 上多 0.0001 的贷方余额 → 权益变动比净利润多 0.0001
            acct_dec(
                "3104", "EQUITY", "CREDIT", "0.0001", "0.0001", "0.0001", "0",
            ),
        ];
        let computed = compute(&income_items(), &accounts).expect("计算");
        let check = build_check(
            &computed,
            &accounts,
            UNDISTRIBUTED_PATTERN_FALLBACK,
            Decimal::ZERO,
            Decimal::ZERO,
            Vec::new(),
        );
        assert!(!check.balanced, "0.0001 的不平也必须报");
        assert_eq!(
            check.diff_text, "0.0001",
            "2 位小数会抹成 0.00 → 必须保留 4 位"
        );
        println!("[R2-⑪] 0.0001 的差额不被抹平 ✓");
    }

    /// ⑫ 公式引用未知项必须报错（不许静默 0）。
    #[test]
    fn r2_formula_with_unknown_item_fails_loudly() {
        let mut items = income_items();
        items.push(item(
            "BROKEN",
            "坏公式",
            1,
            None,
            Some("REVENUE - NOPE"),
            50,
        ));
        let accounts = vec![revenue_account("5001", 1)];
        let error = compute(&items, &accounts).expect_err("必须报错");
        assert!(error.contains("NOPE"), "错误里要点名缺的项：{error}");
        println!("[R2-⑫] 公式引用未知项 → 显式报错 ✓");
    }

    // ---------- 真库用例（ERP_DB_URL） ----------

    /// 清夹具 + 建 2026-09 / 2026-10 / 2000-12 三个期间（**幂等**）。
    async fn setup_fixture() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        let statements = [
            format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'"),
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) VALUES (2026, 9, '2026-09', '2026-09-01', '2026-09-30', 'CLOSED') \
             ON CONFLICT (period_code) DO NOTHING"
                .to_string(),
            "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
             start_date, end_date, status) VALUES (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN') \
             ON CONFLICT (period_code) DO NOTHING"
                .to_string(),
            // 2000-12：早于库里任何凭证 → 「空期间全 0」用例的唯一确定性口径
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
                VALUES ('{FIXTURE_WORD}', '{no}', '{period}', DATE '{date}', 'ERPR2 夹具', \
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
             SELECT v.id, 1, 'ERPR2 借方', d.id, d.account_code, d.account_name, {amount}, 0 FROM v, d \
             UNION ALL \
             SELECT v.id, 2, 'ERPR2 贷方', c.id, c.account_code, c.account_name, 0, {amount} FROM v, c"
        );
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("建凭证失败：{error}\n{sql}"));
    }

    /// 在 REPEATABLE READ 事务里跑一次（与接口层同一条路径）。
    async fn run(period: &str) -> IncomeSnapshot {
        let tx = db().begin().await.expect("开启事务");
        tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .await
            .expect("设置隔离级别");
        let snapshot = build_snapshot(&tx, period).await.expect("编制报表");
        tx.commit().await.expect("提交");
        snapshot
    }

    async fn cleanup() {
        db().execute_with_params(
            &format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'"),
            vec![],
        )
        .await
        .expect("清理夹具凭证");
    }

    /// ⑨b 真库：期间不存在 → `InvalidValue`（接口层 400 + 66010）。
    #[tokio::test]
    async fn r2_real_missing_period_is_rejected() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
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
        println!("[R2-⑨b] 空月份 1999-01 → 400 + 66010 ✓");
    }

    /// ⑦b 真库：空期间（2000-12）→ 全 0 且勾稽平。
    #[tokio::test]
    async fn r2_real_empty_period_is_all_zero() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let snapshot = run("2000-12").await;
        let check = snapshot.check();
        assert!(check.balanced, "空期间必须平：{}", check.message);
        assert_eq!(check.net_profit_ytd, Decimal::ZERO);
        assert_eq!(check.equity_change, Decimal::ZERO);
        for row in &snapshot.computed.items {
            assert!(
                row.current.is_zero() && row.ytd.is_zero(),
                "{} 应为 0，实际 {} / {}",
                row.item_code,
                row.current,
                row.ytd
            );
        }
        println!("[R2-⑦b] 真库空期间 2000-12 → 全 0 且勾稽平 ✓");
    }

    /// ⑥b 真库：跨月两笔（9 月 300 + 10 月 200）→ 本期 200 / 累计 500。
    #[tokio::test]
    async fn r2_real_ytd_accumulates_across_months() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let baseline = run("2026-10").await;
        let base_current = baseline.computed.current_of(REVENUE);
        let base_ytd = baseline.computed.ytd_of(REVENUE);

        // 借 1002 银行存款 / 贷 5001 主营业务收入
        post_voucher("0101", "2026-09-20", "2026-09", "1002", "5001", 300).await;
        post_voucher("0102", "2026-10-20", "2026-10", "1002", "5001", 200).await;

        let snapshot = run("2026-10").await;
        let computed = &snapshot.computed;
        assert_eq!(
            computed.current_of(REVENUE) - base_current,
            Decimal::from(200),
            "本期只含 10 月"
        );
        assert_eq!(
            computed.ytd_of(REVENUE) - base_ytd,
            Decimal::from(500),
            "本年累计含 9 月 + 10 月"
        );
        assert_eq!(
            computed.net_profit_ytd() - baseline.computed.net_profit_ytd(),
            Decimal::from(500)
        );
        cleanup().await;
        println!("[R2-⑥b] 真库跨月：本期 200 / 累计 500 ✓");
    }

    /// ⑧c 真库：收入 + 成本 + 费用 + 税 一组凭证 → 净利润累计 = 权益变动，
    /// 且与**资产负债表**的「未分配利润」逐分对得上（两张报表同一口径）。
    #[tokio::test]
    async fn r2_real_net_profit_ties_to_balance_sheet_undistributed_profit() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let baseline = run("2026-10").await;
        let base_check = baseline.check();
        assert!(
            base_check.balanced,
            "用例前提：基线必须平（红了说明库里有别处留下的不平数据：{} / {:?}）",
            base_check.message, base_check.hints
        );

        // 确认与资产负债表同一口径：未分配利润期末 = R1 报表里的 UNDISTRIBUTED_PROFIT
        let tx = db().begin().await.expect("开启事务");
        let bs = report::build_snapshot(&tx, "2026-10")
            .await
            .expect("资产负债表");
        let _ = tx.rollback().await;
        let bs_undistributed = bs
            .sheet
            .equity
            .iter()
            .find(|row| row.item_code == UNDISTRIBUTED_ITEM)
            .and_then(|row| row.amount)
            .unwrap_or_default();
        assert_eq!(
            base_check.undistributed_profit_end, bs_undistributed,
            "利润表勾稽用的「未分配利润」必须与资产负债表逐分一致"
        );

        // 一笔完整业务：收入 1000 / 成本 600 / 管理费用 100 / 所得税 82
        post_voucher("0201", "2026-10-05", "2026-10", "1002", "5001", 1_000).await;
        post_voucher("0202", "2026-10-06", "2026-10", "5401", "1405", 600).await;
        post_voucher("0203", "2026-10-07", "2026-10", "5602", "1002", 100).await;
        post_voucher("0204", "2026-10-08", "2026-10", "5801", "2221.02", 82).await;

        let after = run("2026-10").await;
        let check = after.check();

        // 资产负债表侧的「未分配利润」（必须在清理凭证**之前**取，否则拿到的是基线）
        let tx = db().begin().await.expect("开启事务");
        let after_bs = report::build_snapshot(&tx, "2026-10")
            .await
            .expect("资产负债表");
        let _ = tx.rollback().await;
        let after_bs_undistributed = after_bs
            .sheet
            .equity
            .iter()
            .find(|row| row.item_code == UNDISTRIBUTED_ITEM)
            .and_then(|row| row.amount)
            .unwrap_or_default();

        // 先清理再做断言：哪怕红了也不给后面的用例留脏数据
        cleanup().await;

        assert_eq!(
            after.computed.current_of(REVENUE) - baseline.computed.current_of(REVENUE),
            Decimal::from(1_000)
        );
        assert_eq!(
            after.computed.net_profit_ytd() - baseline.computed.net_profit_ytd(),
            Decimal::from(218),
            "净利润 = 1000 − 600 − 100 − 82"
        );
        assert!(
            check.balanced,
            "净利润累计必须等于未分配利润变动：{} / hints={:?}",
            check.message, check.hints
        );
        assert_eq!(
            check.equity_change - base_check.equity_change,
            Decimal::from(218),
            "未分配利润本年变动 +218"
        );
        assert_eq!(check.diff, base_check.diff, "我这 4 张凭证必须不改变差额");

        // 资产负债表侧的「未分配利润」也要 +218（两张报表同口径的硬证据）
        assert_eq!(
            after_bs_undistributed - bs_undistributed,
            Decimal::from(218),
            "资产负债表「未分配利润」变动 218"
        );
        println!("[R2-⑧c] 真库：净利润 218 = 权益变动 218 = 资产负债表未分配利润变动 ✓");
    }

    /// ⑬ 真库：配置一致性（12 项 / 公式引用都在 / 标准损益科目全部有映射 / 无重复匹配）。
    #[tokio::test]
    async fn r2_real_config_is_consistent() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let tx = db().begin().await.expect("开启事务");
        let items = report::load_items_of(&tx, REPORT_CODE)
            .await
            .expect("读利润表配置");
        let accounts = report::load_accounts(
            &tx,
            NaiveDate::from_ymd_opt(2026, 10, 1).expect("日期"),
            NaiveDate::from_ymd_opt(2026, 10, 31).expect("日期"),
        )
        .await
        .expect("读科目");
        let pattern = load_undistributed_pattern(&tx)
            .await
            .expect("读未分配利润口径");
        let _ = tx.rollback().await;

        assert_eq!(items.len(), 12, "INCOME_STATEMENT 应有 12 个启用报表项");
        assert!(
            items.iter().any(|row| row.item_code == NET_PROFIT),
            "净利润行必须在"
        );
        assert_eq!(
            pattern, UNDISTRIBUTED_PATTERN_FALLBACK,
            "未分配利润口径应取自资产负债表（与 V0183 种子一致）"
        );

        // 公式引用的项必须都存在
        for item in &items {
            let Some(formula) = item.formula.as_deref() else {
                continue;
            };
            for token in formula.split_whitespace() {
                if token == "+" || token == "-" {
                    continue;
                }
                assert!(
                    items.iter().any(|row| row.item_code == token),
                    "{} 的公式引用了不存在的 {}",
                    item.item_code,
                    token
                );
            }
        }

        // 标准损益类科目必须被**恰好一行**匹配（5111 投资收益按设计不映射）
        let standard = [
            "5001", "5051", "5301", "5401", "5402", "5403", "5601", "5602", "5603", "5711", "5801",
            "6401",
        ];
        let mut problems: Vec<String> = Vec::new();
        for account in &accounts {
            let hits: Vec<&str> = items
                .iter()
                .filter(|item| {
                    item.account_pattern.as_deref().is_some_and(|pattern| {
                        report::match_account(
                            pattern,
                            &account.account_code,
                            &account.account_category,
                        )
                    })
                })
                .map(|item| item.item_code.as_str())
                .collect();
            if account.account_category != "PROFIT_LOSS" {
                if !hits.is_empty() {
                    problems.push(format!(
                        "{} 不是损益类却被利润表匹配到 {:?}",
                        account.account_code, hits
                    ));
                }
                continue;
            }
            if standard.contains(&account.account_code.as_str()) {
                if hits.len() != 1 {
                    problems.push(format!(
                        "标准损益科目 {} 的匹配行数 = {}（{:?}）",
                        account.account_code,
                        hits.len(),
                        hits
                    ));
                }
            } else if hits.len() > 1 {
                problems.push(format!(
                    "{} 被多行匹配（{:?}）—— 同一笔钱会算两遍",
                    account.account_code, hits
                ));
            }
        }
        assert!(problems.is_empty(), "利润表配置有问题：{problems:?}");
        println!("[R2-⑬] 配置一致：12 项 / 公式闭合 / 标准损益科目各匹配一行 ✓");
    }

    /// ⑭ 真库：生产数据出表不报错（有账就出表，没损益也算平）。
    #[tokio::test]
    async fn r2_real_production_period_is_balanced() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let snapshot = run("2026-10").await;
        let report = into_response("云枢", "元", snapshot);
        assert_eq!(report.period, "2026-10");
        assert_eq!(report.current_label, CURRENT_LABEL);
        assert_eq!(report.ytd_label, YTD_LABEL);
        assert_eq!(
            report.ytd_start,
            NaiveDate::from_ymd_opt(2026, 1, 1).expect("日期")
        );
        assert!(
            report.check.balanced,
            "生产数据必须勾稽平：{} / hints={:?}",
            report.check.message, report.check.hints
        );
        // 导出件能生成且带表头
        let (headers, rows) = export_table(&report);
        assert_eq!(headers, vec!["项目", "本期金额", "本年累计"]);
        assert!(
            rows.len() > report.items.len(),
            "表头信息行 + 报表行 + 结论行"
        );
        println!(
            "[R2-⑭] 生产期间 2026-10 出表：净利润 {} / 勾稽 {} ✓",
            report.check.net_profit_ytd, report.check.diff_text
        );
    }
}

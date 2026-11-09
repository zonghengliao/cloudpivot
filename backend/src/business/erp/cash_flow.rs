//! 现金流量表（ERP-R3，**简化版**）：按「现金收支流水 + 对方科目」直接分类成经营 / 投资 / 筹资。
//! **只读**。
//!
//! 承接 [`crate::business::erp::report`]（ERP-R1）的框架：同一套期间口径
//! （[`report::resolve_period`]）、同一套科目匹配语法（[`report::match_account`]）、
//! 同一个金额格式（[`report::money_text`] / [`report::diff_text`]）、同一套导出
//! （`impexp::engine::write_sheet` / `handler::xlsx_response`）与同一个 REPEATABLE READ
//! 快照模式。**本模块不重写框架，也不碰凭证 / 科目余额逻辑**（硬红线 1 / 4）。
//!
//! 配置来源 `backend/migrations/business/V0185__erp_cashflow.up.sql`
//! （`biz_cashflow_item` 的 12 行对方科目分类），取数来源 V0149 的
//! `biz_voucher` / `biz_voucher_entry`。
//!
//! # 接口
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/cash-flow?period=2026-10` | `business:erp:report:cash-flow` | 现金流量表（本期 + 本年累计 + 勾稽货币资金） |
//! | GET | `/business/erp/report/cash-flow/export?period=2026-10` | `business:erp:report:export` | 同一份报表导出 xlsx |
//!
//! # 简化口径（卡面 §一，**必须先读**）
//!
//! 完整现金流量表要三件事：①经营 / 投资 / 筹资三分；②直接法还是间接法；
//! ③现金流量项目表 + 人工指定。第三件事要求会计在每张凭证上再录一遍「现金流量项目」，
//! 工作量翻倍且极易漏录 —— 本卡不做。
//!
//! 本模块的做法是**直接读凭证**，按对方科目自动分类：
//!
//! ```text
//! 现金类科目   = 科目编码以 '1001'（库存现金）/ '1002'（银行存款）开头
//! 对方科目     = 同一张凭证里的非现金分录，按科目汇总净额
//! 对方净额贷方（贷 − 借 > 0） → 现金流入，金额 = 该净额
//! 对方净额借方（借 − 贷 > 0） → 现金流出，金额 = 该净额绝对值
//! 分类规则     = `biz_cashflow_item`：对方科目模式 → (活动, 方向, 明细行)
//! 分类不出来   → 归「其他」，并在 `warnings` 里点名科目与笔数（**绝不静默丢**）
//! ```
//!
//! ## 为什么金额用「对方科目净额」而不是「现金分录金额」
//!
//! 复式记账下每张 POSTED 凭证 `Σ借 = Σ贷`，因此同一张凭证里
//! `Σ非现金(贷−借) = Σ现金(借−贷) = 现金净流量`。按对方科目分组求和，
//! 多借多贷的复合凭证（一笔收款对应应收账款 + 工资 + 收入）天然被**精确拆分**，
//! 不需要按比例分摊（分摊会引入尾差，报表上会出现 0.01 的假不平）。
//! 差额 `现金净流量 − Σ对方净额` 只有一种来源：**凭证本身借贷不平**（分录被删 / 被改），
//! 这部分直接进「其他 - 分录不平差额」并进 `warnings`。
//!
//! ## 与卡面口径的两处刻意差异（都已写进文档）
//!
//! ① **现金类科目只认 `1001` / `1002` 前缀**（卡面原文）。V0183 补的 `1012 其他货币资金`
//!    不进现金类 —— 它一旦真有进出，报表会把它归「其他」并在 `warnings` 里点名，
//!    同时勾稽（期末现金 vs 资产负债表货币资金）会红。宁可闹钟响，不做静默口径。
//! ② **勾稽右式取资产负债表自己的数**：右式不是本模块用 `1001+1002` 再算一遍，
//!    而是复用 R1 的 `report::compute` 对 `BALANCE_SHEET / MONETARY_FUNDS`
//!    算出「货币资金」——两张报表只有一份「货币资金」口径（与 ERP-R2 对
//!    「未分配利润」的处理完全同构）。
//!
//! # 核心不变量
//!
//! ```text
//! 期末现金及现金等价物余额 ≡ 资产负债表「货币资金」
//! 期末余额                 ≡ 期初余额 + 本期净增加额
//! ```
//!
//! 第一条是**跨表**勾稽：不平说明现金类科目的流向没被完整捕获（分录被删、借贷不平、
//! 现金类科目口径不一致）。第二条是**报表自身的加减关系**。
//! 差一分钱都要报出来（`check.balanced == false`、`check.diff` 精确差额、
//! `check.message` 整句人话、`check.hints` 指出最可能的成因），**绝不四舍五入抹平**
//! （复用 [`report::diff_text`]）。
//!
//! # 快照隔离（硬红线 / L11）
//!
//! 整张报表在一个 **REPEATABLE READ** 事务里算完（期间 + 分类配置 + 两段窗口的分录 +
//! 科目余额 + 资产负债表口径，同一个快照）：`SET TRANSACTION ISOLATION LEVEL
//! REPEATABLE READ` 必须是 `BEGIN` 后的**第一条**语句（PG 规定），否则 25001。
//!
//! # 期间口径
//!
//! 与资产负债表 / 利润表**完全一致**（复用 [`report::resolve_period`]）：有期间行用它
//! （`OPEN` / `CLOSED` 都出表）；有账无期间 → 按自然月推算并标 `DERIVED` + 提示开账；
//! 无期间行且无凭证 → `400 + 66010`。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_cashflow_item 是报表配置、biz_account / biz_voucher / biz_voucher_entry
//! 是全局会计台账（V0137/V0149/V0185 建表，无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:report:cash-flow / :export 权限码控制，本模块无分页查询。
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::response::Response;
use nik::prelude::chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::erp::report::{self, AccountBalance, PERIOD_DERIVED, PeriodInfo, UnbalancedVoucherRes};
use crate::shared::error_code;
use crate::utils;

// ==================== 常量 ====================

/// 报表编码（出参 / 导出文件名用，配置在 `biz_cashflow_item` 表，不按 report_code 分区）。
pub const REPORT_CODE: &str = "CASH_FLOW";

/// 报表中文名（导出文件名 / 页头）。
pub const REPORT_NAME: &str = "现金流量表";

/// 现金类科目编码前缀（卡面 §一：1001 库存现金 / 1002 银行存款）。
pub const CASH_PREFIXES: [&str; 2] = ["1001", "1002"];

/// 分类表。
pub const ITEM_TABLE: &str = "biz_cashflow_item";

/// 三大活动（编码, 报表标题行）。
pub const ACTIVITIES: [(&str, &str); 3] = [
    ("OPERATING", "一、经营活动产生的现金流量"),
    ("INVESTING", "二、投资活动产生的现金流量"),
    ("FINANCING", "三、筹资活动产生的现金流量"),
];

/// 「其他」分区（分类不出来 / 分录不平差额）。
pub const OTHER_ACTIVITY: &str = "OTHER";

/// 「其他」分区标题。
pub const OTHER_ACTIVITY_NAME: &str = "四、未分类的现金收支（其他）";

/// 凭证借贷不平差额的归集行编码（不落库，运行时拼出来）。
pub const OTHER_UNBALANCED: &str = "OTHER_UNBALANCED";

/// 流入方向。
pub const INFLOW: &str = "INFLOW";

/// 流出方向。
pub const OUTFLOW: &str = "OUTFLOW";

/// 本期列名。
pub const CURRENT_LABEL: &str = "本期金额";

/// 本年累计列名。
pub const YTD_LABEL: &str = "本年累计";

/// 资产负债表编码（勾稽右式从这张表里读「货币资金」的口径）。
pub const BALANCE_REPORT_CODE: &str = "BALANCE_SHEET";

/// 资产负债表里「货币资金」的报表项编码。
pub const MONETARY_ITEM: &str = "MONETARY_FUNDS";

/// 「货币资金」的兜底匹配模式（与 V0183 种进 `BALANCE_SHEET` 的逐字一致）。
pub const MONETARY_PATTERN_FALLBACK: &str = "1001,1002,1012";

/// 简化口径说明（原样出现在出参里，前端与导出一并展示 —— 口径必须跟着报表走）。
pub const SIMPLIFIED_NOTE: &str = "简化版：按凭证里「现金类科目的收支流水 + 对方科目」直接分类成经营 / \
     投资 / 筹资（对方科目贷方净额=流入、借方净额=流出），不做间接法调整，也不要求会计在\
     凭证上手工指定现金流量项目。分类不出来的收支归「其他」并在 warnings 里点名。";

// ==================== 入参 ====================

/// 现金流量表查询参数（`period` 必填，`yyyy-MM`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CashFlowQuery {
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
}

// ==================== 配置 / 取数结构 ====================

/// `biz_cashflow_item` 的一行（纯数据，供 [`classify`] 消费）。
#[derive(Debug, Clone)]
pub struct CashFlowItemDef {
    /// 明细行编码（`OPER_SALES_RECEIPTS` …）。
    pub item_code: String,
    /// 明细行名称（纸面报表原文）。
    pub item_name: String,
    /// 活动（`OPERATING` / `INVESTING` / `FINANCING`）。
    pub activity: String,
    /// 方向（`INFLOW` / `OUTFLOW`）。
    pub direction: String,
    /// **对方科目**匹配模式（[`report::match_account`] 语法）。
    pub account_pattern: String,
    /// 显示顺序。
    pub sort_order: i32,
}

/// 一张凭证里的分录（只保留分类需要的列）。
#[derive(Debug, Clone)]
pub struct CashEntry {
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别（`ASSET` / `PROFIT_LOSS` …；`@CATEGORY:` 模式要用）。
    pub account_category: String,
    /// 借方金额。
    pub debit: Decimal,
    /// 贷方金额。
    pub credit: Decimal,
}

/// 一张含现金分录的凭证（分类的最小单位）。
#[derive(Debug, Clone)]
pub struct CashVoucher {
    /// 凭证号。
    pub voucher_no: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 单头借方合计（用于识别「分录被删」）。
    pub header_debit: Decimal,
    /// 单头贷方合计。
    pub header_credit: Decimal,
    /// 全部分录（含现金分录）。
    pub entries: Vec<CashEntry>,
}

/// 一张凭证拆出来的一条现金流（分类结果，纯数据）。
#[derive(Debug, Clone, PartialEq)]
pub struct VoucherFlow {
    /// 凭证号。
    pub voucher_no: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 活动（含 `OTHER`）。
    pub activity: String,
    /// 方向。
    pub direction: String,
    /// 归集的明细行编码。
    pub item_code: String,
    /// 明细行名称。
    pub item_name: String,
    /// 金额（恒为正数，方向看 `direction`）。
    pub amount: Decimal,
    /// 对方科目编码（`OTHER_UNBALANCED` 时为空）。
    pub account_code: String,
}

/// 一次分类的完整产物。
#[derive(Debug, Clone, Default)]
pub struct CashClassification {
    /// 现金流。
    pub flows: Vec<VoucherFlow>,
    /// 分录合计与单头不符的凭证。
    pub unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
    /// 分类过程的配置告警（如同一科目命中两行）。
    pub warnings: Vec<String>,
}

// ==================== 出参 ====================

/// 金额对（本期 / 本年累计）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct AmountPair {
    /// 本期金额。
    pub current: Decimal,
    /// 本年累计。
    pub ytd: Decimal,
}

impl AmountPair {
    /// 两列都为零。
    pub fn is_zero(&self) -> bool {
        self.current.is_zero() && self.ytd.is_zero()
    }
}

/// 现金流量表的一行。
#[derive(Debug, Clone, Serialize)]
pub struct CashLineRes {
    /// 明细行编码。
    pub item_code: String,
    /// 明细行名称。
    pub item_name: String,
    /// 本期金额。
    pub current: Decimal,
    /// 本年累计金额。
    pub ytd: Decimal,
    /// 实际命中的对方科目编码（可审计）。
    pub accounts: Vec<String>,
    /// 取数说明（`account_pattern` 原文；配置外的行为说明文字）。
    pub detail: Option<String>,
}

/// 一个活动分区（经营 / 投资 / 筹资 / 其他）。
#[derive(Debug, Clone, Serialize)]
pub struct ActivitySectionRes {
    /// 活动编码（`OPERATING` / `INVESTING` / `FINANCING` / `OTHER`）。
    pub activity: String,
    /// 活动标题（纸面报表原文，含「一、」这类前缀）。
    pub activity_name: String,
    /// 流入明细。
    pub inflow: Vec<CashLineRes>,
    /// 流入小计。
    pub inflow_total: AmountPair,
    /// 流出明细。
    pub outflow: Vec<CashLineRes>,
    /// 流出小计。
    pub outflow_total: AmountPair,
    /// 净额 = 流入小计 − 流出小计。
    pub net: AmountPair,
}

/// 未分类明细（活动未知，已归「其他」）。
#[derive(Debug, Clone, Serialize)]
pub struct UnclassifiedRes {
    /// 归集行编码（`OTHER_1122` / `OTHER_UNBALANCED`）。
    pub item_code: String,
    /// 行名称（含对方科目编码与名称）。
    pub item_name: String,
    /// 方向。
    pub direction: String,
    /// 本期金额。
    pub current: Decimal,
    /// 本年累计金额。
    pub ytd: Decimal,
}

/// 勾稽校验结果（期末现金余额 ≡ 资产负债表「货币资金」）。
#[derive(Debug, Clone, Serialize)]
pub struct CashFlowCheckRes {
    /// 两侧是否相等（精确比较，不做舍入）。
    pub balanced: bool,
    /// 本表算出来的期末现金余额。
    pub closing_balance: Decimal,
    /// 资产负债表「货币资金」（同一份 RBAC / 同一套科目口径）。
    pub balance_sheet_cash: Decimal,
    /// 差额 = 期末现金余额 − 货币资金，**不做任何舍入**。
    pub diff: Decimal,
    /// 差额人话（`'0.00'` / `'差 0.0100 元'` 的数值部分）。
    pub diff_text: String,
    /// 整句结论（前端直接显示）。
    pub message: String,
    /// 期末现金分两种情况：期初 + 净增加（本表）与科目余额（总账）。
    pub closing_by_ledger: Decimal,
    /// 本期现金分录笔数。
    pub cash_entry_count: usize,
    /// 未能分类的笔数（已归「其他」）。
    pub unclassified_count: usize,
    /// 未能分类的流入金额。
    pub unclassified_inflow: Decimal,
    /// 未能分类的流出金额。
    pub unclassified_outflow: Decimal,
    /// 分录合计与单头不符的凭证。
    pub unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
    /// 成因提示（人话）。
    pub hints: Vec<String>,
}

/// 现金流量表出参。
#[derive(Debug, Clone, Serialize)]
pub struct CashFlowRes {
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
    /// 是否简化版（恒 `true`；口径说明见 `simplified_note`）。
    pub simplified: bool,
    /// 简化口径说明（原样展示，口径必须跟着报表走）。
    pub simplified_note: String,
    /// 经营活动。
    pub operating: ActivitySectionRes,
    /// 投资活动。
    pub investing: ActivitySectionRes,
    /// 筹资活动。
    pub financing: ActivitySectionRes,
    /// 其他（未分类，含分录不平差额）。
    pub other: ActivitySectionRes,
    /// 现金及现金等价物净增加额。
    pub net_increase: AmountPair,
    /// 期初现金及现金等价物余额（本期列 = 期间开始日之前；累计列 = 本年 1 月 1 日之前）。
    pub opening_balance: AmountPair,
    /// 期末现金及现金等价物余额（两列都是期间结束日的余额）。
    pub closing_balance: AmountPair,
    /// 勾稽校验。
    pub check: CashFlowCheckRes,
    /// 告警（未分类笔数 / 配置冲突 / 分录不平 / 口径差异）。
    pub warnings: Vec<String>,
}

// ==================== 纯逻辑 ====================

/// 是否现金类科目（`1001` / `1002` 前缀）。**卡面 §一 的口径**。
pub fn is_cash_account(account_code: &str) -> bool {
    let code = account_code.trim();
    CASH_PREFIXES.iter().any(|prefix| code.starts_with(prefix))
}

/// 对方科目分组（同一科目在一张凭证里出现多行时合并净额）。
struct Counterpart {
    name: String,
    category: String,
    /// 贷 − 借（正数 = 现金流入）。
    net: Decimal,
}

/// 在某个方向上找该对方科目的明细行（`sort_order` 最小者优先）。
///
/// 命中多行时会把冲突写进 `notes`（去重）—— 同一笔钱被两行匹配就是静默多计，
/// 必须让配置的人看见。
fn pick_item<'a>(
    items: &'a [CashFlowItemDef],
    account_code: &str,
    account_category: &str,
    direction: &str,
    notes: &mut BTreeSet<String>,
) -> Option<&'a CashFlowItemDef> {
    let hits: Vec<&CashFlowItemDef> = items
        .iter()
        .filter(|item| item.direction == direction)
        .filter(|item| report::match_account(&item.account_pattern, account_code, account_category))
        .collect();
    let first = hits.first().copied()?;
    if hits.len() > 1 {
        let codes: Vec<&str> = hits.iter().map(|item| item.item_code.as_str()).collect();
        notes.insert(format!(
            "对方科目 {} 在「{}」方向上被 {} 行同时匹配（{}）—— 已取 sort_order 最小的 {}，\
             同一笔钱可能被重复计数，请检查 {ITEM_TABLE} 的 account_pattern",
            account_code,
            if direction == INFLOW { "流入" } else { "流出" },
            hits.len(),
            codes.join(" / "),
            first.item_code
        ));
    }
    Some(first)
}

/// 把凭证流拆成现金流并按对方科目分类。**纯函数**，无 IO。
///
/// 每张凭证的处理：
///
/// ```text
/// 现金分录     → 汇总成现金净流量（借−贷）
/// 非现金分录   → 按科目汇总「贷−借」，正数流入、负数流出
/// 差额         → 现金净流量 − Σ对方净额：非 0 只可能是凭证借贷不平（进「其他」）
/// ```
pub fn classify(items: &[CashFlowItemDef], vouchers: &[CashVoucher]) -> CashClassification {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.item_code.cmp(&b.item_code)));

    let mut flows: Vec<VoucherFlow> = Vec::new();
    let mut unbalanced: Vec<UnbalancedVoucherRes> = Vec::new();
    let mut notes: BTreeSet<String> = BTreeSet::new();

    for voucher in vouchers {
        let entry_debit: Decimal = voucher.entries.iter().map(|entry| entry.debit).sum();
        let entry_credit: Decimal = voucher.entries.iter().map(|entry| entry.credit).sum();

        let mut has_cash = false;
        let mut cash_net = Decimal::ZERO;
        let mut counterparts: BTreeMap<String, Counterpart> = BTreeMap::new();

        for entry in &voucher.entries {
            if is_cash_account(&entry.account_code) {
                has_cash = true;
                cash_net += entry.debit - entry.credit;
                continue;
            }
            let slot = counterparts
                .entry(entry.account_code.clone())
                .or_insert_with(|| Counterpart {
                    name: entry.account_name.clone(),
                    category: entry.account_category.clone(),
                    net: Decimal::ZERO,
                });
            slot.net += entry.credit - entry.debit;
        }
        if !has_cash {
            continue;
        }

        // 分录合计 ≠ 单头（或借贷不等）：账本身坏了。与「两侧是否相等」无关，必须点名。
        if entry_debit != voucher.header_debit || entry_credit != voucher.header_credit || entry_debit != entry_credit {
            unbalanced.push(UnbalancedVoucherRes {
                voucher_no: voucher.voucher_no.clone(),
                voucher_date: voucher.voucher_date,
                entry_debit,
                entry_credit,
                header_debit: voucher.header_debit,
                header_credit: voucher.header_credit,
            });
        }

        let mut allocated = Decimal::ZERO;
        for (code, slot) in &counterparts {
            if slot.net.is_zero() {
                // 对方科目在本凭证里借贷抵消（如「借 1122 100 / 贷 1122 100」），
                // 没有现金流含义；它对 Σ对方净额的贡献本来就是 0。
                continue;
            }
            allocated += slot.net;
            let direction = if slot.net > Decimal::ZERO { INFLOW } else { OUTFLOW };
            let amount = report::zero_norm(slot.net.abs());
            match pick_item(&sorted, code, &slot.category, direction, &mut notes) {
                Some(item) => flows.push(VoucherFlow {
                    voucher_no: voucher.voucher_no.clone(),
                    voucher_date: voucher.voucher_date,
                    activity: item.activity.clone(),
                    direction: direction.to_string(),
                    item_code: item.item_code.clone(),
                    item_name: item.item_name.clone(),
                    amount,
                    account_code: code.clone(),
                }),
                None => flows.push(VoucherFlow {
                    voucher_no: voucher.voucher_no.clone(),
                    voucher_date: voucher.voucher_date,
                    activity: OTHER_ACTIVITY.to_string(),
                    direction: direction.to_string(),
                    item_code: format!("OTHER_{code}"),
                    item_name: format!("未分类：{code} {}", slot.name),
                    amount,
                    account_code: code.clone(),
                }),
            }
        }

        let residual = report::zero_norm(cash_net - allocated);
        if !residual.is_zero() {
            flows.push(VoucherFlow {
                voucher_no: voucher.voucher_no.clone(),
                voucher_date: voucher.voucher_date,
                activity: OTHER_ACTIVITY.to_string(),
                direction: if residual > Decimal::ZERO {
                    INFLOW.to_string()
                } else {
                    OUTFLOW.to_string()
                },
                item_code: OTHER_UNBALANCED.to_string(),
                item_name: "未分类：凭证分录不平差额".to_string(),
                amount: report::zero_norm(residual.abs()),
                account_code: String::new(),
            });
        }
    }

    CashClassification {
        flows,
        unbalanced_vouchers: unbalanced,
        warnings: notes.into_iter().collect(),
    }
}

/// 计算结果（三大活动 + 其他 + 净增加额）。
#[derive(Debug, Clone, Default)]
pub struct ComputedCashFlow {
    /// 经营 / 投资 / 筹资 / 其他 四个分区（固定顺序，含全 0 的行）。
    pub sections: Vec<ActivitySectionRes>,
    /// 净增加额 = 四个分区净额之和。
    pub net_increase: AmountPair,
    /// 未分类明细（活动未知，已归「其他」）。
    pub unclassified: Vec<UnclassifiedRes>,
    /// 本期现金分录笔数。
    pub cash_entry_count: usize,
    /// 告警。
    pub warnings: Vec<String>,
    /// 分录不平的凭证。
    pub unbalanced_vouchers: Vec<UnbalancedVoucherRes>,
}

impl ComputedCashFlow {
    /// 某个分区。
    pub fn section(&self, activity: &str) -> Option<&ActivitySectionRes> {
        self.sections.iter().find(|row| row.activity == activity)
    }

    /// 某个分区净额。
    pub fn net_of(&self, activity: &str) -> AmountPair {
        self.section(activity).map(|row| row.net).unwrap_or_default()
    }

    /// 某明细行的金额（找不到按 0）。
    pub fn pair_of(&self, item_code: &str) -> AmountPair {
        self.sections
            .iter()
            .flat_map(|section| section.inflow.iter().chain(section.outflow.iter()))
            .find(|line| line.item_code == item_code)
            .map(|line| AmountPair {
                current: line.current,
                ytd: line.ytd,
            })
            .unwrap_or_default()
    }

    /// 某明细行的本期金额。
    pub fn current_of(&self, item_code: &str) -> Decimal {
        self.pair_of(item_code).current
    }

    /// 某明细行的本年累计金额。
    pub fn ytd_of(&self, item_code: &str) -> Decimal {
        self.pair_of(item_code).ytd
    }

    /// 未能分类的笔数。
    pub fn unclassified_count(&self) -> usize {
        self.unclassified.len()
    }

    /// 未能分类的流入 / 流出金额。
    pub fn unclassified_amount(&self) -> (Decimal, Decimal) {
        let mut inflow = Decimal::ZERO;
        let mut outflow = Decimal::ZERO;
        for row in &self.unclassified {
            if row.direction == INFLOW {
                inflow += row.current;
            } else {
                outflow += row.current;
            }
        }
        (report::zero_norm(inflow), report::zero_norm(outflow))
    }
}

/// 一个窗口的聚合值：`(item_code, direction) -> (金额, 命中科目)`。
///
/// 键里带方向是必须的：同一个对方科目既可能出现在流入行（借 1002 / 贷 2101 取得借款），
/// 也可能出现在流出行（借 2101 / 贷 1002 偿还借款），未分类的科目更是两个方向都会出现。
#[derive(Debug, Default)]
struct Window {
    current: HashMap<(String, String), (Decimal, BTreeSet<String>)>,
    ytd: HashMap<(String, String), (Decimal, BTreeSet<String>)>,
}

impl Window {
    fn add(&mut self, flow: &VoucherFlow, in_current: bool, in_ytd: bool) {
        let key = (flow.item_code.clone(), flow.direction.clone());
        if in_current {
            let slot = self.current.entry(key.clone()).or_default();
            slot.0 += flow.amount;
            if !flow.account_code.is_empty() {
                slot.1.insert(flow.account_code.clone());
            }
        }
        if in_ytd {
            let slot = self.ytd.entry(key).or_default();
            slot.0 += flow.amount;
            if !flow.account_code.is_empty() {
                slot.1.insert(flow.account_code.clone());
            }
        }
    }

    fn pair(&self, item_code: &str, direction: &str) -> AmountPair {
        let key = (item_code.to_string(), direction.to_string());
        AmountPair {
            current: report::zero_norm(self.current.get(&key).map(|slot| slot.0).unwrap_or_default()),
            ytd: report::zero_norm(self.ytd.get(&key).map(|slot| slot.0).unwrap_or_default()),
        }
    }

    fn accounts(&self, item_code: &str, direction: &str) -> Vec<String> {
        let key = (item_code.to_string(), direction.to_string());
        let mut codes: BTreeSet<String> = BTreeSet::new();
        if let Some(slot) = self.current.get(&key) {
            codes.extend(slot.1.iter().cloned());
        }
        if let Some(slot) = self.ytd.get(&key) {
            codes.extend(slot.1.iter().cloned());
        }
        codes.into_iter().collect()
    }
}

fn sum_pair(lines: &[CashLineRes]) -> AmountPair {
    let current: Decimal = lines.iter().map(|line| line.current).sum();
    let ytd: Decimal = lines.iter().map(|line| line.ytd).sum();
    AmountPair {
        current: report::zero_norm(current),
        ytd: report::zero_norm(ytd),
    }
}

fn make_section(
    activity: &str,
    activity_name: &str,
    inflow: Vec<CashLineRes>,
    outflow: Vec<CashLineRes>,
) -> ActivitySectionRes {
    let inflow_total = sum_pair(&inflow);
    let outflow_total = sum_pair(&outflow);
    ActivitySectionRes {
        activity: activity.to_string(),
        activity_name: activity_name.to_string(),
        inflow,
        inflow_total,
        outflow,
        outflow_total,
        net: AmountPair {
            current: report::zero_norm(inflow_total.current - outflow_total.current),
            ytd: report::zero_norm(inflow_total.ytd - outflow_total.ytd),
        },
    }
}

/// 把分类结果按两段窗口（本期 / 本年累计）聚合成一张现金流量表。**纯函数**。
///
/// # 参数
///
/// * `period_start` / `period_end`：本期窗口（`本期金额` 列）；
/// * `ytd_start`：本年累计窗口起点（1 月 1 日，`本年累计` 列）。
///
/// 两张表的行结构由 `items` 决定（**配置里没有的行不出现**，恒 0 的已配置行照样出现，
/// 这样「本期没发生」和「配置漏了」在报表上可分辨）；「其他」分区的行按实际发生额拼出来。
pub fn compute(
    items: &[CashFlowItemDef],
    classification: &CashClassification,
    period_start: NaiveDate,
    ytd_start: NaiveDate,
    period_end: NaiveDate,
) -> ComputedCashFlow {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.item_code.cmp(&b.item_code)));

    let mut window = Window::default();
    let mut other_names: BTreeMap<(String, String), String> = BTreeMap::new();
    for flow in &classification.flows {
        let in_current = flow.voucher_date >= period_start && flow.voucher_date <= period_end;
        let in_ytd = flow.voucher_date >= ytd_start && flow.voucher_date <= period_end;
        if !in_current && !in_ytd {
            continue;
        }
        window.add(flow, in_current, in_ytd);
        if flow.activity == OTHER_ACTIVITY {
            other_names
                .entry((flow.item_code.clone(), flow.direction.clone()))
                .or_insert_with(|| flow.item_name.clone());
        }
    }

    let mut sections: Vec<ActivitySectionRes> = Vec::new();
    for (activity, activity_name) in ACTIVITIES {
        let mut inflow: Vec<CashLineRes> = Vec::new();
        let mut outflow: Vec<CashLineRes> = Vec::new();
        for item in &sorted {
            if item.activity != activity {
                continue;
            }
            let pair = window.pair(&item.item_code, &item.direction);
            let line = CashLineRes {
                item_code: item.item_code.clone(),
                item_name: item.item_name.clone(),
                current: pair.current,
                ytd: pair.ytd,
                accounts: window.accounts(&item.item_code, &item.direction),
                detail: Some(item.account_pattern.clone()),
            };
            if item.direction == INFLOW {
                inflow.push(line);
            } else {
                outflow.push(line);
            }
        }
        sections.push(make_section(activity, activity_name, inflow, outflow));
    }

    // 「其他」：配置之外的行，按（行编码, 方向）排序（金额恒为正、方向由 flow 决定）
    let mut other_inflow: Vec<CashLineRes> = Vec::new();
    let mut other_outflow: Vec<CashLineRes> = Vec::new();
    let mut unclassified: Vec<UnclassifiedRes> = Vec::new();
    let mut keys: BTreeSet<(String, String)> = BTreeSet::new();
    keys.extend(window.current.keys().cloned());
    keys.extend(window.ytd.keys().cloned());
    for (code, direction) in keys {
        if sorted.iter().any(|item| item.item_code == code) {
            continue;
        }
        let Some(name) = other_names.get(&(code.clone(), direction.clone())) else {
            continue;
        };
        let pair = window.pair(&code, &direction);
        if pair.is_zero() {
            continue;
        }
        let line = CashLineRes {
            item_code: code.clone(),
            item_name: name.clone(),
            current: pair.current,
            ytd: pair.ytd,
            accounts: window.accounts(&code, &direction),
            detail: Some("未匹配 biz_cashflow_item 任何一行（对方科目分类缺失，已归「其他」）".to_string()),
        };
        if direction == INFLOW {
            other_inflow.push(line);
        } else {
            other_outflow.push(line);
        }
        unclassified.push(UnclassifiedRes {
            item_code: code.clone(),
            item_name: name.clone(),
            direction: direction.clone(),
            current: pair.current,
            ytd: pair.ytd,
        });
    }
    sections.push(make_section(
        OTHER_ACTIVITY,
        OTHER_ACTIVITY_NAME,
        other_inflow,
        other_outflow,
    ));

    let net_current: Decimal = sections.iter().map(|row| row.net.current).sum();
    let net_ytd: Decimal = sections.iter().map(|row| row.net.ytd).sum();
    let net_increase = AmountPair {
        current: report::zero_norm(net_current),
        ytd: report::zero_norm(net_ytd),
    };

    let mut warnings = classification.warnings.clone();
    let unclassified: Vec<UnclassifiedRes> = unclassified
        .into_iter()
        .filter(|row| !row.current.is_zero() || !row.ytd.is_zero())
        .collect();
    if !unclassified.is_empty() {
        let inflow: Decimal = unclassified
            .iter()
            .filter(|row| row.direction == INFLOW)
            .map(|row| row.current)
            .sum();
        let outflow: Decimal = unclassified
            .iter()
            .filter(|row| row.direction == OUTFLOW)
            .map(|row| row.current)
            .sum();
        let codes: Vec<&str> = unclassified
            .iter()
            .take(8)
            .map(|row| row.item_code.trim_start_matches("OTHER_"))
            .collect();
        warnings.push(format!(
            "{} 笔现金分录未能分类，已归入「其他」（本期流入 {inflow} / 流出 {outflow}；\
             对方科目：{}）—— 到 {ITEM_TABLE} 给这些科目补一行 account_pattern 即可归位",
            unclassified.len(),
            codes.join("、")
        ));
    }
    if !classification.unbalanced_vouchers.is_empty() {
        let nos: Vec<&str> = classification
            .unbalanced_vouchers
            .iter()
            .take(5)
            .map(|row| row.voucher_no.as_str())
            .collect();
        warnings.push(format!(
            "{} 张凭证的分录合计与单头不符：{}（差额已计入「其他」，但账本身要先修好）",
            classification.unbalanced_vouchers.len(),
            nos.join("、")
        ));
    }

    ComputedCashFlow {
        sections,
        net_increase,
        unclassified,
        cash_entry_count: 0,
        warnings,
        unbalanced_vouchers: classification.unbalanced_vouchers.clone(),
    }
}

/// 勾稽校验（纯逻辑：只吃计算结果与两侧余额）。
///
/// `closing_balance` = 期初 + 净增加（本表口径）；`balance_sheet_cash` = 资产负债表
/// 「货币资金」；`closing_by_ledger` = 总账里现金类科目的期末余额合计（第三方视角）。
pub fn build_check(
    computed: &ComputedCashFlow,
    closing_balance: Decimal,
    balance_sheet_cash: Decimal,
    closing_by_ledger: Decimal,
    cash_entry_count: usize,
) -> CashFlowCheckRes {
    let diff = report::zero_norm(closing_balance - balance_sheet_cash);
    let balanced = diff.is_zero();
    let (unclassified_inflow, unclassified_outflow) = computed.unclassified_amount();
    let mut hints: Vec<String> = Vec::new();

    if !balanced {
        if !computed.unclassified.is_empty() {
            let items: Vec<String> = computed
                .unclassified
                .iter()
                .take(8)
                .map(|row| format!("{}({})", row.item_code.trim_start_matches("OTHER_"), row.current))
                .collect();
            hints.push(format!(
                "有 {} 类现金收支的对方科目没有归入任何现金流量表行：{}（已归「其他」，\
                 仍计入净增加额）—— 到 {ITEM_TABLE} 补映射，报表口径才严谨",
                computed.unclassified.len(),
                items.join("、")
            ));
        }
        if !computed.unbalanced_vouchers.is_empty() {
            let nos: Vec<&str> = computed
                .unbalanced_vouchers
                .iter()
                .take(5)
                .map(|row| row.voucher_no.as_str())
                .collect();
            hints.push(format!(
                "分录合计与单头不符的凭证：{}（共 {} 张）—— 现金分录被删 / 被改会让\
                 「期末现金」与总账货币资金背离",
                nos.join("、"),
                computed.unbalanced_vouchers.len()
            ));
        }
        hints.push(
            "现金类科目口径差异：本表按 `1001` / `1002` 前缀取现金类科目，资产负债表\
             「货币资金」还含 `1012 其他货币资金`。若 1012 本期有进出，勾稽会差这一块 —— \
             要么改现金类口径，要么把这笔钱记到 1002"
                .to_string(),
        );
        if !closing_by_ledger.is_zero() && closing_balance != closing_by_ledger {
            hints.push(format!(
                "本表期末现金 {closing_balance} ≠ 总账现金类科目余额 {closing_by_ledger}：\
                 说明有现金分录落在统计窗口之外（凭证日期被改过），逐张核对现金类凭证"
            ));
        }
    }

    let message = if balanced {
        format!("勾稽一致：期末现金及现金等价物余额 {closing_balance} = 资产负债表「货币资金」{balance_sheet_cash}")
    } else {
        format!(
            "不平：期末现金及现金等价物余额 {closing_balance} ≠ 资产负债表「货币资金」{balance_sheet_cash}，\
             差 {} 元（差额不做舍入）",
            report::diff_text(diff)
        )
    };

    CashFlowCheckRes {
        balanced,
        closing_balance,
        balance_sheet_cash,
        diff,
        diff_text: report::diff_text(diff),
        message,
        closing_by_ledger,
        cash_entry_count,
        unclassified_count: computed.unclassified.len(),
        unclassified_inflow,
        unclassified_outflow,
        unbalanced_vouchers: computed.unbalanced_vouchers.clone(),
        hints,
    }
}

// ==================== 数据访问 ====================

/// 期间 → 本年累计起算日（该年 1 月 1 日）。
pub fn ytd_start_of(period: &str) -> Option<NaiveDate> {
    let (year, _month) = report::parse_period(period)?;
    NaiveDate::from_ymd_opt(year, 1, 1)
}

fn ytd_start_for(period: &str, period_end: NaiveDate) -> NaiveDate {
    ytd_start_of(period).unwrap_or_else(|| NaiveDate::from_ymd_opt(period_end.year(), 1, 1).unwrap_or(period_end))
}

/// 分类配置（`biz_cashflow_item`，启用行，按 `sort_order`）。
pub async fn load_items(tx: &Transaction) -> DbResult<Vec<CashFlowItemDef>> {
    let sql = format!(
        "SELECT item_code, item_name, activity, direction, account_pattern, sort_order \
           FROM {ITEM_TABLE} WHERE is_active = 1 ORDER BY sort_order, item_code"
    );
    let rows = tx.find_with_params(&sql, vec![]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(CashFlowItemDef {
            item_code: row.get_typed("item_code")?,
            item_name: row.get_typed("item_name")?,
            activity: row.get_typed("activity")?,
            direction: row.get_typed("direction")?,
            account_pattern: row.get_typed("account_pattern")?,
            sort_order: row.get_typed("sort_order")?,
        });
    }
    Ok(out)
}

/// 窗口内**含现金分录**的凭证与它们的全部分录（按日期 → 凭证号 → 行号排序）。
///
/// 只取「有现金分录」的凭证：没有现金分录的凭证不产生现金流，读进来只会浪费内存。
///
/// # 期初 / 年结凭证不算「本期发生的现金流」（E2E-04 修）
///
/// `OPENING` 是**建账余额**、`YEAR_END` 是**账内搬运**，两者都不是「这一年收了/付了钱」。
/// 不排除它们时，年初录的期初凭证会被当成一笔「未分类的经营活动流入」，
/// 于是「期初现金」= 0、净增加额凭空多出期初金额、`unclassified_count` 永远 ≥ 1，
/// 而「期末现金」又因 `ledger_cash_balance`（走 `report::load_accounts`，期初单列）
/// 是对的 —— 一增一减，勾稽差额恰好抵消成 0，把口径错误藏了起来（实测 2022 年
/// 净增加额 1 616 000 而不是 1 116 000）。期初现金只由 `opening_cash_balance()` 提供。
pub async fn load_cash_vouchers(tx: &Transaction, start: NaiveDate, end: NaiveDate) -> DbResult<Vec<CashVoucher>> {
    let sql = format!(
        "SELECT v.voucher_no, v.voucher_date, v.total_debit, v.total_credit, \
                e.line_no, e.account_code, e.account_name, e.debit_amount, e.credit_amount, \
                COALESCE(a.account_category, '') AS account_category \
           FROM {voucher} v \
           JOIN {entry} e ON e.voucher_id = v.id \
           LEFT JOIN {account} a ON a.id = e.account_id \
          WHERE v.status = '{posted}' AND v.deleted_at IS NULL \
            AND v.voucher_type NOT IN ('OPENING', 'YEAR_END') \
            AND v.voucher_date >= ? AND v.voucher_date <= ? \
            AND v.id IN ( \
                SELECT v2.id FROM {voucher} v2 \
                  JOIN {entry} e2 ON e2.voucher_id = v2.id \
                 WHERE v2.status = '{posted}' AND v2.deleted_at IS NULL \
                   AND v2.voucher_date >= ? AND v2.voucher_date <= ? \
                   AND LEFT(e2.account_code, 4) IN ('1001', '1002') \
            ) \
          ORDER BY v.voucher_date, v.voucher_no, e.line_no",
        voucher = report::VOUCHER_TABLE,
        entry = report::ENTRY_TABLE,
        account = report::ACCOUNT_TABLE,
        posted = report::POSTED,
    );
    let rows = tx
        .find_with_params(
            &sql,
            vec![
                Value::from(start),
                Value::from(end),
                Value::from(start),
                Value::from(end),
            ],
        )
        .await?;

    let mut out: Vec<CashVoucher> = Vec::new();
    for row in &rows {
        let voucher_no: String = row.get_typed("voucher_no")?;
        let voucher_date: NaiveDate = row.get_typed("voucher_date")?;
        let entry = CashEntry {
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_category: row.get_typed("account_category")?,
            debit: row.get_typed("debit_amount")?,
            credit: row.get_typed("credit_amount")?,
        };
        match out.last_mut() {
            Some(last) if last.voucher_no == voucher_no && last.voucher_date == voucher_date => {
                last.entries.push(entry);
            }
            _ => out.push(CashVoucher {
                voucher_no,
                voucher_date,
                header_debit: row.get_typed("total_debit")?,
                header_credit: row.get_typed("total_credit")?,
                entries: vec![entry],
            }),
        }
    }
    Ok(out)
}

/// 「货币资金」的匹配模式（取资产负债表的配置；取不到时回退常量）。
pub async fn load_monetary_pattern(tx: &Transaction) -> DbResult<String> {
    let sql = format!(
        "SELECT account_pattern FROM {} \
          WHERE report_code = ? AND item_code = ? AND is_active = 1 LIMIT 1",
        report::ITEM_TABLE
    );
    let row = tx
        .find_with_params(&sql, vec![Value::from(BALANCE_REPORT_CODE), Value::from(MONETARY_ITEM)])
        .await?
        .into_iter()
        .next();
    let pattern = row
        .and_then(|row| row.get_typed::<String>("account_pattern").ok())
        .filter(|text| !text.trim().is_empty());
    Ok(pattern.unwrap_or_else(|| MONETARY_PATTERN_FALLBACK.to_string()))
}

/// 现金类科目在「`start` 之前」的余额合计（借 − 贷）。
pub fn opening_cash_balance(accounts: &[AccountBalance]) -> Decimal {
    report::zero_norm(
        accounts
            .iter()
            .filter(|row| is_cash_account(&row.account_code))
            .map(|row| row.net_before)
            .sum(),
    )
}

/// 现金类科目在「`end`」的余额合计（借 − 贷）。
pub fn ledger_cash_balance(accounts: &[AccountBalance]) -> Decimal {
    report::zero_norm(
        accounts
            .iter()
            .filter(|row| is_cash_account(&row.account_code))
            .map(|row| row.net())
            .sum(),
    )
}

/// 按匹配模式算「货币资金」（兜底用：资产负债表配置缺失时）。
pub fn cash_by_pattern(accounts: &[AccountBalance], pattern: &str) -> Decimal {
    report::zero_norm(
        accounts
            .iter()
            .filter(|row| report::match_account(pattern, &row.account_code, &row.account_category))
            .map(|row| row.display())
            .sum(),
    )
}

/// 期末现金余额与资产负债表「货币资金」，两处口径必须一致（勾稽右式的来源）。
///
/// 优先复用 R1 的 `report::compute` 对 `BALANCE_SHEET / MONETARY_FUNDS` 出数 —— 两张报表
/// 只有一份「货币资金」口径；资产负债表配置缺失时退化为按 pattern 直算，并给一条告警。
fn monetary_funds(
    bs_items: &[report::ReportItemDef],
    accounts: &[AccountBalance],
    pattern: &str,
) -> (Decimal, Option<String>) {
    if !bs_items.is_empty() {
        match report::compute(bs_items, accounts) {
            Ok(sheet) => {
                if let Some(amount) = sheet.amounts.get(MONETARY_ITEM).copied() {
                    return (report::zero_norm(amount), None);
                }
            }
            Err(error) => {
                return (
                    cash_by_pattern(accounts, pattern),
                    Some(format!(
                        "资产负债表配置报错（{error}），勾稽右式退化为按货币资金 pattern「{pattern}」直算"
                    )),
                );
            }
        }
    }
    (
        cash_by_pattern(accounts, pattern),
        Some(format!(
            "资产负债表没有货币资金报表项，勾稽右式退化为按 pattern「{pattern}」直算"
        )),
    )
}

/// 一次报表计算的完整产物（接口层用它拼返回体）。
#[derive(Debug)]
pub struct CashFlowSnapshot {
    /// 计算结果。
    pub computed: ComputedCashFlow,
    /// 期间。
    pub period: PeriodInfo,
    /// 本年累计起算日。
    pub ytd_start: NaiveDate,
    /// 期初现金（本期列）。
    pub opening_current: Decimal,
    /// 期初现金（本年累计列）。
    pub opening_ytd: Decimal,
    /// 期末现金（本表口径：期初 + 净增加）。
    pub closing_current: Decimal,
    /// 期末现金（总账现金类科目余额）。
    pub closing_by_ledger: Decimal,
    /// 资产负债表「货币资金」。
    pub balance_sheet_cash: Decimal,
    /// 「货币资金」匹配模式。
    pub monetary_pattern: String,
    /// 本期现金分录笔数。
    pub cash_entry_count: usize,
    /// 期间口径 / 数据一致性告警（计入 `warnings`）。
    pub warnings: Vec<String>,
}

impl CashFlowSnapshot {
    /// 勾稽结果。
    pub fn check(&self) -> CashFlowCheckRes {
        build_check(
            &self.computed,
            self.closing_current,
            self.balance_sheet_cash,
            self.closing_by_ledger,
            self.cash_entry_count,
        )
    }
}

/// 在给定事务（**必须是 REPEATABLE READ 快照**）里算出一整张现金流量表。
///
/// # 异常情况
///
/// * 期间不存在 → `Err(DbError::InvalidValue)`，接口层翻成 400 + 66010；
/// * 分类配置为空 → `Err(DbError::InvalidValue)`，接口层翻成 500 + 66011。
pub async fn build_snapshot(tx: &Transaction, period: &str) -> DbResult<CashFlowSnapshot> {
    let info = report::resolve_period(tx, period).await?;
    let items = load_items(tx).await?;
    if items.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "现金流量表分类配置为空（{ITEM_TABLE} 一行都没有），无法编制报表"
        )));
    }

    let ytd_start = ytd_start_for(period, info.end_date);
    // 一张宽窗口查进来，本期 / 累计在内存里切（同一快照、同一份数据）
    let vouchers = load_cash_vouchers(tx, ytd_start, info.end_date).await?;
    let classification = classify(&items, &vouchers);
    let mut computed = compute(&items, &classification, info.start_date, ytd_start, info.end_date);

    let cash_entry_count: usize = vouchers
        .iter()
        .flat_map(|voucher| voucher.entries.iter())
        .filter(|entry| is_cash_account(&entry.account_code))
        .count();
    computed.cash_entry_count = cash_entry_count;

    // 期初：期间开始日之前 / 本年 1 月 1 日之前
    // ERP-R5：期初凭证算「期初」而不是「本期发生」，两个窗口都按报表所属期间识别它
    let period_accounts = report::load_accounts(tx, info.start_date, info.end_date, Some(&info.period_code)).await?;
    let ytd_accounts = report::load_accounts(tx, ytd_start, info.end_date, Some(&info.period_code)).await?;
    let opening_current = opening_cash_balance(&period_accounts);
    let opening_ytd = opening_cash_balance(&ytd_accounts);
    let closing_by_ledger = ledger_cash_balance(&period_accounts);

    // 勾稽右式：资产负债表「货币资金」
    let monetary_pattern = load_monetary_pattern(tx).await?;
    let bs_items = report::load_items_of(tx, BALANCE_REPORT_CODE).await?;
    let (balance_sheet_cash, monetary_warning) = monetary_funds(&bs_items, &period_accounts, &monetary_pattern);

    let closing_current = report::zero_norm(opening_current + computed.net_increase.current);

    let mut warnings: Vec<String> = Vec::new();
    if let Some(note) = monetary_warning {
        warnings.push(note);
    }
    // 现金类口径提示：资产负债表「货币资金」还含 1012，而本表现金类只认 1001/1002
    let outside: Vec<String> = period_accounts
        .iter()
        .filter(|row| !is_cash_account(&row.account_code))
        .filter(|row| report::match_account(&monetary_pattern, &row.account_code, &row.account_category))
        .filter(|row| !row.net().is_zero())
        .map(|row| format!("{}({})", row.account_code, row.net()))
        .collect();
    if !outside.is_empty() {
        warnings.push(format!(
            "资产负债表「货币资金」口径含本表未当作现金类的科目：{} —— 这些科目的收支会归「其他」，\
             勾稽会差这一块（本表现金类只认 1001 / 1002 前缀，卡面 §一）",
            outside.join("、")
        ));
    }

    Ok(CashFlowSnapshot {
        computed,
        period: info,
        ytd_start,
        opening_current,
        opening_ytd,
        closing_current,
        closing_by_ledger,
        balance_sheet_cash,
        monetary_pattern,
        cash_entry_count,
        warnings,
    })
}

/// 组装接口返回体（纯逻辑：IO 结果进来，JSON 出去）。
pub fn into_response(company_name: &str, unit: &str, snapshot: CashFlowSnapshot) -> CashFlowRes {
    let CashFlowSnapshot {
        computed,
        period,
        ytd_start,
        opening_current,
        opening_ytd,
        closing_current,
        closing_by_ledger,
        balance_sheet_cash,
        monetary_pattern: _,
        cash_entry_count,
        warnings,
    } = snapshot;

    let mut check = build_check(
        &computed,
        closing_current,
        balance_sheet_cash,
        closing_by_ledger,
        cash_entry_count,
    );
    let mut warnings = warnings;
    // 分类过程 / 未分类笔数 / 分录不平的告警必须一起出现在出参里（硬红线 4：不许静默丢）
    warnings.extend(computed.warnings.iter().cloned());
    // 期间是按自然月推算出来的（库里没有期间行，但该月有账）：必须让看报表的人知道
    if period.status == PERIOD_DERIVED {
        let hint = format!(
            "会计期间 {code} 未在「会计期间」里建立，本表按自然月（{start} ~ {end}）推算编制；\
             请尽快开账，否则月结 / 期间锁定 / 审计口径都对不上",
            code = period.period_code,
            start = period.start_date,
            end = period.end_date
        );
        check.hints.push(hint.clone());
        warnings.push(hint);
    }

    let empty_section = |activity: &str, name: &str| ActivitySectionRes {
        activity: activity.to_string(),
        activity_name: name.to_string(),
        inflow: Vec::new(),
        inflow_total: AmountPair::default(),
        outflow: Vec::new(),
        outflow_total: AmountPair::default(),
        net: AmountPair::default(),
    };
    let take = |activity: &str, name: &str| {
        computed
            .sections
            .iter()
            .find(|row| row.activity == activity)
            .cloned()
            .unwrap_or_else(|| empty_section(activity, name))
    };
    let operating = take(ACTIVITIES[0].0, ACTIVITIES[0].1);
    let investing = take(ACTIVITIES[1].0, ACTIVITIES[1].1);
    let financing = take(ACTIVITIES[2].0, ACTIVITIES[2].1);
    let other = take(OTHER_ACTIVITY, OTHER_ACTIVITY_NAME);

    CashFlowRes {
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
        simplified: true,
        simplified_note: SIMPLIFIED_NOTE.to_string(),
        operating,
        investing,
        financing,
        other,
        net_increase: computed.net_increase,
        opening_balance: AmountPair {
            current: opening_current,
            ytd: opening_ytd,
        },
        closing_balance: AmountPair {
            current: closing_current,
            ytd: closing_current,
        },
        check,
        warnings,
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
/// `pub(crate)`：PDF-07（[`crate::business::erp::report_pdf`]）复用同一快照与错误码。
pub(crate) async fn snapshot_or_error(period: &str) -> std::result::Result<CashFlowSnapshot, ApiError> {
    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启现金流量表快照事务"))?;
    // ★ L11：整张报表在一个 REPEATABLE READ 快照里算完。
    //   `SET TRANSACTION` 必须是 BEGIN 之后的第一条语句（PG 规定，否则 25001）。
    if let Err(error) = tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "设置现金流量表事务隔离级别"));
    }
    match build_snapshot(&tx, period).await {
        Ok(snapshot) => {
            if let Err(error) = tx.commit().await {
                return Err(utils::db_error(error, "提交现金流量表快照事务"));
            }
            Ok(snapshot)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            match error {
                DbError::InvalidValue(message) => Err(period_error(period, message)),
                other => Err(utils::db_error(other, "编制现金流量表")),
            }
        }
    }
}

/// 编制单位（取系统配置里的系统名，取不到就用产品名）。
/// `pub(crate)`：PDF-07 复用同一份编制单位取数。
pub(crate) async fn company_name() -> String {
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

/// 现金流量表。
#[nik::permission(
    path = "/business/erp/report/cash-flow",
    method = "GET",
    permission = "business:erp:report:cash-flow"
)]
#[nik::get("/business/erp/report/cash-flow")]
pub async fn cash_flow_handler(Query(query): Query<CashFlowQuery>) -> ResResult<CashFlowRes> {
    let period = validate_period(query.period.as_deref())?;
    let snapshot = snapshot_or_error(&period).await?;
    let company = company_name().await;
    Ok(Res::ok(into_response(&company, "元", snapshot)))
}

/// 现金流量表导出（xlsx，复用 impexp 的表格写出能力，与资产负债表 / 利润表同一套写法）。
#[nik::permission(
    path = "/business/erp/report/cash-flow/export",
    method = "GET",
    permission = "business:erp:report:export"
)]
#[nik::get("/business/erp/report/cash-flow/export")]
pub async fn cash_flow_export_handler(Query(query): Query<CashFlowQuery>) -> std::result::Result<Response, ApiError> {
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

/// 把报表拍成「项目 / 本期金额 / 本年累计」三列的表格行（与利润表同构）。
pub fn export_table(report: &CashFlowRes) -> (Vec<&'static str>, Vec<HashMap<String, String>>) {
    const HEADERS: [&str; 3] = ["项目", "本期金额", "本年累计"];
    let mut rows: Vec<HashMap<String, String>> = Vec::new();

    let mut info = HashMap::new();
    info.insert("项目".to_string(), format!("编制单位：{}", report.company_name));
    info.insert(
        "本期金额".to_string(),
        format!("期间：{}（{}）", report.period_name, report.period_status),
    );
    info.insert(
        "本年累计".to_string(),
        format!(
            "单位：{}；本年累计从 {} 起算；{}",
            report.unit, report.ytd_start, report.simplified_note
        ),
    );
    rows.push(info);

    for section in [&report.operating, &report.investing, &report.financing, &report.other] {
        let mut title = HashMap::new();
        title.insert("项目".to_string(), section.activity_name.clone());
        title.insert("本期金额".to_string(), report::money_text(Some(section.net.current)));
        title.insert("本年累计".to_string(), report::money_text(Some(section.net.ytd)));
        rows.push(title);

        for (label, lines, total) in [
            ("流入", &section.inflow, section.inflow_total),
            ("流出", &section.outflow, section.outflow_total),
        ] {
            for line in lines {
                let mut row = HashMap::new();
                row.insert("项目".to_string(), format!("    {}", line.item_name));
                row.insert("本期金额".to_string(), report::money_text(Some(line.current)));
                row.insert("本年累计".to_string(), report::money_text(Some(line.ytd)));
                rows.push(row);
            }
            let mut subtotal = HashMap::new();
            subtotal.insert("项目".to_string(), format!("  {label}小计"));
            subtotal.insert("本期金额".to_string(), report::money_text(Some(total.current)));
            subtotal.insert("本年累计".to_string(), report::money_text(Some(total.ytd)));
            rows.push(subtotal);
        }
        let mut net = HashMap::new();
        net.insert(
            "项目".to_string(),
            format!("  {}净额", activity_label(&section.activity)),
        );
        net.insert("本期金额".to_string(), report::money_text(Some(section.net.current)));
        net.insert("本年累计".to_string(), report::money_text(Some(section.net.ytd)));
        rows.push(net);
    }

    for (label, pair) in [
        ("现金及现金等价物净增加额", report.net_increase),
        ("加：期初现金及现金等价物余额", report.opening_balance),
        ("期末现金及现金等价物余额", report.closing_balance),
    ] {
        let mut row = HashMap::new();
        row.insert("项目".to_string(), label.to_string());
        row.insert("本期金额".to_string(), report::money_text(Some(pair.current)));
        row.insert("本年累计".to_string(), report::money_text(Some(pair.ytd)));
        rows.push(row);
    }

    let mut tail = HashMap::new();
    tail.insert("项目".to_string(), report.check.message.clone());
    tail.insert(
        "本期金额".to_string(),
        format!(
            "资产负债表「货币资金」{}",
            report::money_text(Some(report.check.balance_sheet_cash))
        ),
    );
    tail.insert(
        "本年累计".to_string(),
        format!(
            "差额 {}；未分类 {} 笔",
            report.check.diff_text, report.check.unclassified_count
        ),
    );
    rows.push(tail);

    for warning in &report.warnings {
        let mut row = HashMap::new();
        row.insert("项目".to_string(), format!("⚠ {warning}"));
        rows.push(row);
    }

    (HEADERS.to_vec(), rows)
}

/// 活动编码 → 中文（导出 / 文案用）。
pub fn activity_label(activity: &str) -> &'static str {
    match activity {
        "OPERATING" => "经营活动产生的现金流量",
        "INVESTING" => "投资活动产生的现金流量",
        "FINANCING" => "筹资活动产生的现金流量",
        _ => "未分类现金收支",
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher_query::tests::{DB_LOCK, ensure_db};
    use std::str::FromStr;

    const FIXTURE_WORD: &str = "ERPR3";

    fn day(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).expect("日期")
    }

    /// 造一条分类行（与 V0185 种子逐字一致）。
    fn item(code: &str, name: &str, activity: &str, direction: &str, pattern: &str, order: i32) -> CashFlowItemDef {
        CashFlowItemDef {
            item_code: code.to_string(),
            item_name: name.to_string(),
            activity: activity.to_string(),
            direction: direction.to_string(),
            account_pattern: pattern.to_string(),
            sort_order: order,
        }
    }

    /// 与 V0185 种子逐字一致的 12 行（单测不依赖库，配置改了这里要跟着改）。
    fn items() -> Vec<CashFlowItemDef> {
        vec![
            item(
                "OPER_SALES_RECEIPTS",
                "销售商品、提供劳务收到的现金",
                "OPERATING",
                "INFLOW",
                "1122,1121,2203,5001,5051",
                101,
            ),
            item(
                "OPER_OTHER_INFLOW",
                "收到的其他与经营活动有关的现金",
                "OPERATING",
                "INFLOW",
                "1221,2241,2401,5301,1901",
                102,
            ),
            item(
                "OPER_PURCHASE_PAYMENTS",
                "购买商品、接受劳务支付的现金",
                "OPERATING",
                "OUTFLOW",
                "2202,1123,1401,1402,1403,1404,1405,1408,4001,4101,5401,5402,6401",
                201,
            ),
            item(
                "OPER_STAFF_PAYMENTS",
                "支付给职工以及为职工支付的现金",
                "OPERATING",
                "OUTFLOW",
                "2211",
                202,
            ),
            item(
                "OPER_TAX_PAYMENTS",
                "支付的各项税费",
                "OPERATING",
                "OUTFLOW",
                "2221,5801,5403",
                203,
            ),
            item(
                "OPER_OTHER_OUTFLOW",
                "支付的其他与经营活动有关的现金",
                "OPERATING",
                "OUTFLOW",
                "5601,5602,5603,5711,1221,2241,1901",
                204,
            ),
            item(
                "INVEST_RECOVERY",
                "收回投资、取得投资收益和处置长期资产收到的现金",
                "INVESTING",
                "INFLOW",
                "1601,1602,1604,1605,1606,1701,1702,1801,5111,1131,1132",
                301,
            ),
            item(
                "INVEST_PURCHASES",
                "购建固定资产、无形资产和其他长期资产支付的现金",
                "INVESTING",
                "OUTFLOW",
                "1601,1602,1604,1605,1606,1701,1702,1801",
                351,
            ),
            item(
                "FIN_CAPITAL_IN",
                "吸收投资收到的现金",
                "FINANCING",
                "INFLOW",
                "3001,3101,3102",
                401,
            ),
            item(
                "FIN_LOANS_IN",
                "取得借款收到的现金",
                "FINANCING",
                "INFLOW",
                "2101,2501,2701",
                402,
            ),
            item(
                "FIN_DEBT_REPAY",
                "偿还债务支付的现金",
                "FINANCING",
                "OUTFLOW",
                "2101,2501,2701",
                451,
            ),
            item(
                "FIN_DISTRIBUTION",
                "分配股利、利润或偿付利息支付的现金",
                "FINANCING",
                "OUTFLOW",
                "3104,3103,2231,3001",
                452,
            ),
        ]
    }

    fn entry(code: &str, debit: i64, credit: i64) -> CashEntry {
        CashEntry {
            account_code: code.to_string(),
            account_name: format!("科目{code}"),
            account_category: category_of(code).to_string(),
            debit: Decimal::from(debit),
            credit: Decimal::from(credit),
        }
    }

    fn entry_dec(code: &str, debit: &str, credit: &str) -> CashEntry {
        CashEntry {
            account_code: code.to_string(),
            account_name: format!("科目{code}"),
            account_category: category_of(code).to_string(),
            debit: Decimal::from_str(debit).expect("十进制"),
            credit: Decimal::from_str(credit).expect("十进制"),
        }
    }

    /// 与 V0183 科目表一致的类别（只列单测用到的）。
    fn category_of(code: &str) -> &'static str {
        match code {
            "1001" | "1002" | "1012" | "1122" | "1601" | "1405" | "1901" => "ASSET",
            "2211" | "2221" | "2202" | "2501" => "LIABILITY",
            "3001" | "3103" | "3104" => "EQUITY",
            "5001" | "5051" | "5111" | "5401" | "5602" | "5801" => "PROFIT_LOSS",
            "4001" => "COST",
            _ => "ASSET",
        }
    }

    /// 造一张凭证（单头合计 = 分录合计）。
    fn voucher(no: &str, date: NaiveDate, entries: Vec<CashEntry>) -> CashVoucher {
        let debit: Decimal = entries.iter().map(|entry| entry.debit).sum();
        let credit: Decimal = entries.iter().map(|entry| entry.credit).sum();
        CashVoucher {
            voucher_no: no.to_string(),
            voucher_date: date,
            header_debit: debit,
            header_credit: credit,
            entries,
        }
    }

    /// 一笔简单业务：借 `debit` / 贷 `credit`。
    fn two_line(no: &str, date: NaiveDate, debit: &str, credit: &str, amount: i64) -> CashVoucher {
        voucher(no, date, vec![entry(debit, amount, 0), entry(credit, 0, amount)])
    }

    fn run(vouchers: &[CashVoucher]) -> ComputedCashFlow {
        let classification = classify(&items(), vouchers);
        compute(
            &items(),
            &classification,
            day(10, 1),
            NaiveDate::from_ymd_opt(2026, 1, 1).expect("日期"),
            day(10, 31),
        )
    }

    // ---------- ① 只有一笔现金收入 ----------

    /// ① 借 1002 / 贷 5001 → 经营流入（销售商品收到的现金）。
    #[test]
    fn r3_single_cash_inflow_is_operating() {
        let flows = run(&[two_line("0101", day(10, 5), "1002", "5001", 1_000)]);
        assert_eq!(flows.current_of("OPER_SALES_RECEIPTS"), Decimal::from(1_000));
        assert_eq!(flows.ytd_of("OPER_SALES_RECEIPTS"), Decimal::from(1_000));
        assert_eq!(
            flows.net_of("OPERATING"),
            AmountPair {
                current: Decimal::from(1_000),
                ytd: Decimal::from(1_000)
            }
        );
        assert!(flows.unclassified.is_empty(), "5001 已映射，不该进「其他」");
        println!("[R3-①] 借 1002 / 贷 5001 1000 → 经营流入 1000 ✓");
    }

    // ---------- ② 只有一笔现金支出 ----------

    /// ② 借 5401 / 贷 1002 → 经营流出（购买商品支付的现金）。
    #[test]
    fn r3_single_cash_outflow_is_operating() {
        let flows = run(&[two_line("0102", day(10, 6), "5401", "1002", 600)]);
        assert_eq!(flows.current_of("OPER_PURCHASE_PAYMENTS"), Decimal::from(600));
        let operating = flows.section("OPERATING").expect("经营分区");
        assert_eq!(operating.outflow_total.current, Decimal::from(600));
        assert_eq!(operating.net.current, Decimal::from(-600));
        println!("[R3-②] 借 5401 / 贷 1002 600 → 经营流出 600 ✓");
    }

    // ---------- ③ 经营净额 = 流入 − 流出 ----------

    /// ③ 收 1000、付 600 → 经营净额 400（= 流入小计 − 流出小计）。
    #[test]
    fn r3_operating_net_is_inflow_minus_outflow() {
        let flows = run(&[
            two_line("0103", day(10, 5), "1002", "5001", 1_000),
            two_line("0104", day(10, 6), "5401", "1002", 600),
            two_line("0105", day(10, 7), "2211", "1002", 150),
        ]);
        let operating = flows.section("OPERATING").expect("经营分区");
        assert_eq!(operating.inflow_total.current, Decimal::from(1_000));
        assert_eq!(
            operating.outflow_total.current,
            Decimal::from(750),
            "600 采购 + 150 工资"
        );
        assert_eq!(operating.net.current, Decimal::from(250));
        assert_eq!(
            operating.net.current,
            operating.inflow_total.current - operating.outflow_total.current
        );
        println!("[R3-③] 经营净额 = 1000 − 750 = 250 ✓");
    }

    // ---------- ④ 买固定资产 ----------

    /// ④ 借 1601 / 贷 1002 → 投资流出（不是经营支出）。
    #[test]
    fn r3_fixed_asset_purchase_is_investing_outflow() {
        let flows = run(&[two_line("0106", day(10, 8), "1601", "1002", 5_000)]);
        assert_eq!(flows.current_of("INVEST_PURCHASES"), Decimal::from(5_000));
        let investing = flows.section("INVESTING").expect("投资分区");
        assert_eq!(investing.net.current, Decimal::from(-5_000));
        assert_eq!(
            flows.section("OPERATING").expect("经营分区").net.current,
            Decimal::ZERO,
            "买设备不许混进经营活动"
        );
        println!("[R3-④] 借 1601 / 贷 1002 5000 → 投资流出 5000 ✓");
    }

    // ---------- ⑤ 借款 ----------

    /// ⑤ 借 1002 / 贷 2501 → 筹资流入。
    #[test]
    fn r3_loan_received_is_financing_inflow() {
        let flows = run(&[two_line("0107", day(10, 9), "1002", "2501", 3_000)]);
        assert_eq!(flows.current_of("FIN_LOANS_IN"), Decimal::from(3_000));
        let financing = flows.section("FINANCING").expect("筹资分区");
        assert_eq!(financing.net.current, Decimal::from(3_000));
        // 偿还借款走同一组科目的反方向
        let repay = run(&[two_line("0108", day(10, 10), "2101", "1002", 1_000)]);
        assert_eq!(repay.current_of("FIN_DEBT_REPAY"), Decimal::from(1_000));
        println!("[R3-⑤] 借 1002 / 贷 2501 → 筹资流入；反向 → 筹资流出 ✓");
    }

    // ---------- ⑥ 交税 ----------

    /// ⑥ 借 2221 / 贷 1002 → 经营流出「支付的各项税费」。
    #[test]
    fn r3_tax_payment_is_operating_outflow() {
        let flows = run(&[two_line("0109", day(10, 11), "2221", "1002", 82)]);
        assert_eq!(flows.current_of("OPER_TAX_PAYMENTS"), Decimal::from(82));
        assert_eq!(
            flows.section("OPERATING").expect("经营分区").outflow_total.current,
            Decimal::from(82)
        );
        println!("[R3-⑥] 借 2221 / 贷 1002 82 → 经营流出·税费 82 ✓");
    }

    // ---------- ⑦ 期末 = 期初 + 净增加 ----------

    /// ⑦ 期末余额 = 期初 + 净增加（本表口径的加减恒等式）。
    #[test]
    fn r3_closing_equals_opening_plus_net_increase() {
        let flows = run(&[
            two_line("0110", day(10, 5), "1002", "5001", 1_000),
            two_line("0111", day(10, 6), "5401", "1002", 250),
            two_line("0112", day(10, 7), "1601", "1002", 300),
            two_line("0113", day(10, 8), "1002", "2501", 500),
        ]);
        let opening = Decimal::from(2_000);
        assert_eq!(
            flows.net_increase.current,
            Decimal::from(1_000) - Decimal::from(250) - Decimal::from(300) + Decimal::from(500)
        );
        let closing = opening + flows.net_increase.current;
        assert_eq!(closing, Decimal::from(2_950));
        // 清单式核对：四个分区的净额之和 = 净增加额
        let sum: Decimal = flows.sections.iter().map(|row| row.net.current).sum();
        assert_eq!(sum, flows.net_increase.current);
        println!("[R3-⑦] 期初 2000 + 净增加 950 = 期末 2950 ✓");
    }

    // ---------- ⑧ 期末 = 资产负债表货币资金 ----------

    /// ⑧ 期末余额与资产负债表「货币资金」一致 → `balanced`；差一分也必须红。
    #[test]
    fn r3_check_ties_closing_to_balance_sheet_cash() {
        let flows = run(&[two_line("0114", day(10, 5), "1002", "5001", 1_000)]);
        let closing = Decimal::from(1_000);
        let check = build_check(&flows, closing, closing, closing, 2);
        assert!(check.balanced, "必须平：{}", check.message);
        assert_eq!(check.diff, Decimal::ZERO);
        assert_eq!(check.diff_text, "0.00");
        assert!(check.message.contains("勾稽一致"), "{}", check.message);
        assert!(check.hints.is_empty(), "平的时候不吐提示：{:?}", check.hints);

        let check = build_check(&flows, closing, Decimal::from(1_250), closing, 2);
        assert!(!check.balanced, "差 250 必须报");
        assert_eq!(check.diff, Decimal::from(-250));
        assert_eq!(check.diff_text, "-250.00");
        assert!(check.message.contains("不平"), "{}", check.message);
        assert!(
            check.hints.iter().any(|hint| hint.contains("1012")),
            "必须提示现金类口径（1012）：{:?}",
            check.hints
        );
        println!("[R3-⑧] 期末 = 货币资金 → 平；差 250 → 不平且点名口径 ✓");
    }

    // ---------- ⑨ 无法分类 ----------

    /// ⑨ 对方科目没有任何映射 → 归「其他」+ warnings 非空（不许静默丢）。
    #[test]
    fn r3_unclassified_flow_goes_to_other_with_warning() {
        // 1999 不在 V0183 科目表里，也没有任何分类行匹配
        let flows = run(&[
            two_line("0115", day(10, 5), "1002", "1999", 700),
            two_line("0116", day(10, 6), "1999", "1002", 300),
        ]);
        assert_eq!(flows.unclassified.len(), 2, "两笔都该进「其他」");
        assert_eq!(flows.unclassified_count(), 2);
        assert_eq!(flows.unclassified_amount(), (Decimal::from(700), Decimal::from(300)));
        let other = flows.section(OTHER_ACTIVITY).expect("其他分区");
        assert_eq!(other.inflow_total.current, Decimal::from(700));
        assert_eq!(other.outflow_total.current, Decimal::from(300));
        assert_eq!(other.net.current, Decimal::from(400));
        // 归「其他」的钱仍然计入净增加额 —— 报表自身加减不断
        assert_eq!(flows.net_increase.current, Decimal::from(400));
        assert!(
            flows.warnings.iter().any(|w| w.contains("未能分类")),
            "warnings 必须点名未分类：{:?}",
            flows.warnings
        );
        println!(
            "[R3-⑨] 未分类 2 笔（对方科目 1999）→ 其他分区 + warning ✓\n  {}",
            flows.warnings[0]
        );
    }

    // ---------- ⑩ 空期间 ----------

    /// ⑩ 一个凭证都没有 → 全 0，期初 = 期末。
    #[test]
    fn r3_empty_period_is_all_zero() {
        let flows = run(&[]);
        assert_eq!(flows.sections.len(), 4, "经营 / 投资 / 筹资 / 其他");
        for section in &flows.sections {
            assert!(section.net.is_zero(), "{} 净额应为 0", section.activity);
            for line in section.inflow.iter().chain(section.outflow.iter()) {
                assert!(line.current.is_zero() && line.ytd.is_zero());
            }
        }
        assert_eq!(flows.net_increase, AmountPair::default());
        let opening = Decimal::from(123);
        let closing = opening + flows.net_increase.current;
        assert_eq!(closing, opening, "空期间期初 = 期末");
        let check = build_check(&flows, closing, opening, opening, 0);
        assert!(check.balanced);
        assert!(flows.warnings.is_empty(), "{:?}", flows.warnings);
        println!("[R3-⑩] 空期间 → 全 0、期初 = 期末 ✓");
    }

    // ---------- ⑪ 复合凭证按对方科目拆分 ----------

    /// ⑪ 一笔收款拆给三个对方科目 → 各自归位（不做比例分摊、不留尾差）。
    #[test]
    fn r3_compound_voucher_splits_by_counterpart_account() {
        let voucher = voucher(
            "0117",
            day(10, 12),
            vec![
                entry("1002", 1_000, 0),
                entry("1122", 0, 600),
                entry("2211", 0, 150),
                entry("5001", 0, 250),
            ],
        );
        let flows = run(&[voucher]);
        assert_eq!(flows.current_of("OPER_SALES_RECEIPTS"), Decimal::from(850));
        // 贷方 2211：现金流入方向没有映射行（2211 只配在「流出」），→ 必然进「其他」
        assert_eq!(flows.current_of("OPER_STAFF_PAYMENTS"), Decimal::ZERO);
        assert_eq!(flows.net_increase.current, Decimal::from(1_000));
        assert_eq!(flows.unclassified.len(), 1, "2211 的现金流方向没有映射行");
        println!("[R3-⑪] 1000 = 1122 600 + 2211 150(→其他) + 5001 250；净增加 1000 ✓");
    }

    // ---------- ⑫ 分录被删 → 差额进「其他」并点名 ----------

    /// ⑫ 凭证分录被删（借贷不平）→ 差额进「其他」+ 点名凭证，不许静默丢。
    #[test]
    fn r3_unbalanced_voucher_residual_is_reported() {
        let mut broken = two_line("0118", day(10, 13), "1002", "5001", 1_000);
        broken.entries.retain(|entry| entry.credit.is_zero()); // 删掉贷方分录
        let flows = run(&[broken]);
        assert_eq!(
            flows.current_of("OPER_SALES_RECEIPTS"),
            Decimal::ZERO,
            "贷方分录没了，就没有对方科目"
        );
        let other = flows.section(OTHER_ACTIVITY).expect("其他分区");
        assert_eq!(other.inflow_total.current, Decimal::from(1_000));
        assert_eq!(
            flows
                .section(OTHER_ACTIVITY)
                .expect("其他分区")
                .inflow
                .iter()
                .map(|line| line.item_code.as_str())
                .collect::<Vec<_>>(),
            vec![OTHER_UNBALANCED]
        );
        assert_eq!(flows.unbalanced_vouchers.len(), 1);
        assert_eq!(flows.unbalanced_vouchers[0].voucher_no, "0118");
        assert!(flows.warnings.iter().any(|w| w.contains("0118")));
        println!("[R3-⑫] 分录被删 → 差额 1000 进「其他」+ 点名 0118 ✓");
    }

    // ---------- ⑬ 现金内部划转 ----------

    /// ⑬ 借 1001 / 贷 1002（现金内部划转）→ 没有任何活动现金流。
    #[test]
    fn r3_internal_cash_transfer_produces_no_flow() {
        let flows = run(&[two_line("0119", day(10, 14), "1001", "1002", 800)]);
        assert!(flows.unclassified.is_empty());
        assert_eq!(flows.net_increase.current, Decimal::ZERO);
        for section in &flows.sections {
            assert!(section.net.is_zero(), "{} 应为 0", section.activity);
        }
        assert!(flows.warnings.is_empty(), "{:?}", flows.warnings);
        println!("[R3-⑬] 库存现金 ↔ 银行存款划转 800 → 不影响现金流量 ✓");
    }

    // ---------- ⑭ 0.0001 不许被抹平 ----------

    /// ⑭ 差额 0.0001 不许被舍成 0.00（与另外两张表同一条硬红线）。
    #[test]
    fn r3_tiny_diff_is_not_rounded_away() {
        let flows = run(&[voucher(
            "0120",
            day(10, 15),
            vec![entry_dec("1002", "0.0001", "0"), entry_dec("5001", "0", "0.0001")],
        )]);
        let closing = Decimal::from_str("0.0001").expect("十进制");
        let check = build_check(&flows, closing, Decimal::ZERO, closing, 1);
        assert!(!check.balanced, "0.0001 的不平也必须报");
        assert_eq!(check.diff_text, "0.0001", "2 位小数会抹成 0.00");
        println!("[R3-⑭] 0.0001 的差额不被抹平 ✓");
    }

    // ---------- ⑮ 配置自洽（纯逻辑） ----------

    /// ⑮ 配置自洽：12 行 / 三活动都有流入流出 / 同方向不重叠。
    #[test]
    fn r3_config_is_self_consistent() {
        let items = items();
        assert_eq!(items.len(), 12);
        for (activity, _) in ACTIVITIES {
            assert!(
                items
                    .iter()
                    .any(|item| item.activity == activity && item.direction == INFLOW),
                "{activity} 缺流入行"
            );
            assert!(
                items
                    .iter()
                    .any(|item| item.activity == activity && item.direction == OUTFLOW),
                "{activity} 缺流出行"
            );
        }
        // 现金类科目不许出现在对方科目清单里
        for item in &items {
            for code in item.account_pattern.split(',') {
                assert!(
                    !is_cash_account(code),
                    "{} 把现金类科目 {code} 写进了对方科目清单",
                    item.item_code
                );
            }
        }
        // 每个科目在每个方向上至多命中一行，且至少在一个方向上被覆盖
        let codes = [
            "1121", "1122", "1123", "1131", "1132", "1221", "1401", "1402", "1403", "1404", "1405", "1408", "1601",
            "1602", "1604", "1605", "1606", "1701", "1702", "1801", "1901", "2101", "2202", "2203", "2211", "2221",
            "2221.01", "2221.02", "2231", "2241", "2401", "2501", "2701", "3001", "3101", "3102", "3103", "3104",
            "4001", "4101", "5001", "5051", "5111", "5301", "5401", "5402", "5403", "5601", "5602", "5603", "5711",
            "5801", "6401",
        ];
        let mut problems: Vec<String> = Vec::new();
        for code in codes {
            let mut total = 0_usize;
            for direction in [INFLOW, OUTFLOW] {
                let hits: Vec<&str> = items
                    .iter()
                    .filter(|item| item.direction == direction)
                    .filter(|item| report::match_account(&item.account_pattern, code, category_of(code)))
                    .map(|item| item.item_code.as_str())
                    .collect();
                total += hits.len();
                if hits.len() > 1 {
                    problems.push(format!(
                        "{code} 在 {direction} 上被 {} 行同时匹配（{:?}）—— 同一笔钱会算两遍",
                        hits.len(),
                        hits
                    ));
                }
            }
            if total == 0 {
                problems.push(format!("{code} 没有任何分类行（会全部落到「其他」）"));
            }
        }
        assert!(problems.is_empty(), "分类配置有问题：{problems:?}");
        println!("[R3-⑮] 配置自洽：12 行 / 三活动双向 / 每科目每方向至多一行 ✓");
    }

    /// ⑯ 期间参数校验与本年累计起点。
    #[test]
    fn r3_period_is_validated() {
        assert!(validate_period(None).is_err());
        assert!(validate_period(Some("  ")).is_err());
        assert!(validate_period(Some("2026-13")).is_err());
        assert!(validate_period(Some("2026-1")).is_err());
        assert_eq!(validate_period(Some(" 2026-10 ")).ok().as_deref(), Some("2026-10"));
        assert_eq!(ytd_start_of("2026-10"), NaiveDate::from_ymd_opt(2026, 1, 1));
        println!("[R3-⑯] period 校验 / 累计起点 ✓");
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
    async fn post_voucher(no: &str, date: &str, period: &str, debit_code: &str, credit_code: &str, amount: i64) {
        let sql = format!(
            "WITH v AS ( \
                INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, summary, \
                                         total_debit, total_credit, entry_count, status) \
                VALUES ('{FIXTURE_WORD}', '{no}', '{period}', DATE '{date}', 'ERPR3 夹具', \
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
             SELECT v.id, 1, 'ERPR3 借方', d.id, d.account_code, d.account_name, {amount}, 0 FROM v, d \
             UNION ALL \
             SELECT v.id, 2, 'ERPR3 贷方', c.id, c.account_code, c.account_name, 0, {amount} FROM v, c"
        );
        db().execute_with_params(&sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("建凭证失败：{error}\n{sql}"));
    }

    /// 在 REPEATABLE READ 事务里跑一次（与接口层同一条路径）。
    async fn run_snapshot(period: &str) -> CashFlowSnapshot {
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

    /// ⑰ 真库：一组业务凭证 → 三活动金额正确，且期末现金 = 资产负债表「货币资金」。
    #[tokio::test]
    async fn r3_real_statement_ties_to_balance_sheet_cash() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let baseline = run_snapshot("2026-10").await;
        let base_closing = baseline.closing_current;
        let base_bs = baseline.balance_sheet_cash;
        let base_opening = baseline.opening_current;

        // 收入 1000（经营流入）/ 采购 400（经营流出）/ 工资 150（经营流出）/
        // 交税 82（经营流出）/ 买设备 300（投资流出）/ 借款 500（筹资流入）
        post_voucher("0301", "2026-10-05", "2026-10", "1002", "5001", 1_000).await;
        post_voucher("0302", "2026-10-06", "2026-10", "5401", "1002", 400).await;
        post_voucher("0303", "2026-10-07", "2026-10", "2211", "1002", 150).await;
        // 交税 82（经营流出）—— 2221 自 V0226 起是「仅汇总」科目，记账落末级 2221.01
        post_voucher("0304", "2026-10-08", "2026-10", "2221.01", "1002", 82).await;
        post_voucher("0305", "2026-10-09", "2026-10", "1601", "1002", 300).await;
        post_voucher("0306", "2026-10-10", "2026-10", "1002", "2501", 500).await;

        let after = run_snapshot("2026-10").await;
        let check = after.check();
        let net = after.computed.net_increase.current;
        let operating = after.computed.net_of("OPERATING");
        let investing = after.computed.net_of("INVESTING");
        let financing = after.computed.net_of("FINANCING");
        let closing = after.closing_current;
        let bs_cash = after.balance_sheet_cash;
        let ledger = after.closing_by_ledger;
        cleanup().await;

        assert_eq!(
            operating.current - baseline.computed.net_of("OPERATING").current,
            Decimal::from(368),
            "经营净额 = 1000 − 400 − 150 − 82"
        );
        assert_eq!(
            investing.current - baseline.computed.net_of("INVESTING").current,
            Decimal::from(-300)
        );
        assert_eq!(
            financing.current - baseline.computed.net_of("FINANCING").current,
            Decimal::from(500)
        );
        assert_eq!(
            net - baseline.computed.net_increase.current,
            Decimal::from(568),
            "净增加 = 1000 − 400 − 150 − 82 − 300 + 500"
        );
        assert_eq!(closing - base_closing, Decimal::from(568), "期末 = 期初 + 净增加");
        assert_eq!(bs_cash - base_bs, Decimal::from(568), "资产负债表「货币资金」同步 +568");
        assert_eq!(closing, bs_cash, "期末现金必须逐分等于货币资金");
        assert_eq!(ledger, closing, "总账现金类科目余额也必须等于期末现金");
        assert!(
            check.balanced,
            "勾稽必须平：{} / hints={:?}",
            check.message, check.hints
        );
        assert_eq!(check.diff, Decimal::ZERO);
        assert_eq!(check.unclassified_count, 0, "这 6 张凭证都能分类");
        assert!(
            after.computed.warnings.is_empty(),
            "不该有告警：{:?}",
            after.computed.warnings
        );
        assert_eq!(after.opening_current, base_opening, "本期新增凭证不改变期初");
        println!(
            "[R3-⑰] 真库：经营 {} / 投资 {} / 筹资 {} / 净增 {} / 期末 {} = 货币资金 ✓",
            operating.current, investing.current, financing.current, net, closing
        );
    }

    /// ⑱ 真库：未分类的分录必须出现在「其他」与 warnings 里，并让勾稽照实报（不静默）。
    #[tokio::test]
    async fn r3_real_unclassified_is_visible() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        // 1012 其他货币资金不在现金类口径里 → 这笔钱既不是现金也无 counterpart 映射
        post_voucher("0321", "2026-10-05", "2026-10", "1002", "5001", 1_000).await;
        let baseline = run_snapshot("2026-10").await;
        let base_other = baseline.computed.net_of(OTHER_ACTIVITY).current;
        let base_warn = baseline.warnings.len();

        // 借 1002 / 贷 1999（库里没有的科目编码是插不进去的），改用 2401? 已映射。
        // 这里用「只删掉贷方分录」制造分类不出来的现金流：
        post_voucher("0322", "2026-10-06", "2026-10", "1002", "5001", 500).await;
        let sql = format!(
            "DELETE FROM biz_voucher_entry WHERE voucher_id = ( \
                SELECT id FROM biz_voucher WHERE voucher_no = '0322' AND voucher_word = '{FIXTURE_WORD}') \
               AND credit_amount > 0"
        );
        db().execute_with_params(&sql, vec![]).await.expect("删分录");

        let after = run_snapshot("2026-10").await;
        let other = after.computed.net_of(OTHER_ACTIVITY).current;
        // 出参里的 warnings = 快照级（口径）+ 计算级（未分类 / 分录不平）两段
        let mut warnings = after.warnings.clone();
        warnings.extend(after.computed.warnings.clone());
        let check = after.check();
        cleanup().await;

        assert_eq!(
            other - base_other,
            Decimal::from(500),
            "删掉贷方分录后，500 进「其他」（分录不平差额）"
        );
        assert_eq!(check.unclassified_count, 1);
        assert!(warnings.len() > base_warn, "必须有新增告警：{warnings:?}");
        assert!(
            warnings.iter().any(|w| w.contains("0322")),
            "告警要点名凭证：{warnings:?}"
        );
        // 现金账本身也被这笔孤立分录带歪了：勾稽必须照样把差额报出来（不许因为"平"就噤声）
        assert!(
            check.closing_balance != after.closing_by_ledger || !check.unbalanced_vouchers.is_empty(),
            "孤立分录必须留下痕迹（分录不平凭证 1 张）"
        );
        println!("[R3-⑱] 真库：分录被删 → 500 进「其他」+ 点名 0322 ✓");
    }

    /// ⑲ 真库：配置一致性（12 行 / 非现金科目全覆盖 / 每科目同方向唯一）。
    #[tokio::test]
    async fn r3_real_config_covers_all_accounts() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let tx = db().begin().await.expect("开启事务");
        let items = load_items(&tx).await.expect("读分类配置");
        let accounts = report::load_accounts(
            &tx,
            NaiveDate::from_ymd_opt(2026, 10, 1).expect("日期"),
            NaiveDate::from_ymd_opt(2026, 10, 31).expect("日期"),
            None,
        )
        .await
        .expect("读科目");
        let pattern = load_monetary_pattern(&tx).await.expect("读货币资金口径");
        let _ = tx.rollback().await;

        assert_eq!(items.len(), 12, "V0185 应种 12 条启用分类行");
        assert_eq!(
            pattern, MONETARY_PATTERN_FALLBACK,
            "货币资金口径应取自资产负债表（与 V0183 种子一致）"
        );

        let mut problems: Vec<String> = Vec::new();
        for account in &accounts {
            if is_cash_account(&account.account_code) || account.account_code == "1012" {
                continue;
            }
            let hits: Vec<&str> = items
                .iter()
                .filter(|item| {
                    report::match_account(&item.account_pattern, &account.account_code, &account.account_category)
                })
                .map(|item| item.item_code.as_str())
                .collect();
            if hits.is_empty() {
                problems.push(format!(
                    "{} {} 没有任何分类行（会全部落到「其他」）",
                    account.account_code, account.account_name
                ));
            }
        }
        assert!(problems.is_empty(), "分类配置漏科目：{problems:?}");
        println!(
            "[R3-⑲] 真库配置：{} 条分类行，{} 个非现金科目全覆盖、货币资金口径 {} ✓",
            items.len(),
            accounts.len(),
            pattern
        );
    }

    /// ⑳ 真库：空期间（2000-12）→ 全 0、期初 = 期末、勾稽平。
    #[tokio::test]
    async fn r3_real_empty_period_is_all_zero() {
        let Some(_guard) = setup_fixture().await else {
            return;
        };
        let snapshot = run_snapshot("2000-12").await;
        let check = snapshot.check();
        assert!(check.balanced, "空期间必须平：{}", check.message);
        assert_eq!(snapshot.computed.net_increase.current, Decimal::ZERO);
        assert_eq!(snapshot.opening_current, snapshot.closing_current);
        assert_eq!(snapshot.computed.cash_entry_count, 0);
        println!("[R3-⑳] 真库空期间 2000-12 → 全 0 且期初 = 期末 ✓");
    }
}

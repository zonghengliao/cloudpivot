//! 外币余额表（M1-07 · 多币种收口）：按 `(科目, 币种)` 列示**原币余额** + **本位币折算**。只读。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/fx-balance` | `business:erp:report:fx-balance` | 外币余额表（`period` 必填 `yyyy-MM`，`currency` 可选过滤） |
//!
//! # 会计口径（唯一真相，改这里必须同步卡内汇报与前端 `fx-balance.ts` 的类型注释）
//!
//! ```text
//! 范围：biz_account.is_foreign_currency = 1 且未删除的科目（V0408，M1-03）
//!       × 已过账（status='POSTED'）且未软删、voucher_date <= 期末日 的分录
//! 分组：(科目, 分录所在**凭证**的币种)；本位币凭证（currency_code = 账套本位币）不成行
//!
//! 原币（fc）: 只取外币凭证的分录 —— CASE 口径见 `period_fx_reval` 模块文档 ①
//!   期初 = Σ(fc_debit − fc_credit) FILTER (voucher_date <  期间首日)
//!   本期借 / 本期贷 = Σ fc_debit / Σ fc_credit FILTER (voucher_date >= 期间首日)
//!   期末 = 期初 + 本期借 − 本期贷            （一律「借方为正」的有符号口径）
//!
//! 账面本位币（book）: 取**全部**已过账分录（含本位币分录与历次调汇）
//!   期初 / 本期借 / 本期贷 / 期末  同上三式，但用 debit_amount / credit_amount
//!
//! 折算（只有在期末汇率可用时才有值）
//!   rate  = currency::spot_rate(币种, 本位币, 期间末日)  —— 即期 SPOT，最近一条（V0407 区间口径）
//!   conv  = round(期末原币 × rate, 4)
//!   diff  = conv − 期末账面            （= 尚未入账的汇兑差额；正 = 借方调增）
//! ```
//!
//! # 三条不能改的口径（每一条写错都会让报表数字静默失真）
//!
//! ## ① 原币只取外币凭证，本位币分录不许算进原币
//!
//! `fc_debit_amount` 为 `NULL` 表示「本位币分录」（V0409）。若按
//! `SUM(COALESCE(fc_debit_amount, debit_amount))` 无脑求和，**调汇凭证自己**
//! （本位币凭证、落在外币科目上、金额恰是上期调汇差额）会被当成原币金额 ⇒ 原币余额被
//! 自己的调汇额污染。所以取数按**凭证币种**分流（`CASE WHEN currency_code <> 本位币`），
//! 与 M1-04 / M1-05 逐字同口径。
//!
//! ## ② 账面必须包含本位币分录（含历次调汇）
//!
//! 上期调汇把账面调到上期期末汇率，本期 `diff = 折算 − 账面` 才是「汇率从 7.1 走到 7.9」
//! 这一段。把调汇凭证排除，每期都会把历史累计差额重新算一遍（报表差额越滚越大，
//! 且与 M1-05 的调汇结论对不上）。
//!
//! **一个科目一个账面，不是一个币种一个账面**：本位币分录没有币种归属，按币种切分会
//! 把它们整段丢掉。所以 SQL 用窗口函数同时取「该币种账面」与「科目合计账面」，
//! 单币种科目用科目合计（正常情况），同一科目挂了多个外币币种时退到「该币种自身账面」
//! 并在 `warnings` 里点名（治理办法是把不同币种拆到不同科目，而不是在这里猜分摊比例）
//! —— 这与 `period_fx_reval::FxAccountBalance::book_balance()` 是**同一个判断**。
//!
//! ## ③ 缺牌价不按 rate = 1 折算，折算列留空
//!
//! M1-05 落账时缺牌价必须 400 硬拒（少调一个科目 = 静默错账）。报表不一样：一张余额表
//! 不该因为某个币种没维护期末牌价就打不开。所以这里**不报错、不折算**：
//! `close_rate` / `converted_base` / `reval_diff` 全为 `null`、`rate_missing = true`、
//! 币种码进 `missing_rates`，`hints` 里点名去补牌价。
//!
//! **绝不回落 rate = 1**：外币余额按 1 折算成「原币 = 人民币」，比留空危险得多
//! （留空会被人问，填错会被人直接用）。同理 `Σconverted` / `Σdiff` 只在**全部**币种
//! 都有牌价时才给合计值，否则 `null` —— 半套折算的合计会被当成真数抄进报表。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_account / biz_voucher / biz_voucher_entry / biz_exchange_rate 是全局会计
//! 字典与台账（V0137/V0149 无 dept_id / 归属人列），数据范围表达不出来；可见性由
//! business:erp:report:fx-balance 权限码控制（与资产负债表 / 利润表同一套报表码口径）。
//! ```
//!
//! # 为什么不建台账表 / 不落快照
//!
//! 报表是**凭证的派生视图**：分录一改，报表必须跟着改。落一张 `biz_fx_balance_snapshot`
//! = 双真相（凭证被红冲而快照没跟着改 ⇒ 报表与账不符，且库里没有任何约束能跨表断言）。
//! 需要「历史版本」时用已结账期间 + 凭证只读来保证，而不是复制一份数字。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::business::erp::currency;
use crate::business::erp::period::period_fx_reval;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 报表编码（前端与导出文件名的稳定标识，不随文案变）。
pub const REPORT_CODE: &str = "FX_BALANCE";

/// 报表名称。
pub const REPORT_NAME: &str = "外币余额表";

/// 只统计已过账凭证（与 `ledger` / `period_fx_reval` 逐字一致）。
pub const POSTED: &str = "POSTED";

/// 账面口径：**科目合计**（单币种科目，含本位币分录与历次调汇）。
pub const BASIS_ACCOUNT: &str = "ACCOUNT";

/// 账面口径：**该币种分录合计**（多币种科目的退化口径，会在 `warnings` 里点名）。
pub const BASIS_CURRENCY: &str = "CURRENCY";

/// 科目表。
const ACCOUNT_TABLE: &str = "biz_account";

/// 凭证表。
const VOUCHER_TABLE: &str = "biz_voucher";

/// 分录表。
const ENTRY_TABLE: &str = "biz_voucher_entry";

/// 会计期间表。
const PERIOD_TABLE: &str = "biz_accounting_period";

/// 汇率表。
const RATE_TABLE: &str = "biz_exchange_rate";

/// 币种表。
const CURRENCY_TABLE: &str = "biz_currency";

// ==================== 入参 ====================

/// 外币余额表查询。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FxBalanceQuery {
    /// 会计期间 `yyyy-MM`（必填）。
    pub period: Option<String>,
    /// 币种过滤（三位大写，可选）：只出该币种的行。
    pub currency: Option<String>,
}

// ==================== 出参 ====================

/// 明细行：一个 `(科目, 币种)` 一行。
///
/// 所有金额都是**十进制字符串**（`NUMERIC(20,4)` 序列化后是字符串，前端不要过 `Number()`）。
/// 余额列一律「借方为正」（`借 − 贷` 的有符号口径），负数 = 贷方余额。
#[derive(Debug, Clone, Serialize)]
pub struct FxBalanceRow {
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别（`ASSET` / `LIABILITY` / `EQUITY` / `COST` / `PROFIT_LOSS`）。
    pub account_category: String,
    /// 余额方向（`DEBIT` / `CREDIT`，科目主数据，不参与计算）。
    pub balance_direction: String,
    /// 币种码（分录所在凭证的币种）。
    pub currency_code: String,
    /// 币种名称（`biz_currency.name`；主数据缺失时为 `null`）。
    pub currency_name: Option<String>,
    /// 期初原币余额。
    pub opening_fc: Decimal,
    /// 本期原币借方发生额。
    pub period_fc_debit: Decimal,
    /// 本期原币贷方发生额。
    pub period_fc_credit: Decimal,
    /// 期末原币余额。
    pub closing_fc: Decimal,
    /// 期初账面本位币余额。
    pub opening_book: Decimal,
    /// 本期账面本位币借方发生额。
    pub period_book_debit: Decimal,
    /// 本期账面本位币贷方发生额。
    pub period_book_credit: Decimal,
    /// 期末账面本位币余额。
    pub closing_book: Decimal,
    /// 账面取数口径：`ACCOUNT`（科目合计）/ `CURRENCY`（该币种，多币种科目退化）。
    pub book_basis: String,
    /// 期末即期汇率（`1 原币 = close_rate 本位币`）；缺牌价为 `null`。
    pub close_rate: Option<Decimal>,
    /// 折算本位币余额 = `round(期末原币 × close_rate, 4)`；缺牌价为 `null`。
    pub converted_base: Option<Decimal>,
    /// 汇兑差额 = `折算 − 期末账面`（正 = 借方调增）；缺牌价为 `null`。
    pub reval_diff: Option<Decimal>,
    /// 该行是否缺期末牌价。
    pub rate_missing: bool,
}

impl FxBalanceRow {
    /// 期末原币余额的方向（给人看的派生列）：`DEBIT` / `CREDIT` / `ZERO`。
    pub fn closing_side(&self) -> &'static str {
        side_of(self.closing_fc)
    }
}

/// 按币种汇总（渲染报表的「币种合计」段）。
#[derive(Debug, Clone, Serialize)]
pub struct FxCurrencySummary {
    /// 币种码。
    pub currency_code: String,
    /// 币种名称。
    pub currency_name: Option<String>,
    /// 该币种下的科目行数。
    pub account_count: i64,
    /// 期末原币余额合计（同一币种求和有意义）。
    pub closing_fc: Decimal,
    /// 期末账面本位币余额合计。
    pub closing_book: Decimal,
    /// 期末即期汇率；缺牌价为 `null`。
    pub close_rate: Option<Decimal>,
    /// 折算本位币合计；缺牌价为 `null`。
    pub converted_base: Option<Decimal>,
    /// 汇兑差额合计；缺牌价为 `null`。
    pub reval_diff: Option<Decimal>,
    /// 该币种是否缺期末牌价。
    pub rate_missing: bool,
}

/// 全表合计（跨币种只能用**本位币**列求和；原币跨币种相加没有意义）。
#[derive(Debug, Clone, Default)]
pub struct Totals {
    /// 期末账面本位币余额合计。
    pub closing_book: Decimal,
    /// 折算本位币合计（任一币种缺牌价 ⇒ `None`）。
    pub converted_base: Option<Decimal>,
    /// 汇兑差额合计（任一币种缺牌价 ⇒ `None`）。
    pub reval_diff: Option<Decimal>,
    /// 缺牌价的币种（去重、升序）。
    pub missing_rates: Vec<String>,
}

/// 外币余额表响应。
#[derive(Debug, Clone, Serialize)]
pub struct FxBalanceRes {
    /// 报表编码。
    pub report_code: String,
    /// 报表名称。
    pub report_name: String,
    /// 编制单位（`sys_config.system.name`）。
    pub company_name: String,
    /// 账套本位币。
    pub base_currency: String,
    /// 会计期间 `yyyy-MM`。
    pub period: String,
    /// 期间首日（含）。
    pub period_start: NaiveDate,
    /// 期间末日（含；也是取汇率的日子）。
    pub period_end: NaiveDate,
    /// 期间状态（`OPEN` / `CLOSED`；该月未建账为 `null`，期间按自然月推算）。
    pub period_status: Option<String>,
    /// 实际生效的币种过滤（规范化后的码；未过滤为 `null`）—— 回显，便于前端确认筛了哪个币种。
    pub currency_filter: Option<String>,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
    /// 明细行（按科目编码、币种升序）。
    pub rows: Vec<FxBalanceRow>,
    /// 按币种汇总（按币种码升序）。
    pub currencies: Vec<FxCurrencySummary>,
    /// 期末账面本位币合计。
    pub total_closing_book: Decimal,
    /// 折算本位币合计（任一币种缺牌价 ⇒ `null`）。
    pub total_converted_base: Option<Decimal>,
    /// 汇兑差额合计（任一币种缺牌价 ⇒ `null`）。
    pub total_reval_diff: Option<Decimal>,
    /// 缺期末牌价的币种（升序）。
    pub missing_rates: Vec<String>,
    /// 需要人工处理的问题（多币种科目退化 / 外币分录落在未标记科目上）。
    pub warnings: Vec<String>,
    /// 口径提示（固定几条 + 期间推算）。
    pub hints: Vec<String>,
}

// ==================== 纯逻辑 ====================

/// 余额方向：`DEBIT` / `CREDIT` / `ZERO`（有符号余额 → 方向，给人看的派生值）。
pub fn side_of(balance: Decimal) -> &'static str {
    if balance > Decimal::ZERO {
        "DEBIT"
    } else if balance < Decimal::ZERO {
        "CREDIT"
    } else {
        "ZERO"
    }
}

/// 账面口径判定：科目挂了多个外币币种 ⇒ 只能退到「该币种自身账面」。
///
/// 与 `period_fx_reval::FxAccountBalance::book_balance()` 同一个判断（报表与调汇必须一致，
/// 否则「报表上的差额」和「调汇凭证的金额」会对不上）。
pub fn book_basis_of(fx_currency_count: i64) -> &'static str {
    if fx_currency_count > 1 {
        BASIS_CURRENCY
    } else {
        BASIS_ACCOUNT
    }
}

/// SQL 取回的中间行（`agg` CTE 的一行 = 一个 `(科目, 币种)`，含科目合计口径的三列）。
#[derive(Debug, Clone)]
pub(crate) struct RawRow {
    account_id: Uuid,
    account_code: String,
    account_name: String,
    account_category: String,
    balance_direction: String,
    currency_code: String,
    opening_fc: Decimal,
    period_fc_debit: Decimal,
    period_fc_credit: Decimal,
    opening_book_currency: Decimal,
    period_book_debit_currency: Decimal,
    period_book_credit_currency: Decimal,
    opening_book_account: Decimal,
    period_book_debit_account: Decimal,
    period_book_credit_account: Decimal,
    fx_currency_count: i64,
}

impl RawRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_category: row.get_typed("account_category")?,
            balance_direction: row.get_typed("balance_direction")?,
            currency_code: row.get_typed("voucher_currency")?,
            opening_fc: row.get_typed("opening_fc")?,
            period_fc_debit: row.get_typed("period_fc_debit")?,
            period_fc_credit: row.get_typed("period_fc_credit")?,
            opening_book_currency: row.get_typed("opening_book_currency")?,
            period_book_debit_currency: row.get_typed("period_book_debit_currency")?,
            period_book_credit_currency: row.get_typed("period_book_credit_currency")?,
            opening_book_account: row.get_typed("opening_book_account")?,
            period_book_debit_account: row.get_typed("period_book_debit_account")?,
            period_book_credit_account: row.get_typed("period_book_credit_account")?,
            fx_currency_count: row.get_typed("fx_currency_count_account")?,
        })
    }
}

/// 组装一行明细（纯函数：库里的中间行 + 期末汇率 → 报表行）。
///
/// 返回 `None` 表示**该行没有可列示的内容**（期末原币为 0 且本期没有原币发生额）：
/// 这类行如果是「期初有、本期冲平」之外的纯零行（例如科目只发生过本位币业务），
/// 列出来只会让人以为「这里有一笔外币业务」。
pub(crate) fn assemble_row(raw: &RawRow, rate: Option<Decimal>, currency_name: Option<&str>) -> Option<FxBalanceRow> {
    let closing_fc = raw.opening_fc + raw.period_fc_debit - raw.period_fc_credit;
    if closing_fc.is_zero() && raw.period_fc_debit.is_zero() && raw.period_fc_credit.is_zero() {
        return None;
    }
    let basis = book_basis_of(raw.fx_currency_count);
    let (opening_book, period_book_debit, period_book_credit) = if basis == BASIS_CURRENCY {
        (
            raw.opening_book_currency,
            raw.period_book_debit_currency,
            raw.period_book_credit_currency,
        )
    } else {
        (
            raw.opening_book_account,
            raw.period_book_debit_account,
            raw.period_book_credit_account,
        )
    };
    let closing_book = (opening_book + period_book_debit - period_book_credit).round_dp(currency::AMOUNT_SCALE);
    let (close_rate, converted_base, reval_diff) = match rate {
        Some(rate) if rate > Decimal::ZERO => {
            let converted = period_fx_reval::revalued_base_of(closing_fc, rate);
            let diff = period_fx_reval::diff_of(closing_fc, rate, closing_book);
            (Some(rate), Some(converted), Some(diff))
        }
        // 缺牌价 / 脏数据（rate <= 0）：不折算、不猜（模块文档 ③）
        _ => (None, None, None),
    };
    Some(FxBalanceRow {
        account_id: raw.account_id.to_string(),
        account_code: raw.account_code.clone(),
        account_name: raw.account_name.clone(),
        account_category: raw.account_category.clone(),
        balance_direction: raw.balance_direction.clone(),
        currency_code: raw.currency_code.clone(),
        currency_name: currency_name.map(str::to_string),
        opening_fc: raw.opening_fc.round_dp(currency::AMOUNT_SCALE),
        period_fc_debit: raw.period_fc_debit.round_dp(currency::AMOUNT_SCALE),
        period_fc_credit: raw.period_fc_credit.round_dp(currency::AMOUNT_SCALE),
        closing_fc: closing_fc.round_dp(currency::AMOUNT_SCALE),
        opening_book: opening_book.round_dp(currency::AMOUNT_SCALE),
        period_book_debit: period_book_debit.round_dp(currency::AMOUNT_SCALE),
        period_book_credit: period_book_credit.round_dp(currency::AMOUNT_SCALE),
        closing_book,
        book_basis: basis.to_string(),
        close_rate,
        converted_base,
        reval_diff,
        rate_missing: close_rate.is_none(),
    })
}

/// 按币种汇总（纯函数，输入已按科目编码升序）。
pub fn summarise_currencies(rows: &[FxBalanceRow]) -> Vec<FxCurrencySummary> {
    let mut buckets: BTreeMap<&str, FxCurrencySummary> = BTreeMap::new();
    for row in rows {
        let entry = buckets
            .entry(row.currency_code.as_str())
            .or_insert_with(|| FxCurrencySummary {
                currency_code: row.currency_code.clone(),
                currency_name: row.currency_name.clone(),
                account_count: 0,
                closing_fc: Decimal::ZERO,
                closing_book: Decimal::ZERO,
                close_rate: row.close_rate,
                converted_base: Some(Decimal::ZERO),
                reval_diff: Some(Decimal::ZERO),
                rate_missing: false,
            });
        entry.account_count += 1;
        entry.closing_fc += row.closing_fc;
        entry.closing_book += row.closing_book;
        // 同一币种所有行的汇率必然相同；任一行缺牌价 ⇒ 该币种合计不给折算值
        match (row.converted_base, row.reval_diff) {
            (Some(converted), Some(diff)) => {
                if entry.converted_base.is_some() {
                    entry.converted_base = Some(entry.converted_base.unwrap_or_default() + converted);
                }
                if entry.reval_diff.is_some() {
                    entry.reval_diff = Some(entry.reval_diff.unwrap_or_default() + diff);
                }
            }
            _ => {
                entry.converted_base = None;
                entry.reval_diff = None;
                entry.close_rate = None;
                entry.rate_missing = true;
            }
        }
    }
    buckets
        .into_values()
        .map(|mut item| {
            item.closing_fc = item.closing_fc.round_dp(currency::AMOUNT_SCALE);
            item.closing_book = item.closing_book.round_dp(currency::AMOUNT_SCALE);
            item
        })
        .collect()
}

/// 全表合计（本位币列求和；折算列只在全部币种都有牌价时给值）。
pub fn totals_of(rows: &[FxBalanceRow]) -> Totals {
    let mut out = Totals::default();
    let mut missing: BTreeMap<&str, ()> = BTreeMap::new();
    let mut converted = Decimal::ZERO;
    let mut diff = Decimal::ZERO;
    let mut all_rated = true;
    for row in rows {
        out.closing_book += row.closing_book;
        match (row.converted_base, row.reval_diff) {
            (Some(value), Some(row_diff)) => {
                converted += value;
                diff += row_diff;
            }
            _ => {
                all_rated = false;
                missing.insert(row.currency_code.as_str(), ());
            }
        }
    }
    out.closing_book = out.closing_book.round_dp(currency::AMOUNT_SCALE);
    if all_rated {
        out.converted_base = Some(converted.round_dp(currency::AMOUNT_SCALE));
        out.reval_diff = Some(diff.round_dp(currency::AMOUNT_SCALE));
    }
    out.missing_rates = missing.into_keys().map(str::to_string).collect();
    out
}

/// `period` 必须是 `yyyy-MM`（月 01–12），返回 `(年, 月)`。
pub fn parse_month(period: &str) -> Option<(i32, u32)> {
    let text = period.trim();
    let (year, month) = text.split_once('-')?;
    if year.len() != 4 || month.len() != 2 {
        return None;
    }
    let year: i32 = year.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

/// 自然月区间（该月未建账时的兜底口径）：月首 / 月末（含）。
pub fn natural_month(period: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (year, month) = parse_month(period)?;
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let next_start = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
    Some((start, next_start.pred_opt()?))
}

// ==================== 库访问 ====================

/// 期间窗口：`(期间首日, 期间末日, 期间状态)`。
///
/// 期间表里没有这一行（该月未建账）⇒ 按**自然月**推算、状态 `None` —— 报表不该因为
/// 「这个月还没开账」就打不开（与 `ledger::period_range` 同一套兜底，但多回一列状态，
/// 前端要在页头写清楚「未开账·按自然月推算」）。
async fn period_window(period: &str) -> DbResult<(NaiveDate, NaiveDate, Option<String>)> {
    let Some(fallback) = natural_month(period) else {
        return Err(DbError::InvalidValue(format!(
            "period 必须是 yyyy-MM（月份 01–12），收到「{period}」"
        )));
    };
    let row = db()
        .query_first_sql(
            format!("SELECT start_date, end_date, status FROM {PERIOD_TABLE} WHERE period_code = ?"),
            vec![Value::Text(period.to_string())],
        )
        .await?;
    match row {
        Some(row) => Ok((
            row.get_typed("start_date")?,
            row.get_typed("end_date")?,
            row.get_typed("status")?,
        )),
        None => Ok((fallback.0, fallback.1, None)),
    }
}

/// 明细取数 SQL（见模块文档 ①②）。
///
/// 日期以字面量内联（值来自 `NaiveDate`，不可能注入；用参数会把 6 个 `?` 的绑定顺序
/// 变成维护陷阱），只绑定两个文本参数：**账套本位币** 与 **POSTED**。
fn rows_sql(period_start: NaiveDate, period_end: NaiveDate) -> String {
    format!(
        "WITH raw AS ( \
             SELECT a.id AS account_id, a.account_code, a.account_name, a.account_category, \
                    a.balance_direction, v.currency_code AS voucher_currency, v.voucher_date, \
                    (v.currency_code <> ?) AS is_fx, \
                    COALESCE(ve.fc_debit_amount, ve.debit_amount) AS fc_d, \
                    COALESCE(ve.fc_credit_amount, ve.credit_amount) AS fc_c, \
                    ve.debit_amount AS bk_d, ve.credit_amount AS bk_c \
               FROM {ENTRY_TABLE} ve \
               JOIN {VOUCHER_TABLE} v ON v.id = ve.voucher_id \
               JOIN {ACCOUNT_TABLE} a ON a.id = ve.account_id \
              WHERE a.is_foreign_currency = 1 AND a.deleted_at IS NULL \
                AND v.status = ? AND v.deleted_at IS NULL \
                AND v.voucher_date <= '{period_end}'::text::date \
         ), per_cur AS ( \
             SELECT account_id, account_code, account_name, account_category, balance_direction, \
                    voucher_currency, \
                    COALESCE(SUM(fc_d - fc_c) FILTER \
                        (WHERE is_fx AND voucher_date < '{period_start}'::text::date), 0) \
                        ::numeric(20, 4) AS opening_fc, \
                    COALESCE(SUM(fc_d) FILTER \
                        (WHERE is_fx AND voucher_date >= '{period_start}'::text::date), 0) \
                        ::numeric(20, 4) AS period_fc_debit, \
                    COALESCE(SUM(fc_c) FILTER \
                        (WHERE is_fx AND voucher_date >= '{period_start}'::text::date), 0) \
                        ::numeric(20, 4) AS period_fc_credit, \
                    COALESCE(SUM(bk_d - bk_c) FILTER \
                        (WHERE voucher_date < '{period_start}'::text::date), 0) \
                        ::numeric(20, 4) AS opening_book_currency, \
                    COALESCE(SUM(bk_d) FILTER \
                        (WHERE voucher_date >= '{period_start}'::text::date), 0) \
                        ::numeric(20, 4) AS period_book_debit_currency, \
                    COALESCE(SUM(bk_c) FILTER \
                        (WHERE voucher_date >= '{period_start}'::text::date), 0) \
                        ::numeric(20, 4) AS period_book_credit_currency, \
                    COUNT(DISTINCT voucher_currency) FILTER (WHERE is_fx) AS fx_currency_count \
               FROM raw \
              GROUP BY account_id, account_code, account_name, account_category, \
                       balance_direction, voucher_currency \
         ), agg AS ( \
             SELECT per_cur.*, \
                    SUM(opening_book_currency) OVER (PARTITION BY account_id) \
                        ::numeric(20, 4) AS opening_book_account, \
                    SUM(period_book_debit_currency) OVER (PARTITION BY account_id) \
                        ::numeric(20, 4) AS period_book_debit_account, \
                    SUM(period_book_credit_currency) OVER (PARTITION BY account_id) \
                        ::numeric(20, 4) AS period_book_credit_account, \
                    SUM(fx_currency_count) OVER (PARTITION BY account_id)::bigint \
                        AS fx_currency_count_account \
               FROM per_cur \
         ) \
         SELECT account_id, account_code, account_name, account_category, balance_direction, \
                voucher_currency, opening_fc, period_fc_debit, period_fc_credit, \
                opening_book_currency, period_book_debit_currency, period_book_credit_currency, \
                opening_book_account, period_book_debit_account, period_book_credit_account, \
                fx_currency_count_account \
           FROM agg \
          WHERE voucher_currency <> ? \
          ORDER BY account_code, voucher_currency"
    )
}

async fn load_rows(period_start: NaiveDate, period_end: NaiveDate, base_currency: &str) -> DbResult<Vec<RawRow>> {
    let rows = db()
        .query_sql(
            rows_sql(period_start, period_end),
            vec![
                Value::Text(base_currency.to_string()),
                Value::Text(POSTED.to_string()),
                Value::Text(base_currency.to_string()),
            ],
        )
        .await?;
    rows.iter().map(RawRow::from_row).collect()
}

/// 期末**每个外币币种**的即期汇率（一条 SQL 取回全部，避免按币种循环往返）。
///
/// `DISTINCT ON (from_currency) … ORDER BY from_currency, effective_date DESC` 与
/// `currency::spot_rate` 的取数口径逐字一致（含 V0407 的生效区间谓词）。
async fn rates_at(period_end: NaiveDate, base_currency: &str) -> DbResult<BTreeMap<String, Decimal>> {
    let rows = db()
        .query_sql(
            format!(
                "SELECT DISTINCT ON (from_currency) from_currency, rate \
                   FROM {RATE_TABLE} \
                  WHERE to_currency = ? AND rate_type = ? AND from_currency <> ? \
                    AND effective_date <= ?::text::date \
                    AND (effective_to IS NULL OR effective_to >= ?::text::date) \
                  ORDER BY from_currency, effective_date DESC"
            ),
            vec![
                Value::Text(base_currency.to_string()),
                Value::Text(currency::RATE_TYPE_SPOT.to_string()),
                Value::Text(base_currency.to_string()),
                Value::Text(period_end.to_string()),
                Value::Text(period_end.to_string()),
            ],
        )
        .await?;
    let mut out = BTreeMap::new();
    for row in rows.iter() {
        out.insert(
            row.get_typed::<String>("from_currency")?,
            row.get_typed::<Decimal>("rate")?,
        );
    }
    Ok(out)
}

/// 币种名称表（`code → name`），回显用；读不到只是名称空，不影响数字。
async fn currency_names() -> DbResult<BTreeMap<String, String>> {
    let rows = db()
        .query_sql(format!("SELECT code, name FROM {CURRENCY_TABLE}"), vec![])
        .await?;
    let mut out = BTreeMap::new();
    for row in rows.iter() {
        out.insert(row.get_typed::<String>("code")?, row.get_typed::<String>("name")?);
    }
    Ok(out)
}

/// 外币分录落在**未标记外币核算**的科目上的条数（数据质量探针，只提示不拦）。
///
/// 本表按 `is_foreign_currency = 1` 取数，这类分录不会出现在报表里 —— 静默漏数比报错更危险，
/// 所以给一条 `warnings` 点名，让人去把科目标记补上（或把凭证改到正确科目）。
async fn unmarked_foreign_entries(period_end: NaiveDate, base_currency: &str) -> DbResult<i64> {
    let row = db()
        .query_first_sql(
            format!(
                "SELECT COUNT(*)::bigint AS n \
                   FROM {ENTRY_TABLE} ve \
                   JOIN {VOUCHER_TABLE} v ON v.id = ve.voucher_id \
                   JOIN {ACCOUNT_TABLE} a ON a.id = ve.account_id \
                  WHERE a.is_foreign_currency = 0 AND a.deleted_at IS NULL \
                    AND v.status = ? AND v.deleted_at IS NULL \
                    AND v.currency_code <> ? \
                    AND v.voucher_date <= '{period_end}'::text::date"
            ),
            vec![Value::Text(POSTED.to_string()), Value::Text(base_currency.to_string())],
        )
        .await?;
    match row {
        Some(row) => row.get_typed("n"),
        None => Ok(0),
    }
}

// ==================== 主流程 ====================

/// 外币余额表（`period` 必填）；`currency` 可选过滤。
///
/// # 错误
///
/// * `period` 缺失 / 不是 `yyyy-MM` ⇒ `400 + 10001`；
/// * `currency` 不是三位大写字母 ⇒ `400 + 10001`（**不**静默忽略过滤条件 ——
///   用户以为筛了币种却看到全表，比报错严重）；
/// * 缺期末牌价 **不报错**（折算列留空，见模块文档 ③）。
pub async fn fx_balance(query: &FxBalanceQuery) -> std::result::Result<FxBalanceRes, ApiError> {
    let period = query
        .period
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("period 必填（yyyy-MM，例：2026-11）".to_string()))?;
    if parse_month(period).is_none() {
        return Err(bad_request(format!(
            "period 必须是 yyyy-MM（月份 01–12），收到「{period}」"
        )));
    }
    let filter_currency = match query.currency.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        None => None,
        Some(raw) => Some(
            currency::normalize_currency(raw)
                .ok_or_else(|| bad_request(format!("currency 必须是三位大写币种码（如 USD），收到「{raw}」")))?,
        ),
    };

    let base_currency = currency::base_currency_code(db())
        .await
        .map_err(|error| utils::db_error(error, "查询账套本位币"))?;
    let (period_start, period_end, period_status) = period_window(period)
        .await
        .map_err(|error| utils::db_error(error, "解析会计期间"))?;

    let raw_rows = load_rows(period_start, period_end, &base_currency)
        .await
        .map_err(|error| utils::db_error(error, "查询外币科目余额"))?;
    let rates = rates_at(period_end, &base_currency)
        .await
        .map_err(|error| utils::db_error(error, "查询期末即期汇率"))?;
    let names = currency_names()
        .await
        .map_err(|error| utils::db_error(error, "查询币种名称"))?;

    let mut rows: Vec<FxBalanceRow> = Vec::with_capacity(raw_rows.len());
    let mut multi_currency: Vec<(String, String, i64)> = Vec::new();
    for raw in &raw_rows {
        if let Some(wanted) = filter_currency.as_deref()
            && raw.currency_code != wanted
        {
            continue;
        }
        let rate = rates.get(&raw.currency_code).copied();
        let name = names.get(&raw.currency_code).map(String::as_str);
        if let Some(row) = assemble_row(raw, rate, name) {
            rows.push(row);
        }
        if raw.fx_currency_count > 1 {
            multi_currency.push((
                raw.account_code.clone(),
                raw.account_name.clone(),
                raw.fx_currency_count,
            ));
        }
    }
    rows.sort_by(|left, right| {
        left.account_code
            .cmp(&right.account_code)
            .then_with(|| left.currency_code.cmp(&right.currency_code))
    });

    let currencies = summarise_currencies(&rows);
    let totals = totals_of(&rows);
    let unmarked = unmarked_foreign_entries(period_end, &base_currency)
        .await
        .map_err(|error| utils::db_error(error, "查询未标记外币核算科目的外币分录"))?;

    let mut warnings: Vec<String> = Vec::new();
    multi_currency.sort_by(|left, right| left.0.cmp(&right.0));
    multi_currency.dedup_by(|left, right| left.0 == right.0);
    for (code, name, count) in &multi_currency {
        warnings.push(format!(
            "科目 {code} {name} 同时挂了 {count} 个外币币种：账面列按「该币种分录」口径（本位币分录没有\
             币种归属，无法按币种分摊）。建议把不同币种拆到不同科目"
        ));
    }
    if unmarked > 0 {
        warnings.push(format!(
            "有 {unmarked} 条外币凭证分录记在未标记「外币核算」的科目上（本表只统计已标记科目），\
             这些分录不在报表里 —— 请补齐科目标记或把凭证改到正确科目"
        ));
    }
    if let Some(wanted) = filter_currency.as_deref()
        && wanted == base_currency
    {
        warnings.push(format!(
            "{wanted} 是账套本位币，不构成外币余额（本表只列外币核算科目），所以结果为空"
        ));
    }

    let mut hints: Vec<String> = vec![
        "只统计已过账（POSTED）凭证；草稿与作废凭证不进本表".to_string(),
        "余额为「借方为正」的有符号口径（借 − 贷），负数表示贷方余额 —— 资产与负债科目共用一套公式".to_string(),
        "原币余额只取外币凭证的分录；账面本位币余额取该科目全部已过账分录（含历次调汇），与期末调汇同一口径"
            .to_string(),
        format!(
            "折算汇率 = {period_end} 的即期（SPOT）汇率，折算值 = round(期末原币 × 汇率, 4)；\
             汇兑差额 = 折算值 − 期末账面（正 = 借方调增）"
        ),
    ];
    if period_status.is_none() {
        hints.push(format!(
            "会计期间 {period} 未建账，本表按自然月 {period_start} ~ {period_end} 推算"
        ));
    }
    if !totals.missing_rates.is_empty() {
        hints.push(format!(
            "币种 {} 在 {period_end} 之前没有即期（SPOT）汇率：折算列与合计留空，**没有**按汇率 1 折算；\
             请先在「币种 / 汇率」里补牌价",
            totals.missing_rates.join("、")
        ));
    }

    Ok(FxBalanceRes {
        report_code: REPORT_CODE.to_string(),
        report_name: REPORT_NAME.to_string(),
        company_name: crate::business::erp::report::company_name().await,
        base_currency,
        period: period.to_string(),
        period_start,
        period_end,
        period_status,
        currency_filter: filter_currency.clone(),
        generated_at: Utc::now(),
        rows,
        currencies,
        total_closing_book: totals.closing_book,
        total_converted_base: totals.converted_base,
        total_reval_diff: totals.reval_diff,
        missing_rates: totals.missing_rates,
        warnings,
        hints,
    })
}

fn bad_request(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

// ==================== 接口 ====================

/// 外币余额表。
///
/// ```text
/// GET /business/erp/report/fx-balance?period=2026-11
/// GET /business/erp/report/fx-balance?period=2026-11&currency=USD
/// ```
#[nik::permission(
    path = "/business/erp/report/fx-balance",
    method = "GET",
    permission = "business:erp:report:fx-balance"
)]
#[nik::get("/business/erp/report/fx-balance")]
pub async fn fx_balance_handler(Query(query): Query<FxBalanceQuery>) -> ResResult<FxBalanceRes> {
    Ok(Res::ok(fx_balance(&query).await?))
}

// ==================== 单测 ====================
//
// 纯逻辑用例不连库；真库用例（`ERP_DB_URL` 未设则 SKIP）打通「真凭证落库 → 报表取数」：
//
// ```bash
// ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_m107_t1' \
//   cargo test --lib business::erp::fx_balance -- --test-threads=1
// ```
//
// 期间命名空间 **2059-12 / 2060-xx 归本模块独占**（2050 = 应收重估、2051 = 期末调汇、
// 2031 = period、2033-2040 = 其它期间类用例；2026 是生产期间，绝不碰）。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher::{self, AccountRef, EntryLine, FcAmount, PostVoucher};

    fn dec(raw: &str) -> Decimal {
        raw.parse().expect("测试用十进制字面量合法")
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("固定日期合法")
    }

    fn raw_row(code: &str, currency: &str) -> RawRow {
        RawRow {
            account_id: Uuid::nil(),
            account_code: code.to_string(),
            account_name: format!("{code} 科目"),
            account_category: "ASSET".to_string(),
            balance_direction: "DEBIT".to_string(),
            currency_code: currency.to_string(),
            opening_fc: Decimal::ZERO,
            period_fc_debit: Decimal::ZERO,
            period_fc_credit: Decimal::ZERO,
            opening_book_currency: Decimal::ZERO,
            period_book_debit_currency: Decimal::ZERO,
            period_book_credit_currency: Decimal::ZERO,
            opening_book_account: Decimal::ZERO,
            period_book_debit_account: Decimal::ZERO,
            period_book_credit_account: Decimal::ZERO,
            fx_currency_count: 1,
        }
    }

    // ---------- 期间解析 ----------

    #[test]
    fn parse_month_accepts_canonical_and_rejects_the_rest() {
        assert_eq!(parse_month("2060-01"), Some((2060, 1)));
        assert_eq!(parse_month(" 2026-12 "), Some((2026, 12)));
        // 月 0 / 13 / 少位 / 分隔符错 / 空串一律 None（调用方 400，不猜成 1 月或 12 月）
        assert_eq!(parse_month("2026-00"), None);
        assert_eq!(parse_month("2026-13"), None);
        assert_eq!(parse_month("2026-1"), None);
        assert_eq!(parse_month("26-01"), None);
        assert_eq!(parse_month("2026/01"), None);
        assert_eq!(parse_month(""), None);
    }

    #[test]
    fn natural_month_covers_month_ends_and_leap_february() {
        assert_eq!(natural_month("2060-01"), Some((date(2060, 1, 1), date(2060, 1, 31))));
        // 12 月要跨年（+1 月 → 次年 1 月首日 − 1 天）
        assert_eq!(natural_month("2060-12"), Some((date(2060, 12, 1), date(2060, 12, 31))));
        // 闰年 2 月 29 天、平年 28 天
        assert_eq!(natural_month("2028-02"), Some((date(2028, 2, 1), date(2028, 2, 29))));
        assert_eq!(natural_month("2027-02"), Some((date(2027, 2, 1), date(2027, 2, 28))));
        assert_eq!(natural_month("2060-13"), None);
    }

    // ---------- 账面口径 ----------

    #[test]
    fn single_currency_account_uses_account_total_book() {
        // 单币种科目：账面必须用**科目合计**（含那笔本位币分录 / 上期调汇）
        let mut raw = raw_row("M107A", "USD");
        raw.opening_book_currency = dec("1400");
        raw.opening_book_account = dec("1400");
        raw.period_book_debit_currency = dec("720");
        raw.period_book_debit_account = dec("770"); // 多出的 50 是纯本位币分录
        raw.period_book_credit_currency = Decimal::ZERO;
        raw.period_book_credit_account = Decimal::ZERO;
        raw.opening_fc = dec("200");
        raw.period_fc_debit = dec("100");
        let row = assemble_row(&raw, Some(dec("7.9")), Some("美元")).expect("有余额就有行");
        assert_eq!(row.book_basis, BASIS_ACCOUNT);
        assert_eq!(row.closing_fc, dec("300"));
        assert_eq!(row.closing_book, dec("2170"), "1400 + 770：本位币分录必须算进账面");
        assert_eq!(row.converted_base, Some(dec("2370")), "300 × 7.9");
        assert_eq!(row.reval_diff, Some(dec("200")), "2370 − 2170");
        assert_eq!(row.currency_name.as_deref(), Some("美元"));
        assert_eq!(row.closing_side(), "DEBIT");
    }

    #[test]
    fn multi_currency_account_degrades_to_currency_book() {
        // 多币种科目：本位币分录没有币种归属，只能退到「该币种自身账面」
        let mut raw = raw_row("M107A", "USD");
        raw.fx_currency_count = 2;
        raw.opening_book_currency = dec("1400");
        raw.opening_book_account = dec("1450"); // 含 50 的本位币分录
        raw.opening_fc = dec("200");
        let row = assemble_row(&raw, Some(dec("7.9")), None).expect("有余额就有行");
        assert_eq!(row.book_basis, BASIS_CURRENCY);
        assert_eq!(row.closing_book, dec("1400"), "退到该币种账面，不猜分摊比例");
        assert_eq!(row.reval_diff, Some(dec("180")), "1580 − 1400");
        assert_eq!(book_basis_of(1), BASIS_ACCOUNT);
        assert_eq!(book_basis_of(3), BASIS_CURRENCY);
    }

    #[test]
    fn credit_balance_keeps_sign_and_side() {
        // 负债类外币科目：余额是贷方，有符号口径下为负 —— **不许**取绝对值「修正」
        let mut raw = raw_row("M107L", "USD");
        raw.opening_fc = dec("-200");
        raw.period_fc_credit = dec("100");
        raw.opening_book_account = dec("-1400");
        raw.period_book_credit_account = dec("720");
        let row = assemble_row(&raw, Some(dec("7.9")), None).expect("有余额就有行");
        assert_eq!(row.closing_fc, dec("-300"));
        assert_eq!(row.closing_book, dec("-2120"));
        assert_eq!(row.converted_base, Some(dec("-2370")));
        assert_eq!(row.reval_diff, Some(dec("-250")));
        assert_eq!(row.closing_side(), "CREDIT");
    }

    #[test]
    fn missing_rate_leaves_conversion_empty_instead_of_rate_one() {
        let mut raw = raw_row("M107A", "EUR");
        raw.opening_fc = dec("100");
        raw.opening_book_account = dec("790");
        let row = assemble_row(&raw, None, None).expect("有余额就有行");
        assert_eq!(row.close_rate, None);
        assert_eq!(row.converted_base, None, "缺牌价不许按 rate = 1 折算");
        assert_eq!(row.reval_diff, None);
        assert!(row.rate_missing);
        assert_eq!(row.closing_book, dec("790"), "账面列不受缺牌价影响");

        // 脏数据：rate <= 0 同样按「缺牌价」处理（库层 CHECK 已拦，但别让 0 流进折算）
        let zero = assemble_row(&raw, Some(Decimal::ZERO), None).expect("有余额就有行");
        assert!(zero.rate_missing);
        assert_eq!(zero.converted_base, None);
    }

    #[test]
    fn blank_rows_are_skipped_but_closed_positions_are_kept() {
        // 期末为 0 且本期无发生额 ⇒ 无内容，不出行（例如科目只记过本位币业务）
        let blank = raw_row("M107A", "USD");
        assert!(assemble_row(&blank, Some(dec("7.9")), None).is_none());

        // 本期把外币仓位冲平（期末 0 但本期有发生额）⇒ 必须出行：这张报表要看「关仓」这件事
        let mut closed = raw_row("M107A", "USD");
        closed.opening_fc = dec("100");
        closed.period_fc_credit = dec("100");
        closed.opening_book_account = dec("710");
        closed.period_book_credit_account = dec("710");
        let row = assemble_row(&closed, Some(dec("7.9")), None).expect("冲平也要出行");
        assert_eq!(row.closing_fc, Decimal::ZERO);
        assert_eq!(row.closing_book, Decimal::ZERO);
        assert_eq!(row.reval_diff, Some(Decimal::ZERO));
        assert_eq!(row.closing_side(), "ZERO");
    }

    // ---------- 汇总 ----------

    #[test]
    fn currency_summary_and_totals_hold_the_line_on_missing_rates() {
        let usd = FxBalanceRow {
            account_id: Uuid::nil().to_string(),
            account_code: "M107A".to_string(),
            account_name: "M107A".to_string(),
            account_category: "ASSET".to_string(),
            balance_direction: "DEBIT".to_string(),
            currency_code: "USD".to_string(),
            currency_name: Some("美元".to_string()),
            opening_fc: dec("100"),
            period_fc_debit: Decimal::ZERO,
            period_fc_credit: Decimal::ZERO,
            closing_fc: dec("100"),
            opening_book: dec("710"),
            period_book_debit: Decimal::ZERO,
            period_book_credit: Decimal::ZERO,
            closing_book: dec("710"),
            book_basis: BASIS_ACCOUNT.to_string(),
            close_rate: Some(dec("7.9")),
            converted_base: Some(dec("790")),
            reval_diff: Some(dec("80")),
            rate_missing: false,
        };
        let mut eur = usd.clone();
        eur.currency_code = "EUR".to_string();
        eur.currency_name = Some("欧元".to_string());
        eur.closing_fc = dec("50");
        eur.closing_book = dec("390");
        eur.close_rate = None;
        eur.converted_base = None;
        eur.reval_diff = None;
        eur.rate_missing = true;

        let rows = vec![eur.clone(), usd.clone()];
        let currencies = summarise_currencies(&rows);
        assert_eq!(currencies.len(), 2);
        assert_eq!(currencies[0].currency_code, "EUR", "按币种码升序");
        assert_eq!(currencies[0].account_count, 1);
        assert_eq!(currencies[0].closing_book, dec("390"));
        assert_eq!(currencies[0].converted_base, None, "缺牌价的币种不给折算合计");
        assert!(currencies[0].rate_missing);
        assert_eq!(currencies[1].currency_code, "USD");
        assert_eq!(currencies[1].converted_base, Some(dec("790")));
        assert_eq!(currencies[1].reval_diff, Some(dec("80")));

        let totals = totals_of(&rows);
        assert_eq!(totals.closing_book, dec("1100"), "本位币账面可以跨币种求和");
        assert_eq!(totals.converted_base, None, "半套折算的合计会被人当成真数抄走 ⇒ 不给");
        assert_eq!(totals.reval_diff, None);
        assert_eq!(totals.missing_rates, vec!["EUR".to_string()]);

        // 全部有牌价时才给合计
        let totals_full = totals_of(&[usd.clone(), usd]);
        assert_eq!(totals_full.converted_base, Some(dec("1580")));
        assert_eq!(totals_full.reval_diff, Some(dec("160")));
        assert!(totals_full.missing_rates.is_empty());
    }

    #[test]
    fn side_of_marks_three_states() {
        assert_eq!(side_of(dec("0.0001")), "DEBIT");
        assert_eq!(side_of(Decimal::ZERO), "ZERO");
        assert_eq!(side_of(dec("-0.0001")), "CREDIT");
    }

    // ---------- 真库（ERP_DB_URL 未设则 SKIP） ----------

    /// 报表期间。
    const PERIOD: &str = "2060-01";
    /// 期初所在期间（期初余额必须落在期间首日之前）。
    const PERIOD_OPENING: &str = "2059-12";
    /// 外币资产科目（借方余额）。
    const ACC_ASSET: &str = "M107FX01";
    /// 外币对方科目（贷方余额，制造借贷平衡）。
    const ACC_COUNTER: &str = "M107FX02";
    /// 本位币科目（给外币科目灌纯本位币分录，验证账面口径 ②）。
    const ACC_BASE: &str = "M107BASE";

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = crate::business::erp::voucher::voucher_query::tests::DB_LOCK
            .lock()
            .await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    /// 造数前的准备：腾空本套件的 `M107-` 夹具，并腾出 USD/EUR 汇率区间
    /// （V0407 的「区间不重叠」约束要求整对清掉，与 M1-05 的 cleanup 同口径）。
    async fn cleanup() {
        purge_fixture_rows().await;
        db().exec_sql(
            "DELETE FROM biz_exchange_rate WHERE from_currency IN ('USD', 'EUR')".to_string(),
            vec![],
        )
        .await
        .ok();
    }

    /// **只删本套件自己的行**（收尾清场用：不碰汇率表 —— 那是按币种清的准备工作）。
    async fn purge_fixture_rows() {
        for sql in [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'M107%')",
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'M107%')",
            "DELETE FROM biz_voucher WHERE source_no LIKE 'M107%'",
            "DELETE FROM biz_account WHERE account_code LIKE 'M107%'",
            "DELETE FROM biz_accounting_period WHERE period_code LIKE '2059-%' OR period_code LIKE '2060-%'",
        ] {
            let _ = db().exec_sql(sql.to_string(), vec![]).await;
        }
    }

    async fn seed_account(code: &str, name: &str, is_fx: i16) -> AccountRef {
        db().exec_sql(
            "INSERT INTO biz_account (account_code, account_name, level, account_category, \
                                     balance_direction, is_leaf, is_foreign_currency) \
             VALUES (?, ?, 1, 'ASSET', 'DEBIT', 1, ?) \
             ON CONFLICT (account_code) DO UPDATE SET is_leaf = 1, \
                 is_foreign_currency = EXCLUDED.is_foreign_currency"
                .to_string(),
            vec![
                Value::Text(code.to_string()),
                Value::Text(name.to_string()),
                Value::I16(is_fx),
            ],
        )
        .await
        .expect("种科目");
        let row = db()
            .query_first_sql(
                "SELECT id FROM biz_account WHERE account_code = ?".to_string(),
                vec![Value::Text(code.to_string())],
            )
            .await
            .expect("读科目")
            .expect("科目必须存在");
        AccountRef {
            id: row.get_typed("id").expect("科目 id"),
            code: code.to_string(),
            name: name.to_string(),
        }
    }

    async fn seed_period(period_code: &str, start: &str, end: &str) {
        let (year, month) = parse_month(period_code).expect("期间码合法");
        db().exec_sql(
            format!(
                "INSERT INTO biz_accounting_period \
                    (period_year, period_month, period_code, start_date, end_date, status) \
                 VALUES (?, ?, '{period_code}', '{start}'::text::date, '{end}'::text::date, 'OPEN') \
                 ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN', closed_at = NULL, \
                     closed_by = NULL"
            ),
            vec![Value::I16(year as i16), Value::I16(month as i16)],
        )
        .await
        .expect("种期间");
    }

    async fn seed_rate(currency: &str, on_date: &str, rate: &str) {
        // V0407 的**区间不重叠**排除约束（`ex_biz_exchange_rate_period`）要求每行的
        // `[effective_date, effective_to]` 互不相交，而直接 INSERT 的 `effective_to` 默认 NULL
        // = 无限 ⇒ 先插早行会让后面的行全部冲突。这里按写入口的口径**派生**区间：
        //   ① 上一条（该日之前的最新一行）的 `effective_to` = 该日 − 1；
        //   ② 本行的 `effective_to` = 该币种下一条生效日 − 1（没有下一条则 NULL）。
        db().exec_sql(
            "UPDATE biz_exchange_rate SET effective_to = ?::text::date - 1 \
              WHERE from_currency = ? AND to_currency = 'CNY' AND rate_type = 'SPOT' \
                AND effective_date < ?::text::date \
                AND (effective_to IS NULL OR effective_to >= ?::text::date)"
                .to_string(),
            vec![
                Value::Text(on_date.to_string()),
                Value::Text(currency.to_string()),
                Value::Text(on_date.to_string()),
                Value::Text(on_date.to_string()),
            ],
        )
        .await
        .expect("让位上一条汇率区间");
        db().exec_sql(
            "INSERT INTO biz_exchange_rate \
                 (from_currency, to_currency, rate_type, effective_date, effective_to, rate) \
             VALUES (?, 'CNY', 'SPOT', ?::text::date, \
                     (SELECT MIN(r.effective_date) - 1 FROM biz_exchange_rate r \
                       WHERE r.from_currency = ? AND r.to_currency = 'CNY' \
                         AND r.rate_type = 'SPOT' AND r.effective_date > ?::text::date), \
                     ?::text::numeric) \
             ON CONFLICT (from_currency, to_currency, rate_type, effective_date) \
             DO UPDATE SET rate = EXCLUDED.rate, effective_to = EXCLUDED.effective_to"
                .to_string(),
            vec![
                Value::Text(currency.to_string()),
                Value::Text(on_date.to_string()),
                Value::Text(currency.to_string()),
                Value::Text(on_date.to_string()),
                Value::Text(rate.to_string()),
            ],
        )
        .await
        .expect("种汇率");
    }

    /// 一张外币凭证（两行，借贷相等；本位币金额由调用方给定）。
    #[allow(clippy::too_many_arguments)]
    async fn post_fx(
        tag: &str,
        on_date: NaiveDate,
        rate: &str,
        debit_account: &AccountRef,
        credit_account: &AccountRef,
        fc_amount: &str,
        base_amount: &str,
        currency: &str,
        status: &str,
    ) {
        let entries = vec![
            EntryLine {
                line_no: 1,
                summary: format!("M107 造数 {tag} 借"),
                account: debit_account.clone(),
                debit: dec(base_amount),
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
            EntryLine {
                line_no: 2,
                summary: format!("M107 造数 {tag} 贷"),
                account: credit_account.clone(),
                debit: Decimal::ZERO,
                credit: dec(base_amount),
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
        ];
        let fc = vec![
            FcAmount {
                debit: dec(fc_amount),
                credit: Decimal::ZERO,
            },
            FcAmount {
                debit: Decimal::ZERO,
                credit: dec(fc_amount),
            },
        ];
        voucher::post_voucher_fx(
            db(),
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_AUTO,
                source_type: "M107",
                source_id: None,
                source_no: Some(tag),
                source_doc_no: None,
                voucher_date: on_date,
                summary: format!("M107 造数凭证 {tag}"),
                lines: entries,
                remark: Some("M1-07 真库用例夹具"),
                red_of: None,
                status,
            },
            Some(&fc),
            Some((currency, rate.parse::<Decimal>().expect("汇率"))),
            None,
            Utc::now(),
        )
        .await
        .expect("外币凭证落库");
    }

    /// 一张**纯本位币**凭证（两行）：只改账面、不改原币。
    async fn post_base(tag: &str, on_date: NaiveDate, debit: &AccountRef, credit: &AccountRef, amount: &str) {
        let entries = vec![
            EntryLine {
                line_no: 1,
                summary: format!("M107 本位币 {tag} 借"),
                account: debit.clone(),
                debit: dec(amount),
                credit: Decimal::ZERO,
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
            EntryLine {
                line_no: 2,
                summary: format!("M107 本位币 {tag} 贷"),
                account: credit.clone(),
                debit: Decimal::ZERO,
                credit: dec(amount),
                aux_supplier_id: None,
                aux_material_id: None,
                aux_warehouse_id: None,
                aux_dept_id: None,
            },
        ];
        voucher::post_voucher_fx(
            db(),
            &PostVoucher {
                voucher_type: voucher::VOUCHER_TYPE_AUTO,
                source_type: "M107",
                source_id: None,
                source_no: Some(tag),
                source_doc_no: None,
                voucher_date: on_date,
                summary: format!("M107 本位币凭证 {tag}"),
                lines: entries,
                remark: Some("M1-07 真库用例夹具"),
                red_of: None,
                status: voucher::STATUS_POSTED,
            },
            None,
            Some(("CNY", Decimal::ONE)),
            None,
            Utc::now(),
        )
        .await
        .expect("本位币凭证落库");
    }

    fn report_query(period: &str, currency: Option<&str>) -> FxBalanceQuery {
        FxBalanceQuery {
            period: Some(period.to_string()),
            currency: currency.map(str::to_string),
        }
    }

    /// 主用例：期初 + 本期外币发生 + 本期纯本位币分录（账面 ②）+ 期末牌价 ③。
    async fn seed_main_scenario() -> (AccountRef, AccountRef, AccountRef) {
        let asset = seed_account(ACC_ASSET, "M107 外币资产", 1).await;
        let counter = seed_account(ACC_COUNTER, "M107 外币对方", 1).await;
        let base = seed_account(ACC_BASE, "M107 本位币", 0).await;
        seed_period(PERIOD_OPENING, "2059-12-01", "2059-12-31").await;
        seed_period(PERIOD, "2060-01-01", "2060-01-31").await;
        // 期初：200 USD @ 7.0 = 1400
        post_fx(
            "M107-O1",
            date(2059, 12, 20),
            "7.0",
            &asset,
            &counter,
            "200",
            "1400",
            "USD",
            voucher::STATUS_POSTED,
        )
        .await;
        // 本期：100 USD @ 7.2 = 720
        post_fx(
            "M107-A1",
            date(2060, 1, 10),
            "7.2",
            &asset,
            &counter,
            "100",
            "720",
            "USD",
            voucher::STATUS_POSTED,
        )
        .await;
        // 本期：给对方科目灌一笔纯本位币 50（只改账面、不改原币）
        post_base("M107-A2", date(2060, 1, 20), &counter, &base, "50").await;
        // 期末 7.9；再补一条**更晚**的 8.5，证明取数只认期间末日之前的牌价
        seed_rate("USD", "2060-01-31", "7.9").await;
        seed_rate("USD", "2060-02-05", "8.5").await;
        (asset, counter, base)
    }

    /// 真库：期初 / 本期 / 期末三层数字 + 折算 + 差额（含「账面必须含本位币分录」）。
    #[tokio::test]
    async fn fx_balance_reports_opening_period_and_converted_balances() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_main_scenario().await;

        let report = fx_balance(&report_query(PERIOD, None)).await.expect("报表应成功");
        assert_eq!(report.report_code, REPORT_CODE);
        assert_eq!(report.report_name, REPORT_NAME);
        assert_eq!(report.base_currency, "CNY");
        assert_eq!(report.period, PERIOD);
        assert_eq!(report.period_start, date(2060, 1, 1));
        assert_eq!(report.period_end, date(2060, 1, 31));
        assert_eq!(report.period_status.as_deref(), Some("OPEN"));
        assert_eq!(report.rows.len(), 2, "两个外币科目各一行：{:?}", report.rows);

        let asset = report
            .rows
            .iter()
            .find(|row| row.account_code == ACC_ASSET)
            .expect("资产科目行");
        assert_eq!(asset.currency_code, "USD");
        assert_eq!(asset.opening_fc, dec("200"), "期初原币（12-20 那笔）");
        assert_eq!(asset.period_fc_debit, dec("100"));
        assert_eq!(asset.period_fc_credit, Decimal::ZERO);
        assert_eq!(asset.closing_fc, dec("300"));
        assert_eq!(asset.opening_book, dec("1400"));
        assert_eq!(asset.period_book_debit, dec("720"));
        assert_eq!(asset.closing_book, dec("2120"));
        assert_eq!(asset.book_basis, BASIS_ACCOUNT);
        assert_eq!(asset.close_rate, Some(dec("7.9")), "取期间末日牌价，不是更晚的 8.5");
        assert_eq!(asset.converted_base, Some(dec("2370")), "300 × 7.9");
        assert_eq!(asset.reval_diff, Some(dec("250")), "2370 − 2120");
        assert!(!asset.rate_missing);

        let counter = report
            .rows
            .iter()
            .find(|row| row.account_code == ACC_COUNTER)
            .expect("对方科目行");
        assert_eq!(counter.opening_fc, dec("-200"), "贷方余额是负数，不取绝对值");
        assert_eq!(counter.period_fc_credit, dec("100"));
        assert_eq!(counter.closing_fc, dec("-300"));
        assert_eq!(
            counter.closing_book,
            dec("-2070"),
            "账面必须是**科目合计**（−1400 + 50 本位币借 − 720），按币种切分会丢掉那 50"
        );
        assert_eq!(counter.converted_base, Some(dec("-2370")));
        assert_eq!(counter.reval_diff, Some(dec("-300")));

        assert_eq!(report.currencies.len(), 1);
        assert_eq!(report.currencies[0].currency_code, "USD");
        assert_eq!(report.currencies[0].account_count, 2);
        assert_eq!(report.currencies[0].closing_fc, Decimal::ZERO, "一正一负同币种可相加");
        assert_eq!(report.total_closing_book, dec("50"));
        assert_eq!(report.total_converted_base, Some(Decimal::ZERO));
        assert_eq!(report.total_reval_diff, Some(dec("-50")));
        assert!(report.missing_rates.is_empty());
        assert!(
            report.warnings.is_empty(),
            "干净数据不该有 warnings：{:?}",
            report.warnings
        );
        assert!(
            report.hints.iter().any(|hint| hint.contains("即期")),
            "hints 要写清折算用的是即期汇率：{:?}",
            report.hints
        );
    }

    /// 真库：草稿凭证与「期间之后」的凭证都不进本表。
    #[tokio::test]
    async fn fx_balance_ignores_drafts_and_entries_after_period_end() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (asset, counter, _base) = seed_main_scenario().await;
        let before = fx_balance(&report_query(PERIOD, None)).await.expect("基线报表");
        // 草稿：金额巨大，进表就一眼看得出来
        post_fx(
            "M107-D1",
            date(2060, 1, 25),
            "7.9",
            &asset,
            &counter,
            "999",
            "7892.1",
            "USD",
            voucher::STATUS_DRAFT,
        )
        .await;
        // 期末之后的凭证（2060-02）：不该进 2060-01 的余额
        post_fx(
            "M107-P1",
            date(2060, 2, 10),
            "7.9",
            &asset,
            &counter,
            "500",
            "3950",
            "USD",
            voucher::STATUS_POSTED,
        )
        .await;

        let after = fx_balance(&report_query(PERIOD, None)).await.expect("再取报表");
        assert_eq!(after.rows.len(), before.rows.len(), "草稿与期末之后的凭证都不能新增行");
        let asset_after = after
            .rows
            .iter()
            .find(|row| row.account_code == ACC_ASSET)
            .expect("资产科目行");
        assert_eq!(asset_after.closing_fc, dec("300"), "草稿 999 与 2 月的 500 都不算");
        assert_eq!(asset_after.closing_book, dec("2120"));
    }

    /// 真库：多币种科目退化到「该币种账面」并给出 warning（不猜分摊比例）。
    #[tokio::test]
    async fn fx_balance_degrades_to_currency_book_for_multi_currency_account() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (asset, counter, base) = seed_main_scenario().await;
        // 同一科目再挂一个 EUR 币种（100 EUR @ 8.1）
        seed_rate("EUR", "2060-01-31", "8.1").await;
        post_fx(
            "M107-E1",
            date(2060, 1, 15),
            "8.1",
            &asset,
            &counter,
            "100",
            "810",
            "EUR",
            voucher::STATUS_POSTED,
        )
        .await;
        // 再给资产科目灌一笔纯本位币 60：科目合计账面 = 2120 + 60，该币种账面仍是 2120
        post_base("M107-E2", date(2060, 1, 22), &asset, &base, "60").await;

        let report = fx_balance(&report_query(PERIOD, None)).await.expect("报表应成功");
        assert_eq!(report.rows.len(), 4, "两个科目 × 两个币种");
        let usd = report
            .rows
            .iter()
            .find(|row| row.account_code == ACC_ASSET && row.currency_code == "USD")
            .expect("USD 行");
        assert_eq!(usd.book_basis, BASIS_CURRENCY, "多币种科目退到该币种账面");
        assert_eq!(
            usd.closing_book,
            dec("2120"),
            "该币种账面不含那笔 60 的本位币分录（没有币种归属，无法分摊）"
        );
        let eur = report
            .rows
            .iter()
            .find(|row| row.account_code == ACC_ASSET && row.currency_code == "EUR")
            .expect("EUR 行");
        assert_eq!(eur.closing_fc, dec("100"));
        assert_eq!(eur.closing_book, dec("810"));
        assert_eq!(eur.close_rate, Some(dec("8.1")));
        assert!(
            report
                .warnings
                .iter()
                .any(|warn| warn.contains(ACC_ASSET) && warn.contains("外币币种")),
            "多币种退化必须在 warnings 里点名科目：{:?}",
            report.warnings
        );
        assert_eq!(report.currencies.len(), 2, "USD / EUR 各一段");
    }

    /// 真库：缺期末牌价 ⇒ 折算列留空、合计留空、hints 点名，但**报表照常打开**。
    #[tokio::test]
    async fn fx_balance_leaves_conversion_empty_when_rate_missing() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (asset, counter, _base) = seed_main_scenario().await;
        // EUR 只录 12 月的牌价，**并把区间在 12-31 收口**（`effective_to` 若留 NULL，
        // V0407 的口径认为它一直有效到未来 ⇒ 期末 2060-01-31 取得到 8.0，缺牌价用例就测不到）。
        seed_rate("EUR", "2059-12-31", "8.0").await;
        db().exec_sql(
            "UPDATE biz_exchange_rate SET effective_to = '2059-12-31'::text::date \
              WHERE from_currency = 'EUR' AND to_currency = 'CNY' AND rate_type = 'SPOT'"
                .to_string(),
            vec![],
        )
        .await
        .expect("收口 EUR 牌价区间");
        post_fx(
            "M107-N1",
            date(2060, 1, 15),
            "8.0",
            &asset,
            &counter,
            "100",
            "800",
            "EUR",
            voucher::STATUS_POSTED,
        )
        .await;

        let report = fx_balance(&report_query(PERIOD, None)).await.expect("缺牌价不该 400");
        let eur = report
            .rows
            .iter()
            .find(|row| row.currency_code == "EUR")
            .expect("EUR 行仍要出（余额是事实）");
        assert!(eur.rate_missing);
        assert_eq!(eur.close_rate, None);
        assert_eq!(eur.converted_base, None, "绝不按 rate = 1 折算");
        assert_eq!(eur.reval_diff, None);
        assert_eq!(eur.closing_book, dec("800"), "账面列照旧");
        assert_eq!(report.missing_rates, vec!["EUR".to_string()]);
        assert_eq!(report.total_converted_base, None, "半套折算的合计不给");
        assert_eq!(report.total_reval_diff, None);
        assert!(
            report
                .hints
                .iter()
                .any(|hint| hint.contains("EUR") && hint.contains("汇率 1")),
            "hints 要说明没有按 1 折算：{:?}",
            report.hints
        );
        // 有牌价的 USD 行不受影响
        let usd = report
            .rows
            .iter()
            .find(|row| row.currency_code == "USD" && row.account_code == ACC_ASSET)
            .expect("USD 行");
        assert_eq!(usd.converted_base, Some(dec("2370")));
    }

    /// 真库：外币分录落在未标记科目上 ⇒ 静默漏数变成 warnings 点名。
    #[tokio::test]
    async fn fx_balance_warns_about_foreign_entries_on_unmarked_accounts() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (_asset, _counter, _base) = seed_main_scenario().await;
        // 历史数据常见形态：科目标记是后补的 ⇒ 撤掉对方科目的标记，它的外币分录就落在「未标记」科目上
        db().exec_sql(
            "UPDATE biz_account SET is_foreign_currency = 0 WHERE account_code = ?".to_string(),
            vec![Value::Text(ACC_COUNTER.to_string())],
        )
        .await
        .expect("撤标记");

        let report = fx_balance(&report_query(PERIOD, None)).await.expect("报表应成功");
        assert!(
            report.rows.iter().all(|row| row.account_code != ACC_COUNTER),
            "未标记科目不进本表（按 is_foreign_currency 取数）"
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warn| warn.contains("未标记") && warn.contains("不在报表里")),
            "必须点名漏了多少条：{:?}",
            report.warnings
        );
    }

    /// 真库：币种过滤 / 本位币过滤 / 非法币种码 / 期间未建账。
    #[tokio::test]
    async fn fx_balance_filters_and_period_fallback() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_main_scenario().await;

        // 小写 / 带空格也能筛（normalize_currency 兜底）
        let usd_only = fx_balance(&report_query(PERIOD, Some(" usd "))).await.expect("筛币种");
        assert_eq!(usd_only.rows.len(), 2);
        assert!(usd_only.rows.iter().all(|row| row.currency_code == "USD"));

        // 筛本位币：不是错误，但必须说清为什么是空的
        let base_only = fx_balance(&report_query(PERIOD, Some("CNY"))).await.expect("筛本位币");
        assert!(base_only.rows.is_empty());
        assert!(
            base_only.warnings.iter().any(|warn| warn.contains("本位币")),
            "{:?}",
            base_only.warnings
        );

        // 非法币种码：400 + 10001（不静默忽略过滤条件）
        let error = fx_balance(&report_query(PERIOD, Some("US")))
            .await
            .expect_err("非法币种码必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        // period 缺失 / 非法月份：400 + 10001
        for bad in [None, Some(""), Some("2060-13"), Some("2060-1")] {
            let error = fx_balance(&FxBalanceQuery {
                period: bad.map(str::to_string),
                currency: None,
            })
            .await
            .expect_err("期间非法必须 400");
            assert_eq!(error.status, 400, "period = {bad:?}");
            assert_eq!(error.code, error_code::PARAM_INVALID);
        }

        // 未建账的月份：按自然月推算 + period_status = null + hints 说明。
        // 注意行**不是空的**：余额是「截至该月末」的累计，2060-01 的仓位会结转到 2060-03
        // （这也正是本表能查未建账月份的意义 —— 月末关账前先看余额）。
        let not_opened = fx_balance(&report_query("2060-03", None))
            .await
            .expect("未建账也能出表");
        assert_eq!(not_opened.period_status, None);
        assert_eq!(not_opened.period_start, date(2060, 3, 1));
        assert_eq!(not_opened.period_end, date(2060, 3, 31));
        let carried = not_opened
            .rows
            .iter()
            .find(|row| row.account_code == ACC_ASSET)
            .expect("1 月的仓位要结转到 3 月");
        assert_eq!(carried.closing_fc, dec("300"), "2060-03 无发生额 ⇒ 期末原币沿用 1 月");
        assert_eq!(carried.period_fc_debit, Decimal::ZERO);
        assert!(
            not_opened.hints.iter().any(|hint| hint.contains("未建账")),
            "{:?}",
            not_opened.hints
        );
    }

    /// 收尾清场（`zz_` 排在本模块最后跑）：`M107-` / `2059-` `2060-` 夹具必须删干净。
    ///
    /// 本套件的两个外币科目会被**别人的**报表看见：`fx_ledger` 列的是「该币种全部外币核算
    /// 科目」、`cash_flow::r3_real_config_covers_all_accounts` 更是**全表口径**。`setup()` 的清场
    /// 只管「本套件的下一条用例」，最后一条跑完就漏给了下一轮（卡 t_9c3551b0 实测：留存下来的
    /// `M107FX01` 把 `fx_ledger` 的科目数撑成 3）。
    #[tokio::test]
    async fn zz_teardown_purges_fixtures() {
        let Some(_guard) = setup().await else {
            return;
        };
        purge_fixture_rows().await;
        let left: i64 = db()
            .scalar_sql(
                "SELECT (SELECT count(*) FROM biz_voucher WHERE source_no LIKE 'M107%') \
                      + (SELECT count(*) FROM biz_account WHERE account_code LIKE 'M107%') \
                      + (SELECT count(*) FROM biz_accounting_period \
                          WHERE period_code LIKE '2059-%' OR period_code LIKE '2060-%')"
                    .to_string(),
                vec![],
            )
            .await
            .expect("清场核对");
        assert_eq!(left, 0, "M107- / 2059- / 2060- 夹具残留必须为 0");
        println!("[M1-07] 清场：M107- 残留 0 ✓");
    }
}

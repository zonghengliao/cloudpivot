//! 外币明细账（M1-08 · 多币种收口）：按**币种**列示已过账分录的明细流水 + **逐行原币余额**。只读。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/fx-ledger` | `business:erp:report:fx-ledger` | 外币明细账（`currency` 必填；`period_from` / `period_to` / `account_code` 可选） |
//!
//! # 会计口径（唯一真相，改这里必须同步卡内汇报与前端 `fx-ledger.ts` 的类型注释）
//!
//! ```text
//! 范围：biz_account.is_foreign_currency = 1 且未删除的科目（V0408 · M1-03）
//!       × 已过账（status='POSTED'）且未软删、**凭证币种 = 请求币种** 的分录
//!       × 可选日期区间 [period_from, period_to]（缺省 = 全部历史）
//!
//! 原币（fc）＝ COALESCE(fc_debit_amount, debit_amount)  ← 与外币余额表（M1-07）逐字同式
//!   某科目期初 = Σ(fc 借 − fc 贷)   for voucher_date <  区间首日
//!   本期借 / 本期贷 = Σ fc 借 / Σ fc 贷  for voucher_date >= 区间首日
//!   期末 = 期初 + 本期借 − 本期贷     （「借方为正」的有符号口径，负数 = 贷方）
//!   逐行余额 = 该科目期初 + 按流水顺序累计到本行的净额（**按科目分别累计**）
//! ```
//!
//! # 五条不能改的口径（每条写错都会让账静默失真）
//!
//! ## ① 币种是**必填**的，且不接受本位币
//!
//! 「外币明细账」按定义只对**非本位币**币种成立：本位币分录没有原币口径（`fc_*` 为 NULL），
//! 查本位币应当用总账的「科目流水」（`ledger::ledger_page`，含逐行本位币余额）。所以
//! `currency` 缺失 / 不是三位大写 / 等于账套本位币 ⇒ `400 + 10001`，报文里点名去哪个页面查。
//! **不**回落成「查全部币种」：那样用户以为筛了 USD 却看到混合流水，比报错严重。
//!
//! 这与外币余额表（M1-07）的 `currency` **可选过滤**口径不同（那里筛本位币得到空表 + warning），
//! 因为那张表的主维度是 `(科目, 币种)`、本位币只是其中一个筛选项；本表的币种是**报表本身**。
//!
//! ## ② 原币只认「凭证币种 = 请求币种」的分录，本位币分录不进本表
//!
//! 期末调汇（M1-05）落的是**本位币**凭证、恰好记在外币科目上、金额是上期调汇差额。
//! 若把它并进外币明细账，用户会在 USD 的账上看到一串「没有原币金额、只有本位币金额」的行，
//! 逐行原币余额却不变 —— 这正是「原币被自己的调汇额污染」的经典错账。取数按**凭证币种**
//! 分流（`v.currency_code = ?`），与 M1-04 / M1-05 / M1-07 同一套判断。
//!
//! **代价要写清**：本表的「本位币」三列只累计**该币种原币行**的本位币金额，**不含**历次调汇；
//! 需要「含调汇的账面余额」去看外币余额表的 `opening_book / closing_book` 列。两处都不猜。
//!
//! ## ③ 逐行余额必须**按科目分别累计**
//!
//! 明细账的余额是「某科目的余额」，不是「某币种的余额」。按一份全局游标累加会让 A 科目的
//! 余额串到 B 科目头上（同一币种多科目时余额列全错且看不出来）。所以：
//!
//! ```text
//! 每行 running = 该科目期初 + Σ(该科目在全局流水顺序里本行及之前的净额)
//! ```
//!
//! 翻页时用**两次查询**实现（与 `ledger` 同一套做法，理由见那一处模块文档）：分页取本页行，
//! 再按同一 `ORDER BY` 取「本页之前各行按科目的净额合计」（`LIMIT offset` 的子查询），
//! 在 Rust 里用 `Decimal` 精确累加。排序键与两处**逐字一致**：
//! `a.account_code, v.voucher_date, v.voucher_word, v.voucher_no, e.line_no`。
//!
//! ## ④ 数据质量：外币分录落在未标记科目 / 原币金额缺失 —— 只提示不拦
//!
//! 本表按 `is_foreign_currency = 1` 取数（与外币余额表同口径，两张表才能对拍）。落在
//! **未标记**科目上的外币分录、以及 `fc_*` 为 NULL 的（历史脏数据）分录不会被静默忽略：
//! 前者计数进 `warnings` 并提醒去补科目标记，后者计数进 `warnings` 并说明「原币按本位币金额顶替」。
//! 报表不该因为数据脏就打不开，但必须让人看见脏在哪。
//!
//! ## ⑤ 与外币余额表**同口径同数字**（可对拍，已写成真库用例）
//!
//! 本表某 `(科目, 币种)` 的 `closing_fc` 必须等于外币余额表同期间同 `(科目, 币种)` 的
//! `closing_fc` —— 两张表用的是同一条 `WHERE` 与同一个 `COALESCE` 表达式，数字对不上就是
//! 有人改了其中一边（`fx_ledger_matches_fx_balance_closing` 用例盯着这件事）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_voucher_entry / biz_account 是会计台账与全局主数据
//! （V0149/V0137 无 dept_id / 归属人列），数据范围表达不出来；可见性由
//! business:erp:report:fx-ledger 权限码控制，分页走 scope::paginate_unscoped。
//! ```
//!
//! # 为什么不复用 `ledger::ledger_page`
//!
//! `ledger_page` 是「一个科目的本位币流水」：`account_id` 必填、金额只有本位币、无币种概念。
//! 本表是「一个币种跨科目的原币流水」：币种必填、科目可选、金额是原币 + 本位币双列、
//! 余额按科目分别累计。把两种语义塞进一个函数，等于让两个报表互相污染对方的 `WHERE`
//! —— 外币明细账是**新增的派生视图**，不是总账的一个筛选开关。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::business::erp::currency;
use crate::business::erp::ledger::parse_boundary;
use crate::business::erp::model::normalize_page;
use cloudpivot_kernel::business::shared::scope;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 报表编码（前端与导出文件名的稳定标识，不随文案变）。
pub const REPORT_CODE: &str = "FX_LEDGER";

/// 报表名称。
pub const REPORT_NAME: &str = "外币明细账";

/// 只统计已过账凭证（与 `ledger` / `fx_balance` / `period_fx_reval` 逐字一致）。
pub const POSTED: &str = "POSTED";

/// 科目表。
const ACCOUNT_TABLE: &str = "biz_account";

/// 凭证表。
const VOUCHER_TABLE: &str = "biz_voucher";

/// 分录表。
const ENTRY_TABLE: &str = "biz_voucher_entry";

/// 币种表。
const CURRENCY_TABLE: &str = "biz_currency";

/// 流水排序键：**必须**与前缀和子查询、`load_rows` 两处逐字一致（否则翻页余额会错）。
pub const LEDGER_ORDER: &str = "ORDER BY a.account_code ASC, v.voucher_date ASC, v.voucher_word ASC, \
     v.voucher_no ASC, e.line_no ASC";

/// 流水分页目标（`unscoped`：台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LEDGER_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(ENTRY_TABLE);

// ==================== 入参 ====================

/// 外币明细账查询（Query）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FxLedgerQuery {
    /// 币种（**必填**，三位大写；等于账套本位币 ⇒ 400）。
    pub currency: Option<String>,
    /// 科目 id（与 `account_code` 二选一；都不给 = 该币种全部外币核算科目）。
    pub account_id: Option<String>,
    /// 科目编码（与 `account_id` 二选一）。
    pub account_code: Option<String>,
    /// 起始期间 / 日期：`yyyy-MM`（取月首）或 `yyyy-MM-dd`（缺省 = 不限下限）。
    pub period_from: Option<String>,
    /// 结束期间 / 日期：`yyyy-MM`（取月末）或 `yyyy-MM-dd`（缺省 = 不限上限）。
    pub period_to: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 流水筛选（数据层形态）。
#[derive(Debug, Clone)]
pub struct FxLedgerFilter {
    /// 币种（已规范化，且保证 ≠ 账套本位币）。
    pub currency: String,
    /// 科目 id（已解析；`None` = 不按科目过滤）。
    pub account_id: Option<Uuid>,
    /// 起始日期（含）。
    pub date_from: Option<NaiveDate>,
    /// 结束日期（含）。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

// ==================== 出参 ====================

/// 科目过滤回显（请求里给了科目时才有）。
#[derive(Debug, Clone, Serialize)]
pub struct AccountEcho {
    /// 主键。
    pub id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 是否已标记「外币核算」（`false` 时本表必然为空，`warnings` 里会点名）。
    pub is_foreign_currency: bool,
}

/// 明细行（一条已过账分录）。
///
/// 所有金额都是**十进制字符串**（`NUMERIC(20,4)` 序列化后是字符串，前端不要过 `Number()`）。
/// 余额列一律「借方为正」（`借 − 贷` 的有符号口径），负数 = 贷方余额。
#[derive(Debug, Clone, Serialize)]
pub struct FxLedgerRow {
    /// 凭证 id。
    pub voucher_id: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 分录行号。
    pub line_no: i32,
    /// 摘要（分录摘要）。
    pub summary: String,
    /// 科目 id。
    pub account_id: String,
    /// 科目编码（分录快照）。
    pub account_code: String,
    /// 科目名称（分录快照）。
    pub account_name: String,
    /// 凭证币种（永远等于请求币种；回显用）。
    pub currency_code: String,
    /// 建单时锁定的汇率（`biz_voucher.exchange_rate`）。
    pub exchange_rate: Decimal,
    /// 原币借方金额。
    pub fc_debit: Decimal,
    /// 原币贷方金额。
    pub fc_credit: Decimal,
    /// 本位币借方金额（该币种行的本位币金额，**不含**调汇）。
    pub debit_amount: Decimal,
    /// 本位币贷方金额。
    pub credit_amount: Decimal,
    /// 逐行原币余额（**含**该科目期初，按科目分别累计；借方为正）。
    pub running_fc_balance: Decimal,
    /// 余额方向（`DEBIT` / `CREDIT` / `ZERO`）。
    pub running_side: &'static str,
    /// 是否红字凭证（红字行净额为负，逐行余额直接累加，无需特判）。
    pub is_red: bool,
}

/// 按科目汇总（每个有该币种流水的科目一行，与外币余额表可按 `closing_fc` 对拍）。
#[derive(Debug, Clone, Serialize)]
pub struct FxLedgerAccount {
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 余额方向（科目主数据，不参与计算）。
    pub balance_direction: String,
    /// 科目类别。
    pub account_category: String,
    /// 本区间内该币种的分录条数。
    pub entry_count: i64,
    /// 期初原币余额（借方为正）。
    pub opening_fc: Decimal,
    /// 本期原币借方发生额。
    pub period_fc_debit: Decimal,
    /// 本期原币贷方发生额。
    pub period_fc_credit: Decimal,
    /// 期末原币余额（= 期初 + 本期借 − 本期贷）。
    pub closing_fc: Decimal,
    /// 期末余额方向（`DEBIT` / `CREDIT` / `ZERO`）。
    pub closing_side: &'static str,
    /// 本区间内该币种分录的**本位币**借方合计（不含调汇）。
    pub period_book_debit: Decimal,
    /// 本区间内该币种分录的**本位币**贷方合计（不含调汇）。
    pub period_book_credit: Decimal,
    /// 本区间内该币种分录的本位币净额（借 − 贷）。
    pub period_book_net: Decimal,
}

/// 外币明细账结果。
#[derive(Debug, Clone, Serialize)]
pub struct FxLedgerRes {
    /// 报表编码。
    pub report_code: String,
    /// 报表名称。
    pub report_name: String,
    /// 编制单位。
    pub company_name: String,
    /// 账套本位币（如 `CNY`）。
    pub base_currency: String,
    /// 请求的币种（已规范化）。
    pub currency: String,
    /// 币种名称（主数据缺失为 `null`，不影响数字）。
    pub currency_name: Option<String>,
    /// 起始日期（含；未给 = 不限下限）。
    pub date_from: Option<NaiveDate>,
    /// 结束日期（含；未给 = 不限上限）。
    pub date_to: Option<NaiveDate>,
    /// 科目过滤回显（未过滤 = `null`）。
    pub account: Option<AccountEcho>,
    /// 期初原币合计（Σ 各科目期初，借方为正）。
    pub opening_fc: Decimal,
    /// 本期原币借方合计。
    pub period_fc_debit: Decimal,
    /// 本期原币贷方合计。
    pub period_fc_credit: Decimal,
    /// 期末原币合计（该币种的净头寸）。
    pub closing_fc: Decimal,
    /// 期末合计方向（`DEBIT` / `CREDIT` / `ZERO`）。
    pub closing_side: &'static str,
    /// 本区间内该币种分录的本位币借方合计（不含调汇）。
    pub period_book_debit: Decimal,
    /// 本区间内该币种分录的本位币贷方合计（不含调汇）。
    pub period_book_credit: Decimal,
    /// 本区间内该币种分录的本位币净额（借 − 贷）。
    pub period_book_net: Decimal,
    /// 本区间内该币种的分录条数（全量，跨页；= `rows` 的总数）。
    pub entry_count: i64,
    /// 按科目汇总（按科目编码升序）。
    pub accounts: Vec<FxLedgerAccount>,
    /// 本页明细行（按科目编码、凭证日期、凭证字号、行号升序）。
    pub rows: Vec<FxLedgerRow>,
    /// 明细总条数（分页用）。
    pub total: i64,
    /// 当前页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
    /// 需要人工处理的问题（外币分录落在未标记科目 / 原币金额缺失 / 筛了未标记科目）。
    pub warnings: Vec<String>,
    /// 口径提示。
    pub hints: Vec<String>,
}

// ==================== 纯逻辑 ====================

/// 某行净额（借方为正）。
pub fn net_of(fc_debit: Decimal, fc_credit: Decimal) -> Decimal {
    fc_debit - fc_credit
}

/// 有符号余额 → 借 / 贷 / 平（与 `fx_balance::side_of` 同口径）。
pub fn side_of(balance: Decimal) -> &'static str {
    if balance > Decimal::ZERO {
        "DEBIT"
    } else if balance < Decimal::ZERO {
        "CREDIT"
    } else {
        "ZERO"
    }
}

/// 期初 / 前缀和 → 每个科目的起始余额（`openings + prefixes`，缺项按 0）。
pub fn start_balances(
    openings: &BTreeMap<Uuid, Decimal>,
    prefixes: &BTreeMap<Uuid, Decimal>,
) -> BTreeMap<Uuid, Decimal> {
    let mut out = openings.clone();
    for (account_id, net) in prefixes {
        *out.entry(*account_id).or_insert(Decimal::ZERO) += *net;
    }
    out
}

// ==================== 库访问 ====================

/// 明细 / 期初 / 汇总共用的 `FROM + WHERE`（**参数顺序是契约**：所有查询都从这里取）。
///
/// 日期以字面量内联（值来自 `NaiveDate`，不可能注入）；只绑定文本参数：**币种** 与可选的
/// **科目 id**。`v.status = 'POSTED'` 是常量字符串（与 `ledger` 同做法）。
fn from_where(
    filter: &FxLedgerFilter,
    date_from: Option<NaiveDate>,
    date_to: Option<NaiveDate>,
) -> (String, Vec<Value>) {
    let mut sql = format!(
        "FROM {ENTRY_TABLE} e \
         JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
         JOIN {ACCOUNT_TABLE} a ON a.id = e.account_id \
         WHERE a.is_foreign_currency = 1 AND a.deleted_at IS NULL \
           AND v.status = '{POSTED}' AND v.deleted_at IS NULL \
           AND v.currency_code = ?"
    );
    let mut params = vec![Value::Text(filter.currency.clone())];
    if let Some(account_id) = filter.account_id {
        sql.push_str(" AND a.id = ?::uuid");
        params.push(Value::from(account_id));
    }
    if let Some(day) = date_from {
        sql.push_str(&format!(" AND v.voucher_date >= '{day}'::text::date"));
    }
    if let Some(day) = date_to {
        sql.push_str(&format!(" AND v.voucher_date <= '{day}'::text::date"));
    }
    (sql, params)
}

/// 本页之前的净额合计（`offset` 行），**按科目分组**。
///
/// `LIMIT` 是 `u64` 运算结果的字面量（无注入面），与 `scope::paginate` 的取向一致。
async fn prefix_net(filter: &FxLedgerFilter, offset: u64) -> DbResult<BTreeMap<Uuid, Decimal>> {
    let (from_where, params) = from_where(filter, filter.date_from, filter.date_to);
    let sql = format!(
        "SELECT t.account_id, COALESCE(SUM(t.net), 0)::numeric(20, 4) AS net FROM (\
           SELECT e.account_id AS account_id, \
                  (COALESCE(e.fc_debit_amount, e.debit_amount) \
                   - COALESCE(e.fc_credit_amount, e.credit_amount)) AS net \
           {from_where} {LEDGER_ORDER} LIMIT {offset}\
         ) t GROUP BY t.account_id"
    );
    let rows = db().query_sql(sql, params).await?;
    let mut out = BTreeMap::new();
    for row in rows.iter() {
        out.insert(row.get_typed::<Uuid>("account_id")?, row.get_typed::<Decimal>("net")?);
    }
    Ok(out)
}

/// 明细行的中间形态（`account_id` 保留 `Uuid`，便于逐行余额按科目累计）。
struct FxRowRaw {
    voucher_id: Uuid,
    voucher_date: NaiveDate,
    voucher_word: String,
    voucher_no: String,
    line_no: i32,
    summary: String,
    account_id: Uuid,
    account_code: String,
    account_name: String,
    exchange_rate: Decimal,
    fc_debit: Decimal,
    fc_credit: Decimal,
    debit_amount: Decimal,
    credit_amount: Decimal,
    /// `biz_voucher.is_red` 是 `SMALLINT`（1 = 红字），到这里才转 `bool`
    /// —— 直接 `get_typed::<bool>` 会 `type mismatch: expected Bool, got I64`（500）。
    is_red: i16,
    running_fc_balance: Decimal,
}

impl FxRowRaw {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            voucher_id: row.get_typed("voucher_id")?,
            voucher_date: row.get_typed("voucher_date")?,
            voucher_word: row.get_typed("voucher_word")?,
            voucher_no: row.get_typed("voucher_no")?,
            line_no: row.get_typed("line_no")?,
            summary: row.get_typed("summary")?,
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            exchange_rate: row.get_typed("exchange_rate")?,
            fc_debit: row.get_typed("fc_debit")?,
            fc_credit: row.get_typed("fc_credit")?,
            debit_amount: row.get_typed("debit_amount")?,
            credit_amount: row.get_typed("credit_amount")?,
            is_red: row.get_typed("is_red")?,
            running_fc_balance: Decimal::ZERO,
        })
    }

    fn net(&self) -> Decimal {
        net_of(self.fc_debit, self.fc_credit)
    }

    fn into_res(self, currency: &str) -> FxLedgerRow {
        FxLedgerRow {
            voucher_id: self.voucher_id.to_string(),
            voucher_date: self.voucher_date,
            voucher_word: self.voucher_word,
            voucher_no: self.voucher_no,
            line_no: self.line_no,
            summary: self.summary,
            account_id: self.account_id.to_string(),
            account_code: self.account_code,
            account_name: self.account_name,
            currency_code: currency.to_string(),
            exchange_rate: self.exchange_rate,
            fc_debit: self.fc_debit,
            fc_credit: self.fc_credit,
            debit_amount: self.debit_amount,
            credit_amount: self.credit_amount,
            running_fc_balance: self.running_fc_balance,
            running_side: side_of(self.running_fc_balance),
            is_red: self.is_red == 1,
        }
    }
}

/// 一页明细 + 总数（逐行余额已按科目累计，含期初）。
async fn load_rows(filter: &FxLedgerFilter, openings: &BTreeMap<Uuid, Decimal>) -> DbResult<(Vec<FxLedgerRow>, i64)> {
    let (from_where, params) = from_where(filter, filter.date_from, filter.date_to);
    let base_sql = format!(
        "SELECT v.id AS voucher_id, v.voucher_date, v.voucher_word, v.voucher_no, e.line_no, \
                e.summary, a.id AS account_id, a.account_code, a.account_name, v.exchange_rate, \
                COALESCE(e.fc_debit_amount, e.debit_amount) AS fc_debit, \
                COALESCE(e.fc_credit_amount, e.credit_amount) AS fc_credit, \
                e.debit_amount, e.credit_amount, v.is_red \
         {from_where}"
    );
    let result = scope::paginate_unscoped(
        LEDGER_TARGET,
        &base_sql,
        params,
        Some(LEDGER_ORDER),
        filter.page,
        filter.page_size,
    )
    .await?;
    let mut rows = result
        .list
        .iter()
        .map(FxRowRaw::from_row)
        .collect::<DbResult<Vec<FxRowRaw>>>()?;
    let total = i64::try_from(result.total).unwrap_or(i64::MAX);

    // 逐行余额 = 该科目(期初 + 本页之前各行净额) + 按流水顺序累计到本行的净额
    let offset = (filter.page.saturating_sub(1)) * filter.page_size;
    let prefixes = if offset == 0 {
        BTreeMap::new()
    } else {
        prefix_net(filter, offset).await?
    };
    let mut running = start_balances(openings, &prefixes);
    for row in &mut rows {
        let balance = running.entry(row.account_id).or_insert(Decimal::ZERO);
        *balance += row.net();
        row.running_fc_balance = *balance;
    }

    Ok((
        rows.into_iter().map(|row| row.into_res(&filter.currency)).collect(),
        total,
    ))
}

/// 按科目汇总的 SQL（期初用 `FILTER` 切「区间首日之前」，覆盖全区间则需要看到期初行）。
fn account_sql(filter: &FxLedgerFilter) -> String {
    let (from_where, _) = from_where(filter, None, filter.date_to);
    let in_range = match filter.date_from {
        Some(day) => format!("v.voucher_date >= '{day}'::text::date"),
        None => "TRUE".to_string(),
    };
    let opening = match filter.date_from {
        Some(day) => format!(
            "COALESCE(SUM(COALESCE(e.fc_debit_amount, e.debit_amount) \
              - COALESCE(e.fc_credit_amount, e.credit_amount)) \
              FILTER (WHERE v.voucher_date < '{day}'::text::date), 0)::numeric(20, 4)"
        ),
        None => "0::numeric(20, 4)".to_string(),
    };
    format!(
        "SELECT a.id AS account_id, a.account_code, a.account_name, a.balance_direction, \
                a.account_category, \
                COUNT(*) FILTER (WHERE {in_range})::bigint AS entry_count, \
                {opening} AS opening_fc, \
                COALESCE(SUM(COALESCE(e.fc_debit_amount, e.debit_amount)) \
                    FILTER (WHERE {in_range}), 0)::numeric(20, 4) AS period_fc_debit, \
                COALESCE(SUM(COALESCE(e.fc_credit_amount, e.credit_amount)) \
                    FILTER (WHERE {in_range}), 0)::numeric(20, 4) AS period_fc_credit, \
                COALESCE(SUM(e.debit_amount) FILTER (WHERE {in_range}), 0)::numeric(20, 4) \
                    AS period_book_debit, \
                COALESCE(SUM(e.credit_amount) FILTER (WHERE {in_range}), 0)::numeric(20, 4) \
                    AS period_book_credit \
           {from_where} \
          GROUP BY a.id, a.account_code, a.account_name, a.balance_direction, a.account_category \
          ORDER BY a.account_code"
    )
}

/// 每个科目的期初 / 本期 / 期末（原币）+ 本位币发生额。
async fn load_accounts(filter: &FxLedgerFilter) -> DbResult<Vec<FxLedgerAccount>> {
    let (_, params) = from_where(filter, None, filter.date_to);
    let rows = db().query_sql(account_sql(filter), params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let opening_fc: Decimal = row.get_typed("opening_fc")?;
        let period_fc_debit: Decimal = row.get_typed("period_fc_debit")?;
        let period_fc_credit: Decimal = row.get_typed("period_fc_credit")?;
        let closing_fc = opening_fc + period_fc_debit - period_fc_credit;
        let period_book_debit: Decimal = row.get_typed("period_book_debit")?;
        let period_book_credit: Decimal = row.get_typed("period_book_credit")?;
        let account_id: Uuid = row.get_typed("account_id")?;
        out.push(FxLedgerAccount {
            account_id: account_id.to_string(),
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            balance_direction: row.get_typed("balance_direction")?,
            account_category: row.get_typed("account_category")?,
            entry_count: row.get_typed("entry_count")?,
            opening_fc,
            period_fc_debit,
            period_fc_credit,
            closing_fc,
            closing_side: side_of(closing_fc),
            period_book_debit,
            period_book_credit,
            period_book_net: period_book_debit - period_book_credit,
        });
    }
    Ok(out)
}

/// 数据质量探针：`(外币分录落在未标记外币核算科目上的条数, 原币金额缺失的条数)`。
///
/// 两者都**不进本表**（本表按 `is_foreign_currency = 1` 取数，与外币余额表同口径），
/// 静默漏数比报错更危险 —— 所以一次查询取回两个计数，写进 `warnings`。
async fn quality_probe(filter: &FxLedgerFilter) -> DbResult<(i64, i64)> {
    let mut sql = format!(
        "SELECT COUNT(*) FILTER (WHERE a.is_foreign_currency = 0)::bigint AS unmarked, \
                COUNT(*) FILTER (WHERE e.fc_debit_amount IS NULL OR e.fc_credit_amount IS NULL) \
                    ::bigint AS fc_null \
           FROM {ENTRY_TABLE} e \
           JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
           JOIN {ACCOUNT_TABLE} a ON a.id = e.account_id \
          WHERE a.deleted_at IS NULL AND v.status = '{POSTED}' AND v.deleted_at IS NULL \
            AND v.currency_code = ?"
    );
    let mut params = vec![Value::Text(filter.currency.clone())];
    if let Some(account_id) = filter.account_id {
        sql.push_str(" AND a.id = ?::uuid");
        params.push(Value::from(account_id));
    }
    if let Some(day) = filter.date_from {
        sql.push_str(&format!(" AND v.voucher_date >= '{day}'::text::date"));
    }
    if let Some(day) = filter.date_to {
        sql.push_str(&format!(" AND v.voucher_date <= '{day}'::text::date"));
    }
    let row = db().query_first_sql(sql, params).await?;
    match row {
        Some(row) => Ok((row.get_typed("unmarked")?, row.get_typed("fc_null")?)),
        None => Ok((0, 0)),
    }
}

/// 币种名称（`biz_currency` 里没有该码时 `None`，不影响数字）。
async fn currency_name(code: &str) -> DbResult<Option<String>> {
    let row = db()
        .query_first_sql(
            format!("SELECT name FROM {CURRENCY_TABLE} WHERE code = ?"),
            vec![Value::Text(code.to_string())],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(row.get_typed("name")?)),
        None => Ok(None),
    }
}

/// 科目过滤解析：都不给 = `None`；给了但查不到 = 404；id 不是 UUID = 400。
async fn resolve_account(
    account_id: Option<&str>,
    account_code: Option<&str>,
) -> std::result::Result<Option<AccountEcho>, ApiError> {
    let id_text = account_id.map(str::trim).filter(|text| !text.is_empty());
    let code_text = account_code.map(str::trim).filter(|text| !text.is_empty());
    let (where_sql, params) = match (id_text, code_text) {
        (None, None) => return Ok(None),
        (Some(raw), _) => {
            let id = Uuid::parse_str(raw).map_err(|_| {
                ApiError::new(
                    400,
                    error_code::ERP_FILTER_INVALID,
                    format!("account_id 不是合法的 UUID：{raw}"),
                )
            })?;
            ("id = ?::uuid", vec![Value::from(id)])
        }
        (None, Some(code)) => ("account_code = ?", vec![Value::from(code)]),
    };
    let row = db()
        .query_first_sql(
            format!(
                "SELECT id, account_code, account_name, is_foreign_currency FROM {ACCOUNT_TABLE} \
                 WHERE deleted_at IS NULL AND {where_sql}"
            ),
            params,
        )
        .await
        .map_err(|error| utils::db_error(error, "查询会计科目"))?;
    let Some(row) = row else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "会计科目不存在或已删除".to_string(),
        ));
    };
    Ok(Some(AccountEcho {
        id: row
            .get_typed::<Uuid>("id")
            .map_err(|error| utils::db_error(error, "查询会计科目"))?
            .to_string(),
        account_code: row
            .get_typed("account_code")
            .map_err(|error| utils::db_error(error, "查询会计科目"))?,
        account_name: row
            .get_typed("account_name")
            .map_err(|error| utils::db_error(error, "查询会计科目"))?,
        is_foreign_currency: row
            .get_typed::<i16>("is_foreign_currency")
            .map_err(|error| utils::db_error(error, "查询会计科目"))?
            == 1,
    }))
}

// ==================== 主流程 ====================

/// 外币明细账（`currency` 必填）。
///
/// # 错误
///
/// * `currency` 缺失 / 不是三位大写 / 等于账套本位币 ⇒ `400 + 10001`；
/// * `period_from` / `period_to` 不是 `yyyy-MM` 或 `yyyy-MM-dd`、或起大于止 ⇒ `400 + 10001`；
/// * `account_id` 不是 UUID ⇒ `400 + 101xx`（`ERP_FILTER_INVALID`）；查不到科目 ⇒ `404 + 10104`；
/// * 数据脏（未标记科目 / 原币缺失）**不报错**，进 `warnings`。
pub async fn fx_ledger(query: &FxLedgerQuery) -> std::result::Result<FxLedgerRes, ApiError> {
    let raw_currency = query
        .currency
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("currency 必填（三位大写币种码，例：USD）".to_string()))?;
    let currency_code = currency::normalize_currency(raw_currency).ok_or_else(|| {
        bad_request(format!(
            "currency 必须是三位大写币种码（如 USD），收到「{raw_currency}」"
        ))
    })?;

    let base_currency = currency::base_currency_code(db())
        .await
        .map_err(|error| utils::db_error(error, "查询账套本位币"))?;
    if currency_code == base_currency {
        return Err(bad_request(format!(
            "{currency_code} 是账套本位币，不构成外币明细账 —— 查本位币流水请用「总账 / 科目余额表」的\
             「科目流水」（那里有逐行本位币余额）"
        )));
    }

    let account = resolve_account(query.account_id.as_deref(), query.account_code.as_deref()).await?;
    let account_id = account
        .as_ref()
        .map(|echo| Uuid::parse_str(&echo.id))
        .transpose()
        .map_err(|_| ApiError::new(500, error_code::ERP_FILTER_INVALID, "科目主键解析失败".to_string()))?;

    let date_from = parse_boundary(query.period_from.as_deref(), "period_from", false)?;
    let date_to = parse_boundary(query.period_to.as_deref(), "period_to", true)?;
    if let (Some(from), Some(to)) = (date_from, date_to)
        && from > to
    {
        return Err(bad_request(format!("period_from（{from}）不能晚于 period_to（{to}）")));
    }
    let (page, page_size) = normalize_page(query.page, query.page_size);

    let filter = FxLedgerFilter {
        currency: currency_code.clone(),
        account_id,
        date_from,
        date_to,
        page,
        page_size,
    };

    let accounts = load_accounts(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询外币科目余额"))?;
    let openings: BTreeMap<Uuid, Decimal> = accounts
        .iter()
        .filter_map(|row| Uuid::parse_str(&row.account_id).ok().map(|id| (id, row.opening_fc)))
        .collect();
    let (rows, total) = load_rows(&filter, &openings)
        .await
        .map_err(|error| utils::db_error(error, "查询外币明细流水"))?;
    let (unmarked, fc_null) = quality_probe(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询外币分录数据质量"))?;
    let name = currency_name(&currency_code)
        .await
        .map_err(|error| utils::db_error(error, "查询币种名称"))?;

    let mut opening_fc = Decimal::ZERO;
    let mut period_fc_debit = Decimal::ZERO;
    let mut period_fc_credit = Decimal::ZERO;
    let mut period_book_debit = Decimal::ZERO;
    let mut period_book_credit = Decimal::ZERO;
    for row in &accounts {
        opening_fc += row.opening_fc;
        period_fc_debit += row.period_fc_debit;
        period_fc_credit += row.period_fc_credit;
        period_book_debit += row.period_book_debit;
        period_book_credit += row.period_book_credit;
    }
    let closing_fc = opening_fc + period_fc_debit - period_fc_credit;

    let mut warnings: Vec<String> = Vec::new();
    if unmarked > 0 {
        warnings.push(format!(
            "有 {unmarked} 条 {currency_code} 凭证分录记在**未标记「外币核算」**的科目上（本表只统计\
             已标记科目）：这些分录不在本表，也不在外币余额表里 —— 请补齐科目标记或把凭证改到正确科目"
        ));
    }
    if fc_null > 0 {
        warnings.push(format!(
            "有 {fc_null} 条 {currency_code} 分录的**原币金额为空**（历史脏数据）：本表按\
             「原币 = 本位币金额」顶替计算，金额可能失真，请核对凭证"
        ));
    }
    if let Some(echo) = account.as_ref()
        && !echo.is_foreign_currency
    {
        warnings.push(format!(
            "科目 {} {} 未标记「外币核算」，不产生外币明细账（需先在科目页标记，否则任何币种都查不到它的流水）",
            echo.account_code, echo.account_name
        ));
    }

    let range_text = match (date_from, date_to) {
        (Some(from), Some(to)) => format!("{from} ~ {to}"),
        (Some(from), None) => format!("{from} 起"),
        (None, Some(to)) => format!("截至 {to}"),
        (None, None) => "全部历史".to_string(),
    };
    let mut hints: Vec<String> = vec![
        "只统计已过账（POSTED）凭证；草稿与作废凭证不进本表".to_string(),
        "余额为「借方为正」的有符号口径（借 − 贷），负数表示贷方余额 —— 资产与负债科目共用一套公式".to_string(),
        "逐行余额**按科目分别累计**，含该科目在区间首日之前的累计额（即「期初」）；跨科目的行不可直接连读".to_string(),
        format!(
            "本表只列 {currency_code} 凭证的原币行；本位币分录（含期末调汇）不在本表 —— \
             含历次调汇的本位币账面余额请看「外币余额表」"
        ),
        "同一 (科目, 币种) 的期末原币与「外币余额表」逐字同口径，可直接对拍".to_string(),
        format!("区间：{range_text}（缺省 = 不限）"),
    ];
    if total == 0 {
        hints.push(format!(
            "币种 {currency_code} 在所选区间没有已过账的外币分录（这不是出错；换个区间或先确认凭证已记账）"
        ));
    }

    Ok(FxLedgerRes {
        report_code: REPORT_CODE.to_string(),
        report_name: REPORT_NAME.to_string(),
        company_name: crate::business::erp::report::company_name().await,
        base_currency,
        currency: currency_code,
        currency_name: name,
        date_from,
        date_to,
        account,
        opening_fc,
        period_fc_debit,
        period_fc_credit,
        closing_fc,
        closing_side: side_of(closing_fc),
        period_book_debit,
        period_book_credit,
        period_book_net: period_book_debit - period_book_credit,
        entry_count: total,
        accounts,
        rows,
        total,
        page,
        page_size,
        generated_at: Utc::now(),
        warnings,
        hints,
    })
}

/// 参数类 400（`10001`）。
fn bad_request(message: String) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message)
}

// ==================== 接口 ====================

/// 外币明细账。
///
/// ```text
/// GET /business/erp/report/fx-ledger?currency=USD
/// GET /business/erp/report/fx-ledger?currency=USD&period_from=2026-11&period_to=2026-11
/// GET /business/erp/report/fx-ledger?currency=USD&account_code=1001&period_from=2026-11-01
/// ```
#[nik::permission(
    path = "/business/erp/report/fx-ledger",
    method = "GET",
    permission = "business:erp:report:fx-ledger"
)]
#[nik::get("/business/erp/report/fx-ledger")]
pub async fn fx_ledger_handler(Query(query): Query<FxLedgerQuery>) -> ResResult<FxLedgerRes> {
    Ok(Res::ok(fx_ledger(&query).await?))
}

// ==================== 单测 ====================
//
// 纯逻辑用例不连库；真库用例（`ERP_DB_URL` 未设则 SKIP）打通「真凭证落库 → 明细账取数」：
//
// ```bash
// ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_m108_t1' \
//   cargo test --lib business::erp::fx_ledger -- --test-threads=1
// ```
//
// 期间命名空间 **2062-xx 归本模块独占**（2059/2060 = 外币余额表、2050 = 应收重估、
// 2051 = 期末调汇、2031 = period；2026 是生产期间，绝不碰）。
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

    fn raw_row(account: Uuid, voucher_no: &str, fc_debit: &str, fc_credit: &str) -> FxRowRaw {
        FxRowRaw {
            voucher_id: Uuid::nil(),
            voucher_date: date(2062, 2, 1),
            voucher_word: "记".to_string(),
            voucher_no: voucher_no.to_string(),
            line_no: 1,
            summary: "测试".to_string(),
            account_id: account,
            account_code: "M108T".to_string(),
            account_name: "测试科目".to_string(),
            exchange_rate: dec("7.2"),
            fc_debit: dec(fc_debit),
            fc_credit: dec(fc_credit),
            debit_amount: dec(fc_debit),
            credit_amount: dec(fc_credit),
            is_red: 0,
            running_fc_balance: Decimal::ZERO,
        }
    }

    // ---------- 纯逻辑 ----------

    #[test]
    fn net_and_side_follow_signed_debit_positive() {
        assert_eq!(net_of(dec("100"), Decimal::ZERO), dec("100"));
        assert_eq!(net_of(Decimal::ZERO, dec("100")), dec("-100"));
        assert_eq!(side_of(dec("0.0001")), "DEBIT");
        assert_eq!(side_of(Decimal::ZERO), "ZERO");
        assert_eq!(side_of(dec("-0.0001")), "CREDIT");
    }

    #[test]
    fn start_balances_adds_prefix_to_opening_and_defaults_to_zero() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let openings = BTreeMap::from([(a, dec("200")), (b, dec("-200"))]);
        let prefixes = BTreeMap::from([(a, dec("100")), (c, dec("-50"))]);
        let start = start_balances(&openings, &prefixes);
        assert_eq!(start.get(&a).copied(), Some(dec("300")));
        assert_eq!(start.get(&b).copied(), Some(dec("-200")), "只有期初的科目也在表里");
        assert_eq!(start.get(&c).copied(), Some(dec("-50")), "没有期初的科目按 0 起算");
    }

    #[test]
    fn from_where_binds_currency_then_account_and_inlines_dates() {
        let account = Uuid::from_u128(9);
        let filter = FxLedgerFilter {
            currency: "USD".to_string(),
            account_id: Some(account),
            date_from: Some(date(2062, 2, 1)),
            date_to: Some(date(2062, 2, 28)),
            page: 1,
            page_size: 20,
        };
        let (sql, params) = from_where(&filter, filter.date_from, filter.date_to);
        assert_eq!(params.len(), 2, "只绑定币种与科目：{sql}");
        assert!(sql.contains("v.currency_code = ?"));
        assert!(sql.contains("a.id = ?::uuid"));
        assert!(sql.contains("v.voucher_date >= '2062-02-01'::text::date"));
        assert!(sql.contains("v.voucher_date <= '2062-02-28'::text::date"));
        assert!(sql.contains("a.is_foreign_currency = 1"));
        assert!(sql.contains("v.status = 'POSTED'"));
        assert!(!sql.contains("ORDER BY"), "base_sql 里不许出现 ORDER BY");

        let open = FxLedgerFilter {
            date_from: None,
            date_to: None,
            account_id: None,
            ..filter.clone()
        };
        let (sql_open, params_open) = from_where(&open, None, None);
        assert_eq!(params_open.len(), 1);
        assert!(!sql_open.contains("voucher_date >="));
        assert!(!sql_open.contains("voucher_date <="));
    }

    #[test]
    fn account_sql_switches_opening_expression_on_date_from() {
        let with_from = FxLedgerFilter {
            currency: "USD".to_string(),
            account_id: None,
            date_from: Some(date(2062, 2, 1)),
            date_to: None,
            page: 1,
            page_size: 20,
        };
        let sql = account_sql(&with_from);
        assert!(
            sql.contains("FILTER (WHERE v.voucher_date < '2062-02-01'::text::date)"),
            "期初必须按区间首日切片：{sql}"
        );
        assert!(sql.contains("FILTER (WHERE v.voucher_date >= '2062-02-01'::text::date)"));

        let without_from = FxLedgerFilter {
            date_from: None,
            ..with_from
        };
        let sql_all = account_sql(&without_from);
        assert!(
            sql_all.contains("0::numeric(20, 4)"),
            "不给区间下限时期初恒为 0：{sql_all}"
        );
        assert!(sql_all.contains("COUNT(*) FILTER (WHERE TRUE)"));
        assert!(!sql_all.contains("voucher_date <"));
    }

    #[test]
    fn into_res_marks_running_side_from_signed_balance() {
        let row = raw_row(Uuid::from_u128(1), "1", "0", "300");
        let mut raw = row;
        raw.running_fc_balance = dec("-300");
        let res = raw.into_res("USD");
        assert_eq!(res.running_side, "CREDIT");
        assert_eq!(res.currency_code, "USD");
        assert_eq!(res.fc_credit, dec("300"));
    }

    // ---------- 真库（ERP_DB_URL 未设则 SKIP） ----------

    /// 明细账所在期间。
    const PERIOD: &str = "2062-02";
    /// 期初所在期间（期初余额必须落在区间首日之前）。
    const PERIOD_OPENING: &str = "2062-01";
    /// 外币资产科目（借方余额）。
    const ACC_ASSET: &str = "M108FX01";
    /// 外币对方科目（贷方余额）。
    const ACC_COUNTER: &str = "M108FX02";
    /// 本位币科目（造纯本位币凭证，验证 ②）。
    const ACC_BASE: &str = "M108BASE";

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

    async fn cleanup() {
        for sql in [
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'M108%')",
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
             (SELECT id FROM biz_voucher WHERE source_no LIKE 'M108%')",
            "DELETE FROM biz_voucher WHERE source_no LIKE 'M108%'",
            "DELETE FROM biz_exchange_rate WHERE from_currency IN ('USD', 'EUR')",
            "DELETE FROM biz_account WHERE account_code LIKE 'M108%'",
            "DELETE FROM biz_accounting_period WHERE period_code LIKE '2062-%'",
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
        let year: i16 = period_code[..4].parse().expect("年");
        let month: i16 = period_code[5..].parse().expect("月");
        db().exec_sql(
            format!(
                "INSERT INTO biz_accounting_period \
                    (period_year, period_month, period_code, start_date, end_date, status) \
                 VALUES (?, ?, '{period_code}', '{start}'::text::date, '{end}'::text::date, 'OPEN') \
                 ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN', closed_at = NULL, \
                     closed_by = NULL"
            ),
            vec![Value::I16(year), Value::I16(month)],
        )
        .await
        .expect("种期间");
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
                summary: format!("M108 造数 {tag} 借"),
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
                summary: format!("M108 造数 {tag} 贷"),
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
                source_type: "M108",
                source_id: None,
                source_no: Some(tag),
                source_doc_no: None,
                voucher_date: on_date,
                summary: format!("M108 造数凭证 {tag}"),
                lines: entries,
                remark: Some("M1-08 真库用例夹具"),
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

    /// 一张**纯本位币**凭证（两行）：只改本位币账面，没有原币。
    async fn post_base(tag: &str, on_date: NaiveDate, debit: &AccountRef, credit: &AccountRef, amount: &str) {
        let entries = vec![
            EntryLine {
                line_no: 1,
                summary: format!("M108 本位币 {tag} 借"),
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
                summary: format!("M108 本位币 {tag} 贷"),
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
                source_type: "M108",
                source_id: None,
                source_no: Some(tag),
                source_doc_no: None,
                voucher_date: on_date,
                summary: format!("M108 本位币凭证 {tag}"),
                lines: entries,
                remark: Some("M1-08 真库用例夹具"),
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

    fn ledger_query(from: Option<&str>, to: Option<&str>) -> FxLedgerQuery {
        FxLedgerQuery {
            currency: Some("USD".to_string()),
            account_id: None,
            account_code: None,
            period_from: from.map(str::to_string),
            period_to: to.map(str::to_string),
            page: 1,
            page_size: 50,
        }
    }

    /// 主夹具：期初 200 USD + 本期 100 USD + 本期一笔纯本位币 50 + 草稿 999 USD。
    async fn seed_main_scenario() -> (AccountRef, AccountRef, AccountRef) {
        let asset = seed_account(ACC_ASSET, "M108 外币资产", 1).await;
        let counter = seed_account(ACC_COUNTER, "M108 外币对方", 1).await;
        let base = seed_account(ACC_BASE, "M108 本位币", 0).await;
        seed_period(PERIOD_OPENING, "2062-01-01", "2062-01-31").await;
        seed_period(PERIOD, "2062-02-01", "2062-02-28").await;
        // 期初：200 USD @ 7.0 = 1400
        post_fx(
            "M108-O1",
            date(2062, 1, 20),
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
            "M108-A1",
            date(2062, 2, 10),
            "7.2",
            &asset,
            &counter,
            "100",
            "720",
            "USD",
            voucher::STATUS_POSTED,
        )
        .await;
        // 本期：给对方科目灌一笔纯本位币 50（非外币凭证 ⇒ 不进外币明细账）
        post_base("M108-A2", date(2062, 2, 20), &counter, &base, "50").await;
        // 草稿：金额巨大，进表就一眼看得出来
        post_fx(
            "M108-D1",
            date(2062, 2, 25),
            "7.2",
            &asset,
            &counter,
            "999",
            "7192.8",
            "USD",
            voucher::STATUS_DRAFT,
        )
        .await;
        (asset, counter, base)
    }

    /// 真库：明细行 / 按科目汇总 / 逐行余额 / 本位币分录与草稿排除。
    #[tokio::test]
    async fn fx_ledger_reports_rows_accounts_and_running_balance() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_main_scenario().await;

        let report = fx_ledger(&ledger_query(Some(PERIOD), Some(PERIOD)))
            .await
            .expect("明细账应成功");
        assert_eq!(report.report_code, REPORT_CODE);
        assert_eq!(report.report_name, REPORT_NAME);
        assert_eq!(report.base_currency, "CNY");
        assert_eq!(report.currency, "USD");
        assert_eq!(report.currency_name.as_deref(), Some("美元"));
        assert_eq!(report.date_from, Some(date(2062, 2, 1)));
        assert_eq!(report.date_to, Some(date(2062, 2, 28)));

        assert_eq!(report.accounts.len(), 2, "两个外币科目：{:?}", report.accounts);
        let asset = report
            .accounts
            .iter()
            .find(|row| row.account_code == ACC_ASSET)
            .expect("资产科目");
        assert_eq!(asset.opening_fc, dec("200"), "期初（1 月那笔）");
        assert_eq!(asset.period_fc_debit, dec("100"));
        assert_eq!(asset.period_fc_credit, Decimal::ZERO);
        assert_eq!(asset.closing_fc, dec("300"));
        assert_eq!(asset.closing_side, "DEBIT");
        assert_eq!(asset.period_book_debit, dec("720"), "本位币金额来自分录");
        assert_eq!(asset.entry_count, 1);

        let counter = report
            .accounts
            .iter()
            .find(|row| row.account_code == ACC_COUNTER)
            .expect("对方科目");
        assert_eq!(counter.opening_fc, dec("-200"), "贷方余额是负数，不取绝对值");
        assert_eq!(counter.period_fc_credit, dec("100"));
        assert_eq!(counter.closing_fc, dec("-300"));
        assert_eq!(counter.closing_side, "CREDIT");
        assert_eq!(
            counter.period_book_credit,
            dec("720"),
            "那笔纯本位币 50 不是外币凭证 ⇒ 不进本表（含调汇的账面看外币余额表）"
        );

        assert_eq!(report.rows.len(), 2, "本期只有一张外币凭证的两行");
        assert_eq!(report.total, 2);
        assert_eq!(report.entry_count, 2);
        assert_eq!(report.opening_fc, Decimal::ZERO, "期初跨科目相加 = 0（借贷平衡）");
        assert_eq!(report.period_fc_debit, dec("100"));
        assert_eq!(report.period_fc_credit, dec("100"));
        assert_eq!(report.closing_fc, Decimal::ZERO);

        let first = report.rows.first().expect("第一行");
        assert_eq!(first.account_code, ACC_ASSET, "排序按科目编码");
        assert_eq!(first.currency_code, "USD");
        assert_eq!(first.exchange_rate, dec("7.2"), "建单锁定的汇率");
        assert_eq!(first.fc_debit, dec("100"));
        assert_eq!(first.debit_amount, dec("720"));
        assert_eq!(
            first.running_fc_balance,
            dec("300"),
            "逐行余额含该科目期初 200：{}",
            first.summary
        );
        assert_eq!(first.running_side, "DEBIT");
        let second = report.rows.get(1).expect("第二行");
        assert_eq!(second.account_code, ACC_COUNTER);
        assert_eq!(
            second.running_fc_balance,
            dec("-300"),
            "对方科目的余额从自己的期初 −200 起算，不串用上一科目的 300"
        );
        assert_eq!(second.running_side, "CREDIT");

        assert!(
            report.warnings.is_empty(),
            "干净数据不该有 warnings：{:?}",
            report.warnings
        );
        assert!(
            report.hints.iter().any(|hint| hint.contains("按科目分别累计")),
            "hints 要写清余额是分科目累计：{:?}",
            report.hints
        );
    }

    /// 真库：翻页时逐行余额必须连续（第 2 页的第一行含第 1 页的量）。
    #[tokio::test]
    async fn fx_ledger_running_balance_stays_continuous_across_pages() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_main_scenario().await;

        let mut page1 = ledger_query(None, Some(PERIOD));
        page1.page_size = 2;
        let first = fx_ledger(&page1).await.expect("第 1 页");
        assert_eq!(first.total, 4, "全区间 4 行（期初 2 + 本期 2）");
        assert_eq!(first.rows.len(), 2);
        // 全区间排序：科目优先 ⇒ M108FX01 的 01-20（+200）与 02-10（+100）在前
        assert_eq!(first.rows[0].running_fc_balance, dec("200"));
        assert_eq!(first.rows[1].running_fc_balance, dec("300"));

        let mut page2 = page1.clone();
        page2.page = 2;
        let second = fx_ledger(&page2).await.expect("第 2 页");
        assert_eq!(second.rows.len(), 2);
        assert_eq!(
            second.rows[0].running_fc_balance,
            dec("-200"),
            "第 2 页第一行是 M108FX02 的期初行：从自己的 0 起算、本行 −200"
        );
        assert_eq!(
            second.rows[1].running_fc_balance,
            dec("-300"),
            "同科目累加：−200 + (−100)；不串上一个科目的 300"
        );

        let mut page3 = page1;
        page3.page = 3;
        let third = fx_ledger(&page3).await.expect("第 3 页");
        assert!(third.rows.is_empty(), "越界页返回空 list，不报错");
    }

    /// 真库：入参校验（币种必填 / 本位币 400 / 非法码 / 区间反了 / 日期非法）。
    #[tokio::test]
    async fn fx_ledger_rejects_base_currency_and_malformed_params() {
        let Some(_guard) = setup().await else {
            return;
        };

        let mut missing = ledger_query(None, None);
        missing.currency = None;
        let error = fx_ledger(&missing).await.expect_err("缺 currency 必须 400");
        assert_eq!(error.status, 400);
        assert_eq!(error.code, error_code::PARAM_INVALID);

        let mut base = ledger_query(None, None);
        base.currency = Some("cny".to_string());
        let error = fx_ledger(&base).await.expect_err("本位币必须 400");
        assert_eq!(error.status, 400);
        assert!(
            error.msg.contains("本位币") && error.msg.contains("总账"),
            "报文要指路：{}",
            error.msg
        );

        let mut bad = ledger_query(None, None);
        bad.currency = Some("US".to_string());
        let error = fx_ledger(&bad).await.expect_err("非法币种码必须 400");
        assert_eq!(error.code, error_code::PARAM_INVALID);

        let error = fx_ledger(&ledger_query(Some("2062-02"), Some("2062-01")))
            .await
            .expect_err("区间反了必须 400");
        assert_eq!(error.status, 400);

        let error = fx_ledger(&ledger_query(Some("2062/02"), None))
            .await
            .expect_err("非法日期必须 400");
        assert_eq!(error.status, 400);

        let mut bad_id = ledger_query(None, None);
        bad_id.account_id = Some("not-a-uuid".to_string());
        let error = fx_ledger(&bad_id).await.expect_err("非法科目 id 必须 400");
        assert_eq!(error.status, 400);

        let mut unknown = ledger_query(None, None);
        unknown.account_code = Some("M108NOPE".to_string());
        let error = fx_ledger(&unknown).await.expect_err("科目不存在必须 404");
        assert_eq!(error.status, 404);
    }

    /// 真库：科目过滤只出该科目，且 `accounts` 与 `rows` 同步收窄。
    #[tokio::test]
    async fn fx_ledger_account_filter_narrows_both_rows_and_summary() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_main_scenario().await;

        let mut query = ledger_query(Some(PERIOD), Some(PERIOD));
        query.account_code = Some(ACC_ASSET.to_string());
        let report = fx_ledger(&query).await.expect("按科目过滤");
        assert_eq!(report.accounts.len(), 1);
        assert_eq!(report.accounts[0].account_code, ACC_ASSET);
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].account_code, ACC_ASSET);
        assert_eq!(report.rows[0].running_fc_balance, dec("300"));
        assert_eq!(report.closing_fc, dec("300"));
        let echo = report.account.as_ref().expect("科目回显");
        assert_eq!(echo.account_code, ACC_ASSET);
        assert!(echo.is_foreign_currency);

        // 按 id 过滤等价于按编码过滤
        let by_id = FxLedgerQuery {
            account_id: Some(report.account.as_ref().expect("回显").id.clone()),
            account_code: None,
            ..query
        };
        let report2 = fx_ledger(&by_id).await.expect("按科目 id 过滤");
        assert_eq!(report2.total, report.total);
    }

    /// 真库：与外币余额表**同口径**（期末原币逐科目相等）。
    #[tokio::test]
    async fn fx_ledger_closing_matches_fx_balance() {
        let Some(_guard) = setup().await else {
            return;
        };
        seed_main_scenario().await;

        let ledger = fx_ledger(&ledger_query(Some(PERIOD), Some(PERIOD)))
            .await
            .expect("明细账");
        let balance = crate::business::erp::fx_balance::fx_balance(&crate::business::erp::fx_balance::FxBalanceQuery {
            period: Some(PERIOD.to_string()),
            currency: Some("USD".to_string()),
        })
        .await
        .expect("外币余额表");

        for account in &ledger.accounts {
            let other = balance
                .rows
                .iter()
                .find(|row| row.account_code == account.account_code && row.currency_code == "USD")
                .unwrap_or_else(|| panic!("余额表缺科目 {}", account.account_code));
            assert_eq!(
                account.closing_fc, other.closing_fc,
                "科目 {} 的期末原币必须与外币余额表相等（同一 WHERE、同一 COALESCE）",
                account.account_code
            );
            assert_eq!(account.opening_fc, other.opening_fc, "期初同样要对得上");
        }
    }

    /// 真库：外币分录落在**未标记**科目上 ⇒ 不进表但要点名（数据质量问题不许静默）。
    #[tokio::test]
    async fn fx_ledger_warns_about_unmarked_accounts() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (asset, counter, _base) = seed_main_scenario().await;

        // 造一张正常的外币凭证，然后把「对方科目」改成未标记外币核算（模拟历史数据漂移）
        post_fx(
            "M108-U1",
            date(2062, 2, 15),
            "7.2",
            &asset,
            &counter,
            "10",
            "72",
            "USD",
            voucher::STATUS_POSTED,
        )
        .await;
        db().exec_sql(
            "UPDATE biz_account SET is_foreign_currency = 0 WHERE account_code = ?".to_string(),
            vec![Value::Text(ACC_COUNTER.to_string())],
        )
        .await
        .expect("把对方科目改成未标记外币核算");

        let report = fx_ledger(&ledger_query(Some(PERIOD), Some(PERIOD)))
            .await
            .expect("数据脏也要能出表");
        assert!(
            report.accounts.iter().all(|row| row.account_code != ACC_COUNTER),
            "未标记科目不进表"
        );
        assert!(
            report.warnings.iter().any(|warn| warn.contains("未标记")),
            "warnings 要点名未标记科目：{:?}",
            report.warnings
        );

        // 过滤到未标记科目时同样要解释为什么是空表
        let mut query = ledger_query(Some(PERIOD), Some(PERIOD));
        query.account_code = Some(ACC_COUNTER.to_string());
        let filtered = fx_ledger(&query).await.expect("过滤未标记科目");
        assert!(filtered.rows.is_empty());
        assert!(
            filtered
                .warnings
                .iter()
                .any(|warn| warn.contains("未标记") && warn.contains(ACC_COUNTER)),
            "要点名这个科目：{:?}",
            filtered.warnings
        );
    }
}

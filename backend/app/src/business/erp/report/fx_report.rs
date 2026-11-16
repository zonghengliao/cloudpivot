//! 报表币种口径（M1-09 · 多币种收口）：资产负债表 / 利润表的**币种切换**。只读。
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/report/balance-sheet?period=2026-10[&currency=USD]` | `business:erp:report:balance-sheet` | 资产负债表（可切币种） |
//! | GET | `/business/erp/report/income-statement?period=2026-10[&currency=USD]` | `business:erp:report:income-statement` | 利润表（可切币种） |
//! | GET | `/business/erp/report/balance-sheet/export?period=…[&currency=USD]` | `business:erp:report:export` | 同上（xlsx） |
//! | GET | `/business/erp/report/income-statement/export?period=…[&currency=USD]` | `business:erp:report:export` | 同上（xlsx） |
//!
//! # 口径（唯一真相，改这里必须同步前端 `index.ts` 的类型注释与本模块单测）
//!
//! ```text
//! 不传 currency / 传账套本位币 ⇒ BASE 模式：与 M1-09 之前**完全一致**（本位币分录，含期末调汇）
//!
//! 传外币 currency（如 USD）⇒ FOREIGN 模式：
//!   取数：只取「凭证币种 = 请求币种」的已过账分录（v.currency_code = 币种），
//!         金额 = COALESCE(fc_debit_amount, debit_amount) − COALESCE(fc_credit_amount, credit_amount)
//!   报表金额 = 原币金额（以该外币计价，勾稽恒等式在同一口径内成立）
//!   折合列   = round(原币金额 × 期间末日 SPOT 汇率, 4)；取不到牌价 ⇒ null + rate_missing
//! ```
//!
//! ## ① 原币口径不掺本位币分录（含期末调汇）
//!
//! 与 M1-07 外币余额表 / M1-08 外币明细账**逐字同口径**：本位币分录（`fc_*` 为 NULL 的
//! `currency_code = 本位币` 凭证）不进原币数字。期末调汇凭证是**本位币凭证**（M1-05：
//! `rate = 1`、分录不带原币），它调的是账面本位币，原币头寸一分没动 —— 把它算进原币口径，
//! 报表就会凭空多出「调汇差额」这笔原币。所以在 `currency = USD` 下，
//! `某科目原币期末 ≡ 外币余额表同 (科目, 币种) 的 closing_fc`（真库用例钉住这条对拍）。
//!
//! ## ② 缺牌价：折合列留空，绝不回落 rate = 1
//!
//! 一张外币报表不该因为期末没维护牌价就打不开（与 M1-07 同一取舍）。所以缺牌价时：
//! 原币列照出、`rate = null`、`rate_missing = true`、`notes/hints` 点名去「汇率管理」补。
//! **绝不按 1 折算**：`42 000 USD × 1 = 42 000 CNY` 会被当成真数抄进报表，留空至少会被人问。
//! （与 M1-05 调汇「缺牌价必须 400 硬拒」不冲突：那里是要落账，错一分就进总账；报表只是展示。）
//!
//! ## ③ 币种 = 可选切换维度：本位币 200、非法码 400、未知码 400 + 66084
//!
//! | 入参 | 结果 | 理由 |
//! |------|------|------|
//! | 不传 / 空串 | BASE 模式，200 | 与改动前逐字一致（前端默认列也不传） |
//! | 本位币（CNY） | BASE 模式，200 | 「切回本位币」是正常操作，不是错误 |
//! | 格式非法（`usd$` / `US` / 空里带空格之外的脏字符） | 400 + `10001` | 输入错，**不静默忽略**（否则用户以为切了币种） |
//! | 格式合法但主数据没有（`XYZ`） | 400 + `66084` | 主数据缺失，指路「币种 / 汇率」；绝不当成本位币出表 |
//!
//! ## ④ 勾稽恒等式在哪个口径下成立
//!
//! 外币凭证 `Σ原币借 = Σ原币贷`（M1-04：同一凭证按单一汇率锁定，原币与本位币同时平衡），
//! 所以 FOREIGN 模式下「资产 ≡ 负债 + 权益」仍然是**同一个数学恒等式**，不需要换公式。
//! `check` / `diff` / `unmapped` / `unbalanced_vouchers` 全部在同一币种口径下给出：
//! FOREIGN 模式下 `unbalanced_vouchers` 判的是「该币种分录的原币借贷是否相等」
//! （单头 `total_debit/credit` 是本位币，拿来和原币比毫无意义，只会条条报假红）。
//!
//! 期初余额台账（`biz_opening_balance`）只有一个本位币口径、**没有币种列**，所以：
//! * 「期初不平衡 ⇒ 不出表」这条账套级不变量两种模式都保留（账套歪了，哪个币种出表都不可信）；
//! * 期初凭证（`voucher_type = 'OPENING'`）的币种口径照 `load_accounts` 原规则处理
//!   （FOREIGN 模式下是按币种过滤后的同一套规则，不是特例）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_account / biz_voucher / biz_voucher_entry / biz_currency / biz_exchange_rate
//! 是全局会计台账与主数据（无 dept_id / 归属人列），数据范围表达不出来；可见性由
//! business:erp:report:balance-sheet / :income-statement / :export 权限码控制，本模块无分页查询。
//! ```

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use serde::Serialize;

use super::{BalanceSheetRes, ReportLineRes};
use crate::business::erp::currency;
use crate::business::erp::income_statement::IncomeStatementRes;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 本位币口径（不传 currency / 传本位币）。
pub const MODE_BASE: &str = "BASE";

/// 外币原币口径（传了外币币种）。
pub const MODE_FOREIGN: &str = "FOREIGN";

/// 币种表（取名称 / 判存在性）。
const CURRENCY_TABLE: &str = "biz_currency";

/// 凭证表。
const VOUCHER_TABLE: &str = "biz_voucher";

/// 分录表。
const ENTRY_TABLE: &str = "biz_voucher_entry";

/// 只统计已过账凭证（与 `ledger` / `fx_balance` / `fx_ledger` 逐字一致）。
const POSTED: &str = "POSTED";

// ==================== 口径 ====================

/// 报表币种口径（请求侧的唯一表示）。
///
/// `currency == base_currency` 即 BASE 模式 —— 用「等于本位币」而不是 `Option` 来表达，
/// 是因为两种模式在取数 / 勾稽 / 出参上只差「是否按币种过滤 + 是否折算」，
/// 不需要两个互斥类型（`is_foreign()` 就是那一个判断）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportBasis {
    /// 报表计价币种。
    pub currency: String,
    /// 账套本位币。
    pub base_currency: String,
}

impl ReportBasis {
    /// 本位币口径。
    pub fn base(base_currency: &str) -> Self {
        Self {
            currency: base_currency.trim().to_ascii_uppercase(),
            base_currency: base_currency.trim().to_ascii_uppercase(),
        }
    }

    /// 是否外币原币口径。
    pub fn is_foreign(&self) -> bool {
        self.currency != self.base_currency
    }

    /// 外币原币口径时的币种码（BASE 模式 `None`）—— 直接喂给 SQL 的 `v.currency_code = ?`。
    pub fn fx_currency(&self) -> Option<&str> {
        self.is_foreign().then_some(self.currency.as_str())
    }
}

/// 校验并解析「请求币种」→ 报表口径。
///
/// 见模块文档 ③ 的四条分支。这里**只做校验**（一次主数据查询），不查汇率、不碰凭证。
pub async fn resolve(requested: Option<&str>) -> std::result::Result<ReportBasis, ApiError> {
    let base_currency = currency::base_currency_code(db())
        .await
        .map_err(|error| utils::db_error(error, "查询账套本位币"))?;
    let wanted = requested
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|raw| {
            currency::normalize_currency(raw).ok_or_else(|| {
                ApiError::new(
                    400,
                    error_code::PARAM_INVALID,
                    format!("currency 必须是三位大写币种码（如 USD），收到「{raw}」"),
                )
            })
        })
        .transpose()?;
    let Some(code) = wanted else {
        return Ok(ReportBasis::base(&base_currency));
    };
    if code == base_currency {
        return Ok(ReportBasis::base(&base_currency));
    }
    let row = db()
        .query_first_sql(
            format!("SELECT name, status FROM {CURRENCY_TABLE} WHERE code = ?"),
            vec![Value::Text(code.clone())],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询报表币种"))?;
    if row.is_none() {
        return Err(ApiError::new(
            400,
            error_code::ERP_REPORT_CURRENCY_INVALID,
            format!(
                "报表币种 {code} 不在「币种」主数据里（{CURRENCY_TABLE} 无该行）：\
                 请先在「币种 / 汇率」维护该币种，再按币种出表（本位币 {base_currency} 报表不传 currency 即可）"
            ),
        ));
    }
    Ok(ReportBasis {
        currency: code,
        base_currency,
    })
}

// ==================== 出参 ====================

/// 报表币种块（两张报表共用；前端据 `mode` 决定是否显示「折合本位币」列）。
#[derive(Debug, Clone, Serialize)]
pub struct ReportCurrencyRes {
    /// 口径：`BASE`（本位币，与改动前一致）/ `FOREIGN`（外币原币）。
    pub mode: String,
    /// 报表计价币种（BASE 模式 = 账套本位币）。
    pub currency: String,
    /// 币种名称（`biz_currency.name`；主数据缺失时 `null`）。
    pub currency_name: Option<String>,
    /// 账套本位币。
    pub base_currency: String,
    /// 折算汇率（`1 currency = rate base`）：BASE 模式恒 `1.00000000`；FOREIGN 模式取期间末日
    /// 即期 SPOT，取不到为 `null`（**绝不回落 1**，见模块文档 ②）。
    pub rate: Option<Decimal>,
    /// 汇率取数日（期间末日；BASE 模式 `null`）。实际牌价的 `effective_date` 可能更早（区间口径）。
    pub rate_date: Option<NaiveDate>,
    /// 是否缺牌价（`true` ⇒ 折合列全为 `null`，原币列照出）。
    pub rate_missing: bool,
    /// 期间内该币种已过账分录条数（`0` ⇒ 本表各行必然全 0，`notes/hints` 会点名）。
    pub entry_count: i64,
}

impl ReportCurrencyRes {
    /// 占位块：`into_response` 构造出参时先填它，接口层紧接着用 [`describe`] 覆盖。
    ///
    /// 为什么留占位而不是让 `into_response` 直接收口径：`into_response` 被 PDF 导出
    /// （`report_pdf`）与大量单测直接调用，那些路径**不查库**、也不该为了一个展示块多一次
    /// 主数据往返。占位块里 `currency` 为空串 —— 唯一会漏出去的可能就是「有人新增了一条
    /// 返回 JSON 的路由却忘了调 `apply_to_*`」，那种情况前端显示空币种，比显示一个
    /// 猜出来的 `CNY` 更容易被发现。
    pub fn unset() -> Self {
        Self {
            mode: MODE_BASE.to_string(),
            currency: String::new(),
            currency_name: None,
            base_currency: String::new(),
            rate: Some(Decimal::ONE),
            rate_date: None,
            rate_missing: false,
            entry_count: 0,
        }
    }
}

/// 组装币种块：名称 + （外币时）期间末日即期汇率 + 期间内该币种分录条数。
pub async fn describe(
    basis: &ReportBasis,
    start: NaiveDate,
    end: NaiveDate,
) -> std::result::Result<ReportCurrencyRes, ApiError> {
    let row = db()
        .query_first_sql(
            format!("SELECT name FROM {CURRENCY_TABLE} WHERE code = ?"),
            vec![Value::Text(basis.currency.clone())],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询报表币种名称"))?;
    let name = row.and_then(|row| row.get_typed::<String>("name").ok());

    let (rate, rate_date) = if basis.is_foreign() {
        let rate = currency::spot_rate(db(), &basis.currency, &basis.base_currency, end)
            .await
            .map_err(|error| utils::db_error(error, "查询报表折算汇率"))?;
        (rate, Some(end))
    } else {
        (Some(Decimal::ONE), None)
    };

    let entry_count = count_entries(basis, start, end)
        .await
        .map_err(|error| utils::db_error(error, "统计报表币种分录条数"))?;

    Ok(ReportCurrencyRes {
        mode: if basis.is_foreign() { MODE_FOREIGN } else { MODE_BASE }.to_string(),
        currency: basis.currency.clone(),
        currency_name: name,
        base_currency: basis.base_currency.clone(),
        rate,
        rate_date,
        rate_missing: basis.is_foreign() && rate.is_none(),
        entry_count,
    })
}

/// 期间内该币种的已过账分录条数（报表是不是「真的没数」，靠它区分「没业务」和「取错币种」）。
async fn count_entries(basis: &ReportBasis, start: NaiveDate, end: NaiveDate) -> DbResult<i64> {
    let sql = format!(
        "SELECT COUNT(*)::bigint AS n \
           FROM {ENTRY_TABLE} e JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
          WHERE v.status = '{POSTED}' AND v.deleted_at IS NULL \
            AND v.currency_code = ? \
            AND v.voucher_date >= ? AND v.voucher_date <= ?"
    );
    let row = db()
        .query_first_sql(
            sql,
            vec![
                Value::Text(basis.currency.clone()),
                Value::from(start),
                Value::from(end),
            ],
        )
        .await?;
    match row {
        Some(row) => row.get_typed("n"),
        None => Ok(0),
    }
}

/// 原币 → 折合本位币：缺牌价返回 `None`（**不准回落 1**），标题行（无金额）也返回 `None`。
pub fn convert(amount: Option<Decimal>, rate: Option<Decimal>) -> Option<Decimal> {
    let amount = amount?;
    let rate = rate?;
    Some((amount * rate).round_dp(currency::AMOUNT_SCALE))
}

// ==================== 套到出参上 ====================

/// 把币种块套到资产负债表：逐行补「折合本位币」列、改金额单位、追加口径附注/提示。
///
/// BASE 模式只写 `currency` 块，**不动任何一个已有字段**（与改动前逐字一致，靠这条
/// 「BASE 模式零差异」让老页面 / 老单测不用改）。
pub fn apply_to_balance_sheet(report: &mut BalanceSheetRes, block: ReportCurrencyRes) {
    let foreign = block.mode == MODE_FOREIGN;
    let rate = block.rate;
    for line in report
        .assets
        .iter_mut()
        .chain(report.liabilities.iter_mut())
        .chain(report.equity.iter_mut())
    {
        fill_line(line, foreign, rate);
    }
    if foreign {
        report.unit = block.currency.clone();
        push_common_notes(&block, &mut report.notes, &mut report.check.hints);
        report
            .check
            .hints
            .push("外币口径下「未归类」与「不平」都按该币种原币判断：差一分钱也照样报，绝不折算后抹平".to_string());
    }
    report.currency = block;
}

/// 把币种块套到利润表：逐行补「折合本位币」列（本期 + 本年累计）、改金额单位、追加提示。
pub fn apply_to_income(report: &mut IncomeStatementRes, block: ReportCurrencyRes) {
    let foreign = block.mode == MODE_FOREIGN;
    let rate = block.rate;
    for line in report.items.iter_mut() {
        line.current_base = if foreign {
            convert(Some(line.current), rate)
        } else {
            None
        };
        line.ytd_base = if foreign { convert(Some(line.ytd), rate) } else { None };
    }
    if foreign {
        report.unit = block.currency.clone();
        push_common_notes(&block, &mut report.notes, &mut report.check.hints);
        report.check.hints.push(
            "外币口径下「净利润 ≡ 未分配利润本年变动」按该币种原币判断；\
             折合列只是展示，不参与勾稽"
                .to_string(),
        );
    }
    report.currency = block;
}

/// 逐行折算（BASE 模式清空折合列，防止有人复用到已填过的结构体上）。
fn fill_line(line: &mut ReportLineRes, foreign: bool, rate: Option<Decimal>) {
    line.converted_amount = if foreign { convert(line.amount, rate) } else { None };
    line.previous_converted_amount = if foreign {
        convert(line.previous_amount, rate)
    } else {
        None
    };
}

/// 两张报表共享的口径说明（附注 / 提示同一份文案，避免两处口径说法漂移）。
fn push_common_notes(block: &ReportCurrencyRes, notes: &mut Vec<String>, hints: &mut Vec<String>) {
    let cur = &block.currency;
    let base = &block.base_currency;
    let name = block
        .currency_name
        .as_deref()
        .map(|text| format!("（{text}）"))
        .unwrap_or_default();
    notes.push(format!(
        "本表按 {cur}{name} **原币口径**编制：只取「凭证币种 = {cur}」的已过账分录，\
         本位币分录与期末调汇凭证不进本表；勾稽金额与差额均以 {cur} 计"
    ));
    match (block.rate, block.rate_date) {
        (Some(rate), Some(date)) => notes.push(format!(
            "「折合{base}」列按 {date} 即期 SPOT 汇率 {rate} 折算（1 {cur} = {rate} {base}），\
             仅作展示，不参与勾稽"
        )),
        (_, Some(date)) => {
            notes.push(format!(
                "{date} 取不到 {cur}→{base} 的即期 SPOT 汇率 ⇒ 「折合{base}」列**留空**\
                 （绝不按 1 折算），请到「汇率管理」补 {cur} 牌价后重新出表"
            ));
            hints.push(format!(
                "缺 {cur} 期末牌价：折合列已留空。补一条 {cur}→{base} 的 SPOT 汇率（生效日 ≤ {date}）即可"
            ));
        }
        _ => {}
    }
    if block.entry_count == 0 {
        hints.push(format!(
            "本期间没有任何 {cur} 凭证 ⇒ 各行必然为 0；请确认期间 / 币种是否选错"
        ));
    }
}

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::Utc;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("十进制字面量")
    }

    #[test]
    fn fx_report_base_basis_is_not_foreign_and_has_no_currency_filter() {
        let basis = ReportBasis::base("cny");
        assert!(!basis.is_foreign(), "传本位币必须落 BASE 模式（切回本位币不是错误）");
        assert_eq!(basis.currency, "CNY");
        assert_eq!(basis.fx_currency(), None);
    }

    #[test]
    fn fx_report_foreign_basis_filters_by_currency() {
        let basis = ReportBasis {
            currency: "USD".to_string(),
            base_currency: "CNY".to_string(),
        };
        assert!(basis.is_foreign());
        assert_eq!(basis.fx_currency(), Some("USD"));
    }

    #[test]
    fn fx_report_convert_keeps_null_instead_of_falling_back_to_rate_one() {
        // 缺牌价 ⇒ 折合列 null（绝不 amount × 1）
        assert_eq!(convert(Some(dec("100")), None), None);
        // 标题行（无金额）⇒ null
        assert_eq!(convert(None, Some(dec("7.1"))), None);
        // 正常折算到 4 位小数（与本仓金额列同一个标度）
        assert_eq!(convert(Some(dec("100")), Some(dec("7.1234"))), Some(dec("712.3400")));
        assert_eq!(
            convert(Some(dec("33.3333")), Some(dec("3"))),
            Some(dec("99.9999")),
            "折算必须按 4 位四舍五入"
        );
    }

    #[test]
    fn fx_report_unset_block_is_never_mistaken_for_a_real_currency() {
        let block = ReportCurrencyRes::unset();
        assert_eq!(block.mode, MODE_BASE);
        assert!(
            block.currency.is_empty(),
            "占位块的币种必须为空（漏调 apply_* 时一眼看出来）"
        );
        assert_eq!(block.rate, Some(Decimal::ONE));
        assert!(!block.rate_missing);
    }

    #[test]
    fn fx_report_apply_balance_sheet_fills_converted_columns_only_when_foreign() {
        let mut report = BalanceSheetRes {
            report_code: "BALANCE_SHEET".to_string(),
            report_name: "资产负债表".to_string(),
            company_name: "云枢".to_string(),
            unit: "元".to_string(),
            period: "2063-10".to_string(),
            period_name: "2063年10月".to_string(),
            period_status: "OPEN".to_string(),
            period_start: NaiveDate::from_ymd_opt(2063, 10, 1).expect("日期"),
            period_end: NaiveDate::from_ymd_opt(2063, 10, 31).expect("日期"),
            generated_at: Utc::now(),
            previous_period: None,
            assets: vec![ReportLineRes {
                item_code: "MONETARY_FUNDS".to_string(),
                item_name: "货币资金".to_string(),
                level: 2,
                parent_code: None,
                amount: Some(dec("100")),
                previous_amount: None,
                converted_amount: None,
                previous_converted_amount: None,
                is_deduction: false,
                is_total: false,
                detail: None,
                accounts: vec![],
            }],
            liabilities: vec![],
            equity: vec![],
            reclass: vec![],
            notes: vec![],
            check: super::super::BalanceCheckRes {
                balanced: true,
                assets_total: dec("100"),
                liability_equity_total: dec("100"),
                diff: Decimal::ZERO,
                diff_text: "0.00".to_string(),
                message: "平衡".to_string(),
                ledger_debit_total: dec("100"),
                ledger_credit_total: dec("100"),
                unmapped: vec![],
                unbalanced_vouchers: vec![],
                hints: vec![],
            },
            currency: ReportCurrencyRes::unset(),
        };
        apply_to_balance_sheet(
            &mut report,
            ReportCurrencyRes {
                mode: MODE_FOREIGN.to_string(),
                currency: "USD".to_string(),
                currency_name: Some("美元".to_string()),
                base_currency: "CNY".to_string(),
                rate: Some(dec("7.1")),
                rate_date: Some(NaiveDate::from_ymd_opt(2063, 10, 31).expect("日期")),
                rate_missing: false,
                entry_count: 2,
            },
        );
        assert_eq!(report.unit, "USD", "外币口径的金额单位就是该币种");
        assert_eq!(report.assets[0].converted_amount, Some(dec("710.0000")));
        assert!(!report.notes.is_empty(), "外币口径必须带口径附注");
        assert!(report.notes[0].contains("原币口径"));
    }

    #[test]
    fn fx_report_apply_with_missing_rate_leaves_converted_null_and_warns() {
        let mut report = IncomeStatementRes {
            report_code: "INCOME_STATEMENT".to_string(),
            report_name: "利润表".to_string(),
            company_name: "云枢".to_string(),
            unit: "元".to_string(),
            period: "2063-10".to_string(),
            period_name: "2063年10月".to_string(),
            period_status: "OPEN".to_string(),
            period_start: NaiveDate::from_ymd_opt(2063, 10, 1).expect("日期"),
            period_end: NaiveDate::from_ymd_opt(2063, 10, 31).expect("日期"),
            ytd_start: NaiveDate::from_ymd_opt(2063, 1, 1).expect("日期"),
            current_label: "本期金额".to_string(),
            ytd_label: "本年累计".to_string(),
            generated_at: Utc::now(),
            items: vec![crate::business::erp::income_statement::IncomeLineRes {
                item_code: "REVENUE".to_string(),
                item_name: "营业收入".to_string(),
                level: 1,
                parent_code: None,
                current: dec("500"),
                ytd: dec("1500"),
                current_base: None,
                ytd_base: None,
                is_total: false,
                detail: None,
                accounts: vec![],
            }],
            notes: vec![],
            check: crate::business::erp::income_statement::IncomeCheckRes {
                balanced: true,
                net_profit_ytd: dec("500"),
                equity_change: dec("500"),
                diff: Decimal::ZERO,
                diff_text: "0.00".to_string(),
                message: "勾稽通过".to_string(),
                undistributed_profit_end: dec("500"),
                undistributed_profit_year_start: Decimal::ZERO,
                unmapped_profit_loss_ytd: Decimal::ZERO,
                unmapped: vec![],
                ledger_debit_total: dec("500"),
                ledger_credit_total: dec("500"),
                unbalanced_vouchers: vec![],
                hints: vec![],
            },
            currency: ReportCurrencyRes::unset(),
        };
        apply_to_income(
            &mut report,
            ReportCurrencyRes {
                mode: MODE_FOREIGN.to_string(),
                currency: "HKD".to_string(),
                currency_name: None,
                base_currency: "CNY".to_string(),
                rate: None,
                rate_date: Some(NaiveDate::from_ymd_opt(2063, 10, 31).expect("日期")),
                rate_missing: true,
                entry_count: 0,
            },
        );
        assert_eq!(report.items[0].current_base, None, "缺牌价必须留空");
        assert_eq!(report.items[0].ytd_base, None, "缺牌价必须留空");
        assert!(
            report.notes.iter().any(|text| text.contains("留空")),
            "缺牌价必须在附注里说清楚"
        );
        assert!(
            report.check.hints.iter().any(|text| text.contains("没有")),
            "空币种/空期间要进 hints（否则用户以为报表坏了）"
        );
    }
}

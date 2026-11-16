//! M1-10 多币种单测：**汇率变动 / 期末调汇 / 报表折算**（跨模块串联）。
//!
//! # 为什么要有这一套
//!
//! M1-04..M1-09 每张卡都钉住了自己那一段（凭证锁汇 / 外币余额表 / 外币明细账 / 期末调汇 /
//! 报表切币种），但**没有一条用例把一个月走完**：
//!
//! ```text
//! 汇率 7.0 建单 → 汇率走到 7.3 → 余额表出「汇兑差额」→ 调汇落一张本位币凭证
//!   → 余额表差额归零、原币一分不动 → 报表折合列跟着走；汇率再走到 7.5 时只补新差额
//! ```
//!
//! 这条链上任一环的取数口径漂了（把调汇凭证算进原币、折算列回落 rate = 1、调汇后重复计提），
//! 单卡用例都可能仍然全绿而账是错的。所以本文件断言的是**链上的恒等式**：
//!
//! | 恒等式 | 断在哪 |
//! |--------|--------|
//! | 报表折合列与调汇重估额是同一把尺子（同原币同汇率必须逐位相等） | 纯逻辑 |
//! | 汇率变动只动「折算列」，不动已过账凭证的本位币历史、不动原币 | 场景一 |
//! | 调汇差额 = 折算 − 账面；落账后差额归零、原币不变；同期重跑幂等 | 场景二 |
//! | 次月汇率再变只补「本期这段」差额，不把历史累计重提一遍 | 场景二 · 第二段 |
//! | 报表折合列逐行 = 原币 × 期末牌价；调汇凭证不进外币原币口径 | 场景三 |
//! | 缺牌价：报表留空、调汇 400 硬拒（同一件事的两种正确反应） | 场景四 |
//!
//! # 夹具（本文件独占，绝对值断言）
//!
//! ```text
//! 期间：1999-01 / 1999-02 / 1999-03
//!   为什么用 1999：调汇的取数是「**凭证日期 ≤ 期末**的全部已过账分录」，2031/2050/2064
//!   这些年度都会被别的卡的夹具污染（全量套件串在同一份库上跑），断言只能退化成增量。
//!   全仓最早的夹具年度是 2026（生产期间），其次是 2031；1999 年及以前没有任何用例造数，
//!   所以本文件可以用**绝对值**断言 —— 谁把取数窗口放错（例如把期后凭证算进期初、
//!   或把调汇凭证算进原币）都会立刻红，而不是被「增量相等」这种弱断言放过。
//! 科目：标准科目（1001 库存现金 / 1002 银行存款 / 1122 应收账款 / 2202 应付账款 /
//!   5001 主营业务收入），biz_report_item 的科目映射能命中 —— 报表行是 MONETARY_FUNDS /
//!   ACCOUNTS_RECEIVABLE / REVENUE 这些**真行**，不是「未归类」兜底行。
//!   外币标记按场景显式设置（默认 1001 / 2202 = 1，其余 = 0）：M1-04 的 66081 要求
//!   外币凭证的每条分录科目都标记「外币核算」，夹具不绕过它。
//! 牌价：USD→CNY SPOT
//!   [1999-01-01, 1999-01-31] = 7.0
//!   [1999-02-01, 1999-02-28] = 7.3
//!   [1999-03-01, 1999-12-31] = 7.5   ← 只有场景二第二段用到
//!   区间必须给**有限末日**：V0407 有「同一币对区间不重叠」的排除约束，而模板库里
//!   USD→CNY 有一条 effective_to IS NULL 的兜底行，与本用例 1999 年的行判成重叠
//!   ⇒ 夹具先整对清掉再种（与 M1-05 / M1-09 同一手法）。
//! 凭证（本位币金额 = 原币 × 锁定汇率，与 M1-04 的落库口径一致）：
//!   V1 1999-01-10 USD 1000 @7.0  借 1001 / 贷 2202    ← 场景一 / 二 / 三的起点
//!   V2 1999-02-05 USD  200 @7.3  借 1122 / 贷 5001    ← 场景三的当期业务
//!   V3 1999-02-08 CNY   50       借 1001 / 贷 1002    ← 场景二的「本位币分录」
//! ```
//!
//! V3 是场景二的关键：**本位币分录只改账面、不改原币**（真实场景：外币应收收到一笔人民币
//! 尾款 / 手工本位币调整）。没有它，调汇的 `net` 恒为 0 —— 因为同一张外币凭证借贷两侧
//! 用同一个汇率，逐科目差额必然互相抵消（这条恒等式本身也值得记在脑子里：调汇凭证里
//! 出现汇兑损益行，说明这个期间有「只动一侧账面」的分录）。

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use std::str::FromStr;

use super::fx_balance::{self, FxBalanceQuery};
use super::income_statement;
use super::period::period_fx_reval::{self, SOURCE_FX_REVAL_ACCOUNT};
use super::report::{self, fx_report};
use super::voucher::{self, AccountRef, EntryLine, FcAmount, PostVoucher};
use crate::business::erp::voucher::voucher_query::tests::{DB_LOCK, ensure_db};
use cloudpivot_kernel::shared::error_code;

// ==================== 夹具常量 ====================

/// 凭证号前缀（`biz_voucher.voucher_word`）：本文件独占，清场按它收。
const WORD: &str = "M110";

/// 建单期间（汇率 7.0）。
const P1: &str = "1999-01";

/// 汇率变动期间（汇率 7.3）。
const P2: &str = "1999-02";

/// 再次变动期间（汇率 7.5；只有场景二第二段用到）。
const P3: &str = "1999-03";

/// 库存现金（外币核算）。
const ACC_CASH: &str = "1001";

/// 银行存款（**不**标外币核算，只用来给场景二造一笔本位币凭证）。
const ACC_BANK: &str = "1002";

/// 应收账款。
const ACC_AR: &str = "1122";

/// 应付账款。
const ACC_PAYABLE: &str = "2202";

/// 主营业务收入。
const ACC_REVENUE: &str = "5001";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("十进制字面量")
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("固定日期合法")
}

// ==================== 夹具：清场 / 期间 / 币种标记 / 牌价 / 凭证 ====================

/// 加锁 + 连库 + 清场 + 建三个期间 + 缺省币种标记 + 种三档牌价。
///
/// 返回 `None` ⇒ 未设 `ERP_DB_URL`（打印 SKIP 后跳过用例）。卡面 §数据库约束 要求**真跑**，
/// 汇报里必须贴 pass 数，所以 SKIP 不算通过。
async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    for month in [1_u32, 2, 3] {
        crate::test_seed::ensure_accounting_period(1999, month).await;
    }
    mark_fx(&[(ACC_CASH, true), (ACC_PAYABLE, true)]).await;
    seed_rates().await;
    Some(guard)
}

/// 清场：本文件的凭证 / 状态日志 / 牌价 / 期间。
///
/// 牌价必须**整对清掉**（不只是 1999 年那几条）：V0407 的区间排除约束会把模板库那条
/// `effective_to IS NULL` 的 USD→CNY 兜底行与本用例的有限区间判成重叠，插入直接报冲突。
/// 这与 M1-05（`period_fx_reval`）/ M1-09 的清场逐字同口径。
async fn cleanup() {
    // 判据用 `source_type`（夹具自己写进去的 'M110'），**不是** `voucher_word`：
    // `voucher_word` 由号段规则派生（`PZ` / `PZR` …），不随调用方传的号变化。
    let statements = [
        "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN \
         (SELECT id FROM biz_voucher WHERE source_type = 'M110' \
             OR (source_type = 'FX_REVAL_ACCOUNT' AND source_no LIKE '1999-%'))"
            .to_string(),
        "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
         (SELECT id FROM biz_voucher WHERE source_type = 'M110' \
             OR (source_type = 'FX_REVAL_ACCOUNT' AND source_no LIKE '1999-%'))"
            .to_string(),
        "DELETE FROM biz_voucher WHERE source_type = 'M110' \
             OR (source_type = 'FX_REVAL_ACCOUNT' AND source_no LIKE '1999-%')"
            .to_string(),
        "DELETE FROM biz_exchange_rate WHERE from_currency IN ('USD', 'EUR') AND to_currency = 'CNY'".to_string(),
        "DELETE FROM biz_accounting_period WHERE period_code LIKE '1999-%'".to_string(),
    ];
    for sql in statements {
        db().exec_sql(sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("M110 夹具清场失败：{error}"));
    }
}

/// 外币核算标记：显式设置（不是「按默认」）—— 全量套件里 M1-03 / M1-09 的夹具也会改
/// `biz_account.is_foreign_currency`，不显式定死就会依赖用例执行顺序。
async fn mark_fx(marks: &[(&str, bool)]) {
    for (code, marked) in marks {
        db().execute_with_params(
            "UPDATE biz_account SET is_foreign_currency = ? WHERE account_code = ? AND deleted_at IS NULL",
            vec![Value::I16(i16::from(*marked)), Value::Text((*code).to_string())],
        )
        .await
        .unwrap_or_else(|error| panic!("M110 夹具：设置科目 {code} 外币标记失败：{error}"));
    }
}

/// 收场：清掉本文件的夹具，并把外币标记还原成模板默认（`is_foreign_currency = 0`）。
///
/// # 为什么必须收场，而不是「下一轮 setup 自己清」
///
/// 全量套件是**串在同一份库上**跑的：M1-07 余额表 / M1-08 明细账的用例排在本文件之后，
/// 它们按自己的期间断言「行数 / 期初 / 期末」。本文件留在 1999 年的凭证会被它们算进
/// 「期初」→ 多出来的行让它们红（实测：不加收场时 `fx_balance` 三条用例红，
/// 加了以后与基线逐条一致）。夹具不止要「自己跑得对」，还要**把库还给下一个人**。
async fn teardown() {
    cleanup().await;
    db().execute_with_params(
        "UPDATE biz_account SET is_foreign_currency = 0 \
          WHERE account_code IN ('1001', '1002', '1122', '2202', '5001') AND deleted_at IS NULL",
        vec![],
    )
    .await
    .expect("M110 夹具：还原外币核算标记");
}

/// 种三档 SPOT 牌价（区间首尾相接、互不重叠）。
async fn seed_rates() {
    for (from_date, through, rate) in [
        ("1999-01-01", "1999-01-31", "7.0"),
        ("1999-02-01", "1999-02-28", "7.3"),
        ("1999-03-01", "1999-12-31", "7.5"),
    ] {
        db().execute_with_params(
            "INSERT INTO biz_exchange_rate \
                (from_currency, to_currency, rate_type, effective_date, effective_to, rate) \
             VALUES ('USD', 'CNY', 'SPOT', ?::text::date, ?::text::date, ?::text::numeric)",
            vec![
                Value::Text(from_date.to_string()),
                Value::Text(through.to_string()),
                Value::Text(rate.to_string()),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("M110 夹具：种牌价 {from_date}~{through} = {rate} 失败：{error}"));
    }
}

/// 读科目（`AccountRef` 三件套：id / 编码 / 名称）。
async fn account_ref(code: &str) -> AccountRef {
    let row = db()
        .query_first_sql(
            "SELECT id, account_code, account_name FROM biz_account \
              WHERE account_code = ? AND deleted_at IS NULL"
                .to_string(),
            vec![Value::Text(code.to_string())],
        )
        .await
        .unwrap_or_else(|error| panic!("M110 夹具：读科目 {code} 失败：{error}"))
        .unwrap_or_else(|| panic!("M110 夹具：标准科目 {code} 不在科目表里"));
    AccountRef {
        id: row.get_typed("id").expect("科目 id"),
        code: row.get_typed("account_code").expect("科目编码"),
        name: row.get_typed("account_name").expect("科目名称"),
    }
}

/// 一张**外币**凭证：借贷两行、原币同额、本位币金额 = 原币 × 汇率。
async fn post_fx(no: &str, on: &str, debit: &str, credit: &str, fc: &str, rate: &str) {
    let amount = dec(fc);
    let rate_value = dec(rate);
    let base = (amount * rate_value).round_dp(4);
    let (debit_acc, credit_acc) = (account_ref(debit).await, account_ref(credit).await);
    let lines = vec![
        line(1, format!("M110 夹具 {no} 借"), debit_acc, base, Decimal::ZERO),
        line(2, format!("M110 夹具 {no} 贷"), credit_acc, Decimal::ZERO, base),
    ];
    let fc_amounts = [
        FcAmount {
            debit: amount,
            credit: Decimal::ZERO,
        },
        FcAmount {
            debit: Decimal::ZERO,
            credit: amount,
        },
    ];
    voucher::post_voucher_fx(
        db(),
        &PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: WORD,
            source_id: None,
            source_no: Some(no),
            source_doc_no: None,
            voucher_date: NaiveDate::parse_from_str(on, "%Y-%m-%d").expect("凭证日期"),
            summary: format!("M110 夹具凭证 {no}"),
            lines,
            remark: Some("M1-10 多币种单测夹具"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        Some(&fc_amounts),
        Some(("USD", rate_value)),
        None,
        Utc::now(),
    )
    .await
    .unwrap_or_else(|error| panic!("M110 夹具：外币凭证 {no} 落库失败：{error}"));
}

/// 一张**本位币**凭证（分录不带原币：`fc_*` 两列 NULL）。
async fn post_base(no: &str, on: &str, debit: &str, credit: &str, amount: &str) {
    let value = dec(amount);
    let (debit_acc, credit_acc) = (account_ref(debit).await, account_ref(credit).await);
    let lines = vec![
        line(1, format!("M110 夹具 {no} 借"), debit_acc, value, Decimal::ZERO),
        line(2, format!("M110 夹具 {no} 贷"), credit_acc, Decimal::ZERO, value),
    ];
    voucher::post_voucher_fx(
        db(),
        &PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: WORD,
            source_id: None,
            source_no: Some(no),
            source_doc_no: None,
            voucher_date: NaiveDate::parse_from_str(on, "%Y-%m-%d").expect("凭证日期"),
            summary: format!("M110 夹具凭证 {no}"),
            lines,
            remark: Some("M1-10 多币种单测夹具（本位币分录）"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        None,
        None,
        None,
        Utc::now(),
    )
    .await
    .unwrap_or_else(|error| panic!("M110 夹具：本位币凭证 {no} 落库失败：{error}"));
}

/// 拼一行分录（辅助核算维度一律空：这些标准科目不要求维度）。
fn line(line_no: i32, summary: String, account: AccountRef, debit: Decimal, credit: Decimal) -> EntryLine {
    EntryLine {
        line_no,
        summary,
        account,
        debit,
        credit,
        aux_supplier_id: None,
        aux_material_id: None,
        aux_warehouse_id: None,
        aux_dept_id: None,
    }
}

// ==================== 取数 / 断言小工具 ====================

/// 跑一次外币余额表（与接口层同一条路径）。
async fn fx_balance_of(period: &str, currency: Option<&str>) -> fx_balance::FxBalanceRes {
    fx_balance::fx_balance(&FxBalanceQuery {
        period: Some(period.to_string()),
        currency: currency.map(str::to_string),
    })
    .await
    .unwrap_or_else(|error| panic!("外币余额表 {period} 打开失败：{} {}", error.status, error.msg))
}

/// 在余额表里取某 `(科目, 币种)` 行；没有就 panic 并打印实际行（排障比「找不到」三个字有用）。
fn row_of<'a>(
    result: &'a fx_balance::FxBalanceRes,
    account_code: &str,
    currency: &str,
) -> &'a fx_balance::FxBalanceRow {
    result
        .rows
        .iter()
        .find(|row| row.account_code == account_code && row.currency_code == currency)
        .unwrap_or_else(|| {
            panic!(
                "外币余额表里没有 ({account_code}, {currency}) 行；实际行：{:?}",
                result
                    .rows
                    .iter()
                    .map(|row| format!("{}/{}", row.account_code, row.currency_code))
                    .collect::<Vec<_>>()
            )
        })
}

/// 跑一次资产负债表（与接口层同一条路径：REPEATABLE READ 事务 + 币种块）。
async fn balance_sheet(period: &str, currency: Option<&str>) -> report::BalanceSheetRes {
    let basis = fx_report::resolve(currency)
        .await
        .unwrap_or_else(|error| panic!("解析报表币种失败：{} {}", error.status, error.msg));
    let tx = db().begin().await.expect("开启事务");
    tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .await
        .expect("设置隔离级别");
    let snapshot = report::build_snapshot_of(&tx, period, basis.fx_currency())
        .await
        .unwrap_or_else(|error| panic!("编制资产负债表 {period} 失败：{error}"));
    let block = fx_report::describe(&basis, snapshot.period.start_date, snapshot.period.end_date)
        .await
        .expect("组装币种块");
    tx.commit().await.expect("提交");
    let mut out = report::into_response(
        report::REPORT_CODE,
        report::REPORT_NAME,
        "M110 演练公司",
        "元",
        snapshot,
    );
    fx_report::apply_to_balance_sheet(&mut out, block);
    out
}

/// 跑一次利润表（同上）。
async fn income_statement_of(period: &str, currency: Option<&str>) -> income_statement::IncomeStatementRes {
    let basis = fx_report::resolve(currency)
        .await
        .unwrap_or_else(|error| panic!("解析报表币种失败：{} {}", error.status, error.msg));
    let tx = db().begin().await.expect("开启事务");
    tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .await
        .expect("设置隔离级别");
    let snapshot = income_statement::build_snapshot_of(&tx, period, basis.fx_currency())
        .await
        .unwrap_or_else(|error| panic!("编制利润表 {period} 失败：{error}"));
    let block = fx_report::describe(&basis, snapshot.period.start_date, snapshot.period.end_date)
        .await
        .expect("组装币种块");
    tx.commit().await.expect("提交");
    let mut out = income_statement::into_response("M110 演练公司", "元", snapshot);
    fx_report::apply_to_income(&mut out, block);
    out
}

/// 报表侧找某一项（找不到直接 panic：报表项编码漂了就该红）。
fn line_of<'a>(lines: &'a [report::ReportLineRes], item_code: &str) -> &'a report::ReportLineRes {
    lines.iter().find(|row| row.item_code == item_code).unwrap_or_else(|| {
        panic!(
            "报表里没有 {item_code} 项；实际项：{:?}",
            lines.iter().map(|row| row.item_code.clone()).collect::<Vec<_>>()
        )
    })
}

/// 调汇凭证头部（`exchange_rate` / 借贷合计 / 状态 / 币种）。
struct RevalHeader {
    exchange_rate: Decimal,
    total_debit: Decimal,
    total_credit: Decimal,
    status: String,
    currency_code: String,
}

/// 该期间调汇凭证的张数（幂等断言用）。
async fn reval_count(period: &str) -> i64 {
    db().scalar_with_params(
        "SELECT count(*) FROM biz_voucher \
          WHERE source_type = ? AND source_no = ? AND deleted_at IS NULL",
        vec![
            Value::Text(SOURCE_FX_REVAL_ACCOUNT.to_string()),
            Value::Text(period.to_string()),
        ],
    )
    .await
    .expect("统计调汇凭证张数")
}

/// 该期间调汇凭证的头部（不存在 ⇒ panic）。
async fn reval_header(period: &str) -> RevalHeader {
    let row = db()
        .query_first_sql(
            "SELECT exchange_rate, total_debit, total_credit, status, currency_code \
               FROM biz_voucher \
              WHERE source_type = ? AND source_no = ? AND deleted_at IS NULL \
              ORDER BY created_at ASC LIMIT 1"
                .to_string(),
            vec![
                Value::Text(SOURCE_FX_REVAL_ACCOUNT.to_string()),
                Value::Text(period.to_string()),
            ],
        )
        .await
        .expect("读调汇凭证")
        .unwrap_or_else(|| panic!("{period} 必须有调汇凭证"));
    RevalHeader {
        exchange_rate: row.get_typed("exchange_rate").expect("汇率"),
        total_debit: row.get_typed("total_debit").expect("借方合计"),
        total_credit: row.get_typed("total_credit").expect("贷方合计"),
        status: row.get_typed("status").expect("状态"),
        currency_code: row.get_typed("currency_code").expect("币种"),
    }
}

/// 调汇凭证的一条分录（原币两列是否为空单列出来：调汇凭证**不许**带原币）。
struct RevalEntry {
    account_code: String,
    debit: Decimal,
    credit: Decimal,
    fc_null: bool,
}

/// 该期间调汇凭证的分录（按行号升序）。
async fn reval_entries(period: &str) -> Vec<RevalEntry> {
    let rows = db()
        .query_sql(
            "SELECT e.account_code, e.debit_amount, e.credit_amount, \
                    (e.fc_debit_amount IS NULL AND e.fc_credit_amount IS NULL) AS fc_null \
               FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
              WHERE v.source_type = ? AND v.source_no = ? AND v.deleted_at IS NULL \
              ORDER BY e.line_no"
                .to_string(),
            vec![
                Value::Text(SOURCE_FX_REVAL_ACCOUNT.to_string()),
                Value::Text(period.to_string()),
            ],
        )
        .await
        .expect("读调汇分录");
    rows.iter()
        .map(|row| RevalEntry {
            account_code: row.get_typed("account_code").expect("科目编码"),
            debit: row.get_typed("debit_amount").expect("借方"),
            credit: row.get_typed("credit_amount").expect("贷方"),
            fc_null: row.get_typed("fc_null").expect("原币列是否为空"),
        })
        .collect()
}

// ==================== 纯逻辑：同一把尺子 ====================

/// 报表的「折合本位币」与调汇的「重估后金额」必须是**同一个函数**的口径。
///
/// 这两处各写一份四舍五入，账就会差在分位上：报表说 7300.0001、调汇提 7300.0000，
/// 差额永远抹不平（而两边各自都「看起来对」）。这里用含负数、含 8 位汇率、含 0 的
/// 字面量把两条路径钉在同一个结果上。
#[test]
fn report_conversion_and_revaluation_share_one_ruler() {
    for (fc, rate) in [
        ("1000", "7.3"),
        ("333.3333", "7.12345678"),
        ("-250", "6.5"),
        ("0", "7.9"),
        ("0.00005", "1.0"),
    ] {
        assert_eq!(
            fx_report::convert(Some(dec(fc)), Some(dec(rate))),
            Some(period_fx_reval::revalued_base_of(dec(fc), dec(rate))),
            "原币 {fc} × 汇率 {rate}：报表折合列与调汇重估额必须逐位相等"
        );
    }
    // 缺牌价两边都**不许**回落 rate = 1（报表留空、调汇 400，见场景四）
    assert_eq!(
        fx_report::convert(Some(dec("100")), None),
        None,
        "缺牌价的折算列必须留空，绝不按 1 折算"
    );
}

// ==================== 场景一：汇率变动 ====================

/// 汇率从 7.0 走到 7.3：**只动折算列**，不动原币、不动已过账凭证的本位币历史。
///
/// 口径出处：M1-07 余额表模块文档 ②/③「账面取全部已过账分录、汇率变动不改历史」。
#[tokio::test]
async fn rate_move_changes_only_conversion_not_booked_history() {
    let Some(_guard) = setup().await else {
        return;
    };
    post_fx("M110-A1", "1999-01-10", ACC_CASH, ACC_PAYABLE, "1000", "7.0").await;

    // ① 建单当期：账面 = 折算 = 7000，差额 0（汇率还没动）
    let jan = fx_balance_of(P1, Some("USD")).await;
    let cash = row_of(&jan, ACC_CASH, "USD");
    assert_eq!(cash.opening_fc, Decimal::ZERO, "1999-01 是首月：期初原币为 0");
    assert_eq!(cash.closing_fc, dec("1000"));
    assert_eq!(cash.closing_book, dec("7000"), "账面 = 原币 × 锁定汇率");
    assert_eq!(cash.close_rate, Some(dec("7.0")));
    assert_eq!(cash.converted_base, Some(dec("7000")));
    assert_eq!(cash.reval_diff, Some(Decimal::ZERO), "汇率未变 ⇒ 不出差额");
    let payable = row_of(&jan, ACC_PAYABLE, "USD");
    assert_eq!(payable.closing_fc, dec("-1000"), "负债侧同一套有符号口径（借方为正）");
    assert_eq!(payable.closing_book, dec("-7000"));

    // ② 汇率走到 7.3、本期没有新业务：原币不动、账面不动，只有折算列变
    let feb = fx_balance_of(P2, Some("USD")).await;
    let cash = row_of(&feb, ACC_CASH, "USD");
    assert_eq!(cash.opening_fc, dec("1000"), "期初原币");
    assert_eq!(cash.period_fc_debit, Decimal::ZERO, "本期没有外币发生额");
    assert_eq!(cash.period_fc_credit, Decimal::ZERO);
    assert_eq!(cash.closing_fc, dec("1000"), "汇率变动不改变原币头寸");
    assert_eq!(
        cash.closing_book,
        dec("7000"),
        "汇率变动不追溯改写已过账凭证的本位币历史（否则历史账会跟着今天的牌价漂）"
    );
    assert_eq!(cash.close_rate, Some(dec("7.3")));
    assert_eq!(cash.converted_base, Some(dec("7300")));
    assert_eq!(
        cash.reval_diff,
        Some(dec("300")),
        "差额 = 折算 7300 − 账面 7000，正数 = 借方调增（等调汇落账）"
    );
    let payable = row_of(&feb, ACC_PAYABLE, "USD");
    assert_eq!(payable.reval_diff, Some(dec("-300")), "负债侧镜像：贷方调增");

    // ③ 两行差额合计恰好为 0 —— 这**不代表**没有汇兑影响，只是一张外币凭证的借贷两侧
    //    用同一个汇率、必然互相抵消（见模块文档：出现非 0 合计说明有「只动一侧账面」的分录）
    assert_eq!(
        feb.total_reval_diff,
        Some(Decimal::ZERO),
        "同一凭证两侧同汇率 ⇒ 合计差额为 0；判断有无汇兑影响要看行，不看合计"
    );

    // ④ 凭证与分录本身没被改写（汇率仍 7.0、本位币金额仍 7000、原币仍 1000）
    let row = db()
        .query_first_sql(
            "SELECT exchange_rate, total_debit, currency_code FROM biz_voucher \
              WHERE source_type = 'M110' AND source_no = 'M110-A1' AND deleted_at IS NULL"
                .to_string(),
            vec![],
        )
        .await
        .expect("读夹具凭证")
        .expect("夹具凭证必须存在");
    assert_eq!(
        row.get_typed::<Decimal>("exchange_rate").expect("汇率"),
        dec("7.0"),
        "汇率变动不许回写历史凭证的锁定汇率"
    );
    assert_eq!(
        row.get_typed::<Decimal>("total_debit").expect("借方合计"),
        dec("7000"),
        "历史凭证的本位币金额必须停在入账那一天"
    );
    assert_eq!(row.get_typed::<String>("currency_code").expect("币种"), "USD");
    teardown().await;
}

// ==================== 场景二：期末调汇 ====================

/// 期末调汇：试算不落库 → 落账（差额逐科目 + 净额进汇兑损益）→ 幂等 → 差额归零、原币不动；
/// 次月汇率再变**只补本期这段**，不把历史累计重提一遍。
#[tokio::test]
async fn revaluation_posts_diff_then_next_month_only_tops_up() {
    let Some(_guard) = setup().await else {
        return;
    };
    post_fx("M110-B1", "1999-01-10", ACC_CASH, ACC_PAYABLE, "1000", "7.0").await;
    // 本位币分录：只改 1001 的账面（+50），不改原币 ⇒ 调汇 net ≠ 0（真实场景：人民币尾款）
    post_base("M110-B2", "1999-02-08", ACC_CASH, ACC_BANK, "50").await;

    // ① 试算：算得出来、但**一行都不落库**
    let dry = period_fx_reval::revalue(db(), P2, date(1999, 2, 28), true, None, Utc::now())
        .await
        .expect("试算必须成功");
    assert!(!dry.skipped, "首跑不该被幂等闸跳过");
    assert_eq!(dry.voucher_id, None, "试算不落账");
    assert_eq!(dry.base_currency, "CNY");
    let cash_row = dry
        .rows
        .iter()
        .find(|row| row.account_code == ACC_CASH)
        .expect("1001 必须有调汇行");
    assert_eq!(cash_row.fc_balance, dec("1000"));
    assert_eq!(cash_row.book_balance, dec("7050"), "账面含那笔本位币分录（7000 + 50）");
    assert_eq!(cash_row.close_rate, dec("7.3"));
    assert_eq!(cash_row.revalued_base, dec("7300"));
    assert_eq!(cash_row.diff, dec("250"), "7300 − 7050");
    let payable_row = dry
        .rows
        .iter()
        .find(|row| row.account_code == ACC_PAYABLE)
        .expect("2202 必须有调汇行");
    assert_eq!(payable_row.diff, dec("-300"), "负债侧：贷方调增 300");
    assert_eq!(
        dry.net,
        dec("-50"),
        "净额 = 250 − 300 = −50（汇兑损失）⇒ 汇兑损益科目走借方"
    );
    assert_eq!(reval_count(P2).await, 0, "试算不许落库");

    // ② 落账：一张本位币凭证（rate = 1、分录不带原币），差额逐科目 + 净额进 5603
    let posted = period_fx_reval::revalue(db(), P2, date(1999, 2, 28), false, None, Utc::now())
        .await
        .expect("落账必须成功");
    assert!(posted.voucher_id.is_some(), "落账必须给凭证 id");
    assert!(!posted.skipped);
    assert_eq!(reval_count(P2).await, 1);

    let header = reval_header(P2).await;
    assert_eq!(header.exchange_rate, dec("1"), "调汇凭证的汇率恒 1（它调的是本位币）");
    assert_eq!(header.currency_code, "CNY", "调汇凭证是**本位币**凭证");
    assert_eq!(header.status, voucher::STATUS_POSTED);
    assert_eq!(header.total_debit, dec("300"));
    assert_eq!(header.total_credit, dec("300"), "凭证必须借贷平衡");

    let entries = reval_entries(P2).await;
    assert_eq!(entries.len(), 3, "两个科目 + 一笔汇兑损益（净额非 0）");
    assert_eq!(entries[0].account_code, ACC_CASH);
    assert_eq!(entries[0].debit, dec("250"), "差额为正 ⇒ 借该科目");
    assert_eq!(entries[0].credit, Decimal::ZERO);
    assert_eq!(entries[1].account_code, ACC_PAYABLE);
    assert_eq!(entries[1].credit, dec("300"), "差额为负 ⇒ 贷该科目");
    assert_eq!(entries[2].account_code, voucher::ACCOUNT_FX_GAIN_LOSS);
    assert_eq!(entries[2].debit, dec("50"), "净损失 ⇒ 借 5603");
    assert!(
        entries.iter().all(|entry| entry.fc_null),
        "调汇凭证的分录不许带原币（带了就会被自己的调汇额污染下一期的原币余额）"
    );

    // ③ 幂等：同一期间重跑被跳过，凭证仍是 1 张
    let again = period_fx_reval::revalue(db(), P2, date(1999, 2, 28), false, None, Utc::now())
        .await
        .expect("重跑必须成功（是跳过，不是报错）");
    assert!(again.skipped, "同期已提过 ⇒ 幂等闸必须跳过");
    assert_eq!(
        again.voucher_id, posted.voucher_id,
        "跳过时回的是**同一张**凭证，不是新生成的"
    );
    assert_eq!(reval_count(P2).await, 1, "重跑不许产生第二张调汇凭证");

    // ④ 落账后：原币一分不动、账面追平折算、差额归零
    let feb = fx_balance_of(P2, Some("USD")).await;
    let cash = row_of(&feb, ACC_CASH, "USD");
    assert_eq!(
        cash.closing_fc,
        dec("1000"),
        "调汇凭证是本位币凭证，原币头寸不许被自己污染"
    );
    assert_eq!(cash.closing_book, dec("7300"), "7050 + 250 = 折算 7300");
    assert_eq!(cash.converted_base, Some(dec("7300")));
    assert_eq!(cash.reval_diff, Some(Decimal::ZERO), "调汇后差额归零");
    let payable = row_of(&feb, ACC_PAYABLE, "USD");
    assert_eq!(payable.closing_fc, dec("-1000"));
    assert_eq!(payable.closing_book, dec("-7300"), "−7000 − 300");
    assert_eq!(payable.reval_diff, Some(Decimal::ZERO));

    // ⑤ 次月汇率再走到 7.5：只补「7.3 → 7.5」这 200，不把历史累计（500）重提一遍
    let mar = fx_balance_of(P3, Some("USD")).await;
    let cash = row_of(&mar, ACC_CASH, "USD");
    assert_eq!(cash.closing_fc, dec("1000"));
    assert_eq!(cash.opening_book, dec("7300"), "期初账面 = 上期调汇后的账面");
    assert_eq!(cash.close_rate, Some(dec("7.5")));
    assert_eq!(cash.converted_base, Some(dec("7500")));
    assert_eq!(
        cash.reval_diff,
        Some(dec("200")),
        "本期差额 = 7500 − 7300 = 200；若把上期调汇凭证排除在账面外，这里会算成 500（重复计提）"
    );
    assert_eq!(row_of(&mar, ACC_PAYABLE, "USD").reval_diff, Some(dec("-200")));

    let staged = period_fx_reval::revalue(db(), P3, date(1999, 3, 31), false, None, Utc::now())
        .await
        .expect("次月调汇必须成功");
    assert_eq!(staged.net, Decimal::ZERO, "两侧镜像 ⇒ 净额 0");
    assert_eq!(reval_count(P3).await, 1);
    let entries = reval_entries(P3).await;
    assert_eq!(
        entries.len(),
        2,
        "净额为 0 ⇒ 不出汇兑损益行（0 金额分录会违反分录侧约束）"
    );
    assert_eq!(entries[0].debit, dec("200"), "只补本期这一段");
    assert_eq!(entries[1].credit, dec("200"));
    assert_eq!(reval_count(P2).await, 1, "上期那张凭证不许被动过");
    teardown().await;
}

// ==================== 场景三：报表折算 ====================

/// 报表折算：外币口径逐行折合 = 原币 × 期末牌价、勾稽在同口径成立，
/// 且**调汇落账后外币原币口径一分不动、本位币口径跟上**。
#[tokio::test]
async fn statement_conversion_follows_rate_and_ignores_revaluation_voucher() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 场景三要多记一个外币科目（应收 / 收入）：M1-04 的 66081 要求外币凭证的科目都标外币核算
    mark_fx(&[(ACC_AR, true), (ACC_REVENUE, true)]).await;
    post_fx("M110-C1", "1999-01-10", ACC_CASH, ACC_PAYABLE, "1000", "7.0").await;
    post_fx("M110-C2", "1999-02-05", ACC_AR, ACC_REVENUE, "200", "7.3").await;

    // ① 外币口径：只取 USD 凭证的原币，逐行折合 = 原币 × 7.3
    let report = balance_sheet(P2, Some("USD")).await;
    assert_eq!(report.currency.mode, fx_report::MODE_FOREIGN);
    assert_eq!(report.currency.currency, "USD");
    assert_eq!(report.currency.rate, Some(dec("7.3")), "期间末日 1999-02-28 的 SPOT");
    assert_eq!(report.unit, "USD", "外币口径的金额单位就是该币种");

    let cash = line_of(&report.assets, "MONETARY_FUNDS");
    assert_eq!(cash.amount, Some(dec("1000")));
    assert_eq!(cash.converted_amount, Some(dec("7300")), "1000 × 7.3");
    let ar = line_of(&report.assets, "ACCOUNTS_RECEIVABLE");
    assert_eq!(ar.amount, Some(dec("200")));
    assert_eq!(ar.converted_amount, Some(dec("1460")), "200 × 7.3");
    let ap = line_of(&report.liabilities, "ACCOUNTS_PAYABLE");
    assert_eq!(ap.amount, Some(dec("1000")), "负债侧按贷方正数出表");
    assert_eq!(ap.converted_amount, Some(dec("7300")));

    // ② 逐行恒等（对整张表，不止我点名的三行）：折合列 = round(原币 × 期末汇率, 4)
    for row in report
        .assets
        .iter()
        .chain(report.liabilities.iter())
        .chain(report.equity.iter())
    {
        if let (Some(amount), Some(converted)) = (row.amount, row.converted_amount) {
            assert_eq!(
                converted,
                (amount * dec("7.3")).round_dp(4),
                "报表项 {} 的折合列必须等于 原币 × 期末牌价",
                row.item_code
            );
        }
    }

    // ③ 勾稽在同口径内成立：外币凭证原币也平衡 ⇒ 资产 ≡ 负债 + 权益
    assert!(
        report.check.balanced,
        "USD 口径必须自己也平：{}（资产 {} / 负债+权益 {}）",
        report.check.message, report.check.assets_total, report.check.liability_equity_total
    );
    assert_eq!(report.check.assets_total, dec("1200"), "1000 + 200");
    assert_eq!(
        report.check.liability_equity_total,
        dec("1200"),
        "1000（应付）+ 200（净利润转权益）"
    );

    // ④ 利润表同一把尺子：收入 200 → 折合 1460
    let income = income_statement_of(P2, Some("USD")).await;
    let revenue = income
        .items
        .iter()
        .find(|row| row.item_code == income_statement::REVENUE)
        .expect("利润表必须有营业收入行");
    assert_eq!(revenue.current, dec("200"));
    assert_eq!(revenue.current_base, Some(dec("1460")));
    let net = income
        .items
        .iter()
        .find(|row| row.item_code == income_statement::NET_PROFIT)
        .expect("利润表必须有净利润行");
    assert_eq!(net.current, dec("200"));
    assert_eq!(net.current_base, Some(dec("1460")));

    // ⑤ 与外币余额表（M1-07）对拍：同一 (科目, 币种) 的原币期末必须相等
    let balance = fx_balance_of(P2, Some("USD")).await;
    assert_eq!(
        row_of(&balance, ACC_CASH, "USD").closing_fc,
        cash.amount.expect("1001 有金额"),
        "余额表 closing_fc 与报表外币口径取数必须是同一把尺子"
    );

    // ⑥ 调汇落账后：外币原币口径**一分不动**（调汇凭证不进外币口径），本位币口径跟上
    period_fx_reval::revalue(db(), P2, date(1999, 2, 28), false, None, Utc::now())
        .await
        .expect("场景三的调汇必须成功");
    let after = balance_sheet(P2, Some("USD")).await;
    let cash_after = line_of(&after.assets, "MONETARY_FUNDS");
    assert_eq!(
        cash_after.amount,
        Some(dec("1000")),
        "调汇凭证是本位币凭证：外币原币口径不许因为它多出 300"
    );
    assert_eq!(cash_after.converted_amount, Some(dec("7300")));
    let base_report = balance_sheet(P2, None).await;
    assert_eq!(base_report.currency.mode, fx_report::MODE_BASE);
    assert_eq!(
        line_of(&base_report.assets, "MONETARY_FUNDS").amount,
        Some(dec("7300")),
        "本位币口径跟着调汇走：7000 + 300"
    );
    assert_eq!(
        line_of(&base_report.assets, "MONETARY_FUNDS").converted_amount,
        None,
        "本位币口径不出折合列（金额本身就是本位币）"
    );
    assert!(
        base_report.check.balanced,
        "调汇后本位币口径仍必须平：{}",
        base_report.check.message
    );
    teardown().await;
}

// ==================== 场景四：缺牌价的两种正确反应 ====================

/// 缺期末牌价：报表**留空**（不回落 rate = 1），调汇**400 硬拒**（不静默少调一个科目）。
///
/// 这两条看起来矛盾，其实是同一件事的两种正确反应：报表只是展示，打不开比填错更糟；
/// 调汇要落账，错一分就进总账。
#[tokio::test]
async fn missing_rate_leaves_statement_blank_but_rejects_revaluation() {
    let Some(_guard) = setup().await else {
        return;
    };
    // 应收标外币核算、记一笔 EUR —— **故意不维护 EUR 牌价**
    mark_fx(&[(ACC_AR, true)]).await;
    let amount = dec("100");
    let rate_value = dec("8.0");
    let base = (amount * rate_value).round_dp(4);
    let (ar, ap) = (account_ref(ACC_AR).await, account_ref(ACC_PAYABLE).await);
    voucher::post_voucher_fx(
        db(),
        &PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_AUTO,
            source_type: WORD,
            source_id: None,
            source_no: Some("M110-D1"),
            source_doc_no: None,
            voucher_date: date(1999, 2, 10),
            summary: "M110 夹具 EUR 凭证".to_string(),
            lines: vec![
                line(1, "M110 EUR 借方".to_string(), ar, base, Decimal::ZERO),
                line(2, "M110 EUR 贷方".to_string(), ap, Decimal::ZERO, base),
            ],
            remark: Some("M1-10 缺牌价夹具"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        Some(&[
            FcAmount {
                debit: amount,
                credit: Decimal::ZERO,
            },
            FcAmount {
                debit: Decimal::ZERO,
                credit: amount,
            },
        ]),
        Some(("EUR", rate_value)),
        None,
        Utc::now(),
    )
    .await
    .expect("EUR 凭证落库");

    // ① 调汇：缺牌价 ⇒ 400 + 66082，点名币种与日期，且**不落任何凭证**
    let error = period_fx_reval::revalue(db(), P2, date(1999, 2, 28), false, None, Utc::now())
        .await
        .expect_err("缺期末牌价必须硬拒（少调一个科目 = 静默错账）");
    assert_eq!(error.status, 400);
    assert_eq!(error.code, error_code::ERP_FX_REVAL_RATE_MISSING);
    assert!(error.msg.contains("EUR"), "报文必须点名币种：{}", error.msg);
    assert!(
        error.msg.contains("1999-02-28"),
        "报文必须给出取汇率的日期：{}",
        error.msg
    );
    assert!(error.msg.contains("汇率"), "报文必须指路「币种 / 汇率」：{}", error.msg);
    assert_eq!(reval_count(P2).await, 0, "拒绝时不许留下半张凭证");

    // ② 外币余额表：折算列留空、rate_missing 点名，账面列照出
    let balance = fx_balance_of(P2, Some("EUR")).await;
    let ar_row = row_of(&balance, ACC_AR, "EUR");
    assert_eq!(ar_row.closing_fc, dec("100"), "原币头寸照出");
    assert_eq!(ar_row.closing_book, dec("800"), "账面列不受缺牌价影响");
    assert_eq!(ar_row.close_rate, None, "缺牌价不许回落 1");
    assert_eq!(ar_row.converted_base, None);
    assert_eq!(ar_row.reval_diff, None);
    assert!(ar_row.rate_missing);
    assert_eq!(balance.missing_rates, vec!["EUR".to_string()]);
    assert_eq!(balance.total_converted_base, None, "有币种缺牌价 ⇒ 合计也不许给半套数");

    // ③ 报表：原币列照出、折合列全空、依然出表（不 4xx、不崩）
    let report = balance_sheet(P2, Some("EUR")).await;
    assert_eq!(report.currency.mode, fx_report::MODE_FOREIGN);
    assert!(report.currency.rate_missing);
    assert_eq!(report.currency.rate, None);
    assert_eq!(report.unit, "EUR");
    assert_eq!(
        line_of(&report.assets, "ACCOUNTS_RECEIVABLE").amount,
        Some(dec("100")),
        "原币列照出"
    );
    assert_eq!(
        line_of(&report.assets, "ACCOUNTS_RECEIVABLE").converted_amount,
        None,
        "缺牌价 ⇒ 折合列 null（绝不 100 × 1）"
    );
    for row in report
        .assets
        .iter()
        .chain(report.liabilities.iter())
        .chain(report.equity.iter())
    {
        assert_eq!(
            row.converted_amount, None,
            "报表项 {} 在缺牌价时不许有任何折合值",
            row.item_code
        );
    }
    assert!(report.check.balanced, "缺牌价不影响原币勾稽：{}", report.check.message);
    assert_eq!(report.check.assets_total, dec("100"));
    assert_eq!(report.check.liability_equity_total, dec("100"));
    assert!(
        report.notes.iter().any(|text| text.contains("留空")),
        "缺牌价必须在附注里说清楚（否则看报表的人以为汇率是 1）"
    );
    teardown().await;
}

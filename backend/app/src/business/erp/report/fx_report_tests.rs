//! M1-09 真库用例：**报表取数按币种**（资产负债表 / 利润表）。
//!
//! 需要 `ERP_DB_URL`（99 上指向 145 的演练克隆库；145 上指 loopback）。未设置时打印 SKIP，
//! 但验收口径是「真跑」（卡面 §数据库约束）——CI / 汇报里必须贴出 pass 数。
//!
//! # 夹具口径（为什么这样造）
//!
//! ```text
//! 期间：2064-09 / 2064-10（本文件独占，别的卡用 2059/2060/2063）
//! 科目：全部用**标准科目**（1001 库存现金 / 1122 应收账款 / 2202 应付账款 /
//!       5001 主营业务收入 / 5602.02 管理费用-差旅费）—— 这样 biz_report_item 的科目映射能命中，
//!       报表行是「TOTAL_ASSETS / REVENUE / NET_PROFIT」这些真行，而不是未归类兜底行。
//!       （5602 本身非末级：末级才能记外币，所以用子科目 5602.02）
//! 汇率：USD 2064-09-01 = 7.0 / 2064-10-15..12-31 = 7.1（有限区间，见夹具注释）；
//!       EUR 故意**不录**（验「缺牌价 ⇒ 折合列留空」）。
//! 凭证：
//!   V1 2064-09-20  USD 200 @7.0  借 1001 / 贷 2202      ← 期初（上期）
//!   V2 2064-10-10  USD 100 @7.1  借 1122 / 贷 5001      ← 本期收入
//!   V3 2064-10-12  CNY  50       借 1122 / 贷 2202      ← **本位币凭证**，外币口径必须不算
//!   V4 2064-10-20  USD  30 @7.1  借 5602 / 贷 1001      ← 本期费用
//!   V5 2064-10-22  EUR  99 @8.0  借 1001 / 贷 2202      ← 另一个币种，USD 口径必须不算
//!   V6 2064-10-25  USD 999       **DRAFT**              ← 草稿不进报表
//! ```
//!
//! 收尾：`zz_teardown_purges_fixtures`（模块最后跑）按 `voucher_word` 清场并断言 0 ——
//! 连它自己的 `VOUCHER` 状态日志、本文件独占的 `2064-` 期间 / USD 牌价、以及历史孤儿一起收干净。
//! 只靠 `setup_fixture()` 开头那次清场是不够的：本模块最后一条用例造的东西会一直留到下一轮
//! （卡 `t_255fb28e`：跑完全量后库里常驻 6 张 `summary='M109 夹具'` 的凭证）。
//!
//! 于是 2064-10 的 USD 口径应当是：1001 = 170 / 1122 = 100 / 2202 = 200 /
//! 收入 100 / 管理费用 30 / 净利润 70，且**资产 270 ≡ 负债 200 + 权益 70**（真平）；
//! 本位币口径同一期间是另一组数（1001 = 1979 …），两者必须**不同**才证明「切币种真的换了取数」。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use std::str::FromStr;

use super::fx_report;
use super::{REPORT_CODE, REPORT_NAME, build_snapshot_of, into_response};
use crate::business::erp::income_statement;
use crate::business::erp::voucher::voucher_query::tests::{DB_LOCK, ensure_db};
use cloudpivot_kernel::shared::error_code;

const FIXTURE_WORD: &str = "ERPFX9";
const PERIOD: &str = "2064-10";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("十进制字面量")
}

/// 清场：删掉本文件造的全部夹具凭证（`voucher_word = 'ERPFX9'`，本文件独占）。
///
/// # 为什么先删状态日志、再删凭证行
///
/// `biz_status_log` **没有外键**（审计留痕表刻意不跟单据级联），凭证行一删，它写过的
/// `entity_id` 就再也查不到了 ⇒ 日志变孤儿（判据 `entity_type = 'VOUCHER' AND
/// entity_id NOT IN (SELECT id FROM biz_voucher)`），而全仓的孤儿扫描会一直把它们数进去。
/// 顺序反了会静默留孤儿（卡 `t_7938e24a` 的同类根因）。
///
/// 返回删掉的**凭证行数**（状态日志条数打印出来，方便对照）。
async fn purge_fixtures() -> i64 {
    let logs_sql = format!(
        "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' \
          AND entity_id IN (SELECT id FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}')"
    );
    let logs = db()
        .execute_with_params(logs_sql.as_str(), vec![])
        .await
        .unwrap_or_else(|error| panic!("清场失败（删夹具凭证的 VOUCHER 状态日志）：{error}"));
    let vouchers_sql = format!("DELETE FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'");
    let vouchers = db()
        .execute_with_params(vouchers_sql.as_str(), vec![])
        .await
        .unwrap_or_else(|error| panic!("清场失败（删夹具凭证）：{error}"));
    let (logs, vouchers) = (i64::try_from(logs).unwrap_or(0), i64::try_from(vouchers).unwrap_or(0));
    if vouchers > 0 || logs > 0 {
        println!("[M1-09] 清场：删夹具凭证 {vouchers} 张、其 VOUCHER 状态日志 {logs} 条");
    }
    vouchers
}

/// 本文件夹具凭证的条数（按 `voucher_word` —— 只用这个词定位，不按单号前缀猜）。
async fn fixture_vouchers() -> i64 {
    let sql = format!("SELECT count(*) FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}'");
    db().scalar(sql.as_str())
        .await
        .unwrap_or_else(|error| panic!("统计夹具凭证失败：{error}"))
}

/// 本文件夹具凭证的 id（清场前抓下来，清场后靠它们数状态日志 —— 凭证一删就查不到 id 了）。
async fn fixture_voucher_ids() -> Vec<Uuid> {
    let sql = format!("SELECT id FROM biz_voucher WHERE voucher_word = '{FIXTURE_WORD}' ORDER BY voucher_no");
    let rows = db()
        .find_with_params(sql.as_str(), vec![])
        .await
        .unwrap_or_else(|error| panic!("读夹具凭证 id 失败：{error}"));
    rows.iter()
        .map(|row| {
            row.get_typed("id")
                .unwrap_or_else(|error| panic!("夹具凭证 id：{error}"))
        })
        .collect()
}

/// 这些凭证 id 上的 `VOUCHER` 状态日志条数（清场后必须是 0）。
async fn status_logs_of(ids: &[Uuid]) -> i64 {
    if ids.is_empty() {
        return 0;
    }
    let list = ids
        .iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT count(*) FROM biz_status_log \
          WHERE entity_type = 'VOUCHER' AND entity_id IN ({list})"
    );
    db().scalar(sql.as_str())
        .await
        .unwrap_or_else(|error| panic!("统计夹具凭证状态日志失败：{error}"))
}

/// 建夹具：期间 / 汇率 / 科目标记 / 六张凭证（其中一张草稿）。
async fn setup_fixture() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    purge_fixtures().await;
    let statements = [
        // 牌价：USD / EUR / HKD → CNY 的行整体删掉再种（**只动这三对**）。
        //
        // 为什么不能只删「2064 年及以后」：模板库里 USD→CNY 有一条 `effective_to IS NULL`
        // 的行（= 区间铺到无穷），PG 的 `ex_biz_exchange_rate_period` 排除约束会把它和
        // 本用例 2064 年的行判成**重叠**（本机 PG 18.6 实测：`[2064-09-01,2064-10-15)`
        // 与 `[2026-11-15,∞)` 的 `&&` 返回 true，纯函数算也如此），插入直接报冲突。
        // 所以这三对在本库里由本用例独占：先清空，再种 2064 年内的有限区间。
        "DELETE FROM biz_exchange_rate \
          WHERE from_currency IN ('USD', 'EUR', 'HKD') AND to_currency = 'CNY'"
            .to_string(),
        "DELETE FROM biz_accounting_period WHERE period_code LIKE '2064-%'".to_string(),
        // USD 两条牌价（V0407 的生效区间：早行的 effective_to = 下一条生效日 − 1；
        // 末行**必须给有限末日**——V0407 有「区间不重叠」排除约束，模板库自带
        // `[2026-11-15, ∞)` 的 USD 行，末行写 NULL 就等于把区间铺到无穷 ⇒ 与它冲突）
        "INSERT INTO biz_exchange_rate (from_currency, to_currency, rate_type, effective_date, effective_to, rate) \
         VALUES ('USD', 'CNY', 'SPOT', DATE '2064-09-01', DATE '2064-10-14', 7.0), \
                ('USD', 'CNY', 'SPOT', DATE '2064-10-15', DATE '2064-12-31', 7.1)"
            .to_string(),
        // 外币余额表（M1-07）只取 is_foreign_currency = 1 的科目 ⇒ 交叉对拍要这两个标记。
        // 只标**末级**科目（V0408 的 `ck_biz_account_fc_leaf`：非末级 5602 会被拒）。
        "UPDATE biz_account SET is_foreign_currency = 1 \
          WHERE account_code IN ('1001', '1122', '2202')"
            .to_string(),
        // 断言前提（fail-closed）：2064 年这两个期间必须是「干净」的
        "SELECT 1".to_string(),
    ];
    for sql in &statements {
        db().execute_with_params(sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("夹具 SQL 失败：{error}\n{sql}"));
    }
    // 期间用统一夹具建（手写主数据 INSERT 会撞 `test_seed_gate` 的预算）
    crate::test_seed::ensure_accounting_period(2064, 9).await;
    crate::test_seed::ensure_accounting_period(2064, 10).await;
    db().execute_with_params(
        "UPDATE biz_accounting_period SET status = 'OPEN', closed_at = NULL, closed_by = NULL \
          WHERE period_code IN ('2064-09', '2064-10')",
        vec![],
    )
    .await
    .unwrap_or_else(|error| panic!("夹具：重置 2064 期间状态失败：{error}"));
    let stray: i64 = db()
        .scalar("SELECT count(*) FROM biz_voucher WHERE voucher_date >= DATE '2064-01-01' AND voucher_date < DATE '2065-01-01' AND voucher_word <> 'ERPFX9'")
        .await
        .unwrap_or_else(|error| panic!("夹具自检失败：{error}"));
    assert_eq!(
        stray, 0,
        "2064 年被别的夹具占了（{stray} 张非本用例外凭证），本用例的期望值会失真"
    );
    Some(guard)
}

/// 一张夹具凭证的要素（8 个位置参数会被 `clippy::too_many_arguments` 拦下，用结构体表达更清楚）。
struct VoucherSpec<'a> {
    /// 凭证号。
    no: &'a str,
    /// 凭证日期（`period` 由日期前 7 位推出）。
    date: &'a str,
    /// 凭证币种（`CNY` = 本位币凭证，分录原币列全 NULL）。
    currency: &'a str,
    /// 建单时锁定的汇率（本位币凭证填 `1`）。
    rate: &'a str,
    /// 借方科目编码。
    debit: &'a str,
    /// 贷方科目编码。
    credit: &'a str,
    /// 原币金额（借贷两条同额）。
    fc_amount: &'a str,
    /// 凭证状态（`POSTED` / `DRAFT`）。
    status: &'a str,
}

/// 建一张凭证（币种 = `spec.currency`、汇率 = `spec.rate`）。
///
/// 借贷两条分录，金额一律给「原币」，本位币列 = 原币 × rate（与 M1-04 的落库口径一致）。
async fn post_voucher(spec: VoucherSpec<'_>) {
    let VoucherSpec {
        no,
        date,
        currency,
        rate,
        debit,
        credit,
        fc_amount,
        status,
    } = spec;
    let debit_code = debit;
    let credit_code = credit;
    let amount = dec(fc_amount);
    let rate_value = dec(rate);
    let base = (amount * rate_value).round_dp(4);
    // 原币列：外币凭证「借方行 = (原币, 0) / 贷方行 = (0, 原币)」；本位币凭证**两列全 NULL**
    // （V0409 的 `ck_biz_voucher_entry_fc_side` 要求成对：NULL/0 混写会被拒）。
    let (debit_fc_debit, debit_fc_credit, credit_fc_debit, credit_fc_credit) = if currency == "CNY" {
        // 裸 `NULL` 在 SELECT 投影里是 text 型（PG 报 "column fc_debit_amount is of type
        // numeric but expression is of type text"），必须显式 `::numeric`
        let null = "NULL::numeric".to_string();
        (null.clone(), null.clone(), null.clone(), null)
    } else {
        (
            format!("{amount}"),
            "0".to_string(),
            "0".to_string(),
            format!("{amount}"),
        )
    };
    let sql = format!(
        "WITH v AS ( \
            INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, summary, \
                                     total_debit, total_credit, entry_count, status, currency_code, exchange_rate) \
            VALUES ('{FIXTURE_WORD}', '{no}', '{period_of}', DATE '{date}', 'M109 夹具', \
                    {base}, {base}, 2, '{status}', '{currency}', {rate_value}) RETURNING id \
         ), d AS ( \
            SELECT id, account_code, account_name FROM biz_account \
             WHERE account_code = '{debit_code}' AND deleted_at IS NULL \
         ), c AS ( \
            SELECT id, account_code, account_name FROM biz_account \
             WHERE account_code = '{credit_code}' AND deleted_at IS NULL \
         ) \
         INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, \
                                        account_name, debit_amount, credit_amount, \
                                        fc_debit_amount, fc_credit_amount) \
         SELECT v.id, 1, 'M109 借方', d.id, d.account_code, d.account_name, {base}, 0, \
                {debit_fc_debit}, {debit_fc_credit} FROM v, d \
         UNION ALL \
         SELECT v.id, 2, 'M109 贷方', c.id, c.account_code, c.account_name, 0, {base}, \
                {credit_fc_debit}, {credit_fc_credit} FROM v, c",
        period_of = &date[0..7],
    );
    db().execute_with_params(&sql, vec![])
        .await
        .unwrap_or_else(|error| panic!("建凭证 {no} 失败：{error}\n{sql}"));
}

/// 六张凭证（见模块文档的夹具表）。
async fn seed_vouchers() {
    post_voucher(VoucherSpec {
        no: "9001",
        date: "2064-09-20",
        currency: "USD",
        rate: "7.0",
        debit: "1001",
        credit: "2202",
        fc_amount: "200",
        status: "POSTED",
    })
    .await;
    post_voucher(VoucherSpec {
        no: "9002",
        date: "2064-10-10",
        currency: "USD",
        rate: "7.1",
        debit: "1122",
        credit: "5001",
        fc_amount: "100",
        status: "POSTED",
    })
    .await;
    post_voucher(VoucherSpec {
        no: "9003",
        date: "2064-10-12",
        currency: "CNY",
        rate: "1",
        debit: "1122",
        credit: "2202",
        fc_amount: "50",
        status: "POSTED",
    })
    .await;
    post_voucher(VoucherSpec {
        no: "9004",
        date: "2064-10-20",
        currency: "USD",
        rate: "7.1",
        debit: "5602.02",
        credit: "1001",
        fc_amount: "30",
        status: "POSTED",
    })
    .await;
    // EUR 的牌价故意不录（rate 只是落库用，报表不查它）
    post_voucher(VoucherSpec {
        no: "9005",
        date: "2064-10-22",
        currency: "EUR",
        rate: "8.0",
        debit: "1001",
        credit: "2202",
        fc_amount: "99",
        status: "POSTED",
    })
    .await;
    post_voucher(VoucherSpec {
        no: "9006",
        date: "2064-10-25",
        currency: "USD",
        rate: "7.1",
        debit: "1001",
        credit: "2202",
        fc_amount: "999",
        status: "DRAFT",
    })
    .await;
}

/// 在 REPEATABLE READ 事务里跑一次（与接口层同一条路径），返回报表 + 币种块。
async fn run_balance_sheet(currency: Option<&str>) -> (super::BalanceSheetRes, fx_report::ReportBasis) {
    let basis = fx_report::resolve(currency)
        .await
        .expect("解析报表币种（夹具里 USD/EUR 都在币种表）");
    let tx = db().begin().await.expect("开启事务");
    tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .await
        .expect("设置隔离级别");
    let snapshot = build_snapshot_of(&tx, PERIOD, basis.fx_currency())
        .await
        .expect("编制资产负债表");
    let block = fx_report::describe(&basis, snapshot.period.start_date, snapshot.period.end_date)
        .await
        .expect("组装币种块");
    tx.commit().await.expect("提交");
    let mut report = into_response(REPORT_CODE, REPORT_NAME, "演练公司", "元", snapshot);
    fx_report::apply_to_balance_sheet(&mut report, block);
    (report, basis)
}

/// 在一份报表的某一侧找到某报表项。
fn line_of<'a>(lines: &'a [super::ReportLineRes], code: &str) -> &'a super::ReportLineRes {
    lines
        .iter()
        .find(|row| row.item_code == code)
        .unwrap_or_else(|| panic!("报表里没有 {code} 项"))
}

/// 某报表项的金额（标题行按 0）。
fn amount_of(lines: &[super::ReportLineRes], code: &str) -> Decimal {
    line_of(lines, code).amount.unwrap_or_default()
}

/// ⑨-① USD 口径：只取 USD 凭证的原币，草稿 / 本位币凭证 / 其它币种都不进表。
///
/// # 为什么断言都写成「增量」
///
/// 演练库是**共享**的：别的报表用例（ERPR1/R2）也在 1001 / 1122 / 2202 上记凭证，
/// 且它们的日期早于 2064 ⇒ 会进本用例的「期初」。所以这里先跑一次基线、再种凭证、
/// 再跑一次，断言的是**本用例这六张凭证带来的增量**（170 / 200 / 100 / 270 …）。
/// 库里有别人的历史数据时不会假红，而取数口径一旦错（把本位币凭证 / 草稿 / 别的币种算进来）
/// 增量立刻不等于期望值。
#[tokio::test]
async fn fx_report_usd_balance_sheet_takes_only_usd_entries() {
    let Some(_guard) = setup_fixture().await else {
        return;
    };
    let (before, _) = run_balance_sheet(Some("USD")).await;
    let base = [
        amount_of(&before.assets, "MONETARY_FUNDS"),
        line_of(&before.assets, "MONETARY_FUNDS")
            .previous_amount
            .unwrap_or_default(),
        amount_of(&before.assets, "ACCOUNTS_RECEIVABLE"),
        amount_of(&before.liabilities, "ACCOUNTS_PAYABLE"),
        before.check.assets_total,
        before.check.liability_equity_total,
    ];

    seed_vouchers().await;

    let (report, basis) = run_balance_sheet(Some("USD")).await;
    assert_eq!(basis.currency, "USD");
    assert_eq!(
        report.currency.mode,
        fx_report::MODE_FOREIGN,
        "选了外币必须是 FOREIGN 口径"
    );
    assert_eq!(
        report.currency.rate,
        Some(dec("7.1")),
        "期间末日（2064-10-31）取 10-15 起的 7.1"
    );
    assert_eq!(report.unit, "USD", "外币口径的金额单位就是该币种");

    // 1001 库存现金：期初 200 − 本期 30 = 170（草稿 999、EUR 99 都不许进）
    let cash = amount_of(&report.assets, "MONETARY_FUNDS");
    assert_eq!(
        cash - base[0],
        dec("170"),
        "1001 原币期末增量：USD 200（上期）− 30（本期）= 170；草稿 / EUR 凭证都不许混进来"
    );
    assert_eq!(
        line_of(&report.assets, "MONETARY_FUNDS")
            .previous_amount
            .unwrap_or_default()
            - base[1],
        dec("200"),
        "上期对比也必须是同一币种口径（2064-09 的 200 USD）"
    );
    // 1122 应收账款：本期 100（本位币凭证的 50 不算）
    assert_eq!(
        amount_of(&report.assets, "ACCOUNTS_RECEIVABLE") - base[2],
        dec("100"),
        "1122 原币：只算 USD 的 100（本位币凭证的 50 不进外币口径）"
    );
    // 2202 应付账款：上期 200（贷方为正）
    assert_eq!(
        amount_of(&report.liabilities, "ACCOUNTS_PAYABLE") - base[3],
        dec("200"),
        "2202 原币（贷方为正）：上期 200"
    );

    // 折合列 = 原币 × 7.1（四舍五入到 4 位）——逐行恒等，与基线无关
    assert_eq!(
        line_of(&report.assets, "MONETARY_FUNDS").converted_amount,
        Some((cash * dec("7.1")).round_dp(4)),
        "折合列必须逐行等于 原币 × 期末汇率"
    );
    assert!(
        line_of(&report.assets, "MONETARY_FUNDS")
            .previous_converted_amount
            .is_some(),
        "上期对比也必须有折合值（上期与本期同一把尺子）"
    );

    // 勾稽：外币凭证原币也平衡 ⇒ 资产 ≡ 负债 + 权益（差 0）
    assert!(
        report.check.balanced,
        "USD 口径必须自己也平：{}（资产 {} / 负债+权益 {}）",
        report.check.message, report.check.assets_total, report.check.liability_equity_total
    );
    assert_eq!(
        report.check.assets_total - base[4],
        dec("270"),
        "资产侧增量 = 170 + 100"
    );
    assert_eq!(
        report.check.liability_equity_total - base[5],
        dec("270"),
        "负债+权益侧增量 = 200（应付）+ 70（利润）"
    );
    assert_eq!(
        report.currency.entry_count, 4,
        "2064-10 的 USD 已过账分录 4 条（V2 两条 + V4 两条）"
    );
    assert!(
        report.notes.iter().any(|text| text.contains("原币口径")),
        "外币口径必须带口径附注（否则看报表的人以为是本位币）"
    );
}

/// ⑨-② 本位币口径：与改动前逐字一致（不传 currency），且数字与 USD 口径**不同**。
#[tokio::test]
async fn fx_report_base_mode_keeps_previous_behaviour() {
    let Some(_guard) = setup_fixture().await else {
        return;
    };
    let (before, _) = run_balance_sheet(None).await;
    let base_cash = amount_of(&before.assets, "MONETARY_FUNDS");

    seed_vouchers().await;

    let (report, basis) = run_balance_sheet(None).await;
    assert!(!basis.is_foreign());
    assert_eq!(report.currency.mode, fx_report::MODE_BASE);
    assert_eq!(report.currency.rate, Some(Decimal::ONE), "本位币口径汇率恒 1");
    assert_eq!(report.unit, "元");

    // 本位币口径增量：1001 = 200×7.0 − 30×7.1 + 99×8.0 = 1400 − 213 + 792 = 1979
    //（外币凭证按**本位币金额**算，与本位币凭证一起；这是切换币种前的原口径，必须零变化）
    assert_eq!(
        amount_of(&report.assets, "MONETARY_FUNDS") - base_cash,
        dec("1979"),
        "本位币口径必须把外币凭证按本位币金额、与本位币凭证一起算（1400−213+792）"
    );
    assert_eq!(
        line_of(&report.assets, "MONETARY_FUNDS").converted_amount,
        None,
        "本位币口径不出折合列（金额本身就是本位币）"
    );

    // 同一期间两种口径必须不同 —— 这就是「切币种真的换了取数」的判据
    let (foreign, _) = run_balance_sheet(Some("USD")).await;
    assert_ne!(
        amount_of(&report.assets, "MONETARY_FUNDS"),
        amount_of(&foreign.assets, "MONETARY_FUNDS"),
        "本位币 1979 与 USD 170 必须不同（相同就说明币种没生效）"
    );
}

/// ⑨-③ 缺期末牌价：原币列照出、折合列留空、`rate_missing` 点名（绝不按 1 折算）。
#[tokio::test]
async fn fx_report_missing_rate_leaves_converted_null() {
    let Some(_guard) = setup_fixture().await else {
        return;
    };
    let (before, _) = run_balance_sheet(Some("EUR")).await;
    let base_cash = amount_of(&before.assets, "MONETARY_FUNDS");

    seed_vouchers().await;

    let (report, _) = run_balance_sheet(Some("EUR")).await;
    assert_eq!(report.currency.mode, fx_report::MODE_FOREIGN);
    assert!(report.currency.rate_missing, "EUR 没录牌价 ⇒ rate_missing");
    assert_eq!(report.currency.rate, None, "缺牌价必须留空（不许回落 1）");
    assert_eq!(
        amount_of(&report.assets, "MONETARY_FUNDS") - base_cash,
        dec("99"),
        "原币列照出（只算 EUR 的 99）"
    );
    assert_eq!(
        line_of(&report.assets, "MONETARY_FUNDS").converted_amount,
        None,
        "缺牌价 ⇒ 折合列 null（绝不 99 × 1）"
    );
    assert!(
        report.notes.iter().any(|text| text.contains("留空")),
        "缺牌价必须在附注里说清楚"
    );
    assert!(
        report.currency.entry_count > 0,
        "EUR 本期有 2 条分录（不能是「没业务」）"
    );
}

/// 在 REPEATABLE READ 事务里跑一次利润表（与接口层同一条路径）。
async fn run_income(currency: Option<&str>) -> income_statement::IncomeStatementRes {
    let basis = fx_report::resolve(currency).await.expect("解析报表币种");
    let tx = db().begin().await.expect("开启事务");
    tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .await
        .expect("设置隔离级别");
    let snapshot = income_statement::build_snapshot_of(&tx, PERIOD, basis.fx_currency())
        .await
        .expect("编制利润表");
    let block = fx_report::describe(&basis, snapshot.period.start_date, snapshot.period.end_date)
        .await
        .expect("组装币种块");
    tx.commit().await.expect("提交");
    let mut report = income_statement::into_response("演练公司", "元", snapshot);
    fx_report::apply_to_income(&mut report, block);
    report
}

/// ⑨-④ 利润表按币种：本期 / 本年累计都是原币，且勾稽（净利润 ≡ 未分配利润变动）在同口径成立。
#[tokio::test]
async fn fx_report_income_statement_by_currency() {
    let Some(_guard) = setup_fixture().await else {
        return;
    };
    // 同资产负债表：断言用增量（别的用例的损益凭证也在 5001 / 5602.* 上）
    let before = run_income(Some("USD")).await;
    let pick = |report: &income_statement::IncomeStatementRes,
                code: &str,
                field: fn(&income_statement::IncomeLineRes) -> Decimal| {
        report
            .items
            .iter()
            .find(|row| row.item_code == code)
            .map(field)
            .unwrap_or_else(|| panic!("利润表里没有 {code} 行"))
    };
    let base_revenue = pick(&before, income_statement::REVENUE, |row| row.current);
    let base_expense = pick(&before, "ADMIN_EXPENSE", |row| row.current);
    let base_net = pick(&before, income_statement::NET_PROFIT, |row| row.current);
    let base_net_ytd = before.check.net_profit_ytd;
    let base_equity = before.check.equity_change;

    seed_vouchers().await;

    let report = run_income(Some("USD")).await;
    assert_eq!(report.currency.mode, fx_report::MODE_FOREIGN);

    let revenue = report
        .items
        .iter()
        .find(|row| row.item_code == income_statement::REVENUE)
        .expect("利润表必须有营业收入行");
    assert_eq!(revenue.current - base_revenue, dec("100"), "5001 本期原币增量 100");
    assert_eq!(
        revenue.ytd - base_revenue,
        dec("100"),
        "本年累计同口径（同期只有这一笔）"
    );
    assert_eq!(
        revenue.current_base,
        Some((revenue.current * dec("7.1")).round_dp(4)),
        "折合列 = 本期原币 × 期末汇率"
    );

    let expense = report
        .items
        .iter()
        .find(|row| row.item_code == "ADMIN_EXPENSE")
        .expect("利润表必须有管理费用行");
    assert_eq!(
        expense.current - base_expense,
        dec("30"),
        "5602.02 本期原币增量 30（跨科目汇到「管理费用」行）"
    );

    let net = report
        .items
        .iter()
        .find(|row| row.item_code == income_statement::NET_PROFIT)
        .expect("利润表必须有净利润行");
    assert_eq!(net.current - base_net, dec("70"), "净利润增量 = 100 − 30");
    assert!(
        report.check.balanced,
        "外币口径的利润表也必须勾稽通过：{}",
        report.check.message
    );
    assert_eq!(
        report.check.net_profit_ytd - base_net_ytd,
        dec("70"),
        "净利润（本年累计）增量"
    );
    assert_eq!(
        report.check.equity_change - base_equity,
        dec("70"),
        "外币口径下「未分配利润本年变动」必须与净利润同增量（同口径勾稽）"
    );
}

/// ⑨-⑤ 与外币余额表（M1-07）对拍：某 (科目, 币种) 的原币期末必须相等。
///
/// 这是 M1-08 交接的死口径：两张表用同一把尺子（只取该币种凭证、`COALESCE(fc_*, 金额)`），
/// 一旦谁改了取数，这条断言立刻红。
#[tokio::test]
async fn fx_report_closing_matches_fx_balance() {
    let Some(_guard) = setup_fixture().await else {
        return;
    };
    let cash_of = |accounts: &[super::AccountBalance]| {
        let row = accounts
            .iter()
            .find(|row| row.account_code == "1001")
            .expect("1001 必须在科目表里");
        row.net_before + row.net_period
    };

    // 基线（别人的历史凭证也算在「期初」里，所以先量一次）
    let tx = db().begin().await.expect("开启事务");
    let before = super::load_accounts_of(
        &tx,
        NaiveDate::from_ymd_opt(2064, 10, 1).expect("日期"),
        NaiveDate::from_ymd_opt(2064, 10, 31).expect("日期"),
        Some(PERIOD),
        Some("USD"),
    )
    .await
    .expect("按币种取科目余额");
    tx.commit().await.expect("提交");

    seed_vouchers().await;

    // 报表侧：外币口径下 1001 的原币期末
    let tx = db().begin().await.expect("开启事务");
    let accounts = super::load_accounts_of(
        &tx,
        NaiveDate::from_ymd_opt(2064, 10, 1).expect("日期"),
        NaiveDate::from_ymd_opt(2064, 10, 31).expect("日期"),
        Some(PERIOD),
        Some("USD"),
    )
    .await
    .expect("按币种取科目余额");
    tx.commit().await.expect("提交");
    let cash_closing = cash_of(&accounts);
    assert_eq!(
        cash_closing - cash_of(&before),
        dec("170"),
        "USD 口径下 1001 期末原币增量 = 200 − 30"
    );

    // 外币余额表（M1-07）侧：同 (科目, 币种) 的 closing_fc
    let balance = crate::business::erp::fx_balance::fx_balance(&crate::business::erp::fx_balance::FxBalanceQuery {
        period: Some(PERIOD.to_string()),
        currency: Some("USD".to_string()),
    })
    .await
    .expect("外币余额表");
    let row = balance
        .rows
        .iter()
        .find(|row| row.account_code == "1001" && row.currency_code == "USD")
        .unwrap_or_else(|| {
            panic!(
                "外币余额表里必须有 (1001, USD) 行；实际行：{:?}",
                balance
                    .rows
                    .iter()
                    .map(|row| format!("{}/{}", row.account_code, row.currency_code))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(
        row.closing_fc, cash_closing,
        "M1-07 外币余额表 closing_fc 与 M1-09 外币口径报表取数必须相等（同一把尺子）"
    );
}

/// ⑨-⑥ 币种入参的三条分支：本位币 ⇒ BASE 200；格式非法 ⇒ 400 + 10001；主数据没有 ⇒ 400 + 66084。
#[tokio::test]
async fn fx_report_currency_param_validation() {
    let Some(_guard) = setup_fixture().await else {
        return;
    };

    // 传本位币 = 切回本位币，不是错误
    let base = fx_report::resolve(Some("CNY")).await.expect("本位币必须 200");
    assert!(!base.is_foreign());
    assert_eq!(base.fx_currency(), None);

    // 小写也认（normalize_currency 转大写）
    let lower = fx_report::resolve(Some(" usd ")).await.expect("小写 / 空格要规范化");
    assert_eq!(lower.currency, "USD");
    assert!(lower.is_foreign());

    // 格式非法 → 400 + 10001（不静默忽略，否则用户以为切了币种）
    let bad = fx_report::resolve(Some("usd$")).await.expect_err("格式非法必须报错");
    assert_eq!(bad.status, 400);
    assert_eq!(bad.code, error_code::PARAM_INVALID);

    // 格式合法但主数据没有 → 400 + 66084（指路「币种 / 汇率」）
    let unknown = fx_report::resolve(Some("XYZ")).await.expect_err("未知币种必须报错");
    assert_eq!(unknown.status, 400);
    assert_eq!(unknown.code, error_code::ERP_REPORT_CURRENCY_INVALID);
    assert!(unknown.msg.contains("币种"), "报文要点名去哪儿维护：{}", unknown.msg);
}

/// 收尾清场（`zz_` 排在本模块最后跑）：本文件夹具凭证必须一张不留。
///
/// # 为什么 `setup_fixture()` 里清不够
///
/// `setup_fixture()` 的清场只对**本模块的下一条用例**生效。本模块最后一条跑完，最后造的
/// 那 6 张凭证（`9001~9005 POSTED` + `9006 DRAFT`，`summary='M109 夹具'`）就一直躺在库里，
/// 直到下一次有人跑本模块 —— 于是「全量跑完净增量 = 0」这类统计永远非 0（卡 `t_255fb28e`）。
///
/// # 为什么还要给凭证补一条状态日志
///
/// 夹具是用裸 SQL 灌凭证的，**自己不写** `biz_status_log`。只断言「日志 0 条」等于断言一个
/// 空集合 ⇒ 假绿。所以这里显式补上「凭证落库时会写的那条日志」，让
/// [`purge_fixtures`] 的「先删日志、再删凭证」那条腿真的有牙：顺序反了 / 漏删日志，
/// 这条断言当场红。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup_fixture().await else {
        return;
    };
    seed_vouchers().await;
    let ids = fixture_voucher_ids().await;
    assert_eq!(ids.len(), 6, "清场前必须先把夹具造出来（否则「删了个寂寞」也会绿）");
    let logs_sql = format!(
        "INSERT INTO biz_status_log \
            (entity_type, entity_id, entity_no, from_status, to_status, operator_name, created_at) \
         SELECT 'VOUCHER', v.id, v.voucher_no, NULL, v.status, 'M109 夹具', NOW() \
           FROM biz_voucher v WHERE v.voucher_word = '{FIXTURE_WORD}'"
    );
    db().execute_with_params(logs_sql.as_str(), vec![])
        .await
        .unwrap_or_else(|error| panic!("造夹具凭证的 VOUCHER 状态日志失败：{error}"));
    assert_eq!(
        status_logs_of(&ids).await,
        6,
        "清场前 6 张夹具凭证都该带上一条状态日志（这是「先删日志」那条腿的牙齿）"
    );

    let deleted = purge_fixtures().await;
    assert_eq!(deleted, 6, "清场必须删掉 6 张夹具凭证");
    assert_eq!(
        fixture_vouchers().await,
        0,
        "清场后按 voucher_word = '{FIXTURE_WORD}' 数必须 0 张（跑完全量库里不许常驻 M109 凭证）"
    );
    assert_eq!(
        status_logs_of(&ids).await,
        0,
        "清场后这 {} 个 id 上的 VOUCHER 状态日志必须 0 条（日志没有外键，必须先删）",
        ids.len()
    );

    // 兜历史孤儿：修前每一轮 `setup_fixture()` 删掉上一轮凭证后，那些凭证的状态日志就成了孤儿，
    // 永远没有模块会回来收（`entity_id NOT IN (SELECT id FROM biz_voucher)`）。
    let cleaned = crate::business::erp::fixture_hygiene::purge_orphans(&["VOUCHER"]).await;
    let left = crate::business::erp::fixture_hygiene::orphans(&["VOUCHER"]).await;
    assert_eq!(
        left, 0,
        "VOUCHER 孤儿状态日志必须收干（本轮清了 {cleaned} 条，还剩 {left} 条）"
    );

    // 本文件**独占**的另外两样夹具（`setup_fixture()` 每轮开头都整体重建，见其注释）：
    // 2064-09 / 2064-10 期间与 2064 年的 USD 牌价。不收的话「全量跑完净增量 = 0」仍然非 0。
    let period_sql = "DELETE FROM biz_accounting_period WHERE period_code LIKE '2064-%'";
    let rate_sql = "DELETE FROM biz_exchange_rate \
                     WHERE from_currency IN ('USD', 'EUR', 'HKD') AND to_currency = 'CNY' \
                       AND effective_date >= DATE '2064-01-01' AND effective_date < DATE '2065-01-01'";
    for sql in [period_sql, rate_sql] {
        db().execute_with_params(sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("清场失败：{error}\nSQL: {sql}"));
    }
    let residue: i64 = db()
        .scalar(
            "SELECT (SELECT count(*) FROM biz_accounting_period WHERE period_code LIKE '2064-%') \
                  + (SELECT count(*) FROM biz_exchange_rate \
                      WHERE from_currency IN ('USD', 'EUR', 'HKD') AND to_currency = 'CNY' \
                        AND effective_date >= DATE '2064-01-01' AND effective_date < DATE '2065-01-01')",
        )
        .await
        .unwrap_or_else(|error| panic!("清场核对失败：{error}"));
    assert_eq!(residue, 0, "2064 期间 / USD 牌价夹具残留必须为 0");
    println!(
        "[M1-09] 清场：M109 夹具凭证 0 张、其状态日志 0 条、VOUCHER 历史孤儿已收 {cleaned} 条、2064 期间与牌价 0 行 ✓"
    );
}

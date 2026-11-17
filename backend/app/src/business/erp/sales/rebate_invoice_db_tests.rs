// ============================================================================
// rebate_invoice_db_tests.rs —— 销售返利 · 红字发票（M3-17）的真库用例
//
// # 覆盖什么（每条都对应卡面 / 迁移 / handler 注释里的一句原话）
//
// | 用例 | 钉住的口径 |
// |---|---|
// | `draft_settlement_issues_red_invoice` | 1000 元应返 → `OUT_RED` 票（−884.96 / −115.04 / −1000）+ 三行红字凭证 + 结算单 `SETTLED`（票号/凭证/转结时间都回写） |
// | `preview_matches_issued_invoice` | 试算三列 = 实际开出的三列；`SETTLED` 后 `can_generate=false` 且回显已开票 |
// | `second_issue_is_66093` | 重复开票 → 409 + 66093，库里只有 1 张票（重复开票 = 重复冲收入） |
// | `cancelled_settlement_is_66093` | 作废单开票 → 409 + 66093 |
// | `zero_rebate_is_66094` | 应返 0（期间没订单）→ 409 + 66094 |
// | `closed_period_is_66095_then_open_month_works` | 开票日所属期间已结账 → 409 + 66095，结算单仍是 `DRAFT`（可改日期重来） |
// | `tax_rate_override_splits_differently` | 税率 6% → −943.40 / −56.60（同一应返金额按不同税率拆） |
// | `concurrent_issue_only_one_wins` | 并发开票：只一个成功，另一张 409，库里只有 1 张票 |
// | `zz_teardown_purges_fixtures` | 清场：`M317-` 客户的结算单 / 政策 / 单据 / 发票 0 条 |
//
// # 为什么夹具走 handler
//
// 写入路径就是本卡的 `generate_of`（没有旁路可绕）；主数据一律走
// `crate::test_seed::ensure_*`（门禁：测试不许手写主数据 INSERT），
// 只有**单据**（订单 / 退货）与**结算单**是夹具自己造的输入。
//
// # 与其它 ERP 用例共用串行锁
//
// `db()` 是进程级全局池；本文件复用 `aging_db_tests::DB_LOCK`（各锁各的等于没锁）。
//
// ⚠ 真库用例必须串行，跑法：
//
// ```bash
// ERP_DB_URL="postgres://…/cp_m317_rebate_invoice" cargo test --lib \
//   business::erp::sales::rebate_invoice -- --test-threads=1
// ```
// ============================================================================

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::aging_db_tests::{DB_LOCK, ensure_db, exec};
use crate::business::erp::sales::rebate_dto::{RULE_BY_AMOUNT, RebatePolicyWriteReq, RebateRuleReq};
use crate::business::erp::sales::rebate_handler as policy_handler;
use crate::business::erp::sales::rebate_invoice_dto::RebateInvoiceReq;
use crate::business::erp::sales::rebate_invoice_handler as handler;
use crate::business::erp::sales::rebate_settle_dto::{
    self as settle_dto, STATUS_CANCELLED, STATUS_DRAFT, STATUS_SETTLED,
};
use crate::business::erp::sales::rebate_settle_handler as settle_handler;
use crate::test_seed;
use cloudpivot_kernel::shared::error_code;

/// 夹具客户编码前缀（清场判据）。
const CUS_PREFIX: &str = "M317-";

fn dec(text: &str) -> Decimal {
    text.parse().expect("定点字面量")
}

fn day(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
}

/// 夹具客户（主数据走 `test_seed`）。
async fn customer(code: &str) -> Uuid {
    test_seed::ensure_customer(code, &format!("返利红字票夹具 {code}")).await
}

/// 夹具销售订单 + 一行明细（金额 = 含税 = 本位币；税率 0 ⇒ 价税恒等成立）。
async fn sell(customer_id: Uuid, date: &str, taxed: &str, qty: &str) -> Uuid {
    let so_id = Uuid::now_v7();
    exec(
        "INSERT INTO biz_sales_order \
                (id, so_no, customer_id, order_date, status, total_amount, total_tax, \
                 total_with_tax, base_total_amount, base_total_tax, base_total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, ?::text::date, 'APPROVED', ?::text::numeric, 0, \
                 ?::text::numeric, ?::text::numeric, 0, ?::text::numeric)",
        vec![
            Value::from(&so_id),
            Value::from(format!("M317-SO-{}", &so_id.to_string()[24..])),
            Value::from(&customer_id),
            Value::Text(date.to_string()),
            Value::Text(taxed.to_string()),
            Value::Text(taxed.to_string()),
            Value::Text(taxed.to_string()),
            Value::Text(taxed.to_string()),
        ],
    )
    .await;
    let material = test_seed::ensure_material("M317-MAT", "返利红字票夹具物料").await;
    exec(
        "INSERT INTO biz_sales_order_item \
                (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
         VALUES (?::uuid, 1, ?::uuid, ?::text::numeric, 1, ?::text::numeric, 0, 0, 0)",
        vec![
            Value::from(&so_id),
            Value::from(&material),
            Value::Text(qty.to_string()),
            Value::Text(taxed.to_string()),
        ],
    )
    .await;
    so_id
}

/// 建一份按额返利政策（10% ⇒ 订单 10000 元应返 1000 元）。
async fn policy(customer_id: Uuid, from: &str) {
    policy_handler::create_policy_of(
        None,
        RebatePolicyWriteReq {
            policy_name: "M317 按额返利 10%".to_string(),
            customer_id: customer_id.to_string(),
            rule_type: RULE_BY_AMOUNT.to_string(),
            rate_unit: None,
            settle_period: "MONTH".to_string(),
            effective_from: from.to_string(),
            effective_to: None,
            status: None,
            remark: Some("M3-17 夹具".to_string()),
            rules: vec![RebateRuleReq {
                tier_from: Some(dec("0")),
                tier_to: None,
                rate: Some(dec("10")),
                remark: None,
            }],
        },
    )
    .await
    .expect("建按额政策");
}

fn settle_req(customer_id: Uuid, period_code: &str) -> settle_dto::RebateSettleReq {
    settle_dto::RebateSettleReq {
        customer_id: customer_id.to_string(),
        settle_period: "MONTH".to_string(),
        period_code: period_code.to_string(),
        remark: None,
    }
}

/// 造一张 `DRAFT` 结算单（应返 = 订单含税额 × 10%）。
async fn draft_settlement(customer_id: Uuid, period_code: &str, taxed: &str) -> settle_dto::RebateSettleRes {
    let month_first = format!("{period_code}-01");
    policy(customer_id, &month_first).await;
    if !taxed.is_empty() {
        sell(customer_id, &format!("{period_code}-05"), taxed, "1").await;
    }
    settle_handler::settle_of(None, settle_req(customer_id, period_code))
        .await
        .expect("计算落库")
}

fn invoice_req(date: &str, tax_rate: Option<&str>) -> RebateInvoiceReq {
    RebateInvoiceReq {
        invoice_date: Some(day(date)),
        tax_rate: tax_rate.map(dec),
        remark: None,
    }
}

/// 红字发票行数（按客户 + 返利来源）。
async fn invoice_count(customer_id: Uuid) -> i64 {
    let rows = db()
        .query_sql(
            "SELECT count(*) AS n FROM biz_invoice \
              WHERE customer_id = ?::uuid AND source_type = 'SALES_REBATE'"
                .to_string(),
            vec![Value::from(&customer_id)],
        )
        .await
        .expect("计数");
    rows.first()
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()
        .expect("取计数")
        .unwrap_or(0)
}

/// 清场：物理删本模块造的结算单 / 政策 / 单据 / 发票 / 期间。
async fn cleanup() {
    exec(
        &format!(
            "DELETE FROM biz_voucher WHERE source_id IN (\
                SELECT id FROM biz_invoice WHERE customer_id IN \
                  (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%'))"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_invoice WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_sales_rebate_settlement WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_sales_rebate_policy WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    exec(
        &format!(
            "DELETE FROM biz_sales_order WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
        ),
        vec![],
    )
    .await;
    // 期间行是共享夹具（test_seed 的固定 id）：只把状态拨回 OPEN，不删行
    exec(
        "UPDATE biz_accounting_period SET status = 'OPEN' WHERE period_code IN ('2032-05', '2032-06')",
        vec![],
    )
    .await;
    exec(
        &format!("UPDATE biz_customer SET status = 'ACTIVE' WHERE code LIKE '{CUS_PREFIX}%'"),
        vec![],
    )
    .await;
}

async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
    let guard = DB_LOCK.lock().await;
    if !ensure_db().await {
        return None;
    }
    cleanup().await;
    Some(guard)
}

// ==================== 用例 ====================

/// 主链路：应返 1000 → `OUT_RED` 票（−884.96 / −115.04 / −1000）+ 三行红字凭证 + 结算单 `SETTLED`。
#[tokio::test]
async fn draft_settlement_issues_red_invoice() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C01").await;
    let saved = draft_settlement(cus, "2032-05", "10000").await;
    assert_eq!(saved.status, STATUS_DRAFT);
    assert_eq!(saved.rebate_amount, dec("1000.00"), "10000 × 10%");
    assert!(saved.invoice_no.is_none(), "还没开票");

    let res = handler::generate_of(None, &saved.id, invoice_req("2032-05-31", None))
        .await
        .expect("开红字发票");

    // ── 票面 ──
    assert!(res.invoice_no.starts_with("IV-"), "票号 {}", res.invoice_no);
    assert_eq!(res.invoice_type, "OUT_RED");
    assert_eq!(res.invoice_amount, dec("-884.96"), "1000 / 1.13 = 884.96");
    assert_eq!(res.invoice_tax, dec("-115.04"));
    assert_eq!(res.invoice_total, dec("-1000.00"), "价税合计 = −应返");
    assert_eq!(res.tax_rate, dec("13"));
    assert_eq!(res.invoice_status, "ISSUED");
    assert_eq!(res.currency_code, "CNY");
    assert!(res.voucher_id.is_some(), "红字凭证必须回写");
    assert!(
        res.voucher_no
            .as_deref()
            .is_some_and(|no| no.starts_with("PZ-N") || no.starts_with("PZ")),
        "凭证号 {:?}",
        res.voucher_no
    );
    assert_eq!(res.settlement_status, STATUS_SETTLED);
    assert_eq!(res.settlement_status_label, "已开红字发票");
    assert!(res.settled_at.is_some(), "转结时间必须落库");

    // ── 库里的票：来源类型 / 来源单号 / 本位币三列一起写 ──
    let rows = db()
        .query_sql(
            "SELECT invoice_type, source_type, source_no, amount, tax_amount, total_with_tax, \
                    base_total_amount, base_total_tax, base_total_with_tax, currency_code, exchange_rate, \
                    tax_rate, tax_rate_mode, status, receivable_id \
               FROM biz_invoice WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&res.invoice_id.parse::<Uuid>().expect("发票 id 是 UUID"))],
        )
        .await
        .expect("读发票");
    let row = rows.first().expect("发票必须存在");
    assert_eq!(row.get_typed::<String>("invoice_type").unwrap(), "OUT_RED");
    assert_eq!(row.get_typed::<String>("source_type").unwrap(), "SALES_REBATE");
    assert_eq!(
        row.get_typed::<String>("source_no").unwrap(),
        saved.settlement_no,
        "来源单号必须是结算单号（按 RB-… 查票）"
    );
    assert_eq!(row.get_typed::<Decimal>("base_total_amount").unwrap(), dec("-884.96"));
    assert_eq!(row.get_typed::<Decimal>("base_total_tax").unwrap(), dec("-115.04"));
    assert_eq!(
        row.get_typed::<Decimal>("base_total_with_tax").unwrap(),
        dec("-1000.00")
    );
    assert_eq!(row.get_typed::<String>("currency_code").unwrap(), "CNY");
    assert_eq!(row.get_typed::<Decimal>("exchange_rate").unwrap(), Decimal::ONE);
    assert_eq!(row.get_typed::<String>("tax_rate_mode").unwrap(), "PERCENT");
    assert_eq!(row.get_typed::<String>("status").unwrap(), "ISSUED");
    assert!(
        row.get_typed::<Option<Uuid>>("receivable_id").unwrap().is_none(),
        "返利票不挂应收"
    );

    // ── 凭证：借 5001 / 借 2221.02 / 贷 2241.05，借贷平衡 ──
    // 贷方科目是 **2241.05 其他应付款-销售返利**，不是 2241.02（那是佣金科目，
    // V0440 的种子静默跳过后返利负债被记错过，见 t_627d2a4b / V0445）。
    let entries = db()
        .query_sql(
            "SELECT account_code, account_name, debit_amount, credit_amount, summary \
               FROM biz_voucher_entry WHERE voucher_id = ?::uuid ORDER BY line_no"
                .to_string(),
            vec![Value::from(
                &res.voucher_id
                    .as_deref()
                    .expect("凭证 id")
                    .parse::<Uuid>()
                    .expect("凭证 id 是 UUID"),
            )],
        )
        .await
        .expect("读凭证分录");
    assert_eq!(entries.len(), 3, "收入 + 销项税 + 其他应付款 三行");
    let debit_total: Decimal = entries
        .iter()
        .map(|row| row.get_typed::<Decimal>("debit_amount").unwrap())
        .sum();
    let credit_total: Decimal = entries
        .iter()
        .map(|row| row.get_typed::<Decimal>("credit_amount").unwrap())
        .sum();
    assert_eq!(debit_total, credit_total, "凭证必须借贷平衡");
    assert_eq!(debit_total, dec("1000"), "借方 = 收入 + 税 = 价税合计");
    let codes: Vec<String> = entries
        .iter()
        .map(|row| row.get_typed::<String>("account_code").unwrap())
        .collect();
    assert_eq!(
        codes,
        vec!["5001", "2221.02", "2241.05"],
        "科目与顺序（借收入/借税/贷应付）；贷方 2241.05 = 其他应付款-销售返利"
    );
    assert!(
        !codes.contains(&"2241.02".to_string()),
        "2241.02 是「其他应付款-佣金」，返利凭证不许贷记它（V0440 的静默跳过 bug）"
    );
    let payable = entries
        .iter()
        .find(|row| row.get_typed::<String>("account_code").unwrap() == "2241.05")
        .expect("贷方其他应付款-销售返利");
    assert_eq!(payable.get_typed::<Decimal>("credit_amount").unwrap(), dec("1000.00"));
    assert_eq!(payable.get_typed::<Decimal>("debit_amount").unwrap(), Decimal::ZERO);
    // 名称也验（只比 code 正是当初漏过去的地方）：分录快照名 + 科目主数据实时名
    assert_eq!(
        payable.get_typed::<String>("account_name").unwrap(),
        "其他应付款-销售返利",
        "分录快照的科目名必须是「其他应付款-销售返利」"
    );
    let live_name = db()
        .query_first_sql(
            "SELECT account_name, is_leaf FROM biz_account \
              WHERE account_code = '2241.05' AND deleted_at IS NULL"
                .to_string(),
            vec![],
        )
        .await
        .expect("读科目主数据")
        .expect("科目 2241.05 必须存在");
    assert_eq!(
        live_name.get_typed::<String>("account_name").unwrap(),
        "其他应付款-销售返利",
        "科目主数据里 2241.05 必须是「其他应付款-销售返利」"
    );
    assert_eq!(
        live_name.get_typed::<i16>("is_leaf").unwrap(),
        1,
        "返利应付必须是末级科目（非末级科目不许记账）"
    );

    // ── 结算单：SETTLED + 票号 + 凭证 + 转结时间 ──
    let detail = settle_handler::detail_settlement_of(&saved.id).await.expect("详情");
    assert_eq!(detail.status, STATUS_SETTLED);
    assert_eq!(detail.invoice_no.as_deref(), Some(res.invoice_no.as_str()));
    assert_eq!(detail.invoice_id.as_deref(), Some(res.invoice_id.as_str()));
    assert!(detail.settled_at.is_some());

    // ── 已开票的结算单不能再重算 / 不能作废（M3-16 的时序闸） ──
    let recalc = settle_handler::recalc_of(None, &saved.id, None)
        .await
        .expect_err("SETTLED 后不能重算");
    assert_eq!(recalc.status, 409);
    let cancel = settle_handler::cancel_of(None, &saved.id, Some("试图作废"))
        .await
        .expect_err("SETTLED 后不能作废");
    assert_eq!(cancel.status, 409);

    println!(
        "[M3-17] 结算单 → 红字票 {}（−884.96 / −115.04 / −1000）+ 凭证 {} + SETTLED ✓",
        res.invoice_no,
        res.voucher_no.clone().unwrap_or_default()
    );
}

/// 试算与实际开票一致；已开票后试算 `can_generate=false` 且回显已开票。
#[tokio::test]
async fn preview_matches_issued_invoice() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C02").await;
    let saved = draft_settlement(cus, "2032-05", "10000").await;

    let preview = handler::preview_of(&saved.id, invoice_req("2032-05-31", None))
        .await
        .expect("试算");
    assert!(preview.can_generate, "DRAFT 应当可开票：{:?}", preview.blockers);
    assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
    assert_eq!(preview.invoice_amount, dec("-884.96"));
    assert_eq!(preview.invoice_tax, dec("-115.04"));
    assert_eq!(preview.invoice_total, dec("-1000.00"));
    assert_eq!(preview.voucher_lines.len(), 3, "分录预览三行");
    assert_eq!(preview.voucher_lines[0].side, "DEBIT");
    assert_eq!(
        preview.voucher_lines.last().map(|line| line.side.as_str()),
        Some("CREDIT")
    );
    assert!(preview.existing_invoice.is_none(), "还没开票");

    let issued = handler::generate_of(None, &saved.id, invoice_req("2032-05-31", None))
        .await
        .expect("开票");
    assert_eq!(preview.invoice_amount, issued.invoice_amount, "试算 = 实开");
    assert_eq!(preview.invoice_tax, issued.invoice_tax, "试算 = 实开");

    let after = handler::preview_of(&saved.id, invoice_req("2032-05-31", None))
        .await
        .expect("再试算");
    assert!(!after.can_generate, "已开票不能再开");
    assert!(!after.blockers.is_empty(), "必须写明为什么不能开");
    assert!(after.blockers[0].contains("已开过红字发票"), "{:?}", after.blockers);
    let existing = after.existing_invoice.expect("回显已开票");
    assert_eq!(existing.invoice_no, issued.invoice_no);
    assert_eq!(existing.invoice_total, dec("-1000.00"));

    println!(
        "[M3-17] 试算 = 实开；SETTLED 后 can_generate=false 且回显 {} ✓",
        issued.invoice_no
    );
}

/// 重复开票 → 409 + 66093，库里只有 1 张票。
#[tokio::test]
async fn second_issue_is_66093() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C03").await;
    let saved = draft_settlement(cus, "2032-05", "10000").await;
    handler::generate_of(None, &saved.id, invoice_req("2032-05-31", None))
        .await
        .expect("第一次开票");

    let error = handler::generate_of(None, &saved.id, invoice_req("2032-05-31", None))
        .await
        .expect_err("第二次必须拒");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_REBATE_INVOICE_STATE);
    assert!(error.msg.contains("已开过红字发票"), "{}", error.msg);
    assert_eq!(invoice_count(cus).await, 1, "重复开票不能多出第二张票");

    // 试算也要说清（预览不报 409，但 blockers 必须命中）
    let preview = handler::preview_of(&saved.id, invoice_req("2032-05-31", None))
        .await
        .expect("试算");
    assert!(!preview.can_generate);
    let detail = settle_handler::detail_settlement_of(&saved.id).await.expect("详情");
    assert_eq!(
        preview.existing_invoice.map(|res| res.invoice_no),
        detail.invoice_no,
        "试算要回显已开票号"
    );

    println!("[M3-17] 重复开票 409 + 66093，库里仍只有 1 张票 ✓");
}

/// 作废单开票 → 409 + 66093。
#[tokio::test]
async fn cancelled_settlement_is_66093() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C04").await;
    let saved = draft_settlement(cus, "2032-05", "10000").await;
    settle_handler::cancel_of(None, &saved.id, Some("算错了作废"))
        .await
        .expect("作废");
    let detail = settle_handler::detail_settlement_of(&saved.id).await.expect("详情");
    assert_eq!(detail.status, STATUS_CANCELLED);

    let error = handler::generate_of(None, &saved.id, invoice_req("2032-05-31", None))
        .await
        .expect_err("作废单必须拒");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_REBATE_INVOICE_STATE);
    assert!(error.msg.contains("已作废"), "{}", error.msg);
    assert_eq!(invoice_count(cus).await, 0, "作废单不能产生票");

    println!("[M3-17] 作废单开票 409 + 66093，零票 ✓");
}

/// 应返 0（期间没有订单）→ 409 + 66094。
#[tokio::test]
async fn zero_rebate_is_66094() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C05").await;
    // 有政策、没有订单 ⇒ 净销售 0 ⇒ 应返 0
    let saved = draft_settlement(cus, "2032-05", "").await;
    assert_eq!(saved.rebate_amount, Decimal::ZERO, "没订单应返 0");

    let error = handler::generate_of(None, &saved.id, invoice_req("2032-05-31", None))
        .await
        .expect_err("0 元不能开票");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_REBATE_INVOICE_AMOUNT);
    assert_eq!(invoice_count(cus).await, 0);

    // 结算单仍是 DRAFT：可以作废重来，不需要工程师改库
    let detail = settle_handler::detail_settlement_of(&saved.id).await.expect("详情");
    assert_eq!(detail.status, STATUS_DRAFT, "失败后必须留在可重试的状态");

    println!("[M3-17] 应返 0 → 409 + 66094；结算单仍 DRAFT ✓");
}

/// 期间已结账 → 409 + 66095；换到未结账月份即可开票（结算单一直留在 DRAFT）。
#[tokio::test]
async fn closed_period_is_66095_then_open_month_works() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C06").await;
    let saved = draft_settlement(cus, "2032-05", "10000").await;

    // 期间行走统一夹具（test_seed 只建 OPEN；「已结账」是用例自己的场景，用 UPDATE 改状态）
    test_seed::ensure_accounting_period(2032, 5).await;
    exec(
        "UPDATE biz_accounting_period SET status = 'CLOSED' WHERE period_code = '2032-05'",
        vec![],
    )
    .await;

    let error = handler::generate_of(None, &saved.id, invoice_req("2032-05-31", None))
        .await
        .expect_err("已结账期间必须拒");
    assert_eq!(error.status, 409);
    assert_eq!(error.code, error_code::ERP_REBATE_INVOICE_PERIOD_CLOSED);
    assert!(error.msg.contains("2032-05"), "报文必须点名期间：{}", error.msg);
    assert_eq!(invoice_count(cus).await, 0, "拒绝后不能留票");
    let detail = settle_handler::detail_settlement_of(&saved.id).await.expect("详情");
    assert_eq!(detail.status, STATUS_DRAFT, "失败后结算单仍是 DRAFT");

    // 试算也要拦（blockers 里写明已结账）
    let preview = handler::preview_of(&saved.id, invoice_req("2032-05-31", None))
        .await
        .expect("试算");
    assert!(!preview.can_generate);
    assert!(
        preview.blockers.iter().any(|text| text.contains("已结账")),
        "{:?}",
        preview.blockers
    );

    // 换到未结账月份 → 成功
    let res = handler::generate_of(None, &saved.id, invoice_req("2032-06-01", None))
        .await
        .expect("未结账期间可开票");
    assert_eq!(res.invoice_date, day("2032-06-01"));
    assert_eq!(res.settlement_status, STATUS_SETTLED);

    println!(
        "[M3-17] 已结账期间 409 + 66095 → 换 2032-06 开票成功 {} ✓",
        res.invoice_no
    );
}

/// 税率覆盖：6% ⇒ −943.40 / −56.60（同一应返金额按不同税率拆）。
#[tokio::test]
async fn tax_rate_override_splits_differently() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C07").await;
    let saved = draft_settlement(cus, "2032-05", "10000").await;

    let preview = handler::preview_of(&saved.id, invoice_req("2032-05-31", Some("6")))
        .await
        .expect("试算");
    assert_eq!(preview.tax_rate, dec("6"));
    assert_eq!(preview.invoice_amount, dec("-943.40"), "1000 / 1.06 = 943.40");
    assert_eq!(preview.invoice_tax, dec("-56.60"));
    assert_eq!(preview.invoice_total, dec("-1000.00"));
    assert!(
        preview.warnings.iter().any(|text| text.contains("默认 13%")),
        "非默认税率要提醒：{:?}",
        preview.warnings
    );

    let res = handler::generate_of(None, &saved.id, invoice_req("2032-05-31", Some("6")))
        .await
        .expect("开票");
    assert_eq!(res.tax_rate, dec("6"));
    assert_eq!(res.invoice_amount, dec("-943.40"));
    assert_eq!(res.invoice_tax, dec("-56.60"));

    println!("[M3-17] 6% 税率拆分 −943.40 / −56.60 ✓");
}

/// 并发开票：只一个成功，库里只有 1 张票（另一路 409）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_issue_only_one_wins() {
    let Some(_guard) = setup().await else { return };
    let cus = customer("M317-C08").await;
    let saved = draft_settlement(cus, "2032-05", "10000").await;
    let id = saved.id.clone();

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let id = id.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            handler::generate_of(None, &id, invoice_req("2032-05-31", None)).await
        }));
    }
    let mut ok = 0;
    let mut conflict = 0;
    for task in tasks {
        match task.await.expect("任务不 panic") {
            Ok(_) => ok += 1,
            Err(error) => {
                assert_eq!(error.status, 409, "失败的那一路必须是 409：{}", error.msg);
                conflict += 1;
            }
        }
    }
    assert_eq!(ok, 1, "并发开票只能成功一路");
    assert_eq!(conflict, 1);
    assert_eq!(invoice_count(cus).await, 1, "库里只能有 1 张票");

    println!("[M3-17] 并发开票：1 成功 / 1 冲突（409），库里 1 张票 ✓");
}

/// 清场：`M317-` 客户的结算单 / 政策 / 单据 / 发票 0 条。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    let Some(_guard) = setup().await else { return };
    cleanup().await;
    for (sql, label) in [
        (
            format!(
                "SELECT count(*) AS n FROM biz_invoice WHERE customer_id IN \
                  (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
            ),
            "发票",
        ),
        (
            format!(
                "SELECT count(*) AS n FROM biz_sales_rebate_settlement WHERE customer_id IN \
                  (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
            ),
            "结算单",
        ),
        (
            format!(
                "SELECT count(*) AS n FROM biz_sales_rebate_policy WHERE customer_id IN \
                  (SELECT id FROM biz_customer WHERE code LIKE '{CUS_PREFIX}%')"
            ),
            "政策",
        ),
    ] {
        let rows = db().query_sql(sql, vec![]).await.expect("计数");
        let n = rows
            .first()
            .map(|row| row.get_typed::<i64>("n"))
            .transpose()
            .expect("取计数")
            .unwrap_or(0);
        assert_eq!(n, 0, "{label} 应有 0 条（清场失败）");
    }
    println!("[M3-17] 清场：M317- 夹具全部删除 ✓");
}

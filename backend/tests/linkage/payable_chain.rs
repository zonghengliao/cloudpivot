//! 领域 4：应付 ↔ 付款（8 个用例，P01–P08）。
//!
//! # 链路
//!
//! ```text
//! 收货单过账 ──▶ biz_payable(OPEN, amount, unpaid=amount)
//!                     │
//!                     ├─ 登记付款 ──▶ part: paid+=x, unpaid-=x, 状态 OPEN→PARTIAL_PAID→SETTLED
//!                     │                └─ 每笔付款生成一张凭证（借 2202 / 贷 1002）
//!                     └─ 作废 ─────▶ CANCELLED（只允许「一分未付」的单子）
//! ```
//!
//! 单模块用例（`payable_payment` 的 `p7_*`）已经覆盖了这些原子的行为；本文件的增量在
//! **跨单据一致性**：
//!
//! - 付款流水 → 付款凭证 → 应付「已付/未付」三者的金额必须始终对齐（P04/P08）；
//! - 应付的状态机与凭证的存在性不能在并发/重复调用下分叉（P06/P07）；
//! - 一笔付款失败时，**不许**留下「凭证写了、应付没加」的半成品（P05 的反向断言）。
//!
//! 所有断言按本用例的供应商 / 收货单 id 过滤，不做全局 `COUNT(*)`。

use nik::prelude::*;
use nik::uuid::Uuid;
use rust_decimal::Decimal;

use cloudpivot::business::erp::payable_payment::{PayPayableReq, cancel_payable, pay_payable};
use cloudpivot::business::erp::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};
use cloudpivot::business::erp::stockin_handler::{change_stock_in_status, create_stock_in};

use crate::linkage::common::{self, Ctx};

// ==================== 夹具动作 ====================

/// 建一张**已过账**的收货单（`qty × price` 不含税，`rate` 税率），返回收货单 id。
///
/// 应付单是过账的连带产物，所以这是「造一笔应付」最短的真实路径（不直插台账）。
async fn posted_stock_in(ctx: &Ctx, qty: &str, price: &str, rate: &str) -> Uuid {
    let loc = ctx.loc_row(ctx.mat).await;
    let req = CreateStockInReq {
        source_order_id: None,
        purchase_order_id: None,
        // A4（V0199）起：PURCHASE 且无来源采购单 = 无源单据，必须显式声明（remark 已给）
        is_sourceless: Some(true),
        supplier_id: Some(ctx.supplier.to_string()),
        warehouse_id: Some(ctx.wh_from.to_string()),
        in_date: None,
        invoice_received: None,
        remark: Some(format!("{} 应付链路", ctx.tag)),
        items: vec![StockInItemReq {
            purchase_order_item_id: None,
            material_id: Some(ctx.mat.to_string()),
            qty: Some(common::dec(qty)),
            price_excl: Some(common::dec(price)),
            tax_rate: Some(common::dec(rate)),
            location_id: Some(loc.to_string()),
            serial_ids: Vec::new(),
            remark: None,
        }],
        total_qty: None,
        total_amount: None,
        total_tax: None,
    };
    let id = create_stock_in(&req, Some(ctx.worker))
        .await
        .unwrap_or_else(|error| panic!("{}：建收货单失败 {} {}", ctx.tag, error.code, error.msg));
    change_stock_in_status(
        id,
        &StockInStatusReq {
            status: Some("POSTED".to_string()),
            reason: None,
            serials: Vec::new(),
            // ARCH-3 DC-01：不请求跳过审批（默认行为不变）
            approval: Default::default(),
        },
        Some(ctx.worker),
    )
    .await
    .unwrap_or_else(|error| panic!("{}：过账失败 {} {}", ctx.tag, error.code, error.msg));
    id
}

/// 应付台账快照：`(id, status, amount, paid, unpaid)`。
async fn payable_snapshot(stock_in_id: Uuid) -> (Uuid, String, Decimal, Decimal, Decimal) {
    let rows = db()
        .find_with_params(
            "SELECT id, status, amount, paid_amount, unpaid_amount FROM biz_payable \
              WHERE source_type = 'STOCK_IN' AND source_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查应付失败：{error}"));
    let row = rows
        .first()
        .unwrap_or_else(|| panic!("收货单 {stock_in_id} 没有应付单"));
    (
        row.get_typed("id").expect("应付 id"),
        row.get_typed("status").expect("应付状态"),
        row.get_typed("amount").expect("应付金额"),
        row.get_typed("paid_amount").expect("已付"),
        row.get_typed("unpaid_amount").expect("未付"),
    )
}

/// 本用例某张应付单的付款条数与已付金额合计。
async fn payment_stats(payable_id: Uuid) -> (i64, Decimal) {
    let count = common::scalar_i64(
        "SELECT count(*) AS v FROM biz_payment WHERE payable_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&payable_id)],
    )
    .await;
    let sum = common::scalar_dec(
        "SELECT COALESCE(sum(amount),0)::numeric(18,4) AS v FROM biz_payment \
          WHERE payable_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&payable_id)],
    )
    .await;
    (count, sum)
}

/// 登记一笔付款（走生产路径）。
async fn pay(ctx: &Ctx, payable_id: Uuid, amount: &str) -> std::result::Result<(), ApiError> {
    let req = PayPayableReq {
        amount: Some(common::dec(amount)),
        pay_date: None,
        pay_method: Some("银行转账".to_string()),
        remark: Some(format!("{} 付款", ctx.tag)),
        // ARCH-3 DC-01：新加的「显式跳过审批」片段（不请求跳过 = 默认行为不变）
        approval: Default::default(),
    };
    pay_payable(payable_id, &req, Some(ctx.worker))
        .await
        .map(|_| ())
}

/// 付款成功（失败直接 panic）。
async fn pay_ok(ctx: &Ctx, payable_id: Uuid, amount: &str) {
    pay(ctx, payable_id, amount).await.unwrap_or_else(|error| {
        panic!(
            "{}：付款 {amount} 失败 {} {}",
            ctx.tag, error.code, error.msg
        )
    });
}

/// 某笔付款产生的凭证 id（`biz_payment.voucher_id`）。
async fn payment_voucher_ids(payable_id: Uuid) -> Vec<Uuid> {
    let rows = db()
        .find_with_params(
            "SELECT voucher_id FROM biz_payment \
              WHERE payable_id = ?::uuid AND deleted_at IS NULL AND voucher_id IS NOT NULL \
              ORDER BY created_at",
            vec![Value::from(&payable_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查付款凭证失败：{error}"));
    rows.iter()
        .map(|row| row.get_typed::<Uuid>("voucher_id").expect("voucher_id"))
        .collect()
}

// ==================== P01–P03：付款累计与状态机 ====================

/// P01 全额付款 → SETTLED，unpaid = 0。
#[tokio::test]
async fn p01_full_payment_settles_payable() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P01").await;
    // 10 × 2 = 20 不含税，13% 税 2.60 → 价税合计 22.60
    let stock_in = posted_stock_in(&ctx, "10", "2", "13").await;
    let (payable_id, status, amount, _, unpaid) = payable_snapshot(stock_in).await;
    assert_eq!(status, "OPEN", "P01 前置：过账后应付必须 OPEN");
    common::assert_dec(amount, common::dec("22.6"), "P01 前置：应付金额 = 价税合计");
    common::assert_dec(unpaid, amount, "P01 前置：未付 = 应付金额");

    pay_ok(&ctx, payable_id, "22.6").await;

    let (_, status, amount, paid, unpaid) = payable_snapshot(stock_in).await;
    assert_eq!(status, "SETTLED", "P01：全额付款必须转 SETTLED");
    common::assert_dec(paid, amount, "P01：已付 = 应付金额");
    common::assert_dec(unpaid, common::dec("0"), "P01：未付必须归零");
    let (count, sum) = payment_stats(payable_id).await;
    assert_eq!(count, 1, "P01：只该有 1 条付款流水");
    common::assert_dec(sum, amount, "P01：付款流水合计 = 应付金额");
    ctx.cleanup().await;
}

/// P02 部分付款 → PARTIAL_PAID（未付余额同步减少）。
#[tokio::test]
async fn p02_partial_payment_marks_partial_paid() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P02").await;
    let stock_in = posted_stock_in(&ctx, "10", "2", "0").await; // 20.00
    let (payable_id, _, amount, _, _) = payable_snapshot(stock_in).await;

    pay_ok(&ctx, payable_id, "5").await;

    let (_, status, _, paid, unpaid) = payable_snapshot(stock_in).await;
    assert_eq!(status, "PARTIAL_PAID", "P02：部分付款必须转 PARTIAL_PAID");
    common::assert_dec(paid, common::dec("5"), "P02：已付 5");
    common::assert_dec(unpaid, amount - common::dec("5"), "P02：未付 = 20 - 5");
    ctx.cleanup().await;
}

/// P03 两次部分付款 → 第二次付完转 SETTLED（累计而不是覆盖）。
#[tokio::test]
async fn p03_two_partial_payments_accumulate_to_settled() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P03").await;
    let stock_in = posted_stock_in(&ctx, "10", "2", "0").await; // 20.00
    let (payable_id, _, amount, _, _) = payable_snapshot(stock_in).await;

    pay_ok(&ctx, payable_id, "7").await;
    let (_, status, _, paid, unpaid) = payable_snapshot(stock_in).await;
    assert_eq!(status, "PARTIAL_PAID");
    common::assert_dec(paid, common::dec("7"), "P03：第一笔后已付 7");
    common::assert_dec(unpaid, common::dec("13"), "P03：第一笔后未付 13");

    pay_ok(&ctx, payable_id, "13").await;
    let (_, status, _, paid, unpaid) = payable_snapshot(stock_in).await;
    assert_eq!(status, "SETTLED", "P03：付完必须转 SETTLED");
    common::assert_dec(paid, amount, "P03：已付累计 = 应付金额（不是覆盖成 13）");
    common::assert_dec(unpaid, common::dec("0"), "P03：未付归零");
    let (count, sum) = payment_stats(payable_id).await;
    assert_eq!(count, 2, "P03：必须是 2 条独立付款流水（审计要能逐笔对账）");
    common::assert_dec(sum, amount, "P03：两笔合计 = 20");
    ctx.cleanup().await;
}

// ==================== P04：付款凭证 ====================

/// P04 付款生成凭证（借 2202 / 贷 1002），借贷平衡。
#[tokio::test]
async fn p04_payment_generates_balanced_voucher() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P04").await;
    let stock_in = posted_stock_in(&ctx, "10", "2", "0").await; // 20.00
    let (payable_id, _, amount, _, _) = payable_snapshot(stock_in).await;
    pay_ok(&ctx, payable_id, "8").await;

    let vouchers = payment_voucher_ids(payable_id).await;
    assert_eq!(
        vouchers.len(),
        1,
        "P04：每笔付款必须且只能生成 1 张凭证，实际 {} 张",
        vouchers.len()
    );
    let voucher_id = vouchers[0];

    let rows = db()
        .find_with_params(
            "SELECT account_code, debit_amount::numeric(18,4) AS debit, \
                    credit_amount::numeric(18,4) AS credit \
               FROM biz_voucher_entry WHERE voucher_id = ?::uuid ORDER BY account_code",
            vec![Value::from(&voucher_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查付款凭证分录失败：{error}"));
    let mut debit_sum = common::dec("0");
    let mut credit_sum = common::dec("0");
    let mut codes: Vec<(String, Decimal, Decimal)> = Vec::new();
    for row in &rows {
        let code: String = row.get_typed("account_code").expect("科目");
        let debit: Decimal = row.get_typed("debit").expect("借");
        let credit: Decimal = row.get_typed("credit").expect("贷");
        debit_sum += debit;
        credit_sum += credit;
        codes.push((code, debit, credit));
    }
    common::assert_dec(debit_sum, credit_sum, "P04：Σ借 = Σ贷");
    common::assert_dec(
        debit_sum,
        common::dec("8"),
        "P04：凭证金额 = 本次付款金额（不是应付总额）",
    );

    let find = |code: &str| {
        codes
            .iter()
            .find(|(c, _, _)| c == code)
            .map(|(_, d, c)| (*d, *c))
    };
    let (d2202, c2202) = find("2202").unwrap_or_else(|| panic!("P04：缺 2202 分录：{codes:?}"));
    common::assert_dec(
        d2202,
        common::dec("8"),
        "P04：借 2202 应付账款（付款冲减负债）",
    );
    common::assert_dec(c2202, common::dec("0"), "P04：2202 不许有贷方");
    let (d1002, c1002) = find("1002").unwrap_or_else(|| panic!("P04：缺 1002 分录：{codes:?}"));
    common::assert_dec(d1002, common::dec("0"), "P04：1002 不许有借方");
    common::assert_dec(c1002, common::dec("8"), "P04：贷 1002 银行存款");

    // 反向：付款凭证不能影响库存（付款不是库存事件）
    let (inv, _, txn, _) = common::four_ledger_snapshot(ctx.mat, ctx.wh_from).await;
    common::assert_dec(
        inv,
        common::dec("10"),
        "P04：付款不动库存数量（仍为收货的 10）",
    );
    assert_eq!(txn, 1, "P04：付款不写库存流水（仍只有收货那 1 条）");
    assert!(amount > common::dec("0"));
    ctx.cleanup().await;
}

// ==================== P05–P07：反向保护 ====================

/// P05 付款超未付余额 → 400，且**不产生任何副作用**（无付款流水、无凭证、应付不变）。
#[tokio::test]
async fn p05_over_payment_is_bad_request() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P05").await;
    let stock_in = posted_stock_in(&ctx, "10", "2", "0").await; // 20.00
    let (payable_id, _, amount, _, unpaid) = payable_snapshot(stock_in).await;

    let error = pay(&ctx, payable_id, "20.01")
        .await
        .expect_err("P05：付 20.01 > 未付 20 必须被拒");
    assert_eq!(
        error.status, 400,
        "P05：超额付款是参数问题 → 400，实际 {error:?}"
    );
    assert!(
        error.msg.contains("未付") || error.msg.contains("超过"),
        "P05：文案要点名未付余额，实际：{}",
        error.msg
    );

    let (_, status, amount_after, paid, unpaid_after) = payable_snapshot(stock_in).await;
    assert_eq!(status, "OPEN", "P05：被拒后状态不许变");
    common::assert_dec(amount_after, amount, "P05：应付金额不变");
    common::assert_dec(paid, common::dec("0"), "P05：没有产生已付金额");
    common::assert_dec(unpaid_after, unpaid, "P05：未付不变");
    let (count, _) = payment_stats(payable_id).await;
    assert_eq!(count, 0, "P05：被拒的付款不许留流水（否则对账全乱）");
    assert!(
        payment_voucher_ids(payable_id).await.is_empty(),
        "P05：被拒的付款不许生成凭证"
    );
    ctx.cleanup().await;
}

/// P06 对 SETTLED 再付款 → 409。
#[tokio::test]
async fn p06_pay_settled_is_conflict() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P06").await;
    let stock_in = posted_stock_in(&ctx, "10", "2", "0").await;
    let (payable_id, _, amount, _, _) = payable_snapshot(stock_in).await;
    pay_ok(&ctx, payable_id, "20").await;
    assert_eq!(payable_snapshot(stock_in).await.1, "SETTLED");

    let error = pay(&ctx, payable_id, "1")
        .await
        .expect_err("P06：已结清的应付不能再付款");
    assert_eq!(error.status, 409, "P06：已结清 → 409，实际 {error:?}");
    let (_, status, _, paid, unpaid) = payable_snapshot(stock_in).await;
    assert_eq!(status, "SETTLED", "P06：状态仍是 SETTLED");
    common::assert_dec(paid, amount, "P06：已付不变");
    common::assert_dec(unpaid, common::dec("0"), "P06：未付仍是 0（没被扣成负数）");
    let (count, _) = payment_stats(payable_id).await;
    assert_eq!(count, 1, "P06：被拒的付款不许留流水");
    ctx.cleanup().await;
}

/// P07 已付款的应付单作废 → 409（钱已经付出去了，作废会让账消失）。
#[tokio::test]
async fn p07_cancel_paid_payable_is_conflict() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P07").await;
    let stock_in = posted_stock_in(&ctx, "10", "2", "0").await;
    let (payable_id, _, _, _, _) = payable_snapshot(stock_in).await;
    pay_ok(&ctx, payable_id, "5").await; // 部分付款

    let error = cancel_payable(payable_id, Some("P07 想作废已付款的单"), Some(ctx.worker))
        .await
        .expect_err("P07：已付款的应付单不许作废");
    assert_eq!(error.status, 409, "P07：已付款 → 409，实际 {error:?}");

    let (_, status, _, paid, _) = payable_snapshot(stock_in).await;
    assert_eq!(
        status, "PARTIAL_PAID",
        "P07：被拒后必须仍是 PARTIAL_PAID（不能变成 CANCELLED）"
    );
    common::assert_dec(paid, common::dec("5"), "P07：已付金额不变");

    // 反向：一分未付的单子应该**能**作废（否则就是一刀切锁死了）
    let fresh = posted_stock_in(&ctx, "1", "1", "0").await;
    let (fresh_id, fresh_status, _, _, _) = payable_snapshot(fresh).await;
    assert_eq!(fresh_status, "OPEN");
    cancel_payable(fresh_id, Some("P07 未付款可以作废"), Some(ctx.worker))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "P07：未付款的应付单应当能作废，实际 {} {}",
                error.code, error.msg
            )
        });
    assert_eq!(
        payable_snapshot(fresh).await.1,
        "CANCELLED",
        "P07：未付款的应付单作废后必须是 CANCELLED"
    );
    ctx.cleanup().await;
}

// ==================== P08：恒等式 ====================

/// 本用例应付单的状态日志轨迹：`(from, to)`，按写入顺序（NULL 记成空串）。
///
/// 断言「状态日志只写真实变化」用 —— 分期付款的第二期是 `PARTIAL_PAID → PARTIAL_PAID`，
/// 它**不该**出现在 `biz_status_log` 里（业务流水 / 凭证照样要有）。
async fn status_trail(payable_id: Uuid) -> Vec<(String, String)> {
    let rows = db()
        .find_with_params(
            "SELECT from_status, to_status FROM biz_status_log \
              WHERE entity_type = 'PAYABLE' AND entity_id = ?::uuid \
              ORDER BY created_at ASC, id ASC",
            vec![Value::from(&payable_id)],
        )
        .await
        .unwrap_or_else(|error| panic!("查应付状态日志失败：{error}"));
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<Option<String>>("from_status")
                    .expect("from_status")
                    .unwrap_or_default(),
                row.get_typed::<String>("to_status").expect("to_status"),
            )
        })
        .collect()
}

/// P08 `paid + unpaid = amount` 恒等（**三次**部分付款，每次之后都成立）。
///
/// # 这条用例抓到过真缺陷（LNK-P08，已修）
///
/// 修复前：同一张应付单**第二次**部分付款（`PARTIAL_PAID → PARTIAL_PAID`）被 P10 的
/// 状态机入口以 409 拒掉 ——
///
/// ```text
/// 10001 [10003] 应付单 AP-20261031-000091 的状态没有变化（PARTIAL_PAID → PARTIAL_PAID）：
/// 原地流转不是状态变更，不要写状态日志（重复部分收货请只写业务流水）
/// ```
///
/// 「分三期付款」这条最普通的业务路径第二期就断。根因是 `payable_payment` **无条件**调
/// `state_machine::transition`；修法是改用 `state_machine::transition_if_changed`
/// （状态没变 → `Ok(None)`，只写业务流水 + 凭证，不写状态日志），
/// `transition` 的自环 409 护栏保持不动。
///
/// 单模块用例 `p7_two_partial_payments_accumulate_and_settle` 只付两次、且两次都跨了状态
/// （OPEN→PARTIAL_PAID→SETTLED），所以没抓到 —— 这正是本用例要补的盲区。
///
/// 三条断言对应验收标准 2 / 3：
/// 1. 每次付款后 `paid + unpaid = amount`；
/// 2. 状态日志只写**真实变化**：建单 `NULL→OPEN` 一条、`OPEN→PARTIAL_PAID`、
///    `PARTIAL_PAID→SETTLED` 各一条，**不许**出现 `PARTIAL_PAID→PARTIAL_PAID`；
/// 3. 付完自动 `SETTLED`、未付归零。
#[tokio::test]
async fn p08_paid_plus_unpaid_equals_amount() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("P08").await;
    let stock_in = posted_stock_in(&ctx, "4", "2.5", "13").await; // 10 + 13% = 11.30
    let (payable_id, _, amount, paid, unpaid) = payable_snapshot(stock_in).await;
    common::assert_dec(paid + unpaid, amount, "P08：初始 paid + unpaid = amount");
    // 建单留痕：过账生成应付单本身写一条 `NULL → OPEN`（log_created），此后才是真实流转
    assert_eq!(
        status_trail(payable_id).await,
        vec![(String::new(), "OPEN".to_string())],
        "P08 前置：过账后只有一条建单轨迹（NULL→OPEN），还没付过款"
    );

    // 每一步都对账一次：这是财务最关心的一条恒等式，任何一步漂移都是坏账
    let steps = ["1", "3.5", "6.8"];
    for (index, step) in steps.iter().enumerate() {
        pay_ok(&ctx, payable_id, step).await;
        let (_, _, amount_now, paid_now, unpaid_now) = payable_snapshot(stock_in).await;
        common::assert_dec(
            paid_now + unpaid_now,
            amount_now,
            &format!("P08：付了 {step} 之后 paid + unpaid 仍必须 = amount"),
        );
        assert!(
            unpaid_now >= common::dec("0"),
            "P08：未付金额不许为负（实际 {unpaid_now}）"
        );
        // 状态日志只写真实变化：建单 1 条 + 第 1 期（OPEN→PARTIAL_PAID）1 条；
        // 第 2 期是原地流转（PARTIAL_PAID→PARTIAL_PAID）→ 不新增；第 3 期 → SETTLED 再 1 条。
        let trail_now = status_trail(payable_id).await;
        let expected = if index + 1 == steps.len() { 3 } else { 2 };
        assert_eq!(
            trail_now.len(),
            expected,
            "P08：付了 {step} 之后状态日志应有 {expected} 条（建单 1 + 真实变化），\
             实际 {} 条：{trail_now:?}",
            trail_now.len()
        );
    }
    // 11.30 - 1 - 3.5 - 6.8 = 0 → 自动 SETTLED
    let (_, status, _, _, unpaid_final) = payable_snapshot(stock_in).await;
    common::assert_dec(unpaid_final, common::dec("0"), "P08：付完未付归零");
    assert_eq!(status, "SETTLED", "P08：付完必须 SETTLED（金额累加没丢分）");

    // 状态轨迹必须恰好是「建单 + 两次真实流转」，一条空日志都不许有
    let trail = status_trail(payable_id).await;
    assert_eq!(
        trail,
        vec![
            (String::new(), "OPEN".to_string()),
            ("OPEN".to_string(), "PARTIAL_PAID".to_string()),
            ("PARTIAL_PAID".to_string(), "SETTLED".to_string()),
        ],
        "P08：状态日志必须只记真实变化（建单 NULL→OPEN、OPEN→PARTIAL_PAID、\
         PARTIAL_PAID→SETTLED），实际 {trail:?}"
    );
    assert!(
        !trail
            .iter()
            .any(|(from, to)| from == "PARTIAL_PAID" && to == "PARTIAL_PAID"),
        "P08：不许有 PARTIAL_PAID→PARTIAL_PAID 的空日志"
    );
    ctx.cleanup().await;
}

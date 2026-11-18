//! M3-12 真库单测：**核销即释放** —— 收款核销 / 退款核销当场释放客户信用占用。
//!
//! # 这张卡要钉住什么
//!
//! SO-38（ARCH-7 §15.5.2 ②）已经把「应收核销 = 额度释放主出口」接进了
//! `receipt::allocate` 与 `receipt_refund::allocate`，但当时的用例是**绕过核销写路径**的
//! （`credit::tests::scenario_release_on_settlement` 直接 `UPDATE biz_receivable SET
//! received_amount = …` 再调 `sync`）。那测的是「公式对不对」，测不到「核销这条路上
//! 到底有没有调它」—— 钩子被误删、被 `if` 条件挡住、被挪到事务之外，那些用例全绿。
//!
//! 本文件的用例**全部走真接口**：
//!
//! ```text
//! receipt::create ──► receipt::allocate ──► credit_used（缓存列） + biz_credit_log + 回执
//! receipt_refund::create ──► receipt_refund::allocate ──► 同上（反向边）
//! ```
//!
//! 六条口径，每条都配「错成什么样不会被发现」：
//!
//! | # | 口径 | 错成什么样不会被发现 |
//! |:--|:--|:--|
//! | ① | 部分核销 **按增量** 释放（核 400 → 占用 1000 → 600） | 释放成「核销额的四舍五入 / 全额」，账面看不出 |
//! | ② | 收清 → 占用 **归零**（不是「归到容差内」） | 差一分钱永远挂着，S9 容差 0.01 兜住反而更隐蔽 |
//! | ③ | 同一张收款**二次核销**：调大 → 再释放差额；调小 → **占回** | 只处理「变大」，调小永远不占回 ⇒ 客户白拿额度 |
//! | ④ | 重放同一笔核销（增量 0）→ **无回执、无台账、额度不动** | 每重放一次记账一次 ⇒ 台账与额度双重漂移 |
//! | ⑤ | 核销失败（超未收余额）→ **整体回滚**：额度不动、台账 0 行、应收不动 | 额度先释放后被业务校验拒 ⇒ 白放额度（本条最容易漏） |
//! | ⑥ | 退款核销 → 敞口**占回**，`cause = AR_REVERSED`，回执 `released < 0` | 只做正向释放 ⇒ 钱退了额度不回来 |
//!
//! 另外两条「接口契约」断言贯穿其中：
//!
//! * 动作接口返回 `credit_release` 回执，且**详情接口恒为 `None`**（回执是动作结果，
//!   不是单据属性 —— 存库就会变成会过期的假数据）；
//! * 台账是**客户级一行**（一张收款核多张应收也只有一个 `delta`），`ref_no` = 触发单号。
//!
//! # 夹具（前缀 `M312-`，只动自己的行，不整表清）
//!
//! ```text
//! 客户 M312-CUST（额度 1000，策略 BLOCK）
//!   销售订单 SO-M312-*（APPROVED，价税合计 1000）  ← 占用 1000
//!   应收 AR-M312-0001（1000，OPEN）                ← 核销对象
//! ```
//!
//! 客户主数据走 `test_seed::ensure_customer`（P2-62：主数据 INSERT 有预算闸），
//! 期间走 `test_seed::ensure_accounting_period(2026, 11)`（核销要过「期间未结账」闸）。
//! 清场按 `M312-` 前缀从子到父删（`biz_receipt_allocation` 挂 `biz_receivable` 是 RESTRICT）。
//!
//! # 跑法
//!
//! ```bash
//! # 99（开发机，连 145 的 PG）
//! PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m312_release
//! ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_m312_release' \
//!   cargo test --lib business::erp::credit_release_tests -- --test-threads=1
//! # 145（测试机，走 loopback）
//! ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/<库>' \
//!   cargo test --lib business::erp::credit_release_tests -- --test-threads=1
//! ```
//!
//! 缺 `ERP_DB_URL` 时逐条打印 `SKIP:` 后返回（绝不假绿）。

use std::str::FromStr;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::credit;
use crate::business::erp::receipt::{self, AllocateItemReq, AllocateReq, CreateReceiptReq};
use crate::business::erp::receipt_refund::{self, CreateRefundReq, RefundAllocateLineReq, RefundAllocateReq};
use crate::business::erp::voucher::voucher_query::tests::{DB_LOCK, ensure_db};

/// 客户编码（清场前缀；额度 1000 足以做「部分 / 调大 / 调小 / 收清」）。
const CUSTOMER_CODE: &str = "M312-CUST";
/// 客户名（AGENTS 约定 13：给人看的一律名字）。
const CUSTOMER_NAME: &str = "M312 核销释放客户";
/// 应收单号（固定，便于断言台账 / 回执里的 `ref_no`）。
const AR_NO: &str = "AR-M312-0001";
/// 应收金额 = 订单价税合计 = 客户额度（三者相等 ⇒ 占用 1000、可用 0 的边界最好断言）。
const AMOUNT: &str = "1000";
/// 核销 / 退款日期（期间 2026-11，`ensure_accounting_period` 保证 OPEN）。
const DOC_DATE: &str = "2026-11-16";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("定点可解析")
}

/// 执行测试 SQL（`exec_sql` 只收 `String`）。
async fn exec(sql: impl AsRef<str>, params: Vec<Value>) -> DbResult<u64> {
    db().exec_sql(sql.as_ref().to_string(), params).await
}

/// 客户主键 + 「额度 1000 / 策略 BLOCK / 占用清零」（主数据行为 `ensure_customer` 管，属性本函数管）。
async fn fixture_customer() -> Uuid {
    let customer = crate::test_seed::ensure_customer(CUSTOMER_CODE, CUSTOMER_NAME).await;
    exec(
        "UPDATE biz_customer SET credit_limit = 1000, credit_policy = 'BLOCK', \
                credit_used = 0, status = 'ACTIVE' WHERE id = ?::uuid",
        vec![Value::from(&customer)],
    )
    .await
    .expect("回写客户额度配置");
    customer
}

/// 清掉本文件建过的所有 `M312-` **业务**夹具（子 → 父：核销明细挂着应收是 RESTRICT）。
///
/// 也把客户 `credit_used` 归 0：清完订单与应收后公式结果就是 0，于是「缓存列 == 公式」
/// 在收尾后**仍然成立** —— 演练脚本可以在跑完后直接跑 S9 对拍，不会因为用例的收尾手段
/// 把库改成漂移态。
///
/// ⚠ **本函数不删客户行**：它同时被用例中段（每轮重来）和单个用例收尾调用，客户行一删，
/// 调用方手里那个 `customer` 主键就成了悬空 id（实测：`db_concurrent_submit_is_admission_only`
/// 第 2 轮起全部 404「客户不存在」）。删客户行放在 [`purge_customer`]，只给 `setup()` /
/// 收尾用例用。
async fn purge_fixtures() {
    // 状态日志（无外键，但属于本前缀单据的证据行：先按 id 删，再删单据本身）。
    exec(
        "DELETE FROM biz_status_log WHERE \
            (entity_type = 'RECEIVABLE' AND entity_id IN \
                (SELECT id FROM biz_receivable WHERE ar_no LIKE 'AR-M312-%')) \
         OR (entity_type = 'RECEIPT' AND entity_id IN \
                (SELECT id FROM biz_receipt WHERE customer_id IN \
                    (SELECT id FROM biz_customer WHERE code = ?))) \
         OR (entity_type = 'RECEIPT_REFUND' AND entity_id IN \
                (SELECT id FROM biz_receipt_refund WHERE customer_id IN \
                    (SELECT id FROM biz_customer WHERE code = ?))) \
         OR (entity_type = 'SALE_ORDER' AND entity_id IN \
                (SELECT id FROM biz_sales_order WHERE so_no LIKE 'SO-M312-%'))",
        vec![
            Value::Text(CUSTOMER_CODE.to_string()),
            Value::Text(CUSTOMER_CODE.to_string()),
        ],
    )
    .await
    .expect("清状态日志");
    exec(
        "DELETE FROM biz_receipt_refund_allocation WHERE receivable_id IN \
            (SELECT id FROM biz_receivable WHERE ar_no LIKE 'AR-M312-%')",
        vec![],
    )
    .await
    .expect("清退款明细");
    exec(
        "DELETE FROM biz_receipt_allocation WHERE receivable_id IN \
            (SELECT id FROM biz_receivable WHERE ar_no LIKE 'AR-M312-%')",
        vec![],
    )
    .await
    .expect("清核销明细");
    exec(
        "DELETE FROM biz_receipt_refund WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code = ?)",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
    )
    .await
    .expect("清退款单");
    exec(
        "DELETE FROM biz_receipt WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code = ?)",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
    )
    .await
    .expect("清收款单");
    exec("DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-M312-%'", vec![])
        .await
        .expect("清应收");
    exec(
        "DELETE FROM biz_credit_log WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code = ?)",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
    )
    .await
    .expect("清信用台账");
    exec("DELETE FROM biz_sales_order WHERE so_no LIKE 'SO-M312-%'", vec![])
        .await
        .expect("清销售订单");
    exec(
        "UPDATE biz_customer SET credit_used = 0 WHERE code = ?",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
    )
    .await
    .expect("客户额度归零（清完单据后公式结果本来就是 0）");
}

/// 整段清场：业务夹具 + **客户行本身**（`setup()` 与收尾用例用）。
///
/// 客户行为什么必须删：`setup()` 的清场只在每条用例**开头**跑，留着客户就等于
/// 「最后一条用例的客户永久留在库里」—— 演练脚本
/// （`scripts/verify/m326_sales_gap_closure_drill.sh`）量的是「跑完与基线是否一致」，
/// 客户行不清就会把「整个演练净增量」顶成非 0（M3-26 实测 56 个 `M3xx-` 客户）。
/// 删掉不影响下一轮：`ensure_customer` 的 id 由 `customer:<code>` 派生（确定性），
/// 重建还是同一行。
async fn purge_customer() {
    purge_fixtures().await;
    // 客户行最后删（业务行已先删完，此时不会撞 `biz_customer_*` 的 RESTRICT 子表）
    crate::test_seed::purge_customers(CUSTOMER_CODE).await;
}

/// 清场 + 建夹具（客户 / 期间 / 订单 / 应收），返回（客户, 应收 id）。
///
/// 清场只动 `M312-` 前缀的行：与 `credit::tests`（`CR38-`）、`aging_partial_tests`（`AG22-`）
/// 互不干扰，所以本文件不需要独占整库（仍按仓规 `--test-threads=1` 跑）。
async fn setup() -> Option<(Uuid, Uuid)> {
    if !ensure_db().await {
        return None;
    }
    purge_customer().await;

    let customer = fixture_customer().await;
    // 期间必须存在且未结账（核销会过 `period::ensure_period_writable`）。
    crate::test_seed::ensure_accounting_period(2026, 11).await;
    exec(
        "UPDATE biz_accounting_period SET status = 'OPEN' WHERE period_code = '2026-11'",
        vec![],
    )
    .await
    .expect("期间置 OPEN");

    // 销售订单：APPROVED（唯一占用时机的落点），价税合计 = 额度。
    let suffix = crate::test_seed::uniq_suffix();
    exec(
        "INSERT INTO biz_sales_order \
            (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, 'APPROVED', ?::text::numeric, 0, ?::text::numeric)",
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(format!("SO-M312-{suffix}")),
            Value::from(&customer),
            Value::Text(AMOUNT.to_string()),
            Value::Text(AMOUNT.to_string()),
        ],
    )
    .await
    .expect("插销售订单");

    // 应收：OPEN，一分没收（核销对象）。
    let receivable = Uuid::now_v7();
    exec(
        "INSERT INTO biz_receivable \
            (id, ar_no, customer_id, source_type, source_id, source_no, currency_code, amount, \
             received_amount, unsettled_amount, credit_days, due_date, period_code, status, \
             created_at, updated_at) \
         VALUES (?::uuid, ?, ?::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY', \
                 ?::text::numeric, 0, ?::text::numeric, 30, CURRENT_DATE + 30, '2026-11', 'OPEN', \
                 now(), now())",
        vec![
            Value::from(&receivable),
            Value::Text(AR_NO.to_string()),
            Value::from(&customer),
            Value::Text(AMOUNT.to_string()),
            Value::Text(AMOUNT.to_string()),
        ],
    )
    .await
    .expect("插应收单");

    // 「下单占用」这一脚：与生产同一入口（审批通过时由 `sales_approval_bridge::settle` 调）。
    credit::sync(db(), customer, credit::CAUSE_ORDER_OCCUPY, "SO-M312-SETUP")
        .await
        .expect("初始占用同步");
    assert_eq!(
        credit::stored(db(), customer).await.expect("读缓存列"),
        Some(dec(AMOUNT)),
        "夹具前提：客户此刻占满 1000"
    );
    Some((customer, receivable))
}

/// 收尾：清掉本用例的全部夹具（口径同 [`purge_fixtures`]）。
///
/// 收完「缓存列 == 公式」仍然成立（没有订单 / 应收 ⇒ 公式 = 0 = 缓存列），
/// 所以演练脚本跑完可以直接对整库跑 S9，不会看到用例造成的假漂移。
/// 参数保留是为了调用点一行不改（`teardown(customer)`）。
async fn teardown(_customer: Uuid) {
    purge_fixtures().await;
}

/// 登记一张收款单（走真接口：取号 / 期间闸 / 汇率锁定 / 状态日志全部生效）。
async fn create_receipt(customer: Uuid, amount: &str) -> receipt::ReceiptDetailRes {
    receipt::create(
        None,
        CreateReceiptReq {
            customer_id: customer.to_string(),
            receipt_date: Some(DOC_DATE.to_string()),
            amount: dec(amount),
            method: Some("BANK_TRANSFER".to_string()),
            bank_account: Some("6222 0000 0000 1112".to_string()),
            remark: Some("M3-12 核销即释放夹具".to_string()),
            currency_code: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("登记收款失败（{amount}）：{error:?}"))
}

/// 核销（走真接口）。
async fn allocate(receipt_id: &str, items: &[(&str, &str)]) -> std::result::Result<receipt::ReceiptDetailRes, String> {
    let req = AllocateReq {
        items: items
            .iter()
            .map(|(receivable_id, amount)| AllocateItemReq {
                receivable_id: (*receivable_id).to_string(),
                amount: dec(amount),
                remark: None,
            })
            .collect(),
    };
    receipt::allocate(None, receipt_id, req)
        .await
        .map_err(|error| format!("{:?}", error))
}

/// 客户当前的已占额度（缓存列）。
async fn used(customer: Uuid) -> Decimal {
    credit::stored(db(), customer)
        .await
        .expect("读缓存列")
        .expect("客户存在")
}

/// 台账行（`cause` / `ref_no` / `delta` / `before` / `after` / `trigger`），按入账顺序。
async fn ledger(customer: Uuid) -> Vec<(String, String, Decimal, Decimal, Decimal, String)> {
    let rows = db()
        .query_sql(
            "SELECT cause, COALESCE(ref_no, '') AS ref_no, delta::text AS delta, \
                    before_used::text AS before_used, after_used::text AS after_used, trigger_source \
               FROM biz_credit_log WHERE customer_id = ?::uuid ORDER BY created_at, id"
                .to_string(),
            vec![Value::from(&customer)],
        )
        .await
        .expect("读信用台账");
    rows.iter()
        .map(|row| {
            (
                row.get_typed::<String>("cause").expect("cause"),
                row.get_typed::<String>("ref_no").expect("ref_no"),
                row.get_typed::<String>("delta")
                    .expect("delta")
                    .parse::<Decimal>()
                    .expect("delta 定点"),
                row.get_typed::<String>("before_used")
                    .expect("before")
                    .parse::<Decimal>()
                    .expect("before 定点"),
                row.get_typed::<String>("after_used")
                    .expect("after")
                    .parse::<Decimal>()
                    .expect("after 定点"),
                row.get_typed::<String>("trigger_source").expect("trigger"),
            )
        })
        .collect()
}

/// 应收三个金额 + 状态。
async fn receivable_state(receivable: Uuid) -> (Decimal, Decimal, String) {
    let row = db()
        .query_first_sql(
            "SELECT received_amount::text AS received, unsettled_amount::text AS unsettled, status \
               FROM biz_receivable WHERE id = ?::uuid"
                .to_string(),
            vec![Value::from(&receivable)],
        )
        .await
        .expect("查应收")
        .expect("应收存在");
    (
        row.get_typed::<String>("received")
            .expect("received")
            .parse::<Decimal>()
            .expect("received 定点"),
        row.get_typed::<String>("unsettled")
            .expect("unsettled")
            .parse::<Decimal>()
            .expect("unsettled 定点"),
        row.get_typed::<String>("status").expect("status"),
    )
}

// ==================== ① 部分核销 → 按增量释放 ====================

/// 核销 400：占用 1000 → 600，台账一行 `AR_SETTLED / -400`，回执 1000→600。
///
/// 这是卡面的主场景：「钱到了一部分，额度立刻还一部分」，且**当场**（不等 02:10 重算）。
#[tokio::test]
async fn db_partial_allocate_releases_by_delta() {
    let _guard = DB_LOCK.lock().await;
    let Some((customer, receivable)) = setup().await else {
        return;
    };

    let created = create_receipt(customer, AMOUNT).await;
    let receipt_id = created.receipt.id.clone();
    let receipt_no = created.receipt.receipt_no.clone();

    let detail = allocate(&receipt_id, &[(&receivable.to_string(), "400")])
        .await
        .expect("部分核销应成功");

    // ① 缓存列 / 公式 / 台账 / 回执四处同源
    assert_eq!(used(customer).await, dec("600"), "核销 400 ⇒ 占用 1000 → 600");
    let usage = credit::usage_of(db(), customer).await.expect("现算口径");
    assert_eq!(usage.used, dec("600"), "现算值必须与缓存列一致（S9 同源）");

    let release = detail.credit_release.expect("动过额度 ⇒ 必须有回执");
    assert_eq!(release.cause, credit::CAUSE_AR_SETTLED);
    assert_eq!(release.ref_no, receipt_no, "回执的 ref_no = 收款单号");
    assert_eq!(release.customer_id, customer.to_string());
    assert_eq!(release.before_used, dec("1000"));
    assert_eq!(release.after_used, dec("600"));
    assert_eq!(release.released, dec("400"), "released 正数 = 释放");

    let rows = ledger(customer).await;
    assert_eq!(rows.len(), 2, "初始占用 1 行 + 本次核销 1 行");
    let (cause, ref_no, delta, before, after, trigger) = rows[1].clone();
    assert_eq!(cause, credit::CAUSE_AR_SETTLED);
    assert_eq!(ref_no, receipt_no);
    assert_eq!(delta, dec("-400"), "台账 delta 是 after - before（与回执相反数）");
    assert_eq!((before, after), (dec("1000"), dec("600")));
    assert_eq!(trigger, "API", "单据动作记 API（只有重算记 JOB）");

    // 应收被真回写（不是只释放额度）
    assert_eq!(
        receivable_state(receivable).await,
        (dec("400"), dec("600"), "PARTIAL".to_string())
    );
    teardown(customer).await;
}

// ==================== ② 收清 → 占用归零 ====================

/// 核销满 1000：占用 1000 → 0，应收 SETTLED，回执 `released = 1000`。
#[tokio::test]
async fn db_full_settlement_zeroes_credit() {
    let _guard = DB_LOCK.lock().await;
    let Some((customer, receivable)) = setup().await else {
        return;
    };

    let created = create_receipt(customer, AMOUNT).await;
    let detail = allocate(&created.receipt.id, &[(&receivable.to_string(), AMOUNT)])
        .await
        .expect("收清应成功");

    assert_eq!(used(customer).await, Decimal::ZERO, "钱全收到 ⇒ 额度全额释放");
    assert_eq!(
        receivable_state(receivable).await,
        (dec("1000"), Decimal::ZERO, "SETTLED".to_string())
    );
    let release = detail.credit_release.expect("收清必须有回执");
    assert_eq!(release.released, dec("1000"));
    assert_eq!(release.after_used, Decimal::ZERO);
    teardown(customer).await;
}

// ==================== ③ 二次核销：调大释放差额 / 调小占回 ====================

/// 同一张收款对同一张应收：400 → 500（再释放 100）→ 300（**占回** 200）→ 重放 300（无变化）。
///
/// 调小这一脚最容易被写成「只处理变大」：`received_amount` 变小了、额度却不占回来，
/// 客户就白拿 200 的额度，而且任何账都不会报错。
#[tokio::test]
async fn db_reallocate_tracks_delta_both_ways_and_replay_is_idempotent() {
    let _guard = DB_LOCK.lock().await;
    let Some((customer, receivable)) = setup().await else {
        return;
    };

    let created = create_receipt(customer, AMOUNT).await;
    let receipt_id = created.receipt.id.clone();
    let ar = receivable.to_string();

    allocate(&receipt_id, &[(&ar, "400")]).await.expect("首次核销");
    assert_eq!(used(customer).await, dec("600"));

    // 调大 500：只释放差额 100
    let grown = allocate(&receipt_id, &[(&ar, "500")]).await.expect("调大核销额");
    let grown_release = grown.credit_release.expect("调大动了额度 ⇒ 有回执");
    assert_eq!(grown_release.released, dec("100"), "增量 100 ⇒ 只释放 100");
    assert_eq!(used(customer).await, dec("500"));

    // 调小 300：敞口占回 200
    let shrunk = allocate(&receipt_id, &[(&ar, "300")]).await.expect("调小核销额");
    let shrunk_release = shrunk.credit_release.expect("调小动了额度 ⇒ 有回执");
    assert_eq!(shrunk_release.released, dec("-200"), "调小 = 占回，released 为负");
    assert_eq!(used(customer).await, dec("700"));

    // 重放同一笔（增量 0）：无回执、无台账、额度不动
    let rows_before = ledger(customer).await.len();
    let replay = allocate(&receipt_id, &[(&ar, "300")]).await.expect("重放应成功");
    assert!(
        replay.credit_release.is_none(),
        "增量 0 不许产生回执（有回执 ⇔ 有台账 ⇔ 额度变了）"
    );
    assert_eq!(ledger(customer).await.len(), rows_before, "重放不许记账");
    assert_eq!(used(customer).await, dec("700"), "重放不许改额度");

    // 详情接口恒不带回执（动作结果不落库、也不冒充单据属性）
    let detail = receipt::receipt_detail(Uuid::parse_str(&receipt_id).expect("id"))
        .await
        .expect("读详情");
    assert!(
        detail.credit_release.is_none(),
        "GET 详情不许带动作回执（会变成会过期的假数据）"
    );
    teardown(customer).await;
}

// ==================== ④ 核销失败 → 整体回滚（不许白放额度） ====================

/// 核销 1200 到 1000 的应收：接口 4xx，**额度、台账、应收一个都不许动**。
///
/// 若把 `credit::sync` 挪到事务之外、或先释放再做业务校验，这条会红。
#[tokio::test]
async fn db_failed_allocate_leaves_credit_untouched() {
    let _guard = DB_LOCK.lock().await;
    let Some((customer, receivable)) = setup().await else {
        return;
    };

    let created = create_receipt(customer, AMOUNT).await;
    let error = allocate(&created.receipt.id, &[(&receivable.to_string(), "1200")])
        .await
        .expect_err("超未收余额必须被拒");
    assert!(
        error.contains("1200") || error.contains("未收") || error.contains("超过"),
        "错误文案要点名金额或余额：{error}"
    );

    assert_eq!(used(customer).await, dec(AMOUNT), "失败的核销不许释放额度");
    assert_eq!(
        ledger(customer).await.len(),
        1,
        "失败的核销不许记账（只剩初始占用那行）"
    );
    assert_eq!(
        receivable_state(receivable).await,
        (Decimal::ZERO, dec(AMOUNT), "OPEN".to_string()),
        "失败的核销不许回写应收"
    );
    let detail = receipt::receipt_detail(Uuid::parse_str(&created.receipt.id).expect("id"))
        .await
        .expect("读详情");
    assert_eq!(
        detail.receipt.allocated_amount,
        Decimal::ZERO,
        "失败的核销不许动已核销金额"
    );
    teardown(customer).await;
}

// ==================== ⑤ 退款核销 → 敞口占回 ====================

/// 收清（占用 0）后退 400：占用回到 400，台账 `AR_REVERSED`，回执 `released = -400`。
///
/// 这是「② 的反向边」：钱退出去了，敞口必须回来 —— 只做正向释放的系统会在这里永久少占。
#[tokio::test]
async fn db_refund_reoccupies_credit() {
    let _guard = DB_LOCK.lock().await;
    let Some((customer, receivable)) = setup().await else {
        return;
    };

    // 先收清：占用 1000 → 0
    let receipt = create_receipt(customer, AMOUNT).await;
    allocate(&receipt.receipt.id, &[(&receivable.to_string(), AMOUNT)])
        .await
        .expect("收清");
    assert_eq!(used(customer).await, Decimal::ZERO);

    // 退款 400 冲回该应收的已收款。
    //
    // 不挂 `source_receipt_id`：挂了就必须是**已过账**的收款才有钱可退
    // （`receipt_refund::create` 的闸），而本用例要验的是「退款核销 → 敞口占回」这一脚，
    // 不必把收款过账（凭证 / 附件 / 科目）整条链拉进来 —— 那是收款卡的用例。
    let refund = receipt_refund::create(
        None,
        CreateRefundReq {
            customer_id: customer.to_string(),
            source_receipt_id: None,
            refund_date: Some(DOC_DATE.to_string()),
            amount: dec("400"),
            method: Some("BANK_TRANSFER".to_string()),
            bank_account: Some("6222 0000 0000 1112".to_string()),
            reason: "客户退货，退款 400".to_string(),
            remark: Some("M3-12 退款核销夹具".to_string()),
        },
    )
    .await
    .unwrap_or_else(|error| panic!("登记退款失败：{error:?}"));
    let refund_no = refund.header.refund_no.clone();

    let detail = receipt_refund::allocate(
        None,
        &refund.header.id,
        RefundAllocateReq {
            allocations: vec![RefundAllocateLineReq {
                receivable_id: receivable.to_string(),
                amount: dec("400"),
                remark: None,
            }],
        },
    )
    .await
    .unwrap_or_else(|error| panic!("退款核销失败：{error:?}"));

    assert_eq!(used(customer).await, dec("400"), "退 400 ⇒ 敞口占回 400");
    assert_eq!(
        receivable_state(receivable).await,
        (dec("600"), dec("400"), "PARTIAL".to_string()),
        "退款把已收从 1000 冲回 600"
    );

    let release = detail.credit_release.expect("退款动了额度 ⇒ 有回执");
    assert_eq!(release.cause, credit::CAUSE_AR_REVERSED);
    assert_eq!(release.ref_no, refund_no, "回执 ref_no = 退款单号");
    assert_eq!(release.before_used, Decimal::ZERO);
    assert_eq!(release.after_used, dec("400"));
    assert_eq!(release.released, dec("-400"), "占回 ⇒ released 为负");

    let rows = ledger(customer).await;
    let (cause, ref_no, delta, before, after, _) = rows.last().cloned().expect("台账非空");
    assert_eq!(cause, credit::CAUSE_AR_REVERSED);
    assert_eq!(ref_no, refund_no);
    assert_eq!(delta, dec("400"), "台账 delta 与回执 released 相反数");
    assert_eq!((before, after), (Decimal::ZERO, dec("400")));
    teardown(customer).await;
}

// ==================== ⑥ 回执构造：只在真动额度时才有 ====================

/// 纯逻辑：`CreditRelease::from_sync` 的「没变化 ⇒ None」与正负号约定。
#[test]
fn credit_release_is_only_built_when_credit_moved() {
    let customer = Uuid::now_v7();
    let unchanged = credit::CreditSync {
        customer_id: customer,
        before: dec("600"),
        after: dec("600"),
        repaired: false,
    };
    assert!(credit::CreditRelease::from_sync(&unchanged, credit::CAUSE_AR_SETTLED, "RC-1").is_none());

    let released = credit::CreditSync {
        customer_id: customer,
        before: dec("1000"),
        after: dec("600"),
        repaired: true,
    };
    let receipt =
        credit::CreditRelease::from_sync(&released, credit::CAUSE_AR_SETTLED, "RC-1").expect("动过额度必须有回执");
    assert_eq!(receipt.released, dec("400"), "before > after ⇒ 正数（释放）");
    assert_eq!(receipt.ref_no, "RC-1");
    assert_eq!(receipt.cause, credit::CAUSE_AR_SETTLED);

    let reoccupied = credit::CreditSync {
        customer_id: customer,
        before: dec("0"),
        after: dec("400"),
        repaired: true,
    };
    let refund =
        credit::CreditRelease::from_sync(&reoccupied, credit::CAUSE_AR_REVERSED, "RR-1").expect("占回也有回执");
    assert_eq!(refund.released, dec("-400"), "after > before ⇒ 负数（占回）");
}

/// 回执的**报文契约**（M3-14 客户信用页 / 前端核销 toast 直接读这几个键）。
///
/// 字段名改了前端就静默拿到 `undefined` —— 这种破坏不会让后端编译失败，所以钉在这里。
#[test]
fn credit_release_wire_contract_is_stable() {
    let release = credit::CreditRelease {
        customer_id: "0b0e5a4e-0000-4000-8000-000000000001".to_string(),
        cause: credit::CAUSE_AR_SETTLED.to_string(),
        ref_no: "RC-20261117-000001".to_string(),
        before_used: dec("1000"),
        after_used: dec("600"),
        released: dec("400"),
    };
    let json = serde_json::to_value(&release).expect("回执必须可序列化");
    for key in [
        "customer_id",
        "cause",
        "ref_no",
        "before_used",
        "after_used",
        "released",
    ] {
        assert!(json.get(key).is_some(), "报文缺字段 {key}：{json}");
    }
    assert_eq!(json["cause"], credit::CAUSE_AR_SETTLED);
    assert_eq!(json["ref_no"], "RC-20261117-000001");
    // 金额在本仓一律是**字符串**（`rust_decimal` 开了 `serde-str`，与 `ReceiptRes.amount`
    // 等全仓 DTO 一致）：前端不能用 `typeof === 'number'` 判，也不用 `Number()` 硬转
    // （大额会丢精度）。这条钉住「别只给这一个字段换类型」。
    assert_eq!(json["released"], "400", "released 是定点字符串（本仓金额统一口径）");
    assert_eq!(json["before_used"], "1000");
    assert_eq!(json["after_used"], "600");
}

/// 收尾：`M312-` 夹具一条不留（业务单据 / 应收 / 信用台账 / **客户行**）。
///
/// 为什么必须有这条：`setup()` 里的清场只在**开头**跑，最后一条用例的夹具会留在库里；
/// 演练脚本（`scripts/verify/m326_sales_gap_closure_drill.sh`）按「跑完 vs 基线」判净增量，
/// 客户行不清就会让整条演练报非 0（M3-26 实测 56 个 `M3xx-` 客户）。`zz_` 排序最后，正好收尾。
///
/// 业务行按自己的单号前缀判、信用台账按客户的**确定性 id**（`test_seed::fixture_uuid`）判 ——
/// 客户行一删，用 `code` 反查就永远是 0（空转），所以这里不能用 `code`。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    if setup().await.is_none() {
        return;
    }
    purge_customer().await;

    let customer_id = crate::test_seed::fixture_uuid(&format!("customer:{CUSTOMER_CODE}"));
    let checks: [(&str, String); 4] = [
        (
            "夹具客户",
            format!("SELECT count(*) FROM biz_customer WHERE code = '{CUSTOMER_CODE}'"),
        ),
        (
            "销售订单",
            "SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'SO-M312-%'".to_string(),
        ),
        (
            "应收",
            "SELECT count(*) FROM biz_receivable WHERE ar_no LIKE 'AR-M312-%'".to_string(),
        ),
        (
            "信用台账",
            format!("SELECT count(*) FROM biz_credit_log WHERE customer_id = '{customer_id}'"),
        ),
    ];
    for (label, sql) in checks {
        let left: i64 = db()
            .scalar_with_params::<Option<i64>>(&sql, vec![])
            .await
            .unwrap_or_else(|error| panic!("统计{label}残留失败：{error}"))
            .unwrap_or(0);
        assert_eq!(left, 0, "{label} 必须零残留（残留会让演练净增量非 0）");
    }
    println!("[M3-13] 清场：M312- 夹具 0 残留（含客户行）✓");
}

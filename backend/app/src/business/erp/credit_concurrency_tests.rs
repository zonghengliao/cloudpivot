//! M3-13 真库单测：**客户信用的并发场景（并发下单 / 部分收款）+ 竞态**。
//!
//! # 这张卡要钉住什么
//!
//! M3-09（策略三态）/ M3-10（实时占用）/ M3-11（超信用拦截）/ M3-12（核销即释放）四条卡
//! 建的都是**单线程语义**：一次请求、一个事件、一个断言。真实销售链不是这样跑的 ——
//! 同一客户名下的「占用」与「释放」是并发事件：
//!
//! ```text
//! 占用：提交（DRAFT → SUBMITTED，管道占用）→ 审批通过（SUBMITTED → APPROVED，落 credit_used）
//! 释放：收款核销（写 received_amount → 现算重落 credit_used）
//! ```
//!
//! 本文件的每条用例都是**两个以上 tokio 任务真并发**（`multi_thread` 运行时 + `Barrier`
//! 同步起跑），断言的是「并发之后仍然成立的不变量」，不是某一种特定交错：
//!
//! | # | 不变量 | 错成什么样不会被发现 |
//! |:--|:--|:--|
//! | ① | **敞口 ≤ 额度**：任何时刻 `APPROVED` 起的订单合计不许超 `credit_limit` | 两条并发请求各自「先读后判」⇒ 双双放行 ⇒ 超卖；单线程用例永远测不到 |
//! | ② | **`credit_used` == 公式**（S9 同源） | CAS 重试写坏 / 丢更新 ⇒ 缓存列停在旧值，界面显示的可用额度是假的 |
//! | ③ | **台账首尾相接**：`before_used(i) == after_used(i-1)`，且 `Σdelta == 末值 − 初值` | 两个会话各自写一行「1000→700」，账面上看着两条释放，实际只该释放一次 |
//! | ④ | **重放不双扣**：同一笔核销并发重放，释放只发生一次 | `received_amount` 被写成两份 ⇒ 客户白得额度，而任何一条单据校验都不会报错 |
//!
//! # 竞态模型：三道闸各自承诺什么（**结论，不是假设**）
//!
//! 1. **提交闸**（`sales_order_handler::ensure_submittable`）= **入场预估**，不是承诺。
//!    它读的是「已提交快照」（`credit_used` 缓存列 + `SUBMITTED` 合计），两条提交并发时
//!    彼此看不见 ⇒ **可能双双过闸**。用例 ① 如实钉住这一点，并断言它的后果无钱损失：
//!    两张单都进 `SUBMITTED`、`credit_used` 一分不涨；代价是两张都过不了审批闸，
//!    撤掉一张即可放行另一张（可恢复，不是死单）。
//! 2. **审批闸**（`sales_approval_bridge::guard_credit_on_approve` + `settle`）= **承诺**。
//!    在审单互相计入 `pending`，所以**合计超限时任何一张都落不了 `APPROVED`**（方向保守：
//!    宁可两张都拦，也不许超卖）。用例 ③ 用 `credit::assess_order`（闸用的同一个函数）
//!    并发跑两条审批，断言「零落库 + 超限额精确到 200」。
//! 3. **核销**（`receipt::allocate`）= **行锁 + CAS**。`SELECT … FOR UPDATE` 锁住收款单与
//!    应收行（按 `receivable_id` 排序防交叉死锁），额度同步走 [`credit::sync`] 的
//!    CAS 重试（[`credit::SYNC_ATTEMPTS`] 轮）。用例 ④⑤⑥ 断言串行化的**证据**：
//!    台账链首尾相接、`Σdelta` 等于总变动、重放只释放一次。
//!
//! # 并发用例的前提：**先预热连接池**（不预热 = 假绿）
//!
//! 池是惰性建连的（`min_connections = 1`）。冷池时第二条任务的**第一条**语句要付一次完整
//! 握手（99 → 145 实测 ~20ms），两条「并发」任务于是被错开成串行 —— 守卫总能读到对方
//! **已提交**的状态，竞态分支永远不执行，用例照样全绿。本卡实测：同一条并发提交用例，
//! 不预热两轮都走「串行」分支，预热 2 条连接后同刻起跑、**双双进在审**。
//! 所有并发段之前都调 [`warm_pool`]，并在用例 ① 里断言「3 轮内至少命中一次竞态」——
//! 命中不了就说明这条用例的竞态断言是死代码。
//!
//! # 与上游卡的关系（不重复测）
//!
//! * M3-11 已用**真流程引擎**（`biz_flow_instance` / 待办 / 审批回调）覆盖单线程语义，
//!   本文件不重测流程引擎本身，只在**数据层**并发压「闸 → 写状态 → 落额度」这一脚；
//! * M3-12 已覆盖核销释放的单线程六口径（部分 / 收清 / 调大 / 调小 / 重放 / 退款），
//!   本文件不重测那些口径，只压**同一张 / 相邻几张应收被并发核销**时的增量与竞态；
//! * 本文件不改生产代码（单测卡），也不新增迁移（下一个可用号仍 V0438）。
//!
//! # 夹具（前缀 `M313-`，只动自己的行）
//!
//! ```text
//! 客户 M313-CUST（额度 1000，策略 BLOCK，占用清零）
//!   销售订单 SO-M313-*（DRAFT / SUBMITTED / APPROVED 按用例摆）
//!   应收     AR-M313-000N（OPEN，一分没收）
//!   收款     RCP-*（走 receipt::create 真接口：取号 / 期间闸 / 汇率锁定）
//! ```
//!
//! 清场按 `M313-` 前缀**从子到父**删（`biz_receipt_allocation` 挂 `biz_receivable` 是
//! RESTRICT），最后把 `credit_used` 归 0 ⇒ 收尾后「缓存列 == 公式」仍成立（可对整库跑 S9）。
//!
//! # 跑法
//!
//! ```bash
//! # 99（开发机，连 145 的 PG）
//! PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m313_concurrency
//! ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_m313_concurrency' \
//!   cargo test --lib business::erp::credit_concurrency_tests -- --test-threads=1
//! # 145（测试机，走 loopback）
//! ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/<库>' \
//!   cargo test --lib business::erp::credit_concurrency_tests -- --test-threads=1
//! ```
//!
//! 缺 `ERP_DB_URL` 时逐条打印 `SKIP:` 后返回（**绝不假绿**）。

use std::str::FromStr;
use std::sync::Arc;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use tokio::sync::Barrier;

use crate::business::erp::credit;
use crate::business::erp::receipt::{self, AllocateItemReq, AllocateReq, CreateReceiptReq};
use crate::business::erp::sales::sales_order_handler::{self, SalesOrderStatusReq};
use crate::business::erp::voucher::voucher_query::tests::{DB_LOCK, ensure_db};

/// 客户编码（清场前缀）与名称（AGENTS 约定 13：给人看的一律名字）。
const CUSTOMER_CODE: &str = "M313-CUST";
const CUSTOMER_NAME: &str = "M313 并发信用客户";
/// 客户额度：所有用例的「超 / 不超」都围绕这个数（600 + 600 超，400 + 400 不超）。
const LIMIT: &str = "1000";
/// 核销日期（期间 2026-11，`ensure_accounting_period` 保证 OPEN）。
const DOC_DATE: &str = "2026-11-16";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("定点可解析")
}

/// 执行测试 SQL（`exec_sql` 只收 `String`）。
async fn exec(sql: impl AsRef<str>, params: Vec<Value>) -> DbResult<u64> {
    db().exec_sql(sql.as_ref().to_string(), params).await
}

/// 执行测试 SQL（失败即 panic，夹具用）。
async fn exec_ok(sql: impl AsRef<str>, params: Vec<Value>, what: &str) {
    exec(sql, params)
        .await
        .unwrap_or_else(|error| panic!("{what} 失败：{error}"));
}

/// 客户主键 + 「额度 1000 / 策略 BLOCK / 占用清零」。
async fn fixture_customer() -> Uuid {
    let customer = crate::test_seed::ensure_customer(CUSTOMER_CODE, CUSTOMER_NAME).await;
    exec_ok(
        "UPDATE biz_customer SET credit_limit = ?::text::numeric, credit_policy = 'BLOCK', \
                credit_used = 0, status = 'ACTIVE' WHERE id = ?::uuid",
        vec![Value::Text(LIMIT.to_string()), Value::from(&customer)],
        "回写客户额度配置",
    )
    .await;
    customer
}

/// 清掉本文件建过的所有 `M313-` **业务**夹具（子 → 父），并把客户缓存列归 0。
///
/// 收尾把 `credit_used` 归 0：清完订单与应收后公式结果就是 0，于是
/// 「缓存列 == 公式」在收尾后**仍然成立** —— 演练脚本可以跑完直接对整库跑 S9。
///
/// ⚠ **本函数不删客户行**：它同时被用例**中段**调用（`db_concurrent_submit_is_admission_only`
/// 每轮重来，见 `for round in 1..=3`），客户行一删，用例手里那个 `customer` 主键就悬空了
/// （实测：第 2 轮起提交全部 404「客户不存在」）。删客户行放在 [`purge_customer`]。
async fn purge_fixtures() {
    // 状态日志（无外键，但属于本前缀单据的证据行：先按 id 删）。
    exec_ok(
        "DELETE FROM biz_status_log WHERE \
            (entity_type = 'SALE_ORDER' AND entity_id IN \
                (SELECT id FROM biz_sales_order WHERE so_no LIKE 'SO-M313-%')) \
         OR (entity_type = 'RECEIVABLE' AND entity_id IN \
                (SELECT id FROM biz_receivable WHERE ar_no LIKE 'AR-M313-%')) \
         OR (entity_type = 'RECEIPT' AND entity_id IN \
                (SELECT id FROM biz_receipt WHERE customer_id IN \
                    (SELECT id FROM biz_customer WHERE code = ?)))",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
        "清状态日志",
    )
    .await;
    exec_ok(
        "DELETE FROM biz_receipt_allocation WHERE receivable_id IN \
            (SELECT id FROM biz_receivable WHERE ar_no LIKE 'AR-M313-%')",
        vec![],
        "清核销明细",
    )
    .await;
    exec_ok(
        "DELETE FROM biz_receipt WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code = ?)",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
        "清收款单",
    )
    .await;
    exec_ok(
        "DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-M313-%'",
        vec![],
        "清应收",
    )
    .await;
    exec_ok(
        "DELETE FROM biz_credit_log WHERE customer_id IN \
            (SELECT id FROM biz_customer WHERE code = ?)",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
        "清信用台账",
    )
    .await;
    exec_ok(
        "DELETE FROM biz_sales_order_item WHERE so_id IN \
            (SELECT id FROM biz_sales_order WHERE so_no LIKE 'SO-M313-%')",
        vec![],
        "清订单明细",
    )
    .await;
    exec_ok(
        "DELETE FROM biz_sales_order WHERE so_no LIKE 'SO-M313-%'",
        vec![],
        "清销售订单",
    )
    .await;
    exec_ok(
        "UPDATE biz_customer SET credit_used = 0 WHERE code = ?",
        vec![Value::Text(CUSTOMER_CODE.to_string())],
        "客户额度归零（清完单据后公式结果本来就是 0）",
    )
    .await;
}

/// 整段清场：业务夹具 + **客户行本身**（`setup()` 与收尾用例用，用例中段不许调）。
///
/// 客户行为什么必须删：`setup()` 的清场只在每条用例**开头**跑，留着客户就等于
/// 「最后一条用例的客户永久留在库里」—— 演练脚本
/// （`scripts/verify/m326_sales_gap_closure_drill.sh`）量的是「跑完与基线是否一致」，
/// 客户行不清就会把「整个演练净增量」顶成非 0。删掉不影响下一轮：
/// `ensure_customer` 的 id 由 `customer:<code>` 派生（确定性），重建还是同一行。
async fn purge_customer() {
    purge_fixtures().await;
    // 客户行最后删（业务行已先删完，此时不会撞 `biz_customer_*` 的 RESTRICT 子表）
    crate::test_seed::purge_customers(CUSTOMER_CODE).await;
}

/// 清场 + 建客户 + 打开期间；返回客户主键（`None` = 没有 `ERP_DB_URL`，调用方 print SKIP）。
async fn setup() -> Option<Uuid> {
    if !ensure_db().await {
        return None;
    }
    purge_customer().await;
    let customer = fixture_customer().await;
    // 核销要过「期间未结账」闸。
    crate::test_seed::ensure_accounting_period(2026, 11).await;
    exec_ok(
        "UPDATE biz_accounting_period SET status = 'OPEN' WHERE period_code = '2026-11'",
        vec![],
        "期间置 OPEN",
    )
    .await;
    Some(customer)
}

/// 插一张销售订单（含 1 行明细），返回（id, so_no）。
///
/// `status` 直接落到目标态：`DRAFT` / `SUBMITTED` 用于「提交 / 审批」用例，
/// `APPROVED` 用于「已经把额度占上」的夹具前提（调用方随后调 [`sync_credit`] 让缓存列跟上）。
async fn add_order(customer: Uuid, amount: &str, status: &str) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let so_no = format!("SO-M313-{}", crate::test_seed::uniq_suffix());
    exec_ok(
        "INSERT INTO biz_sales_order \
            (id, so_no, customer_id, order_date, status, total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?, ?::text::numeric, 0, ?::text::numeric)",
        vec![
            Value::from(&id),
            Value::Text(so_no.clone()),
            Value::from(&customer),
            Value::Text(status.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
        ],
        "插销售订单",
    )
    .await;
    exec_ok(
        "INSERT INTO biz_sales_order_item \
            (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount) \
         VALUES (?::uuid, 1, gen_random_uuid(), 1, ?::text::numeric, ?::text::numeric, 0, 0)",
        vec![
            Value::from(&id),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
        ],
        "插订单明细",
    )
    .await;
    (id, so_no)
}

/// 插一张应收（`OPEN`，一分没收），返回 id。
async fn add_receivable(customer: Uuid, ar_no: &str, amount: &str) -> Uuid {
    let id = Uuid::now_v7();
    exec_ok(
        "INSERT INTO biz_receivable \
            (id, ar_no, customer_id, source_type, source_id, source_no, currency_code, amount, \
             received_amount, unsettled_amount, credit_days, due_date, period_code, status, \
             created_at, updated_at) \
         VALUES (?::uuid, ?, ?::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', 'CNY', \
                 ?::text::numeric, 0, ?::text::numeric, 30, CURRENT_DATE + 30, '2026-11', 'OPEN', \
                 now(), now())",
        vec![
            Value::from(&id),
            Value::Text(ar_no.to_string()),
            Value::from(&customer),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
        ],
        "插应收单",
    )
    .await;
    id
}

/// 让缓存列跟上公式（夹具里手插订单 / 应收之后调一次，等价「这些单据已经走过占用出口」）。
async fn sync_credit(customer: Uuid, ref_no: &str) {
    credit::sync(db(), customer, credit::CAUSE_ORDER_OCCUPY, ref_no)
        .await
        .expect("夹具：同步客户信用额度");
}

/// 客户当前的已占额度（缓存列）。
async fn used(customer: Uuid) -> Decimal {
    credit::stored(db(), customer)
        .await
        .expect("读缓存列")
        .expect("客户存在")
}

/// 现算口径（三张表现算，与 S9 / `credit::usage_of` 同源）。
async fn formula(customer: Uuid) -> Decimal {
    credit::usage_of(db(), customer).await.expect("现算口径").used
}

/// 单据状态。
async fn status_of(order_id: Uuid) -> String {
    db().scalar_with_params(
        "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("查订单状态")
}

/// 「敞口」= 处于占用集合的订单含税合计（不许超过额度）。
async fn exposure(customer: Uuid) -> Decimal {
    let text: String = db()
        .scalar_with_params(
            "SELECT COALESCE(SUM(total_with_tax), 0)::text FROM biz_sales_order \
              WHERE customer_id = ?::uuid AND deleted_at IS NULL \
                AND status IN ('APPROVED', 'PARTIAL_OUT', 'OUTBOUND')",
            vec![Value::from(&customer)],
        )
        .await
        .expect("查敞口");
    dec(&text)
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

/// **不变量 ②**：缓存列 == 现算 == 期望值（三处同源，容差 0.01）。
async fn assert_converged(customer: Uuid, expected: &str, what: &str) {
    let stored = used(customer).await;
    let computed = formula(customer).await;
    assert!(
        credit::is_same(stored, computed),
        "{what}：缓存列 {stored} 与公式 {computed} 漂移（S9 会红，说明 CAS 丢了更新）"
    );
    assert!(
        credit::is_same(stored, dec(expected)),
        "{what}：已占额度应为 {expected}，实际 {stored}"
    );
}

/// **不变量 ③**：台账首尾相接、`Σdelta` == 总变动、末行 == 最终缓存列。
///
/// 这是「CAS 真把并发写串行化了」的**直接证据**：没有 CAS（或 CAS 用错列）时，
/// 两个会话会各写一行 `before` 相同的台账，链立刻断。
fn assert_ledger_chain(
    rows: &[(String, String, Decimal, Decimal, Decimal, String)],
    first_before: &str,
    final_after: &str,
) {
    assert!(!rows.is_empty(), "台账不能为空（占用 / 释放都该留痕）");
    assert_eq!(
        rows[0].3,
        dec(first_before),
        "台账第一行的 before 必须等于夹具初始占用（链断在开头）"
    );
    for (idx, pair) in rows.windows(2).enumerate() {
        assert_eq!(
            pair[1].3,
            pair[0].4,
            "第 {} 行的 before 必须等于上一行的 after（{:?} → {:?}）：并发写没串行化",
            idx + 1,
            pair[0],
            pair[1]
        );
    }
    let last = rows.last().expect("非空");
    assert_eq!(last.4, dec(final_after), "台账最后一行的 after 必须等于最终缓存列");
    let sum = rows
        .iter()
        .map(|row| row.2)
        .fold(Decimal::ZERO, |acc, delta| acc + delta);
    assert_eq!(
        sum,
        dec(final_after) - dec(first_before),
        "台账 delta 之和必须等于总变动（多出来的行 = 同一笔释放记了两遍）"
    );
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
            bank_account: Some("6222 0000 0000 1114".to_string()),
            remark: Some("M3-13 并发夹具".to_string()),
            currency_code: None,
        },
    )
    .await
    .unwrap_or_else(|error| panic!("登记收款失败（{amount}）：{error:?}"))
}

/// 核销（走真接口；错误转成字符串便于并发任务返回）。
async fn allocate(
    receipt_id: &str,
    items: &[(String, &str)],
) -> std::result::Result<receipt::ReceiptDetailRes, String> {
    let req = AllocateReq {
        items: items
            .iter()
            .map(|(receivable_id, amount)| AllocateItemReq {
                receivable_id: receivable_id.clone(),
                amount: dec(amount),
                remark: None,
            })
            .collect(),
    };
    receipt::allocate(None, receipt_id, req)
        .await
        .map_err(|error| format!("{:?}", error))
}

/// 提交（`DRAFT → SUBMITTED`，真接口 `PATCH /status` 的同一条路）：提交闸就在这里。
async fn submit_order(order_id: Uuid) -> std::result::Result<(), String> {
    sales_order_handler::change_sales_order_status(
        None,
        &order_id.to_string(),
        SalesOrderStatusReq {
            target_status: "SUBMITTED".to_string(),
            reason: None,
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("{:?}", error))
}

/// 「审批点同意」的数据层等价：先用闸用的同一个 [`credit::assess_order`] 判，通过才落
/// `SUBMITTED → APPROVED`（后者是 `apply_status`，额度占用在同一事务里发生）。
///
/// 流程引擎侧的接线（待办 / 回调 / 打回草稿）由 M3-11 的用例覆盖，本文件只压并发。
async fn approve_if_allowed(order_id: Uuid) -> std::result::Result<(), String> {
    let assessed = credit::assess_order(db(), order_id)
        .await
        .map_err(|error| format!("{error:?}"))?
        .ok_or_else(|| "单据不存在".to_string())?;
    if !assessed.allows() {
        return Err(format!("审批点被拦：{}", assessed.block_message()));
    }
    sales_order_handler::change_sales_order_status(
        None,
        &order_id.to_string(),
        SalesOrderStatusReq {
            target_status: "APPROVED".to_string(),
            reason: None,
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("{:?}", error))
}

/// 作废订单（`SUBMITTED → CANCELLED`，这条边要求 reason —— 给人留「为什么撤」）。
async fn cancel_order(order_id: Uuid, reason: &str) -> std::result::Result<(), String> {
    sales_order_handler::change_sales_order_status(
        None,
        &order_id.to_string(),
        SalesOrderStatusReq {
            target_status: "CANCELLED".to_string(),
            reason: Some(reason.to_string()),
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("{:?}", error))
}

/// 预热连接池：并发发起 `n` 条 no-op 查询，让池里**真的躺着** `n` 条已建好的连接。
///
/// # 为什么必须预热（本卡实测，不预热就是假绿）
///
/// 池是**惰性**建连的（`min_connections = 1`）：冷池时第二条任务的**第一条**语句要付一次
/// 完整握手（99 → 145 实测 ~20ms），于是两条「并发」任务被错开成串行 —— 守卫读到的是
/// 对方**已经提交**的状态，竞态分支永远不执行，用例照样全绿。
///
/// 实测对照（同一条并发提交用例）：不预热 → 两轮都走「串行」分支（`SUBMITTED / DRAFT`）；
/// 预热 2 条连接 → 两条任务同刻起跑、守卫互相看不见，**双双进在审**（`SUBMITTED / SUBMITTED`），
/// 竞态分支才真的跑起来。生产环境池是热的，并发是常态 —— 用例必须模拟那个状态。
async fn warm_pool(n: usize) {
    let mut handles = Vec::new();
    for _ in 0..n {
        handles.push(tokio::spawn(async move {
            db().query_sql("SELECT 1::text AS one".to_string(), vec![])
                .await
                .expect("预热查询");
        }));
    }
    for handle in handles {
        handle.await.expect("预热任务 panic");
    }
}

// ==================== ① 并发提交：提交闸是「入场预估」，可双双过闸，但敞口不涨 ====================

/// 两张 `DRAFT` 单（各 600，额度 1000）**同时提交**。
///
/// 提交闸读的是「已提交快照」（`credit_used` + `SUBMITTED` 合计），两个请求彼此看不见
/// ⇒ 可能双双进 `SUBMITTED`。这条用例**如实钉住竞态**，并断言它的后果**没有钱上的损失**：
///
/// * `credit_used` 一分不涨（占用只发生在 `APPROVED`）；
/// * 不出现 `APPROVED` 单；
/// * **双过闸时两张都过不了审批闸**（`assess_order` 互相把对方算进在审，超 200）；
/// * 撤掉一张之后另一张可正常审批 —— 可恢复，不是死单。
///
/// 竞态是否命中由调度决定（**必须先 [`warm_pool`]**，否则冷池建连握手会把两条任务错开成
/// 串行，测出来的是「串行」而不是竞态），所以最多跑 3 轮、断言「至少一轮命中」：
/// 命中不了说明竞态分支是死代码，用例本身就没意义了。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_concurrent_submit_is_admission_only() {
    let _guard = DB_LOCK.lock().await;
    let Some(customer) = setup().await else {
        return;
    };

    let mut raced = false;
    for round in 1..=3 {
        // 每轮重来：清掉上一轮的订单 / 台账，客户 credit_used 归 0
        purge_fixtures().await;

        let (order_a, no_a) = add_order(customer, "600", "DRAFT").await;
        let (order_b, no_b) = add_order(customer, "600", "DRAFT").await;
        warm_pool(2).await;

        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for order_id in [order_a, order_b] {
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                submit_order(order_id).await
            }));
        }
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.expect("提交任务 panic"));
        }
        let passed_through = results.iter().filter(|result| result.is_ok()).count();
        assert!(
            passed_through == 1 || passed_through == 2,
            "第 {round} 轮：过闸数只能是 1（串行）或 2（竞态），实际 {passed_through}：{results:?}"
        );

        // ① 敞口不涨：提交不占额度（占用唯一时机 = APPROVED）
        assert_eq!(
            used(customer).await,
            Decimal::ZERO,
            "第 {round} 轮：提交不占额度，credit_used 必须还是 0"
        );
        assert_eq!(exposure(customer).await, Decimal::ZERO, "第 {round} 轮：不许出现敞口");
        assert_converged(customer, "0", "并发提交后").await;

        let status_a = status_of(order_a).await;
        let status_b = status_of(order_b).await;
        for status in [&status_a, &status_b] {
            assert!(
                status == "DRAFT" || status == "SUBMITTED",
                "第 {round} 轮：提交的终态只能是 DRAFT（被闸拦）或 SUBMITTED（过闸），实际 {status}"
            );
        }
        assert!(
            status_a == "SUBMITTED" || status_b == "SUBMITTED",
            "第 {round} 轮：至少有一张必须过闸（过闸方看不见对方，不可能双双被拒）"
        );

        if status_a == "SUBMITTED" && status_b == "SUBMITTED" {
            // ② 双过闸 = 竞态命中：审批闸必须把两张都拦住（在审 600 + 本单 600 = 1200 > 1000）
            raced = true;
            for order_id in [order_a, order_b] {
                let assessed = credit::assess_order(db(), order_id)
                    .await
                    .expect("评估信用")
                    .expect("单据存在");
                assert!(
                    !assessed.allows(),
                    "两张都进在审时，任何一张都不许被判定为「可审批」（否则就是超卖）"
                );
                assert_eq!(
                    assessed.snapshot().expect("超限必有快照").over(),
                    dec("200"),
                    "两张 600 在审 + 额度 1000 ⇒ 超 200"
                );
            }
            println!(
                "[M3-13 ①] 第 {round} 轮命中竞态：{no_a} / {no_b} 双双进在审（入场预估），\
                 审批闸两张都拦住（超 200）✓"
            );

            // ③ 可恢复：撤掉一张 → 另一张可审批 → 敞口 600 ≤ 额度（不是死单）
            cancel_order(order_b, "M3-13 并发提交竞态：撤回超额在审单")
                .await
                .expect("作废多余在审单");
            approve_if_allowed(order_a)
                .await
                .expect("撤掉一张后 600 ≤ 1000 应可审批");
            assert_eq!(status_of(order_a).await, "APPROVED");
            assert_eq!(
                exposure(customer).await,
                dec("600"),
                "最终敞口 600 ≤ 额度 1000（超卖 0）"
            );
            assert_converged(customer, "600", "并发提交 + 撤回 + 审批后").await;
            break;
        }

        // 没命中竞态（后到者看见了先到者的 SUBMITTED）：这一张必须可审，不许误伤
        let submitted = if status_a == "SUBMITTED" { order_a } else { order_b };
        let assessed = credit::assess_order(db(), submitted)
            .await
            .expect("评估信用")
            .expect("单据存在");
        assert!(
            assessed.allows(),
            "第 {round} 轮：只有一张进在审时它必须可审（600 ≤ 1000），否则提交闸误伤"
        );
        println!("[M3-13 ①] 第 {round} 轮未命中竞态（后到者看见了在审单）：一张过闸、一张被拦 ✓");
    }
    assert!(
        raced,
        "3 轮并发提交都没命中竞态：预热不足或环境把两笔请求串行化了 —— \
         这条用例的竞态断言就成了死代码，必须让它在真并发下跑出来"
    );

    teardown(customer).await;
}

// ==================== ② 并发审批（都在额度内）：两张都要落地，CAS 不丢更新 ====================

/// 两张在审单（各 400，额度 1000，合计 800 ≤ 1000）**同时审批通过**。
///
/// 这是「并发下单」的**正例**：既不许误拦，也不许丢更新。两张各自把对方算进在审
/// （400 + 400 + 400 = 1200？不 —— 己方在审不计入本单，`exclude_order_id` 排除自己），
/// 所以两边都判「800 ≤ 1000」放行；落库时额度同步走 CAS，最终必须是 800 而不是 400。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_concurrent_approve_within_limit_both_land() {
    let _guard = DB_LOCK.lock().await;
    let Some(customer) = setup().await else {
        return;
    };

    let (order_a, no_a) = add_order(customer, "400", "SUBMITTED").await;
    let (order_b, no_b) = add_order(customer, "400", "SUBMITTED").await;

    warm_pool(2).await;
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for order_id in [order_a, order_b] {
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            approve_if_allowed(order_id).await
        }));
    }
    for handle in handles {
        handle.await.expect("审批任务 panic").expect("额度内的在审单应可审批");
    }

    assert_eq!(status_of(order_a).await, "APPROVED", "{no_a} 应已审批通过");
    assert_eq!(status_of(order_b).await, "APPROVED", "{no_b} 应已审批通过");
    assert_eq!(
        exposure(customer).await,
        dec("800"),
        "两张 400 都应占上：丢更新会停在 400"
    );
    assert_converged(customer, "800", "并发审批（额度内）后").await;

    // 台账链：初始 0 → 400 → 800（两行的 before/after 必须首尾相接）
    let rows = ledger(customer).await;
    assert_eq!(rows.len(), 2, "两次占用 = 两行台账，实际 {}", rows.len());
    assert_ledger_chain(&rows, "0", "800");
    // 占用行都记 ORDER_OCCUPY / ORDER_STATUS，ref_no 是订单号（对账时能回查）
    for row in &rows {
        assert_eq!(row.0, credit::CAUSE_ORDER_OCCUPY, "占用行的 cause");
        assert!(
            row.1 == no_a || row.1 == no_b,
            "台账 ref_no 应是本次涉及的订单号之一，实际 {}",
            row.1
        );
        assert_eq!(row.5, "API", "单据动作记 API（只有重算记 JOB）");
    }

    teardown(customer).await;
}

// ==================== ③ 并发审批（合计超限）：一张都不许落 ====================

/// 两张在审单（各 600，额度 1000，合计 1200 > 1000）**同时审批**。
///
/// 在审单互相计入 `pending` ⇒ 无论交错如何，**两张都会被闸拦住**（方向保守：宁可两张
/// 都拦，也不许超卖）。断言：零落库、敞口 0、超限额精确 200、单据留在 `SUBMITTED`
/// （待办不动 —— 调额 / 收款后可重批）
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_concurrent_approve_over_limit_neither_lands() {
    let _guard = DB_LOCK.lock().await;
    let Some(customer) = setup().await else {
        return;
    };

    let (order_a, no_a) = add_order(customer, "600", "SUBMITTED").await;
    let (order_b, no_b) = add_order(customer, "600", "SUBMITTED").await;

    warm_pool(2).await;
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for order_id in [order_a, order_b] {
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            approve_if_allowed(order_id).await
        }));
    }
    let mut errors = Vec::new();
    for handle in handles {
        if let Err(error) = handle.await.expect("审批任务 panic") {
            errors.push(error);
        }
    }

    assert_eq!(errors.len(), 2, "两张都该被审批闸拦住：{errors:?}");
    assert!(
        errors.iter().all(|error| error.contains("审批点被拦")),
        "拦截理由必须是信用闸（不是别的错）：{errors:?}"
    );
    assert_eq!(status_of(order_a).await, "SUBMITTED", "{no_a} 不许落 APPROVED");
    assert_eq!(status_of(order_b).await, "SUBMITTED", "{no_b} 不许落 APPROVED");
    assert_eq!(exposure(customer).await, Decimal::ZERO, "超限单一张都不许占额度");
    assert_converged(customer, "0", "并发审批（超限）后").await;
    assert!(
        ledger(customer).await.is_empty(),
        "没有任何一次占用发生 ⇒ 台账必须为空（空台账 = 没有假放行记录）"
    );

    // 可恢复：撤一张 → 另一张 600 ≤ 1000 可审批；敞口 600，不是 0（钱能赚回来）
    cancel_order(order_b, "M3-13 并发超限：撤回一张给另一张让额度")
        .await
        .expect("作废一张在审单");
    approve_if_allowed(order_a)
        .await
        .expect("撤掉一张后 600 ≤ 1000 应可审批");
    assert_eq!(status_of(order_a).await, "APPROVED");
    assert_eq!(exposure(customer).await, dec("600"), "最终敞口 600");
    assert_converged(customer, "600", "撤回 + 审批后").await;

    teardown(customer).await;
}

// ==================== ④ 同一张收款单并发核销同一张应收：重放只释放一次 ====================

/// 一张收款（1000）、一张应收（1000，占用 1000），**同一个核销请求并发发两次**（各 300）。
///
/// 语义是「设置该 (收款, 应收) 的核销额」而不是「累加」，所以正确结果是 300 而不是 600：
/// `SELECT … FOR UPDATE` 锁住收款单 ⇒ 后到者读到 `previous = 300`、增量 0 ⇒
/// **不写库、不记台账、不回执**。这条钉住「并发重放不双扣」。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_concurrent_same_receipt_allocate_releases_once() {
    let _guard = DB_LOCK.lock().await;
    let Some(customer) = setup().await else {
        return;
    };

    add_order(customer, "1000", "APPROVED").await;
    let receivable = add_receivable(customer, "AR-M313-0001", "1000").await;
    sync_credit(customer, "SO-M313-SETUP").await;
    assert_eq!(used(customer).await, dec("1000"), "夹具前提：客户此刻占满 1000");

    let created = create_receipt(customer, "1000").await;
    let receipt_id = created.receipt.id.clone();
    let receipt_no = created.receipt.receipt_no.clone();
    let ar = receivable.to_string();

    warm_pool(2).await;
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let receipt_id = receipt_id.clone();
        let ar = ar.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            allocate(&receipt_id, &[(ar, "300")]).await
        }));
    }
    let mut oks = Vec::new();
    for handle in handles {
        oks.push(handle.await.expect("核销任务 panic").expect("并发重放不应失败"));
    }

    // ① 应收只被核销一次（300，不是 600）
    assert_eq!(
        receivable_state(receivable).await,
        (dec("300"), dec("700"), "PARTIAL".to_string()),
        "同一 (收款, 应收) 并发两次 ⇒ 终值是设置的 300，不是累加的 600"
    );
    // ② 额度只释放一次
    assert_converged(customer, "700", "并发重放核销后").await;
    let rows = ledger(customer).await;
    assert_eq!(
        rows.len(),
        2,
        "初始占用 1 行 + 本次释放 1 行（重放没有新增行），实际 {:?}",
        rows.iter().map(|row| row.0.clone()).collect::<Vec<_>>()
    );
    assert_ledger_chain(&rows, "0", "700");
    assert_eq!(rows[1].0, credit::CAUSE_AR_SETTLED, "释放行的 cause");
    assert_eq!(rows[1].1, receipt_no, "释放行 ref_no = 收款单号");
    assert_eq!(rows[1].2, dec("-300"), "增量 300 ⇒ 台账 delta -300");

    // ③ 回执：只有真动额度的那个有回执（另一个是增量 0 的重放）
    let with_release: Vec<_> = oks.iter().filter(|res| res.credit_release.is_some()).collect();
    let without: Vec<_> = oks.iter().filter(|res| res.credit_release.is_none()).collect();
    assert_eq!(with_release.len(), 1, "两次并发重放只有一次真释放 ⇒ 只有一条回执");
    assert_eq!(without.len(), 1, "增量 0 的那次不许有回执（假释放）");
    assert_eq!(
        with_release[0].credit_release.as_ref().expect("回执").released,
        dec("300")
    );

    teardown(customer).await;
}

// ==================== ⑤ 多张收款并发核销同一张应收：串行化 + 台账链 ====================

/// 4 张收款（各 200）**并发**核销同一张应收（1000，占用 1000），每张核 100。
///
/// 断言串行化的证据（不变量 ②③）：终值 400、占用 600、台账 5 行首尾相接、
/// `Σdelta == -400`；任何丢更新都会让「末行 after」或「链」对不上。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_concurrent_multi_receipts_serialize_on_one_receivable() {
    let _guard = DB_LOCK.lock().await;
    let Some(customer) = setup().await else {
        return;
    };

    add_order(customer, "1000", "APPROVED").await;
    let receivable = add_receivable(customer, "AR-M313-0001", "1000").await;
    sync_credit(customer, "SO-M313-SETUP").await;

    let mut receipts = Vec::new();
    for _ in 0..4 {
        receipts.push(create_receipt(customer, "200").await.receipt.id.clone());
    }

    warm_pool(4).await;
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for receipt_id in receipts {
        let barrier = Arc::clone(&barrier);
        let ar = receivable.to_string();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            allocate(&receipt_id, &[(ar, "100")]).await
        }));
    }
    for handle in handles {
        handle
            .await
            .expect("核销任务 panic")
            .expect("4 张收款并发核销同一应收（合计 400 ≤ 1000）都应成功");
    }

    assert_eq!(
        receivable_state(receivable).await,
        (dec("400"), dec("600"), "PARTIAL".to_string()),
        "4 × 100 必须都落上（丢一笔就是少收钱）"
    );
    assert_converged(customer, "600", "4 笔并发部分核销后").await;
    assert_eq!(
        exposure(customer).await,
        dec("1000"),
        "订单敞口本身不变（核销不动订单）"
    );

    let rows = ledger(customer).await;
    assert_eq!(rows.len(), 5, "初始占用 1 行 + 4 笔释放各 1 行");
    assert_ledger_chain(&rows, "0", "600");
    let released: Decimal = rows
        .iter()
        .filter(|row| row.0 == credit::CAUSE_AR_SETTLED)
        .map(|row| row.2)
        .fold(Decimal::ZERO, |acc, delta| acc + delta);
    assert_eq!(released, dec("-400"), "4 笔释放合计 -400（不是 -100 / -800）");

    teardown(customer).await;
}

// ==================== ⑥ 并发核销合计超应收余额：一胜一拒，余额不为负 ====================

/// 2 张收款（各 600）**并发**对同一张应收（1000）各核 600：合计 1200 > 1000。
///
/// 行锁把两笔串行化 ⇒ 后到者读到「可核销余额 400 < 600」而被拒（400 + 业务文案）。
/// 断言：恰好 1 成功 1 失败、应收 600/400 `PARTIAL`、占用 400、台账只多 1 行、
/// **`unsettled_amount` 从不为负**（库层 `ck_biz_ar_unsettled` 兜底之外，业务层也要拦）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_concurrent_allocate_over_balance_one_wins_one_rejected() {
    let _guard = DB_LOCK.lock().await;
    let Some(customer) = setup().await else {
        return;
    };

    add_order(customer, "1000", "APPROVED").await;
    let receivable = add_receivable(customer, "AR-M313-0001", "1000").await;
    sync_credit(customer, "SO-M313-SETUP").await;

    let first = create_receipt(customer, "600").await.receipt.id.clone();
    let second = create_receipt(customer, "600").await.receipt.id.clone();

    warm_pool(2).await;
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for receipt_id in [first, second] {
        let barrier = Arc::clone(&barrier);
        let ar = receivable.to_string();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            allocate(&receipt_id, &[(ar, "600")]).await
        }));
    }
    let mut oks = 0;
    let mut errs = Vec::new();
    for handle in handles {
        match handle.await.expect("核销任务 panic") {
            Ok(_) => oks += 1,
            Err(error) => errs.push(error),
        }
    }

    assert_eq!(oks, 1, "合计 1200 > 1000 ⇒ 只许一笔成功，实际 {oks} 笔");
    assert_eq!(errs.len(), 1, "另一笔必须被余额闸拒掉：{errs:?}");
    assert!(
        errs[0].contains("余额") || errs[0].contains("核销"),
        "拒绝理由要说清「余额不够」，实际 {}",
        errs[0]
    );

    let (received, unsettled, status) = receivable_state(receivable).await;
    assert_eq!(
        (received, unsettled),
        (dec("600"), dec("400")),
        "赢了的那笔 600，余额 400"
    );
    assert_eq!(status, "PARTIAL");
    assert!(
        unsettled >= Decimal::ZERO,
        "未结金额不许为负（库层 ck_biz_ar_unsettled 之外业务层也要拦）"
    );
    assert_converged(customer, "400", "并发超余额核销后").await;

    let rows = ledger(customer).await;
    assert_eq!(rows.len(), 2, "只有赢了的那笔释放过额度");
    assert_ledger_chain(&rows, "0", "400");

    teardown(customer).await;
}

// ==================== ⑦ 占用与释放并发（同客户）：三道事件收敛到公式 ====================

/// 同一客户上**同时**发生：1 次占用（400 → 700，审批通过） + 2 次释放（各 100）。
///
/// 这是生产里最常见的并发形态：销售在批单，财务在核销。三条事件各自的 `credit::sync`
/// 在 `biz_customer` 那一行上撞车，CAS 必须把它们的写入串行化 —— 断言：
///
/// * 终值 = 700（订单）+ 0（退货） − 200（已核销） = **500**；
/// * 台账 4 行（初始 400 + 占用 + 释放 + 释放）首尾相接，`Σdelta == +100`；
/// * 缓存列 == 公式（没有一次 sync 因为 CAS 用尽而静默丢）；
/// * 两笔核销都拿到了自己的回执（动作接口不静默）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_concurrent_occupy_and_releases_converge() {
    let _guard = DB_LOCK.lock().await;
    let Some(customer) = setup().await else {
        return;
    };

    // 夹具前提：一张已占用的订单 400 + 一张应收 400（占用 400）
    add_order(customer, "400", "APPROVED").await;
    let receivable = add_receivable(customer, "AR-M313-0001", "400").await;
    sync_credit(customer, "SO-M313-SETUP").await;
    assert_eq!(used(customer).await, dec("400"));
    // 待审批的订单 300（审批通过 → 占用 700）
    let (pending_order, pending_no) = add_order(customer, "300", "SUBMITTED").await;
    let receipt_a = create_receipt(customer, "100").await.receipt.id.clone();
    let receipt_b = create_receipt(customer, "100").await.receipt.id.clone();

    warm_pool(3).await;
    let barrier = Arc::new(Barrier::new(3));
    // 三个任务的返回统一成 `Result<Option<Decimal>, String>`：
    // `Some(released)` = 本次核销释放的额度（回执里那个数），`None` = 审批通过（不是核销动作）。
    let mut handles: Vec<tokio::task::JoinHandle<std::result::Result<Option<Decimal>, String>>> = Vec::new();
    {
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            approve_if_allowed(pending_order).await.map(|_| None)
        }));
    }
    for receipt_id in [receipt_a, receipt_b] {
        let barrier = Arc::clone(&barrier);
        let ar = receivable.to_string();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            allocate(&receipt_id, &[(ar, "100")])
                .await
                .map(|detail| detail.credit_release.map(|release| release.released))
        }));
    }
    let mut released_amounts = Vec::new();
    for handle in handles {
        let released = handle.await.expect("并发任务 panic").expect("并发占用 / 释放都应成功");
        released_amounts.push(released);
    }

    assert_eq!(status_of(pending_order).await, "APPROVED", "{pending_no} 应已审批通过");
    assert_eq!(exposure(customer).await, dec("700"), "订单敞口 400 + 300");
    assert_eq!(
        receivable_state(receivable).await,
        (dec("200"), dec("200"), "PARTIAL".to_string()),
        "两笔各 100 都要落上"
    );
    assert_converged(customer, "500", "占用与释放并发后").await;

    // 台账：初始占用 400 → … → 500，四行首尾相接
    let rows = ledger(customer).await;
    assert_eq!(rows.len(), 4, "初始 1 + 占用 1 + 释放 2 = 4 行");
    assert_ledger_chain(&rows, "0", "500");

    // 两笔核销都带回了自己的回执（动作结果可见，不是静默释放）
    let releases: Vec<Decimal> = released_amounts.into_iter().flatten().collect();
    assert_eq!(releases.len(), 2, "两笔核销各一条回执，实际 {releases:?}");
    assert!(
        releases.iter().all(|released| *released == dec("100")),
        "每笔释放 100：{releases:?}"
    );

    teardown(customer).await;
}

/// 收尾：清掉本用例的全部夹具（口径同 [`purge_fixtures`]）。
async fn teardown(_customer: Uuid) {
    purge_fixtures().await;
}

/// 收尾：`M313-` 夹具一条不留（订单 / 明细 / 应收 / 信用台账 / **客户行**）。
///
/// 为什么必须有这条：`setup()` 里的清场只在**开头**跑，最后一条用例的夹具会留在库里；
/// 演练脚本（`scripts/verify/m326_sales_gap_closure_drill.sh`）按「跑完 vs 基线」判净增量，
/// 客户行不清就会让整条演练报非 0。`zz_` 排序最后，正好收尾。
///
/// 台账 / 明细这类挂着「客户」的行按客户的**确定性 id**（`test_seed::fixture_uuid`）判：
/// 客户行一删，用 `code` 反查永远是 0（空转）。
#[tokio::test]
async fn zz_teardown_purges_fixtures() {
    if setup().await.is_none() {
        return;
    }
    purge_customer().await;

    let customer_id = crate::test_seed::fixture_uuid(&format!("customer:{CUSTOMER_CODE}"));
    let checks: [(&str, String); 5] = [
        (
            "夹具客户",
            format!("SELECT count(*) FROM biz_customer WHERE code = '{CUSTOMER_CODE}'"),
        ),
        (
            "销售订单",
            "SELECT count(*) FROM biz_sales_order WHERE so_no LIKE 'SO-M313-%'".to_string(),
        ),
        (
            "订单明细",
            "SELECT count(*) FROM biz_sales_order_item WHERE so_id IN \
               (SELECT id FROM biz_sales_order WHERE so_no LIKE 'SO-M313-%')"
                .to_string(),
        ),
        (
            "应收",
            "SELECT count(*) FROM biz_receivable WHERE ar_no LIKE 'AR-M313-%'".to_string(),
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
    println!("[M3-13] 清场：M313- 夹具 0 残留（含客户行）✓");
}

// ============================================================================
// payable_linkage_db_tests.rs —— 真库：应付单（PAYABLE）付款路径的跨单据联动（LNK-03）
//
// 为什么必须真库：本卡交付的是「配一条规则，应付单一付款它真的执行，而且**不多扣一次**」——
// 纯逻辑单测只能证明「源码里出现了调用」，证明不了
// 「规则读得到 → 事务内 / 提交后真的跑了 → 通知落到 sys_notice
//  → 原地流转（PARTIAL_PAID → PARTIAL_PAID）不重复触发
//  → 排除清单里的余额 / 凭证动作确实没跑（钱只扣一次、凭证只有一张）」。
//
// 所以全部用例走**生产同款路径**：
//
// ```text
// 夹具：P7 的供应商 / 会计期间 / 科目（`payable_payment::tests::setup`）
//   直付入口 payable_payment::pay_payable
//     → pay_in_tx（登记付款记录）
//     → settle_payment_in_tx（凭证 → 回写 → 状态推进 → ⑤ 联动）
//     → commit → payable_linkage::run_deferred_after_commit（通知 / 建任务）
// ```
//
// 用法：
// ```bash
// export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/<测试库>'
// cargo test --lib business::erp::payable_payment::tests::payable_linkage_db -- --nocapture --test-threads=1
// ```
// 缺 `ERP_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。
//
// # 夹具卫生
//
// 规则按 `name LIKE 'LNK03%'` 定位（**不带连字符**：真机取证脚本的规则名是
// 「LNK03 应付结清 → 通知制单人」，而唯一索引
// `uk_flow_linkage_rule(trigger_entity, trigger_status, action_code)` 不含
// `WHERE deleted_at IS NULL`，软删的取证规则仍占坑位）；通知按幂等键前缀
// `LNK:PAYABLE:%` 清；任务按标题 `LNK03 %` 清；应付单 / 付款 / 凭证由 P7 夹具
// 的 `setup()` 按 `AP-P7-%` 根清。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::*;
use crate::business::erp::payable_payment::{self, PayPayableReq};
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::business::shared::state_machine;

/// 夹具规则的命名前缀（与其它卡 / 生产的规则互不影响）。
const RULE_PREFIX: &str = "LNK03";

/// 夹具付款操作人 / 制单人（非超管，避免任何提权短路；`sys_user` 里由本模块自建）。
const PAYER: &str = "1a6b0001-0000-4000-8000-000000000001";

/// 执行一条 SQL（失败即 panic，测试里不做容错）。
async fn run_sql(sql: &str, params: Vec<Value>, label: &str) {
    db().execute_with_params(sql, params)
        .await
        .unwrap_or_else(|error| panic!("{label}：{error}"));
}

/// 数一行（失败即 panic）。
async fn count_of(sql: &str, params: Vec<Value>, label: &str) -> i64 {
    db().scalar_with_params::<i64>(sql, params)
        .await
        .unwrap_or_else(|error| panic!("{label}：{error}"))
}

fn payer() -> Uuid {
    Uuid::parse_str(PAYER).expect("夹具 UUID 非法")
}

/// 夹具收尾：规则 / 通知 / 任务 / 夹具账号一把清。
async fn teardown() {
    run_sql(
        &format!("DELETE FROM biz_flow_linkage_rule WHERE name LIKE '{RULE_PREFIX}%'"),
        vec![],
        "清理 LNK-03 联动规则夹具",
    )
    .await;
    run_sql(
        "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:PAYABLE:%'",
        vec![],
        "清理 LNK-03 联动通知",
    )
    .await;
    run_sql(
        "DELETE FROM biz_task WHERE title LIKE 'LNK03 %'",
        vec![],
        "清理 LNK-03 联动任务",
    )
    .await;
    run_sql(
        "DELETE FROM sys_user WHERE username = 'lnk03_payer'",
        vec![],
        "清理 LNK-03 夹具账号",
    )
    .await;
}

/// 建夹具账号（付款操作人 = 制单人 = 通知收件人），幂等。
async fn ensure_payer() {
    run_sql(
        &format!(
            "INSERT INTO sys_user (id, username, password, real_name, status) \
             VALUES ('{PAYER}'::uuid, 'lnk03_payer', 'x', 'LNK03 付款人', 1) \
             ON CONFLICT DO NOTHING"
        ),
        vec![],
        "建 LNK-03 夹具账号",
    )
    .await;
}

/// 插一条 `PAYABLE/<status>` 上的规则。
async fn add_rule(status: &str, action: &str, params: &str) {
    run_sql(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), ?, 'PAYABLE', ?, ?, ?::jsonb, 10) \
         ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE \
            SET name = EXCLUDED.name, action_params = EXCLUDED.action_params, \
                enabled = TRUE, deleted_at = NULL",
        vec![
            Value::Text(format!("{RULE_PREFIX}-{status}-{action}")),
            Value::Text(status.to_string()),
            Value::Text(action.to_string()),
            // jsonb 参数必须用 `Value::Json`（与 `linkage::model::upsert_rule` 同口径）
            Value::Json(params.to_string()),
        ],
        "插入联动规则夹具",
    )
    .await;
}

/// 把制单人写进应付单（`new_payable` 不写 `create_by`，而通知的 `recipient = CREATOR` 要用）。
async fn assign_creator(payable_id: Uuid) {
    run_sql(
        "UPDATE biz_payable SET create_by = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&payer()), Value::from(&payable_id)],
        "写应付单制单人",
    )
    .await;
}

/// 按幂等键数通知条数。
async fn notices_of_key(key: &str) -> i64 {
    count_of(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(key.to_string())],
        "查联动通知",
    )
    .await
}

/// 数本卡探针任务。
async fn probe_tasks() -> i64 {
    count_of(
        "SELECT count(*) FROM biz_task WHERE title = 'LNK03 付款复核'",
        vec![],
        "查联动任务",
    )
    .await
}

/// 某个应付单最新一笔付款的 id（凭证按 `source_id` 挂在付款记录上）。
async fn last_payment_id(payable_id: Uuid) -> Uuid {
    db().scalar_with_params(
        "SELECT id FROM biz_payment WHERE payable_id = ?::uuid AND deleted_at IS NULL \
          ORDER BY created_at DESC LIMIT 1",
        vec![Value::from(&payable_id)],
    )
    .await
    .expect("查付款记录")
}

/// 某个付款记录名下的付款凭证条数（**应当是 1**：付款事务第 ② 步那一张）。
async fn payment_vouchers(payment_id: Uuid) -> i64 {
    count_of(
        "SELECT count(*) FROM biz_voucher WHERE source_type = ? AND source_id = ?::uuid \
           AND deleted_at IS NULL",
        vec![
            Value::Text(crate::business::erp::voucher::SOURCE_PAYMENT.to_string()),
            Value::from(&payment_id),
        ],
        "查付款凭证",
    )
    .await
}

/// 直付一笔（未配付款流程的降级路径 = 生产里「显式跳过审批」的直付）。
async fn pay(payable_id: Uuid, amount: &str) {
    payable_payment::pay_payable(
        payable_id,
        &PayPayableReq {
            amount: Some(dec(amount)),
            pay_date: None,
            pay_method: Some("银行转账".to_string()),
            remark: Some("LNK03 真库用例".to_string()),
            ..Default::default()
        },
        Some(payer()),
    )
    .await
    .unwrap_or_else(|error| panic!("付款 {amount} 失败：{} {}", error.code, error.msg));
}

/// 引擎上下文（反证用：不传排除清单时规则到底读不读得到）。
fn ctx_of<'a>(payable_id: Uuid, payable_no: &'a str, status: &'a str) -> linkage::LinkageContext<'a> {
    linkage::LinkageContext::new(
        state_machine::ENTITY_PAYABLE,
        status,
        payable_id,
        payable_no,
        Some(payer()),
    )
    .with_people(Some(payer()), Some(payer()), None)
}

// ---------- ① 全额付款：OPEN → SETTLED 触发一次，且余额只扣一次 ----------
#[tokio::test]
async fn full_payment_settles_and_triggers_notice_once() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    ensure_payer().await;
    // SEND_NOTICE 是 AfterCommit 动作：必须由 commit 之后的 run_deferred_after_commit 执行
    add_rule(
        "SETTLED",
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let id = new_payable(dec("100"), dec("0"), "OPEN").await;
    assign_creator(id).await;
    pay(id, "100").await;

    let row = row_of(id).await;
    assert_eq!(row.status, "SETTLED", "全额付款必须结清");
    assert_eq!(row.paid_amount, dec("100"), "已付金额 = 100（不是 200）");
    assert_eq!(row.unpaid_amount, dec("0"), "未付金额 = 0（不是 -100）");

    let key = format!("LNK:PAYABLE:SETTLED:{id}");
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "PAYABLE/SETTLED 上配的 SEND_NOTICE 必须真的发出一条通知（幂等键 {key}）—— \
         只改 spec.rs 的 wired 标记、付款路径不调引擎时这里会是 0"
    );
    let rows = db()
        .find_with_params(
            "SELECT title, biz_type, biz_id, link_url, sender_name FROM sys_notice \
              WHERE idempotency_key = ?",
            vec![Value::Text(key.clone())],
        )
        .await
        .expect("查通知行");
    let notice = rows.first().expect("通知行");
    assert_eq!(
        notice.get_typed::<String>("title").expect("title"),
        "应付单已结清",
        "标题来自触发点基稿（目标态中文名），不是配置凭空生成"
    );
    assert_eq!(notice.get_typed::<String>("biz_type").expect("biz_type"), "PAYABLE");
    assert_eq!(notice.get_typed::<String>("biz_id").expect("biz_id"), id.to_string());
    assert!(
        notice
            .get_typed::<String>("link_url")
            .expect("link_url")
            .contains(&id.to_string()),
        "通知必须链回应付单详情"
    );

    // 付款凭证仍然恰好一张（排除清单生效的直接证据）
    let payment_id = last_payment_id(id).await;
    assert_eq!(
        payment_vouchers(payment_id).await,
        1,
        "一笔付款只能有一张付款凭证（联动不许再生成一张）"
    );

    teardown().await;
}

// ---------- ② 部分付款：OPEN → PARTIAL_PAID 触发 ----------
#[tokio::test]
async fn partial_payment_triggers_notice_for_partial_paid() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    ensure_payer().await;
    add_rule(
        "PARTIAL_PAID",
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let id = new_payable(dec("100"), dec("0"), "OPEN").await;
    assign_creator(id).await;
    pay(id, "30").await;

    let row = row_of(id).await;
    assert_eq!(row.status, "PARTIAL_PAID", "付 30/100 必须是部分已付");
    assert_eq!(row.paid_amount, dec("30"));
    assert_eq!(row.unpaid_amount, dec("70"));
    assert_eq!(
        notices_of_key(&format!("LNK:PAYABLE:PARTIAL_PAID:{id}")).await,
        1,
        "部分付款这条边必须触发联动（本卡接线前它永远不响）"
    );

    teardown().await;
}

// ---------- ③ 第二次部分付款（原地流转）不重复触发 ----------
#[tokio::test]
async fn second_partial_payment_does_not_trigger_again() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    ensure_payer().await;
    // 用 CREATE_TASK 做探针（没有幂等键兜底）：重复触发一次就会多一个任务行，
    // 比「通知有幂等键」更能证明「引擎真的只跑了一次」。
    add_rule(
        "PARTIAL_PAID",
        spec::ACTION_CREATE_TASK,
        r#"{"title":"LNK03 付款复核","assignee_strategy":"OPERATOR"}"#,
    )
    .await;

    let id = new_payable(dec("100"), dec("0"), "OPEN").await;
    assign_creator(id).await;
    pay(id, "30").await;
    assert_eq!(probe_tasks().await, 1, "正常触发一次：建 1 个任务");

    // 第二期仍是 PARTIAL_PAID（30 + 30 = 60 < 100）：状态没变 → 不许再触发一次联动
    pay(id, "30").await;
    let row = row_of(id).await;
    assert_eq!(row.status, "PARTIAL_PAID", "前提：60/100 仍是部分已付（状态没变）");
    assert_eq!(row.paid_amount, dec("60"), "两期累计 60（每期只扣一次）");
    assert_eq!(row.unpaid_amount, dec("40"));
    assert_eq!(
        probe_tasks().await,
        1,
        "状态没变不许再触发一次联动（任务数从 1 变 2 = 引擎在没变的状态上被重复调用）"
    );

    // 第三期结清：状态真的变了 → 探针才是 1 条规则在 PARTIAL_PAID 上，SETTLED 不触发
    pay(id, "40").await;
    assert_eq!(row_of(id).await.status, "SETTLED");
    assert_eq!(probe_tasks().await, 1, "SETTLED 上没有规则，任务数不变");

    teardown().await;
}

// ---------- ④ 排除 UPDATE_PAYABLE_BALANCE：余额不被联动重复改 ----------
#[tokio::test]
async fn excluded_payable_balance_action_does_not_double_deduct() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    ensure_payer().await;
    // 故意配一条「再减 100」的余额规则：排除清单要是丢了，这 100 会被扣第二次
    // （已付变 200 / 未付变 -100 → 库侧 ck_biz_payable_unpaid 直接把整笔付款炸成 500）。
    add_rule(
        "SETTLED",
        "UPDATE_PAYABLE_BALANCE",
        r#"{"direction":"DECREASE","amount":"100"}"#,
    )
    .await;

    let id = new_payable(dec("100"), dec("0"), "OPEN").await;
    assign_creator(id).await;
    pay(id, "100").await;

    let row = row_of(id).await;
    assert_eq!(row.status, "SETTLED", "排除清单生效时付款必须成功结清");
    assert_eq!(row.paid_amount, dec("100"), "已付 = 100：钱只扣了一次");
    assert_eq!(row.unpaid_amount, dec("0"), "未付 = 0：没有被联动扣成 -100");

    // 反证：规则确实被引擎读到了（不是「没读到」造成的假绿）
    let row_lite = payable_payment::lock_payable_in_tx(db(), id)
        .await
        .expect("读应付单")
        .expect("应付单存在");
    let ctx = ctx_of(id, &row_lite.payable_no, "SETTLED");
    let excluded = linkage::engine::run_excluding(db(), &ctx, spec::PAYABLE_EXCLUDED_ACTIONS)
        .await
        .expect("排除清单下引擎必须正常返回");
    assert!(
        excluded.runs.is_empty() && excluded.deferred.is_empty(),
        "排除清单里的动作不许产生任何执行记录，实际：{:?}",
        excluded
            .runs
            .iter()
            .map(|item| (&item.action_code, item.outcome.label()))
            .collect::<Vec<_>>()
    );
    // 不传排除清单 → 引擎会真的去跑它（该动作尚未接管 ⇒ Skipped，规则确实被读到）
    let unfiltered = linkage::engine::run_excluding(db(), &ctx, &[])
        .await
        .expect("不带排除清单时引擎仍应返回（占位动作只跳过）");
    assert!(
        unfiltered
            .runs
            .iter()
            .any(|item| item.action_code == "UPDATE_PAYABLE_BALANCE"),
        "规则必须能被引擎读到并尝试执行，否则上面的「没重复扣」是假绿"
    );

    teardown().await;
}

// ---------- ⑤ 排除 GENERATE_VOUCHER：一笔付款只有一张凭证 ----------
#[tokio::test]
async fn excluded_generate_voucher_does_not_create_second_payment_voucher() {
    let Some(_guard) = setup().await else {
        return;
    };
    teardown().await;
    ensure_payer().await;
    // TAKEOVER：真落库（SHADOW 只试算）。排除清单要是丢了，这里会多一张付款凭证。
    add_rule(
        "SETTLED",
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"PAYMENT","mode":"TAKEOVER"}"#,
    )
    .await;

    let id = new_payable(dec("100"), dec("0"), "OPEN").await;
    assign_creator(id).await;
    pay(id, "100").await;

    let row = row_of(id).await;
    assert_eq!(row.status, "SETTLED");
    assert_eq!(row.paid_amount, dec("100"));
    let payment_id = last_payment_id(id).await;
    assert_eq!(
        payment_vouchers(payment_id).await,
        1,
        "一笔付款只能有一张付款凭证：凭证动作必须由付款事务专用路径（第 ② 步）独占"
    );

    // 反证：同一份规则、不传排除清单 → 引擎真的会去跑它（要么落一张第二张凭证，
    // 要么作为关键动作失败返回 Err）—— 两种结果都证明「规则读到了、被通用引擎尝试执行过」。
    let row_lite = payable_payment::lock_payable_in_tx(db(), id)
        .await
        .expect("读应付单")
        .expect("应付单存在");
    let ctx = ctx_of(id, &row_lite.payable_no, "SETTLED");
    match linkage::engine::run_excluding(db(), &ctx, &[]).await {
        Ok(run) => assert!(
            run.runs
                .iter()
                .any(|item| item.action_code == spec::ACTION_GENERATE_VOUCHER),
            "不传排除清单时规则必须被执行到，实际：{:?}",
            run.runs
                .iter()
                .map(|item| (&item.action_code, item.outcome.label()))
                .collect::<Vec<_>>()
        ),
        Err(error) => assert!(
            error.msg.contains(spec::ACTION_GENERATE_VOUCHER) || error.msg.contains("凭证"),
            "关键动作失败必须点名动作 / 原因，实际：{}",
            error.msg
        ),
    }

    teardown().await;
}

// ============================================================================
// transfer_handler_db_linkage_tests.rs —— 真库：调拨状态变更的跨单据联动（LNK-01）
//
// 为什么必须真库：本卡交付的是「配一条规则，状态一变它真的执行」——
// 纯逻辑单测只能证明「源码里出现了调用」，证明不了
// 「规则读得到 → 事务内/提交后真的跑了 → 通知落到 sys_notice / 任务落到 biz_task
//  → 重复推进时不再跑第二次」。所以三条用例都走**生产同款路径**：
//
// ```text
// 夹具：仓 / 物料 / 在途仓（V0139 种子）+ 一行 biz_flow_linkage_rule
//   → transfer_handler::create_transfer / change_transfer_status（人工流转）
//   → transfer_handler_test_support::approve_flow_instance（审批回调那条路）
// ```
//
// 用法：
// ```bash
// export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/<测试库>'
// cargo test --lib business::erp::transfer_handler::db_linkage -- --nocapture --test-threads=1
// ```
// 缺 `ERP_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。
//
// # 夹具卫生
//
// 规则按 `name LIKE 'LNK01-%'` 定位，每条用例**进/出都清**：规则是按触发点全局生效的
// （没有「按单据」粒度），残留一行就会让后续所有跑到 `STOCK_TRANSFER/DONE` 的用例
// 多发一条通知 / 多建一个任务。通知按幂等键前缀 `LNK:STOCK_TRANSFER:%` 清
// （`sys_notice_receiver` 是 `ON DELETE CASCADE`）。
// ============================================================================

use super::db_tests_fixtures::*;
use super::*;
use crate::business::erp::transfer_linkage;
use crate::business::erp::transfer_model::{self, STATUS_APPROVED, STATUS_DONE, STATUS_PENDING};
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use nik::prelude::chrono::Utc;
use nik::prelude::*;

/// 夹具规则的命名前缀（本卡专用，与其它卡 / 生产的规则互不影响）。
const RULE_PREFIX: &str = "LNK01-";

/// 清本卡的联动规则。
async fn purge_rules() {
    exec(
        &format!("DELETE FROM biz_flow_linkage_rule WHERE name LIKE '{RULE_PREFIX}%'"),
        vec![],
        "清理 LNK-01 联动规则夹具",
    )
    .await;
}

/// 清本卡的联动通知（按幂等键前缀，接收人行级联删）。
async fn purge_notices() {
    exec(
        "DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:STOCK_TRANSFER:%'",
        vec![],
        "清理 LNK-01 联动通知",
    )
    .await;
}

/// 清本卡的联动任务（`CREATE_TASK` 动作的落点）。
async fn purge_tasks() {
    exec(
        "DELETE FROM biz_task WHERE title LIKE 'LNK01 %'",
        vec![],
        "清理 LNK-01 联动任务",
    )
    .await;
}

/// 插一条 `STOCK_TRANSFER/<status>` 上的规则。
async fn add_rule(status: &str, action: &str, params: &str) {
    exec(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), ?, 'STOCK_TRANSFER', ?, ?, ?::jsonb, 10) \
         ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE \
            SET name = EXCLUDED.name, action_params = EXCLUDED.action_params, \
                enabled = TRUE, deleted_at = NULL",
        vec![
            Value::Text(format!("{RULE_PREFIX}{status}-{action}")),
            Value::Text(status.to_string()),
            Value::Text(action.to_string()),
            // jsonb 参数必须用 `Value::Json`（与 `linkage::model::upsert_rule` 同口径：
            // `Value::Text` 绑 jsonb 会落到「error serializing parameter」）
            Value::Json(params.to_string()),
        ],
        "插入联动规则夹具",
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

/// 夹具收尾（规则 / 通知 / 任务一把清）。
async fn teardown() {
    purge_rules().await;
    purge_notices().await;
    purge_tasks().await;
}

// ---------- ① 正常触发：状态变更 → 规则真的执行（提交后动作在 commit 之后跑） ----------
#[tokio::test]
async fn done_status_triggers_linkage_notice_once() {
    let Some(_guard) = prepare().await else {
        return;
    };
    teardown().await;
    // SEND_NOTICE 是 AfterCommit 动作：它必须由 `run_deferred_after_commit` 在提交后执行。
    // 收件人策略取 CREATOR（调拨单的制单人 = U_APPLICANT），标题/正文沿用触发点基稿。
    add_rule(
        STATUS_DONE,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(id).await;
    assert_eq!(refresh(id).await.status, STATUS_DONE, "前置：单据必须真的收货完成");

    let key = format!("LNK:STOCK_TRANSFER:{STATUS_DONE}:{id}");
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "STOCK_TRANSFER/DONE 上配的 SEND_NOTICE 必须真的发出一条通知（幂等键 {key}）——\
         只改 spec.rs 的 wired 标记、handler 里不调引擎时这里会是 0"
    );
    // 通知内容来自触发点基稿（不是配置凭空生成）：标题带目标态中文名
    let rows = query(
        "SELECT title, target_type, biz_type, link_url FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(key.clone())],
    )
    .await;
    let row = rows.first().expect("通知行");
    assert_eq!(row.get_typed::<String>("title").expect("title"), "调拨单已完成");
    assert_eq!(row.get_typed::<String>("biz_type").expect("biz_type"), "STOCK_TRANSFER");
    assert_eq!(row.get_typed::<String>("target_type").expect("target_type"), "USER");
    assert!(
        row.get_typed::<String>("link_url")
            .expect("link_url")
            .contains(&id.to_string()),
        "通知必须链回单据详情"
    );

    teardown().await;
}

// ---------- ② 重复过账不重复触发 ----------
#[tokio::test]
async fn repeated_settle_does_not_trigger_linkage_twice() {
    let Some(_guard) = prepare().await else {
        return;
    };
    teardown().await;
    // 用 CREATE_TASK 做探针（没有幂等键兜底）：重复触发一次就会多一个任务行，
    // 比「通知有幂等键」更能证明「引擎真的只跑了一次」。
    add_rule(
        STATUS_DONE,
        spec::ACTION_CREATE_TASK,
        r#"{"title":"LNK01 调拨收货复核","assignee_strategy":"OPERATOR"}"#,
    )
    .await;

    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(id).await;
    let tasks = |label: &'static str| async move {
        count_of(
            "SELECT count(*) FROM biz_task WHERE title = 'LNK01 调拨收货复核'",
            vec![],
            label,
        )
        .await
    };
    assert_eq!(tasks("首次收货后建任务").await, 1, "正常触发一次：建 1 个任务");

    // ②a 再推一次 DONE：非法边 → 409，状态没变就不该再触发
    let applicant = uuid(U_APPLICANT);
    let error = change_transfer_status(id, &status_req(STATUS_DONE, None), applicant)
        .await
        .expect_err("DONE 之后重推必须 409");
    assert_eq!(error.status, 409, "实际：{}", error.msg);

    // ②b 绕过接口直接重放结算（运维修复脚本就是这么干的）：幂等闸跳过，联动也不该跑
    let row = refresh(id).await;
    let outcome = transfer_model::settle_transfer(
        db(),
        row.id,
        &row.transfer_no,
        row.from_warehouse_id,
        row.to_warehouse_id,
        Some(applicant),
        Utc::now(),
    )
    .await
    .expect("重复结算必须安全返回（幂等）而不是报错");
    assert!(!outcome.settled, "第二次结算必须是幂等跳过");

    assert_eq!(
        tasks("重复结算后任务数").await,
        1,
        "重复 settle 不许再触发一次联动（任务数从 1 变 2 = 引擎在 settle 里被重复调用）"
    );
    assert_eq!(txns_of(id).await.len(), 2, "库存流水仍然只有 2 条（没被二次落账）");

    teardown().await;
}

// ---------- ③ 排除动作确实不跑（专用路由闸） ----------
#[tokio::test]
async fn excluded_actions_are_not_executed_by_the_generic_engine() {
    let Some(_guard) = prepare().await else {
        return;
    };
    teardown().await;
    // GENERATE_VOUCHER 是「已实现」的关键动作：如果排除清单丢了，引擎会真的去生成凭证。
    add_rule(
        STATUS_DONE,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_TRANSFER","mode":"TAKEOVER"}"#,
    )
    .await;

    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(id).await;
    let row = refresh(id).await;
    let ctx = transfer_linkage::status_ctx(
        row.id,
        &row.transfer_no,
        STATUS_DONE,
        Some(uuid(U_APPLICANT)),
        row.create_by,
    );

    // ③a 走真实排除清单：命中的规则**连执行记录都不产生**（引擎里是 `continue`，
    //     不是「跑了一次然后跳过」——两者在排障日志里完全不同）
    let excluded = linkage::engine::run_excluding(db(), &ctx, spec::TRANSFER_EXCLUDED_ACTIONS)
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

    // ③b 反证：同一份规则、不传排除清单 → 引擎会去看它（这一步证明 ③a 的「空」不是
    //     「规则没读到」导致的假绿）
    let unfiltered = linkage::engine::run_excluding(db(), &ctx, &[])
        .await
        .expect("不传排除清单时引擎必须正常返回");
    assert_eq!(
        unfiltered.runs.len(),
        1,
        "不带排除清单时那条规则必须被引擎读到（否则 ③a 的断言是假绿）"
    );
    assert_eq!(unfiltered.runs[0].action_code, spec::ACTION_GENERATE_VOUCHER);
    assert!(
        !unfiltered.runs[0].outcome.is_success(),
        "调拨不该生成凭证（该动作只接管 STOCK_IN 取数），实际：{}",
        unfiltered.runs[0].outcome.detail()
    );

    // ③c 落账面：调拨收货只写两条流水、不抛凭证
    assert_eq!(txns_of(id).await.len(), 2, "调拨结算只写 2 条流水，不许被引擎二次落账");
    assert_eq!(
        count_of(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
            vec![Value::from(&id)],
            "查调拨产生的凭证",
        )
        .await,
        0,
        "调拨不生成凭证：GENERATE_VOUCHER 被排除后库里不该多出任何凭证"
    );

    teardown().await;
}

// ---------- ④ 审批回调那条路也真的触发（配了已发布流程的生产环境） ----------
#[tokio::test]
async fn approval_callback_triggers_linkage_on_approved() {
    let Some(_guard) = prepare().await else {
        return;
    };
    teardown().await;
    ensure_seed_flow().await;
    add_rule(
        STATUS_APPROVED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let applicant = uuid(U_APPLICANT);
    let id = create_transfer(&body(vec![item(M_PLAIN, "5")]), Some(applicant), None)
        .await
        .expect("建单");
    change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .expect("提交审批");
    let instance_id = refresh(id).await.approval_instance_id.expect("实例 id");
    // 生产同款路径：引擎投票落终态 → 回调调拨单（`on_transfer_settled`）
    approve_instance(instance_id).await;
    assert_eq!(
        refresh(id).await.status,
        STATUS_APPROVED,
        "前置：审批通过后调拨单必须 APPROVED"
    );

    let key = format!("LNK:STOCK_TRANSFER:{STATUS_APPROVED}:{id}");
    assert_eq!(
        notices_of_key(&key).await,
        1,
        "审批回调这条状态变更路也必须触发联动（幂等键 {key}）——\
         只接人工流转时，配了已发布流程的环境里这条规则永远不响"
    );

    teardown().await;
}

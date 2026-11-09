//! 会计期间联动接线（`ACCOUNTING_PERIOD`，LNK-08）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::period_linkage_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面「结账触发 / 反结账触发 / 事务回滚时通知不发」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk08_close_triggers_closed_notice` | 结账（OPEN → CLOSED）真的触发通知 |
//! | 2 | `lnk08_reopen_triggers_open_notice` | 反结账（CLOSED → OPEN）真的触发通知（`PERIOD/OPEN` 不是只读枚举） |
//! | 3 | `lnk08_rolled_back_tx_never_sends_notice` | **事务回滚 → 通知不发**（deferred 动作随事务一起丢），并有「提交后才发」的正向对照 |
//! | 4 | `lnk08_commit_failure_keeps_period_open_and_silent` | **真实结账事务在 commit 处失败** → 期间仍 OPEN + 0 条通知；去掉注入后通知照发 |
//! | 5 | `lnk08_repeat_close_does_not_trigger_twice` | 重复结账被 409 挡住，通知不增（重复过账不重复触发） |
//! | 6 | `lnk08_excluded_ledger_actions_never_run` | 期间上配 `GENERATE_VOUCHER`：通用引擎**不执行**它（同状态的 `SEND_NOTICE` 照跑） |
//!
//! # 夹具口径
//!
//! * 期间用 `2033-09..12`（`period_test_support` 拥有的号段，与催办 / 盘点共用一把锁
//!   [`support::DB_LOCK`]，谁都不会在别人跑到一半时动手）；**绝不碰 2026-xx**；
//! * 操作人 / 收件人用 `support::seed_user("p2.lnk08…")`（`p2.` 是夹具号段，随
//!   `support::cleanup()` 一起清）；
//! * 联动规则按 `LNK08%`、通知按幂等键 `LNK:PERIOD:%` 清（它们没有单据主键可依）。
//!
//! **串行**（`--test-threads=1`）：联动规则是全表唯一索引、期间行按前缀清，并行会互删。

use nik::prelude::*;
use nik::uuid::Uuid;

use super::{period, period_linkage};
use crate::business::erp::period_test_support as support;
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::business::shared::state_machine;

/// 结账用例的期间。
const PERIOD_CLOSE: &str = "2033-10";
/// 反结账用例的期间。
const PERIOD_REOPEN: &str = "2033-11";
/// 「回滚 / commit 失败」用例的期间。
const PERIOD_ROLLBACK: &str = "2033-12";
/// 「排除动作」用例的期间。
const PERIOD_EXCLUDED: &str = "2033-09";

fn uid(text: &str) -> Uuid {
    Uuid::parse_str(text).expect("夹具 UUID 非法")
}

/// 执行一条写 SQL（失败直接 panic，不静默吞错）。
async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 清联动夹具（规则 + 本卡发过的通知）。
///
/// 通知的接收人行随 `sys_notice` CASCADE（V0043 `ON DELETE CASCADE`），不用单独删。
async fn clear_rules() {
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK08%'").await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:PERIOD:%'").await;
}

/// 清场：先清联动夹具，再走 `period_test_support::cleanup()`（2033/2040 期次 + `p2.` 用户）。
///
/// 顺带兜住「上一个 run 在 commit-失败用例中途 panic」的残留：注入用的延迟约束触发器
/// 是**表级**的，留着会把后续所有结账打成 500。
async fn purge() {
    clear_rules().await;
    exec("DROP TRIGGER IF EXISTS lnk08_fail_close_trg ON biz_accounting_period").await;
    exec("DROP FUNCTION IF EXISTS lnk08_fail_close()").await;
    support::cleanup().await;
}

/// 拿锁 + 连库 + 清场 + 建夹具用户（收件人必须在 `sys_user` 里存活，见
/// `linkage::actions::ensure_recipients_alive`）。
async fn setup() -> Option<(tokio::sync::MutexGuard<'static, ()>, Uuid)> {
    let guard = support::setup().await?;
    purge().await;
    let user = support::seed_user("p2.lnk08_maker", "LNK08 财务").await;
    Some((guard, user))
}

/// 结账 / 反结账的操作人（`closed_by` 是弱引用；通知收件人用它）。
fn operator_of(user: Uuid) -> state_machine::Operator {
    state_machine::Operator::named(Some(user), "lnk08-tester")
}

/// 开一个测试期间并读回行（走真实 `open_period`，不手工插行）。
async fn open_period(code: &str, user: Uuid) -> super::model::PeriodRow {
    let (year, month) = period::parse_year_month(code).expect("期间编码");
    let res = period::open_period(year, month, Some("LNK08 夹具"), operator_of(user))
        .await
        .unwrap_or_else(|err| panic!("开账失败（{code}）：{} {}", err.code, err.msg));
    let id = uid(&res.id);
    period::load_period_row(db(), id)
        .await
        .expect("读期间行")
        .expect("期间行应存在")
}

/// 期间当前状态。
async fn status_of(period_id: Uuid) -> Option<String> {
    db().scalar_with_params::<Option<String>>(
        "SELECT status FROM biz_accounting_period WHERE id = ?::uuid",
        vec![Value::from(&period_id)],
    )
    .await
    .expect("读期间状态")
}

/// 插一条 `ACCOUNTING_PERIOD/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_rule(status: &str, action: &str, params: &str) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), 'LNK08-{status}-{action}-{nanos}', 'ACCOUNTING_PERIOD', \
                 '{status}', '{action}', '{params}'::jsonb, 10)"
    ))
    .await;
}

/// 按幂等键数通知条数（期间联动的通知唯一键 = `LNK:PERIOD:<状态>:<期间 id>`）。
async fn notices_of(status: &str, period_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(format!("LNK:PERIOD:{status}:{period_id}"))],
    )
    .await
    .expect("查联动通知")
}

/// 某期间 id 作为来源单据的凭证条数（防 `GENERATE_VOUCHER` 在期间上落账）。
async fn vouchers_of(period_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&period_id)],
    )
    .await
    .expect("查期间凭证")
}

/// 结账的规则参数：TAKEOVER + 收件人取操作人（期间没有制单人列，见 `period_linkage` 文件头）。
const NOTICE_OPERATOR: &str = r#"{"mode":"TAKEOVER","recipient":"OPERATOR"}"#;

// ==================== ① 结账触发 ====================

/// 结账（`OPEN → CLOSED`）必须真的触发联动：不接这一条，`ACCOUNTING_PERIOD/CLOSED`
/// 上配的「月结完成通知财务」能保存、能启用、永远不触发（P1-FIX-5C 原病）。
#[tokio::test]
async fn lnk08_close_triggers_closed_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let row = open_period(PERIOD_CLOSE, user).await;
    assert_eq!(row.status, period::STATUS_OPEN);
    add_rule(period::STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_OPERATOR).await;

    let res = period::close_period(row.id, Some("LNK-08 用例结账"), operator_of(user))
        .await
        .unwrap_or_else(|err| panic!("结账失败：{} {}", err.code, err.msg));

    assert_eq!(res.status, period::STATUS_CLOSED, "结账成功应返回 CLOSED");
    assert_eq!(
        status_of(row.id).await.as_deref(),
        Some(period::STATUS_CLOSED),
        "期间状态必须真的落库为 CLOSED"
    );
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        1,
        "ACCOUNTING_PERIOD/CLOSED 上配的 SEND_NOTICE 必须真的发出一条通知"
    );
    purge().await;
}

// ==================== ② 反结账触发 ====================

/// 反结账（`CLOSED → OPEN`）同样触发：`ACCOUNTING_PERIOD/OPEN` 上配的
/// 「反结账要上报财务负责人」不能永远不响（否则 OPEN 这个状态在配置页能选、选了不生效）。
#[tokio::test]
async fn lnk08_reopen_triggers_open_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let row = open_period(PERIOD_REOPEN, user).await;
    add_rule(period::STATUS_OPEN, spec::ACTION_SEND_NOTICE, NOTICE_OPERATOR).await;
    period::close_period(row.id, None, operator_of(user))
        .await
        .unwrap_or_else(|err| panic!("前置结账失败：{} {}", err.code, err.msg));
    // 结账是 OPEN → CLOSED，不该触发 OPEN 上的规则（状态判定必须精确）
    assert_eq!(
        notices_of(period::STATUS_OPEN, row.id).await,
        0,
        "结账不许触发 OPEN 状态的规则（触发器按**变更后**的状态匹配）"
    );

    let res = period::reopen_period(
        row.id,
        Some("LNK-08 用例反结账：本月有一笔跨期调账要补"),
        operator_of(user),
    )
    .await
    .unwrap_or_else(|err| panic!("反结账失败：{} {}", err.code, err.msg));

    assert_eq!(res.status, period::STATUS_OPEN, "反结账应返回 OPEN");
    assert_eq!(
        status_of(row.id).await.as_deref(),
        Some(period::STATUS_OPEN),
        "期间状态必须真的退回 OPEN"
    );
    assert_eq!(
        notices_of(period::STATUS_OPEN, row.id).await,
        1,
        "ACCOUNTING_PERIOD/OPEN 上配的 SEND_NOTICE 必须真的发出一条通知"
    );
    purge().await;
}

// ==================== ③ 事务回滚：通知不发 ====================

/// **卡面的硬要求**：通知走 `commit 后 run_deferred`，所以事务回滚时一条都不许发。
///
/// 两层证据：
///
/// 1. 事务内调 hook：`SEND_NOTICE` 只进 `run.deferred`（事务内不执行），此时库里 0 条；
/// 2. 事务回滚后再查：仍然 0 条 —— 通知没有「先发后回滚」的窗口；
/// 3. 正向对照：同一套动作改成 commit + `run_deferred_after_commit`，通知立刻 1 条。
///    （否则上面的 0 可能是「规则没命中」的虚绿）
#[tokio::test]
async fn lnk08_rolled_back_tx_never_sends_notice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let row = open_period(PERIOD_ROLLBACK, user).await;
    add_rule(period::STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_OPERATOR).await;

    // ---- ① 事务内：通知只在 deferred 里 ----
    let tx = db().begin().await.expect("开事务");
    let change = period_linkage::run_in_tx(&tx, row.id, &row.period_code, period::STATUS_CLOSED, Some(user))
        .await
        .expect("事务内跑联动");
    assert_eq!(
        change.run.deferred.len(),
        1,
        "SEND_NOTICE 必须是 AfterCommit 动作：事务内只排队，不许直接发"
    );
    assert_eq!(
        change.run.runs.len(),
        0,
        "事务内不该有 InTx 动作被执行（期间上只配了通知）"
    );
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        0,
        "事务还没提交，通知一条都不许发（否则回滚就是幽灵通知）"
    );

    // ---- ② 回滚：通知仍然 0 ----
    tx.rollback().await.expect("回滚事务");
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        0,
        "事务回滚后通知必须仍然为 0（「通知已发但结账失败」是卡面点名的事故）"
    );

    // ---- ③ 正向对照：commit 之后 flush 才会发 ----
    let tx = db().begin().await.expect("开事务");
    let change = period_linkage::run_in_tx(&tx, row.id, &row.period_code, period::STATUS_CLOSED, Some(user))
        .await
        .expect("事务内跑联动");
    tx.commit().await.expect("提交事务");
    period_linkage::run_deferred_after_commit(change).await;
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        1,
        "对照：commit 之后 run_deferred 必须真的发出通知（否则上面的 0 是虚绿）"
    );
    purge().await;
}

// ==================== ④ 真实结账事务在 commit 处失败 ====================

/// 用**延迟约束触发器**把失败注入到 commit 那一刻：`close_period` 里的事务内动作
/// （含联动 `run_in_tx`）全部跑完了，`tx.commit()` 才炸 —— 这正是卡面担心的场景
/// 「事务内已经把通知发出去、结果结账失败」。
///
/// 断言：
/// * `close_period` 返回 Err（commit 失败）；
/// * 期间仍是 `OPEN`（整笔回滚，状态没落）；
/// * 通知 **0 条**（deferred 动作随事务一起丢）；
/// * 删掉注入后重新结账：成功且通知 1 条（证明「0 条」不是因为规则配错）。
#[tokio::test]
async fn lnk08_commit_failure_keeps_period_open_and_silent() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let row = open_period(PERIOD_ROLLBACK, user).await;
    add_rule(period::STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_OPERATOR).await;

    // 注入：该期间被 UPDATE 成 CLOSED 时，在 **commit 那一刻** 抛错
    exec(
        "CREATE OR REPLACE FUNCTION lnk08_fail_close() RETURNS trigger AS $$ \
         BEGIN \
           IF NEW.period_code = '2033-12' AND NEW.status = 'CLOSED' THEN \
             RAISE EXCEPTION 'LNK08 注入的提交期失败'; \
           END IF; \
           RETURN NEW; \
         END $$ LANGUAGE plpgsql",
    )
    .await;
    exec(
        "CREATE CONSTRAINT TRIGGER lnk08_fail_close_trg AFTER UPDATE ON biz_accounting_period \
         DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION lnk08_fail_close()",
    )
    .await;

    let failed = period::close_period(row.id, None, operator_of(user)).await;
    assert!(
        failed.is_err(),
        "注入 commit 期失败后 close_period 必须返回 Err（否则夹具没生效，下面的断言是虚绿）"
    );
    assert_eq!(
        status_of(row.id).await.as_deref(),
        Some(period::STATUS_OPEN),
        "commit 失败 → 整笔回滚：期间必须仍是 OPEN"
    );
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        0,
        "commit 失败 → 通知一条都不许发（事务内发通知就会在这里留下幽灵通知）"
    );

    // 拆掉注入：同样的结账必须成功，且通知照发（证明上面的 0 条是回滚造成的）
    exec("DROP TRIGGER lnk08_fail_close_trg ON biz_accounting_period").await;
    exec("DROP FUNCTION lnk08_fail_close()").await;

    period::close_period(row.id, None, operator_of(user))
        .await
        .unwrap_or_else(|err| panic!("拆掉注入后结账应当成功：{} {}", err.code, err.msg));
    assert_eq!(
        status_of(row.id).await.as_deref(),
        Some(period::STATUS_CLOSED),
        "拆掉注入后期间应正常结账"
    );
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        1,
        "对照：注入拆掉后通知必须发出来（否则「0 条」是规则没配上的虚绿）"
    );
    purge().await;
}

// ==================== ⑤ 重复结账不重复触发 ====================

/// 结账是 CAS + 状态机闸的单向操作：第二次点「结账」被 409 挡住，
/// 联动**不许**跑第二遍（同一期间同一状态只发一条通知）。
#[tokio::test]
async fn lnk08_repeat_close_does_not_trigger_twice() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let row = open_period(PERIOD_CLOSE, user).await;
    add_rule(period::STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_OPERATOR).await;

    period::close_period(row.id, None, operator_of(user))
        .await
        .unwrap_or_else(|err| panic!("第一次结账失败：{} {}", err.code, err.msg));
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        1,
        "第一次结账必须发一条通知"
    );

    let again = period::close_period(row.id, None, operator_of(user)).await;
    assert!(
        again.is_err(),
        "重复结账必须被挡住（状态已是 CLOSED → 409），否则联动会跑第二遍"
    );
    assert_eq!(
        status_of(row.id).await.as_deref(),
        Some(period::STATUS_CLOSED),
        "重复结账不许把状态改坏"
    );
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        1,
        "被挡住的重复结账**不许**再发一条通知"
    );
    purge().await;
}

// ==================== ⑥ 排除动作确实不跑 ====================

/// 期间上配 `GENERATE_VOUCHER`：通用引擎**不执行**它。
///
/// 两层证据：
///
/// 1. **引擎级**（同一份真实规则行、同一个上下文）：`run_excluding(PERIOD_EXCLUDED_ACTIONS)`
///    里那条规则**不产生任何 RuleRun**（`is_routed_elsewhere` → `continue`），
///    而不带排除清单的 `run` 会产生一条 `SKIPPED`（「只接管收货入库」）——
///    说明这条规则**确实被读到了**，只是被排除清单拦下（不是「规则没生效」这种虚绿）；
/// 2. **端到端**：结账后期间名下 0 张凭证，同状态配的 `SEND_NOTICE` 照常发出
///    （排除清单只排凭证 / 库存 / 余额，不许把通知一起排掉）。
#[tokio::test]
async fn lnk08_excluded_ledger_actions_never_run() {
    let Some((_guard, user)) = setup().await else {
        return;
    };
    let row = open_period(PERIOD_EXCLUDED, user).await;
    add_rule(period::STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_OPERATOR).await;
    add_rule(
        period::STATUS_CLOSED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_IN","mode":"TAKEOVER"}"#,
    )
    .await;

    // ---- 第一层：引擎级对照（真实规则行 + 真实上下文）----
    let ctx = linkage::LinkageContext::new(
        state_machine::ENTITY_PERIOD,
        period::STATUS_CLOSED,
        row.id,
        &row.period_code,
        Some(user),
    );
    let tx = db().begin().await.expect("开事务");
    let without_exclusion = linkage::engine::run(&tx, &ctx).await.expect("跑引擎");
    let with_exclusion = linkage::engine::run_excluding(&tx, &ctx, spec::PERIOD_EXCLUDED_ACTIONS)
        .await
        .expect("跑引擎（带排除清单）");
    tx.rollback().await.expect("回滚");
    assert!(
        without_exclusion
            .runs
            .iter()
            .any(|item| item.action_code == spec::ACTION_GENERATE_VOUCHER),
        "对照：不带排除清单时这条 GENERATE_VOUCHER 规则必须被读到并执行（本例是 SKIPPED，\
         因为该动作只接管收货入库）—— 否则下面的断言就是虚绿（规则根本没命中）"
    );
    assert!(
        with_exclusion
            .runs
            .iter()
            .all(|item| item.action_code != spec::ACTION_GENERATE_VOUCHER),
        "带排除清单时 GENERATE_VOUCHER 必须**整条不跑**（专用路由，不产生 RuleRun）：{:?}",
        with_exclusion
            .runs
            .iter()
            .map(|item| item.action_code.as_str())
            .collect::<Vec<_>>()
    );

    // ---- 第二层：端到端（走真实结账）----
    period::close_period(row.id, None, operator_of(user))
        .await
        .unwrap_or_else(|err| panic!("结账失败：{} {}", err.code, err.msg));
    assert_eq!(
        vouchers_of(row.id).await,
        0,
        "期间不该产生联动凭证：GENERATE_VOUCHER 在排除清单里，通用引擎不许给它落账"
    );
    assert_eq!(
        notices_of(period::STATUS_CLOSED, row.id).await,
        1,
        "排除清单只排凭证 / 库存 / 余额：同状态的 SEND_NOTICE 必须照常发出\
         （排掉通知 = 接线了永远不响）"
    );
    purge().await;
}

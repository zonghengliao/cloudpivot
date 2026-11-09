//! 会计年度联动接线（`ACCOUNTING_YEAR`，LNK-11）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::year_end_linkage_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面「年结成功 + 联动触发一次 + 通知发出」「回滚场景通知不发」「排除动作不跑」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk11_close_triggers_one_notice_for_multi_voucher_year` | 含**多张凭证**的年度跑真年结：结转凭证 1 张、通知 1 条（带年度 / 凭证号 / 分录数） |
//! | 2 | `lnk11_repeat_close_is_blocked_and_does_not_notify` | 重复年结被 `ALREADY_CLOSED` 409 挡住，通知不增、凭证不增 |
//! | 3 | `lnk11_reopen_triggers_notice_once` | 反年结（`CLOSED → NOT_CLOSED`）触发通知一次；重复反年结被 409 挡住，通知不增 |
//! | 4 | `lnk11_rolled_back_tx_never_sends_notice` | **事务回滚 → 通知一条都不发**（deferred 随事务丢），并有「提交后才发」的正向对照 |
//! | 5 | `lnk11_excluded_voucher_actions_never_run` | 年度上配 `GENERATE_VOUCHER`：通用引擎**不执行**它（同状态 `SEND_NOTICE` 照跑），结转凭证只有年结自己那一张 |
//! | 6 | `lnk11_critical_action_failure_rolls_back_year_end` | **卡面核心**：事务内关键动作失败 → 整笔年结回滚（年度仍未结转 / 无结转凭证 / **通知 0 条**），随后撤掉失败规则重跑 → 成功 + 通知 1 条（证明上面的 0 不是「规则没命中」的虚绿） |
//!
//! # 夹具口径
//!
//! * 年度：`year_end_test_support` 的 **2037 / 2038**（该模块独占，`year_end.rs` 自带的
//!   2021–2023 用例与 `period` 的 2031–2033 互不重叠），24 个期间一次开；
//! * 凭证走真实 `voucher::post_voucher`（借 `1002` / 贷 `5001`，都是末级科目），
//!   月份 3 / 7 / 11 各一张 —— 「含多张凭证的年度」是卡面明确要求的夹具；
//! * 年结操作人是一个**真实 `sys_user`**（`p2.lnk11_maker`）：`SEND_NOTICE` 第 3 层会校验
//!   收件人在 `sys_user` 里存活，用假 uuid 的话通知会被静默 `Skipped`（假绿）；
//! * 串行（`--test-threads=1`）：联动规则 `biz_flow_linkage_rule` 是全表唯一索引，
//!   并行会互删；`year_end_test_support::DB_LOCK` 同时也把期间夹具的清理串起来。

use std::str::FromStr;

use nik::prelude::chrono::{NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::voucher::{self, EntryLine, PostVoucher};
use super::year_end::{self, STATUS_CLOSED, STATUS_NOT_CLOSED};
use super::year_end_linkage;
use super::year_end_test_support as support;
use crate::business::erp::period_test_support as p2;
use crate::business::flow::linkage;
use crate::business::flow::linkage::spec;
use crate::business::shared::state_machine;

/// 年结操作人（真实用户：通知的第 3 层校验要求收件人存在于 `sys_user`）。
struct Fixture {
    /// 操作人 id（`SEND_NOTICE` 的收件人）。
    user: Uuid,
    /// 串行锁（drop 即放锁）。
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    /// 操作人（`year_end::close_year` / `reopen_year` 的入参）。
    fn operator(&self) -> state_machine::Operator {
        state_machine::Operator::named(Some(self.user), "lnk11-tester")
    }
}

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

/// 执行一条写 SQL（失败直接 panic，不静默吞错）。
async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 取一个 i64 标量（失败直接 panic）。
async fn scalar_i64(text: &str) -> i64 {
    db().scalar_with_params::<i64>(text, vec![])
        .await
        .unwrap_or_else(|err| panic!("查询失败：{err}\nSQL: {text}"))
}

/// 本卡自己的夹具（联动规则 / 通知）：按前缀删，不碰别人的行。
///
/// 额外按 `trigger_entity = 'ACCOUNTING_YEAR'` 兜一遍：`uk_flow_linkage_rule` 是
/// `(trigger_entity, trigger_status, action_code)` 全表唯一索引，库里只要残留一条
/// 同三元组的规则（例如上一轮真机取证脚本按名字前缀清漏了），本卡就再插不进去。
async fn purge_linkage() {
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK11-%'").await;
    exec("DELETE FROM biz_flow_linkage_rule WHERE trigger_entity = 'ACCOUNTING_YEAR'").await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:ACCOUNTING_YEAR:%'").await;
}

/// 取锁 + 连库（`None` = 没有测试库 → 用例 SKIP）+ 清场 + 造操作人。
async fn setup() -> Option<Fixture> {
    let guard = support::setup().await?;
    purge_linkage().await;
    let user = p2::seed_user("p2.lnk11_maker", "LNK11 年结会计").await;
    Some(Fixture { user, _guard: guard })
}

/// 用例结尾：清本卡的规则 / 通知 + 年度夹具（`year_end_test_support::teardown`）。
async fn teardown() {
    purge_linkage().await;
    support::teardown().await;
}

/// 记一张平衡凭证（`(科目, 借, 贷)`），走生产同款 `voucher::post_voucher`。
async fn post_at(date: NaiveDate, source_no: &str, lines: &[(&str, &str, &str)]) {
    let tx = db().begin().await.expect("开启凭证事务");
    let mut entries = Vec::new();
    for (index, (code, debit, credit)) in lines.iter().enumerate() {
        let account = voucher::account_required(&tx, code, "LNK11 夹具")
            .await
            .unwrap_or_else(|err| panic!("取科目 {code} 失败：{err}"));
        entries.push(EntryLine {
            line_no: i32::try_from(index + 1).unwrap_or(0),
            summary: format!("LNK11 夹具 {source_no}"),
            account,
            debit: dec(debit),
            credit: dec(credit),
            aux_supplier_id: None,
            aux_material_id: None,
            aux_dept_id: None,
            aux_warehouse_id: None,
        });
    }
    voucher::post_voucher(
        &tx,
        &PostVoucher {
            voucher_type: voucher::VOUCHER_TYPE_MANUAL,
            source_type: voucher::SOURCE_MANUAL,
            source_id: None,
            source_no: Some(source_no),
            voucher_date: date,
            summary: format!("LNK11 夹具 {source_no}"),
            lines: entries,
            remark: Some("LNK11 夹具"),
            red_of: None,
            status: voucher::STATUS_POSTED,
        },
        Some(support::uid()),
        Utc::now(),
    )
    .await
    .unwrap_or_else(|err| panic!("记凭证 {source_no} 失败：{err}"));
    tx.commit().await.expect("提交凭证事务");
}

/// 本年度（2037）落 3 张凭证 + 12 个月封账 + 下一年度 12 期开账 —— 年结的全部前置。
async fn ready_year() {
    support::open_years().await;
    for (month, amount) in [(3, "1000"), (7, "2500"), (11, "500")] {
        post_at(
            NaiveDate::from_ymd_opt(support::YEAR.into(), month, 20).expect("夹具日期"),
            &format!("LNK11-{month:02}-01"),
            &[("1002", amount, "0"), ("5001", "0", amount)],
        )
        .await;
    }
    support::close_all_months().await;
}

/// 年结（真实入口，与 HTTP handler 同一个函数）。
async fn close(fixture: &Fixture) -> std::result::Result<year_end::YearEndCloseRes, ApiError> {
    year_end::close_year(support::YEAR, Some("LNK-11 用例：年度结转"), fixture.operator()).await
}

/// 反年结（原因 ≥ 10 字）。
async fn reopen(fixture: &Fixture) -> std::result::Result<year_end::YearEndReopenRes, ApiError> {
    year_end::reopen_year(
        support::YEAR,
        Some("LNK-11 用例：反年结以重算结转基数"),
        fixture.operator(),
    )
    .await
}

/// 某状态下的年度联动通知条数（幂等键 = `LNK:ACCOUNTING_YEAR:<状态>:<年度>:<事件凭证>`）。
async fn notices_of(status: &str) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key LIKE ?",
        vec![Value::Text(format!("LNK:ACCOUNTING_YEAR:{status}:{}:%", support::YEAR))],
    )
    .await
    .expect("查联动通知")
}

/// 最新一条年度通知正文（排障 / 文案口径的用户可见证据）。
async fn notice_content(status: &str) -> String {
    db().scalar_with_params::<String>(
        "SELECT content FROM sys_notice WHERE idempotency_key LIKE ? \
          ORDER BY created_at DESC LIMIT 1",
        vec![Value::Text(format!("LNK:ACCOUNTING_YEAR:{status}:{}:%", support::YEAR))],
    )
    .await
    .expect("查联动通知正文")
}

/// 本年度结转凭证条数（`YEAR_END` + `2037-12`，一个年度最多一张）。
async fn carry_vouchers() -> i64 {
    scalar_i64(&format!(
        "SELECT count(*) FROM biz_voucher \
          WHERE voucher_type = 'YEAR_END' AND period = '{}' AND deleted_at IS NULL",
        format!("{}-12", support::YEAR)
    ))
    .await
}

/// 年度状态日志的最新一行的目标态（`None` = 从来没结转/反年结过）。
async fn latest_year_status() -> Option<String> {
    let rows = db()
        .query_sql(
            format!(
                "SELECT to_status AS value FROM biz_status_log \
                  WHERE entity_type = 'ACCOUNTING_YEAR' AND entity_no = '{}' \
                  ORDER BY created_at DESC LIMIT 1",
                support::YEAR
            ),
            vec![],
        )
        .await
        .expect("查年度状态日志");
    rows.first().and_then(|row| row.get_typed::<String>("value").ok())
}

/// 插一条 `ACCOUNTING_YEAR/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_rule(status: &str, action: &str, params: &str) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), 'LNK11-{status}-{action}-{nanos}', 'ACCOUNTING_YEAR', \
                 '{status}', '{action}', '{params}'::jsonb, 10)"
    ))
    .await;
}

/// 规则参数：TAKEOVER + 收件人取制单人（`year_end_linkage` 把操作人同时当 owner / creator）。
const NOTICE_CREATOR: &str = r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#;

// ==================== ① 年结触发 + 多张凭证的年度只发一条 ====================

/// 含**多张凭证**的年度跑真年结：结转凭证 1 张、`ACCOUNTING_YEAR/CLOSED` 的通知 1 条。
///
/// 卡面验收「年结成功 + 联动触发一次 + 通知发出」的证据链：
///
/// * 年结真的成了（`state.closed`、结转凭证号、分录数都对得上）；
/// * 通知是**一条**（触发单据是年度，不是「每张凭证一条」）；
/// * 正文带年度 / 凭证号 / 分录数 / 净利润（用户不点进去也知道结了哪年结了多少）。
#[tokio::test]
async fn lnk11_close_triggers_one_notice_for_multi_voucher_year() {
    let Some(fixture) = setup().await else {
        return;
    };
    ready_year().await;
    let vouchers_before = scalar_i64(&format!(
        "SELECT count(*) FROM biz_voucher WHERE period LIKE '{}-%' AND deleted_at IS NULL",
        support::YEAR
    ))
    .await;
    assert_eq!(vouchers_before, 3, "夹具必须真的落了 3 张凭证");
    add_rule(STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_CREATOR).await;

    let outcome = close(&fixture)
        .await
        .unwrap_or_else(|error| panic!("年结失败：{} {}", error.code, error.msg));
    assert!(outcome.state.closed, "年结成功后年度必须是已结转");
    assert_eq!(outcome.state.status, STATUS_CLOSED);
    assert!(
        outcome.state.carry_forward_voucher_no.is_some(),
        "有损益余额的年度必须出结转凭证"
    );
    assert!(
        outcome.state.carry_forward_entries > 0,
        "结转凭证必须有分录（损益类科目逐条结平）"
    );
    assert_eq!(carry_vouchers().await, 1, "一个年度只能有一张 YEAR_END 凭证");
    assert_eq!(
        latest_year_status().await.as_deref(),
        Some(STATUS_CLOSED),
        "状态日志必须留痕 CLOSED"
    );

    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        1,
        "年结必须触发一条联动通知（触发单据是年度，不是每张凭证一条）"
    );
    let content = notice_content(STATUS_CLOSED).await;
    let voucher_no = outcome.state.carry_forward_voucher_no.clone().expect("结转凭证号");
    let year_text = support::YEAR.to_string();
    for needle in [year_text.as_str(), voucher_no.as_str(), "条分录", "本年净利润"] {
        assert!(content.contains(needle), "年结通知正文必须带 {needle}：{content}");
    }
    teardown().await;
}

// ==================== ② 重复年结不重复触发 ====================

/// 重复年结被 `ALREADY_CLOSED` 409 挡住：通知不增、结转凭证不增。
#[tokio::test]
async fn lnk11_repeat_close_is_blocked_and_does_not_notify() {
    let Some(fixture) = setup().await else {
        return;
    };
    ready_year().await;
    add_rule(STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_CREATOR).await;

    close(&fixture).await.expect("首次年结");
    assert_eq!(notices_of(STATUS_CLOSED).await, 1, "首次年结发一条");
    assert_eq!(carry_vouchers().await, 1);

    let again = close(&fixture).await.expect_err("重复年结必须被挡住");
    assert_eq!(again.status, 409, "重复年结必须是 409：{}", again.msg);
    assert!(
        again.msg.contains("已经结转过了"),
        "被挡住的原因必须说清「已经结转过了」：{}",
        again.msg
    );
    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        1,
        "被挡住的重复年结**不许**再发一条通知"
    );
    assert_eq!(carry_vouchers().await, 1, "重复年结不许再落一张凭证");
    teardown().await;
}

// ==================== ③ 反年结触发（重复反年结不重复触发） ====================

/// 反年结（`CLOSED → NOT_CLOSED`）必须触发 `ACCOUNTING_YEAR/NOT_CLOSED` 上的规则；
/// 第二次反年结被状态机挡住（409），通知不增。
#[tokio::test]
async fn lnk11_reopen_triggers_notice_once() {
    let Some(fixture) = setup().await else {
        return;
    };
    ready_year().await;
    // 只配 NOT_CLOSED：顺带证明「年结不误触发反年结的规则」
    add_rule(STATUS_NOT_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_CREATOR).await;

    let closed = close(&fixture).await.expect("年结");
    assert!(closed.state.closed);
    assert_eq!(
        notices_of(STATUS_NOT_CLOSED).await,
        0,
        "年结不许触发反年结的规则（触发点按**变更后**的状态匹配）"
    );

    let reopened = reopen(&fixture)
        .await
        .unwrap_or_else(|error| panic!("反年结失败：{} {}", error.code, error.msg));
    assert!(reopened.reopened, "反年结必须成功");
    assert_eq!(carry_vouchers().await, 0, "反年结要把结转凭证整张删掉");
    assert_eq!(
        latest_year_status().await.as_deref(),
        Some(STATUS_NOT_CLOSED),
        "状态日志必须留痕 NOT_CLOSED"
    );
    assert_eq!(
        notices_of(STATUS_NOT_CLOSED).await,
        1,
        "ACCOUNTING_YEAR/NOT_CLOSED 上配的 SEND_NOTICE 必须由反年结触发一次"
    );
    let content = notice_content(STATUS_NOT_CLOSED).await;
    assert!(
        content.contains("反年结以重算结转基数"),
        "反年结通知必须带原因：{content}"
    );

    let again = reopen(&fixture).await.expect_err("重复反年结必须被挡住");
    assert_eq!(again.status, 409, "重复反年结必须是 409：{}", again.msg);
    assert_eq!(
        notices_of(STATUS_NOT_CLOSED).await,
        1,
        "被挡住的重复反年结**不许**再发一条通知"
    );
    teardown().await;
}

// ==================== ④ 事务回滚：通知不发 ====================

/// **卡面铁律**：通知走 commit 后 `run_deferred`，所以事务回滚时一条都不许发。
///
/// 三层证据：
///
/// 1. 事务内调 hook：`SEND_NOTICE` 只进 `run.deferred`（事务内不执行），此时库里 0 条；
/// 2. 回滚后再查：仍然 0 条 —— 没有「先发后回滚」的窗口；
/// 3. 正向对照：同一套动作 `commit()` + `run_deferred_after_commit`，通知立刻 1 条
///    （否则上面的 0 可能是「规则没命中」的虚绿）。
#[tokio::test]
async fn lnk11_rolled_back_tx_never_sends_notice() {
    let Some(fixture) = setup().await else {
        return;
    };
    add_rule(STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_CREATOR).await;
    let event = Uuid::now_v7();
    let operator = Some(fixture.user);

    // ---- ① 事务内：通知只在 deferred 里 ----
    let tx = db().begin().await.expect("开事务");
    let change = year_end_linkage::run_in_tx(
        &tx,
        year_end_linkage::YearEndEvent {
            year: support::YEAR,
            status: STATUS_CLOSED,
            voucher_no: Some("PZ-LNK11-0001"),
            entry_count: 6,
            net_profit: dec("4000"),
            reason: Some("LNK-11 回滚用例"),
            event_ref: event,
            operator,
        },
    )
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
        "年度上只配了通知：事务内不该有 InTx 动作被执行"
    );
    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        0,
        "事务还没提交，通知一条都不许发（否则回滚就是幽灵通知）"
    );

    // ---- ② 回滚：通知仍然 0 ----
    tx.rollback().await.expect("回滚事务");
    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        0,
        "事务回滚后通知必须仍然为 0（「通知已发但年结失败」就是卡面点名的事故）"
    );

    // ---- ③ 正向对照：commit 之后 flush 才会发 ----
    let tx = db().begin().await.expect("开事务");
    let change = year_end_linkage::run_in_tx(
        &tx,
        year_end_linkage::YearEndEvent {
            year: support::YEAR,
            status: STATUS_CLOSED,
            voucher_no: Some("PZ-LNK11-0001"),
            entry_count: 6,
            net_profit: dec("4000"),
            reason: Some("LNK-11 回滚用例"),
            event_ref: event,
            operator,
        },
    )
    .await
    .expect("事务内跑联动");
    tx.commit().await.expect("提交事务");
    year_end_linkage::run_deferred_after_commit(change).await;
    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        1,
        "对照：commit 之后 run_deferred 必须真的发出通知（否则上面的 0 是虚绿）"
    );
    teardown().await;
}

// ==================== ⑤ 排除动作确实不跑 ====================

/// 年度上配 `GENERATE_VOUCHER`：通用引擎**不执行**它。
///
/// 两层证据：
///
/// 1. **引擎级**（同一份真实规则行、同一个上下文）：`run_excluding(YEAR_END_EXCLUDED_ACTIONS)`
///    里那条规则**不产生任何 RuleRun**（`is_routed_elsewhere` → `continue`），
///    而不带排除清单的 `run` 会产生 RuleRun —— 说明规则**确实被读到了**，
///    只是被排除清单拦下（不是「规则没生效」这种虚绿）；
/// 2. **端到端**：年结后结转凭证只有一张（`YEAR_END`，年结自己出的），多出来就是重复落账；
///    同状态配的 `SEND_NOTICE` 照常发出（排除清单只排凭证 / 库存 / 余额，不许把通知一起排掉）。
#[tokio::test]
async fn lnk11_excluded_voucher_actions_never_run() {
    let Some(fixture) = setup().await else {
        return;
    };
    add_rule(STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_CREATOR).await;
    add_rule(
        STATUS_CLOSED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_IN","mode":"TAKEOVER"}"#,
    )
    .await;

    // ---- 第一层：引擎级对照（真实规则行 + 真实上下文）----
    let year_text = support::YEAR.to_string();
    let ctx = linkage::LinkageContext::new(
        state_machine::ENTITY_YEAR_END,
        STATUS_CLOSED,
        year_end_linkage::year_doc_id(support::YEAR),
        &year_text,
        Some(fixture.user),
    )
    .with_people(Some(fixture.user), Some(fixture.user), None);
    let tx = db().begin().await.expect("开事务");
    let without_exclusion = linkage::engine::run(&tx, &ctx).await.expect("跑引擎");
    let with_exclusion = linkage::engine::run_excluding(&tx, &ctx, spec::YEAR_END_EXCLUDED_ACTIONS)
        .await
        .expect("跑引擎（带排除清单）");
    tx.rollback().await.expect("回滚");
    assert!(
        without_exclusion
            .runs
            .iter()
            .any(|item| item.action_code == spec::ACTION_GENERATE_VOUCHER),
        "对照：不带排除清单时这条 GENERATE_VOUCHER 规则必须被读到并执行\
         （本例是 SKIPPED，因为该动作只接管收货入库）—— 否则下面的断言就是虚绿"
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

    // ---- 第二层：端到端（真实 close_year）----
    ready_year().await;
    let outcome = close(&fixture)
        .await
        .unwrap_or_else(|error| panic!("年结失败：{} {}", error.code, error.msg));
    assert!(outcome.state.closed);
    assert_eq!(
        carry_vouchers().await,
        1,
        "年结只该有它自己 post_voucher 出的那一张 YEAR_END 凭证：多出来就是 GENERATE_VOUCHER 在重复落账"
    );
    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        1,
        "排除清单只排凭证 / 库存 / 余额：同状态的 SEND_NOTICE 必须照常发出（排掉通知 = 接线了永远不响）"
    );
    teardown().await;
}

// ==================== ⑥ 关键动作失败 → 整笔年结回滚（卡面核心） ====================

/// **卡面「再造一个会失败的场景，验证回滚后通知没发」**。
///
/// 造法：在 `ACCOUNTING_YEAR/CLOSED` 上配一条**关键动作**（`UPDATE_RECEIVED_QTY`，`InTx` +
/// `Critical`），参数指向一张订购 10 件、已收 0 件的采购明细，`qty = 50` ⇒ 必然「超收」失败。
/// 这条规则在年结事务的**最后一步**（所有账都写完之后、commit 之前）执行 → `run_excluding`
/// 返回 `Err` → `close_year` 整笔回滚。
///
/// 断言四件事（缺一条都不算真回滚）：
///
/// 1. 返回 500 且点名是哪个联动动作失败的；
/// 2. 年度**仍然未结转**（状态日志里没有 CLOSED）；
/// 3. 结转凭证 0 张（① 步写的凭证随事务一起没了）；
/// 4. **通知 0 条**（`SEND_NOTICE` 排在 commit 之后，回滚就永远发不出去）。
///
/// 然后撤掉失败规则重跑一次 → 成功 + 通知 1 条：证明上面的 0 不是「规则没命中」的虚绿。
#[tokio::test]
async fn lnk11_critical_action_failure_rolls_back_year_end() {
    let Some(fixture) = setup().await else {
        return;
    };
    ready_year().await;

    // 一张已关闭的采购单 + 一条订购 10 件的明细（`CLOSED` 不算未完成单据，不挡年结）
    let po = support::seed_po("YE-LNK11-PO-1", 5, "CLOSED").await;
    exec(&format!(
        "INSERT INTO biz_purchase_order_item \
            (id, order_id, line_no, material_id, unit_id, qty, price, amount, tax_amount, with_tax_amount) \
         VALUES (gen_random_uuid(), '{po}', 1, gen_random_uuid(), gen_random_uuid(), 10, 0, 0, 0, 0)"
    ))
    .await;
    let item: Uuid = db()
        .query_first_sql(
            "SELECT id FROM biz_purchase_order_item WHERE order_id = ?::uuid".to_string(),
            vec![Value::from(&po)],
        )
        .await
        .expect("取采购明细 id")
        .and_then(|row| row.get_typed::<Uuid>("id").ok())
        .expect("采购明细必须存在");

    add_rule(STATUS_CLOSED, spec::ACTION_SEND_NOTICE, NOTICE_CREATOR).await;
    add_rule(
        STATUS_CLOSED,
        spec::ACTION_UPDATE_RECEIVED_QTY,
        &format!(r#"{{"source_item_id":"{item}","qty":"50"}}"#),
    )
    .await;

    // ---- ① 失败：500 + 点名联动动作 ----
    let failed = close(&fixture).await.expect_err("关键动作失败必须让年结失败");
    assert_eq!(failed.status, 500, "关键动作失败必须是 500：{}", failed.msg);
    assert!(
        failed.msg.contains("UPDATE_RECEIVED_QTY") && failed.msg.contains("超收"),
        "失败信息必须点名是哪个联动动作、为什么失败（排障靠它）：{}",
        failed.msg
    );

    // ---- ②③④ 回滚：年度未结转 / 无结转凭证 / 通知 0 条 ----
    assert_eq!(
        latest_year_status().await,
        None,
        "回滚后状态日志里不许有 CLOSED（年结没真的发生）"
    );
    assert_eq!(
        carry_vouchers().await,
        0,
        "回滚后结转凭证必须是 0 张（① 步写的凭证随事务一起没了）"
    );
    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        0,
        "**卡面核心**：事务回滚后通知一条都不许发（通知排在 commit 之后）"
    );

    // ---- 正向对照：撤掉失败规则重跑 → 成功 + 通知 1 条 ----
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK11-CLOSED-UPDATE_RECEIVED_QTY-%'").await;
    let outcome = close(&fixture)
        .await
        .unwrap_or_else(|error| panic!("撤掉失败规则后年结必须成功：{} {}", error.code, error.msg));
    assert!(outcome.state.closed, "对照：年结必须成功");
    assert_eq!(carry_vouchers().await, 1, "对照：结转凭证 1 张");
    assert_eq!(
        notices_of(STATUS_CLOSED).await,
        1,
        "对照：年结成功之后通知必须发出来（否则上面的 0 是「规则没命中」的虚绿）"
    );
    teardown().await;
}

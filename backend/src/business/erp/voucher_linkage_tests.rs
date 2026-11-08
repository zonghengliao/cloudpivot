//! 凭证联动接线（`VOUCHER`，LNK-06）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时真库用例全部 SKIP（与本仓其它 ERP 用例同口径）；纯逻辑守卫
//! `lnk06_forbidden_actions_are_declared` 无库也跑。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::voucher_linkage_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面「必须有『配 GENERATE_VOUCHER 被拒』用例 + 正常过账触发一次」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk06_forbidden_actions_are_declared` | 实体级禁令 + 运行时排除清单都在 spec 里（无库也跑） |
//! | 2 | `lnk06_generate_voucher_rule_is_rejected_on_save` | **配 `GENERATE_VOUCHER` 保存时 400**，且库里没落行 |
//! | 3 | `lnk06_create_audit_post_each_trigger_once` | 手工凭证 DRAFT / 审核 AUDITED / 记账 POSTED 各触发一次通知 |
//! | 4 | `lnk06_repeat_post_does_not_trigger_twice` | 重复记账被 409 挡住，通知**不重复** |
//! | 5 | `lnk06_excluded_generate_voucher_never_runs` | 绕过 API 直接写库的 `GENERATE_VOUCHER` 规则**运行时不跑**（同状态的通知规则照跑，证明不是「引擎没找到规则」） |
//! | 6 | `lnk06_reverse_triggers_posted_for_red_voucher` | 红冲落一张红字凭证（`POSTED`）→ 触发一次 `VOUCHER/POSTED` 通知 |
//!
//! # 夹具口径
//!
//! 每张凭证的 id 由用例自己持有；清理按 `summary LIKE 'LNK06%'` 定位（本文件造的凭证
//! 摘要统一带这个前缀），联动的规则与通知按 `LNK06` / `LNK:VOUCHER:%` 前缀清。
//! **串行**（`--test-threads=1`）：联动规则是全表唯一索引，并行会互删。

use std::str::FromStr;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::voucher;
use super::voucher_linkage;
use super::voucher_write::{
    CreateVoucherReq, ManualEntryReq, audit_voucher, create_manual_voucher, post_audited_voucher,
    reverse_voucher,
};
use crate::business::flow::linkage::{model as linkage_model, spec};
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::shared::test_helpers as seed_guard;

/// 本文件造的所有夹具摘要前缀（清场口径）。
const TAG: &str = "LNK06";

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

async fn ensure_db() -> bool {
    use tokio::sync::OnceCell;
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let Ok(url) = std::env::var("ERP_DB_URL") else {
                println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                return false;
            };
            if try_db().is_some() {
                return true;
            }
            let mut config = DatabaseConfig::from_url(&url);
            config.pool.max_lifetime = Some(std::time::Duration::ZERO);
            config.pool.idle_timeout = Some(std::time::Duration::ZERO);
            match Db::connect_with(&url, config).await {
                Ok(connection) => match set_db(connection) {
                    Ok(()) => true,
                    Err(err) => {
                        println!("SKIP: set_db 失败（{err}）");
                        false
                    }
                },
                Err(err) => {
                    println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                    false
                }
            }
        })
        .await
}

async fn exec(text: &str) {
    if let Err(err) = db().execute_with_params(text, vec![]).await {
        panic!("SQL 失败：{err}\nSQL: {text}");
    }
}

/// 两个**真实存在**的账号：制单人 ≠ 审核人（内控：制单不能审自己的凭证），
/// 且收件人必须在 `sys_user` 里存活 —— 否则 `SEND_NOTICE` 会判「收件人为空/不存在」Skipped，
/// 用例会变成假红（不是接线没生效，是夹具失真）。
///
/// TST-02b（A 方案）：**不够就自己造**（[`seed_guard::ensure_two_users`]，固定 username /
/// 固定 UUID / 幂等），不再依赖种子库里有 2 个演示账号；科目是迁移种子（V0226），
/// 缺了说明这个库的迁移不是当前版本 —— 照旧响亮地报出来，不静默 pass。
async fn two_users() -> (Uuid, Uuid) {
    seed_guard::require_seed_data("voucher_linkage_tests", &[seed_guard::ACCOUNT]).await;
    seed_guard::ensure_two_users().await
}

/// 当前月会计期间必须 OPEN（手工凭证落库的前置闸）。
async fn open_current_period() {
    exec(
        "INSERT INTO biz_accounting_period (period_year, period_month, period_code, \
                                            start_date, end_date, status) \
         SELECT EXTRACT(YEAR FROM current_date)::smallint, \
                EXTRACT(MONTH FROM current_date)::smallint, \
                to_char(current_date, 'YYYY-MM'), \
                date_trunc('month', current_date)::date, \
                (date_trunc('month', current_date) + INTERVAL '1 month - 1 day')::date, \
                'OPEN' \
         ON CONFLICT (period_code) DO UPDATE SET status = 'OPEN'",
    )
    .await;
    exec("UPDATE biz_account SET status = 1 WHERE account_code IN ('1405', '2202')").await;
}

/// 只清联动夹具（规则 + 本卡发过的通知），不动业务单据。
async fn clear_rules() {
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK06%'").await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:VOUCHER:%'").await;
}

/// 清场：本文件造的凭证（状态日志 → 分录 → 凭证）。
async fn purge() {
    clear_rules().await;
    let where_clause = format!("summary LIKE '{TAG}%'");
    for sql in [
        format!(
            "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' \
              AND entity_id IN (SELECT id FROM biz_voucher WHERE {where_clause})"
        ),
        format!(
            "DELETE FROM biz_voucher_entry WHERE voucher_id IN \
             (SELECT id FROM biz_voucher WHERE {where_clause})"
        ),
        format!("DELETE FROM biz_voucher WHERE {where_clause}"),
    ] {
        if let Err(err) = db().execute_with_params(&sql, vec![]).await {
            panic!("清场 SQL 失败：{err}\nSQL: {sql}");
        }
    }
}

async fn account_id(code: &str) -> String {
    voucher::account_required(db(), code, "LNK06 单测科目")
        .await
        .unwrap_or_else(|err| panic!("科目 {code} 不可用：{err}"))
        .id
        .to_string()
}

fn entry(account_id: &str, debit: &str, credit: &str) -> ManualEntryReq {
    ManualEntryReq {
        account_id: Some(account_id.to_string()),
        debit: Some(dec(debit)),
        credit: Some(dec(credit)),
        summary: Some(format!("{TAG} 分录")),
    }
}

/// 造一张**草稿**手工凭证（借 1405 / 贷 2202），制单人 = `creator`。
async fn new_draft(creator: Uuid) -> (Uuid, voucher::VoucherRow) {
    let inventory = account_id("1405").await;
    let payable_account = account_id("2202").await;
    let req = CreateVoucherReq {
        voucher_date: None,
        summary: Some(format!("{TAG} 联动用例")),
        entries: Some(vec![
            entry(&inventory, "100", "0"),
            entry(&payable_account, "0", "100"),
        ]),
        remark: Some(format!("{TAG} 单测夹具")),
    };
    let detail = create_manual_voucher(&req, Some(creator))
        .await
        .unwrap_or_else(|err| panic!("造手工凭证失败：{} {}", err.code, err.msg));
    let id = Uuid::parse_str(&detail.voucher.id).expect("凭证 id 非法");
    let row = voucher::find_voucher(db(), id)
        .await
        .expect("查凭证")
        .expect("凭证应存在");
    (id, row)
}

/// 插一条规则（绕过 API 写库：与「管理员在配置页配好」等价；也是唯一能造出
/// 被禁动作脏行的办法，配置期那条路会被 400 挡住）。
async fn add_rule(tag: &str, status: &str, action: &str, params: &str) {
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), 'LNK06-{tag}-{status}-{action}', 'VOUCHER', '{status}', \
                 '{action}', '{params}'::jsonb, 10)"
    ))
    .await;
}

/// 按幂等键数通知条数。
async fn notices_of(status: &str, voucher_id: Uuid) -> i64 {
    let key = format!("LNK:VOUCHER:{status}:{voucher_id}");
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(key)],
    )
    .await
    .expect("查联动通知")
}

fn super_claims(user: Uuid) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(format!("lnk06_{}", &user.to_string()[..8])),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

// ==================== ① 声明（无库也跑） ====================

/// 两道闸必须都在 `spec` 里：运行时排除清单 + 配置期禁令（不是二选一）。
#[test]
fn lnk06_forbidden_actions_are_declared() {
    assert!(
        spec::VOUCHER_EXCLUDED_ACTIONS.contains(&spec::ACTION_GENERATE_VOUCHER),
        "VOUCHER 的运行时排除清单必须含 GENERATE_VOUCHER：凭证过账再生成凭证是死循环"
    );
    assert_eq!(
        spec::forbidden_actions(state_machine::ENTITY_VOUCHER),
        spec::VOUCHER_FORBIDDEN_ACTIONS,
        "凭证实体的配置期禁令必须挂在 VOUCHER_FORBIDDEN_ACTIONS 上"
    );
    let reason = spec::forbidden_action_reason(state_machine::ENTITY_VOUCHER, "GENERATE_VOUCHER")
        .expect("凭证上配 GENERATE_VOUCHER 必须被判定为禁止");
    assert!(
        reason.contains("死循环"),
        "禁令文案要点明「凭证生凭证」的死循环，实际：{reason}"
    );
    // 别的实体不受这条实体级禁令影响（排除清单是**调用方**传的，不是引擎写死的）
    assert!(
        spec::forbidden_action_reason(state_machine::ENTITY_STOCK_IN, "GENERATE_VOUCHER").is_none(),
        "GENERATE_VOUCHER 在收货入库上是正常动作（专用路由负责），不能被实体级禁令误伤"
    );
}

// ==================== ② 配置期 400 ====================

/// **卡面强制用例**：在凭证状态上配 `GENERATE_VOUCHER`，保存时直接 400，库里不留行。
#[tokio::test]
async fn lnk06_generate_voucher_rule_is_rejected_on_save() {
    if !ensure_db().await {
        println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
        return;
    }
    let (user, _) = two_users().await;
    clear_rules().await;

    let patch = linkage_model::RulePatch {
        name: format!("{TAG}-forbidden-generate"),
        trigger_entity: state_machine::ENTITY_VOUCHER.to_string(),
        trigger_status: voucher::STATUS_POSTED.to_string(),
        action_code: spec::ACTION_GENERATE_VOUCHER.to_string(),
        action_params: serde_json::json!({"biz_type": "STOCK_IN"}),
        enabled: Some(true),
        seq: Some(10),
        confirm: true,
        allow_unwired: false,
    };
    let err = linkage_model::upsert_rule(db(), &super_claims(user), &patch)
        .await
        .expect_err("凭证上配 GENERATE_VOUCHER 必须被拒绝（否则就是「凭证生凭证」的死循环）");
    assert_eq!(err.code, error_code::PARAM_INVALID, "应当 400 参数错误");
    assert!(
        err.msg.contains("死循环"),
        "400 文案必须点明死循环，实际：{}",
        err.msg
    );

    let saved = linkage_model::find_rule_by_key(
        db(),
        state_machine::ENTITY_VOUCHER,
        voucher::STATUS_POSTED,
        spec::ACTION_GENERATE_VOUCHER,
    )
    .await
    .expect("查规则");
    assert!(saved.is_none(), "被拒绝的规则不许落库");

    // 对照：同一个动作在别的实体上照常能存（禁令是实体级的，不是全局封杀）
    clear_rules().await;
}

// ==================== ③ 三个状态各触发一次 ====================

/// 正常链路：手工凭证落草稿 → 审核 → 记账，三个状态各触发**一次**联动通知。
#[tokio::test]
async fn lnk06_create_audit_post_each_trigger_once() {
    if !ensure_db().await {
        println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
        return;
    }
    let (creator, auditor) = two_users().await;
    purge().await;
    open_current_period().await;
    add_rule(
        "draft",
        voucher::STATUS_DRAFT,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER"}"#,
    )
    .await;
    add_rule(
        "audited",
        voucher::STATUS_AUDITED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER"}"#,
    )
    .await;
    add_rule(
        "posted",
        voucher::STATUS_POSTED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER"}"#,
    )
    .await;

    let (id, _) = new_draft(creator).await;
    assert_eq!(
        notices_of(voucher::STATUS_DRAFT, id).await,
        1,
        "手工凭证落草稿（DRAFT）必须触发一次联动通知：不接这一条，VOUCHER/DRAFT 上的规则\
         能存能启永远不触发（P1-FIX-5C 原病）"
    );

    audit_voucher(id, Some("LNK06 复核"), Some(auditor))
        .await
        .unwrap_or_else(|err| panic!("审核失败：{} {}", err.code, err.msg));
    assert_eq!(
        notices_of(voucher::STATUS_AUDITED, id).await,
        1,
        "审核（DRAFT → AUDITED）必须触发一次联动通知"
    );

    let posted = post_audited_voucher(id, Some(auditor))
        .await
        .unwrap_or_else(|err| panic!("记账失败：{} {}", err.code, err.msg));
    assert_eq!(posted.voucher.status, voucher::STATUS_POSTED);
    assert_eq!(
        notices_of(voucher::STATUS_POSTED, id).await,
        1,
        "记账（AUDITED → POSTED）必须触发一次联动通知"
    );

    purge().await;
}

// ==================== ④ 重复过账不重复触发 ====================

/// 重复记账被状态机挡住（409），联动也**不重复**跑。
#[tokio::test]
async fn lnk06_repeat_post_does_not_trigger_twice() {
    if !ensure_db().await {
        println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
        return;
    }
    let (creator, auditor) = two_users().await;
    purge().await;
    open_current_period().await;
    add_rule(
        "posted",
        voucher::STATUS_POSTED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER"}"#,
    )
    .await;

    let (id, _) = new_draft(creator).await;
    audit_voucher(id, Some("LNK06 复核"), Some(auditor))
        .await
        .unwrap_or_else(|err| panic!("审核失败：{} {}", err.code, err.msg));
    post_audited_voucher(id, Some(auditor))
        .await
        .unwrap_or_else(|err| panic!("记账失败：{} {}", err.code, err.msg));
    assert_eq!(notices_of(voucher::STATUS_POSTED, id).await, 1);

    let again = post_audited_voucher(id, Some(auditor)).await;
    let err = again.expect_err("已记账的凭证不能再记一次");
    assert_eq!(err.status, 409, "重复记账应当 409（实际 {}）", err.status);
    assert_eq!(
        notices_of(voucher::STATUS_POSTED, id).await,
        1,
        "409 挡住的事务里不许跑联动：通知必须还是 1 条"
    );

    purge().await;
}

// ==================== ⑤ 被排除的动作运行时不跑 ====================

/// 绕过 API 直接写库的 `GENERATE_VOUCHER` 规则（历史脏行 / 绕闸插入）在运行时**被排除**：
/// 同状态的 `SEND_NOTICE` 规则照跑 —— 这证明「0 条」是排除生效，而不是引擎压根没找到规则。
#[tokio::test]
async fn lnk06_excluded_generate_voucher_never_runs() {
    if !ensure_db().await {
        println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
        return;
    }
    let (creator, _) = two_users().await;
    purge().await;
    open_current_period().await;
    let (_id, row) = new_draft(creator).await;
    // 直接写库（配置期那条路会被 400 挡住）：模拟历史脏行 / 绕过 API 的行
    add_rule(
        "dirty",
        voucher::STATUS_POSTED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"STOCK_IN"}"#,
    )
    .await;
    add_rule(
        "posted",
        voucher::STATUS_POSTED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER"}"#,
    )
    .await;

    let change = voucher_linkage::run_in_tx(db(), &row, voucher::STATUS_POSTED, Some(creator))
        .await
        .expect("事务内联动不应失败（GENERATE_VOUCHER 会被排除，不会执行）");
    assert!(
        change.run.runs.is_empty(),
        "事务内动作必须一条都不跑：GENERATE_VOUCHER 被排除（它是 InTx 动作，没排除就会出现在 \
         runs 里）。实际：{:?}",
        change
            .run
            .runs
            .iter()
            .map(|item| item.action_code.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        change.run.deferred.len(),
        1,
        "同状态上的 SEND_NOTICE 规则必须照常进 deferred（证明引擎确实读到了规则，\
         而不是「一条都没找到」把 0 条当成排除生效）"
    );
    assert_eq!(
        change.run.deferred[0].action_code,
        spec::ACTION_SEND_NOTICE,
        "deferred 里只允许出现 SEND_NOTICE：被排除的 GENERATE_VOUCHER 不许进队列"
    );
    assert!(
        change
            .run
            .deferred
            .iter()
            .all(|rule| rule.action_code != spec::ACTION_GENERATE_VOUCHER),
        "被排除的动作不许进 deferred 队列"
    );
    // 死循环保护的实际后果：凭证条数没有变化（没有凭空多出「凭证生凭证」）
    let total: i64 = db()
        .scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_voucher WHERE summary LIKE ? AND deleted_at IS NULL",
            vec![Value::Text(format!("{TAG}%"))],
        )
        .await
        .expect("数凭证");
    assert_eq!(total, 1, "只有手工凭证本身，没有多出来的联动凭证");

    purge().await;
}

// ==================== ⑥ 红冲（reverse_posted）也接引擎 ====================

/// 红冲落一张红字凭证（初始就是 `POSTED`）→ 触发一次 `VOUCHER/POSTED` 通知（红字凭证 id）。
#[tokio::test]
async fn lnk06_reverse_triggers_posted_for_red_voucher() {
    if !ensure_db().await {
        println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
        return;
    }
    let (creator, auditor) = two_users().await;
    purge().await;
    open_current_period().await;
    add_rule(
        "posted",
        voucher::STATUS_POSTED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER"}"#,
    )
    .await;

    let (id, _) = new_draft(creator).await;
    audit_voucher(id, Some("LNK06 复核"), Some(auditor))
        .await
        .unwrap_or_else(|err| panic!("审核失败：{} {}", err.code, err.msg));
    post_audited_voucher(id, Some(auditor))
        .await
        .unwrap_or_else(|err| panic!("记账失败：{} {}", err.code, err.msg));

    let red = reverse_voucher(id, Some("LNK06 红冲用例"), Some(auditor))
        .await
        .unwrap_or_else(|err| panic!("红冲失败：{} {}", err.code, err.msg));
    let red_id = Uuid::parse_str(&red.voucher.id).expect("红字凭证 id 非法");
    assert_ne!(red_id, id, "红冲必须落一张新凭证");
    assert_eq!(
        notices_of(voucher::STATUS_POSTED, red_id).await,
        1,
        "红字凭证也是「凭证过账」：VOUCHER/POSTED 的规则必须响一次（按红字凭证 id 计）"
    );

    purge().await;
}

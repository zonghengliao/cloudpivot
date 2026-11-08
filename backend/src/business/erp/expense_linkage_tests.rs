//! 费用报销联动接线（`EXPENSE_REPORT`，LNK-05）的**真库**用例（`ERP_DB_URL` 开关）。
//!
//! 不设 `ERP_DB_URL` 时全部 SKIP（与本仓其它 ERP 用例同口径）。交付前必须真跑：
//!
//! ```bash
//! ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/<干净库>" \
//!   cargo test --lib business::erp::expense_linkage_tests -- --test-threads=1
//! ```
//!
//! # 覆盖（卡面「测试：通过触发 / 付清触发 / 凭证不被重复生成」逐条对应）
//!
//! | # | 用例 | 覆盖 |
//! |---|---|---|
//! | 1 | `lnk05_submit_triggers_linkage_notice` | 提交（DRAFT → SUBMITTED）真的触发通知 |
//! | 2 | `lnk05_approve_triggers_linkage_notice_and_single_voucher` | 审批通过（→ APPROVED）触发通知；**配了 GENERATE_VOUCHER 规则也不重复出凭证** |
//! | 3 | `lnk05_pay_triggers_only_on_full_payment` | 部分付款不触发、付清（→ PAID）触发一次 |
//! | 4 | `lnk05_cancel_triggers_linkage_notice` | 作废（→ CANCELLED）触发通知 + 蓝字凭证已红冲 |
//! | 5 | `lnk05_repeat_submit_and_pay_do_not_trigger_twice` | 重复提交 / 重复付款都被 409 挡住，通知**不重复** |
//! | 6 | `lnk05_reject_reverts_to_draft_and_triggers` | 驳回 → DRAFT 触发（DRAFT 上的规则不会永远不响） |
//!
//! # 夹具口径
//!
//! 不走「扫全库按前缀删」：每张单的主键都由用例自己持有，清理按 **id 集合**删
//! （审批实例 / 待办 / 通知 / 凭证 / 预算占用 / 状态日志 / 明细 / 单头），
//! 联动的规则与通知按 `LNK05` / `LNK:EXPENSE:%` 前缀清（它们没有单据主键可依）。
//!
//! **串行**（`--test-threads=1`）：联动规则是全表唯一索引，并行会互删。

use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path};
use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use super::expense_bridge;
use super::expense_compute;
use super::expense_dto::{
    CancelReq, PayReq, STATUS_APPROVED, STATUS_CANCELLED, STATUS_DRAFT, STATUS_PAID,
    STATUS_SUBMITTED,
};
use super::expense_handler::{report_cancel, report_pay, report_submit};
use super::expense_model::{self, ItemInsert, ReportInsert};
use crate::business::flow::engine;
use crate::business::flow::linkage::spec;

fn dec(text: &str) -> Decimal {
    Decimal::from_str(text).expect("金额字面量非法")
}

fn date(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期夹具")
}

fn uniq() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}{}", now % 1_000_000_000_000, std::process::id())
}

/// 取响应体（`Res<T>` 的 `data`）。
fn data<T: Serialize>(res: Res<T>) -> T {
    res.data.expect("响应体应带 data")
}

/// 以某个真实用户身份造 claims（联调入口都要 SUPER_ADMIN 才能过 `#[nik::permission]`）。
fn claims_of(user: Uuid) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: user.to_string(),
        username: Some(format!("lnk05_{}", &user.to_string()[..8])),
        roles: vec!["SUPER_ADMIN".to_string()],
        permissions: vec!["*".to_string()],
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
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

/// 一名**有部门**的用户（报销单的部门是预算维度，没有部门的用户建不了单）。
async fn reporter() -> Option<(Uuid, Uuid, String)> {
    let rows = db()
        .query_sql(
            "SELECT u.id, u.dept_id, coalesce(nullif(u.real_name,''), u.username) AS name \
               FROM sys_user u JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
              WHERE u.deleted_at IS NULL AND u.dept_id IS NOT NULL \
              ORDER BY u.created_at LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .ok()?;
    let row = rows.first()?;
    Some((
        row.get_typed("id").ok()?,
        row.get_typed("dept_id").ok()?,
        row.get_typed("name").ok()?,
    ))
}

/// 一条费用类型（V0242 种子；`TRAVEL` 优先）。
async fn travel_type() -> Option<expense_model::TypeRow> {
    if let Ok(Some(kind)) = expense_model::type_find_by_code(db(), "TRAVEL").await {
        return Some(kind);
    }
    expense_model::type_active().await.ok()?.into_iter().next()
}

/// 造单（单头 + 一条 6% 可抵扣明细），返回 id。
async fn create_report(with_tax: Decimal) -> Uuid {
    let (user, dept, name) = reporter().await.expect("库里必须有一名带部门的用户");
    let kind = travel_type()
        .await
        .expect("库里必须有费用类型种子（V0242）");
    let expense_date = date("2026-11-03");
    let split = expense_compute::split(with_tax, dec("0.06"), kind.tax_deductible);
    let insert = ReportInsert {
        id: Uuid::now_v7(),
        report_no: format!("EXP-LNK05-{}", uniq()),
        reporter_id: user,
        reporter_name: name,
        dept_id: dept,
        dept_name: "用例部门".to_string(),
        title: "LNK05 联动用例：差旅报销".to_string(),
        expense_date,
        period_code: expense_compute::period_of(expense_date),
        total_amount: split.amount,
        total_tax: split.tax,
        total_with_tax: split.with_tax,
        item_count: 1,
        remark: Some("expense_linkage_tests".to_string()),
        items: vec![ItemInsert {
            line_no: 1,
            expense_date,
            expense_type_id: kind.id,
            type_code: kind.type_code.clone(),
            type_name: kind.type_name.clone(),
            account_id: kind.account_id,
            account_code: kind.account_code.clone(),
            account_name: kind.account_name.clone(),
            amount: split.amount,
            tax_rate: dec("0.06"),
            tax_amount: split.tax,
            with_tax_amount: split.with_tax,
            invoice_no: Some("INV-LNK05-001".to_string()),
            invoice_date: None,
            invoice_type: Some("VAT_SPECIAL".to_string()),
            summary: "高铁票".to_string(),
            remark: None,
        }],
    };
    // 制单人 = 报销人（真实路径上 `report_create` 就是拿 `claims.sub` 当 create_by）：
    // 用例里必须补上，否则 `recipient = CREATOR` 的规则会因为 ctx.creator 为空
    // 被判成「收件人为空」而 Skipped —— 那是夹具失真，不是接线没生效。
    expense_model::report_insert(db(), &insert, Some(user))
        .await
        .expect("插报销单");
    expense_model::items_insert_many(db(), insert.id, &insert.items)
        .await
        .expect("插明细");
    insert.id
}

/// 只清联动夹具（规则 + 本卡发过的通知），**不动业务单据**。
///
/// 为什么单独一个函数：用例开头要「先把上一次留下的规则清掉再建单」，这时候单据
/// 还不存在；而 [`purge`] 是按单据 id 删业务数据的，两者混用会把自己刚建的单删掉
/// （第一版就这么写，六个用例全报「报销单不存在或已删除」）。
async fn clear_rules() {
    exec("DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'LNK05%'").await;
    exec("DELETE FROM sys_notice WHERE idempotency_key LIKE 'LNK:EXPENSE:%'").await;
}

/// 清场：按 id 集合删（顺序受外键约束：流程 → 通知 → 凭证 → 预算 → 状态日志 → 明细 → 单头）。
async fn purge(ids: &[Uuid]) {
    clear_rules().await;
    for id in ids {
        let one = format!("'{}'::uuid", id);
        let instances = format!(
            "(SELECT id FROM biz_flow_instance WHERE biz_type = 'EXPENSE' AND biz_id = {one})"
        );
        let instance_text = format!(
            "(SELECT id::text FROM biz_flow_instance WHERE biz_type = 'EXPENSE' AND biz_id = {one})"
        );
        for sql in [
            format!(
                "UPDATE biz_flow_instance SET prev_instance_id = NULL WHERE prev_instance_id IN {instances}"
            ),
            format!(
                "UPDATE biz_flow_instance SET status = 'CANCELLED', ended_at = now() WHERE id IN {instances} AND status = 'PENDING'"
            ),
            format!("DELETE FROM biz_flow_task WHERE instance_id IN {instances}"),
            format!("DELETE FROM biz_flow_cc WHERE instance_id IN {instances}"),
            format!(
                "DELETE FROM sys_notice_receiver WHERE notice_id IN (SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN {instance_text})"
            ),
            format!(
                "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN {instance_text}"
            ),
            format!("DELETE FROM biz_flow_instance WHERE biz_type = 'EXPENSE' AND biz_id = {one}"),
            format!(
                "DELETE FROM biz_voucher_entry WHERE voucher_id IN (SELECT id FROM biz_voucher WHERE source_id = {one})"
            ),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'VOUCHER' AND entity_id IN (SELECT id FROM biz_voucher WHERE source_id = {one})"
            ),
            format!("DELETE FROM biz_voucher WHERE source_id = {one}"),
            format!("DELETE FROM biz_budget_usage WHERE source_id = {one}"),
            format!(
                "DELETE FROM biz_status_log WHERE entity_type = 'EXPENSE_REPORT' AND entity_id = {one}"
            ),
            format!("DELETE FROM biz_expense_payment WHERE report_id = {one}"),
            format!("DELETE FROM biz_expense_item WHERE report_id = {one}"),
            format!("DELETE FROM biz_expense_report WHERE id = {one}"),
        ] {
            if let Err(err) = db().execute_with_params(&sql, vec![]).await {
                panic!("清场 SQL 失败：{err}\nSQL: {sql}");
            }
        }
    }
}

/// 插一条 `EXPENSE_REPORT/<status>` 上的规则（绕过 API 写库：与「管理员在配置页配好」等价）。
async fn add_rule(tag: &str, status: &str, action: &str, params: &str) {
    exec(&format!(
        "INSERT INTO biz_flow_linkage_rule \
            (id, name, trigger_entity, trigger_status, action_code, action_params, seq) \
         VALUES (gen_random_uuid(), 'LNK05-{tag}-{status}-{action}', 'EXPENSE_REPORT', '{status}', \
                 '{action}', '{params}'::jsonb, 10)"
    ))
    .await;
}

/// 按幂等键数通知条数。
async fn notices_of(status: &str, report_id: Uuid) -> i64 {
    let key = format!("LNK:EXPENSE:{status}:{report_id}");
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM sys_notice WHERE idempotency_key = ?",
        vec![Value::Text(key)],
    )
    .await
    .expect("查联动通知")
}

/// 某报销单名下的蓝字凭证条数（按来源类型分开数：计提 / 付款）。
async fn vouchers_of(report_id: Uuid, source_type: &str) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_voucher \
          WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL",
        vec![
            Value::Text(source_type.to_string()),
            Value::from(&report_id),
        ],
    )
    .await
    .expect("查报销凭证")
}

/// 某报销单名下的**全部**凭证条数（不按来源类型过滤：防「来源类型写错」的重复落账）。
async fn vouchers_any_source(report_id: Uuid) -> i64 {
    db().scalar_with_params::<i64>(
        "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid AND deleted_at IS NULL",
        vec![Value::from(&report_id)],
    )
    .await
    .expect("查报销单全部凭证")
}

async fn status_of(report_id: Uuid) -> String {
    expense_model::report_find(report_id)
        .await
        .expect("查报销单")
        .expect("报销单存在")
        .status
}

/// 提交（走真实 handler：预算守卫 + 状态机 + 发起审批 + 联动）。
async fn submit(report_id: Uuid) -> String {
    // ATT-06：报销提交前必须有发票影像 / 费用凭证，先挂一份 pdf 夹具
    crate::business::attachment::test_support::attach_pdf(
        crate::business::attachment::model::DocKind::ExpenseReport,
        report_id,
    )
    .await;
    let (user, _, _) = reporter().await.expect("用户");
    let detail = data(
        report_submit(
            Extension(AuthClaims(claims_of(user))),
            Path(report_id.to_string()),
        )
        .await
        .unwrap_or_else(|err| panic!("提交失败：{} {}", err.code, err.msg)),
    );
    detail.report.status
}

/// 某报销单在跑的审批实例（`PENDING`）。
async fn running_instance(report_id: Uuid) -> Uuid {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT id FROM biz_flow_instance \
          WHERE biz_type = 'EXPENSE' AND biz_id = ?::uuid AND status = 'PENDING' AND deleted_at IS NULL",
        vec![Value::from(&report_id)],
    )
    .await
    .expect("查审批实例失败")
    .expect("提交后应当有在跑的审批实例")
}

/// 一条实例的 `PENDING` 待办：`(task_id, assignee_id)`。
async fn pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY id",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("读待办失败");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("id").unwrap(),
                row.get_typed("assignee_id").unwrap(),
            )
        })
        .collect()
}

/// 把实例批到终态（只动引擎 + 按 `flow::handler` 的顺序调业务回调）。
///
/// 返回最后的实例状态（`APPROVED` / `REJECTED` / …）。
async fn approve_all(instance_id: Uuid) -> String {
    let mut last = String::new();
    for _ in 0..6 {
        let tasks = pending_tasks(instance_id).await;
        if tasks.is_empty() {
            break;
        }
        for (task_id, assignee) in tasks {
            let out = db()
                .transaction(|tx| async move {
                    engine::approve(&tx, task_id, assignee, Some("同意（LNK-05 单测）")).await
                })
                .await
                .unwrap_or_else(|err| panic!("审批同意失败：{err}"));
            expense_bridge::on_expense_settled(out.instance_id, &out.instance_status).await;
            last = out.instance_status;
        }
        if !last.eq_ignore_ascii_case("PENDING") {
            break;
        }
    }
    last
}

/// 驳回（`ReturnTo::Final`，实例直接落 `REJECTED`）。
async fn reject_all(instance_id: Uuid, reason: &str) -> String {
    let tasks = pending_tasks(instance_id).await;
    assert!(!tasks.is_empty(), "当前节点至少应有一条待办");
    let mut last = String::new();
    for (task_id, assignee) in tasks {
        let out = db()
            .transaction(|tx| async move {
                engine::reject(
                    &tx,
                    task_id,
                    assignee,
                    "驳回（LNK-05 单测）",
                    engine::ReturnTo::Final,
                )
                .await
            })
            .await
            .unwrap_or_else(|err| panic!("驳回失败：{err}"));
        expense_bridge::on_expense_settled(out.instance_id, &out.instance_status).await;
        last = out.instance_status;
    }
    assert!(
        expense_bridge::should_revert(&last),
        "驳回应当落否定终态（实际 {last}，原因 {reason}）"
    );
    last
}

/// 提交并审批通过（返回单号 id 与实例 id），并断言状态真的到了 APPROVED。
async fn submit_and_approve(report_id: Uuid) -> Uuid {
    assert_eq!(
        submit(report_id).await,
        STATUS_SUBMITTED,
        "提交后应当是审批中"
    );
    let instance = running_instance(report_id).await;
    let status = approve_all(instance).await;
    assert_eq!(status, "APPROVED", "审批应当通过（实际 {status}）");
    assert_eq!(status_of(report_id).await, STATUS_APPROVED);
    instance
}

/// 付款（走真实 handler：行锁 + 付款凭证 + 状态推进 + 联动）。
async fn pay(report_id: Uuid, amount: Decimal) -> String {
    let (user, _, _) = reporter().await.expect("用户");
    let detail = data(
        report_pay(
            Extension(AuthClaims(claims_of(user))),
            Path(report_id.to_string()),
            Json(PayReq {
                amount: Some(amount),
                pay_date: Some(date("2026-11-05")),
                pay_method: Some("BANK_TRANSFER".to_string()),
                pay_account_code: Some("1002".to_string()),
                remark: Some("LNK05 付款用例".to_string()),
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("付款失败：{} {}", err.code, err.msg)),
    );
    detail.report.status
}

// ==================== ① 提交触发 ====================

/// 提交（`DRAFT → SUBMITTED`）真的触发联动：`EXPENSE_REPORT/SUBMITTED` 上的
/// `SEND_NOTICE` 在 commit 之后发出来，且通知链回报销单详情。
#[tokio::test]
async fn lnk05_submit_triggers_linkage_notice() {
    if !ensure_db().await {
        return;
    }
    clear_rules().await;
    let report_id = create_report(dec("500")).await;
    add_rule(
        "submit",
        STATUS_SUBMITTED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    assert_eq!(submit(report_id).await, STATUS_SUBMITTED);
    assert_eq!(
        notices_of(STATUS_SUBMITTED, report_id).await,
        1,
        "EXPENSE_REPORT/SUBMITTED 上配的 SEND_NOTICE 必须真的发出一条通知 —— \
         只改 spec.rs 的 wired 标记、handler 不调引擎时这里会是 0"
    );

    let rows = db()
        .find_with_params(
            "SELECT title, biz_type, biz_id, link_url, sender_name FROM sys_notice \
              WHERE idempotency_key = ?",
            vec![Value::Text(format!(
                "LNK:EXPENSE:{STATUS_SUBMITTED}:{report_id}"
            ))],
        )
        .await
        .expect("查通知行");
    let notice = rows.first().expect("通知行");
    assert_eq!(
        notice.get_typed::<String>("title").expect("title"),
        "报销单审批中",
        "标题来自触发点基稿（目标态中文名），不是配置凭空生成"
    );
    assert_eq!(
        notice.get_typed::<String>("biz_type").expect("biz_type"),
        "EXPENSE_REPORT"
    );
    assert_eq!(
        notice.get_typed::<String>("biz_id").expect("biz_id"),
        report_id.to_string(),
        "通知必须指向这张报销单"
    );
    assert!(
        notice
            .get_typed::<String>("link_url")
            .expect("link_url")
            .contains(&report_id.to_string()),
        "通知必须链回报销单详情"
    );

    purge(&[report_id]).await;
}

// ==================== ② 通过触发 + 凭证不被重复生成 ====================

/// 审批通过（`SUBMITTED → APPROVED`）真的触发联动，且**配了 `GENERATE_VOUCHER` 规则
/// 也不会重复出凭证**。
///
/// 这是本卡最要紧的一条：报销的计提凭证是 `execute_approval` 第 ① 步生成的，
/// 通用引擎再跑一次就是「一张单两张计提凭证」——总账翻倍，靠巡检 L5 才能事后发现。
///
/// # 这条用例能证到什么、不能证到什么（别误读成「排除清单已有红证」）
///
/// 通用引擎的 `GENERATE_VOUCHER` 今天只接管 `STOCK_IN`（`actions::prepare_voucher`
/// 对其它实体直接 `Skipped`），所以**把 `EXPENSE_EXCLUDED_ACTIONS` 清空这条用例也不会红**
/// —— 排除清单在当下是**前向防御**（防 `GENERATE_VOUCHER` 扩到 EXPENSE、`REVERSE_VOUCHER`
/// 上线后作废双红冲），不是今天唯一的挡板。实测过：清空排除清单后本用例仍绿。
///
/// 排除清单的守卫在静态侧：`linkage::tests_wiring::expense_wires_generic_engine_and_excludes_voucher`
/// （hook 必须传清单）+ `expense_excluded_actions_are_routed_elsewhere`（引擎确实把这两个码
/// 判成专用路由）。本用例负责的是另一半：**配了凭证规则，报销单实际落下的凭证仍然恰好一张**
/// （按 `source_id` 数全部来源类型，连「来源类型写错」的重复也能抓到）。
#[tokio::test]
async fn lnk05_approve_triggers_linkage_notice_and_single_voucher() {
    if !ensure_db().await {
        return;
    }
    clear_rules().await;
    let report_id = create_report(dec("500")).await;
    add_rule(
        "approve",
        STATUS_APPROVED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;
    // 故意配一条 GENERATE_VOUCHER：若排除清单失效，它会在 APPROVED 上再跑一次。
    // `biz_type` 用 EXPENSE_REPORT —— 库里真有 EXPENSE_REPORT_DEFAULT 模板（计提凭证走它），
    // 所以这条规则**一旦被执行就真的会落一张凭证**；用别的 biz_type 会因「查不到模板」
    // 被 Skipped，排除清单失效也测不出来（虚绿）。
    add_rule(
        "approve",
        STATUS_APPROVED,
        spec::ACTION_GENERATE_VOUCHER,
        r#"{"biz_type":"EXPENSE_REPORT","mode":"TAKEOVER"}"#,
    )
    .await;

    let _instance = submit_and_approve(report_id).await;

    assert_eq!(
        notices_of(STATUS_APPROVED, report_id).await,
        1,
        "EXPENSE_REPORT/APPROVED 上配的 SEND_NOTICE 必须真的发出一条通知"
    );
    assert_eq!(
        vouchers_of(report_id, expense_bridge::SOURCE_REPORT).await,
        1,
        "一张报销单恰好一张计提凭证（来源类型 EXPENSE_REPORT）"
    );
    // 不按来源类型过滤再数一遍：通用引擎若真跑出凭证，可能把 source_type 写成别的值，
    // 只按 EXPENSE_REPORT 数会漏掉这种重复。
    assert_eq!(
        vouchers_any_source(report_id).await,
        1,
        "这张报销单名下总共只许有一张凭证：GENERATE_VOUCHER 在排除清单里，\
         通用引擎不该在 expense_bridge 之外再落一张（不论它把 source_type 写成什么）"
    );

    purge(&[report_id]).await;
}

// ==================== ③ 付清触发（部分付款不触发）====================

/// 付款联动只在**付清**那一次触发：部分付款不改状态（`APPROVED → APPROVED`），
/// 配一条「报销付清」规则却按付款笔数发信是错的。
#[tokio::test]
async fn lnk05_pay_triggers_only_on_full_payment() {
    if !ensure_db().await {
        return;
    }
    clear_rules().await;
    let report_id = create_report(dec("500")).await;
    add_rule(
        "pay",
        STATUS_PAID,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let _instance = submit_and_approve(report_id).await;

    // ① 部分付款 200 / 500 → 状态不动，联动不许跑
    assert_eq!(pay(report_id, dec("200")).await, STATUS_APPROVED);
    assert_eq!(
        notices_of(STATUS_PAID, report_id).await,
        0,
        "部分付款不改变单据状态，付清规则不许触发（否则付 N 笔发 N 条信）"
    );
    assert_eq!(
        vouchers_of(report_id, expense_bridge::SOURCE_PAYMENT).await,
        1,
        "部分付款一张付款凭证"
    );

    // ② 付清 300 / 500 → PAID，联动跑一次
    assert_eq!(pay(report_id, dec("300")).await, STATUS_PAID);
    assert_eq!(
        notices_of(STATUS_PAID, report_id).await,
        1,
        "付清（APPROVED → PAID）必须触发一次付清通知"
    );
    assert_eq!(
        vouchers_of(report_id, expense_bridge::SOURCE_PAYMENT).await,
        2,
        "两笔付款两张付款凭证；GENERATE_VOUCHER 被排除，不会多出第三张"
    );

    purge(&[report_id]).await;
}

// ==================== ④ 作废触发 ====================

/// 作废（`APPROVED → CANCELLED`）触发联动，且计提凭证被红冲**一次**（不是两次）。
#[tokio::test]
async fn lnk05_cancel_triggers_linkage_notice() {
    if !ensure_db().await {
        return;
    }
    clear_rules().await;
    let report_id = create_report(dec("500")).await;
    add_rule(
        "cancel",
        STATUS_CANCELLED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    let _instance = submit_and_approve(report_id).await;
    assert_eq!(
        vouchers_of(report_id, expense_bridge::SOURCE_REPORT).await,
        1
    );

    let (user, _, _) = reporter().await.expect("用户");
    let detail = data(
        report_cancel(
            Extension(AuthClaims(claims_of(user))),
            Path(report_id.to_string()),
            Json(CancelReq {
                reason: Some("LNK05 用例：部门填错，整单作废重提".to_string()),
            }),
        )
        .await
        .unwrap_or_else(|err| panic!("作废失败：{} {}", err.code, err.msg)),
    );
    assert_eq!(detail.report.status, STATUS_CANCELLED);

    assert_eq!(
        notices_of(STATUS_CANCELLED, report_id).await,
        1,
        "EXPENSE_REPORT/CANCELLED 上配的 SEND_NOTICE 必须真的发出一条通知"
    );
    // 红冲：`reverse_report_vouchers` 只红冲一张蓝字；REVERSE_VOUCHER 被排除，
    // 所以红字凭证恰好 1 张、蓝字被标记 reversed_by
    let reds: i64 = db()
        .scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid AND is_red = 1",
            vec![Value::from(&report_id)],
        )
        .await
        .expect("查红字凭证");
    assert_eq!(reds, 1, "一张蓝字计提凭证对应恰好一张红字凭证");
    let unreversed: i64 = db()
        .scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_voucher \
              WHERE source_id = ?::uuid AND is_red = 0 AND reversed_by IS NULL",
            vec![Value::from(&report_id)],
        )
        .await
        .expect("查未红冲蓝字");
    assert_eq!(unreversed, 0, "作废后不许留下未红冲的蓝字凭证");

    purge(&[report_id]).await;
}

// ==================== ⑤ 重复操作不重复触发 ====================

/// 重复提交 / 重复付款都被状态闸挡住（409），联动**不会**跑第二次。
///
/// 联动只在真的推进了状态的那一次事务里跑；这条用例是「幂等」的活证据：
/// 通知条数停在 1，而不是随点击次数增长。
#[tokio::test]
async fn lnk05_repeat_submit_and_pay_do_not_trigger_twice() {
    if !ensure_db().await {
        return;
    }
    clear_rules().await;
    let report_id = create_report(dec("500")).await;
    add_rule(
        "repeat",
        STATUS_SUBMITTED,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;
    add_rule(
        "repeat",
        STATUS_PAID,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    assert_eq!(submit(report_id).await, STATUS_SUBMITTED);
    // 第二次提交：不是草稿 → 409
    let (user, _, _) = reporter().await.expect("用户");
    let again = report_submit(
        Extension(AuthClaims(claims_of(user))),
        Path(report_id.to_string()),
    )
    .await;
    assert!(again.is_err(), "已提交的单再提交必须 409");
    assert_eq!(again.unwrap_err().status, 409);
    assert_eq!(
        notices_of(STATUS_SUBMITTED, report_id).await,
        1,
        "重复提交不许再发一条通知"
    );

    let instance = running_instance(report_id).await;
    assert_eq!(approve_all(instance).await, "APPROVED");
    assert_eq!(pay(report_id, dec("500")).await, STATUS_PAID);
    assert_eq!(notices_of(STATUS_PAID, report_id).await, 1);

    // 已付清再付 → 409（`pay_status = PAID`）
    let again = report_pay(
        Extension(AuthClaims(claims_of(user))),
        Path(report_id.to_string()),
        Json(PayReq {
            amount: Some(dec("1")),
            pay_date: Some(date("2026-11-05")),
            pay_method: Some("BANK_TRANSFER".to_string()),
            pay_account_code: Some("1002".to_string()),
            remark: None,
        }),
    )
    .await;
    assert!(again.is_err(), "已付清的单再付款必须 409");
    assert_eq!(
        notices_of(STATUS_PAID, report_id).await,
        1,
        "重复付款不许再发一条付清通知"
    );

    purge(&[report_id]).await;
}

// ==================== ⑥ 驳回 → 草稿也触发 ====================

/// 驳回（`SUBMITTED → DRAFT`）触发联动。
///
/// 这一条不在卡面的「提交 / 通过 / 付清 / 作废」四条里，但它必须接：
/// `DRAFT` 是**能走到的状态**（驳回 / 撤回都会回到它），不接的话
/// `EXPENSE_REPORT/DRAFT` 上配的规则又能保存、又能启用、永远不触发。
#[tokio::test]
async fn lnk05_reject_reverts_to_draft_and_triggers() {
    if !ensure_db().await {
        return;
    }
    clear_rules().await;
    let report_id = create_report(dec("500")).await;
    add_rule(
        "reject",
        STATUS_DRAFT,
        spec::ACTION_SEND_NOTICE,
        r#"{"mode":"TAKEOVER","recipient":"CREATOR"}"#,
    )
    .await;

    assert_eq!(submit(report_id).await, STATUS_SUBMITTED);
    let instance = running_instance(report_id).await;
    let status = reject_all(instance, "LNK05 驳回用例").await;
    assert!(expense_bridge::should_revert(&status));
    assert_eq!(status_of(report_id).await, STATUS_DRAFT, "驳回后回草稿");
    assert_eq!(
        notices_of(STATUS_DRAFT, report_id).await,
        1,
        "EXPENSE_REPORT/DRAFT 上配的 SEND_NOTICE 必须真的发出一条通知（驳回回草稿）"
    );

    purge(&[report_id]).await;
}

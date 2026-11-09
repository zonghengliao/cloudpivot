//! [`super`]（`year_end_remind`）的真库用例 —— YEAR-END 的催办口径。
//!
//! 跑法（缺 `ERP_DB_URL` 时全部 SKIP，不假绿）：
//!
//! ```bash
//! export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_year_end_clean'
//! cargo test --lib business::erp::year_end_remind -- --test-threads=1
//! ```
//!
//! 与 `period_remind` 的 11 条用例是**同一套口径**（收件人服务端现算 / 24h 防重 /
//! 计数自洽 / 没处理人不发），差别只有标题词形与 `biz_type`：
//!
//! ```text
//! 期间：2036-03 月结账待处理：IN-…   biz_type = period_remind
//! 年结：2037 年结账待处理：PO-…      biz_type = year_end_remind
//! ```

use super::*;
use crate::business::erp::period_test_support as p2;
use crate::business::erp::year_end::BLOCKER_DOC_UNFINISHED;
use crate::business::erp::year_end_test_support as support;
use nik::uuid::Uuid;

/// 本年夹具单据上的 `year_end_remind` 通知条数。
async fn notice_count() -> i64 {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM sys_notice \
          WHERE biz_type = 'year_end_remind' \
            AND biz_id IN (SELECT id::text FROM biz_purchase_order WHERE order_no LIKE 'YE-%' \
                           UNION SELECT id::text FROM biz_stock_in WHERE in_no LIKE 'YE-%')",
        vec![],
    )
    .await
    .expect("统计年结催办通知")
}

/// 请求构造器（`kind` 空 = 全部，`ids` 空 = 该类全部）。
fn req(year: i16, kind: Option<&str>, ids: &[String]) -> YearEndRemindReq {
    YearEndRemindReq {
        year,
        blocker_kind: kind.map(str::to_string),
        item_ids: ids.to_vec(),
    }
}

// ==================== ① 纯逻辑：上下文词形与权限码 ====================

/// 年结催办的标题 / 正文 / 幂等键 / 业务类型与卡面逐字一致；
/// 权限码是卡面点名的那个串（前端、后端、V0208 三处必须同一个）。
#[test]
fn scope_is_year_shaped_and_permission_code_is_pinned() {
    assert_eq!(PERM_REMIND, "business:erp:year-end:remind");
    assert_eq!(BIZ_TYPE, "year_end_remind");

    let item = BlockingItem {
        doc_type: "PURCHASE_ORDER".to_string(),
        doc_id: Uuid::nil(),
        doc_no: "PO-2037-001".to_string(),
        status_label: "已提交待审批".to_string(),
        amount: Some(nik::prelude::rust_decimal::Decimal::new(8800, 2)),
        created_by_name: Some("赵六".to_string()),
        owner_role: Some("YE-ROLE-PO-APPROVE".to_string()),
        owner_name: Some("采购主管".to_string()),
        action_label: "去审批".to_string(),
        route: "/business/erp/purchase-order?po_id=x".to_string(),
        waiting_since: None,
        owner_role_id: None,
        owner_user_id: None,
    };
    let scope = scope_of(2037);

    let title = period_remind::title_of(&scope, &item);
    assert_eq!(
        title, "2037 年结账待处理：PO-2037-001",
        "标题必须是「{{年度}} 年结账待处理：{{单据号}}」"
    );

    let content = period_remind::content_of(&scope, &item, Utc::now());
    assert!(content.contains("挡住了 2037 年度的结账"), "正文要点名年度：{content}");
    assert!(content.contains("请去审批"), "正文要写下一步动作：{content}");
    assert!(content.contains("（88）"), "正文要带金额：{content}");

    let key = period_remind::idempotency_key_of(&scope, &item, Utc::now());
    assert!(
        key.starts_with("YEAREND:REMIND:2037:PURCHASE_ORDER:"),
        "年结的幂等键必须与期间催办分开（否则同一张单两处互相当成重复）：{key}"
    );

    // 期间催办的词形不能被这次改动带偏（回归护栏）
    let period_scope = period_remind::RemindScope::period("2036-03");
    assert_eq!(
        period_remind::title_of(&period_scope, &item),
        "2036-03 月结账待处理：PO-2037-001"
    );
    assert!(
        period_remind::content_of(&period_scope, &item, Utc::now()).contains("挡住了 2036-03 的结账"),
        "期间催办正文词形必须保持原样"
    );
}

/// 挑明细：按 `blocker_kind` 过滤 + 按 `item_ids` 收窄；非法 UUID → 400。
#[test]
fn pick_filters_by_kind_and_ids() {
    let make = |doc_no: &str| BlockingItem {
        doc_type: "PURCHASE_ORDER".to_string(),
        doc_id: Uuid::now_v7(),
        doc_no: doc_no.to_string(),
        status_label: "草稿（未提交）".to_string(),
        amount: None,
        created_by_name: None,
        owner_role: None,
        owner_name: None,
        action_label: "去提交".to_string(),
        route: "/business/erp/purchase-order".to_string(),
        waiting_since: None,
        owner_role_id: None,
        owner_user_id: None,
    };
    let first = make("YE-PO-001");
    let first_id = first.doc_id;
    let second = make("YE-PO-002");
    let blockers = vec![
        year_end::YearEndBlocker {
            kind: BLOCKER_DOC_UNFINISHED.to_string(),
            periods: Vec::new(),
            count: Some(2),
            diff: None,
            message: "x".to_string(),
            link: None,
            samples: Vec::new(),
            items: vec![first, second],
        },
        year_end::YearEndBlocker {
            kind: year_end::BLOCKER_INVARIANT.to_string(),
            periods: Vec::new(),
            count: Some(1),
            diff: None,
            message: "y".to_string(),
            link: None,
            samples: Vec::new(),
            items: vec![make("YE-V-901")],
        },
    ];

    let only_docs = pick(&blockers, Some(BLOCKER_DOC_UNFINISHED), &[]).expect("挑选");
    assert_eq!(only_docs.len(), 2, "按类过滤");
    let one = pick(&blockers, Some(BLOCKER_DOC_UNFINISHED), &[first_id.to_string()]).expect("挑选");
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].doc_no, "YE-PO-001");
    let all = pick(&blockers, None, &[]).expect("挑选");
    assert_eq!(all.len(), 3, "空 kind = 全部");
    let bad = pick(&blockers, None, &["not-a-uuid".to_string()]);
    assert!(bad.is_err(), "非法 UUID 必须 400");
    assert_eq!(bad.expect_err("应报错").status, 400);
}

// ==================== ② 有 owner 的发出去 ====================

/// 有处理人的明细 → 真的发出站内信；计数自洽；标题 / 正文 / 路由都是年结口径。
#[tokio::test]
async fn remind_sends_to_owner_and_counts_are_consistent() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_po("YE-PO-001", 3, "SUBMITTED").await;
    support::seed_stock_in("YE-IN-001", 6, "DRAFT").await;
    p2::ensure_role_for(
        "business:erp:purchase-order:status",
        "YE-ROLE-PO-APPROVE",
        &[("ye.po", "采购主管")],
    )
    .await;
    p2::ensure_role_for(
        "business:erp:stock-in:status",
        "YE-ROLE-SI-POST",
        &[("ye.si", "仓管甲")],
    )
    .await;

    // 明细里到底有几条有处理人（取决于库里的授权，不写死）
    let blockers = year_end::collect_blockers(db(), support::YEAR).await.expect("年结预检");
    let total_items: Vec<_> = blockers.iter().flat_map(|blocker| blocker.items.iter()).collect();
    let with_owner = i64::try_from(total_items.iter().filter(|item| item.has_owner()).count()).unwrap_or_default();
    assert!(total_items.len() >= 2, "至少两张挡路单据");

    let res = remind_year_end(&req(support::YEAR, None, &[]), support::operator())
        .await
        .expect("年结催办应成功");

    assert_eq!(
        i64::try_from(total_items.len()).unwrap_or_default(),
        res.total,
        "命中的明细条数必须与预检一致：{res:?}"
    );
    assert_eq!(
        res.sent + res.failed + res.skipped_no_owner + res.skipped_recent,
        res.total,
        "计数必须自洽：{res:?}"
    );
    assert_eq!(res.sent, with_owner, "有处理人的必须发出去：{res:?}");
    assert!(
        res.message.starts_with("2037 年度："),
        "摘要要以「2037 年度：」开头：{}",
        res.message
    );

    let stored = notice_count().await;
    assert_eq!(stored, res.sent, "库里通知条数必须等于 sent");

    let rows = db()
        .query_sql(
            "SELECT title, content, link_url, biz_type, sender_name FROM sys_notice \
              WHERE biz_type = 'year_end_remind' \
                AND biz_id IN (SELECT id::text FROM biz_purchase_order WHERE order_no LIKE 'YE-%') \
              ORDER BY created_at LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .expect("读通知");
    let row = rows.first().expect("应有年结催办通知");
    let title: String = row.get_typed("title").expect("title");
    let content: String = row.get_typed("content").expect("content");
    let link: Option<String> = row.get_typed("link_url").ok();
    assert_eq!(title, format!("2037 年结账待处理：{}", "YE-PO-001"));
    assert!(content.contains("挡住了 2037 年度的结账"), "正文要点名年度：{content}");
    assert!(content.contains("请去审批"), "正文要写动作：{content}");
    assert!(
        link.unwrap_or_default()
            .starts_with("/business/erp/purchase-order?po_id="),
        "通知要能一键直达单据"
    );
    support::teardown().await;
}

// ==================== ③ 24h 防重 ====================

/// 同一张单 24 小时内再催一次 → 全部计入 `skipped_recent`，不重复打扰。
#[tokio::test]
async fn remind_dedupes_within_24_hours() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_po("YE-PO-001", 2, "SUBMITTED").await;
    p2::ensure_role_for(
        "business:erp:purchase-order:status",
        "YE-ROLE-PO-APPROVE",
        &[("ye.po", "采购主管")],
    )
    .await;

    let first = remind_year_end(
        &req(support::YEAR, Some(BLOCKER_DOC_UNFINISHED), &[]),
        support::operator(),
    )
    .await
    .expect("第一次催办");
    assert!(first.sent >= 1, "第一次必须发出去：{first:?}");

    let second = remind_year_end(
        &req(support::YEAR, Some(BLOCKER_DOC_UNFINISHED), &[]),
        support::operator(),
    )
    .await
    .expect("第二次催办");
    assert_eq!(second.sent, 0, "24 小时内不许重复发：{second:?}");
    assert_eq!(
        second.skipped_recent, first.total,
        "全部应计入 skipped_recent：{second:?}"
    );
    assert!(
        second.message.contains("24小时内已催过"),
        "摘要要说清为什么不发：{}",
        second.message
    );
    assert_eq!(notice_count().await, first.sent, "重复催办不该多出通知行");
    support::teardown().await;
}

// ==================== ④ 没有处理人就不发 ====================

/// 反查不到任何处理人 → `skipped_no_owner`，**一条通知都不发**（催全员等于没人负责）。
#[tokio::test]
async fn remind_skips_items_without_owner() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_po("YE-PO-001", 5, "SUBMITTED").await;

    // 把「去审批」与「去提交」两个权限码的授权全部摘掉（测反查逻辑，测完还原）
    let mut saved = Vec::new();
    for code in [
        "business:erp:purchase-order:status",
        "business:erp:purchase-order:submit-approval",
        "business:erp:stock-in:create",
    ] {
        if let Some(menu) = support::menu_id_of(code).await {
            saved.extend(support::grants_of(menu).await);
            support::sql(&format!("DELETE FROM sys_role_menu WHERE menu_id = '{menu}'")).await;
        }
    }

    let res = remind_year_end(
        &req(support::YEAR, Some(BLOCKER_DOC_UNFINISHED), &[]),
        support::operator(),
    )
    .await
    .expect("催办不该报错");
    assert_eq!(res.sent, 0, "没有处理人就不许发：{res:?}");
    assert_eq!(res.skipped_no_owner, res.total, "{res:?}");
    assert!(res.message.contains("未指定处理人"), "{}", res.message);
    assert_eq!(notice_count().await, 0, "库里不该有通知");

    support::restore_grants(&saved).await;
    support::teardown().await;
}

// ==================== ⑤ 空 / 非法入参 ====================

/// 年度非法 / 勾选过多 / 没有挡路项 → 各自的正确结果（400 或 total=0），不 500。
#[tokio::test]
async fn remind_handles_empty_and_invalid_requests() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;

    // ① 年度越界 → 400
    let bad_year = remind_year_end(&req(1999, None, &[]), support::operator())
        .await
        .expect_err("非法年度必须 400");
    assert_eq!(bad_year.status, 400, "{}", bad_year.msg);

    // ② 勾选超过上限 → 400
    let too_many = vec!["00000000-0000-7000-8000-000000000000".to_string(); REMIND_MAX_ITEMS + 1];
    let err = remind_year_end(&req(support::YEAR, None, &too_many), support::operator())
        .await
        .expect_err("超过上限必须 400");
    assert_eq!(err.status, 400, "{}", err.msg);

    // ③ 没有任何挡路项 → total = 0（不是 404 / 500）
    let empty = remind_year_end(&req(support::YEAR, None, &[]), support::operator())
        .await
        .expect("空年度催办不该报错");
    assert_eq!(empty.total, 0);
    assert!(
        empty.message.contains("没有可催办"),
        "要说清没有可催的：{}",
        empty.message
    );

    // ④ 勾了不存在的单据 id → 命中 0 条（同样不报错）
    let ghost = remind_year_end(
        &req(
            support::YEAR,
            None,
            &["00000000-0000-7000-8000-000000000000".to_string()],
        ),
        support::operator(),
    )
    .await
    .expect("不存在的 id 不该报错");
    assert_eq!(ghost.total, 0);
    support::teardown().await;
}

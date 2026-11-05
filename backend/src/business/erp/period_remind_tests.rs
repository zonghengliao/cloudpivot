//! [`super`]（`period_remind`）的真库用例 —— PERIOD-CLOSE 的催办口径。
//!
//! 跑法（缺 `ERP_DB_URL` 时全部 SKIP，不假绿）：
//!
//! ```bash
//! export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test'
//! cargo test --lib business::erp::period_remind -- --test-threads=1
//! ```

use super::*;
use crate::business::erp::period::{self, BLOCKER_DRAFT_VOUCHER, BLOCKER_INVARIANT};
use crate::business::erp::period_test_support as support;

/// 期间 id 字符串 → `Uuid`。
fn uuid_of(raw: &str) -> Uuid {
    Uuid::parse_str(raw).expect("期间 id 不是 uuid")
}

/// 本期间 `period_remind` 通知条数。
async fn notice_count(period_code: &str) -> i64 {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM sys_notice n \
          WHERE n.biz_type = 'period_remind' \
            AND n.biz_id IN (SELECT id::text FROM biz_voucher WHERE period = ?)",
        vec![Value::Text(period_code.to_string())],
    )
    .await
    .expect("统计催办通知")
}

// ==================== ① 有 owner 的发出去，没 owner 的记 skipped_no_owner ====================

/// 有处理人的明细 → 真的发出通知；计数自洽（`sent + failed + skipped_* = total`）。
#[tokio::test]
async fn remind_sends_to_owner_and_counts_are_consistent() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(2).await;
    support::ensure_role_for(
        period_items::PERM_VOUCHER_POST,
        "P2-ROLE-POST",
        &[("p2.owner", "李四")],
    )
    .await;
    for no in ["P2-V-001", "P2-V-002"] {
        support::seed_voucher("2033-02", no, "DRAFT", ("10", "10"), ("10", "10")).await;
    }

    // 先看明细里到底有几条有处理人（反查是否命中由库里的角色授权决定，不写死）
    let mut owners = period_items::OwnerResolver::new();
    let (_, items) = period_items::draft_voucher_items(db(), "2033-02", &mut owners)
        .await
        .expect("取明细");
    let with_owner = items.iter().filter(|item| item.has_owner()).count() as i64;
    assert_eq!(items.len(), 2);

    let period_id = period_of("2033-02").await;
    let res = remind_period(
        period_id,
        &RemindReq {
            blocker_kind: Some(BLOCKER_DRAFT_VOUCHER.to_string()),
            item_ids: Vec::new(),
        },
        support::operator(),
    )
    .await
    .expect("催办应成功");

    assert_eq!(res.total, 2, "两张草稿凭证都要被点到");
    assert_eq!(
        res.sent + res.failed + res.skipped_no_owner + res.skipped_recent,
        res.total,
        "计数必须自洽：{res:?}"
    );
    assert_eq!(res.sent, with_owner, "有处理人的必须发出去：{res:?}");
    assert_eq!(
        res.skipped_no_owner,
        2 - with_owner,
        "没处理人的必须记 skipped_no_owner：{res:?}"
    );

    // 真的落库了 sys_notice（不是只在返回值里「声称」发了）
    let stored = notice_count("2033-02").await;
    assert_eq!(stored, res.sent, "库里通知条数必须等于 sent：{res:?}");

    // 通知内容包含 单号 + 期间 + 待办动作（卡面模板）
    //
    // ⚠️ 必须**按本用例期间的凭证**收窄：`period_remind` 是全库共用的 biz_type，
    // 别的用例（真机演示 2036-03、其他模块的夹具）也会落同名通知。
    // 早期版本写的是 `ORDER BY created_at LIMIT 1`，在全量跑时就抓到了别人的通知
    // （实测全量 `cargo test --lib` 因此红过一次：标题里是 2036-03）。
    let rows = db()
        .query_sql(
            "SELECT title, content, link_url FROM sys_notice \
              WHERE biz_type = 'period_remind' \
                AND biz_id IN (SELECT id::text FROM biz_voucher WHERE period = '2033-02') \
              ORDER BY created_at LIMIT 1"
                .to_string(),
            vec![],
        )
        .await
        .expect("读通知");
    let row = rows.first().expect("应有通知");
    let title: String = row.get_typed("title").expect("title");
    let content: String = row.get_typed("content").expect("content");
    let link: Option<String> = row.get_typed("link_url").ok();
    assert!(title.contains("2033-02"), "标题要点名期间：{title}");
    assert!(title.contains("结账待处理"), "标题：{title}");
    assert!(content.contains("P2-V-"), "正文要点名单号：{content}");
    assert!(
        content.contains("请去过账"),
        "正文要写下一步动作：{content}"
    );
    assert!(content.contains("已等待"), "正文要写等了多久：{content}");
    assert!(
        link.unwrap_or_default()
            .starts_with("/business/erp/voucher?voucher_no="),
        "通知要能一键直达单据"
    );
}

/// 没有处理人的明细（不变量类）→ 全部记 `skipped_no_owner`，一条都不发。
#[tokio::test]
async fn remind_skips_items_without_owner() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(3).await;
    support::seed_voucher(
        "2033-03",
        "P2-V-901",
        "POSTED",
        ("100", "100"),
        ("60", "40"),
    )
    .await;
    let period_id = period_of("2033-03").await;

    let res = remind_period(
        period_id,
        &RemindReq {
            blocker_kind: Some(BLOCKER_INVARIANT.to_string()),
            item_ids: Vec::new(),
        },
        support::operator(),
    )
    .await
    .expect("催办应成功");

    assert!(res.total >= 1, "不变量明细应被点到：{res:?}");
    assert_eq!(res.sent, 0, "没有处理人就不该发：{res:?}");
    assert_eq!(res.skipped_no_owner, res.total);
    assert_eq!(notice_count("2033-03").await, 0);
    assert!(
        res.message.contains("未指定处理人"),
        "结论要说明为什么没发：{}",
        res.message
    );
}

// ==================== ② 24 小时防重复 ====================

/// 同一张单 24 小时内催第二次 → 被拦（`skipped_recent`），库里不新增通知。
#[tokio::test]
async fn remind_dedupes_within_24_hours() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(4).await;
    support::ensure_role_for(
        period_items::PERM_VOUCHER_POST,
        "P2-ROLE-POST",
        &[("p2.owner", "李四")],
    )
    .await;
    support::seed_voucher("2033-04", "P2-V-001", "DRAFT", ("10", "10"), ("10", "10")).await;
    let period_id = period_of("2033-04").await;
    let request = RemindReq {
        blocker_kind: Some(BLOCKER_DRAFT_VOUCHER.to_string()),
        item_ids: Vec::new(),
    };

    let first = remind_period(period_id, &request, support::operator())
        .await
        .expect("首次催办");
    if first.sent == 0 {
        println!("SKIP: 该环境没有持有过账权限的角色，无法验证「发出去再拦」");
        return;
    }

    let second = remind_period(period_id, &request, support::operator())
        .await
        .expect("二次催办应成功返回（不报错）");
    assert_eq!(second.sent, 0, "24 小时内不许再发：{second:?}");
    assert_eq!(
        second.skipped_recent, first.sent,
        "重复的应记 skipped_recent：{second:?}"
    );
    assert_eq!(
        notice_count("2033-04").await,
        first.sent,
        "库里通知条数不能增长"
    );
    assert!(
        second.message.contains("已催过") || second.message.contains("不重复打扰"),
        "结论要说清为什么没发：{}",
        second.message
    );
}

// ==================== ③ 入参护栏 ====================

/// 非法 `item_ids` → 400（不静默忽略）。
#[tokio::test]
async fn remind_rejects_invalid_item_ids() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(5).await;
    support::seed_voucher("2033-05", "P2-V-001", "DRAFT", ("10", "10"), ("10", "10")).await;
    let period_id = period_of("2033-05").await;

    let err = remind_period(
        period_id,
        &RemindReq {
            blocker_kind: Some(BLOCKER_DRAFT_VOUCHER.to_string()),
            item_ids: vec!["not-a-uuid".to_string()],
        },
        support::operator(),
    )
    .await
    .expect_err("非法 UUID 必须 400");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("item_ids"), "文案要点名入参：{}", err.msg);
}

/// 指定 `item_ids` → 只催选中那几张。
#[tokio::test]
async fn remind_only_selected_items() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(6).await;
    support::ensure_role_for(
        period_items::PERM_VOUCHER_POST,
        "P2-ROLE-POST",
        &[("p2.owner", "李四")],
    )
    .await;
    let first =
        support::seed_voucher("2033-06", "P2-V-001", "DRAFT", ("10", "10"), ("10", "10")).await;
    support::seed_voucher("2033-06", "P2-V-002", "DRAFT", ("10", "10"), ("10", "10")).await;
    let period_id = period_of("2033-06").await;

    let res = remind_period(
        period_id,
        &RemindReq {
            blocker_kind: Some(BLOCKER_DRAFT_VOUCHER.to_string()),
            item_ids: vec![first.to_string()],
        },
        support::operator(),
    )
    .await
    .expect("催办应成功");
    assert_eq!(res.total, 1, "只催选中的那一张：{res:?}");

    // 库里只该有这一张单的通知
    let stored: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM sys_notice WHERE biz_type = 'period_remind' AND biz_id = ?",
            vec![Value::Text(first.to_string())],
        )
        .await
        .expect("统计通知");
    assert_eq!(stored, res.sent);
}

/// 期间没有挡路单据 → `total = 0`，一句人话结论，不报错。
#[tokio::test]
async fn remind_on_clean_period_is_noop() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(7).await;
    let period_id = period_of("2033-07").await;

    let res = remind_period(period_id, &RemindReq::default(), support::operator())
        .await
        .expect("干净期间催办应成功返回");
    assert_eq!(res.total, 0);
    assert_eq!(res.sent, 0);
    assert!(
        res.message.contains("没有可催办"),
        "结论要说清：{}",
        res.message
    );
}

/// 期间 id 不存在 → 404。
#[tokio::test]
async fn remind_unknown_period_is_404() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let err = remind_period(Uuid::now_v7(), &RemindReq::default(), support::operator())
        .await
        .expect_err("不存在的期间必须 404");
    assert_eq!(err.status, 404, "{}", err.msg);
}

// ==================== ④ 权限 ====================

/// 无 `business:erp:period:remind` → 403；该权限码有 `sys_menu` 节点（V0207）。
#[tokio::test]
async fn remind_permission_is_enforced_and_seeded() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let no_perm = crate::system::tests_support::claims(support::TEST_UID, "pc-no-perm", &[]);
    let err = period::ensure_permission(&no_perm.0, PERM_REMIND, "催办结账待办")
        .expect_err("无码必须 403");
    assert_eq!(err.status, 403);
    assert!(
        err.msg.contains(PERM_REMIND),
        "文案要点名权限码：{}",
        err.msg
    );

    let with_perm =
        crate::system::tests_support::claims(support::TEST_UID, "pc-finance", &[PERM_REMIND]);
    assert!(period::ensure_permission(&with_perm.0, PERM_REMIND, "催办结账待办").is_ok());

    // 权限码必须有 sys_menu 节点，否则注解守的是一个不存在的码，勾给角色永远不生效
    let rows = db()
        .query_sql(
            "SELECT auth_code FROM sys_menu WHERE auth_code = ? AND deleted_at IS NULL".to_string(),
            vec![Value::Text(PERM_REMIND.to_string())],
        )
        .await
        .expect("查权限节点");
    assert_eq!(
        rows.len(),
        1,
        "权限码 {PERM_REMIND} 没有 sys_menu 节点（V0207 未应用？）"
    );
}

// ==================== ⑤ 纯逻辑（不连库） ====================

/// 明细 → 草稿：标题 / 正文 / 幂等键 / 送达强度。
#[test]
fn draft_template_matches_card() {
    let item = period_items::BlockingItem {
        doc_type: period_items::DOC_STOCK_IN.to_string(),
        doc_id: Uuid::nil(),
        doc_no: "IN-2033-08-001".to_string(),
        status_label: "草稿（未过账）".to_string(),
        amount: Some(Decimal::new(123400, 2)),
        created_by_name: Some("王五".to_string()),
        owner_role: Some("WAREHOUSE".to_string()),
        owner_name: Some("赵六".to_string()),
        action_label: "去过账".to_string(),
        route: "/business/erp/stock-in?in_id=x".to_string(),
        waiting_since: Some(Utc::now() - Duration::days(3)),
        owner_role_id: Some(Uuid::now_v7()),
        owner_user_id: Some(Uuid::now_v7()),
    };
    let operator = state_machine::Operator::named(Some(Uuid::nil()), "财务主管");
    let draft = build_draft("2033-08", &item, Utc::now(), &operator).expect("有 owner 就该有草稿");

    assert_eq!(draft.title, "2033-08 月结账待处理：IN-2033-08-001");
    assert!(
        draft.content.contains("王五"),
        "正文要点名制单人：{}",
        draft.content
    );
    assert!(draft.content.contains("收货单 IN-2033-08-001"));
    assert!(
        draft.content.contains("1234"),
        "金额要写进去：{}",
        draft.content
    );
    assert!(draft.content.contains("挡住了 2033-08 的结账"));
    assert!(draft.content.contains("请去过账"));
    assert!(
        draft.content.contains("已等待 3 天"),
        "等了多久要写：{}",
        draft.content
    );
    assert_eq!(draft.biz_type.as_deref(), Some(BIZ_TYPE));
    assert_eq!(
        draft.biz_id.as_deref(),
        Some(Uuid::nil().to_string().as_str())
    );
    assert!(draft.force, "结账被挡是必须处理的一类，强制送达");
    assert!(
        draft
            .idempotency_key
            .as_deref()
            .unwrap_or_default()
            .starts_with("PERIOD:REMIND:2033-08:STOCK_IN:")
    );
    assert_eq!(draft.sender_name.as_deref(), Some("财务主管"));

    // 没有 owner 的明细 → 不给草稿（不发，不猜收件人）
    let mut orphan = item.clone();
    orphan.owner_role_id = None;
    orphan.owner_user_id = None;
    assert!(build_draft("2033-08", &orphan, Utc::now(), &operator).is_none());

    // 制单人未知 / 无金额：文案不能出现「（）」这种空括号
    let mut unknown = item.clone();
    unknown.created_by_name = None;
    unknown.amount = None;
    let text = remind_content("2033-08", &unknown, Utc::now());
    assert!(text.contains("制单人未知"), "{text}");
    assert!(!text.contains("（）"), "无金额时不能留空括号：{text}");
}

/// 24 小时口径与「按类 / 按 id 挑选」的纯逻辑。
#[test]
fn pick_items_filters_by_kind_and_ids() {
    let item = |no: &str, doc_id: Uuid| period_items::BlockingItem {
        doc_type: period_items::DOC_VOUCHER.to_string(),
        doc_id,
        doc_no: no.to_string(),
        status_label: "草稿".to_string(),
        amount: None,
        created_by_name: None,
        owner_role: None,
        owner_name: None,
        action_label: "去过账".to_string(),
        route: "/business/erp/voucher".to_string(),
        waiting_since: None,
        owner_role_id: None,
        owner_user_id: None,
    };
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    let blockers = vec![
        period::Blocker {
            kind: BLOCKER_DRAFT_VOUCHER.to_string(),
            count: Some(2),
            diff: None,
            message: "还有 2 张草稿凭证未过账".to_string(),
            link: None,
            samples: Vec::new(),
            items: vec![item("P2-V-001", first), item("P2-V-002", second)],
        },
        period::Blocker {
            kind: BLOCKER_INVARIANT.to_string(),
            count: Some(1),
            diff: None,
            message: "不变量未通过".to_string(),
            link: None,
            samples: Vec::new(),
            items: vec![item("P2-V-009", Uuid::now_v7())],
        },
    ];

    // 按类：只要草稿凭证那两条
    let picked = pick_items(&blockers, Some(BLOCKER_DRAFT_VOUCHER), &[]).expect("挑选");
    assert_eq!(picked.len(), 2);
    // 按类 + 按 id：只要第一条
    let picked =
        pick_items(&blockers, Some(BLOCKER_DRAFT_VOUCHER), &[first.to_string()]).expect("挑选");
    assert_eq!(picked.len(), 1);
    assert_eq!(picked[0].doc_no, "P2-V-001");
    // 类为空 → 全部
    let picked = pick_items(&blockers, None, &[]).expect("挑选");
    assert_eq!(picked.len(), 3);
    // 非法 id → 400
    assert_eq!(
        pick_items(&blockers, None, &["oops".to_string()])
            .expect_err("非法 UUID")
            .status,
        400
    );
}

/// 金额文案去尾零（`1234.00` → `1234`）。
#[test]
fn money_text_strips_trailing_zeros() {
    assert_eq!(money_text(Decimal::new(123400, 2)), "1234");
    assert_eq!(money_text(Decimal::new(123450, 2)), "1234.5");
    assert_eq!(money_text(Decimal::from(0)), "0");
}

// ==================== ⑥ 台账（P1-FIX-10） ====================

/// `ledger_row_of`：台账 8 列各自取哪个值（纯函数，不连库）。
///
/// 台账写错列不会报错，只会在半年后查不到数据时才发现 —— 所以列口径必须有断言钉住。
#[test]
fn ledger_row_maps_all_columns() {
    let doc_id = Uuid::now_v7();
    let notice_id = Uuid::now_v7();
    let item = period_items::BlockingItem {
        doc_type: period_items::DOC_STOCK_TRANSFER.to_string(),
        doc_id,
        doc_no: "TR-2033-11-009".to_string(),
        status_label: "已批准待发运".to_string(),
        amount: None,
        created_by_name: None,
        owner_role: Some("WAREHOUSE_MANAGER".to_string()),
        owner_name: None,
        action_label: "去发运".to_string(),
        route: "/business/erp/stock-transfer?transfer_no=TR-2033-11-009".to_string(),
        waiting_since: None,
        owner_role_id: Some(Uuid::now_v7()),
        owner_user_id: None,
    };
    // 2026-11-05T16:30:00Z = 北京时间 2026-11-06 00:30 → 业务日必须是 11-06
    let now = DateTime::parse_from_rfc3339("2026-11-05T16:30:00Z")
        .expect("时间")
        .with_timezone(&Utc);

    let period = RemindScope::period("2033-11");
    let row = ledger_row_of(&period, &item, notice_id, 3, now);
    assert_eq!(row.doc_type, "STOCK_TRANSFER");
    assert_eq!(row.doc_id, doc_id);
    assert_eq!(row.doc_no, Some("TR-2033-11-009"));
    assert_eq!(row.remind_kind, "PERIOD_REMIND");
    assert_eq!(
        row.stage, "2033-11",
        "stage 写期间码，台账里一眼看出为哪个月催的"
    );
    assert_eq!(
        row.remind_date.to_string(),
        "2026-11-06",
        "业务日按 Asia/Shanghai 折算（UTC 16:30 = 北京次日 00:30）"
    );
    assert_eq!(row.notice_id, Some(notice_id));
    assert_eq!(row.receiver_count, 3);

    let year = RemindScope::year_end(2026);
    let row = ledger_row_of(&year, &item, notice_id, 1, now);
    assert_eq!(row.remind_kind, "YEAR_END_REMIND");
    assert_eq!(row.stage, "2026");
}

/// 催办真的写台账，且**台账单独就能挡住 24h 内的重复**（通知被清理后仍然拦得住）。
#[tokio::test]
async fn remind_writes_ledger_and_ledger_alone_blocks_repeat() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(8).await;
    support::ensure_role_for(
        period_items::PERM_VOUCHER_POST,
        "P2-ROLE-POST",
        &[("p2.owner", "李四")],
    )
    .await;
    let voucher_id =
        support::seed_voucher("2033-08", "P2-V-801", "DRAFT", ("10", "10"), ("10", "10")).await;
    let period_id = period_of("2033-08").await;
    let request = RemindReq {
        blocker_kind: Some(BLOCKER_DRAFT_VOUCHER.to_string()),
        item_ids: Vec::new(),
    };

    let first = remind_period(period_id, &request, support::operator())
        .await
        .expect("首次催办");
    if first.sent == 0 {
        println!("SKIP: 该环境没有持有过账权限的角色，无法验证台账写入");
        return;
    }
    assert_eq!(first.sent, 1, "一张草稿凭证应催出一条通知：{first:?}");

    // ① 台账有记录，且 8 列都对（不是「只写了个 id」）
    let rows = db()
        .query_sql(
            "SELECT doc_type, doc_no, remind_kind, stage, remind_date::text AS remind_date, \
                    notice_id, receiver_count \
               FROM biz_reminder_log WHERE doc_id = ?"
                .to_string(),
            vec![Value::from(voucher_id)],
        )
        .await
        .expect("读台账");
    assert_eq!(
        rows.len(),
        1,
        "一次催办只该有一行台账（幂等键是 单据×类型×业务日）"
    );
    let row = &rows[0];
    assert_eq!(row.get_typed::<String>("doc_type").unwrap(), "VOUCHER");
    assert_eq!(row.get_typed::<String>("doc_no").unwrap(), "P2-V-801");
    assert_eq!(
        row.get_typed::<String>("remind_kind").unwrap(),
        LEDGER_KIND_PERIOD
    );
    assert_eq!(row.get_typed::<String>("stage").unwrap(), "2033-08");
    assert_eq!(
        row.get_typed::<String>("remind_date").unwrap(),
        reminder::business_day(Utc::now()).to_string(),
        "remind_date 必须是北京时间的业务日"
    );
    let notice_id: Uuid = row.get_typed("notice_id").expect("notice_id 要回填");
    let receiver_count: i64 = row.get_typed("receiver_count").expect("receiver_count");
    assert!(
        receiver_count >= 1,
        "收件人数要记实际扇出数：{receiver_count}"
    );

    // 台账里的 notice_id 必须是真发出去的那条通知
    let stored_notice: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM sys_notice WHERE id = ? AND biz_type = 'period_remind'",
            vec![Value::from(notice_id)],
        )
        .await
        .expect("查通知");
    assert_eq!(stored_notice, 1, "台账的 notice_id 指向不存在的通知");

    // ② 把通知的时间改成 25 小时前（模拟 notice_expire_cleanup 删掉/通知过期）：
    //    通知层判据失效，只剩台账 —— 仍然要拦住重复催办
    db().exec_sql(
        "UPDATE sys_notice SET created_at = now() - interval '25 hours' WHERE id = ?".to_string(),
        vec![Value::from(notice_id)],
    )
    .await
    .expect("回拨通知时间");

    let second = remind_period(period_id, &request, support::operator())
        .await
        .expect("二次催办");
    assert_eq!(second.sent, 0, "台账在 24h 内，不许再发：{second:?}");
    assert_eq!(
        second.skipped_recent, 1,
        "拦下来要记 skipped_recent（说明是台账拦的，不是通知表）：{second:?}"
    );
}

// ==================== 夹具小工具 ====================

/// 期间编码 → id。
async fn period_of(code: &str) -> Uuid {
    db().scalar_with_params(
        "SELECT id FROM biz_accounting_period WHERE period_code = ?",
        vec![Value::Text(code.to_string())],
    )
    .await
    .expect("查期间 id")
}

/// 保留：把 `PeriodRes.id`（字符串）转 uuid（部分用例用得到）。
#[allow(dead_code)]
fn period_id_of(raw: &str) -> Uuid {
    uuid_of(raw)
}

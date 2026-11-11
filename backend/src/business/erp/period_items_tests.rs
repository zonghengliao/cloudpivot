//! [`super`]（`period_items`）的真库用例 —— PERIOD-CLOSE 的明细口径。
//!
//! 跑法（缺 `ERP_DB_URL` 时全部 SKIP，不假绿）：
//!
//! ```bash
//! export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test'
//! cargo test --lib business::erp::period_items -- --test-threads=1
//! ```

use super::*;
use crate::business::erp::period::{
    self, BLOCKER_COST_UNSETTLED, BLOCKER_DOC_UNFINISHED, BLOCKER_DRAFT_VOUCHER, BLOCKER_INVARIANT, BLOCKER_UNBALANCED,
};
use crate::business::erp::period_test_support as support;

// ==================== ① 草稿凭证明细 ====================

/// 3 张草稿凭证 → `items` 长度 3，每项 `doc_no / owner_role / route` 都有值。
#[tokio::test]
async fn items_draft_vouchers_list_every_doc() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    // 期次按用例唯一（2040-01 归本用例独占）：下面「读这个期间的草稿条数」才不会被
    // 别的用例种进来的凭证污染（改造规范 R1；见 period_test_support 头注释）。
    let period = support::open_case_period(1).await.period_code;
    for no in ["P2-V-001", "P2-V-002", "P2-V-003"] {
        support::seed_voucher(&period, no, "DRAFT", ("100", "100"), ("100", "100")).await;
    }

    // 隔离库里可能没有任何角色持有「过账」权限（只有 SUPER_ADMIN 有授权行，且它不含该码）：
    // 那就按「管理员在权限管理页勾选」的方式补一个夹具角色 —— 测的是反查逻辑，
    // 不是这个库的授权现状。
    let mut owners = OwnerResolver::new();
    let mut expected = owners.resolve(db(), PERM_VOUCHER_POST).await.expect("反查");
    if expected.is_none() {
        let menu = support::menu_id_of(PERM_VOUCHER_POST)
            .await
            .expect("权限节点必须存在（V0150 种子）");
        let role = support::seed_role("P2-ROLE-POST").await;
        support::grant(role, menu).await;
        owners = OwnerResolver::new();
        expected = owners.resolve(db(), PERM_VOUCHER_POST).await.expect("反查");
    }
    let expected_role = expected.expect("补上授权后必须能反查到持有过账权限的角色").role_code;

    let (count, items) = draft_voucher_items(db(), &period, &mut owners)
        .await
        .expect("取草稿凭证明细");
    assert_eq!(count, 3, "条数必须点名 3 张");
    assert_eq!(items.len(), 3, "明细必须一张一行：{items:#?}");

    for item in &items {
        assert_eq!(item.doc_type, DOC_VOUCHER);
        assert!(item.doc_no.starts_with("P2-V-"), "单号：{}", item.doc_no);
        assert_eq!(item.status_label, "草稿（未过账）");
        assert_eq!(item.action_label, "去过账");
        assert_eq!(item.amount, Some(Decimal::new(100, 0)), "金额取单头借方");
        assert!(
            item.route.starts_with("/business/erp/voucher?voucher_no=P2-V-"),
            "路由必须带凭证号直达：{}",
            item.route
        );
        assert_eq!(
            item.owner_role.as_deref(),
            Some(expected_role.as_str()),
            "草稿凭证的处理角色 = 持有 {PERM_VOUCHER_POST} 的角色"
        );
        assert_eq!(
            item.owner_role_id.is_some(),
            item.owner_role.is_some(),
            "内部角色 id 与对外的角色码必须同步（催办按 id 扇出）"
        );
        assert!(item.waiting_since.is_some(), "要有「已等多久」的时间基准");
    }
}

/// 超过 50 张 → 明细只返回 50 条，`message` 含「仅显示前 50」。
#[tokio::test]
async fn items_are_capped_at_fifty_with_message_note() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(2).await;
    let period = row.period_code.clone();
    for index in 0..52 {
        support::seed_voucher(
            &period,
            &format!("P2-V-{index:03}"),
            "DRAFT",
            ("10", "10"),
            ("10", "10"),
        )
        .await;
    }

    let check = period::close_check(row.id).await.expect("预检");
    let blocker = check
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DRAFT_VOUCHER)
        .expect("应有草稿凭证阻塞项");
    assert_eq!(blocker.count, Some(52), "条数照实报 52");
    assert_eq!(blocker.items.len(), 50, "明细上限 50 条");
    assert!(
        blocker.message.contains("仅显示前 50 张"),
        "超限必须在文案里说明：{}",
        blocker.message
    );
    assert!(
        blocker.message.contains("共 52 张"),
        "超限文案要点名总数：{}",
        blocker.message
    );
}

// ==================== ② 未完成单据明细 ====================

/// 采购单 / 收货单 / 调拨单各一张 → 明细逐类给出**不同的动作**与**不同的处理角色**。
#[tokio::test]
async fn items_unfinished_docs_cover_three_types_with_actions() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(3).await;
    let period = row.period_code.clone();
    support::seed_purchase_order(&period, "P2-PO-001", "SUBMITTED").await;
    support::seed_stock_in(&period, "P2-IN-001", "DRAFT").await;
    let Some(_) = support::seed_stock_transfer(&period, "P2-TR-001", "IN_TRANSIT").await else {
        println!("SKIP: 隔离库没有两个仓库，调拨单明细无法造");
        return;
    };

    let expected_po_role = expected_role_of(PERM_PO_STATUS).await;
    let mut owners = OwnerResolver::new();
    let (counts, items) = unfinished_doc_items(db(), &row, &mut owners)
        .await
        .expect("取未完成单据明细");

    let total: i64 = counts.iter().map(|(_, count)| *count).sum();
    assert_eq!(total, 3, "三类各一张：{counts:?}");
    assert_eq!(items.len(), 3);

    let po = items
        .iter()
        .find(|item| item.doc_type == DOC_PURCHASE_ORDER)
        .expect("要有采购单明细");
    assert_eq!(po.action_label, "去审批");
    assert_eq!(po.status_label, "已提交待审批");
    assert!(
        po.route.starts_with("/business/erp/purchase-order?po_id="),
        "采购单路由要带 id：{}",
        po.route
    );
    assert_eq!(po.owner_role, expected_po_role);

    let si = items
        .iter()
        .find(|item| item.doc_type == DOC_STOCK_IN)
        .expect("要有收货单明细");
    assert_eq!(si.action_label, "去过账");
    assert!(si.route.starts_with("/business/erp/stock-in?in_id="));

    let tr = items
        .iter()
        .find(|item| item.doc_type == DOC_STOCK_TRANSFER)
        .expect("要有调拨单明细");
    assert_eq!(tr.action_label, "去收货");
    assert_eq!(tr.status_label, "已发运待收货");

    // 结账预检里也能看到这三张（条数与明细同源）
    let check = period::close_check(row.id).await.expect("预检");
    let blocker = check
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
        .expect("应有未完成单据阻塞项");
    assert_eq!(blocker.count, Some(3));
    assert_eq!(blocker.items.len(), 3, "明细与条数必须一致");
}

/// 三类合计超过 50 张 → 明细**合并后**仍只返回 50 条（不能 3 × 50 = 150 行），
/// 且 `message` 写明总数与「仅显示前 50 张」。
#[tokio::test]
async fn unfinished_items_are_capped_at_fifty_across_types() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(4).await;
    let period = row.period_code.clone();
    // 采购单 30 + 收货单 30 = 60 张（都不用到调拨单，避免夹具跳过）
    for index in 0..30 {
        support::seed_purchase_order(&period, &format!("P2-PO-{index:03}"), "SUBMITTED").await;
        support::seed_stock_in(&period, &format!("P2-IN-{index:03}"), "DRAFT").await;
    }

    let check = period::close_check(row.id).await.expect("预检");
    let blocker = check
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
        .expect("应有未完成单据阻塞项");
    assert_eq!(blocker.count, Some(60), "条数是全量 COUNT(*)，照实报 60");
    assert_eq!(blocker.items.len(), 50, "三类合一个 blocker，明细上限仍必须是 50 条");
    assert!(
        blocker.message.contains("仅显示前 50 张"),
        "超限必须在文案里说明：{}",
        blocker.message
    );
    // 截断前按「等得最久」排序，截断后不能出现空的单号
    assert!(blocker.items.iter().all(|item| !item.doc_no.is_empty()));
}

// ==================== ③ 处理人反查 ====================

/// owner_role 反查：要求某权限的角色被返回，且该角色**真的**持有这个权限（缓存命中一致）。
#[tokio::test]
async fn owner_role_is_resolved_from_permission() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    // 造一个「只有我们能确定」的权限码 + 角色，避免依赖隔离库的授权现状
    let code = "business:erp:p2-resolve:post";
    let menu = support::seed_menu(code).await;
    let role = support::seed_role("P2-ROLE-RESOLVE").await;
    support::grant(role, menu).await;

    let mut owners = OwnerResolver::new();
    let owner = owners
        .resolve(db(), code)
        .await
        .expect("反查不该报错")
        .expect("授权后必须能反查到角色");
    assert_eq!(owner.role_id, role, "反查必须命中被授权的那个角色");
    assert_eq!(owner.role_code, "P2-ROLE-RESOLVE");

    let held: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM sys_role_menu rm JOIN sys_menu m ON m.id = rm.menu_id \
              WHERE rm.role_id = ?::uuid AND m.auth_code = ? AND m.deleted_at IS NULL",
            vec![Value::from(&owner.role_id), Value::Text(code.to_string())],
        )
        .await
        .expect("查角色权限");
    assert_eq!(held, 1, "反查出来的角色必须真的持有该权限码");

    // 缓存：同一个权限码第二次不再查库（结果一致即证明口径没跑偏）
    let again = owners
        .resolve(db(), code)
        .await
        .expect("缓存命中")
        .expect("缓存结果不该变空");
    assert_eq!(again, owner);

    // 撤掉授权 → 反查必须回到 None（不缓存旧的错误结论：新的反查器重新查库）
    support::sql(&format!(
        "DELETE FROM sys_role_menu WHERE role_id = '{role}' AND menu_id = '{menu}'"
    ))
    .await;
    let mut fresh = OwnerResolver::new();
    assert!(
        fresh.resolve(db(), code).await.expect("反查").is_none(),
        "没有任何角色持有该权限时必须返回 None"
    );
}

/// `owner_name` 只在「该角色恰好一个启用用户」时填（多人时不猜是谁）。
#[tokio::test]
async fn owner_name_only_when_role_has_single_user() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let code = "business:erp:p2-single:post";
    let menu = support::seed_menu(code).await;
    let role = support::seed_role("P2-ROLE-SINGLE").await;
    support::grant(role, menu).await;

    // ① 角色下没有启用用户 → 只给角色，不给具体人
    let mut owners = OwnerResolver::new();
    let nobody = owners.resolve(db(), code).await.expect("反查").expect("有角色");
    assert!(nobody.user_id.is_none(), "没人时不能编出处理人：{nobody:?}");
    assert!(nobody.user_name.is_none());

    // ② 恰好一个启用用户 → 填上名字
    let first = support::seed_user("p2.one", "李四").await;
    support::assign(first, role).await;
    let mut owners = OwnerResolver::new();
    let single = owners.resolve(db(), code).await.expect("反查").expect("有角色");
    assert_eq!(single.user_id, Some(first), "唯一用户就是处理人");
    assert_eq!(single.user_name.as_deref(), Some("李四"));

    // ③ 第二个用户 → 回到「只给角色」（五选一是猜，不干）
    let second = support::seed_user("p2.two", "王五").await;
    support::assign(second, role).await;
    let mut owners = OwnerResolver::new();
    let many = owners.resolve(db(), code).await.expect("反查").expect("有角色");
    assert!(many.user_id.is_none(), "多人时不指定具体人：{many:?}");
    assert!(many.user_name.is_none());
}

/// 定位不到 owner → `owner_role = None`（不瞎猜）。
#[tokio::test]
async fn owner_role_is_null_when_no_role_holds_permission() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let mut owners = OwnerResolver::new();
    let none = owners
        .resolve(db(), "business:erp:period:not-a-real-code")
        .await
        .expect("反查不该报错");
    assert!(none.is_none(), "不存在的权限码必须返回 None，不能随便找个角色顶上");

    // 空权限码（不变量类明细用空串表示「系统不指定人」）同样返回 None
    assert!(owners.resolve(db(), "").await.expect("空码").is_none());

    // 不变量明细的处理人必须是 None（账被改坏了怎么修由财务主管判断）
    let period = support::open_case_period(5).await.period_code;
    support::seed_voucher(&period, "P2-V-901", "POSTED", ("100", "100"), ("60", "40")).await;
    let (_, items) = invariant_details(db(), &period, &mut owners)
        .await
        .expect("取不变量明细");
    assert!(!items.is_empty(), "分录不平必须给出明细");
    for item in &items {
        assert!(item.owner_role.is_none(), "不变量明细不指定处理人：{item:?}");
        assert!(item.owner_user_id.is_none());
        assert_eq!(item.action_label, "去看凭证");
    }
}

// ==================== ④ 不变量与 warnings ====================

/// 分录不平 → INVARIANT 阻塞项带明细（单号 / 去看凭证），且 UNBALANCED 照旧拦着。
#[tokio::test]
async fn items_invariant_blocker_points_to_broken_vouchers() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(6).await;
    let period = row.period_code.clone();
    support::seed_voucher(&period, "P2-V-901", "POSTED", ("100", "100"), ("60", "40")).await;

    let check = period::close_check(row.id).await.expect("预检");
    let invariant = check
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_INVARIANT)
        .expect("应有不变量阻塞项");
    assert!(
        check.blockers.iter().any(|item| item.kind == BLOCKER_UNBALANCED),
        "分录 60/40 同时应触发试算不平：{:?}",
        check.blockers.iter().map(|item| &item.kind).collect::<Vec<_>>()
    );
    assert_eq!(invariant.items.len(), 1, "一张坏凭证一条明细（不重复列）");
    assert_eq!(invariant.items[0].doc_no, "P2-V-901");
    assert!(
        invariant.items[0].status_label.contains("不平") || invariant.items[0].status_label.contains("不一致"),
        "状态标签要说明坏在哪：{}",
        invariant.items[0].status_label
    );
}

/// 每条 blocker 都带 `items` 字段（序列化后必有该键），warnings 与 blockers 口径一致。
#[tokio::test]
async fn every_blocker_serializes_items_array() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(7).await;
    let period = row.period_code.clone();
    support::seed_voucher(&period, "P2-V-001", "DRAFT", ("10", "10"), ("10", "10")).await;

    let check = period::close_check(row.id).await.expect("预检");
    let json = serde_json::to_value(&check).expect("预检结果必须可序列化");
    let blockers = json["blockers"].as_array().expect("blockers 是数组");
    assert!(!blockers.is_empty());
    for blocker in blockers {
        assert!(
            blocker.get("items").is_some(),
            "每个 blocker 都要有 items 键（前端按它决定表格 / 老样式）：{blocker}"
        );
        assert!(blocker["items"].is_array());
    }
    for warning in json["warnings"].as_array().expect("warnings 是数组") {
        assert!(
            warning.get("items").is_some(),
            "warnings 与 blockers 同结构（口径一致）：{warning}"
        );
    }
}

/// 没有挡路单据时明细为空，接口不报错（backward compat）。
#[tokio::test]
async fn empty_period_has_no_items_and_no_error() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(8).await;
    let check = period::close_check(row.id).await.expect("空期间预检不该报错");
    assert!(check.can_close, "空期间应可结账：{:?}", check.blockers);
    assert!(check.blockers.is_empty());

    let json = serde_json::to_value(&check).expect("可序列化");
    assert_eq!(json["blockers"].as_array().map(Vec::len), Some(0));
}

/// `items` 不影响 `can_close()` 判定（判定只看 blockers 是否为空）。
#[tokio::test]
async fn can_close_is_unaffected_by_items() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(9).await;
    let period = row.period_code.clone();
    let voucher_id = support::seed_voucher(&period, "P2-V-001", "DRAFT", ("10", "10"), ("10", "10")).await;

    let blocked = period::close_check(row.id).await.expect("预检");
    assert!(!blocked.can_close);
    assert!(!blocked.blockers[0].items.is_empty(), "明细非空");

    // 把挡路单据「处理掉」（DRAFT → POSTED）→ 同一份判定逻辑必须放行。
    // **只动本用例自己造的那张凭证**（按 id）：过去这里是「按 2033-10 整期 UPDATE」，
    // 并行跑时会把别的用例刚种进同一期间的草稿凭证一并过账，
    // 它们的「必须有草稿挡路」断言就随机红。
    support::set_voucher_status(voucher_id, "POSTED").await;
    let cleared = period::close_check(row.id).await.expect("预检");
    assert!(cleared.can_close, "处理完单据后必须可结账：{:?}", cleared.blockers);
    let reloaded = period::load_period_row(db(), row.id)
        .await
        .expect("读期间")
        .expect("期间存在");
    assert_eq!(reloaded.status, period::STATUS_OPEN, "预检只读，不该改期间状态");
}

// ==================== ⑤ 纯逻辑（不连库） ====================

/// 下一动作对照表（改这张表 = 改「该谁处理」，必须有单测兜着）。
#[test]
fn next_action_table_is_stable() {
    assert_eq!(voucher_next("DRAFT").action_label, "去过账");
    assert_eq!(voucher_next("DRAFT").permission, PERM_VOUCHER_POST);

    assert_eq!(purchase_order_next("DRAFT").action_label, "去提交");
    assert_eq!(purchase_order_next("SUBMITTED").action_label, "去审批");
    assert_eq!(purchase_order_next("APPROVED").action_label, "去收货");
    assert_eq!(purchase_order_next("PARTIAL_RECEIVED").action_label, "去收货");

    assert_eq!(stock_in_next("DRAFT").action_label, "去过账");
    assert_eq!(stock_in_next("PENDING_POST").action_label, "去过账");
    assert_eq!(stock_in_next("PENDING_CANCEL").action_label, "去处理");

    assert_eq!(stock_transfer_next("DRAFT").action_label, "去提交");
    assert_eq!(stock_transfer_next("APPROVED").action_label, "去发运");
    assert_eq!(stock_transfer_next("IN_TRANSIT").action_label, "去收货");

    // 未知状态：不猜权限（空串 → 反查必然 None → 前端显示「待分配」）
    assert!(next_action(DOC_VOUCHER, "WHAT").permission.is_empty());
    assert_eq!(doc_type_label(DOC_STOCK_IN), "收货单");
}

/// 路由与 URL 转义：单号里的空格 / 中文不能在 query 里裸奔。
#[test]
fn route_encoding_is_safe() {
    let id = Uuid::nil();
    assert_eq!(
        route_of(DOC_VOUCHER, &id, "PZ-2033-08-001"),
        "/business/erp/voucher?voucher_no=PZ-2033-08-001"
    );
    assert_eq!(
        route_of(DOC_STOCK_IN, &id, "IN-1"),
        format!("/business/erp/stock-in?in_id={id}")
    );
    assert_eq!(url_encode("A B/中"), "A%20B%2F%E4%B8%AD");
}

// ==================== ⑤ 成本核算未结算（COST_UNSETTLED） ====================

/// 期间内有未结算的成本批次 → 结账被拦，文案点名期间 / 批次 / 成本对象，且
/// **原有 4 项 blocker 一条不少**（卡面验收第三条）。
///
/// 判据来源：设计 §4.11「该期间存在 biz_cost_period 且 status NOT IN
/// ('SETTLED','CLOSED','CANCELLED','REVERSED')」。
#[tokio::test]
async fn cost_period_unsettled_blocks_close_and_names_objects() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    let row = support::open_case_period(10).await;
    let period = row.period_code.clone();
    // 同期只能有一个**活跃**批次（uk_biz_cost_period）→ 造一个 DRAFT
    let batch_no = "CS-204010-0001";
    let period_id = seed_cost_batch(&period, batch_no, "DRAFT").await;
    let product = Uuid::nil();
    let object_a = seed_cost_object("P2I-CB-204010-001", product).await;
    let object_b = seed_cost_object("P2I-CB-204010-002", product).await;
    seed_cost_item(period_id, object_a, product).await;
    seed_cost_item(period_id, object_b, product).await;
    // 同期间再种一张草稿凭证：本卡新增第 5 项**不许**影响原有 4 项
    support::seed_voucher(&period, "P2-V-901", "DRAFT", ("10", "10"), ("10", "10")).await;

    let check = period::close_check(row.id).await.expect("预检");
    let kinds = check
        .blockers
        .iter()
        .map(|blocker| blocker.kind.as_str())
        .collect::<Vec<_>>();
    assert!(
        kinds.contains(&BLOCKER_DRAFT_VOUCHER),
        "① 草稿凭证必须照旧拦：{kinds:?}"
    );
    assert!(
        kinds.contains(&BLOCKER_COST_UNSETTLED),
        "⑤ 未结算成本批次必须拦：{kinds:?}"
    );
    // 实测落日志（证据）：同一次预检里 ① 草稿凭证与 ⑤ 成本未结算**同时**出现
    println!("BLOCKERS(kinds) => {kinds:?}");
    for blocker in &check.blockers {
        println!("  - {} :: {}", blocker.kind, blocker.message);
    }
    assert!(!check.can_close, "有未结算批次就不许结账");

    let blocker = check
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_COST_UNSETTLED)
        .expect("应有 COST_UNSETTLED 阻塞项");
    // 实测文案落进 `--nocapture` 日志（证据：卡面验收要「说清哪个期间、哪个成本对象」）
    println!("BLOCKER(kind={}) => {}", blocker.kind, blocker.message);
    assert_eq!(blocker.count, Some(1), "一个批次报 1");
    // 文案：期间 + 批次 + 状态 + 对象（卡面验收「说清哪个期间、哪个成本对象」）
    assert!(
        blocker.message.contains(&period),
        "文案要点名期间 {period}：{}",
        blocker.message
    );
    assert!(
        blocker.message.contains(batch_no),
        "文案要点名批次：{}",
        blocker.message
    );
    assert!(
        blocker.message.contains("草稿（未提交）"),
        "状态要中文：{}",
        blocker.message
    );
    assert!(
        blocker.message.contains("P2I-CB-204010-001") && blocker.message.contains("P2I-CB-204010-002"),
        "文案要点名成本对象：{}",
        blocker.message
    );
    assert_eq!(blocker.samples, vec![batch_no.to_string()], "定位线索 = 批次号");

    // 明细口径：一条批次一行，能直接跳到成本核算页
    assert_eq!(blocker.items.len(), 1, "一个批次一条明细");
    let item = &blocker.items[0];
    assert_eq!(item.doc_type, DOC_COST_PERIOD);
    assert_eq!(item.doc_id, period_id);
    assert_eq!(item.doc_no, batch_no);
    assert_eq!(item.status_label, "草稿（未提交）");
    assert_eq!(item.action_label, "去提交");
    assert_eq!(item.amount, None, "未结算批次累计投入落库是 0，显示留空");
    assert_eq!(item.route, format!("/business/erp/cost-period?batch_no={batch_no}"));
    assert!(item.waiting_since.is_some(), "要有「已等多久」");
    assert!(item.owner_role.is_none(), "系统不指定处理人（见 cost_batch_next）");
    let expected_link = format!("/business/erp/cost-period?period={period}");
    assert_eq!(blocker.link.as_deref(), Some(expected_link.as_str()));

    // 真拦（不只是预检显示）：结账 409 且文案里有这一项
    let err = period::close_period(row.id, None, support::operator())
        .await
        .expect_err("有未结算成本批次时结账必须 409");
    assert_eq!(err.status, 409, "{} / {}", err.code, err.msg);
    assert!(err.msg.contains("成本核算未结算"), "{}", err.msg);
    assert_eq!(
        status_of_period(&period).await.as_deref(),
        Some("OPEN"),
        "被拦之后期间必须还是 OPEN（不许改状态）"
    );

    support::cleanup().await;
}

/// 已结算 / 已关闭 / 已作废 / 已红冲的批次**都不拦**；期间里没有任何批次也不拦
/// （未启用成本核算的存量客户不受影响）。
#[tokio::test]
async fn cost_period_settled_terminal_statuses_never_block() {
    let Some(_guard) = support::setup().await else {
        return;
    };

    // ① 已作废批次（cancel_reason 必填）
    let cancelled = support::open_case_period(11).await;
    seed_cost_batch(&cancelled.period_code, "CS-204011-0001", "CANCELLED").await;
    let check = period::close_check(cancelled.id).await.expect("预检");
    assert!(
        check
            .blockers
            .iter()
            .all(|blocker| blocker.kind != BLOCKER_COST_UNSETTLED),
        "已作废批次不许挡结账：{:?}",
        check.blockers
    );
    assert!(check.can_close, "只有作废批次时应能结账：{:?}", check.blockers);

    // ② 已结算批次（settled_at + finish_voucher_id 必填）+ 已红冲批次放在同一期间：
    //    唯一键 `uk_biz_cost_period` 排除了 REVERSED / CANCELLED，所以三态可共存。
    seed_cost_batch(&cancelled.period_code, "CS-204011-0002", "SETTLED").await;
    seed_cost_batch(&cancelled.period_code, "CS-204011-0003", "REVERSED").await;
    let (count, batches) = cost_period_items(db(), &cancelled.period_code).await.expect("取明细");
    assert_eq!(
        count,
        0,
        "已作废 / 已结算 / 已红冲都不算未结算（库里 {} 行批次）",
        batches.len()
    );
    assert!(batches.is_empty(), "不许把算完的批次列出来：{batches:?}");
    let check = period::close_check(cancelled.id).await.expect("预检");
    assert!(
        check
            .blockers
            .iter()
            .all(|blocker| blocker.kind != BLOCKER_COST_UNSETTLED),
        "已结算 / 已红冲批次不许挡结账：{:?}",
        check.blockers
    );
    assert!(check.can_close, "算完的批次应能结账：{:?}", check.blockers);

    // ③ 期间里一行批次都没有 → 第 5 项连出现都不出现（`batches` 空 ⇒ 不生成 blocker）
    let empty = support::open_case_period(12).await;
    let (count, batches) = cost_period_items(db(), &empty.period_code).await.expect("取明细");
    assert_eq!(count, 0, "没有批次时条数是 0");
    assert!(batches.is_empty());
    assert!(
        period::cost_unsettled_blocker(&empty.period_code, count, &batches).is_none(),
        "没有批次不许造出 blocker"
    );
    let check = period::close_check(empty.id).await.expect("预检");
    assert!(
        check
            .blockers
            .iter()
            .all(|blocker| blocker.kind != BLOCKER_COST_UNSETTLED),
        "没有成本批次的期间不许被第 5 项拦住：{:?}",
        check.blockers
    );

    support::cleanup().await;
}

/// 成本批次明细的字段口径（纯函数部分在这里也钉一遍：`doc_type / 状态标签 / 路由`）。
#[test]
fn cost_batch_item_fields_are_stable() {
    let batch = CostBatch {
        id: Uuid::nil(),
        batch_no: "CS-204009-0007".to_string(),
        status: "SUBMITTED".to_string(),
        created_at: DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("时间戳"),
        creator: Some("成本会计".to_string()),
        object_nos: vec!["CB-1".to_string()],
    };
    let item = cost_batch_item(&batch);
    assert_eq!(item.doc_type, DOC_COST_PERIOD);
    assert_eq!(item.status_label, "已提交待审批");
    assert_eq!(item.action_label, "去审批");
    assert_eq!(item.created_by_name.as_deref(), Some("成本会计"));
    assert_eq!(item.route, "/business/erp/cost-period?batch_no=CS-204009-0007");
    assert_eq!(doc_type_label(DOC_COST_PERIOD), "成本批次");
    assert_eq!(next_action(DOC_COST_PERIOD, "APPROVED").action_label, "去结算");
    // 三个「未算完」状态各有中文标签与动作；四个「算完」状态也都有标签（不回落英文码）
    for status in [
        "DRAFT",
        "SUBMITTED",
        "APPROVED",
        "SETTLED",
        "CLOSED",
        "CANCELLED",
        "REVERSED",
    ] {
        assert_ne!(cost_batch_status_label(status), "状态未知", "{status} 缺中文标签");
    }
}

// ==================== 夹具小工具 ====================

/// 造一个成本核算批次（`biz_cost_period`）。
///
/// 状态补列（库层 4 条状态 CHECK 逼着给）：CANCELLED→cancel_reason；
/// SETTLED→settled_at + finish_voucher_id；REVERSED→再加 approval_instance_id /
/// reversed_at / reverse_reason。夹具只造这 4 态，别的状态直接 panic（不要静默造出错数据）。
async fn seed_cost_batch(period_code: &str, batch_no: &str, status: &str) -> Uuid {
    let (cancel, settled_at, finish_voucher, approval, reversed_at, reverse_reason) = match status {
        "DRAFT" => ("", "", "", "", "", ""),
        "CANCELLED" => ("P2I 夹具：作废", "", "", "", "", ""),
        "SETTLED" => (
            "",
            "2026-11-11 00:00:00+00",
            "00000000-0000-7000-8000-000000005e77",
            "00000000-0000-7000-8000-000000005e78",
            "",
            "",
        ),
        "REVERSED" => (
            "",
            "2026-11-11 00:00:00+00",
            "00000000-0000-7000-8000-000000005e77",
            "00000000-0000-7000-8000-000000005e78",
            "2026-11-12 00:00:00+00",
            "P2I 夹具：红冲",
        ),
        other => panic!("夹具不支持的成本批次状态：{other}"),
    };
    let rows = db()
        .query_sql(
            "INSERT INTO biz_cost_period \
                (period_code, batch_no, status, cancel_reason, settled_at, finish_voucher_id, \
                 approval_instance_id, reversed_at, reverse_reason, create_by) \
             VALUES (?, ?, ?, NULLIF(?::text, ''), NULLIF(?::text, '')::timestamptz, \
                     NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid, \
                     NULLIF(?::text, '')::timestamptz, NULLIF(?::text, ''), ?) \
             RETURNING id"
                .to_string(),
            vec![
                Value::Text(period_code.to_string()),
                Value::Text(batch_no.to_string()),
                Value::Text(status.to_string()),
                Value::Text(cancel.to_string()),
                Value::Text(settled_at.to_string()),
                Value::Text(finish_voucher.to_string()),
                Value::Text(approval.to_string()),
                Value::Text(reversed_at.to_string()),
                Value::Text(reverse_reason.to_string()),
                Value::from(&support::uid()),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("造成本批次失败（{batch_no}）：{error}"));
    rows[0].get_typed::<Uuid>("id").expect("批次 id")
}

/// 造一个成本对象（`biz_cost_object`；产品 / 仓库 / 车间都是弱引用，随便给 uuid）。
async fn seed_cost_object(object_no: &str, product_id: Uuid) -> Uuid {
    let rows = db()
        .query_sql(
            "INSERT INTO biz_cost_object \
                (object_no, product_id, target_warehouse_id, plan_qty, workshop_dept_id, \
                 workshop_dept_name, start_date) \
             VALUES (?, ?, ?, 10, ?, 'P2I 夹具车间', '2026-01-01') RETURNING id"
                .to_string(),
            vec![
                Value::Text(object_no.to_string()),
                Value::from(&product_id),
                Value::from(&Uuid::nil()),
                Value::from(&Uuid::nil()),
            ],
        )
        .await
        .unwrap_or_else(|error| panic!("造成本对象失败（{object_no}）：{error}"));
    rows[0].get_typed::<Uuid>("id").expect("对象 id")
}

/// 造一条成本计算单（`biz_cost_period_item`，数量金额全 0 —— 满足库层守恒 CHECK）。
async fn seed_cost_item(period_id: Uuid, object_id: Uuid, product_id: Uuid) {
    support::sql(&format!(
        "INSERT INTO biz_cost_period_item (period_id, cost_object_id, product_id) \
         VALUES ('{period_id}', '{object_id}', '{product_id}')"
    ))
    .await;
}

/// 读期间状态（结账被拦后必须还是 OPEN）。
async fn status_of_period(period_code: &str) -> Option<String> {
    let rows = db()
        .query_sql(
            "SELECT status FROM biz_accounting_period WHERE period_code = ?".to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await
        .expect("读期间状态");
    rows.first().and_then(|row| row.get_typed::<String>("status").ok())
}

/// 隔离库里持有某权限码的角色码（反查是确定性的：`sort_order` + `role_code` 取第一个）。
async fn expected_role_of(permission: &str) -> Option<String> {
    let mut owners = OwnerResolver::new();
    owners
        .resolve(db(), permission)
        .await
        .expect("反查")
        .map(|owner| owner.role_code)
}

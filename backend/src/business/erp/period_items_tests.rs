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
    self, BLOCKER_DOC_UNFINISHED, BLOCKER_DRAFT_VOUCHER, BLOCKER_INVARIANT, BLOCKER_UNBALANCED,
};
use crate::business::erp::period_test_support as support;

// ==================== ① 草稿凭证明细 ====================

/// 3 张草稿凭证 → `items` 长度 3，每项 `doc_no / owner_role / route` 都有值。
#[tokio::test]
async fn items_draft_vouchers_list_every_doc() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_test_period(3).await;
    for no in ["P2-V-001", "P2-V-002", "P2-V-003"] {
        support::seed_voucher("2033-03", no, "DRAFT", ("100", "100"), ("100", "100")).await;
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
    let expected_role = expected
        .expect("补上授权后必须能反查到持有过账权限的角色")
        .role_code;

    let (count, items) = draft_voucher_items(db(), "2033-03", &mut owners)
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
            item.route
                .starts_with("/business/erp/voucher?voucher_no=P2-V-"),
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
    let row = support::open_test_period(4).await;
    for index in 0..52 {
        support::seed_voucher(
            "2033-04",
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
    let row = support::open_test_period(5).await;
    support::seed_purchase_order("2033-05", "P2-PO-001", "SUBMITTED").await;
    support::seed_stock_in("2033-05", "P2-IN-001", "DRAFT").await;
    let Some(_) = support::seed_stock_transfer("2033-05", "P2-TR-001", "IN_TRANSIT").await else {
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
    let row = support::open_test_period(6).await;
    // 采购单 30 + 收货单 30 = 60 张（都不用到调拨单，避免夹具跳过）
    for index in 0..30 {
        support::seed_purchase_order("2033-06", &format!("P2-PO-{index:03}"), "SUBMITTED").await;
        support::seed_stock_in("2033-06", &format!("P2-IN-{index:03}"), "DRAFT").await;
    }

    let check = period::close_check(row.id).await.expect("预检");
    let blocker = check
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
        .expect("应有未完成单据阻塞项");
    assert_eq!(blocker.count, Some(60), "条数是全量 COUNT(*)，照实报 60");
    assert_eq!(
        blocker.items.len(),
        50,
        "三类合一个 blocker，明细上限仍必须是 50 条"
    );
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
    let nobody = owners
        .resolve(db(), code)
        .await
        .expect("反查")
        .expect("有角色");
    assert!(nobody.user_id.is_none(), "没人时不能编出处理人：{nobody:?}");
    assert!(nobody.user_name.is_none());

    // ② 恰好一个启用用户 → 填上名字
    let first = support::seed_user("p2.one", "李四").await;
    support::assign(first, role).await;
    let mut owners = OwnerResolver::new();
    let single = owners
        .resolve(db(), code)
        .await
        .expect("反查")
        .expect("有角色");
    assert_eq!(single.user_id, Some(first), "唯一用户就是处理人");
    assert_eq!(single.user_name.as_deref(), Some("李四"));

    // ③ 第二个用户 → 回到「只给角色」（五选一是猜，不干）
    let second = support::seed_user("p2.two", "王五").await;
    support::assign(second, role).await;
    let mut owners = OwnerResolver::new();
    let many = owners
        .resolve(db(), code)
        .await
        .expect("反查")
        .expect("有角色");
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
    assert!(
        none.is_none(),
        "不存在的权限码必须返回 None，不能随便找个角色顶上"
    );

    // 空权限码（不变量类明细用空串表示「系统不指定人」）同样返回 None
    assert!(owners.resolve(db(), "").await.expect("空码").is_none());

    // 不变量明细的处理人必须是 None（账被改坏了怎么修由财务主管判断）
    support::open_test_period(6).await;
    support::seed_voucher(
        "2033-06",
        "P2-V-901",
        "POSTED",
        ("100", "100"),
        ("60", "40"),
    )
    .await;
    let (_, items) = invariant_details(db(), "2033-06", &mut owners)
        .await
        .expect("取不变量明细");
    assert!(!items.is_empty(), "分录不平必须给出明细");
    for item in &items {
        assert!(
            item.owner_role.is_none(),
            "不变量明细不指定处理人：{item:?}"
        );
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
    let row = support::open_test_period(7).await;
    support::seed_voucher(
        "2033-07",
        "P2-V-901",
        "POSTED",
        ("100", "100"),
        ("60", "40"),
    )
    .await;

    let check = period::close_check(row.id).await.expect("预检");
    let invariant = check
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_INVARIANT)
        .expect("应有不变量阻塞项");
    assert!(
        check
            .blockers
            .iter()
            .any(|item| item.kind == BLOCKER_UNBALANCED),
        "分录 60/40 同时应触发试算不平：{:?}",
        check
            .blockers
            .iter()
            .map(|item| &item.kind)
            .collect::<Vec<_>>()
    );
    assert_eq!(invariant.items.len(), 1, "一张坏凭证一条明细（不重复列）");
    assert_eq!(invariant.items[0].doc_no, "P2-V-901");
    assert!(
        invariant.items[0].status_label.contains("不平")
            || invariant.items[0].status_label.contains("不一致"),
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
    let row = support::open_test_period(8).await;
    support::seed_voucher("2033-08", "P2-V-001", "DRAFT", ("10", "10"), ("10", "10")).await;

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
    let row = support::open_test_period(9).await;
    let check = period::close_check(row.id)
        .await
        .expect("空期间预检不该报错");
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
    let row = support::open_test_period(10).await;
    support::seed_voucher("2033-10", "P2-V-001", "DRAFT", ("10", "10"), ("10", "10")).await;

    let blocked = period::close_check(row.id).await.expect("预检");
    assert!(!blocked.can_close);
    assert!(!blocked.blockers[0].items.is_empty(), "明细非空");

    // 把挡路单据「处理掉」（DRAFT → POSTED）→ 同一份判定逻辑必须放行
    support::sql("UPDATE biz_voucher SET status = 'POSTED' WHERE period = '2033-10'").await;
    let cleared = period::close_check(row.id).await.expect("预检");
    assert!(
        cleared.can_close,
        "处理完单据后必须可结账：{:?}",
        cleared.blockers
    );
    let reloaded = period::load_period_row(db(), row.id)
        .await
        .expect("读期间")
        .expect("期间存在");
    assert_eq!(
        reloaded.status,
        period::STATUS_OPEN,
        "预检只读，不该改期间状态"
    );
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
    assert_eq!(
        purchase_order_next("PARTIAL_RECEIVED").action_label,
        "去收货"
    );

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

// ==================== 夹具小工具 ====================

/// 隔离库里持有某权限码的角色码（反查是确定性的：`sort_order` + `role_code` 取第一个）。
async fn expected_role_of(permission: &str) -> Option<String> {
    let mut owners = OwnerResolver::new();
    owners
        .resolve(db(), permission)
        .await
        .expect("反查")
        .map(|owner| owner.role_code)
}

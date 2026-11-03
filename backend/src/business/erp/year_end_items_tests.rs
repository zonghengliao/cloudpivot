//! [`super`]（`year_end`）的真库用例 —— YEAR-END 的「挡路单据明细 + 处理人」口径。
//!
//! 跑法（缺 `ERP_DB_URL` 时全部 SKIP，不假绿）：
//!
//! ```bash
//! export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_year_end_clean'
//! cargo test --lib business::erp::year_end -- --test-threads=1
//! ```
//!
//! 夹具年度是 **2037 / 2038**（`year_end_test_support`），与 `year_end.rs` 自带的
//! 2021–2023 用例、`period` 的 2031–2033 互不重叠。
//!
//! # 这一版要钉住的五条验收
//!
//! ```text
//! ① 明细上限 50（不是 12 个月 × 50，也不是三类各 50）
//! ② owner 反查不到就留 null（不瞎猜）
//! ③ can_close() 判定不受 items 影响
//! ④ 空明细不报错（items 是空数组，不是缺字段）
//! ⑤ 明细与条数同源（count = 全量 COUNT(*)，items ≤ 50）
//! ```

use super::*;
use crate::business::erp::period_items::{DOC_PURCHASE_ORDER, DOC_STOCK_IN, DOC_VOUCHER};
use crate::business::erp::period_test_support as p2;
use crate::business::erp::year_end_test_support as support;

// ==================== ① 未完成单据：全年逐单明细 ====================

/// 全年的未完成单据**逐张**列出来（跨 3 个不同月份），并带上「该谁处理」与「去处理」路由。
///
/// 判据（口径照 PERIOD-CLOSE）：单号 / 类型 / 中文状态 / 动作文案 / 路由 / owner 全都有值。
#[tokio::test]
async fn items_list_every_blocking_doc_across_the_year() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_po("YE-PO-001", 2, "SUBMITTED").await;
    support::seed_po("YE-PO-002", 7, "DRAFT").await;
    support::seed_po("YE-PO-003", 11, "APPROVED").await;
    support::seed_stock_in("YE-IN-001", 5, "DRAFT").await;

    // 处理人按「下一个动作需要的权限码」反查：先按管理员在权限管理页勾选的方式补两个夹具角色
    let po_role = p2::ensure_role_for(
        "business:erp:purchase-order:status",
        "YE-ROLE-PO-APPROVE",
        &[("ye.po", "采购主管")],
    )
    .await;
    let si_role = p2::ensure_role_for(
        "business:erp:stock-in:status",
        "YE-ROLE-SI-POST",
        &[("ye.si", "仓管甲")],
    )
    .await;
    let po_role_code: String = db()
        .scalar_with_params(
            "SELECT role_code FROM sys_role WHERE id = ?::uuid",
            vec![Value::from(&po_role)],
        )
        .await
        .expect("读角色码");
    let si_role_code: String = db()
        .scalar_with_params(
            "SELECT role_code FROM sys_role WHERE id = ?::uuid",
            vec![Value::from(&si_role)],
        )
        .await
        .expect("读角色码");

    let blockers = collect_blockers(db(), support::YEAR)
        .await
        .expect("年结预检");
    let blocker = blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
        .expect("必须报 DOC_UNFINISHED");

    assert_eq!(blocker.count, Some(4), "条数是全量 COUNT(*)（4 张）");
    assert_eq!(
        blocker.items.len(),
        4,
        "明细必须一张一行：{:#?}",
        blocker.items
    );
    assert_eq!(
        blocker.samples.len(),
        3,
        "老字段 samples 仍给前 3 个单号（backward compat）"
    );

    let po1 = blocker
        .items
        .iter()
        .find(|item| item.doc_no == "YE-PO-001")
        .expect("YE-PO-001 必须在明细里");
    assert_eq!(po1.doc_type, DOC_PURCHASE_ORDER);
    assert_eq!(po1.status_label, "已提交待审批");
    assert_eq!(po1.action_label, "去审批");
    assert!(
        po1.route.starts_with("/business/erp/purchase-order?po_id="),
        "采购单路由要带 id 直达：{}",
        po1.route
    );
    assert_eq!(
        po1.owner_role.as_deref(),
        Some(po_role_code.as_str()),
        "处理角色 = 持有 purchase-order:status 的角色"
    );
    assert_eq!(
        po1.owner_name.as_deref(),
        Some("采购主管"),
        "角色下唯一用户就点名"
    );
    assert!(po1.waiting_since.is_some(), "要有「已等多久」的基准");

    let si1 = blocker
        .items
        .iter()
        .find(|item| item.doc_no == "YE-IN-001")
        .expect("YE-IN-001 必须在明细里");
    assert_eq!(si1.doc_type, DOC_STOCK_IN);
    assert_eq!(si1.action_label, "去过账");
    assert!(
        si1.route.starts_with("/business/erp/stock-in?in_id="),
        "收货单路由要带 id 直达：{}",
        si1.route
    );
    assert_eq!(si1.owner_role.as_deref(), Some(si_role_code.as_str()));

    // 已批准待收货的采购单：动作是「去收货」
    let po3 = blocker
        .items
        .iter()
        .find(|item| item.doc_no == "YE-PO-003")
        .expect("YE-PO-003 必须在明细里");
    assert_eq!(po3.action_label, "去收货");
    support::teardown().await;
}

/// 全年 60 张（12 个月 × 5 张）：`count` 照实报 60，**明细只给 50 条**，
/// 且 `message` 写明「共 60 张，仅显示前 50 张」。
///
/// 这条用例锁的是 `t_1d3a6fce` 踩过的坑：三类各取 50 → 150 行。
/// 年结多一层风险（12 个月各取 50 → 600 行），所以按月摊开造，最后仍必须 ≤ 50。
#[tokio::test]
async fn unfinished_items_are_capped_at_fifty_across_twelve_months() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    for month in 1..=12 {
        for index in 0..5 {
            support::seed_po(&format!("YE-PO-{month:02}{index:02}"), month, "SUBMITTED").await;
        }
    }

    let blockers = collect_blockers(db(), support::YEAR)
        .await
        .expect("年结预检");
    let blocker = blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
        .expect("必须报 DOC_UNFINISHED");

    assert_eq!(blocker.count, Some(60), "条数是全量，照实报 60");
    assert_eq!(
        blocker.items.len(),
        50,
        "明细上限 50 条（不能 12 个月 × 50 = 600）"
    );
    assert!(
        blocker.message.contains("共 60 张") && blocker.message.contains("仅显示前 50 张"),
        "超限文案要点名总数与上限：{}",
        blocker.message
    );
    assert!(
        blocker.items.iter().all(|item| !item.doc_no.is_empty()),
        "截断后不能出现空单号"
    );
    support::teardown().await;
}

// ==================== ② 不变量：坏凭证逐张列出 ====================

/// 分录不平的已过账凭证 → `INVARIANT` 带明细（单号 / 「去看凭证」/ 处理人留空）。
///
/// 处理人**必须是 None**：账被改坏了要怎么修（红冲？补分录？）是财务主管的判断，
/// 系统不指定人（与结账页同一个口径）。
#[tokio::test]
async fn invariant_items_point_to_broken_vouchers_without_owner() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_broken_voucher("YE-V-901", 3).await;

    let blockers = collect_blockers(db(), support::YEAR)
        .await
        .expect("年结预检");
    let invariant = blockers
        .iter()
        .find(|item| item.kind == BLOCKER_INVARIANT)
        .expect("必须报 INVARIANT");
    assert!(
        blockers.iter().any(|item| item.kind == BLOCKER_UNBALANCED),
        "60/40 的分录同时应触发整年借贷不平：{:?}",
        blockers.iter().map(|item| &item.kind).collect::<Vec<_>>()
    );
    assert_eq!(invariant.items.len(), 1, "一张坏凭证一条明细（A1/A2 去重）");
    let item = &invariant.items[0];
    assert_eq!(item.doc_no, "YE-V-901");
    assert_eq!(item.doc_type, DOC_VOUCHER);
    assert_eq!(item.action_label, "去看凭证");
    assert!(
        item.status_label.contains("不平") || item.status_label.contains("不一致"),
        "状态标签要说明坏在哪：{}",
        item.status_label
    );
    assert!(
        item.route
            .starts_with("/business/erp/voucher?voucher_no=YE-V-901"),
        "凭证路由要带单号直达：{}",
        item.route
    );
    assert!(item.owner_role.is_none(), "不变量不指定处理人：{item:?}");
    assert!(item.owner_user_id.is_none());
    support::teardown().await;
}

// ==================== ③ owner 反查不到 → null ====================

/// 没有任何启用角色持有「下一个动作」所需的权限码 → `owner_role = None`（前端显示「待分配」）。
///
/// 做法：先把该权限节点的授权行**存一份**再清空（测的是反查逻辑，不是这个库的授权现状），
/// 断言完立刻还原，不污染后续用例。
#[tokio::test]
async fn owner_is_null_when_no_role_holds_the_permission() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    let menu = support::menu_id_of("business:erp:purchase-order:status")
        .await
        .expect("权限节点必须存在（V0150 种子）");
    let saved = support::grants_of(menu).await;
    support::sql(&format!(
        "DELETE FROM sys_role_menu WHERE menu_id = '{menu}'"
    ))
    .await;

    support::seed_po("YE-PO-900", 4, "SUBMITTED").await;
    let blockers = collect_blockers(db(), support::YEAR)
        .await
        .expect("年结预检");
    let blocker = blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
        .expect("必须报 DOC_UNFINISHED");
    let item = blocker
        .items
        .iter()
        .find(|item| item.doc_no == "YE-PO-900")
        .expect("明细里要有它");

    assert!(
        item.owner_role.is_none(),
        "没有角色持有该权限时必须留 null（不随便找个角色顶上）：{item:?}"
    );
    assert!(item.owner_name.is_none());
    assert!(item.owner_role_id.is_none());
    assert!(!item.has_owner(), "没有处理人 → 前端「催办」置灰");

    support::restore_grants(&saved).await;
    support::teardown().await;
}

// ==================== ④ 结构 / 边界 ====================

/// 每条年结阻塞项都带 `items`（序列化后必有该键），空明细是空数组而不是缺字段。
///
/// 前端靠 `items.length > 0` 决定「上表格」还是「只显示一句话」，键缺失会让 `?? []` 兜底
/// —— 能跑，但会让「这一类到底有没有单据」变成猜测。这里钉死：键必须在。
#[tokio::test]
async fn every_year_end_blocker_serializes_items_array() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_po("YE-PO-001", 6, "DRAFT").await;

    let check = check_year(support::YEAR).await.expect("前置检查不该报错");
    let json = serde_json::to_value(&check).expect("年结预检必须可序列化");
    let blockers = json["blockers"].as_array().expect("blockers 是数组");
    assert!(!blockers.is_empty());
    for blocker in blockers {
        assert!(
            blocker.get("items").is_some(),
            "每个阻塞项都要有 items 键：{blocker}"
        );
        assert!(blocker["items"].is_array());
    }
    // 期间类阻塞项（没有单据级明细）必须是空数组
    let period_blocker = blockers
        .iter()
        .find(|blocker| blocker["type"] == "PERIOD_NOT_CLOSED");
    assert!(
        period_blocker.is_none(),
        "12 个月都封了，不该有「期间未结账」阻塞项"
    );
    support::teardown().await;
}

/// 空年度（没有任何挡路单据）→ `items` 全空、接口不报错、`can_close = true`。
#[tokio::test]
async fn empty_year_has_no_items_and_no_error() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;

    let check = check_year(support::YEAR).await.expect("空年度预检不该报错");
    assert!(check.can_close, "空年度应可年结：{:?}", check.blockers);
    assert!(check.blockers.is_empty());

    let json = serde_json::to_value(&check).expect("可序列化");
    assert_eq!(json["blockers"].as_array().map(Vec::len), Some(0));
    support::teardown().await;
}

/// `items` 不影响 `can_close()` 判定（判定只看 `blockers` 是否为空）。
///
/// 把挡路单据处理掉（这里直接删夹具行，等价于「到终态」）→ 同一份判定必须放行。
#[tokio::test]
async fn can_close_is_unaffected_by_items() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_po("YE-PO-001", 8, "DRAFT").await;

    let blocked = check_year(support::YEAR).await.expect("预检");
    assert!(!blocked.can_close, "有草稿采购单不能年结");
    let blocker = blocked
        .blockers
        .iter()
        .find(|item| item.kind == BLOCKER_DOC_UNFINISHED)
        .expect("必须报 DOC_UNFINISHED");
    assert!(!blocker.items.is_empty(), "明细非空");

    support::sql("DELETE FROM biz_purchase_order WHERE order_no = 'YE-PO-001'").await;
    let cleared = check_year(support::YEAR).await.expect("处理完单据后预检");
    assert!(
        cleared.can_close,
        "单据处理掉之后必须能年结：{:?}",
        cleared.blockers
    );
    support::teardown().await;
}

/// 年结与期间结账的**不变量口径一致**：同一批坏凭证，两个入口列出的单号一字不差。
///
/// 这条是「不要另发明一套」的护栏：只要有人把年结的 SQL 改回自写的一份，
/// 两个入口的明细就会开始漂移，这条用例会先红。
#[tokio::test]
async fn year_and_period_invariant_items_agree() {
    let Some(_guard) = support::setup().await else {
        return;
    };
    support::open_and_close().await;
    support::seed_broken_voucher("YE-V-902", 9).await;

    let year_items = {
        let blockers = collect_blockers(db(), support::YEAR)
            .await
            .expect("年结预检");
        blockers
            .into_iter()
            .find(|item| item.kind == BLOCKER_INVARIANT)
            .expect("年结必须报 INVARIANT")
            .items
    };
    let mut owners = period_items::OwnerResolver::new();
    let (_, period_list) = period_items::invariant_details_between(
        db(),
        NaiveDate::from_ymd_opt(i32::from(support::YEAR), 9, 1).expect("日期"),
        NaiveDate::from_ymd_opt(i32::from(support::YEAR), 9, 30).expect("日期"),
        &mut owners,
    )
    .await
    .expect("期间不变量明细");

    assert_eq!(
        year_items.len(),
        period_list.len(),
        "年结与期间的明细条数必须一致（同一份 SQL）"
    );
    let year_no: Vec<&str> = year_items.iter().map(|item| item.doc_no.as_str()).collect();
    let period_no: Vec<&str> = period_list
        .iter()
        .map(|item| item.doc_no.as_str())
        .collect();
    assert_eq!(
        year_no, period_no,
        "单号清单必须一致：{year_no:?} vs {period_no:?}"
    );
    support::teardown().await;
}

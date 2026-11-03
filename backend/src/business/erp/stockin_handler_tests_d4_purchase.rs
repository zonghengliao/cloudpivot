// ============================================================================
// stockin_handler_tests_d4_purchase.rs —— D4：在途/待收口径与采购单状态推进
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;

use super::stockin_handler_tests_fixtures::*;
use super::stockin_handler_tests_helpers::*;
use super::*;

/// ① 全新采购单（没收过）→ `pending_qty = ordered_qty`，且可以收货。
#[tokio::test]
async fn d4_pending_fresh_order_equals_ordered() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, _) = d4_seed_po("100", "0").await;
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items.len(), 1);
    assert_eq!(pending.items[0].ordered_qty, dec("100"));
    assert_eq!(pending.items[0].received_qty, dec("0"));
    assert_eq!(
        pending.items[0].pending_qty,
        dec("100"),
        "没收过 → 待收 = 订单"
    );
    assert_eq!(pending.items[0].material_code, "P2B1-M1", "带出物料编码");
    assert_eq!(pending.order.supplier_id, SUPPLIER, "带出供应商（O1）");
    assert_eq!(pending.order.warehouse_id, WAREHOUSE, "带出仓库（O1）");
    assert_eq!(pending.order.supplier_name, "P2B1 测试供应商");
    assert!(pending.can_receive);
    assert_eq!(pending.pending_line_count, 1);
    assert!(pending.blocked_reason.is_none());
}

/// ② 部分收货后 → `pending_qty = ordered − received`（只算已过账）。
#[tokio::test]
async fn d4_pending_after_partial_receipt() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, _) = d4_seed_po_with_posted("100", "30").await;
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items[0].received_qty, dec("30"));
    assert_eq!(pending.items[0].pending_qty, dec("70"), "待收 = 100 − 30");
    assert!(pending.can_receive);
}

/// ③ 全部收完 → 所有行 `pending_qty = 0`，`can_receive = false` + 明确原因。
#[tokio::test]
async fn d4_pending_all_received_is_zero() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, _) = d4_seed_po_with_posted("100", "100").await;
    d4_set_po_status(order_id, "RECEIVED").await;
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items[0].pending_qty, Decimal::ZERO);
    assert_eq!(pending.pending_line_count, 0);
    assert!(!pending.can_receive, "收满后前端不该显示「生成收货单」按钮");
    let reason = pending.blocked_reason.expect("必须给出不能收的原因");
    assert!(reason.contains("已收满"), "{reason}");
}

/// ④ 多张收货单累加 → `received_qty` 是总和。
#[tokio::test]
async fn d4_pending_sums_multiple_receipts() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po("100", "50").await;
    // 明细列写 50，真账拆成两张 POSTED（30 + 20）
    d4_insert_posted(order_id, item_id, "30").await;
    d4_insert_posted(order_id, item_id, "20").await;
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items[0].received_qty, dec("50"), "两张单要累加");
    assert_eq!(pending.items[0].pending_qty, dec("50"));
    assert_eq!(pending.stock_ins.len(), 2, "已生成收货单要能反查");
    assert!(pending.stock_ins.iter().all(|item| item.posted));
}

/// ⑤ 草稿收货单**不算**已收（硬红线 4：只有 POSTED 计数）。
#[tokio::test]
async fn d4_pending_ignores_draft_stock_in() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, _) = d4_seed_po("100", "0").await;
    // 只给 source_order_id → 自动带出 100 行；**不过账**，停在 DRAFT
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_id.to_string());
    let draft = create_stock_in(&request, None).await.unwrap();
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items[0].received_qty, dec("0"), "草稿不算已收");
    assert_eq!(pending.items[0].pending_qty, dec("100"));
    assert_eq!(pending.stock_ins.len(), 1);
    assert!(!pending.stock_ins[0].posted, "草稿要标成未过账");
    // 过账后才算
    change_stock_in_status(draft, &post_req(), None)
        .await
        .unwrap();
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items[0].received_qty, dec("100"), "过账后才计入");
    assert_eq!(pending.items[0].pending_qty, Decimal::ZERO);
}

/// ⑥ 超收 → 400，文案点明「本次 80 > 待收 70（已收 30 / 订单 100）」（O5 / 硬红线 6）。
#[tokio::test]
async fn d4_over_receipt_message_is_exact() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po_with_posted("100", "30").await;
    let mut over = line(MATERIAL_A, "80", "2", "0");
    over.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![over]);
    request.source_order_id = Some(order_id.to_string());
    let err = create_stock_in(&request, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("物料 P2B1-M1"), "要点名物料：{}", err.msg);
    assert!(err.msg.contains("本次收 80 件"), "{}", err.msg);
    assert!(err.msg.contains("只剩 70 件未收"), "{}", err.msg);
    assert!(err.msg.contains("已收 30"), "{}", err.msg);
    assert!(err.msg.contains("订单 100"), "{}", err.msg);
}

/// ⑦ 采购单是 DRAFT → 400（草稿不能收货）。
#[tokio::test]
async fn d4_draft_order_cannot_receive() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, _) = d4_seed_po("10", "0").await;
    d4_set_po_status(order_id, "DRAFT").await;
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_id.to_string());
    let err = create_stock_in(&request, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("DRAFT"), "{}", err.msg);
    assert!(err.msg.contains("不能收货"), "{}", err.msg);
    // 待收接口同样给出 false + 原因
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert!(!pending.can_receive);
    assert!(
        pending
            .blocked_reason
            .unwrap_or_default()
            .contains("不能收货"),
        "待收接口也要说清为什么不能收"
    );
}

/// ⑧ 采购单已 RECEIVED → 400（已收完，D4 把 RECEIVED 移出收货白名单）。
#[tokio::test]
async fn d4_received_order_cannot_receive() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po_with_posted("100", "100").await;
    d4_set_po_status(order_id, "RECEIVED").await;
    let mut again = line(MATERIAL_A, "1", "2", "0");
    again.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![again]);
    request.source_order_id = Some(order_id.to_string());
    let err = create_stock_in(&request, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("RECEIVED"), "{}", err.msg);
    assert!(err.msg.contains("不能收货"), "{}", err.msg);
}

/// ⑨ 采购单已 CANCELLED → 400。
#[tokio::test]
async fn d4_cancelled_order_cannot_receive() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, _) = d4_seed_po("10", "0").await;
    d4_set_po_status(order_id, "CANCELLED").await;
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_id.to_string());
    let err = create_stock_in(&request, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("CANCELLED"), "{}", err.msg);
}

/// ⑩ 明细的 `source_item_id` 不属于该采购单 → 400（不许跨单收货）。
#[tokio::test]
async fn d4_foreign_source_item_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_a, _) = d4_seed_po("10", "0").await;
    let (_order_b, item_b) = d4_seed_po("10", "0").await;
    let mut foreign = line(MATERIAL_A, "5", "2", "0");
    foreign.purchase_order_item_id = Some(item_b.to_string());
    let mut request = req(vec![foreign]);
    request.source_order_id = Some(order_a.to_string());
    let err = create_stock_in(&request, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(
        err.msg.contains("不属于本单的来源采购单"),
        "跨单明细必须点名拒绝：{}",
        err.msg
    );
}

/// ⑪ 生成收货单并过账（部分）→ 采购单变 PARTIAL_RECEIVED。
#[tokio::test]
async fn d4_post_advances_to_partial_received() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po("100", "0").await;
    let mut partial = line(MATERIAL_A, "30", "2", "0");
    partial.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![partial]);
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    // 生成的是草稿：过账前采购单状态不许变（硬红线 7「不许自动过账」）
    assert_eq!(d4_po_status(order_id).await, "SUBMITTED", "建单不许改状态");
    assert_eq!(d4_ledger_received(item_id).await, dec("0"), "草稿不动已收");
    change_stock_in_status(id, &post_req(), None).await.unwrap();
    assert_eq!(d4_po_status(order_id).await, "PARTIAL_RECEIVED");
    assert_eq!(d4_ledger_received(item_id).await, dec("30"));
}

/// ⑫ 收完全部 → 采购单变 RECEIVED。
#[tokio::test]
async fn d4_post_all_advances_to_received() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, _) = d4_seed_po("40", "0").await;
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    change_stock_in_status(id, &post_req(), None).await.unwrap();
    assert_eq!(d4_po_status(order_id).await, "RECEIVED");
}

/// ⑬ 删除草稿收货单 → 采购单状态与已收数量都不变（草稿从不计入）。
#[tokio::test]
async fn d4_delete_draft_does_not_change_order() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po("100", "0").await;
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    delete_stock_in(id, None).await.unwrap();
    assert_eq!(
        d4_po_status(order_id).await,
        "SUBMITTED",
        "删草稿不该动采购单"
    );
    assert_eq!(d4_ledger_received(item_id).await, dec("0"));
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(
        pending.items[0].pending_qty,
        dec("100"),
        "删草稿后待收回到满额"
    );
    assert!(pending.stock_ins.is_empty(), "软删的收货单不该出现在反查里");
}

/// ⑭ 作废已过账收货单 → 已收数量回退、采购单从 RECEIVED 退回 PARTIAL_RECEIVED，
/// 并且**能再收一次**（D4 的核心：收错了还能重来）。
#[tokio::test]
async fn d4_cancel_posted_restores_pending() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po("100", "0").await;
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    change_stock_in_status(id, &post_req(), None).await.unwrap();
    assert_eq!(d4_po_status(order_id).await, "RECEIVED");
    assert_eq!(d4_ledger_received(item_id).await, dec("100"));

    cancel(id, "收错了要退").await;
    assert_eq!(
        d4_ledger_received(item_id).await,
        dec("0"),
        "作废必须把已收数量退回去"
    );
    assert_eq!(
        d4_po_status(order_id).await,
        "PARTIAL_RECEIVED",
        "已收货单作废后采购单退回部分收货（否则再也收不进来）"
    );
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items[0].received_qty, dec("0"), "作废的单不算已收");
    assert_eq!(pending.items[0].pending_qty, dec("100"));
    assert!(pending.can_receive, "作废后必须能重新收货");

    // 真的能再收：再建一张并过账
    let mut again = req(Vec::new());
    again.source_order_id = Some(order_id.to_string());
    let second = create_stock_in(&again, None).await.unwrap();
    change_stock_in_status(second, &post_req(), None)
        .await
        .unwrap();
    assert_eq!(d4_po_status(order_id).await, "RECEIVED");
}

/// ⑮ `purchase_order_id`（卡面字段名）与 `source_order_id`（库列名）等价。
#[tokio::test]
async fn d4_purchase_order_id_alias_links_source() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po("20", "0").await;
    let mut request = req(Vec::new());
    request.purchase_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    let detail = detail_of(id).await.unwrap();
    assert_eq!(
        detail.header.source_order_id.as_deref(),
        Some(order_id.to_string().as_str()),
        "别名要落到 source_order_id 列"
    );
    assert_eq!(
        detail.purchase_order_id.as_deref(),
        Some(order_id.to_string().as_str()),
        "详情响应要给 purchase_order_id（卡面 ①）"
    );
    assert!(
        detail.purchase_order_no.is_some(),
        "详情响应要给 purchase_order_no（卡面 F3）"
    );
    assert_eq!(
        detail.items[0].source_item_id.as_deref(),
        Some(item_id.to_string().as_str())
    );
}

/// ⑯ 两个字段都给且不一致 → 400（不静默挑一个）。
#[tokio::test]
async fn d4_conflicting_alias_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_a, _) = d4_seed_po("20", "0").await;
    let (order_b, _) = d4_seed_po("20", "0").await;
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_a.to_string());
    request.purchase_order_id = Some(order_b.to_string());
    let err = create_stock_in(&request, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("不一致"), "{}", err.msg);
}

/// ⑰ 两条「已收」口径必须永远相等（冗余列 vs POSTED 聚合）。
///
/// 这是 D4 的口径锁：以后谁改一侧忘了另一侧，这条先红。
#[tokio::test]
async fn d4_pending_matches_ledger_column() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = d4_seed_po("100", "0").await;
    // 两张**部分**收货单：30 → 过账，20 → 过账
    for qty in ["30", "20"] {
        let mut request = req(vec![{
            let mut line = line(MATERIAL_A, qty, "2", "0");
            line.purchase_order_item_id = Some(item_id.to_string());
            line
        }]);
        request.source_order_id = Some(order_id.to_string());
        let id = create_stock_in(&request, None).await.unwrap();
        change_stock_in_status(id, &post_req(), None).await.unwrap();
    }
    let pending = crate::business::erp::purchase_handler::pending_items_of(order_id)
        .await
        .unwrap();
    assert_eq!(pending.items.len(), 1);
    let row = &pending.items[0];
    assert_eq!(row.received_qty, dec("50"), "两张单要累加");
    assert_eq!(row.pending_qty, dec("50"));
    let ledger: Decimal = d4_ledger_received(item_id).await;
    assert_eq!(
        row.received_qty, ledger,
        "冗余列（写路径闸）与已过账聚合（读路径）必须一致"
    );
}

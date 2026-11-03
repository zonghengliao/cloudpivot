// ============================================================================
// stockin_handler_tests_p10_locked.rs —— P10：已过账字段锁（金额/数量/明细/总额/备注）
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;

use super::stockin_handler_tests_fixtures::*;
use super::stockin_handler_tests_helpers::*;
use super::*;
use crate::business::erp::stockin_dto::CreateStockInReq;

// ==================== P10（铁律 ③④）：入账后只读 + 采购单状态追溯 ====================
/// ⑧ POSTED 收货单改金额 → 409，文案点「已入账」。
#[tokio::test]
async fn p10_posted_amount_change_is_conflict() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "5", "3", "0")]), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("单据已入账"), "{}", err.msg);
    assert!(err.msg.contains("POSTED"), "文案要点名状态：{}", err.msg);
}

/// ⑨ POSTED 收货单改数量 → 409。
#[tokio::test]
async fn p10_posted_qty_change_is_conflict() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "6", "2", "0")]), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("单据已入账"), "{}", err.msg);
}

/// ⑩ POSTED 收货单改明细（换物料）→ 409。
#[tokio::test]
async fn p10_posted_items_change_is_conflict() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    let err = update_stock_in(id, &req(vec![line(MATERIAL_B, "5", "2", "0")]), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("明细"), "{}", err.msg);
}

/// ⑪ POSTED 收货单只改 remark → 放行（白名单），且真的落库、金额不动。
#[tokio::test]
async fn p10_posted_remark_change_is_allowed() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    let mut request = req(vec![line(MATERIAL_A, "5", "2", "0")]);
    request.remark = Some("客户急件，先入先出".to_string());
    update_stock_in(id, &request, None)
        .await
        .unwrap_or_else(|err| panic!("只改备注应当放行：{} {}", err.code, err.msg));

    let remark: Option<String> = db()
        .scalar_with_params(
            "SELECT remark FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(remark.as_deref(), Some("客户急件，先入先出"));
    let amount: Decimal = db()
        .scalar_with_params(
            "SELECT total_amount FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(amount, dec("10"), "白名单只放 remark，金额一分不能动");
}

/// ⑫ DRAFT 收货单改金额 → 放行（未入账可以改）。
#[tokio::test]
async fn p10_draft_amount_change_is_allowed() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
        .await
        .unwrap();
    update_stock_in(id, &req(vec![line(MATERIAL_A, "5", "3", "0")]), None)
        .await
        .unwrap_or_else(|err| panic!("草稿应当能改：{} {}", err.code, err.msg));
    let amount: Decimal = db()
        .scalar_with_params(
            "SELECT total_amount FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(amount, dec("15"));
}

/// ⑰ 卡面 §五-3 的验收请求原样跑一遍：`PUT {"total_amount": 999}` → 409。
///
/// 这条专门钉住「未知字段被 serde 静默丢掉 → 200」的假绿：
/// `CreateStockInReq` 必须显式收下 `total_*`，否则改金额的请求会变成「什么都没改」。
#[tokio::test]
async fn p10_posted_total_amount_field_alone_is_conflict() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    // 整个 body 只有 total_amount：明细、表头字段一个都不给
    let payload = CreateStockInReq {
        total_amount: Some(dec("999")),
        ..Default::default()
    };
    let err = update_stock_in(id, &payload, None).await.unwrap_err();
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("单据已入账"), "{}", err.msg);
    assert!(
        err.msg.contains("total_amount"),
        "文案要点名被改的字段：{}",
        err.msg
    );
    // 库里金额一个字节都不能动
    let stored: Decimal = db()
        .scalar_with_params(
            "SELECT total_amount FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(stored, dec("10"));
}

/// ⑱ 草稿态直接传合计 → 400（合计只能由明细派生；不是「已入账」问题）。
#[tokio::test]
async fn p10_draft_total_amount_direct_is_bad_request() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
        .await
        .unwrap();
    let payload = CreateStockInReq {
        total_amount: Some(dec("999")),
        ..Default::default()
    };
    let err = update_stock_in(id, &payload, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("由明细派生"), "{}", err.msg);
}

/// ⑬ 收货过账后采购单变 RECEIVED，`biz_status_log` 一行且 `biz_ref_no` = 收货单号。
#[tokio::test]
async fn p10_receipt_advances_order_and_logs_ref() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("30", "10").await;
    let mut line_a = line(MATERIAL_A, "20", "2", "0");
    line_a.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![line_a]);
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    change_stock_in_status(id, &post_req(), None)
        .await
        .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));

    let status: String = db()
        .scalar_with_params(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap();
    assert_eq!(status, "RECEIVED");

    let in_no: String = db()
        .scalar_with_params(
            "SELECT in_no FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    let rows = db()
        .find_with_params(
            "SELECT entity_type, from_status, to_status, biz_ref_type, biz_ref_no \
               FROM biz_status_log WHERE entity_id = ?::uuid ORDER BY created_at",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "推进一次 = 一行日志（不许自环重复写）");
    let row = &rows[0];
    assert_eq!(
        row.get_typed::<String>("entity_type").unwrap(),
        "PURCHASE_ORDER"
    );
    assert_eq!(row.get_typed::<String>("from_status").unwrap(), "SUBMITTED");
    assert_eq!(row.get_typed::<String>("to_status").unwrap(), "RECEIVED");
    assert_eq!(
        row.get_typed::<Option<String>>("biz_ref_no")
            .unwrap()
            .as_deref(),
        Some(in_no.as_str()),
        "日志必须能追到是哪张收货单推的"
    );
    assert_eq!(
        row.get_typed::<Option<String>>("biz_ref_type")
            .unwrap()
            .as_deref(),
        Some("STOCK_IN")
    );
}

/// ⑭ 部分收货 → 采购单变 PARTIAL_RECEIVED。
#[tokio::test]
async fn p10_partial_receipt_advances_to_partial_received() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("30", "10").await;
    let mut line_a = line(MATERIAL_A, "10", "2", "0");
    line_a.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![line_a]);
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    change_stock_in_status(id, &post_req(), None).await.unwrap();

    let status: String = db()
        .scalar_with_params(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap();
    assert_eq!(status, "PARTIAL_RECEIVED");

    let order_no: String = db()
        .scalar_with_params(
            "SELECT order_no FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap();
    let trail = crate::business::shared::state_machine::trail_of_no(db(), &order_no)
        .await
        .unwrap();
    assert_eq!(trail.len(), 1);
    assert_eq!(trail[0].from_status.as_deref(), Some("SUBMITTED"));
    assert_eq!(trail[0].to_status, "PARTIAL_RECEIVED");
}

// ============================================================================
// stockin_handler_tests_update.rs —— 编辑草稿/已过账锁定/序列号校验/来源采购单/分页筛选
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
use crate::business::erp::stockin_dto::StockInLineSerials;
use crate::business::erp::stockin_dto::StockInStatusReq;
use crate::business::erp::stockin_model::{self, STATUS_CANCELLED, STATUS_POSTED};
use crate::business::flow::model as flow_model;
use crate::shared::error_code;

/// ⑧ DRAFT 改单：能改，明细整单重建、汇总重算。
#[tokio::test]
async fn update_draft_replaces_lines() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = create_stock_in(&req(vec![line(MATERIAL_A, "100", "2", "13")]), None)
        .await
        .unwrap();
    update_stock_in(
        id,
        &req(vec![
            line(MATERIAL_A, "10", "2", "13"),
            line(MATERIAL_B, "5", "3", "6"),
        ]),
        None,
    )
    .await
    .unwrap();
    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.items.len(), 2, "旧行必须被替换掉");
    assert_eq!(detail.items[0].line_no, 1);
    assert_eq!(detail.items[1].line_no, 2);
    assert_eq!(detail.header.total_amount, dec("35"), "10×2 + 5×3");
    assert_eq!(detail.header.total_tax, dec("3.50"), "2.60 + 0.90");
    assert_eq!(detail.header.total_qty, dec("15"));
}

/// ⑨ POSTED 改单：409。P10 起文案换成统一的「单据已入账」口径（铁律 ④），
/// 语义不变：已过账的单据不能改任何账务字段。
#[tokio::test]
async fn update_posted_is_conflict() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
    let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "1", "2", "0")]), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("单据已入账"), "{}", err.msg);
    assert!(err.msg.contains("作废或红冲"), "{}", err.msg);
}

/// ⑩ 一物一码数量与 `serial_ids` 条数不符 → 400（建单就拦住）。
#[tokio::test]
async fn serial_count_mismatch_is_bad_request() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mut bad = line(MATERIAL_SN, "3", "40", "13");
    bad.serial_ids = vec!["SN-A".to_string(), "SN-B".to_string()];
    let err = create_stock_in(&req(vec![bad]), None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("序列号条数与数量不符"), "{}", err.msg);

    // 数量对得上就放行；自定义码在**过账**时提交（明细表没有存序列号的列）
    let mut good = line(MATERIAL_SN, "2", "40", "0");
    good.serial_ids = vec!["SN-P2B1-001".to_string(), "SN-P2B1-002".to_string()];
    let id = create_stock_in(&req(vec![good]), None).await.unwrap();
    let mut post = post_req();
    post.serials = vec![StockInLineSerials {
        line_no: 1,
        serial_ids: vec!["SN-P2B1-001".to_string(), "SN-P2B1-002".to_string()],
    }];
    change_stock_in_status(id, &post, None).await.unwrap();
    let rows = db()
        .find_with_params(
            "SELECT serial_no FROM biz_material_serial WHERE source_id = ?::uuid ORDER BY serial_no",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    let numbers: Vec<String> = rows
        .iter()
        .map(|row| row.get_typed("serial_no").unwrap())
        .collect();
    assert_eq!(
        numbers,
        vec!["SN-P2B1-001", "SN-P2B1-002"],
        "过账时提交的码要原样落库（不改写供应商贴的码）"
    );

    // 过账时条数不符也拦住
    let mut bad_post = post_req();
    bad_post.serials = vec![StockInLineSerials {
        line_no: 1,
        serial_ids: vec!["ONLY-ONE".to_string()],
    }];
    let another = create_stock_in(&req(vec![line(MATERIAL_SN, "2", "40", "0")]), None)
        .await
        .unwrap();
    let err = change_stock_in_status(another, &bad_post, None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("序列号条数与数量不符"), "{}", err.msg);
}

/// ⑩b 非一物一码物料给了 `serial_ids` → 400（`66007`），不静默丢。
#[tokio::test]
async fn serial_ids_on_non_serial_material_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let mut bad = line(MATERIAL_A, "2", "1", "0");
    bad.serial_ids = vec!["SN-X".to_string(), "SN-Y".to_string()];
    let err = create_stock_in(&req(vec![bad]), None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SERIAL_NOT_REQUIRED);
}

/// ⑪ 收货数量超过采购单未收数量 → 400（带 `source_order_id`）。
#[tokio::test]
async fn over_receipt_against_purchase_order_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("30", "10").await;
    // 剩余 20，要收 25 → 建单即 400
    let mut over = line(MATERIAL_A, "25", "2", "0");
    over.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![over]);
    request.source_order_id = Some(order_id.to_string());
    let err = create_stock_in(&request, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    // D4 文案口径：一句话说清「本次 / 待收 / 已收 / 订单」四个数
    assert!(err.msg.contains("本次收 25 件"), "{}", err.msg);
    assert!(err.msg.contains("只剩 20 件未收"), "{}", err.msg);
    assert!(err.msg.contains("已收 10"), "{}", err.msg);
    assert!(err.msg.contains("订单 30"), "{}", err.msg);

    // 收 20（刚好剩余）→ 成功，采购单变 RECEIVED 且 received_qty = 30
    let mut ok = line(MATERIAL_A, "20", "2", "0");
    ok.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![ok]);
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    change_stock_in_status(id, &post_req(), None).await.unwrap();

    let row = db()
        .find_with_params(
            "SELECT received_qty FROM biz_purchase_order_item WHERE id = ?::uuid",
            vec![Value::from(&item_id)],
        )
        .await
        .unwrap();
    assert_eq!(
        row[0].get_typed::<Decimal>("received_qty").unwrap(),
        dec("30"),
        "过账必须回写采购明细的已收数量"
    );
    let status: String = db()
        .scalar_with_params(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap();
    assert_eq!(status, "RECEIVED", "收满自动推进采购单状态");
}

/// ⑪b 自动带出：只给 `source_order_id`（items 空），按未收数量成单。
#[tokio::test]
async fn auto_pull_lines_from_purchase_order() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("30", "10").await;
    let mut request = req(Vec::new());
    request.source_order_id = Some(order_id.to_string());
    let id = create_stock_in(&request, None).await.unwrap();
    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.items.len(), 1);
    assert_eq!(detail.items[0].qty, dec("20"), "自动带出的应该是未收的 20");
    assert_eq!(
        detail.items[0].source_item_id.as_deref(),
        Some(item_id.to_string().as_str())
    );

    change_stock_in_status(id, &post_req(), None).await.unwrap();
    let status: String = db()
        .scalar_with_params(
            "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
            vec![Value::from(&order_id)],
        )
        .await
        .unwrap();
    assert_eq!(status, "RECEIVED");
}

/// ⑭ 删除：仅 DRAFT 能软删；POSTED 删除 → 409。
#[tokio::test]
async fn delete_only_draft() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let draft = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
        .await
        .unwrap();
    delete_stock_in(draft, None).await.unwrap();
    assert!(detail_of(draft).await.is_err(), "软删后详情 404");

    let posted = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    let err = delete_stock_in(posted, None).await.unwrap_err();
    assert_eq!(err.status, 409, "{}", err.msg);
}

/// ⑮ 列表：按状态 / 供应商筛，分页字段与总数正确。
#[tokio::test]
async fn page_filters_by_status() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    create_stock_in(&req(vec![line(MATERIAL_B, "5", "2", "0")]), None)
        .await
        .unwrap();
    let filter = stockin_model::StockInFilter {
        status: Some("POSTED".to_string()),
        supplier_id: Some(SUPPLIER.to_string()),
        page: 1,
        page_size: 50,
        ..Default::default()
    };
    let (rows, total) = stockin_model::stock_in_page(&filter).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, STATUS_POSTED);
    assert!(rows[0].in_no.starts_with("IN-"), "{}", rows[0].in_no);
    assert_eq!(rows[0].currency_code, "CNY");
}

// ---------- 审批实例唯一索引冲突：409 + 自家单号（不是「单号重复、去查 biz_seq」） ----------
/// 撞 `uk_biz_flow_instance_biz_active` → **409 + 本单号 + 「已有审批中实例」**。
///
/// 两条分支都钉住：驱动给了约束名（`UniqueViolation`）与退化形态（`DbError::InvalidValue`，
/// 文案里只剩 PG 原文）—— 后者正是真库里掉到「联系运维排查 biz_seq」的那条路。
#[test]
fn biz_active_conflict_maps_to_409_with_stock_in_no() {
    let variant = || DbError::UniqueViolation {
        column: None,
        constraint: Some(flow_model::BIZ_ACTIVE_INDEX.to_string()),
        source: None,
    };
    let by_text = || {
        DbError::InvalidValue(format!(
            "db error: ERROR: duplicate key value violates unique constraint \"{}\"",
            flow_model::BIZ_ACTIVE_INDEX
        ))
    };
    // 别的唯一索引不能被误判成审批冲突
    let other = DbError::UniqueViolation {
        column: None,
        constraint: Some("uk_biz_stock_in_no".to_string()),
        source: None,
    };
    assert!(!flow_model::is_biz_active_duplicate(&other));

    for err in [variant(), by_text()] {
        let api = submit_conflict(err, "发起过账审批单", "IN-20261102-000123");
        assert_eq!(api.status, 409, "{}", api.msg);
        assert_eq!(api.code, error_code::RESOURCE_CONFLICT);
        assert!(api.msg.contains("IN-20261102-000123"), "{}", api.msg);
        assert!(api.msg.contains("已有审批中实例"), "{}", api.msg);
        assert!(
            !api.msg.contains("biz_seq"),
            "不能把用户指到取号器：{}",
            api.msg
        );
        assert!(!api.msg.contains("单号重复"), "{}", api.msg);
    }

    // 拿不到单号的通用出口（write_error）也必须 409 + 可执行文案
    let generic = write_error(variant(), "收货入库单状态流转");
    assert_eq!(generic.status, 409);
    assert_eq!(generic.code, error_code::RESOURCE_CONFLICT);
    assert!(generic.msg.contains("该收货入库单"), "{}", generic.msg);
    assert!(!generic.msg.contains("biz_seq"), "{}", generic.msg);

    // 真的号段冲突不能被新分支吞掉：仍然 409 + 取号器提示
    let genuine = write_error(
        DbError::UniqueViolation {
            column: None,
            constraint: Some("uk_biz_stock_in_no".to_string()),
            source: None,
        },
        "创建收货入库单",
    );
    assert_eq!(genuine.status, 409);
    assert!(genuine.msg.contains("biz_seq"), "{}", genuine.msg);
}

/// ⑬ 作废：DRAFT → CANCELLED 不写库存；POSTED → CANCELLED 需原因，**库存冲回**（P9 改口径）。
#[tokio::test]
async fn cancel_paths() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let draft = create_stock_in(&req(vec![line(MATERIAL_A, "5", "2", "0")]), None)
        .await
        .unwrap();
    change_stock_in_status(
        draft,
        &StockInStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: Some("供应商没货".to_string()),
            serials: Vec::new(),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        Decimal::ZERO,
        "草稿作废不该产生库存"
    );
    let detail = detail_of(draft).await.unwrap();
    assert_eq!(detail.header.status, STATUS_CANCELLED);
    assert!(detail.header.cancelled_at.is_some());
    assert_eq!(detail.header.cancel_reason.as_deref(), Some("供应商没货"));

    // POSTED → CANCELLED：必须给原因
    let posted = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    let err = change_stock_in_status(
        posted,
        &StockInStatusReq {
            status: Some("CANCELLED".to_string()),
            reason: None,
            serials: Vec::new(),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    // P16：给了原因 → 发起作废审批 → 复核通过 → 才回滚六本账
    cancel(posted, "收错仓了").await;
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        dec("0"),
        "P9：POSTED→CANCELLED 必须把库存退回去（旧口径「只改状态」是 E4 的根因）"
    );
    assert_eq!(inventory_of(MATERIAL_A).await.1, dec("0"));
}

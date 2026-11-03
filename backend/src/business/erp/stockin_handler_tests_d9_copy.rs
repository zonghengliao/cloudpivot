// ============================================================================
// stockin_handler_tests_d9_copy.rs —— D9：复制入库单（新单号、清联动、权限）
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_tests_fixtures::*;
use super::stockin_handler_tests_helpers::*;
use super::*;
use crate::business::erp::stockin_model::{STATUS_DRAFT, STATUS_POSTED};
use crate::shared::error_code;

/// ① 复制一张已过账收货单：新单号 + `DRAFT` + 明细/金额/入库日期口径一致。
#[tokio::test]
async fn d9_stockin_copy_creates_draft_with_new_no_and_same_items() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let origin_id = post_new(vec![
        line(MATERIAL_A, "10", "2", "13"),
        line(MATERIAL_B, "5", "1.5", "0"),
    ])
    .await;
    let origin = detail_of(origin_id).await.unwrap();
    assert_eq!(origin.header.status, STATUS_POSTED);

    let new_id = copy_stock_in(origin_id, &create_only())
        .await
        .expect("复制应成功");
    let copied = detail_of(new_id).await.unwrap();

    assert_ne!(copied.header.id, origin.header.id);
    assert_ne!(copied.header.in_no, origin.header.in_no, "单号必须是新的");
    assert!(
        copied.header.in_no.starts_with("IN-"),
        "{}",
        copied.header.in_no
    );
    assert_eq!(copied.header.status, STATUS_DRAFT, "复制出来的必须是草稿");
    assert_eq!(copied.header.status_label, "草稿");
    assert_eq!(copied.header.supplier_id, origin.header.supplier_id);
    assert_eq!(copied.header.warehouse_id, origin.header.warehouse_id);
    assert_eq!(copied.header.total_qty, origin.header.total_qty);
    assert_eq!(copied.header.total_amount, origin.header.total_amount);
    assert_eq!(copied.header.total_tax, origin.header.total_tax);
    assert_eq!(copied.header.total_with_tax, origin.header.total_with_tax);
    assert_eq!(
        copied.header.invoice_received,
        origin.header.invoice_received
    );
    assert_eq!(copied.items.len(), origin.items.len());
    for (new_line, old_line) in copied.items.iter().zip(origin.items.iter()) {
        assert_eq!(new_line.material_id, old_line.material_id);
        assert_eq!(new_line.qty, old_line.qty);
        assert_eq!(new_line.price, old_line.price);
        assert_eq!(new_line.tax_rate, old_line.tax_rate);
        assert_eq!(new_line.amount, old_line.amount);
        assert_eq!(new_line.location_id, old_line.location_id);
    }
    // 入库日期口径：复制是新的入库动作 → 今天（不沿用原单日期）
    assert_eq!(
        copied.header.in_date,
        crate::business::erp::stockin_dto::today_cst()
    );
    // 原单没被动：仍是已过账
    let origin_after = detail_of(origin_id).await.unwrap();
    assert_eq!(origin_after.header.status, STATUS_POSTED);
    assert_eq!(origin_after.header.in_no, origin.header.in_no);
}

/// ② 复制带来源采购单的收货单 → 新单 `purchase_order_id`（`source_order_id`）清空。
#[tokio::test]
async fn d9_stockin_copy_clears_purchase_order_id() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("30", "0").await;
    let mut only = line(MATERIAL_A, "10", "2", "0");
    only.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![only]);
    request.source_order_id = Some(order_id.to_string());
    let origin_id = create_stock_in(&request, None).await.unwrap();
    let origin = detail_of(origin_id).await.unwrap();
    assert_eq!(
        origin.header.source_order_id.as_deref(),
        Some(order_id.to_string().as_str()),
        "前提：原单确实引用了采购单"
    );

    let new_id = copy_stock_in(origin_id, &create_only()).await.unwrap();
    let copied = detail_of(new_id).await.unwrap();
    assert!(
        copied.header.source_order_id.is_none(),
        "卡面决策 B：新单必须清空采购关联（否则待收额度被二次占用），实际 {:?}",
        copied.header.source_order_id
    );
    assert!(copied.header.source_order_no.is_none());
    assert!(
        copied
            .items
            .iter()
            .all(|line| line.source_item_id.is_none()),
        "明细行的 source_item_id 也必须清空"
    );
    // 原单的关联原样保留（复制不改原单）
    let origin_after = detail_of(origin_id).await.unwrap();
    assert_eq!(
        origin_after.header.source_order_id,
        origin.header.source_order_id
    );
    assert_eq!(
        origin_after.items[0].source_item_id,
        origin.items[0].source_item_id
    );
}

/// ③ 已过账原单（有凭证 + 应付）复制后：凭证 / 应付 / 审批实例 / 过账时间全部为空。
#[tokio::test]
async fn d9_stockin_copy_clears_voucher_payable_and_approval_links() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let origin_id = post_new(vec![line(MATERIAL_A, "10", "2", "13")]).await;
    let origin = detail_of(origin_id).await.unwrap();
    assert!(
        origin.header.voucher_id.is_some(),
        "前提：过账要生成进项凭证"
    );
    assert!(origin.header.payable_id.is_some(), "前提：过账要生成应付单");

    let new_id = copy_stock_in(origin_id, &create_only()).await.unwrap();
    let copied = detail_of(new_id).await.unwrap();
    assert!(copied.header.voucher_id.is_none(), "凭证关联必须清空");
    assert!(copied.header.payable_id.is_none(), "应付关联必须清空");
    assert!(
        copied.header.approval_instance_id.is_none(),
        "审批实例必须清空"
    );
    assert!(copied.header.posted_at.is_none(), "过账时间必须清空");
    assert!(copied.header.posted_by.is_none());
    assert!(copied.header.cancelled_at.is_none());
    assert!(copied.header.cancel_reason.is_none());
    assert!(copied.header.created_at >= origin.header.created_at);
    // 库里再核一遍（详情 DTO 万一漏字段，这条兜住）：用 `IS NULL` 判据，
    // 免得依赖驱动的时间类型映射
    let rows = db()
        .find_with_params(
            "SELECT status, (voucher_id IS NULL) AS v_null, (payable_id IS NULL) AS p_null, \
                    (approval_instance_id IS NULL) AS a_null, (posted_at IS NULL) AS posted_null, \
                    (posted_by IS NULL) AS poster_null, (cancelled_at IS NULL) AS cancel_null \
               FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&new_id)],
        )
        .await
        .unwrap();
    let row = rows.first().expect("新单必须存在");
    assert_eq!(row.get_typed::<String>("status").unwrap(), STATUS_DRAFT);
    assert!(
        row.get_typed::<bool>("v_null").unwrap(),
        "voucher_id 必须 NULL"
    );
    assert!(
        row.get_typed::<bool>("p_null").unwrap(),
        "payable_id 必须 NULL"
    );
    assert!(
        row.get_typed::<bool>("a_null").unwrap(),
        "approval_instance_id 必须 NULL"
    );
    assert!(
        row.get_typed::<bool>("posted_null").unwrap(),
        "posted_at 必须 NULL"
    );
    assert!(row.get_typed::<bool>("poster_null").unwrap());
    assert!(row.get_typed::<bool>("cancel_null").unwrap());
}

/// ⑥ 原单引用的采购单已被物理删除 → 复制照常成功（采购关联本来就清空）。
#[tokio::test]
async fn d9_stockin_copy_works_when_source_purchase_order_is_gone() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("10", "0").await;
    let mut only = line(MATERIAL_A, "10", "2", "0");
    only.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![only]);
    request.source_order_id = Some(order_id.to_string());
    let origin_id = create_stock_in(&request, None).await.unwrap();

    // 采购单物理删除（`source_order_id` 是弱引用，没有外键挡）
    sql(&format!(
        "DELETE FROM biz_purchase_order_item WHERE order_id = '{order_id}'::uuid"
    ))
    .await;
    sql(&format!(
        "DELETE FROM biz_purchase_order WHERE id = '{order_id}'::uuid"
    ))
    .await;

    let new_id = copy_stock_in(origin_id, &create_only())
        .await
        .expect("采购单没了也要能复制（复制根本不读它）");
    let copied = detail_of(new_id).await.unwrap();
    assert!(copied.header.source_order_id.is_none());
    assert!(copied.header.source_order_no.is_none());
    assert_eq!(copied.items.len(), 1);
    assert_eq!(copied.items[0].qty, dec("10"));
}

/// ⑦ 原单没有明细 → 400，且文案点明「没有明细」。
#[tokio::test]
async fn d9_stockin_copy_without_items_is_400() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let origin_id = create_stock_in(&req(vec![line(MATERIAL_A, "3", "2", "0")]), None)
        .await
        .unwrap();
    // 造「空壳单」：把明细删掉（库层没有「至少一行」的约束）
    sql(&format!(
        "DELETE FROM biz_stock_in_item WHERE stock_in_id = '{origin_id}'::uuid"
    ))
    .await;

    let err = copy_stock_in(origin_id, &create_only())
        .await
        .expect_err("没有明细必须 400");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::PARAM_INVALID);
    assert!(err.msg.contains("没有任何明细"), "文案要点名：{}", err.msg);
    // 一张新单都不许建出来
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_stock_in WHERE supplier_id = ?::uuid",
            vec![Value::from(&Uuid::parse_str(SUPPLIER).unwrap())],
        )
        .await
        .unwrap();
    assert_eq!(count, 1, "失败时不能留下半张单");
}

/// ⑧ 连点三下 → 三张不同单号的草稿（取号器每次都取新号）。
#[tokio::test]
async fn d9_stockin_copy_three_times_yields_three_distinct_nos() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let origin_id = post_new(vec![line(MATERIAL_A, "2", "2", "0")]).await;
    let mut nos = Vec::new();
    for _ in 0..3 {
        let new_id = copy_stock_in(origin_id, &create_only()).await.unwrap();
        let detail = detail_of(new_id).await.unwrap();
        assert_eq!(detail.header.status, STATUS_DRAFT);
        nos.push(detail.header.in_no);
    }
    nos.sort();
    nos.dedup();
    assert_eq!(nos.len(), 3, "三次复制必须三个不同单号：{nos:?}");
}

/// ⑨ 复制不改原单：整行快照 + 明细行 id / 数量前后完全一致。
#[tokio::test]
async fn d9_stockin_copy_does_not_touch_origin() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let origin_id = post_new(vec![line(MATERIAL_A, "4", "2.5", "13")]).await;
    let before_header = header_snapshot(origin_id).await;
    let before_items: Vec<(Uuid, i32, Decimal)> = db()
        .find_with_params(
            "SELECT id, line_no, qty FROM biz_stock_in_item WHERE stock_in_id = ?::uuid \
              ORDER BY line_no",
            vec![Value::from(&origin_id)],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("id").unwrap(),
                row.get_typed::<i32>("line_no").unwrap(),
                row.get_typed::<Decimal>("qty").unwrap(),
            )
        })
        .collect();
    let before_updated: String = db()
        .scalar_with_params(
            "SELECT updated_at::text FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&origin_id)],
        )
        .await
        .unwrap();

    copy_stock_in(origin_id, &create_only()).await.unwrap();
    copy_stock_in(origin_id, &create_only()).await.unwrap();

    assert_eq!(
        header_snapshot(origin_id).await,
        before_header,
        "原单整行必须一个字节都不变（含 updated_at）"
    );
    let after_updated: String = db()
        .scalar_with_params(
            "SELECT updated_at::text FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&origin_id)],
        )
        .await
        .unwrap();
    assert_eq!(after_updated, before_updated, "原单 updated_at 不许变");
    let after_items: Vec<(Uuid, i32, Decimal)> = db()
        .find_with_params(
            "SELECT id, line_no, qty FROM biz_stock_in_item WHERE stock_in_id = ?::uuid \
              ORDER BY line_no",
            vec![Value::from(&origin_id)],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get_typed::<Uuid>("id").unwrap(),
                row.get_typed::<i32>("line_no").unwrap(),
                row.get_typed::<Decimal>("qty").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        after_items, before_items,
        "原单明细行必须原样（id 都不许变）"
    );
}

/// ⑩ 权限：只有 `:list` 码 → 403 且文案点名 create；复制守的码与建单逐字相同。
#[tokio::test]
async fn d9_stockin_copy_requires_create_permission() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    assert_eq!(
        COPY_PERMISSION, "business:erp:stock-in:create",
        "硬红线 5：复制权限必须 = 建单权限码"
    );
    let origin_id = create_stock_in(&req(vec![line(MATERIAL_A, "1", "2", "0")]), None)
        .await
        .unwrap();

    let no_create = copy_claims(&["business:erp:stock-in:list"]);
    let err = copy_stock_in(origin_id, &no_create)
        .await
        .expect_err("没有建单权限必须 403");
    assert_eq!(err.status, 403, "{}", err.msg);
    assert!(
        err.msg.contains("business:erp:stock-in:create"),
        "403 文案要点名权限码：{}",
        err.msg
    );
    // 放宽成「有 create」→ 放行（证明差异只在那一个码）
    let with_create = copy_claims(&["business:erp:stock-in:list", COPY_PERMISSION]);
    copy_stock_in(origin_id, &with_create)
        .await
        .expect("有 create 码就应放行");
}

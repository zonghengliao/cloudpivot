// ============================================================================
// stockin_handler_tests_location.rs —— 库位账：空桶 / 单行累加 / 失败回滚 / 并发共用一行
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
use crate::business::erp::stockin_model::{self, STATUS_DRAFT};

/// ⑥（P6）收货**不带** `location_id` → 库位账写 `location_id IS NULL` 那一行。
#[tokio::test]
async fn receipt_without_location_writes_null_bucket() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    assert_eq!(
        null_bucket_qty(MATERIAL_A).await,
        None,
        "开局不能有 NULL 桶行"
    );
    post_new(vec![line(MATERIAL_A, "200", "2", "0")]).await;

    assert_eq!(
        null_bucket_qty(MATERIAL_A).await,
        Some(dec("200")),
        "不带库位的收货必须落进 (物料, 仓库, NULL) 桶"
    );
    assert_eq!(location_row_count(MATERIAL_A).await, 1, "只该有一行");
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT id FROM biz_inventory_location \
                  WHERE material_id = '{MATERIAL_A}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            vec![],
        )
        .await
        .unwrap();
    let row_id: Uuid = rows[0].get_typed("id").unwrap();
    let (_, locked) = location_row_of(&row_id).await.expect("库位账行必须存在");
    assert_eq!(locked, Decimal::ZERO, "收货不占锁定量，locked_qty 初始 0");
}

/// ⑦（P6）收货**带** `location_id` → 库位账加到那一行（不是新起一行）。
#[tokio::test]
async fn receipt_with_location_updates_that_row() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
    assert_eq!(location_qty_of(&row).await, Some(dec("0")));

    post_new(vec![line_at(MATERIAL_A, "120", "2", "0", &row.to_string())]).await;

    assert_eq!(
        location_qty_of(&row).await,
        Some(dec("120")),
        "库位账必须加在指定的那一行上"
    );
    assert_eq!(location_row_count(MATERIAL_A).await, 1, "不能多起一行");
    assert_eq!(
        null_bucket_qty(MATERIAL_A).await,
        None,
        "有库位时不该动 NULL 桶"
    );
    // 成本账同一维度：biz_inventory 的 location_id 也指这一行
    let cost_loc: Option<Uuid> = db()
        .find_with_params(
            &format!(
                "SELECT location_id FROM biz_inventory \
                  WHERE material_id = '{MATERIAL_A}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
            ),
            vec![],
        )
        .await
        .unwrap()
        .first()
        .and_then(|row| row.get_typed("location_id").unwrap());
    assert_eq!(cost_loc, Some(row), "成本账与库位账必须指向同一行");
}

/// ⑧（P6）同一物料同一库位多次收货 → **累加**，不新建行（唯一键生效）。
#[tokio::test]
async fn same_location_receipts_accumulate_one_row() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
    post_new(vec![line_at(MATERIAL_A, "70", "2", "0", &row.to_string())]).await;
    post_new(vec![line_at(MATERIAL_A, "30", "2", "0", &row.to_string())]).await;

    assert_eq!(location_qty_of(&row).await, Some(dec("100")));
    assert_eq!(
        location_row_count(MATERIAL_A).await,
        1,
        "uk_biz_inv_loc(material, warehouse, location) 生效：只有一行"
    );
}

/// ⑨（P6）不同库位分别收 → 两行独立（互不串数）。
#[tokio::test]
async fn different_locations_stay_separate() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let a = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
    let b = seed_location_row(MATERIAL_A, LOC_DICT_B).await;
    assert_ne!(a, b);

    // 一张单里两行分别进两个库位
    post_new(vec![
        line_at(MATERIAL_A, "40", "2", "0", &a.to_string()),
        line_at(MATERIAL_A, "60", "2", "0", &b.to_string()),
    ])
    .await;

    assert_eq!(location_qty_of(&a).await, Some(dec("40")));
    assert_eq!(location_qty_of(&b).await, Some(dec("60")));
    assert_eq!(location_row_count(MATERIAL_A).await, 2);
}

/// ⑩（P6）不变式③：`SUM(库位账 qty) = 成本账 qty`（多库位 + 多张单混合）。
#[tokio::test]
async fn location_sum_equals_cost_book() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let a = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
    let b = seed_location_row(MATERIAL_A, LOC_DICT_B).await;
    post_new(vec![
        line_at(MATERIAL_A, "120", "2", "0", &a.to_string()),
        line_at(MATERIAL_A, "80", "3", "0", &b.to_string()),
    ])
    .await;
    post_new(vec![line(MATERIAL_A, "200", "4", "0")]).await; // NULL 桶
    post_new(vec![line_at(MATERIAL_A, "50", "5", "0", &a.to_string())]).await;

    let loc = location_qty_sum(MATERIAL_A).await;
    let cost = cost_qty_sum(MATERIAL_A).await;
    assert_eq!(loc, dec("450"), "120 + 80 + 200 + 50");
    assert_eq!(
        loc, cost,
        "不变式③：SUM(库位账) 必须等于成本账 qty（{loc} vs {cost}）"
    );
    // 成本金额仍按移动加权走（120×2 + 80×3 + 200×4 + 50×5 = 1490）
    let (_, amount, _) = inventory_of(MATERIAL_A).await;
    // NULL 桶那一行的金额 = 这一桶自己的加法（200×4 = 800）
    assert_eq!(amount, dec("800"), "NULL 桶的成本账金额");
}

/// ⑪（P6）事务原子性：库位账写入失败 → 成本账**一起回滚**。
///
/// 造法：明细的 `location_id` 指向**别的物料**的库位账行。建单只校验「行存在 + 仓库一致」
/// （P2-B1 的既有口径），过账时 [`stockin_model::apply_location_qty`] 才会发现物料不一致 →
/// 这一步在 `apply_cost` 之后报错，正是「成本账已写、库位账写不进」的那一瞬。
#[tokio::test]
async fn location_failure_rolls_back_cost_account() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 别的物料的库位账行（同一个仓，所以建单校验能过）
    let other = seed_location_row(MATERIAL_B, LOC_DICT_A).await;
    let other_before = location_qty_of(&other).await;

    let id = create_stock_in(
        &req(vec![line_at(
            MATERIAL_A,
            "15",
            "2",
            "0",
            &other.to_string(),
        )]),
        None,
    )
    .await
    .expect("建单应通过（只校验库位行存在且同仓）");
    let err = change_stock_in_status(id, &post_req(), None)
        .await
        .expect_err("过账必须被拒：库位账行属于另一个物料");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("不一致"), "{}", err.msg);

    // 成本账、库位账、流水、单据状态 —— 四样都必须回到过账前
    assert_eq!(
        cost_qty_sum(MATERIAL_A).await,
        Decimal::ZERO,
        "成本账必须一起回滚（不能被先写进去）"
    );
    assert_eq!(location_row_count(MATERIAL_A).await, 0);
    assert_eq!(
        location_qty_of(&other).await,
        other_before,
        "别人的库位账行一个字都不能动"
    );
    let txns: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(txns, 0, "流水也不能留下");
    let status = stockin_model::find_stock_in(id)
        .await
        .unwrap()
        .expect("单据还在")
        .status;
    assert_eq!(status, STATUS_DRAFT, "状态必须还是 DRAFT（可重试）");
}

/// ⑫（P6）并发收货同一物料 → 不死锁、不丢数、库位账仍是一行。
#[tokio::test]
async fn concurrent_receipts_share_one_location_row() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let a = create_stock_in(&req(vec![line(MATERIAL_A, "10", "2", "0")]), None)
        .await
        .unwrap();
    let b = create_stock_in(&req(vec![line(MATERIAL_A, "20", "2", "0")]), None)
        .await
        .unwrap();
    let post_request = post_req();
    let (ra, rb) = tokio::join!(
        change_stock_in_status(a, &post_request, None),
        change_stock_in_status(b, &post_request, None)
    );
    ra.unwrap_or_else(|err| panic!("并发过账 A 失败（死锁？）：{} {}", err.code, err.msg));
    rb.unwrap_or_else(|err| panic!("并发过账 B 失败（死锁？）：{} {}", err.code, err.msg));

    assert_eq!(
        null_bucket_qty(MATERIAL_A).await,
        Some(dec("30")),
        "10 + 20，库位账一笔都不能丢"
    );
    assert_eq!(
        location_row_count(MATERIAL_A).await,
        1,
        "并发下 ON CONFLICT 必须把它们并到一行"
    );
    assert_eq!(cost_qty_sum(MATERIAL_A).await, dec("30"));
    assert_eq!(location_qty_sum(MATERIAL_A).await, dec("30"));
}

/// ⑬（P6）已有库位账行时 → **UPDATE** 而非 INSERT 冲突（行数与主键都不变）。
#[tokio::test]
async fn existing_location_row_is_updated_not_reinserted() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 先手造一行「已有库存」的库位账（模拟历史库位库存 500）
    let row = seed_location_row(MATERIAL_A, LOC_DICT_B).await;
    db().execute_with_params(
        "UPDATE biz_inventory_location SET qty = 500, locked_qty = 5 WHERE id = ?::uuid",
        vec![Value::from(&row)],
    )
    .await
    .unwrap();

    post_new(vec![line_at(MATERIAL_A, "80", "2", "0", &row.to_string())]).await;

    assert_eq!(
        location_qty_of(&row).await,
        Some(dec("580")),
        "必须在原行上累加（500 + 80）"
    );
    let (_, locked) = location_row_of(&row).await.unwrap();
    assert_eq!(locked, dec("5"), "已有 locked_qty 不能被收货重置");
    assert_eq!(location_row_count(MATERIAL_A).await, 1);
    // 主键没被 upsert 换掉
    let still: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM biz_inventory_location WHERE id = ?::uuid",
            vec![Value::from(&row)],
        )
        .await
        .unwrap();
    assert_eq!(still, 1, "行主键不能变（序列号/调拨都硬引用它）");
}

/// ⑭（P6）指针校验：`location_id` 指向**别的物料**的库位账行 → 过账 400 并说明原因。
///
/// 与 ⑪ 同一路径的两侧口径（⑪ 证回滚，本条证「拒绝 + 报文」）：400 + 原文，
/// 不能折成 500「服务器内部错误」。
#[tokio::test]
async fn location_row_of_another_material_is_rejected_with_reason() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let other = seed_location_row(MATERIAL_B, LOC_DICT_A).await;
    let id = create_stock_in(
        &req(vec![line_at(MATERIAL_A, "5", "1", "0", &other.to_string())]),
        None,
    )
    .await
    .unwrap();
    let err = change_stock_in_status(id, &post_req(), None)
        .await
        .expect_err("必须拒绝：库位行属于另一个物料");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(
        err.msg.contains("物料") && err.msg.contains("不一致"),
        "报文要点名物料不一致：{}",
        err.msg
    );
    assert!(
        !err.msg.contains("服务器内部错误"),
        "必须是可读的业务报文：{}",
        err.msg
    );
}

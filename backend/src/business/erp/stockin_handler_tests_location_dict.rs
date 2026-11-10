// ============================================================================
// stockin_handler_tests_location_dict.rs —— P2-4「库位做完整」真库用例
//
// 覆盖卡面 §三 的验收项：
//   ① 指定库位（`biz_location.id`）→ 过账把数量写到那个库位账行，未分配桶不动；
//   ② 不指定 → 自动用仓库默认库位（`is_default = 1`）；
//   ③ 仓库没有默认库位 → 进「未分配」桶，并在详情里给出 `location_warnings` 提示；
//   ④ 跨仓库 / 停用 / 已软删的库位一律拒在门口；
//   ⑤ 改单可以换库位（草稿态），过账按新库位落账；
//   ⑥ `location_id`（旧口径：库位账行）与 `location_dict_id`（新口径：库位字典）
//      指向不同库位 → 400（不许「用户选 A、账落 B」）；
//   ⑦ 容量超限只提示不拦截（`location_warnings` 里有话，账照样过）；
//   ⑧ 出库未指定库位 → FIFO（最早建账的库位先出），指定库位 → 只扣那一桶。
//
// ⚠ `ERP_DB_URL` 未设置时全部 SKIP（假绿判据：跑完总耗时一秒 vs 几分钟）。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_tests_fixtures::*;
use super::stockin_handler_tests_helpers::*;
use super::*;
use crate::business::erp::stockin_model;
use crate::business::erp::stockout_dto::{CreateStockOutReq, StockOutItemReq};
use crate::business::erp::stockout_handler;

// ==================== 小工具 ====================

fn uid(raw: &str) -> Uuid {
    Uuid::parse_str(raw).expect("夹具 UUID 非法")
}

/// 该物料在该仓**某库位字典**上的库位账数量（行不存在 → 0）。
async fn qty_at(material: &str, location_dict: &str) -> Decimal {
    db().scalar_with_params::<Option<Decimal>>(
        "SELECT qty FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id = ?::uuid",
        vec![
            Value::from(&uid(material)),
            Value::from(&uid(WAREHOUSE)),
            Value::from(&uid(location_dict)),
        ],
    )
    .await
    .expect("查库位账数量")
    .unwrap_or(Decimal::ZERO)
}

/// 该物料在该仓某库位字典上的库位账行 id（行不存在 → `None`）。
async fn row_at(material: &str, location_dict: &str) -> Option<Uuid> {
    stockin_model::ledger_row_of_location(uid(material), uid(WAREHOUSE), uid(location_dict))
        .await
        .expect("查库位账行")
}

/// 把某库位临时设成该仓默认（cleanup 每轮复位）。
async fn set_default(location_dict: &str) {
    sql(&format!(
        "UPDATE biz_location SET is_default = 1 WHERE id = '{location_dict}'::uuid"
    ))
    .await;
}

/// 建一张出库单（`location_row = None` → 走 FIFO）。
async fn out_doc(material: &str, qty: &str, location_row: Option<Uuid>) -> Uuid {
    let req = CreateStockOutReq {
        out_type: Some("ISSUE".to_string()),
        warehouse_id: Some(WAREHOUSE.to_string()),
        out_date: None,
        reason: Some("P2-4 出库库位用例".to_string()),
        remark: None,
        cost_object_id: None,
        items: vec![StockOutItemReq {
            material_id: material.to_string(),
            qty: dec(qty),
            location_id: location_row.map(|id| id.to_string()),
            lot_no: None,
            remark: None,
        }],
    };
    stockout_handler::create_stock_out(&req, None)
        .await
        .unwrap_or_else(|err| panic!("建出库单失败：{} {}", err.code, err.msg))
}

// ==================== ① 指定库位 → 账写到那个库位 ====================

/// 指定库位 A → 过账后 A 有 200，「未分配」桶**根本没有行**。
///
/// 这是卡面最核心的一条：改之前收货只能进未分配桶（明细 0/1 填了库位）。
#[tokio::test]
async fn p24_receipt_with_dict_location_writes_that_location() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 开局：该库位连账行都没有（证明「不需要预先有库存账行」）
    assert_eq!(row_at(MATERIAL_A, LOC_DICT_A).await, None);

    let id = create_stock_in(&req(vec![line_dict(MATERIAL_A, "200", "2", "0", LOC_DICT_A)]), None)
        .await
        .expect("建单：指定库位应通过");

    // 建单期就存了「用户意图」，并回读成编码（前端下拉回显用）
    let draft = detail_of(id).await.expect("详情");
    assert_eq!(
        draft.items[0].location_dict_id.as_deref(),
        Some(LOC_DICT_A),
        "明细必须记住用户选的库位字典"
    );
    assert_eq!(
        draft.items[0].location_code.as_deref(),
        Some("P2B1-L01"),
        "详情应把库位译成编码"
    );
    assert!(draft.location_warnings.is_empty(), "指定了库位就不该有提示");
    // 草稿态**不建**库位账行（免得「库位库存」Tab 被 0 数量的空行撑爆）
    assert_eq!(row_at(MATERIAL_A, LOC_DICT_A).await, None);

    change_stock_in_status(id, &post_req(), None).await.expect("过账");

    assert_eq!(
        qty_at(MATERIAL_A, LOC_DICT_A).await,
        dec("200"),
        "数量必须落在指定的库位"
    );
    assert_eq!(null_bucket_qty(MATERIAL_A).await, None, "「未分配」桶不该被创建");
    // 明细回写了库位账行 id（作废 / 红冲按它原路退回）
    let row = row_at(MATERIAL_A, LOC_DICT_A).await.expect("库位账行");
    let detail = detail_of(id).await.expect("过账后详情");
    assert_eq!(detail.items[0].location_id, Some(row.to_string()));
    println!("[P2-4] 指定库位收货：{} 落在 {}", 200, LOC_DICT_A);
}

// ==================== ② 不指定 → 用仓库默认库位 ====================

/// 仓库有默认库位时，明细不传库位 → 自动带出默认库位，过账写进它（不是未分配桶）。
#[tokio::test]
async fn p24_receipt_without_location_uses_default() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    set_default(LOC_DEFAULT).await;

    let id = create_stock_in(&req(vec![line(MATERIAL_A, "100", "2", "0")]), None)
        .await
        .expect("建单：不传库位应通过（默认库位兜底）");
    let draft = detail_of(id).await.expect("详情");
    assert_eq!(
        draft.items[0].location_dict_id.as_deref(),
        Some(LOC_DEFAULT),
        "不传库位时必须带出仓库默认库位"
    );
    assert!(draft.location_warnings.is_empty(), "有默认库位就不该提示未分配");

    change_stock_in_status(id, &post_req(), None).await.expect("过账");
    assert_eq!(
        qty_at(MATERIAL_A, LOC_DEFAULT).await,
        dec("100"),
        "默认库位必须收到 100"
    );
    assert_eq!(null_bucket_qty(MATERIAL_A).await, None, "有默认库位时不该再落未分配桶");
}

// ==================== ③ 无默认库位 → 未分配桶 + 提示 ====================

/// 仓库没有默认库位、明细也不指定 → 进「未分配」桶，同时详情给出可读提示。
#[tokio::test]
async fn p24_receipt_without_default_falls_back_to_unassigned_with_warning() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 夹具里没有默认库位（cleanup 每轮把 LOC_DEFAULT 复位成 0）
    let id = create_stock_in(&req(vec![line(MATERIAL_A, "50", "2", "0")]), None)
        .await
        .expect("建单：无默认库位也应放行（进未分配桶）");
    let draft = detail_of(id).await.expect("详情");
    assert_eq!(draft.items[0].location_dict_id, None, "没有默认库位就留空");
    assert_eq!(draft.location_warnings.len(), 1, "必须有一条提示");
    assert!(
        draft.location_warnings[0].contains("未分配"),
        "提示必须点名未分配桶：{}",
        draft.location_warnings[0]
    );

    change_stock_in_status(id, &post_req(), None).await.expect("过账");
    assert_eq!(
        null_bucket_qty(MATERIAL_A).await,
        Some(dec("50")),
        "无默认库位时进未分配桶（与改之前一致）"
    );
    // 过账后提示措辞改成「已记在未分配桶」
    let detail = detail_of(id).await.expect("过账后详情");
    assert!(
        detail.location_warnings[0].contains("未分配"),
        "过账后仍要能看到这条事实"
    );
}

// ==================== ④ 跨仓库 / 停用 / 已删 库位被拒 ====================

/// 库位属于另一个仓库 → 400（点名两个仓库），单据不落库。
#[tokio::test]
async fn p24_cross_warehouse_location_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let err = create_stock_in(
        &req(vec![line_dict(MATERIAL_A, "10", "2", "0", LOC_IN_WAREHOUSE_B)]),
        None,
    )
    .await
    .expect_err("跨仓库位必须被拒");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("不一致"), "文案要点名仓库不一致：{}", err.msg);
    assert_eq!(null_bucket_qty(MATERIAL_A).await, None, "被拒的单不能动账");
}

/// 停用库位 → 400「已停用」；已软删库位 → 400「已被删除」。
#[tokio::test]
async fn p24_disabled_and_deleted_location_are_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    for (location, keyword) in [(LOC_DISABLED, "已停用"), (LOC_DELETED, "已被删除")] {
        let err = create_stock_in(&req(vec![line_dict(MATERIAL_A, "10", "2", "0", location)]), None)
            .await
            .expect_err(&format!("库位 {location} 必须被拒"));
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(err.msg.contains(keyword), "文案要含「{keyword}」：{}", err.msg);
    }
}

// ==================== ⑤ 改单换库位 ====================

/// 草稿单把库位从 A 改成 B → 过账落到 B，A 一行都不建。
#[tokio::test]
async fn p24_update_draft_can_move_location() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = create_stock_in(&req(vec![line_dict(MATERIAL_A, "30", "2", "0", LOC_DICT_A)]), None)
        .await
        .expect("建单");

    let mut request = req(vec![line_dict(MATERIAL_A, "30", "2", "0", LOC_DICT_B)]);
    request.warehouse_id = Some(WAREHOUSE.to_string());
    update_stock_in(id, &request, None).await.expect("改单");

    let draft = detail_of(id).await.expect("详情");
    assert_eq!(draft.items[0].location_dict_id.as_deref(), Some(LOC_DICT_B));

    change_stock_in_status(id, &post_req(), None).await.expect("过账");
    assert_eq!(qty_at(MATERIAL_A, LOC_DICT_B).await, dec("30"));
    assert_eq!(row_at(MATERIAL_A, LOC_DICT_A).await, None, "改单后原库位不该建账");
}

// ==================== ⑥ 两个口径打架 → 400 ====================

/// `location_id`（库位账行 B）与 `location_dict_id`（库位字典 A）指向不同库位 → 400。
#[tokio::test]
async fn p24_conflicting_dict_and_ledger_row_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row_b = seed_location_row(MATERIAL_A, LOC_DICT_B).await;

    let mut bad = line_dict(MATERIAL_A, "10", "2", "0", LOC_DICT_A);
    bad.location_id = Some(row_b.to_string());
    let err = create_stock_in(&req(vec![bad]), None)
        .await
        .expect_err("两个库位口径打架必须被拒");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("不同的库位"), "文案要点名冲突：{}", err.msg);
}

// ==================== ⑦ 容量只提示不拦截 ====================

/// 库位容量 50、本次收 80 → 详情给出「超出容量」提示，**过账照常成功**。
#[tokio::test]
async fn p24_capacity_overflow_is_warning_only() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = create_stock_in(&req(vec![line_dict(MATERIAL_A, "80", "2", "0", LOC_CAP)]), None)
        .await
        .expect("建单");
    let draft = detail_of(id).await.expect("详情");
    assert!(
        draft.location_warnings.iter().any(|text| text.contains("超出容量")),
        "容量 50 收 80 必须给出超容量提示：{:?}",
        draft.location_warnings
    );

    change_stock_in_status(id, &post_req(), None)
        .await
        .expect("容量只是提示：过账必须放行");
    assert_eq!(qty_at(MATERIAL_A, LOC_CAP).await, dec("80"), "提示不拦截：账照样落");
}

// ==================== ⑧ 出库：FIFO / 指定 ====================

/// 先进 A（200）、后进 B（100）→ 不指定库位出 50 → 扣**最早建账**的 A。
#[tokio::test]
async fn p24_stockout_without_location_deducts_fifo() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    post_into(LOC_DICT_A, "200").await;
    post_into(LOC_DICT_B, "100").await;
    let row_a = row_at(MATERIAL_A, LOC_DICT_A).await.expect("A 有账行");
    let row_b = row_at(MATERIAL_A, LOC_DICT_B).await.expect("B 有账行");
    // 把先后次序钉死（不靠两个事务的时间戳碰运气）
    sql(&format!(
        "UPDATE biz_inventory_location SET created_at = CURRENT_TIMESTAMP - interval '2 hour' \
          WHERE id = '{row_a}'::uuid"
    ))
    .await;

    let doc = out_doc(MATERIAL_A, "50", None).await;
    stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("FIFO 出库应成功");

    assert_eq!(
        qty_at(MATERIAL_A, LOC_DICT_A).await,
        dec("150"),
        "FIFO：先扣最早建账的 A"
    );
    assert_eq!(qty_at(MATERIAL_A, LOC_DICT_B).await, dec("100"), "后排的 B 不动");
    // 明细回写了 FIFO 选中的库位账行（作废时按它原路退回）
    let item_location: Option<Uuid> = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT location_id FROM biz_stock_out_item WHERE stock_out_id = ?::uuid",
            vec![Value::from(&doc)],
        )
        .await
        .expect("查出库明细");
    assert_eq!(item_location, Some(row_a), "明细必须记下 FIFO 选中的库位");
    assert_ne!(item_location, Some(row_b));
}

/// 指定库位 B 出 40 → 只扣 B，A 原样不动。
#[tokio::test]
async fn p24_stockout_with_location_deducts_that_one() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    post_into(LOC_DICT_A, "200").await;
    post_into(LOC_DICT_B, "100").await;
    let row_b = row_at(MATERIAL_A, LOC_DICT_B).await.expect("B 有账行");

    let doc = out_doc(MATERIAL_A, "40", Some(row_b)).await;
    stockout_handler::post_stock_out(doc, None, None)
        .await
        .expect("指定库位出库应成功");

    assert_eq!(qty_at(MATERIAL_A, LOC_DICT_B).await, dec("60"), "B 只剩 60");
    assert_eq!(
        qty_at(MATERIAL_A, LOC_DICT_A).await,
        dec("200"),
        "指定库位出库：A 一克不动"
    );
}

/// 收货进某库位并过账（FIFO 用例的前置）。
async fn post_into(location_dict: &str, qty: &str) {
    post_new(vec![line_dict(MATERIAL_A, qty, "2", "0", location_dict)]).await;
}

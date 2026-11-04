// ============================================================================
// stockin_handler_tests_a4.rs —— A4：供应商来源派生与快照列
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_tests_fixtures::*;
use super::stockin_handler_tests_helpers::*;
use super::*;
use crate::business::erp::stockin_dto::{CreateStockInReq, StockInPageQuery};
use crate::business::erp::stockin_model::{self};
use crate::shared::error_code;

/// ① + ⑥ + ⑨：有源单 → 创建成功，供应商**从采购单派生**（请求里根本没给），
/// 且明细 `source_item_id` 是从采购单带出的。
#[tokio::test]
async fn a4_01_purchase_with_source_derives_supplier() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;

    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "4", None), None)
        .await
        .unwrap_or_else(|err| panic!("有源单应当建得出来：{} {}", err.code, err.msg));

    let row = stockin_model::find_stock_in(id)
        .await
        .unwrap()
        .expect("单头");
    assert_eq!(
        row.supplier_id,
        Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap()),
        "供应商必须从采购单派生（请求里没传 supplier_id）"
    );
    assert_eq!(row.source_order_id, Some(order_id), "主关联 = 采购单");
    assert!(!row.is_sourceless, "有源单不能被打成无源");

    let items = stockin_model::items_of(id).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].source_item_id,
        Some(item_id),
        "明细的来源行必须是从采购单带出的 source_item_id（只绑上一环）"
    );
    assert_eq!(items[0].qty, dec("4"));
}

/// ②：PURCHASE 型却没绑采购单、也没声明无源 → 400 + 66027。
///
/// 这条是 AP-4 的机械保障：入库绑的是**采购单**，不是供应商 ——
/// 客户端再也不能「填个供应商就建单」。
#[tokio::test]
async fn a4_02_purchase_without_source_is_400() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let payload = CreateStockInReq {
        supplier_id: Some(SUPPLIER.to_string()),
        is_sourceless: None,
        warehouse_id: Some(WAREHOUSE.to_string()),
        items: vec![line(MATERIAL_A, "1", "2", "0")],
        ..Default::default()
    };
    let err = create_stock_in(&payload, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SI_SOURCE_REQUIRED);
    assert!(
        err.msg.contains("来源采购单") && err.msg.contains("is_sourceless"),
        "文案要说清「要么绑采购单、要么显式声明无源」：{}",
        err.msg
    );
}

/// ③：来源采购单不存在 / 已软删 → 400（66002 引用缺失）。
#[tokio::test]
async fn a4_03_source_order_missing_is_400() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    db().execute_with_params(
        "UPDATE biz_purchase_order SET deleted_at = now() WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("软删采购单");

    let err = create_stock_in(&a4_sourced_req(order_id, item_id, "1", None), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_REFERENCE_MISSING);
    assert!(err.msg.contains("采购单不存在或已删除"), "{}", err.msg);
}

/// ④：采购单状态不允许收货（DRAFT / CANCELLED / CLOSED）→ 400。
///
/// ⚠️ 与卡面的**一处口径差异**（已写进完成说明）：卡面写「状态允许收货
/// （APPROVED / PARTIAL_RECEIVED）」，本仓 `purchase_receivable` 的白名单是
/// `SUBMITTED | APPROVED | PARTIAL_RECEIVED`（D4 定稿；ERP-B2 的内控开关默认关，
/// SUBMITTED 是「还没走审批」的常态）。把 SUBMITTED 判死会让「建单 → 收货」这条主链路
/// 在默认配置下直接断掉，所以本卡**保留** SUBMITTED 可收，只把终态 / 草稿态判死。
#[tokio::test]
async fn a4_04_source_order_not_receivable_is_400() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    for status in ["DRAFT", "CANCELLED", "CLOSED"] {
        let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", status).await;
        let err = create_stock_in(&a4_sourced_req(order_id, item_id, "1", None), None)
            .await
            .err()
            .unwrap_or_else(|| panic!("采购单状态 {status} 不允许收货，却建单成功了"));
        assert_eq!(err.status, 400, "{}", err.msg);
        assert!(
            err.msg.contains("不能收货"),
            "文案要说清「这张采购单现在不能收货」：{}",
            err.msg
        );
    }
}

/// ⑤：客户端传的供应商 ≠ 采购单供应商 → 400 + 66028（**不静默覆盖**）。
/// 传相同的值 → 幂等放行（不是错误）。
#[tokio::test]
async fn a4_05_client_supplier_mismatch_is_400() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;

    let err = create_stock_in(
        &a4_sourced_req(order_id, item_id, "1", Some(SUPPLIER)),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SI_SUPPLIER_MISMATCH);
    assert!(
        err.msg.contains("由采购单带出") && err.msg.contains("不允许手工填写"),
        "文案要说清「从采购单带出、别手填」：{}",
        err.msg
    );

    // 传对（与采购单一致）→ 幂等放行，落库仍是采购单那边的值
    let id = create_stock_in(
        &a4_sourced_req(order_id, item_id, "1", Some(A4_SUPPLIER_B)),
        None,
    )
    .await
    .unwrap_or_else(|err| panic!("传对供应商不该报错：{} {}", err.code, err.msg));
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert_eq!(
        row.supplier_id,
        Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap())
    );
}

/// ⑦ + ⑧：无源单据必须**显式**打标记 + 写原因；有源单不许标无源。
#[tokio::test]
async fn a4_06_sourceless_requires_flag_and_reason() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // ① 无源 + 没有标记 → 400（走的是「必须绑采购单」那条）
    let no_flag = CreateStockInReq {
        supplier_id: Some(SUPPLIER.to_string()),
        warehouse_id: Some(WAREHOUSE.to_string()),
        items: vec![line(MATERIAL_A, "1", "2", "0")],
        ..Default::default()
    };
    let err = create_stock_in(&no_flag, None).await.unwrap_err();
    assert_eq!(err.code, error_code::ERP_SI_SOURCE_REQUIRED, "{}", err.msg);

    // ② 无源 + 有标记但没写原因 → 400（AP-5：无源要说清「哪来的」）
    let no_reason = CreateStockInReq {
        supplier_id: Some(SUPPLIER.to_string()),
        is_sourceless: Some(true),
        warehouse_id: Some(WAREHOUSE.to_string()),
        items: vec![line(MATERIAL_A, "1", "2", "0")],
        ..Default::default()
    };
    let err = create_stock_in(&no_reason, None).await.unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SI_SOURCELESS_CONFLICT);
    assert!(err.msg.contains("remark"), "{}", err.msg);

    // ③ 无源 + 标记 + 原因 → 成功，落库 is_sourceless = true
    let ok = CreateStockInReq {
        remark: Some("期初补录：2026-10 前的手工库存".to_string()),
        ..no_reason
    };
    let id = create_stock_in(&ok, None)
        .await
        .unwrap_or_else(|err| panic!("无源单应当建得出来：{} {}", err.code, err.msg));
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert!(row.is_sourceless, "无源标记必须落库");
    assert_eq!(row.source_order_id, None);
    assert_eq!(row.supplier_id, Some(Uuid::parse_str(SUPPLIER).unwrap()));

    // ④ 有源单却声明无源 → 以**来源单为准**归一化（不拒单、也不静默改供应商）：
    //    标记本来就等价于「没有来源单」，多带的 true 是冗余而非谎言，落库仍是
    //    is_sourceless = false + 供应商来自采购单。这条把「归一化」钉成显式行为，
    //    免得将来有人把它改成「静默信任客户端的标记」。
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let mut mixed = a4_sourced_req(order_id, item_id, "1", None);
    mixed.is_sourceless = Some(true);
    mixed.remark = Some("自相矛盾的请求".to_string());
    let id = create_stock_in(&mixed, None)
        .await
        .unwrap_or_else(|err| panic!("有源单多带无源标记不该拒单：{} {}", err.code, err.msg));
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert!(!row.is_sourceless, "有源单落库必须是无源 = false");
    assert_eq!(
        row.supplier_id,
        Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap()),
        "供应商仍取自采购单"
    );
}

/// ⑦ + ⑧（DB 层）：非 PURCHASE 型入库单带着供应商 → 撞 `ck_biz_si_supplier_scope`；
/// 无源却不打标记 → 撞 `ck_biz_si_sourceless_pair`。
///
/// 为什么用裸 SQL 而不是 DTO：`create` 路径把 `in_type` 硬编码成 `'PURCHASE'`，
/// 接口层根本造不出 `RETURN` / `OTHER`（A4 不扩枚举，设计 §4.4 要求「未来新增枚举值」
/// 而不是放宽现有约束）。这两条约束必须由 DB 兜住，才挡得住绕过接口的写入。
#[tokio::test]
async fn a4_07_non_purchase_with_supplier_db_rejects() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let insert = |in_type: &'static str, sourceless: bool, with_supplier: bool| {
        let supplier = if with_supplier {
            format!("'{SUPPLIER}'::uuid")
        } else {
            "NULL".to_string()
        };
        format!(
            "INSERT INTO biz_stock_in (id, in_no, in_type, supplier_id, warehouse_id, in_date, \
                 status, total_qty, total_amount, total_tax, total_with_tax, is_sourceless) \
             VALUES (gen_random_uuid(), 'B1-A4-NEG-{in_type}-{sourceless}-{with_supplier}', \
                 '{in_type}', {supplier}, '{WAREHOUSE}'::uuid, CURRENT_DATE, 'DRAFT', \
                 0, 0, 0, 0, {sourceless})"
        )
    };

    // ① 非 PURCHASE 带供应商 → 拒（A3 的 ck_biz_si_supplier_scope）
    let err = db()
        .execute_with_params(&insert("RETURN", true, true), vec![])
        .await
        .expect_err("非采购入库带供应商必须被 DB 拒");
    assert!(
        format!("{err}").contains("ck_biz_si_supplier_scope"),
        "应当撞 ck_biz_si_supplier_scope：{err}"
    );

    // ② 无源却不打标记（任何 in_type）→ 拒（本卡的 ck_biz_si_sourceless_pair）
    let err = db()
        .execute_with_params(&insert("OTHER", false, false), vec![])
        .await
        .expect_err("无源单据不打标记必须被 DB 拒");
    assert!(
        format!("{err}").contains("ck_biz_si_sourceless_pair"),
        "应当撞 ck_biz_si_sourceless_pair：{err}"
    );

    // ③ 一行都不许落库
    let left: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'B1-A4-NEG-%'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(left, 0, "反例一行都不该落库");
}

/// ⑩：超收（本次数量 > 采购单待收）→ 400（D4 的老校验还在）。
#[tokio::test]
async fn a4_08_over_receipt_is_400() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "5", "SUBMITTED").await;

    // ① 一次就要 6 件（采购单只有 5）→ 400
    let err = create_stock_in(&a4_sourced_req(order_id, item_id, "6", None), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(
        err.msg.contains("未收") && err.msg.contains("件"),
        "文案要说清本次 / 待收 / 已收 / 订单：{}",
        err.msg
    );

    // ② 先收 3 并**过账**（待收 5 → 2），再要 3 → 400（待收是按已过账数量算的）
    let first = create_stock_in(&a4_sourced_req(order_id, item_id, "3", None), None)
        .await
        .expect("收 3 件合法");
    change_stock_in_status(first, &post_req(), None)
        .await
        .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
    let err = create_stock_in(&a4_sourced_req(order_id, item_id, "3", None), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(
        err.msg.contains("未收"),
        "过账后待收只剩 2 件，再收 3 件必须被拒：{}",
        err.msg
    );
}

/// ⑪：列表下发 `source_order_no` 快照 + `supplier_name`（读主数据）+ `is_sourceless`；
/// 详情另外下发**一跳带出**的合同号（不 JOIN 合同表）。
#[tokio::test]
async fn a4_09_list_and_detail_expose_snapshot_columns() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
        .await
        .expect("建单");
    let po_no = stockin_model::order_no_of(order_id)
        .await
        .unwrap()
        .expect("采购单号");

    let page = stock_in_page(Query(StockInPageQuery {
        supplier_id: Some(A4_SUPPLIER_B.to_string()),
        page: 1,
        page_size: 20,
        ..Default::default()
    }))
    .await
    .expect("列表应能查");
    let list = page.data.expect("有数据").list;
    assert_eq!(list.len(), 1, "本供应商下就这一张单");
    assert_eq!(
        list[0].source_order_no.as_deref(),
        Some(po_no.as_str()),
        "列表必须下发来源采购单号快照"
    );
    assert_eq!(
        list[0].supplier_name.as_deref(),
        Some("A4 二号供应商"),
        "供应商名读 supplier_id 快照关联主数据（不是实时 JOIN 采购单）"
    );
    assert!(!list[0].is_sourceless);

    let detail = detail_of(id).await.expect("详情");
    assert_eq!(
        detail.header.supplier_name.as_deref(),
        Some("A4 二号供应商")
    );
    assert_eq!(detail.purchase_order_no.as_deref(), Some(po_no.as_str()));
    // 这张采购单是 SPOT（没挂合同）→ 一跳带出为空，且**绝不**去 JOIN biz_contract
    assert_eq!(detail.contract_no, None);
}

/// 过账前复校（设计 §7.2）：建单后有人直接改库把供应商换掉 → 过账 400，
/// 库存一分没动（应付单取的就是单据头供应商，放过就是记错账）。
#[tokio::test]
async fn a4_10_posting_rechecks_supplier() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
        .await
        .expect("建单");

    // 模拟「绕过接口直接改库」
    db().execute_with_params(
        "UPDATE biz_stock_in SET supplier_id = ?::uuid WHERE id = ?::uuid",
        vec![
            Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
            Value::from(&id),
        ],
    )
    .await
    .expect("模拟改库");

    let err = change_stock_in_status(id, &post_req(), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SI_SUPPLIER_MISMATCH);
    assert!(err.msg.contains("直接改库"), "{}", err.msg);
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (dec("0"), dec("0"), dec("0")),
        "复校拦下后库存必须一分没动"
    );
}

/// 全链路（卡面 §5）：有源建单 → 过账（库存 + 流水 + 凭证 + 采购回写 + 状态推进）→
/// 作废（账与采购数量全部回基线）。这条就是 A4 最怕被改坏的链。
#[tokio::test]
async fn a4_11_post_and_cancel_keep_purchase_chain() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "6", None), None)
        .await
        .expect("建单");

    change_stock_in_status(id, &post_req(), None)
        .await
        .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));

    // ① 库存增加
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (dec("6"), dec("12"), dec("2.000000")),
        "过账后库存 6 件 / 12 元 / 单价 2"
    );
    // ② 流水 1 条（PURCHASE_IN）
    assert_eq!(txn_count(id, "PURCHASE_IN").await, 1, "过账写一条入库流水");
    // ③ 凭证生成（1 张，非红冲）
    assert_eq!(voucher_count(id, None).await, 1, "过账生成 1 张凭证");
    assert_eq!(voucher_count(id, Some(1)).await, 0, "此刻不该有红冲凭证");
    // ④ 采购明细已收数量 + 采购单状态推进（部分收货）
    //    P2-3：已收数量是派生值（只算已过账收货单），走生产读路径断言
    assert_eq!(
        crate::business::erp::model::purchase_order_item_find_by_id(item_id)
            .await
            .unwrap()
            .expect("采购明细行必须存在")
            .received_qty,
        dec("6"),
        "过账后采购明细的已收数量（派生）必须 = 本次过账数量"
    );
    assert_eq!(a4_po_status(order_id).await, "PARTIAL_RECEIVED");

    // 作废 → 账与采购数量全部回基线（ERP 铁律）
    cancel(id, "A4 全链路：作废回基线").await;
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (dec("0"), dec("0"), dec("0")),
        "作废后库存必须回基线"
    );
    assert_eq!(
        crate::business::erp::model::purchase_order_item_find_by_id(item_id)
            .await
            .unwrap()
            .expect("采购明细行必须存在")
            .received_qty,
        dec("0"),
        "作废后已收数量必须回退（本单不再计入 POSTED 集合）"
    );
    assert_eq!(
        a4_po_status(order_id).await,
        "PARTIAL_RECEIVED",
        "退回后采购单回到「部分收货」（RECEIVED → PARTIAL_RECEIVED 是 D4 的合法边）"
    );
}

/// 复制为新单：`source_order_id` 被清空 ⇒ 必须打成无源，并在备注里留「复制自 IN-xxx」。
#[tokio::test]
async fn a4_12_copy_marks_sourceless() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let origin = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
        .await
        .expect("建单");
    let origin_no = stockin_model::find_stock_in(origin)
        .await
        .unwrap()
        .unwrap()
        .in_no;

    let copy_id = copy_stock_in(origin, &create_only()).await.expect("复制");
    let copy = stockin_model::find_stock_in(copy_id)
        .await
        .unwrap()
        .unwrap();
    assert!(copy.is_sourceless, "复制单没有来源单 ⇒ 必须是无源单");
    assert_eq!(copy.source_order_id, None);
    assert_eq!(
        copy.supplier_id,
        Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap())
    );
    assert!(
        copy.remark
            .as_deref()
            .unwrap_or_default()
            .contains(&origin_no),
        "备注要留「复制自 {origin_no}」：{:?}",
        copy.remark
    );
}

/// 草稿改单也不能把供应商单独换成别的（有源单只能跟着采购单走）。
#[tokio::test]
async fn a4_13_draft_update_cannot_hand_pick_supplier() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
        .await
        .expect("建单");

    let err = update_stock_in(
        id,
        &a4_sourced_req(order_id, item_id, "2", Some(SUPPLIER)),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SI_SUPPLIER_MISMATCH);
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert_eq!(
        row.supplier_id,
        Some(Uuid::parse_str(A4_SUPPLIER_B).unwrap()),
        "被拒之后库里的供应商一个字节都不许动"
    );
}

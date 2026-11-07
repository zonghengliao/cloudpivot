// ============================================================================
// stockin_handler_tests_posting.rs —— 过账主链：库存/序列号/成本/并发/重复过账
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
use crate::business::erp::stockin_model::{self, STATUS_DRAFT, STATUS_POSTED};
use crate::shared::error_code;

/// DC-09（P0 单位止血 · 收货单侧）：物料挂折算单位「箱」（1 箱 = 12 个）→ 400 + 折算建议，
/// 单头与明细**一行都不落库**（过账更是无从发生）。
///
/// 收货单是库存三本账的唯一入口：这里漏掉一行，账上就是「箱数当个数」。
#[tokio::test]
async fn non_base_unit_line_is_rejected() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    // 2 箱 = 24 个
    let error = create_stock_in(&req(vec![line(MATERIAL_BOX, "2", "2", "13")]), None)
        .await
        .unwrap_err();
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
    assert!(
        error.msg.contains(
            "单位「箱」不是基本单位：本版本不支持多单位换算，\
             请按基本单位录入（1 箱 = 12 个 → 录 24）"
        ),
        "{}",
        error.msg
    );
    assert!(error.msg.contains("items[0]"), "{}", error.msg);
    assert!(error.msg.contains("P2B1-M4-BOX"), "{}", error.msg);

    let rows = db()
        .find_with_params(
            "SELECT COUNT(*) AS cnt FROM biz_stock_in_item WHERE material_id = ?::uuid",
            vec![Value::from(
                &Uuid::parse_str(MATERIAL_BOX).expect("夹具 UUID"),
            )],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.first().map(|row| row.get_typed::<i64>("cnt").unwrap()),
        Some(0),
        "被拒的收货单不许留下明细行"
    );

    // 混填（基本单位 + 折算单位）整单拒：前一行也不能先落库
    let mixed = create_stock_in(
        &req(vec![
            line(MATERIAL_A, "1", "2", "13"),
            line(MATERIAL_BOX, "1", "2", "13"),
        ]),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(mixed.code, error_code::ERP_UNIT_NOT_BASE, "{}", mixed.msg);
    assert!(mixed.msg.contains("items[1]"), "{}", mixed.msg);

    // 基本单位路径不受影响（同一夹具、同一请求形状）
    create_stock_in(&req(vec![line(MATERIAL_A, "1", "2", "13")]), None)
        .await
        .unwrap();
}

/// ★ 本卡补口（DC-09 缺口 1 · 收货侧）：**过账**期按**明细快照的 `unit_id`** 复检单位。
///
/// 造法：先用基本单位建一张草稿（正常路径），再**绕过接口**把明细的 `unit_id` 改成折算单位
/// （`UPDATE biz_stock_in_item`）—— 这就是「升级前建的折算单位草稿 / 被 UPDATE 过的单」
/// 在库里的样子：物料本身没问题（建单期校验早跑完了），过账只认明细快照。
///
/// 断言四件事：`400 + 66030`、文案带折算建议、**三本账 / 凭证 / 应付 0 变化**、单据仍是草稿。
/// 最后把单位改回去，同一张单正常过账 —— 证明这道闸只拦该拦的。
#[tokio::test]
async fn posting_rejects_non_base_unit_snapshot() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = create_stock_in(&req(vec![line(MATERIAL_A, "2", "2", "13")]), None)
        .await
        .expect("建单");
    assert_eq!(
        posting_footprint(id).await,
        vec![0, 0, 0, 0, 0],
        "前提：只建单不动任何账"
    );

    // 绕过接口改明细单位：2 个 → 2 箱（= 24 个）
    db().execute_with_params(
        "UPDATE biz_stock_in_item SET unit_id = ?::uuid WHERE stock_in_id = ?::uuid",
        vec![
            Value::from(&Uuid::parse_str(UNIT_BOX).expect("夹具 UUID")),
            Value::from(&id),
        ],
    )
    .await
    .expect("改明细单位");

    let error = change_stock_in_status(id, &post_req(), None)
        .await
        .expect_err("明细单位是折算单位，不许过账");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
    assert!(
        error.msg.contains(
            "单位「箱」不是基本单位：本版本不支持多单位换算，\
             请按基本单位录入（1 箱 = 12 个 → 录 24）"
        ),
        "{}",
        error.msg
    );
    assert!(error.msg.contains("items[0]"), "{}", error.msg);
    assert!(
        error.msg.contains("P2B1-M1"),
        "物料编码要点出来（用户在收货单里照着找行）：{}",
        error.msg
    );

    // 三本账 / 凭证 / 应付 0 变化；单据仍是草稿（没被推成 POSTED）
    assert_eq!(
        posting_footprint(id).await,
        vec![0, 0, 0, 0, 0],
        "被拒的过账不许动任何一本账"
    );
    assert_eq!(
        detail_of(id).await.expect("回读").header.status,
        STATUS_DRAFT,
        "被拒的过账不能改单据状态"
    );

    // 明细单位改回基本单位 → 同一张单正常过账（正常路径回归绿）
    db().execute_with_params(
        "UPDATE biz_stock_in_item SET unit_id = ?::uuid WHERE stock_in_id = ?::uuid",
        vec![
            Value::from(&Uuid::parse_str(UNIT).expect("夹具 UUID")),
            Value::from(&id),
        ],
    )
    .await
    .expect("复位明细单位");
    change_stock_in_status(id, &post_req(), None)
        .await
        .expect("基本单位必须能过账");
    assert_eq!(
        posting_footprint(id).await,
        vec![1, 1, 1, 1, 1],
        "正常过账要动成本账 / 库位账 / 流水 / 凭证 / 应付"
    );
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("2"));
}

/// ① 首次入库：库存从无到有，qty / amount / avg_cost 正确。
#[tokio::test]
async fn first_receipt_creates_inventory_row() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
    );
    let id = post_new(vec![line(MATERIAL_A, "100", "2", "13")]).await;

    let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("100"));
    assert_eq!(
        amount,
        dec("200"),
        "入库金额必须是不含税 200，不是价税合计 226"
    );
    assert_eq!(avg, dec("2.000000"));

    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.header.status, STATUS_POSTED);
    assert!(detail.header.posted_at.is_some(), "过账必须写 posted_at");
    assert_eq!(detail.header.total_amount, dec("200"));
    assert_eq!(detail.header.total_tax, dec("26"));
    assert_eq!(detail.header.total_with_tax, dec("226"));
    assert!(detail.summary.contains("不含税"), "{}", detail.summary);
}

/// ② 二次入库：移动加权重算（PRD §3.2.5 算例 1：100@2 + 100@3 → 200 / 500 / 2.5）。
#[tokio::test]
async fn second_receipt_moves_weighted_average() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
    post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;
    let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("200"));
    assert_eq!(amount, dec("500"));
    assert_eq!(avg, dec("2.500000"), "500 / 200 = 2.5（PRD §3.2.5 算例 1）");
}

/// ②b 除不尽的尾差：10@1.11 + 3@3.33 → 13 / 21.09 / 1.622308（PRD §3.2.5 算例 2）。
#[tokio::test]
async fn rounding_dust_keeps_amount_authoritative() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    post_new(vec![line(MATERIAL_A, "10", "1.11", "0")]).await;
    post_new(vec![line(MATERIAL_A, "3", "3.33", "0")]).await;
    let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("13"));
    assert_eq!(amount, dec("21.09"), "金额是加法得出的权威值，不修尾差");
    assert_eq!(
        avg,
        dec("1.622308"),
        "21.09 / 13 = 1.62230769… → 6 位四舍五入"
    );
}

/// ③ 一物一码物料：生成对应数量的序列号，`warehouse_id` 正确（卡面 ③）。
#[tokio::test]
async fn serial_material_generates_serials_in_receipt_warehouse() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_SN, "5", "40", "13")]).await;
    assert_eq!(stockin_model::serial_count_of(id).await.unwrap(), 5);
    let rows = db()
        .find_with_params(
            "SELECT serial_no, warehouse_id, status, source_type, source_id \
               FROM biz_material_serial WHERE source_id = ?::uuid ORDER BY serial_no",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 5);
    for row in &rows {
        let serial_no: String = row.get_typed("serial_no").unwrap();
        assert!(serial_no.starts_with("SN-"), "{serial_no}");
        let warehouse: Uuid = row.get_typed("warehouse_id").unwrap();
        assert_eq!(warehouse.to_string(), WAREHOUSE, "序列号必须落在收货仓");
        let status: String = row.get_typed("status").unwrap();
        assert_eq!(status, "IN_STOCK");
        // FIX-7 口径：**无源入库（本用例没有采购单）不记来源类型** ⇒ `source_type` 为 NULL。
        // 有源（采购 / 退货）入库才是 `PURCHASE` / `RETURN`，见 `serial_source_of_in_type`。
        let source_type: Option<String> = row.get_typed("source_type").unwrap();
        assert!(
            source_type.is_none(),
            "无源入库的序列号不该记来源类型（实际 {source_type:?}）"
        );
        // P2-6b：来源 id 必须回指本收货单（写入点补 id 的验收点）
        let source_id: Option<Uuid> = row.get_typed("source_id").unwrap();
        assert_eq!(
            source_id,
            Some(id),
            "序列号必须回指本收货单（source_id 恒写，来源类型才是可空的）"
        );
    }
    // 账面数量与序列号件数一致（不变式①）
    let (qty, _, _) = inventory_of(MATERIAL_SN).await;
    assert_eq!(qty, dec("5"));
    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.serial_count, 5);
}

/// ④ 非一物一码：不生成序列号。
#[tokio::test]
async fn non_serial_material_generates_no_serials() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "7", "1.5", "0")]).await;
    assert_eq!(stockin_model::serial_count_of(id).await.unwrap(), 0);
    let total: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_material_serial WHERE material_id = ?::uuid",
            vec![Value::from(&Uuid::parse_str(MATERIAL_A).unwrap())],
        )
        .await
        .unwrap();
    assert_eq!(total, 0);
}

/// ⑤ 流水：`txn_type=IN`、`biz_type=PURCHASE_IN`、`before_*` 快照正确（第二次入库才有非零快照）。
#[tokio::test]
async fn txn_snapshot_is_before_and_after() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let first = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
    let second = post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;

    let rows = db()
        .find_with_params(
            "SELECT txn_type, biz_type, doc_type, doc_no, line_no, qty, unit_cost, amount, \
                    before_qty, before_amount, before_cost, after_qty, after_amount, after_cost, \
                    period_code \
               FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id = ?::uuid",
            vec![Value::from(&second)],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "一张收货单的一行 = 一条流水");
    let row = &rows[0];
    assert_eq!(row.get_typed::<String>("txn_type").unwrap(), "IN");
    assert_eq!(row.get_typed::<String>("biz_type").unwrap(), "PURCHASE_IN");
    assert_eq!(row.get_typed::<String>("doc_type").unwrap(), "STOCK_IN");
    assert_eq!(row.get_typed::<i32>("line_no").unwrap(), 1);
    assert_eq!(row.get_typed::<Decimal>("qty").unwrap(), dec("100"));
    assert_eq!(
        row.get_typed::<Decimal>("unit_cost").unwrap(),
        dec("3.000000")
    );
    assert_eq!(row.get_typed::<Decimal>("amount").unwrap(), dec("300"));
    assert_eq!(
        row.get_typed::<Decimal>("before_qty").unwrap(),
        dec("100"),
        "第二次入库的 before_qty 必须是第一张单留下的 100"
    );
    assert_eq!(
        row.get_typed::<Decimal>("before_amount").unwrap(),
        dec("200")
    );
    assert_eq!(
        row.get_typed::<Decimal>("before_cost").unwrap(),
        dec("2.000000")
    );
    assert_eq!(row.get_typed::<Decimal>("after_qty").unwrap(), dec("200"));
    assert_eq!(
        row.get_typed::<Decimal>("after_amount").unwrap(),
        dec("500")
    );
    assert_eq!(
        row.get_typed::<Decimal>("after_cost").unwrap(),
        dec("2.500000")
    );
    let period: String = row.get_typed("period_code").unwrap();
    assert_eq!(period.len(), 7, "period_code 是 yyyy-MM：{period}");
    // 第一张单的 before 全是 0
    let first_rows = db()
        .find_with_params(
            "SELECT before_qty, before_amount FROM biz_inventory_txn \
              WHERE doc_id = ?::uuid",
            vec![Value::from(&first)],
        )
        .await
        .unwrap();
    assert_eq!(
        first_rows[0].get_typed::<Decimal>("before_qty").unwrap(),
        dec("0")
    );
}

/// ⑥ 并发：两个收货单同时收同一物料（两行顺序相反）→ 不丢数、不死锁。
///
/// 两行顺序相反是关键：若不按 `material_id` 升序加锁，A 先锁 M1 再锁 M2、B 先锁 M2 再锁 M1
/// 就是教科书死锁。这里断言（a）两单都过账成功（无死锁）；（b）库存 = 两单之和（不丢数）。
#[tokio::test]
async fn concurrent_receipts_do_not_lose_or_deadlock() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let a = create_stock_in(
        &req(vec![
            line(MATERIAL_A, "10", "2", "0"),
            line(MATERIAL_B, "10", "5", "0"),
        ]),
        None,
    )
    .await
    .unwrap();
    let b = create_stock_in(
        &req(vec![
            line(MATERIAL_B, "20", "5", "0"),
            line(MATERIAL_A, "20", "2", "0"),
        ]),
        None,
    )
    .await
    .unwrap();

    let post_request = post_req();
    let (ra, rb) = tokio::join!(
        change_stock_in_status(a, &post_request, None),
        change_stock_in_status(b, &post_request, None)
    );
    ra.unwrap_or_else(|err| panic!("并发过账 A 失败（死锁？）：{} {}", err.code, err.msg));
    rb.unwrap_or_else(|err| panic!("并发过账 B 失败（死锁？）：{} {}", err.code, err.msg));

    let (qty_a, amount_a, avg_a) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty_a, dec("30"), "10 + 20，一笔都不能丢");
    assert_eq!(amount_a, dec("60"));
    assert_eq!(avg_a, dec("2.000000"));
    let (qty_b, amount_b, _) = inventory_of(MATERIAL_B).await;
    assert_eq!(qty_b, dec("30"));
    assert_eq!(amount_b, dec("150"), "10×5 + 20×5");
    // 两单各一行流水，共 4 条
    let txns: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id IN (?::uuid, ?::uuid)",
            vec![Value::from(&a), Value::from(&b)],
        )
        .await
        .unwrap();
    assert_eq!(txns, 4);
}

/// ⑦ 过账幂等：重复 POSTED → 409，且库存只加一次。
#[tokio::test]
async fn repeated_post_is_conflict() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
    let err = change_stock_in_status(id, &post_req(), None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("已过账"), "{}", err.msg);
    let (qty, _, _) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("10"), "库存只加一次");
    let txns: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(txns, 1, "流水也只写一条");
}

/// ⑫ 成本口径：入库成本用不含税金额，不是价税合计（13% 时差 13%）。
#[tokio::test]
async fn cost_uses_net_amount_not_gross() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "50", "10", "13")]).await;
    // 不含税 500、税 65、价税合计 565
    let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("50"));
    assert_eq!(amount, dec("500"), "存货成本 = 不含税 500（不是 565）");
    assert_ne!(amount, dec("565"));
    assert_eq!(avg, dec("10.000000"));
    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.items[0].amount, dec("500"));
    assert_eq!(detail.items[0].tax_amount, dec("65"));
    assert_eq!(detail.items[0].with_tax_amount, dec("565"));
    assert_eq!(
        detail.items[0].cost_price,
        dec("10.000000"),
        "cost_price 落不含税单价"
    );
    let txn_amount: Decimal = db()
        .scalar_with_params(
            "SELECT amount FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(txn_amount, dec("500"), "流水金额也只用不含税");
}

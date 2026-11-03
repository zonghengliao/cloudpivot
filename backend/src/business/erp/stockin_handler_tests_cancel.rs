// ============================================================================
// stockin_handler_tests_cancel.rs —— 作废反冲：成本/库位/流水/红字凭证/序列号/并发/不变量
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
use crate::business::erp::stockin_model::{self, STATUS_CANCELLED, STATUS_POSTED};

/// ① 作废收货单 → 库存回滚到作废前（成本账三值全归零）。
#[tokio::test]
async fn cancel_rolls_back_cost_inventory() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (dec("100"), dec("200"), dec("2.000000")),
        "前置：过账后库存 100 / 200 / 2.0"
    );
    cancel(id, "供应商召回").await;
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
        "作废后必须回到作废前（库存行 qty / amount / avg_cost 全归零）"
    );
    assert_eq!(detail_of(id).await.unwrap().header.status, STATUS_CANCELLED);
}

/// ② 作废收货单 → 库位账同步回滚（P6 写入的那一本账也要退）。
#[tokio::test]
async fn cancel_rolls_back_location_ledger() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let row = seed_location_row(MATERIAL_A, LOC_DICT_A).await;
    let id = post_new(vec![line_at(MATERIAL_A, "40", "2", "0", &row.to_string())]).await;
    assert_eq!(
        location_qty_of(&row).await,
        Some(dec("40")),
        "前置：过账写了库位账 40"
    );
    cancel(id, "库位选错").await;
    assert_eq!(
        location_qty_of(&row).await,
        Some(Decimal::ZERO),
        "库位账必须同步回滚到 0（否则两账不一致 = 不变式 B1）"
    );
}

/// ③ 作废 → 生成反向流水 `OUT / PURCHASE_IN_CANCEL`，且 before_/after_ 六字段真填。
#[tokio::test]
async fn cancel_writes_out_txn() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "60", "2.5", "0")]).await;
    let in_no = detail_of(id).await.unwrap().header.in_no;
    cancel(id, "重复下单").await;

    let rows = db()
        .find_with_params(
            "SELECT txn_type, biz_type, doc_no, qty, amount, unit_cost, before_qty, after_qty, \
                    before_amount, after_amount, before_cost, after_cost \
               FROM biz_inventory_txn WHERE doc_id = ?::uuid AND txn_type = 'OUT'",
            vec![Value::from(&id)],
        )
        .await
        .unwrap_or_else(|err| panic!("查反向流水失败：{err}"));
    assert_eq!(rows.len(), 1, "一张单一行明细 → 一条反向流水");
    let row = &rows[0];
    let txn_type: String = row.get_typed("txn_type").unwrap();
    let biz_type: String = row.get_typed("biz_type").unwrap();
    let doc_no: String = row.get_typed("doc_no").unwrap();
    assert_eq!(txn_type, stockin_model::TXN_TYPE_OUT);
    assert_eq!(biz_type, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL);
    assert_eq!(doc_no, in_no, "反向流水必须带原收货单号（可追溯）");
    assert_eq!(row.get_typed::<Decimal>("qty").unwrap(), dec("60"));
    assert_eq!(row.get_typed::<Decimal>("amount").unwrap(), dec("150"));
    assert_eq!(row.get_typed::<Decimal>("unit_cost").unwrap(), dec("2.5"));
    assert_eq!(row.get_typed::<Decimal>("before_qty").unwrap(), dec("60"));
    assert_eq!(
        row.get_typed::<Decimal>("after_qty").unwrap(),
        Decimal::ZERO
    );
    assert_eq!(
        row.get_typed::<Decimal>("before_amount").unwrap(),
        dec("150")
    );
    assert_eq!(
        row.get_typed::<Decimal>("after_amount").unwrap(),
        Decimal::ZERO
    );
    assert_eq!(
        row.get_typed::<Decimal>("before_cost").unwrap(),
        dec("2.500000")
    );
    assert_eq!(
        row.get_typed::<Decimal>("after_cost").unwrap(),
        Decimal::ZERO
    );
}

/// ④ 作废 → 红字凭证照旧生成（P2-C1 逻辑不被破坏，双向指针都在）。
#[tokio::test]
async fn cancel_still_generates_red_voucher() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
    let originals = db()
        .find_with_params(
            "SELECT id, voucher_no, total_debit, total_credit FROM biz_voucher \
              WHERE source_id = ?::uuid AND is_red = 0",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(originals.len(), 1, "前置：过账即生成 1 张有效凭证");
    let original_id: Uuid = originals[0].get_typed("id").unwrap();

    cancel(id, "退货").await;
    let reds = db()
        .find_with_params(
            "SELECT id, reverse_of, total_debit, total_credit FROM biz_voucher \
              WHERE source_id = ?::uuid AND is_red = 1",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(reds.len(), 1, "作废必须生成 1 张红字凭证");
    let red_id: Uuid = reds[0].get_typed("id").unwrap();
    assert_eq!(
        reds[0].get_typed::<Option<Uuid>>("reverse_of").unwrap(),
        Some(original_id),
        "红字凭证必须指向原凭证"
    );
    assert_eq!(
        reds[0].get_typed::<Decimal>("total_debit").unwrap(),
        originals[0].get_typed::<Decimal>("total_debit").unwrap(),
        "红冲金额 = 原凭证金额（借贷互换，不用负数）"
    );
    let reversed_by: Option<Uuid> = db()
        .scalar_with_params(
            "SELECT reversed_by FROM biz_voucher WHERE id = ?::uuid",
            vec![Value::from(&original_id)],
        )
        .await
        .unwrap();
    assert_eq!(reversed_by, Some(red_id), "原凭证要打 reversed_by（双向）");
}

/// ⑤ 幂等：已作废再作废 → 409，且库存不会被二次扣成负数。
#[tokio::test]
async fn cancel_twice_is_409() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    cancel(id, "第一次作废").await;
    let err = change_stock_in_status(id, &cancel_req(Some("第二次作废")), None)
        .await
        .expect_err("重复作废必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("不能重复作废"),
        "报文要点明幂等语义：{}",
        err.msg
    );
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO);
    assert_eq!(
        txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        1,
        "反向流水只能有一条"
    );
}

/// ⑥ 库存不足（货已出库）时作废 → 409 + 文案，不许把库存扣成负数。
#[tokio::test]
async fn cancel_with_shipped_goods_is_409() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "9", "2", "0")]).await;
    // 模拟「货已出库部分」：库存只剩 3
    db().execute_with_params(
        "UPDATE biz_inventory SET qty = 3, amount = 6, avg_cost = 2 \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![
            Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
            Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
        ],
    )
    .await
    .unwrap();

    let err = change_stock_in_status(id, &cancel_req(Some("作废试试")), None)
        .await
        .expect_err("库存不足必须拒绝");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("已部分出库") && err.msg.contains("退货入库冲销"),
        "文案要说清「为什么不能作废」和「该怎么处理」：{}",
        err.msg
    );
    assert!(err.msg.contains('9'), "文案要点名本单数量：{}", err.msg);
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        dec("3"),
        "拒绝时库存一个字节都不能动"
    );
    assert_eq!(
        detail_of(id).await.unwrap().header.status,
        STATUS_POSTED,
        "拒绝时必须整单回滚（状态仍是 POSTED）"
    );
}

/// ⑦ 冲减金额用过账流水里的 `amount`，不是「当前均价 × 数量」。
///
/// 构造：先 100@2（200），再 100@3（300）→ 200 件 / 500 元 / 均价 2.5。
/// 作废**第一张**只能退 200（流水原额），按现价算会退 250，直接污染成本。
#[tokio::test]
async fn cancel_uses_posted_amount_not_current_avg() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let first = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
    post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (dec("200"), dec("500"), dec("2.500000")),
        "前置：移动加权后 200 / 500 / 2.5"
    );

    cancel(first, "第一张收错了").await;
    let (qty, amount, avg) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("100"), "退的是第一张的 100 件");
    assert_eq!(
        amount,
        dec("300"),
        "金额必须减第一张过账流水里的 200（500 - 200 = 300）"
    );
    assert_ne!(
        amount,
        dec("250"),
        "按当前均价退 100×2.5=250 是错的：会把第二层的成本摊到第一层上"
    );
    assert_eq!(
        avg,
        dec("3.000000"),
        "剩余结存 300/100 = 3.0，正好是第二层的原始单价（摘掉一层 = 还原加权平均）"
    );
}

/// ⑧ 冲减后数量归零 → 金额也必须归零（不留「0 件还有钱」的幽灵金额）。
#[tokio::test]
async fn cancel_zero_qty_zeroes_amount() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "7", "3", "0")]).await;
    assert_eq!(inventory_of(MATERIAL_A).await.1, dec("21"));
    cancel(id, "整单退回").await;
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
        "数量归零时金额与均价必须同时归零"
    );
}

/// ⑨ 并发作废两张单（同一物料）→ 不死锁、两单都成功、数量正确。
#[tokio::test]
async fn concurrent_cancels_do_not_deadlock() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let a = post_new(vec![line(MATERIAL_A, "30", "2", "0")]).await;
    let b = post_new(vec![line(MATERIAL_A, "20", "3", "0")]).await;
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("50"));

    let req_a = cancel_req(Some("并发 A"));
    let req_b = cancel_req(Some("并发 B"));
    let (ra, rb) = tokio::join!(
        change_stock_in_status(a, &req_a, Some(uuid_keeper())),
        change_stock_in_status(b, &req_b, Some(uuid_keeper()))
    );
    ra.unwrap_or_else(|err| panic!("A 作废失败：{} {}", err.code, err.msg));
    rb.unwrap_or_else(|err| panic!("B 作废失败：{} {}", err.code, err.msg));
    // P16：六本账回滚发生在**复核通过**那一刻 —— 并发通过两张单，验证的是同一件事
    // （反向冲减按 material_id 升序加锁，两张单不会交叉等待）。
    tokio::join!(approve_cancel(a), approve_cancel(b));

    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        Decimal::ZERO,
        "两张单都作废 → 库存归零（没有互相踩掉/多扣）"
    );
    assert_eq!(
        txn_count(a, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        1
    );
    assert_eq!(
        txn_count(b, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        1
    );
}

/// ⑩ 作废失败 → 事务整体回滚：库存 / 流水 / 凭证 / 状态全都不变。
#[tokio::test]
async fn failed_cancel_rolls_back_everything() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "9", "2", "0")]).await;
    let vouchers_before = voucher_count(id, None).await;
    assert_eq!(vouchers_before, 1);
    // 只留 1 件（模拟货已出库 8 件）
    db().execute_with_params(
        "UPDATE biz_inventory SET qty = 1, amount = 2 \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![
            Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
            Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
        ],
    )
    .await
    .unwrap();

    let err = change_stock_in_status(id, &cancel_req(Some("作废")), None)
        .await
        .expect_err("必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);

    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("1"), "库存不变");
    assert_eq!(
        txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        0,
        "不能留下半条反向流水"
    );
    assert_eq!(
        voucher_count(id, None).await,
        vouchers_before,
        "不能留下红字凭证"
    );
    assert_eq!(voucher_count(id, Some(1)).await, 0, "红字凭证一条都不许有");
    assert_eq!(
        detail_of(id).await.unwrap().header.status,
        STATUS_POSTED,
        "状态必须回滚成 POSTED"
    );
}

/// ⑪ 作废后不变量 E4 通过：库存 ≤ 有效入库（POSTED 单明细合计），且流水累计 = 库存。
#[tokio::test]
async fn cancel_keeps_invariant_e4() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let a = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
    post_new(vec![line(MATERIAL_A, "50", "4", "0")]).await;
    cancel(a, "作废其中一张").await;

    let material = Uuid::parse_str(MATERIAL_A).unwrap();
    let warehouse = Uuid::parse_str(WAREHOUSE).unwrap();
    // E4：库存 > 有效入库 的组合（本夹具范围内）
    let over: i64 = db()
        .scalar_with_params(
            "SELECT COUNT(*) FROM ( \
                 SELECT v.qty AS inv_qty, coalesce(p.q, 0) AS posted_qty \
                   FROM (SELECT sum(qty) AS qty FROM biz_inventory \
                          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid) v \
                   LEFT JOIN (SELECT sum(i.qty) AS q \
                                FROM biz_stock_in s JOIN biz_stock_in_item i ON i.stock_in_id = s.id \
                               WHERE s.deleted_at IS NULL AND s.status = 'POSTED' \
                                 AND s.warehouse_id = ?::uuid AND i.material_id = ?::uuid) p ON TRUE \
             ) t WHERE t.inv_qty > t.posted_qty + 0.0001",
            vec![
                Value::from(&material),
                Value::from(&warehouse),
                Value::from(&warehouse),
                Value::from(&material),
            ],
        )
        .await
        .unwrap();
    assert_eq!(over, 0, "作废后库存不许大于有效入库量（E4）");
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("50"));

    // B4：流水累计 = 当前库存
    let signed: Decimal = db()
        .scalar_with_params(
            "SELECT coalesce(sum(CASE WHEN txn_type = 'IN' THEN qty ELSE -qty END), 0) \
               FROM biz_inventory_txn WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
            vec![Value::from(&material), Value::from(&warehouse)],
        )
        .await
        .unwrap();
    assert_eq!(
        signed,
        inventory_of(MATERIAL_A).await.0,
        "反向流水必须让「流水累计 = 当前库存」重新成立（B4）"
    );
}

/// ① 作废一物一码收货单 → 本单生成的序列号全部退回（软删），库存同步归零。
#[tokio::test]
async fn cancel_posted_voids_serials_of_the_receipt() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_SN, "3", "40", "0")]).await;
    assert_eq!(
        stockin_model::serial_count_of(id).await.unwrap(),
        3,
        "前置：过账生成 3 条序列号"
    );
    assert_eq!(inventory_of(MATERIAL_SN).await.0, dec("3"));

    cancel(id, "供应商整批退回").await;

    assert_eq!(
        stockin_model::serial_count_of(id).await.unwrap(),
        0,
        "P10：作废后本单的序列号必须全部出账（之前一条都不退，8 条 IN_STOCK 挂在作废单上）"
    );
    assert_eq!(
        serial_counts_of(id).await,
        (0, 3),
        "退回 = 软删（行留着做审计），不是物理删"
    );
    assert_eq!(
        inventory_of(MATERIAL_SN).await,
        (dec("0"), dec("0"), dec("0")),
        "序列号退账与库存退账是同一步，作废后两账都归零"
    );

    // 软删不是状态变更：`status` 仍是 IN_STOCK（作废 ≠ 出库/报废，三态约束不含 VOID）
    let rows = db()
        .find_with_params(
            "SELECT status FROM biz_material_serial \
              WHERE source_id = ?::uuid AND deleted_at IS NOT NULL",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row.get_typed::<String>("status").unwrap(), "IN_STOCK");
    }

    // 不变式①（W2c 口径）：在库序列号件数 == 库存数量
    let in_stock: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_material_serial \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND status = 'IN_STOCK' AND deleted_at IS NULL",
            vec![
                Value::from(&Uuid::parse_str(MATERIAL_SN).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(in_stock, 0, "作废后在库序列号件数必须归零");
}

/// ② 本单有序列号已出库 → 拒绝作废（409），库存 / 序列号 / 单据状态一个字节都不动。
#[tokio::test]
async fn cancel_refuses_when_serial_already_shipped() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_SN, "3", "40", "0")]).await;
    // 模拟「已出库 1 件」：最小号转 OUTBOUND，库存同步 3 → 2（真实出库就是这么走的）
    db().execute_with_params(
        "UPDATE biz_material_serial SET status = 'OUTBOUND', outbound_at = now() \
          WHERE id = (SELECT id FROM biz_material_serial \
                       WHERE source_id = ?::uuid AND deleted_at IS NULL \
                       ORDER BY serial_no LIMIT 1)",
        vec![Value::from(&id)],
    )
    .await
    .unwrap();
    db().execute_with_params(
        "UPDATE biz_inventory SET qty = 2, amount = 80, avg_cost = 40 \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![
            Value::from(&Uuid::parse_str(MATERIAL_SN).unwrap()),
            Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
        ],
    )
    .await
    .unwrap();

    let err = change_stock_in_status(id, &cancel_req(Some("作废试试")), None)
        .await
        .expect_err("本单序列号已出库必须拒绝");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("序列号") && err.msg.contains("已出库"),
        "文案要点名「是哪一类货走了」：{}",
        err.msg
    );
    assert!(
        err.msg.contains("SN-"),
        "文案要带出具体号码（运维才能去查那一件）：{}",
        err.msg
    );
    assert!(
        err.msg.contains("退货入库冲销"),
        "文案要给可执行出路：{}",
        err.msg
    );

    // 整单回滚：序列号一条都没被软删、库存没有二次扣减、状态仍 POSTED
    assert_eq!(
        serial_counts_of(id).await,
        (3, 0),
        "拒绝时序列号一条都不许动"
    );
    assert_eq!(
        inventory_of(MATERIAL_SN).await.0,
        dec("2"),
        "拒绝时库存不许二次扣减"
    );
    assert_eq!(
        detail_of(id).await.unwrap().header.status,
        STATUS_POSTED,
        "拒绝时必须整单回滚"
    );
}

/// ③ 只退本单的号：同物料另一张收货单的序列号不能被连带作废。
#[tokio::test]
async fn cancel_voids_only_serials_of_that_receipt() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let first = post_new(vec![line(MATERIAL_SN, "2", "40", "0")]).await;
    let second = post_new(vec![line(MATERIAL_SN, "1", "40", "0")]).await;
    assert_eq!(serial_counts_of(first).await, (2, 0));
    assert_eq!(serial_counts_of(second).await, (1, 0));

    cancel(first, "少收两件").await;

    assert_eq!(
        serial_counts_of(first).await,
        (0, 2),
        "本单的号必须全部退回"
    );
    assert_eq!(
        serial_counts_of(second).await,
        (1, 0),
        "别的单的号不能被连带作废（`source_id` 精确到单）"
    );
    assert_eq!(
        inventory_of(MATERIAL_SN).await.0,
        dec("1"),
        "库存只退本单那 2 件"
    );
}

/// ⑫ 流水缺失的历史单：拒绝作废并说明原因（不许按现价估算，不许静默）。
#[tokio::test]
async fn cancel_refuses_when_posted_txn_missing() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "12", "2", "0")]).await;
    // 造「单据 POSTED 但过账流水丢了」的历史形态（P9 之前的数据缺口长这样）
    db().execute_with_params(
        "DELETE FROM biz_inventory_txn WHERE doc_id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .unwrap();

    let err = change_stock_in_status(id, &cancel_req(Some("作废")), None)
        .await
        .expect_err("流水缺失必须拒绝");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("找不到过账流水"),
        "报文要说清缺口在哪：{}",
        err.msg
    );
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        dec("12"),
        "拒绝时库存不动（不能凭现价估算退多少）"
    );
    assert_eq!(detail_of(id).await.unwrap().header.status, STATUS_POSTED);

    // 收尾清理（本模块最后一个用例）：过账即生成凭证，凭证若留在库里，
    // 按「period 全局计数」断言的模块（voucher_query）会漂数。
    // 既有用例只在 setup 里清上一轮，最后一条跑完夹具会留在库 —— 这里补一刀。
    cleanup_fixtures().await;
}

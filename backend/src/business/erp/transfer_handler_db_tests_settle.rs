// ============================================================================
// transfer_handler_db_tests_settle.rs —— 真库：收货结算（W3c）— 库存双仓移动 / 流水 / 序列号 / 成本守恒 / 并发与幂等 + 在途→完成全链路
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::db_tests_fixtures::*;
use super::*;
use crate::business::erp::transfer_model::{self, STATUS_PENDING};
use crate::business::erp::transfer_model::{
    STATUS_APPROVED, STATUS_CANCELLED, STATUS_DONE, STATUS_IN_TRANSIT,
};
use crate::shared::error_code;
use nik::prelude::chrono::Utc;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

/// ★ 本卡补口（DC-09 缺口 1 · 调拨侧）：**结算**期复检单位。
///
/// 造法（与收货侧同一形态）：先用基本单位把单建到 `IN_TRANSIT`，再**绕过接口**把物料
/// 改成折算单位「箱」—— 这正是「升级前建的折算单位草稿 / 主数据被改」在库里的样子：
/// 建单期的 `prepare_items` 早跑完了。结算必须 `400 + 66030`，
/// 且单据留在 `IN_TRANSIT`、两本账（成本账 + 库位账）与流水**一分不动**。
#[tokio::test]
async fn settle_rejects_non_base_unit_material() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let qty_before = needed(M_PLAIN, WH_FROM).await;
    let amount_before = bucket_amount(M_PLAIN, WH_FROM).await;
    let to_qty_before = needed(M_PLAIN, WH_TO).await;

    // 绕过接口：物料的计量单位改成折算单位「箱」（1 箱 = 12 个）
    exec(
        "UPDATE biz_material SET unit_id = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&uuid(U_BOX)), Value::from(&uuid(M_PLAIN))],
        "改物料单位（模拟绕过接口 / 升级前的存量配置）",
    )
    .await;

    let error = receive(id).await.expect_err("折算单位不许结算");
    assert_eq!(error.status, 400, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_UNIT_NOT_BASE, "{}", error.msg);
    assert!(
        error.msg.contains("单位「箱」不是基本单位"),
        "{}",
        error.msg
    );
    assert!(
        error.msg.contains("本版本不支持多单位换算"),
        "{}",
        error.msg
    );
    // 5 件折算成 5 箱 × 12 = 60（建议值按本行数量算，不是拼死的 12）
    assert!(error.msg.contains("1 箱 = 12 个 → 录 60"), "{}", error.msg);
    assert!(error.msg.contains("W3B-M-PLAIN"), "{}", error.msg);
    assert!(error.msg.contains("items[0]"), "{}", error.msg);

    // 结算失败 = 整笔回滚：单据留在 IN_TRANSIT，两本账 + 流水一字未动
    assert_eq!(
        refresh(id).await.status,
        STATUS_IN_TRANSIT,
        "被拒的结算不能把单据推到 DONE"
    );
    assert_eq!(needed(M_PLAIN, WH_FROM).await, qty_before, "调出仓不许扣");
    assert_eq!(
        bucket_amount(M_PLAIN, WH_FROM).await,
        amount_before,
        "调出仓金额不许动"
    );
    assert_eq!(needed(M_PLAIN, WH_TO).await, to_qty_before, "调入仓不许加");
    assert!(txns_of(id).await.is_empty(), "被拒的结算不许写流水");

    // 把单位改回基本单位 → 同一张单正常结算（正常路径回归绿）
    exec(
        "UPDATE biz_material SET unit_id = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&uuid(UNIT)), Value::from(&uuid(M_PLAIN))],
        "复位物料单位",
    )
    .await;
    receive_ok(id).await;
    assert_eq!(needed(M_PLAIN, WH_FROM).await, qty_before - dec("5"));
    assert_eq!(txns_of(id).await.len(), 2, "正常结算仍是两条流水");
}

// ---------- ⑰-g（P5 ⑧ + ⑩）审批通过 → 发货 → 收货：链路通 + W3c 库存双写不变 ----------
#[tokio::test]
async fn approved_transfer_continues_to_done_and_settles_inventory() {
    let Some(_guard) = prepare().await else {
        return;
    };
    ensure_seed_flow().await;
    let applicant = uuid(U_APPLICANT);
    let id = create_transfer(&body(vec![item(M_PLAIN, "5")]), Some(applicant), None)
        .await
        .expect("建单");
    change_transfer_status(id, &status_req(STATUS_PENDING, None), applicant)
        .await
        .expect("提交审批");
    let instance_id = refresh(id).await.approval_instance_id.expect("实例 id");

    // 审批通过：走**生产同款路径** —— 引擎在事务里投票落终态 → 再回调调拨单
    approve_instance(instance_id).await;
    let row = refresh(id).await;
    assert_eq!(row.status, STATUS_APPROVED, "⑧ 审批通过 → 调拨单 APPROVED");
    assert!(row.approved_at.is_some(), "⑧ approved_at 有值");

    // ⑧ 通过后能继续流转：APPROVED → IN_TRANSIT → DONE
    change_transfer_status(id, &status_req(STATUS_IN_TRANSIT, None), applicant)
        .await
        .expect("⑧ 审批通过后必须能发货");
    assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
    change_transfer_status(id, &status_req(STATUS_DONE, None), applicant)
        .await
        .expect("⑧ 在途必须能收货");
    let row = refresh(id).await;
    assert_eq!(row.status, STATUS_DONE);
    assert!(row.received_at.is_some());

    // ⑩ W3c 的库存双写没被本卡破坏：调出仓 100-5=95、调入仓 10+5=15，两条流水
    assert_dec_eq(
        inventory_qty(M_PLAIN, WH_FROM).await,
        "95",
        "⑩ 调出仓扣减 5",
    );
    assert_dec_eq(inventory_qty(M_PLAIN, WH_TO).await, "15", "⑩ 调入仓增加 5");
    assert_dec_eq(
        inventory_amount(M_PLAIN, WH_FROM).await,
        "1187.5",
        "⑩ 调出仓金额 1250 - 5×12.5",
    );
    assert_dec_eq(
        inventory_amount(M_PLAIN, WH_TO).await,
        "142.5",
        "⑩ 调入仓金额 80 + 5×12.5（成本原价转移）",
    );
    let txns = db()
        .find_with_params(
            "SELECT txn_type, qty::text AS qty FROM biz_inventory_txn \
              WHERE doc_type = 'STOCK_TRANSFER' AND doc_id = ?::uuid ORDER BY txn_type",
            vec![Value::from(&id)],
        )
        .await
        .expect("查库存流水");
    assert_eq!(txns.len(), 2, "⑩ 一张调拨单恰好两条流水（OUT + IN）");
    let kinds: Vec<String> = txns
        .iter()
        .map(|row| row.get_typed::<String>("txn_type").expect("txn_type"))
        .collect();
    assert_eq!(kinds, vec!["IN".to_string(), "OUT".to_string()]);
    for txn in &txns {
        let qty: String = txn.get_typed("qty").expect("qty");
        assert_eq!(qty, "5.0000", "⑩ 流水数量恒正（ck_biz_inv_txn_qty 禁负数）");
    }
}

// ---------- W3c ① 数量双写：调出仓 -、调入仓 + ----------
#[tokio::test]
async fn w3c_settle_moves_qty_between_warehouses() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let from_before = needed(M_PLAIN, WH_FROM).await;
    let to_before = needed(M_PLAIN, WH_TO).await;
    receive_ok(id).await;
    assert_eq!(
        needed(M_PLAIN, WH_FROM).await,
        from_before - dec("5"),
        "调出仓必须 -5"
    );
    assert_eq!(
        needed(M_PLAIN, WH_TO).await,
        to_before + dec("5"),
        "调入仓必须 +5"
    );
    assert_eq!(refresh(id).await.status, STATUS_DONE);
}

// ---------- W3c ② 序列号跟着走 ----------
#[tokio::test]
async fn w3c_settle_moves_serials_to_target_warehouse() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let mut line = item(M_SN, "1");
    line.serial_ids = vec![SERIAL_1.to_string()];
    let id = create_and_ship(vec![line]).await;
    assert_eq!(
        serial_warehouse(SERIAL_1).await,
        Some(uuid(WH_FROM)),
        "结算前序列号在调出仓"
    );
    receive_ok(id).await;
    assert_eq!(
        serial_warehouse(SERIAL_1).await,
        Some(uuid(WH_TO)),
        "结算后序列号必须到调入仓"
    );
    assert_eq!(
        serial_warehouse(SERIAL_2).await,
        Some(uuid(WH_FROM)),
        "没上单的序列号不得被顺带挪走"
    );
}

// ---------- W3c ③ 调出流水：OUT / TRANSFER_OUT ----------
#[tokio::test]
async fn w3c_settle_writes_out_txn_row() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let no = refresh(id).await.transfer_no;
    receive_ok(id).await;
    let txns = txns_of(id).await;
    let out: Vec<&Row> = txns
        .iter()
        .filter(|row| row_text(row, "txn_type") == "OUT")
        .collect();
    assert_eq!(out.len(), 1, "调出流水必须正好 1 条");
    let row = out[0];
    assert_eq!(row_text(row, "biz_type"), "TRANSFER_OUT");
    assert_eq!(row_text(row, "doc_type"), "STOCK_TRANSFER");
    assert_eq!(row_text(row, "doc_no"), no, "流水必须挂单号");
    assert_eq!(
        row.get_typed::<Uuid>("warehouse_id").expect("仓"),
        uuid(WH_FROM),
        "调出流水挂在调出仓"
    );
    assert_eq!(txn_text(row, "qty"), "5.0000", "qty 恒正（禁负数）");
    assert_eq!(
        txn_text(row, "unit_cost"),
        "12.500000",
        "原价 = 调出仓均价快照"
    );
    assert_eq!(txn_text(row, "amount"), "62.5000");
}

// ---------- W3c ④ 调入流水：IN / TRANSFER_IN ----------
#[tokio::test]
async fn w3c_settle_writes_in_txn_row() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(id).await;
    let txns = txns_of(id).await;
    let r#in: Vec<&Row> = txns
        .iter()
        .filter(|row| row_text(row, "txn_type") == "IN")
        .collect();
    assert_eq!(r#in.len(), 1, "调入流水必须正好 1 条");
    let row = r#in[0];
    assert_eq!(row_text(row, "biz_type"), "TRANSFER_IN");
    assert_eq!(
        row.get_typed::<Uuid>("warehouse_id").expect("仓"),
        uuid(WH_TO),
        "调入流水挂在调入仓"
    );
    assert_eq!(txn_text(row, "qty"), "5.0000");
}

// ---------- W3c ⑤ 成本守恒：调出 amount 减少 == 调入 amount 增加 ----------
#[tokio::test]
async fn w3c_settle_keeps_cost_conserved() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let from_before = bucket_amount(M_PLAIN, WH_FROM).await.expect("调出仓金额");
    let to_before = bucket_amount(M_PLAIN, WH_TO).await.expect("调入仓金额");
    receive_ok(id).await;
    let from_after = bucket_amount(M_PLAIN, WH_FROM).await.expect("调出仓金额");
    let to_after = bucket_amount(M_PLAIN, WH_TO).await.expect("调入仓金额");
    let out = from_before - from_after;
    let r#in = to_after - to_before;
    assert_eq!(out, dec("62.5"), "调出仓金额减少 = 5 × 12.5");
    assert_eq!(r#in, out, "金额不能凭空产生或消失");

    // 流水侧同样守恒（两笔流水金额相等）
    let txns = txns_of(id).await;
    let out_amt: Decimal = txns
        .iter()
        .filter(|row| row_text(row, "txn_type") == "OUT")
        .map(|row| row.get_typed::<Decimal>("amount").expect("amount"))
        .sum();
    let in_amt: Decimal = txns
        .iter()
        .filter(|row| row_text(row, "txn_type") == "IN")
        .map(|row| row.get_typed::<Decimal>("amount").expect("amount"))
        .sum();
    assert_eq!(out_amt, in_amt, "流水 OUT 合计 == IN 合计");
    assert_eq!(out_amt, dec("62.5"));
}

// ---------- W3c ⑥ 调入仓移动加权重算 avg_cost ----------
#[tokio::test]
async fn w3c_settle_recomputes_target_avg_cost() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let old_qty = needed(M_PLAIN, WH_TO).await;
    let old_amount = bucket_amount(M_PLAIN, WH_TO).await.expect("金额");
    receive_ok(id).await;
    let new_qty = needed(M_PLAIN, WH_TO).await;
    let new_amount = bucket_amount(M_PLAIN, WH_TO).await.expect("金额");
    let new_cost = bucket_avg_cost(M_PLAIN, WH_TO).await.expect("均价");
    assert_eq!(new_qty, old_qty + dec("5"));
    assert_eq!(new_amount, old_amount + dec("62.5"));
    // 10 件 @8 = 80，再收 5 件 @12.5 = 62.5 → 142.5 / 15 = 9.5
    assert_eq!(new_cost, dec("9.5"), "新均价 = 新金额 / 新数量");
    // 调出仓的均价不因出库而改变（移动加权口径）
    assert_eq!(
        bucket_avg_cost(M_PLAIN, WH_FROM).await.expect("调出仓均价"),
        dec("12.5"),
        "出库不改调出仓均价"
    );
}

// ---------- W3c ⑦ 调入仓库存行不存在 → INSERT 建档 ----------
#[tokio::test]
async fn w3c_settle_creates_missing_target_row() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // M_SN 在总仓本来没有库存行（夹具只给分仓建了）
    assert!(
        bucket_qty(M_SN, WH_TO).await.is_none(),
        "前置：总仓没有 M_SN 的库存行"
    );
    let mut line = item(M_SN, "2");
    line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
    let id = create_and_ship(vec![line]).await;
    receive_ok(id).await;
    assert_eq!(needed(M_SN, WH_TO).await, dec("2"), "建档后数量 = 调入量");
    assert_eq!(
        bucket_avg_cost(M_SN, WH_TO).await.expect("均价"),
        dec("30"),
        "建档行的均价 = 原价（不是 0）"
    );
    assert_eq!(
        bucket_amount(M_SN, WH_TO).await.expect("金额"),
        dec("60"),
        "建档金额 = 数量 × 原价"
    );
}

// ---------- W3c ⑧ 串行多明细（3 个物料） ----------
#[tokio::test]
async fn w3c_settle_handles_three_lines_in_one_doc() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 第三个物料（再插一个按数量的物料 L3）一起走
    exec(
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, \
                                  safety_stock, costing_method, tax_rate, status, serial_required) \
         VALUES ('d3b0000c-0000-4000-8000-000000000003', 'W3C-M-L3', 'W3C 第三物料', \
                 ?::uuid, 'RAW', 0, 'MOVING_AVG', 0, 1, false) ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(UNIT))],
        "第三物料",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                    avg_cost, amount) \
         VALUES (gen_random_uuid(), 'd3b0000c-0000-4000-8000-000000000003', ?::uuid, NULL, \
                 6, 0, 2, 12) ON CONFLICT DO NOTHING",
        vec![Value::from(&uuid(WH_FROM))],
        "第三物料库存",
    )
    .await;
    // 库位账同步（settle 末尾的「两本账相符」要求基线就相等）
    exec(
        "INSERT INTO biz_inventory_location \
             (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (gen_random_uuid(), 'd3b0000c-0000-4000-8000-000000000003', ?::uuid, NULL, 6, 0)",
        vec![Value::from(&uuid(WH_FROM))],
        "第三物料库位账",
    )
    .await;
    let mut sn = item(M_SN, "1");
    sn.serial_ids = vec![SERIAL_1.to_string()];
    let id = create_and_ship(vec![
        item(M_PLAIN, "5"),
        sn,
        item("d3b0000c-0000-4000-8000-000000000003", "4"),
    ])
    .await;
    receive_ok(id).await;

    assert_eq!(needed(M_PLAIN, WH_FROM).await, dec("95"));
    assert_eq!(needed(M_SN, WH_FROM).await, dec("1"));
    assert_eq!(
        bucket_qty("d3b0000c-0000-4000-8000-000000000003", WH_FROM)
            .await
            .expect("第三物料库存行"),
        dec("2")
    );
    assert_eq!(needed(M_PLAIN, WH_TO).await, dec("15"));
    assert_eq!(needed(M_SN, WH_TO).await, dec("1"));
    assert_eq!(
        bucket_qty("d3b0000c-0000-4000-8000-000000000003", WH_TO)
            .await
            .expect("第三物料在总仓建档"),
        dec("4")
    );
    // 6 条流水（3 行 × 进出各一条）
    assert_eq!(txns_of(id).await.len(), 6, "三行必须各写进/出两条流水");
    let detail = detail_of(id).await.expect("详情");
    assert!(
        detail
            .items
            .iter()
            .all(|item| item.received_qty == item.qty),
        "三行都必须回填 received_qty"
    );
}

// ---------- W3c ⑨ 并发：两笔各扣 5、库存 8 → 一个成功一个 409 ----------
#[tokio::test]
async fn w3c_settle_concurrent_transfers_only_one_wins() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 把分仓库存压到 8（两笔各 5 只能成一笔）
    exec(
        "UPDATE biz_inventory SET qty = 8, amount = 8 * avg_cost \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
        "压库存到 8",
    )
    .await;
    // 库位账跟着压到同数：不压的话成功那笔结算会被「两本账相符」拦下（本卡新增的不变式④），
    // 这里要验的是并发抢扣（一笔 200 一笔 409），不是两账不符
    exec(
        "UPDATE biz_inventory_location SET qty = 8, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_FROM))],
        "压库位账到 8",
    )
    .await;
    let a = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let b = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let (ra, rb) = tokio::join!(receive(a), receive(b));
    let mut statuses = Vec::new();
    for (id, result) in [(a, ra), (b, rb)] {
        match result {
            Ok(()) => statuses.push((id, 200)),
            Err(err) => statuses.push((id, err.status)),
        }
    }
    statuses.sort_by_key(|(_, status)| *status);
    assert_eq!(
        statuses
            .iter()
            .map(|(_, status)| *status)
            .collect::<Vec<_>>(),
        vec![200, 409],
        "库存 8、两笔各扣 5：必须一个成功一个 409，实际 {statuses:?}"
    );
    assert_eq!(needed(M_PLAIN, WH_FROM).await, dec("3"), "只能扣掉一笔");
    // 成功那张单是 DONE，失败那张单仍在 IN_TRANSIT（回滚）
    let done = statuses
        .iter()
        .find(|(_, s)| *s == 200)
        .expect("成功的那张")
        .0;
    let lost = statuses
        .iter()
        .find(|(_, s)| *s == 409)
        .expect("失败的那张")
        .0;
    assert_eq!(refresh(done).await.status, STATUS_DONE);
    assert_eq!(refresh(lost).await.status, STATUS_IN_TRANSIT);
}

// ---------- W3c ⑩ 库存不足 → 409 + 状态与库存都不动（回滚验证） ----------
#[tokio::test]
async fn w3c_settle_insufficient_stock_rolls_back_to_in_transit() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    // 发货后库存被别人扣走（模拟并发 / 手工改库）
    exec(
        "UPDATE biz_inventory SET qty = 1, amount = 1 * avg_cost \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
        "压库存到 1",
    )
    .await;
    // 库位账同步（保持两账基线相等；本用例走的是「成本账扣不动 → 409」的早退分支）
    exec(
        "UPDATE biz_inventory_location SET qty = 1, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_FROM))],
        "压库位账到 1",
    )
    .await;
    let err = receive(id).await.expect_err("库存 1 调 5 必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("库存不足"), "{}", err.msg);
    // 状态回滚
    assert_eq!(
        refresh(id).await.status,
        STATUS_IN_TRANSIT,
        "结算失败后必须仍是 IN_TRANSIT"
    );
    // 库存一行都没动、流水一条都没写
    assert_eq!(needed(M_PLAIN, WH_FROM).await, dec("1"), "调出仓不得被扣");
    assert_eq!(needed(M_PLAIN, WH_TO).await, dec("10"), "调入仓不得增加");
    assert!(txns_of(id).await.is_empty(), "失败必须没有流水");
    let detail = detail_of(id).await.expect("详情");
    assert_eq!(detail.items[0].received_qty, Decimal::ZERO, "明细不得回填");
}

// ---------- W3c ⑪ 一物一码：qty == COUNT(serial IN_STOCK) ----------
#[tokio::test]
async fn w3c_settle_keeps_serial_invariant() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let mut line = item(M_SN, "2");
    line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
    let id = create_and_ship(vec![line]).await;
    receive_ok(id).await;
    for warehouse in [WH_FROM, WH_TO] {
        let book = needed(M_SN, warehouse).await;
        let serials = serial_in_stock_count(M_SN, warehouse).await;
        assert_eq!(
            book,
            Decimal::from(serials),
            "不变式③：{warehouse} 的账面数量必须等于在库序列号件数"
        );
    }
    assert_eq!(needed(M_SN, WH_FROM).await, Decimal::ZERO);
    assert_eq!(needed(M_SN, WH_TO).await, dec("2"));
}

// ---------- W3c ⑫ 按数量管理的物料：不动任何序列号 ----------
#[tokio::test]
async fn w3c_settle_does_not_touch_serials_of_quantity_material() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let before = serial_in_stock_count(M_SN, WH_FROM).await;
    let id = create_and_ship(vec![item(M_PLAIN, "3")]).await;
    receive_ok(id).await;
    assert_eq!(
        serial_in_stock_count(M_SN, WH_FROM).await,
        before,
        "数量物料调拨不得影响任何序列号"
    );
    assert_eq!(serial_warehouse(SERIAL_1).await, Some(uuid(WH_FROM)));
    assert_eq!(serial_warehouse(SERIAL_2).await, Some(uuid(WH_FROM)));
}

// ---------- W3c ⑬ 序列号个数 != 数量 → 400 ----------
#[tokio::test]
async fn w3c_settle_rejects_serial_count_mismatch() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let mut line = item(M_SN, "2");
    line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
    let id = create_and_ship(vec![line]).await;
    // 建单后被改库：只留一条序列号关联（数量 2 vs 序列号 1）
    exec(
        "DELETE FROM biz_stock_transfer_item_serial \
          WHERE id IN (SELECT l.id FROM biz_stock_transfer_item_serial l \
                         JOIN biz_stock_transfer_item i ON i.id = l.item_id \
                        WHERE i.transfer_id = ?::uuid \
                        ORDER BY l.created_at DESC LIMIT 1)",
        vec![Value::from(&id)],
        "删一条序列号关联",
    )
    .await;
    let err = receive(id)
        .await
        .expect_err("序列号 1 个 vs 数量 2 必须 400");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("序列号"), "{}", err.msg);
    assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
    assert_eq!(
        needed(M_SN, WH_FROM).await,
        dec("2"),
        "校验失败不得先扣库存"
    );
}

// ---------- W3c ⑭ 幂等：结算两次不重复扣 ----------
#[tokio::test]
async fn w3c_settle_twice_is_idempotent() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(id).await;
    let row = refresh(id).await;
    let qty_after = needed(M_PLAIN, WH_FROM).await;
    let amount_after = bucket_amount(M_PLAIN, WH_FROM).await.expect("金额");
    // 直接再调一次模型层（模拟运维重放 / 回调重复）
    let outcome = transfer_model::settle_transfer(
        db(),
        row.id,
        &row.transfer_no,
        row.from_warehouse_id,
        row.to_warehouse_id,
        Some(uuid(U_APPLICANT)),
        Utc::now(),
    )
    .await
    .expect("重复调用必须安全返回（幂等）而不是报错");
    assert!(!outcome.settled, "第二次必须是幂等跳过");
    assert_eq!(needed(M_PLAIN, WH_FROM).await, qty_after, "不重复扣");
    assert_eq!(
        bucket_amount(M_PLAIN, WH_FROM).await.expect("金额"),
        amount_after,
        "不重复扣金额"
    );
    assert_eq!(txns_of(id).await.len(), 2, "流水仍然只有 2 条");
}

// ---------- W3c ⑮ 终态与非法来源：DONE 之后 / 其它状态 → 409 ----------
#[tokio::test]
async fn w3c_done_is_sticky_and_other_states_rejected() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "1")]).await;
    receive_ok(id).await;
    let qty_after = needed(M_PLAIN, WH_FROM).await;
    // DONE → DONE：非法流转（幂等保护在接口层）
    let err = receive(id).await.expect_err("DONE 之后再调必须 409");
    assert_eq!(err.status, 409);
    assert!(err.msg.contains("非法状态流转"), "{}", err.msg);
    assert_eq!(needed(M_PLAIN, WH_FROM).await, qty_after, "不得二次扣");

    // 取消单不能收货
    let cancelled = create_transfer(
        &body(vec![item(M_PLAIN, "1")]),
        Some(uuid(U_APPLICANT)),
        None,
    )
    .await
    .expect("建单");
    change_transfer_status(
        cancelled,
        &status_req(STATUS_CANCELLED, Some("不调了")),
        uuid(U_APPLICANT),
    )
    .await
    .expect("DRAFT 能直接取消");
    let err = receive(cancelled).await.expect_err("CANCELLED 不能收货");
    assert_eq!(err.status, 409);

    // 草稿不能跳级收货
    let draft = create_transfer(
        &body(vec![item(M_PLAIN, "1")]),
        Some(uuid(U_APPLICANT)),
        None,
    )
    .await
    .expect("建单");
    let err = receive(draft).await.expect_err("DRAFT 不能收货");
    assert_eq!(err.status, 409);
}

// ---------- W3c ⑯ 序列号已不在调出仓 → 409 且整笔回滚 ----------
#[tokio::test]
async fn w3c_settle_rejects_serial_outside_source() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let mut line = item(M_SN, "2");
    line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
    let id = create_and_ship(vec![line]).await;
    // 发货后有人把 SERIAL_2 挪去总仓（另一张单据出库 / 手工改库）
    exec(
        "UPDATE biz_material_serial SET warehouse_id = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&uuid(WH_TO)), Value::from(&uuid(SERIAL_2))],
        "挪走序列号",
    )
    .await;
    let err = receive(id).await.expect_err("序列号不全在调出仓必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("序列号"), "{}", err.msg);
    // 整笔回滚：库存、SERIAL_1 都回到原样
    assert_eq!(needed(M_SN, WH_FROM).await, dec("2"));
    assert_eq!(
        serial_warehouse(SERIAL_1).await,
        Some(uuid(WH_FROM)),
        "先转移成功的序列号也必须回滚"
    );
    assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
    assert!(txns_of(id).await.is_empty());
}

// ---------- W3c ⑰ 明细回填：shipped/received/unit_cost/amount ----------
#[tokio::test]
async fn w3c_settle_fills_item_shipped_received_and_cost() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    let before = transfer_model::items_of(id).await.expect("明细");
    assert_eq!(
        before[0].shipped_qty,
        Decimal::ZERO,
        "IN_TRANSIT 时还没回填"
    );
    receive_ok(id).await;
    let items = transfer_model::items_of(id).await.expect("明细");
    assert_eq!(items[0].shipped_qty, dec("5"), "shipped_qty = qty");
    assert_eq!(items[0].received_qty, dec("5"), "received_qty = qty");
    assert_eq!(items[0].unit_cost, dec("12.5"), "unit_cost = 原价快照");
    assert_eq!(items[0].amount, dec("62.5"), "amount = 数量 × 原价");
    assert_eq!(
        refresh(id).await.total_amount,
        dec("62.5"),
        "单据头金额合计"
    );
    // 接口视图（详情）也要能看到已发/已收
    let detail = detail_of(id).await.expect("详情");
    assert_eq!(detail.items[0].shipped_qty, dec("5"));
    assert_eq!(detail.items[0].received_qty, dec("5"));
}

// ---------- W3c ⑱ 库位维度：没有汇总桶的仓按库位行记账 ----------
#[tokio::test]
async fn w3c_settle_uses_location_row_when_no_bucket() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 把 M_PLAIN 的整仓汇总桶换成两个库位行（模拟纯库位记账的仓）
    exec(
        "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ANY(?::text::uuid[])",
        vec![
            Value::from(&uuid(M_PLAIN)),
            array_param(&[uuid(WH_FROM), uuid(WH_TO)]),
        ],
        "清掉汇总桶",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                    avg_cost, amount) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, 7, 0, 4, 28)",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(LOC_FROM)),
        ],
        "库位库存行",
    )
    .await;
    // 库位账同样换成「按库位行记账」：LOC_FROM 7（与成本账行同数）、LOC_TO 0（待调入时加）。
    // 本卡起 settle 会写库位账并在末尾比对两本账，所以这里必须从「两账相等」的状态起步。
    exec(
        "UPDATE biz_inventory_location SET qty = 7, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_FROM))],
        "库位账 LOC_FROM = 7",
    )
    .await;
    exec(
        "UPDATE biz_inventory_location SET qty = 0, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_TO))],
        "库位账 LOC_TO = 0",
    )
    .await;
    let mut line = item(M_PLAIN, "3");
    line.from_location_id = Some(LOC_FROM.to_string());
    line.to_location_id = Some(LOC_TO.to_string());
    let id = create_and_ship(vec![line]).await;
    receive_ok(id).await;
    // 调出：库位行 7 → 4（金额 28 → 16，原价 4）
    let from_row = query(
        "SELECT qty::numeric(18,4) AS qty, amount::numeric(18,4) AS amount \
           FROM biz_inventory WHERE location_id = ?::uuid",
        vec![Value::from(&uuid(LOC_FROM))],
    )
    .await;
    assert_eq!(
        from_row[0].get_typed::<Decimal>("qty").expect("qty"),
        dec("4")
    );
    assert_eq!(
        from_row[0].get_typed::<Decimal>("amount").expect("amount"),
        dec("16")
    );
    // 调入：LOC_TO 建档 qty 3 / 均价 4 / 金额 12
    let to_row = query(
        "SELECT qty::numeric(18,4) AS qty, amount::numeric(18,4) AS amount, \
                avg_cost::numeric(18,6) AS avg_cost \
           FROM biz_inventory WHERE location_id = ?::uuid",
        vec![Value::from(&uuid(LOC_TO))],
    )
    .await;
    assert_eq!(
        to_row[0].get_typed::<Decimal>("qty").expect("qty"),
        dec("3")
    );
    assert_eq!(
        to_row[0].get_typed::<Decimal>("amount").expect("amount"),
        dec("12")
    );
    assert_eq!(
        to_row[0]
            .get_typed::<Decimal>("avg_cost")
            .expect("avg_cost"),
        dec("4")
    );
}

// ---------- W3c ⑲ 结算金额与流水金额一致（ROUND 口径） ----------
#[tokio::test]
async fn w3c_settle_amount_rounding_matches_txn() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 均价改成 3.333333（6 位小数）× 3 件 = 9.999999 → 必须四舍五入成 10.0000 而不是各算一套
    exec(
        "UPDATE biz_inventory SET avg_cost = 3.333333, amount = qty * 3.333333 \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
        "改均价",
    )
    .await;
    let id = create_and_ship(vec![item(M_PLAIN, "3")]).await;
    receive_ok(id).await;
    let txns = txns_of(id).await;
    let amounts: Vec<String> = txns.iter().map(|row| txn_text(row, "amount")).collect();
    assert_eq!(
        amounts,
        vec!["10.0000".to_string(), "10.0000".to_string()],
        "两条流水金额必须逐字相同（同一个 ROUND 结果）"
    );
    assert_eq!(refresh(id).await.total_amount, dec("10"));
}

// ---------- P0-03 回归：汇总桶 + 库位桶并存时，两本账必须扣同一个桶 ----------
//
// 145 真机演练（P0-03）复现的漂移：同仓同时有「整仓汇总桶（`location_id IS NULL`）」
// 和「具体库位桶」时，明细带了 `from_location_id`，成本账按「汇总桶优先」扣汇总桶、
// 库位账却按明细库位扣具体库位桶 —— 两本账**合计**仍相等（`assert_settle_invariants`
// 的第 4 条看不出来），但分库位库存永久错位（实测调拨 40 件后库位桶 50 / 10、汇总桶 42 / 82）。
//
// 修复后的判据：库位账跟着成本账实际扣的那个桶走，逐桶相等。
#[tokio::test]
async fn p003_settle_deducts_the_same_bucket_in_both_books() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 种子库位字典（V0138）里的固定行：只当弱引用（`biz_inventory_location.location_id`
    // 存的是库位字典 id，无外键），不新建也不改它。
    const LOC_DICT: &str = "d1370002-0000-4000-8000-000000000001";

    // ① 把 LOC_FROM 这条库位账行改成「具体库位桶」：库位字典 = LOC_DICT，数量 6
    exec(
        "UPDATE biz_inventory_location SET location_id = ?::uuid, qty = 6, updated_at = now() \
          WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_DICT)), Value::from(&uuid(LOC_FROM))],
        "库位账：具体库位桶 6",
    )
    .await;
    // ② 汇总库位桶补一行 100（与 prepare 的整仓成本账 100 对齐）
    exec(
        "INSERT INTO biz_inventory_location \
             (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 100, 0)",
        vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
        "库位账：汇总桶 100",
    )
    .await;
    // ③ 具体库位桶的成本账行（qty 6 / 均价 4 / 金额 24）
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                    avg_cost, amount) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, 6, 0, 4, 24)",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(LOC_FROM)),
        ],
        "成本账：具体库位桶 6",
    )
    .await;
    assert_two_books(M_PLAIN, "构造基线").await;

    // ④ 调拨 3 件：明细指定「具体库位」，同仓另有 100 的整仓汇总桶
    let mut line = item(M_PLAIN, "3");
    line.from_location_id = Some(LOC_FROM.to_string());
    let id = create_and_ship(vec![line]).await;
    receive_ok(id).await;

    // ⑤ FIX-4 问题 2（改口径）：**扣的就是明细指定的库位桶** 6 → 3（金额 24 → 3×4=12）。
    //    修复前是「有整仓汇总桶就只扣汇总桶」—— 审批人看到的是库位级意图、账上动的是汇总桶。
    let (qty, amount) = cost_bucket_of(M_PLAIN, WH_FROM, Some(LOC_FROM))
        .await
        .expect("指定库位桶的成本账行必须还在");
    assert_eq!(qty, dec("3"), "调出：指定库位桶成本账 6 → 3");
    assert_eq!(amount, dec("12"), "调出：指定库位桶金额 24 → 12（原价 4）");
    // ⑥ 整仓汇总桶一分不动（成本账 100 / 1250，库位账 100）
    let (summary_qty, summary_amount) = cost_bucket_of(M_PLAIN, WH_FROM, None)
        .await
        .expect("整仓汇总桶的成本账行必须还在");
    assert_eq!(summary_qty, dec("100"), "调出：整仓汇总桶成本账不许被动");
    assert_eq!(summary_amount, dec("1250"), "调出：整仓汇总桶金额不许被动");
    assert_eq!(
        location_row(LOC_FROM).await,
        dec("3"),
        "调出：指定库位桶的库位账必须与成本账同桶（6 → 3）"
    );
    assert_eq!(
        summary_ledger_qty(M_PLAIN, WH_FROM).await,
        dec("100"),
        "调出：整仓汇总桶的库位账不许被动"
    );
    // ⑦ 逐桶对账：每个 (物料, 仓, 库位桶) 上成本账 == 库位账（0 行差异）
    let broken = query(
        "WITH c AS ( \
             SELECT COALESCE(il.location_id::text, '-') AS loc, SUM(i.qty) AS qty \
               FROM biz_inventory i \
               LEFT JOIN biz_inventory_location il ON il.id = i.location_id \
              WHERE i.material_id = ?::uuid AND i.warehouse_id = ?::uuid \
              GROUP BY 1), \
         l AS ( \
             SELECT COALESCE(location_id::text, '-') AS loc, SUM(qty) AS qty \
               FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
              GROUP BY 1) \
         SELECT COALESCE(c.loc, l.loc) AS loc, \
                COALESCE(c.qty, 0) - COALESCE(l.qty, 0) AS diff \
           FROM c FULL JOIN l ON c.loc = l.loc \
          WHERE COALESCE(c.qty, 0) <> COALESCE(l.qty, 0)",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
        ],
    )
    .await;
    assert!(
        broken.is_empty(),
        "分库位两本账必须逐桶相等，实际有 {} 行差额",
        broken.len()
    );
}

// ============================================================================
// FIX-4 问题 2（t_763677cd）：调拨明细指定库位 → 扣的就是那个库位桶
//
// 修复前：`resolve_inventory_row` 是「有整仓汇总桶就只动汇总桶」（`ORDER BY (location_id
// IS NULL) DESC`），明细点名库位 A 也不报错、不提示，账上扣的是汇总桶 —— 审批人看到
// 库位级意图、账上动的是汇总桶（P2）。修复后 `pick_bucket` 是「精确匹配优先」：
// 指定了库位就只动那个库位桶，该桶不够/不存在就**明确报错**，绝不静默改扣。
// ============================================================================

/// 给 WH_FROM 造一条「具体库位桶」的库存账 + 库位账（物料 M_PLAIN）。
///
/// 返回 `(库位字典 id, 库位账行 id)`；数量为 0 时只建库位账行、**不建成本账行**
/// （= 「该库位没有这个物料的库存账」，FIX-4 报错路径的触发条件）。
async fn seed_plain_location_bucket(
    dict_id: &str,
    dict_code: &str,
    row_id: &str,
    qty: Option<&str>,
) -> (Uuid, Uuid) {
    exec(
        "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type, \
                                   is_default, capacity, status, sort_order) \
         VALUES (?::uuid, ?, 'FIX-4 夹具库位', ?::uuid, 'STORAGE', 0, NULL, 1, 9) \
         ON CONFLICT DO NOTHING",
        vec![
            Value::from(&uuid(dict_id)),
            Value::Text(dict_code.to_string()),
            Value::from(&uuid(WH_FROM)),
        ],
        "FIX-4 夹具库位字典",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (?::uuid, ?::uuid, ?::uuid, ?::uuid, 0, 0) ON CONFLICT DO NOTHING",
        vec![
            Value::from(&uuid(row_id)),
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(dict_id)),
        ],
        "FIX-4 夹具库位账行",
    )
    .await;
    if let Some(qty) = qty {
        exec(
            "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                        avg_cost, amount) \
             VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, 0, 4, \
                     NULLIF(?::text,'')::numeric * 4) ON CONFLICT DO NOTHING",
            vec![
                Value::from(&uuid(M_PLAIN)),
                Value::from(&uuid(WH_FROM)),
                Value::from(&uuid(row_id)),
                Value::Text(qty.to_string()),
                Value::Text(qty.to_string()),
            ],
            "FIX-4 夹具库位成本账行",
        )
        .await;
        exec(
            "UPDATE biz_inventory_location SET qty = NULLIF(?::text,'')::numeric, updated_at = now() \
              WHERE id = ?::uuid",
            vec![
                Value::Text(qty.to_string()),
                Value::from(&uuid(row_id)),
            ],
            "FIX-4 夹具库位账数量",
        )
        .await;
    }
    (uuid(dict_id), uuid(row_id))
}

/// 某库存桶的成本账 `(qty, amount)`：`None` = 整仓汇总桶。
async fn cost_bucket_of(material: &str, warehouse: &str, row: Option<&str>) -> Option<(Decimal, Decimal)> {
    let rows = query(
        "SELECT qty::numeric(18,4) AS qty, amount::numeric(18,4) AS amount FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
            AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
              = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid)",
        vec![
            Value::from(&uuid(material)),
            Value::from(&uuid(warehouse)),
            Value::Text(row.unwrap_or_default().to_string()),
        ],
    )
    .await;
    rows.first().map(|row| {
        (
            row.get_typed::<Decimal>("qty").expect("qty"),
            row.get_typed::<Decimal>("amount").expect("amount"),
        )
    })
}

/// 整仓汇总桶（`location_id IS NULL`）的库位账数量。
async fn summary_ledger_qty(material: &str, warehouse: &str) -> Decimal {
    let rows = query(
        "SELECT COALESCE(SUM(qty), 0)::numeric(18,4) AS qty FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
        vec![Value::from(&uuid(material)), Value::from(&uuid(warehouse))],
    )
    .await;
    rows[0].get_typed::<Decimal>("qty").expect("汇总桶库位账")
}

/// 指定库位桶的成本账 + 库位账数量（断言「明确报错时账一分不动」）。
async fn bucket_snapshot(row: Option<&str>) -> (Option<(Decimal, Decimal)>, Decimal) {
    let cost = match row {
        Some(row_id) => cost_bucket_of(M_PLAIN, WH_FROM, Some(row_id)).await,
        None => cost_bucket_of(M_PLAIN, WH_FROM, None).await,
    };
    let ledger = match row {
        Some(row_id) => location_row(row_id).await,
        None => summary_ledger_qty(M_PLAIN, WH_FROM).await,
    };
    (cost, ledger)
}

/// 明细指定库位、但该库位桶的库存**不够**：409 + 点名库位与可用量，账一分不动。
#[tokio::test]
async fn fix4_settle_rejects_when_specified_location_short() {
    let Some(_guard) = prepare().await else {
        return;
    };
    const LOC_DICT: &str = "d1370002-0000-4000-8000-000000000001";
    // 指定库位桶 6 件（成本账 6 @4 = 24），同仓另有 100 的整仓汇总桶
    exec(
        "UPDATE biz_inventory_location SET location_id = ?::uuid, qty = 6, updated_at = now() \
          WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_DICT)), Value::from(&uuid(LOC_FROM))],
        "库位账：具体库位桶 6",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory_location \
             (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, NULL, 100, 0)",
        vec![Value::from(&uuid(M_PLAIN)), Value::from(&uuid(WH_FROM))],
        "库位账：汇总桶 100",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                    avg_cost, amount) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, 6, 0, 4, 24)",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(LOC_FROM)),
        ],
        "成本账：具体库位桶 6",
    )
    .await;
    // 库位字典的编码（报错文案里点名用）
    let dict_code: String = query(
        "SELECT location_code FROM biz_location WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_DICT))],
    )
    .await[0]
        .get_typed::<String>("location_code")
        .expect("库位编码");

    let mut line = item(M_PLAIN, "10"); // 该库位只有 6 → 不够（汇总桶有 100 也不许改扣）
    line.from_location_id = Some(LOC_FROM.to_string());
    let id = create_and_ship(vec![line]).await;
    let error = receive(id).await.expect_err("指定库位不够必须报错，不许静默改扣汇总桶");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(error.code, error_code::ERP_CODE_CONFLICT, "{}", error.msg);
    assert!(
        error.msg.contains("当前可用 6") && error.msg.contains(&dict_code),
        "报文必须点名「哪个库位桶不够、可用多少」：{}",
        error.msg
    );

    // 整笔回滚：单据留在 IN_TRANSIT，两本账 + 流水一分不动
    assert_eq!(
        refresh(id).await.status,
        STATUS_IN_TRANSIT,
        "被拒的结算不能把单据推到 DONE"
    );
    let (cost, ledger) = bucket_snapshot(Some(LOC_FROM)).await;
    assert_eq!(cost, Some((dec("6"), dec("24"))), "指定库位桶成本账不许动");
    assert_eq!(ledger, dec("6"), "指定库位桶库位账不许动");
    let (summary_cost, summary_ledger) = bucket_snapshot(None).await;
    assert_eq!(
        summary_cost,
        Some((dec("100"), dec("1250"))),
        "整仓汇总桶不许被改扣"
    );
    assert_eq!(summary_ledger, dec("100"), "整仓汇总桶库位账不许被改扣");
    assert!(txns_of(id).await.is_empty(), "被拒的结算不许写流水");
}

/// 明细指定库位、但该库位**没有该物料的库存账**：409 + 明确说「不会改扣汇总桶」。
#[tokio::test]
async fn fix4_settle_rejects_when_specified_location_has_no_bucket() {
    let Some(_guard) = prepare().await else {
        return;
    };
    const LOC_DICT2: &str = "d3b0000e-0000-4000-8000-0000000000f4";
    const LOC_ROW2: &str = "d3b0000f-0000-4000-8000-0000000000f4";
    // 新库位：只有库位账行（qty 0），**没有成本账行** —— 历史漂移 / 刚建的库位就是这个样子
    seed_plain_location_bucket(LOC_DICT2, "W3B-L-FIX4", LOC_ROW2, None).await;

    let mut line = item(M_PLAIN, "3");
    line.from_location_id = Some(LOC_ROW2.to_string());
    let id = create_and_ship(vec![line]).await;
    let error = receive(id).await.expect_err("指定库位没有库存账时必须报错");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert!(
        error.msg.contains("调拨不会改扣整仓汇总桶"),
        "报文必须明确「不改扣汇总桶」：{}",
        error.msg
    );
    assert!(
        error.msg.contains("整仓汇总桶上该物料还有 100"),
        "报文要给出汇总桶可用量，便于用户判断下一步：{}",
        error.msg
    );
    assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
    let (summary_cost, summary_ledger) = bucket_snapshot(None).await;
    assert_eq!(
        summary_cost,
        Some((dec("100"), dec("1250"))),
        "整仓汇总桶不许被改扣"
    );
    assert_eq!(summary_ledger, dec("100"), "整仓汇总桶库位账不许被改扣");
    assert!(txns_of(id).await.is_empty(), "被拒的结算不许写流水");
}

/// 未指定库位、该物料在该仓**只有一个库位桶**：扣它（唯一可选，不算改扣）。
#[tokio::test]
async fn fix4_settle_deducts_the_only_location_bucket_when_unspecified() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 纯库位记账的仓：没有整仓汇总桶，只有 LOC_FROM 这一行
    exec(
        "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ANY(?::text::uuid[])",
        vec![
            Value::from(&uuid(M_PLAIN)),
            array_param(&[uuid(WH_FROM), uuid(WH_TO)]),
        ],
        "清掉汇总桶",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                    avg_cost, amount) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, 7, 0, 4, 28)",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(LOC_FROM)),
        ],
        "库位库存行",
    )
    .await;
    exec(
        "UPDATE biz_inventory_location SET qty = 7, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_FROM))],
        "库位账 LOC_FROM = 7",
    )
    .await;
    exec(
        "UPDATE biz_inventory_location SET qty = 0, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_TO))],
        "库位账 LOC_TO = 0",
    )
    .await;

    // 明细**不指定**调出库位：唯一库位桶可扣 → 走它
    let id = create_and_ship(vec![item(M_PLAIN, "3")]).await;
    receive_ok(id).await;
    let (qty, amount) = cost_bucket_of(M_PLAIN, WH_FROM, Some(LOC_FROM))
        .await
        .expect("唯一库位桶的成本账行");
    assert_eq!(qty, dec("4"), "未指定库位 → 扣唯一库位桶 7 → 4");
    assert_eq!(amount, dec("16"), "金额 28 → 16（原价 4）");
    assert_eq!(location_row(LOC_FROM).await, dec("4"), "库位账同桶 7 → 4");
    assert_two_books(M_PLAIN, "唯一库位桶结算后").await;
}

/// 未指定库位、库存分散在**多个库位桶**：409 要求点名库位（不猜）。
#[tokio::test]
async fn fix4_settle_rejects_ambiguous_bucket_when_unspecified() {
    let Some(_guard) = prepare().await else {
        return;
    };
    const LOC_DICT2: &str = "d3b0000e-0000-4000-8000-0000000000f5";
    const LOC_ROW2: &str = "d3b0000f-0000-4000-8000-0000000000f5";
    // 两个库位桶，没有整仓汇总桶
    exec(
        "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ANY(?::text::uuid[])",
        vec![
            Value::from(&uuid(M_PLAIN)),
            array_param(&[uuid(WH_FROM), uuid(WH_TO)]),
        ],
        "清掉汇总桶",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                                    avg_cost, amount) \
         VALUES (gen_random_uuid(), ?::uuid, ?::uuid, ?::uuid, 7, 0, 4, 28)",
        vec![
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(LOC_FROM)),
        ],
        "库位桶 1 的成本账",
    )
    .await;
    exec(
        "UPDATE biz_inventory_location SET qty = 7, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_FROM))],
        "库位桶 1 的库位账",
    )
    .await;
    exec(
        "UPDATE biz_inventory_location SET qty = 0, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_TO))],
        "库位账 LOC_TO = 0",
    )
    .await;
    seed_plain_location_bucket(LOC_DICT2, "W3B-L-FIX4B", LOC_ROW2, Some("3")).await;

    let id = create_and_ship(vec![item(M_PLAIN, "2")]).await;
    let error = receive(id).await.expect_err("多库位桶且未指定库位必须报错");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert!(
        error.msg.contains("分散在 2 个库位桶") && error.msg.contains("请在明细里指定调出库位"),
        "报文必须要求点名库位：{}",
        error.msg
    );
    assert_eq!(refresh(id).await.status, STATUS_IN_TRANSIT);
    assert!(txns_of(id).await.is_empty(), "被拒的结算不许写流水");
}


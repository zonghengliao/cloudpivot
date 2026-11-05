// ============================================================================
// transfer_handler_db_tests_books.rs —— 真库：成本账与库位账两本账始终相等（U1–U4）
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::db_tests_fixtures::*;
use crate::business::erp::stockin_model;
use crate::business::erp::transfer_dto::CreateTransferReq;
use crate::business::erp::transfer_model::STATUS_IN_TRANSIT;
use nik::prelude::chrono::Utc;
use nik::prelude::*;

/// U1 普通调拨（汇总桶 → 汇总桶）：结算后「库位账合计 == 成本账 qty」，调出/调入各一断言。
#[tokio::test]
async fn u1_plain_transfer_keeps_two_books_equal() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 前置：夹具基线两本账就相等（调出仓 100 / 调入仓 10）
    assert_eq!(
        location_book(M_PLAIN, WH_FROM).await,
        dec("100"),
        "前置：调出仓库位账基线"
    );
    assert_eq!(
        location_book(M_PLAIN, WH_TO).await,
        dec("10"),
        "前置：调入仓库位账基线"
    );
    let id = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(id).await;
    // 调出仓 / 调入仓两侧的数量账都要动（本卡修的就是这两笔原来根本没写）
    assert_eq!(
        location_book(M_PLAIN, WH_FROM).await,
        dec("95"),
        "调出仓库位账必须 -5"
    );
    assert_eq!(
        location_book(M_PLAIN, WH_TO).await,
        dec("15"),
        "调入仓库位账必须 +5"
    );
    // 未指定库位 ⇒ 落「未分配库位桶」那一行（与收货过账同口径）
    assert_eq!(location_row(LOC_FROM).await, dec("95"), "调出仓桶行 qty");
    assert_eq!(location_row(LOC_TO).await, dec("15"), "调入仓桶行 qty");
    assert_two_books(M_PLAIN, "U1 结算后").await;
    // 成本账没被新逻辑碰到（口径不变，仍是 95 / 15）
    assert_eq!(cost_book(M_PLAIN, WH_FROM).await, dec("95"));
    assert_eq!(cost_book(M_PLAIN, WH_TO).await, dec("15"));
}

/// U2 带库位的调拨（`from_location_id` / `to_location_id` 指向 `biz_inventory_location.id`）：
/// 调出库位行减、调入库位行加，两仓仓级合计仍与成本账相等。
#[tokio::test]
async fn u2_transfer_with_location_rows_moves_both_books() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 换成「纯库位记账」的仓：成本账与库位账都只有 LOC_FROM 7（@4 = 28）/ LOC_TO 0
    // （删除顺序按外键：biz_inventory.location_id → biz_inventory_location.id）
    let warehouses = vec![uuid(WH_FROM), uuid(WH_TO)];
    exec(
        "DELETE FROM biz_inventory WHERE material_id = ?::uuid AND warehouse_id = ANY(?::text::uuid[])",
        vec![Value::from(&uuid(M_PLAIN)), array_param(&warehouses)],
        "U2 清掉 M_PLAIN 成本账行",
    )
    .await;
    exec(
        "DELETE FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ANY(?::text::uuid[])",
        vec![Value::from(&uuid(M_PLAIN)), array_param(&warehouses)],
        "U2 清掉 M_PLAIN 库位账行",
    )
    .await;
    exec(
        "INSERT INTO biz_inventory_location \
             (id, material_id, warehouse_id, location_id, qty, locked_qty) \
         VALUES (?::uuid, ?::uuid, ?::uuid, NULL, 7, 0), \
                (?::uuid, ?::uuid, ?::uuid, NULL, 0, 0)",
        vec![
            Value::from(&uuid(LOC_FROM)),
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_FROM)),
            Value::from(&uuid(LOC_TO)),
            Value::from(&uuid(M_PLAIN)),
            Value::from(&uuid(WH_TO)),
        ],
        "U2 库位账行",
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
        "U2 成本账行（按库位）",
    )
    .await;
    assert_two_books(M_PLAIN, "U2 前置").await;

    let mut line = item(M_PLAIN, "3");
    line.from_location_id = Some(LOC_FROM.to_string());
    line.to_location_id = Some(LOC_TO.to_string());
    let id = create_and_ship(vec![line]).await;
    receive_ok(id).await;

    assert_eq!(
        location_row(LOC_FROM).await,
        dec("4"),
        "调出库位行必须 7 → 4"
    );
    assert_eq!(
        location_row(LOC_TO).await,
        dec("3"),
        "调入库位行必须 0 → 3（不是新建一行）"
    );
    // 仓级合计：成本账（LOC_FROM 4 行 + 调入侧按 LOC_TO 建档 3）与库位账逐仓相等
    assert_eq!(cost_book(M_PLAIN, WH_FROM).await, dec("4"));
    assert_eq!(cost_book(M_PLAIN, WH_TO).await, dec("3"));
    assert_two_books(M_PLAIN, "U2 带库位调拨后").await;
    // 库位账行数不因调拨而增加（未分配桶没被顺手建出来）
    let rows = query(
        "SELECT COUNT(*) AS cnt FROM biz_inventory_location \
          WHERE material_id = ?::uuid AND warehouse_id = ANY(?::text::uuid[])",
        vec![Value::from(&uuid(M_PLAIN)), array_param(&warehouses)],
    )
    .await;
    assert_eq!(
        rows[0].get_typed::<i64>("cnt").expect("cnt"),
        2,
        "两个仓各一行库位账，调拨不得新增未分配桶"
    );
}

/// U3 调拨与「入库/出库」交替：每笔作业后两本账都相等，最终数量守恒。
///
/// # 卡面 U3 的落地口径（与卡面的差异逐条说清，**不是漏做**）
///
/// 卡面写「调拨 + 报废/出库交替后 `SUM(库位账) == SUM(成本账)`」。实测**主干的
/// 序列号报废/出库路径并不写库位账**：
///
/// ```text
/// serial_model::transition_status_with(IN_STOCK → OUTBOUND|SCRAPPED)
///   → link_inventory → recompute_quantity
///   → 只有 `INSERT INTO biz_inventory … ON CONFLICT … DO UPDATE`（成本账）
///   → `biz_inventory_location` 零处写入
/// ```
///
/// 复跑证据（`7269630` 的 `serial_model.rs`）：
/// `grep -nE "INTO biz_inventory_location|UPDATE biz_inventory_location|DELETE FROM biz_inventory_location"`
/// = **0 条**；7 处 `biz_inventory_location` 全是注释 + 一条 `SELECT`（`:875` 的库位归属校验），
/// 且 `recompute_quantity` 函数体里一次都没提到它。也就是说「报废/出库漏写库位账」是
/// **同族第二处缺口**，修它必须动 `serial_model.rs` —— 那是 `t_421d8aa1` 的范围
/// （该卡在途改动已经把 `apply_location_qty` 接进出库/报废链与批量生成，见
/// `/root/p10-cp-131816` 里 `serial_model.rs` 的 `git diff`），本卡按
/// 「不许为了这条缺口去改 serial_model」的红线不动它。
///
/// 所以本用例用**当前主干上真实存在的、会同时写两本账的两条路径**交替：
///
/// 1. 调拨结算（本卡修的路径：`apply_location_qty` 两笔）；
/// 2. 入库（`stockin_model::apply_cost` + `apply_location_qty`，就是
///    `stockin_handler::post_in_tx` ③→③b 的那一对原语，同一事务）；
/// 3. 反向调拨（`WH_TO → WH_FROM`，业务上一次真实的出库）。
///
/// 报废那一步的两账相符留在 `t_421d8aa1` 落地后由巡检 B1 覆盖（本卡的结算断言
/// 也不受它影响：只校验本次调拨动过的 (物料, 仓库)）。
#[tokio::test]
async fn u3_interleaved_inbound_outbound_keep_two_books_equal() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // ① 正向调拨 5（WH_FROM 100 → 95、WH_TO 10 → 15）
    let out = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(out).await;
    assert_two_books(M_PLAIN, "U3 第一笔结算后").await;
    assert_eq!(location_book(M_PLAIN, WH_FROM).await, dec("95"));
    assert_eq!(location_book(M_PLAIN, WH_TO).await, dec("15"));

    // ② 入库 20 @12.5 到 WH_TO（收货过账 ③→③b 的同款两笔，同一事务）
    //    两本账同时 +20：成本账 15 → 35（金额 15×8 + 250 = 370）、库位账 15 → 35
    let now = Utc::now();
    let tx = db().begin().await.expect("开入库事务");
    stockin_model::apply_cost(
        &tx,
        uuid(M_PLAIN),
        uuid(WH_TO),
        None,
        stockin_model::CostBasis {
            qty: dec("20"),
            amount: dec("250"),
            unit_cost: dec("12.5"),
        },
        now,
    )
    .await
    .expect("入库：成本账加法");
    stockin_model::apply_location_qty(&tx, uuid(M_PLAIN), uuid(WH_TO), None, dec("20"), now)
        .await
        .expect("入库：库位账加法");
    tx.commit().await.expect("提交入库事务");
    assert_two_books(M_PLAIN, "U3 入库后").await;
    assert_eq!(location_book(M_PLAIN, WH_TO).await, dec("35"));

    // ③ 反向调拨 2（出库方向：WH_TO 35 → 33、WH_FROM 95 → 97）
    let back = create_and_ship_with(CreateTransferReq {
        from_warehouse_id: WH_TO.to_string(),
        to_warehouse_id: WH_FROM.to_string(),
        ..body(vec![item(M_PLAIN, "2")])
    })
    .await;
    receive_ok(back).await;
    assert_two_books(M_PLAIN, "U3 反向调拨后").await;
    assert_eq!(location_book(M_PLAIN, WH_FROM).await, dec("97"));
    assert_eq!(location_book(M_PLAIN, WH_TO).await, dec("33"));

    // ④ 再来一笔正向 1（97 → 96 / 33 → 34）：反复交替后两本账仍然相等、总量守恒
    let again = create_and_ship(vec![item(M_PLAIN, "1")]).await;
    receive_ok(again).await;
    assert_two_books(M_PLAIN, "U3 第三笔结算后").await;
    assert_eq!(location_book(M_PLAIN, WH_FROM).await, dec("96"));
    assert_eq!(location_book(M_PLAIN, WH_TO).await, dec("34"));
    assert_eq!(
        location_book(M_PLAIN, WH_FROM).await + location_book(M_PLAIN, WH_TO).await,
        dec("130"),
        "两仓合计守恒（初始 100 + 10 + 入库 20）"
    );
}

/// U4（**反向对照**，证明新自检真的有牙）：把一条库位账行人为改脏 1 件，
/// 下一次同一物料的调拨结算必须在**结算当刻** 409 整单回滚并指名「两账不符」，
/// 而不是等下一次巡检 B1（BLOCKER）变红。
///
/// 这条用例就是卡面第 2 条诉求的证明：漏账（只写成本账、库位账少 1 件）
/// 在结算那一刻被抓住，报错文案直接给出「物料 / 仓 / 成本账 / 库位账 / 差额」。
#[tokio::test]
async fn u4_settle_rejects_dirty_location_book() {
    let Some(_guard) = prepare().await else {
        return;
    };
    // 先正常结算一笔，制造「成本账行 + 库位账行都真实存在」的状态
    let first = create_and_ship(vec![item(M_PLAIN, "5")]).await;
    receive_ok(first).await;
    // 人为把调出仓库位账多记 1（等价于「某条路径只写了成本账」的历史漏账）
    exec(
        "UPDATE biz_inventory_location SET qty = qty + 1, updated_at = now() WHERE id = ?::uuid",
        vec![Value::from(&uuid(LOC_FROM))],
        "污染库位账（+1）",
    )
    .await;
    let second = create_and_ship(vec![item(M_PLAIN, "1")]).await;
    let err = receive(second).await.expect_err("两账不符时结算必须失败");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("调拨结算后不变式校验失败"), "{}", err.msg);
    assert!(err.msg.contains("库位账与成本账不符"), "{}", err.msg);
    // 整单回滚：状态回 IN_TRANSIT，两本账都还是「污染后」原值（没有半截落库）
    assert_eq!(
        refresh(second).await.status,
        STATUS_IN_TRANSIT,
        "整单回滚，状态必须仍是 IN_TRANSIT"
    );
    assert_eq!(
        location_book(M_PLAIN, WH_FROM).await,
        dec("96"),
        "库位账仍是污染后的 96（95 + 1）"
    );
    assert_eq!(
        cost_book(M_PLAIN, WH_FROM).await,
        dec("95"),
        "成本账已回滚，仍是 95"
    );
}

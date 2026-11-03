// ============================================================================
// stockin_handler_tests_dc03.rs —— DC-03 来源单关闭/重复开单 + 过账与作废并发不死锁
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
use crate::business::erp::stockin_model::{self, STATUS_CANCELLED, STATUS_DRAFT, STATUS_POSTED};
use crate::shared::error_code;

/// DC-03 单测 A：采购单在「建单」与「过账」之间被取消 → 过账 **409 + 66031**，
/// 报文点名采购单号与当前状态，且三本账 / 凭证 / 应付 **0 变化**。
#[tokio::test]
async fn dc03_posting_rejected_when_source_order_cancelled() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("100", "0").await;
    let order_no = po_no_of(order_id).await;
    let id = sourced_doc(order_id, item_id, "100").await;
    assert_eq!(
        posting_footprint(id).await,
        vec![0, 0, 0, 0, 0],
        "前提：只建单不动任何账"
    );

    // 采购单被取消（模拟另一个人在采购页点了「取消」，与建单不在同一时刻）
    d4_set_po_status(order_id, "CANCELLED").await;

    let err = change_stock_in_status(id, &post_req(), None)
        .await
        .expect_err("采购单已取消，过账必须被拦");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert_eq!(
        err.code,
        error_code::ERP_SI_PO_NOT_RECEIVABLE,
        "{}",
        err.msg
    );
    assert!(
        err.msg.contains(&order_no),
        "报文必须点名采购单号：{}",
        err.msg
    );
    assert!(
        err.msg.contains("已取消") && err.msg.contains("CANCELLED"),
        "报文必须带当前状态：{}",
        err.msg
    );
    assert!(err.msg.contains("本单未过账"), "{}", err.msg);

    assert_eq!(
        posting_footprint(id).await,
        vec![0, 0, 0, 0, 0],
        "被拒后三本账 / 凭证 / 应付都不许留痕迹"
    );
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
    );
    assert_eq!(stock_in_status(id).await, STATUS_DRAFT, "单据必须留在草稿");
    assert_eq!(
        d4_ledger_received(item_id).await,
        Decimal::ZERO,
        "采购明细的已收数量不许动"
    );
    assert_eq!(
        d4_po_status(order_id).await,
        "CANCELLED",
        "采购单状态不许被悄悄推成 PARTIAL_RECEIVED"
    );
}

/// DC-03 单测 B：两张收货单**双开**（采购 100，两张各收 60）→ 先过账的成功，
/// 后过账的 **409 + 66032**，报文给出「只剩 40 件待收」。
///
/// ⚠️ 卡面写的是「建 A（60）→ 建 B（60，采购只剩 40）→ 过账 B」：A 过账后
/// 采购端只剩 40，B 按 60 建单会被 D4 的建单闸当场 400，**B 根本建不出来**。
/// 真实可达的双开是「先建两张、各自都按未收 100 通过、再先后过账」——本用例就钉这条。
#[tokio::test]
async fn dc03_second_open_receipt_is_rejected_with_available_qty() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("100", "0").await;
    let order_no = po_no_of(order_id).await;
    let a = sourced_doc(order_id, item_id, "60").await;
    let b = sourced_doc(order_id, item_id, "60").await;

    change_stock_in_status(a, &post_req(), None)
        .await
        .unwrap_or_else(|err| panic!("A 过账失败：{} {}", err.code, err.msg));
    assert_eq!(d4_ledger_received(item_id).await, dec("60"));
    let before_b = posting_footprint(b).await;
    assert_eq!(
        before_b,
        vec![1, 1, 0, 0, 0],
        "前提：A 已过账（成本账 / 库位账各 1 行），B 还没有任何痕迹"
    );

    let err = change_stock_in_status(b, &post_req(), None)
        .await
        .expect_err("采购单只剩 40，B 收 60 必须被拦");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SI_OVER_RECEIVE, "{}", err.msg);
    assert!(err.msg.contains(&order_no), "{}", err.msg);
    assert!(
        err.msg.contains("只剩 40 件待收"),
        "报文必须给出可用待收量：{}",
        err.msg
    );
    assert!(err.msg.contains("订单 100"), "{}", err.msg);
    assert!(err.msg.contains("已过账收货 60"), "{}", err.msg);

    assert_eq!(
        posting_footprint(b).await,
        before_b,
        "被拒的 B 不许留下任何账"
    );
    assert_eq!(stock_in_status(b).await, STATUS_DRAFT);
    assert_eq!(
        d4_ledger_received(item_id).await,
        dec("60"),
        "采购已收只算 A 的 60"
    );
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("60"));
}

/// DC-03 单测 C：正常路径不受影响 —— 采购 100 / 收 100/100 → 过账成功，采购单 RECEIVED。
///
/// 同时钉住「两道闸不打架」：收满之后再建一张就会被**建单闸**（400，不可收）挡住，
/// 而不是落到过账闸上。
#[tokio::test]
async fn dc03_full_receipt_still_posts() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let (order_id, item_id) = seed_purchase_order("100", "0").await;
    let id = sourced_doc(order_id, item_id, "100").await;
    change_stock_in_status(id, &post_req(), None)
        .await
        .expect("正常收满 100/100 必须放行");
    assert_eq!(stock_in_status(id).await, STATUS_POSTED);
    assert_eq!(d4_ledger_received(item_id).await, dec("100"));
    assert_eq!(d4_po_status(order_id).await, "RECEIVED");
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("100"));

    // 采购单已 RECEIVED ⇒ 建单闸（400）先拦，不靠过账闸兜
    let mut another = line(MATERIAL_A, "1", "2", "13");
    another.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![another]);
    request.source_order_id = Some(order_id.to_string());
    let err = create_stock_in(&request, None)
        .await
        .expect_err("已收满的采购单不许再建收货单");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("不能收货"), "{}", err.msg);
}

/// **P2 并发用例（40P01）**：A 过账 / B 作废，**同一物料 + 同一仓库 + 同一张采购单**，各 20 轮。
///
/// # 为什么必须「同一张采购单」
///
/// 环由两条路径的**相反加锁顺序**构成：
///
/// ```text
/// 过账：收货单头 → 采购单头(FOR UPDATE) → 库存行
/// 作废：收货单头 → 库存行             → 采购单头(FOR UPDATE)
/// ```
///
/// 两张单若挂在**不同**采购单上，B 等的采购单行 A 并不持有，构不成环（只是普通等待）。
/// 所以本轮让 victim（作废）与 poster（过账）都来自同一张 PO。
///
/// # 判据
///
/// 1. `pg_stat_database.deadlocks` 的**增量必须为 0**（改动前实测 20/20 轮各 1 次，见证据文档）；
/// 2. **每一轮**都断言账实一致：被中止的那一笔必须**整笔回滚**，
///    库存数量 / 采购已收 = 「按单据状态应有的量」，绝不允许半笔账。
///
/// # 时序（用库里的行锁做**确定性**交错，不靠 sleep）
///
/// 自然并发下这个窗口太窄：`tokio::spawn` + 递增延迟跑 20 轮，**一次都没撞上**（实测 0/20）。
/// 所以用例自己占住那把**两个事务都要抢的库存行**当红绿灯，把环的**必要顺序**钉死：
///
/// 1. 测试事务 `inv_hold` 先 `FOR UPDATE` 锁住**库存行**（物料 A × 仓库 × 未分配库位）；
/// 2. 起 B（作废）→ 轮询 `pg_locks` 直到出现**等待中的锁**（别的后端 `granted = false`）
///    ⇒ B 已经走到 `reverse_cost`、卡在库存行上（B 在它前面该拿的锁都拿到了）；
/// 3. 起 A（过账）→ 等等待数变成 **2** ⇒ A 也入场并落到锁上
///    （改前它也排在库存行；改后它排的是 B 持有的采购单行）；
/// 4. 提交 `inv_hold` 松手：改前 B 排在前、先拿到库存行，一路走到 `sync_purchase_status`
///    要采购单行（A 持着），而 A 还在等库存行（B 持着）⇒ 环闭合，PG 1s 后判死锁；
///    改后 B 早在第 3 条 SQL 就锁了采购单头，A 只能排在那把锁上 ⇒ 无环。
///
/// ⚠️ 探针**不能按表名过滤**：等一把被 `UPDATE` 占住的行锁时，PG 记的是
/// `locktype='transactionid'`（`relation` 为空）；而 `FOR UPDATE` 没有争用时又**不会**留
/// tuple 行 ⇒ 只有「谁在等」这个信号可靠，见 [`waiting_locks`]。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lockorder_concurrent_post_and_cancel_no_deadlock() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    const ROUNDS: usize = 20;
    let material = Uuid::parse_str(MATERIAL_A).unwrap();
    let warehouse = Uuid::parse_str(WAREHOUSE).unwrap();
    let before = deadlocks_detected().await;
    let mut both_ok = 0usize;
    let mut post_failed = 0usize;
    let mut cancel_failed = 0usize;
    for round in 0..ROUNDS {
        // 每轮独立单据：采购 200 → 先过账 60（作废对象）+ 再建 40（过账对象，仍是 DRAFT）
        let (order_id, item_id) = seed_purchase_order("200", "0").await;
        let victim = sourced_doc(order_id, item_id, "60").await;
        let poster = sourced_doc(order_id, item_id, "40").await;
        // 库存是**连续余额**（同一物料同一仓库被所有轮次共用）：断言用「本轮增量」
        let qty_before = inventory_of(MATERIAL_A).await.0;
        change_stock_in_status(victim, &post_req(), None)
            .await
            .unwrap_or_else(|err| {
                panic!("第 {round} 轮：victim 过账失败 {} {}", err.code, err.msg)
            });
        // 只发起作废审批（POSTED → PENDING_CANCEL，账一动没动、也不加任何业务锁）
        cancel_request(victim, "并发用例：同物料同仓库并发作废").await;

        // ① 红绿灯：占住**库存行**（过账的 `apply_cost` 与作废的 `reverse_cost` 抢的就是它）
        let inv_hold = db().begin().await.expect("开红绿灯事务");
        let held_rows = inv_hold
            .query_sql(
                "SELECT id FROM biz_inventory \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
                  FOR UPDATE"
                    .to_string(),
                vec![Value::from(&material), Value::from(&warehouse)],
            )
            .await
            .expect("红绿灯：锁库存行失败");
        assert!(
            !held_rows.is_empty(),
            "第 {round} 轮：库存行不存在（victim 刚过账过，必须先有这一行）"
        );

        // ② B（作废）起手。直接调 `cancel_after_approval` —— 就是审批回调
        //    `on_cancel_settled` 内部调的那个函数（`stockin_bridge.rs:178`），一个事务、
        //    一字不差。这里不走 `engine::approve` 的编排：那几笔查询与业务行锁无关。
        let cancel_task = tokio::spawn(async move {
            let row = match stockin_model::find_stock_in(victim).await {
                Ok(Some(row)) => row,
                other => panic!("作废任务查单失败：{other:?}"),
            };
            cancel_after_approval(
                victim,
                &row,
                "并发用例：同物料同仓库并发作废",
                Some(uuid_keeper()),
            )
            .await
        });
        let waited_b = wait_waiters(8_000, 1).await;

        // ③ A（过账）入场；等它也落到锁上（改前：库存行排队；改后：采购单行排队）
        let post_task =
            tokio::spawn(async move { change_stock_in_status(poster, &post_req(), None).await });
        let waited_a = wait_waiters(8_000, 2).await;

        // ④ 松手：改前的环在这里闭合，改后 B 直接跑完
        inv_hold.commit().await.expect("提交红绿灯事务");
        let post_outcome = post_task.await.expect("过账任务 panic");
        let cancel_outcome = cancel_task.await.expect("作废任务 panic");
        if let Err(ref err) = post_outcome {
            println!("第 {round} 轮 过账失败：{} {}", err.status, err.msg);
        }
        if let Err(ref err) = cancel_outcome {
            println!("第 {round} 轮 作废失败：{} {}", err.status, err.msg);
        }
        assert!(
            waited_b.is_some() && waited_a.is_some(),
            "第 {round} 轮：交错没形成（B 卡库存行 {waited_b:?} / A 入场 {waited_a:?}）—— \
             本用例的结论不可信；post_ok={} cancel_ok={}",
            post_outcome.is_ok(),
            cancel_outcome.is_ok()
        );

        let victim_status = stock_in_status(victim).await;
        let poster_status = stock_in_status(poster).await;
        if post_outcome.is_ok() {
            assert_eq!(
                poster_status, STATUS_POSTED,
                "第 {round} 轮：过账返回 Ok 但单据不是 POSTED"
            );
        }
        // 账实一致（本轮增量）：victim 过账 +60、可能作废 −60；poster 过账 +40（若成功）
        let expected_delta = dec("60")
            + dec(if poster_status == STATUS_POSTED {
                "40"
            } else {
                "0"
            })
            - dec(if victim_status == STATUS_CANCELLED {
                "60"
            } else {
                "0"
            });
        assert_eq!(
            inventory_of(MATERIAL_A).await.0 - qty_before,
            expected_delta,
            "第 {round} 轮：库存增量与单据状态对不上（victim={victim_status} / poster={poster_status}）\
             —— 被中止的一笔必须整笔回滚"
        );
        // 采购已收：每轮都是**新采购行**（从 0 起），可以断言绝对值
        let expected = dec(if poster_status == STATUS_POSTED {
            "40"
        } else {
            "0"
        }) + dec(if victim_status == STATUS_CANCELLED {
            "0"
        } else {
            "60"
        });
        assert_eq!(
            d4_ledger_received(item_id).await,
            expected,
            "第 {round} 轮：采购已收数量与单据状态对不上（victim={victim_status} / poster={poster_status}）"
        );

        match (post_outcome.is_ok(), victim_status.as_str()) {
            (true, STATUS_CANCELLED) => both_ok += 1,
            (false, _) => post_failed += 1,
            (true, _) => cancel_failed += 1,
        }
    }
    let deadlocks = deadlocks_detected().await - before;
    println!(
        "lockorder 并发用例：{ROUNDS} 轮（两边都成功 {both_ok} / 过账被中止 {post_failed} / \
         作废被中止 {cancel_failed}），PG 检出死锁 = {deadlocks}"
    );
    assert_eq!(
        deadlocks, 0,
        "过账与作废同时处理同一物料同一仓库时不许出现 40P01（PG 检出 {deadlocks} 次）：\
         两条路径必须按「收货单头 → 采购单头 → 库存行」的同一全序加锁"
    );
}

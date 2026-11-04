// ============================================================================
// stockin_handler_tests_helpers.rs —— 测试辅助：造单 / 造采购单 / 造库位 / 查询小工具
//
// 由 `stockin_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::stockin_handler_tests_fixtures::*;
use super::*;
use crate::business::erp::stockin_bridge;
use crate::business::erp::stockin_dto::{CreateStockInReq, StockInItemReq, StockInStatusReq};
use crate::business::erp::stockin_model::{self, STATUS_CANCELLED};
use crate::business::flow::{engine, model as flow_model};

/// 库存三值。
pub(super) async fn inventory_of(material: &str) -> (Decimal, Decimal, Decimal) {
    let rows = db()
        .find_with_params(
            "SELECT qty, amount, avg_cost FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL",
            vec![
                Value::from(&Uuid::parse_str(material).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("查库存失败：{err}"));
    match rows.first() {
        None => (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
        Some(row) => (
            row.get_typed("qty").unwrap(),
            row.get_typed("amount").unwrap(),
            row.get_typed("avg_cost").unwrap(),
        ),
    }
}

/// 建 + 过账一步到位，返回收货单 id。
pub(super) async fn post_new(lines: Vec<StockInItemReq>) -> Uuid {
    let id = create_stock_in(&req(lines), None).await.unwrap();
    change_stock_in_status(id, &post_req(), None)
        .await
        .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
    id
}

// ==================== DC-03：过账复核来源采购单（ARCH-3 R-P0-3 / §5） ====================
/// 建一张**带来源采购单**的收货单草稿（一行，收 `qty`，单价 2 / 税率 13%）。
pub(super) async fn sourced_doc(order_id: Uuid, item_id: Uuid, qty: &str) -> Uuid {
    let mut only = line(MATERIAL_A, qty, "2", "13");
    only.purchase_order_item_id = Some(item_id.to_string());
    let mut request = req(vec![only]);
    request.source_order_id = Some(order_id.to_string());
    create_stock_in(&request, None)
        .await
        .unwrap_or_else(|err| panic!("建单失败：{} {}", err.code, err.msg))
}

/// 采购单号（报文断言用）。
pub(super) async fn po_no_of(order_id: Uuid) -> String {
    db().scalar_with_params(
        "SELECT order_no FROM biz_purchase_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .unwrap()
}

/// 过账会动的五处痕迹：库存成本账 / 库位账 / 流水 / 凭证 / 应付。
///
/// 顺序固定为 `[inv, loc, txn, voucher, payable]`；被拒的单**每一处都必须不变**。
pub(super) async fn posting_footprint(stock_in_id: Uuid) -> Vec<i64> {
    let material = Uuid::parse_str(MATERIAL_A).unwrap();
    let warehouse = Uuid::parse_str(WAREHOUSE).unwrap();
    let inv: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND qty <> 0",
            vec![Value::from(&material), Value::from(&warehouse)],
        )
        .await
        .unwrap();
    let loc: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND qty <> 0",
            vec![Value::from(&material), Value::from(&warehouse)],
        )
        .await
        .unwrap();
    let txn: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_type = 'STOCK_IN' AND doc_id = ?::uuid",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .unwrap();
    let voucher: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .unwrap();
    let payable: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_payable WHERE source_id = ?::uuid",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .unwrap();
    vec![inv, loc, txn, voucher, payable]
}

// ==================== P2：过账 / 作废 的加锁顺序（40P01） ====================
/// 本库 `pg_stat_database.deadlocks` 累计值 —— **PG 侧权威计数**。
///
/// 不用「谁返回了什么错」来数：作废侧走审批回调（`on_cancel_settled`），
/// 业务失败的出口是 `Ok(None)`（只留 warn 日志），错误在那条路上被**有意吞掉**了；
/// 而 PG 的计数器与「哪一笔被中止」无关，两边都算得到。
pub(super) async fn deadlocks_detected() -> i64 {
    db().scalar_with_params::<Option<i64>>(
        "SELECT deadlocks FROM pg_stat_database WHERE datname = current_database()",
        vec![],
    )
    .await
    .expect("读 pg_stat_database.deadlocks 失败")
    .expect("pg_stat_database.deadlocks 不该是 NULL")
}

/// 一眼看清「有没有人在等锁」：返回**别的后端**当前等待中（`granted = false`）的锁数。
///
/// 为什么只数「在等」：`FOR UPDATE` 在没有争用时**不会**在 `pg_locks` 里留 `locktype='tuple'`
/// 行（实测只有关系级的 `RowShareLock`），所以「谁持有」不可靠；但「谁在等」一定有行。
///
/// 为什么**不按表名过滤**：等一把被 `UPDATE` 占住的行锁时，PG 记的是
/// `locktype='transactionid'`（`relation` 为空），按 `relname` 过滤会漏 —— 第一版就漏了。
pub(super) async fn waiting_locks() -> i64 {
    db().scalar_with_params::<Option<i64>>(
        "SELECT count(*) FROM pg_locks WHERE NOT granted AND pid <> pg_backend_pid()",
        vec![],
    )
    .await
    .expect("查 pg_locks 失败")
    .unwrap_or(0)
}

/// 轮询直到「等待中的锁数 ≥ `want`」，返回耗时；超时返回 `None`。
///
/// 按**墙钟**算超时：一次轮询本身就要几十毫秒（这套池子 `max_lifetime = 0`，每次查询新建
/// 连接），按次数算会把 5s 拖成几分钟。不靠 `sleep` 对齐时序也是同一个原因
/// （实测 `sleep(1ms)` 会被连接握手拖到 37ms 才醒）。
pub(super) async fn wait_waiters(max_ms: u64, want: i64) -> Option<std::time::Duration> {
    let budget = std::time::Duration::from_millis(max_ms);
    let started = std::time::Instant::now();
    while started.elapsed() < budget {
        if waiting_locks().await >= want {
            return Some(started.elapsed());
        }
    }
    None
}

/// 造一张采购单（`qty` 件的 1 行，已收 `received`）并置为 `SUBMITTED`。
///
/// # P2-3：`received > 0` 时补一张**已过账收货单**
///
/// 以前这里直接把 `received` 写进 `biz_purchase_order_item.received_qty` 列；
/// 该列已被迁移 V0217 删除，已收数量只能由「已过账收货单明细」派生。
/// 所以夹具改成**造真账**：`received > 0` 就补一张 `status='POSTED'` 的收货单
/// （[`d4_insert_posted`]，只写单头 + 一行明细 —— 这些用例只验待收口径，
/// 不跑不变量套件）。
pub(super) async fn seed_purchase_order(qty: &str, received: &str) -> (Uuid, Uuid) {
    let order_id = Uuid::now_v7();
    let item_id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_purchase_order \
             (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty, \
              total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?::uuid, 'SUBMITTED', \
             NULLIF(?::text,'')::numeric, 0, 0, 0)",
        vec![
            Value::from(&order_id),
            // 单号尾段必须取 uuid 的**随机段**：`Uuid::now_v7` 字符串前 8 位是毫秒时间戳的
            // 高 32 位，同一个 ~65 秒窗口内所有用例会撞成同一个单号（`uk_biz_po_no` 直接炸）。
            Value::Text(format!("B1-{}", &order_id.simple().to_string()[24..32])),
            Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
            Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .unwrap();
    db().execute_with_params(
        "INSERT INTO biz_purchase_order_item \
             (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
              amount, tax_amount, with_tax_amount) \
         VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, \
             NULLIF(?::text,'')::numeric, 0, 0, NULLIF(?::text,'')::numeric, 0, \
             NULLIF(?::text,'')::numeric)",
        vec![
            Value::from(&item_id),
            Value::from(&order_id),
            Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
            Value::from(&Uuid::parse_str(UNIT).unwrap()),
            Value::Text(qty.to_string()),
            Value::Text("2".to_string()),
            Value::Text(qty.to_string()),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .unwrap();
    // P2-3：已收数量 = 已过账收货单派生，造「已收 N」就必须造一张 POSTED 收货单
    if dec(received) > Decimal::ZERO {
        d4_insert_posted(order_id, item_id, received).await;
    }
    (order_id, item_id)
}

// ==================== P6：库位账（biz_inventory_location）====================
/// 造 / 取一个库位账行（`(物料, 仓库, 库位字典)` 三元组），返回**真实行 id**。
///
/// 不写死主键：`uk_biz_inv_loc(material, warehouse, location_id)` 决定唯一性，
/// 上一轮留下的行会保留原主键（`DO NOTHING` 就取不到 id，所以用「自更新」形态回读）。
pub(super) async fn seed_location_row(material: &str, location_dict: &str) -> Uuid {
    let rows = db()
        .find_with_params(
            &format!(
                "INSERT INTO biz_inventory_location AS il \
                     (id, material_id, warehouse_id, location_id, qty, locked_qty) \
                 VALUES (gen_random_uuid(), '{material}'::uuid, '{WAREHOUSE}'::uuid, \
                         '{location_dict}'::uuid, 0, 0) \
                 ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE \
                     SET qty = il.qty \
                 RETURNING il.id"
            ),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("造库位账行失败：{err}"));
    rows.first()
        .unwrap_or_else(|| panic!("造库位账行没有返回行"))
        .get_typed("id")
        .unwrap_or_else(|err| panic!("取库位账行 id 失败：{err}"))
}

/// 库位账某一行的 `(qty, locked_qty)`（行不存在 → `None`）。
pub(super) async fn location_row_of(id: &Uuid) -> Option<(Decimal, Decimal)> {
    let rows = db()
        .find_with_params(
            "SELECT qty, locked_qty FROM biz_inventory_location WHERE id = ?::uuid",
            vec![Value::from(id)],
        )
        .await
        .unwrap_or_else(|err| panic!("查库位账行失败：{err}"));
    rows.first().map(|row| {
        (
            row.get_typed("qty").unwrap(),
            row.get_typed("locked_qty").unwrap(),
        )
    })
}

/// 库位账某一行的数量（行不存在 → `None`）。
pub(super) async fn location_qty_of(id: &Uuid) -> Option<Decimal> {
    location_row_of(id).await.map(|(qty, _)| qty)
}

/// 库位账「未分配库位桶」（`location_id IS NULL`）的数量（行不存在 → `None`）。
pub(super) async fn null_bucket_qty(material: &str) -> Option<Decimal> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT qty FROM biz_inventory_location \
                  WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid \
                    AND location_id IS NULL"
            ),
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("查 NULL 桶失败：{err}"));
    rows.first().map(|row| row.get_typed("qty").unwrap())
}

/// 库位账：某物料在该仓的**行数**。
pub(super) async fn location_row_count(material: &str) -> i64 {
    db().scalar_with_params(
        &format!(
            "SELECT COALESCE(COUNT(*), 0) FROM biz_inventory_location \
              WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
        ),
        vec![],
    )
    .await
    .unwrap_or_else(|err| panic!("统计库位账行数失败：{err}"))
}

/// 库位账：某物料在该仓的 qty **合计**（`SUM`，含 NULL 桶）。
pub(super) async fn location_qty_sum(material: &str) -> Decimal {
    db().scalar_with_params(
        &format!(
            "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location \
              WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
        ),
        vec![],
    )
    .await
    .unwrap_or_else(|err| panic!("汇总库位账失败：{err}"))
}

/// 成本账：某物料在该仓的 qty **合计**（`SUM`，含 NULL 汇总桶）。
///
/// V0139 之后 `biz_inventory` 也是 `(物料, 仓库, 库位)` 三列唯一，所以「该物料该仓的成本账数量」
/// 在**同一口径**下就是按库位求和 —— 与库位账的 `SUM(qty)` 可比。
pub(super) async fn cost_qty_sum(material: &str) -> Decimal {
    db().scalar_with_params(
        &format!(
            "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory \
              WHERE material_id = '{material}'::uuid AND warehouse_id = '{WAREHOUSE}'::uuid"
        ),
        vec![],
    )
    .await
    .unwrap_or_else(|err| panic!("汇总成本账失败：{err}"))
}

// ==================== P9：作废回滚库存（卡面 §五，12 条） ====================
//
// ⚠️ P16 起「作废」是**两步**：`POSTED → PENDING_CANCEL`（发起审批）→ 审批通过 →
// `CANCELLED`（执行六本账回滚）。P9 的这 12 条断言（库存 / 库位账 / 流水 / 凭证 /
// 序列号全部回滚）**一条都没改**，只是把它们共同的前置「作废」从句柄换成了
// [`cancel`]（提交 + 复核通过 + 回调），走的是与生产完全相同的路径。
// 这样新增的「复核」既被覆盖，P9 的回归也仍然钉得住。
/// 作废请求体。
pub(super) fn cancel_req(reason: Option<&str>) -> StockInStatusReq {
    StockInStatusReq {
        status: Some("CANCELLED".to_string()),
        reason: reason.map(str::to_string),
        serials: Vec::new(),
        ..Default::default()
    }
}

/// 作废并断言成功 = **发起作废审批 + 复核通过 + 六本账回滚**（P16 生产路径）。
pub(super) async fn cancel(id: Uuid, reason: &str) {
    cancel_request(id, reason).await;
    approve_cancel(id).await;
    assert_eq!(
        detail_of(id).await.unwrap().header.status,
        STATUS_CANCELLED,
        "复核通过后单据必须真的作废"
    );
}

/// 只**发起**作废审批（不进审批），返回审批单落库后的库存三值快照。
pub(super) async fn cancel_request(id: Uuid, reason: &str) {
    change_stock_in_status(id, &cancel_req(Some(reason)), Some(uuid_keeper()))
        .await
        .unwrap_or_else(|err| panic!("发起作废审批失败：{} {}", err.code, err.msg));
}

/// 取某单作废审批实例 id（没有 → panic）。
pub(super) async fn cancel_instance_of(stock_in_id: Uuid) -> Uuid {
    let instance_id: Option<Uuid> = db()
        .scalar_with_params(
            "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .expect("查 approval_instance_id");
    instance_id.expect("作废审批中必须有实例 id（ck_biz_si_pending_cancel）")
}

/// 取某单审批单里第一条 `PENDING` 待办（含审批人）。
pub(super) async fn pending_task_of(stock_in_id: Uuid) -> (Uuid, Uuid) {
    let instance_id = cancel_instance_of(stock_in_id).await;
    let rows = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' \
              ORDER BY created_at ASC",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("查作废审批待办");
    let row = rows.first().expect("作废审批必须有 PENDING 待办");
    (
        row.get_typed("id").expect("待办 id"),
        row.get_typed("assignee_id").expect("审批人"),
    )
}

/// 复核**通过**：引擎投票 → 提交事务 → 回调收货单（生产同款顺序）。
///
/// ⚠️ `stock_in_cancel` 的审批人来源是 `ADMIN`（运行时展开**全部超管**），
/// `join_mode=AND` ⇒ **会签**。库里超管有几个，节点就有几条待办；只批第一条会让实例
/// 停在 `PENDING`，业务桥 `on_cancel_settled` 根本不会被调到（实测单据卡在
/// `PENDING_CANCEL`、`settle_guard` 留痕 0 条）。所以这里必须**批到终态**，
/// 并断言实例真的 `APPROVED` —— 没到终态就是夹具的错，不能让它悄悄绿着过去。
pub(super) async fn approve_cancel(stock_in_id: Uuid) {
    let instance_id = cancel_instance_of(stock_in_id).await;
    let outs =
        crate::business::flow::test_support::approve_all_pending(instance_id, "单测：同意作废")
            .await;
    assert!(
        !outs.is_empty(),
        "作废审批必须至少有一条 PENDING 待办可批（实例 {instance_id}）"
    );
    for out in outs {
        // 与 `flow::handler::approve_task` 同序：事务已提交，再跑业务回调
        stockin_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;
    }
    crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;
}

/// 复核**驳回**（并终止）：实例落 `REJECTED` → 回执打回已过账。
///
/// 驳回是「一票否决」（`reject_veto = true`），所以只驳**一条**待办实例就终止；
/// 仍然断言实例确实 `REJECTED` 再回调 —— 否则回调不该被触发。
pub(super) async fn reject_cancel(stock_in_id: Uuid, comment: &str) {
    let (task_id, approver) = pending_task_of(stock_in_id).await;
    let tx = db().begin().await.expect("开审批事务");
    let out = engine::reject(&tx, task_id, approver, comment, engine::ReturnTo::Final)
        .await
        .expect("引擎驳回失败");
    tx.commit().await.expect("提交审批事务");
    let instance_id = out.instance_id.expect("驳回出参必须带实例 id");
    crate::business::flow::test_support::assert_instance_status(instance_id, "REJECTED").await;
    stockin_bridge::on_cancel_settled(out.instance_id, &out.instance_status).await;
}

/// 某单的实例状态（没有实例 → `None`）。
pub(super) async fn instance_status_of(stock_in_id: Uuid) -> Option<String> {
    let instance_id: Option<Uuid> = db()
        .scalar_with_params(
            "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&stock_in_id)],
        )
        .await
        .expect("查 approval_instance_id");
    let instance_id = instance_id?;
    flow_model::find_instance(instance_id)
        .await
        .expect("查审批实例")
        .map(|row| row.status)
}

/// 某张单某类流水的条数。
pub(super) async fn txn_count(doc: Uuid, biz_type: &str) -> i64 {
    db().scalar_with_params(
        "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid AND biz_type = ?",
        vec![Value::from(&doc), Value::from(biz_type)],
    )
    .await
    .unwrap_or_else(|err| panic!("查流水失败：{err}"))
}

/// 某张单的凭证条数（`is_red` 为空 = 全部）。
pub(super) async fn voucher_count(doc: Uuid, red: Option<i16>) -> i64 {
    let sql = if red.is_some() {
        "SELECT COUNT(*) FROM biz_voucher WHERE source_id = ?::uuid AND is_red = 1"
    } else {
        "SELECT COUNT(*) FROM biz_voucher WHERE source_id = ?::uuid"
    };
    db().scalar_with_params(sql, vec![Value::from(&doc)])
        .await
        .unwrap_or_else(|err| panic!("查凭证失败：{err}"))
}

// ==================== P10：作废退序列号（卡面 §一） ====================
/// 某张单生成的序列号：`(存活条数, 已软删条数)`。
pub(super) async fn serial_counts_of(doc: Uuid) -> (i64, i64) {
    let mut out = [0i64; 2];
    for (slot, cond) in out
        .iter_mut()
        .zip(["deleted_at IS NULL", "deleted_at IS NOT NULL"])
    {
        let sql = format!(
            "SELECT count(*) FROM biz_material_serial \
              WHERE source_id = ?::uuid AND source_type = 'PURCHASE' AND {cond}"
        );
        *slot = db()
            .scalar_with_params(&sql, vec![Value::from(&doc)])
            .await
            .unwrap_or_else(|err| panic!("查序列号失败：{err}"));
    }
    (out[0], out[1])
}

// ==================== P16：收货单作废审批（卡面 §二，14 条 + 内控 2 条） ====================
//
// 全部走**生产同款路径**：`change_stock_in_status`（发起）→ `engine::approve/reject/withdraw`
// （复核）→ `stockin_bridge::on_cancel_settled`（回调）。夹具里的「发起人」是仓管员
// （非超管），审批人是库里唯一的 SUPER_ADMIN —— 这正是 `self_approval=FORBID`
// （内控：审批人 ≠ 发起人）能在真实库里跑起来的前提。
/// 建 + 过账（`create_by` = 操作人，供内控判据用）。
#[allow(dead_code)]
pub(super) async fn post_new_by(lines: Vec<StockInItemReq>, operator: Option<Uuid>) -> Uuid {
    let id = create_stock_in(&req(lines), operator).await.unwrap();
    change_stock_in_status(id, &post_req(), operator)
        .await
        .unwrap_or_else(|err| panic!("过账失败：{} {}", err.code, err.msg));
    id
}

// ==================== D4：采购订单 → 一键生成收货单 ====================
/// 造一张采购单（1 行，物料 A），返回 `(order_id, item_id)`。
///
/// `received` > 0 时由 [`seed_purchase_order`] 造一张真实已过账收货单
/// （P2-3：已收数量只有一个来源，没有「写列」这条路可走了）。
pub(super) async fn d4_seed_po(qty: &str, received: &str) -> (Uuid, Uuid) {
    seed_purchase_order(qty, received).await
}

/// 插一张「已过账」的收货单（只写单头 + 一行明细，用于喂 pending 聚合）。
///
/// 不生成凭证 / 库存流水 —— 这些用例只验「待收数量」口径，**不能**跑不变量套件
/// （D1 要求已过账单必有凭证），所以单测库与 e2e 库分开。
pub(super) async fn d4_insert_posted(order_id: Uuid, item_id: Uuid, qty: &str) {
    let id = Uuid::now_v7();
    // 同上：单号取 uuid 随机段，别用时间戳段（同窗口多张单会撞 `uk_biz_si_no`）
    let in_no = format!("B1-IN-{}", &id.simple().to_string()[24..32]);
    db().execute_with_params(
        "INSERT INTO biz_stock_in \
             (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, total_qty) \
         VALUES (?::uuid, ?, 'PURCHASE', ?::uuid, ?::uuid, ?::uuid, CURRENT_DATE, 'POSTED', \
             NULLIF(?::text,'')::numeric)",
        vec![
            Value::from(&id),
            Value::Text(in_no),
            Value::from(&order_id),
            Value::from(&Uuid::parse_str(SUPPLIER).unwrap()),
            Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .unwrap();
    db().execute_with_params(
        "INSERT INTO biz_stock_in_item \
             (id, stock_in_id, line_no, source_item_id, material_id, unit_id, qty) \
         VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric)",
        vec![
            Value::from(&Uuid::now_v7()),
            Value::from(&id),
            Value::from(&item_id),
            Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
            Value::from(&Uuid::parse_str(UNIT).unwrap()),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .unwrap();
}

/// 造采购单 + 真账：`received > 0` 时补一张 POSTED 收货单。
///
/// P2-3 起这一步已收进 [`seed_purchase_order`]（列删除后「造已收」只有这一条路），
/// 本函数保留为语义化别名，调用方不必改。
pub(super) async fn d4_seed_po_with_posted(qty: &str, received: &str) -> (Uuid, Uuid) {
    d4_seed_po(qty, received).await
}

/// 改采购单状态（造 DRAFT / RECEIVED / CANCELLED 这些非默认态）。
pub(super) async fn d4_set_po_status(order_id: Uuid, status: &str) {
    db().execute_with_params(
        "UPDATE biz_purchase_order SET status = ? WHERE id = ?::uuid",
        vec![Value::Text(status.to_string()), Value::from(&order_id)],
    )
    .await
    .unwrap();
}

/// 读采购单状态。
pub(super) async fn d4_po_status(order_id: Uuid) -> String {
    db().scalar_with_params(
        "SELECT status FROM biz_purchase_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .unwrap()
}

/// 读采购明细的「已收数量」——**派生值**（P2-3）。
///
/// 以前直接读 `biz_purchase_order_item.received_qty` 列；列已删除（迁移 V0217），
/// 这里改成与生产**同一个**聚合表达式（`POSTED_RECEIVED_SQL`），
/// 断言才有意义（读自己的实现会把 bug 一起读回来，所以表达式直接从生产常量取）。
pub(super) async fn d4_ledger_received(item_id: Uuid) -> Decimal {
    let sql = format!(
        "SELECT ({}::numeric(18,4)) AS received_qty FROM biz_purchase_order_item i WHERE i.id = ?::uuid",
        crate::business::erp::purchase_model::POSTED_RECEIVED_SQL
    );
    db().scalar_with_params(&sql, vec![Value::from(&item_id)])
        .await
        .unwrap()
}

/// 过账审批夹具：仓管主管账号 + 角色绑定 + 清掉本卡夹具的审批实例 / 通知。
pub(super) async fn setup_post_approval() {
    // 仓管主管角色是共享资源：只留本卡夹具的人（否则「派给谁」不可判定）
    sql(&format!(
        "DELETE FROM sys_user_role WHERE role_id = '{ROLE_WH_MGR}'::uuid \
            AND user_id <> '{U_WH_MGR}'::uuid"
    ))
    .await;
    sql(&format!(
        "INSERT INTO sys_user (id, username, password, real_name, status) \
         VALUES ('{U_WH_MGR}'::uuid, 'p3a_wh_mgr', 'x', 'p3a 仓管主管', 1) \
         ON CONFLICT (id) DO NOTHING"
    ))
    .await;
    sql(&format!(
        "INSERT INTO sys_user_role (user_id, role_id) \
         VALUES ('{U_WH_MGR}'::uuid, '{ROLE_WH_MGR}'::uuid) ON CONFLICT DO NOTHING"
    ))
    .await;
    for sql_text in [
        // 通知接收人是硬外键 → 必须先删接收人再删通知（顺序反了会 FK 违约）。
        // ⚠️ `sys_notice.biz_id` 是 **varchar**（V0043），必须 `id::text` 才能和它比。
        format!(
            "DELETE FROM sys_notice_receiver WHERE notice_id IN \
               (SELECT id FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
                 (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
                    AND biz_id IN \
                   (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))))"
        ),
        format!(
            "DELETE FROM sys_notice WHERE biz_type = 'FLOW_NOTIFY' AND biz_id IN \
               (SELECT id::text FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
                  AND biz_id IN \
                 (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid)))"
        ),
        format!(
            "DELETE FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' AND biz_id IN \
               (SELECT id FROM biz_stock_in WHERE supplier_id IN ('{SUPPLIER}'::uuid, '{A4_SUPPLIER_B}'::uuid))"
        ),
    ] {
        sql(&sql_text).await;
    }
}

/// 造一张含税金额 = `amount` 的收货单（单行、税率 0）。
pub(super) async fn stock_in_with_amount(amount: &str, maker: Option<&str>) -> Uuid {
    create_stock_in(
        &req(vec![line(MATERIAL_A, "1", amount, "0")]),
        maker.map(uid_of),
    )
    .await
    .unwrap_or_else(|err| panic!("造收货单失败：{} {}", err.code, err.msg))
}

pub(super) async fn running_post_instance(stock_in_id: Uuid) -> Uuid {
    db().scalar_with_params::<Option<Uuid>>(
        "SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
          AND biz_id = ?::uuid AND status = 'PENDING' LIMIT 1",
        vec![Value::from(&stock_in_id)],
    )
    .await
    .unwrap()
    .expect("应当有在跑的过账审批实例")
}

pub(super) async fn post_pending_tasks(instance_id: Uuid) -> Vec<(Uuid, Uuid)> {
    let rows = db()
        .find_with_params(
            "SELECT id, assignee_id FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY assignee_id, id",
            vec![Value::from(&instance_id)],
        )
        .await
        .expect("读待办失败");
    rows.iter()
        .map(|row| {
            (
                row.get_typed("id").unwrap(),
                row.get_typed("assignee_id").unwrap(),
            )
        })
        .collect()
}

/// 某人在本实例上的待办（**不限状态**）：`(task_id, status, comment)`。
///
/// P0-FIX 之后「制单人 / 发起人」不再拿 `PENDING`（生成时就被排除成 `SKIPPED`）。
pub(super) async fn post_task_of(
    instance_id: Uuid,
    assignee: Uuid,
) -> Option<(Uuid, String, String)> {
    let rows = db()
        .find_with_params(
            "SELECT id, status, COALESCE(comment,'') AS comment FROM biz_flow_task \
              WHERE instance_id = ?::uuid AND assignee_id = ?::uuid ORDER BY created_at LIMIT 1",
            vec![Value::from(&instance_id), Value::from(&assignee)],
        )
        .await
        .expect("读待办失败");
    rows.first().map(|row| {
        (
            row.get_typed("id").unwrap(),
            row.get_typed("status").unwrap(),
            row.get_typed("comment").unwrap(),
        )
    })
}

/// 以某人身份同意一条待办，并按 `flow::handler` 的口径推进业务回调。
pub(super) async fn post_approve_as(
    task_id: Uuid,
    operator: &str,
) -> crate::business::flow::engine::ActionOut {
    let op = uid_of(operator);
    let out = db()
        .transaction(|tx| async move {
            crate::business::flow::engine::approve(&tx, task_id, op, Some("同意")).await
        })
        .await
        .unwrap_or_else(|err| panic!("同意失败：{err}"));
    stockin_bridge::on_post_settled(out.instance_id, &out.instance_status).await;
    out
}

pub(super) async fn stock_in_status(id: Uuid) -> String {
    stockin_model::find_stock_in(id)
        .await
        .unwrap()
        .expect("收货单不存在")
        .status
}

// ==================== D9：复制为新单（收货单） ====================
//
// 每个用例都断言「原单一个字节都不改」的至少一个切面；`mutation` 这条用例做整行对照。
/// 复制用的假令牌：`permissions` 决定 `has_permission`（与框架判定同口径）。
pub(super) fn copy_claims(perms: &[&str]) -> TokenClaims {
    TokenClaims {
        jti: None,
        sub: "018f4b2c-d9d9-7d9d-8d9d-000000000001".to_string(),
        username: Some("d9-copier".to_string()),
        roles: Vec::new(),
        permissions: perms.iter().map(|p| (*p).to_string()).collect(),
        token_type: None,
        exp: 0,
        iat: 0,
        extra: std::collections::HashMap::new(),
    }
}

/// 只给「建单」一个码的令牌：证明「复制权限 = create 权限」在业务分支里也成立。
pub(super) fn create_only() -> TokenClaims {
    copy_claims(&[COPY_PERMISSION])
}

/// 原单整行快照（用于「复制不改原单」的整行对照）。
///
/// 用 `to_jsonb(t)` 把整行拍成文本：比逐列断言更狠 —— 以后加了新列也自动纳入对照。
/// 取出来的是**同一行前后两次**的文本，键序恒定，所以可以逐字比。
pub(super) async fn header_snapshot(id: Uuid) -> String {
    db().scalar_with_params(
        "SELECT (to_jsonb(t))::text FROM biz_stock_in t WHERE t.id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .expect("查原单快照失败")
}

// ==================== A4：入库单改绑采购单（供应商从采购单派生 + 无源标记） ====================
//
// 卡面 t_a8aa8c71 验收 A 的 11 条断言在这里逐条落地。全部走**生产同一条代码路径**
// （`create_stock_in` / `update_stock_in` / `change_stock_in_status` / `detail_of` /
// `stock_in_page`），只跳过认证 —— 单测能调的路径就是 HTTP 走的那条。
//
// 夹具设计的关键：A4 用**第二个供应商** [`A4_SUPPLIER_B`] 建采购单，
// 而请求里不带 `supplier_id`。如果供应商不是真从采购单派生，落库值会是空的或另一个值，
// 断言立刻红 —— 用同一个供应商就测不出「派生」。
/// 造一张**指定供应商 / 指定状态**的采购单（1 行物料 A），返回 `(order_id, item_id)`。
pub(super) async fn a4_seed_po(supplier: &str, qty: &str, status: &str) -> (Uuid, Uuid) {
    let order_id = Uuid::now_v7();
    let item_id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_purchase_order \
             (id, order_no, supplier_id, order_date, warehouse_id, status, total_qty, \
              total_amount, total_tax, total_with_tax) \
         VALUES (?::uuid, ?, ?::uuid, CURRENT_DATE, ?::uuid, ?, \
             NULLIF(?::text,'')::numeric, 0, 0, 0)",
        vec![
            Value::from(&order_id),
            // 单号取 uuid 的随机段（同毫秒多张单不会撞 uk_biz_po_no）
            Value::Text(format!("B1-A4-{}", &order_id.simple().to_string()[24..32])),
            Value::from(&Uuid::parse_str(supplier).unwrap()),
            Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
            Value::Text(status.to_string()),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .expect("A4 夹具：采购单");
    db().execute_with_params(
        "INSERT INTO biz_purchase_order_item \
             (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
              amount, tax_amount, with_tax_amount) \
         VALUES (?::uuid, ?::uuid, 1, ?::uuid, ?::uuid, NULLIF(?::text,'')::numeric, \
             2, 2, 0, NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric)",
        vec![
            Value::from(&item_id),
            Value::from(&order_id),
            Value::from(&Uuid::parse_str(MATERIAL_A).unwrap()),
            Value::from(&Uuid::parse_str(UNIT).unwrap()),
            Value::Text(qty.to_string()),
            Value::Text(qty.to_string()),
            Value::Text(qty.to_string()),
        ],
    )
    .await
    .expect("A4 夹具：采购明细");
    (order_id, item_id)
}

/// 从采购单生成收货单的请求（`supplier` = `None` 时证明服务端派生）。
pub(super) fn a4_sourced_req(
    order_id: Uuid,
    item_id: Uuid,
    qty: &str,
    supplier: Option<&str>,
) -> CreateStockInReq {
    let mut one = line(MATERIAL_A, qty, "2", "0");
    one.purchase_order_item_id = Some(item_id.to_string());
    CreateStockInReq {
        source_order_id: Some(order_id.to_string()),
        supplier_id: supplier.map(str::to_string),
        warehouse_id: Some(WAREHOUSE.to_string()),
        items: vec![one],
        ..Default::default()
    }
}

/// 采购单当前状态（A4 用例读回写结果用）。
pub(super) async fn a4_po_status(order_id: Uuid) -> String {
    db().scalar_with_params(
        "SELECT status::text FROM biz_purchase_order WHERE id = ?::uuid",
        vec![Value::from(&order_id)],
    )
    .await
    .expect("查采购单状态")
}

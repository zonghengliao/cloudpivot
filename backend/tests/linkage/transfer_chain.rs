//! 领域 3：调拨全链路（12 个用例，F01–F12）。
//!
//! # 这条链路为什么危险
//!
//! 调拨单是**唯一同时挂在「库存」和「审批」两条链上**的单据：
//!
//! ```text
//! DRAFT ──提交──▶ PENDING ──审批通过──▶ APPROVED ──发货──▶ IN_TRANSIT ──收货──▶ DONE
//!   │               │                                  │
//!   └──取消─────────┴──撤回（必须同时终止在跑审批单）    └─ 结算：调出仓 -qty / 调入仓 +qty（同一事务）
//! ```
//!
//! 单模块用例各自都绿：`transfer_handler` 测 6 条边，`flow` 测审批引擎。但
//! 「撤回后审批单没终止 → 重新提交撞 `uk_biz_flow_instance_biz_active` 永久 409」、
//! 「结算失败但状态已经 DONE」这类**跨链路**问题只有把两条链一起跑才看得见。
//!
//! # 与卡面期望的一处口径差异（已实测，写在这里免得下一张卡重新猜）
//!
//! 卡面 F03 写「APPROVED → IN_TRANSIT 调出仓扣减 + 在途仓增加」。**实现不是这样**：
//! 库存只在 `IN_TRANSIT → DONE` 的结算里双写（[`transfer_model::settle_transfer`]），
//! 发货这一步只回填 `transit_warehouse_id`（在途仓**不收库存**，没有实物在途台账）。
//! 因此 F03 断言的是「记账口径」而不是卡面字面，并在该用例里标注为**设计差异**（非缺陷）；
//! F04/F05 才是「数量真的动了」的地方。

use nik::prelude::*;
use nik::uuid::Uuid;

use cloudpivot::business::erp::transfer_dto::{CreateTransferReq, TransferItemReq, TransferRes, TransferStatusReq};
use cloudpivot::business::erp::transfer_handler::{change_transfer_status, create_transfer};
use cloudpivot::business::erp::transfer_model::{self, TransferQuery};
use cloudpivot::business::erp::transfer_overdue;
use cloudpivot::business::flow::dto as flow_dto;
use cloudpivot::business::flow::handler as flow_handler;

use crate::linkage::common::{self, Ctx};

// ==================== 夹具动作 ====================

/// 给某物料在某仓建一行整仓汇总库存（`location_id IS NULL` 的汇总桶）。
///
/// 说明：调拨结算只认汇总桶（有汇总桶就只动它，否则按明细库位精确到库位），
/// 所以夹具只需要建这一行；没有它，`DONE` 结算会 409「调出仓库存不足」。
async fn seed_stock(ctx: &Ctx, material: Uuid, warehouse: Uuid, qty: &str, cost: &str) {
    let qty = common::dec(qty);
    let cost = common::dec(cost);
    let amount = qty * cost;
    // 数值直接写字面量：`Decimal` 的 `Display` 输出就是合法 numeric 字面量，
    // 省掉「文本参数 → numeric」这一层（实测该驱动在这条 INSERT 上报
    // `error serializing parameter`，而 uuid 参数反而正常）。
    db().execute_with_params(
        &format!(
            "INSERT INTO biz_inventory \
                 (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at) \
             VALUES (gen_random_uuid(), '{}'::uuid, '{}'::uuid, NULL, {qty}, 0, {cost}, {amount}, now())",
            material, warehouse
        ),
        vec![],
    )
    .await
    .unwrap_or_else(|error| panic!("{}：建库存基线失败：{error}", ctx.tag));
    // **库位账也要同数**：结算（`transfer_model::settle_transfer`）在写完之后自检
    // 「库位账合计 == 成本账 qty」（巡检 B1，BLOCKER），只种成本账的夹具会让
    // `IN_TRANSIT → DONE` 直接 409 + 66001「库位账与成本账不符」（2026-11 合并实测 F11）。
    // 汇总桶 = `location_id IS NULL`（与成本账那一行同口径），幂等靠 `uk_biz_inv_loc`。
    db().execute_with_params(
        &format!(
            "INSERT INTO biz_inventory_location \
                 (id, material_id, warehouse_id, location_id, qty, locked_qty, last_txn_at) \
             VALUES (gen_random_uuid(), '{}'::uuid, '{}'::uuid, NULL, {qty}, 0, now()) \
             ON CONFLICT (material_id, warehouse_id, location_id) \
             DO UPDATE SET qty = EXCLUDED.qty, updated_at = now()",
            material, warehouse
        ),
        vec![],
    )
    .await
    .unwrap_or_else(|error| panic!("{}：建库位账基线失败：{error}", ctx.tag));
}

/// 建一个在库序列号（`IN_STOCK`，挂在调出仓），返回它的 id。
async fn seed_serial(ctx: &Ctx, serial_no: &str) -> Uuid {
    let id = Uuid::now_v7();
    db().execute_with_params(
        "INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status) \
         VALUES (?::uuid, ?, ?::uuid, ?::uuid, 'IN_STOCK')",
        vec![
            Value::from(&id),
            Value::Text(serial_no.to_string()),
            Value::from(&ctx.mat_sn),
            Value::from(&ctx.wh_from),
        ],
    )
    .await
    .unwrap_or_else(|error| panic!("{}：建序列号失败：{error}", ctx.tag));
    id
}

/// 建单请求（调出仓 = `wh_from`，调入仓 = `wh_to`）。
fn req(ctx: &Ctx, items: Vec<TransferItemReq>) -> CreateTransferReq {
    CreateTransferReq {
        from_warehouse_id: ctx.wh_from.to_string(),
        to_warehouse_id: ctx.wh_to.to_string(),
        transfer_date: None,
        reason: Some(format!("{} 联动矩阵", ctx.tag)),
        remark: None,
        items,
    }
}

fn item(material: Uuid, qty: &str) -> TransferItemReq {
    TransferItemReq {
        material_id: material.to_string(),
        from_location_id: None,
        to_location_id: None,
        qty: common::dec(qty),
        serial_ids: Vec::new(),
    }
}

fn status_req(to: &str, reason: Option<&str>) -> TransferStatusReq {
    TransferStatusReq {
        status: to.to_string(),
        reason: reason.map(str::to_string),
    }
}

/// 建单（`DRAFT`），返回调拨单 id。
async fn draft(ctx: &Ctx, items: Vec<TransferItemReq>) -> Uuid {
    create_transfer(&req(ctx, items), Some(ctx.worker), Some("worker"))
        .await
        .unwrap_or_else(|error| panic!("{}：建调拨单失败 {} {}", ctx.tag, error.code, error.msg))
}

/// 推一次状态（失败直接 panic，返回 `()`）。
async fn step(ctx: &Ctx, id: Uuid, to: &str) {
    change_transfer_status(id, &status_req(to, None), ctx.worker)
        .await
        .unwrap_or_else(|error| panic!("{}：推进到 {to} 失败 {} {}", ctx.tag, error.code, error.msg));
}

/// 调拨单号。
///
/// 保留：F 系列后续「按单号查审批实例」的用例会用到（当前断言都用 id/状态），
/// 显式豁免 dead_code 而不是删掉。
#[allow(dead_code)]
async fn transfer_no_of(id: Uuid) -> String {
    common::scalar_text(
        "SELECT transfer_no AS v FROM biz_stock_transfer WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .unwrap_or_else(|| panic!("调拨单 {id} 不存在"))
}

/// 单据状态。
async fn status_of(id: Uuid) -> String {
    common::scalar_text(
        "SELECT status AS v FROM biz_stock_transfer WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .unwrap_or_else(|| panic!("调拨单 {id} 不存在"))
}

/// 单据上的审批实例 id。
async fn instance_id_of(id: Uuid) -> Option<Uuid> {
    let raw = common::scalar_text(
        "SELECT approval_instance_id::text AS v FROM biz_stock_transfer WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await?;
    Some(Uuid::parse_str(&raw).expect("审批实例 id"))
}

/// 某物料在某仓的汇总桶数量（行不存在 → 0）。
async fn bucket_qty(material: Uuid, warehouse: Uuid) -> rust_decimal::Decimal {
    common::scalar_dec(
        "SELECT COALESCE(sum(qty),0)::numeric(18,4) AS v FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material), Value::from(&warehouse)],
    )
    .await
}

/// 整个仓里该物料的库存行数（用于断言「在途仓不收库存」）。
async fn inventory_rows(material: Uuid, warehouse: Uuid) -> i64 {
    common::scalar_i64(
        "SELECT count(*) AS v FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id = ?::uuid",
        vec![Value::from(&material), Value::from(&warehouse)],
    )
    .await
}

/// 本单的库存流水条数。
async fn txn_count(id: Uuid) -> i64 {
    common::scalar_i64(
        "SELECT count(*) AS v FROM biz_inventory_txn \
          WHERE doc_type = 'STOCK_TRANSFER' AND doc_id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
}

/// 实例状态（不存在 → `None`）。
async fn instance_status(instance_id: Option<Uuid>) -> Option<String> {
    let id = instance_id?;
    common::scalar_text(
        "SELECT status AS v FROM biz_flow_instance WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
}

/// 走**生产路径**让审批单落终态：把所有 PENDING 待办逐条「同意」，直到实例不再 PENDING。
///
/// # 为什么要遍历而不是投一票
///
/// 调拨流程（V0157）的审批节点是 `ADMIN / FALLBACK_ADMIN`，受派人不是夹具用户，
/// 而是 `biz_flow_instance` 发起时按角色解析出来的**全部** `SUPER_ADMIN` 用户。
/// `cloudpivot_test` 是并发共享库：别的卡片（如批量用例生成器）会往 `sys_user` 里
/// 造几十个 SUPER_ADMIN 账号 —— 于是同一节点上会生成几十条待办，投一票不会落终态
/// （实测：只投一票时调拨单停在 PENDING，下游 F02/F03/F04 全红）。
/// 所以这里循环投到没有 PENDING 待办为止，每票都用**该待办真正的受派人**身份。
async fn approve_instance(instance_id: Uuid) {
    for _ in 0..64 {
        let rows = db()
            .find_with_params(
                "SELECT id, assignee_id FROM biz_flow_task \
                  WHERE instance_id = ?::uuid AND status = 'PENDING' ORDER BY created_at",
                vec![Value::from(&instance_id)],
            )
            .await
            .expect("查待办");
        let Some(row) = rows.first() else {
            return;
        };
        let task_id: Uuid = row.get_typed("id").expect("待办 id");
        let assignee: Uuid = row.get_typed("assignee_id").expect("受派人");
        let body = flow_dto::ApproveReq {
            comment: Some("同意（调拨联动测试）".to_string()),
        };
        flow_handler::approve_task(
            common::claims(assignee, "transfer-approver"),
            axum::extract::Path(task_id.to_string()),
            Some(axum::Json(body)),
        )
        .await
        .unwrap_or_else(|error| panic!("审批同意失败 {} {}", error.code, error.msg));
    }
    panic!("实例 {instance_id} 的待办超过 64 条仍没落终态（疑似死循环）");
}

/// 建单 → PENDING →（真审批）→ APPROVED → IN_TRANSIT，返回调拨单 id。
async fn in_transit(ctx: &Ctx, items: Vec<TransferItemReq>) -> Uuid {
    let id = draft(ctx, items).await;
    step(ctx, id, "PENDING").await;
    let instance = instance_id_of(id)
        .await
        .unwrap_or_else(|| panic!("{}：提交后必须挂审批实例", ctx.tag));
    approve_instance(instance).await;
    assert_eq!(status_of(id).await, "APPROVED", "{}：审批通过后必须 APPROVED", ctx.tag);
    step(ctx, id, "IN_TRANSIT").await;
    id
}

// ==================== F01–F05：正向主线 ====================

/// F01 DRAFT → PENDING → 生成审批实例（PENDING + 待办 + 回写 approval_instance_id）。
#[tokio::test]
async fn f01_submit_creates_pending_instance() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F01").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    let id = draft(&ctx, vec![item(ctx.mat, "5")]).await;
    assert_eq!(status_of(id).await, "DRAFT");
    assert!(instance_id_of(id).await.is_none(), "F01：草稿态不该挂审批实例");

    step(&ctx, id, "PENDING").await;
    let instance = instance_id_of(id)
        .await
        .unwrap_or_else(|| panic!("F01：提交调拨必须发起审批并回写 approval_instance_id"));
    assert_eq!(
        instance_status(Some(instance)).await.as_deref(),
        Some("PENDING"),
        "F01：新发起的调拨审批必须是 PENDING"
    );
    let todos = common::flow_tasks_of(instance).await;
    assert!(!todos.is_empty(), "F01：提交调拨必须生成待办（一条都没有＝审批没发起）");
    assert!(
        todos.iter().all(|(_, status, _)| status == "PENDING"),
        "F01：新发起的调拨待办必须全部 PENDING，实际 {todos:?}"
    );
    // 待办条数**不做固定值断言**：`ADMIN / FALLBACK_ADMIN` 节点按角色解析出全部
    // SUPER_ADMIN 用户，而 `cloudpivot_test` 是并发共享库（别的卡会造几十个超管账号），
    // 所以「几条」是环境相关的；真正的不变量是「≥1 条且都 PENDING」。
    // 台账侧：提交只改状态，不动库存
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_from).await,
        common::dec("100"),
        "F01：提交不动库存",
    );
    assert_eq!(txn_count(id).await, 0, "F01：提交不写库存流水");
    ctx.cleanup().await;
}

/// F02 审批通过 → APPROVED（实例同步 APPROVED）。
#[tokio::test]
async fn f02_approval_moves_to_approved() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F02").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    let id = draft(&ctx, vec![item(ctx.mat, "5")]).await;
    step(&ctx, id, "PENDING").await;
    let instance = instance_id_of(id).await.expect("审批实例");
    approve_instance(instance).await;

    assert_eq!(status_of(id).await, "APPROVED", "F02：审批通过 → 调拨单 APPROVED");
    assert_eq!(
        instance_status(Some(instance)).await.as_deref(),
        Some("APPROVED"),
        "F02：审批实例本身也必须落 APPROVED"
    );
    assert_eq!(txn_count(id).await, 0, "F02：审批通过还不该写库存流水（发货才写）");
    ctx.cleanup().await;
}

/// F03 APPROVED → IN_TRANSIT：回填在途仓；**库存不动**（记账口径，见文件头说明）。
#[tokio::test]
async fn f03_ship_records_transit_warehouse_without_touching_stock() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F03").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    let id = in_transit(&ctx, vec![item(ctx.mat, "5")]).await;

    assert_eq!(status_of(id).await, "IN_TRANSIT", "F03：发货后必须 IN_TRANSIT");
    let transit = common::scalar_text(
        "SELECT transit_warehouse_id::text AS v FROM biz_stock_transfer WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await;
    let transit =
        transit.unwrap_or_else(|| panic!("F03：IN_TRANSIT 起 transit_warehouse_id 必须非空（ck_biz_transfer_state）"));

    // 卡面 F03 期望「调出仓扣减 + 在途仓增加」；实现是「发货只回填在途仓，结算才双写」。
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_from).await,
        common::dec("100"),
        "F03（设计差异，非缺陷）：发货这一步**不动**调出仓库存",
    );
    let transit_id = Uuid::parse_str(&transit).expect("在途仓 id");
    assert_eq!(
        inventory_rows(ctx.mat, transit_id).await,
        0,
        "F03（设计差异，非缺陷）：在途仓不收库存（没有实物在途台账，只在单据上记一个仓）"
    );
    ctx.cleanup().await;
}

/// F04 IN_TRANSIT → DONE：调出仓 -qty、调入仓 +qty（结算双写）。
#[tokio::test]
async fn f04_receive_moves_qty_between_warehouses() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F04").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    let id = in_transit(&ctx, vec![item(ctx.mat, "5")]).await;

    step(&ctx, id, "DONE").await;
    assert_eq!(status_of(id).await, "DONE");
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_from).await,
        common::dec("95"),
        "F04：调出仓必须 -5",
    );
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_to).await,
        common::dec("5"),
        "F04：调入仓必须 +5",
    );
    assert_eq!(
        txn_count(id).await,
        2,
        "F04：结算必须写 2 条流水（OUT TRANSFER_OUT + IN TRANSFER_IN）"
    );
    ctx.cleanup().await;
}

/// F05 ★ 调拨完成后：调出仓 + 调入仓总量守恒（成本也必须守恒）。
#[tokio::test]
async fn f05_total_qty_and_cost_conserved_after_done() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F05").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_to, "10", "8").await;

    let total_qty_before = bucket_qty(ctx.mat, ctx.wh_from).await + bucket_qty(ctx.mat, ctx.wh_to).await;
    let amount_before = common::scalar_dec(
        "SELECT COALESCE(sum(amount),0)::numeric(18,4) AS v FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id IN (?::uuid, ?::uuid)",
        vec![
            Value::from(&ctx.mat),
            Value::from(&ctx.wh_from),
            Value::from(&ctx.wh_to),
        ],
    )
    .await;

    let id = in_transit(&ctx, vec![item(ctx.mat, "5")]).await;
    step(&ctx, id, "DONE").await;

    let total_qty_after = bucket_qty(ctx.mat, ctx.wh_from).await + bucket_qty(ctx.mat, ctx.wh_to).await;
    let amount_after = common::scalar_dec(
        "SELECT COALESCE(sum(amount),0)::numeric(18,4) AS v FROM biz_inventory \
          WHERE material_id = ?::uuid AND warehouse_id IN (?::uuid, ?::uuid)",
        vec![
            Value::from(&ctx.mat),
            Value::from(&ctx.wh_from),
            Value::from(&ctx.wh_to),
        ],
    )
    .await;
    common::assert_dec(total_qty_after, total_qty_before, "F05：总量必须守恒（110 → 110）");
    common::assert_dec(
        amount_after,
        amount_before,
        "F05：成本必须原价转移 —— 两仓金额合计守恒（不是按调入仓均价重估）",
    );
    ctx.cleanup().await;
}

// ==================== F06–F09：取消 / 门闸 / 超发 ====================

/// F06 DRAFT → CANCELLED：不动库存、不写流水、不产生审批单。
#[tokio::test]
async fn f06_draft_cancel_leaves_stock_untouched() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F06").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    let id = draft(&ctx, vec![item(ctx.mat, "5")]).await;
    change_transfer_status(id, &status_req("CANCELLED", Some("F06 不调了")), ctx.worker)
        .await
        .unwrap_or_else(|error| panic!("F06：草稿取消失败 {} {}", error.code, error.msg));

    assert_eq!(status_of(id).await, "CANCELLED");
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_from).await,
        common::dec("100"),
        "F06：库存不动",
    );
    assert_eq!(txn_count(id).await, 0, "F06：不写流水");
    assert!(instance_id_of(id).await.is_none(), "F06：草稿取消不该凭空产生审批实例");
    ctx.cleanup().await;
}

/// F07 PENDING → CANCELLED：**在跑审批单必须一起终止**（否则再也提交不了）。
#[tokio::test]
async fn f07_pending_cancel_terminates_running_instance() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F07").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    let id = draft(&ctx, vec![item(ctx.mat, "5")]).await;
    step(&ctx, id, "PENDING").await;
    let instance = instance_id_of(id).await.expect("审批实例");

    change_transfer_status(id, &status_req("CANCELLED", Some("F07 撤回：计划改了")), ctx.worker)
        .await
        .unwrap_or_else(|error| panic!("F07：撤回失败 {} {}", error.code, error.msg));

    assert_eq!(status_of(id).await, "CANCELLED");
    assert_eq!(
        instance_status(Some(instance)).await.as_deref(),
        Some("CANCELLED"),
        "F07：撤回必须终止在跑的审批单（否则审批中心挂着点不动的孤儿单）"
    );
    let todos = common::flow_tasks_of(instance).await;
    assert!(
        todos.iter().all(|(_, status, _)| status != "PENDING"),
        "F07：撤回后不该还有 PENDING 待办，实际 {todos:?}"
    );
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_from).await,
        common::dec("100"),
        "F07：撤回不动库存",
    );

    // 反向（这条是 F07 真正的价值）：撤回后同一张单必须能重新提交，
    // 而不是撞 `uk_biz_flow_instance_biz_active` 永久 409。
    let again = change_transfer_status(id, &status_req("PENDING", None), ctx.worker).await;
    // CANCELLED 是终态，重新提交会被状态机挡（409）—— 关键是没有 500 / 唯一键冲突。
    match again {
        Ok(()) => panic!("F07：CANCELLED 不该能回到 PENDING"),
        Err(error) => assert_eq!(
            error.status, 409,
            "F07：重新提交应被状态机以 409 拦住（不是唯一键 500），实际 {error:?}"
        ),
    }
    ctx.cleanup().await;
}

/// F08 PENDING 状态人工直推 APPROVED → 被审批门闸拦住（409 + 65030）。
///
/// 卡面原写「当前可，标注缺陷」—— 实测 P5 起已有门闸 `flow_gate_blocked`：
/// 挂着在跑的审批单时，人工不许把单据直推 APPROVED。这里断言**正确行为**（拦住）。
#[tokio::test]
async fn f08_manual_approve_while_pending_is_blocked() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F08").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;
    let id = draft(&ctx, vec![item(ctx.mat, "5")]).await;
    step(&ctx, id, "PENDING").await;
    let instance = instance_id_of(id).await.expect("审批实例");

    let error = change_transfer_status(id, &status_req("APPROVED", None), ctx.worker)
        .await
        .expect_err("F08：审批中人工直推 APPROVED 必须被门闸拦住");
    assert_eq!(error.status, 409, "F08：门闸 → 409，实际 {error:?}");
    assert_eq!(status_of(id).await, "PENDING", "F08：被拦住后必须仍是 PENDING");
    assert_eq!(
        instance_status(Some(instance)).await.as_deref(),
        Some("PENDING"),
        "F08：绕过审批不许改实例状态"
    );
    ctx.cleanup().await;
}

/// F09 调拨数量 > 调出仓库存 → 结算被拒（409「库存不足」），状态留在 IN_TRANSIT。
///
/// 实测口径：数量校验**不在建单时**，而在 `IN_TRANSIT → DONE` 的结算里
/// （条件 UPDATE 返回 0 行 → 整笔回滚）。所以这里一路推到 IN_TRANSIT，
/// 再断言收货失败且状态/库存都不变 —— 这才是「超发」真正会被挡下的位置。
#[tokio::test]
async fn f09_over_issue_is_rejected_at_settlement() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F09").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "3", "12.5").await;
    let id = in_transit(&ctx, vec![item(ctx.mat, "5")]).await;
    let before_from = bucket_qty(ctx.mat, ctx.wh_from).await;
    let before_to = bucket_qty(ctx.mat, ctx.wh_to).await;

    let error = change_transfer_status(id, &status_req("DONE", None), ctx.worker)
        .await
        .expect_err("F09：库存只有 3，调拨 5 必须被拒");
    assert_eq!(error.status, 409, "F09：库存不足 → 409，实际 {error:?}");
    assert!(
        error.msg.contains("库存不足"),
        "F09：文案必须点名库存不足，实际：{}",
        error.msg
    );
    assert_eq!(
        status_of(id).await,
        "IN_TRANSIT",
        "F09：结算失败必须回滚到 IN_TRANSIT（不能出现「状态 DONE、库存没动」）"
    );
    common::assert_dec(bucket_qty(ctx.mat, ctx.wh_from).await, before_from, "F09：调出仓不变");
    common::assert_dec(bucket_qty(ctx.mat, ctx.wh_to).await, before_to, "F09：调入仓不变");
    ctx.cleanup().await;
}

// ==================== F10–F12：并发 / 序列号 / 在途压货 ====================

/// F10 同物料并发调拨 → 不死锁，且总量守恒。
///
/// 两张单各发 5 件（库存 100）：并行收货必须都成功。若两张单都要 8 件（库存 12），
/// 则只允许一张成功 —— 这里取「都成功」的版本，因为要证的是**不互相锁死**
/// （死锁会让两边都超时，测试直接挂住，比断言失败更明显）。
#[tokio::test]
async fn f10_concurrent_transfers_do_not_deadlock() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F10").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "10").await;
    let a = in_transit(&ctx, vec![item(ctx.mat, "5")]).await;
    let b = in_transit(&ctx, vec![item(ctx.mat, "5")]).await;

    let worker = ctx.worker;
    let req_a = status_req("DONE", None);
    let req_b = status_req("DONE", None);
    let both = tokio::time::timeout(std::time::Duration::from_secs(60), async move {
        let ra = change_transfer_status(a, &req_a, worker);
        let rb = change_transfer_status(b, &req_b, worker);
        tokio::join!(ra, rb)
    })
    .await
    .expect("F10：并发收货超时 ＝ 死锁（60s 未返回）");

    let (first, second) = both;
    if let Err(error) = first {
        panic!("F10：第一张单收货失败：{} {}", error.code, error.msg);
    }
    if let Err(error) = second {
        panic!("F10：第二张单收货失败：{} {}", error.code, error.msg);
    }
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_from).await,
        common::dec("90"),
        "F10：调出仓 100 - 5 - 5",
    );
    common::assert_dec(
        bucket_qty(ctx.mat, ctx.wh_to).await,
        common::dec("10"),
        "F10：调入仓 5 + 5",
    );
    ctx.cleanup().await;
}

/// F11 调拨中的序列号跟着走（一物一码）：收货后序列号落在调入仓、仍在库。
#[tokio::test]
async fn f11_serials_follow_the_transfer() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F11").await;
    seed_stock(&ctx, ctx.mat_sn, ctx.wh_from, "1", "30").await;
    let serial = seed_serial(&ctx, &ctx.code("SN-F11")).await;

    let mut line = item(ctx.mat_sn, "1");
    line.serial_ids = vec![serial.to_string()];
    let id = in_transit(&ctx, vec![line]).await;
    step(&ctx, id, "DONE").await;

    let warehouse = common::scalar_text(
        "SELECT warehouse_id::text AS v FROM biz_material_serial WHERE id = ?::uuid",
        vec![Value::from(&serial)],
    )
    .await;
    assert_eq!(
        warehouse.as_deref(),
        Some(ctx.wh_to.to_string().as_str()),
        "F11：序列号必须跟着货到调入仓"
    );
    let status = common::scalar_text(
        "SELECT status AS v FROM biz_material_serial WHERE id = ?::uuid",
        vec![Value::from(&serial)],
    )
    .await;
    assert_eq!(
        status.as_deref(),
        Some("IN_STOCK"),
        "F11：调拨不改变件的在库状态（只是换了仓）"
    );
    common::assert_dec(
        bucket_qty(ctx.mat_sn, ctx.wh_from).await,
        common::dec("0"),
        "F11：调出仓清零",
    );
    common::assert_dec(
        bucket_qty(ctx.mat_sn, ctx.wh_to).await,
        common::dec("1"),
        "F11：调入仓 1 件",
    );
    ctx.cleanup().await;
}

/// F12 在途仓不能长期压货（`IN_TRANSIT` 超期：可查询 + 可告警）。
///
/// # 这条用例守什么（LNK-F12 转正后的口径）
///
/// 原用例只断言了一句「`IN_TRANSIT` 且 `shipped_at < now() - 7 天` 的裸 SQL 计数」——
/// 那种断言在**没有任何超期实现**时也是绿的（实测：带 `--ignored` 单跑 4.85s ok），
/// 等于没测。转正后必须钉住四件事，缺一件这条用例就会红：
///
/// 1. **阈值口径**：来自 `sys_config`（默认 7 天），判定基准是 `shipped_at`；
/// 2. **可查询**：列表筛选 `in_transit_overdue=true` 能把压了 30 天的单捞出来，
///    `=false` 与「阈值放到 40 天」两种反向对照都不能捞出来；刚发货的对照单也不许出现；
/// 3. **可告警**：扫描任务（`transfer_overdue::scan_once`，生产里每小时跑）真的往
///    `sys_notice` 落了一条告警，收件人是申请人，并回写了告警台账；
/// 4. **不重复轰炸**：紧接着再扫一次不再新增通知（重提醒间隔 24 小时 + 幂等键 + CAS）。
#[tokio::test]
async fn f12_in_transit_stuck_shipment_should_alert() {
    if common::db_or_skip().await.is_none() {
        return;
    }
    // 进程级串行闸（T17 会临时下线全局默认流程，见 common::serial 的说明）
    let _gate = common::serial().await;
    let ctx = Ctx::new("F12").await;
    seed_stock(&ctx, ctx.mat, ctx.wh_from, "100", "12.5").await;

    // 两张在途单：`stuck` 压 30 天（应告警），`fresh` 刚发货（对照，不许被误判）
    let stuck = in_transit(&ctx, vec![item(ctx.mat, "5")]).await;
    let fresh = in_transit(&ctx, vec![item(ctx.mat, "3")]).await;
    db().execute_with_params(
        "UPDATE biz_stock_transfer SET shipped_at = now() - interval '30 days' WHERE id = ?::uuid",
        vec![Value::from(&stuck)],
    )
    .await
    .expect("F12：倒推发货时间失败");

    // ---------- ① 阈值：走 sys_config，默认 7 天 ----------
    let threshold = transfer_model::overdue_threshold_days().await;
    assert_eq!(
        threshold,
        transfer_model::DEFAULT_OVERDUE_DAYS,
        "F12：超期阈值必须来自 sys_config 且默认 7 天（实测读到 {threshold}）"
    );
    assert_eq!(
        transfer_model::CONFIG_OVERDUE_DAYS_KEY,
        "erp.transfer.in_transit_overdue_days",
        "F12：参数键是与迁移/前端约定的契约，不能改"
    );

    // ---------- ② 行级判定（生产路径：TransferRes::with_overdue） ----------
    let now = nik::prelude::chrono::Utc::now();
    let stuck_row = transfer_model::find_by_id(stuck)
        .await
        .expect("F12：查调拨单")
        .expect("F12：调拨单必须存在");
    assert_eq!(stuck_row.status, "IN_TRANSIT");
    assert!(
        stuck_row.is_overdue(now, threshold),
        "F12：压了 30 天的在途单必须被判为超期"
    );
    let stuck_res = TransferRes::of(&stuck_row, None, None).with_overdue(&stuck_row, now, threshold);
    assert!(stuck_res.in_transit_overdue, "F12：列表响应要带超期标记");
    assert_eq!(
        stuck_res.overdue_days,
        Some(30),
        "F12：已压天数要算得出来（30 天 → 30）"
    );

    // ---------- ③ 可查询：列表筛选 in_transit_overdue=true ----------
    // 只按本用例的调出仓收口（cloudpivot_test 是并发共享库，不做全局断言）
    let (overdue_rows, _) = transfer_model::page(&TransferQuery {
        from_warehouse_id: Some(ctx.wh_from),
        in_transit_overdue: Some(true),
        overdue_days: threshold,
        page_size: 200,
        ..Default::default()
    })
    .await
    .expect("F12：按超期筛选分页");
    assert!(
        overdue_rows.iter().any(|row| row.id == stuck),
        "F12：超期单必须能被 in_transit_overdue=true 筛出来（这是「压货可见」的入口）"
    );
    assert!(
        !overdue_rows.iter().any(|row| row.id == fresh),
        "F12：刚发货的单不能被筛成超期（否则告警就是噪音）"
    );

    // 反向对照一：in_transit_overdue=false 不含超期单
    let (normal_rows, _) = transfer_model::page(&TransferQuery {
        from_warehouse_id: Some(ctx.wh_from),
        in_transit_overdue: Some(false),
        overdue_days: threshold,
        page_size: 200,
        ..Default::default()
    })
    .await
    .expect("F12：排除超期分页");
    assert!(
        !normal_rows.iter().any(|row| row.id == stuck),
        "F12：in_transit_overdue=false 不该返回超期单"
    );
    assert!(
        normal_rows.iter().any(|row| row.id == fresh),
        "F12：in_transit_overdue=false 应当保留正常在途单"
    );

    // 反向对照二：阈值放宽到 40 天 → 同一张单不再超期（证明阈值真的参与判定，
    // 而不是「写死 7 天」或「只要有 shipped_at 就算」）
    let (loose_rows, _) = transfer_model::page(&TransferQuery {
        from_warehouse_id: Some(ctx.wh_from),
        in_transit_overdue: Some(true),
        overdue_days: 40,
        page_size: 200,
        ..Default::default()
    })
    .await
    .expect("F12：放宽阈值分页");
    assert!(
        !loose_rows.iter().any(|row| row.id == stuck),
        "F12：阈值 40 天时压 30 天的单不该算超期（阈值必须生效）"
    );

    // 汇总接口口径（全库计数，至少包含本用例这张）
    let summary = transfer_model::overdue_summary(threshold, now)
        .await
        .expect("F12：超期汇总");
    assert!(
        summary.total >= 1,
        "F12：超期汇总必须数到本用例这张（实测 total={}）",
        summary.total
    );
    assert_eq!(summary.threshold_days, threshold);

    // ---------- ④ 可告警：扫描任务真的把单推给人 ----------
    // 台账基线：扫描前从未提醒过
    assert_eq!(
        common::scalar_i64(
            "SELECT overdue_alert_count::int AS v FROM biz_stock_transfer WHERE id = ?::uuid",
            vec![Value::from(&stuck)],
        )
        .await,
        0,
        "F12：扫描前告警台账必须是 0"
    );

    let report = transfer_overdue::scan_once(now)
        .await
        .expect("F12：超期扫描任务不能报错");
    assert!(
        report.alerted >= 1,
        "F12：扫描必须至少告警一张单（{}）",
        report.summary()
    );

    // 告警落库到 sys_notice（复用既有通知设施，不另造表）
    let notice_count = common::scalar_i64(
        "SELECT count(*) AS v FROM sys_notice \
          WHERE biz_type = ? AND biz_id = ? AND deleted_at IS NULL",
        vec![
            Value::Text(transfer_overdue::BIZ_TYPE.to_string()),
            Value::Text(stuck.to_string()),
        ],
    )
    .await;
    assert_eq!(
        notice_count,
        1,
        "F12：必须落一条超期告警通知（biz_type={}）",
        transfer_overdue::BIZ_TYPE
    );

    // 收件人是申请人（发货方），且是强制送达（force=true 跳过 notify.inbox 偏好）
    let receiver = common::scalar_i64(
        "SELECT count(*) AS v FROM sys_notice_receiver r \
           JOIN sys_notice n ON n.id = r.notice_id \
          WHERE n.biz_type = ? AND n.biz_id = ? AND r.user_id = ?::uuid",
        vec![
            Value::Text(transfer_overdue::BIZ_TYPE.to_string()),
            Value::Text(stuck.to_string()),
            Value::from(&ctx.worker),
        ],
    )
    .await;
    assert_eq!(
        receiver, 1,
        "F12：告警必须推到申请人（发货方）手上，而不是「发出去没人收」"
    );

    // 台账回写：次数 1 + 时间非空（下一轮才能按间隔跳过）
    let (alert_count, alerted_at_set) = {
        let row = db()
            .find_with_params(
                "SELECT overdue_alert_count::int AS cnt, \
                        (overdue_alerted_at IS NOT NULL)::text AS flagged \
                   FROM biz_stock_transfer WHERE id = ?::uuid",
                vec![Value::from(&stuck)],
            )
            .await
            .expect("F12：读告警台账");
        let row = row.first().expect("F12：调拨单必须存在");
        (
            row.get_typed::<i32>("cnt").expect("告警次数"),
            row.get_typed::<String>("flagged").expect("告警时间标记"),
        )
    };
    assert_eq!(alert_count, 1, "F12：告警次数必须回写为 1");
    assert_eq!(alerted_at_set, "true", "F12：告警时间必须回写");

    // 刚发货的对照单不许被告警（否则「超期」这两个字就没意义了）
    let fresh_notices = common::scalar_i64(
        "SELECT count(*) AS v FROM sys_notice WHERE biz_type = ? AND biz_id = ?",
        vec![
            Value::Text(transfer_overdue::BIZ_TYPE.to_string()),
            Value::Text(fresh.to_string()),
        ],
    )
    .await;
    assert_eq!(fresh_notices, 0, "F12：未超期的在途单不许告警");

    // ---------- ⑤ 不重复轰炸：紧接着再扫一次 ----------
    let second = transfer_overdue::scan_once(now).await.expect("F12：第二轮扫描不能报错");
    assert_eq!(
        second.alerted,
        0,
        "F12：重提醒间隔内不许再次告警（{}）",
        second.summary()
    );
    let notice_after = common::scalar_i64(
        "SELECT count(*) AS v FROM sys_notice WHERE biz_type = ? AND biz_id = ?",
        vec![
            Value::Text(transfer_overdue::BIZ_TYPE.to_string()),
            Value::Text(stuck.to_string()),
        ],
    )
    .await;
    assert_eq!(notice_after, 1, "F12：第二轮不许新增通知（幂等）");

    // ---------- 收尾：告警通知属于本轮夹具，清掉 ----------
    // 只删本模块 biz_type 的行：`transfer_overdue` 是 LNK-F12 新引入的业务类型，
    // 库里不存在历史数据；再按 biz_id 收口到本用例的调拨单。
    db().execute_with_params(
        "DELETE FROM sys_notice WHERE biz_type = ? AND biz_id IN (?, ?)",
        vec![
            Value::Text(transfer_overdue::BIZ_TYPE.to_string()),
            Value::Text(stuck.to_string()),
            Value::Text(fresh.to_string()),
        ],
    )
    .await
    .expect("F12：清理告警通知失败");
    ctx.cleanup().await;
}

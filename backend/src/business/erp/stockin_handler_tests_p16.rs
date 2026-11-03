// ============================================================================
// stockin_handler_tests_p16.rs —— P16：作废走审批流（提交/撤回/通过/驳回/对账/自审拦截）
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
use crate::business::erp::stockin_model::{
    self, STATUS_CANCELLED, STATUS_PENDING_CANCEL, STATUS_POSTED,
};
use crate::business::flow::model as flow_model;
use crate::shared::error_code;

/// ① 发起：`POSTED → PENDING_CANCEL` + 生成审批实例 + 回写 `approval_instance_id`，
/// 而**六本账一动没动**。
#[tokio::test]
async fn p16_submit_moves_to_pending_cancel_with_instance() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "10", "2", "0")]).await;
    let before = inventory_of(MATERIAL_A).await;
    assert_eq!(before.0, dec("10"), "前置：过账后库存 10");

    cancel_request(id, "供应商召回").await;

    let detail = detail_of(id).await.unwrap();
    assert_eq!(
        detail.header.status, STATUS_PENDING_CANCEL,
        "发起作废只挂审批单，单据进「作废审批中」"
    );
    let instance_id = detail
        .header
        .approval_instance_id
        .clone()
        .expect("必须回写 approval_instance_id");
    assert_eq!(
        detail.header.cancel_reason.as_deref(),
        Some("供应商召回"),
        "申请理由要落库（审批人据此判断）"
    );
    // 状态日志：POSTED → PENDING_CANCEL
    let trail = crate::business::shared::state_machine::trail_of_no(db(), &detail.header.in_no)
        .await
        .unwrap();
    assert_eq!(trail.last().unwrap().to_status, STATUS_PENDING_CANCEL);
    // 六本账一个字节都没动
    assert_eq!(inventory_of(MATERIAL_A).await, before, "发起审批不许动库存");
    assert_eq!(
        txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        0,
        "发起审批不许写反向流水"
    );
    assert_eq!(voucher_count(id, Some(1)).await, 0, "发起审批不许红冲");
    // 审批中心真的有待办（否则等于没人能复核）。
    // ⚠️ `n_review` 的审批人来源是 `ADMIN`（运行时展开**全部超管**）+ `join_mode=AND`（会签）
    //    ⇒ 待办数 = 超管人数（库里多一个超管就多一条，写死 1 会在有第二个超管时假红）。
    let admins = crate::business::flow::test_support::super_admin_count().await;
    assert!(
        admins > 0,
        "库里必须至少有一个超管，否则 `ADMIN` 节点只能走 `FALLBACK_ADMIN` 兜底"
    );
    let todos: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&Uuid::parse_str(&instance_id).unwrap())],
        )
        .await
        .unwrap();
    assert_eq!(
        todos, admins,
        "发起后审批中心必须给**每个超管**各 1 条待办（ADMIN 展开 + 会签：待办数 {todos}，超管 {admins}）"
    );
}

/// **DC-01 ③**：没配 `stock_in_cancel` 流程 → **409 + 66033**（改造前是静默直接作废、六本账立刻回滚）。
///
/// 这是 P16 那条「② 未配流程 → 降级：直接作废」用例的**反向改写**：
/// 断言的是「必须拒绝」，并且拒绝后**六本账一个字节都没动**、单据仍是 POSTED。
#[tokio::test]
async fn dc01_cancel_without_published_flow_is_409() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_B, "6", "5", "0")]).await;
    let before = inventory_of(MATERIAL_B).await;
    // 把种子流程临时「下线」（干净恢复由 cleanup_fixtures 保证）
    db().execute_with_params(
        "UPDATE biz_flow_def SET deleted_at = now() WHERE flow_code = 'stock_in_cancel'",
        vec![],
    )
    .await
    .unwrap();
    assert!(
        stockin_model::published_cancel_flow()
            .await
            .unwrap()
            .is_none(),
        "流程下线后必须查不到"
    );

    let error = change_stock_in_status(id, &cancel_req(Some("这次没配流程")), Some(uuid_keeper()))
        .await
        .expect_err("没配流程必须 409，不能静默直接作废");
    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(
        error.code,
        error_code::APPROVAL_FLOW_NOT_CONFIGURED,
        "必须用 DC-01 的新码 66033：{}",
        error.msg
    );
    assert!(
        error.msg.contains("stock_in_cancel"),
        "报文必须点名缺哪个 flow_code：{}",
        error.msg
    );

    // 六本账 / 单据状态一动不动（改造前这里会被直接作废并回滚库存）
    let detail = detail_of(id).await.unwrap();
    assert_eq!(
        detail.header.status, STATUS_POSTED,
        "被拒后单据必须仍是已过账"
    );
    assert!(
        detail.header.approval_instance_id.is_none(),
        "被拒后不许挂审批单"
    );
    assert_eq!(
        inventory_of(MATERIAL_B).await,
        before,
        "被拒后库存一个字节都不能动"
    );
    assert_eq!(
        txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        0,
        "被拒后不许写反向流水"
    );
}

/// ③ 重复发起 → 409（已有在跑的实例），且不再生成第二条实例。
#[tokio::test]
async fn p16_duplicate_submit_is_409() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "4", "2", "0")]).await;
    cancel_request(id, "第一次发起").await;
    let err = change_stock_in_status(id, &cancel_req(Some("第二次发起")), Some(uuid_keeper()))
        .await
        .expect_err("已有在跑的审批单必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("审批中") || err.msg.contains("已有在跑的作废审批"),
        "报文要点明原因与出路：{}",
        err.msg
    );
    let instances: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_CANCEL' AND biz_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    assert_eq!(instances, 1, "不能生成第二条审批实例");
    assert_eq!(
        detail_of(id).await.unwrap().header.status,
        STATUS_PENDING_CANCEL
    );
}

/// ④ 作废审批中单据**锁定**：改金额 / 改明细 → 409，数据一个字节都不动。
#[tokio::test]
async fn p16_pending_cancel_is_locked() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "8", "2", "0")]).await;
    cancel_request(id, "审批中不许改").await;

    // 改明细（数量从 8 改成 99）
    let mut changed = req(vec![line(MATERIAL_A, "99", "2", "0")]);
    changed.total_amount = Some(dec("198"));
    let err = update_stock_in(id, &changed, Some(uuid_keeper()))
        .await
        .expect_err("作废审批中改明细必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("不可修改") || err.msg.contains("已入账"),
        "文案要说清锁定语义：{}",
        err.msg
    );
    // 只改金额合计
    let mut amount_only = req(vec![line(MATERIAL_A, "8", "2", "0")]);
    amount_only.total_amount = Some(dec("999"));
    let err = update_stock_in(id, &amount_only, Some(uuid_keeper()))
        .await
        .expect_err("作废审批中改金额必须 409");
    assert_eq!(err.status, 409, "{}", err.msg);
    // 库里数据没被改（明细 qty 仍是 8、合计仍是 16）
    let items = stockin_model::items_of(id).await.unwrap();
    assert_eq!(items[0].qty, dec("8"), "锁定期间明细必须原样");
    assert_eq!(detail_of(id).await.unwrap().header.total_amount, dec("16"));
    // 审批中的单子也删不掉（只能先撤回）
    let err = delete_stock_in(id, Some(uuid_keeper()))
        .await
        .expect_err("审批中的单子不能删");
    assert_eq!(err.status, 409, "{}", err.msg);
}

/// ⑤ 审批通过 → 单据 `CANCELLED`，**六本账全部回滚**（库存 / 库位账 / 流水 / 凭证 / 应付 / 序列号）。
#[tokio::test]
async fn p16_approval_rolls_back_six_ledgers() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let loc = seed_location_row(MATERIAL_SN, LOC_DICT_A).await;
    let id = post_new(vec![line_at(
        MATERIAL_SN,
        "3",
        "100",
        "0",
        &loc.to_string(),
    )])
    .await;
    assert_eq!(
        location_qty_of(&loc).await,
        Some(dec("3")),
        "前置：库位账 3 件"
    );
    assert_eq!(serial_counts_of(id).await.0, 3, "一物一码 3 件在库");
    assert_eq!(voucher_count(id, None).await, 1, "过账生成 1 张凭证");
    let payable_status = |sql: String| async move {
        db().scalar_with_params(&sql, vec![Value::from(&id)])
            .await
            .unwrap()
    };
    let payable_before: String =
        payable_status("SELECT status::text FROM biz_payable WHERE source_id = ?::uuid".into())
            .await;
    assert_eq!(payable_before, "OPEN");

    cancel_request(id, "整批退回").await;
    approve_cancel(id).await;

    // ① 单据
    assert_eq!(detail_of(id).await.unwrap().header.status, STATUS_CANCELLED);
    // ② 库存（成本账）—— 本单带库位，成本账与库位账两本都要归零
    let cost_rows = db()
        .find_with_params(
            "SELECT qty, amount FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id = ?::uuid",
            vec![
                Value::from(&Uuid::parse_str(MATERIAL_SN).unwrap()),
                Value::from(&Uuid::parse_str(WAREHOUSE).unwrap()),
                Value::from(&loc),
            ],
        )
        .await
        .unwrap();
    if let Some(row) = cost_rows.first() {
        assert_eq!(
            row.get_typed::<Decimal>("qty").unwrap(),
            Decimal::ZERO,
            "成本账数量必须归零"
        );
        assert_eq!(
            row.get_typed::<Decimal>("amount").unwrap(),
            Decimal::ZERO,
            "成本账金额必须归零"
        );
    }
    // ③ 库位账
    assert_eq!(location_qty_of(&loc).await, Some(Decimal::ZERO));
    // ④ 流水：一条 OUT / PURCHASE_IN_CANCEL
    assert_eq!(
        txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        1
    );
    // ⑤ 凭证：多出一张红字
    assert_eq!(voucher_count(id, Some(1)).await, 1, "必须生成红字凭证");
    // ⑥ 应付：作废
    let payable_after: String =
        payable_status("SELECT status::text FROM biz_payable WHERE source_id = ?::uuid".into())
            .await;
    assert_eq!(payable_after, "CANCELLED", "应付单必须联动作废");
    // ⑦ 序列号：3 件全部软删（行保留、审计不丢）
    let (alive, deleted) = serial_counts_of(id).await;
    assert_eq!((alive, deleted), (0, 3), "序列号必须软删（不是物理删）");
}

/// ⑥ 审批通过 → 库存回滚金额用**过账流水里的 amount**，不是现价（P9 口径）。
#[tokio::test]
async fn p16_approval_uses_posted_amount_not_current_avg() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let first = post_new(vec![line(MATERIAL_A, "100", "2", "0")]).await;
    post_new(vec![line(MATERIAL_A, "100", "3", "0")]).await;
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (dec("200"), dec("500"), dec("2.500000"))
    );

    cancel_request(first, "第一张收错了").await;
    // 审批中：账没动
    assert_eq!(
        inventory_of(MATERIAL_A).await,
        (dec("200"), dec("500"), dec("2.500000")),
        "审批未通过前库存必须原样"
    );
    approve_cancel(first).await;

    let (qty, amount, _avg) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("100"));
    assert_eq!(amount, dec("300"), "退的是流水原额 200（500-200）");
    assert_ne!(amount, dec("250"), "按现价 2.5×100 是错的");
}

/// ⑦ 审批通过 → 生成红字凭证（借贷互换、原凭证打 reversed_by）。
#[tokio::test]
async fn p16_approval_generates_red_voucher() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "10", "3", "0")]).await;
    assert_eq!(voucher_count(id, Some(1)).await, 0);
    cancel(id, "退货").await;
    assert_eq!(voucher_count(id, Some(1)).await, 1, "必须有 1 张红字凭证");
    // 红字凭证借贷平衡：Σ借 = Σ贷
    let (debit, credit): (Decimal, Decimal) = db()
        .find_with_params(
            "SELECT COALESCE(SUM(e.debit_amount),0) AS d, COALESCE(SUM(e.credit_amount),0) AS c \
               FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id \
              WHERE v.source_id = ?::uuid AND v.is_red = 1",
            vec![Value::from(&id)],
        )
        .await
        .map(|rows| {
            let row = rows.first().expect("红字凭证分录");
            (
                row.get_typed::<Decimal>("d").unwrap(),
                row.get_typed::<Decimal>("c").unwrap(),
            )
        })
        .unwrap();
    assert_eq!(debit, credit, "红字凭证必须借贷平衡");
}

/// ⑧ 审批通过 → 序列号软删（行保留，`deleted_at` 置位）。
#[tokio::test]
async fn p16_approval_voids_serials() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_SN, "2", "50", "0")]).await;
    assert_eq!(serial_counts_of(id).await, (2, 0));
    cancel_request(id, "整批退回").await;
    assert_eq!(serial_counts_of(id).await, (2, 0), "审批中不许提前动序列号");
    approve_cancel(id).await;
    assert_eq!(serial_counts_of(id).await, (0, 2), "审批通过才软删");
}

/// ⑨ 审批驳回 → 单据回到 `POSTED`，**账一点没动**，实例落 `REJECTED`。
#[tokio::test]
async fn p16_reject_restores_posted() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "12", "2", "0")]).await;
    let before = inventory_of(MATERIAL_A).await;
    cancel_request(id, "想作废").await;
    reject_cancel(id, "不同意，数量再核一遍").await;

    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.header.status, STATUS_POSTED, "驳回必须打回已过账");
    assert!(
        detail.header.approval_instance_id.is_none(),
        "打回后实例引用必须清空（否则下次发起撞唯一索引）"
    );
    assert_eq!(inventory_of(MATERIAL_A).await, before, "驳回不许动账");
    assert_eq!(
        txn_count(id, stockin_model::BIZ_TYPE_PURCHASE_IN_CANCEL).await,
        0
    );
    assert_eq!(voucher_count(id, Some(1)).await, 0);
    let instance_id: Option<Uuid> = db()
        .scalar_with_params(
            "SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_CANCEL' AND biz_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    let instance = flow_model::find_instance(instance_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance.status, "REJECTED");
    assert_eq!(
        instance_status_of(id).await,
        None,
        "打回后单据不该再挂着实例（ck_biz_si_pending_cancel）"
    );
}

/// ⑩ 申请人撤回 → 实例 `WITHDRAWN`，单据回到 `POSTED`，账不动。
#[tokio::test]
async fn p16_withdraw_restores_posted() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_B, "5", "4", "0")]).await;
    let before = inventory_of(MATERIAL_B).await;
    cancel_request(id, "反悔了").await;
    let instance_id: Option<Uuid> = db()
        .scalar_with_params(
            "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();
    let instance_id = instance_id.expect("审批中必须有实例");

    withdraw_cancel_approval(id, uuid_keeper())
        .await
        .expect("发起人撤回必须成功");

    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.header.status, STATUS_POSTED);
    assert!(detail.header.approval_instance_id.is_none());
    let instance = flow_model::find_instance(instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance.status, "WITHDRAWN", "撤回要留 WITHDRAWN 轨迹");
    assert_eq!(inventory_of(MATERIAL_B).await, before, "撤回不许动账");
    // 待办不能留成 PENDING（否则就是孤儿待办）
    let pending: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance_id)],
        )
        .await
        .unwrap();
    assert_eq!(pending, 0, "撤回后不许留下 PENDING 待办");
}

/// ⑪ 驳回后**可以再次发起**作废（实例已落终态，不撞 uk_ 唯一索引）。
#[tokio::test]
async fn p16_resubmit_after_reject() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "6", "2", "0")]).await;
    cancel_request(id, "第一次申请").await;
    reject_cancel(id, "不同意：先跟供应商确认").await;
    cancel_request(id, "第二次申请：已确认").await;
    assert_eq!(
        detail_of(id).await.unwrap().header.status,
        STATUS_PENDING_CANCEL,
        "驳回后必须能再次发起"
    );
    approve_cancel(id).await;
    assert_eq!(
        detail_of(id).await.unwrap().header.status,
        STATUS_CANCELLED,
        "第二次复核通过后必须真作废"
    );
}

/// ⑫ 反向：收货单被**软删**（数据修复 / 其它路径）→ 审批实例被回收，不留孤儿待办。
#[tokio::test]
async fn p16_soft_deleted_doc_reconciles_instance() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_B, "3", "2", "0")]).await;
    cancel_request(id, "要作废").await;
    let instance_id: Uuid = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .expect("审批中必须有实例");
    // 模拟「单据被别的路径软删」：删单 API 只放行 DRAFT，所以这里直接改库（数据修复口径），
    // 而 `ck_biz_si_pending_cancel` 要求非审批中状态不得挂实例 → 一起清掉引用
    db().execute_with_params(
        "UPDATE biz_stock_in SET deleted_at = now(), status = 'CANCELLED', \
            approval_instance_id = NULL WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .unwrap();

    let cancelled = crate::business::erp::stockin_bridge::reconcile_orphan_cancels()
        .await
        .expect("孤儿回收必须成功");
    assert!(cancelled >= 1, "必须回收至少 1 条（实际 {cancelled}）");

    let instance = flow_model::find_instance(instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance.status, "CANCELLED", "单据没了，审批单必须终止");
    let pending: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance_id)],
        )
        .await
        .unwrap();
    assert_eq!(pending, 0, "P11 教训：不许留孤儿待办");
}

/// ⑬ 反向：审批中单据被**其它路径**改成 `CANCELLED` → 审批实例被回收。
#[tokio::test]
async fn p16_other_path_cancel_reconciles_instance() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "5", "2", "0")]).await;
    cancel_request(id, "要作废").await;
    let instance_id: Uuid = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .expect("审批中必须有实例");
    // 模拟「别处把单据改成了已作废」：审批单还挂着 PENDING
    db().execute_with_params(
        "UPDATE biz_stock_in SET status = 'CANCELLED', approval_instance_id = NULL, \
            cancelled_at = now() WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
    .unwrap();

    crate::business::erp::stockin_bridge::reconcile_orphan_cancels()
        .await
        .expect("孤儿回收必须成功");
    let instance = flow_model::find_instance(instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance.status, "CANCELLED");
    // 回收不许「顺手」回滚六本账（那是审批通过才有的动作）
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        dec("5"),
        "回收只终止审批单，正本库存不动"
    );
}

/// ⑭ 撤审批失败（非发起人撤回 → 403）→ **整笔回滚**：单据状态与实例都不变。
#[tokio::test]
async fn p16_withdraw_failure_rolls_back() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "7", "2", "0")]).await;
    cancel_request(id, "要作废").await;
    let instance_id: Uuid = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT approval_instance_id FROM biz_stock_in WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .expect("审批中必须有实例");
    // 别人（不是发起人）来撤 → 引擎 403，整笔回滚
    let outsider = Uuid::parse_str("b2b10009-0000-4000-8000-0000000000ff").unwrap();
    let err = withdraw_cancel_approval(id, outsider)
        .await
        .expect_err("非发起人不能撤回");
    assert_eq!(err.status, 403, "{}", err.msg);

    let detail = detail_of(id).await.unwrap();
    assert_eq!(
        detail.header.status, STATUS_PENDING_CANCEL,
        "撤回失败时单据状态必须原样（整笔回滚）"
    );
    assert_eq!(
        detail.header.approval_instance_id.as_deref(),
        Some(instance_id.to_string().as_str()),
        "实例引用也不能被清空"
    );
    let instance = flow_model::find_instance(instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(instance.status, "PENDING", "审批单还在跑");
}

/// ⑮ 内控：作废复核人**不能是制单人**（`self_approval=FORBID` 硬拦在派单）。
///
/// 构造：制单人是超管（= 唯一的审批候选人），由仓管员代发起 → 引擎派单时命中
/// 「候选人是发起人？不是；但流程节点 FORBID…」—— 这里断言的是**发起人=超管**的场景：
/// 超管自己发起作废，候选里就有他自己 → 422 + 65019，单据保持 POSTED，账不动。
#[tokio::test]
async fn p16_self_approval_is_forbidden() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = post_new(vec![line(MATERIAL_A, "3", "2", "0")]).await;
    // 库里唯一的超管（种子流程的审批候选人）
    let admin: Uuid = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT u.id FROM sys_user u \
               JOIN sys_user_role ur ON ur.user_id = u.id \
               JOIN sys_role r ON r.id = ur.role_id \
              WHERE r.role_code = 'SUPER_ADMIN' LIMIT 1",
            vec![],
        )
        .await
        .unwrap()
        .expect("库里必须有超管");

    let err = change_stock_in_status(id, &cancel_req(Some("超管自批试试")), Some(admin))
        .await
        .expect_err("审批人 = 发起人 必须被内控拦下");
    assert_eq!(err.status, 422, "{}", err.msg);
    assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
    assert!(
        err.msg.contains("内控") && err.msg.contains("复核人"),
        "报文要说清内控口径与出路：{}",
        err.msg
    );
    // 整笔回滚：单据仍是已过账、没有审批单、账没动
    let detail = detail_of(id).await.unwrap();
    assert_eq!(detail.header.status, STATUS_POSTED);
    assert!(detail.header.approval_instance_id.is_none());
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("3"));
}

/// ⑯ 种子流程自洽：`graph_hash = sha256(canonical_json(快照))`（改了快照不改 hash 就该红），
/// 且审批节点确实是「超管 + 禁自审」。
#[tokio::test]
async fn p16_seed_flow_is_self_consistent() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    use crate::business::flow::graph::{canonical_json, parse_snapshot};
    let row = db()
        .find_with_params(
            "SELECT d.flow_code, d.biz_type, d.status, d.is_default, v.graph_hash, \
                    v.graph_snapshot, v.node_count, v.edge_count \
               FROM biz_flow_def d JOIN biz_flow_def_version v ON v.id = d.current_version_id \
              WHERE d.flow_code = 'stock_in_cancel' AND d.deleted_at IS NULL",
            vec![],
        )
        .await
        .unwrap();
    let row = row.first().expect("V0174 必须种好 stock_in_cancel");
    assert_eq!(
        row.get_typed::<String>("biz_type").unwrap(),
        "STOCK_IN_CANCEL"
    );
    assert_eq!(row.get_typed::<String>("status").unwrap(), "PUBLISHED");
    assert!(row.get_typed::<bool>("is_default").unwrap());
    let stored: String = row.get_typed("graph_hash").unwrap();
    let snapshot: serde_json::Value = row.get_typed("graph_snapshot").unwrap();
    let recomputed = {
        let json = canonical_json(&snapshot);
        // 与 graph::FlowGraph::graph_hash 同一算法（sha256 → 小写 hex）
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(json.as_bytes());
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    assert_eq!(
        stored, recomputed,
        "种子快照与 graph_hash 必须自洽（改了快照不改 hash = 版本漂移）"
    );
    assert_eq!(row.get_typed::<i32>("node_count").unwrap(), 3);
    assert_eq!(row.get_typed::<i32>("edge_count").unwrap(), 2);
    // 快照能被引擎解析（否则发起就 500）
    let snapshot_text = canonical_json(&snapshot);
    let graph = parse_snapshot(&snapshot_text).expect("快照必须可解析");
    let review = graph
        .nodes
        .iter()
        .find(|node| node.node_type == "APPROVAL")
        .expect("必须有审批节点");
    assert_eq!(review.assignee_type.as_deref(), Some("ADMIN"));
    assert_eq!(review.self_approval, "FORBID", "内控：不许自审自批");
}

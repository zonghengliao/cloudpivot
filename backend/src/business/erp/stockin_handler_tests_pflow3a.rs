// ============================================================================
// stockin_handler_tests_pflow3a.rs —— P-FLOW-3A：过账走流程（小额自动/大额审批/驳回/锁定/自审）
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
use crate::business::erp::stockin_bridge;
use crate::business::erp::stockin_model::{self, STATUS_DRAFT};
use crate::shared::error_code;

/// ⑨ 提交过账 → PENDING_POST + 实例 + 六本账一分没动。
#[tokio::test]
async fn pflow3a_si_submit_post_pending_without_ledger() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("5000", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("提交过账失败：{} {}", err.code, err.msg));

    assert_eq!(stock_in_status(id).await, "PENDING_POST");
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert!(row.approval_instance_id.is_some(), "必须回写审批实例");
    let instance = running_post_instance(id).await;
    let tasks = post_pending_tasks(instance).await;
    assert_eq!(tasks.len(), 1, "5000 元走仓管主管审批");
    assert_eq!(tasks[0].1, uid_of(U_WH_MGR));
    // 六本账一分没动
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO, "库存未加");
    let txns: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(txns, 0, "未过账不许写流水");
    let vouchers: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vouchers, 0, "未过账不许出凭证");
}

/// **DC-01 ②**：没配 `stock_in_post` 流程 → **409 + 66033**（改造前是静默直接过账、
/// 三本账 + 凭证 + 应付一起动）。被拒后六本账一分不动、单据仍是 DRAFT。
#[tokio::test]
async fn dc01_post_without_published_flow_is_409() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = stock_in_with_amount("5000", None).await;
    db().execute_with_params(
        "UPDATE biz_flow_def SET deleted_at = now() WHERE flow_code = 'stock_in_post'",
        vec![],
    )
    .await
    .unwrap();
    assert!(
        stockin_model::published_post_flow()
            .await
            .unwrap()
            .is_none(),
        "流程下线后必须查不到"
    );
    let error = submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .expect_err("没配流程必须 409，不能静默直接过账");
    // 恢复流程（`now()` 软删的那一行，用 `cleanup_fixtures` 的同一口径）
    db().execute_with_params(
        "UPDATE biz_flow_def SET deleted_at = NULL WHERE flow_code = 'stock_in_post'",
        vec![],
    )
    .await
    .unwrap();
    assert!(
        stockin_model::published_post_flow()
            .await
            .unwrap()
            .is_some(),
        "夹具自检：流程必须已还原"
    );

    assert_eq!(error.status, 409, "{}", error.msg);
    assert_eq!(
        error.code,
        error_code::APPROVAL_FLOW_NOT_CONFIGURED,
        "必须用 DC-01 的新码 66033：{}",
        error.msg
    );
    assert!(
        error.msg.contains("stock_in_post"),
        "报文必须点名缺哪个 flow_code：{}",
        error.msg
    );
    // 单据 / 未记账痕迹
    assert_eq!(stock_in_status(id).await, STATUS_DRAFT, "被拒后仍应是草稿");
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        Decimal::ZERO,
        "被拒后库存不许动"
    );
    let txns: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(txns, 0, "被拒后不许写库存流水");
    let vouchers: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vouchers, 0, "被拒后不许出凭证");
}

/// ⑩ 500 元（< 1000）→ 免审直接过账 + 通知仓管主管（NOTIFY_NODE）。
#[tokio::test]
async fn pflow3a_si_small_amount_auto_posts_and_notifies() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("500", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("提交过账失败：{} {}", err.code, err.msg));

    // 免审：同一请求里就走完了过账
    assert_eq!(stock_in_status(id).await, "POSTED", "小额免审必须直接过账");
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("1"), "库存已加");
    // 实例已结束（NOTIFY 节点不阻塞）
    let instance: Uuid = db()
        .scalar_with_params::<Option<Uuid>>(
            "SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_IN_POST' \
              AND biz_id = ?::uuid ORDER BY started_at DESC LIMIT 1",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .expect("实例应当存在");
    let status: String = db()
        .scalar_with_params::<Option<String>>(
            "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, "APPROVED", "免审分支实例应自动走完");
    // 知会通知（sys_notice.biz_type = FLOW_NOTIFY，收件人 = 仓管主管）
    let notices: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM sys_notice WHERE biz_id = ? AND biz_type = 'FLOW_NOTIFY'",
            vec![Value::Text(instance.to_string())],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notices, 1, "免审必须发一条知会通知");
    let receivers: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM sys_notice_receiver r JOIN sys_notice n ON n.id = r.notice_id \
              WHERE n.biz_id = ? AND n.biz_type = 'FLOW_NOTIFY'",
            vec![Value::Text(instance.to_string())],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receivers, 1, "通知要扇出给仓管主管（1 人）");
}

/// ⑪ 5000 元 → 走仓管主管审批（待办派给角色成员）。
#[tokio::test]
async fn pflow3a_si_5000_needs_manager_approval() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("5000", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap();
    let instance = running_post_instance(id).await;
    let tasks = post_pending_tasks(instance).await;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].1, uid_of(U_WH_MGR), "5000 元派给仓管主管");
    assert_eq!(stock_in_status(id).await, "PENDING_POST");
}

/// ⑫ 审批通过 → POSTED + 六本账全动（库存 / 流水 / 凭证 / 应付 / 库位账）。
#[tokio::test]
async fn pflow3a_si_approve_posts_all_books() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("5000", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap();
    let tasks = post_pending_tasks(running_post_instance(id).await).await;
    post_approve_as(tasks[0].0, U_WH_MGR).await;

    assert_eq!(stock_in_status(id).await, "POSTED");
    let (qty, amount, _) = inventory_of(MATERIAL_A).await;
    assert_eq!(qty, dec("1"), "库存数量");
    assert_eq!(amount, dec("5000"), "库存成本（不含税）");
    let txns: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(txns, 1, "库存流水 1 条");
    let vouchers: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vouchers, 1, "过账即出凭证");
    let payables: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_payable WHERE source_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(payables, 1, "过账即生成应付单");
    let loc_qty: Decimal = db()
        .scalar_with_params::<Option<Decimal>>(
            "SELECT COALESCE(SUM(qty), 0) FROM biz_inventory_location WHERE material_id = ?::uuid",
            vec![Value::from(&uid_of(MATERIAL_A))],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loc_qty, dec("1"), "库位账与成本账相等");
}

/// ⑬ 审批驳回 → 回 DRAFT + 账一分没动；打回后还能重新提交。
#[tokio::test]
async fn pflow3a_si_reject_back_to_draft_without_ledger() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("5000", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap();
    let tasks = post_pending_tasks(running_post_instance(id).await).await;
    let op = uid_of(U_WH_MGR);
    let out = db()
        .transaction(|tx| async move {
            crate::business::flow::engine::reject(
                &tx,
                tasks[0].0,
                op,
                "数量与送货单不符，退回核对",
                crate::business::flow::engine::ReturnTo::Final,
            )
            .await
        })
        .await
        .unwrap();
    stockin_bridge::on_post_settled(out.instance_id, &out.instance_status).await;

    assert_eq!(stock_in_status(id).await, "DRAFT", "驳回必须打回草稿");
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO, "账没动");
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert!(row.approval_instance_id.is_none(), "打回要清空审批实例");
    // 打回后能重新提交
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .expect("打回后必须能重新提交过账审批");
    assert_eq!(stock_in_status(id).await, "PENDING_POST");
}

/// ⑭ PENDING_POST 状态改金额 / 数量 → 409（锁单据）。
#[tokio::test]
async fn pflow3a_si_pending_post_is_locked() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("5000", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap();
    assert_eq!(stock_in_status(id).await, "PENDING_POST");

    let err = update_stock_in(id, &req(vec![line(MATERIAL_A, "9", "5000", "0")]), None)
        .await
        .expect_err("审批中的单据不许改金额 / 明细");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(
        err.msg.contains("PENDING_POST") || err.msg.contains("不可修改"),
        "{}",
        err.msg
    );
    // 状态流转也不能「直接过账」绕过审批
    let err = change_stock_in_status(id, &post_req(), None)
        .await
        .expect_err("审批中不许直接过账");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("过账审批中"), "{}", err.msg);
    // 单据没被改动
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert_eq!(row.total_with_tax, dec("5000"));
    assert_eq!(row.status, "PENDING_POST");
}

/// ⑮ 审批中删除收货单 → 同一事务撤审批（不留孤儿待办，P11）。
#[tokio::test]
async fn pflow3a_si_delete_cancels_pending_approval() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let id = stock_in_with_amount("5000", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap();
    let instance = running_post_instance(id).await;

    delete_stock_in(id, None)
        .await
        .unwrap_or_else(|err| panic!("删除失败：{} {}", err.code, err.msg));

    let status: String = db()
        .scalar_with_params::<Option<String>>(
            "SELECT status FROM biz_flow_instance WHERE id = ?::uuid",
            vec![Value::from(&instance)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, "CANCELLED", "删单必须终止审批实例");
    let pending: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_flow_task WHERE instance_id = ?::uuid AND status = 'PENDING'",
            vec![Value::from(&instance)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending, 0, "不许留孤儿待办");
    assert!(
        stockin_model::find_stock_in(id).await.unwrap().is_none(),
        "单据已软删"
    );
}

/// ⑯ 制单人自批 → 403 + 65019。
///
/// P0-FIX 之后语义是「**生成待办时就排除**」：制单人（也是仓管主管）拿到的是
/// `SKIPPED` 留痕行，不是 `PENDING`；403 兜底仍在（防绕过）。
#[tokio::test]
async fn pflow3a_si_maker_cannot_approve_own_post() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    // 制单人 = 仓管主管本人（自己提自己批）
    sql(&format!(
        "INSERT INTO sys_user_role (user_id, role_id) \
         VALUES ('{U_KEEPER}'::uuid, '{ROLE_WH_MGR}'::uuid) ON CONFLICT DO NOTHING"
    ))
    .await;
    let id = stock_in_with_amount("5000", Some(U_KEEPER)).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap();
    let instance = running_post_instance(id).await;
    let (mine, status, comment) = post_task_of(instance, uid_of(U_KEEPER))
        .await
        .expect("制单人必须留痕（不删待办，只改状态）");
    assert_eq!(
        status, "SKIPPED",
        "P0-FIX：制单人的票在生成时就被内控排除，不进 PENDING"
    );
    assert!(comment.contains("内控"), "留痕要写原因：{comment}");
    assert!(
        post_pending_tasks(instance)
            .await
            .iter()
            .all(|(_, who)| *who != uid_of(U_KEEPER)),
        "制单人不该出现在「待批」清单里（他点不动）"
    );

    let err = stockin_bridge::guard_self_approval(mine, uid_of(U_KEEPER))
        .await
        .expect_err("自批必须被拦");
    assert_eq!(err.status, 403, "{}", err.msg);
    assert_eq!(err.code, error_code::FLOW_SELF_APPROVAL_FORBIDDEN);
    assert!(
        err.msg.contains("不能审批自己提交的过账申请"),
        "{}",
        err.msg
    );
    // 他人可正常审批
    stockin_bridge::guard_self_approval(mine, uid_of(U_WH_MGR))
        .await
        .expect("他人可以正常审批");
    sql(&format!(
        "DELETE FROM sys_user_role WHERE user_id = '{U_KEEPER}'::uuid \
          AND role_id = '{ROLE_WH_MGR}'::uuid"
    ))
    .await;
}

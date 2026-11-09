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
        stockin_model::published_post_flow().await.unwrap().is_none(),
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
        stockin_model::published_post_flow().await.unwrap().is_some(),
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
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO, "被拒后库存不许动");
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
    assert!(stockin_model::find_stock_in(id).await.unwrap().is_none(), "单据已软删");
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
    assert!(err.msg.contains("不能审批自己提交的过账申请"), "{}", err.msg);
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

// ============================================================================
// BUG-01（t_f427e5d6）：免审过账失败必须打回草稿
//
// 生产事故 IN-20261031-000002：免审分支里 `post_after_approval` 失败时只有
// `ERP_SEPARATION_OF_DUTIES` 有兜底，其它错误直接 `return Err` —— 事务已提交
// （单据 PENDING_POST + 实例 APPROVED），没有待办、没有人工过账接口 ⇒ 死单。
// 下面三条钉住「任何失败都打回 DRAFT + 原因写进状态日志 + 正常免审仍直过」。
// ============================================================================

/// 某单最近一条状态日志：`(from_status, to_status, reason)`。
async fn last_status_log(id: Uuid) -> (String, String, String) {
    let rows = db()
        .find_with_params(
            "SELECT from_status, to_status, COALESCE(reason, '') AS reason \
               FROM biz_status_log WHERE entity_type = 'STOCK_IN' AND entity_id = ?::uuid \
              ORDER BY created_at DESC, id DESC LIMIT 1",
            vec![Value::from(&id)],
        )
        .await
        .expect("读状态日志失败");
    let row = rows.first().expect("必须留下状态日志");
    (
        row.get_typed("from_status").unwrap_or_default(),
        row.get_typed("to_status").unwrap_or_default(),
        row.get_typed("reason").unwrap_or_default(),
    )
}

/// ① 来源采购单已被**另一张**入库单收满 → 免审过账失败 → 409 + 打回 DRAFT（不是死单）。
#[tokio::test]
async fn bug01_auto_post_over_receive_reverts_to_draft() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    // 采购单 100 件；两张入库单在「都还没过账」时双开，各按 100 件待收建单
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "100", "SUBMITTED").await;
    let first = create_stock_in(&a4_sourced_req(order_id, item_id, "100", None), None)
        .await
        .expect("建单 A");
    let second = create_stock_in(&a4_sourced_req(order_id, item_id, "100", None), None)
        .await
        .expect("建单 B");

    // A 先过账（100×2 = 200 元 < 1000 → 免审直过）
    submit_post(first, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("A 应免审直过：{} {}", err.code, err.msg));
    assert_eq!(stock_in_status(first).await, "POSTED");
    assert_eq!(a4_po_status(order_id).await, "RECEIVED", "A 过账后采购单收满");

    // B 再提交 → 来源采购单已收满/已收货 → 必须失败，且**打回草稿**
    let err = submit_post(second, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .expect_err("收满后另一张入库单不许过账");
    assert_eq!(err.status, 409, "{}", err.msg);

    assert_eq!(
        stock_in_status(second).await,
        "DRAFT",
        "免审过账失败必须打回 DRAFT —— 停在 PENDING_POST 就是死单（BUG-01）"
    );
    let row = stockin_model::find_stock_in(second).await.unwrap().unwrap();
    assert!(
        row.approval_instance_id.is_none(),
        "打回草稿必须清空审批实例（否则再也提交不了）"
    );
    // 状态日志：PENDING_POST → DRAFT，原因带原始错误
    let (from, to, reason) = last_status_log(second).await;
    assert_eq!((from.as_str(), to.as_str()), ("PENDING_POST", "DRAFT"));
    assert!(reason.contains("免审过账失败"), "原因要能看懂：{reason}");
    assert!(
        reason.contains(&err.msg),
        "原因要带原始错误（{reason} 不含 {}）",
        err.msg
    );
    // 六本账只有 A 的 100 件，B 一分没动
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("100"), "B 不许加库存");
    // 打回后可以修单重提（界面能继续操作）
    let again = submit_post(second, &post_req(), Some(uid_of(U_KEEPER)), &none_gate()).await;
    assert!(
        again.is_err(),
        "额度仍是 0，重提仍会被拦 —— 但必须是「可重试」的普通错误"
    );
    assert_eq!(stock_in_status(second).await, "DRAFT", "重试失败也要回到 DRAFT");
}

/// ② 供应商与来源采购单不符（绕过接口改库）→ 免审过账失败 → 同样的打回。
#[tokio::test]
async fn bug01_auto_post_supplier_mismatch_reverts_to_draft() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
        .await
        .expect("建单");
    // 模拟「绕过接口直接改库」把单据头供应商换掉（金额 4 元 → 免审）
    db().execute_with_params(
        "UPDATE biz_stock_in SET supplier_id = ?::uuid WHERE id = ?::uuid",
        vec![Value::from(&Uuid::parse_str(SUPPLIER).unwrap()), Value::from(&id)],
    )
    .await
    .expect("模拟改库");

    let err = submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .expect_err("供应商不符必须拦下");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SI_SUPPLIER_MISMATCH);

    assert_eq!(
        stock_in_status(id).await,
        "DRAFT",
        "免审过账失败必须打回 DRAFT（不只是 SoD 才兜底）"
    );
    let row = stockin_model::find_stock_in(id).await.unwrap().unwrap();
    assert!(row.approval_instance_id.is_none());
    let (from, to, reason) = last_status_log(id).await;
    assert_eq!((from.as_str(), to.as_str()), ("PENDING_POST", "DRAFT"));
    assert!(reason.contains("免审过账失败"), "{reason}");
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO, "被拦下后库存一分没动");
}

/// ③ 回归：一切正常的小额单 → 仍然免审直过（打回逻辑不许把正常路径也打回）。
#[tokio::test]
async fn bug01_auto_post_success_still_posts() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "10", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "2", None), None)
        .await
        .expect("建单");
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("正常免审必须直过：{} {}", err.code, err.msg));
    assert_eq!(stock_in_status(id).await, "POSTED");
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("2"), "库存已加");
    assert_eq!(a4_po_status(order_id).await, "PARTIAL_RECEIVED", "采购单推进");
}

// ============================================================================
// BUG-02（t_1bf6c58d）：重新过账 —— 「审批已通过、过账失败」死单的唯一出路
//
// 生产事故 IN-20261031-000002：审批实例 APPROVED、单据停在 PENDING_POST、六本账没动。
// `PATCH /status` 明确拒绝 PENDING_POST → POSTED（防绕过审批）、审批通过回调已经跑过
// 一次不会再回来 —— 没有 repost 就只能删单重建（单号与历史全丢）。
//
// 下面钉死：正常重试能落六本账 / 数据有问题给明确错误且**不改状态** / 修好数据能再试 /
// DRAFT 与 POSTED 一律 400 / 审批还在跑不能重试 / 内控开关打开时制单人被拦。
// ============================================================================

/// 把审批实例推到 `APPROVED`，但**不跑**审批通过回调。
///
/// 这正是 BUG-01 之前的存量死单现场：回调那次过账失败后单据留在 `PENDING_POST`，
/// 而实例已经是 `APPROVED`、待办全空 —— 没有任何人会再来推进它。
async fn approve_instance_without_callback(stock_in_id: Uuid) -> Uuid {
    let instance = running_post_instance(stock_in_id).await;
    let tasks = post_pending_tasks(instance).await;
    assert_eq!(tasks.len(), 1, "夹具：应恰好一条审批待办");
    let task_id = tasks[0].0;
    let out = db()
        .transaction(|tx| async move {
            crate::business::flow::engine::approve(&tx, task_id, uid_of(U_WH_MGR), Some("同意")).await
        })
        .await
        .unwrap_or_else(|err| panic!("审批推进失败：{err}"));
    assert!(
        out.instance_status.eq_ignore_ascii_case("APPROVED"),
        "夹具：实例应 APPROVED，实际 {}",
        out.instance_status
    );
    instance
}

/// 打开 / 关闭「制单 ≠ 过账」开关（写库 + 立刻重载进程内快照）。
async fn set_sod_post(enabled: bool) {
    let value = if enabled { "true" } else { "false" };
    sql(&format!(
        "UPDATE sys_config SET config_value = '{value}', updated_at = now() \
          WHERE config_key = 'erp.separation_of_duties.post'"
    ))
    .await;
    crate::support::config::model::reload_runtime_snapshot()
        .await
        .expect("重载参数快照失败");
}

/// ① 正常重试：PENDING_POST + 实例 APPROVED → repost → POSTED + 六本账落库。
#[tokio::test]
async fn bug02_repost_pending_approved_posts_and_moves_books() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    // 600 × 2 = 1200 元 ≥ 1000 → 走仓管主管审批（不是小额免审，才能造出死单现场）
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "600", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "600", None), None)
        .await
        .expect("建单");
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("提交过账审批失败：{} {}", err.code, err.msg));
    assert_eq!(stock_in_status(id).await, "PENDING_POST");
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO, "送审不动账");

    approve_instance_without_callback(id).await;
    // 死单现场：单据仍是 PENDING_POST、实例 APPROVED、六本账一两没动
    assert_eq!(stock_in_status(id).await, "PENDING_POST");
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO);

    // 详情必须告诉前端「审批已通过 + 可以重试」，否则按钮永远不出现
    let detail = detail_of(id).await.expect("读详情");
    assert_eq!(
        detail.approval_instance_status.as_deref(),
        Some("APPROVED"),
        "详情要带审批实例状态"
    );
    assert!(
        detail.repost_available,
        "PENDING_POST + 实例 APPROVED 必须 repost_available=true"
    );

    repost_stock_in(id, Some(uid_of(U_WH_MGR)))
        .await
        .unwrap_or_else(|err| panic!("重新过账应成功：{} {}", err.code, err.msg));

    assert_eq!(stock_in_status(id).await, "POSTED", "过账成功");
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("600"), "库存已加");
    let txns: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_inventory_txn WHERE doc_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(txns, 1, "库存流水已写");
    let vouchers: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_voucher WHERE source_id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(vouchers, 1, "凭证已生成");
    let payables: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM biz_payable WHERE source_id = ?::uuid AND source_type = 'STOCK_IN'",
            vec![Value::from(&id)],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(payables, 1, "应付单已生成");
    assert_eq!(a4_po_status(order_id).await, "RECEIVED", "采购单推进");

    // 审计留痕：PENDING_POST → POSTED，原因写清是「审批通过后重新过账」
    let (from, to, reason) = last_status_log(id).await;
    assert_eq!((from.as_str(), to.as_str()), ("PENDING_POST", "POSTED"));
    assert!(reason.contains("重新过账"), "状态日志要能看出这是一次重试：{reason}");

    // 过账后详情不再提示可重试
    assert!(!detail_of(id).await.unwrap().repost_available);
}

/// ② 数据有问题（采购单已收满）→ 明确错误 + 状态仍 PENDING_POST；修好数据后再点就成功。
#[tokio::test]
async fn bug02_repost_over_receive_keeps_pending_then_recovers_after_fix() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "600", "SUBMITTED").await;
    // 两张单在采购单还没收货时双开，各按 600 件待收建单（与 BUG-01 同一夹具口径）
    let first = create_stock_in(&a4_sourced_req(order_id, item_id, "600", None), None)
        .await
        .expect("建单 A");
    let second = create_stock_in(&a4_sourced_req(order_id, item_id, "600", None), None)
        .await
        .expect("建单 B");

    // A 走完审批 → 回调过账成功 → 采购单收满
    submit_post(first, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("A 提交失败：{} {}", err.code, err.msg));
    let instance = running_post_instance(first).await;
    let tasks = post_pending_tasks(instance).await;
    post_approve_as(tasks[0].0, U_WH_MGR).await;
    assert_eq!(stock_in_status(first).await, "POSTED");
    assert_eq!(a4_po_status(order_id).await, "RECEIVED", "A 收满");

    // B：审批通过但回调那次过账失败 = 存量死单
    submit_post(second, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("B 提交失败：{} {}", err.code, err.msg));
    approve_instance_without_callback(second).await;
    assert_eq!(stock_in_status(second).await, "PENDING_POST");

    // 重新过账：采购单已收满 → 409 + 明确文案，状态不动、账不动
    let err = repost_stock_in(second, Some(uid_of(U_WH_MGR)))
        .await
        .expect_err("采购单已收满必须拦下");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert!(err.msg.contains("不能过账"), "文案要说清为什么：{}", err.msg);
    assert!(
        err.msg.contains("已收货") || err.msg.contains("RECEIVED"),
        "文案要点名采购单现状（已收货 / 已收满）：{}",
        err.msg
    );
    assert_eq!(
        stock_in_status(second).await,
        "PENDING_POST",
        "失败不许改状态（否则又要重新走审批）"
    );
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        dec("600"),
        "只有 A 的 600 件，B 一分没动"
    );

    // 「修数据」：采购单退回后重新收货 —— 待收额度加到 1200，状态回部分收货
    sql(&format!(
        "UPDATE biz_purchase_order_item SET qty = 1200 WHERE id = '{item_id}'::uuid"
    ))
    .await;
    sql(&format!(
        "UPDATE biz_purchase_order SET total_qty = 1200, status = 'PARTIAL_RECEIVED' \
          WHERE id = '{order_id}'::uuid"
    ))
    .await;

    // 再点一次「重新过账」→ 这次过账成功（不需要删单重建，单号与历史都保住）
    repost_stock_in(second, Some(uid_of(U_WH_MGR)))
        .await
        .unwrap_or_else(|err| panic!("修好数据后重试应成功：{} {}", err.code, err.msg));
    assert_eq!(stock_in_status(second).await, "POSTED");
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("1200"), "A 600 + B 600");
    assert_eq!(a4_po_status(order_id).await, "RECEIVED", "再次收满");
}

/// ③ DRAFT 调 repost → 400（草稿请走「过账」）。
#[tokio::test]
async fn bug02_repost_rejects_draft() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    let id = stock_in_with_amount("500", None).await;
    let err = repost_stock_in(id, Some(uid_of(U_KEEPER)))
        .await
        .expect_err("草稿不能重新过账");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("草稿"), "{}", err.msg);
    assert_eq!(stock_in_status(id).await, "DRAFT", "状态不许被改");
    assert!(
        !detail_of(id).await.unwrap().repost_available,
        "草稿详情不许给「重新过账」按钮"
    );
}

/// ④ POSTED 调 repost → 400（已过账不能重复过账，库存只加一次）。
#[tokio::test]
async fn bug02_repost_rejects_posted() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    // 500 元 < 1000 → 免审直过
    let id = stock_in_with_amount("500", None).await;
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("免审过账失败：{} {}", err.code, err.msg));
    assert_eq!(stock_in_status(id).await, "POSTED");

    let err = repost_stock_in(id, Some(uid_of(U_KEEPER)))
        .await
        .expect_err("已过账不能重新过账");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("已过账"), "{}", err.msg);
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        dec("1"),
        "库存只加一次（不许重复过账）"
    );
    assert!(!detail_of(id).await.unwrap().repost_available);
}

/// ⑤ 审批还在跑（实例 PENDING）→ 400：重试等于绕过复核。
#[tokio::test]
async fn bug02_repost_rejects_instance_still_pending() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "600", "SUBMITTED").await;
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "600", None), None)
        .await
        .expect("建单");
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("提交失败：{} {}", err.code, err.msg));
    assert_eq!(stock_in_status(id).await, "PENDING_POST");

    let err = repost_stock_in(id, Some(uid_of(U_WH_MGR)))
        .await
        .expect_err("审批没通过不能重新过账");
    assert_eq!(err.status, 400, "{}", err.msg);
    assert!(err.msg.contains("还没通过"), "{}", err.msg);
    assert_eq!(stock_in_status(id).await, "PENDING_POST");
    assert_eq!(
        inventory_of(MATERIAL_A).await.0,
        Decimal::ZERO,
        "绕过复核的尝试不许动账"
    );
    assert!(
        !detail_of(id).await.unwrap().repost_available,
        "审批中详情不许给重试按钮"
    );
}

/// ⑥ 内控开关「制单 ≠ 过账」打开时：制单人调 repost 被拦（409 + 66013），换个人可以。
#[tokio::test]
async fn bug02_repost_blocked_by_separation_of_duties() {
    let _db = match setup().await {
        Some(guard) => guard,
        None => return,
    };
    setup_post_approval().await;
    let (order_id, item_id) = a4_seed_po(A4_SUPPLIER_B, "600", "SUBMITTED").await;
    // 制单人 = U_KEEPER（SoD 才有「自己」可比）
    let id = create_stock_in(&a4_sourced_req(order_id, item_id, "600", None), Some(uid_of(U_KEEPER)))
        .await
        .expect("建单");
    submit_post(id, &post_req(), Some(uid_of(U_KEEPER)), &none_gate())
        .await
        .unwrap_or_else(|err| panic!("提交失败：{} {}", err.code, err.msg));
    approve_instance_without_callback(id).await;
    assert_eq!(stock_in_status(id).await, "PENDING_POST");

    // 打开开关 → 制单人重试必须被拦
    set_sod_post(true).await;
    let blocked = repost_stock_in(id, Some(uid_of(U_KEEPER))).await;
    // 立刻复位开关，别污染其它用例（哪怕断言失败也要先复位）
    set_sod_post(false).await;

    let err = blocked.expect_err("内控开关打开时制单人不能自己过账");
    assert_eq!(err.status, 409, "{}", err.msg);
    assert_eq!(err.code, error_code::ERP_SEPARATION_OF_DUTIES, "{}", err.msg);
    assert!(err.msg.contains("制单与过账分离"), "{}", err.msg);
    assert_eq!(stock_in_status(id).await, "PENDING_POST", "被拦不改状态");
    assert_eq!(inventory_of(MATERIAL_A).await.0, Decimal::ZERO);

    // 换一个有权限的账号 → 同一张单能正常重试（内控只拦「同一个人」）
    repost_stock_in(id, Some(uid_of(U_WH_MGR)))
        .await
        .unwrap_or_else(|err| panic!("他人重试应成功：{} {}", err.code, err.msg));
    assert_eq!(stock_in_status(id).await, "POSTED");
    assert_eq!(inventory_of(MATERIAL_A).await.0, dec("600"));
}

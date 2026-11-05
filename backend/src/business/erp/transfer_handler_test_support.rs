// ============================================================================
// transfer_handler_test_support.rs —— 测试支撑：走生产审批路径把审批实例批到终态（`#[cfg(test)]`，两套真库用例共用）
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::uuid::Uuid;

/// 让一张调拨单的审批单**真的落终态**（引擎在事务里投票 → 提交 → 回调调拨单）。
///
/// P5 起 `PENDING → APPROVED` 有人工门闸（`65030`），夹具不能再靠「人工直推」跨越审批，
/// 只能走生产路径。两套真库用例（`db_tests` / `w3c_tests`）共用这一个实现，
/// 免得两边对「怎么样才算审批通过」各写一套。
///
/// ⚠️ `stock_transfer` 的审批人来源是 `ADMIN`（运行时展开**全部超管**）：库里超管有几个
/// 就有几条待办，只批第一条会让实例停在 `PENDING`、单据永远进不了 `APPROVED`。
/// 所以这里**批到终态**并断言实例真的 `APPROVED`。
#[cfg(test)]
pub(super) async fn approve_flow_instance(instance_id: Uuid) {
    let outs =
        crate::business::flow::test_support::approve_all_pending(instance_id, "单测：同意").await;
    assert!(
        !outs.is_empty(),
        "审批通过前必须有 PENDING 待办（实例 {instance_id}）"
    );
    for out in outs {
        // 与 `flow::handler::approve_task` 同序：事务已提交，再跑业务回调
        crate::business::flow::task_bridge::on_transfer_settled(
            out.instance_id,
            &out.instance_status,
        )
        .await;
    }
    crate::business::flow::test_support::assert_instance_status(instance_id, "APPROVED").await;
}

// ==================== 单测：真库行为（卡面 §五 的 ①–⑰） ====================
//
// 为什么必须真库：本卡交付的是「6 态状态机真的能落库」「非 DRAFT 真的 409」
// 「提交审批真的回写 approval_instance_id」——纯逻辑单测证明不了
// 「`ck_biz_transfer_state` 的 5 个分支与 6 条边一一对上」这类必然 500 的错误。
// 所以这里走**生产同款路径**：
//
// ```text
// 夹具：仓 / 物料 / 序列号 / 在途仓（V0139 种子）
//   → transfer_handler::create_transfer / update_transfer / change_transfer_status / delete_transfer
//   → transfer_model 的 SQL 直接打到 cloudpivot_w3b（隔离库）
// ```
//
// 用法：
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_w3b'
// cargo test --lib business::erp::transfer_handler -- --nocapture --test-threads=1
// ```
// 缺 `ERP_DB_URL` 时打印 `SKIP` 并直接返回（CI 无库时 `cargo test --lib` 仍然全绿）。

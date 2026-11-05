// ============================================================================
// transfer_handler_tests.rs —— 单测（不连库）：权限码导出与审批冲突 409 文案
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::*;
use crate::business::erp::transfer_dto::TRANSFER_PERMISSIONS;
use crate::business::flow::model as flow_model;
use crate::shared::error_code;
use nik::prelude::*;

/// 与 V0144 逐字一致的 6 个权限码（重复导出一次，单测与脚本都能拿到）。
const _: [(&str, &str); 6] = TRANSFER_PERMISSIONS;

#[test]
fn permission_reexport_is_same_list() {
    assert_eq!(TRANSFER_PERMISSIONS_PUB.len(), 6);
    assert_eq!(TRANSFER_PERMISSIONS_PUB, TRANSFER_PERMISSIONS);
}

/// 审批实例唯一索引冲突 → **409 + 单号 + 「已有审批中实例」**，绝不能是 500。
///
/// 两个分支都要钉住：
/// - 驱动给了约束名（`DbError::UniqueViolation`）→ 按变体判；
/// - 驱动没给约束名（错误退化成 `DbError::Query`，`Display` 里只剩 PG 原文）→ 文案兜底。
///   这条分支正是卡面「500 服务器内部错误」的来源：只按变体判会把它漏到 `utils::db_error`。
#[test]
fn biz_active_conflict_maps_to_409_with_transfer_no() {
    // 每个用例各造一份（`DbError` 不实现 Clone，底层挂着驱动的 Box<dyn Error>）
    let variant = || DbError::UniqueViolation {
        column: None,
        constraint: Some(flow_model::BIZ_ACTIVE_INDEX.to_string()),
        source: None,
    };
    let by_text = || {
        DbError::InvalidValue(format!(
            "db error: ERROR: duplicate key value violates unique constraint \"{}\"",
            flow_model::BIZ_ACTIVE_INDEX
        ))
    };
    assert!(
        is_biz_active_duplicate(&variant()),
        "带约束名的 23505 必须被认出来"
    );
    assert!(
        is_biz_active_duplicate(&by_text()),
        "文案兜底必须生效（否则真库里就会变成 500）"
    );

    // 别的唯一索引不能被误判成审批冲突
    let other = DbError::UniqueViolation {
        column: None,
        constraint: Some("uk_biz_stock_transfer_no".to_string()),
        source: None,
    };
    assert!(!is_biz_active_duplicate(&other));

    // 通用出口（write_error / 拿不到单号）也不许再变成 500
    let generic = write_error(variant(), "调拨单状态流转");
    assert_eq!(generic.status, 409, "通用出口也必须是 409");
    assert!(generic.msg.contains("审批中实例"), "{}", generic.msg);

    // 拿得到单号时文案必须带单号（用户要能对上自己那一单）
    let with_no = write_error(by_text(), "调拨单状态流转");
    assert_eq!(with_no.status, 409);
    let api = approval_in_progress(Some("TK-20261102-000007"));
    assert_eq!(api.status, 409);
    assert_eq!(api.code, error_code::RESOURCE_CONFLICT);
    assert!(
        api.msg.contains("TK-20261102-000007"),
        "409 文案必须带单号：{}",
        api.msg
    );
    assert!(
        api.msg.contains("已有审批中实例"),
        "409 文案必须说清原因：{}",
        api.msg
    );
}

// ============================================================================
// flow_model_guard.rs —— 校验与错误出口：业务错误构造（not_found / version_not_found / approval_in_progress）与唯一键冲突识别（biz_active / pending / node）
//
// 由 `model.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use crate::shared::error_code;
use nik::prelude::*;

/// 流程错误码转 [`ApiError`]（写路径复用的 404 文案）。
pub fn not_found() -> ApiError {
    ApiError::new(404, error_code::FLOW_NOT_FOUND, "流程不存在或已删除")
}

/// 版本不存在 → 404。
pub fn version_not_found() -> ApiError {
    ApiError::new(404, error_code::FLOW_NOT_FOUND, "流程版本不存在")
}

// ==================== 审批实例唯一索引冲突（跨模块统一口径） ====================

/// 「同一业务单据同时只能有一个在跑的审批单」的部分唯一索引（V0122）。
pub const BIZ_ACTIVE_INDEX: &str = "uk_biz_flow_instance_biz_active";

/// 是不是「已有审批中实例」的唯一索引冲突（**变体优先 + 文案兜底**）。
///
/// # 为什么不能只按变体判
///
/// 正常路径上 PG 的 `23505` 会被驱动方言翻成 [`DbError::UniqueViolation`] 并带上约束名；
/// 但事务内执行、或驱动拿不到 `SqlState` 时错误会落到 [`DbError::Query`]
/// （`Display` 形如 `query failed: <sql>: db error: ...`），变体判据就失效了 ——
/// 只按变体判的后果是「明明是可解释的冲突，用户却看到 500 服务器内部错误」，
/// 或者落到「单号重复、请联系运维排查 biz_seq」这种把方向指错的文案上。
///
/// # 谁该用
///
/// 所有「业务单据 → 发起/重投审批单」的模块（`erp::transfer_handler` /
/// `stockin_handler` / `purchase_handler` / `payable_payment` / `voucher_write`）。
/// 判据只有这一份实现，文案只有 [`approval_in_progress`] 一份实现，改口径改这里。
pub fn is_biz_active_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains(BIZ_ACTIVE_INDEX))
            .unwrap_or(false),
        other => other.to_string().contains(BIZ_ACTIVE_INDEX),
    }
}

/// 「<单据> 已有审批中实例」→ `409 + RESOURCE_CONFLICT`（可执行提示，不是 500，也不是取号器问题）。
///
/// * `biz_label`：单据名词（`调拨单` / `收货入库单` / `采购订单` / `应付单` / `凭证`…），
///   文案里会拼成「单据 单号」；
/// * `biz_no`：业务单号。拿不到时（例如从通用写错误出口兜底）退化成「该<单据>」，
///   仍然给用户可执行的下一步（刷新看进度 / 审批中心撤回）。
pub fn approval_in_progress(biz_label: &str, biz_no: Option<&str>) -> ApiError {
    let who = match biz_no.map(str::trim).filter(|no| !no.is_empty()) {
        Some(no) => format!("{biz_label} {no}"),
        None => format!("该{biz_label}"),
    };
    ApiError::new(
        409,
        error_code::RESOURCE_CONFLICT,
        format!(
            "{who} 已有审批中实例（同一单据同时只能有一个在跑的审批单）：\
             请刷新查看审批进度，或在审批中心撤回后再重新提交"
        ),
    )
}

/// 是否「同一节点同一人已有 PENDING 待办」的唯一约束冲突（三层去重第 2 层）。
pub fn is_pending_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_flow_task_pending"))
            .unwrap_or(false),
        other => other.to_string().contains("uk_biz_flow_task_pending"),
    }
}

/// 是否「同一实例同一节点同一轮次」的唯一约束冲突（幂等激活）。
pub fn is_node_duplicate(error: &DbError) -> bool {
    match error {
        DbError::UniqueViolation { constraint, .. } => constraint
            .as_deref()
            .map(|name| name.contains("uk_biz_flow_instance_node"))
            .unwrap_or(false),
        other => other.to_string().contains("uk_biz_flow_instance_node"),
    }
}

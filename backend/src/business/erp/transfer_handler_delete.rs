// ============================================================================
// transfer_handler_delete.rs —— 软删：仅 DRAFT / CANCELLED 可删（与作废 CANCELLED 区分：本部件只做删除标记）
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::transfer_handler_common::*;
use crate::business::erp::transfer_model::{self};
use crate::utils;
use nik::prelude::chrono::Utc;
use nik::prelude::*;
use nik::uuid::Uuid;

/// 删除核心逻辑：**只有 `DRAFT` / `CANCELLED`** 能删（软删）。
pub async fn delete_transfer(
    id: Uuid,
    operator: Option<Uuid>,
) -> std::result::Result<(), ApiError> {
    let row = transfer_model::find_by_id(id)
        .await
        .map_err(|error| utils::db_error(error, "查询调拨单"))?
        .ok_or_else(|| not_found("调拨单"))?;
    if !matches!(
        row.status.as_str(),
        transfer_model::STATUS_DRAFT | transfer_model::STATUS_CANCELLED
    ) {
        return Err(conflict(format!(
            "调拨单当前是「{}」（{}），只有草稿（DRAFT）/ 已取消（CANCELLED）能删：\
             在途 / 已完成的单据是库存与审批的事实，不能删",
            transfer_model::status_label(&row.status),
            row.status
        )));
    }
    let affected = transfer_model::soft_delete(db(), id, operator, Utc::now())
        .await
        .map_err(|error| utils::db_error(error, "软删调拨单"))?;
    if affected == 0 {
        return Err(not_found("调拨单"));
    }
    Ok(())
}

/// 删除调拨单（软删；仅 DRAFT / CANCELLED）。
#[nik::permission(
    path = "/business/erp/transfer/{id}",
    method = "DELETE",
    permission = "business:erp:transfer:delete"
)]
#[nik::delete("/business/erp/transfer/{id}")]
pub async fn transfer_delete(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Path(id): Path<String>,
) -> ResResult<()> {
    let id = parse_uuid(&id, "id")?;
    delete_transfer(id, operator_of(&claims)).await?;
    Ok(Res::ok(()))
}

// ==================== ⑦ 序列号 ====================

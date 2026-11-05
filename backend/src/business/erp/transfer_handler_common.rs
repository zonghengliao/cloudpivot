// ============================================================================
// transfer_handler_common.rs —— 底座：错误出口（409/404/写库映射）/ 入参解析 / 明细预处理（行校验 + 单位与序列号）
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use crate::business::erp::transfer_dto::CreateTransferReq;
use crate::business::erp::transfer_model::{self, NewItem, SettleError};
use crate::business::erp::unit_guard;
use crate::business::flow::model as flow_model;
use crate::shared::error_code;
use crate::utils;
use nik::prelude::*;
use nik::uuid::Uuid;

// ==================== 共用辅助 ====================

/// 路径 / 请求体里的 UUID：非法 → `400 + 66006`（绝不静默当作「没传」）。
pub(super) fn parse_uuid(raw: &str, field: &str) -> std::result::Result<Uuid, ApiError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不能为空"),
        ));
    }
    Uuid::parse_str(text).map_err(|_| {
        ApiError::new(
            400,
            error_code::ERP_FILTER_INVALID,
            format!("{field} 不是合法的 UUID：{text}"),
        )
    })
}

/// 可空 UUID：空串 / 缺席都是 `None`。
pub(super) fn optional_uuid(
    raw: Option<&str>,
    field: &str,
) -> std::result::Result<Option<Uuid>, ApiError> {
    match raw.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => parse_uuid(text, field).map(Some),
        None => Ok(None),
    }
}

/// 操作人（`create_by` / `update_by`）。
pub(super) fn operator_of(claims: &TokenClaims) -> Option<Uuid> {
    utils::current_user_uuid(claims)
}

/// 不存在统一 404。
pub(super) fn not_found(what: &str) -> ApiError {
    ApiError::new(
        404,
        error_code::RESOURCE_NOT_FOUND,
        format!("{what}不存在或已删除"),
    )
}

/// 状态冲突统一 409（非法流转 / 非 DRAFT 改单 / 非 DRAFT|CANCELLED 删除）。
pub(super) fn conflict(message: impl Into<String>) -> ApiError {
    ApiError::new(409, error_code::RESOURCE_CONFLICT, message.into())
}

/// 弱引用缺失 / 数据不一致统一 400（`66002`）。
pub(super) fn reference_missing(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::ERP_REFERENCE_MISSING, message.into())
}

/// 是不是「已有审批中实例」的唯一索引冲突。
///
/// **判据只有一份实现**：[`flow_model::is_biz_active_duplicate`]（变体优先 + 文案兜底）。
/// 正常路径上 PG 的 `23505` 会被驱动方言翻成 [`DbError::UniqueViolation`] 并带上约束名；
/// 但事务内执行、或驱动拿不到 `SqlState` 时错误会落到 [`DbError::Query`]
/// （`Display` 形如 `query failed: <sql>: db error: ...`），变体判据就失效了。
/// 只按变体判的后果是「明明是可解释的冲突，用户却看到 500 服务器内部错误」。
pub(super) fn is_biz_active_duplicate(error: &DbError) -> bool {
    flow_model::is_biz_active_duplicate(error)
}

/// 「该调拨单已有审批中实例」→ **409 + 单号**（可执行提示，不是 500）。
///
/// 文案与状态码收在 [`flow_model::approval_in_progress`]（兄弟模块共用同一份）；
/// `transfer_no` 拿不到时（例如从 [`write_error`] 的通用出口兜底）退化成「该调拨单」。
pub(super) fn approval_in_progress(transfer_no: Option<&str>) -> ApiError {
    flow_model::approval_in_progress("调拨单", transfer_no)
}

/// 写路径的数据库错误收口：唯一冲突 → `409 + 66001`、CHECK 违反 → `400`。
///
/// 判据按**变体**而不是文案：`DbError::UniqueViolation` 的 `Display` 里没有
/// `duplicate key` 字样（W1c 实测），按文案匹配会把 409 判成 500。
pub(super) fn write_error(err: DbError, ctx: &str) -> ApiError {
    // ⚠️ 顺序要紧：审批实例的唯一索引冲突**先**认出来。
    // 落到下面的 `UniqueViolation` 分支会被说成「调拨单号重复（唯一约束 uk_biz_flow_instance_biz_active）
    // …联系运维排查 biz_seq」——用户按提示去查取号器，方向完全错；
    // 而如果驱动没给约束名（错误退化成 `DbError::Query`），就会一路掉到
    // `utils::db_error` 变成 500「服务器内部错误」，把可解释的冲突彻底藏起来。
    if is_biz_active_duplicate(&err) {
        tracing::warn!(context = ctx, error = %err, "提交审批撞审批实例唯一索引：已有在跑实例，翻 409");
        return approval_in_progress(None);
    }
    match err {
        DbError::UniqueViolation { constraint, .. } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "调拨单号重复（唯一约束{}）：取号器与库里的号段不一致，请联系运维排查 biz_seq",
                constraint
                    .as_deref()
                    .map(|name| format!(" {name}"))
                    .unwrap_or_default()
            ),
        ),
        DbError::CheckViolation { constraint, .. } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "数据不满足库约束 {}（状态与时间戳必须匹配 ck_biz_transfer_state；\
                 调出/调入仓库不能相同）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        DbError::ForeignKeyViolation { constraint, .. } => ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!(
                "引用的数据不存在（外键 {}）",
                constraint.as_deref().unwrap_or("<未知>")
            ),
        ),
        other => utils::db_error(other, ctx),
    }
}

/// 结算失败 → HTTP：库存不足 409、序列号不符 400、不变式不满足 409。
///
/// 为什么不复用 [`write_error`]：结算的失败原因里有一半**不是数据库错误**
/// （库存不足是业务冲突、序列号个数不匹配是参数问题），必须给出能直接照着改的文案，
/// 而不是笼统的「HTTP 500 服务内部错误」。
pub(super) fn settle_error(err: SettleError) -> ApiError {
    match err {
        SettleError::Db(inner) => write_error(inner, "调拨结算（库存双写）"),
        SettleError::Insufficient {
            material_code,
            required,
            available,
        } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "调出仓库存不足：物料 {material_code} 本次需要 {required}，当前可用 {available}。\
                 另一张调拨单可能刚扣走同一批库存，请刷新库存后重新提交"
            ),
        ),
        SettleError::SerialMismatch {
            line_no,
            qty,
            serials,
        } => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "第 {line_no} 行是一物一码物料：明细数量 {qty} 与建单时的序列号个数 {serials} 不一致，\
                 请改单重新选择序列号"
            ),
        ),
        SettleError::SerialNotInSource {
            line_no,
            expected,
            moved,
        } => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!(
                "第 {line_no} 行的序列号当前不在调出仓或已不在库（应有 {expected} 个，实际只转移了 \
                 {moved} 个）：请核对序列号是否已被其他单据出库"
            ),
        ),
        SettleError::Invariant(message) => ApiError::new(
            409,
            error_code::ERP_CODE_CONFLICT,
            format!("调拨结算后不变式校验失败：{message}"),
        ),
        SettleError::EmptyItems => ApiError::new(
            400,
            error_code::PARAM_INVALID,
            "调拨单没有明细，无法结算".to_string(),
        ),
        // DC-09 补口：单位止血闸已经组好 400 + ERP_UNIT_NOT_BASE 与折算建议，原样透出
        SettleError::UnitGuard(inner) => inner,
    }
}

// ==================== 引用校验（一次查完，落库前） ====================

/// DTO 明细 → 数据层明细（解析物料 / 库位 / 序列号引用并校验归属）。
///
/// 校验顺序：**物料 → 调出库位 → 调入库位 → 序列号**，任何一条不过都 `400`，
/// 一行都不落库（调用方在事务外先跑这个）。
pub(super) async fn prepare_items(
    req: &CreateTransferReq,
    from_warehouse_id: Uuid,
    to_warehouse_id: Uuid,
) -> std::result::Result<Vec<NewItem>, ApiError> {
    let mut items = Vec::with_capacity(req.items.len());
    let mut payload = String::new();
    let mut expected = 0_usize;
    for (index, item) in req.items.iter().enumerate() {
        let material_id = parse_uuid(&item.material_id, &format!("items[{index}].material_id"))?;
        transfer_model::material_ref(material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询物料"))?
            .ok_or_else(|| reference_missing(format!("items[{index}].material_id 物料不存在")))?;
        // ★ DC-09 单位止血（方案 B）：调拨明细不存 `unit_id`，数量直接进两本账 ——
        //   物料挂折算单位时「5 箱」会被当成 5 个搬走，直接拒单。
        unit_guard::ensure_base_unit(material_id, index, item.qty).await?;

        let from_location_id = optional_uuid(
            item.from_location_id.as_deref(),
            &format!("items[{index}].from_location_id"),
        )?;
        if let Some(location_id) = from_location_id {
            let (_, warehouse_id) = transfer_model::inventory_location_ref(location_id)
                .await
                .map_err(|error| utils::db_error(error, "查询调出库位"))?
                .ok_or_else(|| {
                    reference_missing(format!(
                        "items[{index}].from_location_id 库位库存行不存在\
                         （location_id 指向 biz_inventory_location.id）"
                    ))
                })?;
            if warehouse_id != from_warehouse_id {
                return Err(reference_missing(format!(
                    "items[{index}].from_location_id 不属于调出仓库"
                )));
            }
        }
        let to_location_id = optional_uuid(
            item.to_location_id.as_deref(),
            &format!("items[{index}].to_location_id"),
        )?;
        if let Some(location_id) = to_location_id {
            let (_, warehouse_id) = transfer_model::inventory_location_ref(location_id)
                .await
                .map_err(|error| utils::db_error(error, "查询调入库位"))?
                .ok_or_else(|| {
                    reference_missing(format!(
                        "items[{index}].to_location_id 库位库存行不存在\
                         （location_id 指向 biz_inventory_location.id）"
                    ))
                })?;
            if warehouse_id != to_warehouse_id {
                return Err(reference_missing(format!(
                    "items[{index}].to_location_id 不属于调入仓库"
                )));
            }
        }

        let mut serial_ids = Vec::with_capacity(item.serial_ids.len());
        for (serial_index, raw) in item.serial_ids.iter().enumerate() {
            let serial_id = parse_uuid(raw, &format!("items[{index}].serial_ids[{serial_index}]"))?;
            serial_ids.push(serial_id);
            payload.push_str(&format!("{index}|{serial_id}|{material_id}\n"));
            expected += 1;
        }

        items.push(NewItem {
            material_id,
            from_location_id,
            to_location_id,
            qty: item.qty.round_dp(4),
            serial_ids,
        });
    }

    if expected > 0 {
        let payload = payload.trim_end().to_string();
        let valid = transfer_model::count_valid_serials(&payload)
            .await
            .map_err(|error| utils::db_error(error, "校验序列号"))?;
        if usize::try_from(valid).unwrap_or(0) != expected {
            return Err(reference_missing(format!(
                "提交了 {expected} 个序列号，其中只有 {valid} 个是「该物料 + 在库（IN_STOCK）+ 未删除」：\
                 序列号不存在 / 不属于该物料 / 已经出库的都不能随行调拨"
            )));
        }
    }
    Ok(items)
}

// ==================== ① 分页 ====================

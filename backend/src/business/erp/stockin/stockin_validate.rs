// ============================================================================
// stockin_validate.rs —— 校验：可作废前置校验（短溢/缺流水/序列号已出库/采购单抢跑）
//
// 由 `stockin_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::serial_model;

use super::stockin_cancel::{
    SERIAL_SHIPPED_PREFIX, SHORTFALL_PREFIX, cancel_plan_of, lock_serials_of, serial_status_label,
};

/// 作废的**只读可行性预检**（P16）：发起审批之前跑一遍，做不成的单别送去复核。
///
/// # 为什么要有它
///
/// P16 把「作废」拆成「发起 → 复核 → 执行」。执行（[`reverse_cost`] / [`void_serials_of`]）
/// 本来就会拒绝「货已出库」的单，但那时审批人已经批了 —— 结论是 APPROVED、账却一分没退，
/// 单据卡在「作废审批中」，比直接报错更糟。所以判据提前到**发起**这一步：
/// 用同一套哨兵前缀（[`SHORTFALL_PREFIX`] / [`SERIAL_SHIPPED_PREFIX`]）给出同一份 409 文案，
/// handler 原样映射，前端看到的话术与 P16 之前完全一致。
///
/// 只读：不加锁、不改行（`lock_serials_of` 的 `FOR UPDATE` 在事务外层会自动失效，
/// 预检本来就允许「判完就被人改了」——真正的护栏仍在执行时的 `reverse_cost` / `void_serials_of`）。
pub async fn ensure_cancellable(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    in_no: &str,
    warehouse_id: Uuid,
) -> DbResult<()> {
    // ① 一物一码：本单生成的号码必须全部仍在库
    let serials = lock_serials_of(ex, stock_in_id).await?;
    if let Some(blocked) = serials
        .iter()
        .find(|serial| serial.status != serial_model::STATUS_IN_STOCK)
    {
        return Err(DbError::InvalidValue(format!(
            "{SERIAL_SHIPPED_PREFIX}收货单 {in_no} 生成的一物一码序列号 {} 已{}（{}），\
             不能随单作废：那几件已经离开仓库，请先做退货入库冲销",
            blocked.serial_no,
            serial_status_label(&blocked.status),
            blocked.status
        )));
    }
    // ② 成本账数量够不够退（与 [`reverse_cost`] 同一判据、同一字典序）
    let plan = cancel_plan_of(ex, stock_in_id, in_no, warehouse_id).await?;
    for line in &plan {
        let before = inventory_qty_of(ex, line.material_id, warehouse_id, line.location_id).await?;
        if before < line.qty {
            let code: Option<String> = ex
                .scalar_sql(
                    "SELECT material_code FROM biz_material WHERE id = ?::uuid".to_string(),
                    vec![Value::from(&line.material_id)],
                )
                .await?;
            return Err(DbError::InvalidValue(format!(
                "{SHORTFALL_PREFIX}物料 {} 库存 {} < 本单 {}",
                code.unwrap_or_else(|| line.material_id.to_string()),
                before.round_dp(4),
                line.qty.round_dp(4)
            )));
        }
    }
    Ok(())
}

/// 读 `(物料, 仓库, 库位)` 的成本账数量（`location_id IS NULL` = 未分配库位汇总桶）。
///
/// 与 [`lock_inventory`] 同一个命中口径：`COALESCE(location_id, 零 uuid)`，
/// 这样走得上 `uk_biz_inv_mat_wh_loc` 的表达式索引。
pub(super) async fn inventory_qty_of(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Decimal> {
    let qty: Option<Decimal> = ex
        .scalar_sql(
            "SELECT qty FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                    = ?::uuid"
                .to_string(),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::from(&location_id.unwrap_or_else(Uuid::nil)),
            ],
        )
        .await?;
    Ok(qty.unwrap_or(Decimal::ZERO))
}

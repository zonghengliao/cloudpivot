// ============================================================================
// shipment_transit.rs —— 发货过账的在途库存效果（M2-02）
//
// # 卡面口径（t_6c090f97 · M2-02【M2·仓库】在途库存：发货单过账 → 在途 + 出库待确认）
//
// ```text
// 发货时数量进在途，库存不扣。
// ```
//
// # 语义变化（与原 SO-10 相比）
//
// SO-10 的口径是「发货过账 = 销售出库过账」：一个事务里扣三本账 + 生成成本结转凭证 +
// 挂应收。M2-02 把它改成**两段**：
//
// ```text
// 发货过账（本文件）        ① 在途账 +shipped_qty（一行 = 物料 × 发货仓 × 发货单）
//                            ② 生成销售出库单，落 PENDING_POST（= 出库待确认）
//                            ③ 三本账 / 凭证 / 应收**一律不动**（库存不扣）
// 签收确认（M2-03）          ④ 出库单过账 → 三本账负向 + 成本结转 + 挂应收
//                            ⑤ 在途账 received_qty += n（一条 UPDATE 同时推状态）
// ```
//
// 为什么这样切：货发出去了但客户还没签收，实物已经离开仓库、账面还没确认出库 ——
// 这段时间的量既不是「仓里的库存」（所以不能扣 biz_inventory），也不是「已出库」
// （所以不能生成成本结转凭证 / 应收），只能单独记一本**在途账**。
//
// # 在途账不归三本账管
//
// `biz_inventory_in_transit` **不是** `biz_inventory` / `biz_inventory_location` /
// `biz_inventory_txn` 中的任何一本：它不影响库存数量、不生成出入库流水、不参与
// 移动加权成本，所以不经 `inventory_writer`（P2-5A 的写入口闸只认那三张表）。
// 表结构与约束理由见迁移 `V0420__inventory_in_transit.up.sql`。
//
// # 幂等
//
// 唯一键 `uk_biz_inv_transit_key (material_id, warehouse_id, shipment_id)` 兜住 upsert：
// 同一发货单同一物料重复写只累加一次（正常路径下来不了第二次 —— 调用方
// `shipment_posting::post_effects` 的幂等闸在 `biz_shipment.stock_out_id` 上）。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::shipment::shipment_model::{ShipmentItemRow, ShipmentRow};
use cloudpivot_kernel::business::shared::state_machine;
use cloudpivot_kernel::utils;

/// 在途库存账表名（V0420）。
pub const TABLE: &str = "biz_inventory_in_transit";

/// 在途：`received_qty < shipped_qty`（部分签收时仍是在途，余量就是差值）。
pub const STATUS_IN_TRANSIT: &str = "IN_TRANSIT";
/// 全部签收：`received_qty = shipped_qty` 且 `received_at` 必填（M2-03 写）。
pub const STATUS_RECEIVED: &str = "RECEIVED";
/// 已作废：发货单作废时由调用方写；⚠ 所有在途取数必须带 `status <> 'CANCELLED'`。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 一条在途行的写入结果（供 `PostOutcome` 回显与日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitLine {
    /// 物料。
    pub material_id: Uuid,
    /// 本次写进在途的发货数量（同一物料多行已合并）。
    pub shipped_qty: Decimal,
}

/// 明细按物料合并（纯函数，单测钉死）：同一物料多行合成一条在途量。
///
/// 为什么要合并：在途的唯一键含 `material_id`，不合并就得靠 upsert 累加 ——
/// 那也能对，但「本次写了哪几行」的回显会变成同一物料出现多次（日志/详情误导）。
/// 合并后顺序 = 首次出现顺序，回显稳定。
pub fn merge_by_material(items: &[ShipmentItemRow]) -> Vec<TransitLine> {
    let mut order: Vec<Uuid> = Vec::with_capacity(items.len());
    let mut sums: HashMap<Uuid, Decimal> = HashMap::with_capacity(items.len());
    for item in items {
        match sums.get_mut(&item.material_id) {
            Some(total) => *total += item.qty,
            None => {
                order.push(item.material_id);
                sums.insert(item.material_id, item.qty);
            }
        }
    }
    order
        .into_iter()
        .map(|material_id| TransitLine {
            material_id,
            shipped_qty: sums
                .get(&material_id)
                .map(|qty| qty.round_dp(4))
                .unwrap_or(Decimal::ZERO),
        })
        .collect()
}

/// 把本次发货量写进在途账（`INSERT … ON CONFLICT DO UPDATE`，调用方负责事务边界）。
///
/// 冲突时 `shipped_qty += EXCLUDED.shipped_qty`（M2-01 的 downstream_contract 原文），
/// 状态按新数量重算：作废行保持作废，其余一律回到 `IN_TRANSIT`
/// （追加了发货量以后 `received_qty < shipped_qty` 恒成立，`ck_biz_inv_transit_state` 不会炸）。
pub async fn record_shipped(
    ex: &impl SqlExecutor,
    shipment: &ShipmentRow,
    items: &[ShipmentItemRow],
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<Vec<TransitLine>, ApiError> {
    let lines = merge_by_material(items);
    let operator_text = Value::Text(operator.id.map(|id| id.to_string()).unwrap_or_default());
    for line in &lines {
        ex.exec_sql(
            format!(
                "INSERT INTO {TABLE} \
                    (shipment_id, material_id, warehouse_id, ship_date, shipped_qty, status, \
                     created_at, updated_at, create_by, update_by) \
                 VALUES (?::uuid, ?::uuid, ?::uuid, ?::text::date, ?::text::numeric, \
                         '{STATUS_IN_TRANSIT}', ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
                 ON CONFLICT (material_id, warehouse_id, shipment_id) DO UPDATE \
                    SET shipped_qty = {TABLE}.shipped_qty + EXCLUDED.shipped_qty, \
                        status = CASE WHEN {TABLE}.status = '{STATUS_CANCELLED}' \
                                      THEN '{STATUS_CANCELLED}' ELSE '{STATUS_IN_TRANSIT}' END, \
                        updated_at = EXCLUDED.updated_at, \
                        update_by = EXCLUDED.update_by"
            ),
            vec![
                Value::from(&shipment.id),
                Value::from(&line.material_id),
                Value::from(&shipment.warehouse_id),
                Value::Text(shipment.ship_date.to_string()),
                Value::Text(line.shipped_qty.to_string()),
                Value::from(now),
                Value::from(now),
                operator_text.clone(),
                operator_text.clone(),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "写发货在途账"))?;
    }
    tracing::info!(
        shipment_id = %shipment.id,
        ship_no = %shipment.ship_no,
        lines = lines.len(),
        "发货过账：在途账已写入（库存不扣，出库单待签收确认）"
    );
    Ok(lines)
}

/// 该 (物料, 仓库) 当前未签收的在途量合计（`status = 'IN_TRANSIT'`）。
///
/// 发货过账的「可用量」判据要用它：M2-02 起库存不再随发货扣减，
/// 不把在途减掉就能把同一批货反复发出去（账面一直是满的）。
pub async fn open_qty(ex: &impl SqlExecutor, material_id: Uuid, warehouse_id: Uuid) -> DbResult<Decimal> {
    // SCOPE-EXEMPT: 在途账是发货单的派生账，无归属列（可见性靠权限码）。
    let rows = ex
        .query_sql(
            format!(
                "SELECT COALESCE(SUM(qty_in_transit), 0)::text AS open FROM {TABLE} \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND status = '{STATUS_IN_TRANSIT}'"
            ),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?;
    Ok(rows
        .first()
        .and_then(|row| row.get_typed::<String>("open").ok())
        .and_then(|value| value.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO)
        .round_dp(4))
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dec(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("数量字面量非法")
    }

    fn item(material_id: Uuid, qty: &str) -> ShipmentItemRow {
        ShipmentItemRow {
            id: Uuid::now_v7(),
            shipment_id: Uuid::now_v7(),
            so_item_id: Uuid::now_v7(),
            material_id,
            qty: dec(qty),
            location_id: None,
            serial_nos: Vec::new(),
            remark: None,
            line_no: 1,
        }
    }

    /// 同一物料多行 → 一条在途量；不同物料按首次出现顺序排列。
    #[test]
    fn merge_groups_same_material_in_first_seen_order() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let merged = merge_by_material(&[item(a, "1.5"), item(b, "2"), item(a, "3.25")]);
        assert_eq!(merged.len(), 2, "两个物料只能出两条在途行");
        assert_eq!(merged[0].material_id, a);
        assert_eq!(merged[0].shipped_qty, dec("4.75"));
        assert_eq!(merged[1].material_id, b);
        assert_eq!(merged[1].shipped_qty, dec("2"));
    }

    /// 空明细 → 空结果（调用方在此之前已经拦过「没有明细不能过账」）。
    #[test]
    fn merge_of_empty_items_is_empty() {
        assert!(merge_by_material(&[]).is_empty());
    }

    /// 状态常量必须与 V0420 的 `ck_biz_inv_transit_status` 逐字一致。
    #[test]
    fn status_domain_matches_migration_check() {
        assert_eq!(STATUS_IN_TRANSIT, "IN_TRANSIT");
        assert_eq!(STATUS_RECEIVED, "RECEIVED");
        assert_eq!(STATUS_CANCELLED, "CANCELLED");
        assert_eq!(TABLE, "biz_inventory_in_transit");
    }

    /// 在途行必须带发货日期快照（在途账龄口径），日期格式化走 ISO `yyyy-MM-dd`。
    #[test]
    fn ship_date_renders_as_iso_date() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 16).expect("日期");
        assert_eq!(date.to_string(), "2026-11-16");
    }
}

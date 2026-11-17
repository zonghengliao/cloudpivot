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
// 出库待确认作废（M2-FIX）    ⑥ 出库单 DRAFT|PENDING_POST → CANCELLED ⇒ 在途行置 CANCELLED
//                              （同一事务，见 `cancel_open`；作废回滚时在途行一起回滚）
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

/// 状态域（3 值，顺序与 V0420 的 `ck_biz_inv_transit_status` 一致）。
///
/// 唯一真相在迁移里；本常量只给「筛选校验」与「报文列举合法值」用（M2-04 查询接口）。
pub const STATUSES: [&str; 3] = [STATUS_IN_TRANSIT, STATUS_RECEIVED, STATUS_CANCELLED];

/// 状态是否在域内（精确匹配，不做大小写归一 —— 库里存的就是大写）。
pub fn is_valid_status(status: &str) -> bool {
    STATUSES.contains(&status)
}

/// 状态中文标签（列表 / 详情回显；未知值原样返回，不 panic —— 库层 CHECK 已经兜住非法值）。
pub fn status_label(status: &str) -> &str {
    match status {
        STATUS_IN_TRANSIT => "在途",
        STATUS_RECEIVED => "已签收",
        STATUS_CANCELLED => "已作废",
        other => other,
    }
}

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

// ==================== 签收确认（M2-03） ====================

/// 一行**未签收**的在途（`status = 'IN_TRANSIT'`），签收确认要读/推的量。
///
/// `id` 是行主键（签收按行 UPDATE，不按 `(物料, 仓库, 发货单)` 条件改 —— 后者在
/// 并发下会改到别人刚插进来的行）；`qty_in_transit` 是 PG 生成列算出来的余量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenLine {
    /// 在途行主键。
    pub id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 发货数量（累计上限）。
    pub shipped_qty: Decimal,
    /// 已签收数量。
    pub received_qty: Decimal,
    /// 当前在途量 = `shipped_qty - received_qty`（生成列）。
    pub qty_in_transit: Decimal,
}

/// 一条签收结果（供 handler 打日志 / 文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptLine {
    /// 物料。
    pub material_id: Uuid,
    /// 本次签收数量。
    pub qty: Decimal,
    /// 签收后状态（`RECEIVED`；余量未清完则是 `IN_TRANSIT`）。
    pub status: String,
}

/// 锁住一张发货单的**全部未签收在途行**（`SELECT … FOR UPDATE`，物料升序）。
///
/// 调用方必须在**事务**里调用：并发签收靠这把行锁串行化 —— 两个请求都读到「还有 10 在途」，
/// 各自 `received_qty += 10` 就会撞 `ck_biz_inv_transit_qty`（容不下）或把一个单据签两遍。
pub async fn lock_open(ex: &impl SqlExecutor, shipment_id: Uuid) -> DbResult<Vec<OpenLine>> {
    read_open(ex, shipment_id, true).await
}

/// 只读一遍「还有多少未签收」（不加锁；给接口层做前置闸与文案用）。
///
/// 不加锁是**故意的**：真正的互斥在真正的写事务里（`receive_open` 的 `FOR UPDATE`
/// 与出库单状态的 CAS）。接口层先读一遍只为把「没有在途可签收」变成一句人话的 409，
/// 而不是让用户看到一个由状态机吐出来的通用冲突。
pub async fn open_lines(ex: &impl SqlExecutor, shipment_id: Uuid) -> DbResult<Vec<OpenLine>> {
    read_open(ex, shipment_id, false).await
}

async fn read_open(ex: &impl SqlExecutor, shipment_id: Uuid, for_update: bool) -> DbResult<Vec<OpenLine>> {
    let mut sql = format!(
        "SELECT id, material_id, shipped_qty::text AS shipped, \
                received_qty::text AS received, qty_in_transit::text AS open \
           FROM {TABLE} \
          WHERE shipment_id = ?::uuid AND status = '{STATUS_IN_TRANSIT}' \
          ORDER BY material_id"
    );
    if for_update {
        sql.push_str(" FOR UPDATE");
    }
    let rows = ex.query_sql(sql, vec![Value::from(&shipment_id)]).await?;
    rows.iter().map(OpenLine::from_row).collect()
}

impl OpenLine {
    fn from_row(row: &Row) -> DbResult<Self> {
        let dec = |name: &str| -> DbResult<Decimal> {
            let text = row.get_typed::<String>(name)?;
            text.trim()
                .parse::<Decimal>()
                .map_err(|error| DbError::InvalidValue(format!("在途行 {name} 不是合法数值：{text}（{error}）")))
        };
        Ok(Self {
            id: row.get_typed("id")?,
            material_id: row.get_typed("material_id")?,
            shipped_qty: dec("shipped")?,
            received_qty: dec("received")?,
            qty_in_transit: dec("open")?,
        })
    }
}

/// 签收：把该发货单**所有未签收在途行一次签满**（每行一条 UPDATE，`received_qty += 余量`）。
///
/// # 为什么是「一条 UPDATE」而不是先改数量再改状态
///
/// V0420 的 `ck_biz_inv_transit_state` 钉死了「IN_TRANSIT 必须还有余量 / RECEIVED 必须余量为 0」：
/// 分两句写会在第一句就被 CHECK 拦下（这正是设计意图 —— 不存在「签满了还挂在 IN_TRANSIT」
/// 的中间态能被提交出去）。所以 `received_qty` / `status` / `received_at` 在**同一条** SET 里推。
///
/// # 口径
///
/// * 只认 `status = 'IN_TRANSIT'` 的行（`CANCELLED` 是作废、`RECEIVED` 已签完，都不算在途）；
/// * 没有未签收行 → 返回空 `Vec`，**不是错误**：调用方自己决定这算不算问题
///   （签收确认入口把它当 409；出库单过账侧把它当「本就不是发货单生成的单」）。
/// * `WHERE … AND status = 'IN_TRANSIT'` 是 CAS：行在本次签收期间被别人签掉 → 0 行 → 409。
pub async fn receive_open(
    ex: &impl SqlExecutor,
    shipment_id: Uuid,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
    remark: Option<&str>,
) -> std::result::Result<Vec<ReceiptLine>, ApiError> {
    let operator_text = Value::Text(operator.id.map(|id| id.to_string()).unwrap_or_default());
    let remark = Value::Text(remark.unwrap_or_default().to_string());
    let mut done: Vec<ReceiptLine> = Vec::new();
    for line in lock_open(ex, shipment_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定在途行"))?
    {
        let open = line.qty_in_transit;
        if open <= Decimal::ZERO {
            continue;
        }
        let open_text = Value::Text(open.to_string());
        let affected = ex
            .exec_sql(
                format!(
                    "UPDATE {TABLE} \
                        SET received_qty = received_qty + ?::text::numeric, \
                            status = CASE WHEN received_qty + ?::text::numeric >= shipped_qty \
                                          THEN '{STATUS_RECEIVED}' ELSE '{STATUS_IN_TRANSIT}' END, \
                            received_at = CASE WHEN received_qty + ?::text::numeric >= shipped_qty \
                                               THEN ? ELSE received_at END, \
                            remark = COALESCE(NULLIF(?::text, ''), remark), \
                            updated_at = ?, \
                            update_by = NULLIF(?::text, '')::uuid \
                      WHERE id = ?::uuid AND status = '{STATUS_IN_TRANSIT}'"
                ),
                vec![
                    open_text.clone(),
                    open_text.clone(),
                    open_text.clone(),
                    Value::from(now),
                    remark.clone(),
                    Value::from(now),
                    operator_text.clone(),
                    Value::from(&line.id),
                ],
            )
            .await
            .map_err(|error| utils::db_error(error, "写在途签收"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                cloudpivot_kernel::shared::error_code::RESOURCE_CONFLICT,
                format!("物料 {} 的在途行在本次签收期间已被改动，请刷新后重试", line.material_id),
            ));
        }
        done.push(ReceiptLine {
            material_id: line.material_id,
            qty: open.round_dp(4),
            status: STATUS_RECEIVED.to_string(),
        });
    }
    if !done.is_empty() {
        tracing::info!(
            shipment_id = %shipment_id,
            lines = done.len(),
            "在途签收：received_qty 已推到 shipped_qty，status → RECEIVED"
        );
    }
    Ok(done)
}

// ==================== 作废释放在途（M2-FIX t_f123b57f） ====================

/// 作废释放在途：把该发货单**未签收**的在途行一律置 `CANCELLED`（两个数量列原样不动）。
///
/// # 什么时候用它
///
/// 出库单「待确认」（`DRAFT` / `PENDING_POST`）**作废**时，由
/// `stockout_handler::cancel_out_tx` 的未过账分支在**同一个事务**里调用。
///
/// 为什么非要它：M2-02 的「发货过账」只做两件事 —— 写一条在途 + 生成一张停在
/// `PENDING_POST` 的销售出库单（库存一分不扣）。作废这张出库单等于**宣布这批货不再出库**，
/// 在途必须同步撤回。不撤的后果不是报错，而是静默少货：M2-02 的可用量口径是
/// 「可用 = 账面 − 未签收在途」，这批货从此被永久扣着，而单据已经是终态 `CANCELLED`、
/// 界面上没有任何可点的按钮 —— 只能改库才能恢复。
///
/// 已过账（`POSTED`）的出库单**不要**调它：那条路走红冲，在途早在过账 ⑤c 里推到
/// `RECEIVED` 了；再作废一次等于把已签收的量重复放回可用量。
///
/// # 只改状态，不动数量
///
/// 「这批发过货」是事实（`shipped_qty` 是历史快照），作废的是「它还占着可用量」这件事。
/// V0420 的 `ck_biz_inv_transit_state` 对 `CANCELLED` 不限数量，所以一条 UPDATE 就够；
/// `qty_in_transit` 是生成列，作废行不参与任何在途取数（所有查询都带
/// `status <> 'CANCELLED'`）。
///
/// # 返回
///
/// 被作废的行（**作废前**的快照，可直接打日志）。没有未签收行 → 空 `Vec`，不是错误：
/// 手工建的出库单没有在途、重复作废也到不了这里（状态机先拦）。
pub async fn cancel_open(
    ex: &impl SqlExecutor,
    shipment_id: Uuid,
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
    remark: Option<&str>,
) -> std::result::Result<Vec<OpenLine>, ApiError> {
    let operator_text = Value::Text(operator.id.map(|id| id.to_string()).unwrap_or_default());
    // `remark` 列是 VARCHAR(255)：按**字符**截断（多字节安全，切不坏 UTF-8）；
    // 空串交给 NULLIF —— 已有备注（例如过账时写的）不被清空。
    let remark_text: String = remark.unwrap_or_default().trim().chars().take(255).collect();
    let remark_value = Value::Text(remark_text);
    let mut released: Vec<OpenLine> = Vec::new();
    for line in lock_open(ex, shipment_id)
        .await
        .map_err(|error| utils::db_error(error, "锁定在途行（作废释放）"))?
    {
        let affected = ex
            .exec_sql(
                format!(
                    "UPDATE {TABLE} \
                        SET status = '{STATUS_CANCELLED}', \
                            remark = COALESCE(NULLIF(?::text, ''), remark), \
                            updated_at = ?, \
                            update_by = NULLIF(?::text, '')::uuid \
                      WHERE id = ?::uuid AND status = '{STATUS_IN_TRANSIT}'"
                ),
                vec![
                    remark_value.clone(),
                    Value::from(now),
                    operator_text.clone(),
                    Value::from(&line.id),
                ],
            )
            .await
            .map_err(|error| utils::db_error(error, "作废：释放在途"))?;
        if affected == 0 {
            // 行在本次作废期间被别人改掉（例如刚被签收）—— 整笔作废回滚，让用户刷新重试，
            // 绝不留下「出库单已作废、在途还占着」的半截账。
            return Err(ApiError::new(
                409,
                cloudpivot_kernel::shared::error_code::RESOURCE_CONFLICT,
                format!(
                    "物料 {} 的在途行在本次作废期间已被改动（可能刚被签收），请刷新后重试",
                    line.material_id
                ),
            ));
        }
        released.push(line);
    }
    if !released.is_empty() {
        tracing::info!(
            shipment_id = %shipment_id,
            lines = released.len(),
            "出库单作废：在途行已置 CANCELLED（可用量释放）"
        );
    }
    Ok(released)
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

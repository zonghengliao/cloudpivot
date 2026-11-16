// ============================================================================
// shipment_posting.rs —— 发货过账的库存效果（M2-02：发货 → 在途 + 出库待确认）
//
// # 一个事务里做完五件事（顺序不能换）
//
// ```text
// ① 防御性幂等：biz_shipment.stock_out_id 已回填 → 409（本函数永不重复写）
// ② 锁来源订单行（SELECT … FOR UPDATE）：同一订单的多张发货单并发过账在这里串行化，
//    否则两张单各自读到 APPROVED，订单状态会被后写的那张覆盖回退
// ③ 建销售出库单（biz_stock_out, source_type='SALES', out_type='OTHER'）并推进到
//    **PENDING_POST = 出库待确认**：账一分没动，等签收确认（M2-03）再真过账
// ④ 写**在途库存账**（`biz_inventory_in_transit`，见 `shipment_transit`）：
//    一行 = 物料 × 发货仓 × 发货单，`shipped_qty` 按物料汇总，**库存不扣**
// ⑤ 回填 biz_shipment.stock_out_id → 订单行 delivered_qty += qty → 订单转
//    PARTIAL_OUT / OUTBOUND
// ```
//
// # M2-02 起「发货过账」不再动三本账（这是卡面口径，不是退化）
//
// 卡面原话：「发货时数量进在途，库存不扣」。旧口径（SO-10）是把发货过账等同于
// 销售出库过账 —— 一个事务里扣成本账 / 库位账 / 流水、生成成本结转凭证、挂应收。
// 现在把它切成两段：
//
// ```text
// 本文件（发货过账）  在途账 + 出库单待确认 + delivered_qty + 订单状态
// M2-03（签收确认）   出库单过账（三本账负向 + 成本结转凭证 + 挂应收）+ 在途转已出库
// ```
//
// 这么切的原因：货出了仓库、客户还没签收。这段量既不是「仓里的库存」
// （所以不能扣 biz_inventory，否则账上凭空少货），也不是「已出库」
// （所以不能出成本结转凭证 / 挂应收，否则收入成本早确认一个环节）。
//
// # 三本账一行 SQL 都不在本文件
//
// 库存三本账（成本账 / 库位账 / 流水）的唯一写入口是 `erp/inventory_writer.rs`
// （P2-5A 门禁）。本文件不碰它们：在途账不属于三本账，独立成表、独立口径
// （见迁移 V0420 的注释与 `shipment_transit` 的模块文档）。
//
// # 原子性
//
// 全部动作跑在**调用方（`shipment_handler::change_shipment_status`）的同一个事务**里：
// 任何一步 Err 都由调用方 `rollback()`，出库单 / 在途行 / 回填列 / delivered_qty /
// 订单状态一起消失。本文件自己没有 `commit()`，也不该有。
//
// # 幂等
//
// 三道闸：① 发货单 CAS（`apply_status` 只在 `status=PENDING_POST` 时改，第二次 0 行）；
// ② `biz_shipment.stock_out_id` 回填 CAS（第二次 0 行）；③ 库层部分唯一索引
// `uk_biz_so_source(source_type, source_id)`（并发下最后一道，撞了就是 409）；
// 在途账另有 `uk_biz_inv_transit_key` 兜住 upsert（不会多算一笔在途）。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::sales::sales_order_model;
use crate::business::erp::shipment::shipment_model::{
    self as shipment, SHIPPABLE_SO_STATUSES, ShipmentItemRow, ShipmentRow,
};
use crate::business::erp::shipment::shipment_transit::{self, TransitLine};
use crate::business::erp::stockout::stockout_base::{
    LEGAL_EDGES as STOCK_OUT_EDGES, NewStockOut, NewStockOutLine, OUT_TYPE_OTHER, SOURCE_TYPE_SALES,
    STATUS_DRAFT as STOCK_OUT_DRAFT, STATUS_PENDING_POST as STOCK_OUT_PENDING_POST, Totals,
};
use crate::business::erp::stockout::stockout_model::{self as stock_out, MaterialInfo};
use cloudpivot_kernel::business::shared::state_machine;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

/// 过账成功后的业务结果（供 handler 打日志 / 回显）。
#[derive(Debug)]
pub struct PostOutcome {
    /// 新生成的销售出库单 id（`PENDING_POST`，等签收确认）。
    pub stock_out_id: Uuid,
    /// 出库单号（`OUT-yyyyMMdd-000001`）。
    pub stock_out_no: String,
    /// 过账后的销售订单状态（`PARTIAL_OUT` / `OUTBOUND`；订单状态没变时为 None）。
    pub so_status: Option<String>,
    /// 写进销售出库单的明细行数（同物料同库位已合并）。
    pub lines: usize,
    /// 写进在途账的行（物料 × 发货仓 × 发货单，同物料已合并）。
    pub transit: Vec<TransitLine>,
}

/// 出库单摘要（出库列表 / 详情一眼能看出这是销售发货）。
pub fn stock_out_reason(ship_no: &str) -> String {
    format!("销售发货 {ship_no}")
}

/// 出库单落 `PENDING_POST` 时写进状态日志的原因（谁能看出这张单是怎么来的）。
pub fn pending_confirm_reason(ship_no: &str) -> String {
    format!("发货单 {ship_no} 过账生成，待客户签收确认后过账出库")
}

/// 签收确认时写进出库单状态日志 / 在途行备注的原因（M2-03）。
///
/// 一条文案两处用（状态日志 + 在途行 `remark`）：对账时要能从出库单的 `POSTED` 那一刻
/// 倒推回「这是客户签收确认，不是谁手工过账」，所以带上发货单号。
pub fn receipt_reason(ship_no: &str) -> String {
    format!("客户签收确认 {ship_no}")
}

/// 订单在本次发货后应该处于哪个状态（纯函数，单测钉死）。
///
/// * `ordered` = Σ(`biz_sales_order_item.qty`)：订单总量
/// * `delivered` = Σ(`biz_sales_order_item.delivered_qty`)：已发总量（**含本次**）
///
/// `delivered >= ordered` → `OUTBOUND`（已出库）；否则 `PARTIAL_OUT`（部分出库）。
/// 卡面写的 `PARTIAL_SHIPPED` / `SHIPPED` 是技术架构旧稿的值 —— V0264 的
/// `ck_sales_order_status` 里没有这两个值（迁移自检里专门插了一条 SHIPPED 断言必被拒），
/// PRD §4.13.3 用的是 `PARTIAL_OUT` / `OUTBOUND`，所以这里只可能产出后两者。
pub fn order_status_after(ordered: Decimal, delivered: Decimal) -> &'static str {
    if ordered > Decimal::ZERO && delivered >= ordered {
        sales_order_model::STATUS_OUTBOUND
    } else {
        sales_order_model::STATUS_PARTIAL_OUT
    }
}

/// 一条发货明细对应的出库行备注（行号可追：出库单明细 ↔ 订单行）。
pub fn line_remark(order_line_no: i32) -> String {
    format!("销售发货订单行 {order_line_no}")
}

/// 出库/发货主数据错误 → ApiError（唯一口径：写库错误按约束类型翻译）。
fn write_error(error: DbError, ctx: &str) -> ApiError {
    if let DbError::UniqueViolation { constraint, .. } = &error
        && constraint
            .as_deref()
            .is_some_and(|name| name.contains("uk_biz_so_source"))
    {
        return ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("{ctx}：该发货单已经生成过销售出库单（幂等闸），不会重复写"),
        );
    }
    utils::db_error(error, ctx)
}

/// 锁来源销售订单并返回其当前状态（同一订单的并发过账在这里串行化）。
async fn lock_order(ex: &impl SqlExecutor, so_id: Uuid) -> std::result::Result<String, ApiError> {
    // SCOPE-EXEMPT: 销售订单是 ERP 单据表，无归属列（可见性靠权限码）。
    let rows = ex
        .query_sql(
            "SELECT status FROM biz_sales_order \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(so_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "锁定来源销售订单"))?;
    let Some(row) = rows.first() else {
        return Err(ApiError::new(
            400,
            error_code::ERP_REFERENCE_MISSING,
            format!("来源销售订单不存在或已删除：{so_id}"),
        ));
    };
    row.get_typed::<String>("status")
        .map_err(|error| utils::db_error(error, "读取销售订单状态"))
}

/// 库位**字典** id（`biz_location.id`，发货明细里存的）→ 库位**账行** id
/// （`biz_inventory_location.id`，出库明细扣减时用的）。
///
/// 发货明细的 `location_id` 是库位字典（V0283），出库明细的 `location_id` 是账行 ——
/// 两套 id 不能混用（混了就是签收确认时扣错桶）。该库位没有这个物料的账行 → 409：
/// 指定了库位就必须从那个库位出，不能静默改走 FIFO。
///
/// 本卡（M2-02）在**建出库单**这一步就校验：错在建单时暴露，而不是等到客户签收后
/// 才在出库确认里 409（那时货已经发了，单只能挂着等人修）。
async fn location_ledger_row(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Uuid,
) -> std::result::Result<Uuid, ApiError> {
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id = ?::uuid \
              LIMIT 1"
                .to_string(),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::from(&location_id),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询发货库位的库存账"))?;
    rows.first()
        .map(|row| {
            row.get_typed::<Uuid>("id")
                .map_err(|error| utils::db_error(error, "读取库位账 id"))
        })
        .transpose()?
        .ok_or_else(|| {
            ApiError::new(
                409,
                error_code::ERP_STOCK_OUT_NEGATIVE,
                format!(
                    "发货单指定的库位 {location_id} 上没有物料 {material_id} 的库存账（该库位无货）：\
                     换库位、清空库位（按 FIFO 出）或先补货"
                ),
            )
        })
}

/// 明细 → 出库行（物料信息 / 单位 / 库位账行都在这里换算，纯读）。
async fn to_stock_out_lines(
    ex: &impl SqlExecutor,
    shipment_row: &ShipmentRow,
    items: &[ShipmentItemRow],
) -> std::result::Result<Vec<NewStockOutLine>, ApiError> {
    let mut lines = Vec::with_capacity(items.len());
    let mut seen: HashMap<(Uuid, Option<Uuid>), Decimal> = HashMap::new();
    for item in items {
        let info = stock_out::material_info(ex, item.material_id)
            .await
            .map_err(|error| utils::db_error(error, "查询发货明细的物料"))?
            .ok_or_else(|| {
                ApiError::new(
                    400,
                    error_code::ERP_REFERENCE_MISSING,
                    format!("发货明细引用的物料不存在或已删除：{}", item.material_id),
                )
            })?;
        // 与 `stockout_handler::plan` 同口径：一物一码物料走出库单会把
        // `biz_material.qty`（序列号条件数）冲成负数，出库单只处理普通物料。
        if info.serial_required {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!(
                    "物料 {} 是一物一码物料，不能通过出库单扣减：请走序列号状态流转后再发货",
                    MaterialInfo::label(&info)
                ),
            ));
        }
        let location_row = match item.location_id {
            Some(dict_id) => Some(location_ledger_row(ex, item.material_id, shipment_row.warehouse_id, dict_id).await?),
            None => None,
        };
        // 同一物料 + 同一库位出现多行：合并成一行出库明细（行号仍可追）。
        // 不合并也能过账，但「发货 2 行 → 出库 2 行」会让签收确认后的 FIFO 对同一桶做两次
        // 够量判定，部分库位实现下会对同一桶重复分配（stockout_fifo_tests 踩过）。
        let key = (item.material_id, location_row);
        if let Some(total) = seen.get_mut(&key) {
            *total += item.qty;
        } else {
            seen.insert(key, item.qty);
            lines.push(NewStockOutLine {
                material_id: item.material_id,
                unit_id: info.unit_id,
                qty: item.qty,
                location_id: location_row,
                lot_no: None,
                remark: Some(line_remark(item.line_no)),
            });
        }
    }
    // 合并后的数量写回（顺序按首次出现，出库行号稳定）
    for line in &mut lines {
        if let Some(total) = seen.get(&(line.material_id, line.location_id)) {
            line.qty = *total;
        }
    }
    Ok(lines)
}

/// 把出库单从 `DRAFT` 推进到 `PENDING_POST`（= **出库待确认**）。
///
/// 两条写必须都在：`state_machine::transition` 写状态日志（谁、什么时候、为什么），
/// `stock_out::apply_status` 做 CAS（`WHERE status='DRAFT'`，并发下只有一个人能推进）。
/// 账一分不动 —— 三本账只在 M2-03 的签收确认里动。
async fn mark_pending_confirm(
    ex: &impl SqlExecutor,
    stock_out_id: Uuid,
    out_no: &str,
    operator: &state_machine::Operator,
    reason: &str,
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    state_machine::transition(
        ex,
        state_machine::ENTITY_STOCK_OUT,
        stock_out_id,
        out_no,
        &STOCK_OUT_EDGES,
        STOCK_OUT_DRAFT,
        STOCK_OUT_PENDING_POST,
        operator.clone(),
        Some(reason),
        None,
    )
    .await?;
    let affected = stock_out::apply_status(
        ex,
        stock_out_id,
        STOCK_OUT_DRAFT,
        STOCK_OUT_PENDING_POST,
        None,
        operator.id,
        now,
    )
    .await
    .map_err(|error| write_error(error, "出库单落待确认状态"))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!("出库单 {out_no} 的状态在本次过账期间被改动（期望 {STOCK_OUT_DRAFT}），请刷新后重试"),
        ));
    }
    Ok(())
}

/// 回写订单行 `delivered_qty`（**超发闸在 SQL 里**：`delivered_qty + qty <= qty`）。
///
/// 返回 0 行 = 该行已发满 / 并发被别人发掉 → 409，由调用方整单回滚
/// （这时出库单与在途行已经写了，回滚把它们一起还回去 —— 这正是「原子」的意思）。
async fn bump_delivered(
    ex: &impl SqlExecutor,
    items: &[ShipmentItemRow],
    now: DateTime<Utc>,
) -> std::result::Result<(), ApiError> {
    for item in items {
        let affected = ex
            .exec_sql(
                "UPDATE biz_sales_order_item \
                    SET delivered_qty = delivered_qty + ?::text::numeric, updated_at = ? \
                  WHERE id = ?::uuid \
                    AND delivered_qty + ?::text::numeric <= qty"
                    .to_string(),
                vec![
                    Value::Text(item.qty.to_string()),
                    Value::from(now),
                    Value::from(&item.so_item_id),
                    Value::Text(item.qty.to_string()),
                ],
            )
            .await
            .map_err(|error| utils::db_error(error, "回写订单行已发货数量"))?;
        if affected == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!(
                    "订单行 {} 已发满（本次要发 {}）：并发发货把它发掉了 —— \
                     本单的出库单与在途账已整笔回滚，请刷新订单后重试",
                    item.so_item_id, item.qty
                ),
            ));
        }
    }
    Ok(())
}

/// 订单总量与已发总量（同一事务内读，看得到本事务刚写的 `delivered_qty`）。
async fn order_totals(ex: &impl SqlExecutor, so_id: Uuid) -> std::result::Result<(Decimal, Decimal), ApiError> {
    // SCOPE-EXEMPT: 销售订单行是 ERP 单据明细，无归属列。
    let rows = ex
        .query_sql(
            "SELECT COALESCE(SUM(qty), 0)::text AS ordered, \
                    COALESCE(SUM(delivered_qty), 0)::text AS delivered \
               FROM biz_sales_order_item WHERE so_id = ?::uuid"
                .to_string(),
            vec![Value::from(so_id)],
        )
        .await
        .map_err(|error| utils::db_error(error, "汇总订单已发货数量"))?;
    let parse = |column: &str| -> Decimal {
        rows.first()
            .and_then(|row| row.get_typed::<String>(column).ok())
            .and_then(|text| text.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    };
    Ok((parse("ordered"), parse("delivered")))
}

/// **发货过账的库存效果**：调用方必须已在自己的事务里把发货单推进到 `POSTED`（CAS 成功）。
///
/// 做的事：生成销售出库单（`PENDING_POST` = 出库待确认）→ 写在途账 →
/// 回填 `stock_out_id` → 订单行 `delivered_qty` → 订单状态。
/// **不扣库存、不出成本结转凭证、不挂应收** —— 那些在 M2-03 的签收确认里做。
pub async fn post_effects(
    ex: &impl SqlExecutor,
    shipment_row: &ShipmentRow,
    items: &[ShipmentItemRow],
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<PostOutcome, ApiError> {
    // ① 防御性幂等：已回填过出库单 = 已经写过账，绝不再写第二遍
    if let Some(existing) = shipment_row.stock_out_id {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "发货单 {} 已经生成过销售出库单 {existing}（过账幂等闸）：\
                 在途只记一次，重复过账请查单据而不是重发",
                shipment_row.ship_no
            ),
        ));
    }
    if items.is_empty() {
        return Err(ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!("发货单 {} 没有明细，不能过账", shipment_row.ship_no),
        ));
    }

    // ② 锁来源订单（并发过账串行化）+ 状态闸
    let so_status = lock_order(ex, shipment_row.so_id).await?;
    if !SHIPPABLE_SO_STATUSES.contains(&so_status.as_str()) {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "来源销售订单当前是「{so_status}」，不能过账发货（只有 {} 的订单有未发量）",
                SHIPPABLE_SO_STATUSES.join(" / ")
            ),
        ));
    }
    // 订单号：订单不存在 / 已软删时（`so_no_of` 带 `deleted_at IS NULL`，与列表批量补齐同判据）
    // 退回订单 id —— 只影响出库单 remark 的可读性，不能让凭证摘要里出现「来源销售订单 ／…」空号
    let so_no = shipment::so_no_of(ex, shipment_row.so_id)
        .await
        .map_err(|error| utils::db_error(error, "查询来源销售订单号"))?
        .unwrap_or_else(|| shipment_row.so_id.to_string());

    // ③ 明细 → 出库行 → 建销售出库单（DRAFT）
    let lines = to_stock_out_lines(ex, shipment_row, items).await?;
    let reason = stock_out_reason(&shipment_row.ship_no);
    let remark = format!("来源销售订单 {so_no}／发货单 {}", shipment_row.ship_no);
    let new = NewStockOut {
        // 销售出库的借方是 6401 主营业务成本 —— 与 OTHER 同科目（ISSUE 也是 6401），
        // 但来源由 source_type='SALES' 表达（V0285），不新增 out_type 值。
        out_type: OUT_TYPE_OTHER,
        warehouse_id: shipment_row.warehouse_id,
        out_date: shipment_row.ship_date,
        reason: Some(&reason),
        remark: Some(&remark),
        source_type: Some(SOURCE_TYPE_SALES),
        source_id: Some(shipment_row.id),
        // 销售出库不挂成本对象（只有生产领料挂；ARCH-9 §4.7.1）
        cost_object_id: None,
    };
    let totals = Totals::of(&lines);
    let stock_out_id = stock_out::create(ex, &new, &lines, &totals, operator.clone(), now)
        .await
        .map_err(|error| write_error(error, "生成销售出库单"))?;
    let stock_out_no = stock_out::lock(ex, stock_out_id)
        .await
        .map_err(|error| utils::db_error(error, "回读销售出库单"))?
        .map(|row| row.out_no)
        .ok_or_else(|| {
            ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "销售出库单刚建好就查不到（事务内异常）",
            )
        })?;

    // ④ 出库单 → PENDING_POST（出库待确认）：账不动，等客户签收确认（M2-03）
    mark_pending_confirm(
        ex,
        stock_out_id,
        &stock_out_no,
        operator,
        &pending_confirm_reason(&shipment_row.ship_no),
        now,
    )
    .await?;

    // ⑤ 在途账（`biz_inventory_in_transit`）：发货数量进在途，**库存不扣**
    let transit = shipment_transit::record_shipped(ex, shipment_row, items, operator, now).await?;

    // ⑥ 回填发货单 → 出库单的引用（CAS：只有未回填才写）
    let affected = ex
        .exec_sql(
            "UPDATE biz_shipment \
                SET stock_out_id = ?::uuid, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND stock_out_id IS NULL AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(&stock_out_id),
                Value::from(now),
                Value::Text(operator.id.map(|id| id.to_string()).unwrap_or_default()),
                Value::from(&shipment_row.id),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "回填发货单的出库单引用"))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "发货单 {} 的出库单引用已被并发过账写入（期望为空），请刷新后重试",
                shipment_row.ship_no
            ),
        ));
    }

    // ⑦ 订单行已发数量（超发闸在 SQL 里）
    bump_delivered(ex, items, now).await?;

    // ⑧ 订单状态：PARTIAL_OUT / OUTBOUND（`from == to` 时不写状态日志）
    let (ordered, delivered) = order_totals(ex, shipment_row.so_id).await?;
    let target = order_status_after(ordered, delivered);
    let outcome = state_machine::transition_if_changed(
        ex,
        state_machine::ENTITY_SALES_ORDER,
        shipment_row.so_id,
        &so_no,
        &state_machine::SALES_ORDER_EDGES,
        &so_status,
        target,
        operator.clone(),
        None,
        Some((state_machine::ENTITY_SHIPMENT, &shipment_row.ship_no)),
    )
    .await?;
    if outcome.is_some() {
        let applied = sales_order_model::apply_status(ex, shipment_row.so_id, &so_status, target, operator.id, now)
            .await
            .map_err(|error| utils::db_error(error, "回写销售订单状态"))?;
        if applied == 0 {
            return Err(ApiError::new(
                409,
                error_code::RESOURCE_CONFLICT,
                format!("销售订单 {so_no} 的状态在本次过账期间被改动（期望 {so_status}），请刷新后重试"),
            ));
        }
    }

    tracing::info!(
        shipment_id = %shipment_row.id,
        ship_no = %shipment_row.ship_no,
        stock_out_id = %stock_out_id,
        stock_out_no = %stock_out_no,
        lines = lines.len(),
        transit_lines = transit.len(),
        order_status = target,
        "发货过账完成：在途账已记 + 出库单待确认（库存未扣）+ delivered_qty 回写已同事务落库"
    );
    Ok(PostOutcome {
        stock_out_id,
        stock_out_no,
        so_status: Some(target.to_string()),
        lines: lines.len(),
        transit,
    })
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse::<Decimal>().expect("数量字面量非法")
    }

    /// 卡面写的是 PARTIAL_SHIPPED / SHIPPED，但库层约束只认 PARTIAL_OUT / OUTBOUND
    /// （V0264 自检里专门断言 SHIPPED 必被拒）—— 这里把口径钉死。
    #[test]
    fn order_status_domain_matches_check_constraint() {
        assert_eq!(order_status_after(dec("10"), dec("0")), "PARTIAL_OUT");
        assert_eq!(order_status_after(dec("10"), dec("3")), "PARTIAL_OUT");
        assert_eq!(order_status_after(dec("10"), dec("9.9999")), "PARTIAL_OUT");
        assert_eq!(order_status_after(dec("10"), dec("10")), "OUTBOUND");
        // 浮点/精度噪音：多一点点也算发完（库层是 delivered_qty <= qty，不会真的超）
        assert_eq!(order_status_after(dec("10"), dec("10.0000")), "OUTBOUND");
        // 没有明细的订单不能判成「已出库」（否则空单会被误封成终态）
        assert_eq!(order_status_after(Decimal::ZERO, Decimal::ZERO), "PARTIAL_OUT");
    }

    /// 状态值必须与销售订单模块常量逐字一致（防手写漂移）。
    #[test]
    fn order_status_constants_are_the_module_constants() {
        assert_eq!(
            order_status_after(dec("1"), dec("1")),
            sales_order_model::STATUS_OUTBOUND
        );
        assert_eq!(
            order_status_after(dec("1"), dec("0")),
            sales_order_model::STATUS_PARTIAL_OUT
        );
    }

    /// 出库事由 / 明细备注 / 待确认原因要能反查到发货单与订单行（对账时靠它）。
    #[test]
    fn reason_and_line_remark_are_traceable() {
        assert_eq!(stock_out_reason("SH-20261106-000001"), "销售发货 SH-20261106-000001");
        assert_eq!(line_remark(3), "销售发货订单行 3");
        let reason = pending_confirm_reason("SH-20261106-000001");
        assert!(
            reason.contains("SH-20261106-000001"),
            "待确认原因必须带发货单号：{reason}"
        );
        assert!(reason.contains("签收"), "待确认原因必须说明在等签收确认：{reason}");
    }

    /// 销售出库的 out_type 必须是 OTHER + 来源 SALES（两个维度，不许互相替代）。
    #[test]
    fn sales_source_is_not_an_out_type() {
        assert_eq!(SOURCE_TYPE_SALES, "SALES");
        assert_eq!(OUT_TYPE_OTHER, "OTHER");
        assert!(
            !crate::business::erp::stockout::stockout_base::OUT_TYPES.contains(&SOURCE_TYPE_SALES),
            "SALES 是来源类型，不是 out_type 域值（out_type 只有 ISSUE/SCRAP/LOSS/OTHER）"
        );
    }

    /// 出库单落的状态必须是 `PENDING_POST`（出库待确认），不是 DRAFT、更不是 POSTED。
    #[test]
    fn stock_out_lands_in_pending_confirm_state() {
        assert_eq!(STOCK_OUT_PENDING_POST, "PENDING_POST");
        assert_eq!(STOCK_OUT_DRAFT, "DRAFT");
        assert!(
            STOCK_OUT_EDGES.contains(&(STOCK_OUT_DRAFT, STOCK_OUT_PENDING_POST)),
            "DRAFT → PENDING_POST 必须是登记过的合法边（STOCK_OUT_EDGES）"
        );
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关） ----------
    //
    // ⚠ 不设 `ERP_DB_URL` 时这些用例打印 SKIP 并 return —— libtest 记 passed，属「假绿」。
    //   交付前必须显式带库跑一遍（卡内汇报记了实测命令）。
    //
    // 覆盖卡面四项验收：
    //   ① 发货过账 → 在途记账 + 出库单待确认（三本账一分不动）；
    //   ② 部分发货 → 分批发货各自的在途行、库存仍不动；
    //   ③ 库存不足（409，在途/出库单都不许留）；
    //   ④ 重复过账（幂等，在途只记一次）；
    // 另加两条：在途占用的可用量（不扣库存也能挡住超发）、后置失败整笔回滚。

    use crate::business::erp::shipment::shipment_handler::{self, ShipmentStatusReq};
    use crate::business::erp::stockin::stockin_model;

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `SO10T-` 夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    const WAREHOUSE: &str = "d510f000-0000-4000-8000-000000000002";
    const MAT_A: &str = "d510f000-0000-4000-8000-000000000011";
    const CUSTOMER_CODE: &str = "SO10T-CUST";
    const MATS: &str = "'d510f000-0000-4000-8000-000000000011'::uuid, \
                        'd510f000-0000-4000-8000-000000000012'::uuid";

    const FIXTURES: &[&str] = &[
        "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order) \
         VALUES ('d510f000-0000-4000-8000-000000000001', 'SO10-PCS', '个', 1, NULL, 1.000000, 1, 1) \
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path, level, is_leaf, status, allow_negative) \
         VALUES ('d510f000-0000-4000-8000-000000000002', 'SO10-WH', 'SO-10 发货仓', \
                 '/d510f000-0000-4000-8000-000000000002/', 1, 1, 1, 0) \
         ON CONFLICT DO NOTHING",
        "INSERT INTO biz_material (id, material_code, material_name, unit_id, status, serial_required) \
         VALUES ('d510f000-0000-4000-8000-000000000011', 'SO10-M1', 'SO-10 物料甲', \
                 'd510f000-0000-4000-8000-000000000001', 1, false), \
                ('d510f000-0000-4000-8000-000000000012', 'SO10-M2', 'SO-10 物料乙', \
                 'd510f000-0000-4000-8000-000000000001', 1, false) \
         ON CONFLICT DO NOTHING",
    ];

    fn uuid(raw: &str) -> Uuid {
        Uuid::parse_str(raw).expect("夹具 UUID 非法")
    }

    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                // 池参数 / 建池 runtime 由夹具复用模板统一提供（P2-60）：旧写法把 `max_lifetime`
                // 与 `idle_timeout` 压成 ZERO ⇒ **每条语句**新建连接（99→145 每次 1~3 ms 握手）。
                match cloudpivot_kernel::shared::test_fixture::connect(&url).await {
                    Ok(connection) => match set_db(connection) {
                        Ok(()) => true,
                        Err(err) => {
                            println!("SKIP: set_db 失败（{err}）");
                            false
                        }
                    },
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn sql(statement: &str) {
        db().exec_sql(statement.to_string(), vec![])
            .await
            .unwrap_or_else(|error| panic!("执行失败：{error}\n{statement}"));
    }

    /// 清场：只清本模块夹具（`SO10T-` 客户 / 两张夹具物料），不碰别的用例。
    ///
    /// 在途行随 `biz_shipment` 的 `ON DELETE CASCADE` 一起消失（V0420 的硬外键），
    /// 所以不需要单独 DELETE 它 —— 但显式删一遍更稳（防止别的用例留下孤儿夹具）。
    async fn cleanup() {
        let orders = format!(
            "(SELECT id FROM biz_sales_order WHERE customer_id IN \
              (SELECT id FROM biz_customer WHERE code = '{CUSTOMER_CODE}'))"
        );
        let shipments = format!("(SELECT id FROM biz_shipment WHERE so_id IN {orders})");
        let stock_outs = format!("(SELECT id FROM biz_stock_out WHERE source_id IN {shipments})");
        sql(&format!(
            "DELETE FROM biz_voucher WHERE source_type = 'STOCK_OUT' AND source_id IN {stock_outs}"
        ))
        .await;
        sql(&format!(
            "DELETE FROM biz_status_log WHERE \
                (entity_type = 'STOCK_OUT' AND entity_id IN {stock_outs}) \
             OR (entity_type = 'SHIPMENT' AND entity_id IN {shipments}) \
             OR (entity_type = 'SALE_ORDER' AND entity_id IN {orders})"
        ))
        .await;
        sql(&format!(
            "DELETE FROM biz_inventory_in_transit WHERE shipment_id IN {shipments} \
                OR material_id IN ({MATS})"
        ))
        .await;
        sql(&format!("DELETE FROM biz_stock_out WHERE source_id IN {shipments}")).await;
        sql(&format!("DELETE FROM biz_shipment WHERE so_id IN {orders}")).await;
        sql(&format!(
            "DELETE FROM biz_sales_order WHERE customer_id IN \
                      (SELECT id FROM biz_customer WHERE code = '{CUSTOMER_CODE}')"
        ))
        .await;
        sql(&format!("DELETE FROM biz_customer WHERE code = '{CUSTOMER_CODE}'")).await;
        sql(&format!("DELETE FROM biz_inventory_txn WHERE material_id IN ({MATS})")).await;
        sql(&format!("DELETE FROM biz_inventory WHERE material_id IN ({MATS})")).await;
        sql(&format!(
            "DELETE FROM biz_inventory_location WHERE material_id IN ({MATS})"
        ))
        .await;
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        for statement in FIXTURES {
            sql(statement).await;
        }
        Some(guard)
    }

    /// 造库存（成本账 + 库位账，走**入库侧同一批写入口**；不建凭证 / 不建流水 ——
    /// 本卡只验发货侧的在途记账，凭证与流水在签收确认时才产生）。
    async fn seed_stock(material: &str, qty: &str) {
        let now = Utc::now();
        let material_id = uuid(material);
        let warehouse_id = uuid(WAREHOUSE);
        let quantity = dec(qty);
        let amount = (quantity * dec("10")).round_dp(4);
        stockin_model::apply_location_qty(db(), material_id, warehouse_id, None, quantity, now)
            .await
            .expect("库位账加法（未分配桶）");
        stockin_model::apply_cost(
            db(),
            material_id,
            warehouse_id,
            None,
            stockin_model::CostBasis::of(quantity, amount),
            now,
        )
        .await
        .expect("成本账加法");
    }

    /// 成本账数量合计（`biz_inventory`，按物料跨库位求和）。
    async fn cost_qty(material: &str) -> Decimal {
        let text: Option<String> = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory WHERE material_id = ?::uuid",
                vec![Value::from(&uuid(material))],
            )
            .await
            .expect("读成本账");
        text.and_then(|value| value.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    }

    /// 库位账数量合计（`biz_inventory_location`）。
    async fn location_qty(material: &str) -> Decimal {
        let text: Option<String> = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(qty), 0)::text FROM biz_inventory_location \
                  WHERE material_id = ?::uuid",
                vec![Value::from(&uuid(material))],
            )
            .await
            .expect("读库位账");
        text.and_then(|value| value.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    }

    /// 该物料出库流水的净额（OUT 为负、IN 为正）。
    async fn txn_net(material: &str) -> Decimal {
        let text: Option<String> = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(CASE WHEN txn_type = 'OUT' THEN -qty ELSE qty END), 0)::text \
                   FROM biz_inventory_txn WHERE material_id = ?::uuid",
                vec![Value::from(&uuid(material))],
            )
            .await
            .expect("读流水");
        text.and_then(|value| value.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    }

    /// 在途账当前在途量（`status = 'IN_TRANSIT'`，与 `shipment_transit::open_qty` 同口径）。
    async fn transit_qty(material: &str) -> Decimal {
        let text: Option<String> = db()
            .scalar_with_params(
                "SELECT COALESCE(SUM(qty_in_transit), 0)::text FROM biz_inventory_in_transit \
                  WHERE material_id = ?::uuid AND status = 'IN_TRANSIT'",
                vec![Value::from(&uuid(material))],
            )
            .await
            .expect("读在途账");
        text.and_then(|value| value.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    }

    /// 该发货单在途行数。
    async fn transit_rows(ship_id: Uuid) -> i64 {
        db().scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_inventory_in_transit WHERE shipment_id = ?::uuid",
            vec![Value::from(&ship_id)],
        )
        .await
        .expect("数在途行")
    }

    /// 出库单的凭证数（本卡口径：发货过账**不**生成凭证，签收确认才生成）。
    async fn voucher_count(ship_id: Uuid) -> i64 {
        db().scalar_with_params::<i64>(
            "SELECT count(*) FROM biz_voucher v \
               JOIN biz_stock_out o ON o.id = v.source_id \
              WHERE v.source_type = 'STOCK_OUT' AND o.source_id = ?::uuid",
            vec![Value::from(&ship_id)],
        )
        .await
        .expect("数凭证")
    }

    async fn fixture_customer() -> Uuid {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, 'SO-10 夹具客户', 100000, 'ACTIVE') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("插夹具客户");
        db().scalar_with_params::<Uuid>(
            "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("查夹具客户")
    }

    /// 一张审批通过的订单 + 一行明细（`delivered` 由用例指定，用来造并发超发场景）。
    async fn fixture_order(customer: Uuid, so_item_qty: &str, delivered: &str, status: &str) -> (Uuid, Uuid) {
        let so_id = Uuid::now_v7();
        let so_no = format!("SO10T-SO-{}", &so_id.to_string()[24..]);
        db().execute_with_params(
            "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status) \
             VALUES (?, ?, ?, CURRENT_DATE, ?)",
            vec![
                Value::from(so_id),
                Value::from(so_no),
                Value::from(customer),
                Value::from(status),
            ],
        )
        .await
        .expect("插夹具订单");
        let item_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_sales_order_item \
                (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty) \
             VALUES (?, ?, 1, ?, ?::text::numeric, 100, 0, 13, 0, ?::text::numeric)",
            vec![
                Value::from(item_id),
                Value::from(so_id),
                Value::from(&uuid(MAT_A)),
                Value::Text(so_item_qty.to_string()),
                Value::Text(delivered.to_string()),
            ],
        )
        .await
        .expect("插夹具订单行");
        (so_id, item_id)
    }

    /// 直接落一张 `PENDING_POST` 的发货单（绕过建单守卫 —— 本卡验的是**过账**，
    /// 建单守卫由 SO-09 的单测覆盖；并发超发场景也只能这样造）。
    async fn fixture_shipment(customer: Uuid, so_id: Uuid, so_item_id: Uuid, material: &str, qty: &str) -> Uuid {
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_shipment \
                (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty) \
             VALUES (?, ?, ?, ?, ?, CURRENT_DATE, 'PENDING_POST', ?::text::numeric)",
            vec![
                Value::from(id),
                Value::from(format!("SO10T-SH-{}", &id.to_string()[24..])),
                Value::from(so_id),
                Value::from(customer),
                Value::from(&uuid(WAREHOUSE)),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .expect("插夹具发货单");
        db().execute_with_params(
            "INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty) \
             VALUES (?, ?, ?, ?::text::numeric)",
            vec![
                Value::from(id),
                Value::from(so_item_id),
                Value::from(&uuid(material)),
                Value::Text(qty.to_string()),
            ],
        )
        .await
        .expect("插夹具发货明细");
        id
    }

    /// 过账（走 handler 的公开入口：状态机 + 事务 + 在途/出库待确认一条龙）。
    async fn post_shipment(ship_id: Uuid) -> std::result::Result<shipment_handler::ShipmentRes, ApiError> {
        shipment_handler::change_shipment_status(
            None,
            &ship_id.to_string(),
            ShipmentStatusReq {
                target_status: "POSTED".to_string(),
                reason: None,
            },
        )
        .await
    }

    async fn text_of(query: &str, params: Vec<Value>) -> Option<String> {
        match db().scalar_with_params::<String>(query, params).await {
            Ok(text) => Some(text),
            Err(DbError::NotFound) => None,
            Err(error) => panic!("标量查询失败：{error}\n{query}"),
        }
    }

    /// 出库单来源 / 状态 / 数量（断言用）。
    async fn stock_out_row(ship_id: Uuid) -> Option<(String, String, String, String)> {
        let rows = db()
            .query_sql(
                "SELECT status, out_type, source_type, total_qty::text AS qty \
                   FROM biz_stock_out WHERE source_id = ?::uuid AND deleted_at IS NULL"
                    .to_string(),
                vec![Value::from(ship_id)],
            )
            .await
            .expect("查出库单");
        rows.first().map(|row| {
            (
                row.get_typed::<String>("status").expect("status"),
                row.get_typed::<String>("out_type").expect("out_type"),
                row.get_typed::<String>("source_type").expect("source_type"),
                row.get_typed::<String>("qty").expect("qty"),
            )
        })
    }

    /// **正常发货**：在途记账 + 出库单待确认，三本账一分不动，订单已发完（OUTBOUND）。
    ///
    /// 这是 M2-02 的口径证据：**库存不扣**（20 还是 20），在途 +10，
    /// 出库单停在 PENDING_POST 且没有成本结转凭证。
    #[tokio::test]
    async fn normal_post_records_transit_without_touching_stock() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "0", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, MAT_A, "10").await;

        let res = post_shipment(ship_id)
            .await
            .unwrap_or_else(|error| panic!("正常过账应成功：{} {}", error.code, error.msg));
        assert_eq!(res.status, "POSTED", "发货单应推进到 POSTED");
        assert!(res.stock_out_id.is_some(), "必须回填销售出库单 id");
        assert!(
            res.stock_out_no.as_deref().is_some_and(|no| no.starts_with("OUT-")),
            "详情必须回显关联出库单号，实际 {:?}",
            res.stock_out_no
        );

        // 出库单：来源 SALES、状态 PENDING_POST（出库待确认）、数量 = 发货数量
        let (status, out_type, source_type, qty) = stock_out_row(ship_id).await.expect("必须生成销售出库单");
        assert_eq!(status, "PENDING_POST", "出库单必须停在「出库待确认」，账一分不动");
        assert_eq!(out_type, "OTHER", "out_type 是领用性质，销售出库用 OTHER（借 6401）");
        assert_eq!(source_type, "SALES", "来源类型必须是 SALES（V0285）");
        assert_eq!(qty, "10.0000", "出库单总数量应等于发货数量");
        assert_eq!(
            voucher_count(ship_id).await,
            0,
            "发货过账不生成成本结转凭证（签收确认才生成）"
        );

        // 三本账：一分不动（这是本卡最核心的一条）
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "发货过账不许扣成本账");
        assert_eq!(location_qty(MAT_A).await, dec("20"), "发货过账不许扣库位账");
        assert_eq!(txn_net(MAT_A).await, Decimal::ZERO, "发货过账不许写库存流水");

        // 在途账：一行，发货 10 / 在途 10 / IN_TRANSIT，日期快照 = 发货单日期
        assert_eq!(transit_rows(ship_id).await, 1, "一张发货单一条物料只该有一行在途");
        assert_eq!(transit_qty(MAT_A).await, dec("10"), "在途量应等于发货数量");
        let transit = text_of(
            "SELECT shipped_qty::text || '/' || received_qty::text || '/' || qty_in_transit::text || '/' || status \
               FROM biz_inventory_in_transit WHERE shipment_id = ?::uuid",
            vec![Value::from(&ship_id)],
        )
        .await
        .expect("在途行必须存在");
        assert_eq!(
            transit, "10.0000/0.0000/10.0000/IN_TRANSIT",
            "在途行口径不符：{transit}"
        );
        let same_date = text_of(
            "SELECT (t.ship_date = s.ship_date)::text FROM biz_inventory_in_transit t \
               JOIN biz_shipment s ON s.id = t.shipment_id WHERE t.shipment_id = ?::uuid",
            vec![Value::from(&ship_id)],
        )
        .await;
        assert_eq!(same_date.as_deref(), Some("true"), "在途行必须带发货日期快照");

        // 订单行已发 + 订单状态
        let delivered = text_of(
            "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
            vec![Value::from(&item_id)],
        )
        .await;
        assert_eq!(delivered.as_deref(), Some("10.0000"), "delivered_qty 必须回写");
        let so_status = text_of(
            "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(&so_id)],
        )
        .await;
        assert_eq!(so_status.as_deref(), Some("OUTBOUND"), "全发完 → OUTBOUND");
        println!("[M2-02] 正常过账：出库单待确认 + 在途 10，库存仍 20，订单 OUTBOUND");
    }

    /// **部分发货**：3/10 → PARTIAL_OUT；再发 7 → OUTBOUND；两批各自一行在途，库存始终 20。
    #[tokio::test]
    async fn partial_post_keeps_two_transit_rows_and_stock_untouched() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "0", "APPROVED").await;

        let first = fixture_shipment(customer, so_id, item_id, MAT_A, "3").await;
        post_shipment(first)
            .await
            .unwrap_or_else(|error| panic!("第一次部分发货应成功：{} {}", error.code, error.msg));
        let so_status = text_of(
            "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(&so_id)],
        )
        .await;
        assert_eq!(so_status.as_deref(), Some("PARTIAL_OUT"), "发了一部分 → PARTIAL_OUT");
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "库存不随发货扣减");
        assert_eq!(transit_qty(MAT_A).await, dec("3"), "第一批在途 3");

        let second = fixture_shipment(customer, so_id, item_id, MAT_A, "7").await;
        post_shipment(second)
            .await
            .unwrap_or_else(|error| panic!("第二次发货应成功：{} {}", error.code, error.msg));
        let so_status = text_of(
            "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(&so_id)],
        )
        .await;
        assert_eq!(so_status.as_deref(), Some("OUTBOUND"), "发完 → OUTBOUND");
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "两批发完库存仍是 20（签收确认才扣）");
        assert_eq!(location_qty(MAT_A).await, dec("20"));
        assert_eq!(txn_net(MAT_A).await, Decimal::ZERO, "没有库存流水");
        // 两张发货单 = 两行在途（唯一键含 shipment_id），合计在途 10
        assert_eq!(transit_rows(first).await, 1, "第一张发货单一行在途");
        assert_eq!(transit_rows(second).await, 1, "第二张发货单一行在途");
        assert_eq!(transit_qty(MAT_A).await, dec("10"), "在途合计 3 + 7 = 10");
        let delivered = text_of(
            "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
            vec![Value::from(&item_id)],
        )
        .await;
        assert_eq!(delivered.as_deref(), Some("10.0000"), "累计已发 3 + 7 = 10");
    }

    /// **库存不足整单回滚**：账只有 2，要发 5 → 409，且在途 / 出库单 / delivered 都不许留。
    #[tokio::test]
    async fn insufficient_stock_leaves_everything_untouched() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        seed_stock(MAT_A, "2").await;
        let (so_id, item_id) = fixture_order(customer, "10", "0", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, MAT_A, "5").await;

        let error = post_shipment(ship_id).await.expect_err("库存只有 2，发 5 必须被拒");
        assert_eq!(error.status, 409, "库存不足应是 409，实际 {}", error.status);
        assert!(
            error.msg.contains("库存不足"),
            "文案应说明库存不足，实际：{}",
            error.msg
        );

        assert_eq!(cost_qty(MAT_A).await, dec("2"), "成本账不能被扣");
        assert_eq!(location_qty(MAT_A).await, dec("2"), "库位账不能被扣");
        assert_eq!(txn_net(MAT_A).await, Decimal::ZERO, "不能留流水");
        assert_eq!(transit_rows(ship_id).await, 0, "被拒的发货单不能留下在途行");
        assert!(stock_out_row(ship_id).await.is_none(), "拒绝过账不能留下半张销售出库单");
        assert_eq!(
            text_of(
                "SELECT status FROM biz_shipment WHERE id = ?::uuid",
                vec![Value::from(&ship_id)]
            )
            .await
            .as_deref(),
            Some("PENDING_POST"),
            "发货单必须仍停在 PENDING_POST"
        );
        assert_eq!(
            text_of(
                "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
                vec![Value::from(&item_id)]
            )
            .await
            .as_deref(),
            Some("0.0000"),
            "delivered_qty 不能被回写"
        );
    }

    /// **在途占住可用量**：库存不扣，但已发的货必须从可用量里扣掉 ——
    /// 否则账面一直是满的，同一批货能被反复发出去（M2-02 的核心副作用）。
    #[tokio::test]
    async fn in_transit_reserves_available_quantity() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "30", "0", "APPROVED").await;

        // 先发 15：可用 20 → 通过（在途 15，账面仍 20）
        let first = fixture_shipment(customer, so_id, item_id, MAT_A, "15").await;
        post_shipment(first)
            .await
            .unwrap_or_else(|error| panic!("第一批发 15 应成功：{} {}", error.code, error.msg));
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "账面仍是 20");
        assert_eq!(transit_qty(MAT_A).await, dec("15"), "在途 15");

        // 再发 10：账面 20 够，但扣掉在途 15 只剩 5 → 409
        let second = fixture_shipment(customer, so_id, item_id, MAT_A, "10").await;
        let error = post_shipment(second)
            .await
            .expect_err("在途已经占了 15，可用只剩 5，再发 10 必须被拒");
        assert_eq!(error.status, 409, "应是在途占用导致的库存不足，实际 {}", error.status);
        assert!(
            error.msg.contains("库存不足") && error.msg.contains("在途"),
            "文案要点明「在途占用后的可用量」，实际：{}",
            error.msg
        );
        assert_eq!(transit_rows(second).await, 0, "被拒的发货单不许留在途行");
        assert_eq!(transit_qty(MAT_A).await, dec("15"), "在途仍是 15");

        // 再发 5：可用 5 = 5 → 通过（边界）
        let third = fixture_shipment(customer, so_id, item_id, MAT_A, "5").await;
        post_shipment(third)
            .await
            .unwrap_or_else(|error| panic!("在可用量边界发 5 应成功：{} {}", error.code, error.msg));
        assert_eq!(transit_qty(MAT_A).await, dec("20"), "在途 15 + 5 = 20");
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "签收确认前账面一直是 20");
    }

    /// **重复过账幂等**：第二次 POSTED → 409，在途只记一次，销售出库单只有一张。
    #[tokio::test]
    async fn repeated_post_is_idempotent() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "0", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, MAT_A, "4").await;

        post_shipment(ship_id).await.expect("第一次过账应成功");
        assert_eq!(transit_qty(MAT_A).await, dec("4"), "第一次在途 4");

        let error = post_shipment(ship_id).await.expect_err("已 POSTED 再次过账必须被拒");
        assert_eq!(error.status, 409, "重复过账应 409，实际 {}", error.status);
        assert!(
            error.msg.contains("终态") || error.msg.contains("不能"),
            "文案应说明已终态，实际：{}",
            error.msg
        );

        assert_eq!(transit_qty(MAT_A).await, dec("4"), "在途不能被记两遍");
        assert_eq!(transit_rows(ship_id).await, 1, "在途行只有一行");
        assert_eq!(cost_qty(MAT_A).await, dec("20"), "库存始终没动");
        let count = text_of(
            "SELECT count(*)::text FROM biz_stock_out WHERE source_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&ship_id)],
        )
        .await;
        assert_eq!(count.as_deref(), Some("1"), "同一发货单只能有一张销售出库单");
        assert_eq!(
            text_of(
                "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
                vec![Value::from(&item_id)]
            )
            .await
            .as_deref(),
            Some("4.0000"),
            "delivered_qty 不能被加两遍"
        );
    }

    /// **后置失败整笔回滚**：出库单与在途行已经写了，但订单行超发守卫拦下 →
    /// 出库单 / 在途行 / 发货单状态全部回滚。
    ///
    /// 这是「原子性」的真正证据：失败点在**写完在途之后**，回滚必须把它们一起消失。
    #[tokio::test]
    async fn late_failure_rolls_back_the_whole_posting() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        seed_stock(MAT_A, "20").await;
        // 订单行 qty=10，但已被并发发掉 8（绕过建单守卫造场景）：再发 5 就会超发
        let (so_id, item_id) = fixture_order(customer, "10", "8", "PARTIAL_OUT").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, MAT_A, "5").await;

        let error = post_shipment(ship_id)
            .await
            .expect_err("并发超发必须在回写 delivered_qty 时被拦下");
        assert_eq!(error.status, 409, "超发应 409，实际 {}", error.status);
        assert!(
            error.msg.contains("已发满") || error.msg.contains("超发"),
            "文案应点明超发，实际：{}",
            error.msg
        );

        assert_eq!(cost_qty(MAT_A).await, dec("20"), "库存没动过（本来就不扣）");
        assert_eq!(transit_rows(ship_id).await, 0, "写进去的在途行必须回滚");
        assert_eq!(transit_qty(MAT_A).await, Decimal::ZERO, "在途一分不许留");
        assert!(stock_out_row(ship_id).await.is_none(), "半张销售出库单必须回滚");
        assert_eq!(
            text_of(
                "SELECT status FROM biz_shipment WHERE id = ?::uuid",
                vec![Value::from(&ship_id)]
            )
            .await
            .as_deref(),
            Some("PENDING_POST"),
            "发货单状态必须回滚到 PENDING_POST"
        );
        assert_eq!(
            text_of(
                "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
                vec![Value::from(&item_id)]
            )
            .await
            .as_deref(),
            Some("8.0000"),
            "delivered_qty 不能被改"
        );
        assert_eq!(
            text_of(
                "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
                vec![Value::from(&so_id)]
            )
            .await
            .as_deref(),
            Some("PARTIAL_OUT"),
            "订单状态不能被推进"
        );
    }
}

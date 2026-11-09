// ============================================================================
// shipment_posting.rs —— 发货过账的库存效果（SO-10：发货 POSTED → 销售出库 → 扣库存）
//
// # 一个事务里做完四件事（顺序不能换）
//
// ```text
// ① 防御性幂等：biz_shipment.stock_out_id 已回填 → 409（本函数永不重复扣）
// ② 锁来源订单行（SELECT … FOR UPDATE）：同一订单的多张发货单并发过账在这里串行化，
//    否则两张单各自读到 APPROVED，订单状态会被后写的那张覆盖回退
// ③ 建销售出库单（biz_stock_out, source_type='SALES', out_type='OTHER'）
//    → 调 `stockout_handler::post_out_in_tx` 过账：负库存闸 → 三本账负向 → 成本结转凭证 → 联动
// ④ 回填 biz_shipment.stock_out_id → 订单行 delivered_qty += qty → 订单转 PARTIAL_OUT / OUTBOUND
// ```
//
// **库存三本账一行 SQL 都不在本文件**：扣减 / 库位账 / 流水全部由
// `stockout_handler::post_out_in_tx` → `stockout_model` → `inventory_writer` 完成，
// 本文件只建单、只回写业务列。这是「唯一写入口」铁律的落地方式（scripts/check.sh 也会查）。
//
// # 为什么复用出库过账内核，而不是自己 deduct
//
// 出库过账已经付费解决了四件难事：① 负库存闸必须锁住成本账行再判（并发下先读后写会双过账）；
// ② 库位策略「指定就按指定扣，没指定 FIFO」；③ 按物料合并的成本结转凭证；④ 跨单据联动。
// 另写一份等于把四件事重新错一遍。
//
// # 原子性怎么保证
//
// 全部动作跑在**调用方（`shipment_handler::change_shipment_status`）的同一个事务**里：
// 任何一步 Err 都由调用方 `rollback()`，库存 / 库位账 / 流水 / 出库单 / delivered_qty /
// 订单状态一起消失。本文件自己没有 `commit()`，也不该有。
//
// # 幂等
//
// 三道闸：① 发货单 CAS（`apply_status` 只在 `status=PENDING_POST` 时改，第二次 0 行）；
// ② `biz_shipment.stock_out_id` 回填 CAS（第二次 0 行）；③ 库层部分唯一索引
// `uk_biz_so_source(source_type, source_id)`（并发下最后一道，撞了就是 409）。
// ============================================================================

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::sales_order_model;
use crate::business::erp::shipment_model::{self as shipment, SHIPPABLE_SO_STATUSES, ShipmentItemRow, ShipmentRow};
use crate::business::erp::stockout_base::{NewStockOut, NewStockOutLine, OUT_TYPE_OTHER, SOURCE_TYPE_SALES, Totals};
use crate::business::erp::stockout_handler;
use crate::business::erp::stockout_model::{self as stock_out, MaterialInfo};
use crate::business::flow::linkage;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

/// 过账成功后的业务结果（供 handler 打日志 / 回显）。
#[derive(Debug)]
pub struct PostOutcome {
    /// 新生成的销售出库单 id（已过账）。
    pub stock_out_id: Uuid,
    /// 出库单号（`OUT-yyyyMMdd-000001`）。
    pub stock_out_no: String,
    /// 过账后的销售订单状态（`PARTIAL_OUT` / `OUTBOUND`；订单状态没变时为 None）。
    pub so_status: Option<String>,
    /// 写进销售出库单的明细行数。
    pub lines: usize,
    /// 出库过账的联动结果：`deferred` 里的动作必须在**提交之后**跑。
    pub linkage: linkage::LinkageRun,
}

/// 出库单摘要（出库列表 / 详情一眼能看出这是销售发货）。
pub fn stock_out_reason(ship_no: &str) -> String {
    format!("销售发货 {ship_no}")
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
            format!("{ctx}：该发货单已经生成过销售出库单（幂等闸），不会重复扣库存"),
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
/// 两套 id 不能混用（混了就是扣错桶）。该库位没有这个物料的账行 → 409：
/// 指定了库位就必须从那个库位出，不能静默改走 FIFO。
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
        // 不合并也能过账，但「发货 2 行 → 出库 2 行」会让 FIFO 对同一桶做两次够量判定，
        // 部分库位实现下会对同一桶重复分配（stockout_fifo_tests 踩过）。
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

/// 回写订单行 `delivered_qty`（**超发闸在 SQL 里**：`delivered_qty + qty <= qty`）。
///
/// 返回 0 行 = 该行已发满 / 并发被别人发掉 → 409，由调用方整单回滚
/// （这时库存已经扣了，回滚把它还回去 —— 这正是「原子」的意思）。
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
                     本单的库存与出库单已整笔回滚，请刷新订单后重试",
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
/// 返回 [`PostOutcome`]；`Ok` 后调用方 `commit()`，再跑
/// `stockout_handler::run_deferred_linkage_after_commit`。
pub async fn post_effects(
    ex: &impl SqlExecutor,
    shipment_row: &ShipmentRow,
    items: &[ShipmentItemRow],
    operator: &state_machine::Operator,
    now: DateTime<Utc>,
) -> std::result::Result<PostOutcome, ApiError> {
    // ① 防御性幂等：已回填过出库单 = 已经动过账，绝不再扣第二遍
    if let Some(existing) = shipment_row.stock_out_id {
        return Err(ApiError::new(
            409,
            error_code::RESOURCE_CONFLICT,
            format!(
                "发货单 {} 已经生成过销售出库单 {existing}（过账幂等闸）：\
                 库存只扣一次，重复过账请查单据而不是重发",
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

    // ④ 过账：负库存闸 → 三本账负向 → 成本结转凭证 → 联动（同一事务）
    let run = stockout_handler::post_out_in_tx(ex, stock_out_id, operator, Some(&reason), now).await?;

    // ⑤ 回填发货单 → 出库单的引用（CAS：只有未回填才写）
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

    // ⑥ 订单行已发数量（超发闸在 SQL 里）
    bump_delivered(ex, items, now).await?;

    // ⑦ 订单状态：PARTIAL_OUT / OUTBOUND（`from == to` 时不写状态日志）
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
        order_status = target,
        "发货过账完成：销售出库单 + 三本账负向 + delivered_qty 回写已同事务落库"
    );
    Ok(PostOutcome {
        stock_out_id,
        stock_out_no,
        so_status: Some(target.to_string()),
        lines: lines.len(),
        linkage: run,
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

    /// 出库事由 / 明细备注要能反查到发货单与订单行（对账时靠它）。
    #[test]
    fn reason_and_line_remark_are_traceable() {
        assert_eq!(stock_out_reason("SH-20261106-000001"), "销售发货 SH-20261106-000001");
        assert_eq!(line_remark(3), "销售发货订单行 3");
    }

    /// 销售出库的 out_type 必须是 OTHER + 来源 SALES（两个维度，不许互相替代）。
    #[test]
    fn sales_source_is_not_an_out_type() {
        assert_eq!(SOURCE_TYPE_SALES, "SALES");
        assert_eq!(OUT_TYPE_OTHER, "OTHER");
        assert!(
            !crate::business::erp::stockout_base::OUT_TYPES.contains(&SOURCE_TYPE_SALES),
            "SALES 是来源类型，不是 out_type 域值（out_type 只有 ISSUE/SCRAP/LOSS/OTHER）"
        );
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关） ----------
    //
    // ⚠ 不设 `ERP_DB_URL` 时这些用例打印 SKIP 并 return —— libtest 记 passed，属「假绿」。
    //   交付前必须显式带库跑一遍（卡内汇报记了实测命令）。
    //
    // 覆盖卡面四项验收：正常发货（全发→OUTBOUND）/ 部分发货（→PARTIAL_OUT→再次发货→OUTBOUND）/
    // 库存不足（409，账一分不动）/ 重复过账（幂等，只扣一次）；另加一条「后置失败整笔回滚」。

    use crate::business::erp::shipment_handler::{self, ShipmentStatusReq};
    use crate::business::erp::stockin_model;

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
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
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
    /// 本卡只验出库侧，凭证与流水在出库过账时才产生）。
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

    /// 过账（走 handler 的公开入口：状态机 + 事务 + 库存效果一条龙）。
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

    /// 出库单来源 / 状态 / 金额（断言用）。
    async fn stock_out_row(ship_id: Uuid) -> Option<(String, String, String, Option<String>, String)> {
        let rows = db()
            .query_sql(
                "SELECT status, out_type, source_type, voucher_id::text AS voucher, total_qty::text AS qty \
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
                row.get_typed::<Option<String>>("voucher").expect("voucher"),
                row.get_typed::<String>("qty").expect("qty"),
            )
        })
    }

    /// 出库流水的业务类型（FIX-7：销售出库必须是 `SALE_OUT`，不许是 `OTHER_OUT`）。
    async fn stock_out_txn_biz_type(ship_id: Uuid) -> Option<String> {
        text_of(
            "SELECT t.biz_type FROM biz_inventory_txn t \
               JOIN biz_stock_out s ON s.id = t.doc_id \
              WHERE s.source_id = ?::uuid AND t.doc_type = 'STOCK_OUT' AND t.txn_type = 'OUT' \
              LIMIT 1",
            vec![Value::from(ship_id)],
        )
        .await
    }

    /// **正常发货**：全量发货 → 三本账负向 + 成本结转凭证 + delivered_qty + 订单 OUTBOUND。
    #[tokio::test]
    async fn normal_post_deducts_three_books_and_closes_order() {
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

        let (status, out_type, source_type, voucher, qty) = stock_out_row(ship_id).await.expect("必须生成销售出库单");
        assert_eq!(status, "POSTED", "销售出库单必须在一个事务里过账");
        assert_eq!(out_type, "OTHER", "out_type 是领用性质，销售出库用 OTHER（借 6401）");
        assert_eq!(source_type, "SALES", "来源类型必须是 SALES（V0285）");
        assert_eq!(qty, "10.0000", "出库单总数量应等于发货数量");
        let voucher = voucher.expect("有成本（10×10=100）时必须生成成本结转凭证");
        assert!(!voucher.is_empty());

        // 三本账：成本账 / 库位账各减 10，流水一笔 OUT −10（净额 = −10）
        assert_eq!(cost_qty(MAT_A).await, dec("10"), "成本账应 20 − 10 = 10");
        assert_eq!(location_qty(MAT_A).await, dec("10"), "库位账应 20 − 10 = 10");
        assert_eq!(txn_net(MAT_A).await, dec("-10"), "流水净额应为 −10");
        // FIX-7：销售出库流水的 biz_type 必须是 SALE_OUT（记成 OTHER_OUT 的话
        // 「按 biz_type 统计销售出库」一条都数不到，与手工其他出库混在一起）
        assert_eq!(
            stock_out_txn_biz_type(ship_id).await.as_deref(),
            Some("SALE_OUT"),
            "销售出库流水必须记 SALE_OUT（source_type='SALES' ⇒ 不看 out_type 的 OTHER）"
        );

        // 订单行已发 + 订单状态
        let delivered = text_of(
            "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
            vec![Value::from(item_id)],
        )
        .await;
        assert_eq!(delivered.as_deref(), Some("10.0000"), "delivered_qty 必须回写");
        let so_status = text_of(
            "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(so_id)],
        )
        .await;
        assert_eq!(so_status.as_deref(), Some("OUTBOUND"), "全发完 → OUTBOUND");
        println!("[SO-10] 正常过账：单据 {ship_id} → 出库单 {voucher}，三账各 −10，订单 OUTBOUND");
    }

    /// **部分发货**：3/10 → PARTIAL_OUT；再发 7 → OUTBOUND（两次累计 = 10）。
    #[tokio::test]
    async fn partial_post_moves_to_partial_out_then_outbound() {
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
            vec![Value::from(so_id)],
        )
        .await;
        assert_eq!(so_status.as_deref(), Some("PARTIAL_OUT"), "发了一部分 → PARTIAL_OUT");
        assert_eq!(cost_qty(MAT_A).await, dec("17"), "库存应 20 − 3 = 17");

        let second = fixture_shipment(customer, so_id, item_id, MAT_A, "7").await;
        post_shipment(second)
            .await
            .unwrap_or_else(|error| panic!("第二次发货应成功：{} {}", error.code, error.msg));
        let so_status = text_of(
            "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
            vec![Value::from(so_id)],
        )
        .await;
        assert_eq!(so_status.as_deref(), Some("OUTBOUND"), "发完 → OUTBOUND");
        assert_eq!(cost_qty(MAT_A).await, dec("10"), "库存应 17 − 7 = 10");
        assert_eq!(location_qty(MAT_A).await, dec("10"));
        assert_eq!(txn_net(MAT_A).await, dec("-10"), "两笔出库流水合计 −10");
        let delivered = text_of(
            "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
            vec![Value::from(item_id)],
        )
        .await;
        assert_eq!(delivered.as_deref(), Some("10.0000"), "累计已发 3 + 7 = 10");
    }

    /// **库存不足整单回滚**：账只有 2，要发 5 → 409，且三账 / 出库单 / delivered_qty 一分不动。
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
        assert!(stock_out_row(ship_id).await.is_none(), "拒绝过账不能留下半张销售出库单");
        assert_eq!(
            text_of(
                "SELECT status FROM biz_shipment WHERE id = ?::uuid",
                vec![Value::from(ship_id)]
            )
            .await
            .as_deref(),
            Some("PENDING_POST"),
            "发货单必须仍停在 PENDING_POST"
        );
        assert_eq!(
            text_of(
                "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
                vec![Value::from(item_id)]
            )
            .await
            .as_deref(),
            Some("0.0000"),
            "delivered_qty 不能被回写"
        );
    }

    /// **重复过账幂等**：第二次 POSTED → 409，库存只扣一次，销售出库单只有一张。
    #[tokio::test]
    async fn repeated_post_is_idempotent() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        seed_stock(MAT_A, "20").await;
        let (so_id, item_id) = fixture_order(customer, "10", "0", "APPROVED").await;
        let ship_id = fixture_shipment(customer, so_id, item_id, MAT_A, "4").await;

        post_shipment(ship_id).await.expect("第一次过账应成功");
        assert_eq!(cost_qty(MAT_A).await, dec("16"), "第一次扣 4");

        let error = post_shipment(ship_id).await.expect_err("已 POSTED 再次过账必须被拒");
        assert_eq!(error.status, 409, "重复过账应 409，实际 {}", error.status);
        assert!(
            error.msg.contains("终态") || error.msg.contains("不能"),
            "文案应说明已终态，实际：{}",
            error.msg
        );

        assert_eq!(cost_qty(MAT_A).await, dec("16"), "第二次过账不能再扣库存");
        assert_eq!(location_qty(MAT_A).await, dec("16"));
        assert_eq!(txn_net(MAT_A).await, dec("-4"), "流水只有一笔 −4");
        let count = text_of(
            "SELECT count(*)::text FROM biz_stock_out WHERE source_id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(ship_id)],
        )
        .await;
        assert_eq!(count.as_deref(), Some("1"), "同一发货单只能有一张销售出库单");
        assert_eq!(
            text_of(
                "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
                vec![Value::from(item_id)]
            )
            .await
            .as_deref(),
            Some("4.0000"),
            "delivered_qty 不能被加两遍"
        );
    }

    /// **后置失败整笔回滚**：库存已经扣了，但订单行超发守卫拦下 → 库存 / 出库单 / 状态全部回滚。
    ///
    /// 这是「原子性」的真正证据：失败点在**扣完库存之后**，回滚必须把它还回去。
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

        assert_eq!(cost_qty(MAT_A).await, dec("20"), "扣掉的库存必须回滚");
        assert_eq!(location_qty(MAT_A).await, dec("20"), "库位账必须回滚");
        assert_eq!(txn_net(MAT_A).await, Decimal::ZERO, "流水必须回滚");
        assert!(stock_out_row(ship_id).await.is_none(), "半张销售出库单必须回滚");
        assert_eq!(
            text_of(
                "SELECT status FROM biz_shipment WHERE id = ?::uuid",
                vec![Value::from(ship_id)]
            )
            .await
            .as_deref(),
            Some("PENDING_POST"),
            "发货单状态必须回滚到 PENDING_POST"
        );
        assert_eq!(
            text_of(
                "SELECT delivered_qty::text FROM biz_sales_order_item WHERE id = ?::uuid",
                vec![Value::from(item_id)]
            )
            .await
            .as_deref(),
            Some("8.0000"),
            "delivered_qty 不能被改"
        );
        assert_eq!(
            text_of(
                "SELECT status FROM biz_sales_order WHERE id = ?::uuid",
                vec![Value::from(so_id)]
            )
            .await
            .as_deref(),
            Some("PARTIAL_OUT"),
            "订单状态不能被推进"
        );
    }
}

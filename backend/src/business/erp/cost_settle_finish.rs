// ============================================================================
// cost_settle_finish.rs —— 结算 ①：完工入库（ARCH-9 §4.7.4 / §4.7.6 / §4.8 ③ · COST-B3-4）
//
// # 本模块负责 §4.8 事务体的哪几步
//
// ```text
// ③ 逐成本对象（本文件）
//    a. INSERT biz_stock_in (in_type='PRODUCTION', status='POSTED',
//                            cost_object_id, cost_period_id)      ← §4.7.6「不走收货过账路径」
//    b. INSERT biz_stock_in_item (qty=completed_qty,
//                                 cost_price=unit_finished_cost,
//                                 amount=finished_cost, lot_no)   ← §4.7.6「入库单价」行
//    c. inventory_writer 三本账                                  ← 唯一写入口（见下）
//    d. UPDATE biz_cost_period_item
//         SET stock_in_id=?, status='SETTLED'                     ← 计算单 ↔ 入库单绑定
//    e. biz_cost_object.lot_no 有值 → upsert biz_lot
//         (source_type='PRODUCTION')                              ← 批次主数据（第三本账）
// ```
//
// ④⑤⑥（三张凭证 / 批次状态与合计 / 事务内 6 条断言）是 B3-5 / B3-6 的卡，本文件一行不碰。
//
// # 为什么入库单**不**走 `stockin_handler::post_in_tx`
//
// 那条路径是**采购收货**的过账事务：它有采购单状态回写、「入库即生成应付单」
// （`payable::create_for_stock_in`）与应付科目（借 1405 + 借进项税 / 贷 2202）。
// 完工入库没有供应商、没有应付、没有进项税，硬走会凭空生成一张应付单 —— 账直接错。
// 所以本模块只复用它的**内核**：三本账全部经
// `stockin_model::apply_cost` / `apply_location_qty` → `inventory_writer`
// （唯一写入口；本文件一行 `biz_inventory*` SQL 都没有，
//  `scripts/verify/inventory_write_gate.py` 在 CI 里钉这一点）。
//
// # 三本账同时更新（卡面验收逐条对应）
//
// | 账 | 写什么 | 谁写 |
// |---|---|---|
// | 成本账 `biz_inventory` | `qty += completed_qty`、`amount += finished_cost`、`avg_cost` 重算 | `apply_cost` → `inventory_writer::add_cost` |
// | 库位账 `biz_inventory_location` | `qty += completed_qty`（按成本对象的完工入库库位行） | `apply_location_qty` → `inventory_writer::add_location_qty` |
// | 批次账 `biz_lot` | 生产批（`source_type='PRODUCTION'`，来源 = 结算批次） | [`upsert_lot`] |
// | 流水 `biz_inventory_txn` | `txn_type='IN'`、`biz_type='PRODUCTION_IN'`、`doc_type='STOCK_IN'` | `inventory_writer::insert_txn` |
//
// 前三本**同事务、同数量、同金额**：任何一本写失败都由调用方回滚整笔结算
// （本文件自己没有 `commit()`，也不该有）。
//
// # 一张入库单 = 一个成本对象
//
// §4.8 ③a 是「逐成本对象 INSERT biz_stock_in」：一个成本对象就是一个生产批、一个产出品，
// 合成一张多行单会让「这批货是哪张入库单收的」在单据维度上失真（红冲也要按对象回冲）。
// 所以本模块对一个期间产出 N 张入库单（每张 1 行明细），全部共指同一个 `cost_period_id`。
// **不能**用 `source_type='COST_PERIOD'` 表达来源：`uk_biz_si_source_doc (source_type, source_id)`
// （V0305）是**唯一**索引，同期间第二张单就会撞键 —— 来源列是 `cost_period_id`（V0376）。
//
// # 幂等（两道闸，都为「绝不重复入库」）
//
// ① 批次级：该 `cost_period_id` 已有**非作废**入库单 ⇒ 409 + `66074`，一张不写；
//    （`CANCELLED` 不算 —— 红冲作废后设计允许重新结算，见 §4.8「红冲后回到未结算」）
// ② 计算单级：`UPDATE … WHERE status='CALC' AND stock_in_id IS NULL` 的 CAS，
//    并发下第二次影响 0 行 ⇒ 409 + `66074`，整笔回滚。
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_writer::{self, TxnRecord};
use crate::business::erp::stockin_base::{AppliedInventory, CostBasis, DOC_TYPE_STOCK_IN, TXN_TYPE_IN};
use crate::business::erp::stockin_model;
use crate::business::shared::doc_kind::TxnBizType;
use crate::business::shared::state_machine;
use crate::shared::error_code;
use crate::utils;

// ==================== 常量（与迁移逐字对齐，单测钉死） ====================

/// 完工入库单的 `in_type`（V0376 把 `ck_biz_si_type` 从 4 值放宽到 5 值）。
pub const IN_TYPE_PRODUCTION: &str = "PRODUCTION";

/// 入库单状态：结算直接落 `POSTED`（§4.8 ③a），无草稿态。
pub const STOCK_IN_POSTED: &str = "POSTED";

/// 计算单：已结算（`ck_biz_cost_item_settled` 要求此时必须有 `stock_in_id`）。
pub const ITEM_SETTLED: &str = "SETTLED";

/// 计算单：仅计算（待结算行只认这个状态 + `stock_in_id IS NULL`）。
pub const ITEM_CALC: &str = "CALC";

/// 入库单已作废（红冲时由 B3-7 写；本模块的幂等闸**排除**它，否则红冲后永远不能重算）。
pub const STOCK_IN_CANCELLED: &str = "CANCELLED";

/// 批次主数据来源类型：生产批（V0370 `ck_biz_lot_source`）。
pub const LOT_SOURCE_PRODUCTION: &str = "PRODUCTION";

/// 批次主数据的来源单据类型（V0370 `source_doc_type` 注释点名 `COST_PERIOD`）。
pub const LOT_DOC_TYPE_COST_PERIOD: &str = "COST_PERIOD";

/// 库存流水的业务类型（`biz_inventory_txn.biz_type`）。
///
/// **直接取词表枚举**而不是手写字符串：`PRODUCTION_IN` 在 `TxnBizType` 里早就有
/// （`ProductionIn`），写串字会有「改词表漏改这里」的漂移风险；`as_str()` 是 `const fn`，
/// 所以这里能是编译期常量（单测另把字面值与 `doc_kind` 一起钉住）。
pub const BIZ_TYPE_PRODUCTION_IN: &str = TxnBizType::ProductionIn.as_str();

// ==================== 结构 ====================

/// 一条「待完工入库」的计算单行 + 它落库需要的主数据。
///
/// `product_id` 取**计算单自己的** `product_id`（V0373 冗余列，防成本对象中途改品），
/// 仓库 / 库位 / 批号取成本对象（那是「这批货入到哪」的事实）。
#[derive(Debug, Clone)]
pub struct FinishLine {
    /// `biz_cost_period_item.id`。
    pub item_id: Uuid,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号（写进入库单备注，人也看得懂）。
    pub object_no: String,
    /// 产出品（= `biz_stock_in_item.material_id`）。
    pub product_id: Uuid,
    /// 计量单位（`biz_material.unit_id`；空 = 主数据缺失，直接报错不猜）。
    pub unit_id: Option<Uuid>,
    /// 完工入库仓（`biz_cost_object.target_warehouse_id`，非空）。
    pub warehouse_id: Uuid,
    /// 完工入库库位（`biz_cost_object.target_location_id`，**已是** `biz_inventory_location.id`）。
    pub location_id: Option<Uuid>,
    /// 批号（可空：成本对象没填批号就不建批次主数据）。
    pub lot_no: Option<String>,
    /// 完工数量（= `completed_qty`，> 0）。
    pub qty: Decimal,
    /// 完工成本（= `finished_cost`，权威金额）。
    pub amount: Decimal,
    /// 完工单位成本（= `unit_finished_cost`，→ `stock_in_item.cost_price`）。
    pub unit_cost: Decimal,
}

/// 结算上下文（批次信息 + 会计日期 + 操作人）。
#[derive(Debug, Clone)]
pub struct SettleContext<'a> {
    /// 结算批次 id（写进 `biz_stock_in.cost_period_id`）。
    pub period_id: Uuid,
    /// 期间码 `yyyy-MM`（库存流水的 `period_code`：结算的流水落**会计期间**，不落当天）。
    pub period_code: &'a str,
    /// 批次号 `CS-yyyyMM-NNNN`（写进备注 / 批次主数据来源单号）。
    pub batch_no: &'a str,
    /// 入库日期（一般 = 期间末或结算日）。
    pub in_date: NaiveDate,
    /// 操作人（`posted_by` / `create_by` / 状态日志）。
    pub operator: Option<Uuid>,
    /// 业务时间。
    pub now: DateTime<Utc>,
}

/// 一张已生成的完工入库单（B3-6 要用它回写批次合计 + 事务内对平）。
#[derive(Debug, Clone)]
pub struct FinishStockIn {
    /// `biz_stock_in.id`。
    pub stock_in_id: Uuid,
    /// `biz_stock_in.in_no`（`IN-yyyyMMdd-NNNNNN`）。
    pub in_no: String,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 绑定的计算单。
    pub item_id: Uuid,
    /// 入库数量。
    pub qty: Decimal,
    /// 入库金额。
    pub amount: Decimal,
}

impl FinishStockIn {
    /// 本批入库合计（数量, 金额）—— 与完工结转凭证借方、`biz_cost_period.total_finished` 对平。
    pub fn totals(rows: &[FinishStockIn]) -> (Decimal, Decimal) {
        rows.iter().fold((Decimal::ZERO, Decimal::ZERO), |(qty, amount), row| {
            (qty + row.qty, amount + row.amount)
        })
    }
}

// ==================== 读：待完工入库的计算单 ====================

/// 该批次里「已算出完工、但还没入库」的计算单行（按对象号排序，结果稳定）。
///
/// 判据三列一起用（少一列都会重复入库或漏入库）：
/// * `status = 'CALC'` —— 已 `SETTLED` 的行有入库单，不能再收一次；
/// * `stock_in_id IS NULL` —— 防御「状态没改但指针已写」的中间态；
/// * `completed_qty > 0` —— 完工 0 件不建空入库单（`ck_biz_si_item_qty` 也要求 `qty > 0`）。
pub async fn pending_lines(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<Vec<FinishLine>> {
    let rows = ex
        .query_sql(
            "SELECT i.id            AS item_id, \
                    i.cost_object_id, \
                    i.product_id, \
                    i.completed_qty, \
                    i.finished_cost, \
                    i.unit_finished_cost, \
                    o.object_no, \
                    o.target_warehouse_id, \
                    o.target_location_id, \
                    o.lot_no, \
                    m.unit_id \
               FROM biz_cost_period_item i \
               JOIN biz_cost_object o ON o.id = i.cost_object_id AND o.deleted_at IS NULL \
               LEFT JOIN biz_material m ON m.id = i.product_id AND m.deleted_at IS NULL \
              WHERE i.period_id = ?::uuid \
                AND i.status = 'CALC' \
                AND i.stock_in_id IS NULL \
                AND i.completed_qty > 0 \
              ORDER BY o.object_no ASC, i.id ASC"
                .to_string(),
            vec![Value::from(&period_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(FinishLine {
                item_id: row.get_typed("item_id")?,
                cost_object_id: row.get_typed("cost_object_id")?,
                object_no: row.get_typed("object_no")?,
                product_id: row.get_typed("product_id")?,
                unit_id: row.get_typed("unit_id")?,
                warehouse_id: row.get_typed("target_warehouse_id")?,
                location_id: row.get_typed("target_location_id")?,
                lot_no: row.get_typed("lot_no")?,
                qty: row.get_typed("completed_qty")?,
                amount: row.get_typed("finished_cost")?,
                unit_cost: row.get_typed("unit_finished_cost")?,
            })
        })
        .collect()
}

/// 该批次已生成的入库单 id（红冲 / 对平 / 巡检都要按批次反查，#4.7.6 的 `cost_period_id` 索引）。
pub async fn stock_in_ids_of(ex: &impl SqlExecutor, period_id: Uuid) -> DbResult<Vec<Uuid>> {
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_stock_in \
              WHERE cost_period_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY in_no ASC, id ASC"
                .to_string(),
            vec![Value::from(&period_id)],
        )
        .await?;
    rows.iter().map(|row| row.get_typed("id")).collect()
}

// ==================== 写：完工入库 ====================

/// **结算 ③**：逐成本对象生成完工入库单 + 明细 + 三本账 + 回写计算单。
///
/// 返回生成的入库单（一张 per 成本对象）；本期**没有完工数量**时返回空 `Vec`（不报错 ——
/// 那是合法的「本期无产出」，由 B3-6 决定批次怎么落）。
///
/// 调用方（B3-6）必须：
/// * 先把批次行 `SELECT … FOR UPDATE` 锁住、校验 `status='APPROVED'`、期间未结账；
/// * 在**同一个事务**里调用本函数，随后写三张凭证与批次状态；
/// * 不要自己再 commit —— 本文件只落语句，事务边界归调用方。
pub async fn generate(
    ex: &impl SqlExecutor,
    ctx: &SettleContext<'_>,
) -> std::result::Result<Vec<FinishStockIn>, ApiError> {
    ensure_not_settled(ex, ctx).await?;
    let lines = pending_lines(ex, ctx.period_id)
        .await
        .map_err(|error| utils::db_error(error, "结算：读待完工入库的计算单"))?;
    let mut done = Vec::with_capacity(lines.len());
    for line in &lines {
        done.push(write_one(ex, ctx, line).await?);
    }
    Ok(done)
}

/// 幂等闸 ①：该批次已有**非作废**入库单 ⇒ 409 + `66074`（红冲作废的行不算，允许重算）。
async fn ensure_not_settled(ex: &impl SqlExecutor, ctx: &SettleContext<'_>) -> std::result::Result<(), ApiError> {
    let existing: i64 = ex
        .query_first_sql(
            "SELECT count(*)::bigint AS n FROM biz_stock_in \
              WHERE cost_period_id = ?::uuid AND deleted_at IS NULL AND status <> ?"
                .to_string(),
            vec![Value::from(&ctx.period_id), Value::Text(STOCK_IN_CANCELLED.to_string())],
        )
        .await
        .map_err(|error| utils::db_error(error, "结算：查该批次是否已生成完工入库单"))?
        .map(|row| row.get_typed::<i64>("n"))
        .transpose()?
        .unwrap_or(0);
    if existing > 0 {
        return Err(ApiError::new(
            409,
            error_code::ERP_COST_FINISH_ALREADY_SETTLED,
            format!(
                "结算批次 {} 已经生成过 {existing} 张完工入库单，不会重复入库（要重算请先红冲该批次）",
                ctx.batch_no
            ),
        ));
    }
    Ok(())
}

/// 一个成本对象的完工入库（单头 + 明细 + 三本账 + 计算单回写 + 批次主数据）。
async fn write_one(
    ex: &impl SqlExecutor,
    ctx: &SettleContext<'_>,
    line: &FinishLine,
) -> std::result::Result<FinishStockIn, ApiError> {
    // 计量单位来自物料主数据；缺了就是主数据坏了 —— 不猜、不落 0（`unit_id` 是 NOT NULL）
    let unit_id = line.unit_id.ok_or_else(|| {
        ApiError::new(
            400,
            error_code::PARAM_INVALID,
            format!(
                "成本对象 {} 的产出品 {} 缺少计量单位（物料主数据不存在或已删除），无法生成完工入库明细",
                line.object_no, line.product_id
            ),
        )
    })?;

    let in_no = stockin_model::next_in_no(ex, ctx.now)
        .await
        .map_err(|error| utils::db_error(error, "结算：取完工入库单号"))?;
    let id = Uuid::now_v7();
    let qty = line.qty.round_dp(4);
    let amount = line.amount.round_dp(4);
    let unit_cost = line.unit_cost.round_dp(6);
    let operator_text = Value::Text(ctx.operator.map(|value| value.to_string()).unwrap_or_default());
    let remark = format!("完工入库 · 结算批次 {} · 成本对象 {}", ctx.batch_no, line.object_no);

    // ---------- a. 单头（直接 POSTED：结算没有草稿态，§4.8 ③a） ----------
    // `source_order_id` / `supplier_id` 恒 NULL、`is_sourceless` 恒 FALSE：
    // V0382 起的 `ck_biz_si_sourceless_pair` 给 PRODUCTION 单开了这条分支
    // （完工入库的来源是 `cost_period_id`，不是采购单）。
    ex.exec_sql(
        "INSERT INTO biz_stock_in \
            (id, in_no, in_type, source_order_id, supplier_id, is_sourceless, warehouse_id, in_date, \
             status, total_qty, total_amount, total_tax, total_with_tax, invoice_received, \
             cost_object_id, cost_period_id, remark, posted_at, posted_by, \
             created_at, updated_at, create_by, update_by) \
         VALUES (?, ?, ?, NULL, NULL, FALSE, ?::uuid, ?::text::date, \
             ?, ?::text::numeric, ?::text::numeric, 0, ?::text::numeric, FALSE, \
             ?::uuid, ?::uuid, ?, ?, NULLIF(?::text,'')::uuid, \
             ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::Text(in_no.clone()),
            Value::Text(IN_TYPE_PRODUCTION.to_string()),
            Value::from(&line.warehouse_id),
            Value::Text(ctx.in_date.to_string()),
            Value::Text(STOCK_IN_POSTED.to_string()),
            Value::Text(qty.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
            Value::from(&line.cost_object_id),
            Value::from(&ctx.period_id),
            Value::Text(remark.clone()),
            Value::from(ctx.now),
            operator_text.clone(),
            Value::from(ctx.now),
            Value::from(ctx.now),
            operator_text.clone(),
            operator_text.clone(),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "结算：生成完工入库单"))?;

    // ---------- b. 明细（1 行：qty / 金额 / 入库单价 / 批号） ----------
    // `price` / `price_incl` / `cost_price` 三列同值（完工入库没有税，不含税 = 含税 = 成本单价），
    // `tax_rate = 0`、`tax_amount = 0` ⇒ 自动满足 `ck_biz_si_item_tax`（with_tax = amount + tax）。
    ex.exec_sql(
        "INSERT INTO biz_stock_in_item \
            (stock_in_id, line_no, material_id, unit_id, qty, price, price_incl, tax_rate, \
             amount, tax_amount, with_tax_amount, cost_price, location_id, lot_no, remark, \
             created_at, updated_at) \
         VALUES (?::uuid, 1, ?::uuid, ?::uuid, ?::text::numeric, ?::text::numeric, ?::text::numeric, 0, \
             ?::text::numeric, 0, ?::text::numeric, ?::text::numeric, NULLIF(?::text,'')::uuid, \
             NULLIF(?::text,''), NULLIF(?::text,''), ?, ?)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(&line.product_id),
            Value::from(&unit_id),
            Value::Text(qty.to_string()),
            Value::Text(unit_cost.to_string()),
            Value::Text(unit_cost.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(amount.to_string()),
            Value::Text(unit_cost.to_string()),
            Value::Text(line.location_id.map(|value| value.to_string()).unwrap_or_default()),
            Value::Text(line.lot_no.clone().unwrap_or_default()),
            Value::Text(remark.clone()),
            Value::from(ctx.now),
            Value::from(ctx.now),
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "结算：生成完工入库明细"))?;

    // ---------- c. 三本账（唯一写入口：成本账 → 库位账 → 流水，锁顺序固定） ----------
    let applied: AppliedInventory = stockin_model::apply_cost(
        ex,
        line.product_id,
        line.warehouse_id,
        line.location_id,
        CostBasis { qty, amount, unit_cost },
        ctx.now,
    )
    .await
    .map_err(|error| utils::db_error(error, "结算：成本账加法"))?;
    stockin_model::apply_location_qty(ex, line.product_id, line.warehouse_id, line.location_id, qty, ctx.now)
        .await
        .map_err(|error| utils::db_error(error, "结算：库位账加法"))?;
    // 流水：`biz_type` 是本卡自己的 `PRODUCTION_IN`（`stockin_write::write_txn` 写死
    // `PURCHASE_IN`，用它会把这批完工货记成采购入库 —— 「货从哪来」给假答案）。
    // `period_code` 显式给结算期间：流水要落在**会计期间**，不是「今天」。
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id: line.product_id,
            warehouse_id: line.warehouse_id,
            txn_type: TXN_TYPE_IN,
            biz_type: BIZ_TYPE_PRODUCTION_IN,
            doc_type: DOC_TYPE_STOCK_IN,
            doc_id: id,
            doc_no: &in_no,
            line_no: 1,
            qty,
            unit_cost,
            amount,
            before: applied.before,
            after: applied.after,
            operator: ctx.operator,
            period_code: Some(ctx.period_code),
            remark: Some("完工入库"),
            now: ctx.now,
        },
    )
    .await
    .map_err(|error| utils::db_error(error, "结算：写库存流水"))?;

    // ---------- d. 计算单回写（CAS：只认 CALC + 未绑定） ----------
    let affected = ex
        .exec_sql(
            "UPDATE biz_cost_period_item \
                SET stock_in_id = ?::uuid, status = ?, updated_at = ? \
              WHERE id = ?::uuid AND status = ? AND stock_in_id IS NULL"
                .to_string(),
            vec![
                Value::from(&id),
                Value::Text(ITEM_SETTLED.to_string()),
                Value::from(ctx.now),
                Value::from(&line.item_id),
                Value::Text(ITEM_CALC.to_string()),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "结算：回写计算单入库单指针"))?;
    if affected == 0 {
        return Err(ApiError::new(
            409,
            error_code::ERP_COST_FINISH_ALREADY_SETTLED,
            format!(
                "成本对象 {} 的计算单已被其它事务结算（状态不是 CALC 或已绑定入库单），本次结算作废",
                line.object_no
            ),
        ));
    }

    // ---------- e. 批次主数据（有批号才建；已存在同物料同批号则不覆盖） ----------
    upsert_lot(ex, ctx, line).await?;

    // ---------- f. 状态日志（`null → POSTED`，「建单留痕」；同事务，失败整笔回滚） ----------
    let log_reason = format!("成本结算自动生成（批次 {}）", ctx.batch_no);
    state_machine::log_created(
        ex,
        state_machine::ENTITY_STOCK_IN,
        id,
        &in_no,
        STOCK_IN_POSTED,
        ctx.operator,
        Some(log_reason.as_str()),
    )
    .await
    .map_err(|error| ApiError::new(error.status, error.code, error.msg))?;

    Ok(FinishStockIn {
        stock_in_id: id,
        in_no,
        cost_object_id: line.cost_object_id,
        item_id: line.item_id,
        qty,
        amount,
    })
}

/// 批次账：把成本对象的批号登记进 `biz_lot`（`source_type='PRODUCTION'`）。
///
/// **不覆盖已有行**（`ON CONFLICT … DO NOTHING`）：同一个 (物料, 批号) 可能是采购批先建的，
/// 也可能有人手工维护过有效期 / 质检状态 —— 结算不能把人工维护的档案冲掉。
/// 批号为空 ⇒ 什么都不做（成本对象没填批号就不该凭空造一个）。
async fn upsert_lot(
    ex: &impl SqlExecutor,
    ctx: &SettleContext<'_>,
    line: &FinishLine,
) -> std::result::Result<(), ApiError> {
    let Some(lot_no) = line.lot_no.as_deref().map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(());
    };
    let operator_text = Value::Text(ctx.operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        "INSERT INTO biz_lot \
            (id, lot_no, material_id, source_type, source_doc_type, source_doc_id, source_doc_no, \
             prod_date, qc_status, status, remark, created_at, updated_at, create_by, update_by) \
         VALUES (?, ?, ?::uuid, ?, ?, ?::uuid, ?, ?::text::date, 'NA', 1, ?, ?, ?, \
             NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
         ON CONFLICT (material_id, lot_no) WHERE deleted_at IS NULL DO NOTHING"
            .to_string(),
        vec![
            Value::from(Uuid::now_v7()),
            Value::Text(lot_no.to_string()),
            Value::from(&line.product_id),
            Value::Text(LOT_SOURCE_PRODUCTION.to_string()),
            Value::Text(LOT_DOC_TYPE_COST_PERIOD.to_string()),
            Value::from(&ctx.period_id),
            Value::Text(ctx.batch_no.to_string()),
            Value::Text(ctx.in_date.to_string()),
            Value::Text(format!("结算批次 {} 自动登记的生产批", ctx.batch_no)),
            Value::from(ctx.now),
            Value::from(ctx.now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await
    .map_err(|error| utils::db_error(error, "结算：登记生产批批次主数据"))?;
    Ok(())
}

// ==================== 单测（纯逻辑 / 常量对齐；真库用例见 cost_settle_finish_db_tests） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 迁移原文 —— 常量必须与库层定义逐字对齐（改迁移不改常量 ⇒ 这里先红）。
    const V0376_UP: &str = include_str!("../../../migrations/business/V0376__stock_in_production.up.sql");
    const V0382_UP: &str = include_str!("../../../migrations/business/V0382__stock_in_production_sourceless.up.sql");

    #[test]
    fn production_in_type_is_what_the_migrations_allow() {
        assert_eq!(IN_TYPE_PRODUCTION, "PRODUCTION", "ck_biz_si_type 的第五个值");
        // V0376 把值域放宽到 5 值；漏了 PRODUCTION 就写不进完工入库单
        assert!(V0376_UP.contains("ck_biz_si_type"));
        assert!(
            V0376_UP.contains("'PURCHASE','RETURN','OTHER','SOURCELESS','PRODUCTION'"),
            "V0376 的 ck_biz_si_type 必须含 PRODUCTION"
        );
        // ck_biz_si_production 的四要素（本模块每条都满足：对象 + 批次非空、无供应商、非无源）
        assert!(V0376_UP.contains("cost_object_id IS NOT NULL"));
        assert!(V0376_UP.contains("cost_period_id IS NOT NULL"));
        assert!(V0376_UP.contains("supplier_id IS NULL"));
        assert!(V0376_UP.contains("is_sourceless = FALSE"));
    }

    #[test]
    fn sourceless_pair_is_relaxed_for_production() {
        // 本模块写的单头是「无采购单 + 非无源」，只有 V0382 的分支才放行；
        // 迁移被回滚 / 被改回单分支，这里立刻红（比真库报 23514 早得多）。
        assert!(
            V0382_UP.contains("WHEN in_type = 'PRODUCTION'"),
            "V0382 必须给 PRODUCTION 单开分支"
        );
        assert!(V0382_UP.contains("source_order_id IS NULL"));
        assert!(V0382_UP.contains("is_sourceless = FALSE"));
        assert!(
            V0382_UP.contains("ELSE (is_sourceless = (source_order_id IS NULL))"),
            "存量语义必须原样保留"
        );
    }

    #[test]
    fn txn_biz_type_matches_the_vocabulary() {
        // 手写串 vs 词表枚举：两边必须同字面，且枚举真的存在这条（防「改词表漏改这里」）
        assert_eq!(BIZ_TYPE_PRODUCTION_IN, TxnBizType::ProductionIn.as_str());
        assert_eq!(BIZ_TYPE_PRODUCTION_IN, "PRODUCTION_IN");
        // 这条流水造成的单据是入库单 —— E4 净额判据按单据轴解释，不能记到别的 doc_kind
        assert_eq!(
            TxnBizType::ProductionIn.doc_kind(),
            crate::business::shared::doc_kind::DocKind::StockIn
        );
    }

    #[test]
    fn doc_type_and_direction_are_the_shared_constants() {
        assert_eq!(DOC_TYPE_STOCK_IN, "STOCK_IN");
        assert_eq!(TXN_TYPE_IN, "IN", "完工入库是入库方向（qty 恒正，方向看 txn_type）");
    }

    #[test]
    fn item_statuses_match_v0373_checks() {
        const V0373_UP: &str = include_str!("../../../migrations/business/V0373__cost_period_tables.up.sql");
        assert_eq!(ITEM_CALC, "CALC");
        assert_eq!(ITEM_SETTLED, "SETTLED");
        assert!(
            V0373_UP.contains("'CALC','SETTLED'"),
            "ck_biz_cost_item_status 只认两值"
        );
        // 库层要求 SETTLED 必须绑入库单 —— 本模块先写 stock_in 再回写，顺序不能反
        assert!(V0373_UP.contains("status <> 'SETTLED' OR stock_in_id IS NOT NULL"));
    }

    #[test]
    fn lot_source_values_match_v0370() {
        const V0370_UP: &str = include_str!("../../../migrations/business/V0370__cost_lot_and_cost_object.up.sql");
        assert_eq!(LOT_SOURCE_PRODUCTION, "PRODUCTION");
        assert_eq!(LOT_DOC_TYPE_COST_PERIOD, "COST_PERIOD");
        assert!(V0370_UP.contains("'PURCHASE','PRODUCTION','OPENING','MANUAL'"));
        assert!(V0370_UP.contains("COST_PERIOD"), "source_doc_type 注释点名 COST_PERIOD");
    }

    #[test]
    fn totals_sum_qty_and_amount() {
        let rows = vec![
            FinishStockIn {
                stock_in_id: Uuid::now_v7(),
                in_no: "IN-1".into(),
                cost_object_id: Uuid::now_v7(),
                item_id: Uuid::now_v7(),
                qty: Decimal::new(8000000, 4),
                amount: Decimal::new(223111104, 4),
            },
            FinishStockIn {
                stock_in_id: Uuid::now_v7(),
                in_no: "IN-2".into(),
                cost_object_id: Uuid::now_v7(),
                item_id: Uuid::now_v7(),
                qty: Decimal::new(1500000, 4),
                amount: Decimal::new(300000, 4),
            },
        ];
        let (qty, amount) = FinishStockIn::totals(&rows);
        assert_eq!(qty, Decimal::new(9500000, 4), "800 + 150 = 950");
        assert_eq!(amount, Decimal::new(223411104, 4), "22311.1104 + 30.0000");
        assert_eq!(
            FinishStockIn::totals(&[]),
            (Decimal::ZERO, Decimal::ZERO),
            "无产出 ⇒ 合计 0（B3-6 回写 total_finished 用）"
        );
    }
}

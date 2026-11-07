// ============================================================================
// transfer_settle.rs —— 过账结算：借调出/调入两段库存与库位账搬迁、序列号转移、流水、不变量断言
//
// 由 `transfer_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::stockin_model;
use crate::business::erp::unit_guard;

use super::inventory_writer::{self, CostDelta, TxnRecord};
use super::stockin_base::InventorySnapshot;
use super::transfer_base::*;

/// 库存流水的单据类型（`biz_inventory_txn.doc_type`，varchar(24)）。
pub const DOC_TYPE_TRANSFER: &str = "STOCK_TRANSFER";

/// 调出流水的 `biz_type`（`txn_type = 'OUT'`）。
pub const BIZ_TYPE_TRANSFER_OUT: &str = "TRANSFER_OUT";

/// 调入流水的 `biz_type`（`txn_type = 'IN'`）。
pub const BIZ_TYPE_TRANSFER_IN: &str = "TRANSFER_IN";

/// `uk_biz_inv_mat_wh_loc` 里「整仓汇总桶」的占位值（`location_id IS NULL` 等价于全零 UUID）。
pub const BUCKET_LOCATION_SQL: &str = "'00000000-0000-0000-0000-000000000000'::uuid";

/// 结算失败原因（handler 翻成 400 / 409；不是「500 + 看不出哪里不对」）。
#[derive(Debug)]
pub enum SettleError {
    /// 数据库异常（唯一键 / CHECK / 外键 / 连接）。
    Db(DbError),
    /// 调出仓库存不足：条件扣减返回 0 行（并发超卖或账面本来就不够）。
    Insufficient {
        /// 物料编码（报错点名人看得懂的东西，不是 UUID）。
        material_code: String,
        /// 本次需要调出多少。
        required: Decimal,
        /// 该库存行当前可用多少（行不存在 = 0）。
        available: Decimal,
    },
    /// 一物一码物料：明细数量与序列号个数不一致（建单后被改库 / 历史脏单）。
    SerialMismatch {
        /// 明细行号。
        line_no: i32,
        /// 明细数量。
        qty: Decimal,
        /// 该行关联的序列号个数。
        serials: i64,
    },
    /// 序列号当前不在调出仓 / 不是 `IN_STOCK`（被别人先挪走了）。
    SerialNotInSource {
        /// 明细行号。
        line_no: i32,
        /// 应有几个。
        expected: i64,
        /// 实际转移成功几个。
        moved: u64,
    },
    /// 不变式校验失败（数量 != 在库序列号件数 / 成本不守恒 / 出现负库存）。
    Invariant(String),
    /// 单据没有明细，没什么可结算的。
    EmptyItems,
    /// 明细单位不是基本单位（DC-09 补口 · 缺口 1）：结算会把 `qty` 直接写进两本账，
    /// 折算单位（箱/打）的单必须在这里拦下。载荷是 `unit_guard` 已经组好的 `ApiError`
    /// （`400 + ERP_UNIT_NOT_BASE` + 折算建议），handler 原样透出，文案只此一份。
    UnitGuard(ApiError),
    /// 跨单据联动（LNK-01）的关键动作失败：引擎已经组好「哪个规则、哪个动作、为什么」的
    /// `ApiError`，handler 原样透出并整笔回滚（不允许「状态改了、联动没跑」的半条账）。
    Linkage(ApiError),
}

impl From<DbError> for SettleError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

/// 结算的返回类型（`alun`/`nik` 的 `DbResult` 只有一种错误，这里要多一种）。
pub type SettleResult<T> = std::result::Result<T, SettleError>;

/// 可空 UUID → 参数（`Value::Null` 绑 `?::uuid` 会在驱动侧编码失败，统一走「空串 + NULLIF」）。
pub(super) fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 一条待结算的明细（连表带出 `serial_required` 与这行的序列号）。
#[derive(Debug, Clone)]
pub struct SettleLine {
    /// 明细主键。
    pub item_id: Uuid,
    /// 行号。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码（报错文案用）。
    pub material_code: String,
    /// 调出库位（`None` = 整仓汇总桶）。
    pub from_location_id: Option<Uuid>,
    /// 调入库位（`None` = 整仓汇总桶）。
    pub to_location_id: Option<Uuid>,
    /// 数量（> 0）。
    pub qty: Decimal,
    /// 是否一物一码物料。
    pub serial_required: bool,
    /// 建单时选定的序列号。
    pub serial_ids: Vec<Uuid>,
}

/// 结算结果。
#[derive(Debug, Clone)]
pub struct SettleOutcome {
    /// `false` = 本单之前已经结算过（流水已在），本次幂等跳过，库存未二次改动。
    pub settled: bool,
    /// 结算了几行明细。
    pub lines: usize,
    /// 转移数量合计。
    pub qty: Decimal,
    /// 转移金额合计（= 调出仓 amount 减少总量 = 调入仓 amount 增加总量）。
    pub amount: Decimal,
    /// 跟着走的序列号件数。
    pub serials: u64,
}

/// 调出仓扣减的回读快照（流水的前后六列 + 原价转移用的成本）。
#[derive(Debug, Clone, Copy)]
pub(super) struct OutSnapshot {
    before_qty: Decimal,
    after_qty: Decimal,
    unit_cost: Decimal,
    amount: Decimal,
    after_amount: Decimal,
}

/// 调入仓增加的回读快照。
#[derive(Debug, Clone, Copy)]
pub(super) struct InSnapshot {
    before_qty: Decimal,
    before_amount: Decimal,
    before_cost: Decimal,
    after_qty: Decimal,
    after_amount: Decimal,
    after_cost: Decimal,
}

/// 一条库存流水的全部字段（避免 `too_many_arguments`，也不用 `#[allow]`）。
pub(super) struct StockTxn<'a> {
    transfer_id: Uuid,
    transfer_no: &'a str,
    line: &'a SettleLine,
    warehouse_id: Uuid,
    txn_type: &'a str,
    biz_type: &'a str,
    qty: Decimal,
    unit_cost: Decimal,
    amount: Decimal,
    before_qty: Decimal,
    before_amount: Decimal,
    before_cost: Decimal,
    after_qty: Decimal,
    after_amount: Decimal,
    after_cost: Decimal,
    operator: Option<Uuid>,
}

/// `IN_TRANSIT → DONE` 的库存双写 + 成本原价转移（**调用方负责事务边界**）。
///
/// # 顺序（同事务，任一步失败整笔回滚）
///
/// 1. 幂等闸：本单已有 `doc_type='STOCK_TRANSFER'` 的流水 → 直接返回 `settled = false`；
/// 2. 读明细（含 `serial_required` 与序列号），**先把「序列号个数 == 数量」校验掉**；
/// 3. 逐行：调出仓条件扣减（`AND qty >= ?`）→ **调出仓库位账 -qty** → 调入仓加库存（不存在则建档）
///    → **调入仓库位账 +qty** → 序列号移仓 → 回填明细；
/// 4. 回写单据头 `total_amount`；
/// 5. 不变式断言（负库存 / 一物一码件数 / 成本守恒 / **库位账合计 == 成本账 qty**），
///    不满足即报错回滚。
///
/// 第 3 步的库位账两笔走 [`stockin_model::apply_location_qty`]（与收货过账同一个原语，
/// 同一套「有成行按主键、无成行落未分配桶」分支），且**成本账先、库位账后**，
/// 与收货并发时锁顺序一致（PRD §3.2.6）。缺了这两笔就是本卡修的缺口：
/// 调拨每结算一次，`SUM(biz_inventory_location.qty) = SUM(biz_inventory.qty)`（巡检 B1，
/// BLOCKER）就在调出仓/调入仓两侧同时变红。
///
/// # 异常情况
///
/// 见 [`SettleError`]：库存不足 / 序列号不符 → 由 handler 翻成 409 / 400。
pub async fn settle_transfer(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
    transfer_no: &str,
    from_warehouse_id: Uuid,
    to_warehouse_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> SettleResult<SettleOutcome> {
    // ① 幂等闸：流水只增不改，「本单已有流水」就是「已经结算过」的唯一事实
    let posted: i64 = ex
        .scalar_sql(
            "SELECT COUNT(*) FROM biz_inventory_txn \
              WHERE doc_type = ? AND doc_id = ?::uuid"
                .to_string(),
            vec![Value::from(DOC_TYPE_TRANSFER), Value::from(&transfer_id)],
        )
        .await?;
    if posted > 0 {
        tracing::info!(
            transfer_id = %transfer_id,
            transfer_no = %transfer_no,
            txn_rows = posted,
            "调拨单已结算过，跳过库存双写（幂等）"
        );
        return Ok(SettleOutcome {
            settled: false,
            lines: 0,
            qty: Decimal::ZERO,
            amount: Decimal::ZERO,
            serials: 0,
        });
    }

    let mut lines = load_settle_lines(ex, transfer_id).await?;
    if lines.is_empty() {
        return Err(SettleError::EmptyItems);
    }
    // ①a 单位复检（DC-09 补口 · 缺口 1）：结算会把 `qty` 直接写进两本账。
    //     调拨明细**不存 `unit_id`**（`biz_stock_transfer_item` 没有这一列），
    //     所以判据落在「物料当前的计量单位」上 —— 与建单期 `prepare_items` 同一判据、
    //     同一文案（`unit_guard::ensure_base_unit_in`），只是执行时机挪到了写账前：
    //     升级前建的折算单位草稿 / 绕过接口把物料改成折算单位的单据，在这里拦下，
    //     整笔事务回滚（单据仍停在 `IN_TRANSIT`，两本账 0 变化）。
    //     位置在 `lines` 排序之前：`index` 此时就是明细的行序（与 `items[i]` 对齐）。
    for (index, line) in lines.iter().enumerate() {
        unit_guard::ensure_base_unit_in(ex, line.material_id, index, line.qty)
            .await
            .map_err(SettleError::UnitGuard)?;
    }
    // ①b 锁顺序（PRD §3.2.6）：明细按 `material_id` 升序处理（同物料再按行号）。
    //     `load_settle_lines` 是按 `line_no` 读的，而建单时的物料顺序由用户决定；
    //     不重排就会与收货过账（`stockin_handler` 按 `material_id` 升序排好）在
    //     「成本账 → 库位账」两级锁上反向推进，两张单并发动同一组物料会交叉等待。
    lines.sort_by(|a, b| {
        a.material_id
            .cmp(&b.material_id)
            .then(a.line_no.cmp(&b.line_no))
    });
    // ② 先校验再动库存：`ck_biz_transfer_item_qty` 保证 qty > 0，所以一物一码物料
    //    没有序列号一定是错的（先扣库存再报 400 就是白扣一轮再回滚）
    for line in &lines {
        if !line.serial_required {
            continue;
        }
        let serials = i64::try_from(line.serial_ids.len()).unwrap_or(i64::MAX);
        if serials <= 0 || line.qty != Decimal::from(serials) {
            return Err(SettleError::SerialMismatch {
                line_no: line.line_no,
                qty: line.qty,
                serials,
            });
        }
    }

    let mut total_qty = Decimal::ZERO;
    let mut total_amount = Decimal::ZERO;
    let mut moved_serials = 0_u64;

    for line in &lines {
        // ③ 调出仓：锁行（或锁唯一键上的那一行）→ 条件扣减 → 回读快照
        let out_row = resolve_inventory_row(
            ex,
            line.material_id,
            from_warehouse_id,
            line.from_location_id,
        )
        .await?;
        let Some(out_row) = out_row else {
            return Err(SettleError::Insufficient {
                material_code: line.material_code.clone(),
                required: line.qty,
                available: Decimal::ZERO,
            });
        };
        let Some(out) = deduct_inventory_row(ex, out_row, line.qty, now).await? else {
            let available = current_qty(ex, out_row).await?.unwrap_or(Decimal::ZERO);
            return Err(SettleError::Insufficient {
                material_code: line.material_code.clone(),
                required: line.qty,
                available,
            });
        };
        tracing::debug!(
            transfer_no = %transfer_no,
            line_no = line.line_no,
            qty = %line.qty,
            unit_cost = %out.unit_cost,
            amount = %out.amount,
            "调出仓扣减完成"
        );
        // ③b 调出仓**库位账**：与成本账同一事务同步减（口径镜像收货过账的 ③→③b：
        //     成本账先、库位账后；明细已在上面的 ①b 按 `material_id` 升序排好，
        //     锁顺序就是 PRD §3.2.6 的「material_id 升序 → 成本账 → 库位账」，
        //     与收货并发不会交叉等待）。
        //     传负数走 `apply_location_qty` 的加法原语：`Some(行 id)` 按主键 UPDATE，
        //     `None` 落「未分配库位桶」，与调入侧完全对称。
        let out_location = stockin_model::apply_location_qty(
            ex,
            line.material_id,
            from_warehouse_id,
            line.from_location_id,
            -line.qty,
            now,
        )
        .await?;
        tracing::debug!(
            transfer_no = %transfer_no,
            line_no = line.line_no,
            qty = %line.qty,
            location_before = %out_location.before_qty,
            location_after = %out_location.after_qty,
            "调出仓库位账扣减完成"
        );
        post_stock_txn(
            ex,
            &StockTxn {
                transfer_id,
                transfer_no,
                line,
                warehouse_id: from_warehouse_id,
                txn_type: "OUT",
                biz_type: BIZ_TYPE_TRANSFER_OUT,
                qty: line.qty,
                unit_cost: out.unit_cost,
                amount: out.amount,
                before_qty: out.before_qty,
                before_amount: out.after_amount + out.amount,
                before_cost: out.unit_cost,
                after_qty: out.after_qty,
                after_amount: out.after_amount,
                after_cost: out.unit_cost,
                operator,
            },
            now,
        )
        .await?;

        // ④ 调入仓：加库存（行不存在就建档）+ 移动加权重算 avg_cost
        let ins = credit_inventory_row(
            ex,
            &CreditReq {
                material_id: line.material_id,
                warehouse_id: to_warehouse_id,
                location_id: line.to_location_id,
                qty: line.qty,
                unit_cost: out.unit_cost,
                amount: out.amount,
            },
            now,
        )
        .await?;
        // ④b 调入仓**库位账**：+qty（明细指定了库位就加那一行，没有就落「未分配库位桶」）。
        //     放在成本账之后，与收货过账的锁顺序一致（成本账 → 库位账）。
        let in_location = stockin_model::apply_location_qty(
            ex,
            line.material_id,
            to_warehouse_id,
            line.to_location_id,
            line.qty,
            now,
        )
        .await?;
        tracing::debug!(
            transfer_no = %transfer_no,
            line_no = line.line_no,
            qty = %line.qty,
            location_before = %in_location.before_qty,
            location_after = %in_location.after_qty,
            "调入仓库位账加计完成"
        );
        post_stock_txn(
            ex,
            &StockTxn {
                transfer_id,
                transfer_no,
                line,
                warehouse_id: to_warehouse_id,
                txn_type: "IN",
                biz_type: BIZ_TYPE_TRANSFER_IN,
                qty: line.qty,
                unit_cost: out.unit_cost,
                amount: out.amount,
                before_qty: ins.before_qty,
                before_amount: ins.before_amount,
                before_cost: ins.before_cost,
                after_qty: ins.after_qty,
                after_amount: ins.after_amount,
                after_cost: ins.after_cost,
                operator,
            },
            now,
        )
        .await?;

        // ⑤ 序列号跟着走（一物一码物料；按数量管理的物料一个序列号都不动）
        if line.serial_required {
            let expected = i64::try_from(line.serial_ids.len()).unwrap_or(i64::MAX);
            let moved = move_serials(
                ex,
                &line.serial_ids,
                from_warehouse_id,
                to_warehouse_id,
                line.to_location_id,
                operator,
                now,
            )
            .await?;
            if moved != u64::try_from(expected).unwrap_or(0) {
                return Err(SettleError::SerialNotInSource {
                    line_no: line.line_no,
                    expected,
                    moved,
                });
            }
            moved_serials += moved;
        }

        // ⑥ 回填明细：已发 / 已收 + 原价（金额 = 数量 × 原价，成本守恒的另一半证据）
        ex.exec_sql(
            format!(
                "UPDATE {ITEM_TABLE} \
                    SET shipped_qty = qty, received_qty = qty, \
                        unit_cost = NULLIF(?::text,'')::numeric, \
                        amount = NULLIF(?::text,'')::numeric, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid"
            ),
            vec![
                Value::Text(out.unit_cost.to_string()),
                Value::Text(out.amount.to_string()),
                Value::from(now),
                uuid_text(operator),
                Value::from(&line.item_id),
            ],
        )
        .await?;

        total_qty += line.qty;
        total_amount += out.amount;
    }

    // ⑦ 单据头金额合计（`ck_biz_transfer_total` 要求 >= 0；这里按明细实际转移金额重算）
    ex.exec_sql(
        format!(
            "UPDATE {TRANSFER_TABLE} AS t \
                SET total_amount = COALESCE((SELECT SUM(i.amount) FROM {ITEM_TABLE} i \
                                              WHERE i.transfer_id = t.id), 0), \
                    updated_at = ? \
              WHERE t.id = ?::uuid"
        ),
        vec![Value::from(now), Value::from(&transfer_id)],
    )
    .await?;

    // ⑧ 不变式断言（同事务末尾）：任何一条不满足都整笔回滚
    assert_settle_invariants(ex, transfer_id).await?;

    Ok(SettleOutcome {
        settled: true,
        lines: lines.len(),
        qty: total_qty,
        amount: total_amount,
        serials: moved_serials,
    })
}

/// 读待结算明细（连 `biz_material` 取 `material_code` / `serial_required`，序列号按行分组）。
pub(super) async fn load_settle_lines(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
) -> DbResult<Vec<SettleLine>> {
    let rows = ex
        .query_sql(
            format!(
                "SELECT i.id, i.line_no, i.material_id, m.material_code, m.serial_required, \
                        i.from_location_id, i.to_location_id, i.qty::numeric(18,4) AS qty \
                   FROM {ITEM_TABLE} i \
                   JOIN biz_material m ON m.id = i.material_id \
                  WHERE i.transfer_id = ?::uuid \
                  ORDER BY i.line_no ASC"
            ),
            vec![Value::from(&transfer_id)],
        )
        .await?;
    let mut lines = Vec::with_capacity(rows.len());
    for row in &rows {
        lines.push(SettleLine {
            item_id: row.get_typed("id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            from_location_id: row.get_typed("from_location_id")?,
            to_location_id: row.get_typed("to_location_id")?,
            qty: row.get_typed("qty")?,
            serial_required: row.get_typed("serial_required")?,
            serial_ids: Vec::new(),
        });
    }
    if lines.is_empty() {
        return Ok(lines);
    }
    // 序列号：一行一条记录，Rust 侧按 item_id 归组（不用数组列，驱动解不了 _text / _uuid）
    let serial_rows = ex
        .query_sql(
            format!(
                "SELECT l.item_id, l.serial_id \
                   FROM {ITEM_SERIAL_TABLE} l \
                   JOIN {ITEM_TABLE} i ON i.id = l.item_id \
                  WHERE i.transfer_id = ?::uuid \
                  ORDER BY l.item_id, l.created_at ASC"
            ),
            vec![Value::from(&transfer_id)],
        )
        .await?;
    for row in &serial_rows {
        let item_id: Uuid = row.get_typed("item_id")?;
        let serial_id: Uuid = row.get_typed("serial_id")?;
        if let Some(line) = lines.iter_mut().find(|line| line.item_id == item_id) {
            line.serial_ids.push(serial_id);
        }
    }
    Ok(lines)
}

/// 选目标库存行：**有整仓汇总桶就只动汇总桶**，没有才按库位精确匹配。
///
/// 返回 `None` = 该物料在这个仓还没有任何库存行（调入侧要建档；调出侧 = 库存 0）。
pub(super) async fn resolve_inventory_row(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Option<Uuid>> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列（口径同 erp/model.rs）。
    let row = ex
        .query_first_sql(
            "SELECT id FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND (location_id IS NULL \
                     OR location_id IS NOT DISTINCT FROM NULLIF(?::text,'')::uuid) \
              ORDER BY (location_id IS NULL) DESC \
              LIMIT 1"
                .to_string(),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                uuid_text(location_id),
            ],
        )
        .await?;
    row.map(|row| row.get_typed::<Uuid>("id")).transpose()
}

/// 该库存行当前账面数量（`None` = 行已不存在）。
pub(super) async fn current_qty(
    ex: &impl SqlExecutor,
    inventory_id: Uuid,
) -> DbResult<Option<Decimal>> {
    // SCOPE-EXEMPT: 库存是仓储数据，无归属列（口径同 erp/model.rs）。
    let row = ex
        .query_first_sql(
            "SELECT qty::numeric(18,4) AS qty FROM biz_inventory WHERE id = ?::uuid".to_string(),
            vec![Value::from(&inventory_id)],
        )
        .await?;
    row.map(|row| row.get_typed::<Decimal>("qty")).transpose()
}

/// 调出仓**条件扣减**（`AND qty >= ?`，卡面红线③）。
///
/// 返回 `None` = 0 行受影响 = 库存不足（并发超卖在这里被挡下）。
///
/// 金额用 SQL 侧的 `ROUND(数量 * avg_cost, 4)`：既拿到「调出时的均价快照」，
/// 又保证「调出仓 amount 减少量」与「调入仓 amount 增加量」是**同一个数**
/// （Rust 侧的 `Decimal` 舍入规则与 PG numeric 不同，各算一遍会在半分位差 1 分）。
pub(super) async fn deduct_inventory_row(
    ex: &impl SqlExecutor,
    inventory_id: Uuid,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<Option<OutSnapshot>> {
    // P2-5A：条件扣减 SQL（`AND qty >= ?` + SQL 侧 `ROUND(qty × avg_cost, 4)`）收口到
    // [`inventory_writer::try_deduct_cost`]；`None` 仍然是「库存不足」。
    match inventory_writer::try_deduct_cost(ex, inventory_id, qty, now).await? {
        None => Ok(None),
        Some(snapshot) => Ok(Some(OutSnapshot {
            before_qty: snapshot.before_qty,
            after_qty: snapshot.after_qty,
            unit_cost: snapshot.unit_cost,
            amount: snapshot.amount,
            after_amount: snapshot.after_amount,
        })),
    }
}

/// 调入仓加库存的入参（8 个字段里 6 个是数据，收成结构体避免 `too_many_arguments`）。
#[derive(Debug, Clone, Copy)]
pub(super) struct CreditReq {
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    unit_cost: Decimal,
    amount: Decimal,
}

/// 调入仓加库存：行存在 → 移动加权重算 `avg_cost`；行不存在 → 建档（`avg_cost` 取原价）。
pub(super) async fn credit_inventory_row(
    ex: &impl SqlExecutor,
    req: &CreditReq,
    now: DateTime<Utc>,
) -> DbResult<InSnapshot> {
    let CreditReq {
        material_id,
        warehouse_id,
        location_id,
        qty,
        unit_cost,
        amount,
    } = *req;
    // P2-5A：调入（汇总桶优先锁读 → 加/建档 + 移动加权）收口到
    // [`inventory_writer::credit_cost`]，返回 (变动前, 变动后)。
    let (before, after) = inventory_writer::credit_cost(
        ex,
        &CostDelta {
            material_id,
            warehouse_id,
            location_id,
            qty,
            unit_cost,
            amount,
        },
        now,
    )
    .await?;
    Ok(InSnapshot {
        before_qty: before.qty,
        before_amount: before.amount,
        before_cost: before.avg_cost,
        after_qty: after.qty,
        after_amount: after.amount,
        after_cost: after.avg_cost,
    })
}

/// 一物一码物料的序列号移仓（只动 `IN_STOCK` 且在**调出仓**的行，返回实际转移件数）。
pub(super) async fn move_serials(
    ex: &impl SqlExecutor,
    serial_ids: &[Uuid],
    from_warehouse_id: Uuid,
    to_warehouse_id: Uuid,
    to_location_id: Option<Uuid>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    if serial_ids.is_empty() {
        return Ok(0);
    }
    // 数组参数走 `Value::Text("{a,b}")` + `?::text::uuid[]`（技能 §6.5 实测的唯一可行形态）
    let payload = Value::Text(format!(
        "{{{}}}",
        serial_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    ));
    // SCOPE-EXEMPT: 序列号是仓储数据，无归属列（口径同 erp/model.rs）。
    ex.exec_sql(
        "UPDATE biz_material_serial \
            SET warehouse_id = ?::uuid, location_id = NULLIF(?::text,'')::uuid, \
                updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ANY(?::text::uuid[]) \
            AND status = 'IN_STOCK' AND deleted_at IS NULL \
            AND warehouse_id = ?::uuid"
            .to_string(),
        vec![
            Value::from(&to_warehouse_id),
            uuid_text(to_location_id),
            Value::from(now),
            uuid_text(operator),
            payload,
            Value::from(&from_warehouse_id),
        ],
    )
    .await
}

/// 写一条调拨库存流水（`txn_type` 决定方向，`qty` 恒正 —— `ck_biz_inv_txn_qty` 禁负数）。
pub(super) async fn post_stock_txn(
    ex: &impl SqlExecutor,
    txn: &StockTxn<'_>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    // P2-5A：流水 SQL 收口到 [`inventory_writer::insert_txn`]。
    inventory_writer::insert_txn(
        ex,
        &TxnRecord {
            material_id: txn.line.material_id,
            warehouse_id: txn.warehouse_id,
            txn_type: txn.txn_type,
            biz_type: txn.biz_type,
            doc_type: DOC_TYPE_TRANSFER,
            doc_id: txn.transfer_id,
            doc_no: txn.transfer_no,
            line_no: txn.line.line_no,
            qty: txn.qty,
            unit_cost: txn.unit_cost,
            amount: txn.amount,
            before: InventorySnapshot {
                qty: txn.before_qty,
                amount: txn.before_amount,
                avg_cost: txn.before_cost,
            },
            after: InventorySnapshot {
                qty: txn.after_qty,
                amount: txn.after_amount,
                avg_cost: txn.after_cost,
            },
            operator: txn.operator,
            period_code: None,
            remark: None,
            now,
        },
    )
    .await
    .map(|_| ())
}

/// 不变式断言（**同事务末尾**，任何一条不满足就整笔回滚）：
///
/// 1. 本次动过的库存行 `qty >= 0`（条件扣减已保证，这里再断一次防止将来被改成无条件 UPDATE）；
/// 2. 一物一码物料：「账面数量 == 在库序列号件数」（W2c-1 / W2c-2 的口径：
///    `COALESCE(汇总桶 qty, SUM(库位行 qty))` vs `COUNT(serial WHERE status='IN_STOCK')`）；
/// 3. 成本守恒：本单 `SUM(OUT amount) == SUM(IN amount)`；
/// 4. **两本账相符**（与巡检 B1 同判据，容差 0.0001）：本次调拨涉及的每个
///    `(material_id, warehouse_id)` 上 `SUM(biz_inventory_location.qty) == SUM(biz_inventory.qty)`。
///    这一条是本卡补的「当场自检」：历史上调拨/批量生成只写成本账、库位账一行不写，
///    造成巡检 B1（BLOCKER）在调出仓/调入仓两侧必红；现在任何一次漏账都会在结算当刻
///    整单回滚并指名道姓地报出来，而不是等下一次巡检。
pub(super) async fn assert_settle_invariants(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
) -> SettleResult<()> {
    // SCOPE-EXEMPT: 库存 / 序列号 / 库位账都是仓储数据，无归属列（口径同 erp/model.rs）。
    let row = ex
        .query_first_sql(
            format!(
                "WITH touched AS ( \
                     SELECT DISTINCT material_id, warehouse_id FROM biz_inventory_txn \
                      WHERE doc_type = '{DOC_TYPE_TRANSFER}' AND doc_id = ?::uuid), \
                 book AS ( \
                     SELECT t.material_id, t.warehouse_id, \
                            COALESCE(MAX(b.qty) FILTER (WHERE b.location_id IS NULL), \
                                     SUM(b.qty), 0)::numeric(18,4) AS book_qty \
                       FROM touched t \
                       LEFT JOIN biz_inventory b \
                              ON b.material_id = t.material_id \
                             AND b.warehouse_id = t.warehouse_id \
                      GROUP BY t.material_id, t.warehouse_id), \
                 broken AS ( \
                     SELECT bk.material_id \
                       FROM book bk \
                       JOIN biz_material m ON m.id = bk.material_id \
                      WHERE m.serial_required \
                        AND bk.book_qty <> (SELECT COUNT(*) FROM biz_material_serial s \
                                             WHERE s.material_id = bk.material_id \
                                               AND s.warehouse_id = bk.warehouse_id \
                                               AND s.status = 'IN_STOCK' \
                                               AND s.deleted_at IS NULL)), \
                 money AS ( \
                     SELECT COALESCE(SUM(amount) FILTER (WHERE txn_type = 'OUT'), 0) \
                          - COALESCE(SUM(amount) FILTER (WHERE txn_type = 'IN'), 0) AS diff \
                       FROM biz_inventory_txn \
                      WHERE doc_type = '{DOC_TYPE_TRANSFER}' AND doc_id = ?::uuid), \
                 two_books AS ( \
                     SELECT t.material_id, t.warehouse_id, \
                            COALESCE((SELECT SUM(b.qty) FROM biz_inventory b \
                                       WHERE b.material_id = t.material_id \
                                         AND b.warehouse_id = t.warehouse_id), 0)::numeric(18,4) AS cost_qty, \
                            COALESCE((SELECT SUM(l.qty) FROM biz_inventory_location l \
                                       WHERE l.material_id = t.material_id \
                                         AND l.warehouse_id = t.warehouse_id), 0)::numeric(18,4) AS loc_qty \
                       FROM touched t) \
                 SELECT (SELECT COUNT(*) FROM broken) AS serial_broken, \
                        (SELECT COUNT(*) FROM biz_inventory b JOIN touched t \
                            ON t.material_id = b.material_id \
                           AND t.warehouse_id = b.warehouse_id \
                          WHERE b.qty < 0) AS negative_qty, \
                        (SELECT diff FROM money) AS cost_diff, \
                        (SELECT COUNT(*) FROM two_books \
                          WHERE abs(cost_qty - loc_qty) > 0.0001) AS location_broken"
            ),
            vec![Value::from(&transfer_id), Value::from(&transfer_id)],
        )
        .await?
        .ok_or_else(|| {
            SettleError::Invariant("不变式查询没有返回行（聚合查询不应为空）".to_string())
        })?;

    let serial_broken: i64 = row.get_typed("serial_broken")?;
    let negative_qty: i64 = row.get_typed("negative_qty")?;
    let cost_diff: Decimal = row.get_typed("cost_diff")?;
    let location_broken: i64 = row.get_typed("location_broken")?;
    if negative_qty > 0 {
        return Err(SettleError::Invariant(format!(
            "调拨结算后出现负库存（{negative_qty} 个库存行 qty < 0）"
        )));
    }
    if serial_broken > 0 {
        return Err(SettleError::Invariant(format!(
            "一物一码物料的账面数量与在库序列号件数不一致（{serial_broken} 个「物料 × 仓库」），\
             先跑 GET /business/erp/inventory/audit?dry_run=false 修正后再结算"
        )));
    }
    if cost_diff != Decimal::ZERO {
        return Err(SettleError::Invariant(format!(
            "成本不守恒：调出合计 - 调入合计 = {cost_diff}（必须为 0）"
        )));
    }
    if location_broken > 0 {
        return Err(SettleError::Invariant(format!(
            "库位账与成本账不符（{location_broken} 个「物料 × 仓库」）：{}。\
             库位账（biz_inventory_location）必须与成本账（biz_inventory）逐仓相等，\
             先跑 GET /business/erp/inventory/audit?dry_run=false 修账后再结算",
            location_broken_detail(ex, transfer_id).await?
        )));
    }
    Ok(())
}

/// 两本账不符的明细文案（**只在断言失败时查**，正常结算不付这次开销）。
///
/// 逐条打印 `物料编码 / 仓库名 / 成本账 / 库位账 / 差额`，让运维不用自己拼 SQL 就能定位。
pub(super) async fn location_broken_detail(
    ex: &impl SqlExecutor,
    transfer_id: Uuid,
) -> DbResult<String> {
    // SCOPE-EXEMPT: 库存 / 库位账是仓储数据，无归属列（口径同 erp/model.rs）。
    let rows = ex
        .query_sql(
            format!(
                "WITH touched AS ( \
                     SELECT DISTINCT material_id, warehouse_id FROM biz_inventory_txn \
                      WHERE doc_type = '{DOC_TYPE_TRANSFER}' AND doc_id = ?::uuid), \
                 two_books AS ( \
                     SELECT t.material_id, t.warehouse_id, \
                            COALESCE((SELECT SUM(b.qty) FROM biz_inventory b \
                                       WHERE b.material_id = t.material_id \
                                         AND b.warehouse_id = t.warehouse_id), 0)::numeric(18,4) AS cost_qty, \
                            COALESCE((SELECT SUM(l.qty) FROM biz_inventory_location l \
                                       WHERE l.material_id = t.material_id \
                                         AND l.warehouse_id = t.warehouse_id), 0)::numeric(18,4) AS loc_qty \
                       FROM touched t) \
                 SELECT m.material_code, \
                        COALESCE(w.warehouse_name, '?') AS warehouse_name, \
                        tb.cost_qty::text AS cost_qty, \
                        tb.loc_qty::text AS loc_qty, \
                        (tb.cost_qty - tb.loc_qty)::text AS diff \
                   FROM two_books tb \
                   JOIN biz_material m ON m.id = tb.material_id \
                   LEFT JOIN biz_warehouse w ON w.id = tb.warehouse_id \
                  WHERE abs(tb.cost_qty - tb.loc_qty) > 0.0001 \
                  ORDER BY m.material_code, warehouse_name"
            ),
            vec![Value::from(&transfer_id)],
        )
        .await?;
    let mut parts = Vec::with_capacity(rows.len());
    for row in &rows {
        parts.push(format!(
            "物料 {} / 仓 {} 成本账 {} 库位账 {}（差 {}）",
            row.get_typed::<String>("material_code")?,
            row.get_typed::<String>("warehouse_name")?,
            row.get_typed::<String>("cost_qty")?,
            row.get_typed::<String>("loc_qty")?,
            row.get_typed::<String>("diff")?,
        ));
    }
    Ok(parts.join("；"))
}

// ==================== 在途超期：扫描 / 告警台账 / 汇总（LNK-F12） ====================

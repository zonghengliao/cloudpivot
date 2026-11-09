//! 库存三本账的**唯一写入口**（InventoryWriter）。
//!
//! # 为什么要有这个模块（P2-5A）
//!
//! 收口前 `biz_inventory`（成本账）有 **13 个文件**直接写、`biz_inventory_location`
//! （库位账）13 个、`biz_inventory_txn`（流水）8 个 —— **没有唯一写入口**。
//! 同一个「加法」在 A 文件里 `ROUND(…, 6)`、在 B 文件里不 ROUND；
//! 同一个「减法」在 C 文件里整桶归零、在 D 文件里 `GREATEST(…, 0)` 夹住；
//! 任何一处口径漂移都不会立刻报错，只会在月度巡检里变成「账不对」。
//!
//! 本模块把**所有**对这三张表的写 SQL（`INSERT` / `UPDATE` / `DELETE`）搬进来，
//! 成为唯一落 SQL 的地方；业务模块只保留「算什么、什么时候写」的编排，SQL 全部经此。
//! 闸门在 `scripts/verify/inventory_write_gate.py`（CI 里跑）：
//! `backend/src` 下除本文件外任何**非测试**代码出现 `UPDATE biz_inventory…`
//! / `INSERT INTO biz_inventory…` 一律判红。
//!
//! # 分层
//!
//! ```text
//! handler（接口 / 事务边界）
//!    ↓
//! 业务 model（算什么：移动加权 / 整桶取完 / 冲销原额 / 序列号派生）
//!    ↓
//! inventory_writer（唯一写 SQL 的地方：锁读 + 加法 + 减法 + upsert + 流水）
//!    ↓
//! 三张表：biz_inventory（成本账）/ biz_inventory_location（库位账）/ biz_inventory_txn（流水）
//! ```
//!
//! **口径留在业务侧**：本模块只做「加 / 减 / 建成 / 批对齐」这几件事，
//! 不允许把「整桶取完要不要清零」「均价怎么算」这类业务判断搬进来 ——
//! 但 SQL 里的算子（`ROUND` 的位数、`GREATEST` 夹 0）必须在 SQL 里写一次，
//! 因为 Rust 侧的 `Decimal` 舍入与 PG 的 `numeric` 不是同一套规则（W3c 踩过差 1 分钱）。
//!
//! # 锁顺序（PRD §3.2.6，防死锁）
//!
//! 全局只有一种顺序：**明细按 `material_id` 升序 → 成本账先锁 → 库位账后锁**。
//! 本模块的每个「先锁后改」都遵守它（`lock_cost` / `lock_location` 只读锁，
//! 写函数自己带锁的见函数文档）。
//!
//! # 两处**不合并**的同名语义（有意为之，别「顺手」合并）
//!
//! | 语义 | 函数 | 为什么不能和别人合并 |
//! |------|------|----------------------|
//! | 出库扣减（整桶取完金额归零） | [`deduct_cost`] | 库存出到 0 还留着钱 = 「0 件还有钱」，巡检 C4 必红 |
//! | 调拨调出（条件扣减、均价不动） | [`try_deduct_cost`] | 调拨金额必须与调入侧**同一个数**，均价原样不动才对得上 |
//! | 入库加法（upsert，`ROUND(…,6)`） | [`add_cost`] | 收货用的是「金额权威、加法不重算」口径 |
//! | 调拨调入（先锁后改、汇总桶优先） | [`credit_cost`] | 调拨要先按「汇总桶优先」选行并锁住，再移动加权 |

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use super::serial_model;
use super::stockin_base::InventorySnapshot;

/// `location_id` 空桶占位（与 `uk_biz_inv_mat_wh_loc`、`uk_biz_inv_loc` 的表达式逐字一致）。
const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// 库存不足的哨兵前缀（`DbError` 只有字符串，handler 据此回 `409` 而不是 `500`）。
///
/// 形态：`INV_SHORTFALL|<明细文案>`，例如
/// `INV_SHORTFALL|物料 P2B1-M1 库存 3.0000 < 本单 9.0000`。
///
/// 定义在本模块而不是 `stockin_cancel`：库位账冲减（[`subtract_location_qty`]）也要用它，
/// 而冲减 SQL 现在住在这里；`stockin_cancel` 用 `pub use` 重导出，调用方路径不变。
pub const SHORTFALL_PREFIX: &str = "INV_SHORTFALL|";

fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|v| v.to_string()).unwrap_or_default())
}

/// 从 `RETURNING` 行里取三项拼快照（`Row` 类型在 `nik` 门面里不公开，用宏避开类型名）。
macro_rules! snapshot {
    ($row:expr, $q:literal, $a:literal, $c:literal) => {{
        Ok(InventorySnapshot {
            qty: $row.get_typed($q)?,
            amount: $row.get_typed($a)?,
            avg_cost: $row.get_typed($c)?,
        })
    }};
}

// ============================================================================
// 一、成本账 biz_inventory（数量 + 金额 + 移动加权单价）
// ============================================================================

/// 成本账一次增量的**坐标 + 数值**（物料 × 仓库 × 库位桶 + 数量/单价/金额）。
///
/// 收成结构体不是为了少打字：这三组值必须**同源**（同一个明细行算出来的），
/// 散成 6 个位置参数时最容易「位置传错」，而 `qty`/`amount` 都是 `Decimal`，
/// 编译器不会拦。`clippy::too_many_arguments` 只是顺带被解决。
#[derive(Debug, Clone, Copy)]
pub(crate) struct CostDelta {
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 库位账行 id（`None` = 未分配库位汇总桶）。
    pub location_id: Option<Uuid>,
    /// 变动数量（正入负出由调用方语义决定，本模块只做加法）。
    pub qty: Decimal,
    /// 本次单价（只用于「建档」时的 `avg_cost` 兜底与流水）。
    pub unit_cost: Decimal,
    /// 变动金额（**权威值**，不用 `qty × unit_cost` 重算）。
    pub amount: Decimal,
}

/// 锁读 `(物料, 仓库, 库位)` 的库存行（`SELECT … FOR UPDATE`，PRD §3.2.6）。
///
/// `location_id` 为 `None` 时按 `COALESCE(location_id, 零 uuid) = 零 uuid` 命中
/// 「未分配库位汇总桶」，与 `uk_biz_inv_mat_wh_loc` 的表达式索引完全一致
/// （否则走不上索引、也锁不到同一行）。
pub(crate) async fn lock_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Option<InventorySnapshot>> {
    let row = ex
        .query_first_sql(
            "SELECT qty, amount, avg_cost FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                  = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
              FOR UPDATE"
                .to_string(),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                uuid_text(location_id),
            ],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(snapshot!(&row, "qty", "amount", "avg_cost")?)),
    }
}

/// **入库加法**（PRD §3.2.3）：加数量 + 加金额 + 重算移动加权单价。
///
/// 1. `SELECT … FOR UPDATE` 锁住库存行（行不存在则无锁可拿）；
/// 2. `INSERT … ON CONFLICT (material, warehouse, COALESCE(location)) DO UPDATE` 做**加法**，
///    `RETURNING` 回读真实落库值。
///
/// 为什么用 upsert 而不是「读到就 UPDATE、读不到就 INSERT」：两个收货单并发收同一物料、
/// 且库存行**还不存在**时，两边都读到「无行」→ 两边 INSERT → 后到的撞唯一键（42P10/23505）。
/// upsert 让后到的那笔走 `DO UPDATE` 分支，加法照做，数字不丢。
///
/// `avg_cost` 的算子在 SQL 里（不是在 Rust 里）：`ROUND` 是 PG 的 numeric 语义，
/// 与库内既有 `avg_cost` 的构造方式同源（W3c 踩过「Rust 与 PG 舍入差 1 分钱」）。
pub(crate) async fn add_cost(
    ex: &impl SqlExecutor,
    delta: &CostDelta,
    now: DateTime<Utc>,
) -> DbResult<InventorySnapshot> {
    let CostDelta {
        material_id,
        warehouse_id,
        location_id,
        qty,
        unit_cost,
        amount,
    } = *delta;
    let _ = lock_cost(ex, material_id, warehouse_id, location_id).await?;
    let qty_text = qty.round_dp(4).to_string();
    let amount_text = amount.round_dp(4).to_string();
    let unit_text = unit_cost.to_string();

    let row = ex
        .query_first_sql(
            "INSERT INTO biz_inventory AS i \
                 (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, \
                  last_txn_at, created_at, updated_at) \
             VALUES (?, ?::uuid, ?::uuid, NULLIF(?::text,'')::uuid, \
                     NULLIF(?::text,'')::numeric, 0, \
                     CASE WHEN NULLIF(?::text,'')::numeric > 0 \
                          THEN ROUND(NULLIF(?::text,'')::numeric / NULLIF(?::text,'')::numeric, 6) \
                          ELSE 0 END, \
                     NULLIF(?::text,'')::numeric, ?, ?, ?) \
             ON CONFLICT (material_id, warehouse_id, \
                          COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid)) \
             DO UPDATE SET \
                 qty = i.qty + EXCLUDED.qty, \
                 amount = i.amount + EXCLUDED.amount, \
                 avg_cost = CASE \
                     WHEN i.qty > 0 AND (i.qty + EXCLUDED.qty) > 0 \
                         THEN ROUND((i.amount + EXCLUDED.amount) / (i.qty + EXCLUDED.qty), 6) \
                     WHEN EXCLUDED.qty > 0 \
                         THEN ROUND(EXCLUDED.amount / EXCLUDED.qty, 6) \
                     ELSE 0 END, \
                 last_txn_at = EXCLUDED.last_txn_at, \
                 updated_at = EXCLUDED.updated_at \
             RETURNING qty, amount, avg_cost"
                .to_string(),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&material_id),
                Value::from(&warehouse_id),
                uuid_text(location_id),
                Value::Text(qty_text.clone()),
                Value::Text(unit_text),
                Value::Text(amount_text.clone()),
                Value::Text(qty_text),
                Value::Text(amount_text),
                Value::from(now),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("库存 upsert 未返回行（biz_inventory 的 RETURNING 不应为空）".into()))?;
    snapshot!(&row, "qty", "amount", "avg_cost")
}

/// **作废冲减**（收货单作废）：`qty -= 本单数量`、`amount -= 过账时的金额`。
///
/// 三条硬口径：`qty` 归零 → `amount` 同时归零（避免留下「0 件还有钱」的幽灵金额）；
/// 均价按 `(amount - a) / (qty - q)` 重算（等价于「把这一层从加权平均里摘掉」，
/// 金额减了、单价不动会让 `amount ≠ qty × avg_cost`，巡检 C4 直接红）；调用方
/// 必须**先**做过可退性校验（本函数只负责按桶键减，不自带「够不够」的判断）。
///
/// 返回减后的账面三值；命中 0 行 = 锁读之后行被删了，报错整单回滚。
pub(crate) async fn subtract_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<InventorySnapshot> {
    let qty_text = qty.round_dp(4).to_string();
    let amount_text = amount.round_dp(4).to_string();
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory \
                SET qty = qty - NULLIF(?::text,'')::numeric, \
                    amount = CASE WHEN qty - NULLIF(?::text,'')::numeric <= 0 THEN 0 \
                                  ELSE amount - NULLIF(?::text,'')::numeric END, \
                    avg_cost = CASE WHEN qty - NULLIF(?::text,'')::numeric <= 0 THEN 0 \
                                    ELSE ROUND((amount - NULLIF(?::text,'')::numeric) \
                                               / (qty - NULLIF(?::text,'')::numeric), 6) END, \
                    last_txn_at = ?, updated_at = ? \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                  = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
              RETURNING qty, amount, avg_cost"
                .to_string(),
            vec![
                // 每个 `?` 都占一个独立的位置号（`?` → `$n` 逐次递增），
                // 所以 qty/amount 出现几次就要传几次（与 add_cost 同款写法）。
                Value::Text(qty_text.clone()),    // $1 qty 减法
                Value::Text(qty_text.clone()),    // $2 CASE 判断数量是否归零
                Value::Text(amount_text.clone()), // $3 金额减法
                Value::Text(qty_text.clone()),    // $4 avg_cost 的 CASE 判断
                Value::Text(amount_text),         // $5 均价分子
                Value::Text(qty_text),            // $6 均价分母
                Value::from(now),                 // $7 last_txn_at
                Value::from(now),                 // $8 updated_at
                Value::from(&material_id),        // $9
                Value::from(&warehouse_id),       // $10
                uuid_text(location_id),           // $11
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("库存反向冲减未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into()))?;
    snapshot!(&row, "qty", "amount", "avg_cost")
}

/// **出库作废回加**（把当时扣掉的 `qty` 与 `amount` 原路加回；行不存在则建档）。
///
/// 金额取**作废时的明细 `amount` 快照**（过账时写入的账上真实差额），
/// 不用当前 `avg_cost × qty` 重算：期间又进过货，均价已变，用现价加回会污染成本。
pub(crate) async fn restore_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    amount: Decimal,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let qty_text = qty.round_dp(4).to_string();
    let amount_text = amount.round_dp(4).to_string();
    // 行已存在 → 加数量 + 加金额 + 重算均价；行不存在（历史缺口）→ 建档
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory \
                SET qty = qty + NULLIF(?::text,'')::numeric, \
                    amount = amount + NULLIF(?::text,'')::numeric, \
                    avg_cost = CASE WHEN qty + NULLIF(?::text,'')::numeric > 0 \
                                    THEN ROUND((amount + NULLIF(?::text,'')::numeric) \
                                               / (qty + NULLIF(?::text,'')::numeric), 6) \
                                    ELSE 0 END, \
                    last_txn_at = ?, updated_at = ? \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid) \
                  = COALESCE(NULLIF(?::text,'')::uuid, '00000000-0000-0000-0000-000000000000'::uuid) \
             RETURNING id"
                .to_string(),
            vec![
                Value::Text(qty_text.clone()),    // $1 qty 加法
                Value::Text(amount_text.clone()), // $2 金额加法
                Value::Text(qty_text.clone()),    // $3 avg 判断
                Value::Text(amount_text.clone()), // $4 avg 分子
                Value::Text(qty_text.clone()),    // $5 avg 分母
                Value::from(now),
                Value::from(now),
                Value::from(&material_id),
                Value::from(&warehouse_id),
                uuid_text(location_id),
            ],
        )
        .await?;
    if row.is_some() {
        return Ok(());
    }
    // 建档（成本账行被硬删过的历史缺口）：数量与金额直接用本次退回值
    ex.exec_sql(
        "INSERT INTO biz_inventory \
             (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, \
              last_txn_at, created_at, updated_at) \
         VALUES (?, ?::uuid, ?::uuid, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,'')::numeric, 0, \
                 CASE WHEN NULLIF(?::text,'')::numeric > 0 \
                      THEN ROUND(NULLIF(?::text,'')::numeric / NULLIF(?::text,'')::numeric, 6) \
                      ELSE 0 END, \
                 NULLIF(?::text,'')::numeric, ?, ?, ?)"
            .to_string(),
        vec![
            Value::from(&Uuid::now_v7()),     // $1 id
            Value::from(&material_id),        // $2 material_id
            Value::from(&warehouse_id),       // $3 warehouse_id
            uuid_text(location_id),           // $4 location_id
            Value::Text(qty_text.clone()),    // $5 qty
            Value::Text(amount_text.clone()), // $6 CASE 判断（金额 > 0 才建均价）
            Value::Text(amount_text.clone()), // $7 均价分子
            Value::Text(qty_text.clone()),    // $8 均价分母
            // $9 amount 列：**原实现漏了这个绑定**（12 个 `?` 只传 11 个参数，
            // 建档分支一旦走到就会把时间戳当 numeric 绑上去报错 —— 只是历史缺口很少触发，
            // 所以一直没被发现）。收口时按 SQL 逐位对上，别再漏。
            Value::Text(amount_text), // $9 amount
            Value::from(now),         // $10 last_txn_at
            Value::from(now),         // $11 created_at
            Value::from(now),         // $12 updated_at
        ],
    )
    .await?;
    Ok(())
}

/// **出库扣减**（按库存行 id，金额是账上差额、不是 `qty × avg` 重算）。
///
/// * 整桶取完（`qty_out >= 账面数量`）：`amount` 归零 —— 避免留下「0 件还有钱」的幽灵金额；
/// * 部分取：`amount -= ROUND(qty_out × avg_cost, 4)`，并用 `GREATEST(…, 0)` 兜住
///   脏账（`amount < qty × avg`）—— 金额绝不减成负数（`ck_biz_inv_txn_amt` 与 B2 都不允许）。
///
/// `avg_cost` 按**剩余金额 / 剩余数量**重算：扣减只从「这一桶的金额池」里拿走对应金额，
/// 拿走之后均价在数学上就是 `(amount − out) / (qty − out)`；原样不动会在
/// 分数位（`ROUND` 到 4 位）上让 `amount ≠ qty × avg_cost`，触发巡检 C4。
pub(crate) async fn deduct_cost(
    ex: &impl SqlExecutor,
    inventory_id: Uuid,
    qty_out: Decimal,
    now: DateTime<Utc>,
) -> DbResult<InventorySnapshot> {
    let qty_text = qty_out.round_dp(4).to_string();
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory \
                SET qty = qty - NULLIF(?::text,'')::numeric, \
                    amount = CASE WHEN NULLIF(?::text,'')::numeric >= qty THEN 0 \
                                  ELSE GREATEST(amount - ROUND(NULLIF(?::text,'')::numeric * avg_cost, 4), 0) END, \
                    avg_cost = CASE WHEN qty - NULLIF(?::text,'')::numeric > 0 \
                                    THEN ROUND( \
                                        CASE WHEN NULLIF(?::text,'')::numeric >= qty THEN 0 \
                                             ELSE GREATEST(amount - ROUND(NULLIF(?::text,'')::numeric * avg_cost, 4), 0) END \
                                        / (qty - NULLIF(?::text,'')::numeric), 6) \
                                    ELSE 0 END, \
                    last_txn_at = ?, updated_at = ? \
              WHERE id = ?::uuid \
             RETURNING qty, amount, avg_cost"
                .to_string(),
            vec![
                Value::Text(qty_text.clone()), // $1 qty 减法
                Value::Text(qty_text.clone()), // $2 整桶取完判断
                Value::Text(qty_text.clone()), // $3 金额减数
                Value::Text(qty_text.clone()), // $4 avg 的 CASE 判断
                Value::Text(qty_text.clone()), // $5 整桶取完判断（avg 分支）
                Value::Text(qty_text.clone()), // $6 金额减数（avg 分支）
                Value::Text(qty_text),         // $7 avg 分母
                Value::from(now),              // $8 last_txn_at
                Value::from(now),              // $9 updated_at
                Value::from(&inventory_id),    // $10
            ],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue(
                "出库扣减成本账未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into(),
            )
        })?;
    snapshot!(&row, "qty", "amount", "avg_cost")
}

/// 调拨调出的一次扣减快照（`None` = 0 行受影响 = 库存不足，并发超卖在这里被挡下）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeductSnapshot {
    /// 扣减前数量（`qty + 本单`）。
    pub before_qty: Decimal,
    /// 扣减后数量。
    pub after_qty: Decimal,
    /// 扣减时的均价快照（`avg_cost`，调拨不动它）。
    pub unit_cost: Decimal,
    /// 账上真实扣减金额 = `ROUND(qty × avg_cost, 4)`（凭证与明细都用它）。
    pub amount: Decimal,
    /// 扣减后账面金额。
    pub after_amount: Decimal,
}

/// **调拨调出条件扣减**（`AND qty >= ?`，卡面红线③）。
///
/// 返回 `None` = 0 行受影响 = 库存不足（并发超卖在这里被挡下）。
///
/// 金额用 SQL 侧的 `ROUND(数量 * avg_cost, 4)`：既拿到「调出时的均价快照」，
/// 又保证「调出仓 amount 减少量」与「调入仓 amount 增加量」是**同一个数**
/// （Rust 侧的 `Decimal` 舍入规则与 PG numeric 不同，各算一遍会在半分位差 1 分）。
pub(crate) async fn try_deduct_cost(
    ex: &impl SqlExecutor,
    inventory_id: Uuid,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<Option<DeductSnapshot>> {
    let qty_text = qty.to_string();
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory \
                SET qty = qty - NULLIF(?::text,'')::numeric, \
                    amount = amount - ROUND(NULLIF(?::text,'')::numeric * avg_cost, 4), \
                    last_txn_at = ?, updated_at = ? \
              WHERE id = ?::uuid AND qty >= NULLIF(?::text,'')::numeric \
             RETURNING (qty + NULLIF(?::text,'')::numeric)::numeric(18,4) AS before_qty, \
                       qty::numeric(18,4) AS after_qty, \
                       avg_cost::numeric(18,6) AS unit_cost, \
                       ROUND(NULLIF(?::text,'')::numeric * avg_cost, 4)::numeric(18,4) AS out_amount, \
                       amount::numeric(18,4) AS after_amount"
                .to_string(),
            vec![
                Value::Text(qty_text.clone()),
                Value::Text(qty_text.clone()),
                Value::from(now),
                Value::from(now),
                Value::from(&inventory_id),
                Value::Text(qty_text.clone()),
                Value::Text(qty_text.clone()),
                Value::Text(qty_text),
            ],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(DeductSnapshot {
            before_qty: row.get_typed("before_qty")?,
            after_qty: row.get_typed("after_qty")?,
            unit_cost: row.get_typed("unit_cost")?,
            amount: row.get_typed("out_amount")?,
            after_amount: row.get_typed("after_amount")?,
        })),
    }
}

/// 按 `(物料, 仓库, 库位)` 锁读并返回库存行 `(id, 账面三值)`（**精确匹配优先**）。
///
/// 调拨调入要「先按明细库位选行再锁」：`ORDER BY (location_id IS NOT DISTINCT FROM 明细库位) DESC`
/// 让**明细指定的那个库位桶**排在前面 —— 与 `transfer_settle::pick_bucket` 同一个口径
/// （FIX-4 问题 2：明细点名库位就动那个库位桶，不再「有汇总桶就只动汇总桶」）。
/// 明细没给库位（参数 `None`）时排序表达式退化成 `location_id IS NULL`，命中整仓汇总桶，
/// 与历史口径逐字一致。
///
/// 为什么必须「先锁后改」：移动加权要基于**本事务锁住的**旧值，否则两笔并发调入会各自
/// 算出一个 `avg_cost`，后写的把先写的覆盖掉。
pub(crate) async fn lock_cost_bucket(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Option<(Uuid, InventorySnapshot)>> {
    let row = ex
        .query_first_sql(
            "SELECT id, qty::numeric(18,4) AS qty, amount::numeric(18,4) AS amount, \
                    avg_cost::numeric(18,6) AS avg_cost \
               FROM biz_inventory \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                AND (location_id IS NULL \
                     OR location_id IS NOT DISTINCT FROM NULLIF(?::text,'')::uuid) \
              ORDER BY (location_id IS NOT DISTINCT FROM NULLIF(?::text,'')::uuid) DESC \
              LIMIT 1 \
                FOR UPDATE"
                .to_string(),
            vec![
                Value::from(&material_id),
                Value::from(&warehouse_id),
                uuid_text(location_id),
                uuid_text(location_id),
            ],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get_typed("id")?,
            snapshot!(&row, "qty", "amount", "avg_cost")?,
        ))),
    }
}

/// **调拨调入**（行存在 → 移动加权重算 `avg_cost`；行不存在 → 建档 `avg_cost` 取原价）。
///
/// 返回 `(变动前, 变动后)` 两个快照：调用方要拿它们写流水的 `before_*` / `after_*`。
/// 建档分支的「变动前」= 本次增量减去后的账面（并发下另一笔刚好插进来的行也按这个口径，
/// 保证 `before + delta == after` 恒成立）。
pub(crate) async fn credit_cost(
    ex: &impl SqlExecutor,
    delta: &CostDelta,
    now: DateTime<Utc>,
) -> DbResult<(InventorySnapshot, InventorySnapshot)> {
    let CostDelta {
        material_id,
        warehouse_id,
        location_id,
        qty,
        unit_cost,
        amount,
    } = *delta;
    let qty_text = qty.to_string();
    let amount_text = amount.to_string();

    if let Some((id, before)) = lock_cost_bucket(ex, material_id, warehouse_id, location_id).await? {
        let updated = ex
            .query_first_sql(
                "UPDATE biz_inventory \
                    SET qty = qty + NULLIF(?::text,'')::numeric, \
                        amount = amount + NULLIF(?::text,'')::numeric, \
                        avg_cost = CASE WHEN qty + NULLIF(?::text,'')::numeric > 0 \
                                        THEN (amount + NULLIF(?::text,'')::numeric) \
                                             / (qty + NULLIF(?::text,'')::numeric) \
                                        ELSE avg_cost END, \
                        last_txn_at = ?, updated_at = ? \
                  WHERE id = ?::uuid \
                 RETURNING qty::numeric(18,4) AS after_qty, \
                           amount::numeric(18,4) AS after_amount, \
                           avg_cost::numeric(18,6) AS after_cost"
                    .to_string(),
                vec![
                    Value::Text(qty_text.clone()),
                    Value::Text(amount_text.clone()),
                    Value::Text(qty_text.clone()),
                    Value::Text(amount_text.clone()),
                    Value::Text(qty_text.clone()),
                    Value::from(now),
                    Value::from(now),
                    Value::from(&id),
                ],
            )
            .await?
            .ok_or_else(|| {
                DbError::InvalidValue(format!(
                    "调入仓库存行 {id} 在加库存时消失（本事务已 FOR UPDATE 锁住，不应发生）"
                ))
            })?;
        let after = snapshot!(&updated, "after_qty", "after_amount", "after_cost")?;
        return Ok((before, after));
    }

    // 建档：`location_id` 用明细的库位（没有就是整仓汇总桶）
    let created = ex
        .query_first_sql(
            format!(
                "INSERT INTO biz_inventory AS i \
                     (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, \
                      amount, last_txn_at, created_at, updated_at) \
                 VALUES (?, ?::uuid, ?::uuid, NULLIF(?::text,'')::uuid, \
                         NULLIF(?::text,'')::numeric, 0, NULLIF(?::text,'')::numeric, \
                         NULLIF(?::text,'')::numeric, ?, ?, ?) \
                 ON CONFLICT (material_id, warehouse_id, \
                              COALESCE(location_id, '{ZERO_UUID}'::uuid)) DO UPDATE \
                    SET qty = i.qty + EXCLUDED.qty, \
                        amount = i.amount + EXCLUDED.amount, \
                        avg_cost = CASE WHEN i.qty + EXCLUDED.qty > 0 \
                                        THEN (i.amount + EXCLUDED.amount) \
                                             / (i.qty + EXCLUDED.qty) \
                                        ELSE i.avg_cost END, \
                        last_txn_at = EXCLUDED.last_txn_at, \
                        updated_at = EXCLUDED.updated_at \
                 RETURNING qty::numeric(18,4) AS after_qty, \
                           amount::numeric(18,4) AS after_amount, \
                           avg_cost::numeric(18,6) AS after_cost"
            ),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&material_id),
                Value::from(&warehouse_id),
                uuid_text(location_id),
                Value::Text(qty_text.clone()),
                Value::Text(unit_cost.to_string()),
                Value::Text(amount_text.clone()),
                Value::from(now),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("调入仓库存建档未返回行（upsert 不应为空）".to_string()))?;
    let after = snapshot!(&created, "after_qty", "after_amount", "after_cost")?;
    let before_qty = after.qty - qty;
    let before_amount = after.amount - amount;
    let before_cost = if before_qty > Decimal::ZERO {
        (before_amount / before_qty).round_dp(6)
    } else {
        unit_cost
    };
    Ok((
        InventorySnapshot {
            qty: before_qty,
            amount: before_amount,
            avg_cost: before_cost,
        },
        after,
    ))
}

/// **一物一码数量派生**：把成本账逐桶对齐成「在库序列号件数」（`avg_cost` 不动）。
///
/// 两条 UPDATE 一起搬进来，因为它们**必须成对**：
///
/// 1. `UPDATE … FROM (SELECT location_id, count(*) … GROUP BY 1)` 只覆盖「有在库件的桶」；
/// 2. 没有在库件的桶要显式归零 —— `UPDATE … FROM` 不会碰没有匹配行的那一侧；
///    漏了这条，某桶的号全部出库后成本账会**保留旧 qty**，B1 直接红。
///
/// 调用方（[`super::serial_model::recompute_quantity`]）负责锁顺序与「记账断链」
/// （有件数但没有成本账行）的 fail-closed 拒绝 —— 那两条是业务判断，不是写入。
pub(crate) async fn sync_cost_qty_from_serials(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<()> {
    // ① 逐桶对齐成在库件数（没有在库件的桶归零由 ② 收口）
    ex.exec_sql(
        format!(
            "UPDATE biz_inventory i \
                SET qty = c.q, amount = c.q * i.avg_cost, last_txn_at = now(), updated_at = now() \
               FROM (SELECT location_id AS bucket, count(*)::numeric AS q \
                       FROM biz_material_serial \
                      WHERE material_id = ?::uuid AND warehouse_id = ?::uuid \
                        AND status = 'IN_STOCK' AND deleted_at IS NULL \
                      GROUP BY 1) c \
              WHERE i.material_id = ?::uuid AND i.warehouse_id = ?::uuid \
                AND COALESCE(i.location_id, '{ZERO_UUID}'::uuid) = COALESCE(c.bucket, '{ZERO_UUID}'::uuid)"
        ),
        vec![
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::from(&material_id),
            Value::from(&warehouse_id),
        ],
    )
    .await?;
    // ② 没有在库件的桶显式归零
    ex.exec_sql(
        "UPDATE biz_inventory i SET qty = 0, amount = 0, last_txn_at = now(), updated_at = now() \
          WHERE i.material_id = ?::uuid AND i.warehouse_id = ?::uuid AND i.qty <> 0 \
            AND NOT EXISTS (SELECT 1 FROM biz_material_serial s \
                             WHERE s.material_id = i.material_id \
                               AND s.warehouse_id = i.warehouse_id \
                               AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL \
                               AND ((i.location_id IS NULL AND s.location_id IS NULL) \
                                    OR s.location_id = i.location_id))"
            .to_string(),
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await?;
    Ok(())
}

/// 巡检修正语句（① 单桶按差额调整）—— 把不一致的 `(物料, 仓库)` 账面**按差额**推平（一条语句）。
///
/// `source_cte` 是巡检提供的只读 CTE，必须产出关系
/// `combos(material_id, warehouse_id, book_qty, actual_qty, bucket_count)`。
/// **写语句本身住在本模块**（这是唯一写入口的一部分）；只有读侧 CTE 留在
/// [`super::inventory_audit`]，保证「报的口径」和「修的口径」是同一段 SQL。
///
/// # 为什么是「差额」而不是「覆盖」
///
/// 单桶时两者数学等价（桶里就那么多，`qty + (actual - qty) = actual`），
/// 但语义差在三处：
///
/// 1. **不写死 `qty = actual`**：将来若给单桶加「在途」之类的附加列，
///    差额语义不会把它们抹掉（覆盖语义会）；
/// 2. 口径与迁移后的两桶/多桶分支**同源** —— 多桶时也只允许「往某一个桶加差额」，
///    不存在「整仓覆盖」这种会把别的桶抹掉的写法；
/// 3. 历史事故（E2E-03，`Σqty=5` 报成 `2`）的根因就是「覆盖成汇总桶的值」：
///    `location_id = NULL` 的桶被当成整仓汇总，写 `qty = actual` 直接把库位桶的 3 件抹掉。
///
/// # 为什么只修单桶（`bucket_count = 1`）
///
/// 多桶并存（`bucket_count >= 2`）时「差额该记到哪个桶」本身有歧义：
/// 记到未分配桶会让「库位账 vs 成本账」逐桶对不上（B1b），记到某个库位桶则是凭空猜。
/// 这种行**一律不动**，由巡检以 `unfixable_reason = MULTI_BUCKET` 报出来要求人工处理。
///
/// # 其余口径
///
/// - `amount = 新 qty × avg_cost`：数量变了金额必须跟着变（否则成本账两列互相矛盾）；
///   差值由 SQL 侧 numeric 算，不在 Rust 里重算（PG 与 Rust 的舍入不是一套）；
/// - 只处理 `warehouse_id IS NOT NULL` 的行（没有仓库维度就写不进 `biz_inventory`）；
/// - `last_txn_at` 不写：巡检不是一笔业务，不该伪造流水时刻（只动 `updated_at`）。
pub(crate) fn align_cost_qty_delta_sql(source_cte: &str) -> String {
    format!(
        "{source_cte} \
         , bad AS ( \
            SELECT * FROM combos \
             WHERE book_qty <> actual_qty \
               AND warehouse_id IS NOT NULL \
               AND bucket_count = 1 \
         ) \
         UPDATE biz_inventory i \
            SET qty = i.qty + (b.actual_qty - b.book_qty), \
                amount = (i.qty + (b.actual_qty - b.book_qty)) * i.avg_cost, \
                updated_at = now() \
           FROM bad b \
          WHERE i.material_id = b.material_id AND i.warehouse_id = b.warehouse_id \
         RETURNING i.id"
    )
}

/// 巡检修正语句（② 反向：整行没建账 → 建档），`bucket_count = 0` 的那些组合。
///
/// 冲突目标是三列表达式 `ON CONFLICT (material_id, warehouse_id,
/// COALESCE(location_id, 全零 uuid))`（`uk_biz_inv_mat_wh_loc`），`INSERT` 显式写
/// `location_id = NULL` = 未分配桶 —— 与「期初落未分配桶」的历史口径一致
/// （ADR-20261031 §2.2 的批量生成同样落这一桶）。写两列目标会在 V0139 之后以 PG 42P10 失败。
///
/// `bucket_count = 0` 说明这个 `(物料, 仓库)` 在成本账里**一行都没有**，
/// 不存在可参照的均价 → `avg_cost = 0`、`amount = 0`（与修正前的行为一致：
/// 老实现 `LEFT JOIN` 未分配桶取 `avg_cost` 也只会拿到 NULL → 0）。
pub(crate) fn align_cost_qty_insert_sql(source_cte: &str) -> String {
    format!(
        "{source_cte} \
         , bad AS ( \
            SELECT * FROM combos \
             WHERE book_qty <> actual_qty \
               AND warehouse_id IS NOT NULL \
               AND bucket_count = 0 \
         ) \
         INSERT INTO biz_inventory AS i \
             (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount) \
         SELECT gen_random_uuid(), b.material_id, b.warehouse_id, NULL::uuid, b.actual_qty, \
                0, 0::numeric, 0::numeric \
           FROM bad b \
         ON CONFLICT (material_id, warehouse_id, \
                      COALESCE(location_id, '{ZERO_UUID}'::uuid)) \
         DO UPDATE \
            SET qty = EXCLUDED.qty, \
                amount = EXCLUDED.qty * i.avg_cost, \
                updated_at = now() \
         RETURNING i.id"
    )
}

/// 执行巡检批量修正，返回实际修正行数（差额调整 + 反向建档都算，多桶被跳过的行不算）。
///
/// 两条语句同一段 `source_cte`、同一个 executor（`dry_run=false` 时是同一个事务），
/// 各自按 `bucket_count` 分流，**互不相交**（`= 1` 与 `= 0`），所以顺序无关；
/// 也绝不按 (物料, 仓库) 循环 —— 每类都是一条集合语句。
pub(crate) async fn align_cost_qty_batch(ex: &impl SqlExecutor, source_cte: &str, params: Vec<Value>) -> DbResult<u64> {
    let updated = ex
        .query_sql(align_cost_qty_delta_sql(source_cte), params.clone())
        .await?;
    let inserted = ex.query_sql(align_cost_qty_insert_sql(source_cte), params).await?;
    Ok(u64::try_from(updated.len() + inserted.len()).unwrap_or(u64::MAX))
}

// ============================================================================
// 二、库位账 biz_inventory_location（只记数量）
// ============================================================================

/// 锁读库位账行（`location_id` 在本仓是 `biz_inventory_location.id`，不是 `biz_location.id`）。
///
/// `location_id = None` → 未分配库位桶（`location_id IS NULL` 的那一行，
/// `uk_biz_inv_loc … NULLS NOT DISTINCT` 保证最多一行）。
pub(crate) async fn lock_location(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
) -> DbResult<Option<(Uuid, Decimal)>> {
    let sql = match location_id {
        Some(_) => "SELECT id, qty FROM biz_inventory_location WHERE id = ?::uuid FOR UPDATE".to_string(),
        None => "SELECT id, qty FROM biz_inventory_location \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
                  FOR UPDATE"
            .to_string(),
    };
    let params = match location_id {
        Some(row_id) => vec![Value::from(&row_id)],
        None => vec![Value::from(&material_id), Value::from(&warehouse_id)],
    };
    let row = ex.query_first_sql(sql, params).await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some((row.get_typed("id")?, row.get_typed("qty")?))),
    }
}

/// **库位账加法**（P6：过账要同时写成本账 + 库位账）。返回 `(变动前, 变动后)`。
///
/// 两种分支（`location_id` 在本仓是 `biz_inventory_location.id`，不是 `biz_location.id`）：
///
/// * `Some(row_id)`：**按主键 UPDATE 这一行**，并校验它的 `material_id` / `warehouse_id`
///   与本次明细一致（库位下拉里可能选到别的物料／别的仓的库位行）；
/// * `None`：往 `(material, warehouse, location_id IS NULL)` 这个**未分配库位桶**加
///   （`uk_biz_inv_loc … NULLS NOT DISTINCT` 保证最多一行，NULL 也占一行、不会重复）。
///
/// 为什么有行 id 时不走「三列 upsert」：那会把库位行 id 当成库位字典 id 塞进
/// `biz_inventory_location.location_id`，**新建**一行（qty 从 0 开始），库位账合计立刻大于成本账。
///
/// `locked_qty` 是**预留列**（全仓没有任何占用/拣货流程写它，见 `docs/ERP-摆设字段口径.md`）：
/// 新建行显式写 0，已有行不动。
pub(crate) async fn add_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<(Decimal, Decimal)> {
    let qty_text = qty.round_dp(4).to_string();

    // ---------- 分支 A：明细指定了库位库存行 → 按主键加 ----------
    if let Some(row_id) = location_id {
        let row = ex
            .query_first_sql(
                "SELECT material_id, warehouse_id, qty FROM biz_inventory_location \
                  WHERE id = ?::uuid FOR UPDATE"
                    .to_string(),
                vec![Value::from(&row_id)],
            )
            .await?
            .ok_or_else(|| {
                DbError::InvalidValue(format!(
                    "库位账行不存在（location_id = {row_id}）：建单时校验通过，过账前被删掉了"
                ))
            })?;
        let row_material: Uuid = row.get_typed("material_id")?;
        let row_warehouse: Uuid = row.get_typed("warehouse_id")?;
        if row_material != material_id || row_warehouse != warehouse_id {
            return Err(DbError::InvalidValue(format!(
                "明细 location_id（{row_id}）指向的库位账行属于物料 {row_material} / 仓库 {row_warehouse}，\
                 与本次过账的物料 {material_id} / 仓库 {warehouse_id} 不一致（库位选错了）"
            )));
        }
        let before_qty: Decimal = row.get_typed("qty")?;
        let updated = ex
            .query_first_sql(
                "UPDATE biz_inventory_location \
                    SET qty = qty + NULLIF(?::text,'')::numeric, last_txn_at = ?, updated_at = ? \
                  WHERE id = ?::uuid RETURNING qty"
                    .to_string(),
                vec![
                    Value::Text(qty_text),
                    Value::from(now),
                    Value::from(now),
                    Value::from(&row_id),
                ],
            )
            .await?
            .ok_or_else(|| {
                DbError::InvalidValue("库位账 UPDATE 未返回行（biz_inventory_location 的 RETURNING 不应为空）".into())
            })?;
        return Ok((before_qty, updated.get_typed("qty")?));
    }

    // ---------- 分支 B：未分配库位桶（location_id IS NULL）----------
    let existing = ex
        .query_first_sql(
            "SELECT qty FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
              FOR UPDATE"
                .to_string(),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?;
    let before_qty: Decimal = match existing {
        Some(row) => row.get_typed("qty")?,
        None => Decimal::ZERO,
    };
    // `ON CONFLICT (material_id, warehouse_id, location_id)` 能直接推断到
    // `uk_biz_inv_loc … NULLS NOT DISTINCT`（PG 18 实测：`NULL` 同样命中冲突分支，
    // 不会因为「NULL 不相等」再插一行）。**不要**改成 `COALESCE(…)` 表达式形式 ——
    // 那是 `uk_biz_inv_mat_wh_loc`（biz_inventory）的口径，套到这里会 42P10。
    let row = ex
        .query_first_sql(
            "INSERT INTO biz_inventory_location AS il \
                 (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                  last_txn_at, created_at, updated_at) \
             VALUES (?, ?::uuid, ?::uuid, NULL, NULLIF(?::text,'')::numeric, 0, ?, ?, ?) \
             ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET \
                 qty = il.qty + EXCLUDED.qty, \
                 last_txn_at = EXCLUDED.last_txn_at, \
                 updated_at = EXCLUDED.updated_at \
             RETURNING qty"
                .to_string(),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::Text(qty_text),
                Value::from(now),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue("库位账 upsert 未返回行（biz_inventory_location 的 RETURNING 不应为空）".into())
        })?;
    Ok((before_qty, row.get_typed("qty")?))
}

/// **库位账反向冲减**（与 [`add_location_qty`] 的两个分支一一对应）。
///
/// 退不回来（库位账 < 本单数量）报 [`SHORTFALL_PREFIX`]，让 handler 回 `409`
/// 而不是把库位账冲成负数。
pub(crate) async fn subtract_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<(Decimal, Decimal)> {
    let qty_text = qty.round_dp(4).to_string();

    // ---------- 分支 A：明细指定了库位库存行 → 按主键退 ----------
    if let Some(row_id) = location_id {
        let row = ex
            .query_first_sql(
                "SELECT material_id, warehouse_id, qty FROM biz_inventory_location \
                  WHERE id = ?::uuid FOR UPDATE"
                    .to_string(),
                vec![Value::from(&row_id)],
            )
            .await?
            .ok_or_else(|| {
                DbError::InvalidValue(format!(
                    "库位账行不存在（location_id = {row_id}）：过账时写过，作废前被删掉了"
                ))
            })?;
        let row_material: Uuid = row.get_typed("material_id")?;
        let row_warehouse: Uuid = row.get_typed("warehouse_id")?;
        let before_qty: Decimal = row.get_typed("qty")?;
        if row_material != material_id || row_warehouse != warehouse_id {
            return Err(DbError::InvalidValue(format!(
                "库位账行 {row_id} 属于物料 {row_material} / 仓库 {row_warehouse}，\
                 与作废的物料 {material_id} / 仓库 {warehouse_id} 不一致"
            )));
        }
        if before_qty - qty < Decimal::ZERO {
            return Err(DbError::InvalidValue(format!(
                "{SHORTFALL_PREFIX}库位账 库存 {} < 本单 {}",
                before_qty.round_dp(4),
                qty.round_dp(4)
            )));
        }
        let updated = ex
            .query_first_sql(
                "UPDATE biz_inventory_location \
                    SET qty = qty - NULLIF(?::text,'')::numeric, last_txn_at = ?, updated_at = ? \
                  WHERE id = ?::uuid RETURNING qty"
                    .to_string(),
                vec![
                    Value::Text(qty_text),
                    Value::from(now),
                    Value::from(now),
                    Value::from(&row_id),
                ],
            )
            .await?
            .ok_or_else(|| {
                DbError::InvalidValue("库位账反向冲减未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into())
            })?;
        return Ok((before_qty, updated.get_typed("qty")?));
    }

    // ---------- 分支 B：未分配库位桶（location_id IS NULL）----------
    let existing = ex
        .query_first_sql(
            "SELECT qty FROM biz_inventory_location \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
              FOR UPDATE"
                .to_string(),
            vec![Value::from(&material_id), Value::from(&warehouse_id)],
        )
        .await?;
    let Some(row) = existing else {
        // 历史单（P6 之前过账）从来没写过库位账：库里就是 0，退 0 才对；
        // 但若本单数量 > 0 而库位账一行都没有，说明账本身不成立 → 报错交给人。
        return Err(DbError::InvalidValue(format!(
            "{SHORTFALL_PREFIX}库位账 没有该物料×仓库的未分配库位行（库存 0 < 本单 {}）",
            qty.round_dp(4)
        )));
    };
    let before_qty: Decimal = row.get_typed("qty")?;
    if before_qty - qty < Decimal::ZERO {
        return Err(DbError::InvalidValue(format!(
            "{SHORTFALL_PREFIX}库位账 库存 {} < 本单 {}",
            before_qty.round_dp(4),
            qty.round_dp(4)
        )));
    }
    let updated = ex
        .query_first_sql(
            "UPDATE biz_inventory_location \
                SET qty = qty - NULLIF(?::text,'')::numeric, last_txn_at = ?, updated_at = ? \
              WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
              RETURNING qty"
                .to_string(),
            vec![
                Value::Text(qty_text),
                Value::from(now),
                Value::from(now),
                Value::from(&material_id),
                Value::from(&warehouse_id),
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("库位账反向冲减（未分配桶）未返回行（UPDATE 命中 0 行）".into()))?;
    Ok((before_qty, updated.get_typed("qty")?))
}

/// 库位账按行 id 扣减（出库：只记数量、不记成本）。返回扣减后的数量。
pub(crate) async fn deduct_location_qty(
    ex: &impl SqlExecutor,
    location_row_id: Uuid,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<Decimal> {
    let row = ex
        .query_first_sql(
            "UPDATE biz_inventory_location \
                SET qty = qty - NULLIF(?::text,'')::numeric, last_txn_at = ?, updated_at = ? \
              WHERE id = ?::uuid RETURNING qty"
                .to_string(),
            vec![
                Value::Text(qty.round_dp(4).to_string()),
                Value::from(now),
                Value::from(now),
                Value::from(&location_row_id),
            ],
        )
        .await?
        .ok_or_else(|| DbError::InvalidValue("出库扣减库位账未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into()))?;
    row.get_typed("qty")
}

/// 库位账回加（作废出库单）：行存在 → `qty += 本次数量`；行不存在 → 往未分配桶建档。
pub(crate) async fn restore_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let qty_text = qty.round_dp(4).to_string();
    let updated = match location_id {
        Some(row_id) => {
            ex.query_first_sql(
                "UPDATE biz_inventory_location SET qty = qty + NULLIF(?::text,'')::numeric, \
                        last_txn_at = ?, updated_at = ? WHERE id = ?::uuid RETURNING id"
                    .to_string(),
                vec![
                    Value::Text(qty_text.clone()),
                    Value::from(now),
                    Value::from(now),
                    Value::from(&row_id),
                ],
            )
            .await?
        }
        None => {
            ex.query_first_sql(
                "UPDATE biz_inventory_location SET qty = qty + NULLIF(?::text,'')::numeric, \
                        last_txn_at = ?, updated_at = ? \
                  WHERE material_id = ?::uuid AND warehouse_id = ?::uuid AND location_id IS NULL \
                 RETURNING id"
                    .to_string(),
                vec![
                    Value::Text(qty_text.clone()),
                    Value::from(now),
                    Value::from(now),
                    Value::from(&material_id),
                    Value::from(&warehouse_id),
                ],
            )
            .await?
        }
    };
    if updated.is_some() {
        return Ok(());
    }
    // 行不存在：往未分配库位桶建档（`ON CONFLICT` 目标与 uk_biz_inv_loc 一致，NULL 也命中）
    ex.exec_sql(
        "INSERT INTO biz_inventory_location \
             (id, material_id, warehouse_id, location_id, qty, locked_qty, last_txn_at, created_at, updated_at) \
         VALUES (?, ?::uuid, ?::uuid, NULL, NULLIF(?::text,'')::numeric, 0, ?, ?, ?) \
         ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE SET \
             qty = biz_inventory_location.qty + EXCLUDED.qty, \
             last_txn_at = EXCLUDED.last_txn_at, updated_at = EXCLUDED.updated_at"
            .to_string(),
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(qty_text),
            Value::from(now),
            Value::from(now),
            Value::from(now),
        ],
    )
    .await?;
    Ok(())
}

/// **find-or-create** 库位账行：`(物料, 仓库, 库位字典)` → `biz_inventory_location.id`。
///
/// 这是 P2-4 的核心原语：过账时按用户选的库位建账（qty 从 0 起，随后的加法把它加上去），
/// 因此**不需要**用户在收货表单里「先有一条该物料的库存账行」。
///
/// 冲突目标必须写成三列（`uk_biz_inv_loc … NULLS NOT DISTINCT`）；这里 `location_id`
/// 恒非空（走默认库位 / 未分配桶的分支不经过本函数），不会触及 NULL 折叠。
pub(crate) async fn ensure_location_row(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_dict_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let row = ex
        .query_first_sql(
            "INSERT INTO biz_inventory_location AS il \
                 (id, material_id, warehouse_id, location_id, qty, locked_qty, \
                  created_at, updated_at) \
             VALUES (?, ?::uuid, ?::uuid, ?::uuid, 0, 0, ?, ?) \
             ON CONFLICT (material_id, warehouse_id, location_id) DO UPDATE \
                 SET updated_at = EXCLUDED.updated_at \
             RETURNING il.id"
                .to_string(),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&material_id),
                Value::from(&warehouse_id),
                Value::from(&location_dict_id),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue("库位账行 upsert 未返回行（biz_inventory_location 的 RETURNING 不应为空）".into())
        })?;
    row.get_typed("id")
}

/// **一物一码数量派生（库位账侧）**：以自身行驱动（`il.id` 对应序列号的 `location_id`；
/// NULL 桶单独匹配），把 `qty` 设为在库件数。
pub(crate) async fn sync_location_qty_from_serials(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
) -> DbResult<()> {
    ex.exec_sql(
        "UPDATE biz_inventory_location il \
            SET qty = (SELECT count(*) FROM biz_material_serial s \
                        WHERE s.material_id = il.material_id \
                          AND s.warehouse_id = il.warehouse_id \
                          AND s.status = 'IN_STOCK' AND s.deleted_at IS NULL \
                          AND ((il.location_id IS NULL AND s.location_id IS NULL) \
                               OR s.location_id = il.id)), \
                last_txn_at = now(), updated_at = now() \
          WHERE il.material_id = ?::uuid AND il.warehouse_id = ?::uuid"
            .to_string(),
        vec![Value::from(&material_id), Value::from(&warehouse_id)],
    )
    .await?;
    Ok(())
}

// ============================================================================
// 三、流水 biz_inventory_txn（只增不改）
// ============================================================================

/// 一条库存流水（22 列全在这里，各业务只填值）。
///
/// `qty` 恒为正数（`ck_biz_inv_txn_qty` 禁负数），方向看 `txn_type`（`ck_biz_inv_txn_type`
/// 只认 `IN` / `OUT`）。数值字段用 `Decimal` 直传、由调用方决定精度
/// （`unit_cost` 有的口径 round 到 6、有的原样），本函数只负责**格式化与绑定**，
/// 不再二次舍入 —— 否则「Rust 舍入 vs PG 舍入」会在半分位差 1 分钱。
#[derive(Debug, Clone)]
pub(crate) struct TxnRecord<'a> {
    pub material_id: Uuid,
    pub warehouse_id: Uuid,
    /// 方向：`IN` / `OUT`。
    pub txn_type: &'a str,
    /// 业务类型（`PURCHASE_IN` / `STOCK_OUT_CANCEL` / `TRANSFER` …）。
    pub biz_type: &'a str,
    /// 单据类型（`STOCK_IN` / `STOCK_OUT` / `TRANSFER` / `STOCK_TAKE` / `OPENING` …）。
    pub doc_type: &'a str,
    pub doc_id: Uuid,
    pub doc_no: &'a str,
    pub line_no: i32,
    /// 变动数量（恒正）。
    pub qty: Decimal,
    /// 单价（调用方按需 `round_dp(6)`）。
    pub unit_cost: Decimal,
    /// 变动金额。
    pub amount: Decimal,
    /// 变动前账面三值。
    pub before: InventorySnapshot,
    /// 变动后账面三值。
    pub after: InventorySnapshot,
    pub operator: Option<Uuid>,
    /// 期间码（`yyyy-MM`）。`Some` = 用调用方给的会计期间（**期初**录入按期间、不按当天，
    /// `save_inventory("2034-09")` 的流水必须落在 `2034-09`，即便今天是 2026-11）；
    /// `None` = 按 `now` 折算（CST）。
    pub period_code: Option<&'a str>,
    /// 备注（`None` / 空串都落 `NULL`）。
    pub remark: Option<&'a str>,
    pub now: DateTime<Utc>,
}

/// 写一条库存流水（**只增不改**；22 列含 `remark`，空串按 `NULL` 落库）。
pub(crate) async fn insert_txn(ex: &impl SqlExecutor, txn: &TxnRecord<'_>) -> DbResult<u64> {
    let period_code = txn
        .period_code
        .map(str::to_string)
        .unwrap_or_else(|| serial_model::period_code_of(txn.now));
    ex.exec_sql(
        "INSERT INTO biz_inventory_txn \
             (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, \
              qty, unit_cost, amount, before_qty, before_amount, before_cost, \
              after_qty, after_amount, after_cost, period_code, operator_id, remark, created_at) \
         VALUES (?, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,''), ?)"
            .to_string(),
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(&txn.material_id),
            Value::from(&txn.warehouse_id),
            Value::from(txn.txn_type),
            Value::from(txn.biz_type),
            Value::from(txn.doc_type),
            Value::from(&txn.doc_id),
            Value::from(txn.doc_no),
            Value::from(txn.line_no),
            Value::Text(txn.qty.to_string()),
            Value::Text(txn.unit_cost.to_string()),
            Value::Text(txn.amount.to_string()),
            Value::Text(txn.before.qty.to_string()),
            Value::Text(txn.before.amount.to_string()),
            Value::Text(txn.before.avg_cost.to_string()),
            Value::Text(txn.after.qty.to_string()),
            Value::Text(txn.after.amount.to_string()),
            Value::Text(txn.after.avg_cost.to_string()),
            Value::from(period_code.as_str()),
            uuid_text(txn.operator),
            Value::Text(txn.remark.unwrap_or_default().to_string()),
            Value::from(txn.now),
        ],
    )
    .await
}

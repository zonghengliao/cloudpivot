//! 收货入库单数据层 + 过账（移动加权 / 一物一码）（批次 D · P2-B1）。
//!
//! 依据：`backend/migrations/business/V0145__purchase_order_amounts_and_stock_in.up.sql`（P2-A1 建的两张表）
//! 与 `docs/ERP-批次1-产品需求PRD.md` §3.2（成本核算，380-470 行）。
//! **表结构以 `\d` 实测为准**（2026-10-31 开工实测 `cloudpivot_dev`，已到 V0146）：
//!
//! ```text
//! biz_stock_in       id, in_no, in_type(PURCHASE/RETURN/OTHER), source_order_id, supplier_id,
//!                    warehouse_id(NOT NULL), in_date, status(DRAFT/POSTED/CANCELLED),
//!                    currency_code(CHAR(3)), total_qty, total_amount, total_tax, total_with_tax,
//!                    invoice_received, voucher_id, payable_id, remark, posted_at, posted_by,
//!                    cancelled_at, cancelled_by, cancel_reason,
//!                    created_at, updated_at, create_by, update_by, deleted_at
//! biz_stock_in_item  id, stock_in_id, line_no, source_item_id, material_id, unit_id(NOT NULL),
//!                    qty, price, price_incl, tax_rate, amount, tax_amount, with_tax_amount,
//!                    cost_price, location_id, lot_no, remark, created_at, updated_at
//! biz_inventory      id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost(18,6),
//!                    amount(18,4), last_txn_at, …
//! biz_inventory_txn  22 列（qty > 0 禁负数、txn_type 只认 IN/OUT、period_code NOT NULL）
//! ```
//!
//! # 卡面没写、但库里有约束的六处（照卡面写必然挂）
//!
//! 1. **明细 `unit_id` 是 `NOT NULL`**：卡面 body 里没有这个字段 → 由物料的基础单位带出，不让客户端传。
//! 2. **`biz_stock_in_item` 的 `ck_biz_si_item_tax` 是精确恒等**（`with_tax_amount = amount + tax_amount`），
//!    回写的三金额必须逐行算好，不能靠库里补。
//! 3. **`ck_biz_si_item_qty CHECK (qty > 0)`**：0 数量的行插不进库 → 建单即 400（与 P2-A2 同口径）。
//! 4. **`ck_biz_si_supplier`**：`in_type='PURCHASE'` 时 `supplier_id` 必填（本卡只产生 `PURCHASE`）。
//! 5. **`biz_inventory` 的唯一键是表达式索引** `uk_biz_inv_mat_wh_loc
//!    (material_id, warehouse_id, COALESCE(location_id, '000…0'::uuid))` → 所有 upsert 的
//!    `ON CONFLICT` 目标必须**写成同一个表达式**，否则真库 42P10（W3a 实测）。
//! 6. **`biz_inventory_txn` 有 `period_code`（`yyyy-MM`）与 `after_*` 三列**：卡面只要求 `before_*` 快照，
//!    但列是 `NOT NULL`，`after_*` 用 upsert 回读的真实落库值填（不是「before ± 数量」算出来的）。
//!
//! # 过账口径（PRD §3.2.3 / §3.2.6，本卡核心）
//!
//! ```text
//! new_qty    = old_qty + in_qty
//! new_amount = old_amount + in_amount      ← 加法，金额是权威值，绝不用「单价×数量」重算
//! new_avg    = round(new_amount / new_qty, 6)  (new_qty > 0)
//! 结存 ≤ 0 时入库：new_avg = 本次入库单价（旧成本基准已失效，PRD §3.2.6 边界表）
//! 加锁：SELECT … FOR UPDATE，**多行按 material_id 升序**（防两个收货单互相死锁）
//! ```
//!
//! **入库成本用不含税金额**（PRD §3.2：进项税是价外可抵扣的，不进存货成本）——
//! `amount` 列与 `biz_inventory_txn.amount` 都只加 `biz_stock_in_item.amount`（不含税），
//! 不是 `with_tax_amount`。`cost_price` 落不含税单价。
//!
//! # 与卡面 §三 e) 的一处**实现差异**（有意为之，写在这里免得下游踩坑）
//!
//! 卡面写「一物一码物料调用 `serial_model::batch_generate`」。实测该函数（W2b/W2c-1）内部
//! 会走 `link_inventory` → `recompute_quantity`（把 `qty` **重算成序列号件数**）并**再写一条
//! `SERIAL_IN` 流水**。若照卡面调用，同一张收货单会：
//!
//! * 写出两条 `IN` 流水（`PURCHASE_IN` + `SERIAL_IN`）→ 台账双计；
//! * 被 `recompute_quantity` 把 `amount` 改写成 `qty × avg_cost`（`avg_cost` 还是 0）
//!   → **过账算出来的移动加权金额被静默抹掉**。
//!
//! 所以本模块只复用 `serial_model` 的**取号原语**（[`crate::business::erp::serial_model::take_numbers`]、
//! [`crate::business::erp::serial_model::format_serial_no`]、
//! [`crate::business::erp::serial_model::pair_payload`]）自己落序列号行
//! （`source_type='PURCHASE'`、`source_id=收货单 id`），库存侧只有本模块**一处**加法 +
//! **一条** `PURCHASE_IN` 流水。不变式「一物一码物料 `qty` = 在库序列号件数」仍然成立
//! （本次入库的增量两边相等），且成本链不被破坏。
//!
//! # 作废（`POSTED → CANCELLED`）本卡**只改状态**
//!
//! 卡面 §二 明写「作废需红冲 —— 但凭证是 P2-C1，本卡先只改状态」。所以
//! [`apply_status`] 在 `POSTED → CANCELLED` 时**不回滚库存**，只写状态与 `cancel_reason`，
//! 并打一条 WARN 日志点名「库存未冲回，待 P2-C1 红冲」。这条欠账写在证据文档里。
//!
//! # P6：过账同时写**两本账**（成本账 + 库位账）
//!
//! P2-B1 的过账只写成本账（`biz_inventory`，含 `location_id` 维度）与流水，
//! `biz_inventory_location`（数量账 / 库位账）**一行不写** —— 结果是
//! 「即时库存」Tab（成本账）与「库位库存」Tab（库位账）永远对不上，违反不变式③
//! `SUM(库位账 qty) = 成本账 qty`，后续按库位出库 / 盘点 / 调拨都查不到这批货。
//!
//! 现在一条明细落地**两笔**，同一个 `ex`（同一个事务）：
//!
//! ```text
//! ③  成本账：apply_cost       （biz_inventory，material + warehouse + location 三列唯一键）
//! ③b 库位账：apply_location_qty（biz_inventory_location，同一把锁顺序）
//! ```
//!
//! `location_id` 的两处语义不要混（见 [`apply_location_qty`] 文档）：
//! 明细上的 `location_id` 是 **`biz_inventory_location.id`**，而 `biz_inventory_location.location_id`
//! 列存的是 `biz_location.id`。所以「有库位」走按主键 UPDATE，「无库位」走 NULL 桶 upsert。
//!
//! **存量数据不补**：本卡只保证「新单正确」（增量修复）。已过账的历史收货单没有库位账行，
//! 差异留给人决策后再单独开卡补，绝不在迁移或代码里批量 INSERT 历史数据。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 收货入库单是 ERP 单据表（V0145 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:stock-in:* 权限码控制。
//! 列表分页走 scope::paginate_unscoped（闸①口径）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列，不需要数据范围。
//! SCOPE-EXEMPT: biz_inventory / biz_inventory_txn 是仓储台账，无归属列（口径同 erp/model.rs）。
//! ```
//!
//! # 事务边界
//!
//! 写函数一律吃 `&impl SqlExecutor`（`Db` / `Transaction` 都实现），由
//! [`crate::business::erp::stockin_handler`] 开事务：**过账的「改状态 + 加减库存 + 写流水 +
//! 生成序列号 + 回写采购单已收数量」必须在同一个事务里**（硬红线 2）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::RoundingStrategy;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::{PurchaseOrderItemRow, PurchaseOrderRow, normalize_page};
use crate::business::erp::serial_model;
use crate::business::shared::scope;
use crate::business::shared::state_machine;
use crate::utils;

/// 金额 / 单价舍入策略：**四舍五入**（与 PG `ROUND(numeric, n)` 同口径）。
///
/// `rust_decimal` 的默认 `round_dp` 是「四舍六入五成双」，`.005` 边界会比库里少 1 分钱
/// （W3c 在成本转移上实测过）。
const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

// ==================== 表名 / 常量 ====================

/// 单据头表。
pub const TABLE: &str = "biz_stock_in";

/// 明细表（无 `deleted_at`，主单软删后明细留在库里）。
pub const ITEM_TABLE: &str = "biz_stock_in_item";

/// 入库类型：采购入库（本卡唯一产生的类型）。
pub const IN_TYPE_PURCHASE: &str = "PURCHASE";

/// 状态：草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已过账（库存已加）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已作废。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 本卡负责的合法边（卡面 §二）。
///
/// P10 起**唯一真相在 [`crate::business::shared::state_machine`]**：这里只是同一份
/// 常量数组的别名，`state_machine::transition` 还会核对两者一致 ——
/// 想在调用点偷偷多放一条边会立刻 500，而不是静默绕过状态机。
pub const LEGAL_EDGES: [(&str, &str); 3] = state_machine::STOCK_IN_EDGES;

/// 流水方向：入（`ck_biz_inv_txn_type` 只认 `IN` / `OUT`）。
pub const TXN_TYPE_IN: &str = "IN";

/// 流水业务类型：采购入库（卡面 §三 d）。
pub const BIZ_TYPE_PURCHASE_IN: &str = "PURCHASE_IN";

/// 流水单据类型：收货入库单。
pub const DOC_TYPE_STOCK_IN: &str = "STOCK_IN";

/// 序列号来源类型（`biz_material_serial.source_type` 白名单见 serial_model::SOURCE_TYPES）。
pub const SERIAL_SOURCE_PURCHASE: &str = "PURCHASE";

/// 零 UUID（`COALESCE(location_id, 零 uuid)` 的唯一键口径）。
pub const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// 单号前缀（`IN-yyyyMMdd-000001`）。
pub const NO_PREFIX: &str = "IN";

/// 详情里的汇总口径说明（前端展示「金额为不含税」用）。
pub const STOCK_IN_SUMMARY_PLACEHOLDER: &str =
    "金额均为不含税（进项税价外可抵扣，不进存货成本）；结存单价 = 结存金额 / 结存数量（6 位）";

/// 金额一律四舍五入到 2 位（与 PG `ROUND(numeric, 2)` 同口径）。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, ROUNDING)
}

/// 单价一律 6 位。
pub fn unit_price(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(6, ROUNDING)
}

/// 流水号补零宽度（超过 6 位不截断，宁可长一位也不撞号）。
pub const NO_SEQ_WIDTH: usize = 6;

/// `biz_seq.seq_type`（本卡新增的取号行）。
pub const SEQ_TYPE: &str = "IN";

/// 状态中文标签（错误文案与前端徽标共用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_POSTED => "已过账",
        STATUS_CANCELLED => "已作废",
        _ => "未知状态",
    }
}

/// 某状态能去的下一个状态。
pub fn next_statuses(from: &str) -> Vec<&'static str> {
    LEGAL_EDGES
        .iter()
        .filter(|(f, _)| *f == from)
        .map(|(_, t)| *t)
        .collect()
}

/// 状态机判定：只有 [`LEGAL_EDGES`] 里的 3 条边放行。
pub fn can_transition(from: &str, to: &str) -> bool {
    LEGAL_EDGES.iter().any(|(f, t)| *f == from && *t == to)
}

/// 非法流转的 409 文案（点名当前态的可选去向）。
pub fn transition_rejected_message(from: &str, to: &str) -> String {
    let allowed = next_statuses(from);
    if allowed.is_empty() {
        return format!(
            "收货入库单当前是「{}」（{from}），没有可流转的下一个状态",
            status_label(from)
        );
    }
    format!(
        "收货入库单当前是「{}」（{from}），不能直接流转到「{}」（{to}）：允许的去向是 {}",
        status_label(from),
        status_label(to),
        allowed
            .iter()
            .map(|status| format!("「{}」（{}）", status_label(status), status))
            .collect::<Vec<_>>()
            .join(" / ")
    )
}

// ==================== 纯逻辑：成本口径 ====================

/// 一行的过账成本（**不含税**，PRD §3.2）。
///
/// `amount` 是与 `biz_inventory.amount` 同标度（4 位）的不含税金额，
/// `unit_cost` 是 6 位的入库单价（`amount / qty`，供流水 `unit_cost` / 明细 `cost_price`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostBasis {
    /// 入库数量（4 位）。
    pub qty: Decimal,
    /// 不含税金额（4 位，权威值）。
    pub amount: Decimal,
    /// 入库单价（6 位）。
    pub unit_cost: Decimal,
}

impl CostBasis {
    /// 由一行明细的 `(qty, 不含税金额)` 构造：单价 6 位四舍五入（与 PG `ROUND` 同口径）。
    pub fn of(qty: Decimal, amount: Decimal) -> Self {
        let qty = qty.round_dp(4);
        let amount = amount.round_dp(4);
        let unit_cost = if qty.is_zero() {
            Decimal::ZERO
        } else {
            (amount / qty).round_dp_with_strategy(6, ROUNDING)
        };
        Self {
            qty,
            amount,
            unit_cost,
        }
    }
}

/// 库存快照（流水前后六列里的三项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventorySnapshot {
    /// 数量（4 位）。
    pub qty: Decimal,
    /// 金额（4 位）。
    pub amount: Decimal,
    /// 移动加权单价（6 位）。
    pub avg_cost: Decimal,
}

impl InventorySnapshot {
    /// 全 0（该 (物料, 仓库, 库位) 还没建账）。
    pub fn zero() -> Self {
        Self {
            qty: Decimal::ZERO,
            amount: Decimal::ZERO,
            avg_cost: Decimal::ZERO,
        }
    }

    /// 由「变动后」快照反推变动前（`None` 分支下 upsert 走了 `DO UPDATE` 时用）。
    ///
    /// `before_qty = after_qty − in_qty`、`before_amount = after_amount − in_amount` 是
    /// **精确**的（加法可逆）；`before_cost` 按 `before_amount / before_qty` 回算——
    /// 与库里 `avg_cost` 的构造方式（`ROUND(amount/qty, 6)`）同源，因此与锁读到的值一致。
    pub fn derived_before(after: Self, qty: Decimal, amount: Decimal) -> Self {
        let before_qty = after.qty - qty;
        let before_amount = after.amount - amount;
        let before_cost = if before_qty.is_zero() {
            Decimal::ZERO
        } else {
            (before_amount / before_qty).round_dp_with_strategy(6, ROUNDING)
        };
        Self {
            qty: before_qty,
            amount: before_amount,
            avg_cost: before_cost,
        }
    }
}

/// 过账后的库存三值（[`apply_cost`] 的回读结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedInventory {
    /// 变动前快照（流水 `before_*`）。
    pub before: InventorySnapshot,
    /// 变动后快照（流水 `after_*`）。
    pub after: InventorySnapshot,
}

// ==================== 取号（`IN-yyyyMMdd-000001`） ====================

/// 组装单号。
pub fn format_in_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// `biz_seq(seq_type='IN')` 缺失时幂等补一行。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'IN', '收货入库单号', 'IN', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '收货入库单号：IN-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'IN' AND deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await
    .or_else(|err| match err {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='IN')` 一个号（三分支：同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
    // SCOPE-EXEMPT: 同上，biz_seq 是全局取号规则字典。
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = 'IN' AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("current_no"))
        .transpose()
}

/// 取一个收货单号（周期键按北京时间，与 `IN-yyyyMMdd-*` 的 `yyyyMMdd` 同源）。
pub async fn next_in_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = serial_model::period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_in_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='IN') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 写库结构 ====================

/// 建单 / 改单的单头字段。
#[derive(Debug, Clone)]
pub struct NewStockIn<'a> {
    /// 供应商（`in_type='PURCHASE'` 时库约束要求必填）。
    pub supplier_id: Uuid,
    /// 收货仓库（列 `NOT NULL`）。
    pub warehouse_id: Uuid,
    /// 来源采购单（可空）。
    pub source_order_id: Option<Uuid>,
    /// 入库日期。
    pub in_date: NaiveDate,
    /// 发票是否已到（缺省 true，与列默认值一致）。
    pub invoice_received: bool,
    /// 备注。
    pub remark: Option<&'a str>,
}

/// 一行待落库的明细（金额已算好、`unit_id` 由物料带出）。
#[derive(Debug, Clone)]
pub struct NewStockInLine {
    /// 物料。
    pub material_id: Uuid,
    /// 计量单位（物料基础单位）。
    pub unit_id: Uuid,
    /// 来源采购明细行（可空）。
    pub source_item_id: Option<Uuid>,
    /// 数量（4 位，> 0）。
    pub qty: Decimal,
    /// 不含税单价（6 位）。
    pub price: Decimal,
    /// 含税单价（6 位快照）。
    pub price_incl: Decimal,
    /// 税率（百分数 0–100）。
    pub tax_rate: Decimal,
    /// 不含税金额（4 位）。
    pub amount: Decimal,
    /// 税额（4 位）。
    pub tax_amount: Decimal,
    /// 价税合计（4 位，恒等 `amount + tax_amount`）。
    pub with_tax_amount: Decimal,
    /// 库位（可空）。
    pub location_id: Option<Uuid>,
    /// 客户端给的单品序列号（空 = 过账时自动取号生成）。
    pub serial_ids: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
}

impl NewStockInLine {
    /// 该行的过账成本（不含税金额）。
    pub fn cost(&self) -> CostBasis {
        CostBasis::of(self.qty, self.amount)
    }
}

/// 单据汇总（与 `total_*` 四列一一对应；逐行舍入后相加）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// 合计数量。
    pub qty: Decimal,
    /// 不含税合计。
    pub amount: Decimal,
    /// 税额合计。
    pub tax: Decimal,
    /// 价税合计。
    pub with_tax: Decimal,
}

impl Totals {
    /// 由明细行算汇总。
    pub fn of(lines: &[NewStockInLine]) -> Self {
        Self {
            qty: lines.iter().fold(Decimal::ZERO, |acc, l| acc + l.qty),
            amount: lines.iter().fold(Decimal::ZERO, |acc, l| acc + l.amount),
            tax: lines
                .iter()
                .fold(Decimal::ZERO, |acc, l| acc + l.tax_amount),
            with_tax: lines
                .iter()
                .fold(Decimal::ZERO, |acc, l| acc + l.with_tax_amount),
        }
    }
}

// ==================== 读：行结构 ====================

/// `biz_stock_in` 列清单（`currency_code` 是 `CHAR(3)`：显式 `::text` 去掉补空格）。
pub const HEADER_COLUMNS: &str = "id, in_no, in_type, source_order_id, supplier_id, warehouse_id, \
     in_date, status, currency_code::text AS currency_code, total_qty, total_amount, total_tax, \
     total_with_tax, invoice_received, voucher_id, payable_id, remark, posted_at, posted_by, \
     cancelled_at, cancelled_by, cancel_reason, created_at, updated_at, create_by, update_by, \
     deleted_at";

/// 收货入库单头行。
#[derive(Debug, Clone)]
pub struct StockInRow {
    /// 主键。
    pub id: Uuid,
    /// 单号（`IN-yyyyMMdd-000001`，未删除内唯一）。
    pub in_no: String,
    /// 入库类型（`PURCHASE`）。
    pub in_type: String,
    /// 来源采购单。
    pub source_order_id: Option<Uuid>,
    /// 供应商。
    pub supplier_id: Option<Uuid>,
    /// 收货仓库。
    pub warehouse_id: Uuid,
    /// 入库日期。
    pub in_date: NaiveDate,
    /// 状态。
    pub status: String,
    /// 币种（`CHAR(3)`，已 `::text`）。
    pub currency_code: String,
    /// 合计数量。
    pub total_qty: Decimal,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计（库约束 `= total_amount + total_tax`）。
    pub total_with_tax: Decimal,
    /// 发票是否已到。
    pub invoice_received: bool,
    /// 凭证（P2-C1 才写）。
    pub voucher_id: Option<Uuid>,
    /// 应付单（P2-C1 才写）。
    pub payable_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 过账时间。
    pub posted_at: Option<DateTime<Utc>>,
    /// 过账人。
    pub posted_by: Option<Uuid>,
    /// 作废时间。
    pub cancelled_at: Option<DateTime<Utc>>,
    /// 作废人。
    pub cancelled_by: Option<Uuid>,
    /// 作废原因。
    pub cancel_reason: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl StockInRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            in_no: row.get_typed("in_no")?,
            in_type: row.get_typed("in_type")?,
            source_order_id: row.get_typed("source_order_id")?,
            supplier_id: row.get_typed("supplier_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            in_date: row.get_typed("in_date")?,
            status: row.get_typed("status")?,
            currency_code: row.get_typed("currency_code")?,
            total_qty: row.get_typed("total_qty")?,
            total_amount: row.get_typed("total_amount")?,
            total_tax: row.get_typed("total_tax")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            invoice_received: row.get_typed("invoice_received")?,
            voucher_id: row.get_typed("voucher_id")?,
            payable_id: row.get_typed("payable_id")?,
            remark: row.get_typed("remark")?,
            posted_at: row.get_typed("posted_at")?,
            posted_by: row.get_typed("posted_by")?,
            cancelled_at: row.get_typed("cancelled_at")?,
            cancelled_by: row.get_typed("cancelled_by")?,
            cancel_reason: row.get_typed("cancel_reason")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

/// `biz_stock_in_item` 列清单。
pub const ITEM_COLUMNS: &str = "id, stock_in_id, line_no, source_item_id, material_id, unit_id, \
     qty, price, price_incl, tax_rate, amount, tax_amount, with_tax_amount, cost_price, \
     location_id, lot_no, remark, created_at, updated_at";

/// 收货入库单明细行。
#[derive(Debug, Clone)]
pub struct StockInItemRow {
    /// 主键。
    pub id: Uuid,
    /// 主单。
    pub stock_in_id: Uuid,
    /// 行号（单内唯一）。
    pub line_no: i32,
    /// 来源采购明细行。
    pub source_item_id: Option<Uuid>,
    /// 物料。
    pub material_id: Uuid,
    /// 计量单位。
    pub unit_id: Uuid,
    /// 数量。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 含税单价。
    pub price_incl: Decimal,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 不含税金额。
    pub amount: Decimal,
    /// 税额。
    pub tax_amount: Decimal,
    /// 价税合计。
    pub with_tax_amount: Decimal,
    /// 入库成本单价（不含税）。
    pub cost_price: Decimal,
    /// 库位。
    pub location_id: Option<Uuid>,
    /// 批次号。
    pub lot_no: Option<String>,
    /// 行备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl StockInItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            stock_in_id: row.get_typed("stock_in_id")?,
            line_no: row.get_typed("line_no")?,
            source_item_id: row.get_typed("source_item_id")?,
            material_id: row.get_typed("material_id")?,
            unit_id: row.get_typed("unit_id")?,
            qty: row.get_typed("qty")?,
            price: row.get_typed("price")?,
            price_incl: row.get_typed("price_incl")?,
            tax_rate: row.get_typed("tax_rate")?,
            amount: row.get_typed("amount")?,
            tax_amount: row.get_typed("tax_amount")?,
            with_tax_amount: row.get_typed("with_tax_amount")?,
            cost_price: row.get_typed("cost_price")?,
            location_id: row.get_typed("location_id")?,
            lot_no: row.get_typed("lot_no")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 列表筛选（与 `model` 的 `*Filter` 同口径）。
#[derive(Debug, Clone, Default)]
pub struct StockInFilter {
    /// 单号 / 备注关键词。
    pub keyword: Option<String>,
    /// 状态。
    pub status: Option<String>,
    /// 供应商。
    pub supplier_id: Option<String>,
    /// 仓库。
    pub warehouse_id: Option<String>,
    /// 入库日期起。
    pub date_from: Option<NaiveDate>,
    /// 入库日期止。
    pub date_to: Option<NaiveDate>,
    /// 页码（从 1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 收货入库单分页排序（入库日期降序 → 单号降序，走 `idx_biz_si_date`）。
pub const LIST_ORDER: &str = "ORDER BY in_date DESC, in_no DESC";

/// `biz_stock_in` 的分页锚点（13 张 ERP 表都不参与数据权限）。
pub const LIST_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_stock_in");

/// 按主键取单头（未删除）。
pub async fn find_stock_in(id: Uuid) -> DbResult<Option<StockInRow>> {
    let sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE id = ? AND deleted_at IS NULL");
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(StockInRow::from_row).transpose()
}

/// 在事务/连接上按主键取单头并**加行锁**（过账用：CAS 之前先锁）。
pub async fn lock_stock_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<StockInRow>> {
    let row = ex
        .query_first_sql(
            format!(
                "SELECT {HEADER_COLUMNS} FROM {TABLE} \
                  WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    row.as_ref().map(StockInRow::from_row).transpose()
}

/// 取整单明细（行号升序）。
pub async fn items_of(id: Uuid) -> DbResult<Vec<StockInItemRow>> {
    let rows = db()
        .find_with_params(
            &format!(
                "SELECT {ITEM_COLUMNS} FROM {ITEM_TABLE} WHERE stock_in_id = ?::uuid \
                 ORDER BY line_no ASC"
            ),
            vec![Value::from(&id)],
        )
        .await?;
    rows.iter().map(StockInItemRow::from_row).collect()
}

/// 分页（关键词 / 状态 / 供应商 / 仓库 / 入库日期区间）。
pub async fn stock_in_page(q: &StockInFilter) -> DbResult<(Vec<StockInRow>, i64)> {
    let mut sql = format!("SELECT {HEADER_COLUMNS} FROM {TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = utils::like_pattern(q.keyword.as_deref()) {
        sql.push_str(" AND (in_no LIKE ? OR remark LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(supplier_id) = q.supplier_id.as_deref() {
        sql.push_str(" AND supplier_id = ?::uuid");
        params.push(Value::from(parse_id(supplier_id)?));
    }
    if let Some(warehouse_id) = q.warehouse_id.as_deref() {
        sql.push_str(" AND warehouse_id = ?::uuid");
        params.push(Value::from(parse_id(warehouse_id)?));
    }
    if let Some(date_from) = q.date_from {
        sql.push_str(" AND in_date >= ?");
        params.push(Value::from(date_from));
    }
    if let Some(date_to) = q.date_to {
        sql.push_str(" AND in_date <= ?");
        params.push(Value::from(date_to));
    }
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result =
        scope::paginate_unscoped(LIST_TARGET, &sql, params, Some(LIST_ORDER), page, page_size)
            .await?;
    let rows = result
        .list
        .iter()
        .map(StockInRow::from_row)
        .collect::<DbResult<Vec<StockInRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// DTO 里的 id 是 `String`：非法 UUID 一律报错（不静默跳过条件）。
fn parse_id(value: &str) -> DbResult<Uuid> {
    Uuid::parse_str(value.trim())
        .map_err(|err| DbError::InvalidValue(format!("非法 UUID `{value}`：{err}")))
}

// ==================== 写：建单 / 改单 / 状态 / 删 ====================

/// 插入明细（建单 / 改单共用），返回明细 id（按行序）。
async fn insert_items(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    lines: &[NewStockInLine],
) -> DbResult<Vec<Uuid>> {
    let mut ids = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let id = Uuid::now_v7();
        let cost = line.cost();
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (id, stock_in_id, line_no, source_item_id, material_id, unit_id, qty, price, \
                     price_incl, tax_rate, amount, tax_amount, with_tax_amount, cost_price, \
                     location_id, remark) \
                 VALUES (?, ?::uuid, ?, NULLIF(?::text,'')::uuid, ?::uuid, ?::uuid, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::uuid, NULLIF(?::text,''))"
            ),
            vec![
                Value::from(&id),
                Value::from(&stock_in_id),
                Value::from(i32::try_from(index + 1).unwrap_or(i32::MAX)),
                Value::Text(
                    line.source_item_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(&line.material_id),
                Value::from(&line.unit_id),
                Value::Text(line.qty.round_dp(4).to_string()),
                Value::Text(line.price.to_string()),
                Value::Text(line.price_incl.to_string()),
                Value::Text(line.tax_rate.to_string()),
                Value::Text(line.amount.round_dp(4).to_string()),
                Value::Text(line.tax_amount.round_dp(4).to_string()),
                Value::Text(line.with_tax_amount.round_dp(4).to_string()),
                Value::Text(cost.unit_cost.to_string()),
                Value::Text(line.location_id.map(|v| v.to_string()).unwrap_or_default()),
                Value::Text(line.remark.clone().unwrap_or_default()),
            ],
        )
        .await?;
        ids.push(id);
    }
    Ok(ids)
}

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
pub async fn create(
    ex: &impl SqlExecutor,
    new: &NewStockIn<'_>,
    lines: &[NewStockInLine],
    totals: &Totals,
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let in_no = next_in_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.id.map(|v| v.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, in_no, in_type, source_order_id, supplier_id, warehouse_id, in_date, status, \
                 total_qty, total_amount, total_tax, total_with_tax, invoice_received, remark, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, '{IN_TYPE_PURCHASE}', NULLIF(?::text,'')::uuid, ?::uuid, ?::uuid, \
                 NULLIF(?::text,'')::date, '{STATUS_DRAFT}', \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,''), \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(in_no.clone()),
            Value::Text(
                new.source_order_id
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            Value::from(&new.supplier_id),
            Value::from(&new.warehouse_id),
            Value::Text(new.in_date.to_string()),
            Value::Text(totals.qty.round_dp(4).to_string()),
            Value::Text(totals.amount.round_dp(4).to_string()),
            Value::Text(totals.tax.round_dp(4).to_string()),
            Value::Text(totals.with_tax.round_dp(4).to_string()),
            Value::from(new.invoice_received),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    insert_items(ex, id, lines).await?;
    // P10 铁律 ③：建单留痕（`from_status = NULL → DRAFT`），与建单**同一事务**。
    // 失败即整单回滚：宁可建不出单，也不要「有单没轨迹」。
    state_machine::log_created(
        ex,
        state_machine::ENTITY_STOCK_IN,
        id,
        &in_no,
        STATUS_DRAFT,
        operator.clone(),
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(id)
}

/// 改单（**仅 DRAFT 由调用方保证**）：单头整单覆盖 + 明细「删旧插新」。返回 `affected`。
pub async fn update(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewStockIn<'_>,
    lines: &[NewStockInLine],
    totals: &Totals,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {TABLE} \
                    SET source_order_id = NULLIF(?::text,'')::uuid, supplier_id = ?::uuid, \
                        warehouse_id = ?::uuid, in_date = NULLIF(?::text,'')::date, \
                        total_qty = NULLIF(?::text,'')::numeric, \
                        total_amount = NULLIF(?::text,'')::numeric, \
                        total_tax = NULLIF(?::text,'')::numeric, \
                        total_with_tax = NULLIF(?::text,'')::numeric, invoice_received = ?, \
                        remark = NULLIF(?::text,''), updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(
                    new.source_order_id
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(&new.supplier_id),
                Value::from(&new.warehouse_id),
                Value::Text(new.in_date.to_string()),
                Value::Text(totals.qty.round_dp(4).to_string()),
                Value::Text(totals.amount.round_dp(4).to_string()),
                Value::Text(totals.tax.round_dp(4).to_string()),
                Value::Text(totals.with_tax.round_dp(4).to_string()),
                Value::from(new.invoice_received),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::from(now),
                operator_text,
                Value::from(&id),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    // 明细没有 deleted_at，也不该留历史行：整单重建（行号从 1 连续）
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE stock_in_id = ?::uuid"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_items(ex, id, lines).await?;
    Ok(affected)
}

/// 只改备注（**入账后唯一允许改的业务列**，卡面 §二 ③ 白名单）。
///
/// `remark` 不进任何账务口径（不参与成本、不进凭证），所以 `POSTED` / `RECEIVED` 之后
/// 仍可补一句「这单是 XX 客户急件」；改它必须走这个函数，**不能**顺手把金额带过去。
pub async fn update_remark(
    ex: &impl SqlExecutor,
    id: Uuid,
    remark: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET remark = NULLIF(?::text,''), updated_at = ?, \
                update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 状态流转（CAS：`WHERE id = ? AND status = from`），返回 `affected`。
///
/// * `DRAFT → POSTED`：写 `posted_at` / `posted_by`。**库存增加不在这里做** ——
///   由 [`apply_cost`] / [`post_stock_in`] 在同一事务里完成。
/// * `DRAFT → CANCELLED`：写 `cancelled_at/by/reason`，不动库存（从未加过）。
/// * `POSTED → CANCELLED`：只改状态 + 原因；**库存不冲回**（红冲属 P2-C1），打 WARN。
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    let reason_text = reason.unwrap_or_default().to_string();
    let (sql, params) = match to {
        STATUS_POSTED => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_POSTED}', posted_at = ?, \
                        posted_by = NULLIF(?::text,'')::uuid, updated_at = ?, \
                        update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_CANCELLED => (
            format!(
                "UPDATE {TABLE} \
                    SET status = '{STATUS_CANCELLED}', cancelled_at = ?, \
                        cancelled_by = NULLIF(?::text,'')::uuid, cancel_reason = ?::text, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::Text(reason_text),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        other => {
            return Err(DbError::InvalidValue(format!(
                "状态流转的目标 `{other}` 不在 2 个可写目标里（DRAFT 只由建单产生）"
            )));
        }
    };
    ex.exec_sql(sql, params).await
}

/// 软删（**仅 DRAFT 由调用方保证**），返回 `affected`。
pub async fn soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} SET deleted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ?::uuid AND status = '{STATUS_DRAFT}' AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== ★ 过账：库存加法 + 流水 ====================

/// 锁读 `(物料, 仓库, 库位)` 的库存行（`FOR UPDATE`，PRD §3.2.6）。
///
/// `location_id` 为 `None` 时按 `COALESCE(location_id, 零 uuid) = 零 uuid` 命中「未分配库位汇总桶」，
/// 与 `uk_biz_inv_mat_wh_loc` 的表达式索引完全一致（否则走不上索引、也锁不到同一行）。
async fn lock_inventory(
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
                Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default()),
            ],
        )
        .await?;
    row.as_ref()
        .map(|row| {
            Ok(InventorySnapshot {
                qty: row.get_typed("qty")?,
                amount: row.get_typed("amount")?,
                avg_cost: row.get_typed("avg_cost")?,
            })
        })
        .transpose()
}

/// **一条明细的过账成本落地**（PRD §3.2.3）：加数量 + 加金额 + 重算移动加权单价。
///
/// 两步：
/// 1. `SELECT … FOR UPDATE` 锁住库存行（行不存在则无锁可拿）；
/// 2. `INSERT … ON CONFLICT (material, warehouse, COALESCE(location)) DO UPDATE` 做**加法**，
///    `RETURNING` 回读真实落库值。
///
/// 为什么第 2 步用 upsert 而不是「读到就 UPDATE、读不到就 INSERT」：两个收货单并发收同一物料、
/// 且库存行**还不存在**时，两边都读到「无行」→ 两边 INSERT → 后到的撞唯一键（42P10/23505）。
/// upsert 让后到的那笔走 `DO UPDATE` 分支，加法照做，数字不丢。
///
/// `avg_cost` 的算子在 SQL 里（不是在 Rust 里）：`ROUND` 是 PG 的 numeric 语义，
/// 与库内既有 `avg_cost` 的构造方式同源（W3c 踩过「Rust 与 PG 舍入差 1 分钱」）。
pub async fn apply_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    cost: CostBasis,
    now: DateTime<Utc>,
) -> DbResult<AppliedInventory> {
    let locked = lock_inventory(ex, material_id, warehouse_id, location_id).await?;
    let qty_text = cost.qty.round_dp(4).to_string();
    let amount_text = cost.amount.round_dp(4).to_string();
    let unit_text = cost.unit_cost.to_string();
    let location_text = Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default());

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
                location_text,
                Value::Text(qty_text.clone()),
                Value::Text(unit_text.clone()),
                Value::Text(amount_text.clone()),
                Value::Text(qty_text.clone()),
                Value::Text(amount_text.clone()),
                Value::from(now),
                Value::from(now),
                Value::from(now),
            ],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue(
                "库存 upsert 未返回行（biz_inventory 的 RETURNING 不应为空）".into(),
            )
        })?;
    let after = InventorySnapshot {
        qty: row.get_typed("qty")?,
        amount: row.get_typed("amount")?,
        avg_cost: row.get_typed("avg_cost")?,
    };
    let before = match locked {
        Some(snapshot) => snapshot,
        // 无行 → 并发插入分支：从「变动后」精确反推（加法可逆）
        None => InventorySnapshot::derived_before(after, cost.qty, cost.amount),
    };
    Ok(AppliedInventory { before, after })
}

// ==================== ★ 过账：库位账（数量账）加法 ====================

/// 库位账一次加法前后的数量（`biz_inventory_location`）。
#[derive(Debug, Clone, Copy)]
pub struct AppliedLocation {
    /// 加之前（行不存在 = 0）。
    pub before_qty: Decimal,
    /// 加之后（`RETURNING` 回读的真实落库值）。
    pub after_qty: Decimal,
}

/// **一条明细的库位账落地**（P6）：把 `qty` 加到 `biz_inventory_location` 的对应行。
///
/// 过账原来只写成本账（`biz_inventory`）+ 流水，**库位账一行不写** → 「即时库存」与
/// 「库位库存」两个 Tab 永远对不上（违反不变式③）。本函数补上那一半，且由
/// [`crate::business::erp::stockin_handler`] 的过账事务在**同一事务**里调用（失败整单回滚）。
///
/// # 两种分支（`location_id` 在本仓是 `biz_inventory_location.id`，不是 `biz_location.id`）
///
/// V0139 文件头 ① 与 `serial_model` 模块头把这条口径钉死了：ERP 里凡是 `location_id`
/// 都指**库位库存行 id**（`biz_inventory.location_id` / `biz_stock_in_item.location_id` /
/// `biz_material_serial.location_id` 三处外键都指向 `biz_inventory_location(id)`）。
/// 而 `biz_inventory_location.location_id` 这一列存的才是**库位字典 id**（`biz_location.id`，弱引用）。
/// 两条分支就是从这个「两个 id 不能混用」里长出来的：
///
/// * `Some(row_id)`：**按主键 UPDATE 这一行**，并校验它的 `material_id` / `warehouse_id`
///   与本次明细一致（库位下拉里可能选到别的物料／别的仓的库位行）；
/// * `None`：往 `(material, warehouse, location_id IS NULL)` 这个**未分配库位桶**加
///   （`uk_biz_inv_loc … NULLS NOT DISTINCT` 保证最多一行，NULL 也占一行、不会重复）。
///
/// 为什么有行 id 时不走「三列 upsert」：那会把库位行 id 当成库位字典 id 塞进
/// `biz_inventory_location.location_id`，**新建**一行（qty 从 0 开始），库位账合计立刻大于成本账。
///
/// # 锁顺序（PRD §3.2.6，防死锁）
///
/// 先 `SELECT … FOR UPDATE` 拿 before 快照，再加法。全局只有一种加锁顺序：
/// **明细按 `material_id` 升序 → 成本账先锁 → 库位账后锁**。所以两个收货单并发收同一物料时
/// 只会在成本账行上排队，库位账那一把锁永远在对方放手之后才拿，不会交叉等待。
///
/// # `locked_qty`
///
/// 收货不入锁定量：新建行显式写 `locked_qty = 0`，已有行**不动**（`ck_biz_inv_loc_locked` 要求 ≥ 0，
/// 且锁定量属于出库/占用流程）。
pub async fn apply_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<AppliedLocation> {
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
                DbError::InvalidValue(
                    "库位账 UPDATE 未返回行（biz_inventory_location 的 RETURNING 不应为空）".into(),
                )
            })?;
        let after_qty: Decimal = updated.get_typed("qty")?;
        return Ok(AppliedLocation {
            before_qty,
            after_qty,
        });
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
    // 不会因为「NULL 不相等」再插一行）。**不要**改成 `COALESCE(…)` 表达式形式——
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
            DbError::InvalidValue(
                "库位账 upsert 未返回行（biz_inventory_location 的 RETURNING 不应为空）".into(),
            )
        })?;
    let after_qty: Decimal = row.get_typed("qty")?;
    Ok(AppliedLocation {
        before_qty,
        after_qty,
    })
}

/// 写一条 `IN` 流水（**只增不改**，`qty` 恒正、方向看 `txn_type`）。
#[allow(clippy::too_many_arguments)]
pub async fn write_txn(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    line_no: i32,
    cost: CostBasis,
    applied: &AppliedInventory,
    doc_id: Uuid,
    doc_no: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let period_code = serial_model::period_code_of(now);
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    ex.exec_sql(
        "INSERT INTO biz_inventory_txn \
             (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, \
              qty, unit_cost, amount, before_qty, before_amount, before_cost, \
              after_qty, after_amount, after_cost, period_code, operator_id, created_at) \
         VALUES (?, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,'')::uuid, ?)"
            .to_string(),
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::from(TXN_TYPE_IN),
            Value::from(BIZ_TYPE_PURCHASE_IN),
            Value::from(DOC_TYPE_STOCK_IN),
            Value::from(&doc_id),
            Value::from(doc_no),
            Value::from(line_no),
            Value::Text(cost.qty.round_dp(4).to_string()),
            Value::Text(cost.unit_cost.to_string()),
            Value::Text(cost.amount.round_dp(4).to_string()),
            Value::Text(applied.before.qty.to_string()),
            Value::Text(applied.before.amount.to_string()),
            Value::Text(applied.before.avg_cost.to_string()),
            Value::Text(applied.after.qty.to_string()),
            Value::Text(applied.after.amount.to_string()),
            Value::Text(applied.after.avg_cost.to_string()),
            Value::from(period_code.as_str()),
            operator_text,
            Value::from(now),
        ],
    )
    .await?;
    Ok(())
}

// ==================== ★ 作废：库存反向冲减（P9） ====================

/// 流水方向：出（作废冲销 / `ck_biz_inv_txn_type` 只认 `IN` / `OUT`）。
pub const TXN_TYPE_OUT: &str = "OUT";

/// 流水业务类型：采购入库**作废冲销**（P9 新增，与 `PURCHASE_IN` 配对）。
pub const BIZ_TYPE_PURCHASE_IN_CANCEL: &str = "PURCHASE_IN_CANCEL";

/// 库存不足的哨兵前缀（`DbError` 只有字符串，handler 据此回 `409` 而不是 `500`）。
///
/// 形态：`INV_SHORTFALL|<明细文案>`，例如
/// `INV_SHORTFALL|物料 P2B1-M1 库存 3.0000 < 本单 9.0000`。
pub const SHORTFALL_PREFIX: &str = "INV_SHORTFALL|";

/// 过账流水缺失的哨兵前缀（历史数据缺口：单据 POSTED 但 `biz_inventory_txn` 里没有对应行）。
///
/// 遇到它必须**拒绝作废**（没法知道该退多少金额），并提示走数据修复脚本 —— 绝不按现价估算。
pub const MISSING_TXN_PREFIX: &str = "INV_NO_TXN|";

/// 序列号已流出仓库的哨兵前缀（P10：本单生成的一物一码号码里已有出库 / 报废的件）。
///
/// 形态：`SERIAL_SHIPPED|<明细文案>`，例如
/// `SERIAL_SHIPPED|收货单 IN-… 生成的一物一码序列号 SN-… 已出库（OUTBOUND）…`。
///
/// 为什么不与 [`SHORTFALL_PREFIX`] 合并：**判据不同**。一物一码物料的「货已出库」最硬的证据是
/// 序列号状态，而不是库存数量 —— 同一物料后来又进过别的货时，数量上「够退」，
/// 但退回来的不是本单那几件（按数量放开就会把别批的货冲掉）。
pub const SERIAL_SHIPPED_PREFIX: &str = "SERIAL_SHIPPED|";

/// 作废时**一行**要退回的数量与金额。
///
/// 数量来自 `biz_stock_in_item`（**不**信单据头的 `total_qty`），金额来自
/// `biz_inventory_txn` 里这行过账时**实际写入**的 `amount`（**不**用当前 `avg_cost × qty`：
/// 期间又进过货，均价已变，用现价冲减会污染成本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelLine {
    /// 明细行号（流水 `line_no`）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 收货仓库（单据头的 `warehouse_id`，冲销流水要用）。
    pub warehouse_id: Uuid,
    /// 库位账行 id（`biz_inventory_location.id`，可空 = 未分配库位桶）。
    pub location_id: Option<Uuid>,
    /// 本单这一行的入库数量（`biz_stock_in_item.qty`）。
    pub qty: Decimal,
    /// 本行过账时写入的不含税金额（`biz_inventory_txn.amount`）。
    pub amount: Decimal,
    /// 本行过账时的入库单价（`amount / qty`，6 位；写冲销流水用）。
    pub unit_cost: Decimal,
}

/// 作废单的基本信息（冲销流水要写的 `doc_id` / `doc_no`）。
#[derive(Debug, Clone, Copy)]
pub struct CancelDoc<'a> {
    /// 收货单 id（`biz_inventory_txn.doc_id`）。
    pub id: Uuid,
    /// 收货单号（`biz_inventory_txn.doc_no`）。
    pub in_no: &'a str,
}

/// 组装作废计划：逐行读「明细数量 + 过账流水的金额」。
///
/// * 排序：`material_id, location, line_no` —— **锁顺序**与过账一致（PRD §3.2.6），
///   两张单并发作废时按同一全序拿锁，不会交叉等待；
/// * 流水缺失 / 数量对不上 → 直接报错（**绝不静默按现价重算**）：历史缺口只能由
///   数据修复脚本补流水，代码层面必须让调用方看见问题。
pub async fn cancel_plan_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    in_no: &str,
    warehouse_id: Uuid,
) -> DbResult<Vec<CancelLine>> {
    let rows = ex
        .query_sql(
            "SELECT i.line_no, i.material_id, i.location_id, i.qty, \
                    coalesce(t.qty, 0) AS txn_qty, coalesce(t.amount, 0) AS txn_amount, \
                    coalesce(t.cnt, 0) AS txn_cnt \
               FROM biz_stock_in_item i \
               LEFT JOIN (SELECT line_no, sum(qty) AS qty, sum(amount) AS amount, count(*) AS cnt \
                            FROM biz_inventory_txn \
                           WHERE doc_type = ? AND doc_id = ?::uuid \
                             AND txn_type = ? AND biz_type = ? \
                           GROUP BY line_no) t ON t.line_no = i.line_no \
              WHERE i.stock_in_id = ?::uuid \
              ORDER BY i.material_id, i.location_id NULLS FIRST, i.line_no"
                .to_string(),
            vec![
                Value::from(DOC_TYPE_STOCK_IN),
                Value::from(&stock_in_id),
                Value::from(TXN_TYPE_IN),
                Value::from(BIZ_TYPE_PURCHASE_IN),
                Value::from(&stock_in_id),
            ],
        )
        .await?;

    let mut plan = Vec::with_capacity(rows.len());
    for row in rows {
        let line_no: i32 = row.get_typed("line_no")?;
        let material_id: Uuid = row.get_typed("material_id")?;
        let location_id: Option<Uuid> = row.get_typed("location_id")?;
        let qty: Decimal = row.get_typed("qty")?;
        let txn_qty: Decimal = row.get_typed("txn_qty")?;
        let txn_amount: Decimal = row.get_typed("txn_amount")?;
        let txn_cnt: i64 = row.get_typed("txn_cnt")?;
        if txn_cnt == 0 {
            return Err(DbError::InvalidValue(format!(
                "{MISSING_TXN_PREFIX}收货单 {in_no} 第 {line_no} 行在 biz_inventory_txn 里找不到\
                 过账流水（P9 之前的历史数据缺口）：无法确定要退回多少金额，拒绝按现价估算。\
                 请先用数据修复脚本补流水，或走人工红字调整单"
            )));
        }
        if (txn_qty - qty).abs() > Decimal::new(1, 4) {
            return Err(DbError::InvalidValue(format!(
                "收货单 {in_no} 第 {line_no} 行：明细数量 {qty} 与过账流水数量 {txn_qty} 不一致，\
                 单据已被改过（POSTED 不应可改），拒绝作废"
            )));
        }
        let unit_cost = if txn_qty.is_zero() {
            Decimal::ZERO
        } else {
            (txn_amount / txn_qty).round_dp_with_strategy(6, ROUNDING)
        };
        plan.push(CancelLine {
            line_no,
            material_id,
            warehouse_id,
            location_id,
            qty,
            amount: txn_amount,
            unit_cost,
        });
    }
    Ok(plan)
}

/// 一条明细的**成本账反向冲减**：`qty -= 本单数量`、`amount -= 过账时的金额`。
///
/// # 三条硬口径（卡面 §一 的 ①②③）
///
/// 1. `SELECT … FOR UPDATE` 锁库存行（调用方已按 `material_id` 升序排好，锁顺序全局一致）；
/// 2. **可退性校验**：`qty - 本单数量 >= 0`。已出库导致退不回来 → `DbError::InvalidValue`
///    带 [`SHORTFALL_PREFIX`]，handler 转 `409`（不是 500，也不是静默少退）；
/// 3. `qty` 归零 → `amount` 同时归零（避免留下「0 件还有钱」的幽灵金额）。
///
/// # 与卡面的一处差异：`avg_cost` 按剩余金额重算（不是原样不动）
///
/// 卡面写「移动加权：减量不改单价」。但作废退的是**这一层入库的原额**（`amount` 取自流水），
/// 而当前均价已被期间内其它入库稀释 ⇒ 「金额减 a、数量减 q、单价不动」三者必然互相矛盾
/// （`amount ≠ qty × avg_cost`，直接触发不变式 C4）。
/// 按 `(amount - a) / (qty - q)` 重算单价在数学上恰好等于「把这一层从加权平均里摘掉」：
/// 冲销**最近一层**入库时它会精确回到冲销前的历史均价（可被单测证明），冲销**中间层**时
/// 得到剩余各层的加权平均 —— 两者都是移动加权平均法的正解。
pub async fn reverse_cost(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    line: &CancelLine,
    now: DateTime<Utc>,
) -> DbResult<AppliedInventory> {
    let locked = lock_inventory(ex, material_id, warehouse_id, location_id).await?;
    let Some(before) = locked else {
        return Err(DbError::InvalidValue(format!(
            "{}物料 {material_id} 在仓库 {warehouse_id} 没有库存行，无法冲减（过账流水存在但库存被删了）",
            SHORTFALL_PREFIX
        )));
    };
    let after_qty = before.qty - line.qty;
    if after_qty < Decimal::ZERO {
        let code: Option<String> = ex
            .scalar_sql(
                "SELECT material_code FROM biz_material WHERE id = ?::uuid".to_string(),
                vec![Value::from(&material_id)],
            )
            .await?;
        return Err(DbError::InvalidValue(format!(
            "{SHORTFALL_PREFIX}物料 {} 库存 {} < 本单 {}",
            code.unwrap_or_else(|| material_id.to_string()),
            before.qty.round_dp(4),
            line.qty.round_dp(4)
        )));
    }

    let qty_text = line.qty.round_dp(4).to_string();
    let amount_text = line.amount.round_dp(4).to_string();
    let location_text = Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default());
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
                // 所以 qty/amount 出现几次就要传几次（与 apply_cost 同款写法）。
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
                location_text,                    // $11
            ],
        )
        .await?
        .ok_or_else(|| {
            DbError::InvalidValue(
                "库存反向冲减未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into(),
            )
        })?;
    let after = InventorySnapshot {
        qty: row.get_typed("qty")?,
        amount: row.get_typed("amount")?,
        avg_cost: row.get_typed("avg_cost")?,
    };
    Ok(AppliedInventory { before, after })
}

/// 一条明细的**库位账反向冲减**（与 [`apply_location_qty`] 的两个分支一一对应）。
///
/// P6 让过账同时加库位账，这里必须能退；退不回来（库位账 < 本单数量）同样报
/// [`SHORTFALL_PREFIX`]，让 handler 回 `409` 而不是把库位账冲成负数。
pub async fn reverse_location_qty(
    ex: &impl SqlExecutor,
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<AppliedLocation> {
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
                DbError::InvalidValue(
                    "库位账反向冲减未返回行（UPDATE 命中 0 行：锁读之后行被删了）".into(),
                )
            })?;
        let after_qty: Decimal = updated.get_typed("qty")?;
        return Ok(AppliedLocation {
            before_qty,
            after_qty,
        });
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
        .ok_or_else(|| {
            DbError::InvalidValue("库位账反向冲减（未分配桶）未返回行（UPDATE 命中 0 行）".into())
        })?;
    let after_qty: Decimal = updated.get_typed("qty")?;
    Ok(AppliedLocation {
        before_qty,
        after_qty,
    })
}

/// 写一条**反向流水**：`txn_type='OUT'` + `biz_type='PURCHASE_IN_CANCEL'`。
///
/// `before_*` / `after_*` 六个字段全部真填（审计要能从流水复原每一次冲减）。
#[allow(clippy::too_many_arguments)]
pub async fn write_cancel_txn(
    ex: &impl SqlExecutor,
    doc: &CancelDoc<'_>,
    line: &CancelLine,
    applied: &AppliedInventory,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<()> {
    let period_code = serial_model::period_code_of(now);
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    ex.exec_sql(
        "INSERT INTO biz_inventory_txn \
             (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no, \
              qty, unit_cost, amount, before_qty, before_amount, before_cost, \
              after_qty, after_amount, after_cost, period_code, operator_id, created_at) \
         VALUES (?, ?::uuid, ?::uuid, ?, ?, ?, ?::uuid, ?, ?, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,'')::uuid, ?)"
            .to_string(),
        vec![
            Value::from(Uuid::now_v7()),
            Value::from(&line.material_id),
            Value::from(&line.warehouse_id),
            Value::from(TXN_TYPE_OUT),
            Value::from(BIZ_TYPE_PURCHASE_IN_CANCEL),
            Value::from(DOC_TYPE_STOCK_IN),
            Value::from(&doc.id),
            Value::from(doc.in_no),
            Value::from(line.line_no),
            Value::Text(line.qty.round_dp(4).to_string()),
            Value::Text(line.unit_cost.to_string()),
            Value::Text(line.amount.round_dp(4).to_string()),
            Value::Text(applied.before.qty.to_string()),
            Value::Text(applied.before.amount.to_string()),
            Value::Text(applied.before.avg_cost.to_string()),
            Value::Text(applied.after.qty.to_string()),
            Value::Text(applied.after.amount.to_string()),
            Value::Text(applied.after.avg_cost.to_string()),
            Value::from(period_code.as_str()),
            operator_text,
            Value::from(now),
        ],
    )
    .await?;
    Ok(())
}

// ==================== 一物一码：序列号 ====================

/// 落一行序列号（客户给号 / 自动取号共用）。
async fn insert_serials(
    ex: &impl SqlExecutor,
    pairs: &[(Uuid, String)],
    material_id: Uuid,
    warehouse_id: Uuid,
    location_id: Option<Uuid>,
    stock_in_id: Uuid,
    operator: Option<Uuid>,
) -> DbResult<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let payload = serial_model::pair_payload(pairs);
    let operator_text = Value::Text(operator.map(|v| v.to_string()).unwrap_or_default());
    ex.exec_sql(
        "INSERT INTO biz_material_serial \
             (id, serial_no, material_id, warehouse_id, location_id, batch_no, status, inbound_at, \
              source_type, source_id, create_by, update_by) \
         SELECT split_part(t.line, '|', 1)::uuid, split_part(t.line, '|', 2), ?::uuid, ?::uuid, \
                NULLIF(?::text,'')::uuid, NULL, 'IN_STOCK', now(), \
                NULLIF(?::text,''), NULLIF(?::text,'')::uuid, \
                NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid \
           FROM unnest(string_to_array(?, chr(10))) AS t(line)"
            .to_string(),
        vec![
            Value::from(&material_id),
            Value::from(&warehouse_id),
            Value::Text(location_id.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(SERIAL_SOURCE_PURCHASE),
            Value::Text(stock_in_id.to_string()),
            operator_text.clone(),
            operator_text,
            Value::Text(payload),
        ],
    )
    .await?;
    Ok(())
}

/// 一批序列号的落库入参（收进结构体：既避开 `clippy::too_many_arguments`，
/// 也让「谁的序列号、落在哪个仓/库位、来自哪张单」在调用点一眼看清）。
#[derive(Debug, Clone)]
pub struct SerialBatch<'a> {
    /// 来源收货单（`biz_material_serial.source_id`）。
    pub stock_in_id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 收货仓库（序列号的 `warehouse_id`）。
    pub warehouse_id: Uuid,
    /// 库位（可空）。
    pub location_id: Option<Uuid>,
    /// 本次入库件数（必须与 `serial_ids` 等长，或按其取号）。
    pub qty: Decimal,
    /// 客户端给的号码（空 = 自动取号）。
    pub serial_ids: &'a [String],
    /// 操作人。
    pub operator: Option<Uuid>,
    /// 记账时刻（Rust 侧算好，保证号码周期键与流水时间同源）。
    pub now: DateTime<Utc>,
}

/// 生成（或落客户端给的）序列号，返回本次落的号码（按顺序）。
///
/// * `serial_ids` 非空：**必须**正好 `qty` 条（数量不符在 DTO 层就 400），号码原样落库
///   （供应商已经贴好的码，不允许改写）；
/// * 否则按 `SN-yyyyMMdd-NNNNNN` 取号生成 `qty` 条。
///
/// 见模块文档「与卡面 §三 e) 的实现差异」：这里**不调用** `serial_model::batch_generate`
/// （它会重算库存数量并再写一条 `SERIAL_IN` 流水，与本模块的移动加权加法冲突）。
pub async fn generate_serials(
    ex: &impl SqlExecutor,
    batch: &SerialBatch<'_>,
) -> DbResult<Vec<String>> {
    let SerialBatch {
        stock_in_id,
        material_id,
        warehouse_id,
        location_id,
        qty,
        serial_ids,
        operator,
        now,
    } = *batch;
    let count: i64 = qty.try_into().map_err(|_| {
        DbError::InvalidValue(format!("一物一码物料的数量必须是整数（当前 {qty}）"))
    })?;
    if count <= 0 {
        return Err(DbError::InvalidValue(
            "一物一码物料的入库数量必须 > 0".into(),
        ));
    }
    let pairs: Vec<(Uuid, String)> = if serial_ids.is_empty() {
        let period_key = serial_model::period_key_of(now);
        let start = serial_model::take_numbers(ex, count, &period_key).await?;
        (1..=count)
            .map(|offset| {
                (
                    Uuid::now_v7(),
                    serial_model::format_serial_no(&period_key, start + offset),
                )
            })
            .collect()
    } else {
        serial_ids
            .iter()
            .map(|serial_no| (Uuid::now_v7(), serial_no.trim().to_string()))
            .collect()
    };
    insert_serials(
        ex,
        &pairs,
        material_id,
        warehouse_id,
        location_id,
        stock_in_id,
        operator,
    )
    .await?;
    Ok(pairs.into_iter().map(|(_, serial_no)| serial_no).collect())
}

/// 收货单已生成的序列号数量（详情展示 / 单测断言）。
pub async fn serial_count_of(stock_in_id: Uuid) -> DbResult<i64> {
    // SCOPE-EXEMPT: 序列号台账是仓储数据，无归属列。
    let count: i64 = db()
        .scalar_with_params(
            "SELECT count(*) FROM biz_material_serial \
              WHERE source_id = ?::uuid AND source_type = 'PURCHASE' AND deleted_at IS NULL",
            vec![Value::from(&stock_in_id)],
        )
        .await?;
    Ok(count)
}

// ==================== 作废：退序列号（P10） ====================

/// 本单生成的一条序列号（作废前的校验视图：号码 + 状态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialBrief {
    /// 系统内码（`SN-yyyyMMdd-NNNNNN`）。
    pub serial_no: String,
    /// 当前状态（`IN_STOCK` / `OUTBOUND` / `SCRAPPED`）。
    pub status: String,
}

/// 序列号状态的中文文案（`409` 报文用）。
fn serial_status_label(status: &str) -> &'static str {
    match status {
        serial_model::STATUS_OUTBOUND => "出库",
        serial_model::STATUS_SCRAPPED => "报废",
        _ => "变更",
    }
}

/// 锁读本单生成的一物一码序列号（`source_type='PURCHASE' AND source_id = 本单`）。
///
/// `FOR UPDATE` 是必须的：作废与「单品出库 / 报废」改的是同一行的 `status`，
/// 行锁让两边在这行上串行 —— 否则「刚校验完就有人出库」会让后面的软删漏掉已出库的件，
/// 留下一条「单已作废、号还在库」的幽灵序列号（正是 W2c 口径要防的账实不符）。
pub async fn lock_serials_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
) -> DbResult<Vec<SerialBrief>> {
    // SCOPE-EXEMPT: 序列号台账是仓储数据，无归属列（口径同 serial_model）。
    let rows = ex
        .query_sql(
            "SELECT serial_no, status FROM biz_material_serial \
              WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY serial_no FOR UPDATE"
                .to_string(),
            vec![
                Value::from(SERIAL_SOURCE_PURCHASE),
                Value::from(&stock_in_id),
            ],
        )
        .await?;
    let mut serials = Vec::with_capacity(rows.len());
    for row in rows {
        serials.push(SerialBrief {
            serial_no: row.get_typed("serial_no")?,
            status: row.get_typed("status")?,
        });
    }
    Ok(serials)
}

/// **作废时「退序列号」**（P10）：本单生成的号码必须全部仍在库，否则拒绝；全部在库则**软删**。
/// 返回被软删的行数（0 行 = 该单不是一物一码，正常）。
///
/// # 口径（卡面 §一 三选一 → 取 ②+③ 的组合，理由写在这里免得下一张卡重猜）
///
/// 1. **不加 `VOID` 第四态**：`ck_biz_material_serial_status` 只认
///    `IN_STOCK / OUTBOUND / SCRAPPED`（V0138 定稿）。加 `VOID` 要动迁移 + `ALL_STATUSES` +
///    合法边表 + 前端状态映射 + 序列号分页筛选，只为一个「从未出库的单据作废」付这份代价不值；
/// 2. **未出库 → 软删该单生成的号**（`deleted_at` 置位、行保留）而不是物理删：
///    `uk_biz_material_serial_no` 是 `WHERE deleted_at IS NULL` 的部分唯一索引，
///    软删后同号可被重新贴用（W2a 口径），而「谁的号、哪天作废」的审计痕迹不丢；
///    物理删会同时丢掉痕迹与外键（`biz_stock_transfer_item_serial.serial_id` 是硬外键）；
/// 3. **已出库 / 已报废 → 拒绝作废**（[`SERIAL_SHIPPED_PREFIX`]，handler 转 `409`）：
///    与「货已部分出库」同款语义 —— 件已经离开仓库，只能先做退货入库冲销。
///
/// 幂等：重复作废在单据状态 CAS 处就被 `409` 拦住，本函数不会第二次执行；
/// 即便执行，`deleted_at IS NULL` 的条件也让第二遍命中 0 行。
pub async fn void_serials_of(
    ex: &impl SqlExecutor,
    stock_in_id: Uuid,
    in_no: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let serials = lock_serials_of(ex, stock_in_id).await?;
    if serials.is_empty() {
        return Ok(0);
    }
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
    // SCOPE-EXEMPT: 序列号台账是仓储数据，无归属列（口径同 serial_model）。
    let affected = ex
        .exec_sql(
            "UPDATE biz_material_serial \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE source_type = ? AND source_id = ?::uuid AND status = ? \
                AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(now),
                Value::from(now),
                Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
                Value::from(SERIAL_SOURCE_PURCHASE),
                Value::from(&stock_in_id),
                Value::from(serial_model::STATUS_IN_STOCK),
            ],
        )
        .await?;
    Ok(affected)
}

// ==================== 采购单回写（已收数量 + 单据状态） ====================

/// 回写采购明细的已收数量（条件更新：`received_qty + 本次 ≤ qty`），返回 `affected`。
///
/// 0 行 = 超收（或明细已被删）→ 调用方按 400 处理（卡面 ⑪）。
/// 条件写在 `WHERE` 里而不是先读再判：并发两张收货单收同一采购行时，
/// 后到的那笔在拿到行锁后会**重判**最新值（READ COMMITTED），不会双花。
pub async fn add_received_qty(
    ex: &impl SqlExecutor,
    source_item_id: Uuid,
    qty: Decimal,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_purchase_order_item \
            SET received_qty = received_qty + NULLIF(?::text,'')::numeric, updated_at = ? \
          WHERE id = ?::uuid \
            AND received_qty + NULLIF(?::text,'')::numeric <= qty"
            .to_string(),
        vec![
            Value::Text(qty.round_dp(4).to_string()),
            Value::from(now),
            Value::from(&source_item_id),
            Value::Text(qty.round_dp(4).to_string()),
        ],
    )
    .await
}

/// 按明细已收情况推进采购单状态（`PARTIAL_RECEIVED` / `RECEIVED`）。
///
/// # P10 改造（铁律 ③：状态机不许绕过）
///
/// 改造前这里是一句 `UPDATE … SET status = CASE WHEN NOT EXISTS(未收完) THEN 'RECEIVED'
/// ELSE 'PARTIAL_RECEIVED' END` —— **完全绕过 `can_transition()`**：采购订单的
/// 状态机等于名存实亡（卡面 §一 ①）。
///
/// 现在拆成三步，全部在收货过账的同一个事务里：
///
/// 1. `SELECT … FOR UPDATE` 锁采购单，读「当前状态 + 单号」；
/// 2. 算目标状态 `target`（还有未收明细 → `PARTIAL_RECEIVED`，否则 `RECEIVED`）；
/// 3. `target != 当前态` 才调 [`state_machine::transition`]（写 `biz_status_log`，
///    `biz_ref = ("STOCK_IN", 收货单号)`），再 CAS 改状态。
///
/// `PARTIAL_RECEIVED → PARTIAL_RECEIVED`（继续部分收货）**不调 transition**：
/// 原地不动不是状态变更，否则每次收货都多一条无意义日志（卡面 §三 的 ⚠️）。
///
/// 只对「已提交 / 已审批 / 部分收货」推进；`DRAFT` / `CLOSED` / `CANCELLED` /
/// `RECEIVED` 一律不动（返回 0）。
pub async fn sync_purchase_status(
    ex: &impl SqlExecutor,
    order_id: Uuid,
    stock_in_no: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    // ① 锁单 + 读当前状态（`FOR UPDATE` 让并发两张收货单串行推进同一采购单）
    let header = ex
        .query_sql(
            "SELECT order_no, status FROM biz_purchase_order \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(&order_id)],
        )
        .await?;
    let Some(row) = header.first() else {
        return Ok(0);
    };
    let order_no: String = row.get_typed("order_no")?;
    let current: String = row.get_typed("status")?;
    if !matches!(
        current.as_str(),
        "SUBMITTED" | "APPROVED" | "PARTIAL_RECEIVED"
    ) {
        return Ok(0);
    }

    // ② 收货进度 → 目标状态
    let pending: i64 = ex
        .scalar_sql(
            "SELECT count(*) FROM biz_purchase_order_item \
              WHERE order_id = ?::uuid AND received_qty < qty"
                .to_string(),
            vec![Value::from(&order_id)],
        )
        .await?;
    let target = if pending == 0 {
        "RECEIVED"
    } else {
        "PARTIAL_RECEIVED"
    };

    // ③ 状态没变（继续部分收货）→ 只留收货流水，不写状态日志
    if target == current {
        return Ok(0);
    }

    // ④ 唯一入口：边表校验 + reason 校验 + 写 biz_status_log（关联收货单号）
    let reason = format!("收货过账 {stock_in_no} 自动推进");
    state_machine::transition(
        ex,
        state_machine::ENTITY_PURCHASE_ORDER,
        order_id,
        &order_no,
        &crate::business::erp::purchase_model::LEGAL_EDGES,
        &current,
        target,
        operator,
        Some(&reason),
        Some((state_machine::ENTITY_STOCK_IN, stock_in_no)),
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;

    // ⑤ CAS 改状态（`WHERE status = 当前态` 防止把别人改过的单覆盖掉）
    ex.exec_sql(
        "UPDATE biz_purchase_order SET status = ?, updated_at = ?, \
            update_by = NULLIF(?::text,'')::uuid \
          WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(target.to_string()),
            Value::from(now),
            Value::Text(operator.map(|v| v.to_string()).unwrap_or_default()),
            Value::from(&order_id),
            Value::Text(current),
        ],
    )
    .await
}

/// 采购单是否「可收货」（状态白名单）。
pub fn purchase_receivable(status: &str) -> bool {
    matches!(
        status,
        "SUBMITTED" | "APPROVED" | "PARTIAL_RECEIVED" | "RECEIVED"
    )
}

/// 采购单的一条明细的「未收数量」文本（`qty - received_qty`，负数按 0）。
pub fn remaining_qty(item: &PurchaseOrderItemRow) -> Decimal {
    let remaining = item.qty - item.received_qty;
    if remaining < Decimal::ZERO {
        Decimal::ZERO
    } else {
        remaining
    }
}

/// 采购单引用（收货要回写 `received_qty`，先确认单子存在且可收）。
pub async fn purchase_order_ref(order_id: Uuid) -> DbResult<Option<PurchaseOrderRow>> {
    crate::business::erp::model::purchase_order_find_by_id(order_id).await
}

/// 物料引用（判 `serial_required` 与带出 `unit_id`）。
pub async fn material_serial_required(material_id: Uuid) -> DbResult<Option<bool>> {
    serial_model::material_serial_required(material_id).await
}

// ==================== 单测（纯逻辑，不连库） ====================
//
// 真库用例在 `stockin_handler::tests`（需要 `ERP_DB_URL`）；这里放**不依赖数据库**的口径与状态机，
// 保证「没配 ERP_DB_URL 的 CI」也能覆盖成本口径与 3 条合法边。
/// 回写凭证 / 应付单引用（P2-C1：入库即生成）。
///
/// `voucher_id` / `payable_id` 是**弱引用**（V0145 建表时就有这两列、可空、无外键），
/// 由过账事务在同一 `ex` 里回写（卡面 §三 ⑤）。
pub async fn link_voucher_and_payable(
    ex: &impl SqlExecutor,
    id: Uuid,
    voucher_id: Uuid,
    payable_id: Uuid,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} \
                SET voucher_id = ?::uuid, payable_id = ?::uuid, updated_at = ? \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&voucher_id),
            Value::from(&payable_id),
            Value::from(now),
            Value::from(&id),
        ],
    )
    .await
}

#[cfg(test)]
mod pure_tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("测试金额字面量非法")
    }

    /// ① 成本口径：`CostBasis::of` 用**不含税金额**除数量，单价 6 位四舍五入。
    #[test]
    fn cost_basis_is_net_amount_per_unit() {
        let cost = CostBasis::of(dec("3"), dec("9.99"));
        assert_eq!(cost.qty, dec("3"));
        assert_eq!(cost.amount, dec("9.99"), "金额原样保留（不进存货的是税额）");
        assert_eq!(cost.unit_cost, dec("3.330000"));
        // 13%：不含税 200 / 含税 226，成本只能是 200
        let taxed = CostBasis::of(dec("100"), dec("200"));
        assert_eq!(taxed.unit_cost, dec("2.000000"));
        assert_ne!(taxed.amount, dec("226"));
    }

    /// ② 尾差：单价除不尽时 6 位四舍五入（PRD §3.2.5 算例 2：21.09 / 13 = 1.622308）。
    #[test]
    fn unit_cost_rounds_half_up_to_six_places() {
        let cost = CostBasis::of(dec("13"), dec("21.09"));
        assert_eq!(cost.unit_cost, dec("1.622308"));
    }

    /// ③ 无行建档分支的反推：`before = after - 增量` 精确可逆，单价回算与库里同源。
    #[test]
    fn derived_before_is_exact_inverse_of_addition() {
        let after = InventorySnapshot {
            qty: dec("200"),
            amount: dec("500"),
            avg_cost: dec("2.500000"),
        };
        let before = InventorySnapshot::derived_before(after, dec("100"), dec("300"));
        assert_eq!(before.qty, dec("100"));
        assert_eq!(before.amount, dec("200"));
        assert_eq!(before.avg_cost, dec("2.000000"));
        // 首次入库：before 全 0
        let first = InventorySnapshot::derived_before(
            InventorySnapshot {
                qty: dec("5"),
                amount: dec("10"),
                avg_cost: dec("2.000000"),
            },
            dec("5"),
            dec("10"),
        );
        assert_eq!(first, InventorySnapshot::zero());
    }

    /// ④ 状态机：只有 3 条合法边，`POSTED → POSTED` 与 `CANCELLED` 之后的一切都是 409。
    #[test]
    fn only_three_legal_edges() {
        assert!(can_transition(STATUS_DRAFT, STATUS_POSTED));
        assert!(can_transition(STATUS_DRAFT, STATUS_CANCELLED));
        assert!(can_transition(STATUS_POSTED, STATUS_CANCELLED));
        assert!(
            !can_transition(STATUS_POSTED, STATUS_POSTED),
            "重复过账非法"
        );
        assert!(!can_transition(STATUS_CANCELLED, STATUS_POSTED));
        assert!(!can_transition(STATUS_CANCELLED, STATUS_DRAFT));
        assert_eq!(
            next_statuses(STATUS_DRAFT),
            vec![STATUS_POSTED, STATUS_CANCELLED]
        );
        assert!(next_statuses(STATUS_CANCELLED).is_empty());
        let message = transition_rejected_message(STATUS_DRAFT, "CANCELLED");
        assert!(message.contains("POSTED"), "文案要点名可选去向：{message}");
    }

    /// ⑤ 单号格式：`IN-yyyyMMdd-000001`（6 位只补零不截断）。
    #[test]
    fn in_no_format() {
        assert_eq!(format_in_no("20261031", 1), "IN-20261031-000001");
        assert_eq!(format_in_no("20261031", 1234567), "IN-20261031-1234567");
    }

    /// ⑥ 作废哨兵：三个前缀必须互不包含（handler 靠 `strip_prefix` 分派 409 文案；
    /// 一旦谁是谁的前缀，报文就会静默走错分支，把「序列号已出库」说成「库存不足」）。
    #[test]
    fn cancel_sentinels_are_distinct() {
        assert_eq!(SHORTFALL_PREFIX, "INV_SHORTFALL|");
        assert_eq!(MISSING_TXN_PREFIX, "INV_NO_TXN|");
        assert_eq!(SERIAL_SHIPPED_PREFIX, "SERIAL_SHIPPED|");
        let all = [SHORTFALL_PREFIX, MISSING_TXN_PREFIX, SERIAL_SHIPPED_PREFIX];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                if i != j {
                    assert!(
                        !a.starts_with(b),
                        "{a} 不能以 {b} 开头（前缀重叠会让文案串台）"
                    );
                    assert!(
                        !b.starts_with(a),
                        "{b} 不能以 {a} 开头（前缀重叠会让文案串台）"
                    );
                }
            }
        }
        assert_eq!(TXN_TYPE_OUT, "OUT");
        assert_ne!(BIZ_TYPE_PURCHASE_IN_CANCEL, BIZ_TYPE_PURCHASE_IN);
        assert_eq!(BIZ_TYPE_PURCHASE_IN_CANCEL, "PURCHASE_IN_CANCEL");
    }

    /// ⑦ P10：序列号状态文案（`409` 报文里「已出库 / 已报废」两个分支都要有落点）。
    #[test]
    fn serial_status_label_covers_all_states() {
        assert_eq!(serial_status_label(serial_model::STATUS_OUTBOUND), "出库");
        assert_eq!(serial_status_label(serial_model::STATUS_SCRAPPED), "报废");
        assert_eq!(serial_status_label(serial_model::STATUS_IN_STOCK), "变更");
    }
}

//! 发货单数据层（销售链 · SO-08）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0283__sales_shipment.up.sql`
//! （`biz_shipment` / `biz_shipment_item`）。
//!
//! | 列 | 口径 |
//! |----|------|
//! | `ship_no` | `SH-yyyyMMdd-000001`；**部分唯一索引** `uk_biz_shipment_no (ship_no) WHERE deleted_at IS NULL` |
//! | `status` | `VARCHAR(24)` 域值 4 个（ARCH-7 §4.2）：`DRAFT`/`PENDING_POST`/`POSTED`/`CANCELLED` |
//! | `total_qty` | 明细合计，`DEFAULT 0`，`CHECK total_qty >= 0`（`ck_shipment_total_qty`） |
//! | `qty`（明细） | `CHECK > 0`（`ck_biz_shipment_item_qty`） |
//! | `stock_out_id` | 过账后回填的销售出库单（SO-10），可空、无外键（避免过账事务成环） |
//!
//! # 本卡的范围（SO-08 = 表 + 迁移 + 取号 + 超发守卫）
//!
//! 本文件**不写** CRUD / 分页 / 状态迁移落库 —— 那是 SO-09（`shipment_handler.rs`）的地盘。
//! 交付的是 SO-09 必须复用的三块底座：
//!
//! 1. [`STATUSES`] / [`is_valid_status`] / [`status_label`]：状态域与中文标签；
//! 2. [`next_ship_no`]：`SH-yyyyMMdd-6 位` 取号（复用 `biz_seq` 的原子 `UPDATE … RETURNING`）；
//! 3. [`ensure_qty_within_remaining`]：**超发守卫**（卡面「qty > 0，且不超过订单行未发货量」）。
//!
//! ## 为什么不登记 `LEGAL_EDGES`（与 SO-04 的分工一致）
//!
//! 单据状态机的唯一真相在 `business/shared/state_machine.rs`；新增实体要先在
//! `shared/doc_kind.rs` 的 `DocKind` 加枚举值（被闸④ `biz_type_domain.py` 与前端 TS 常量表
//! 逐字比对）。SO-04（销售订单建表卡）同样没在本卡登记 `DocKind::SaleOrder`，
//! 而是 SO-05 落 API 时登记。本卡沿用这条分工：**SO-09 登记 `DocKind::Shipment` +
//! `SHIPMENT_EDGES`（4 条边：DRAFT→PENDING_POST→POSTED、DRAFT/PENDING_POST→CANCELLED）**，
//! 届时边表域值必须与 [`STATUSES`] 逐字一致（迁移 V0283 的 `ck_shipment_status` 已钉死）。
//!
//! # 超发口径（跨表，库层 CHECK 表达不了）
//!
//! ```text
//! 未发货量 = 订单行.qty − 订单行.delivered_qty − 其他「未过账」发货单占用
//!            └─ 已过账累计（SO-10 回写）        └─ status IN (DRAFT, PENDING_POST) 的本单之外的行
//! ```
//!
//! * `POSTED` 的发货量**已经**累加进 `delivered_qty`（SO-10 过账时写），所以不能再按
//!   「占用」算一遍 —— 否则一张单过账后会把同一行的可发量扣两次（本文件单测
//!   `posted_shipment_is_not_counted_twice` 钉住这条）。
//! * `CANCELLED` 的单不占用（作废即释放），所以只要把状态改成 CANCELLED，可发量自动回来，
//!   不需要改数量。
//! * 判定必须在**同一事务**内、并且先 [`lock_so_item`]（`SELECT … FOR UPDATE`）再算：
//!   两个并发建单只做「读 → 判 → 写」会双双通过（各看到剩余 10，各发 10 ⇒ 超发 20-10）。
//!   SO-09 的建单 / 改单入口必须按 [`lock_so_item`] → [`ensure_qty_within_remaining`] →
//!   写明细 的顺序调用。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_shipment / biz_shipment_item 是 ERP 单据表（V0283 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由
//! business:erp:shipment:* 权限码控制（SO-09 落）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::{find_by_id, page_of};
use crate::business::erp::serial_model::period_key_of;
use crate::business::shared::{scope, state_machine};

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）直接复用底座。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 表名 / 常量 ====================

/// 单据头表。
pub const SHIP_TABLE: &str = "biz_shipment";

/// 明细表（无 `deleted_at`：明细随单头走，外键 `ON DELETE CASCADE`）。
pub const ITEM_TABLE: &str = "biz_shipment_item";

/// 单号前缀（卡面：`SH-YYYYMMDD-NNNNNN`）。落库一律 4 位年 + 2 位月 + 2 位日，与 SO/PO 同款。
pub const NO_PREFIX: &str = "SH";

/// 流水号补零宽度（超过 6 位不截断，宁可长一位也不撞号）。
pub const NO_SEQ_WIDTH: usize = 6;

/// `biz_seq.seq_type`（V0283 种下的 `SH` 行）。
pub const SEQ_TYPE: &str = "SH";

/// 状态：草稿（**默认值**，唯一允许改明细的状态）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：待过账（已确认要发，等 SO-10 过账生成销售出库单）。
pub const STATUS_PENDING_POST: &str = "PENDING_POST";
/// 状态：已过账（销售出库单已生成、库存已扣，由 SO-10 写入）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已取消（终态；作废即释放占用，库存不动）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 状态域白名单（与 `ck_shipment_status` **逐字同序**，单测钉住）。
pub const STATUSES: [&str; 4] = [STATUS_DRAFT, STATUS_PENDING_POST, STATUS_POSTED, STATUS_CANCELLED];

/// 「未过账但已占用未发量」的状态：这两个状态的明细要从可发量里扣掉。
///
/// `POSTED` **不在**这里 —— 它已经通过 `delivered_qty` 体现（见模块文档「超发口径」）。
pub const RESERVING_STATUSES: [&str; 2] = [STATUS_DRAFT, STATUS_PENDING_POST];

/// 状态是否在域内。
pub fn is_valid_status(value: &str) -> bool {
    STATUSES.contains(&value)
}

/// 状态中文标签（错误文案与前端徽标共用；未知值返回「未知」，不 panic）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_PENDING_POST => "待过账",
        STATUS_POSTED => "已过账",
        STATUS_CANCELLED => "已取消",
        _ => "未知",
    }
}

// ==================== 单号（纯逻辑，无数据库） ====================

/// 组装单号（`SH-{周期键}-{6 位流水}`）。
pub fn format_ship_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 拆单号：`SH-yyyyMMdd-NNNNNN` → `(周期键, 流水)`。
///
/// 严格校验（不是 `split('-')` 一把梭）：
/// - 前缀必须是 [`NO_PREFIX`]、分隔符必须是 `-`；
/// - 周期键 8 位数字且**年月日都合法**（`20261306` 要拒）；
/// - 流水是 ≥ 1 的全数字（7 位以上也接受，不截断）。
///
/// 任何一条不符 → `None`。
pub fn parse_ship_no(value: &str) -> Option<(String, i64)> {
    let rest = value.strip_prefix(NO_PREFIX)?.strip_prefix('-')?;
    let (period, seq) = rest.split_once('-')?;
    if period.len() != 8 || !period.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i32 = period.get(0..4)?.parse().ok()?;
    let month: u32 = period.get(4..6)?.parse().ok()?;
    let day: u32 = period.get(6..8)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        return None;
    }
    if seq.is_empty() || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let no: i64 = seq.parse().ok()?;
    if no < 1 {
        return None;
    }
    Some((period.to_string(), no))
}

/// 单号是否合法（[`parse_ship_no`] 的布尔薄壳）。
pub fn is_valid_ship_no(value: &str) -> bool {
    parse_ship_no(value).is_some()
}

// ==================== 超发判定（纯逻辑） ====================

/// 订单行的可发量快照（[`availability_of`] 的返回值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoItemAvailability {
    /// 订单行数量（`biz_sales_order_item.qty`）。
    pub ordered: Decimal,
    /// 已过账发货量（`biz_sales_order_item.delivered_qty`）。
    pub delivered: Decimal,
    /// 其他未过账发货单（DRAFT / PENDING_POST）在本行上的占用合计。
    pub reserved: Decimal,
    /// 可发量 = `ordered - delivered - reserved`（不小于 0）。
    pub remaining: Decimal,
}

impl SoItemAvailability {
    /// 由三个分量算出快照（`remaining` 夹在 0 以上，负剩余不往下游传）。
    pub fn of(ordered: Decimal, delivered: Decimal, reserved: Decimal) -> Self {
        Self {
            ordered,
            delivered,
            reserved,
            remaining: remaining_qty(ordered, delivered, reserved),
        }
    }
}

/// 未发货量 = `ordered - delivered - reserved`，**下限 0**。
///
/// 夹 0 是因为三者在并发窗口里可能瞬时「超扣」（另一事务刚过账），
/// 负数剩余会让 UI 显示 `-0.5` 这种怪值；真正的判定仍由
/// [`qty_within_remaining`]（`qty <= remaining`）负责，负剩余必然拒。
pub fn remaining_qty(ordered: Decimal, delivered: Decimal, reserved: Decimal) -> Decimal {
    let left = ordered - delivered - reserved;
    if left < Decimal::ZERO { Decimal::ZERO } else { left }
}

/// 本单行数量是否允许：`qty > 0` **且** `qty <= 未发货量`。
///
/// 两个条件对应卡面「qty > 0，且不超过订单行未发货量」；库层只兜住了前一条
/// （`ck_biz_shipment_item_qty`），后一条是跨表的，只能在这里判。
pub fn qty_within_remaining(qty: Decimal, ordered: Decimal, delivered: Decimal, reserved: Decimal) -> bool {
    qty > Decimal::ZERO && qty <= remaining_qty(ordered, delivered, reserved)
}

// ==================== 取号（`SH-yyyyMMdd-000001`） ====================

/// `biz_seq(seq_type='SH')` 缺失时幂等补一行（正常库有 V0283 的种子）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'SH', '发货单号', 'SH', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '发货单单号：SH-yyyyMMdd-000001，按天（CST）滚动' \
         ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING"
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

/// 推进 `biz_seq(seq_type='SH')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
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
              WHERE seq_type = 'SH' AND deleted_at IS NULL \
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
    rows.first().map(|row| row.get_typed("current_no")).transpose()
}

/// 取一个发货单号（周期键按北京时间）。
///
/// 调用方（SO-09 的建单）必须在**同一个事务**里调用本函数 + 写单头 + 写明细：
/// 取号把 `current_no` 推进了却没落单，那个号就永久占用（跳号可接受，重号不可接受）。
pub async fn next_ship_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_ship_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='SH') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 超发守卫（真库） ====================

/// 锁住一行销售订单明细（`SELECT … FOR UPDATE`），把同一行的并发发货串行化。
///
/// 必须在**事务**里调用；行不存在 → `InvalidValue`（对用户是 400，不是 500）。
pub async fn lock_so_item(ex: &impl SqlExecutor, so_item_id: Uuid) -> DbResult<()> {
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_sales_order_item WHERE id = ?::uuid FOR UPDATE".to_string(),
            vec![Value::from(so_item_id)],
        )
        .await?;
    if rows.is_empty() {
        return Err(DbError::InvalidValue(format!("销售订单行不存在：{so_item_id}")));
    }
    Ok(())
}

/// 读订单行的可发量快照；`exclude_shipment` = 改单时排除本单（否则自己的数量会算成占用）。
///
/// 调用前请先 [`lock_so_item`]（本函数只读，不加锁）。
pub async fn availability_of(
    ex: &impl SqlExecutor,
    so_item_id: Uuid,
    exclude_shipment: Option<Uuid>,
) -> DbResult<SoItemAvailability> {
    // 排除本单用 `?::uuid` + 哨兵 nil：发货单 id 永不为 nil，等价于「不排除」，
    // 这样 SQL 只有一条（两条 SQL 迟早漂移）。
    let exclude = exclude_shipment.unwrap_or_else(Uuid::nil);
    // SCOPE-EXEMPT: 订单行 / 发货单都是 ERP 单据表，无归属列（见模块文档）。
    let rows = ex
        .query_sql(
            "SELECT i.qty::text AS ordered, i.delivered_qty::text AS delivered, \
                    COALESCE((SELECT SUM(si.qty) \
                                FROM biz_shipment_item si \
                                JOIN biz_shipment s ON s.id = si.shipment_id \
                               WHERE si.so_item_id = i.id \
                                 AND s.deleted_at IS NULL \
                                 AND s.id <> ?::uuid \
                                 AND s.status IN (?, ?)), 0)::text AS reserved \
               FROM biz_sales_order_item i \
              WHERE i.id = ?::uuid"
                .to_string(),
            vec![
                Value::from(exclude),
                Value::from(STATUS_DRAFT),
                Value::from(STATUS_PENDING_POST),
                Value::from(so_item_id),
            ],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Err(DbError::InvalidValue(format!("销售订单行不存在：{so_item_id}")));
    };
    let text = |column: &str| -> Decimal {
        row.get_typed::<String>(column)
            .ok()
            .and_then(|value| value.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    };
    Ok(SoItemAvailability::of(
        text("ordered"),
        text("delivered"),
        text("reserved"),
    ))
}

/// 超发守卫：`qty > 0` 且 `qty <= 未发货量`，否则 `InvalidValue`（handler 侧是 400）。
///
/// 返回判定用的快照（成功时 handler 可据此回显 `remaining`）。
/// **调用方必须已在事务内先 [`lock_so_item`]**，否则并发窗口下仍可能超发。
pub async fn ensure_qty_within_remaining(
    ex: &impl SqlExecutor,
    so_item_id: Uuid,
    qty: Decimal,
    exclude_shipment: Option<Uuid>,
) -> DbResult<SoItemAvailability> {
    if qty <= Decimal::ZERO {
        return Err(DbError::InvalidValue(format!("发货数量必须大于 0（实际 {qty}）")));
    }
    let snapshot = availability_of(ex, so_item_id, exclude_shipment).await?;
    if qty > snapshot.remaining {
        return Err(DbError::InvalidValue(format!(
            "超发：订单行 {so_item_id} 未发货量仅 {}（订单 {} − 已发 {} − 未过账占用 {}），\
             本次要发 {qty}",
            snapshot.remaining, snapshot.ordered, snapshot.delivered, snapshot.reserved
        )));
    }
    Ok(snapshot)
}

/// 可发货明细（SO-09「从销售订单带出明细，只带未发完的行」的唯一取数口径）。
///
/// 只返回 `remaining > 0` 的行；`reserved` 只算**其他**未过账发货单的占用
/// （订单本身还没有发货单，所以这里一般等于 0，除了「同一订单分多张单」的情形）。
pub async fn shippable_items(ex: &impl SqlExecutor, so_id: Uuid) -> DbResult<Vec<ShippableItemRow>> {
    // SCOPE-EXEMPT: 订单行 / 发货单都是 ERP 单据表，无归属列。
    let rows = ex
        .query_sql(
            "SELECT i.id AS so_item_id, i.line_no, i.material_id, \
                    i.qty::text AS ordered, i.delivered_qty::text AS delivered, \
                    COALESCE((SELECT SUM(si.qty) \
                                FROM biz_shipment_item si \
                                JOIN biz_shipment s ON s.id = si.shipment_id \
                               WHERE si.so_item_id = i.id \
                                 AND s.deleted_at IS NULL \
                                 AND s.status IN (?, ?)), 0)::text AS reserved \
               FROM biz_sales_order_item i \
              WHERE i.so_id = ?::uuid \
              ORDER BY i.line_no"
                .to_string(),
            vec![
                Value::from(STATUS_DRAFT),
                Value::from(STATUS_PENDING_POST),
                Value::from(so_id),
            ],
        )
        .await?;
    let mut items = Vec::new();
    for row in rows {
        let text = |column: &str| -> Decimal {
            row.get_typed::<String>(column)
                .ok()
                .and_then(|value| value.parse::<Decimal>().ok())
                .unwrap_or(Decimal::ZERO)
        };
        let snapshot = SoItemAvailability::of(text("ordered"), text("delivered"), text("reserved"));
        if snapshot.remaining <= Decimal::ZERO {
            continue;
        }
        let so_item_id: Uuid = row.get_typed("so_item_id")?;
        let line_no: i32 = row.get_typed("line_no")?;
        let material_id: Uuid = row.get_typed("material_id")?;
        items.push(ShippableItemRow {
            so_item_id,
            line_no,
            material_id,
            availability: snapshot,
        });
    }
    Ok(items)
}

/// 可发货明细的一行（订单行 + 可发量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShippableItemRow {
    /// 订单行 id。
    pub so_item_id: Uuid,
    /// 订单行号（1 起）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 可发量快照。
    pub availability: SoItemAvailability,
}

// ==================== SO-09：读写 + 状态迁移落库 ====================
//
// 分工与 `sales_order_model` 一致：**本文件只做「行结构 + 读写 + 状态迁移落库」**，
// 取号 / 单号解析 / 状态域 / 超发守卫都在本文件上半部分（SO-08 已交付，不另造一份）。
//
// 库存三本账**一行都不写**：过账的库存效果是 SO-10 的事，走 `inventory_writer`（唯一写入口）。
// 本文件只用 `stockout_model::fifo_plan` 做**只读**的「够不够量」判定（与出库过账同一口径）。

/// 单头列清单（与 V0283 逐列对应）。
pub const SHIP_COLUMNS: &str = "id, ship_no, so_id, customer_id, warehouse_id, ship_date, \
     status, total_qty, stock_out_id, remark, created_at, updated_at, \
     create_by, update_by, deleted_at";

/// 明细查询：`serial_nos` 是 `TEXT[]`，用 `array_to_string` 转成逗号串再在 Rust 侧拆
/// （驱动没有 `text[]` 解码器，直读会在运行期炸 —— 与 `serial_model` 的数组坑同源）。
const ITEM_SELECT: &str = "SELECT si.id, si.shipment_id, si.so_item_id, si.material_id, \
     si.qty, si.location_id, \
     COALESCE(array_to_string(si.serial_nos, ','), '') AS serial_nos, si.remark, \
     COALESCE(oi.line_no, 0) AS line_no \
     FROM biz_shipment_item si \
     LEFT JOIN biz_sales_order_item oi ON oi.id = si.so_item_id";

/// 合法边表：**唯一真相**在 `state_machine::SHIPMENT_EDGES`（本文件只引用，不复制）。
pub const LEGAL_EDGES: [(&str, &str); 4] = state_machine::SHIPMENT_EDGES;

/// 状态机判定：只有 [`LEGAL_EDGES`] 里的边放行（其余一律 409，见 handler）。
pub fn can_transition(from: &str, to: &str) -> bool {
    LEGAL_EDGES.iter().any(|(f, t)| *f == from && *t == to)
}

/// 从 `from` 出发的合法目标态（非法迁移的 409 文案里要列出来）。
pub fn allowed_targets(from: &str) -> Vec<&'static str> {
    LEGAL_EDGES
        .iter()
        .filter(|(f, _)| *f == from)
        .map(|(_, t)| *t)
        .collect()
}

/// 分页锚点（ERP 单据表，不参与数据范围）。
pub const SHIP_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(SHIP_TABLE);

/// 列表排序：发货日期倒序、同一天按单号倒序（最新单在前）。
pub const SHIP_ORDER: &str = "ORDER BY ship_date DESC, ship_no DESC";

/// 可发货的订单状态：只有审批通过 / 部分出库的订单能发货。
///
/// `DRAFT` / `SUBMITTED` 的订单还没被批准（批的是一张随时会改的单），
/// `OUTBOUND` / `CLOSED` / `CANCELLED` 已经没有未发量（前者发完了，后两者是终态）。
/// 与超发守卫是两道独立的闸：状态闸挡「给还没批的单发货」，数量闸挡「发超」。
pub const SHIPPABLE_SO_STATUSES: [&str; 2] = ["APPROVED", "PARTIAL_OUT"];

/// `biz_shipment` 一行。
#[derive(Debug, Clone)]
pub struct ShipmentRow {
    /// 主键。
    pub id: Uuid,
    /// 单号（`SH-yyyyMMdd-000001`，存活行唯一）。
    pub ship_no: String,
    /// 来源销售订单（硬外键 RESTRICT）。
    pub so_id: Uuid,
    /// 客户（弱引用 `biz_customer.id`）。
    pub customer_id: Uuid,
    /// 发货仓库（弱引用 `biz_warehouse.id`）。
    pub warehouse_id: Uuid,
    /// 发货日期。
    pub ship_date: NaiveDate,
    /// 状态（4 值之一）。
    pub status: String,
    /// 明细数量合计。
    pub total_qty: Decimal,
    /// 过账后生成的销售出库单（SO-10 回填；未过账为 `None`）。
    pub stock_out_id: Option<Uuid>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 更新人。
    pub update_by: Option<Uuid>,
    /// 软删时间（`None` = 存活）。
    pub deleted_at: Option<DateTime<Utc>>,
}

impl ShipmentRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            ship_no: row.get_typed("ship_no")?,
            so_id: row.get_typed("so_id")?,
            customer_id: row.get_typed("customer_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            ship_date: row.get_typed("ship_date")?,
            status: row.get_typed("status")?,
            total_qty: row.get_typed("total_qty")?,
            stock_out_id: row.get_typed("stock_out_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }

    /// 是否终态（`POSTED` / `CANCELLED`，见 `state_machine::SHIPMENT_LOCKED`）。
    pub fn is_locked(&self) -> bool {
        state_machine::Entity::Shipment.is_locked(&self.status)
    }
}

/// `biz_shipment_item` 一行（`line_no` 是订单行号，从 `biz_sales_order_item` 带出）。
#[derive(Debug, Clone)]
pub struct ShipmentItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属发货单。
    pub shipment_id: Uuid,
    /// 对应的销售订单行。
    pub so_item_id: Uuid,
    /// 物料（弱引用 `biz_material.id`）。
    pub material_id: Uuid,
    /// 本次发货数量（> 0）。
    pub qty: Decimal,
    /// 发货库位（`None` = 整仓 / 未指定，过账按 FIFO 选）。
    pub location_id: Option<Uuid>,
    /// 序列号（只有序列号物料才非空）。
    pub serial_nos: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
    /// 订单行号（1 起；订单行已被物理删时回落 0）。
    pub line_no: i32,
}

impl ShipmentItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let serials: String = row.get_typed("serial_nos")?;
        Ok(Self {
            id: row.get_typed("id")?,
            shipment_id: row.get_typed("shipment_id")?,
            so_item_id: row.get_typed("so_item_id")?,
            material_id: row.get_typed("material_id")?,
            qty: row.get_typed("qty")?,
            location_id: row.get_typed("location_id")?,
            serial_nos: split_serials(&serials),
            remark: row.get_typed("remark")?,
            line_no: row.get_typed("line_no")?,
        })
    }
}

/// 逗号串 → 序列号列表（空串 / 全空白 → 空 vec；逐项 trim 后丢空项）。
pub fn split_serials(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// 一行待落库的明细（`material_id` 由 handler 从订单行带出，不信请求体）。
#[derive(Debug, Clone)]
pub struct NewShipmentItem {
    /// 对应的销售订单行。
    pub so_item_id: Uuid,
    /// 物料（取自 `biz_sales_order_item.material_id`）。
    pub material_id: Uuid,
    /// 发货数量（> 0，且 ≤ 未发量）。
    pub qty: Decimal,
    /// 发货库位（`None` = 由过账按 FIFO 选）。
    pub location_id: Option<Uuid>,
    /// 序列号。
    pub serial_nos: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
}

/// 待落库的单头。
#[derive(Debug, Clone)]
pub struct NewShipment<'a> {
    /// 来源销售订单。
    pub so_id: Uuid,
    /// 客户（取自订单，不信请求体）。
    pub customer_id: Uuid,
    /// 发货仓库。
    pub warehouse_id: Uuid,
    /// 发货日期。
    pub ship_date: NaiveDate,
    /// 备注。
    pub remark: Option<&'a str>,
}

/// 明细数量合计（纯逻辑，建单与改单共用；**先逐行 trim 再求和**不改变精度）。
pub fn total_qty_of(items: &[NewShipmentItem]) -> Decimal {
    items.iter().fold(Decimal::ZERO, |acc, item| acc + item.qty).round_dp(4)
}

/// 库存缺口（过账前置检查的结果行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockShortfall {
    /// 订单行（报错文案里点名）。
    pub so_item_id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 本行要发多少。
    pub needed: Decimal,
    /// 该仓该物料可用合计。
    pub available: Decimal,
}

impl StockShortfall {
    /// 缺口数量（`needed - available`，恒 > 0）。
    pub fn shortfall(&self) -> Decimal {
        self.needed - self.available
    }
}

/// 按主键取发货单（软删行不返回）。
pub async fn find_shipment(id: Uuid) -> DbResult<Option<ShipmentRow>> {
    find_by_id(SHIP_TABLE, SHIP_COLUMNS, id, true, ShipmentRow::from_row).await
}

/// 取一张发货单的明细（订单行号升序；订单行没了排最后）。
pub async fn items_of(shipment_id: Uuid) -> DbResult<Vec<ShipmentItemRow>> {
    let sql = format!("{ITEM_SELECT} WHERE si.shipment_id = ? ORDER BY COALESCE(oi.line_no, 0), si.id");
    let rows = db().find_with_params(&sql, vec![Value::from(shipment_id)]).await?;
    rows.iter().map(ShipmentItemRow::from_row).collect()
}

/// 分页：`ship_no`（模糊）/ `status`（精确）/ `so_id`（精确）/ `customer_id`（精确）。
pub async fn shipment_page(
    ship_no: Option<&str>,
    status: Option<&str>,
    so_id: Option<Uuid>,
    customer_id: Option<Uuid>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<ShipmentRow>, i64)> {
    let mut sql = format!("SELECT {SHIP_COLUMNS} FROM {SHIP_TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = crate::utils::like_pattern(ship_no) {
        sql.push_str(" AND ship_no LIKE ?");
        params.push(Value::from(pattern));
    }
    if let Some(status) = status.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(so_id) = so_id {
        sql.push_str(" AND so_id = ?");
        params.push(Value::from(so_id));
    }
    if let Some(customer_id) = customer_id {
        sql.push_str(" AND customer_id = ?");
        params.push(Value::from(customer_id));
    }
    page_of(
        SHIP_TARGET,
        sql,
        params,
        SHIP_ORDER,
        page,
        page_size,
        ShipmentRow::from_row,
    )
    .await
}

/// 订单行的 `(material_id, line_no)`：建单时**物料从订单行取**，不信请求体
/// （否则可以给 A 物料的订单行开一张 B 物料的发货单，库存扣到别的物料上）。
pub async fn so_item_brief(ex: &impl SqlExecutor, so_item_id: Uuid) -> DbResult<Option<(Uuid, i32)>> {
    // SCOPE-EXEMPT: 订单行明细表，ERP 单据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT material_id, line_no FROM biz_sales_order_item WHERE id = ?::uuid".to_string(),
            vec![Value::from(so_item_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((row.get_typed("material_id")?, row.get_typed("line_no")?)))
}

/// 订单状态（建单 / 提交时的状态闸用；订单不存在或已删 → `None`）。
pub async fn so_status_of(ex: &impl SqlExecutor, so_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 销售订单，ERP 单据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT status FROM biz_sales_order WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(so_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed::<String>("status")).transpose()
}

/// 订单单号（详情 / 列表回显用；订单不存在或已软删 → `None`，不报错）。
///
/// 口径与 [`so_no_labels`]（列表批量补齐）**逐字一致**（同样带 `deleted_at IS NULL`），
/// 否则会出现「同一张发货单，列表说没订单号、详情说有」的自相矛盾
/// （本卡 t_45900ea7 就是在收这类注释与实现不一致的账）。
pub async fn so_no_of(ex: &impl SqlExecutor, so_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 销售订单，ERP 单据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT so_no FROM biz_sales_order WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(so_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed::<String>("so_no")).transpose()
}

/// 批量取「订单 id → 订单号」，分页列表一次往返补齐（避免 N+1）。
///
/// 与 [`warehouse_labels`] 同形同口径：去重后一条 `IN (…)`，软删订单不出现在结果里
/// （调用方按缺失处理 = `so_no` 留 `None`）；订单行被物理删除时自然也查不到。
/// 与 [`so_no_of`]（详情）共用 `deleted_at IS NULL` 判据 ⇒ 列表与详情永远同一个值。
pub async fn so_no_labels(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<Vec<(Uuid, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let placeholders = vec!["?::uuid"; unique.len()].join(", ");
    let sql = format!(
        "SELECT id, so_no FROM biz_sales_order \
          WHERE id IN ({placeholders}) AND deleted_at IS NULL"
    );
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = ex.query_sql(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((row.get_typed("id")?, row.get_typed("so_no")?));
    }
    Ok(out)
}

/// 批量取「仓库 id → （编码, 名称）」，分页列表一次往返补齐（避免 N+1）。
///
/// 软删仓库不出现在结果里（调用方按缺失处理 = 两列留 `None`），
/// 与 `sales_order_model::customer_labels` 同口径：历史单据照常显示，只是不再显示已删仓库的名字。
pub async fn warehouse_labels(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<Vec<(Uuid, String, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let placeholders = vec!["?::uuid"; unique.len()].join(", ");
    let sql = format!(
        "SELECT id, warehouse_code AS code, warehouse_name AS name FROM biz_warehouse \
          WHERE id IN ({placeholders}) AND deleted_at IS NULL"
    );
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = ex.query_sql(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((row.get_typed("id")?, row.get_typed("code")?, row.get_typed("name")?));
    }
    Ok(out)
}

/// 订单行是否属于该订单（防止把别的订单的行挂到本单上）。
pub async fn so_item_belongs(ex: &impl SqlExecutor, so_item_id: Uuid, so_id: Uuid) -> DbResult<bool> {
    // SCOPE-EXEMPT: 销售订单行，ERP 单据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT 1 AS hit FROM biz_sales_order_item WHERE id = ?::uuid AND so_id = ?::uuid".to_string(),
            vec![Value::from(so_item_id), Value::from(so_id)],
        )
        .await?;
    Ok(!rows.is_empty())
}

/// 关联销售出库单号（SO-10 过账回填的 `stock_out_id` → `out_no`；出库单不存在或已软删 → `None`）。
///
/// 只读、单表：发货单详情要显示「关联出库单号」，这里给一个不回链 model 之外的查询入口。
/// 口径与 [`stock_out_no_labels`]（列表批量补齐）逐字一致（同样带 `deleted_at IS NULL`）。
pub async fn stock_out_no_of(ex: &impl SqlExecutor, stock_out_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 出库单，ERP 单据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT out_no FROM biz_stock_out WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(stock_out_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed::<String>("out_no")).transpose()
}

/// 批量取「出库单 id → 出库单号」，分页列表一次往返补齐（避免 N+1）。
///
/// 与 [`so_no_labels`] / [`warehouse_labels`] 同形同口径：去重后一条 `IN (…)`，
/// 软删出库单不出现在结果里（调用方按缺失处理 = `stock_out_no` 留 `None`）。
/// 未过账的发货单没有 `stock_out_id`，调用方先过滤，本函数对空入参直接短路（不多发一条 SQL）。
pub async fn stock_out_no_labels(ex: &impl SqlExecutor, ids: &[Uuid]) -> DbResult<Vec<(Uuid, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let placeholders = vec!["?::uuid"; unique.len()].join(", ");
    let sql = format!(
        "SELECT id, out_no FROM biz_stock_out \
          WHERE id IN ({placeholders}) AND deleted_at IS NULL"
    );
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = ex.query_sql(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((row.get_typed("id")?, row.get_typed("out_no")?));
    }
    Ok(out)
}

/// 仓库是否存在且未软删（弱引用，没外键兜）。返回 `(编码, 名称)`。
pub async fn warehouse_label(ex: &impl SqlExecutor, warehouse_id: Uuid) -> DbResult<Option<(String, String)>> {
    // SCOPE-EXEMPT: 仓库主数据，无归属列（可见性由权限码控制）。
    let rows = ex
        .query_sql(
            "SELECT warehouse_code, warehouse_name FROM biz_warehouse \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(warehouse_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((
        row.get_typed("warehouse_code")?,
        row.get_typed("warehouse_name")?,
    )))
}

/// **过账前的库存充足校验**（只读，不写三本账）。
///
/// 逐行调 `stockout_model::fifo_plan`（销售出库过账用的是同一个函数），
/// 所以「这里说够 / 过账时说不够」不会出现两套口径。
/// 返回空 vec = 全部够量；否则每行一条缺口（handler 拼文案回 409）。
pub async fn stock_shortfalls(
    ex: &impl SqlExecutor,
    warehouse_id: Uuid,
    items: &[ShipmentItemRow],
) -> DbResult<Vec<StockShortfall>> {
    let mut out = Vec::new();
    for item in items {
        let plan =
            crate::business::erp::stockout_model::fifo_plan(ex, item.material_id, warehouse_id, item.qty).await?;
        if plan.shortfall > Decimal::ZERO {
            out.push(StockShortfall {
                so_item_id: item.so_item_id,
                material_id: item.material_id,
                needed: item.qty,
                available: plan.available,
            });
        }
    }
    Ok(out)
}

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
///
/// 返回 `(新单 id, 单号)`。取号与单头同一事务：取号把 `biz_seq.current_no` 推了却没落单
/// 会永久跳号（见 [`next_ship_no`] 的注释）。
///
/// ⚠ **超发守卫不在本函数里**：调用方必须先按
/// [`lock_so_item`] → [`ensure_qty_within_remaining`] → [`create_shipment`] 的顺序执行，
/// 否则两个并发建单会各看到同一份剩余量（SO-08 模块文档的「并发安全」段）。
pub async fn create_shipment(
    ex: &impl SqlExecutor,
    new: &NewShipment<'_>,
    items: &[NewShipmentItem],
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    let ship_no = next_ship_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.id.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {SHIP_TABLE} \
                (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, \
                 total_qty, remark, created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, ?, ?, ?::text::date, ?, ?::text::numeric, NULLIF(?::text,''), \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(ship_no.clone()),
            Value::from(&new.so_id),
            Value::from(&new.customer_id),
            Value::from(&new.warehouse_id),
            Value::Text(new.ship_date.to_string()),
            Value::Text(STATUS_DRAFT.to_string()),
            Value::Text(total_qty_of(items).to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    insert_items(ex, id, items).await?;
    state_machine::log_created(
        ex,
        state_machine::ENTITY_SHIPMENT,
        id,
        &ship_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok((id, ship_no))
}

/// 插明细（调用方保证单头已落、`so_item_id` 属于该单的订单）。
///
/// `serial_nos` 走 `string_to_array` + 一个 text 参数：驱动没有 `text[]` 编码器，
/// 直绑 `?::text[]` 会 `error serializing parameter`（`serial_model` 实测坑）。
async fn insert_items(ex: &impl SqlExecutor, shipment_id: Uuid, items: &[NewShipmentItem]) -> DbResult<()> {
    for item in items {
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (shipment_id, so_item_id, material_id, qty, location_id, serial_nos, remark) \
                 VALUES (?, ?, ?, ?::text::numeric, NULLIF(?::text,'')::uuid, \
                    string_to_array(NULLIF(?::text,''), ','), NULLIF(?::text,''))"
            ),
            vec![
                Value::from(&shipment_id),
                Value::from(&item.so_item_id),
                Value::from(&item.material_id),
                Value::Text(item.qty.to_string()),
                Value::Text(item.location_id.map(|id| id.to_string()).unwrap_or_default()),
                Value::Text(item.serial_nos.join(",")),
                Value::Text(item.remark.clone().unwrap_or_default()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 状态迁移落库（CAS：`WHERE status = from`，返回 0 = 并发被别人推走）。
///
/// **调用方必须先过 `state_machine::transition`**（边表校验 + reason 校验 +
/// 写 `biz_status_log`），本函数只改单据头的 `status` 列。
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "UPDATE {SHIP_TABLE} \
                SET status = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ? AND status = ? AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(to.to_string()),
            Value::from(now),
            operator_text,
            Value::from(&id),
            Value::Text(from.to_string()),
        ],
    )
    .await
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 定点字面量（浮点会丢精度，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    /// 状态域与 `ck_shipment_status` 同域：4 个值，**不含**销售订单的状态（易混）。
    #[test]
    fn status_domain_matches_check_constraint() {
        assert_eq!(
            STATUSES,
            ["DRAFT", "PENDING_POST", "POSTED", "CANCELLED"],
            "状态域必须与 ck_shipment_status 逐字同序"
        );
        for good in STATUSES {
            assert!(is_valid_status(good), "`{good}` 应在域内");
            assert_ne!(status_label(good), "未知", "`{good}` 缺中文标签");
        }
        for bad in [
            "SHIPPED",  // 销售订单的终态，发货单没有
            "APPROVED", // 审批态不属于发货单
            "SUBMITTED",
            "draft",
            "Posted",
            "",
            "POSTED ",
        ] {
            assert!(!is_valid_status(bad), "`{bad}` 不应通过发货单状态域校验");
        }
        assert_eq!(status_label("WHATEVER"), "未知");
    }

    /// 占用口径：只有 DRAFT / PENDING_POST 占用，POSTED 靠 delivered_qty 体现。
    #[test]
    fn reserving_statuses_exclude_posted() {
        assert!(!RESERVING_STATUSES.contains(&STATUS_POSTED));
        assert!(RESERVING_STATUSES.contains(&STATUS_DRAFT));
        assert!(RESERVING_STATUSES.contains(&STATUS_PENDING_POST));
        assert!(!RESERVING_STATUSES.contains(&STATUS_CANCELLED));
    }

    /// 单号格式：`SH-yyyyMMdd-6 位`；超 6 位不截断；拆解可逆。
    #[test]
    fn format_and_parse_ship_no_roundtrip() {
        assert_eq!(format_ship_no("20261106", 7), "SH-20261106-000007");
        assert_eq!(format_ship_no("20261106", 123456), "SH-20261106-123456");
        assert_eq!(format_ship_no("20261106", 1234567), "SH-20261106-1234567");
        assert_eq!(parse_ship_no("SH-20261106-000007"), Some(("20261106".to_string(), 7)));
        for no in [1_i64, 2, 999, 1000, 999_999, 1_000_000] {
            let text = format_ship_no("20261106", no);
            assert_eq!(parse_ship_no(&text), Some(("20261106".to_string(), no)));
        }
        // 3 位流水也接受（历史手工号 / 老数据），但不合法日期一律拒
        assert!(is_valid_ship_no("SH-20261106-001"));
    }

    /// 单号严格校验：前缀 / 分隔符 / 年月日 / 流水，一条不符就拒。
    #[test]
    fn parse_ship_no_rejects_malformed_values() {
        for bad in [
            "SO-20261106-000001", // 销售订单前缀
            "sh-20261106-000001", // 小写
            "SH20261106-000001",  // 缺第一个分隔符
            "SH-2026116-000001",  // 周期键 7 位
            "SH-20261106_000001", // 分隔符不对
            "SH-20261106-",       // 流水空
            "SH-20261106-abc",    // 流水非数字
            "SH-20261106-000000", // 流水 0 不是合法单号
            "SH-20261306-000001", // 13 月
            "SH-20261132-000001", // 32 日
            "SH-19691106-000001", // 早于 1970
            "",
            "SH",
        ] {
            assert_eq!(parse_ship_no(bad), None, "`{bad}` 不应被当成合法单号");
            assert!(!is_valid_ship_no(bad));
        }
        assert!(is_valid_ship_no("SH-20240229-000001"), "闰年 2 月 29 日合法");
    }

    /// 未发货量 = 订单 − 已发 − 未过账占用；负数夹到 0。
    #[test]
    fn remaining_qty_subtracts_delivered_and_reserved() {
        // 干净行：可发 10
        assert_eq!(remaining_qty(dec("10"), Decimal::ZERO, Decimal::ZERO), dec("10"));
        // 已发 4、另有草稿单占用 3 ⇒ 可发 3
        assert_eq!(remaining_qty(dec("10"), dec("4"), dec("3")), dec("3"));
        // 发完了 ⇒ 0
        assert_eq!(remaining_qty(dec("10"), dec("10"), Decimal::ZERO), Decimal::ZERO);
        // 瞬时超扣（并发/脏数据）⇒ 夹到 0，不外溢负数
        assert_eq!(remaining_qty(dec("10"), dec("8"), dec("5")), Decimal::ZERO);
        // 4 位小数精度：0.0001 也算数
        assert_eq!(
            remaining_qty(dec("10.0000"), dec("9.9999"), Decimal::ZERO),
            dec("0.0001")
        );
    }

    /// 超发判定边界：qty > 0 且 ≤ 剩余；等号放行、多 0.0001 拒。
    #[test]
    fn qty_within_remaining_boundary() {
        // 剩余 10：10 放行、10.0001 拒、0 拒、负数拒
        assert!(qty_within_remaining(dec("10"), dec("10"), Decimal::ZERO, Decimal::ZERO));
        assert!(!qty_within_remaining(
            dec("10.0001"),
            dec("10"),
            Decimal::ZERO,
            Decimal::ZERO
        ));
        assert!(!qty_within_remaining(
            Decimal::ZERO,
            dec("10"),
            Decimal::ZERO,
            Decimal::ZERO
        ));
        assert!(!qty_within_remaining(
            dec("-1"),
            dec("10"),
            Decimal::ZERO,
            Decimal::ZERO
        ));
        // 已发 + 占用吃掉全部剩余 ⇒ 再多 0.0001 也拒
        assert!(!qty_within_remaining(dec("0.0001"), dec("10"), dec("7"), dec("3")));
        // 剩余 0 ⇒ 一切正数都拒（防止「库层 qty>0 放行、业务超发」）
        assert!(!qty_within_remaining(
            dec("0.0001"),
            dec("10"),
            dec("10"),
            Decimal::ZERO
        ));
        // 快照结构体：remaining 由三件套算出
        let snapshot = SoItemAvailability::of(dec("10"), dec("4"), dec("3"));
        assert_eq!(snapshot.remaining, dec("3"));
        assert_eq!(snapshot.ordered, dec("10"));
    }

    // ---------- 真库用例（`ERP_DB_URL` 开关） ----------
    //
    // ⚠ 不设 `ERP_DB_URL` 时这些用例打印 SKIP 并 return —— libtest 记 passed，属「假绿」。
    //   交付前必须显式带库跑一遍（卡内汇报记了实测命令）。

    /// 真库用例必须串行（共用全局 `db()` 与同一批 `SH8T-` 夹具）。
    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 夹具客户编码 —— 清场按它定位（单号是取号器生成的，不能按前缀删单据）。
    const CUSTOMER_CODE: &str = "SH8T-CUST";

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

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场顺序受外键约束：发货明细/头 → 订单行（CASCADE 随订单）→ 订单 → 客户。
    /// `biz_shipment.so_id` 是 RESTRICT，所以必须先删发货单再删订单。
    async fn cleanup() {
        let statements: [(&str, Vec<Value>); 3] = [
            (
                "DELETE FROM biz_shipment \
                  WHERE so_id IN (SELECT o.id FROM biz_sales_order o \
                                    JOIN biz_customer c ON c.id = o.customer_id \
                                   WHERE c.code = ?)",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_sales_order \
                  WHERE customer_id IN (SELECT id FROM biz_customer WHERE code = ?)",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_customer WHERE code = ?",
                vec![Value::from(CUSTOMER_CODE)],
            ),
        ];
        for (sql, params) in statements {
            if let Err(err) = db().execute_with_params(sql, params).await {
                panic!("清理夹具失败：{err}\nSQL: {sql}");
            }
        }
    }

    /// 夹具：客户 + 一张订单（两行：A=10 件、B=5 件），返回 `(customer, so_id, item_a, item_b)`。
    async fn fixture() -> (Uuid, Uuid, Uuid, Uuid) {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, 'SO-08 夹具客户', 10000, 'ACTIVE') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("插夹具客户应成功");
        let customer: Uuid = db()
            .scalar_with_params(
                "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
                vec![Value::from(CUSTOMER_CODE)],
            )
            .await
            .expect("查夹具客户应成功");

        let so_id = Uuid::now_v7();
        // ⚠ V0264 的 biz_sales_order **没有** warehouse_id 列（ARCH-7 §3.1.3 的旧稿有，实际未落），
        //   这里只插卡面实有的列。
        db().execute_with_params(
            "INSERT INTO biz_sales_order (id, so_no, customer_id, order_date) \
             VALUES (?, ?, ?, CURRENT_DATE)",
            vec![
                Value::from(so_id),
                Value::from(format!("SH8T-SO-{}", &so_id.to_string()[..8])),
                Value::from(customer),
            ],
        )
        .await
        .expect("插夹具订单应成功");

        let mut item_ids = Vec::new();
        for (line_no, qty) in [(1_i32, "10"), (2, "5")] {
            let item_id = Uuid::now_v7();
            db().execute_with_params(
                "INSERT INTO biz_sales_order_item \
                    (id, so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount) \
                 VALUES (?, ?, ?, gen_random_uuid(), ?::text::numeric, 100, 0, 13, 0)",
                vec![
                    Value::from(item_id),
                    Value::from(so_id),
                    Value::from(line_no),
                    Value::from(qty.to_string()),
                ],
            )
            .await
            .expect("插夹具订单行应成功");
            item_ids.push(item_id);
        }
        (customer, so_id, item_ids[0], item_ids[1])
    }

    /// 插一张发货单（状态可指定），返回发货单 id。
    async fn insert_shipment(so_id: Uuid, customer: Uuid, status: &str) -> Uuid {
        let id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status) \
             VALUES (?, ?, ?, ?, gen_random_uuid(), CURRENT_DATE, ?)",
            vec![
                Value::from(id),
                Value::from(format!("SH8T-{}", &id.to_string()[..8])),
                Value::from(so_id),
                Value::from(customer),
                Value::from(status.to_string()),
            ],
        )
        .await
        .expect("插夹具发货单应成功");
        id
    }

    /// 插一条发货明细（`qty` 走 `?::text::numeric`，驱动不接受 Text 直绑 numeric）。
    async fn insert_item(shipment_id: Uuid, so_item_id: Uuid, qty: &str) {
        db().execute_with_params(
            "INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty) \
             VALUES (?, ?, gen_random_uuid(), ?::text::numeric)",
            vec![
                Value::from(shipment_id),
                Value::from(so_item_id),
                Value::from(qty.to_string()),
            ],
        )
        .await
        .expect("插夹具发货明细应成功");
    }

    /// 取号：连续两次 → 同一天、流水 +1、格式可拆解（真库 `biz_seq` 原子 UPDATE）。
    #[tokio::test]
    async fn next_ship_no_generates_consecutive_numbers_on_real_db() {
        let Some(_guard) = setup().await else { return };
        let now = Utc::now();
        let first = next_ship_no(db(), now).await.expect("第一次取号应成功");
        let second = next_ship_no(db(), now).await.expect("第二次取号应成功");

        let (period_a, no_a) = parse_ship_no(&first).expect("取出的号必须能拆解");
        let (period_b, no_b) = parse_ship_no(&second).expect("取出的号必须能拆解");
        assert_eq!(period_a, period_key_of(now), "周期键必须是北京时间的今天");
        assert_eq!(period_b, period_a, "同一天两次取号周期键应相同");
        assert_eq!(no_b, no_a + 1, "同一天流水必须 +1（{first} → {second}）");
        assert_eq!(first.len(), "SH-yyyyMMdd-000001".len(), "6 位补零：{first}");
        println!("[SO-08] 取号实况：{first} → {second}（period_key={period_a}）");

        let rows = db()
            .query_sql(
                "SELECT current_no, period_key FROM biz_seq WHERE seq_type = 'SH' AND deleted_at IS NULL".to_string(),
                vec![],
            )
            .await
            .expect("读 biz_seq 应成功");
        let row = rows.first().expect("biz_seq(seq_type=SH) 必须存在");
        let current: i64 = row.get_typed("current_no").expect("current_no 可读");
        assert!(current >= no_b, "biz_seq.current_no({current}) 应 ≥ {no_b}");
    }

    /// 默认值与库层约束：status=DRAFT、total_qty=0；qty=0 与越界状态被拒；删头带走明细。
    #[tokio::test]
    async fn defaults_and_check_constraints_on_real_db() {
        let Some(_guard) = setup().await else { return };
        let (customer, so_id, item_a, _item_b) = fixture().await;

        // 最简插入：不传 status / total_qty
        let ship_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date) \
             VALUES (?, ?, ?, ?, gen_random_uuid(), CURRENT_DATE)",
            vec![
                Value::from(ship_id),
                Value::from("SH8T-DEFAULTS"),
                Value::from(so_id),
                Value::from(customer),
            ],
        )
        .await
        .expect("最简插入应成功（status / total_qty 都有 DEFAULT）");
        let rows = db()
            .query_sql(
                "SELECT status, total_qty::text AS t FROM biz_shipment WHERE id = ?".to_string(),
                vec![Value::from(ship_id)],
            )
            .await
            .expect("回读单头应成功");
        let row = rows.first().expect("单头必须存在");
        let status: String = row.get_typed("status").expect("status 可读");
        let total: String = row.get_typed("t").expect("total_qty 可读");
        assert_eq!(status, STATUS_DRAFT, "未传 status 应落默认 DRAFT");
        assert_eq!(total, "0.0000", "total_qty 默认 0");

        // 越界状态被 ck_shipment_status 拒
        let err = db()
            .execute_with_params(
                "INSERT INTO biz_shipment (ship_no, so_id, customer_id, warehouse_id, ship_date, status) \
                 VALUES ('SH8T-BADSTATUS', ?, ?, gen_random_uuid(), CURRENT_DATE, 'SHIPPED')",
                vec![Value::from(so_id), Value::from(customer)],
            )
            .await
            .expect_err("status='SHIPPED' 必须被 ck_shipment_status 拒绝");
        assert!(
            format!("{err}").contains("ck_shipment_status") || format!("{err}").contains("violat"),
            "错误应指向 CHECK 约束，实际：{err}"
        );

        // qty = 0 被 ck_biz_shipment_item_qty 拒
        let err = db()
            .execute_with_params(
                "INSERT INTO biz_shipment_item (shipment_id, so_item_id, material_id, qty) \
                 VALUES (?, ?, gen_random_uuid(), 0)",
                vec![Value::from(ship_id), Value::from(item_a)],
            )
            .await
            .expect_err("qty = 0 必须被 ck_biz_shipment_item_qty 拒绝");
        assert!(
            format!("{err}").contains("ck_biz_shipment_item_qty") || format!("{err}").contains("violat"),
            "错误应指向 CHECK 约束，实际：{err}"
        );

        // 正常明细 + 级联：删单头 → 明细一起消失
        insert_item(ship_id, item_a, "2").await;
        db().execute_with_params("DELETE FROM biz_shipment WHERE id = ?", vec![Value::from(ship_id)])
            .await
            .expect("删单头应成功");
        let left = db()
            .query_sql(
                "SELECT count(*) AS n FROM biz_shipment_item WHERE shipment_id = ?".to_string(),
                vec![Value::from(ship_id)],
            )
            .await
            .expect("数残留明细应成功");
        let n: i64 = left
            .first()
            .expect("count 必有一行")
            .get_typed("n")
            .expect("count 可读");
        assert_eq!(n, 0, "ON DELETE CASCADE 未生效：明细还剩 {n} 行");
        cleanup().await;
    }

    /// **超发校验生效**（卡面第二条验收）：已发 4，剩 6 ⇒ 7 拒、6 放行；
    /// 再插一张草稿单占 6 ⇒ 剩 0 ⇒ 0.0001 也拒；作废该单 ⇒ 剩余回来。
    #[tokio::test]
    async fn over_ship_guard_blocks_more_than_remaining() {
        let Some(_guard) = setup().await else { return };
        let (customer, so_id, item_a, _item_b) = fixture().await;

        // 订单行 A：qty=10，已过账 4
        db().execute_with_params(
            "UPDATE biz_sales_order_item SET delivered_qty = 4 WHERE id = ?",
            vec![Value::from(item_a)],
        )
        .await
        .expect("回写 delivered_qty 应成功");

        let snapshot = ensure_qty_within_remaining(db(), item_a, dec("6"), None)
            .await
            .expect("发剩余 6 应放行");
        assert_eq!(snapshot.remaining, dec("6"));
        assert_eq!(snapshot.delivered, dec("4"));
        println!("[SO-08] 超发探针：订单 10 / 已发 4 ⇒ 未发 {}", snapshot.remaining);

        // 超 0.0001 就拒（不是「差一点也行」）
        let err = ensure_qty_within_remaining(db(), item_a, dec("6.0001"), None)
            .await
            .expect_err("发 6.0001 必须被拒（未发只有 6）");
        let text = format!("{err}");
        assert!(text.contains("超发"), "错误文案应说明超发，实际：{text}");
        assert!(text.contains("未发货量仅 6"), "文案应回显剩余量，实际：{text}");

        // 非正数量也拒（库层 qty>0 的应用层同款，给 400 而不是 23514）
        ensure_qty_within_remaining(db(), item_a, Decimal::ZERO, None)
            .await
            .expect_err("qty = 0 必须被拒");
        ensure_qty_within_remaining(db(), item_a, dec("-1"), None)
            .await
            .expect_err("qty < 0 必须被拒");

        // 草稿单占满剩余 6 ⇒ 未发 0
        let draft = insert_shipment(so_id, customer, STATUS_DRAFT).await;
        insert_item(draft, item_a, "6").await;
        let err = ensure_qty_within_remaining(db(), item_a, dec("0.0001"), None)
            .await
            .expect_err("草稿单占满后必须拒");
        assert!(format!("{err}").contains("超发"));
        // 改单排除本单 ⇒ 自己占的 6 不算，仍可发 6
        let snapshot = ensure_qty_within_remaining(db(), item_a, dec("6"), Some(draft))
            .await
            .expect("排除本单后应重新放行 6");
        assert_eq!(snapshot.reserved, Decimal::ZERO);
        assert_eq!(snapshot.remaining, dec("6"));

        // 作废（CANCELLED）⇒ 占用释放
        db().execute_with_params(
            "UPDATE biz_shipment SET status = 'CANCELLED' WHERE id = ?",
            vec![Value::from(draft)],
        )
        .await
        .expect("作废应成功");
        let snapshot = ensure_qty_within_remaining(db(), item_a, dec("6"), None)
            .await
            .expect("作废后剩余 6 应回来");
        assert_eq!(snapshot.reserved, Decimal::ZERO);

        cleanup().await;
    }

    /// 过账后的发货量不重复扣：POSTED 单已计入 `delivered_qty`，不能再算「占用」。
    #[tokio::test]
    async fn posted_shipment_is_not_counted_twice() {
        let Some(_guard) = setup().await else { return };
        let (customer, so_id, item_a, _item_b) = fixture().await;

        // 一张已过账的单发 4，同时 delivered_qty 已回写 4（SO-10 的口径）
        let posted = insert_shipment(so_id, customer, STATUS_POSTED).await;
        insert_item(posted, item_a, "4").await;
        db().execute_with_params(
            "UPDATE biz_sales_order_item SET delivered_qty = 4 WHERE id = ?",
            vec![Value::from(item_a)],
        )
        .await
        .expect("回写 delivered_qty 应成功");

        let snapshot = availability_of(db(), item_a, None).await.expect("读可发量应成功");
        assert_eq!(snapshot.delivered, dec("4"), "已发 = 过账累计");
        assert_eq!(
            snapshot.reserved,
            Decimal::ZERO,
            "POSTED 不能再算未过账占用（否则扣两次，BUG）"
        );
        assert_eq!(snapshot.remaining, dec("6"), "未发 = 10 − 4 = 6");

        cleanup().await;
    }

    /// `shippable_items`：只带未发完的行，且随草稿占用实时减少。
    #[tokio::test]
    async fn shippable_items_skip_fully_delivered_lines() {
        let Some(_guard) = setup().await else { return };
        let (customer, so_id, item_a, item_b) = fixture().await;

        let items = shippable_items(db(), so_id).await.expect("读可发明细应成功");
        assert_eq!(items.len(), 2, "两行都没发 ⇒ 两行都可发");
        assert_eq!(items[0].line_no, 1, "按行号升序");
        assert_eq!(items[0].so_item_id, item_a);
        assert_eq!(items[0].availability.remaining, dec("10"));
        assert_eq!(items[1].availability.remaining, dec("5"));

        // A 行发完 ⇒ 只剩 B
        let posted = insert_shipment(so_id, customer, STATUS_DRAFT).await;
        insert_item(posted, item_a, "10").await;
        db().execute_with_params(
            "UPDATE biz_shipment SET status = 'POSTED' WHERE id = ?",
            vec![Value::from(posted)],
        )
        .await
        .expect("置为已过账应成功");
        db().execute_with_params(
            "UPDATE biz_sales_order_item SET delivered_qty = 10 WHERE id = ?",
            vec![Value::from(item_a)],
        )
        .await
        .expect("回写 delivered_qty 应成功");

        let items = shippable_items(db(), so_id).await.expect("再读可发明细应成功");
        assert_eq!(items.len(), 1, "A 发完后不该再出现");
        assert_eq!(items[0].so_item_id, item_b);

        cleanup().await;
    }

    // ---------- SO-09：CRUD / 状态落库 / 库存闸 的真库用例 ----------

    /// 夹具仓库编码（清场按它定位：仓库表没有软删的话不能按单号删）。
    const WAREHOUSE_CODE: &str = "SH9T-WH";

    /// 建一个夹具仓库（`warehouse_path` / `level` / `is_leaf` 都有默认值，只填必填三列）。
    ///
    /// **幂等**：`warehouse_code` 是唯一键，上一轮留下的脏行不能让本轮炸
    /// （先查再插；`ON CONFLICT (id)` 挡不住「同 code 不同 id」）。
    async fn fixture_warehouse() -> Uuid {
        let rows = db()
            .query_sql(
                "SELECT id FROM biz_warehouse WHERE warehouse_code = ? AND deleted_at IS NULL".to_string(),
                vec![Value::from(WAREHOUSE_CODE)],
            )
            .await
            .expect("查夹具仓库应成功");
        if let Some(row) = rows.first() {
            return row.get_typed("id").expect("仓库 id 可读");
        }
        let warehouse_id = Uuid::now_v7();
        db().execute_with_params(
            "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_path) \
             VALUES (?, ?, 'SO-09 夹具仓', ?)",
            vec![
                Value::from(warehouse_id),
                Value::from(WAREHOUSE_CODE),
                Value::from(format!("/{WAREHOUSE_CODE}")),
            ],
        )
        .await
        .expect("插夹具仓库应成功");
        warehouse_id
    }

    /// 清掉库存闸夹具（库位账 → 仓库）。
    async fn cleanup_stock_fixture() {
        for sql in [
            "DELETE FROM biz_inventory_location \
              WHERE warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code = ?)",
            "DELETE FROM biz_inventory WHERE warehouse_id IN \
              (SELECT id FROM biz_warehouse WHERE warehouse_code = ?)",
            "DELETE FROM biz_warehouse WHERE warehouse_code = ?",
        ] {
            db().execute_with_params(sql, vec![Value::from(WAREHOUSE_CODE)])
                .await
                .expect("清理库存闸夹具应成功");
        }
    }

    /// 建单：取号 `SH-yyyyMMdd-000001` 形态、`total_qty = Σqty`、明细行号可回读、
    /// `biz_status_log` 有建单留痕（`from_status IS NULL`）。
    #[tokio::test]
    async fn create_shipment_writes_header_items_and_log() {
        let Some(_guard) = setup().await else { return };
        let (customer, so_id, item_a, item_b) = fixture().await;
        let warehouse_id = fixture_warehouse().await;
        let now = Utc::now();
        let new = NewShipment {
            so_id,
            customer_id: customer,
            warehouse_id,
            ship_date: now.date_naive(),
            remark: Some("SO-09 建单夹具"),
        };
        let items = vec![
            NewShipmentItem {
                so_item_id: item_a,
                material_id: so_item_brief(db(), item_a)
                    .await
                    .expect("查订单行应成功")
                    .expect("订单行必须存在")
                    .0,
                qty: dec("4"),
                location_id: None,
                serial_nos: vec!["SN-SO9-1".to_string(), "SN-SO9-2".to_string()],
                remark: None,
            },
            NewShipmentItem {
                so_item_id: item_b,
                material_id: so_item_brief(db(), item_b)
                    .await
                    .expect("查订单行应成功")
                    .expect("订单行必须存在")
                    .0,
                qty: dec("2.5"),
                location_id: None,
                serial_nos: Vec::new(),
                remark: Some("第二行".to_string()),
            },
        ];
        let (id, ship_no) = create_shipment(db(), &new, &items, state_machine::Operator::default(), now)
            .await
            .expect("建单应成功");
        assert!(is_valid_ship_no(&ship_no), "单号必须合 SH-yyyyMMdd-NNNNNN：{ship_no}");

        let row = find_shipment(id).await.expect("查单头应成功").expect("单头必须存在");
        assert_eq!(row.ship_no, ship_no);
        assert_eq!(row.status, STATUS_DRAFT, "新单必须是草稿");
        assert_eq!(row.total_qty, dec("6.5"), "total_qty = Σqty");
        assert_eq!(row.remark.as_deref(), Some("SO-09 建单夹具"));
        assert!(row.stock_out_id.is_none(), "未过账不该有出库单");
        assert!(!row.is_locked(), "草稿不是锁定态");

        let lines = items_of(id).await.expect("查明细应成功");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].so_item_id, item_a, "明细按订单行号升序");
        assert_eq!(lines[0].line_no, 1);
        assert_eq!(lines[0].serial_nos, vec!["SN-SO9-1", "SN-SO9-2"]);
        assert_eq!(lines[1].line_no, 2);
        assert_eq!(lines[1].qty, dec("2.5"));
        assert_eq!(lines[1].remark.as_deref(), Some("第二行"));

        // 建单留痕：biz_status_log 有 DRAFT 行（from_status 为空）
        let rows = db()
            .query_sql(
                "SELECT from_status, to_status FROM biz_status_log \
                  WHERE entity_type = 'SHIPMENT' AND entity_id = ?::uuid"
                    .to_string(),
                vec![Value::from(id)],
            )
            .await
            .expect("查状态日志应成功");
        let first = rows.first().expect("建单必须写一条状态日志");
        assert_eq!(
            first
                .get_typed::<Option<String>>("to_status")
                .expect("to_status 可读")
                .as_deref(),
            Some(STATUS_DRAFT)
        );
        assert!(
            first
                .get_typed::<Option<String>>("from_status")
                .expect("from_status 可读")
                .is_none(),
            "建单行的 from_status 必须为空"
        );

        cleanup().await;
        cleanup_stock_fixture().await;
    }

    /// 状态落库是 CAS：`WHERE status = from` 命中才改；旧 from 再写一次返回 0（并发被别人推走）。
    #[tokio::test]
    async fn apply_status_is_cas_guarded() {
        let Some(_guard) = setup().await else { return };
        let (customer, so_id, _item_a, _item_b) = fixture().await;
        let id = insert_shipment(so_id, customer, STATUS_DRAFT).await;
        let now = Utc::now();

        let hit = apply_status(db(), id, STATUS_DRAFT, STATUS_PENDING_POST, None, now)
            .await
            .expect("首次流转应成功");
        assert_eq!(hit, 1, "CAS 首次命中 1 行");
        let row = find_shipment(id).await.expect("查单头应成功").expect("单头存在");
        assert_eq!(row.status, STATUS_PENDING_POST);

        let miss = apply_status(db(), id, STATUS_DRAFT, STATUS_PENDING_POST, None, now)
            .await
            .expect("重复流转不应报 SQL 错");
        assert_eq!(miss, 0, "期望态已被推走 ⇒ 0 行（handler 按 409 处理）");

        cleanup().await;
    }

    /// 库存闸：按库位账的可用量判（与出库过账同一个 `fifo_plan`），
    /// 不够量逐行给缺口；补足后为空。
    #[tokio::test]
    async fn stock_shortfalls_follow_location_ledger() {
        let Some(_guard) = setup().await else { return };
        let (_customer, _so_id, item_a, _item_b) = fixture().await;
        let warehouse_id = fixture_warehouse().await;
        let material_id = so_item_brief(db(), item_a)
            .await
            .expect("查订单行应成功")
            .expect("订单行必须存在")
            .0;
        let item = ShipmentItemRow {
            id: Uuid::now_v7(),
            shipment_id: Uuid::now_v7(),
            so_item_id: item_a,
            material_id,
            qty: dec("5"),
            location_id: None,
            serial_nos: Vec::new(),
            remark: None,
            line_no: 1,
        };

        // 一件库存都没有 ⇒ 整行缺口 5
        let gaps = stock_shortfalls(db(), warehouse_id, std::slice::from_ref(&item))
            .await
            .expect("查缺口应成功");
        assert_eq!(gaps.len(), 1, "没库存必须报缺口");
        assert_eq!(gaps[0].available, Decimal::ZERO);
        assert_eq!(gaps[0].shortfall(), dec("5"));

        // 库位账只有 3（未分配桶）⇒ 缺 2
        db().execute_with_params(
            "INSERT INTO biz_inventory_location (material_id, warehouse_id, location_id, qty) \
             VALUES (?, ?, NULL, 3)",
            vec![Value::from(material_id), Value::from(warehouse_id)],
        )
        .await
        .expect("插库位账应成功");
        let gaps = stock_shortfalls(db(), warehouse_id, std::slice::from_ref(&item))
            .await
            .expect("再查缺口应成功");
        assert_eq!(gaps[0].available, dec("3"));
        assert_eq!(gaps[0].shortfall(), dec("2"));

        // 补到 5 ⇒ 够量（`shortfall` 为 0 的行不进结果）
        db().execute_with_params(
            "UPDATE biz_inventory_location SET qty = 5 \
              WHERE material_id = ? AND warehouse_id = ? AND location_id IS NULL",
            vec![Value::from(material_id), Value::from(warehouse_id)],
        )
        .await
        .expect("补库存应成功");
        let gaps = stock_shortfalls(db(), warehouse_id, std::slice::from_ref(&item))
            .await
            .expect("三查缺口应成功");
        assert!(gaps.is_empty(), "够量时不该报缺口：{gaps:?}");

        cleanup().await;
        cleanup_stock_fixture().await;
    }
}

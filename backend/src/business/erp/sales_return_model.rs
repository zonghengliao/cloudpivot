//! 销售退货单数据层（销售链 · SO-29）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0303__sales_return.up.sql`
//! （`biz_sales_return` / `biz_sales_return_item`，SO-28 交付）。
//!
//! | 列 | 口径 |
//! |----|------|
//! | `return_no` | `SR-yyyyMMdd-000001`（`biz_seq seq_type='SR'`，按 CST 的天滚动） |
//! | `status` | 6 值（ARCH-7 §15.2.4）：`DRAFT`/`SUBMITTED`/`APPROVED`/`PENDING_POST`/`POSTED`/`CANCELLED` |
//! | `return_type` | 3 值：`QUALITY`（质量）/ `WRONG_ITEM`（错发）/ `OTHER`（其他） |
//! | 金额 | 全 `NUMERIC(18,4)`；`total_with_tax = total_amount + total_tax`（库层 `ck_biz_sreturn_amount`） |
//! | `so_id` / `shipment_id` / 主数据列 | 弱引用，无外键（ADR-06 单据链零外键，见 V0303 头部注释） |
//!
//! # 本文件的范围
//!
//! SO-29 = CRUD 的数据层 + 状态机落库 + **超退守卫**。过账（回库 / 冲应收 / 红字凭证）
//! 是 SO-30 的活：本文件**一行库存 SQL 都不写**，三本账的唯一写入口仍是
//! `inventory_writer`（经入库单过账内核）。
//!
//! # 超退口径（跨表，库层 CHECK 表达不了）
//!
//! ```text
//! 可退量 = 已发货量 − 已退量
//!   已发货量：指定原发货单时 = 该单该订单行已发出的数量（单必须是 POSTED）；
//!             没指定发货单时 = biz_sales_order_item.delivered_qty（过账累计）
//!   已退量  ：同一 (so_item_id[, shipment_id]) 上「未作废」退货单的 qty 合计
//!             （deleted_at IS NULL AND status <> 'CANCELLED'）
//! ```
//!
//! * 作废（CANCELLED）的退货**不占**已退量 —— 改动状态即自动释放，不需要改数量；
//! * 判定必须在**同一事务**内、并且先 [`lock_shipment`] / [`lock_so_item`]
//!   （`SELECT … FOR UPDATE`）再算，否则两个并发建单会各看到同一份余量；
//! * 判定在**提交 / 待过账**时**再跑一遍**：草稿期间别人可能已经退过同一行。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_sales_return / biz_sales_return_item 是 ERP 单据表（V0303 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:sales-return:*
//! 权限码控制。分页走 return_page → page_of → paginate_unscoped。
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
pub const TABLE: &str = "biz_sales_return";

/// 明细表（无 `deleted_at`：明细随单头走，外键 `ON DELETE CASCADE`）。
pub const ITEM_TABLE: &str = "biz_sales_return_item";

/// 单号前缀（卡面：`SR-YYYYMMDD-NNNNNN`）。
pub const NO_PREFIX: &str = "SR";

/// 流水号补零宽度（超过 6 位不截断，宁可长一位也不撞号）。
pub const NO_SEQ_WIDTH: usize = 6;

/// `biz_seq.seq_type`（V0303 种下的 `SR` 行）。
pub const SEQ_TYPE: &str = "SR";

/// 状态：草稿（**默认值**，唯一允许改明细的状态；本卡只提供建单，改单接口不在卡面）。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已提交（等审批）。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 状态：已批准（可以推进到待过账）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 状态：待过账（等 SO-30 过账：回库 + 冲应收 + 红字凭证）。
pub const STATUS_PENDING_POST: &str = "PENDING_POST";
/// 状态：已过账（SO-30 写入；本卡不解锁这条边的执行）。
pub const STATUS_POSTED: &str = "POSTED";
/// 状态：已作废（终态；作废即释放已退量占用，库存不动）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 状态域白名单（与 `ck_biz_sreturn_status` **逐字同序**，单测钉住）。
pub const STATUSES: [&str; 6] = [
    STATUS_DRAFT,
    STATUS_SUBMITTED,
    STATUS_APPROVED,
    STATUS_PENDING_POST,
    STATUS_POSTED,
    STATUS_CANCELLED,
];

/// 退货类型：质量。
pub const TYPE_QUALITY: &str = "QUALITY";
/// 退货类型：错发。
pub const TYPE_WRONG_ITEM: &str = "WRONG_ITEM";
/// 退货类型：其他。
pub const TYPE_OTHER: &str = "OTHER";

/// 退货类型白名单（与 `ck_biz_sreturn_type` **逐字同序**）。
pub const RETURN_TYPES: [&str; 3] = [TYPE_QUALITY, TYPE_WRONG_ITEM, TYPE_OTHER];

/// 状态是否在域内。
pub fn is_valid_status(value: &str) -> bool {
    STATUSES.contains(&value)
}

/// 退货类型是否在域内。
pub fn is_valid_return_type(value: &str) -> bool {
    RETURN_TYPES.contains(&value)
}

/// 状态中文标签（错误文案与前端徽标共用；未知值返回「未知」，不 panic）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "已提交",
        STATUS_APPROVED => "已批准",
        STATUS_PENDING_POST => "待过账",
        STATUS_POSTED => "已过账",
        STATUS_CANCELLED => "已取消",
        _ => "未知",
    }
}

/// 退货类型中文标签（未知值返回「未知」）。
pub fn return_type_label(value: &str) -> &'static str {
    match value {
        TYPE_QUALITY => "质量",
        TYPE_WRONG_ITEM => "错发",
        TYPE_OTHER => "其他",
        _ => "未知",
    }
}

// ==================== 单号 ====================

/// 拼单号：`SR-yyyyMMdd-NNNNNN`。
pub fn format_return_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 解析单号 → `(period_key, 流水号)`；格式不对返回 `None`。
pub fn parse_return_no(value: &str) -> Option<(String, i64)> {
    let rest = value.trim().strip_prefix(NO_PREFIX)?.strip_prefix('-')?;
    let (period, no) = rest.split_once('-')?;
    if period.len() != 8 || !period.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let no: i64 = no.parse().ok()?;
    if no <= 0 {
        return None;
    }
    Some((period.to_string(), no))
}

/// 单号是否合法（[`parse_return_no`] 的布尔薄壳）。
pub fn is_valid_return_no(value: &str) -> bool {
    parse_return_no(value).is_some()
}

// ==================== 超退判定（纯逻辑） ====================

/// 可退量 = `shipped - returned`，**下限 0**。
///
/// 夹 0 是因为并发窗口里可能瞬时「超退」（另一事务刚退过），负数剩余会让 UI 显示
/// `-0.5` 这种怪值；真正的判定仍由 [`qty_within_returnable`]（`qty <= remaining`）负责。
pub fn remaining_returnable(shipped: Decimal, returned: Decimal) -> Decimal {
    let left = shipped - returned;
    if left < Decimal::ZERO {
        Decimal::ZERO
    } else {
        left
    }
}

/// 本行退货量是否允许：`qty > 0` **且** `qty <= 已发货量 − 已退量`。
///
/// 两个条件对应卡面「退货数量 <= 已发货量 - 已退量」；库层只兜住了 `qty > 0`
/// （`ck_biz_sreturn_item_qty`），上界是跨表的，只能在这里判。
pub fn qty_within_returnable(qty: Decimal, shipped: Decimal, returned: Decimal) -> bool {
    qty > Decimal::ZERO && qty <= remaining_returnable(shipped, returned)
}

/// 行金额拆分：`(amount, tax_amount)`，各 4 位定标。
///
/// 与销售订单**同源**（直接调 `sales_order_model::line_amounts`）：
/// `amount = round(price × qty, 4)`、`tax = round(amount × rate / 100, 4)`，
/// 四舍五入（不是 `round_dp` 默认的银行家舍入）—— 两处口径不许各写一份。
pub fn line_amounts(qty: Decimal, price: Decimal, tax_rate: Decimal) -> (Decimal, Decimal) {
    crate::business::erp::sales_order_model::line_amounts(qty, price, tax_rate)
}

/// 明细合计：`(total_amount, total_tax, total_with_tax)`。
///
/// 逐行 4 位定标后相加，保证与库层 `ck_biz_sreturn_amount`（精确恒等）一致。
pub fn totals_of(items: &[NewReturnItem]) -> (Decimal, Decimal, Decimal) {
    let mut amount = Decimal::ZERO;
    let mut tax = Decimal::ZERO;
    for item in items {
        amount += item.amount;
        tax += item.tax_amount;
    }
    let amount = amount.round_dp(4);
    let tax = tax.round_dp(4);
    (amount, tax, amount + tax)
}

// ==================== 取号（`SR-yyyyMMdd-000001`） ====================

/// `biz_seq(seq_type='SR')` 缺失时幂等补一行（正常库有 V0303 的种子）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'SR', '销售退货单号', 'SR', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '销售退货单单号：SR-yyyyMMdd-000001，按天（CST）滚动' \
          WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SR' AND deleted_at IS NULL)"
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

/// 推进 `biz_seq(seq_type='SR')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
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
              WHERE seq_type = 'SR' AND deleted_at IS NULL \
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

/// 取一个退货单号（周期键按北京时间）。
///
/// 调用方必须在**同一个事务**里调用本函数 + 写单头 + 写明细：取号把 `current_no`
/// 推进了却没落单，那个号就永久占用（跳号可接受，重号不可接受）。
pub async fn next_return_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_return_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='SR') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 超退守卫（真库） ====================

/// 可退明细的一行（[`returnable_lines`] 的返回值，也是「从原发货单带出」的读数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnableLine {
    /// 原销售订单行。
    pub so_item_id: Uuid,
    /// 订单行号（1 起；订单行已被物理删时回落 0）。
    pub line_no: i32,
    /// 物料（取自发货行）。
    pub material_id: Uuid,
    /// 已发货量（该发货行发出的数量）。
    pub shipped: Decimal,
    /// 已退量（同一 `(so_item_id, shipment_id)` 上未作废退货的合计）。
    pub returned: Decimal,
    /// 可退量（`shipped - returned`，下限 0）。
    pub remaining: Decimal,
    /// 单价（取自订单行，拷贝不丢精度）。
    pub price: Decimal,
    /// 税率（百分数，取自订单行）。
    pub tax_rate: Decimal,
}

/// 锁住一张发货单（`SELECT … FOR UPDATE`），把同一张单上的并发退货串行化。
///
/// 必须在**事务**里调用。返回该单的状态（不存在 → `None`，handler 报 404）。
pub async fn lock_shipment(ex: &impl SqlExecutor, shipment_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 发货单是 ERP 单据表，无归属列。
    let rows = ex
        .query_sql(
            "SELECT status FROM biz_shipment \
              WHERE id = ?::uuid AND deleted_at IS NULL FOR UPDATE"
                .to_string(),
            vec![Value::from(shipment_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("status")).transpose()
}

/// 锁住一行销售订单明细（`SELECT … FOR UPDATE`），把同一订单行的并发退货串行化。
pub async fn lock_so_item(ex: &impl SqlExecutor, so_item_id: Uuid) -> DbResult<()> {
    let rows = ex
        .query_sql(
            "SELECT id FROM biz_sales_order_item WHERE id = ?::uuid FOR UPDATE".to_string(),
            vec![Value::from(so_item_id)],
        )
        .await?;
    if rows.is_empty() {
        return Err(DbError::InvalidValue(format!(
            "销售订单行不存在：{so_item_id}"
        )));
    }
    Ok(())
}

/// 发货单的元信息：`(customer_id, so_id, status)`；不存在 / 已软删 → `None`。
pub async fn shipment_meta(
    ex: &impl SqlExecutor,
    shipment_id: Uuid,
) -> DbResult<Option<(Uuid, Uuid, String)>> {
    // SCOPE-EXEMPT: 发货单是 ERP 单据表，无归属列。
    let rows = ex
        .query_sql(
            "SELECT customer_id, so_id, status FROM biz_shipment \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(shipment_id)],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some((
        row.get_typed("customer_id")?,
        row.get_typed("so_id")?,
        row.get_typed("status")?,
    )))
}

/// 发货单的可退明细（「从原发货单带出可退明细」的唯一取数口径）。
///
/// 只返回 `remaining > 0` 的行；`returned` 只算**同一张发货单**上未作废退货的占用
/// （别的发货单退过多少不影响本单的可退量 —— 每张发货单单独结算）。
pub async fn returnable_lines(
    ex: &impl SqlExecutor,
    shipment_id: Uuid,
    exclude_return: Option<Uuid>,
) -> DbResult<Vec<ReturnableLine>> {
    // 排除本单用 `?::uuid` + 哨兵 nil：退货单 id 永不为 nil，等价于「不排除」，
    // 这样 SQL 只有一条（两条 SQL 迟早漂移）。**改单 / 提交时必须排除自己**，
    // 否则本单自己的数量会被算成「别人已退」，10 退 10 的单永远提交不了。
    let exclude = exclude_return.unwrap_or_else(Uuid::nil);
    // SCOPE-EXEMPT: 发货单 / 订单行 / 退货单都是 ERP 单据表，无归属列。
    let rows = ex
        .query_sql(
            "SELECT si.so_item_id, \
                    COALESCE(oi.line_no, 0) AS line_no, \
                    si.material_id, si.qty::text AS shipped, \
                    COALESCE(oi.price::text, '0') AS price, \
                    COALESCE(oi.tax_rate::text, '0') AS tax_rate, \
                    COALESCE((SELECT SUM(ri.qty) \
                                FROM biz_sales_return_item ri \
                                JOIN biz_sales_return r ON r.id = ri.return_id \
                               WHERE ri.so_item_id = si.so_item_id \
                                 AND r.shipment_id = ?::uuid \
                                 AND r.deleted_at IS NULL \
                                 AND r.status <> ? \
                                 AND r.id <> ?::uuid), 0)::text AS returned \
               FROM biz_shipment_item si \
               LEFT JOIN biz_sales_order_item oi ON oi.id = si.so_item_id \
              WHERE si.shipment_id = ?::uuid \
              ORDER BY COALESCE(oi.line_no, 0), si.id"
                .to_string(),
            vec![
                Value::from(shipment_id),
                Value::from(STATUS_CANCELLED),
                Value::from(exclude),
                Value::from(shipment_id),
            ],
        )
        .await?;
    let mut lines = Vec::new();
    for row in rows {
        let text = |column: &str| -> Decimal {
            row.get_typed::<String>(column)
                .ok()
                .and_then(|value| value.parse::<Decimal>().ok())
                .unwrap_or(Decimal::ZERO)
        };
        let shipped = text("shipped");
        let returned = text("returned");
        let remaining = remaining_returnable(shipped, returned);
        if remaining <= Decimal::ZERO {
            continue;
        }
        lines.push(ReturnableLine {
            so_item_id: row.get_typed("so_item_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            shipped,
            returned,
            remaining,
            price: text("price"),
            tax_rate: text("tax_rate"),
        });
    }
    Ok(lines)
}

/// 订单行的可退快照：`(material_id, price, tax_rate, shipped, returned)`。
///
/// 没指定发货单时用它：`shipped = delivered_qty`（过账累计），
/// `returned` = 该订单行上**全部**未作废退货的合计。
pub async fn so_item_snapshot(
    ex: &impl SqlExecutor,
    so_item_id: Uuid,
    exclude_return: Option<Uuid>,
) -> DbResult<Option<(Uuid, Decimal, Decimal, Decimal, Decimal)>> {
    // 同上：哨兵 nil 表示不排除（本函数被「建单编译明细」与「提交前重判」两处共用）
    let exclude = exclude_return.unwrap_or_else(Uuid::nil);
    // SCOPE-EXEMPT: 订单行 / 退货单都是 ERP 单据表，无归属列。
    let rows = ex
        .query_sql(
            "SELECT i.material_id, i.price::text AS price, i.tax_rate::text AS tax_rate, \
                    i.delivered_qty::text AS shipped, \
                    COALESCE((SELECT SUM(ri.qty) \
                                FROM biz_sales_return_item ri \
                                JOIN biz_sales_return r ON r.id = ri.return_id \
                               WHERE ri.so_item_id = i.id \
                                 AND r.deleted_at IS NULL \
                                 AND r.status <> ? \
                                 AND r.id <> ?::uuid), 0)::text AS returned \
               FROM biz_sales_order_item i \
              WHERE i.id = ?::uuid"
                .to_string(),
            vec![
                Value::from(STATUS_CANCELLED),
                Value::from(exclude),
                Value::from(so_item_id),
            ],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let text = |column: &str| -> Decimal {
        row.get_typed::<String>(column)
            .ok()
            .and_then(|value| value.parse::<Decimal>().ok())
            .unwrap_or(Decimal::ZERO)
    };
    Ok(Some((
        row.get_typed("material_id")?,
        text("price"),
        text("tax_rate"),
        text("shipped"),
        text("returned"),
    )))
}

/// 超退守卫：`qty > 0` 且 `qty <= 已发货量 − 已退量`，否则 `InvalidValue`（handler 侧是 400）。
///
/// * 传了 `shipment_id` ⇒ 已发货量取**该发货单该行**发出的数量（未过账的发货单 handler
///   在更外层就拒了，这里不重复判）；
/// * 没传 ⇒ 已发货量取 `biz_sales_order_item.delivered_qty`（过账累计）。
///
/// `exclude_return` = 改单 / 提交前重判时**排除本单自己**（否则要把自己算成「已退」）。
/// 建单时传 `None`。
///
/// **调用方必须已在事务内先 [`lock_shipment`] / [`lock_so_item`]**。
pub async fn ensure_qty_within_returnable(
    ex: &impl SqlExecutor,
    shipment_id: Option<Uuid>,
    so_item_id: Uuid,
    qty: Decimal,
    exclude_return: Option<Uuid>,
) -> DbResult<(Decimal, Decimal)> {
    if qty <= Decimal::ZERO {
        return Err(DbError::InvalidValue(format!(
            "退货数量必须大于 0（实际 {qty}）"
        )));
    }
    let (shipped, returned) = match shipment_id {
        Some(shipment_id) => {
            let lines = returnable_lines(ex, shipment_id, exclude_return).await?;
            match lines.into_iter().find(|line| line.so_item_id == so_item_id) {
                Some(line) => (line.shipped, line.returned),
                None => {
                    // 该行在本单上已无可退量：再查一次原始已发量，报错文案才说得清
                    let snapshot = so_item_snapshot(ex, so_item_id, exclude_return).await?;
                    let (shipped, returned) = snapshot
                        .map(|(_, _, _, shipped, returned)| (shipped, returned))
                        .unwrap_or((Decimal::ZERO, Decimal::ZERO));
                    return Err(DbError::InvalidValue(format!(
                        "超退：订单行 {so_item_id} 不在发货单 {shipment_id} 的可退明细里（已发 {shipped} − 已退 {returned} = 0）"
                    )));
                }
            }
        }
        None => {
            let Some((_, _, _, shipped, returned)) =
                so_item_snapshot(ex, so_item_id, exclude_return).await?
            else {
                return Err(DbError::InvalidValue(format!(
                    "销售订单行不存在：{so_item_id}"
                )));
            };
            (shipped, returned)
        }
    };
    if !qty_within_returnable(qty, shipped, returned) {
        return Err(DbError::InvalidValue(format!(
            "超退：订单行 {so_item_id} 可退量仅 {}（已发 {shipped} − 已退 {returned}），本次要退 {qty}",
            remaining_returnable(shipped, returned)
        )));
    }
    Ok((shipped, returned))
}

// ==================== 行结构 ====================

/// 单头列清单（与 V0303 逐列对应）。
pub const RETURN_COLUMNS: &str = "id, return_no, customer_id, so_id, shipment_id, return_date, \
     return_type, warehouse_id, status, total_amount, total_tax, total_with_tax, \
     stock_in_id, receivable_id, red_invoice_id, approval_instance_id, remark, \
     created_at, updated_at, create_by, update_by, deleted_at";

/// 明细查询：`serial_nos` 是 `TEXT[]`，用 `array_to_string` 转成逗号串再在 Rust 侧拆
/// （驱动没有 `text[]` 解码器，直读会在运行期炸 —— 与 `shipment_model` 的数组坑同源）。
const ITEM_SELECT: &str = "SELECT ri.id, ri.return_id, ri.so_item_id, ri.material_id, \
     ri.qty, ri.price, ri.amount, ri.tax_rate, ri.tax_amount, ri.location_id, \
     COALESCE(array_to_string(ri.serial_nos, ','), '') AS serial_nos, ri.remark, \
     COALESCE(oi.line_no, 0) AS line_no \
     FROM biz_sales_return_item ri \
     LEFT JOIN biz_sales_order_item oi ON oi.id = ri.so_item_id";

/// 合法边表：**唯一真相**在 `state_machine::SALES_RETURN_EDGES`（本文件只引用，不复制）。
pub const LEGAL_EDGES: [(&str, &str); 7] = state_machine::SALES_RETURN_EDGES;

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
pub const RETURN_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

/// 列表排序：退货日期倒序、同一天按单号倒序（最新单在前）。
pub const RETURN_ORDER: &str = "ORDER BY return_date DESC, return_no DESC";

/// `biz_sales_return` 一行。
#[derive(Debug, Clone)]
pub struct ReturnRow {
    /// 主键。
    pub id: Uuid,
    /// 单号（`SR-yyyyMMdd-000001`，存活行唯一）。
    pub return_no: String,
    /// 客户（弱引用 `biz_customer.id`）。
    pub customer_id: Uuid,
    /// 原销售订单（弱引用，可空）。
    pub so_id: Option<Uuid>,
    /// 原发货单（弱引用，可空）。
    pub shipment_id: Option<Uuid>,
    /// 退货日期。
    pub return_date: NaiveDate,
    /// 退货类型（3 值之一）。
    pub return_type: String,
    /// 退回入库仓库（弱引用 `biz_warehouse.id`）。
    pub warehouse_id: Uuid,
    /// 状态（6 值之一）。
    pub status: String,
    /// 不含税金额合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计（= `total_amount + total_tax`）。
    pub total_with_tax: Decimal,
    /// 过账后生成的入库单（SO-30 回填；未过账为 `None`）。
    pub stock_in_id: Option<Uuid>,
    /// 冲减的应收单（SO-30 回填）。
    pub receivable_id: Option<Uuid>,
    /// 红字发票（SO-30 回填）。
    pub red_invoice_id: Option<Uuid>,
    /// 审批实例（本卡不接审批流，恒为 `None`）。
    pub approval_instance_id: Option<Uuid>,
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

impl ReturnRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            return_no: row.get_typed("return_no")?,
            customer_id: row.get_typed("customer_id")?,
            so_id: row.get_typed("so_id")?,
            shipment_id: row.get_typed("shipment_id")?,
            return_date: row.get_typed("return_date")?,
            return_type: row.get_typed("return_type")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            status: row.get_typed("status")?,
            total_amount: row.get_typed("total_amount")?,
            total_tax: row.get_typed("total_tax")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            stock_in_id: row.get_typed("stock_in_id")?,
            receivable_id: row.get_typed("receivable_id")?,
            red_invoice_id: row.get_typed("red_invoice_id")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }

    /// 是否终态（`POSTED` / `CANCELLED`，见 `state_machine::SALES_RETURN_LOCKED`）。
    pub fn is_locked(&self) -> bool {
        state_machine::Entity::SalesReturn.is_locked(&self.status)
    }
}

/// `biz_sales_return_item` 一行（`line_no` 是订单行号，从 `biz_sales_order_item` 带出）。
#[derive(Debug, Clone)]
pub struct ReturnItemRow {
    /// 主键。
    pub id: Uuid,
    /// 所属退货单。
    pub return_id: Uuid,
    /// 对应的原销售订单行（可空）。
    pub so_item_id: Option<Uuid>,
    /// 物料（弱引用 `biz_material.id`）。
    pub material_id: Uuid,
    /// 退货数量（> 0）。
    pub qty: Decimal,
    /// 单价（不含税）。
    pub price: Decimal,
    /// 行不含税金额。
    pub amount: Decimal,
    /// 税率百分数（0–100）。
    pub tax_rate: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
    /// 退回库位（`None` = 整仓）。
    pub location_id: Option<Uuid>,
    /// 序列号（只有序列号物料才非空）。
    pub serial_nos: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
    /// 订单行号（1 起；无原单 / 订单行已删时回落 0）。
    pub line_no: i32,
}

impl ReturnItemRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        let serials: String = row.get_typed("serial_nos")?;
        Ok(Self {
            id: row.get_typed("id")?,
            return_id: row.get_typed("return_id")?,
            so_item_id: row.get_typed("so_item_id")?,
            material_id: row.get_typed("material_id")?,
            qty: row.get_typed("qty")?,
            price: row.get_typed("price")?,
            amount: row.get_typed("amount")?,
            tax_rate: row.get_typed("tax_rate")?,
            tax_amount: row.get_typed("tax_amount")?,
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

/// 一行待落库的明细（金额由 [`line_amounts`] 算好，**不信请求体**）。
#[derive(Debug, Clone)]
pub struct NewReturnItem {
    /// 对应的原销售订单行（可空）。
    pub so_item_id: Option<Uuid>,
    /// 物料（有原单行时取自订单行，否则取请求体）。
    pub material_id: Uuid,
    /// 退货数量（> 0，且 ≤ 已发 − 已退）。
    pub qty: Decimal,
    /// 单价（不含税）。
    pub price: Decimal,
    /// 税率百分数。
    pub tax_rate: Decimal,
    /// 行不含税金额（= [`line_amounts`] 的第一个返回值）。
    pub amount: Decimal,
    /// 行税额（= [`line_amounts`] 的第二个返回值）。
    pub tax_amount: Decimal,
    /// 退回库位（`None` = 整仓）。
    pub location_id: Option<Uuid>,
    /// 序列号。
    pub serial_nos: Vec<String>,
    /// 行备注。
    pub remark: Option<String>,
}

impl NewReturnItem {
    /// 按数量 / 单价 / 税率算好金额后构造（金额只由这里算，调用方不许自己填）。
    pub fn new(
        so_item_id: Option<Uuid>,
        material_id: Uuid,
        qty: Decimal,
        price: Decimal,
        tax_rate: Decimal,
    ) -> Self {
        let (amount, tax_amount) = line_amounts(qty, price, tax_rate);
        Self {
            so_item_id,
            material_id,
            qty,
            price,
            tax_rate,
            amount,
            tax_amount,
            location_id: None,
            serial_nos: Vec::new(),
            remark: None,
        }
    }
}

/// 待落库的单头。
#[derive(Debug, Clone)]
pub struct NewReturn<'a> {
    /// 客户。
    pub customer_id: Uuid,
    /// 原销售订单（可空）。
    pub so_id: Option<Uuid>,
    /// 原发货单（可空）。
    pub shipment_id: Option<Uuid>,
    /// 退货日期。
    pub return_date: NaiveDate,
    /// 退货类型。
    pub return_type: &'a str,
    /// 退回入库仓库。
    pub warehouse_id: Uuid,
    /// 备注。
    pub remark: Option<&'a str>,
}

// ==================== 读 ====================

/// 按主键取退货单（软删行不返回）。
pub async fn find_return(id: Uuid) -> DbResult<Option<ReturnRow>> {
    find_by_id(TABLE, RETURN_COLUMNS, id, true, ReturnRow::from_row).await
}

/// 取一张退货单的明细（订单行号升序；无原单的行排最后）。
pub async fn items_of(return_id: Uuid) -> DbResult<Vec<ReturnItemRow>> {
    let sql =
        format!("{ITEM_SELECT} WHERE ri.return_id = ? ORDER BY COALESCE(oi.line_no, 0), ri.id");
    let rows = db()
        .find_with_params(&sql, vec![Value::from(return_id)])
        .await?;
    rows.iter().map(ReturnItemRow::from_row).collect()
}

/// 分页：`return_no`（模糊）/ `status`（精确）/ `so_id` / `customer_id` / `shipment_id`（精确）。
pub async fn return_page(
    return_no: Option<&str>,
    status: Option<&str>,
    so_id: Option<Uuid>,
    customer_id: Option<Uuid>,
    shipment_id: Option<Uuid>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<ReturnRow>, i64)> {
    let mut sql = format!("SELECT {RETURN_COLUMNS} FROM {TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = crate::utils::like_pattern(return_no) {
        sql.push_str(" AND return_no LIKE ?");
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
    if let Some(shipment_id) = shipment_id {
        sql.push_str(" AND shipment_id = ?");
        params.push(Value::from(shipment_id));
    }
    page_of(
        RETURN_TARGET,
        sql,
        params,
        RETURN_ORDER,
        page,
        page_size,
        ReturnRow::from_row,
    )
    .await
}

/// 订单状态（建单闸用；订单不存在或已删 → `None`）。
pub async fn so_status_of(ex: &impl SqlExecutor, so_id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 销售订单，ERP 单据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT customer_id, status FROM biz_sales_order \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(so_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("status")).transpose()
}

/// 订单的客户（建单时校验并带出；订单不存在 → `None`）。
pub async fn so_customer_of(ex: &impl SqlExecutor, so_id: Uuid) -> DbResult<Option<Uuid>> {
    // SCOPE-EXEMPT: 销售订单，ERP 单据，无归属列。
    let rows = ex
        .query_sql(
            "SELECT customer_id FROM biz_sales_order \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(so_id)],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("customer_id"))
        .transpose()
}

/// 发货单的单号（详情展示用；不存在 / 已软删 → `None`）。
pub async fn shipment_no_of(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: 发货单是 ERP 单据表，无归属列。
    let rows = ex
        .query_sql(
            "SELECT ship_no FROM biz_shipment WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("ship_no")).transpose()
}

/// 批量取「发货单 id → 单号」，分页列表一次往返补齐（避免 N+1）。
///
/// 软删 / 不存在的发货单不出现在结果里（调用方按缺失处理 = 单号留 `None`）。
pub async fn shipment_no_labels(
    ex: &impl SqlExecutor,
    ids: &[Uuid],
) -> DbResult<Vec<(Uuid, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let placeholders = vec!["?::uuid"; unique.len()].join(", ");
    let sql = format!(
        "SELECT id, ship_no FROM biz_shipment \
          WHERE id IN ({placeholders}) AND deleted_at IS NULL"
    );
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = ex.query_sql(sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((row.get_typed("id")?, row.get_typed("ship_no")?));
    }
    Ok(out)
}

// ==================== 写 ====================

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
///
/// ⚠ **超退守卫不在本函数里**：调用方必须先按
/// [`lock_shipment`] / [`lock_so_item`] → [`ensure_qty_within_returnable`] → `create_return`
/// 的顺序执行，否则两个并发建单会各看到同一份可退量。
pub async fn create_return(
    ex: &impl SqlExecutor,
    new: &NewReturn<'_>,
    items: &[NewReturnItem],
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    let return_no = next_return_no(ex, now).await?;
    let id = Uuid::now_v7();
    let (total_amount, total_tax, total_with_tax) = totals_of(items);
    let operator_text = Value::Text(
        operator
            .id
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, return_no, customer_id, so_id, shipment_id, return_date, return_type, \
                 warehouse_id, status, total_amount, total_tax, total_with_tax, remark, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, \
                 ?::text::date, ?, ?, ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                 NULLIF(?::text,''), ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(return_no.clone()),
            Value::from(&new.customer_id),
            Value::Text(new.so_id.map(|value| value.to_string()).unwrap_or_default()),
            Value::Text(
                new.shipment_id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::Text(new.return_date.to_string()),
            Value::Text(new.return_type.to_string()),
            Value::from(&new.warehouse_id),
            Value::Text(STATUS_DRAFT.to_string()),
            Value::Text(total_amount.to_string()),
            Value::Text(total_tax.to_string()),
            Value::Text(total_with_tax.to_string()),
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
        state_machine::ENTITY_SALES_RETURN,
        id,
        &return_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok((id, return_no))
}

/// 插明细（调用方保证单头已落、数量已过超退守卫）。
///
/// `serial_nos` 走 `string_to_array` + 一个 text 参数：驱动没有 `text[]` 编码器，
/// 直绑 `?::text[]` 会 `error serializing parameter`（`serial_model` 实测坑）。
async fn insert_items(
    ex: &impl SqlExecutor,
    return_id: Uuid,
    items: &[NewReturnItem],
) -> DbResult<()> {
    for item in items {
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (return_id, so_item_id, material_id, qty, price, amount, tax_rate, tax_amount, \
                     location_id, serial_nos, remark) \
                 VALUES (?, NULLIF(?::text,'')::uuid, ?, ?::text::numeric, ?::text::numeric, \
                    ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                    NULLIF(?::text,'')::uuid, string_to_array(NULLIF(?::text,''), ','), \
                    NULLIF(?::text,''))"
            ),
            vec![
                Value::from(&return_id),
                Value::Text(
                    item.so_item_id
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(&item.material_id),
                Value::Text(item.qty.to_string()),
                Value::Text(item.price.to_string()),
                Value::Text(item.amount.to_string()),
                Value::Text(item.tax_rate.to_string()),
                Value::Text(item.tax_amount.to_string()),
                Value::Text(
                    item.location_id
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                ),
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
            "UPDATE {TABLE} \
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

// ==================== 单测（纯逻辑） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 定点字面量（浮点会丢精度，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    /// ① 状态域 = 6 值，且与库层 `ck_biz_sreturn_status` 逐字一致。
    #[test]
    fn status_domain_matches_arch() {
        assert_eq!(
            STATUSES,
            [
                "DRAFT",
                "SUBMITTED",
                "APPROVED",
                "PENDING_POST",
                "POSTED",
                "CANCELLED"
            ]
        );
        for status in STATUSES {
            assert!(is_valid_status(status));
            assert_ne!(status_label(status), "未知", "{status} 缺中文标签");
        }
        assert!(!is_valid_status("draft"), "大小写敏感");
        assert!(!is_valid_status("SHIPPED"), "ARCH 旧稿的值不认");
    }

    /// ② 退货类型 = 3 值（与 `ck_biz_sreturn_type` 逐字一致）。
    #[test]
    fn return_type_domain_matches_arch() {
        assert_eq!(RETURN_TYPES, ["QUALITY", "WRONG_ITEM", "OTHER"]);
        for value in RETURN_TYPES {
            assert!(is_valid_return_type(value));
            assert_ne!(return_type_label(value), "未知");
        }
        assert!(!is_valid_return_type("quality"));
        assert!(!is_valid_return_type(""));
    }

    /// ③ 边表：4 条主链 + 3 条作废 = 7 条，且域值全部落在 6 值里。
    #[test]
    fn legal_edges_are_exactly_the_seven() {
        assert_eq!(LEGAL_EDGES.len(), 7);
        assert_eq!(
            LEGAL_EDGES.to_vec(),
            vec![
                ("DRAFT", "SUBMITTED"),
                ("SUBMITTED", "APPROVED"),
                ("APPROVED", "PENDING_POST"),
                ("PENDING_POST", "POSTED"),
                ("DRAFT", "CANCELLED"),
                ("SUBMITTED", "CANCELLED"),
                ("APPROVED", "CANCELLED"),
            ]
        );
        for (from, to) in LEGAL_EDGES {
            assert!(is_valid_status(from), "{from} 不在状态域");
            assert!(is_valid_status(to), "{to} 不在状态域");
            assert_ne!(from, to, "自环不是状态变更");
        }
        assert_eq!(LEGAL_EDGES, state_machine::SALES_RETURN_EDGES);
    }

    /// ④ 主链单向可走、跳步不行、终态出不去。
    #[test]
    fn transition_rules() {
        assert!(can_transition("DRAFT", "SUBMITTED"));
        assert!(can_transition("SUBMITTED", "APPROVED"));
        assert!(can_transition("APPROVED", "PENDING_POST"));
        assert!(can_transition("PENDING_POST", "POSTED"));
        assert!(can_transition("DRAFT", "CANCELLED"));
        assert!(can_transition("SUBMITTED", "CANCELLED"));
        assert!(can_transition("APPROVED", "CANCELLED"));
        // 跳步 / 回退 / 终态出去
        assert!(!can_transition("DRAFT", "APPROVED"));
        assert!(!can_transition("DRAFT", "PENDING_POST"));
        assert!(!can_transition("DRAFT", "POSTED"));
        assert!(
            !can_transition("SUBMITTED", "DRAFT"),
            "退货单没有回草稿的边"
        );
        assert!(
            !can_transition("PENDING_POST", "CANCELLED"),
            "待过账必须走完过账或先撤回"
        );
        assert!(!can_transition("POSTED", "CANCELLED"));
        assert!(!can_transition("CANCELLED", "DRAFT"));
        assert_eq!(
            allowed_targets("DRAFT"),
            vec!["SUBMITTED", "CANCELLED"],
            "草稿只有两个去处"
        );
        assert!(allowed_targets("POSTED").is_empty(), "POSTED 是终态");
        assert!(allowed_targets("CANCELLED").is_empty(), "CANCELLED 是终态");
    }

    /// ⑤ 超退公式：`qty <= shipped - returned`，且余量下限 0。
    #[test]
    fn qty_guard_formula() {
        // 已发 10 / 已退 3 ⇒ 可退 7
        assert!(qty_within_returnable(dec("7"), dec("10"), dec("3")));
        assert!(qty_within_returnable(dec("0.0001"), dec("10"), dec("3")));
        assert!(!qty_within_returnable(dec("7.0001"), dec("10"), dec("3")));
        // 超退被拒
        assert!(!qty_within_returnable(dec("11"), dec("10"), dec("0")));
        // 数量必须 > 0
        assert!(!qty_within_returnable(
            Decimal::ZERO,
            dec("10"),
            Decimal::ZERO
        ));
        assert!(!qty_within_returnable(dec("-1"), dec("10"), Decimal::ZERO));
        // 已退超过已发（数据异常也绝不放过）
        assert_eq!(remaining_returnable(dec("2"), dec("5")), Decimal::ZERO);
        assert!(!qty_within_returnable(dec("0.5"), dec("2"), dec("5")));
    }

    /// ⑥ 金额：与销售订单同源（四舍五入，不是银行家舍入），合计精确恒等。
    #[test]
    fn totals_are_exact() {
        let (amount, tax) = line_amounts(dec("3"), dec("17.655"), dec("13"));
        assert_eq!(amount, dec("52.9650"));
        assert_eq!(tax, dec("6.8855"));

        let mut first = NewReturnItem::new(None, Uuid::nil(), dec("2"), dec("10"), dec("13"));
        first.remark = Some("第一行".to_string());
        let second = NewReturnItem::new(None, Uuid::nil(), dec("1.5"), dec("0.3333"), dec("0"));
        let (total_amount, total_tax, total_with_tax) = totals_of(&[first, second]);
        // 2×10=20.0000；1.5×0.3333=0.49995 → 0.5000
        assert_eq!(total_amount, dec("20.5000"));
        assert_eq!(total_tax, dec("2.6000"));
        assert_eq!(total_with_tax, total_amount + total_tax);
        assert_eq!(
            totals_of(&[]),
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
        );
    }

    /// ⑦ 单号格式：拼 / 解析 / 非法输入一律 None（不静默纠正）。
    #[test]
    fn return_no_format() {
        assert_eq!(format_return_no("20261107", 1), "SR-20261107-000001");
        assert_eq!(format_return_no("20261107", 1234567), "SR-20261107-1234567");
        assert_eq!(
            parse_return_no("SR-20261107-000001"),
            Some(("20261107".to_string(), 1))
        );
        for bad in [
            "",
            "SR",
            "SR-20261107",
            "SR-2026110-000001",
            "SR-20261107-0",
            "SH-20261107-000001",
            "sr-20261107-000001",
        ] {
            assert_eq!(parse_return_no(bad), None, "非法单号 {bad} 不该解析成功");
            assert!(!is_valid_return_no(bad));
        }
    }

    /// ⑧ 建单行：金额由服务端算（请求体只能提数量 / 单价 / 税率）。
    #[test]
    fn new_item_computes_amounts() {
        let item = NewReturnItem::new(None, Uuid::nil(), dec("1.5"), dec("100"), dec("13"));
        assert_eq!(item.amount, dec("150.0000"));
        assert_eq!(item.tax_amount, dec("19.5000"));
        assert_eq!(item.qty, dec("1.5"));
        assert!(item.serial_nos.is_empty());
    }

    /// ⑨ 序列号逗号串拆分（与发货单同一口径）。
    #[test]
    fn serials_split() {
        assert_eq!(split_serials(""), Vec::<String>::new());
        assert_eq!(split_serials(" , "), Vec::<String>::new());
        assert_eq!(
            split_serials("SN1, SN2 ,SN3"),
            vec!["SN1".to_string(), "SN2".to_string(), "SN3".to_string()]
        );
    }

    /// ⑩ 分页口径复用底座（0 → 默认 20，上限 200）。
    #[test]
    fn page_normalization() {
        assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(2, 500), (2, MAX_PAGE_SIZE));
    }
}

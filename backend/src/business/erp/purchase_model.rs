//! 采购订单数据层 + 两种金额口径 + 4 边状态机（批次 D · P2-A2）。
//!
//! 依据：`backend/migrations/business/V0145__purchase_order_amounts_and_stock_in.up.sql`（P2-A1）
//! 与 `docs/ERP-批次1-产品需求PRD.md` §3.1（金额口径，296-380 行）。
//! **表结构以 `\d` 实测为准**（2026-10-31 开工时实测 `cloudpivot_dev`，已到 V0145）：
//!
//! ```text
//! biz_purchase_order        id, order_no, supplier_id, order_date, expect_date, warehouse_id,
//!                           status, currency_code, total_qty, total_amount, total_tax,
//!                           total_with_tax, credit_days, remark, price_mode(EXCL/INCL),
//!                           submitted_at/by, approved_at/by, closed_at/by,
//!                           created_at, updated_at, create_by, update_by, deleted_at
//! biz_purchase_order_item   id, order_id, line_no, material_id, unit_id, qty, price,
//!                           price_incl, tax_rate, amount, tax_amount, with_tax_amount,
//!                           remark, created_at, updated_at
//!                           （P2-3 / V0217 起 **没有 `received_qty`**：已收数量读时现算，
//!                            口径见 [`POSTED_RECEIVED_SQL`]）
//! ```
//!
//! # 五条「卡面没写、但库里有」的硬事实（照卡面写必然挂）
//!
//! 1. **`ck_biz_po_tax_sum` / `ck_biz_po_item_tax` 是精确恒等**（`total_with_tax = total_amount + total_tax`），
//!    所以口径 B 的税额**只能减法倒挤**，否则写入直接 `CHECK` 失败（不是 500，是 400 库约束）。
//! 2. **`ck_biz_po_item_qty CHECK (qty > 0)`**：卡面 §四 ③ 写的「草稿允许 0、提交时拦截」**做不到** ——
//!    0 数量的明细行根本插不进库。见 [`crate::business::erp::purchase_dto`] 的 `qty` 校验说明。
//! 3. **`warehouse_id` 是 `NOT NULL`**：卡面建单 body 里没有这个字段，但库要求必填 → 本实现要求必传。
//! 4. **没有 `cancelled_at` / `cancel_reason` 列**：作废只能落 `closed_at` / `closed_by`，
//!    取消原因拼到 `remark` 末尾（` | 取消原因：xxx`），不新建列（不动 V0137-V0145）。
//! 5. **`currency_code` 是 `CHAR(3)`**：SELECT 一律 `currency_code::text`（沿用 `model` 的口径）。
//!
//! # 金额口径（PRD §3.1.3 / §3.1.5）
//!
//! ```text
//! 口径 A（EXCL，默认）：amount = round(qty×price_excl, 2)
//!                      tax    = round(amount × rate/100, 2)
//!                      with_tax = amount + tax              ← 加法
//! 口径 B（INCL）：      with_tax = round(qty×price_incl, 2)
//!                      amount = round(with_tax / (1+rate/100), 2)
//!                      tax    = with_tax - amount           ← **减法倒挤**
//! 单据汇总：Σ 各行（**逐行先舍入到 2 位再相加**，禁止先加总再舍入）
//! ```
//!
//! 舍入策略固定为**四舍五入（`MidpointAwayFromZero`）**，与 PG `numeric` 的 `ROUND` 一致：
//! `rust_decimal` 的默认 `round_dp` 是「四舍六入五成双」，`.005` 这类边界会和库里的
//! `ROUND()` 差 1 分钱（W3c 踩过），所以这里显式指定策略。
//!
//! # 状态机（卡面 §三，4 条合法边）
//!
//! ```text
//! DRAFT      → SUBMITTED（提交，写 submitted_at/by；明细数量必须 > 0）
//! DRAFT      → CANCELLED
//! SUBMITTED  → CLOSED   （关闭，写 closed_at/by；可能部分收货后关）
//! SUBMITTED  → CANCELLED（需 reason 非空）
//! 其余一切   → 409
//! ```
//!
//! `APPROVED` / `PARTIAL_RECEIVED` / `RECEIVED` 是 P2-B1（收货）与审批接入的地盘：
//! 本卡**不产生**这三个状态，但它们出现在 `ck_biz_po_status` 白名单里，所以状态机按
//! 「只有 4 条合法边」判，落到其它组合一律 409（`RECEIVED` 后再关单由 P2-B1 处理）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: 采购订单是 ERP 单据表（V0031 建表，无 dept_id / 归属人列），
//! 数据范围（SELF / DEPT_*）表达不出来；可见性由 business:erp:purchase-order:* 权限码控制。
//! 列表分页沿用 model::purchase_order_page（内部走 scope::paginate_unscoped）。
//! SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列，不需要数据范围。
//! ```
//!
//! # 事务边界
//!
//! 写函数一律吃 `&impl SqlExecutor`（`Db` / `Transaction` 都实现），
//! 由 [`crate::business::erp::purchase_handler`] 开事务：**取号 + 单头 + 明细**必须原子，
//! 改单是**单头 UPDATE + 明细「删旧插新」**同一事务。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::RoundingStrategy;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model::{
    PurchaseOrderItemRow, PurchaseOrderRow, purchase_order_find_by_id, purchase_order_items,
};
use crate::business::erp::serial_model::period_key_of;
use crate::business::shared::state_machine;

// LNK-02：采购单状态变更的跨单据联动接线（`purchase_linkage.rs`）。
//
// 用 `#[path]` 挂在这里而不是写进 `erp/mod.rs`：多 worker 并行时 `mod.rs` 是冲突热点，
// 而联动 hook 只需要「状态写入点」自己能看见（`stockin_purchase.rs` 走
// `purchase_model::LinkageOutbox` 这条重导出，不必知道模块长在哪）。
#[path = "purchase_linkage.rs"]
pub(crate) mod purchase_linkage;

pub(crate) use self::purchase_linkage::LinkageOutbox;

// ==================== 表名 / 常量 ====================

/// 单据头表。
pub const ORDER_TABLE: &str = "biz_purchase_order";

/// 明细表（无 `deleted_at`，主单软删后明细留在库里）。
pub const ITEM_TABLE: &str = "biz_purchase_order_item";

/// 口径：不含税录入（默认）。
pub const PRICE_MODE_EXCL: &str = "EXCL";

/// 口径：含税录入。
pub const PRICE_MODE_INCL: &str = "INCL";

/// 允许的两种口径（顺序即前端徽标顺序）。
pub const PRICE_MODES: [&str; 2] = [PRICE_MODE_EXCL, PRICE_MODE_INCL];

/// 状态：草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 状态：已提交。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 状态：审批通过（P-FLOW-3A）。此后才允许收货（`purchase_receivable` 同时收 SUBMITTED / APPROVED）。
pub const STATUS_APPROVED: &str = "APPROVED";
/// 状态：部分收货（P2-B1 收货推进写入）。
pub const STATUS_PARTIAL_RECEIVED: &str = "PARTIAL_RECEIVED";
/// 状态：已收货（明细的「已收数量」全部收满；P2-3 起该数量是派生值）。
pub const STATUS_RECEIVED: &str = "RECEIVED";
/// 状态：已关闭。
pub const STATUS_CLOSED: &str = "CLOSED";
/// 状态：已取消。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 合法边表（唯一真相在 `shared/state_machine`，P-FLOW-3A 扩到 13 条）。
pub const LEGAL_EDGES: [(&str, &str); 15] = state_machine::PURCHASE_ORDER_EDGES;

/// 状态中文标签（错误文案与前端徽标共用）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SUBMITTED => "已提交",
        STATUS_CLOSED => "已关闭",
        STATUS_CANCELLED => "已取消",
        "APPROVED" => "已审批",
        "PARTIAL_RECEIVED" => "部分收货",
        "RECEIVED" => "已收货",
        _ => "未知状态",
    }
}

/// 某状态能去的下一个状态（空 = 终态 / 本卡不接的商业态）。
pub fn next_statuses(from: &str) -> Vec<&'static str> {
    LEGAL_EDGES
        .iter()
        .filter(|(f, _)| *f == from)
        .map(|(_, t)| *t)
        .collect()
}

/// 状态机判定：只有 [`LEGAL_EDGES`] 里的 4 条边放行。
pub fn can_transition(from: &str, to: &str) -> bool {
    LEGAL_EDGES.iter().any(|(f, t)| *f == from && *t == to)
}

/// 非法流转的 409 文案（点名「当前态的可选去向」）。
pub fn transition_rejected_message(from: &str, to: &str) -> String {
    let allowed = next_statuses(from);
    if allowed.is_empty() {
        return format!(
            "采购订单当前是「{}」（{from}），没有可流转的下一个状态：\
             它已是终态，或该状态由收货 / 审批流程维护（P2-B1）",
            status_label(from)
        );
    }
    format!(
        "采购订单当前是「{}」（{from}），不能直接流转到「{}」（{to}）：\
         允许的去向是 {}",
        status_label(from),
        status_label(to),
        allowed
            .iter()
            .map(|status| format!("「{}」（{}）", status_label(status), status))
            .collect::<Vec<_>>()
            .join(" / ")
    )
}

// ==================== 口径：金额计算（纯函数，好测） ====================

/// 金额一律**四舍五入**到 2 位（与 PG `ROUND(numeric, 2)` 同口径）。
///
/// 不用 `round_dp` 的默认策略：`rust_decimal` 默认是 `MidpointNearestEven`（银行家舍入），
/// `.005` 边界会比库里的 `ROUND()` 少 1 分钱（W3c 在成本转移上实测过）。
pub fn money(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// 单价一律 6 位（PRD §3.1.2）。
pub fn unit_price(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(6, RoundingStrategy::MidpointAwayFromZero)
}

/// 一行明细的三种金额（**一行只算一次**，单据汇总直接加这三个值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineAmounts {
    /// 不含税金额（价款，`amount` 列）。
    pub amount: Decimal,
    /// 税额（`tax_amount` 列）。
    pub tax_amount: Decimal,
    /// 价税合计（`with_tax_amount` 列），**恒等于** `amount + tax_amount`。
    pub with_tax_amount: Decimal,
}

/// 行级金额计算（PRD §3.1.3）。
///
/// * `PRICE_MODE_INCL`（含税录入）：先定价税合计 → 倒算价款 → **税额减法倒挤**；
/// * 其它（含 `EXCL`，默认口径）：先定价款 → 按价款计税 → **加法**得价税合计。
///
/// `tax_rate` 是百分数（13 表示 13%），与 `tax_rate NUMERIC(7,4)` 列同口径。
pub fn line_amounts(
    price_mode: &str,
    qty: Decimal,
    price: Decimal,
    tax_rate: Decimal,
) -> LineAmounts {
    if price_mode == PRICE_MODE_INCL {
        let with_tax_amount = money(qty * price);
        let divisor = Decimal::ONE + tax_rate / Decimal::ONE_HUNDRED;
        let amount = if divisor.is_zero() {
            with_tax_amount
        } else {
            money(with_tax_amount / divisor)
        };
        // ⚠️ 减法倒挤：再乘一次税率会出现 amount + tax ≠ with_tax 的 1 分钱差额，
        // 而库里的 ck_biz_po_item_tax 是**精确恒等**，写不进去。
        let tax_amount = with_tax_amount - amount;
        LineAmounts {
            amount,
            tax_amount,
            with_tax_amount,
        }
    } else {
        let amount = money(qty * price);
        let tax_amount = money(amount * tax_rate / Decimal::ONE_HUNDRED);
        LineAmounts {
            amount,
            tax_amount,
            with_tax_amount: amount + tax_amount,
        }
    }
}

/// 单据汇总：**逐行舍入后的值再相加**（PRD §3.1.5，禁止先加总再舍入）。
pub fn sum_lines(lines: &[LineAmounts]) -> (Decimal, Decimal, Decimal) {
    let mut amount = Decimal::ZERO;
    let mut tax = Decimal::ZERO;
    let mut with_tax = Decimal::ZERO;
    for line in lines {
        amount += line.amount;
        tax += line.tax_amount;
        with_tax += line.with_tax_amount;
    }
    (amount, tax, with_tax)
}

// ==================== 数据层结构 ====================

/// 建单 / 改单的单头字段（`None` = 落 NULL）。
#[derive(Debug, Clone)]
pub struct NewPurchaseOrder<'a> {
    /// 供应商（`CONTRACT` 由合同乙方派生，`SPOT` 用户选择）。
    pub supplier_id: Uuid,
    /// 收货仓库（列 `NOT NULL`）。
    pub warehouse_id: Uuid,
    /// 采购类型（`CONTRACT` / `SPOT`，V0198）。
    pub purchase_type: &'a str,
    /// 合同（弱引用；`SPOT` 必为 `None`）。
    pub contract_id: Option<Uuid>,
    /// 合同号快照（`CONTRACT` 必填，`SPOT` 必为 `None`）。
    pub contract_no_snapshot: Option<&'a str>,
    /// 供应商名称快照（业务发生那一刻；拿不到主档名时可为 `None`）。
    pub supplier_name_snapshot: Option<&'a str>,
    /// 收货仓库名称快照（P2-9 / DC-23：建单 / 改单那一刻的仓库名，`None` = 查不到）。
    ///
    /// 由 handler 从 `warehouse_ref` 的返回值直接带上（那次查询本来就要做存在性校验，
    /// 不多一次往返）；仓库被软删 → `None`，落库为 NULL（不阻塞建单，历史单据仍可用）。
    pub warehouse_name_snapshot: Option<&'a str>,
    /// 所属项目（弱引用 `biz_project.id`，P2-8；`None` = 不归集项目）。
    /// 事务前已由 handler 校验「存在且未软删」，库侧无外键。
    pub project_id: Option<Uuid>,
    /// 下单日期。
    pub order_date: NaiveDate,
    /// 期望到货日。
    pub expect_date: Option<NaiveDate>,
    /// 口径（`EXCL` / `INCL`）。
    pub price_mode: &'a str,
    /// 备注。
    pub remark: Option<&'a str>,
    /// 账期天数。
    pub credit_days: i32,
}

/// 一行待落库的明细（金额已算好，`unit_id` 由物料带出）。
#[derive(Debug, Clone)]
pub struct NewItem {
    /// 物料。
    pub material_id: Uuid,
    /// 计量单位（物料的基础单位）。
    pub unit_id: Uuid,
    /// 数量（4 位标度）。
    pub qty: Decimal,
    /// 不含税单价（6 位）。
    pub price: Decimal,
    /// 含税单价（6 位，快照）。
    pub price_incl: Decimal,
    /// 税率（百分数）。
    pub tax_rate: Decimal,
    /// 行金额（已按口径算好）。
    pub amounts: LineAmounts,
    /// 行备注。
    pub remark: Option<String>,
}

/// 单据汇总值（与 `total_*` 三列一一对应）。
#[derive(Debug, Clone, Copy)]
pub struct Totals {
    /// 合计数量。
    pub total_qty: Decimal,
    /// 不含税合计。
    pub amount: Decimal,
    /// 税额合计。
    pub tax: Decimal,
    /// 价税合计。
    pub with_tax: Decimal,
}

impl Totals {
    /// 由明细行算汇总（数量先按 4 位定标再相加）。
    pub fn of(items: &[NewItem]) -> Self {
        let lines: Vec<LineAmounts> = items.iter().map(|item| item.amounts).collect();
        let (amount, tax, with_tax) = sum_lines(&lines);
        let total_qty = items
            .iter()
            .fold(Decimal::ZERO, |acc, item| acc + item.qty.round_dp(4));
        Self {
            total_qty,
            amount,
            tax,
            with_tax,
        }
    }
}

/// 物料引用（建单要 `unit_id`，详情要编码/名称）。
#[derive(Debug, Clone)]
pub struct MaterialRef {
    /// 物料编码。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 基础计量单位。
    pub unit_id: Uuid,
}

// ==================== 取号（`PO-yyyyMMdd-000001`） ====================

/// 单号前缀。
pub const NO_PREFIX: &str = "PO";

/// 流水号补零宽度（超过 6 位不截断，宁可长一位也不撞号）。
pub const NO_SEQ_WIDTH: usize = 6;

/// `biz_seq.seq_type`（V0031 种的 `PO` 行）。
pub const SEQ_TYPE: &str = "PO";

/// 组装单号（`PO-{周期键}-{6 位流水}`）。
pub fn format_order_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// `biz_seq(seq_type='PO')` 缺失时幂等补一行（正常库有 V0031/V0137 的种子）。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'PO', '采购订单号', 'PO', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '采购订单单号：PO-yyyyMMdd-000001，按天（CST）滚动' \
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

/// 推进 `biz_seq(seq_type='PO')` 一个号（三分支：同日累加 / 新日重置 / 时钟回拨不重置）。
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
              WHERE seq_type = 'PO' AND deleted_at IS NULL \
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

/// 取一个采购订单号（周期键按北京时间）。
pub async fn next_order_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_order_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='PO') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 写：建单 / 改单 / 流转 / 删 ====================

/// 插入明细（建单 / 改单共用）。
async fn insert_items(
    ex: &impl SqlExecutor,
    order_id: Uuid,
    items: &[NewItem],
    operator: Option<Uuid>,
) -> DbResult<()> {
    for (index, item) in items.iter().enumerate() {
        let line_no = i32::try_from(index + 1).unwrap_or(i32::MAX);
        ex.exec_sql(
            format!(
                "INSERT INTO {ITEM_TABLE} \
                    (id, order_id, line_no, material_id, unit_id, qty, price, price_incl, \
                     tax_rate, amount, tax_amount, with_tax_amount, remark) \
                 VALUES (?, ?::uuid, ?, ?::uuid, ?::uuid, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                     NULLIF(?::text,'')::numeric, NULLIF(?::text,''))"
            ),
            vec![
                Value::from(Uuid::now_v7()),
                Value::from(&order_id),
                Value::from(line_no),
                Value::from(&item.material_id),
                Value::from(&item.unit_id),
                Value::Text(item.qty.round_dp(4).to_string()),
                Value::Text(item.price.to_string()),
                Value::Text(item.price_incl.to_string()),
                Value::Text(item.tax_rate.to_string()),
                Value::Text(item.amounts.amount.to_string()),
                Value::Text(item.amounts.tax_amount.to_string()),
                Value::Text(item.amounts.with_tax_amount.to_string()),
                Value::Text(item.remark.clone().unwrap_or_default()),
            ],
        )
        .await?;
    }
    let _ = operator;
    Ok(())
}

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
pub async fn create(
    ex: &impl SqlExecutor,
    new: &NewPurchaseOrder<'_>,
    items: &[NewItem],
    totals: &Totals,
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<Uuid> {
    let order_no = next_order_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.id.map(|id| id.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {ORDER_TABLE} \
                (id, order_no, supplier_id, order_date, expect_date, warehouse_id, status, \
                 price_mode, total_qty, total_amount, total_tax, total_with_tax, credit_days, \
                 remark, created_at, updated_at, create_by, update_by, \
                 purchase_type, contract_id, contract_no_snapshot, supplier_name_snapshot, \
                 warehouse_name_snapshot, project_id) \
             VALUES (?, ?, ?::uuid, NULLIF(?::text,'')::date, NULLIF(?::text,'')::date, \
                 ?::uuid, 'DRAFT', ?, NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, \
                 NULLIF(?::text,'')::numeric, NULLIF(?::text,'')::numeric, ?, NULLIF(?::text,''), \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid, \
                 ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), NULLIF(?::text,''), \
                 NULLIF(?::text,''), NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(order_no.clone()),
            Value::from(&new.supplier_id),
            Value::Text(new.order_date.to_string()),
            Value::Text(new.expect_date.map(|d| d.to_string()).unwrap_or_default()),
            Value::from(&new.warehouse_id),
            Value::Text(new.price_mode.to_string()),
            Value::Text(totals.total_qty.to_string()),
            Value::Text(totals.amount.to_string()),
            Value::Text(totals.tax.to_string()),
            Value::Text(totals.with_tax.to_string()),
            Value::from(new.credit_days),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
            Value::Text(new.purchase_type.to_string()),
            Value::Text(
                new.contract_id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            Value::Text(new.contract_no_snapshot.unwrap_or_default().to_string()),
            Value::Text(new.supplier_name_snapshot.unwrap_or_default().to_string()),
            Value::Text(new.warehouse_name_snapshot.unwrap_or_default().to_string()),
            Value::Text(
                new.project_id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
        ],
    )
    .await?;
    insert_items(ex, id, items, operator.id).await?;
    // P10 铁律 ③：建单留痕（`from_status = NULL → DRAFT`），与建单同一事务
    state_machine::log_created(
        ex,
        state_machine::ENTITY_PURCHASE_ORDER,
        id,
        &order_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok(id)
}

/// 改单（**仅 DRAFT 由调用方保证**）：单头整单覆盖 + 明细「删旧插新」。
///
/// 返回 `affected`（0 = 读状态与写状态之间被别人推走，调用方按 409 处理）。
pub async fn update(
    ex: &impl SqlExecutor,
    id: Uuid,
    new: &NewPurchaseOrder<'_>,
    items: &[NewItem],
    totals: &Totals,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {ORDER_TABLE} \
                    SET supplier_id = ?::uuid, order_date = NULLIF(?::text,'')::date, \
                        expect_date = NULLIF(?::text,'')::date, warehouse_id = ?::uuid, \
                        price_mode = ?, total_qty = NULLIF(?::text,'')::numeric, \
                        total_amount = NULLIF(?::text,'')::numeric, \
                        total_tax = NULLIF(?::text,'')::numeric, \
                        total_with_tax = NULLIF(?::text,'')::numeric, credit_days = ?, \
                        remark = NULLIF(?::text,''), updated_at = ?, update_by = NULLIF(?::text,'')::uuid, \
                        purchase_type = ?, contract_id = NULLIF(?::text,'')::uuid, \
                        contract_no_snapshot = NULLIF(?::text,''), \
                        supplier_name_snapshot = NULLIF(?::text,''), \
                        warehouse_name_snapshot = NULLIF(?::text,''), \
                        project_id = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
            ),
            vec![
                Value::from(&new.supplier_id),
                Value::Text(new.order_date.to_string()),
                Value::Text(new.expect_date.map(|d| d.to_string()).unwrap_or_default()),
                Value::from(&new.warehouse_id),
                Value::Text(new.price_mode.to_string()),
                Value::Text(totals.total_qty.to_string()),
                Value::Text(totals.amount.to_string()),
                Value::Text(totals.tax.to_string()),
                Value::Text(totals.with_tax.to_string()),
                Value::from(new.credit_days),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::from(now),
                operator_text.clone(),
                Value::Text(new.purchase_type.to_string()),
                Value::Text(
                    new.contract_id
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::Text(new.contract_no_snapshot.unwrap_or_default().to_string()),
                Value::Text(new.supplier_name_snapshot.unwrap_or_default().to_string()),
                Value::Text(new.warehouse_name_snapshot.unwrap_or_default().to_string()),
                Value::Text(
                    new.project_id
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(&id),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    // 明细没有 deleted_at，也不该留历史行：整单重建（行号从 1 连续）
    ex.exec_sql(
        format!("DELETE FROM {ITEM_TABLE} WHERE order_id = ?::uuid"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_items(ex, id, items, operator).await?;
    Ok(affected)
}

/// 状态流转（CAS：`WHERE id = ? AND status = from`），返回 `affected`。
///
/// 目标态接受 `SUBMITTED` / `APPROVED` / `DRAFT` / `CLOSED` / `CANCELLED`：
/// 表里没有 `cancelled_at` / `cancel_reason` 列，作废落 `closed_at` / `closed_by`，
/// 原因追加到 `remark`（`concat_ws(' | ', remark, '取消原因：xxx')`，不覆盖原备注）。
///
/// # LNK-02：状态真的变了就把待提交后的联动动作交给 `outbox`
///
/// `outbox` 由**调用方**在开事务之前建、`commit()` 之后 `flush()` —— 事务内的
/// `Phase::InTx` 动作（写库存 / 凭证一类）在这里执行（失败整笔回滚），
/// 通知 / 建任务这类 `Phase::AfterCommit` 动作只在提交之后跑（永不回滚业务）。
#[allow(clippy::too_many_arguments)]
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    reason: Option<&str>,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
    outbox: &mut LinkageOutbox,
) -> DbResult<u64> {
    apply_status_with_instance(ex, id, from, to, reason, operator, None, now, outbox).await
}

/// 同 [`apply_status`]，但能**同时**写 `approval_instance_id`（P-FLOW-3A 发起审批用）。
///
/// 拆两个入口的理由与收货单同款：「写不写实例 id」是两件事 —— 普通流转必须保持
/// `NULL`（不然驳回打回草稿之后还挂着旧实例，下一次提交的判断会被污染）。
#[allow(clippy::too_many_arguments)]
pub async fn apply_status_with_instance(
    ex: &impl SqlExecutor,
    id: Uuid,
    from: &str,
    to: &str,
    reason: Option<&str>,
    operator: Option<Uuid>,
    approval_instance_id: Option<Uuid>,
    now: DateTime<Utc>,
    outbox: &mut LinkageOutbox,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    let reason_text = reason.unwrap_or_default().to_string();
    let (sql, params) = match to {
        STATUS_SUBMITTED => (
            format!(
                "UPDATE {ORDER_TABLE} \
                    SET status = 'SUBMITTED', submitted_at = ?, submitted_by = NULLIF(?::text,'')::uuid, \
                        approval_instance_id = NULLIF(?::text,'')::uuid, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::Text(
                    approval_instance_id
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        // P-FLOW-3A：审批通过（`SUBMITTED → APPROVED`）。单据状态由**审批结论**决定，
        // 不是人工点的；`approval_instance_id` 保留作审计（详情显示「审批通过 FLW-xxx」）。
        STATUS_APPROVED => (
            format!(
                "UPDATE {ORDER_TABLE} \
                    SET status = 'APPROVED', approved_at = ?, approved_by = NULLIF(?::text,'')::uuid, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
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
        // P-FLOW-3A：审批驳回 → 打回草稿（清实例与提交痕迹，改完可重新提交）
        STATUS_DRAFT => (
            format!(
                "UPDATE {ORDER_TABLE} \
                    SET status = 'DRAFT', approval_instance_id = NULL, \
                        submitted_at = NULL, submitted_by = NULL, \
                        remark = CASE WHEN ? = '' THEN remark \
                                      ELSE concat_ws(' | ', NULLIF(remark, ''), '审批驳回：' || ?) \
                                 END, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::Text(reason_text.clone()),
                Value::Text(reason_text),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        STATUS_CLOSED => (
            format!(
                "UPDATE {ORDER_TABLE} \
                    SET status = 'CLOSED', closed_at = ?, closed_by = NULLIF(?::text,'')::uuid, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
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
                "UPDATE {ORDER_TABLE} \
                    SET status = 'CANCELLED', closed_at = ?, closed_by = NULLIF(?::text,'')::uuid, \
                        remark = CASE WHEN ? = '' THEN remark \
                                      ELSE concat_ws(' | ', NULLIF(remark, ''), '取消原因：' || ?) \
                                 END, \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(now),
                operator_text.clone(),
                Value::Text(reason_text.clone()),
                Value::Text(reason_text),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(from.to_string()),
            ],
        ),
        other => {
            return Err(DbError::InvalidValue(format!(
                "状态流转的目标 `{other}` 不在 3 个可写目标里（DRAFT 只由建单产生）"
            )));
        }
    };
    let affected = ex.exec_sql(sql, params).await?;
    // LNK-02：【CAS 成功才跑联动】状态没变（affected = 0）说明目标态 = 当前态，
    // 或者单据被别人先推走了 —— 此时推一条「已变更为 X」的通知是假的。
    if affected > 0 {
        purchase_linkage::run_in_tx(ex, id, to, operator, outbox).await?;
    }
    Ok(affected)
}

/// 软删（**DRAFT / SUBMITTED 由调用方保证**），返回 `affected`。
///
/// P-FLOW-3A：`SUBMITTED` 也允许删 —— 前提是调用方**先**在同一个事务里终止在跑的审批单
/// （`purchase_bridge::cancel_approval_of`）。否则删掉的单据会留下孤儿待办（P11 教训）。
pub async fn soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    ex.exec_sql(
        format!(
            "UPDATE {ORDER_TABLE} SET deleted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid, approval_instance_id = NULL \
              WHERE id = ?::uuid AND status IN ('{STATUS_DRAFT}', '{STATUS_SUBMITTED}') \
                AND deleted_at IS NULL"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 读：单头 / 明细 / 引用校验 ====================

/// 按主键取单头（未删除）。
pub async fn find_order(id: Uuid) -> DbResult<Option<PurchaseOrderRow>> {
    purchase_order_find_by_id(id).await
}

/// 取整单明细（行号升序）。
pub async fn items_of(order_id: Uuid) -> DbResult<Vec<PurchaseOrderItemRow>> {
    purchase_order_items(order_id).await
}

/// 供应商是否存在（返回名称）。
pub async fn supplier_ref(id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed("supplier_name")?)),
    }
}

/// 仓库是否存在（返回名称）。
pub async fn warehouse_ref(id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT warehouse_name FROM biz_warehouse WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed("warehouse_name")?)),
    }
}

/// 供应商名称（批量夹具回填用：一次查多行，`None` = 主档已删 / 不存在）。
///
/// 与 [`supplier_ref`] 的区别只是「一条 SQL 查一批」——列表页 20 行逐行查就是 20 次往返
/// （与 `fill_approval_no` 同一套理由）。返回 `id → 名称` 映射。
pub async fn supplier_names_of(ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let joined = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // SCOPE-EXEMPT: biz_supplier 是全局主数据（无 dept_id / 归属人列），按主键批量解析名称。
    let rows = db()
        .find_with_params(
            "SELECT id, supplier_name FROM biz_supplier \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL",
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let name: String = row.get_typed("supplier_name")?;
        map.insert(id, name);
    }
    Ok(map)
}

// ==================== P2-8：所属项目（弱引用 biz_project） ====================

/// 项目是否存在且未软删（返回项目名）。
///
/// 为什么不用外键：`biz_project` 是**软删**表，外键拦不住「已软删」，真正的校验必须
/// select 一次（同时能给中文文案：「project_id 项目不存在或已删除」）。库侧只留裸 uuid
/// 列，与 `supplier_id` / `warehouse_id` / `contract_id` 的口径一致。
///
/// `ARCHIVED`（已归档）**不拦**：归档项目仍可能补录当期采购（跨期入账），拒绝会让
/// 用户把单挂到错的项目上。是否允许新单挂归档项目由前端下拉控（只列 ACTIVE）。
///
/// SCOPE-EXEMPT: 本处 `biz_project` 只做**按主键的弱引用解析**（存在性 + 名称），
/// 不承担项目模块的任务可见性口径（那在 `business/project/model.rs`，走 scope::）。
/// ERP 单据的数据范围是有意保持为 0（ARCH-3 DC-11），若在这里套 `my_project_ids`，
/// 采购员将挂不上自己没参与的项目 —— 成本归集会直接断掉。
pub async fn project_ref(id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .find_with_params(
            "SELECT project_name FROM biz_project WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed("project_name")?)),
    }
}

/// 项目名称批量解析（列表页一页 20 行只查一次，`None` = 项目已软删 / 不存在）。
///
/// 与 [`project_ref`] 同一口径（**实时值，不是快照**）：项目改名后列表/详情要跟着变，
/// 所以本查询**不带** `deleted_at IS NULL` 之外的条件 —— 已软删项目查不到名字，
/// 前端显示「（已删除项目）」，不回落 uuid。
///
/// SCOPE-EXEMPT: 同 [`project_ref`]，按主键批量取名称，不引入项目可见性。
pub async fn project_names_of(ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let joined = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let rows = db()
        .find_with_params(
            "SELECT id, project_name FROM biz_project \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL",
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let name: String = row.get_typed("project_name")?;
        map.insert(id, name);
    }
    Ok(map)
}

/// 仓库名称（批量：一次查多行，`None` = 主档已删 / 不存在）。
///
/// # 为什么需要批量版（P2-9 / DC-23）
///
/// 列表页要显示「收货仓库」，而**存量单没有快照**（`warehouse_name_snapshot` 为 NULL）：
/// 这些行只能按 `warehouse_id` 现查名称。一页 20 行逐行查就是 20 次往返
/// （与 [`supplier_names_of`] / [`project_names_of`] 同一套理由）。
///
/// 新单（有快照）**不进这条查询**：列表口径是「快照优先」，改了名的仓库不该让
/// 历史单的列表列跟着变（详情才显示现值，见 `purchase_handler::fill_warehouse_names`）。
pub async fn warehouse_names_of(ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let joined = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // SCOPE-EXEMPT: biz_warehouse 是全局主数据（无 dept_id / 归属人列），按主键批量解析名称。
    let rows = db()
        .find_with_params(
            "SELECT id, warehouse_name FROM biz_warehouse \
              WHERE id = ANY(?::text::uuid[]) AND deleted_at IS NULL",
            vec![Value::Text(format!("{{{joined}}}"))],
        )
        .await?;
    let mut map = std::collections::HashMap::with_capacity(rows.len());
    for row in rows.iter() {
        let id: Uuid = row.get_typed("id")?;
        let name: String = row.get_typed("warehouse_name")?;
        map.insert(id, name);
    }
    Ok(map)
}

// ==================== A3：合同额度现算（**不做缓存列**） ====================

/// 某张合同**已下单**的含税累计额（V-21/V-23：单一数据源，必须现算）。
///
/// 口径（与 ARCH-2 V-23 逐字一致）：
/// - 只算 `contract_id = ?` 且 `status <> 'CANCELLED'` 且未软删的采购单；
/// - `exclude` 用于**改单**：把自己这一张排除掉，否则「改单时自己算两遍」会误报超限；
/// - 返回 `NUMERIC(18,4)` 的精确值（`Decimal`），不做浮点转换。
///
/// ⚠ 调用方必须在**事务内**先 `SELECT … FOR UPDATE` 锁住合同行（见
/// `contract_model::contract_lock_full`），否则两个并发请求会双双放行（V-23 的并发坑）。
pub async fn committed_amount(
    ex: &impl SqlExecutor,
    contract_id: Uuid,
    exclude: Option<Uuid>,
) -> DbResult<Decimal> {
    let mut sql = "SELECT COALESCE(SUM(total_with_tax), 0)::text AS total \
               FROM biz_purchase_order \
              WHERE contract_id = ?::uuid AND status <> 'CANCELLED' AND deleted_at IS NULL"
        .to_string();
    let mut params = vec![Value::from(&contract_id)];
    if let Some(exclude_id) = exclude {
        // 改单时把自己排除掉（否则自己会被算两遍）。条件按需拼接，**不用**
        // `NULLIF(?::text,'')` 那类写法 —— 空串参数在驱动里会被当成 NULL 序列化，实测报
        // `error serializing parameter 2`（A3 踩坑）。
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&exclude_id));
    }
    let row = ex.query_first_sql(sql, params).await?;
    let total: Option<String> = match row {
        None => None,
        Some(row) => Some(row.get_typed("total")?),
    };
    Ok(total
        .and_then(|text| text.parse::<Decimal>().ok())
        .unwrap_or(Decimal::ZERO))
}

/// 物料是否存在（返回编码 / 名称 / 基础单位）。
pub async fn material_ref(id: Uuid) -> DbResult<Option<MaterialRef>> {
    let rows = db()
        .find_with_params(
            "SELECT material_code, material_name, unit_id FROM biz_material \
              WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(MaterialRef {
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            unit_id: row.get_typed("unit_id")?,
        })),
    }
}

// ==================== ⑫ 待收明细（D4：采购订单 → 一键生成收货单） ====================

/// 「已过账收货量」聚合表达式（按**明细行**匹配，不按物料）。
///
/// # ★ 唯一真相（P2-3）
///
/// 采购明细的「已收数量」**只有这一个来源**：`biz_purchase_order_item` 上的
/// `received_qty` 列已被迁移 V0217 删除，读的地方（采购单详情 / 待收明细 /
/// 过账闸 / 判收满 / 联动动作）全部用本表达式现算 —— 写一个数、读一个数，
/// 不存在「列和现算打架」。历史教训：列由过账 +N 作废 −N 维护、
/// 待收明细另算一遍，并发/异常回滚/手工改库都会让两个数漂开，
/// 用户看到「已收完」的采购单还能建收货单。
///
/// 口径（卡面硬红线 4 / 5，一个条件都不能少）：
///
/// - `s.status = 'POSTED'`：**只算已过账**。草稿（`DRAFT`）还没进仓库，作废
///   （`CANCELLED` / `PENDING_CANCEL`）已经把账回滚了 —— 两者都不算「已收」；
/// - `s.deleted_at IS NULL`：软删的单不算；
/// - `si.source_item_id = i.id`：按采购**明细行**匹配。按 `material_id` 匹配是错的：
///   同一张采购单里同一物料可以有多行（不同单价 / 不同交期），按物料会被"合并吃掉"；
/// - `s.source_order_id = i.order_id`：兜住「明细行被挪到别的单」这类脏数据。
///
/// 调用方须给 `biz_purchase_order_item` 起别名 `i`（表达式里写死了 `i.order_id` / `i.id`）。
pub const POSTED_RECEIVED_SQL: &str = "COALESCE((\
     SELECT SUM(si.qty) FROM biz_stock_in s \
     JOIN biz_stock_in_item si ON si.stock_in_id = s.id \
     WHERE s.source_order_id = i.order_id \
       AND si.source_item_id = i.id \
       AND s.status = 'POSTED' \
       AND s.deleted_at IS NULL), 0)";

/// 采购单的一条「待收明细」（收货页带出用）。
#[derive(Debug, Clone)]
pub struct PendingReceiveLine {
    /// 采购明细行 id（落收货明细的 `source_item_id`）。
    pub item_id: Uuid,
    /// 行号（1 起）。
    pub line_no: i32,
    /// 物料。
    pub material_id: Uuid,
    /// 物料编码（物料被删也给出空串，不 500）。
    pub material_code: String,
    /// 物料名称。
    pub material_name: String,
    /// 计量单位。
    pub unit_id: Uuid,
    /// 单位名称。
    pub unit_name: String,
    /// 订单数量。
    pub ordered_qty: Decimal,
    /// 已收数量（**只算已过账收货单**，见 [`POSTED_RECEIVED_SQL`]）。
    ///
    /// P2-3 起这是唯一口径：列 `biz_purchase_order_item.received_qty` 已删除，
    /// 不再有「冗余列 vs 现算」两套数（`ledger_received_qty` 字段随之退场）。
    pub received_qty: Decimal,
    /// 不含税单价。
    pub price_excl: Decimal,
    /// 含税单价快照。
    pub price_incl: Decimal,
    /// 税率（百分数 0–100）。
    pub tax_rate: Decimal,
}

impl PendingReceiveLine {
    /// 待收数量 = 订单数量 − 已收数量（负数按 0：脏数据不许让前端显示负数）。
    pub fn pending_qty(&self) -> Decimal {
        let pending = self.ordered_qty - self.received_qty;
        if pending < Decimal::ZERO {
            Decimal::ZERO
        } else {
            pending
        }
    }

    /// 这一行是否还收得动（`pending_qty > 0`）。
    pub fn has_pending(&self) -> bool {
        self.pending_qty() > Decimal::ZERO
    }

    /// 收货成本价：采购明细没有单独的成本列，一律用不含税单价（卡面 §① 的 `cost_price`）。
    pub fn cost_price(&self) -> Decimal {
        self.price_excl
    }

    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            item_id: row.get_typed("item_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            unit_id: row.get_typed("unit_id")?,
            unit_name: row.get_typed("unit_name")?,
            ordered_qty: row.get_typed("ordered_qty")?,
            received_qty: row.get_typed("received_qty")?,
            price_excl: row.get_typed("price")?,
            price_incl: row.get_typed("price_incl")?,
            tax_rate: row.get_typed("tax_rate")?,
        })
    }
}

/// 取某张采购单的全部明细行 + 各自「已收 / 待收」（行号升序）。
///
/// 一次查询拿到整单：收货页一次带出几十行，不做 N+1。
pub async fn pending_receive_lines(order_id: Uuid) -> DbResult<Vec<PendingReceiveLine>> {
    let sql = format!(
        "SELECT i.id AS item_id, i.line_no, i.material_id, \
                COALESCE(m.material_code, '') AS material_code, \
                COALESCE(m.material_name, '') AS material_name, \
                i.unit_id, COALESCE(u.unit_name, '') AS unit_name, \
                i.qty AS ordered_qty, \
                ({POSTED_RECEIVED_SQL})::numeric(18,4) AS received_qty, \
                i.price, i.price_incl, i.tax_rate \
           FROM biz_purchase_order_item i \
           LEFT JOIN biz_material m ON m.id = i.material_id \
           LEFT JOIN biz_unit u ON u.id = i.unit_id \
          WHERE i.order_id = ?::uuid \
          ORDER BY i.line_no ASC"
    );
    let rows = db()
        .find_with_params(&sql, vec![Value::from(&order_id)])
        .await?;
    rows.iter().map(PendingReceiveLine::from_row).collect()
}

/// 采购单上已生成的收货单（详情页「来源 → 下游」反查，卡面 F3 / ⑤）。
#[derive(Debug, Clone)]
pub struct StockInBrief {
    /// 收货单 id。
    pub id: Uuid,
    /// 收货单号 `IN-yyyyMMdd-000001`。
    pub in_no: String,
    /// 状态（`DRAFT` / `POSTED` / `PENDING_CANCEL` / `CANCELLED`）。
    pub status: String,
    /// 入库日期。
    pub in_date: NaiveDate,
    /// 合计数量。
    pub total_qty: Decimal,
}

impl StockInBrief {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            in_no: row.get_typed("in_no")?,
            status: row.get_typed("status")?,
            in_date: row.get_typed("in_date")?,
            total_qty: row.get_typed("total_qty")?,
        })
    }
}

/// 取某张采购单生成过的收货单（未软删，新单在前）。
pub async fn stock_ins_of_order(order_id: Uuid) -> DbResult<Vec<StockInBrief>> {
    let rows = db()
        .find_with_params(
            "SELECT id, in_no, status, in_date, total_qty FROM biz_stock_in \
              WHERE source_order_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY in_date DESC, in_no DESC",
            vec![Value::from(&order_id)],
        )
        .await?;
    rows.iter().map(StockInBrief::from_row).collect()
}

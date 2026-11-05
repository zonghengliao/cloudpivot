// ============================================================================
// stockin_base.rs —— 底座：表名/状态/流水常量、状态机、行结构与列清单、取号（纯函数 + 取号写库）
//
// 由 `stockin_model.rs` 按职责拆出（TD-1 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `stockin_model.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::rust_decimal::RoundingStrategy;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::serial_model;
use crate::business::shared::scope;
use crate::business::shared::state_machine;

/// 金额 / 单价舍入策略：**四舍五入**（与 PG `ROUND(numeric, n)` 同口径）。
///
/// `rust_decimal` 的默认 `round_dp` 是「四舍六入五成双」，`.005` 边界会比库里少 1 分钱
/// （W3c 在成本转移上实测过）。
pub(super) const ROUNDING: RoundingStrategy = RoundingStrategy::MidpointAwayFromZero;

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
/// 状态：作废审批中（P16）。
///
/// 只有「发起作废 → 审批中」这一种进入方式：单据**锁定**（不许改数量 / 金额 / 明细），
/// 六本账一动没动。审批通过才落 `CANCELLED` 并执行回滚；驳回 / 撤回回到 `POSTED`。
/// 该状态与 `biz_stock_in.approval_instance_id` 由 `ck_biz_si_pending_cancel` 绑死：
/// 有单必有实例、有实例必有单（杜绝「审批中却没有实例 = 永远等不到回调」的孤儿）。
pub const STATUS_PENDING_CANCEL: &str = "PENDING_CANCEL";

/// 状态：过账审批中（P-FLOW-3A）。
///
/// 进入方式只有「提交过账审批」：单据**锁定**（不许改数量 / 金额 / 明细），
/// 库存 / 库位账 / 流水 / 凭证 / 应付 / 序列号**一分没动**。
/// 审批通过才落 `POSTED` 并执行 [`crate::business::erp::stockin_handler`] 的 `post_in_tx`；
/// 驳回 / 撤回打回 `DRAFT`。状态与 `approval_instance_id` 由
/// `ck_biz_si_pending_approval` 绑死（有单必有实例，杜绝孤儿审批）。
pub const STATUS_PENDING_POST: &str = "PENDING_POST";

/// 本卡负责的合法边（卡面 §二）。
///
/// P10 起**唯一真相在 [`crate::business::shared::state_machine`]**：这里只是同一份
/// 常量数组的别名，`state_machine::transition` 还会核对两者一致 ——
/// 想在调用点偷偷多放一条边会立刻 500，而不是静默绕过状态机。
pub const LEGAL_EDGES: [(&str, &str); 10] = state_machine::STOCK_IN_EDGES;

/// 流水方向：入（`ck_biz_inv_txn_type` 只认 `IN` / `OUT`）。
pub const TXN_TYPE_IN: &str = "IN";

/// 流水业务类型：采购入库（卡面 §三 d）。
pub const BIZ_TYPE_PURCHASE_IN: &str = "PURCHASE_IN";

/// 流水单据类型：收货入库单。
pub const DOC_TYPE_STOCK_IN: &str = "STOCK_IN";

/// 收货单 `in_type` → 序列号来源类型（`source_id` 一律 = 本收货单 id）。
///
/// P2-6b 起这张单**自己决定**序列号来源：采购入库写 `PURCHASE`、退货入库写 `RETURN`
/// （两者都在 `biz_stock_in` 这张表里，靠 `in_type` 区分）。
///
/// | `in_type` | `source_type` | 说明 |
/// |---|---|---|
/// | `PURCHASE` | `PURCHASE` | 采购入库 |
/// | `RETURN`   | `RETURN`   | 退货入库（同一张表，靠 `in_type` 区分） |
/// | `OTHER`    | `None`     | 其他入库不在序列号来源的 4 类域内 ⇒ 来源不记（type/id 都不写） |
///
/// 为什么 `OTHER` 不硬塞成 `PURCHASE`：那会让「按来源追溯」把其他入库当成采购，
/// 追溯给出的是**假答案**。来源域里没有 `OTHER` 是设计选择（V0138 定的 4 值），
/// 不在这张卡扩域 —— 扩域要动 CHECK + 前端映射 + 巡检登记表，属于独立卡。
pub fn serial_source_of_in_type(in_type: &str) -> Option<&'static str> {
    use crate::business::shared::doc_kind::SerialSourceType;
    match in_type {
        "PURCHASE" => Some(SerialSourceType::Purchase.as_str()),
        "RETURN" => Some(SerialSourceType::Return.as_str()),
        _ => None,
    }
}

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
        STATUS_PENDING_CANCEL => "作废审批中",
        STATUS_PENDING_POST => "过账审批中",
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
pub(super) async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
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
pub(super) async fn bump_seq(ex: &impl SqlExecutor, period_key: &str) -> DbResult<Option<i64>> {
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
    /// 供应商（**快照**：有源时由采购单派生；无源时由用户显式填写）。
    pub supplier_id: Uuid,
    /// 收货仓库（列 `NOT NULL`）。
    pub warehouse_id: Uuid,
    /// 来源采购单（PURCHASE 有源单必填；无源单为 `None`）。
    pub source_order_id: Option<Uuid>,
    /// 无源单据标记（AP-5：期初 / 复制 / 其他）。
    ///
    /// 与 `source_order_id` 由库约束 `ck_biz_si_sourceless_pair` 钉死
    /// （`is_sourceless = (source_order_id IS NULL)`）：无源必须显式声明，
    /// 不能靠「空就是无源」隐式表达（AP-5 要求可枚举、能进巡检）。
    pub is_sourceless: bool,
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
    /// 库位账行（`biz_inventory_location.id`，可空；P2-4 起由过账回写）。
    pub location_id: Option<Uuid>,
    /// 用户选的库位（`biz_location.id`，可空；过账时 find-or-create 出库位账行）。
    pub location_dict_id: Option<Uuid>,
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
pub const HEADER_COLUMNS: &str = "id, in_no, in_type, source_order_id, supplier_id, is_sourceless, warehouse_id, \
     in_date, status, currency_code::text AS currency_code, total_qty, total_amount, total_tax, \
     total_with_tax, invoice_received, voucher_id, payable_id, remark, posted_at, posted_by, \
     cancelled_at, cancelled_by, cancel_reason, approval_instance_id, created_at, updated_at, \
     create_by, update_by, deleted_at";

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
    /// 供应商（**快照**：有源时派生自采购单，无源时用户显式填写）。
    pub supplier_id: Option<Uuid>,
    /// 无源单据标记（`true` ⇔ 不绑采购单，由 `ck_biz_si_sourceless_pair` 钉死）。
    pub is_sourceless: bool,
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
    /// 作废审批实例 id（P16，`biz_flow_instance.id` 弱引用，无外键）。
    ///
    /// 只有 `status = PENDING_CANCEL` 时非空（由 `ck_biz_si_pending_cancel` 保证）。
    pub approval_instance_id: Option<Uuid>,
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
            is_sourceless: row.get_typed("is_sourceless")?,
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
            approval_instance_id: row.get_typed("approval_instance_id")?,
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
     location_id, location_dict_id, lot_no, remark, created_at, updated_at";

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
    /// 库位账行（`biz_inventory_location.id`；P2-4 起由过账按 `location_dict_id` 回写）。
    pub location_id: Option<Uuid>,
    /// 用户选的库位（`biz_location.id`；NULL = 未指定 → 默认库位 / 未分配桶）。
    pub location_dict_id: Option<Uuid>,
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
            location_dict_id: row.get_typed("location_dict_id")?,
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
    /// 来源采购单号（模糊匹配：`biz_stock_in.source_order_id → biz_purchase_order.order_no`）。
    pub purchase_order_no: Option<String>,
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

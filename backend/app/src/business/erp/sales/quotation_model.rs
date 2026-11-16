//! 销售报价单数据层（销售链 · M3-02：CRUD + 状态机落库）。
//!
//! 表结构唯一依据：`backend/app/migrations/business/V0432__sales_quotation.up.sql`
//! （`biz_quotation` 17 列 / `biz_quotation_line` 12 列 / `biz_seq(seq_type='QT')`）。
//!
//! # 分工
//!
//! | 层 | 负责 |
//! |----|------|
//! | `state_machine`（kernel） | 合法边表 `QUOTATION_EDGES`、需 reason 的边、入账态 —— **唯一真相** |
//! | 本文件 | 行结构 / 取号 / 建单 / 改单 / 软删 / 分页 / 状态迁移落库（CAS） |
//! | `quotation_handler` | 入参校验、业务守卫（客户冻结 / 有效期 / 已转单）、错误文案、HTTP 入口 |
//!
//! # 金额口径（与库里的 CHECK 逐条对应）
//!
//! | 列 | 口径 | 库层兜底 |
//! |----|------|----------|
//! | `amount` | `round(price × qty, 4)`（逐行先舍入再汇总，**禁止先加总再舍入**） | `ck_biz_quotation_line_amount`（非负） |
//! | `tax_amount` | `round(amount × tax_rate / 100, 4)` | 同上 |
//! | `total_amount` / `total_tax` | 各明细同名列之和 | `ck_biz_quotation_amount_nonneg` |
//! | `total_with_tax` | `total_amount + total_tax`（**精确恒等**） | `ck_biz_quotation_amount` |
//!
//! 舍入策略必须是**四舍五入**（`MidpointAwayFromZero`）——`rust_decimal` 的
//! `round_dp` 默认是银行家舍入，恰好 .5 时差 1 个最小单位（与销售订单行同口径，
//! 转订单时整列搬不出尾差）。
//!
//! # 有效期
//!
//! 「已过期」**不入库、不进状态机**（M3-01 建表口径）：一律现算
//! [`is_expired`]（`CURRENT_DATE > valid_until`，按 CST 的今天）。
//! 过期不是状态，所以没有「过期」这个迁移边 —— 只有 handler 在
//! 「发出 / 接受」两个入口上按它拦（过期价不能被客户接受）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_quotation / biz_quotation_line 是 ERP 单据表（V0432 无 dept_id /
//! 归属人列），数据范围（SELF / DEPT_*）表达不出来；可见性由
//! business:erp:quotation:* 权限码控制。
//! 分页走 model_base::page_of → scope::paginate_unscoped（闸①口径），本文件不含手写 COUNT/LIMIT。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::{Decimal, RoundingStrategy};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::{find_by_id, page_of};
use cloudpivot_kernel::business::shared::period_key;
use cloudpivot_kernel::business::shared::{scope, state_machine};

/// 分页口径（默认 20 / 上限 200 / `page_size = 0` 回落）直接复用底座。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 常量 ====================

/// 单头表名（全仓只有这一处字面量）。
pub const TABLE: &str = "biz_quotation";
/// 明细表名。
pub const LINE_TABLE: &str = "biz_quotation_line";

/// 单头列清单（与 V0432 逐列对应）。
pub const COLUMNS: &str = "id, quote_no, customer_id, quote_date, valid_until, status, \
     total_amount, total_tax, total_with_tax, converted_so_id, converted_at, remark, \
     created_at, updated_at, create_by, update_by, deleted_at";

/// 明细列清单（与 V0432 逐列对应）。
pub const LINE_COLUMNS: &str =
    "id, quotation_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, remark";

/// 单号前缀（`biz_seq.seq_type = 'QT'`，V0432 种子）。
pub const NO_PREFIX: &str = "QT";
/// 取号规则类型。
pub const SEQ_TYPE: &str = "QT";
/// 流水位数（不足补零；超过 6 位自然变长，不截断）。
pub const NO_SEQ_WIDTH: usize = 6;

/// 状态域（与 `ck_biz_quotation_status` 逐字一致，5 值）。
pub const STATUSES: [&str; 5] = [
    STATUS_DRAFT,
    STATUS_SENT,
    STATUS_ACCEPTED,
    STATUS_REJECTED,
    STATUS_CANCELLED,
];
/// 草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 已发出（单价与明细冻结）。
pub const STATUS_SENT: &str = "SENT";
/// 客户已接受（M3-03 才允许转销售订单）。
pub const STATUS_ACCEPTED: &str = "ACCEPTED";
/// 客户已拒绝（终态）。
pub const STATUS_REJECTED: &str = "REJECTED";
/// 已作废（终态）。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 允许软删的两个状态（M3-01 建表注释：「仅 DRAFT/CANCELLED 允许删，由 M3-02 守卫」）。
///
/// 已发出（`SENT`）的单子不许删：客户手里那份报价还在，删了系统里就查不到
/// 「当时报了多少」—— 要撤回请走 `SENT → CANCELLED`（要原因、留轨迹）。
pub const DELETABLE_STATUSES: [&str; 2] = [STATUS_DRAFT, STATUS_CANCELLED];

/// 合法边表：**唯一真相**在 `state_machine::QUOTATION_EDGES`（本文件只引用，不复制）。
pub const LEGAL_EDGES: [(&str, &str); 6] = state_machine::QUOTATION_EDGES;

/// 状态机判定：只有 [`LEGAL_EDGES`] 里的边放行（其余一律 409，见 handler）。
pub fn can_transition(from: &str, to: &str) -> bool {
    LEGAL_EDGES.iter().any(|(f, t)| *f == from && *t == to)
}

/// 从 `from` 出发的合法目标态（非法迁移的 409 文案里要列出来，让用户知道能点哪几个）。
pub fn allowed_targets(from: &str) -> Vec<&'static str> {
    LEGAL_EDGES
        .iter()
        .filter(|(f, _)| *f == from)
        .map(|(_, t)| *t)
        .collect()
}

/// 状态值是否在域内（handler 对 `status` 筛选参数与 `target_status` 都要过这一关，
/// 静默忽略会把「已接受」筛成「全部」，是假数据）。
pub fn is_valid_status(value: &str) -> bool {
    STATUSES.contains(&value)
}

/// 状态中文标签（错误文案与前端徽标共用；未知值原样给「未知」，不 panic）。
pub fn status_label(status: &str) -> &'static str {
    match status {
        STATUS_DRAFT => "草稿",
        STATUS_SENT => "已发出",
        STATUS_ACCEPTED => "已接受",
        STATUS_REJECTED => "已拒绝",
        STATUS_CANCELLED => "已作废",
        _ => "未知",
    }
}

/// 分页锚点（ERP 单据表，不参与数据范围）。
pub const QUOTATION_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

/// 列表排序：报价日期倒序、同一天按单号倒序（最新单在前）。
pub const QUOTATION_ORDER: &str = "ORDER BY quote_date DESC, quote_no DESC";

/// 明细排序：行号升序。
pub const LINE_ORDER: &str = "ORDER BY line_no ASC";

// ==================== 纯逻辑：有效期 / 金额 ====================

/// 「已过期」：`today > valid_until`（`valid_until` 当天仍有效 —— M3-01 的既定口径）。
///
/// 过期是**时间事实**不是状态：报价单停在 `SENT` 但今天已经过了有效期，
/// 界面上显示「已过期」，库里 `status` 不变（没有定时任务翻转，也就没有时钟回拨对账）。
pub fn is_expired(valid_until: NaiveDate, today: NaiveDate) -> bool {
    today > valid_until
}

/// 行金额拆分：`(amount, tax_amount)`，各 4 位定标、四舍五入。
///
/// `amount = round(price × qty, 4)`、`tax_amount = round(amount × tax_rate / 100, 4)`。
/// 与 `sales_order_model::line_amounts` 逐字同口径（转订单时能整列搬）。
pub fn line_amounts(qty: Decimal, price: Decimal, tax_rate: Decimal) -> (Decimal, Decimal) {
    let amount = (price * qty).round_dp_with_strategy(4, RoundingStrategy::MidpointAwayFromZero);
    let tax =
        (amount * tax_rate / Decimal::from(100)).round_dp_with_strategy(4, RoundingStrategy::MidpointAwayFromZero);
    (amount, tax)
}

/// 单头金额三项是否**非负**（DDL 的 `ck_biz_quotation_amount_nonneg` 是兜底，入口先拒）。
pub fn amounts_non_negative(total_amount: Decimal, total_tax: Decimal, total_with_tax: Decimal) -> bool {
    total_amount >= Decimal::ZERO && total_tax >= Decimal::ZERO && total_with_tax >= Decimal::ZERO
}

/// 价税合计恒等：`total_with_tax == total_amount + total_tax`（**精确相等**，不容差）。
pub fn totals_consistent(total_amount: Decimal, total_tax: Decimal, total_with_tax: Decimal) -> bool {
    total_with_tax == total_amount + total_tax
}

// ==================== 单号（纯逻辑 + 取号） ====================

/// 组装单号（`QT-{周期键}-{6 位流水}`）。
pub fn format_quote_no(period_key: &str, no: i64) -> String {
    format!("{NO_PREFIX}-{period_key}-{no:0NO_SEQ_WIDTH$}")
}

/// 拆单号：`QT-yyyyMMdd-NNNNNN` → `(周期键, 流水)`。
///
/// 严格校验（不是 `split('-')` 一把梭）：前缀 / 8 位合法日期 / 流水 ≥ 1。
/// 任何一条不符 → `None`。
pub fn parse_quote_no(value: &str) -> Option<(String, i64)> {
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

/// `biz_seq(seq_type='QT')` 缺失时幂等补一行（正常库有 V0432 的种子）。
///
/// 补的是**与 V0432 逐字相同**的规则（prefix `QT` / `yyyyMMdd` / 分隔符 `-`），
/// 否则同一个号段会出现两种格式的单号。
async fn ensure_seq_row(ex: &impl SqlExecutor) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, 'QT', '报价单号', 'QT', 'yyyyMMdd', '', 0, 1, '-', 1, \
                '报价单单号：QT-yyyyMMdd-000001，按天（CST）滚动' \
         WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'QT' AND q.deleted_at IS NULL)"
            .to_string(),
        vec![Value::from(Uuid::now_v7())],
    )
    .await?;
    Ok(())
}

/// 推进 `biz_seq(seq_type='QT')` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
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
              WHERE seq_type = 'QT' AND deleted_at IS NULL \
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

/// 取一个报价单号（周期键按北京时间）。
///
/// 调用方必须在**同一个事务**里调用本函数 + 写单头 + 写明细：取号推了
/// `current_no` 却没落单，那个号就永久占用（跳号可接受，重号不可接受）。
pub async fn next_quote_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key::period_key_of(now);
    ensure_seq_row(ex).await?;
    match bump_seq(ex, &period_key).await? {
        Some(no) => Ok(format_quote_no(&period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='QT') 行不存在（period_key={period_key}）"
        ))),
    }
}

/// CST 的今天（有效期判定 / 列表「已过期」筛选共用）。
pub fn today_cst(now: DateTime<Utc>) -> NaiveDate {
    period_key::today_cst(now)
}

// ==================== 行结构 ====================

/// `biz_quotation` 一行。
#[derive(Debug, Clone)]
pub struct QuotationRow {
    /// 主键。
    pub id: Uuid,
    /// 单号（`QT-yyyyMMdd-000001`，存活行唯一）。
    pub quote_no: String,
    /// 客户（弱引用 `biz_customer.id`，无外键）。
    pub customer_id: Uuid,
    /// 报价日期。
    pub quote_date: NaiveDate,
    /// 有效期至（含当天）。
    pub valid_until: NaiveDate,
    /// 状态（5 值之一）。
    pub status: String,
    /// 不含税合计。
    pub total_amount: Decimal,
    /// 税额合计。
    pub total_tax: Decimal,
    /// 价税合计（= 前两项之和）。
    pub total_with_tax: Decimal,
    /// 转成的销售订单（M3-03 回填；`None` = 未转单）。
    pub converted_so_id: Option<Uuid>,
    /// 转单时间（与 `converted_so_id` 成对）。
    pub converted_at: Option<DateTime<Utc>>,
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

impl QuotationRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            quote_no: row.get_typed("quote_no")?,
            customer_id: row.get_typed("customer_id")?,
            quote_date: row.get_typed("quote_date")?,
            valid_until: row.get_typed("valid_until")?,
            status: row.get_typed("status")?,
            total_amount: row.get_typed("total_amount")?,
            total_tax: row.get_typed("total_tax")?,
            total_with_tax: row.get_typed("total_with_tax")?,
            converted_so_id: row.get_typed("converted_so_id")?,
            converted_at: row.get_typed("converted_at")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            create_by: row.get_typed("create_by")?,
            update_by: row.get_typed("update_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }

    /// 这张报价单是否已转成销售订单（M3-03 回填 `converted_so_id`）。
    pub fn is_converted(&self) -> bool {
        self.converted_so_id.is_some()
    }

    /// 在 `today` 这天是否已过期（现算，不读状态列）。
    pub fn expired_on(&self, today: NaiveDate) -> bool {
        is_expired(self.valid_until, today)
    }
}

/// `biz_quotation_line` 一行。
#[derive(Debug, Clone)]
pub struct QuotationLineRow {
    /// 主键。
    pub id: Uuid,
    /// 所属报价单。
    pub quotation_id: Uuid,
    /// 行号（1 起，单内唯一）。
    pub line_no: i32,
    /// 物料（弱引用 `biz_material.id`）。
    pub material_id: Uuid,
    /// 报价数量（> 0）。
    pub qty: Decimal,
    /// 不含税单价。
    pub price: Decimal,
    /// 行金额（不含税）。
    pub amount: Decimal,
    /// 税率（百分数 0–100）。
    pub tax_rate: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

impl QuotationLineRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            quotation_id: row.get_typed("quotation_id")?,
            line_no: row.get_typed("line_no")?,
            material_id: row.get_typed("material_id")?,
            qty: row.get_typed("qty")?,
            price: row.get_typed("price")?,
            amount: row.get_typed("amount")?,
            tax_rate: row.get_typed("tax_rate")?,
            tax_amount: row.get_typed("tax_amount")?,
            remark: row.get_typed("remark")?,
        })
    }

    /// 含税金额（= `amount + tax_amount`，库里不单独存）。
    pub fn with_tax(&self) -> Decimal {
        self.amount + self.tax_amount
    }
}

/// 待写入的一行明细（金额由 [`line_amounts`] 算好，调用方不许自己编）。
#[derive(Debug, Clone)]
pub struct NewLine {
    /// 物料。
    pub material_id: Uuid,
    /// 数量（> 0）。
    pub qty: Decimal,
    /// 不含税单价（>= 0）。
    pub price: Decimal,
    /// 税率百分数（0–100）。
    pub tax_rate: Decimal,
    /// 行金额（= `round(price × qty, 4)`）。
    pub amount: Decimal,
    /// 行税额。
    pub tax_amount: Decimal,
    /// 行备注。
    pub remark: Option<String>,
}

/// 建单 / 改单的单头字段。
#[derive(Debug, Clone, Copy)]
pub struct NewQuotation<'a> {
    /// 客户。
    pub customer_id: Uuid,
    /// 报价日期。
    pub quote_date: NaiveDate,
    /// 有效期至（>= `quote_date`）。
    pub valid_until: NaiveDate,
    /// 备注（空串由 SQL 转 NULL）。
    pub remark: Option<&'a str>,
}

/// 单头三项合计（由明细汇总，禁止调用方直接给）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// 不含税合计。
    pub amount: Decimal,
    /// 税额合计。
    pub tax: Decimal,
    /// 价税合计（= `amount + tax`，精确恒等）。
    pub with_tax: Decimal,
}

impl Totals {
    /// 由明细汇总：**逐行金额已舍入**，这里只做加法（先舍入再相加）。
    pub fn of(lines: &[NewLine]) -> Self {
        let mut amount = Decimal::ZERO;
        let mut tax = Decimal::ZERO;
        for line in lines {
            amount += line.amount;
            tax += line.tax_amount;
        }
        Self {
            amount,
            tax,
            with_tax: amount + tax,
        }
    }
}

// ==================== 查询 ====================

/// 按主键取报价单（软删行不返回）。
pub async fn find_quotation(id: Uuid) -> DbResult<Option<QuotationRow>> {
    find_by_id(TABLE, COLUMNS, id, true, QuotationRow::from_row).await
}

/// 在事务里**锁住**一张报价单（`SELECT … FOR UPDATE`），把同一张单上的并发转单串行化。
///
/// 必须在事务里调用。不存在 / 已软删 → `None`（调用方报 404）。
///
/// 为什么转单必须加锁（M3-03）：
/// ① 转单是「建销售订单 + 回填 `converted_so_id`」两件事，只有 CAS 没有锁时，
///    两个并发请求会各建一张订单，各自 CAS 只有一张能成功 —— 失败那张的订单**已经建出来了**，
///    只能靠回滚兜住；有锁则在建单前就把第二个请求挡住（它读到的就是「已转过」）。
/// ② 与 `biz_quotation` 的行锁同序（只有这一把锁），不会和别的链路互锁。
pub async fn lock_quotation(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<QuotationRow>> {
    // SCOPE-EXEMPT: 报价单是 ERP 单据表（V0432 无归属列），锁单行不产出用户可见行集。
    let rows = ex
        .query_sql(
            format!("SELECT {COLUMNS} FROM {TABLE} WHERE id = ? AND deleted_at IS NULL FOR UPDATE"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(QuotationRow::from_row).transpose()
}

/// 取一张报价单的明细（行号升序）。
pub async fn lines_of(quotation_id: Uuid) -> DbResult<Vec<QuotationLineRow>> {
    lines_of_in(db(), quotation_id).await
}

/// 取明细的**指定执行器**版本（转单时要在持有行锁的同一事务里读明细，
/// 否则「锁住的单头」与「读到的明细」可能来自两个快照）。
pub async fn lines_of_in(ex: &impl SqlExecutor, quotation_id: Uuid) -> DbResult<Vec<QuotationLineRow>> {
    let sql = format!("SELECT {LINE_COLUMNS} FROM {LINE_TABLE} WHERE quotation_id = ? {LINE_ORDER}");
    let rows = ex.query_sql(sql, vec![Value::from(quotation_id)]).await?;
    rows.iter().map(QuotationLineRow::from_row).collect()
}

/// 分页筛选入参。
#[derive(Debug, Clone, Copy, Default)]
pub struct QuotationPageQuery<'a> {
    /// 单号模糊（`LIKE %...%`）。
    pub quote_no: Option<&'a str>,
    /// 状态精确（调用方已校验在域内）。
    pub status: Option<&'a str>,
    /// 客户精确。
    pub customer_id: Option<Uuid>,
    /// 有效期筛选：`Some(true)` = 已过期、`Some(false)` = 未过期（含今天）。
    pub expired: Option<bool>,
    /// CST 的今天（`expired` 筛选要用）。
    pub today: NaiveDate,
    /// 页码（1 起）。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 分页：`quote_no`（模糊）/ `status`（精确）/ `customer_id`（精确）/ `expired`（现算）。
pub async fn quotation_page(query: &QuotationPageQuery<'_>) -> DbResult<(Vec<QuotationRow>, i64)> {
    let mut sql = format!("SELECT {COLUMNS} FROM {TABLE} WHERE deleted_at IS NULL");
    let mut params: Vec<Value> = Vec::new();
    if let Some(pattern) = cloudpivot_kernel::utils::like_pattern(query.quote_no) {
        sql.push_str(" AND quote_no LIKE ?");
        params.push(Value::from(pattern));
    }
    if let Some(status) = query.status.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND status = ?");
        params.push(Value::from(status));
    }
    if let Some(customer_id) = query.customer_id {
        sql.push_str(" AND customer_id = ?");
        params.push(Value::from(customer_id));
    }
    match query.expired {
        // 过期一律现算（状态列里没有 EXPIRED，见模块注释）
        Some(true) => {
            sql.push_str(" AND valid_until < ?::text::date");
            params.push(Value::Text(query.today.to_string()));
        }
        Some(false) => {
            sql.push_str(" AND valid_until >= ?::text::date");
            params.push(Value::Text(query.today.to_string()));
        }
        None => {}
    }
    page_of(
        QUOTATION_TARGET,
        sql,
        params,
        QUOTATION_ORDER,
        query.page,
        query.page_size,
        QuotationRow::from_row,
    )
    .await
}

/// 批量取「客户 id → （编码, 名称）」，分页列表一次往返补齐（避免 N+1）。
///
/// 软删客户不出现在结果里（调用方按缺失处理 = 两列留 `None`）—— 与
/// `sales_order_model::customer_labels` 同口径：单号 / 金额照常显示，只是不再显示已删客户名。
pub async fn customer_labels(ids: &[Uuid]) -> DbResult<Vec<(Uuid, String, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let placeholders = vec!["?"; unique.len()].join(", ");
    let sql = format!(
        "SELECT id, code, name FROM biz_customer \
          WHERE id IN ({placeholders}) AND deleted_at IS NULL"
    );
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id: Uuid = row.get_typed("id")?;
        let code: String = row.get_typed("code")?;
        let name: String = row.get_typed("name")?;
        out.push((id, code, name));
    }
    Ok(out)
}

// ==================== 写入 ====================

/// 插明细（调用方保证 `quotation_id` 存在、行号从 1 起连续）。
async fn insert_lines(ex: &impl SqlExecutor, quotation_id: Uuid, lines: &[NewLine]) -> DbResult<()> {
    for (index, line) in lines.iter().enumerate() {
        // `?::text::numeric`：驱动不接受把 `Value::Text` 直接绑到 numeric 列（SO-04 实测坑）
        ex.exec_sql(
            format!(
                "INSERT INTO {LINE_TABLE} \
                    (quotation_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, remark) \
                 VALUES (?, ?, ?, ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                    ?::text::numeric, ?::text::numeric, NULLIF(?::text,''))"
            ),
            vec![
                Value::from(&quotation_id),
                Value::from(i32::try_from(index + 1).unwrap_or(i32::MAX)),
                Value::from(&line.material_id),
                Value::Text(line.qty.to_string()),
                Value::Text(line.price.to_string()),
                Value::Text(line.amount.to_string()),
                Value::Text(line.tax_rate.to_string()),
                Value::Text(line.tax_amount.to_string()),
                Value::Text(line.remark.clone().unwrap_or_default()),
            ],
        )
        .await?;
    }
    Ok(())
}

/// 建单（`status = DRAFT`）：**取号 → 单头 → 明细 → 建单日志**，调用方负责事务边界。
///
/// 返回 `(新单 id, 单号)`。
///
/// # 日期参数为什么写 `?::text::date` 而不是 `?::date`
///
/// 驱动的 `Value::Text` 只能绑到**文本家族**参数（见 `nik-db-driver-postgres` 的
/// `PgParam::Text`：目标类型不是 text/varchar/枚举就直接 `type_mismatch`）。
/// 写成 `?::date` 时 PG 会把该参数的类型推断成 `date`，于是执行期报
/// **`error serializing parameter N`**（对用户是 500 + 10004，日志里看不出是日期问题）——
/// M3-02 实测踩过一次。正确写法是**先转 text 再转 date**：`?::text::date`
/// （与 `sales_order_model` 的 `NULLIF(?::text,'')::date` 同一手法）。
pub async fn create_quotation(
    ex: &impl SqlExecutor,
    new: &NewQuotation<'_>,
    lines: &[NewLine],
    totals: &Totals,
    operator: state_machine::Operator,
    now: DateTime<Utc>,
) -> DbResult<(Uuid, String)> {
    let quote_no = next_quote_no(ex, now).await?;
    let id = Uuid::now_v7();
    let operator_text = Value::Text(operator.id.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "INSERT INTO {TABLE} \
                (id, quote_no, customer_id, quote_date, valid_until, status, \
                 total_amount, total_tax, total_with_tax, remark, \
                 created_at, updated_at, create_by, update_by) \
             VALUES (?, ?, ?, ?::text::date, ?::text::date, ?, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, NULLIF(?::text,''), \
                 ?, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
        ),
        vec![
            Value::from(&id),
            Value::Text(quote_no.clone()),
            Value::from(&new.customer_id),
            Value::Text(new.quote_date.to_string()),
            Value::Text(new.valid_until.to_string()),
            Value::Text(STATUS_DRAFT.to_string()),
            Value::Text(totals.amount.to_string()),
            Value::Text(totals.tax.to_string()),
            Value::Text(totals.with_tax.to_string()),
            Value::Text(new.remark.unwrap_or_default().to_string()),
            Value::from(now),
            Value::from(now),
            operator_text.clone(),
            operator_text,
        ],
    )
    .await?;
    insert_lines(ex, id, lines).await?;
    // 建单留痕（`from_status = NULL → DRAFT`），与建单同一事务
    state_machine::log_created(
        ex,
        state_machine::ENTITY_QUOTATION,
        id,
        &quote_no,
        STATUS_DRAFT,
        operator,
        None,
    )
    .await
    .map_err(|error| DbError::InvalidValue(format!("[{}] {}", error.code, error.msg)))?;
    Ok((id, quote_no))
}

/// 改单入参（单头覆盖 + 明细整表替换）。
#[derive(Debug, Clone, Copy)]
pub struct UpdateQuotationInput<'a> {
    /// 单据主键。
    pub id: Uuid,
    /// 新的单头字段。
    pub new: &'a NewQuotation<'a>,
    /// 新的明细（旧明细整表删除后按此重插）。
    pub lines: &'a [NewLine],
    /// 单头合计（调用方算好：`Totals::of(lines)`）。
    pub totals: &'a Totals,
    /// 操作人（落 `update_by`）。
    pub operator: Option<Uuid>,
    /// 业务时间（落 `updated_at`）。
    pub now: DateTime<Utc>,
}

/// 改单（**CAS：`WHERE status = 'DRAFT'`**）：单头覆盖 + 明细「删旧插新」。
///
/// 返回 `affected`（0 = 读状态与写状态之间被别人推走，调用方按 409 处理）。
/// `affected = 0` 时**不动明细**（否则会把别人的单据明细删了）。
pub async fn update_quotation(ex: &impl SqlExecutor, input: UpdateQuotationInput<'_>) -> DbResult<u64> {
    let UpdateQuotationInput {
        id,
        new,
        lines,
        totals,
        operator,
        now,
    } = input;
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    let affected = ex
        .exec_sql(
            format!(
                "UPDATE {TABLE} \
                    SET customer_id = ?, quote_date = ?::text::date, valid_until = ?::text::date, \
                        total_amount = ?::text::numeric, total_tax = ?::text::numeric, \
                        total_with_tax = ?::text::numeric, remark = NULLIF(?::text,''), \
                        updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
                  WHERE id = ? AND status = ? AND deleted_at IS NULL"
            ),
            vec![
                Value::from(&new.customer_id),
                Value::Text(new.quote_date.to_string()),
                Value::Text(new.valid_until.to_string()),
                Value::Text(totals.amount.to_string()),
                Value::Text(totals.tax.to_string()),
                Value::Text(totals.with_tax.to_string()),
                Value::Text(new.remark.unwrap_or_default().to_string()),
                Value::from(now),
                operator_text,
                Value::from(&id),
                Value::Text(STATUS_DRAFT.to_string()),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(0);
    }
    ex.exec_sql(
        format!("DELETE FROM {LINE_TABLE} WHERE quotation_id = ?"),
        vec![Value::from(&id)],
    )
    .await?;
    insert_lines(ex, id, lines).await?;
    Ok(affected)
}

/// 状态迁移落库（CAS：`WHERE status = from`，返回 0 = 并发被别人推走）。
///
/// **调用方必须先过 `state_machine::transition`**（边表校验 + reason 校验 + 写
/// `biz_status_log`），本函数只改单据头的 `status` 列。
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

/// 转单回填（M3-03）：把「转成了哪张销售订单」写回报价单。
///
/// **CAS 双条件**：`status = <调用方读到的状态>` 且 `converted_so_id IS NULL`。
/// 返回 `affected`（0 = 期间被别人转过 / 状态变了，调用方按 409 处理并整笔回滚）。
///
/// 为什么必须与「建销售订单」在**同一个事务**里：
/// ⑪ 约定（事务提交后失败必须有补偿）—— 如果先提交订单再回填，回填失败就留下一张
/// 没有来源的订单（重复点一次会再建一张），界面上没有任何按钮能修正它，只能改库。
/// 这里两件事同一事务：要么一起成功，要么订单根本没建出来。
pub async fn mark_converted(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    so_id: Uuid,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} \
                SET converted_so_id = ?, converted_at = ?, updated_at = ?, \
                    update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ? AND status = ? AND converted_so_id IS NULL AND deleted_at IS NULL"
        ),
        vec![
            Value::from(&so_id),
            Value::from(now),
            Value::from(now),
            operator_text,
            Value::from(&id),
            Value::Text(from_status.to_string()),
        ],
    )
    .await
}

/// 软删（CAS：`WHERE id = ? AND status = <调用方读到的状态>`，且只允许
/// [`DELETABLE_STATUSES`] 里的状态）。
///
/// 返回 `affected`（0 = 状态在读写之间被改了 / 状态本来就不允许删，调用方按 409 处理）。
/// 明细**不删**（外键 CASCADE 只在物理删时触发；软删保留明细，历史报价仍可回看）。
pub async fn soft_delete(
    ex: &impl SqlExecutor,
    id: Uuid,
    from_status: &str,
    operator: Option<Uuid>,
    now: DateTime<Utc>,
) -> DbResult<u64> {
    let operator_text = Value::Text(operator.map(|value| value.to_string()).unwrap_or_default());
    ex.exec_sql(
        format!(
            "UPDATE {TABLE} \
                SET deleted_at = ?, updated_at = ?, update_by = NULLIF(?::text,'')::uuid \
              WHERE id = ? AND status = ? AND deleted_at IS NULL \
                AND status IN (?, ?)"
        ),
        vec![
            Value::from(now),
            Value::from(now),
            operator_text,
            Value::from(&id),
            Value::Text(from_status.to_string()),
            Value::Text(STATUS_DRAFT.to_string()),
            Value::Text(STATUS_CANCELLED.to_string()),
        ],
    )
    .await
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// 定点字面量（`17.65` 这类在 f64 下不精确，一律走字符串）。
    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    /// 状态域必须与 V0432 的 `ck_biz_quotation_status` 逐字一致（5 值）。
    #[test]
    fn status_domain_matches_check_constraint() {
        assert_eq!(STATUSES, ["DRAFT", "SENT", "ACCEPTED", "REJECTED", "CANCELLED"]);
        for status in STATUSES {
            assert!(is_valid_status(status));
            assert_ne!(status_label(status), "未知", "每个域内值都要有中文标签");
        }
        assert!(!is_valid_status("WIN"), "域外值必须判非法");
        assert!(!is_valid_status("draft"), "大小写敏感");
        assert_eq!(status_label("WIN"), "未知");
    }

    /// 边表的每个端点都必须在状态域里（否则迁移能过的值会被判非法边）。
    #[test]
    fn legal_edges_use_declared_status_domain() {
        assert_eq!(
            LEGAL_EDGES,
            state_machine::QUOTATION_EDGES,
            "LEGAL_EDGES 必须与注册表同源"
        );
        assert_eq!(
            LEGAL_EDGES.len(),
            6,
            "卡面 3 条正向 + 3 条反向（ARCH-7 §12.3 正向必配反向）"
        );
        for (from, to) in LEGAL_EDGES {
            assert!(is_valid_status(from), "边的起点 {from} 不在状态域里");
            assert!(is_valid_status(to), "边的终点 {to} 不在状态域里");
        }
    }

    /// 卡面的三条正向主链全通；顺手钉住「已发出 → 已接受 / 已拒绝」两条并列终点。
    #[test]
    fn main_chain_edges_allowed() {
        assert!(can_transition("DRAFT", "SENT"));
        assert!(can_transition("SENT", "ACCEPTED"));
        assert!(can_transition("SENT", "REJECTED"));
        assert_eq!(allowed_targets("DRAFT"), vec!["SENT", "CANCELLED"]);
        assert_eq!(allowed_targets("SENT"), vec!["ACCEPTED", "REJECTED", "CANCELLED"]);
        assert_eq!(allowed_targets("ACCEPTED"), vec!["CANCELLED"]);
        assert!(allowed_targets("REJECTED").is_empty(), "已拒绝是终态");
        assert!(allowed_targets("CANCELLED").is_empty(), "已作废是终态");
    }

    /// 非法边一条都不放（每条都是一个真实的业务错误）。
    #[test]
    fn illegal_transitions_rejected() {
        let cases = [
            // 发出后再打回草稿改价 = 客户手里的纸和库里对不上
            ("SENT", "DRAFT"),
            // 跳步：草稿直接接受 / 拒绝（还没报出去）
            ("DRAFT", "ACCEPTED"),
            ("DRAFT", "REJECTED"),
            // 已接受再回已发出（改条款要走新单）
            ("ACCEPTED", "SENT"),
            ("ACCEPTED", "REJECTED"),
            // 终态出不去
            ("REJECTED", "DRAFT"),
            ("REJECTED", "SENT"),
            ("REJECTED", "CANCELLED"),
            ("CANCELLED", "DRAFT"),
            ("CANCELLED", "SENT"),
        ];
        for (from, to) in cases {
            assert!(!can_transition(from, to), "{from} → {to} 不该是合法边");
        }
    }

    /// 需要 reason 的边：客户拒绝 + 两条「已对外发出后」的作废；草稿作废不要原因。
    #[test]
    fn reason_edges_are_client_reject_and_post_send_cancels() {
        let entity = state_machine::Entity::Quotation;
        assert!(entity.needs_reason("SENT", "REJECTED"));
        assert!(entity.needs_reason("SENT", "CANCELLED"));
        assert!(entity.needs_reason("ACCEPTED", "CANCELLED"));
        assert!(!entity.needs_reason("DRAFT", "CANCELLED"), "草稿从未对外发出，不要原因");
        assert!(!entity.needs_reason("DRAFT", "SENT"));
        assert!(!entity.needs_reason("SENT", "ACCEPTED"));
    }

    /// 入账态：只有草稿可写（发出即冻结单价与明细）。
    #[test]
    fn only_draft_is_writable() {
        let entity = state_machine::Entity::Quotation;
        assert!(!entity.is_locked(STATUS_DRAFT));
        for status in [STATUS_SENT, STATUS_ACCEPTED, STATUS_REJECTED, STATUS_CANCELLED] {
            assert!(entity.is_locked(status), "{status} 必须是入账态（只读）");
        }
        for status in DELETABLE_STATUSES {
            assert!(!STATUSES.contains(&status) || status != STATUS_SENT, "SENT 不许软删");
        }
        assert_eq!(DELETABLE_STATUSES, ["DRAFT", "CANCELLED"]);
    }

    /// 行金额：四舍五入（不是银行家舍入），逐行先舍入再相加。
    #[test]
    fn line_amounts_round_half_away_from_zero() {
        // 3 × 17.655 = 52.965000；× 13% = 6.8854500 → 四舍五入到 4 位 = 6.8855
        // （银行家舍入会给 6.8854，两者差 1 个最小单位）
        let (amount, tax) = line_amounts(dec("3"), dec("17.655"), dec("13"));
        assert_eq!(amount, dec("52.9650"));
        assert_eq!(tax, dec("6.8855"), "税额必须四舍五入（MidpointAwayFromZero）");
        // 0 税率：税额恒 0
        let (amount, tax) = line_amounts(dec("2"), dec("100"), dec("0"));
        assert_eq!((amount, tax), (dec("200.0000"), dec("0.0000")));
        // 单价 6 位小数：先乘再舍入到 4 位
        let (amount, _) = line_amounts(dec("1"), dec("0.000001"), dec("13"));
        assert_eq!(amount, dec("0.0000"), "四舍五入到 4 位后不足 1 个最小单位");
    }

    /// 合计恒等：`with_tax = amount + tax`，逐行舍入后汇总不产生尾差。
    #[test]
    fn totals_are_exact_sums_of_rounded_lines() {
        let lines = vec![
            NewLine {
                material_id: Uuid::now_v7(),
                qty: dec("3"),
                price: dec("17.655"),
                tax_rate: dec("13"),
                amount: dec("52.9650"),
                tax_amount: dec("6.8855"),
                remark: None,
            },
            NewLine {
                material_id: Uuid::now_v7(),
                qty: dec("2"),
                price: dec("100"),
                tax_rate: dec("0"),
                amount: dec("200.0000"),
                tax_amount: dec("0.0000"),
                remark: None,
            },
        ];
        let totals = Totals::of(&lines);
        assert_eq!(totals.amount, dec("252.9650"));
        assert_eq!(totals.tax, dec("6.8855"));
        assert_eq!(totals.with_tax, dec("259.8505"));
        assert!(totals_consistent(totals.amount, totals.tax, totals.with_tax));
        assert!(amounts_non_negative(totals.amount, totals.tax, totals.with_tax));
        // 空明细：三项都是 0（库层 CHECK 允许，handler 不允许提交空单）
        let empty = Totals::of(&[]);
        assert_eq!(
            (empty.amount, empty.tax, empty.with_tax),
            (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO)
        );
    }

    /// 有效期边界：`valid_until` 当天仍有效，次日才算过期。
    #[test]
    fn expiry_boundary_is_inclusive_on_valid_until() {
        let valid_until = NaiveDate::from_ymd_opt(2026, 11, 30).expect("合法日期");
        let same_day = NaiveDate::from_ymd_opt(2026, 11, 30).expect("合法日期");
        let next_day = NaiveDate::from_ymd_opt(2026, 12, 1).expect("合法日期");
        let day_before = NaiveDate::from_ymd_opt(2026, 11, 29).expect("合法日期");
        assert!(!is_expired(valid_until, same_day), "有效期至当天不算过期");
        assert!(!is_expired(valid_until, day_before));
        assert!(is_expired(valid_until, next_day), "次日起算过期");
    }

    /// 单号格式：`QT-yyyyMMdd-000001`，6 位补零；拆分严格校验。
    #[test]
    fn quote_no_format_and_parse() {
        let no = format_quote_no("20261117", 1);
        assert_eq!(no, "QT-20261117-000001");
        assert_eq!(no.len(), "QT-yyyyMMdd-000001".len());
        assert_eq!(parse_quote_no(&no), Some(("20261117".to_string(), 1)));
        // 超过 6 位自然变长，不截断
        assert_eq!(format_quote_no("20261117", 1_000_001), "QT-20261117-1000001");
        assert_eq!(
            parse_quote_no("QT-20261117-1000001"),
            Some(("20261117".to_string(), 1_000_001))
        );
        // 非法：前缀 / 日期 / 流水
        for bad in [
            "SO-20261117-000001",
            "QT-2026111-000001",
            "QT-20261306-000001",
            "QT-20261117-000000",
            "QT-20261117-abc",
            "QT-20261117",
            "",
        ] {
            assert_eq!(parse_quote_no(bad), None, "`{bad}` 不该被当成合法单号");
        }
    }

    // ==================== 真库用例 ====================

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 夹具客户编码 —— 清场按它定位（单号是取号器生成的，不能按前缀删单据）。
    const CUSTOMER_CODE: &str = "M302-CUST";

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

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 清场：状态日志 → 单头（明细 CASCADE）→ 夹具客户。
    async fn cleanup() {
        let statements: [(&str, Vec<Value>); 3] = [
            (
                "DELETE FROM biz_status_log \
                  WHERE entity_type = 'QUOTATION' \
                    AND entity_id IN ( \
                          SELECT q.id FROM biz_quotation q \
                            JOIN biz_customer c ON c.id = q.customer_id \
                           WHERE c.code = ?)",
                vec![Value::from(CUSTOMER_CODE)],
            ),
            (
                "DELETE FROM biz_quotation \
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

    /// 夹具客户（ACTIVE）。存在则复用。
    async fn fixture_customer() -> Uuid {
        db().execute_with_params(
            "INSERT INTO biz_customer (id, code, name, credit_limit, status) \
             VALUES (gen_random_uuid(), ?, 'M3-02 夹具客户', 10000, 'ACTIVE') \
             ON CONFLICT DO NOTHING",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("插夹具客户应成功");
        db().scalar_with_params(
            "SELECT id FROM biz_customer WHERE code = ? AND deleted_at IS NULL",
            vec![Value::from(CUSTOMER_CODE)],
        )
        .await
        .expect("查夹具客户应成功")
    }

    fn line(qty: &str, price: &str, rate: &str) -> NewLine {
        let qty = dec(qty);
        let price = dec(price);
        let rate = dec(rate);
        let (amount, tax_amount) = line_amounts(qty, price, rate);
        NewLine {
            material_id: Uuid::now_v7(),
            qty,
            price,
            tax_rate: rate,
            amount,
            tax_amount,
            remark: None,
        }
    }

    fn new_quotation(customer_id: Uuid, remark: &str) -> NewQuotation<'_> {
        let today = today_cst(Utc::now());
        NewQuotation {
            customer_id,
            quote_date: today,
            valid_until: today + nik::prelude::chrono::Duration::days(14),
            remark: Some(remark),
        }
    }

    /// 建单：取号 → 单头 DRAFT → 明细行号从 1 起 → 金额三项恒等 → 建单日志一行。
    #[tokio::test]
    async fn create_quotation_persists_header_lines_and_log() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![line("3", "17.655", "13"), line("2", "100", "0")];
        let totals = Totals::of(&lines);
        let operator = Uuid::now_v7();
        let tx = db().begin().await.expect("开事务");
        let (id, quote_no) = create_quotation(
            &tx,
            &new_quotation(customer, "M3-02 建单"),
            &lines,
            &totals,
            state_machine::Operator::of(Some(operator)),
            Utc::now(),
        )
        .await
        .expect("建单应成功");
        tx.commit().await.expect("提交");

        assert!(quote_no.starts_with("QT-"), "单号前缀必须是 QT-（实际 {quote_no}）");
        assert_eq!(quote_no.len(), "QT-yyyyMMdd-000001".len(), "6 位补零：{quote_no}");
        let row = find_quotation(id).await.expect("查单应成功").expect("单头必须存在");
        assert_eq!(row.status, STATUS_DRAFT);
        assert_eq!(row.quote_no, quote_no);
        assert_eq!(row.total_amount, dec("252.9650"));
        assert_eq!(row.total_tax, dec("6.8855"));
        assert_eq!(row.total_with_tax, dec("259.8505"));
        assert!(row.converted_so_id.is_none());
        let stored = lines_of(id).await.expect("查明细应成功");
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].line_no, 1);
        assert_eq!(stored[1].line_no, 2);
        assert_eq!(stored[0].with_tax(), dec("52.9650") + dec("6.8855"));

        // 建单日志：from_status = NULL → DRAFT，操作人快照
        let logged: i64 = db()
            .scalar_with_params(
                "SELECT count(*) FROM biz_status_log \
                  WHERE entity_type = 'QUOTATION' AND entity_id = ?::uuid \
                    AND from_status IS NULL AND to_status = 'DRAFT'",
                vec![Value::from(&id)],
            )
            .await
            .expect("查状态日志应成功");
        assert_eq!(logged, 1, "建单必须留一行 from_status = NULL 的日志");
    }

    /// 取号连续：同一 CST 日的两次取号流水 +1。
    #[tokio::test]
    async fn next_quote_no_is_consecutive_on_real_db() {
        let Some(_guard) = setup().await else { return };
        let now = Utc::now();
        let first = next_quote_no(db(), now).await.expect("第一次取号应成功");
        let second = next_quote_no(db(), now).await.expect("第二次取号应成功");
        let (period_a, no_a) = parse_quote_no(&first).expect("取出的号必须能拆解");
        let (period_b, no_b) = parse_quote_no(&second).expect("取出的号必须能拆解");
        assert_eq!(period_a, period_key::period_key_of(now), "周期键必须是北京时间的今天");
        assert_eq!(period_b, period_a);
        assert_eq!(no_b, no_a + 1, "同一天流水必须 +1（{first} → {second}）");
        println!("[M3-02] 取号实况：{first} → {second}（period_key={period_a}）");
    }

    /// 改单：明细整表替换；**非草稿一律改不动**（CAS 返回 0）。
    #[tokio::test]
    async fn update_quotation_replaces_lines_and_cas_on_draft() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![line("1", "10", "13")];
        let totals = Totals::of(&lines);
        let tx = db().begin().await.expect("开事务");
        let (id, _no) = create_quotation(
            &tx,
            &new_quotation(customer, "首版"),
            &lines,
            &totals,
            state_machine::Operator::of(None),
            Utc::now(),
        )
        .await
        .expect("建单应成功");
        tx.commit().await.expect("提交");

        // 草稿：改得动，明细被整表替换
        let new_lines = vec![line("4", "25", "6"), line("5", "7.5", "13")];
        let new_totals = Totals::of(&new_lines);
        let affected = update_quotation(
            db(),
            UpdateQuotationInput {
                id,
                new: &new_quotation(customer, "第二版"),
                lines: &new_lines,
                totals: &new_totals,
                operator: None,
                now: Utc::now(),
            },
        )
        .await
        .expect("改单应成功");
        assert_eq!(affected, 1);
        let stored = lines_of(id).await.expect("查明细应成功");
        assert_eq!(stored.len(), 2, "旧明细必须被整表替换");
        assert_eq!(stored[1].qty, dec("5"));
        let row = find_quotation(id).await.expect("查单应成功").expect("单头存在");
        assert_eq!(row.total_with_tax, new_totals.with_tax);
        assert_eq!(row.remark.as_deref(), Some("第二版"));

        // 推到 SENT 之后再改：CAS 返回 0（且明细不动）
        apply_status(db(), id, STATUS_DRAFT, STATUS_SENT, None, Utc::now())
            .await
            .expect("推状态应成功");
        let affected = update_quotation(
            db(),
            UpdateQuotationInput {
                id,
                new: &new_quotation(customer, "第三版"),
                lines: &lines,
                totals: &totals,
                operator: None,
                now: Utc::now(),
            },
        )
        .await
        .expect("改单调用本身应成功（只是没命中行）");
        assert_eq!(affected, 0, "非草稿必须改不动");
        let stored = lines_of(id).await.expect("查明细应成功");
        assert_eq!(stored.len(), 2, "没命中行时不许动明细");
        assert_eq!(stored[1].qty, dec("5"));
    }

    /// 状态迁移是 CAS：from 不符返回 0。
    #[tokio::test]
    async fn apply_status_is_compare_and_swap() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![line("1", "10", "13")];
        let totals = Totals::of(&lines);
        let tx = db().begin().await.expect("开事务");
        let (id, _no) = create_quotation(
            &tx,
            &new_quotation(customer, "CAS"),
            &lines,
            &totals,
            state_machine::Operator::of(None),
            Utc::now(),
        )
        .await
        .expect("建单应成功");
        tx.commit().await.expect("提交");

        // 期望 DRAFT → SENT 但库里是 DRAFT：成功
        assert_eq!(
            apply_status(db(), id, STATUS_DRAFT, STATUS_SENT, None, Utc::now())
                .await
                .expect("迁移应成功"),
            1
        );
        // 再拿旧的 from 推一次：0（被别人推走了）
        assert_eq!(
            apply_status(db(), id, STATUS_DRAFT, STATUS_SENT, None, Utc::now())
                .await
                .expect("重复迁移调用本身应成功"),
            0
        );
        assert_eq!(
            find_quotation(id).await.expect("查单").expect("存在").status,
            STATUS_SENT
        );
        // SENT → ACCEPTED
        assert_eq!(
            apply_status(db(), id, STATUS_SENT, STATUS_ACCEPTED, None, Utc::now())
                .await
                .expect("迁移应成功"),
            1
        );
    }

    /// 软删只对 DRAFT / CANCELLED 生效；SENT 与域外状态一律 0 行。
    #[tokio::test]
    async fn soft_delete_only_from_deletable_statuses() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![line("1", "10", "0")];
        let totals = Totals::of(&lines);
        let mut ids = Vec::new();
        for remark in ["draft", "sent"] {
            let tx = db().begin().await.expect("开事务");
            let (id, _no) = create_quotation(
                &tx,
                &new_quotation(customer, remark),
                &lines,
                &totals,
                state_machine::Operator::of(None),
                Utc::now(),
            )
            .await
            .expect("建单应成功");
            tx.commit().await.expect("提交");
            ids.push(id);
        }
        apply_status(db(), ids[1], STATUS_DRAFT, STATUS_SENT, None, Utc::now())
            .await
            .expect("推状态应成功");

        // SENT：不许软删
        assert_eq!(
            soft_delete(db(), ids[1], STATUS_SENT, None, Utc::now())
                .await
                .expect("软删调用本身应成功"),
            0
        );
        assert!(find_quotation(ids[1]).await.expect("查单").is_some());
        // DRAFT：可以软删，删后查不到（软删过滤）
        assert_eq!(
            soft_delete(db(), ids[0], STATUS_DRAFT, None, Utc::now())
                .await
                .expect("软删应成功"),
            1
        );
        assert!(
            find_quotation(ids[0]).await.expect("查单").is_none(),
            "软删后必须查不到"
        );
        // 明细保留（软删不物理删，历史可回看）
        assert_eq!(lines_of(ids[0]).await.expect("查明细").len(), 1);
    }

    /// 分页筛选：单号模糊 / 状态 / 客户 / 有效期（过期现算）。
    #[tokio::test]
    async fn page_filters_quote_no_status_customer_and_expiry() {
        let Some(_guard) = setup().await else { return };
        let customer = fixture_customer().await;
        let lines = vec![line("1", "10", "0")];
        let totals = Totals::of(&lines);
        let today = today_cst(Utc::now());
        // 一张未过期（有效 14 天），一张已过期（有效期 = 报价日 = 昨天）
        let mut ids = Vec::new();
        let mut nos = Vec::new();
        for (index, (quote_date, valid_until)) in [
            (today, today + nik::prelude::chrono::Duration::days(14)),
            (
                today - nik::prelude::chrono::Duration::days(10),
                today - nik::prelude::chrono::Duration::days(1),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let tx = db().begin().await.expect("开事务");
            let new = NewQuotation {
                customer_id: customer,
                quote_date,
                valid_until,
                remark: Some(if index == 0 { "未过期" } else { "已过期" }),
            };
            let (id, no) = create_quotation(
                &tx,
                &new,
                &lines,
                &totals,
                state_machine::Operator::of(None),
                Utc::now(),
            )
            .await
            .expect("建单应成功");
            tx.commit().await.expect("提交");
            ids.push(id);
            nos.push(no);
        }
        apply_status(db(), ids[1], STATUS_DRAFT, STATUS_SENT, None, Utc::now())
            .await
            .expect("推状态应成功");

        // 全部（按客户筛）
        let (rows, total) = quotation_page(&QuotationPageQuery {
            customer_id: Some(customer),
            today,
            page: 1,
            page_size: 20,
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(total, 2, "夹具客户下应有 2 张");
        assert_eq!(rows.len(), 2);

        // 状态筛
        let (rows, total) = quotation_page(&QuotationPageQuery {
            customer_id: Some(customer),
            status: Some(STATUS_SENT),
            today,
            page: 1,
            page_size: 20,
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(total, 1);
        assert_eq!(rows[0].quote_no, nos[1]);

        // 单号模糊
        let (_, total) = quotation_page(&QuotationPageQuery {
            quote_no: Some(&nos[0]),
            today,
            page: 1,
            page_size: 20,
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(total, 1, "单号精确片段应命中 1 张");

        // 已过期 / 未过期（现算）
        let (rows, total) = quotation_page(&QuotationPageQuery {
            customer_id: Some(customer),
            expired: Some(true),
            today,
            page: 1,
            page_size: 20,
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(total, 1);
        assert_eq!(rows[0].quote_no, nos[1]);
        assert!(rows[0].expired_on(today));

        let (rows, total) = quotation_page(&QuotationPageQuery {
            customer_id: Some(customer),
            expired: Some(false),
            today,
            page: 1,
            page_size: 20,
            ..Default::default()
        })
        .await
        .expect("分页应成功");
        assert_eq!(total, 1);
        assert_eq!(rows[0].quote_no, nos[0]);
        assert!(!rows[0].expired_on(today));
    }
}

//! 币种 / 汇率维护数据层（销售链 · SO-42 / ARCH-7 §15.6）：`biz_currency` + `biz_exchange_rate` 的 CRUD。
//!
//! 表结构唯一依据：`backend/migrations/business/V0272__sales_currency.up.sql`
//! （`biz_currency` 11 列 / `biz_exchange_rate` 11 列）。**取数口径**（本位币 / SPOT / 建单锁定）
//! 在 [`crate::business::erp::currency`]，本文件只管**主数据维护**（写口径）；
//! 两者共用 `currency::normalize_currency` / `currency::scale_rate`，避免「维护一套、取数另一套」。
//!
//! # 五条与 DDL 对齐的铁律
//!
//! 1. **币种不软删**：`biz_currency` 没有 `deleted_at`（V0272 有意为之 —— 汇率与单据历史按
//!    `code` 引用它，物理删会毁掉历史可读性）。「停用」用 `status = 'INACTIVE'`；
//!    停用后新单据不可选，但历史单据与汇率行照旧可查可对账。
//! 2. **本位币唯一**：`uk_biz_currency_single_base`（`ON (is_base) WHERE is_base`）是库层硬约束。
//!    本层的 [`currency_set_base`] 在**同一事务**里先清旧再立新（两条 UPDATE，不靠单语句
//!    的行序碰运气）；本位币行**不允许**停用（[`currency_update`] 之前由 handler 拦住），
//!    因为 [`currency::base_currency_code`] 只认 `is_base AND status = 'ACTIVE'`。
//! 3. **汇率物理删**：`biz_exchange_rate` 没有 `deleted_at`，录错就 `DELETE`（重录走 upsert）。
//! 4. **方向 `from → to`，只存「外币 → 本位币」**：`ck_biz_exchange_rate_pair` 禁自反汇率；
//!    反向要 `1 / rate` 推导，不落重复行（两行互为倒数必然因四舍五入对不上，是假账源头）。
//! 5. **`effective_date` 必填**（DDL 无 DEFAULT）：月度批量录入取**月末日**，
//!    取汇率 = `effective_date <= 目标日 ORDER BY effective_date DESC LIMIT 1`（含当日）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_currency / biz_exchange_rate 是全局主数据（V0272 无 dept_id / 归属人列），
//! 数据范围表达不出来；可见性由 business:erp:currency:* / business:erp:fx-rate:* 权限码控制。
//! 分页走 model_base::page_of → scope::paginate_unscoped（闸①口径）。
//! ```

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::currency;
use crate::business::erp::model_base::page_of;
use crate::business::shared::scope;
use crate::utils;

/// 分页口径复用底座（默认 20 / 上限 200 / `page_size = 0` 回落）。
pub use crate::business::erp::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};

// ==================== 常量 ====================

/// 状态域值：启用（新单据可选）。
pub const STATUS_ACTIVE: &str = "ACTIVE";
/// 状态域值：停用（历史单据与汇率行保留）。
pub const STATUS_INACTIVE: &str = "INACTIVE";
/// 状态域白名单（与 `ck_biz_currency_status` 逐字一致）。
pub const STATUSES: [&str; 2] = [STATUS_ACTIVE, STATUS_INACTIVE];

/// 汇率类型：即时汇率（建单锁定用这个）。
pub const RATE_TYPE_SPOT: &str = "SPOT";
/// 汇率类型：月末汇率（期末重估用，SO-41 `fx_revaluation`）。
pub const RATE_TYPE_MONTH_END: &str = "MONTH_END";
/// 汇率类型白名单（与 `ck_biz_exchange_rate_type` 逐字一致）。
pub const RATE_TYPES: [&str; 2] = [RATE_TYPE_SPOT, RATE_TYPE_MONTH_END];

/// 币种名长度上限（`biz_currency.name VARCHAR(64)`）。
pub const NAME_MAX_LEN: usize = 64;
/// 币种符号长度上限（`biz_currency.symbol VARCHAR(8)`，`HK$` 只有 3 字符）。
pub const SYMBOL_MAX_LEN: usize = 8;
/// 小数位上限（`ck_biz_currency_decimal_places BETWEEN 0 AND 4`）。
pub const DECIMAL_PLACES_MAX: i32 = 4;
/// 一次月度批量录入最多几行（4 个币种 × 12 个月也该拆；50 是「一个月全量」的上限）。
pub const BATCH_MAX_ROWS: usize = 50;
/// 备注长度上限（`remark TEXT`，但一次录入的备注不该是文章）。
pub const REMARK_MAX_LEN: usize = 200;

// ==================== 纯逻辑（可单测） ====================

/// 币种码规范化：去空格 + 转大写 + `^[A-Z]{3}$`（委托 [`currency::normalize_currency`]，
/// 与建单入口同一把尺：**维护侧能建出来的码，开单侧一定认**）。
pub fn normalize_code(raw: &str) -> Option<String> {
    currency::normalize_currency(raw)
}

/// 状态域校验（缺省口径由 handler 给，这里只判值）。
pub fn is_valid_status(value: &str) -> bool {
    STATUSES.contains(&value)
}

/// 汇率类型域校验。
pub fn is_valid_rate_type(value: &str) -> bool {
    RATE_TYPES.contains(&value)
}

/// 小数位是否合法（`0..=4`；0 = JPY/KRW，3 = KWD/BHD）。
pub fn decimal_places_allowed(value: i32) -> bool {
    (0..=DECIMAL_PLACES_MAX).contains(&value)
}

/// 名称是否可用：非空且 ≤ 64 **字符**（`VARCHAR(64)` 按字符计，中文 1 字 1 位）。
pub fn name_allowed(value: &str) -> bool {
    let text = value.trim();
    !text.is_empty() && text.chars().count() <= NAME_MAX_LEN
}

/// 符号是否可用：非空且 ≤ 8 字符。
pub fn symbol_allowed(value: &str) -> bool {
    let text = value.trim();
    !text.is_empty() && text.chars().count() <= SYMBOL_MAX_LEN
}

/// 汇率是否可用：**严格 > 0**（`ck_biz_exchange_rate_rate`）。
///
/// `0` 会把整单折成 0 元、负数会造出反向应收，两者都比「没录汇率」危险得多 ——
/// 外币单取不到汇率时建单入口会 4xx 拒绝，绝不肯按 1 或 0 入账。
pub fn rate_allowed(value: Decimal) -> bool {
    value > Decimal::ZERO
}

/// 汇率定标（8 位小数，四舍五入）：落库前统一，避免 `numeric(18,8)` 静默截断。
pub fn scale_rate(value: Decimal) -> Decimal {
    currency::scale_rate(value)
}

/// 是否自反汇率（`from == to`，DDL 的 `ck_biz_exchange_rate_pair` 会拒）。
pub fn is_self_pair(from: &str, to: &str) -> bool {
    from == to
}

/// 月末日：`2026-02 → 2026-02-28`（闰年 `2024-02 → 2024-02-29`）。
///
/// 月度批量录入的 `effective_date` 取它（月末重估按「月末那天」取 `MONTH_END` 汇率）。
pub fn month_end(year: i32, month: u32) -> Option<NaiveDate> {
    if !(1..=12).contains(&month) {
        return None;
    }
    // 下个月 1 号减 1 天：不写 28/30/31 的天数表，闰年自动正确
    let (next_year, next_month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    NaiveDate::from_ymd_opt(next_year, next_month, 1).and_then(|date| date.pred_opt())
}

/// `"2026-11"` → `(2026, 11)`；格式不对返回 `None`（handler 转 400，不猜）。
pub fn parse_month(raw: &str) -> Option<(i32, u32)> {
    let text = raw.trim();
    let (year_text, month_text) = text.split_once('-')?;
    if year_text.len() != 4 || month_text.len() != 2 {
        return None;
    }
    let year: i32 = year_text.parse().ok()?;
    let month: u32 = month_text.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((year, month))
}

/// `"2026-11"` → 月末生效日 `2026-11-30`。
pub fn month_end_of(raw: &str) -> Option<NaiveDate> {
    let (year, month) = parse_month(raw)?;
    month_end(year, month)
}

// ==================== 行结构 ====================

/// `biz_currency` 一行（**带汇率条数**：页面上直接显示「该币种录了 N 条汇率」）。
#[derive(Debug, Clone)]
pub struct CurrencyRow {
    /// 币种码（PK，三位大写）。
    pub code: String,
    /// 中文名称。
    pub name: String,
    /// 符号。
    pub symbol: String,
    /// 小数位。
    pub decimal_places: i32,
    /// 是否本位币。
    pub is_base: bool,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 该币种作为 `from_currency` 的汇率条数（删除/停用前的风险提示用）。
    pub rate_count: i64,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl CurrencyRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            code: row.get_typed("code")?,
            name: row.get_typed("name")?,
            symbol: row.get_typed("symbol")?,
            decimal_places: row.get_typed("decimal_places")?,
            is_base: row.get_typed("is_base")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            rate_count: row.get_typed("rate_count")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `biz_exchange_rate` 一行。
#[derive(Debug, Clone)]
pub struct RateRow {
    /// 主键。
    pub id: Uuid,
    /// 原币。
    pub from_currency: String,
    /// 目标币种（业务上 = 本位币）。
    pub to_currency: String,
    /// `1 from = rate to`。
    pub rate: Decimal,
    /// `SPOT` / `MONTH_END`。
    pub rate_type: String,
    /// 生效日（含）。
    pub effective_date: NaiveDate,
    /// 备注（来源：中国银行 / 央行中间价 …）。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl RateRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            from_currency: row.get_typed("from_currency")?,
            to_currency: row.get_typed("to_currency")?,
            rate: row.get_typed("rate")?,
            rate_type: row.get_typed("rate_type")?,
            effective_date: row.get_typed("effective_date")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 币种写入值（新建 / 编辑共用；`code` 只在新建时有意义）。
#[derive(Debug, Clone)]
pub struct CurrencyWrite {
    /// 币种码（规范化后的三位大写）。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 符号。
    pub symbol: String,
    /// 小数位。
    pub decimal_places: i32,
    /// 状态。
    pub status: String,
    /// 备注（空串已折成 `None`）。
    pub remark: Option<String>,
}

/// 汇率写入值。
#[derive(Debug, Clone)]
pub struct RateWrite {
    /// 原币。
    pub from_currency: String,
    /// 目标币种。
    pub to_currency: String,
    /// 汇率（已定标 8 位）。
    pub rate: Decimal,
    /// `SPOT` / `MONTH_END`。
    pub rate_type: String,
    /// 生效日。
    pub effective_date: NaiveDate,
    /// 备注。
    pub remark: Option<String>,
}

/// `remark` 空串一律落 `NULL`（`''` 与 `NULL` 两种「没备注」不该并存）。
pub fn normalize_remark(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 可选审计人 → 绑参文本（`NULL` 走 `NULLIF(?::text,'')::uuid`）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

// ==================== 查询 ====================

/// 币种查询列（带汇率条数子查询：主页面上要看得出「这个币种有没有汇率」）。
const CURRENCY_SELECT: &str = "SELECT c.code, c.name, c.symbol, c.decimal_places, c.is_base, \
     c.status, c.remark, c.created_at, c.updated_at, \
     (SELECT count(*) FROM biz_exchange_rate r WHERE r.from_currency = c.code) AS rate_count \
     FROM biz_currency c";

/// 汇率查询列。
const RATE_SELECT: &str = "SELECT r.id, r.from_currency, r.to_currency, r.rate, r.rate_type, \
     r.effective_date, r.remark, r.created_at, r.updated_at FROM biz_exchange_rate r";

/// 币种列表排序：本位币置顶 → 码升序（`false < true`，故 DESC 让 `true` 在前）。
const CURRENCY_ORDER: &str = "ORDER BY c.is_base DESC, c.code ASC";

/// 汇率列表排序：生效日倒序 → 原币升序（最近录的排最前，同一天按币种码扫）。
const RATE_ORDER: &str = "ORDER BY r.effective_date DESC, r.from_currency ASC, r.rate_type ASC, r.id ASC";

/// 币种全量列表（4~10 行的小主数据，**不分页**：一次给全，前端 filterable 下拉直接用）。
///
/// `status` 精确筛选（非法值由 handler 先判 400）；`keyword` 命中码 / 名称（模糊）。
pub async fn currency_list(status: Option<&str>, keyword: Option<&str>) -> DbResult<Vec<CurrencyRow>> {
    let mut sql = format!("{CURRENCY_SELECT} WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = status.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND c.status = ?");
        params.push(Value::from(status));
    }
    if let Some(pattern) = utils::like_pattern(keyword) {
        sql.push_str(" AND (c.code LIKE ? OR c.name LIKE ?)");
        params.push(Value::from(pattern.clone()));
        params.push(Value::from(pattern));
    }
    sql.push(' ');
    sql.push_str(CURRENCY_ORDER);
    let rows = db().query_sql(sql, params).await?;
    rows.iter().map(CurrencyRow::from_row).collect()
}

/// 按码取一行（不存在 → `None`）。
pub async fn currency_find(code: &str) -> DbResult<Option<CurrencyRow>> {
    let rows = db()
        .query_sql(format!("{CURRENCY_SELECT} WHERE c.code = ?"), vec![Value::from(code)])
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(CurrencyRow::from_row(row)?)),
    }
}

/// 新建币种（码已存在时由 handler 先判 409；索引/主键冲突是并发兜底）。
pub async fn currency_insert(write: &CurrencyWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "INSERT INTO biz_currency \
             (code, name, symbol, decimal_places, is_base, status, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?, ?, ?, ?, false, ?, ?, now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(write.code.clone()),
            Value::from(write.name.clone()),
            Value::from(write.symbol.clone()),
            Value::from(write.decimal_places),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 编辑币种（`code` 是主键，不可改；`is_base` 走 [`currency_set_base`] 单独一条路）。
pub async fn currency_update(code: &str, write: &CurrencyWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_currency SET name = ?, symbol = ?, decimal_places = ?, status = ?, \
             remark = ?, updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE code = ?",
        vec![
            Value::from(write.name.clone()),
            Value::from(write.symbol.clone()),
            Value::from(write.decimal_places),
            Value::from(write.status.clone()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            Value::from(code),
        ],
    )
    .await
}

/// 立本位币：**同一事务**里先清掉旧本位币、再立新的。
///
/// 为什么分两条 UPDATE 而不是 `SET is_base = (code = $1)`：部分唯一索引
/// `uk_biz_currency_single_base` 只收 `is_base = true` 的行，单语句多行更新要依赖
/// 引擎对「同命令内行序」的处理；两条语句顺序固定（先腾位、再落座），行为可预期，
/// 也不会出现「旧本位币还没清、新本位币已立」的中间态被别的连接看见。
pub async fn currency_set_base(code: &str, operator: Option<Uuid>) -> DbResult<u64> {
    let tx = db().begin().await?;
    let cleared = tx
        .execute_with_params(
            "UPDATE biz_currency SET is_base = false, updated_at = now(), \
                 update_by = NULLIF(?::text,'')::uuid \
             WHERE is_base AND code <> ?",
            vec![uuid_text(operator), Value::from(code)],
        )
        .await?;
    let promoted = tx
        .execute_with_params(
            "UPDATE biz_currency SET is_base = true, updated_at = now(), \
                 update_by = NULLIF(?::text,'')::uuid \
             WHERE code = ?",
            vec![uuid_text(operator), Value::from(code)],
        )
        .await?;
    tx.commit().await?;
    Ok(cleared + promoted)
}

/// 当前本位币码（`None` = 库里一行本位币都没有，账套配置缺失）。
pub async fn current_base_code() -> DbResult<Option<String>> {
    let row = db()
        .query_first_sql(
            "SELECT code FROM biz_currency WHERE is_base LIMIT 1".to_string(),
            vec![],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed::<String>("code")?)),
    }
}

/// 币种是否被汇率引用（`from` 或 `to` 任一侧）。
pub async fn currency_rate_count(code: &str) -> DbResult<i64> {
    let row = db()
        .query_first_sql(
            "SELECT count(*) AS n FROM biz_exchange_rate \
              WHERE from_currency = ? OR to_currency = ?"
                .to_string(),
            vec![Value::from(code), Value::from(code)],
        )
        .await?;
    match row {
        None => Ok(0),
        Some(row) => row.get_typed::<i64>("n"),
    }
}

/// 汇率分页（`from_currency` / `to_currency` / `rate_type` 精确，`date_from` / `date_to` 为生效日闭区间）。
pub async fn rate_page(
    from_currency: Option<&str>,
    to_currency: Option<&str>,
    rate_type: Option<&str>,
    date_from: Option<NaiveDate>,
    date_to: Option<NaiveDate>,
    page: u64,
    page_size: u64,
) -> DbResult<(Vec<RateRow>, i64)> {
    let mut sql = format!("{RATE_SELECT} WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(code) = from_currency.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND r.from_currency = ?");
        params.push(Value::from(code));
    }
    if let Some(code) = to_currency.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND r.to_currency = ?");
        params.push(Value::from(code));
    }
    if let Some(kind) = rate_type.map(str::trim).filter(|text| !text.is_empty()) {
        sql.push_str(" AND r.rate_type = ?");
        params.push(Value::from(kind));
    }
    if let Some(date) = date_from {
        sql.push_str(" AND r.effective_date >= ?::text::date");
        params.push(Value::Text(date.to_string()));
    }
    if let Some(date) = date_to {
        sql.push_str(" AND r.effective_date <= ?::text::date");
        params.push(Value::Text(date.to_string()));
    }
    page_of(
        scope::ScopeTarget::unscoped("biz_exchange_rate"),
        sql,
        params,
        RATE_ORDER,
        page,
        page_size,
        RateRow::from_row,
    )
    .await
}

/// 按 id 取汇率行。
pub async fn rate_find(id: Uuid) -> DbResult<Option<RateRow>> {
    let rows = db()
        .query_sql(format!("{RATE_SELECT} WHERE r.id = ?::uuid"), vec![Value::from(&id)])
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(RateRow::from_row(row)?)),
    }
}

/// 同一组键（币对 + 类型 + 生效日）是否已存在 —— 新建前查一次，给人话 409。
pub async fn rate_taken(
    from_currency: &str,
    to_currency: &str,
    rate_type: &str,
    effective_date: NaiveDate,
) -> DbResult<Option<Uuid>> {
    let row = db()
        .query_first_sql(
            "SELECT id FROM biz_exchange_rate \
              WHERE from_currency = ? AND to_currency = ? AND rate_type = ? \
                AND effective_date = ?::text::date LIMIT 1"
                .to_string(),
            vec![
                Value::from(from_currency),
                Value::from(to_currency),
                Value::from(rate_type),
                Value::Text(effective_date.to_string()),
            ],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed::<Uuid>("id")?)),
    }
}

/// 新建汇率行（`from` / `to` 必须已存在：库层 FK 兜底，handler 先查给人话）。
pub async fn rate_insert(id: Uuid, write: &RateWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "INSERT INTO biz_exchange_rate \
             (id, from_currency, to_currency, rate, rate_type, effective_date, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?::text::numeric, ?, ?::text::date, ?, now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)",
        vec![
            Value::from(&id),
            Value::from(write.from_currency.clone()),
            Value::from(write.to_currency.clone()),
            Value::Text(write.rate.to_string()),
            Value::from(write.rate_type.clone()),
            Value::Text(write.effective_date.to_string()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 整行覆盖编辑。
pub async fn rate_update(id: Uuid, write: &RateWrite, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        "UPDATE biz_exchange_rate SET from_currency = ?, to_currency = ?, \
             rate = ?::text::numeric, rate_type = ?, effective_date = ?::text::date, \
             remark = ?, updated_at = now(), update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid",
        vec![
            Value::from(write.from_currency.clone()),
            Value::from(write.to_currency.clone()),
            Value::Text(write.rate.to_string()),
            Value::from(write.rate_type.clone()),
            Value::Text(write.effective_date.to_string()),
            Value::from(write.remark.clone()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 物理删汇率行（该表无软删列；录错就该消失，否则「取最近一条」会被错行污染）。
pub async fn rate_delete(id: Uuid) -> DbResult<u64> {
    db().execute_with_params(
        "DELETE FROM biz_exchange_rate WHERE id = ?::uuid",
        vec![Value::from(&id)],
    )
    .await
}

/// 覆盖写入（月度批量录入）：同币对 / 同类型 / 同生效日已存在则**改值不改行**。
///
/// `ON CONFLICT … DO UPDATE` 命中 `uk_biz_exchange_rate` 的四个列（V0272 头注释 ②.5 已
/// 按这个写法约定索引列序）。返回 `true` = 新插、`false` = 更新既有行。
pub async fn rate_upsert(write: &RateWrite, operator: Option<Uuid>) -> DbResult<bool> {
    let affected = db()
        .execute_with_params(
            "INSERT INTO biz_exchange_rate \
                 (id, from_currency, to_currency, rate, rate_type, effective_date, remark, \
                  created_at, updated_at, create_by, update_by) \
             VALUES (gen_random_uuid(), ?, ?, ?::text::numeric, ?, ?::text::date, ?, \
                     now(), now(), NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid) \
             ON CONFLICT (from_currency, to_currency, rate_type, effective_date) DO UPDATE \
                SET rate = EXCLUDED.rate, \
                    remark = COALESCE(EXCLUDED.remark, biz_exchange_rate.remark), \
                    updated_at = now(), \
                    update_by = EXCLUDED.update_by",
            vec![
                Value::from(write.from_currency.clone()),
                Value::from(write.to_currency.clone()),
                Value::Text(write.rate.to_string()),
                Value::from(write.rate_type.clone()),
                Value::Text(write.effective_date.to_string()),
                Value::from(write.remark.clone()),
                uuid_text(operator),
                uuid_text(operator),
            ],
        )
        .await?;
    Ok(affected > 0)
}

/// 某币种在某日期（含）之前最近一条 SPOT 汇率（**预览用**，取数口径委托
/// [`currency::spot_rate`]，与建单锁定同一条 SQL）。
pub async fn spot_preview(code: &str, on_date: NaiveDate) -> DbResult<Option<Decimal>> {
    let base = currency::base_currency_code(db()).await?;
    if base == code {
        return Ok(Some(Decimal::ONE));
    }
    currency::spot_rate(db(), code, &base, on_date).await
}

/// 取到的那条 SPOT 汇率行：`(生效日, 汇率)`（预览界面要显示「用的是哪天的牌价」）。
pub async fn spot_row(
    from_currency: &str,
    to_currency: &str,
    on_date: NaiveDate,
) -> DbResult<Option<(NaiveDate, Decimal)>> {
    let row = db()
        .query_first_sql(
            "SELECT effective_date, rate FROM biz_exchange_rate \
              WHERE from_currency = ? AND to_currency = ? AND rate_type = ? \
                AND effective_date <= ?::text::date \
              ORDER BY effective_date DESC LIMIT 1"
                .to_string(),
            vec![
                Value::from(from_currency),
                Value::from(to_currency),
                Value::from(RATE_TYPE_SPOT),
                Value::Text(on_date.to_string()),
            ],
        )
        .await?;
    match row {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get_typed::<NaiveDate>("effective_date")?,
            row.get_typed::<Decimal>("rate")?,
        ))),
    }
}

// ==================== 单测（纯逻辑；真库行为由 curl 实测留证） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("固定日期合法")
    }

    fn dec(raw: &str) -> Decimal {
        raw.parse().expect("测试用十进制字面量合法")
    }

    // ---------- 币种码 ----------

    #[test]
    fn normalize_code_uppercases_and_trims() {
        assert_eq!(normalize_code(" usd ").as_deref(), Some("USD"));
        assert_eq!(normalize_code("cny").as_deref(), Some("CNY"));
    }

    #[test]
    fn normalize_code_rejects_wrong_length_or_digits() {
        assert_eq!(normalize_code("US"), None);
        assert_eq!(normalize_code("USDD"), None);
        assert_eq!(normalize_code("U1D"), None);
        assert_eq!(normalize_code(""), None);
    }

    #[test]
    fn name_and_symbol_bounds() {
        assert!(name_allowed("美元"));
        assert!(!name_allowed("   "));
        assert!(!name_allowed(&"字".repeat(NAME_MAX_LEN + 1)));
        assert!(name_allowed(&"字".repeat(NAME_MAX_LEN)));

        assert!(symbol_allowed("HK$"));
        assert!(!symbol_allowed(""));
        assert!(!symbol_allowed(&"$".repeat(SYMBOL_MAX_LEN + 1)));
    }

    #[test]
    fn status_and_rate_type_domains_are_closed() {
        assert!(is_valid_status("ACTIVE"));
        assert!(is_valid_status("INACTIVE"));
        assert!(!is_valid_status("active"));
        assert!(!is_valid_status("ENABLED"));

        assert!(is_valid_rate_type("SPOT"));
        assert!(is_valid_rate_type("MONTH_END"));
        assert!(!is_valid_rate_type("MONTHEND"));
    }

    #[test]
    fn decimal_places_within_zero_and_four() {
        assert!(decimal_places_allowed(0));
        assert!(decimal_places_allowed(2));
        assert!(decimal_places_allowed(4));
        assert!(!decimal_places_allowed(-1));
        assert!(!decimal_places_allowed(5));
    }

    // ---------- 汇率值 ----------

    #[test]
    fn rate_must_be_positive() {
        assert!(rate_allowed(dec("7.1")));
        assert!(rate_allowed(dec("0.00000001")));
        assert!(!rate_allowed(Decimal::ZERO));
        assert!(!rate_allowed(dec("-7.1")));
    }

    #[test]
    fn rate_is_scaled_to_eight_places() {
        // 超过 8 位要按四舍五入收敛（numeric(18,8) 落库前必须定标，否则驱动静默截断）；
        // 不足 8 位**不补零**（Decimal 保持原 scale，PG 的 numeric(18,8) 自己补）
        assert_eq!(scale_rate(dec("7.123456789")), dec("7.12345679"));
        assert_eq!(scale_rate(dec("7.1")), dec("7.1"));
        assert!(!scale_rate(dec("7.123456789")).to_string().contains("7.123456789"));
    }

    #[test]
    fn self_pair_detected() {
        assert!(is_self_pair("USD", "USD"));
        assert!(!is_self_pair("USD", "CNY"));
    }

    // ---------- 月份 ----------

    #[test]
    fn parse_month_requires_yyyy_dash_mm() {
        assert_eq!(parse_month("2026-11"), Some((2026, 11)));
        assert_eq!(parse_month(" 2026-01 "), Some((2026, 1)));
        assert_eq!(parse_month("2026-1"), None);
        assert_eq!(parse_month("2026-13"), None);
        assert_eq!(parse_month("2026/11"), None);
        assert_eq!(parse_month("26-11"), None);
    }

    #[test]
    fn month_end_handles_leap_year_and_year_rollover() {
        assert_eq!(month_end_of("2026-02"), Some(date(2026, 2, 28)));
        assert_eq!(month_end_of("2024-02"), Some(date(2024, 2, 29)));
        assert_eq!(month_end_of("2026-04"), Some(date(2026, 4, 30)));
        assert_eq!(month_end_of("2026-12"), Some(date(2026, 12, 31)));
        assert_eq!(month_end_of("2026-13"), None);
    }

    #[test]
    fn remark_blank_becomes_none() {
        assert_eq!(
            normalize_remark(Some("  中国银行".to_string())).as_deref(),
            Some("中国银行")
        );
        assert_eq!(normalize_remark(Some("   ".to_string())), None);
        assert_eq!(normalize_remark(None), None);
    }

    #[test]
    fn page_limits_are_sane() {
        assert_eq!(normalize_page(0, 0), (1, DEFAULT_PAGE_SIZE));
        assert_eq!(normalize_page(3, 500), (3, MAX_PAGE_SIZE));
    }
}

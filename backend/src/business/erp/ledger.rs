//! 总账（流水账）+ 科目余额表（批次 D · P2-D1）：**只读**。
//!
//! 表结构以 V0149（`biz_voucher` / `biz_voucher_entry`）、V0137（`biz_account`）、
//! V0137（`biz_accounting_period`）为准，2026-10-31 开工 `\d` 实测。
//!
//! # 接口（卡面 §二 ③④）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/erp/ledger/page` | `business:erp:ledger:list` | 某科目的分录流水 + **逐行余额**（`account_id` 或 `account_code` 必填） |
//! | GET | `/business/erp/ledger/trial-balance` | `business:erp:ledger:trial-balance` | 科目余额表（`period` 必填，★ Σ借 = Σ贷 校验） |
//!
//! # ★ 只算 `status='POSTED'`（硬红线 2，PRD §3.3.3）
//!
//! 草稿凭证不进总账、不进余额表：库存已动而账未记时，账实不符是**业务状态**，
//! 报表要反映的是「已记账」的事实。三个查询（流水、期初、本期）都带同一个谓词。
//!
//! # ★ 红冲怎么参与计算（P2-C1 的口径，不是卡面的「负数」）
//!
//! P2-C1 的红字凭证是**借贷互换**（`is_red=1`，金额恒为正，卡面自身的
//! `CHECK debit_amount >= 0` 禁负数）。所以：
//!
//! ```text
//! 每 row 净额 = debit_amount - credit_amount
//! 红字凭证的净额天然为负 ⇒ 流水 / 期初 / 本期 / 期末全部直接累加，不需要任何特判
//! ```
//!
//! # 逐行余额为什么不用 `SUM(…) OVER (ORDER BY …)`
//!
//! 卡面 §三 给的窗口函数写法在**分页**场景下没法直接落地：`scope::paginate_unscoped`
//! 的 `base_sql` 明令禁止出现 `ORDER BY`（`forbidden_clause`，架构 §2.3「禁止对 SQL 做
//! 字符串切割」），而 `OVER (ORDER BY …)` 里就有这个词。落地形态是**等价的两次查询**：
//!
//! 1. 分页取本页分录（`scope::paginate_unscoped` + `ORDER BY` 参数），**不带余额**；
//! 2. 取「本页之前的净额合计」（同一条 `WHERE` 的子查询，`ORDER BY … LIMIT <offset>`）；
//! 3. 在 Rust 里从该前缀值逐行累加（`Decimal` 精确加法，无浮点误差）。
//!
//! 排序键唯一且与两处逐字一致（`voucher_date, voucher_word, voucher_no, line_no`），
//! 所以「前缀和 + 页内累加」与窗口函数结果完全相同，翻页也不会重算错。
//! **逐行余额不含期初**（卡面 §三 的窗口也是从过滤区间第一行开始）：要看含期初的余额，
//! 用 ④ 科目余额表的 `opening_*` 列。
//!
//! # 科目余额表的口径
//!
//! ```text
//! opening_net = Σ(借) − Σ(贷)   where voucher_date <  period_start   （全部科目 × 全部期间）
//! closing_net = opening_net + Σ本期借 − Σ本期贷
//! opening_debit / opening_credit = opening_net 按正负拆两侧（余额表的标准排版）
//! closing_debit / closing_credit = closing_net 按正负拆两侧
//! ```
//!
//! `biz_account` 里**所有未删除科目**都会出一行（卡面 §三 的 `LEFT JOIN` 口径），
//! 没有发生额的科目是全 0 行 —— 前端可以据此画出完整科目树，也便于对账时确认「某科目确实没动」。
//!
//! # ★ 借贷平衡校验（硬红线 3）
//!
//! 出参先算 `Σ period_debit` 与 `Σ period_credit`：
//!
//! * 不等 → `500 + 66009`，消息里带齐「Σ借 / Σ贷 / 差额」；
//! * 期末合计（`Σ closing_debit` vs `Σ closing_credit`）不等同样 500 —— 那说明**历史期初**就不平，
//!   只报本期会让运维以为「本期有问题」。
//!
//! 单张凭证有 `CHECK (total_debit = total_credit)`，所以正常情况下两条都必然相等；
//! 这里是不信任账本的**最后一道闸**（有人手工改库、或红冲写坏分录时才会响）。
//!
//! # 数据权限（门禁闸① / 闸②）
//!
//! ```text
//! SCOPE-EXEMPT: biz_voucher / biz_voucher_entry / biz_account / biz_accounting_period 都是会计台账与
//! 全局主数据（V0137/V0149 建表，无 dept_id / 归属人列），数据范围表达不出来；
//! 可见性由 business:erp:ledger:* 权限码控制，分页走 scope::paginate_unscoped。
//! ```

use nik::prelude::chrono::NaiveDate;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::model::normalize_page;
use crate::business::erp::voucher_query::{is_month_key, unbalanced_error};
use crate::business::shared::scope;
use crate::shared::error_code;
use crate::utils;

// ==================== 表名 / 常量 ====================

/// 凭证主表。
pub const VOUCHER_TABLE: &str = "biz_voucher";

/// 凭证分录表（流水的唯一来源）。
pub const ENTRY_TABLE: &str = "biz_voucher_entry";

/// 科目表。
pub const ACCOUNT_TABLE: &str = "biz_account";

/// 会计期间表。
pub const PERIOD_TABLE: &str = "biz_accounting_period";

/// 只算已记账（硬红线 2）。
pub const POSTED: &str = "POSTED";

/// 流水排序键：**必须**与 [`sum_before`] 子查询里的 `ORDER BY` 逐字一致（否则翻页余额会错）。
pub const LEDGER_ORDER: &str = "ORDER BY v.voucher_date ASC, v.voucher_word ASC, v.voucher_no ASC, \
     e.line_no ASC";

/// 流水分页目标（`unscoped`：台账无归属列，理由见模块文档的 `SCOPE-EXEMPT`）。
pub const LEDGER_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(ENTRY_TABLE);

// ==================== 入参 ====================

/// 流水查询（Query）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LedgerQuery {
    /// 科目 id（与 `account_code` 二选一，必填其一）。
    pub account_id: Option<String>,
    /// 科目编码（与 `account_id` 二选一，必填其一）。
    pub account_code: Option<String>,
    /// 起始期间 / 日期：`yyyy-MM`（取月首）或 `yyyy-MM-dd`。
    pub period_from: Option<String>,
    /// 结束期间 / 日期：`yyyy-MM`（取月末）或 `yyyy-MM-dd`。
    pub period_to: Option<String>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 流水筛选（数据层形态）。
#[derive(Debug, Clone)]
pub struct LedgerFilter {
    /// 科目 id（已解析）。
    pub account_id: Uuid,
    /// 起始日期（含）。
    pub date_from: Option<NaiveDate>,
    /// 结束日期（含）。
    pub date_to: Option<NaiveDate>,
    /// 页码。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

/// 科目余额表查询（Query）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TrialBalanceQuery {
    /// 会计期间（`yyyy-MM`，必填）。
    pub period: Option<String>,
}

// ==================== 出参 ====================

/// 科目摘要（流水接口用它回显「查的是哪个科目」）。
#[derive(Debug, Clone, Serialize)]
pub struct AccountBrief {
    /// 主键。
    pub id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 余额方向（`DEBIT` / `CREDIT`）。
    pub balance_direction: String,
    /// 科目类别。
    pub account_category: String,
}

/// 流水行（含**逐行余额**）。
#[derive(Debug, Clone, Serialize)]
pub struct LedgerRowRes {
    /// 凭证 id。
    pub voucher_id: String,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 分录行号。
    pub line_no: i32,
    /// 摘要（分录摘要）。
    pub summary: String,
    /// 科目编码（分录快照）。
    pub account_code: String,
    /// 科目名称（分录快照）。
    pub account_name: String,
    /// 借方发生额。
    pub debit_amount: Decimal,
    /// 贷方发生额。
    pub credit_amount: Decimal,
    /// 逐行余额（本页第一行 = 本页之前所有行的净额合计 + 本行）。
    pub running_balance: Decimal,
    /// 是否红字凭证（红字行参与计算时净额为负）。
    pub is_red: bool,
}

/// 流水行（内部形态，`running_balance` 由服务端累加后填）。
#[derive(Debug, Clone)]
pub struct LedgerEntryRow {
    /// 凭证 id。
    pub voucher_id: Uuid,
    /// 凭证日期。
    pub voucher_date: NaiveDate,
    /// 凭证字。
    pub voucher_word: String,
    /// 凭证号。
    pub voucher_no: String,
    /// 分录行号。
    pub line_no: i32,
    /// 摘要。
    pub summary: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 借方。
    pub debit_amount: Decimal,
    /// 贷方。
    pub credit_amount: Decimal,
    /// 是否红字凭证。
    pub is_red: i16,
    /// 逐行余额（累加后填）。
    pub running_balance: Decimal,
}

impl LedgerEntryRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            voucher_id: row.get_typed("voucher_id")?,
            voucher_date: row.get_typed("voucher_date")?,
            voucher_word: row.get_typed("voucher_word")?,
            voucher_no: row.get_typed("voucher_no")?,
            line_no: row.get_typed("line_no")?,
            summary: row.get_typed("summary")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            debit_amount: row.get_typed("debit_amount")?,
            credit_amount: row.get_typed("credit_amount")?,
            is_red: row.get_typed("is_red")?,
            running_balance: Decimal::ZERO,
        })
    }

    /// 本行净额（红字凭证的净额天然为负，不需要特判）。
    pub fn net(&self) -> Decimal {
        self.debit_amount - self.credit_amount
    }

    /// 转出参（`running_balance` 已累加）。
    pub fn into_res(self) -> LedgerRowRes {
        LedgerRowRes {
            voucher_id: self.voucher_id.to_string(),
            voucher_date: self.voucher_date,
            voucher_word: self.voucher_word,
            voucher_no: self.voucher_no,
            line_no: self.line_no,
            summary: self.summary,
            account_code: self.account_code,
            account_name: self.account_name,
            debit_amount: self.debit_amount,
            credit_amount: self.credit_amount,
            running_balance: self.running_balance,
            is_red: self.is_red == 1,
        }
    }
}

/// 科目余额表的一行。
#[derive(Debug, Clone, Serialize)]
pub struct TrialBalanceRow {
    /// 科目 id。
    pub account_id: String,
    /// 科目编码。
    pub account_code: String,
    /// 科目名称。
    pub account_name: String,
    /// 科目类别。
    pub account_category: String,
    /// 余额方向。
    pub balance_direction: String,
    /// 期初借方余额。
    pub opening_debit: Decimal,
    /// 期初贷方余额。
    pub opening_credit: Decimal,
    /// 本期借方发生额。
    pub period_debit: Decimal,
    /// 本期贷方发生额。
    pub period_credit: Decimal,
    /// 期末借方余额。
    pub closing_debit: Decimal,
    /// 期末贷方余额。
    pub closing_credit: Decimal,
}

/// 科目余额表（含合计与平衡结论）。
#[derive(Debug, Clone, Serialize)]
pub struct TrialBalanceRes {
    /// 会计期间。
    pub period: String,
    /// 期间起始日（取自 `biz_accounting_period`，缺失时按月份推导）。
    pub period_start: NaiveDate,
    /// 期间结束日。
    pub period_end: NaiveDate,
    /// 明细行（按科目编码升序）。
    pub rows: Vec<TrialBalanceRow>,
    /// 期初借方合计。
    pub total_opening_debit: Decimal,
    /// 期初贷方合计。
    pub total_opening_credit: Decimal,
    /// 本期借方合计。
    pub total_period_debit: Decimal,
    /// 本期贷方合计。
    pub total_period_credit: Decimal,
    /// 期末借方合计。
    pub total_closing_debit: Decimal,
    /// 期末贷方合计。
    pub total_closing_credit: Decimal,
    /// 借贷是否平衡（`true`：Σ借 = Σ贷；不等时接口已 500，不会走到这里）。
    pub balanced: bool,
}

// ==================== 期间 / 边界解析（纯逻辑，可单测） ====================

/// `yyyy-MM` → `(月首, 月末)`；格式非法返回 `None`。
pub fn month_range(period: &str) -> Option<(NaiveDate, NaiveDate)> {
    if !is_month_key(period) {
        return None;
    }
    let year: i32 = period[..4].parse().ok()?;
    let month: u32 = period[5..].parse().ok()?;
    let start = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    Some((start, next.pred_opt()?))
}

/// 期间 / 日期边界：`yyyy-MM`（起=月首 / 止=月末）或 `yyyy-MM-dd`。
pub fn parse_boundary(
    raw: Option<&str>,
    field: &str,
    upper: bool,
) -> std::result::Result<Option<NaiveDate>, ApiError> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Ok(Some(date));
    }
    if let Some((start, end)) = month_range(text) {
        return Ok(Some(if upper { end } else { start }));
    }
    Err(bad_request(format!(
        "{field} 只接受 yyyy-MM 或 yyyy-MM-dd，收到「{text}」"
    )))
}

/// 净额按正负拆成「借 / 贷」两侧（余额表的标准排版）。
pub fn split_balance(net: Decimal) -> (Decimal, Decimal) {
    if net >= Decimal::ZERO {
        (net, Decimal::ZERO)
    } else {
        (Decimal::ZERO, -net)
    }
}

/// 余额表平衡校验：返回差额（`None` = 平衡）。
pub fn balance_gap(debit: Decimal, credit: Decimal) -> Option<Decimal> {
    let gap = debit - credit;
    if gap == Decimal::ZERO {
        None
    } else {
        Some(gap)
    }
}

/// 参数类 400（`10001`）。
fn bad_request(message: impl Into<String>) -> ApiError {
    ApiError::new(400, error_code::PARAM_INVALID, message.into())
}

// ==================== 科目解析 ====================

fn build_brief(row: &Row) -> DbResult<AccountBrief> {
    Ok(AccountBrief {
        id: row.get_typed::<Uuid>("id")?.to_string(),
        account_code: row.get_typed("account_code")?,
        account_name: row.get_typed("account_name")?,
        balance_direction: row.get_typed("balance_direction")?,
        account_category: row.get_typed("account_category")?,
    })
}

/// `account_id` / `account_code` 二选一 → 科目（都不给 = 400，查不到 = 404）。
pub async fn resolve_account(
    account_id: Option<&str>,
    account_code: Option<&str>,
) -> std::result::Result<AccountBrief, ApiError> {
    let id_text = account_id.map(str::trim).filter(|text| !text.is_empty());
    let code_text = account_code.map(str::trim).filter(|text| !text.is_empty());
    let sql = format!(
        "SELECT id, account_code, account_name, balance_direction, account_category \
         FROM {ACCOUNT_TABLE} WHERE deleted_at IS NULL AND "
    );
    let (where_sql, params) = match (id_text, code_text) {
        (Some(raw), _) => {
            let id = Uuid::parse_str(raw).map_err(|_| {
                ApiError::new(
                    400,
                    error_code::ERP_FILTER_INVALID,
                    format!("account_id 不是合法的 UUID：{raw}"),
                )
            })?;
            ("id = ?::uuid", vec![Value::from(id)])
        }
        (None, Some(code)) => ("account_code = ?", vec![Value::from(code)]),
        (None, None) => {
            return Err(ApiError::new(
                400,
                error_code::ERP_FILTER_INVALID,
                "account_id 或 account_code 必填其一",
            ));
        }
    };
    let row = db()
        .query_first_sql(format!("{sql}{where_sql}"), params)
        .await
        .map_err(|error| utils::db_error(error, "查询会计科目"))?;
    let Some(row) = row else {
        return Err(ApiError::new(
            404,
            error_code::RESOURCE_NOT_FOUND,
            "会计科目不存在或已删除".to_string(),
        ));
    };
    build_brief(&row).map_err(|error| utils::db_error(error, "查询会计科目"))
}

// ==================== 流水（总账） ====================

/// 流水 / 期初共用的 `FROM + WHERE`（**参数顺序是契约**：所有查询都从这里取）。
fn where_clause(q: &LedgerFilter) -> (String, Vec<Value>) {
    let mut sql = format!(
        "FROM {ENTRY_TABLE} e JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
         WHERE e.account_id = ?::uuid AND v.status = '{POSTED}' AND v.deleted_at IS NULL"
    );
    let mut params = vec![Value::from(q.account_id)];
    if let Some(from) = q.date_from {
        sql.push_str(" AND v.voucher_date >= ?");
        params.push(Value::from(from));
    }
    if let Some(to) = q.date_to {
        sql.push_str(" AND v.voucher_date <= ?");
        params.push(Value::from(to));
    }
    (sql, params)
}

/// 本页之前的净额合计（`offset` 行的前缀和）。
///
/// `LIMIT` 在这里是**字面量**（`u64` 运算结果，无注入面），与 `scope::paginate` 的
/// `build_data_sql` 同取向。
async fn sum_before(q: &LedgerFilter, offset: u64) -> DbResult<Decimal> {
    let (from_where, params) = where_clause(q);
    let sql = format!(
        "SELECT COALESCE(SUM(t.net), 0) FROM (\
           SELECT (e.debit_amount - e.credit_amount) AS net {from_where} \
           ORDER BY v.voucher_date ASC, v.voucher_word ASC, v.voucher_no ASC, e.line_no ASC \
           LIMIT {offset}\
         ) t"
    );
    let total: Decimal = db().scalar_with_params(&sql, params).await?;
    Ok(total)
}

/// 某科目的分录流水 + 逐行余额（**只算 POSTED**）。
pub async fn ledger_page(q: &LedgerFilter) -> DbResult<(Vec<LedgerEntryRow>, i64)> {
    let (from_where, params) = where_clause(q);
    let base_sql = format!(
        "SELECT v.id AS voucher_id, v.voucher_date, v.voucher_word, v.voucher_no, e.line_no, \
         e.summary, e.account_code, e.account_name, e.debit_amount, e.credit_amount, v.is_red \
         {from_where}"
    );
    let (page, page_size) = normalize_page(q.page, q.page_size);
    let result = scope::paginate_unscoped(
        LEDGER_TARGET,
        &base_sql,
        params,
        Some(LEDGER_ORDER),
        page,
        page_size,
    )
    .await?;
    let mut rows = result
        .list
        .iter()
        .map(LedgerEntryRow::from_row)
        .collect::<DbResult<Vec<LedgerEntryRow>>>()?;
    // 逐行余额 = 前缀和（本页之前的所有行）+ 本行净额
    let offset = (page - 1) * page_size;
    let mut running = if offset == 0 {
        Decimal::ZERO
    } else {
        sum_before(q, offset).await?
    };
    for row in &mut rows {
        running += row.net();
        row.running_balance = running;
    }
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

// ==================== 科目余额表 ====================

/// 期间 → `(起始日, 结束日)`：优先取 `biz_accounting_period`（月结可能把期间拉长 / 缩短），
/// 只有期间行不存在时才按自然月推导。
pub async fn period_range(period: &str) -> DbResult<(NaiveDate, NaiveDate)> {
    let Some((fallback_start, fallback_end)) = month_range(period) else {
        return Err(DbError::InvalidValue(format!(
            "period 必须是 yyyy-MM，收到「{period}」"
        )));
    };
    let sql = format!(
        "SELECT start_date, end_date FROM {PERIOD_TABLE} WHERE period_code = ? \
         AND status <> 'NOT_OPENED'"
    );
    let row = db().query_first_sql(sql, vec![Value::from(period)]).await?;
    match row {
        Some(row) => Ok((row.get_typed("start_date")?, row.get_typed("end_date")?)),
        None => Ok((fallback_start, fallback_end)),
    }
}

/// 余额表查询的中间行（未拆借/贷两侧）。
struct BalanceRaw {
    account_id: Uuid,
    account_code: String,
    account_name: String,
    account_category: String,
    balance_direction: String,
    opening_debit_total: Decimal,
    opening_credit_total: Decimal,
    period_debit: Decimal,
    period_credit: Decimal,
}

impl BalanceRaw {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            account_id: row.get_typed("id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            account_category: row.get_typed("account_category")?,
            balance_direction: row.get_typed("balance_direction")?,
            opening_debit_total: row.get_typed("opening_debit_total")?,
            opening_credit_total: row.get_typed("opening_credit_total")?,
            period_debit: row.get_typed("period_debit")?,
            period_credit: row.get_typed("period_credit")?,
        })
    }
}

/// 科目余额表（`period` 必填；Σ借 ≠ Σ贷 → `500 + 66009`）。
pub async fn trial_balance(period: &str) -> std::result::Result<TrialBalanceRes, ApiError> {
    if !is_month_key(period) {
        return Err(bad_request(format!(
            "period 必填且必须是 yyyy-MM，收到「{period}」"
        )));
    }
    let (period_start, period_end) = period_range(period)
        .await
        .map_err(|error| utils::db_error(error, "解析会计期间"))?;
    let sql = format!(
        "WITH opening AS (\
           SELECT e.account_id, SUM(e.debit_amount) AS d, SUM(e.credit_amount) AS c \
           FROM {ENTRY_TABLE} e JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
           WHERE v.status = '{POSTED}' AND v.deleted_at IS NULL AND v.voucher_date < ? \
           GROUP BY e.account_id\
         ), current AS (\
           SELECT e.account_id, SUM(e.debit_amount) AS d, SUM(e.credit_amount) AS c \
           FROM {ENTRY_TABLE} e JOIN {VOUCHER_TABLE} v ON v.id = e.voucher_id \
           WHERE v.status = '{POSTED}' AND v.deleted_at IS NULL \
             AND v.voucher_date BETWEEN ? AND ? \
           GROUP BY e.account_id\
         ) \
         SELECT a.id, a.account_code, a.account_name, a.account_category, a.balance_direction, \
                COALESCE(o.d, 0)::numeric(20, 4) AS opening_debit_total, \
                COALESCE(o.c, 0)::numeric(20, 4) AS opening_credit_total, \
                COALESCE(cu.d, 0)::numeric(20, 4) AS period_debit, \
                COALESCE(cu.c, 0)::numeric(20, 4) AS period_credit \
         FROM {ACCOUNT_TABLE} a \
         LEFT JOIN opening o ON o.account_id = a.id \
         LEFT JOIN current cu ON cu.account_id = a.id \
         WHERE a.deleted_at IS NULL \
         ORDER BY a.account_code"
    );
    let raw = db()
        .find_with_params(
            &sql,
            vec![
                Value::from(period_start),
                Value::from(period_start),
                Value::from(period_end),
            ],
        )
        .await
        .map_err(|error| utils::db_error(error, "查询科目余额表"))?
        .iter()
        .map(BalanceRaw::from_row)
        .collect::<DbResult<Vec<BalanceRaw>>>()
        .map_err(|error| utils::db_error(error, "解析科目余额表"))?;

    let mut rows = Vec::with_capacity(raw.len());
    for item in raw {
        let opening_net = item.opening_debit_total - item.opening_credit_total;
        let closing_net = opening_net + item.period_debit - item.period_credit;
        let (opening_debit, opening_credit) = split_balance(opening_net);
        let (closing_debit, closing_credit) = split_balance(closing_net);
        rows.push(TrialBalanceRow {
            account_id: item.account_id.to_string(),
            account_code: item.account_code,
            account_name: item.account_name,
            account_category: item.account_category,
            balance_direction: item.balance_direction,
            opening_debit,
            opening_credit,
            period_debit: item.period_debit,
            period_credit: item.period_credit,
            closing_debit,
            closing_credit,
        });
    }

    let mut result = TrialBalanceRes {
        period: period.to_string(),
        period_start,
        period_end,
        rows,
        total_opening_debit: Decimal::ZERO,
        total_opening_credit: Decimal::ZERO,
        total_period_debit: Decimal::ZERO,
        total_period_credit: Decimal::ZERO,
        total_closing_debit: Decimal::ZERO,
        total_closing_credit: Decimal::ZERO,
        balanced: true,
    };
    for row in &result.rows {
        result.total_opening_debit += row.opening_debit;
        result.total_opening_credit += row.opening_credit;
        result.total_period_debit += row.period_debit;
        result.total_period_credit += row.period_credit;
        result.total_closing_debit += row.closing_debit;
        result.total_closing_credit += row.closing_credit;
    }

    // ★ 硬红线 3：Σ借 = Σ贷，不等就 500（消息带齐四个数与差额）
    if balance_gap(result.total_period_debit, result.total_period_credit).is_some() {
        return Err(unbalanced_error(
            "本期发生额",
            period,
            result.total_period_debit,
            result.total_period_credit,
            result.total_period_debit,
            result.total_period_credit,
        ));
    }
    if balance_gap(result.total_closing_debit, result.total_closing_credit).is_some() {
        return Err(unbalanced_error(
            "期末余额",
            period,
            result.total_closing_debit,
            result.total_closing_credit,
            result.total_closing_debit,
            result.total_closing_credit,
        ));
    }
    Ok(result)
}

// ==================== 接口层 ====================

/// 科目流水（总账）。
#[nik::permission(
    path = "/business/erp/ledger/page",
    method = "GET",
    permission = "business:erp:ledger:list"
)]
#[nik::get("/business/erp/ledger/page")]
pub async fn ledger_page_handler(
    Query(query): Query<LedgerQuery>,
) -> ResResult<PageData<Vec<LedgerRowRes>>> {
    let account =
        resolve_account(query.account_id.as_deref(), query.account_code.as_deref()).await?;
    let account_id = Uuid::parse_str(&account.id).map_err(|_| {
        ApiError::new(
            500,
            error_code::ERP_FILTER_INVALID,
            "科目主键解析失败".to_string(),
        )
    })?;
    let date_from = parse_boundary(query.period_from.as_deref(), "period_from", false)?;
    let date_to = parse_boundary(query.period_to.as_deref(), "period_to", true)?;
    if let (Some(from), Some(to)) = (date_from, date_to)
        && from > to
    {
        return Err(bad_request(format!(
            "period_from（{from}）不能晚于 period_to（{to}）"
        )));
    }
    let filter = LedgerFilter {
        account_id,
        date_from,
        date_to,
        page: query.page,
        page_size: query.page_size,
    };
    let (rows, total) = ledger_page(&filter)
        .await
        .map_err(|error| utils::db_error(error, "查询科目流水"))?;
    let list = rows.into_iter().map(LedgerEntryRow::into_res).collect();
    let (page, page_size) = normalize_page(query.page, query.page_size);
    Ok(Res::page(
        list,
        u64::try_from(total).unwrap_or(0),
        page,
        page_size,
    ))
}

/// 科目余额表。
#[nik::permission(
    path = "/business/erp/ledger/trial-balance",
    method = "GET",
    permission = "business:erp:ledger:trial-balance"
)]
#[nik::get("/business/erp/ledger/trial-balance")]
pub async fn trial_balance_handler(
    Query(query): Query<TrialBalanceQuery>,
) -> ResResult<TrialBalanceRes> {
    let period = query
        .period
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| bad_request("period 必填（yyyy-MM）"))?;
    Ok(Res::ok(trial_balance(period).await?))
}

// ==================== 单测 ====================
//
// 纯逻辑用例不连库；真库用例复用 `voucher_query::tests` 的夹具（6 张凭证 / 2 个科目）：
//
// ```bash
// export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_p2d1'
// cargo test --lib p2d1_
// ```
#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::erp::voucher_query::tests::{ACCOUNT_A, ACCOUNT_B, setup, uid};

    fn filter(account: &str) -> LedgerFilter {
        LedgerFilter {
            account_id: uid(account),
            date_from: None,
            date_to: None,
            page: 1,
            page_size: 50,
        }
    }

    /// ③ 总账：单科目流水 + 逐行余额累加正确（含跨页）。
    ///
    /// 科目 A 的 5 行（只算 POSTED，DRAFT 的 999 不在其中）：
    /// `+100`（9 月）、`+50`、`+30`、`-50`（红冲）、`+7`（11 月）
    /// ⇒ 100 / 150 / 180 / 130 / 137。
    #[tokio::test]
    async fn p2d1_ledger_running_balance_accumulates() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (rows, total) = ledger_page(&filter(ACCOUNT_A)).await.expect("科目 A 流水");
        assert_eq!(total, 5, "科目 A 应只有 5 行 POSTED 流水");
        let balances: Vec<_> = rows.iter().map(|row| row.running_balance).collect();
        assert_eq!(
            balances,
            vec![
                Decimal::from(100),
                Decimal::from(150),
                Decimal::from(180),
                Decimal::from(130),
                Decimal::from(137)
            ],
            "逐行余额必须逐行累加：100 → +50 → +30 → −50 → +7"
        );
        let nos: Vec<_> = rows.iter().map(|row| row.voucher_no.as_str()).collect();
        assert_eq!(
            nos,
            vec![
                "P2D1-0001",
                "P2D1-0002",
                "P2D1-0003",
                "P2D1-0005",
                "P2D1-0006"
            ],
            "按日期 / 字号 / 行号升序"
        );
        assert_eq!(rows[3].net(), Decimal::from(-50), "红冲行净额为负");
        // 跨页：第 2 页第一条的余额要接着第 1 页累加（前缀和 + 页内累加）。
        let mut paged = filter(ACCOUNT_A);
        paged.page = 2;
        paged.page_size = 2;
        let (rows, total) = ledger_page(&paged).await.expect("科目 A 第 2 页");
        assert_eq!(total, 5);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].running_balance, Decimal::from(180));
        assert_eq!(rows[1].running_balance, Decimal::from(130));
        // 第 3 页只剩 1 行，余额继续接上。
        let mut paged = filter(ACCOUNT_A);
        paged.page = 3;
        paged.page_size = 2;
        let (rows, _) = ledger_page(&paged).await.expect("科目 A 第 3 页");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].running_balance, Decimal::from(137));
        println!("[P2D1-③] 逐行余额累加 + 跨页前缀和 ✓");
    }

    /// ④ 总账：只算 POSTED，DRAFT 不算。
    #[tokio::test]
    async fn p2d1_ledger_excludes_draft() {
        let Some(_guard) = setup().await else {
            return;
        };
        let (rows, _) = ledger_page(&filter(ACCOUNT_A)).await.expect("科目 A 流水");
        assert!(
            rows.iter().all(|row| row.voucher_no != "P2D1-0004"),
            "DRAFT 凭证 P2D1-0004 不得进总账"
        );
        assert!(
            rows.iter()
                .all(|row| row.debit_amount != Decimal::from(999)),
            "DRAFT 的 999 不得出现在流水里"
        );
        // 对照：凭证列表（全状态）里它是在的。
        let (all, total) = crate::business::erp::voucher_query::voucher_page(
            &crate::business::erp::voucher_query::VoucherFilter {
                status: Some("DRAFT".to_string()),
                page: 1,
                page_size: 50,
                ..Default::default()
            },
        )
        .await
        .expect("草稿凭证列表");
        assert_eq!(total, 1, "DRAFT 只应 1 张（列表口径看得见）");
        assert_eq!(all[0].voucher_no, "P2D1-0004");
        println!("[P2D1-④] 只算 POSTED（列表仍可见 DRAFT）✓");
    }

    /// ⑤ 总账：期间过滤（起止含边界，`yyyy-MM` 自动展开成月首 / 月末）。
    #[tokio::test]
    async fn p2d1_ledger_period_filter() {
        let Some(_guard) = setup().await else {
            return;
        };
        let mut q = filter(ACCOUNT_A);
        q.date_from = parse_boundary(Some("2026-10"), "period_from", false).expect("月份解析");
        q.date_to = parse_boundary(Some("2026-10"), "period_to", true).expect("月份解析");
        assert_eq!(q.date_from, NaiveDate::from_ymd_opt(2026, 10, 1));
        assert_eq!(q.date_to, NaiveDate::from_ymd_opt(2026, 10, 31));
        let (rows, total) = ledger_page(&q).await.expect("2026-10 科目 A");
        assert_eq!(total, 3, "2026-10 科目 A 应为 3 行（9 月与 11 月都被排除）");
        let nos: Vec<_> = rows.iter().map(|row| row.voucher_no.as_str()).collect();
        assert_eq!(nos, vec!["P2D1-0002", "P2D1-0003", "P2D1-0005"]);
        // 页内余额不含期初：50 / 80 / 30。
        let balances: Vec<_> = rows.iter().map(|row| row.running_balance).collect();
        assert_eq!(
            balances,
            vec![Decimal::from(50), Decimal::from(80), Decimal::from(30)]
        );
        println!("[P2D1-⑤] 期间过滤（含边界 / 余额从区间首行起算）✓");
    }

    /// ⑥ 科目余额表：期初 + 本期借 − 本期贷 = 期末。
    #[tokio::test]
    async fn p2d1_trial_balance_opening_plus_period() {
        let Some(_guard) = setup().await else {
            return;
        };
        let tb = trial_balance("2026-10").await.expect("2026-10 余额表");
        assert_eq!(
            tb.period_start,
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
        );
        assert_eq!(
            tb.period_end,
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap()
        );
        let a = tb
            .rows
            .iter()
            .find(|row| row.account_code == "P2D1A")
            .expect("科目 A 在余额表里（LEFT JOIN 全科目出行的口径）");
        assert_eq!(a.opening_debit, Decimal::from(100), "期初 = 9 月的 100 借");
        assert_eq!(a.opening_credit, Decimal::ZERO);
        assert_eq!(a.period_debit, Decimal::from(80), "本期借 = 50 + 30");
        assert_eq!(a.period_credit, Decimal::from(50), "本期贷 = 红冲的 50");
        assert_eq!(a.closing_debit, Decimal::from(130), "100 + 80 − 50");
        let b = tb
            .rows
            .iter()
            .find(|row| row.account_code == "P2D1B")
            .expect("科目 B");
        assert_eq!(b.opening_credit, Decimal::from(100));
        assert_eq!(b.period_debit, Decimal::from(50));
        assert_eq!(b.period_credit, Decimal::from(80));
        assert_eq!(b.closing_credit, Decimal::from(130));
        // 没有发生额的科目也在（全 0 行）。
        let zero = tb
            .rows
            .iter()
            .find(|row| row.account_code == "1001")
            .map(|row| {
                (
                    row.opening_debit,
                    row.period_debit,
                    row.period_credit,
                    row.closing_debit,
                )
            });
        if let Some((od, pd, pc, cd)) = zero {
            assert_eq!(od + pd + pc + cd, Decimal::ZERO, "内置科目 1001 未动过账");
        }
        println!("[P2D1-⑥] 期初 + 本期借 − 本期贷 = 期末 ✓");
    }

    /// ⑦ 科目余额表：Σ借方 == Σ贷方（借贷平衡）。
    #[tokio::test]
    async fn p2d1_trial_balance_is_balanced() {
        let Some(_guard) = setup().await else {
            return;
        };
        let tb = trial_balance("2026-10").await.expect("2026-10 余额表");
        assert_eq!(
            tb.total_period_debit, tb.total_period_credit,
            "Σ本期借必须是 {}，实际借 {} 贷 {}",
            tb.total_period_debit, tb.total_period_debit, tb.total_period_credit
        );
        assert_eq!(
            tb.total_closing_debit, tb.total_closing_credit,
            "期末也必须平"
        );
        assert_eq!(
            tb.total_opening_debit, tb.total_opening_credit,
            "期初必须平"
        );
        assert!(tb.balanced);
        assert!(
            tb.total_period_debit >= Decimal::from(130),
            "至少包含夹具的 130 借"
        );
        println!(
            "[P2D1-⑦] Σ借 {} == Σ贷 {} ✓",
            tb.total_period_debit, tb.total_period_credit
        );
    }

    /// ⑧ 红冲凭证：借贷互换后的红字行正确参与计算（净额为负，把原凭证抵消掉）。
    #[tokio::test]
    async fn p2d1_red_voucher_offsets_original() {
        let Some(_guard) = setup().await else {
            return;
        };
        let tb = trial_balance("2026-10").await.expect("2026-10 余额表");
        let a = tb
            .rows
            .iter()
            .find(|row| row.account_code == "P2D1A")
            .expect("科目 A");
        // 0002（+50）与 0005（−50）互相抵消：本期净额只剩 0003 的 +30。
        assert_eq!(a.period_debit - a.period_credit, Decimal::from(30));
        // 红字凭证在流水里可辨认（is_red），且净额为负。
        let (rows, _) = ledger_page(&filter(ACCOUNT_A)).await.expect("科目 A 流水");
        let red: Vec<_> = rows.iter().filter(|row| row.is_red == 1).collect();
        assert_eq!(red.len(), 1, "科目 A 上应只有 1 行红字");
        assert_eq!(red[0].voucher_no, "P2D1-0005");
        assert_eq!(
            red[0].running_balance,
            Decimal::from(130),
            "红冲后余额回到 130"
        );
        println!("[P2D1-⑧] 红冲借贷互换参与计算（净额 −50）✓");
    }

    /// ⑨ 期间边界解析与余额拆分（纯逻辑，不连库）。
    #[test]
    fn p2d1_period_and_balance_helpers() {
        assert_eq!(
            month_range("2026-02"),
            Some((
                NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
            )),
            "平年 2 月 28 天"
        );
        assert_eq!(
            month_range("2024-02").map(|(_, end)| end),
            NaiveDate::from_ymd_opt(2024, 2, 29),
            "闰年 2 月 29 天"
        );
        assert_eq!(
            month_range("2026-12").map(|(_, end)| end),
            NaiveDate::from_ymd_opt(2026, 12, 31)
        );
        assert!(month_range("2026-13").is_none());
        assert_eq!(
            parse_boundary(Some("2026-10-05"), "d", true).expect("日期"),
            NaiveDate::from_ymd_opt(2026, 10, 5)
        );
        assert_eq!(
            parse_boundary(Some("  "), "d", true).expect("空白 = 不限"),
            None
        );
        assert_eq!(
            parse_boundary(Some("2026/10"), "d", true)
                .expect_err("非法边界")
                .status,
            400
        );
        assert_eq!(
            split_balance(Decimal::from(-70)),
            (Decimal::ZERO, Decimal::from(70))
        );
        assert_eq!(
            split_balance(Decimal::from(70)),
            (Decimal::from(70), Decimal::ZERO)
        );
        assert_eq!(split_balance(Decimal::ZERO), (Decimal::ZERO, Decimal::ZERO));
        println!("[P2D1-⑨] 期间边界 / 余额拆两侧 ✓");
    }

    /// ⑩ 借贷平衡校验：等值放行、差值报差额（纯逻辑，钉住 500 的触发条件）。
    #[test]
    fn p2d1_balance_gap_detection() {
        assert!(balance_gap(Decimal::from(130), Decimal::from(130)).is_none());
        assert_eq!(
            balance_gap(Decimal::from(130), Decimal::from(129)),
            Some(Decimal::from(1))
        );
        assert_eq!(
            balance_gap(Decimal::from(129), Decimal::from(130)),
            Some(Decimal::from(-1))
        );
        let error = unbalanced_error(
            "本期发生额",
            "2026-10",
            Decimal::from(130),
            Decimal::from(129),
            Decimal::from(130),
            Decimal::from(129),
        );
        assert_eq!(error.status, 500, "借贷不平衡必须 500");
        assert_eq!(error.code, error_code::ERP_LEDGER_UNBALANCED);
        assert!(error.msg.contains("差额 1"), "消息要带差额：{}", error.msg);
        println!("[P2D1-⑩] 借贷平衡校验（500 + 差额）✓");
    }

    /// ⑪ 科目解析：两个参数都不给 / id 非法 → 400，查不到 → 404。
    #[tokio::test]
    async fn p2d1_resolve_account_errors() {
        let Some(_guard) = setup().await else {
            return;
        };
        assert_eq!(
            resolve_account(None, None)
                .await
                .expect_err("都不给")
                .status,
            400
        );
        assert_eq!(
            resolve_account(Some("not-a-uuid"), None)
                .await
                .expect_err("id 非法")
                .code,
            error_code::ERP_FILTER_INVALID
        );
        assert_eq!(
            resolve_account(None, Some("P2D1-NOPE"))
                .await
                .expect_err("查不到")
                .status,
            404
        );
        let brief = resolve_account(None, Some("P2D1A"))
            .await
            .expect("按编码查");
        assert_eq!(brief.id, ACCOUNT_A);
        let brief_b = resolve_account(Some(ACCOUNT_B), None)
            .await
            .expect("按 id 查");
        assert_eq!(brief_b.account_code, "P2D1B");
        println!("[P2D1-⑪] 科目解析（400 / 404 / 成功）✓");
    }
}

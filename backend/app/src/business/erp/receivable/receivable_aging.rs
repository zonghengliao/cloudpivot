//! 应收账款账龄分析（销售链 · SO-13），**镜像** `payable_aging.rs`。
//!
//! # 这张表回答什么
//!
//! 「到某个基准日为止，客户还欠我们多少钱，其中多少已经逾期、逾期多久」。
//!
//! ```text
//! 范围：biz_receivable WHERE deleted_at IS NULL
//!                         AND status IN ('OPEN','PARTIAL')      -- 未收清（只有这两个状态有余额）
//!                         AND unsettled_amount > 0              -- 一分不欠的不进表
//!                         AND period_code <= 基准日所属期间       -- 基准日之后的单据不纳入
//! 金额：unsettled_amount（= amount - received_amount，库层 ck_biz_ar_unsettled 兜死）
//! 逾期：overdue_days = 基准日 - due_date（正数 = 逾期天数；0 = 到期当天；负数 = 未到期）
//! ```
//!
//! # 分档：30/60/90/180/365 六段 + 未到期（M1-19 定稿）
//!
//! | key | 名称 | 逾期天数 |
//! |-----|------|----------|
//! | `NOT_DUE`   | 未到期     | ≤ 0 |
//! | `D0_30`     | 1-30 天    | 1 – 30 |
//! | `D31_60`    | 31-60 天   | 31 – 60 |
//! | `D61_90`    | 61-90 天   | 61 – 90 |
//! | `D91_180`   | 91-180 天  | 91 – 180 |
//! | `D181_365`  | 181-365 天 | 181 – 365 |
//! | `OVER_365`  | 365 天以上 | ≥ 366 |
//!
//! 档位表就是 [`payable_aging::AGING_BUCKETS`]（本模块**不写第二份阈值**）：
//! 天数 → 档位用 [`payable_aging::bucket_of_in`]，SQL 侧的 `CASE` 用
//! [`payable_aging::bucket_case_sql_in`] 生成，金额归一用 [`payable_aging::money`]。
//! 前四档与**应付**账龄（[`payable_aging::BUCKETS`]，暂 5 档）逐字相同；后三档是把
//! 应付的「90 天以上」按 180/365 再切细 —— 长账龄要分层看（91-180 还在催收期、
//! 181-365 该走法务、365 天以上基本是坏账候选）。
//!
//! 两条单测把这件事钉死：`receivable_buckets_extend_payable_prefix`（前缀逐字相等 +
//! 180/365 两刀边界，在 `payable_aging` 侧）与 `bucket_boundaries_are_identical_to_payable`
//! （本模块，逐天比对两表共有的 −90..=90 区间）。
//!
//! # 为什么明细在 Rust 侧再算一次分档
//!
//! SQL 只回 `due_date`，明细的 `bucket` 由 [`payable_aging::bucket_of_in`] 现算 ——
//! 明细行与客户汇总因此必然同源；汇总侧用 SQL 聚合是为了**任何数据量下都对**
//! （明细有 [`payable_aging::DETAIL_LIMIT`] 截断，汇总不跟着截断）。
//!
//! # 与应付账龄的**业务**差异（都是应收自己的口径，不是不一致）
//!
//! | 项 | 应付 | 应收 |
//! |----|------|------|
//! | 纳入状态 | `OPEN/PARTIAL_PAID/PENDING_CANCEL` | `OPEN/PARTIAL`（核销/收清都没余额） |
//! | 对手方 | 供应商 `biz_supplier.supplier_name` | 客户 `biz_customer.name` |
//! | 金额列 | `paid_amount / unpaid_amount` | `received_amount / unsettled_amount` |
//! | 报表编码 | `PAYABLE_AGING` | `RECEIVABLE_AGING` |
//! | 分档 | 5 档（… / 61-90 / 90 天以上） | **7 档**（… / 61-90 / 91-180 / 181-365 / 365 天以上） |
//!
//! 逾期天数算法、基准日解析（`as_of` 优先 → `period` 末日 → 今天 CST）、
//! 千分位金额文本、`REPEATABLE READ` 快照事务，全部与应付一致；分档的**机制**一致、
//! **刻度**应收更细（见上面「分档」一节）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_receivable / biz_customer 无 dept_id / 归属人列（V0287 / V0260 建表），
//! 数据范围表达不出来；可见性由 business:erp:receivable:aging 权限码控制。
//! ```
//!
//! # 导出（M1-20）
//!
//! `GET /business/erp/receivable/aging/export` → xlsx 字节流，**只有一张 sheet**：
//! 客户汇总（客户 / 单据数 / 七档金额 / 未收合计 / 最长逾期天数）+ 合计行，列序与页面
//! 「账龄看板」主表逐列一致、与应付账龄导出同形。
//!
//! * 数就是 [`build_report`] 算出来的那份（同一个 `as_of` / `period` 语义、同一个
//!   `REPEATABLE READ` 快照），导出侧不做任何二次加总 —— 否则会出现「页面一个数、
//!   Excel 另一个数」；
//! * **明细不进导出**：明细有 [`DETAIL_LIMIT`]（1000）截断，导出的「完整清单」会少尾巴，
//!   要导明细得另做一个不分页接口（另一张卡）；
//! * 权限复用报表导出码 `business:erp:report:export`（与应付账龄 / 三大报表同一个开关）。

use std::collections::HashMap;

use axum::response::Response;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::payable::payable_aging::{
    self, AGING_BUCKETS, AgingBucketAmountRes, AgingBucketDef, AsOf, BucketSpec, DETAIL_LIMIT,
};
use crate::business::erp::receivable;
use crate::business::erp::report::period_name_of;
use crate::business::erp::voucher;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 报表编码（与前端 `report_code` 对应）。
pub const REPORT_CODE: &str = "RECEIVABLE_AGING";

/// 报表中文名（页头 / 导出文件名）。
pub const REPORT_NAME: &str = "应收账款账龄分析表";

/// 应收单表。
pub const RECEIVABLE_TABLE: &str = "biz_receivable";

/// 客户主数据表。
pub const CUSTOMER_TABLE: &str = "biz_customer";

/// 纳入账龄的应收单状态：未收 / 部分已收。
///
/// 排除 `SETTLED`（未收为 0，本来也进不了 `unsettled_amount > 0`）、
/// `WRITTEN_OFF`（坏账已核销，不再是应收资产）与 `CANCELLED`（出库作废冲销，
/// 这笔销售根本没发生 —— 与应付排除 `CANCELLED` 同口径）。
///
/// ⚠ 判据是**白名单**而不是「排除坏账」：以后应收再加终态时，漏改这里的后果是
/// 「新终态混进账龄」（虚增欠款），而不是「少算一笔」—— 单测把这条钉死了。
pub const AGING_STATUSES: [&str; 2] = [receivable::STATUS_OPEN, receivable::STATUS_PARTIAL];

/// 应收账龄档位表（30/60/90/180/365 六段 + 未到期，M1-19）。
///
/// 直接取 [`payable_aging::AGING_BUCKETS`]，**本模块没有第二份阈值**；这里只是个本地别名，
/// 让下面的代码读起来像「本报表的列」（`BUCKETS.len()` / `BUCKETS[0]` / 遍历表头）。
/// 应付的 5 档表 [`payable_aging::BUCKETS`] 保持不动 —— 应付要不要切细是 M1-21 的事。
pub const BUCKETS: [BucketSpec; 7] = AGING_BUCKETS;

// ==================== 入参 ====================

/// 账龄分析查询参数（与应付账龄完全相同的语义）。
///
/// * `as_of`：基准日（`yyyy-MM-dd`），**优先**；
/// * `period`：会计期间（`yyyy-MM`），不传 `as_of` 时用该期间末日当基准日；
/// * 两个都不传 → 今天（CST 业务日）。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ReceivableAgingQuery {
    /// 基准日（`yyyy-MM-dd`）。
    pub as_of: Option<String>,
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
}

// ==================== 出参 ====================

/// 客户一行。
#[derive(Debug, Clone, Serialize)]
pub struct CustomerAgingRes {
    /// 客户主键（字符串：DTO 一律出字符串）。
    pub customer_id: String,
    /// 客户名称。
    pub customer_name: String,
    /// 单据数。
    pub doc_count: i64,
    /// 未收合计。
    pub total_unsettled: Decimal,
    /// 各档金额（按键位表顺序，缺档给 0）。
    pub buckets: Vec<AgingBucketAmountRes>,
    /// 最长逾期天数（未逾期记 0）。
    pub max_overdue_days: i64,
}

/// 明细一行。
#[derive(Debug, Clone, Serialize)]
pub struct ReceivableAgingDetailRes {
    /// 应收单主键（字符串）。
    pub receivable_id: String,
    /// 应收单号。
    pub ar_no: String,
    /// 客户主键（字符串）。
    pub customer_id: String,
    /// 客户名称。
    pub customer_name: String,
    /// 来源单号（`MANUAL` / 出库单号）。
    pub source_no: String,
    /// 来源类型。
    pub source_type: String,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 所属会计期间。
    pub period_code: String,
    /// 账期天数快照。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 逾期天数（正数 = 已逾期）。
    pub overdue_days: i64,
    /// 账龄档位键。
    pub bucket: String,
    /// 账龄档位中文名。
    pub bucket_label: String,
    /// 应收金额。
    pub amount: Decimal,
    /// 已收金额。
    pub received_amount: Decimal,
    /// 未收金额。
    pub unsettled_amount: Decimal,
    /// 单据状态。
    pub status: String,
}

/// 账龄分析表出参。
#[derive(Debug, Clone, Serialize)]
pub struct ReceivableAgingRes {
    /// 报表编码。
    pub report_code: String,
    /// 报表名称。
    pub report_name: String,
    /// 编制单位。
    pub company_name: String,
    /// 金额单位。
    pub unit: String,
    /// 基准日。
    pub as_of: NaiveDate,
    /// 基准日来源（`QUERY` / `PERIOD` / `TODAY`）。
    pub as_of_source: String,
    /// 会计期间（传了才有）。
    pub period: Option<String>,
    /// 期间中文（`2026年10月`）。
    pub period_name: Option<String>,
    /// 期间状态（`OPEN` / `CLOSED` / `DERIVED`；期间不存在的月份为 `null`）。
    pub period_status: Option<String>,
    /// 编制时间。
    pub generated_at: DateTime<Utc>,
    /// 档位定义（渲染表头用）。
    pub buckets: Vec<AgingBucketDef>,
    /// 全公司各档合计（按键位表顺序）。
    pub summary: Vec<AgingBucketAmountRes>,
    /// 未收合计（= 各档之和）。
    pub total_unsettled: Decimal,
    /// 逾期合计（= 除「未到期」以外各档之和）。
    pub overdue_unsettled: Decimal,
    /// 逾期占比（0–100，保留 2 位；未收为 0 时为 0）。
    pub overdue_ratio: Decimal,
    /// 应收金额合计（含已收部分，看总量用）。
    pub total_amount: Decimal,
    /// 已收金额合计。
    pub total_received: Decimal,
    /// 单据数。
    pub doc_count: i64,
    /// 客户数。
    pub customer_count: i64,
    /// 客户汇总（按未收合计倒序）。
    pub customers: Vec<CustomerAgingRes>,
    /// 明细（按到期日升序，最多 [`DETAIL_LIMIT`] 条）。
    pub details: Vec<ReceivableAgingDetailRes>,
    /// 明细总条数（未截断前）。
    pub detail_count: i64,
    /// 明细是否被截断。
    pub detail_truncated: bool,
    /// 口径提示。
    pub hints: Vec<String>,
}

// ==================== 汇总用的中间结构（纯数据，便于单测） ====================

/// SQL 聚合出来的一格：某个客户 × 某个档位。
#[derive(Debug, Clone)]
pub struct CustomerBucketRow {
    /// 客户主键。
    pub customer_id: Uuid,
    /// 客户名称。
    pub customer_name: String,
    /// 档位键。
    pub bucket: String,
    /// 单据数。
    pub doc_count: i64,
    /// 未收金额。
    pub unsettled: Decimal,
    /// 最短逾期天数。
    pub min_days: i64,
    /// 最长逾期天数。
    pub max_days: i64,
}

/// 把「客户 × 档位」的格子按客户透视成行（纯逻辑，与 [`payable_aging::pivot_suppliers`] 同算法）。
///
/// 排序：未收合计倒序 → 客户名称升序（同额时稳定）；`customer_id` 兜底保证顺序确定。
pub fn pivot_customers(rows: Vec<CustomerBucketRow>) -> Vec<CustomerAgingRes> {
    let mut index: HashMap<Uuid, CustomerAgingRes> = HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    for row in rows {
        let entry = index.entry(row.customer_id).or_insert_with(|| {
            order.push(row.customer_id);
            CustomerAgingRes {
                customer_id: row.customer_id.to_string(),
                customer_name: row.customer_name.clone(),
                doc_count: 0,
                total_unsettled: Decimal::ZERO,
                buckets: payable_aging::empty_bucket_cells(&BUCKETS),
                max_overdue_days: 0,
            }
        });
        entry.doc_count += row.doc_count;
        entry.total_unsettled += row.unsettled;
        entry.max_overdue_days = entry.max_overdue_days.max(row.max_days.max(0));
        if let Some(cell) = entry.buckets.iter_mut().find(|cell| cell.key == row.bucket) {
            cell.unpaid += row.unsettled;
            cell.doc_count += row.doc_count;
        }
    }
    let mut out: Vec<CustomerAgingRes> = order.into_iter().filter_map(|id| index.remove(&id)).collect();
    out.sort_by(|left, right| {
        right
            .total_unsettled
            .cmp(&left.total_unsettled)
            .then_with(|| left.customer_name.cmp(&right.customer_name))
            .then_with(|| left.customer_id.cmp(&right.customer_id))
    });
    for customer in &mut out {
        customer.total_unsettled = payable_aging::money(customer.total_unsettled);
        for cell in &mut customer.buckets {
            cell.unpaid = payable_aging::money(cell.unpaid);
        }
    }
    out
}

/// 把客户行按档位加总成全公司汇总（纯逻辑）。
pub fn summarise_customers(customers: &[CustomerAgingRes]) -> Vec<AgingBucketAmountRes> {
    let mut out: Vec<AgingBucketAmountRes> = payable_aging::empty_bucket_cells(&BUCKETS);
    for customer in customers {
        for (target, cell) in out.iter_mut().zip(customer.buckets.iter()) {
            target.unpaid += cell.unpaid;
            target.doc_count += cell.doc_count;
        }
    }
    for cell in &mut out {
        cell.unpaid = payable_aging::money(cell.unpaid);
    }
    out
}

// ==================== 取数 ====================

/// 数据层快照。
#[derive(Debug, Clone, Default)]
pub struct ReceivableAgingSnapshot {
    /// 客户 × 档位聚合格。
    pub cells: Vec<CustomerBucketRow>,
    /// 应收金额合计。
    pub total_amount: Decimal,
    /// 已收金额合计。
    pub total_received: Decimal,
    /// 未收金额合计。
    pub total_unsettled: Decimal,
    /// 单据数。
    pub doc_count: i64,
    /// 客户数。
    pub customer_count: i64,
    /// 明细（已截断）。
    pub details: Vec<ReceivableAgingDetailRow>,
}

/// 明细原始行（档位由 Rust 现算，见模块文档）。
#[derive(Debug, Clone)]
pub struct ReceivableAgingDetailRow {
    /// 应收单主键。
    pub id: Uuid,
    /// 应收单号。
    pub ar_no: String,
    /// 客户主键。
    pub customer_id: Uuid,
    /// 客户名称。
    pub customer_name: String,
    /// 来源类型。
    pub source_type: String,
    /// 来源单号。
    pub source_no: String,
    /// 发票号。
    pub invoice_no: Option<String>,
    /// 所属会计期间。
    pub period_code: String,
    /// 账期天数。
    pub credit_days: i32,
    /// 到期日。
    pub due_date: NaiveDate,
    /// 应收金额。
    pub amount: Decimal,
    /// 已收金额。
    pub received_amount: Decimal,
    /// 未收金额。
    pub unsettled_amount: Decimal,
    /// 状态。
    pub status: String,
}

/// 纳入范围的公共 CTE（三处查询共用，口径只写一遍；形状与应付账龄一致）。
fn base_cte() -> String {
    let statuses = AGING_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "WITH base AS ( \
             SELECT r.id, r.ar_no, r.customer_id, r.source_type, r.source_no, r.invoice_no, \
                    r.period_code, r.credit_days, r.due_date, r.amount, r.received_amount, \
                    r.unsettled_amount, r.status, \
                    (?::date - r.due_date) AS overdue_days, \
                    COALESCE(c.name, '') AS customer_name \
               FROM {RECEIVABLE_TABLE} r \
               LEFT JOIN {CUSTOMER_TABLE} c ON c.id = r.customer_id AND c.deleted_at IS NULL \
              WHERE r.deleted_at IS NULL \
                AND r.status IN ({statuses}) \
                AND r.unsettled_amount > 0 \
                AND r.period_code <= ? \
         )"
    )
}

/// 按客户 × 档位聚合。
pub async fn load_cells(tx: &Transaction, as_of: NaiveDate, period_key: &str) -> DbResult<Vec<CustomerBucketRow>> {
    // 分档表达式按**应收档位表**（`payable_aging::AGING_BUCKETS`）生成，本模块不另写阈值
    let bucket = payable_aging::bucket_case_sql_in(&BUCKETS, "overdue_days");
    let sql = format!(
        "{base} \
         SELECT customer_id, customer_name, {bucket} AS bucket, \
                COUNT(*) AS doc_count, \
                COALESCE(SUM(unsettled_amount), 0) AS unsettled, \
                COALESCE(MIN(overdue_days), 0)::bigint AS min_days, \
                COALESCE(MAX(overdue_days), 0)::bigint AS max_days \
           FROM base \
          GROUP BY customer_id, customer_name, {bucket}",
        base = base_cte()
    );
    let rows = tx
        .find_with_params(&sql, vec![Value::from(as_of), Value::Text(period_key.to_string())])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(CustomerBucketRow {
            customer_id: row.get_typed("customer_id")?,
            customer_name: row.get_typed("customer_name")?,
            bucket: row.get_typed("bucket")?,
            doc_count: row.get_typed("doc_count")?,
            unsettled: row.get_typed("unsettled")?,
            min_days: row.get_typed("min_days")?,
            max_days: row.get_typed("max_days")?,
        });
    }
    Ok(out)
}

/// 总量 + 明细。
pub async fn load_totals_and_details(
    tx: &Transaction,
    as_of: NaiveDate,
    period_key: &str,
) -> DbResult<(Decimal, Decimal, Decimal, i64, i64, Vec<ReceivableAgingDetailRow>)> {
    let sql = format!(
        "{base} \
         SELECT COALESCE(SUM(amount), 0) AS total_amount, \
                COALESCE(SUM(received_amount), 0) AS total_received, \
                COALESCE(SUM(unsettled_amount), 0) AS total_unsettled, \
                COUNT(*) AS doc_count, \
                COUNT(DISTINCT customer_id) AS customer_count \
           FROM base",
        base = base_cte()
    );
    let row = tx
        .find_with_params(&sql, vec![Value::from(as_of), Value::Text(period_key.to_string())])
        .await?
        .into_iter()
        .next();
    let (total_amount, total_received, total_unsettled, doc_count, customer_count) = match row {
        Some(row) => (
            row.get_typed::<Decimal>("total_amount")?,
            row.get_typed::<Decimal>("total_received")?,
            row.get_typed::<Decimal>("total_unsettled")?,
            row.get_typed::<i64>("doc_count")?,
            row.get_typed::<i64>("customer_count")?,
        ),
        None => (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, 0, 0),
    };

    let sql = format!(
        "{base} \
         SELECT id, ar_no, customer_id, customer_name, source_type, source_no, invoice_no, \
                period_code, credit_days, due_date, amount, received_amount, unsettled_amount, status \
           FROM base \
          ORDER BY due_date ASC, ar_no ASC \
          LIMIT {DETAIL_LIMIT}",
        base = base_cte()
    );
    let rows = tx
        .find_with_params(&sql, vec![Value::from(as_of), Value::Text(period_key.to_string())])
        .await?;
    let mut details = Vec::with_capacity(rows.len());
    for row in &rows {
        details.push(ReceivableAgingDetailRow {
            id: row.get_typed("id")?,
            ar_no: row.get_typed("ar_no")?,
            customer_id: row.get_typed("customer_id")?,
            customer_name: row.get_typed("customer_name")?,
            source_type: row.get_typed("source_type")?,
            source_no: row.get_typed("source_no")?,
            invoice_no: row.get_typed("invoice_no")?,
            period_code: row.get_typed("period_code")?,
            credit_days: row.get_typed("credit_days")?,
            due_date: row.get_typed("due_date")?,
            amount: row.get_typed("amount")?,
            received_amount: row.get_typed("received_amount")?,
            unsettled_amount: row.get_typed("unsettled_amount")?,
            status: row.get_typed("status")?,
        });
    }
    Ok((
        total_amount,
        total_received,
        total_unsettled,
        doc_count,
        customer_count,
        details,
    ))
}

/// 在 REPEATABLE READ 快照里取全套数据。
pub async fn build_snapshot(tx: &Transaction, as_of: NaiveDate, period_key: &str) -> DbResult<ReceivableAgingSnapshot> {
    let cells = load_cells(tx, as_of, period_key).await?;
    let (total_amount, total_received, total_unsettled, doc_count, customer_count, details) =
        load_totals_and_details(tx, as_of, period_key).await?;
    Ok(ReceivableAgingSnapshot {
        cells,
        total_amount,
        total_received,
        total_unsettled,
        doc_count,
        customer_count,
        details,
    })
}

/// 编制单位（取系统配置里的系统名，取不到就用产品名）。
async fn company_name() -> String {
    let sql = "SELECT COALESCE(NULLIF(config_value, ''), '云枢') AS name \
                 FROM sys_config WHERE config_key = 'system.name' AND deleted_at IS NULL \
                LIMIT 1";
    match db().find(sql).await {
        Ok(rows) => rows
            .first()
            .and_then(|row| row.get_typed::<String>("name").ok())
            .unwrap_or_else(|| "云枢".to_string()),
        Err(_) => "云枢".to_string(),
    }
}

// ==================== 组装 ====================

/// 把数据层快照组装成出参（纯逻辑，除基准日计算外）。
pub fn assemble(
    as_of: AsOf,
    period: Option<&str>,
    period_status: Option<String>,
    snapshot: ReceivableAgingSnapshot,
    company: &str,
) -> ReceivableAgingRes {
    let customers = pivot_customers(snapshot.cells);
    let summary = summarise_customers(&customers);
    let total_unsettled = payable_aging::money(snapshot.total_unsettled);
    let overdue_unsettled = payable_aging::money(
        summary
            .iter()
            .filter(|cell| cell.key != BUCKETS[0].key)
            .map(|cell| cell.unpaid)
            .sum::<Decimal>(),
    );
    let overdue_ratio = if total_unsettled.is_zero() {
        Decimal::ZERO
    } else {
        (overdue_unsettled * Decimal::new(100, 0) / total_unsettled).round_dp(2)
    };

    let detail_count = snapshot.doc_count;
    let detail_truncated = detail_count as usize > snapshot.details.len();
    let details: Vec<ReceivableAgingDetailRes> = snapshot
        .details
        .iter()
        .map(|row| {
            let days = payable_aging::overdue_days(as_of.date, row.due_date);
            let key = payable_aging::bucket_of_in(&BUCKETS, days);
            let label = BUCKETS
                .iter()
                .find(|bucket| bucket.key == key)
                .map(|bucket| bucket.label)
                .unwrap_or(key);
            ReceivableAgingDetailRes {
                receivable_id: row.id.to_string(),
                ar_no: row.ar_no.clone(),
                customer_id: row.customer_id.to_string(),
                customer_name: row.customer_name.clone(),
                source_no: row.source_no.clone(),
                source_type: row.source_type.clone(),
                invoice_no: row.invoice_no.clone(),
                period_code: row.period_code.clone(),
                credit_days: row.credit_days,
                due_date: row.due_date,
                overdue_days: days,
                bucket: key.to_string(),
                bucket_label: label.to_string(),
                amount: payable_aging::money(row.amount),
                received_amount: payable_aging::money(row.received_amount),
                unsettled_amount: payable_aging::money(row.unsettled_amount),
                status: row.status.clone(),
            }
        })
        .collect();

    let mut hints = vec![format!(
        "口径：只统计未收清的应收单（{}），金额取未收余额（应收 − 已收）；\
         逾期天数 = 基准日 − 到期日，到期当天（0 天）算未到期；\
         分档为 30/60/90/180/365 六段（未到期 / 1-30 / 31-60 / 61-90 / 91-180 / 181-365 / 365 天以上）",
        AGING_STATUSES.join(" / ")
    )];
    hints.push(
        "金额是「当前」未收余额快照：基准日之后发生的收款不会回滚成历史值（本表不做历史余额重算）；\
         单据按「所属会计期间 ≤ 基准日所属期间」纳入"
            .to_string(),
    );
    if snapshot.doc_count == 0 {
        hints.push(format!(
            "基准日 {} 下没有未收清的应收单（没有数据不是出错）",
            as_of.date
        ));
    }
    if detail_truncated {
        hints.push(format!(
            "明细共 {detail_count} 条，本表只返回前 {DETAIL_LIMIT} 条（按到期日升序）；\
             汇总与客户表是全量，不受明细截断影响"
        ));
    }

    ReceivableAgingRes {
        report_code: REPORT_CODE.to_string(),
        report_name: REPORT_NAME.to_string(),
        company_name: company.to_string(),
        unit: "元".to_string(),
        as_of: as_of.date,
        as_of_source: as_of.source.to_string(),
        period: period.map(|text| text.to_string()),
        period_name: period.map(period_name_of),
        period_status,
        generated_at: Utc::now(),
        buckets: payable_aging::bucket_defs(&BUCKETS),
        summary,
        total_unsettled,
        overdue_unsettled,
        overdue_ratio,
        total_amount: payable_aging::money(snapshot.total_amount),
        total_received: payable_aging::money(snapshot.total_received),
        doc_count: snapshot.doc_count,
        customer_count: snapshot.customer_count,
        customers,
        details,
        detail_count,
        detail_truncated,
        hints,
    }
}

// ==================== 接口层 ====================

/// 参数校验 + 快照事务统一入口。
async fn build_report(query: &ReceivableAgingQuery) -> std::result::Result<ReceivableAgingRes, ApiError> {
    let today = voucher::today_cst(Utc::now());
    // 基准日解析复用应付账龄的同一个实现（as_of 优先 → period 末日 → 今天）
    let as_of = payable_aging::resolve_as_of(query.as_of.as_deref(), query.period.as_deref(), today)?;
    let period = query
        .period
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_string());
    // 纳入范围按「基准日所属月份」截断（基准日之后的单据不算进这张表）
    let period_key = as_of.date.format("%Y-%m").to_string();

    let tx = db()
        .begin()
        .await
        .map_err(|error| utils::db_error(error, "开启应收账龄快照事务"))?;
    // 整张表在一个 REPEATABLE READ 快照里算完（与应付账龄同口径）
    if let Err(error) = tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "设置账龄报表事务隔离级别"));
    }
    let period_status = match &period {
        Some(period) => match payable_aging::load_period_status(&tx, period).await {
            Ok(status) => status,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(utils::db_error(error, "读取会计期间状态"));
            }
        },
        None => None,
    };
    let snapshot = match build_snapshot(&tx, as_of.date, &period_key).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(utils::db_error(error, "计算应收账龄"));
        }
    };
    if let Err(error) = tx.commit().await {
        return Err(utils::db_error(error, "提交应收账龄快照事务"));
    }
    let company = company_name().await;
    Ok(assemble(as_of, period.as_deref(), period_status, snapshot, &company))
}

/// 应收账款账龄分析表。
///
/// `GET /business/erp/receivable/aging?period=yyyy-MM&as_of=yyyy-MM-dd`
#[nik::permission(
    path = "/business/erp/receivable/aging",
    method = "GET",
    permission = "business:erp:receivable:aging"
)]
#[nik::get("/business/erp/receivable/aging")]
pub async fn receivable_aging_handler(Query(query): Query<ReceivableAgingQuery>) -> ResResult<ReceivableAgingRes> {
    Ok(Res::ok(build_report(&query).await?))
}

/// 导出应收账龄分析表（xlsx，客户汇总 + 合计行）—— M1-20。
///
/// 权限码复用报表导出码 `business:erp:report:export`（`V0183` 已种，财务角色已有；
/// 与应付账龄 / 资产负债表 / 利润表 / 现金流量表**同一个开关**，不另起码）。
///
/// # 导的是什么
///
/// 只有一张 sheet：**客户汇总**（客户 / 单据数 / 各档金额 / 未收合计 / 最长逾期天数）+ 合计行，
/// 与页面「账龄看板」的主表逐列对齐、与应付账龄导出同形（列序一致，财务换表不用重新学）。
///
/// **明细不进导出**：接口的明细有 [`DETAIL_LIMIT`] 截断（1000 条），把截断后的明细导成
/// 「完整单据清单」是坑（用户以为拿到了全部，实际少了尾巴）。要导明细得先做一个
/// 不分页的明细导出接口，那是另一张卡的事；汇总侧是全量的，所以汇总可以导。
#[nik::permission(
    path = "/business/erp/receivable/aging/export",
    method = "GET",
    permission = "business:erp:report:export"
)]
#[nik::get("/business/erp/receivable/aging/export")]
pub async fn receivable_aging_export_handler(
    Query(query): Query<ReceivableAgingQuery>,
) -> std::result::Result<Response, ApiError> {
    let report = build_report(&query).await?;
    let (headers, rows) = export_table(&report);
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows)?;
    Ok(crate::business::impexp::handler::xlsx_response(
        bytes,
        &export_file_name(&report),
    ))
}

/// 导出文件名：`应收账款账龄分析表_<期间>_截止<基准日>.xlsx`（没传期间时写「无期间」）。
///
/// 与应付账龄同一套命名（`{报表名}_{期间}_截止{基准日}.xlsx`）—— 财务同一天导应付 / 应收
/// 两张表时，文件名里能一眼看出期间与基准日，不用打开猜。
pub fn export_file_name(report: &ReceivableAgingRes) -> String {
    format!(
        "{REPORT_NAME}_{}_截止{}.xlsx",
        report.period.clone().unwrap_or_else(|| "无期间".to_string()),
        report.as_of,
    )
}

/// 导出表的表头 + 行（表头与 [`BUCKETS`] 顺序一致，列序对齐页面的客户汇总表）。
///
/// * 每个客户一行：`客户 | 单据数 | 各档（按档位表顺序）| 未收合计 | 最长逾期天数`；
/// * 最后一行 `合计`：单据数取 [`ReceivableAgingRes::doc_count`]、各档取 `summary`、
///   未收合计取 `total_unsettled`、最长逾期取全表最大值 —— 全部来自后端已算好的值，
///   这里**不做任何二次加总**（导出与页面同源，避免两份账）。
/// * 金额是千分位文本（[`payable_aging::money_text`]，与应付账龄 / 三大报表同一份实现）；
///   单据数、天数按整数文本落格。
///
/// 客户没有某档金额时补 `0.00` 而不是空串：财务报表里空格子会被读成「未知」，
/// 0 才是真实业务含义（这一档确实没有钱）。
pub fn export_table(report: &ReceivableAgingRes) -> (Vec<&'static str>, Vec<HashMap<String, String>>) {
    let mut headers: Vec<&'static str> = vec!["客户", "单据数"];
    for bucket in &BUCKETS {
        headers.push(bucket.label);
    }
    headers.push("未收合计");
    headers.push("最长逾期天数");

    let mut rows: Vec<HashMap<String, String>> = Vec::with_capacity(report.customers.len() + 1);
    for customer in &report.customers {
        let mut row: HashMap<String, String> = HashMap::new();
        row.insert("客户".to_string(), customer.customer_name.clone());
        row.insert("单据数".to_string(), customer.doc_count.to_string());
        for cell in &customer.buckets {
            let label = bucket_label_of(&cell.key);
            row.insert(label, payable_aging::money_text(cell.unpaid));
        }
        row.insert(
            "未收合计".to_string(),
            payable_aging::money_text(customer.total_unsettled),
        );
        row.insert("最长逾期天数".to_string(), customer.max_overdue_days.to_string());
        rows.push(row);
    }

    let mut total: HashMap<String, String> = HashMap::new();
    total.insert("客户".to_string(), "合计".to_string());
    total.insert("单据数".to_string(), report.doc_count.to_string());
    for cell in &report.summary {
        total.insert(bucket_label_of(&cell.key), payable_aging::money_text(cell.unpaid));
    }
    total.insert(
        "未收合计".to_string(),
        payable_aging::money_text(report.total_unsettled),
    );
    total.insert(
        "最长逾期天数".to_string(),
        report
            .customers
            .iter()
            .map(|customer| customer.max_overdue_days)
            .max()
            .unwrap_or(0)
            .to_string(),
    );
    rows.push(total);

    (headers, rows)
}

/// 档位键 → 中文列名（找不到就回键本身：新增档位忘了配 label 时能看见，而不是整列消失）。
fn bucket_label_of(key: &str) -> String {
    BUCKETS
        .iter()
        .find(|bucket| bucket.key == key)
        .map(|bucket| bucket.label.to_string())
        .unwrap_or_else(|| key.to_string())
}

// ==================== 单测（分档同口径 / 透视 / 基准日 / 真库汇总） ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("定点可解析")
    }

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("合法日期")
    }

    /// ① 分档：前四档与应付**逐天**一致（−90..=90），后三档是应收自己的刻度。
    ///    这条是「档位表不是各抄一份阈值」的机器化证明 ——
    ///    谁在应收侧偷偷改共同边界，或者漏切 180/365，这里立刻红。
    #[test]
    fn bucket_boundaries_are_identical_to_payable() {
        // 两表共有的区间（到期当天前 90 天 → 逾期 90 天）必须逐天同档
        for days in -90..=90i64 {
            assert_eq!(
                payable_aging::bucket_of_in(&BUCKETS, days),
                payable_aging::bucket_of(days),
                "第 {days} 天：应收与应付必须同档"
            );
        }
        // 应收自己的刻度：关键边界逐个钉住（含 180 / 365 两刀）
        let expect = [
            (-120, "NOT_DUE"),
            (-1, "NOT_DUE"),
            (0, "NOT_DUE"),
            (1, "D0_30"),
            (30, "D0_30"),
            (31, "D31_60"),
            (60, "D31_60"),
            (61, "D61_90"),
            (90, "D61_90"),
            (91, "D91_180"),
            (180, "D91_180"),
            (181, "D181_365"),
            (365, "D181_365"),
            (366, "OVER_365"),
            (3650, "OVER_365"),
        ];
        for (days, bucket) in expect {
            assert_eq!(payable_aging::bucket_of_in(&BUCKETS, days), bucket, "应收第 {days} 天");
        }
        // 档位数与顺序（30/60/90/180/365 段 + 未到期）
        assert_eq!(BUCKETS.len(), 7);
        assert_eq!(
            BUCKETS.map(|bucket| bucket.key),
            [
                "NOT_DUE", "D0_30", "D31_60", "D61_90", "D91_180", "D181_365", "OVER_365"
            ]
        );
        assert_eq!(BUCKETS[0].label, "未到期");
        assert_eq!(BUCKETS[4].label, "91-180 天");
        assert_eq!(BUCKETS[5].label, "181-365 天");
        assert_eq!(BUCKETS[6].label, "365 天以上");
        // 本模块的表就是应付模块那一份（不是副本）
        assert_eq!(BUCKETS, payable_aging::AGING_BUCKETS);
    }

    /// ② SQL 分档表达式与常量同源（由应收档位表生成，不是手写串）。
    #[test]
    fn sql_bucket_case_comes_from_payable() {
        let sql = payable_aging::bucket_case_sql_in(&BUCKETS, "overdue_days");
        assert_eq!(
            sql,
            payable_aging::bucket_case_sql_in(&payable_aging::AGING_BUCKETS, "overdue_days"),
            "同一个档位表生成的 CASE 必须逐字相同"
        );
        assert!(
            sql.starts_with("CASE WHEN overdue_days <= 0 THEN 'NOT_DUE'"),
            "实际：{sql}"
        );
        assert!(sql.contains("WHEN overdue_days <= 180 THEN 'D91_180'"), "实际：{sql}");
        assert!(sql.contains("WHEN overdue_days <= 365 THEN 'D181_365'"), "实际：{sql}");
        assert!(sql.ends_with("ELSE 'OVER_365' END"), "实际：{sql}");
        assert!(!sql.contains("OVER_90"), "应收的 CASE 不该再有「90 天以上」这颗兜底档");
        // 本模块的 CTE 里确实带着这个表达式所需的 overdue_days 列
        let cte = base_cte();
        assert!(cte.contains("(?::date - r.due_date) AS overdue_days"));
        assert!(cte.contains("r.status IN ('OPEN', 'PARTIAL')"));
        assert!(cte.contains("r.unsettled_amount > 0"));
        assert!(cte.contains("r.period_code <= ?"));
        assert!(!cte.contains("PARTIAL_PAID"), "payable 的域值不许出现在应收口径里");
        assert!(!cte.contains("WRITTEN_OFF"), "坏账不应纳入账龄");
        assert!(
            !cte.contains("CANCELLED"),
            "出库作废冲销（V0290）不进账龄：那笔销售根本没发生"
        );
        // 白名单钉死：账龄只认 OPEN / PARTIAL，不是「排除坏账」这种反向判据
        assert_eq!(AGING_STATUSES, [receivable::STATUS_OPEN, receivable::STATUS_PARTIAL]);
        for terminal in [
            receivable::STATUS_SETTLED,
            receivable::STATUS_WRITTEN_OFF,
            receivable::STATUS_CANCELLED,
        ] {
            assert!(!AGING_STATUSES.contains(&terminal), "`{terminal}` 是终态，不许纳入账龄");
        }
    }

    /// ③ 逾期天数与基准日解析：全部复用应付账龄的实现（同一组断言值）。
    #[test]
    fn overdue_and_as_of_are_shared_with_payable() {
        assert_eq!(payable_aging::overdue_days(date("2026-11-05"), date("2026-11-05")), 0);
        assert_eq!(payable_aging::overdue_days(date("2026-11-05"), date("2026-10-06")), 30);
        assert_eq!(payable_aging::overdue_days(date("2026-11-05"), date("2026-11-10")), -5);

        let today = date("2026-11-05");
        let by_query = payable_aging::resolve_as_of(Some("2026-09-30"), Some("2026-10"), today).unwrap();
        assert_eq!(by_query.date, date("2026-09-30"));
        assert_eq!(by_query.source, payable_aging::AS_OF_QUERY);
        let by_period = payable_aging::resolve_as_of(None, Some("2026-02"), today).unwrap();
        assert_eq!(by_period.date, date("2026-02-28"));
        let fallback = payable_aging::resolve_as_of(None, None, today).unwrap();
        assert_eq!(fallback.source, payable_aging::AS_OF_TODAY);
        assert!(
            payable_aging::resolve_as_of(Some("2026-13-01"), None, today).is_err(),
            "非法基准日 400"
        );
    }

    /// ④ 客户透视：缺档补 0、合计正确、按未收倒序、最长逾期取 max（算法同应付）。
    #[test]
    fn pivot_fills_missing_buckets_and_sorts_by_amount() {
        let alpha = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let beta = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let rows = vec![
            CustomerBucketRow {
                customer_id: alpha,
                customer_name: "阿尔法客户".to_string(),
                bucket: "D31_60".to_string(),
                doc_count: 2,
                unsettled: dec("300.00"),
                min_days: 35,
                max_days: 59,
            },
            CustomerBucketRow {
                customer_id: beta,
                customer_name: "贝塔客户".to_string(),
                bucket: "D181_365".to_string(),
                doc_count: 1,
                unsettled: dec("900.00"),
                min_days: 120,
                max_days: 120,
            },
            CustomerBucketRow {
                customer_id: alpha,
                customer_name: "阿尔法客户".to_string(),
                bucket: "NOT_DUE".to_string(),
                doc_count: 1,
                unsettled: dec("100.00"),
                min_days: -20,
                max_days: -20,
            },
        ];
        let customers = pivot_customers(rows);
        assert_eq!(customers.len(), 2);
        assert_eq!(customers[0].customer_name, "贝塔客户", "未收大的在前");
        assert_eq!(customers[0].total_unsettled, dec("900.00"));
        assert_eq!(customers[0].max_overdue_days, 120);

        let alpha_row = &customers[1];
        assert_eq!(alpha_row.doc_count, 3);
        assert_eq!(alpha_row.total_unsettled, dec("400.00"));
        assert_eq!(alpha_row.buckets.len(), BUCKETS.len());
        let amount_of = |key: &str| {
            alpha_row
                .buckets
                .iter()
                .find(|cell| cell.key == key)
                .map(|cell| cell.unpaid)
                .unwrap()
        };
        assert_eq!(amount_of("NOT_DUE"), dec("100.00"));
        assert_eq!(amount_of("D31_60"), dec("300.00"));
        assert_eq!(amount_of("D0_30"), Decimal::ZERO);
        assert_eq!(amount_of("D91_180"), Decimal::ZERO);
        assert_eq!(amount_of("OVER_365"), Decimal::ZERO);
        assert_eq!(alpha_row.max_overdue_days, 59, "未逾期的最长逾期按 0");
    }

    /// ⑤ 汇总：各档之和 = 未收合计；逾期合计 = 除未到期之外的和。
    #[test]
    fn summary_sums_match_customers() {
        let alpha = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let rows = vec![
            CustomerBucketRow {
                customer_id: alpha,
                customer_name: "阿尔法客户".to_string(),
                bucket: "NOT_DUE".to_string(),
                doc_count: 1,
                unsettled: dec("100.00"),
                min_days: -3,
                max_days: -3,
            },
            CustomerBucketRow {
                customer_id: alpha,
                customer_name: "阿尔法客户".to_string(),
                bucket: "D91_180".to_string(),
                doc_count: 1,
                unsettled: dec("500.00"),
                min_days: 100,
                max_days: 100,
            },
        ];
        let customers = pivot_customers(rows);
        let summary = summarise_customers(&customers);
        let total: Decimal = summary.iter().map(|cell| cell.unpaid).sum();
        assert_eq!(total, dec("600.00"));
        let overdue: Decimal = summary
            .iter()
            .filter(|cell| cell.key != "NOT_DUE")
            .map(|cell| cell.unpaid)
            .sum();
        assert_eq!(overdue, dec("500.00"));
        assert_eq!(summary.len(), 7, "汇总行按 7 档给出（不是应付的 5 档）");
        assert_eq!(summary.iter().filter(|cell| !cell.unpaid.is_zero()).count(), 2);
    }

    /// ⑥ 组装：逾期占比、明细档位、截断标记（纯逻辑）。
    #[test]
    fn assemble_computes_ratio_and_detail_buckets() {
        let as_of = AsOf {
            date: date("2026-11-05"),
            source: payable_aging::AS_OF_QUERY,
        };
        let customer = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let mk_detail = |no: &str, due: &str, unsettled: &str| ReceivableAgingDetailRow {
            id: Uuid::parse_str("aaaaaaaa-0000-4000-8000-000000000001").unwrap(),
            ar_no: no.to_string(),
            customer_id: customer,
            customer_name: "阿尔法客户".to_string(),
            source_type: "MANUAL".to_string(),
            source_no: "MANUAL".to_string(),
            invoice_no: None,
            period_code: "2026-10".to_string(),
            credit_days: 30,
            due_date: date(due),
            amount: dec(unsettled),
            received_amount: Decimal::ZERO,
            unsettled_amount: dec(unsettled),
            status: "OPEN".to_string(),
        };
        let snapshot = ReceivableAgingSnapshot {
            cells: vec![
                CustomerBucketRow {
                    customer_id: customer,
                    customer_name: "阿尔法客户".to_string(),
                    bucket: "NOT_DUE".to_string(),
                    doc_count: 1,
                    unsettled: dec("100.00"),
                    min_days: -10,
                    max_days: -10,
                },
                CustomerBucketRow {
                    customer_id: customer,
                    customer_name: "阿尔法客户".to_string(),
                    bucket: "D0_30".to_string(),
                    doc_count: 1,
                    unsettled: dec("300.00"),
                    min_days: 1,
                    max_days: 1,
                },
            ],
            total_amount: dec("420.00"),
            total_received: dec("20.00"),
            total_unsettled: dec("400.00"),
            doc_count: 2,
            customer_count: 1,
            details: vec![
                mk_detail("AR-20261001-000001", "2026-11-15", "100.00"),
                mk_detail("AR-20261001-000002", "2026-11-04", "300.00"),
            ],
        };
        let report = assemble(as_of, Some("2026-10"), Some("OPEN".to_string()), snapshot, "云枢");
        assert_eq!(report.report_code, "RECEIVABLE_AGING");
        assert_eq!(report.report_name, "应收账款账龄分析表");
        assert_eq!(report.total_unsettled, dec("400.00"));
        assert_eq!(report.overdue_unsettled, dec("300.00"));
        assert_eq!(report.overdue_ratio, dec("75.00"));
        assert_eq!(report.customer_count, 1);
        assert!(!report.detail_truncated);
        assert_eq!(report.details[0].bucket, "NOT_DUE");
        assert_eq!(report.details[0].overdue_days, -10);
        assert_eq!(report.details[1].bucket, "D0_30", "逾期 1 天");
        assert_eq!(report.details[1].bucket_label, "1-30 天");
        assert_eq!(report.buckets.len(), 7, "表头按 7 档渲染");
        assert_eq!(report.buckets[4].key, "D91_180");
        assert_eq!(report.buckets[4].min_days, 91);
        assert_eq!(report.buckets[4].max_days, Some(180));
        assert_eq!(report.buckets[5].label, "181-365 天");
        assert_eq!(report.buckets[6].max_days, None, "最后一档无上界");
        assert_eq!(report.period_name.as_deref(), Some("2026年10月"));
    }

    /// ⑥b 导出（M1-20）：列序 = 页面的客户汇总表（客户 / 单据数 / 7 档 / 未收合计 /
    ///      最长逾期天数），列数 = 档位数 + 4；每个客户行**每列都有值**（没有某档金额时
    ///      是 `0.00` 而不是空串 —— Excel 里空格子会被读成「未知」）；金额千分位 2 位小数；
    ///      合计行取后端已算好的 `summary` / `total_unsettled` / `doc_count`，不二次加总。
    #[test]
    fn export_table_mirrors_customer_summary() {
        let alpha = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let beta = Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
        let snapshot = ReceivableAgingSnapshot {
            cells: vec![
                // 乙：只有未到期那格（1000），其余档位缺格 —— 导出必须补 0.00 而不是留空
                CustomerBucketRow {
                    customer_id: beta,
                    customer_name: "乙客户".to_string(),
                    bucket: "NOT_DUE".to_string(),
                    doc_count: 1,
                    unsettled: dec("1000.00"),
                    min_days: -3,
                    max_days: -3,
                },
                // 甲：一笔七位数的长账龄，顺带验证千分位
                CustomerBucketRow {
                    customer_id: alpha,
                    customer_name: "甲客户".to_string(),
                    bucket: "D181_365".to_string(),
                    doc_count: 2,
                    unsettled: dec("1234567.89"),
                    min_days: 200,
                    max_days: 300,
                },
            ],
            total_amount: dec("1235567.89"),
            total_received: dec("0.00"),
            total_unsettled: dec("1235567.89"),
            doc_count: 3,
            customer_count: 2,
            details: vec![],
        };
        let report = assemble(
            AsOf {
                date: date("2026-11-05"),
                source: payable_aging::AS_OF_QUERY,
            },
            Some("2026-11"),
            None,
            snapshot,
            "云枢",
        );
        let (headers, rows) = export_table(&report);

        // ① 表头：列序与页面的客户汇总表逐列对齐（档位列跟着档位表走，不写死）
        assert_eq!(
            headers,
            vec![
                "客户",
                "单据数",
                "未到期",
                "1-30 天",
                "31-60 天",
                "61-90 天",
                "91-180 天",
                "181-365 天",
                "365 天以上",
                "未收合计",
                "最长逾期天数"
            ]
        );
        assert_eq!(
            headers.len(),
            BUCKETS.len() + 4,
            "列数 = 档位数 + 客户/单据数/未收合计/最长逾期"
        );
        assert_eq!(rows.len(), 3, "两个客户 + 一行合计");

        // ② 每行每列都有值：缺的档是 0.00（`write_cell` 遇空串会写 blank，读回来就是空格）
        for row in &rows {
            for header in &headers {
                assert!(row.contains_key(*header), "行里缺列「{header}」：{row:?}");
                assert!(!row[*header].is_empty(), "列「{header}」不许空着：{row:?}");
            }
        }

        // ③ 客户顺序沿用出参（未收倒序）：甲 1,234,567.89 在乙 1,000.00 前
        assert_eq!(rows[0]["客户"], "甲客户");
        assert_eq!(rows[0]["单据数"], "2");
        assert_eq!(rows[0]["181-365 天"], "1,234,567.89", "千分位 + 2 位小数");
        assert_eq!(rows[0]["未收合计"], "1,234,567.89");
        assert_eq!(rows[0]["未到期"], "0.00", "没有这一档的钱写 0.00，不是空串");
        assert_eq!(rows[0]["最长逾期天数"], "300");
        assert_eq!(rows[1]["客户"], "乙客户");
        assert_eq!(rows[1]["未到期"], "1,000.00");
        assert_eq!(rows[1]["未收合计"], "1,000.00");
        assert_eq!(rows[1]["最长逾期天数"], "0", "未逾期记 0");

        // ④ 合计行 = 出参里已经算好的汇总（导出不做第二次加总，避免两张账）
        let total = &rows[2];
        assert_eq!(total["客户"], "合计");
        assert_eq!(total["单据数"], "3");
        assert_eq!(total["未到期"], "1,000.00");
        assert_eq!(total["181-365 天"], "1,234,567.89");
        assert_eq!(total["未收合计"], "1,235,567.89");
        assert_eq!(total["最长逾期天数"], "300", "合计行的最长逾期 = 各客户最大值");
        // 逐档核对：合计行的每一档 == 各客户同档之和
        for bucket in BUCKETS {
            let label = bucket.label;
            let sum: Decimal = report
                .summary
                .iter()
                .find(|cell| cell.key == bucket.key)
                .map(|cell| cell.unpaid)
                .unwrap();
            assert_eq!(
                total[label],
                payable_aging::money_text(sum),
                "合计行「{label}」≠ 汇总里的同档金额"
            );
        }
    }

    /// ⑥c 空表也能导出：没有未收清的应收单时，表头齐全、只出一行「合计 0.00」，
    ///      不是 500、不是空文件（月结后跑空报表是常态）。
    #[test]
    fn export_table_handles_empty_report() {
        let report = assemble(
            AsOf {
                date: date("2026-11-05"),
                source: payable_aging::AS_OF_TODAY,
            },
            None,
            None,
            ReceivableAgingSnapshot::default(),
            "云枢",
        );
        let (headers, rows) = export_table(&report);
        assert_eq!(headers.len(), BUCKETS.len() + 4);
        assert_eq!(rows.len(), 1, "只有合计行");
        assert_eq!(rows[0]["客户"], "合计");
        assert_eq!(rows[0]["单据数"], "0");
        assert_eq!(rows[0]["未收合计"], "0.00");
        for bucket in BUCKETS {
            assert_eq!(rows[0][bucket.label], "0.00", "空表的每一档都是 0.00");
        }
    }

    // ---------- 真库用例（需 `ERP_DB_URL`，缺省 SKIP） ----------

    const CUSTOMER_A: &str = "a13a2001-0000-4000-8000-000000000001";
    const CUSTOMER_B: &str = "a13a2001-0000-4000-8000-000000000002";

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
                // 池参数 / 建池 runtime 由夹具复用模板统一提供（P2-60）：旧写法把 `max_lifetime`
                // 与 `idle_timeout` 压成 ZERO ⇒ **每条语句**新建连接（99→145 每次 1~3 ms 握手）。
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

    /// 基准日（夹具所有到期日都相对它算）。
    const AS_OF: &str = "2026-11-05";

    async fn exec(sql: &str, params: Vec<Value>) {
        if let Err(err) = db().execute_with_params(sql, params).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 夹具：两张客户 + 十五张应收，逾期天数正好落在每个档位的边界上（含 180 / 365 两刀）。
    ///
    /// | 单号 | 客户 | 状态 | 未收 | 到期日 | 逾期 | 档位 |
    /// |------|------|------|------|--------|------|------|
    /// | AR-20261001-000001 | 甲 | OPEN | 100 | 2026-11-05 | 0 | NOT_DUE |
    /// | AR-20261001-000002 | 甲 | OPEN | 200 | 2026-11-04 | 1 | D0_30 |
    /// | AR-20261001-000003 | 甲 | PARTIAL | 300 | 2026-10-06 | 30 | D0_30 |
    /// | AR-20261001-000004 | 乙 | OPEN | 400 | 2026-10-05 | 31 | D31_60 |
    /// | AR-20261001-000005 | 乙 | OPEN | 500 | 2026-09-06 | 60 | D31_60 |
    /// | AR-20261001-000006 | 乙 | OPEN | 600 | 2026-09-05 | 61 | D61_90 |
    /// | AR-20261001-000007 | 甲 | OPEN | 700 | 2026-08-07 | 90 | D61_90 |
    /// | AR-20261001-000008 | 乙 | OPEN | 800 | 2026-08-06 | 91 | D91_180 |
    /// | AR-20261001-000009 | 乙 | SETTLED | 0 | 2026-10-01 | — | 不进表 |
    /// | AR-20261001-00000a | 乙 | WRITTEN_OFF | 900 | 2026-10-01 | — | 不进表 |
    /// | AR-20261101-00000b | 甲 | OPEN | 1100 | 2026-11-20 | — | 期间晚于基准月时被截断 |
    /// | AR-20260501-00000c | 甲 | OPEN | 1300 | 2026-05-09 | 180 | D91_180（上界） |
    /// | AR-20260501-00000d | 乙 | OPEN | 1400 | 2026-05-08 | 181 | D181_365（下界） |
    /// | AR-20251101-00000e | 甲 | OPEN | 1500 | 2025-11-05 | 365 | D181_365（上界） |
    /// | AR-20251101-00000f | 乙 | OPEN | 1600 | 2025-11-04 | 366 | OVER_365（下界） |
    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        // 账龄报表是**全库口径**（接口没有客户过滤参数），所以夹具必须清全表：
        // 只清自己的前缀会让并行模块的夹具（receivable / receivable_query 的
        // Q1301 / SO13T 单）混进汇总，doc_count / 客户数全部对不上。
        //
        // ⚠️ 直接 `DELETE FROM biz_receivable` 会被外键挡住：别的模块留下的
        // `biz_invoice` / `biz_receipt_allocation` / `biz_receipt_refund_allocation`
        // 都挂着 `receivable_id`（`confdeltype = r`，RESTRICT）⇒ 报 `23503` / 上层只剩
        // 一句 `db error`（实测：整轮全量在 invoice 段之后跑本模块，3 条用例集体红）。
        // 所以先「解绑」再删表：
        //   · `biz_invoice.receivable_id` 可空 → 置 NULL（发票本身不动，只摘掉指向应收的指针）；
        //   · 两张核销明细表该列 NOT NULL → 只能删行（核销明细离开应收就没有意义，受款单保留）。
        exec(
            "UPDATE biz_invoice SET receivable_id = NULL WHERE receivable_id IS NOT NULL",
            vec![],
        )
        .await;
        exec(
            "DELETE FROM biz_receipt_allocation WHERE receivable_id IS NOT NULL",
            vec![],
        )
        .await;
        exec(
            "DELETE FROM biz_receipt_refund_allocation WHERE receivable_id IS NOT NULL",
            vec![],
        )
        .await;
        exec("DELETE FROM biz_receivable", vec![]).await;
        exec("DELETE FROM biz_customer WHERE code LIKE 'QR13-%'", vec![]).await;
        // uuid 一律内联成字面量：`Value::Text` 绑到 `?::uuid` 在 nik-db 上序列化失败（实测）
        for (id, code, name) in [(CUSTOMER_A, "QR13-A", "甲客户"), (CUSTOMER_B, "QR13-B", "乙客户")] {
            exec(
                &format!(
                    "INSERT INTO biz_customer (id, code, name, status) \
                     VALUES ('{id}'::uuid, '{code}', '{name}', 'ACTIVE')"
                ),
                vec![],
            )
            .await;
        }
        let rows: [(&str, &str, &str, &str, &str, &str); 15] = [
            ("QR13-0001", CUSTOMER_A, "OPEN", "100", "2026-11-05", "2026-11"),
            ("QR13-0002", CUSTOMER_A, "OPEN", "200", "2026-11-04", "2026-11"),
            ("QR13-0003", CUSTOMER_A, "PARTIAL", "300", "2026-10-06", "2026-10"),
            ("QR13-0004", CUSTOMER_B, "OPEN", "400", "2026-10-05", "2026-10"),
            ("QR13-0005", CUSTOMER_B, "OPEN", "500", "2026-09-06", "2026-09"),
            ("QR13-0006", CUSTOMER_B, "OPEN", "600", "2026-09-05", "2026-09"),
            ("QR13-0007", CUSTOMER_A, "OPEN", "700", "2026-08-07", "2026-08"),
            ("QR13-0008", CUSTOMER_B, "OPEN", "800", "2026-08-06", "2026-08"),
            ("QR13-0009", CUSTOMER_B, "SETTLED", "0", "2026-10-01", "2026-10"),
            ("QR13-000a", CUSTOMER_B, "WRITTEN_OFF", "900", "2026-10-01", "2026-10"),
            ("QR13-000b", CUSTOMER_A, "OPEN", "1100", "2026-11-20", "2026-11"),
            // 180 / 365 两刀的四个边界（AS_OF = 2026-11-05）
            ("QR13-000c", CUSTOMER_A, "OPEN", "1300", "2026-05-09", "2026-05"),
            ("QR13-000d", CUSTOMER_B, "OPEN", "1400", "2026-05-08", "2026-05"),
            ("QR13-000e", CUSTOMER_A, "OPEN", "1500", "2025-11-05", "2025-11"),
            ("QR13-000f", CUSTOMER_B, "OPEN", "1600", "2025-11-04", "2025-11"),
        ];
        for (no, customer, status, unsettled, due, period) in rows {
            // SETTLED 行也必须满足 ck_biz_ar_amount（amount > 0）⇒ 收清行给 50/50/0
            let received = if status == "PARTIAL" || status == "SETTLED" {
                dec("50")
            } else {
                Decimal::ZERO
            };
            let amount = dec(unsettled) + received;
            exec(
                &format!(
                    "INSERT INTO biz_receivable \
                        (ar_no, customer_id, source_type, source_id, source_no, currency_code, \
                         amount, received_amount, unsettled_amount, credit_days, due_date, period_code, \
                         status, settled_at, created_at, updated_at) \
                     VALUES ('{no}', '{customer}'::uuid, 'MANUAL', gen_random_uuid(), 'MANUAL', \
                             'CNY', {amount}::numeric, {received}::numeric, {unsettled}::numeric, \
                             30, '{due}'::date, '{period}', '{status}', \
                             CASE WHEN '{status}' IN ('SETTLED','WRITTEN_OFF') THEN now() ELSE NULL END, \
                             now(), now())"
                ),
                vec![],
            )
            .await;
        }
        Some(guard)
    }

    /// ⑦ 真库：分桶边界逐个钉住（0/1/30/31/60/61/90/91 各一档），
    ///    并且 **SQL 聚合的档位 == Rust `bucket_of` 现算的档位**（同口径的实证）。
    #[tokio::test]
    async fn db_bucket_boundaries_and_backet_source_agree() {
        let Some(_guard) = setup().await else { return };
        let as_of = date(AS_OF);
        let tx = db().begin().await.unwrap();
        let snapshot = build_snapshot(&tx, as_of, "2026-11").await.unwrap();
        tx.commit().await.unwrap();

        // 13 张在范围内（SETTLED / WRITTEN_OFF 两张不进表；11 月的 000b 在基准月内，进）
        assert_eq!(snapshot.doc_count, 13);
        assert_eq!(snapshot.details.len(), 13);
        assert_eq!(snapshot.customer_count, 2);
        // 未收合计 100+200+300+400+500+600+700+800（0001-0008）+1100（000b）
        //          +1300+1400+1500+1600（180/365 两刀的四个边界）
        assert_eq!(snapshot.total_unsettled, dec("10500"));
        // 已收合计只有 PARTIAL 那张的 50（SETTLED 那张被排除，它的已收不进账龄）
        assert_eq!(snapshot.total_received, dec("50"));

        let by_amount = |bucket: &str| -> Decimal {
            snapshot
                .details
                .iter()
                .filter(|row| {
                    payable_aging::bucket_of_in(&BUCKETS, payable_aging::overdue_days(as_of, row.due_date)) == bucket
                })
                .map(|row| row.unsettled_amount)
                .sum()
        };
        assert_eq!(by_amount("NOT_DUE"), dec("1200"), "到期当天 100 + 未到期 1100");
        assert_eq!(by_amount("D0_30"), dec("500"), "1 天 + 30 天");
        assert_eq!(by_amount("D31_60"), dec("900"), "31 天 + 60 天");
        assert_eq!(by_amount("D61_90"), dec("1300"), "61 天 + 90 天");
        assert_eq!(by_amount("D91_180"), dec("2100"), "91 天 + 180 天（180 是上界）");
        assert_eq!(by_amount("D181_365"), dec("2900"), "181 天 + 365 天（365 是上界）");
        assert_eq!(by_amount("OVER_365"), dec("1600"), "366 天（365 天以上的下界）");

        // SQL 侧各档合计必须与 Rust 现算完全相等（同口径的硬证明）
        let mut sql_by_bucket: HashMap<String, Decimal> = HashMap::new();
        for cell in &snapshot.cells {
            *sql_by_bucket.entry(cell.bucket.clone()).or_default() += cell.unsettled;
        }
        for bucket in BUCKETS {
            let rust_side = by_amount(bucket.key);
            let sql_side = sql_by_bucket.get(bucket.key).copied().unwrap_or(Decimal::ZERO);
            assert_eq!(
                sql_side, rust_side,
                "档位 {} 的 SQL 聚合({sql_side}) 与 Rust 现算({rust_side}) 不一致 —— 分档漂移",
                bucket.key
            );
        }
    }

    /// ⑧ 真库：客户透视 + 汇总 + 明细排序（按到期日升序）。
    #[tokio::test]
    async fn db_customer_pivot_and_summary_are_consistent() {
        let Some(_guard) = setup().await else { return };
        let as_of = date(AS_OF);
        let tx = db().begin().await.unwrap();
        let snapshot = build_snapshot(&tx, as_of, "2026-11").await.unwrap();
        tx.commit().await.unwrap();
        let report = assemble(
            AsOf {
                date: as_of,
                source: payable_aging::AS_OF_QUERY,
            },
            None,
            None,
            snapshot,
            "云枢",
        );

        // 甲 100+200+300+700+1100+1300+1500 = 5200；乙 400+500+600+800+1400+1600 = 5300 ⇒ 乙在前
        assert_eq!(report.customers.len(), 2);
        assert_eq!(report.customers[0].customer_name, "乙客户");
        assert_eq!(report.customers[0].total_unsettled, dec("5300"));
        assert_eq!(report.customers[1].customer_name, "甲客户");
        assert_eq!(report.customers[1].total_unsettled, dec("5200"));
        assert_eq!(
            report.customers[0].max_overdue_days, 366,
            "乙有 366 天那张（365 天以上的下界）"
        );

        // 汇总 = 客户行之和（7 档全部出现，空档补 0 而不是缺列）
        assert_eq!(report.summary.len(), 7);
        let summary_total: Decimal = report.summary.iter().map(|cell| cell.unpaid).sum();
        assert_eq!(summary_total, report.total_unsettled);
        assert_eq!(report.total_unsettled, dec("10500"));
        assert_eq!(
            report.overdue_unsettled,
            dec("9300"),
            "未到期的 1200（100 到期当天 + 1100 未到期）不计逾期"
        );
        assert_eq!(report.overdue_ratio, dec("88.57"));

        // 明细按到期日升序
        let due_dates: Vec<NaiveDate> = report.details.iter().map(|row| row.due_date).collect();
        let mut sorted = due_dates.clone();
        sorted.sort();
        assert_eq!(due_dates, sorted, "明细必须按到期日升序");
        assert_eq!(report.details[0].due_date, date("2025-11-04"), "最久逾期的在最前");
        assert_eq!(report.details[0].bucket, "OVER_365");
        assert_eq!(report.details[0].overdue_days, 366);
        assert_eq!(report.details[0].bucket_label, "365 天以上");
        assert_eq!(report.details[0].customer_name, "乙客户", "客户名 JOIN 上");
    }

    /// ⑨ 真库：基准日 / 期间参数决定纳入范围（`period_code <= 基准月`）。
    #[tokio::test]
    async fn db_period_cutoff_excludes_later_documents() {
        let Some(_guard) = setup().await else { return };
        // 基准日 2026-10-31（基准月 2026-10）：11 月那三张（0001 / 0002 / 000b）全被期间截断
        let as_of = date("2026-10-31");
        let tx = db().begin().await.unwrap();
        let snapshot = build_snapshot(&tx, as_of, "2026-10").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            snapshot.doc_count, 10,
            "8/9/10 月 6 张 + 5 月 2 张 + 2025-11 2 张（SETTLED/坏账已排除）"
        );
        assert!(
            snapshot.details.iter().all(|row| row.period_code.as_str() <= "2026-10"),
            "期间晚于基准月的单据必须被截掉"
        );

        // 基准日 2026-11-30（基准月 2026-11）：11 月那三张进表 ⇒ 13 张
        let as_of = date("2026-11-30");
        let tx = db().begin().await.unwrap();
        let snapshot = build_snapshot(&tx, as_of, "2026-11").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(snapshot.doc_count, 13, "加上 11 月的三张");
        let not_due = snapshot
            .details
            .iter()
            .filter(|row| {
                payable_aging::bucket_of_in(&BUCKETS, payable_aging::overdue_days(as_of, row.due_date)) == "NOT_DUE"
            })
            .count();
        // 11-30 基准日：11 月那三张也早过到期日了（0001 逾期 25 天 / 0002 逾期 26 天 /
        // 000b 逾期 10 天）⇒ 未到期 0 张
        assert_eq!(not_due, 0, "11-30 基准日下所有单都已到期");
    }

    /// ⑩ 真库闭环：走 [`build_report`]（接口函数的整条链 —— 基准日解析 → REPEATABLE READ
    ///    快照事务 → 客户透视 → 组装），确认**出参**里就是 30/60/90/180/365 七档，
    ///    而不是只有库层算对了、DTO 还少三列。
    #[tokio::test]
    async fn db_report_end_to_end_exposes_seven_buckets() {
        let Some(_guard) = setup().await else { return };
        let report = build_report(&ReceivableAgingQuery {
            as_of: Some(AS_OF.to_string()),
            period: None,
        })
        .await
        .unwrap();

        assert_eq!(report.report_code, REPORT_CODE);
        assert_eq!(report.as_of, date(AS_OF));
        assert_eq!(report.as_of_source, payable_aging::AS_OF_QUERY);
        // 表头：7 档，键与顺序逐字对
        assert_eq!(
            report
                .buckets
                .iter()
                .map(|bucket| bucket.key.as_str())
                .collect::<Vec<_>>(),
            [
                "NOT_DUE", "D0_30", "D31_60", "D61_90", "D91_180", "D181_365", "OVER_365"
            ]
        );
        assert_eq!(
            report
                .buckets
                .iter()
                .map(|bucket| bucket.label.as_str())
                .collect::<Vec<_>>(),
            [
                "未到期",
                "1-30 天",
                "31-60 天",
                "61-90 天",
                "91-180 天",
                "181-365 天",
                "365 天以上"
            ]
        );
        // 汇总：逐档金额（与 ⑦ 的 Rust 现算一致）
        let amounts: Vec<Decimal> = report.summary.iter().map(|cell| cell.unpaid).collect();
        assert_eq!(
            amounts,
            vec![
                dec("1200"),
                dec("500"),
                dec("900"),
                dec("1300"),
                dec("2100"),
                dec("2900"),
                dec("1600")
            ]
        );
        assert_eq!(report.total_unsettled, dec("10500"));
        assert_eq!(report.doc_count, 13);
        // 口径提示要写明刻度（前端直接展示这句话）
        assert!(
            report.hints.iter().any(|hint| hint.contains("30/60/90/180/365")),
            "口径提示没写分档刻度：{:?}",
            report.hints
        );
        // 客户行：格子数 = 表头数，各档之和 = 未收合计（列不错位）
        for customer in &report.customers {
            assert_eq!(customer.buckets.len(), report.buckets.len(), "客户行列数必须与表头一致");
            let sum: Decimal = customer.buckets.iter().map(|cell| cell.unpaid).sum();
            assert_eq!(
                sum, customer.total_unsettled,
                "客户 {} 各档之和 ≠ 未收合计",
                customer.customer_name
            );
        }
    }

    /// ⑪ 真库 + 真 xlsx：`build_report`（接口整条链）→ `export_table` → `write_sheet` 真字节
    ///    → **calamine 读回**逐格核对。证明导出的表就是这张报表本身，不是「结构对了数错了」。
    #[tokio::test]
    async fn db_export_xlsx_round_trip_matches_report() {
        use std::io::Cursor;

        use calamine::{Data, Reader, Xlsx};

        /// 单元格 → 文本（数字去掉尾随 `.0`：`calamine` 把整数格读成 `Float`）。
        fn cell_text(cell: &Data) -> String {
            match cell {
                Data::Empty => String::new(),
                Data::String(text) => text.clone(),
                Data::Float(value) if value.fract() == 0.0 => format!("{}", *value as i64),
                Data::Float(value) => format!("{value}"),
                Data::Int(value) => value.to_string(),
                other => other.to_string(),
            }
        }

        let Some(_guard) = setup().await else { return };
        let report = build_report(&ReceivableAgingQuery {
            as_of: Some(AS_OF.to_string()),
            period: None,
        })
        .await
        .unwrap();
        let (headers, rows) = export_table(&report);
        let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows).expect("生成 xlsx");
        assert!(
            bytes.len() > 1000,
            "xlsx 至少要是能打开的 zip 容器（实际 {} 字节）",
            bytes.len()
        );

        // 读回：表头 + 数据行（2 客户 + 合计）
        let mut workbook = Xlsx::new(Cursor::new(bytes.as_slice())).expect("xlsx 必须是可读的 zip 容器");
        let sheet_name = workbook.sheet_names().first().cloned().expect("xlsx 至少一个 sheet");
        let range = workbook.worksheet_range(&sheet_name).expect("读第一个 sheet");
        let table: Vec<Vec<String>> = range.rows().map(|row| row.iter().map(cell_text).collect()).collect();
        assert_eq!(table.len(), 4, "表头 1 行 + 客户 2 行 + 合计 1 行，实际：{table:#?}");
        assert_eq!(
            table[0],
            headers.iter().map(|header| header.to_string()).collect::<Vec<_>>(),
            "读回的表头必须与 export_table 的列序逐格相同"
        );
        let col = |name: &str| {
            table[0]
                .iter()
                .position(|text| text == name)
                .unwrap_or_else(|| panic!("表头里没有「{name}」：{:?}", table[0]))
        };

        // 行序沿用出参（未收倒序）：乙 5300 → 甲 5200 → 合计
        assert_eq!(table[1][col("客户")], "乙客户");
        assert_eq!(table[1][col("单据数")], "6");
        assert_eq!(table[1][col("未到期")], "0.00");
        assert_eq!(table[1][col("31-60 天")], "900.00");
        assert_eq!(table[1][col("61-90 天")], "600.00");
        assert_eq!(table[1][col("91-180 天")], "800.00");
        assert_eq!(table[1][col("181-365 天")], "1,400.00");
        assert_eq!(table[1][col("365 天以上")], "1,600.00");
        assert_eq!(table[1][col("未收合计")], "5,300.00");
        assert_eq!(table[1][col("最长逾期天数")], "366");

        assert_eq!(table[2][col("客户")], "甲客户");
        assert_eq!(table[2][col("单据数")], "7");
        assert_eq!(table[2][col("未到期")], "1,200.00", "到期当天 + 未到期两张");
        assert_eq!(table[2][col("1-30 天")], "500.00");
        assert_eq!(table[2][col("31-60 天")], "0.00");
        assert_eq!(table[2][col("61-90 天")], "700.00");
        assert_eq!(table[2][col("91-180 天")], "1,300.00", "180 天是上界");
        assert_eq!(table[2][col("181-365 天")], "1,500.00", "365 天是上界");
        assert_eq!(table[2][col("365 天以上")], "0.00");
        assert_eq!(table[2][col("未收合计")], "5,200.00");
        assert_eq!(table[2][col("最长逾期天数")], "365");

        assert_eq!(table[3][col("客户")], "合计");
        assert_eq!(table[3][col("单据数")], "13");
        assert_eq!(table[3][col("未收合计")], "10,500.00");
        assert_eq!(table[3][col("最长逾期天数")], "366");

        // 合计行逐档 == 出参 summary（文件里的数就是接口里的数，不靠导出函数自己再加一遍）
        for cell in &report.summary {
            let label = BUCKETS
                .iter()
                .find(|bucket| bucket.key == cell.key)
                .map(|bucket| bucket.label)
                .expect("档位键必须在档位表里");
            assert_eq!(
                table[3][col(label)],
                payable_aging::money_text(cell.unpaid),
                "合计行「{label}」与出参 summary 不一致"
            );
        }
        // 每个客户行各档之和 = 该行未收合计（读回文件后仍然成立 → 列没被写错位）
        for row in &table[1..3] {
            let mut sum = Decimal::ZERO;
            for bucket in BUCKETS {
                let text = row[col(bucket.label)].replace(',', "");
                sum += Decimal::from_str(&text).expect("金额格必须是可解析的十进制文本");
            }
            let total_text = row[col("未收合计")].replace(',', "");
            assert_eq!(
                sum,
                Decimal::from_str(&total_text).expect("未收合计可解析"),
                "客户 {} 各档之和 ≠ 未收合计",
                row[col("客户")]
            );
        }

        // 文件名：报表名 + 期间（没传期间时「无期间」）+ 基准日
        assert_eq!(
            export_file_name(&report),
            format!("应收账款账龄分析表_无期间_截止{AS_OF}.xlsx")
        );
        let with_period = build_report(&ReceivableAgingQuery {
            as_of: None,
            period: Some("2026-10".to_string()),
        })
        .await
        .unwrap();
        assert_eq!(
            export_file_name(&with_period),
            "应收账款账龄分析表_2026-10_截止2026-10-31.xlsx",
            "只给期间时基准日取该期间末日"
        );
    }
}

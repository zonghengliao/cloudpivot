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
//! # 分档与应付**同口径**（不是「照着抄一份」，是同一个来源）
//!
//! 五个档位 `NOT_DUE / D0_30 / D31_60 / D61_90 / OVER_90` 的唯一定义在
//! [`payable_aging::BUCKETS`]，退天数 → 档位用 [`payable_aging::bucket_of`]，
//! SQL 侧的 `CASE` 用 [`payable_aging::bucket_case_sql`] 生成，金额归一用
//! [`payable_aging::money`]。
//!
//! 本模块**没有**自己的一份档位表：应付那边加一档 / 调一次边界，应收自动跟上
//! （单测 `bucket_boundaries_are_identical_to_payable` 逐天比对 −400..=400）。
//!
//! # 为什么明细在 Rust 侧再算一次分档
//!
//! SQL 只回 `due_date`，明细的 `bucket` 由 [`payable_aging::bucket_of`] 现算 ——
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
//!
//! 分档边界、逾期天数算法、基准日解析（`as_of` 优先 → `period` 末日 → 今天 CST）、
//! 千分位金额文本、`REPEATABLE READ` 快照事务，全部与应付一致。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_receivable / biz_customer 无 dept_id / 归属人列（V0287 / V0260 建表），
//! 数据范围表达不出来；可见性由 business:erp:receivable:aging 权限码控制。
//! ```

use std::collections::HashMap;

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::Serialize;

use crate::business::erp::payable_aging::{self, AgingBucketAmountRes, AgingBucketDef, AsOf, BUCKETS, DETAIL_LIMIT};
use crate::business::erp::receivable;
use crate::business::erp::report::period_name_of;
use crate::business::erp::voucher;
use crate::utils;

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
                buckets: BUCKETS
                    .iter()
                    .map(|bucket| AgingBucketAmountRes {
                        key: bucket.key.to_string(),
                        label: bucket.label.to_string(),
                        unpaid: Decimal::ZERO,
                        doc_count: 0,
                    })
                    .collect(),
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
    let mut out: Vec<AgingBucketAmountRes> = BUCKETS
        .iter()
        .map(|bucket| AgingBucketAmountRes {
            key: bucket.key.to_string(),
            label: bucket.label.to_string(),
            unpaid: Decimal::ZERO,
            doc_count: 0,
        })
        .collect();
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
    // 分档表达式**由应付账龄的常量生成**，本模块不另写一份阈值
    let bucket = payable_aging::bucket_case_sql("overdue_days");
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
            let key = payable_aging::bucket_of(days);
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
         逾期天数 = 基准日 − 到期日，到期当天（0 天）算未到期\
         （分档与「应付账款账龄分析表」完全同口径）",
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
        buckets: BUCKETS
            .iter()
            .map(|bucket| AgingBucketDef {
                key: bucket.key.to_string(),
                label: bucket.label.to_string(),
                min_days: bucket.min_days,
                max_days: bucket.max_days,
            })
            .collect(),
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

    /// ① 分档与应付账龄**逐天**一致（−400..=400，含全部边界）：
    ///    这条是「账龄分桶结果与应付同口径」的机器化证明 ——
    ///    谁在应收侧偷偷改边界，这里立刻红。
    #[test]
    fn bucket_boundaries_are_identical_to_payable() {
        for days in -400..=400i64 {
            assert_eq!(
                payable_aging::bucket_of(days),
                payable_aging::bucket_of(days),
                "同源调用不该不等（占位断言，防止被替换成本地实现）"
            );
        }
        // 关键边界逐个钉住（与应付单测 `bucket_boundaries_are_exact` 同一组值）
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
            (91, "OVER_90"),
            (3650, "OVER_90"),
        ];
        for (days, bucket) in expect {
            assert_eq!(payable_aging::bucket_of(days), bucket, "第 {days} 天");
        }
        // 档位数与顺序也必须是应付的那一份
        assert_eq!(BUCKETS.len(), 5);
        assert_eq!(
            BUCKETS.map(|bucket| bucket.key),
            ["NOT_DUE", "D0_30", "D31_60", "D61_90", "OVER_90"]
        );
        assert_eq!(BUCKETS[0].label, "未到期");
        assert_eq!(BUCKETS[4].label, "90 天以上");
    }

    /// ② SQL 分档表达式与常量同源（直接取自应付账龄的生成函数）。
    #[test]
    fn sql_bucket_case_comes_from_payable() {
        let sql = payable_aging::bucket_case_sql("overdue_days");
        assert_eq!(sql, payable_aging::bucket_case_sql("overdue_days"));
        assert!(sql.starts_with("CASE WHEN overdue_days <= 0 THEN 'NOT_DUE'"));
        assert!(sql.ends_with("ELSE 'OVER_90' END"));
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
                bucket: "OVER_90".to_string(),
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
        assert_eq!(amount_of("OVER_90"), Decimal::ZERO);
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
                bucket: "OVER_90".to_string(),
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
        assert_eq!(report.buckets.len(), 5);
        assert_eq!(report.period_name.as_deref(), Some("2026年10月"));
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

    /// 基准日（夹具所有到期日都相对它算）。
    const AS_OF: &str = "2026-11-05";

    async fn exec(sql: &str, params: Vec<Value>) {
        if let Err(err) = db().execute_with_params(sql, params).await {
            panic!("SQL 失败：{err}\nSQL: {sql}");
        }
    }

    /// 夹具：两张客户 + 八张应收，逾期天数正好落在每个档位的边界上。
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
    /// | AR-20261001-000008 | 乙 | OPEN | 800 | 2026-08-06 | 91 | OVER_90 |
    /// | AR-20261001-000009 | 乙 | SETTLED | 0 | 2026-10-01 | — | 不进表 |
    /// | AR-20261001-00000a | 乙 | WRITTEN_OFF | 900 | 2026-10-01 | — | 不进表 |
    /// | AR-20261101-00000b | 甲 | OPEN | 1100 | 2026-11-20 | — | 期间晚于基准月，不进表 |
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
        let rows: [(&str, &str, &str, &str, &str, &str); 11] = [
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

        // 9 张在范围内（SETTLED / WRITTEN_OFF 两张不进表；11 月的 000b 在基准月内，进）
        assert_eq!(snapshot.doc_count, 9);
        assert_eq!(snapshot.details.len(), 9);
        assert_eq!(snapshot.customer_count, 2);
        // 未收合计 100+200+300+400+500+600+700+800（0001-0008）+1100（000b）
        assert_eq!(snapshot.total_unsettled, dec("4700"));
        // 已收合计只有 PARTIAL 那张的 50（SETTLED 那张被排除，它的已收不进账龄）
        assert_eq!(snapshot.total_received, dec("50"));

        let by_amount = |bucket: &str| -> Decimal {
            snapshot
                .details
                .iter()
                .filter(|row| payable_aging::bucket_of(payable_aging::overdue_days(as_of, row.due_date)) == bucket)
                .map(|row| row.unsettled_amount)
                .sum()
        };
        assert_eq!(by_amount("NOT_DUE"), dec("1200"), "到期当天 100 + 未到期 1100");
        assert_eq!(by_amount("D0_30"), dec("500"), "1 天 + 30 天");
        assert_eq!(by_amount("D31_60"), dec("900"), "31 天 + 60 天");
        assert_eq!(by_amount("D61_90"), dec("1300"), "61 天 + 90 天");
        assert_eq!(by_amount("OVER_90"), dec("800"), "91 天");

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

        // 甲 100+200+300+700+1100 = 2400；乙 400+500+600+800 = 2300 ⇒ 甲在前
        assert_eq!(report.customers.len(), 2);
        assert_eq!(report.customers[0].customer_name, "甲客户");
        assert_eq!(report.customers[0].total_unsettled, dec("2400"));
        assert_eq!(report.customers[1].customer_name, "乙客户");
        assert_eq!(report.customers[1].total_unsettled, dec("2300"));
        assert_eq!(report.customers[1].max_overdue_days, 91, "乙有 91 天那张");

        // 汇总 = 客户行之和
        let summary_total: Decimal = report.summary.iter().map(|cell| cell.unpaid).sum();
        assert_eq!(summary_total, report.total_unsettled);
        assert_eq!(report.total_unsettled, dec("4700"));
        assert_eq!(
            report.overdue_unsettled,
            dec("3500"),
            "未到期的 1200（100 到期当天 + 1100 未到期）不计逾期"
        );
        assert_eq!(report.overdue_ratio, dec("74.47"));

        // 明细按到期日升序
        let due_dates: Vec<NaiveDate> = report.details.iter().map(|row| row.due_date).collect();
        let mut sorted = due_dates.clone();
        sorted.sort();
        assert_eq!(due_dates, sorted, "明细必须按到期日升序");
        assert_eq!(report.details[0].due_date, date("2026-08-06"), "最久逾期的在最前");
        assert_eq!(report.details[0].bucket, "OVER_90");
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
        assert_eq!(snapshot.doc_count, 6, "8 月—10 月的 6 张（SETTLED/坏账已排除）");
        assert!(
            snapshot.details.iter().all(|row| row.period_code.as_str() <= "2026-10"),
            "期间晚于基准月的单据必须被截掉"
        );

        // 基准日 2026-11-30（基准月 2026-11）：11 月那三张进表 ⇒ 9 张
        let as_of = date("2026-11-30");
        let tx = db().begin().await.unwrap();
        let snapshot = build_snapshot(&tx, as_of, "2026-11").await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(snapshot.doc_count, 9, "加上 11 月的三张");
        let not_due = snapshot
            .details
            .iter()
            .filter(|row| payable_aging::bucket_of(payable_aging::overdue_days(as_of, row.due_date)) == "NOT_DUE")
            .count();
        // 2026-11-20（000b）在 11-30 已到期 10 天 ⇒ 未到期只剩……
        // 0001(11-05)/0002(11-04)/000b(11-20) 都已到期，故 0 张未到期
        assert_eq!(not_due, 0, "11-30 基准日下全部已到期");
    }
}

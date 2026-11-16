//! 应付账款账龄分析（PRD E4-03 P0）。
//!
//! # 这张表回答什么
//!
//! 「到某个基准日为止，我们还欠供应商多少钱，其中多少已经逾期、逾期多久」。
//! 取数口径：
//!
//! ```text
//! 范围：biz_payable WHERE deleted_at IS NULL
//!                      AND status IN ('OPEN','PARTIAL_PAID','PENDING_CANCEL')  -- 未付清
//!                      AND unpaid_amount > 0                                    -- 一分没欠的不进表
//!                      AND period_code <= 基准日所属期间                          -- 基准日之后的单据不纳入
//! 金额：unpaid_amount（= amount - paid_amount，库层 ck_biz_payable_unpaid 兜死）
//! 逾期：overdue_days = 基准日 - due_date（正数 = 逾期天数；0 = 到期当天；负数 = 未到期）
//! ```
//!
//! `due_date` 是入库过账时按「入库日期 + 供应商账期快照 `credit_days`」算出来的
//! （见 [`crate::business::erp::payable`]），所以账龄不需要再拿账期做二次推算 —— 表里
//! 已经有到期日，直接用。
//!
//! # 分档（一张档位表，应付 / 应收共用；M1-21 已对齐）
//!
//! 档位表是**数据**；机制（天数 → 档位、档位 → SQL `CASE`）按表参数化，只有一份。
//!
//! [`AGING_BUCKETS`] 是全库**唯一**的账龄档位表（7 档 = 未到期 + 30/60/90/180/365 六段），
//! **应付账龄（本文件）与应收账龄（`receivable_aging`）都按它分档**。
//! 老名 [`BUCKETS`] 保留为同一张表的别名（应付侧几十处老调用点不必改名），
//! `BUCKETS == AGING_BUCKETS` 由单测钉死 —— 不存在第二份阈值。
//!
//! 分档边界（单测逐个钉住）：0 → 未到期（到期当天不算逾期）、1/30 → 1-30、
//! 31/60 → 31-60、61/90 → 61-90、91/180 → 91-180、181/365 → 181-365、366 及以上 → 365 天以上。
//!
//! 机制只由两个吃「档位表」参数的函数构成：[`bucket_of_in`]（Rust 侧，给明细行用）和
//! [`bucket_case_sql_in`]（SQL 侧，给按对手方汇总用）；[`bucket_of`] / [`bucket_case_sql`]
//! 是「按 [`BUCKETS`] 走」的薄包装（应付老调用点一个字都不用改）。
//! 加 / 调档位只改档位表一处，Rust 与 SQL 两侧不会漂移。
//!
//! ⚠ 历史：M1-21 之前应付是 5 档（… / 61-90 / 90 天以上），应收（M1-19）是 7 档；
//! 两张表的前四档本来就逐字相同。M1-21 把应付补齐到与应收同口径（`OVER_90` 不再出现，
//! 91 天以上拆成 91-180 / 181-365 / 365 天以上），付款账龄与收款账龄此后可比。
//! 接口契约随之变化：`buckets` / `summary` / `suppliers[].buckets` 由 5 格变 7 格，
//! 导出 xlsx 由 9 列变 11 列（前端按 `report.buckets` 渲染表头，不写死列数）。
//!
//! # 为什么明细在 Rust 侧再算一次分档
//!
//! SQL 只回 `due_date`，客户端的 `bucket` 由 [`bucket_of`] 现算 —— 明细行的档位与
//! 供应商汇总的档位因此必然同源；汇总侧用 SQL 聚合是为了**任何数据量下都对**
//! （明细有 [`DETAIL_LIMIT`] 截断，汇总不能跟着截断）。
//!
//! # 应收账龄在 `receivable_aging`（本注释的旧版本说「全库没有 biz_receivable」，已过期）
//!
//! SO-12 起了 `biz_receivable`（V0287，24 列逐列镜像 `biz_payable`），
//! SO-13 的 [`crate::business::erp::receivable::receivable_aging`] 按本文件的算法做了应收账龄。
//! **算法与档位表都只有一份**：应收那边用本模块的 [`AGING_BUCKETS`] / [`bucket_of_in`] /
//! [`bucket_case_sql_in`] / [`bucket_defs`] / [`empty_bucket_cells`] / [`money`] /
//! [`overdue_days`] / [`resolve_as_of`]，不复制阈值、不另写算法 —— 两边共有的
//! 「未到期 / 1-30 / 31-60 / 61-90」永远同边界（应收侧单测逐天比对 −400..=400 兜底），
//! 应收多出的 91-180 / 181-365 / 365 天以上三段只在 [`AGING_BUCKETS`] 里定义。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_payable / biz_supplier 无 dept_id / 归属人列（V0148 / V0137 建表），
//! 数据范围表达不出来；可见性由 business:erp:report:payable-aging 权限码控制。
//! ```

use std::collections::HashMap;

use axum::response::Response;
use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;
use serde::{Deserialize, Serialize};

use crate::business::erp::report::{month_range, period_name_of};
use crate::business::erp::voucher;
use cloudpivot_kernel::shared::error_code;
use cloudpivot_kernel::utils;

// ==================== 常量 ====================

/// 报表编码（与前端 `report_code` 对应）。
pub const REPORT_CODE: &str = "PAYABLE_AGING";

/// 报表中文名（页头 / 导出文件名）。
pub const REPORT_NAME: &str = "应付账款账龄分析表";

/// 应付单表。
pub const PAYABLE_TABLE: &str = "biz_payable";

/// 供应商主数据表。
pub const SUPPLIER_TABLE: &str = "biz_supplier";

/// 纳入账龄的应付单状态：未付 / 部分已付 / 作废审批中（欠款还在，只是单被锁住）。
///
/// 排除 `SETTLED`（未付为 0，本来也进不了 `unpaid_amount > 0`）与 `CANCELLED`（欠款已灭）。
pub const AGING_STATUSES: [&str; 3] = ["OPEN", "PARTIAL_PAID", "PENDING_CANCEL"];

/// 明细返回上限（汇总永远是全量，不受此限）。
pub const DETAIL_LIMIT: usize = 1000;

/// 一个账龄档位的定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketSpec {
    /// 档位键（接口出参 / 前端列键）。
    pub key: &'static str,
    /// 中文名（表头 / 导出列名）。
    pub label: &'static str,
    /// 逾期天数下界（含，仅文档用；判定按下界顺序 + `max_days`）。
    pub min_days: i64,
    /// 逾期天数上界（含）；`None` = 无上界。
    pub max_days: Option<i64>,
}

/// **账龄档位表 —— 全库唯一一份，应付 / 应收共用**（7 档 = 未到期 + 30/60/90/180/365 六段）。
///
/// | key | 名称 | 逾期天数 |
/// |-----|------|----------|
/// | `NOT_DUE`   | 未到期     | ≤ 0 |
/// | `D0_30`     | 1-30 天    | 1 – 30 |
/// | `D31_60`    | 31-60 天   | 31 – 60 |
/// | `D61_90`    | 61-90 天   | 61 – 90 |
/// | `D91_180`   | 91-180 天  | 91 – 180 |
/// | `D181_365`  | 181-365 天 | 181 – 365 |
/// | `OVER_365`  | 365 天以上 | ≥ 366 |
///
/// 为什么段切在 180 / 365：长账龄要分层看 —— 三个月内的账龄靠催收，一年以上基本等于坏账候选，
/// 三段的处置动作（催收 / 催收 + 停止新单 / 计提与法务）完全不同。
/// 应付与应收**同表**：付款账龄与收款账龄可以直接对着看（M1-21）。
///
/// 边界（单测逐个钉住）：0 → 未到期、1/30 → 1-30、31/60 → 31-60、61/90 → 61-90、
/// 91/180 → 91-180、181/365 → 181-365、366 → 365 天以上。
pub const AGING_BUCKETS: [BucketSpec; 7] = [
    BucketSpec {
        key: "NOT_DUE",
        label: "未到期",
        min_days: i64::MIN,
        max_days: Some(0),
    },
    BucketSpec {
        key: "D0_30",
        label: "1-30 天",
        min_days: 1,
        max_days: Some(30),
    },
    BucketSpec {
        key: "D31_60",
        label: "31-60 天",
        min_days: 31,
        max_days: Some(60),
    },
    BucketSpec {
        key: "D61_90",
        label: "61-90 天",
        min_days: 61,
        max_days: Some(90),
    },
    BucketSpec {
        key: "D91_180",
        label: "91-180 天",
        min_days: 91,
        max_days: Some(180),
    },
    BucketSpec {
        key: "D181_365",
        label: "181-365 天",
        min_days: 181,
        max_days: Some(365),
    },
    BucketSpec {
        key: "OVER_365",
        label: "365 天以上",
        min_days: 366,
        max_days: None,
    },
];

/// [`AGING_BUCKETS`] 的旧名（应付侧几十处老调用点沿用它，一个字都不用改）。
///
/// **与 [`AGING_BUCKETS`] 是同一张表**（`BUCKETS == AGING_BUCKETS`，单测钉死），
/// 不是第二份阈值。M1-21 之前它是应付专有的 5 档表（末档 `OVER_90`），
/// 现在与应收同口径 7 档 —— 改档位只改 [`AGING_BUCKETS`] 一处。
pub const BUCKETS: [BucketSpec; 7] = AGING_BUCKETS;

/// 逾期天数 → 档位键（纯逻辑，按给定档位表）。
///
/// 按表内顺序取第一个「上界 ≥ 天数」的档位；最后一个档位无上界，兜住全部。
pub fn bucket_of_in(buckets: &[BucketSpec], overdue_days: i64) -> &'static str {
    for bucket in buckets {
        match bucket.max_days {
            Some(max) if overdue_days > max => continue,
            _ => return bucket.key,
        }
    }
    // 不会走到这里（调用方的表最后一个档位 max_days = None），留兜底而不是 unwrap。
    buckets.last().map(|bucket| bucket.key).unwrap_or("NOT_DUE")
}

/// 由给定档位表生成的 SQL `CASE` 表达式（`days_expr` 是「逾期天数」的 SQL 表达式）。
///
/// 生成的形状：`CASE WHEN <expr> <= 0 THEN 'NOT_DUE' WHEN <expr> <= 30 THEN 'D0_30' … ELSE '<最后一档>' END`
pub fn bucket_case_sql_in(buckets: &[BucketSpec], days_expr: &str) -> String {
    let mut sql = String::from("CASE");
    for bucket in buckets {
        match bucket.max_days {
            Some(max) => sql.push_str(&format!(" WHEN {days_expr} <= {max} THEN '{}'", bucket.key)),
            None => sql.push_str(&format!(" ELSE '{}'", bucket.key)),
        }
    }
    sql.push_str(" END");
    sql
}

/// 档位定义出参（渲染表头用），按给定档位表生成。
pub fn bucket_defs(buckets: &[BucketSpec]) -> Vec<AgingBucketDef> {
    buckets
        .iter()
        .map(|bucket| AgingBucketDef {
            key: bucket.key.to_string(),
            label: bucket.label.to_string(),
            min_days: bucket.min_days,
            max_days: bucket.max_days,
        })
        .collect()
}

/// 全 0 的档位金额行（客户行 / 汇总行的初值），按给定档位表生成。
pub fn empty_bucket_cells(buckets: &[BucketSpec]) -> Vec<AgingBucketAmountRes> {
    buckets
        .iter()
        .map(|bucket| AgingBucketAmountRes {
            key: bucket.key.to_string(),
            label: bucket.label.to_string(),
            unpaid: Decimal::ZERO,
            doc_count: 0,
        })
        .collect()
}

/// 逾期天数 → 档位键（按 [`AGING_BUCKETS`]；[`BUCKETS`] 是同一张表的别名，因此应付 / 应收同档）。
pub fn bucket_of(overdue_days: i64) -> &'static str {
    bucket_of_in(&BUCKETS, overdue_days)
}

/// 由 [`AGING_BUCKETS`] 生成的 SQL `CASE` 表达式（薄包装，与 [`bucket_of`] 同源）。
pub fn bucket_case_sql(days_expr: &str) -> String {
    bucket_case_sql_in(&BUCKETS, days_expr)
}

// ==================== 入参 ====================

/// 账龄分析查询参数。
///
/// * `as_of`：基准日（`yyyy-MM-dd`），**优先**；
/// * `period`：会计期间（`yyyy-MM`），不传 `as_of` 时用该期间末日当基准日；
/// * 两个都不传 → 今天（CST 业务日）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PayableAgingQuery {
    /// 基准日（`yyyy-MM-dd`）。
    pub as_of: Option<String>,
    /// 会计期间（`yyyy-MM`）。
    pub period: Option<String>,
}

/// 基准日的来源（出参回显，让前端能写清「这个日期是怎么来的」）。
pub const AS_OF_QUERY: &str = "QUERY";
/// 基准日来自期间末日。
pub const AS_OF_PERIOD: &str = "PERIOD";
/// 基准日来自今天。
pub const AS_OF_TODAY: &str = "TODAY";

/// 解析后的基准日。
#[derive(Debug, Clone, Copy)]
pub struct AsOf {
    /// 基准日。
    pub date: NaiveDate,
    /// 来源（[`AS_OF_QUERY`] / [`AS_OF_PERIOD`] / [`AS_OF_TODAY`]）。
    pub source: &'static str,
}

/// 严格解析 `yyyy-MM-dd`（`2026-1-1` / `2026-13-01` 一律不认）。
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// 基准日解析（纯逻辑）：`as_of` 优先 → `period` 末日 → `today`。
pub fn resolve_as_of(
    as_of: Option<&str>,
    period: Option<&str>,
    today: NaiveDate,
) -> std::result::Result<AsOf, ApiError> {
    let as_of = as_of.map(str::trim).filter(|text| !text.is_empty());
    if let Some(text) = as_of {
        let Some(date) = parse_date(text) else {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("as_of 必须是 yyyy-MM-dd（例：2026-11-05），收到「{text}」"),
            ));
        };
        return Ok(AsOf {
            date,
            source: AS_OF_QUERY,
        });
    }
    let period = period.map(str::trim).filter(|text| !text.is_empty());
    if let Some(text) = period {
        let Some((_, end)) = month_range(text) else {
            return Err(ApiError::new(
                400,
                error_code::PARAM_INVALID,
                format!("period 必须是 yyyy-MM（月份 01–12），收到「{text}」"),
            ));
        };
        return Ok(AsOf {
            date: end,
            source: AS_OF_PERIOD,
        });
    }
    Ok(AsOf {
        date: today,
        source: AS_OF_TODAY,
    })
}

/// 逾期天数：基准日 − 到期日（正数 = 已逾期天数）。
pub fn overdue_days(as_of: NaiveDate, due_date: NaiveDate) -> i64 {
    (as_of - due_date).num_days()
}

// ==================== 出参 ====================

/// 档位定义（前端据此渲染表头，后端加档位前端自动跟上）。
#[derive(Debug, Clone, Serialize)]
pub struct AgingBucketDef {
    /// 档位键。
    pub key: String,
    /// 中文名。
    pub label: String,
    /// 逾期天数下界（含）。
    pub min_days: i64,
    /// 逾期天数上界（含）；`null` = 无上界。
    pub max_days: Option<i64>,
}

/// 档位金额（汇总行 / 供应商行的单元格）。
#[derive(Debug, Clone, Serialize)]
pub struct AgingBucketAmountRes {
    /// 档位键。
    pub key: String,
    /// 档位中文名。
    pub label: String,
    /// 未付金额合计。
    pub unpaid: Decimal,
    /// 单据数。
    pub doc_count: i64,
}

/// 供应商一行。
#[derive(Debug, Clone, Serialize)]
pub struct SupplierAgingRes {
    /// 供应商主键（字符串：`nik::uuid::Uuid` 不实现 `Serialize`，DTO 一律出字符串）。
    pub supplier_id: String,
    /// 供应商名称。
    pub supplier_name: String,
    /// 单据数。
    pub doc_count: i64,
    /// 未付合计。
    pub total_unpaid: Decimal,
    /// 各档金额（按键位表顺序，缺档给 0）。
    pub buckets: Vec<AgingBucketAmountRes>,
    /// 最长逾期天数（未逾期记 0）。
    pub max_overdue_days: i64,
}

/// 明细一行。
#[derive(Debug, Clone, Serialize)]
pub struct AgingDetailRes {
    /// 应付单主键（字符串，同 [`SupplierAgingRes::supplier_id`]）。
    pub payable_id: String,
    /// 应付单号。
    pub payable_no: String,
    /// 供应商主键（字符串）。
    pub supplier_id: String,
    /// 供应商名称。
    pub supplier_name: String,
    /// 来源单号（收货单号）。
    pub source_no: String,
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
    /// 应付金额。
    pub amount: Decimal,
    /// 已付金额。
    pub paid_amount: Decimal,
    /// 未付金额。
    pub unpaid_amount: Decimal,
    /// 单据状态。
    pub status: String,
}

/// 账龄分析表出参。
#[derive(Debug, Clone, Serialize)]
pub struct PayableAgingRes {
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
    /// 未付合计（= 各档之和）。
    pub total_unpaid: Decimal,
    /// 逾期合计（= 除「未到期」以外各档之和）。
    pub overdue_unpaid: Decimal,
    /// 逾期占比（0–100，保留 2 位；未付为 0 时为 0）。
    pub overdue_ratio: Decimal,
    /// 应付金额合计（含已付部分，看总量用）。
    pub total_amount: Decimal,
    /// 已付金额合计。
    pub total_paid: Decimal,
    /// 单据数。
    pub doc_count: i64,
    /// 供应商数。
    pub supplier_count: i64,
    /// 供应商汇总（按未付合计倒序）。
    pub suppliers: Vec<SupplierAgingRes>,
    /// 明细（按到期日升序，最多 [`DETAIL_LIMIT`] 条）。
    pub details: Vec<AgingDetailRes>,
    /// 明细总条数（未截断前）。
    pub detail_count: i64,
    /// 明细是否被截断。
    pub detail_truncated: bool,
    /// 口径提示。
    pub hints: Vec<String>,
}

// ==================== 汇总用的中间结构（纯数据，便于单测） ====================

/// SQL 聚合出来的一格：某个供应商 × 某个档位。
#[derive(Debug, Clone)]
pub struct SupplierBucketRow {
    /// 供应商主键。
    pub supplier_id: Uuid,
    /// 供应商名称。
    pub supplier_name: String,
    /// 档位键。
    pub bucket: String,
    /// 单据数。
    pub doc_count: i64,
    /// 未付金额。
    pub unpaid: Decimal,
    /// 最短逾期天数。
    pub min_days: i64,
    /// 最长逾期天数。
    pub max_days: i64,
}

/// 把「供应商 × 档位」的格子按供应商透视成行（纯逻辑）。
///
/// 排序：未付合计倒序 → 供应商名称升序（同额时稳定）；`supplier_id` 兜底保证顺序确定。
pub fn pivot_suppliers(rows: Vec<SupplierBucketRow>) -> Vec<SupplierAgingRes> {
    let mut index: HashMap<Uuid, SupplierAgingRes> = HashMap::new();
    let mut order: Vec<Uuid> = Vec::new();
    for row in rows {
        let entry = index.entry(row.supplier_id).or_insert_with(|| {
            order.push(row.supplier_id);
            SupplierAgingRes {
                supplier_id: row.supplier_id.to_string(),
                supplier_name: row.supplier_name.clone(),
                doc_count: 0,
                total_unpaid: Decimal::ZERO,
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
        entry.total_unpaid += row.unpaid;
        entry.max_overdue_days = entry.max_overdue_days.max(row.max_days.max(0));
        if let Some(cell) = entry.buckets.iter_mut().find(|cell| cell.key == row.bucket) {
            cell.unpaid += row.unpaid;
            cell.doc_count += row.doc_count;
        }
    }
    let mut out: Vec<SupplierAgingRes> = order.into_iter().filter_map(|id| index.remove(&id)).collect();
    out.sort_by(|left, right| {
        right
            .total_unpaid
            .cmp(&left.total_unpaid)
            .then_with(|| left.supplier_name.cmp(&right.supplier_name))
            .then_with(|| left.supplier_id.cmp(&right.supplier_id))
    });
    for supplier in &mut out {
        supplier.total_unpaid = money(supplier.total_unpaid);
        for cell in &mut supplier.buckets {
            cell.unpaid = money(cell.unpaid);
        }
    }
    out
}

/// 把供应商行按档位加总成全公司汇总（纯逻辑）。
pub fn summarise(suppliers: &[SupplierAgingRes]) -> Vec<AgingBucketAmountRes> {
    let mut out: Vec<AgingBucketAmountRes> = BUCKETS
        .iter()
        .map(|bucket| AgingBucketAmountRes {
            key: bucket.key.to_string(),
            label: bucket.label.to_string(),
            unpaid: Decimal::ZERO,
            doc_count: 0,
        })
        .collect();
    for supplier in suppliers {
        for (target, cell) in out.iter_mut().zip(supplier.buckets.iter()) {
            target.unpaid += cell.unpaid;
            target.doc_count += cell.doc_count;
        }
    }
    for cell in &mut out {
        cell.unpaid = money(cell.unpaid);
    }
    out
}

/// 金额归一：`-0` 统一成 `0`，保留 4 位（`{:.2}` 是截断，先 round 再格式化）。
pub fn money(value: Decimal) -> Decimal {
    let value = value.round_dp(4);
    if value.is_zero() { Decimal::ZERO } else { value }
}

// ==================== 取数 ====================

/// 数据层快照。
#[derive(Debug, Clone, Default)]
pub struct AgingSnapshot {
    /// 供应商 × 档位聚合格。
    pub cells: Vec<SupplierBucketRow>,
    /// 应付金额合计。
    pub total_amount: Decimal,
    /// 已付金额合计。
    pub total_paid: Decimal,
    /// 未付金额合计。
    pub total_unpaid: Decimal,
    /// 单据数。
    pub doc_count: i64,
    /// 供应商数。
    pub supplier_count: i64,
    /// 明细（已截断）。
    pub details: Vec<AgingDetailRow>,
}

/// 明细原始行（档位由 Rust 现算，见模块文档）。
#[derive(Debug, Clone)]
pub struct AgingDetailRow {
    /// 应付单主键。
    pub id: Uuid,
    /// 应付单号。
    pub payable_no: String,
    /// 供应商主键。
    pub supplier_id: Uuid,
    /// 供应商名称。
    pub supplier_name: String,
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
    /// 应付金额。
    pub amount: Decimal,
    /// 已付金额。
    pub paid_amount: Decimal,
    /// 未付金额。
    pub unpaid_amount: Decimal,
    /// 状态。
    pub status: String,
}

/// 纳入范围的公共 CTE（三处查询共用，口径只写一遍）。
fn base_cte() -> String {
    let statuses = AGING_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "WITH base AS ( \
             SELECT p.id, p.payable_no, p.supplier_id, p.source_no, p.invoice_no, \
                    p.period_code, p.credit_days, p.due_date, p.amount, p.paid_amount, \
                    p.unpaid_amount, p.status, \
                    (?::date - p.due_date) AS overdue_days, \
                    COALESCE(s.supplier_name, '') AS supplier_name \
               FROM {PAYABLE_TABLE} p \
               LEFT JOIN {SUPPLIER_TABLE} s ON s.id = p.supplier_id AND s.deleted_at IS NULL \
              WHERE p.deleted_at IS NULL \
                AND p.status IN ({statuses}) \
                AND p.unpaid_amount > 0 \
                AND p.period_code <= ? \
         )"
    )
}

/// 按供应商 × 档位聚合。
pub async fn load_cells(tx: &Transaction, as_of: NaiveDate, period_key: &str) -> DbResult<Vec<SupplierBucketRow>> {
    let bucket = bucket_case_sql("overdue_days");
    let sql = format!(
        "{base} \
         SELECT supplier_id, supplier_name, {bucket} AS bucket, \
                COUNT(*) AS doc_count, \
                COALESCE(SUM(unpaid_amount), 0) AS unpaid, \
                COALESCE(MIN(overdue_days), 0)::bigint AS min_days, \
                COALESCE(MAX(overdue_days), 0)::bigint AS max_days \
           FROM base \
          GROUP BY supplier_id, supplier_name, {bucket}",
        base = base_cte()
    );
    let rows = tx
        .find_with_params(&sql, vec![Value::from(as_of), Value::Text(period_key.to_string())])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(SupplierBucketRow {
            supplier_id: row.get_typed("supplier_id")?,
            supplier_name: row.get_typed("supplier_name")?,
            bucket: row.get_typed("bucket")?,
            doc_count: row.get_typed("doc_count")?,
            unpaid: row.get_typed("unpaid")?,
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
) -> DbResult<(Decimal, Decimal, Decimal, i64, i64, Vec<AgingDetailRow>)> {
    let sql = format!(
        "{base} \
         SELECT COALESCE(SUM(amount), 0) AS total_amount, \
                COALESCE(SUM(paid_amount), 0) AS total_paid, \
                COALESCE(SUM(unpaid_amount), 0) AS total_unpaid, \
                COUNT(*) AS doc_count, \
                COUNT(DISTINCT supplier_id) AS supplier_count \
           FROM base",
        base = base_cte()
    );
    let row = tx
        .find_with_params(&sql, vec![Value::from(as_of), Value::Text(period_key.to_string())])
        .await?
        .into_iter()
        .next();
    let (total_amount, total_paid, total_unpaid, doc_count, supplier_count) = match row {
        Some(row) => (
            row.get_typed::<Decimal>("total_amount")?,
            row.get_typed::<Decimal>("total_paid")?,
            row.get_typed::<Decimal>("total_unpaid")?,
            row.get_typed::<i64>("doc_count")?,
            row.get_typed::<i64>("supplier_count")?,
        ),
        None => (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, 0, 0),
    };

    let sql = format!(
        "{base} \
         SELECT id, payable_no, supplier_id, supplier_name, source_no, invoice_no, \
                period_code, credit_days, due_date, amount, paid_amount, unpaid_amount, status \
           FROM base \
          ORDER BY due_date ASC, payable_no ASC \
          LIMIT {DETAIL_LIMIT}",
        base = base_cte()
    );
    let rows = tx
        .find_with_params(&sql, vec![Value::from(as_of), Value::Text(period_key.to_string())])
        .await?;
    let mut details = Vec::with_capacity(rows.len());
    for row in &rows {
        details.push(AgingDetailRow {
            id: row.get_typed("id")?,
            payable_no: row.get_typed("payable_no")?,
            supplier_id: row.get_typed("supplier_id")?,
            supplier_name: row.get_typed("supplier_name")?,
            source_no: row.get_typed("source_no")?,
            invoice_no: row.get_typed("invoice_no")?,
            period_code: row.get_typed("period_code")?,
            credit_days: row.get_typed("credit_days")?,
            due_date: row.get_typed("due_date")?,
            amount: row.get_typed("amount")?,
            paid_amount: row.get_typed("paid_amount")?,
            unpaid_amount: row.get_typed("unpaid_amount")?,
            status: row.get_typed("status")?,
        });
    }
    Ok((
        total_amount,
        total_paid,
        total_unpaid,
        doc_count,
        supplier_count,
        details,
    ))
}

/// 在 REPEATABLE READ 快照里取全套数据。
pub async fn build_snapshot(tx: &Transaction, as_of: NaiveDate, period_key: &str) -> DbResult<AgingSnapshot> {
    let cells = load_cells(tx, as_of, period_key).await?;
    let (total_amount, total_paid, total_unpaid, doc_count, supplier_count, details) =
        load_totals_and_details(tx, as_of, period_key).await?;
    Ok(AgingSnapshot {
        cells,
        total_amount,
        total_paid,
        total_unpaid,
        doc_count,
        supplier_count,
        details,
    })
}

/// 期间信息（状态 / 中文名）。期间行不存在时返回 `None`（账龄表不因未开账而 400）。
pub async fn load_period_status(tx: &Transaction, period: &str) -> DbResult<Option<String>> {
    let sql = "SELECT status FROM biz_accounting_period WHERE period_code = ?".to_string();
    let row = tx
        .find_with_params(&sql, vec![Value::Text(period.to_string())])
        .await?
        .into_iter()
        .next();
    match row {
        Some(row) => Ok(Some(row.get_typed("status")?)),
        None => Ok(None),
    }
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
    snapshot: AgingSnapshot,
    company: &str,
) -> PayableAgingRes {
    let suppliers = pivot_suppliers(snapshot.cells);
    let summary = summarise(&suppliers);
    let total_unpaid = money(snapshot.total_unpaid);
    let overdue_unpaid = money(
        summary
            .iter()
            .filter(|cell| cell.key != BUCKETS[0].key)
            .map(|cell| cell.unpaid)
            .sum::<Decimal>(),
    );
    let overdue_ratio = if total_unpaid.is_zero() {
        Decimal::ZERO
    } else {
        (overdue_unpaid * Decimal::new(100, 0) / total_unpaid).round_dp(2)
    };

    let detail_count = snapshot.doc_count;
    let detail_truncated = detail_count as usize > snapshot.details.len();
    let details: Vec<AgingDetailRes> = snapshot
        .details
        .iter()
        .map(|row| {
            let days = overdue_days(as_of.date, row.due_date);
            let key = bucket_of(days);
            let label = BUCKETS
                .iter()
                .find(|bucket| bucket.key == key)
                .map(|bucket| bucket.label)
                .unwrap_or(key);
            AgingDetailRes {
                payable_id: row.id.to_string(),
                payable_no: row.payable_no.clone(),
                supplier_id: row.supplier_id.to_string(),
                supplier_name: row.supplier_name.clone(),
                source_no: row.source_no.clone(),
                invoice_no: row.invoice_no.clone(),
                period_code: row.period_code.clone(),
                credit_days: row.credit_days,
                due_date: row.due_date,
                overdue_days: days,
                bucket: key.to_string(),
                bucket_label: label.to_string(),
                amount: money(row.amount),
                paid_amount: money(row.paid_amount),
                unpaid_amount: money(row.unpaid_amount),
                status: row.status.clone(),
            }
        })
        .collect();

    let mut hints = vec![format!(
        "口径：只统计未付清的应付单（{}），金额取未付余额（应付 − 已付）；\
         逾期天数 = 基准日 − 到期日，到期当天（0 天）算未到期",
        AGING_STATUSES.join(" / ")
    )];
    hints.push(format!(
        "分档：{}（与应收账龄同一张档位表，7 档口径可直接对照）",
        AGING_BUCKETS
            .iter()
            .map(|bucket| bucket.label)
            .collect::<Vec<_>>()
            .join(" / ")
    ));
    hints.push(
        "金额是「当前」未付余额快照：基准日之后发生的付款不会回滚成历史值（本表不做历史余额重算）；\
         单据按「所属会计期间 ≤ 基准日所属期间」纳入"
            .to_string(),
    );
    if snapshot.doc_count == 0 {
        hints.push(format!(
            "基准日 {} 下没有未付清的应付单（没有数据不是出错）",
            as_of.date
        ));
    }
    if detail_truncated {
        hints.push(format!(
            "明细共 {detail_count} 条，本表只返回前 {DETAIL_LIMIT} 条（按到期日升序）；\
             汇总与供应商表是全量，不受明细截断影响"
        ));
    }

    PayableAgingRes {
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
        total_unpaid,
        overdue_unpaid,
        overdue_ratio,
        total_amount: money(snapshot.total_amount),
        total_paid: money(snapshot.total_paid),
        doc_count: snapshot.doc_count,
        supplier_count: snapshot.supplier_count,
        suppliers,
        details,
        detail_count,
        detail_truncated,
        hints,
    }
}

// ==================== 接口层 ====================

/// 参数校验 + 快照事务统一入口。
async fn build_report(query: &PayableAgingQuery) -> std::result::Result<PayableAgingRes, ApiError> {
    let today = voucher::today_cst(Utc::now());
    let as_of = resolve_as_of(query.as_of.as_deref(), query.period.as_deref(), today)?;
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
        .map_err(|error| utils::db_error(error, "开启应付账龄快照事务"))?;
    // 整张表在一个 REPEATABLE READ 快照里算完（与资产负债表同口径）
    if let Err(error) = tx.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").await {
        let _ = tx.rollback().await;
        return Err(utils::db_error(error, "设置账龄报表事务隔离级别"));
    }
    let period_status = match &period {
        Some(period) => match load_period_status(&tx, period).await {
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
            return Err(utils::db_error(error, "计算应付账龄"));
        }
    };
    if let Err(error) = tx.commit().await {
        return Err(utils::db_error(error, "提交应付账龄快照事务"));
    }
    let company = company_name().await;
    Ok(assemble(as_of, period.as_deref(), period_status, snapshot, &company))
}

/// 应付账款账龄分析表。
///
/// `GET /business/erp/report/payable-aging?period=yyyy-MM&as_of=yyyy-MM-dd`
#[nik::permission(
    path = "/business/erp/report/payable-aging",
    method = "GET",
    permission = "business:erp:report:payable-aging"
)]
#[nik::get("/business/erp/report/payable-aging")]
pub async fn payable_aging_handler(Query(query): Query<PayableAgingQuery>) -> ResResult<PayableAgingRes> {
    Ok(Res::ok(build_report(&query).await?))
}

/// 导出应付账龄分析表（xlsx，供应商汇总 + 合计行）。
///
/// 权限码复用报表导出码 `business:erp:report:export`（V0183 已种，财务角色已有）。
#[nik::permission(
    path = "/business/erp/report/payable-aging/export",
    method = "GET",
    permission = "business:erp:report:export"
)]
#[nik::get("/business/erp/report/payable-aging/export")]
pub async fn payable_aging_export_handler(
    Query(query): Query<PayableAgingQuery>,
) -> std::result::Result<Response, ApiError> {
    let report = build_report(&query).await?;
    let (headers, rows) = export_table(&report);
    let bytes = crate::business::impexp::engine::write_sheet(&headers, &rows)?;
    Ok(crate::business::impexp::handler::xlsx_response(
        bytes,
        &format!(
            "{REPORT_NAME}_{}_截止{}.xlsx",
            report.period.clone().unwrap_or_else(|| "无期间".to_string()),
            report.as_of,
        ),
    ))
}

/// 导出表的表头 + 行（表头与 [`BUCKETS`] 顺序一致）。
pub fn export_table(report: &PayableAgingRes) -> (Vec<&'static str>, Vec<HashMap<String, String>>) {
    let mut headers: Vec<&'static str> = vec!["供应商", "单据数"];
    for bucket in &BUCKETS {
        headers.push(bucket.label);
    }
    headers.push("未付合计");
    headers.push("最长逾期天数");

    let mut rows: Vec<HashMap<String, String>> = Vec::with_capacity(report.suppliers.len() + 1);
    for supplier in &report.suppliers {
        let mut row: HashMap<String, String> = HashMap::new();
        row.insert("供应商".to_string(), supplier.supplier_name.clone());
        row.insert("单据数".to_string(), supplier.doc_count.to_string());
        for cell in &supplier.buckets {
            let label = BUCKETS
                .iter()
                .find(|bucket| bucket.key == cell.key)
                .map(|bucket| bucket.label)
                .unwrap_or(cell.key.as_str());
            row.insert(label.to_string(), money_text(cell.unpaid));
        }
        row.insert("未付合计".to_string(), money_text(supplier.total_unpaid));
        row.insert("最长逾期天数".to_string(), supplier.max_overdue_days.to_string());
        rows.push(row);
    }

    let mut total: HashMap<String, String> = HashMap::new();
    total.insert("供应商".to_string(), "合计".to_string());
    total.insert("单据数".to_string(), report.doc_count.to_string());
    for cell in &report.summary {
        let label = BUCKETS
            .iter()
            .find(|bucket| bucket.key == cell.key)
            .map(|bucket| bucket.label)
            .unwrap_or(cell.key.as_str());
        total.insert(label.to_string(), money_text(cell.unpaid));
    }
    total.insert("未付合计".to_string(), money_text(report.total_unpaid));
    total.insert(
        "最长逾期天数".to_string(),
        report
            .suppliers
            .iter()
            .map(|supplier| supplier.max_overdue_days)
            .max()
            .unwrap_or(0)
            .to_string(),
    );
    rows.push(total);

    (headers, rows)
}

/// 金额文本：千分位 + 2 位小数（与资产负债表导出同一口径：先 `round_dp(2)` 再定点打印）。
pub fn money_text(value: Decimal) -> String {
    let text = format!("{:.2}", value.round_dp(2));
    let (integer, fraction) = text.split_once('.').unwrap_or((text.as_str(), "00"));
    let negative = integer.starts_with('-');
    let digits: String = integer.chars().filter(|c| c.is_ascii_digit()).collect();
    let grouped = digits
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect::<Vec<_>>()
        .join(",");
    let integer = if grouped.is_empty() { "0".to_string() } else { grouped };
    let is_zero = digits.chars().all(|c| c == '0') && fraction == "00";
    format!("{}{integer}.{fraction}", if negative && !is_zero { "-" } else { "" })
}

// ==================== 单测（分档边界 / 基准日 / 透视） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        parse_date(text).expect("合法日期")
    }

    /// ① 分档边界（7 档，与应收同口径）：0 天算未到期，1/30/31/60/61/90/91/180/181/365/366 逐格钉住。
    #[test]
    fn bucket_boundaries_are_exact() {
        assert_eq!(bucket_of(-120), "NOT_DUE");
        assert_eq!(bucket_of(-1), "NOT_DUE");
        assert_eq!(bucket_of(0), "NOT_DUE", "到期当天不算逾期");
        assert_eq!(bucket_of(1), "D0_30");
        assert_eq!(bucket_of(30), "D0_30");
        assert_eq!(bucket_of(31), "D31_60");
        assert_eq!(bucket_of(60), "D31_60");
        assert_eq!(bucket_of(61), "D61_90");
        assert_eq!(bucket_of(90), "D61_90");
        assert_eq!(bucket_of(91), "D91_180", "M1-21：91 天起进 91-180，不再一锅端");
        assert_eq!(bucket_of(180), "D91_180");
        assert_eq!(bucket_of(181), "D181_365");
        assert_eq!(bucket_of(365), "D181_365");
        assert_eq!(bucket_of(366), "OVER_365");
        assert_eq!(bucket_of(3650), "OVER_365");
    }

    /// ①b 应付与应收**共用同一张档位表**（M1-21 起）：`BUCKETS == AGING_BUCKETS`，
    ///     两边逐天同档，不存在第二份阈值。
    ///
    /// 这条是「一张表、两个使用者」的机器化证明：谁在任何一侧偷偷加一张表 / 改阈值，这里立刻红。
    #[test]
    fn payable_and_receivable_share_one_bucket_table() {
        assert_eq!(
            BUCKETS, AGING_BUCKETS,
            "应付的 BUCKETS 必须是同一张表（别名，不是副本）"
        );
        assert_eq!(AGING_BUCKETS.len(), 7, "未到期 + 30/60/90/180/365 六段");
        assert_eq!(
            AGING_BUCKETS.map(|bucket| bucket.key),
            [
                "NOT_DUE", "D0_30", "D31_60", "D61_90", "D91_180", "D181_365", "OVER_365"
            ]
        );
        // 逐天：-400 ..= 3650 两边完全一致（不是「差不多」）
        for days in -400..=3650i64 {
            assert_eq!(
                bucket_of_in(&AGING_BUCKETS, days),
                bucket_of(days),
                "第 {days} 天两表必须同档"
            );
        }
        // 关键边界逐个钉住（180 / 365 两刀，应付侧也不例外）
        let expect = [
            (91, "D91_180"),
            (180, "D91_180"),
            (181, "D181_365"),
            (365, "D181_365"),
            (366, "OVER_365"),
            (3650, "OVER_365"),
        ];
        for (days, key) in expect {
            assert_eq!(bucket_of_in(&AGING_BUCKETS, days), key, "应收第 {days} 天");
            assert_eq!(bucket_of(days), key, "应付第 {days} 天（M1-21 起与应收同档）");
        }
        // SQL 侧与 Rust 侧同源（应付的薄包装与应收的显式调用逐字相同）
        let sql = bucket_case_sql_in(&AGING_BUCKETS, "d");
        assert_eq!(
            bucket_case_sql("d"),
            sql,
            "应付的 CASE 必须与应收用同一张表生成（逐字相同）"
        );
        assert!(sql.contains("WHEN d <= 180 THEN 'D91_180'"), "实际：{sql}");
        assert!(sql.contains("WHEN d <= 365 THEN 'D181_365'"), "实际：{sql}");
        assert!(sql.ends_with("ELSE 'OVER_365' END"), "实际：{sql}");
        assert!(!sql.contains("OVER_90"), "7 档口径里不该再有「90 天以上」这颗兜底档");
        // 出参生成器按表宽度给出
        assert_eq!(empty_bucket_cells(&AGING_BUCKETS).len(), 7);
        assert_eq!(bucket_defs(&AGING_BUCKETS).len(), 7);
        assert_eq!(empty_bucket_cells(&AGING_BUCKETS)[6].label, "365 天以上");
        assert_eq!(bucket_defs(&AGING_BUCKETS)[5].max_days, Some(365));
    }

    /// ② 每个档位的上下界与 `bucket_of` 自洽（下界 - 1 必须落到上一档）。
    #[test]
    fn buckets_are_contiguous_and_cover_all_days() {
        let mut previous_max: Option<i64> = None;
        for bucket in &BUCKETS {
            match (previous_max, bucket.max_days) {
                (Some(prev), Some(_)) => assert_eq!(prev + 1, bucket.min_days, "档位 {} 与上一档不连续", bucket.key),
                (None, Some(_)) => {}
                (_, None) => {}
            }
            if let Some(max) = bucket.max_days {
                assert_eq!(bucket_of(max), bucket.key, "上界 {} 应落在 {}", max, bucket.key);
                assert_ne!(bucket_of(max + 1), bucket.key, "上界 + 1 必须离开 {}", bucket.key);
            } else {
                assert_eq!(bucket_of(i64::MAX), bucket.key);
            }
            previous_max = bucket.max_days;
        }
    }

    /// ③ 逾期天数：基准日 − 到期日。
    #[test]
    fn overdue_days_counts_from_due_date() {
        assert_eq!(overdue_days(date("2026-11-05"), date("2026-11-05")), 0);
        assert_eq!(overdue_days(date("2026-11-05"), date("2026-11-04")), 1);
        assert_eq!(overdue_days(date("2026-11-05"), date("2026-10-06")), 30);
        assert_eq!(overdue_days(date("2026-11-05"), date("2026-10-05")), 31);
        assert_eq!(overdue_days(date("2026-11-05"), date("2026-11-10")), -5);
        // 跨月 / 跨年边界
        assert_eq!(overdue_days(date("2027-01-01"), date("2026-12-31")), 1);
        assert_eq!(overdue_days(date("2026-03-01"), date("2026-02-28")), 1);
    }

    /// ④ 基准日解析：as_of 优先 → period 末日 → 今天；非法值 400。
    #[test]
    fn as_of_resolution_priority_and_validation() {
        let today = date("2026-11-05");
        let by_query = resolve_as_of(Some("2026-09-30"), Some("2026-10"), today).unwrap();
        assert_eq!(by_query.date, date("2026-09-30"));
        assert_eq!(by_query.source, AS_OF_QUERY);

        let by_period = resolve_as_of(None, Some("2026-02"), today).unwrap();
        assert_eq!(by_period.date, date("2026-02-28"), "2 月末日按闰年规则");
        assert_eq!(by_period.source, AS_OF_PERIOD);

        let by_period_dec = resolve_as_of(Some("  "), Some("2026-12"), today).unwrap();
        assert_eq!(by_period_dec.date, date("2026-12-31"));

        let fallback = resolve_as_of(None, None, today).unwrap();
        assert_eq!(fallback.date, today);
        assert_eq!(fallback.source, AS_OF_TODAY);

        let err = resolve_as_of(Some("2026-1-1"), None, today).unwrap_err();
        assert_eq!(err.status, 400);
        let err = resolve_as_of(None, Some("2026-13"), today).unwrap_err();
        assert_eq!(err.status, 400);
        let err = resolve_as_of(Some("2026-02-30"), None, today).unwrap_err();
        assert_eq!(err.status, 400);
    }

    /// ⑤ SQL 分档表达式与常量同源：七个档位键、六道阈值都在（M1-21 起 7 档）。
    #[test]
    fn bucket_case_sql_mirrors_constants() {
        let sql = bucket_case_sql("overdue_days");
        assert!(sql.starts_with("CASE WHEN overdue_days <= 0 THEN 'NOT_DUE'"));
        assert!(sql.contains("WHEN overdue_days <= 30 THEN 'D0_30'"));
        assert!(sql.contains("WHEN overdue_days <= 60 THEN 'D31_60'"));
        assert!(sql.contains("WHEN overdue_days <= 90 THEN 'D61_90'"));
        assert!(sql.contains("WHEN overdue_days <= 180 THEN 'D91_180'"));
        assert!(sql.contains("WHEN overdue_days <= 365 THEN 'D181_365'"));
        assert!(sql.ends_with("ELSE 'OVER_365' END"));
        assert!(!sql.contains("OVER_90"), "7 档口径里不该再有「90 天以上」兜底：{sql}");
    }

    /// ⑥ 供应商透视：缺档补 0、合计正确、按未付倒序、最长逾期取 max。
    #[test]
    fn pivot_fills_missing_buckets_and_sorts_by_amount() {
        let alpha = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let beta = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let rows = vec![
            SupplierBucketRow {
                supplier_id: alpha,
                supplier_name: "阿尔法".to_string(),
                bucket: "D31_60".to_string(),
                doc_count: 2,
                unpaid: Decimal::new(30000, 2),
                min_days: 35,
                max_days: 59,
            },
            SupplierBucketRow {
                supplier_id: beta,
                supplier_name: "贝塔".to_string(),
                bucket: "D91_180".to_string(),
                doc_count: 1,
                unpaid: Decimal::new(90000, 2),
                min_days: 120,
                max_days: 120,
            },
            SupplierBucketRow {
                supplier_id: alpha,
                supplier_name: "阿尔法".to_string(),
                bucket: "NOT_DUE".to_string(),
                doc_count: 1,
                unpaid: Decimal::new(10000, 2),
                min_days: -20,
                max_days: -20,
            },
        ];
        let suppliers = pivot_suppliers(rows);
        assert_eq!(suppliers.len(), 2);
        // 贝塔 900 > 阿尔法 400 → 贝塔在前
        assert_eq!(suppliers[0].supplier_name, "贝塔");
        assert_eq!(suppliers[0].total_unpaid, Decimal::new(90000, 2));
        assert_eq!(suppliers[0].max_overdue_days, 120);
        // 阿尔法：两个档位有值，其余为 0
        let alpha_row = &suppliers[1];
        assert_eq!(alpha_row.doc_count, 3);
        assert_eq!(alpha_row.total_unpaid, Decimal::new(40000, 2));
        assert_eq!(alpha_row.buckets.len(), BUCKETS.len());
        let amount_of = |key: &str| {
            alpha_row
                .buckets
                .iter()
                .find(|cell| cell.key == key)
                .map(|cell| cell.unpaid)
                .unwrap()
        };
        assert_eq!(amount_of("NOT_DUE"), Decimal::new(10000, 2));
        assert_eq!(amount_of("D31_60"), Decimal::new(30000, 2));
        assert_eq!(amount_of("D0_30"), Decimal::ZERO);
        // M1-21 起 7 档：长账龄三段（91-180 / 181-365 / 365 天以上）都要有格子且缺档补 0
        assert_eq!(amount_of("D91_180"), Decimal::ZERO);
        assert_eq!(amount_of("D181_365"), Decimal::ZERO);
        assert_eq!(amount_of("OVER_365"), Decimal::ZERO);
        // 未逾期的最长逾期按 0（不出现负数）
        assert_eq!(alpha_row.max_overdue_days, 59);
    }

    /// ⑦ 汇总：各档之和 = 未付合计，逾期合计 = 除未到期之外的和。
    #[test]
    fn summary_sums_match_suppliers() {
        let alpha = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let rows = vec![
            SupplierBucketRow {
                supplier_id: alpha,
                supplier_name: "阿尔法".to_string(),
                bucket: "NOT_DUE".to_string(),
                doc_count: 1,
                unpaid: Decimal::new(10000, 2),
                min_days: -3,
                max_days: -3,
            },
            SupplierBucketRow {
                supplier_id: alpha,
                supplier_name: "阿尔法".to_string(),
                bucket: "D91_180".to_string(),
                doc_count: 1,
                unpaid: Decimal::new(50000, 2),
                min_days: 100,
                max_days: 100,
            },
        ];
        let suppliers = pivot_suppliers(rows);
        let summary = summarise(&suppliers);
        let total: Decimal = summary.iter().map(|cell| cell.unpaid).sum();
        assert_eq!(total, Decimal::new(60000, 2));
        let overdue: Decimal = summary
            .iter()
            .filter(|cell| cell.key != "NOT_DUE")
            .map(|cell| cell.unpaid)
            .sum();
        assert_eq!(overdue, Decimal::new(50000, 2));
    }

    /// ⑧ 金额文本：千分位 + 2 位，负零归一。
    #[test]
    fn money_text_formats_like_the_pdf() {
        assert_eq!(money_text(Decimal::new(123456789, 4)), "12,345.68");
        assert_eq!(money_text(Decimal::new(500000, 4)), "50.00");
        assert_eq!(money_text(Decimal::ZERO), "0.00");
    }

    /// ⑨ 导出表：列序 = 页面主表 = **7 档** + 前后各两列（共 11 列）；
    ///    长账龄三段各占一列，缺档写 `0.00` 而不是空串（M1-21 补分档后 Excel 必须跟着变宽）。
    #[test]
    fn export_table_has_seven_bucket_columns() {
        let alpha = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let suppliers = pivot_suppliers(vec![
            SupplierBucketRow {
                supplier_id: alpha,
                supplier_name: "阿尔法".to_string(),
                bucket: "D181_365".to_string(),
                doc_count: 2,
                unpaid: Decimal::new(123456789, 4),
                min_days: 200,
                max_days: 300,
            },
            SupplierBucketRow {
                supplier_id: alpha,
                supplier_name: "阿尔法".to_string(),
                bucket: "NOT_DUE".to_string(),
                doc_count: 1,
                unpaid: Decimal::new(5000000, 4),
                min_days: -5,
                max_days: -5,
            },
        ]);
        let summary = summarise(&suppliers);
        let report = PayableAgingRes {
            report_code: REPORT_CODE.to_string(),
            report_name: REPORT_NAME.to_string(),
            company_name: "云枢".to_string(),
            unit: "元".to_string(),
            as_of: date("2026-11-05"),
            as_of_source: AS_OF_QUERY.to_string(),
            period: None,
            period_name: None,
            period_status: None,
            generated_at: Utc::now(),
            buckets: bucket_defs(&BUCKETS),
            summary,
            total_unpaid: Decimal::new(128456789, 4),
            overdue_unpaid: Decimal::new(123456789, 4),
            overdue_ratio: Decimal::new(9610, 2),
            total_amount: Decimal::new(128456789, 4),
            total_paid: Decimal::ZERO,
            doc_count: 3,
            supplier_count: 1,
            suppliers,
            details: Vec::new(),
            detail_count: 3,
            detail_truncated: false,
            hints: Vec::new(),
        };
        let (headers, rows) = export_table(&report);
        assert_eq!(
            headers,
            vec![
                "供应商",
                "单据数",
                "未到期",
                "1-30 天",
                "31-60 天",
                "61-90 天",
                "91-180 天",
                "181-365 天",
                "365 天以上",
                "未付合计",
                "最长逾期天数",
            ],
            "列序 = 页面上表；档位列由 BUCKETS 生成"
        );
        assert_eq!(headers.len(), BUCKETS.len() + 4);
        assert_eq!(rows.len(), 2, "一个供应商行 + 合计行");
        let supplier_row = &rows[0];
        for header in &headers {
            assert!(supplier_row.contains_key(*header), "供应商行缺列 {header}");
        }
        assert_eq!(supplier_row.get("181-365 天").unwrap(), "12,345.68");
        assert_eq!(supplier_row.get("91-180 天").unwrap(), "0.00", "缺档补 0.00");
        assert_eq!(supplier_row.get("365 天以上").unwrap(), "0.00", "缺档补 0.00");
        assert_eq!(supplier_row.get("未付合计").unwrap(), "12,845.68");
        assert_eq!(supplier_row.get("最长逾期天数").unwrap(), "300");
        let total_row = &rows[1];
        assert_eq!(total_row.get("供应商").unwrap(), "合计");
        assert_eq!(total_row.get("单据数").unwrap(), "3");
        for header in &headers {
            assert!(total_row.contains_key(*header), "合计行缺列 {header}");
        }
        assert_eq!(total_row.get("181-365 天").unwrap(), "12,345.68");
        assert_eq!(total_row.get("91-180 天").unwrap(), "0.00");
        assert_eq!(total_row.get("365 天以上").unwrap(), "0.00");
        assert_eq!(total_row.get("未付合计").unwrap(), "12,845.68");
    }
}

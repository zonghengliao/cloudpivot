//! 库龄分段报表取数（M2-09）：一条聚合 SQL，把库龄层按 物料 × 仓库 × 档位 汇总。
//!
//! 卡：t_4e2cb6f6（M2-09【M2·仓库】库龄分析：报表（0-30/31-60/61-90/90+））。
//!
//! # 算术在库层，本文件只分组
//!
//! ```text
//! biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)   ← V0424（M2-08）唯一 FIFO 实现
//!   └─ 每笔 IN 一层：inbound_date / qty_remaining / amount_remaining / unit_cost
//!        └─ 本文件：age_days = p_as_of − inbound_date → GROUP BY 物料, 仓库, 档位
//! ```
//!
//! 本文件里**没有**窗口函数、没有累计、没有夹 0 —— 「结存由哪些层构成」是 M2-08 的事，
//! 这里只做 `SUM` 与分档。M2-08 的模块头注释已经把这条写死了：
//! 报表 / 呆滞料 / 明细三处各写一遍 FIFO，漂了不会有任何地方报错。
//!
//! # 分档表达式也只有一个来源
//!
//! `CASE WHEN age_days <= 30 THEN 'D0_30' …` 由
//! [`payable_aging::bucket_case_sql_in`] 按 [`LAYER_BUCKETS`] 生成，Rust 侧的
//! [`super::dto::bucket_of`] 用同一张表判 —— 阈值只写在档位表一处。
//!
//! # 为什么带 `WITH` 而不是把日期算术塞进 `CASE`
//!
//! 档位 `CASE` 要出现在 `SELECT` 与 `GROUP BY` 两处（Postgres 允许按别名分组，
//! 但 `CASE` 里带参数时，两处展开会让占位符个数翻倍、参数顺序变得难以核对）。
//! 所以先把 `age_days` 算成列（`with_age`），`CASE` 里只引用列名，**零占位符**。
//!
//! 代价是 `as_of` 出现两次（函数入参一次 + 算 `age_days` 一次）：参数顺序是
//! `[as_of, material_id, warehouse_id, as_of]`，单测 `params_order_is_documented`
//! 逐个钉住，改 SQL 时忘改参数会当场红。
//!
//! # 只取还有结存的层
//!
//! `WHERE l.qty_remaining > 0`：被 FIFO 吃满的层是历史（剩 0），混进来只会把
//! 「层数」变成「历史上有过几笔入库」。**数量上两者等价**（剩 0 加不加都是 0），
//! 但层数与最老日期会跑偏（一个老层吃满后仍会把「最老日期」拉到很早）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_inventory_txn 无 dept_id / 归属人列（V0137），数据范围表达不出来；
//!               与 M2-08 的批次层分页同锚点（inventory_aging::model::AGING_TARGET），
//!               可见性由权限码 business:erp:inventory-aging:report 控制。
//! ```
//!
//! 一次聚合查询 = 一个快照，所以**不用**开 REPEATABLE READ 事务（应收 / 应付账龄要开，
//! 是因为它们分多条 SQL 取明细与汇总）；主数据名字只是标签，晚一拍没有账务含义。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::model::parse_decimal;
use crate::business::erp::inventory_aging::report::dto::{CellRow, LAYER_BUCKETS};
use crate::business::erp::payable::payable_aging;

/// 本模块依赖的库函数（V0424 建；改名要同时改这里与迁移自检）。
pub const AGING_FUNCTION: &str = "biz_inventory_aging_layers";

/// 组聚合 SQL：`WITH` 层 → 算 `age_days` → 按 物料 × 仓库 × 档位 `GROUP BY`。
///
/// `?` 顺序 = `[as_of, material_id, warehouse_id, as_of]`（见模块头注释）。
pub fn cells_sql() -> String {
    // 档位表达式按库龄档位表生成（本模块不写死阈值）
    let bucket = payable_aging::bucket_case_sql_in(&LAYER_BUCKETS, "age_days");
    format!(
        "WITH layers AS ( \
             SELECT l.material_id, l.warehouse_id, l.inbound_date, l.qty_remaining, l.amount_remaining \
               FROM {AGING_FUNCTION}(?::text::date, NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid) l \
              WHERE l.qty_remaining > 0 \
         ), \
         with_age AS ( \
             SELECT w.material_id, w.warehouse_id, w.inbound_date, w.qty_remaining, w.amount_remaining, \
                    (?::text::date - w.inbound_date) AS age_days \
               FROM layers w \
         ) \
         SELECT material_id, warehouse_id, {bucket} AS bucket, \
                COUNT(*)::bigint AS layer_count, \
                SUM(qty_remaining)::text AS qty, \
                SUM(amount_remaining)::text AS amount, \
                MIN(inbound_date) AS oldest_inbound_date, \
                MAX(age_days)::bigint AS max_age_days, \
                COALESCE(SUM(qty_remaining * age_days) / NULLIF(SUM(qty_remaining), 0), 0)::text AS avg_age_days \
           FROM with_age \
          GROUP BY material_id, warehouse_id, {bucket}"
    )
}

/// 聚合参数（顺序与 [`cells_sql`] 的占位符逐个对应）。
///
/// 空物料 / 仓库传空串 → `NULLIF(?::text,'')` → `NULL` = 不过滤
/// （驱动不接受 `Value::Null` 绑 `?::uuid`，全仓统一这条写法）。
pub fn cells_params(as_of: NaiveDate, material_id: Option<Uuid>, warehouse_id: Option<Uuid>) -> Vec<Value> {
    let as_of = Value::Text(as_of.to_string());
    vec![
        as_of.clone(),
        Value::Text(material_id.map(|id| id.to_string()).unwrap_or_default()),
        Value::Text(warehouse_id.map(|id| id.to_string()).unwrap_or_default()),
        as_of,
    ]
}

/// 按 物料 × 仓库 × 档位 聚合（返回的是「格子」，透视成行由 [`super::dto::pivot_rows`] 做）。
///
/// # 异常情况
///
/// 查库 / 解析失败 → [`DbError`]（handler 直接 `?`）；数值列一律 `::text` 出来再解析成
/// 定点数（驱动不做 `numeric → Decimal` 的直接映射，与 M2-08 的 `parse_decimal` 同一实现）。
pub async fn cells(as_of: NaiveDate, material_id: Option<Uuid>, warehouse_id: Option<Uuid>) -> DbResult<Vec<CellRow>> {
    let rows = db()
        .find_with_params(&cells_sql(), cells_params(as_of, material_id, warehouse_id))
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(CellRow {
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            bucket: row.get_typed("bucket")?,
            layer_count: row.get_typed("layer_count")?,
            qty: parse_decimal(row, "qty")?,
            amount: parse_decimal(row, "amount")?,
            oldest_inbound_date: row.get_typed("oldest_inbound_date")?,
            max_age_days: row.get_typed("max_age_days")?,
            avg_age_days: parse_decimal(row, "avg_age_days")?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    /// SQL 必须 FROM 库函数（不是自己再拼一遍流水），且不许出现窗口函数 / 累计。
    #[test]
    fn sql_reuses_the_aging_function_and_has_no_fifo() {
        let sql = cells_sql();
        assert!(
            sql.contains("FROM biz_inventory_aging_layers("),
            "报表必须复用 M2-08 的库函数：{sql}"
        );
        assert!(!sql.contains("biz_inventory_txn"), "不许自己去读流水再摊层：{sql}");
        let upper = sql.to_uppercase();
        assert!(!upper.contains("OVER ("), "不许有窗口函数（FIFO 在库函数里）：{sql}");
        assert!(
            !upper.contains("ORDER BY"),
            "聚合 SQL 不需要 ORDER BY（行序在 Rust 排）：{sql}"
        );
        assert!(!upper.contains("LIMIT"), "报表不做分页（行上限在 Rust 截）：{sql}");
        assert!(sql.contains("WHERE l.qty_remaining > 0"), "只看还有结存的层：{sql}");
        println!("[M2-09] 聚合 SQL：复用库龄层函数、无窗口函数 / 无 ORDER BY / 只看有结存的层 ✓");
    }

    /// 档位 `CASE` 由档位表生成，四档全在、边界与表一致。
    #[test]
    fn bucket_case_is_generated_from_the_bucket_table() {
        let sql = cells_sql();
        for bucket in LAYER_BUCKETS {
            assert!(sql.contains(bucket.key), "缺档位 {}：{sql}", bucket.key);
        }
        assert!(sql.contains("WHEN age_days <= 30 THEN 'D0_30'"), "{sql}");
        assert!(sql.contains("WHEN age_days <= 60 THEN 'D31_60'"), "{sql}");
        assert!(sql.contains("WHEN age_days <= 90 THEN 'D61_90'"), "{sql}");
        assert!(sql.contains("ELSE 'OVER_90'"), "{sql}");
        // CASE 里不许带占位符（否则 SELECT + GROUP BY 两处展开会让参数个数翻倍）
        let case = sql.split("SELECT material_id").nth(1).unwrap_or_default();
        let case = case.split("AS bucket").next().unwrap_or_default();
        assert!(!case.contains('?'), "档位 CASE 里不许有占位符：{case}");
        println!("[M2-09] 档位 CASE 由 LAYER_BUCKETS 生成（四档齐、CASE 零占位符）✓");
    }

    /// 占位符个数与参数个数逐个对得上，且顺序是 `[as_of, material, warehouse, as_of]`。
    #[test]
    fn params_order_is_documented() {
        let sql = cells_sql();
        assert_eq!(sql.matches('?').count(), 4, "占位符 4 个：{sql}");
        let params = cells_params(
            date("2026-11-16"),
            Some(Uuid::from_u128(0x11)),
            Some(Uuid::from_u128(0x22)),
        );
        assert_eq!(params.len(), 4);
        assert_eq!(params[0], Value::Text("2026-11-16".to_string()), "第 1 个是函数 as_of");
        assert_eq!(
            params[1],
            Value::Text(Uuid::from_u128(0x11).to_string()),
            "第 2 个是物料（函数入参）"
        );
        assert_eq!(
            params[2],
            Value::Text(Uuid::from_u128(0x22).to_string()),
            "第 3 个是仓库（函数入参）"
        );
        assert_eq!(
            params[3],
            Value::Text("2026-11-16".to_string()),
            "第 4 个是 age_days 的 as_of"
        );

        // 不传物料 / 仓库 → 空串（NULLIF 后 = NULL = 不过滤），不是 Value::Null
        let params = cells_params(date("2026-11-16"), None, None);
        assert_eq!(params[1], Value::Text(String::new()));
        assert_eq!(params[2], Value::Text(String::new()));
        println!("[M2-09] 参数顺序 [as_of, 物料, 仓库, as_of]，空串走 NULLIF ✓");
    }
}

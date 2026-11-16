//! 呆滞料标记取数（M2-10）：一条聚合 SQL，把库龄层按 物料 × 仓库 分「全部 / 呆滞」两组。
//!
//! 卡：t_bb0fe3c6（M2-10【M2·仓库】库龄：呆滞料标记（超 N 天无出库））。
//!
//! # 算术在库层，本文件只分组
//!
//! ```text
//! biz_inventory_aging_layers(p_as_of, p_material_id, p_warehouse_id)   ← V0424（M2-08）唯一 FIFO 实现
//!   └─ 每笔 IN 一层：inbound_date / qty_remaining / amount_remaining / unit_cost
//!        └─ 本文件：age_days = p_as_of − inbound_date，is_slow = age_days > 阈值
//!             └─ GROUP BY 物料, 仓库：COUNT/SUM 全量 + COUNT/SUM FILTER (WHERE is_slow)
//! ```
//!
//! 本文件里**没有**窗口函数、没有累计、没有夹 0 —— 「结存由哪些层构成」是 M2-08 的事。
//! 报表（M2-09）与本卡（M2-10）都只是这个函数的两种 `GROUP BY`。
//!
//! # 阈值为什么只出现一次（`is_slow` 列）
//!
//! 呆滞列要算四个：`slow_layer_count` / `qty_slow` / `amount_slow`（以及它们的全量对照）。
//! 如果每列各写一遍 `FILTER (WHERE age_days > ?)`，阈值就会在 SQL 里出现四次，
//! 漏改一处的后果是「层数是呆滞的、金额不是」而没有任何地方报错。
//! 所以阈值**只比一次**（`flagged` CTE 里落成布尔列 `is_slow`），
//! 后面所有 `FILTER` 只引用列名 —— 占位符只有 5 个，参数顺序由单测逐个钉住：
//!
//! ```text
//! [as_of(函数), material_id, warehouse_id, as_of(算库龄), threshold_days]
//! ```
//!
//! 三个 CTE 的分工：`layers`（有结存的层）→ `aged`（算 `age_days`，**零布尔判断**）→
//! `flagged`（`is_slow = age_days > 阈值`，阈值在这里且只在这里出现）。
//!
//! # 只取还有结存的层
//!
//! `WHERE l.qty_remaining > 0`：被 FIFO 吃满的层是历史（剩 0），它们既不该进
//! `qty_on_hand`（加 0 无影响），也不该进 `slow_layer_count`（否则「呆滞 3 批」里
//! 有两批其实早就出完了）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_inventory_txn 无 dept_id / 归属人列（V0137），数据范围表达不出来；
//!               与 M2-08 的批次层分页、M2-09 的报表同锚点（inventory_aging::model::AGING_TARGET），
//!               可见性由权限码 business:erp:inventory-aging:slow-moving 控制。
//! ```
//!
//! 一次聚合查询 = 一个快照，所以**不用**开 REPEATABLE READ 事务（与 M2-09 同因）。

use nik::prelude::chrono::NaiveDate;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::inventory_aging::model::{AGING_FUNCTION, parse_decimal};
use crate::business::erp::inventory_aging::slow_moving::dto::CellRow;

/// 聚合 SQL：`WITH` 层 → 算 `age_days` + `is_slow` → 按 物料 × 仓库 聚合。
///
/// `?` 顺序 = `[as_of, material_id, warehouse_id, as_of, threshold_days]`
/// （见模块头注释；改动 SQL 必须同步 [`cells_params`]，单测会当场红）。
pub fn cells_sql() -> String {
    format!(
        "WITH layers AS ( \
             SELECT l.material_id, l.warehouse_id, l.inbound_date, l.qty_remaining, l.amount_remaining \
               FROM {AGING_FUNCTION}(?::text::date, NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid) l \
              WHERE l.qty_remaining > 0 \
         ), \
         aged AS ( \
             SELECT a.material_id, a.warehouse_id, a.inbound_date, a.qty_remaining, a.amount_remaining, \
                    (?::text::date - a.inbound_date) AS age_days \
               FROM layers a \
         ), \
         flagged AS ( \
             SELECT g.material_id, g.warehouse_id, g.inbound_date, g.qty_remaining, g.amount_remaining, \
                    g.age_days, (g.age_days > ?::text::int) AS is_slow \
               FROM aged g \
         ) \
         SELECT material_id, warehouse_id, \
                COUNT(*)::bigint AS layer_count, \
                SUM(qty_remaining)::text AS qty_on_hand, \
                SUM(amount_remaining)::text AS amount_on_hand, \
                MIN(inbound_date) AS oldest_inbound_date, \
                MAX(age_days)::bigint AS max_age_days, \
                COUNT(*) FILTER (WHERE is_slow)::bigint AS slow_layer_count, \
                COALESCE(SUM(qty_remaining) FILTER (WHERE is_slow), 0)::text AS qty_slow, \
                COALESCE(SUM(amount_remaining) FILTER (WHERE is_slow), 0)::text AS amount_slow \
           FROM flagged \
          GROUP BY material_id, warehouse_id"
    )
}

/// 聚合参数（顺序与 [`cells_sql`] 的占位符逐个对应）。
///
/// 空物料 / 仓库传空串 → `NULLIF(?::text,'')` → `NULL` = 不过滤
/// （驱动不接受 `Value::Null` 绑 `?::uuid`，全仓统一这条写法）。
pub fn cells_params(
    as_of: NaiveDate,
    material_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    threshold_days: i64,
) -> Vec<Value> {
    let as_of = Value::Text(as_of.to_string());
    vec![
        as_of.clone(),
        Value::Text(material_id.map(|id| id.to_string()).unwrap_or_default()),
        Value::Text(warehouse_id.map(|id| id.to_string()).unwrap_or_default()),
        as_of,
        Value::Text(threshold_days.to_string()),
    ]
}

/// 按 物料 × 仓库 聚合「全部层 / 呆滞层」两组数。
///
/// # 异常情况
///
/// 查库 / 解析失败 → [`DbError`]（handler 直接 `?`）；数值列一律 `::text` 出来再解析成
/// 定点数（驱动不做 `numeric → Decimal` 的直接映射，与 M2-08 的 `parse_decimal` 同一实现）。
pub async fn cells(
    as_of: NaiveDate,
    material_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    threshold_days: i64,
) -> DbResult<Vec<CellRow>> {
    let rows = db()
        .find_with_params(
            &cells_sql(),
            cells_params(as_of, material_id, warehouse_id, threshold_days),
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(CellRow {
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            layer_count: row.get_typed("layer_count")?,
            qty_on_hand: parse_decimal(row, "qty_on_hand")?,
            amount_on_hand: parse_decimal(row, "amount_on_hand")?,
            slow_layer_count: row.get_typed("slow_layer_count")?,
            qty_slow: parse_decimal(row, "qty_slow")?,
            amount_slow: parse_decimal(row, "amount_slow")?,
            oldest_inbound_date: row.get_typed("oldest_inbound_date")?,
            max_age_days: row.get_typed("max_age_days")?,
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

    /// SQL 必须 FROM 库函数，不许自己读流水 / 不许窗口函数（FIFO 在库函数里）。
    #[test]
    fn sql_reuses_the_aging_function_and_has_no_fifo() {
        let sql = cells_sql();
        assert!(
            sql.contains("FROM biz_inventory_aging_layers("),
            "呆滞料必须复用 M2-08 的库函数：{sql}"
        );
        assert!(!sql.contains("biz_inventory_txn"), "不许自己去读流水再摊层：{sql}");
        let upper = sql.to_uppercase();
        assert!(!upper.contains("OVER ("), "不许有窗口函数（FIFO 在库函数里）：{sql}");
        assert!(
            !upper.contains("ORDER BY"),
            "聚合 SQL 不需要 ORDER BY（行序在 Rust 排）：{sql}"
        );
        assert!(!upper.contains("LIMIT"), "不做分页（行上限在 Rust 截）：{sql}");
        assert!(sql.contains("WHERE l.qty_remaining > 0"), "只看还有结存的层：{sql}");
        println!("[M2-10] 聚合 SQL：复用库龄层函数、无窗口函数 / 无 ORDER BY / 只看有结存的层 ✓");
    }

    /// 阈值在 SQL 里**只出现一次**（落成 `is_slow` 列），呆滞列全部改用它。
    #[test]
    fn threshold_appears_once_and_slow_columns_use_the_flag() {
        let sql = cells_sql();
        assert_eq!(sql.matches("?::text::int").count(), 1, "阈值只能有一个占位符：{sql}");
        assert_eq!(sql.matches("is_slow").count(), 4, "1 处定义 + 3 处 FILTER：{sql}");
        assert!(
            sql.contains("(g.age_days > ?::text::int) AS is_slow"),
            "判定必须是「库龄严格大于阈值」：{sql}"
        );
        // age_days 必须穿过 flagged 这一层（最终 SELECT 要 MAX(age_days)；漏了会等到执行才炸）
        assert!(
            sql.contains("g.age_days, (g.age_days >"),
            "flagged 必须带出 age_days：{sql}"
        );
        assert!(sql.contains("MAX(age_days)"), "最终 SELECT 要用 age_days：{sql}");
        for column in ["slow_layer_count", "qty_slow", "amount_slow"] {
            assert!(sql.contains(column), "缺呆滞列 {column}：{sql}");
        }
        // 每次 FILTER 都引用同一个布尔列，不许再比一次阈值
        assert!(
            !sql.contains("FILTER (WHERE age_days > "),
            "FILTER 里不许再比阈值：{sql}"
        );
        println!("[M2-10] 阈值只比一次（is_slow 列），三个呆滞列共用 ✓");
    }

    /// 占位符个数与参数个数逐个对得上，顺序是 `[as_of, 物料, 仓库, as_of, 阈值]`。
    #[test]
    fn params_order_is_documented() {
        let sql = cells_sql();
        assert_eq!(sql.matches('?').count(), 5, "占位符 5 个：{sql}");
        let params = cells_params(
            date("2026-11-16"),
            Some(Uuid::from_u128(0x11)),
            Some(Uuid::from_u128(0x22)),
            30,
        );
        assert_eq!(params.len(), 5);
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
            "第 4 个是算库龄的 as_of"
        );
        assert_eq!(params[4], Value::Text("30".to_string()), "第 5 个是阈值");

        let params = cells_params(date("2026-11-16"), None, None, 90);
        assert_eq!(params[1], Value::Text(String::new()));
        assert_eq!(params[2], Value::Text(String::new()));
        println!("[M2-10] 参数顺序 [as_of, 物料, 仓库, as_of, 阈值]，空串走 NULLIF ✓");
    }
}

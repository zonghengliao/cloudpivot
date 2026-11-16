//! 补货建议取数（M2-15）：一条扫描 SQL 把一格的四个事实一次取齐。
//!
//! 卡：t_527ceaa3（M2-15【M2·仓库】补货建议：安全库存 + 在途 + 未清订单）。
//!
//! ```text
//! biz_inventory i                    结存（qty）—— 物料 × 仓库 一格一行
//!   JOIN biz_material m              停用 / 删除的物料不出现；带 material.safety_stock
//!   LEFT JOIN biz_unit u             单位名（展示用）
//!   LEFT JOIN biz_warehouse w        名字 / 类型（在途仓排除）
//!   LEFT JOIN biz_material_stock_policy p   策略行三列（min_qty / max_qty / status）
//!   LEFT JOIN LATERAL (…) t          发货在途：biz_inventory_in_transit where status='IN_TRANSIT'
//!   LEFT JOIN LATERAL (…) o          未清订单：biz_purchase_order(_item) 未收量
//! ```
//!
//! # 为什么用 `LATERAL` 而不是两个 `LEFT JOIN … GROUP BY`
//!
//! 在途与未清订单是**两张互不相关的多行表**，直接 join 再 `GROUP BY` 会做笛卡尔积，
//! 然后必须给每个 `SUM` 套 `DISTINCT` 才不会翻倍 —— 那种写法在「同一物料 2 条在途 +
//! 3 条未清订单行」时极容易算成 6 倍，而且没人会立刻发现（数字看起来还挺像）。
//! 两个 `LATERAL` 各自先聚合成一行再挂上来，一行一格的语义由 SQL 结构本身保证。
//!
//! # 三处「口径」只以列的形式出现，判定一律在 Rust
//!
//! 本文件**不**判「低于水位」、**不**算「建议量」：阈值三级回退的唯一实现在
//! `stock_policy::dto::resolve_min_qty`，判定在 `replenish::dto::suggest`。
//! 这里只负责把「策略行三列 + 物料级 safety_stock + 三个量」原样交给它们
//! （理由同 M2-14 的 `scan_cells`：SQL 里再写一份 `COALESCE(NULLIF(...))` 就没人对得上了）。
//!
//! # 排序与上限
//!
//! `ORDER BY` 只用来让**扫描**确定（结存小的先来，最可能缺料的先入结果），最终展示顺序
//! 由 `dto::sort_rows`（建议量降序）决定；`LIMIT` 是扫描上限 [`CELL_SCAN_LIMIT`]，
//! 撞上限时 handler 会置 `scan_truncated` 并提示收窄。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: biz_inventory 无 dept_id / 归属人列（V0137），数据范围表达不出来；
//!               与 M2-14 的水位扫描同锚点，可见性由读码 business:erp:replenish:list 控制。
//! ```

use std::str::FromStr;

use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::purchase::purchase_model::POSTED_RECEIVED_SQL;
use crate::business::erp::replenish::dto::CELL_SCAN_LIMIT;
use crate::business::erp::stock_policy::dto::{PolicyLevels, STATUS_DISABLED, STATUS_ENABLED};

/// 一格的四项原始事实 + 展示字段（判定前的形态；判定见 `replenish::dto::suggest`）。
#[derive(Debug, Clone)]
pub struct ReplenishCell {
    /// 这一格由几条 `biz_inventory` 成本桶汇总而来（按库位分桶时 > 1）。
    pub inventory_row_count: i64,
    /// 物料 id。
    pub material_id: Uuid,
    /// 物料编码（JOIN 条件已排除软删物料，这里理论上有值；保留 `Option` 以免主数据缺行时 panic）。
    pub material_code: Option<String>,
    /// 物料名称。
    pub material_name: Option<String>,
    /// 规格型号。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
    /// 仓库 id。
    pub warehouse_id: Uuid,
    /// 仓库编码。
    pub warehouse_code: Option<String>,
    /// 仓库名称。
    pub warehouse_name: Option<String>,
    /// 账面结存。
    pub on_hand_qty: Decimal,
    /// 发货在途。
    pub outbound_transit_qty: Decimal,
    /// 未清采购订单未收量。
    pub open_po_qty: Decimal,
    /// 物料级安全库存（策略行缺席时的回退值）。
    pub material_safety_stock: Decimal,
    /// 策略行三列（未命中 = `None`）。
    pub policy: Option<PolicyLevels>,
}

impl ReplenishCell {
    /// 便利：一格的身份（`(material_id, warehouse_id)`），用例里用来打断言。
    pub fn key(&self) -> (Uuid, Uuid) {
        (self.material_id, self.warehouse_id)
    }
}

/// 扫描 SQL（占位符顺序见 [`scan_params`]，单测逐个钉住）。
///
/// 四处容易写错的地方，注释里点名：
/// * **先按 物料 × 仓库 合计**（`FROM (SELECT … GROUP BY …) inv`）：`biz_inventory` 的唯一键是
///   `(material_id, warehouse_id, location_id)`，按库位分成本桶时一格会有好几行；直接读行会把
///   一格拆成多条建议（账面 35 变成「30 和 5」两格，各自判水位，建议量凭空变大）。
///   补货是按**仓**补的（`docs/ERP-摆设字段口径.md` §2「用库位账吗 → 不用」）。
/// * **已收数量是现算的，不是列**：`biz_purchase_order_item.received_qty` 由 V0217（P2-3）删除，
///   唯一真相是 [`POSTED_RECEIVED_SQL`]（已过账收货 − 已确认退货）。本文件**直接复用那个常量**，
///   不另写一份 —— 两份「已收」正是 V0217 收敛掉的那类双真相 bug。
/// * `w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT'`（**不是** `<>`：仓库软删时 `w.*` 全 NULL，
///   `<>` 会把整行静默丢掉 —— 与 M2-14 同一条坑）；
/// * 在途只取 `status = 'IN_TRANSIT'`（已签收 / 已作废不算在途，M2-03 的 downstream contract）；
///   未清订单只取 `SUBMITTED / APPROVED / PARTIAL_RECEIVED`（`DRAFT` 还没提交、不可收货）
///   且 `qty − 已收 > 0`（`RECEIVED / CLOSED / CANCELLED` 自然落在 0 上）。
///
/// 别名约定：外层库存格 = `inv`；未清订单 LATERAL 里的采购明细行 = `i`
/// （[`POSTED_RECEIVED_SQL`] 的表达式里写死了 `i.order_id` / `i.id`，所以必须叫 `i`）。
fn scan_sql(filters: &ScanFilters) -> String {
    let mut sql = format!(
        "SELECT inv.material_id AS material_id, inv.warehouse_id AS warehouse_id, \
                m.material_code AS material_code, m.material_name AS material_name, \
                m.spec AS spec, u.unit_name AS unit_name, \
                w.warehouse_code AS warehouse_code, w.warehouse_name AS warehouse_name, \
                inv.row_count AS inventory_row_count, \
                inv.qty::text AS on_hand_qty, \
                COALESCE(t.qty_in_transit, 0)::text AS outbound_transit_qty, \
                COALESCE(o.open_qty, 0)::text AS open_po_qty, \
                COALESCE(m.safety_stock, 0)::text AS material_safety_stock, \
                p.min_qty::text AS policy_min, p.max_qty::text AS policy_max, \
                p.status::text AS policy_status \
           FROM ( \
                SELECT material_id, warehouse_id, SUM(qty) AS qty, COUNT(*)::bigint AS row_count \
                  FROM biz_inventory \
                 GROUP BY material_id, warehouse_id \
           ) inv \
           JOIN biz_material m ON m.id = inv.material_id AND m.deleted_at IS NULL AND m.status = 1 \
           LEFT JOIN biz_unit u ON u.id = m.unit_id \
           LEFT JOIN biz_warehouse w ON w.id = inv.warehouse_id AND w.deleted_at IS NULL \
           LEFT JOIN biz_material_stock_policy p \
                  ON p.material_id = inv.material_id AND p.warehouse_id = inv.warehouse_id \
                 AND p.deleted_at IS NULL \
           LEFT JOIN LATERAL ( \
                SELECT SUM(x.qty_in_transit) AS qty_in_transit \
                  FROM biz_inventory_in_transit x \
                 WHERE x.material_id = inv.material_id \
                   AND x.warehouse_id = inv.warehouse_id \
                   AND x.status = 'IN_TRANSIT' \
           ) t ON TRUE \
           LEFT JOIN LATERAL ( \
                SELECT SUM(line.open_qty) AS open_qty \
                  FROM ( \
                       SELECT (i.qty - {POSTED_RECEIVED_SQL}) AS open_qty \
                         FROM biz_purchase_order_item i \
                         JOIN biz_purchase_order po ON po.id = i.order_id \
                        WHERE i.material_id = inv.material_id \
                          AND po.warehouse_id = inv.warehouse_id \
                          AND po.deleted_at IS NULL \
                          AND po.status IN ('SUBMITTED', 'APPROVED', 'PARTIAL_RECEIVED') \
                  ) line \
                 WHERE line.open_qty > 0 \
           ) o ON TRUE \
          WHERE w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT'"
    );
    if filters.material_id.is_some() {
        sql.push_str(" AND inv.material_id = ?::uuid");
    }
    if filters.warehouse_id.is_some() {
        sql.push_str(" AND inv.warehouse_id = ?::uuid");
    }
    if filters.keyword.is_some() {
        sql.push_str(
            " AND (m.material_code LIKE ? OR m.material_name LIKE ? \
               OR w.warehouse_code LIKE ? OR w.warehouse_name LIKE ?)",
        );
    }
    // 扫描序：结存小的先来（最可能缺料的一格先进结果集，撞上限时丢的是「最不缺」的格）。
    // 三列合起来全序（一格 = 物料 × 仓库）：翻轮不重复、不丢行。
    sql.push_str(" ORDER BY inv.qty ASC, m.material_code ASC, inv.warehouse_id ASC LIMIT ?");
    sql
}

/// 扫描筛选（handler 已把入参解析成 UUID / 关键字模式）。
#[derive(Debug, Clone, Default)]
pub struct ScanFilters {
    /// 物料（精确）。
    pub material_id: Option<Uuid>,
    /// 仓库（精确）。
    pub warehouse_id: Option<Uuid>,
    /// 关键字 `LIKE` 模式（`utils::like_pattern` 已包 `%`）；`None` = 不筛。
    pub keyword: Option<String>,
}

/// 扫描参数（顺序与 [`scan_sql`] 的占位符逐个对应）。
fn scan_params(filters: &ScanFilters) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = filters.material_id {
        params.push(Value::from(&id));
    }
    if let Some(id) = filters.warehouse_id {
        params.push(Value::from(&id));
    }
    if let Some(pattern) = filters.keyword.as_ref() {
        // 四个 LIKE 共用同一个模式（物料编码 / 名称 + 仓库编码 / 名称）
        for _ in 0..4 {
            params.push(Value::Text(pattern.clone()));
        }
    }
    params.push(Value::I64(CELL_SCAN_LIMIT));
    params
}

/// 扫描一格行（`物料 × 仓库`），不做任何判定。
///
/// # 异常情况
///
/// 查库失败 / 数值列解析失败 → [`DbError`]（handler 直接 `?`，出口是 `ResResult`）。
/// `NUMERIC` 一律 `::text` 出来再解析成定点数（驱动不做 `numeric → Decimal` 直接映射）。
pub async fn scan_cells(filters: &ScanFilters) -> DbResult<Vec<ReplenishCell>> {
    let rows = db().find_with_params(&scan_sql(filters), scan_params(filters)).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        out.push(ReplenishCell {
            inventory_row_count: row.get_typed("inventory_row_count")?,
            material_id: row.get_typed("material_id")?,
            material_code: row.get_typed("material_code")?,
            material_name: row.get_typed("material_name")?,
            spec: row.get_typed("spec")?,
            unit_name: row.get_typed("unit_name")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            warehouse_code: row.get_typed("warehouse_code")?,
            warehouse_name: row.get_typed("warehouse_name")?,
            on_hand_qty: decimal_of(row, "on_hand_qty")?,
            outbound_transit_qty: decimal_of(row, "outbound_transit_qty")?,
            open_po_qty: decimal_of(row, "open_po_qty")?,
            material_safety_stock: decimal_of(row, "material_safety_stock")?,
            policy: policy_of(
                row.get_typed::<Option<String>>("policy_min")?,
                row.get_typed::<Option<String>>("policy_max")?,
                row.get_typed::<Option<String>>("policy_status")?,
            )?,
        });
    }
    Ok(out)
}

/// 策略行三列 → [`PolicyLevels`]（`LEFT JOIN` 未命中时三列都是 NULL → `None`）。
fn policy_of(min: Option<String>, max: Option<String>, status: Option<String>) -> DbResult<Option<PolicyLevels>> {
    let (Some(min), Some(status)) = (min, status) else {
        return Ok(None);
    };
    let min_qty = parse_decimal(min, "policy.min_qty")?;
    let max_qty = match max.filter(|text| !text.trim().is_empty()) {
        Some(text) => Some(parse_decimal(text, "policy.max_qty")?),
        None => None,
    };
    let status: i16 = status
        .trim()
        .parse()
        .map_err(|error| DbError::InvalidValue(format!("policy.status 不是合法 smallint：{error}")))?;
    // 域外的值按停用处理（库层 CHECK 已挡住，这里只是不给下游留 panic 的机会）
    let status = if status == STATUS_ENABLED {
        STATUS_ENABLED
    } else {
        STATUS_DISABLED
    };
    Ok(Some(PolicyLevels::new(min_qty, max_qty, status)))
}

/// 取一列 `NUMERIC`（`::text`) 并解析成定点数。
fn decimal_of(row: &Row, column: &str) -> DbResult<Decimal> {
    let text = row.get_typed::<String>(column)?;
    parse_decimal(text, column)
}

/// `NUMERIC::text` → `Decimal`（错误文案点名是哪一列，排障时不用猜）。
fn parse_decimal(raw: String, field: &str) -> DbResult<Decimal> {
    let text = raw.trim();
    Decimal::from_str(text)
        .map_err(|error| DbError::InvalidValue(format!("补货建议 {field} 不是合法定点数（{text}）：{error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SQL 必须读四张表、也必须只读四张表（多读一张 = 口径多一个来源）。
    #[test]
    fn sql_reads_exactly_the_four_sources() {
        let sql = scan_sql(&ScanFilters::default());
        for table in [
            "biz_inventory ",
            "biz_material ",
            "biz_material_stock_policy ",
            "biz_inventory_in_transit ",
            "biz_purchase_order ",
            "biz_purchase_order_item ",
        ] {
            assert!(sql.contains(table), "SQL 必须读 {table}：{sql}");
        }
        assert!(
            !sql.contains("biz_stock_transfer"),
            "调拨不在口径里（过账即两边记账）：{sql}"
        );
        assert!(!sql.contains("biz_sales_order"), "销售订单不在本卡口径里：{sql}");
        println!("[M2-15] 扫描 SQL：结存 / 水位 / 在途 / 未清订单四源，不外扩 ✓");
    }

    /// 三处关键口径必须在 SQL 文本里看得见（否则改一行就静默改语义）。
    #[test]
    fn sql_pins_the_three_semantics() {
        let sql = scan_sql(&ScanFilters::default());
        assert!(
            sql.contains("AND x.status = 'IN_TRANSIT'"),
            "在途只取 IN_TRANSIT：{sql}"
        );
        assert!(
            sql.contains("po.status IN ('SUBMITTED', 'APPROVED', 'PARTIAL_RECEIVED')"),
            "未清订单状态域：{sql}"
        );
        assert!(sql.contains("WHERE line.open_qty > 0"), "只看还没收完的行：{sql}");
        assert!(sql.contains("po.deleted_at IS NULL"), "软删的采购单不算在途：{sql}");
        assert!(
            sql.contains("w.warehouse_type IS DISTINCT FROM 'IN_TRANSIT'"),
            "在途仓排除必须用 IS DISTINCT FROM：{sql}"
        );
        assert!(
            sql.contains("m.deleted_at IS NULL AND m.status = 1"),
            "停用 / 删除的物料不出现：{sql}"
        );
        assert!(!sql.to_uppercase().contains(" GREATEST("), "负结存不许抹平：{sql}");
        println!("[M2-15] SQL 钉住在途状态 / 未清订单状态 / 在途仓 / 停用物料 / 不夹 0 ✓");
    }

    /// 已收数量必须复用 P2-3 的唯一真相（`POSTED_RECEIVED_SQL`），**不许**自己算一份、也不许再读已删列。
    ///
    /// 这是 V0217 收敛掉的那类双真相 bug 的回归防线：`biz_purchase_order_item.received_qty`
    /// 已被迁移删除，任何「自己 SUM 收货单」或「假设有个列」的写法都会在真库上直接炸。
    #[test]
    fn posted_received_qty_is_reused_not_reimplemented() {
        let sql = scan_sql(&ScanFilters::default());
        assert!(
            sql.contains("SELECT SUM(si.qty) FROM biz_stock_in s"),
            "必须内联 POSTED_RECEIVED_SQL（已过账收货）：{sql}"
        );
        assert!(
            sql.contains("biz_purchase_return_item"),
            "必须带上已确认退货的冲减（同一常量的另一半）：{sql}"
        );
        assert!(
            sql.contains("s.status = 'POSTED'") && sql.contains("pr.status = 'CONFIRMED'"),
            "只算已过账收货 / 已确认退货：{sql}"
        );
        assert!(
            !sql.contains("received_qty"),
            "received_qty 列 V0217 已删，出现即为死代码：{sql}"
        );
        println!("[M2-15] 已收数量复用 purchase_model::POSTED_RECEIVED_SQL（不自己算、不读已删列）✓");
    }

    /// `LATERAL` 各聚合成一行：不会因两张多行表 join 出笛卡尔积；库存侧也先按 物料×仓库 合计。
    #[test]
    fn lateral_subqueries_and_stock_bucket_are_aggregated() {
        let sql = scan_sql(&ScanFilters::default());
        assert_eq!(sql.matches("LEFT JOIN LATERAL").count(), 2, "两条 LATERAL：{sql}");
        assert!(
            sql.matches("SELECT SUM(").count() >= 3,
            "在途 / 未清订单 / 已收量都要 SUM：{sql}"
        );
        assert!(
            sql.contains("SUM(qty) AS qty, COUNT(*)::bigint AS row_count"),
            "库存行必须先按 物料 × 仓库 合计（否则一格拆成多条建议）：{sql}"
        );
        assert!(
            sql.contains("GROUP BY material_id, warehouse_id"),
            "库存桶的合计键必须是 物料 × 仓库：{sql}"
        );
        assert_eq!(
            sql.matches("GROUP BY").count(),
            1,
            "只有库存子查询该分组（外层一格一行，不许再 GROUP BY）：{sql}"
        );
        println!("[M2-15] 在途 / 未清订单各走 LATERAL 预聚合、库存按 物料×仓库 合计，无笛卡尔积 ✓");
    }

    /// 占位符个数与参数个数逐个对得上（改 SQL 忘了同步参数，这里当场红）。
    #[test]
    fn params_match_placeholders() {
        let empty = ScanFilters::default();
        assert_eq!(
            scan_sql(&empty).matches('?').count(),
            1,
            "只筛仓库类型时 1 个 LIMIT 占位符"
        );
        assert_eq!(scan_params(&empty).len(), 1);

        let mat = ScanFilters {
            material_id: Some(Uuid::from_u128(0x11)),
            ..Default::default()
        };
        assert_eq!(scan_sql(&mat).matches('?').count(), 2);
        assert_eq!(scan_params(&mat).len(), 2);

        let wh = ScanFilters {
            warehouse_id: Some(Uuid::from_u128(0x22)),
            ..Default::default()
        };
        assert_eq!(scan_sql(&wh).matches('?').count(), 2);
        assert_eq!(scan_params(&wh).len(), 2);

        let keyword = ScanFilters {
            keyword: Some("%甲%".to_string()),
            ..Default::default()
        };
        assert_eq!(scan_sql(&keyword).matches('?').count(), 5, "4 个 LIKE + 1 个 LIMIT");
        assert_eq!(scan_params(&keyword).len(), 5);

        let all = ScanFilters {
            material_id: Some(Uuid::from_u128(0x11)),
            warehouse_id: Some(Uuid::from_u128(0x22)),
            keyword: Some("%甲%".to_string()),
        };
        assert_eq!(scan_sql(&all).matches('?').count(), 7);
        assert_eq!(scan_params(&all).len(), 7);
        // 前两个是 UUID，其后四个是同值 LIKE，最后是上限
        let params = scan_params(&all);
        assert_eq!(params[0], Value::from(&Uuid::from_u128(0x11)));
        assert_eq!(params[1], Value::from(&Uuid::from_u128(0x22)));
        for param in &params[2..6] {
            assert_eq!(param, &Value::Text("%甲%".to_string()));
        }
        assert_eq!(params[6], Value::I64(CELL_SCAN_LIMIT));
        println!("[M2-15] 占位符 / 参数个数与顺序逐个对齐（含关键字 4 LIKE）✓");
    }

    /// 策略行三列 → `PolicyLevels`：未命中 `None`、停用保留数值但 `enabled=false`、
    /// `max_qty` 空串按未设（`NULL::text` 与 `''` 两种形态都当「没设」）。
    #[test]
    fn policy_levels_are_built_from_three_columns() {
        let none = policy_of(None, None, None).expect("未命中不报错");
        assert!(none.is_none());

        let enabled = policy_of(Some("50".into()), Some("100".into()), Some("1".into())).expect("解析");
        let enabled = enabled.expect("命中");
        assert_eq!(enabled.min_qty.to_string(), "50");
        assert_eq!(enabled.max_qty.map(|qty| qty.to_string()), Some("100".to_string()));
        assert!(enabled.enabled);

        let disabled = policy_of(Some("50".into()), Some("".into()), Some("0".into())).expect("解析");
        let disabled = disabled.expect("命中");
        assert!(!disabled.enabled, "status = 0 → 停用（下游回退物料级）");
        assert_eq!(disabled.max_qty, None, "空串 = 未设最高水位");

        let bad = policy_of(Some("abc".into()), None, Some("1".into()));
        assert!(bad.is_err(), "非法数值必须报错，不许静默当 0");
        println!("[M2-15] 策略行三列 → PolicyLevels（未命中 / 启用 / 停用 / 空 max / 坏值）✓");
    }
}

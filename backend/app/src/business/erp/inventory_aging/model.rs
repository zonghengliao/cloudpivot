//! 库龄层取数（M2-08）：调 `biz_inventory_aging_layers(...)` 函数分页 + 补主数据名字。
//!
//! 卡：t_397dc77c（M2-08【M2·仓库】库龄：计算（按批次入库时间））。
//!
//! # 算术在库层，本文件只搬运
//!
//! FIFO 分层是**集合运算**（按 `物料 × 仓库` 分组、按入库日期累计、夹 0），写在
//! `V0424__inventory_aging_calc.up.sql` 的 `biz_inventory_aging_layers(p_as_of, p_material_id,
//! p_warehouse_id)` 里 —— 一份实现，三张卡共用：
//!
//! | 谁 | 怎么用 |
//! |:--|:--|
//! | 本卡（M2-08 计算） | `SELECT … FROM biz_inventory_aging_layers(…) l WHERE …` 分页出批次层 |
//! | M2-09（库龄分析报表） | 同一个函数 `GROUP BY 物料, 仓库` 出 0-30/31-60/61-90/90+ |
//! | M2-10（呆滞料标记） | 同一个函数按 `年龄 ≥ N 天` 标记 |
//!
//! 所以这里**没有**窗口聚合、也没有任何 FIFO 代码：一旦两处各写一遍
//! 「累计 + 夹 0」，漂移只会在对不上账时才被发现。纯 Rust 那份只用于**互证**
//! （[`super::fifo`]），生产路径不跑它。
//!
//! # 为什么分页走 `page_of` 而不是自己拼 LIMIT
//!
//! `base_sql` 是「`FROM 函数` + `WHERE`」，没有 `ORDER BY` / `LIMIT`（由
//! `scope::paginate_unscoped` 拼），`total` 是筛选后的真实总量 —— 与全仓 40+ 个列表同款。
//! `biz_inventory_txn` 不参与数据权限（V0137 起就没有部门 / 归属人列，§2.5 判为 ❌），
//! 所以锚点是 [`AGING_TARGET`]（`ScopeTarget::unscoped`）。
//!
//! # 名字怎么补（AGENTS.md 约定 13）
//!
//! 物料 / 仓库是**主数据关联**（要显示「现在叫什么」）⇒ 批量 `LEFT JOIN` 主数据，
//! 一页固定 2 条 SQL（[`material_briefs`] / [`warehouse_briefs`]），**不逐行查**。
//! 软删的主数据按「缺字段」处理（`null`），老层照常可读。
//!
//! # 已知口径（明写，不假装）
//!
//! * 只有**有入库流水**的库存才有库龄层：直接写进 `biz_inventory` 而没有流水的库存在这里
//!   看不见。库存期初走「库存期初录入」（它会写 `OPENING` 流水），所以合规路径不会漏。
//! * 出库量大于入库量（脏数据 / 允许负库存的仓）时，这一格的所有层剩余都是 0，
//!   `Σ剩余 < biz_inventory.qty` 就是「这格账不干净」的信号；本模块**不编造负数层**。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::page_of;
use cloudpivot_kernel::business::shared::scope;

/// 库龄的函数名（迁移 V0424 建；报表 / 呆滞料卡也读它）。
pub const AGING_FUNCTION: &str = "biz_inventory_aging_layers";

/// 分页锚点：`biz_inventory_txn` 不参与数据权限（见模块头注释）。
pub const AGING_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped("biz_inventory_txn");

/// 全序：**最老在前**（库龄分析先看老货），同一天按过账时刻、再用层 id 收全序
/// （没有全序时翻页会漏行 / 重复行）。
pub const LAYER_ORDER: &str = "ORDER BY l.inbound_date ASC, l.posted_at ASC, l.layer_id ASC";

// ==================== 行结构 ====================

/// 一层入库（= 一个批次层）在基准日的快照。
///
/// 数量四列**原样来自函数**（`qty_in` / `qty_consumed` / `qty_remaining` /
/// `amount_remaining`），Rust 一行算术都不做 —— 与出参的「页面/接口不算账」同款。
#[derive(Debug, Clone)]
pub struct LayerRow {
    /// 层 id（= 那笔 `IN` 流水的 id）。
    pub layer_id: Uuid,
    /// 物料。
    pub material_id: Uuid,
    /// 仓库。
    pub warehouse_id: Uuid,
    /// 批号（入库单明细的 `lot_no`；非入库单来源为 `None`）。
    pub lot_no: Option<String>,
    /// 来源单据类型。
    pub doc_type: String,
    /// 来源单号。
    pub doc_no: String,
    /// 来源单据行号。
    pub line_no: i32,
    /// 业务类型。
    pub biz_type: String,
    /// 入库日期（业务日期）。
    pub inbound_date: NaiveDate,
    /// 过账时间。
    pub posted_at: DateTime<Utc>,
    /// 本层入库数量。
    pub qty_in: Decimal,
    /// 已被 FIFO 吃掉的数量。
    pub qty_consumed: Decimal,
    /// 本层剩余数量。
    pub qty_remaining: Decimal,
    /// 本层入库单价。
    pub unit_cost: Decimal,
    /// 参考金额 = 剩余 × 层单价。
    pub amount_remaining: Decimal,
}

/// 取数条件（handler 已把字符串参数解析成强类型）。
#[derive(Debug, Clone)]
pub struct LayerQuery {
    /// 物料（推进入函数，只算这一格）。
    pub material_id: Option<Uuid>,
    /// 仓库（推进入函数）。
    pub warehouse_id: Option<Uuid>,
    /// 批号（**在函数结果上**筛，见模块头注释：不能提前到窗口之前）。
    pub lot_no: Option<String>,
    /// 基准日。
    pub as_of: NaiveDate,
    /// 入库日期下界（由 `max_age_days` 折算，含）。
    pub inbound_from: Option<NaiveDate>,
    /// 入库日期上界（由 `min_age_days` 折算，含）。
    pub inbound_to: Option<NaiveDate>,
    /// 是否连「已吃满（剩余 0）」的层一起返回。
    pub include_consumed: bool,
}

/// 主数据（物料）的展示字段（批量补齐，防 N+1）。
#[derive(Debug, Clone)]
pub struct MaterialBrief {
    /// 物料 id。
    pub id: Uuid,
    /// 编码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 规格。
    pub spec: Option<String>,
    /// 计量单位名。
    pub unit_name: Option<String>,
}

// ==================== 取数 ====================

/// 组 `base_sql`：`FROM 函数` + 外层 `WHERE`（无 `ORDER BY` / `LIMIT`）。
///
/// `?` 的顺序 = 函数三个入参（基准日 / 物料 / 仓库）+ 外层 WHERE 的筛选项，
/// 与 [`layer_params`] 一一对应（单测 `where_params_match_placeholders` 盯着这件事）。
fn layer_sql(query: &LayerQuery) -> (String, Vec<Value>) {
    let mut sql = String::from(
        "SELECT l.layer_id, l.material_id, l.warehouse_id, l.lot_no, l.doc_type, l.doc_no, l.line_no, \
                l.biz_type, l.inbound_date, l.posted_at, \
                l.qty_in::text AS qty_in, l.qty_consumed::text AS qty_consumed, \
                l.qty_remaining::text AS qty_remaining, l.unit_cost::text AS unit_cost, \
                l.amount_remaining::text AS amount_remaining \
           FROM biz_inventory_aging_layers(?::text::date, NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid) l \
          WHERE 1 = 1",
    );
    let (extra, params) = layer_params(query);
    sql.push_str(&extra);
    (sql, params)
}

/// 外层 `WHERE` 片段 + 参数（顺序：基准日、物料、仓库、批号、日期下界、日期上界）。
fn layer_params(query: &LayerQuery) -> (String, Vec<Value>) {
    let mut sql = String::new();
    let mut params: Vec<Value> = vec![
        Value::Text(query.as_of.to_string()),
        // 空串 → NULLIF → NULL：不传物料 / 仓库 = 不过滤（`Value::Null` 绑 `?::uuid` 会报
        // serializing parameter，全仓统一改用 `NULLIF(?::text,'')` 这条写法）。
        Value::Text(query.material_id.map(|id| id.to_string()).unwrap_or_default()),
        Value::Text(query.warehouse_id.map(|id| id.to_string()).unwrap_or_default()),
    ];
    if !query.include_consumed {
        // 默认只看还有结存的层：吃满的层是历史，混进来就是噪音
        sql.push_str(" AND l.qty_remaining > 0");
    }
    if let Some(lot_no) = &query.lot_no {
        sql.push_str(" AND l.lot_no = ?");
        params.push(Value::Text(lot_no.clone()));
    }
    if let Some(from) = query.inbound_from {
        sql.push_str(" AND l.inbound_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = query.inbound_to {
        sql.push_str(" AND l.inbound_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    (sql, params)
}

/// 批次层分页。返回 `(rows, total)`，`total` 是筛选后的真实总量。
///
/// # 异常情况
///
/// 查库 / 行解析失败 → [`DbError`]（handler 直接 `?`）。SQL 里没有 `ORDER BY` / `LIMIT`
/// （由 `scope::paginate_unscoped` 自己拼），违者会被 `page_of` 的守卫报错。
pub async fn layer_page(query: &LayerQuery, page: u64, page_size: u64) -> DbResult<(Vec<LayerRow>, i64)> {
    let (sql, params) = layer_sql(query);
    page_of(
        AGING_TARGET,
        sql,
        params,
        LAYER_ORDER,
        page,
        page_size,
        LayerRow::from_row,
    )
    .await
}

impl LayerRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            layer_id: row.get_typed("layer_id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            lot_no: row.get_typed("lot_no")?,
            doc_type: row.get_typed("doc_type")?,
            doc_no: row.get_typed("doc_no")?,
            line_no: row.get_typed("line_no")?,
            biz_type: row.get_typed("biz_type")?,
            inbound_date: row.get_typed("inbound_date")?,
            posted_at: row.get_typed("posted_at")?,
            qty_in: parse_decimal(row, "qty_in")?,
            qty_consumed: parse_decimal(row, "qty_consumed")?,
            qty_remaining: parse_decimal(row, "qty_remaining")?,
            unit_cost: parse_decimal(row, "unit_cost")?,
            amount_remaining: parse_decimal(row, "amount_remaining")?,
        })
    }
}

/// `numeric` 一律 `::text` 出来再解析成定点数（驱动不做 `numeric → Decimal` 的直接映射，
/// 与 `inventory_transit::model::parse_decimal` 同款；顺带避开浮点丢精度）。
fn parse_decimal(row: &Row, column: &str) -> DbResult<Decimal> {
    let text = row.get_typed::<String>(column)?;
    text.trim()
        .parse::<Decimal>()
        .map_err(|error| DbError::InvalidValue(format!("库龄层 {column} 不是合法数值：{text}（{error}）")))
}

// ==================== 主数据批量补名 ====================

/// 一页的物料展示字段（一条 SQL，不是每行一次）。
pub async fn material_briefs(ids: &[Uuid]) -> DbResult<Vec<MaterialBrief>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT m.id, m.material_code, m.material_name, m.spec, u.unit_name \
               FROM biz_material m \
               LEFT JOIN biz_unit u ON u.id = m.unit_id \
              WHERE m.id = ANY(?::text::uuid[]) AND m.deleted_at IS NULL",
            vec![ids_array_param(ids)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(MaterialBrief {
                id: row.get_typed("id")?,
                code: row.get_typed("material_code")?,
                name: row.get_typed("material_name")?,
                spec: row.get_typed("spec")?,
                unit_name: row.get_typed("unit_name")?,
            })
        })
        .collect()
}

/// 一页的仓库展示字段（一条 SQL）。
pub async fn warehouse_briefs(ids: &[Uuid]) -> DbResult<Vec<(Uuid, String, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = db()
        .find_with_params(
            "SELECT w.id, w.warehouse_code, w.warehouse_name \
               FROM biz_warehouse w \
              WHERE w.id = ANY(?::text::uuid[]) AND w.deleted_at IS NULL",
            vec![ids_array_param(ids)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.get_typed("id")?,
                row.get_typed("warehouse_code")?,
                row.get_typed("warehouse_name")?,
            ))
        })
        .collect()
}

/// UUID 数组参数（PG 的 `{a,b}` 文本字面量 + `?::text::uuid[]`；驱动不接受 `Vec<Uuid>`）。
fn ids_array_param(ids: &[Uuid]) -> Value {
    let mut literal = String::with_capacity(ids.len() * 37 + 2);
    literal.push('{');
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            literal.push(',');
        }
        literal.push_str(&id.to_string());
    }
    literal.push('}');
    Value::Text(literal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期字面量")
    }

    fn base_query() -> LayerQuery {
        LayerQuery {
            material_id: None,
            warehouse_id: None,
            lot_no: None,
            as_of: date("2026-11-16"),
            inbound_from: None,
            inbound_to: None,
            include_consumed: false,
        }
    }

    /// 默认口径：只看还有结存的层（`qty_remaining > 0`），函数三个入参恒在。
    #[test]
    fn base_sql_always_filters_remaining_and_passes_as_of() {
        let (sql, params) = layer_sql(&base_query());
        assert!(sql.contains("FROM biz_inventory_aging_layers(?::text::date"), "{sql}");
        assert!(sql.contains("NULLIF(?::text, '')::uuid"), "{sql}");
        assert!(sql.contains("l.qty_remaining > 0"), "{sql}");
        assert!(
            !sql.to_uppercase().contains("ORDER BY"),
            "base_sql 不许带 ORDER BY：{sql}"
        );
        assert!(!sql.to_uppercase().contains("LIMIT"), "base_sql 不许带 LIMIT：{sql}");
        assert_eq!(params.len(), 3, "函数三入参 = 3 个占位");
        assert_eq!(sql.matches('?').count(), 3);
        println!("[M2-08] base_sql：函数三入参 + 只看有结存的层，无 ORDER BY/LIMIT ✓");
    }

    /// `?` 个数必须与参数个数**逐个**对得上（少一个 = 运行期 binding 错位）。
    #[test]
    fn where_params_match_placeholders() {
        let cases = [
            (base_query(), 3),
            (
                LayerQuery {
                    include_consumed: true,
                    ..base_query()
                },
                3,
            ),
            (
                LayerQuery {
                    material_id: Some(Uuid::from_u128(0x11)),
                    warehouse_id: Some(Uuid::from_u128(0x22)),
                    lot_no: Some("LOT-01".to_string()),
                    inbound_from: Some(date("2026-09-01")),
                    inbound_to: Some(date("2026-11-16")),
                    include_consumed: true,
                    ..base_query()
                },
                6,
            ),
        ];
        for (query, want) in cases {
            let (sql, params) = layer_sql(&query);
            assert_eq!(sql.matches('?').count(), want, "占位符数不对：{sql}");
            assert_eq!(params.len(), want, "参数数不对：{sql}");
        }
        println!("[M2-08] `?` 与参数个数逐组一致（3 / 3 / 6）✓");
    }

    /// 物料 / 仓库是**推进函数**的参数（不是外层 WHERE）：推不进去就会全表算一遍。
    #[test]
    fn partition_filters_are_pushed_into_the_function() {
        let query = LayerQuery {
            material_id: Some(Uuid::from_u128(0x11)),
            warehouse_id: Some(Uuid::from_u128(0x22)),
            ..base_query()
        };
        let (sql, params) = layer_sql(&query);
        let call = sql.split("WHERE").next().unwrap_or_default();
        assert!(call.contains("NULLIF(?::text, '')::uuid"), "{sql}");
        // 外层 WHERE 里不许再出现物料 / 仓库条件（重复过滤会让「推进去」白做）
        let outer = sql.split("WHERE 1 = 1").nth(1).unwrap_or_default();
        assert!(!outer.contains("material_id"), "物料条件应在函数入参里：{outer}");
        assert!(!outer.contains("warehouse_id"), "仓库条件应在函数入参里：{outer}");
        assert_eq!(
            params[1],
            Value::Text(Uuid::from_u128(0x11).to_string()),
            "第 2 个参数必须是物料 id"
        );
        println!("[M2-08] 物料/仓库推进函数入参，外层不重复过滤 ✓");
    }

    /// 全序必须**最老在前**（同一天用层 id 收全序，翻页不漏不重）。
    #[test]
    fn order_is_oldest_first_with_stable_tiebreak() {
        assert!(LAYER_ORDER.contains("l.inbound_date ASC"));
        assert!(LAYER_ORDER.contains("l.layer_id ASC"));
        assert_eq!(AGING_TARGET.table, "biz_inventory_txn");
        assert_eq!(AGING_FUNCTION, "biz_inventory_aging_layers");
        println!("[M2-08] 全序：入库日期升序 → 过账时刻 → 层 id ✓");
    }

    /// UUID 数组参数是 PG 的 `{a,b}` 文本字面量（不是 `[a,b]`）。
    #[test]
    fn ids_array_param_uses_pg_literal() {
        let literal = ids_array_param(&[Uuid::from_u128(1), Uuid::from_u128(2)]);
        match literal {
            Value::Text(text) => {
                assert!(text.starts_with('{') && text.ends_with('}'), "{text}");
                assert_eq!(text.matches(',').count(), 1, "{text}");
            }
            other => panic!("必须是文本数组字面量：{other:?}"),
        }
        assert!(matches!(ids_array_param(&[]), Value::Text(text) if text == "{}"));
        println!("[M2-08] 批量补名参数是 PG 文本数组 `{{…}}` ✓");
    }
}

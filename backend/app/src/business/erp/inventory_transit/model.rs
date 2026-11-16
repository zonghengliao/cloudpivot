//! 在途库存查询的取数层（M2-04）。
//!
//! 表：`biz_inventory_in_transit`（V0420，M2-01）。读入口只有本文件，
//! **一行写 SQL 都没有** —— 写入是 M2-02（`shipment_transit::record_shipped`）与
//! M2-03（`shipment_transit::receive_open`）的地盘。
//!
//! ```text
//! SCOPE-EXEMPT: biz_inventory_in_transit 是发货单的派生账（V0420 无 dept_id / 归属人列），
//!               数据范围（SELF / DEPT_*）表达不出来；可见性由权限码
//!               business:erp:inventory-transit:list 控制。分页走 page_of →
//!               scope::paginate_unscoped（与 erp/model_base.rs 的 13 个 *_page 同款）。
//! ```
//!
//! # 三条口径（都来自上游契约，不是本卡的发挥）
//!
//! 1. **在途量取库里的生成列** `qty_in_transit = shipped_qty - received_qty`
//!    （M2-01 迁移注释：任何写入路径都写不出「不符」的行）。本文件不自己减。
//! 2. **在途取数一律带状态过滤**（M2-03 的 downstream_contract：`status <> 'CANCELLED'`）。
//!    明细分页恒有 `t.status = ?`，默认 `IN_TRANSIT`，`CANCELLED` 只能被**显式点名**；
//!    汇总恒为 `status = 'IN_TRANSIT'`（在途 = 还没签收的量）。
//! 3. **客户是经发货单头关联的**（在途行本身没有客户列）：`LEFT JOIN biz_shipment s
//!    ON s.id = t.shipment_id AND s.deleted_at IS NULL`。用 LEFT JOIN 而不是 INNER：
//!    发货单头被软删时，在途行仍然要能看到（它是发货过账的派生账，不该因为单头软删
//!    就从报表里消失 —— 那会让「发出去的货」凭空不见），只是 `ship_no` / `customer_id`
//!    为 `null`。
//!
//! # 为什么不按「物料 × 仓库」自己聚合出一份在途表
//!
//! 因为那是派生值：`qty_in_transit` 已经由库层保证恒等，再落一份汇总表就多出一条
//! 会漂移的口径（M2-01 迁移注释「三本账为什么不收在途」的同一个理由）。汇总接口
//! 就是一条 `GROUP BY`，读时算。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::model_base::page_of;
use crate::business::erp::shipment::shipment_transit::{self, TABLE};
use cloudpivot_kernel::business::shared::scope;

/// 汇总一次最多返回的 `物料 × 仓库` 组数（超过则 `truncated = true`）。
///
/// 为什么要有上限：汇总不受分页保护（它不是行级列表，是个「合计」），
/// 一个仓位几万条 物料×仓库 组合时整份吐出去会把接口和浏览器一起拖死。
/// 500 组已经覆盖正常人的「一眼看全部在途」；再多就该按物料 / 仓库收窄，
/// 而不是靠调大上限（上游 M2-15 补货建议要的是按物料查，不是全量铺开）。
pub const SUMMARY_ROW_LIMIT: usize = 500;

/// 明细行 SQL 的列清单（顺序与 [`TransitRow::from_row`] 的取列名一一对应）。
///
/// `ship_no` / `customer_id` 来自发货单头（软删则为 `NULL`）；
/// `age_days` 由 PG 用 `CURRENT_DATE - ship_date` 算 —— 在 Rust 里取「今天」
/// 会跟着进程时区漂，跨零点还会与库里的日期差一天。
const ROW_COLUMNS: &str = "t.id, t.shipment_id, t.material_id, t.warehouse_id, t.ship_date, \
     t.shipped_qty::text AS shipped_qty, t.received_qty::text AS received_qty, \
     t.qty_in_transit::text AS qty_in_transit, t.status, t.received_at, t.remark, \
     t.created_at, t.updated_at, s.ship_no AS ship_no, s.customer_id AS customer_id, \
     (CURRENT_DATE - t.ship_date) AS age_days";

/// 明细排序：**最老的在前**（账龄视角 —— 在途库存页面要一眼看到哪批货挂最久），
/// 同一天再按发货单 / 物料 / 仓库排，保证分页稳定（四列合起来唯一）。
const ROW_ORDER: &str = "ORDER BY t.ship_date ASC, t.shipment_id ASC, t.material_id ASC, t.warehouse_id ASC";

/// 明细查询条件（由 handler 从 [`crate::business::erp::inventory_transit::dto::TransitFilter`] 解析而来）。
#[derive(Debug, Clone, Default)]
pub struct DetailQuery {
    /// 状态（**必填**：handler 已经把留空归一成 `IN_TRANSIT`）。
    pub status: String,
    /// 物料。
    pub material_id: Option<Uuid>,
    /// 发货仓库。
    pub warehouse_id: Option<Uuid>,
    /// 客户（经发货单头）。
    pub customer_id: Option<Uuid>,
    /// 发货单。
    pub shipment_id: Option<Uuid>,
    /// 发货日期下界（含）。
    pub ship_date_from: Option<NaiveDate>,
    /// 发货日期上界（含）。
    pub ship_date_to: Option<NaiveDate>,
}

/// 汇总查询条件（恒为未签收在途，所以没有 `status`）。
#[derive(Debug, Clone, Default)]
pub struct SummaryQuery {
    /// 物料。
    pub material_id: Option<Uuid>,
    /// 发货仓库。
    pub warehouse_id: Option<Uuid>,
    /// 客户（经发货单头）。
    pub customer_id: Option<Uuid>,
}

impl SummaryQuery {
    /// 恒为未签收在途，所以只有三个 id 条件（见模块文档第 2 条口径）。
    pub fn new(material_id: Option<Uuid>, warehouse_id: Option<Uuid>, customer_id: Option<Uuid>) -> Self {
        Self {
            material_id,
            warehouse_id,
            customer_id,
        }
    }
}

/// 一行在途明细（库里的原样值 + 发货单头带出的两个字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitRow {
    /// 在途行主键。
    pub id: Uuid,
    /// 发货单。
    pub shipment_id: Uuid,
    /// 发货单号（单头软删 → `None`）。
    pub ship_no: Option<String>,
    /// 客户（单头软删 → `None`）。
    pub customer_id: Option<Uuid>,
    /// 物料。
    pub material_id: Uuid,
    /// 发货仓库。
    pub warehouse_id: Uuid,
    /// 发货日期。
    pub ship_date: NaiveDate,
    /// 在途天数（`CURRENT_DATE - ship_date`，库侧算）。
    pub age_days: i32,
    /// 发货数量。
    pub shipped_qty: Decimal,
    /// 已签收数量。
    pub received_qty: Decimal,
    /// 当前在途量（生成列）。
    pub qty_in_transit: Decimal,
    /// 状态。
    pub status: String,
    /// 签收时间。
    pub received_at: Option<DateTime<Utc>>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

/// 一行汇总（物料 × 发货仓）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRow {
    /// 物料。
    pub material_id: Uuid,
    /// 发货仓库。
    pub warehouse_id: Uuid,
    /// 未签收在途量合计。
    pub qty_in_transit: Decimal,
    /// 发货量合计。
    pub shipped_qty: Decimal,
    /// 已签收量合计。
    pub received_qty: Decimal,
    /// 涉及的发货单张数。
    pub shipments: i64,
    /// 最早发货日期（`yyyy-MM-dd`）。
    pub first_ship_date: String,
    /// 最早那批的在途天数。
    pub max_age_days: i32,
}

/// 主数据（物料）的展示字段（批量补齐，防 N+1）。
#[derive(Debug, Clone, PartialEq, Eq)]
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

// ==================== 条件拼装（明细 / 汇总各一份，口径只写一次） ====================

/// 明细 WHERE：恒带 `t.status = ?`，其余条件按需追加。
fn detail_where(query: &DetailQuery) -> (String, Vec<Value>) {
    let mut sql = String::from(" WHERE t.status = ?");
    let mut params: Vec<Value> = vec![Value::from(query.status.as_str())];
    if let Some(material_id) = query.material_id {
        sql.push_str(" AND t.material_id = ?::uuid");
        params.push(Value::from(&material_id));
    }
    if let Some(warehouse_id) = query.warehouse_id {
        sql.push_str(" AND t.warehouse_id = ?::uuid");
        params.push(Value::from(&warehouse_id));
    }
    if let Some(shipment_id) = query.shipment_id {
        sql.push_str(" AND t.shipment_id = ?::uuid");
        params.push(Value::from(&shipment_id));
    }
    if let Some(customer_id) = query.customer_id {
        // 客户挂在发货单头上（在途行没有客户列）；单头软删时 JOIN 出来是 NULL，
        // 于是按客户筛也筛不到它 —— 安全侧：宁可少一行，不把「不知道属于谁的货」算给某个客户。
        sql.push_str(" AND s.customer_id = ?::uuid");
        params.push(Value::from(&customer_id));
    }
    if let Some(from) = query.ship_date_from {
        sql.push_str(" AND t.ship_date >= ?::text::date");
        params.push(Value::Text(from.to_string()));
    }
    if let Some(to) = query.ship_date_to {
        sql.push_str(" AND t.ship_date <= ?::text::date");
        params.push(Value::Text(to.to_string()));
    }
    (sql, params)
}

/// 汇总 WHERE：恒带 `t.status = 'IN_TRANSIT'`（在途 = 未签收），其余条件按需追加。
///
/// 用**参数化**的状态而不是把字面量拼进 SQL —— 字面量拼进去，一旦哪天状态域改了，
/// 这条 WHERE 会静默变成「什么都查不到」。
fn summary_where(query: &SummaryQuery) -> (String, Vec<Value>) {
    let mut sql = String::from(" WHERE t.status = ?");
    let mut params: Vec<Value> = vec![Value::from(shipment_transit::STATUS_IN_TRANSIT)];
    if let Some(material_id) = query.material_id {
        sql.push_str(" AND t.material_id = ?::uuid");
        params.push(Value::from(&material_id));
    }
    if let Some(warehouse_id) = query.warehouse_id {
        sql.push_str(" AND t.warehouse_id = ?::uuid");
        params.push(Value::from(&warehouse_id));
    }
    if let Some(customer_id) = query.customer_id {
        sql.push_str(" AND s.customer_id = ?::uuid");
        params.push(Value::from(&customer_id));
    }
    (sql, params)
}

/// 在途行 → 发货单头的关联（明细与汇总共用同一句，保证两边「客户」口径一致）。
const SHIPMENT_JOIN: &str = " LEFT JOIN biz_shipment s ON s.id = t.shipment_id AND s.deleted_at IS NULL";

// ==================== 明细 ====================

/// 在途明细分页。
///
/// # 异常情况
///
/// 查库 / 行解析失败 → [`DbError`]（handler 直接 `?`）。SQL 里没有 `ORDER BY` / `LIMIT`
/// （由 `scope::paginate_unscoped` 自己拼），违者会被 `page_of` 的守卫报错。
pub async fn transit_page(query: &DetailQuery, page: u64, page_size: u64) -> DbResult<(Vec<TransitRow>, i64)> {
    let (where_sql, params) = detail_where(query);
    let sql = format!("SELECT {ROW_COLUMNS} FROM {TABLE} t{SHIPMENT_JOIN}{where_sql}");
    page_of(
        scope::ScopeTarget::unscoped(TABLE),
        sql,
        params,
        ROW_ORDER,
        page,
        page_size,
        TransitRow::from_row,
    )
    .await
}

impl TransitRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            shipment_id: row.get_typed("shipment_id")?,
            ship_no: row.get_typed("ship_no")?,
            customer_id: row.get_typed("customer_id")?,
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            ship_date: row.get_typed("ship_date")?,
            age_days: row.get_typed("age_days")?,
            shipped_qty: parse_decimal(row, "shipped_qty")?,
            received_qty: parse_decimal(row, "received_qty")?,
            qty_in_transit: parse_decimal(row, "qty_in_transit")?,
            status: row.get_typed("status")?,
            received_at: row.get_typed("received_at")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// `numeric` 一律 `::text` 出来再解析成定点数（驱动不做 `numeric → Decimal` 的直接映射，
/// 与 `shipment_transit::OpenLine::from_row` 同款；顺带避开浮点丢精度）。
fn parse_decimal(row: &Row, column: &str) -> DbResult<Decimal> {
    let text = row.get_typed::<String>(column)?;
    text.trim()
        .parse::<Decimal>()
        .map_err(|error| DbError::InvalidValue(format!("在途行 {column} 不是合法数值：{text}（{error}）")))
}

// ==================== 汇总 ====================

/// 按 物料 × 发货仓 汇总未签收在途。
///
/// 返回值：`(rows, truncated)` —— `rows` 最多 [`SUMMARY_ROW_LIMIT`] 行，
/// 命中第 `LIMIT + 1` 行说明还有别的组没返回（`truncated = true`）。
///
/// # 为什么多取一行
///
/// 只按 `LIMIT 500` 取，调用方无法区分「正好 500 组」与「被截断」——
/// 前者是完整答案、后者是残缺答案，两者在界面上长得一模一样。
pub async fn summary_rows(query: &SummaryQuery) -> DbResult<(Vec<SummaryRow>, bool)> {
    let (where_sql, params) = summary_where(query);
    let fetch = SUMMARY_ROW_LIMIT + 1;
    let sql = format!(
        "SELECT t.material_id, t.warehouse_id, \
                SUM(t.qty_in_transit)::text AS qty_in_transit, \
                SUM(t.shipped_qty)::text AS shipped_qty, \
                SUM(t.received_qty)::text AS received_qty, \
                count(DISTINCT t.shipment_id) AS shipments, \
                to_char(min(t.ship_date), 'YYYY-MM-DD') AS first_ship_date, \
                (CURRENT_DATE - min(t.ship_date)) AS max_age_days \
           FROM {TABLE} t{SHIPMENT_JOIN}{where_sql} \
          GROUP BY t.material_id, t.warehouse_id \
          ORDER BY SUM(t.qty_in_transit) DESC, t.material_id ASC, t.warehouse_id ASC \
          LIMIT {fetch}"
    );
    let rows = db().find_with_params(&sql, params).await?;
    let truncated = rows.len() > SUMMARY_ROW_LIMIT;
    let mut out = Vec::with_capacity(rows.len().min(SUMMARY_ROW_LIMIT));
    for row in rows.iter().take(SUMMARY_ROW_LIMIT) {
        out.push(SummaryRow {
            material_id: row.get_typed("material_id")?,
            warehouse_id: row.get_typed("warehouse_id")?,
            qty_in_transit: parse_decimal(row, "qty_in_transit")?,
            shipped_qty: parse_decimal(row, "shipped_qty")?,
            received_qty: parse_decimal(row, "received_qty")?,
            shipments: row.get_typed("shipments")?,
            first_ship_date: row.get_typed("first_ship_date")?,
            max_age_days: row.get_typed("max_age_days")?,
        });
    }
    Ok((out, truncated))
}

/// 汇总的合计（**不受** [`SUMMARY_ROW_LIMIT`] 影响）：`(在途合计, 发货单张数, 在途行数)`。
///
/// 与 [`summary_rows`] 共用同一个 WHERE（`summary_where`），所以「各组之和」与
/// 「合计」不可能各算一套 —— 只有截断时两者才不等，而那时 `truncated` 已经明确告诉调用方。
pub async fn summary_totals(query: &SummaryQuery) -> DbResult<(Decimal, i64, i64)> {
    let (where_sql, params) = summary_where(query);
    let sql = format!(
        "SELECT COALESCE(SUM(t.qty_in_transit), 0)::text AS qty_in_transit, \
                count(DISTINCT t.shipment_id) AS shipments, \
                count(*) AS lines \
           FROM {TABLE} t{SHIPMENT_JOIN}{where_sql}"
    );
    let rows = db().find_with_params(&sql, params).await?;
    let Some(row) = rows.first() else {
        return Err(DbError::InvalidValue(
            "在途汇总合计查询返回 0 行（聚合查询不应如此）".to_string(),
        ));
    };
    Ok((
        parse_decimal(row, "qty_in_transit")?,
        row.get_typed("shipments")?,
        row.get_typed("lines")?,
    ))
}

// ==================== 名称批量补齐（防 N+1） ====================

/// 批量取「物料 id → 编码 / 名称 / 规格 / 单位名」，分页与汇总各一次往返。
///
/// 软删物料不出现在结果里（调用方按缺失处理：`material_code` / `material_name` 留 `None`），
/// 与 `shipment_model::warehouse_labels` / `sales_order_model::customer_labels` 同口径。
pub async fn material_labels(ids: &[Uuid]) -> DbResult<Vec<MaterialBrief>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::BTreeSet::new();
    let unique: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    let placeholders = vec!["?::uuid"; unique.len()].join(", ");
    let sql = format!(
        "SELECT m.id, m.material_code AS code, m.material_name AS name, m.spec, u.unit_name \
           FROM biz_material m \
           LEFT JOIN biz_unit u ON u.id = m.unit_id AND u.deleted_at IS NULL \
          WHERE m.id IN ({placeholders}) AND m.deleted_at IS NULL"
    );
    let params: Vec<Value> = unique.iter().map(Value::from).collect();
    let rows = db().find_with_params(&sql, params).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(MaterialBrief {
            id: row.get_typed("id")?,
            code: row.get_typed("code")?,
            name: row.get_typed("name")?,
            spec: row.get_typed("spec")?,
            unit_name: row.get_typed("unit_name")?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// 明细 WHERE **恒带** `t.status = ?`（M2-03 契约：在途取数必须带状态过滤）。
    #[test]
    fn detail_where_always_filters_status() {
        let (sql, params) = detail_where(&DetailQuery {
            status: "IN_TRANSIT".to_string(),
            ..DetailQuery::default()
        });
        assert!(sql.contains("t.status = ?"), "sql={sql}");
        assert_eq!(params.len(), 1, "只有状态一个参数");
        println!("[M2-04] 空筛选的 WHERE = `{sql}`（恒带状态）✓");
    }

    /// 每个筛选条件各加一个 `?`，且**占位符数量与参数数量一致**（S8）。
    #[test]
    fn detail_where_params_match_placeholders() {
        use nik::prelude::chrono::NaiveDate;
        let date = |text: &str| NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日期");
        let query = DetailQuery {
            status: "RECEIVED".to_string(),
            material_id: Some(uid(0x01)),
            warehouse_id: Some(uid(0x02)),
            customer_id: Some(uid(0x03)),
            shipment_id: Some(uid(0x04)),
            ship_date_from: Some(date("2026-11-01")),
            ship_date_to: Some(date("2026-11-30")),
        };
        let (sql, params) = detail_where(&query);
        assert_eq!(
            sql.matches('?').count(),
            params.len(),
            "sql={sql} params={}",
            params.len()
        );
        assert_eq!(params.len(), 7, "状态 + 6 个条件");
        // 客户条件必须锚在发货单头（在途行没有客户列）
        assert!(sql.contains("s.customer_id = ?::uuid"), "sql={sql}");
        println!(
            "[M2-04] 全条件 WHERE 有 {} 个占位符 = {} 个参数 ✓",
            params.len(),
            params.len()
        );
    }

    /// 汇总 WHERE 恒为 `IN_TRANSIT`，且**不接受**外部状态（在途 = 未签收）。
    #[test]
    fn summary_where_is_always_open_transit() {
        let (sql, params) = summary_where(&SummaryQuery::default());
        assert!(sql.contains("t.status = ?"), "sql={sql}");
        assert_eq!(params.len(), 1);
        match &params[0] {
            Value::Text(text) => assert_eq!(text, "IN_TRANSIT", "汇总只算未签收在途"),
            other => panic!("状态参数应为文本 IN_TRANSIT，实际 {other:?}"),
        }
        println!("[M2-04] 汇总 WHERE = `{sql}`（恒 IN_TRANSIT）✓");
    }

    /// 明细与汇总必须用**同一句**发货单关联（客户口径不能各写一套）。
    #[test]
    fn both_paths_share_the_shipment_join() {
        assert!(SHIPMENT_JOIN.contains("LEFT JOIN biz_shipment"));
        assert!(SHIPMENT_JOIN.contains("s.deleted_at IS NULL"), "软删单头不出名字");
        let detail = format!("SELECT {ROW_COLUMNS} FROM {TABLE} t{SHIPMENT_JOIN}");
        assert!(detail.contains(SHIPMENT_JOIN));
        println!("[M2-04] 明细/汇总共用 `{SHIPMENT_JOIN}` ✓");
    }
}

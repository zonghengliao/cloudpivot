//! 销售目标 / 预测数据层（SO-44 / ARCH-7 §15.8）。
//!
//! 表结构唯一依据：`migrations/business/V0318__sales_target_forecast.up.sql`。
//!
//! | 表 | 行数级 | 关键不变量 |
//! |----|--------|------------|
//! | `biz_sales_target` | 几十~几百（人 × 期间） | 「范围 + 期间」唯一（存活行）；`ALL ⇒ scope_id IS NULL` |
//! | `biz_sales_forecast` | 几十~几千（客户 × 物料 × 期间 × 版本） | 「维度 + 期间 + 版本」唯一（存活行） |
//!
//! # 看板取数的三条 JOIN 口径（写错就是达成率造假）
//!
//! ```text
//! SALESPERSON      业务表的 salesperson_id 直接命中 sys_user.id
//! DEPT             业务表 → sys_user.salesperson_id → sys_user.dept_id 命中 sys_dept.id
//! MATERIAL_CATEGORY 业务表 → 明细分行 → material_id 命中 biz_material.id
//! ALL              不过滤（全公司口径）
//! ```
//!
//! **收款口径没有自己的归属列**：收款单只挂客户，不挂销售员。所以「按已收款」
//! 必须顺着单据链反查：`biz_receipt_allocation` → `biz_receivable`
//! →（`source_type='SALES_OUT'`）`biz_stock_out` →（`source_type='SALES'`）
//! `biz_shipment` → `biz_sales_order.salesperson_id`。链上任一环缺失（手工挂账、
//! 未过账出库、未关联发货单）就归属不到人 —— 这类收款只进 `ALL` 口径，
//! 这是账实一致的结果，不是漏算（handler 文档里也写明）。
//!
//! 物料维度在收款口径里要**分摊**：一张订单的收款不区分物料，按该物料行含税额
//! 占整单含税额的比例摊（明细口径见 [`receipt_actual_by_object`] 的 SQL）。
//!
//! # 数据权限
//!
//! ```text
//! SCOPE-EXEMPT: 目标 / 预测都是**配置表**（没有归属人 / 部门列，scope_id 是"被统计对象"
//!         而不是"数据归属"），可见性由 business:erp:sales-target:* / sales-forecast:*
//!         权限码控制。因此分页直接 `LIMIT/OFFSET`，不走 scope::paginate。
//! ```

use nik::prelude::chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::sales_target_dto::{
    ForecastFilter, ORDER_STATUSES, RECEIPT_STATUSES, SCOPE_DEPT, SCOPE_MATERIAL,
    SCOPE_SALESPERSON, TargetFilter, money2,
};
use crate::utils;

// ==================== 表名 / 列清单 ====================

/// 销售目标表。
pub const TARGET_TABLE: &str = "biz_sales_target";
/// 销售预测表。
pub const FORECAST_TABLE: &str = "biz_sales_forecast";

/// 目标列清单（与 V0318 逐列对应，顺序无关）。
pub const TARGET_COLUMNS: &str = "id, target_no, scope_type, scope_id, period_code, \
     target_amount, target_qty, status, remark, created_at, updated_at";

/// 预测列清单。
pub const FORECAST_COLUMNS: &str = "id, forecast_no, customer_id, material_id, period_code, \
     forecast_amount, confidence, version_no, remark, created_at, updated_at";

// ==================== 行结构 ====================

/// `biz_sales_target` 一行。
#[derive(Debug, Clone)]
pub struct TargetRow {
    /// 主键。
    pub id: Uuid,
    /// 目标编号。
    pub target_no: String,
    /// 范围类型。
    pub scope_type: String,
    /// 范围对象。
    pub scope_id: Option<Uuid>,
    /// 期间码。
    pub period_code: String,
    /// 目标金额。
    pub target_amount: Decimal,
    /// 目标数量。
    pub target_qty: Decimal,
    /// 状态。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl TargetRow {
    /// 由查询行构造（列名见 [`TARGET_COLUMNS`]）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            target_no: row.get_typed("target_no")?,
            scope_type: row.get_typed("scope_type")?,
            scope_id: row.get_typed("scope_id")?,
            period_code: row.get_typed("period_code")?,
            target_amount: row.get_typed("target_amount")?,
            target_qty: row.get_typed("target_qty")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 目标列表行 = 本体 + `scope_name`（`ALL` 为 `None`）。
pub type TargetListRow = (TargetRow, Option<String>);

/// `biz_sales_forecast` 一行。
#[derive(Debug, Clone)]
pub struct ForecastRow {
    /// 主键。
    pub id: Uuid,
    /// 预测编号。
    pub forecast_no: String,
    /// 客户。
    pub customer_id: Option<Uuid>,
    /// 物料。
    pub material_id: Option<Uuid>,
    /// 期间码。
    pub period_code: String,
    /// 预测金额。
    pub forecast_amount: Decimal,
    /// 信心度。
    pub confidence: String,
    /// 版本号。
    pub version_no: i32,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
}

impl ForecastRow {
    /// 由查询行构造。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            forecast_no: row.get_typed("forecast_no")?,
            customer_id: row.get_typed("customer_id")?,
            material_id: row.get_typed("material_id")?,
            period_code: row.get_typed("period_code")?,
            forecast_amount: row.get_typed("forecast_amount")?,
            confidence: row.get_typed("confidence")?,
            version_no: row.get_typed("version_no")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
        })
    }
}

/// 预测列表行 = 本体 + 客户名 + 物料名。
pub type ForecastListRow = (ForecastRow, Option<String>, Option<String>);

// ==================== 参数辅助 ====================

/// 可空 UUID 走文本绑定（`Value::Null` 绑 `::uuid` 驱动编不出来）。
fn uuid_text(value: Option<Uuid>) -> Value {
    Value::Text(value.map(|id| id.to_string()).unwrap_or_default())
}

/// 可空文本。
fn optional_text(value: Option<&str>) -> Value {
    Value::Text(value.unwrap_or_default().to_string())
}

/// 读派生列名（空白串折成 `None`）。
fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

// ==================== 目标：读 ====================

/// 目标列表 / 详情的公共 SELECT（三个互斥的 LEFT JOIN 派生 `scope_name`）。
fn target_select_sql() -> String {
    let columns: Vec<String> = TARGET_COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                CASE t.scope_type \
                     WHEN '{SCOPE_SALESPERSON}' THEN COALESCE(NULLIF(u.real_name, ''), u.username) \
                     WHEN '{SCOPE_DEPT}' THEN d.dept_name \
                     WHEN '{SCOPE_MATERIAL}' THEN m.material_name \
                     ELSE NULL \
                END AS scope_name \
           FROM {TARGET_TABLE} t \
           LEFT JOIN sys_user u ON u.id = t.scope_id AND t.scope_type = '{SCOPE_SALESPERSON}' \
           LEFT JOIN sys_dept d ON d.id = t.scope_id AND t.scope_type = '{SCOPE_DEPT}' \
           LEFT JOIN biz_material m ON m.id = t.scope_id AND t.scope_type = '{SCOPE_MATERIAL}'",
        columns.join(", ")
    )
}

fn map_target_row(row: &Row) -> DbResult<TargetListRow> {
    let scope_name: Option<String> = row.get_typed("scope_name")?;
    Ok((TargetRow::from_row(row)?, clean_optional(scope_name)))
}

/// 按主键取存活目标。
pub async fn find_target(id: Uuid) -> DbResult<Option<TargetListRow>> {
    let sql = format!(
        "{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL",
        target_select_sql()
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(map_target_row).transpose()
}

/// 某个范围对象在目标表里是否还有引用（改范围前提示用；本卡不阻断）。
pub async fn target_count_for_scope(scope_type: &str, scope_id: Uuid) -> DbResult<i64> {
    db().scalar_with_params(
        &format!(
            "SELECT count(*) FROM {TARGET_TABLE} \
              WHERE scope_type = ? AND scope_id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![Value::Text(scope_type.to_string()), Value::from(&scope_id)],
    )
    .await
}

/// 目标分页（期间 / 范围 / 状态精确，关键字模糊）。
pub async fn page_targets(filter: &TargetFilter) -> DbResult<(Vec<TargetListRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", target_select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(period) = filter.period_code.as_deref() {
        sql.push_str(" AND t.period_code = ?");
        params.push(Value::Text(period.to_string()));
    }
    if let Some(scope_type) = filter.scope_type.as_deref() {
        sql.push_str(" AND t.scope_type = ?");
        params.push(Value::Text(scope_type.to_string()));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(pattern) = utils::like_pattern(filter.keyword.as_deref()) {
        sql.push_str(
            " AND (t.target_no LIKE ? OR COALESCE(t.remark, '') LIKE ? \
                OR COALESCE(u.real_name, u.username, '') LIKE ? \
                OR COALESCE(d.dept_name, '') LIKE ? \
                OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..5 {
            params.push(Value::Text(pattern.clone()));
        }
    }
    let count_sql = format!("SELECT count(*) AS total FROM ({sql}) AS q");
    // count 直接包一层：JOIN 都在子查询里，别再造一遍 join 条件
    let count_row = db()
        .query_first_sql(count_sql, params.clone())
        .await
        .map_err(|error| DbError::InvalidValue(format!("统计销售目标失败：{error}")))?;
    let total: i64 = match count_row.as_ref() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };

    let mut list_params = params;
    list_params.push(Value::I64(
        i64::try_from(filter.page_size).unwrap_or(i64::MAX),
    ));
    list_params.push(Value::I64(
        i64::try_from(filter.page.saturating_sub(1) * filter.page_size).unwrap_or(0),
    ));
    let list_sql = format!(
        "{sql} ORDER BY t.period_code DESC, t.scope_type ASC, t.created_at DESC, t.id ASC \
         LIMIT ? OFFSET ?"
    );
    let rows = db().query_sql(list_sql, list_params).await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(map_target_row(row)?);
    }
    Ok((list, total))
}

/// 看板取数：某期间（可选某范围类型）的**启用**目标，按范围名排序。
pub async fn targets_for_period(
    period_code: &str,
    scope_type: Option<&str>,
) -> DbResult<Vec<TargetListRow>> {
    let mut sql = format!(
        "{} WHERE t.deleted_at IS NULL AND t.status = 'ACTIVE' AND t.period_code = ?",
        target_select_sql()
    );
    let mut params = vec![Value::Text(period_code.to_string())];
    if let Some(scope_type) = scope_type {
        sql.push_str(" AND t.scope_type = ?");
        params.push(Value::Text(scope_type.to_string()));
    }
    sql.push_str(" ORDER BY t.scope_type ASC, scope_name ASC NULLS FIRST, t.id ASC");
    let rows = db().query_sql(sql, params).await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(map_target_row(row)?);
    }
    Ok(list)
}

// ==================== 目标：写 ====================

/// 新建目标的 SQL（抽成函数，单测可断言「每个 uuid 参数都带 `::uuid` 转换」）。
fn insert_target_sql() -> String {
    format!(
        "INSERT INTO {TARGET_TABLE} \
            (id, target_no, scope_type, scope_id, period_code, target_amount, target_qty, \
             status, remark, create_by, update_by) \
         VALUES (?, ?, ?, NULLIF(?::text, '')::uuid, ?, ?::text::numeric, ?::text::numeric, \
                 ?, NULLIF(?::text, ''), NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)"
    )
}

/// 新建目标（`id` / `target_no` 由 handler 先取号再落库）。
pub async fn insert_target(
    id: Uuid,
    target_no: &str,
    row: &TargetRow,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    db().execute_with_params(
        &insert_target_sql(),
        vec![
            Value::from(&id),
            Value::Text(target_no.to_string()),
            Value::Text(row.scope_type.clone()),
            uuid_text(row.scope_id),
            Value::Text(row.period_code.clone()),
            Value::Text(row.target_amount.to_string()),
            Value::Text(row.target_qty.to_string()),
            Value::Text(row.status.clone()),
            optional_text(row.remark.as_deref()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 编辑目标（整行覆盖；软删 / 不存在 → 0，由 handler 翻 404）。
pub async fn update_target(id: Uuid, row: &TargetRow, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        &format!(
            "UPDATE {TARGET_TABLE} \
                SET scope_type = ?, scope_id = NULLIF(?::text, '')::uuid, period_code = ?, \
                    target_amount = ?::text::numeric, target_qty = ?::text::numeric, status = ?, \
                    remark = NULLIF(?::text, ''), updated_at = now(), \
                    update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            Value::Text(row.scope_type.clone()),
            uuid_text(row.scope_id),
            Value::Text(row.period_code.clone()),
            Value::Text(row.target_amount.to_string()),
            Value::Text(row.target_qty.to_string()),
            Value::Text(row.status.clone()),
            optional_text(row.remark.as_deref()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删目标（不物理删：编号是审计事实，且唯一索引带 `deleted_at IS NULL` 后编号可复用）。
pub async fn soft_delete_target(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        &format!(
            "UPDATE {TARGET_TABLE} SET deleted_at = now(), updated_at = now(), \
                    update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

// ==================== 预测：读 ====================

fn forecast_select_sql() -> String {
    let columns: Vec<String> = FORECAST_COLUMNS
        .split(',')
        .map(|column| format!("f.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, c.name AS customer_name, m.material_name AS material_name \
           FROM {FORECAST_TABLE} f \
           LEFT JOIN biz_customer c ON c.id = f.customer_id \
           LEFT JOIN biz_material m ON m.id = f.material_id",
        columns.join(", ")
    )
}

fn map_forecast_row(row: &Row) -> DbResult<ForecastListRow> {
    let customer_name: Option<String> = row.get_typed("customer_name")?;
    let material_name: Option<String> = row.get_typed("material_name")?;
    Ok((
        ForecastRow::from_row(row)?,
        clean_optional(customer_name),
        clean_optional(material_name),
    ))
}

/// 按主键取存活预测。
pub async fn find_forecast(id: Uuid) -> DbResult<Option<ForecastListRow>> {
    let sql = format!(
        "{} WHERE f.id = ?::uuid AND f.deleted_at IS NULL",
        forecast_select_sql()
    );
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(map_forecast_row).transpose()
}

/// 预测分页（期间 / 客户 / 物料 / 信心度精确）。
pub async fn page_forecasts(filter: &ForecastFilter) -> DbResult<(Vec<ForecastListRow>, i64)> {
    let mut sql = format!("{} WHERE f.deleted_at IS NULL", forecast_select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(period) = filter.period_code.as_deref() {
        sql.push_str(" AND f.period_code = ?");
        params.push(Value::Text(period.to_string()));
    }
    if let Some(customer) = filter.customer_id {
        sql.push_str(" AND f.customer_id = ?::uuid");
        params.push(Value::from(&customer));
    }
    if let Some(material) = filter.material_id {
        sql.push_str(" AND f.material_id = ?::uuid");
        params.push(Value::from(&material));
    }
    if let Some(confidence) = filter.confidence.as_deref() {
        sql.push_str(" AND f.confidence = ?");
        params.push(Value::Text(confidence.to_string()));
    }
    let count_sql = format!("SELECT count(*) AS total FROM ({sql}) AS q");
    let count_row = db()
        .query_first_sql(count_sql, params.clone())
        .await
        .map_err(|error| DbError::InvalidValue(format!("统计销售预测失败：{error}")))?;
    let total: i64 = match count_row.as_ref() {
        Some(row) => row.get_typed("total")?,
        None => 0,
    };

    let mut list_params = params;
    list_params.push(Value::I64(
        i64::try_from(filter.page_size).unwrap_or(i64::MAX),
    ));
    list_params.push(Value::I64(
        i64::try_from(filter.page.saturating_sub(1) * filter.page_size).unwrap_or(0),
    ));
    let list_sql = format!(
        "{sql} ORDER BY f.period_code DESC, f.version_no DESC, f.created_at DESC, f.id ASC \
         LIMIT ? OFFSET ?"
    );
    let rows = db().query_sql(list_sql, list_params).await?;
    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        list.push(map_forecast_row(row)?);
    }
    Ok((list, total))
}

// ==================== 预测：写 ====================

/// 新建预测的 SQL（抽成函数，理由同 [`insert_target_sql`]）。
fn insert_forecast_sql() -> String {
    format!(
        "INSERT INTO {FORECAST_TABLE} \
            (id, forecast_no, customer_id, material_id, period_code, forecast_amount, \
             confidence, version_no, remark, create_by, update_by) \
         VALUES (?, ?, NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid, ?, \
                 ?::text::numeric, ?, ?::int, NULLIF(?::text, ''), \
                 NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)"
    )
}

/// 新建预测。
pub async fn insert_forecast(
    id: Uuid,
    forecast_no: &str,
    row: &ForecastRow,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    db().execute_with_params(
        &insert_forecast_sql(),
        vec![
            Value::from(&id),
            Value::Text(forecast_no.to_string()),
            uuid_text(row.customer_id),
            uuid_text(row.material_id),
            Value::Text(row.period_code.clone()),
            Value::Text(row.forecast_amount.to_string()),
            Value::Text(row.confidence.clone()),
            Value::I32(row.version_no),
            optional_text(row.remark.as_deref()),
            uuid_text(operator),
            uuid_text(operator),
        ],
    )
    .await
}

/// 编辑预测（整行覆盖）。
pub async fn update_forecast(id: Uuid, row: &ForecastRow, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        &format!(
            "UPDATE {FORECAST_TABLE} \
                SET customer_id = NULLIF(?::text, '')::uuid, material_id = NULLIF(?::text, '')::uuid, \
                    period_code = ?, forecast_amount = ?::text::numeric, confidence = ?, \
                    version_no = ?::int, \
                    remark = NULLIF(?::text, ''), updated_at = now(), \
                    update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![
            uuid_text(row.customer_id),
            uuid_text(row.material_id),
            Value::Text(row.period_code.clone()),
            Value::Text(row.forecast_amount.to_string()),
            Value::Text(row.confidence.clone()),
            Value::I32(row.version_no),
            optional_text(row.remark.as_deref()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删预测。
pub async fn soft_delete_forecast(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().execute_with_params(
        &format!(
            "UPDATE {FORECAST_TABLE} SET deleted_at = now(), updated_at = now(), \
                    update_by = NULLIF(?::text, '')::uuid \
              WHERE id = ?::uuid AND deleted_at IS NULL"
        ),
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

// ==================== 范围对象存在性 ====================

/// 范围对象必须存在且未软删：`SALESPERSON` ⇒ `sys_user`、`DEPT` ⇒ `sys_dept`、
/// `MATERIAL_CATEGORY` ⇒ `biz_material`（基础版无分类表，见模块头注释）。
///
/// 写错表会让整类目标永远取不到实际数，而且**不会报错** —— 属于静默失效，
/// 必须在入口挡住。
pub async fn scope_target_exists(scope_type: &str, scope_id: Uuid) -> DbResult<bool> {
    let sql = match scope_type {
        SCOPE_SALESPERSON => {
            "SELECT count(*) FROM sys_user WHERE id = ?::uuid AND deleted_at IS NULL"
        }
        SCOPE_DEPT => "SELECT count(*) FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL",
        SCOPE_MATERIAL => {
            "SELECT count(*) FROM biz_material WHERE id = ?::uuid AND deleted_at IS NULL"
        }
        _ => return Ok(false),
    };
    let count: i64 = db()
        .scalar_with_params(sql, vec![Value::from(&scope_id)])
        .await?;
    Ok(count > 0)
}

// ==================== 达成率取数（订单口径 / 收款口径） ====================

/// 订单状态白名单 → SQL `IN (...)` 片段（值来自常量，不存在注入面）。
fn order_status_in() -> String {
    ORDER_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 收款状态白名单 → SQL `IN (...)`。
fn receipt_status_in() -> String {
    RECEIPT_STATUSES
        .iter()
        .map(|status| format!("'{status}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 订单口径金额表达式：**本位币**含税金额（`CASE` 里的 `o` 是 `biz_sales_order` 别名）。
///
/// V0314 / SO-40 起订单有 `base_total_with_tax`（建单时按锁定汇率算出）。
/// 目标达成率必须按**本位币**汇总：`total_with_tax` 是原币，多币种订单直接相加
/// 会把 226 USD 当 226 CNY —— E2E-02 实测正是这个数（1582 CNY + 226 USD 报 1808，
/// 真实本位币 3186.60，达成率虚高 76%）。
///
/// 本位币单的 `rate = 1`，两者相等；V0314 之前的老行 `base_*` 可能为 0，回落原币。
pub fn base_amount_expr() -> &'static str {
    "CASE WHEN o.base_total_with_tax > 0 THEN o.base_total_with_tax ELSE o.total_with_tax END"
}

/// 订单**明细行**的本位币含税金额表达式（`i` = 明细别名、`o` = 订单别名）。
///
/// 明细表没有 `base_*` 列（V0264 建表时没有多币种），按订单的「本位币 / 原币」比例
/// 逐行折算。逐行 round 后合计与订单 `base_total_with_tax` 可能差几分（尾差），
/// 这是有意的：物料口径是「分摊」，不是「必须与订单级完全相等」。
pub fn base_line_amount_expr() -> &'static str {
    "CASE WHEN o.total_with_tax > 0 AND o.base_total_with_tax > 0 \
       THEN ROUND((i.amount + i.tax_amount) * o.base_total_with_tax / o.total_with_tax, 4) \
       ELSE i.amount + i.tax_amount END"
}

/// 订单口径 · 按范围对象汇总实际金额（**本位币**含税）。
///
/// - `SALESPERSON`：`biz_sales_order.salesperson_id`
/// - `DEPT`：`salesperson_id → sys_user.dept_id`
/// - `MATERIAL_CATEGORY`：订单明细 `material_id`（一张多物料订单会同时计入多个物料）
pub async fn order_actual_by_object(
    scope_type: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> DbResult<Vec<(Uuid, Decimal)>> {
    let statuses = order_status_in();
    let amount = base_amount_expr();
    let sql = match scope_type {
        SCOPE_SALESPERSON => format!(
            "SELECT o.salesperson_id AS object_id, SUM({amount}) AS amount \
               FROM biz_sales_order o \
              WHERE o.deleted_at IS NULL AND o.salesperson_id IS NOT NULL \
                AND o.status IN ({statuses}) AND o.order_date >= ?::date AND o.order_date < ?::date \
              GROUP BY o.salesperson_id"
        ),
        SCOPE_DEPT => format!(
            "SELECT u.dept_id AS object_id, SUM({amount}) AS amount \
               FROM biz_sales_order o \
               JOIN sys_user u ON u.id = o.salesperson_id AND u.deleted_at IS NULL \
              WHERE o.deleted_at IS NULL AND u.dept_id IS NOT NULL \
                AND o.status IN ({statuses}) AND o.order_date >= ?::date AND o.order_date < ?::date \
              GROUP BY u.dept_id"
        ),
        SCOPE_MATERIAL => format!(
            "SELECT i.material_id AS object_id, SUM({}) AS amount \
               FROM biz_sales_order o \
               JOIN biz_sales_order_item i ON i.so_id = o.id \
              WHERE o.deleted_at IS NULL \
                AND o.status IN ({statuses}) AND o.order_date >= ?::date AND o.order_date < ?::date \
              GROUP BY i.material_id",
            base_line_amount_expr()
        ),
        _ => return Ok(Vec::new()),
    };
    let rows = db()
        .query_sql(sql, vec![Value::from(from), Value::from(to)])
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let object_id: Option<Uuid> = row.get_typed("object_id")?;
        let amount: Option<Decimal> = row.get_typed("amount")?;
        if let Some(object_id) = object_id {
            out.push((object_id, money2(amount.unwrap_or_default())));
        }
    }
    Ok(out)
}

/// 订单口径 · 全公司实际金额（`scope_type=ALL`，本位币含税）。
pub async fn order_actual_total(from: NaiveDate, to: NaiveDate) -> DbResult<Decimal> {
    let statuses = order_status_in();
    let amount = base_amount_expr();
    let total: Option<Decimal> = db()
        .scalar_with_params(
            &format!(
                "SELECT COALESCE(SUM({amount}), 0) \
                   FROM biz_sales_order o \
                  WHERE o.deleted_at IS NULL AND o.status IN ({statuses}) \
                    AND o.order_date >= ?::date AND o.order_date < ?::date"
            ),
            vec![Value::from(from), Value::from(to)],
        )
        .await?;
    Ok(money2(total.unwrap_or_default()))
}

/// 收款口径 · 按范围对象汇总**已收款净额**（原币：收款核销额 − 退款冲减额）。
///
/// 收款单没有销售员列，必须顺单据链反查（见模块头注释）：
/// `biz_receipt_allocation → biz_receivable → biz_stock_out(source_type='SALES')`
/// `→ biz_shipment → biz_sales_order`。退款走同一条链做**负数**事件，
/// 这样「按范围」的和与「全公司」口径对得上（两边都净额）。
///
/// `MATERIAL_CATEGORY` 按「该物料行含税额 / 整单含税额」把订单的收款净额分摊到物料。
/// 含税额为 0 的订单（赠样单）分摊结果记 0，不报错。
pub async fn receipt_actual_by_object(
    scope_type: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> DbResult<Vec<(Uuid, Decimal)>> {
    let statuses = receipt_status_in();
    let refund_statuses = statuses.clone();
    // 两个口径共用的事件 CTE：收款为正、退款为负，都落在 so_id 上
    let events = format!(
        "WITH ev AS ( \
             SELECT sh.so_id, a.amount AS amt \
               FROM biz_receipt_allocation a \
               JOIN biz_receipt r ON r.id = a.receipt_id \
                    AND r.deleted_at IS NULL AND r.status IN ({statuses}) \
                    AND r.receipt_date >= ?::date AND r.receipt_date < ?::date \
               JOIN biz_receivable ar ON ar.id = a.receivable_id AND ar.deleted_at IS NULL \
               JOIN biz_stock_out so ON so.id = ar.source_id \
                    AND so.source_type = 'SALES' AND so.deleted_at IS NULL \
               JOIN biz_shipment sh ON sh.id = so.source_id AND sh.deleted_at IS NULL \
             UNION ALL \
             SELECT sh.so_id, -a.amount \
               FROM biz_receipt_refund_allocation a \
               JOIN biz_receipt_refund f ON f.id = a.refund_id \
                    AND f.deleted_at IS NULL AND f.status IN ({refund_statuses}) \
                    AND f.refund_date >= ?::date AND f.refund_date < ?::date \
               JOIN biz_receivable ar ON ar.id = a.receivable_id AND ar.deleted_at IS NULL \
               JOIN biz_stock_out so ON so.id = ar.source_id \
                    AND so.source_type = 'SALES' AND so.deleted_at IS NULL \
               JOIN biz_shipment sh ON sh.id = so.source_id AND sh.deleted_at IS NULL) "
    );
    let sql = match scope_type {
        SCOPE_SALESPERSON => format!(
            "{events} \
             SELECT o.salesperson_id AS object_id, SUM(ev.amt) AS amount \
               FROM ev JOIN biz_sales_order o ON o.id = ev.so_id AND o.deleted_at IS NULL \
              WHERE o.salesperson_id IS NOT NULL \
              GROUP BY o.salesperson_id"
        ),
        SCOPE_DEPT => format!(
            "{events} \
             SELECT u.dept_id AS object_id, SUM(ev.amt) AS amount \
               FROM ev \
               JOIN biz_sales_order o ON o.id = ev.so_id AND o.deleted_at IS NULL \
               JOIN sys_user u ON u.id = o.salesperson_id AND u.deleted_at IS NULL \
              WHERE u.dept_id IS NOT NULL \
              GROUP BY u.dept_id"
        ),
        SCOPE_MATERIAL => format!(
            "{events} \
             , received AS (SELECT so_id, SUM(amt) AS amount FROM ev GROUP BY so_id) \
             , order_total AS ( \
                 SELECT so_id, SUM(amount + tax_amount) AS total \
                   FROM biz_sales_order_item GROUP BY so_id) \
             , order_material AS ( \
                 SELECT so_id, material_id, SUM(amount + tax_amount) AS total \
                   FROM biz_sales_order_item GROUP BY so_id, material_id) \
             SELECT m.material_id AS object_id, \
                    SUM(r.amount * m.total / t.total) AS amount \
               FROM order_material m \
               JOIN order_total t ON t.so_id = m.so_id AND t.total <> 0 \
               JOIN received r ON r.so_id = m.so_id \
              GROUP BY m.material_id"
        ),
        _ => return Ok(Vec::new()),
    };
    let rows = db()
        .query_sql(
            sql,
            vec![
                Value::from(from),
                Value::from(to),
                Value::from(from),
                Value::from(to),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let object_id: Option<Uuid> = row.get_typed("object_id")?;
        let amount: Option<Decimal> = row.get_typed("amount")?;
        if let Some(object_id) = object_id {
            out.push((object_id, money2(amount.unwrap_or_default())));
        }
    }
    Ok(out)
}

/// 收款口径 · 全公司实际金额（`scope_type=ALL`）。
pub async fn receipt_actual_total(from: NaiveDate, to: NaiveDate) -> DbResult<Decimal> {
    let statuses = receipt_status_in();
    let total: Option<Decimal> = db()
        .scalar_with_params(
            &format!(
                // 事件式（与 `receipt_actual_by_object` 同口径）：收款按 `receipt_date`、
                // 退款按 `refund_date` 各自落期间，都是**净额**。
                // 不能写成「期间内收款 − 其挂的全部退款」：那样 11 月收 5000、12 月退 1000 时，
                // 11 月 ALL 口径会凭空少 1000，而按范围口径仍是 5000 —— 两个口径对不上。
                "SELECT COALESCE(SUM(ev.amt), 0) FROM ( \
                     SELECT r.amount AS amt \
                       FROM biz_receipt r \
                      WHERE r.deleted_at IS NULL AND r.status IN ({statuses}) \
                        AND r.receipt_date >= ?::date AND r.receipt_date < ?::date \
                     UNION ALL \
                     SELECT -a.amount AS amt \
                       FROM biz_receipt_refund_allocation a \
                       JOIN biz_receipt_refund f ON f.id = a.refund_id AND f.deleted_at IS NULL \
                            AND f.status IN ({statuses}) \
                            AND f.refund_date >= ?::date AND f.refund_date < ?::date \
                 ) ev"
            ),
            vec![
                Value::from(from),
                Value::from(to),
                Value::from(from),
                Value::from(to),
            ],
        )
        .await?;
    Ok(money2(total.unwrap_or_default()))
}

// ==================== 取号（`STG-` / `SFC-`） ====================

/// 北京时间日期键（`yyyyMMdd`）。
pub fn period_key_of(now: DateTime<Utc>) -> String {
    let offset = FixedOffset::east_opt(8 * 3600).expect("CST 偏移恒定");
    let local = now.with_timezone(&offset);
    format!("{:04}{:02}{:02}", local.year(), local.month(), local.day())
}

/// 单号格式：`{prefix}-{yyyyMMdd}-{流水 6 位}`。
pub fn format_no(prefix: &str, period_key: &str, no: i64) -> String {
    format!("{prefix}-{period_key}-{no:06}")
}

/// `biz_seq` 缺失时幂等补一行（正常库有 V0318 的种子）。
///
/// 用 `ON CONFLICT … DO NOTHING` 而不是「捕获唯一键冲突」：失败的语句会把当前事务打成
/// aborted（PG `25P02`），取号在建单事务内 ⇒ 首次并发建单会 500（FIX-8 实测）。
async fn ensure_seq_row(
    ex: &impl SqlExecutor,
    seq_type: &str,
    seq_name: &str,
    remark: &str,
) -> DbResult<()> {
    // SCOPE-EXEMPT: biz_seq 是全局取号规则字典，没有归属列。
    ex.exec_sql(
        "INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, \
                              current_no, step, separator, status, remark) \
         SELECT ?, ?, ?, ?, 'yyyyMMdd', '', 0, 1, '-', 1, ? \
         ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING"
            .to_string(),
        vec![
            Value::from(Uuid::now_v7()),
            Value::Text(seq_type.to_string()),
            Value::Text(seq_name.to_string()),
            Value::Text(seq_type.to_string()),
            Value::Text(remark.to_string()),
        ],
    )
    .await
    .or_else(|err| match err {
        DbError::UniqueViolation { .. } => Ok(0),
        other => Err(other),
    })?;
    Ok(())
}

/// 推进 `biz_seq` 一个号（同日累加 / 新日重置 / 时钟回拨不重置）。
async fn bump_seq(
    ex: &impl SqlExecutor,
    seq_type: &str,
    period_key: &str,
) -> DbResult<Option<i64>> {
    let rows = ex
        .query_sql(
            "UPDATE biz_seq \
                SET current_no = CASE WHEN period_key = ? THEN current_no + 1 \
                                      WHEN period_key < ? THEN 1 \
                                      ELSE current_no + 1 END, \
                    period_key = CASE WHEN period_key < ? THEN ? ELSE period_key END, \
                    updated_at = now() \
              WHERE seq_type = ? AND deleted_at IS NULL \
             RETURNING current_no"
                .to_string(),
            vec![
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::from(period_key),
                Value::Text(seq_type.to_string()),
            ],
        )
        .await?;
    rows.first()
        .map(|row| row.get_typed("current_no"))
        .transpose()
}

/// 取一个目标号（`STG-yyyyMMdd-000001`）。
pub async fn next_target_no(now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(
        db(),
        "STG",
        "销售目标编号",
        "销售目标编号：STG-yyyyMMdd-000001，按天（CST）滚动",
    )
    .await?;
    match bump_seq(db(), "STG", &period_key).await? {
        Some(no) => Ok(format_no("STG", &period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='STG') 行不存在（period_key={period_key}）"
        ))),
    }
}

/// 取一个预测号（`SFC-yyyyMMdd-000001`）。
pub async fn next_forecast_no(now: DateTime<Utc>) -> DbResult<String> {
    let period_key = period_key_of(now);
    ensure_seq_row(
        db(),
        "SFC",
        "销售预测编号",
        "销售预测编号：SFC-yyyyMMdd-000001，按天（CST）滚动",
    )
    .await?;
    match bump_seq(db(), "SFC", &period_key).await? {
        Some(no) => Ok(format_no("SFC", &period_key, no)),
        None => Err(DbError::InvalidValue(format!(
            "取号失败：biz_seq(seq_type='SFC') 行不存在（period_key={period_key}）"
        ))),
    }
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::chrono::NaiveDate;

    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(year, month, day)
            .expect("日期合法")
            .and_hms_opt(hour, minute, 0)
            .expect("时间合法")
            .and_utc()
    }

    // ---------- 取号：北京时间日期键 ----------

    #[test]
    fn period_key_uses_cst_not_utc() {
        // 2026-11-07T03:00Z = 11:00 CST ⇒ 当天
        assert_eq!(period_key_of(utc(2026, 11, 7, 3, 0)), "20261107");
        // 2026-11-06T16:00Z = 11-07 00:00 CST ⇒ 已跨到次日（按 UTC 算会写成 20261106）
        assert_eq!(period_key_of(utc(2026, 11, 6, 16, 0)), "20261107");
        // 2026-12-31T16:30Z = 2027-01-01 00:30 CST ⇒ 年份也要滚
        assert_eq!(period_key_of(utc(2026, 12, 31, 16, 30)), "20270101");
    }

    #[test]
    fn format_no_pads_six_digits() {
        assert_eq!(format_no("STG", "20261107", 1), "STG-20261107-000001");
        assert_eq!(format_no("SFC", "20261107", 42), "SFC-20261107-000042");
        // 超过 6 位不截断（单号前缀 + 日期 + 流水，长度仍远小于 varchar(32)）
        assert_eq!(
            format_no("STG", "20261107", 1234567),
            "STG-20261107-1234567"
        );
        assert!(format_no("STG", "20261107", 1).len() <= 32);
    }

    // ---------- 状态白名单拼进 SQL ----------

    #[test]
    fn order_status_whitelist_excludes_cancelled() {
        let list = order_status_in();
        for status in ORDER_STATUSES {
            assert!(list.contains(&format!("'{status}'")), "{list}");
        }
        assert!(!list.contains("CANCELLED"), "作废单不得进业绩：{list}");
        assert!(!list.contains("DRAFT"), "草稿不得进业绩：{list}");
    }

    #[test]
    fn receipt_status_whitelist_only_confirmed_and_posted() {
        let list = receipt_status_in();
        assert_eq!(list, "'CONFIRMED', 'POSTED'");
        assert!(!list.contains("DRAFT"));
        assert!(!list.contains("VOID"));
    }

    // ---------- 订单口径必须按本位币（E2E-02 回归） ----------

    #[test]
    fn order_basis_uses_base_currency_not_original() {
        // 目标达成率按本位币汇总：外币单的 total_with_tax 是原币，直接相加会虚高。
        let expr = base_amount_expr();
        assert!(expr.contains("base_total_with_tax"), "{expr}");
        assert!(
            expr.contains("ELSE o.total_with_tax"),
            "老数据要回落原币：{expr}"
        );
        // 明细口径按订单比例折算，同样不能直接用原币行金额
        let line = base_line_amount_expr();
        assert!(line.contains("base_total_with_tax"), "{line}");
        assert!(line.contains("i.amount + i.tax_amount"), "{line}");
    }

    // ---------- 写语句的参数绑定（回归：uuid 位必须带 ::uuid） ----------
    //
    // 实测踩过：`create_by` / `update_by` 写成裸 `?, ?`，驱动把 `Value::Text(uuid 串)`
    // 绑到 PG 推断出的 `uuid` 参数上 ⇒ `query failed: error serializing parameter 9`
    // （参数序号 0 起算，9 = create_by），接口 500。这条用例把转换钉死在 SQL 文本上。

    #[test]
    fn insert_target_sql_binds_uuid_columns_with_cast() {
        let sql = insert_target_sql();
        assert!(sql.contains(TARGET_TABLE));
        // 11 个参数位
        assert_eq!(sql.matches('?').count(), 11, "{sql}");
        // uuid 列 3 处：scope_id / create_by / update_by
        assert_eq!(sql.matches("::uuid").count(), 3, "{sql}");
        // 金额 / 数量走文本再转 numeric（驱动不接受 Text → NUMERIC 直接绑定）
        assert_eq!(sql.matches("?::text::numeric").count(), 2, "{sql}");
        // 尾部两个审计列必须都是 uuid 转换，不能是裸 `?, ?`
        assert!(
            sql.contains("NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)"),
            "{sql}"
        );
    }

    #[test]
    fn insert_forecast_sql_binds_uuid_columns_with_cast() {
        let sql = insert_forecast_sql();
        assert!(sql.contains(FORECAST_TABLE));
        assert_eq!(sql.matches('?').count(), 11, "{sql}");
        // uuid 列 4 处：customer_id / material_id / create_by / update_by
        assert_eq!(sql.matches("::uuid").count(), 4, "{sql}");
        assert_eq!(sql.matches("?::text::numeric").count(), 1, "{sql}");
        // version_no 是 INT，必须显式 ?::int（Text 绑 INT 同样会 serializing 失败）
        assert!(sql.contains("?::int"), "{sql}");
        assert!(
            sql.contains("NULLIF(?::text, '')::uuid, NULLIF(?::text, '')::uuid)"),
            "{sql}"
        );
    }
}

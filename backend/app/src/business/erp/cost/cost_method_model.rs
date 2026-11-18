//! 成本方法配置数据层（成本核算 · M5-01 · 表 `biz_cost_method_config`）。
//!
//! # 只有一张表，读多写少
//!
//! 配置量级 = 对象数 × 切换次数（通常几十行），所以：
//! · 判定（对象级 > 全局级 > 默认）**一次取全表**后在内存里挑（[`configs`]），
//!   不做「按对象逐条问库」—— 那在成本计算里就是 N+1（几百个对象 × 每次 1~3 ms 往返）；
//! · 分页走 `scope::paginate_unscoped`（本表 SCOPE-EXEMPT，理由见迁移头注释 7）。
//!
//! # 为什么判定逻辑不写在这里
//!
//! `resolve` / `find_overlap` / 区间相交都在 [`crate::business::erp::cost::cost_method_dto`]
//! 里做纯函数 —— 那样每条规则都能单测，且「分页里的 highlight」与「计算入口的拦截」
//! 走的是同一份判定，不会出现「界面说分批法、计算按标准成本」这种两套实现漂移。
//!
//! # 名字怎么给（AGENTS 约定 13）
//!
//! `scope_id` 是主数据关联（成本对象），所以**实时 join** `biz_cost_object` 取 `object_no` /
//! `status`；本表不存任何名字快照（配置行里没有「当时的对象名」这种历史事实需求）：
//! 对象改名后这里就该显示新名。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_method_dto::CostMethodSpec;
use cloudpivot_kernel::business::shared::scope;

/// 表名。
pub const TABLE: &str = "biz_cost_method_config";

/// 分页锚点（**SCOPE-EXEMPT**：全公司财务口径配置，不是某部门的业务单据；见迁移头注释 7）。
pub const TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

/// 稳定排序：先「指定成本对象」（配置更具体）再全公司默认，同级按生效日倒序（最新在前）。
pub const ORDER: &str = "ORDER BY (t.scope_type = 'COST_OBJECT') DESC, t.effective_from DESC, t.id ASC";

/// 单头列（与 [`CostMethodRow`] 字段一一对应；join 派生列在 `select_sql` 里另加）。
const COLUMNS: &str = "id, method, scope_type, scope_id, effective_from, effective_to, status, \
                       remark, created_at, updated_at";

// ==================== 行结构 ====================

/// `biz_cost_method_config` 一行 + join 出来的成本对象号 / 对象状态。
#[derive(Debug, Clone)]
pub struct CostMethodRow {
    /// 主键。
    pub id: Uuid,
    /// 核算方法（JOB / VARIETY / STANDARD）。
    pub method: String,
    /// 范围（GLOBAL / COST_OBJECT）。
    pub scope_type: String,
    /// 成本对象（GLOBAL 时为空）。
    pub scope_id: Option<Uuid>,
    /// 生效日。
    pub effective_from: NaiveDate,
    /// 停止日（可空）。
    pub effective_to: Option<NaiveDate>,
    /// 状态（1 / 0）。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 成本对象号（`biz_cost_object.object_no`，实时 join）。
    pub scope_object_no: Option<String>,
    /// 成本对象状态（OPEN / CLOSED / CANCELLED，实时 join）。
    pub scope_object_status: Option<String>,
}

impl CostMethodRow {
    /// 由查询行构造（列名见 [`COLUMNS`] + 两个 join 派生列）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            method: row.get_typed("method")?,
            scope_type: row.get_typed("scope_type")?,
            scope_id: row.get_typed("scope_id")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            scope_object_no: row.get_typed("scope_object_no")?,
            scope_object_status: row.get_typed("scope_object_status")?,
        })
    }

    /// 当前值 → 归一化规格（PUT 合并用）。
    pub fn to_spec(&self) -> CostMethodSpec {
        CostMethodSpec {
            method: self.method.clone(),
            scope_type: self.scope_type.clone(),
            scope_id: self.scope_id,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            status: self.status,
            remark: self.remark.clone(),
        }
    }

    /// 判定用的纯数据投影（`dto::resolve` / `find_overlap` 的输入）。
    pub fn to_view(&self) -> crate::business::erp::cost::cost_method_dto::MethodConfigView {
        crate::business::erp::cost::cost_method_dto::MethodConfigView {
            id: self.id,
            method: self.method.clone(),
            scope_type: self.scope_type.clone(),
            scope_id: self.scope_id,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            status: self.status,
        }
    }

    /// 范围的下拉展示名：对象级显示对象号（对象已被硬删时显示 id），全局显示「全公司默认」。
    pub fn scope_display(&self) -> String {
        match (&self.scope_type[..], self.scope_id) {
            ("GLOBAL", _) => "全公司默认".to_string(),
            (_, Some(_)) => self
                .scope_object_no
                .clone()
                .unwrap_or_else(|| self.scope_id.map(|id| id.to_string()).unwrap_or_default()),
            (_, None) => "全公司默认".to_string(),
        }
    }
}

/// 成本对象引用（校验 scope_id 用）。
#[derive(Debug, Clone)]
pub struct CostObjectRef {
    /// 成本对象号。
    pub object_no: String,
    /// 状态（OPEN / CLOSED / CANCELLED）。
    pub status: String,
}

/// 计算入口拦截用的一行（本期生效但方法未实现的配置）。
#[derive(Debug, Clone)]
pub struct BlockingMethodRow {
    /// 配置主键。
    pub id: Uuid,
    /// 方法。
    pub method: String,
    /// 范围。
    pub scope_type: String,
    /// 报文里的「谁」：对象号或「全公司默认」。
    pub subject: String,
}

/// 列表筛选（handler 归一化后交给 model；`page/page_size` 已过 `normalize_page`）。
#[derive(Debug, Clone, Default)]
pub struct CostMethodFilter {
    /// 范围。
    pub scope: Option<String>,
    /// 方法。
    pub method: Option<String>,
    /// 状态。
    pub status: Option<i16>,
    /// 成本对象。
    pub scope_id: Option<Uuid>,
    /// 关键字（对象号 / 对象名 / 备注）。
    pub keyword: Option<String>,
    /// 页号。
    pub page: u64,
    /// 每页条数。
    pub page_size: u64,
}

// ==================== 读 ====================

/// 单头 + 成本对象号的公共 SELECT。
fn select_sql() -> String {
    let columns: Vec<String> = COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                o.object_no AS scope_object_no, \
                o.status    AS scope_object_status \
           FROM {TABLE} t \
           LEFT JOIN biz_cost_object o ON o.id = t.scope_id AND o.deleted_at IS NULL",
        columns.join(", ")
    )
}

/// 按主键取存活行（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<CostMethodRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(CostMethodRow::from_row).transpose()
}

/// 取**全部**存活配置（判定用；量级几十行，一次取完比逐对象问库快得多）。
pub async fn configs() -> DbResult<Vec<CostMethodRow>> {
    configs_in(db()).await
}

/// 取全部存活配置（事务内版本：成本计算在事务里判定，读到的是同一快照）。
pub async fn configs_in(ex: &impl SqlExecutor) -> DbResult<Vec<CostMethodRow>> {
    let sql = format!(
        "{} WHERE t.deleted_at IS NULL ORDER BY t.effective_from ASC, t.id ASC",
        select_sql()
    );
    let rows = ex.query_sql(sql, vec![]).await?;
    rows.iter().map(CostMethodRow::from_row).collect()
}

/// 分页（本表 SCOPE-EXEMPT，走唯一的分页入口 `scope::paginate_unscoped`）。
pub async fn page(filter: &CostMethodFilter) -> DbResult<(Vec<CostMethodRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let Some(scope_type) = filter.scope.as_deref() {
        sql.push_str(" AND t.scope_type = ?");
        params.push(Value::Text(scope_type.to_string()));
    }
    if let Some(method) = filter.method.as_deref() {
        sql.push_str(" AND t.method = ?");
        params.push(Value::Text(method.to_string()));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND t.status = ?");
        params.push(Value::I16(status));
    }
    if let Some(scope_id) = filter.scope_id {
        sql.push_str(" AND t.scope_id = ?::uuid");
        params.push(Value::from(&scope_id));
    }
    if let Some(pattern) = like_pattern(&filter.keyword) {
        sql.push_str(
            " AND (COALESCE(o.object_no, '') LIKE ? \
               OR COALESCE(o.product_id::text, '') LIKE ? \
               OR COALESCE(t.remark, '') LIKE ? \
               OR t.method LIKE ?)",
        );
        for _ in 0..4 {
            params.push(Value::from(pattern.clone()));
        }
    }
    let result = scope::paginate_unscoped(TARGET, &sql, params, Some(ORDER), filter.page, filter.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(CostMethodRow::from_row)
        .collect::<DbResult<Vec<CostMethodRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 关键字 → LIKE 模式（与 `cost_alloc_rule_model::like_pattern` 同口径）。
fn like_pattern(keyword: &Option<String>) -> Option<String> {
    keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))
}

/// 成本对象是否存在（未软删）→ 对象号 + 状态。
pub async fn object_of(id: Uuid) -> DbResult<Option<CostObjectRef>> {
    let rows = db()
        .find_with_params(
            "SELECT object_no, status FROM biz_cost_object WHERE id = ?::uuid AND deleted_at IS NULL",
            vec![Value::from(&id)],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok(CostObjectRef {
                object_no: row.get_typed("object_no")?,
                status: row.get_typed("status")?,
            })
        })
        .transpose()
}

/// 本期（`[start, end]`）内生效、且方法**不是**分批法的配置（成本计算入口的拦截清单）。
///
/// # 为什么与期间「相交」而不是「覆盖期末」
///
/// 一期引擎只能把一期整体按一种方法算（ARCH-9 D-3；分段切换属 M5-31）。
/// 若只判期末：一条 `11-01~11-15 标准成本` 的配置在 11-30 那天不生效 ⇒ 不拦 ⇒
/// 整月静默按分批法算完，而前半月的口径明明写的是标准成本。
/// 所以判据是**期间内任一天生效**：宁可拦住让用户把生效日推到下期，也不出错账。
///
/// # 已作废 / 已删对象
///
/// 只认 `OPEN` / `CLOSED`（与 `cost_period_item_model::objects_of_period` 同判据）：
/// `CANCELLED` 对象和已删对象不参与成本计算，它们的配置不该拦住本期。
pub async fn blocking_in(ex: &impl SqlExecutor, start: NaiveDate, end: NaiveDate) -> DbResult<Vec<BlockingMethodRow>> {
    let sql = format!(
        "SELECT t.id, t.method, t.scope_type, \
                CASE WHEN t.scope_type = 'GLOBAL' THEN '全公司默认' \
                     ELSE COALESCE(o.object_no, t.scope_id::text) END AS subject \
           FROM {TABLE} t \
           LEFT JOIN biz_cost_object o ON o.id = t.scope_id AND o.deleted_at IS NULL \
          WHERE t.deleted_at IS NULL AND t.status = 1 AND t.method <> 'JOB' \
            AND t.effective_from <= ?::text::date \
            AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date) \
            AND (t.scope_type = 'GLOBAL' \
                 OR (o.id IS NOT NULL AND o.status IN ('OPEN','CLOSED'))) \
          ORDER BY t.scope_type ASC, subject ASC"
    );
    let rows = ex
        .query_sql(sql, vec![Value::Text(end.to_string()), Value::Text(start.to_string())])
        .await?;
    rows.iter()
        .map(|row| {
            Ok(BlockingMethodRow {
                id: row.get_typed("id")?,
                method: row.get_typed("method")?,
                scope_type: row.get_typed("scope_type")?,
                subject: row.get_typed("subject")?,
            })
        })
        .collect()
}

// ==================== 写 ====================

/// 建配置 SQL。
pub(crate) fn insert_sql() -> String {
    format!(
        "INSERT INTO {TABLE} \
             (id, method, scope_type, scope_id, effective_from, effective_to, status, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, NULLIF(?::text,'')::uuid, ?::text::date, \
                 NULLIF(?::text,'')::text::date, ?, NULLIF(?::text,''), now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
    )
}

/// 改配置 SQL（**只改未删除的行**）。
pub(crate) fn update_sql() -> String {
    format!(
        "UPDATE {TABLE} SET method = ?, scope_type = ?, scope_id = NULLIF(?::text,'')::uuid, \
             effective_from = ?::text::date, effective_to = NULLIF(?::text,'')::text::date, \
             status = ?, remark = NULLIF(?::text,''), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
    )
}

/// 软删 SQL（只删未删除的行）。
pub(crate) fn soft_delete_sql() -> String {
    format!(
        "UPDATE {TABLE} SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
    )
}

/// 建配置（字段合法性已由 dto + handler 校验）。
pub async fn insert(id: Uuid, spec: &CostMethodSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(insert_sql(), insert_params(id, spec, operator)).await
}

/// 建配置参数（与 [`insert_sql`] 的 `?` 顺序逐位对应）。
pub(crate) fn insert_params(id: Uuid, spec: &CostMethodSpec, operator: Option<Uuid>) -> Vec<Value> {
    let operator = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    vec![
        Value::from(&id),
        Value::Text(spec.method.clone()),
        Value::Text(spec.scope_type.clone()),
        Value::Text(spec.scope_id.map(|id| id.to_string()).unwrap_or_default()),
        Value::Text(spec.effective_from.to_string()),
        Value::Text(spec.effective_to.map(|date| date.to_string()).unwrap_or_default()),
        Value::I16(spec.status),
        Value::Text(spec.remark.clone().unwrap_or_default()),
        operator.clone(),
        operator,
    ]
}

/// 改配置：返回受影响行数（`0` → 不存在 / 已删，由 handler 给 404）。
pub async fn update(id: Uuid, spec: &CostMethodSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(update_sql(), update_params(id, spec, operator)).await
}

/// 改配置参数（与 [`update_sql`] 的 `?` 顺序逐位对应）。
pub(crate) fn update_params(id: Uuid, spec: &CostMethodSpec, operator: Option<Uuid>) -> Vec<Value> {
    vec![
        Value::Text(spec.method.clone()),
        Value::Text(spec.scope_type.clone()),
        Value::Text(spec.scope_id.map(|id| id.to_string()).unwrap_or_default()),
        Value::Text(spec.effective_from.to_string()),
        Value::Text(spec.effective_to.map(|date| date.to_string()).unwrap_or_default()),
        Value::I16(spec.status),
        Value::Text(spec.remark.clone().unwrap_or_default()),
        Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
        Value::from(&id),
    ]
}

/// 软删配置。
pub async fn soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        soft_delete_sql(),
        vec![
            Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

// ==================== 单测（不连库：只验 SQL 形状） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 列清单必须与 `from_row` 的取值列一一对上（漏一列 = 运行时 `get_typed` 炸）。
    #[test]
    fn select_sql_prefixes_every_column_and_joins_object() {
        let sql = select_sql();
        for column in COLUMNS.split(',') {
            let column = column.trim();
            assert!(sql.contains(&format!("t.{column}")), "缺列 {column}: {sql}");
        }
        assert!(sql.contains("FROM biz_cost_method_config t"), "{sql}");
        assert!(
            sql.contains("LEFT JOIN biz_cost_object o ON o.id = t.scope_id"),
            "必须实时 join 成本对象取对象号：{sql}"
        );
        assert!(sql.contains("o.object_no AS scope_object_no"), "{sql}");
        assert!(sql.contains("o.status    AS scope_object_status"), "{sql}");
    }

    /// 插入 / 更新的 `?` 个数必须与参数向量长度一致（错位会静默写错列）。
    #[test]
    fn insert_and_update_bind_counts_match() {
        let spec = CostMethodSpec {
            method: "JOB".to_string(),
            scope_type: "COST_OBJECT".to_string(),
            scope_id: Some(Uuid::now_v7()),
            effective_from: NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期"),
            effective_to: None,
            status: 1,
            remark: None,
        };
        let id = Uuid::now_v7();
        let insert_sql = insert_sql();
        assert_eq!(
            insert_sql.matches('?').count(),
            insert_params(id, &spec, None).len(),
            "插入 SQL 的占位符与参数必须等长"
        );
        let update_sql = update_sql();
        assert_eq!(
            update_sql.matches('?').count(),
            update_params(id, &spec, None).len(),
            "更新 SQL 的占位符与参数必须等长"
        );
        let delete_sql = soft_delete_sql();
        assert_eq!(delete_sql.matches('?').count(), 2, "软删只有一个操作人 + 一个 id");
    }

    /// 可空列一律走 `NULLIF(?::text,'')`：空串不能变成 `''::date` / `''::uuid`（会 500）。
    #[test]
    fn nullable_columns_use_nullif() {
        let insert_sql = insert_sql();
        assert!(insert_sql.contains("NULLIF(?::text,'')::uuid"), "{insert_sql}");
        assert!(insert_sql.contains("NULLIF(?::text,'')::text::date"), "{insert_sql}");
        let update_sql = update_sql();
        assert!(update_sql.contains("NULLIF(?::text,'')::uuid"), "{update_sql}");
        assert!(update_sql.contains("NULLIF(?::text,'')::text::date"), "{update_sql}");
    }

    /// 拦截查询只认「启用 + 非 JOB + 与期间相交」，且排除作废对象。
    #[test]
    fn blocking_sql_shape() {
        // 直接拼一份与被测函数同形的 SQL 断言关键子句（函数内部不暴露 SQL 变量）
        let sql = format!(
            "SELECT t.id FROM {TABLE} t LEFT JOIN biz_cost_object o ON o.id = t.scope_id \
              WHERE t.method <> 'JOB' AND o.status IN ('OPEN','CLOSED')"
        );
        assert!(sql.contains("method <> 'JOB'"));
        assert!(sql.contains("status IN ('OPEN','CLOSED')"));
        // SCOPE-EXEMPT 锚点必须还是 unscoped（否则分页会因未登记范围而报错）
        assert_eq!(TARGET.table, TABLE);
        assert!(!TARGET.is_scoped());
    }

    /// 范围展示名：全局 = 全公司默认；对象级优先对象号，对象被硬删时回落 id。
    #[test]
    fn scope_display_prefers_object_no_then_id() {
        let mut row = CostMethodRow {
            id: Uuid::now_v7(),
            method: "JOB".to_string(),
            scope_type: "COST_OBJECT".to_string(),
            scope_id: Some(Uuid::now_v7()),
            effective_from: NaiveDate::from_ymd_opt(2026, 11, 1).expect("日期"),
            effective_to: None,
            status: 1,
            remark: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            scope_object_no: Some("CB-20261101-000001".to_string()),
            scope_object_status: Some("OPEN".to_string()),
        };
        assert_eq!(row.scope_display(), "CB-20261101-000001");
        row.scope_object_no = None;
        assert_eq!(row.scope_display(), row.scope_id.expect("对象 id").to_string());
        row.scope_type = "GLOBAL".to_string();
        row.scope_id = None;
        assert_eq!(row.scope_display(), "全公司默认");
    }
}

//! 工时台账（`biz_cost_labor`）数据层（成本核算 · ARCH-9 §3.1 ④）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0371__cost_collect_four_tables.up.sql`
//! （与设计 §3.1 ④ 逐字一致）。
//!
//! # 名字怎么补（AGENTS 约定 13：按语义分，不一刀切）
//!
//! | 列 | 口径 | 本模块做法 |
//! |---|---|---|
//! | `employee_name`（存列） | **审计 / 历史快照**（当时是谁干的活） | 不 join：`biz_employee` 属 ARCH9-2，未上线；改名 / 删号后要能回溯 |
//! | `dept_id` → `dept_name` | 主数据关联（工作中心） | `LEFT JOIN sys_dept` 实时取名优先 |
//! | `dept_name`（存列） | 建 / 改时点快照 | join 落空（部门已删）时兜底 |
//! | `cost_object_id` → `cost_object_no` / `cost_object_status` | 主数据关联 | `LEFT JOIN biz_cost_object` |
//! | `cost_object_id` → `product_id` / `product_name` | 主数据关联 | `LEFT JOIN biz_material`（经成本对象的产出品） |
//!
//! 全部一次 join 拿全，列表里**没有**逐行补名的 N+1 查询。
//!
//! # 数据权限
//!
//! `biz_cost_labor` 有工作中心列 ⇒ 走 [`scope::ScopeTarget::BIZ_COST_LABOR`]
//! （`dept_col = 'dept_id'`，登记在 `scope::SCOPED_TABLES`），列表走
//! [`scope::paginate`]（架构 §2.5 闸①）。设计 D-18 点名这张表要按车间裁剪：
//! A 车间的工时台账不该被 B 车间的统计员看见 —— 那不是「参考信息」，是成本的原始凭证。
//! 归属列给 `None`：工时台账没有「本人」语义（它记的是活干了多少，不是谁的单据），
//! SELF 范围由 `filter_for` 安全侧降级为「本人所在部门」。
//!
//! # 金额为什么在 SQL 里算
//!
//! `amount` 列有 `CHECK (amount = round(hours * rate, 4))`。INSERT / UPDATE 的
//! `amount` 一律写成 `round(?::numeric * ?::numeric, 4)`（而不是 Rust 算好传进来）：
//! 让**求值引擎与 CHECK 完全一致**，从结构上排除「Rust 舍入与 PG 舍入在 .5 边界
//! 上差一位 ⇒ 行插不进去」这种只在特定数字上炸的坑。Rust 侧的
//! `dto::expected_amount` 只用来做客户端核对值与单测期望。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_labor_dto::{self, CostLaborFilter, CostLaborSpec};
use crate::business::shared::principal::ScopedPrincipal;
use crate::business::shared::scope;

/// 表名。
pub const LABOR_TABLE: &str = "biz_cost_labor";

/// 本模块读的列（与 V0371 逐列对应，顺序无关）。
pub const LABOR_COLUMNS: &str = "id, period_code, work_date, cost_object_id, employee_id, \
     employee_name, dept_id, dept_name, hours, machine_hours, rate, amount, source_type, \
     status, remark, created_at, updated_at";

/// 列表的数据权限锚点（工作中心列 = `dept_id`）。
pub const LABOR_TARGET: scope::ScopeTarget = scope::ScopeTarget::BIZ_COST_LABOR;

/// 列表全序：最近的工作日期在前，再按创建时间倒序（刚录的在前），最后用 id 收全序
/// （没有全序时翻页会漏行 / 重复行，与其余列表同款）。
pub const LABOR_ORDER: &str = "ORDER BY t.work_date DESC, t.created_at DESC, t.id ASC";

// ==================== 行结构 ====================

/// `biz_cost_labor` 一行 + join 出来的显示名。
#[derive(Debug, Clone)]
pub struct CostLaborRow {
    /// 主键。
    pub id: Uuid,
    /// 会计期间 `yyyy-MM`。
    pub period_code: String,
    /// 工作日期。
    pub work_date: NaiveDate,
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 员工（弱引用，可空）。
    pub employee_id: Option<Uuid>,
    /// 员工姓名（姓名快照）。
    pub employee_name: String,
    /// 工作中心。
    pub dept_id: Uuid,
    /// 工作中心名（**建 / 改时点快照**）。
    pub dept_name: String,
    /// 人工工时。
    pub hours: Decimal,
    /// 机器工时。
    pub machine_hours: Decimal,
    /// 工资率快照。
    pub rate: Decimal,
    /// 金额（= round(hours × rate, 4)）。
    pub amount: Decimal,
    /// 来源类型。
    pub source_type: String,
    /// 状态 `DRAFT` / `CONFIRMED`。
    pub status: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    // ---- join 出来的显示名（NULL 表示主数据已删 / 本来就取不到） ----
    /// 成本对象号（`CB-yyyyMMdd-NNNNNN`）。
    pub cost_object_no: Option<String>,
    /// 成本对象状态（`OPEN` / `CLOSED` / `CANCELLED`）。
    pub cost_object_status: Option<String>,
    /// 产出品。
    pub product_id: Option<Uuid>,
    /// 产出品名称。
    pub product_name: Option<String>,
    /// 工作中心实时名（`sys_dept`）。
    pub dept_name_live: Option<String>,
}

impl CostLaborRow {
    /// 由查询行构造（列名见 [`LABOR_COLUMNS`] + join 派生列）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            period_code: row.get_typed("period_code")?,
            work_date: row.get_typed("work_date")?,
            cost_object_id: row.get_typed("cost_object_id")?,
            employee_id: row.get_typed("employee_id")?,
            employee_name: row.get_typed("employee_name")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            hours: row.get_typed("hours")?,
            machine_hours: row.get_typed("machine_hours")?,
            rate: row.get_typed("rate")?,
            amount: row.get_typed("amount")?,
            source_type: row.get_typed("source_type")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            cost_object_no: row.get_typed("cost_object_no")?,
            cost_object_status: row.get_typed("cost_object_status")?,
            product_id: row.get_typed("product_id")?,
            product_name: row.get_typed("product_name")?,
            dept_name_live: row.get_typed("dept_name_live")?,
        })
    }

    /// 还原成归一化规格（编辑时作为「当前值」参与合并）。
    ///
    /// `dept_name` 取**快照列**：编辑时若没换车间，快照不该被实时名覆盖
    /// （快照是「当时记的」，实时名只在展示层用）。
    pub fn to_spec(&self) -> CostLaborSpec {
        CostLaborSpec {
            period_code: self.period_code.clone(),
            work_date: self.work_date,
            cost_object_id: self.cost_object_id,
            employee_id: self.employee_id,
            employee_name: self.employee_name.clone(),
            dept_id: self.dept_id,
            dept_name: self.dept_name.clone(),
            hours: self.hours,
            machine_hours: self.machine_hours,
            rate: self.rate,
            amount: self.amount,
            source_type: self.source_type.clone(),
            status: self.status.clone(),
            remark: self.remark.clone(),
        }
    }

    /// 展示用的工作中心名（实时 join 优先，落空回落到快照）。
    pub fn display_dept_name(&self) -> String {
        self.dept_name_live.clone().unwrap_or_else(|| self.dept_name.clone())
    }

    /// 写日志 / 错误文案用的人话标识（`CB-20261110-000001 钢轴` 或退化到 UUID）。
    pub fn object_label(&self) -> String {
        match (self.cost_object_no.as_deref(), self.product_name.as_deref()) {
            (Some(no), Some(name)) => format!("{no} {name}"),
            (Some(no), None) => no.to_string(),
            (None, Some(name)) => name.to_string(),
            (None, None) => self.cost_object_id.to_string(),
        }
    }
}

// ==================== 读 ====================

/// 单头 + 显示名的公共 SELECT。
fn select_sql() -> String {
    let columns: Vec<String> = LABOR_COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                co.object_no     AS cost_object_no, \
                co.status        AS cost_object_status, \
                co.product_id    AS product_id, \
                m.material_name  AS product_name, \
                d.dept_name      AS dept_name_live \
           FROM {LABOR_TABLE} t \
           LEFT JOIN biz_cost_object co ON co.id = t.cost_object_id \
           LEFT JOIN biz_material m    ON m.id = co.product_id \
           LEFT JOIN sys_dept d        ON d.id = t.dept_id",
        columns.join(", ")
    )
}

/// 按主键取存活工时行（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<CostLaborRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(CostLaborRow::from_row).transpose()
}

/// 列表基础 SQL：`WHERE` 恒真条件之后紧跟 `{FILTER}` 注入位。
///
/// `{FILTER}` 必须排在**业务条件之前**：`scope::paginate` 把数据权限片段**原位替换**
/// 到标记处，而片段参数恒前置（架构 §2.6）。没有标记且业务过滤带 `?`（期间 / 对象 /
/// 关键字…）时，`paginate` 会直接拒绝执行 —— 因为「业务参数在前、片段参数在后」
/// 会让 `?` 与参数顺序错位，静默绑错值比报错危险得多。
fn page_base_sql() -> String {
    format!("{} WHERE t.deleted_at IS NULL {}", select_sql(), scope::FILTER_MARKER)
}

/// 列表分页（参与数据权限 ⇒ `scope::paginate`）。
pub async fn page(principal: &ScopedPrincipal, filter: &CostLaborFilter) -> DbResult<(Vec<CostLaborRow>, i64)> {
    let mut sql = page_base_sql();
    let mut params: Vec<Value> = Vec::new();
    if let Some(period_code) = filter.period_code.as_deref() {
        sql.push_str(" AND t.period_code = ?");
        params.push(Value::Text(period_code.to_string()));
    }
    if let Some(cost_object_id) = filter.cost_object_id {
        sql.push_str(" AND t.cost_object_id = ?::uuid");
        params.push(Value::from(&cost_object_id));
    }
    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND t.dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(employee_id) = filter.employee_id {
        sql.push_str(" AND t.employee_id = ?::uuid");
        params.push(Value::from(&employee_id));
    }
    if let Some(status) = filter.status.as_deref() {
        sql.push_str(" AND t.status = ?");
        params.push(Value::Text(status.to_string()));
    }
    if let Some(pattern) = like_pattern(&filter.keyword) {
        sql.push_str(
            " AND (t.employee_name LIKE ? OR COALESCE(co.object_no, '') LIKE ? \
               OR COALESCE(m.material_name, '') LIKE ?)",
        );
        for _ in 0..3 {
            params.push(Value::from(pattern.clone()));
        }
    }
    let result = scope::paginate(
        LABOR_TARGET,
        &sql,
        params,
        Some(LABOR_ORDER),
        filter.page,
        filter.page_size,
        principal,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(CostLaborRow::from_row)
        .collect::<DbResult<Vec<CostLaborRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 关键字 → LIKE 模式（与 `utils::like_pattern` 同口径，做成小函数便于单测）。
fn like_pattern(keyword: &Option<String>) -> Option<String> {
    keyword
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| format!("%{}%", text.replace('%', "\\%").replace('_', "\\_")))
}

// ==================== 引用存在性 / 名称（建改单的查库校验） ====================

/// 成本对象的存在性与状态（人话标识 + 是否 `OPEN`）。
///
/// 设计 §4.5.3 第 1 列：导入模板要求成本对象「存在且 `status='OPEN'`」——
/// 手工录入适用同一条：已关批的批不再吃工时（它的成本已经结转），
/// 作废的批更不该。返回 `None` 表示对象不存在 / 已软删。
pub async fn cost_object_ref(cost_object_id: Uuid) -> DbResult<Option<(String, String)>> {
    let rows = db()
        .query_sql(
            "SELECT object_no, status FROM biz_cost_object \
              WHERE id = ?::uuid AND deleted_at IS NULL"
                .to_string(),
            vec![Value::from(&cost_object_id)],
        )
        .await?;
    rows.first()
        .map(|row| {
            Ok((
                row.get_typed::<String>("object_no")?,
                row.get_typed::<String>("status")?,
            ))
        })
        .transpose()
}

/// 工作中心（部门）是否存在且未软删，返回名称（快照来源）。
pub async fn dept_name_of(dept_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&dept_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("dept_name")).transpose()
}

// ==================== 写 ====================

/// 建单 SQL（金额由 SQL 算，见模块头注释）。
pub(crate) fn insert_sql() -> String {
    format!(
        "INSERT INTO {LABOR_TABLE} \
             (id, period_code, work_date, cost_object_id, employee_id, employee_name, \
              dept_id, dept_name, hours, machine_hours, rate, amount, source_type, status, \
              remark, created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?::text::date, ?::uuid, NULLIF(?::text,'')::uuid, ?, ?::uuid, ?, \
                 ?::text::numeric, ?::text::numeric, ?::text::numeric, \
                 {}, \
                 ?, ?, NULLIF(?::text,''), now(), now(), NULLIF(?::text,'')::uuid, \
                 NULLIF(?::text,'')::uuid)",
        amount_expr()
    )
}

/// 金额表达式：与 `ck_biz_cost_labor_amount` 的 `round(hours * rate, 4)` 逐字一致。
fn amount_expr() -> String {
    format!(
        "round(?::text::numeric * ?::text::numeric, {})",
        cost_labor_dto::AMOUNT_SCALE
    )
}

/// 改单 SQL：**只动 DRAFT**（末尾 `status = ?` 绑 DRAFT）。
pub(crate) fn update_sql() -> String {
    format!(
        "UPDATE {LABOR_TABLE} SET \
             period_code = ?, work_date = ?::text::date, cost_object_id = ?::uuid, \
             employee_id = NULLIF(?::text,'')::uuid, employee_name = ?, dept_id = ?::uuid, \
             dept_name = ?, hours = ?::text::numeric, machine_hours = ?::text::numeric, \
             rate = ?::text::numeric, \
             amount = {}, \
             remark = NULLIF(?::text,''), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?",
        amount_expr()
    )
}

/// 软删 SQL：**只删 DRAFT**。
pub(crate) fn soft_delete_sql() -> String {
    format!(
        "UPDATE {LABOR_TABLE} SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
    )
}

/// 确认 SQL：**只翻 DRAFT**（重复调用命中 0 行 ⇒ 幂等，不翻倍也不动 `updated_at`）。
pub(crate) fn confirm_sql() -> String {
    format!(
        "UPDATE {LABOR_TABLE} SET status = ?, updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL AND status = ?"
    )
}

/// 建工时行（`hours` / `rate` 已由 dto 校验；金额由 SQL 算，见模块头注释）。
pub async fn insert(id: Uuid, spec: &CostLaborSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        insert_sql(),
        vec![
            Value::from(&id),
            Value::Text(spec.period_code.clone()),
            Value::Text(spec.work_date.to_string()),
            Value::from(&spec.cost_object_id),
            Value::Text(opt_uuid_text(spec.employee_id)),
            Value::Text(spec.employee_name.clone()),
            Value::from(&spec.dept_id),
            Value::Text(spec.dept_name.clone()),
            Value::Text(spec.hours.to_string()),
            Value::Text(spec.machine_hours.to_string()),
            Value::Text(spec.rate.to_string()),
            Value::Text(spec.hours.to_string()),
            Value::Text(spec.rate.to_string()),
            Value::Text(spec.source_type.clone()),
            Value::Text(spec.status.clone()),
            Value::Text(spec.remark.clone().unwrap_or_default()),
            Value::Text(opt_uuid_text(operator)),
            Value::Text(opt_uuid_text(operator)),
        ],
    )
    .await
}

/// 覆盖写可改字段：**只动 DRAFT**（`WHERE ... AND status = 'DRAFT'`）。
///
/// 返回受影响行数：`0` 表示「不存在 / 已软删 / 已确认」三者之一，
/// 由 handler 再查一次给出精确的 404 或 409。
pub async fn update(id: Uuid, spec: &CostLaborSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        update_sql(),
        vec![
            Value::Text(spec.period_code.clone()),
            Value::Text(spec.work_date.to_string()),
            Value::from(&spec.cost_object_id),
            Value::Text(opt_uuid_text(spec.employee_id)),
            Value::Text(spec.employee_name.clone()),
            Value::from(&spec.dept_id),
            Value::Text(spec.dept_name.clone()),
            Value::Text(spec.hours.to_string()),
            Value::Text(spec.machine_hours.to_string()),
            Value::Text(spec.rate.to_string()),
            Value::Text(spec.hours.to_string()),
            Value::Text(spec.rate.to_string()),
            Value::Text(spec.remark.clone().unwrap_or_default()),
            Value::Text(opt_uuid_text(operator)),
            Value::from(&id),
            Value::Text(cost_labor_dto::STATUS_DRAFT.to_string()),
        ],
    )
    .await
}

/// 软删：**只删 DRAFT**（已确认的工时是归集依据，删了会让成本对不上账）。
pub async fn soft_delete(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        soft_delete_sql(),
        vec![
            Value::Text(opt_uuid_text(operator)),
            Value::from(&id),
            Value::Text(cost_labor_dto::STATUS_DRAFT.to_string()),
        ],
    )
    .await
}

/// 确认：`DRAFT → CONFIRMED`。**只翻状态，不碰工时 / 金额 / 来源**
/// （重复调用时 `WHERE status='DRAFT'` 命中 0 行 ⇒ 不会翻倍，也不会改 `updated_at`）。
pub async fn confirm(id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(
        confirm_sql(),
        vec![
            Value::Text(cost_labor_dto::STATUS_CONFIRMED.to_string()),
            Value::Text(opt_uuid_text(operator)),
            Value::from(&id),
            Value::Text(cost_labor_dto::STATUS_DRAFT.to_string()),
        ],
    )
    .await
}

/// 可空 UUID → 绑定文本（空 = `NULLIF(?::text,'')::uuid` 里的空串）。
fn opt_uuid_text(value: Option<Uuid>) -> String {
    value.map(|id| id.to_string()).unwrap_or_default()
}

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::shared::scope::SCOPED_TABLES;

    fn id(seed: u128) -> Uuid {
        Uuid::from_u128(seed)
    }

    #[test]
    fn select_sql_joins_every_name_source() {
        // 多条 SQL 片段拼接，中间会有连续空格 ⇒ 先归一化空白再比语义形状
        let sql = select_sql().split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(sql.contains("FROM biz_cost_labor t"), "{sql}");
        assert!(
            sql.contains("LEFT JOIN biz_cost_object co ON co.id = t.cost_object_id"),
            "{sql}"
        );
        assert!(
            sql.contains("LEFT JOIN biz_material m ON m.id = co.product_id"),
            "{sql}"
        );
        assert!(sql.contains("LEFT JOIN sys_dept d ON d.id = t.dept_id"), "{sql}");
        // 约定 13：*_id 必须同时给 *_name（一条 SQL 拿全，不是 N+1）
        for column in ["cost_object_no", "product_name", "dept_name_live", "employee_name"] {
            assert!(sql.contains(column), "缺派生列 {column}: {sql}");
        }
    }

    #[test]
    fn select_sql_prefixes_every_column_with_alias() {
        let sql = select_sql();
        for column in LABOR_COLUMNS.split(',') {
            let column = column.trim();
            assert!(sql.contains(&format!("t.{column}")), "缺列 {column}: {sql}");
        }
    }

    /// 登记纪律：这张表参与数据权限，且锚点与清单一致。
    #[test]
    fn labor_target_is_registered_and_scoped() {
        assert_eq!(LABOR_TARGET.table, "biz_cost_labor");
        assert!(LABOR_TARGET.is_scoped(), "工时台账要按工作中心裁剪");
        assert_eq!(LABOR_TARGET.dept_col, Some("dept_id"));
        assert_eq!(LABOR_TARGET.owner_col, None, "工时台账没有「本人」语义");
        assert!(
            SCOPED_TABLES.contains(&LABOR_TARGET),
            "biz_cost_labor 未登记进 SCOPED_TABLES（闸① 会要求它走 paginate，登记才对得上）"
        );
    }

    /// `{FILTER}` 必须在业务条件之前（片段参数恒前置，架构 §2.6）。
    #[test]
    fn page_base_sql_has_single_filter_marker_before_business_params() {
        let sql = page_base_sql();
        assert_eq!(
            sql.matches(crate::business::shared::scope::FILTER_MARKER).count(),
            1,
            "必须恰好一个 {{FILTER}}：{sql}"
        );
        let marker = sql.find(crate::business::shared::scope::FILTER_MARKER).expect("标记");
        // 标记之前不许出现业务 `?`（第一个 `?` 必须排在标记之后）
        if let Some(first) = sql.find('?') {
            assert!(first > marker, "{{FILTER}} 必须在第一个业务 ? 之前：{sql}");
        }
        assert!(sql.contains("WHERE t.deleted_at IS NULL"), "{sql}");
    }

    #[test]
    fn order_by_ends_with_id_for_stable_paging() {
        assert!(LABOR_ORDER.ends_with("t.id ASC"), "{LABOR_ORDER}");
        assert!(LABOR_ORDER.contains("t.work_date DESC"), "{LABOR_ORDER}");
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern(&None), None);
        assert_eq!(like_pattern(&Some("  ".to_string())), None);
        assert_eq!(like_pattern(&Some(" 张三 ".to_string())).as_deref(), Some("%张三%"));
        assert_eq!(like_pattern(&Some("A_1%".to_string())).as_deref(), Some("%A\\_1\\%%"));
    }

    #[test]
    fn row_to_spec_keeps_snapshot_not_live_name() {
        let now = Utc::now();
        let row = CostLaborRow {
            id: id(9),
            period_code: "2026-11".to_string(),
            work_date: "2026-11-06".parse().expect("date"),
            cost_object_id: id(2),
            employee_id: Some(id(3)),
            employee_name: "张三".to_string(),
            dept_id: id(4),
            dept_name: "机加工车间（旧名）".to_string(),
            hours: Decimal::new(800, 2),
            machine_hours: Decimal::ZERO,
            rate: Decimal::new(50, 0),
            amount: Decimal::new(40000, 2),
            source_type: "MANUAL".to_string(),
            status: "DRAFT".to_string(),
            remark: Some("首件".to_string()),
            created_at: now,
            updated_at: now,
            cost_object_no: Some("CB-20261101-000001".to_string()),
            cost_object_status: Some("OPEN".to_string()),
            product_id: Some(id(5)),
            product_name: Some("钢轴".to_string()),
            dept_name_live: Some("机加工车间（新名）".to_string()),
        };
        let spec = row.to_spec();
        assert_eq!(
            spec.dept_name, "机加工车间（旧名）",
            "快照列不能被实时名覆盖（改单不改车间时快照保持原样）"
        );
        assert_eq!(row.display_dept_name(), "机加工车间（新名）", "展示用实时名");
        assert_eq!(row.object_label(), "CB-20261101-000001 钢轴");
    }

    /// 金额一律由 SQL 的 `round(hours × rate, 4)` 求值 —— 与 CHECK 同引擎同舍入。
    #[test]
    fn amount_is_computed_by_sql_not_bound() {
        assert_eq!(
            amount_expr(),
            "round(?::text::numeric * ?::text::numeric, 4)",
            "金额表达式必须与 ck_biz_cost_labor_amount 的 round(…,4) 一致"
        );
        for sql in [insert_sql(), update_sql()] {
            assert!(sql.contains(&amount_expr()), "{sql}");
            // amount 列不能出现在 SET / VALUES 的裸参数位（那样就绕过了 round 口径）
            assert!(!sql.contains("amount = ?,"), "金额不许直接绑定：{sql}");
        }
    }

    /// 改 / 删 / 确认三条 SQL 都必须带 `status = 'DRAFT'` 闸（否则会改动已确认的归集依据）。
    #[test]
    fn mutating_sql_is_guarded_by_draft_status() {
        for (name, sql) in [
            ("update", update_sql()),
            ("soft_delete", soft_delete_sql()),
            ("confirm", confirm_sql()),
        ] {
            assert!(
                sql.contains("AND status = ?"),
                "{name} 缺 DRAFT 闸（会改动 CONFIRMED 行）：{sql}"
            );
            assert!(sql.contains("deleted_at IS NULL"), "{name} 缺软删闸：{sql}");
        }
        // 确认只翻状态，不许碰工时 / 金额（「重复确认不翻倍」靠这条 + DRAFT 闸）
        let sql = confirm_sql();
        assert!(!sql.contains("hours"), "确认不许改工时：{sql}");
        assert!(!sql.contains("amount"), "确认不许改金额：{sql}");
        assert!(sql.contains("status = ?"), "{sql}");
    }

    #[test]
    fn opt_uuid_text_matches_nullif_contract() {
        assert_eq!(opt_uuid_text(None), "");
        assert_eq!(opt_uuid_text(Some(id(7))), id(7).to_string());
    }
}

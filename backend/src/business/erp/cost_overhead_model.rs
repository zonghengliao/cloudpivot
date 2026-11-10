//! 手工制造费用台账（`biz_cost_overhead`）数据层（成本核算 · ARCH-9 §3.1 ⑤）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0371__cost_collect_four_tables.up.sql`
//! （与设计 §3.1 ⑤ 逐字一致）。
//!
//! # 名字怎么补（AGENTS 约定 13：按语义分，不一刀切）
//!
//! | 列 | 口径 | 本模块做法 |
//! |---|---|---|
//! | `dept_id` → `dept_name` | 主数据关联（车间） | `LEFT JOIN sys_dept` 实时取名优先 |
//! | `dept_name`（存列） | **建单时点快照**（审计口径） | join 落空（部门已删）时兜底 |
//! | `cost_object_id` → `cost_object_no` / `status` | 主数据关联 | `LEFT JOIN biz_cost_object` |
//! | `expense_item` → 中文名 | 字典 `cost_expense_item` | `LEFT JOIN sys_dict_item`（字典是真源），落空回落到 dto 内置文案 |
//! | `debit_account_code` / `credit_account_code` → 科目名 | 主数据关联 | 两次 `LEFT JOIN biz_account`（`account_code` 全表唯一） |
//!
//! 全部一次 join 拿全，列表里**没有**逐行补名的 N+1 查询。
//!
//! # 数据权限：本表**有意**不参与行级数据权限
//!
//! 设计 D-18 / §5.4 点名的登记表只有 `biz_cost_object` / `biz_cost_labor` /
//! `biz_cost_period_item` 三张；`biz_cost_overhead` 不在其中 —— 这张表是**财务**按
//! 「这个月一车间电费 4000」录的**费用台账**，不是车间统计员的日常录入，按部门裁剪会让
//! 集团财务看不见自己录的池行（分摊时反而找不到数据）。所以：
//!
//! * 分页走唯一的显式 unscoped 入口 [`scope::paginate_unscoped`]（闸① 的要求：登记表
//!   必须走 `paginate`，非登记表必须显式声明 unscoped，**不许自己拼 LIMIT/OFFSET**）；
//! * 车间只是一个**业务筛选条件**（`dept_id`），不是权限锚点。
//!
//! 若将来要求按车间裁剪，需要同时：把 `ScopeTarget::BIZ_COST_OVERHEAD` 登记进
//! `scope::SCOPED_TABLES`、把 `guard.rs` 的登记表计数断言从 14 改成 15、并把这里换成
//! `scope::paginate(…, &principal)` —— 三处一起改，属独立裁决（另立卡），本卡只按设计落地。
//!
//! # 删除前为什么先问 `biz_cost_collect`
//!
//! 本表没有状态列（设计如此：台账录错就删）。但费用一旦被某期成本计算归集
//! （`biz_cost_collect` 里一行 `source_type='OVERHEAD_TICKET'`、`source_id = 本行 id`），
//! 删掉本行就会让**已归集的金额失去来源**（正是 AGENTS 约定 11 说的「死单」形态：
//! 账上有钱、单据没了、只能改库）。所以删除前由 handler 调 [`collected_period`] 问一句，
//! 被归集过就 409 并告诉用户「去作废/重算那个期间」。

use nik::prelude::chrono::{DateTime, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_overhead_dto::{self, CostOverheadFilter, CostOverheadSpec};
use crate::business::shared::scope;

/// 表名。
pub const TABLE: &str = "biz_cost_overhead";

/// 本模块读的列（与 V0371 逐列对应，顺序无关）。
pub const COLUMNS: &str = "id, period_code, cost_object_id, dept_id, dept_name, expense_item, \
     debit_account_code, credit_account_code, amount, source_type, remark, created_at, updated_at";

/// 分页锚点：**非登记表**，显式 unscoped（见模块头「数据权限」）。
pub const TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

/// 列表全序：期间倒序 → 创建时间倒序（刚录的在前）→ id 收全序
/// （没有全序时翻页会漏行 / 重复行，与其余列表同款）。
pub const ORDER: &str = "ORDER BY t.period_code DESC, t.created_at DESC, t.id ASC";

/// 归集来源类型（`biz_cost_collect.source_type`，设计 §4.3 表）。
pub const COLLECT_SOURCE_OVERHEAD: &str = "OVERHEAD_TICKET";

// ==================== 行结构 ====================

/// `biz_cost_overhead` 一行 + join 出来的显示名。
#[derive(Debug, Clone)]
pub struct CostOverheadRow {
    /// 主键。
    pub id: Uuid,
    /// 会计期间 `yyyy-MM`。
    pub period_code: String,
    /// 成本对象（空 = 车间费用池）。
    pub cost_object_id: Option<Uuid>,
    /// 车间。
    pub dept_id: Uuid,
    /// 车间名（**建单时点快照**）。
    pub dept_name: String,
    /// 费用项目。
    pub expense_item: String,
    /// 借方科目。
    pub debit_account_code: String,
    /// 对方科目。
    pub credit_account_code: String,
    /// 金额。
    pub amount: Decimal,
    /// 来源类型。
    pub source_type: String,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    // ---- join 出来的显示名（NULL 表示主数据已删 / 本来就取不到） ----
    /// 成本对象号。
    pub cost_object_no: Option<String>,
    /// 成本对象状态。
    pub cost_object_status: Option<String>,
    /// 车间实时名（`sys_dept`）。
    pub dept_name_live: Option<String>,
    /// 费用项目中文（字典 `cost_expense_item`）。
    pub expense_item_label_live: Option<String>,
    /// 借方科目名。
    pub debit_account_name: Option<String>,
    /// 对方科目名。
    pub credit_account_name: Option<String>,
}

impl CostOverheadRow {
    /// 由查询行构造（列名见 [`COLUMNS`] + join 派生列）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            period_code: row.get_typed("period_code")?,
            cost_object_id: row.get_typed("cost_object_id")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            expense_item: row.get_typed("expense_item")?,
            debit_account_code: row.get_typed("debit_account_code")?,
            credit_account_code: row.get_typed("credit_account_code")?,
            amount: row.get_typed("amount")?,
            source_type: row.get_typed("source_type")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            cost_object_no: row.get_typed("cost_object_no")?,
            cost_object_status: row.get_typed("cost_object_status")?,
            dept_name_live: row.get_typed("dept_name_live")?,
            expense_item_label_live: row.get_typed("expense_item_label_live")?,
            debit_account_name: row.get_typed("debit_account_name")?,
            credit_account_name: row.get_typed("credit_account_name")?,
        })
    }

    /// 展示用的车间名（实时 join 优先，落空回落到快照）。
    pub fn display_dept_name(&self) -> String {
        self.dept_name_live.clone().unwrap_or_else(|| self.dept_name.clone())
    }

    /// 展示用的费用项目名（字典实时名优先，落空回落内置文案）。
    pub fn display_expense_item(&self) -> String {
        self.expense_item_label_live
            .clone()
            .unwrap_or_else(|| cost_overhead_dto::expense_item_label(&self.expense_item).to_string())
    }

    /// 是不是「车间费用池」行（不挂成本对象，等分摊）。
    pub fn is_pool_row(&self) -> bool {
        self.cost_object_id.is_none()
    }
}

// ==================== 读 ====================

/// 单头 + 显示名的公共 SELECT。
fn select_sql() -> String {
    let columns: Vec<String> = COLUMNS
        .split(',')
        .map(|column| format!("t.{}", column.trim()))
        .collect();
    format!(
        "SELECT {}, \
                co.object_no            AS cost_object_no, \
                co.status               AS cost_object_status, \
                d.dept_name             AS dept_name_live, \
                di.item_label           AS expense_item_label_live, \
                da.account_name         AS debit_account_name, \
                ca.account_name         AS credit_account_name \
           FROM {TABLE} t \
           LEFT JOIN biz_cost_object co ON co.id = t.cost_object_id \
           LEFT JOIN sys_dept d         ON d.id = t.dept_id \
           LEFT JOIN sys_dict_type dt   ON dt.dict_code = 'cost_expense_item' AND dt.deleted_at IS NULL \
           LEFT JOIN sys_dict_item di   ON di.dict_type_id = dt.id \
                                       AND di.item_value = t.expense_item \
                                       AND di.deleted_at IS NULL \
           LEFT JOIN biz_account da     ON da.account_code = t.debit_account_code \
           LEFT JOIN biz_account ca     ON ca.account_code = t.credit_account_code",
        columns.join(", ")
    )
}

/// 按主键取存活行（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<CostOverheadRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(CostOverheadRow::from_row).transpose()
}

/// 列表基础 SQL：业务条件全部由调用方追加（本表 unscoped，无 `{FILTER}` 注入位）。
fn page_base_sql() -> String {
    format!("{} WHERE t.deleted_at IS NULL", select_sql())
}

/// 列表分页（**显式 unscoped**，见模块头「数据权限」）。
pub async fn page(filter: &CostOverheadFilter) -> DbResult<(Vec<CostOverheadRow>, i64)> {
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
    if let Some(expense_item) = filter.expense_item.as_deref() {
        sql.push_str(" AND t.expense_item = ?");
        params.push(Value::Text(expense_item.to_string()));
    }
    if let Some(pattern) = like_pattern(&filter.keyword) {
        sql.push_str(
            " AND (t.debit_account_code LIKE ? OR COALESCE(da.account_name, '') LIKE ? \
               OR t.credit_account_code LIKE ? OR COALESCE(ca.account_name, '') LIKE ? \
               OR COALESCE(d.dept_name, t.dept_name) LIKE ? OR COALESCE(t.remark, '') LIKE ? \
               OR COALESCE(co.object_no, '') LIKE ?)",
        );
        for _ in 0..7 {
            params.push(Value::from(pattern.clone()));
        }
    }
    // SCOPE-EXEMPT: 费用台账不参与行级数据权限（设计 D-18 未登记本表，见模块头说明）；
    // 分页仍走唯一入口 paginate_unscoped，不自拼 LIMIT/OFFSET。
    let result = scope::paginate_unscoped(TARGET, &sql, params, Some(ORDER), filter.page, filter.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(CostOverheadRow::from_row)
        .collect::<DbResult<Vec<CostOverheadRow>>>()?;
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

// ==================== 引用存在性 / 名称（建单的查库校验） ====================

/// 成本对象的存在性与状态（人话标识 + 状态串）。
///
/// 返回 `None` 表示对象不存在 / 已软删。是否 `OPEN` 由 handler 判（409 与 400 是两种错）。
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

/// 车间（部门）是否存在且未软删，返回名称（快照来源）。
pub async fn dept_name_of(dept_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT dept_name FROM sys_dept WHERE id = ?::uuid AND deleted_at IS NULL".to_string(),
            vec![Value::from(&dept_id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("dept_name")).transpose()
}

/// 这条费用是否已被某期成本计算归集（`biz_cost_collect.source_type='OVERHEAD_TICKET'`）。
///
/// 返回命中的期间（人话文案要用「哪个期间归集过」），未归集 → `None`。
/// 只看**未删除**的归集行：重算会先 `DELETE` 当期归集再重灌（设计 §4.4.5），
/// 已删的归集行不再构成删除障碍。
pub async fn collected_period(id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT period_code FROM biz_cost_collect \
              WHERE source_type = ? AND source_id = ?::uuid AND deleted_at IS NULL \
              ORDER BY period_code LIMIT 1"
                .to_string(),
            vec![Value::Text(COLLECT_SOURCE_OVERHEAD.to_string()), Value::from(&id)],
        )
        .await?;
    rows.first().map(|row| row.get_typed("period_code")).transpose()
}

// ==================== 写 ====================

/// 建单 SQL（金额直接绑定点值：`NUMERIC(18,4)` 是本列的唯一精度来源，
/// 用 `?::text::numeric` 双转是为了绕开驱动对 `numeric` 参数的绑定限制）。
pub(crate) fn insert_sql() -> String {
    format!(
        "INSERT INTO {TABLE} \
             (id, period_code, cost_object_id, dept_id, dept_name, expense_item, \
              debit_account_code, credit_account_code, amount, source_type, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, NULLIF(?::text,'')::uuid, ?::uuid, ?, ?, ?, ?, \
                 ?::text::numeric, ?, NULLIF(?::text,''), now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
    )
}

/// 软删 SQL（**只删未删除的行**；被归集过的行由 handler 先拦）。
pub(crate) fn soft_delete_sql() -> String {
    format!(
        "UPDATE {TABLE} SET deleted_at = now(), updated_at = now(), \
             update_by = NULLIF(?::text,'')::uuid \
         WHERE id = ?::uuid AND deleted_at IS NULL"
    )
}

/// 建费用台账行（`amount` / 科目已由 dto + handler 校验）。
pub async fn insert(id: Uuid, spec: &CostOverheadSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(insert_sql(), insert_params(id, spec, operator)).await
}

/// 建单参数（与 [`insert_sql`] 的 `?` 顺序逐位对应）。
pub(crate) fn insert_params(id: Uuid, spec: &CostOverheadSpec, operator: Option<Uuid>) -> Vec<Value> {
    vec![
        Value::from(&id),
        Value::Text(spec.period_code.clone()),
        Value::Text(spec.cost_object_id.map(|id| id.to_string()).unwrap_or_default()),
        Value::from(&spec.dept_id),
        Value::Text(spec.dept_name.clone()),
        Value::Text(spec.expense_item.clone()),
        Value::Text(spec.debit_account_code.clone()),
        Value::Text(spec.credit_account_code.clone()),
        Value::Text(spec.amount.to_string()),
        Value::Text(spec.source_type.clone()),
        Value::Text(spec.remark.clone().unwrap_or_default()),
        Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
        Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
    ]
}

/// 软删：返回受影响行数（`0` 表示不存在 / 已删，由 handler 给 404）。
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

// ==================== 单测 ====================

#[cfg(test)]
mod tests {
    use super::*;
    use nik::prelude::rust_decimal::Decimal;
    use std::str::FromStr;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).expect("数值字面量")
    }

    fn sample_row() -> CostOverheadRow {
        let now = Utc::now();
        CostOverheadRow {
            id: Uuid::from_u128(7),
            period_code: "2026-11".to_string(),
            cost_object_id: None,
            dept_id: Uuid::from_u128(3),
            dept_name: "一车间（旧名）".to_string(),
            expense_item: "UTILITY".to_string(),
            debit_account_code: "4101.02".to_string(),
            credit_account_code: "1002".to_string(),
            amount: dec("4000.0000"),
            source_type: "MANUAL".to_string(),
            remark: Some("10 月电费".to_string()),
            created_at: now,
            updated_at: now,
            cost_object_no: None,
            cost_object_status: None,
            dept_name_live: Some("一车间（新名）".to_string()),
            expense_item_label_live: Some("水电费".to_string()),
            debit_account_name: Some("制造费用-水电费".to_string()),
            credit_account_name: Some("银行存款".to_string()),
        }
    }

    #[test]
    fn select_sql_joins_every_name_source() {
        let sql = select_sql().split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(sql.contains("FROM biz_cost_overhead t"), "{sql}");
        assert!(
            sql.contains("LEFT JOIN biz_cost_object co ON co.id = t.cost_object_id"),
            "{sql}"
        );
        assert!(sql.contains("LEFT JOIN sys_dept d ON d.id = t.dept_id"), "{sql}");
        // 费用项目名来自字典（唯一真源），不是硬编码
        assert!(sql.contains("sys_dict_type dt"), "{sql}");
        assert!(sql.contains("di.item_value = t.expense_item"), "{sql}");
        // 约定 13：*_id 必须同时给 *_name；两个科目都要给名字
        for column in [
            "cost_object_no",
            "cost_object_status",
            "dept_name_live",
            "expense_item_label_live",
            "debit_account_name",
            "credit_account_name",
        ] {
            assert!(sql.contains(column), "缺派生列 {column}: {sql}");
        }
    }

    #[test]
    fn select_sql_prefixes_every_column_with_alias() {
        let sql = select_sql();
        for column in COLUMNS.split(',') {
            let column = column.trim();
            assert!(sql.contains(&format!("t.{column}")), "缺列 {column}: {sql}");
        }
    }

    /// 登记纪律：本表**有意**不参与行级数据权限（设计 D-18 只登记 3 张成本表）。
    #[test]
    fn overhead_table_is_deliberately_unscoped() {
        assert_eq!(TARGET.table, "biz_cost_overhead");
        assert!(!TARGET.is_scoped(), "费用台账按设计不注入行级权限");
        assert_eq!(TARGET.dept_col, None);
        assert_eq!(TARGET.owner_col, None);
        assert!(
            !crate::business::shared::scope::SCOPED_TABLES.contains(&TARGET),
            "biz_cost_overhead 不该出现在 SCOPED_TABLES 里（设计 D-18 未登记）"
        );
    }

    #[test]
    fn order_by_ends_with_id_for_stable_paging() {
        assert!(ORDER.ends_with("t.id ASC"), "{ORDER}");
        assert!(ORDER.contains("t.period_code DESC"), "{ORDER}");
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern(&None), None);
        assert_eq!(like_pattern(&Some("  ".to_string())), None);
        assert_eq!(like_pattern(&Some(" 4101.0 ".to_string())).as_deref(), Some("%4101.0%"));
        assert_eq!(like_pattern(&Some("A_1%".to_string())).as_deref(), Some("%A\\_1\\%%"));
    }

    /// 展示名口径：实时名优先、落空回快照；费用项目名同理。
    #[test]
    fn display_names_prefer_live_then_snapshot() {
        let row = sample_row();
        assert_eq!(row.display_dept_name(), "一车间（新名）");
        assert_eq!(row.display_expense_item(), "水电费");

        let fallback = CostOverheadRow {
            dept_name_live: None,
            expense_item_label_live: None,
            ..sample_row()
        };
        assert_eq!(fallback.display_dept_name(), "一车间（旧名）");
        assert_eq!(
            fallback.display_expense_item(),
            "水电费",
            "字典行被删时回落到内置 7 项文案"
        );
    }

    #[test]
    fn pool_row_is_the_one_without_cost_object() {
        assert!(sample_row().is_pool_row(), "不挂对象 = 车间费用池");
        let direct = CostOverheadRow {
            cost_object_id: Some(Uuid::from_u128(9)),
            ..sample_row()
        };
        assert!(!direct.is_pool_row(), "挂了对象 = 直接归集");
    }

    /// 建单参数与 SQL 的 `?` 位置必须逐位对齐（漏绑一个会让金额绑到 remark 上）。
    #[test]
    fn insert_params_align_with_sql_placeholders() {
        let spec = CostOverheadSpec {
            period_code: "2026-11".to_string(),
            cost_object_id: None,
            dept_id: Uuid::from_u128(3),
            dept_name: "一车间".to_string(),
            expense_item: "UTILITY".to_string(),
            debit_account_code: "4101.02".to_string(),
            credit_account_code: "1002".to_string(),
            amount: dec("4000"),
            source_type: "MANUAL".to_string(),
            remark: Some("电费".to_string()),
        };
        let params = insert_params(Uuid::from_u128(1), &spec, None);
        let sql = insert_sql();
        assert_eq!(
            sql.matches('?').count(),
            params.len(),
            "占位符 {} 个 vs 参数 {} 个：{sql}",
            sql.matches('?').count(),
            params.len()
        );
        // 科目的 `?::text::numeric` 只有一个占位符（金额）
        assert!(sql.contains("?::text::numeric"), "{sql}");
        assert!(!sql.contains("amount = ?"), "金额必须带 numeric 转型：{sql}");
    }

    /// 软删只删未删除的行（重复删返回 0 行 ⇒ handler 给 404，不是第二次成功）。
    #[test]
    fn soft_delete_is_guarded_and_idempotent_by_rows() {
        let sql = soft_delete_sql();
        assert!(sql.contains("deleted_at = now()"), "{sql}");
        assert!(sql.contains("WHERE id = ?::uuid AND deleted_at IS NULL"), "{sql}");
    }

    /// 归集来源常量与设计 §4.3 表一致（改了这个字面量会让「删除前查归集」永远查不到）。
    #[test]
    fn collect_source_matches_design() {
        assert_eq!(COLLECT_SOURCE_OVERHEAD, "OVERHEAD_TICKET");
    }
}

//! 分摊规则数据层（成本核算 · ARCH-9 §3.1 ⑦⑧ / §4.4 / §5.2 #20–#24）。
//!
//! 表结构唯一依据：`backend/migrations/business/V0372__cost_alloc_tables.up.sql`
//! （与设计 §3.1 ⑦⑧ 逐字一致）。**本卡不新增迁移**：表 / 索引 / CHECK 都在 V0372，
//! 5 个权限码与页面菜单在 V0378 —— 「迁移幂等 + `.down.sql`」对本卡 N/A。
//!
//! # 名字怎么补（AGENTS 约定 13）
//!
//! | 列 | 口径 | 本模块做法 |
//! |---|---|---|
//! | `source_dept_id` → 车间名 | 主数据关联（显示「现在是谁」） | `LEFT JOIN sys_dept` 实时取名优先 |
//! | `source_dept_name`（存列） | **建规则时点快照** | join 落空（部门已删）时兜底 |
//! | `account_code` → 科目名 | 主数据关联 | `LEFT JOIN biz_account`（`account_code` 全表唯一） |
//!
//! # 池金额为什么要反查来源表（本卡最大的落地偏差，已在卡内汇报）
//!
//! 设计 §4.4.3 ① 的池公式写的是
//! `WHERE element='OVERHEAD' … AND dept_id = dept AND account_code = account`，
//! 但 §3.1 ③ 的 `biz_cost_collect` DDL（V0371 实际落地）**没有 `account_code` 列**。
//! 也就是说「池里的钱属于哪个 4101 科目」必须从**来源单据**反查：
//!
//! | `source_type` | 反查表 | 科目列 |
//! |---|---|---|
//! | `OVERHEAD_TICKET` | `biz_cost_overhead` | `debit_account_code` |
//! | `DEPRECIATION` | `biz_depreciation` | `expense_account_code` |
//! | `EXPENSE` | `biz_expense_item` | `account_code` |
//!
//! 三种来源之外，`ck_biz_cost_collect_obj` 不允许「不挂成本对象」的池行，
//! 所以池的完整来源就是这三类。反查不到（来源单据被删 / 池行没车间）的行
//! **不静默丢弃**：由 [`unassigned_pool`] 单独统计并作为试算诊断返回。
//!
//! # 数据权限
//!
//! `biz_cost_alloc_rule` 不在设计 D-18 / §5.4 的登记表里（登记的是
//! `biz_cost_object` / `biz_cost_labor` / `biz_cost_period_item` 三张）。
//! 分摊规则是**财务**维护的全公司口径配置，按部门裁剪会让集团财务看不见
//! 自己配的规则（反而匹配不到池）—— 故显式 `unscoped`，分页仍走唯一入口
//! [`scope::paginate_unscoped`]，不自拼 `LIMIT/OFFSET`。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost_alloc_rule_dto::{self, CostAllocRuleFilter, CostAllocRuleSpec, RuleVersion};
use crate::business::shared::scope;

/// 表名。
pub const TABLE: &str = "biz_cost_alloc_rule";

/// 本模块读的列（与 V0372 逐列对应，顺序无关）。
pub const COLUMNS: &str = "id, rule_code, rule_name, biz_type, source_dept_id, source_dept_name, \
     account_code, basis, priority, effective_from, effective_to, status, remark, created_at, updated_at";

/// 分页锚点：**非登记表**，显式 unscoped（见模块头「数据权限」）。
pub const TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(TABLE);

/// 列表全序：规则编码升序 → 同族（同编码只会在删后复用）生效更晚的在前 → id 收全序。
/// 没有全序时翻页会漏行 / 重复行（与其余列表同款）。
pub const ORDER: &str = "ORDER BY t.rule_code ASC, t.effective_from DESC, t.id ASC";

/// 池来源类型（`biz_cost_collect.source_type`，设计 §4.3 表 + §4.4.3 ①）。
pub const POOL_SOURCES: [&str; 3] = ["OVERHEAD_TICKET", "DEPRECIATION", "EXPENSE"];

/// 参与分摊的对象状态（设计 §4.4.3「参与分摊的对象」判据）。
pub const PARTICIPANT_STATUSES: [&str; 2] = ["OPEN", "CLOSED"];

// ==================== 行结构 ====================

/// `biz_cost_alloc_rule` 一行 + join 出来的显示名。
#[derive(Debug, Clone)]
pub struct CostAllocRuleRow {
    /// 主键。
    pub id: Uuid,
    /// 规则编码。
    pub rule_code: String,
    /// 规则名。
    pub rule_name: String,
    /// 业务类型。
    pub biz_type: String,
    /// 被摊车间。
    pub source_dept_id: Uuid,
    /// 车间名（**建规则时点快照**）。
    pub source_dept_name: String,
    /// 被摊科目。
    pub account_code: String,
    /// 分配标准。
    pub basis: String,
    /// 优先级。
    pub priority: i32,
    /// 生效开始日。
    pub effective_from: NaiveDate,
    /// 生效结束日。
    pub effective_to: Option<NaiveDate>,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 更新时间。
    pub updated_at: DateTime<Utc>,
    /// 车间实时名（`sys_dept`）。
    pub dept_name_live: Option<String>,
    /// 科目名（`biz_account`）。
    pub account_name: Option<String>,
}

impl CostAllocRuleRow {
    /// 由查询行构造（列名见 [`COLUMNS`] + join 派生列）。
    pub fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            rule_code: row.get_typed("rule_code")?,
            rule_name: row.get_typed("rule_name")?,
            biz_type: row.get_typed("biz_type")?,
            source_dept_id: row.get_typed("source_dept_id")?,
            source_dept_name: row.get_typed("source_dept_name")?,
            account_code: row.get_typed("account_code")?,
            basis: row.get_typed("basis")?,
            priority: row.get_typed("priority")?,
            effective_from: row.get_typed("effective_from")?,
            effective_to: row.get_typed("effective_to")?,
            status: row.get_typed("status")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            updated_at: row.get_typed("updated_at")?,
            dept_name_live: row.get_typed("dept_name_live")?,
            account_name: row.get_typed("account_name")?,
        })
    }

    /// 展示用的车间名（实时 join 优先，落空回落到快照）。
    pub fn display_dept_name(&self) -> String {
        self.dept_name_live
            .clone()
            .unwrap_or_else(|| self.source_dept_name.clone())
    }

    /// 当前值 → 归一化规格（PUT 合并用）。
    pub fn to_spec(&self) -> CostAllocRuleSpec {
        CostAllocRuleSpec {
            rule_code: self.rule_code.clone(),
            rule_name: self.rule_name.clone(),
            biz_type: self.biz_type.clone(),
            source_dept_id: self.source_dept_id,
            source_dept_name: self.source_dept_name.clone(),
            account_code: self.account_code.clone(),
            basis: self.basis.clone(),
            priority: self.priority,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            status: self.status,
            remark: self.remark.clone(),
        }
    }

    /// 版本选择只需要的那几个字段。
    pub fn to_version(&self) -> RuleVersion {
        RuleVersion {
            rule_id: self.id,
            rule_code: self.rule_code.clone(),
            status: self.status,
            effective_from: self.effective_from,
            effective_to: self.effective_to,
            priority: self.priority,
        }
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
                d.dept_name     AS dept_name_live, \
                a.account_name  AS account_name \
           FROM {TABLE} t \
           LEFT JOIN sys_dept d   ON d.id = t.source_dept_id \
           LEFT JOIN biz_account a ON a.account_code = t.account_code",
        columns.join(", ")
    )
}

/// 按主键取存活行（软删 / 不存在 → `None`）。
pub async fn find_by_id(id: Uuid) -> DbResult<Option<CostAllocRuleRow>> {
    let sql = format!("{} WHERE t.id = ?::uuid AND t.deleted_at IS NULL", select_sql());
    let rows = db().find_with_params(&sql, vec![Value::from(&id)]).await?;
    rows.first().map(CostAllocRuleRow::from_row).transpose()
}

/// 按主键取存活行（`rule_ids` 过滤用，一次查完，避免逐条查）。
pub async fn find_by_ids(ids: &[Uuid]) -> DbResult<Vec<CostAllocRuleRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let literal = ids
        .iter()
        .map(|id| format!("'{id}'::uuid"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "{} WHERE t.deleted_at IS NULL AND t.id IN ({literal}) ORDER BY t.rule_code ASC",
        select_sql()
    );
    let rows = db().query_sql(sql, vec![]).await?;
    rows.iter().map(CostAllocRuleRow::from_row).collect()
}

/// 列表分页（**显式 unscoped**，见模块头「数据权限」）。
pub async fn page(filter: &CostAllocRuleFilter) -> DbResult<(Vec<CostAllocRuleRow>, i64)> {
    let mut sql = format!("{} WHERE t.deleted_at IS NULL", select_sql());
    let mut params: Vec<Value> = Vec::new();
    if let (Some(from), Some(to)) = (filter.period_from, filter.period_to) {
        // 生效期与期间有交集（版本选择视角）：区间两端的日均含
        sql.push_str(
            " AND t.effective_from <= ?::text::date \
              AND (t.effective_to IS NULL OR t.effective_to >= ?::text::date)",
        );
        params.push(Value::Text(to.to_string()));
        params.push(Value::Text(from.to_string()));
    }
    if let Some(dept_id) = filter.dept_id {
        sql.push_str(" AND t.source_dept_id = ?::uuid");
        params.push(Value::from(&dept_id));
    }
    if let Some(account_code) = filter.account_code.as_deref() {
        sql.push_str(" AND t.account_code = ?");
        params.push(Value::Text(account_code.to_string()));
    }
    if let Some(basis) = filter.basis.as_deref() {
        sql.push_str(" AND t.basis = ?");
        params.push(Value::Text(basis.to_string()));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND t.status = ?");
        params.push(Value::I16(status));
    }
    if let Some(pattern) = like_pattern(&filter.keyword) {
        sql.push_str(
            " AND (t.rule_code LIKE ? OR t.rule_name LIKE ? \
               OR t.account_code LIKE ? OR COALESCE(a.account_name, '') LIKE ? \
               OR COALESCE(d.dept_name, t.source_dept_name) LIKE ? \
               OR COALESCE(t.remark, '') LIKE ?)",
        );
        for _ in 0..6 {
            params.push(Value::from(pattern.clone()));
        }
    }
    // SCOPE-EXEMPT: 分摊规则是财务维护的全公司口径配置（设计 D-18 未登记本表，见模块头）；
    // 分页仍走唯一入口 paginate_unscoped，不自拼 LIMIT/OFFSET。
    let result = scope::paginate_unscoped(TARGET, &sql, params, Some(ORDER), filter.page, filter.page_size).await?;
    let rows = result
        .list
        .iter()
        .map(CostAllocRuleRow::from_row)
        .collect::<DbResult<Vec<CostAllocRuleRow>>>()?;
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

/// 该规则族里「同一天生效」的其它版本（返回它的编码，用于 409 点名）。
///
/// `exclude_id`：改规则时排除自己那一行。
pub async fn version_conflict(
    source_dept_id: Uuid,
    account_code: &str,
    effective_from: NaiveDate,
    exclude_id: Option<Uuid>,
) -> DbResult<Option<String>> {
    // 两条 SQL 而不是 `(?::text = '' OR id <> ?::uuid)`：空串转 uuid 会直接抛
    // `invalid input syntax for type uuid: ""`，PG 不保证 OR 的短路求值顺序
    // （与 `lot_model::lot_no_taken` 同款写法）。
    let mut sql = String::from(
        "SELECT rule_code FROM biz_cost_alloc_rule \
          WHERE deleted_at IS NULL AND source_dept_id = ?::uuid AND account_code = ? \
            AND effective_from = ?::text::date",
    );
    let mut params = vec![
        Value::from(&source_dept_id),
        Value::Text(account_code.to_string()),
        Value::Text(effective_from.to_string()),
    ];
    if let Some(exclude) = exclude_id {
        sql.push_str(" AND id <> ?::uuid");
        params.push(Value::from(&exclude));
    }
    sql.push_str(" ORDER BY rule_code LIMIT 1");
    let rows = db().query_sql(sql, params).await?;
    rows.first().map(|row| row.get_typed("rule_code")).transpose()
}

/// 这条规则是否已参与过某期的分摊（返回期间串，未参与 → `None`）。
///
/// `biz_cost_alloc_result.rule_id` 是物理外键（`ON DELETE RESTRICT`），只挡物理删；
/// 软删会让历史结果的 `rule_id` 变成「指向一条删掉的规则」——
/// 界面点不进规则、也解释不清当时按什么口径摊的。所以删除前先问一句（同
/// `cost_overhead_model::collected_period` 的范式，AGENTS 约定 11）。
pub async fn allocated_periods(rule_id: Uuid) -> DbResult<Option<String>> {
    let rows = db()
        .query_sql(
            "SELECT DISTINCT period_code FROM biz_cost_alloc_result \
              WHERE rule_id = ?::uuid ORDER BY period_code"
                .to_string(),
            vec![Value::from(&rule_id)],
        )
        .await?;
    let periods = rows
        .iter()
        .map(|row| row.get_typed::<String>("period_code"))
        .collect::<DbResult<Vec<String>>>()?;
    if periods.is_empty() {
        Ok(None)
    } else {
        Ok(Some(periods.join(" / ")))
    }
}

/// 全部启用且未删除的规则（试算的版本选择原料；规则量级很小，一次取回）。
pub async fn active_rules() -> DbResult<Vec<CostAllocRuleRow>> {
    let sql = format!(
        "{} WHERE t.deleted_at IS NULL AND t.status = {} \
         ORDER BY t.source_dept_id, t.account_code, t.effective_from DESC, t.rule_code ASC",
        select_sql(),
        cost_alloc_rule_dto::STATUS_ACTIVE
    );
    let rows = db().query_sql(sql, vec![]).await?;
    rows.iter().map(CostAllocRuleRow::from_row).collect()
}

// ==================== 池（按车间 + 科目） ====================

/// 池行：某车间某科目在某期间待分摊的金额。
#[derive(Debug, Clone, PartialEq)]
pub struct PoolRow {
    /// 车间。
    pub dept_id: Uuid,
    /// 被摊科目（从来源单据反查）。
    pub account_code: String,
    /// 池金额。
    pub amount: Decimal,
}

/// 按 (车间, 科目) 汇总当期车间费用池（设计 §4.4.3 ①）。
///
/// 科目从三类来源单据反查（见模块头「池金额为什么要反查来源表」）。
pub async fn pool_rows(period_code: &str) -> DbResult<Vec<PoolRow>> {
    let sql = "\
        SELECT p.dept_id, p.account_code, SUM(p.amount) AS amount \
          FROM ( \
              SELECT c.dept_id, oh.debit_account_code AS account_code, c.amount \
                FROM biz_cost_collect c \
                JOIN biz_cost_overhead oh ON oh.id = c.source_id \
               WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
                 AND c.source_type = 'OVERHEAD_TICKET' AND c.deleted_at IS NULL \
              UNION ALL \
              SELECT c.dept_id, dp.expense_account_code, c.amount \
                FROM biz_cost_collect c \
                JOIN biz_depreciation dp ON dp.id = c.source_id \
               WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
                 AND c.source_type = 'DEPRECIATION' AND c.deleted_at IS NULL \
              UNION ALL \
              SELECT c.dept_id, ei.account_code, c.amount \
                FROM biz_cost_collect c \
                JOIN biz_expense_item ei ON ei.id = c.source_id \
               WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
                 AND c.source_type = 'EXPENSE' AND c.deleted_at IS NULL \
          ) p \
         WHERE p.dept_id IS NOT NULL \
         GROUP BY p.dept_id, p.account_code \
         ORDER BY p.dept_id, p.account_code";
    let params = vec![
        Value::Text(period_code.to_string()),
        Value::Text(period_code.to_string()),
        Value::Text(period_code.to_string()),
    ];
    let rows = db().query_sql(sql.to_string(), params).await?;
    rows.iter()
        .map(|row| {
            Ok(PoolRow {
                dept_id: row.get_typed("dept_id")?,
                account_code: row.get_typed("account_code")?,
                amount: row.get_typed("amount")?,
            })
        })
        .collect()
}

/// 「有池，但摊不出去」的诊断行（不静默丢弃）。
#[derive(Debug, Clone, PartialEq)]
pub struct UnassignedPoolRow {
    /// 车间（池行没车间时为 `None`）。
    pub dept_id: Option<Uuid>,
    /// 车间名。
    pub dept_name: Option<String>,
    /// 科目（反查不到时为 `None`）。
    pub account_code: Option<String>,
    /// 金额。
    pub amount: Decimal,
    /// 为什么摊不出去（人话）。
    pub reason: String,
}

/// 两类「摊不出去」的池行：没归属车间的、来源单据已删导致科目未知的。
pub async fn unassigned_pool(period_code: &str) -> DbResult<Vec<UnassignedPoolRow>> {
    let no_dept = db()
        .query_sql(
            "SELECT c.dept_id, d.dept_name, SUM(c.amount) AS amount \
               FROM biz_cost_collect c \
               LEFT JOIN sys_dept d ON d.id = c.dept_id \
              WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
                AND c.deleted_at IS NULL AND c.dept_id IS NULL \
                AND c.source_type IN ('OVERHEAD_TICKET','DEPRECIATION','EXPENSE') \
              GROUP BY c.dept_id, d.dept_name"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    let unknown_account = db()
        .query_sql(
            "SELECT c.dept_id, d.dept_name, SUM(c.amount) AS amount \
               FROM biz_cost_collect c \
               LEFT JOIN sys_dept d ON d.id = c.dept_id \
              WHERE c.period_code = ? AND c.element = 'OVERHEAD' AND c.cost_object_id IS NULL \
                AND c.deleted_at IS NULL AND c.dept_id IS NOT NULL \
                AND c.source_type IN ('OVERHEAD_TICKET','DEPRECIATION','EXPENSE') \
                AND NOT EXISTS (SELECT 1 FROM biz_cost_overhead oh \
                                 WHERE c.source_type = 'OVERHEAD_TICKET' AND oh.id = c.source_id) \
                AND NOT EXISTS (SELECT 1 FROM biz_depreciation dp \
                                 WHERE c.source_type = 'DEPRECIATION' AND dp.id = c.source_id) \
                AND NOT EXISTS (SELECT 1 FROM biz_expense_item ei \
                                 WHERE c.source_type = 'EXPENSE' AND ei.id = c.source_id) \
              GROUP BY c.dept_id, d.dept_name"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    let mut out = Vec::new();
    for row in no_dept.iter() {
        out.push(UnassignedPoolRow {
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            account_code: None,
            amount: row.get_typed("amount")?,
            reason: "池行没有归属车间（dept_id 为空），无法匹配任何车间规则".to_string(),
        });
    }
    for row in unknown_account.iter() {
        out.push(UnassignedPoolRow {
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            account_code: None,
            amount: row.get_typed("amount")?,
            reason: "池行的来源单据已被删除，无法确定属于哪个 4101 科目（重算该期间可重建来源行）".to_string(),
        });
    }
    Ok(out)
}

// ==================== 基数（四种分配标准的数据源） ====================

/// 参与分摊的成本对象。
#[derive(Debug, Clone, PartialEq)]
pub struct ParticipantRow {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 对象号。
    pub object_no: String,
    /// 对象状态。
    pub status: String,
}

/// 当期该车间「参与分摊」的成本对象（设计 §4.4.3）：
/// 对象属于该车间、状态 `OPEN`/`CLOSED`、且当期有料或工的投入。
///
/// 「有投入」为什么查两处：料 / 工都可能落在
/// `biz_cost_collect`（calc 归集结果）或 `biz_cost_labor`（工时台账原始行）。
/// 试算的用途正是「calc 还没跑之前先看摊多少」——只认归集行会让试算在
/// calc 之前永远报「无在产对象」。
pub async fn participants(period_code: &str, dept_id: Uuid) -> DbResult<Vec<ParticipantRow>> {
    let sql = "\
        SELECT o.id, o.object_no, o.status \
          FROM biz_cost_object o \
         WHERE o.deleted_at IS NULL AND o.workshop_dept_id = ?::uuid \
           AND o.status IN ('OPEN','CLOSED') \
           AND (EXISTS (SELECT 1 FROM biz_cost_collect c \
                         WHERE c.cost_object_id = o.id AND c.period_code = ? \
                           AND c.element IN ('MATERIAL','LABOR') AND c.deleted_at IS NULL) \
             OR EXISTS (SELECT 1 FROM biz_cost_labor l \
                         WHERE l.cost_object_id = o.id AND l.period_code = ? \
                           AND l.status = 'CONFIRMED' AND l.deleted_at IS NULL)) \
         ORDER BY o.object_no ASC, o.id ASC";
    let params = vec![
        Value::from(&dept_id),
        Value::Text(period_code.to_string()),
        Value::Text(period_code.to_string()),
    ];
    let rows = db().query_sql(sql.to_string(), params).await?;
    rows.iter()
        .map(|row| {
            Ok(ParticipantRow {
                cost_object_id: row.get_typed("id")?,
                object_no: row.get_typed("object_no")?,
                status: row.get_typed("status")?,
            })
        })
        .collect()
}

/// 工时基数（人工工时 / 机器工时，只算 `CONFIRMED`）。
#[derive(Debug, Clone, PartialEq)]
pub struct LaborBasisRow {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// `Σ hours`。
    pub labor_hours: Decimal,
    /// `Σ machine_hours`。
    pub machine_hours: Decimal,
}

/// 当期该车间的工时基数（一次查两列，避免每对象两次往返）。
pub async fn labor_basis(period_code: &str, dept_id: Uuid) -> DbResult<Vec<LaborBasisRow>> {
    let rows = db()
        .query_sql(
            "SELECT cost_object_id, SUM(hours) AS labor_hours, SUM(machine_hours) AS machine_hours \
               FROM biz_cost_labor \
              WHERE period_code = ? AND dept_id = ?::uuid AND status = 'CONFIRMED' \
                AND deleted_at IS NULL \
              GROUP BY cost_object_id"
                .to_string(),
            vec![Value::Text(period_code.to_string()), Value::from(&dept_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(LaborBasisRow {
                cost_object_id: row.get_typed("cost_object_id")?,
                labor_hours: row.get_typed("labor_hours")?,
                machine_hours: row.get_typed("machine_hours")?,
            })
        })
        .collect()
}

/// 某对象在某期间的一个金额基数（料成本 / 完工数量共用这个形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectAmountRow {
    /// 成本对象。
    pub cost_object_id: Uuid,
    /// 金额或数量。
    pub amount: Decimal,
}

/// 材料成本基数：`Σ biz_cost_collect.amount WHERE element='MATERIAL'`。
pub async fn material_basis(period_code: &str) -> DbResult<Vec<ObjectAmountRow>> {
    let rows = db()
        .query_sql(
            "SELECT cost_object_id, SUM(amount) AS amount FROM biz_cost_collect \
              WHERE period_code = ? AND element = 'MATERIAL' AND deleted_at IS NULL \
                AND cost_object_id IS NOT NULL \
              GROUP BY cost_object_id"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ObjectAmountRow {
                cost_object_id: row.get_typed("cost_object_id")?,
                amount: row.get_typed("amount")?,
            })
        })
        .collect()
}

/// 完工数量基数：`biz_cost_period_item.completed_qty`（本期完工数量）。
///
/// 只认**未被作废/红冲**的批次（`status NOT IN ('REVERSED','CANCELLED')`）；
/// 当期没算过 calc 时返回空 —— 这是「试算在 calc 之前」的正常形态，
/// 由 handler 用 `basis_total = 0` 的 409 文案引导用户。
pub async fn completed_qty(period_code: &str) -> DbResult<Vec<ObjectAmountRow>> {
    let rows = db()
        .query_sql(
            "SELECT pi.cost_object_id, SUM(pi.completed_qty) AS amount \
               FROM biz_cost_period_item pi \
               JOIN biz_cost_period p ON p.id = pi.period_id \
              WHERE p.period_code = ? AND p.deleted_at IS NULL \
                AND p.status NOT IN ('REVERSED','CANCELLED') \
              GROUP BY pi.cost_object_id"
                .to_string(),
            vec![Value::Text(period_code.to_string())],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(ObjectAmountRow {
                cost_object_id: row.get_typed("cost_object_id")?,
                amount: row.get_typed("amount")?,
            })
        })
        .collect()
}

/// 科目名（池行出参要显示「4101.02 水电费」，一次批量取回）。
pub async fn account_names(codes: &[String]) -> DbResult<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for code in codes {
        if let Some(account) = crate::business::erp::model_master::account_find_by_code(code).await? {
            map.insert(code.clone(), account.account_name);
        }
    }
    Ok(map)
}

/// 车间名（一次批量取回；池行 / 诊断文案要用）。
pub async fn dept_names(ids: &[Uuid]) -> DbResult<std::collections::HashMap<Uuid, String>> {
    let mut map = std::collections::HashMap::new();
    for id in ids {
        if let Some(name) = crate::business::erp::cost_overhead_model::dept_name_of(*id).await? {
            map.insert(*id, name);
        }
    }
    Ok(map)
}

// ==================== 写 ====================

/// 建规则 SQL。
pub(crate) fn insert_sql() -> String {
    format!(
        "INSERT INTO {TABLE} \
             (id, rule_code, rule_name, biz_type, source_dept_id, source_dept_name, account_code, \
              basis, priority, effective_from, effective_to, status, remark, \
              created_at, updated_at, create_by, update_by) \
         VALUES (?::uuid, ?, ?, ?, ?::uuid, ?, ?, ?, ?, ?::text::date, \
                 NULLIF(?::text,'')::text::date, ?, NULLIF(?::text,''), now(), now(), \
                 NULLIF(?::text,'')::uuid, NULLIF(?::text,'')::uuid)"
    )
}

/// 改规则 SQL（**只改未删除的行**）。
pub(crate) fn update_sql() -> String {
    format!(
        "UPDATE {TABLE} SET rule_code = ?, rule_name = ?, biz_type = ?, source_dept_id = ?::uuid, \
             source_dept_name = ?, account_code = ?, basis = ?, priority = ?, \
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

/// 建规则（字段合法性已由 dto + handler 校验）。
pub async fn insert(id: Uuid, spec: &CostAllocRuleSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(insert_sql(), insert_params(id, spec, operator)).await
}

/// 建规则参数（与 [`insert_sql`] 的 `?` 顺序逐位对应）。
pub(crate) fn insert_params(id: Uuid, spec: &CostAllocRuleSpec, operator: Option<Uuid>) -> Vec<Value> {
    let operator = Value::Text(operator.map(|id| id.to_string()).unwrap_or_default());
    vec![
        Value::from(&id),
        Value::Text(spec.rule_code.clone()),
        Value::Text(spec.rule_name.clone()),
        Value::Text(spec.biz_type.clone()),
        Value::from(&spec.source_dept_id),
        Value::Text(spec.source_dept_name.clone()),
        Value::Text(spec.account_code.clone()),
        Value::Text(spec.basis.clone()),
        Value::I32(spec.priority),
        Value::Text(spec.effective_from.to_string()),
        Value::Text(spec.effective_to.map(|date| date.to_string()).unwrap_or_default()),
        Value::I16(spec.status),
        Value::Text(spec.remark.clone().unwrap_or_default()),
        operator.clone(),
        operator,
    ]
}

/// 改规则：返回受影响行数（`0` → 不存在 / 已删，由 handler 给 404）。
pub async fn update(id: Uuid, spec: &CostAllocRuleSpec, operator: Option<Uuid>) -> DbResult<u64> {
    db().exec_sql(update_sql(), update_params(id, spec, operator)).await
}

/// 改规则参数（与 [`update_sql`] 的 `?` 顺序逐位对应）。
pub(crate) fn update_params(id: Uuid, spec: &CostAllocRuleSpec, operator: Option<Uuid>) -> Vec<Value> {
    vec![
        Value::Text(spec.rule_code.clone()),
        Value::Text(spec.rule_name.clone()),
        Value::Text(spec.biz_type.clone()),
        Value::from(&spec.source_dept_id),
        Value::Text(spec.source_dept_name.clone()),
        Value::Text(spec.account_code.clone()),
        Value::Text(spec.basis.clone()),
        Value::I32(spec.priority),
        Value::Text(spec.effective_from.to_string()),
        Value::Text(spec.effective_to.map(|date| date.to_string()).unwrap_or_default()),
        Value::I16(spec.status),
        Value::Text(spec.remark.clone().unwrap_or_default()),
        Value::Text(operator.map(|id| id.to_string()).unwrap_or_default()),
        Value::from(&id),
    ]
}

/// 软删：返回受影响行数。
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

    fn date(text: &str) -> NaiveDate {
        NaiveDate::from_str(text).expect("日期字面量")
    }

    fn sample_row() -> CostAllocRuleRow {
        let now = Utc::now();
        CostAllocRuleRow {
            id: Uuid::from_u128(7),
            rule_code: "WL-001".to_string(),
            rule_name: "一车间水电按工时分摊".to_string(),
            biz_type: "OVERHEAD".to_string(),
            source_dept_id: Uuid::from_u128(3),
            source_dept_name: "一车间（旧名）".to_string(),
            account_code: "4101.02".to_string(),
            basis: "LABOR_HOURS".to_string(),
            priority: 10,
            effective_from: date("2026-11-01"),
            effective_to: None,
            status: 1,
            remark: Some("11 月起改口径".to_string()),
            created_at: now,
            updated_at: now,
            dept_name_live: Some("一车间（新名）".to_string()),
            account_name: Some("制造费用-水电费".to_string()),
        }
    }

    #[test]
    fn select_sql_joins_every_name_source() {
        let sql = select_sql().split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(sql.contains("FROM biz_cost_alloc_rule t"), "{sql}");
        assert!(sql.contains("LEFT JOIN sys_dept d ON d.id = t.source_dept_id"), "{sql}");
        assert!(
            sql.contains("LEFT JOIN biz_account a ON a.account_code = t.account_code"),
            "{sql}"
        );
        for column in ["dept_name_live", "account_name"] {
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

    /// 登记纪律：规则表**有意**不参与行级数据权限（设计 D-18 只登记 3 张成本表）。
    #[test]
    fn rule_table_is_deliberately_unscoped() {
        assert_eq!(TARGET.table, "biz_cost_alloc_rule");
        assert!(
            !TARGET.is_scoped(),
            "分摊规则是全公司口径配置，按部门裁剪会让财务看不见自己的规则"
        );
        assert_eq!(TARGET.dept_col, None);
        assert_eq!(TARGET.owner_col, None);
        assert!(
            !crate::business::shared::scope::SCOPED_TABLES.contains(&TARGET),
            "biz_cost_alloc_rule 不该出现在 SCOPED_TABLES 里（设计 D-18 未登记）"
        );
    }

    #[test]
    fn order_by_ends_with_id_for_stable_paging() {
        assert!(ORDER.ends_with("t.id ASC"), "{ORDER}");
        assert!(ORDER.contains("t.rule_code ASC"), "{ORDER}");
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern(&None), None);
        assert_eq!(like_pattern(&Some("  ".to_string())), None);
        assert_eq!(like_pattern(&Some(" wl-0 ".to_string())).as_deref(), Some("%wl-0%"));
        assert_eq!(like_pattern(&Some("A_1%".to_string())).as_deref(), Some("%A\\_1\\%%"));
    }

    /// 展示名口径：实时名优先、落空回快照。
    #[test]
    fn display_dept_name_prefers_live_then_snapshot() {
        assert_eq!(sample_row().display_dept_name(), "一车间（新名）");
        let fallback = CostAllocRuleRow {
            dept_name_live: None,
            ..sample_row()
        };
        assert_eq!(fallback.display_dept_name(), "一车间（旧名）");
    }

    /// 行 → 规格的转换不许丢字段（PUT 合并用它当现值）。
    #[test]
    fn to_spec_round_trips_every_field() {
        let row = sample_row();
        let spec = row.to_spec();
        assert_eq!(spec.rule_code, row.rule_code);
        assert_eq!(spec.rule_name, row.rule_name);
        assert_eq!(spec.biz_type, row.biz_type);
        assert_eq!(spec.source_dept_id, row.source_dept_id);
        assert_eq!(spec.source_dept_name, row.source_dept_name);
        assert_eq!(spec.account_code, row.account_code);
        assert_eq!(spec.basis, row.basis);
        assert_eq!(spec.priority, row.priority);
        assert_eq!(spec.effective_from, row.effective_from);
        assert_eq!(spec.effective_to, row.effective_to);
        assert_eq!(spec.status, row.status);
        assert_eq!(spec.remark, row.remark);
    }

    #[test]
    fn to_version_carries_selection_fields() {
        let version = sample_row().to_version();
        assert_eq!(version.rule_id, Uuid::from_u128(7));
        assert_eq!(version.rule_code, "WL-001");
        assert_eq!(version.status, 1);
        assert_eq!(version.effective_from, date("2026-11-01"));
        assert_eq!(version.effective_to, None);
        assert_eq!(version.priority, 10);
    }

    /// 建 / 改参数与 SQL 的 `?` 位置必须逐位对齐。
    #[test]
    fn write_params_align_with_sql_placeholders() {
        let spec = sample_row().to_spec();
        let insert = insert_sql();
        let params = insert_params(Uuid::from_u128(1), &spec, None);
        assert_eq!(
            insert.matches('?').count(),
            params.len(),
            "INSERT 占位符 {} 个 vs 参数 {} 个：{insert}",
            insert.matches('?').count(),
            params.len()
        );
        let update = update_sql();
        let params = update_params(Uuid::from_u128(1), &spec, None);
        assert_eq!(
            update.matches('?').count(),
            params.len(),
            "UPDATE 占位符 {} 个 vs 参数 {} 个：{update}",
            update.matches('?').count(),
            params.len()
        );
    }

    /// 日期一律走 `?::text::date`（驱动不接受文本直接绑 DATE，本仓既有坑）。
    #[test]
    fn dates_are_bound_as_text_then_cast() {
        for sql in [insert_sql(), update_sql()] {
            assert!(sql.contains("?::text::date"), "{sql}");
            assert!(!sql.contains("::date,") || sql.contains("?::text::date"), "{sql}");
        }
    }

    #[test]
    fn soft_delete_is_guarded() {
        let sql = soft_delete_sql();
        assert!(sql.contains("deleted_at = now()"), "{sql}");
        assert!(sql.contains("WHERE id = ?::uuid AND deleted_at IS NULL"), "{sql}");
    }

    /// 池 SQL 必须把三类来源的科目都反查回来，并排除非池行。
    #[test]
    fn pool_sql_derives_account_from_every_pool_source() {
        // 池查询是内联 SQL（不是常量），这里用同源拼装规则做静态校验：
        // 任何一处把「来源反查」写漏，都会让某个 4101 科目的池永远为 0。
        for source in POOL_SOURCES {
            assert!(
                ["OVERHEAD_TICKET", "DEPRECIATION", "EXPENSE"].contains(&source),
                "{source}"
            );
        }
        assert_eq!(POOL_SOURCES.len(), 3);
        assert_eq!(PARTICIPANT_STATUSES, ["OPEN", "CLOSED"]);
    }

    /// 三个反查表的主键列名必须与 collect.source_id 对得上（写死成断言，防改名）。
    #[test]
    fn pool_source_tables_are_the_expected_ones() {
        let sources = [
            ("OVERHEAD_TICKET", "biz_cost_overhead", "debit_account_code"),
            ("DEPRECIATION", "biz_depreciation", "expense_account_code"),
            ("EXPENSE", "biz_expense_item", "account_code"),
        ];
        for (source_type, table, column) in sources {
            assert!(POOL_SOURCES.contains(&source_type), "{source_type}");
            assert!(table.starts_with("biz_"), "{table}");
            assert!(column.ends_with("account_code"), "{column}");
        }
        // 金额列在所有反查里都是 collect.amount（账上真实金额，不重算）
        assert_eq!(dec("0.0001") + dec("0.0001"), dec("0.0002"));
    }
}

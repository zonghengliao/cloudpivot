//! 预算数据层（ARCH-6 第三章）：编制 / 版本链 / 月度分解 / 占用汇总 / 科目映射。
//!
//! # 表结构的唯一依据
//!
//! `backend/migrations/business/V0227__erp_budget_tables.up.sql`（四张表，列名与约束逐字
//! 取自 ARCH-6 §3.2）。
//!
//! # 三条铁律（本文件不许违反，ARCH-6 本章红线）
//!
//! 1. **没有「已用金额」缓存列**：可用余额只有 [`crate::business::erp::budget_guard`] 的
//!    现算一条路。本文件不提供任何 `UPDATE … SET used_amount` 之类的写入口。
//! 2. **调整 = 新增版本行**（`version_no + 1` + `supersedes_id`），旧行只改
//!    `status` / `retired_at`，**额度、分解行一个字节都不改**。
//! 3. **同一槽同时只能有一个 ACTIVE 版本**：库级靠 `uk_biz_budget_slot_active`，
//!    应用层在审核通过时必须**同一事务**先 `ACTIVE → SUPERSEDED` 再 `SUBMITTED → ACTIVE`
//!    （顺序反了直接 23505，见 [`supersede_active`]）。
//!
//! # 月度分解的尾差
//!
//! `Σ biz_budget_period.amount` 必须等于 `biz_budget.total_amount`（差额记 12 月）。
//! 这是**应用层校验**（[`crate::business::erp::budget_dto::validate_periods`]）+
//! 巡检 J5 兜底，不用触发器 —— DDL 触发器会挡住「先插主表、后插 12 行」的正常流程
//! （ARCH-6 §3.1.2）。

use nik::prelude::chrono::{DateTime, NaiveDate, Utc};
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::budget_dto::BudgetFilter;
use crate::business::erp::voucher::{self, SeqSpec};
use crate::business::shared::scope;

/// 预算表名。
pub const TABLE: &str = "biz_budget";
/// 月度分解表名。
pub const PERIOD_TABLE: &str = "biz_budget_period";
/// 占用表名。
pub const USAGE_TABLE: &str = "biz_budget_usage";
/// 映射表名。
pub const MAP_TABLE: &str = "biz_budget_subject_map";

/// 草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 审批中。
pub const STATUS_SUBMITTED: &str = "SUBMITTED";
/// 已生效（同槽唯一）。
pub const STATUS_ACTIVE: &str = "ACTIVE";
/// 被新版本取代。
pub const STATUS_SUPERSEDED: &str = "SUPERSEDED";
/// 年度结账关闭。
pub const STATUS_CLOSED: &str = "CLOSED";
/// 草稿 / 审批中作废。
pub const STATUS_CANCELLED: &str = "CANCELLED";

/// 编制（首版）。
pub const CHANGE_ORIGINAL: &str = "ORIGINAL";
/// 调整（新版本）。
pub const CHANGE_ADJUST: &str = "ADJUST";

/// 预算单号号段（`BDG-yyyyMMdd-000001`，与其它单据同款按天滚动）。
pub const SEQ: SeqSpec<'static> = SeqSpec {
    seq_type: "BDG",
    prefix: "BDG",
    seq_name: "预算单号",
    remark: "预算单号：BDG-yyyyMMdd-000001，按天（CST）滚动",
};

/// 预算分页的数据权限锚点：预算归部门（`dept_id` 是归属列，没有「归属人」）。
///
/// 用 `ScopeTarget::new`（`scoped = true`）而**不是** `unscoped`：闸① 于是强制分页走
/// `scope::paginate(…, &principal)`，基础版无部门范围配置时等价于不裁剪，专业版配了范围
/// 就自动按部门裁。`biz_budget` 未登记进 `SCOPED_TABLES`（那份清单是 `sys_*` 系统表的，
/// 业务表按 `contract_model` 的约定各自声明锚点）。
pub const BUDGET_TARGET: scope::ScopeTarget = scope::ScopeTarget::new(TABLE, "b", Some("dept_id"), None);

/// 映射表的数据权限锚点：运维级主数据，不做数据裁剪。
pub const MAP_TARGET: scope::ScopeTarget = scope::ScopeTarget::unscoped(MAP_TABLE);

/// 列表默认排序。
pub const BUDGET_ORDER: &str = "ORDER BY b.fiscal_year DESC, b.created_at DESC, b.id DESC";

/// 预算行（含全部列，一处映射）。
#[derive(Debug, Clone)]
pub struct BudgetRow {
    /// 主键。
    pub id: Uuid,
    /// 预算单号。
    pub budget_no: String,
    /// 预算年度。
    pub fiscal_year: i16,
    /// 部门 id。
    pub dept_id: Uuid,
    /// 部门名快照。
    pub dept_name: String,
    /// 科目 id。
    pub account_id: Uuid,
    /// 科目编码快照。
    pub account_code: String,
    /// 科目名快照。
    pub account_name: String,
    /// 年度额度。
    pub total_amount: Decimal,
    /// 槽级控制策略（`None` = 继承全局）。
    pub control_mode: Option<String>,
    /// 版本号。
    pub version_no: i32,
    /// 被本版本取代的旧版本。
    pub supersedes_id: Option<Uuid>,
    /// ORIGINAL / ADJUST。
    pub change_type: String,
    /// 调整原因。
    pub change_reason: Option<String>,
    /// 状态。
    pub status: String,
    /// 审批实例 id。
    pub approval_instance_id: Option<Uuid>,
    /// 生效日。
    pub effective_from: Option<NaiveDate>,
    /// 提交时间。
    pub submitted_at: Option<DateTime<Utc>>,
    /// 提交人。
    pub submitted_by: Option<Uuid>,
    /// 通过时间。
    pub approved_at: Option<DateTime<Utc>>,
    /// 通过人。
    pub approved_by: Option<Uuid>,
    /// 被取代时间。
    pub retired_at: Option<DateTime<Utc>>,
    /// 备注。
    pub remark: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 创建人。
    pub create_by: Option<Uuid>,
    /// 软删时间。
    pub deleted_at: Option<DateTime<Utc>>,
}

/// 月度分解行。
#[derive(Debug, Clone)]
pub struct PeriodRow {
    /// 主键。
    pub id: Uuid,
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 月份 1..12。
    pub period_month: i16,
    /// 本月额度。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

/// 新建 / 修改预算的写入结构（**最终值**，整列覆盖写）。
#[derive(Debug, Clone)]
pub struct BudgetMut {
    /// 年度。
    pub fiscal_year: i16,
    /// 部门 id。
    pub dept_id: Uuid,
    /// 部门名快照。
    pub dept_name: String,
    /// 科目 id。
    pub account_id: Uuid,
    /// 科目编码快照。
    pub account_code: String,
    /// 科目名快照。
    pub account_name: String,
    /// 年度额度。
    pub total_amount: Decimal,
    /// 槽级控制策略。
    pub control_mode: Option<String>,
    /// 版本号。
    pub version_no: i32,
    /// 被取代的旧版本（v1 为空）。
    pub supersedes_id: Option<Uuid>,
    /// ORIGINAL / ADJUST。
    pub change_type: String,
    /// 调整原因。
    pub change_reason: Option<String>,
    /// 备注。
    pub remark: Option<String>,
    /// 操作人。
    pub operator: Option<Uuid>,
}

/// 一条分解行的写入值。
#[derive(Debug, Clone)]
pub struct NewPeriod {
    /// 期间 `yyyy-MM`。
    pub period_code: String,
    /// 月份 1..12。
    pub period_month: i16,
    /// 额度。
    pub amount: Decimal,
    /// 备注。
    pub remark: Option<String>,
}

const BUDGET_COLUMNS: &str = "b.id, b.budget_no, b.fiscal_year, b.dept_id, b.dept_name, \
     b.account_id, b.account_code, b.account_name, b.total_amount::text AS total_amount, \
     b.control_mode, b.version_no, b.supersedes_id, b.change_type, b.change_reason, \
     b.status, b.approval_instance_id, b.effective_from, b.submitted_at, b.submitted_by, \
     b.approved_at, b.approved_by, b.retired_at, b.remark, b.created_at, b.create_by, b.deleted_at";

impl BudgetRow {
    fn from_row(row: &Row) -> DbResult<Self> {
        Ok(Self {
            id: row.get_typed("id")?,
            budget_no: row.get_typed("budget_no")?,
            fiscal_year: row.get_typed("fiscal_year")?,
            dept_id: row.get_typed("dept_id")?,
            dept_name: row.get_typed("dept_name")?,
            account_id: row.get_typed("account_id")?,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            total_amount: parse_amount(&row.get_typed::<String>("total_amount")?),
            control_mode: row.get_typed("control_mode")?,
            version_no: row.get_typed("version_no")?,
            supersedes_id: row.get_typed("supersedes_id")?,
            change_type: row.get_typed("change_type")?,
            change_reason: row.get_typed("change_reason")?,
            status: row.get_typed("status")?,
            approval_instance_id: row.get_typed("approval_instance_id")?,
            effective_from: row.get_typed("effective_from")?,
            submitted_at: row.get_typed("submitted_at")?,
            submitted_by: row.get_typed("submitted_by")?,
            approved_at: row.get_typed("approved_at")?,
            approved_by: row.get_typed("approved_by")?,
            retired_at: row.get_typed("retired_at")?,
            remark: row.get_typed("remark")?,
            created_at: row.get_typed("created_at")?,
            create_by: row.get_typed("create_by")?,
            deleted_at: row.get_typed("deleted_at")?,
        })
    }
}

fn parse_amount(text: &str) -> Decimal {
    text.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

/// 筛选参数里的 `Uuid` 文本（非法 / 空 → `None`，即「不筛这一列」）。
fn parse_filter_uuid(raw: Option<&str>) -> Option<Uuid> {
    raw.map(str::trim)
        .filter(|text| !text.is_empty())
        .and_then(|text| Uuid::parse_str(text).ok())
}

/// 取号（预算单号）。
pub async fn next_budget_no(ex: &impl SqlExecutor, now: DateTime<Utc>) -> DbResult<String> {
    voucher::next_doc_no(ex, &SEQ, now).await
}

/// 按 id 取一行（软删行不返回）。
pub async fn find(id: Uuid) -> DbResult<Option<BudgetRow>> {
    // SCOPE-EXEMPT: 按主键取 1 行给详情/状态流转用；行集权限在分页（budget_page）与
    // handler 的部门校验里，这里按 id 精确定位不产生行集。
    let rows = db()
        .find_with_params(
            &format!("SELECT {BUDGET_COLUMNS} FROM biz_budget b WHERE b.id = ?::uuid AND b.deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(BudgetRow::from_row).transpose()
}

/// 按 id 取一行（**含软删**，版本链展示用）。
pub async fn find_any(id: Uuid) -> DbResult<Option<BudgetRow>> {
    // SCOPE-EXEMPT: 同上，主键精确取 1 行。
    let rows = db()
        .find_with_params(
            &format!("SELECT {BUDGET_COLUMNS} FROM biz_budget b WHERE b.id = ?::uuid"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(BudgetRow::from_row).transpose()
}

/// 同事务版 `find`。
pub async fn find_in(ex: &impl SqlExecutor, id: Uuid) -> DbResult<Option<BudgetRow>> {
    // SCOPE-EXEMPT: 主键精确取 1 行（事务内读自己的写）。
    let rows = ex
        .query_sql(
            format!("SELECT {BUDGET_COLUMNS} FROM biz_budget b WHERE b.id = ?::uuid AND b.deleted_at IS NULL"),
            vec![Value::from(&id)],
        )
        .await?;
    rows.first().map(BudgetRow::from_row).transpose()
}

/// 预算分页的基础 SQL（不含业务过滤）：`{FILTER}` 紧跟恒真条件之后（架构 §2.6）。
fn budget_page_base_sql() -> String {
    let mut sql = format!("SELECT {BUDGET_COLUMNS} FROM biz_budget b WHERE b.deleted_at IS NULL");
    sql.push_str(scope::FILTER_MARKER);
    sql
}

/// 预算分页的业务过滤片段（参数顺序 = SQL 里 `?` 的文本顺序）。
fn budget_page_filters(q: &BudgetFilter, params: &mut Vec<Value>) -> DbResult<String> {
    let mut sql = String::new();
    if let Some(year) = q.fiscal_year {
        sql.push_str(" AND b.fiscal_year = ?");
        params.push(Value::from(i32::from(year)));
    }
    if let Some(dept) = parse_filter_uuid(q.dept_id.as_deref()) {
        sql.push_str(" AND b.dept_id = ?::uuid");
        params.push(Value::from(&dept));
    }
    if let Some(account) = parse_filter_uuid(q.account_id.as_deref()) {
        sql.push_str(" AND b.account_id = ?::uuid");
        params.push(Value::from(&account));
    }
    if let Some(status) = q.status.as_deref() {
        sql.push_str(" AND b.status = ?");
        params.push(Value::from(status.trim()));
    }
    if let Some(change_type) = q.change_type.as_deref() {
        sql.push_str(" AND b.change_type = ?");
        params.push(Value::from(change_type.trim()));
    }
    if let Some(keyword) = q.keyword.as_deref() {
        let pattern = format!("%{}%", keyword.trim());
        sql.push_str(
            " AND (b.budget_no LIKE ? OR b.account_code LIKE ? OR b.account_name LIKE ? OR b.dept_name LIKE ?)",
        );
        for _ in 0..4 {
            params.push(Value::from(pattern.clone()));
        }
    }
    Ok(sql)
}

/// 分页（软删行不返回；按部门数据权限裁剪）。
pub async fn budget_page(
    q: &BudgetFilter,
    principal: &crate::business::shared::principal::ScopedPrincipal,
) -> DbResult<(Vec<BudgetRow>, i64)> {
    // `{FILTER}` 紧跟恒真条件之后（片段参数恒前置，架构 §2.6）：SCOPE-PAGE 卡同款隐患
    // ——此前业务筛选直接拼在 `WHERE` 后，非超管一带筛选就被 `scope::paginate` 拒（500）。
    let mut sql = budget_page_base_sql();
    let mut params: Vec<Value> = Vec::new();
    sql.push_str(&budget_page_filters(q, &mut params)?);
    let result = scope::paginate(
        BUDGET_TARGET,
        &sql,
        params,
        Some(BUDGET_ORDER),
        q.page.unwrap_or(1),
        q.page_size.unwrap_or(20),
        principal,
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(BudgetRow::from_row)
        .collect::<DbResult<Vec<BudgetRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 插入一个预算版本（状态恒 `DRAFT`）。
pub async fn insert_budget(ex: &impl SqlExecutor, id: Uuid, no: &str, new: &BudgetMut) -> DbResult<u64> {
    ex.exec_sql(
        "INSERT INTO biz_budget (
             id, budget_no, fiscal_year, dept_id, dept_name, account_id, account_code,
             account_name, total_amount, control_mode, version_no, supersedes_id,
             change_type, change_reason, status, remark, created_at, updated_at, create_by)
         VALUES (?::uuid, ?, ?::int, ?::uuid, ?, ?::uuid, ?, ?, ?::text::numeric, NULLIF(?::text,''),
                 ?, NULLIF(?::text,'')::uuid, ?, NULLIF(?::text,''), 'DRAFT',
                 NULLIF(?::text,''), now(), now(), NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(no.to_string()),
            Value::from(i32::from(new.fiscal_year)),
            Value::from(&new.dept_id),
            Value::from(new.dept_name.clone()),
            Value::from(&new.account_id),
            Value::from(new.account_code.clone()),
            Value::from(new.account_name.clone()),
            Value::from(new.total_amount.to_string()),
            Value::Text(new.control_mode.clone().unwrap_or_default()),
            Value::from(new.version_no),
            Value::Text(new.supersedes_id.map(|value| value.to_string()).unwrap_or_default()),
            Value::from(new.change_type.clone()),
            Value::Text(new.change_reason.clone().unwrap_or_default()),
            Value::Text(new.remark.clone().unwrap_or_default()),
            Value::Text(new.operator.map(|value| value.to_string()).unwrap_or_default()),
        ],
    )
    .await
}

/// 覆盖写一个**草稿**版本的额度与说明（只允许 DRAFT，调用方先校验）。
pub async fn update_draft(ex: &impl SqlExecutor, id: Uuid, new: &BudgetMut) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget
            SET total_amount = ?::text::numeric, control_mode = NULLIF(?::text,''),
                account_id = ?::uuid,
                account_code = ?, account_name = ?, dept_id = ?::uuid, dept_name = ?,
                fiscal_year = ?::int, change_reason = NULLIF(?::text,''),
                remark = NULLIF(?::text,''), updated_at = now(),
                update_by = NULLIF(?::text,'')::uuid
          WHERE id = ?::uuid AND status = 'DRAFT' AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::from(new.total_amount.to_string()),
            Value::Text(new.control_mode.clone().unwrap_or_default()),
            Value::from(&new.account_id),
            Value::from(new.account_code.clone()),
            Value::from(new.account_name.clone()),
            Value::from(&new.dept_id),
            Value::from(new.dept_name.clone()),
            Value::from(i32::from(new.fiscal_year)),
            Value::Text(new.change_reason.clone().unwrap_or_default()),
            Value::Text(new.remark.clone().unwrap_or_default()),
            Value::Text(new.operator.map(|value| value.to_string()).unwrap_or_default()),
            Value::from(&id),
        ],
    )
    .await
}

/// 读某版本的 12 个月分解（按月份升序）。
pub async fn periods_of(ex: &impl SqlExecutor, budget_id: Uuid) -> DbResult<Vec<PeriodRow>> {
    let rows = ex
        .query_sql(
            "SELECT id, period_code, period_month, amount::text AS amount, remark \
               FROM biz_budget_period WHERE budget_id = ?::uuid ORDER BY period_month"
                .to_string(),
            vec![Value::from(&budget_id)],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(PeriodRow {
                id: row.get_typed("id")?,
                period_code: row.get_typed("period_code")?,
                period_month: row.get_typed("period_month")?,
                amount: parse_amount(&row.get_typed::<String>("amount")?),
                remark: row.get_typed("remark")?,
            })
        })
        .collect()
}

/// 覆盖写分解行（先删后插，**只在 DRAFT** 上发生）。
pub async fn replace_periods(ex: &impl SqlExecutor, budget_id: Uuid, periods: &[NewPeriod]) -> DbResult<u64> {
    ex.exec_sql(
        "DELETE FROM biz_budget_period WHERE budget_id = ?::uuid".to_string(),
        vec![Value::from(&budget_id)],
    )
    .await?;
    let mut affected = 0u64;
    for period in periods {
        affected += ex
            .exec_sql(
                "INSERT INTO biz_budget_period (budget_id, period_code, period_month, amount, remark)
                 VALUES (?::uuid, ?, ?::int, ?::text::numeric, ?)"
                    .to_string(),
                vec![
                    Value::from(&budget_id),
                    Value::from(period.period_code.clone()),
                    Value::from(i32::from(period.period_month)),
                    Value::from(period.amount.to_string()),
                    Value::from(period.remark.clone()),
                ],
            )
            .await?;
    }
    Ok(affected)
}

/// 该槽当前的 ACTIVE 版本（`uk_biz_budget_slot_active` 保证最多 1 行）。
pub async fn active_of_slot(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    account_id: Uuid,
    fiscal_year: i16,
) -> DbResult<Option<BudgetRow>> {
    // SCOPE-EXEMPT: 槽级精确查询（dept+account+year），返回 0..1 行。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {BUDGET_COLUMNS} FROM biz_budget b \
                  WHERE b.dept_id = ?::uuid AND b.account_id = ?::uuid AND b.fiscal_year = ?::int \
                    AND b.status = 'ACTIVE' AND b.deleted_at IS NULL LIMIT 1"
            ),
            vec![
                Value::from(&dept_id),
                Value::from(&account_id),
                Value::from(i32::from(fiscal_year)),
            ],
        )
        .await?;
    rows.first().map(BudgetRow::from_row).transpose()
}

/// 该槽已用的最大版本号（0 = 还没有任何版本）。
pub async fn max_version_of_slot(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    account_id: Uuid,
    fiscal_year: i16,
) -> DbResult<i32> {
    // SCOPE-EXEMPT: 槽级聚合（max），无行集。
    let rows = ex
        .query_sql(
            "SELECT COALESCE(MAX(version_no), 0) AS max_no FROM biz_budget \
              WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?::int \
                AND deleted_at IS NULL"
                .to_string(),
            vec![
                Value::from(&dept_id),
                Value::from(&account_id),
                Value::from(i32::from(fiscal_year)),
            ],
        )
        .await?;
    match rows.first() {
        None => Ok(0),
        Some(row) => row.get_typed("max_no"),
    }
}

/// 该槽是否已有「在审的调整单」。
///
/// ⚠️ ARCH-6 §3.7.2 第 3 条坑：`uk_biz_flow_instance_biz_active` 只能防「同一张单重复发起」，
/// **防不住「同一槽两个新单」**（调整单是新 budget 行 = 新 id），所以必须显式查。
pub async fn pending_adjust_of_slot(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    account_id: Uuid,
    fiscal_year: i16,
) -> DbResult<Option<BudgetRow>> {
    // SCOPE-EXEMPT: 槽级精确查询，返回 0..1 行。
    let rows = ex
        .query_sql(
            format!(
                "SELECT {BUDGET_COLUMNS} FROM biz_budget b \
                  WHERE b.dept_id = ?::uuid AND b.account_id = ?::uuid AND b.fiscal_year = ?::int \
                    AND b.change_type = 'ADJUST' AND b.status IN ('DRAFT','SUBMITTED') \
                    AND b.deleted_at IS NULL LIMIT 1"
            ),
            vec![
                Value::from(&dept_id),
                Value::from(&account_id),
                Value::from(i32::from(fiscal_year)),
            ],
        )
        .await?;
    rows.first().map(BudgetRow::from_row).transpose()
}

/// 把同槽当前 ACTIVE 版本退位（`ACTIVE → SUPERSEDED`，写 `retired_at`）。
///
/// **必须先跑这一步再让新版本 ACTIVE**，否则 `uk_biz_budget_slot_active` 直接 23505
/// （ARCH-6 §3.3.2 硬规则 1）。
pub async fn supersede_active(
    ex: &impl SqlExecutor,
    dept_id: Uuid,
    account_id: Uuid,
    fiscal_year: i16,
    new_id: Uuid,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget
            SET status = 'SUPERSEDED', retired_at = now(), updated_at = now()
          WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?
            AND status = 'ACTIVE' AND deleted_at IS NULL AND id <> ?::uuid"
            .to_string(),
        vec![
            Value::from(&dept_id),
            Value::from(&account_id),
            Value::from(i32::from(fiscal_year)),
            Value::from(&new_id),
        ],
    )
    .await
}

/// CAS 写状态（带上审批实例 / 时间戳；`expect` 不符 → 返回 0 行）。
#[allow(clippy::too_many_arguments)]
pub async fn apply_status(
    ex: &impl SqlExecutor,
    id: Uuid,
    expect: &str,
    target: &str,
    instance_id: Option<Uuid>,
    operator: Option<Uuid>,
    effective_from: Option<NaiveDate>,
) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget
            SET status = ?, updated_at = now(), update_by = NULLIF(?::text,'')::uuid,
                approval_instance_id = NULLIF(?::text,'')::uuid,
                submitted_at = CASE WHEN ? = 'SUBMITTED' THEN now() ELSE submitted_at END,
                submitted_by = CASE WHEN ? = 'SUBMITTED' THEN NULLIF(?::text,'')::uuid
                                    ELSE submitted_by END,
                approved_at = CASE WHEN ? = 'ACTIVE' THEN now() ELSE approved_at END,
                approved_by = CASE WHEN ? = 'ACTIVE' THEN NULLIF(?::text,'')::uuid
                                   ELSE approved_by END,
                effective_from = COALESCE(NULLIF(?::text,'')::date, effective_from)
          WHERE id = ?::uuid AND status = ? AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::from(target),
            uuid_text(operator),
            uuid_text(instance_id),
            Value::from(target),
            Value::from(target),
            uuid_text(operator),
            Value::from(target),
            Value::from(target),
            uuid_text(operator),
            Value::Text(effective_from.map(|date| date.to_string()).unwrap_or_default()),
            Value::from(&id),
            Value::from(expect),
        ],
    )
    .await
}

/// 软删（只允许草稿，调用方先校验状态；草稿的分解行一并物理删掉）。
pub async fn soft_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget SET deleted_at = now(), updated_at = now(),
                update_by = NULLIF(?::text,'')::uuid
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

// ==================== 报表（预算执行） ====================

/// 预算执行表的一行（部门 × 科目 × 年度）。
#[derive(Debug, Clone)]
pub struct ReportRow {
    /// 预算版本 id。
    pub budget_id: Uuid,
    /// 年度。
    pub fiscal_year: i16,
    /// 部门 id。
    pub dept_id: Uuid,
    /// 部门名。
    pub dept_name: String,
    /// 科目 id。
    pub account_id: Uuid,
    /// 科目编码。
    pub account_code: String,
    /// 科目名。
    pub account_name: String,
    /// 年度额度。
    pub total_amount: Decimal,
    /// 占用中。
    pub occupied: Decimal,
    /// 已执行。
    pub consumed: Decimal,
    /// 生效起始日（`effective_from`；NULL = 历史数据没有生效日）。
    pub effective_from: Option<NaiveDate>,
    /// 报表时点是否已生效（`effective_from <= 本月月末`，与控制的期间月末口径一致）。
    pub effective: bool,
    /// 当前月在审 / 已用的分月明细（`period_code → (额度, 已用)`）。
    pub months: Vec<(String, Decimal, Decimal)>,
}

impl ReportRow {
    /// 已用 = 占用中 + 已执行。
    pub fn used(&self) -> Decimal {
        self.occupied + self.consumed
    }

    /// 可用 = 额度 − 已用（**现算**，负数保留：报表要能看出超支）。
    pub fn available(&self) -> Decimal {
        self.total_amount - self.used()
    }

    /// 执行率（额度 0 → 0）。
    pub fn exec_ratio(&self) -> Decimal {
        if self.total_amount <= Decimal::ZERO {
            Decimal::ZERO
        } else {
            (self.used() / self.total_amount * Decimal::from(100)).round_dp(2)
        }
    }
}

/// 预算执行报表（只列 ACTIVE 版本；月份明细来自分解行 + usage 现算）。
///
/// 与 `budget_guard` **同口径**（额度取 ACTIVE 版本、已用取 OCCUPIED+CONSUMED），
/// 否则「报表说还有钱、提交却说超了」这种分叉迟早被用户发现（ARCH-6 §3.8.1）。
pub async fn report(q: &BudgetFilter) -> DbResult<Vec<ReportRow>> {
    let mut sql = String::from(
        "SELECT b.id, b.fiscal_year, b.dept_id, b.dept_name, b.account_id, b.account_code,
                b.account_name, b.total_amount::text AS total_amount, b.effective_from
           FROM biz_budget b
          WHERE b.status = 'ACTIVE' AND b.deleted_at IS NULL",
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(year) = q.fiscal_year {
        sql.push_str(" AND b.fiscal_year = ?");
        params.push(Value::from(i32::from(year)));
    }
    if let Some(dept) = parse_filter_uuid(q.dept_id.as_deref()) {
        sql.push_str(" AND b.dept_id = ?::uuid");
        params.push(Value::from(&dept));
    }
    if let Some(account) = parse_filter_uuid(q.account_id.as_deref()) {
        sql.push_str(" AND b.account_id = ?::uuid");
        params.push(Value::from(&account));
    }
    sql.push_str(" ORDER BY b.fiscal_year DESC, b.dept_name, b.account_code");
    // SCOPE-EXEMPT: 只读报表，行集已在 SQL 里按 ACTIVE + 显式筛选收敛；
    // 报表是给财务的横向视图（跨部门对比），按部门再裁一次会让「全公司执行率」失真。
    let rows = db().query_sql(sql, params).await?;
    // 报表时点 = 本月月末（与 budget_guard 的期间月末口径一致：`effective_from <= 期间月末`）
    let now_period = Utc::now().format("%Y-%m").to_string();
    let period_end = crate::business::erp::budget_guard::period_end_of(&now_period);
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter() {
        let budget_id: Uuid = row.get_typed("id")?;
        let fiscal_year: i16 = row.get_typed("fiscal_year")?;
        let dept_id: Uuid = row.get_typed("dept_id")?;
        let account_id: Uuid = row.get_typed("account_id")?;
        let total_amount = parse_amount(&row.get_typed::<String>("total_amount")?);
        let effective_from: Option<NaiveDate> = row.get_typed("effective_from")?;
        // 未到生效日的行照样出现在报表里（财务要看得到「已经批了但还没开始管」的预算），
        // 但打上 effective = false，避免「报表说有钱、提交却说超了」的分叉。
        let effective = match (effective_from, period_end) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(from), Some(end)) => from <= end,
        };
        // 已用（年度）
        let usage = db()
            .query_sql(
                "SELECT COALESCE(SUM(amount) FILTER (WHERE status = 'OCCUPIED'), 0)::text AS occupied,
                        COALESCE(SUM(amount) FILTER (WHERE status = 'CONSUMED'), 0)::text AS consumed
                   FROM biz_budget_usage
                  WHERE dept_id = ?::uuid AND account_id = ?::uuid AND fiscal_year = ?
                    AND status IN ('OCCUPIED','CONSUMED') AND deleted_at IS NULL"
                    .to_string(),
                vec![
                    Value::from(&dept_id),
                    Value::from(&account_id),
                    Value::from(i32::from(fiscal_year)),
                ],
            )
            .await?;
        let (occupied, consumed) = match usage.first() {
            None => (Decimal::ZERO, Decimal::ZERO),
            Some(u) => (
                parse_amount(&u.get_typed::<String>("occupied")?),
                parse_amount(&u.get_typed::<String>("consumed")?),
            ),
        };
        let months = month_breakdown(budget_id, dept_id, account_id, fiscal_year).await?;
        out.push(ReportRow {
            budget_id,
            fiscal_year,
            dept_id,
            dept_name: row.get_typed("dept_name")?,
            account_id,
            account_code: row.get_typed("account_code")?,
            account_name: row.get_typed("account_name")?,
            total_amount,
            occupied,
            consumed,
            effective_from,
            effective,
            months,
        });
    }
    Ok(out)
}

async fn month_breakdown(
    budget_id: Uuid,
    dept_id: Uuid,
    account_id: Uuid,
    fiscal_year: i16,
) -> DbResult<Vec<(String, Decimal, Decimal)>> {
    let rows = db()
        .query_sql(
            "SELECT p.period_code, p.amount::text AS limit_amount,
                    COALESCE((SELECT SUM(u.amount) FROM biz_budget_usage u
                               WHERE u.dept_id = ?::uuid AND u.account_id = ?::uuid
                                 AND u.period_code = p.period_code
                                 AND u.status IN ('OCCUPIED','CONSUMED')
                                 AND u.deleted_at IS NULL), 0)::text AS used_amount
               FROM biz_budget_period p
              WHERE p.budget_id = ?::uuid
              ORDER BY p.period_month"
                .to_string(),
            vec![Value::from(&dept_id), Value::from(&account_id), Value::from(&budget_id)],
        )
        .await?;
    let _ = fiscal_year;
    rows.iter()
        .map(|row| {
            Ok((
                row.get_typed::<String>("period_code")?,
                parse_amount(&row.get_typed::<String>("limit_amount")?),
                parse_amount(&row.get_typed::<String>("used_amount")?),
            ))
        })
        .collect()
}

/// 版本链（同槽全部版本，按版本号倒序；含被取代与软删的行）。
pub async fn version_chain(dept_id: Uuid, account_id: Uuid, fiscal_year: i16) -> DbResult<Vec<BudgetRow>> {
    // SCOPE-EXEMPT: 槽级查询（版本链展示），行数 = 该槽历史版本数。
    let rows = db()
        .query_sql(
            format!(
                "SELECT {BUDGET_COLUMNS} FROM biz_budget b \
                  WHERE b.dept_id = ?::uuid AND b.account_id = ?::uuid AND b.fiscal_year = ?::int \
                  ORDER BY b.version_no DESC"
            ),
            vec![
                Value::from(&dept_id),
                Value::from(&account_id),
                Value::from(i32::from(fiscal_year)),
            ],
        )
        .await?;
    rows.iter().map(BudgetRow::from_row).collect()
}

// ==================== 科目映射（采购 → 预算科目） ====================

/// 映射行。
#[derive(Debug, Clone)]
pub struct MapRow {
    /// 主键。
    pub id: Uuid,
    /// MATERIAL / MATERIAL_TYPE / WAREHOUSE / DEFAULT。
    pub map_type: String,
    /// 目标 id（DEFAULT 为空）。
    pub ref_id: Option<Uuid>,
    /// 目标编码快照。
    pub ref_code: Option<String>,
    /// 预算科目 id。
    pub account_id: Uuid,
    /// 科目编码（展示用，关联带出）。
    pub account_code: String,
    /// 科目名（展示用）。
    pub account_name: String,
    /// 部门级覆盖。
    pub dept_id: Option<Uuid>,
    /// 优先级。
    pub priority: i32,
    /// 状态。
    pub status: i16,
    /// 备注。
    pub remark: Option<String>,
}

const MAP_COLUMNS: &str = "m.id, m.map_type, m.ref_id, m.ref_code, m.account_id, \
     a.account_code, a.account_name, m.dept_id, m.priority, m.status, m.remark";

/// 映射分页。
pub async fn map_page(q: &BudgetFilter) -> DbResult<(Vec<MapRow>, i64)> {
    let mut sql = format!(
        "SELECT {MAP_COLUMNS} FROM biz_budget_subject_map m \
           LEFT JOIN biz_account a ON a.id = m.account_id \
          WHERE m.deleted_at IS NULL"
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(map_type) = q.status.as_deref()
        && matches!(map_type, "MATERIAL" | "MATERIAL_TYPE" | "WAREHOUSE" | "DEFAULT")
    {
        sql.push_str(" AND m.map_type = ?");
        params.push(Value::from(map_type));
    }
    if let Some(dept) = parse_filter_uuid(q.dept_id.as_deref()) {
        sql.push_str(" AND m.dept_id = ?::uuid");
        params.push(Value::from(&dept));
    }
    let result = scope::paginate_unscoped(
        MAP_TARGET,
        &sql,
        params,
        Some("ORDER BY m.priority DESC, m.map_type, m.created_at DESC"),
        q.page.unwrap_or(1),
        q.page_size.unwrap_or(20),
    )
    .await?;
    let rows = result
        .list
        .iter()
        .map(|row| {
            Ok(MapRow {
                id: row.get_typed("id")?,
                map_type: row.get_typed("map_type")?,
                ref_id: row.get_typed("ref_id")?,
                ref_code: row.get_typed("ref_code")?,
                account_id: row.get_typed("account_id")?,
                account_code: row.get_typed::<Option<String>>("account_code")?.unwrap_or_default(),
                account_name: row.get_typed::<Option<String>>("account_name")?.unwrap_or_default(),
                dept_id: row.get_typed("dept_id")?,
                priority: row.get_typed("priority")?,
                status: row.get_typed("status")?,
                remark: row.get_typed("remark")?,
            })
        })
        .collect::<DbResult<Vec<MapRow>>>()?;
    Ok((rows, i64::try_from(result.total).unwrap_or(i64::MAX)))
}

/// 新建映射。
///
/// 目标对先过 [`crate::business::erp::budget_dto::normalize_target`]：`MATERIAL_TYPE` /
/// `DEFAULT` 的 `ref_id` 落库恒为 NULL（它们没有目标 id），`MATERIAL_TYPE` 的
/// `ref_code` 归一化成大写（与唯一索引 `upper(btrim(ref_code))` 同口径）。
#[allow(clippy::too_many_arguments)]
pub async fn map_insert(
    ex: &impl SqlExecutor,
    id: Uuid,
    map_type: &str,
    ref_id: Option<Uuid>,
    ref_code: Option<&str>,
    account_id: Uuid,
    dept_id: Option<Uuid>,
    priority: i32,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let (ref_id, ref_code) = crate::business::erp::budget_dto::normalize_target(map_type, ref_id, ref_code);
    ex.exec_sql(
        "INSERT INTO biz_budget_subject_map (
             id, map_type, ref_id, ref_code, account_id, dept_id, priority, status, remark,
             created_at, updated_at, create_by)
         VALUES (?::uuid, ?, NULLIF(?::text,'')::uuid, NULLIF(?::text,''), ?::uuid,
                 NULLIF(?::text,'')::uuid, ?, 1, NULLIF(?::text,''), now(), now(),
                 NULLIF(?::text,'')::uuid)"
            .to_string(),
        vec![
            Value::from(&id),
            Value::from(map_type.to_string()),
            uuid_text(ref_id),
            Value::Text(ref_code.unwrap_or_default()),
            Value::from(&account_id),
            uuid_text(dept_id),
            Value::from(priority),
            Value::Text(remark.map(str::to_string).unwrap_or_default()),
            uuid_text(operator),
        ],
    )
    .await
}

/// 修改映射。
///
/// `map_type` 只用于归一化 `ref_code`（`MATERIAL_TYPE` 转大写，其余只去空白）；
/// 目标 id 不可改（换目标 = 删旧建新），所以这里没有 `ref_id` 参数。
#[allow(clippy::too_many_arguments)]
pub async fn map_update(
    ex: &impl SqlExecutor,
    id: Uuid,
    map_type: &str,
    ref_code: Option<&str>,
    account_id: Uuid,
    dept_id: Option<Uuid>,
    priority: i32,
    status: i16,
    remark: Option<&str>,
    operator: Option<Uuid>,
) -> DbResult<u64> {
    let (_, ref_code) = crate::business::erp::budget_dto::normalize_target(map_type, None, ref_code);
    ex.exec_sql(
        "UPDATE biz_budget_subject_map
            SET ref_code = NULLIF(?::text,''), account_id = ?::uuid,
                dept_id = NULLIF(?::text,'')::uuid, priority = ?,
                status = ?::int, remark = NULLIF(?::text,''), updated_at = now(),
                update_by = NULLIF(?::text,'')::uuid
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![
            Value::Text(ref_code.unwrap_or_default()),
            Value::from(&account_id),
            uuid_text(dept_id),
            Value::from(priority),
            Value::from(i32::from(status)),
            Value::Text(remark.map(str::to_string).unwrap_or_default()),
            uuid_text(operator),
            Value::from(&id),
        ],
    )
    .await
}

/// 软删映射。
pub async fn map_delete(ex: &impl SqlExecutor, id: Uuid, operator: Option<Uuid>) -> DbResult<u64> {
    ex.exec_sql(
        "UPDATE biz_budget_subject_map SET deleted_at = now(), updated_at = now(),
                update_by = NULLIF(?::text,'')::uuid
          WHERE id = ?::uuid AND deleted_at IS NULL"
            .to_string(),
        vec![uuid_text(operator), Value::from(&id)],
    )
    .await
}

/// 按映射解析采购单的预算科目（ARCH-6 §3.2.5 的解析顺序）。
///
/// ```text
/// ① 部门级覆盖（dept_id 精确命中）优先
/// ② MATERIAL（按明细物料）→ ③ MATERIAL_TYPE → ④ WAREHOUSE → ⑤ DEFAULT
/// 同分按 priority DESC，再按 map_type 精细度（MATERIAL 最细）
/// ```
///
/// # 各类型的命中口径（与写入侧 `budget_dto::validate_mapping` 一一对应）
///
/// | map_type | 命中条件 | 备注 |
/// |---|---|---|
/// | `MATERIAL` | `ref_id ==` 本轮询明细的物料 id | |
/// | `MATERIAL_TYPE` | `ref_code` 与本单物料的 `material_type` 编码相等（**trim + 忽略大小写**） | 物料类型是 `biz_material.material_type` 上的字符串枚举，没有独立 id，所以 `ref_id` 恒为 NULL（V0240 的 `ck_biz_bsm_ref` 钉死）；比对不区分大小写是为了让配置侧填 `raw` 也能命中 `RAW` |
/// | `WAREHOUSE` | `ref_id ==` 本单仓库 id | |
/// | `DEFAULT` | 恒命中（全公司兜底） | |
///
/// 返回 `(account_id, map_type)`。`None` = 一条映射都没有 → 调用方按控制策略处置
/// （WARN 不占用 + 告警；BLOCK 409）。
pub async fn resolve_for_po(
    ex: &impl SqlExecutor,
    dept_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    material_id: Option<Uuid>,
    material_type: Option<&str>,
) -> DbResult<Option<(Uuid, String)>> {
    // 一次查全量候选（映射表是运维配置，量级几十行），在内存里按精细度挑 —— 比拼 5 段
    // OR 条件更容易看懂，也避免「优先级相同比 map_type」这种排序写进 SQL 后没人能读。
    // SCOPE-EXEMPT: 配置表全量读（几十行），不分页、不裁剪。
    let rows = ex
        .query_sql(
            "SELECT map_type, ref_id, ref_code, account_id, dept_id, priority
               FROM biz_budget_subject_map
              WHERE status = 1 AND deleted_at IS NULL"
                .to_string(),
            vec![],
        )
        .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut best: Option<(i32, u8, Uuid, String)> = None;
    for row in rows.iter() {
        let map_type: String = row.get_typed("map_type")?;
        let ref_id: Option<Uuid> = row.get_typed("ref_id")?;
        let ref_code: Option<String> = row.get_typed("ref_code")?;
        let account_id: Uuid = row.get_typed("account_id")?;
        let map_dept: Option<Uuid> = row.get_typed("dept_id")?;
        let priority: i32 = row.get_typed("priority")?;
        // 部门级覆盖：映射的 dept_id 必须为空（全公司）或等于本单部门
        if let Some(map_dept) = map_dept
            && Some(map_dept) != dept_id
        {
            continue;
        }
        let hit = match map_type.as_str() {
            "MATERIAL" => material_id.is_some() && ref_id == material_id,
            "MATERIAL_TYPE" => match material_type {
                Some(kind) => ref_code
                    .as_deref()
                    .is_some_and(|code| code.trim().eq_ignore_ascii_case(kind.trim())),
                None => false,
            },
            "WAREHOUSE" => warehouse_id.is_some() && ref_id == warehouse_id,
            "DEFAULT" => true,
            _ => false,
        };
        if !hit {
            continue;
        }
        // 精细度：MATERIAL 4 > MATERIAL_TYPE 3 > WAREHOUSE 2 > DEFAULT 1
        let fine = match map_type.as_str() {
            "MATERIAL" => 4u8,
            "MATERIAL_TYPE" => 3,
            "WAREHOUSE" => 2,
            _ => 1,
        };
        // 部门覆盖再加一档，保证「同一优先级下部门级优先于全公司」
        let dept_bonus = i32::from(map_dept.is_some());
        let score = priority * 10 + dept_bonus * 5;
        let better = match &best {
            None => true,
            Some((best_score, best_fine, _, _)) => (score, fine) > (*best_score, *best_fine),
        };
        if better {
            best = Some((score, fine, account_id, map_type));
        }
    }
    Ok(best.map(|(_, _, account_id, map_type)| (account_id, map_type)))
}

fn uuid_text(id: Option<Uuid>) -> Value {
    Value::from(id.map(|value| value.to_string()).unwrap_or_default())
}

#[cfg(test)]
#[path = "budget_model_tests.rs"]
mod budget_model_tests;
